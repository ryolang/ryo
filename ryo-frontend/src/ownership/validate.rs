//! Debug-only cleanup-completeness validation (backend validators, frontend half) —
//! the phase after ownership analysis, before the sidecar is handed to
//! codegen. Construction-time structural validation (flow/validate.rs)
//! checks the Flow IR itself; this validator checks the CLEANUP
//! CONTRACT between the ownership pass and the backend, as ONE unified
//! event model:
//!
//! **Identity** — place-UNION groups (union-find over home ↔ seated-owner
//! and scrutinee ↔ arm-token) plus GENERATIONS (a generation ends at any
//! member's Init/Replace/FieldReplace write, producer step, or token
//! adoption). In-place `Mutate` preserves identity; `WriteBack` is
//! non-splitting (a writeback that stored the same value back discharges
//! nothing — the conservative corner).
//!
//! **Events** — every executable fact that discharges (transfers) or
//! releases a generation, at a resolved firing point (the shared
//! `ryo_core::flow::fire_points` resolver — the backend's fire map
//! consumes the same resolutions):
//! - RELEASES: `Destroy` (a scheduled `DropOp`), `Displace`
//!   (`free_on_reassign` / `field_free_on_reassign` — the old content's
//!   release at a replacement store; a consumed-concat `Assign` skips its
//!   `free_on_reassign` free at codegen, so it is not an event), and
//!   `PromoRelease` (a promotion-buffer free — its OWN identity, never the
//!   borrowed original's).
//! - TRANSFERS: `TransferReturn` (a `Move` hazard at a Return — ownership
//!   moves to the caller; the callee must never release the transferred
//!   buffer), `TransferCall` (a move argument / match consume), `Consume`
//!   (an in-place consuming concat — the old buffer's ownership moves into
//!   the result), and `TransferInoutWriteback` (an inout param's seated
//!   value leaves through the write-back pointer at EVERY exit).
//!
//! **Checks** (each verdict is PROVEN VALID / PROVEN INVALID / NOT
//! PROVED, and the per-body [`ValidationSummary`] counts what was checked
//! vs not-proved — never silently "complete"):
//! 1. **Position resolution** — every event position resolves
//!    (`FireVerdict::Unresolved` is a validation error, never guessed).
//! 2. **No double destruction** — (a) same-(place, position) with DISTINCT
//!    targets both emit; (b) PATH-AWARE: a release reaching another
//!    release of the same union with no intervening generation write both
//!    fire on one path. A release reaching a TRANSFER (or vice versa) is
//!    the same violation — freeing storage whose ownership left the
//!    function (the return-in-loop UAF shape). Designed exemptions: the
//!    identical (target, position) twin (one bucket, deduped by codegen)
//!    and codegen's per-block-visit `freed_targets` same-target collapse
//!    (not on cycles, not across blocks). Static-literal targets are
//!    pre-elided no-ops (cap == 0 sentinel) — all-literal pairs pass.
//! 3. **Path coverage (must-analysis)** — a generation established by an
//!    Init/Replace/FieldReplace write, a producer step, a token adoption,
//!    or function entry (params) must be DISCHARGED on EVERY path to a
//!    function exit — by a RELEASE or a TRANSFER of its union. There is NO
//!    global escape predicate: a transfer at one site covers only the
//!    paths through it; a return-epilogue OBSERVATION is not a release; a
//!    final Moved / missing / unseated state is not proof of coverage.
//!    A generation-ending write is a discharge ONLY when the old
//!    generation's release fires on the path before the store (the
//!    write alone establishes the new generation; it never releases
//!    the old) — reaching the write unreleased is a LEAK and flags.
//!
//! Conservative corners (leak direction unless stated — unknown shapes
//! pass; flag only PROVABLE violations). The NOT PROVED categories
//! (each counted in the per-body [`ValidationSummary`], which the check
//! path propagates on `FunctionSidecar::validation` — the deterministic
//! accessor for tests/tooling; the `RYO_CLEANUP_SUMMARY` env printer is
//! a convenience only):
//! - Positions resolving `NonExact` (the resolver's unresolvable-loop
//!   fallback — a compound-condition header whose matching test is
//!   unreachable, i.e. a malformed body): the position, and every
//!   pair/obligation/view/promo check containing one, is not provable
//!   and is counted, never asserted.
//! - Events on all-diverging constructs (an empty `ResolvedPerArm`) never
//!   fire — excluded from the path checks.
//! - Static-literal targets (see check 2).
//! - Reaching a `Call{never}`/`Unreachable` terminator counts as covered
//!   (dead path, leak direction).
//! - Events/obligations/view checks in blocks unreachable from the entry
//!   are EXCLUDED and counted separately (`*_unreachable`) — unreachable
//!   code creates no obligation, a different category from unknown proof.
//! - Loop cycles use an optimistic must-fixpoint: a cycle without an
//!   in-cycle event stays covered (a forever-looping path leaks).
//! - View-backed storage: views protect the backing allocation but never
//!   discharge it — the base group's coverage runs through the same
//!   must-analysis (no exemption), and a release that can fire before a
//!   reachable view use — following loop-closing edges, so a release at
//!   the end of iteration N reaches a use at the start of iteration N+1 —
//!   is PROVEN INVALID (the UAF direction; the check lives in the
//!   `views` child module).
//!
//! Intentionally UNSUPPORTED internal shapes (each must surface as NOT
//! PROVED — counted, never silently Proven — so
//! `ValidationSummary::proof_complete` stays honest):
//! - A `NonExact` position (above) — nothing downstream of it proves.
//! - A view producer whose final use is not a Flow step (the
//!   `views_not_proved` count).
//! - A promotion alloc/release whose position is `NonExact`
//!   (the `promo_pairings_not_proved` count).
//!
//! All checks are debug-only, failing loudly (panic) on a PROVEN INVALID
//! verdict — mirroring the existing validate.rs style.

use super::{Ownership, flow_owner_place};
pub(crate) use ryo_core::flow::fire_points::FirePoint as Point;
use ryo_core::flow::fire_points::{FirePoints, FireVerdict};
use ryo_core::flow::{BlockId, DropPosition, Flow, Place, PlaceId, Terminator, WriteKind};
use ryo_core::ownership::FunctionSidecar;
pub(crate) use ryo_core::ownership::ValidationSummary;
use ryo_core::tir::{Tir, TirRef, TirTag};
use ryo_core::types::InternPool;
use std::collections::{HashMap, HashSet};

mod promotions;
mod replacements;
mod views;
pub(crate) use promotions::validate_promo;
pub(crate) use replacements::validate_displacement;
pub(crate) use views::validate_view_protection;

/// The shared read-only context every check entry point consumes:
/// the event model, the generation tables, and the shared
/// firing-point resolver, built once by the validation entry. One
/// argument instead of per-check plumbing; children read what they
/// need and never reach sideways (each depends only on this module's
/// shared model).
pub(crate) struct Ctx<'a> {
    pub(crate) tir: &'a Tir,
    pub(crate) flow: &'a Flow,
    pub(crate) own: &'a Ownership,
    pub(crate) sidecar: &'a FunctionSidecar,
    pub(crate) fire_points: &'a FirePoints<'a>,
    pub(crate) events: &'a HashMap<PlaceId, Vec<Event>>,
    pub(crate) table: &'a GenerationTable,
}

/// Validate the cleanup contract for one function body. Debug-only;
/// `check` invokes this after `analyze_function` per body, while the
/// final lattice state, the walk tables, and the sidecar are all live.
///
/// `body_clean` — the body produced no diagnostics. The path checks (d/e)
/// prove properties of the schedule the ownership pass guarantees only
/// for clean bodies; an errored body's best-effort schedule never reaches
/// codegen, so the flaggers skip it (documented in the header).
pub(crate) fn validate_cleanup(
    tir: &Tir,
    flow: &Flow,
    pool: &InternPool,
    own: &Ownership,
    sidecar: &FunctionSidecar,
    body_clean: bool,
) -> ValidationSummary {
    let places = TargetPlaces::build(tir, flow, pool);
    // The shared firing-point resolver: single authority for position
    // interpretation, consumed by position validation and the event
    // model alike.
    let fire_points = FirePoints::new(flow);
    let mut summary = ValidationSummary::default();
    validate_positions(flow, &fire_points, sidecar, &mut summary);
    if summary.invalid {
        return summary;
    }
    validate_no_duplicate_destruction(&places, flow, sidecar);
    let idx = StepIndex::build(flow);
    let events = collect_events(tir, &places, flow, &fire_points, sidecar, own, pool);
    let table = generation_table(tir, flow, pool, &idx, sidecar);
    #[cfg(test)]
    record_test_ownership(tir.name, own.clone());
    if body_clean && !summary.invalid {
        summary.required_checks_ran = true;
        let ctx = Ctx {
            tir,
            flow,
            own,
            sidecar,
            fire_points: &fire_points,
            events: &events,
            table: &table,
        };
        validate_no_path_double_destruction(tir, flow, own, sidecar, &events, &table, &mut summary);
        validate_path_coverage(tir, flow, pool, own, sidecar, &events, &table, &mut summary);
        validate_view_protection(&ctx, &mut summary);
        validate_displacement(&ctx, &mut summary);
        validate_promo(&ctx, &mut summary);
        // Debug-only tally surfacing: the NOT PROVED corners are listed
        // here (per body) instead of vanishing into a silent skip.
        if !summary.proof_complete() && std::env::var_os("RYO_CLEANUP_SUMMARY").is_some() {
            eprintln!(
                "[cleanup-summary] {}: positions {}+{}; pairs examined={} not_proved={};                  obligations={} not_proved={} unreachable={}; events unreachable={};                  views {}+{} unreachable={}; displacements={}; promo pairings={} not_proved={}",
                pool.str(tir.name),
                summary.positions,
                summary.positions_not_proved,
                summary.pairs_examined,
                summary.pairs_not_proved,
                summary.obligations,
                summary.obligations_not_proved,
                summary.obligations_unreachable,
                summary.events_unreachable,
                summary.view_checks,
                summary.views_not_proved,
                summary.views_unreachable,
                summary.displacements,
                summary.promo_pairings,
                summary.promo_pairings_not_proved
            );
        }
    }
    summary
}

/// An intra-body firing point is the canonical [`FirePoint`]
/// (`ryo_core::flow::fire_points`): a block plus an ordinal, where
/// ordinals run `0..=n_steps` — 0 is the block start (before step 0),
/// ordinal i fires between step i-1 and step i, and `n_steps` is the
/// terminator position (`PreTerminator`, `AfterStep` of the last step,
/// and payload-anchored drops the construct's handler fires).
///
/// Every stepped instruction → (block, ordinal). Built once per body:
/// generation writes resolve through it.
pub(crate) struct StepIndex {
    map: HashMap<TirRef, (BlockId, u32)>,
}

impl StepIndex {
    pub(crate) fn build(flow: &Flow) -> Self {
        let mut map = HashMap::new();
        for b in flow.forward_order() {
            for (i, s) in flow.block_steps(b).iter().enumerate() {
                debug_assert!(
                    map.insert(s.inst, (b, i as u32)).is_none(),
                    "one step per instruction"
                );
            }
        }
        StepIndex { map }
    }

    pub(crate) fn get(&self, inst: TirRef) -> Option<(BlockId, u32)> {
        self.map.get(&inst).copied()
    }
}

/// Map a drop position to its firing point(s) through the shared
/// resolver (`ryo_core::flow::fire_points` — the single authority both
/// the backend's fire map and this validator consume). Each point comes
/// with whether it fires EXACTLY there (see `Event::exact`): every
/// verdict shape is exact except the resolver's compound loop-condition
/// fallback. `ResolvedPerArm` yields one point per non-diverging arm
/// continuation (an IfStmt payload fires when the construct completes,
/// on whichever arm completed); an empty vec is a real shape — the
/// anchor can never fire (every arm diverges / no join) — and excludes
/// the event from the path checks. `Unresolved` has no firing point;
/// `validate_positions` records it as invalid, so these checks must not
/// use a guessed point.
pub(crate) fn position_points(fire_points: &FirePoints, pos: DropPosition) -> Vec<(Point, bool)> {
    match fire_points.resolve(pos) {
        FireVerdict::Resolved(p) => vec![(p, true)],
        FireVerdict::ResolvedPerArm(ps) => ps.into_iter().map(|p| (p, true)).collect(),
        FireVerdict::NonExact(p) => vec![(p, false)],
        // An unresolved position is reported by validate_positions and
        // makes the summary invalid. Do not invent a firing point here.
        FireVerdict::Unresolved => Vec::new(),
    }
}

#[cfg(test)]
thread_local! {
    /// Test-only capture of the final per-body ownership state (see
    /// `record_test_ownership`): tests corrupt the sidecar after
    /// `check` and re-run the path checks against this state.
    static TEST_OWNERSHIPS: std::sync::Mutex<
        Vec<(ryo_core::types::StringId, Ownership)>,
    > = const { std::sync::Mutex::new(Vec::new()) };
}

#[cfg(test)]
pub(crate) fn record_test_ownership(name: ryo_core::types::StringId, own: Ownership) {
    TEST_OWNERSHIPS.with(|v| {
        v.lock().expect("test ownerships lock").push((name, own));
    });
}

#[cfg(test)]
pub(crate) fn clear_test_ownerships() {
    TEST_OWNERSHIPS.with(|v| v.lock().expect("test ownerships lock").clear());
}

/// Test-only: take the captured ownership state for `name`.
#[cfg(test)]
pub(crate) fn take_test_ownership(name: ryo_core::types::StringId) -> Option<Ownership> {
    TEST_OWNERSHIPS.with(|v| {
        let mut v = v.lock().expect("test ownerships lock");
        v.iter()
            .position(|(n, _)| *n == name)
            .map(|i| v.remove(i).1)
    })
}

/// One destruction event: a drop's firing point for one resolved place.
/// `exact` comes from the shared resolver's verdict (`FireVerdict`):
/// every resolved shape is exact — a stepped `AfterStep` resolves to its
/// enclosing statement's boundary, the same point the backend's fire
/// map builds from. The single non-exact shape is the unresolvable-loop
/// fallback (`FireVerdict::NonExact` — a compound-condition header whose
/// matching test is unreachable, i.e. a malformed body): its ordinal is
/// a lower bound, not the fire point — pairs involving a non-exact event
/// are not PROVABLE double-fires (counted in [`ValidationSummary`],
/// never silently complete).
/// One executable cleanup event: a RELEASE frees the generation's buffer
/// in this function; a TRANSFER moves ownership out (the buffer leaves
/// this function alive — the callee must never release it). Both
/// discharge a generation's obligation on the paths that execute them.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Event {
    pub(crate) point: Point,
    pub(crate) target: TirRef,
    pub(crate) kind: EventKind,
    pub(crate) pos: DropPosition,
    pub(crate) exact: bool,
    /// Sub-identity for FIELD-targeted releases: two events naming
    /// different fields of one aggregate (or a field and the whole)
    /// free DIFFERENT buffers — never a double-free pair. `None` = the
    /// whole-place identity.
    pub(crate) field: Option<TirRef>,
}

/// The event taxonomy (see the module header). `Destroy`/`Displace`/
/// `PromoRelease` are releases; the `Transfer*`/`Consume` kinds are
/// transfers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EventKind {
    /// A static produced value with no allocation to destroy.
    StaticNoop,
    /// A scheduled `DropOp` destruction.
    Destroy,
    /// `free_on_reassign` / `field_free_on_reassign`: the old content's
    /// release at a replacement store (fires before the write).
    Displace,
    /// A promotion-buffer free — a distinct allocation identity from
    /// the borrowed original; a promo release covers only the promoted
    /// copy.
    PromoRelease,
    /// Ownership moved out through a `Return` (a `Move` hazard at a
    /// return site).
    TransferReturn,
    /// Ownership moved out through a move argument or a match consume.
    TransferCall,
    /// An in-place consuming concat: the old buffer's ownership moves
    /// into the concat result (codegen skips the displacement free).
    Consume,
    /// An inout param's seated value leaves through the write-back
    /// pointer at a function exit.
    TransferInoutWriteback,
    /// A by-value match arm read: ownership moves into the match
    /// result — a transfer IN (the result's release frees the buffer
    /// later, in this function).
    TransferArm,
}

impl Event {
    /// Releases free the buffer in this function (double-discharge
    /// relevant).
    pub(crate) fn is_release(&self) -> bool {
        matches!(
            self.kind,
            EventKind::Destroy | EventKind::Displace | EventKind::PromoRelease
        )
    }

    /// Transfers OUT: ownership leaves this function — a release on
    /// either side of a transfer-out is a violation (freeing storage
    /// whose ownership left, or freeing it twice).
    fn is_transfer_out(&self) -> bool {
        matches!(
            self.kind,
            EventKind::TransferReturn | EventKind::TransferCall | EventKind::TransferInoutWriteback
        )
    }
}

/// The identity TirRef of a place (for synthetic events' targets).
fn scrutinee_owner_ref(flow: &Flow, place: PlaceId) -> TirRef {
    match flow.place(place) {
        Place::Param { index, .. } => TirRef::param(*index as usize),
        Place::Local { decl, .. }
        | Place::Token { inst: decl, .. }
        | Place::Temp { producer: decl, .. }
        | Place::MatchResult {
            match_inst: decl, ..
        }
        | Place::LoopVar {
            loop_inst: decl, ..
        }
        | Place::View { producer: decl, .. } => *decl,
    }
}

/// The tag of a target for diagnostics — param sentinels have no
/// instruction (their "tag" is their identity).
pub(crate) fn tag_of(tir: &Tir, target: TirRef) -> String {
    if target.is_param() {
        return format!("{target:?} (param)");
    }
    format!("{:?}", tir.inst(target).tag)
}

/// The sub-identity of a field-targeted release: the target itself when
/// it is a `FieldAccess` chain (one identity per field), else `None`
/// (the whole place).
pub(crate) fn field_key(tir: &Tir, target: TirRef) -> Option<TirRef> {
    if target.is_param() {
        return None;
    }
    matches!(tir.inst(target).tag, TirTag::FieldAccess).then_some(target)
}

/// All cleanup events per resolved place: scheduled destructions, the
/// sidecar's displacement releases (minus the consumed-concat slots,
/// whose free codegen skips — modelled as `Consume` transfers instead),
/// and the walk-recorded transfers (`Move` hazards at return/call/match
/// sites; inout write-backs at every exit). Events whose anchor can
/// never fire (a payload on an all-diverging construct) are dropped; an
/// IfStmt payload yields one event per non-diverging arm continuation.
/// Promotion frees stay resolution-only here (their pairing checks land
/// with the promo identity work) — see the header.
pub(crate) fn collect_events(
    tir: &Tir,
    places: &TargetPlaces,
    flow: &Flow,
    fire_points: &FirePoints,
    sidecar: &FunctionSidecar,
    own: &Ownership,
    pool: &InternPool,
) -> HashMap<PlaceId, Vec<Event>> {
    let storage = ryo_core::flow::cleanup::CleanupPlan::resolve(tir, flow, sidecar, pool)
        .expect("cleanup positions resolve before event collection");
    let mut collector = EventCollector {
        tir,
        places,
        flow,
        fire_points,
        out: HashMap::new(),
    };
    collector.collect_drops(sidecar, &storage);
    collector.collect_displacement_releases(sidecar);
    collector.collect_consumed_concat_transfers(sidecar);
    collector.collect_walk_transfers(sidecar);
    collector.collect_adoptions();
    collector.collect_inout_writebacks(own);
    collector.collect_promotion_releases(sidecar);
    collector.out
}

/// Collect events in the same source order used by validation. Keeping
/// the resolved places and deduplication rule here ensures every source
/// uses the same event identity.
struct EventCollector<'a> {
    tir: &'a Tir,
    places: &'a TargetPlaces,
    flow: &'a Flow,
    fire_points: &'a FirePoints<'a>,
    out: HashMap<PlaceId, Vec<Event>>,
}

impl EventCollector<'_> {
    fn push(&mut self, place: PlaceId, event: Event) {
        let list = self.out.entry(place).or_default();
        // Same (point, target, kind) twins collapse (the designed
        // double-anchor is one obligation at one point).
        if !list
            .iter()
            .any(|e| e.point == event.point && e.target == event.target && e.kind == event.kind)
        {
            list.push(event);
        }
    }

    fn resolved_place(&self, target: TirRef) -> Option<PlaceId> {
        let place = self.places.place(self.flow, target)?;
        (place.0 != u32::MAX).then_some(place)
    }

    fn collect_drops(
        &mut self,
        sidecar: &FunctionSidecar,
        storage: &ryo_core::flow::cleanup::CleanupPlan,
    ) {
        let tir = self.tir;
        let fire_points = self.fire_points;
        for (index, fp) in sidecar.drops.iter().enumerate() {
            let actual = match storage.releases[index] {
                ryo_core::flow::cleanup::ReleaseStorage::Binding(place) => Some(place),
                ryo_core::flow::cleanup::ReleaseStorage::Value(target) => {
                    self.places.place(self.flow, target)
                }
            };
            let Some(place) = actual else {
                continue;
            };
            if place.0 == u32::MAX {
                continue; // unresolvable target group — check (b) handles
            }
            for (point, exact) in position_points(fire_points, fp.position) {
                self.push(
                    place,
                    Event {
                        point,
                        target: fp.target,
                        kind: if storage.releases[index].is_static_noop(tir) {
                            EventKind::StaticNoop
                        } else {
                            EventKind::Destroy
                        },
                        pos: fp.position,
                        exact,
                        field: field_key(tir, fp.target),
                    },
                );
            }
        }
    }

    fn collect_displacement_releases(&mut self, sidecar: &FunctionSidecar) {
        let tir = self.tir;
        let fire_points = self.fire_points;
        // Displacement releases: the old content's free at a replacement
        // store. A consumed-concat Assign skips its `free_on_reassign` free
        // at codegen (the buffer is consumed in place) — those slots become
        // `Consume` transfers, not releases.
        for (slot, target) in sidecar.free_on_reassign.iter().enumerate() {
            let Some(target) = *target else { continue };
            if sidecar.consumed_concat_lhs[slot].is_some() {
                continue;
            }
            let Some(place) = self.resolved_place(target) else {
                continue;
            };
            let inst = TirRef::from_raw(slot as u32);
            for (point, exact) in position_points(fire_points, DropPosition::BeforeStep { inst }) {
                self.push(
                    place,
                    Event {
                        point,
                        target,
                        kind: EventKind::Displace,
                        pos: DropPosition::BeforeStep { inst },
                        exact,
                        field: field_key(tir, target),
                    },
                );
            }
        }
        for (slot, target) in sidecar.field_free_on_reassign.iter().enumerate() {
            let Some(target) = *target else { continue };
            let Some(place) = self.resolved_place(target) else {
                continue;
            };
            let inst = TirRef::from_raw(slot as u32);
            for (point, exact) in position_points(fire_points, DropPosition::BeforeStep { inst }) {
                self.push(
                    place,
                    Event {
                        point,
                        target,
                        kind: EventKind::Displace,
                        pos: DropPosition::BeforeStep { inst },
                        exact,
                        field: field_key(tir, target),
                    },
                );
            }
        }
    }

    fn collect_consumed_concat_transfers(&mut self, sidecar: &FunctionSidecar) {
        let fire_points = self.fire_points;
        // Consumed-concat transfers: codegen skips the displacement free
        // for a consumed-concat Assign — the old lhs buffer's ownership
        // moves INTO the in-place concat result. The old buffer is the
        // slot's `free_on_reassign` target (the consumed_concat slot names
        // the concat itself, for codegen's in-place decision).
        for (slot, value) in sidecar.consumed_concat_lhs.iter().enumerate() {
            let Some(_concat) = *value else { continue };
            let Some(target) = sidecar.free_on_reassign[slot] else {
                continue;
            };
            let Some(place) = self.resolved_place(target) else {
                continue;
            };
            let inst = TirRef::from_raw(slot as u32);
            for (point, exact) in position_points(fire_points, DropPosition::BeforeStep { inst }) {
                self.push(
                    place,
                    Event {
                        point,
                        target,
                        kind: EventKind::Consume,
                        pos: DropPosition::BeforeStep { inst },
                        exact,
                        field: None,
                    },
                );
            }
        }
    }

    fn collect_walk_transfers(&mut self, sidecar: &FunctionSidecar) {
        let tir = self.tir;
        let flow = self.flow;
        let fire_points = self.fire_points;
        // Walk-recorded transfers. `Move` hazards discharge the owner's
        // obligation at the hazard's resolved point; `InoutPass`/`Mutate`
        // hazards do not (see `Ownership::owner_hazards`).
        for transfer in &sidecar.transfers {
            let site = transfer.site;
            let place = transfer.source;
            if place.0 == u32::MAX {
                continue;
            }
            // Only consumes that move ownership OUT of this function (or
            // into a DIFFERENT un-unioned aggregate) are transfers. Binding
            // seating (VarDecl/Assign/destructure) and match adoptions
            // continue the identity inside ONE union — no event. Return
            // sites transfer at the return block's terminator point (after
            // the operand evaluation, before the exit). Call sites (move
            // args) transfer at the consuming statement's boundary.
            let (kind, resolved): (EventKind, Vec<(Point, bool)>) = match tir.inst(site).tag {
                TirTag::Return | TirTag::ReturnVoid => (
                    EventKind::TransferReturn,
                    position_points(fire_points, DropPosition::AfterStep { inst: site }),
                ),
                TirTag::Match => {
                    // An arm-expression by-value read (including an arm
                    // literal): ownership moves into the match result — a
                    // transfer IN at the arm value's producer boundary. The
                    // scrutinee-adoption hazard gets one too; it is
                    // harmless (an earlier IN on the same union — the
                    // synthetic per-match events below keep the switch and
                    // join discharges explicit).
                    let Some(producer) = (!transfer.target.is_param()).then_some(transfer.target)
                    else {
                        continue; // param arm read: documented corner
                    };
                    let Some(arm_block) = flow.step_block_of(producer) else {
                        continue;
                    };
                    (
                        EventKind::TransferArm,
                        vec![(
                            Point {
                                block: arm_block,
                                ordinal: flow.block_steps(arm_block).len() as u32,
                            },
                            true,
                        )],
                    )
                }
                TirTag::Call => (
                    EventKind::TransferCall,
                    position_points(fire_points, DropPosition::AfterStep { inst: site }),
                ),
                // Aggregate seating: a struct/enum literal field's buffer
                // moves into the aggregate — a different place, not a
                // unioned home — so the field generation is discharged by
                // the move itself (the aggregate's release frees it
                // recursively). Binding seating (VarDecl/Assign/
                // destructure) continues the identity in the SAME union and
                // needs no event.
                TirTag::FieldAssign | TirTag::StructLit | TirTag::EnumLit => (
                    EventKind::Consume,
                    position_points(fire_points, DropPosition::AfterStep { inst: site }),
                ),
                _ => continue,
            };
            for (point, exact) in resolved {
                self.push(
                    place,
                    Event {
                        point,
                        target: transfer.target,
                        kind,
                        pos: DropPosition::AfterStep { inst: site },
                        exact,
                        field: None,
                    },
                );
            }
        }
    }

    fn collect_adoptions(&mut self) {
        let tir = self.tir;
        let flow = self.flow;
        let fire_points = self.fire_points;
        // Adoption: a by-move match's scrutinee content is consumed at the
        // dispatch; a destructure adopts its bound fields into tokens the
        // same way (on binding arms the tokens adopt it — the place union
        // continues it; on non-binding arms there is no payload to
        // release). One synthetic transfer IN per construct: at the switch
        // block terminator for a match, at the destructure statement's
        // boundary for a destructure (the unbound fields are released
        // inline by the destructure itself).
        for (match_inst, token_places) in flow.token_places_iter() {
            if token_places.is_empty() {
                continue;
            }
            let (scrutinee_place, dispatch): (PlaceId, Vec<(Point, bool)>) = match tir
                .inst(match_inst)
                .tag
            {
                TirTag::Match => {
                    let Some(sw_block) = flow.term_block_of(match_inst) else {
                        continue;
                    };
                    let Some(sp) = (match flow.term(sw_block) {
                        Terminator::Switch { scrutinee, .. } => flow_owner_place(flow, *scrutinee),
                        _ => None,
                    }) else {
                        continue;
                    };
                    (
                        sp,
                        vec![(
                            Point {
                                block: sw_block,
                                ordinal: flow.block_steps(sw_block).len() as u32,
                            },
                            true,
                        )],
                    )
                }
                TirTag::Destructure => {
                    let Some(sp) = flow_owner_place(flow, tir.destructure_view(match_inst).rhs)
                    else {
                        continue;
                    };
                    (
                        sp,
                        position_points(fire_points, DropPosition::AfterStep { inst: match_inst }),
                    )
                }
                _ => continue,
            };
            for (point, exact) in dispatch {
                self.push(
                    scrutinee_place,
                    Event {
                        point,
                        target: scrutinee_owner_ref(flow, scrutinee_place),
                        kind: EventKind::TransferArm,
                        pos: DropPosition::AfterStep { inst: match_inst },
                        exact,
                        field: None,
                    },
                );
            }
            // An arm-expression by-value read moves the adopted content
            // into the match result: a synthetic transfer IN at the join
            // discharges the token's arm-start adoption on result-flowing
            // arms (statement arms discharge earlier — at their own frees;
            // the scan stops at the first event). The synthetic sub-
            // identity keeps it from pairing with the whole-scrutinee
            // releases (different buffers).
            if let TirTag::Match = tir.inst(match_inst).tag
                && let Some(sw_block) = flow.term_block_of(match_inst)
                && let Terminator::Switch {
                    join: Some(join), ..
                } = flow.term(sw_block)
            {
                self.push(
                    scrutinee_place,
                    Event {
                        point: Point {
                            block: *join,
                            ordinal: 0,
                        },
                        target: scrutinee_owner_ref(flow, scrutinee_place),
                        kind: EventKind::TransferArm,
                        pos: DropPosition::BlockStart { block: *join },
                        exact: true,
                        field: Some(match_inst),
                    },
                );
            }
        }
    }

    fn collect_inout_writebacks(&mut self, own: &Ownership) {
        let flow = self.flow;
        // Inout write-backs: whatever is seated in an inout param leaves
        // through the write-back pointer at EVERY exit — a transfer at each
        // return's terminator point.
        let param_place = |name: ryo_core::types::StringId| -> Option<PlaceId> {
            let &idx = own.param_index.get(&name)?;
            Some(own.param_places[idx])
        };
        for &name in &own.inout_str_params {
            let Some(place) = param_place(name) else {
                continue;
            };
            for b in flow.forward_order() {
                if !matches!(
                    flow.term(b),
                    Terminator::Return { .. } | Terminator::ReturnVoid
                ) {
                    continue;
                }
                let point = Point {
                    block: b,
                    ordinal: flow.block_steps(b).len() as u32,
                };
                self.push(
                    place,
                    Event {
                        point,
                        target: TirRef::param(own.param_index[&name]),
                        kind: EventKind::TransferInoutWriteback,
                        pos: DropPosition::PreTerminator { block: b },
                        exact: true,
                        field: None,
                    },
                );
            }
        }
    }

    fn collect_promotion_releases(&mut self, sidecar: &FunctionSidecar) {
        let fire_points = self.fire_points;
        // Promotion-buffer releases: the promoted copy is a
        // DISTINCT identity from the borrowed original — keyed here under
        // the base's place so the double-discharge check relates releases
        // of one buffer (borrowed params carry no callee obligations, so
        // coverage adds nothing; the pairing check is `validate_promo`).
        for pf in &sidecar.promotion_frees {
            let Some(place) = self.resolved_place(pf.base) else {
                continue;
            };
            for (point, exact) in position_points(fire_points, pf.position) {
                self.push(
                    place,
                    Event {
                        point,
                        target: pf.base,
                        kind: EventKind::PromoRelease,
                        pos: pf.position,
                        exact,
                        field: None,
                    },
                );
            }
        }
    }
}

/// Per-place generation tables, split by role:
/// - `establish`: block → sorted step ordinals that ESTABLISH a
///   generation — `Init` write facts (bindings/loop vars), the PRODUCER
///   step for anonymous places (temps/match results carry no write
///   facts; their producing step is their establishment — e.g. an
///   in-loop call result is a fresh generation every iteration), and
///   the ARM START for by-move payload tokens (the match adopts the
///   payload at arm entry; arms that don't bind the token never
///   establish it).
/// - `cut`: block → sorted step ordinals that END a generation —
///   `Replace`/`FieldReplace` writes, whose previously-seated content
///   is displaced (a reseat never releases the old content: reaching a
///   cut unreleased is a leak; events
///   across a cut name different buffers). A seating write (`Init`, or
///   a `Replace` seating its value) does NOT cut — the same buffer
///   continues in the new home. In-place `Mutate` keeps the storage
///   identity; `WriteBack` is a conservative corner — a writeback that
///   stored the same value back discharges nothing, so it is treated
///   as NOT generation-ending (the leak direction).
pub(crate) struct GenerationTable {
    pub establish: HashMap<PlaceId, HashMap<BlockId, Vec<u32>>>,
    pub cut: HashMap<PlaceId, HashMap<BlockId, Vec<u32>>>,
    /// Token adoptions (match/destructure arm starts): the content
    /// exists AT the arm start — the obligation is the adoption point
    /// itself, not ord+1 (the arm's own ord-0 events — an arm-start
    /// drop, a return-epilogue free — discharge it).
    pub adopt: HashMap<PlaceId, Vec<Point>>,
}

pub(crate) fn generation_table(
    tir: &Tir,
    flow: &Flow,
    pool: &InternPool,
    idx: &StepIndex,
    sidecar: &FunctionSidecar,
) -> GenerationTable {
    // Token adoptions: place → the arm-start points that establish it.
    let mut token_arms: HashMap<PlaceId, Vec<Point>> = HashMap::new();
    for (owner_inst, places) in flow.token_places_iter() {
        match tir.inst(owner_inst).tag {
            TirTag::Match => {
                let sw_block = flow.term_block_of(owner_inst).expect("match is a switch");
                let Terminator::Switch { arms, .. } = flow.term(sw_block) else {
                    unreachable!("match is a switch")
                };
                let arm_blocks: Vec<BlockId> =
                    arms.as_range().map(|i| BlockId(flow.extra[i])).collect();
                let mut nth = 0usize;
                for (arm, &arm_block) in tir.match_view(owner_inst).arms().zip(&arm_blocks) {
                    let n_moves = arm.bindings().filter(|b| !pool.is_copy(b.ty)).count();
                    for _ in 0..n_moves {
                        if let Some(&p) = places.get(nth) {
                            token_arms.entry(p).or_default().push(Point {
                                block: arm_block,
                                ordinal: 0,
                            });
                        }
                        nth += 1;
                    }
                }
            }
            TirTag::Destructure => {
                for &p in places {
                    if let Some((b, ord)) = idx.get(owner_inst) {
                        token_arms.entry(p).or_default().push(Point {
                            block: b,
                            ordinal: ord,
                        });
                    }
                }
            }
            other => unreachable!("token places only for matches/destructures: {other:?}"),
        }
    }
    let mut establish: HashMap<PlaceId, HashMap<BlockId, Vec<u32>>> = HashMap::new();
    let mut cut: HashMap<PlaceId, HashMap<BlockId, Vec<u32>>> = HashMap::new();
    let mut adopt: HashMap<PlaceId, Vec<Point>> = HashMap::new();
    for pid in 0..flow.places.len() as u32 {
        let place = PlaceId(pid);
        let mut est: HashMap<BlockId, Vec<u32>> = HashMap::new();
        for &w in flow.place_writes(place) {
            let Some((b, ord)) = idx.get(w) else { continue };
            let kind = flow
                .step_of(w)
                .and_then(|s| s.facts.write)
                .map(|pw| pw.kind);
            // A seating `Init` establishes the binding/loop-var
            // generation (a `Replace` does not — the value's producer
            // did; see `GenerationTable`).
            if matches!(kind, Some(WriteKind::Init)) {
                est.entry(b).or_default().push(ord);
            }
        }
        // Anonymous places: the producer step establishes the content;
        // tokens are established at their adoption points (arm starts).
        match flow.place(place) {
            Place::Token { .. } => {
                for p in token_arms.remove(&place).unwrap_or_default() {
                    adopt.entry(place).or_default().push(p);
                }
            }
            Place::Temp { producer, .. }
            | Place::MatchResult {
                match_inst: producer,
                ..
            } => {
                // A `Var` read produces NO new buffer — its temp place
                // mirrors the read owner (a by-value arm read, an
                // identity rewrite); the obligation rides the owner's
                // own group, so no self-establishment here.
                let producer_tag = tir.inst(*producer).tag;
                if !matches!(
                    producer_tag,
                    TirTag::Var | TirTag::StrConst | TirTag::BytesConst
                ) && let Some((b, ord)) = idx.get(*producer)
                {
                    // Static strings and bytes have no allocation to
                    // release, so they create no storage obligation.
                    est.entry(b).or_default().push(ord);
                }
            }
            _ => {}
        }
        for ords in est.values_mut() {
            ords.sort_unstable();
        }
        if !est.is_empty() {
            establish.insert(place, est);
        }
    }
    // Cuts are attributed to the DISPLACED content, not the written
    // home: a Replace/FieldReplace ends the generation seated just
    // before the store, which the sidecar names precisely — a
    // displacement release (`free_on_reassign` / `field_free_on_reassign`)
    // or a consumed-concat slot (the old buffer moves into the in-place
    // result). A Replace whose old content needed no release (untracked
    // or already discharged) cuts nothing.
    let mut add_cut = |target: TirRef, slot: usize| {
        let Some((b, ord)) = idx.get(TirRef::from_raw(slot as u32)) else {
            return;
        };
        let owner_place = match flow.temp_of(target) {
            Some(p) => p,
            None => match flow.local_of_decl(target) {
                Some(p) => p,
                None => match target.as_param_index().and_then(|idx| {
                    flow.places
                        .iter()
                        .position(|pl| matches!(pl, Place::Param { index, .. } if *index == idx))
                }) {
                    // An inout-param reseat displaces the previously
                    // seated value — cut the param's lineage.
                    Some(i) => PlaceId(i as u32),
                    None => match flow_owner_place(flow, target) {
                        // A field-reassignment target (a FieldAccess
                        // chain) resolves to its aggregate base — the
                        // cut ends the old field value's generation of
                        // the base union.
                        Some(p) => p,
                        None => return,
                    },
                },
            },
        };
        cut.entry(owner_place)
            .or_default()
            .entry(b)
            .or_default()
            .push(ord);
    };
    for (slot, target) in sidecar.free_on_reassign.iter().enumerate() {
        if let Some(target) = *target {
            add_cut(target, slot);
        }
    }
    for (slot, target) in sidecar.field_free_on_reassign.iter().enumerate() {
        if let Some(target) = *target {
            add_cut(target, slot);
        }
    }

    for by_block in cut.values_mut() {
        for ords in by_block.values_mut() {
            ords.sort_unstable();
        }
    }
    GenerationTable {
        establish,
        cut,
        adopt,
    }
}

/// Is step `ord` of `block` in the given per-place ordinal table?
pub(crate) fn in_ordinal_table(
    table: Option<&HashMap<BlockId, Vec<u32>>>,
    block: BlockId,
    ord: u32,
) -> bool {
    table.is_some_and(|m| {
        m.get(&block)
            .is_some_and(|ords| ords.binary_search(&ord).is_ok())
    })
}

/// Blocks reachable from the entry (obligations/events in dead blocks
/// are skipped — the conservative corner).
pub(crate) fn reachable_blocks(flow: &Flow) -> HashSet<BlockId> {
    let mut seen = HashSet::new();
    let mut stack = vec![flow.entry];
    while let Some(b) = stack.pop() {
        if seen.insert(b) {
            stack.extend_from_slice(flow.successors(b));
        }
    }
    seen
}

/// Place unions via final seating: a value's content keeps its producer
/// place's identity while seated in a binding home (a temp moved into a
/// local stays `Owner(temp_place)` in `current_owner[home]`), so events
/// and obligations fragment across the two place identities. Union every
/// home with the place of the owner seated in it — one buffer, one
/// obligation group. (Generations are still separated by writes; the
/// union merges identities, not generations.)
pub(crate) fn place_roots(
    tir: &Tir,
    flow: &Flow,
    own: &Ownership,
    sidecar: &FunctionSidecar,
) -> Vec<u32> {
    let n = flow.places.len();
    let mut parent: Vec<u32> = (0..n as u32).collect();
    fn find(parent: &mut Vec<u32>, x: u32) -> u32 {
        if parent[x as usize] != x {
            let r = find(parent, parent[x as usize]);
            parent[x as usize] = r;
        }
        parent[x as usize]
    }
    // Binding LINEAGE: a home's slot and every value ever seated in it
    // share the runtime home slot — a release lowering through the
    // binding (codegen's redirect) frees whichever is seated, and a
    // displacement cut (`free_on_reassign`) ends the seated generation.
    // Union the home with each init/reassign value's place so the
    // checks see one obligation group per binding slot; the per-gen
    // separation comes from the cut table.
    let seat = |parent: &mut Vec<u32>, home: PlaceId, value: TirRef| {
        let seated = flow
            .temp_of(value)
            .or_else(|| flow.match_result(value))
            .map(|p| p.0);
        if let Some(sp) = seated {
            let (ra, rb) = (find(parent, home.0), find(parent, sp));
            if ra != rb {
                parent[ra as usize] = rb;
            }
        }
    };
    let mut lineage_decls: HashSet<TirRef> = HashSet::new();
    // Inout params: the write-back transfers whatever value was last
    // seated, so reassign values join the param's union (the home is
    // the caller's slot, not a Local place).
    for (slot, bound) in sidecar.assign_binding.iter().enumerate() {
        let Some(decl) = *bound else { continue };
        if !decl.is_param() {
            continue;
        }
        let Some(idx) = decl.as_param_index() else {
            continue;
        };
        let Some(param_place) = flow
            .places
            .iter()
            .position(|pl| matches!(pl, Place::Param { index, .. } if *index == idx))
        else {
            continue;
        };
        let assign = TirRef::from_raw(slot as u32);
        if tir.inst(assign).tag == TirTag::Assign {
            seat(
                &mut parent,
                PlaceId(param_place as u32),
                tir.assign_view(assign).value,
            );
        }
    }
    for place in flow.places.iter() {
        if let Place::Local { decl, .. } = place {
            if !lineage_decls.insert(*decl) {
                continue;
            }
            let Some(home) = flow.local_of_decl(*decl) else {
                continue;
            };
            // The declaring VarDecl's initializer seats into the home.
            if tir.inst(*decl).tag == TirTag::VarDecl {
                seat(&mut parent, home, tir.var_decl_view(*decl).initializer);
            }
            // Every Assign the walk stamped with this binding seats its
            // value.
            for (slot, bound) in sidecar.assign_binding.iter().enumerate() {
                if *bound == Some(*decl) {
                    let assign = TirRef::from_raw(slot as u32);
                    if tir.inst(assign).tag == TirTag::Assign {
                        seat(&mut parent, home, tir.assign_view(assign).value);
                    }
                }
            }
        }
    }
    for (home, owner) in &own.current_owner {
        let (a, b) = (home.0, owner.0.0);
        // Errored bodies can mint placeholder owners with out-of-range
        // places; the path checks skip those anyway — don't index.
        if a as usize >= n || b as usize >= n {
            continue;
        }
        let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
        if ra != rb {
            parent[ra as usize] = rb;
        }
    }
    // Match adoption: a by-move payload token IS the scrutinee's content
    // on the arm that binds it — union the scrutinee's place with its
    // token places so arm-start/token/whole-value events group into one
    // obligation group (the pre-adoption scrutinee and the post-adoption
    // payload are different generations; the arm split separates them).
    for (match_inst, token_places) in flow.token_places_iter() {
        let Some(sw_block) = flow.term_block_of(match_inst) else {
            continue;
        };
        let Terminator::Switch { scrutinee, .. } = flow.term(sw_block) else {
            continue;
        };
        if let Some(sp) = flow_owner_place(flow, *scrutinee) {
            for &tp in token_places {
                if tp.0 as usize >= n {
                    continue;
                }
                let (ra, rb) = (find(&mut parent, sp.0), find(&mut parent, tp.0));
                if ra != rb {
                    parent[ra as usize] = rb;
                }
            }
        }
    }
    (0..n as u32).map(|i| find(&mut parent, i)).collect()
}

/// True when `block` can be re-entered within one execution (some path
/// from it returns to it) — codegen's per-block-visit `freed_targets`
/// dedup is cleared on re-entry, so the co-located double-anchor
/// exemption does not apply there.
fn block_on_cycle(flow: &Flow, block: BlockId) -> bool {
    let mut seen = HashSet::new();
    let mut stack: Vec<BlockId> = flow.successors(block).to_vec();
    while let Some(b) = stack.pop() {
        if b == block {
            return true;
        }
        if seen.insert(b) {
            stack.extend_from_slice(flow.successors(b));
        }
    }
    false
}

/// Drop-target → owning place, resolved through the same identity
/// codegen's binding tables use: params by index, temps / match results
/// / tokens by their producing inst, declarations and reads by their
/// step facts. Built once per validation (the token reverse map is the
/// only piece not directly queryable).
pub(crate) struct TargetPlaces {
    tokens: HashMap<TirRef, PlaceId>,
}

impl TargetPlaces {
    pub(crate) fn build(tir: &Tir, flow: &Flow, pool: &InternPool) -> Self {
        let mut tokens = HashMap::new();
        for (owner_inst, places) in flow.token_places_iter() {
            // The token refs come from the producing instruction's wire —
            // a match's by-move refs, a destructure's bound-owner refs
            // (same construction as `analyze_function`'s inst_place map).
            let refs = match tir.inst(owner_inst).tag {
                TirTag::Match => tir.match_by_move_owner_refs(owner_inst, pool),
                TirTag::Destructure => tir.destructure_bound_owner_refs(owner_inst),
                other => unreachable!("token places only for matches/destructures: {other:?}"),
            };
            debug_assert_eq!(refs.len(), places.len(), "token/place mismatch");
            for (r, p) in refs.iter().zip(places.iter()) {
                tokens.insert(*r, *p);
            }
        }
        TargetPlaces { tokens }
    }

    pub(crate) fn place(&self, flow: &Flow, target: TirRef) -> Option<PlaceId> {
        if let Some(idx) = target.as_param_index() {
            return flow.places.iter().enumerate().find_map(|(i, p)| {
                matches!(p, Place::Param { index, .. } if *index == idx)
                    .then_some(PlaceId(i as u32))
            });
        }
        if let Some(p) = flow.temp_of(target) {
            return Some(p);
        }
        if let Some(p) = flow.match_result(target) {
            return Some(p);
        }
        if let Some(p) = self.tokens.get(&target) {
            return Some(*p);
        }
        flow_owner_place(flow, target)
    }
}

/// (a) Every scheduled position resolves through the shared resolver —
/// `FireVerdict::Unresolved` (an anchor that is neither a Flow step nor
/// a terminator payload, or a block the body does not contain) is a
/// VALIDATION ERROR: unknown is never "treated as validated". A
/// `NonExact` resolution (the resolver's unresolvable-loop fallback) is
/// not an error — it is NOT PROVED, counted in the summary. And every
/// conditional-dead-drop block is one of its `IfStmt`'s RECORDED
/// cascade arms (the fall-through block included).
pub(crate) fn validate_positions(
    flow: &Flow,
    fire_points: &FirePoints,
    sidecar: &FunctionSidecar,
    summary: &mut ValidationSummary,
) {
    for fp in sidecar
        .drops
        .iter()
        .map(|d| &d.position)
        .chain(sidecar.promotion_frees.iter().map(|p| &p.position))
    {
        summary.positions += 1;
        match fire_points.resolve(*fp) {
            FireVerdict::Unresolved => {
                summary.invalid = true;
            }
            FireVerdict::NonExact(_) => summary.positions_not_proved += 1,
            FireVerdict::Resolved(_) | FireVerdict::ResolvedPerArm(_) => {}
        }
    }
    if summary.invalid {
        return;
    }
    // BlockStart drops are either match-arm-start payload destruction
    // or conditional dead drops; tell them apart by the same identity
    // the backend fire point uses (the block is an arm of a match whose
    // scrutinee is the target). Dead drops must additionally sit in a
    // RECORDED cascade arm of some IfStmt (the fall-through block
    // included) — the membership the old ConditionalDeadDrop table
    // enforced against its `if_stmt`.
    let mut allowed_dead: Vec<BlockId> = Vec::new();
    let mut arm_scrutinees: Vec<(BlockId, TirRef)> = Vec::new();
    for b in flow.forward_order() {
        match flow.term(b) {
            Terminator::Branch { .. } => {
                if flow.branch_if(b).is_some() {
                    let arms = flow.branch_arm_blocks(flow.branch_if(b).expect("checked"));
                    allowed_dead.push(arms.then_block);
                    allowed_dead.extend_from_slice(&arms.elif_blocks);
                    allowed_dead.push(arms.else_block);
                }
            }
            Terminator::Switch {
                scrutinee, arms, ..
            } => {
                for i in arms.as_range() {
                    arm_scrutinees.push((BlockId(flow.extra[i]), *scrutinee));
                }
            }
            _ => {}
        }
    }
    for d in &sidecar.drops {
        let DropPosition::BlockStart { block } = d.position else {
            continue;
        };
        let is_arm_start = arm_scrutinees
            .iter()
            .any(|&(ab, scrut)| ab == block && scrut == d.target);
        if is_arm_start {
            continue;
        }
        debug_assert!(
            allowed_dead.contains(&block),
            "conditional dead drop fires in {block:?}, which is not a recorded cascade arm"
        );
    }
}

/// A total order key for a drop position (sort only; `DropPosition`
/// carries no `Hash`).
fn position_key(pos: DropPosition) -> (u8, u32, u32) {
    match pos {
        DropPosition::AfterStep { inst } => (0, inst.raw(), 0),
        DropPosition::BeforeStep { inst } => (1, inst.raw(), 0),
        DropPosition::BlockStart { block } => (2, block.0, 0),
        DropPosition::PreTerminator { block } => (3, block.0, 0),
    }
}

/// (b) No double destruction. Codegen buckets drops purely by position
/// and, within one bucket, marks a same-target entry already freed a
/// no-op (`codegen/frees.rs` — "the ownership pass's double-anchors
/// (consumer + return epilogue) make one same-target duplicate a no-op
/// by design"). So the soundness contract is about PLACE ALIASING, not
/// target identity: two drops naming DIFFERENT target TirRefs that
/// resolve to the SAME place at the SAME position both emit (codegen's
/// dedup keys on the raw TirRef) — a double-free. Same target + same
/// position is the designed double-anchor; same place at DIFFERENT
/// positions is legal conditional destruction.
pub(crate) fn validate_no_duplicate_destruction(
    places: &TargetPlaces,
    flow: &Flow,
    sidecar: &FunctionSidecar,
) {
    let mut keys: Vec<(PlaceId, (u8, u32, u32), TirRef)> = sidecar
        .drops
        .iter()
        .map(|fp| {
            (
                places.place(flow, fp.target).unwrap_or(PlaceId(u32::MAX)),
                position_key(fp.position),
                fp.target,
            )
        })
        .collect();
    keys.sort_by_key(|&(place, pos, _)| (place, pos));
    for w in keys.windows(2) {
        debug_assert!(
            // Same (place, position) with the SAME target is the designed
            // double-anchor (deduped by codegen); only DISTINCT targets
            // aliasing one place at one position are double-frees.
            w[0].0 != w[1].0 || w[0].1 != w[1].1 || w[0].2 == w[1].2,
            "double destruction of place {:?} at position {:?}: two distinct targets {:?} and {:?} alias one place at one position — codegen frees both",
            w[0].0,
            w[0].1,
            w[0].2,
            w[1].2
        );
    }
}

/// (d) No PATH double destruction. Two events of one place-union group
/// whose points reach each other through the CFG with no intervening
/// Init/Replace write (or producer step / token adoption) of any member
/// place both fire on one path — a double-free. Sibling-arm events are
/// mutually unreachable (no path) → legal. The identical (target,
/// position) twin at the reached point is the designed
/// consumer+epilogue double-anchor (one bucket, deduped by codegen) —
/// skipped here; distinct targets at one position are check (b)'s job
/// and also fire, so they are flagged here too (a same-point reach is a
/// path). O(events × blocks): one DFS per event, visited-set on points —
/// no per-pair matrices. The per-body tally type lives in `ryo-core`
/// (`ValidationSummary`) so the check path propagates it to the consumer
/// on `FunctionSidecar::validation`; see its docs and the module header
/// for the category list.
pub(crate) fn validate_no_path_double_destruction(
    tir: &Tir,
    flow: &Flow,
    own: &Ownership,
    sidecar: &FunctionSidecar,
    events: &HashMap<PlaceId, Vec<Event>>,
    table: &GenerationTable,
    summary: &mut ValidationSummary,
) {
    let reachable = reachable_blocks(flow);
    let roots = place_roots(tir, flow, own, sidecar);
    let index = StepIndex::build(flow);
    let by_root = group_path_events(events, &roots);
    let reseat_cuts = reseat_cuts(flow, &index, &roots);
    for ((root, allocation), evs) in &by_root {
        // Cuts of ANY member place end a path (a Replace/FieldReplace
        // displaces the seated content — the buffer identity changes).
        let root_cuts = root_generation_cuts(
            events,
            &roots,
            table,
            &reseat_cuts,
            *root,
            allocation.is_some(),
        );
        check_path_event_group(tir, flow, &reachable, *root, evs, &root_cuts, summary);
    }
}

/// Group events by place-union root. Promotion releases use their own
/// allocation identity because they free a scratch copy, not the source.
fn group_path_events(
    events: &HashMap<PlaceId, Vec<Event>>,
    roots: &[u32],
) -> HashMap<(u32, Option<TirRef>), Vec<Event>> {
    let mut by_root: HashMap<(u32, Option<TirRef>), Vec<Event>> = HashMap::new();
    for (place, evs) in events {
        for event in evs {
            let allocation = (event.kind == EventKind::PromoRelease).then_some(event.target);
            by_root
                .entry((roots[place.0 as usize], allocation))
                .or_default()
                .push(*event);
        }
    }
    by_root
}

/// Index replacement writes that start a new home generation for a root.
fn reseat_cuts(flow: &Flow, index: &StepIndex, roots: &[u32]) -> HashMap<u32, Vec<(BlockId, u32)>> {
    let mut cuts: HashMap<u32, Vec<(BlockId, u32)>> = HashMap::new();
    for step in &flow.steps {
        if let Some(write) = step.facts.write
            && matches!(
                write.kind,
                WriteKind::Replace | WriteKind::FieldReplace { .. }
            )
            && let Some(point) = index.get(step.inst)
        {
            cuts.entry(roots[write.place.0 as usize])
                .or_default()
                .push(point);
        }
    }
    cuts
}

/// Collect every cut that ends a group's current generation.
fn root_generation_cuts(
    events: &HashMap<PlaceId, Vec<Event>>,
    roots: &[u32],
    table: &GenerationTable,
    reseat_cuts: &HashMap<u32, Vec<(BlockId, u32)>>,
    root: u32,
    is_promotion: bool,
) -> HashMap<BlockId, Vec<u32>> {
    let mut cuts: HashMap<BlockId, Vec<u32>> = HashMap::new();
    for place in events.keys() {
        if is_promotion || roots[place.0 as usize] != root {
            continue;
        }
        if let Some(by_block) = table.cut.get(place) {
            for (&block, ordinals) in by_block {
                cuts.entry(block).or_default().extend_from_slice(ordinals);
            }
        }
    }
    // A reseat starts a new home generation even when the old value moved
    // out and therefore needed no displacement release.
    if !is_promotion && let Some(reseats) = reseat_cuts.get(&root) {
        for &(block, ordinal) in reseats {
            cuts.entry(block).or_default().push(ordinal);
        }
    }
    for ordinals in cuts.values_mut() {
        ordinals.sort_unstable();
        ordinals.dedup();
    }
    cuts
}

fn index_events_by_point(events: &[Event]) -> HashMap<Point, Vec<usize>> {
    events
        .iter()
        .enumerate()
        .fold(HashMap::new(), |mut by_point, (index, event)| {
            by_point.entry(event.point).or_default().push(index);
            by_point
        })
}

/// Check reachability between events within one storage-generation group.
fn check_path_event_group(
    tir: &Tir,
    flow: &Flow,
    reachable: &HashSet<BlockId>,
    root: u32,
    events: &[Event],
    cuts: &HashMap<BlockId, Vec<u32>>,
    summary: &mut ValidationSummary,
) {
    let place_cuts = Some(cuts);
    let by_point = index_events_by_point(events);
    for e1 in events {
        if !reachable.contains(&e1.point.block) {
            // Never fires — excluded from the path checks, counted
            // (unreachable events create no false obligations).
            summary.events_unreachable += 1;
            continue;
        }
        let mut visited: HashSet<Point> = HashSet::new();
        let mut parent: HashMap<Point, Point> = HashMap::new();
        let mut stack: Vec<Point> = Vec::new();
        push_successors(flow, place_cuts, e1.point, &mut stack);
        for &next in &stack {
            parent.insert(next, e1.point);
        }
        while let Some(p) = stack.pop() {
            if !visited.insert(p) {
                continue;
            }
            if let Some(ids) = by_point.get(&p) {
                for &j in ids {
                    let e2 = &events[j];
                    if !is_path_double_destruction_candidate(flow, e1, e2) {
                        continue;
                    }
                    // Provability gate: a pair is only PROVABLE when
                    // both events fire exactly where modeled (every
                    // resolved shape is exact except the resolver's
                    // unresolvable-loop fallback); a pair containing
                    // a non-exact event is NOT PROVED — counted,
                    // never silently valid.
                    summary.pairs_examined += 1;
                    if !e1.exact || !e2.exact {
                        summary.pairs_not_proved += 1;
                        continue;
                    }
                    // Reconstruct one witnessing path (block chain).
                    let mut path = vec![p];
                    let mut cur = p;
                    while cur != e1.point {
                        cur = parent[&cur];
                        path.push(cur);
                    }
                    path.reverse();
                    debug_assert!(
                        false,
                        "path double discharge of place union {root}: event at {:?} \
                             ({:?} kind {:?} target {:?} tag {:?}) reaches event at {:?} \
                             ({:?} kind {:?} target {:?} tag {:?}) with no intervening \
                             generation write — a release frees storage whose ownership \
                             already left, or frees it twice; witness blocks: {:?}",
                        e1.point,
                        e1.pos,
                        e1.kind,
                        e1.target,
                        tag_of(tir, e1.target),
                        e2.point,
                        e2.pos,
                        e2.kind,
                        e2.target,
                        tag_of(tir, e2.target),
                        path.iter()
                            .map(|p| (p.block, p.ordinal))
                            .collect::<Vec<_>>()
                    );
                }
            }
            let before = stack.len();
            push_successors(flow, place_cuts, p, &mut stack);
            for &next in &stack[before..] {
                parent.entry(next).or_insert(p);
            }
        }
    }
}

/// Apply event-kind and codegen-dedup rules before proving reachability
/// is a double discharge of the same storage generation.
fn is_path_double_destruction_candidate(flow: &Flow, first: &Event, second: &Event) -> bool {
    if first.kind == EventKind::StaticNoop || second.kind == EventKind::StaticNoop {
        return false;
    }
    // Transfer-to-transfer is the walk's E0020 jurisdiction, not a
    // cleanup-schedule violation.
    if !first.is_release() && !second.is_release() {
        return false;
    }
    // A transfer out on either side is invalid. A transfer in reached
    // from a release is use-after-free; a release reached from a
    // transfer in is the normal lifecycle of the adopted value.
    if first.is_release() != second.is_release() {
        let transfer = if first.is_release() { second } else { first };
        if !transfer.is_transfer_out() && !std::ptr::eq(transfer, second) {
            return false;
        }
    }
    // The identical event is the designed consumer + epilogue no-op.
    if second.target == first.target && second.pos == first.pos && second.kind == first.kind {
        return false;
    }
    // Different fields of one aggregate free different buffers.
    if first.field != second.field {
        return false;
    }
    // Promotion releases are flag-guarded and flag-clearing.
    if first.kind == EventKind::PromoRelease
        && second.kind == EventKind::PromoRelease
        && first.target == second.target
    {
        return false;
    }
    // Same-target releases in one non-cyclic block visit are deduped by
    // codegen. Re-entry clears that dedup, so cyclic blocks stay checked.
    !(first.is_release()
        && second.is_release()
        && second.target == first.target
        && first.point.block == second.point.block
        && !block_on_cycle(flow, first.point.block))
}

/// The coverage verdict for one scan: `Proven` (an exact event
/// discharges every path), `NotProved` (a non-exact event does — the
/// resolver's unresolvable-loop fallback; counted, never silently
/// valid), `Uncovered` (a path reaches an exit or a replacement write
/// with no discharge — a PROVEN INVALID leak).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) enum Coverage {
    Uncovered,
    NotProved,
    Proven,
}

/// The must-fixpoint: `must[b]` = the best coverage verdict for "every
/// path from block b's start". Monotone descending from Proven (the AND
/// over successors is the min); cycles without an in-cycle event stay
/// Proven (a forever-looping path leaks — the listed corner).
/// Push the points reachable from `p` without crossing a generation
/// CUT of the place (a Replace/FieldReplace that displaces the seated
/// content — the buffer identity changes there; a seating `Init` does
/// not): the next intra-block ordinal (cut when step `p.ordinal` cuts
/// the place), or every successor's start at the terminator.
fn push_successors(
    flow: &Flow,
    place_cuts: Option<&HashMap<BlockId, Vec<u32>>>,
    p: Point,
    stack: &mut Vec<Point>,
) {
    let steps = flow.block_steps(p.block).len() as u32;
    if p.ordinal < steps {
        if !in_ordinal_table(place_cuts, p.block, p.ordinal) {
            stack.push(Point {
                block: p.block,
                ordinal: p.ordinal + 1,
            });
        }
    } else {
        // A back edge / continue closes a loop iteration: the target's
        // events are the NEXT generation's (the body re-establishes its
        // locals per iteration), never this one's — do not traverse it.
        // Same-iteration reachability never needs the closing edge.
        if !matches!(
            flow.term(p.block),
            Terminator::Backedge { .. } | Terminator::Continue { .. }
        ) {
            for &s in flow.successors(p.block) {
                stack.push(Point {
                    block: s,
                    ordinal: 0,
                });
            }
        }
    }
}

/// (e) Path coverage — must-analysis over the unified event model. A
/// generation established by an Init/Replace/FieldReplace write, a
/// producer step, a token adoption, or function entry (params) must be
/// DISCHARGED on every path — by a release or a transfer of its union.
/// There is NO escape predicate: a transfer covers only the paths
/// through it, a final Moved/unseated state is not evidence, and a
/// generation-ending write discharges the old generation ONLY when its
/// release fired on the path (reaching the write unreleased is a LEAK).
/// `must[b]` is a tri-state lattice (Proven > NotProved >
/// Uncovered) so non-exact covering events stay countable as NOT
/// PROVED instead of silently valid.
#[allow(clippy::too_many_arguments)]
pub(crate) fn validate_path_coverage(
    tir: &Tir,
    flow: &Flow,
    pool: &InternPool,
    own: &Ownership,
    sidecar: &FunctionSidecar,
    events: &HashMap<PlaceId, Vec<Event>>,
    table: &GenerationTable,
    summary: &mut ValidationSummary,
) {
    let reachable = reachable_blocks(flow);
    let roots = place_roots(tir, flow, own, sidecar);
    // Group by place-union root (one buffer, one obligation group): a
    // root's events/writes merge across the member places.
    let mut members: HashMap<u32, Vec<PlaceId>> = HashMap::new();
    for pid in 0..flow.places.len() as u32 {
        members
            .entry(roots[pid as usize])
            .or_default()
            .push(PlaceId(pid));
    }
    // Roots to check: anything with an event, plus every tracked
    // storage-owning place (a tracked owner with no event anywhere is
    // itself the suspicion — the must-analysis proves the leak).
    let mut to_check: HashSet<u32> = events.keys().map(|p| roots[p.0 as usize]).collect();
    for owner in own.states.keys() {
        if !flow.place(owner.0).owns_storage(pool) {
            continue;
        }
        // A BORROW-mode param's buffer belongs to the caller — the
        // callee holds no destruction obligation for it (move and inout
        // params are covered: move params are freed by the callee, inout
        // params leave through the write-back transfer events).
        if let Place::Param { index, .. } = flow.place(owner.0)
            && tir.params[*index as usize].mode != ryo_core::tir::ParamMode::Move
        {
            continue;
        }
        to_check.insert(roots[owner.0.0 as usize]);
    }
    for root in to_check {
        let group = &members[&root];
        if group.iter().all(|p| !flow.place(*p).owns_storage(pool)) {
            continue; // views never own
        }
        // View-backed storage is covered like every other group (the
        // exemption is gone): views protect the backing allocation but
        // never discharge it — the base group's releases/transfers must
        // cover every path from each establishment. The
        // view-protection check (the UAF direction, `views` module)
        // still applies in full.
        // Merged events, establishments, and cuts of the whole group.
        let group_events: Vec<Event> = group
            .iter()
            .flat_map(|p| events.get(p).into_iter().flatten().copied())
            .collect();
        let mut group_est: HashMap<BlockId, Vec<u32>> = HashMap::new();
        let mut group_cuts: HashMap<BlockId, Vec<u32>> = HashMap::new();
        for p in group {
            if let Some(by_block) = table.establish.get(p) {
                for (&b, ords) in by_block {
                    group_est.entry(b).or_default().extend_from_slice(ords);
                }
            }
            if let Some(by_block) = table.cut.get(p) {
                for (&b, ords) in by_block {
                    group_cuts.entry(b).or_default().extend_from_slice(ords);
                }
            }
        }
        for ords in group_est.values_mut() {
            ords.sort_unstable();
            ords.dedup();
        }
        for ords in group_cuts.values_mut() {
            ords.sort_unstable();
            ords.dedup();
        }
        let place_cuts = Some(&group_cuts);
        // Obligation points: just after each establishing write of any
        // member, plus function entry for param members.
        let mut obligations: Vec<Point> = Vec::new();
        if group
            .iter()
            .any(|p| matches!(flow.place(*p), Place::Param { .. }))
            && reachable.contains(&flow.entry)
        {
            obligations.push(Point {
                block: flow.entry,
                ordinal: 0,
            });
        }
        for (&b, ords) in &group_est {
            if !reachable.contains(&b) {
                // Unreachable code creates no obligation (a separate
                // category from unknown proof).
                summary.obligations_unreachable += ords.len();
                continue;
            }
            for &ord in ords {
                obligations.push(Point {
                    block: b,
                    ordinal: ord + 1,
                });
            }
        }
        for p in group {
            if let Some(points) = table.adopt.get(p) {
                for point in points {
                    if reachable.contains(&point.block) {
                        obligations.push(*point);
                    } else {
                        summary.obligations_unreachable += 1;
                    }
                }
            }
        }
        if obligations.is_empty() {
            continue; // no establishment point the analysis can anchor
        }
        let must = must_fixup(flow, &group_events, place_cuts);
        for ob in obligations {
            summary.obligations += 1;
            match covered_from(flow, ob.block, ob.ordinal, &group_events, place_cuts, &must) {
                Coverage::Proven => {}
                Coverage::NotProved => summary.obligations_not_proved += 1,
                Coverage::Uncovered => {
                    debug_assert!(
                        false,
                        "path coverage: place union {root} (members {:?}) established at {:?} \
                         has a path to an exit or a replacement write with no release or \
                         transfer of the generation — a leak; {} events for the union",
                        group
                            .iter()
                            .map(|p| format!("{:?}={:?}", p, flow.place(*p)))
                            .collect::<Vec<_>>(),
                        ob,
                        group_events.len()
                    );
                }
            }
        }
    }
}

pub(crate) fn must_fixup(
    flow: &Flow,
    place_events: &[Event],
    place_cuts: Option<&HashMap<BlockId, Vec<u32>>>,
) -> Vec<Coverage> {
    let n = flow.blocks.len();
    let mut must = vec![Coverage::Proven; n];
    loop {
        let mut changed = false;
        for bi in 0..n {
            let b = BlockId(bi as u32);
            let covered = covered_from(flow, b, 0, place_events, place_cuts, &must);
            if covered != must[bi] {
                must[bi] = covered;
                changed = true;
            }
        }
        if !changed {
            return must;
        }
    }
}

/// Scan block `b` from ordinal `from`: a group event at any ordinal
/// discharges with the event's exactness; a generation CUT (a
/// Replace/FieldReplace displacing the seated content) ends the old
/// generation UNdischarged (the write alone establishes the new
/// generation and never releases the old; reaching it unreleased is a
/// leak); falling off the end delegates to the terminator — real exits
/// (Return/ReturnVoid) are Uncovered, `Call{never}`/`Unreachable` are
/// Proven (dead path, leak direction), other terminators take the min
/// over successors' `must`.
pub(crate) fn covered_from(
    flow: &Flow,
    b: BlockId,
    from: u32,
    place_events: &[Event],
    place_cuts: Option<&HashMap<BlockId, Vec<u32>>>,
    must: &[Coverage],
) -> Coverage {
    let steps = flow.block_steps(b).len() as u32;
    for ord in from..=steps {
        let point = Point {
            block: b,
            ordinal: ord,
        };
        let mut best = Coverage::Uncovered;
        for e in place_events {
            if e.point == point {
                // The strongest discharge on this ordinal wins.
                best = best.max(if e.exact {
                    Coverage::Proven
                } else {
                    Coverage::NotProved
                });
            }
        }
        if best != Coverage::Uncovered {
            return best;
        }
        if ord < steps && in_ordinal_table(place_cuts, b, ord) {
            return Coverage::Uncovered;
        }
    }
    match flow.term(b) {
        Terminator::Return { .. } | Terminator::ReturnVoid => Coverage::Uncovered,
        Terminator::Unreachable | Terminator::Call { .. } => Coverage::Proven,
        _ => flow
            .successors(b)
            .iter()
            .map(|&s| must[s.0 as usize])
            .min()
            .unwrap_or(Coverage::Proven),
    }
}
