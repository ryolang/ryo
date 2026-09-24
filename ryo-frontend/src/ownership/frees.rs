//! Free scheduling (last-use anchors, materialize sites) — split from `mod.rs`.

use super::{
    Owner, OwnerState, Ownership, inout_escape_owner, needs_tracking, owner_sort_key,
    projection_root,
};
use ryo_core::diag::{Diag, DiagCode, DiagSink};
use ryo_core::tir::{ChildKind, Tir, TirData, TirRef, TirTag};
use ryo_core::types::InternPool;
use std::collections::{HashMap, HashSet};

/// Assign every instruction in the function a monotonic rank in
/// forward walk order (the same traversal `collect_last_uses` uses),
/// so two last-use anchors can be compared for "later" (P5). Dense
/// per-instruction table indexed by `TirRef::index()`; rank 0 means
/// "unranked" (matching the old `unwrap_or(0)` fallback), so
/// assignment starts at 1 — a uniform shift that leaves every
/// rank-vs-rank comparison unchanged.
pub(crate) fn program_order(tir: &Tir) -> Vec<u32> {
    fn assign(tir: &Tir, r: TirRef, order: &mut [u32], next: &mut u32) {
        debug_assert!(!r.is_param());
        if order[r.index()] != 0 {
            return;
        }
        order[r.index()] = *next;
        *next += 1;
        tir.walk_operands(r, &mut |_parent, child, _kind| {
            assign(tir, child, order, next);
        });
    }
    let mut order = vec![0; tir.instructions.len()];
    let mut next = 1u32;
    for &stmt in &tir.body_stmts() {
        assign(tir, stmt, &mut order, &mut next);
    }
    order
}

/// P5 (final spec §3.2): an owner's destruction is deferred to the
/// last use of any projection of it. Returns the later of the owner's
/// own `anchor` and its projections' last uses by program order. A
/// projection that is never read defers nothing — no one can observe
/// the buffer through it.
pub(crate) fn defer_anchor(
    anchor: TirRef,
    owner: &Owner,
    projections_of: &HashMap<Owner, Vec<TirRef>>,
    last_use: &HashMap<TirRef, TirRef>,
    order: &[u32],
) -> TirRef {
    let rank = |r: TirRef| order.get(r.index()).copied().unwrap_or(0);
    let mut best = anchor;
    if let Some(views) = projections_of.get(owner) {
        for &view in views {
            if let Some(&read) = last_use.get(&view)
                && rank(read) > rank(best)
            {
                best = read;
            }
        }
    }
    best
}

/// Collect the init/value `TirRef` of every `VarDecl`/`Assign` anywhere
/// in `stmts`, recursing into nested control flow. These are the
/// "named" producers whose Free is owned by the last-use / dead-store /
/// `free_on_reassign` / loop-exit pass; the anon-temp pass skips them to
/// avoid a double-free. Stateless replacement for the old
/// sticky side-set that formerly lived on `Ownership` and accumulated
/// named-init TirRefs during the forward walk. Unlike a
/// `current_owner.values()` derivation this is merge-immune: a temp
/// reassigned inside a loop body is statically the loop-body `Assign`'s
/// value regardless of any loop-merge state.
pub(crate) fn collect_named_inits(tir: &Tir, stmts: &[TirRef]) -> HashSet<TirRef> {
    let mut set = HashSet::new();
    for &s in stmts {
        collect_named_inits_rec(tir, s, &mut set);
    }
    set
}

/// Recursive core of [`collect_named_inits`]. Dispatches on the
/// statement tag, records each `VarDecl`/`Assign` producer, and
/// recurses into `IfStmt` arms / `WhileLoop` body / `ForRange` body so
/// named initializers buried in nested control flow are still
/// classified as named inits.
pub(crate) fn collect_named_inits_rec(tir: &Tir, r: TirRef, set: &mut HashSet<TirRef>) {
    match tir.inst(r).tag {
        TirTag::VarDecl => {
            set.insert(tir.var_decl_view(r).initializer);
        }
        TirTag::Assign => {
            set.insert(tir.assign_view(r).value);
        }
        TirTag::IfStmt => {
            let v = tir.if_stmt_view(r);
            for &s in &v.then_stmts {
                collect_named_inits_rec(tir, s, set);
            }
            for arm in &v.elif_branches {
                for &s in &arm.body {
                    collect_named_inits_rec(tir, s, set);
                }
            }
            if let Some(else_stmts) = v.else_stmts.as_deref() {
                for &s in else_stmts {
                    collect_named_inits_rec(tir, s, set);
                }
            }
        }
        TirTag::WhileLoop => {
            for &s in tir.while_loop_view(r).body.iter() {
                collect_named_inits_rec(tir, s, set);
            }
        }
        TirTag::ForRange => {
            for &s in tir.for_range_view(r).body.iter() {
                collect_named_inits_rec(tir, s, set);
            }
        }
        _ => {}
    }
}

/// True when `r` is a call to a synthesized materialize callee
/// (`__ryo_str_from_view`, M8.4.1.2; `__ryo_bytes_from_view`, M8.4.2)
/// with a single view-typed argument. The callee names are
/// unshadowable (`__ryo_` is reserved), so a name match is unambiguous.
pub(crate) fn is_materialize_call(tir: &Tir, pool: &InternPool, r: TirRef) -> bool {
    if tir.inst(r).tag != TirTag::Call {
        return false;
    }
    let view = tir.call_view(r);
    let name = pool.str(view.name);
    (name == "__ryo_str_from_view" || name == "__ryo_bytes_from_view")
        && view.args.len() == 1
        && pool.is_view(tir.inst(view.args[0]).ty)
}

/// True when `r` is a call to the `__ryo_str_to_bytes` bridge callee
/// (`str`/`strview`.to_bytes(), M8.4.2) — an owned, allocating copy.
/// The callee name is unshadowable (`__ryo_` is reserved), so a name
/// match is unambiguous. `as_bytes()` lowers to a `ToView`, not a
/// call, so it never matches here.
pub(crate) fn is_to_bytes_call(tir: &Tir, pool: &InternPool, r: TirRef) -> bool {
    if tir.inst(r).tag != TirTag::Call {
        return false;
    }
    let view = tir.call_view(r);
    pool.str(view.name) == "__ryo_str_to_bytes" && view.args.len() == 1
}

/// Collect every bound call site — a `VarDecl`/`Assign` whose value
/// satisfies `pred` — as `(decl_stmt, call)` pairs, recursing into
/// nested control flow like [`collect_named_inits`]. Unbound results
/// (call arguments, return operands) are never collected: argument
/// positions are the sema-side warning shapes' jurisdiction.
pub(crate) fn collect_bound_call_sites(
    tir: &Tir,
    stmts: &[TirRef],
    pred: &dyn Fn(&Tir, TirRef) -> bool,
    out: &mut Vec<(TirRef, TirRef)>,
) {
    for &r in stmts {
        match tir.inst(r).tag {
            TirTag::VarDecl => {
                let init = tir.var_decl_view(r).initializer;
                if pred(tir, init) {
                    out.push((r, init));
                }
            }
            TirTag::Assign => {
                let value = tir.assign_view(r).value;
                if pred(tir, value) {
                    out.push((r, value));
                }
            }
            TirTag::IfStmt => {
                let v = tir.if_stmt_view(r);
                collect_bound_call_sites(tir, &v.then_stmts, pred, out);
                for arm in &v.elif_branches {
                    collect_bound_call_sites(tir, &arm.body, pred, out);
                }
                if let Some(else_stmts) = v.else_stmts.as_deref() {
                    collect_bound_call_sites(tir, else_stmts, pred, out);
                }
            }
            TirTag::WhileLoop => {
                collect_bound_call_sites(tir, &tir.while_loop_view(r).body, pred, out);
            }
            TirTag::ForRange => {
                collect_bound_call_sites(tir, &tir.for_range_view(r).body, pred, out);
            }
            _ => {}
        }
    }
}

/// Collect every bound materialize site — a `VarDecl`/`Assign` whose
/// value satisfies [`is_materialize_call`]. Unbound materialize results
/// (call arguments, return operands) are case A's / the escape-fix's
/// jurisdiction, not case B's.
pub(crate) fn collect_materialize_sites(
    tir: &Tir,
    stmts: &[TirRef],
    pool: &InternPool,
    out: &mut Vec<(TirRef, TirRef)>,
) {
    collect_bound_call_sites(tir, stmts, &|t, r| is_materialize_call(t, pool, r), out);
}

/// W0003 case B (M8.4.1.2; generalized to bytes in M8.4.2): a bound
/// `x = str(view)` / `x = bytes(view)` whose copy never escapes and
/// whose source is never mutated after the copy is a redundant
/// allocation — the view could have been used directly.
///
/// Heuristic, warning-only. The escape classification REUSES the
/// walk's results instead of inventing a second escape analysis: the
/// copy's owner ends `Moved` exactly when a consume (return, `move`
/// argument, rebinding assign) followed its binding, and
/// `owner_hazards` records every `inout` pass and mutation the walk
/// observed. Two deliberate non-goals, both resolved toward NO
/// warning (conservative direction):
///
///  - interprocedural flow: a copy passed by borrow to a function
///    that stores it somewhere is invisible here and treated as a
///    non-escape — borrow reads are precisely the uses the view could
///    have served;
///  - conditionally-executed escapes: a move/mutation on ANY branch
///    counts (the merged lattice and the monotone hazard log are
///    path-insensitive), so a maybe-escape suppresses the warning.
///
/// `order` is the per-function [`program_order`] table, built once in
/// `analyze_function` and shared with the P5 deferral.
pub(crate) fn warn_redundant_materialize(
    tir: &Tir,
    pool: &InternPool,
    own: &Ownership,
    order: &[u32],
    sink: &mut DiagSink,
) {
    let mut sites: Vec<(TirRef, TirRef)> = Vec::new();
    collect_materialize_sites(tir, &tir.body_stmts(), pool, &mut sites);
    if sites.is_empty() {
        return;
    }
    let rank = |r: TirRef| order.get(r.index()).copied().unwrap_or(0);
    for (decl, call) in sites {
        let copy = Owner::Inst(call);
        // Escape check: the copy was consumed (returned, move-passed,
        // rebound away) after its binding, or mutated / `inout`-passed
        // anywhere — either makes the owned allocation legitimate. The
        // hazard at `site == decl` is the binding's own consume, how
        // the walk models `x = <value>` — not an escape.
        if matches!(own.states.get(&copy), Some(OwnerState::Moved { .. })) {
            continue;
        }
        if own
            .owner_hazards
            .iter()
            .any(|&(o, site)| o == copy && site != decl)
        {
            continue;
        }
        // Never read at all: W0001 dead-store's jurisdiction, not W0003's.
        if own.pending_dead_store.contains_key(&copy) {
            continue;
        }
        // The view's root must be local and resolvable; a `strview`
        // parameter's buffer belongs to the caller, and the pass
        // cannot judge mutations it cannot see — no warning.
        let view_arg = tir.call_view(call).args[0];
        let Some(root) = projection_root(own, tir, pool, view_arg) else {
            continue;
        };
        // Defensive-copy exception: the root owner is moved, mutated,
        // or `inout`-passed after the materialize point — copying to
        // survive the source's later mutation is the sanctioned use
        // (e.g. ring-buffer reuse). A hazard inside a shared loop
        // re-executes between iterations regardless of source order,
        // so it suppresses too (conservative).
        let mat_rank = rank(call);
        let defensive = own.owner_hazards.iter().any(|&(o, site)| {
            o == root
                && (rank(site) > mat_rank
                    || own.loop_nesting.ancestors_innermost_first(site).any(|l| {
                        own.loop_nesting
                            .ancestors_innermost_first(call)
                            .any(|m| m == l)
                    }))
        });
        if defensive {
            continue;
        }
        let callee = pool.str(tir.call_view(call).name);
        let type_name = if callee == "__ryo_bytes_from_view" {
            "bytes"
        } else {
            "str"
        };
        sink.emit(Diag::warning(
            tir.span(call),
            DiagCode::RedundantMaterialize,
            format!(
                "`{type_name}(...)` copy never escapes and its source is never mutated — the view can be used directly, without the allocation"
            ),
        ));
    }
}

/// W0004: a bound `b = s.to_bytes()` whose `bytes` result is only ever
/// read or borrow-passed is a redundant O(n) allocation + copy —
/// `s.as_bytes()` projects the same bytes as a `bytesview` for free,
/// and every read-only consumer (`bytesview`/`bytes` borrow parameters,
/// indexing, slicing, `print`) accepts it.
///
/// Same post-walk shape as W0003 case B: the classification REUSES the
/// walk's results. Escape/mutation evidence comes from `owner_hazards`
/// (every consume, reassign-drop, and `inout` pass the walk observed,
/// path-insensitive) and the surviving `pending_dead_store` set.
/// Deliberate non-goals, all resolved toward NO warning (conservative):
///
///  - interprocedural flow: a copy borrow-passed to a function that
///    stores it somewhere is invisible here — borrow reads are
///    precisely the uses the view could have served (Rules 5/6 make
///    them non-escaping), so they stay warnable;
///  - conditionally-executed escapes: a hazard on ANY branch counts;
///  - `Assign`-site hazards are ambiguous (a move into an existing
///    `mut` binding and a reassign-drop of the old value look alike in
///    the log), so they always suppress — only moves into a FRESH
///    binding (`VarDecl`) are followed;
///  - never-read results are W0001 dead-store's jurisdiction.
pub(crate) fn warn_redundant_to_bytes(
    tir: &Tir,
    pool: &InternPool,
    own: &Ownership,
    order: &[u32],
    last_use: &HashMap<TirRef, TirRef>,
    sink: &mut DiagSink,
) {
    let mut sites: Vec<(TirRef, TirRef)> = Vec::new();
    collect_bound_call_sites(
        tir,
        &tir.body_stmts(),
        &|t, r| is_to_bytes_call(t, pool, r),
        &mut sites,
    );
    let rank = |r: TirRef| order.get(r.index()).copied().unwrap_or(0);
    for (decl, call) in sites {
        let Some(chain) = copy_chain_clean(own, tir, Owner::Inst(call), decl) else {
            continue;
        };
        // Receiver-root hazard check: `to_bytes()` SNAPSHOTS the
        // source's bytes, while the suggested `as_bytes()` is a live
        // view that freezes its root owner. A mutation, move, or
        // consume of the root AFTER the copy makes the rewrite either
        // fail to compile (P2 freeze) or observe different bytes — but
        // only while the replacement view would still be LIVE, i.e. up
        // to the chain's last read. A hazard past the last read touches
        // a view that no longer exists, so the straight-line
        // `copy … read … mutate` shape warns. A hazard lexically BEFORE
        // the copy inside a shared loop re-executes after the copy on
        // the next iteration. That back-edge collision only materializes
        // when the replacement view would still be LIVE at the next
        // hazard — a copy fully consumed within the loop body (bound
        // and read only inside it) dies before the back-edge and the
        // rewrite is sound, so the clause defers to
        // `chain_outlives_loop_iteration`. A receiver with no local
        // root (a view of caller storage) cannot be hazarded by
        // anything this function does.
        let receiver = tir.call_view(call).args[0];
        if let Some(root) = projection_root(own, tir, pool, receiver) {
            let copy_rank = rank(call);
            // Latest read of any chain link: the point where the
            // replacement view dies. Chain owners with no recorded read
            // are unreachable here (`copy_chain_clean` rejects
            // never-read owners), so the fallback only covers
            // degenerate shapes.
            let chain_last_use = chain
                .iter()
                .filter_map(|o| o.inst_tirref())
                .filter_map(|r| last_use.get(&r))
                .map(|r| rank(*r))
                .max()
                .unwrap_or(copy_rank);
            let hazarded = own.owner_hazards.iter().any(|&(o, site)| {
                o == root
                    && ((rank(site) > copy_rank && rank(site) <= chain_last_use)
                        || own.loop_nesting.ancestors_innermost_first(site).any(|l| {
                            own.loop_nesting
                                .ancestors_innermost_first(call)
                                .any(|m| m == l)
                                && chain_outlives_loop_iteration(
                                    tir, own, &chain, decl, call, l, &rank,
                                )
                        }))
            });
            if hazarded {
                continue;
            }
        }
        sink.emit(Diag::warning(
            tir.span(call),
            DiagCode::RedundantToBytes,
            "this `bytes` is never mutated and never escapes — use `as_bytes()` (zero-copy view) instead of `to_bytes()`".to_string(),
        ));
    }
}

/// W0004 shared-loop refinement: with the hazard lexically before the
/// copy inside loop `l`, the next iteration's hazard collides with the
/// replacement view only if that view is still live at the back-edge.
/// True (suppress) when the chain outlives one iteration:
///
///  - the binding statement lies outside `l`'s subtree (the value
///    predates the loop, so the view spans iterations), or
///  - the binding's NAME is read outside `l`'s subtree after the copy
///    (e.g. a `mut` binding declared before the loop, reassigned inside
///    it, read after it — the last iteration's view is live at the
///    post-loop read). Owner-based matching would miss this: the loop
///    merge may seat the post-loop read on a different owner than the
///    in-loop copy.
///
/// A copy bound and read only within the loop body dies at its last
/// in-iteration use, so the next iteration's earlier-lexical hazard is
/// fine and the warning stands.
fn chain_outlives_loop_iteration(
    tir: &Tir,
    own: &Ownership,
    chain: &[Owner],
    decl: TirRef,
    call: TirRef,
    loop_ref: TirRef,
    rank: &dyn Fn(TirRef) -> u32,
) -> bool {
    let mut subtree: HashSet<TirRef> = HashSet::new();
    tir.collect_reachable(loop_ref, &mut subtree);
    if !subtree.contains(&decl) {
        return true;
    }
    let name = match tir.inst(decl).tag {
        TirTag::VarDecl => tir.var_decl_view(decl).name,
        TirTag::Assign => tir.assign_view(decl).name,
        _ => return true,
    };
    let copy_rank = rank(call);
    (1..tir.instructions.len()).any(|i| {
        let r = TirRef::from_raw(u32::try_from(i).expect("TIR arena index fits u32"));
        if subtree.contains(&r) || rank(r) <= copy_rank {
            return false;
        }
        // A `Var` read of the binding's name, or any read the walk
        // anchored on a chain owner (covers move-chain targets).
        match tir.inst(r).data {
            TirData::Var(n) => {
                n == name
                    || Ownership::dense_get(&own.owner_at_read, r)
                        .is_some_and(|o| chain.contains(&o))
            }
            _ => false,
        }
    })
}

/// W0004 classification: `Some(chain)` when `owner` — a `to_bytes()`
/// result or a local it moved into — is only ever read / borrow-passed,
/// where `chain` is every owner the value flowed through (the copy plus
/// move targets). Follows moves across fresh local bindings (`c = b`):
/// the walk models the move as a consume hazard anchored at the
/// target's `VarDecl`, whose initializer becomes the value's new owner;
/// a chain that ends in a binding that itself never escapes still
/// warns. Any other hazard site — `Return`, a `move` call argument, an
/// `inout` pass (`bytes_push`), an `Assign` (move-into-`mut` or
/// reassign-drop), an aggregate store — is a legitimate use of the
/// owned copy and yields `None`.
fn copy_chain_clean(own: &Ownership, tir: &Tir, start: Owner, decl: TirRef) -> Option<Vec<Owner>> {
    let mut work = vec![(start, decl)];
    let mut seen: HashSet<Owner> = HashSet::new();
    while let Some((owner, binding_site)) = work.pop() {
        if !seen.insert(owner) {
            continue;
        }
        // Never read at all: W0001 dead-store's jurisdiction, not W0004's.
        if own.pending_dead_store.contains_key(&owner) {
            return None;
        }
        for &(o, site) in &own.owner_hazards {
            // The hazard at `site == binding_site` is the binding's own
            // consume — how the walk models `b = <value>` — not an escape.
            if o != owner || site == binding_site {
                continue;
            }
            match tir.inst(site).tag {
                TirTag::VarDecl => {
                    work.push((Owner::Inst(tir.var_decl_view(site).initializer), site));
                }
                _ => return None,
            }
        }
    }
    Some(seen.into_iter().collect())
}

/// Every `Return`/`ReturnVoid` statement in `stmts` (any depth), in
/// forward source order. The promotion-free return epilogue anchors a
/// free at each one, so an early-exit path cannot bypass a promotion
/// buffer's single normal anchor.
pub(crate) fn collect_return_stmts(tir: &Tir, stmts: &[TirRef], out: &mut Vec<TirRef>) {
    for &r in stmts {
        match tir.inst(r).tag {
            TirTag::Return | TirTag::ReturnVoid => out.push(r),
            TirTag::IfStmt => {
                let view = tir.if_stmt_view(r);
                collect_return_stmts(tir, &view.then_stmts, out);
                for elif in &view.elif_branches {
                    collect_return_stmts(tir, &elif.body, out);
                }
                if let Some(else_stmts) = &view.else_stmts {
                    collect_return_stmts(tir, else_stmts, out);
                }
            }
            TirTag::WhileLoop | TirTag::ForRange => {
                if let Some(body) = tir.loop_body(r) {
                    collect_return_stmts(tir, &body, out);
                }
            }
            _ => {}
        }
    }
}

/// Snapshot the owners still `Valid` at a return — values the function
/// must destroy on that exit path (see `Ownership::return_epilogue`).
/// The returned value itself is already `Moved` by `analyze_return`,
/// so it is naturally excluded; inout-escape owners are excluded too —
/// they leave through the write-back pointer, not through destruction.
pub(crate) fn record_return_epilogue(own: &mut Ownership, return_stmt: TirRef) {
    let mut live: Vec<Owner> = own
        .states
        .iter()
        .filter(|(o, s)| matches!(s, OwnerState::Valid) && !inout_escape_owner(own, **o))
        .map(|(o, _)| *o)
        .collect();
    // Sorted for deterministic sidecar emission order.
    live.sort_by_key(owner_sort_key);
    if !live.is_empty() {
        own.return_epilogue.push((return_stmt, live));
    }
}

/// For every Move-typed owner that has at least one `Var` read,
/// record the *last* read in forward source order. The map is
/// populated by overwriting (not `or_insert`), so the latest read
/// wins — semantically equivalent to the previous reverse-walk +
/// `or_insert` approach for a tree-shaped IR. Recurses through
/// `Tir::walk_operands` so reads buried inside calls, loops, and if-arms
/// are still seen. M8.4 (P4/P5, final spec §3.2): `strview`-typed reads
/// are recorded too — keyed by the view's slice instruction — so the
/// P5 deferral can compare an owner's last use against its
/// projections' last uses.
pub(crate) fn collect_last_uses(
    tir: &Tir,
    pool: &InternPool,
    own: &Ownership,
    r: TirRef,
    last_use: &mut HashMap<TirRef, TirRef>,
) {
    let inst = *tir.inst(r);
    // Record this instruction's own `Var` read, if any. Resolve via
    // the per-read `owner_at_read` snapshot taken during the forward
    // walk — `current_owner`'s end-of-function state would misroute
    // reads that precede a `mut` reassignment to the post-rebind
    // owner (wrong target, double-free hazard once heap-allocated
    // strings reach this pattern). The snapshot anchors each read to
    // the owner that was live *at that read*, regardless of any
    // subsequent rebinds.
    if let TirTag::Var = inst.tag
        && let TirData::Var(_) = inst.data
        && (needs_tracking(inst.ty, pool) || pool.is_view(inst.ty))
        && let Some(owner) = Ownership::dense_get(&own.owner_at_read, r)
    {
        // Overwriting insert: latest forward-order read wins =
        // last source-order read. `Owner::tirref` keys a `Param`
        // owner under its sentinel ref, so reads of a param-owned
        // binding register a last use for the param too — the
        // last-use pass can then anchor its Free after the param's
        // true last read instead of the last body statement.
        last_use.insert(owner.tirref(&own.param_index), r);
    }
    tir.walk_operands(r, &mut |_parent, operand, _kind| {
        collect_last_uses(tir, pool, own, operand, last_use);
    });
}

/// Build a `child_TirRef → parent_TirRef` map. Each temporary owner
/// has at most one direct parent in the TIR (the tree-shape invariant
/// documented on `Tir`, checked by `validate_tree_shape` in debug
/// builds), so `or_insert` correctly preserves the first parent
/// observed. Used by the anonymous-temporary-free pass to anchor
/// each temp's Free after its single consumer.
///
/// Only `Operand`-kinded edges contribute to `consumer_of`: a body
/// statement nested inside an `if`/`while`/`for` is not a consumer
/// of the surrounding control-flow instruction, so its Free must
/// not be anchored on the loop/branch header. Recursion still
/// descends through `BodyStmt` edges to reach operands buried
/// inside those nested statements.
pub(crate) fn find_consumers(tir: &Tir, r: TirRef, consumer_of: &mut HashMap<TirRef, TirRef>) {
    tir.walk_operands(r, &mut |parent, operand, kind| {
        if matches!(kind, ChildKind::Operand) {
            consumer_of.entry(operand).or_insert(parent);
        }
        find_consumers(tir, operand, consumer_of);
    });
}
