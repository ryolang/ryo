use super::tests::*;
use super::*;
use crate::astgen;
use ryo_core::ast;
use ryo_core::tir::{TirData, TirTag};
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

#[test]
fn unit_positional_and_named_errors_construct_enum_literals() {
    let src = "error Unit\nerror Positional(int, str)\nerror Named(status: int, message: str)\n\
               fn main():\n\tu = Unit\n\tp = Positional(7, \"seven\")\n\tn = Named(message=\"seven\", status=7)\n";
    let (tirs, pool) = run(src).expect("declared errors should construct");
    let main = tir_named(&tirs, &pool, "main");
    let literals: Vec<_> = main
        .instructions
        .iter()
        .enumerate()
        .skip(1)
        .filter(|(_, inst)| inst.tag == TirTag::EnumLit)
        .map(|(index, inst)| (TirRef::from_raw(index as u32), inst))
        .collect();
    assert_eq!(literals.len(), 3, "unexpected TIR: {main:?}");
    assert!(literals.iter().all(|(_, inst)| {
        pool.kind(inst.ty) == TypeKind::DeclaredError
            && pool.enum_view(inst.ty).variants().count() == 1
    }));
    let fields: Vec<_> = main
        .enum_lit_view(literals[2].0)
        .fields()
        .map(|(index, _)| index)
        .collect();
    assert_eq!(fields, vec![0, 1], "named fields use declaration order");
}

#[test]
fn error_constructor_shape_and_payload_errors_are_diagnosed() {
    let cases = [
        (
            "error E(int)\nfn main():\n\tx = E()\n",
            DiagCode::MissingVariantFields,
        ),
        (
            "error E(int)\nfn main():\n\tx = E(1, 2)\n",
            DiagCode::UnknownVariantField,
        ),
        (
            "error E(value: int)\nfn main():\n\tx = E(other=1)\n",
            DiagCode::UnknownVariantField,
        ),
        (
            "error E(value: int)\nfn main():\n\tx = E(value=1, value=2)\n",
            DiagCode::DuplicateVariantField,
        ),
        (
            "error E(a: int, b: str)\nfn main():\n\tx = E(a=1)\n",
            DiagCode::MissingVariantFields,
        ),
        (
            "error E(int)\nfn main():\n\tx = E(\"bad\")\n",
            DiagCode::TypeMismatch,
        ),
        (
            "error E(value: int)\nfn main():\n\tx = E(value=\"bad\")\n",
            DiagCode::TypeMismatch,
        ),
    ];
    for (src, code) in cases {
        let (_tirs, diags, _pool) = run_with_errors(src);
        assert!(any_code(&diags, code), "{src:?}: {diags:?}");
        assert!(
            !any_code(&diags, DiagCode::UndefinedFunction),
            "{src:?}: {diags:?}"
        );
        assert!(
            !any_code(&diags, DiagCode::UndefinedVariable),
            "{src:?}: {diags:?}"
        );
    }
}

#[test]
fn ordinary_calls_reject_named_arguments() {
    let src = "fn Work(value: int) -> int:\n\treturn value\n\
               fn main():\n\tresult = Work(value=1)\n";
    let (_tirs, diags, _pool) = run_with_errors(src);
    assert!(
        diags.iter().any(|diag| {
            diag.code == DiagCode::NamedArgInParens || diag.message.contains("named arguments")
        }),
        "unexpected diagnostics: {diags:?}"
    );
}

#[test]
fn returns_and_typed_bindings_wrap_success_and_error_values() {
    let src = "error A\nerror B\n\
               fn success() -> (A | B)!int:\n\treturn 7\n\
               fn failure() -> (A | B)!int:\n\treturn A\n\
               fn locals() -> A!int:\n\tvalue: A!int = 1\n\terr: A!int = A\n\treturn value\n";
    let (tirs, pool) = run(src).expect("union coercions should succeed");
    for name in ["success", "failure"] {
        let tir = tir_named(&tirs, &pool, name);
        let ret = stmt_at(tir, 0);
        let TirData::UnOp(operand) = tir.inst(ret).data else {
            panic!("return must have an operand")
        };
        assert_eq!(tir.inst(operand).tag, TirTag::ErrorUnionCoerce, "{name}");
        assert_eq!(tir.inst(operand).ty, tir.return_type);
    }
    let locals = tir_named(&tirs, &pool, "locals");
    for index in 0..2 {
        let decl = locals.var_decl_view(stmt_at(locals, index));
        assert_eq!(locals.inst(decl.initializer).tag, TirTag::ErrorUnionCoerce);
        assert_eq!(locals.inst(decl.initializer).ty, locals.return_type);
    }
}

#[test]
fn narrower_unions_widen_but_narrowing_is_rejected() {
    let src = "error A\nerror B\n\
               fn narrow() -> A!int:\n\treturn 1\n\
               fn wide() -> (A | B)!int:\n\tvalue = narrow()\n\treturn value\n";
    let (tirs, pool) = run(src).expect("subset union should widen");
    let wide = tir_named(&tirs, &pool, "wide");
    let ret = stmt_at(wide, 1);
    let TirData::UnOp(operand) = wide.inst(ret).data else {
        panic!("return must have an operand")
    };
    assert_eq!(wide.inst(operand).tag, TirTag::ErrorUnionCoerce);
    assert_eq!(wide.inst(operand).ty, wide.return_type);

    let bad = "error A\nerror B\n\
               fn wide() -> (A | B)!int:\n\treturn 1\n\
               fn narrow() -> A!int:\n\tvalue = wide()\n\treturn value\n";
    let (_tirs, diags, _pool) = run_with_errors(bad);
    assert!(any_code(&diags, DiagCode::TypeMismatch), "{diags:?}");
}

#[test]
fn incompatible_success_and_nonmember_errors_are_rejected() {
    let cases = [
        "error A\nfn f() -> A!int:\n\treturn \"bad\"\n",
        "error A\nerror B\nfn f() -> A!int:\n\treturn B\n",
        "error A\nerror B\nfn f() -> A!int:\n\tvalue: B!int = 1\n\treturn value\n",
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
fn unannotated_raw_union_bindings_remain_unions() {
    let src = "error A\nfn fail() -> A!int:\n\treturn A\nfn main():\n\tresult = fail()\n";
    let (tirs, pool) = run(src).expect("raw inferred union binding should succeed");
    let main = tir_named(&tirs, &pool, "main");
    let decl = stmt_at(main, 0);
    assert_eq!(pool.kind(main.inst(decl).ty), TypeKind::ErrorUnion);
    let init = main.var_decl_view(decl).initializer;
    assert_eq!(main.inst(init).ty, main.inst(decl).ty);
}

#[test]
fn value_bindings_shadow_error_constructors() {
    let src = "error E\n\
               fn param(E: int) -> int:\n\treturn E\n\
               fn main():\n\tE = 7\n\tvalue = E\n\tother = param(E)\n";
    let (tirs, pool) = run(src).expect("bound values should win over error names");
    let param = tir_named(&tirs, &pool, "param");
    let ret = stmt_at(param, 0);
    let TirData::UnOp(operand) = param.inst(ret).data else {
        panic!("return must have an operand")
    };
    assert_eq!(param.inst(operand).tag, TirTag::Var);
    assert_eq!(param.inst(operand).ty, pool.int());
    let main = tir_named(&tirs, &pool, "main");
    let value_init = main.var_decl_view(stmt_at(main, 1)).initializer;
    assert_eq!(main.inst(value_init).tag, TirTag::Var);
    assert_eq!(main.inst(value_init).ty, pool.int());
}

#[test]
fn direct_error_union_use_is_rejected_with_unwrap_guidance() {
    let bodies = [
        "result = fail()\n\tprint(result.0)\n",
        "result = fail()\n\tprint(result + 1)\n",
        "result = fail()\n\tif result:\n\t\tprint(1)\n",
        "result = fail()\n\tprint(result)\n",
        "result = fail()\n\tresult.len()\n",
        "result = fail()\n\tmatch result:\n\t\t_: 0\n",
    ];
    for body in bodies {
        let src = format!("error A\nfn fail() -> A!int:\n\treturn A\nfn main():\n\t{body}");
        let (_tirs, diags, _pool) = run_with_errors(&src);
        assert!(
            diags.iter().any(|diag| {
                diag.message.contains("try")
                    || diag.message.contains("catch")
                    || diag
                        .notes
                        .iter()
                        .any(|note| note.message.contains("try") || note.message.contains("catch"))
            }),
            "{src:?}: {diags:?}"
        );
    }
}

#[test]
fn void_success_return_wraps_ok() {
    let src = "error A\nfn succeed() -> A!void:\n\treturn\n";
    let (tirs, pool) = run(src).expect("void success should wrap in Ok");
    let tir = tir_named(&tirs, &pool, "succeed");
    let ret = stmt_at(tir, 0);
    assert_eq!(tir.inst(ret).tag, TirTag::Return);
    let TirData::UnOp(operand) = tir.inst(ret).data else {
        panic!("return must have an operand")
    };
    assert_eq!(tir.inst(operand).tag, TirTag::ErrorUnionCoerce);
    assert_eq!(tir.inst(operand).ty, tir.return_type);
}
