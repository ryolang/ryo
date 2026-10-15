//! CodSpeed benchmarks for the Ryo compiler frontend.
//!
//! These benchmarks exercise the CPU-bound parts of the front end:
//! lexing (logos scan + indentation pre-processing + interning) and
//! parsing (chumsky grammar over the interned token stream). Both are
//! pure, deterministic work with no I/O, which makes them a good fit
//! for CodSpeed's simulation instrument.

use chumsky::Parser;
use chumsky::input::Input;
use ryo_core::types::InternPool;
use ryo_frontend::lexer::{self, Span, Token};
use ryo_frontend::parser::{ParseState, program_parser};

fn main() {
    divan::main();
}

/// A small, representative program: nested control flow and calls.
const FIZZBUZZ: &str = r#"fn fizzbuzz(n: int):
	if n % 15 == 0:
		print("FizzBuzz\n")
	elif n % 3 == 0:
		print("Fizz\n")
	elif n % 5 == 0:
		print("Buzz\n")

fn main():
	fizzbuzz(3)
	fizzbuzz(5)
	fizzbuzz(15)
	fizzbuzz(7)
"#;

/// Deep recursion / arithmetic — the canonical fibonacci example.
const FIBONACCI: &str = r#"fn fibonacci(n: int) -> int:
	if n <= 1:
		return n
	return fibonacci(n - 1) + fibonacci(n - 2)

fn main():
	assert(fibonacci(40) == 102334155, "fib(40) check")
	print("assert passed, fib(40) is correct\n")
"#;

/// Build a larger synthetic source file by repeating a function
/// template. This stresses the lexer and parser with a realistic
/// multi-declaration program rather than a single tiny snippet.
fn large_source(functions: usize) -> String {
    let mut src = String::new();
    for i in 0..functions {
        src.push_str(&format!(
            "fn compute_{i}(a: int, b: int) -> int:\n\
             \tresult = a + b * {i} - (a % {n})\n\
             \tif result > 0:\n\
             \t\treturn result + fibonacci({i})\n\
             \treturn a - b\n\n",
            i = i,
            n = i + 1
        ));
    }
    src
}

/// Lex a source string, returning the interned token stream.
fn lex(src: &str) -> Vec<(Token, Span)> {
    let mut pool = InternPool::new();
    let mut sink = ryo_core::diag::DiagSink::new();
    let tokens = lexer::lex(src, &mut pool, &mut sink);
    assert!(!sink.has_errors(), "lex should succeed");
    tokens
}

/// Full lex + parse of a source string.
fn parse(src: &str) {
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
    let (ast, _pool) = state.into_parts();
    divan::black_box(&ast);
}

// ============================================================================
// Lexer benchmarks
// ============================================================================

#[divan::bench(args = [("fizzbuzz", FIZZBUZZ), ("fibonacci", FIBONACCI)])]
fn lex_snippet(bencher: divan::Bencher, case: (&str, &str)) {
    let src = case.1;
    bencher.bench(|| divan::black_box(lex(divan::black_box(src))));
}

#[divan::bench(args = [16, 64, 256])]
fn lex_large(bencher: divan::Bencher, functions: usize) {
    let src = large_source(functions);
    bencher.bench(|| divan::black_box(lex(divan::black_box(&src))));
}

// ============================================================================
// Parser benchmarks (full lex + parse pipeline)
// ============================================================================

#[divan::bench(args = [("fizzbuzz", FIZZBUZZ), ("fibonacci", FIBONACCI)])]
fn parse_snippet(bencher: divan::Bencher, case: (&str, &str)) {
    let src = case.1;
    bencher.bench(|| parse(divan::black_box(src)));
}

#[divan::bench(args = [16, 64, 256])]
fn parse_large(bencher: divan::Bencher, functions: usize) {
    let src = large_source(functions);
    bencher.bench(|| parse(divan::black_box(&src)));
}

// ============================================================================
// Sema / Middle-End benchmarks (AstGen + Sema pipeline)
// ============================================================================

/// Full parse of a source string, returning the AST arena and populated InternPool.
fn parse_program(src: &str) -> (ryo_core::ast::Ast, InternPool) {
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
    let (ast, pool) = state.into_parts();
    (ast, pool)
}

#[divan::bench(args = [("fizzbuzz", FIZZBUZZ), ("fibonacci", FIBONACCI)])]
fn sema_snippet(bencher: divan::Bencher, case: (&str, &str)) {
    let src = case.1;
    bencher
        .with_inputs(|| parse_program(src))
        .bench_values(|(program, mut pool)| {
            let mut sink = ryo_core::diag::DiagSink::new();
            let uir = ryo_frontend::astgen::generate(&program, &mut pool, &mut sink);
            let mut sema_sink = ryo_core::diag::DiagSink::new();
            let tirs = ryo_frontend::sema::analyze(
                &uir,
                &mut pool,
                &mut sema_sink,
                src,
                std::path::Path::new("bench.ryo"),
            );
            divan::black_box(tirs);
        });
}

#[divan::bench(args = [16, 64, 256])]
fn sema_large(bencher: divan::Bencher, functions: usize) {
    let src = large_source(functions);
    bencher
        .with_inputs(|| parse_program(&src))
        .bench_values(|(program, mut pool)| {
            let mut sink = ryo_core::diag::DiagSink::new();
            let uir = ryo_frontend::astgen::generate(&program, &mut pool, &mut sink);
            let mut sema_sink = ryo_core::diag::DiagSink::new();
            let tirs = ryo_frontend::sema::analyze(
                &uir,
                &mut pool,
                &mut sema_sink,
                &src,
                std::path::Path::new("bench.ryo"),
            );
            divan::black_box(tirs);
        });
}

// ============================================================================
// Ownership benchmarks (AstGen + Sema + Ownership pipeline)
// ============================================================================

/// Deeply nested control flow: a while loop wrapping `depth` levels of
/// if/else, with break/continue at the leaves. Stresses the ownership
/// pass's branch tracking, which answers per-statement queries against
/// every enclosing branch.
fn nested_control_source(functions: usize, depth: usize) -> String {
    let mut src = String::new();
    for i in 0..functions {
        src.push_str(&format!(
            "fn nested_{i}(n: int) -> int:\n\
             \tmut total = 0\n\
             \tmut i = 0\n\
             \twhile i < n:\n"
        ));
        for d in 0..depth {
            let indent = "\t".repeat(d + 2);
            src.push_str(&format!("{indent}if i % {m} == 0:\n", m = d + 2));
        }
        let inner = "\t".repeat(depth + 2);
        src.push_str(&format!("{inner}total += i\n{inner}i += 1\n"));
        for d in (0..depth).rev() {
            let indent = "\t".repeat(d + 2);
            let leaf = if d == 0 { "break" } else { "continue" };
            src.push_str(&format!("{indent}else:\n{indent}\t{leaf}\n"));
        }
        src.push_str("\treturn total\n\n");
    }
    src
}

/// Call-heavy functions mixing the three parameter modes: `move`,
/// `inout` (with `&` at the call site), and implicit borrow. Stresses
/// the ownership pass's per-call borrowed/moved/view-borrowed tracking.
fn call_heavy_source(functions: usize) -> String {
    let mut src = String::new();
    for i in 0..functions {
        src.push_str(&format!(
            "fn mix_{i}(move owned: str, inout shared: str, read: str, n: int) -> int:\n\
             \tshared = shared + owned\n\
             \treturn n + read.len()\n\n\
             fn caller_{i}():\n\
             \tmut a = \"alpha{i}\"\n\
             \tmut b = \"beta{i}\"\n\
             \tmix_{i}(\"m1\", &a, b, {i})\n\
             \tmix_{i}(\"m2\", &b, a, {i} + 1)\n\
             \tmix_{i}(\"m3\", &a, \"lit\", {i} + 2)\n\n"
        ));
    }
    src
}

/// Parse + astgen + sema a source string, returning the TIRs and pool.
fn sema_program(src: &str) -> (Vec<ryo_core::tir::Tir>, InternPool) {
    let (program, mut pool) = parse_program(src);
    let mut sink = ryo_core::diag::DiagSink::new();
    let uir = ryo_frontend::astgen::generate(&program, &mut pool, &mut sink);
    let mut sema_sink = ryo_core::diag::DiagSink::new();
    let tirs = ryo_frontend::sema::analyze(
        &uir,
        &mut pool,
        &mut sema_sink,
        src,
        std::path::Path::new("bench.ryo"),
    );
    assert!(!sema_sink.has_errors(), "sema should succeed");
    (tirs, pool)
}

#[divan::bench(args = [(4, 4), (64, 8)])]
fn ownership_nested_control(bencher: divan::Bencher, case: (usize, usize)) {
    let src = nested_control_source(case.0, case.1);
    bencher
        .with_inputs(|| sema_program(&src))
        .bench_values(|(tirs, pool)| {
            let mut sink = ryo_core::diag::DiagSink::new();
            let flows: Vec<ryo_core::flow::Flow> = tirs
                .iter()
                .map(|t| ryo_core::flow::Flow::build(t, &pool))
                .collect();
            let sidecar =
                ryo_frontend::ownership::check(divan::black_box(&tirs), &flows, &pool, &mut sink);
            assert!(!sink.has_errors(), "ownership should succeed");
            divan::black_box(sidecar);
        });
}

#[divan::bench(args = [16, 256])]
fn ownership_call_heavy(bencher: divan::Bencher, functions: usize) {
    let src = call_heavy_source(functions);
    bencher
        .with_inputs(|| sema_program(&src))
        .bench_values(|(tirs, pool)| {
            let mut sink = ryo_core::diag::DiagSink::new();
            let flows: Vec<ryo_core::flow::Flow> = tirs
                .iter()
                .map(|t| ryo_core::flow::Flow::build(t, &pool))
                .collect();
            let sidecar =
                ryo_frontend::ownership::check(divan::black_box(&tirs), &flows, &pool, &mut sink);
            assert!(!sink.has_errors(), "ownership should succeed");
            divan::black_box(sidecar);
        });
}

/// A 40-char string literal — past the SSO inline cap, so every
/// binding initialized from it owns a real destruction event.
fn heap_lit(tag: usize) -> String {
    format!("\"{tag:040}\"")
}

/// One owner, many views: a heap str sliced `views` times at staggered
/// 2-byte offsets, each view read exactly once (its final use), the
/// owner read last. Stresses projection liveness/last-use tracking and
/// the view-protection + coverage checks over many protection lifetimes
/// per group.
fn many_views_source(functions: usize, views: usize) -> String {
    let mut src = String::new();
    for i in 0..functions {
        src.push_str(&format!(
            "fn views_{i}() -> int:\n\tmut total = 0\n\ts = {}\n",
            heap_lit(i)
        ));
        for v in 0..views {
            let off = v * 2;
            src.push_str(&format!(
                "\tv{v} = s[{off}:{end}]\n\ttotal += v{v}.len()\n",
                end = off + 2
            ));
        }
        src.push_str("\ttotal += s.len()\n\treturn total\n\n");
    }
    src
}

/// Many destruction events: `drops` sequential heap str locals per
/// function, each initialized and last-used in declaration order — one
/// destruction event per binding. Stresses the cleanup schedule's
/// per-owner anchors and the coverage must-analysis over many groups.
fn many_drops_source(functions: usize, drops: usize) -> String {
    let mut src = String::new();
    for i in 0..functions {
        src.push_str(&format!("fn drops_{i}() -> int:\n\tmut total = 0\n"));
        for d in 0..drops {
            src.push_str(&format!(
                "\ts{d} = {lit}\n\ttotal += s{d}.len()\n",
                lit = heap_lit(i * 1000 + d)
            ));
        }
        src.push_str("\treturn total\n\n");
    }
    src
}

#[divan::bench(args = [4, 16])]
fn ownership_many_views(bencher: divan::Bencher, views: usize) {
    let src = many_views_source(4, views);
    bencher
        .with_inputs(|| sema_program(&src))
        .bench_values(|(tirs, pool)| {
            let mut sink = ryo_core::diag::DiagSink::new();
            let flows: Vec<ryo_core::flow::Flow> = tirs
                .iter()
                .map(|t| ryo_core::flow::Flow::build(t, &pool))
                .collect();
            let sidecar =
                ryo_frontend::ownership::check(divan::black_box(&tirs), &flows, &pool, &mut sink);
            assert!(!sink.has_errors(), "ownership should succeed");
            divan::black_box(sidecar);
        });
}

#[divan::bench(args = [16, 64])]
fn ownership_many_drops(bencher: divan::Bencher, drops: usize) {
    let src = many_drops_source(4, drops);
    bencher
        .with_inputs(|| sema_program(&src))
        .bench_values(|(tirs, pool)| {
            let mut sink = ryo_core::diag::DiagSink::new();
            let flows: Vec<ryo_core::flow::Flow> = tirs
                .iter()
                .map(|t| ryo_core::flow::Flow::build(t, &pool))
                .collect();
            let sidecar =
                ryo_frontend::ownership::check(divan::black_box(&tirs), &flows, &pool, &mut sink);
            assert!(!sink.has_errors(), "ownership should succeed");
            divan::black_box(sidecar);
        });
}
