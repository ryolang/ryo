mod common;
use common::*;

use std::process::Command;
use tempfile::TempDir;

const PROGRAM: &str = r#"error A
error B
struct Holder:
	value: (A | B)!int
struct AnonymousNested:
	value: {value: (A | B)!int}
struct NamedNested:
	value: Holder
fn narrow() -> A!int:
	return 3
fn wide() -> (A | B)!int:
	return 4
fn unwrap(value: (A | B)!int) -> int:
	return value catch as err:
		A: 10
		B: 20
fn show(value: (A | B)!int):
	print(int_to_str(unwrap(value)) + "\n")
fn main():
	named_success = Holder{value=1}
	named_error = Holder{value=A}
	named_narrow = Holder{value=narrow()}
	named_exact = Holder{value=wide()}
	anonymous_success: {value: (A | B)!int} = {value=5}
	anonymous_error: {value: (A | B)!int} = {value=A}
	anonymous_narrow: {value: (A | B)!int} = {value=narrow()}
	anonymous_exact: {value: (A | B)!int} = {value=wide()}
	anonymous_nested = AnonymousNested{value={value=6}}
	named_nested = NamedNested{value=Holder{value=7}}
	show(named_success.value)
	show(named_error.value)
	show(named_narrow.value)
	show(named_exact.value)
	show(anonymous_success.value)
	show(anonymous_error.value)
	show(anonymous_narrow.value)
	show(anonymous_exact.value)
	show(anonymous_nested.value.value)
	show(named_nested.value.value)
"#;
const OUTPUT: &str = "1\n10\n3\n4\n5\n10\n3\n4\n6\n7\n";

#[test]
fn struct_literal_error_union_coercion_runs_jit() {
    assert_ryo_output(
        "struct_literal_error_union_coercion_jit.ryo",
        PROGRAM,
        OUTPUT,
    );
}

#[test]
fn struct_literal_error_union_coercion_runs_aot() {
    let temp_dir = TempDir::new().expect("temp");
    let test_file = create_test_file(
        temp_dir.path(),
        "struct_literal_error_union_coercion_aot.ryo",
        PROGRAM,
    );
    let build = run_ryo_build(&test_file, temp_dir.path());
    assert!(
        build.status.success(),
        "ryo build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(exe_path(
        temp_dir.path(),
        "struct_literal_error_union_coercion_aot",
    ))
    .output()
    .expect("run struct-literal coercion AOT binary");
    assert!(
        run.status.success(),
        "AOT binary failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), OUTPUT);
}

#[test]
fn struct_literal_error_union_narrowing_is_rejected() {
    let cases = [
        (
            "named_narrowing.ryo",
            r#"error A
error B
struct Holder:
	value: A!int
fn wide() -> (A | B)!int:
	return 1
fn main():
	holder = Holder{value=wide()}
"#,
        ),
        (
            "anonymous_narrowing.ryo",
            r#"error A
error B
fn wide() -> (A | B)!int:
	return 1
fn main():
	holder: {value: A!int} = {value=wide()}
"#,
        ),
    ];
    for (name, src) in cases {
        let temp_dir = TempDir::new().expect("temp");
        let test_file = create_test_file(temp_dir.path(), name, src);
        let output = run_ryo_command(&["run", name], &test_file).expect("run narrowing case");
        assert!(!output.status.success(), "narrowing must be rejected");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("E0012"), "wrong diagnostic: {stderr}");
    }
}
