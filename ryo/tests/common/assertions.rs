use super::runner::{create_test_file, run_ryo_command};
use tempfile::TempDir;
pub fn assert_ryo_runs(test_name: &str, code: &str) {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), test_name, code);
    let output =
        run_ryo_command(&["run", test_name], &test_file).expect("Failed to run ryo command");
    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Assert the program compiles, runs under JIT, and prints exactly
/// `expected` on stdout. In default mode stdout IS the program's own
/// output — the compiler prints nothing else (`print` appends no
/// newline, so multiple prints concatenate).
pub fn assert_ryo_output(test_name: &str, code: &str, expected: &str) {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), test_name, code);
    let output =
        run_ryo_command(&["run", test_name], &test_file).expect("Failed to run ryo command");
    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout, expected, "stdout mismatch");
}

/// A `never` value anywhere but a bare statement is a compile error
/// — `panic` diverges and produces no value to bind, return, pass,
/// or operate on. Assert on the user-facing message, not the exit
/// code alone.
pub fn assert_never_rejected(file_name: &str, code: &str) {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), file_name, code);

    let output =
        run_ryo_command(&["run", file_name], &test_file).expect("Failed to run ryo run command");

    assert!(
        !output.status.success(),
        "never-binding should be rejected. stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("'never' value"),
        "stderr should name the never-binding error, got: {}",
        stderr
    );
}

/// Runs `code` via the JIT and asserts an "integer overflow" panic
/// (stderr message + nonzero exit).
pub fn assert_int_overflow_panics(name: &str, code: &str) {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), name, code);

    let output =
        run_ryo_command(&["run", name], &test_file).expect("Failed to run ryo run command");

    assert_eq!(
        output.status.code(),
        Some(101),
        "integer overflow should exit 101. stdout: {}",
        String::from_utf8_lossy(&output.stdout),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("integer overflow"),
        "stderr should contain overflow message, got: {}",
        stderr
    );
}

/// Runs `code` via the JIT and asserts it completes successfully.
pub fn assert_program_succeeds(name: &str, code: &str) {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), name, code);

    let output =
        run_ryo_command(&["run", name], &test_file).expect("Failed to run ryo run command");

    assert!(
        output.status.success(),
        "{} should succeed. STDERR: {}",
        name,
        String::from_utf8_lossy(&output.stderr)
    );
}
