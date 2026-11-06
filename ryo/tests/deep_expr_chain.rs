//! Regression tests for deeply nested operator chains.
//!
//! A ~7k-term `1+1+...` chain used to abort the compiler with
//! `thread has overflowed its stack` (SIGABRT): every pass that walks
//! expression nesting recursed one frame per chain link. Those walks
//! are iterative now; these tests pin the fix end to end through the
//! CLI (which runs the compiler on a 32 MiB thread — the recursion
//! overflowed it at ~7k terms).

mod common;
use common::*;

use std::process::{Command, Stdio};
use tempfile::TempDir;

/// `print(1+1+... )` with `terms` ones.
fn add_chain(terms: usize) -> String {
    let mut src = String::from("fn main():\n\tprint(");
    for i in 0..terms {
        if i > 0 {
            src.push('+');
        }
        src.push('1');
    }
    src.push_str(")\n");
    src
}

fn right_nested_add_chain(terms: usize) -> String {
    let mut src = String::from("fn main():\n\tprint(");
    for _ in 1..terms {
        src.push_str("1+(");
    }
    src.push('1');
    for _ in 1..terms {
        src.push(')');
    }
    src.push_str(")\n");
    src
}

#[test]
fn jit_run_right_nested_addition_chain_prints_sum() {
    let terms = 20_000;
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(
        temp_dir.path(),
        "right_nested_chain.ryo",
        &right_nested_add_chain(terms),
    );

    let output = run_ryo_command(&["run", "right_nested_chain.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(
        output.status.success(),
        "right-nested chain compile failed. STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        terms.to_string(),
        "right-nested chain must evaluate to its term count"
    );
}

#[test]
fn jit_run_deep_addition_chain_prints_sum() {
    // 20k terms: well past the old recursion's ~7k overflow threshold.
    let terms = 20_000;
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "deep_chain.ryo", &add_chain(terms));

    let output = run_ryo_command(&["run", "deep_chain.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(
        output.status.success(),
        "deep chain compile failed. STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        terms.to_string(),
        "deep chain must evaluate to its term count"
    );
}

#[test]
fn parse_deep_addition_chain_succeeds() {
    // The tree dump is quadratic in nesting depth (every line carries
    // its full indentation prefix) — discard stdout and assert only
    // the exit code, or this test would buffer gigabytes.
    let terms = 20_000;
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "deep_chain.ryo", &add_chain(terms));

    // `.output()` (not `.status()`): the child must be able to finish
    // writing stderr even when a diagnostic regression floods it past
    // the pipe buffer, and the failure message needs the cause.
    let output = Command::new(env!("CARGO_BIN_EXE_ryo"))
        .arg("parse")
        .arg(&test_file)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .expect("Failed to run ryo parse command");

    assert!(
        output.status.success(),
        "deep chain parse failed (overflow or diagnostics): {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
