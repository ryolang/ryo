use super::*;
use chumsky::span::{SimpleSpan, Span as _};
use cranelift::codegen::ir::Value as ClifValue;
use ryo_core::tir::{ParamMode, TirBuilder};
use ryo_core::types::InternPool;

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
            !writes_out_slot(&tir, &pool, call),
            "inlined builtin {inlined} must not be treated as a slot-out producer"
        );
    }
    assert!(writes_out_slot(&tir, &pool, producer_call));
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
