mod common;
use common::*;

use std::process::Command;
use tempfile::TempDir;

const DIRECT_TRY: &str = r#"fn inner() -> !int:
	return 7
fn outer() -> !int:
	value = try inner()
	return value
fn main():
	value = outer() catch:
		_: 0
	print(int_to_str(value) + "\n")
"#;

const NESTED_TRY: &str = r#"fn inner() -> !int:
	return 7
fn middle() -> !int:
	return try inner()
fn outer() -> !int:
	return try middle()
fn main():
	value = outer() catch:
		_: 0
	print(int_to_str(value) + "\n")
"#;

const RAW_BINDING: &str = r#"fn inner() -> !int:
	return 7
fn main():
	value = inner()
	print("raw\n")
"#;

const EMPTY_CATCH: &str = r#"fn inner() -> !int:
	return 7
fn main():
	value = inner() catch:
		_: 0
	print(int_to_str(value) + "\n")
"#;

const NON_EMPTY_CONTROL: &str = r#"error A
fn inner(fail: bool) -> !int:
	if fail:
		return A
	return 7
fn outer(fail: bool) -> !int:
	value = try inner(fail)
	return value
fn main():
	ok = outer(false) catch:
		A: 0
	caught = outer(true) catch:
		A: 9
	print(int_to_str(ok) + "," + int_to_str(caught) + "\n")
"#;

const INSPECTION: &str = r#"fn inner() -> !int:
	return 7
fn outer() -> !int:
	value = try inner()
	return value
fn main():
	outer()
"#;
const INSPECTION_OUTPUT: &str = "inner() -> !int (no errors)\nouter() -> !int (no errors)\n";

fn assert_aot_output(test_name: &str, source: &str, expected: &str) {
    let temp_dir = TempDir::new().expect("temp");
    let test_file = create_test_file(temp_dir.path(), test_name, source);
    let build = run_ryo_build(&test_file, temp_dir.path());
    assert!(
        build.status.success(),
        "ryo build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let stem = test_name.strip_suffix(".ryo").expect("Ryo file name");
    let run = Command::new(exe_path(temp_dir.path(), stem))
        .output()
        .expect("run AOT binary");
    assert!(
        run.status.success(),
        "AOT binary failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), expected);
}

#[test]
fn direct_try_with_empty_inferred_errors_runs_jit() {
    assert_ryo_output("empty_direct_try_jit.ryo", DIRECT_TRY, "7\n");
}

#[test]
fn direct_try_with_empty_inferred_errors_runs_aot() {
    assert_aot_output("empty_direct_try_aot.ryo", DIRECT_TRY, "7\n");
}

#[test]
fn nested_try_with_empty_inferred_errors_runs_jit() {
    assert_ryo_output("empty_nested_try_jit.ryo", NESTED_TRY, "7\n");
}

#[test]
fn nested_try_with_empty_inferred_errors_runs_aot() {
    assert_aot_output("empty_nested_try_aot.ryo", NESTED_TRY, "7\n");
}

#[test]
fn raw_empty_inferred_union_binding_runs_jit() {
    assert_ryo_output("empty_raw_binding_jit.ryo", RAW_BINDING, "raw\n");
}

#[test]
fn raw_empty_inferred_union_binding_runs_aot() {
    assert_aot_output("empty_raw_binding_aot.ryo", RAW_BINDING, "raw\n");
}

#[test]
fn catch_with_empty_inferred_errors_runs_jit() {
    assert_ryo_output("empty_catch_jit.ryo", EMPTY_CATCH, "7\n");
}

#[test]
fn catch_with_empty_inferred_errors_runs_aot() {
    assert_aot_output("empty_catch_aot.ryo", EMPTY_CATCH, "7\n");
}

#[test]
fn non_empty_inferred_error_control_runs_jit() {
    assert_ryo_output(
        "non_empty_inference_control_jit.ryo",
        NON_EMPTY_CONTROL,
        "7,9\n",
    );
}

#[test]
fn non_empty_inferred_error_control_runs_aot() {
    assert_aot_output(
        "non_empty_inference_control_aot.ryo",
        NON_EMPTY_CONTROL,
        "7,9\n",
    );
}

#[test]
fn empty_inferred_error_inspection_runs_jit() {
    let temp_dir = TempDir::new().expect("temp");
    let test_file = create_test_file(temp_dir.path(), "empty_inspect_jit.ryo", INSPECTION);
    let output = run_ryo_command(
        &["run", "--show-inferred-errors", "empty_inspect_jit.ryo"],
        &test_file,
    )
    .expect("run with empty inferred-signature inspection");
    assert!(
        output.status.success(),
        "inspection failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), INSPECTION_OUTPUT);
}

#[test]
fn empty_inferred_error_inspection_runs_aot() {
    let temp_dir = TempDir::new().expect("temp");
    let test_file = create_test_file(temp_dir.path(), "empty_inspect_aot.ryo", INSPECTION);
    let output = run_ryo_command(
        &["build", "--show-inferred-errors", "empty_inspect_aot.ryo"],
        &test_file,
    )
    .expect("build with empty inferred-signature inspection");
    assert!(
        output.status.success(),
        "inspection build failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), INSPECTION_OUTPUT);
    let run = Command::new(exe_path(temp_dir.path(), "empty_inspect_aot"))
        .output()
        .expect("run inspected AOT binary");
    assert!(
        run.status.success(),
        "inspected AOT binary failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(run.stdout.is_empty());
}

const EMPTY_TO_EXPLICIT: &str = r#"error A
fn inner() -> !int:
	return 7
fn outer() -> A!int:
	return try inner()
fn main():
	value = outer() catch:
		A: 0
	print(int_to_str(value) + "\n")
"#;

#[test]
fn empty_inferred_try_to_explicit_union_runs_jit() {
    assert_ryo_output("empty_to_explicit_try_jit.ryo", EMPTY_TO_EXPLICIT, "7\n");
}

#[test]
fn empty_inferred_try_to_explicit_union_runs_aot() {
    assert_aot_output("empty_to_explicit_try_aot.ryo", EMPTY_TO_EXPLICIT, "7\n");
}
