//! Fresh-depth audit checklist:
//! - `sema/expr/mod.rs`, `methods.rs`, `operators.rs`: nested operands.
//! - `sema/call.rs`, `errors.rs`, `match_.rs`: nested operands and scoped bodies.
//! - `sema/stmt.rs`: statement roots and nested expression statements.
//! - `codegen/expr/mod.rs`, `fat_slot.rs`, `builtins.rs`: nested value evaluation.
//! - `codegen/structs.rs`, `match_.rs`, `control.rs`: aggregates and Flow barriers.
//! - `codegen/mod.rs`, `decls.rs`: statement/declaration roots.

use super::super::tests::{RunOk, run};
use super::walker::with_expr_limit;
use ryo_core::diag::Diag;
use std::process::Command;

const DEEP_REPRO_CHILD: &str = "RYO_DEEP_EXPR_REPRO_CHILD";

struct DeepExpr;

impl DeepExpr {
    fn program(expr: String) -> String {
        format!("fn id(x: int) -> int:\n\treturn x\n\nfn main():\n\tprint({expr})\n")
    }

    fn unary_mediated(links: usize) -> String {
        let mut expr = String::new();
        for _ in 0..links {
            expr.push_str("1+(-(");
        }
        expr.push('1');
        for _ in 0..links {
            expr.push_str("))");
        }
        Self::program(expr)
    }

    fn call_mediated(links: usize) -> String {
        let mut expr = String::new();
        for _ in 0..links {
            expr.push_str("1+id(");
        }
        expr.push('1');
        for _ in 0..links {
            expr.push(')');
        }
        Self::program(expr)
    }

    fn nested_calls(links: usize) -> String {
        let mut expr = String::new();
        for _ in 0..links {
            expr.push_str("id(");
        }
        expr.push('1');
        for _ in 0..links {
            expr.push(')');
        }
        Self::program(expr)
    }

    fn identity_expr(expr: &str, links: usize) -> String {
        let mut wrapped = String::new();
        for _ in 0..links {
            wrapped.push_str("id(");
        }
        wrapped.push_str(expr);
        for _ in 0..links {
            wrapped.push(')');
        }
        wrapped
    }

    fn identity_calls(expr: &str, links: usize) -> String {
        Self::program(Self::identity_expr(expr, links))
    }

    fn nested_matches(depth: usize) -> String {
        let mut src = String::from("fn main():\n");
        for level in 0..=depth {
            src.push_str(&"\t".repeat(2 * level + 1));
            src.push_str("match 1:\n");
            src.push_str(&"\t".repeat(2 * level + 2));
            src.push_str("1:\n");
        }
        src.push_str(&"\t".repeat(2 * depth + 3));
        src.push_str("print(0)\n");
        for level in (0..=depth).rev() {
            src.push_str(&"\t".repeat(2 * level + 2));
            src.push_str("_:\n");
            src.push_str(&"\t".repeat(2 * level + 3));
            src.push_str("print(0)\n");
        }
        src
    }

    fn nested_catches(depth: usize) -> String {
        let mut src = String::from("error E\nfn find() -> E!int:\n\treturn E\nfn main():\n");
        for level in 0..=depth {
            src.push_str(&"\t".repeat(level + 1));
            if level == 0 {
                src.push_str("value = find() catch as err:\n");
            } else {
                src.push_str("E: find() catch as err:\n");
            }
        }
        src.push_str(&"\t".repeat(depth + 2));
        src.push_str("E: 0\n");
        src.push_str("\tprint(value)\n");
        src
    }

    fn scalar_shapes(depth: usize) -> Vec<String> {
        if depth == 0 {
            return vec!["1".to_string()];
        }
        let mut shapes = Vec::new();
        for inner in Self::scalar_shapes(depth - 1) {
            shapes.push(format!("-({inner})"));
            shapes.push(format!("({inner})+1"));
            shapes.push(format!("1+({inner})"));
            shapes.push(format!("({inner})+(1+1)"));
        }
        shapes
    }

    fn diagnostic_cases() -> Vec<(&'static str, String)> {
        vec![
            ("division_by_zero", Self::program("1/(1-1)".to_string())),
            (
                "overflow",
                Self::program("9223372036854775807+1".to_string()),
            ),
            ("never_rhs", Self::program("1+panic(\"boom\")".to_string())),
            ("never_lhs", Self::program("panic(\"boom\")+1".to_string())),
        ]
    }

    fn small_cases() -> Vec<(&'static str, String)> {
        vec![
            ("operators", Self::program("1+2*3-4".to_string())),
            (
                "invalid_operator",
                Self::program("1+true".to_string()),
            ),
            ("calls", Self::program("id(1+id(2))".to_string())),
            (
                "invalid_call",
                Self::program("missing(1+true)".to_string()),
            ),
            (
                "reserved_call_diagnostic_order",
                Self::program("range(1+true)".to_string()),
            ),
            (
                "call_arity_and_argument_diagnostic_order",
                Self::program("id(1+true, 2)".to_string()),
            ),
            (
                "expected_struct_argument",
                "struct Point:\n\tx: int\n\ty: int\nfn norm(p: Point) -> int:\n\treturn p.x\nfn main():\n\tprint(norm(Point{x=1, y=2}))\n".to_string(),
            ),
            (
                "materialization_call",
                "fn main():\n\ts = \"abc\"\n\tprint(str(s[0:1]))\n".to_string(),
            ),
            (
                "anonymous_struct",
                "fn main():\n\tp = {x=1, y=2}\n\tprint(p.x)\n".to_string(),
            ),
            (
                "invalid_enum_payload",
                "enum Shape:\n\tCircle(int)\nfn main():\n\ts = Shape.Circle(\"bad\")\n".to_string(),
            ),
            (
                "invalid_error_payload",
                "error E(int)\nfn main():\n\te = E(\"bad\")\n".to_string(),
            ),
            (
                "invalid_field",
                "struct Point:\n\tx: int\nfn main():\n\tp = Point{x=1}\n\tprint(p.y)\n".to_string(),
            ),
            (
                "methods",
                "fn main():\n\tprint(\"abc\".len())\n".to_string(),
            ),
            (
                "invalid_method",
                "fn main():\n\tprint(1.len())\n".to_string(),
            ),
            (
                "slice_and_index",
                "fn main():\n\tb = b\"abc\"\n\tprint(b[0])\n\tprint(b[0:1])\n".to_string(),
            ),
            (
                "invalid_index",
                "fn main():\n\tprint(1[1+true])\n".to_string(),
            ),
            (
                "invalid_slice",
                "fn main():\n\tprint(1[1+true:2])\n".to_string(),
            ),
            (
                "lowercase_enum_method",
                "enum color:\n\tRed\nfn main():\n\tprint(color.red(1+true))\n".to_string(),
            ),
            (
                "struct_name_method",
                "struct Point:\n\tx: int\nfn main():\n\tprint(Point.make(1+true))\n".to_string(),
            ),
            (
                "unit_variant_field",
                "enum Color:\n\tRed\nfn main():\n\tprint(Color.Red)\n".to_string(),
            ),
            (
                "positional_named_enum",
                "enum Shape:\n\tRectangle(width: int)\nfn main():\n\ts = Shape.Rectangle(1+true)\n".to_string(),
            ),
            (
                "unit_error_with_payload",
                "error E\nfn main():\n\te = E(1+true)\n".to_string(),
            ),
            (
                "borrow",
                "fn inc(inout x: int):\n\tx += 1\nfn main():\n\tmut n = 1\n\tinc(&n)\n".to_string(),
            ),
            (
                "invalid_borrow",
                "fn main():\n\tmut n = 1\n\tprint(&n)\n".to_string(),
            ),
            (
                "struct_and_field",
                "struct Point:\n\tx: int\n\ty: int\nfn main():\n\tp = Point{x=1, y=2}\n\tprint(p.x)\n".to_string(),
            ),
            (
                "invalid_struct",
                "struct Point:\n\tx: int\nfn main():\n\tp = Point{z=1}\n".to_string(),
            ),
            (
                "enum",
                "enum Shape:\n\tCircle(int)\nfn main():\n\ts = Shape.Circle(1)\n".to_string(),
            ),
            (
                "error_construct",
                "error E(int)\nfn main():\n\te = E(1)\n".to_string(),
            ),
            (
                "match",
                "fn main():\n\tvalue = match 1:\n\t\t1: 10\n\t\t_: 20\n\tprint(value)\n".to_string(),
            ),
            (
                "statement_match_scopes",
                "enum Opt:\n\tSome(value: int)\n\tNone\nfn f(o: Opt) -> int:\n\tmatch o:\n\t\tOpt.Some(v):\n\t\t\tw = v + 1\n\t\t\treturn w\n\t\tOpt.None:\n\t\t\treturn 0\n".to_string(),
            ),
            (
                "diverging_match",
                "fn f(b: bool) -> int:\n\tmatch b:\n\t\ttrue: panic(\"a\")\n\t\tfalse: return 7\nfn main():\n\tprint(f(false))\n".to_string(),
            ),
            (
                "invalid_match_join",
                "enum Opt:\n\tSome(value: int)\n\tNone\nfn main():\n\to = Opt.Some{value=5}\n\tx = match o:\n\t\tOpt.Some(v): v\n\t\tOpt.None: \"s\"\n".to_string(),
            ),
            (
                "catch",
                "error E\nfn find() -> E!int:\n\treturn E\nfn main():\n\tvalue = find() catch as err:\n\t\tE: 0\n\tprint(value)\n".to_string(),
            ),
            (
                "try",
                "error E\nfn inner() -> E!int:\n\treturn 1\nfn outer() -> E!int:\n\tvalue = try inner()\n\treturn value\n".to_string(),
            ),
            (
                "assert_desugar",
                "fn main():\n\tassert(1+1 == 2)\n".to_string(),
            ),
        ]
    }
}

fn run_with_expr_limit(src: &str, limit: u32) -> Result<RunOk, Vec<Diag>> {
    with_expr_limit(limit, || run(src))
}

fn snapshot(result: Result<RunOk, Vec<Diag>>) -> String {
    match result {
        Ok((tirs, _pool)) => tirs
            .iter()
            .map(|tir| {
                format!(
                    "{:?}\n{:?}\n{:?}\n{:?}",
                    tir.instructions, tir.extra, tir.body, tir.spans
                )
            })
            .collect(),
        Err(diags) => format!("{diags:?}"),
    }
}

fn run_deep_repro(test_name: &str, src: String) {
    if std::env::var_os(DEEP_REPRO_CHILD).is_some() {
        run(&src).expect("deep expression should analyze");
        return;
    }
    let output = Command::new(std::env::current_exe().expect("current test executable"))
        .args([test_name, "--exact", "--nocapture"])
        .env(DEEP_REPRO_CHILD, "1")
        .output()
        .expect("spawn deep expression child");
    assert!(
        output.status.success(),
        "deep expression child failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn unary_mediated_rhs_chain_analyzes_without_overflow() {
    run_deep_repro(
        "sema::expr::walker_tests::unary_mediated_rhs_chain_analyzes_without_overflow",
        DeepExpr::unary_mediated(50_000),
    );
}

#[test]
fn call_mediated_rhs_chain_analyzes_without_overflow() {
    run_deep_repro(
        "sema::expr::walker_tests::call_mediated_rhs_chain_analyzes_without_overflow",
        DeepExpr::call_mediated(50_000),
    );
}

#[test]
fn nested_call_chain_analyzes_without_overflow() {
    run_deep_repro(
        "sema::expr::walker_tests::nested_call_chain_analyzes_without_overflow",
        DeepExpr::nested_calls(50_000),
    );
}

#[test]
fn nested_match_expressions_analyze_without_overflow() {
    run_deep_repro(
        "sema::expr::walker_tests::nested_match_expressions_analyze_without_overflow",
        DeepExpr::nested_matches(100),
    );
}

#[test]
fn nested_catch_expressions_analyze_without_overflow() {
    run_deep_repro(
        "sema::expr::walker_tests::nested_catch_expressions_analyze_without_overflow",
        DeepExpr::nested_catches(100),
    );
}

#[test]
fn scalar_trees_match_at_every_depth_limit() {
    for depth in 0..=4 {
        for expr in DeepExpr::scalar_shapes(depth) {
            let src = DeepExpr::program(expr.clone());
            let baseline = snapshot(run(&src));
            for limit in [0, 1, 64] {
                assert_eq!(
                    snapshot(run_with_expr_limit(&src, limit)),
                    baseline,
                    "{expr} differed at expression depth limit {limit}"
                );
            }
        }
    }
}

#[test]
fn scalar_diagnostics_match_at_every_depth_limit() {
    for (name, src) in DeepExpr::diagnostic_cases() {
        let baseline = snapshot(run(&src));
        for limit in [0, 1, 64] {
            assert_eq!(
                snapshot(run_with_expr_limit(&src, limit)),
                baseline,
                "{name} differed at expression depth limit {limit}"
            );
        }
    }
}

#[test]
fn small_expression_families_match_at_every_depth_limit() {
    for (name, src) in DeepExpr::small_cases() {
        let baseline = snapshot(run(&src));
        for limit in [0, 1, 64] {
            assert_eq!(
                snapshot(run_with_expr_limit(&src, limit)),
                baseline,
                "{name} differed at expression depth limit {limit}"
            );
        }
    }
}
#[test]
fn resolution_broken_nested_match_arm_matches_at_every_depth_limit() {
    let unary = format!("{}0{}", "-(".repeat(70), ")".repeat(70));
    let src = format!(
        "fn id(x: int) -> int:\n\treturn x\n\nfn main():\n\tmatch 0:\n\t\t0:\n\t\t\tvalue = match 1:\n\t\t\t\ttrue: {unary}\n\t\t\t\t_: 0\n\t\t\tprint(value)\n\t\t_:\n\t\t\tprint(0)\n",
    );
    let baseline = snapshot(run(&src));
    for limit in [0, 1, 64] {
        assert_eq!(snapshot(run_with_expr_limit(&src, limit)), baseline);
    }
}

#[test]
fn unknown_struct_suppresses_field_diagnostics_at_every_depth_limit() {
    let src = DeepExpr::identity_calls("1+(Missing{x=missing()})", 70);
    for limit in [0, 1, 64] {
        let diags = run_with_expr_limit(&src, limit).expect_err("struct must be unknown");
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].code, ryo_core::diag::DiagCode::UnknownType);
    }
}

#[test]
fn undefined_function_diagnostic_follows_argument_diagnostics() {
    let src = DeepExpr::identity_calls("missing(1+true)", 70);
    for limit in [0, 1, 64] {
        let diags = run_with_expr_limit(&src, limit).expect_err("call must be invalid");
        assert_eq!(diags.len(), 2, "{diags:?}");
        assert_eq!(diags[0].code, ryo_core::diag::DiagCode::TypeMismatch);
        assert_eq!(diags[1].code, ryo_core::diag::DiagCode::UndefinedFunction);
    }
}

#[test]
fn direct_catch_marker_is_preserved_at_every_depth_limit() {
    let src = r#"error A(str)
fn id(move x: str) -> str:
	return x
fn describe(err: A, move message: str) -> str:
	return err.message()
fn find() -> A!str:
	return A("message")
fn main():
	value = find() catch as err:
		A(message): describe(err, message)
	print(value)
"#;
    let src = src.replace(
        "A(message): describe(err, message)",
        &format!(
            "A(message): {}describe(err, message){}",
            "id(".repeat(70),
            ")".repeat(70)
        ),
    );
    for limit in [0, 1, 64] {
        let (tirs, pool) = run_with_expr_limit(&src, limit).expect("direct catch analyzes");
        let main = tirs
            .iter()
            .find(|tir| pool.str(tir.name) == "main")
            .expect("main TIR");
        assert_eq!(main.direct_catch_matches.len(), 1, "limit {limit}");
    }
}

fn expected_type_struct_chain(depth: usize) -> String {
    let mut src = String::from("struct S0:\n\tvalue: int\n");
    for index in 1..=depth {
        src.push_str(&format!("struct S{index}:\n\tvalue: S{}\n", index - 1));
    }
    src.push_str(&format!(
        "fn make() -> S{depth}:\n\treturn S{depth}{{value="
    ));
    for index in (0..depth).rev() {
        src.push_str(&format!("S{index}{{value="));
    }
    src.push('0');
    for _ in 0..=depth {
        src.push('}');
    }
    src.push_str("\nfn main():\n\tvalue = make()\n");
    src
}

#[test]
fn expected_type_struct_chain_analyzes_without_overflow() {
    run_deep_repro(
        "sema::expr::walker_tests::expected_type_struct_chain_analyzes_without_overflow",
        expected_type_struct_chain(10_000),
    );
}

#[test]
fn composite_diagnostics_match_at_every_depth_limit() {
    let cases = [
        (
            "error_slice_base",
            DeepExpr::program("missing[1+true:2]".to_string()),
        ),
        (
            "borrow_before_inner",
            DeepExpr::program("print(&missing)".to_string()),
        ),
        (
            "unknown_error_member",
            "errors IoError:\n\tEof\nfn main():\n\tvalue = IoError.Unknown(missing())\n"
                .to_string(),
        ),
        (
            "struct_field_order",
            "struct Point:\n\tx: int\nfn main():\n\tp = Point{z=1+true, x=missing()}\n".to_string(),
        ),
        (
            "duplicate_enum_field_order",
            "enum Shape:\n\tRectangle(width: int)\nfn main():\n\tshape = Shape.Rectangle{width=1+true, width=missing()}\n"
                .to_string(),
        ),
        (
            "error_payload_order",
            "error E(int, int)\nfn main():\n\tvalue = E(1+true, missing())\n".to_string(),
        ),
        (
            "never_under_unary",
            DeepExpr::program("-panic(\"boom\")".to_string()),
        ),
        (
            "never_under_composite",
            DeepExpr::program("({x=panic(\"boom\")}).x".to_string()),
        ),
    ];
    for (name, src) in cases {
        let baseline = snapshot(run(&src));
        for limit in [0, 1, 64] {
            assert_eq!(
                snapshot(run_with_expr_limit(&src, limit)),
                baseline,
                "{name} differed at expression depth limit {limit}"
            );
        }
    }
}

#[test]
fn never_rejection_matches_at_every_depth_limit() {
    for src in [
        DeepExpr::program("-panic(\"boom\")".to_string()),
        DeepExpr::program("({x=panic(\"boom\")}).x".to_string()),
    ] {
        let baseline = snapshot(run(&src));
        for limit in [0, 1, 64] {
            assert_eq!(snapshot(run_with_expr_limit(&src, limit)), baseline);
        }
    }
}

#[test]
fn nested_expected_struct_coercions_match_at_every_depth_limit() {
    let src = r#"error A
error B
struct AnonymousNested:
	value: {value: (A | B)!int}
struct NamedNested:
	value: {value: (A | B)!int}
fn main():
	anonymous = AnonymousNested{value={value=1}}
	named = NamedNested{value={value=A}}
"#;
    let baseline = snapshot(run(src));
    for limit in [0, 1, 64] {
        assert_eq!(snapshot(run_with_expr_limit(src, limit)), baseline);
    }
}

#[test]
fn shallow_analysis_does_not_start_allocating_machine() {
    let src = DeepExpr::program("1+2*3-4".to_string());
    super::walker::reset_machine_call_count();
    run(&src).expect("shallow expression analyzes");
    assert_eq!(super::walker::machine_call_count(), 0);
    run_with_expr_limit(&src, 0).expect("forced fallback analyzes");
    assert!(super::walker::machine_call_count() > 0);
}

#[test]
fn match_return_struct_literal_uses_function_return_type() {
    let src = r#"error A
error B
fn narrow() -> A!int:
	return 3
fn choose() -> {value: (A | B)!int}:
	result = match 1:
		1:
			return {value=narrow()}
		_:
			return {value=1}
	return result
"#;
    let baseline = snapshot(run(src));
    for limit in [0, 1, 64] {
        assert_eq!(snapshot(run_with_expr_limit(src, limit)), baseline);
    }
}

#[test]
fn borrowed_match_scrutinee_matches_at_every_depth_limit() {
    let src = r#"enum Opt:
	Some(value: int)
	None
fn main():
	o = Opt.Some{value=5}
	x = match &o:
		Opt.Some(v): v
		Opt.None: 0
	print(x)
"#;
    let baseline = snapshot(run(src));
    for limit in [0, 1, 64] {
        assert_eq!(snapshot(run_with_expr_limit(src, limit)), baseline);
    }
}

#[test]
fn named_struct_uses_declared_shape_not_incompatible_call_target() {
    let src = r#"struct Point:
	x: int
struct Other:
	y: int
fn take(other: Other):
	return
fn main():
	take(Point{x=1})
"#;
    let baseline = snapshot(run(src));
    for limit in [0, 1, 64] {
        assert_eq!(snapshot(run_with_expr_limit(src, limit)), baseline);
    }
}

#[test]
fn non_struct_call_target_does_not_reach_struct_layout_lookup() {
    for literal in ["Point{x=1}", "{x=1}"] {
        let src = format!(
            "struct Point:\n\tx: int\nfn take(value: int):\n\treturn\nfn main():\n\ttake({literal})\n"
        );
        let baseline = snapshot(run(&src));
        for limit in [0, 1, 64] {
            assert_eq!(snapshot(run_with_expr_limit(&src, limit)), baseline);
        }
    }
}
