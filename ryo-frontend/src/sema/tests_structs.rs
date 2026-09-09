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
fn struct_literal_field_type_mismatch() {
    let src = "struct Point:\n\tx: float\n\nfn main():\n\tp = Point{x=1}\n";
    let (_t, diags, _p) = run_with_errors(src);
    assert!(any_code(&diags, DiagCode::TypeMismatch), "got {diags:?}");
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
fn compound_field_assignment_rejects_bad_operator() {
    let src = "struct Point:\n\tx: float\n\nfn main():\n\tmut p = Point{x=1.0}\n\tp.x %= 2.0\n";
    let (_t, diags, _p) = run_with_errors(src);
    assert!(any_code(&diags, DiagCode::FloatModulo), "got {diags:?}");
}
