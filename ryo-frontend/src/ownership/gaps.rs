//! The unified event model's per-gap checks (R4-C): promotion-buffer
//! pairing (Gap 5), displacement at replacement writes (Gap 6), and
//! view-backed storage protection (Gap 4). See `validate.rs` for the
//! event model and the shared machinery.

use super::validate::{
    Coverage, Event, EventKind, GenerationTable, Point, StepIndex, ValidationSummary, covered_from,
    in_ordinal_table, must_fixup, place_roots, position_points, reachable_blocks,
};
use super::{HazardKind, Ownership};
use ryo_core::flow::fire_points::FirePoints;
use ryo_core::flow::{BlockId, Flow, Place, PlaceId};
use ryo_core::ownership::FunctionSidecar;
use ryo_core::tir::{Tir, TirRef};
use std::collections::{HashMap, HashSet};

/// (Gap 5) Promotion buffers: the conditional alloc (promote-on-view)
/// establishes the promoted copy's obligation; the base's releases
/// (`promotion_frees` — the normal anchor plus the return-epilogue
/// copies) discharge it. Per base: every path from an allocation to an
/// exit passes a release of that base (a must-analysis over a synthetic
/// event set; the flag-conditional runtime alloc/release pair
/// statically). A missing release leaves every path of the allocation
/// uncovered — PROVEN INVALID; mutually exclusive conditional releases
/// are per-path legal. Duplicate releases on one path are the
/// double-discharge check's job (the PromoRelease events).
pub(crate) fn validate_promo(
    flow: &Flow,
    fire_points: &FirePoints,
    sidecar: &FunctionSidecar,
    summary: &mut ValidationSummary,
) {
    let reachable = reachable_blocks(flow);
    // Releases per base.
    let mut releases: HashMap<TirRef, Vec<Event>> = HashMap::new();
    for pf in &sidecar.promotion_frees {
        for (point, exact) in position_points(fire_points, pf.position) {
            releases.entry(pf.base).or_default().push(Event {
                point,
                target: pf.base,
                kind: EventKind::PromoRelease,
                pos: pf.position,
                exact,
                field: None,
            });
        }
    }
    for alloc in &sidecar.promo_allocs {
        summary.promo_pairings += 1;
        let points = position_points(fire_points, alloc.position);
        debug_assert!(
            !points.is_empty(),
            "promotion alloc at {:?} does not resolve",
            alloc.position
        );
        let evs = releases.get(&alloc.base).cloned().unwrap_or_default();
        let must = must_fixup(flow, &evs, None);
        for (point, _exact) in points {
            if !reachable.contains(&point.block) {
                continue;
            }
            match covered_from(flow, point.block, point.ordinal, &evs, None, &must) {
                Coverage::Proven | Coverage::NotProved => {}
                Coverage::Uncovered => {
                    debug_assert!(
                        false,
                        "promotion buffer: the alloc at {:?} (base {:?}) has a path to an                          exit with no release of the promoted copy — a leak",
                        point, alloc.base
                    );
                }
            }
        }
    }
}

/// (Gap 6) Displacement at replacement writes: a generation-ending
/// write (an `Assign` reseat — `free_on_reassign` / the consumed-concat
/// family) establishes the NEW generation but is NOT a release; the old
/// generation needs its displacement release. The walk records the
/// release (`free_on_reassign`) and the hazard (`Mutate`) TOGETHER, so
/// the hazard is the corruption-proof evidence: a Mutate hazard whose
/// site names no release for the displaced owner (and no consumed
/// concat, which moves the old buffer into the result instead) means
/// the old generation's release was removed while the write stays — a
/// leak the schedule must not contain. One pass over the hazards.
pub(crate) fn validate_displacement(
    _tir: &Tir,
    flow: &Flow,
    own: &Ownership,
    sidecar: &FunctionSidecar,
    summary: &mut ValidationSummary,
) {
    for &(owner, site, kind) in &own.owner_hazards {
        if !matches!(kind, HazardKind::Mutate) {
            continue;
        }
        summary.displacements += 1;
        let released = sidecar
            .free_on_reassign
            .get(site.index())
            .copied()
            .flatten()
            == Some(owner.tirref(flow));
        let consumed = sidecar
            .consumed_concat_lhs
            .get(site.index())
            .copied()
            .flatten()
            .is_some();
        debug_assert!(
            released || consumed,
            "displacement: the reassign at {site:?} displaces {owner:?} but names no              release (free_on_reassign missing, not a consumed concat) — the old              generation leaks"
        );
    }
}

/// (Gap 4) View-backed storage: views PROTECT the backing allocation
/// but never discharge it (the coverage exemption is gone — the base's
/// destruction obligation is explicit). The view's FINAL USE bounds
/// the owner's earliest valid release: a release of the base group
/// from which the view's final-use point is reachable (no intervening
/// generation cut) may fire BEFORE the view's read — the UAF
/// direction, the strongest check. O(events × views) reachability
/// queries per group, one DFS each — no graph rescan per event beyond
/// the walk itself.
pub(crate) fn validate_view_protection(
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
    let idx = StepIndex::build(flow);
    let mut member_of: HashMap<PlaceId, u32> = HashMap::new();
    let mut by_group: HashMap<u32, Vec<Event>> = HashMap::new();
    for pid in 0..flow.places.len() as u32 {
        member_of.insert(PlaceId(pid), roots[pid as usize]);
    }
    for (place, evs) in events {
        let root = roots[place.0 as usize];
        by_group
            .entry(root)
            .or_default()
            .extend(evs.iter().copied());
    }
    // Views per group: every view whose base chain reaches a member.
    // The view's FINAL USE bounds the base's earliest valid release.
    let base_of = |p: PlaceId| -> Option<PlaceId> {
        match flow.place(p) {
            Place::View { base, .. } => Some(*base),
            _ => None,
        }
    };
    let mut view_uses: Vec<(u32, Point, TirRef)> = Vec::new();
    for pid in 0..flow.places.len() as u32 {
        let view = PlaceId(pid);
        let Some(mut base) = base_of(view) else {
            continue;
        };
        while let Some(next) = base_of(base) {
            base = next;
        }
        let Some(root) = member_of.get(&base).copied() else {
            continue;
        };
        let Place::View { producer, .. } = flow.place(view) else {
            unreachable!()
        };
        let Some(last_use) = own.view_last_use.get(producer.index()).copied().flatten() else {
            continue; // never read: projection lives to scope end
        };
        let Some((b, ord)) = idx.get(last_use) else {
            summary.views_not_proved += 1;
            continue;
        };
        view_uses.push((
            root,
            Point {
                block: b,
                ordinal: ord,
            },
            *producer,
        ));
    }
    for (root, evs) in &by_group {
        let releases: Vec<Event> = evs.iter().copied().filter(|e| e.is_release()).collect();
        if releases.is_empty() {
            continue;
        }
        // The group's merged generation cuts.
        let mut merged: HashMap<BlockId, Vec<u32>> = HashMap::new();
        for (p, r) in &member_of {
            if r != root {
                continue;
            }
            if let Some(by_block) = table.cut.get(p) {
                for (&b, ords) in by_block {
                    merged.entry(b).or_default().extend_from_slice(ords);
                }
            }
        }
        for ords in merged.values_mut() {
            ords.sort_unstable();
            ords.dedup();
        }
        for (vroot, use_point, view_inst) in &view_uses {
            if vroot != root || !reachable.contains(&use_point.block) {
                continue;
            }
            for e in &releases {
                summary.view_checks += 1;
                if !reachable.contains(&e.point.block) {
                    continue;
                }
                if reaches_without_cut(flow, &merged, e.point, *use_point) {
                    debug_assert!(
                        false,
                        "view protection: a release of the base group at {:?}                          ({:?} target {:?}) can fire before the view's final use at                          {:?} (view {:?}) — a use-after-free",
                        e.point, e.kind, e.target, use_point, view_inst
                    );
                }
            }
        }
    }
}

/// True when `to` is reachable from `from` without crossing a
/// generation cut (BFS over points, intra-block ordinals and block
/// successors; back/continue edges traverse — a later-iteration view
/// use IS a real hazard).
fn reaches_without_cut(
    flow: &Flow,
    cuts: &HashMap<BlockId, Vec<u32>>,
    from: Point,
    to: Point,
) -> bool {
    let mut visited: HashSet<Point> = HashSet::new();
    let mut stack = vec![from];
    while let Some(p) = stack.pop() {
        if p == to {
            return true;
        }
        if !visited.insert(p) {
            continue;
        }
        let steps = flow.block_steps(p.block).len() as u32;
        if p.ordinal < steps {
            if !in_ordinal_table(Some(cuts), p.block, p.ordinal) {
                stack.push(Point {
                    block: p.block,
                    ordinal: p.ordinal + 1,
                });
            }
        } else {
            // A loop-closing edge (back edge / continue) reaches the
            // NEXT iteration's uses — the freeze machinery's
            // loop-deferral owns that liveness (the same conservative
            // corner as the coverage exemption). Same-iteration
            // reachability never needs the closing edge.
            if !matches!(
                flow.term(p.block),
                ryo_core::flow::Terminator::Backedge { .. }
                    | ryo_core::flow::Terminator::Continue { .. }
            ) {
                for &s2 in flow.successors(p.block) {
                    stack.push(Point {
                        block: s2,
                        ordinal: 0,
                    });
                }
            }
        }
    }
    false
}
