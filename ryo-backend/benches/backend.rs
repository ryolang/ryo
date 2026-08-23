//! CodSpeed benchmarks for the Ryo compiler backend (Cranelift codegen).
//!
//! These benchmarks run the full in-memory pipeline — lex, parse, astgen,
//! sema, ownership — as input setup, and measure only the codegen stage:
//! `Codegen<JITModule>::compile` over the typed IR. No linking and no
//! execution, so the work is pure CPU and deterministic, a good fit for
//! CodSpeed's simulation instrument.

use chumsky::Parser;
use chumsky::input::Input;
use ryo_backend::codegen::Codegen;
use ryo_core::ownership::OwnershipSidecar;
use ryo_core::tir::Tir;
use ryo_core::types::InternPool;
use ryo_frontend::lexer;
use ryo_frontend::parser::program_parser;

fn main() {
    divan::main();
}

/// Arithmetic-dense functions — many instructions per body, which
/// stresses codegen's per-instruction value bookkeeping.
fn arith_source(functions: usize) -> String {
    let mut src = String::new();
    for i in 0..functions {
        src.push_str(&format!(
            "fn compute_{i}(a: int, b: int) -> int:\n\
             \tresult = a + b * {i} - (a % {n})\n\
             \tif result > 0:\n\
             \t\treturn result + compute_{p}(a, b)\n\
             \treturn a - b\n\n",
            i = i,
            n = i + 1,
            p = i.saturating_sub(1)
        ));
    }
    src.push_str("fn main():\n\tprint(\"bench\\n\")\n");
    src
}

/// Deeply nested control flow: a while loop wrapping `depth` levels of
/// if/else, with break/continue at the leaves. Stresses branch-heavy
/// TIR lowering and block management in codegen.
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
    src.push_str("fn main():\n\tprint(\"bench\\n\")\n");
    src
}

/// Run the frontend + ownership on a source string, returning everything
/// codegen needs: TIRs, the intern pool, and the ownership sidecar.
fn analyze(src: &str) -> (Vec<Tir>, InternPool, OwnershipSidecar) {
    let mut pool = InternPool::new();
    let mut sink = ryo_core::diag::DiagSink::new();
    let tokens = lexer::lex(src, &mut pool, &mut sink);
    assert!(!sink.has_errors(), "lex should succeed");
    let token_stream = tokens[..].split_token_span((0..src.len()).into());
    let mut ast = ryo_core::ast::Ast::new();
    program_parser()
        .parse_with_state(token_stream, &mut ast)
        .into_result()
        .expect("parse should succeed");
    let mut astgen_sink = ryo_core::diag::DiagSink::new();
    let uir = ryo_frontend::astgen::generate(&ast, &mut pool, &mut astgen_sink);
    let mut sema_sink = ryo_core::diag::DiagSink::new();
    let tirs = ryo_frontend::sema::analyze(
        &uir,
        &mut pool,
        &mut sema_sink,
        src,
        std::path::Path::new("bench.ryo"),
    );
    assert!(!sema_sink.has_errors(), "sema should succeed");
    let mut ownership_sink = ryo_core::diag::DiagSink::new();
    let sidecar = ryo_frontend::ownership::check(&tirs, &pool, &mut ownership_sink);
    assert!(!ownership_sink.has_errors(), "ownership should succeed");
    (tirs, pool, sidecar)
}

fn bench_codegen(bencher: divan::Bencher, src: &str) {
    bencher
        .with_inputs(|| analyze(src))
        .bench_values(|(tirs, pool, sidecar)| {
            let mut codegen = Codegen::new_jit().expect("JIT codegen should initialize");
            let main_id = codegen
                .compile(divan::black_box(&tirs), &pool, &sidecar)
                .expect("codegen should succeed");
            divan::black_box(main_id);
        });
}

#[divan::bench(args = [16, 256])]
fn codegen_arith(bencher: divan::Bencher, functions: usize) {
    bench_codegen(bencher, &arith_source(functions));
}

#[divan::bench(args = [(4, 4), (64, 8)])]
fn codegen_nested_control(bencher: divan::Bencher, case: (usize, usize)) {
    bench_codegen(bencher, &nested_control_source(case.0, case.1));
}
