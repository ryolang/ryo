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
