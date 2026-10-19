//! View-backed storage protection: views borrow the backing
//! allocation but never own it — the base's destruction obligation is
//! explicit and its coverage runs through the shared must-analysis in
//! `super` (no view exemption). This module owns the UAF-direction
//! check: the view's FINAL USE bounds the base's earliest valid
//! release, and a release of the base group from which the view's
//! final-use point is reachable (no intervening generation boundary)
//! may fire BEFORE the view's read — a use-after-free. See the
//! `ownership::validate` module header for the event model.

use super::{
    Ctx, Event, EventKind, Point, StepIndex, ValidationSummary, in_ordinal_table, place_roots,
    position_points, reachable_blocks,
};
use ryo_core::flow::{BlockId, Place, PlaceId};
use ryo_core::tir::TirRef;
use std::collections::{HashMap, HashSet};

/// The view-protection check (the UAF direction, the strongest check).
/// Reachability FOLLOWS loop-closing edges: a release at the end of
/// iteration N reaches a use at the start of iteration N+1, and for
/// pre-loop storage that is a real hazard; a per-iteration allocation's
/// establishment (its slice / promotion-alloc step) stops the walk.
/// O(events × views) reachability queries per group, one DFS each — no
/// graph rescan per event beyond the walk itself.
pub(crate) fn validate_view_protection(ctx: &Ctx<'_>, summary: &mut ValidationSummary) {
    let flow = ctx.flow;
    let reachable = reachable_blocks(flow);
    let roots = place_roots(ctx.tir, flow, ctx.own, ctx.sidecar);
    let idx = StepIndex::build(flow);
    let mut member_of: HashMap<PlaceId, u32> = HashMap::new();
    let mut by_group: HashMap<u32, Vec<Event>> = HashMap::new();
    for pid in 0..flow.places.len() as u32 {
        member_of.insert(PlaceId(pid), roots[pid as usize]);
    }
    for (place, evs) in ctx.events {
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
        let Some(last_use) = ctx
            .own
            .view_last_use
            .get(producer.index())
            .copied()
            .flatten()
        else {
            continue; // never read: projection lives to scope end
        };
        let Some((b, ord)) = idx.get(last_use) else {
            summary.views_not_proved += 1;
            continue;
        };
        if !reachable.contains(&b) {
            // The view's final use never executes — no protection
            // obligation on this path (unreachable, not unproved).
            summary.views_unreachable += 1;
            continue;
        }
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
        // The group's merged generation boundaries, split by role for
        // the hazard walk: CUTS (a Replace/FieldReplace displaces the
        // seated buffer) block always; ESTABLISHMENTS (an Init /
        // producer / adoption step, plus the promotion-buffer
        // ALLOCATION at the view-creating step — the promoted copy's
        // generation) block only after a loop-closing edge, where they
        // belong to the next iteration's fresh allocation. Never
        // crossing one is exactly what proves pre-loop storage live
        // across a backedge.
        let mut merged_cuts: HashMap<BlockId, Vec<u32>> = HashMap::new();
        let mut merged_est: HashMap<BlockId, Vec<u32>> = HashMap::new();
        for (p, r) in &member_of {
            if r != root {
                continue;
            }
            if let Some(by_block) = ctx.table.cut.get(p) {
                for (&b, ords) in by_block {
                    merged_cuts.entry(b).or_default().extend_from_slice(ords);
                }
            }
            if let Some(by_block) = ctx.table.establish.get(p) {
                for (&b, ords) in by_block {
                    merged_est.entry(b).or_default().extend_from_slice(ords);
                }
            }
            if let Some(points) = ctx.table.adopt.get(p) {
                for point in points {
                    merged_est
                        .entry(point.block)
                        .or_default()
                        .push(point.ordinal);
                }
            }
        }
        // Promotion-buffer allocations establish the promoted copy's
        // generation at the view-creating step (a per-iteration slice
        // re-allocates; the previous iteration's release cannot dangle
        // the next iteration's buffer). The group's promo bases are the
        // targets of its PromoRelease events.
        let promo_bases: HashSet<TirRef> = evs
            .iter()
            .filter(|e| e.kind == EventKind::PromoRelease)
            .map(|e| e.target)
            .collect();
        if !promo_bases.is_empty() {
            for alloc in &ctx.sidecar.promo_allocs {
                if !promo_bases.contains(&alloc.base) {
                    continue;
                }
                for (point, _exact) in position_points(ctx.fire_points, alloc.position) {
                    merged_est
                        .entry(point.block)
                        .or_default()
                        .push(point.ordinal);
                }
            }
        }
        for ords in merged_cuts.values_mut().chain(merged_est.values_mut()) {
            ords.sort_unstable();
            ords.dedup();
        }
        for (vroot, use_point, view_inst) in &view_uses {
            if vroot != root {
                continue;
            }
            for e in &releases {
                summary.view_checks += 1;
                if !reachable.contains(&e.point.block) {
                    // Never fires — no hazard can materialize
                    // (unreachable, not unproved).
                    summary.views_unreachable += 1;
                    continue;
                }
                // Provability gate: a hazard is PROVABLE only when the
                // release fires exactly where modeled; a non-exact
                // (estimated) release point cannot prove a violation —
                // the pair is NOT PROVED, counted, never asserted.
                if !e.exact {
                    summary.views_not_proved += 1;
                    continue;
                }
                // A crossed establishment protects only the per-buffer
                // promo lifecycle (a PromoRelease frees one promo
                // generation; a use reached past a later promo
                // establishment reads a newer buffer). A base-owner
                // release does not get that shelter: promo generations
                // derive from the base, so a base free can still dangle
                // a use reached past a promo alloc.
                let est_protects = e.kind == EventKind::PromoRelease;
                if reaches_without_cut(
                    flow,
                    &merged_cuts,
                    &merged_est,
                    e.point,
                    *use_point,
                    est_protects,
                ) {
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
/// generation boundary of the group (BFS over points; back edges and
/// continue edges traverse — a release at the end of iteration N
/// reaches a view use at the start of iteration N+1, and for pre-loop
/// storage that IS a real hazard: the same buffer is read again).
///
/// Boundaries: a CUT (a Replace/FieldReplace displaces the seated
/// buffer) blocks always. An ESTABLISHMENT (an Init / producer /
/// adoption / promotion-allocation step) blocks only once the walk has
/// crossed a loop-closing edge: before that, the establishment ahead
/// of the walk is the release's OWN generation (first iteration entry,
/// or the promotion site's free-before-overwrite pattern — the
/// release fires before the conditional alloc, which is sound); after
/// a closing edge it belongs to the NEXT iteration — a per-iteration
/// body local — whose uses read a different allocation. When
/// `est_protects` is set (promo releases), ANY establishment crossed on
/// the walk also discharges the pair — a use reached past an
/// establishment reads a generation established after the released
/// promo buffer's.
///
/// The visited set is on points (finite: blocks × ordinals), so the
/// cycles terminate; the `cycled` flag rides the stack.
fn reaches_without_cut(
    flow: &ryo_core::flow::Flow,
    cuts: &HashMap<BlockId, Vec<u32>>,
    establishes: &HashMap<BlockId, Vec<u32>>,
    from: Point,
    to: Point,
    est_protects: bool,
) -> bool {
    // `crossed_est` tracks whether the path passed an establishment
    // point: with `est_protects` (promo releases only), a use reached
    // after ANY establishment on the walk reads a generation
    // established after the released promo buffer's — the released
    // buffer cannot dangle it (the one-behind release inside a loop
    // body crosses the rebind's establishment before reaching the next
    // iteration's read). Keyed in `visited` so both flag states of a
    // point are explored.
    let mut visited: HashSet<(Point, bool)> = HashSet::new();
    let mut stack = vec![(from, false, false)];
    while let Some((p, cycled, crossed_est)) = stack.pop() {
        if p == to {
            // A use AT an active establishment boundary reads the
            // generation the boundary establishes (the re-allocated
            // copy) — the released buffer cannot dangle it.
            return !((est_protects && crossed_est)
                || (cycled && in_ordinal_table(Some(establishes), p.block, p.ordinal)));
        }
        if !visited.insert((p, crossed_est)) {
            continue;
        }
        let saw_establishment =
            p != from && in_ordinal_table(Some(establishes), p.block, p.ordinal);
        let crossed_est = crossed_est || saw_establishment;
        let steps = flow.block_steps(p.block).len() as u32;
        if p.ordinal < steps {
            let boundary = in_ordinal_table(Some(cuts), p.block, p.ordinal)
                || (cycled && in_ordinal_table(Some(establishes), p.block, p.ordinal));
            if !boundary {
                stack.push((
                    Point {
                        block: p.block,
                        ordinal: p.ordinal + 1,
                    },
                    cycled,
                    crossed_est,
                ));
            }
        } else {
            // A loop-closing edge reaches the NEXT iteration's uses —
            // mark the walk cycled: establishments beyond this point
            // are the next generation's.
            let closes = matches!(
                flow.term(p.block),
                ryo_core::flow::Terminator::Backedge { .. }
                    | ryo_core::flow::Terminator::Continue { .. }
            );
            for &s2 in flow.successors(p.block) {
                stack.push((
                    Point {
                        block: s2,
                        ordinal: 0,
                    },
                    cycled || closes,
                    crossed_est,
                ));
            }
        }
    }
    false
}
