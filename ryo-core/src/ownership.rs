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
}
