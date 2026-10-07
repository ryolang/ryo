//! E2E for tail-call codegen: eligible self-tail-calls run in O(1) stack, so
//! tail recursion far beyond the old SIGSEGV ceiling completes. 10M frames is
//! ~50x the depth that used to crash; without `return_call` the stack-limit
//! check aborts (exit 101) long before the count reaches zero.

mod common;
use common::*;

use std::process::Command;
use tempfile::TempDir;

const COUNT_SRC: &str = "fn count(n: int) -> int:\n\tif n == 0:\n\t\treturn 0\n\treturn count(n - 1)\n\nfn main():\n\tprint(count(10000000))\n";

#[test]
fn aot_tail_recursion_constant_stack() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "tail_calls_aot.ryo", COUNT_SRC);

    let build_output = run_ryo_build(&test_file, temp_dir.path());
    assert!(
        build_output.status.success(),
        "ryo build failed. STDERR: {}",
        String::from_utf8_lossy(&build_output.stderr)
    );

    let binary_path = exe_path(temp_dir.path(), "tail_calls_aot");
    let run_output = Command::new(&binary_path)
        .output()
        .expect("Failed to execute compiled binary");

    assert!(
        run_output.status.success(),
        "tail recursion to depth 10M should complete. stdout: {}, stderr: {}",
        String::from_utf8_lossy(&run_output.stdout),
        String::from_utf8_lossy(&run_output.stderr),
    );
    assert_eq!(
        String::from_utf8_lossy(&run_output.stdout),
        "0",
        "count(10000000) should print 0"
    );
}

#[test]
fn jit_tail_recursion_constant_stack() {
    assert_ryo_output("tail_calls_jit.ryo", COUNT_SRC, "0");
}
