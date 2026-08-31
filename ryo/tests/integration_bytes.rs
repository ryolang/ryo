mod common;
use common::*;

use tempfile::TempDir;

// ---------------------------------------------------------------------------
// M8.4.2: bytes/bytesview — compile-and-run, runtime panics, diagnostics.
// Same black-box pattern as integration_views.rs: assertions target exit
// codes, stderr/stdout text, and E-codes — never compiler internals.
// ---------------------------------------------------------------------------

/// Compile-and-run asserting exact stdout (the `print` path).
#[allow(dead_code)] // First print-path consumer lands in the next codegen task.
fn assert_ryo_prints(test_name: &str, code: &str, expected_stdout: &str) {
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
    assert_eq!(stdout, expected_stdout, "stdout mismatch");
}

#[test]
fn test_bytes_literal_and_len() {
    assert_ryo_runs(
        "bytes_len.ryo",
        "fn main():\n\tb = b\"\\x01\\x02\\x03\"\n\tassert(b.len() == 3, \"len\")\n\tassert(not b.is_empty(), \"empty\")\n\te = b\"\"\n\tassert(e.is_empty(), \"empty literal\")\n",
    );
}
