//! Per-function emission state (`FunctionContext`) — split from `mod.rs`;
//! see module docs there.

use super::abi::LoweredAbi;
use super::match_;
use super::ranges;
use super::{
    BlockPrologue, CodegenNameIds, DeclaredFunctions, FatLocals, IfRecord, LoopRecord, ValueRepr,
    ViewLocals,
};
use cranelift::codegen::ir::StackSlot;
use cranelift::prelude::*;
use cranelift_module::{DataDescription, DataId, FuncId, Module};
use ryo_core::tir::{Tir, TirRef};
use ryo_core::types::{InternPool, StringId, TypeId};
use std::collections::HashMap;

/// Per-function emission state. Lives only for the duration of one
/// `compile_function` call; reset between functions because
/// Cranelift `Variable` ids and the `TirRef → Value` memo are both
/// function-local — and because `TirRef` itself is scoped to a
/// single `Tir`.
pub(crate) struct FunctionContext<'a, M: Module> {
    pub(super) optional_optimizations: bool,
    pub(super) module: &'a mut M,
    pub(super) data_ctx: &'a mut DataDescription,
    pub(super) string_data: &'a mut HashMap<StringId, DataId>,
    pub(super) int_type: types::Type,
    pub(super) pool: &'a InternPool,
    pub(super) tir: &'a Tir,
    /// Whether the function being lowered is `main`, resolved by
    /// `compile_function` from [`CodegenNameIds`]. The ReturnVoid arm
    /// needs it to emit the C ABI's int-0 return.
    pub(super) is_main: bool,
    /// Whether the function being lowered was compiled with
    /// `CallConv::Tail`: its name was in `Codegen`'s
    /// `tail_candidates` when `lower_abi` ran. `return_call` is
    /// only legal from a `tail`-convention function, so the tail path
    /// consults this before attempting one.
    pub(super) tail_conv_candidate: bool,
    /// Interned-name ids resolved once per compilation; the per-call
    /// dispatch in `emit_call_slot`, the literal hoisting exclusion,
    /// and the slot-out exclusion all compare `StringId` equality
    /// against these.
    pub(super) name_ids: CodegenNameIds,
    /// Scalar binding name → Cranelift `Variable`. Dense table indexed
    /// by `StringId::raw()`, sized once per function from
    /// `pool.string_count()` (codegen never interns, so every name in
    /// the TIR is in bounds). Binding names are not unique per
    /// function — a nested VarDecl shadows the outer slot — so every
    /// write goes through `write_slot`, which records the previous
    /// slot value on `locals_undo`; the Flow-block driver's region
    /// prologues replay the log down to each construct's marks
    /// so sibling regions never observe each other's bindings.
    pub(super) locals: Vec<Option<Variable>>,
    pub(super) locals_undo: Vec<(u32, Option<Variable>)>,
    /// Value-range facts for spec §18 guard elision: binding name →
    /// inclusive `[lo, hi]` bounds proven by dominating comparisons
    /// (see `codegen/ranges.rs` for the seeding rules). Dense
    /// `StringId::raw()`-indexed table with an undo log, scoped like
    /// `locals`: the driver's join/exit prologues restore it via
    /// `restore_slots`, and any binding assigned inside a restored
    /// scope is killed at the join via `assigned_log`.
    pub(super) range_facts: Vec<Option<ranges::IntRange>>,
    pub(super) range_facts_undo: Vec<(u32, Option<ranges::IntRange>)>,
    /// Flat log of every binding assigned (or passed as `inout`) since
    /// function entry. Branch/loop emitters snapshot the length before
    /// a scoped body and kill those names from `range_facts` after the
    /// restore, so a join whose predecessors disagree invalidates the
    /// fact instead of resurrecting the pre-scope one. Distinct from
    /// the slot-table undo logs: those restore pre-scope slot values,
    /// while this drives the resurrect-then-re-kill join invalidation.
    pub(super) assigned_log: Vec<StringId>,
    pub(super) func_ids: &'a DeclaredFunctions,
    pub(super) abi: &'a LoweredAbi,
    /// `TirRef → ValueRepr` memo. Materializing the same instruction
    /// twice in one function would either duplicate side effects
    /// (calls) or waste Cranelift IR; both are cheap-but-wrong.
    ///
    /// Dense table indexed directly by `TirRef::index()` (slot 0
    /// unused — refs are 1-based), sized once per function from the
    /// TIR instruction count: TIR is tree-shaped today, so the memo
    /// almost never hits and per-instruction HashMap hashing was pure
    /// overhead. Param sentinel refs (`TirRef::param`) are not valid
    /// arena indices and live in `param_values` instead.
    ///
    /// INVARIANT: this table is deliberately cross-block (one flat
    /// table per function, not scoped per basic block). That is sound
    /// only because the current TIR producers guarantee:
    ///   (a) TIR instructions are unique per use — no shared
    ///       sub-expressions, so a `TirRef` is materialized in exactly
    ///       one block and read only where that block dominates;
    ///   (b) `BoolAnd`/`BoolOr` merge values via the short-circuit
    ///       join block's I8 param (phi node), so the memoized `Value`
    ///       is the join-block param, which dominates every downstream
    ///       use;
    ///   (c) `IfStmt` is statement-level — no values flow out of
    ///       branches, so no branch-local value is ever read after the
    ///       merge. `TirTag::Match` (M12) is expression-level control
    ///       flow, but its results flow out through a pre-allocated
    ///       result slot (loaded/cached at the join by the MatchJoin
    ///       prologue — never a block param), and arm-body expressions
    ///       are scoped to their arm block (never read across the
    ///       merge).
    /// If a future TIR producer introduces expression-level control
    /// flow (ternary if) or shared sub-expressions across blocks, this
    /// memo MUST be re-scoped per-block or reads will hit Cranelift
    /// dominator errors.
    pub(super) inst_values: Vec<Option<ValueRepr>>,
    /// Memo entries for param sentinel refs (`TirRef::param`), which
    /// cannot index `inst_values`. Dense table indexed by
    /// `TirRef::as_param_index()`, sized once per function from
    /// `tir.params.len()`. Only fat (str/bytes) and view params ever
    /// land here.
    pub(super) param_values: Vec<Option<ValueRepr>>,
    /// Dense flag per `sidecar.drops` index: true once that entry's
    /// Free has been consumed — either emitted at its Flow fire
    /// position or marked consumed by the lowering-time provably-inline
    /// pre-filter (position-mechanical: elision is a lowering-time no-op-free decision,
    /// not an ownership decision). Each drop lives in exactly one fire
    /// bucket, so consumption is exactly once by construction; the flag
    /// both guards the pre-filter against the fire paths and backs the
    /// debug-only all-consumed assertion.
    pub(super) freed_at: Vec<bool>,
    /// Debug-only: per-drop flag marking drops consumed AT A FIRE
    /// POINT (the fire-map buckets, including same-target duplicates
    /// consumed there) — as opposed to the lowering-time pre-filter's
    /// marks in `freed_at`. The split lets the place-resolution
    /// assertion check every pre-elided drop against the elision
    /// predicate (a pre-elided drop never resolved storage, so the
    /// "has storage" leg must not excuse it) and every fired drop
    /// against storage resolution.
    #[cfg(debug_assertions)]
    pub(super) fire_marked: Vec<bool>,
    /// Targets whose destruction has already been emitted in the
    /// CURRENT block visit (position-mechanical) — cleared per block, so the dedup is
    /// per-path, not global: the ownership pass deliberately
    /// double-anchors some frees (the consumer step AND the enclosing
    /// Return's epilogue, both firing in the same block's terminator
    /// handling) — a sweep-era belt-and-suspenders that relied on the
    /// duplicate's anchor being unmaterializable. Fired mechanically
    /// from position, both duplicates would run, so the
    /// once-per-target-per-path invariant is enforced at consumption: a
    /// drop whose target was already destroyed in this block is consumed
    /// as a no-op. Same-target drops in DIFFERENT blocks are different
    /// paths and all fire. Elided (no-op) drops do NOT record their
    /// target here — the buffer was never freed, so a later same-target
    /// drop must still fire.
    pub(super) freed_targets: std::collections::HashSet<TirRef>,
    /// The drop fire map (position-mechanical): scheduled drops keyed by their Flow
    /// fire position, built once per function before emission. Firing
    /// is mechanical from the Flow position — no cached-value
    /// representation, pending-sweep state, or materialization order
    /// decides whether a drop fires.
    ///
    /// * `fire_before_step[inst]` — `BeforeStep{inst}` drops, fired by
    ///   the block driver immediately before the step emits.
    /// * `fire_after_stmt[inst]` — `AfterStep` drops whose anchor is a
    ///   step of the statement rooted at `inst` (a value-step anchor
    ///   fires at its enclosing statement's end — after the consumer,
    ///   the only sound position for a last-use free), fired by the
    ///   post-statement hook.
    /// * `fire_by_payload[inst]` — `AfterStep` drops anchored on a
    ///   terminator payload (if/loop/match/return/break/continue
    ///   instruction), fired by that construct's handler at its single
    ///   emit position (the join/exit prologue, the terminator arm).
    /// * `fire_at_term[block]` — `PreTerminator{block}` drops plus
    ///   `AfterStep` drops whose anchor step has no enclosing statement
    ///   (match-arm body expressions, deferred return operands), fired
    ///   inside the block's terminator handler at its canonical point
    ///   (after operand/body evaluation, before the jump/return).
    pub(super) fire_before_step: Vec<Vec<usize>>,
    pub(super) fire_after_stmt: Vec<Vec<usize>>,
    pub(super) fire_by_payload: Vec<Vec<usize>>,
    pub(super) fire_at_term: HashMap<ryo_core::flow::BlockId, Vec<usize>>,
    /// `BlockStart` drops (position-mechanical unification): fired once per
    /// Flow block, when the driver switches into the mapped Cranelift
    /// block, BEFORE the block's steps. Two kinds share the bucket,
    /// told apart at the fire point by target identity: match-arm-start
    /// payload destruction (the block is an arm of a match and the
    /// target is that match's scrutinee — codegen recomputes the arm's
    /// unbound needs-drop fields and drops each) and conditional dead
    /// drops of pre-branch buffers (target = the pre-branch owner,
    /// resolved through the binding tables exactly as the old
    /// block-start emission did).
    pub(super) fire_at_block_start: HashMap<ryo_core::flow::BlockId, Vec<usize>>,
    /// ---- Flow-block driver state ----
    /// One Cranelift block per Flow IR block, pre-created 1:1 before
    /// the driver runs. The entry Flow block reuses the function's
    /// entry block (index 0). The ONLY block params: a Flow block whose
    /// first step is a `BoolAnd`/`BoolOr` (the short-circuit join) gets
    /// one I8 param — the merged boolean. Every other value merge
    /// (match results) rides a pre-allocated result slot, never a
    /// block param.
    pub(super) flow_blocks: Vec<Block>,
    /// Per-Flow-block sealing discipline: a block seals once every Flow
    /// predecessor's terminator has been emitted (loop headers
    /// naturally wait for their backedge); unreachable no-predecessor
    /// blocks self-seal right after their own terminator. Entry starts
    /// sealed.
    pub(super) flow_sealed: Vec<bool>,
    /// Per-Flow-block count of emitted predecessor terminators, for
    /// the sealing rule above.
    pub(super) flow_preds_emitted: Vec<u32>,
    /// Debug-only visit state per Flow block: low 7 bits count
    /// emissions (the driver must visit each block at most once), bit 7
    /// marks the terminator dispatched (a tail call counts — the
    /// `return_call` terminates the block). Release builds compile this
    /// out entirely.
    #[cfg(debug_assertions)]
    pub(super) flow_visit_state: Vec<u8>,
    /// The prologue the driver runs when reaching each block (scope
    /// restores / fact seeding / match merge handling).
    pub(super) block_prologue: Vec<BlockPrologue>,
    /// Dense flag: the block's first step is a `BoolAnd`/`BoolOr` — its
    /// Cranelift block carries the merged-boolean I8 param, and edges
    /// INTO it pass the merge value (constant on the short-circuit
    /// edge, the rhs value on the rhs edge).
    pub(super) bool_join: Vec<bool>,
    /// Dense flag: the block's last step is an `ExprStmt` calling a
    /// function, in tail position (last statement of the function body
    /// or of an if-cascade arm body, no loop/match arm in between).
    /// The driver defers the operand subtree so `try_emit_tail_call`
    /// sees exactly the cache state the old recursive emitter saw.
    pub(super) tail_candidate: Vec<bool>,
    /// Dense map: arm fallthrough block → the `IfStmt` it belongs to
    /// (then block, elif body block, or else block). Drives the tail
    /// recursion (`construct_is_last`) and confirms a Goto-target arm
    /// region is an if arm.
    pub(super) if_arm_owner: Vec<Option<TirRef>>,
    /// Dense map: block → the match whose join block it is. A match
    /// join can coincide with another region entry (an elif whose
    /// condition is a match holds its Branch in the condition's join
    /// block, and a `while match …:` header is the match's join), so
    /// the join handling (result caching + fact/slot restores) runs
    /// from this map BEFORE the block's region prologue, independent
    /// of the `block_prologue` variant.
    pub(super) match_join: Vec<Option<TirRef>>,
    /// Dense map: a match arm's fallthrough block (the block ending in
    /// `Goto { join }`) → (match, arm). Each falling-through arm
    /// contributes exactly one such edge; the source can be deep inside
    /// the arm (a nested value match's join), so a prologue-based
    /// source check is not enough — the owner arm is found by a
    /// forward walk from each arm block at map time.
    pub(super) match_fallthrough: Vec<Option<(TirRef, usize)>>,
    /// Construct records, created when the construct's entry block is
    /// emitted (marks taken then), consumed by prologues and handlers.
    pub(super) if_records: HashMap<TirRef, IfRecord>,
    pub(super) loop_records: HashMap<TirRef, LoopRecord>,
    /// Per-match state for the Flow-block driver: fact/slot
    /// marks (taken at the pre-step hook), the scrutinee home, the
    /// merge shape, the pre-allocated result slot, and per-arm token
    /// offsets. Created before the scrutinee steps emit, filled by the
    /// Switch terminator handler, consumed by arm prologues, the join
    /// prologue, and arm fallthroughs.
    pub(super) match_records: HashMap<TirRef, match_::MatchRecord>,
    /// Whether the current function returns through a hidden sret
    /// pointer (fat/struct/enum) — the synthesized fall-off return
    /// needs it, recomputed from the signature per function.
    pub(super) has_sret: bool,
    /// Owned fat (str/bytes) bindings: three SSA `Variable`s (ptr, len,
    /// cap) per binding name. Dense `StringId::raw()`-indexed table with
    /// an undo log, same scoping discipline as `locals`.
    pub(super) fat_locals: Vec<Option<FatLocals>>,
    /// Storage survives lexical name restoration until its resolved cleanup fires.
    /// Entries identify representation only; Flow establishes ownership.
    pub(super) fat_binding_storage: HashMap<TirRef, FatLocals>,
    pub(super) assignment_aliases: Vec<bool>,
    pub(super) cleanup_plan: ryo_core::flow::cleanup::CleanupPlan,
    pub(super) fat_locals_undo: Vec<(u32, Option<FatLocals>)>,
    /// Borrowed-param view bases (Slice/ToView) that were promoted
    /// through a scratch slot instead of the param's FatLocals: base
    /// inst → slot holding flag (offset 0) + promoted triple (8/16/24).
    pub(super) promo_slots: HashMap<TirRef, StackSlot>,
    /// Dense flag per `sidecar.promotion_frees` index (emission-time
    /// dedup, same discipline as `freed_at`).
    pub(super) promo_freed_at: Vec<bool>,
    /// `strview` view bindings (M8.4): two SSA `Variable`s per binding,
    /// mirroring `fat_locals`. Views are non-owning — they never
    /// appear in the free schedule.
    pub(super) view_locals: Vec<Option<ViewLocals>>,
    pub(super) view_locals_undo: Vec<(u32, Option<ViewLocals>)>,
    /// Struct-typed bindings (M9): one address-holding `Variable` per
    /// binding — the value itself lives in a stack slot (see
    /// `codegen/structs.rs`). Dense `StringId::raw()`-indexed table
    /// with an undo log, same scoping discipline as `locals`.
    pub(super) struct_locals: Vec<Option<Variable>>,
    pub(super) aggregate_binding_storage: HashMap<TirRef, Variable>,
    pub(super) struct_locals_undo: Vec<(u32, Option<Variable>)>,
    /// Free-target (initializer / Assign value / fat-param virtual ref)
    /// → binding-name map, built once per function by
    /// `build_free_binding_names`. `emit_frees` uses it to release a
    /// named binding's CURRENT `FatLocals` rather than the producing
    /// init's possibly-stale cached repr.
    ///
    /// Split into two dense tables dispatched on `TirRef::is_param()`
    /// (same shape as `cached_repr`): `free_binding_names` is indexed
    /// by `TirRef::index()` for real instruction refs (slot 0 unused),
    /// `free_binding_param_names` by `TirRef::as_param_index()` for
    /// fat-param sentinel refs.
    pub(super) free_binding_names: Vec<Option<StringId>>,
    pub(super) free_binding_param_names: Vec<Option<StringId>>,
    /// M8.3 inout parameters: for each inout param's name, the
    /// caller-provided slot address (a function-entry block param)
    /// and its pointee `TypeId`. The write-back chokepoint stores each
    /// param's current `Variable` back through this pointer before
    /// every `return_`. Scalars store one field at offset 0; fat
    /// (str/bytes) pointees store three fields.
    ///
    /// Dense table indexed by `StringId::raw()`, sized once per
    /// function from `pool.string_count()` (codegen never interns, so
    /// every name in the TIR is in bounds). Never scoped — params are
    /// bound at function entry — so it has no undo log.
    pub(super) inout_ptrs: Vec<Option<(Value, TypeId)>>,
    pub(super) scalar_param_storage: Vec<Option<Variable>>,
    /// For fat-returning functions: the hidden sret pointer (first block param)
    /// through which the callee writes the (ptr, len, cap) triple.
    pub(super) sret_ptr: Option<Value>,
    /// Ownership sidecar for the function currently being lowered.
    /// `TirRef`s are scoped per-function — each `Tir`'s arena restarts
    /// at `TirRef(1)` — so codegen must consult only the entry that
    /// belongs to this function. `compile_function` picks
    /// `sidecar.functions[i]`, positional with the `tirs` slice, and
    /// threads the resulting per-function entry here. Drops are
    /// consumed mechanically at their anchor instructions; arm-only
    /// drops are gated by block membership — their anchors are only
    /// evaluated on the arm's path — not by any branch predicate.
    pub(super) sidecar: &'a ryo_core::ownership::FunctionSidecar,
    /// The canonical Flow IR for this body — the backend's emission
    /// driver: one pre-created Cranelift block per Flow block,
    /// walked in DFS order from the entry, with per-step evaluation and
    /// terminator handlers. Positional with the sidecar entry (both
    /// are built once per pipeline run, right after sema).
    pub(super) flow: &'a ryo_core::flow::Flow,
    /// Module-level cache for compiler-generated guard messages;
    /// see `Codegen::guard_msg_data`.
    pub(super) guard_msg_data: &'a mut HashMap<&'static str, DataId>,
    /// Module-level import cache for runtime functions; see
    /// `Codegen::runtime_fns`.
    pub(super) runtime_fns: &'a mut HashMap<&'static str, FuncId>,
    /// Cold panic blocks for guard failures (overflow, div-by-zero),
    /// paired with their message so all guards with the same message
    /// share one block. A Vec (not a map) keeps the drain order
    /// deterministic — identical input must produce identical binary
    /// bytes — and there are only two guard messages in total.
    /// Emitted at end-of-function (see `compile_function`) so the hot
    /// path falls through the guard `brif` instead of jumping over
    /// inline panic code.
    pub(super) panic_blocks: Vec<(&'static str, Block)>,
    /// Cold block for the stack-limit check, created by the
    /// first `emit_stack_check` in the function and shared by every
    /// later check; emitted end-of-function beside the deferred panic
    /// blocks. `None` while no check has been emitted.
    pub(super) stack_cold_block: Option<Block>,
    /// SCC ids of the recursive functions (`Codegen::recursive_sccs`):
    /// a user call gets the stack-limit check iff caller and callee
    /// share an id (a call-graph cycle edge).
    pub(super) recursive_sccs: &'a HashMap<StringId, u32>,
    /// Block the last stack-limit check fell through into. The stack
    /// pointer is fixed for the whole function body, so a check that
    /// already ran earlier in the current block covers every later
    /// cycle-edge call in it (e.g. both recursive calls of
    /// `fibonacci`).
    pub(super) stack_checked_block: Option<Block>,
}
