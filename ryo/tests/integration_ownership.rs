mod common;
use common::*;

use tempfile::TempDir;

#[test]
fn inout_scalar_writeback() {
    // M8.3: an `inout int` parameter is mutated in the callee and the
    // change is visible to the caller via the write-back ABI.
    assert_ryo_output(
        "inout_scalar.ryo",
        "fn inc(inout x: int):\n\tx += 1\n\nfn main():\n\tmut c = 0\n\tinc(&c)\n\tinc(&c)\n\tprint(int_to_str(c))\n",
        "2",
    );
}

#[test]
fn inout_float_writeback() {
    // M8.3: `inout float` — exercises a non-int scalar width through
    // the write-back ABI (f64).
    assert_ryo_output(
        "inout_float.ryo",
        "fn scale(inout x: float):\n\tx += 1.5\n\nfn main():\n\tmut f = 1.0\n\tscale(&f)\n\tprint(float_to_str(f))\n",
        "2.5",
    );
}

#[test]
fn inout_early_return_still_writes_back() {
    // M8.3: the write-back chokepoint must fire on an EARLY `return`
    // (ReturnVoid) too, not just function fallthrough. Here `bump` exits
    // via `return` inside the `if` arm, so `a` must be 0+1+10 == 11.
    assert_ryo_output(
        "inout_early_ret.ryo",
        "fn bump(inout x: int, cond: bool):\n\tx += 1\n\tif cond:\n\t\tx += 10\n\t\treturn\n\tx += 100\n\nfn main():\n\tmut a = 0\n\tbump(&a, true)\n\tprint(int_to_str(a))\n",
        "11",
    );
}

#[test]
fn str_push_inout_str_builtin() {
    // M8.3: str_push(s: inout str, suffix: str) appends in place via the
    // __ryo_str_push runtime + the inout str write-back ABI.
    assert_ryo_output(
        "str_push.ryo",
        "fn main():\n\tmut s = \"hi\"\n\tstr_push(&s, \" there\")\n\tprint(s)\n",
        "hi there",
    );
}

#[test]
fn inout_aliasing_same_owner_rejected() {
    // M8.3b Rule 7: swap(&c, &c) passes one owner as two mutable borrows
    // in the same call — the ownership pass must reject with E0032.
    let temp_dir = TempDir::new().expect("temp");
    let code = "fn swap(inout a: int, inout b: int):\n\tmut t = a\n\ta = b\n\tb = t\n\nfn main():\n\tmut c = 5\n\tswap(&c, &c)\n";
    let test_file = create_test_file(temp_dir.path(), "inout_alias_bad.ryo", code);
    let output = run_ryo_command(&["run", "inout_alias_bad.ryo"], &test_file).expect("run");
    assert!(
        !output.status.success(),
        "swap(&c, &c) must be rejected as a Rule 7 violation"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("E0032"),
        "expected E0032 in stderr: {}",
        stderr
    );
}

#[test]
fn inout_aliasing_distinct_owners_ok() {
    // M8.3b Rule 7: swap(&a, &b) with distinct owners compiles and the
    // write-back ABI actually swaps the values.
    assert_ryo_output(
        "inout_alias_ok.ryo",
        "fn swap(inout a: int, inout b: int):\n\tmut t = a\n\ta = b\n\tb = t\n\nfn main():\n\tmut x = 5\n\tmut y = 7\n\tswap(&x, &y)\n\tprint(int_to_str(x))\n",
        "7",
    );
}

#[test]
fn inout_str_reassign_in_callee_writeback() {
    // Reassigning an inout str param inside the callee replaces the
    // caller's buffer. The replacement escapes via the write-back — it must
    // NOT be freed by the callee (UAF) nor flagged W0001, and the caller's
    // old buffer must be dropped exactly once.
    let temp_dir = TempDir::new().expect("temp");
    let code = "fn set(inout s: str):\n\ts = \"new\"\n\nfn main():\n\tmut s = \"old\"\n\tset(&s)\n\tprint(s)\n";
    let test_file = create_test_file(temp_dir.path(), "inout_str_set.ryo", code);
    let output = run_ryo_command(&["run", "inout_str_set.ryo"], &test_file).expect("run");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "STDERR: {}", stderr);
    assert!(
        !stderr.contains("W0001"),
        "inout param reassignment escapes — no dead-store warning expected: {}",
        stderr
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout, "new",
        "inout str reassignment should write back 'new', got: {}",
        stdout
    );
}

#[test]
fn inout_str_user_fn_and_reborrow_writeback() {
    // A user function taking `inout str`, mutating it via str_push
    // (a reborrow of the inout param). Exercises the general str inout ABI
    // plus the nested inout call inside the callee.
    assert_ryo_output(
        "inout_str_app.ryo",
        "fn app(inout s: str):\n\tstr_push(&s, \"!\")\n\nfn main():\n\tmut s = \"hi\"\n\tapp(&s)\n\tprint(s)\n",
        "hi!",
    );
}

#[test]
fn str_push_growth_beyond_capacity() {
    // Growing past capacity forces a realloc MOVE inside the
    // runtime — the caller's old buffer is freed there, so the caller must
    // not free the stale pre-call triple (double-free). Behavioral half of
    // the check; the sanitizer half is the `str_push_growth` ASan fixture.
    let temp_dir = TempDir::new().expect("temp");
    let suffix = "x".repeat(200);
    let code = format!(
        "fn main():\n\tmut s = \"hi\"\n\tstr_push(&s, \"{}\")\n\tprint(s)\n",
        suffix
    );
    let test_file = create_test_file(temp_dir.path(), "str_push_grow.ryo", &code);
    let output = run_ryo_command(&["run", "str_push_grow.ryo"], &test_file).expect("run");
    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let expected = format!("hi{suffix}");
    assert_eq!(
        stdout, expected,
        "str_push growth should print the full concatenated string",
    );
}

#[test]
fn inout_bool_writeback() {
    // Review coverage gap: `inout bool` — exercises the i8 scalar width
    // through the write-back ABI.
    assert_ryo_output(
        "inout_bool.ryo",
        "fn set(inout b: bool):\n\tb = true\n\nfn main():\n\tmut b = false\n\tset(&b)\n\tprint(bool_to_str(b))\n",
        "true",
    );
}

#[test]
fn inout_int_reborrow_chain() {
    // Review coverage gap: an inout param is itself a valid `&` target —
    // `twice` reborrows its own inout param into `inc`, twice.
    assert_ryo_output(
        "inout_reborrow.ryo",
        "fn inc(inout x: int):\n\tx += 1\n\nfn twice(inout x: int):\n\tinc(&x)\n\tinc(&x)\n\nfn main():\n\tmut c = 0\n\ttwice(&c)\n\tprint(int_to_str(c))\n",
        "2",
    );
}

#[test]
fn inout_fallthrough_return_writes_back() {
    // Review coverage gap: the multi-return-site test only exercised the
    // EARLY return. The fallthrough exit must write back too:
    // bump(&b, false) takes the fallthrough path, so b == 0+1+100 == 101.
    assert_ryo_output(
        "inout_fallthrough.ryo",
        "fn bump(inout x: int, cond: bool):\n\tx += 1\n\tif cond:\n\t\tx += 10\n\t\treturn\n\tx += 100\n\nfn main():\n\tmut b = 0\n\tbump(&b, false)\n\tprint(int_to_str(b))\n",
        "101",
    );
}

#[test]
fn str_reassign_inside_if_no_false_dead_store() {
    // Follow-up (pre-existing M8.1 bug): a str reassignment inside
    // a branch, read after the join, must not warn W0001 and must free
    // both buffers correctly (sanitizer half: `reassign_inside_if` ASan
    // fixture).
    let temp_dir = TempDir::new().expect("temp");
    let code = "fn main():\n\tmut s = \"a\"\n\tc = true\n\tif c:\n\t\ts = \"b\"\n\tprint(s)\n";
    let test_file = create_test_file(temp_dir.path(), "reassign_if.ryo", code);
    let output = run_ryo_command(&["run", "reassign_if.ryo"], &test_file).expect("run");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "STDERR: {}", stderr);
    assert!(
        !stderr.contains("W0001"),
        "s is read after the if — no dead-store warning expected: {}",
        stderr
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout, "b",
        "reassigned value should print 'b', got: {}",
        stdout
    );
}

#[test]
fn last_use_across_multiple_top_level_statements() {
    // Regression test: when an owned heap string is read in multiple
    // top-level statements, the last-use Free must anchor after the
    // *final* source-order read. A previous bug iterated the outer
    // statement loop in reverse while the inner operand walker ran
    // forward with overwriting `insert`, anchoring the Free after the
    // first read instead — turning the second read into use-after-free.
    // The printed value (7 * 6 == 42) must appear exactly once per
    // `print(s)` call.
    assert_ryo_output(
        "multi_read.ryo",
        "fn main():\n\ts: str = int_to_str(7 * 6)\n\tprint(s)\n\tprint(s)\n",
        "4242",
    );
}

#[test]
fn test_redundant_move_on_int_warns() {
    let temp_dir = TempDir::new().expect("temp dir");
    let code = "fn f(move x: int):\n\tprint(int_to_str(x))\n\nf(42)";
    let test_file = create_test_file(temp_dir.path(), "redundant_move.ryo", code);
    let output = run_ryo_command(&["run", "redundant_move.ryo"], &test_file).expect("run");
    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("W0002"),
        "expected W0002 in stderr: {}",
        stderr
    );
}

#[test]
fn test_move_on_str_no_warning() {
    let temp_dir = TempDir::new().expect("temp dir");
    let code = "fn f(move s: str):\n\tprint(s)\n\nf(\"hi\")";
    let test_file = create_test_file(temp_dir.path(), "move_on_str.ryo", code);
    let output = run_ryo_command(&["run", "move_on_str.ryo"], &test_file).expect("run");
    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("W0002"), "unexpected W0002: {}", stderr);
}

#[test]
fn test_use_after_move_assignment() {
    let temp_dir = TempDir::new().expect("temp");
    let code = "name: str = \"Alice\"\nother: str = name\nprint(name)";
    let test_file = create_test_file(temp_dir.path(), "uam_assign.ryo", code);
    let output = run_ryo_command(&["run", "uam_assign.ryo"], &test_file).expect("run");
    assert!(!output.status.success(), "expected compile error");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("E0020"),
        "expected E0020 in stderr: {}",
        stderr
    );
}

#[test]
fn test_move_out_of_borrowed_parameter() {
    let temp_dir = TempDir::new().expect("temp");
    let code = "fn process(data: str):\n\tother: str = data\n\nprocess(\"hi\")";
    let test_file = create_test_file(temp_dir.path(), "move_borrowed.ryo", code);
    let output = run_ryo_command(&["run", "move_borrowed.ryo"], &test_file).expect("run");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("E0021"), "expected E0021: {}", stderr);
}

#[test]
fn test_move_param_consumed_ok() {
    let temp_dir = TempDir::new().expect("temp");
    let code = "fn consume(move s: str):\n\tother: str = s\n\nconsume(\"hi\")";
    let test_file = create_test_file(temp_dir.path(), "move_ok.ryo", code);
    let output = run_ryo_command(&["run", "move_ok.ryo"], &test_file).expect("run");
    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_str_concat_then_use_ok() {
    let temp_dir = TempDir::new().expect("temp");
    let code = "a: str = \"hi\"\nb: str = \"there\"\nc: str = a + b\nprint(c)";
    let test_file = create_test_file(temp_dir.path(), "concat_ok.ryo", code);
    let output = run_ryo_command(&["run", "concat_ok.ryo"], &test_file).expect("run");
    assert!(output.status.success());
}

#[test]
fn test_str_concat_then_use_after_move_in_concat() {
    let temp_dir = TempDir::new().expect("temp");
    // a + b reads a and b as borrows (not moves); a is still valid.
    let code = "a: str = \"hi\"\nb: str = \"!\"\nc: str = a + b\nprint(a)";
    let test_file = create_test_file(temp_dir.path(), "concat_borrows.ryo", code);
    let output = run_ryo_command(&["run", "concat_borrows.ryo"], &test_file).expect("run");
    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_use_after_move_return() {
    let temp_dir = TempDir::new().expect("temp");
    let code = "fn make() -> str:\n\ts: str = \"hi\"\n\treturn s\n\nx: str = make()\nprint(x)";
    let test_file = create_test_file(temp_dir.path(), "ret_ok.ryo", code);
    let output = run_ryo_command(&["run", "ret_ok.ryo"], &test_file).expect("run");
    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_return_borrowed_param() {
    let temp_dir = TempDir::new().expect("temp");
    let code =
        "fn passthrough(s: str) -> str:\n\treturn s\n\nx: str = passthrough(\"hi\")\nprint(x)";
    let test_file = create_test_file(temp_dir.path(), "ret_borrowed.ryo", code);
    let output = run_ryo_command(&["run", "ret_borrowed.ryo"], &test_file).expect("run");
    assert!(!output.status.success(), "expected E0022");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("E0022"), "expected E0022: {}", stderr);
}

#[test]
fn test_use_after_move_param() {
    let temp_dir = TempDir::new().expect("temp");
    let code =
        "fn consume(move s: str):\n\tprint(s)\n\nname: str = \"Alice\"\nconsume(name)\nprint(name)";
    let test_file = create_test_file(temp_dir.path(), "uam_param.ryo", code);
    let output = run_ryo_command(&["run", "uam_param.ryo"], &test_file).expect("run");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("E0020"), "expected E0020: {}", stderr);
    let count = stderr.matches("E0020").count();
    assert_eq!(
        count, 1,
        "expected exactly one E0020, got {}: {}",
        count, stderr
    );
}

#[test]
fn test_use_after_move_in_vardecl_one_diag() {
    let temp_dir = TempDir::new().expect("temp");
    let code = r#"
fn consume(move s: str):
	print(s)

fn main():
	name: str = "Alice"
	consume(name)
	other: str = name
"#;
    let test_file = create_test_file(temp_dir.path(), "uam_vardecl_one.ryo", code);
    let output = run_ryo_command(&["run", "uam_vardecl_one.ryo"], &test_file).expect("run");
    assert!(
        !output.status.success(),
        "expected compile error, STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    let count = stderr.matches("E0020").count();
    assert_eq!(
        count, 1,
        "expected exactly one E0020, got {}: {}",
        count, stderr
    );
}

#[test]
fn test_use_after_move_in_return_one_diag() {
    let temp_dir = TempDir::new().expect("temp");
    let code = r#"
fn consume(move s: str):
	print(s)

fn forward(move s: str) -> str:
	consume(s)
	return s

fn main():
	x: str = forward("hi")
	print(x)
"#;
    let test_file = create_test_file(temp_dir.path(), "uam_ret_one.ryo", code);
    let output = run_ryo_command(&["run", "uam_ret_one.ryo"], &test_file).expect("run");
    assert!(
        !output.status.success(),
        "expected compile error, STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    let count = stderr.matches("E0020").count();
    assert_eq!(
        count, 1,
        "expected exactly one E0020, got {}: {}",
        count, stderr
    );
}

#[test]
fn test_e0020_message_includes_binding_name() {
    let temp_dir = TempDir::new().expect("temp");
    let code = r#"
fn consume(move s: str):
	print(s)

fn main():
	name: str = "Alice"
	consume(name)
	print(name)
"#;
    let test_file = create_test_file(temp_dir.path(), "e0020_label.ryo", code);
    let output = run_ryo_command(&["run", "e0020_label.ryo"], &test_file).expect("run");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("E0020"), "stderr: {}", stderr);
    assert!(
        stderr.contains("name"),
        "expected binding name in message: {}",
        stderr
    );
    assert!(
        stderr.contains("moved here") || stderr.contains("moved into"),
        "expected move-site note: {}",
        stderr
    );
}

#[test]
fn test_borrow_param_then_use_ok() {
    let temp_dir = TempDir::new().expect("temp");
    let code = "fn print_twice(s: str):\n\tprint(s)\n\tprint(s)\n\nprint_twice(\"hi\")";
    let test_file = create_test_file(temp_dir.path(), "borrow_ok.ryo", code);
    let output = run_ryo_command(&["run", "borrow_ok.ryo"], &test_file).expect("run");
    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_conditional_move_then_use_fails() {
    let temp_dir = TempDir::new().expect("temp");
    let code = r#"
fn consume(move s: str):
	print(s)

fn main():
	flag: bool = true
	name: str = "Alice"
	if flag:
		consume(name)
	print(name)
"#;
    let test_file = create_test_file(temp_dir.path(), "cond_move.ryo", code);
    let output = run_ryo_command(&["run", "cond_move.ryo"], &test_file).expect("run");
    assert!(
        !output.status.success(),
        "expected E0020 — name moved on then-branch"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("E0020"), "expected E0020: {}", stderr);
}

#[test]
fn test_conditional_move_both_branches_fails() {
    let temp_dir = TempDir::new().expect("temp");
    let code = r#"
fn consume(move s: str):
	print(s)

fn main():
	flag: bool = true
	name: str = "Alice"
	if flag:
		consume(name)
	else:
		consume(name)
	print(name)
"#;
    let test_file = create_test_file(temp_dir.path(), "cond_both.ryo", code);
    let output = run_ryo_command(&["run", "cond_both.ryo"], &test_file).expect("run");
    assert!(!output.status.success());
}

#[test]
fn test_conditional_use_inside_branch_ok() {
    let temp_dir = TempDir::new().expect("temp");
    let code = r#"
fn main():
	flag: bool = true
	name: str = "Alice"
	if flag:
		print(name)
	else:
		print(name)
"#;
    let test_file = create_test_file(temp_dir.path(), "cond_ok.ryo", code);
    let output = run_ryo_command(&["run", "cond_ok.ryo"], &test_file).expect("run");
    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_move_in_while_without_rebind_fails() {
    let temp_dir = TempDir::new().expect("temp");
    let code = r#"
fn consume(move s: str):
	print(s)

fn main():
	mut name: str = "Alice"
	mut i: int = 0
	while i < 3:
		consume(name)
		i = i + 1
"#;
    let test_file = create_test_file(temp_dir.path(), "loop_move.ryo", code);
    let output = run_ryo_command(&["run", "loop_move.ryo"], &test_file).expect("run");
    assert!(
        !output.status.success(),
        "expected E0020 — name moved without rebind"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("E0020"), "expected E0020: {}", stderr);
}

#[test]
fn test_borrow_in_while_ok() {
    let temp_dir = TempDir::new().expect("temp");
    let code = r#"
fn main():
	name: str = "Alice"
	mut i: int = 0
	while i < 3:
		print(name)
		i = i + 1
"#;
    let test_file = create_test_file(temp_dir.path(), "loop_borrow.ryo", code);
    let output = run_ryo_command(&["run", "loop_borrow.ryo"], &test_file).expect("run");
    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_move_then_rebind_in_loop_ok() {
    let temp_dir = TempDir::new().expect("temp");
    let code = r#"
fn consume(move s: str):
	print(s)

fn main():
	mut i: int = 0
	while i < 3:
		mut name: str = "Alice"
		consume(name)
		i = i + 1
"#;
    let test_file = create_test_file(temp_dir.path(), "loop_rebind.ryo", code);
    let output = run_ryo_command(&["run", "loop_rebind.ryo"], &test_file).expect("run");
    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_conditional_partial_rebind_then_use_fails() {
    let temp_dir = TempDir::new().expect("temp");
    let code = r#"
fn consume(move s: str):
	print(s)

fn main():
	mut s: str = "hello"
	flag: bool = true
	if flag:
		s = "world"
		consume(s)
	else:
		print("else")
	print(s)
"#;
    let test_file = create_test_file(temp_dir.path(), "cond_partial_rebind.ryo", code);
    let output = run_ryo_command(&["run", "cond_partial_rebind.ryo"], &test_file).expect("run");
    assert!(
        !output.status.success(),
        "expected E0020: STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("E0020"), "expected E0020: {}", stderr);
}

#[test]
fn test_loop_consume_then_rebind_ok() {
    let temp_dir = TempDir::new().expect("temp");
    let code = r#"
fn consume(move s: str):
	print(s)

fn main():
	mut name: str = "Alice"
	mut i: int = 0
	while i < 3:
		consume(name)
		name = "Bob"
		i = i + 1
"#;
    let test_file = create_test_file(temp_dir.path(), "loop_consume_rebind.ryo", code);
    let output = run_ryo_command(&["run", "loop_consume_rebind.ryo"], &test_file).expect("run");
    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_both_branches_consume_and_rebind_ok() {
    let temp_dir = TempDir::new().expect("temp");
    let code = r#"
fn consume(move s: str):
	print(s)

fn main():
	mut name: str = "Alice"
	flag: bool = true
	if flag:
		consume(name)
		name = "Bob"
	else:
		consume(name)
		name = "Charlie"
	print(name)
"#;
    let test_file = create_test_file(temp_dir.path(), "both_branches_rebind.ryo", code);
    let output = run_ryo_command(&["run", "both_branches_rebind.ryo"], &test_file).expect("run");
    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_dead_store_warning() {
    let temp_dir = TempDir::new().expect("temp");
    let code = "name: str = \"Alice\"\nprint(\"hello\")";
    let test_file = create_test_file(temp_dir.path(), "dead_store.ryo", code);
    let output = run_ryo_command(&["run", "dead_store.ryo"], &test_file).expect("run");
    // Warning, not error — exit success.
    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("W0001"),
        "expected W0001 in stderr: {}",
        stderr
    );
}

#[test]
fn test_no_dead_store_when_used() {
    let temp_dir = TempDir::new().expect("temp");
    let code = "name: str = \"Alice\"\nprint(name)";
    let test_file = create_test_file(temp_dir.path(), "live_store.ryo", code);
    let output = run_ryo_command(&["run", "live_store.ryo"], &test_file).expect("run");
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("W0001"), "unexpected W0001: {}", stderr);
}

#[test]
fn test_dead_store_warning_reassignment() {
    let temp_dir = TempDir::new().expect("temp");
    let code = "fn main():\n\tmut name: str = \"Alice\"\n\tprint(name)\n\tname = \"Bob\"\n\tprint(\"done\")\n";
    let test_file = create_test_file(temp_dir.path(), "reassign_dead.ryo", code);
    let output = run_ryo_command(&["run", "reassign_dead.ryo"], &test_file).expect("run");
    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("W0001"),
        "expected W0001 in stderr: {}",
        stderr
    );
}

// Milestone 8.1c Task 7: codegen emits unconditional ryo_str_free
// calls scheduled by the ownership pass. These tests don't grep
// the CLIF dump (Cranelift renames runtime functions to opaque
// `u0:N` identifiers in its text format); they instead exercise
// the runtime path end-to-end. If a Free fires too early, the
// `write` syscall reads from freed memory and the printed output
// is corrupted; if a Free is missed, ASan (Task 11) catches the
// leak.

#[test]
fn str_var_assignment_runs_clean() {
    // Last-use Free anchored after the Var read inside `print(s)`.
    // The Free must land after the syscall has copied the bytes;
    // otherwise stdout is garbled.
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\ts: str = \"hello\"\n\tprint(s)\n";
    let test_file = create_test_file(temp_dir.path(), "free_var.ryo", code);
    let output =
        run_ryo_command(&["run", "free_var.ryo"], &test_file).expect("Failed to run ryo command");
    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("hello"),
        "stdout should contain 'hello': {}",
        stdout
    );
}

#[test]
fn str_concat_runs_clean_after_free() {
    // Concat allocates a fresh buffer and the schedule frees the
    // operand temporaries plus the named-binding owner. If any
    // Free fires before `print` reads the concatenation, the
    // allocator reuses the slab and `Hello, World!` comes out
    // garbled.
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code =
        "fn main():\n\ta: str = \"Hello, \"\n\tb: str = \"World!\"\n\tc: str = a + b\n\tprint(c)\n";
    let test_file = create_test_file(temp_dir.path(), "free_concat.ryo", code);
    let output = run_ryo_command(&["run", "free_concat.ryo"], &test_file)
        .expect("Failed to run ryo command");
    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Hello, World!"),
        "stdout should contain 'Hello, World!': {}",
        stdout
    );
}

#[test]
fn mut_str_reassign_runs_clean() {
    // Milestone 8.1c Task 8: a `mut` str binding reassigned with a
    // new literal must free the old allocation before overwriting
    // the FatLocals triple. If the Free is missing, ASan (Task 11)
    // catches the leak; if it fires too late or reads from stale
    // inst_values, stdout is corrupted or the run aborts.
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tmut s: str = \"hello\"\n\ts = \"world\"\n\tprint(s)\n";
    let test_file = create_test_file(temp_dir.path(), "free_reassign.ryo", code);
    let output = run_ryo_command(&["run", "free_reassign.ryo"], &test_file)
        .expect("Failed to run ryo command");
    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("world"),
        "stdout should contain 'world': {}",
        stdout
    );
    // stdout is exactly the program's own output, so a naive
    // `!stdout.contains("hello")` check is safe — there is no echoed
    // source to false-positive on. The program printed exactly
    // `"world"` — not the old `"hello"` value, and not garbled bytes
    // from a use-after-free.
    assert!(
        !stdout.contains("hello"),
        "runtime stdout should not leak old value 'hello': {}",
        stdout
    );
}

#[test]
fn conditional_move_runs_clean() {
    // Milestone 8.1c Task 9: branch-gated Frees for owners that end
    // Valid in some arms and Moved in others. With `flag = false`
    // we take the else-arm: `s` stays Valid through `print(s)` and
    // must be freed by the conditional Free anchored at the last
    // statement of the else-arm. The then-arm's `consume(s)` move
    // means the post-merge state stamps `s` as Moved, so the
    // function-exit last-use pass does NOT schedule a Free — the
    // branch-gated entry is the only thing keeping the allocation
    // from leaking under ASan.
    //
    // Heap-backed initializer (int_to_str returns an owned heap string)
    // so that ryo_str_free in the else-arm actually frees a real
    // allocation. A rodata-backed `"hello"` literal would have cap=0,
    // making ryo_str_free a no-op — the test would pass even if the
    // conditional-Free emission were missing entirely.
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "\
fn consume(move s: str):
\tprint(s)

fn main():
\ts: str = int_to_str(42)
\tflag: bool = false
\tif flag:
\t\tconsume(s)
\telse:
\t\tprint(s)
";
    let test_file = create_test_file(temp_dir.path(), "free_cond.ryo", code);
    let output =
        run_ryo_command(&["run", "free_cond.ryo"], &test_file).expect("Failed to run ryo command");
    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // stdout is exactly the program's own output.
    let runtime_output = String::from_utf8_lossy(&output.stdout);
    assert!(
        runtime_output.contains("42"),
        "runtime stdout should contain '42': {}",
        runtime_output
    );
}

#[test]
fn break_emits_pre_loop_owner_free() {
    // Milestone 8.1c Task 10: pre-loop owners still Valid at a
    // `break` site need a Free emitted before the Cranelift `jump`,
    // because (1) the post-stmt sweep skips Free emission on
    // terminating statements, and (2) the post-loop region — where
    // the function-exit last-use pass would normally fire — is
    // skipped entirely once we jump out.
    //
    // `s` is a pre-loop owner whose only read is inside the loop
    // body. The `break` short-circuits the rest of the iteration;
    // without the loop-exit-Free pass the heap allocation would leak.
    //
    // Heap-backed initializer (int_to_str returns an owned heap
    // string) so that ryo_str_free actually frees a real allocation.
    // A rodata-backed string literal would have cap=0, making
    // ryo_str_free a no-op — the test would still pass even if the
    // break-site Free were missing entirely. ASan in Task 11 will
    // ultimately validate the Free actually happened.
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "\
fn main():
\ts: str = int_to_str(7)
\tmut i: int = 0
\twhile i < 10:
\t\tprint(s)
\t\tif i == 0:
\t\t\tbreak
\t\ti = i + 1
";
    let test_file = create_test_file(temp_dir.path(), "free_break.ryo", code);
    let output =
        run_ryo_command(&["run", "free_break.ryo"], &test_file).expect("Failed to run ryo command");
    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // stdout is exactly the program's own output.
    let runtime_output = String::from_utf8_lossy(&output.stdout);
    assert!(
        runtime_output.contains("7"),
        "runtime stdout should contain '7': {}",
        runtime_output
    );
}

#[test]
fn cross_function_str_return_runs_clean() {
    // Reproduces the bug where program-wide free_schedule caused
    // frees from one function to fire in another at numerically-
    // matching TirRefs. The pre-fix output garbled msg's bytes
    // before print(msg) ran.
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "\
fn greet(name: str) -> str:
\treturn \"Hello, \" + name + \"!\"

fn main():
\tuser: str = \"Alice\"
\tmsg: str = greet(user)
\tprint(user)
\tprint(msg)
";
    let test_file = create_test_file(temp_dir.path(), "cross_fn_str.ryo", code);
    let output = run_ryo_command(&["run", "cross_fn_str.ryo"], &test_file)
        .expect("Failed to run ryo command");
    assert!(
        output.status.success(),
        "ryo run should succeed. STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // stdout is exactly the program's own output.
    let runtime_output = String::from_utf8_lossy(&output.stdout);
    assert!(
        runtime_output.contains("Alice"),
        "expected 'Alice' in runtime stdout, got: {:?}",
        runtime_output
    );
    assert!(
        runtime_output.contains("Hello, Alice!"),
        "expected 'Hello, Alice!' in runtime stdout, got: {:?}",
        runtime_output
    );
}

#[test]
fn branch_ids_do_not_collide_after_loop() {
    // Regression for Bug 4 (M8.1c): merge_branches must take the
    // max next_branch_id across branches so that BranchIds minted
    // inside a loop body's `if` survive the post-loop merge. If the
    // allocator rolled backward, the post-loop `if` below would
    // reuse those BranchIds and collide in codegen's branch_blocks
    // map (or mis-gate Frees), causing either a Cranelift panic or
    // a runtime double-free.
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn consume(move x: str):\n\tprint(x)\n\nfn main():\n\tmut i: int = 0\n\twhile i < 1:\n\t\tif i == 0:\n\t\t\ts1: str = int_to_str(1)\n\t\t\tconsume(s1)\n\t\telse:\n\t\t\ts2: str = int_to_str(2)\n\t\t\tconsume(s2)\n\t\ti += 1\n\tflag: bool = true\n\tif flag:\n\t\ts3: str = int_to_str(3)\n\t\tconsume(s3)\n\telse:\n\t\ts4: str = int_to_str(4)\n\t\tconsume(s4)\n";
    let test_file = create_test_file(temp_dir.path(), "branch_id_collision.ryo", code);

    let output = run_ryo_command(&["run", "branch_id_collision.ryo"], &test_file)
        .expect("Failed to run ryo command");

    assert!(
        output.status.success(),
        "ryo run should succeed without branch_blocks collision. STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn heap_str_last_use_in_loop_slice_comparison() {
    // A heap-owned str whose last use is a slice comparison inside a
    // loop-condition-guarded `if` must stay alive until the loop exits —
    // the Free anchor is the loop statement, not the in-loop read.
    let temp_dir = TempDir::new().expect("temp");
    let code = "fn main():\n\tmut s: str = \"\"\n\tfor i in range(0, 8):\n\t\tstr_push(&s, \"fox \")\n\tmut i = 0\n\tmut count = 0\n\twhile i + 3 <= s.len():\n\t\tif s[i:i+3] == \"fox\":\n\t\t\tcount += 1\n\t\ti += 1\n\tassert(count == 8, \"count must be 8\")\n\tprint(\"ok\\n\")\n";
    let test_file = create_test_file(temp_dir.path(), "loop_slice_cmp.ryo", code);
    let output = run_ryo_command(&["run", "loop_slice_cmp.ryo"], &test_file).expect("run");
    assert!(
        output.status.success(),
        "loop slice comparison on heap str should succeed. STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn slice_then_reassign_while_view_live_rejected() {
    // P2 freeze (final spec §3.2): `v`'s last use is after the
    // reassign-concat, so the slice projection is live at `s = s + ...`.
    // The consuming-concat fast path (in-place append) must not bypass
    // this check — reassignment of a projected owner stays E0035.
    let temp_dir = TempDir::new().expect("temp");
    let code = "fn main():\n\tmut s: str = int_to_str(12345)\n\tv = s[1:3]\n\ts = s + \"678901234567890123456789\"\n\tprint(v)\n";
    let test_file = create_test_file(temp_dir.path(), "freeze_reassign_concat.ryo", code);
    let output = run_ryo_command(&["run", "freeze_reassign_concat.ryo"], &test_file).expect("run");
    assert!(!output.status.success(), "expected compile error");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("E0035"), "expected E0035: {}", stderr);
    assert!(
        stderr.contains("cannot mutate `s` while a slice of it is live"),
        "expected freeze message: {}",
        stderr
    );
}

#[test]
fn heap_str_last_use_in_inline_assert() {
    // `assert(s.len() == ...)` as the last use of a concat-built string:
    // the desugared if's condition read must count as a use (no W0001,
    // no Free before the assert).
    let temp_dir = TempDir::new().expect("temp");
    let code = "fn main():\n\tmut s: str = \"\"\n\tfor i in range(0, 25):\n\t\ts = s + \"fox \"\n\tassert(s.len() == 100, \"len must be 100\")\n\tprint(\"ok\\n\")\n";
    let test_file = create_test_file(temp_dir.path(), "inline_assert.ryo", code);
    let output = run_ryo_command(&["run", "inline_assert.ryo"], &test_file).expect("run");
    assert!(
        output.status.success(),
        "inline assert on heap str should succeed. STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("W0001"),
        "assert condition read must count as a use (no W0001), got: {}",
        stderr
    );
}

#[test]
fn last_use_in_returning_arm_fallthrough_runs_clean() {
    // The owner's only read is inside a returning if-arm: the taken
    // path frees in-arm (plus the return epilogue), the not-taken path
    // must still free at the branch exit — the fallthrough reaches the
    // function end with the owner live and no in-arm anchor firing.
    // Heap-backed initializer (runtime concat, > SSO inline capacity)
    // so ryo_str_free releases a real allocation. The Valgrind/ASan
    // fixtures of the same name are the hard leak net; this is the
    // behavioral guard.
    assert_ryo_output(
        "last_use_ret_arm.ryo",
        "fn f(take: bool):\n\ts: str = int_to_str(42) + \"abcdefghijklmnopqrstuvwxyz0123456789\"\n\tif take:\n\t\tprint(s)\n\t\treturn\n\nfn main():\n\tf(true)\n\tf(false)\n\tprint(\"done\")\n",
        "42abcdefghijklmnopqrstuvwxyz0123456789done",
    );
}

#[test]
fn last_use_in_fallthrough_arm_sibling_returns_runs_clean() {
    // Mirrored shape: the last use sits in an arm that falls through
    // while a sibling arm returns — the implicit-else path reaches the
    // merge with the owner live and must free at the branch exit.
    assert_ryo_output(
        "last_use_sibling_ret.ryo",
        "fn f(x: int):\n\ts: str = int_to_str(42) + \"abcdefghijklmnopqrstuvwxyz0123456789\"\n\tif x == 1:\n\t\tprint(s)\n\telif x == 2:\n\t\treturn\n\nfn main():\n\tf(1)\n\tf(2)\n\tf(3)\n\tprint(\"done\")\n",
        "42abcdefghijklmnopqrstuvwxyz0123456789done",
    );
}

#[test]
fn elif_heap_reassign_every_arm_read_after_runs_clean() {
    // `mut` binding declared before an if/elif chain, reseated
    // with a heap value in EVERY arm, and read again after the chain.
    // Each arm's reseated value has its last use inside that arm, so
    // the last-use pass re-anchored every value's Free to the branch
    // exit — N frees all lowering through the same home-slot redirect.
    // The non-last-write targets failed codegen's stale-target redirect
    // filter and fell back to arm-local cached values that do not
    // dominate the merge block: "Verifier errors: uses value from
    // non-dominating inst". The redundant exit Frees are now
    // suppressed; the merge-seated pre-branch owner's Free (anchored
    // after the post-branch read, slot-redirected) releases the buffer
    // on every path.
    assert_ryo_output(
        "elif_heap_reassign.ryo",
        "fn make(tag: int) -> str:\n\treturn int_to_str(tag) + \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\n\nfn main():\n\tmut s = make(0)\n\tif false:\n\t\ts = make(1)\n\t\tprint(s)\n\telif true:\n\t\ts = make(2)\n\t\tprint(s)\n\tprint(s)\n",
        "2aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa2aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    );
}

#[test]
fn if_heap_reassign_read_after_no_garbage() {
    // Companion UAF shape (same root cause, silently corrupt before):
    // one arm reseats the binding, which is read again after the
    // branch. The arm value's exit-anchored Free fired at the merge and
    // released the slot the post-branch read still needed (garbage
    // output / double free with the pre-branch owner's Free).
    assert_ryo_output(
        "if_heap_reassign_read_after.ryo",
        "fn make(tag: int) -> str:\n\treturn int_to_str(tag) + \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\n\nfn main():\n\tmut s = make(0)\n\tif false:\n\t\ts = make(1)\n\t\tprint(s)\n\tprint(s)\n",
        "0aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    );
}

#[test]
fn if_heap_reassign_partial_elif_read_after_no_garbage() {
    // The elif twin of the partial-reseat shape: only the elif arm
    // reseats; the binding is read after the chain.
    assert_ryo_output(
        "if_elif_heap_reassign_partial.ryo",
        "fn make(tag: int) -> str:\n\treturn int_to_str(tag) + \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\n\nfn main():\n\tmut s = make(0)\n\tif false:\n\t\tprint(s)\n\telif true:\n\t\ts = make(2)\n\t\tprint(s)\n\tprint(s)\n",
        "2aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa2aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    );
}

#[test]
fn sequential_if_heap_reassign_runs_clean() {
    // Sequential single-arm reseats of one binding, no read after.
    // Each reseated value's Free re-anchored to its branch exit; the
    // later exit's Free (slot-redirected) covers every path that passed
    // the earlier one, so the earlier exit Frees are suppressed too.
    assert_ryo_output(
        "seq_if_heap_reassign.ryo",
        "fn make(tag: int) -> str:\n\treturn int_to_str(tag) + \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\n\nfn main():\n\tmut s = make(0)\n\tif true:\n\t\ts = make(1)\n\t\tprint(s)\n\tif false:\n\t\ts = make(2)\n\t\tprint(s)\n\tprint(\"done\")\n",
        "1aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaadone",
    );
}

#[test]
fn returning_arm_reseat_after_elif_chain_runs_clean() {
    // Companion to the every-arm reseat case above: every arm of the
    // if/elif chain reseats the binding with a heap value and then
    // RETURNS, so no arm reaches the branch exit. The arm values' exit-anchored Frees never fire on
    // the return paths; the return epilogue must free each reseated
    // value at its own return and must NOT free the pre-reassign owner
    // there — the arm's reassign already released it via the
    // displacement Free, and an epilogue Free for it double-freed
    // (runtime abort). Only the fall-through path still owns the
    // pre-branch buffer, freed after the post-branch read.
    assert_ryo_output(
        "returning_arm_reseat.ryo",
        "fn make(tag: int) -> str:\n\treturn int_to_str(tag) + \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\n\nfn f(c1: bool, c2: bool):\n\tmut s = make(0)\n\tif c1:\n\t\ts = make(1)\n\t\tprint(s)\n\t\treturn\n\telif c2:\n\t\ts = make(2)\n\t\tprint(s)\n\t\treturn\n\tprint(s)\n\nfn main():\n\tf(true, false)\n\tprint(\"|\")\n\tf(false, true)\n\tprint(\"|\")\n\tf(false, false)\n\tprint(\"\\n\")\n",
        "1aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa|2aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa|0aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
    );
}

#[test]
fn loop_reassign_heap_early_return_runs_clean() {
    // Heap-valued variant of the early-return-after-loop-reassign
    // guard: a `mut` binding reseated inside a loop, then returned
    // directly on one path and read into a concat on another. The
    // first-wins loop merge seats the pre-loop owner at the return
    // read, so the walk's consume record names THAT owner while the
    // buffer physically leaving is the in-loop reseated value — a
    // still-`Valid` owner of the same binding. The return epilogue
    // must destroy NEITHER (the slot content hands to the caller), and
    // the zero-iteration path must still free the pre-loop buffer at
    // the fallthrough return's slot-redirected Free. Pre-fix the
    // epilogue freed the reseated value at the direct return (the
    // caller received freed memory — garbage output, then abort) and
    // the all-arm-reseat shapes above failed the Cranelift verifier.
    assert_ryo_output(
        "loop_reassign_heap_early_ret.ryo",
        "fn make(tag: int) -> str:\n\treturn int_to_str(tag) + \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\n\nfn f(n: int) -> str:\n\tmut s = make(0)\n\tmut i = 0\n\twhile i < n:\n\t\ts = make(1)\n\t\ti += 1\n\tif n > 1:\n\t\treturn s\n\treturn s + \"!\"\n\nfn main():\n\tprint(f(0))\n\tprint(\"|\")\n\tprint(f(1))\n\tprint(\"|\")\n\tprint(f(3))\n\tprint(\"\\n\")\n",
        "0aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa!|1aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa!|1aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
    );
}

#[test]
fn reseat_fallthrough_skipped_read_arm_runs_clean() {
    // Pre-branch `mut` binding, conditional reseat, read inside a
    // later branch's arm, fall-through exit. The pre-branch owner's
    // last-use Free re-anchors to the later branch's exit, but the
    // reassign's displacement Free already released that buffer on the
    // taken paths — codegen's stale-target redirect filter rejected the
    // redirect and the cached-value fallback double-freed (valgrind:
    // Invalid free on f(true, false)). The exit Free now targets the
    // binding's last write so the redirect frees the slot's path-correct
    // content exactly once on every path. Under plain execution the
    // double free is silent; the valgrind fixture
    // (valgrind_reseat_fallthrough_skipped_read_arm) is the regression
    // guard.
    assert_ryo_output(
        "reseat_fallthrough_skipped_read_arm.ryo",
        "fn make(tag: int) -> str:\n\treturn int_to_str(tag) + \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\n\nfn f(c1: bool, c2: bool) -> int:\n\tmut s = make(0)\n\tif c1:\n\t\ts = make(1)\n\tif c2:\n\t\tprint(s)\n\t\treturn 0\n\treturn 1\n\nfn main():\n\tf(true, true)\n\tf(false, true)\n\tf(true, false)\n\tf(false, false)\n\tprint(\"done\\n\")\n",
        "1aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa0aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaadone\n",
    );
}

#[test]
fn heap_return_from_loop_jit() {
    // R4-C: a HEAP (> inline cap) value returned from inside a loop
    // must reach the caller intact — the return transfer discharges
    // the obligation; the callee must never release the transferred
    // buffer (the pre-fix schedule freed it after the sret store —
    // caller-visible corruption). 200-char payload forces a heap
    // allocation (inline cap is well below that).
    let long = "z".repeat(200);
    let code = format!(
        "fn f(flag: bool) -> str:\n\tmut i = 0\n\twhile flag:\n\t\tv = \"{long}\" + \"y\"\n\t\tif i == 1:\n\t\t\treturn v\n\t\ti += 1\n\treturn \"x\"\n\nfn main():\n\tprint(f(true))\n\tprint(\"\\n\")\n"
    );
    let expected = format!("{long}y\n");
    assert_ryo_output("heap_return_from_loop.ryo", &code, &expected);
}

#[test]
fn heap_return_from_loop_aot() {
    // AOT twin of the loop-return pin: the sret store ordering must be
    // correct in the linked binary too.
    let long = "z".repeat(200);
    let code = format!(
        "fn f(flag: bool) -> str:\n\tmut i = 0\n\twhile flag:\n\t\tv = \"{long}\" + \"y\"\n\t\tif i == 1:\n\t\t\treturn v\n\t\ti += 1\n\treturn \"x\"\n\nfn main():\n\tprint(f(true))\n\tprint(\"\\n\")\n"
    );
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "heap_ret_loop.ryo", &code);
    let build_output = run_ryo_build(&test_file, temp_dir.path());
    assert!(
        build_output.status.success(),
        "ryo build failed. STDERR: {}",
        String::from_utf8_lossy(&build_output.stderr)
    );
    let binary_path = exe_path(temp_dir.path(), "heap_ret_loop");
    let run_output = std::process::Command::new(&binary_path)
        .output()
        .expect("Failed to execute compiled binary");
    assert!(
        run_output.status.success(),
        "compiled binary should exit 0. stderr: {}",
        String::from_utf8_lossy(&run_output.stderr)
    );
    let stdout = String::from_utf8_lossy(&run_output.stdout);
    assert_eq!(stdout, format!("{long}y\n"), "AOT stdout mismatch");
}

const CONCAT_AFTER_BREAK: &str = "\
fn main():
\tmut total = \"😀😀😀😀😀😀😀\"
\twhile true:
\t\ttotal = total + \"🦊\"
\t\tbreak
\t\tif total.len() > 5000:
\t\t\tbreak
\tprint(total)
\tprint(\"\\n\")
";

#[test]
fn double_free_str_concat_after_break() {
    // Pre-fix: SIGTRAP/SIGABRT (double free) with no output. The
    // explicit print("\n") is the only newline — print appends none.
    let temp_dir = tempfile::TempDir::new().expect("create source directory");
    let test_file = create_test_file(
        temp_dir.path(),
        "bug_double_free_str_concat_after_break.ryo",
        CONCAT_AFTER_BREAK,
    );
    let output = run_ryo_command(
        &["run", "bug_double_free_str_concat_after_break.ryo"],
        &test_file,
    )
    .expect("run ryo");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.stdout.len(), 33, "stdout length mismatch");
    assert_eq!(stdout, "😀😀😀😀😀😀😀🦊\n");
}

const CONCAT_IN_LOOP: &str = "\
fn main():
\tmut total = \"😀😀😀😀😀😀😀\"
\twhile true:
\t\ttotal = total + \"🦊\"
\t\tif total.len() > 5000:
\t\t\tbreak
\tprint(total)
\tprint(\"\\n\")
";

#[test]
fn double_free_str_concat_in_loop() {
    // Pre-fix: heap corruption / double free while growing the
    // accumulator in the loop. Post-fix the loop breaks once
    // total.len() > 5000: 7x😀 (28 B) + 1244x🦊 (4976 B) + "\n" =
    // 5005 bytes exactly.
    let temp_dir = tempfile::TempDir::new().expect("create source directory");
    let test_file = create_test_file(
        temp_dir.path(),
        "bug_double_free_str_concat_in_loop.ryo",
        CONCAT_IN_LOOP,
    );
    let output = run_ryo_command(
        &["run", "bug_double_free_str_concat_in_loop.ryo"],
        &test_file,
    )
    .expect("run ryo");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.stdout.len(), 5005, "stdout length mismatch");
    assert!(stdout.starts_with("😀😀😀😀😀😀😀"), "7x😀 prefix");
    assert!(stdout.ends_with("🦊\n"), "🦊 + newline suffix");
    assert_eq!(stdout, format!("{}\n", "😀".repeat(7) + &"🦊".repeat(1244)));
}

const CODEGEN_FREE_NO_VALUEREPR: &str = "\
fn adv(inout p: int):
\tp += 1

fn slice_at(s: str, at: int) -> str:
\treturn str(s[at:at + 2])

fn find(b: bytesview, s: str, key: str) -> str:
\tmut p = 0
\twhile true:
\t\tif p >= b.len():
\t\t\treturn \"<none>\"
\t\tk = slice_at(s, p)
\t\tadv(&p)
\t\tv = slice_at(s, p)
\t\tif k == key:
\t\t\treturn v
\t\tadv(&p)
\treturn \"<none>\"

fn main():
\tdoc = \"hello world\"
\tb = doc.as_bytes()
\tprint(find(b, doc, \"he\"))
\tprint(\"\\n\")
";

#[test]
fn codegen_free_no_valuerepr() {
    // Pre-fix: codegen aborted with "ownership pass scheduled Free ...
    // but no ValueRepr cached". The lookup key is "he" (not the
    // original "el") so the match prints "el" — the original key never
    // matched because p steps by 2 over even offsets only, and the
    // program then panics out-of-bounds; flipping it exercises the
    // previously-crashing `return v` path. That is intentional, keep it.
    let temp_dir = tempfile::TempDir::new().expect("create source directory");
    let test_file = create_test_file(
        temp_dir.path(),
        "bug_codegen_free_no_valuerepr.ryo",
        CODEGEN_FREE_NO_VALUEREPR,
    );
    let output = run_ryo_command(&["run", "bug_codegen_free_no_valuerepr.ryo"], &test_file)
        .expect("run ryo");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout, "el\n");
}

const RETURN_ACCUMULATOR_IN_LOOP: &str = "\
fn build(n: int) -> str:
\tmut out = \"[\"
\tmut i = 0
\twhile i < n:
\t\tstr_push(&out, \"x\")
\t\tif i == 2:
\t\t\treturn out
\t\ti += 1
\treturn out

fn main():
\tprint(build(5))
\tprint(\"\\n\")
";

#[test]
fn e0020_return_accumulator_in_loop() {
    // Pre-fix: three bogus E0020 "use of moved value" errors — the
    // flow-insensitive move analysis treated the in-loop `return out`
    // as moving the accumulator on every iteration.
    let temp_dir = tempfile::TempDir::new().expect("create source directory");
    let test_file = create_test_file(
        temp_dir.path(),
        "bug_e0020_return_accumulator_in_loop.ryo",
        RETURN_ACCUMULATOR_IN_LOOP,
    );
    let output = run_ryo_command(
        &["run", "bug_e0020_return_accumulator_in_loop.ryo"],
        &test_file,
    )
    .expect("run ryo");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout, "[xxx\n");
}

const EARLY_RETURN_AFTER_LOOP_REASSIGN: &str = "\
fn f(n: int) -> str:
\tmut s = \"aaaaaaaaaaaaaaaa\"
\tmut i = 0
\twhile i < n:
\t\ts = \"bbbbbbbbbbbbbbbb\"
\t\ti += 1
\tif n > 1:
\t\treturn s
\treturn s + \"!\"

fn main():
\tprint(f(3))
\tprint(\"\\n\")
";

#[test]
fn early_return_after_loop_reassign() {
    // Pre-fix (binding-covering without the terminator-anchor
    // exclusion): the inner-return-anchored Free counted as covering
    // the outer return's epilogue Free, codegen's leak-direction
    // assert tripped ("frees anchored to unmaterialized instructions
    // were dropped"), and the debug compiler aborted on this natural
    // guard shape.
    let temp_dir = tempfile::TempDir::new().expect("create source directory");
    let test_file = create_test_file(
        temp_dir.path(),
        "bug_early_return_after_loop_reassign.ryo",
        EARLY_RETURN_AFTER_LOOP_REASSIGN,
    );
    let output = run_ryo_command(
        &["run", "bug_early_return_after_loop_reassign.ryo"],
        &test_file,
    )
    .expect("run ryo");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout, "bbbbbbbbbbbbbbbb\n");
}

#[test]
fn displaced_static_literal_is_not_a_heap_release() {
    assert_ryo_output(
        "loop_static_displacement.ryo",
        "fn main():\n\tmut i = 0\n\twhile i < 3:\n\t\tmut s = \"ab\"\n\t\ts = s + \"🦊🦊🦊🦊🦊🦊\"\n\t\ti += 1\n\t\tif i == 2:\n\t\t\tbreak\n\tprint(\"ok\\n\")\n",
        "ok\n",
    );
}

#[test]
fn early_return_under_shadow_writes_back_the_declared_parameter() {
    assert_ryo_output(
        "inout_parameter_shadow.ryo",
        include_str!("fixtures/inout_parameter_shadow.ryo"),
        "innerouter_0123456789012345678901234567890123456789!",
    );
}

#[test]
fn break_concat_after_break_e2e() {
    // A break in one loop must not schedule frees of concat temps
    // produced in a later loop (the jump-exit defensive emit used to
    // crash the compiler — debug panic in the validator, "cleanup
    // target has no storage" in release).
    assert_ryo_output(
        "concat_after_break.ryo",
        "fn main():\n\tmut i = 0\n\twhile i < 10:\n\t\tbreak\n\tmut result = \"x\"\n\tmut j = 0\n\twhile j < 3:\n\t\tresult = result + \"0\"\n\t\tj += 1\n\tprint(result)\n",
        "x000",
    );
}

#[test]
fn fixed7_concat_after_break_e2e() {
    // Fixed-7 float formatter, inlined verbatim so the test is
    // self-contained (the original reproducer was never committed).
    assert_ryo_output(
        "fixed7_after_break.ryo",
        "fn pow10_int(exp: int) -> int:\n\tmut r = 1\n\tmut i = 0\n\twhile i < exp:\n\t\tr = r * 10\n\t\ti += 1\n\treturn r\n\nfn format_fixed_7(v: float) -> str:\n\ts = float_to_str(v)\n\tb = s.as_bytes()\n\tmut sign = \"\"\n\tmut start = 0\n\tif b[0] == 45:\n\t\tsign = \"-\"\n\t\tstart = 1\n\tmut dot = -1\n\tmut i = start\n\twhile i < b.len():\n\t\tif b[i] == 46:\n\t\t\tdot = i\n\t\t\tbreak\n\t\ti += 1\n\tmut int_str = \"\"\n\tmut frac_str = \"\"\n\tif dot < 0:\n\t\tint_str = str(s[start:b.len()])\n\t\tfrac_str = \"\"\n\telse:\n\t\tint_str = str(s[start:dot])\n\t\tfrac_str = str(s[dot + 1:b.len()])\n\tmut int_val = 0\n\tmut k = 0\n\twhile k < int_str.len():\n\t\tc = int_str.as_bytes()[k]\n\t\tint_val = int_val * 10 + (c - 48)\n\t\tk += 1\n\tmut frac_len = frac_str.len()\n\tmut frac_val = 0\n\tif frac_len > 0:\n\t\tmut take = frac_len\n\t\tif take > 8:\n\t\t\ttake = 8\n\t\tk = 0\n\t\twhile k < take:\n\t\t\tc = frac_str.as_bytes()[k]\n\t\t\tfrac_val = frac_val * 10 + (c - 48)\n\t\t\tk += 1\n\t\tif frac_len > 7:\n\t\t\tdigit8 = frac_val % 10\n\t\t\tfrac_val = frac_val / 10\n\t\t\tif digit8 >= 5:\n\t\t\t\tfrac_val += 1\n\t\telif frac_len < 7:\n\t\t\tfrac_val = frac_val * pow10_int(7 - frac_len)\n\tif frac_val >= 10000000:\n\t\tfrac_val = 0\n\t\tint_val += 1\n\tmut result = sign + int_to_str(int_val) + \".\"\n\tmut frac_out = int_to_str(frac_val)\n\tmut zeros = 7 - frac_out.len()\n\twhile zeros > 0:\n\t\tresult = result + \"0\"\n\t\tzeros -= 1\n\tresult = result + frac_out\n\treturn result\n\nfn main():\n\tprint(format_fixed_7(3.14159))\n\tprint(\"\\n\")\n",
        "3.1415900\n",
    );
}
