//! Frontend-owned transitions from typed bodies to cleanup-elaborated Flow.

use ryo_core::diag::DiagSink;
use ryo_core::flow::Flow;
use ryo_core::ownership::OwnershipSidecar;
use ryo_core::tir::Tir;
use ryo_core::types::InternPool;

/// Built execution graphs, paired with the typed bodies that produced them.
#[derive(Debug)]
pub struct BuiltProgram {
    tirs: Vec<Tir>,
    flows: Vec<Flow>,
}

/// Analysis-ready bodies borrow a compatible built artifact.
pub struct AnalysisReady<'a> {
    tirs: &'a [Tir],
    flows: &'a [Flow],
    pool: &'a InternPool,
}

/// Immutable published analysis and cleanup artifacts, including partial results.
#[derive(Debug)]
pub struct CleanupProgram {
    built: BuiltProgram,
    cleanup: OwnershipSidecar,
    source_clean: bool,
}

impl BuiltProgram {
    pub fn build(tirs: Vec<Tir>, pool: &InternPool) -> Self {
        let flows = tirs.iter().map(|tir| Flow::build(tir, pool)).collect();
        Self { tirs, flows }
    }

    pub fn analysis_ready<'a>(&'a self, pool: &'a InternPool) -> Result<AnalysisReady<'a>, String> {
        for (tir, flow) in self.tirs.iter().zip(&self.flows) {
            flow.require_body(tir)?;
            flow.require_layout(pool)?;
        }
        Ok(AnalysisReady {
            tirs: &self.tirs,
            flows: &self.flows,
            pool,
        })
    }

    pub fn analyze(self, pool: &InternPool, sink: &mut DiagSink) -> Result<CleanupProgram, String> {
        let cleanup = self.analysis_ready(pool)?.analyze(sink);
        Ok(CleanupProgram {
            built: self,
            cleanup,
            source_clean: !sink.has_errors(),
        })
    }
}

impl AnalysisReady<'_> {
    fn analyze(self, sink: &mut DiagSink) -> OwnershipSidecar {
        crate::ownership::check(self.tirs, self.flows, self.pool, sink)
    }
}

impl CleanupProgram {
    pub fn tirs(&self) -> &[Tir] {
        &self.built.tirs
    }
    pub fn flows(&self) -> &[Flow] {
        &self.built.flows
    }
    pub fn cleanup(&self) -> &OwnershipSidecar {
        &self.cleanup
    }

    /// Diagnostics may permit inspection while forbidding backend preparation.
    pub fn require_backend_transition(&self) -> Result<(), String> {
        if !self.source_clean {
            return Err("backend preparation requires source without errors".into());
        }
        // Owned published artifacts expose only immutable references. The backend
        // checks imported arena contents once and keeps those references borrowed.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ryo_core::tir::TirBuilder;

    fn body(pool: &mut InternPool) -> Tir {
        let name = pool.intern_str("main");
        let span = ryo_core::span::Span::EMPTY;
        let mut builder = TirBuilder::new(name, vec![], pool.void(), span);
        let end = builder.return_void(pool.void(), span);
        builder.finish(&[end])
    }

    #[test]
    fn analysis_rejects_another_body_with_equal_indices() {
        let mut pool = InternPool::new();
        let first = body(&mut pool);
        let second = body(&mut pool);
        let flow = Flow::build(&first, &pool);
        assert!(
            crate::ownership::try_check(&[second], &[flow], &pool, &mut DiagSink::new()).is_err()
        );
    }

    #[test]
    fn invalid_source_allows_inspection_and_rejects_backend_transition() {
        let mut pool = InternPool::new();
        let tir = body(&mut pool);
        let mut sink = DiagSink::new();
        sink.emit(ryo_core::diag::Diag::error(
            tir.span,
            ryo_core::diag::DiagCode::TypeMismatch,
            "invalid body",
        ));
        let analyzed = BuiltProgram::build(vec![tir], &pool)
            .analyze(&pool, &mut sink)
            .expect("compatible artifacts");
        assert_eq!(analyzed.tirs().len(), 1);
        assert!(analyzed.require_backend_transition().is_err());
        #[cfg(debug_assertions)]
        assert_eq!(
            analyzed.cleanup().functions[0].validation_outcome(),
            ryo_core::ownership::ValidationOutcome::Skipped
        );
    }
}
