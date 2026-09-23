//! Object/CLIF pins for the slot-home binding work and the
//! provably-inline producer free elision:
//!
//! * A fat binding initialized by a slot-out producer gets a canonical
//!   24-byte home slot the producer writes directly — no temp slot,
//!   no extraction scratch for `print`/`len` uses of the binding.
//! * A producer whose output is provably SSO-inline (`int_to_str`,
//!   `bool_to_str` — see `builtins.rs::max_output_len`) pays no
//!   `ryo_str_free`: the inline tag makes it a guaranteed no-op.
//!   Reassigned, pushed, inout-passed, or view-promoted bindings keep
//!   their frees, as does `float_to_str` (ryu's f64 worst case is 24
//!   bytes, one over the inline capacity).
//! * `bool_to_str` is fully inlined as a select between two static
//!   literals — no runtime call at all.
//!
//! Runtime callee names do not survive in CLIF text, so frees are
//! pinned through the AOT object's symbol table (same rationale as
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

fn object_bytes(src: &str) -> Vec<u8> {
    let (tirs, pool, sidecar) = analyze(src);
    let mut codegen = Codegen::new_aot(Triple::host()).expect("AOT codegen should initialize");
    codegen
        .compile(&tirs, &pool, &sidecar, false)
        .expect("compile should succeed");
    codegen.finish().expect("object emission should succeed")
}

fn clif_of(src: &str) -> String {
    let (tirs, pool, sidecar) = analyze(src);
    let mut codegen = Codegen::new_jit().expect("JIT codegen should initialize");
    codegen
        .compile_and_dump_ir(&tirs, &pool, &sidecar)
        .expect("codegen should succeed")
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn int_to_str_binding_elides_free() {
    // `s` is provably inline forever (never reassigned, pushed,
    // inout-passed, or sliced): its scheduled free is a guaranteed
    // no-op on the inline tag and must not be emitted.
    let obj = object_bytes("fn main():\n\ts: str = int_to_str(42)\n\tprint(s)\n");
    assert!(
        !contains(&obj, b"ryo_str_free"),
        "provably-inline binding must not reference ryo_str_free"
    );
    assert!(
        contains(&obj, b"ryo_int_to_str"),
        "the producer call itself stays"
    );
}

#[test]
fn int_to_str_binding_writes_home_directly() {
    // Slot discipline: exactly ONE 24-byte slot — the binding's home.
    // The producer writes it directly and the `print` extraction reads
    // the inline bytes through the home address, so neither a temp
    // slot-out slot nor an extraction scratch slot remains.
    let clif = clif_of("fn main():\n\ts: str = int_to_str(42)\n\tprint(s)\n");
    let slots = clif.matches("explicit_slot").count();
    assert_eq!(slots, 1, "only the binding's home slot:\n{clif}");
}

#[test]
fn reassigned_binding_keeps_free() {
    // After `s = "a" + "b"` the binding can hold a heap buffer, so the
    // provably-inline fact about its initializer no longer applies.
    let obj =
        object_bytes("fn main():\n\tmut s = int_to_str(1)\n\ts = \"a\" + \"b\"\n\tprint(s)\n");
    assert!(
        contains(&obj, b"ryo_str_free"),
        "reassigned binding must keep its free"
    );
}

#[test]
fn slice_base_binding_keeps_free() {
    // Slicing promotes the binding to a heap buffer in place
    // (promote-on-view), so the free is real.
    let obj = object_bytes("fn main():\n\ts: str = int_to_str(7)\n\tprint(s[0:1])\n");
    assert!(
        contains(&obj, b"ryo_str_free"),
        "view-promoted binding must keep its free"
    );
}

#[test]
fn float_to_str_keeps_free() {
    // ryu's f64 worst case is 24 bytes > 23 inline capacity: not
    // provably inline, free stays.
    let obj = object_bytes("fn main():\n\ts: str = float_to_str(1.5)\n\tprint(s)\n");
    assert!(
        contains(&obj, b"ryo_str_free"),
        "float_to_str result can be heap; free must stay"
    );
}

#[test]
fn bool_to_str_is_fully_inlined() {
    // No runtime call, no slot-out, no free: the result is a select
    // between the static "true"/"false" literals (cap=0 sentinel).
    let obj = object_bytes("fn main():\n\ts: str = bool_to_str(true)\n\tprint(s)\n");
    assert!(
        !contains(&obj, b"ryo_bool_to_str"),
        "bool_to_str must not call into the runtime"
    );
    assert!(
        !contains(&obj, b"ryo_str_free"),
        "static-literal result needs no free"
    );
    let clif = clif_of("fn main():\n\ts: str = bool_to_str(true)\n\tprint(s)\n");
    assert!(
        !clif.contains("explicit_slot"),
        "no slot at all for the inlined select:\n{clif}"
    );
}

#[test]
fn reassign_to_inline_elides_frees_via_provenance() {
    // The home-provenance flag generalizes the elision to reassigned
    // bindings: both the old-value free at the reassign (previous home
    // contents provably inline) and the end-of-scope free (current
    // contents provably inline) are guaranteed no-ops.
    let obj =
        object_bytes("fn main():\n\tmut s = int_to_str(1)\n\ts = int_to_str(2)\n\tprint(s)\n");
    assert!(
        !contains(&obj, b"ryo_str_free"),
        "reassign between provably-inline values needs no frees"
    );
}

#[test]
fn if_merge_invalidates_provenance() {
    // The then-arm stores a concat (heap-capable) into the home; at
    // the merge the flag must be gone, so the frees after the if stay.
    let obj = object_bytes(
        "fn f(x: int):\n\tmut s = int_to_str(1)\n\tif x > 0:\n\t\ts = \"a\" + \"b\"\n\tprint(s)\n",
    );
    assert!(
        contains(&obj, b"ryo_str_free"),
        "post-merge frees must stay"
    );
}

#[test]
fn loop_back_edge_invalidates_provenance() {
    // Iteration 2+ frees the heap buffer stored by the previous
    // iteration — the frees inside and after the loop must stay.
    let obj = object_bytes(
        "fn f(n: int):\n\tmut s = int_to_str(1)\n\tfor i in range(0, n):\n\t\ts = \"a\" + \"b\"\n\tprint(s)\n",
    );
    assert!(
        contains(&obj, b"ryo_str_free"),
        "loop-carried frees must stay"
    );
}

#[test]
fn push_invalidates_provenance() {
    // str_push may heap-promote the home buffer in place — the free
    // after the push is real.
    let obj =
        object_bytes("fn main():\n\tmut s = int_to_str(1)\n\tstr_push(&s, \"x\")\n\tprint(s)\n");
    assert!(
        contains(&obj, b"ryo_str_free"),
        "pushed binding may hold a heap buffer; free must stay"
    );
}

#[test]
fn mutable_literal_initialized_binding_gets_home() {
    // `mut` bindings get a home even when the initializer is not a
    // slot-out producer: the reassign writes the home directly (no
    // temp slot) and both frees elide — the old value is a static
    // literal, the new one a provably-inline producer result.
    let src = "fn main():\n\tmut s = \"\"\n\ts = int_to_str(1)\n\tprint(s)\n";
    let obj = object_bytes(src);
    assert!(
        !contains(&obj, b"ryo_str_free"),
        "static old value + provably-inline new value need no frees"
    );
    let clif = clif_of(src);
    let slots = clif.matches("explicit_slot").count();
    assert_eq!(slots, 1, "exactly the binding's home slot:\n{clif}");
}

#[test]
fn mutable_bool_to_str_initialized_binding_gets_home() {
    // Same coverage for a codegen-inlined initializer: the home is
    // created (mut), the inlined select's triple is stored into it by
    // hand, and the reassign + frees take the home paths.
    let src = "fn main():\n\tmut s = bool_to_str(true)\n\ts = int_to_str(1)\n\tprint(s)\n";
    let obj = object_bytes(src);
    assert!(
        !contains(&obj, b"ryo_str_free"),
        "provably-inline old and new values need no frees"
    );
    let clif = clif_of(src);
    let slots = clif.matches("explicit_slot").count();
    assert_eq!(slots, 1, "exactly the binding's home slot:\n{clif}");
}

#[test]
fn registry_bounds_match_codegen_elision() {
    // Coherence pin between the frontend registry annotation
    // (`builtins.rs::max_output_len`) and the backend's elision
    // behavior: a builtin annotated at-or-under the SSO inline
    // capacity (23 bytes) must get its free elided; anything else
    // must keep it. If one table changes without the other, this
    // test fails.
    let mut pool = InternPool::new();
    for (name, arg) in [
        ("int_to_str", "42"),
        ("bool_to_str", "true"),
        ("float_to_str", "1.5"),
    ] {
        let id = pool.intern_str(name);
        let bound = ryo_frontend::builtins::max_output_len(id, &pool);
        let provably_inline = bound.is_some_and(|max| max as usize <= 23);
        let src = format!("fn main():\n\ts: str = {name}({arg})\n\tprint(s)\n");
        let obj = object_bytes(&src);
        let has_free = contains(&obj, b"ryo_str_free");
        assert_eq!(
            has_free, !provably_inline,
            "{name}: registry bound {bound:?} disagrees with codegen elision"
        );
    }
}
