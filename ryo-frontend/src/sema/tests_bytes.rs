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
    // The `bytesview` receiver is a parameter: slicing `bytes` is a
    // later M8.4.2 task, so a `b[0:1]` projection does not exist yet.
    run("fn f(b: bytes, v: bytesview):\n\tn = b.len()\n\tm = v.len()\n\te = b.is_empty()\n\tp = v.is_empty()\n")
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
