//! Free scheduling (last-use anchors, materialize sites) — split from `mod.rs`.

use super::{
    Owner, OwnerState, Ownership, inout_escape_owner, needs_tracking, nesting_of, owner_sort_key,
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

/// True when `r` is a call to the synthesized `__ryo_str_from_view`
/// materialize callee (M8.4.1.2) with a single view-typed argument.
/// The callee name is unshadowable (`__ryo_` is reserved), so a name
/// match is unambiguous.
pub(crate) fn is_materialize_call(tir: &Tir, pool: &InternPool, r: TirRef) -> bool {
    if tir.inst(r).tag != TirTag::Call {
        return false;
    }
    let view = tir.call_view(r);
    pool.str(view.name) == "__ryo_str_from_view"
        && view.args.len() == 1
        && pool.is_view(tir.inst(view.args[0]).ty)
}

/// Collect every bound materialize site — a `VarDecl`/`Assign` whose
/// value satisfies [`is_materialize_call`] — as `(decl_stmt, call)`
/// pairs, recursing into nested control flow like
/// [`collect_named_inits`]. Unbound materialize results (call
/// arguments, return operands) are case A's / the escape-fix's
/// jurisdiction, not case B's.
pub(crate) fn collect_materialize_sites(
    tir: &Tir,
    stmts: &[TirRef],
    pool: &InternPool,
    out: &mut Vec<(TirRef, TirRef)>,
) {
    for &r in stmts {
        match tir.inst(r).tag {
            TirTag::VarDecl => {
                let init = tir.var_decl_view(r).initializer;
                if is_materialize_call(tir, pool, init) {
                    out.push((r, init));
                }
            }
            TirTag::Assign => {
                let value = tir.assign_view(r).value;
                if is_materialize_call(tir, pool, value) {
                    out.push((r, value));
                }
            }
            TirTag::IfStmt => {
                let v = tir.if_stmt_view(r);
                collect_materialize_sites(tir, &v.then_stmts, pool, out);
                for arm in &v.elif_branches {
                    collect_materialize_sites(tir, &arm.body, pool, out);
                }
                if let Some(else_stmts) = v.else_stmts.as_deref() {
                    collect_materialize_sites(tir, else_stmts, pool, out);
                }
            }
            TirTag::WhileLoop => {
                collect_materialize_sites(tir, &tir.while_loop_view(r).body, pool, out);
            }
            TirTag::ForRange => {
                collect_materialize_sites(tir, &tir.for_range_view(r).body, pool, out);
            }
            _ => {}
        }
    }
}

/// W0003 case B (M8.4.1.2): a bound `x = str(view)` whose copy never
/// escapes and whose source is never mutated after the copy is a
/// redundant allocation — the view could have been used directly.
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
        let mat_loops = nesting_of(&own.loop_nesting, call);
        let defensive = own.owner_hazards.iter().any(|&(o, site)| {
            o == root
                && (rank(site) > mat_rank
                    || nesting_of(&own.loop_nesting, site)
                        .iter()
                        .any(|l| mat_loops.contains(l)))
        });
        if defensive {
            continue;
        }
        sink.emit(Diag::warning(
            tir.span(call),
            DiagCode::RedundantMaterialize,
            "`str(...)` copy never escapes and its source is never mutated — the view can be used directly, without the allocation",
        ));
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
