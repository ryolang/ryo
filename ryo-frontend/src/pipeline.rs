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

/// Immutable published analysis and cleanup artifacts.
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

    pub fn analyze(
        self,
        pool: &InternPool,
        sink: &mut DiagSink,
    ) -> Result<CleanupProgram, crate::ownership::OwnershipError> {
        let cleanup = self.analysis_ready(pool)?.analyze(sink)?;
        Ok(CleanupProgram {
            built: self,
            cleanup,
            source_clean: !sink.has_errors(),
        })
    }
}

impl AnalysisReady<'_> {
    fn analyze(
        self,
        sink: &mut DiagSink,
    ) -> Result<OwnershipSidecar, crate::ownership::OwnershipError> {
        crate::ownership::try_check(self.tirs, self.flows, self.pool, sink)
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
    pub fn require_backend_transition(
        &self,
        context: &ryo_core::compilation::CompilationContext,
    ) -> Result<(), String> {
        if !self.source_clean {
            return Err("backend preparation requires source without errors".into());
        }
        for tir in &self.built.tirs {
            context
                .declarations()
                .get(tir.decl)
                .map_err(|error| format!("invalid TIR body declaration {:?}: {error}", tir.decl))?;
            for (index, inst) in tir.instructions.iter().enumerate().skip(1) {
                if inst.tag != ryo_core::tir::TirTag::Call {
                    continue;
                }
                let r = ryo_core::tir::TirRef::from_raw(
                    u32::try_from(index).expect("TIR instruction index exceeds u32"),
                );
                if let Some(callee) = tir.call_view(r).callee {
                    context.declarations().get(callee).map_err(|error| {
                        format!("invalid direct-call declaration {callee:?}: {error}")
                    })?;
                }
            }
        }
        // Owned published artifacts expose only immutable references. The backend
        // checks imported arena contents once and keeps those references borrowed.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ryo_core::compilation::CompilationContext;
    use ryo_core::declaration::DeclId;
    use ryo_core::extra::ExtraRange;
    use ryo_core::tir::{TirBuilder, TirTag};
    use ryo_core::uir::{FuncBody, Uir};

    fn body(pool: &mut InternPool) -> Tir {
        let name = pool.intern_str("main");
        let span = ryo_core::span::Span::EMPTY;
        let mut builder = TirBuilder::new(name, vec![], pool.void(), span);
        let end = builder.return_void(pool.void(), span);
        builder.finish(&[end])
    }

    fn body_with_decl(pool: &mut InternPool, decl: DeclId) -> Tir {
        let name = pool.intern_str("main");
        let span = ryo_core::span::Span::EMPTY;
        let mut builder = TirBuilder::new(name, vec![], pool.void(), span).with_decl(decl);
        let end = builder.return_void(pool.void(), span);
        builder.finish(&[end])
    }

    fn body_with_call(pool: &mut InternPool, body_decl: DeclId, callee: DeclId) -> Tir {
        let name = pool.intern_str("main");
        let span = ryo_core::span::Span::EMPTY;
        let mut builder = TirBuilder::new(name, vec![], pool.void(), span).with_decl(body_decl);
        let call = builder.call(callee, name, &[], &[], pool.void(), span);
        let statement = builder.unary(TirTag::ExprStmt, pool.void(), call, span);
        let end = builder.return_void(pool.void(), span);
        builder.finish(&[statement, end])
    }

    fn body_with_builtin(pool: &mut InternPool, body_decl: DeclId) -> Tir {
        let name = pool.intern_str("main");
        let print = pool.intern_str("print");
        let span = ryo_core::span::Span::EMPTY;
        let mut builder = TirBuilder::new(name, vec![], pool.void(), span).with_decl(body_decl);
        let call = builder.builtin_call(print, &[], &[], pool.void(), span);
        let statement = builder.unary(TirTag::ExprStmt, pool.void(), call, span);
        let end = builder.return_void(pool.void(), span);
        builder.finish(&[statement, end])
    }

    fn register_main(context: &mut CompilationContext, pool: &mut InternPool) -> DeclId {
        let mut uir = Uir::new();
        uir.func_bodies.push(FuncBody {
            name: pool.intern_str("main"),
            params: vec![],
            return_type: pool.void(),
            body: ExtraRange { offset: 0, len: 0 },
            span: ryo_core::span::Span::EMPTY,
        });
        context.declarations_mut().register_functions(&uir).unwrap()[0]
    }

    fn cleanup_program(tir: Tir, pool: &InternPool) -> CleanupProgram {
        BuiltProgram::build(vec![tir], pool)
            .analyze(pool, &mut DiagSink::new())
            .expect("compatible artifacts")
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
    fn failed_analysis_does_not_publish_cleanup_program() {
        let mut pool = InternPool::new();
        let first = body(&mut pool);
        let second = body(&mut pool);
        let flow = Flow::build(&first, &pool);
        let built = BuiltProgram {
            tirs: vec![second],
            flows: vec![flow],
        };

        let result = built.analyze(&pool, &mut DiagSink::new());

        assert!(matches!(
            result,
            Err(crate::ownership::OwnershipError::IncompatibleArtifacts(_))
        ));
    }

    #[test]
    fn invalid_source_allows_inspection_and_rejects_backend_transition() {
        let mut pool = InternPool::new();
        let context = CompilationContext::new();
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
        assert!(analyzed.require_backend_transition(&context).is_err());
        #[cfg(debug_assertions)]
        assert_eq!(
            analyzed.cleanup().functions[0].validation_outcome(),
            ryo_core::ownership::ValidationOutcome::Skipped
        );
    }

    #[test]
    fn backend_rejects_missing_and_foreign_declarations() {
        let mut pool = InternPool::new();
        let mut context = CompilationContext::new();
        let local = register_main(&mut context, &mut pool);
        let mut foreign_context = CompilationContext::new();
        let foreign = register_main(&mut foreign_context, &mut pool);

        for (label, tir) in [
            ("missing body identity", body(&mut pool)),
            ("foreign body identity", body_with_decl(&mut pool, foreign)),
            (
                "missing call identity",
                body_with_call(&mut pool, local, DeclId::NONE),
            ),
            (
                "foreign call identity",
                body_with_call(&mut pool, local, foreign),
            ),
        ] {
            let analyzed = cleanup_program(tir, &pool);
            let error = analyzed
                .require_backend_transition(&context)
                .expect_err(label);
            assert!(error.contains("declaration"), "{label}: {error}");
        }
    }

    #[test]
    fn backend_accepts_registered_and_builtin_calls() {
        let mut pool = InternPool::new();
        let mut context = CompilationContext::new();
        let local = register_main(&mut context, &mut pool);

        let direct = cleanup_program(body_with_call(&mut pool, local, local), &pool);
        assert!(direct.require_backend_transition(&context).is_ok());
        let builtin = cleanup_program(body_with_builtin(&mut pool, local), &pool);
        assert!(builtin.require_backend_transition(&context).is_ok());
    }
}
