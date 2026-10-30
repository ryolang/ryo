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
    assert_eq!(pool.kind(inferred), TypeKind::ErrorUnion);
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
fn reassignment_coerces_success_error_and_narrower_union() {
    let src = "error A\nerror B\n\
               fn main():\n\tmut x: (A | B)!int = 1\n\tx = 2\n\tx = A\n\tmut narrow: A!int = 3\n\tx = narrow\n";
    let (tirs, pool) = run(src).expect("union reassignment should coerce");
    let main = tir_named(&tirs, &pool, "main");
    for index in [1, 2, 4] {
        let value = main.assign_view(stmt_at(main, index)).value;
        assert_eq!(main.inst(value).tag, TirTag::ErrorUnionCoerce);
        assert_eq!(main.inst(value).ty, main.inst(stmt_at(main, index)).ty);
    }
}

#[test]
fn field_assignment_coerces_success_error_and_narrower_union() {
    let src = "error A\nerror B\nstruct Holder:\n\tvalue: (A | B)!int\n\
               fn wide() -> (A | B)!int:\n\treturn 0\n\
               fn narrow() -> A!int:\n\treturn 3\n\
               fn main():\n\tmut h = Holder{value=wide()}\n\th.value = 2\n\th.value = A\n\th.value = narrow()\n";
    let (tirs, pool) = run(src).expect("union field assignment should coerce");
    let main = tir_named(&tirs, &pool, "main");
    for index in 1..=3 {
        let value = main.field_assign_view(stmt_at(main, index)).value;
        assert_eq!(main.inst(value).tag, TirTag::ErrorUnionCoerce);
        assert_eq!(main.inst(value).ty, main.inst(stmt_at(main, index)).ty);
    }
}

#[test]
fn call_arguments_coerce_success_error_set_and_narrower_union() {
    let src = "error A\nerror B\n\
               fn take(value: (A | B)!int) -> int:\n\treturn 0\n\
               fn narrow() -> A!int:\n\treturn 3\n\
               fn wide() -> (A | B)!int:\n\treturn B\n\
               fn from_set() -> int:\n\treturn wide() catch as err:\n\t\t_: take(err)\n\
               fn main():\n\ttake(1)\n\ttake(A)\n\ttake(narrow())\n";
    let (tirs, pool) = run(src).expect("union call arguments should coerce");
    let take = pool.find_str("take").expect("take is interned");
    let calls: Vec<_> = tirs
        .iter()
        .flat_map(|tir| {
            (1..tir.instructions.len() as u32)
                .map(TirRef::from_raw)
                .filter(|&r| tir.inst(r).tag == TirTag::Call && tir.call_view(r).name == take)
                .map(move |r| (tir, r))
                .collect::<Vec<_>>()
        })
        .collect();
    assert_eq!(calls.len(), 4);
    for (tir, call) in calls {
        let arg = tir.call_view(call).args.at(0);
        assert_eq!(tir.inst(arg).tag, TirTag::ErrorUnionCoerce);
        assert_eq!(pool.kind(tir.inst(arg).ty), TypeKind::ErrorUnion);
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

#[test]
fn try_unwraps_success_and_early_returns_through_destination_coercion() {
    let src = r#"error A
error B
fn inner() -> A!str:
	return "ok"
fn outer() -> (A | B)!str:
	value = try inner()
	return value
"#;
    let (tirs, pool) = run(src).expect("subset propagation should succeed");
    let outer = tir_named(&tirs, &pool, "outer");
    assert_eq!(
        outer
            .instructions
            .iter()
            .filter(|inst| inst.tag == TirTag::Call)
            .count(),
        1,
        "the try operand must be evaluated once"
    );
    let init = outer.var_decl_view(stmt_at(outer, 0)).initializer;
    assert_eq!(outer.inst(init).tag, TirTag::Match);
    assert_eq!(outer.inst(init).ty, pool.str_());
    let view = outer.match_view(init);
    let arms: Vec<_> = view.arms().collect();
    assert_eq!(arms.len(), 2);
    let ryo_core::tir::MatchBodyView::Expr(_) = arms[0].body else {
        panic!("Ok must yield the extracted success value")
    };
    let err_stmts: Vec<_> = arms[1].body.stmts().collect();
    assert_eq!(err_stmts.len(), 1);
    assert_eq!(outer.inst(err_stmts[0]).tag, TirTag::Return);
    let TirData::UnOp(coercion) = outer.inst(err_stmts[0]).data else {
        panic!("the error arm must return the coerced error")
    };
    assert_eq!(outer.inst(coercion).tag, TirTag::ErrorUnionCoerce);
    assert_eq!(outer.inst(coercion).ty, outer.return_type);
    let TirData::UnOp(error) = outer.inst(coercion).data else {
        panic!("union coercion must consume the active error")
    };
    assert_eq!(pool.kind(outer.inst(error).ty), TypeKind::DeclaredError);
}

#[test]
fn try_rejects_a_non_union_operand() {
    let src = r#"error A
fn f() -> A!int:
	value = try 1
	return value
"#;
    let (_tirs, diags, _pool) = run_with_errors(src);
    assert_eq!(diags.len(), 1, "unexpected diagnostic cascade: {diags:?}");
    assert_eq!(diags[0].code, DiagCode::TypeMismatch);
    assert!(
        diags[0].message.contains("try") && diags[0].message.contains("error union operand"),
        "{diags:?}"
    );
}

#[test]
fn try_rejects_void_and_non_union_enclosing_returns() {
    let cases = [
        r#"error A
fn inner() -> A!int:
	return 1
fn main():
	value = try inner()
"#,
        r#"error A
fn inner() -> A!int:
	return 1
fn f() -> int:
	value = try inner()
	return value
"#,
    ];
    for src in cases {
        let (_tirs, diags, _pool) = run_with_errors(src);
        assert_eq!(diags.len(), 1, "{src:?}: {diags:?}");
        assert_eq!(diags[0].code, DiagCode::TypeMismatch);
        assert!(
            diags[0].message.contains("try")
                && diags[0].message.contains("enclosing function")
                && diags[0].message.contains("error union"),
            "{src:?}: {diags:?}"
        );
    }
}

#[test]
fn try_lists_every_missing_destination_error_in_order() {
    let src = r#"error A
error B
error C
fn inner() -> (A | B | C)!int:
	return 1
fn outer() -> A!int:
	value = try inner()
	return value
"#;
    let (_tirs, diags, _pool) = run_with_errors(src);
    assert_eq!(diags.len(), 1, "unexpected diagnostic cascade: {diags:?}");
    assert_eq!(diags[0].code, DiagCode::TypeMismatch);
    assert!(
        diags[0].message.contains("missing error members 'B', 'C'"),
        "{diags:?}"
    );
}

#[test]
fn try_rejects_nominal_error_conversion() {
    let src = r#"error A
error B
fn inner() -> B!int:
	return 1
fn outer() -> A!int:
	value = try inner()
	return value
"#;
    let (_tirs, diags, _pool) = run_with_errors(src);
    assert_eq!(diags.len(), 1, "unexpected diagnostic cascade: {diags:?}");
    assert_eq!(diags[0].code, DiagCode::TypeMismatch);
    assert!(
        diags[0].message.contains("missing error member 'B'"),
        "{diags:?}"
    );
}

#[test]
fn bare_try_statement_accepts_void_success() {
    let src = r#"error A
fn inner() -> A!void:
	return
fn outer() -> A!void:
	try inner()
	return
"#;
    let (tirs, pool) = run(src).expect("a bare try statement should allow void success");
    let outer = tir_named(&tirs, &pool, "outer");
    let stmt = stmt_at(outer, 0);
    assert_eq!(outer.inst(stmt).tag, TirTag::ExprStmt);
    let TirData::UnOp(init) = outer.inst(stmt).data else {
        panic!("expression statement must wrap try")
    };
    assert_eq!(outer.inst(init).tag, TirTag::Match);
    assert_eq!(outer.inst(init).ty, pool.void());
}

#[test]
fn catch_forms_return_the_operand_success_type() {
    let direct = r#"error NotFound
fn find() -> NotFound!str:
	return NotFound
fn main():
	value = find() catch as err:
		NotFound: "unknown"
	print(value)
"#;
    let bare = direct.replace("catch as err", "catch");
    let nested = r#"error NotFound
fn find() -> NotFound!str:
	return NotFound
fn main():
	value = find() catch as err:
		match err:
			NotFound: "unknown"
	print(value)
"#;

    for src in [direct, &bare, nested] {
        let (tirs, pool) = run(src).expect("catch should return its success type");
        let main = tir_named(&tirs, &pool, "main");
        let init = main.var_decl_view(stmt_at(main, 0)).initializer;
        assert_eq!(main.inst(init).tag, TirTag::Match, "{src}");
        assert_eq!(main.inst(init).ty, pool.str_(), "{src}");
    }
}

#[test]
fn catch_rejects_non_exhaustive_direct_arms() {
    let src = r#"error A
error B
fn find() -> (A | B)!int:
	return A
fn main():
	value = find() catch:
		A: 1
	print(value)
"#;
    let (_tirs, diags, _pool) = run_with_errors(src);
    assert!(any_code(&diags, DiagCode::NonExhaustiveMatch), "{diags:?}");
}

#[test]
fn catch_rejects_handler_type_mismatch() {
    let src = r#"error A
fn find() -> A!int:
	return A
fn main():
	value = find() catch as err:
		"bad"
	print(value)
"#;
    let (_tirs, diags, _pool) = run_with_errors(src);
    assert!(any_code(&diags, DiagCode::ArmTypeMismatch), "{diags:?}");
}

#[test]
fn catch_requires_an_error_union_operand() {
    let src = r#"fn main():
	value = 1 catch as err:
		_: 0
	print(value)
"#;
    let (_tirs, diags, _pool) = run_with_errors(src);
    assert_eq!(diags.len(), 1, "unexpected diagnostic cascade: {diags:?}");
    assert_eq!(diags[0].code, DiagCode::TypeMismatch, "{diags:?}");
    assert!(
        diags[0].message.contains("catch") && diags[0].message.contains("error union"),
        "{diags:?}"
    );
}

#[test]
fn catch_allows_a_diverging_handler() {
    let src = r#"error A
fn find() -> A!int:
	return 7
fn main():
	value = find() catch as err:
		panic("failure")
	print(value)
"#;
    let (tirs, pool) = run(src).expect("a diverging handler should join with Ok(int)");
    let main = tir_named(&tirs, &pool, "main");
    let init = main.var_decl_view(stmt_at(main, 0)).initializer;
    assert_eq!(main.inst(init).tag, TirTag::Match);
    assert_eq!(main.inst(init).ty, pool.int());
}

#[test]
fn catch_warns_once_at_an_unused_shared_binding() {
    let src = r#"error A(str)
fn find() -> A!int:
	return A("failure")
fn main():
	value = find() catch as err:
		A(_): 0
	print(value)
"#;
    let (_tirs, diags, _pool) = run_with_errors(src);
    let warnings: Vec<_> = diags
        .iter()
        .filter(|diag| diag.code == DiagCode::DeadStore)
        .collect();
    assert_eq!(warnings.len(), 1, "{diags:?}");
    assert!(warnings[0].message.contains("err"), "{warnings:?}");
    let binding_start = src.find("as err").expect("catch binding") + "as ".len();
    assert_eq!(warnings[0].span.start, binding_start, "{warnings:?}");
    assert_eq!(warnings[0].span.end, binding_start + "err".len());
}

#[test]
fn catch_binding_use_in_any_arm_suppresses_the_warning() {
    let src = r#"error A
error B
fn find() -> (A | B)!int:
	return A
fn main():
	value = find() catch as err:
		A: match err:
			A: 1
			_: 2
		_: 3
	print(value)
"#;
    let (_tirs, diags, _pool) = run_with_errors(src);
    assert!(diags.is_empty(), "{diags:?}");
}

#[test]
fn catch_shared_binding_is_visible_in_a_wildcard_arm() {
    let src = r#"error A
error B
fn find() -> (A | B)!int:
	return B
fn main():
	value = find() catch as err:
		A: 1
		_: match err:
			A: 2
			B: 3
	print(value)
"#;
    let (_tirs, diags, _pool) = run_with_errors(src);
    assert!(diags.is_empty(), "{diags:?}");
}

#[test]
fn bare_catch_has_no_binding_warning() {
    let src = r#"error A(str)
fn find() -> A!int:
	return A("failure")
fn main():
	value = find() catch:
		A(_): 0
	print(value)
"#;
    let (_tirs, diags, _pool) = run_with_errors(src);
    assert!(diags.is_empty(), "{diags:?}");
}
