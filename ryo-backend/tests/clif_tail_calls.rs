//! CLIF-level pinning tests for I-178: eligible self-tail-calls lower to
//! Cranelift `return_call` (O(1) stack), and a call with a scheduled free
//! after it silently falls back to `call` + `return`.

use chumsky::Parser;
use chumsky::input::Input;
use ryo_backend::codegen::Codegen;
use ryo_core::ownership::OwnershipSidecar;
use ryo_core::tir::Tir;
use ryo_core::types::InternPool;
use ryo_frontend::lexer;
use ryo_frontend::parser::{ParseState, program_parser};

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

fn clif_of(src: &str) -> String {
    let (tirs, pool, sidecar) = analyze(src);
    let mut codegen = Codegen::new_jit().expect("JIT codegen should initialize");
    codegen
        .compile_and_dump_ir(&tirs, &pool, &sidecar)
        .expect("codegen should succeed")
}

#[test]
fn return_call_emitted_for_scalar_self_tail_call() {
    let clif = clif_of(
        "fn f(n: int) -> int:\n\tif n == 0:\n\t\treturn 0\n\treturn f(n - 1)\n\nfn main():\n\tprint(f(3))\n",
    );
    assert!(
        clif.contains("return_call"),
        "expected return_call:\n{clif}"
    );
}

#[test]
fn return_call_emitted_for_void_self_tail_call_stmt() {
    let clif =
        clif_of("fn f(n: int):\n\tif n == 0:\n\t\treturn\n\tf(n - 1)\n\nfn main():\n\tf(3)\n");
    assert!(
        clif.contains("return_call"),
        "expected return_call:\n{clif}"
    );
}

#[test]
fn no_return_call_when_arg_carries_drop() {
    // The concat temp `acc + "x"` is rejected twice over: condition 3
    // (fat arg, not scalar-only) and condition 4 (the temp's free is
    // anchored on the consuming Call). Either alone forces the silent
    // fallback to call + return.
    let clif = clif_of(
        "fn f(n: int, acc: str) -> int:\n\tif n == 0:\n\t\treturn 0\n\treturn f(n - 1, acc + \"x\")\n\nfn main():\n\tprint(f(3, \"a\"))\n",
    );
    assert!(
        !clif.contains("return_call"),
        "a drop anchored on the call (and a fat arg) must fall back to call + return:\n{clif}"
    );
}

#[test]
fn tail_call_sweeps_free_materialized_during_arg_marshalling() {
    // `s`'s only recursive-path use is the `s.len()` arg read, so its
    // last-use free is anchored on that read. The read has no cached
    // repr when tail_call_eligible runs (before marshalling), so the
    // free passes the pending-sweep mirror — and becomes sweep-eligible
    // the moment marshalling evaluates the arg. emit_body never sweeps
    // after a Return terminator, so the tail path must sweep between
    // marshalling and the return_call; otherwise every recursive frame
    // leaks the float_to_str buffer (float_to_str is deliberately NOT
    // provably-inline, so the free is really emitted). float_to_str's
    // sret shape and len's selects don't matter; the pin is: a `call`
    // (the free) on the line immediately before the `return_call`.
    let clif = clif_of(
        "fn f(x: int, n: int):\n\ts = float_to_str(1.5)\n\tif n == 0:\n\t\treturn\n\tf(s.len(), n - 1)\n\nfn main():\n\tf(0, 3)\n",
    );
    assert!(
        clif.contains("return_call"),
        "the trailing self-call should be an eligible tail call:\n{clif}"
    );
    let free_before_return_call = clif
        .lines()
        .zip(clif.lines().skip(1))
        .any(|(a, b)| a.contains("call") && b.contains("return_call"));
    assert!(
        free_before_return_call,
        "the free materialized during arg marshalling must be swept before the return_call:\n{clif}"
    );
}

#[test]
fn no_return_call_with_fat_param_self_call() {
    // v1 scope is scalar-only: a self-call passing a fat (str) param
    // has a fat arg and silently falls back to call + return. The
    // function is still compiled with CallConv::Tail (the pre-pass
    // marked it) and must compile correctly as a plain Tail-conv
    // function.
    let clif = clif_of(
        "fn f(s: str, n: int) -> int:\n\tif n == 0:\n\t\treturn 0\n\treturn f(s, n - 1)\n\nfn main():\n\tprint(f(\"ab\", 3))\n",
    );
    assert!(
        clif.contains("function u0:0(i64, i64, i64, i64) -> i64 tail"),
        "the pre-pass should still mark the candidate Tail-conv:\n{clif}"
    );
    assert!(
        !clif.contains("return_call"),
        "a fat-arg self-call must fall back to call + return:\n{clif}"
    );
}
