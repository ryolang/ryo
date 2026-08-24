mod common;
use common::*;

use std::process::Command;
use tempfile::TempDir;

// ============================================================================
// Milestone 8b2: Panic and Assert
// ============================================================================

#[test]
fn panic_exits_with_101_jit() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tpanic(\"boom\")\n";
    let test_file = create_test_file(temp_dir.path(), "panic_basic.ryo", code);

    let output = run_ryo_command(&["run", "panic_basic.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert_ne!(
        output.status.code(),
        Some(0),
        "panic() should exit nonzero. stdout: {}, stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("panicked"),
        "stderr should contain panic message, got: {}",
        stderr
    );
}

#[test]
fn assert_true_compiles_and_succeeds() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tassert(true, \"should pass\")\n\tprint(\"ok\\n\")\n";
    let test_file = create_test_file(temp_dir.path(), "assert_true.ryo", code);

    let output = run_ryo_command(&["run", "assert_true.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(
        output.status.success(),
        "assert(true) should succeed. STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("[Result] => 0"));
}

#[test]
fn assert_false_exits_with_101_jit() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tassert(false, \"this should fail\")\n";
    let test_file = create_test_file(temp_dir.path(), "assert_false.ryo", code);

    let output = run_ryo_command(&["run", "assert_false.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert_ne!(
        output.status.code(),
        Some(0),
        "assert(false) should exit nonzero. stdout: {}, stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("assertion failed"),
        "stderr should contain assert failure message, got: {}",
        stderr
    );
}

#[test]
fn assert_expression_condition_compiles() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tassert(1 == 1, \"equality works\")\n\tassert(2 != 3, \"inequality works\")\n\tprint(\"all good\\n\")\n";
    let test_file = create_test_file(temp_dir.path(), "assert_expr.ryo", code);

    let output = run_ryo_command(&["run", "assert_expr.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn multiple_asserts_all_passing() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tassert(true, \"first\")\n\tassert(1 == 1, \"second\")\n\tassert(1 != 2, \"third\")\n\tprint(\"done\\n\")\n";
    let test_file = create_test_file(temp_dir.path(), "multi_assert.ryo", code);

    let output = run_ryo_command(&["run", "multi_assert.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(output.status.success());
}

#[test]
fn assert_as_last_statement() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tassert(true, \"final\")\n";
    let test_file = create_test_file(temp_dir.path(), "assert_last.ryo", code);

    let output = run_ryo_command(&["run", "assert_last.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn assert_inside_if_body() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tif true:\n\t\tassert(1 == 1, \"in if\")\n\tprint(\"after\\n\")\n";
    let test_file = create_test_file(temp_dir.path(), "assert_in_if.ryo", code);

    let output = run_ryo_command(&["run", "assert_in_if.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn panic_inside_if_branch_taken() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tif true:\n\t\tpanic(\"taken\")\n\tprint(\"unreachable\\n\")\n";
    let test_file = create_test_file(temp_dir.path(), "panic_in_if.ryo", code);

    let output = run_ryo_command(&["run", "panic_in_if.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(
        !output.status.success(),
        "panic in taken branch should exit nonzero. stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn panic_inside_if_branch_not_taken() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tif false:\n\t\tpanic(\"not taken\")\n\tprint(\"reached\\n\")\n";
    let test_file = create_test_file(temp_dir.path(), "panic_skipped.ryo", code);

    let output = run_ryo_command(&["run", "panic_skipped.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(
        output.status.success(),
        "untaken panic branch should not fire. STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn never_var_decl_scalar_rejected() {
    assert_never_rejected(
        "never_vardecl_scalar.ryo",
        "fn f() -> int:\n\tx: int = panic(\"boom\")\n\nfn main():\n\ty = f()\n",
    );
}

#[test]
fn never_var_decl_str_rejected() {
    assert_never_rejected(
        "never_vardecl_str.ryo",
        "fn f() -> int:\n\tx: str = panic(\"boom\")\n\nfn main():\n\ty = f()\n",
    );
}

#[test]
fn never_assign_scalar_rejected() {
    assert_never_rejected(
        "never_assign_scalar.ryo",
        "fn f() -> int:\n\tmut x = 1\n\tx = panic(\"boom\")\n\nfn main():\n\ty = f()\n",
    );
}

#[test]
fn never_assign_str_rejected() {
    assert_never_rejected(
        "never_assign_str.ryo",
        "fn f() -> int:\n\tmut x = \"a\"\n\tx = panic(\"boom\")\n\nfn main():\n\ty = f()\n",
    );
}

#[test]
fn never_compound_assign_rejected() {
    assert_never_rejected(
        "never_compound.ryo",
        "fn f() -> int:\n\tmut x = 1\n\tx += panic(\"boom\")\n\nfn main():\n\ty = f()\n",
    );
}

#[test]
fn never_assign_view_rejected() {
    assert_never_rejected(
        "never_assign_view.ryo",
        "fn f() -> int:\n\ts = \"hello\"\n\tmut v = s[0:2]\n\tv = panic(\"boom\")\n\nfn main():\n\ty = f()\n",
    );
}

#[test]
fn never_return_rejected() {
    assert_never_rejected(
        "never_return.ryo",
        "fn f() -> int:\n\treturn panic(\"boom\")\n\nfn main():\n\ty = f()\n",
    );
}

#[test]
fn never_binop_operand_rejected() {
    assert_never_rejected(
        "never_binop.ryo",
        "fn f() -> int:\n\treturn 1 + panic(\"boom\")\n\nfn main():\n\ty = f()\n",
    );
}

#[test]
fn never_call_arg_rejected() {
    assert_never_rejected(
        "never_call_arg.ryo",
        "fn g(x: int):\n\tprint(\"{x}\")\n\nfn main():\n\tg(panic(\"boom\"))\n",
    );
}

#[test]
fn assert_non_bool_condition_rejected() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tassert(42, \"not bool\")\n";
    let test_file = create_test_file(temp_dir.path(), "assert_bad_cond.ryo", code);

    let output = run_ryo_command(&["run", "assert_bad_cond.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert_ne!(
        output.status.code(),
        Some(0),
        "non-bool condition should be a compile error"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("E0012"));
}

#[test]
fn assert_wrong_arity_rejected() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tassert(true)\n";
    let test_file = create_test_file(temp_dir.path(), "assert_arity.ryo", code);

    let output = run_ryo_command(&["run", "assert_arity.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert_ne!(
        output.status.code(),
        Some(0),
        "wrong arity should be a compile error"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("E0013"));
}

#[test]
fn panic_wrong_arity_rejected() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tpanic()\n";
    let test_file = create_test_file(temp_dir.path(), "panic_arity.ryo", code);

    let output = run_ryo_command(&["run", "panic_arity.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert_ne!(
        output.status.code(),
        Some(0),
        "panic with no args should be a compile error"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("E0013"));
}

#[test]
fn panic_non_literal_rejected() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tpanic(42)\n";
    let test_file = create_test_file(temp_dir.path(), "panic_bad_arg.ryo", code);

    let output = run_ryo_command(&["run", "panic_bad_arg.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert_ne!(
        output.status.code(),
        Some(0),
        "panic with non-literal should be a compile error"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("E0014"));
}

// ============================================================================
// Division / modulo by zero
// ============================================================================

#[test]
fn div_by_zero_literal_rejected() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tx = 1 / 0\n";
    let test_file = create_test_file(temp_dir.path(), "div_zero_lit.ryo", code);

    let output = run_ryo_command(&["run", "div_zero_lit.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(
        !output.status.success(),
        "literal division by zero should be a compile error"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("division by zero"),
        "expected division-by-zero diagnostic, got: {}",
        stderr
    );
}

#[test]
fn mod_by_zero_literal_rejected() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tx = 1 % 0\n";
    let test_file = create_test_file(temp_dir.path(), "mod_zero_lit.ryo", code);

    let output = run_ryo_command(&["run", "mod_zero_lit.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(
        !output.status.success(),
        "literal modulo by zero should be a compile error"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("modulo by zero"),
        "expected modulo-by-zero diagnostic, got: {}",
        stderr
    );
}

#[test]
fn div_by_neg_zero_literal_rejected_e0037() {
    // `-0` is unary minus over the zero literal, not a signed literal —
    // sema must still reject it with the same E0037 diagnostic.
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tx = 1 / -0\n";
    let test_file = create_test_file(temp_dir.path(), "div_neg_zero_lit.ryo", code);

    let output = run_ryo_command(&["run", "div_neg_zero_lit.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(
        !output.status.success(),
        "division by -0 should be a compile error"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("E0037") && stderr.contains("division by zero"),
        "expected E0037 division-by-zero diagnostic, got: {}",
        stderr
    );
}

#[test]
fn div_by_const_expr_zero_rejected_e0037() {
    // The divisor const-evals to zero without being a literal.
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tx = 1 / (2 - 2)\n";
    let test_file = create_test_file(temp_dir.path(), "div_const_zero.ryo", code);

    let output = run_ryo_command(&["run", "div_const_zero.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(
        !output.status.success(),
        "division by constant-zero expression should be a compile error"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("E0037") && stderr.contains("division by zero"),
        "expected E0037 division-by-zero diagnostic, got: {}",
        stderr
    );
}

#[test]
fn const_int_overflow_rejected_e0200() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tx = 9223372036854775807 + 1\n";
    let test_file = create_test_file(temp_dir.path(), "const_overflow.ryo", code);

    let output = run_ryo_command(&["run", "const_overflow.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(
        !output.status.success(),
        "constant integer overflow should be a compile error"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("E0200") && stderr.contains("overflow"),
        "expected E0200 overflow diagnostic, got: {}",
        stderr
    );
}

#[test]
fn div_by_zero_panics_jit() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tx = 0\n\ty = 10 / x\n";
    let test_file = create_test_file(temp_dir.path(), "div_zero.ryo", code);

    let output = run_ryo_command(&["run", "div_zero.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert_ne!(
        output.status.code(),
        Some(0),
        "division by zero should exit nonzero. stdout: {}",
        String::from_utf8_lossy(&output.stdout),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("integer division by zero"),
        "stderr should contain division-by-zero message, got: {}",
        stderr
    );
}

#[test]
fn mod_by_zero_panics_jit() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tx = 0\n\ty = 10 % x\n";
    let test_file = create_test_file(temp_dir.path(), "mod_zero.ryo", code);

    let output = run_ryo_command(&["run", "mod_zero.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert_ne!(
        output.status.code(),
        Some(0),
        "modulo by zero should exit nonzero. stdout: {}",
        String::from_utf8_lossy(&output.stdout),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("integer modulo by zero"),
        "stderr should contain modulo-by-zero message, got: {}",
        stderr
    );
}

#[test]
fn compound_div_by_zero_panics_jit() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\ty = 0\n\tmut x = 10\n\tx /= y\n";
    let test_file = create_test_file(temp_dir.path(), "compound_div_zero.ryo", code);

    let output = run_ryo_command(&["run", "compound_div_zero.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert_ne!(
        output.status.code(),
        Some(0),
        "compound division by zero should exit nonzero. stdout: {}",
        String::from_utf8_lossy(&output.stdout),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("integer division by zero"),
        "stderr should contain division-by-zero message, got: {}",
        stderr
    );
}

#[test]
fn compound_mod_by_zero_panics_jit() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\ty = 0\n\tmut x = 10\n\tx %= y\n";
    let test_file = create_test_file(temp_dir.path(), "compound_mod_zero.ryo", code);

    let output = run_ryo_command(&["run", "compound_mod_zero.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert_ne!(
        output.status.code(),
        Some(0),
        "compound modulo by zero should exit nonzero. stdout: {}",
        String::from_utf8_lossy(&output.stdout),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("integer modulo by zero"),
        "stderr should contain modulo-by-zero message, got: {}",
        stderr
    );
}

#[test]
fn div_by_zero_aot_run_exits_101() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tx = 0\n\ty = 10 / x\n";
    let test_file = create_test_file(temp_dir.path(), "div_zero_aot.ryo", code);

    let build_output = run_ryo_build(&test_file, temp_dir.path());
    assert!(
        build_output.status.success(),
        "ryo build failed. STDERR: {}",
        String::from_utf8_lossy(&build_output.stderr)
    );

    let binary_path = exe_path(temp_dir.path(), "div_zero_aot");
    let run_output = Command::new(&binary_path)
        .output()
        .expect("Failed to execute compiled binary");

    assert_eq!(
        run_output.status.code(),
        Some(101),
        "binary should exit 101 on division by zero"
    );
    let stderr = String::from_utf8_lossy(&run_output.stderr);
    assert!(
        stderr.contains("integer division by zero"),
        "stderr should contain division-by-zero message, got: {}",
        stderr
    );
}

// ============================================================================
// Integer overflow traps (spec §18: checked arithmetic in all build modes)
// ============================================================================

#[test]
fn add_overflow_panics_jit() {
    // Var operand defeats sema const-eval; the trap fires at runtime.
    assert_int_overflow_panics(
        "add_overflow.ryo",
        "fn main():\n\tx = 9223372036854775807\n\ty = x + 1\n",
    );
}

#[test]
fn sub_overflow_panics_jit() {
    // x const-evals to i64::MIN (no overflow); x - 1 overflows at runtime.
    assert_int_overflow_panics(
        "sub_overflow.ryo",
        "fn main():\n\tx = (0 - 9223372036854775807) - 1\n\ty = x - 1\n",
    );
}

#[test]
fn mul_overflow_panics_jit() {
    assert_int_overflow_panics(
        "mul_overflow.ryo",
        "fn main():\n\tx = 9223372036854775807\n\ty = x * 2\n",
    );
}

#[test]
fn neg_overflow_panics_jit() {
    // `-(i64::MIN)` with a non-constant operand.
    assert_int_overflow_panics(
        "neg_overflow.ryo",
        "fn main():\n\tx = (0 - 9223372036854775807) - 1\n\ty = -x\n",
    );
}

#[test]
fn compound_add_overflow_panics_jit() {
    assert_int_overflow_panics(
        "compound_add_overflow.ryo",
        "fn main():\n\tmut x = 9223372036854775807\n\tx += 1\n",
    );
}

#[test]
fn add_at_max_does_not_trap() {
    // Boundary: i64::MAX + 0 must NOT trip the overflow guard.
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tx = 9223372036854775807\n\ty = x + 0\n\tassert(y == 9223372036854775807, \"max\")\n";
    let test_file = create_test_file(temp_dir.path(), "add_max_ok.ryo", code);

    let output = run_ryo_command(&["run", "add_max_ok.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(
        output.status.success(),
        "i64::MAX + 0 should succeed. STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// ─── guard elision (codegen drops checks a constant makes unreachable) ────

#[test]
fn identity_arithmetic_at_boundaries_does_not_trap() {
    // `x - 0`, `x * 1` and `x * 0` are exact for every x, so codegen
    // drops their overflow guards. Pinned at i64::MIN, where a wrong
    // elision (or a wrongly kept guard) would show up immediately.
    assert_program_succeeds(
        "identity_arith.ryo",
        "fn main():\n\tx = (0 - 9223372036854775807) - 1\n\ty = x - 0\n\tz = x * 1\n\tw = x * 0\n\tassert(y == x, \"sub zero\")\n\tassert(z == x, \"mul one\")\n\tassert(w == 0, \"mul zero\")\n",
    );
}

#[test]
fn mul_by_minus_one_at_min_still_traps() {
    // The mirror of the test above: -1 is *not* an exact factor, so
    // the guard must survive — `INT_MIN * -1` has no i64 result.
    assert_int_overflow_panics(
        "mul_minus_one.ryo",
        "fn main():\n\tx = (0 - 9223372036854775807) - 1\n\tmut f = 0 - 1\n\ty = x * f\n",
    );
}

#[test]
fn constant_divisor_still_divides() {
    // A non-zero constant divisor cannot trip the zero-divisor guard,
    // so codegen skips it. The arithmetic must be unaffected.
    assert_program_succeeds(
        "const_divisor.ryo",
        "fn main():\n\tmut x = 17\n\tassert(x / 2 == 8, \"div\")\n\tassert(x % 5 == 2, \"mod\")\n\tx /= 4\n\tassert(x == 4, \"compound div\")\n\tx %= 3\n\tassert(x == 1, \"compound mod\")\n",
    );
}
