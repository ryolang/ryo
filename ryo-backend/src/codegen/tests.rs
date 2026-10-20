use super::*;
use chumsky::Parser;
use chumsky::input::Input as _;
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
    let (tirs, flows, pool, sidecar) = analyze(src);
    let mut codegen = Codegen::new_jit().expect("JIT codegen should initialize");
    codegen
        .compile_and_dump_ir(&tirs, &flows, &pool, &sidecar)
        .expect("codegen should succeed")
}

/// AOT-compile `src` and return the object file bytes, for assertions
/// on the emitted symbol/data surface (runtime-callee names and .rodata
/// strings do not survive in CLIF text).
fn object_bytes(src: &str) -> Vec<u8> {
    let (tirs, flows, pool, sidecar) = analyze(src);
    let mut codegen = Codegen::new_aot(Triple::host()).expect("AOT codegen should initialize");
    codegen
        .compile(&tirs, &flows, &pool, &sidecar, false)
        .expect("compile should succeed");
    codegen.finish().expect("object emission should succeed")
}

fn analyze(
    src: &str,
) -> (
    Vec<Tir>,
    Vec<ryo_core::flow::Flow>,
    InternPool,
    ryo_core::ownership::OwnershipSidecar,
) {
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
    let flows: Vec<ryo_core::flow::Flow> = tirs
        .iter()
        .map(|t| ryo_core::flow::Flow::build(t, &pool))
        .collect();
    let mut ownership_sink = ryo_core::diag::DiagSink::new();
    let sidecar = ryo_frontend::ownership::check(&tirs, &flows, &pool, &mut ownership_sink);
    assert!(!ownership_sink.has_errors(), "ownership should succeed");
    (tirs, flows, pool, sidecar)
}

/// (a) `EnumLit` emits an i32 tag store at offset 0 followed by
/// field-wise payload stores at the pool-computed absolute offsets.
/// The 16-byte `Rectangle` payload sits at offset 8 (natural alignment)
/// — the stores must come from `EnumVariantView.offset`, never from
/// natural alignment assumptions.
#[test]
fn enum_lit_emits_tag_store_and_payload_stores_at_pool_offsets() {
    let src = "enum Shape:\n\tCircle(float)\n\tRectangle(width: float, height: float)\n\nfn main():\n\ts = Shape.Rectangle{width=1.0, height=2.0}\n";
    let (tirs, _mirs, pool, _sidecar) = analyze(src);
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
    let span = || ryo_core::span::Span::EMPTY;
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

#[test]
fn backend_rejects_same_shape_artifacts_from_another_body() {
    let source = "fn main():\n\ts = \"heap_012345678901234567890123456789\" + \"!\"\n\tprint(s)\n";
    let (tirs, flows, pool, cleanup) = analyze(source);
    let (other_tirs, other_flows, _, other_cleanup) = analyze(source);
    let mut codegen = Codegen::new_jit().expect("JIT");
    assert!(
        codegen
            .compile(&tirs, &other_flows, &pool, &cleanup, false)
            .is_err()
    );
    assert!(
        codegen
            .compile(&other_tirs, &flows, &pool, &cleanup, false)
            .is_err()
    );
    assert!(
        codegen
            .compile(&tirs, &flows, &pool, &other_cleanup, false)
            .is_err()
    );
}

#[test]
fn backend_rejects_unresolved_cleanup_before_emission() {
    let (tirs, flows, pool, mut cleanup) = analyze("fn main():\n\ts = \"x\" + \"y\"\n\tprint(s)\n");
    cleanup.functions[0].drops[0].position = ryo_core::flow::DropPosition::BlockStart {
        block: ryo_core::flow::BlockId(u32::MAX),
    };
    let mut codegen = Codegen::new_jit().expect("JIT");
    let error = codegen
        .compile(&tirs, &flows, &pool, &cleanup, false)
        .expect_err("bad position");
    assert!(
        error.contains("cleanup changed after publication"),
        "{error}"
    );
}

#[test]
fn flow_inspection_uses_stable_arenas_and_resolved_storage() {
    let src = include_str!("../../../ryo/tests/fixtures/shadowed_heap_reassignment.ryo");
    let (tirs, flows, pool, cleanup) = analyze(src);
    let first = flows[0]
        .dump(&tirs[0], &cleanup.functions[0], &pool)
        .expect("dump");
    let (tirs, flows, pool, cleanup) = analyze(src);
    let second = flows[0]
        .dump(&tirs[0], &cleanup.functions[0], &pool)
        .expect("dump");
    assert_eq!(first, second);
    assert!(first.contains("place") && first.contains("release") && first.contains("Resolved"));
    assert!(first.contains(if cfg!(debug_assertions) {
        "validation: complete"
    } else {
        "validation: not run"
    }));
}

#[test]
fn backend_rejects_incomplete_and_skipped_validation() {
    let (tirs, flows, pool, mut cleanup) = analyze("fn main():\n\tprint(1)\n");
    for summary in [
        ryo_core::ownership::ValidationSummary::default(),
        ryo_core::ownership::ValidationSummary {
            required_checks_ran: true,
            positions_not_proved: 1,
            ..Default::default()
        },
    ] {
        cleanup.functions[0].validation = Some(summary);
        let mut codegen = Codegen::new_jit().expect("JIT");
        assert!(
            codegen
                .compile(&tirs, &flows, &pool, &cleanup, false)
                .is_err()
        );
    }
}

#[test]
fn backend_rejects_validation_mutated_after_publication() {
    let (tirs, flows, pool, mut cleanup) = analyze("fn main():\n\tprint(1)\n");
    cleanup.functions[0].validation = Some(ryo_core::ownership::ValidationSummary {
        required_checks_ran: true,
        ..Default::default()
    });
    let mut codegen = Codegen::new_jit().expect("JIT");
    let error = codegen
        .compile(&tirs, &flows, &pool, &cleanup, false)
        .expect_err("forged validation must be rejected");
    assert!(
        error.contains("cleanup changed after publication"),
        "{error}"
    );
}

#[test]
fn cleanup_executes_without_optional_optimizations() {
    for source in [
        include_str!("../../../ryo/tests/fixtures/shadowed_heap_reassignment.ryo"),
        include_str!("../../../ryo/tests/fixtures/same_parameter_promotions.ryo"),
        include_str!("../../../ryo/tests/fixtures/nested_heap_shadows.ryo"),
        include_str!("../../../ryo/tests/fixtures/nested_heap_byte_shadows.ryo"),
        "fn main():\n\tmut s = \"heap_012345678901234567890123456789\" + \"!\"\n\ts = s + \"?\"\n\tassert(s == \"heap_012345678901234567890123456789!?\", \"concat without reuse\")\n",
    ] {
        let (tirs, flows, pool, _) = analyze(source);
        let cleanup = ryo_frontend::ownership::check_without_optimizations(
            &tirs,
            &flows,
            &pool,
            &mut ryo_core::diag::DiagSink::new(),
        )
        .expect("unoptimized cleanup");
        let mut jit = Codegen::new_jit_with_optimizations(false).expect("JIT");
        let (main, _) = jit
            .compile(&tirs, &flows, &pool, &cleanup, false)
            .expect("unoptimized emission");
        assert_eq!(jit.execute(main, &[]).expect("execution"), 0);
        let mut aot = Codegen::new_aot_with_optimizations(Triple::host(), false).expect("AOT");
        aot.compile(&tirs, &flows, &pool, &cleanup, false)
            .expect("unoptimized object");
        assert!(!aot.finish().expect("object bytes").is_empty());
    }
}

#[test]
fn cleanup_rejects_physical_storage_of_another_binding() {
    let expected = TirRef::from_raw(1);
    let wrong = TirRef::from_raw(2);
    let var = Variable::from_u32(0);
    let storage = FatLocals {
        binding: Some(wrong),
        ptr: var,
        len: var,
        cap: var,
        home: None,
        home_inline: false,
    };
    assert!(
        Codegen::<cranelift_jit::JITModule>::require_fat_storage_binding(storage, expected)
            .is_err()
    );
    assert!(
        Codegen::<cranelift_jit::JITModule>::require_fat_storage_binding(storage, wrong).is_ok()
    );
}

#[test]
fn mixed_abi_emits_objects_for_supported_target_layouts() {
    let (tirs, flows, pool, cleanup) = analyze(include_str!(
        "../../../ryo/tests/fixtures/mixed_call_abi.ryo"
    ));
    for target in [
        "aarch64-apple-darwin",
        "aarch64-unknown-linux-gnu",
        "x86_64-unknown-linux-gnu",
        "x86_64-pc-windows-gnu",
    ] {
        let mut codegen =
            Codegen::new_aot(target.parse().expect("target triple")).expect("target ISA");
        codegen
            .compile(&tirs, &flows, &pool, &cleanup, false)
            .expect("target ABI emission");
        assert!(!codegen.finish().expect("object").is_empty());
    }
}

#[test]
fn backend_rejects_edits_to_cloned_body_and_execution_arenas() {
    let (tirs, flows, pool, cleanup) = analyze("fn main():\n\tprint(1)\n");
    let mut changed_tirs = tirs.clone();
    let instruction = changed_tirs[0]
        .instructions
        .iter_mut()
        .find(|inst| inst.tag == TirTag::IntConst)
        .expect("integer");
    instruction.data = TirData::Int(2);
    let mut codegen = Codegen::new_jit().expect("JIT");
    assert!(
        codegen
            .compile(&changed_tirs, &flows, &pool, &cleanup, false)
            .is_err()
    );
    let mut changed_flows = flows.clone();
    changed_flows[0].blocks[0].unreachable = true;
    assert!(
        codegen
            .compile(&tirs, &changed_flows, &pool, &cleanup, false)
            .is_err()
    );
    assert!(
        codegen
            .compile(&tirs, &flows, &InternPool::new(), &cleanup, false)
            .is_err()
    );
}
