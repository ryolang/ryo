use crate::flow::{DropOp, DropPosition};
use crate::tir::{Span, TirRef};
use crate::types::StringId;

/// One scheduled promotion-buffer free. When a borrowed (non-inout)
/// `str`/`bytes` param is used as a view base, codegen promotes the
/// callee's inline copy to heap and records the result in a scratch
/// slot keyed by `base`; codegen emits a flag-conditional
/// `ryo_str_free(ptr, cap)` from that slot at the given position.
#[derive(Clone, Debug)]
pub struct PromoFree {
    pub position: DropPosition,
    pub base: TirRef,
    pub span: Span,
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

/// Per-function ownership metadata. Owns the four TirRef-keyed maps
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
    /// Explicit destruction operations, positioned on the Flow IR's
    /// execution paths. Codegen consumes them mechanically at the
    /// given positions — conditionality is expressed by block
    /// membership (an arm-only drop lives in the arm's blocks), never
    /// by a branch-gating predicate.
    pub drops: Vec<DropOp>,
    /// Reassignment Frees. Dense side table indexed by the `Assign`
    /// instruction's `TirRef::index()` (slot 0 unused — refs are
    /// 1-based), sized to the owning function's TIR arena length.
    /// `Some(target)` at slot `r` means: the buffer of `target` must be
    /// freed *before* the new fat-pointer triple is stored into the
    /// binding's `FatLocals`. Keys are always real instruction refs
    /// (never param sentinels); `target` itself may be a param sentinel
    /// ref for `inout` params.
    pub free_on_reassign: Vec<Option<TirRef>>,
    /// The lexical binding each `Assign` targets, recorded by the
    /// ownership walk. Dense side table indexed by the `Assign`
    /// instruction's `TirRef::index()`, sized like `free_on_reassign`.
    /// `Some(decl)` at slot `r` means: the name the `Assign` at `r`
    /// stores into resolved to the binding declared by `decl` — a
    /// `VarDecl` instruction's `TirRef`, or a param's virtual ref
    /// (`TirRef::param`) for param bindings. The free-scheduling passes
    /// use it to group reassignments by BINDING rather than by name:
    /// one name can denote several bindings (an explicit `mut` shadow or
    /// sibling-scope declarations), and name-grouped suppression lets a
    /// shadow binding's reassigns interfere with the outer binding's
    /// cleanup (a same-name reassign anchored after an outer owner's
    /// last use suppresses the outer owner's Free — leaking the outer
    /// buffer on paths where the outer reassign never ran).
    pub assign_binding: Vec<Option<TirRef>>,
    /// In-place concat selections (consuming-concat optimization).
    /// Dense side table indexed by the `Assign` instruction's
    /// `TirRef::index()`, sized like `free_on_reassign`. `Some(concat)`
    /// at slot `r` means: the Assign's value is the StrConcat/
    /// BytesConcat `concat` whose lhs is a plain local binding that
    /// dies exactly at this reassign (Valid owner, no live views — the
    /// same facts `free_on_reassign` already proves), so codegen may
    /// append the rhs onto the lhs buffer in place instead of
    /// allocating. The old buffer is CONSUMED, not orphaned: codegen
    /// must skip the `free_on_reassign` free for this Assign.
    pub consumed_concat_lhs: Vec<Option<TirRef>>,
    /// Field-reassignment Frees (M9). Dense side table indexed by the
    /// `FieldAssign`/`CompoundFieldAssign` instruction's
    /// `TirRef::index()`, sized like `free_on_reassign`. `Some(target)`
    /// at slot `r` means: the field designated by the `target`
    /// `FieldAccess` chain holds a needs-drop value that must be freed
    /// *before* the new value is stored into it. Codegen
    /// walks the chain to compute the old field's address.
    pub field_free_on_reassign: Vec<Option<TirRef>>,
    /// ALL scheduled destruction travels through this one DropOp list
    /// (the unified cleanup schedule): `AfterStep`/`BeforeStep`/`PreTerminator` anchors plus
    /// `BlockStart` drops — match-arm-start payload destruction (one
    /// entry per consuming arm, target = the scrutinee) and conditional
    /// dead drops of pre-branch buffers (one entry per untouched arm
    /// block, target = the pre-branch owner), both fired by codegen at
    /// the START of the named Flow block. Promotion-buffer frees for
    /// borrowed-param view bases keep their own anchor-keyed schedule
    /// (I-216).
    pub promotion_frees: Vec<PromoFree>,
    /// Promotion-buffer ALLOCATIONS (Gap 5): one entry per promo
    /// candidate, anchored at the view-creating step — codegen's
    /// promote-on-view writes the flag-guarded conditional heap triple
    /// there. The validator pairs each allocation with the base's
    /// releases (`promotion_frees`): the promoted copy is a DISTINCT
    /// identity from the borrowed original.
    pub promo_allocs: Vec<PromoFree>,
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
            drops: Vec::new(),
            free_on_reassign: vec![None; arena_len],
            assign_binding: vec![None; arena_len],
            consumed_concat_lhs: vec![None; arena_len],
            field_free_on_reassign: vec![None; arena_len],
            promotion_frees: Vec::new(),
            promo_allocs: Vec::new(),
        }
    }
}
