//! Regression tests for deeply nested operator chains.
//!
//! A ~7k-term `1+1+...` chain used to abort the compiler with
//! `thread has overflowed its stack` (SIGABRT): every pass that walks
//! expression nesting recursed one frame per chain link. Those walks
//! are iterative now; these tests pin the fix end to end through the
//! CLI (which runs the compiler on a 32 MiB thread — the recursion
//! overflowed it at ~7k terms).

mod common;
use common::*;

use std::process::{Command, Stdio};
use tempfile::TempDir;

/// `print(1+1+... )` with `terms` ones.
fn add_chain(terms: usize) -> String {
    let mut src = String::from("fn main():\n\tprint(");
    for i in 0..terms {
        if i > 0 {
            src.push('+');
        }
        src.push('1');
    }
    src.push_str(")\n");
    src
}

fn right_nested_add_chain(terms: usize) -> String {
    let mut src = String::from("fn main():\n\tprint(");
    for _ in 1..terms {
        src.push_str("1+(");
    }
    src.push('1');
    for _ in 1..terms {
        src.push(')');
    }
    src.push_str(")\n");
    src
}

#[test]
fn jit_run_right_nested_addition_chain_prints_sum() {
    let terms = 20_000;
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(
        temp_dir.path(),
        "right_nested_chain.ryo",
        &right_nested_add_chain(terms),
    );

    let output = run_ryo_command(&["run", "right_nested_chain.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(
        output.status.success(),
        "right-nested chain compile failed. STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        terms.to_string(),
        "right-nested chain must evaluate to its term count"
    );
}

fn unary_mediated_add_chain(links: usize) -> String {
    let mut src = String::from("fn main():\n\tprint(");
    for _ in 0..links {
        src.push_str("1+(-(");
    }
    src.push('1');
    for _ in 0..links {
        src.push_str("))");
    }
    src.push_str(")\n");
    src
}

fn call_mediated_add_chain(links: usize) -> String {
    let mut src = String::from("fn id(x: int) -> int:\n\treturn x\n\nfn main():\n\tprint(");
    for _ in 0..links {
        src.push_str("1+id(");
    }
    src.push('1');
    for _ in 0..links {
        src.push(')');
    }
    src.push_str(")\n");
    src
}

fn nested_call_chain(links: usize) -> String {
    let mut src = String::from("fn id(x: int) -> int:\n\treturn x\n\nfn main():\n\tprint(");
    for _ in 0..links {
        src.push_str("id(");
    }
    src.push('1');
    for _ in 0..links {
        src.push(')');
    }
    src.push_str(")\n");
    src
}

fn assert_jit_prints(file_name: &str, src: &str, expected: &str) {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), file_name, src);
    let output =
        run_ryo_command(&["run", file_name], &test_file).expect("Failed to run ryo run command");
    assert!(
        output.status.success(),
        "expression compile failed. STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), expected);
}

#[test]
fn jit_run_unary_mediated_rhs_chain() {
    assert_jit_prints("unary_mediated.ryo", &unary_mediated_add_chain(10_000), "1");
}

#[test]
fn jit_run_call_mediated_rhs_chain() {
    assert_jit_prints(
        "call_mediated.ryo",
        &call_mediated_add_chain(10_000),
        "10001",
    );
}

#[test]
fn jit_run_nested_call_chain() {
    assert_jit_prints("nested_calls.ryo", &nested_call_chain(10_000), "1");
}

fn nested_string_concat(links: usize) -> String {
    let mut src = String::from("fn main():\n\ts = ");
    for _ in 0..links {
        src.push_str("\"a\"+(");
    }
    src.push_str("\"a\"");
    for _ in 0..links {
        src.push(')');
    }
    src.push_str("\n\tprint(s.len())\n");
    src
}

fn nested_slices(links: usize) -> String {
    let mut src = String::from("fn main():\n\ts = \"a\"\n\tv = s");
    for _ in 0..links {
        src.push_str("[0:1]");
    }
    src.push_str("\n\tprint(v.len())\n");
    src
}

#[test]
fn jit_run_nested_string_concat() {
    assert_jit_prints("nested_concat.ryo", &nested_string_concat(10_000), "10001");
}

#[test]
fn jit_run_nested_slices() {
    assert_jit_prints("nested_slices.ryo", &nested_slices(1_000), "1");
}

#[test]
fn jit_run_aggregate_field_evaluation_order() {
    let src = "fn mark(x: int) -> int:\n\tprint(x)\n\treturn x\n\nstruct Pair:\n\tx: int\n\ty: int\n\nfn main():\n\tp = Pair{x=mark(1), y=mark(2)}\n\tprint(p.x)\n";
    assert_jit_prints("aggregate_order.ryo", src, "121");
}

fn nested_bool_and(links: usize) -> String {
    let mut src = String::from("fn main():\n\tprint(");
    for _ in 0..links {
        src.push_str("true and (");
    }
    src.push_str("true");
    for _ in 0..links {
        src.push(')');
    }
    src.push_str(")\n");
    src
}

fn nested_statement_matches(depth: usize) -> String {
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

fn nested_ifs(depth: usize) -> String {
    let mut src = String::from("fn main():\n");
    for level in 0..depth {
        src.push_str(&"\t".repeat(level + 1));
        src.push_str("if true:\n");
    }
    src.push_str(&"\t".repeat(depth + 1));
    src.push_str("print(1)\n");
    src
}

#[test]
fn jit_run_nested_bool_and() {
    assert_jit_prints("nested_bool_and.ryo", &nested_bool_and(10_000), "true");
}

#[test]
fn jit_run_nested_statement_matches() {
    assert_jit_prints(
        "nested_statement_matches.ryo",
        &nested_statement_matches(1_000),
        "0",
    );
}

#[test]
fn jit_run_nested_ifs() {
    assert_jit_prints("nested_ifs.ryo", &nested_ifs(1_000), "1");
}

fn alternating_add_chain(terms: usize) -> String {
    let mut prefixes = Vec::with_capacity(terms - 1);
    let mut suffix = String::new();
    for i in 0..terms - 1 {
        if i % 2 == 0 {
            prefixes.push("(");
            suffix.push_str("+1)");
        } else {
            prefixes.push("(1+");
            suffix.push(')');
        }
    }
    let mut src = String::from("fn main():\n\tprint(");
    for prefix in prefixes.iter().rev() {
        src.push_str(prefix);
    }
    src.push('1');
    src.push_str(&suffix);
    src.push_str(")\n");
    src
}

#[test]
fn jit_run_alternating_nested_addition_chain_prints_sum() {
    let terms = 20_000;
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(
        temp_dir.path(),
        "alternating_chain.ryo",
        &alternating_add_chain(terms),
    );

    let output = run_ryo_command(&["run", "alternating_chain.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(
        output.status.success(),
        "alternating nested chain compile failed. STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        terms.to_string(),
        "alternating nested chain must evaluate to its term count"
    );
}

fn nested_left_spines_add_chain(groups: usize, spine_terms: usize) -> String {
    let mut src = String::from("fn main():\n\tprint(");
    for _ in 1..groups {
        src.push_str("1+(");
    }
    src.push('1');
    for _ in 1..groups {
        src.push(')');
        for _ in 0..spine_terms {
            src.push_str("+1");
        }
    }
    src.push_str(")\n");
    src
}

#[test]
fn jit_run_nested_long_left_spines_print_sum() {
    let groups = 1_000;
    let spine_terms = 70;
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(
        temp_dir.path(),
        "nested_left_spines_chain.ryo",
        &nested_left_spines_add_chain(groups, spine_terms),
    );

    let output = run_ryo_command(&["run", "nested_left_spines_chain.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(
        output.status.success(),
        "nested long left spines compile failed. STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        ((groups - 1) * (spine_terms + 1) + 1).to_string(),
        "nested long left spines must evaluate to their term count"
    );
}

#[test]
fn jit_run_deep_addition_chain_prints_sum() {
    // 20k terms: well past the old recursion's ~7k overflow threshold.
    let terms = 20_000;
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "deep_chain.ryo", &add_chain(terms));

    let output = run_ryo_command(&["run", "deep_chain.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(
        output.status.success(),
        "deep chain compile failed. STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        terms.to_string(),
        "deep chain must evaluate to its term count"
    );
}

#[test]
fn parse_deep_addition_chain_succeeds() {
    // The tree dump is quadratic in nesting depth (every line carries
    // its full indentation prefix) — discard stdout and assert only
    // the exit code, or this test would buffer gigabytes.
    let terms = 20_000;
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "deep_chain.ryo", &add_chain(terms));

    // `.output()` (not `.status()`): the child must be able to finish
    // writing stderr even when a diagnostic regression floods it past
    // the pipe buffer, and the failure message needs the cause.
    let output = Command::new(env!("CARGO_BIN_EXE_ryo"))
        .arg("parse")
        .arg(&test_file)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .expect("Failed to run ryo parse command");

    assert!(
        output.status.success(),
        "deep chain parse failed (overflow or diagnostics): {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
