//! Object-symbol pins for the stack-limit check.
//! The CLIF text dump renders global-value symbols opaquely, so the
//! earliest pipeline stage where the callee name survives is the AOT
//! object's symbol table: every recursive call (a call-graph cycle
//! edge) is guarded by a check whose deferred cold block calls the
//! runtime's `ryo_stack_overflow` abort, so the emitted object must
//! contain that undefined symbol — and must not when no function is
//! recursive (non-recursive code carries no check at all).

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
fn recursive_call_references_stack_overflow_abort() {
    // The self-call is a cycle edge: it is preceded by a load of
    // RYO_STACK_LIMIT and a branch to a cold block calling
    // ryo_stack_overflow. The data symbol reference does not survive
    // as a name at any earlier stage inspectable from here, so pin the
    // callee in the object symbol table instead (same pattern as
    // obj_bytes_dead_drop.rs).
    let obj = obj_bytes_of(
        "fn f(n: int) -> int:\n\tif n <= 0:\n\t\treturn 0\n\treturn f(n - 1) + 1\n\n\
         fn main():\n\tprint(f(1))\n",
    );
    assert!(
        contains(&obj, b"ryo_stack_overflow"),
        "recursive call must be guarded by the stack check"
    );
}

#[test]
fn mutual_recursion_references_stack_overflow_abort() {
    // Cycle edges are found per strongly connected component, so a
    // mutually recursive pair is guarded even without self-calls.
    let obj = obj_bytes_of(
        "fn even(n: int) -> bool:\n\tif n == 0:\n\t\treturn true\n\treturn odd(n - 1)\n\n\
         fn odd(n: int) -> bool:\n\tif n == 0:\n\t\treturn false\n\treturn even(n - 1)\n\n\
         fn main():\n\tprint(even(4))\n",
    );
    assert!(
        contains(&obj, b"ryo_stack_overflow"),
        "mutually recursive calls must be guarded by the stack check"
    );
}

#[test]
fn non_recursive_program_has_no_stack_check() {
    // No call-graph cycle, so no check and no reference to the abort.
    let obj = obj_bytes_of(
        "fn g(n: int) -> int:\n\treturn n * 2\n\n\
         fn f(n: int) -> int:\n\treturn g(n) + 1\n\n\
         fn main():\n\tprint(f(1))\n",
    );
    assert!(
        !contains(&obj, b"ryo_stack_overflow"),
        "non-recursive code must not carry the stack check"
    );
}
