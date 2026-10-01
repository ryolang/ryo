mod common;
use common::*;

use std::process::Command;
use tempfile::TempDir;

// ============================================================================
// Milestone 9.2: process_exit CLI intrinsic
// ============================================================================

#[test]
fn exit_code_3_jit() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tprocess_exit(3)\n";
    let test_file = create_test_file(temp_dir.path(), "exit_code_3.ryo", code);

    let output = run_ryo_command(&["run", "exit_code_3.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert_eq!(
        output.status.code(),
        Some(3),
        "process_exit(3) should exit 3. stdout: {}, stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn exit_code_3_aot() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tprocess_exit(3)\n";
    let test_file = create_test_file(temp_dir.path(), "exit_code_3_aot.ryo", code);

    let build_output = run_ryo_build(&test_file, temp_dir.path());
    assert!(
        build_output.status.success(),
        "ryo build failed. STDERR: {}",
        String::from_utf8_lossy(&build_output.stderr)
    );

    let binary_path = exe_path(temp_dir.path(), "exit_code_3_aot");
    let run_output = Command::new(&binary_path)
        .output()
        .expect("Failed to execute compiled binary");

    assert_eq!(
        run_output.status.code(),
        Some(3),
        "binary should exit 3. stdout: {}, stderr: {}",
        String::from_utf8_lossy(&run_output.stdout),
        String::from_utf8_lossy(&run_output.stderr),
    );
}

#[test]
fn exit_after_output_jit() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tprint(\"before\\n\")\n\tprocess_exit(7)\n";
    let test_file = create_test_file(temp_dir.path(), "exit_after_output.ryo", code);

    let output = run_ryo_command(&["run", "exit_after_output.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert_eq!(
        output.status.code(),
        Some(7),
        "process_exit(7) should exit 7. stdout: {}, stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("before"),
        "stdout should contain 'before', got: {}",
        stdout
    );
}

#[test]
fn exit_expr_position_rejected() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tx = process_exit(1)\n";
    let test_file = create_test_file(temp_dir.path(), "exit_expr_pos.ryo", code);

    let output = run_ryo_command(&["run", "exit_expr_pos.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert_ne!(
        output.status.code(),
        Some(0),
        "never in expression position should be a compile error"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("E0017"),
        "stderr should contain E0017, got: {}",
        stderr
    );
}
