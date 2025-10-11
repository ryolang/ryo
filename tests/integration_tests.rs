use std::path::{Path, PathBuf};
use std::process::Command;
use std::{env, fs};
use tempfile::TempDir;

// Helper to clean up generated files
struct TestCleanup {
    files_to_remove: Vec<String>,
}

impl TestCleanup {
    fn new() -> Self {
        Self {
            files_to_remove: Vec::new(),
        }
    }

    fn track(&mut self, filename: &str) {
        self.files_to_remove.push(filename.to_string());
    }
}

impl Drop for TestCleanup {
    fn drop(&mut self) {
        for file in &self.files_to_remove {
            let _ = fs::remove_file(file);
        }
    }
}

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
fn test_output_filename_generation() {
    let mut cleanup = TestCleanup::new();
    let base_name = "mytest";
    let source_filename = format!("{}.ryo", base_name);
    let object_name = format!("{}.{}", base_name, if cfg!(windows) { "obj" } else { "o" });
    let executable_name = format!("{}{}", base_name, env::consts::EXE_SUFFIX);

    cleanup.track(&object_name);
    cleanup.track(&executable_name);

    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), &source_filename, "1 + 2");

    let output =
        run_ryo_command(&["run", "mytest.ryo"], &test_file).expect("Failed to run ryo command");

    // Check that the command succeeded
    if !output.status.success() {
        println!("STDOUT: {}", String::from_utf8_lossy(&output.stdout));
        println!("STDERR: {}", String::from_utf8_lossy(&output.stderr));
        panic!("Command failed");
    }

    // Verify output files were created with correct names in current directory
    assert!(
        PathBuf::from(&object_name).exists(),
        "Object file '{}' was not created",
        object_name
    );
    assert!(
        PathBuf::from(&executable_name).exists(),
        "Executable '{}' was not created",
        executable_name
    );

    // Verify the executable actually works
    let exec_output = Command::new("./mytest")
        .output()
        .expect("Failed to run generated executable");

    // The expression "1 + 2" should exit with code 3
    assert_eq!(exec_output.status.code(), Some(3));
}

#[test]
fn test_different_filename_stems() {
    let mut cleanup = TestCleanup::new();

    let temp_dir = TempDir::new().expect("Failed to create temp directory");

    let test_cases = [
        ("calc_test.ryo", "calc_test"),
        ("math_prog.ryo", "math_prog"),
        ("simple_test.ryo", "simple_test"),
    ];

    for (input_file, expected_stem) in test_cases {
        cleanup.track(&format!("{}.o", expected_stem));
        cleanup.track(expected_stem);

        let test_file_path = create_test_file(temp_dir.path(), input_file, "5 * 6");

        let output = run_ryo_command(&["run", input_file], &test_file_path)
            .expect("Failed to run ryo command");

        if !output.status.success() {
            println!("STDOUT: {}", String::from_utf8_lossy(&output.stdout));
            println!("STDERR: {}", String::from_utf8_lossy(&output.stderr));
            panic!("Command failed for {}", input_file);
        }

        // Check output files in current directory
        let object_file = PathBuf::from(format!("{}.o", expected_stem));
        let executable_file = PathBuf::from(expected_stem);

        assert!(
            object_file.exists(),
            "Object file '{}.o' was not created",
            expected_stem
        );
        assert!(
            executable_file.exists(),
            "Executable '{}' was not created",
            expected_stem
        );

        // Verify execution
        let exec_output = Command::new(format!("./{}", expected_stem))
            .output()
            .expect("Failed to run generated executable");

        assert_eq!(exec_output.status.code(), Some(30)); // 5 * 6 = 30
    }
}

#[test]
fn test_lex_command_integration() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "tokens.ryo", "1 + 2 * 3");

    let output =
        run_ryo_command(&["lex", "tokens.ryo"], &test_file).expect("Failed to run ryo lex command");

    if !output.status.success() {
        println!("STDOUT: {}", String::from_utf8_lossy(&output.stdout));
        println!("STDERR: {}", String::from_utf8_lossy(&output.stderr));
        panic!("Lex command failed");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Verify token output contains expected tokens
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
    assert!(
        !PathBuf::from("tokens").exists(),
        "Executable should not be created for lex command"
    );
}

#[test]
fn test_parse_error_handling() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "invalid.ryo", "1 + @ invalid");

    let output =
        run_ryo_command(&["run", "invalid.ryo"], &test_file).expect("Failed to run ryo command");

    // Command should fail with parse error
    assert!(
        !output.status.success(),
        "Command should fail with parse error"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ParseError")
            || stderr.contains("Parse error")
            || stderr.contains("Error:"),
        "Should contain parse error message, got: {}",
        stderr
    );

    // Verify no output files are created on parse error
    assert!(
        !PathBuf::from("invalid.o").exists(),
        "Object file should not be created on parse error"
    );
    assert!(
        !PathBuf::from("invalid").exists(),
        "Executable should not be created on parse error"
    );
}

#[test]
fn test_file_not_found_error() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let nonexistent_path = temp_dir.path().join("nonexistent.ryo");

    let output = run_ryo_command(&["run", "nonexistent.ryo"], &nonexistent_path)
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

#[test]
fn test_complex_expression_compilation() {
    let mut cleanup = TestCleanup::new();
    cleanup.track("complex_math.o");
    cleanup.track("complex_math");

    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let complex_expr = "((10 + 5) * 2) - (8 / 4)"; // Should evaluate to 28
    let test_file = create_test_file(temp_dir.path(), "complex_math.ryo", complex_expr);

    let output = run_ryo_command(&["run", "complex_math.ryo"], &test_file)
        .expect("Failed to run ryo command");

    if !output.status.success() {
        println!("STDOUT: {}", String::from_utf8_lossy(&output.stdout));
        println!("STDERR: {}", String::from_utf8_lossy(&output.stderr));
        panic!("Complex expression compilation failed");
    }

    // Verify execution result
    let exec_output = Command::new("./complex_math")
        .output()
        .expect("Failed to run generated executable");

    assert_eq!(exec_output.status.code(), Some(28));
}
