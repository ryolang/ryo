mod common;
use common::*;

use std::process::Command;
use tempfile::TempDir;

const ERROR_RETURN: &str = "error NotFound\n\nfn fail() -> NotFound!int:\n\treturn NotFound\n\nfn main():\n\tresult = fail()\n\tprint(\"ok\\n\")\n";

#[test]
fn error_return_constructs_and_runs_jit() {
    assert_ryo_output("error_return_jit.ryo", ERROR_RETURN, "ok\n");
}

#[test]
fn error_return_constructs_and_runs_aot() {
    let temp_dir = TempDir::new().expect("temp");
    let test_file = create_test_file(temp_dir.path(), "error_return_aot.ryo", ERROR_RETURN);
    let build = run_ryo_build(&test_file, temp_dir.path());
    assert!(
        build.status.success(),
        "ryo build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(exe_path(temp_dir.path(), "error_return_aot"))
        .output()
        .expect("run AOT error-return binary");
    assert!(
        run.status.success(),
        "AOT binary failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "ok\n");
}

#[test]
fn active_union_alternatives_widen_with_heap_payload_jit() {
    let pad = "a".repeat(40);
    let src = format!(
        "error A(str)\nerror B(int)\n\
         fn narrow(fail: bool) -> A!int:\n\tif fail:\n\t\treturn A(int_to_str(7) + \"{pad}\")\n\treturn 7\n\
         fn widen(fail: bool) -> (A | B)!int:\n\tresult: A!int = narrow(fail)\n\treturn result\n\
         fn main():\n\tok = widen(false)\n\terr = widen(true)\n\tprint(\"ok\\n\")\n"
    );
    assert_ryo_output("error_widening_jit.ryo", &src, "ok\n");
}

#[test]
fn void_success_return_runs_jit() {
    let src = "error A\nfn succeed() -> A!void:\n\treturn\nfn main():\n\tresult = succeed()\n\tprint(\"ok\\n\")\n";
    assert_ryo_output("error_void_success_jit.ryo", src, "ok\n");
}

const DECLARED_ERROR_MATCH: &str = "error NotFound(str)\n\
     fn main():\n\terr = NotFound(int_to_str(7) + \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\")\n\tmatch err:\n\t\tNotFound(message): print(message)\n";
const DECLARED_ERROR_MATCH_OUTPUT: &str = "7aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

#[test]
fn declared_error_match_moves_heap_payload_jit() {
    assert_ryo_output(
        "declared_error_match_jit.ryo",
        DECLARED_ERROR_MATCH,
        DECLARED_ERROR_MATCH_OUTPUT,
    );
}

#[test]
fn declared_error_match_moves_heap_payload_aot() {
    let temp_dir = TempDir::new().expect("temp");
    let test_file = create_test_file(
        temp_dir.path(),
        "declared_error_match_aot.ryo",
        DECLARED_ERROR_MATCH,
    );
    let build = run_ryo_build(&test_file, temp_dir.path());
    assert!(
        build.status.success(),
        "ryo build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(exe_path(temp_dir.path(), "declared_error_match_aot"))
        .output()
        .expect("run AOT declared-error match binary");
    assert!(
        run.status.success(),
        "AOT binary failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        DECLARED_ERROR_MATCH_OUTPUT
    );
}

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
        .expect("run AOT catch binary");
    assert!(
        run.status.success(),
        "AOT binary failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), expected);
}

const DIRECT_CATCH: &str = r#"error NotFound
error Denied

fn find_user(id: int) -> (NotFound | Denied)!str:
	if id == 0:
		return NotFound
	return "user_" + int_to_str(id)

fn main():
	user = find_user(42) catch as err:
		NotFound: "unknown"
		_: "denied"
	print(user + "\n")
	missing = find_user(0) catch as err:
		NotFound: "unknown"
		_: "denied"
	print(missing + "\n")
"#;

#[test]
fn direct_catch_runs_jit() {
    assert_ryo_output("direct_catch_jit.ryo", DIRECT_CATCH, "user_42\nunknown\n");
}

#[test]
fn direct_catch_runs_aot() {
    assert_aot_output("direct_catch_aot", DIRECT_CATCH, "user_42\nunknown\n");
}

#[test]
fn bare_direct_catch_runs_jit() {
    let src = r#"error NotFound
error Denied
fn find_user(id: int) -> (NotFound | Denied)!str:
	if id == 0:
		return NotFound
	return "user_" + int_to_str(id)
fn main():
	user = find_user(0) catch:
		NotFound: "unknown"
		_: "denied"
	print(user + "\n")
"#;
    assert_ryo_output("bare_direct_catch_jit.ryo", src, "unknown\n");
}

#[test]
fn nested_match_catch_remains_compatible_jit() {
    let src = r#"error NotFound
fn find_user(id: int) -> NotFound!str:
	if id == 0:
		return NotFound
	return "user_" + int_to_str(id)
fn main():
	user = find_user(0) catch as err:
		match err:
			NotFound: "unknown"
	print(user + "\n")
"#;
    assert_ryo_output("nested_match_catch_jit.ryo", src, "unknown\n");
}

#[test]
fn wildcard_arm_reads_shared_binding_jit() {
    let src = r#"error NotFound
fn fallback(err: NotFound) -> str:
	return "unknown"
fn find() -> NotFound!str:
	return NotFound
fn main():
	user = find() catch as err:
		_: fallback(err)
	print(user + "\n")
"#;
    assert_ryo_output("catch_wildcard_binding_jit.ryo", src, "unknown\n");
}

const REPEATED_HEAP_CATCH: &str = r#"error NotFound(str)
fn find(id: int) -> NotFound!str:
	if id == 0:
		return NotFound(int_to_str(id) + "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
	return int_to_str(id) + "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
fn main():
	for i in range(0, 8):
		value = find(i % 2) catch as err:
			NotFound(message): message
		print(value)
"#;
const REPEATED_HEAP_CATCH_OUTPUT: &str = "0aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa1bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb0aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa1bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb0aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa1bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb0aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa1bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

#[test]
fn repeated_heap_payload_catch_runs_jit() {
    assert_ryo_output(
        "repeated_heap_catch_jit.ryo",
        REPEATED_HEAP_CATCH,
        REPEATED_HEAP_CATCH_OUTPUT,
    );
}

#[test]
fn repeated_heap_payload_catch_runs_aot() {
    assert_aot_output(
        "repeated_heap_catch_aot",
        REPEATED_HEAP_CATCH,
        REPEATED_HEAP_CATCH_OUTPUT,
    );
}

#[test]
fn diverging_catch_handler_preserves_success_jit() {
    let src = r#"error NotFound
fn find() -> NotFound!str:
	return "user_42"
fn main():
	user = find() catch as err:
		panic("unexpected failure")
	print(user + "\n")
"#;
    assert_ryo_output("diverging_catch_jit.ryo", src, "user_42\n");
}

#[test]
fn ordinary_handler_unused_non_copy_binding_warns_once_at_catch_binding() {
    let src = r#"error A(str)
fn find() -> A!int:
	return A(int_to_str(1) + "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
fn main():
	value = find() catch as err:
		0
	print(value)
"#;
    let temp_dir = TempDir::new().expect("temp");
    let test_file = create_test_file(temp_dir.path(), "catch_handler_warning.ryo", src);
    let output = run_ryo_command(&["run", "catch_handler_warning.ryo"], &test_file)
        .expect("run ordinary catch handler");
    assert!(
        output.status.success(),
        "ordinary handler failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "0");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stderr.matches("W0001").count(),
        1,
        "expected exactly one unused-binding warning: {stderr}"
    );
    assert!(
        stderr.contains(":5:26"),
        "warning must point at the `as err` identifier: {stderr}"
    );
}

#[test]
fn direct_payload_arm_can_read_shared_non_copy_error_jit() {
    let src = r#"error A(str)
fn describe(err: A, move message: str) -> str:
	return message
fn find() -> A!str:
	return A(int_to_str(7) + "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
fn main():
	value = find() catch as err:
		A(message): describe(err, message)
	print(value + "\n")
"#;
    assert_ryo_output(
        "catch_payload_shared_read_jit.ryo",
        src,
        "7aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
    );
}

#[test]
fn nested_match_non_copy_payload_move_rejects_later_shared_read() {
    let src = r#"error A(str)
fn describe(err: A, move message: str) -> str:
	return message
fn find() -> A!str:
	return A(int_to_str(7) + "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
fn main():
	value = find() catch as err:
		match err:
			A(message): describe(err, message)
	print(value + "\n")
"#;
    let temp_dir = TempDir::new().expect("temp");
    let test_file = create_test_file(temp_dir.path(), "nested_catch_move_rejected.ryo", src);
    let output = run_ryo_command(&["run", "nested_catch_move_rejected.ryo"], &test_file)
        .expect("run nested catch match");
    assert!(
        !output.status.success(),
        "a read after the nested match's move must be rejected"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("use of moved value `err`"),
        "unexpected diagnostics: {stderr}"
    );
}

#[test]
fn nested_match_non_copy_payload_move_without_shared_read_runs_jit() {
    let src = r#"error A(str)
fn find() -> A!str:
	return A(int_to_str(7) + "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
fn main():
	value = find() catch as err:
		match err:
			A(message): message
	print(value + "\n")
"#;
    assert_ryo_output(
        "nested_catch_move_cleanup_jit.ryo",
        src,
        "7aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
    );
}

const TRY_PROPAGATION: &str = r#"error A
error B
fn inner(fail: bool) -> A!str:
	print("inner\n")
	if fail:
		return A
	return "ok"
fn outer(fail: bool) -> (A | B)!str:
	value = try inner(fail)
	return value
fn main():
	ok = outer(false) catch:
		A: "caught A"
		B: "caught B"
	print(ok + "\n")
	err = outer(true) catch:
		A: "caught A"
		B: "caught B"
	print(err + "\n")
"#;
const TRY_PROPAGATION_OUTPUT: &str = "inner\nok\ninner\ncaught A\n";

#[test]
fn try_success_and_subset_error_propagation_run_jit() {
    assert_ryo_output(
        "try_propagation_jit.ryo",
        TRY_PROPAGATION,
        TRY_PROPAGATION_OUTPUT,
    );
}

#[test]
fn try_success_and_subset_error_propagation_run_aot() {
    assert_aot_output(
        "try_propagation_aot",
        TRY_PROPAGATION,
        TRY_PROPAGATION_OUTPUT,
    );
}

#[test]
fn bare_try_void_success_runs_jit() {
    let src = r#"error A
fn inner() -> A!void:
	return
fn outer() -> A!void:
	try inner()
	return
fn main():
	outer()
	print("ok\n")
"#;
    assert_ryo_output("try_void_success_jit.ryo", src, "ok\n");
}

const TRY_NESTED_HEAP: &str = r#"struct Envelope:
	message: str
struct Payload:
	envelope: Envelope
	suffix: str
error Narrow(Payload)
error Other(int)
error Largest(str, str, str, str)
fn make(label: str) -> Payload:
	return Payload{envelope=Envelope{message=label + "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}, suffix="bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}
fn inner(fail: bool) -> Narrow!Payload:
	if fail:
		return Narrow(make("error"))
	return make("success")
fn outer(fail: bool) -> (Narrow | Other | Largest)!Payload:
	value = try inner(fail)
	return value
fn main():
	for i in range(0, 4):
		value = outer(i % 2 == 0) catch:
			Narrow(payload): payload
			_: make("other")
		print(value.envelope.message + "\n")
"#;
const TRY_NESTED_HEAP_OUTPUT: &str = "erroraaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\nsuccessaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\nerroraaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\nsuccessaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n";

#[test]
fn try_nested_heap_widening_and_early_return_run_jit() {
    assert_ryo_output(
        "try_nested_heap_jit.ryo",
        TRY_NESTED_HEAP,
        TRY_NESTED_HEAP_OUTPUT,
    );
}

#[test]
fn try_nested_heap_widening_and_early_return_run_aot() {
    assert_aot_output(
        "try_nested_heap_aot",
        TRY_NESTED_HEAP,
        TRY_NESTED_HEAP_OUTPUT,
    );
}
