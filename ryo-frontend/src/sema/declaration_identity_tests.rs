use super::*;
use crate::astgen;
use ryo_core::compilation::CompilationContext;
use ryo_core::uir::{CallView, InstRef, InstTag, Uir};
use std::path::PathBuf;

fn lower(context: &mut CompilationContext, input: &str) -> (Uir, InternPool, DiagSink) {
    let source_id = context.register_source(PathBuf::from("test.ryo"), input.into());
    lower_source(context, source_id)
}

fn lower_source(
    context: &CompilationContext,
    source_id: ryo_core::source::SourceId,
) -> (Uir, InternPool, DiagSink) {
    let source = context.sources().get(source_id).unwrap();
    let mut pool = InternPool::new();
    let (program, parse_diags) =
        crate::parse::parse_source_with_source(source, &mut pool).expect("parse");
    assert!(parse_diags.is_empty());
    let mut sink = DiagSink::new();
    let uir = astgen::generate(&program, &mut pool, &mut sink);
    (uir, pool, sink)
}

fn first_call(uir: &Uir) -> CallView<'_> {
    for (index, inst) in uir.instructions.iter().enumerate().skip(1) {
        if inst.tag == InstTag::Call {
            return uir.call_view(InstRef::from_raw(index as u32));
        }
    }
    panic!("expected a direct call")
}

#[test]
fn direct_call_resolution_uses_declaration_identity() {
    let input = "fn fact(n: int) -> int:\n\tif n <= 1:\n\t\treturn 1\n\treturn n * fact(n - 1)\nfn main():\n\tprint(fact(3))\n";
    let mut context = CompilationContext::new();
    let (uir, mut pool, mut sink) = lower(&mut context, input);
    let mut sema = Sema::new(
        &uir,
        &mut pool,
        &mut sink,
        input,
        Path::new("test.ryo"),
        &mut context,
    );
    sema.resolve_signatures();
    let name = sema.pool.intern_str("fact");
    let decl = sema.name_to_decl[&name];

    assert_eq!(sema.signatures[&decl].return_type, sema.pool.int());
    let plan = prepare_call(&mut sema, &first_call(&uir), Span::EMPTY);
    match plan {
        CallPlan::User { callee, .. } => assert_eq!(callee, decl),
        _ => panic!("expected user call"),
    }
    assert_eq!(
        sema.context.declarations().name(decl).unwrap(),
        sema.pool.str(name)
    );
}

#[test]
fn same_name_in_separate_contexts_is_not_conflated() {
    let input = "fn same() -> int:\n\treturn 1\nfn main():\n\tprint(same())\n";
    let mut first = CompilationContext::new();
    let (first_uir, mut first_pool, mut first_sink) = lower(&mut first, input);
    let first_sema = Sema::new(
        &first_uir,
        &mut first_pool,
        &mut first_sink,
        input,
        Path::new("first.ryo"),
        &mut first,
    );
    let mut second = CompilationContext::new();
    let (second_uir, mut second_pool, mut second_sink) = lower(&mut second, input);
    let second_sema = Sema::new(
        &second_uir,
        &mut second_pool,
        &mut second_sink,
        input,
        Path::new("second.ryo"),
        &mut second,
    );
    let first_name = first_sema.pool.find_str("same").unwrap();
    let second_name = second_sema.pool.find_str("same").unwrap();
    let first_id = first_sema.name_to_decl[&first_name];
    let second_id = second_sema.name_to_decl[&second_name];

    assert_eq!(first_id.index(), second_id.index());
    assert_ne!(first_id, second_id);
    assert!(first.declarations().get(second_id).is_err());
    assert!(second.declarations().get(first_id).is_err());
    assert_eq!(
        first.declarations().get(first_id).unwrap().span.source,
        first_uir.func_bodies[0].span.source
    );
    assert_eq!(
        second.declarations().get(second_id).unwrap().span.source,
        second_uir.func_bodies[0].span.source
    );
}

#[test]
fn conflicting_registration_emits_internal_error_without_tirs() {
    let input = "fn main():\n\tpass\n";
    let mut context = CompilationContext::new();
    let (mut uir, mut pool, mut sink) = lower(&mut context, input);
    let first = Sema::run(
        &uir,
        &mut pool,
        &mut sink,
        input,
        Path::new("test.ryo"),
        &mut context,
    );
    assert!(!first.tirs.is_empty());
    uir.func_bodies[0].name = pool.intern_str("other");

    let report = Sema::run(
        &uir,
        &mut pool,
        &mut sink,
        input,
        Path::new("test.ryo"),
        &mut context,
    );

    assert!(report.tirs.is_empty());
    assert!(
        sink.into_diags()
            .iter()
            .any(|diag| diag.code == DiagCode::InternalError)
    );
}

#[test]
fn source_revision_recompiles_direct_recursive_and_inferred_calls() {
    let cases = [
        (
            "direct",
            "fn twice(value: int) -> int:\n\treturn value * 2\nfn main():\n\tprint(twice(2))\n",
            "fn twice(value: int) -> int:\n\treturn value * 2\nfn main():\n\tprint(twice(3))\n",
        ),
        (
            "recursive",
            "fn fact(n: int) -> int:\n\tif n <= 1:\n\t\treturn 1\n\treturn n * fact(n - 1)\nfn main():\n\tprint(fact(3))\n",
            "fn fact(n: int) -> int:\n\tif n <= 1:\n\t\treturn 1\n\treturn n * fact(n - 1)\nfn main():\n\tprint(fact(4))\n",
        ),
        (
            "inferred",
            "error Failure\nfn fail() -> !int:\n\treturn Failure\nfn forward() -> !int:\n\tvalue = try fail()\n\treturn value\nfn main():\n\tvalue = forward() catch:\n\t\tFailure: 7\n\tprint(value)\n",
            "error Failure\nfn fail() -> !int:\n\treturn Failure\nfn forward() -> !int:\n\tvalue = try fail()\n\treturn value\nfn main():\n\tvalue = forward() catch:\n\t\tFailure: 8\n\tprint(value)\n",
        ),
    ];

    for (name, first_input, second_input) in cases {
        let mut context = CompilationContext::new();
        let (first_uir, mut first_pool, mut first_sink) = lower(&mut context, first_input);
        let first_tirs = analyze(
            &first_uir,
            &mut first_pool,
            &mut first_sink,
            first_input,
            Path::new("test.ryo"),
            &mut context,
        );
        assert!(!first_sink.has_errors(), "{name} first revision");

        let old_source = first_uir.func_bodies[0].span.source;
        let new_source = context
            .replace_source(old_source, second_input.into())
            .unwrap();
        let (second_uir, mut second_pool, mut second_sink) = lower_source(&context, new_source);
        let second_tirs = analyze(
            &second_uir,
            &mut second_pool,
            &mut second_sink,
            second_input,
            Path::new("test.ryo"),
            &mut context,
        );

        assert!(!second_sink.has_errors(), "{name} second revision");
        assert_eq!(second_tirs.len(), first_tirs.len(), "{name}");
        for (first, second) in first_tirs.iter().zip(&second_tirs) {
            assert_ne!(first.decl, second.decl, "{name}");
        }

        let expected_calls: &[(&str, &str)] = match name {
            "direct" => &[("main", "twice")],
            "recursive" => &[("fact", "fact"), ("main", "fact")],
            "inferred" => &[("forward", "fail"), ("main", "forward")],
            _ => unreachable!(),
        };
        for &(caller, target) in expected_calls {
            let caller_name = second_pool.find_str(caller).unwrap();
            let target_name = second_pool.find_str(target).unwrap();
            let caller_body = second_tirs
                .iter()
                .find(|tir| tir.name == caller_name)
                .expect("expected caller body");
            let target_body = second_tirs
                .iter()
                .find(|tir| tir.name == target_name)
                .expect("expected callee body");
            let has_current_call =
                caller_body
                    .instructions
                    .iter()
                    .enumerate()
                    .skip(1)
                    .any(|(index, inst)| {
                        inst.tag == ryo_core::tir::TirTag::Call
                            && caller_body
                                .call_view(ryo_core::tir::TirRef::from_raw(index as u32))
                                .callee
                                == Some(target_body.decl)
                    });
            assert!(
                has_current_call,
                "{name}: {caller} must call the new {target}"
            );
        }
        let analyzed = crate::pipeline::BuiltProgram::build(second_tirs, &second_pool)
            .analyze(&second_pool, &mut DiagSink::new())
            .expect("second revision must produce compatible artifacts");
        let _ready = analyzed
            .require_backend_transition(&context, &second_pool)
            .unwrap_or_else(|error| {
                panic!("{name}: second revision must pass backend preparation: {error}")
            });
    }
}

#[test]
fn built_program_rejects_tir_from_another_pool() {
    let input = "fn main():\n\tprint(\"one\")\n";
    let mut context = CompilationContext::new();
    let (uir, mut pool, mut sink) = lower(&mut context, input);
    let tirs = analyze(
        &uir,
        &mut pool,
        &mut sink,
        input,
        Path::new("test.ryo"),
        &mut context,
    );
    assert!(!sink.has_errors());
    let mut other_pool = InternPool::new();
    other_pool.intern_str("main");
    other_pool.intern_str("print");
    other_pool.intern_str("two");
    for index in 0..32 {
        other_pool.intern_str(&format!("padding{index}"));
    }

    let error = crate::pipeline::BuiltProgram::build(tirs, &other_pool)
        .analyze(&other_pool, &mut DiagSink::new())
        .expect_err("TIR must not be interpreted through another pool");
    assert!(format!("{error:?}").contains("TIR belongs"), "{error:?}");
}

#[test]
fn source_revision_recompiles_implicit_main_for_backend() {
    let first_input = "print(1)\n";
    let second_input = "print(2)\n";
    let mut context = CompilationContext::new();
    let (first_uir, mut first_pool, mut first_sink) = lower(&mut context, first_input);
    let old_source = first_uir.func_bodies[0].span.source;
    let first_tirs = analyze(
        &first_uir,
        &mut first_pool,
        &mut first_sink,
        first_input,
        Path::new("test.ryo"),
        &mut context,
    );
    assert!(!first_sink.has_errors());
    assert!(!first_tirs.is_empty());

    let new_source = context
        .replace_source(old_source, second_input.into())
        .unwrap();
    let (second_uir, mut second_pool, mut second_sink) = lower_source(&context, new_source);
    assert_eq!(second_uir.func_bodies[0].span.source, new_source);
    let second_tirs = analyze(
        &second_uir,
        &mut second_pool,
        &mut second_sink,
        second_input,
        Path::new("test.ryo"),
        &mut context,
    );
    assert!(!second_sink.has_errors());
    assert_eq!(second_tirs.len(), 1);
    assert_eq!(second_tirs[0].name, second_pool.find_str("main").unwrap());

    let analyzed = crate::pipeline::BuiltProgram::build(second_tirs, &second_pool)
        .analyze(&second_pool, &mut second_sink)
        .expect("implicit main should analyze");
    assert!(
        analyzed
            .require_backend_transition(&context, &second_pool)
            .is_ok()
    );
}

#[test]
fn empty_implicit_main_tracks_source_revisions() {
    let mut context = CompilationContext::new();
    let old_source = context.register_source(PathBuf::from("test.ryo"), String::new());
    let (uir, mut pool, mut sink) = lower_source(&context, old_source);
    let tirs = analyze(
        &uir,
        &mut pool,
        &mut sink,
        "",
        Path::new("test.ryo"),
        &mut context,
    );
    assert!(!sink.has_errors());

    let analyzed = crate::pipeline::BuiltProgram::build(tirs, &pool)
        .analyze(&pool, &mut sink)
        .expect("empty implicit main should analyze");
    assert_eq!(analyzed.tirs().len(), 1);
    assert_eq!(analyzed.tirs()[0].name, pool.find_str("main").unwrap());
    assert!(analyzed.require_backend_transition(&context, &pool).is_ok());

    let new_source = context
        .replace_source(old_source, "print(1)\n".into())
        .unwrap();
    let error = analyzed
        .require_backend_transition(&context, &pool)
        .expect_err("the empty old revision must become stale");
    assert!(error.contains("stale declaration"), "{error}");

    let (new_uir, mut new_pool, mut new_sink) = lower_source(&context, new_source);
    let new_tirs = analyze(
        &new_uir,
        &mut new_pool,
        &mut new_sink,
        "print(1)\n",
        Path::new("test.ryo"),
        &mut context,
    );
    assert!(!new_sink.has_errors());
    let replacement = crate::pipeline::BuiltProgram::build(new_tirs, &new_pool)
        .analyze(&new_pool, &mut new_sink)
        .expect("replacement implicit main should analyze");
    assert_eq!(replacement.tirs().len(), 1);
    assert_eq!(
        replacement.tirs()[0].name,
        new_pool.find_str("main").unwrap()
    );
    assert!(
        replacement
            .require_backend_transition(&context, &new_pool)
            .is_ok()
    );
}

#[test]
fn flow_rejects_use_outside_binding_scope() {
    let mut pool = InternPool::new();
    let name = pool.intern_str("main");
    let binding_name = pool.intern_str("inner");
    let int = pool.int();
    let void = pool.void();
    let span = Span::EMPTY;
    let mut builder =
        ryo_core::tir::TirBuilder::new(name, vec![], void, span).with_layout_context(&pool);
    let inner = builder.register_binding(ryo_core::tir::BindingInfo::new(
        binding_name,
        int,
        ryo_core::tir::BindingKind::Local {
            mutable: false,
            borrowed: false,
        },
        ryo_core::tir::ScopeId::from_index(7),
        span,
    ));
    let cond = builder.bool_const(true, pool.bool_(), span);
    let one = builder.int_const(1, int, span);
    let decl = builder.var_decl(inner, false, int, one, span);
    let branch = builder.if_stmt(cond, &[decl], &[], None, void, span);
    let read = builder.var(inner, int, span);
    let statement = builder.unary(ryo_core::tir::TirTag::ExprStmt, void, read, span);
    let tir = builder.finish(&[branch, statement]);

    let error = crate::pipeline::BuiltProgram::build(vec![tir], &pool)
        .analyze(&pool, &mut DiagSink::new())
        .expect_err("an inner-scope binding must not escape through Flow publication");
    assert!(
        format!("{error:?}").contains("outside binding scope"),
        "{error:?}"
    );
}

#[test]
fn flow_preserves_reserved_binding_recovery() {
    let cases = [
        (
            "parameter",
            "fn f(__ryo_p: int) -> int:\n\treturn __ryo_p\n\nfn main():\n\tprint(f(1))\n",
        ),
        ("local", "fn main():\n\t__ryo_value = 1\n"),
        ("annotated local", "fn main():\n\t__ryo_value: int = 1\n"),
    ];
    for (label, input) in cases {
        let mut context = CompilationContext::new();
        let (uir, mut pool, mut sink) = lower(&mut context, input);
        let tirs = analyze(
            &uir,
            &mut pool,
            &mut sink,
            input,
            Path::new("test.ryo"),
            &mut context,
        );
        let analyzed = crate::pipeline::BuiltProgram::build(tirs, &pool)
            .analyze(&pool, &mut sink)
            .unwrap_or_else(|error| panic!("{label}: recovery must not become an ICE: {error}"));
        assert!(!analyzed.tirs().is_empty(), "{label}");
        let diags = sink.into_diags();
        assert!(
            diags
                .iter()
                .any(|diag| diag.code == DiagCode::ReservedIdentifier),
            "{label}: missing reserved-name diagnostic: {diags:?}"
        );
        assert!(
            !diags
                .iter()
                .any(|diag| diag.code == DiagCode::InternalError),
            "{label}: recovery emitted an internal error: {diags:?}"
        );
    }
}
