//! Canonical control flow over TIR ("Flow IR"), one per function body.
//!
//! Flow IR is the single authoritative execution structure and
//! storage-identity model shared by the ownership pass and the backend.
//! It is built once from a finished [`Tir`] (post-sema) and is never
//! mutated. TIR remains the typed instruction/value stream: Flow IR blocks
//! list the *same* instruction refs in the *same* evaluation order the
//! backend already uses (post-order operand evaluation, statements in
//! source order), so lowering order is preserved by construction.
//!
//! Design rules (see `docs/dev/ryo-compiler-llm-instructions.md`):
//! flat `Vec` arenas + `Copy` `u32` newtype indices (R1), no pointer
//! trees, exhaustive `match`es on [`Terminator`] (no wildcard — a new
//! control form is a compile error here and at every consumer).
//!
//! The representation contracts Flow IR provides:
//!
//! 1. **Control flow** — indexed [`BlockId`]s with an explicit
//!    [`Terminator`] per block: conditional branches, match dispatch
//!    ([`Terminator::Switch`]) with explicit per-arm blocks and join,
//!    loop headers/backedges, `break`/`continue` with resolved targets,
//!    returns, diverging calls, and plain jumps. A `match` in an
//!    initializer and a statement-level `match` are *both* a `Switch`;
//!    source position is never an execution property.
//! 2. **Storage identity** — [`PlaceId`] distinguishes a binding's
//!    identity ([`Place::Local`], keyed by the declaring `VarDecl` ref,
//!    never by interned name), params (by index), anonymous temporaries
//!    ([`Place::Temp`]), match/destructure by-move owner tokens
//!    ([`Place::Token`]), views with their protected owner
//!    ([`Place::View`]), match results ([`Place::MatchResult`]), and
//!    loop variables. Per-step [`StepFacts`] record reads, writes
//!    ([`WriteKind`]: init / replace / mutate / field-replace), and view
//!    creation explicitly.
//! 3. **Analysis results** — [`Analysis`] is tri-state: unavailable
//!    information is never conflated with proven absence.
//! 4. **Cleanup lowering** — [`DropOp`] positions destruction explicitly
//!    on execution paths (block start, before/after a step,
//!    pre-terminator); conditionality is block membership, so arm-gated
//!    frees are drops *inside* arm blocks, not metadata-checked anchors.

use std::collections::HashMap;
use std::ops::Range;

use crate::extra::ExtraRange;
use crate::tir::{Span, Tir, TirRef};
use crate::types::{InternPool, StringId, TypeId};

/// Indexed basic block. Blocks are single-entry; steps are listed in
/// evaluation order (see the module header).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlockId(pub u32);

/// Storage identity (see the module header). `Copy`, cheap to pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PlaceId(pub u32);

impl BlockId {
    /// Slot-0 sentinel analogue: builders never hand this out.
    pub const NONE: BlockId = BlockId(u32::MAX);
}

/// One evaluated instruction inside a block.
#[derive(Clone, Copy, Debug)]
pub struct Step {
    /// The TIR instruction this step evaluates.
    pub inst: TirRef,
    /// Explicit storage facts for this step.
    pub facts: StepFacts,
}

/// Explicit per-step ownership-relevant facts.
#[derive(Clone, Copy, Debug, Default)]
pub struct StepFacts {
    /// `Var` reads: the place this step reads. Projections
    /// (`FieldAccess`) read their base place. Call/operator arguments
    /// record their reads on their own steps; the *mode* of a call
    /// argument stays on the call's wire (sema-stamped `ParamMode`s).
    pub read: Option<PlaceId>,
    /// Writes (declarations, assignments, in-place mutation, field
    /// replacement, `inout` writeback).
    pub write: Option<PlaceWrite>,
    /// `Slice`/`ToView` steps: the view place this step creates; the
    /// protected owner is [`Place::View::base`].
    pub view: Option<PlaceId>,
}

/// A write of a place's storage.
#[derive(Clone, Copy, Debug)]
pub struct PlaceWrite {
    pub place: PlaceId,
    pub kind: WriteKind,
}

/// What kind of storage update a write performs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteKind {
    /// Declaration / first initialization.
    Init,
    /// Full replacement of the place's storage (assignment).
    Replace,
    /// In-place mutation (`push`, compound assign): the storage
    /// identity is preserved.
    Mutate,
    /// Replacement of one field of the base place's aggregate.
    FieldReplace { field: u32 },
    /// `inout` callee-side writeback through the caller's pointer.
    WriteBack,
}

/// A lexical/projected storage identity. Never keyed by name alone:
/// same-named bindings in different scopes are distinct places.
#[derive(Clone, Copy, Debug)]
pub enum Place {
    /// Function parameter, identified by position (never by name —
    /// params shadow and are shadowed like locals).
    Param {
        index: u32,
        name: StringId,
        ty: TypeId,
    },
    /// A `VarDecl` binding. `decl` is the declaring instruction — the
    /// stable identity across reassignment; shadowing declares a new
    /// `Local`.
    Local {
        decl: TirRef,
        name: StringId,
        ty: TypeId,
        mutable: bool,
    },
    /// A `for` loop induction variable, owned by the loop instruction.
    LoopVar {
        loop_inst: TirRef,
        name: StringId,
        ty: TypeId,
    },
    /// An anonymous owned value (concat result, call result, literal
    /// aggregate, …), identified by its producing instruction.
    Temp { producer: TirRef, ty: TypeId },
    /// A match/destructure by-move payload owner token (the
    /// `FieldAccess` instruction sema emits immediately before the
    /// match/destructure). Codegen never evaluates token instructions;
    /// the place table is their identity.
    Token {
        inst: TirRef,
        name: StringId,
        ty: TypeId,
    },
    /// A `strview`/`bytesview` created by a `Slice`/`ToView` step.
    /// `base` is the protected owner place.
    View {
        producer: TirRef,
        base: PlaceId,
        ty: TypeId,
    },
    /// The joined result storage of a value-position `match`.
    MatchResult { match_inst: TirRef, ty: TypeId },
}

impl Place {
    /// The declared type carried by the place.
    pub fn ty(&self) -> TypeId {
        match *self {
            Place::Param { ty, .. }
            | Place::Local { ty, .. }
            | Place::LoopVar { ty, .. }
            | Place::Temp { ty, .. }
            | Place::Token { ty, .. }
            | Place::View { ty, .. }
            | Place::MatchResult { ty, .. } => ty,
        }
    }

    /// The source name (a label for diagnostics/spans only — never an
    /// identity key).
    pub fn name(&self) -> StringId {
        match *self {
            Place::Param { name, .. }
            | Place::Local { name, .. }
            | Place::LoopVar { name, .. }
            | Place::Token { name, .. } => name,
            Place::Temp { .. } | Place::View { .. } | Place::MatchResult { .. } => {
                StringId::from_raw(0)
            }
        }
    }

    /// `true` for places that own destruction obligations:
    /// bindings/params/loop vars/temps/tokens/match results whose type
    /// needs drop. Views never own.
    pub fn owns_storage(&self, pool: &InternPool) -> bool {
        !matches!(self, Place::View { .. }) && pool.needs_drop(self.ty())
    }
}

/// Explicit block terminator — the exhaustive control-flow enum.
/// Adding a control form is a compile error at every consumer.
#[derive(Clone, Copy, Debug)]
pub enum Terminator {
    /// Unconditional jump.
    Goto { target: BlockId },
    /// Conditional branch (if/elif cascades: `else_` is the next elif's
    /// condition block, the else arm, or a synthetic fallthrough block).
    Branch {
        cond: TirRef,
        then_: BlockId,
        else_: BlockId,
    },
    /// `match` dispatch. `arms` (in TIR written order, wildcard
    /// included) lists each arm's block into `extra`; discriminants are
    /// decoded from the TIR match wire. Falling-through arms end in
    /// `Goto { join }`; diverging arms have no join edge. `join` is
    /// `None` iff every arm diverges (a `never` match) — the
    /// continuation block is then unreachable.
    Switch {
        match_inst: TirRef,
        scrutinee: TirRef,
        arms: ExtraRange,
        join: Option<BlockId>,
    },
    /// `while` loop at the pre-header block; jumps to `header`.
    While { loop_inst: TirRef, header: BlockId },
    /// `for` loop at the pre-header block; jumps to `header`.
    For { loop_inst: TirRef, header: BlockId },
    /// Loop-header condition test for a `while` (cond is a real TIR
    /// instruction evaluated in this block).
    WhileTest {
        loop_inst: TirRef,
        cond: TirRef,
        body: BlockId,
        exit: BlockId,
    },
    /// Loop-header test for a `for` range (condition compares the
    /// induction place against `end`; no TIR condition instruction
    /// exists, which is why this is its own variant).
    ForTest {
        loop_inst: TirRef,
        var: PlaceId,
        end: TirRef,
        body: BlockId,
        exit: BlockId,
    },
    /// End of a loop body: explicit backedge to the header. `inc` names
    /// the induction place a `for` latch increments on the edge
    /// (`None` for `while` loops, which carry no per-edge state).
    Backedge { to: BlockId, inc: Option<PlaceId> },
    /// `break`, with its loop exit block resolved.
    Break { target: BlockId },
    /// `continue`, with its loop header block resolved.
    Continue { target: BlockId },
    /// A call that never returns (`never`-typed — `panic`, …). The
    /// call instruction is the terminator payload; `next` receives the
    /// (unreachable) continuation.
    Call { call: TirRef, next: BlockId },
    /// `return expr`.
    Return { value: TirRef },
    /// `return` in a void function.
    ReturnVoid,
    /// Unreachable code (explicit `Unreachable`, or reached only via
    /// violated exhaustiveness).
    Unreachable,
}

/// Tri-state analysis fact: unavailable information is never conflated
/// with proven absence (a missing per-arm read map must not justify
/// refining away a live projection).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Analysis<T> {
    /// The analysis did not produce information for this fact.
    Unavailable,
    /// Proven absent (e.g. this branch provably has no reads).
    Absent,
    Present(T),
}

/// Where a cleanup drop fires. Conditionality is expressed by block
/// membership (an arm-only drop lives in the arm's block).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropPosition {
    // NOTE: variants documented below.
    /// Start of a block (arm-start payload drops, conditional dead
    /// drops, loop-exit paths).
    BlockStart { block: BlockId },
    /// Before the given step runs (free-before-overwrite).
    BeforeStep { inst: TirRef },
    /// After the given step (last-use destruction).
    AfterStep { inst: TirRef },
    /// Just before a block's terminator (break/continue/return paths).
    PreTerminator { block: BlockId },
}

impl DropPosition {
    /// The instruction this position is anchored on, when it is a
    /// step/terminator anchor (see [`DropOp`]). Positions that name a
    /// block directly return `None`. Cleanup-lowering scans that reason
    /// about anchor order (coverage, superseded-by-reassign) use this.
    pub fn anchor_inst(self) -> Option<TirRef> {
        match self {
            DropPosition::BeforeStep { inst } | DropPosition::AfterStep { inst } => Some(inst),
            DropPosition::BlockStart { .. } | DropPosition::PreTerminator { .. } => None,
        }
    }
}

/// The if arm blocks recorded while lowering the cascade (see
/// [`Flow::branch_arm_blocks`]). `then_block` is the first `Branch`'s
/// `then_`; each `elif_blocks` entry is the block HOLDING that elif's
/// `Branch` terminator — for a simple condition that is the condition
/// block itself; for a match / `and` / `or` condition (which cut blocks
/// and land their value in a join) it is the condition's join block.
/// The arm body is always that block's `then_` successor. The final
/// block covers BOTH the else body and the else-less fallthrough.
#[derive(Debug, Clone)]
pub struct BranchArms {
    pub then_block: BlockId,
    pub elif_blocks: Vec<BlockId>,
    /// Else body for an if/else; the empty fallthrough block for an
    /// else-less if (the synthetic block that jumps to the join).
    pub else_block: BlockId,
    /// The block after the cascade: every falling-through arm's
    /// `close_region` targets it. Recorded — never derived — so
    /// consumers can bound arm regions without reconstructing the
    /// cascade. Unreachable when every arm diverges (arms never reach
    /// it, so nothing follows the edge).
    pub join: BlockId,
}

/// One explicit destruction operation, produced by cleanup lowering and
/// consumed mechanically by the backend.
#[derive(Clone, Copy, Debug)]
pub struct DropOp {
    pub position: DropPosition,
    pub target: TirRef,
    pub span: Span,
}

/// Per-block header: steps are `steps[steps_start..steps_start+steps_len]`
/// in the global [`Flow::steps`] arena, in evaluation order.
/// `steps_start` is stamped on the block's FIRST step emission (blocks
/// created up front in a batch must not capture a creation-time start —
/// their ranges would overlap); a block with `steps_len == 0` never
/// uses `steps_start` and reads back as an empty slice.
#[derive(Clone, Copy, Debug)]
pub struct FlowBlock {
    pub steps_start: u32,
    pub steps_len: u32,
    /// `true` when the block's entry cannot be reached (dead code after
    /// a return / diverging call, or the continuation of an all-diverge
    /// match). Such blocks have no predecessors.
    pub unreachable: bool,
}

/// One function body's canonical control flow.
#[derive(Debug, Clone)]
pub struct Flow {
    pub entry: BlockId,
    pub blocks: Vec<FlowBlock>,
    /// Steps in global evaluation order; each block's range is its
    /// header's `steps_start`/`steps_len`.
    pub steps: Vec<Step>,
    /// One terminator per block.
    pub terms: Vec<Terminator>,
    /// The storage-identity table.
    pub places: Vec<Place>,
    /// Variable-length payloads (switch arm block lists).
    pub extra: Vec<u32>,
    succs: Vec<Range<u32>>,
    edges: Vec<BlockId>,
    preds: Vec<Range<u32>>,
    redges: Vec<BlockId>,
    /// Dense per-instruction metadata, one alloc per body (R3):
    /// step index, emission order, terminator block, and the
    /// place-table lookups consumers use.
    inst_meta: Vec<InstMeta>,
    token_places: HashMap<TirRef, Box<[PlaceId]>>,
    /// If/elif/else arm blocks recorded while lowering each `IfStmt`
    /// (see [`BranchArms`]). Consumers never reconstruct the cascade:
    /// conditions that cut blocks (match, `and`/`or`) do not hold their
    /// `Branch` in the condition-entry block.
    branch_arms: HashMap<TirRef, BranchArms>,
    /// Reverse of `inst_meta.term_block` for `IfStmt`s only: the block
    /// holding a cascade's FIRST `Branch` → the `IfStmt` it implements.
    /// A `Branch` without an entry here is a `BoolAnd`/`BoolOr` split or
    /// an elif's branch (none of which carry a payload), never an
    /// if-cascade entry.
    branch_if: HashMap<BlockId, TirRef>,
    /// The structural instruction each block's terminator implements
    /// (IfStmt/Match/loop/Return/Break/…), when one exists — the
    /// reverse of `inst_meta.term_block`, block-dense for O(1) queries.
    term_payload: Vec<Option<TirRef>>,
    place_write_ranges: Vec<Range<u32>>,
    place_write_list: Vec<TirRef>,
}

#[derive(Clone, Copy, Debug, Default)]
struct InstMeta {
    step: Option<u32>,
    order: Option<u32>,
    term_block: Option<u32>,
    /// The instruction that consumes this one as an operand (the
    /// parent step in the single-parent TIR tree). Stamped at build;
    /// `None` for roots and non-stepped instructions.
    parent: Option<u32>,
    /// The block containing this instruction's step (stamped at
    /// `push_step`; `None` for non-stepped instructions). Dense
    /// replacement for scanning `blocks` in `step_block_of`.
    block: Option<u32>,
    local: Option<PlaceId>,
    temp: Option<PlaceId>,
    match_result: Option<PlaceId>,
    loop_var: Option<PlaceId>,
}

impl Flow {
    /// Build the canonical Flow IR for one function body. Panics (debug
    /// builds only, via [`Flow::validate`]) on TIR that violates the
    /// invariants Flow IR depends on; well-formed post-sema TIR always
    /// passes.
    pub fn build(tir: &Tir, pool: &InternPool) -> Flow {
        build::build_flow(tir, pool)
    }

    /// This block's steps, in evaluation order. Step-less blocks (empty
    /// fallthroughs, dead continuations) return an empty slice.
    pub fn block_steps(&self, b: BlockId) -> &[Step] {
        let blk = &self.blocks[b.0 as usize];
        if blk.steps_len == 0 {
            return &[];
        }
        &self.steps[blk.steps_start as usize..(blk.steps_start + blk.steps_len) as usize]
    }

    /// The block's terminator.
    pub fn term(&self, b: BlockId) -> &Terminator {
        &self.terms[b.0 as usize]
    }

    /// Successor blocks (control-flow traversal).
    pub fn successors(&self, b: BlockId) -> &[BlockId] {
        let r = &self.succs[b.0 as usize];
        &self.edges[r.start as usize..r.end as usize]
    }

    /// Predecessor blocks (control-flow traversal).
    pub fn predecessors(&self, b: BlockId) -> &[BlockId] {
        let r = &self.preds[b.0 as usize];
        &self.redges[r.start as usize..r.end as usize]
    }

    /// The step evaluating `inst`, if it is an evaluated instruction.
    pub fn step_of(&self, inst: TirRef) -> Option<Step> {
        self.inst_meta
            .get(inst.index())
            .and_then(|m| m.step)
            .map(|i| self.steps[i as usize])
    }

    /// Facts for `inst`'s step.
    pub fn facts_of(&self, inst: TirRef) -> Option<&StepFacts> {
        self.inst_meta
            .get(inst.index())
            .and_then(|m| m.step)
            .map(|i| &self.steps[i as usize].facts)
    }

    /// The global step index evaluating `inst`, if it is a stepped
    /// instruction. The position within the step's block is
    /// `step_index_of(inst) - blocks[block].steps_start`.
    pub fn step_index_of(&self, inst: TirRef) -> Option<u32> {
        self.inst_meta.get(inst.index()).and_then(|m| m.step)
    }

    /// The block containing this instruction's step, if it is a step.
    pub fn step_block_of(&self, inst: TirRef) -> Option<BlockId> {
        self.inst_meta
            .get(inst.index())
            .and_then(|m| m.block)
            .map(BlockId)
    }

    /// The if/elif/else arm blocks of an `IfStmt`, in source order —
    /// recorded while lowering the cascade, never reconstructed.
    /// `then_block` is the then body; each `elif_blocks` entry is the
    /// block holding that elif's `Branch` (the condition block for a
    /// simple condition, the condition's join block when the condition
    /// is a match/`and`/`or`); the arm body is that block's `then_`
    /// successor. The final block covers BOTH the else body and the
    /// else-less fallthrough — the two are indistinguishable downstream
    /// (dead drops fire at its start in both cases).
    pub fn branch_arm_blocks(&self, if_inst: TirRef) -> BranchArms {
        self.branch_arms
            .get(&if_inst)
            .expect("IfStmt arm blocks recorded at build")
            .clone()
    }

    /// The `IfStmt` whose cascade's FIRST `Branch` terminates block `b`,
    /// if any. Recorded at build; a `Branch` without an entry here is a
    /// `BoolAnd`/`BoolOr` split (or an elif's branch) — consumers dispatch
    /// on context, never on the shape of `Terminator::Branch` alone.
    pub fn branch_if(&self, b: BlockId) -> Option<TirRef> {
        self.branch_if.get(&b).copied()
    }

    /// The structural instruction this block's terminator implements
    /// (the IfStmt/Match/loop/Return/… payload recorded at build), if the
    /// terminator carries one. `BoolAnd`/`BoolOr` splits, elif branches,
    /// loop tests and backedges carry no payload.
    pub fn term_payload(&self, b: BlockId) -> Option<TirRef> {
        self.term_payload.get(b.0 as usize).copied().flatten()
    }

    /// The block whose terminator carries this structural instruction
    /// (match/if/loop/return/…), if any.
    pub fn term_block_of(&self, inst: TirRef) -> Option<BlockId> {
        self.inst_meta
            .get(inst.index())
            .and_then(|m| m.term_block)
            .map(BlockId)
    }

    /// Global emission order stamp for an instruction (lower = earlier).
    pub fn order_of(&self, inst: TirRef) -> Option<u32> {
        self.inst_meta.get(inst.index()).and_then(|m| m.order)
    }

    /// The place a `VarDecl` declares.
    pub fn local_of_decl(&self, decl: TirRef) -> Option<PlaceId> {
        self.inst_meta.get(decl.index()).and_then(|m| m.local)
    }

    /// The anonymous temp place for a producing instruction.
    pub fn temp_of(&self, producer: TirRef) -> Option<PlaceId> {
        self.inst_meta.get(producer.index()).and_then(|m| m.temp)
    }

    /// The joined result place of a value `match`.
    pub fn match_result(&self, match_inst: TirRef) -> Option<PlaceId> {
        self.inst_meta
            .get(match_inst.index())
            .and_then(|m| m.match_result)
    }

    /// The induction-variable place of a `for` loop.
    pub fn loop_var(&self, loop_inst: TirRef) -> Option<PlaceId> {
        self.inst_meta
            .get(loop_inst.index())
            .and_then(|m| m.loop_var)
    }

    /// The instruction that consumes `inst` as an operand — the parent
    /// step in the TIR's single-parent tree. `None` for statement
    /// roots and non-stepped instructions. Consumers of Flow facts
    /// use this instead of re-walking TIR operand trees.
    pub fn parent_of(&self, inst: TirRef) -> Option<TirRef> {
        self.inst_meta
            .get(inst.index())
            .and_then(|m| m.parent)
            .map(TirRef::from_raw)
    }

    /// Every (match/destructure inst, its token places) pair — the
    /// build-time table behind [`Flow::token_places`]. Consumed by the
    /// ownership pass's one-time owner-place index.
    pub fn token_places_iter(&self) -> impl Iterator<Item = (TirRef, &[PlaceId])> + '_ {
        self.token_places
            .iter()
            .map(|(inst, places)| (*inst, &places[..]))
    }

    /// The by-move owner-token places adopted at a match/destructure
    /// instruction (arm order, binding order, non-`Copy` only).
    pub fn token_places(&self, inst: TirRef) -> &[PlaceId] {
        static EMPTY: [PlaceId; 0] = [];
        match self.token_places.get(&inst) {
            Some(p) => &p[..],
            None => &EMPTY,
        }
    }

    /// Every write to a place, in emission order (storage lineage).
    pub fn place_writes(&self, place: PlaceId) -> &[TirRef] {
        let r = &self.place_write_ranges[place.0 as usize];
        &self.place_write_list[r.start as usize..r.end as usize]
    }

    /// The place table entry.
    pub fn place(&self, id: PlaceId) -> &Place {
        &self.places[id.0 as usize]
    }

    /// Blocks in a valid forward propagation order (construction order
    /// is one: operands dominate their uses within and across blocks).
    pub fn forward_order(&self) -> impl Iterator<Item = BlockId> + '_ {
        (0..self.blocks.len() as u32).map(BlockId)
    }

    /// Debug-only structural validation of every Flow IR invariant; called
    /// once at the end of [`Flow::build`].
    #[cfg(debug_assertions)]
    fn validate(&self, tir: &Tir, pool: &InternPool) {
        validate::validate_flow(self, tir, pool);
    }

    #[cfg(not(debug_assertions))]
    fn validate(&self, _tir: &Tir, _pool: &InternPool) {}
}

pub mod build;
pub mod fire_points;
#[cfg(test)]
mod tests;
#[cfg(debug_assertions)]
mod validate;
