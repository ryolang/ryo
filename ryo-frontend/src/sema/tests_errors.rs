use super::tests::*;
use super::*;
use crate::astgen;
use ryo_core::ast;
use ryo_core::types::TypeKind;

#[test]
fn explicit_and_inferred_error_union_signatures_resolve() {
    let src = "error A\nerror B\n\
               fn one() -> A!str:\n\tpanic(\"x\")\n\
               fn many() -> (B | A | A)!str:\n\tpanic(\"x\")\n\
               fn inferred() -> !str:\n\tpanic(\"x\")\n\
               fn void_success() -> A!void:\n\tpanic(\"x\")\n";
    let (tirs, pool) = run(src).expect("error union signatures should resolve");

    let one = tir_named(&tirs, &pool, "one").return_type;
    let many = tir_named(&tirs, &pool, "many").return_type;
    let inferred = tir_named(&tirs, &pool, "inferred").return_type;
    let void_success = tir_named(&tirs, &pool, "void_success").return_type;
    assert_eq!(pool.display(one).to_string(), "A!str");
    assert_eq!(pool.display(many).to_string(), "(A | B)!str");
    assert_eq!(pool.display(inferred).to_string(), "!str");
    assert_eq!(pool.display(void_success).to_string(), "A!void");
    assert_eq!(pool.kind(one), TypeKind::ErrorUnion);
    assert_eq!(pool.kind(many), TypeKind::ErrorUnion);
    assert_eq!(pool.kind(inferred), TypeKind::InferredErrorUnion);
    assert_eq!(pool.error_union_view(void_success).success, pool.void());
}

#[test]
fn later_declared_error_resolves_with_owned_payload() {
    let src = "fn f() -> Later!void:\n\tpanic(\"x\")\n\
               error Later(str)\n";
    let (tirs, pool) = run(src).expect("later error declaration should resolve");
    let return_type = tir_named(&tirs, &pool, "f").return_type;
    let error_set = pool.error_union_view(return_type).error_set;
    assert_eq!(pool.kind(error_set), TypeKind::DeclaredError);
    assert_eq!(pool.display(error_set).to_string(), "Later");
    let variant = pool.enum_view(error_set).variants().next().unwrap();
    assert_eq!(variant.fields.len(), 1);
    assert_eq!(variant.fields[0].ty, pool.str_());
}

#[test]
fn explicit_error_unions_are_valid_owned_annotations() {
    let src = "error E\n\
               struct Holder:\n\tvalue: E!int\n\
               fn take(value: E!int):\n\treturn\n";
    run(src).expect("explicit unions should be valid parameter and field types");
}

#[test]
fn duplicate_error_declaration_is_rejected() {
    let (_tirs, diags, _pool) = run_with_errors("error A\nerror A\n");
    assert_eq!(count_code(&diags, DiagCode::DuplicateDeclaration), 1);
    assert_eq!(diags.len(), 1, "unexpected diagnostics: {diags:?}");
}

#[test]
fn error_names_collide_with_struct_and_enum_names() {
    let cases = [
        "error A\nstruct A:\n\tvalue: int\n",
        "struct A:\n\tvalue: int\nerror A\n",
        "error A\nenum A:\n\tV\n",
        "enum A:\n\tV\nerror A\n",
    ];
    for src in cases {
        let (_tirs, diags, _pool) = run_with_errors(src);
        assert_eq!(
            count_code(&diags, DiagCode::DuplicateDeclaration),
            1,
            "{src:?}: {diags:?}"
        );
        assert_eq!(diags.len(), 1, "{src:?}: {diags:?}");
    }
}

#[test]
fn view_typed_error_payload_is_rejected() {
    for src in ["error E(strview)\n", "error E(value: bytesview)\n"] {
        let (_tirs, diags, _pool) = run_with_errors(src);
        assert!(
            any_code(&diags, DiagCode::ViewFieldType),
            "{src:?}: {diags:?}"
        );
    }
}

#[test]
fn infinite_by_value_error_payload_is_rejected() {
    let cases = [
        "error Loop(Loop)\n",
        "error A(B)\nerror B(A)\n",
        "error A(S)\nstruct S:\n\ta: A\n",
    ];
    for src in cases {
        let (_tirs, diags, _pool) = run_with_errors(src);
        assert!(
            any_code(&diags, DiagCode::InfiniteSize),
            "{src:?}: {diags:?}"
        );
    }
}

#[test]
fn duplicate_error_payload_field_is_rejected() {
    let (_tirs, diags, _pool) = run_with_errors("error Duplicate(field: int, field: str)\n");
    assert!(any_code(&diags, DiagCode::DuplicateStructField));
}

#[test]
fn invalid_error_union_success_types_are_rejected() {
    let cases = [
        "error E\nfn f() -> E!never:\n\tpanic(\"x\")\n",
        "error E\nfn f() -> E!strview:\n\tpanic(\"x\")\n",
        "error E\nfn f() -> E!bytesview:\n\tpanic(\"x\")\n",
        "error E\nfn f() -> E!E:\n\tpanic(\"x\")\n",
        "error E\nerror F\nfn f() -> E!F!int:\n\tpanic(\"x\")\n",
    ];
    for src in cases {
        let (_tirs, diags, _pool) = run_with_errors(src);
        assert!(
            any_code(&diags, DiagCode::TypeMismatch),
            "{src:?}: {diags:?}"
        );
    }
}

#[test]
fn inferred_error_union_is_rejected_outside_function_return_position() {
    let cases = [
        "fn f(value: !int):\n\treturn\n",
        "error E(!int)\n",
        "struct S:\n\tvalue: !int\n",
        "fn main():\n\tvalue: !int = 1\n",
    ];
    for src in cases {
        let (_tirs, diags, _pool) = run_with_errors(src);
        assert!(
            diags.iter().any(|d| d.code == DiagCode::TypeMismatch
                && d.message.contains("function-return position")),
            "{src:?}: {diags:?}"
        );
    }
}

#[test]
fn explicit_empty_error_set_is_rejected_during_lowering() {
    let mut pool = InternPool::new();
    let function_name = pool.intern_str("f");
    let int_name = pool.intern_str("int");
    let mut ast = ast::Ast::new();
    let int = ast::TypeExpr::new(int_name, Span::EMPTY);
    let return_type = ast.error_union_type(&[], int, false, Span::EMPTY);
    let body = ast.return_stmt(None, Span::EMPTY);
    let function = ast.function_def(
        ast::Ident::new(function_name, Span::EMPTY),
        &[],
        Some(return_type),
        &[body],
        Span::EMPTY,
    );
    ast.set_top_level(vec![function]);

    let mut sink = DiagSink::new();
    let _uir = astgen::generate(&ast, &mut pool, &mut sink);
    let diags = sink.into_diags();
    assert!(any_code(&diags, DiagCode::TypeMismatch));
    assert!(diags[0].message.contains("at least one error"));
}

#[test]
fn error_union_member_must_be_a_declared_error() {
    let src = "struct S:\n\tvalue: int\nfn f() -> S!int:\n\tpanic(\"x\")\n";
    let (_tirs, diags, _pool) = run_with_errors(src);
    assert!(any_code(&diags, DiagCode::TypeMismatch));
}
