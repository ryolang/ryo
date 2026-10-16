//! Promotion-buffer pairing: when a borrowed (non-inout) `str`/`bytes`
//! param is used as a view base, codegen promotes the callee's inline
//! copy to heap (a flag-guarded conditional allocation at the
//! view-creating step). The promoted copy is a DISTINCT allocation
//! identity from the borrowed original: its alloc establishes the
//! obligation, the base's releases (`promotion_frees` — the normal
//! anchor plus the return-epilogue copies) discharge it, and this
//! check pairs them. See the `ownership::validate` module header for
//! the event model.

use super::{
    Coverage, Ctx, Event, EventKind, ValidationSummary, covered_from, must_fixup, position_points,
    reachable_blocks,
};
use ryo_core::tir::TirRef;
use std::collections::HashMap;

/// Per base: every path from an allocation to an exit passes a release
/// of that base (a must-analysis over a synthetic event set; the
/// flag-conditional runtime alloc/release pair is statically paired).
/// A missing release leaves every path of the allocation uncovered —
/// PROVEN INVALID; mutually exclusive conditional releases are
/// per-path legal. Duplicate releases on one path are the
/// double-discharge check's job (the PromoRelease events it sees).
pub(crate) fn validate_promo(ctx: &Ctx<'_>, summary: &mut ValidationSummary) {
    let flow = ctx.flow;
    let reachable = reachable_blocks(flow);
    // Releases per base.
    let mut releases: HashMap<TirRef, Vec<Event>> = HashMap::new();
    for pf in &ctx.sidecar.promotion_frees {
        for (point, exact) in position_points(ctx.fire_points, pf.position) {
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
    for alloc in &ctx.sidecar.promo_allocs {
        summary.promo_pairings += 1;
        let points = position_points(ctx.fire_points, alloc.position);
        debug_assert!(
            !points.is_empty(),
            "promotion alloc at {:?} does not resolve",
            alloc.position
        );
        let evs = releases.get(&alloc.base).cloned().unwrap_or_default();
        let must = must_fixup(flow, &evs, None);
        for (point, exact) in points {
            if !reachable.contains(&point.block) {
                continue;
            }
            let coverage = covered_from(flow, point.block, point.ordinal, &evs, None, &must);
            match coverage {
                Coverage::Proven | Coverage::NotProved => {}
                Coverage::Uncovered => {
                    debug_assert!(
                        false,
                        "promotion buffer: the alloc at {:?} (base {:?}) has a path to an                          exit with no release of the promoted copy — a leak",
                        point, alloc.base
                    );
                }
            }
            // A NON-exact covering event — or a NON-exact obligation
            // point (the alloc position is an estimated lower bound)
            // — cannot prove the pairing: NOT PROVED, counted, never
            // silently valid.
            if matches!(coverage, Coverage::NotProved) || !exact {
                summary.promo_pairings_not_proved += 1;
            }
        }
    }
}
