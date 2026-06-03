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
use crate::tir::Tir;
use crate::types::InternPool;

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
