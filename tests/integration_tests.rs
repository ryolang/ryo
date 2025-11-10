use std::path::{Path, PathBuf};
use std::process::Command;
use std::fs;
use tempfile::TempDir;

// Helper function to run ryo compiler and capture output
fn run_ryo_command(
    args: &[&str],
    file_path: &Path,
) -> Result<std::process::Output, std::io::Error> {
    let mut cmd = Command::new("cargo");
    cmd.args(&["run", "--"])
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

    // Verify token output contains expected tokens
    assert!(stdout.contains("Ident(\"x\")"), "Missing x identifier");
    assert!(stdout.contains("Assign"), "Missing Assign token");
    assert!(stdout.contains("Int(\"1\")"), "Missing Int(1) token");
    assert!(stdout.contains("Add"), "Missing Add token");
    assert!(stdout.contains("Int(\"2\")"), "Missing Int(2) token");
    assert!(stdout.contains("Mul"), "Missing Mul token");
    assert!(stdout.contains("Int(\"3\")"), "Missing Int(3) token");

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

    let output =
        run_ryo_command(&["parse", "simple.ryo"], &test_file).expect("Failed to run ryo parse command");

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

    let output =
        run_ryo_command(&["parse", "typed.ryo"], &test_file).expect("Failed to run ryo parse command");

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

    let output =
        run_ryo_command(&["parse", "multi.ryo"], &test_file).expect("Failed to run ryo parse command");

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

    let output =
        run_ryo_command(&["run", "exit_simple.ryo"], &test_file).expect("Failed to run ryo run command");

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

    let output =
        run_ryo_command(&["run", "exit_zero.ryo"], &test_file).expect("Failed to run ryo run command");

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

    let output =
        run_ryo_command(&["run", "exit_div.ryo"], &test_file).expect("Failed to run ryo run command");

    assert!(output.status.success(), "ryo run should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("[Result] => 0"),
        "Should exit with code 0"
    );
}

#[test]
fn test_run_subtraction() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "exit_sub.ryo", "result = 100 - 30");

    let output =
        run_ryo_command(&["run", "exit_sub.ryo"], &test_file).expect("Failed to run ryo run command");

    assert!(output.status.success(), "ryo run should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("[Result] => 0"),
        "Should exit with code 0"
    );
}

#[test]
fn test_run_parenthesized_expression() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "exit_paren.ryo", "result = (10 + 5) * 2");

    let output =
        run_ryo_command(&["run", "exit_paren.ryo"], &test_file).expect("Failed to run ryo run command");

    assert!(output.status.success(), "ryo run should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    // (10 + 5) * 2 = 15 * 2 = 30 (computed), but exit code is 0
    assert!(
        stdout.contains("[Result] => 0"),
        "Should exit with code 0"
    );
}

#[test]
fn test_run_with_type_annotation() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "exit_typed.ryo", "x: int = 99");

    let output =
        run_ryo_command(&["run", "exit_typed.ryo"], &test_file).expect("Failed to run ryo run command");

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

    let output =
        run_ryo_command(&["run", "exit_mut.ryo"], &test_file).expect("Failed to run ryo run command");

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

    let output =
        run_ryo_command(&["run", "exit_neg.ryo"], &test_file).expect("Failed to run ryo run command");

    assert!(output.status.success(), "ryo run should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    // All programs exit with 0 (success)
    assert!(
        stdout.contains("[Result] => 0"),
        "Should exit with code 0"
    );
}

// Milestone 3.5: String Literals and Print Tests

#[test]
fn test_print_hello_world() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(
        temp_dir.path(),
        "hello.ryo",
        "msg = print(\"Hello, World!\")",
    );

    let output =
        run_ryo_command(&["run", "hello.ryo"], &test_file).expect("Failed to run ryo run command");

    assert!(output.status.success(), "ryo run should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("[Result] => 0"),
        "Should exit with code 0"
    );
}

#[test]
fn test_print_with_newline() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(
        temp_dir.path(),
        "newline.ryo",
        "line = print(\"Line\\n\")",
    );

    let output =
        run_ryo_command(&["run", "newline.ryo"], &test_file).expect("Failed to run ryo run command");

    assert!(output.status.success(), "ryo run should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("[Result] => 0"),
        "Should exit with code 0"
    );
}

#[test]
fn test_multiple_print_calls() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(
        temp_dir.path(),
        "multi_print.ryo",
        "a = print(\"First\\n\")\nb = print(\"Second\\n\")\nc = print(\"Third\\n\")",
    );

    let output =
        run_ryo_command(&["run", "multi_print.ryo"], &test_file).expect("Failed to run ryo run command");

    assert!(output.status.success(), "ryo run should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("[Result] => 0"),
        "Should exit with code 0"
    );
}

#[test]
fn test_print_empty_string() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(
        temp_dir.path(),
        "empty.ryo",
        "empty = print(\"\")",
    );

    let output =
        run_ryo_command(&["run", "empty.ryo"], &test_file).expect("Failed to run ryo run command");

    assert!(output.status.success(), "ryo run should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("[Result] => 0"),
        "Should exit with code 0"
    );
}
