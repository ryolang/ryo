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

#[test]
fn bytes_materialize_types_and_lowers() {
    let (tirs, pool) =
        run("fn main():\n\tb = b\"\\x01\\x02\"\n\tv = b[0:1]\n\tc = bytes(v)\n").expect("sema ok");
    let main = tir_named(&tirs, &pool, "main");
    let callee = pool
        .find_str("__ryo_bytes_from_view")
        .expect("callee interned");
    let found = (1..main.instructions.len()).any(|idx| {
        let r = TirRef::from_raw(u32::try_from(idx).expect("idx fits u32"));
        main.inst(r).tag == TirTag::Call
            && main.inst(r).ty == pool.bytes()
            && main.call_view(r).name == callee
    });
    assert!(found, "bytes(v) must lower to __ryo_bytes_from_view");
}

#[test]
fn bytes_materialize_rejects_non_bytesview() {
    // strview argument.
    let (_, diags, _) = run_with_errors("fn main():\n\ts = \"ab\"\n\tc = bytes(s[0:1])\n");
    assert!(
        diags.iter().any(|d| d.code == DiagCode::TypeMismatch
            && d.message.contains("bytes() argument must be bytesview")),
        "got: {:?}",
        diags.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
    // Owned bytes argument (already an owner).
    let (_, diags, _) = run_with_errors("fn main():\n\tb = b\"\\x01\"\n\tc = bytes(b)\n");
    assert!(any_code(&diags, DiagCode::TypeMismatch));
    // Arity.
    let (_, diags, _) = run_with_errors("fn main():\n\tc = bytes()\n");
    assert!(any_code(&diags, DiagCode::ArityMismatch));
}

#[test]
fn bytesview_reborrows_into_bytes_param() {
    // P6': a bytesview passed to a `bytes` borrow parameter is a
    // call-scoped cap=0 re-borrow — no materialization, no error.
    let (tirs, pool) = run(
        "fn takes(b: bytes):\n\tprint(int_to_str(b.len()))\n\nfn main():\n\traw = b\"\\x01\\x02\"\n\ttakes(raw[0:1])\n",
    )
    .expect("sema ok");
    let main = tir_named(&tirs, &pool, "main");
    assert!(
        any_inst(main, |tag, ty| tag == TirTag::ViewAsOwner
            && ty == pool.bytes()),
        "expected a ViewAsOwner re-borrow to bytes"
    );
}

#[test]
fn binding_bytesview_to_bytes_is_type_error() {
    // Only call parameters get the re-borrow; binding stays E0012.
    let (_, diags, _) = run_with_errors("fn main():\n\traw = b\"\\x01\"\n\tv: bytes = raw[0:1]\n");
    assert!(any_code(&diags, DiagCode::TypeMismatch));
}

#[test]
fn move_bytes_param_rejects_bytesview() {
    let (_, diags, _) = run_with_errors(
        "fn takes(move b: bytes):\n\treturn\n\nfn main():\n\traw = b\"\\x01\"\n\ttakes(raw[0:1])\n",
    );
    assert!(any_code(&diags, DiagCode::TypeMismatch));
}

#[test]
fn w0003_case_a_warns_for_bytes_in_param_position() {
    // f(bytes(v)) where f takes a borrowed bytes — the re-borrow
    // already serves it.
    let (_, diags, _) = run_with_errors(
        "fn takes(b: bytes):\n\treturn\n\nfn main():\n\traw = b\"\\x01\"\n\ttakes(bytes(raw[0:1]))\n",
    );
    let warnings = diags
        .iter()
        .filter(|d| d.code == DiagCode::RedundantMaterialize)
        .count();
    assert_eq!(
        warnings,
        1,
        "got: {:?}",
        diags.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
    assert!(
        diags
            .iter()
            .any(|d| d.code == DiagCode::RedundantMaterialize
                && d.message.contains("redundant `bytes(...)`")),
        "message must name bytes(...): {:?}",
        diags.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
}

#[test]
fn w0003_str_messages_unchanged() {
    // The generalization must not reword the str warnings.
    let (_, diags, _) = run_with_errors(
        "fn takes(s: str):\n\treturn\n\nfn main():\n\traw = \"ab\"\n\ttakes(str(raw[0:1]))\n",
    );
    assert!(
        diags
            .iter()
            .any(|d| d.code == DiagCode::RedundantMaterialize
                && d.message.contains("redundant `str(...)`")),
        "got: {:?}",
        diags.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
}

#[test]
fn str_materialize_rejects_bytesview_arg() {
    // The `str()` gate must require `strview` specifically: admitting
    // a `bytesview` would lower to a copy WITHOUT UTF-8 validation,
    // breaking "str is valid UTF-8 by construction".
    let (_, diags, _) = run_with_errors("fn main():\n\tb = b\"\\xff\"\n\ts = str(b[0:1])\n");
    assert!(
        diags.iter().any(|d| d.code == DiagCode::TypeMismatch
            && d.message.contains("str() argument must be strview")),
        "got: {:?}",
        diags.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
}

#[test]
fn w0003_materialize_bytes_arg_to_print_warns() {
    // `print` accepts `bytesview` directly — materializing first is a
    // redundant allocation, same as the str shape.
    let (_, diags, _) = run_with_errors("fn main():\n\tb = b\"\\x01\"\n\tprint(bytes(b[0:1]))\n");
    assert_eq!(
        count_code(&diags, DiagCode::RedundantMaterialize),
        1,
        "expected exactly one W0003; got {:?}",
        diags
    );
    assert!(
        diags
            .iter()
            .any(|d| d.code == DiagCode::RedundantMaterialize
                && d.message.contains("redundant `bytes(...)`")),
        "message must name bytes(...): {:?}",
        diags.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
}

#[test]
fn w0003_materialize_str_arg_to_print_still_warns_once() {
    // The bytes fix must not disturb the str shape: exactly one W0003.
    let (_, diags, _) = run_with_errors("fn main():\n\ts: str = \"hi\"\n\tprint(str(s[0:1]))\n");
    assert_eq!(
        count_code(&diags, DiagCode::RedundantMaterialize),
        1,
        "expected exactly one W0003; got {:?}",
        diags
    );
}

#[test]
fn user_defined_fn_bytes_shadows_intercept() {
    // Type names are not reserved: a user `fn bytes` wins (same rule
    // as the str intercept).
    run("fn bytes(x: int):\n\treturn\n\nfn main():\n\tbytes(1)\n").expect("sema ok");
}

#[test]
fn to_str_types_on_bytes_and_bytesview() {
    let (tirs, pool) =
        run("fn main():\n\tb = b\"\\x61\"\n\ts = b.to_str()\n\tv = b[0:1]\n\tt = v.to_str()\n")
            .expect("sema ok");
    let main = tir_named(&tirs, &pool, "main");
    let callee = pool
        .find_str("__ryo_bytes_to_str")
        .expect("callee interned");
    let calls = (1..main.instructions.len())
        .filter(|&idx| {
            let r = TirRef::from_raw(u32::try_from(idx).expect("idx fits u32"));
            main.inst(r).tag == TirTag::Call
                && main.inst(r).ty == pool.str_()
                && main.call_view(r).name == callee
        })
        .count();
    assert_eq!(
        calls, 2,
        "both to_str() calls must lower to __ryo_bytes_to_str"
    );
}

#[test]
fn to_bytes_types_on_str_and_strview() {
    let (tirs, pool) =
        run("fn main():\n\ts = \"ab\"\n\tb = s.to_bytes()\n\tv = s[0:1]\n\tc = v.to_bytes()\n")
            .expect("sema ok");
    let main = tir_named(&tirs, &pool, "main");
    let callee = pool
        .find_str("__ryo_str_to_bytes")
        .expect("callee interned");
    let calls = (1..main.instructions.len())
        .filter(|&idx| {
            let r = TirRef::from_raw(u32::try_from(idx).expect("idx fits u32"));
            main.inst(r).tag == TirTag::Call
                && main.inst(r).ty == pool.bytes()
                && main.call_view(r).name == callee
        })
        .count();
    assert_eq!(
        calls, 2,
        "both to_bytes() calls must lower to __ryo_str_to_bytes"
    );
}

#[test]
fn to_str_on_str_keeps_no_method_diagnostic() {
    // Same diagnostic shape as before M8.4.2 for wrong-family methods.
    let (_, diags, _) = run_with_errors("fn main():\n\ts = \"ab\"\n\tx = s.to_str()\n");
    assert!(
        diags.iter().any(|d| d.code == DiagCode::UndefinedFunction
            && d.message.contains("str has no method 'to_str'")),
        "got: {:?}",
        diags.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
}

#[test]
fn to_bytes_on_bytes_keeps_no_method_diagnostic() {
    let (_, diags, _) = run_with_errors("fn main():\n\tb = b\"\\x61\"\n\tx = b.to_bytes()\n");
    assert!(
        diags.iter().any(|d| d.code == DiagCode::UndefinedFunction
            && d.message.contains("bytes has no method 'to_bytes'")),
        "got: {:?}",
        diags.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
}

#[test]
fn bridging_methods_take_no_arguments() {
    let (_, diags, _) = run_with_errors("fn main():\n\tb = b\"\\x61\"\n\ts = b.to_str(1)\n");
    assert!(any_code(&diags, DiagCode::ArityMismatch));
}

#[test]
fn bytes_index_yields_int() {
    let (tirs, pool) = run("fn main():\n\tb = b\"\\x01\"\n\tx = b[0]\n\tv = b[0:1]\n\ty = v[0]\n")
        .expect("sema ok");
    let main = tir_named(&tirs, &pool, "main");
    let indexed = main
        .instructions
        .iter()
        .filter(|i| i.tag == TirTag::BytesIndex && i.ty == pool.int())
        .count();
    assert_eq!(indexed, 2, "b[0] and v[0] must type as int");
}

#[test]
fn str_indexing_stays_forbidden() {
    let (_, diags, _) = run_with_errors("fn main():\n\ts = \"ab\"\n\tx = s[0]\n");
    assert!(
        diags.iter().any(|d| d.code == DiagCode::TypeMismatch
            && d.message.contains("str does not support indexing")),
        "got: {:?}",
        diags.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
}

#[test]
fn index_requires_int_and_indexable_base() {
    let (_, diags, _) = run_with_errors("fn main():\n\tb = b\"\\x01\"\n\tx = b[\"a\"]\n");
    assert!(
        diags
            .iter()
            .any(|d| d.code == DiagCode::TypeMismatch && d.message.contains("index must be int")),
        "got: {:?}",
        diags.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
    let (_, diags, _) = run_with_errors("fn main():\n\tx = 1\n\ty = x[0]\n");
    assert!(
        diags
            .iter()
            .any(|d| d.code == DiagCode::TypeMismatch
                && d.message.contains("cannot index type 'int'")),
        "got: {:?}",
        diags.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
}
