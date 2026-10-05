//! M10 destructuring — end-to-end JIT tests.
//!
//! `(q, r) = divmod(...)`, `{quot, _} = ...`, `{x = quot} = ...`: full
//! coverage (bind or `_` every field), `_` fields destroyed inline, the
//! source value fully consumed. Bound fields move out as fresh owners;
//! expression-level field moves stay forbidden (MoveOutOfField).

mod common;
use common::*;

use tempfile::TempDir;

const DIVMOD: &str =
    "fn divmod(a: int, b: int) -> {q: int, r: int}:\n\treturn {q=a / b, r=a % b}\n\n";

#[test]
fn destructure_divmod_positional_and_named() {
    // Positional binds by canonical field order; `{quot, _}` binds the
    // first uncovered field (q) to a renamed local and `_`-skips the
    // rest. Both destructure the same call result.
    assert_ryo_output(
        "destructure_divmod",
        &format!(
            "{DIVMOD}fn main():\n\t(q, r) = divmod(10, 3)\n\tprint(q)\n\tprint(\"\\n\")\n\tprint(r)\n\tprint(\"\\n\")\n\t{{quot, _}} = divmod(10, 3)\n\tprint(quot)\n\tprint(\"\\n\")\n"
        ),
        "3\n1\n3\n",
    );
}

#[test]
fn destructure_parenless_and_rename() {
    // The paren-less statement form and the explicit rename form
    // (`{q = qq}`).
    assert_ryo_output(
        "destructure_parenless",
        &format!(
            "{DIVMOD}fn main():\n\tq, r = divmod(10, 3)\n\tprint(q)\n\tprint(\"\\n\")\n\t{{q = qq, r = rr}} = divmod(10, 4)\n\tprint(qq)\n\tprint(\"\\n\")\n\tprint(rr)\n\tprint(\"\\n\")\n"
        ),
        "3\n2\n2\n",
    );
}

#[test]
fn destructure_named_struct() {
    // Named structs destructure by field name against struct_view.
    assert_ryo_output(
        "destructure_named_struct",
        "struct Point:\n\tx: int\n\ty: int\n\nfn main():\n\t{x, y} = Point{x=1, y=2}\n\tprint(x)\n\tprint(\"\\n\")\n\tprint(y)\n\tprint(\"\\n\")\n\t{x = ex, y = why} = Point{x=3, y=4}\n\tprint(ex)\n\tprint(\"\\n\")\n\tprint(why)\n\tprint(\"\\n\")\n",
        "1\n2\n3\n4\n",
    );
}

#[test]
fn destructure_named_struct_positional_rejected() {
    // Positional patterns only work on anonymous structs / tuples: a
    // named struct's fields aren't "0"/"1", so the pattern is rejected
    // with the by-name fix-it.
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code =
        "struct Point:\n\tx: int\n\ty: int\n\nfn main():\n\t(q, r) = Point{x=1, y=2}\n\tprint(q)\n";
    let test_file = create_test_file(temp_dir.path(), "pos_named.ryo", code);

    let output =
        run_ryo_command(&["run", "pos_named.ryo"], &test_file).expect("Failed to run ryo command");

    assert!(
        !output.status.success(),
        "positional destructure of a named struct must be rejected"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("E0115"),
        "should emit E0115, got: {}",
        stderr
    );
    assert!(
        stderr.contains("cannot destructure named struct `Point` positionally"),
        "error should name the struct and the positional form, got: {}",
        stderr
    );
    assert!(
        stderr.contains("{x, y}"),
        "error should suggest the by-name pattern, got: {}",
        stderr
    );
}

#[test]
fn destructure_wildcard_moves() {
    // `_` fields are destroyed inline (freed exactly once); the bound
    // field moves out as a fresh owner.
    assert_ryo_output(
        "destructure_wildcard_moves",
        "fn main():\n\t{a, _} = {x=\"keep\", y=\"drop\"}\n\tprint(a)\n\tprint(\"\\n\")\n",
        "keep\n",
    );
}

#[test]
fn destructure_wildcard_str_in_loop() {
    // A fresh call result each iteration: the bound str field is freed
    // per iteration, the shell is not double-freed, the loop converges.
    assert_ryo_output(
        "destructure_wildcard_str_in_loop",
        "fn f(i: int) -> {s: str, n: int}:\n\treturn {s=\"abc\", n=i}\n\nfn main():\n\tmut i = 0\n\twhile i < 3:\n\t\t(s, _) = f(i)\n\t\tprint(s)\n\t\tprint(\"\\n\")\n\t\ti = i + 1\n",
        "abc\nabc\nabc\n",
    );
}

#[test]
fn destructure_nested() {
    // Nested positional pattern: the inner pattern destructures the
    // field-1 struct through a compiler temp.
    assert_ryo_output(
        "destructure_nested",
        "fn f() -> {p: int, q: {u: int, v: int}}:\n\treturn {p=1, q={u=2, v=3}}\n\nfn main():\n\t(a, (b, c)) = f()\n\tprint(a)\n\tprint(\"\\n\")\n\tprint(b)\n\tprint(\"\\n\")\n\tprint(c)\n\tprint(\"\\n\")\n",
        "1\n2\n3\n",
    );
}

#[test]
fn destructure_nested_with_str_field() {
    // The nested temp's str field moves out whole; wildcard drops run
    // inline at both levels.
    assert_ryo_output(
        "destructure_nested_str",
        "fn f() -> {a: str, b: {c: str, d: int}}:\n\treturn {a=\"one\", b={c=\"two\", d=4}}\n\nfn main():\n\t(x, (y, _)) = f()\n\tprint(x)\n\tprint(\"\\n\")\n\tprint(y)\n\tprint(\"\\n\")\n",
        "one\ntwo\n",
    );
}

#[test]
fn destructure_coverage_error() {
    // `{q}` leaves `r` uncovered: the error names the missing field and
    // teaches the fix.
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = format!("{DIVMOD}fn main():\n\t{{q}} = divmod(10, 3)\n\tprint(q)\n");
    let test_file = create_test_file(temp_dir.path(), "coverage.ryo", &code);

    let output =
        run_ryo_command(&["run", "coverage.ryo"], &test_file).expect("Failed to run ryo command");

    assert!(!output.status.success(), "a coverage gap must be rejected");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("E0114"),
        "should emit E0114 (DestructureUnknownField), got: {}",
        stderr
    );
    assert!(
        stderr.contains("`r`"),
        "error should name the missing field, got: {}",
        stderr
    );
    assert!(
        stderr.contains("bind it or write `_`"),
        "error should teach the `_` fix, got: {}",
        stderr
    );
}

#[test]
fn destructure_arity_error() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = format!("{DIVMOD}fn main():\n\t(q, r, s) = divmod(10, 3)\n\tprint(q)\n");
    let test_file = create_test_file(temp_dir.path(), "arity.ryo", &code);

    let output =
        run_ryo_command(&["run", "arity.ryo"], &test_file).expect("Failed to run ryo command");

    assert!(
        !output.status.success(),
        "a positional count mismatch must be rejected"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("E0113"),
        "should emit E0113 (DestructureArity), got: {}",
        stderr
    );
    assert!(
        stderr.contains("expected 2 fields, found 3 bindings"),
        "error should pin the expected/found counts, got: {}",
        stderr
    );
}

#[test]
fn destructure_unknown_field_error() {
    // A rename names its field explicitly — an unknown field is an
    // error (unlike a bare pun, which falls back to the next
    // uncovered field).
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = format!("{DIVMOD}fn main():\n\t{{q, z = zz}} = divmod(10, 3)\n\tprint(q)\n");
    let test_file = create_test_file(temp_dir.path(), "unknown.ryo", &code);

    let output =
        run_ryo_command(&["run", "unknown.ryo"], &test_file).expect("Failed to run ryo command");

    assert!(
        !output.status.success(),
        "an unknown field in the pattern must be rejected"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("E0114"),
        "should emit E0114 (DestructureUnknownField), got: {}",
        stderr
    );
    assert!(
        stderr.contains("has no field `z`"),
        "error should name the unknown field, got: {}",
        stderr
    );
}

#[test]
fn destructure_non_struct() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\t(q, r) = 5\n\tprint(q)\n";
    let test_file = create_test_file(temp_dir.path(), "non_struct.ryo", code);

    let output =
        run_ryo_command(&["run", "non_struct.ryo"], &test_file).expect("Failed to run ryo command");

    assert!(
        !output.status.success(),
        "destructuring an int must be rejected"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cannot destructure `int` — destructuring works on structs and tuples"),
        "error should pin the non-struct message bar, got: {}",
        stderr
    );
}

#[test]
fn destructure_rebind_error() {
    // A binding the pattern would create already exists: the error names
    // it and points at the individual-assignment alternative.
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = format!("{DIVMOD}fn main():\n\tq = 1\n\t(q, r) = divmod(10, 3)\n\tprint(q)\n");
    let test_file = create_test_file(temp_dir.path(), "rebind.ryo", &code);

    let output =
        run_ryo_command(&["run", "rebind.ryo"], &test_file).expect("Failed to run ryo command");

    assert!(
        !output.status.success(),
        "a binding collision must be rejected"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("E0029"),
        "should emit E0029 (DuplicateDeclaration), got: {}",
        stderr
    );
    assert!(
        stderr.contains("'q' is already declared"),
        "error should name the colliding binding, got: {}",
        stderr
    );
    assert!(
        stderr.contains("assign"),
        "error should suggest individual assignment, got: {}",
        stderr
    );
}

#[test]
fn field_move_still_forbidden() {
    // M9 rule holds unchanged: a needs-drop field read in a consuming
    // position is MoveOutOfField — only whole-pattern destructuring may
    // move fields.
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "struct Pair:\n\ts: str\n\tn: int\n\nfn main():\n\tp = Pair{s=\"x\", n=1}\n\tx = p.s\n\tprint(x)\n";
    let test_file = create_test_file(temp_dir.path(), "field_move.ryo", code);

    let output =
        run_ryo_command(&["run", "field_move.ryo"], &test_file).expect("Failed to run ryo command");

    assert!(
        !output.status.success(),
        "expression-level field moves must stay forbidden"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("E0043"),
        "should emit E0043 (MoveOutOfField), got: {}",
        stderr
    );
    assert!(
        stderr.contains("cannot move field `s` out of `Pair`"),
        "error should pin the MoveOutOfField message, got: {}",
        stderr
    );
}

#[test]
fn field_move_anon_names_the_shape() {
    // M10: the same E0043 against an anonymous struct names the
    // structural shape, not the empty sentinel name.
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tpair = (17, \"alice\")\n\ts = pair.1\n\tprint(s)\n";
    let test_file = create_test_file(temp_dir.path(), "field_move_anon.ryo", code);

    let output = run_ryo_command(&["run", "field_move_anon.ryo"], &test_file)
        .expect("Failed to run ryo command");

    assert!(
        !output.status.success(),
        "expression-level field moves must stay forbidden"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("E0043"),
        "should emit E0043 (MoveOutOfField), got: {}",
        stderr
    );
    assert!(
        stderr.contains("cannot move field `1` out of `(int, str)`"),
        "error should pin the anonymous shape in the message, got: {}",
        stderr
    );
}

#[test]
fn failed_destructure_registers_error_typed_bindings() {
    // A destructure that fails shape validation still introduces its
    // pattern names (error-typed): a later use points back at the
    // failed statement instead of cascading 'undefined variable'.
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\t(a, b) = 5\n\tprint(a)\n";
    let test_file = create_test_file(temp_dir.path(), "not_a_struct_use.ryo", code);

    let output = run_ryo_command(&["run", "not_a_struct_use.ryo"], &test_file)
        .expect("Failed to run ryo command");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("E0041"),
        "should emit E0041 (NotAStruct), got: {}",
        stderr
    );
    assert!(
        !stderr.contains("undefined variable"),
        "later uses of the pattern names must not cascade, got: {}",
        stderr
    );
}

#[test]
fn arity_failed_destructure_registers_bindings() {
    // Same contract on the positional-arity path, where shape
    // validation binds nothing before recovering.
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\t(a, b, c) = (1, 2)\n\tprint(a)\n";
    let test_file = create_test_file(temp_dir.path(), "arity_use.ryo", code);

    let output =
        run_ryo_command(&["run", "arity_use.ryo"], &test_file).expect("Failed to run ryo command");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("E0113"),
        "should emit E0113 (DestructureArity), got: {}",
        stderr
    );
    assert!(
        !stderr.contains("undefined variable"),
        "later uses of the pattern names must not cascade, got: {}",
        stderr
    );
}

#[test]
fn void_destructure_names_first_user_binding() {
    // The void/never diagnostic names the first USER binding — never
    // a compiler temp — and the nested sub-pattern fails silently
    // instead of leaking `__ryo_destructure_N` as an undefined name.
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn p():\n\tprint(1)\n\nfn main():\n\t((a, b), c) = p()\n";
    let test_file = create_test_file(temp_dir.path(), "void_temp_first.ryo", code);

    let output = run_ryo_command(&["run", "void_temp_first.ryo"], &test_file)
        .expect("Failed to run ryo command");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cannot bind 'c' to a 'void' value"),
        "the message should name the first user binding, got: {}",
        stderr
    );
    assert!(
        !stderr.contains("__ryo_destructure"),
        "compiler temp names must never surface, got: {}",
        stderr
    );
}

#[test]
fn destructure_failed_struct_definition_recovers() {
    // A struct whose definition failed (unknown field type) leaves the
    // declared type undefined; a variable annotated with it carries that
    // raw TypeId. Destructuring it must recover with the original
    // diagnostic instead of panicking in `struct_view` (the guard
    // mirrors expression-level field access).
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code =
        "struct Bad:\n\tx: Nope\n\nfn main():\n\tb: Bad = Bad{x = 1}\n\t{x} = b\n\tprint(x)\n";
    let test_file = create_test_file(temp_dir.path(), "undefined_struct.ryo", code);

    let output = run_ryo_command(&["run", "undefined_struct.ryo"], &test_file)
        .expect("Failed to run ryo command");

    assert!(
        !output.status.success(),
        "the failed struct definition must fail compilation"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unknown type: 'Nope'"),
        "the original field-type diagnostic must survive, got: {}",
        stderr
    );
    assert!(
        !stderr.contains("panicked"),
        "destructuring must not panic on an undefined struct, got: {}",
        stderr
    );
}
