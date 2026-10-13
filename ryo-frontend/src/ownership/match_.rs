//! M12 match walking — split from `walk.rs`; see module docs there.
//!
//! A typed `TirTag::Match` is a discriminated join over per-arm
//! branches, exactly like an `IfStmt` whose arms all entered from one
//! shared scrutinee evaluation: each arm walks from the pre-match
//! snapshot independently (`snapshot_branch` / `take_branch`), and the
//! post-match state is the union over arms (`merge_branches` — moved in
//! any falling-through arm ⇒ moved after).
//!
//! What match adds over if/elif/else:
//!
//! * payload bindings become fresh owners keyed on the by-move owner
//!   tokens (`Tir::match_by_move_owner_refs`, the M10 destructure
//!   convention) — arm-local, seeded before the arm body walks;
//! * an arm that binds at least one non-`Copy` field by move consumes
//!   the SCRUTINEE on that arm's path (the whole-value model: the
//!   payload leaves through the arm, the shell is dead);
//! * on an OWNED scrutinee, the consuming arm's unbound needs-drop
//!   payload fields (`Some(_)` positions) register as arm-start drops
//!   ([`MatchArmDrop`]) — codegen destroys them when the arm starts.
//!   Borrowed scrutinees register nothing (`OwnerState::Borrowed` in
//!   the lattice; sema's E0133 already refuses by-move binds there);
//! * arm-local dead binds (never read in their arm) are swept at arm
//!   end into arm-gated Frees — an ungated after-match Free would free
//!   garbage on the sibling arms' paths.

use super::{
    BranchState, Owner, OwnerState, Ownership, analyze_stmt, body_must_terminate,
    check_source_projected, check_use_moved, consume_for_assignment, consumed_binding_name,
    drain_dying_views, needs_tracking, owner_sort_key, prune_branch_dead_projections,
    refine_view_liveness_for_arm, register_pending_dead_store, restore_view_last_use,
    stmts_subtree, underlying_owner, visit_expr,
};
use ryo_core::diag::DiagSink;
use ryo_core::ownership::{BranchId, FreePoint, FunctionSidecar, MatchArmDrop, MatchBranchIds};
use ryo_core::tir::{MatchArmView, MatchBodyView, Tir, TirData, TirRef, TirTag};
use ryo_core::types::{InternPool, StringId, StructField, TypeId};
use std::collections::HashSet;

/// `visit_expr`'s `Match` arm (M12). The scrutinee is shared flow
/// (evaluated before any arm, like an if condition); each arm then
/// walks from the pre-match snapshot. Errored matches never reach here
/// — sema recovers with the error-typed `Unreachable` sentinel.
pub(crate) fn analyze_match(
    tir: &Tir,
    pool: &InternPool,
    own: &mut Ownership,
    sink: &mut DiagSink,
    sidecar: &mut FunctionSidecar,
    r: TirRef,
) {
    let view = tir.match_view(r);
    if view.is_empty() {
        // Defensive: sema diagnoses source-level; well-formed TIR
        // always has at least the catch-all arm.
        return;
    }
    let scrutinee = view.scrutinee;
    let scrutinee_ty = tir.inst(scrutinee).ty;
    let tracked = needs_tracking(scrutinee_ty, pool);
    // Whether ANY arm binds a non-`Copy` field by move — the consume
    // authority for the scrutinee (each such arm consumes it, and
    // `consume_for_assignment` is the E0020/E0021 reporter there).
    let any_consumes = view
        .arms()
        .flat_map(|arm| arm.bindings())
        .any(|b| !pool.is_copy(b.ty));
    visit_expr(tir, pool, own, sink, sidecar, scrutinee);
    // P4 lift (final spec §3.2), mirroring analyze_if_stmt's condition
    // drain (walk.rs): a projection whose last use is a read inside
    // the scrutinee is dead before any arm runs — drain now so every
    // arm starts from the same freeze state. Without this, a by-move
    // arm's `check_source_projected` (or any freeze check in an arm
    // body) sees the stale projection and emits a false P2.
    drain_dying_views(own);
    if tracked && !any_consumes {
        // No arm consumes: nothing else will check the scrutinee read
        // (the Match inst is the operand's consumer, and this arm IS
        // the match) — a moved scrutinee is a use-after-move here.
        check_use_moved(tir, pool, own, sink, scrutinee, tir.span(scrutinee));
    }
    // Borrow-ness is sourced from OWNERSHIP's own lattice, not sema's
    // `Scope::Binding::borrowed`: `OwnerState::Borrowed` is stamped only
    // on borrow/inout params. `match &o:` on an owned local lands here
    // as `Valid` — safe, because sema's E0133 keeps by-move binds off
    // sema-borrowed scrutinees, so no arm of it can consume.
    let scrutinee_owner = underlying_owner(own, scrutinee);
    let borrowed = matches!(own.states.get(&scrutinee_owner), Some(OwnerState::Borrowed));

    // BranchId per arm, recorded for codegen before any arm walks
    // (the `if_branches` discipline — codegen pushes each arm's id as
    // it lowers the arm).
    let mut arm_ids = Vec::with_capacity(view.len());
    for _ in 0..view.len() {
        arm_ids.push(BranchId(own.next_branch_id));
        own.next_branch_id += 1;
    }
    sidecar.match_branches[r.index()] = Some(MatchBranchIds {
        arms: arm_ids.clone(),
    });

    // By-move owner tokens, arm order then binding order (the
    // `TirBuilder::match_expr` convention); one `FieldAccess` per
    // non-`Copy` binding, immediately before the Match inst.
    let tokens = tir.match_by_move_owner_refs(r, pool);

    let snap = own.snapshot_branch();
    let mut match_subtree: HashSet<TirRef> = HashSet::new();
    tir.collect_reachable(r, &mut match_subtree);

    let mut arm_results: Vec<BranchState> = Vec::with_capacity(view.len());
    let mut falls_through: Vec<bool> = Vec::with_capacity(view.len());
    let mut token_cursor = 0;
    for (arm_index, arm) in view.arms().enumerate() {
        let branch_id = arm_ids[arm_index];
        // (a) Seed the arm's payload bindings as fresh owners keyed on
        // their owner tokens (mirror M10 destructure, structs.rs) —
        // arm-local: the per-arm snapshot/restore keeps sibling arms
        // from seeing them. Copy binds need no owner at all.
        let mut arm_binds: Vec<(TirRef, StringId)> = Vec::new();
        for b in arm.bindings() {
            own.binding_of_name.insert(b.name, r);
            if pool.is_copy(b.ty) {
                continue;
            }
            let token = tokens[token_cursor];
            token_cursor += 1;
            let span = tir.span(r);
            own.states.insert(Owner::Inst(token), OwnerState::Valid);
            Ownership::dense_set(&mut own.origin, token, None);
            own.current_owner.insert(b.name, Owner::Inst(token));
            register_pending_dead_store(own, token, b.name, span, r);
            arm_binds.push((token, b.name));
        }
        // (b) Consumption: an arm binding a non-`Copy` field by move
        // consumes the scrutinee on this arm's path (the payload move
        // IS the whole-value move). The consume also covers E0020 /
        // E0021 and the M9 field guard — a struct-field scrutinee with
        // a by-move bind is a `MoveOutOfField` via `check_field_move_out`.
        let consumes = !arm_binds.is_empty();
        if consumes && tracked {
            let span = tir.span(r);
            let consumed_name = consumed_binding_name(tir, scrutinee);
            // P2 freeze (final spec §3.2): the consume moves the owner.
            check_source_projected(
                tir,
                pool,
                own,
                sink,
                scrutinee_owner,
                span,
                "move",
                consumed_name,
            );
            consume_for_assignment(tir, pool, own, sink, scrutinee, span, consumed_name, r);
        }
        // (c) Arm-start drops: on an OWNED scrutinee, the consuming
        // arm's unbound needs-drop payload fields (the pattern's `_`
        // positions) are destroyed when the arm starts — codegen's
        // jurisdiction, registered here. Borrowed scrutinees register
        // nothing.
        if consumes && !borrowed {
            for (field_index, ty) in unbound_payload_fields(pool, scrutinee_ty, &arm) {
                sidecar.match_arm_drops.push(MatchArmDrop {
                    match_inst: r,
                    arm: branch_id,
                    field_index,
                    ty,
                });
            }
        }
        // (d) Walk the body from the pre-match snapshot (the if-arm
        // discipline, including the per-arm freeze refinement).
        let arm_subtree = arm_stmts(&arm)
            .map(|stmts| stmts_subtree(tir, &stmts))
            .unwrap_or_default();
        let saved = refine_view_liveness_for_arm(own, r, arm_index, &match_subtree, &arm_subtree);
        match arm.body {
            MatchBodyView::Expr(e) => visit_expr(tir, pool, own, sink, sidecar, e),
            MatchBodyView::Stmts(_) => {
                for s in arm.body.stmts() {
                    analyze_stmt(tir, pool, own, sink, sidecar, s);
                }
            }
        }
        restore_view_last_use(own, saved);
        let falls = arm_falls_through(tir, &arm);
        // (e) Dead arm-local binds: a tracked bind never read nor
        // consumed inside the arm dies with the arm. Always remove the
        // pending entry (it must not survive into the merge — the
        // drain would anchor an ungated Free after the match and free
        // garbage on the sibling arms' paths) and buffer the W0001;
        // the arm-gated Free is scheduled only where the anchor
        // actually executes:
        // * a Return end — the return epilogue owns the exit (a second
        //   Free here would double-free);
        // * a Break/Continue end — codegen fires due Frees before the
        //   jump, and the loop-exit pass dedups against it;
        // * any other end — only when the arm falls through (a
        //   must-terminating last statement never completes, so an
        //   after-it anchor would never fire; the nested exits' own
        //   machinery owns those paths).
        let after = arm_end_anchor(&arm);
        let anchor_fires = match after.map(|a| tir.inst(a).tag) {
            Some(TirTag::Return | TirTag::ReturnVoid) => false,
            Some(TirTag::Break | TirTag::Continue) => true,
            _ => falls,
        };
        for (token, name) in &arm_binds {
            if let Some((_, span, _)) = own.pending_dead_store.remove(&Owner::Inst(*token)) {
                own.arm_dead_stores.push((*name, span));
                if anchor_fires && let Some(after) = after {
                    sidecar.free_schedule.push(FreePoint {
                        after,
                        target: *token,
                        span,
                        branch: Some(branch_id),
                    });
                }
            }
        }
        arm_results.push(own.take_branch(snap.clone()));
        falls_through.push(falls);
    }
    debug_assert_eq!(
        token_cursor,
        tokens.len(),
        "one owner token consumed per by-move binding"
    );

    // (f) Conditional divergence frees + union merge (the
    // `analyze_if_stmt` machinery verbatim): an owner moved in any
    // falling-through arm is moved after the match; an owner still
    // Valid on a sibling arm needs an arm-gated Free or its buffer
    // leaks on that arm's path.
    schedule_arm_divergence_frees(
        tir,
        pool,
        own,
        sidecar,
        &view,
        &arm_ids,
        &arm_results,
        &falls_through,
    );
    own.merge_branches(arm_results, &falls_through);
    // P4: a view whose last use is inside this match is dead at the
    // join on every path (subtree-based, same as the if prune).
    prune_branch_dead_projections(tir, own, r);
}

/// The statement refs of a statement-list arm body (`None` for an
/// expression arm).
fn arm_stmts(arm: &MatchArmView<'_>) -> Option<Vec<TirRef>> {
    match arm.body {
        MatchBodyView::Expr(_) => None,
        MatchBodyView::Stmts(_) => Some(arm.body.stmts().collect()),
    }
}

/// Each match arm's body statements in arm order — empty for an
/// expression arm (`MatchBodyView::Expr` decodes zero statement refs).
/// The Match-shaped descent every statement-list walker mirrors on its
/// `IfStmt` arms.
pub(crate) fn match_arm_bodies(tir: &Tir, r: TirRef) -> Vec<Vec<TirRef>> {
    tir.match_view(r)
        .arms()
        .map(|arm| arm.body.stmts().collect())
        .collect()
}

/// The `Match` instruction a statement-list slot refers to: the
/// statement itself when it is a bare `Match` (only future producers
/// emit one — sema always wraps), or the operand of an `ExprStmt`
/// wrapper — sema lowers a statement-level `match` to `ExprStmt(Match)`
/// (see `sema/stmt.rs`), so any statement-list walker that dispatches
/// on the statement's OWN tag would otherwise never see the wrapped
/// match's arms. Every walker below unwraps through this before its
/// tag dispatch.
pub(crate) fn stmt_match(tir: &Tir, r: TirRef) -> Option<TirRef> {
    match (tir.inst(r).tag, tir.inst(r).data) {
        (TirTag::Match, _) => Some(r),
        (TirTag::ExprStmt, TirData::UnOp(o)) if tir.inst(o).tag == TirTag::Match => Some(o),
        _ => None,
    }
}

/// Where an arm's flow ends: the anchor for arm-gated Frees of values
/// that die with the arm (the body expr for an expr arm, the last
/// statement for a statement arm).
fn arm_end_anchor(arm: &MatchArmView<'_>) -> Option<TirRef> {
    match arm.body {
        MatchBodyView::Expr(e) => Some(e),
        MatchBodyView::Stmts(_) => arm.body.stmts().last(),
    }
}

/// True when at least one path through the arm reaches the match's
/// merge block: an expression arm always yields; a statement arm falls
/// through unless it must-terminate (the `if_may_fall_through` sense).
fn arm_falls_through(tir: &Tir, arm: &MatchArmView<'_>) -> bool {
    match arm.body {
        MatchBodyView::Expr(_) => true,
        MatchBodyView::Stmts(_) => {
            let stmts: Vec<TirRef> = arm.body.stmts().collect();
            !body_must_terminate(tir, &stmts)
        }
    }
}

/// The arm's unbound needs-drop payload fields: the discriminating
/// shape's fields (the variant's payload for a variant arm; the anon
/// struct's fields for an anon-pattern arm — a `Wildcard` discriminant
/// with bindings) minus the fields the arm binds. Literal/wildcard arms
/// bind nothing and never consume, so the caller never reaches this for
/// them.
fn unbound_payload_fields(
    pool: &InternPool,
    scrutinee_ty: TypeId,
    arm: &MatchArmView<'_>,
) -> Vec<(u32, TypeId)> {
    let fields: Vec<StructField> = match arm.discriminant {
        ryo_core::tir::Discriminant::Variant(vidx) => {
            let eview = pool.enum_view(scrutinee_ty);
            let variant = eview
                .variants()
                .nth(vidx as usize)
                .expect("sema resolved the variant index against the scrutinee enum");
            variant.fields.to_vec()
        }
        // Anon-pattern arm: `Wildcard` coverage placeholder + bindings
        // + literal guards (sema's E0136 gated the scrutinee to an
        // anonymous struct for this shape).
        _ => pool.struct_view(scrutinee_ty).fields.to_vec(),
    };
    fields
        .into_iter()
        .filter(|f| pool.needs_drop(f.ty))
        .filter(|f| !arm.bindings().any(|b| b.field_index == f.idx))
        .map(|f| (f.idx, f.ty))
        .collect()
}

/// Branch-gated Frees for owners that diverge across arms: moved in at
/// least one falling-through arm (⇒ moved at the merge, so the exit
/// passes skip them) but still `Valid` at the end of a sibling arm —
/// the sibling's buffer leaks without an arm-gated Free. Owners live
/// only in arms that never move them are left to the usual passes
/// (last-use / dead-store / return epilogue), exactly the exemption the
/// else-less fall-through arm has in `analyze_if_stmt`.
#[allow(clippy::too_many_arguments)]
fn schedule_arm_divergence_frees(
    tir: &Tir,
    pool: &InternPool,
    own: &Ownership,
    sidecar: &mut FunctionSidecar,
    view: &ryo_core::tir::MatchView<'_>,
    arm_ids: &[BranchId],
    arm_results: &[BranchState],
    falls_through: &[bool],
) {
    struct ArmInfo<'a> {
        branch_id: BranchId,
        last: Option<TirRef>,
        state: &'a BranchState,
        falls_through: bool,
    }
    let arms: Vec<ArmInfo<'_>> = view
        .arms()
        .enumerate()
        .map(|(i, arm)| ArmInfo {
            branch_id: arm_ids[i],
            last: arm_end_anchor(&arm),
            state: &arm_results[i],
            falls_through: falls_through[i],
        })
        .collect();
    let all_keys: HashSet<Owner> = arms
        .iter()
        .flat_map(|a| a.state.states.keys().copied())
        .collect();
    // Sorted iteration for deterministic free_schedule order.
    let mut all_keys: Vec<Owner> = all_keys.into_iter().collect();
    all_keys.sort_by_key(owner_sort_key);
    for owner in all_keys {
        let is_tracked = match owner {
            Owner::Inst(_) => true,
            Owner::Param(name) => {
                let idx = super::param_idx(&own.param_index, name);
                needs_tracking(tir.params[idx].ty, pool)
            }
        };
        if !is_tracked {
            continue;
        }
        let any_moved = arms
            .iter()
            .filter(|a| a.falls_through)
            .any(|a| matches!(a.state.states.get(&owner), Some(OwnerState::Moved { .. })));
        if !any_moved {
            continue;
        }
        for arm in arms.iter().filter(|a| a.falls_through) {
            if matches!(arm.state.states.get(&owner), Some(OwnerState::Valid))
                && let Some(after) = arm.last
            {
                let span = match owner {
                    Owner::Inst(r) => tir.span(r),
                    Owner::Param(name) => {
                        let idx = super::param_idx(&own.param_index, name);
                        tir.params[idx].span
                    }
                };
                sidecar.free_schedule.push(FreePoint {
                    after,
                    target: owner.tirref(&own.param_index),
                    span,
                    branch: Some(arm.branch_id),
                });
            }
        }
    }
}
