//! CLIF-level pinning tests for M9 struct codegen: struct values are
//! stack-resident (`explicit_slot`), struct returns use an out-pointer
//! (`sret` special parameter), and struct calls pass a single pointer
//! per struct argument. Assertions pin only the stable CLIF surface —
//! no offset or register pins. Runtime-callee names do not survive in
//! CLIF text (`fn0 = u0:1 sig0`), so the drop tests pin `ryo_str_free`
//! through the AOT object's symbol table instead (same rationale as
//! `obj_bytes_dead_drop.rs`).

use chumsky::Parser;
use chumsky::input::Input;
use ryo_backend::codegen::Codegen;
use ryo_core::ownership::OwnershipSidecar;
use ryo_core::tir::Tir;
use ryo_core::types::InternPool;
use ryo_frontend::lexer;
use ryo_frontend::parser::program_parser;
use target_lexicon::Triple;

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

fn object_bytes(src: &str) -> Vec<u8> {
    let (tirs, pool, sidecar) = analyze(src);
    let mut codegen = Codegen::new_aot(Triple::host()).expect("AOT codegen should initialize");
    codegen
        .compile(&tirs, &pool, &sidecar)
        .expect("compile should succeed");
    codegen.finish().expect("object emission should succeed")
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn struct_literal_uses_stack_slot() {
    let clif = clif_of(
        "struct Point:\n\tx: float\n\ty: float\n\nfn main():\n\tp = Point{x=1.0, y=2.0}\n\tprint(float_to_str(p.x))\n",
    );
    assert!(
        clif.contains("explicit_slot"),
        "struct value must be stack-resident:\n{clif}"
    );
}

#[test]
fn struct_return_uses_sret() {
    let clif = clif_of(
        "struct Point:\n\tx: float\n\nfn make() -> Point:\n\treturn Point{x=1.0}\n\nfn main():\n\tp = make()\n\tprint(float_to_str(p.x))\n",
    );
    assert!(
        clif.contains("sret"),
        "struct return must use an out-pointer:\n{clif}"
    );
}

#[test]
fn struct_call_passes_pointer() {
    // A borrow-mode struct param is a single pointer argument; the
    // caller's value must be stack-resident at the call site.
    let clif = clif_of(
        "struct Point:\n\tx: float\n\ty: float\n\nfn area(p: Point) -> float:\n\treturn p.x * p.y\n\nfn main():\n\tq = Point{x=3.0, y=4.0}\n\tprint(float_to_str(area(q)))\n",
    );
    assert!(
        clif.contains("explicit_slot"),
        "struct arg must be stack-resident at the call site:\n{clif}"
    );
    // The callee's own signature is one pointer in, one float out —
    // not a per-field expansion.
    assert!(
        clif.contains("function u0:0(i64) -> f64"),
        "area must take a single pointer argument:\n{clif}"
    );
    // The colocated callee ref is invoked with exactly one argument.
    let callee = clif
        .lines()
        .find(|l| l.contains("= colocated u0:0 "))
        .and_then(|l| l.split_whitespace().next())
        .expect("colocated callee ref should exist");
    let call_line = clif
        .lines()
        .find(|l| l.contains(&format!("call {callee}(")))
        .expect("callee should be called");
    let args = call_line
        .split('(')
        .nth(1)
        .and_then(|rest| rest.split(')').next())
        .expect("call should have an argument list");
    assert!(
        !args.contains(','),
        "struct call must pass exactly one pointer argument, got ({args}):\n{clif}"
    );
}

#[test]
fn whole_struct_drop_frees_str_fields() {
    // Person owns a str field; the last-use whole-struct drop must
    // reference `ryo_str_free` (recursive field destruction).
    let obj = object_bytes(
        "struct Person:\n\tname: str\n\nfn main():\n\tp = Person{name=\"alice\"}\n\tprint(p.name)\n",
    );
    assert!(
        contains(&obj, b"ryo_str_free"),
        "whole-struct drop must reference ryo_str_free"
    );
}

#[test]
fn field_reassign_drops_old_str_value() {
    // `p.name = "bob"` over a live str field must free the old field
    // value before the store (field_free_on_reassign).
    let src = "struct Person:\n\tname: str\n\nfn main():\n\tmut p = Person{name=\"alice\"}\n\tp.name = \"bob\"\n\tprint(p.name)\n";
    let obj = object_bytes(src);
    assert!(
        contains(&obj, b"ryo_str_free"),
        "field reassign must reference ryo_str_free"
    );
    // The symbol reference alone is ambiguous — the end-of-scope drop
    // also references ryo_str_free. Pin the ordering instead: the
    // reassign free is the one `call` immediately followed by the
    // overwrite `store` into the struct slot.
    let clif = clif_of(src);
    let free_before_overwrite = clif
        .lines()
        .map(str::trim_start)
        .collect::<Vec<_>>()
        .windows(2)
        .any(|w| w[0].starts_with("call fn") && w[1].starts_with("store "));
    assert!(
        free_before_overwrite,
        "the old field value must be freed immediately before the overwrite store:\n{clif}"
    );
}
