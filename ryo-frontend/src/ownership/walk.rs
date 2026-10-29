//! Flow IR-driven forward walk — the analysis driver.
//!
//! Iterates [`Flow`] blocks in construction order; for each block, the
//! steps run in evaluation order (operands before consumers — the block
//! iteration replaces the old TIR operand recursion), then the
//! terminator dispatches: `Branch` with a recorded payload is an
//! if-cascade, `Switch` is a match, `While`/`For` are loops, `Return`s
//! record the exit-path epilogue. `BoolAnd`/`BoolOr` splits, elif
//! branches, loop tests and backedges carry no payload and need nothing.
//!
//! Region walks (arms, loop bodies) recurse through the same
//! step/terminator machinery and mark their blocks visited, so the
//! outer driver never re-processes them; join/exit blocks stay unmarked
//! and are processed later with the merged state, exactly where the old
//! recursive statement walk resumed after an arm/loop body.

use super::{
    BorrowedAction, HazardKind, Owner, OwnerState, Ownership, PromoCandidate, analyze_destructure,
    analyze_enum_lit, analyze_loop, analyze_match_switch, check_field_target_projected,
    check_source_projected, check_use_moved, consume_for_assignment, consume_struct_lit_fields,
    consume_underlying, consumed_binding_name, drain_dying_views, field_path_of, needs_tracking,
    owner_name_for_diag, owner_sort_key, projection_root, push_unique, record_return_epilogue,
    register_projection, rule7_owner_name, struct_base_name, struct_root,
};
use crate::builtins::{is_borrowed_scalar_param, view_borrow_params};
use ryo_core::diag::{Diag, DiagCode, DiagSink};
use ryo_core::flow::{BlockId, Flow, PlaceId, Step, Terminator};
use ryo_core::ownership::FunctionSidecar;
use ryo_core::tir::{ChildKind, ParamMode, Span, Tir, TirData, TirRef, TirTag};
use ryo_core::types::{InternPool, StringId, TypeKind};
use std::collections::{HashMap, HashSet};

mod cascades;

use cascades::{analyze_if_cascade, check_operands_moved};
pub(crate) use cascades::{inout_escape_owner, rebind_to_init};

/// How a region walk treats its boundary block.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RegionStop {
    /// Exclusive: the block is never visited (arms stop before the join;
    /// `At(BlockId::NONE)` means "no stop").
    At(BlockId),
    /// Inclusive: the block's steps run, but its terminator is not
    /// dispatched and its successors are not followed (an elif cond
    /// chain ends at its Branch-holding block).
    Through(BlockId),
}

/// Per-block region-boundary decision for [`collect_region`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum StopKind {
    /// Visit and follow successors.
    Visit,
    /// Visit (steps only), then stop.
    VisitStop,
    /// Do not visit at all.
    Skip,
}

impl RegionStop {
    pub(crate) fn kind(self, b: BlockId) -> StopKind {
        if self.excludes(b) {
            StopKind::Skip
        } else if self.freezes(b) {
            StopKind::VisitStop
        } else {
            StopKind::Visit
        }
    }
    fn excludes(self, b: BlockId) -> bool {
        matches!(self, RegionStop::At(s) if s == b)
    }
    fn freezes(self, b: BlockId) -> bool {
        matches!(self, RegionStop::Through(s) if s == b)
    }
}

/// The blocks a region walk will visit, in document (DFS preorder)
/// order — the same order [`Walk::walk_region`] processes them. Purely
/// structural: no analysis state is touched. `stop` decides the
/// boundary per block (see [`StopKind`]); `Break`/`Continue`/`Backedge`
/// and diverging terminators contribute no successors, so loop
/// headers/exits and dead continuations never leak into a region.
pub(crate) fn collect_region(
    flow: &Flow,
    scratch: &mut Vec<bool>,
    entry: BlockId,
    stop: impl Fn(BlockId) -> StopKind,
) -> (Vec<BlockId>, bool) {
    scratch.clear();
    scratch.resize(flow.blocks.len(), false);
    let mut blocks = Vec::new();
    let mut reached_stop = false;
    let mut stack = vec![entry];
    while let Some(b) = stack.pop() {
        match stop(b) {
            StopKind::Skip => {
                reached_stop = true;
                continue;
            }
            StopKind::VisitStop => {
                if scratch[b.0 as usize] {
                    continue;
                }
                scratch[b.0 as usize] = true;
                blocks.push(b);
                continue;
            }
            StopKind::Visit => {}
        }
        if scratch[b.0 as usize] {
            continue;
        }
        scratch[b.0 as usize] = true;
        blocks.push(b);
        for &s in region_successors(flow, b).iter().rev() {
            stack.push(s);
        }
    }
    (blocks, reached_stop)
}

/// Document-ordered successors a region DFS follows for structural
/// coverage (see [`collect_region`]): unlike the walk's `visit_block`
/// continuations — where a construct handler owns its sub-region and
/// returns only the join — region collection must include EVERYTHING
/// the enclosing region will visit, so if-cascade arms and match arms
/// expand here and the DFS is bounded only by the caller's stop block.
fn region_successors(flow: &Flow, b: BlockId) -> Vec<BlockId> {
    match *flow.term(b) {
        Terminator::Goto { target } => vec![target],
        Terminator::Branch { then_, else_, .. } => vec![then_, else_],
        Terminator::Switch { arms, .. } => {
            arms.as_range().map(|i| BlockId(flow.extra[i])).collect()
        }
        Terminator::While { header, .. } | Terminator::For { header, .. } => vec![header],
        Terminator::WhileTest { body, exit, .. } | Terminator::ForTest { body, exit, .. } => {
            vec![body, exit]
        }
        Terminator::Backedge { .. }
        | Terminator::Break { .. }
        | Terminator::Continue { .. }
        | Terminator::Call { .. }
        | Terminator::Return { .. }
        | Terminator::ReturnVoid
        | Terminator::Unreachable => Vec::new(),
    }
}

/// Per-function walk state: the Flow IR driver plus the tables the
/// post-walk passes consume (last-use reads recorded during the walk,
/// loop body regions, document-order ranks).
pub(crate) struct Walk<'a> {
    pub(crate) tir: &'a Tir,
    pub(crate) flow: &'a Flow,
    pub(crate) pool: &'a InternPool,
    pub(crate) dead_stores: &'a super::DeadStoreAnalysis,
    /// Outer-driver skip marks: set once per block (first visit); the
    /// id-ordered driver uses them to process dead continuations the
    /// region walks never reach, and never to re-process a block.
    processed: Vec<bool>,
    /// Re-entrancy guard for region walks (arm walks, fixed-point
    /// passes): blocks stamped with the walk's current epoch are
    /// skipped, so nested or repeated walks stay exactly-once. Loop
    /// handlers re-stamp their finished body regions with the
    /// enclosing generation's epoch (see `analyze_loop`).
    pub(crate) epoch: Vec<u32>,
    epoch_next: u32,
    /// First-VarDecl emission rank per binding name (the "declared
    /// before" test of the loop-carried scan) — keyed on the build's
    /// emission order, which is document order even for dead code.
    pub(crate) decl_rank: HashMap<StringId, u32>,
    /// The last instruction processed within the current region (steps
    /// and terminator payloads) — the Flow IR replacement for "the
    /// arm's last statement".
    pub(crate) last_inst: Option<TirRef>,
    /// Every block in first-visit (document) order.
    pub(crate) block_order: Vec<BlockId>,
    /// Per-loop body regions, for the post-walk loop-carried scan.
    pub(crate) loop_regions: Vec<(TirRef, Vec<BlockId>)>,
    /// Mutable needs-drop homes assigned in the active loop bodies.
    /// Their owner identity is the runtime home slot across iterations.
    pub(crate) active_loop_carried_homes: Vec<HashSet<PlaceId>>,
    /// Owner-keyed last-use reads, recorded at `Var` steps (replaces the
    /// old post-walk `collect_last_uses` scan).
    pub(crate) last_use: HashMap<TirRef, TirRef>,
    /// Binding name → latest read (same recording rules as `last_use`).
    pub(crate) last_read_by_name: HashMap<StringId, TirRef>,
    /// Scratch for `collect_region`.
    pub(crate) region_scratch: Vec<bool>,
    /// The enclosing-statement inst per stepped instruction
    /// (promo-candidate anchors; replaces `Ownership::current_stmt`).
    stmt_of: Vec<Option<TirRef>>,
    /// Step insts that stand alone as a statement (not an operand of any
    /// stepped inst or terminator payload) — statement boundaries where
    /// dying projections drain.
    is_root: Vec<bool>,
}

impl<'a> Walk<'a> {
    pub(crate) fn new(
        tir: &'a Tir,
        flow: &'a Flow,
        pool: &'a InternPool,
        dead_stores: &'a super::DeadStoreAnalysis,
    ) -> Self {
        let len = tir.instructions.len();
        // Statement roots (drain gates) and `stmt_of` (promo anchors)
        // matter only when the body can create views or promotion
        // candidates: any view-typed step, or a borrowed `str`/`bytes`
        // param (promotion only leaks through borrowed params — a move
        // param's buffer is callee-owned). Bodies without either skip
        // the operand-marking precompute entirely (it walks every
        // operand edge — the old recursion's cost — for nothing).
        let has_views = tir.params.iter().any(|p| {
            p.mode == ParamMode::Borrow
                && matches!(pool.kind(p.ty), TypeKind::Str | TypeKind::Bytes)
        }) || flow
            .steps
            .iter()
            .any(|step| pool.is_view(tir.inst(step.inst).ty));
        let (is_root, stmt_of) = if has_views {
            // Statement roots: stepped insts that no stepped inst or
            // terminator payload consumes as an operand. Operand
            // marking covers conditions/scrutinees/bounds/return
            // operands via the recorded terminator payloads
            // (`term_payload`).
            let mut is_operand = vec![false; len];
            for b in flow.forward_order() {
                for step in flow.block_steps(b) {
                    tir.walk_operands(step.inst, &mut |_p, child, kind| {
                        if kind == ChildKind::Operand {
                            is_operand[child.index()] = true;
                        }
                    });
                }
                if let Some(p) = flow.term_payload(b) {
                    tir.walk_operands(p, &mut |_p, child, kind| {
                        if kind == ChildKind::Operand {
                            is_operand[child.index()] = true;
                        }
                    });
                }
            }
            let mut is_root = vec![false; len];
            for b in flow.forward_order() {
                for step in flow.block_steps(b) {
                    is_root[step.inst.index()] = !is_operand[step.inst.index()];
                }
            }
            let mut stmt_of = vec![None; len];
            compute_stmt_of(flow, &is_root, &mut stmt_of);
            (is_root, stmt_of)
        } else {
            (vec![false; len], vec![None; len])
        };
        Walk {
            tir,
            flow,
            pool,
            dead_stores,
            processed: vec![false; flow.blocks.len()],
            epoch: vec![0; flow.blocks.len()],
            epoch_next: 0,
            decl_rank: HashMap::new(),
            last_inst: None,
            block_order: Vec::new(),
            loop_regions: Vec::new(),
            active_loop_carried_homes: Vec::new(),
            last_use: HashMap::new(),
            last_read_by_name: HashMap::new(),
            region_scratch: Vec::new(),
            stmt_of,
            is_root,
        }
    }
}

/// The enclosing-statement inst for every stepped instruction: the
/// nearest root step at or after it in its block, or — for trailing
/// operand steps (conditions, scrutinees, bounds, return operands,
/// diverging-call args) — the first root/payload reachable through the
/// continuation chain.
fn compute_stmt_of(flow: &Flow, is_root: &[bool], stmt_of: &mut [Option<TirRef>]) {
    for b in flow.forward_order() {
        let steps = flow.block_steps(b);
        let mut next_root: Option<TirRef> = None;
        let mut resolved: Vec<Option<TirRef>> = vec![None; steps.len()];
        for i in (0..steps.len()).rev() {
            resolved[i] = next_root;
            if is_root[steps[i].inst.index()] {
                next_root = Some(steps[i].inst);
            }
        }
        let tail = trailing_stmt(flow, is_root, b);
        for (i, step) in steps.iter().enumerate() {
            stmt_of[step.inst.index()] = resolved[i].or(tail);
        }
    }
}

/// The enclosing statement for trailing steps of `b`: the block's
/// terminator payload when it carries one (if/match/loop/return…), the
/// loop instruction for loop tests/backedges, else the first
/// root/payload found by following the continuation chain (a
/// `BoolAnd`/`BoolOr` lhs resolves to the statement boundary in the
/// join block).
fn trailing_stmt(flow: &Flow, is_root: &[bool], b: BlockId) -> Option<TirRef> {
    match *flow.term(b) {
        Terminator::WhileTest { loop_inst, .. } | Terminator::ForTest { loop_inst, .. } => {
            Some(loop_inst)
        }
        Terminator::Backedge { to, .. } => match *flow.term(to) {
            Terminator::WhileTest { loop_inst, .. } | Terminator::ForTest { loop_inst, .. } => {
                Some(loop_inst)
            }
            _ => None,
        },
        _ => {
            if let Some(p) = flow.term_payload(b) {
                return Some(p);
            }
            // Follow continuations (bounded by block count) to the
            // nearest block that names the enclosing statement.
            let mut visited = vec![false; flow.blocks.len()];
            let mut stack: Vec<BlockId> = region_successors(flow, b);
            while let Some(c) = stack.pop() {
                if visited[c.0 as usize] {
                    continue;
                }
                visited[c.0 as usize] = true;
                if let Some(p) = flow.term_payload(c) {
                    return Some(p);
                }
                for step in flow.block_steps(c) {
                    if is_root[step.inst.index()] {
                        return Some(step.inst);
                    }
                }
                stack.extend(region_successors(flow, c));
            }
            None
        }
    }
}

impl<'a> Walk<'a> {
    /// Bump the fixed-point pass counter.
    pub(crate) fn next_epoch(&mut self) -> u32 {
        self.epoch_next += 1;
        self.epoch_next
    }

    /// The build's emission order for `r` (document order — parents
    /// before children, dead code at its source position). Order-aware
    /// recordings compare with this instead of visit order: dead
    /// continuation blocks are walked late but emitted in place.
    pub(crate) fn order(&self, r: TirRef) -> u32 {
        self.flow.order_of(r).unwrap_or(0)
    }

    /// The `StepFacts` of the step that evaluates `inst`.
    pub(crate) fn step_facts(&self, inst: TirRef) -> ryo_core::flow::StepFacts {
        self.flow.step_of(inst).expect("stepped inst").facts
    }

    pub(crate) fn mark_processed(&mut self, b: BlockId) {
        if !self.processed[b.0 as usize] {
            self.processed[b.0 as usize] = true;
            self.block_order.push(b);
        }
    }

    pub(crate) fn is_processed(&self, b: BlockId) -> bool {
        self.processed[b.0 as usize]
    }

    /// Enter a block for a once-ever walk (outer driver, arm/cond
    /// lists): false when the current generation (`epoch`) already
    /// visited it. Loop handlers re-stamp their finished body regions
    /// with the enclosing generation's epoch, so enclosing lists skip
    /// them without consulting `processed` — which fixed-point phase
    /// passes must not gate on (they re-walk the body per pass via
    /// [`Walk::enter_epoch`], whose epoch differs from the enclosing
    /// generation's).
    fn enter_seen(&mut self, b: BlockId, epoch: u32) -> bool {
        if self.epoch[b.0 as usize] == epoch {
            return false;
        }
        self.epoch[b.0 as usize] = epoch;
        self.mark_processed(b);
        true
    }

    /// Enter a block for a fixed-point pass: false when THIS pass
    /// already visited it (earlier passes' stamps do not count — the
    /// pass re-walks the region, and `processed` must not gate it).
    fn enter_epoch(&mut self, b: BlockId, epoch: u32) -> bool {
        if self.epoch[b.0 as usize] == epoch {
            return false;
        }
        self.epoch[b.0 as usize] = epoch;
        true
    }

    /// Open-ended region walk (the outer driver, if/match cond
    /// chains): DFS preorder from `entry`, stopping per `stop`.
    /// Returns true when the `At` boundary was reached.
    pub(crate) fn walk_region(
        &mut self,
        own: &mut Ownership,
        sink: &mut DiagSink,
        sidecar: &mut FunctionSidecar,
        entry: BlockId,
        stop: RegionStop,
        epoch: u32,
    ) -> bool {
        let mut reached_stop = false;
        let mut stack = vec![entry];
        while let Some(b) = stack.pop() {
            if stop.excludes(b) {
                reached_stop = true;
                continue;
            }
            if !self.enter_seen(b, epoch) {
                continue;
            }
            let conts = self.visit_block(own, sink, sidecar, b, epoch);
            if stop.freezes(b) {
                continue;
            }
            for c in conts.into_iter().flatten().rev() {
                stack.push(c);
            }
        }
        reached_stop
    }

    /// Bounded region walk (if/match arms, loop-body fixed points):
    /// visits exactly the precomputed `list`, ignoring continuations
    /// (every block a construct dispatch would reach is a list member).
    pub(crate) fn walk_block_list(
        &mut self,
        own: &mut Ownership,
        sink: &mut DiagSink,
        sidecar: &mut FunctionSidecar,
        list: &[BlockId],
        epoch: u32,
    ) {
        for &b in list {
            if !self.enter_seen(b, epoch) {
                continue;
            }
            let _ = self.visit_block(own, sink, sidecar, b, epoch);
        }
    }

    /// Bounded region walk for fixed-point passes: visits exactly
    /// `list`, allowing re-visits across passes (per-pass epoch) but
    /// never within one.
    pub(crate) fn walk_phase(
        &mut self,
        own: &mut Ownership,
        sink: &mut DiagSink,
        sidecar: &mut FunctionSidecar,
        list: &[BlockId],
        epoch: u32,
    ) {
        for &b in list {
            if !self.enter_epoch(b, epoch) {
                continue;
            }
            let _ = self.visit_block(own, sink, sidecar, b, epoch);
        }
    }

    /// Process one block's steps, then dispatch its terminator.
    /// Returns the continuation blocks (a `BoolAnd`/`BoolOr` rhs+join, a
    /// cascade/match/loop join) in visit order.
    fn visit_block(
        &mut self,
        own: &mut Ownership,
        sink: &mut DiagSink,
        sidecar: &mut FunctionSidecar,
        b: BlockId,
        epoch: u32,
    ) -> [Option<BlockId>; 2] {
        let dead = self.flow.blocks[b.0 as usize].unreachable;
        for step in self.flow.block_steps(b) {
            self.process_step(own, sink, sidecar, *step, dead);
        }
        match *self.flow.term(b) {
            Terminator::Goto { target } => [Some(target), None],
            Terminator::Branch { then_, else_, .. } => match self.flow.branch_if(b) {
                Some(if_inst) => [
                    analyze_if_cascade(self, own, sink, sidecar, b, if_inst, epoch),
                    None,
                ],
                None => [Some(then_), Some(else_)],
            },
            Terminator::Switch { match_inst, .. } => [
                analyze_match_switch(self, own, sink, sidecar, b, match_inst, epoch),
                None,
            ],
            Terminator::While { .. } | Terminator::For { .. } => {
                [analyze_loop(self, own, sink, sidecar, b, epoch), None]
            }
            Terminator::Return { .. } => {
                // The return's own steps (the operand expression) ran
                // above; the payload instruction is the consumer: E1 /
                // the move-or-borrowed transition, then the exit-path
                // epilogue snapshot.
                if let Some(payload) = self.flow.term_payload(b) {
                    self.analyze_return(own, sink, payload);
                    record_return_epilogue(self.tir, self.flow, own, payload);
                }
                [None, None]
            }
            Terminator::ReturnVoid => {
                // The implicit end-of-body `ReturnVoid` carries no
                // payload (no TIR instruction exists for it) — the
                // promo fallthrough backstop and synthesized-return
                // epilogue own that path, exactly as when the old walk
                // saw no explicit return statement.
                if let Some(payload) = self.flow.term_payload(b) {
                    record_return_epilogue(self.tir, self.flow, own, payload);
                }
                [None, None]
            }
            Terminator::Backedge { .. }
            | Terminator::Break { .. }
            | Terminator::Continue { .. }
            | Terminator::Unreachable
            | Terminator::WhileTest { .. }
            | Terminator::ForTest { .. } => [None, None],
            Terminator::Call { .. } => {
                // A diverging call (`panic`, …): the call instruction
                // is the terminator payload, never a step, but it still
                // needs the Call analysis — the borrowed-scalar arg
                // un-seeding and the Rule-7 partition run here (the arg
                // operand steps already ran above). There is no
                // continuation: `next` is an unreachable block the
                // outer driver processes as dead code.
                let payload = self
                    .flow
                    .term_payload(b)
                    .expect("diverging calls carry their instruction");
                self.analyze_call(own, sink, payload);
                [None, None]
            }
        }
    }

    /// Handle one stepped instruction. Operand steps have already run
    /// (block iteration order), so handlers keep only the
    /// parent-owned work: seeding, reads, use-safety checks, consumes.
    /// Statement-root steps drain dying projections at the end (the
    /// P4 per-statement lift).
    fn process_step(
        &mut self,
        own: &mut Ownership,
        sink: &mut DiagSink,
        sidecar: &mut FunctionSidecar,
        step: Step,
        dead: bool,
    ) {
        let r = step.inst;
        let inst = *self.tir.inst(r);
        match inst.tag {
            TirTag::VarDecl => self.analyze_var_decl(own, sink, sidecar, r, dead),
            TirTag::Assign => self.analyze_assign(own, sink, sidecar, r),
            TirTag::CompoundAssign => {
                // Sema rejects compound-assign on Move-typed values
                // today; enforce the invariant so a future sema
                // relaxation trips a debug build. The RHS reads were
                // recorded by the RHS operand steps.
                let view = self.tir.compound_assign_view(r);
                debug_assert!(
                    !needs_tracking(self.tir.inst(view.value).ty, self.pool),
                    "compound-assign on Move-typed value reached ownership pass; \
                     sema should have rejected",
                );
            }
            TirTag::FieldAssign => self.analyze_field_assign(own, sink, sidecar, r, false),
            TirTag::CompoundFieldAssign => self.analyze_field_assign(own, sink, sidecar, r, true),
            TirTag::Destructure => analyze_destructure(self, own, sink, sidecar, r),
            TirTag::ExprStmt => {}
            TirTag::StrConst | TirTag::BytesConst => self.seed_temp(own, r),
            TirTag::StrConcat | TirTag::BytesConcat => {
                self.seed_temp(own, r);
                if let TirData::BinOp { lhs, rhs } = inst.data {
                    for op in [lhs, rhs] {
                        if needs_tracking(self.tir.inst(op).ty, self.pool) {
                            check_use_moved(self.tir, self.pool, own, sink, op, self.tir.span(op));
                        }
                    }
                }
            }
            TirTag::DebugRepr => {
                self.seed_temp(own, r);
                if let TirData::UnOp(operand) = inst.data
                    && needs_tracking(self.tir.inst(operand).ty, self.pool)
                {
                    check_use_moved(
                        self.tir,
                        self.pool,
                        own,
                        sink,
                        operand,
                        self.tir.span(operand),
                    );
                }
            }
            TirTag::StructLit => {
                self.seed_temp(own, r);
                consume_struct_lit_fields(self.tir, self.flow, self.pool, own, sink, r);
            }
            TirTag::EnumLit => analyze_enum_lit(self, own, sink, r),
            TirTag::Call => self.analyze_call(own, sink, r),
            TirTag::Var => self.analyze_var_read(own, r, dead),
            TirTag::Slice => {
                check_operands_moved(self.tir, self.pool, own, sink, r);
                if let TirData::Slice { base, .. } = inst.data {
                    self.record_promo_candidate(own, r, base);
                }
            }
            TirTag::ToView => {
                check_operands_moved(self.tir, self.pool, own, sink, r);
                if let TirData::UnOp(operand) = inst.data {
                    self.record_promo_candidate(own, r, operand);
                }
            }
            // Control constructs are terminator payloads, never steps —
            // except `Unreachable`, which sema also emits inside an
            // `ExprStmt` wrapper (error recovery), and the ExprStmt
            // arm of the builder steps its operand like any expression.
            TirTag::Unreachable => {}
            TirTag::IfStmt
            | TirTag::WhileLoop
            | TirTag::ForRange
            | TirTag::Match
            | TirTag::Return
            | TirTag::ReturnVoid
            | TirTag::Break
            | TirTag::Continue => {
                unreachable!(
                    "structural instruction {:?} reached the step walk",
                    inst.tag
                )
            }
            _ => check_operands_moved(self.tir, self.pool, own, sink, r),
        }
        self.last_inst = Some(r);
        if self.is_root[r.index()] {
            drain_dying_views(own);
        }
    }

    /// Fresh owned producer (concat result, call result, literal, …):
    /// enters the lattice as `Valid`; the temp's Flow place is its
    /// owner identity.
    fn seed_temp(&mut self, own: &mut Ownership, r: TirRef) {
        if needs_tracking(self.tir.inst(r).ty, self.pool) {
            let owner = own.owner_of_inst(r);
            own.states.insert(owner, OwnerState::Valid);
            own.temp_owners.insert(owner);
        }
    }

    // ---------- statement-step handlers (operand visits removed) ----------

    /// A Move-typed `VarDecl` consumes its initializer; view and Copy
    /// declarations only establish their binding state.
    fn analyze_var_decl(
        &mut self,
        own: &mut Ownership,
        sink: &mut DiagSink,
        _sidecar: &mut FunctionSidecar,
        r: TirRef,
        dead: bool,
    ) {
        let view = self.tir.var_decl_view(r);
        let init = view.initializer;
        let init_ty = self.tir.inst(init).ty;
        // The declaration's write fact names the binding's home place.
        let home = self.step_facts(r).write.expect("VarDecl writes").place;
        own.binding_of_name.insert(view.name, r);
        if needs_tracking(init_ty, self.pool) {
            let span = self.tir.span(r);
            let consumed_name = consumed_binding_name(self.tir, init);
            // P2 freeze (final spec §3.2): the consume moves the owner.
            check_source_projected(
                self.tir,
                self.flow,
                self.pool,
                own,
                sink,
                own.underlying_owner(init),
                span,
                "move",
                consumed_name,
            );
            consume_for_assignment(self.tir, self.pool, own, sink, init, span, consumed_name, r);
            rebind_to_init(own, home, init);
        } else if self.pool.is_view(init_ty) {
            // P3 (final spec §3.2): binding a slice registers a
            // projection against the root owner (re-slices resolve
            // transitively). Var copies alias the original slice rather
            // than projecting again — the read fact resolves the alias.
            let view_owner = own.underlying_owner(init);
            own.current_owner.insert(home, view_owner);
            register_projection(own, self.tir, self.flow, self.pool, init, view_owner);
        } else {
            own.current_owner.insert(home, Owner(home));
        }
        // First-declared rank for the loop-carried scan. Live blocks
        // walk in document order, so the first visit wins; a dead-code
        // declaration walks late but was emitted in place, so its rank
        // is order-aware.
        if dead {
            let order = self.order(r);
            match self.decl_rank.get(&view.name) {
                Some(&cur) if cur <= order => {}
                _ => {
                    self.decl_rank.insert(view.name, order);
                }
            }
        } else {
            let order = self.order(r);
            self.decl_rank.entry(view.name).or_insert(order);
        }
    }

    /// Reassignment consumes Move-typed values and reseats their owner;
    /// view and Copy assignments update only binding state.
    fn analyze_assign(
        &mut self,
        own: &mut Ownership,
        sink: &mut DiagSink,
        sidecar: &mut FunctionSidecar,
        r: TirRef,
    ) {
        let view = self.tir.assign_view(r);
        // Stamp the Assign with the lexical binding its name resolved to
        // BEFORE any early return: the free-scheduling passes group
        // reassignments by this identity so a same-named shadow's
        // reassigns cannot suppress the outer binding's cleanup.
        // The write fact names the home place; the binding identity
        // recorded for the free-scheduling passes is the place's
        // declaring inst (a `Local`'s VarDecl, a param's virtual ref).
        let home = self.step_facts(r).write.expect("Assign writes").place;
        sidecar.assign_binding[r.index()] = match self.flow.place(home) {
            ryo_core::flow::Place::Local { decl, .. } => Some(*decl),
            ryo_core::flow::Place::Param { index, .. } => Some(TirRef::param(*index as usize)),
            _ => None,
        };
        let value_ty = self.tir.inst(view.value).ty;
        if needs_tracking(value_ty, self.pool) {
            // Capture the old owner before consume_for_assignment /
            // rebind overwrite current_owner[name]. Only emit the Free
            // entry if the old owner is Valid — Borrowed/Moved/missing
            // means there is no live allocation to release. EXCEPTION:
            // an `inout str` param's current owner is Borrowed, yet the
            // callee must drop the old pointee when reassigning the
            // param (the write-back ABI hands the caller whatever triple
            // the param holds at exit).
            if let Some(&old_owner) = own.current_owner.get(&home) {
                let old_droppable = match own.states.get(&old_owner) {
                    Some(OwnerState::Valid) => true,
                    Some(OwnerState::Borrowed) => {
                        // An inout param's home is its own place.
                        matches!(self.flow.place(home), ryo_core::flow::Place::Param { .. })
                            && own.inout_str_params.contains(&self.flow.place(home).name())
                    }
                    _ => false,
                };
                if old_droppable {
                    // Self-assignment (`s = s`, `p = p`): the RHS reads
                    // the same owner the target currently holds, so the
                    // reseat is a liveness no-op. Bail before scheduling
                    // anything.
                    if own.underlying_owner(view.value) == old_owner {
                        return;
                    }
                    // P2 freeze (final spec §3.2): reassignment mutates
                    // the owner — illegal while a slice of it is live.
                    check_source_projected(
                        self.tir,
                        self.flow,
                        self.pool,
                        own,
                        sink,
                        old_owner,
                        self.tir.span(r),
                        "mutate",
                        Some(view.name),
                    );
                    // `tirref` (not `inst_tirref`): an inout param's old
                    // owner is a `Param`, resolved here to its virtual
                    // ref — codegen caches that ref's repr at the
                    // prologue.
                    sidecar.free_on_reassign[r.index()] = Some(old_owner.tirref(self.flow));
                    // Consuming concat: `s = s + suffix` where the lhs
                    // Var resolves to the dying owner and the rhs is a
                    // different owner.
                    let old_valid = matches!(own.states.get(&old_owner), Some(OwnerState::Valid));
                    let value_inst = self.tir.inst(view.value);
                    if old_valid
                        && matches!(value_inst.tag, TirTag::StrConcat | TirTag::BytesConcat)
                        && let TirData::BinOp { lhs, rhs } = value_inst.data
                        && matches!(self.tir.inst(lhs).data, TirData::Var(_))
                        && own.underlying_owner(lhs) == old_owner
                        && own.underlying_owner(rhs) != old_owner
                    {
                        sidecar.consumed_concat_lhs[r.index()] = Some(view.value);
                    }
                    // W0003 case-B support: reassignment mutates the
                    // binding's owner — a defensive-copy hazard on it.
                    own.owner_hazards.push((old_owner, r, HazardKind::Mutate));
                }
            }
            let span = self.tir.span(r);
            let consumed_name = consumed_binding_name(self.tir, view.value);
            // P2 freeze (final spec §3.2): the consume moves the owner.
            check_source_projected(
                self.tir,
                self.flow,
                self.pool,
                own,
                sink,
                own.underlying_owner(view.value),
                span,
                "move",
                consumed_name,
            );
            consume_for_assignment(
                self.tir,
                self.pool,
                own,
                sink,
                view.value,
                span,
                consumed_name,
                r,
            );
            rebind_to_init(own, home, view.value);
            let loop_carried = self
                .active_loop_carried_homes
                .iter()
                .any(|homes| homes.contains(&home));
            if loop_carried {
                let home_owner = Owner(home);
                own.current_owner.insert(home, home_owner);
                own.states.insert(home_owner, OwnerState::Valid);
            }
        } else if self.pool.is_view(value_ty) {
            // P3/P4 (final spec §3.2): rebinding a view registers the
            // new projection and ends the old binding's — unless another
            // binding still aliases the old view.
            let old_owner = own.current_owner.get(&home).copied();
            let view_owner = own.underlying_owner(view.value);
            register_projection(own, self.tir, self.flow, self.pool, view.value, view_owner);
            own.current_owner.insert(home, view_owner);
            if let Some(old) = old_owner
                && old != view_owner
                && own.root_owner.contains_key(&old)
                && !own.current_owner.values().any(|o| *o == old)
            {
                own.pending_dying.push(old);
            }
        }
    }

    /// M9: `p.f = v` reads its root and moves the value into the field;
    /// when the field type needs-drop, the OLD field value must be
    /// freed before the store (codegen consults `field_free_on_reassign`
    /// and walks the target chain). Target/value operand steps already
    /// ran; this handler does the field-free scheduling, the target-side
    /// freeze check and the value consume.
    fn analyze_field_assign(
        &mut self,
        own: &mut Ownership,
        sink: &mut DiagSink,
        sidecar: &mut FunctionSidecar,
        r: TirRef,
        compound: bool,
    ) {
        let inst = *self.tir.inst(r);
        let (target, value) = if compound {
            let v = self.tir.compound_field_assign_view(r);
            (v.target, v.value)
        } else {
            let v = self.tir.field_assign_view(r);
            (v.target, v.value)
        };
        if needs_tracking(inst.ty, self.pool) {
            sidecar.field_free_on_reassign[r.index()] = Some(target);
            if !compound {
                let span = self.tir.span(r);
                // P2 freeze on the TARGET side: the reassign frees the
                // old buffer of the assigned field only, so the check
                // matches the target's field path against each live
                // projection's.
                if let Some(root) = struct_root(own, self.tir, target) {
                    let target_path = field_path_of(self.tir, target).unwrap_or_default();
                    check_field_target_projected(
                        self.tir,
                        self.flow,
                        self.pool,
                        own,
                        sink,
                        root,
                        &target_path,
                        span,
                        struct_base_name(self.tir, target),
                    );
                }
                let consumed_name = consumed_binding_name(self.tir, value);
                // P2 freeze (final spec §3.2): the consume moves the owner.
                check_source_projected(
                    self.tir,
                    self.flow,
                    self.pool,
                    own,
                    sink,
                    own.underlying_owner(value),
                    span,
                    "move",
                    consumed_name,
                );
                consume_for_assignment(
                    self.tir,
                    self.pool,
                    own,
                    sink,
                    value,
                    span,
                    consumed_name,
                    r,
                );
            }
        }
    }

    /// Move-typed `Return` is a consumer: the returned value flows out
    /// of the function and the caller takes ownership. The operand
    /// steps already ran; E1 rejects returning views, then the consume.
    fn analyze_return(&mut self, own: &mut Ownership, sink: &mut DiagSink, r: TirRef) {
        let inst = *self.tir.inst(r);
        let operand = match inst.data {
            TirData::UnOp(o) => o,
            _ => unreachable!("Return must carry TirData::UnOp"),
        };
        let ty = self.tir.inst(operand).ty;
        if self.pool.is_view(ty) {
            // E1 (final spec §3.3): slices cannot be returned from
            // functions. Backstop to sema's signature-level rejection.
            sink.emit(
                Diag::error(
                    self.tir.span(r),
                    DiagCode::ViewEscape,
                    "cannot return a slice from a function",
                )
                .with_help("slices are non-escaping; return an owned `str` instead"),
            );
            return;
        }
        if !needs_tracking(ty, self.pool) {
            return;
        }
        let span = self.tir.span(r);
        let consumed_name = consumed_binding_name(self.tir, operand);
        // P2 freeze (final spec §3.2): returning moves the value out.
        check_source_projected(
            self.tir,
            self.flow,
            self.pool,
            own,
            sink,
            own.underlying_owner(operand),
            span,
            "move",
            consumed_name,
        );
        consume_underlying(
            self.tir,
            self.pool,
            own,
            sink,
            operand,
            span,
            consumed_name,
            BorrowedAction::ReturnBorrowed,
            r,
        );
    }

    /// A `Call` step: seed a needs-drop result, run the borrow/move/
    /// inout partition (Rule 7, E0031/E0032, E4 view borrows), then
    /// commit the moves. Arg operand steps already ran (phase 1 of the
    /// old walk is inherent in the block iteration).
    fn analyze_call(&mut self, own: &mut Ownership, sink: &mut DiagSink, r: TirRef) {
        let inst = *self.tir.inst(r);
        // A str-returning call (e.g. `int_to_str`) is a producer.
        if needs_tracking(inst.ty, self.pool) {
            let owner = own.owner_of_inst(r);
            own.states.insert(owner, OwnerState::Valid);
            own.temp_owners.insert(owner);
        }
        let view = self.tir.call_view(r);

        // Phase 2 — use-safety check + borrow/move/inout partition.
        let mut borrowed: Vec<Owner> = Vec::new();
        let mut moved: Vec<Owner> = Vec::new();
        // E4 (final spec §3.3): a view argument borrows its ROOT owner
        // for the duration of the call.
        let mut view_borrowed: Vec<Owner> = Vec::new();
        // inout occurrences: (owner, arg span) — a Vec, not a Set, so a
        // double-inout of one owner is detectable by count (Rule 7).
        let mut inout_uses: Vec<(Owner, Span)> = Vec::new();
        for (i, arg) in view.args.iter().enumerate() {
            if is_borrowed_scalar_param(view.name, self.pool, i)
                && matches!(self.tir.inst(arg).tag, TirTag::StrConst)
            {
                // Undo the lattice seeding from the StrConst step: the
                // borrowed-scalar ABI never owns its argument.
                let arg_owner = own.owner_of_inst(arg);
                own.temp_owners.remove(&arg_owner);
                own.states.remove(&arg_owner);
            }
            let mode = view.modes.get(i).unwrap_or(ParamMode::Borrow);
            let arg_ty = self.tir.inst(arg).ty;
            if mode == ParamMode::Inout {
                // Rule 7 (M8.3): resolve an aliasing identity even for
                // Copy scalars, which never enter the lattice — the
                // arg's read fact names the binding home place.
                let owner = own.underlying_owner(arg);
                check_use_moved(self.tir, self.pool, own, sink, arg, self.tir.span(arg));
                if needs_tracking(arg_ty, self.pool) {
                    // P2 freeze (final spec §3.2): `inout` passing
                    // mutates the owner.
                    check_source_projected(
                        self.tir,
                        self.flow,
                        self.pool,
                        own,
                        sink,
                        owner,
                        self.tir.span(arg),
                        "mutate",
                        consumed_binding_name(self.tir, arg),
                    );
                }
                inout_uses.push((owner, self.tir.span(arg)));
                // W0003 case-B support: an `inout` pass mutates the
                // owner — a defensive-copy hazard on it.
                own.owner_hazards.push((owner, r, HazardKind::InoutPass));
                continue;
            }
            if !needs_tracking(arg_ty, self.pool) {
                if mode == ParamMode::Borrow && self.pool.is_view(arg_ty) {
                    // A view arg borrows its root owner for the call's
                    // duration (E4). `projection_root` looks through
                    // ToView conversions (the implicit str → strview
                    // coercion) to the underlying owner.
                    if let Some(root) = projection_root(own, self.tir, self.flow, self.pool, arg) {
                        push_unique(&mut view_borrowed, root);
                    }
                } else if mode == ParamMode::Borrow {
                    // A Copy borrow is a no-op for liveness, but it
                    // still aliases an `inout` of the same root in this
                    // call. Record every Var read and Copy field read in
                    // the arg's expression — the walk stops at Call
                    // boundaries: a nested call's args are that call's
                    // own Rule-7 partition, fully evaluated before this
                    // call starts.
                    let mut stack = vec![arg];
                    while let Some(cur) = stack.pop() {
                        let inst = self.tir.inst(cur);
                        match inst.tag {
                            // The read fact names the binding home
                            // place (FieldAccess steps carry the
                            // chain's root) — resolution is uniform.
                            TirTag::Var | TirTag::FieldAccess => {
                                push_unique(&mut borrowed, own.underlying_owner(cur));
                            }
                            TirTag::Call => continue,
                            _ => {}
                        }
                        self.tir
                            .walk_operands(cur, &mut |_, child, _| stack.push(child));
                    }
                }
                continue;
            }
            // M9: a needs-drop field read in a borrow-mode argument
            // borrows the ROOT struct owner for the call's duration.
            if mode == ParamMode::Borrow && matches!(self.tir.inst(arg).tag, TirTag::FieldAccess) {
                push_unique(&mut borrowed, own.underlying_owner(arg));
                continue;
            }
            // M8.4.1.2: a `str(view)` materialization call passed as a
            // borrow-mode arg READS the view's buffer at call time —
            // the view's ROOT owner is immutably borrowed for the outer
            // call's duration (E4).
            if mode == ParamMode::Borrow && self.tir.inst(arg).tag == TirTag::Call {
                let inner = self.tir.call_view(arg);
                let vb_params = view_borrow_params(inner.name, self.pool);
                if !vb_params.is_empty() {
                    for &idx in vb_params {
                        if let Some(varg) = inner.args.get(idx)
                            && let Some(root) =
                                projection_root(own, self.tir, self.flow, self.pool, varg)
                        {
                            push_unique(&mut view_borrowed, root);
                        }
                    }
                    continue;
                }
            }
            // P6': a view re-borrowed into a `str` arg (ViewAsOwner)
            // borrows the view's ROOT owner for the call's duration.
            let owner = self.call_arg_owner(own, mode, arg);
            if mode == ParamMode::Borrow {
                check_use_moved(self.tir, self.pool, own, sink, arg, self.tir.span(arg));
                push_unique(&mut borrowed, owner);
            } else {
                push_unique(&mut moved, owner);
            }
        }
        self.check_call_argument_aliasing(
            own,
            sink,
            r,
            &view,
            &borrowed,
            &moved,
            &view_borrowed,
            &inout_uses,
        );
        self.commit_call_moves(own, sink, r, &view, &view_borrowed);
        // `inout` args need no ownership transition: the callee only
        // borrows the slot, and the binding keeps its pre-call owner.
        // The stale-triple hazard (callee realloc'd/replaced the
        // buffer) is handled in CODEGEN (named-binding Frees emit the
        // binding's CURRENT `FatLocals`).
    }

    /// The owner an argument contributes to Rule-7 partition and aliasing
    /// checks. For a borrow-mode `ViewAsOwner` re-borrow, this is the
    /// projection's root owner when one exists; otherwise the argument's
    /// underlying owner (P6', E4).
    fn call_arg_owner(&self, own: &Ownership, mode: ParamMode, arg: TirRef) -> Owner {
        if mode == ParamMode::Borrow && self.tir.inst(arg).tag == TirTag::ViewAsOwner {
            projection_root(own, self.tir, self.flow, self.pool, arg)
                .unwrap_or_else(|| own.underlying_owner(arg))
        } else {
            own.underlying_owner(arg)
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn check_call_argument_aliasing(
        &self,
        own: &Ownership,
        sink: &mut DiagSink,
        r: TirRef,
        view: &ryo_core::tir::CallView<'_>,
        borrowed: &[Owner],
        moved: &[Owner],
        view_borrowed: &[Owner],
        inout_uses: &[(Owner, Span)],
    ) {
        // Rule 7 (M8.3): at most one mutable borrow per owner in a
        // call, and no immutable borrow / move alongside it.
        for (pos, (owner, _span)) in inout_uses.iter().enumerate() {
            if inout_uses[..pos].iter().any(|(o, _)| o == owner) {
                continue;
            }
            let name = rule7_owner_name(own, self.tir, self.flow, self.pool, &view.args, *owner);
            // (1) The same owner is mutably borrowed more than once —
            // two notes at DISTINCT arg spans.
            let occurrences: Vec<Span> = inout_uses
                .iter()
                .filter(|(o, _)| o == owner)
                .map(|(_, s)| *s)
                .collect();
            if occurrences.len() > 1 {
                let mut diag = Diag::error(
                    self.tir.span(r),
                    DiagCode::MutableAliasingViolation,
                    format!(
                        "cannot borrow {} as mutable more than once in the same call",
                        name
                    ),
                );
                diag = diag.with_note(Some(occurrences[0]), "first mutable borrow here");
                diag = diag.with_note(Some(occurrences[1]), "second mutable borrow here");
                diag = diag.with_help("a value can have one mutable borrow OR many immutable borrows in a call, never both (Rule 7)");
                sink.emit(diag);
            }
            // (2) inout ∩ borrowed (a view arg counts as an immutable
            // borrow of its root, E4).
            if borrowed.contains(owner) || view_borrowed.contains(owner) {
                sink.emit(
                    Diag::error(
                        self.tir.span(r),
                        DiagCode::MutableAliasingViolation,
                        format!(
                            "cannot borrow {} as immutable while it is mutably borrowed in the same call",
                            name
                        ),
                    )
                    .with_help("a value can have one mutable borrow OR many immutable borrows in a call, never both (Rule 7)"),
                );
            }
            // (3) inout ∩ moved.
            if moved.contains(owner) {
                sink.emit(
                    Diag::error(
                        self.tir.span(r),
                        DiagCode::MutableAliasingViolation,
                        format!(
                            "cannot move {} while it is mutably borrowed in the same call",
                            name
                        ),
                    )
                    .with_help("a value can have one mutable borrow OR many immutable borrows in a call, never both (Rule 7)"),
                );
            }
        }
        // Overlap — same owner borrowed AND moved in one call.
        for owner in borrowed.iter().filter(|o| moved.contains(o)) {
            let name = owner_name_for_diag(*owner, self.flow, self.pool);
            // Find spans of the conflicting arguments for this owner.
            let mut borrow_span = None;
            let mut move_span = None;
            for (i, arg) in view.args.iter().enumerate() {
                if !needs_tracking(self.tir.inst(arg).ty, self.pool) {
                    continue;
                }
                let mode = view.modes.get(i).unwrap_or(ParamMode::Borrow);
                // P6' (mirrors the Rule-7 partition above): a view
                // re-borrowed into a `str` arg via ViewAsOwner borrows
                // the view's ROOT owner.
                // The read fact names the binding home place (a
                // FieldAccess step carries the chain's ROOT place), so
                // alias resolution is uniform; ViewAsOwner args resolve
                // through the projection chain.
                let arg_owner = self.call_arg_owner(own, mode, arg);
                if arg_owner == *owner {
                    match mode {
                        ParamMode::Borrow => borrow_span = Some(self.tir.span(arg)),
                        ParamMode::Move => move_span = Some(self.tir.span(arg)),
                        ParamMode::Inout => {}
                    }
                }
            }
            let mut diag = Diag::error(
                self.tir.span(r),
                DiagCode::MoveWhileBorrowedInCall,
                format!("cannot move {} while it is borrowed in the same call", name),
            );
            if let Some(b_span) = borrow_span {
                diag = diag.with_note(Some(b_span), "borrowed here");
            }
            if let Some(m_span) = move_span {
                diag = diag.with_note(Some(m_span), "moved here");
            }
            diag = diag.with_help("borrows are live for the whole call; pass by `move` on a separate statement, or borrow in both positions");
            sink.emit(diag);
        }

        // P2 freeze (final spec §3.2): a view argument keeps its root
        // owner live for the whole call, so a `move` of the same owner
        // in the same call is a freeze violation.
        let mut view_move_overlap: Vec<Owner> = view_borrowed
            .iter()
            .filter(|o| moved.contains(o))
            .copied()
            .collect();
        view_move_overlap.sort_by_key(owner_sort_key);
        for owner in view_move_overlap {
            // E0020/E0021 already cover owners that are `Moved` or
            // `Borrowed` — don't double-report.
            if matches!(
                own.states.get(&owner),
                Some(OwnerState::Moved { .. }) | Some(OwnerState::Borrowed)
            ) {
                continue;
            }
            let name = owner_name_for_diag(owner, self.flow, self.pool);
            let mut diag = Diag::error(
                self.tir.span(r),
                DiagCode::SourceProjected,
                format!("cannot move {name} while a slice of it is live"),
            );
            for arg in &view.args {
                if self.pool.is_view(self.tir.inst(arg).ty)
                    && projection_root(own, self.tir, self.flow, self.pool, arg) == Some(owner)
                {
                    diag = diag.with_note(Some(self.tir.span(arg)), "slice passed here");
                    break;
                }
            }
            sink.emit(diag);
        }
    }

    fn commit_call_moves(
        &self,
        own: &mut Ownership,
        sink: &mut DiagSink,
        r: TirRef,
        view: &ryo_core::tir::CallView<'_>,
        view_borrowed: &[Owner],
    ) {
        // Phase 3 — commit the moves.
        for (i, arg) in view.args.iter().enumerate() {
            let mode = view.modes.get(i).unwrap_or(ParamMode::Borrow);
            let arg_ty = self.tir.inst(arg).ty;
            // E2 (final spec §3.3): slices cannot be passed to `move`
            // parameters. Backstop — sema rejects `move` on view-typed
            // parameters already.
            if mode == ParamMode::Move && self.pool.is_view(arg_ty) {
                sink.emit(
                    Diag::error(
                        self.tir.span(r),
                        DiagCode::ViewEscape,
                        "cannot pass a slice to a `move` parameter",
                    )
                    .with_help("slices are non-escaping; pass by default (borrow), or take an owned `str` parameter"),
                );
                continue;
            }
            if mode == ParamMode::Move && needs_tracking(arg_ty, self.pool) {
                let owner = own.underlying_owner(arg);
                // P2 freeze (final spec §3.2) — unless the view
                // partition above already reported this owner.
                if !view_borrowed.contains(&owner) {
                    check_source_projected(
                        self.tir,
                        self.flow,
                        self.pool,
                        own,
                        sink,
                        owner,
                        self.tir.span(r),
                        "move",
                        consumed_binding_name(self.tir, arg),
                    );
                }
                let consumed_name = consumed_binding_name(self.tir, arg);
                consume_for_assignment(
                    self.tir,
                    self.pool,
                    own,
                    sink,
                    arg,
                    self.tir.span(r),
                    consumed_name,
                    r,
                );
            }
        }
    }

    /// Aliasing read: the step's `read` fact names the binding home
    /// place; the owner live in that home is recorded for the
    /// post-walk last-use passes (replacing `collect_last_uses`), and a
    /// view read at its projection's last use queues the projection's
    /// death.
    fn analyze_var_read(&mut self, own: &mut Ownership, r: TirRef, dead: bool) {
        let inst = *self.tir.inst(r);
        let name = match inst.data {
            TirData::Var(n) => n,
            _ => unreachable!("Var must carry TirData::Var"),
        };
        // Error-shaped TIR can leave a `Var` unresolved (no read
        // fact) — the old name-keyed walk skipped those reads; mirror
        // that (the unit already carries sema's diagnostic).
        let Some(read) = self.step_facts(r).read else {
            return;
        };
        let owner = own.current_owner.get(&read).copied().unwrap_or(Owner(read));
        if needs_tracking(inst.ty, self.pool) {
            // Snapshot owner-at-read so the post-walk passes anchor to
            // the owner live *at this read*.
            Ownership::dense_set(&mut own.owner_at_read, r, owner);
        } else if self.pool.is_view(inst.ty) {
            // P4 lift (final spec §3.2): record the read; when it is
            // the projection's precomputed last use (and not
            // loop-deferred), the projection dies at the end of this
            // statement.
            Ownership::dense_set(&mut own.owner_at_read, r, owner);
            // A view-typed PARAM read owns no projection here (its
            // buffer belongs to the caller) — the dense view tables
            // are inst-indexed, so param owners skip the death check.
            if let Some(vi) = owner.inst_tirref(self.flow)
                && Ownership::dense_get(&own.view_defer_loop, vi).is_none()
                && Ownership::dense_get(&own.view_last_use, vi) == Some(r)
            {
                own.pending_dying.push(owner);
            }
        }
        // Last-use recording (old `collect_last_uses` semantics): by
        // NAME, latest DOCUMENT-order read wins; the per-read owner
        // snapshot keys the owner-level map (a Param owner lands under
        // its sentinel ref). The comparison uses the build's emission
        // order, not visit order: dead continuation blocks walk after
        // everything but emitted in place, and their reads must not
        // displace live last uses.
        if needs_tracking(inst.ty, self.pool) || self.pool.is_view(inst.ty) {
            if !dead {
                // Live blocks walk in document order: latest wins is a
                // plain insert.
                self.last_read_by_name.insert(name, r);
                if let Some(o) = Ownership::dense_get(&own.owner_at_read, r) {
                    self.last_use.insert(o.tirref(self.flow), r);
                }
            } else {
                // Dead continuation blocks walk after everything but
                // were emitted in place: their reads must not
                // displace a live last use, so compare emission
                // (document) order.
                let order = self.order(r);
                let later = match self.last_read_by_name.get(&name) {
                    Some(&cur) => self.order(cur) < order,
                    None => true,
                };
                if later {
                    self.last_read_by_name.insert(name, r);
                }
                if let Some(o) = Ownership::dense_get(&own.owner_at_read, r) {
                    let key = o.tirref(self.flow);
                    let later = match self.last_use.get(&key) {
                        Some(&cur) => self.order(cur) < order,
                        None => true,
                    };
                    if later {
                        self.last_use.insert(key, r);
                    }
                }
            }
        }
    }

    /// Record a `Slice`/`ToView` whose base is a borrowed `str`/`bytes`
    /// param. Only owner-typed bases promote, and only borrowed params
    /// leak the promotion buffer. The enclosing statement comes from
    /// the precomputed `stmt_of` table.
    fn record_promo_candidate(&mut self, own: &mut Ownership, view_inst: TirRef, base: TirRef) {
        let base_inst = self.tir.inst(base);
        if !matches!(
            self.pool.kind(base_inst.ty),
            TypeKind::Str | TypeKind::Bytes
        ) {
            return;
        }
        // The base read fact must name a borrowed param's place
        // directly (not a shadowing local seated on it).
        let Some(base_place) = own.inst_place[base.index()] else {
            return;
        };
        let ryo_core::flow::Place::Param { index, .. } = self.flow.place(base_place) else {
            return;
        };
        if self.tir.params[*index as usize].mode != ParamMode::Borrow {
            return;
        }
        let Some(stmt) = self.stmt_of[view_inst.index()] else {
            return;
        };
        // Loop convergence re-walks insts; record each view inst once.
        if own
            .promo_candidates
            .iter()
            .any(|c| c.view_inst == view_inst)
        {
            return;
        }
        own.promo_candidates.push(PromoCandidate {
            view_inst,
            base,
            stmt,
        });
    }
}
