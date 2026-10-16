//! Materialization classification and redundant-copy diagnostics.

use super::{Owner, OwnerState, Ownership, projection_root};
use ryo_core::diag::{Diag, DiagCode, DiagSink};
use ryo_core::tir::{Tir, TirData, TirRef, TirTag};
use ryo_core::types::{InternPool, StringId};
use std::collections::{HashMap, HashSet};

/// Interned ids for the synthesized `__ryo_*` callees the W0003/W0004
/// warnings match call names against. Resolved once per `check` via
/// `find_str` probes: sema interns one of these names only when it
/// synthesized such a call, so a missing id means the predicate can
/// never match — the same result the old `pool.str(...) == "..."`
/// compare produced.
pub(crate) struct SynthCalleeIds {
    str_from_view: Option<StringId>,
    bytes_from_view: Option<StringId>,
    str_to_bytes: Option<StringId>,
}

impl SynthCalleeIds {
    pub(crate) fn resolve(pool: &InternPool) -> Self {
        SynthCalleeIds {
            str_from_view: pool.find_str("__ryo_str_from_view"),
            bytes_from_view: pool.find_str("__ryo_bytes_from_view"),
            str_to_bytes: pool.find_str("__ryo_str_to_bytes"),
        }
    }
}

/// True when `r` is a call to a synthesized materialize callee
/// (`__ryo_str_from_view`, M8.4.1.2; `__ryo_bytes_from_view`, M8.4.2)
/// with a single view-typed argument. The callee names are
/// unshadowable (`__ryo_` is reserved), so a name match is unambiguous.
pub(crate) fn is_materialize_call(
    tir: &Tir,
    pool: &InternPool,
    ids: &SynthCalleeIds,
    r: TirRef,
) -> bool {
    if tir.inst(r).tag != TirTag::Call {
        return false;
    }
    let view = tir.call_view(r);
    (ids.str_from_view == Some(view.name) || ids.bytes_from_view == Some(view.name))
        && view.args.len() == 1
        && pool.is_view(tir.inst(view.args.at(0)).ty)
}

/// True when `r` is a call to the `__ryo_str_to_bytes` bridge callee
/// (`str`/`strview`.to_bytes(), M8.4.2) — an owned, allocating copy.
/// The callee name is unshadowable (`__ryo_` is reserved), so a name
/// match is unambiguous. `as_bytes()` lowers to a `ToView`, not a
/// call, so it never matches here.
pub(crate) fn is_to_bytes_call(tir: &Tir, ids: &SynthCalleeIds, r: TirRef) -> bool {
    if tir.inst(r).tag != TirTag::Call {
        return false;
    }
    let view = tir.call_view(r);
    ids.str_to_bytes == Some(view.name) && view.args.len() == 1
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
    collect_bound_call_sites_iter(tir, stmts.iter().copied(), pred, out);
}

fn collect_bound_call_sites_iter(
    tir: &Tir,
    stmts: impl IntoIterator<Item = TirRef>,
    pred: &dyn Fn(&Tir, TirRef) -> bool,
    out: &mut Vec<(TirRef, TirRef)>,
) {
    for r in stmts {
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
                collect_bound_call_sites_iter(tir, tir.while_loop_body_view(r), pred, out);
            }
            TirTag::ForRange => {
                collect_bound_call_sites_iter(tir, tir.for_range_body_view(r), pred, out);
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
    stmts: impl IntoIterator<Item = TirRef>,
    pool: &InternPool,
    ids: &SynthCalleeIds,
    out: &mut Vec<(TirRef, TirRef)>,
) {
    collect_bound_call_sites_iter(
        tir,
        stmts,
        &|t, r| is_materialize_call(t, pool, ids, r),
        out,
    );
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
/// `order` is the per-function document-rank table (the Flow builder's
/// emission stamp — see [`document_rank`]), built once in
/// `analyze_function` and shared with the P5 deferral.
pub(crate) fn warn_redundant_materialize(
    tir: &Tir,
    flow: &ryo_core::flow::Flow,
    pool: &InternPool,
    ids: &SynthCalleeIds,
    own: &Ownership,
    order: &[u32],
    sink: &mut DiagSink,
) {
    let mut sites: Vec<(TirRef, TirRef)> = Vec::new();
    collect_materialize_sites(tir, tir.body_stmts_view(), pool, ids, &mut sites);
    if sites.is_empty() {
        return;
    }
    let rank = |r: TirRef| order.get(r.index()).copied().unwrap_or(0);
    for (decl, call) in sites {
        let copy = own.owner_of_inst(call);
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
            .any(|&(o, site, _)| o == copy && site != decl)
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
        let view_arg = tir.call_view(call).args.at(0);
        let Some(root) = projection_root(own, tir, flow, pool, view_arg) else {
            continue;
        };
        // Defensive-copy exception: the root owner is moved, mutated,
        // or `inout`-passed after the materialize point — copying to
        // survive the source's later mutation is the sanctioned use
        // (e.g. ring-buffer reuse). A hazard inside a shared loop
        // re-executes between iterations regardless of source order,
        // so it suppresses too (conservative).
        let mat_rank = rank(call);
        let defensive = own.owner_hazards.iter().any(|&(o, site, _)| {
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
        let type_name = if ids.bytes_from_view == Some(tir.call_view(call).name) {
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
#[allow(clippy::too_many_arguments)]
pub(crate) fn warn_redundant_to_bytes(
    tir: &Tir,
    flow: &ryo_core::flow::Flow,
    pool: &InternPool,
    ids: &SynthCalleeIds,
    own: &Ownership,
    order: &[u32],
    last_use: &HashMap<TirRef, TirRef>,
    sink: &mut DiagSink,
) {
    let _ = (pool,);
    let mut sites: Vec<(TirRef, TirRef)> = Vec::new();
    collect_bound_call_sites_iter(
        tir,
        tir.body_stmts_view(),
        &|t, r| is_to_bytes_call(t, ids, r),
        &mut sites,
    );
    let rank = |r: TirRef| order.get(r.index()).copied().unwrap_or(0);
    for (decl, call) in sites {
        let Some(chain) = copy_chain_clean(own, tir, flow, own.owner_of_inst(call), decl) else {
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
        // `chain_outlives_loop_iteration`. The mirror image also
        // collides: a hazard inside a loop that encloses a chain READ
        // but not the copy — the copy predates the loop and is re-read
        // every iteration, so the replacement view lives through the
        // whole loop (the same cyclic liveness `view_defer_loop`
        // models) and any in-loop hazard freezes it regardless of
        // lexical order against the last read. A receiver with no
        // local root (a view of caller storage) cannot be hazarded by
        // anything this function does.
        let receiver = tir.call_view(call).args.at(0);
        if let Some(root) = projection_root(own, tir, flow, pool, receiver) {
            let copy_rank = rank(call);
            // Latest read of any chain link: the point where the
            // replacement view dies. Chain owners with no recorded read
            // are unreachable here (`copy_chain_clean` rejects
            // never-read owners), so the fallback only covers
            // degenerate shapes.
            let chain_last_use = chain
                .iter()
                .filter_map(|o| o.inst_tirref(flow))
                .filter_map(|r| last_use.get(&r))
                .map(|r| rank(*r))
                .max()
                .unwrap_or(copy_rank);
            let hazarded = own.owner_hazards.iter().any(|&(o, site, _)| {
                o == root
                    && ((rank(site) > copy_rank && rank(site) <= chain_last_use)
                        || own.loop_nesting.ancestors_innermost_first(site).any(|l| {
                            own.loop_nesting
                                .ancestors_innermost_first(call)
                                .any(|m| m == l)
                                && chain_outlives_loop_iteration(
                                    tir, own, &chain, decl, call, l, &rank,
                                )
                        })
                        || loop_encloses_chain_read_not_copy(tir, own, &chain, call, site))
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

/// W0004 loop-liveness clause (mirror of the shared-loop refinement):
/// true when the hazard at `site` sits inside a loop that does NOT
/// enclose the copy but DOES enclose a read of a chain owner. The copy
/// then predates the loop and is re-read every iteration, so the
/// replacement view stays live for the whole loop (the same cyclic
/// liveness `view_defer_loop` models) and collides with the hazard no
/// matter where in the body it sits lexically. Owner-based matching
/// suffices here: the read sits inside the loop while the owner lives
/// outside it, so no loop merge re-seats it mid-chain.
fn loop_encloses_chain_read_not_copy(
    tir: &Tir,
    own: &Ownership,
    chain: &[Owner],
    call: TirRef,
    site: TirRef,
) -> bool {
    own.loop_nesting.ancestors_innermost_first(site).any(|l| {
        if own
            .loop_nesting
            .ancestors_innermost_first(call)
            .any(|m| m == l)
        {
            return false;
        }
        let mut subtree: HashSet<TirRef> = HashSet::new();
        tir.collect_reachable(l, &mut subtree);
        (1..tir.instructions.len()).any(|i| {
            let r = TirRef::from_raw(u32::try_from(i).expect("TIR arena index fits u32"));
            subtree.contains(&r)
                && matches!(tir.inst(r).data, TirData::Var(_))
                && Ownership::dense_get(&own.owner_at_read, r).is_some_and(|o| chain.contains(&o))
        })
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
fn copy_chain_clean(
    own: &Ownership,
    tir: &Tir,
    _flow: &ryo_core::flow::Flow,
    start: Owner,
    decl: TirRef,
) -> Option<Vec<Owner>> {
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
        for &(o, site, _) in &own.owner_hazards {
            // The hazard at `site == binding_site` is the binding's own
            // consume — how the walk models `b = <value>` — not an escape.
            if o != owner || site == binding_site {
                continue;
            }
            match tir.inst(site).tag {
                TirTag::VarDecl => {
                    work.push((
                        Owner(
                            own.inst_place[tir.var_decl_view(site).initializer.index()]
                                .expect("chain value owner place"),
                        ),
                        site,
                    ));
                }
                _ => return None,
            }
        }
    }
    Some(seen.into_iter().collect())
}
