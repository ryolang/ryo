//! Loop/branch ownership analysis — split from `mod.rs`.

use super::{
    Owner, OwnerState, Ownership, analyze_stmt, merge_non_monotone, owner_sort_key,
    remove_loop_deferred_views, states_differ_snapshot, visit_expr,
};
use ryo_core::diag::DiagSink;
use ryo_core::ownership::{FreePoint, FunctionSidecar};
use ryo_core::tir::{Tir, TirData, TirRef, TirTag};
use ryo_core::types::{InternPool, StringId};
use std::collections::{HashMap, HashSet};

/// Outermost loop statement (`WhileLoop`/`ForRange`) whose body
/// contains `target`, or `None` when `target` is not inside any loop.
/// Walks the body's statements tracking the loop stack; if/elif/else
/// arms are transparent (an if inside a loop does not reset the stack).
/// Used by the dead-store re-anchor.
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
                _ => {}
            }
        }
        None
    }
    walk(tir, &tir.body_stmts(), target, &mut Vec::new())
}

/// True if `name` is declared by a `VarDecl` anywhere before `stop` in
/// program order (any nesting depth). Guards the dead-store re-anchor: only
/// bindings that exist BEFORE their outermost reseating loop may move
/// their Free to the loop anchor — a loop-local value's `FatLocals`
/// don't exist on the zero-iteration path.
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
                _ => {}
            }
        }
    }
    let mut found = false;
    let mut stopped = false;
    walk(tir, &tir.body_stmts(), name, stop, &mut found, &mut stopped);
    found
}

/// True if any statement in `stmts` (any depth) is a `Return` /
/// `ReturnVoid` — a body that may leave the function early, where an
/// after-loop Free would be unreachable on the return path.
pub(crate) fn body_may_return(tir: &Tir, stmts: &[TirRef]) -> bool {
    for &r in stmts {
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
            _ => {}
        }
    }
    false
}

/// True when any statement in `stmts` is a `Break`/`Continue` whose
/// target is the loop ENCLOSING `stmts` — i.e. the list may jump out
/// instead of falling through. Recurses into `if` arms (including an
/// `ExprStmt`-wrapped if, sema's `assert` desugaring) but NOT into
/// nested loops: their breaks/continues target the inner loop and
/// don't leave the arm.
pub(crate) fn body_may_jump_out(tir: &Tir, stmts: &[TirRef]) -> bool {
    for &r in stmts {
        match tir.inst(r).tag {
            TirTag::Break | TirTag::Continue => return true,
            TirTag::IfStmt => {
                let view = tir.if_stmt_view(r);
                if body_may_jump_out(tir, &view.then_stmts)
                    || view
                        .elif_branches
                        .iter()
                        .any(|e| body_may_jump_out(tir, &e.body))
                    || view
                        .else_stmts
                        .as_ref()
                        .is_some_and(|es| body_may_jump_out(tir, es))
                {
                    return true;
                }
            }
            _ => {
                if let TirData::UnOp(o) = tir.inst(r).data
                    && tir.inst(o).tag == TirTag::IfStmt
                    && body_may_jump_out(tir, std::slice::from_ref(&o))
                {
                    return true;
                }
            }
        }
    }
    false
}

/// True when at least one path through the if reaches its merge block:
/// an else-less if always has the fall-through path; with an else,
/// some arm's body must neither return nor jump out of the enclosing
/// loop. A Free anchored after an if whose merge block is unreachable
/// would never fire.
pub(crate) fn if_may_fall_through(tir: &Tir, if_stmt: TirRef) -> bool {
    let view = tir.if_stmt_view(if_stmt);
    if view.else_stmts.is_none() {
        return true;
    }
    let arm_falls_through =
        |body: &[TirRef]| !body_may_return(tir, body) && !body_may_jump_out(tir, body);
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
/// `None`. Descends through arm bodies, loop bodies, and `ExprStmt`
/// wrappers. Elif conditions are deliberately excluded: they are
/// evaluated only on the paths that reach them, so a temp produced
/// there does not exist on every exit path and cannot be freed at the
/// merge block. Used by the anonymous-temporary Free pass.
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
                    // In an elif condition: not eligible (see doc).
                    return None;
                }
                TirTag::WhileLoop | TirTag::ForRange => {
                    if let Some(body) = tir.loop_body(r)
                        && let Some(found) = walk(tir, &body, target)
                    {
                        return Some(found);
                    }
                    // In the loop's condition/bounds: not an if cond.
                    return None;
                }
                _ => {
                    // Transparent wrapper: an `ExprStmt` around a branch
                    // (sema's `assert` desugars to `ExprStmt(IfStmt)`).
                    if let TirData::UnOp(o) = tir.inst(r).data
                        && matches!(
                            tir.inst(o).tag,
                            TirTag::IfStmt | TirTag::WhileLoop | TirTag::ForRange
                        )
                    {
                        return walk(tir, std::slice::from_ref(&o), target);
                    }
                    // A plain statement's subtree contains `target` —
                    // not an if condition.
                    return None;
                }
            }
        }
        None
    }
    walk(tir, &tir.body_stmts(), target)
}

/// True when the branch (`IfStmt`/`WhileLoop`/`ForRange`) contains no
/// `Return`/`ReturnVoid` on any path — its exit anchor is reachable on
/// every path, so a conditional-last-use Free can safely move there.
pub(crate) fn branch_may_not_return(tir: &Tir, branch_stmt: TirRef) -> bool {
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
        _ => false,
    }
}
/// Outermost branch statement (`IfStmt`/`WhileLoop`/`ForRange`) whose
/// arm, body, or condition/bounds contains `target`, or `None` when
/// `target` is not inside any branch. `target` may be a sub-expression
/// (e.g. a `Var` read inside a call or in a branch condition), so
/// statements are matched by subtree, not by reference equality. Used
/// by the conditional-last-use re-anchor.
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
            // `target` is inside this statement's subtree. Descend
            // through branches; a plain statement means the enclosing
            // branch (if any) is the answer.
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
                    // Not in any arm body, but the if's subtree contains
                    // `target` (the caller's `contains_reachable` guard):
                    // it sits in the if/elif CONDITION. Conditions run on
                    // every path through the branch, so the branch still
                    // contains the read — without this, a last use in a
                    // condition keeps its raw anchor and its Free fires at
                    // the first statement end inside an arm (mid-loop UAF
                    // when the whole shape sits in a loop body).
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
                    // Same condition case as the if arm: a read in the
                    // loop's condition or bounds is inside the loop.
                    if found.is_none() {
                        found = stack.first().copied();
                    }
                    stack.pop();
                    if found.is_some() {
                        return found;
                    }
                }
                // Transparent wrapper: an `ExprStmt` around a branch
                // (sema's `assert` desugars to `ExprStmt(IfStmt)`) must
                // not hide the branch from the walk — descend into the
                // operand with the branch stack unchanged.
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
                    return stack.first().copied();
                }
            }
        }
        None
    }
    walk(tir, &tir.body_stmts(), target, &mut Vec::new())
}

/// All branch statements (`IfStmt`/`WhileLoop`/`ForRange`) containing
/// `target`, outermost first. A Free anchored after any of these never
/// fires on a return path that exits through `target` — the branch
/// statement does not complete before the return leaves the function.
/// Used by the return-epilogue dedup.
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
                _ => return Some(stack.clone()),
            }
        }
        None
    }
    walk(tir, &tir.body_stmts(), target, &mut Vec::new()).unwrap_or_default()
}

/// The binding name whose initializer (`VarDecl`) or value (`Assign`)
/// produced `owner`, or `None` for anonymous producers. The
/// ownership-side view of codegen's `free_binding_names` map.
pub(crate) fn owner_binding_name(tir: &Tir, owner: TirRef) -> Option<StringId> {
    fn walk(tir: &Tir, stmts: &[TirRef], owner: TirRef) -> Option<StringId> {
        for &r in stmts {
            match tir.inst(r).tag {
                TirTag::VarDecl => {
                    let view = tir.var_decl_view(r);
                    if view.initializer == owner {
                        return Some(view.name);
                    }
                }
                TirTag::Assign => {
                    let view = tir.assign_view(r);
                    if view.value == owner {
                        return Some(view.name);
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
                _ => {}
            }
        }
        None
    }
    walk(tir, &tir.body_stmts(), owner)
}

// ---------- M8.4: slice projections (final spec §3.2/§3.3) ----------

/// Per-instruction loop-nesting as parent-pointer chains instead of
/// per-instruction stack copies: each instruction stores only its
/// INNERMOST enclosing `WhileLoop`/`ForRange` and its enclosing-loop
/// count; the full ancestor stack is recovered by walking parent hops
/// (a loop instruction's `innermost` slot is its own enclosing loop).
/// Dense tables indexed by `TirRef::index()`, sized to
/// `tir.instructions.len()` (slot 0, the reserved sentinel, stays
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
    /// Number of loops enclosing `r` (0 at top level). The tables
    /// cover every instruction reachable from the body — all callers
    /// query body instructions (views, reads, materialize calls,
    /// hazard sites) — and a ref outside the body nests in no loop
    /// anyway, so a missing entry reads as depth 0, exactly what a
    /// fresh body walk would find.
    pub(crate) fn depth_of(&self, r: TirRef) -> u32 {
        self.depth.get(r.index()).copied().unwrap_or(0)
    }

    /// `r`'s enclosing loops, innermost first, by parent hops: for a
    /// loop instruction `l`, `innermost[l]` IS `l`'s enclosing loop,
    /// so each hop moves one level out.
    pub(crate) fn ancestors_innermost_first(&self, r: TirRef) -> impl Iterator<Item = TirRef> + '_ {
        let mut cur = self.innermost.get(r.index()).copied().flatten();
        std::iter::from_fn(move || {
            let l = cur?;
            cur = self.innermost[l.index()];
            Some(l)
        })
    }

    /// The enclosing loop whose OWN depth is `d` — i.e. the loop at
    /// position `d` of the old outermost-first stack (`d` <
    /// `depth_of(r)` must hold). Innermost-first, that ancestor is
    /// `depth_of(r) - d - 1` parent hops away. The debug_asserts pin
    /// the prefix invariant the old stack-slice indexing relied on.
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

/// Compute every instruction's loop nesting — the
/// `WhileLoop`/`ForRange` instructions whose bodies (or conditions,
/// which re-evaluate per iteration) contain it — in a single
/// traversal, as [`LoopNesting`] parent-pointer chains. Replaces
/// per-query body walks: the nesting is walk-constant, so the
/// liveness passes and the redundant-materialize pass look it up
/// instead (P4, final spec §3.2).
///
/// A loop's whole subtree (condition, bounds, body) counts as inside
/// the loop; an if's subtree keeps the if's own nesting. Recording a
/// loop's full subtree before recursing into its body lets nested
/// loops overwrite their own subtrees with deeper chains — the TIR is
/// tree-shaped, so each instruction's final entry is the one written
/// by its unique outermost-in path.
///
/// Two scalar writes per instruction (no per-instruction heap
/// clones). One scratch `HashSet` is reused (cleared) across
/// statements instead of allocating a fresh subtree set per
/// statement.
pub(crate) fn collect_loop_nesting(tir: &Tir) -> LoopNesting {
    fn walk(
        tir: &Tir,
        stmts: &[TirRef],
        innermost: Option<TirRef>,
        depth: u32,
        nesting: &mut LoopNesting,
        sub: &mut HashSet<TirRef>,
    ) {
        for &r in stmts {
            sub.clear();
            tir.collect_reachable(r, sub);
            match tir.inst(r).tag {
                TirTag::WhileLoop | TirTag::ForRange => {
                    // Everything the loop evaluates re-executes per
                    // iteration, so it all counts as inside the loop.
                    sub.remove(&r);
                    for &x in sub.iter() {
                        nesting.innermost[x.index()] = Some(r);
                        nesting.depth[x.index()] = depth + 1;
                    }
                    // The loop instruction itself sits at the
                    // enclosing nesting.
                    nesting.innermost[r.index()] = innermost;
                    nesting.depth[r.index()] = depth;
                    if let Some(body) = tir.loop_body(r) {
                        walk(tir, &body, Some(r), depth + 1, nesting, sub);
                    }
                }
                _ => {
                    // Plain statements and ifs (condition included)
                    // keep the enclosing nesting.
                    for &x in sub.iter() {
                        nesting.innermost[x.index()] = innermost;
                        nesting.depth[x.index()] = depth;
                    }
                    if tir.inst(r).tag == TirTag::IfStmt {
                        let view = tir.if_stmt_view(r);
                        walk(tir, &view.then_stmts, innermost, depth, nesting, sub);
                        for elif in &view.elif_branches {
                            walk(tir, &elif.body, innermost, depth, nesting, sub);
                        }
                        if let Some(else_stmts) = &view.else_stmts {
                            walk(tir, else_stmts, innermost, depth, nesting, sub);
                        }
                    }
                }
            }
        }
    }
    let mut nesting = LoopNesting {
        innermost: vec![None; tir.instructions.len()],
        depth: vec![0; tir.instructions.len()],
    };
    let mut sub = HashSet::new();
    walk(tir, &tir.body_stmts(), None, 0, &mut nesting, &mut sub);
    nesting
}

/// Shared loop-body fixed-point, in two phases. Caller has already
/// visited the prelude (cond / start+end).
///
/// Phase 1 — propagate-only propagation, bounded to two walks (the
/// bound is load-bearing — see the comment in the body). Walks the
/// body with a throwaway `DiagSink` AND a staging sidecar (a clone of
/// the real one), so speculative diagnostics and sidecar writes are
/// discarded at the end of each iteration. After each walk, compares
/// entry vs post-body via `states_differ_snapshot` (the full tuple —
/// owner states plus live-projection emptiness) and always merges
/// (entry ⊔ post-body) into `own` via `merge_non_monotone`. Converged
/// → break early; otherwise the merged state becomes the new entry
/// and the loop iterates up to the cap.
///
/// Phase 2 — single check pass. From the propagated entry
/// state, walks the body exactly once against the REAL sink and
/// sidecar, then does the final merge with the loop-entry snapshot
/// (the loop may execute zero times), so post-loop state =
/// entry ⊔ post-check-pass. Diagnostics are therefore always derived
/// from the propagated lattice, never from whichever speculative
/// iteration happened to emit them.
///
/// Monotone `Ownership` fields (`temp_owners`, `owner_at_read`,
/// `root_owner`, `reseat_drops`, `return_epilogue`, `owner_hazards`,
/// `origin`) keep accumulating across propagate passes and are deduped
/// at scheduling time (documented on those fields) — they are
/// deliberately not rolled back. `next_branch_id` likewise stays
/// monotone (never snapshotted/restored): propagate passes may leave
/// gaps in BranchId numbering, which is harmless because ids only
/// need function-uniqueness (see `merge_branches`' BranchId
/// monotonicity note and `merge_branches_takes_max_next_branch_id`).
pub(crate) fn analyze_loop_body(
    tir: &Tir,
    pool: &InternPool,
    own: &mut Ownership,
    sink: &mut DiagSink,
    sidecar: &mut FunctionSidecar,
    body: &[TirRef],
) {
    // Snapshot ONLY the non-monotone fields (see Step 2).
    // `live_projections` joined that set in M8.4 (projections die at
    // their last use); `root_owner` is insert-only and stays live.
    // The loop-entry snapshot is kept for the final merge in Phase 2.
    let snap_states = own.states.clone();
    let snap_current_owner = own.current_owner.clone();
    let snap_pending_dead_store = own.pending_dead_store.clone();
    let snap_live_projections = own.live_projections.clone();

    // Phase 1 — propagate-only fixed-point, bounded to
    // MAX_PROPAGATE_PASSES walks. The bound is load-bearing, not just
    // pragmatic: the binding-aware override (`merge_binding_states`,
    // called by `merge_non_monotone`) is NOT monotone — when the body
    // reseats a binding (consume-then-rebind), the override merges the
    // pre-reseat owner's entry state with the post-reseat owner's
    // post-body state, which can flip `Moved` back to `Valid` on every
    // merge. An unbounded loop then oscillates forever (`while:
    // consume(name); name = "Bob"` never converges).
    // Two walks reproduce the historical 2-pass precision: genuinely
    // divergent bodies (move-without-rebind) converge by the second
    // walk's comparison and break early; oscillating bodies stop at
    // the cap with the same merged state the old re-walk started from.
    const MAX_PROPAGATE_PASSES: usize = 2;
    let mut entry_states = snap_states.clone();
    let mut entry_current_owner = snap_current_owner.clone();
    let mut entry_pending_dead_store = snap_pending_dead_store.clone();
    let mut entry_live_projections = snap_live_projections.clone();
    for _ in 0..MAX_PROPAGATE_PASSES {
        let mut scratch = DiagSink::new();
        let mut staging = sidecar.clone();
        for stmt in body {
            analyze_stmt(tir, pool, own, &mut scratch, &mut staging, *stmt);
        }
        let after = (
            own.states.clone(),
            own.current_owner.clone(),
            own.pending_dead_store.clone(),
            own.live_projections.clone(),
        );
        let differ =
            states_differ_snapshot(&entry_states, &after.0, &entry_live_projections, &after.3);
        // Always merge (entry ⊔ post-body) into `own`; on convergence
        // post-body == entry so the merge is a no-op and `own` already
        // holds the converged entry state Phase 2 starts from.
        merge_non_monotone(
            own,
            &entry_states,
            &after.0,
            &entry_current_owner,
            &after.1,
            &entry_pending_dead_store,
            &after.2,
            &entry_live_projections,
            &after.3,
        );
        if !differ {
            break;
        }
        entry_states = own.states.clone();
        entry_current_owner = own.current_owner.clone();
        entry_pending_dead_store = own.pending_dead_store.clone();
        entry_live_projections = own.live_projections.clone();
    }

    // Phase 2 — single check pass against the real sink and sidecar.
    for stmt in body {
        analyze_stmt(tir, pool, own, sink, sidecar, *stmt);
    }
    // Final merge with the loop-entry snapshot: the loop may execute
    // zero times, so post-loop state = entry ⊔ post-check-pass.
    let own_states_clone = own.states.clone();
    let own_current_owner_clone = own.current_owner.clone();
    let own_pending_dead_store_clone = own.pending_dead_store.clone();
    let own_live_projections_clone = own.live_projections.clone();
    merge_non_monotone(
        own,
        &snap_states,
        &own_states_clone,
        &snap_current_owner,
        &own_current_owner_clone,
        &snap_pending_dead_store,
        &own_pending_dead_store_clone,
        &snap_live_projections,
        &own_live_projections_clone,
    );
}

/// Fixed-point ownership analysis for `while`, in the
/// propagate-then-check shape of `analyze_loop_body`: Phase 1 walks
/// the body against a throwaway sink and a staging sidecar, merging
/// (entry ⊔ post-body) until the full state tuple (owner states +
/// live-projection emptiness) is unchanged across the back-edge;
/// Phase 2 then walks the body exactly once from the converged entry
/// state against the real sink and sidecar — a binding moved inside
/// the body without rebinding before the back-edge surfaces as E0020
/// on that check pass, because the converged entry already records
/// the move.
///
/// Why two propagate walks suffice for the M8.1 pattern set: the
/// state merge is monotonic over the Moved-ness sub-lattice
/// (`Valid → Moved` is the only transition, and merge takes "any
/// branch Moved → Moved") and the projection merge is a plain union
/// (also monotone) — so a TirRef that flips from `Valid` to `Moved`
/// in one pass stays `Moved` after the (entry ⊔ post-body) merge and
/// a further propagate pass observes nothing new. Original phrasing
/// for traceability: "Converges in at most 2 iterations for the M8.1
/// pattern set".
///
/// **Maintainer note.** The merge is NOT fully monotone: the
/// binding-aware override in `merge_binding_states` (used by
/// `merge_non_monotone`) can flip `Moved` back to `Valid` when a body
/// reseats its binding (consume-then-rebind), so an UNBOUNDED
/// propagate loop oscillates forever on that pattern. The two-walk
/// cap is what keeps Phase 1 total; do not replace it with an
/// unbounded `loop` unless the override's monotonicity is addressed
/// first.
pub(crate) fn analyze_while_loop(
    tir: &Tir,
    pool: &InternPool,
    own: &mut Ownership,
    sink: &mut DiagSink,
    sidecar: &mut FunctionSidecar,
    r: TirRef,
) {
    let view = tir.while_loop_view(r);
    visit_expr(tir, pool, own, sink, sidecar, view.cond);
    analyze_loop_body(tir, pool, own, sink, sidecar, &view.body);
    // P4 (final spec §3.2): projections whose last read is inside this
    // loop (but whose creation is outside it) die at the loop's exit.
    remove_loop_deferred_views(own, r);
}

/// `for i in range(start, end)` loop var is `int` (Copy), so the
/// induction variable never enters the lattice. The body runs the
/// same propagate-then-check fixed-point as `while`.
pub(crate) fn analyze_for_range(
    tir: &Tir,
    pool: &InternPool,
    own: &mut Ownership,
    sink: &mut DiagSink,
    sidecar: &mut FunctionSidecar,
    r: TirRef,
) {
    let view = tir.for_range_view(r);
    // Start/end are visited unconditionally — they're plain `int`
    // exprs, so they don't move anything, but they may contain nested
    // reads we want to record.
    visit_expr(tir, pool, own, sink, sidecar, view.start);
    visit_expr(tir, pool, own, sink, sidecar, view.end);
    analyze_loop_body(tir, pool, own, sink, sidecar, &view.body);
    // P4 (final spec §3.2): projections whose last read is inside this
    // loop (but whose creation is outside it) die at the loop's exit.
    remove_loop_deferred_views(own, r);
}

/// Per-loop invariants for jump-exit Free scheduling, computed once
/// when the traversal enters a `WhileLoop`/`ForRange` and shared by
/// every `break`/`continue` jump inside the loop:
///
/// * `body` — the loop's top-level body statements.
/// * `inside_loop` — every TirRef reachable from the loop body.
///   Classifies owners as inside-loop vs pre-loop; raw-index
///   comparisons are unsound (producer refs sit numerically below
///   their parent body stmt).
/// * `has_any` — targets of Frees scheduled anywhere at loop entry.
///   Jump-exit scheduling only appends Frees anchored inside the loop,
///   which the per-jump `free_inside_loop` check already accounts for,
///   so this snapshot stays exact for every jump in the loop.
/// * `top_level` — each ref inside the loop body mapped to the
///   top-level body statement that contains it (the first container in
///   source order wins). Turns "which body stmt reaches the jump" into
///   one lookup instead of a per-stmt containment walk of the body.
pub(crate) struct LoopExitCtx {
    body: Vec<TirRef>,
    inside_loop: HashSet<TirRef>,
    has_any: HashSet<TirRef>,
    top_level: HashMap<TirRef, TirRef>,
}

impl LoopExitCtx {
    fn new(tir: &Tir, sidecar: &FunctionSidecar, loop_inst: TirRef) -> Option<Self> {
        let body = tir.loop_body(loop_inst)?;
        let mut inside_loop: HashSet<TirRef> = HashSet::new();
        tir.collect_loop_body_refs(loop_inst, &mut inside_loop);
        let has_any: HashSet<TirRef> = sidecar.free_schedule.iter().map(|fp| fp.target).collect();
        let mut top_level: HashMap<TirRef, TirRef> = HashMap::new();
        for &stmt in &body {
            let mut reachable: HashSet<TirRef> = HashSet::new();
            tir.collect_reachable(stmt, &mut reachable);
            for r in reachable {
                top_level.entry(r).or_insert(stmt);
            }
        }
        Some(Self {
            body,
            inside_loop,
            has_any,
            top_level,
        })
    }
}

/// Schedule unconditional Frees on `break`/`continue` paths. Runs after
/// the last-use, anonymous-temp, and dead-store passes have populated
/// `sidecar.free_schedule`, so per-jump scheduling can read existing
/// entries to decide whether an inside-loop owner is already covered
/// (see `schedule_break_continue_frees`). `enclosing` is the per-loop
/// context of the nearest enclosing `WhileLoop`/`ForRange`, or `None`
/// at top-level (where `Break`/`Continue` would already have been
/// rejected by sema).
pub(crate) fn schedule_loop_exit_frees_in(
    tir: &Tir,
    own: &Ownership,
    sidecar: &mut FunctionSidecar,
    stmts: &[TirRef],
    enclosing: Option<&LoopExitCtx>,
) {
    for &r in stmts {
        let inst = *tir.inst(r);
        match inst.tag {
            TirTag::Break | TirTag::Continue => {
                if let Some(ctx) = enclosing {
                    schedule_break_continue_frees(tir, own, sidecar, r, ctx);
                }
                // Else: outside any loop — sema rejects this with a
                // dedicated diagnostic, so well-formed TIR never
                // reaches here.
            }
            TirTag::WhileLoop | TirTag::ForRange => {
                // The loop-body reachability and scheduled-Free index
                // are invariant across jumps in the same loop — compute
                // them once here, not per break/continue.
                if let Some(ctx) = LoopExitCtx::new(tir, sidecar, r) {
                    schedule_loop_exit_frees_in(tir, own, sidecar, &ctx.body, Some(&ctx));
                }
            }
            TirTag::IfStmt => {
                let view = tir.if_stmt_view(r);
                schedule_loop_exit_frees_in(tir, own, sidecar, &view.then_stmts, enclosing);
                for elif in &view.elif_branches {
                    schedule_loop_exit_frees_in(tir, own, sidecar, &elif.body, enclosing);
                }
                if let Some(else_stmts) = &view.else_stmts {
                    schedule_loop_exit_frees_in(tir, own, sidecar, else_stmts, enclosing);
                }
            }
            _ => {}
        }
    }
}

/// Schedule one Free per `Valid` owner not already covered by a Free
/// that fires on the jump's path.
///
/// Inside-loop owners (in `inside_loop`) — schedule iff no scheduled
/// Free is anchored on the path that reaches this jump. The next
/// iteration allocates a fresh buffer, so jump-side Frees are safe.
///
/// Pre-loop owners — schedule defensively iff NO Free is scheduled
/// anywhere. We can't free a pre-loop owner on the jump path: a
/// `continue` would resurrect the binding for the next iteration,
/// which would then read freed memory. The defensive emit covers
/// future producers that bypass last-use AND dead-store passes.
///
/// Catches the jump-path leak in two shapes:
///   - linear: `for: s = alloc(); break; print(s)` — natural Free is
///     anchored AFTER the break in source order; not on jump path.
///   - cross-branch: `for: s = alloc(); if cond: print(s) else: break`
///     — natural Free is anchored INSIDE the then-arm; the else-arm's
///     break is not on the same path, so the Free doesn't cover it.
///
/// `inside_loop` comes precomputed from the per-loop context; only
/// `on_path` is per-jump (it depends on `jump_inst`). Both are derived
/// by transitive reachability from the loop's body — `raw()`
/// comparisons are unsound (producer refs sit numerically below their
/// parent body stmt, and sibling if-arms compare lexically but are not
/// on the same control-flow path).
pub(crate) fn schedule_break_continue_frees(
    tir: &Tir,
    own: &Ownership,
    sidecar: &mut FunctionSidecar,
    jump_inst: TirRef,
    ctx: &LoopExitCtx,
) {
    let inside_loop = &ctx.inside_loop;

    // Compute the set of TirRefs evaluated on the path that takes
    // the jump. A Free covers this jump iff its `after` anchor is
    // in this set — purely lexical raw() ordering misclassifies
    // anchors in sibling if-arms. The jump's enclosing top-level body
    // stmt is one map lookup; stmts before it run to completion, so
    // they are collected directly and the path walk descends only
    // from the enclosing stmt — no per-stmt containment probes along
    // the body.
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
        }
        // Defensive: every jump visited from this loop's body is in
        // the map, so this only guards against malformed input.
        None => {
            let _ = tir.collect_jump_path(&ctx.body, jump_inst, &mut on_path);
        }
    }

    // Index sidecar.free_schedule by target.
    //   covers_this_jump  — Free anchored on a TirRef in `on_path`
    //                       (i.e. fires on the path that takes the jump)
    //   free_inside_loop  — Free anchored anywhere inside the loop
    let mut covers_this_jump: HashSet<TirRef> = HashSet::new();
    let mut free_inside_loop: HashSet<TirRef> = HashSet::new();
    for fp in &sidecar.free_schedule {
        if on_path.contains(&fp.after) {
            covers_this_jump.insert(fp.target);
        }
        if inside_loop.contains(&fp.after) {
            free_inside_loop.insert(fp.target);
        }
    }

    let is_break = matches!(tir.inst(jump_inst).tag, TirTag::Break);

    // Sorted iteration for deterministic free_schedule order.
    let mut sorted_states: Vec<(Owner, OwnerState)> =
        own.states.iter().map(|(o, s)| (*o, s.clone())).collect();
    sorted_states.sort_by_key(|(o, _)| owner_sort_key(o));
    for (owner, state) in &sorted_states {
        let r = match owner.inst_tirref() {
            Some(r) => r,
            None => continue,
        };
        if !matches!(state, OwnerState::Valid) {
            continue;
        }

        if inside_loop.contains(&r) {
            // Inside-loop owner: each iteration allocates fresh, so
            // a jump-anchored Free is safe. Schedule iff no Free
            // already fires on this jump's path.
            if covers_this_jump.contains(&r) {
                continue;
            }
            sidecar.free_schedule.push(FreePoint {
                after: jump_inst,
                target: r,
                span: tir.span(jump_inst),
                branch: None,
            });
            continue;
        }

        // Pre-loop owner: cannot free on continue path — `continue`
        // would resurrect the binding for the next iteration's read.
        // But on break path, if its last-use is inside the loop (which
        // we bypassed), we must free it as we exit the loop.
        if is_break && free_inside_loop.contains(&r) {
            if covers_this_jump.contains(&r) {
                continue;
            }
            sidecar.free_schedule.push(FreePoint {
                after: jump_inst,
                target: r,
                span: tir.span(jump_inst),
                branch: None,
            });
            continue;
        }

        // Defensive emit only when no Free is scheduled anywhere — AND only on
        // break. On `continue` the next iteration would re-read the freed buffer
        // (UAF); the principled fix is path-relative liveness, but until then we
        // accept a potential leak over a UAF.
        if is_break && ctx.has_any.contains(&r) {
            continue;
        }
        if is_break {
            sidecar.free_schedule.push(FreePoint {
                after: jump_inst,
                target: r,
                span: tir.span(jump_inst),
                branch: None,
            });
        }
    }
}

/// Insert into an arg-partition set. These sets are arg-count-sized
/// (usually ≤ 3 entries, very often empty), so a linear-scan Vec beats
/// a HashSet: no `RandomState` seeding or hashing per Call.
pub(crate) fn push_unique(set: &mut Vec<Owner>, owner: Owner) {
    if !set.contains(&owner) {
        set.push(owner);
    }
}
