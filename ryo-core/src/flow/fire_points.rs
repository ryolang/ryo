//! Drop firing-point resolution: the ONE deterministic mapping from a
//! cleanup [`DropPosition`] to its exact executable point(s), shared by
//! the backend's fire-map build (codegen) and the frontend's cleanup
//! validator (ownership/validate.rs). Neither side may re-interpret a
//! position privately; this module is the behavioral contract both
//! consume.
//!
//! A [`FirePoint`] is a `(BlockId, ordinal)` pair; ordinals run
//! `0..=n_steps` in a block — 0 is the block entry, ordinal i fires
//! between step i-1 and step i, and `n_steps` is the terminator
//! position (the point the block's terminator handler fires at, after
//! payload/body evaluation and before the jump).
//!
//! Resolution rules (the backend's pre-unification lowering is the
//! behavioral ground truth; the CLIF assertion tests and the Linux
//! ASan/Valgrind suites pin it):
//!
//! - `BlockStart{block}` → `(block, 0)`; `PreTerminator{block}` →
//!   `(block, n_steps)`; `BeforeStep{inst}` → the ordinal before the
//!   anchor's step.
//! - `AfterStep{inst}` on a stepped instruction → the ENCLOSING
//!   STATEMENT boundary: the first statement-root step at/after the
//!   anchor in its block, resolved to the point AFTER that root step. A
//!   statement-root step is a stepped instruction that is not an
//!   `Operand`-kind child of any other stepped instruction or
//!   terminator payload (see [`Flow::parent_of`] and
//!   [`Flow::term_block_of`]); the root table is computed once per body
//!   at [`FirePoints::new`]. No root step at/after the anchor → the
//!   block's terminator point.
//! - `AfterStep{inst}` on a terminator payload → the
//!   construct-completion point(s): an `IfStmt` resolves to EVERY
//!   non-diverging arm's continuation block (empty when every arm
//!   diverges — the anchor can never fire); a `match` (`Switch`)
//!   resolves to its join block (empty when all arms diverge); a
//!   `while`/`for` resolves to its exit block; `Return`/`Break`/
//!   `Continue` (and any other payload) resolve to the payload block's
//!   terminator point. A `while` whose header carries a compound
//!   condition (`and`/`or`/`match` — the header's terminator is not the
//!   loop test) resolves to the payload block's terminator as a
//!   NON-exact point: the bucket still fires at the loop exit, but the
//!   precise modeling is unproven, so consumers must treat the point as
//!   a lower bound (see [`FireVerdict::NonExact`]).
//! - Anything else — an anchor that is neither a step nor a terminator
//!   payload, or a block id the body does not contain — is
//!   [`FireVerdict::Unresolved`]: internal schedule corruption. The
//!   resolver never guesses.
//!
//! Contract rules for consumers:
//!
//! (a) *An operand is never freed before its borrowing consumer
//! finishes.* A last-use drop anchored on a stepped instruction places
//! its event at the consuming statement's boundary — the statement root
//! is the LAST step of its statement in evaluation order (post-order
//! operand evaluation), so the point after it lands after every operand
//! evaluation and after the consumer step itself. This holds by
//! construction of the root table: steps between an anchor and its
//! statement's root are `Operand`-kind children of the same consuming
//! tree, hence non-roots, so the first root at/after the anchor IS the
//! anchor's consuming statement boundary.
//!
//! (b) *Deliberate same-position dedup.* Two events naming the SAME
//! target at the SAME resolved point are a designed no-op pair (the
//! consumer + return-epilogue double-anchor) — the resolver contract
//! names this pair; the backend's per-block-visit `freed_targets` dedup
//! (cleared on re-entry) is the consumption-side mechanism that
//! consumes it. Pairs of this shape are never double-fires.
//!
//! (c) *Static/no-op elision is not an event.* Drops whose target is a
//!   static `.rodata` literal (`StrConst`/`BytesConst`, cap == 0
//!   sentinel) or a provably-inline producer (output never exceeds
//!   `INLINE_CAP`) are pre-elided by the backend's lowering-time
//!   pre-filter: the runtime free is a guaranteed no-op, the drop is
//!   marked consumed, and it never fires. Elided drops therefore are
//!   not events — a pair whose distinct targets are all elided literals
//!   can never double-fire (both emissions would be immediate returns).

use super::{BlockId, DropPosition, Flow, Terminator};
use crate::tir::TirRef;

/// An exact executable point: ordinals run `0..=n_steps` in a block —
/// 0 is the block entry, ordinal i fires between step i-1 and step i,
/// and `n_steps` is the terminator position.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct FirePoint {
    pub block: BlockId,
    pub ordinal: u32,
}

/// The resolution of a [`DropPosition`]: an exactness-verdict taxonomy
/// for consumers. `RESOLVED` points are exact; `NON_EXACT` is the
/// compound loop-condition fallback (pairs containing one are NOT
/// PROVED — counted, never silently validated); `UNRESOLVED` is
/// internal schedule corruption (a validation error, never guessed).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FireVerdict {
    /// The position fires exactly at this point.
    Resolved(FirePoint),
    /// One exact point per construct-completion continuation (an
    /// `IfStmt`'s non-diverging arm continuations). An EMPTY vec is a
    /// real verdict: the anchor can never fire (every arm diverges / no
    /// join) — the event is excluded from path checks.
    ResolvedPerArm(Vec<FirePoint>),
    /// The compound loop-condition fallback: the bucket fires at or
    /// after this point (never before), but the precise modeling is
    /// unproven. Consumers must treat the point as a lower bound.
    NonExact(FirePoint),
    /// The anchor is neither a Flow step nor a terminator payload, or
    /// the named block does not exist. Never guess — surface as a
    /// validation error.
    Unresolved,
}

/// The resolution of an `AfterStep` anchor, split the way the backend's
/// fire-map buckets it (see `codegen/build_drop_fire_map`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AfterStepFire {
    /// Stepped anchor: fires at the boundary of the statement rooted at
    /// the block's step `ordinal - 1` — the consuming statement's end.
    StatementBoundary(FirePoint),
    /// Stepped anchor with no statement-root step at/after it in the
    /// block: fires at the block's terminator point.
    BlockTerminator(FirePoint),
    /// Terminator-payload anchor: the construct's handler fires the
    /// bucket at construct completion; the verdict carries the
    /// completion point(s).
    Payload(FireVerdict),
    /// Neither a step nor a terminator payload — see
    /// [`FireVerdict::Unresolved`].
    Unresolved,
}

/// The deterministic firing-point resolver for one function body. Built
/// once per body (the statement-root table is computed up front, not
/// per call); cheap to pass around (`Copy`-free but small).
pub struct FirePoints<'f> {
    flow: &'f Flow,
    /// Per global step index: the step's instruction is a statement
    /// root — not an `Operand`-kind child of any other stepped
    /// instruction or terminator payload. Computed once in [`new`].
    step_roots: Vec<bool>,
}

impl<'f> FirePoints<'f> {
    /// Build the resolver for one body: stamp the root bit of every
    /// step. A step is a root when its instruction has no parent, or a
    /// parent that is neither stepped nor a terminator payload (a
    /// hand-built statement shape is still its own boundary).
    pub fn new(flow: &'f Flow) -> Self {
        let mut step_roots = Vec::with_capacity(flow.steps.len());
        for s in &flow.steps {
            let root = match flow.parent_of(s.inst) {
                None => true,
                Some(p) => flow.step_of(p).is_none() && flow.term_block_of(p).is_none(),
            };
            step_roots.push(root);
        }
        FirePoints { flow, step_roots }
    }

    /// Resolve any drop position to its firing point(s) and exactness
    /// verdict. The frontend validator's event model consumes this; the
    /// backend's bucket build consumes [`resolve_after_step`] for
    /// `AfterStep` positions (it needs the bucket split).
    pub fn resolve(&self, pos: DropPosition) -> FireVerdict {
        match pos {
            DropPosition::BlockStart { block } => {
                if self.block_exists(block) {
                    FireVerdict::Resolved(FirePoint { block, ordinal: 0 })
                } else {
                    FireVerdict::Unresolved
                }
            }
            DropPosition::PreTerminator { block } => {
                if self.block_exists(block) {
                    FireVerdict::Resolved(self.terminator_point(block))
                } else {
                    FireVerdict::Unresolved
                }
            }
            DropPosition::BeforeStep { inst } => match self.step_index_of(inst) {
                Some((block, ordinal)) => FireVerdict::Resolved(FirePoint { block, ordinal }),
                None => FireVerdict::Unresolved,
            },
            DropPosition::AfterStep { inst } => match self.resolve_after_step(inst) {
                AfterStepFire::StatementBoundary(p) | AfterStepFire::BlockTerminator(p) => {
                    FireVerdict::Resolved(p)
                }
                AfterStepFire::Payload(v) => v,
                AfterStepFire::Unresolved => FireVerdict::Unresolved,
            },
        }
    }

    /// Resolve an `AfterStep` anchor, split for the backend's buckets:
    /// the consuming statement's boundary (fires from the
    /// post-statement hook) vs the block's terminator bucket vs the
    /// construct-handler payload bucket.
    pub fn resolve_after_step(&self, inst: TirRef) -> AfterStepFire {
        if let Some((block, pos)) = self.step_index_of(inst) {
            let blk = &self.flow.blocks[block.0 as usize];
            let start = blk.steps_start as usize;
            let len = blk.steps_len as usize;
            let roots = &self.step_roots[start..start + len];
            let found = roots[pos as usize..]
                .iter()
                .position(|root| *root)
                .map(|off| pos as usize + off);
            match found {
                Some(root_pos) => AfterStepFire::StatementBoundary(FirePoint {
                    block,
                    ordinal: root_pos as u32 + 1,
                }),
                None => AfterStepFire::BlockTerminator(self.terminator_point(block)),
            }
        } else if let Some(block) = self.flow.term_block_of(inst) {
            AfterStepFire::Payload(self.payload_points(block, inst))
        } else {
            AfterStepFire::Unresolved
        }
    }

    /// The point just before `inst`'s step, when `inst` is a stepped
    /// instruction.
    fn step_index_of(&self, inst: TirRef) -> Option<(BlockId, u32)> {
        let block = self.flow.step_block_of(inst)?;
        let global = self.flow.step_index_of(inst)?;
        let start = self.flow.blocks[block.0 as usize].steps_start;
        Some((block, global - start))
    }

    fn block_exists(&self, block: BlockId) -> bool {
        (block.0 as usize) < self.flow.blocks.len()
    }

    /// The block's terminator point: ordinal `n_steps`.
    fn terminator_point(&self, block: BlockId) -> FirePoint {
        FirePoint {
            block,
            ordinal: self.flow.block_steps(block).len() as u32,
        }
    }

    /// Construct-completion points of a terminator-payload anchor.
    /// `block` is the block whose terminator carries `inst` (the
    /// caller resolved it via `term_block_of`).
    fn payload_points(&self, block: BlockId, inst: TirRef) -> FireVerdict {
        // IfStmt: every non-diverging arm's continuation block is a
        // completion point (Flow already validated the join shapes:
        // falling-through arms close with `Goto`, diverging arms carry
        // their own terminator and contribute nothing).
        if self.flow.branch_if(block) == Some(inst) {
            let arms = self.flow.branch_arm_blocks(inst);
            let mut arm_ends = vec![arms.then_block, arms.else_block];
            for &holder in &arms.elif_blocks {
                if let Terminator::Branch { then_, .. } = self.flow.term(holder) {
                    arm_ends.push(*then_);
                }
            }
            let mut candidates: Vec<BlockId> = Vec::new();
            for end in arm_ends {
                if let Terminator::Goto { .. } = self.flow.term(end) {
                    candidates.extend_from_slice(self.flow.successors(end));
                }
            }
            candidates.sort_unstable_by_key(|b| b.0);
            candidates.dedup();
            return FireVerdict::ResolvedPerArm(
                candidates
                    .into_iter()
                    .map(|block| FirePoint { block, ordinal: 0 })
                    .collect(),
            );
        }
        match self.flow.term(block) {
            // A match completes at its join block.
            Terminator::Switch {
                match_inst,
                join: Some(join),
                ..
            } if *match_inst == inst => FireVerdict::Resolved(FirePoint {
                block: *join,
                ordinal: 0,
            }),
            // Every arm diverges: the continuation is unreachable and
            // the anchor can never fire.
            Terminator::Switch { match_inst, .. } if *match_inst == inst => {
                FireVerdict::ResolvedPerArm(Vec::new())
            }
            // A loop completes at its exit block. A compound condition
            // (and/or/match) cuts the header so its terminator is not
            // the loop test: the bucket still fires at the exit, but
            // the modeling is unproven — the documented non-exact
            // fallback (the precise exit point is a lower bound).
            Terminator::While { header, .. } | Terminator::For { header, .. } => {
                match self.flow.term(*header) {
                    Terminator::WhileTest { exit, .. } | Terminator::ForTest { exit, .. } => {
                        FireVerdict::Resolved(FirePoint {
                            block: *exit,
                            ordinal: 0,
                        })
                    }
                    _ => FireVerdict::NonExact(self.terminator_point(block)),
                }
            }
            // Return/Break/Continue (and any other payload): the
            // payload block's terminator point, before the exit/jump
            // completes.
            _ => FireVerdict::Resolved(self.terminator_point(block)),
        }
    }
}
