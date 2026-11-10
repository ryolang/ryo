//! Regression tests for bounded expression nesting and rejection above the limit.
//!
//! A ~7k-term `1+1+...` chain used to abort the compiler with
//! `thread has overflowed its stack` (SIGABRT): every pass that walks
//! expression nesting recursed one frame per chain link. Those walks
//! are iterative for accepted programs, and astgen rejects nesting above
//! 256 levels before later passes receive it. These tests exercise both
//! behaviors end to end through the CLI's 32 MiB compiler thread.

mod common;
use common::*;

use std::process::{Command, Stdio};
use tempfile::TempDir;

static OVERSIZED_SOURCE_REGRESSIONS: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
    let terms = 255;
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

fn assert_run_fails_with_diagnostic(file_name: &str, src: String, expected: &[&str]) {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), file_name, &src);
    // Do not keep a multi-gigabyte parent copy alive while the compiler reads the file.
    drop(src);
    let output =
        run_ryo_command(&["run", file_name], &test_file).expect("Failed to run ryo run command");
    assert!(
        !output.status.success(),
        "over-limit input unexpectedly ran"
    );
    assert!(
        output.status.code().is_some(),
        "compiler terminated by a signal instead of a diagnostic"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    for fragment in expected {
        assert!(
            stderr.contains(fragment),
            "stderr must contain {fragment:?}: {stderr}"
        );
    }
}

fn assert_jit_nesting_rejected(file_name: &str, src: String) {
    assert_run_fails_with_diagnostic(file_name, src, &["E0140", "256"]);
}

#[test]
fn jit_run_unary_mediated_rhs_chain() {
    assert_jit_prints("unary_mediated.ryo", &unary_mediated_add_chain(126), "1");
}

#[test]
fn jit_run_call_mediated_rhs_chain() {
    assert_jit_prints("call_mediated.ryo", &call_mediated_add_chain(127), "128");
}

#[test]
fn jit_run_nested_call_chain() {
    assert_jit_prints("nested_calls.ryo", &nested_call_chain(254), "1");
}

fn assert_aot_prints(test_name: &str, src: &str, expected: &str) {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), &format!("{test_name}.ryo"), src);
    let build = run_ryo_build(&test_file, temp_dir.path());
    assert!(
        build.status.success(),
        "AOT compile failed. STDERR: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(exe_path(temp_dir.path(), test_name))
        .output()
        .expect("Failed to run AOT binary");
    assert!(
        run.status.success(),
        "AOT binary failed. STDERR: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), expected);
}

#[test]
fn aot_run_unary_mediated_rhs_chain() {
    assert_aot_prints("unary_mediated_aot", &unary_mediated_add_chain(126), "1");
}

#[test]
fn aot_run_call_mediated_rhs_chain() {
    assert_aot_prints("call_mediated_aot", &call_mediated_add_chain(127), "128");
}

#[test]
fn aot_run_nested_call_chain() {
    assert_aot_prints("nested_calls_aot", &nested_call_chain(254), "1");
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
    assert_jit_prints("nested_concat.ryo", &nested_string_concat(254), "255");
}

#[test]
fn jit_run_nested_slices() {
    assert_jit_prints("nested_slices.ryo", &nested_slices(200), "1");
}

#[test]
fn jit_run_slice_nesting_near_limit() {
    assert_jit_prints("deep_slices.ryo", &nested_slices(254), "1");
}

#[test]
fn jit_run_nested_direct_catch_ownership() {
    let src = r#"error A(str)
fn id(move x: str) -> str:
	return x
fn consume(move message: str):
	print(message)
fn describe(err: A, move message: str) -> str:
	consume(message)
	return err.message()
fn find() -> A!str:
	return A("x")
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
    assert_jit_prints("nested_direct_catch.ryo", &src, "xx");
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
    assert_jit_prints("nested_bool_and.ryo", &nested_bool_and(254), "true");
}

#[test]
fn jit_run_nested_statement_matches() {
    assert_jit_prints(
        "nested_statement_matches.ryo",
        &nested_statement_matches(100),
        "0",
    );
}

#[test]
fn jit_run_nested_ifs() {
    assert_jit_prints("nested_ifs.ryo", &nested_ifs(200), "1");
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
    let terms = 255;
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
    let groups = 3;
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
fn jit_run_bounded_addition_chain_prints_sum() {
    let terms = 255;
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "deep_chain.ryo", &add_chain(terms));

    let output = run_ryo_command(&["run", "deep_chain.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(
        output.status.success(),
        "bounded chain compile failed. STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        terms.to_string(),
        "bounded chain must evaluate to its term count"
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

fn nested_fat_return_chain(links: usize) -> String {
    let mut src =
        String::from("fn id(move x: str) -> str:\n\treturn x\nfn make() -> str:\n\treturn ");
    src.push_str(&"id(".repeat(links));
    src.push_str("\"x\"");
    src.push_str(&")".repeat(links));
    src.push_str("\nfn main():\n\tprint(make())\n");
    src
}

#[test]
fn jit_run_deferred_fat_return_chain() {
    assert_jit_prints(
        "deferred_fat_return.ryo",
        &nested_fat_return_chain(254),
        "x",
    );
}

#[test]
fn jit_run_aggregate_arguments_snapshot_before_later_effects() {
    let src = r#"struct Pair:
	x: int
fn mutate(inout pair: Pair) -> int:
	pair.x = 9
	return 0
fn first(move pair: Pair, later: int) -> int:
	return pair.x
fn id(x: int) -> int:
	return x
fn make() -> int:
	mut pair = Pair{x=1}
	return ID(first(pair, mutate(&pair)))
fn main():
	print(make())
"#;
    let src = src.replace(
        "ID(first(pair, mutate(&pair)))",
        &format!(
            "{}first(pair, mutate(&pair)){}",
            "id(".repeat(70),
            ")".repeat(70)
        ),
    );
    assert_jit_prints("aggregate_argument_snapshot.ryo", &src, "1");
}

#[test]
fn jit_run_aggregate_fields_snapshot_before_later_effects() {
    let src = r#"struct Inner:
	x: int
struct Outer:
	inner: Inner
	later: int
fn mutate(inout inner: Inner) -> int:
	inner.x = 9
	return 0
fn id(move x: Outer) -> Outer:
	return x
fn make() -> int:
	mut inner = Inner{x=1}
	return ID(Outer{inner=inner, later=mutate(&inner)}).inner.x
fn main():
	print(make())
"#;
    let src = src.replace(
        "ID(Outer{inner=inner, later=mutate(&inner)})",
        &format!(
            "{}Outer{{inner=inner, later=mutate(&inner)}}{}",
            "id(".repeat(70),
            ")".repeat(70)
        ),
    );
    assert_jit_prints("aggregate_field_snapshot.ryo", &src, "1");
}

fn nested_struct_return_chain(links: usize) -> String {
    let mut src = String::from(
        "struct Pair:\n\tx: int\nfn id(move x: Pair) -> Pair:\n\treturn x\nfn make() -> Pair:\n\treturn ",
    );
    src.push_str(&"id(".repeat(links));
    src.push_str("Pair{x=7}");
    src.push_str(&")".repeat(links));
    src.push_str("\nfn main():\n\tprint(make().x)\n");
    src
}

#[test]
fn jit_run_deferred_struct_return_chain() {
    assert_jit_prints(
        "deferred_struct_return.ryo",
        &nested_struct_return_chain(254),
        "7",
    );
}

fn nested_enum_return_chain(links: usize) -> String {
    let mut src = String::from(
        "enum Shape:\n\tCircle(int)\nfn id(move x: Shape) -> Shape:\n\treturn x\nfn make() -> Shape:\n\treturn ",
    );
    src.push_str(&"id(".repeat(links));
    src.push_str("Shape.Circle(8)");
    src.push_str(&")".repeat(links));
    src.push_str("\nfn main():\n\tresult = make()\n\tmatch result:\n\t\tShape.Circle(value):\n\t\t\tprint(value)\n");
    src
}

#[test]
fn jit_run_deferred_enum_return_chain() {
    assert_jit_prints(
        "deferred_enum_return.ryo",
        &nested_enum_return_chain(254),
        "8",
    );
}

#[test]
fn jit_run_deferred_slice_length_chain() {
    let src = format!(
        "fn make() -> int:\n\ts = \"abc\"\n\treturn s{}.len()\nfn main():\n\tprint(make())\n",
        "[0:2]".repeat(250)
    );
    assert_jit_prints("deferred_slice_length.ryo", &src, "2");
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

#[test]
fn jit_run_nested_statement_matches_50000() {
    let _guard = OVERSIZED_SOURCE_REGRESSIONS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert_run_fails_with_diagnostic(
        "nested_statement_matches_50000.ryo",
        nested_statement_matches(50_000),
        &["E0130"],
    );
}

#[test]
fn jit_run_nested_catches_50000() {
    let _guard = OVERSIZED_SOURCE_REGRESSIONS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert_jit_nesting_rejected("nested_catches_50000.ryo", nested_catches(50_000));
}
