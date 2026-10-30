mod common;
use common::*;

use std::process::Command;
use tempfile::TempDir;

const MESSAGE_PROGRAM: &str = r#"error Timeout
error Missing(str)
error Named(path: str)
error Detailed(code: int, message: str)
error FileNotFound(path: str, permission_level: int)
error Answer(int)
error Active(str)

fn find(id: int) -> (Missing | Named)!str:
	if id == 0:
		return Missing("missing")
	return Named(path="not found")

fn active() -> (Timeout | Active)!str:
	return Active("42")

fn print_twice(err: FileNotFound):
	print(err.message() + "\n")
	print(err.message() + "\n")

fn main():
	timeout = Timeout
	print(timeout.message() + "\n")
	first = find(0) catch as err:
		_: err.message()
	print(first + "\n")
	second = find(1) catch as err:
		_: err.message()
	print(second + "\n")
	detailed = Detailed(code=404, message="not found")
	print(detailed.message() + "\n")
	file = FileNotFound(path="/x", permission_level=7)
	print_twice(file)
	print(file.message() + "\n")
	answer = Answer(42)
	print(answer.message() + "\n")
	active_message = active() catch as err:
		_: err.message()
	print(active_message + "\n")
"#;

const MESSAGE_OUTPUT: &str = "Timeout\nmissing\nnot found\nnot found\nFileNotFound(path=/x, permission_level=7)\nFileNotFound(path=/x, permission_level=7)\nFileNotFound(path=/x, permission_level=7)\nAnswer(42)\n42\n";

#[test]
fn compiler_known_messages_run_jit() {
    assert_ryo_output("error_messages_jit.ryo", MESSAGE_PROGRAM, MESSAGE_OUTPUT);
}

#[test]
fn compiler_known_messages_run_aot() {
    let temp_dir = TempDir::new().expect("temp");
    let test_file = create_test_file(temp_dir.path(), "error_messages_aot.ryo", MESSAGE_PROGRAM);
    let build = run_ryo_build(&test_file, temp_dir.path());
    assert!(
        build.status.success(),
        "ryo build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(exe_path(temp_dir.path(), "error_messages_aot"))
        .output()
        .expect("run AOT error-message binary");
    assert!(
        run.status.success(),
        "AOT binary failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), MESSAGE_OUTPUT);
}

#[test]
fn direct_error_union_message_is_rejected() {
    let src = r#"error Timeout
fn fail() -> Timeout!int:
	return Timeout
fn main():
	result = fail()
	print(result.message())
"#;
    let temp_dir = TempDir::new().expect("temp");
    let test_file = create_test_file(temp_dir.path(), "union_message_rejected.ryo", src);
    let output = run_ryo_command(&["run", "union_message_rejected.ryo"], &test_file)
        .expect("run direct-union message rejection");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cannot call a method on error union 'Timeout!int' directly")
            && stderr.contains("use `try` or `catch` to unwrap it"),
        "unexpected diagnostics: {stderr}"
    );
}

#[test]
fn message_rejects_arguments() {
    let src = "error Timeout\nfn main():\n\ttimeout = Timeout\n\tprint(timeout.message(1))\n";
    let temp_dir = TempDir::new().expect("temp");
    let test_file = create_test_file(temp_dir.path(), "message_arguments.ryo", src);
    let output = run_ryo_command(&["run", "message_arguments.ryo"], &test_file)
        .expect("run message arity rejection");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("message() takes no arguments"),
        "unexpected diagnostics: {stderr}"
    );
}
