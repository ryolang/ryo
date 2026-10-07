//! Object-symbol pin for the I-177 prologue stack check.
//! The CLIF text dump renders global-value symbols opaquely, so the
//! earliest pipeline stage where the callee name survives is the AOT
//! object's symbol table: every user function's prologue references
//! the runtime's `ryo_stack_overflow` abort (via the deferred cold
//! block), so the emitted object must contain that undefined symbol.

use chumsky::Parser;
use chumsky::input::Input;
use ryo_backend::codegen::Codegen;
use ryo_core::ownership::OwnershipSidecar;
use ryo_core::tir::Tir;
use ryo_core::types::InternPool;
use ryo_frontend::lexer;
use ryo_frontend::parser::{ParseState, program_parser};
use target_lexicon::Triple;

fn analyze(src: &str) -> (Vec<Tir>, InternPool, OwnershipSidecar) {
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
    let mut sema_sink = ryo_core::diag::DiagSink::new();
    let tirs = ryo_frontend::sema::analyze(
        &uir,
        &mut pool,
        &mut sema_sink,
        src,
        std::path::Path::new("test.ryo"),
    );
    assert!(!sema_sink.has_errors(), "sema should succeed");
    let mut ownership_sink = ryo_core::diag::DiagSink::new();
    let sidecar = ryo_frontend::ownership::check(&tirs, &pool, &mut ownership_sink);
    assert!(!ownership_sink.has_errors(), "ownership should succeed");
    (tirs, pool, sidecar)
}

fn obj_bytes_of(src: &str) -> Vec<u8> {
    let (tirs, pool, sidecar) = analyze(src);
    let mut codegen = Codegen::new_aot(Triple::host()).expect("AOT codegen should initialize");
    codegen
        .compile(&tirs, &pool, &sidecar, false)
        .expect("compile should succeed");
    codegen.finish().expect("object emission should succeed")
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn prologue_references_stack_overflow_abort() {
    // Every user function's prologue loads RYO_STACK_LIMIT and branches
    // to a cold block calling ryo_stack_overflow on failure. The data
    // symbol reference does not survive as a name at any earlier stage
    // inspectable from here, so pin the callee in the object symbol
    // table instead (same pattern as obj_bytes_dead_drop.rs).
    let obj = obj_bytes_of("fn f(n: int) -> int:\n\treturn n\n\nfn main():\n\tprint(f(1))\n");
    assert!(
        contains(&obj, b"ryo_stack_overflow"),
        "prologue stack check must reference ryo_stack_overflow"
    );
}
