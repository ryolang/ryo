//! CLIF-level checks for error-union construction and its enum-shaped ABI.

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

fn clif_of(src: &str) -> String {
    let (tirs, flows, pool, sidecar) = analyze(src);
    let mut codegen = Codegen::new_jit().expect("JIT codegen should initialize");
    codegen
        .compile_and_dump_ir(&tirs, &flows, &pool, &sidecar)
        .expect("codegen should succeed")
}

#[test]
fn error_return_uses_sret() {
    let clif = clif_of(
        "error NotFound\n\nfn fail() -> NotFound!int:\n\treturn NotFound\n\nfn main():\n\tresult = fail()\n\tprint(\"ok\\n\")\n",
    );
    assert!(
        clif.contains("sret"),
        "error-union return must use the hidden out-pointer ABI:\n{clif}"
    );
    assert!(
        clif.contains("explicit_slot"),
        "error-union values must use explicit enum-shaped slots:\n{clif}"
    );
}
