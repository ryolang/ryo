mod common;
use common::*;

use std::process::Command;
use tempfile::TempDir;

const DIRECT_ERROR_UNION: &str = r#"error A
fn f(n: int) -> A!int:
	if n == 0:
		return 42
	return f(n - 1)
fn main():
	value = f(3) catch:
		A: -1
	print(value)
"#;

const MUTUAL_ERROR_UNION: &str = r#"error A
fn f(n: int) -> A!int:
	if n == 0:
		return 42
	return g(n - 1)
fn g(n: int) -> A!int:
	if n == 0:
		return 42
	return f(n - 1)
fn main():
	value = f(3) catch:
		A: -1
	print(value)
"#;

const DIRECT_STRUCT: &str = r#"struct Payload:
	value: int
fn f(n: int) -> Payload:
	if n == 0:
		return Payload{value=42}
	return f(n - 1)
fn main():
	result = f(3)
	print(result.value)
"#;

const MUTUAL_STRUCT: &str = r#"struct Payload:
	value: int
fn f(n: int) -> Payload:
	if n == 0:
		return Payload{value=42}
	return g(n - 1)
fn g(n: int) -> Payload:
	if n == 0:
		return Payload{value=42}
	return f(n - 1)
fn main():
	result = f(3)
	print(result.value)
"#;

const DIRECT_ENUM: &str = r#"enum Outcome:
	Value(int)
fn f(n: int) -> Outcome:
	if n == 0:
		return Outcome.Value(42)
	return f(n - 1)
fn main():
	result = f(3)
	print(result)
"#;

const MUTUAL_ENUM: &str = r#"enum Outcome:
	Value(int)
fn f(n: int) -> Outcome:
	if n == 0:
		return Outcome.Value(42)
	return g(n - 1)
fn g(n: int) -> Outcome:
	if n == 0:
		return Outcome.Value(42)
	return f(n - 1)
fn main():
	result = f(3)
	print(result)
"#;

const SCALAR_TAIL_CALL: &str = r#"fn f(n: int) -> int:
	if n == 0:
		return 0
	return f(n - 1)
fn main():
	print(f(10000000))
"#;

fn assert_jit_and_aot(name: &str, source: &str, expected: &str) {
    assert_ryo_output(&format!("{name}_jit.ryo"), source, expected);

    let temp_dir = TempDir::new().expect("temp");
    let stem = format!("{name}_aot");
    let test_file = create_test_file(temp_dir.path(), &format!("{stem}.ryo"), source);
    let build = run_ryo_build(&test_file, temp_dir.path());
    assert!(
        build.status.success(),
        "ryo build failed for {name}: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(exe_path(temp_dir.path(), &stem))
        .output()
        .expect("run AOT binary");
    assert!(
        run.status.success(),
        "AOT binary failed for {name}: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), expected);
}

#[test]
fn direct_error_union_recursion_runs_jit_and_aot() {
    assert_jit_and_aot("direct_error_union", DIRECT_ERROR_UNION, "42");
}

#[test]
fn mutual_error_union_recursion_runs_jit_and_aot() {
    assert_jit_and_aot("mutual_error_union", MUTUAL_ERROR_UNION, "42");
}

#[test]
fn direct_struct_recursion_runs_jit_and_aot() {
    assert_jit_and_aot("direct_struct", DIRECT_STRUCT, "42");
}

#[test]
fn mutual_struct_recursion_runs_jit_and_aot() {
    assert_jit_and_aot("mutual_struct", MUTUAL_STRUCT, "42");
}

#[test]
fn direct_enum_recursion_runs_jit_and_aot() {
    assert_jit_and_aot("direct_enum", DIRECT_ENUM, "Outcome.Value(42)");
}

#[test]
fn mutual_enum_recursion_runs_jit_and_aot() {
    assert_jit_and_aot("mutual_enum", MUTUAL_ENUM, "Outcome.Value(42)");
}

#[test]
fn scalar_tail_recursion_remains_constant_stack_jit_and_aot() {
    assert_jit_and_aot("scalar_tail_call", SCALAR_TAIL_CALL, "0");
}
