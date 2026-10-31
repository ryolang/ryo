mod common;
use common::*;

use std::process::Command;
use tempfile::TempDir;

fn assert_aot_output(name: &str, src: &str, expected: &str) {
    let temp_dir = TempDir::new().expect("temp");
    let test_file = create_test_file(temp_dir.path(), &format!("{name}.ryo"), src);
    let build = run_ryo_build(&test_file, temp_dir.path());
    assert!(
        build.status.success(),
        "ryo build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(exe_path(temp_dir.path(), name))
        .output()
        .expect("run AOT void-catch binary");
    assert!(
        run.status.success(),
        "AOT binary failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), expected);
}

const DIRECT_CATCH: &str = r#"error A
fn maybe(fail: bool) -> A!void:
	if fail:
		return A
	return
fn check(fail: bool):
	maybe(fail) catch as err:
		A: print("direct " + err.message() + "\n")
	print("direct success path\n")
fn main():
	check(false)
	check(true)
"#;
const DIRECT_CATCH_OUTPUT: &str = "direct success path\ndirect A\ndirect success path\n";

#[test]
fn direct_catch_void_success_and_error_run_jit() {
    assert_ryo_output(
        "direct_catch_void_success_jit.ryo",
        DIRECT_CATCH,
        DIRECT_CATCH_OUTPUT,
    );
}

#[test]
fn direct_catch_void_success_and_error_run_aot() {
    assert_aot_output(
        "direct_catch_void_success_aot",
        DIRECT_CATCH,
        DIRECT_CATCH_OUTPUT,
    );
}

const BARE_CATCH: &str = r#"error A
fn maybe(fail: bool) -> A!void:
	if fail:
		return A
	return
fn check(fail: bool):
	maybe(fail) catch:
		A: print("bare error\n")
	print("bare success path\n")
fn main():
	check(false)
	check(true)
"#;
const BARE_CATCH_OUTPUT: &str = "bare success path\nbare error\nbare success path\n";

#[test]
fn bare_catch_void_success_and_error_run_jit() {
    assert_ryo_output(
        "bare_catch_void_success_jit.ryo",
        BARE_CATCH,
        BARE_CATCH_OUTPUT,
    );
}

#[test]
fn bare_catch_void_success_and_error_run_aot() {
    assert_aot_output("bare_catch_void_success_aot", BARE_CATCH, BARE_CATCH_OUTPUT);
}

const NESTED_CATCH: &str = r#"error A
fn maybe(fail: bool) -> A!void:
	if fail:
		return A
	return
fn check(fail: bool):
	maybe(fail) catch as err:
		match err:
			A: print("nested error\n")
	print("nested success path\n")
fn main():
	check(false)
	check(true)
"#;
const NESTED_CATCH_OUTPUT: &str = "nested success path\nnested error\nnested success path\n";

#[test]
fn nested_catch_void_success_and_error_run_jit() {
    assert_ryo_output(
        "nested_catch_void_success_jit.ryo",
        NESTED_CATCH,
        NESTED_CATCH_OUTPUT,
    );
}

#[test]
fn nested_catch_void_success_and_error_run_aot() {
    assert_aot_output(
        "nested_catch_void_success_aot",
        NESTED_CATCH,
        NESTED_CATCH_OUTPUT,
    );
}

const TRY_WITHOUT_CATCH: &str = r#"error A
fn inner() -> A!void:
	return
fn outer() -> A!void:
	try inner()
	return
fn main():
	outer()
	print("try ok\n")
"#;

#[test]
fn try_void_success_without_catch_runs_jit() {
    assert_ryo_output(
        "try_void_success_control_jit.ryo",
        TRY_WITHOUT_CATCH,
        "try ok\n",
    );
}

#[test]
fn try_void_success_without_catch_runs_aot() {
    assert_aot_output(
        "try_void_success_control_aot",
        TRY_WITHOUT_CATCH,
        "try ok\n",
    );
}
