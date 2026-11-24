//! Backend input preconditions.

use ryo_core::tir::{Tir, TirTag};

/// Reject typed bodies that still contain error placeholders.
pub(crate) fn no_unreachable_in(tirs: &[Tir]) -> bool {
    for tir in tirs {
        // Slot 0 is the reserved sentinel and intentionally has
        // tag = Unreachable in the builder; it is *never* part of a
        // body. Skip it.
        for inst in tir.instructions.iter().skip(1) {
            if matches!(inst.tag, TirTag::Unreachable) {
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
        tir.require_layout(pool)?;
        flow.require_layout(pool)?;
        tir.require_bindings()
            .map_err(|error| format!("backend binding validation: {error}"))?;
        if let Err(error) = flow.require_bindings(tir, pool) {
            // A malformed TIR body cannot have a matching published Flow.
            // Rebuild only on this error path to expose lexical scope
            // violations; valid inputs pay no second Flow construction.
            let error = match ryo_core::flow::Flow::try_build(tir, pool) {
                Err(scope_error) => scope_error,
                Ok(_) => error,
            };
            return Err(format!("backend binding validation: {error}"));
        }
        cleanup.require_flow(tir, flow)?;
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
