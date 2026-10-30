use chumsky::Parser;
use chumsky::input::Input;
use ryo_backend::codegen::Codegen;
use ryo_core::ownership::OwnershipSidecar;
use ryo_core::tir::Tir;
use ryo_core::types::InternPool;
use ryo_frontend::grammar::{ParseState, program_parser};
use ryo_frontend::lexer;

fn analyze(
    src: &str,
) -> (
    Vec<Tir>,
    Vec<ryo_core::flow::Flow>,
    InternPool,
    OwnershipSidecar,
) {
    let mut pool = InternPool::new();
    let mut sink = ryo_core::diag::DiagSink::new();
    let tokens = lexer::lex(src, &mut pool, &mut sink);
    assert!(!sink.has_errors(), "lex should succeed");
    let token_stream = tokens[..].split_token_span((0..src.len()).into());
    let mut state = ParseState::new(pool);
    program_parser()
        .parse_with_state(token_stream, &mut state)
        .into_result()
        .expect("parse should succeed");
    let (ast, mut pool) = state.into_parts();
    let mut astgen_sink = ryo_core::diag::DiagSink::new();
    let uir = ryo_frontend::astgen::generate(&ast, &mut pool, &mut astgen_sink);
    assert!(!astgen_sink.has_errors(), "astgen should succeed");
    let mut sema_sink = ryo_core::diag::DiagSink::new();
    let tirs = ryo_frontend::sema::analyze(
        &uir,
        &mut pool,
        &mut sema_sink,
        src,
        std::path::Path::new("test.ryo"),
    );
    assert!(
        !sema_sink.has_errors(),
        "sema should succeed: {:?}",
        sema_sink.into_diags()
    );
    let mut ownership_sink = ryo_core::diag::DiagSink::new();
    let flows: Vec<ryo_core::flow::Flow> = tirs
        .iter()
        .map(|t| ryo_core::flow::Flow::build(t, &pool))
        .collect();
    let sidecar = ryo_frontend::ownership::check(&tirs, &flows, &pool, &mut ownership_sink);
    assert!(!ownership_sink.has_errors(), "ownership should succeed");
    (tirs, flows, pool, sidecar)
}

fn clif_of(src: &str) -> (String, Vec<Tir>, InternPool) {
    let (tirs, flows, pool, sidecar) = analyze(src);
    let mut codegen = Codegen::new_jit().expect("JIT codegen should initialize");
    let clif = codegen
        .compile_and_dump_ir(&tirs, &flows, &pool, &sidecar)
        .expect("codegen should succeed");
    (clif, tirs, pool)
}

#[test]
fn empty_try_and_catch_dispatch_has_no_zero_sized_payload_storage() {
    let (clif, tirs, pool) = clif_of(
        r#"fn inner() -> !int:
	return 7
fn middle() -> !int:
	return try inner()
fn outer() -> !int:
	value = try middle()
	return value
fn main():
	value = outer() catch:
		_: 0
	print(int_to_str(value))
"#,
    );

    for name in ["inner", "middle", "outer"] {
        let tir = tirs
            .iter()
            .find(|tir| pool.str(tir.name) == name)
            .expect("inferred function");
        let error_set = pool.error_union_view(tir.return_type).error_set;
        assert!(pool.enum_view(error_set).is_empty());
        assert_eq!(
            pool.enum_view(error_set).size_align(),
            (0, 0),
            "empty inferred error-set layout must stay zero-sized and uninhabited"
        );
    }
    assert!(
        !clif.contains("explicit_slot 0"),
        "empty error dispatch must not allocate a zero-sized payload slot:\n{clif}"
    );
    let payload_tag_load = clif.split("\n\n").any(|block| {
        block.starts_with("block") && block.contains("load.i32") && !block.contains("br_table")
    });
    assert!(
        !payload_tag_load,
        "empty error dispatch must not load a nonexistent payload tag:\n{clif}"
    );
}
