//! Backend input preconditions.

use ryo_core::tir::{Tir, TirTag};

/// Reject typed bodies that still contain error placeholders.
pub(crate) fn no_unreachable_in(tirs: &[Tir]) -> bool {
    for tir in tirs {
        // Slot 0 is the reserved sentinel and intentionally has
        // tag = Unreachable in the builder; it is *never* part of a
        // body. Skip it.
        for idx in 1..tir.instructions.len() {
            if matches!(tir.instructions[idx].tag, TirTag::Unreachable) {
                return false;
            }
        }
    }
    true
}

/// Release builds enforce association and supported transitions too.
pub(super) fn require_backend_input<'a>(
    tirs: &'a [Tir],
    flows: &'a [ryo_core::flow::Flow],
    sidecar: &'a ryo_core::ownership::OwnershipSidecar,
    pool: &'a ryo_core::types::InternPool,
) -> Result<CheckedInputs<'a>, String> {
    if tirs.len() != flows.len() || tirs.len() != sidecar.functions.len() {
        return Err("backend requires one Flow and cleanup artifact per body".into());
    }
    if !no_unreachable_in(tirs) {
        return Err("backend requires typed bodies without error placeholders".into());
    }
    for ((tir, flow), cleanup) in tirs.iter().zip(flows).zip(&sidecar.functions) {
        cleanup.require_flow(tir, flow)?;
        flow.require_layout(pool)?;
        if cleanup
            .validation
            .as_ref()
            .is_some_and(|result| !result.proof_complete())
        {
            return Err("backend rejects skipped or incomplete cleanup validation".into());
        }
    }
    Ok(CheckedInputs {
        tirs,
        flows,
        sidecar,
        pool,
    })
}

/// The checked references remain immutable throughout backend preparation.
pub(super) struct CheckedInputs<'a> {
    tirs: &'a [Tir],
    flows: &'a [ryo_core::flow::Flow],
    sidecar: &'a ryo_core::ownership::OwnershipSidecar,
    pool: &'a ryo_core::types::InternPool,
}

impl<'a> CheckedInputs<'a> {
    pub(super) fn prepare_body(&self, index: usize) -> Result<BackendReadyBody<'a>, String> {
        let tir = &self.tirs[index];
        let flow = &self.flows[index];
        let cleanup = &self.sidecar.functions[index];
        let plan = ryo_core::flow::cleanup::CleanupPlan::resolve(tir, flow, cleanup, self.pool)?;
        Ok(BackendReadyBody {
            tir,
            flow,
            cleanup,
            plan,
        })
    }
}

/// Only this validated representation can enter per-body Cranelift emission.
pub(super) struct BackendReadyBody<'a> {
    tir: &'a Tir,
    flow: &'a ryo_core::flow::Flow,
    cleanup: &'a ryo_core::ownership::FunctionSidecar,
    plan: ryo_core::flow::cleanup::CleanupPlan,
}

impl<'a> BackendReadyBody<'a> {
    pub(super) fn into_parts(
        self,
    ) -> (
        &'a Tir,
        &'a ryo_core::flow::Flow,
        &'a ryo_core::ownership::FunctionSidecar,
        ryo_core::flow::cleanup::CleanupPlan,
    ) {
        (self.tir, self.flow, self.cleanup, self.plan)
    }
}
