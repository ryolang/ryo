mod common;
use common::*;

use tempfile::TempDir;

#[test]
fn inout_scalar_writeback() {
    // M8.3: an `inout int` parameter is mutated in the callee and the
    // change is visible to the caller via the write-back ABI.
    let temp_dir = TempDir::new().expect("temp");
    let code = "fn inc(inout x: int):\n\tx += 1\n\nfn main():\n\tmut c = 0\n\tinc(&c)\n\tinc(&c)\n\tprint(int_to_str(c))\n";
    let test_file = create_test_file(temp_dir.path(), "inout_scalar.ryo", code);
    let output = run_ryo_command(&["run", "inout_scalar.ryo"], &test_file).expect("run");
    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("[Codegen]\n2[Result]"),
        "inout write-back should print 2, got: {}",
        stdout
    );
}

#[test]
fn inout_float_writeback() {
    // M8.3: `inout float` — exercises a non-int scalar width through
    // the write-back ABI (f64).
    let temp_dir = TempDir::new().expect("temp");
    let code = "fn scale(inout x: float):\n\tx += 1.5\n\nfn main():\n\tmut f = 1.0\n\tscale(&f)\n\tprint(float_to_str(f))\n";
    let test_file = create_test_file(temp_dir.path(), "inout_float.ryo", code);
    let output = run_ryo_command(&["run", "inout_float.ryo"], &test_file).expect("run");
    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("[Codegen]\n2.5[Result]"),
        "inout float write-back should print 2.5, got: {}",
        stdout
    );
}

#[test]
fn inout_early_return_still_writes_back() {
    // M8.3: the write-back chokepoint must fire on an EARLY `return`
    // (ReturnVoid) too, not just function fallthrough. Here `bump` exits
    // via `return` inside the `if` arm, so `a` must be 0+1+10 == 11.
    let temp_dir = TempDir::new().expect("temp");
    let code = "fn bump(inout x: int, cond: bool):\n\tx += 1\n\tif cond:\n\t\tx += 10\n\t\treturn\n\tx += 100\n\nfn main():\n\tmut a = 0\n\tbump(&a, true)\n\tprint(int_to_str(a))\n";
    let test_file = create_test_file(temp_dir.path(), "inout_early_ret.ryo", code);
    let output = run_ryo_command(&["run", "inout_early_ret.ryo"], &test_file).expect("run");
    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("[Codegen]\n11[Result]"),
        "inout early-return write-back should print 11, got: {}",
        stdout
    );
}

#[test]
fn str_push_inout_str_builtin() {
    // M8.3: str_push(s: inout str, suffix: str) appends in place via the
    // __ryo_str_push runtime + the inout str write-back ABI.
    let temp_dir = TempDir::new().expect("temp");
    let code = "fn main():\n\tmut s = \"hi\"\n\tstr_push(&s, \" there\")\n\tprint(s)\n";
    let test_file = create_test_file(temp_dir.path(), "str_push.ryo", code);
    let output = run_ryo_command(&["run", "str_push.ryo"], &test_file).expect("run");
    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("[Codegen]\nhi there[Result]"),
        "str_push should append ' there' -> 'hi there', got: {}",
        stdout
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
    let temp_dir = TempDir::new().expect("temp");
    let code = "fn swap(inout a: int, inout b: int):\n\tmut t = a\n\ta = b\n\tb = t\n\nfn main():\n\tmut x = 5\n\tmut y = 7\n\tswap(&x, &y)\n\tprint(int_to_str(x))\n";
    let test_file = create_test_file(temp_dir.path(), "inout_alias_ok.ryo", code);
    let output = run_ryo_command(&["run", "inout_alias_ok.ryo"], &test_file).expect("run");
    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("[Codegen]\n7[Result]"),
        "swap(&x, &y) should leave x == 7, got: {}",
        stdout
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
    assert!(
        stdout.contains("[Codegen]\nnew[Result]"),
        "inout str reassignment should write back 'new', got: {}",
        stdout
    );
}

#[test]
fn inout_str_user_fn_and_reborrow_writeback() {
    // A user function taking `inout str`, mutating it via str_push
    // (a reborrow of the inout param). Exercises the general str inout ABI
    // plus the nested inout call inside the callee.
    let temp_dir = TempDir::new().expect("temp");
    let code = "fn app(inout s: str):\n\tstr_push(&s, \"!\")\n\nfn main():\n\tmut s = \"hi\"\n\tapp(&s)\n\tprint(s)\n";
    let test_file = create_test_file(temp_dir.path(), "inout_str_app.ryo", code);
    let output = run_ryo_command(&["run", "inout_str_app.ryo"], &test_file).expect("run");
    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("[Codegen]\nhi![Result]"),
        "user-fn inout str write-back should print 'hi!', got: {}",
        stdout
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
    let expected = format!("hi{}", suffix);
    assert!(
        stdout.contains(&expected),
        "str_push growth should print the full concatenated string, got: {}",
        stdout
    );
}

#[test]
fn inout_bool_writeback() {
    // Review coverage gap: `inout bool` — exercises the i8 scalar width
    // through the write-back ABI.
    let temp_dir = TempDir::new().expect("temp");
    let code = "fn set(inout b: bool):\n\tb = true\n\nfn main():\n\tmut b = false\n\tset(&b)\n\tprint(bool_to_str(b))\n";
    let test_file = create_test_file(temp_dir.path(), "inout_bool.ryo", code);
    let output = run_ryo_command(&["run", "inout_bool.ryo"], &test_file).expect("run");
    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("[Codegen]\ntrue[Result]"),
        "inout bool write-back should print true, got: {}",
        stdout
    );
}

#[test]
fn inout_int_reborrow_chain() {
    // Review coverage gap: an inout param is itself a valid `&` target —
    // `twice` reborrows its own inout param into `inc`, twice.
    let temp_dir = TempDir::new().expect("temp");
    let code = "fn inc(inout x: int):\n\tx += 1\n\nfn twice(inout x: int):\n\tinc(&x)\n\tinc(&x)\n\nfn main():\n\tmut c = 0\n\ttwice(&c)\n\tprint(int_to_str(c))\n";
    let test_file = create_test_file(temp_dir.path(), "inout_reborrow.ryo", code);
    let output = run_ryo_command(&["run", "inout_reborrow.ryo"], &test_file).expect("run");
    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("[Codegen]\n2[Result]"),
        "reborrowed inout chain should print 2, got: {}",
        stdout
    );
}

#[test]
fn inout_fallthrough_return_writes_back() {
    // Review coverage gap: the multi-return-site test only exercised the
    // EARLY return. The fallthrough exit must write back too:
    // bump(&b, false) takes the fallthrough path, so b == 0+1+100 == 101.
    let temp_dir = TempDir::new().expect("temp");
    let code = "fn bump(inout x: int, cond: bool):\n\tx += 1\n\tif cond:\n\t\tx += 10\n\t\treturn\n\tx += 100\n\nfn main():\n\tmut b = 0\n\tbump(&b, false)\n\tprint(int_to_str(b))\n";
    let test_file = create_test_file(temp_dir.path(), "inout_fallthrough.ryo", code);
    let output = run_ryo_command(&["run", "inout_fallthrough.ryo"], &test_file).expect("run");
    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("[Codegen]\n101[Result]"),
        "fallthrough-return write-back should print 101, got: {}",
        stdout
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
    assert!(
        stdout.contains("[Codegen]\nb[Result]"),
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
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    // Compute the printed value (7 * 6 == 42) so the literal "42" never
    // appears in the source dump that `ryo run` emits under the
    // `[Input Source]` heading. That way `stdout.matches("42").count()`
    // reflects only the two `print(s)` calls, not the echoed source.
    let code = "fn main():\n\ts: str = int_to_str(7 * 6)\n\tprint(s)\n\tprint(s)\n";
    let test_file = create_test_file(temp_dir.path(), "multi_read.ryo", code);

    let output =
        run_ryo_command(&["run", "multi_read.ryo"], &test_file).expect("Failed to run ryo command");

    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let occurrences = stdout.matches("42").count();
    assert_eq!(
        occurrences, 2,
        "expected '42' to appear exactly twice (once per print) — got {} occurrences. stdout: {}",
        occurrences, stdout
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
    // the StrLocals triple. If the Free is missing, ASan (Task 11)
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
    // The `ryo run` CLI dumps the source program to stdout before
    // executing, so a naive `!stdout.contains("hello")` check would
    // false-positive on the echoed source. Instead, look at the
    // post-`[Codegen]` slice (the runtime's actual output) and
    // confirm the program printed exactly `"world"` — not the old
    // `"hello"` value, and not garbled bytes from a use-after-free.
    let runtime_output = stdout
        .split("[Codegen]")
        .nth(1)
        .expect("CLI trace should include [Codegen] section");
    assert!(
        !runtime_output.contains("hello"),
        "runtime stdout should not leak old value 'hello': {}",
        runtime_output
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
    let stdout = String::from_utf8_lossy(&output.stdout);
    let runtime_output = stdout
        .split("[Codegen]")
        .nth(1)
        .expect("CLI trace should include [Codegen] section");
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
    let stdout = String::from_utf8_lossy(&output.stdout);
    let runtime_output = stdout
        .split("[Codegen]")
        .nth(1)
        .expect("CLI trace should include [Codegen] section");
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
    let stdout = String::from_utf8_lossy(&output.stdout);
    let runtime_output = stdout
        .split("[Codegen]")
        .nth(1)
        .expect("CLI trace should include [Codegen] section");
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
