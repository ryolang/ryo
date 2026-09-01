use crate::tir::{Span, TirRef};
use crate::types::StringId;

/// Identifies a specific arm of an `IfStmt` (and, future, `Match`).
/// Assigned by the ownership pass; codegen maps each `BranchId` to a
/// concrete Cranelift `Block` as it lowers if/else regions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct BranchId(pub u32);

/// One scheduled Free. Codegen emits `ryo_str_free(ptr, cap)` after
/// the instruction at `after`, gated by `branch` (None =
/// unconditional, Some = only inside that arm or a descendant).
#[derive(Clone, Debug)]
pub struct FreePoint {
    pub after: TirRef,
    pub target: TirRef,
    pub span: Span,
    pub branch: Option<BranchId>,
}

/// Per-`IfStmt` mapping from arm position to its assigned [`BranchId`].
/// Codegen uses this to push the right `BranchId` onto `branch_stack`
/// as it lowers each arm, so a branch-gated `FreePoint` only fires
/// inside the arm that ended with the owner still `Valid`.
#[derive(Debug, Clone, Default)]
pub struct IfBranchIds {
    pub then_branch: BranchId,
    pub elif_branches: Vec<BranchId>,
    pub else_branch: Option<BranchId>,
}

/// One conditional drop of a pre-branch buffer. When a binding
/// is reassigned in SOME arms of an if but kept untouched in others,
/// and the reassigned value is never read after the join, the
/// pre-branch buffer would leak on the untouched paths. This drop
/// covers it: codegen emits `ryo_str_free` at the START of each arm in
/// `arms` (the binding's value there is still the pre-if one).
/// `target` is the pre-branch owner's `TirRef`, resolved to the
/// binding's current `FatLocals` via `free_binding_names`.
#[derive(Clone, Debug)]
pub struct ConditionalDeadDrop {
    pub if_stmt: TirRef,
    pub target: TirRef,
    pub arms: Vec<BranchId>,
}

/// Side-table produced by the ownership pass alongside diagnostics.
/// Codegen consults it to decide where to emit `ryo_str_free` calls.
/// The TIR itself is never mutated — index stability is load-bearing
/// for `inst_values` memoisation in `codegen.rs`.
///
/// `TirRef`s are scoped per-function (each `Tir` arena restarts at
/// `TirRef(1)`), so the free schedule and per-instruction maps must
/// also be per-function — otherwise codegen processing function B
/// could pick up entries scheduled for function A whose TirRefs
/// happen to match B's at the same numeric index, emitting wrong/
/// extra `ryo_str_free` calls. One entry per `Tir`, positional with
/// the `tirs` slice handed to `ownership::check` and on to codegen;
/// keying by body index (not function name) is what keeps future
/// same-name functions in different scopes from silently colliding.
#[derive(Default, Debug, Clone)]
pub struct OwnershipSidecar {
    pub functions: Vec<FunctionSidecar>,
}

/// Per-function ownership metadata. Owns the three TirRef-keyed maps
/// that codegen consults during lowering. Created fresh by the
/// ownership pass (`ryo-frontend`'s `check`, one per body, in order)
/// and pushed onto the parent [`OwnershipSidecar`].
#[derive(Debug, Clone)]
pub struct FunctionSidecar {
    /// Name of the `Tir` this entry belongs to, recorded at push
    /// time. Positional indexing alone cannot detect a `tirs` slice
    /// that was reordered or filtered between `ownership::check` and
    /// codegen (same length, wrong alignment): codegen
    /// `debug_assert!`s this against `tir.name` so a misaligned entry
    /// fails loudly in debug builds instead of silently applying one
    /// function's frees to another.
    pub name: StringId,
    /// Frees anchored after specific instructions.
    pub free_schedule: Vec<FreePoint>,
    /// Reassignment Frees. Dense side table indexed by the `Assign`
    /// instruction's `TirRef::index()` (slot 0 unused — refs are
    /// 1-based), sized to the owning function's TIR arena length.
    /// `Some(target)` at slot `r` means: the buffer of `target` must be
    /// freed *before* the new fat-pointer triple is stored into the
    /// binding's `FatLocals`. Keys are always real instruction refs
    /// (never param sentinels); `target` itself may be a param sentinel
    /// ref for `inout` params.
    pub free_on_reassign: Vec<Option<TirRef>>,
    /// `BranchId` assignments per `IfStmt`. Dense side table indexed by
    /// the `IfStmt` instruction's `TirRef::index()` (slot 0 unused),
    /// sized to the owning function's TIR arena length. Codegen
    /// consults this when lowering if/elif/else to know which
    /// `BranchId` to push onto `branch_stack` for each arm.
    pub if_branches: Vec<Option<IfBranchIds>>,
    /// Conditional drops of pre-branch buffers for dead conditional
    /// reassignments. Codegen fires each entry at the start of
    /// the arms it names (including a synthetic fall-through block for
    /// else-less ifs).
    pub conditional_dead_drops: Vec<ConditionalDeadDrop>,
}

impl FunctionSidecar {
    /// Empty sidecar for the function named `name`, with the dense
    /// `TirRef`-indexed tables sized to the function's TIR arena
    /// length (`tir.instructions.len()`). `Default` is deliberately
    /// not derived: a sidecar without a recorded name would defeat the
    /// codegen alignment assert documented on [`Self::name`].
    pub fn new(name: StringId, arena_len: usize) -> Self {
        FunctionSidecar {
            name,
            free_schedule: Vec::new(),
            free_on_reassign: vec![None; arena_len],
            if_branches: vec![None; arena_len],
            conditional_dead_drops: Vec::new(),
        }
    }
}
