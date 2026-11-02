use super::tests::*;
use super::*;
use ryo_core::tir::TirTag;
use ryo_core::types::TypeKind;

fn assert_final_type(tirs: &[Tir], pool: &InternPool, name: &str, expected: &str) -> TypeId {
    let return_type = tir_named(tirs, pool, name).return_type;
    assert_eq!(pool.kind(return_type), TypeKind::ErrorUnion, "{name}");
    assert_eq!(pool.display(return_type).to_string(), expected, "{name}");
    return_type
}

fn assert_no_placeholders(tirs: &[Tir], pool: &InternPool) {
    for tir in tirs {
        assert_ne!(pool.kind(tir.return_type), TypeKind::InferredErrorUnion);
        for inst in tir.instructions.iter().skip(1) {
            assert_ne!(
                pool.kind(inst.ty),
                TypeKind::InferredErrorUnion,
                "unresolved placeholder in {}",
                pool.str(tir.name)
            );
        }
    }
}

fn assert_coercions_patched(tirs: &[Tir], pool: &InternPool, name: &str) {
    let tir = tir_named(tirs, pool, name);
    let coercions: Vec<_> = tir
        .instructions
        .iter()
        .filter(|inst| inst.tag == TirTag::ErrorUnionCoerce)
        .collect();
    assert!(
        !coercions.is_empty(),
        "expected deferred coercions in {name}"
    );
    for coercion in coercions {
        assert_eq!(coercion.ty, tir.return_type, "unpatched coercion in {name}");
    }
}

fn assert_inferred_cycle_rejected(src: &str) {
    let (_tirs, diags, _pool) = run_with_errors(src);
    assert_eq!(
        count_code(&diags, DiagCode::CycleInResolution),
        1,
        "expected one cycle diagnostic: {diags:?}"
    );
    assert!(
        diags
            .iter()
            .any(|diag| diag.message.contains("explicit error union")),
        "diagnostic must recommend explicit unions: {diags:?}"
    );
    assert_eq!(diags.len(), 1, "unexpected cascading errors: {diags:?}");
}

#[test]
fn direct_error_returns_form_a_concrete_union() {
    let src = r#"error A
error B
fn choose(which: int) -> !str:
	if which == 1:
		return A
	if which == 2:
		return B
	return "ok"
"#;
    let (tirs, pool) = run(src).expect("direct errors should infer");
    let return_type = assert_final_type(&tirs, &pool, "choose", "(A | B)!str");
    assert_eq!(
        pool.error_set_view(pool.error_union_view(return_type).error_set)
            .members
            .len(),
        2
    );
    assert_coercions_patched(&tirs, &pool, "choose");
    assert_no_placeholders(&tirs, &pool);
}

#[test]
fn returned_concrete_union_contributes_all_members() {
    let src = r#"error A
error B
fn concrete() -> (A | B)!str:
	panic("stop")
fn inferred() -> !str:
	return concrete()
"#;
    let (tirs, pool) = run(src).expect("returned union should infer");
    assert_final_type(&tirs, &pool, "inferred", "(A | B)!str");
    assert_coercions_patched(&tirs, &pool, "inferred");
    assert_no_placeholders(&tirs, &pool);
}

#[test]
fn try_propagates_inferred_dependencies_and_patches_early_return() {
    let src = r#"error A
fn inner() -> !str:
	return A
fn outer() -> !str:
	value = try inner()
	return value
"#;
    let (tirs, pool) = run(src).expect("try should infer its dependency");
    assert_final_type(&tirs, &pool, "inner", "A!str");
    assert_final_type(&tirs, &pool, "outer", "A!str");
    assert_coercions_patched(&tirs, &pool, "outer");
    assert_no_placeholders(&tirs, &pool);
}

#[test]
fn forward_call_finalizes_callee_before_caller_signature_is_cloned() {
    let src = r#"error A
fn outer() -> !str:
	return inner()
fn inner() -> !str:
	return A
"#;
    let (tirs, pool) = run(src).expect("forward inference should resolve");
    assert_final_type(&tirs, &pool, "outer", "A!str");
    assert_final_type(&tirs, &pool, "inner", "A!str");
    assert_no_placeholders(&tirs, &pool);
}

#[test]
fn inference_resolves_chains() {
    let src = r#"error A
fn first() -> !str:
	return A
fn second() -> !str:
	return first()
fn third() -> !str:
	return second()
"#;
    let (tirs, pool) = run(src).expect("inference chain should resolve");
    for name in ["first", "second", "third"] {
        assert_final_type(&tirs, &pool, name, "A!str");
    }
    assert_no_placeholders(&tirs, &pool);
}

#[test]
fn inference_reuses_completed_diamond_dependencies() {
    let src = r#"error A
fn leaf() -> !str:
	return A
fn left() -> !str:
	return leaf()
fn right() -> !str:
	return leaf()
fn join(use_left: bool) -> !str:
	if use_left:
		return left()
	return right()
"#;
    let (tirs, pool) = run(src).expect("diamond inference should resolve");
    for name in ["leaf", "left", "right", "join"] {
        assert_final_type(&tirs, &pool, name, "A!str");
    }
    assert_no_placeholders(&tirs, &pool);
}

#[test]
fn direct_inferred_recursion_requires_an_explicit_union() {
    assert_inferred_cycle_rejected(
        "fn recurse(n: int) -> !int:\n\tif n == 0:\n\t\treturn 0\n\treturn recurse(n - 1)\n",
    );
}

#[test]
fn mutual_inferred_recursion_requires_explicit_unions() {
    assert_inferred_cycle_rejected(
        "fn a() -> !int:\n\treturn b()\n\nfn b() -> !int:\n\treturn a()\n",
    );
}

#[test]
fn mixed_explicit_and_inferred_cycle_gets_targeted_diagnostic() {
    assert_inferred_cycle_rejected(
        "fn a() -> !int:\n\treturn b()\n\nfn b() -> int:\n\treturn a()\n",
    );
}

#[test]
fn explicit_error_union_recursion_remains_legal() {
    let src = r#"error A
fn recurse(n: int) -> A!int:
	if n == 0:
		return 0
	return recurse(n - 1)
"#;
    let (tirs, pool) = run(src).expect("explicit-only recursion should remain legal");
    assert_final_type(&tirs, &pool, "recurse", "A!int");
}

#[test]
fn empty_inferred_set_has_only_a_constructible_success_alternative() {
    let src = "fn succeed() -> !int:\n\treturn 7\n";
    let (tirs, pool) = run(src).expect("empty inferred set should be accepted");
    let return_type = assert_final_type(&tirs, &pool, "succeed", "!int");
    let view = pool.error_union_view(return_type);
    assert_eq!(view.success, pool.int());
    assert!(pool.error_set_view(view.error_set).members.is_empty());
    assert!(pool.enum_view(view.error_set).is_empty());
    let variants: Vec<_> = pool.enum_view(return_type).variants().collect();
    assert_eq!(pool.str(variants[0].name), "Ok");
    assert_eq!(variants[0].fields[0].ty, pool.int());
    assert_eq!(pool.str(variants[1].name), "Err");
    assert!(pool.enum_view(variants[1].fields[0].ty).is_empty());
    assert_coercions_patched(&tirs, &pool, "succeed");
}
