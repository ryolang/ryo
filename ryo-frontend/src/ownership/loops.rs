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
//! The post-walk scheduling helpers (`schedule_loop_exit_frees_in` and
//! its `LoopExitCtx`) stay statement-recursive: they are sidecar↔TIR
//! relation transforms (jump paths, loop-carried classification) pinned
//! by the free-scheduling tests, outside this migration's walk scope.
//! Their consumers keep `stmt_match` / `visit_operand_matches` /
//! `match_arm_bodies` alive (the scheduling helpers still reason about
//! statement-level matches in TIR terms, outside the Flow-walked body).

use super::{
    BranchState, Owner, OwnerState, Ownership, RegionStop, StopKind, Walk, anchored_drops,
    binding_state_writes, branch_states_differ, collect_region, drop_anchor, match_arm_bodies,
    merge_loop_plain, merge_non_monotone, owner_sort_key, remove_loop_deferred_views, stmt_match,
    visit_operand_matches,
};
use ryo_core::diag::DiagSink;
use ryo_core::flow::{BlockId, DropOp, DropPosition, Flow, Place, Terminator, WriteKind};
use ryo_core::ownership::FunctionSidecar;
use ryo_core::tir::{MatchBodyView, Tir, TirData, TirRef, TirTag};
use ryo_core::types::StringId;
use std::collections::{HashMap, HashSet};

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
pub(crate) fn analyze_loop(
    w: &mut Walk<'_>,
    own: &mut Ownership,
    sink: &mut DiagSink,
    sidecar: &mut FunctionSidecar,
    ph: BlockId,
    epoch: u32,
) -> Option<BlockId> {
    let (loop_inst, header) = match *w.flow.term(ph) {
        Terminator::While { loop_inst, header } | Terminator::For { loop_inst, header } => {
            (loop_inst, header)
        }
        _ => unreachable!("analyze_loop at a non-loop terminator"),
    };
    let test_block = loop_test_block(w.flow, loop_inst, header);
    let (body, exit) = match *w.flow.term(test_block) {
        Terminator::WhileTest { body, exit, .. } | Terminator::ForTest { body, exit, .. } => {
            (body, exit)
        }
        _ => unreachable!("loop test block holds the test"),
    };
    // Condition region: the header block plus any cascade a match/
    // `and`/`or` condition cuts, ending at the test block (inclusive).
    // For a `while` these steps are the condition — processed ONCE with
    // the entry state (the fixed-point phases walk the body region
    // only, so cond steps never re-run and cond-tied projections are
    // not drained mid-body — the old walk visited the cond exactly
    // once). A `for` test carries no steps; its bounds ran in the
    // pre-header. The test terminator is not dispatched: body and exit
    // are driven explicitly below.
    let (cond_region, _) = collect_region(w.flow, &mut w.region_scratch, header, |b| {
        RegionStop::Through(test_block).kind(b)
    });
    w.walk_block_list(own, sink, sidecar, &cond_region, epoch);
    // Body region: blocks reachable from `body` that cannot leave
    // without passing `exit` or the test block (the backedge targets
    // the test, which is not part of the body).
    let (region, _) = collect_region(w.flow, &mut w.region_scratch, body, |b| {
        if b == exit || b == test_block {
            StopKind::Skip
        } else {
            StopKind::Visit
        }
    });
    let mut loop_carried_homes = HashSet::new();
    for &b in &region {
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
    // A mutable binding assigned from a match in the loop can hold
    // values produced by any iteration. Give that runtime slot a stable
    // owner identity before the fixed-point walk, so no later release
    // names a producer that may not dominate the loop exit.
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
                own.pending_dead_store.remove(&old_owner);
                own.states.remove(&old_owner);
            }
        }
    }
    // Record the body region for the post-walk loop-carried scan.
    // The region is marked processed only after the fixed point below:
    // phase passes must re-walk these blocks, and the `processed` mark
    // gates the once-ever walks (the outer driver and enclosing arm
    // lists skip the region from then on).
    w.loop_regions.push((loop_inst, region.clone()));
    w.active_loop_carried_homes.push(loop_carried_homes);
    // Structural whole-region test, never per-owner — the Flow IR
    // replacement for the old `!stmt_must_return(last body stmt)`: the
    // body's end-state reaches the back-edge / post-loop join iff some
    // live region exit falls through (Backedge), breaks to the exit, or
    // continues to the header. A body returning on every live path
    // (only Return exits) cannot complete without exiting the function
    // — its Moved end-state reaches neither the backedge nor the join,
    // so the merges below drop it (merge_non_monotone).
    let after_flows_off_end = region.iter().any(|&b| {
        matches!(
            w.flow.term(b),
            Terminator::Backedge { .. } | Terminator::Break { .. } | Terminator::Continue { .. }
        )
    });

    let snap = own.snapshot_branch();
    // Phase 1 — convergent propagation (G3). Chaotic iteration of a
    // MONOTONE loop-entry join to a real fixed point:
    //
    // Lattice. Per-owner state ordered NotTracked < {Valid, Borrowed}
    // < Moved (Moved absorbing; the join never distinguishes Valid vs
    // Borrowed once merged — entry first-observed wins — so the two
    // form one class below Moved), product-ordered over the owner set;
    // plus live-projection sets under union (membership grows in one
    // direction only); plus the first-wins home/binding maps and the
    // dead-store map (see `BranchState` — the five merge-mutated
    // fields).
    //
    // Operator monotonicity. Each pass merges `entry ⊔ post-body` with
    // `merge_loop_plain`: any-Moved-wins adds Moved-ness and inserts
    // missing keys; projections union; the maps keep the entry side.
    // The four maps (states, current_owner, binding_of_name,
    // live_projections) therefore ASCEND the product order (never
    // removes a Moved, never shrinks a projection set, never changes a
    // seated home). The fifth, pending_dead_store, is neither growing
    // nor shrinking — its merged key set EQUALS the post-body key set
    // ((entry ∩ after) ∪ after-locals = after's keys), and the walk is
    // state-independent (every reachable transition fires regardless
    // of the lattice state — the G3 finding), so `after` — hence the
    // merged dead-store map — is identical on every pass and the map
    // aligns after a single merge.
    //
    // Convergence test. `differ` compares the PREVIOUS entry against
    // the MERGED state now in `own` — the state the iteration actually
    // feeds back. Comparing against the RAW post-body state instead is
    // a real bug (cap abort): when the body's live exits are Return-only,
    // the merge DISCARDS the body's changes (path-local to the exit —
    // see merge_loop_plain), the merged entry equals the previous entry
    // (a genuine fixed point), while the raw post-body state keeps
    // differing on every pass. The merged-vs-entry comparison covers
    // all five merge-mutated components (see branch_states_differ) —
    // for live_projections by set MEMBERSHIP, not emptiness: the merged
    // set is the state the next pass consumes, and `{v1}` vs `{v1, v2}`
    // are different states even though both are non-empty — so
    // `differ == false` exactly when the next pass could observe
    // nothing new.
    //
    // Height. Every non-converging pass consumes at least one ascension
    // unit on the four monotone maps: a state key inserted or flipped
    // toward Moved (≤ 2 per owner), a home or binding-map key inserted
    // (≤ 1 per binding), a projection view inserted (≤ 1 per view) —
    // owners, bindings, and views are all bounded by the instruction
    // arena A — plus at most ONE dead-store alignment pass (the map is
    // pass-constant, see above). Projection-set insertion was already
    // counted at ≤ 1 unit per view, and the membership witness adds no
    // new ascension kind: the join is monotone (pointwise ≥ the
    // previous entry), so a membership difference between previous and
    // merged entry IS a strict set growth — at least one view inserted
    // — never a lateral move that the stricter compare would chase
    // forever. Chain height ≤ 2A + A + A + A + 1, so the cap below
    // bounds every real pass sequence. (For real bodies the walk is
    // state-independent, so the fixed point is reached in exactly 2
    // passes; the bound is the guarantee, not the expectation.)
    //
    // The binding-aware override (`binding_state_writes`) is NOT
    // monotone — it can write a binding's end-of-body state over a
    // Moved owner (the consume-then-rebind shape), which fed back into
    // the old per-pass merge and made the entry oscillate (the old
    // load-bearing two-pass cap existed for exactly that). It is
    // therefore kept OUT of the iteration and applied once as a final
    // polish on the converged entry — legal because no further
    // iteration consumes the result (plan option (a)). The Phase-2
    // check pass and the final merge see the polished state, matching
    // the old cap's observable behavior on the pinned shapes. When the
    // body's live exits are Return-only the after side is excluded
    // from the polish (same path-locality rule as the merge — a
    // Return-only body's moves/reseats never reach the join).
    //
    // Fallback. Cap exhaustion is UNREACHABLE for the monotone operator
    // — reaching it is an INTERNAL ANALYSIS FAILURE (a compiler bug),
    // never a normal outcome: the debug_assert above makes it LOUD in
    // debug builds. The release fallback is purely the defensive arm:
    // it restores the pre-loop snapshot, whose conservative direction
    // is documented, not assumed —
    // (a) the walk's transitions are state-independent — every
    // reachable free/consume still schedules from the snap-based pass;
    // (b) the iterated chain ASCENDS from `snap`, so snap ≤ every
    // iterated entry: snap-based post-loop states UNDER-approximate
    // Moved / OVER-approximate destruction obligations — leak-safe,
    // never a UAF;
    // (c) the cost of (b) is that diagnostics can UNDER-REPORT (the
    // check pass ran against a smaller Moved set). That under-reporting
    // is the fallback's documented conservative direction: tolerable
    // only because the state is unreachable-by-proof, never a silently
    // accepted analysis result.
    let mut entry_state = snap.clone();
    let mut converged = false;
    let height_cap = 5 * w.tir.instructions.len() + 2;
    let mut pass_count = 0usize;
    while pass_count < height_cap {
        pass_count += 1;
        let mut scratch = DiagSink::new();
        // Fresh scratch sidecar: speculative sidecar writes are
        // discarded at the end of each pass, and the walk never reads
        // pre-existing sidecar content (the only readers — LoopExitCtx /
        // the return epilogue — run post-walk against the REAL
        // sidecar).
        let mut staging = FunctionSidecar::new(w.tir.name, w.tir.instructions.len());
        let pass_epoch = w.next_epoch();
        w.walk_phase(own, &mut scratch, &mut staging, &region, pass_epoch);
        // Move the post-body state out, installing an empty placeholder
        // — the merge overwrites all five fields below.
        let after = own.take_branch(BranchState::default());
        // Compute the binding-aware override writes while `after` is
        // intact; applied only on the converging pass (below) — never
        // fed back into iteration. The pre-owner side is the PRE-LOOP
        // snapshot (not the converged entry): the override models a
        // reseated binding's home holding a fresh value each iteration,
        // so the pre-rebind owner's header state is the one from BEFORE
        // the loop — reproducing the old feedback merge's final state
        // exactly (the pinned consume-then-rebind shape). A Return-only
        // body's after side is path-local to the exit (see the proof
        // comment) and contributes nothing.
        let writes = if after_flows_off_end {
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
        // Always merge (entry ⊔ post-body) into `own`. The convergence
        // test compares the PREVIOUS entry against THIS MERGED state
        // (not the raw post-body state — see the proof comment): the
        // merged state is what the next pass would walk from.
        let prev_entry = entry_state.clone();
        merge_loop_plain(own, entry_state, after, after_flows_off_end);
        let differ = branch_states_differ(&prev_entry, own);
        if !differ {
            // Final polish (no feedback — see the proof comment).
            for (owner, state) in writes {
                own.states.insert(owner, state);
            }
            converged = true;
            break;
        }
        entry_state = own.snapshot_branch();
    }
    debug_assert!(
        converged,
        "INTERNAL ANALYSIS FAILURE: loop propagate did not converge within \
         the lattice-height bound ({} passes) — the monotone operator regressed",
        pass_count,
    );
    if !converged {
        // Defensive arm (unreachable — see the proof comment): restore
        // the pre-loop snapshot. Its conservative direction is
        // documented there: leak-safe, never a UAF; diagnostics may
        // under-report.
        let _discarded = own.take_branch(snap.clone());
    }

    // Phase 2 — single check pass against the real sink and sidecar:
    // a binding moved inside the body without rebinding before the
    // back-edge surfaces as E0020 on this pass.
    let pass_epoch = w.next_epoch();
    w.walk_phase(own, sink, sidecar, &region, pass_epoch);
    // Final merge with the loop-entry snapshot: the loop may execute
    // zero times, so post-loop state = entry ⊔ post-check-pass.
    let after = own.take_branch(BranchState::default());
    merge_non_monotone(own, snap, after, after_flows_off_end);
    w.active_loop_carried_homes.pop();
    // P4 (final spec §3.2): projections whose last read is inside this
    // loop (but whose creation is outside it) die at the loop's exit.
    remove_loop_deferred_views(own, loop_inst);
    for &b in region.iter().chain(cond_region.iter()) {
        w.mark_processed(b);
        // Re-stamp the finished region with the enclosing generation's
        // epoch: enclosing once-ever walks (the id-ordered driver, an
        // arm block list) skip it via the epoch guard, while the
        // per-pass phase epochs above stay distinct.
        w.epoch[b.0 as usize] = epoch;
    }
    w.last_inst = Some(loop_inst);
    Some(exit)
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
    fn walk(
        tir: &Tir,
        stmts: &[TirRef],
        target: TirRef,
        stack: &mut Vec<TirRef>,
    ) -> Option<TirRef> {
        for &r in stmts {
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
                    let found = walk(tir, &tir.while_loop_view(r).body, target, stack);
                    stack.pop();
                    if found.is_some() {
                        return found;
                    }
                }
                TirTag::ForRange => {
                    stack.push(r);
                    let found = walk(tir, &tir.for_range_view(r).body, target, stack);
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
    walk(tir, &tir.body_stmts(), target, &mut Vec::new())
}

/// True if `name` is declared by a `VarDecl` anywhere before `stop` in
/// program order (any nesting depth). Guards the dead-store re-anchor.
/// Kept for the post-walk passes (see the module header).
pub(crate) fn declared_before_stmt(tir: &Tir, name: StringId, stop: TirRef) -> bool {
    fn walk(
        tir: &Tir,
        stmts: &[TirRef],
        name: StringId,
        stop: TirRef,
        found: &mut bool,
        stopped: &mut bool,
    ) {
        for &r in stmts {
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
                    walk(
                        tir,
                        &tir.while_loop_view(r).body,
                        name,
                        stop,
                        found,
                        stopped,
                    );
                }
                TirTag::ForRange => {
                    walk(tir, &tir.for_range_view(r).body, name, stop, found, stopped);
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
    walk(tir, &tir.body_stmts(), name, stop, &mut found, &mut stopped);
    found
}

/// True if any statement in `stmts` (any depth) is a `Return` /
/// `ReturnVoid` — a body that may leave the function early. Kept for
/// the post-walk passes.
pub(crate) fn body_may_return(tir: &Tir, stmts: &[TirRef]) -> bool {
    for &r in stmts {
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
                if let Some(body) = tir.loop_body(r)
                    && body_may_return(tir, &body)
                {
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
pub(crate) fn body_must_terminate(tir: &Tir, stmts: &[TirRef]) -> bool {
    let Some(&last) = stmts.last() else {
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
    let arm_falls_through = |body: &[TirRef]| !body_must_terminate(tir, body);
    arm_falls_through(&view.then_stmts)
        || view
            .elif_branches
            .iter()
            .any(|e| arm_falls_through(&e.body))
        || view
            .else_stmts
            .as_ref()
            .is_some_and(|es| arm_falls_through(es))
}

/// The `IfStmt` whose MAIN condition's subtree contains `target`, or
/// `None`. Kept for the anonymous-temporary Free pass.
pub(crate) fn enclosing_if_main_cond(tir: &Tir, target: TirRef) -> Option<TirRef> {
    fn walk(tir: &Tir, stmts: &[TirRef], target: TirRef) -> Option<TirRef> {
        for &r in stmts {
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
                    if let Some(body) = tir.loop_body(r)
                        && let Some(found) = walk(tir, &body, target)
                    {
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
    walk(tir, &tir.body_stmts(), target)
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
        TirTag::WhileLoop | TirTag::ForRange => match tir.loop_body(branch_stmt) {
            Some(body) => !body_may_return(tir, &body),
            None => false,
        },
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
    fn walk(tir: &Tir, stmts: &[TirRef], target: TirRef) -> Option<TirRef> {
        for &r in stmts {
            let r = stmt_match(tir, r).unwrap_or(r);
            if !tir.contains_reachable(r, target) {
                continue;
            }
            match tir.inst(r).tag {
                TirTag::IfStmt => {
                    let view = tir.if_stmt_view(r);
                    for body in std::iter::once(&view.then_stmts)
                        .chain(view.elif_branches.iter().map(|e| &e.body))
                        .chain(view.else_stmts.iter())
                    {
                        if let Some(found) = walk(tir, body, target) {
                            return Some(found);
                        }
                    }
                    return Some(r);
                }
                TirTag::WhileLoop | TirTag::ForRange => {
                    if let Some(body) = tir.loop_body(r)
                        && let Some(found) = walk(tir, &body, target)
                    {
                        return Some(found);
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
                        return walk(tir, &[o], target);
                    }
                    return None;
                }
            }
        }
        None
    }
    walk(tir, &tir.body_stmts(), target)
}

/// Outermost branch statement containing `target`. Kept for the
/// conditional-last-use re-anchor. A `match` in VALUE position (an
/// operand of the containing statement, e.g. a VarDecl initializer)
/// counts as a branch too: its arms cut paths the same way an if's
/// do, and a last-use anchor inside one arm must re-anchor to the
/// match statement so the cleanup fires on every arm's path (the
/// switch payload resolves to the join — reached from all arms).
pub(crate) fn outermost_branch_of(tir: &Tir, target: TirRef) -> Option<TirRef> {
    fn walk(
        tir: &Tir,
        stmts: &[TirRef],
        target: TirRef,
        stack: &mut Vec<TirRef>,
    ) -> Option<TirRef> {
        for &r in stmts {
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
                    let mut found = match tir.loop_body(r) {
                        Some(body) => walk(tir, &body, target, stack),
                        None => None,
                    };
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
                        return walk(tir, &[o], target, stack);
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
    walk(tir, &tir.body_stmts(), target, &mut Vec::new())
}

/// Last-use Free anchor for a named-binding owner whose raw last read
/// sits inside a branch or loop: the OUTERMOST branch/loop statement's
/// exit (the earliest point dead on ALL paths), except a loop-carried
/// owner read again after the loop anchors at its true last read. Kept
/// for the post-walk passes.
#[allow(clippy::too_many_arguments)]
pub(crate) fn last_use_anchor(
    tir: &Tir,
    flow: &Flow,
    owner: TirRef,
    after: TirRef,
    sidecar: &FunctionSidecar,
    order: &[u32],
    last_read_by_name: &HashMap<StringId, TirRef>,
    loop_carried_owners: &HashSet<TirRef>,
    return_transferred: bool,
) -> Option<TirRef> {
    // Assignment sites inherit the declaring binding, even when names shadow.
    let binding = owner_binding_decl(tir, owner).and_then(|(site, name)| {
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
    // An all-diverging match has no completion point. Do not anchor the
    // scrutinee's last-use Free after it. That position cannot fire.
    // Return and loop-exit cleanup handle each terminating arm.
    if let Some(branch_stmt) = outermost_branch_of(tir, after)
        && tir.inst(branch_stmt).tag == TirTag::Match
        && flow
            .term_block_of(branch_stmt)
            .is_some_and(|block| matches!(flow.term(block), Terminator::Switch { join: None, .. }))
    {
        return None;
    }
    Some(match outermost_branch_of(tir, after) {
        Some(branch_stmt) if binding_outlives(branch_stmt) => {
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
        Some(branch_stmt)
            if binding.is_some()
                && return_transferred
                && matches!(
                    tir.inst(branch_stmt).tag,
                    TirTag::WhileLoop | TirTag::ForRange
                ) =>
        {
            match innermost_branch_of(tir, after) {
                Some(inner) if inner != branch_stmt => inner,
                _ => after,
            }
        }
        _ => after,
    })
}

/// All branch statements containing `target`, outermost first. Kept
/// for the return-epilogue dedup.
pub(crate) fn ancestor_branches_of(tir: &Tir, target: TirRef) -> Vec<TirRef> {
    fn walk(
        tir: &Tir,
        stmts: &[TirRef],
        target: TirRef,
        stack: &mut Vec<TirRef>,
    ) -> Option<Vec<TirRef>> {
        for &r in stmts {
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
                    let found = match tir.loop_body(r) {
                        Some(body) => walk(tir, &body, target, stack),
                        None => None,
                    };
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
    walk(tir, &tir.body_stmts(), target, &mut Vec::new()).unwrap_or_default()
}

/// The binding whose initializer (`VarDecl`) or value (`Assign`)
/// produced `owner` — (decl statement, binding name) — or `None` for
/// anonymous producers. Kept for the post-walk passes. The decl
/// statement, not just the name, lets consumers tell a binding
/// declared outside a branch from a same-named shadow declared
/// inside it.
pub(crate) fn owner_binding_decl(tir: &Tir, owner: TirRef) -> Option<(TirRef, StringId)> {
    fn walk(tir: &Tir, stmts: &[TirRef], owner: TirRef) -> Option<(TirRef, StringId)> {
        for &r in stmts {
            let r = stmt_match(tir, r).unwrap_or(r);
            match tir.inst(r).tag {
                TirTag::VarDecl => {
                    let view = tir.var_decl_view(r);
                    if view.initializer == owner {
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
                    if let Some(found) = walk(tir, &view.then_stmts, owner) {
                        return Some(found);
                    }
                    for elif in &view.elif_branches {
                        if let Some(found) = walk(tir, &elif.body, owner) {
                            return Some(found);
                        }
                    }
                    if let Some(else_stmts) = &view.else_stmts
                        && let Some(found) = walk(tir, else_stmts, owner)
                    {
                        return Some(found);
                    }
                }
                TirTag::WhileLoop | TirTag::ForRange => {
                    if let Some(body) = tir.loop_body(r)
                        && let Some(found) = walk(tir, &body, owner)
                    {
                        return Some(found);
                    }
                }
                TirTag::Match => {
                    for body in match_arm_bodies(tir, r) {
                        if let Some(found) = walk(tir, &body, owner) {
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
                        found = walk(tir, &body, owner);
                    }
                }
            });
            if found.is_some() {
                return found;
            }
        }
        None
    }
    walk(tir, &tir.body_stmts(), owner)
}

// ---------- Post-walk jump-exit free scheduling (kept TIR-based) ----------

/// Per-loop invariants for jump-exit Free scheduling, computed once
/// when the traversal enters a `WhileLoop`/`ForRange` and shared by
/// every `break`/`continue` jump inside the loop. See
/// `schedule_loop_exit_frees_in` (kept statement-recursive).
pub(crate) struct LoopExitCtx {
    body: Vec<TirRef>,
    inside_loop: HashSet<TirRef>,
    has_any: HashSet<TirRef>,
    top_level: HashMap<TirRef, TirRef>,
    loop_carried_owners: HashSet<TirRef>,
}

/// Collect every `Assign` (name, value) pair reachable from `stmts`,
/// descending through if/loop bodies. Kept for `LoopExitCtx`.
pub(crate) fn collect_assign_writes(
    tir: &Tir,
    stmts: &[TirRef],
    out: &mut Vec<(StringId, TirRef)>,
) {
    for &r in stmts {
        let r = stmt_match(tir, r).unwrap_or(r);
        match tir.inst(r).tag {
            TirTag::Assign => {
                let view = tir.assign_view(r);
                out.push((view.name, view.value));
            }
            TirTag::IfStmt => {
                let view = tir.if_stmt_view(r);
                collect_assign_writes(tir, &view.then_stmts, out);
                for elif in &view.elif_branches {
                    collect_assign_writes(tir, &elif.body, out);
                }
                if let Some(else_stmts) = &view.else_stmts {
                    collect_assign_writes(tir, else_stmts, out);
                }
            }
            TirTag::WhileLoop => {
                collect_assign_writes(tir, &tir.while_loop_view(r).body, out);
            }
            TirTag::ForRange => {
                collect_assign_writes(tir, &tir.for_range_view(r).body, out);
            }
            TirTag::Match => {
                for body in match_arm_bodies(tir, r) {
                    collect_assign_writes(tir, &body, out);
                }
            }
            _ => {}
        }
        visit_operand_matches(tir, r, &mut |m| {
            for body in match_arm_bodies(tir, m) {
                collect_assign_writes(tir, &body, out);
            }
        });
    }
}

impl LoopExitCtx {
    fn new(tir: &Tir, sidecar: &FunctionSidecar, loop_inst: TirRef) -> Option<Self> {
        let body = tir.loop_body(loop_inst)?;
        let mut inside_loop: HashSet<TirRef> = HashSet::new();
        tir.collect_loop_body_refs(loop_inst, &mut inside_loop);
        let has_any: HashSet<TirRef> = sidecar.drops.iter().map(|fp| fp.target).collect();
        let mut top_level: HashMap<TirRef, TirRef> = HashMap::new();
        for &stmt in &body {
            let mut reachable: HashSet<TirRef> = HashSet::new();
            tir.collect_reachable(stmt, &mut reachable);
            for r in reachable {
                top_level.entry(r).or_insert(stmt);
            }
        }
        let mut writes = Vec::new();
        collect_assign_writes(tir, &body, &mut writes);
        let loop_carried_owners: HashSet<TirRef> = writes
            .into_iter()
            .filter(|(name, _)| declared_before_stmt(tir, *name, loop_inst))
            .map(|(_, value)| value)
            .collect();
        Some(Self {
            body,
            inside_loop,
            has_any,
            top_level,
            loop_carried_owners,
        })
    }
}

/// Schedule unconditional Frees on `break`/`continue` paths. Runs after
/// the last-use, anonymous-temp, and dead-store passes have populated
/// `sidecar.drops`. Kept statement-recursive (see the module header).
pub(crate) fn schedule_loop_exit_frees_in(
    tir: &Tir,
    flow: &ryo_core::flow::Flow,
    own: &Ownership,
    sidecar: &mut FunctionSidecar,
    stmts: &[TirRef],
    enclosing: Option<&LoopExitCtx>,
) {
    for &r in stmts {
        let r = stmt_match(tir, r).unwrap_or(r);
        let inst = *tir.inst(r);
        match inst.tag {
            TirTag::Break | TirTag::Continue => {
                if let Some(ctx) = enclosing {
                    schedule_break_continue_frees(tir, flow, own, sidecar, r, ctx);
                }
            }
            TirTag::WhileLoop | TirTag::ForRange => {
                if let Some(ctx) = LoopExitCtx::new(tir, sidecar, r) {
                    schedule_loop_exit_frees_in(tir, flow, own, sidecar, &ctx.body, Some(&ctx));
                }
            }
            TirTag::IfStmt => {
                let view = tir.if_stmt_view(r);
                schedule_loop_exit_frees_in(tir, flow, own, sidecar, &view.then_stmts, enclosing);
                for elif in &view.elif_branches {
                    schedule_loop_exit_frees_in(tir, flow, own, sidecar, &elif.body, enclosing);
                }
                if let Some(else_stmts) = &view.else_stmts {
                    schedule_loop_exit_frees_in(tir, flow, own, sidecar, else_stmts, enclosing);
                }
            }
            TirTag::Match => {
                for body in match_arm_bodies(tir, r) {
                    schedule_loop_exit_frees_in(tir, flow, own, sidecar, &body, enclosing);
                }
            }
            _ => {}
        }
        if !matches!(inst.tag, TirTag::WhileLoop | TirTag::ForRange) {
            visit_operand_matches(tir, r, &mut |m| {
                for body in match_arm_bodies(tir, m) {
                    schedule_loop_exit_frees_in(tir, flow, own, sidecar, &body, enclosing);
                }
            });
        }
    }
}

/// M12: repair `collect_jump_path`'s over-approximation at a
/// statement-level match. Kept for `schedule_break_continue_frees`.
fn prune_sibling_match_arms(tir: &Tir, stmt: TirRef, jump: TirRef, on_path: &mut HashSet<TirRef>) {
    let stmt_level = stmt_match(tir, stmt);
    if let Some(m) = stmt_level {
        prune_match_arms(tir, m, jump, on_path);
    }
    visit_operand_matches(tir, stmt_level.unwrap_or(stmt), &mut |m| {
        if on_path.contains(&m) {
            prune_match_arms(tir, m, jump, on_path);
        }
    });
}

/// One match's sibling-arm pruning. Kept for
/// `schedule_break_continue_frees`.
fn prune_match_arms(tir: &Tir, m: TirRef, jump: TirRef, on_path: &mut HashSet<TirRef>) {
    if !on_path.contains(&m) {
        return;
    }
    for arm in tir.match_view(m).arms() {
        let roots: Vec<TirRef> = match arm.body {
            MatchBodyView::Expr(e) => vec![e],
            MatchBodyView::Stmts(_) => arm.body.stmts().collect(),
        };
        if roots.iter().any(|&s| tir.contains_reachable(s, jump)) {
            for &s in &roots {
                if tir.contains_reachable(s, jump) {
                    prune_sibling_match_arms(tir, s, jump, on_path);
                }
            }
        } else {
            let mut sub = HashSet::new();
            for &s in &roots {
                tir.collect_reachable(s, &mut sub);
            }
            on_path.retain(|r| !sub.contains(r));
        }
    }
}

/// Schedule one Free per `Valid` owner not already covered by a Free
/// that fires on the jump's path. Kept for `schedule_loop_exit_frees_in`.
pub(crate) fn schedule_break_continue_frees(
    tir: &Tir,
    flow: &ryo_core::flow::Flow,
    own: &Ownership,
    sidecar: &mut FunctionSidecar,
    jump_inst: TirRef,
    ctx: &LoopExitCtx,
) {
    let inside_loop = &ctx.inside_loop;

    let mut on_path: HashSet<TirRef> = HashSet::new();
    match ctx.top_level.get(&jump_inst) {
        Some(&enclosing_stmt) => {
            for &stmt in ctx.body.iter().take_while(|&&s| s != enclosing_stmt) {
                tir.collect_reachable(stmt, &mut on_path);
            }
            let _ = tir.collect_jump_path(
                std::slice::from_ref(&enclosing_stmt),
                jump_inst,
                &mut on_path,
            );
            prune_sibling_match_arms(tir, enclosing_stmt, jump_inst, &mut on_path);
        }
        None => {
            let _ = tir.collect_jump_path(&ctx.body, jump_inst, &mut on_path);
            for &stmt in &ctx.body {
                prune_sibling_match_arms(tir, stmt, jump_inst, &mut on_path);
            }
        }
    }

    let mut covers_this_jump: HashSet<TirRef> = HashSet::new();
    let mut free_inside_loop: HashSet<TirRef> = HashSet::new();
    for fp in anchored_drops(sidecar) {
        if on_path.contains(&drop_anchor(fp)) {
            covers_this_jump.insert(fp.target);
        }
        if inside_loop.contains(&drop_anchor(fp)) {
            free_inside_loop.insert(fp.target);
        }
    }

    let is_break = matches!(tir.inst(jump_inst).tag, TirTag::Break);

    let mut sorted_states: Vec<(Owner, OwnerState)> =
        own.states.iter().map(|(o, s)| (*o, s.clone())).collect();
    sorted_states.sort_by_key(|(o, _)| owner_sort_key(o));
    for (owner, state) in &sorted_states {
        let r = match owner.inst_tirref(flow) {
            Some(r) => r,
            None => continue,
        };
        if !matches!(state, OwnerState::Valid) {
            continue;
        }

        if inside_loop.contains(&r) {
            // The owner's producer must actually run before this jump
            // on the jump's path.
            if !on_path.contains(&r) {
                continue;
            }
            if ctx.loop_carried_owners.contains(&r) {
                continue;
            }
            if covers_this_jump.contains(&r) {
                continue;
            }
            sidecar.drops.push(DropOp {
                position: DropPosition::AfterStep { inst: jump_inst },
                target: r,
                span: tir.span(jump_inst),
            });
            continue;
        }

        if is_break && free_inside_loop.contains(&r) {
            if covers_this_jump.contains(&r) {
                continue;
            }
            sidecar.drops.push(DropOp {
                position: DropPosition::AfterStep { inst: jump_inst },
                target: r,
                span: tir.span(jump_inst),
            });
            continue;
        }

        // Defensive emit only when no Free is scheduled anywhere — and
        // only on break. On `continue` the next iteration would re-read
        // the freed buffer (UAF).
        if is_break && ctx.has_any.contains(&r) {
            continue;
        }
        if is_break {
            sidecar.drops.push(DropOp {
                position: DropPosition::AfterStep { inst: jump_inst },
                target: r,
                span: tir.span(jump_inst),
            });
        }
    }
}

/// Insert into an arg-partition set. These sets are arg-count-sized
/// (usually ≤ 3 entries, very often empty), so a linear-scan Vec beats
/// a HashSet.
pub(crate) fn push_unique(set: &mut Vec<Owner>, owner: Owner) {
    if !set.contains(&owner) {
        set.push(owner);
    }
}
