//! M9 struct sema tests. Harness shared with `tests.rs` via its
//! `pub(super)` helpers (the 2000-line file cap keeps these out of
//! `tests.rs`).

use super::tests::*;
use super::*;
use ryo_core::tir::{TirData, TirTag};
use ryo_core::types::TypeKind;

#[test]
fn struct_literal_type_checks() {
    let src = "struct Point:\n\tx: float\n\ty: float\n\nfn main():\n\tp = Point{x=1.0, y=2.0}\n";
    let (tirs, pool) = run(src).expect("sema ok");
    let main = tir_named(&tirs, &pool, "main");
    let decl = main.var_decl_view(stmt_at(main, 0));
    let view = main.struct_lit_view(decl.initializer);
    assert_eq!(view.fields.len(), 2);
    assert!(matches!(pool.kind(view.ty), TypeKind::Struct));
}

#[test]
fn struct_literal_fields_emitted_in_canonical_order() {
    // Literal order is irrelevant after sema: the TIR payload is
    // sorted by declaration-order field index.
    let src = "struct Point:\n\tx: float\n\ty: float\n\nfn main():\n\tp = Point{y=2.0, x=1.0}\n";
    let (tirs, pool) = run(src).expect("sema ok");
    let main = tir_named(&tirs, &pool, "main");
    let decl = main.var_decl_view(stmt_at(main, 0));
    let view = main.struct_lit_view(decl.initializer);
    let indices: Vec<u32> = view.fields.iter().map(|&(i, _)| i).collect();
    assert_eq!(indices, vec![0, 1]);
}

#[test]
fn struct_literal_missing_field() {
    let src = "struct Point:\n\tx: float\n\ty: float\n\nfn main():\n\tp = Point{x=1.0}\n";
    let (_t, diags, _p) = run_with_errors(src);
    assert!(
        any_code(&diags, DiagCode::MissingStructFields),
        "got {diags:?}"
    );
}

#[test]
fn struct_literal_unknown_and_duplicate_fields() {
    let unknown = "struct Point:\n\tx: float\n\nfn main():\n\tp = Point{z=1.0, x=2.0}\n";
    let (_t, diags, _p) = run_with_errors(unknown);
    assert!(any_code(&diags, DiagCode::UnknownField), "got {diags:?}");
    let dup = "struct Point:\n\tx: float\n\nfn main():\n\tp = Point{x=1.0, x=2.0}\n";
    let (_t, diags, _p) = run_with_errors(dup);
    assert!(
        any_code(&diags, DiagCode::DuplicateStructField),
        "got {diags:?}"
    );
}

#[test]
fn bad_field_suppresses_consequential_missing_fields_error() {
    // 'z' is unknown (E0038) and 'y' is absent; the derived
    // 'missing field(s)' error (E0039) must not pile on.
    let unknown = "struct Point:\n\tx: int\n\ty: int\n\nfn main():\n\tp = Point{z=1, x=2}\n";
    let (_t, diags, _p) = run_with_errors(unknown);
    assert_eq!(diags.len(), 1, "expected exactly one error: {diags:?}");
    assert_eq!(diags[0].code, DiagCode::UnknownField);

    // Same for the duplicate-field shape.
    let dup = "struct Point:\n\tx: int\n\ty: int\n\nfn main():\n\tp = Point{x=1, x=2}\n";
    let (_t, diags, _p) = run_with_errors(dup);
    assert_eq!(diags.len(), 1, "expected exactly one error: {diags:?}");
    assert_eq!(diags[0].code, DiagCode::DuplicateStructField);
}

#[test]
fn struct_literal_field_type_mismatch() {
    let src = "struct Point:\n\tx: float\n\nfn main():\n\tp = Point{x=1}\n";
    let (_t, diags, _p) = run_with_errors(src);
    assert!(any_code(&diags, DiagCode::TypeMismatch), "got {diags:?}");
}

#[test]
fn literal_of_failed_struct_does_not_re_report_unknown_struct() {
    // E0005 (InfiniteSize) fires in astgen for the recursive struct;
    // a literal use must recover quietly, not re-report E0001
    // "unknown struct" on top.
    let src = "struct Node:\n\tnext: Node\n\nfn main():\n\tn = Node{}\n";
    let (_t, diags, _p) = run_with_errors(src);
    assert_eq!(diags.len(), 1, "expected exactly one error: {diags:?}");
    assert_eq!(diags[0].code, DiagCode::InfiniteSize);
}

#[test]
fn literal_of_never_declared_struct_still_reports_unknown() {
    let src = "fn main():\n\tn = Widget{}\n";
    let (_t, diags, _p) = run_with_errors(src);
    assert!(
        any_code(&diags, DiagCode::UnknownType),
        "never-declared name must still error: {diags:?}"
    );
}

#[test]
fn struct_literal_unknown_name_recovers() {
    let src = "fn main():\n\tp = Nope{x=1.0}\n";
    let (tirs, diags, _p) = run_with_errors(src);
    assert!(any_code(&diags, DiagCode::UnknownType), "got {diags:?}");
    // Recovery: the slot is poisoned with an Unreachable, TIR stays
    // well-formed.
    let main = tir_named(&tirs, &_p, "main");
    let decl = main.var_decl_view(stmt_at(main, 0));
    assert!(matches!(
        main.inst(decl.initializer).tag,
        TirTag::Unreachable
    ));
}

#[test]
fn field_access_resolves_type() {
    let src = "struct Point:\n\tx: float\n\nfn get(p: Point) -> float:\n\treturn p.x\n";
    let (tirs, pool) = run(src).expect("sema ok");
    let get = tir_named(&tirs, &pool, "get");
    let ret = stmt_at(get, 0);
    let TirData::UnOp(operand) = get.inst(ret).data else {
        panic!("Return must carry TirData::UnOp");
    };
    let inst = get.inst(operand);
    assert!(matches!(inst.tag, TirTag::FieldAccess));
    assert_eq!(inst.ty, pool.float());
}

#[test]
fn field_access_unknown_field() {
    let src = "struct Point:\n\tx: float\n\nfn main():\n\tp = Point{x=1.0}\n\ty = p.z\n";
    let (_t, diags, _p) = run_with_errors(src);
    assert!(any_code(&diags, DiagCode::UnknownField), "got {diags:?}");
}

#[test]
fn field_access_unknown_field_names_the_shape_kind() {
    // The message names the kind so the reader has the reference
    // vocabulary: "tuple" and "anonymous struct" are searchable, the
    // bare paren/brace display is not. Named structs keep the
    // original wording — the type name already says everything.
    let cases: &[(&str, &str)] = &[
        (
            "fn main():\n\tz = (\"zero\",)\n\ty = z.1\n",
            "tuple '(str,)' has no field '1'",
        ),
        (
            "fn main():\n\tp = {x=1}\n\ty = p.q\n",
            "anonymous struct '{x: int}' has no field 'q'",
        ),
        (
            "struct Point:\n\tx: int\n\nfn main():\n\tp = Point{x=1}\n\ty = p.z\n",
            "'Point' has no field 'z'",
        ),
    ];
    for (src, expected) in cases {
        let (_t, diags, _p) = run_with_errors(src);
        let diag = diags
            .iter()
            .find(|d| d.code == DiagCode::UnknownField)
            .unwrap_or_else(|| panic!("expected E0038 for {src:?}, got {diags:?}"));
        assert!(
            diag.message.contains(expected),
            "message should contain {expected:?}, got {:?}",
            diag.message,
        );
    }

    // Single candidate: the note points straight at it, no guessing.
    let (_t, diags, _p) = run_with_errors("fn main():\n\tz = (\"zero\",)\n\ty = z.1\n");
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::UnknownField)
        .expect("E0038");
    assert!(
        diag.notes
            .iter()
            .any(|n| n.message.contains("the only valid field is '0'")),
        "expected a single-candidate note, got {diags:?}",
    );

    // Multiple candidates: no note — the field list in the message
    // already carries the information.
    let (_t, diags, _p) = run_with_errors("fn main():\n\tz = (1, 2)\n\ty = z.5\n");
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::UnknownField)
        .expect("E0038");
    assert!(
        diag.notes.is_empty(),
        "multi-field shapes must not get the note, got {diags:?}",
    );
}

#[test]
fn field_access_on_non_struct_is_error() {
    let src = "fn main():\n\tx = 1\n\ty = x.foo\n";
    let (_t, diags, _p) = run_with_errors(src);
    assert!(any_code(&diags, DiagCode::NotAStruct), "got {diags:?}");
}

#[test]
fn view_typed_field_rejected() {
    let src = "struct Parser:\n\tsource: strview\n";
    let (_t, diags, _p) = run_with_errors(src);
    assert!(any_code(&diags, DiagCode::ViewFieldType), "got {diags:?}");
}

#[test]
fn copy_struct_assignment_allows_both_bindings() {
    // Copy inference: Point is all-Copy, so `q = p` copies — no move
    // diagnostic.
    let src = "struct Point:\n\tx: float\n\nfn main():\n\tp = Point{x=1.0}\n\tq = p\n\tr = p.x\n\ts = q.x\n";
    let (_t, diags, _p) = run_with_errors(src);
    assert!(
        !any_code(&diags, DiagCode::UseAfterMove),
        "Point must be Copy; got {diags:?}"
    );
    assert!(diags.is_empty(), "expected clean sema; got {diags:?}");
}

#[test]
fn field_assignment_type_checks() {
    let src = "struct Point:\n\tx: float\n\nfn main():\n\tmut p = Point{x=1.0}\n\tp.x = 2.0\n";
    assert!(run(src).is_ok());
}

#[test]
fn field_assignment_requires_mut() {
    let src = "struct Point:\n\tx: float\n\nfn main():\n\tp = Point{x=1.0}\n\tp.x = 2.0\n";
    let (_t, diags, _p) = run_with_errors(src);
    assert!(any_code(&diags, DiagCode::ImmutableAssign), "got {diags:?}");
}

#[test]
fn compound_field_assignment_type_checks() {
    let src = "struct Point:\n\tx: float\n\nfn main():\n\tmut p = Point{x=1.0}\n\tp.x += 2.0\n";
    assert!(run(src).is_ok());
}

#[test]
fn compound_field_assignment_requires_mut() {
    let src = "struct Point:\n\tx: float\n\nfn main():\n\tp = Point{x=1.0}\n\tp.x += 2.0\n";
    let (_t, diags, _p) = run_with_errors(src);
    assert!(any_code(&diags, DiagCode::ImmutableAssign), "got {diags:?}");
}

#[test]
fn nested_field_assignment_type_checks() {
    let src = "struct Inner:\n\tv: int\n\nstruct Outer:\n\tinner: Inner\n\nfn main():\n\tmut o = Outer{inner=Inner{v=1}}\n\to.inner.v = 2\n\to.inner.v += 3\n";
    assert!(run(src).is_ok());
}

#[test]
fn field_assignment_type_mismatch() {
    let src = "struct Point:\n\tx: float\n\nfn main():\n\tmut p = Point{x=1.0}\n\tp.x = 1\n";
    let (_t, diags, _p) = run_with_errors(src);
    assert!(any_code(&diags, DiagCode::TypeMismatch), "got {diags:?}");
}

#[test]
fn field_assignment_undefined_root() {
    let src = "struct Point:\n\tx: float\n\nfn main():\n\tp.x = 1.0\n";
    let (_t, diags, _p) = run_with_errors(src);
    assert!(
        any_code(&diags, DiagCode::UndefinedAssignTarget),
        "got {diags:?}"
    );
}

#[test]
fn inout_field_arg_accepted_when_root_mut() {
    // `&p.x` is a valid inout argument (M9): the ROOT binding `p` is
    // `mut`, so the field borrow is allowed.
    let src = "struct Point:\n\tx: int\n\ty: int\n\nfn inc(inout v: int):\n\tv += 1\n\nfn main():\n\tmut p = Point{x=1, y=2}\n\tinc(&p.x)\n";
    assert!(run(src).is_ok());
}

#[test]
fn inout_nested_field_arg_accepted() {
    let src = "struct Inner:\n\tv: int\n\nstruct Outer:\n\tinner: Inner\n\nfn inc(inout v: int):\n\tv += 1\n\nfn main():\n\tmut o = Outer{inner=Inner{v=1}}\n\tinc(&o.inner.v)\n";
    assert!(run(src).is_ok());
}

#[test]
fn inout_field_arg_requires_mut_root() {
    // Same rule as a bare `&x` on an immutable binding: BorrowMismatch.
    let src = "struct Point:\n\tx: int\n\nfn inc(inout v: int):\n\tv += 1\n\nfn main():\n\tp = Point{x=1}\n\tinc(&p.x)\n";
    let (_t, diags, _p) = run_with_errors(src);
    assert!(any_code(&diags, DiagCode::BorrowMismatch), "got {diags:?}");
}

#[test]
fn inout_field_arg_unknown_field_uses_field_diagnostic() {
    // An invalid hop keeps the FieldAccess analysis diagnostic — no
    // extra BorrowMismatch noise, no panic.
    let src = "struct Point:\n\tx: int\n\nfn inc(inout v: int):\n\tv += 1\n\nfn main():\n\tmut p = Point{x=1}\n\tinc(&p.z)\n";
    let (_t, diags, _p) = run_with_errors(src);
    assert!(any_code(&diags, DiagCode::UnknownField), "got {diags:?}");
    assert!(!any_code(&diags, DiagCode::BorrowMismatch), "got {diags:?}");
}

#[test]
fn compound_field_assignment_rejects_bad_operator() {
    let src = "struct Point:\n\tx: float\n\nfn main():\n\tmut p = Point{x=1.0}\n\tp.x %= 2.0\n";
    let (_t, diags, _p) = run_with_errors(src);
    assert!(any_code(&diags, DiagCode::FloatModulo), "got {diags:?}");
}

#[test]
fn derive_eq_struct_with_eq_capable_fields_is_clean() {
    // `int` and `str` are both Eq-capable primitives, so the derive
    // resolves with no diagnostics (the `==` acceptance itself is
    // pinned by the operator-gate work).
    let src = "#[derive(Eq)] struct P:\n\tx: int\n\ty: str\n";
    assert!(run(src).is_ok());
}

#[test]
fn derive_eq_rejects_non_eq_field() {
    // `Inner` has no `#[derive(Eq)]`, so `Outer`'s derive must name
    // the field and its type.
    let src = "struct Inner:\n\tx: int\n\n#[derive(Eq)] struct Outer:\n\tinner: Inner\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::DeriveFieldNotEq)
        .expect("DeriveFieldNotEq must fire");
    assert_eq!(
        diag.message,
        "cannot derive 'Eq' for 'Outer': field 'inner' of type 'Inner' is not Eq-capable"
    );
}

#[test]
fn repr_c_struct_compiles_clean() {
    // `#[repr(C)]` is recorded on the type, not branched on: the
    // default layout algorithm computes identically.
    let src = "#[repr(C)] struct Mixed:\n\ta: int\n\tb: float\n\tc: int\n";
    assert!(run(src).is_ok());
}

#[test]
fn print_struct_rewrites_to_debug_repr() {
    // M9.1: print() on a struct value is rewritten at the TIR level to
    // print(DebugRepr(arg)) — the repr temp is a normal str producer.
    let src = "struct Point:\n\tx: int\n\nfn main():\n\tp = Point{x=1}\n\tprint(p)\n";
    let (tirs, pool) = run(src).expect("sema ok");
    let main = tir_named(&tirs, &pool, "main");
    assert!(
        main.instructions.iter().any(|i| i.tag == TirTag::DebugRepr),
        "print(struct) must lower to a DebugRepr inst"
    );
}

#[test]
fn print_scalar_rewrites_to_debug_repr() {
    // int / float / bool are Debug-capable — each print() rewrites to
    // DebugRepr exactly like the struct case.
    let cases = [
        "fn main():\n\tprint(42)\n",
        "fn main():\n\tprint(3.14)\n",
        "fn main():\n\tprint(true)\n",
    ];
    for src in cases {
        let (tirs, pool) = run(src).expect("sema ok");
        let main = tir_named(&tirs, &pool, "main");
        assert!(
            main.instructions.iter().any(|i| i.tag == TirTag::DebugRepr),
            "print(int/float/bool) must lower to a DebugRepr inst: {src}"
        );
    }
}

#[test]
fn derived_struct_equality_lowers_to_struct_eq() {
    // M9.1: `==` on an Eq-derived struct lowers to TirTag::StructEq —
    // BinOp payload, bool result.
    let src = "#[derive(Eq)] struct P:\n\tx: int\n\nfn main():\n\tp = P{x=1}\n\tq = P{x=2}\n\tr = p == q\n";
    let (tirs, pool) = run(src).expect("sema ok");
    let main = tir_named(&tirs, &pool, "main");
    let eq = main
        .instructions
        .iter()
        .find(|i| i.tag == TirTag::StructEq)
        .expect("p == q must lower to a StructEq inst");
    assert_eq!(eq.ty, pool.bool_(), "StructEq result must be bool");
}

#[test]
fn derived_struct_inequality_lowers_to_struct_ne() {
    // `!=` lowers to StructNe directly at sema — not StructEq + not.
    let src = "#[derive(Eq)] struct P:\n\tx: int\n\nfn main():\n\tp = P{x=1}\n\tq = P{x=2}\n\tr = p != q\n";
    let (tirs, pool) = run(src).expect("sema ok");
    let main = tir_named(&tirs, &pool, "main");
    let ne = main
        .instructions
        .iter()
        .find(|i| i.tag == TirTag::StructNe)
        .expect("p != q must lower to a StructNe inst");
    assert_eq!(ne.ty, pool.bool_(), "StructNe result must be bool");
}

#[test]
fn struct_inequality_without_derive_requires_eq() {
    // `!=` hits the same gate, with the operator spelling interpolated.
    let src = "struct P:\n\tx: int\n\nfn main():\n\tp = P{x=1}\n\tq = P{x=2}\n\tr = p != q\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::EqDeriveRequired)
        .expect("EqDeriveRequired must fire for !=");
    assert_eq!(diag.message, "binary operator `!=` requires `P` to be `Eq`");
}

#[test]
fn struct_equality_without_derive_requires_eq() {
    // No `#[derive(Eq)]` → EqDeriveRequired with the roadmap's fix-it
    // wording, message and help asserted verbatim.
    let src = "struct P:\n\tx: int\n\nfn main():\n\tp = P{x=1}\n\tq = P{x=2}\n\tr = p == q\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::EqDeriveRequired)
        .expect("EqDeriveRequired must fire");
    assert_eq!(diag.message, "binary operator `==` requires `P` to be `Eq`");
    assert!(
        diag.notes
            .iter()
            .any(|n| n.message == "help: add `#[derive(Eq)]` to `P`"),
        "expected the derive fix-it help note; got {:?}",
        diag.notes
    );
}

#[test]
fn derived_struct_with_mixed_eq_capable_fields_compares_clean() {
    // int, float, str, bool, bytes, and a nested derived struct are all
    // Eq-capable (Task 2's is_eq_capable), so both `==` and `!=` on the
    // derived struct resolve with no diagnostics.
    let src = "#[derive(Eq)] struct Inner:\n\tv: int\n\n#[derive(Eq)] struct P:\n\ta: int\n\tb: float\n\tc: str\n\td: bool\n\te: bytes\n\tinner: Inner\n\nfn main():\n\tp = P{a=1, b=2.0, c=\"x\", d=true, e=b\"yz\", inner=Inner{v=3}}\n\tq = P{a=1, b=2.0, c=\"x\", d=true, e=b\"yz\", inner=Inner{v=3}}\n\tr = p == q\n\ts = p != q\n";
    assert!(run(src).is_ok());
}

#[test]
fn anon_eq_structural() {
    // M10: `==` / `!=` on anonymous structs lower to the shared
    // StructEq/StructNe tags — Eq-capability is structural (every
    // field Eq-capable, computed recursively), so no attribute gates
    // the operator. A nested shape compares through the same
    // memberwise path as a nested derived struct.
    let src = "fn main():\n\tp = {x=1, y=2.0, s=\"a\", n={v=3}}\n\tq = {x=1, y=2.0, s=\"a\", n={v=3}}\n\tr = p == q\n\ts = p != q\n";
    let (tirs, pool) = run(src).expect("sema ok");
    let main = tir_named(&tirs, &pool, "main");
    let eq = main
        .instructions
        .iter()
        .find(|i| i.tag == TirTag::StructEq)
        .expect("p == q must lower to a StructEq inst");
    assert_eq!(eq.ty, pool.bool_(), "StructEq result must be bool");
    let ne = main
        .instructions
        .iter()
        .find(|i| i.tag == TirTag::StructNe)
        .expect("p != q must lower to a StructNe inst");
    assert_eq!(ne.ty, pool.bool_(), "StructNe result must be bool");
}

#[test]
fn anon_eq_field_not_eq_capable() {
    // A shape with a non-Eq field — a struct without `#[derive(Eq)]`
    // — is not Eq-capable: the operator is rejected with
    // AnonFieldNotEq naming the offending field and its type.
    let src = "struct Inner:\n\tx: int\n\nfn main():\n\tp = {v=Inner{x=1}, w=2}\n\tq = {v=Inner{x=1}, w=2}\n\tr = p == q\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::AnonFieldNotEq)
        .expect("AnonFieldNotEq must fire");
    assert_eq!(
        diag.message,
        "binary operator `==` requires field 'v' of type 'Inner' to be Eq-capable"
    );
}

#[test]
fn positional_key_on_named_struct_gets_a_teaching_note() {
    // The graduation stumble: positional access on a named struct.
    // The note teaches the rule instead of leaving the field list to
    // speak for itself.
    let src =
        "struct Point:\n\tx: int\n\ty: int\n\nfn main():\n\tp = Point{x=1, y=2}\n\tprint(p.0)\n";
    let (_t, diags, _p) = run_with_errors(src);
    let d = diags
        .iter()
        .find(|d| d.code == DiagCode::UnknownField)
        .expect("E0038");
    assert!(
        d.notes
            .iter()
            .any(|n| n.message.contains("accessed by field name")),
        "expected the field-name note, got: {diags:?}"
    );
}
