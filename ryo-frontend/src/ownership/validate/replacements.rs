//! Displacement at replacement writes: a generation-ending write (an
//! `Assign` reseat — `free_on_reassign` / the consumed-concat family)
//! establishes the NEW generation but is NOT a release; the old
//! generation needs its displacement release. The ownership walk
//! records the release (`free_on_reassign`) and the hazard (`Mutate`)
//! TOGETHER, so the hazard is the corruption-proof evidence: a Mutate
//! hazard whose site names no release for the displaced owner (and no
//! consumed concat, which moves the old buffer into the result
//! instead) means the old generation's release was removed while the
//! write stays — a leak the schedule must not contain. See the
//! `ownership::validate` module header for the event model.

use super::super::HazardKind;
use super::{Ctx, ValidationSummary};

/// One pass over the walk-recorded hazards: every generation-ending
/// write names its displacement release (or a consumed concat that
/// moves the old buffer into the in-place result).
pub(crate) fn validate_displacement(ctx: &Ctx<'_>, summary: &mut ValidationSummary) {
    for &(owner, site, kind) in &ctx.own.owner_hazards {
        if !matches!(kind, HazardKind::Mutate) {
            continue;
        }
        summary.displacements += 1;
        let released = ctx
            .sidecar
            .free_on_reassign
            .get(site.index())
            .copied()
            .flatten()
            == Some(owner.tirref(ctx.flow));
        let consumed = ctx
            .sidecar
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
