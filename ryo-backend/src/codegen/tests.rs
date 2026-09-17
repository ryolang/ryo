use super::*;
use cranelift::codegen::ir::Value as ClifValue;

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
