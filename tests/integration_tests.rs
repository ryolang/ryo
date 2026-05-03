use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

// Helper function to run ryo compiler and capture output
fn run_ryo_command(
    args: &[&str],
    file_path: &Path,
) -> Result<std::process::Output, std::io::Error> {
    let mut cmd = Command::new("cargo");
    cmd.args(["run", "--"])
        .args(&args[..args.len() - 1]) // All args except the filename
        .arg(file_path); // Use absolute path for the file
    cmd.output()
}

// Helper function to create a temporary test file
fn create_test_file(dir: &Path, filename: &str, content: &str) -> std::path::PathBuf {
    let file_path = dir.join(filename);
    fs::write(&file_path, content).expect("Failed to write test file");
    file_path
}

#[test]
fn test_lex_command_integration() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "tokens.ryo", "x = 1 + 2 * 3");

    let output =
        run_ryo_command(&["lex", "tokens.ryo"], &test_file).expect("Failed to run ryo lex command");

    if !output.status.success() {
        println!("STDOUT: {}", String::from_utf8_lossy(&output.stdout));
        println!("STDERR: {}", String::from_utf8_lossy(&output.stderr));
        panic!("Lex command failed");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Verify token output contains expected tokens.
    // The lex driver renders ident/string-literal payloads through
    // the InternPool (Phase 2), so we see the original text;
    // integer literals are parsed at lex time so they print as
    // typed values rather than as the source slice.
    assert!(stdout.contains("Ident(\"x\")"), "Missing x identifier");
    assert!(stdout.contains("Assign"), "Missing Assign token");
    assert!(stdout.contains("IntLit(1)"), "Missing IntLit(1) token");
    assert!(stdout.contains("Add"), "Missing Add token");
    assert!(stdout.contains("IntLit(2)"), "Missing IntLit(2) token");
    assert!(stdout.contains("Mul"), "Missing Mul token");
    assert!(stdout.contains("IntLit(3)"), "Missing IntLit(3) token");

    // Verify no output files are created for lex command (lex doesn't generate files)
    assert!(
        !PathBuf::from("tokens.o").exists(),
        "Object file should not be created for lex command"
    );
}

#[test]
fn test_parse_command_simple_declaration() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "simple.ryo", "x = 42");

    let output = run_ryo_command(&["parse", "simple.ryo"], &test_file)
        .expect("Failed to run ryo parse command");

    if !output.status.success() {
        println!("STDOUT: {}", String::from_utf8_lossy(&output.stdout));
        println!("STDERR: {}", String::from_utf8_lossy(&output.stderr));
        panic!("Parse command failed");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Verify AST output contains expected elements
    assert!(stdout.contains("[AST]"), "Missing AST section");
    assert!(stdout.contains("Program"), "Missing Program node");
    assert!(stdout.contains("VarDecl"), "Missing VarDecl node");
}

#[test]
fn test_parse_command_with_type_annotation() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "typed.ryo", "x: int = 42");

    let output = run_ryo_command(&["parse", "typed.ryo"], &test_file)
        .expect("Failed to run ryo parse command");

    if !output.status.success() {
        println!("STDOUT: {}", String::from_utf8_lossy(&output.stdout));
        println!("STDERR: {}", String::from_utf8_lossy(&output.stderr));
        panic!("Parse command failed");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Verify AST output
    assert!(stdout.contains("VarDecl"), "Missing VarDecl node");
    assert!(stdout.contains("int"), "Missing type annotation");
}

#[test]
fn test_parse_command_multiple_statements() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "multi.ryo", "x = 1\ny = 2\nz = 3");

    let output = run_ryo_command(&["parse", "multi.ryo"], &test_file)
        .expect("Failed to run ryo parse command");

    if !output.status.success() {
        println!("STDOUT: {}", String::from_utf8_lossy(&output.stdout));
        println!("STDERR: {}", String::from_utf8_lossy(&output.stderr));
        panic!("Parse command failed");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Verify AST output
    assert!(stdout.contains("VarDecl"), "Missing VarDecl nodes");
}

#[test]
fn test_file_not_found_error() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let nonexistent_path = temp_dir.path().join("nonexistent.ryo");

    let output = run_ryo_command(&["parse", "nonexistent.ryo"], &nonexistent_path)
        .expect("Failed to run ryo command");

    // Command should fail
    assert!(
        !output.status.success(),
        "Command should fail when file doesn't exist"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("IoError") || stderr.contains("No such file") || stderr.contains("Error:"),
        "Should contain file not found error, got: {}",
        stderr
    );
}

// ============================================================================
// Codegen Integration Tests (ryo run command)
// ============================================================================

#[test]
fn test_run_simple_integer_exit_code() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "exit_simple.ryo", "x = 42");

    let output = run_ryo_command(&["run", "exit_simple.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    // Verify compilation succeeded
    assert!(
        output.status.success(),
        "ryo run should succeed. STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Verify output shows successful compilation
    // All programs exit with 0 (success) in Milestone 3
    assert!(
        stdout.contains("[Result] => 0"),
        "Output should show exit code 0, got: {}",
        stdout
    );

    // Verify intermediate outputs are present
    assert!(stdout.contains("[Input Source]"), "Missing input source");
    assert!(stdout.contains("[AST]"), "Missing AST output");
    assert!(stdout.contains("[Codegen]"), "Missing codegen output");
}

#[test]
fn test_run_zero_exit_code() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "exit_zero.ryo", "x = 0");

    let output = run_ryo_command(&["run", "exit_zero.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(output.status.success(), "ryo run should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("[Result] => 0"),
        "Output should show exit code 0"
    );
}

#[test]
fn test_run_arithmetic_expression_exit_code() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "exit_arithmetic.ryo", "result = 2 + 3 * 4");

    let output = run_ryo_command(&["run", "exit_arithmetic.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(output.status.success(), "ryo run should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    // 2 + 3 * 4 = 2 + 12 = 14 (correct precedence), but exit code is 0
    assert!(
        stdout.contains("[Result] => 0"),
        "Should exit with code 0, got: {}",
        stdout
    );
}

#[test]
fn test_run_multiple_statements_last_value() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "x = 10\ny = 20\nz = 30";
    let test_file = create_test_file(temp_dir.path(), "exit_multi.ryo", code);

    let output = run_ryo_command(&["run", "exit_multi.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(output.status.success(), "ryo run should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    // All programs exit with 0 (success)
    assert!(
        stdout.contains("[Result] => 0"),
        "Multiple statements should exit with 0"
    );
}

#[test]
fn test_run_division_by_constant() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "exit_div.ryo", "result = 100 / 2");

    let output = run_ryo_command(&["run", "exit_div.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(output.status.success(), "ryo run should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("[Result] => 0"), "Should exit with code 0");
}

#[test]
fn test_run_subtraction() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "exit_sub.ryo", "result = 100 - 30");

    let output = run_ryo_command(&["run", "exit_sub.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(output.status.success(), "ryo run should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("[Result] => 0"), "Should exit with code 0");
}

#[test]
fn test_run_parenthesized_expression() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "exit_paren.ryo", "result = (10 + 5) * 2");

    let output = run_ryo_command(&["run", "exit_paren.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(output.status.success(), "ryo run should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    // (10 + 5) * 2 = 15 * 2 = 30 (computed), but exit code is 0
    assert!(stdout.contains("[Result] => 0"), "Should exit with code 0");
}

#[test]
fn test_run_with_type_annotation() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "exit_typed.ryo", "x: int = 99");

    let output = run_ryo_command(&["run", "exit_typed.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(output.status.success(), "ryo run should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("[Result] => 0"),
        "Should correctly compile typed variable and exit with 0"
    );
}

#[test]
fn test_run_mutable_variable() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "exit_mut.ryo", "mut x = 55");

    let output = run_ryo_command(&["run", "exit_mut.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(output.status.success(), "ryo run should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("[Result] => 0"),
        "Should correctly compile mutable variable and exit with 0"
    );
}

#[test]
fn test_run_negation_operator() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "exit_neg.ryo", "x = -42");

    let output = run_ryo_command(&["run", "exit_neg.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(output.status.success(), "ryo run should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    // All programs exit with 0 (success)
    assert!(stdout.contains("[Result] => 0"), "Should exit with code 0");
}

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

// ---------- ryo ir --emit=... ----------
//
// Exit-criteria coverage for pipeline_alignment.md §3.6 / §4.5:
// `Uir::dump` and `Tir::dump` reachable from the CLI, distinct
// listings, deterministic ordering.

#[test]
fn ir_emit_uir_dumps_flat_listing() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "uir.ryo", "x = 1 + 2\n");

    let output = run_ryo_command(&["ir", "--emit=uir", "uir.ryo"], &test_file)
        .expect("Failed to run ryo ir --emit=uir");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout.contains("[UIR]"), "missing [UIR] banner: {}", stdout);
    assert!(
        stdout.contains("fn main() -> void"),
        "missing fn header: {}",
        stdout
    );
    assert!(
        stdout.contains("= int 1"),
        "missing int literal listing: {}",
        stdout
    );
    assert!(
        stdout.contains("= add %"),
        "missing add listing: {}",
        stdout
    );
    // UIR must not include typed listings.
    assert!(
        !stdout.contains("[TIR]"),
        "TIR leaked into UIR-only run: {}",
        stdout
    );
    assert!(
        !stdout.contains("iadd"),
        "TIR-spelled op leaked: {}",
        stdout
    );
}

#[test]
fn ir_emit_tir_dumps_typed_listing() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "tir.ryo", "x = 1 + 2\n");

    let output = run_ryo_command(&["ir", "--emit=tir", "tir.ryo"], &test_file)
        .expect("Failed to run ryo ir --emit=tir");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout.contains("[TIR]"), "missing [TIR] banner: {}", stdout);
    assert!(
        stdout.contains(": int ="),
        "missing typed slot rendering: {}",
        stdout
    );
    assert!(stdout.contains("iadd %"), "missing typed add: {}", stdout);
    // TIR-only run must not print UIR's untyped spelling.
    assert!(!stdout.contains("[UIR]"), "UIR banner leaked: {}", stdout);
}

#[test]
fn ir_emit_default_is_ast_and_clif() {
    // Bare `ryo ir <file>` preserves the pre-Phase-5 default of
    // AST + Cranelift IR so existing scripts keep working.
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "default.ryo", "x = 42\n");

    let output = run_ryo_command(&["ir", "default.ryo"], &test_file).expect("Failed to run ryo ir");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout.contains("[AST]"), "missing [AST]: {}", stdout);
    assert!(
        stdout.contains("[Cranelift IR]"),
        "missing CLIF: {}",
        stdout
    );
    assert!(
        !stdout.contains("[UIR]"),
        "UIR leaked into default: {}",
        stdout
    );
    assert!(
        !stdout.contains("[TIR]"),
        "TIR leaked into default: {}",
        stdout
    );
}

#[test]
fn ir_emit_order_is_pipeline_not_flag() {
    // Section order must be AST → UIR → TIR → CLIF regardless of
    // the order in which flags are listed. We exercise this two
    // ways:
    //   1. Two shuffled permutations of the full four-section list
    //      must produce **byte-identical** output.
    //   2. Within a single run, banners must appear in pipeline
    //      order — ast_idx < uir_idx < tir_idx < clif_idx.
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "order.ryo", "x = 1\n");

    // A handful of deliberately-shuffled permutations. Not
    // exhaustive (24 total) — a representative selection that
    // covers each section appearing both first and last is enough
    // to catch a regression that respects flag order.
    let perms = [
        "ast,uir,tir,clif",
        "clif,tir,uir,ast",
        "tir,ast,clif,uir",
        "uir,clif,ast,tir",
    ];

    let outputs: Vec<_> = perms
        .iter()
        .map(|p| {
            let arg = format!("--emit={}", p);
            let out = run_ryo_command(&["ir", &arg, "order.ryo"], &test_file)
                .unwrap_or_else(|e| panic!("ryo ir --emit={}: {}", p, e));
            assert!(
                out.status.success(),
                "--emit={} failed: {}",
                p,
                String::from_utf8_lossy(&out.stderr)
            );
            out.stdout
        })
        .collect();

    // (1) flag order must not change output.
    for (i, perm) in perms.iter().enumerate().skip(1) {
        assert_eq!(
            outputs[0], outputs[i],
            "--emit=ast,uir,tir,clif and --emit={} produced different output",
            perm
        );
    }

    // (2) banners appear in pipeline order within a run.
    let stdout = String::from_utf8_lossy(&outputs[0]);
    let ast_idx = stdout.find("[AST]").expect("AST banner");
    let uir_idx = stdout.find("[UIR]").expect("UIR banner");
    let tir_idx = stdout.find("[TIR]").expect("TIR banner");
    let clif_idx = stdout.find("[Cranelift IR]").expect("CLIF banner");
    assert!(
        ast_idx < uir_idx && uir_idx < tir_idx && tir_idx < clif_idx,
        "sections out of pipeline order \
         (ast={}, uir={}, tir={}, clif={}):\n{}",
        ast_idx,
        uir_idx,
        tir_idx,
        clif_idx,
        stdout
    );
}

#[test]
fn ir_emit_uir_with_sema_error_still_prints_uir() {
    // A type-error fixture: `--emit=uir` should print the UIR
    // (astgen succeeded) and exit 0 — sema is never run, so its
    // diagnostics never fire.
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "bad.ryo", "x = -true\n");

    let output = run_ryo_command(&["ir", "--emit=uir", "bad.ryo"], &test_file)
        .expect("ryo ir --emit=uir on bad source");
    assert!(
        output.status.success(),
        "UIR-only run should not run sema; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("[UIR]"), "missing UIR: {}", stdout);
    assert!(stdout.contains("= neg %"), "missing neg op: {}", stdout);
}

#[test]
fn ir_emit_tir_prints_partial_tir_with_unreachable_on_sema_error() {
    // §4.5: sema emits `Unreachable` in place of failed expressions
    // and keeps going. `--emit=tir` deliberately renders that
    // partial TIR — the whole point of the flag is debugging sema.
    // Driver still exits non-zero because the sink has errors.
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "partial.ryo", "x = -true\n");

    let output = run_ryo_command(&["ir", "--emit=tir", "partial.ryo"], &test_file)
        .expect("ryo ir --emit=tir on bad source");
    assert!(
        !output.status.success(),
        "should exit non-zero on sema error"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("[TIR]"), "TIR banner missing: {}", stdout);
    assert!(
        stdout.contains("unreachable"),
        "Unreachable not rendered: {}",
        stdout
    );
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
// Milestone 8b2: Panic and Assert
// ============================================================================

#[test]
fn panic_exits_with_101_jit() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tpanic(\"boom\")\n";
    let test_file = create_test_file(temp_dir.path(), "panic_basic.ryo", code);

    let output = run_ryo_command(&["run", "panic_basic.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(
        !output.status.success(),
        "panic() should exit nonzero. stdout: {}, stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
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

    assert!(
        !output.status.success(),
        "assert(false) should exit nonzero. stdout: {}, stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
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
fn assert_non_bool_condition_rejected() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tassert(42, \"not bool\")\n";
    let test_file = create_test_file(temp_dir.path(), "assert_bad_cond.ryo", code);

    let output = run_ryo_command(&["run", "assert_bad_cond.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(
        !output.status.success(),
        "non-bool condition should be a compile error"
    );
}

#[test]
fn assert_wrong_arity_rejected() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tassert(true)\n";
    let test_file = create_test_file(temp_dir.path(), "assert_arity.ryo", code);

    let output = run_ryo_command(&["run", "assert_arity.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(
        !output.status.success(),
        "wrong arity should be a compile error"
    );
}

#[test]
fn panic_wrong_arity_rejected() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tpanic()\n";
    let test_file = create_test_file(temp_dir.path(), "panic_arity.ryo", code);

    let output = run_ryo_command(&["run", "panic_arity.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(
        !output.status.success(),
        "panic with no args should be a compile error"
    );
}

#[test]
fn panic_non_literal_rejected() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tpanic(42)\n";
    let test_file = create_test_file(temp_dir.path(), "panic_bad_arg.ryo", code);

    let output = run_ryo_command(&["run", "panic_bad_arg.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(
        !output.status.success(),
        "panic with non-literal should be a compile error"
    );
}

// =============================================================================
// AOT Build + Run Verification Tests
// =============================================================================

/// Run `ryo build` and return the path to the compiled binary.
///
/// The AOT pipeline writes the binary (named after the source file stem) into
/// the process's CWD. We point CWD at a dedicated output directory so the
/// artifact lands somewhere predictable and is cleaned up with the `TempDir`.
fn run_ryo_build(source_file: &Path, out_dir: &Path) -> std::process::Output {
    // We need the Cargo project root so `cargo run` can find Cargo.toml.
    // CARGO_MANIFEST_DIR is set by Cargo during `cargo test`.
    let manifest_dir = env!("CARGO_MANIFEST_DIR");

    Command::new("cargo")
        .args(["run", "--manifest-path"])
        .arg(format!("{}/Cargo.toml", manifest_dir))
        .args(["--", "build"])
        .arg(source_file)
        .current_dir(out_dir)
        .output()
        .expect("Failed to run ryo build command")
}

#[test]
fn assert_true_aot_run_succeeds() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tassert(true, \"aot ok\")\n\tprint(\"built\\n\")\n";
    let test_file = create_test_file(temp_dir.path(), "assert_aot.ryo", code);

    let build_output = run_ryo_build(&test_file, temp_dir.path());
    assert!(
        build_output.status.success(),
        "ryo build failed. STDERR: {}",
        String::from_utf8_lossy(&build_output.stderr)
    );

    let binary_path = temp_dir.path().join("assert_aot");
    let run_output = Command::new(&binary_path)
        .output()
        .expect("Failed to execute compiled binary");

    assert!(
        run_output.status.success(),
        "compiled binary should exit 0. stderr: {}",
        String::from_utf8_lossy(&run_output.stderr)
    );
    let stdout = String::from_utf8_lossy(&run_output.stdout);
    assert!(
        stdout.contains("built"),
        "expected 'built' in stdout, got: {}",
        stdout
    );
}

#[test]
fn assert_false_aot_run_exits_101() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tassert(false, \"boom\")\n";
    let test_file = create_test_file(temp_dir.path(), "assert_false_aot.ryo", code);

    let build_output = run_ryo_build(&test_file, temp_dir.path());
    assert!(
        build_output.status.success(),
        "ryo build failed. STDERR: {}",
        String::from_utf8_lossy(&build_output.stderr)
    );

    let binary_path = temp_dir.path().join("assert_false_aot");
    let run_output = Command::new(&binary_path)
        .output()
        .expect("Failed to execute compiled binary");

    assert_eq!(
        run_output.status.code(),
        Some(101),
        "binary should exit 101 on assert failure"
    );
    let stderr = String::from_utf8_lossy(&run_output.stderr);
    assert!(
        stderr.contains("assertion failed")
            && stderr.contains("in main()")
            && stderr.contains("boom"),
        "stderr should contain formatted message with location, got: {}",
        stderr
    );
}

#[test]
fn panic_aot_run_exits_101() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tpanic(\"explicit\")\n";
    let test_file = create_test_file(temp_dir.path(), "panic_aot.ryo", code);

    let build_output = run_ryo_build(&test_file, temp_dir.path());
    assert!(
        build_output.status.success(),
        "ryo build failed. STDERR: {}",
        String::from_utf8_lossy(&build_output.stderr)
    );

    let binary_path = temp_dir.path().join("panic_aot");
    let run_output = Command::new(&binary_path)
        .output()
        .expect("Failed to execute compiled binary");

    assert_eq!(
        run_output.status.code(),
        Some(101),
        "binary should exit 101 on panic"
    );
    let stderr = String::from_utf8_lossy(&run_output.stderr);
    assert!(
        stderr.contains("panicked") && stderr.contains("in main()") && stderr.contains("explicit"),
        "stderr should contain panic message with location, got: {}",
        stderr
    );
}
