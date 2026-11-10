//! Loop ownership analysis — split from `mod.rs`.
//!
//! The walk side (`analyze_loop`) is Flow IR-driven: the header block's
//! steps are the while condition (processed once with the entry state,
//! like the old single `visit_expr(cond)`), and the body fixed point
//! re-walks the body region's blocks per pass with a fresh epoch. The
//! structural `after_flows_off_end` test — "the body's live exit never
//! reaches the backedge" — replaces the old `stmt_must_return` on the
//! last body statement.
//!
//! The post-walk jump-exit Free scheduling (`schedule_loop_exit_frees_in`,
//! `LoopExitCtx`, `schedule_break_continue_frees`) lives in `jumps` —
//! see its module doc.

use super::{
    BranchState, Dominators, Owner, OwnerState, Ownership, RegionStop, StopKind, Walk,
    binding_state_writes, branch_states_differ, collect_region, flow_owner_place, match_arm_bodies,
    merge_loop_plain, merge_non_monotone, remove_loop_deferred_views, stmt_match,
    visit_operand_matches,
};
use ryo_core::diag::DiagSink;
use ryo_core::flow::fire_points::{FirePoint, FirePoints, FireVerdict};
use ryo_core::flow::{BlockId, DropPosition, Flow, Place, Terminator, WriteKind};
use ryo_core::ownership::FunctionSidecar;
use ryo_core::tir::{ExtraList, Tir, TirData, TirRef, TirTag};
use ryo_core::types::{InternPool, StringId};
use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};

mod jumps;
pub(crate) use jumps::*;

pub(crate) trait TirStmtList {
    type Iter<'a>: Iterator<Item = TirRef> + DoubleEndedIterator
    where
        Self: 'a;

    fn iter_refs(&self) -> Self::Iter<'_>;
}

impl TirStmtList for [TirRef] {
    type Iter<'a> = std::iter::Copied<std::slice::Iter<'a, TirRef>>;

    fn iter_refs(&self) -> Self::Iter<'_> {
        self.iter().copied()
    }
}

impl TirStmtList for Vec<TirRef> {
    type Iter<'a> = std::iter::Copied<std::slice::Iter<'a, TirRef>>;

    fn iter_refs(&self) -> Self::Iter<'_> {
        self.iter().copied()
    }
}

impl TirStmtList for ExtraList<'_, TirRef> {
    type Iter<'a>
        = ryo_core::extra::ExtraListIter<'a, TirRef>
    where
        Self: 'a;

    fn iter_refs(&self) -> Self::Iter<'_> {
        self.iter()
    }
}

/// The loop-header test block: the block terminating in `WhileTest`/
/// `ForTest` for `loop_inst`. A simple condition tests in the header
/// itself; a match/`and`/`or` condition cuts blocks and lands its test
/// in the condition's join — recorded nowhere, so it is located by a
/// bounded DFS (the condition region is acyclic and ends at the test).
pub(crate) fn loop_test_block(flow: &Flow, loop_inst: TirRef, header: BlockId) -> BlockId {
    fn is_test(flow: &Flow, loop_inst: TirRef, b: BlockId) -> bool {
        matches!(
            flow.term(b),
            Terminator::WhileTest { loop_inst: li, .. } | Terminator::ForTest { loop_inst: li, .. }
            if *li == loop_inst
        )
    }
    if is_test(flow, loop_inst, header) {
        return header;
    }
    let mut visited = vec![false; flow.blocks.len()];
    let mut stack = vec![header];
    while let Some(b) = stack.pop() {
        if visited[b.0 as usize] {
            continue;
        }
        visited[b.0 as usize] = true;
        if is_test(flow, loop_inst, b) {
            return b;
        }
        stack.extend_from_slice(flow.successors(b));
    }
    unreachable!("every loop has a test block")
}

/// `visit_block`'s `While`/`For` dispatch: header steps once, then the
/// propagate-then-check body fixed point over the body region, then the
/// deferred-projection cleanup. Returns the loop's exit block
/// (continuation — processed later with the merged post-loop state).
pub(crate) struct LoopFrame {
    loop_inst: TirRef,
    cond_region: Vec<BlockId>,
    region: Vec<BlockId>,
    stage: LoopStage,
    epoch: u32,
    after_flows_off_end: bool,
    snap: Option<BranchState>,
    hazard_count: usize,
    entry_state: Option<BranchState>,
    converged: bool,
    height_cap: usize,
    pass_count: usize,
    pass_active: bool,
    pub(crate) pass_io: Option<usize>,
}

enum LoopStage {
    Condition,
    Prepare,
    Propagate,
    Check,
    Final,
}

impl LoopFrame {
    pub(crate) fn new(
        w: &mut Walk<'_>,
        _own: &mut Ownership,
        ph: BlockId,
        epoch: u32,
    ) -> (Self, BlockId) {
        let (loop_inst, header) = match *w.flow.term(ph) {
            Terminator::While { loop_inst, header } | Terminator::For { loop_inst, header } => {
                (loop_inst, header)
            }
            _ => unreachable!("loop frame at a non-loop terminator"),
        };
        let test_block = loop_test_block(w.flow, loop_inst, header);
        let (body, exit) = match *w.flow.term(test_block) {
            Terminator::WhileTest { body, exit, .. } | Terminator::ForTest { body, exit, .. } => {
                (body, exit)
            }
            _ => unreachable!("loop test block holds the test"),
        };
        let (cond_region, _) = collect_region(w.flow, &mut w.region_scratch, header, |b| {
            RegionStop::Through(test_block).kind(b)
        });
        let (region, _) = collect_region(w.flow, &mut w.region_scratch, body, |b| {
            if b == exit || b == test_block {
                StopKind::Skip
            } else {
                StopKind::Visit
            }
        });
        (
            Self {
                loop_inst,
                cond_region,
                region,
                stage: LoopStage::Condition,
                epoch,
                after_flows_off_end: false,
                snap: None,
                hazard_count: 0,
                entry_state: None,
                converged: false,
                height_cap: 5 * w.tir.instructions.len() + 2,
                pass_count: 0,
                pass_active: false,
                pass_io: None,
            },
            exit,
        )
    }

    pub(crate) fn advance(
        &mut self,
        w: &mut Walk<'_>,
        own: &mut Ownership,
        _sink: &mut DiagSink,
        _sidecar: &mut FunctionSidecar,
    ) -> super::walk::machine::WalkAdvance {
        loop {
            match self.stage {
                LoopStage::Condition => {
                    self.stage = LoopStage::Prepare;
                    return super::walk::machine::WalkAdvance::seen(
                        self.cond_region.clone(),
                        self.epoch,
                    );
                }
                LoopStage::Prepare => {
                    self.prepare(w, own);
                    self.stage = LoopStage::Propagate;
                }
                LoopStage::Propagate => {
                    if !self.pass_active {
                        if self.converged || self.pass_count == self.height_cap {
                            debug_assert!(
                                self.converged,
                                "INTERNAL ANALYSIS FAILURE: loop propagate did not converge within \
                                 the lattice-height bound ({} passes) — the monotone operator regressed",
                                self.pass_count,
                            );
                            if !self.converged {
                                let _discarded = own.take_branch(
                                    self.snap.as_ref().expect("loop snapshot").clone(),
                                );
                            }
                            own.owner_hazards.truncate(self.hazard_count);
                            self.stage = LoopStage::Check;
                            continue;
                        }
                        self.pass_count += 1;
                        self.pass_active = true;
                        let pass_epoch = w.next_epoch();
                        return super::walk::machine::WalkAdvance::staging(
                            self.region.clone(),
                            pass_epoch,
                            DiagSink::new(),
                            FunctionSidecar::new(w.tir.name, w.tir.instructions.len()),
                        );
                    }
                    self.pass_active = false;
                    let after = own.take_branch(BranchState::default());
                    let snap = self.snap.as_ref().expect("loop snapshot");
                    let writes = if self.after_flows_off_end {
                        binding_state_writes(
                            &snap.current_owner,
                            &[
                                (&snap.current_owner, &snap.states),
                                (&after.current_owner, &after.states),
                            ],
                        )
                    } else {
                        Vec::new()
                    };
                    let previous = self.entry_state.take().expect("loop entry state");
                    let differ = {
                        let prior = previous.clone();
                        merge_loop_plain(own, previous, after, self.after_flows_off_end);
                        branch_states_differ(&prior, own)
                    };
                    if !differ {
                        for (owner, state) in writes {
                            own.states.insert(owner, state);
                        }
                        self.converged = true;
                    } else {
                        self.entry_state = Some(own.snapshot_branch());
                    }
                }
                LoopStage::Check => {
                    self.stage = LoopStage::Final;
                    let pass_epoch = w.next_epoch();
                    return super::walk::machine::WalkAdvance::phase(
                        self.region.clone(),
                        pass_epoch,
                    );
                }
                LoopStage::Final => {
                    let after = own.take_branch(BranchState::default());
                    merge_non_monotone(
                        own,
                        self.snap.take().expect("loop snapshot"),
                        after,
                        self.after_flows_off_end,
                    );
                    w.active_loop_carried_homes.pop();
                    remove_loop_deferred_views(own, self.loop_inst);
                    for &b in self.region.iter().chain(self.cond_region.iter()) {
                        w.mark_processed(b);
                        w.epoch[b.0 as usize] = self.epoch;
                    }
                    w.last_inst = Some(self.loop_inst);
                    return super::walk::machine::WalkAdvance::Done;
                }
            }
        }
    }

    fn prepare(&mut self, w: &mut Walk<'_>, own: &mut Ownership) {
        let mut loop_carried_homes = HashSet::new();
        for &b in &self.region {
            for step in w.flow.block_steps(b) {
                if w.tir.inst(step.inst).tag != TirTag::Assign {
                    continue;
                }
                let view = w.tir.assign_view(step.inst);
                if w.tir.inst(view.value).tag != TirTag::Match {
                    continue;
                }
                let Some(write) = step.facts.write else {
                    continue;
                };
                if write.kind != WriteKind::Replace {
                    continue;
                }
                let Place::Local { ty, .. } = w.flow.place(write.place) else {
                    continue;
                };
                if w.pool.needs_drop(*ty) && own.current_owner.contains_key(&write.place) {
                    loop_carried_homes.insert(write.place);
                }
            }
        }
        for &home in &loop_carried_homes {
            let dynamic_owner = Owner(home);
            let Some(old_owner) = own.current_owner.insert(home, dynamic_owner) else {
                continue;
            };
            if old_owner != dynamic_owner {
                let state = own
                    .states
                    .get(&old_owner)
                    .cloned()
                    .unwrap_or(OwnerState::NotTracked);
                own.states.insert(dynamic_owner, state);
                if !own.current_owner.values().any(|owner| *owner == old_owner) {
                    own.states.remove(&old_owner);
                }
            }
        }
        w.loop_regions.push((self.loop_inst, self.region.clone()));
        w.active_loop_carried_homes.push(loop_carried_homes);
        self.after_flows_off_end = self.region.iter().any(|&b| {
            matches!(
                w.flow.term(b),
                Terminator::Backedge { .. }
                    | Terminator::Break { .. }
                    | Terminator::Continue { .. }
            )
        });
        let snap = own.snapshot_branch();
        self.entry_state = Some(snap.clone());
        self.snap = Some(snap);
        self.hazard_count = own.owner_hazards.len();
    }
}

/// Per-instruction loop nesting as parent-pointer chains (see the
/// [`LoopNesting`] docs). Built from Flow IR: each loop terminator
/// stamps its header + body region blocks one level deeper than its
/// pre-header context; construction (block id) order visits an outer
/// loop's terminator before any inner loop's, so enclosing loops stamp
/// first and inner loops overwrite with deeper chains. A `for` loop's
/// start/end bounds evaluate in the pre-header block (Flow IR places
/// the *steps* outside the loop) but count as inside the loop — the
/// old TIR-subtree builder counted them too — so they and their
/// operand subtrees are stamped at the loop depth explicitly.
pub(crate) fn collect_loop_nesting(tir: &Tir, flow: &Flow) -> LoopNesting {
    let mut nesting = LoopNesting {
        innermost: vec![None; tir.instructions.len()],
        depth: vec![0; tir.instructions.len()],
    };
    // Per-block enclosing-loop context, stamped as regions are claimed.
    let mut block_innermost: Vec<Option<TirRef>> = vec![None; flow.blocks.len()];
    let mut block_depth: Vec<u32> = vec![0; flow.blocks.len()];
    let mut scratch = Vec::new();
    let mut mark_scratch = vec![false; tir.instructions.len()];
    for b in flow.forward_order() {
        let (loop_inst, header, bounds) = match *flow.term(b) {
            Terminator::While { loop_inst, header } => (loop_inst, header, None),
            Terminator::For { loop_inst, header } => {
                let view = tir.for_range_view(loop_inst);
                (loop_inst, header, Some((view.start, view.end)))
            }
            _ => continue,
        };
        let test_block = loop_test_block(flow, loop_inst, header);
        let (_body, exit) = match *flow.term(test_block) {
            Terminator::WhileTest { body, exit, .. } | Terminator::ForTest { body, exit, .. } => {
                (body, exit)
            }
            _ => unreachable!("loop test block holds the test"),
        };
        let context_innermost = block_innermost[b.0 as usize];
        let context_depth = block_depth[b.0 as usize];
        // The loop instruction itself sits at the enclosing nesting.
        nesting.innermost[loop_inst.index()] = context_innermost;
        nesting.depth[loop_inst.index()] = context_depth;
        let (region, _) = collect_region(flow, &mut scratch, header, |x| {
            if x == exit {
                StopKind::Skip
            } else {
                StopKind::Visit
            }
        });
        let claim = |nesting: &mut LoopNesting, r: TirRef| {
            nesting.innermost[r.index()] = Some(loop_inst);
            nesting.depth[r.index()] = context_depth + 1;
        };
        for &rb in &region {
            for step in flow.block_steps(rb) {
                claim(&mut nesting, step.inst);
            }
            // Structural payloads inside the region (a cond match, a
            // nested loop header, returns) nest at the region depth
            // exactly like the old subtree walk covered them; a nested
            // loop's own claim of its instruction (at its pre-header
            // context) agrees with this value.
            if let Some(p) = flow.term_payload(rb) {
                claim(&mut nesting, p);
            }
            block_innermost[rb.0 as usize] = Some(loop_inst);
            block_depth[rb.0 as usize] = context_depth + 1;
        }
        // For-range bounds: evaluated once in the pre-header but
        // ownership-nested inside the loop (a view read by the bounds
        // re-executes conceptually per the old subtree model).
        if let Some((start, end)) = bounds {
            mark_scratch.fill(false);
            let mut stack = vec![start, end];
            while let Some(r) = stack.pop() {
                if mark_scratch[r.index()] {
                    continue;
                }
                mark_scratch[r.index()] = true;
                claim(&mut nesting, r);
                tir.walk_operands(r, &mut |_, child, _| stack.push(child));
            }
        }
    }
    nesting
}

/// Every in-loop `Assign` value whose binding is declared BEFORE the
/// enclosing loop — loop-carried owners. Driven by the walk-recorded
/// body regions and document-order ranks (replaces the old
/// `collect_assign_writes` + `declared_before_stmt` recursion; the
/// predicate is identical: a `VarDecl` of the name anywhere earlier).
pub(crate) fn collect_loop_carried_owners(tir: &Tir, flow: &Flow, w: &Walk<'_>) -> HashSet<TirRef> {
    let mut out = HashSet::new();
    for &(loop_inst, ref region) in &w.loop_regions {
        let Some(loop_rank) = w.flow.order_of(loop_inst) else {
            continue;
        };
        for &b in region {
            for step in flow.block_steps(b) {
                if tir.inst(step.inst).tag != TirTag::Assign {
                    continue;
                }
                let view = tir.assign_view(step.inst);
                if w.decl_rank.get(&view.name).is_some_and(|r| *r < loop_rank) {
                    out.insert(view.value);
                    if tir.inst(view.value).tag == TirTag::Match
                        && let Some(write) = step.facts.write
                        && let Place::Local { decl, .. } = flow.place(write.place)
                    {
                        out.insert(*decl);
                    }
                }
            }
        }
    }
    out
}

/// Per-instruction loop-nesting as parent-pointer chains: each
/// instruction stores only its INNERMOST enclosing `WhileLoop`/
/// `ForRange` and its enclosing-loop count; the full ancestor stack is
/// recovered by walking parent hops. Dense tables indexed by
/// `TirRef::index()`, sized to `tir.instructions.len()` (slot 0 stays
/// `None`/0); `None`/depth 0 means "not nested".
#[derive(Clone, Debug, Default)]
pub(crate) struct LoopNesting {
    /// inst → innermost enclosing `WhileLoop`/`ForRange`, `None` at
    /// top level. The parent-hop table.
    innermost: Vec<Option<TirRef>>,
    /// inst → number of enclosing loops.
    depth: Vec<u32>,
}

impl LoopNesting {
    /// Number of loops enclosing `r` (0 at top level).
    pub(crate) fn depth_of(&self, r: TirRef) -> u32 {
        self.depth.get(r.index()).copied().unwrap_or(0)
    }

    /// `r`'s enclosing loops, innermost first, by parent hops.
    pub(crate) fn ancestors_innermost_first(&self, r: TirRef) -> impl Iterator<Item = TirRef> + '_ {
        let mut cur = self.innermost.get(r.index()).copied().flatten();
        std::iter::from_fn(move || {
            let l = cur?;
            cur = self.innermost[l.index()];
            Some(l)
        })
    }

    /// The enclosing loop whose OWN depth is `d` — innermost-first,
    /// that ancestor is `depth_of(r) - d - 1` parent hops away.
    pub(crate) fn ancestor_at_depth(&self, r: TirRef, d: u32) -> TirRef {
        let depth = self.depth_of(r);
        debug_assert!(
            d < depth,
            "no ancestor at depth {d} (nesting depth {depth})"
        );
        let hops = depth - d - 1;
        let ancestor = self
            .ancestors_innermost_first(r)
            .nth(hops as usize)
            .expect("d < depth_of(r) guarantees the ancestor exists");
        debug_assert_eq!(self.depth_of(ancestor), d);
        ancestor
    }
}

/// Outermost loop statement (`WhileLoop`/`ForRange`) whose body contains
/// `target`, or `None` when `target` is not inside any loop. Kept for
/// the post-walk dead-store re-anchor (statement-recursive; see the
/// module header).
pub(crate) fn outermost_loop_of(tir: &Tir, target: TirRef) -> Option<TirRef> {
    fn walk<S: TirStmtList + ?Sized>(
        tir: &Tir,
        stmts: &S,
        target: TirRef,
        stack: &mut Vec<TirRef>,
    ) -> Option<TirRef> {
        for r in stmts.iter_refs() {
            if r == target {
                return stack.first().copied();
            }
            // M12: statement-level matches arrive ExprStmt-wrapped —
            // unwrap before the tag dispatch so the arms are descended.
            let r = stmt_match(tir, r).unwrap_or(r);
            match tir.inst(r).tag {
                TirTag::IfStmt => {
                    let view = tir.if_stmt_view(r);
                    if let Some(found) = walk(tir, &view.then_stmts, target, stack) {
                        return Some(found);
                    }
                    for elif in &view.elif_branches {
                        if let Some(found) = walk(tir, &elif.body, target, stack) {
                            return Some(found);
                        }
                    }
                    if let Some(else_stmts) = &view.else_stmts
                        && let Some(found) = walk(tir, else_stmts, target, stack)
                    {
                        return Some(found);
                    }
                }
                TirTag::WhileLoop => {
                    stack.push(r);
                    let body = tir.while_loop_body_view(r);
                    let found = walk(tir, &body, target, stack);
                    stack.pop();
                    if found.is_some() {
                        return found;
                    }
                }
                TirTag::ForRange => {
                    stack.push(r);
                    let body = tir.for_range_body_view(r);
                    let found = walk(tir, &body, target, stack);
                    stack.pop();
                    if found.is_some() {
                        return found;
                    }
                }
                TirTag::Match => {
                    for body in match_arm_bodies(tir, r) {
                        if let Some(found) = walk(tir, &body, target, stack) {
                            return Some(found);
                        }
                    }
                }
                _ => {}
            }
            let mut found = None;
            visit_operand_matches(tir, r, &mut |m| {
                if found.is_some() {
                    return;
                }
                for body in match_arm_bodies(tir, m) {
                    if found.is_none() {
                        found = walk(tir, &body, target, stack);
                    }
                }
            });
            if found.is_some() {
                return found;
            }
        }
        None
    }
    let body = tir.body_stmts();
    walk(tir, &body, target, &mut Vec::new())
}

/// True if `name` is declared by a `VarDecl` anywhere before `stop` in
/// program order (any nesting depth). Guards the dead-store re-anchor.
/// Kept for the post-walk passes (see the module header).
pub(crate) fn declared_before_stmt(tir: &Tir, name: StringId, stop: TirRef) -> bool {
    fn walk<S: TirStmtList + ?Sized>(
        tir: &Tir,
        stmts: &S,
        name: StringId,
        stop: TirRef,
        found: &mut bool,
        stopped: &mut bool,
    ) {
        for r in stmts.iter_refs() {
            if *found || *stopped {
                return;
            }
            if r == stop {
                *stopped = true;
                return;
            }
            let r = stmt_match(tir, r).unwrap_or(r);
            match tir.inst(r).tag {
                TirTag::VarDecl => {
                    if tir.var_decl_view(r).name == name {
                        *found = true;
                        return;
                    }
                }
                TirTag::IfStmt => {
                    let view = tir.if_stmt_view(r);
                    walk(tir, &view.then_stmts, name, stop, found, stopped);
                    for elif in &view.elif_branches {
                        walk(tir, &elif.body, name, stop, found, stopped);
                    }
                    if let Some(else_stmts) = &view.else_stmts {
                        walk(tir, else_stmts, name, stop, found, stopped);
                    }
                }
                TirTag::WhileLoop => {
                    let body = tir.while_loop_body_view(r);
                    walk(tir, &body, name, stop, found, stopped);
                }
                TirTag::ForRange => {
                    let body = tir.for_range_body_view(r);
                    walk(tir, &body, name, stop, found, stopped);
                }
                TirTag::Match => {
                    for body in match_arm_bodies(tir, r) {
                        walk(tir, &body, name, stop, found, stopped);
                    }
                }
                _ => {}
            }
            visit_operand_matches(tir, r, &mut |m| {
                for body in match_arm_bodies(tir, m) {
                    walk(tir, &body, name, stop, found, stopped);
                }
            });
        }
    }
    let mut found = false;
    let mut stopped = false;
    let body = tir.body_stmts();
    walk(tir, &body, name, stop, &mut found, &mut stopped);
    found
}

/// True if any statement in `stmts` (any depth) is a `Return` /
/// `ReturnVoid` — a body that may leave the function early. Kept for
/// the post-walk passes.
pub(crate) fn body_may_return<S: TirStmtList + ?Sized>(tir: &Tir, stmts: &S) -> bool {
    for r in stmts.iter_refs() {
        let r = stmt_match(tir, r).unwrap_or(r);
        match tir.inst(r).tag {
            TirTag::Return | TirTag::ReturnVoid => return true,
            TirTag::IfStmt => {
                let view = tir.if_stmt_view(r);
                if body_may_return(tir, &view.then_stmts)
                    || view
                        .elif_branches
                        .iter()
                        .any(|e| body_may_return(tir, &e.body))
                    || view
                        .else_stmts
                        .as_ref()
                        .is_some_and(|es| body_may_return(tir, es))
                {
                    return true;
                }
            }
            TirTag::WhileLoop | TirTag::ForRange => {
                let body = tir.loop_body_view(r).expect("loop tag has a body");
                if body_may_return(tir, &body) {
                    return true;
                }
            }
            TirTag::Match => {
                for body in match_arm_bodies(tir, r) {
                    if body_may_return(tir, &body) {
                        return true;
                    }
                }
            }
            _ => {}
        }
        let mut may = false;
        visit_operand_matches(tir, r, &mut |m| {
            for body in match_arm_bodies(tir, m) {
                may = may || body_may_return(tir, &body);
            }
        });
        if may {
            return true;
        }
    }
    false
}

/// True when NO path through `stmts` reaches the end of the list: the
/// last statement terminates in Return/Break/Continue, or is an if
/// whose arms ALL must terminate. Kept for the post-walk passes
/// (`if_may_fall_through` / `stmt_must_return`).
pub(crate) fn body_must_terminate<S: TirStmtList + ?Sized>(tir: &Tir, stmts: &S) -> bool {
    let Some(last) = stmts.iter_refs().next_back() else {
        return false;
    };
    match tir.inst(last).tag {
        TirTag::Return | TirTag::ReturnVoid | TirTag::Break | TirTag::Continue => true,
        TirTag::IfStmt => {
            let view = tir.if_stmt_view(last);
            if view.else_stmts.is_none() {
                return false;
            }
            body_must_terminate(tir, &view.then_stmts)
                && view
                    .elif_branches
                    .iter()
                    .all(|e| body_must_terminate(tir, &e.body))
                && view
                    .else_stmts
                    .as_ref()
                    .is_some_and(|es| body_must_terminate(tir, es))
        }
        TirTag::WhileLoop | TirTag::ForRange => false,
        TirTag::Match => {
            let view = tir.match_view(last);
            view.arms().all(|arm| {
                let stmts: Vec<TirRef> = arm.body.stmts().collect();
                body_must_terminate(tir, &stmts)
            })
        }
        _ => {
            if let TirData::UnOp(o) = tir.inst(last).data
                && tir.inst(o).tag == TirTag::IfStmt
            {
                return body_must_terminate(tir, std::slice::from_ref(&o));
            }
            if let Some(m) = stmt_match(tir, last) {
                return body_must_terminate(tir, std::slice::from_ref(&m));
            }
            false
        }
    }
}

/// True when at least one path through the if reaches its merge block.
/// Kept for the post-walk passes.
pub(crate) fn if_may_fall_through(tir: &Tir, if_stmt: TirRef) -> bool {
    let view = tir.if_stmt_view(if_stmt);
    if view.else_stmts.is_none() {
        return true;
    }
    !body_must_terminate(tir, &view.then_stmts)
        || view
            .elif_branches
            .iter()
            .any(|e| !body_must_terminate(tir, &e.body))
        || view
            .else_stmts
            .as_ref()
            .is_some_and(|es| !body_must_terminate(tir, es))
}

/// The `IfStmt` whose MAIN condition's subtree contains `target`, or
/// `None`. Kept for the anonymous-temporary Free pass.
pub(crate) fn enclosing_if_main_cond(tir: &Tir, target: TirRef) -> Option<TirRef> {
    fn walk<S: TirStmtList + ?Sized>(tir: &Tir, stmts: &S, target: TirRef) -> Option<TirRef> {
        for r in stmts.iter_refs() {
            if !tir.contains_reachable(r, target) {
                continue;
            }
            match tir.inst(r).tag {
                TirTag::IfStmt => {
                    let view = tir.if_stmt_view(r);
                    if tir.contains_reachable(view.cond, target) {
                        return Some(r);
                    }
                    if let Some(found) = walk(tir, &view.then_stmts, target) {
                        return Some(found);
                    }
                    for arm in &view.elif_branches {
                        if let Some(found) = walk(tir, &arm.body, target) {
                            return Some(found);
                        }
                    }
                    if let Some(else_stmts) = &view.else_stmts
                        && let Some(found) = walk(tir, else_stmts, target)
                    {
                        return Some(found);
                    }
                    return None;
                }
                TirTag::WhileLoop | TirTag::ForRange => {
                    let body = tir.loop_body_view(r).expect("loop tag has a body");
                    if let Some(found) = walk(tir, &body, target) {
                        return Some(found);
                    }
                    return None;
                }
                _ => {
                    if let TirData::UnOp(o) = tir.inst(r).data
                        && matches!(
                            tir.inst(o).tag,
                            TirTag::IfStmt | TirTag::WhileLoop | TirTag::ForRange
                        )
                    {
                        return walk(tir, std::slice::from_ref(&o), target);
                    }
                    return None;
                }
            }
        }
        None
    }
    let body = tir.body_stmts();
    walk(tir, &body, target)
}

/// True when the branch contains no `Return`/`ReturnVoid` on any path.
/// Kept for the post-walk passes.
pub(crate) fn branch_may_not_return(tir: &Tir, branch_stmt: TirRef) -> bool {
    let branch_stmt = stmt_match(tir, branch_stmt).unwrap_or(branch_stmt);
    match tir.inst(branch_stmt).tag {
        TirTag::IfStmt => {
            let view = tir.if_stmt_view(branch_stmt);
            !body_may_return(tir, &view.then_stmts)
                && !view
                    .elif_branches
                    .iter()
                    .any(|e| body_may_return(tir, &e.body))
                && !view
                    .else_stmts
                    .as_ref()
                    .is_some_and(|es| body_may_return(tir, es))
        }
        TirTag::WhileLoop | TirTag::ForRange => {
            let body = tir
                .loop_body_view(branch_stmt)
                .expect("loop tag has a body");
            !body_may_return(tir, &body)
        }
        TirTag::Match => {
            let view = tir.match_view(branch_stmt);
            view.arms().all(|arm| {
                let stmts: Vec<TirRef> = arm.body.stmts().collect();
                !body_may_return(tir, &stmts)
            })
        }
        _ => false,
    }
}

/// Innermost branch statement (if/loop) containing `target` — the
/// conditional-last-use re-anchor for a binding declared INSIDE the
/// outermost branch: the outermost construct's exit fires once (leaking
/// every earlier iteration of a loop body), while the innermost
/// branch's exit is reached on the same iteration, after the read, and
/// a returning arm leaves before it (the return transfer owns that
/// path). Mirrors `outermost_branch_of`'s shape walk.
pub(crate) fn innermost_branch_of(tir: &Tir, target: TirRef) -> Option<TirRef> {
    fn walk<S: TirStmtList + ?Sized>(tir: &Tir, stmts: &S, target: TirRef) -> Option<TirRef> {
        for r in stmts.iter_refs() {
            let r = stmt_match(tir, r).unwrap_or(r);
            if !tir.contains_reachable(r, target) {
                continue;
            }
            match tir.inst(r).tag {
                TirTag::IfStmt => {
                    let view = tir.if_stmt_view(r);
                    if let Some(found) = walk(tir, &view.then_stmts, target) {
                        return Some(found);
                    }
                    for elif in view.elif_branches.iter() {
                        if let Some(found) = walk(tir, &elif.body, target) {
                            return Some(found);
                        }
                    }
                    if let Some(body) = &view.else_stmts
                        && let Some(found) = walk(tir, body, target)
                    {
                        return Some(found);
                    }
                    return Some(r);
                }
                TirTag::WhileLoop | TirTag::ForRange => {
                    let body = tir.loop_body_view(r).expect("loop tag has a body");
                    if let Some(found) = walk(tir, &body, target) {
                        return Some(found);
                    }
                    return Some(r);
                }
                TirTag::Match => {
                    for arm in tir.match_view(r).arms() {
                        let body: Vec<TirRef> = arm.body.stmts().collect();
                        if let Some(found) = walk(tir, &body, target) {
                            return Some(found);
                        }
                    }
                    return Some(r);
                }
                _ => {
                    if let TirData::UnOp(o) = tir.inst(r).data
                        && matches!(
                            tir.inst(o).tag,
                            TirTag::IfStmt | TirTag::WhileLoop | TirTag::ForRange
                        )
                        && tir.contains_reachable(o, target)
                    {
                        return walk(tir, std::slice::from_ref(&o), target);
                    }
                    let mut operand_match = None;
                    visit_operand_matches(tir, r, &mut |m| {
                        if operand_match.is_none() && tir.contains_reachable(m, target) {
                            operand_match = Some(m);
                        }
                    });
                    if let Some(m) = operand_match {
                        return walk(tir, std::slice::from_ref(&m), target);
                    }
                    return None;
                }
            }
        }
        None
    }
    let body = tir.body_stmts();
    walk(tir, &body, target)
}

/// Outermost branch statement containing `target`. Kept for the
/// conditional-last-use re-anchor. A `match` in VALUE position (an
/// operand of the containing statement, e.g. a VarDecl initializer)
/// counts as a branch too: its arms cut paths the same way an if's
/// do, and a last-use anchor inside one arm must re-anchor to the
/// match statement so the cleanup fires on every arm's path (the
/// switch payload resolves to the join — reached from all arms).
pub(crate) fn outermost_branch_of(tir: &Tir, target: TirRef) -> Option<TirRef> {
    fn walk<S: TirStmtList + ?Sized>(
        tir: &Tir,
        stmts: &S,
        target: TirRef,
        stack: &mut Vec<TirRef>,
    ) -> Option<TirRef> {
        for r in stmts.iter_refs() {
            if r == target {
                return stack.first().copied();
            }
            if !tir.contains_reachable(r, target) {
                continue;
            }
            match tir.inst(r).tag {
                TirTag::IfStmt => {
                    stack.push(r);
                    let view = tir.if_stmt_view(r);
                    let mut found = walk(tir, &view.then_stmts, target, stack);
                    for elif in &view.elif_branches {
                        if found.is_none() {
                            found = walk(tir, &elif.body, target, stack);
                        }
                    }
                    if found.is_none()
                        && let Some(else_stmts) = &view.else_stmts
                    {
                        found = walk(tir, else_stmts, target, stack);
                    }
                    if found.is_none() {
                        found = stack.first().copied();
                    }
                    stack.pop();
                    if found.is_some() {
                        return found;
                    }
                }
                TirTag::WhileLoop | TirTag::ForRange => {
                    stack.push(r);
                    let body = tir.loop_body_view(r).expect("loop tag has a body");
                    let mut found = walk(tir, &body, target, stack);
                    if found.is_none() {
                        found = stack.first().copied();
                    }
                    stack.pop();
                    if found.is_some() {
                        return found;
                    }
                }
                _ => {
                    if let TirData::UnOp(o) = tir.inst(r).data
                        && matches!(
                            tir.inst(o).tag,
                            TirTag::IfStmt | TirTag::WhileLoop | TirTag::ForRange
                        )
                        && tir.contains_reachable(o, target)
                    {
                        return walk(tir, std::slice::from_ref(&o), target, stack);
                    }
                    // A match nested in the statement's operands (a
                    // VarDecl initializer, an Assign value, a call
                    // argument — any depth): the outermost operand
                    // match containing the target. Scrutinee and arms
                    // alike are evaluated before the join, so an
                    // arm-local last use re-anchors to the match — the
                    // switch payload resolves to the join, reached
                    // from every non-diverging arm (expression arms
                    // included, which the statement-list descent above
                    // cannot see).
                    let mut operand_match: Option<TirRef> = None;
                    visit_operand_matches(tir, r, &mut |m| {
                        if operand_match.is_none() && tir.contains_reachable(m, target) {
                            operand_match = Some(m);
                        }
                    });
                    if let Some(m) = operand_match {
                        return Some(stack.first().copied().unwrap_or(m));
                    }
                    return stack.first().copied();
                }
            }
        }
        None
    }
    let body = tir.body_stmts();
    walk(tir, &body, target, &mut Vec::new())
}

fn declaration_dominates_anchor(
    flow: &Flow,
    fire_points: &FirePoints,
    dominators: &OnceCell<Dominators>,
    declaration: TirRef,
    anchor: TirRef,
) -> bool {
    let Some(declaration_block) = flow
        .step_block_of(declaration)
        .or_else(|| flow.term_block_of(declaration))
    else {
        return false;
    };
    let dominated = |point: FirePoint| {
        dominators
            .get_or_init(|| Dominators::new(flow))
            .dominates(declaration_block, point.block)
    };
    match fire_points.resolve(DropPosition::AfterStep { inst: anchor }) {
        FireVerdict::Resolved(point) => dominated(point),
        FireVerdict::ResolvedPerArm(points) => {
            !points.is_empty() && points.into_iter().all(dominated)
        }
        FireVerdict::NonExact(_) | FireVerdict::Unresolved => false,
    }
}

fn normally_completing_branch(tir: &Tir, branch: TirRef) -> bool {
    let mut body = HashSet::new();
    tir.collect_reachable(branch, &mut body);
    body.into_iter().all(|r| {
        !matches!(
            tir.inst(r).tag,
            TirTag::Return | TirTag::ReturnVoid | TirTag::Break | TirTag::Continue
        )
    })
}

/// Last-use Free anchor for a named-binding owner whose raw last read
/// sits inside a branch or loop: the OUTERMOST branch/loop statement's
/// exit (the earliest point dead on ALL paths), except a loop-carried
/// owner read again after the loop anchors at its true last read. A
/// loop-local owner's last read in a normally completing branch anchors
/// at that branch's exit, covering every path through the iteration.
/// Kept for the post-walk passes.
#[allow(clippy::too_many_arguments)]
pub(crate) fn last_use_anchor(
    tir: &Tir,
    flow: &Flow,
    pool: &InternPool,
    owner: TirRef,
    after: TirRef,
    sidecar: &FunctionSidecar,
    order: &[u32],
    last_read_by_name: &HashMap<StringId, TirRef>,
    loop_carried_owners: &HashSet<TirRef>,
    fire_points: &FirePoints,
    dominators: &OnceCell<Dominators>,
    return_transferred: bool,
) -> Option<TirRef> {
    // Assignment sites inherit the declaring binding, even when names shadow.
    let binding = owner_binding_decl(tir, owner, pool).and_then(|(site, name)| {
        if tir.inst(site).tag == TirTag::Assign {
            sidecar.assign_binding[site.index()].map(|decl| (decl, name))
        } else {
            Some((site, name))
        }
    });
    let binding_outlives = |branch_stmt: TirRef| {
        binding
            .is_some_and(|(decl, _)| decl.is_param() || !tir.contains_reachable(branch_stmt, decl))
    };
    let outermost = outermost_branch_of(tir, after);
    // A by-move arm takes a payload from the enum. Do not free the enum
    // after its scrutinee read. The arm must read the payload first.
    // Match cleanup frees the payload on consuming arms. It frees the
    // whole enum on other arms. Suppress only a direct scrutinee read
    // of the same binding. Match-free bodies (and anonymous owners)
    // skip the innermost-branch walk entirely: it can only ever yield
    // a `Match` candidate when the body contains one.
    let is_consuming_scrutinee_read = tir.contains_match
        && binding.is_some()
        && innermost_branch_of(tir, after).is_some_and(|candidate| {
            tir.inst(candidate).tag == TirTag::Match
                && tir.match_view(candidate).scrutinee == after
                && !tir.match_by_move_owner_refs(candidate, pool).is_empty()
                && binding.is_some_and(|(decl, _)| {
                    flow_owner_place(flow, after) == flow_owner_place(flow, decl)
                })
        });
    if is_consuming_scrutinee_read {
        return None;
    }
    // An all-diverging match has no completion point. Do not anchor the
    // scrutinee's last-use Free after it. That position cannot fire.
    // Return and loop-exit cleanup handle each terminating arm.
    if let Some(branch_stmt) = outermost
        && tir.inst(branch_stmt).tag == TirTag::Match
        && flow
            .term_block_of(branch_stmt)
            .is_some_and(|block| matches!(flow.term(block), Terminator::Switch { join: None, .. }))
    {
        return None;
    }
    let branch = outermost
        .filter(|&branch_stmt| binding_outlives(branch_stmt))
        .or_else(|| {
            // A match payload binding lives inside its match arm. If
            // its last read is nested in an if, the match itself cannot
            // anchor the Free because that fires after every arm. Use
            // the nested if join instead. Keep loop and if handling on
            // their existing paths: their exits have separate cleanup
            // rules for values declared inside them.
            outermost.filter(|&branch_stmt| tir.inst(branch_stmt).tag == TirTag::Match)?;
            innermost_branch_of(tir, after).filter(|&branch_stmt| {
                tir.inst(branch_stmt).tag == TirTag::IfStmt && binding_outlives(branch_stmt)
            })
        });
    Some(match branch {
        Some(branch_stmt) => {
            if matches!(
                tir.inst(branch_stmt).tag,
                TirTag::WhileLoop | TirTag::ForRange
            ) && loop_carried_owners.contains(&owner)
                && binding.is_some_and(|(_, name)| {
                    last_read_by_name.get(&name).is_some_and(|read| {
                        read != &after
                            && order.get(read.index()).copied().unwrap_or(0)
                                > order.get(branch_stmt.index()).copied().unwrap_or(0)
                    })
                })
            {
                binding
                    .and_then(|(_, name)| last_read_by_name.get(&name).copied())
                    .unwrap_or(branch_stmt)
            } else {
                branch_stmt
            }
        }
        // The transferred case: the owner has a return-site move
        // hazard (its buffer leaves through a Return on that path), so
        // a free anchored inside the returned-through region would
        // fire AFTER the sret store — freeing the caller's buffer. The
        // binding is declared INSIDE the outermost branch (a loop
        // body): the outermost exit fires once, leaking every earlier
        // iteration; a branch STRICTLY inside the outer construct is
        // reached on the same iteration and a returning arm leaves
        // before it (the return transfer owns that path).
        None if outermost.is_some_and(|branch_stmt| {
            binding.is_some()
                && return_transferred
                && matches!(
                    tir.inst(branch_stmt).tag,
                    TirTag::WhileLoop | TirTag::ForRange
                )
        }) =>
        {
            let declaration = binding.map(|(decl, _)| decl);
            match innermost_branch_of(tir, after) {
                Some(inner)
                    if Some(inner) != outermost
                        && declaration.is_some_and(|decl| {
                            declaration_dominates_anchor(flow, fire_points, dominators, decl, inner)
                        }) =>
                {
                    inner
                }
                _ => after,
            }
        }
        // A loop-local value can use a normally completing branch's exit
        // only when its declaration dominates every firing point.
        None if outermost.is_some_and(|branch_stmt| {
            binding.is_some()
                && matches!(
                    tir.inst(branch_stmt).tag,
                    TirTag::WhileLoop | TirTag::ForRange
                )
        }) =>
        {
            let declaration = binding.map(|(decl, _)| decl);
            let ancestors = ancestor_branches_of(tir, after);
            let loop_depth = ancestors.iter().rposition(|&branch_stmt| {
                matches!(
                    tir.inst(branch_stmt).tag,
                    TirTag::WhileLoop | TirTag::ForRange
                ) && declaration.is_some_and(|decl| tir.contains_reachable(branch_stmt, decl))
            });
            loop_depth
                .and_then(|depth| {
                    ancestors
                        .iter()
                        .skip(depth + 1)
                        .copied()
                        .find(|&candidate| {
                            normally_completing_branch(tir, candidate)
                                && declaration.is_some_and(|decl| {
                                    declaration_dominates_anchor(
                                        flow,
                                        fire_points,
                                        dominators,
                                        decl,
                                        candidate,
                                    )
                                })
                        })
                })
                .unwrap_or(after)
        }
        _ => after,
    })
}

/// All branch statements containing `target`, outermost first. Kept
/// for the return-epilogue dedup.
pub(crate) fn ancestor_branches_of(tir: &Tir, target: TirRef) -> Vec<TirRef> {
    fn walk<S: TirStmtList + ?Sized>(
        tir: &Tir,
        stmts: &S,
        target: TirRef,
        stack: &mut Vec<TirRef>,
    ) -> Option<Vec<TirRef>> {
        for r in stmts.iter_refs() {
            if r == target {
                return Some(stack.clone());
            }
            if !tir.contains_reachable(r, target) {
                continue;
            }
            let r = stmt_match(tir, r).unwrap_or(r);
            match tir.inst(r).tag {
                TirTag::IfStmt => {
                    stack.push(r);
                    let view = tir.if_stmt_view(r);
                    let mut found = walk(tir, &view.then_stmts, target, stack);
                    for elif in &view.elif_branches {
                        if found.is_none() {
                            found = walk(tir, &elif.body, target, stack);
                        }
                    }
                    if found.is_none()
                        && let Some(else_stmts) = &view.else_stmts
                    {
                        found = walk(tir, else_stmts, target, stack);
                    }
                    stack.pop();
                    if found.is_some() {
                        return found;
                    }
                }
                TirTag::WhileLoop | TirTag::ForRange => {
                    stack.push(r);
                    let body = tir.loop_body_view(r).expect("loop tag has a body");
                    let found = walk(tir, &body, target, stack);
                    stack.pop();
                    if found.is_some() {
                        return found;
                    }
                }
                TirTag::Match => {
                    stack.push(r);
                    let mut found = None;
                    for body in match_arm_bodies(tir, r) {
                        if found.is_none() {
                            found = walk(tir, &body, target, stack);
                        }
                    }
                    stack.pop();
                    if found.is_some() {
                        return found;
                    }
                }
                _ => {
                    let mut found = None;
                    visit_operand_matches(tir, r, &mut |m| {
                        if found.is_some() || !tir.contains_reachable(m, target) {
                            return;
                        }
                        stack.push(m);
                        for body in match_arm_bodies(tir, m) {
                            if found.is_none() {
                                found = walk(tir, &body, target, stack);
                            }
                        }
                        stack.pop();
                    });
                    return Some(found.unwrap_or_else(|| stack.clone()));
                }
            }
            let mut found = None;
            visit_operand_matches(tir, r, &mut |m| {
                if found.is_some() || !tir.contains_reachable(m, target) {
                    return;
                }
                stack.push(m);
                for body in match_arm_bodies(tir, m) {
                    if found.is_none() {
                        found = walk(tir, &body, target, stack);
                    }
                }
                stack.pop();
            });
            if found.is_some() {
                return found;
            }
        }
        None
    }
    let body = tir.body_stmts();
    walk(tir, &body, target, &mut Vec::new()).unwrap_or_default()
}

/// The binding whose initializer (`VarDecl`) or value (`Assign`)
/// produced `owner` — (decl statement, binding name) — or `None` for
/// anonymous producers. Kept for the post-walk passes. The decl
/// statement, not just the name, lets consumers tell a binding
/// declared outside a branch from a same-named shadow declared
/// inside it.
pub(crate) fn owner_binding_decl(
    tir: &Tir,
    owner: TirRef,
    pool: &InternPool,
) -> Option<(TirRef, StringId)> {
    fn walk<S: TirStmtList + ?Sized>(
        tir: &Tir,
        stmts: &S,
        owner: TirRef,
        pool: &InternPool,
    ) -> Option<(TirRef, StringId)> {
        for r in stmts.iter_refs() {
            let r = stmt_match(tir, r).unwrap_or(r);
            match tir.inst(r).tag {
                TirTag::VarDecl => {
                    let view = tir.var_decl_view(r);
                    if view.initializer == owner || r == owner {
                        return Some((r, view.name));
                    }
                }
                TirTag::Assign => {
                    let view = tir.assign_view(r);
                    if view.value == owner {
                        return Some((r, view.name));
                    }
                }
                TirTag::IfStmt => {
                    let view = tir.if_stmt_view(r);
                    if let Some(found) = walk(tir, &view.then_stmts, owner, pool) {
                        return Some(found);
                    }
                    for elif in &view.elif_branches {
                        if let Some(found) = walk(tir, &elif.body, owner, pool) {
                            return Some(found);
                        }
                    }
                    if let Some(else_stmts) = &view.else_stmts
                        && let Some(found) = walk(tir, else_stmts, owner, pool)
                    {
                        return Some(found);
                    }
                }
                TirTag::WhileLoop | TirTag::ForRange => {
                    let body = tir.loop_body_view(r).expect("loop tag has a body");
                    if let Some(found) = walk(tir, &body, owner, pool) {
                        return Some(found);
                    }
                }
                TirTag::Match => {
                    let token_refs = tir.match_by_move_owner_refs(r, pool);
                    let mut token_cursor = 0;
                    for arm in tir.match_view(r).arms() {
                        for binding in arm.bindings() {
                            if pool.is_copy(binding.ty) {
                                continue;
                            }
                            let token = token_refs[token_cursor];
                            token_cursor += 1;
                            if token == owner {
                                return Some((r, binding.name));
                            }
                        }
                        let body: Vec<TirRef> = arm.body.stmts().collect();
                        if let Some(found) = walk(tir, &body, owner, pool) {
                            return Some(found);
                        }
                    }
                }
                _ => {}
            }
            let mut found = None;
            visit_operand_matches(tir, r, &mut |m| {
                if found.is_some() {
                    return;
                }
                for body in match_arm_bodies(tir, m) {
                    if found.is_none() {
                        found = walk(tir, &body, owner, pool);
                    }
                }
            });
            if found.is_some() {
                return found;
            }
        }
        None
    }
    let body = tir.body_stmts();
    walk(tir, &body, owner, pool)
}

/// Insert into an arg-partition set. These sets are arg-count-sized
/// (usually ≤ 3 entries, very often empty), so a linear-scan Vec beats
/// a HashSet.
pub(crate) fn push_unique(set: &mut Vec<Owner>, owner: Owner) {
    if !set.contains(&owner) {
        set.push(owner);
    }
}
