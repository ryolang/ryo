use super::tests::*;
use super::*;
use ryo_core::tir::TirTag;

#[test]
fn named_struct_fields_coerce_success_error_and_narrower_union() {
    let src = r#"error A
error B
struct Holder:
	value: (A | B)!int
fn narrow() -> A!int:
	return 3
fn wide() -> (A | B)!int:
	return 4
fn main():
	success = Holder{value=1}
	failure = Holder{value=A}
	narrowed = Holder{value=narrow()}
	exact = Holder{value=wide()}
"#;
    let (tirs, pool) = run(src).expect("named struct fields should coerce");
    let main = tir_named(&tirs, &pool, "main");
    for index in 0..3 {
        let decl = main.var_decl_view(stmt_at(main, index));
        let literal = main.struct_lit_view(decl.initializer);
        let (_, value) = literal.fields().next().expect("one field");
        assert_eq!(main.inst(value).tag, TirTag::ErrorUnionCoerce);
        assert_eq!(pool.display(main.inst(value).ty).to_string(), "(A | B)!int");
    }
    let decl = main.var_decl_view(stmt_at(main, 3));
    let literal = main.struct_lit_view(decl.initializer);
    let (_, value) = literal.fields().next().expect("one field");
    assert_eq!(main.inst(value).tag, TirTag::Call);
}

#[test]
fn anonymous_struct_fields_coerce_success_error_and_narrower_union() {
    let src = r#"error A
error B
fn narrow() -> A!int:
	return 3
fn wide() -> (A | B)!int:
	return 4
fn main():
	success: {value: (A | B)!int} = {value=1}
	failure: {value: (A | B)!int} = {value=A}
	narrowed: {value: (A | B)!int} = {value=narrow()}
	exact: {value: (A | B)!int} = {value=wide()}
"#;
    let (tirs, pool) = run(src).expect("anonymous struct fields should coerce");
    let main = tir_named(&tirs, &pool, "main");
    for index in 0..3 {
        let decl = main.var_decl_view(stmt_at(main, index));
        let literal = main.struct_lit_view(decl.initializer);
        let (_, value) = literal.fields().next().expect("one field");
        assert_eq!(main.inst(value).tag, TirTag::ErrorUnionCoerce);
        assert_eq!(pool.display(main.inst(value).ty).to_string(), "(A | B)!int");
    }
    let decl = main.var_decl_view(stmt_at(main, 3));
    let literal = main.struct_lit_view(decl.initializer);
    let (_, value) = literal.fields().next().expect("one field");
    assert_eq!(main.inst(value).tag, TirTag::Call);
}

#[test]
fn nested_struct_fields_coerce_error_unions() {
    let src = r#"error A
error B
struct Holder:
	value: (A | B)!int
struct AnonymousNested:
	value: {value: (A | B)!int}
struct NamedNested:
	value: Holder
fn main():
	anonymous = AnonymousNested{value={value=1}}
	named = NamedNested{value=Holder{value=A}}
"#;
    let (tirs, pool) = run(src).expect("nested struct fields should coerce");
    let main = tir_named(&tirs, &pool, "main");
    for index in 0..2 {
        let decl = main.var_decl_view(stmt_at(main, index));
        let outer = main.struct_lit_view(decl.initializer);
        let (_, inner) = outer.fields().next().expect("outer field");
        let inner = main.struct_lit_view(inner);
        let (_, value) = inner.fields().next().expect("inner field");
        assert_eq!(main.inst(value).tag, TirTag::ErrorUnionCoerce);
    }
}

#[test]
fn struct_literal_union_narrowing_is_rejected() {
    let cases = [
        r#"error A
error B
struct Holder:
	value: A!int
fn wide() -> (A | B)!int:
	return 1
fn main():
	holder = Holder{value=wide()}
"#,
        r#"error A
error B
fn wide() -> (A | B)!int:
	return 1
fn main():
	holder: {value: A!int} = {value=wide()}
"#,
    ];
    for src in cases {
        let (_tirs, diags, _pool) = run_with_errors(src);
        assert_eq!(diags.len(), 1, "unexpected diagnostics: {diags:?}");
        assert_eq!(diags[0].code, DiagCode::TypeMismatch);
    }
}

#[test]
fn anonymous_struct_fields_use_expected_type_positions() {
    let src = r#"error A
error B
fn narrow() -> A!int:
	return 3
fn take(holder: {value: (A | B)!int}):
	return
fn make() -> {value: (A | B)!int}:
	return {value=narrow()}
fn main():
	take({value=1})
	take({value=A})
	mut assigned: {value: (A | B)!int} = {value=0}
	assigned = {value=narrow()}
	mut nested: {inner: {value: (A | B)!int}} = {inner={value=0}}
	nested.inner = {value=A}
"#;
    run(src).expect("anonymous literals should use call, return, and assignment targets");
}
