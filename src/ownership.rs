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
use crate::tir::{Span, Tir, TirRef};
use crate::types::{InternPool, StringId, TypeId, TypeKind};
use std::collections::HashMap;

// ---------- Classification ----------

/// True for types whose values can be freely duplicated by `=`
/// without invalidating the source. Mirrors Mojo's `Copyable` trait
/// for the scalar primitives Ryo currently has. The ownership walk
/// short-circuits on these — they never enter the lattice.
#[allow(dead_code)] // wired into the walk in Task 10
pub(crate) fn is_copy_type(ty: TypeId, pool: &InternPool) -> bool {
    matches!(
        pool.kind(ty),
        TypeKind::Int | TypeKind::Float | TypeKind::Bool
    )
}

/// True for types whose values transfer ownership on `=` and must be
/// tracked through the function body. Today: `str` only. Future heap
/// types (`List[T]`, `Dict[K, V]`) will join this set.
#[allow(dead_code)] // wired into the walk in Task 10
pub(crate) fn is_move_type(ty: TypeId, pool: &InternPool) -> bool {
    matches!(pool.kind(ty), TypeKind::Str)
}

/// Predicate the ownership walk uses to decide whether a `TirRef`
/// needs a lattice slot. Currently identical to `is_move_type`, but
/// kept as its own name so the walk reads correctly when borrows
/// land and the answer becomes "move OR borrowed-of-move".
#[allow(dead_code)] // wired into the walk in Task 10
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

fn analyze_function(_tir: &Tir, _pool: &InternPool, _sink: &mut DiagSink) {
    // No-op for now; lattice + walk land in subsequent tasks.
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
}
