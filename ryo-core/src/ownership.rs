use crate::tir::{Span, TirRef};
use crate::types::StringId;
use std::collections::HashMap;

/// Identifies a specific arm of an `IfStmt` (and, future, `Match`).
/// Assigned by the ownership pass; codegen maps each `BranchId` to a
/// concrete Cranelift `Block` as it lowers if/else regions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct BranchId(pub u32);

/// One scheduled Free. Codegen emits `ryo_str_free(ptr, cap)` after
/// the instruction at `after`, gated by `branch` (None =
/// unconditional, Some = only inside that arm or a descendant).
#[derive(Clone, Debug)]
#[allow(dead_code)] // fields read by Task 7+ Free emission
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
#[allow(dead_code)] // fields read by codegen's branch_stack
pub struct IfBranchIds {
    pub then_branch: BranchId,
    pub elif_branches: Vec<BranchId>,
    pub else_branch: Option<BranchId>,
}

/// One conditional drop of a pre-branch buffer (I-117). When a binding
/// is reassigned in SOME arms of an if but kept untouched in others,
/// and the reassigned value is never read after the join, the
/// pre-branch buffer would leak on the untouched paths. This drop
/// covers it: codegen emits `ryo_str_free` at the START of each arm in
/// `arms` (the binding's value there is still the pre-if one).
/// `target` is the pre-branch owner's `TirRef`, resolved to the
/// binding's current `StrLocals` via `free_binding_names`.
#[derive(Clone, Debug)]
#[allow(dead_code)] // fields read by Task 7+ Free emission
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
/// extra `ryo_str_free` calls. Keyed by function name's `StringId`;
/// codegen looks up the entry for the current function before
/// consulting any of the per-function maps.
#[derive(Default, Debug, Clone)]
#[allow(dead_code)] // fields read by Task 7+ Free emission
pub struct OwnershipSidecar {
    pub functions: HashMap<StringId, FunctionSidecar>,
}

/// Per-function ownership metadata. Owns the three TirRef-keyed maps
/// that codegen consults during lowering. Created fresh inside
/// `analyze_function` and inserted into the parent
/// [`OwnershipSidecar`] under the function's name.
#[derive(Default, Debug, Clone)]
#[allow(dead_code)] // fields read by Task 7+ Free emission
pub struct FunctionSidecar {
    /// Frees anchored after specific instructions.
    pub free_schedule: Vec<FreePoint>,
    /// Reassignment Frees. Key: the `Assign` instruction's `TirRef`.
    /// Value: the `TirRef` whose buffer must be freed *before* the new
    /// fat-pointer triple is stored into the binding's `StrLocals`.
    pub free_on_reassign: HashMap<TirRef, TirRef>,
    /// `BranchId` assignments per `IfStmt`. Codegen consults this when
    /// lowering if/elif/else to know which `BranchId` to push onto
    /// `branch_stack` for each arm.
    pub if_branches: HashMap<TirRef /* IfStmt inst */, IfBranchIds>,
    /// Conditional drops of pre-branch buffers for dead conditional
    /// reassignments (I-117). Codegen fires each entry at the start of
    /// the arms it names (including a synthetic fall-through block for
    /// else-less ifs).
    pub conditional_dead_drops: Vec<ConditionalDeadDrop>,
}
