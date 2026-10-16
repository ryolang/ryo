//! M12 match analysis over the `Switch` terminator — split from
//! `walk.rs`; see module docs there.
//!
//! A typed `TirTag::Match` is a discriminated join over per-arm
//! branches: each arm walks from the pre-match snapshot independently
//! (`snapshot_branch` / `take_branch`), and the post-match state is the
//! union over arms (`merge_branches`). The Switch's arm blocks (wire
//! order, wildcard included) are the arm identities — a statement
//! position match and a value-position match are structurally
//! identical, so the statement-vs-value special-casing of the old TIR
//! walk deletes itself.
//!
//! What match adds over if/elif/else:
//!
//! * payload bindings become fresh owners keyed on the by-move owner
//!   tokens (`Tir::match_by_move_owner_refs`, the M10 destructure
//!   convention) — arm-local, seeded before the arm body walks;
//! * an arm binding at least one non-`Copy` field by move consumes the
//!   SCRUTINEE on that arm's path (the whole-value model);
//! * on an OWNED scrutinee, the consuming arm's unbound needs-drop
//!   payload fields (`Some(_)` positions) register as arm-start drops
//!   (one `DropOp` per consuming arm, target = the scrutinee, keyed by
//!   the arm's Flow IR block) on the unified cleanup schedule;
//! * arm-local dead binds are swept at arm end into arm-gated Frees;
//! * arm-result transfer: a non-diverging aggregate value arm's
//!   body owner is consumed into the match inst (mirroring
//!   `analyze_var_decl`'s consume + rebind), so the result slot's
//!   buffer has exactly one destruction obligation — the consumer's;
//! * an aggregate-typed match result can never ride a borrowed owner
//!   (E0138, the borrowed-result family — unchanged guard).

use super::{
    BranchState, HazardKind, Owner, OwnerState, Ownership, RegionStop, Walk,
    check_source_projected, check_use_moved, consume_for_assignment, consumed_binding_name,
    drain_dying_views, inout_escape_owner, needs_tracking, owner_sort_key,
    prune_branch_dead_projections, refine_view_liveness_for_arm, register_pending_dead_store,
    restore_view_last_use,
};
use ryo_core::diag::{Diag, DiagCode, DiagSink};
use ryo_core::flow::{BlockId, DropOp, DropPosition, PlaceId, Terminator, WriteKind};
use ryo_core::ownership::FunctionSidecar;
use ryo_core::tir::{ChildKind, MatchArmView, MatchBodyView, Tir, TirData, TirRef, TirTag};
use ryo_core::types::{InternPool, StringId, StructField, TypeId};
use std::collections::HashSet;

/// `visit_block`'s `Switch` dispatch (M12). The scrutinee's steps ran
/// in `sw` before this dispatch; each arm then walks from the
/// pre-match snapshot. Diverging (never-typed) matches have no join —
/// `None` tells the caller there is no continuation. Errored matches
/// never reach here — sema recovers with the error-typed `Unreachable`
/// sentinel.
pub(crate) fn analyze_match_switch(
    w: &mut Walk<'_>,
    own: &mut Ownership,
    sink: &mut DiagSink,
    sidecar: &mut FunctionSidecar,
    sw: BlockId,
    match_inst: TirRef,
    epoch: u32,
) -> Option<BlockId> {
    let view = w.tir.match_view(match_inst);
    if view.is_empty() {
        // Defensive: sema diagnoses source-level; well-formed TIR
        // always has at least the catch-all arm.
        return None;
    }
    let scrutinee = view.scrutinee;
    let scrutinee_ty = w.tir.inst(scrutinee).ty;
    let tracked = needs_tracking(scrutinee_ty, w.pool);
    // Whether ANY arm binds a non-`Copy` field by move — the consume
    // authority for the scrutinee.
    let any_consumes = view
        .arms()
        .flat_map(|arm| arm.bindings())
        .any(|b| !w.pool.is_copy(b.ty));
    // P4 lift (final spec §3.2), mirroring the if-condition drain: a
    // projection whose last use is a read inside the scrutinee is dead
    // before any arm runs.
    drain_dying_views(own);
    if tracked && !any_consumes {
        // No arm consumes: nothing else will check the scrutinee read —
        // a moved scrutinee is a use-after-move here.
        check_use_moved(w.tir, w.pool, own, sink, scrutinee, w.tir.span(scrutinee));
    }
    // Borrow-ness is sourced from OWNERSHIP's own lattice (unchanged).
    let scrutinee_owner = own.underlying_owner(scrutinee);
    let borrowed = matches!(own.states.get(&scrutinee_owner), Some(OwnerState::Borrowed));
    // The borrowed-source family keeps its unchanged guards.
    let aggregate_result = w.pool.needs_drop(w.tir.inst(match_inst).ty);

    // The arm blocks (wire order) and the join from the Switch.
    let Terminator::Switch {
        arms: rng, join, ..
    } = *w.flow.term(sw)
    else {
        unreachable!("match is a switch")
    };
    let arm_blocks: Vec<BlockId> = rng.as_range().map(|i| BlockId(w.flow.extra[i])).collect();

    // By-move owner tokens, arm order then binding order (the
    // `TirBuilder::match_expr` convention) — and their Flow places
    // (same order, non-`Copy` only), the owner identities.
    let tokens = w.tir.match_by_move_owner_refs(match_inst, w.pool);
    let token_places = w.flow.token_places(match_inst);

    // Arm regions + the whole-match block set, precomputed for the
    // per-arm freeze refinement and the join-time prune (block
    // membership replaces the old `collect_reachable` subtree sets).
    let stop = match join {
        Some(j) => RegionStop::At(j),
        None => RegionStop::At(BlockId::NONE),
    };
    let mut match_blocks: Vec<BlockId> = vec![sw];
    let mut arm_regions: Vec<Vec<BlockId>> = Vec::with_capacity(arm_blocks.len());
    for &ab in &arm_blocks {
        let (region, _) =
            super::collect_region(w.flow, &mut w.region_scratch, ab, |b| stop.kind(b));
        match_blocks.extend_from_slice(&region);
        arm_regions.push(region);
    }
    let match_set: HashSet<BlockId> = match_blocks.iter().copied().collect();

    let snap = own.snapshot_branch();
    let mut arm_results: Vec<BranchState> = Vec::with_capacity(view.len());
    let mut falls_through: Vec<bool> = Vec::with_capacity(view.len());
    let mut arm_lasts: Vec<Option<TirRef>> = Vec::with_capacity(view.len());
    let mut token_cursor = 0;
    for (arm_index, arm) in view.arms().enumerate() {
        // (a) Seed the arm's payload bindings as fresh owners keyed on
        // their owner tokens — arm-local. Copy binds need no owner.
        let mut arm_binds: Vec<(TirRef, PlaceId, StringId)> = Vec::new();
        for b in arm.bindings() {
            own.binding_of_name.insert(b.name, match_inst);
            if w.pool.is_copy(b.ty) {
                continue;
            }
            let token = tokens[token_cursor];
            let token_place = token_places[token_cursor];
            token_cursor += 1;
            let span = w.tir.span(match_inst);
            // The token's place is both the binding's identity (Flow
            // binds the payload name directly to the token place, so
            // reads of it resolve without a name lookup) and the owner
            // of the moved payload field.
            let owner = Owner(token_place);
            own.states.insert(owner, OwnerState::Valid);
            register_pending_dead_store(own, owner, b.name, span, match_inst);
            arm_binds.push((token, token_place, b.name));
        }
        // (b) Consumption: an arm binding a non-`Copy` field by move
        // consumes the scrutinee on this arm's path.
        let consumes = !arm_binds.is_empty();
        if consumes && tracked {
            let span = w.tir.span(match_inst);
            let consumed_name = consumed_binding_name(w.tir, scrutinee);
            check_source_projected(
                w.tir,
                w.flow,
                w.pool,
                own,
                sink,
                scrutinee_owner,
                span,
                "move",
                consumed_name,
            );
            consume_for_assignment(
                w.tir,
                w.pool,
                own,
                sink,
                scrutinee,
                span,
                consumed_name,
                match_inst,
            );
        }
        // (c) Arm-start drops: on an OWNED scrutinee, the consuming
        // arm's unbound needs-drop payload fields are destroyed when
        // the arm starts. Registered on the unified DropOp schedule as
        // ONE block-start drop per arm, target = the scrutinee: codegen
        // recomputes the arm's unbound fields at the fire point (the
        // same `unbound_payload_fields` derivation) and drops each —
        // keeping the target a TirRef the free path can resolve while
        // the field list stays derivable from the typed match wire.
        if consumes && !borrowed && !unbound_payload_fields(w.pool, scrutinee_ty, &arm).is_empty() {
            sidecar.drops.push(DropOp {
                position: DropPosition::BlockStart {
                    block: arm_blocks[arm_index],
                },
                target: scrutinee,
                span: w.tir.span(scrutinee),
            });
        }
        // (d) The E0138 borrowed-source guard runs BEFORE the arm body
        // steps (it inspects the pre-arm lattice).
        if let MatchBodyView::Expr(e) = arm.body {
            check_arm_result_borrowed(w.tir, own, sink, e, aggregate_result);
        }
        // (e) Per-arm freeze refinement, then the body walk from the
        // pre-match snapshot.
        let arm_set: HashSet<BlockId> = arm_regions[arm_index].iter().copied().collect();
        let analysis = own.arm_last_reads.get(&arm_blocks[arm_index]).cloned();
        let saved =
            refine_view_liveness_for_arm(own, w.flow, &match_set, &arm_set, analysis.as_ref());
        let saved_last = w.last_inst;
        w.last_inst = None;
        w.walk_block_list(own, sink, sidecar, &arm_regions[arm_index], epoch);
        let arm_last = w.last_inst;
        w.last_inst = saved_last;
        restore_view_last_use(own, saved);
        let arm_exit = arm_regions[arm_index].iter().find_map(|&block| {
            matches!(
                w.flow.term(block),
                Terminator::Return { .. }
                    | Terminator::ReturnVoid
                    | Terminator::Break { .. }
                    | Terminator::Continue { .. }
            )
            .then_some(block)
        });
        // (e2) arm-result transfer: a non-diverging aggregate
        // value arm's body is consumed into the match inst, so the
        // result slot's buffer is owned by exactly one identity — the
        // match inst the result's consumers (VarDecl/Assign/Return/
        // call arg) then move on. Runs after the body steps (the body
        // owner must be evaluated) and before the dead-bind sweep (the
        // consume already removed a transferred bind's pending entry).
        if let MatchBodyView::Expr(e) = arm.body {
            // E0138's borrowed family was already rejected at (d) —
            // skip the consume there or the same arm would double-report
            // (E0138 + the transfer's E0021).
            if aggregate_result
                && needs_tracking(w.tir.inst(e).ty, w.pool)
                && !arm_result_borrows_caller(w.tir, own, e)
            {
                transfer_arm_result(w, own, sink, match_inst, e);
            }
        }
        let falls = arm_reaches_stop(w, &arm_regions[arm_index], join);
        // (f) Dead arm-local binds: a tracked bind never read nor
        // consumed inside the arm dies with the arm. Always remove the
        // pending entry and buffer the W0001; the arm-gated Free is
        // scheduled only where the anchor actually executes:
        // * a Return end — the return epilogue owns the exit;
        // * a Break/Continue end — codegen fires due Frees before the
        //   jump, and the loop-exit pass dedups against it;
        // * any other end — only when the arm falls through.
        let anchor_fires = match arm_last.map(|a| w.tir.inst(a).tag) {
            Some(TirTag::Return | TirTag::ReturnVoid) => false,
            Some(TirTag::Break | TirTag::Continue) => true,
            _ => match arm_exit.map(|block| w.flow.term(block)) {
                Some(Terminator::Return { .. } | Terminator::ReturnVoid) => false,
                Some(Terminator::Break { .. } | Terminator::Continue { .. }) => true,
                _ => falls,
            },
        };
        for (token_ref, token_place, name) in &arm_binds {
            if let Some((_, span, _)) = own.pending_dead_store.remove(&Owner(*token_place)) {
                own.arm_dead_stores.push((*name, span));
                if anchor_fires {
                    let position = if let Some(after) = arm_last {
                        Some(DropPosition::AfterStep { inst: after })
                    } else {
                        arm_exit.map(|block| DropPosition::PreTerminator { block })
                    };
                    if let Some(position) = position {
                        sidecar.drops.push(DropOp {
                            position,
                            target: *token_ref,
                            span,
                        });
                    }
                }
            }
        }
        arm_results.push(own.take_branch(snap.clone()));
        falls_through.push(falls);
        arm_lasts.push(arm_last);
    }
    debug_assert_eq!(
        token_cursor,
        tokens.len(),
        "one owner token consumed per by-move binding"
    );

    // (g) Conditional divergence frees + union merge + join prune.
    schedule_arm_divergence_frees(
        w.tir,
        w.flow,
        w.pool,
        own,
        sidecar,
        &arm_lasts,
        &arm_regions,
        &arm_results,
        &falls_through,
    );
    own.merge_branches(arm_results, &falls_through);
    prune_branch_dead_projections(own, w.flow, &match_set);
    w.last_inst = Some(match_inst);
    join
}

/// True when the walked arm region's exit reaches the Switch's join.
fn arm_reaches_stop(w: &Walk<'_>, region: &[BlockId], join: Option<BlockId>) -> bool {
    let Some(join) = join else { return false };
    region.iter().any(|&b| {
        matches!(
            w.flow.term(b),
            Terminator::Goto { target } if *target == join
        )
    })
}

/// The E0138 borrowed-source guard for an expression arm body: an
/// aggregate-typed arm body rooted in a BORROWED owner hands
/// caller-owned storage to the result binding (whose cleanup would
/// free it), so the shape stays rejected. Runs before the arm body
/// steps (it inspects the pre-arm lattice). Owned arm bodies — producer
/// temps, by-value reads of tracked bindings, by-move payload binds —
/// are legalized by the arm-result transfer instead
/// (`transfer_arm_result`).
fn check_arm_result_borrowed(
    tir: &Tir,
    own: &Ownership,
    sink: &mut DiagSink,
    e: TirRef,
    aggregate_result: bool,
) {
    if !aggregate_result || !arm_result_borrows_caller(tir, own, e) {
        return;
    }
    sink.emit(
        Diag::error(
            tir.span(e),
            DiagCode::MatchValueArmBorrowed,
            "match value arms cannot yield values borrowed from the caller",
        )
        .with_help(
            "yield an owned value in the arm, or restructure with a \
             `mut` binding assigned in an if/else",
        ),
    );
}

/// Arm-result transfer: consume a non-diverging aggregate value
/// arm's body owner into the match inst, mirroring `analyze_var_decl`'s
/// `consume_for_assignment` + `rebind_to_init`. Before the transfer the
/// arm-body owner is `Valid` and the match inst owns nothing; after it
/// the arm-body owner is `Moved { moved_at }` — which removes BOTH
/// destruction obligations the result slot would otherwise alias (the
/// producer temp's Free anchored on the Match inst, and the binding /
/// payload-token last-use Free) — and the match inst is the single
/// `Valid` owner the result's consumers then move on (eager destruction
/// at the consumer's last use, unchanged semantics).
///
/// Two body shapes get special handling:
///
/// * A `FieldAccess` chain body (`true: p.name`) moves its ROOT
///   binding whole — fields move only with the struct, and the result
///   slot aliases the field's buffer exactly like a whole move, so
///   `consume_underlying`'s `MoveOutOfField` guard (which sanctions
///   the field read for `analyze_var_decl`) is skipped here; the
///   consumed root is the identity.
/// * The match inst is seeded into `temp_owners`: consumed results are
///   skipped by the anon-temp pass (a named init/value), while a
///   DISCARDED aggregate match result (a statement-level value match)
///   still gets its one Free after the wrapping statement.
fn transfer_arm_result(
    w: &Walk<'_>,
    own: &mut Ownership,
    sink: &mut DiagSink,
    match_inst: TirRef,
    e: TirRef,
) {
    let span = w.tir.span(match_inst);
    let consumed_name = consumed_binding_name(w.tir, e);
    // The arm body's read fact names the root place for FieldAccess
    // chains (Flow resolves the chain at build); producers resolve to
    // their temp. `underlying_owner` covers both.
    let owner = own.underlying_owner(e);
    // P2 freeze (final spec §3.2): the consume moves the owner.
    check_source_projected(
        w.tir,
        w.flow,
        w.pool,
        own,
        sink,
        owner,
        span,
        "move",
        consumed_name,
    );
    // The consume transition proper — `consume_underlying` verbatim,
    // minus the `MoveOutOfField` guard (the FieldAccess family above is
    // the sanctioned whole-root move).
    let mut state = own
        .states
        .get(&owner)
        .cloned()
        .unwrap_or(OwnerState::NotTracked);
    // A Valid value currently bound to an `inout str` param escapes
    // through the write-back pointer — treat it as borrowed at consume
    // sites (mirrors `consume_underlying`).
    if matches!(state, OwnerState::Valid) && inout_escape_owner(own, owner) {
        state = OwnerState::Borrowed;
    }
    match state {
        OwnerState::Valid => {
            own.pending_dead_store.remove(&owner);
            own.states
                .insert(owner, OwnerState::Moved { moved_at: span });
            // W0003 case-B support: the arm-result transfer is a
            // move-hazard on the owner, anchored at the consuming
            // match. (The validator models it as a transfer IN: the
            // arm's value moves into the match result.)
            own.owner_hazards
                .push((owner, match_inst, HazardKind::Move));
        }
        OwnerState::Borrowed => {
            sink.emit(
                Diag::error(
                    span,
                    DiagCode::MoveOutOfBorrowedParam,
                    format!(
                        "cannot move out of borrowed parameter {}",
                        super::format_binding(consumed_name, w.pool),
                    ),
                )
                .with_help("add `move` to the parameter declaration if you need ownership"),
            );
        }
        OwnerState::Moved { .. } => {
            check_use_moved(w.tir, w.pool, own, sink, e, span);
        }
        OwnerState::NotTracked => {}
    }
    // The match result becomes the single owner of the merged result —
    // the `rebind_to_init` half of the mirror.
    let result = w
        .flow
        .match_result(match_inst)
        .expect("aggregate match result place");
    let owner = Owner(result);
    own.states.insert(owner, OwnerState::Valid);
    own.temp_owners.insert(owner);
}

/// True when an aggregate-typed expression arm's body reads (a field
/// projection of) a BORROWED owner — the borrowed-source family.
fn arm_result_borrows_caller(_tir: &Tir, own: &Ownership, e: TirRef) -> bool {
    // The body's read fact resolves FieldAccess chains to their root
    // place; producers and plain reads resolve to themselves.
    let root = own.underlying_owner(e);
    let state = own
        .states
        .get(&root)
        .cloned()
        .unwrap_or(OwnerState::NotTracked);
    matches!(state, OwnerState::Borrowed)
        || (matches!(state, OwnerState::Valid) && inout_escape_owner(own, root))
}

/// The arm's unbound needs-drop payload fields: the discriminating
/// shape's fields minus the fields the arm binds.
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
        _ => pool.struct_view(scrutinee_ty).fields.to_vec(),
    };
    fields
        .into_iter()
        .filter(|f| pool.needs_drop(f.ty))
        .filter(|f| !arm.bindings().any(|b| b.field_index == f.idx))
        .map(|f| (f.idx, f.ty))
        .collect()
}

/// Arm-conditional Frees for owners that diverge across arms: moved in
/// at least one falling-through arm (⇒ moved at the merge) but still
/// `Valid` at the end of a sibling arm — the sibling's buffer leaks
/// without a Free anchored inside that arm. Owners live only in arms
/// that never move them are left to the usual passes, exactly the
/// exemption the else-less fall-through arm has in `analyze_if_cascade`.
#[allow(clippy::too_many_arguments)]
fn schedule_arm_divergence_frees(
    tir: &Tir,
    flow: &ryo_core::flow::Flow,
    pool: &InternPool,
    pre_match: &Ownership,
    sidecar: &mut FunctionSidecar,
    arm_lasts: &[Option<TirRef>],
    arm_regions: &[Vec<BlockId>],
    arm_results: &[BranchState],
    falls_through: &[bool],
) {
    let mut all_keys: HashSet<Owner> = arm_results
        .iter()
        .flat_map(|a| a.states.keys().copied())
        .collect();
    // Sorted iteration for deterministic drops order.
    let mut all_keys: Vec<Owner> = all_keys.drain().collect();
    all_keys.sort_by_key(owner_sort_key);
    for owner in all_keys {
        let is_tracked = match flow.place(owner.0) {
            ryo_core::flow::Place::Param { index, .. } => {
                needs_tracking(tir.params[*index as usize].ty, pool)
            }
            _ => true,
        };
        if !is_tracked {
            continue;
        }
        let any_moved = arm_results
            .iter()
            .zip(falls_through)
            .any(|(a, &ft)| ft && matches!(a.states.get(&owner), Some(OwnerState::Moved { .. })));
        if !any_moved {
            continue;
        }
        for (i, a) in arm_results.iter().enumerate() {
            if !falls_through[i] {
                continue;
            }
            // A binding-aware nested merge can write `Valid` back onto
            // the pre-match owner after every path has reseated the
            // binding. That state describes the new value in the home
            // slot, not the allocation represented by `owner`; freeing
            // it here would release the replacement before its later
            // use. The replacement's own owner and the post-match
            // binding cleanup retain its destruction obligation.
            let is_reseated = pre_match.current_owner.iter().any(|(&home, &before)| {
                before == owner
                    && arm_regions[i].iter().any(|&block| {
                        flow.block_steps(block).iter().any(|step| {
                            step.facts.write.is_some_and(|write| {
                                write.place == home && write.kind == WriteKind::Replace
                            })
                        })
                    })
            });
            if matches!(a.states.get(&owner), Some(OwnerState::Valid))
                && !is_reseated
                && let Some(after) = arm_lasts[i]
            {
                sidecar.drops.push(DropOp {
                    position: DropPosition::AfterStep { inst: after },
                    target: owner.tirref(flow),
                    span: owner.span(flow, tir),
                });
            }
        }
    }
}

// ---------- Kept TIR-shape match helpers ----------
//
// The post-walk scheduling passes (loop-exit frees, return epilogue,
// W0003/W0004) stay statement-recursive and still need to recognize
// statement-level and value-position matches. The walk itself drives
// from Flow IR blocks, where both shapes are one `Switch` and no
// unwrap/memo machinery is needed.

/// Each match arm's body statements in arm order — empty for an
/// expression arm. The Match-shaped descent every kept statement-list
/// walker mirrors on its `IfStmt` arms.
pub(crate) fn match_arm_bodies(tir: &Tir, r: TirRef) -> Vec<Vec<TirRef>> {
    tir.match_view(r)
        .arms()
        .map(|arm| arm.body.stmts().collect())
        .collect()
}

/// The `Match` instruction a statement-list slot refers to: the
/// statement itself when it is a bare `Match`, or the operand of an
/// `ExprStmt` wrapper — sema lowers a statement-level `match` to
/// `ExprStmt(Match)`. Every kept statement-list walker unwraps through
/// this before its tag dispatch.
pub(crate) fn stmt_match(tir: &Tir, r: TirRef) -> Option<TirRef> {
    // Match-free bodies cannot contain a statement-level match: the
    // builder-stamped flag answers without decoding the instruction.
    if !tir.contains_match {
        return None;
    }
    let inst = tir.inst(r);
    match (inst.tag, inst.data) {
        (TirTag::Match, _) => Some(r),
        (TirTag::ExprStmt, TirData::UnOp(o)) if tir.inst(o).tag == TirTag::Match => Some(o),
        _ => None,
    }
}

/// Visit every `Match` nested in `r`'s expression operands — a
/// `VarDecl` initializer, an `Assign` value, a `Return` operand, a
/// call argument — at any depth. The walk continues through every
/// operand (including each found match's scrutinee and expression-arm
/// bodies) so multiple/nested operand matches are ALL visited; statement
/// arm bodies are deliberately skipped, because the callback owns those
/// via `match_arm_bodies` and its own statement recursion.
///
/// Two gates keep match-free bodies at ~zero cost: the
/// builder-stamped [`Tir::contains_match`] flag rejects them in one
/// load, and once a body is known to contain matches, every descent is
/// gated on [`Tir::subtree_has_match`] — the shared per-body memo.
pub(crate) fn visit_operand_matches(tir: &Tir, r: TirRef, f: &mut impl FnMut(TirRef)) {
    if !tir.contains_match {
        return;
    }
    fn visit(tir: &Tir, memo: &[bool], r: TirRef, f: &mut impl FnMut(TirRef)) {
        if !memo[r.index()] {
            return;
        }
        tir.walk_operands(r, &mut |_parent, child, kind| {
            if tir.inst(child).tag == TirTag::Match {
                f(child);
            }
            if matches!(kind, ChildKind::Operand) {
                visit(tir, memo, child, f);
            }
        });
    }
    visit(tir, tir.operand_match_memo(), r, f);
}

// ---------- If-arm region machinery ----------

/// Per-arm walker for `analyze_if_cascade`: refine the freeze state
/// for the arm (when the liveness pre-pass recorded a per-arm map for
/// it — `refine_key`), walk the arm's region blocks from the pre-branch
/// snapshot, restore, and record the arm end state plus whether the arm
/// reaches the join. The per-arm analysis map is cloned out first (arm
/// maps are small; refine needs `&mut Ownership`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn walk_if_arm(
    w: &mut Walk<'_>,
    own: &mut Ownership,
    sink: &mut DiagSink,
    sidecar: &mut FunctionSidecar,
    arm_regions: &[Vec<BlockId>],
    cascade_set: &HashSet<BlockId>,
    snap: &BranchState,
    arm_index: usize,
    refine_key: Option<BlockId>,
    join: BlockId,
    epoch: u32,
) -> (BranchState, bool, Option<TirRef>) {
    if let Some(key) = refine_key {
        let arm_set: HashSet<BlockId> = arm_regions[arm_index].iter().copied().collect();
        let analysis = own.arm_last_reads.get(&key).cloned();
        let saved =
            refine_view_liveness_for_arm(own, w.flow, cascade_set, &arm_set, analysis.as_ref());
        let saved_last = w.last_inst;
        w.last_inst = None;
        w.walk_block_list(own, sink, sidecar, &arm_regions[arm_index], epoch);
        let arm_last = w.last_inst;
        w.last_inst = saved_last;
        restore_view_last_use(own, saved);
        (
            own.take_branch(snap.clone()),
            arm_reaches_join(w, &arm_regions[arm_index], join),
            arm_last,
        )
    } else {
        let saved_last = w.last_inst;
        w.last_inst = None;
        w.walk_block_list(own, sink, sidecar, &arm_regions[arm_index], epoch);
        let arm_last = w.last_inst;
        w.last_inst = saved_last;
        (
            own.take_branch(snap.clone()),
            arm_reaches_join(w, &arm_regions[arm_index], join),
            arm_last,
        )
    }
}

/// True when the walked arm region's exit reaches `join` (the arm's
/// last live block jumps there). Arms whose every live path diverges
/// return false; their end-state is path-local to an exit the join
/// never sees.
pub(crate) fn arm_reaches_join(w: &Walk<'_>, region: &[BlockId], join: BlockId) -> bool {
    region.iter().any(|&b| {
        matches!(
            w.flow.term(b),
            Terminator::Goto { target } if *target == join
        )
    })
}
