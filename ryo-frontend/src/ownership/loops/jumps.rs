//! Post-walk jump-exit Free scheduling — `break`/`continue` cleanup.
//!
//! `schedule_loop_exit_frees_in` and its `LoopExitCtx` stay
//! statement-recursive: they are sidecar↔TIR relation transforms (jump
//! paths, loop-carried classification) pinned by the free-scheduling
//! tests, outside the walk migration's scope (see `mod.rs`). They keep
//! `stmt_match` / `visit_operand_matches` / `match_arm_bodies` alive
//! (the scheduling helpers still reason about statement-level matches
//! in TIR terms, outside the Flow-walked body).

use super::super::{
    Owner, OwnerState, Ownership, anchored_drops, drop_anchor, match_arm_bodies, owner_sort_key,
    stmt_match, visit_operand_matches,
};
use super::{TirStmtList, declared_before_stmt};
use ryo_core::flow::{BlockId, DropOp, DropPosition, Flow};
use ryo_core::ownership::FunctionSidecar;
use ryo_core::tir::{ExtraList, MatchBodyView, Tir, TirRef, TirTag};
use ryo_core::types::{InternPool, StringId};
use std::collections::{HashMap, HashSet};

/// Iterative dominator sets over the Flow CFG, dense per block id.
/// Function graphs are small (tens of blocks), so the textbook
/// set-intersection fixpoint is the cheapest correct choice.
fn compute_dominators(flow: &Flow) -> Vec<HashSet<BlockId>> {
    let order: Vec<BlockId> = flow.forward_order().collect();
    // The dense `doms[b.0]` indexing below relies on forward_order
    // yielding EVERY block (unreachable ones included) in id order
    // (Flow::forward_order is `(0..blocks.len()).map(BlockId)` today).
    // Pin that contract: if it ever changes to skip unreachable blocks
    // or yield a propagation order, this must become BlockId-keyed.
    debug_assert!(
        order.iter().enumerate().all(|(i, &b)| b.0 as usize == i),
        "compute_dominators requires forward_order to yield all blocks in id order"
    );
    let all: HashSet<BlockId> = order.iter().copied().collect();
    let mut doms: Vec<HashSet<BlockId>> = order
        .iter()
        .map(|&b| {
            if b == flow.entry {
                HashSet::from([b])
            } else {
                all.clone()
            }
        })
        .collect();
    let mut changed = true;
    while changed {
        changed = false;
        for &b in &order {
            if b == flow.entry {
                continue;
            }
            let preds = flow.predecessors(b);
            let mut new_dom = HashSet::from([b]);
            if let Some((&first, rest)) = preds.split_first() {
                let mut intersect = doms[first.0 as usize].clone();
                for &p in rest {
                    intersect.retain(|blk| doms[p.0 as usize].contains(blk));
                }
                new_dom.extend(intersect);
            }
            if new_dom != doms[b.0 as usize] {
                doms[b.0 as usize] = new_dom;
                changed = true;
            }
        }
    }
    doms
}

/// `def` dominates `use_` when every path from the entry to `use_`
/// passes through `def`.
fn dominates(doms: &[HashSet<BlockId>], def: BlockId, use_: BlockId) -> bool {
    def == use_ || doms[use_.0 as usize].contains(&def)
}

// ---------- Post-walk jump-exit free scheduling (kept TIR-based) ----------

/// Per-loop invariants for jump-exit Free scheduling, computed once
/// when the traversal enters a `WhileLoop`/`ForRange` and shared by
/// every `break`/`continue` jump inside the loop. See
/// `schedule_loop_exit_frees_in` (kept statement-recursive).
pub(crate) struct LoopExitCtx<'a> {
    body: ExtraList<'a, TirRef>,
    inside_loop: HashSet<TirRef>,
    has_any: HashSet<TirRef>,
    top_level: HashMap<TirRef, TirRef>,
    loop_carried_owners: HashSet<TirRef>,
    reassigned_after_loop_bindings: HashSet<TirRef>,
    /// Dominator sets for the owning function's Flow CFG, shared by
    /// every jump in this loop (see `compute_dominators`).
    doms: Vec<HashSet<BlockId>>,
}

/// Collect every `Assign` (site, name, value) reachable from `stmts`,
/// descending through if/loop bodies. Kept for `LoopExitCtx`.
fn collect_assign_writes<S: TirStmtList + ?Sized>(
    tir: &Tir,
    stmts: &S,
    out: &mut Vec<(TirRef, StringId, TirRef)>,
) {
    for r in stmts.iter_refs() {
        let r = stmt_match(tir, r).unwrap_or(r);
        match tir.inst(r).tag {
            TirTag::Assign => {
                let view = tir.assign_view(r);
                out.push((r, view.name, view.value));
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
                let body = tir.while_loop_body_view(r);
                collect_assign_writes(tir, &body, out);
            }
            TirTag::ForRange => {
                let body = tir.for_range_body_view(r);
                collect_assign_writes(tir, &body, out);
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

/// Collect consecutive direct assignments after `loop_inst` in the
/// same statement list. These stores run on every normal path out of
/// the loop, so break cleanup can leave their old home value for the
/// assignment's displacement free.
fn assignments_immediately_after_loop<S: TirStmtList + ?Sized>(
    tir: &Tir,
    stmts: &S,
    loop_inst: TirRef,
    out: &mut Vec<TirRef>,
) -> bool {
    for (index, stmt) in stmts.iter_refs().enumerate() {
        let stmt = stmt_match(tir, stmt).unwrap_or(stmt);
        if stmt == loop_inst {
            for after in stmts.iter_refs().skip(index + 1) {
                let after = stmt_match(tir, after).unwrap_or(after);
                if tir.inst(after).tag != TirTag::Assign {
                    break;
                }
                out.push(after);
            }
            return true;
        }
        let found = match tir.inst(stmt).tag {
            TirTag::IfStmt => {
                let view = tir.if_stmt_view(stmt);
                assignments_immediately_after_loop(tir, &view.then_stmts, loop_inst, out)
                    || view.elif_branches.iter().any(|arm| {
                        assignments_immediately_after_loop(tir, &arm.body, loop_inst, out)
                    })
                    || view.else_stmts.as_ref().is_some_and(|body| {
                        assignments_immediately_after_loop(tir, body, loop_inst, out)
                    })
            }
            TirTag::WhileLoop => assignments_immediately_after_loop(
                tir,
                &tir.while_loop_body_view(stmt),
                loop_inst,
                out,
            ),
            TirTag::ForRange => assignments_immediately_after_loop(
                tir,
                &tir.for_range_body_view(stmt),
                loop_inst,
                out,
            ),
            TirTag::Match => match_arm_bodies(tir, stmt)
                .into_iter()
                .any(|body| assignments_immediately_after_loop(tir, &body, loop_inst, out)),
            _ => false,
        };
        if found {
            return true;
        }
    }
    false
}

impl<'a> LoopExitCtx<'a> {
    fn new(
        tir: &'a Tir,
        flow: &Flow,
        sidecar: &FunctionSidecar,
        loop_inst: TirRef,
    ) -> Option<Self> {
        let body = tir.loop_body_view(loop_inst)?;
        let mut inside_loop: HashSet<TirRef> = HashSet::new();
        tir.collect_loop_body_refs(loop_inst, &mut inside_loop);
        let has_any: HashSet<TirRef> = sidecar.drops.iter().map(|fp| fp.target).collect();
        let mut top_level: HashMap<TirRef, TirRef> = HashMap::new();
        for stmt in body {
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
            .filter(|(_, name, _)| declared_before_stmt(tir, *name, loop_inst))
            .map(|(_, _, value)| value)
            .collect();
        let mut following_assignments = Vec::new();
        let root_stmts = tir.body_stmts();
        assignments_immediately_after_loop(tir, &root_stmts, loop_inst, &mut following_assignments);
        let reassigned_after_loop_bindings = following_assignments
            .into_iter()
            .filter(|site| sidecar.free_on_reassign[site.index()].is_some())
            .filter_map(|site| sidecar.assign_binding[site.index()])
            .collect();
        Some(Self {
            body,
            inside_loop,
            has_any,
            top_level,
            loop_carried_owners,
            reassigned_after_loop_bindings,
            doms: compute_dominators(flow),
        })
    }
}

/// Schedule unconditional Frees on `break`/`continue` paths. Runs after
/// the last-use, anonymous-temp, and dead-store passes have populated
/// `sidecar.drops`. Kept statement-recursive (see the module header).
pub(crate) fn schedule_loop_exit_frees_in<S: TirStmtList + ?Sized>(
    tir: &Tir,
    flow: &ryo_core::flow::Flow,
    pool: &InternPool,
    own: &Ownership,
    sidecar: &mut FunctionSidecar,
    stmts: &S,
    enclosing: Option<&LoopExitCtx<'_>>,
) {
    for r in stmts.iter_refs() {
        let r = stmt_match(tir, r).unwrap_or(r);
        let inst = *tir.inst(r);
        match inst.tag {
            TirTag::Break | TirTag::Continue => {
                if let Some(ctx) = enclosing {
                    schedule_break_continue_frees(tir, flow, pool, own, sidecar, r, ctx);
                }
            }
            TirTag::WhileLoop | TirTag::ForRange => {
                if let Some(ctx) = LoopExitCtx::new(tir, flow, sidecar, r) {
                    schedule_loop_exit_frees_in(
                        tir,
                        flow,
                        pool,
                        own,
                        sidecar,
                        &ctx.body,
                        Some(&ctx),
                    );
                }
            }
            TirTag::IfStmt => {
                let view = tir.if_stmt_view(r);
                schedule_loop_exit_frees_in(
                    tir,
                    flow,
                    pool,
                    own,
                    sidecar,
                    &view.then_stmts,
                    enclosing,
                );
                for elif in &view.elif_branches {
                    schedule_loop_exit_frees_in(
                        tir, flow, pool, own, sidecar, &elif.body, enclosing,
                    );
                }
                if let Some(else_stmts) = &view.else_stmts {
                    schedule_loop_exit_frees_in(
                        tir, flow, pool, own, sidecar, else_stmts, enclosing,
                    );
                }
            }
            TirTag::Match => {
                for body in match_arm_bodies(tir, r) {
                    schedule_loop_exit_frees_in(tir, flow, pool, own, sidecar, &body, enclosing);
                }
            }
            _ => {}
        }
        if !matches!(inst.tag, TirTag::WhileLoop | TirTag::ForRange) {
            visit_operand_matches(tir, r, &mut |m| {
                for body in match_arm_bodies(tir, m) {
                    schedule_loop_exit_frees_in(tir, flow, pool, own, sidecar, &body, enclosing);
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
    pool: &InternPool,
    own: &Ownership,
    sidecar: &mut FunctionSidecar,
    jump_inst: TirRef,
    ctx: &LoopExitCtx<'_>,
) {
    let inside_loop = &ctx.inside_loop;

    let mut on_path: HashSet<TirRef> = HashSet::new();
    match ctx.top_level.get(&jump_inst) {
        Some(&enclosing_stmt) => {
            for stmt in ctx.body.iter().take_while(|&s| s != enclosing_stmt) {
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
            let _ = tir.collect_jump_path_iter(ctx.body, jump_inst, &mut on_path);
            for stmt in ctx.body.iter() {
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
    let mut consumed_scrutinees = HashSet::new();
    for &candidate in &on_path {
        if tir.inst(candidate).tag != TirTag::Match {
            continue;
        }
        let view = tir.match_view(candidate);
        let selected_arm = view.arms().find(|arm| match arm.body {
            MatchBodyView::Expr(expr) => tir.contains_reachable(expr, jump_inst),
            MatchBodyView::Stmts(_) => arm
                .body
                .stmts()
                .any(|stmt| tir.contains_reachable(stmt, jump_inst)),
        });
        let consumes_scrutinee =
            selected_arm.is_some_and(|arm| arm.bindings().any(|binding| !pool.is_copy(binding.ty)));
        if consumes_scrutinee
            && let Some(owner) = own.underlying_owner(view.scrutinee).inst_tirref(flow)
        {
            consumed_scrutinees.insert(owner);
        }
    }
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

        // The selected arm transfers the payload to its by-move binding.
        // Do not release the source slot again at the loop exit.
        if consumed_scrutinees.contains(&r) {
            continue;
        }

        if is_break && ctx.reassigned_after_loop_bindings.contains(&r) {
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
            // The Free fires where its anchor fires: the producer must
            // dominate the jump's block, i.e. have run on every path
            // reaching this break. Owners produced after the loop — or
            // in a branch that can bypass them — do not dominate it;
            // anchoring their Free here releases storage before the
            // value exists (validator: path double discharge) and the
            // bogus target poisons CleanupPlan's Binding/Value redirect
            // for the owners it actually covers (the
            // bug_reports/bug_str_concat_after_break.ryo family).
            let Some(jump_block) = flow.term_block_of(jump_inst) else {
                continue;
            };
            // Match-result and loop-var owners identify with terminator
            // payloads (the match / loop instruction) rather than steps —
            // resolve those through the terminator's block so a
            // dominating producer of that class still qualifies.
            let producer_dominates = flow
                .step_block_of(r)
                .or_else(|| flow.term_block_of(r))
                .is_some_and(|b| dominates(&ctx.doms, b, jump_block));
            if !producer_dominates {
                continue;
            }
            sidecar.drops.push(DropOp {
                position: DropPosition::AfterStep { inst: jump_inst },
                target: r,
                span: tir.span(jump_inst),
            });
        }
    }
}
