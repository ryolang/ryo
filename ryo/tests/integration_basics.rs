mod common;
use common::*;

use tempfile::TempDir;

// Milestone 3.5: String Literals and Print Tests

#[test]
fn test_print_hello_world() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "hello.ryo", "print(\"Hello, World!\")");

    let output =
        run_ryo_command(&["run", "hello.ryo"], &test_file).expect("Failed to run ryo run command");

    assert!(output.status.success(), "ryo run should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("[Result] => 0"), "Should exit with code 0");
}

#[test]
fn test_print_with_newline() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "newline.ryo", "print(\"Line\\n\")");

    let output = run_ryo_command(&["run", "newline.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(output.status.success(), "ryo run should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("[Result] => 0"), "Should exit with code 0");
}

#[test]
fn test_multiple_print_calls() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(
        temp_dir.path(),
        "multi_print.ryo",
        "print(\"First\\n\")\nprint(\"Second\\n\")\nprint(\"Third\\n\")",
    );

    let output = run_ryo_command(&["run", "multi_print.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(output.status.success(), "ryo run should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("[Result] => 0"), "Should exit with code 0");
}

#[test]
fn test_print_empty_string() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "empty.ryo", "print(\"\")");

    let output =
        run_ryo_command(&["run", "empty.ryo"], &test_file).expect("Failed to run ryo run command");

    assert!(output.status.success(), "ryo run should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("[Result] => 0"), "Should exit with code 0");
}

// ============================================================================
// Milestone 4: Functions & Calls
// ============================================================================

#[test]
fn test_fn_main_empty() {
    // M8a: `fn main():` is the canonical signature — no args, no
    // return type. The C-ABI shim emitted by codegen always
    // returns 0 to the OS.
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tprint(\"hello\\n\")\n";
    let test_file = create_test_file(temp_dir.path(), "fn_main_empty.ryo", code);

    let output = run_ryo_command(&["run", "fn_main_empty.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(
        output.status.success(),
        "ryo run should succeed. STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("[Result] => 0"),
        "void main always exits with 0, got: {}",
        stdout
    );
}

#[test]
fn test_fn_main_with_return_type_rejected() {
    // M8a: explicit return type on main is a compile error.
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main() -> int:\n\treturn 42\n";
    let test_file = create_test_file(temp_dir.path(), "fn_main_typed.ryo", code);

    let output = run_ryo_command(&["run", "fn_main_typed.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(
        !output.status.success(),
        "fn main() with a return type must be rejected"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("main") && stderr.contains("return type"),
        "diagnostic should mention main + return type, got: {}",
        stderr
    );
}

#[test]
fn test_fn_main_with_params_rejected() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main(x: int):\n\tprint(\"hi\")\n";
    let test_file = create_test_file(temp_dir.path(), "fn_main_args.ryo", code);

    let output = run_ryo_command(&["run", "fn_main_args.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(
        !output.status.success(),
        "fn main() with parameters must be rejected"
    );
}

#[test]
fn test_fn_with_variable() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tx = 42\n\tprint(\"ok\\n\")\n";
    let test_file = create_test_file(temp_dir.path(), "fn_var.ryo", code);

    let output =
        run_ryo_command(&["run", "fn_var.ryo"], &test_file).expect("Failed to run ryo run command");

    assert!(output.status.success(), "ryo run should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("[Result] => 0"));
}

#[test]
fn test_fn_add_two_functions() {
    // Helper functions still return int; only main is constrained
    // to void in M8a.
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn add(a: int, b: int) -> int:\n\treturn a + b\n\nfn main():\n\tx = add(2, 3)\n\tprint(\"done\\n\")\n";
    let test_file = create_test_file(temp_dir.path(), "fn_add.ryo", code);

    let output =
        run_ryo_command(&["run", "fn_add.ryo"], &test_file).expect("Failed to run ryo run command");

    assert!(
        output.status.success(),
        "ryo run should succeed. STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("[Result] => 0"));
}

#[test]
fn test_expression_statement_print() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tprint(\"Hello\\n\")\n";
    let test_file = create_test_file(temp_dir.path(), "fn_print.ryo", code);

    let output = run_ryo_command(&["run", "fn_print.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(
        output.status.success(),
        "ryo run should succeed. STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("[Result] => 0"),
        "Should exit with code 0, got: {}",
        stdout
    );
}

#[test]
fn test_backward_compat_flat_program() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "x = 42\ny = x + 1";
    let test_file = create_test_file(temp_dir.path(), "flat.ryo", code);

    let output =
        run_ryo_command(&["run", "flat.ryo"], &test_file).expect("Failed to run ryo run command");

    assert!(
        output.status.success(),
        "Flat programs should still work. STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("[Result] => 0"),
        "Flat programs should exit with 0, got: {}",
        stdout
    );
}

#[test]
fn test_forward_reference() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code =
        "fn main():\n\tx = helper()\n\tprint(\"done\\n\")\n\nfn helper() -> int:\n\treturn 10\n";
    let test_file = create_test_file(temp_dir.path(), "forward_ref.ryo", code);

    let output = run_ryo_command(&["run", "forward_ref.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("[Result] => 0"),
        "Forward reference should work, got stdout: {}\nstderr: {}",
        stdout,
        stderr
    );
}

#[test]
fn test_multiple_params() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn sum3(a: int, b: int, c: int) -> int:\n\treturn a + b + c\n\nfn main():\n\tx = sum3(10, 20, 30)\n\tprint(\"done\\n\")\n";
    let test_file = create_test_file(temp_dir.path(), "multi_params.ryo", code);

    let output = run_ryo_command(&["run", "multi_params.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("[Result] => 0"),
        "sum3(10, 20, 30) should compile and exit 0, got stdout: {}\nstderr: {}",
        stdout,
        stderr
    );
}

#[test]
fn test_nested_calls() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn double(x: int) -> int:\n\treturn x * 2\n\nfn main():\n\tx = double(double(3))\n\tprint(\"done\\n\")\n";
    let test_file = create_test_file(temp_dir.path(), "nested_calls.ryo", code);

    let output = run_ryo_command(&["run", "nested_calls.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("[Result] => 0"),
        "double(double(3)) should compile and exit 0, got stdout: {}\nstderr: {}",
        stdout,
        stderr
    );
}

#[test]
fn test_arithmetic_in_function() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn compute(a: int, b: int) -> int:\n\tx = a * 2\n\ty = b + 3\n\treturn x + y\n\nfn main():\n\tx = compute(5, 7)\n\tprint(\"done\\n\")\n";
    let test_file = create_test_file(temp_dir.path(), "fn_arith.ryo", code);

    let output = run_ryo_command(&["run", "fn_arith.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("[Result] => 0"),
        "compute(5, 7) should compile and exit 0, got stdout: {}\nstderr: {}",
        stdout,
        stderr
    );
}

#[test]
fn test_top_level_with_explicit_main_error() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "x = 42\n\nfn main():\n\tprint(\"hi\")\n";
    let test_file = create_test_file(temp_dir.path(), "mixed_error.ryo", code);

    let output = run_ryo_command(&["run", "mixed_error.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(
        !output.status.success(),
        "Mixing top-level stmts with explicit main should fail"
    );
}

#[test]
fn test_parse_function_def() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn add(a: int, b: int) -> int:\n\treturn a + b\n";
    let test_file = create_test_file(temp_dir.path(), "parse_fn.ryo", code);

    let output = run_ryo_command(&["parse", "parse_fn.ryo"], &test_file)
        .expect("Failed to run ryo parse command");

    assert!(
        output.status.success(),
        "Parse should succeed. STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("FunctionDef"),
        "AST should contain FunctionDef, got: {}",
        stdout
    );
}

// ============================================================================
// Milestone 6.5: Booleans & Equality
// ============================================================================

#[test]
fn bool_program_compiles_and_runs() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code =
        "fn main():\n\tflag = true\n\tsame = 1 == 1\n\tdiff = 1 != 1\n\tboth = flag == same\n";
    let test_file = create_test_file(temp_dir.path(), "bool_test.ryo", code);

    let output = run_ryo_command(&["run", "bool_test.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(
        output.status.success(),
        "ryo run should succeed. STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("[Result] => 0"),
        "Should exit with code 0, got: {}",
        stdout
    );
}

// ============================================================================
// Milestone 7: Float, Ordering, Modulo
// ============================================================================

#[test]
fn float_program_compiles_and_runs() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code =
        "fn main():\n\tx: float = 3.5\n\ty: float = 2.5\n\tavg = x + y / 2.0\n\tcmp = x > y\n";
    let test_file = create_test_file(temp_dir.path(), "float_test.ryo", code);

    let output = run_ryo_command(&["run", "float_test.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(
        output.status.success(),
        "ryo run should succeed. STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("[Result] => 0"),
        "Should exit with code 0, got: {}",
        stdout
    );
}

#[test]
fn integer_division_and_modulo_compile_and_run() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    // 10 / 3 = 3, 10 % 3 = 1. M8a: void main, so the program just
    // has to compile and exit 0; runtime value verification will
    // come back with `exit(code)` (M24) or stdlib formatting.
    let code = "fn main():\n\ta = 10\n\tb = 3\n\tq = a / b\n\tr = a % b\n\tcmp = q < a\n";
    let test_file = create_test_file(temp_dir.path(), "int_div_mod.ryo", code);

    let output = run_ryo_command(&["run", "int_div_mod.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(
        output.status.success(),
        "ryo run should succeed. STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("[Result] => 0"));
}

// ============================================================================
// Milestone 8b: Conditionals & Logical Operators
// ============================================================================

#[test]
fn test_if_elif_else_classify() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn classify(n: int) -> int:\n\tif n < 0:\n\t\treturn -1\n\telif n == 0:\n\t\treturn 0\n\telse:\n\t\treturn 1\n\nfn main():\n\tx = classify(5)\n\tprint(\"done\\n\")\n";
    let test_file = create_test_file(temp_dir.path(), "classify.ryo", code);

    let output = run_ryo_command(&["run", "classify.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(
        output.status.success(),
        "ryo run should succeed. STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("[Result] => 0"));
}

#[test]
fn test_and_short_circuit_in_range() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn in_range(x: int, lo: int, hi: int) -> bool:\n\treturn x >= lo and x <= hi\n\nfn main():\n\tr = in_range(5, 0, 10)\n\tprint(\"done\\n\")\n";
    let test_file = create_test_file(temp_dir.path(), "in_range.ryo", code);

    let output = run_ryo_command(&["run", "in_range.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(
        output.status.success(),
        "ryo run should succeed. STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("[Result] => 0"));
}

#[test]
fn test_not_operator_codegen() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tx = not true\n\ty = not false\n\tprint(\"done\\n\")\n";
    let test_file = create_test_file(temp_dir.path(), "not_op.ryo", code);

    let output =
        run_ryo_command(&["run", "not_op.ryo"], &test_file).expect("Failed to run ryo run command");

    assert!(
        output.status.success(),
        "ryo run should succeed. STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("[Result] => 0"));
}

#[test]
fn test_simple_if_else() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tif true:\n\t\tprint(\"yes\\n\")\n\telse:\n\t\tprint(\"no\\n\")\n";
    let test_file = create_test_file(temp_dir.path(), "if_else.ryo", code);

    let output = run_ryo_command(&["run", "if_else.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(
        output.status.success(),
        "ryo run should succeed. STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("[Result] => 0"));
}

#[test]
fn test_if_without_else() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tif true:\n\t\tprint(\"yes\\n\")\n\tprint(\"done\\n\")\n";
    let test_file = create_test_file(temp_dir.path(), "if_no_else.ryo", code);

    let output = run_ryo_command(&["run", "if_no_else.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(
        output.status.success(),
        "ryo run should succeed. STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("[Result] => 0"));
}

#[test]
fn test_nested_if() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tif true:\n\t\tif false:\n\t\t\tprint(\"inner\\n\")\n\t\telse:\n\t\t\tprint(\"outer\\n\")\n";
    let test_file = create_test_file(temp_dir.path(), "nested_if.ryo", code);

    let output = run_ryo_command(&["run", "nested_if.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(
        output.status.success(),
        "ryo run should succeed. STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("[Result] => 0"));
}

#[test]
fn test_combined_logical_and_conditional() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tif true and not false:\n\t\tprint(\"ok\\n\")\n\telse:\n\t\tprint(\"fail\\n\")\n";
    let test_file = create_test_file(temp_dir.path(), "combined.ryo", code);

    let output = run_ryo_command(&["run", "combined.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(
        output.status.success(),
        "ryo run should succeed. STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("[Result] => 0"));
}

// ============================================================================
// Milestone 8c1: Variable Reassignment & Compound Assignment
// ============================================================================

#[test]
fn test_mut_reassign_basic() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tmut x = 1\n\tx = 2\n\tassert(x == 2, \"x should be 2 after reassignment\")\n";
    let test_file = create_test_file(temp_dir.path(), "reassign.ryo", code);

    let output =
        run_ryo_command(&["run", "reassign.ryo"], &test_file).expect("Failed to run ryo command");

    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_compound_assign_int() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tmut x = 10\n\tx += 5\n\tx -= 3\n\tassert(x == 12, \"10 + 5 - 3 should be 12\")\n";
    let test_file = create_test_file(temp_dir.path(), "compound.ryo", code);

    let output =
        run_ryo_command(&["run", "compound.ryo"], &test_file).expect("Failed to run ryo command");

    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_compound_assign_mul_div_mod() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    // 20 * 3 = 60, 60 / 2 = 30, 30 % 7 = 2
    let code = "fn main():\n\tmut x = 20\n\tx *= 3\n\tx /= 2\n\tx %= 7\n\tassert(x == 2, \"20*3/2%7 should be 2\")\n";
    let test_file = create_test_file(temp_dir.path(), "compound_mdm.ryo", code);

    let output = run_ryo_command(&["run", "compound_mdm.ryo"], &test_file)
        .expect("Failed to run ryo command");

    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_compound_assign_float() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    // 10.0 + 2.5 = 12.5, 12.5 - 0.5 = 12.0
    let code = "fn main():\n\tmut x = 10.0\n\tx += 2.5\n\tx -= 0.5\n\tassert(x == 12.0, \"10.0+2.5-0.5 should be 12.0\")\n";
    let test_file = create_test_file(temp_dir.path(), "compound_float.ryo", code);

    let output = run_ryo_command(&["run", "compound_float.ryo"], &test_file)
        .expect("Failed to run ryo command");

    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_immutable_reassign_error() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tx = 1\n\tx = 2\n";
    let test_file = create_test_file(temp_dir.path(), "immutable_err.ryo", code);

    let output = run_ryo_command(&["run", "immutable_err.ryo"], &test_file)
        .expect("Failed to run ryo command");

    assert!(
        !output.status.success(),
        "should fail for immutable reassign"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cannot assign to immutable variable"),
        "expected immutability error, got: {}",
        stderr
    );
}

#[test]
fn test_cross_scope_mut_reassign() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tmut x = 1\n\tif true:\n\t\tx = 42\n\tassert(x == 42, \"cross-scope reassign should persist\")\n";
    let test_file = create_test_file(temp_dir.path(), "cross_scope.ryo", code);

    let output = run_ryo_command(&["run", "cross_scope.ryo"], &test_file)
        .expect("Failed to run ryo command");

    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// ─── while loops ──────────────────────────────────────────────────────────

#[test]
fn while_loop_countdown() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tmut i = 5\n\twhile i > 0:\n\t\ti -= 1\n\tassert(i == 0, \"countdown should reach 0\")\n";
    let test_file = create_test_file(temp_dir.path(), "while_countdown.ryo", code);

    let output = run_ryo_command(&["run", "while_countdown.ryo"], &test_file)
        .expect("Failed to run ryo command");

    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn while_loop_accumulate() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    // sum = 1+2+3+4+5 = 15
    let code = "fn main():\n\tmut sum = 0\n\tmut i = 1\n\twhile i <= 5:\n\t\tsum += i\n\t\ti += 1\n\tassert(sum == 15, \"1+2+3+4+5 should be 15\")\n";
    let test_file = create_test_file(temp_dir.path(), "while_accum.ryo", code);

    let output = run_ryo_command(&["run", "while_accum.ryo"], &test_file)
        .expect("Failed to run ryo command");

    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn while_break_exits_loop() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tmut i = 0\n\twhile true:\n\t\ti += 1\n\t\tif i == 3:\n\t\t\tbreak\n\tassert(i == 3, \"break at 3\")\n";
    let test_file = create_test_file(temp_dir.path(), "while_break.ryo", code);

    let output = run_ryo_command(&["run", "while_break.ryo"], &test_file)
        .expect("Failed to run ryo command");

    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn while_continue_skips_iteration() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    // Sum only odd numbers 1..10: 1+3+5+7+9 = 25
    let code = "fn main():\n\tmut sum = 0\n\tmut i = 0\n\twhile i < 10:\n\t\ti += 1\n\t\tif i % 2 == 0:\n\t\t\tcontinue\n\t\tsum += i\n\tassert(sum == 25, \"odd sum 1..10 should be 25\")\n";
    let test_file = create_test_file(temp_dir.path(), "while_continue.ryo", code);

    let output = run_ryo_command(&["run", "while_continue.ryo"], &test_file)
        .expect("Failed to run ryo command");

    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn while_nested_loops() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    // 3 * 4 = 12
    let code = "fn main():\n\tmut total = 0\n\tmut i = 0\n\twhile i < 3:\n\t\tmut j = 0\n\t\twhile j < 4:\n\t\t\ttotal += 1\n\t\t\tj += 1\n\t\ti += 1\n\tassert(total == 12, \"3*4 should be 12\")\n";
    let test_file = create_test_file(temp_dir.path(), "while_nested.ryo", code);

    let output = run_ryo_command(&["run", "while_nested.ryo"], &test_file)
        .expect("Failed to run ryo command");

    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn while_break_inner_only() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    // break in inner loop doesn't exit outer; outer runs 3 times, inner breaks at 2 each
    let code = "fn main():\n\tmut total = 0\n\tmut i = 0\n\twhile i < 3:\n\t\tmut j = 0\n\t\twhile true:\n\t\t\tif j == 2:\n\t\t\t\tbreak\n\t\t\ttotal += 1\n\t\t\tj += 1\n\t\ti += 1\n\tassert(total == 6, \"3 outer * 2 inner = 6\")\n";
    let test_file = create_test_file(temp_dir.path(), "while_break_inner.ryo", code);

    let output = run_ryo_command(&["run", "while_break_inner.ryo"], &test_file)
        .expect("Failed to run ryo command");

    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn while_false_body_never_runs() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tmut x = 0\n\twhile false:\n\t\tx = 99\n\tassert(x == 0, \"while false body should never run\")\n";
    let test_file = create_test_file(temp_dir.path(), "while_false.ryo", code);

    let output = run_ryo_command(&["run", "while_false.ryo"], &test_file)
        .expect("Failed to run ryo command");

    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn while_break_outside_loop_error() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tbreak\n";
    let test_file = create_test_file(temp_dir.path(), "break_outside.ryo", code);

    let output = run_ryo_command(&["run", "break_outside.ryo"], &test_file)
        .expect("Failed to run ryo command");

    assert!(
        !output.status.success(),
        "break outside loop should be rejected"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("E0024"),
        "should emit E0024 for break outside loop, got: {}",
        stderr
    );
}

#[test]
fn while_continue_outside_loop_error() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tcontinue\n";
    let test_file = create_test_file(temp_dir.path(), "continue_outside.ryo", code);

    let output = run_ryo_command(&["run", "continue_outside.ryo"], &test_file)
        .expect("Failed to run ryo command");

    assert!(
        !output.status.success(),
        "continue outside loop should be rejected"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("E0025"),
        "should emit E0025 for continue outside loop, got: {}",
        stderr
    );
}

#[test]
fn while_non_bool_condition_error() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\twhile 42:\n\t\tx = 1\n";
    let test_file = create_test_file(temp_dir.path(), "while_nonbool.ryo", code);

    let output = run_ryo_command(&["run", "while_nonbool.ryo"], &test_file)
        .expect("Failed to run ryo command");

    assert!(
        !output.status.success(),
        "non-bool while condition should be rejected"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("E0018"),
        "should emit E0018 for non-bool condition, got: {}",
        stderr
    );
}

#[test]
fn while_true_with_return() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\twhile true:\n\t\treturn\n";
    let test_file = create_test_file(temp_dir.path(), "while_return.ryo", code);

    let output = run_ryo_command(&["run", "while_return.ryo"], &test_file)
        .expect("Failed to run ryo command");

    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// ============================================================================
// Milestone 8c3: For-Range Loops
// ============================================================================

#[test]
fn test_for_range_sum() {
    // 0+1+2+3+4 = 10
    let code = "fn main():\n\tmut sum = 0\n\tfor i in range(0, 5):\n\t\tsum += i\n\tassert(sum == 10, \"0+1+2+3+4 should be 10\")\n";
    assert_ryo_runs("for_sum.ryo", code);
}

#[test]
fn test_for_range_zero_iterations_start_gt_end() {
    let code = "fn main():\n\tmut ran = 0\n\tfor i in range(5, 3):\n\t\tran = 1\n\tassert(ran == 0, \"body should never run when start > end\")\n";
    assert_ryo_runs("for_empty.ryo", code);
}

#[test]
fn test_for_range_zero_iterations_equal() {
    let code = "fn main():\n\tmut ran = 0\n\tfor i in range(5, 5):\n\t\tran = 1\n\tassert(ran == 0, \"body should never run when start == end\")\n";
    assert_ryo_runs("for_equal.ryo", code);
}

#[test]
fn test_for_range_with_break() {
    // break at i==3, so last assigned is 2
    let code = "fn main():\n\tmut last = 0\n\tfor i in range(0, 10):\n\t\tif i == 3:\n\t\t\tbreak\n\t\tlast = i\n\tassert(last == 2, \"last before break at 3 should be 2\")\n";
    assert_ryo_runs("for_break.ryo", code);
}

#[test]
fn test_for_range_with_continue() {
    // 0+1+3+4 = 8 (skipped 2)
    let code = "fn main():\n\tmut sum = 0\n\tfor i in range(0, 5):\n\t\tif i == 2:\n\t\t\tcontinue\n\t\tsum += i\n\tassert(sum == 8, \"0+1+3+4 should be 8\")\n";
    assert_ryo_runs("for_continue.ryo", code);
}

#[test]
fn test_nested_for_loops() {
    // 3 * 2 = 6
    let code = "fn main():\n\tmut sum = 0\n\tfor i in range(0, 3):\n\t\tfor j in range(0, 2):\n\t\t\tsum += 1\n\tassert(sum == 6, \"3*2 should be 6\")\n";
    assert_ryo_runs("nested_for.ryo", code);
}

#[test]
fn test_for_inside_while() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    // 2 * 3 = 6
    let code = "fn main():\n\tmut count = 0\n\tmut n = 2\n\twhile n > 0:\n\t\tfor i in range(0, 3):\n\t\t\tcount += 1\n\t\tn -= 1\n\tassert(count == 6, \"2*3 should be 6\")\n";
    let test_file = create_test_file(temp_dir.path(), "for_in_while.ryo", code);

    let output = run_ryo_command(&["run", "for_in_while.ryo"], &test_file)
        .expect("Failed to run ryo command");

    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_while_inside_for() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    // 3 * 2 = 6
    let code = "fn main():\n\tmut count = 0\n\tfor i in range(0, 3):\n\t\tmut j = 2\n\t\twhile j > 0:\n\t\t\tcount += 1\n\t\t\tj -= 1\n\tassert(count == 6, \"3*2 should be 6\")\n";
    let test_file = create_test_file(temp_dir.path(), "while_in_for.ryo", code);

    let output = run_ryo_command(&["run", "while_in_for.ryo"], &test_file)
        .expect("Failed to run ryo command");

    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_nested_for_same_var_name() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    // 3 * 2 = 6, inner `i` shadows outer `i`
    let code = "fn main():\n\tmut sum = 0\n\tfor i in range(0, 3):\n\t\tfor i in range(0, 2):\n\t\t\tsum += 1\n\tassert(sum == 6, \"3*2 with shadowed i should be 6\")\n";
    let test_file = create_test_file(temp_dir.path(), "for_shadow.ryo", code);

    let output =
        run_ryo_command(&["run", "for_shadow.ryo"], &test_file).expect("Failed to run ryo command");

    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_for_body_return() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn first_multiple_of_3(limit: int) -> int:\n\tfor i in range(0, limit):\n\t\tif i > 0 and i % 3 == 0:\n\t\t\treturn i\n\treturn -1\n\nfn main():\n\tx = first_multiple_of_3(10)\n\tassert(x == 3, \"first multiple of 3 in 0..10 should be 3\")\n";
    let test_file = create_test_file(temp_dir.path(), "for_return.ryo", code);

    let output =
        run_ryo_command(&["run", "for_return.ryo"], &test_file).expect("Failed to run ryo command");

    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_str_variable_print() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tname: str = \"Hello\"\n\tprint(name)\n";
    let test_file = create_test_file(temp_dir.path(), "str_var_print.ryo", code);

    let output = run_ryo_command(&["run", "str_var_print.ryo"], &test_file)
        .expect("Failed to run ryo command");

    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Hello"),
        "Output should contain 'Hello', got: {}",
        stdout
    );
    assert!(stdout.contains("[Result] => 0"), "Should exit with code 0");
}

#[test]
fn test_str_concat() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code =
        "fn main():\n\ta: str = \"Hello, \"\n\tb: str = \"World!\"\n\tc: str = a + b\n\tprint(c)\n";
    let test_file = create_test_file(temp_dir.path(), "str_concat.ryo", code);

    let output =
        run_ryo_command(&["run", "str_concat.ryo"], &test_file).expect("Failed to run ryo command");

    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Hello, World!"),
        "Output should contain 'Hello, World!', got: {}",
        stdout
    );
    assert!(stdout.contains("[Result] => 0"), "Should exit with code 0");
}

#[test]
fn test_str_concat_chained() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tresult: str = \"a\" + \"b\" + \"c\"\n\tprint(result)\n";
    let test_file = create_test_file(temp_dir.path(), "str_concat_chained.ryo", code);

    let output = run_ryo_command(&["run", "str_concat_chained.ryo"], &test_file)
        .expect("Failed to run ryo command");

    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("abc"),
        "Output should contain 'abc', got: {}",
        stdout
    );
    assert!(stdout.contains("[Result] => 0"), "Should exit with code 0");
}

#[test]
fn test_str_equality() {
    let code = "fn main():\n\ta: str = \"hello\"\n\tb: str = \"hello\"\n\tassert(a == b, \"equal strings should be equal\")\n";
    assert_ryo_runs("str_equality.ryo", code);
}

#[test]
fn test_str_inequality() {
    let code = "fn main():\n\ta: str = \"hello\"\n\tb: str = \"world\"\n\tassert(a != b, \"different strings should not be equal\")\n";
    assert_ryo_runs("str_inequality.ryo", code);
}

#[test]
fn test_slice_print_roundtrip() {
    assert_ryo_runs(
        "slice_print.ryo",
        "fn main():\n\ts: str = \"hello world\"\n\tprint(s[0:5])\n\tprint(s[6:])\n\tprint(s[:5])\n",
    );
}

#[test]
fn test_bare_int_to_str_statement() {
    // A formatter builtin called as a bare statement (result
    // discarded) must not trip the scalar-Free guard in codegen.
    assert_ryo_runs("bare_int_to_str.ryo", "fn main():\n\tint_to_str(5)\n");
}

#[test]
fn test_bare_float_to_str_statement() {
    // Same, for the float formatter.
    assert_ryo_runs("bare_float_to_str.ryo", "fn main():\n\tfloat_to_str(2.5)\n");
}

#[test]
fn test_bare_bool_to_str_statement() {
    // Same, for the bool formatter.
    assert_ryo_runs("bare_bool_to_str.ryo", "fn main():\n\tbool_to_str(true)\n");
}

#[test]
fn test_bare_slice_statement() {
    // A bare view-typed expression statement (result discarded) must
    // evaluate through the view entry point, not the scalar path.
    assert_ryo_runs(
        "bare_slice.ryo",
        "fn main():\n\ts: str = \"hello\"\n\ts[0:2]\n",
    );
}

#[test]
fn test_int_to_str_builtin() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\ts: str = int_to_str(42)\n\tprint(s)\n";
    let test_file = create_test_file(temp_dir.path(), "int_to_str.ryo", code);

    let output =
        run_ryo_command(&["run", "int_to_str.ryo"], &test_file).expect("Failed to run ryo command");

    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("42"),
        "Output should contain '42', got: {}",
        stdout
    );
}

#[test]
fn test_float_to_str_builtin() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\ts: str = float_to_str(2.75)\n\tprint(s)\n";
    let test_file = create_test_file(temp_dir.path(), "float_to_str.ryo", code);

    let output = run_ryo_command(&["run", "float_to_str.ryo"], &test_file)
        .expect("Failed to run ryo command");

    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("2.75"),
        "Output should contain '2.75', got: {}",
        stdout
    );
}

#[test]
fn test_float_to_str_large_number() {
    let dir = tempfile::tempdir().unwrap();
    // 18000000000000000000.0 is a large number (1.8e19)
    let src = create_test_file(
        dir.path(),
        "large_float.ryo",
        "fn main():\n\tprint(float_to_str(18000000000000000000.0))\n",
    );
    let output = run_ryo_command(&["run", "large_float.ryo"], &src);
    let output = output.unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Extract the float value: it's after "[Codegen]" and before "[Result]"
    let after_codegen = stdout.split("[Codegen]").nth(1).unwrap();
    let float_str = after_codegen.split("[Result]").next().unwrap().trim();
    let parsed: f64 = float_str.parse().unwrap();
    assert_eq!(parsed, 1.8e19);
}

#[test]
fn test_float_to_str_small_decimal() {
    let dir = tempfile::tempdir().unwrap();
    let src = create_test_file(
        dir.path(),
        "small_float.ryo",
        "fn main():\n\tprint(float_to_str(0.1))\n",
    );
    let output = run_ryo_command(&["run", "small_float.ryo"], &src);
    let output = output.unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Extract the float value: it's after "[Codegen]" and before "[Result]"
    let after_codegen = stdout.split("[Codegen]").nth(1).unwrap();
    let float_str = after_codegen.split("[Result]").next().unwrap().trim();
    let parsed: f64 = float_str.parse().unwrap();
    assert_eq!(parsed, 0.1);
}

#[test]
fn test_bool_to_str_builtin() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\ts: str = bool_to_str(true)\n\tprint(s)\n";
    let test_file = create_test_file(temp_dir.path(), "bool_to_str.ryo", code);

    let output = run_ryo_command(&["run", "bool_to_str.ryo"], &test_file)
        .expect("Failed to run ryo command");

    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("true"),
        "Output should contain 'true', got: {}",
        stdout
    );
}

// ---- str.len() and str.is_empty() method calls ----

#[test]
fn test_str_len() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "s: str = \"hello\"\nassert(s.len() == 5, \"len should be 5\")";
    let test_file = create_test_file(temp_dir.path(), "str_len.ryo", code);
    let output = run_ryo_command(&["run", "str_len.ryo"], &test_file).expect("Failed to run");
    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_str_is_empty() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "s: str = \"\"\nassert(s.is_empty(), \"empty string should be empty\")";
    let test_file = create_test_file(temp_dir.path(), "str_empty.ryo", code);
    let output = run_ryo_command(&["run", "str_empty.ryo"], &test_file).expect("Failed to run");
    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_str_is_empty_false() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code =
        "s: str = \"hi\"\nassert(not s.is_empty(), \"non-empty string should not be empty\")";
    let test_file = create_test_file(temp_dir.path(), "str_not_empty.ryo", code);
    let output = run_ryo_command(&["run", "str_not_empty.ryo"], &test_file).expect("Failed to run");
    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_str_len_concat() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "s: str = \"ab\" + \"cde\"\nassert(s.len() == 5, \"concat len should be 5\")";
    let test_file = create_test_file(temp_dir.path(), "str_len_concat.ryo", code);
    let output =
        run_ryo_command(&["run", "str_len_concat.ryo"], &test_file).expect("Failed to run");
    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_str_empty_concat_left() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code =
        "s: str = \"\" + \"hello\"\nassert(s.len() == 5, \"empty + hello should have len 5\")";
    let test_file = create_test_file(temp_dir.path(), "empty_left.ryo", code);
    let output = run_ryo_command(&["run", "empty_left.ryo"], &test_file).expect("Failed");
    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_str_empty_concat_both() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "s: str = \"\" + \"\"\nassert(s.is_empty(), \"empty + empty should be empty\")";
    let test_file = create_test_file(temp_dir.path(), "empty_both.ryo", code);
    let output = run_ryo_command(&["run", "empty_both.ryo"], &test_file).expect("Failed");
    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_str_empty_equality() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code =
        "a: str = \"\"\nb: str = \"\"\nassert(a == b, \"two empty strings should be equal\")";
    let test_file = create_test_file(temp_dir.path(), "empty_eq.ryo", code);
    let output = run_ryo_command(&["run", "empty_eq.ryo"], &test_file).expect("Failed");
    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_str_concat_with_to_str() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "n: int = 42\ns: str = \"value = \" + int_to_str(n)\nprint(s)";
    let test_file = create_test_file(temp_dir.path(), "concat_int.ryo", code);
    let output = run_ryo_command(&["run", "concat_int.ryo"], &test_file).expect("Failed");
    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_str_empty_len_zero() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "s: str = \"\"\nassert(s.len() == 0, \"empty string len should be 0\")";
    let test_file = create_test_file(temp_dir.path(), "empty_len.ryo", code);
    let output = run_ryo_command(&["run", "empty_len.ryo"], &test_file).expect("Failed");
    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_str_passed_to_function() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn greet(name: str):\n\tprint(name)\n\ngreet(\"Alice\")";
    let test_file = create_test_file(temp_dir.path(), "str_param.ryo", code);
    let output = run_ryo_command(&["run", "str_param.ryo"], &test_file).expect("Failed");
    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_str_returned_from_function() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code =
        "fn make_greeting() -> str:\n\treturn \"Hello!\"\n\ns: str = make_greeting()\nprint(s)";
    let test_file = create_test_file(temp_dir.path(), "str_return.ryo", code);
    let output = run_ryo_command(&["run", "str_return.ryo"], &test_file).expect("Failed");
    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_str_shadowed_by_int_assignment_does_not_panic() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tmut s: str = \"hello\"\n\tif true:\n\t\tmut s: int = 1\n\t\ts = 2\n\t\tprint(int_to_str(s))\n\tprint(s)\n";
    let test_file = create_test_file(temp_dir.path(), "str_shadow.ryo", code);
    let output = run_ryo_command(&["run", "str_shadow.ryo"], &test_file).expect("Failed");
    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Assert on the program's runtime output only (the slice after the
    // "[Codegen]" marker), not the full stdout.
    let runtime = stdout.split("[Codegen]").nth(1).unwrap();
    assert!(
        runtime.contains("2"),
        "Output should contain '2', got: {}",
        runtime
    );
    assert!(
        runtime.contains("hello"),
        "Output should contain 'hello', got: {}",
        runtime
    );
}
