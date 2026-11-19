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
    validation_error: Option<String>,
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

/// Validated artifacts and the inputs they must retain through backend emission.
///
/// ```compile_fail
/// use ryo_core::compilation::CompilationContext;
/// use ryo_core::source::SourceId;
/// use ryo_core::types::InternPool;
/// use ryo_frontend::pipeline::CleanupProgram;
///
/// fn replace_before_emission(
///     program: &CleanupProgram,
///     context: &mut CompilationContext,
///     pool: &InternPool,
/// ) {
///     let ready = program.require_backend_transition(context, pool).unwrap();
///     context.replace_source(SourceId::NONE, String::new());
///     let _ = ready.tirs();
/// }
/// ```
#[must_use = "keep validated artifacts borrowed through backend emission"]
#[derive(Debug)]
pub struct BackendReady<'a> {
    program: &'a CleanupProgram,
    _context: &'a ryo_core::compilation::CompilationContext,
    pool: &'a InternPool,
}

impl BackendReady<'_> {
    pub fn tirs(&self) -> &[Tir] {
        self.program.tirs()
    }

    pub fn flows(&self) -> &[Flow] {
        self.program.flows()
    }

    pub fn cleanup(&self) -> &OwnershipSidecar {
        self.program.cleanup()
    }

    pub fn pool(&self) -> &InternPool {
        self.pool
    }
}

fn require_valid_call_names(tir: &Tir, pool: &InternPool) -> Result<(), String> {
    for (index, inst) in tir.instructions.iter().enumerate().skip(1) {
        if inst.tag != ryo_core::tir::TirTag::Call {
            continue;
        }
        let r = ryo_core::tir::TirRef::from_raw(
            u32::try_from(index).expect("TIR instruction index exceeds u32"),
        );
        let name = tir.call_view(r).name;
        if (name.raw() as usize) >= pool.string_count()
            || std::str::from_utf8(pool.bytes_payload(name)).is_err()
        {
            return Err(format!("invalid call name in declaration {:?}", tir.decl));
        }
    }
    Ok(())
}

impl BuiltProgram {
    pub fn build(tirs: Vec<Tir>, pool: &InternPool) -> Self {
        let validation_error = tirs.iter().find_map(|tir| {
            tir.require_layout(pool)
                .err()
                .or_else(|| require_valid_call_names(tir, pool).err())
        });
        let flows = if validation_error.is_some() {
            Vec::new()
        } else {
            tirs.iter().map(|tir| Flow::build(tir, pool)).collect()
        };
        Self {
            tirs,
            flows,
            validation_error,
        }
    }

    pub fn analysis_ready<'a>(&'a self, pool: &'a InternPool) -> Result<AnalysisReady<'a>, String> {
        if let Some(error) = &self.validation_error {
            return Err(error.clone());
        }
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
    pool: &InternPool,
    id: DeclId,
    name: ryo_core::types::StringId,
) -> Result<(), String> {
    let declaration = context
        .declarations()
        .get(id)
        .map_err(|error| format!("invalid declaration {id:?}: {error}"))?;
    let registered_name = context
        .declarations()
        .name(id)
        .expect("declaration was just validated");
    if (name.raw() as usize) >= pool.string_count()
        || registered_name.as_bytes() != pool.bytes_payload(name)
    {
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
    pub fn require_backend_transition<'a>(
        &'a self,
        context: &'a ryo_core::compilation::CompilationContext,
        pool: &'a InternPool,
    ) -> Result<BackendReady<'a>, String> {
        if !self.source_clean {
            return Err("backend preparation requires source without errors".into());
        }
        for flow in &self.built.flows {
            flow.require_layout(pool)?;
        }
        let mut body_ids = std::collections::HashSet::with_capacity(self.built.tirs.len());
        let mut body_names = std::collections::HashSet::with_capacity(self.built.tirs.len());
        for tir in &self.built.tirs {
            require_current_declaration(context, pool, tir.decl, tir.name)?;
            let declaration = context
                .declarations()
                .get(tir.decl)
                .expect("declaration was just validated");
            if tir.span.source != declaration.span.source {
                return Err(format!(
                    "body source revision does not match declaration {:?}",
                    tir.decl
                ));
            }
            if !body_names.insert(tir.name) {
                return Err(format!(
                    "duplicate backend name for declaration {:?}",
                    tir.decl
                ));
            }
            body_ids.insert(tir.decl);
        }
        for tir in &self.built.tirs {
            for (index, inst) in tir.instructions.iter().enumerate().skip(1) {
                if inst.tag != ryo_core::tir::TirTag::Call {
                    continue;
                }
                let r = ryo_core::tir::TirRef::from_raw(
                    u32::try_from(index).expect("TIR instruction index exceeds u32"),
                );
                let view = tir.call_view(r);
                if let Some(callee) = view.callee {
                    require_current_declaration(context, pool, callee, view.name)?;
                    if !body_ids.contains(&callee) {
                        return Err(format!(
                            "direct call to {callee:?} has no corresponding body in the backend batch"
                        ));
                    }
                    if ryo_core::builtins::is_runtime_callee(view.name, pool) {
                        return Err(format!(
                            "direct call to {callee:?} conflicts with backend runtime dispatch"
                        ));
                    }
                } else if !ryo_core::builtins::is_runtime_callee(view.name, pool) {
                    return Err(format!(
                        "declaration-free call {:?} is not a registered runtime callee",
                        view.name
                    ));
                }
            }
        }
        Ok(BackendReady {
            program: self,
            _context: context,
            pool,
        })
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
        let mut builder =
            TirBuilder::new(name, vec![], pool.void(), span).with_layout_context(pool);
        let end = builder.return_void(pool.void(), span);
        builder.finish(&[end])
    }

    fn declaration_span(context: &CompilationContext, decl: DeclId) -> ryo_core::span::Span {
        context
            .declarations()
            .get(decl)
            .map(|declaration| declaration.span)
            .unwrap_or(ryo_core::span::Span::EMPTY)
    }

    fn body_with_decl(context: &CompilationContext, pool: &mut InternPool, decl: DeclId) -> Tir {
        body_with_decl_name(context, pool, decl, "main")
    }

    fn body_with_decl_name(
        context: &CompilationContext,
        pool: &mut InternPool,
        decl: DeclId,
        name: &str,
    ) -> Tir {
        let name = pool.intern_str(name);
        let span = declaration_span(context, decl);
        let mut builder = TirBuilder::new(name, vec![], pool.void(), span)
            .with_decl(decl)
            .with_layout_context(pool);
        let end = builder.return_void(pool.void(), span);
        builder.finish(&[end])
    }

    fn body_with_call(
        context: &CompilationContext,
        pool: &mut InternPool,
        body_decl: DeclId,
        callee: DeclId,
    ) -> Tir {
        body_with_call_name(context, pool, body_decl, callee, "main")
    }

    fn body_with_call_name(
        context: &CompilationContext,
        pool: &mut InternPool,
        body_decl: DeclId,
        callee: DeclId,
        call_name: &str,
    ) -> Tir {
        let name = pool.intern_str("main");
        let call_name = pool.intern_str(call_name);
        let span = declaration_span(context, body_decl);
        let mut builder = TirBuilder::new(name, vec![], pool.void(), span)
            .with_decl(body_decl)
            .with_layout_context(pool);
        let call = builder.call(callee, call_name, &[], &[], pool.void(), span);
        let statement = builder.unary(TirTag::ExprStmt, pool.void(), call, span);
        let end = builder.return_void(pool.void(), span);
        builder.finish(&[statement, end])
    }

    fn body_with_builtin(
        context: &CompilationContext,
        pool: &mut InternPool,
        body_decl: DeclId,
    ) -> Tir {
        body_with_builtin_name(context, pool, body_decl, "print")
    }

    fn body_with_builtin_name(
        context: &CompilationContext,
        pool: &mut InternPool,
        body_decl: DeclId,
        call_name: &str,
    ) -> Tir {
        let is_print = call_name == "print";
        let name = pool.intern_str("main");
        let call_name = pool.intern_str(call_name);
        let span = declaration_span(context, body_decl);
        let mut builder = TirBuilder::new(name, vec![], pool.void(), span)
            .with_decl(body_decl)
            .with_layout_context(pool);
        let (args, modes) = if is_print {
            let arg = builder.str_const(pool.intern_str("fixture"), pool.str_(), span);
            (vec![arg], vec![ryo_core::tir::ParamMode::Borrow])
        } else {
            (Vec::new(), Vec::new())
        };
        let call = builder.builtin_call(call_name, &args, &modes, pool.void(), span);
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
        let mut uir = Uir::new().with_layout_context(pool);
        for (index, name) in names.iter().enumerate() {
            let start = index * 10;
            uir.func_bodies.push(FuncBody {
                name: pool.intern_str(name),
                params: vec![],
                return_type: None,
                body: ExtraRange { offset: 0, len: 0 },
                span: ryo_core::span::Span::in_source(source, start, start + 4),
            });
        }
        context.register_functions(&uir, pool).unwrap()
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
            validation_error: None,
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
        let error = analyzed
            .require_backend_transition(&context, &pool)
            .expect_err("source errors must block backend preparation");
        assert!(error.contains("source without errors"), "{error}");
        #[cfg(debug_assertions)]
        assert_eq!(
            analyzed.cleanup().functions[0].validation_outcome(),
            ryo_core::ownership::ValidationOutcome::Skipped
        );
    }

    #[test]
    fn backend_rejects_another_layout_context_before_resolving_names() {
        let mut pool = InternPool::new();
        let mut context = CompilationContext::new();
        let local = register_main(&mut context, &mut pool);
        let analyzed = cleanup_program(body_with_builtin(&context, &mut pool, local), &pool);
        let mut other_pool = InternPool::new();
        other_pool.intern_str("main");
        other_pool.intern_str("print");

        let error = analyzed
            .require_backend_transition(&context, &other_pool)
            .expect_err("Flow must not accept another intern pool");
        assert!(error.contains("type and layout context"), "{error}");
    }

    #[test]
    fn unbound_tir_cannot_build_flow() {
        let mut pool = InternPool::new();
        let name = pool.intern_str("main");
        let span = ryo_core::span::Span::EMPTY;
        let mut builder = TirBuilder::new(name, vec![], pool.void(), span);
        let end = builder.return_void(pool.void(), span);
        let tir = builder.finish(&[end]);

        let error = BuiltProgram::build(vec![tir], &pool)
            .analyze(&pool, &mut DiagSink::new())
            .expect_err("an unbound body must not acquire a pool through Flow");
        assert!(format!("{error:?}").contains("TIR belongs"), "{error:?}");
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
            (
                "foreign body identity",
                body_with_decl(&context, &mut pool, foreign),
            ),
            (
                "missing call identity",
                body_with_call(&context, &mut pool, local, DeclId::NONE),
            ),
            (
                "foreign call identity",
                body_with_call(&context, &mut pool, local, foreign),
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
    fn backend_requires_matching_sourceless_provenance() {
        let mut pool = InternPool::new();
        let mut context = CompilationContext::new();
        let mut sourceless_uir = Uir::new().with_layout_context(&pool);
        sourceless_uir.func_bodies.push(FuncBody {
            name: pool.intern_str("main"),
            params: vec![],
            return_type: None,
            body: ExtraRange { offset: 0, len: 0 },
            span: ryo_core::span::Span::EMPTY,
        });
        let sourceless = context.register_functions(&sourceless_uir, &pool).unwrap()[0];
        let accepted = cleanup_program(body_with_decl(&context, &mut pool, sourceless), &pool);
        assert!(accepted.require_backend_transition(&context, &pool).is_ok());

        let source = context.register_source(std::path::PathBuf::from("main.ryo"), "source".into());
        let mut sourced_body = body_with_decl(&context, &mut pool, sourceless);
        sourced_body.span = ryo_core::span::Span::in_source(source, 0, 0);
        let rejected = cleanup_program(sourced_body, &pool);
        assert!(
            rejected
                .require_backend_transition(&context, &pool)
                .expect_err("a source-less declaration must not accept a sourced body")
                .contains("source revision")
        );

        let sourced = register_main(&mut context, &mut pool);
        let mut sourceless_body = body_with_decl(&context, &mut pool, sourced);
        sourceless_body.span = ryo_core::span::Span::EMPTY;
        let rejected = cleanup_program(sourceless_body, &pool);
        assert!(
            rejected
                .require_backend_transition(&context, &pool)
                .expect_err("a sourced declaration must not accept a source-less body")
                .contains("source revision")
        );
    }

    #[test]
    fn backend_accepts_registered_and_builtin_calls() {
        let mut pool = InternPool::new();
        let mut context = CompilationContext::new();
        let local = register_main(&mut context, &mut pool);

        let direct = cleanup_program(body_with_call(&context, &mut pool, local, local), &pool);
        assert!(direct.require_backend_transition(&context, &pool).is_ok());
        let builtin = cleanup_program(body_with_builtin(&context, &mut pool, local), &pool);
        assert!(builtin.require_backend_transition(&context, &pool).is_ok());
    }

    #[test]
    fn backend_accepts_same_revision_names_from_another_pool() {
        let mut context = CompilationContext::new();
        let mut first_pool = InternPool::new();
        let decl = register_main(&mut context, &mut first_pool);
        let span = context.declarations().get(decl).unwrap().span;
        let first_name = first_pool.find_str("main").unwrap();
        let mut second_pool = InternPool::new();
        second_pool.intern_str("padding");
        let second_name = second_pool.intern_str("main");
        assert_ne!(first_name, second_name);
        let mut second_uir = Uir::new().with_layout_context(&second_pool);
        second_uir.func_bodies.push(FuncBody {
            name: second_name,
            params: vec![],
            return_type: None,
            body: ExtraRange { offset: 0, len: 0 },
            span,
        });
        assert_eq!(
            context
                .register_functions(&second_uir, &second_pool)
                .expect("identical name text must register"),
            vec![decl]
        );
        let tir = body_with_decl(&context, &mut second_pool, decl);
        let analyzed = cleanup_program(tir, &second_pool);

        assert!(
            analyzed
                .require_backend_transition(&context, &second_pool)
                .is_ok()
        );
    }

    #[test]
    fn backend_rejects_calls_without_their_declared_body() {
        let mut pool = InternPool::new();
        let mut context = CompilationContext::new();
        let declarations = register_declarations(&mut context, &mut pool, &["main", "other"]);
        let tir = body_with_call_name(
            &context,
            &mut pool,
            declarations[0],
            declarations[1],
            "other",
        );
        let analyzed = BuiltProgram::build(vec![tir], &pool)
            .analyze(&pool, &mut DiagSink::new())
            .expect("compatible artifacts");

        let error = analyzed
            .require_backend_transition(&context, &pool)
            .expect_err("every direct callee needs a body in the backend batch");
        assert!(error.contains("backend batch"), "{error}");
    }

    #[test]
    fn backend_rejects_duplicate_symbol_names() {
        let mut pool = InternPool::new();
        let mut context = CompilationContext::new();
        let declarations = register_declarations(&mut context, &mut pool, &["main", "main"]);
        let tirs = vec![
            body_with_decl(&context, &mut pool, declarations[0]),
            body_with_decl(&context, &mut pool, declarations[1]),
        ];
        let analyzed = BuiltProgram::build(tirs, &pool)
            .analyze(&pool, &mut DiagSink::new())
            .expect("compatible artifacts");

        let error = analyzed
            .require_backend_transition(&context, &pool)
            .expect_err("name-based backend symbols must be unambiguous");
        assert!(error.contains("duplicate backend name"), "{error}");
    }

    #[test]
    fn backend_rejects_stale_declarations() {
        let mut pool = InternPool::new();
        let mut context = CompilationContext::new();
        let local = register_main(&mut context, &mut pool);
        let analyzed = cleanup_program(body_with_call(&context, &mut pool, local, local), &pool);
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
    fn backend_rejects_body_from_another_source_revision() {
        let mut pool = InternPool::new();
        let mut context = CompilationContext::new();
        let old_decl = register_main(&mut context, &mut pool);
        let old_span = context.declarations().get(old_decl).unwrap().span;
        let new_source = context
            .replace_source(old_span.source, "replacement".into())
            .unwrap();
        let mut new_uir = Uir::new().with_layout_context(&pool);
        new_uir.func_bodies.push(FuncBody {
            name: pool.intern_str("main"),
            params: vec![],
            return_type: None,
            body: ExtraRange { offset: 0, len: 0 },
            span: ryo_core::span::Span::in_source(new_source, 0, 4),
        });
        let new_decl = context.register_functions(&new_uir, &pool).unwrap()[0];
        let mut stale_body = body_with_decl(&context, &mut pool, new_decl);
        stale_body.span = old_span;
        let analyzed = cleanup_program(stale_body, &pool);

        let error = analyzed
            .require_backend_transition(&context, &pool)
            .expect_err("a current declaration must not validate an old body");
        assert!(error.contains("source revision"), "{error}");
    }

    #[test]
    fn build_rejects_invalid_call_names_before_flow() {
        let mut pool = InternPool::new();
        let mut context = CompilationContext::new();
        let local = register_main(&mut context, &mut pool);
        let mut other = InternPool::new();
        for index in 0..32 {
            other.intern_str(&format!("padding{index}"));
        }
        let out_of_range = other.intern_str("invalid");
        let non_utf8 = pool.intern_bytes(&[0xff]);

        for name in [out_of_range, non_utf8] {
            let mut tir = body_with_builtin(&context, &mut pool, local);
            let call_index = tir
                .instructions
                .iter()
                .position(|inst| inst.tag == TirTag::Call)
                .expect("fixture must contain a call");
            let ryo_core::tir::TirData::Extra(range) = tir.instructions[call_index].data else {
                unreachable!("calls carry extra payloads")
            };
            tir.extra[range.as_range().start + 2] = name.raw();

            let error = BuiltProgram::build(vec![tir], &pool)
                .analyze(&pool, &mut DiagSink::new())
                .expect_err("invalid call names must fail before Flow construction");
            assert!(format!("{error:?}").contains("call name"), "{error:?}");
        }
    }

    #[test]
    fn backend_rejects_invalid_tir_name_handles() {
        let mut pool = InternPool::new();
        let mut context = CompilationContext::new();
        let local = register_main(&mut context, &mut pool);
        let mut other = InternPool::new();
        for index in 0..32 {
            other.intern_str(&format!("padding{index}"));
        }
        let out_of_range = other.intern_str("main");
        let non_utf8 = pool.intern_bytes(&[0xff]);

        for name in [out_of_range, non_utf8] {
            let mut tir = body_with_decl(&context, &mut pool, local);
            tir.name = name;
            let analyzed = cleanup_program(tir, &pool);
            let error = analyzed
                .require_backend_transition(&context, &pool)
                .expect_err("invalid TIR names must return a declaration error");
            assert!(error.contains("declaration"), "{error}");
        }
    }

    #[test]
    fn backend_rejects_mismatched_declaration_names() {
        let mut pool = InternPool::new();
        let mut context = CompilationContext::new();
        let declarations = register_declarations(&mut context, &mut pool, &["main", "other"]);
        let main = declarations[0];

        let body = cleanup_program(
            body_with_decl_name(&context, &mut pool, main, "other"),
            &pool,
        );
        assert!(
            body.require_backend_transition(&context, &pool)
                .expect_err("body name must match its declaration")
                .contains("declaration")
        );
        let call = cleanup_program(
            body_with_call_name(&context, &mut pool, main, main, "other"),
            &pool,
        );
        assert!(
            call.require_backend_transition(&context, &pool)
                .expect_err("call name must match its declaration")
                .contains("declaration")
        );
    }

    #[test]
    fn backend_rejects_direct_calls_to_runtime_dispatched_names() {
        let mut pool = InternPool::new();
        let mut context = CompilationContext::new();
        let declarations = register_declarations(&mut context, &mut pool, &["main", "print"]);
        let tirs = vec![
            body_with_call_name(
                &context,
                &mut pool,
                declarations[0],
                declarations[1],
                "print",
            ),
            body_with_decl_name(&context, &mut pool, declarations[1], "print"),
        ];
        let analyzed = BuiltProgram::build(tirs, &pool)
            .analyze(&pool, &mut DiagSink::new())
            .expect("compatible artifacts");

        let error = analyzed
            .require_backend_transition(&context, &pool)
            .expect_err("direct calls must not resolve to a different runtime target");
        assert!(error.contains("runtime dispatch"), "{error}");
    }

    #[test]
    fn backend_rejects_source_only_builtin_calls() {
        let mut pool = InternPool::new();
        let mut context = CompilationContext::new();
        let local = register_main(&mut context, &mut pool);

        for name in ["assert", "panic"] {
            let analyzed = cleanup_program(
                body_with_builtin_name(&context, &mut pool, local, name),
                &pool,
            );
            let error = analyzed
                .require_backend_transition(&context, &pool)
                .expect_err("source-only builtins have no direct runtime target");
            assert!(error.contains("runtime callee"), "{name}: {error}");
        }
    }

    #[test]
    fn backend_rejects_unknown_declaration_free_calls() {
        let mut pool = InternPool::new();
        let mut context = CompilationContext::new();
        let local = register_main(&mut context, &mut pool);
        let analyzed = cleanup_program(
            body_with_builtin_name(&context, &mut pool, local, "main"),
            &pool,
        );

        let error = analyzed
            .require_backend_transition(&context, &pool)
            .expect_err("a user function is not a declaration-free runtime callee");
        assert!(error.contains("declaration"), "{error}");
    }
}
