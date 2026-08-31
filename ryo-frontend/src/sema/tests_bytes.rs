//! M8.4.2 `bytes`/`bytesview` sema tests. Harness shared with
//! `tests.rs` via its `pub(super)` helpers (the 2000-line file cap
//! keeps these out of `tests.rs`).

use super::tests::*;
use super::*;
use ryo_core::tir::{Tir, TirRef, TirTag};

// (`tests.rs`'s own `use ryo_core::tir::...` is private to that module,
// so this file needs its own import; `DiagCode`, `TypeId`, `InternPool`
// etc. come through `use super::*;` from the `sema` module scope, same
// as in `tests.rs`.)

fn any_inst(tir: &Tir, pred: impl Fn(TirTag, TypeId) -> bool) -> bool {
    tir.instructions.iter().any(|i| pred(i.tag, i.ty))
}

#[test]
fn bytes_literal_types_as_bytes() {
    let (tirs, pool) = run("b = b\"\\x01\\x02\"\n").expect("sema ok");
    let main = tir_named(&tirs, &pool, "main");
    assert!(
        any_inst(main, |tag, ty| tag == TirTag::BytesConst
            && ty == pool.bytes()),
        "no bytes-typed BytesConst in TIR"
    );
}

#[test]
fn bytes_and_bytesview_annotations_resolve() {
    run("fn f(b: bytes, v: bytesview):\n\treturn\n").expect("sema ok");
}

#[test]
fn len_and_is_empty_work_on_bytes_and_bytesview() {
    // The `bytesview` receiver is a parameter here; the sliced-projection
    // case (`v = b[0:1]`) is covered by
    // `len_and_is_empty_work_on_sliced_bytesview` below.
    run("fn f(b: bytes, v: bytesview):\n\tn = b.len()\n\tm = v.len()\n\te = b.is_empty()\n\tp = v.is_empty()\n")
        .expect("sema ok");
}

#[test]
fn len_and_is_empty_work_on_sliced_bytesview() {
    // The slice gate now accepts `bytes`, so a `b[0:1]` projection is a
    // real `bytesview` receiver for `len()`/`is_empty()`.
    run("fn main():\n\tb = b\"\\x01\\x02\"\n\tv = b[0:1]\n\tn = v.len()\n\te = v.is_empty()\n")
        .expect("sema ok");
}

#[test]
fn print_rewrites_bytes_through_repr() {
    let (tirs, pool) = run("print(b\"\\x01\")\n").expect("sema ok");
    let main = tir_named(&tirs, &pool, "main");
    let repr = pool.find_str("__ryo_bytes_repr").expect("repr interned");
    // `Tir::call_view` takes a `TirRef`, so iterate by index and
    // reconstruct refs (same pattern as codegen's literal hoist).
    let has_repr_call = (1..main.instructions.len()).any(|idx| {
        let r = TirRef::from_raw(u32::try_from(idx).expect("idx fits u32"));
        main.inst(r).tag == TirTag::Call && main.call_view(r).name == repr
    });
    assert!(
        has_repr_call,
        "print(bytes) must lower via __ryo_bytes_repr"
    );
}

#[test]
fn unknown_method_on_bytes_reports_type_name() {
    let (_, diags, _) = run_with_errors("b = b\"\\x01\"\nb.frobnicate()\n");
    assert!(any_code(&diags, DiagCode::UndefinedFunction));
    assert!(
        first_msg(&diags).contains("bytes has no method 'frobnicate'"),
        "got: {}",
        first_msg(&diags)
    );
}

#[test]
fn int_still_has_no_methods() {
    let (_, diags, _) = run_with_errors("x = 1\nx.len()\n");
    assert!(
        diags
            .iter()
            .any(|d| d.message.contains("type 'int' has no methods")),
        "got: {:?}",
        diags.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
}

#[test]
fn bytes_concat_types_as_bytes() {
    let (tirs, pool) = run("c = b\"\\x01\" + b\"\\x02\"\n").expect("sema ok");
    let main = tir_named(&tirs, &pool, "main");
    assert!(
        any_inst(main, |tag, ty| tag == TirTag::BytesConcat
            && ty == pool.bytes()),
        "no bytes-typed BytesConcat in TIR"
    );
}

#[test]
fn bytes_concat_rejects_str_and_views() {
    // Mixed bytes + str is a type mismatch.
    let (_, diags, _) = run_with_errors("c = b\"\\x01\" + \"s\"\n");
    assert!(any_code(&diags, DiagCode::TypeMismatch));
    // View operands are rejected exactly like strview in str `+`:
    // owner + view fails the generic compatibility check (E0012)…
    let (_, diags, _) = run_with_errors("fn main():\n\tb = b\"\\x01\"\n\tc = b + b[0:1]\n");
    assert!(any_code(&diags, DiagCode::TypeMismatch));
    // …and view + view falls through to E0015 UnsupportedOperator.
    let (_, diags, _) = run_with_errors("fn main():\n\tb = b\"\\x01\"\n\tc = b[0:1] + b[0:1]\n");
    assert!(
        diags.iter().any(|d| d.code == DiagCode::UnsupportedOperator
            && d.message.contains("'+' not supported for type 'bytesview'")),
        "got: {:?}",
        diags.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
}

#[test]
fn bytes_equality_all_pairs() {
    run("fn main():\n\tb = b\"\\x01\"\n\tv = b[0:1]\n\tx = b == b\"\\x02\"\n\ty = v == b[0:1]\n\tz = b == v\n\tw = v == b\n")
        .expect("sema ok");
}

#[test]
fn bytes_equality_vs_str_is_mismatch() {
    let (_, diags, _) = run_with_errors("fn main():\n\tb = b\"\\x01\"\n\tx = b == \"s\"\n");
    assert!(any_code(&diags, DiagCode::TypeMismatch));
}

#[test]
fn bytes_slice_yields_bytesview_and_reslices() {
    let (tirs, pool) = run("fn main():\n\tb = b\"\\x01\\x02\\x03\"\n\tv = b[0:2]\n\tw = v[1:]\n")
        .expect("sema ok");
    let main = tir_named(&tirs, &pool, "main");
    let slices = main
        .instructions
        .iter()
        .filter(|i| i.tag == TirTag::Slice && i.ty == pool.bytes_view())
        .count();
    assert_eq!(slices, 2, "both slices must type as bytesview");
}

#[test]
fn non_sliceable_types_still_rejected() {
    let (_, diags, _) = run_with_errors("fn main():\n\tx = 1\n\ty = x[0:1]\n");
    assert!(
        diags
            .iter()
            .any(|d| d.code == DiagCode::TypeMismatch
                && d.message.contains("cannot slice type 'int'")),
        "got: {:?}",
        diags.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
}

#[test]
fn bytes_push_validates_like_str_push() {
    // Happy path.
    run("fn main():\n\tmut b = b\"\\x00\"\n\tbytes_push(&b, 255)\n").expect("sema ok");
    // Missing `&`.
    let (_, diags, _) = run_with_errors("fn main():\n\tmut b = b\"\\x00\"\n\tbytes_push(b, 1)\n");
    assert!(any_code(&diags, DiagCode::BorrowMismatch));
    // Non-mut target.
    let (_, diags, _) = run_with_errors("fn main():\n\tb = b\"\\x00\"\n\tbytes_push(&b, 1)\n");
    assert!(any_code(&diags, DiagCode::BorrowMismatch));
    // Wrong arg types.
    let (_, diags, _) = run_with_errors("fn main():\n\tmut s = \"x\"\n\tbytes_push(&s, 1)\n");
    assert!(any_code(&diags, DiagCode::TypeMismatch));
    let (_, diags, _) =
        run_with_errors("fn main():\n\tmut b = b\"\\x00\"\n\tbytes_push(&b, \"x\")\n");
    assert!(any_code(&diags, DiagCode::TypeMismatch));
}

#[test]
fn bytesview_signature_rejections() {
    // E1: view return type rejected (same machinery as strview).
    let (_, diags, _) = run_with_errors("fn f() -> bytesview:\n\tpanic(\"x\")\n");
    assert!(any_code(&diags, DiagCode::ReturnBorrowedValue));
    // E2: `move bytesview` parameter rejected.
    let (_, diags, _) = run_with_errors("fn f(move v: bytesview):\n\treturn\n");
    assert!(!diags.is_empty(), "move bytesview param must be rejected");
}
