//! Frontend-owned transitions from typed bodies to cleanup-elaborated Flow.

use ryo_core::declaration::DeclId;
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

fn require_current_declaration(
    context: &ryo_core::compilation::CompilationContext,
    id: DeclId,
    name: ryo_core::types::StringId,
) -> Result<(), String> {
    let declaration = context
        .declarations()
        .get(id)
        .map_err(|error| format!("invalid declaration {id:?}: {error}"))?;
    if declaration.name != name {
        return Err(format!("declaration name mismatch for {id:?}"));
    }
    if declaration.span.source != ryo_core::source::SourceId::NONE {
        context
            .sources()
            .require_current(declaration.span.source)
            .map_err(|error| format!("stale declaration {id:?}: {error}"))?;
    }
    Ok(())
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
        pool: &InternPool,
    ) -> Result<(), String> {
        if !self.source_clean {
            return Err("backend preparation requires source without errors".into());
        }
        for tir in &self.built.tirs {
            require_current_declaration(context, tir.decl, tir.name)?;
            for (index, inst) in tir.instructions.iter().enumerate().skip(1) {
                if inst.tag != ryo_core::tir::TirTag::Call {
                    continue;
                }
                let r = ryo_core::tir::TirRef::from_raw(
                    u32::try_from(index).expect("TIR instruction index exceeds u32"),
                );
                let view = tir.call_view(r);
                if let Some(callee) = view.callee {
                    require_current_declaration(context, callee, view.name)?;
                } else if !ryo_core::builtins::is_runtime_callee(view.name, pool) {
                    return Err(format!(
                        "declaration-free call {:?} is not a registered runtime callee",
                        view.name
                    ));
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
        body_with_decl_name(pool, decl, "main")
    }

    fn body_with_decl_name(pool: &mut InternPool, decl: DeclId, name: &str) -> Tir {
        let name = pool.intern_str(name);
        let span = ryo_core::span::Span::EMPTY;
        let mut builder = TirBuilder::new(name, vec![], pool.void(), span).with_decl(decl);
        let end = builder.return_void(pool.void(), span);
        builder.finish(&[end])
    }

    fn body_with_call(pool: &mut InternPool, body_decl: DeclId, callee: DeclId) -> Tir {
        body_with_call_name(pool, body_decl, callee, "main")
    }

    fn body_with_call_name(
        pool: &mut InternPool,
        body_decl: DeclId,
        callee: DeclId,
        call_name: &str,
    ) -> Tir {
        let name = pool.intern_str("main");
        let call_name = pool.intern_str(call_name);
        let span = ryo_core::span::Span::EMPTY;
        let mut builder = TirBuilder::new(name, vec![], pool.void(), span).with_decl(body_decl);
        let call = builder.call(callee, call_name, &[], &[], pool.void(), span);
        let statement = builder.unary(TirTag::ExprStmt, pool.void(), call, span);
        let end = builder.return_void(pool.void(), span);
        builder.finish(&[statement, end])
    }

    fn body_with_builtin(pool: &mut InternPool, body_decl: DeclId) -> Tir {
        body_with_builtin_name(pool, body_decl, "print")
    }

    fn body_with_builtin_name(pool: &mut InternPool, body_decl: DeclId, call_name: &str) -> Tir {
        let name = pool.intern_str("main");
        let call_name = pool.intern_str(call_name);
        let span = ryo_core::span::Span::EMPTY;
        let mut builder = TirBuilder::new(name, vec![], pool.void(), span).with_decl(body_decl);
        let call = builder.builtin_call(call_name, &[], &[], pool.void(), span);
        let statement = builder.unary(TirTag::ExprStmt, pool.void(), call, span);
        let end = builder.return_void(pool.void(), span);
        builder.finish(&[statement, end])
    }

    fn register_declarations(
        context: &mut CompilationContext,
        pool: &mut InternPool,
        names: &[&str],
    ) -> Vec<DeclId> {
        let source = context.register_source(std::path::PathBuf::from("main.ryo"), "source".into());
        let mut uir = Uir::new();
        for (index, name) in names.iter().enumerate() {
            let start = index * 10;
            uir.func_bodies.push(FuncBody {
                name: pool.intern_str(name),
                params: vec![],
                return_type: pool.void(),
                body: ExtraRange { offset: 0, len: 0 },
                span: ryo_core::span::Span::in_source(source, start, start + 4),
            });
        }
        context.declarations_mut().register_functions(&uir).unwrap()
    }

    fn register_main(context: &mut CompilationContext, pool: &mut InternPool) -> DeclId {
        register_declarations(context, pool, &["main"])[0]
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
        assert!(
            analyzed
                .require_backend_transition(&context, &pool)
                .is_err()
        );
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
                .require_backend_transition(&context, &pool)
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
        assert!(direct.require_backend_transition(&context, &pool).is_ok());
        let builtin = cleanup_program(body_with_builtin(&mut pool, local), &pool);
        assert!(builtin.require_backend_transition(&context, &pool).is_ok());
    }

    #[test]
    fn backend_rejects_stale_declarations() {
        let mut pool = InternPool::new();
        let mut context = CompilationContext::new();
        let local = register_main(&mut context, &mut pool);
        let analyzed = cleanup_program(body_with_call(&mut pool, local, local), &pool);
        let old_source = context.declarations().get(local).unwrap().span.source;
        context
            .replace_source(old_source, "replacement".into())
            .unwrap();

        let error = analyzed
            .require_backend_transition(&context, &pool)
            .expect_err("old-revision artifacts must not be executable");
        assert!(error.contains("declaration"), "{error}");
    }

    #[test]
    fn backend_rejects_mismatched_declaration_names() {
        let mut pool = InternPool::new();
        let mut context = CompilationContext::new();
        let declarations = register_declarations(&mut context, &mut pool, &["main", "other"]);
        let main = declarations[0];

        let body = cleanup_program(body_with_decl_name(&mut pool, main, "other"), &pool);
        assert!(
            body.require_backend_transition(&context, &pool)
                .expect_err("body name must match its declaration")
                .contains("declaration")
        );
        let call = cleanup_program(body_with_call_name(&mut pool, main, main, "other"), &pool);
        assert!(
            call.require_backend_transition(&context, &pool)
                .expect_err("call name must match its declaration")
                .contains("declaration")
        );
    }

    #[test]
    fn backend_rejects_unknown_declaration_free_calls() {
        let mut pool = InternPool::new();
        let mut context = CompilationContext::new();
        let local = register_main(&mut context, &mut pool);
        let analyzed = cleanup_program(body_with_builtin_name(&mut pool, local, "main"), &pool);

        let error = analyzed
            .require_backend_transition(&context, &pool)
            .expect_err("a user function is not a declaration-free runtime callee");
        assert!(error.contains("declaration"), "{error}");
    }
}
