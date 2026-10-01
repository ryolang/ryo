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

// ============================================================================
// Milestone 9.2: io_eprint CLI intrinsic
// ============================================================================

#[test]
fn stderr_not_stdout_jit() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tio_eprint(\"err\\n\")\n";
    let test_file = create_test_file(temp_dir.path(), "stderr_not_stdout.ryo", code);

    let output = run_ryo_command(&["run", "stderr_not_stdout.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(
        output.status.success(),
        "io_eprint program should exit 0. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.is_empty(),
        "stdout should be empty, got: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "err\n",
        "io_eprint must write to stderr, not stdout"
    );
}

#[test]
fn eprint_strview_jit() {
    // A strview argument projects the owner's buffer directly —
    // io_eprint(s[0:2]) writes the viewed bytes with no copy.
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\ts = \"hello\"\n\tio_eprint(s[0:2])\n";
    let test_file = create_test_file(temp_dir.path(), "eprint_strview.ryo", code);

    let output = run_ryo_command(&["run", "eprint_strview.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(
        output.status.success(),
        "io_eprint(strview) should exit 0. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "he",
        "io_eprint of a strview should write the viewed bytes"
    );
}

#[test]
fn eprint_no_auto_newline_jit() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tio_eprint(\"a\")\n\tio_eprint(\"b\")\n";
    let test_file = create_test_file(temp_dir.path(), "eprint_no_auto_newline.ryo", code);

    let output = run_ryo_command(&["run", "eprint_no_auto_newline.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(
        output.status.success(),
        "io_eprint program should exit 0. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "ab",
        "io_eprint appends no newline — consecutive calls concatenate"
    );
}

// ============================================================================
// Milestone 9.2: hosted entry shim — trailing CLI args forwarded to the program
// ============================================================================

#[test]
fn trailing_args_forwarded_jit() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tprint(\"ok\")\n";
    let test_file = create_test_file(temp_dir.path(), "trailing_args.ryo", code);

    // Trailing args must follow the source file on the command line, so
    // `run_ryo_command` (which appends the file last) cannot express
    // this — build the command directly.
    let output = Command::new(env!("CARGO_BIN_EXE_ryo"))
        .arg("run")
        .arg(&test_file)
        .args(["--verbose", "x"])
        .output()
        .expect("Failed to run ryo run command");

    assert!(
        output.status.success(),
        "args after the file are the program's, not compiler flags. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "ok",
        "program output should be exactly 'ok'"
    );
}
