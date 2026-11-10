//! Post-walk jump-exit Free scheduling — `break`/`continue` cleanup.
//!
//! `schedule_loop_exit_frees_in` keeps statement bodies and enclosing
//! loop contexts on explicit stacks. `LoopExitCtx` remains a
//! sidecar↔TIR relation transform (jump paths, loop-carried
//! classification) pinned by the free-scheduling tests. The scheduling
//! helpers reason about statement-level matches in TIR terms through
//! `stmt_match` / `visit_operand_matches` / `match_arm_bodies`.

use super::super::{
    Dominators, Owner, OwnerState, Ownership, anchored_drops, drop_anchor, match_arm_bodies,
    owner_sort_key, stmt_match, visit_operand_matches,
};
use super::{TirStmtList, declared_before_stmt};
use ryo_core::flow::{DropOp, DropPosition};
use ryo_core::ownership::FunctionSidecar;
use ryo_core::tir::{ExtraList, MatchBodyView, Tir, TirRef, TirTag};
use ryo_core::types::{InternPool, StringId};
use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};

#[cfg(test)]
thread_local! {
    static ACTIVE_LOOP_CONTEXTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static MAX_ACTIVE_LOOP_CONTEXTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn loop_exit_context_counts() -> (usize, usize) {
    (
        ACTIVE_LOOP_CONTEXTS.with(std::cell::Cell::get),
        MAX_ACTIVE_LOOP_CONTEXTS.with(std::cell::Cell::get),
    )
}

// ---------- Post-walk jump-exit free scheduling (kept TIR-based) ----------

/// Per-loop invariants for jump-exit Free scheduling, computed once
/// when the traversal enters a `WhileLoop`/`ForRange` and shared by
/// every `break`/`continue` jump inside the loop. See
/// `schedule_loop_exit_frees_in` (kept TIR-based).
pub(crate) struct LoopExitCtx<'a> {
    body: ExtraList<'a, TirRef>,
    inside_loop: HashSet<TirRef>,
    has_any: HashSet<TirRef>,
    top_level: HashMap<TirRef, TirRef>,
    loop_carried_owners: HashSet<TirRef>,
    reassigned_after_loop_bindings: HashSet<TirRef>,
    sorted_owners: Vec<Owner>,
    /// Computed lazily — only the break defensive-emit path needs
    /// dominance — and shared by every loop in the function.
    doms: &'a OnceCell<Dominators>,
}

/// Collect every `Assign` (site, name, value) reachable from `stmts`,
/// descending through control and operand-match bodies on an explicit stack.
fn collect_assign_writes<S: TirStmtList + ?Sized>(
    tir: &Tir,
    stmts: &S,
    out: &mut Vec<(TirRef, StringId, TirRef)>,
) {
    enum BodyList<'a> {
        Extra(ExtraList<'a, TirRef>),
        Owned(Vec<TirRef>),
    }
    enum Task<'a> {
        Body(BodyList<'a>, usize),
        Stmt(TirRef),
    }
    impl BodyList<'_> {
        fn get(&self, index: usize) -> Option<TirRef> {
            match self {
                Self::Extra(list) => list.get(index),
                Self::Owned(list) => list.get(index).copied(),
            }
        }
    }
    let mut stack = ryo_core::spine::ChainSpine::new();
    for stmt in stmts.iter_refs().collect::<Vec<_>>().into_iter().rev() {
        stack.push(Task::Stmt(stmt));
    }
    while let Some(task) = stack.pop() {
        match task {
            Task::Body(body, index) => {
                let Some(stmt) = body.get(index) else {
                    continue;
                };
                stack.push(Task::Body(body, index + 1));
                stack.push(Task::Stmt(stmt));
            }
            Task::Stmt(stmt) => {
                let r = stmt_match(tir, stmt).unwrap_or(stmt);
                if tir.inst(r).tag == TirTag::Assign {
                    let view = tir.assign_view(r);
                    out.push((r, view.name, view.value));
                }
                let mut bodies = Vec::new();
                match tir.inst(r).tag {
                    TirTag::IfStmt => {
                        let view = tir.if_stmt_view(r);
                        bodies.push(BodyList::Extra(view.then_stmts));
                        bodies.extend(
                            view.elif_branches
                                .iter()
                                .map(|arm| BodyList::Extra(arm.body)),
                        );
                        bodies.extend(view.else_stmts.map(BodyList::Extra));
                    }
                    TirTag::WhileLoop => {
                        bodies.push(BodyList::Extra(tir.while_loop_body_view(r)));
                    }
                    TirTag::ForRange => {
                        bodies.push(BodyList::Extra(tir.for_range_body_view(r)));
                    }
                    TirTag::Match => {
                        bodies.extend(match_arm_bodies(tir, r).into_iter().map(BodyList::Owned));
                    }
                    _ => {}
                }
                visit_operand_matches(tir, r, &mut |m| {
                    bodies.extend(match_arm_bodies(tir, m).into_iter().map(BodyList::Owned));
                });
                for body in bodies.into_iter().rev() {
                    stack.push(Task::Body(body, 0));
                }
            }
        }
    }
}

/// Collect consecutive direct assignments after `loop_inst` in the
/// same statement list. These stores run on every normal path out of
/// the loop, so break cleanup can leave their old home value for the
/// assignment's displacement free.
fn assignments_immediately_after_loop<'tir>(
    tir: &'tir Tir,
    stmts: ExtraList<'tir, TirRef>,
    loop_inst: TirRef,
    out: &mut Vec<TirRef>,
) -> bool {
    enum Task<'a> {
        Body(ExtraList<'a, TirRef>, usize),
        Owned(Vec<TirRef>, usize),
        Stmt(TirRef),
    }
    let mut stack = ryo_core::spine::ChainSpine::new();
    stack.push(Task::Body(stmts, 0));
    while let Some(task) = stack.pop() {
        match task {
            Task::Body(body, index) => {
                let Some(stmt) = body.get(index) else {
                    continue;
                };
                let stmt = stmt_match(tir, stmt).unwrap_or(stmt);
                if stmt == loop_inst {
                    for after in body.iter().skip(index + 1) {
                        let after = stmt_match(tir, after).unwrap_or(after);
                        if tir.inst(after).tag != TirTag::Assign {
                            break;
                        }
                        out.push(after);
                    }
                    return true;
                }
                stack.push(Task::Body(body, index + 1));
                stack.push(Task::Stmt(stmt));
            }
            Task::Owned(body, index) => {
                let Some(&stmt) = body.get(index) else {
                    continue;
                };
                let stmt = stmt_match(tir, stmt).unwrap_or(stmt);
                if stmt == loop_inst {
                    for &after in &body[index + 1..] {
                        let after = stmt_match(tir, after).unwrap_or(after);
                        if tir.inst(after).tag != TirTag::Assign {
                            break;
                        }
                        out.push(after);
                    }
                    return true;
                }
                stack.push(Task::Owned(body, index + 1));
                stack.push(Task::Stmt(stmt));
            }
            Task::Stmt(stmt) => match tir.inst(stmt).tag {
                TirTag::IfStmt => {
                    let view = tir.if_stmt_view(stmt);
                    let mut bodies = Vec::new();
                    bodies.push(view.then_stmts);
                    bodies.extend(view.elif_branches.iter().map(|arm| arm.body));
                    bodies.extend(view.else_stmts);
                    for body in bodies.into_iter().rev() {
                        stack.push(Task::Body(body, 0));
                    }
                }
                TirTag::WhileLoop => {
                    stack.push(Task::Body(tir.while_loop_body_view(stmt), 0));
                }
                TirTag::ForRange => {
                    stack.push(Task::Body(tir.for_range_body_view(stmt), 0));
                }
                TirTag::Match => {
                    for body in match_arm_bodies(tir, stmt).into_iter().rev() {
                        stack.push(Task::Owned(body, 0));
                    }
                }
                _ => {}
            },
        }
    }
    false
}

impl<'a> LoopExitCtx<'a> {
    fn new(
        tir: &'a Tir,
        doms: &'a OnceCell<Dominators>,
        own: &Ownership,
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
        assignments_immediately_after_loop(tir, root_stmts, loop_inst, &mut following_assignments);
        let reassigned_after_loop_bindings = following_assignments
            .into_iter()
            .filter(|site| sidecar.free_on_reassign[site.index()].is_some())
            .filter_map(|site| sidecar.assign_binding[site.index()])
            .collect();
        let mut sorted_owners: Vec<Owner> = own.states.keys().copied().collect();
        sorted_owners.sort_by_key(owner_sort_key);
        Some(Self {
            body,
            inside_loop,
            has_any,
            top_level,
            loop_carried_owners,
            reassigned_after_loop_bindings,
            sorted_owners,
            doms,
        })
    }
}

/// Schedule unconditional Frees on `break`/`continue` paths. Statement
/// bodies and their enclosing loop contexts stay on explicit stacks.
#[allow(clippy::too_many_arguments)]
pub(crate) fn schedule_loop_exit_frees_in<'tir>(
    tir: &'tir Tir,
    flow: &ryo_core::flow::Flow,
    pool: &InternPool,
    own: &Ownership,
    sidecar: &mut FunctionSidecar,
    stmts: ExtraList<'tir, TirRef>,
    doms: &'tir OnceCell<Dominators>,
) {
    enum BodyList<'a> {
        Extra(ExtraList<'a, TirRef>),
        Owned(Vec<TirRef>),
    }
    struct BodyTask<'a> {
        list: BodyList<'a>,
        next: usize,
        enclosing: Option<usize>,
        release_on_complete: Option<usize>,
    }
    impl BodyList<'_> {
        fn get(&self, index: usize) -> Option<TirRef> {
            match self {
                Self::Extra(list) => list.get(index),
                Self::Owned(list) => list.get(index).copied(),
            }
        }
    }
    #[cfg(test)]
    {
        ACTIVE_LOOP_CONTEXTS.with(|count| count.set(0));
        MAX_ACTIVE_LOOP_CONTEXTS.with(|count| count.set(0));
    }
    let mut tasks = ryo_core::spine::ChainSpine::new();
    let mut contexts: Vec<Option<LoopExitCtx<'tir>>> = Vec::new();
    tasks.push(BodyTask {
        list: BodyList::Extra(stmts),
        next: 0,
        enclosing: None,
        release_on_complete: None,
    });
    while let Some(mut task) = tasks.pop() {
        let Some(next) = task.list.get(task.next) else {
            if let Some(id) = task.release_on_complete {
                let released = contexts[id]
                    .take()
                    .expect("completed loop still owns its context");
                drop(released);
                #[cfg(test)]
                ACTIVE_LOOP_CONTEXTS.with(|count| count.set(count.get() - 1));
            }
            continue;
        };
        task.next += 1;
        let enclosing = task.enclosing;
        tasks.push(task);
        let r = stmt_match(tir, next).unwrap_or(next);
        let inst = *tir.inst(r);
        match inst.tag {
            TirTag::Break | TirTag::Continue => {
                if let Some(ctx) = enclosing.and_then(|id| contexts[id].as_ref()) {
                    schedule_break_continue_frees(tir, flow, pool, own, sidecar, r, ctx);
                }
            }
            TirTag::WhileLoop | TirTag::ForRange => {
                if let Some(ctx) = LoopExitCtx::new(tir, doms, own, sidecar, r) {
                    let id = contexts.len();
                    let body = ctx.body;
                    contexts.push(Some(ctx));
                    #[cfg(test)]
                    ACTIVE_LOOP_CONTEXTS.with(|count| {
                        let active = count.get() + 1;
                        count.set(active);
                        MAX_ACTIVE_LOOP_CONTEXTS.with(|max| max.set(max.get().max(active)));
                    });
                    tasks.push(BodyTask {
                        list: BodyList::Extra(body),
                        next: 0,
                        enclosing: Some(id),
                        release_on_complete: Some(id),
                    });
                }
            }
            TirTag::IfStmt => {
                let view = tir.if_stmt_view(r);
                let mut bodies = Vec::new();
                bodies.push(BodyList::Extra(view.then_stmts));
                bodies.extend(
                    view.elif_branches
                        .iter()
                        .map(|arm| BodyList::Extra(arm.body)),
                );
                bodies.extend(view.else_stmts.map(BodyList::Extra));
                for body in bodies.into_iter().rev() {
                    tasks.push(BodyTask {
                        list: body,
                        next: 0,
                        enclosing,
                        release_on_complete: None,
                    });
                }
            }
            TirTag::Match => {
                for body in match_arm_bodies(tir, r).into_iter().rev() {
                    tasks.push(BodyTask {
                        list: BodyList::Owned(body),
                        next: 0,
                        enclosing,
                        release_on_complete: None,
                    });
                }
            }
            _ => {}
        }
        if !matches!(inst.tag, TirTag::WhileLoop | TirTag::ForRange) {
            let mut bodies = Vec::new();
            visit_operand_matches(tir, r, &mut |m| {
                bodies.extend(match_arm_bodies(tir, m));
            });
            for body in bodies.into_iter().rev() {
                tasks.push(BodyTask {
                    list: BodyList::Owned(body),
                    next: 0,
                    enclosing,
                    release_on_complete: None,
                });
            }
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
#[allow(clippy::too_many_arguments)]
fn schedule_break_continue_frees(
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
    let mut covers_this_jump: HashSet<TirRef> = HashSet::new();
    let mut free_inside_loop: HashSet<TirRef> = HashSet::new();
    let mut consumed_scrutinees = HashSet::new();
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

    for fp in anchored_drops(sidecar) {
        if on_path.contains(&drop_anchor(fp)) {
            covers_this_jump.insert(fp.target);
        }
        if inside_loop.contains(&drop_anchor(fp)) {
            free_inside_loop.insert(fp.target);
        }
    }

    let is_break = matches!(tir.inst(jump_inst).tag, TirTag::Break);
    for &candidate in on_path.iter() {
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
    for &owner in &ctx.sorted_owners {
        let r = match owner.inst_tirref(flow) {
            Some(r) => r,
            None => continue,
        };
        if !matches!(own.states.get(&owner), Some(OwnerState::Valid)) {
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
            // break-before-later-loop-reassign crash family).
            let Some(jump_block) = flow.term_block_of(jump_inst) else {
                continue;
            };
            // Terminator-backed owners (match results, loop vars)
            // identify with a terminator payload rather than a step —
            // resolve those through the terminator's block. A match
            // RESULT is different: it is materialized at the match JOIN,
            // after the Switch, so an arm's nested-loop break can be
            // dominated by the Switch yet sit before the result exists
            // on that path — resolve MatchResult producers to the join.
            let producer_block = if flow.match_result(r).is_some() {
                match flow.term_block_of(r).map(|b| *flow.term(b)) {
                    Some(ryo_core::flow::Terminator::Switch {
                        join: Some(join), ..
                    }) => Some(join),
                    _ => None,
                }
            } else {
                flow.step_block_of(r).or_else(|| flow.term_block_of(r))
            };
            let producer_dominates = producer_block.is_some_and(|b| {
                ctx.doms
                    .get_or_init(|| Dominators::new(flow))
                    .dominates(b, jump_block)
            });
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

#[cfg(test)]
mod tests {
    use super::*;
    use ryo_core::span::Span;
    use ryo_core::tir::{Discriminant, TirBuilder, TypedMatchArm, TypedMatchBody};

    #[test]
    fn finds_assignments_after_loop_directly_in_match_arm() {
        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let int_ty = pool.int();
        let bool_ty = pool.bool_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let name = pool.intern_str("s");
        let span = Span::EMPTY;
        let mut tb = TirBuilder::new(main, vec![], void, span);
        let initial = tb.str_const(pool.intern_str("a"), str_ty, span);
        let decl = tb.var_decl(name, true, str_ty, initial, span);
        let cond = tb.bool_const(true, bool_ty, span);
        let loop_inst = tb.while_loop(cond, &[], void, span);
        let value = tb.str_const(pool.intern_str("b"), str_ty, span);
        let assignment = tb.assign(name, str_ty, value, span);
        let scrutinee = tb.int_const(1, int_ty, span);
        let match_inst = tb.match_expr(
            scrutinee,
            void,
            &[TypedMatchArm {
                span,
                discriminant: Discriminant::Wildcard,
                bindings: &[],
                guards: &[],
                body: TypedMatchBody::Stmts(&[loop_inst, assignment]),
            }],
            span,
        );
        let tir = tb.finish(&[decl, match_inst]);
        let mut assignments = Vec::new();

        let found =
            assignments_immediately_after_loop(&tir, tir.body_stmts(), loop_inst, &mut assignments);

        assert!(found, "the loop is directly in the match arm body");
        assert_eq!(assignments, [assignment]);
    }

    #[test]
    fn collect_assign_writes_preserves_body_and_operand_match_order() {
        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let int_ty = pool.int();
        let bool_ty = pool.bool_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let name = pool.intern_str("s");
        let span = Span::EMPTY;
        let mut tb = TirBuilder::new(main, vec![], void, span);
        let initial = tb.str_const(pool.intern_str("a"), str_ty, span);
        let decl = tb.var_decl(name, true, str_ty, initial, span);
        let before_value = tb.str_const(pool.intern_str("before"), str_ty, span);
        let before = tb.assign(name, str_ty, before_value, span);
        let then_value = tb.str_const(pool.intern_str("then"), str_ty, span);
        let then_assign = tb.assign(name, str_ty, then_value, span);
        let else_value = tb.str_const(pool.intern_str("else"), str_ty, span);
        let else_assign = tb.assign(name, str_ty, else_value, span);
        let operand_value = tb.str_const(pool.intern_str("operand"), str_ty, span);
        let operand_assign = tb.assign(name, str_ty, operand_value, span);
        let match_result = tb.bool_const(true, bool_ty, span);
        let scrutinee = tb.int_const(1, int_ty, span);
        let cond = tb.match_expr(
            scrutinee,
            bool_ty,
            &[TypedMatchArm {
                span,
                discriminant: Discriminant::Wildcard,
                bindings: &[],
                guards: &[],
                body: TypedMatchBody::Stmts(&[operand_assign, match_result]),
            }],
            span,
        );
        let if_inst = tb.if_stmt(cond, &[then_assign], &[], Some(&[else_assign]), void, span);
        let tir = tb.finish(&[decl, before, if_inst]);
        let mut writes = Vec::new();

        collect_assign_writes(&tir, &tir.body_stmts(), &mut writes);

        let sites: Vec<TirRef> = writes.into_iter().map(|(site, _, _)| site).collect();
        assert_eq!(sites, [before, then_assign, else_assign, operand_assign]);
    }
}

#[cfg(test)]
mod deep_collection_tests {
    use super::*;
    use ryo_core::span::Span;
    use ryo_core::tir::TirBuilder;
    use std::process::Command;

    const CHILD: &str = "RYO_DEEP_ASSIGN_COLLECTION_CHILD";

    fn collect_deep_assignment() {
        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let bool_ty = pool.bool_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let name = pool.intern_str("s");
        let span = Span::EMPTY;
        let mut tb = TirBuilder::new(main, vec![], void, span);
        let initial = tb.str_const(pool.intern_str("a"), str_ty, span);
        let decl = tb.var_decl(name, true, str_ty, initial, span);
        let value = tb.str_const(pool.intern_str("b"), str_ty, span);
        let assignment = tb.assign(name, str_ty, value, span);
        let mut root = assignment;
        for _ in 0..20_000 {
            let cond = tb.bool_const(true, bool_ty, span);
            root = tb.if_stmt(cond, &[root], &[], None, void, span);
        }
        let loop_cond = tb.bool_const(false, bool_ty, span);
        let loop_inst = tb.while_loop(loop_cond, &[root], void, span);
        let tir = tb.finish(&[decl, loop_inst]);
        let body = tir.while_loop_body_view(loop_inst);
        let mut writes = Vec::new();

        collect_assign_writes(&tir, &body, &mut writes);

        assert_eq!(writes, [(assignment, name, value)]);
    }

    #[test]
    fn deep_nested_control_assignment_collection_uses_bounded_stack() {
        if std::env::var_os(CHILD).is_some() {
            collect_deep_assignment();
            return;
        }
        let output = Command::new(std::env::current_exe().expect("current test executable"))
            .args([
                "ownership::loops::jumps::deep_collection_tests::deep_nested_control_assignment_collection_uses_bounded_stack",
                "--exact",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .expect("spawn deep assignment collection child");
        assert!(
            output.status.success(),
            "deep assignment collection child failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
