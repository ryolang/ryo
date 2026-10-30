//! Cleanup scheduling and destruction anchors.

use super::{
    DeadStoreAnalysis, HazardKind, Owner, OwnerState, Ownership, PromoCandidate, UNTRACKED_PLACE,
    anchored_after, body_may_return, branch_may_not_return, convert_honored_reseat_drops,
    declared_before_stmt, enclosing_if_main_cond, if_may_fall_through, inout_escape_owner,
    last_use_anchor, outermost_branch_of, outermost_loop_of, owner_sort_key, param_idx,
    schedule_loop_exit_frees_in, schedule_return_epilogue_frees, suppress_redundant_exit_frees,
};
use ryo_core::diag::{Diag, DiagCode, DiagSink};
use ryo_core::flow::{BlockId, DropOp, DropPosition, Flow, Terminator};
use ryo_core::ownership::{FunctionSidecar, PromoFree};
use ryo_core::tir::{ExtraList, Tir, TirRef, TirTag};
use ryo_core::types::{InternPool, StringId, TypeKind};
use std::collections::{HashMap, HashSet};

pub(crate) struct CleanupContext<'a> {
    pub(super) tir: &'a Tir,
    pub(super) flow: &'a Flow,
    pub(super) pool: &'a InternPool,
    pub(super) body_stmts: ExtraList<'a, TirRef>,
    pub(super) order: &'a [u32],
    pub(super) projections_of: &'a HashMap<Owner, Vec<TirRef>>,
    pub(super) last_use: &'a HashMap<TirRef, TirRef>,
    pub(super) last_read_by_name: &'a HashMap<StringId, TirRef>,
    pub(super) loop_carried_owners: &'a HashSet<TirRef>,
    pub(super) reassign_targets: &'a HashSet<Owner>,
    pub(super) live_binding_owners: &'a HashSet<Owner>,
    pub(super) inout_escape_owners: &'a HashSet<Owner>,
    pub(super) reassign_orders: &'a HashMap<TirRef, Vec<TirRef>>,
    pub(super) decl_of_init: &'a HashMap<TirRef, TirRef>,
    pub(super) assign_value_of: &'a HashMap<TirRef, TirRef>,
    pub(super) dead_stores: &'a DeadStoreAnalysis,
}

fn instruction_last_use_drop(
    ctx: &CleanupContext<'_>,
    own: &Ownership,
    sidecar: &FunctionSidecar,
    owner: Owner,
    r: TirRef,
) -> Option<DropOp> {
    let stale_reassign_target =
        ctx.reassign_targets.contains(&owner) && !ctx.live_binding_owners.contains(&owner);
    if stale_reassign_target || ctx.inout_escape_owners.contains(&owner) {
        return None;
    }
    let &after = ctx.last_use.get(&r)?;
    // P5 (final spec §3.2): defer the destruction to
    // the last use of any projection of this owner.
    let after = defer_anchor(
        after,
        &owner,
        ctx.projections_of,
        ctx.last_use,
        &own.view_last_use,
        ctx.order,
    );
    // Conditional last use: a named binding whose LAST
    // READ is inside a branch/loop is re-anchored by
    // `last_use_anchor` (see loops.rs) — normally to
    // the branch exit, the earliest point where the
    // value is dead on ALL paths, EXCEPT a loop-carried
    // owner whose binding is read again after the loop.
    let anchor = last_use_anchor(
        ctx.tir,
        ctx.flow,
        ctx.pool,
        r,
        after,
        sidecar,
        ctx.order,
        ctx.last_read_by_name,
        ctx.loop_carried_owners,
        own.owner_hazards.iter().any(|&(o, site, kind)| {
            o == owner
                && matches!(kind, HazardKind::Move)
                && matches!(ctx.tir.inst(site).tag, TirTag::Return | TirTag::ReturnVoid)
        }),
    )?;
    if covered_by_later_reassign(ctx, owner, r, anchor) {
        return None;
    }
    Some(DropOp {
        position: DropPosition::AfterStep { inst: anchor },
        target: r,
        span: ctx.tir.span(r),
    })
}

fn covered_by_later_reassign(
    ctx: &CleanupContext<'_>,
    owner: Owner,
    r: TirRef,
    anchor: TirRef,
) -> bool {
    // Anchor-order suppression: the owner is its
    // binding's current owner (the stale-target check
    // above did not fire — a loop merge's first-wins
    // reseat leaves the PRE-reassign owner seated), but
    // the binding is reassigned AFTER this anchor. The
    // reassign-Free then releases this exact buffer at
    // the first store that displaces it, and codegen's
    // binding-path redirect frees the slot's CURRENT
    // content wherever a later Free targets this owner —
    // so scheduling a last-use Free here double-frees.
    // Whatever the binding holds after its last
    // reassign is released by that value's own final
    // Free (last-use / dead-store / loop-exit), which
    // redirects to the same home slot. The reassign set
    // is the owner's OWN binding (its declaring
    // VarDecl): a same-named shadow's reassigns target
    // a different binding and home slot, so they must
    // not suppress this owner — when they did, the
    // outer buffer leaked on every path where the
    // outer reassign never ran.
    // A value belongs to exactly one binding's
    // home-slot lineage — either a declaring VarDecl's
    // init or a single reseating Assign's value, never
    // both — so the `.or()` below is unambiguous.
    debug_assert!(
        !(ctx.decl_of_init.contains_key(&r) && ctx.assign_value_of.contains_key(&r)),
        "owner {r:?} is both a VarDecl init and an Assign value"
    );
    if ctx.reassign_targets.contains(&owner)
        && let Some(&binding) = ctx.decl_of_init.get(&r).or(ctx.assign_value_of.get(&r))
        && ctx.reassign_orders.get(&binding).is_some_and(|v| {
            v.iter()
                .any(|&rr| anchored_after(ctx.tir, ctx.order, rr, anchor))
        })
    {
        return true;
    }
    false
}

fn param_last_use_drop(ctx: &CleanupContext<'_>, own: &Ownership, owner: Owner) -> Option<DropOp> {
    // A param owner: an `inout str` param's value escapes
    // through the write-back pointer — never freed by the
    // callee, even if a branch merge left its owner stamped
    // Valid.
    let name = ctx.flow.place(owner.0).name();
    if own.inout_str_params.contains(&name) {
        return None;
    }
    let idx = param_idx(&own.param_index, name);
    // Anchor the Free after the param's last read — the
    // same policy locals get — so later statements that
    // never touch the param don't keep its buffer alive.
    // A never-read param keeps the old anchor (after the
    // last body statement): it must still be freed exactly
    // once.
    let after = match ctx.last_use.get(&TirRef::param(idx)) {
        Some(&after) => {
            // P5 (final spec §3.2): defer the destruction to
            // the last use of any projection of this param
            // (a slice of it keeps the buffer alive).
            let after = defer_anchor(
                after,
                &owner,
                ctx.projections_of,
                ctx.last_use,
                &own.view_last_use,
                ctx.order,
            );
            // Conditional last use: same re-anchor as `Inst`
            // owners — a last read inside a branch frees at
            // the branch's exit. Anchoring after the read
            // itself fires per-iteration in loops (UAF on
            // later reads) and never fires on not-taken
            // arms (leak); arms that `return` are covered
            // by the return epilogue. The declared-before
            // check locals need is trivially true here:
            // params precede the body.
            Some(outermost_branch_of(ctx.tir, after).unwrap_or(after))
        }
        None => ctx.body_stmts.iter().next_back(),
    }?;
    Some(DropOp {
        position: DropPosition::AfterStep { inst: after },
        target: TirRef::param(idx),
        span: ctx.tir.params[idx].span,
    })
}

pub(crate) fn schedule_last_use_frees(
    ctx: &CleanupContext<'_>,
    own: &Ownership,
    sidecar: &mut FunctionSidecar,
) {
    // Iterate owners in a sorted order so `drops` push
    // order does not depend on HashMap iteration order.
    let mut sorted_states: Vec<(Owner, OwnerState)> =
        own.states.iter().map(|(o, s)| (*o, s.clone())).collect();
    sorted_states.sort_by_key(|(o, _)| owner_sort_key(o));
    for (owner, state) in &sorted_states {
        if !matches!(state, OwnerState::Valid) {
            continue;
        }
        let drop = match owner.inst_tirref(ctx.flow) {
            Some(r) => instruction_last_use_drop(ctx, own, sidecar, *owner, r),
            None => param_last_use_drop(ctx, own, *owner),
        };
        if let Some(drop) = drop {
            sidecar.drops.push(drop);
        }
    }
}

pub(crate) fn schedule_anonymous_temp_frees(
    ctx: &CleanupContext<'_>,
    own: &Ownership,
    sidecar: &mut FunctionSidecar,
) {
    // Anonymous-temporary frees: temp_owners still Valid at function
    // exit need their own Free anchored after their single consumer —
    // UNLESS the temp was ever a named binding's initializer/value, in
    // which case its Free is owned by the last-use / dead-store /
    // free_on_reassign / loop-exit pass and must be skipped here to
    // avoid a double-free.
    //
    // This "was a named init" predicate is a TIR-shape fact, not a
    // lattice-state fact, so it is derived statically via
    // `collect_named_inits`. The old implementation carried a
    // walk; the static set is merge-immune where a
    // `current_owner.values()` derivation would not be (it drops a
    // loop-rebound temp at the loop merge but the temp is still freed
    // by the loop-exit pass, so the dynamic classifier would schedule
    // a spurious second Free). The static set is provably equivalent
    // to the old sticky set across all cases.
    let named_inits: HashSet<TirRef> = collect_named_inits(ctx.tir, ctx.flow);
    let mut consumer_of: HashMap<TirRef, TirRef> = HashMap::new();
    find_consumers(ctx.flow, ctx.tir.instructions.len(), &mut consumer_of);
    let mut sorted_temps: Vec<Owner> = own.temp_owners.iter().copied().collect();
    sorted_temps.sort_by_key(owner_sort_key);
    for temp in sorted_temps {
        // Temps always name an instruction, never a param.
        let Some(t) = temp.inst_tirref(ctx.flow) else {
            continue;
        };
        if named_inits.contains(&t) {
            // Freed by the last-use / dead-store / free_on_reassign /
            // loop-exit pass — skip to avoid a double-free.
            continue;
        }
        if !matches!(own.states.get(&temp), Some(OwnerState::Valid)) {
            // Already moved (flowed into a `move` arg, return, etc.).
            continue;
        }
        if let Some(&consumer) = consumer_of.get(&t) {
            // P5 (final spec §3.2): a sliced temp stays alive until
            // the projection's last use (e.g. `v = (a + b)[0:1]` keeps
            // the concat buffer alive through reads of `v`).
            let anchor = defer_anchor(
                consumer,
                &temp,
                ctx.projections_of,
                ctx.last_use,
                &own.view_last_use,
                ctx.order,
            );
            // A temp PRODUCED in an if's main condition exists on every
            // path through the branch (the condition is always
            // evaluated), so anchor its Free after the if itself: codegen
            // then emits it in the merge block on all paths. Anchoring
            // after the consumer lets the end-of-statement sweep fire
            // inside the taken arm only, leaking the temp on every
            // not-taken path. Skip when no path reaches the merge block
            // (every arm returns or jumps out) — the anchor would never
            // fire there; the return-epilogue / loop-exit passes own
            // those paths.
            let anchor = match enclosing_if_main_cond(ctx.tir, t) {
                Some(if_stmt) if if_may_fall_through(ctx.tir, if_stmt) => if_stmt,
                _ => anchor,
            };
            // A P5-deferred anchor can land inside an if arm (a view of
            // the temp used only there). When the producer dominates the
            // if and the if falls through, re-anchor at the if: the
            // merge fires the Free on every path, where the producer's
            // slot is initialized. An arm anchor leaks on the not-taken
            // paths (path coverage proves it). A producer created
            // inside the conditional keeps the arm anchor: its slot is
            // uninitialized on the other paths, so a merge-anchored
            // Free would release garbage.
            let anchor = match outermost_branch_of(ctx.tir, anchor) {
                Some(branch_stmt)
                    if ctx.tir.inst(branch_stmt).tag == TirTag::IfStmt
                        && if_may_fall_through(ctx.tir, branch_stmt)
                        && !ctx.tir.contains_reachable(branch_stmt, t)
                        && ctx.order.get(t.index()).copied().unwrap_or(0)
                            < ctx.order.get(branch_stmt.index()).copied().unwrap_or(0) =>
                {
                    branch_stmt
                }
                _ => anchor,
            };
            sidecar.drops.push(DropOp {
                position: DropPosition::AfterStep { inst: anchor },
                target: t,
                span: ctx.tir.span(t),
            });
        }
        // No consumer = unreachable from any body statement; can't
        // happen in well-formed TIR. Don't emit (no consumer means
        // codegen's inst_values won't have ptr/cap either).
    }
}

fn promotion_anchor(ctx: &CleanupContext<'_>, own: &Ownership, cand: PromoCandidate) -> TirRef {
    let rank = |r: TirRef| ctx.order.get(r.index()).copied().unwrap_or(0);
    let bound = own.root_owner.contains_key(&Owner(
        own.inst_place[cand.view_inst.index()].expect("view owner place"),
    ));
    if !bound {
        // Transient slice (e.g. `print(s[0:1])`): the projection
        // dies with its enclosing statement.
        return cand.stmt;
    }
    if let Some(loop_ref) = Ownership::dense_get(&own.view_defer_loop, cand.view_inst) {
        // Created outside a loop, last read inside it: dies at
        // the loop's exit. The creation dominates the loop, so
        // the scratch slot is initialized on every path here.
        loop_ref
    } else if let Some(lu) = Ownership::dense_get(&own.view_last_use, cand.view_inst) {
        // P5: a reslice of this view (e.g. `w = s[0:2];
        // v = w[0:1]`) keeps the same promotion buffer alive —
        // defer to the last use of any projection of this view,
        // exactly as owner frees do.
        let lu = defer_anchor(
            lu,
            &Owner(own.inst_place[cand.view_inst.index()].expect("view owner place")),
            ctx.projections_of,
            ctx.last_use,
            &own.view_last_use,
            ctx.order,
        );
        // Conditional last use: re-anchor to the branch exit —
        // but only when the view's creation dominates the
        // branch, else the slot may be uninitialized on the
        // skipped path (mirrors the declared-before check of
        // the Owner::Param free path).
        match outermost_branch_of(ctx.tir, lu) {
            Some(branch_stmt)
                if branch_may_not_return(ctx.tir, branch_stmt)
                    && rank(cand.view_inst) < rank(branch_stmt) =>
            {
                branch_stmt
            }
            _ => lu,
        }
    } else {
        // No possible binding read reaches this view, so its
        // promotion buffer can be freed at creation. Loop
        // back-edges and zero-iteration exits are included in
        // the view-liveness fixed point.
        cand.stmt
    }
}

fn schedule_promotion_exit_frees(
    ctx: &CleanupContext<'_>,
    sidecar: &mut FunctionSidecar,
    exit_stmt: TirRef,
    promo_anchors: &[(TirRef, TirRef)],
    emitted: &mut HashSet<(TirRef, TirRef)>,
) {
    // Each candidate's normal free has one anchor, so returns and the
    // synthesized fallthrough return need backstop frees on paths that
    // bypass it. Flag-conditional emission makes duplicate path coverage
    // harmless. Skip a normal anchor on this statement and dedupe repeated
    // exits through the shared set, preserving candidate order.
    for &(base, normal_anchor) in promo_anchors {
        if normal_anchor == exit_stmt {
            continue;
        }
        if !emitted.insert((exit_stmt, base)) {
            continue;
        }
        sidecar.promotion_frees.push(PromoFree {
            position: DropPosition::AfterStep { inst: exit_stmt },
            base,
            span: ctx.tir.span(exit_stmt),
        });
    }
}

pub(crate) fn schedule_promotion_frees(
    ctx: &CleanupContext<'_>,
    own: &mut Ownership,
    sidecar: &mut FunctionSidecar,
    block_order: &[BlockId],
) {
    let candidates = std::mem::take(&mut own.promo_candidates);
    // (base, normal anchor) pairs, kept for the exit epilogues below.
    let mut promo_anchors: Vec<(TirRef, TirRef)> = Vec::with_capacity(candidates.len());
    for cand in candidates {
        let after = promotion_anchor(ctx, own, cand);
        sidecar.promotion_frees.push(PromoFree {
            position: DropPosition::AfterStep { inst: after },
            base: cand.base,
            span: ctx.tir.span(cand.view_inst),
        });
        // Pair the allocation identity with the base's releases.
        sidecar.promo_allocs.push(PromoFree {
            position: DropPosition::AfterStep {
                inst: cand.view_inst,
            },
            base: cand.base,
            span: ctx.tir.span(cand.view_inst),
        });
        promo_anchors.push((cand.base, after));
    }

    let mut return_stmts: Vec<TirRef> = Vec::new();
    collect_return_stmts(ctx.flow, block_order, &mut return_stmts);
    let mut promo_epilogue_emitted: HashSet<(TirRef, TirRef)> = HashSet::new();
    for return_stmt in return_stmts {
        schedule_promotion_exit_frees(
            ctx,
            sidecar,
            return_stmt,
            &promo_anchors,
            &mut promo_epilogue_emitted,
        );
    }

    if let Some(last) = ctx.body_stmts.iter().next_back() {
        let may_fall_through = match ctx.tir.inst(last).tag {
            TirTag::Return | TirTag::ReturnVoid => false,
            TirTag::IfStmt => if_may_fall_through(ctx.tir, last),
            _ => true,
        };
        if may_fall_through {
            schedule_promotion_exit_frees(
                ctx,
                sidecar,
                last,
                &promo_anchors,
                &mut promo_epilogue_emitted,
            );
        }
    }
}

pub(crate) fn drain_dead_stores(
    ctx: &CleanupContext<'_>,
    own: &Ownership,
    sidecar: &mut FunctionSidecar,
    warnings: &mut DiagSink,
) {
    // Warnings (W0001 below, W0003/W0004 in frees.rs) go to the
    // `warnings` buffer, never `sink`: `check` flushes the buffer only
    // when the whole unit is error-free, so an error from any function
    // — analyzed before or after this one — silences them all.

    // Dead-store survivors: emit W0001 and, for live owned values,
    // schedule a Free anchored after the declaring instruction. Skip
    // owners already covered by `free_on_reassign` to avoid double-frees.
    // (`reassign_targets` was computed above for the last-use pass.)
    let mut sorted_dead: Vec<(Owner, super::DeadStoreEntry)> = ctx
        .dead_stores
        .pending()
        .map(|(o, v)| (*o, *v))
        // The shared untracked sentinel (error-shaped TIR seats) owns
        // nothing schedulable; the unit's own diagnostic carries it.
        .filter(|(o, _)| o.0 != UNTRACKED_PLACE)
        .collect();

    sorted_dead.sort_by_key(|(o, _)| owner_sort_key(o));
    for (owner, (name, span, decl_inst, home)) in &sorted_dead {
        // Bound to an `inout str` param: the value escapes through the
        // write-back — it IS used, just not by any TIR instruction the
        // pass can see. Checked by binding home (not by current owner): a
        // rebind inside a branch is discarded by the merge, leaving the
        // entry keyed by a branch-local owner the exit-time escape set
        // can't see. Same-named shadow bindings still warn. No W0001, no Free.
        if own.inout_str_params.contains(name) && own.param_places[own.param_index[name]] == *home {
            continue;
        }
        if ctx.inout_escape_owners.contains(owner) {
            continue;
        }
        // Never pile a warning onto an already-failing unit — `check`
        // drops this buffer if any error fired anywhere in the unit.
        warnings.emit(Diag::warning(
            *span,
            DiagCode::DeadStore,
            format!("value `{}` is declared but never used", ctx.pool.str(*name)),
        ));
        if decl_inst.is_param()
            || !matches!(
                own.states.get(owner),
                Some(OwnerState::Valid | OwnerState::Moved { .. })
            )
        {
            continue;
        }
        if ctx.reassign_targets.contains(owner) {
            // The reassignment Free already covers this owner;
            // emitting another dead-store Free would double-free.
            continue;
        }
        // A dead reassign INSIDE A LOOP for a binding declared
        // before that loop: anchor the Free after the outermost loop
        // rather than after the in-loop assign. The in-loop anchor
        // fires only when the body executes — the zero-iteration path
        // leaks the pre-loop buffer. The after-loop anchor emits the
        // binding's CURRENT FatLocals (the init→name map): the final iteration's
        // value on taken paths, the pre-loop value on zero iterations.
        // When the body may `return`, keep the in-loop Free too — the
        // after-loop anchor is unreachable on the return path.
        let (anchor, also_in_body) = match outermost_loop_of(ctx.tir, *decl_inst) {
            Some(loop_stmt) if declared_before_stmt(ctx.tir, *name, loop_stmt) => {
                let body = ctx
                    .tir
                    .loop_body_view(loop_stmt)
                    .expect("outermost_loop_of returns loops");
                let may_return = body_may_return(ctx.tir, &body);
                (loop_stmt, may_return)
            }
            _ => (*decl_inst, false),
        };
        sidecar.drops.push(DropOp {
            position: DropPosition::AfterStep { inst: anchor },
            target: owner.tirref(ctx.flow),
            span: *span,
        });
        if also_in_body {
            sidecar.drops.push(DropOp {
                position: DropPosition::AfterStep { inst: *decl_inst },
                target: owner.tirref(ctx.flow),
                span: *span,
            });
        }
    }

    // Match-arm bindings never read inside their arm (M12): ownership
    // already scheduled their arm-gated Frees; only W0001 belongs here.
    for (name, span, _, home) in ctx.dead_stores.arm_dead_stores() {
        let hidden_union_binding = ctx.flow.token_places_iter().any(|(owner, places)| {
            if !places.contains(&home) || ctx.tir.inst(owner).tag != TirTag::Match {
                return false;
            }
            let scrutinee = ctx.tir.match_view(owner).scrutinee;
            matches!(
                ctx.pool.kind(ctx.tir.inst(scrutinee).ty),
                TypeKind::ErrorUnion | TypeKind::InferredErrorUnion
            )
        });
        // Sema owns unused-binding warnings for hidden catch bindings.
        if hidden_union_binding {
            continue;
        }
        warnings.emit(Diag::warning(
            span,
            DiagCode::DeadStore,
            format!("value `{}` is declared but never used", ctx.pool.str(name)),
        ));
    }
}

pub(crate) fn finalize_cleanup(
    ctx: &CleanupContext<'_>,
    own: &Ownership,
    sidecar: &mut FunctionSidecar,
    optional_optimizations: bool,
    _sink: &mut DiagSink,
) {
    // Convert honored reseat records into block-start DropOps (see the
    // helper for the honoring rules).
    convert_honored_reseat_drops(
        ctx.tir,
        own,
        ctx.dead_stores,
        ctx.flow,
        sidecar,
        ctx.decl_of_init,
        ctx.assign_value_of,
    );

    // Drop redundant branch-exit Frees of branch-reseated values before
    // the loop-exit / return-epilogue passes dedup against the schedule.
    suppress_redundant_exit_frees(
        ctx.tir,
        ctx.flow,
        sidecar,
        ctx.order,
        ctx.decl_of_init,
        ctx.assign_value_of,
        ctx.reassign_orders,
    );

    // Loop-exit Frees run LAST so they can inspect the now-complete
    // `drops` schedule and only add jump-anchored Frees for inside-loop
    // owners that no earlier pass already covered.
    // Dominators are built on demand (break defensive emit only) and
    // shared across every loop in this function.
    let doms = std::cell::OnceCell::new();
    schedule_loop_exit_frees_in(
        ctx.tir,
        ctx.flow,
        ctx.pool,
        own,
        sidecar,
        &ctx.body_stmts,
        None,
        &doms,
    );

    schedule_return_epilogue_frees(
        ctx.tir,
        ctx.flow,
        own,
        ctx.dead_stores,
        sidecar,
        &ctx.body_stmts,
        ctx.order,
        ctx.decl_of_init,
        ctx.assign_value_of,
        ctx.reassign_orders,
    );

    if !optional_optimizations {
        sidecar.consumed_concat_lhs.fill(None);
    }
    sidecar.transfers = own
        .owner_hazards
        .iter()
        .filter_map(|&(owner, site, kind)| {
            (kind == HazardKind::Move).then_some(ryo_core::ownership::StorageTransfer {
                source: owner.0,
                target: owner.tirref(ctx.flow),
                site,
            })
        })
        .collect();
    sidecar
        .transfers
        .sort_by_key(|transfer| (transfer.site.raw(), transfer.source.0));
    sidecar.transfers.dedup();

    // backend validators: debug-only cleanup-schedule completeness validator (see
    // `cleanup_validation`). Runs after every scheduling pass so the sidecar is
    // final; any broken invariant is a compiler bug, so panic. The path
    // checks run only on bodies the analysis itself found clean (an
    // errored body's best-effort schedule never reaches codegen).
    #[cfg(debug_assertions)]
    {
        let summary = super::cleanup_validation::validate_cleanup(
            ctx.tir,
            ctx.flow,
            ctx.pool,
            own,
            sidecar,
            !_sink.has_errors(),
        );
        sidecar.validation = Some(summary);
    }
    sidecar.seal();
}

/// Document-order rank: the build's emission stamp
/// (`Flow::order_of`) — parents before children in source position,
/// dead continuations emitted in place. Replaces the old TIR pre-order
/// `program_order` scan; the Flow builder stamps every instruction
/// exactly once while lowering, so no second walk is needed.
pub(crate) fn document_rank(flow: &Flow, r: TirRef) -> u32 {
    flow.order_of(r).unwrap_or(0)
}

/// P5 (final spec §3.2): an owner's destruction is deferred to the
/// last use of any projection of it. Returns the later of the owner's
/// own `anchor` and its projections' last uses by program order. A
/// projection that is never read defers nothing — no one can observe
/// the buffer through it.
///
/// A projection's final use lives in the view-liveness table
/// (`view_last_use`), never in `last_use`: a view is consumed by its
/// binding/assign, not Var-read, so the owner-at-read-keyed table has
/// no entry for it. Reading only `last_use` let a base's destruction
/// anchor at a branch exit that fires before a reseated view's final
/// use (view-protection UAF). Type-agnostic — the tables are keyed by
/// view instruction, so strview/bytesview and future slice[T]/list[T]/
/// map[K][V] views defer their bases identically.
pub(crate) fn defer_anchor(
    anchor: TirRef,
    owner: &Owner,
    projections_of: &HashMap<Owner, Vec<TirRef>>,
    last_use: &HashMap<TirRef, TirRef>,
    view_last_use: &[Option<TirRef>],
    order: &[u32],
) -> TirRef {
    let rank = |r: TirRef| order.get(r.index()).copied().unwrap_or(0);
    let mut best = anchor;
    if let Some(views) = projections_of.get(owner) {
        for &view in views {
            let uses = last_use
                .get(&view)
                .copied()
                .into_iter()
                .chain(Ownership::dense_get(view_last_use, view));
            for read in uses {
                if rank(read) > rank(best) {
                    best = read;
                }
            }
        }
    }
    best
}

/// Collect the init/value `TirRef` of every `VarDecl`/`Assign`/
/// `Destructure` in the body — the "named" producers whose Free is
/// owned by the last-use / dead-store / `free_on_reassign` / loop-exit
/// pass; the anon-temp pass skips them to avoid a double-free. Driven
/// from Flow IR steps: declarations are always stepped, and block
/// iteration covers arms and nested positions with no statement
/// recursion (the old match-descent machinery is unnecessary). Unlike a
/// `current_owner.values()` derivation this static set is merge-immune:
/// a temp reassigned inside a loop body is statically the loop-body
/// `Assign`'s value regardless of any loop-merge state.
pub(crate) fn collect_named_inits(tir: &Tir, flow: &Flow) -> HashSet<TirRef> {
    let mut set = HashSet::new();
    for b in flow.forward_order() {
        for step in flow.block_steps(b) {
            match tir.inst(step.inst).tag {
                TirTag::VarDecl => {
                    set.insert(tir.var_decl_view(step.inst).initializer);
                }
                TirTag::Assign => {
                    set.insert(tir.assign_view(step.inst).value);
                }
                TirTag::Destructure => {
                    // M10: a destructured rhs is "named" by the
                    // destructure — the anon-temp pass must skip it or
                    // it would schedule a whole-struct Free on top of
                    // the field moves (double-free).
                    set.insert(tir.destructure_view(step.inst).rhs);
                }
                _ => {}
            }
        }
    }
    set
}

/// Every `Return`/`ReturnVoid` in the walk's block order (document
/// order). The promotion-free return epilogue anchors a free at each
/// one, so an early-exit path cannot bypass a promotion buffer's single
/// normal anchor. Driven from Flow IR terminators: a return is a
/// terminator payload, and the walk order covers returns inside match
/// arms and nested positions with no statement recursion.
pub(crate) fn collect_return_stmts(flow: &Flow, block_order: &[BlockId], out: &mut Vec<TirRef>) {
    for &b in block_order {
        match flow.term(b) {
            Terminator::Return { .. } | Terminator::ReturnVoid => {
                if let Some(p) = flow.term_payload(b) {
                    out.push(p);
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
/// Owners whose producer is ranked AFTER the return's own expression
/// subtree are also dropped: a value produced lexically later than the
/// return cannot exist when it executes. Such owners only appear `Valid`
/// here because loop convergence re-walks seed body-local owners from
/// the backedge state (`merge_non_monotone`) — recording them would
/// schedule a Free the return's path never needs (and codegen cannot
/// lower, since the producer is never materialized on that path).
pub(crate) fn record_return_epilogue(
    tir: &Tir,
    flow: &ryo_core::flow::Flow,
    own: &mut Ownership,
    return_stmt: TirRef,
) {
    // Rank table, built once per function and cached on `Ownership` —
    // loop convergence re-walks Returns, and rebuilding the per-
    // function Vec for each one is pure waste. The rank is the
    // Flow builder's emission stamp (see `document_rank`).
    if own.order.is_empty() {
        own.order = (0..tir.instructions.len() as u32)
            .map(|i| {
                if i == 0 {
                    0
                } else {
                    document_rank(flow, TirRef::from_raw(i))
                }
            })
            .collect();
    }
    let order = &own.order;
    let rank = |r: TirRef| order.get(r.index()).copied().unwrap_or(0);
    // The return's operand subtree is contiguous in the build's
    // emission order, and the operand's own step is stamped LAST in
    // it — so the subtree's max rank is the value step's stamp. No
    // TIR operand recursion.
    let ret_rank = match tir.inst(return_stmt).data {
        ryo_core::tir::TirData::UnOp(value) => {
            flow.step_of(value).map(|s| rank(s.inst)).unwrap_or(0)
        }
        _ => rank(return_stmt),
    };

    let mut live: Vec<Owner> = own
        .states
        .iter()
        .filter(|(o, s)| {
            if !matches!(s, OwnerState::Valid) || inout_escape_owner(own, **o) {
                return false;
            }
            match o.inst_tirref(flow) {
                // Params predate every statement.
                None => true,
                Some(r) => rank(r) <= ret_rank,
            }
        })
        .map(|(o, _)| *o)
        .collect();
    // Sorted for deterministic sidecar emission order.
    live.sort_by_key(owner_sort_key);
    if !live.is_empty() {
        own.return_epilogue.push((return_stmt, live));
    }
}

/// Build a `child_TirRef → parent_TirRef` map from the Flow builder's
/// single-parent stamps: `Flow::parent_of` records the consuming
/// instruction of every operand while lowering — no TIR operand walk.
/// Used by the anonymous-temporary-free pass to anchor each temp's
/// Free after its single consumer. (Body-statement edges contribute
/// nothing: `parent_of` stamps `Operand` kind only.)
pub(crate) fn find_consumers(
    flow: &Flow,
    arena_len: usize,
    consumer_of: &mut HashMap<TirRef, TirRef>,
) {
    // The Flow builder stamps the single consuming parent of every
    // operand while lowering (`Flow::parent_of`) — the tree-shaped
    // TIR gives each inst at most one parent, so this is the complete
    // child → consumer map with no TIR operand recursion.
    for i in 1..arena_len {
        let r = TirRef::from_raw(u32::try_from(i).expect("TIR arena index fits u32"));
        if let Some(parent) = flow.parent_of(r) {
            consumer_of.entry(r).or_insert(parent);
        }
    }
}
