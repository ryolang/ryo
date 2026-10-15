//! Deterministic inspection of execution and resolved cleanup artifacts.

use super::{Flow, fire_points::FirePoints};
use crate::ownership::FunctionSidecar;
use crate::tir::Tir;
use crate::types::InternPool;
use std::fmt::Write;

impl Flow {
    /// Print existing arenas and resolver results without reconstructing execution.
    pub fn dump(
        &self,
        tir: &Tir,
        cleanup: &FunctionSidecar,
        pool: &InternPool,
    ) -> Result<String, String> {
        cleanup.require_flow(tir, self)?;
        self.require_layout(pool)?;
        let mut text = format!("fn {} entry {:?}\n", pool.str(tir.name), self.entry);
        for (index, place) in self.places.iter().enumerate() {
            let _ = writeln!(text, "  place {index}: {place:?}");
        }
        for (index, block) in self.blocks.iter().enumerate() {
            let id = super::BlockId(index as u32);
            let _ = writeln!(text, "  block {index} unreachable={}", block.unreachable);
            for step in self.block_steps(id) {
                let _ = writeln!(
                    text,
                    "    {:?} {:?} {:?} @ {:?}",
                    step.inst,
                    tir.inst(step.inst).tag,
                    step.facts,
                    tir.span(step.inst)
                );
            }
            let _ = writeln!(
                text,
                "    -> {:?} successors {:?}",
                self.term(id),
                self.successors(id)
            );
        }
        let points = FirePoints::new(self);
        let plan = match super::cleanup::CleanupPlan::resolve(tir, self, cleanup, pool) {
            Ok(plan) => Some(plan),
            Err(error) => {
                let _ = writeln!(
                    text,
                    "  cleanup validation: {:?}: {error}",
                    crate::ownership::ValidationOutcome::Invalid
                );
                None
            }
        };
        for (index, drop) in cleanup.drops.iter().enumerate() {
            let _ = writeln!(
                text,
                "  release {:?} storage {:?} @ {:?} => {:?}",
                drop.target,
                plan.as_ref().map(|plan| plan.releases[index]),
                drop.position,
                points.resolve(drop.position)
            );
        }
        if let Some(plan) = &plan {
            for (index, payload) in plan.payloads.iter().enumerate() {
                if let Some(payload) = payload {
                    let _ = writeln!(text, "  payload release {index}: {payload:?}");
                }
            }
        }
        for transfer in &cleanup.transfers {
            let position = super::DropPosition::AfterStep {
                inst: transfer.site,
            };
            let _ = writeln!(
                text,
                "  transfer {:?} value {:?} at {:?} => {:?}",
                transfer.source,
                transfer.target,
                transfer.site,
                points.resolve(position)
            );
        }
        for allocation in &cleanup.promo_allocs {
            let _ = writeln!(
                text,
                "  promotion allocation {:?} @ {:?} => {:?}",
                allocation.base,
                allocation.position,
                points.resolve(allocation.position)
            );
        }
        for release in &cleanup.promotion_frees {
            let _ = writeln!(
                text,
                "  promotion release {:?} @ {:?} => {:?}",
                release.base,
                release.position,
                points.resolve(release.position)
            );
        }
        for (index, target) in cleanup.free_on_reassign.iter().enumerate() {
            if let Some(target) = target {
                let _ = writeln!(text, "  displacement %{index} releases {target:?}");
            }
        }
        match &cleanup.validation {
            None => text.push_str("  validation: not run (build configuration)\n"),
            Some(summary) if !summary.required_checks_ran => {
                text.push_str("  validation: required checks skipped (invalid source)\n")
            }
            Some(summary) => {
                let status = if summary.proof_complete() {
                    "complete"
                } else {
                    "incomplete"
                };
                let _ = writeln!(text, "  validation: {status} {summary:?}");
            }
        }
        Ok(text)
    }
}
