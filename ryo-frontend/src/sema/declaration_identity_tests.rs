use super::*;
use crate::astgen;
use ryo_core::compilation::CompilationContext;
use ryo_core::uir::{CallView, InstRef, InstTag, Uir};
use std::path::PathBuf;

fn lower(context: &mut CompilationContext, input: &str) -> (Uir, InternPool, DiagSink) {
    let source_id = context.register_source(PathBuf::from("test.ryo"), input.into());
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
    assert_eq!(context.declarations().get(decl).unwrap().name, name);
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
