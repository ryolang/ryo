//! Fresh-depth audit checklist:
//! - `sema/expr/mod.rs`, `methods.rs`, `operators.rs`: nested operands.
//! - `sema/call.rs`, `errors.rs`, `match_.rs`: nested operands and scoped bodies.
//! - `sema/stmt.rs`: statement roots and nested expression statements.
//! - `codegen/expr/mod.rs`, `fat_slot.rs`, `builtins.rs`: nested value evaluation.
//! - `codegen/structs.rs`, `match_.rs`, `control.rs`: aggregates and Flow barriers.
//! - `codegen/mod.rs`, `decls.rs`: statement/declaration roots.

use super::super::tests::{RunOk, run};
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
                Self::program("missing(1)".to_string()),
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
                "fn main():\n\tprint(1[0])\n".to_string(),
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

fn run_with_expr_limit(src: &str, _limit: u32) -> Result<RunOk, Vec<Diag>> {
    run(src)
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
#[ignore = "I-269: unary operands still reset expression depth"]
fn unary_mediated_rhs_chain_analyzes_without_overflow() {
    run_deep_repro(
        "sema::expr::walker_tests::unary_mediated_rhs_chain_analyzes_without_overflow",
        DeepExpr::unary_mediated(50_000),
    );
}

#[test]
#[ignore = "I-269: call arguments still reset expression depth"]
fn call_mediated_rhs_chain_analyzes_without_overflow() {
    run_deep_repro(
        "sema::expr::walker_tests::call_mediated_rhs_chain_analyzes_without_overflow",
        DeepExpr::call_mediated(50_000),
    );
}

#[test]
#[ignore = "I-269: nested call arguments still reset expression depth"]
fn nested_call_chain_analyzes_without_overflow() {
    run_deep_repro(
        "sema::expr::walker_tests::nested_call_chain_analyzes_without_overflow",
        DeepExpr::nested_calls(50_000),
    );
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
