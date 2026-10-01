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
// Milestone 9.2: process_argc / process_argv CLI intrinsics
// ============================================================================

/// Prints argc on the first line, then every argv entry (argv[0] included)
/// on its own line. With program args `a b`, argc is 3 everywhere (JIT and
/// AOT): argv[0] is the invocation path — the source file under `ryo run`,
/// the binary path for a standalone executable.
const ARGS_ECHO: &str = "\
fn main():
\tn = process_argc()
\tprint(n)
\tprint(\"\\n\")
\tmut i = 0
\twhile i < n:
\t\tprint(process_argv(i))
\t\tprint(\"\\n\")
\t\ti += 1
";

#[test]
fn args_echo_jit_forwarding() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "args_echo.ryo", ARGS_ECHO);

    // Program args must follow the source file on the command line, so
    // `run_ryo_command` (which appends the file last) cannot express
    // this — build the command directly (same as the Task 5 shim test).
    let output = Command::new(env!("CARGO_BIN_EXE_ryo"))
        .arg("run")
        .arg(&test_file)
        .args(["a", "b"])
        .output()
        .expect("Failed to run ryo run command");

    assert!(
        output.status.success(),
        "args_echo should exit 0. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines.len(),
        4,
        "expected 4 stdout lines (argc + 3 args), got: {stdout:?}"
    );
    assert_eq!(lines[0], "3", "argc should be 3 (argv[0] + a + b)");
    assert_eq!(
        lines[1],
        test_file.to_string_lossy(),
        "JIT argv[0] should be the source file path"
    );
    assert_eq!(lines[2], "a");
    assert_eq!(lines[3], "b");
}

#[test]
fn args_echo_aot() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "args_echo_aot.ryo", ARGS_ECHO);

    let build_output = run_ryo_build(&test_file, temp_dir.path());
    assert!(
        build_output.status.success(),
        "ryo build failed. STDERR: {}",
        String::from_utf8_lossy(&build_output.stderr)
    );

    let binary_path = exe_path(temp_dir.path(), "args_echo_aot");
    let run_output = Command::new(&binary_path)
        .args(["a", "b"])
        .output()
        .expect("Failed to execute compiled binary");

    assert!(
        run_output.status.success(),
        "args_echo binary should exit 0. stderr: {}",
        String::from_utf8_lossy(&run_output.stderr)
    );
    let stdout = String::from_utf8_lossy(&run_output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines.len(),
        4,
        "expected 4 stdout lines (argc + 3 args), got: {stdout:?}"
    );
    assert_eq!(lines[0], "3", "argc should be 3 (argv[0] + a + b)");
    assert!(
        lines[1].contains("args_echo_aot"),
        "AOT argv[0] should be the binary path, got: {}",
        lines[1]
    );
    assert_eq!(lines[2], "a");
    assert_eq!(lines[3], "b");
}

#[test]
fn args_out_of_range_panics_jit() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tprocess_argv(99)\n";
    let test_file = create_test_file(temp_dir.path(), "args_oob.ryo", code);

    let output = run_ryo_command(&["run", "args_oob.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert_eq!(
        output.status.code(),
        Some(101),
        "out-of-range process_argv should exit 101. stdout: {}",
        String::from_utf8_lossy(&output.stdout),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("process_argv index out of range"),
        "stderr should contain the out-of-range message, got: {}",
        stderr
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

// ============================================================================
// Milestone 9.2: process_env CLI intrinsic
// ============================================================================

/// `RYO_M92_TEST` is set on the child command; the source prints the looked-up
/// value on line 1 and an unset variable (empty string) on line 2. Deliberately
/// NOT `HOME` or `PATH` — the Windows CI job's values are not predictable.
#[test]
fn env_present_and_unset_jit() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tprint(process_env(\"RYO_M92_TEST\"))\n\tprint(\"\\n\")\n\tprint(process_env(\"RYO_M92_DEFINITELY_UNSET\"))\n\tprint(\"\\n\")\n";
    let test_file = create_test_file(temp_dir.path(), "env_echo.ryo", code);

    // `run_ryo_command` takes no env hook, so build the command directly
    // (same as the args_echo test). The JIT runs in this process, so the
    // variable lands in the compiled program's own environment.
    let output = Command::new(env!("CARGO_BIN_EXE_ryo"))
        .arg("run")
        .arg(&test_file)
        .env("RYO_M92_TEST", "hello-from-env")
        .output()
        .expect("Failed to run ryo run command");

    assert!(
        output.status.success(),
        "env_echo should exit 0. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 2, "expected 2 stdout lines, got: {stdout:?}");
    assert_eq!(lines[0], "hello-from-env", "set variable returns its value");
    assert_eq!(lines[1], "", "unset variable returns the empty string");
}

// ============================================================================
// Milestone 9.2: io_read_line CLI intrinsic
// ============================================================================

/// `line = io_read_line()` then `print(line)`: the trailing newline of
/// the piped line is stripped; a closed/empty stdin yields the empty
/// string (EOF is not an error), still exit 0.
#[test]
fn echo_stdin_jit() {
    use std::io::Write;
    use std::process::Stdio;

    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tline = io_read_line()\n\tprint(line)\n";
    let test_file = create_test_file(temp_dir.path(), "echo_stdin.ryo", code);

    // stdin must be piped, so build the command directly (same as the
    // args_echo test). wait_with_output drops stdin before waiting, so
    // the child sees a clean EOF after the line.
    let mut child = Command::new(env!("CARGO_BIN_EXE_ryo"))
        .arg("run")
        .arg(&test_file)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn ryo run");
    child
        .stdin
        .as_mut()
        .expect("stdin should be piped")
        .write_all(b"hello\n")
        .expect("Failed to write to child stdin");
    let output = child.wait_with_output().expect("Failed to wait on child");

    assert!(
        output.status.success(),
        "echo_stdin should exit 0. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "hello",
        "the trailing newline must be stripped"
    );

    // Second run: closed/empty stdin — EOF before any byte yields the
    // empty line, still exit 0.
    let output = Command::new(env!("CARGO_BIN_EXE_ryo"))
        .arg("run")
        .arg(&test_file)
        .stdin(Stdio::null())
        .output()
        .expect("Failed to run ryo run command");
    assert!(
        output.status.success(),
        "empty stdin should exit 0. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "",
        "EOF before any byte yields the empty line"
    );
}
