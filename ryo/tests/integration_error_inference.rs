mod common;
use common::*;

use std::process::Command;
use tempfile::TempDir;

const INFERRED_PROGRAM: &str = r#"error A
error B
fn inner(which: int) -> !str:
	if which == 1:
		return A
	if which == 2:
		return B
	return "ok"
fn outer(which: int) -> !str:
	value = try inner(which)
	return value
fn main():
	for i in range(0, 3):
		value = outer(i) catch:
			A: "caught A"
			B: "caught B"
		print(value + "\n")
"#;
const INFERRED_OUTPUT: &str = "ok\ncaught A\ncaught B\n";

const INSPECTION_PROGRAM: &str = r#"error A
error B
fn inner() -> !str:
	return A
fn outer() -> !str:
	if true:
		return B
	value = try inner()
	return value
fn main():
	return
"#;
const INSPECTION_OUTPUT: &str = "inner() -> A!str\nouter() -> (A | B)!str\n";

#[test]
fn inferred_unions_with_two_nominal_errors_run_jit() {
    assert_ryo_output("inferred_unions_jit.ryo", INFERRED_PROGRAM, INFERRED_OUTPUT);
}

#[test]
fn inferred_unions_with_two_nominal_errors_run_aot() {
    let temp_dir = TempDir::new().expect("temp");
    let test_file = create_test_file(temp_dir.path(), "inferred_unions_aot.ryo", INFERRED_PROGRAM);
    let build = run_ryo_build(&test_file, temp_dir.path());
    assert!(
        build.status.success(),
        "ryo build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(exe_path(temp_dir.path(), "inferred_unions_aot"))
        .output()
        .expect("run inferred-union AOT binary");
    assert!(
        run.status.success(),
        "AOT binary failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), INFERRED_OUTPUT);
}

#[test]
fn run_show_inferred_errors_prints_source_order_and_continues() {
    let temp_dir = TempDir::new().expect("temp");
    let test_file = create_test_file(temp_dir.path(), "inspect_run.ryo", INSPECTION_PROGRAM);
    let output = run_ryo_command(
        &["run", "--show-inferred-errors", "inspect_run.ryo"],
        &test_file,
    )
    .expect("run with inferred-signature inspection");
    assert!(
        output.status.success(),
        "inspection must continue through JIT compilation: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), INSPECTION_OUTPUT);
}

#[test]
fn build_show_inferred_errors_prints_source_order_and_continues() {
    let temp_dir = TempDir::new().expect("temp");
    let test_file = create_test_file(temp_dir.path(), "inspect_build.ryo", INSPECTION_PROGRAM);
    let output = run_ryo_command(
        &["build", "--show-inferred-errors", "inspect_build.ryo"],
        &test_file,
    )
    .expect("build with inferred-signature inspection");
    assert!(
        output.status.success(),
        "inspection must continue through AOT compilation: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), INSPECTION_OUTPUT);
    let run = Command::new(exe_path(temp_dir.path(), "inspect_build"))
        .output()
        .expect("run inspected AOT binary");
    assert!(run.status.success());
    assert!(run.stdout.is_empty());
}

#[test]
fn show_inferred_errors_marks_a_finalized_empty_error_set() {
    let src = "fn succeed() -> !int:\n\treturn 1\nfn main():\n\treturn\n";
    let temp_dir = TempDir::new().expect("temp");
    let test_file = create_test_file(temp_dir.path(), "inspect_empty.ryo", src);
    let output = run_ryo_command(
        &["run", "--show-inferred-errors", "inspect_empty.ryo"],
        &test_file,
    )
    .expect("run empty inferred-signature inspection");
    assert!(
        output.status.success(),
        "empty inspection failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "succeed() -> !int (no errors)\n"
    );
}
