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

use crate::diag::{Diag, DiagCode, DiagSink};
use crate::tir::{Span, Tir, TirData, TirRef, TirTag};
use crate::types::{InternPool, StringId, TypeId, TypeKind};
use std::collections::{HashMap, HashSet};

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
#[derive(Default, Clone)]
#[allow(dead_code)]
pub(crate) struct Ownership {
    pub states: HashMap<TirRef, OwnerState>,
    pub current_owner: HashMap<StringId, TirRef>,
    pub origin: HashMap<TirRef, Option<TirRef>>,
}

impl Ownership {
    /// Capture the current lattice for branch analysis. The CFG-join
    /// logic in `analyze_if_stmt` snapshots state before each branch,
    /// walks branches independently from that snapshot, then merges.
    fn clone_snapshot(&self) -> Ownership {
        self.clone()
    }

    /// Reset the lattice to a previous snapshot. Used to walk each
    /// branch of an `if` from the same starting state.
    fn restore_from(&mut self, snap: &Ownership) {
        self.states = snap.states.clone();
        self.current_owner = snap.current_owner.clone();
        self.origin = snap.origin.clone();
    }

    /// Conservatively merge per-branch lattices into `self`. For each
    /// tracked `TirRef`, if any branch left it `Moved` the merged
    /// state is `Moved` — that way a value consumed on only one
    /// branch is still treated as moved at the join point and a
    /// post-`if` use trips E0020. Non-`Moved` states pick the first
    /// observed branch state. `current_owner` entries from any branch
    /// are preserved so reseats inside a branch survive the join.
    fn merge_branches(&mut self, branches: &[Ownership]) {
        let mut all_refs: std::collections::HashSet<TirRef> = std::collections::HashSet::new();
        for b in branches {
            all_refs.extend(b.states.keys().copied());
        }
        for r in all_refs {
            let mut merged: Option<OwnerState> = None;
            for b in branches {
                let s = b.states.get(&r).cloned().unwrap_or(OwnerState::NotTracked);
                merged = Some(match (merged, s) {
                    (None, s) => s,
                    (Some(OwnerState::Moved { moved_at, kind }), _) => {
                        OwnerState::Moved { moved_at, kind }
                    }
                    (_, OwnerState::Moved { moved_at, kind }) => {
                        OwnerState::Moved { moved_at, kind }
                    }
                    (Some(a), _) => a,
                });
            }
            if let Some(state) = merged {
                self.states.insert(r, state);
            }
        }
        for b in branches {
            for (name, owner) in &b.current_owner {
                self.current_owner.entry(*name).or_insert(*owner);
            }
            for (k, v) in &b.origin {
                self.origin.entry(*k).or_insert(*v);
            }
        }
    }
}

/// Validate move safety for every function body. Emits diagnostics
/// into `sink`. Returns nothing — codegen continues to consume the
/// unchanged `&[Tir]`.
pub fn check(tirs: &[Tir], pool: &InternPool, sink: &mut DiagSink) {
    // Name-keyed lookup so call sites can read the callee's per-
    // parameter `is_move` flags. Builtins (`print`, `assert`,
    // `int_to_str`) are not in this map and default to borrowing.
    let by_name: HashMap<StringId, &Tir> = tirs.iter().map(|t| (t.name, t)).collect();
    for tir in tirs {
        analyze_function(tir, pool, sink, &by_name);
    }
}

fn analyze_function(
    tir: &Tir,
    pool: &InternPool,
    sink: &mut DiagSink,
    by_name: &HashMap<StringId, &Tir>,
) {
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
        analyze_stmt(tir, pool, &mut own, sink, by_name, stmt);
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

fn analyze_stmt(
    tir: &Tir,
    pool: &InternPool,
    own: &mut Ownership,
    sink: &mut DiagSink,
    by_name: &HashMap<StringId, &Tir>,
    stmt: TirRef,
) {
    let inst = *tir.inst(stmt);
    match inst.tag {
        TirTag::VarDecl => analyze_var_decl(tir, pool, own, sink, by_name, stmt),
        TirTag::Assign => analyze_assign(tir, pool, own, sink, by_name, stmt),
        TirTag::Return => analyze_return(tir, pool, own, sink, by_name, stmt),
        TirTag::ReturnVoid => {}
        TirTag::IfStmt => analyze_if_stmt(tir, pool, own, sink, by_name, stmt),
        TirTag::WhileLoop => analyze_while_loop(tir, pool, own, sink, by_name, stmt),
        TirTag::ForRange => analyze_for_range(tir, pool, own, sink, by_name, stmt),
        TirTag::Break | TirTag::Continue => {
            // 8.1c attaches Free metadata here; 8.1b is a no-op.
        }
        TirTag::ExprStmt => {
            if let TirData::UnOp(o) = inst.data {
                visit_expr(tir, pool, own, sink, by_name, o);
            }
        }
        _ => {
            visit_expr(tir, pool, own, sink, by_name, stmt);
        }
    }
}

/// Move-typed `VarDecl` is a consumer: the new binding takes
/// ownership of the initializer's underlying value. If the
/// initializer aliases a borrowed parameter, this is the E0021
/// "move out of borrowed parameter" site; if the underlying owner is
/// already `Moved`, this is the E0020 "use after move" site.
fn analyze_var_decl(
    tir: &Tir,
    pool: &InternPool,
    own: &mut Ownership,
    sink: &mut DiagSink,
    by_name: &HashMap<StringId, &Tir>,
    r: TirRef,
) {
    let view = tir.var_decl_view(r);
    let init = view.initializer;
    let init_ty = tir.inst(init).ty;
    visit_expr(tir, pool, own, sink, by_name, init);
    if needs_tracking(init_ty, pool) {
        let span = tir.span(r);
        consume_for_assignment(own, sink, init, span, MoveKind::Assignment);
        rebind_to_init(own, view.name, init);
    } else {
        own.current_owner.insert(view.name, init);
    }
}

/// Reassignment to a Move-typed binding. Same consumption rules as
/// `VarDecl`; the existing binding name is reseated to whichever
/// SSA value owns the new underlying allocation.
fn analyze_assign(
    tir: &Tir,
    pool: &InternPool,
    own: &mut Ownership,
    sink: &mut DiagSink,
    by_name: &HashMap<StringId, &Tir>,
    r: TirRef,
) {
    let view = tir.assign_view(r);
    let value_ty = tir.inst(view.value).ty;
    visit_expr(tir, pool, own, sink, by_name, view.value);
    if needs_tracking(value_ty, pool) {
        let span = tir.span(r);
        consume_for_assignment(own, sink, view.value, span, MoveKind::Assignment);
        rebind_to_init(own, view.name, view.value);
    }
}

/// Move-typed `Return` is a consumer: the returned value flows out of
/// the function and the caller takes ownership. Borrowed parameters
/// cannot be returned (E0022). If the underlying owner is already
/// `Moved`, this is the E0020 "use after move" site.
fn analyze_return(
    tir: &Tir,
    pool: &InternPool,
    own: &mut Ownership,
    sink: &mut DiagSink,
    by_name: &HashMap<StringId, &Tir>,
    r: TirRef,
) {
    let inst = *tir.inst(r);
    let operand = match inst.data {
        TirData::UnOp(o) => o,
        _ => unreachable!("Return must carry TirData::UnOp"),
    };
    let ty = tir.inst(operand).ty;
    visit_expr(tir, pool, own, sink, by_name, operand);
    if !needs_tracking(ty, pool) {
        return;
    }
    let span = tir.span(r);
    let underlying = underlying_owner(own, operand);
    let state = own
        .states
        .get(&underlying)
        .cloned()
        .unwrap_or(OwnerState::NotTracked);
    match state {
        OwnerState::Valid => {
            own.states.insert(
                underlying,
                OwnerState::Moved {
                    moved_at: span,
                    kind: MoveKind::Return,
                },
            );
        }
        OwnerState::Borrowed => {
            sink.emit(Diag::error(
                span,
                DiagCode::ReturnBorrowedValue,
                "cannot return borrowed value",
            ));
        }
        OwnerState::Moved { moved_at, .. } => {
            sink.emit(
                Diag::error(span, DiagCode::UseAfterMove, "use of moved value in return")
                    .with_note(Some(moved_at), "value was moved here"),
            );
        }
        OwnerState::NotTracked => {}
    }
}

/// Reseat `name` to point at `init` as its current owner. After a
/// consume, the source binding's underlying value is `Moved`; the
/// new binding takes a fresh, independent slot in the lattice so
/// subsequent reads of `name` resolve to a `Valid` owner instead of
/// tripping over the just-moved underlying. We do this by severing
/// `init`'s `origin` link (if any) and stamping it `Valid`.
fn rebind_to_init(own: &mut Ownership, name: StringId, init: TirRef) {
    own.origin.insert(init, None);
    own.states.insert(init, OwnerState::Valid);
    own.current_owner.insert(name, init);
}

/// Walk back from `init` to whichever SSA value currently owns the
/// underlying allocation. `visit_expr` is responsible for populating
/// `origin` for `Var` reads; for fresh producers (`StrConst`,
/// `StrConcat`, `Call`) `init` is itself the owner.
fn underlying_owner(own: &Ownership, init: TirRef) -> TirRef {
    match own.origin.get(&init).copied() {
        Some(Some(owner)) => owner,
        _ => init,
    }
}

/// Apply the consumption transition for a Move-typed initializer.
/// Caller must have already populated origin/state for `init` via
/// `visit_expr`.
fn consume_for_assignment(
    own: &mut Ownership,
    sink: &mut DiagSink,
    init: TirRef,
    span: Span,
    kind: MoveKind,
) {
    let underlying = underlying_owner(own, init);
    let state = own
        .states
        .get(&underlying)
        .cloned()
        .unwrap_or(OwnerState::NotTracked);
    match state {
        OwnerState::Valid => {
            own.states.insert(
                underlying,
                OwnerState::Moved {
                    moved_at: span,
                    kind,
                },
            );
        }
        OwnerState::Borrowed => {
            sink.emit(Diag::error(
                span,
                DiagCode::MoveOutOfBorrowedParam,
                "cannot move out of borrowed parameter",
            ));
        }
        OwnerState::Moved { moved_at, .. } => {
            sink.emit(
                Diag::error(span, DiagCode::UseAfterMove, "use of moved value")
                    .with_note(Some(moved_at), "value was moved here"),
            );
        }
        OwnerState::NotTracked => {}
    }
}

/// CFG join for `if` / `elif` / `else`. The naïve forward walk
/// would let a move inside a then-branch persist past the merge
/// regardless of whether else also moved — wrong for the spec's
/// guarantee that conditionally-moved values are not safe to use
/// after the join. Snapshot the lattice before each branch, walk
/// each branch from the snapshot independently, then merge. If any
/// branch left a value `Moved`, the post-`if` state is `Moved`;
/// when no `else` is present, the implicit fall-through branch is
/// the pre-`if` snapshot itself, so an unconsumed pre-`if` value
/// stays usable after the join.
fn analyze_if_stmt(
    tir: &Tir,
    pool: &InternPool,
    own: &mut Ownership,
    sink: &mut DiagSink,
    by_name: &HashMap<StringId, &Tir>,
    r: TirRef,
) {
    let view = tir.if_stmt_view(r);
    visit_expr(tir, pool, own, sink, by_name, view.cond);

    let snapshot = own.clone_snapshot();

    for stmt in &view.then_stmts {
        analyze_stmt(tir, pool, own, sink, by_name, *stmt);
    }
    let then_state = own.clone_snapshot();

    let mut branch_results = vec![then_state];
    for elif in &view.elif_branches {
        own.restore_from(&snapshot);
        visit_expr(tir, pool, own, sink, by_name, elif.cond);
        for stmt in &elif.body {
            analyze_stmt(tir, pool, own, sink, by_name, *stmt);
        }
        branch_results.push(own.clone_snapshot());
    }

    if let Some(else_stmts) = &view.else_stmts {
        own.restore_from(&snapshot);
        for stmt in else_stmts {
            analyze_stmt(tir, pool, own, sink, by_name, *stmt);
        }
        branch_results.push(own.clone_snapshot());
    } else {
        branch_results.push(snapshot.clone());
    }

    own.restore_from(&snapshot);
    own.merge_branches(&branch_results);
}

/// Fixed-point ownership analysis for `while`. Walks the body once
/// from the entry state into a scratch sink; if no tracked TirRef
/// changed Moved-ness across the back-edge, the body is loop-invariant
/// for ownership purposes and the scratch diagnostics are flushed.
/// Otherwise we re-walk from the merged (entry ⊔ post-body) state and
/// emit diagnostics on the second pass — a binding moved inside the
/// body without rebinding before the back-edge surfaces as E0020 on
/// iteration two. Converges in at most 2 iterations for the M8.1
/// pattern set (per the design doc).
fn analyze_while_loop(
    tir: &Tir,
    pool: &InternPool,
    own: &mut Ownership,
    sink: &mut DiagSink,
    by_name: &HashMap<StringId, &Tir>,
    r: TirRef,
) {
    let view = tir.while_loop_view(r);
    let entry = own.clone_snapshot();

    // First pass into a scratch sink — diagnostics are kept only if
    // a single iteration suffices. If the lattice changes we re-walk
    // and the scratch diags are dropped (the second walk re-emits
    // anything that's still wrong).
    let mut scratch_sink = DiagSink::new();
    visit_expr(tir, pool, own, &mut scratch_sink, by_name, view.cond);
    for stmt in &view.body {
        analyze_stmt(tir, pool, own, &mut scratch_sink, by_name, *stmt);
    }
    let after_first = own.clone_snapshot();

    if states_differ(&entry, &after_first) {
        let merged = merge_two(&entry, &after_first);
        own.restore_from(&merged);
        visit_expr(tir, pool, own, sink, by_name, view.cond);
        for stmt in &view.body {
            analyze_stmt(tir, pool, own, sink, by_name, *stmt);
        }
        let after_second = own.clone_snapshot();
        let final_merged = merge_two(&entry, &after_second);
        own.restore_from(&final_merged);
    } else {
        for d in scratch_sink.into_diags() {
            sink.emit(d);
        }
        let final_merged = merge_two(&entry, &after_first);
        own.restore_from(&final_merged);
    }
}

/// `for i in range(start, end)` loop var is `int` (Copy), so the
/// induction variable never enters the lattice. The body runs the
/// same fixed-point as `while`.
fn analyze_for_range(
    tir: &Tir,
    pool: &InternPool,
    own: &mut Ownership,
    sink: &mut DiagSink,
    by_name: &HashMap<StringId, &Tir>,
    r: TirRef,
) {
    let view = tir.for_range_view(r);
    // Start/end are visited unconditionally — they're plain `int`
    // exprs, so they don't move anything, but they may contain nested
    // reads we want to record.
    visit_expr(tir, pool, own, sink, by_name, view.start);
    visit_expr(tir, pool, own, sink, by_name, view.end);

    let entry = own.clone_snapshot();

    let mut scratch_sink = DiagSink::new();
    for stmt in &view.body {
        analyze_stmt(tir, pool, own, &mut scratch_sink, by_name, *stmt);
    }
    let after_first = own.clone_snapshot();

    if states_differ(&entry, &after_first) {
        let merged = merge_two(&entry, &after_first);
        own.restore_from(&merged);
        for stmt in &view.body {
            analyze_stmt(tir, pool, own, sink, by_name, *stmt);
        }
        let after_second = own.clone_snapshot();
        let final_merged = merge_two(&entry, &after_second);
        own.restore_from(&final_merged);
    } else {
        for d in scratch_sink.into_diags() {
            sink.emit(d);
        }
        let final_merged = merge_two(&entry, &after_first);
        own.restore_from(&final_merged);
    }
}

/// Compare two lattices on the Moved-ness of every tracked `TirRef`.
/// Per the M8.1b plan, the only transition that forces a re-walk is a
/// `Valid`/`Borrowed` → `Moved` flip across the back-edge — non-Moved
/// state changes (e.g. fresh definitions added inside the body) are
/// loop-invariant for the use-after-move check.
fn states_differ(a: &Ownership, b: &Ownership) -> bool {
    let keys: HashSet<TirRef> = a.states.keys().chain(b.states.keys()).copied().collect();
    for k in keys {
        let av = a.states.get(&k).cloned().unwrap_or(OwnerState::NotTracked);
        let bv = b.states.get(&k).cloned().unwrap_or(OwnerState::NotTracked);
        if matches!(av, OwnerState::Moved { .. }) != matches!(bv, OwnerState::Moved { .. }) {
            return true;
        }
    }
    false
}

/// Merge two lattices by reusing the existing branch-merge join: if
/// either input has a TirRef `Moved`, the result is `Moved`. Used to
/// seed the second pass and to compute the post-loop state.
fn merge_two(a: &Ownership, b: &Ownership) -> Ownership {
    let mut merged = a.clone_snapshot();
    merged.merge_branches(&[a.clone_snapshot(), b.clone_snapshot()]);
    merged
}

fn visit_expr(
    tir: &Tir,
    pool: &InternPool,
    own: &mut Ownership,
    sink: &mut DiagSink,
    by_name: &HashMap<StringId, &Tir>,
    r: TirRef,
) {
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
                visit_expr(tir, pool, own, sink, by_name, lhs);
                visit_expr(tir, pool, own, sink, by_name, rhs);
            }
        }
        TirTag::Call => {
            // A str-returning call (e.g. `int_to_str`) is a producer.
            if needs_tracking(inst.ty, pool) {
                own.states.insert(r, OwnerState::Valid);
                own.origin.insert(r, None);
            }
            let view = tir.call_view(r);
            // Look up callee parameter conventions. Builtins
            // (`print`, `assert`, `int_to_str`) have no entry and
            // default to borrowing all args.
            let callee_params = by_name.get(&view.name).map(|t| t.params.as_slice());
            for (i, arg) in view.args.iter().enumerate() {
                visit_expr(tir, pool, own, sink, by_name, *arg);
                let arg_ty = tir.inst(*arg).ty;
                if !needs_tracking(arg_ty, pool) {
                    continue;
                }
                let is_move_param = callee_params
                    .and_then(|ps| ps.get(i))
                    .map(|p| p.is_move)
                    .unwrap_or(false);
                if is_move_param {
                    consume_for_assignment(
                        own,
                        sink,
                        *arg,
                        tir.span(r),
                        MoveKind::MoveParam { callee: view.name },
                    );
                }
                // Borrow path: no extra check here. With the current
                // set of producers (StrConst/StrConcat/Call/Var), every
                // tracked str argument flows through `Var`, and the
                // `Var` arm already fires E0020 for use-after-move.
                // Adding a call-site check would double-report. If a
                // future producer pattern bypasses `Var` (e.g. passing
                // a moved Call result directly), revisit this.
            }
        }
        // ---- Aliasing read ----
        // `Var` is a non-consuming read. Record which SSA value it
        // currently aliases so a later use-after-move diagnostic can
        // walk back to the root owner. Per the design spec, an
        // aliasing read of an already-`Moved` owner is itself an
        // E0020 use-after-move. Reads of `Borrowed` owners are fine
        // (Rule 2 — borrowed parameters can be freely read).
        TirTag::Var => {
            let name = match inst.data {
                TirData::Var(n) => n,
                _ => unreachable!("Var must carry TirData::Var"),
            };
            if let Some(&owner) = own.current_owner.get(&name)
                && needs_tracking(inst.ty, pool)
            {
                own.origin.insert(r, Some(owner));
                if let Some(OwnerState::Moved { moved_at, .. }) = own.states.get(&owner).cloned() {
                    sink.emit(
                        Diag::error(tir.span(r), DiagCode::UseAfterMove, "use of moved value")
                            .with_note(Some(moved_at), "value was moved here"),
                    );
                }
            }
        }
        // ---- Everything else: recurse on operands so nested
        // ---- producers/aliases are still observed.
        _ => {
            recurse_operands(tir, pool, own, sink, by_name, r);
        }
    }
}

fn recurse_operands(
    tir: &Tir,
    pool: &InternPool,
    own: &mut Ownership,
    sink: &mut DiagSink,
    by_name: &HashMap<StringId, &Tir>,
    r: TirRef,
) {
    let inst = *tir.inst(r);
    match inst.data {
        TirData::UnOp(o) => visit_expr(tir, pool, own, sink, by_name, o),
        TirData::BinOp { lhs, rhs } => {
            visit_expr(tir, pool, own, sink, by_name, lhs);
            visit_expr(tir, pool, own, sink, by_name, rhs);
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
