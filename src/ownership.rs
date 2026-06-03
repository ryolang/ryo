//! Ownership pass — validates move safety on per-`TirRef` lattice.
//!
//! Runs between sema and codegen. Walks each `Tir` forward, tracking
//! ownership state for every Move-typed value. Catches use-after-move,
//! moves out of borrowed parameters, and returns of borrowed values.
//! Emits diagnostics into the shared `DiagSink` — does not mutate TIR
//! and does not insert Free instructions (that lands in M8.1c).
//!
//! ## State lattice
//!
//! Per-TirRef state, not per-binding. A binding name resolves through
//! a shadow `current_owner: HashMap<StringId, TirRef>` to whichever
//! SSA value currently owns the underlying allocation. Anonymous owned
//! temporaries (concat results, formatter outputs) live in the same
//! `states` map with no shadow entry.
//!
//! See `docs/superpowers/specs/2026-05-20-milestone-8.1-heap-str-and-move-semantics-design.md`
//! sub-milestone 8.1b for the full algorithm.
//!
//! ## Mojo reference
//!
//! See `docs/dev/mojo_reference.md`.

use crate::diag::DiagSink;
use crate::tir::{Span, Tir, TirData, TirRef, TirTag};
use crate::types::{InternPool, StringId, TypeId, TypeKind};
use std::collections::HashMap;

// ---------- Classification ----------

/// True for types whose values can be freely duplicated by `=`
/// without invalidating the source. Mirrors Mojo's `Copyable` trait
/// for the scalar primitives Ryo currently has. The ownership walk
/// short-circuits on these — they never enter the lattice.
#[allow(dead_code)] // wired into consumption logic in later tasks
pub(crate) fn is_copy_type(ty: TypeId, pool: &InternPool) -> bool {
    matches!(
        pool.kind(ty),
        TypeKind::Int | TypeKind::Float | TypeKind::Bool
    )
}

/// True for types whose values transfer ownership on `=` and must be
/// tracked through the function body. Today: `str` only. Future heap
/// types (`List[T]`, `Dict[K, V]`) will join this set.
pub(crate) fn is_move_type(ty: TypeId, pool: &InternPool) -> bool {
    matches!(pool.kind(ty), TypeKind::Str)
}

/// Predicate the ownership walk uses to decide whether a `TirRef`
/// needs a lattice slot. Currently identical to `is_move_type`, but
/// kept as its own name so the walk reads correctly when borrows
/// land and the answer becomes "move OR borrowed-of-move".
pub(crate) fn needs_tracking(ty: TypeId, pool: &InternPool) -> bool {
    is_move_type(ty, pool)
}

// ---------- Lattice ----------

/// Per-`TirRef` ownership state. Anything Copy-typed lives in
/// `NotTracked` for its whole lifetime (the walk skips it). Move-
/// typed values start at `Valid` on definition, transition to
/// `Borrowed` while a borrow is live, and to `Moved` once consumed.
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum OwnerState {
    NotTracked,
    Valid,
    Borrowed,
    Moved { moved_at: Span, kind: MoveKind },
}

/// Why a value transitioned to `Moved`. Drives diagnostic phrasing
/// at the use-after-move site ("moved into return", "moved into
/// `foo`", etc.).
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum MoveKind {
    Assignment,
    Return,
    MoveParam { callee: StringId },
}

/// Per-function ownership state. `states` is the lattice itself,
/// keyed by the `TirRef` that produced the value. `current_owner`
/// is a shadow map from binding name to whichever SSA value
/// currently owns the underlying allocation (so reassignment
/// reseats ownership without disturbing the producing SSA value).
/// `origin` records, for each tracked `TirRef`, the upstream value
/// it derives from (or `None` for fresh allocations) — used to walk
/// back to the root owner when diagnosing a use-after-move.
#[derive(Default)]
#[allow(dead_code)]
pub(crate) struct Ownership {
    pub states: HashMap<TirRef, OwnerState>,
    pub current_owner: HashMap<StringId, TirRef>,
    pub origin: HashMap<TirRef, Option<TirRef>>,
}

/// Validate move safety for every function body. Emits diagnostics
/// into `sink`. Returns nothing — codegen continues to consume the
/// unchanged `&[Tir]`.
pub fn check(tirs: &[Tir], pool: &InternPool, sink: &mut DiagSink) {
    for tir in tirs {
        analyze_function(tir, pool, sink);
    }
}

fn analyze_function(tir: &Tir, pool: &InternPool, _sink: &mut DiagSink) {
    let mut own = Ownership::default();

    // Initialise per-parameter state. Move-typed params start at
    // `Valid` (the callee owns them); borrowed params start at
    // `Borrowed`. Copy-typed params skip the lattice entirely.
    for param in &tir.params {
        if !needs_tracking(param.ty, pool) {
            continue;
        }
        let synthetic = synthetic_param_ref(param.name);
        let state = if param.is_move {
            OwnerState::Valid
        } else {
            OwnerState::Borrowed
        };
        own.states.insert(synthetic, state);
        own.current_owner.insert(param.name, synthetic);
    }

    for stmt in tir.body_stmts() {
        analyze_stmt(tir, pool, &mut own, stmt);
    }
}

/// Build a stable synthetic [`TirRef`] for a parameter so it can
/// share the `states` map with real instruction refs. Encoded near
/// `u32::MAX` to keep it well clear of real per-function indices
/// (which start at 1 and grow with body size). `u32::MAX - name.raw()`
/// stays non-zero for any plausible interned-string id.
fn synthetic_param_ref(name: StringId) -> TirRef {
    let raw = u32::MAX - name.raw();
    TirRef::from_raw(raw)
}

fn analyze_stmt(tir: &Tir, pool: &InternPool, own: &mut Ownership, stmt: TirRef) {
    visit_expr(tir, pool, own, stmt);
}

fn visit_expr(tir: &Tir, pool: &InternPool, own: &mut Ownership, r: TirRef) {
    let inst = *tir.inst(r);
    match inst.tag {
        // ---- Allocating instructions ----
        // `StrConst` materializes a fresh heap string at runtime;
        // `StrConcat` produces a brand-new allocation from its two
        // operands. Both enter the lattice as `Valid` with no
        // upstream origin.
        TirTag::StrConst => {
            if needs_tracking(inst.ty, pool) {
                own.states.insert(r, OwnerState::Valid);
                own.origin.insert(r, None);
            }
        }
        TirTag::StrConcat => {
            if needs_tracking(inst.ty, pool) {
                own.states.insert(r, OwnerState::Valid);
                own.origin.insert(r, None);
            }
            if let TirData::BinOp { lhs, rhs } = inst.data {
                visit_expr(tir, pool, own, lhs);
                visit_expr(tir, pool, own, rhs);
            }
        }
        TirTag::Call => {
            // A str-returning call (e.g. `int_to_str`) is a producer.
            // Argument-side consumption lands in a later task; for
            // now just recurse so any nested producers/aliases are
            // observed.
            if needs_tracking(inst.ty, pool) {
                own.states.insert(r, OwnerState::Valid);
                own.origin.insert(r, None);
            }
            let view = tir.call_view(r);
            for arg in view.args {
                visit_expr(tir, pool, own, arg);
            }
        }
        // ---- Aliasing read ----
        // `Var` is a non-consuming read. Record which SSA value it
        // currently aliases so a later use-after-move diagnostic can
        // walk back to the root owner.
        TirTag::Var => {
            let name = match inst.data {
                TirData::Var(n) => n,
                _ => unreachable!("Var must carry TirData::Var"),
            };
            if let Some(&owner) = own.current_owner.get(&name)
                && needs_tracking(inst.ty, pool)
            {
                own.origin.insert(r, Some(owner));
            }
        }
        // ---- Everything else: recurse on operands so nested
        // ---- producers/aliases are still observed.
        _ => {
            recurse_operands(tir, pool, own, r);
        }
    }
}

fn recurse_operands(tir: &Tir, pool: &InternPool, own: &mut Ownership, r: TirRef) {
    let inst = *tir.inst(r);
    match inst.data {
        TirData::UnOp(o) => visit_expr(tir, pool, own, o),
        TirData::BinOp { lhs, rhs } => {
            visit_expr(tir, pool, own, lhs);
            visit_expr(tir, pool, own, rhs);
        }
        // `Extra`-shaped instructions (VarDecl, Assign, Call,
        // IfStmt, WhileLoop, ForRange, CompoundAssign) have
        // bespoke decoders. Consumption logic lands in subsequent
        // tasks; until then their operands are deliberately not
        // descended into here so we avoid double-visits when those
        // tasks introduce per-tag handling.
        TirData::Extra(_) => {}
        TirData::None
        | TirData::Int(_)
        | TirData::Float(_)
        | TirData::Str(_)
        | TirData::Bool(_)
        | TirData::Var(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_types_classified() {
        let pool = InternPool::new();
        assert!(is_copy_type(pool.int(), &pool));
        assert!(is_copy_type(pool.float(), &pool));
        assert!(is_copy_type(pool.bool_(), &pool));
        assert!(!is_copy_type(pool.str_(), &pool));
    }

    #[test]
    fn move_types_classified() {
        let pool = InternPool::new();
        assert!(is_move_type(pool.str_(), &pool));
        assert!(!is_move_type(pool.int(), &pool));
        assert!(!is_move_type(pool.bool_(), &pool));
    }

    #[test]
    fn needs_tracking_matches_move() {
        let pool = InternPool::new();
        assert!(needs_tracking(pool.str_(), &pool));
        assert!(!needs_tracking(pool.int(), &pool));
    }

    #[test]
    fn str_const_walk_no_panic() {
        use crate::tir::TirBuilder;
        use chumsky::span::{SimpleSpan, Span as _};

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let hello = pool.intern_str("hello");
        let span = SimpleSpan::new((), 0..0);

        // fn main() -> void: <str_const "hello"> as expr_stmt
        let mut b = TirBuilder::new(main, vec![], void, span);
        let s = b.str_const(hello, str_ty, span);
        let stmt = b.unary(TirTag::ExprStmt, void, s, span);
        let tir = b.finish(&[stmt]);

        let mut sink = DiagSink::new();
        check(std::slice::from_ref(&tir), &pool, &mut sink);
        assert!(sink.is_empty());
    }
}
