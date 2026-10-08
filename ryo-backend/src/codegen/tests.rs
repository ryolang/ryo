use super::*;
use chumsky::Parser;
use chumsky::input::Input as _;
use chumsky::span::{SimpleSpan, Span as _};
use cranelift::codegen::ir::Value as ClifValue;
use ryo_core::tir::{ParamMode, Tir, TirBuilder};
use ryo_core::types::InternPool;
use ryo_frontend::parser::ParseState;
use target_lexicon::Triple;

/// M11 enum codegen harness: run the full frontend pipeline over `src`
/// and lower with JIT codegen, returning the rendered CLIF. Mirrors the
/// `clif_of` helpers in the `tests/clif_*.rs` integration suites (kept
/// inline here because the enum codegen tests are unit tests in this
/// module, per the M11 plan).
fn clif_of(src: &str) -> String {
    let (tirs, pool, sidecar) = analyze(src);
    let mut codegen = Codegen::new_jit().expect("JIT codegen should initialize");
    codegen
        .compile_and_dump_ir(&tirs, &pool, &sidecar)
        .expect("codegen should succeed")
}

/// AOT-compile `src` and return the object file bytes, for assertions
/// on the emitted symbol/data surface (runtime-callee names and .rodata
/// strings do not survive in CLIF text).
fn object_bytes(src: &str) -> Vec<u8> {
    let (tirs, pool, sidecar) = analyze(src);
    let mut codegen = Codegen::new_aot(Triple::host()).expect("AOT codegen should initialize");
    codegen
        .compile(&tirs, &pool, &sidecar, false)
        .expect("compile should succeed");
    codegen.finish().expect("object emission should succeed")
}

fn analyze(src: &str) -> (Vec<Tir>, InternPool, ryo_core::ownership::OwnershipSidecar) {
    let mut pool = InternPool::new();
    let mut sink = ryo_core::diag::DiagSink::new();
    let tokens = ryo_frontend::lexer::lex(src, &mut pool, &mut sink);
    assert!(!sink.has_errors(), "lex should succeed");
    let token_stream = tokens[..].split_token_span((0..src.len()).into());
    let mut state = ParseState::new(pool);
    ryo_frontend::parser::program_parser()
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

/// (a) `EnumLit` emits an i32 tag store at offset 0 followed by
/// field-wise payload stores at the pool-computed absolute offsets.
/// The 16-byte `Rectangle` payload sits at offset 8 (natural alignment);
/// the 8-byte `Circle` payload shares the tag's slot at offset 4 — the
/// stores must come from `EnumVariantView.offset`, never from natural
/// alignment assumptions.
#[test]
fn enum_lit_emits_tag_store_and_payload_stores_at_pool_offsets() {
    let src = "enum Shape:\n\tCircle(float)\n\tRectangle(width: float, height: float)\n\nfn main():\n\ts = Shape.Rectangle{width=1.0, height=2.0}\n";
    let (tirs, pool, _sidecar) = analyze(src);
    // Expected offsets straight from the pool layout.
    let main = tirs
        .iter()
        .find(|t| pool.str(t.name) == "main")
        .expect("main should exist");
    let mut enum_ty = None;
    for inst in &main.instructions {
        if matches!(inst.tag, ryo_core::tir::TirTag::EnumLit) {
            enum_ty = Some(inst.ty);
        }
    }
    let enum_ty = enum_ty.expect("main should contain an EnumLit");
    let rect = pool
        .enum_view(enum_ty)
        .variants()
        .nth(1)
        .expect("Rectangle is the second variant");
    let expected: Vec<i64> = std::iter::once(0) // tag
        .chain(rect.fields.iter().map(|f| i64::from(f.offset)))
        .collect();

    let clif = clif_of(src);
    // CLIF prints `store v, base+off` and omits the `+off` when the
    // offset is 0 — the tag store at offset 0 parses as bare `base`.
    let store_offsets: Vec<i64> = clif
        .lines()
        .filter(|l| l.trim_start().starts_with("store "))
        .map(|l| l.trim_start())
        .map(|l| match l.split('+').nth(1) {
            Some(raw) => raw
                .split_whitespace()
                .next()
                .expect("offset immediate should follow the '+'")
                .parse()
                .unwrap_or_else(|e| panic!("store offset should be decimal: {e:?} in {l:?}")),
            None => 0,
        })
        .collect();
    assert_eq!(
        store_offsets, expected,
        "tag store + payload stores must land at the pool offsets:\n{clif}"
    );
    assert!(
        clif.contains("iconst.i32 1"),
        "tag store must write the Rectangle discriminant:\n{clif}"
    );
}

/// (b) `EnumEq` lowers to an i32 tag compare (`icmp eq` on the loaded
/// tags) plus a branch — the payload compares live behind the
/// same-tag arm, never reading an inactive variant's padding.
#[test]
fn enum_eq_lowers_to_tag_compare_and_branch() {
    let clif = clif_of(
        "#[derive(Eq)] enum Color:\n\tRed\n\tGreen\n\nfn main():\n\tc = Color.Red\n\tprint(c == Color.Green)\n",
    );
    assert!(
        clif.contains("load.i32"),
        "enum equality must load the i32 tags:\n{clif}"
    );
    assert!(
        clif.contains("icmp eq"),
        "enum equality must compare tags with icmp eq:\n{clif}"
    );
    assert!(
        clif.contains("brif"),
        "enum equality must branch on the tag comparison:\n{clif}"
    );
}

/// (c) Debug repr of a unit value: `print(Color.Red)` renders
/// `Color.Red` — the enum name, a `.` separator, and the active
/// variant's name are pushed as three separate .rodata pieces (the
/// exact assembled output is pinned by the end-to-end run; here the
/// object must carry all three blobs).
#[test]
fn debug_repr_of_unit_enum_value_pushes_name_dot_variant() {
    let obj =
        object_bytes("enum Color:\n\tRed\n\tGreen\n\tBlue\n\nfn main():\n\tprint(Color.Red)\n");
    for needle in [b"Color".as_slice(), b".", b"Red"] {
        assert!(
            obj.windows(needle.len()).any(|w| w == needle),
            "object must contain {needle:?} for the unit Debug repr"
        );
    }
    let clif = clif_of("enum Color:\n\tRed\n\tGreen\n\tBlue\n\nfn main():\n\tprint(Color.Red)\n");
    assert!(
        clif.contains("load.i32"),
        "enum Debug repr must dispatch on the loaded i32 tag:\n{clif}"
    );
}

/// `writes_out_slot` must reject every codegen-inlined builtin (their
/// `eval_inst_fat_slot` arm ignores `out_slot`) and accept real
/// slot-out producers. A regression here either fails loudly (inlined
/// builtin handed a home slot → compile error) or silently forgoes the
/// home (non-inlined builtin excluded).
#[test]
fn writes_out_slot_matches_codegen_inlined_builtins() {
    let span = || SimpleSpan::new((), 0..0);
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let bool_ty = pool.bool_();
    let name = pool.intern_str("f");
    let mut builder = TirBuilder::new(name, Vec::new(), str_ty, span());

    let mut inlined_calls = Vec::new();
    for &inlined in CODEGEN_INLINED_BUILTINS {
        let builtin = pool.intern_str(inlined);
        let arg = builder.bool_const(true, bool_ty, span());
        inlined_calls.push((
            inlined,
            builder.call(builtin, &[arg], &[ParamMode::Borrow], str_ty, span()),
        ));
    }
    let int_to_str = pool.intern_str("int_to_str");
    let arg = builder.int_const(1, pool.int(), span());
    let producer_call = builder.call(int_to_str, &[arg], &[ParamMode::Borrow], str_ty, span());
    let tir = builder.finish(&[]);

    for (inlined, call) in inlined_calls {
        assert!(
            !structs::writes_out_slot(&tir, &pool, call),
            "inlined builtin {inlined} must not be treated as a slot-out producer"
        );
    }
    assert!(structs::writes_out_slot(&tir, &pool, producer_call));
}

#[test]
fn value_repr_scalar_roundtrip() {
    let v = ClifValue::from_u32(1);
    let repr = ValueRepr::Scalar(v);
    assert_eq!(repr.expect_scalar(), v);
}

#[test]
fn value_repr_str_fields() {
    let repr = ValueRepr::Str {
        ptr: ClifValue::from_u32(1),
        len: ClifValue::from_u32(2),
        cap: ClifValue::from_u32(3),
    };
    match repr {
        ValueRepr::Str { ptr, len, cap } => {
            assert_ne!(ptr, len);
            assert_ne!(len, cap);
        }
        _ => panic!("expected Str"),
    }
}

#[test]
#[should_panic(expected = "expected Scalar, got Str")]
fn value_repr_expect_scalar_panics_on_str() {
    let repr = ValueRepr::Str {
        ptr: ClifValue::from_u32(1),
        len: ClifValue::from_u32(2),
        cap: ClifValue::from_u32(3),
    };
    repr.expect_scalar();
}
