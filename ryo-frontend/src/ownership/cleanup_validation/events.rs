//! The cleanup-event model: [`Event`] / [`EventKind`] (one executable
//! fact that discharges or releases a generation — see the
//! `ownership::cleanup_validation` module header for the taxonomy) and
//! [`collect_events`], the single collector that turns the sidecar's
//! scheduled destructions, displacement releases, and walk-recorded
//! transfers into per-place event lists at resolved firing points.

use super::super::{Ownership, flow_owner_place};
use super::{TargetPlaces, position_points};
use ryo_core::flow::fire_points::{FirePoint as Point, FirePoints};
use ryo_core::flow::{DropPosition, Flow, Place, PlaceId, Terminator};
use ryo_core::ownership::FunctionSidecar;
use ryo_core::tir::{Tir, TirRef, TirTag};
use ryo_core::types::InternPool;
use std::collections::HashMap;
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
    pub(crate) fn is_transfer_out(&self) -> bool {
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
