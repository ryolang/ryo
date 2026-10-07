//! E2E for the I-177 stack-limit check: unbounded recursion must
//! abort with the runtime's "stack overflow" diagnostic (exit 101) —
//! not with an OS guard-page SIGSEGV. The `+ 1` after the recursive
//! call keeps the call non-tail, so no tail-call rescue (I-178) can
//! mask a missing check.

mod common;
use common::*;

use std::process::Command;
use tempfile::TempDir;

const RECURSION_SRC: &str = "fn f(n: int) -> int:\n\treturn f(n + 1) + 1\n\nfn main():\n\tf(0)\n";

/// Build `code` with `ryo build`, run the AOT binary, and assert the
/// stack-limit abort contract: exit 101 and the stderr message.
fn assert_aot_stack_overflow(name: &str, code: &str) {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), name, code);

    let build_output = run_ryo_build(&test_file, temp_dir.path());
    assert!(
        build_output.status.success(),
        "ryo build failed. STDERR: {}",
        String::from_utf8_lossy(&build_output.stderr)
    );

    let binary_path = exe_path(temp_dir.path(), name.trim_end_matches(".ryo"));
    let run_output = Command::new(&binary_path)
        .output()
        .expect("Failed to execute compiled binary");

    assert_eq!(
        run_output.status.code(),
        Some(101),
        "stack overflow should exit 101. stdout: {}",
        String::from_utf8_lossy(&run_output.stdout),
    );
    let stderr = String::from_utf8_lossy(&run_output.stderr);
    assert!(
        stderr.contains("stack overflow"),
        "stderr should contain the stack overflow message, got: {}",
        stderr
    );
}

#[test]
fn aot_recursion_aborts_with_stack_overflow() {
    assert_aot_stack_overflow("stack_overflow_aot.ryo", RECURSION_SRC);
}

#[test]
fn jit_recursion_aborts_with_stack_overflow() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "stack_overflow_jit.ryo", RECURSION_SRC);

    let output = run_ryo_command(&["run", "stack_overflow_jit.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert_eq!(
        output.status.code(),
        Some(101),
        "stack overflow should exit 101. stdout: {}",
        String::from_utf8_lossy(&output.stdout),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("stack overflow"),
        "stderr should contain the stack overflow message, got: {}",
        stderr
    );
}
