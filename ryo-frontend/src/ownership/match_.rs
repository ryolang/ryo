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
//!   SCRUTINEE on that arm's path (the whole-value model), except that
//!   shared direct-catch arms own independent payload copies and leave
//!   the original error alive;
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
    prune_branch_dead_projections, refine_view_liveness_for_arm, restore_view_last_use,
};
use ryo_core::diag::{Diag, DiagCode, DiagSink};
use ryo_core::flow::{BlockId, DropOp, DropPosition, Place, PlaceId, Terminator, WriteKind};
use ryo_core::ownership::FunctionSidecar;
use ryo_core::tir::{ChildKind, MatchArmView, MatchBodyView, Tir, TirData, TirRef, TirTag};
use ryo_core::types::{InternPool, StructField, TypeId};
use std::collections::HashSet;

/// `visit_block`'s `Switch` dispatch (M12). The scrutinee's steps ran
/// in `sw` before this dispatch; each arm then walks from the
/// pre-match snapshot. Diverging (never-typed) matches have no join —
/// `None` tells the caller there is no continuation. Errored matches
/// never reach here — sema recovers with the error-typed `Unreachable`
/// sentinel.
pub(crate) struct MatchFrame<'a> {
    match_inst: TirRef,
    arms: Box<[MatchArmView<'a>]>,
    scrutinee: TirRef,
    scrutinee_ty: TypeId,
    tracked: bool,
    shared_catch: bool,
    borrowed: bool,
    aggregate_result: bool,
    scrutinee_owner: Owner,
    sw: BlockId,
    pub(crate) join: Option<BlockId>,
    arm_blocks: Vec<BlockId>,
    tokens: Vec<TirRef>,
    token_places: Vec<PlaceId>,
    token_cursor: usize,
    snap: BranchState,
    arm_results: Vec<BranchState>,
    falls_through: Vec<bool>,
    arm_lasts: Vec<Option<TirRef>>,
    next_arm: usize,
    current_arm: Option<MatchArm>,
    epoch: u32,
    pub(crate) region_id: Option<usize>,
    pub(crate) region_summary: Option<super::walk::machine::FlowRegionSummary>,
    pub(crate) parent_region: Option<usize>,
    pub(crate) region_exit: Option<BlockId>,
}

struct MatchArm {
    binds: Vec<(TirRef, PlaceId)>,
    saved: Vec<(TirRef, TirRef)>,
    saved_last: Option<TirRef>,
}

fn match_arm_region(w: &mut Walk<'_>, entry: BlockId, join: Option<BlockId>) -> Vec<BlockId> {
    let stop = RegionStop::At(join.unwrap_or(BlockId::NONE));
    super::collect_region(w.flow, &mut w.region_scratch, entry, |b| stop.kind(b)).0
}

fn match_block_set(
    w: &mut Walk<'_>,
    sw: BlockId,
    arm_blocks: &[BlockId],
    join: Option<BlockId>,
) -> HashSet<BlockId> {
    let mut blocks = vec![sw];
    for &entry in arm_blocks {
        blocks.extend(match_arm_region(w, entry, join));
    }
    blocks.into_iter().collect()
}

impl<'a> MatchFrame<'a> {
    pub(crate) fn new(
        w: &mut Walk<'a>,
        own: &mut Ownership,
        sink: &mut DiagSink,
        _sidecar: &mut FunctionSidecar,
        sw: BlockId,
        match_inst: TirRef,
        epoch: u32,
    ) -> (Option<Self>, Option<BlockId>) {
        let view = w.tir.match_view(match_inst);
        if view.is_empty() {
            return (None, None);
        }
        let scrutinee = view.scrutinee;
        let scrutinee_ty = w.tir.inst(scrutinee).ty;
        let tracked = needs_tracking(scrutinee_ty, w.pool);
        let any_consumes = view
            .arms()
            .flat_map(|arm| arm.bindings())
            .any(|b| !w.pool.is_copy(b.ty));
        drain_dying_views(own);
        if tracked && !any_consumes {
            check_use_moved(w.tir, w.pool, own, sink, scrutinee, w.tir.span(scrutinee));
        }
        let scrutinee_owner = own.underlying_owner(scrutinee);
        let borrowed = matches!(own.states.get(&scrutinee_owner), Some(OwnerState::Borrowed));
        let shared_catch = any_consumes && w.tir.is_direct_catch_match(match_inst);
        let aggregate_result = w.pool.needs_drop(w.tir.inst(match_inst).ty);
        let Terminator::Switch {
            arms: rng, join, ..
        } = *w.flow.term(sw)
        else {
            unreachable!("match is a switch")
        };
        let arm_blocks: Vec<BlockId> = rng.as_range().map(|i| BlockId(w.flow.extra[i])).collect();
        let tokens = w.tir.match_by_move_owner_refs(match_inst, w.pool);
        let token_places = w.flow.token_places(match_inst);
        if aggregate_result {
            let result = w
                .flow
                .match_result(match_inst)
                .expect("aggregate match result place");
            own.states.remove(&Owner(result));
        }
        let snap = own.snapshot_branch();
        (
            Some(Self {
                match_inst,
                arms: view.arms().collect(),
                scrutinee,
                scrutinee_ty,
                tracked,
                shared_catch,
                borrowed,
                aggregate_result,
                scrutinee_owner,
                sw,
                join,
                arm_blocks,
                tokens,
                token_places: token_places.to_vec(),
                token_cursor: 0,
                snap,
                arm_results: Vec::with_capacity(view.len()),
                falls_through: Vec::with_capacity(view.len()),
                arm_lasts: Vec::with_capacity(view.len()),
                next_arm: 0,
                current_arm: None,
                epoch,
                region_id: None,
                region_summary: None,
                parent_region: None,
                region_exit: None,
            }),
            join,
        )
    }

    pub(crate) fn advance(
        &mut self,
        w: &mut Walk<'_>,
        own: &mut Ownership,
        sink: &mut DiagSink,
        sidecar: &mut FunctionSidecar,
    ) -> super::walk::machine::WalkAdvance {
        if let Some(arm) = self.current_arm.take() {
            self.finish_arm(w, own, sink, sidecar, arm);
            self.next_arm += 1;
        }
        if self.next_arm == self.arm_blocks.len() {
            debug_assert_eq!(
                self.token_cursor,
                self.tokens.len(),
                "one owner token consumed per by-move binding"
            );
            schedule_arm_divergence_frees(
                w,
                self.join,
                &self.arm_blocks,
                own,
                sidecar,
                &self.arm_lasts,
                &self.arm_results,
                &self.falls_through,
            );
            own.merge_branches(std::mem::take(&mut self.arm_results), &self.falls_through);
            if !own.root_owner.is_empty() {
                let match_set = match_block_set(w, self.sw, &self.arm_blocks, self.join);
                prune_branch_dead_projections(own, w.flow, &match_set);
            }
            w.last_inst = Some(self.match_inst);
            return super::walk::machine::WalkAdvance::Done;
        }
        self.start_arm(w, own, sink, sidecar);
        super::walk::machine::WalkAdvance::flow(
            self.arm_blocks[self.next_arm],
            RegionStop::At(self.join.unwrap_or(BlockId::NONE)),
            self.epoch,
        )
    }

    fn start_arm(
        &mut self,
        w: &mut Walk<'_>,
        own: &mut Ownership,
        sink: &mut DiagSink,
        sidecar: &mut FunctionSidecar,
    ) {
        let arm_index = self.next_arm;
        let arm = self.arms[arm_index];
        let mut arm_binds: Vec<(TirRef, PlaceId)> = Vec::new();
        for b in arm.bindings() {
            own.binding_of_name.insert(b.name, self.match_inst);
            if w.pool.is_copy(b.ty) {
                continue;
            }
            let token = self.tokens[self.token_cursor];
            let token_place = self.token_places[self.token_cursor];
            self.token_cursor += 1;
            let owner = Owner(token_place);
            own.states.insert(owner, OwnerState::Valid);
            arm_binds.push((token, token_place));
        }
        let consumes = !arm_binds.is_empty();
        if consumes && self.tracked {
            let span = w.tir.span(self.match_inst);
            let consumed_name = consumed_binding_name(w.tir, self.scrutinee);
            check_source_projected(
                w.tir,
                w.flow,
                w.pool,
                own,
                sink,
                self.scrutinee_owner,
                span,
                "move",
                consumed_name,
            );
            if !self.shared_catch {
                consume_for_assignment(
                    w.tir,
                    w.pool,
                    own,
                    sink,
                    self.scrutinee,
                    span,
                    consumed_name,
                    self.match_inst,
                );
            }
        }
        if consumes
            && !self.shared_catch
            && !self.borrowed
            && !unbound_payload_fields(w.pool, self.scrutinee_ty, &arm).is_empty()
        {
            sidecar.drops.push(DropOp {
                position: DropPosition::BlockStart {
                    block: self.arm_blocks[arm_index],
                },
                target: self.scrutinee,
                span: w.tir.span(self.scrutinee),
            });
        }
        if let MatchBodyView::Expr(e) = arm.body {
            check_arm_result_borrowed(w.tir, own, sink, e, self.aggregate_result);
        }
        let saved = if own.root_owner.is_empty() {
            Vec::new()
        } else {
            let region = match_arm_region(w, self.arm_blocks[arm_index], self.join);
            let match_set = match_block_set(w, self.sw, &self.arm_blocks, self.join);
            let arm_set: HashSet<BlockId> = region.iter().copied().collect();
            let analysis = own.arm_last_reads.get(&self.arm_blocks[arm_index]).cloned();
            refine_view_liveness_for_arm(own, w.flow, &match_set, &arm_set, analysis.as_ref())
        };
        let saved_last = w.last_inst;
        w.last_inst = None;
        self.current_arm = Some(MatchArm {
            binds: arm_binds,
            saved,
            saved_last,
        });
    }

    fn finish_arm(
        &mut self,
        w: &mut Walk<'_>,
        own: &mut Ownership,
        sink: &mut DiagSink,
        sidecar: &mut FunctionSidecar,
        arm: MatchArm,
    ) {
        let arm_index = self.next_arm;
        let match_arm = self.arms[arm_index];
        let arm_last = w.last_inst;
        w.last_inst = arm.saved_last;
        restore_view_last_use(own, arm.saved);
        let summary = self.region_summary.take().expect("walked arm summary");
        if self.region_exit.is_none() {
            self.region_exit = summary.exit;
        }
        let arm_exit = summary.exit;
        if let MatchBodyView::Expr(e) = match_arm.body
            && self.aggregate_result
            && needs_tracking(w.tir.inst(e).ty, w.pool)
            && !arm_result_borrows_caller(w.tir, own, e)
        {
            transfer_arm_result(w, own, sink, self.match_inst, e);
        }
        let falls = summary.falls_through;
        let anchor_fires = match arm_last.map(|a| w.tir.inst(a).tag) {
            Some(TirTag::Return | TirTag::ReturnVoid) => false,
            Some(TirTag::Break | TirTag::Continue) => true,
            _ => match arm_exit.map(|block| w.flow.term(block)) {
                Some(Terminator::Return { .. } | Terminator::ReturnVoid) => false,
                Some(Terminator::Break { .. } | Terminator::Continue { .. }) => true,
                _ => falls,
            },
        };
        for (token_ref, token_place) in &arm.binds {
            if let Some((_, span, _, _)) = w.dead_stores.arm_dead_store(*token_place)
                && anchor_fires
            {
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
        if !falls
            && let Some(block) = arm_exit
            && matches!(
                w.flow.term(block),
                Terminator::Break { .. } | Terminator::Continue { .. }
            )
        {
            let mut local_owners: Vec<Owner> = w
                .flow
                .places
                .iter()
                .enumerate()
                .filter_map(|(index, place)| {
                    let Place::Local { decl, .. } = place else {
                        return None;
                    };
                    if !w
                        .flow
                        .block_steps(block)
                        .iter()
                        .any(|step| step.inst == *decl)
                    {
                        return None;
                    }
                    let owner = *own.current_owner.get(&PlaceId(index as u32))?;
                    matches!(own.states.get(&owner), Some(OwnerState::Valid)).then_some(owner)
                })
                .collect();
            local_owners.sort_by_key(owner_sort_key);
            local_owners.dedup();
            for owner in local_owners {
                let target = owner.tirref(w.flow);
                let already_scheduled = sidecar.drops.iter().any(|drop| {
                    if drop.target != target {
                        return false;
                    }
                    match drop.position {
                        DropPosition::AfterStep { inst } | DropPosition::BeforeStep { inst } => {
                            w.flow.step_block_of(inst) == Some(block)
                        }
                        DropPosition::BlockStart { block: at }
                        | DropPosition::PreTerminator { block: at } => at == block,
                    }
                });
                if !already_scheduled {
                    sidecar.drops.push(DropOp {
                        position: DropPosition::PreTerminator { block },
                        target,
                        span: owner.span(w.flow, w.tir),
                    });
                }
            }
        }
        self.arm_results.push(own.take_branch(self.snap.clone()));
        self.falls_through.push(falls);
        self.arm_lasts.push(arm_last);
    }
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
    w: &mut Walk<'_>,
    join: Option<BlockId>,
    arm_blocks: &[BlockId],
    pre_match: &Ownership,
    sidecar: &mut FunctionSidecar,
    arm_lasts: &[Option<TirRef>],
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
        let is_tracked = match w.flow.place(owner.0) {
            ryo_core::flow::Place::Param { index, .. } => {
                needs_tracking(w.tir.params[*index as usize].ty, w.pool)
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
            let needs_region = pre_match
                .current_owner
                .values()
                .any(|&before| before == owner);
            let region = needs_region.then(|| match_arm_region(w, arm_blocks[i], join));
            let is_reseated = pre_match.current_owner.iter().any(|(&home, &before)| {
                before == owner
                    && region.as_ref().is_some_and(|region| {
                        region.iter().any(|&block| {
                            w.flow.block_steps(block).iter().any(|step| {
                                step.facts.write.is_some_and(|write| {
                                    write.place == home && write.kind == WriteKind::Replace
                                })
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
                    target: owner.tirref(w.flow),
                    span: owner.span(w.flow, w.tir),
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
    enum Task {
        Visit(TirRef),
        Emit(TirRef),
    }
    let memo = tir.operand_match_memo();
    let mut stack = ryo_core::spine::ChainSpine::new();
    stack.push(Task::Visit(r));
    while let Some(task) = stack.pop() {
        match task {
            Task::Emit(r) => f(r),
            Task::Visit(r) => {
                if !memo[r.index()] {
                    continue;
                }
                let mut children = Vec::new();
                tir.walk_operands(r, &mut |_parent, child, kind| {
                    if kind == ChildKind::Operand {
                        children.push(child);
                    }
                });
                for child in children.into_iter().rev() {
                    stack.push(Task::Visit(child));
                    if tir.inst(child).tag == TirTag::Match {
                        stack.push(Task::Emit(child));
                    }
                }
            }
        }
    }
}
