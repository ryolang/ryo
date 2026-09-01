//! Object-symbol pin for the M8.4.2 dead-drop free-family selection.
//! The CLIF text dump renders runtime callees opaquely
//! (`fn0 = u0:1 sig0`), so the earliest pipeline stage where the callee
//! name survives is the AOT object's symbol table: a conditional
//! dead-drop on a `bytes` owner must reference `ryo_bytes_free` and
//! never `ryo_str_free`.

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
fn bytes_conditional_dead_drop_references_bytes_free() {
    // A bytes owner conditionally reassigned and never read afterwards
    // yields a ConditionalDeadDrop (the ownership-side half is pinned
    // by `bytes_conditional_reassign_emits_dead_drop`), for which
    // codegen must select `ryo_bytes_free` (not `ryo_str_free`).
    //
    // The fixture is literal-backed on purpose: with heap-backed
    // `.to_bytes()` sources, the str literal temporaries get scheduled
    // Frees whose cap=0 *calls* are elided but whose `ryo_str_free`
    // *declaration* still lands in the symbol table — declaration and
    // call are not separable at symbol granularity, and what this pin
    // cares about is the dead-drop's family selection. (The dead
    // reassign warns W0001; warnings do not fail analysis.)
    let obj = object_bytes(
        "fn main():\n\tmut b = b\"AB\"\n\tflag = false\n\tif flag:\n\t\tb = b\"CD\"\n",
    );
    assert!(
        contains(&obj, b"ryo_bytes_free"),
        "dead-dropped bytes owner must reference ryo_bytes_free"
    );
    assert!(
        !contains(&obj, b"ryo_str_free"),
        "no str-family free may appear in an all-bytes dead-drop program"
    );
}
