//! ASan leak-detection smoke tests for M8.1c.
//!
//! Compiles representative .ryo programs, then re-links the object
//! file with `-fsanitize=address` via zig cc and runs the binary.
//! Any ASan-detected leak or memory error fails the test.
//! Skipped on platforms where AOT linking isn't supported.

#![cfg(any(target_os = "linux", target_os = "macos"))]

mod common;

use std::process::Command;

fn run_asan_smoke(source: &str, name: &str) {
    let (_tmp, exe) = common::build_and_link(source, name, &["-fsanitize=address"]);

    // Step 3: run with leak detection
    let run = Command::new(&exe)
        .env("ASAN_OPTIONS", "detect_leaks=1:halt_on_error=1")
        .env("LSAN_OPTIONS", "detect_leaks=1")
        .output()
        .expect("run binary");
    assert!(
        run.status.success(),
        "binary {name} exited with leak/memory error:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn asan_simple_hello() {
    run_asan_smoke(common::find_fixture("simple_hello"), "simple_hello");
}

#[test]
fn asan_concat_chain() {
    run_asan_smoke(common::find_fixture("concat_chain"), "concat_chain");
}

#[test]
fn asan_mut_reassign() {
    run_asan_smoke(common::find_fixture("mut_reassign"), "mut_reassign");
}

#[test]
fn asan_conditional_move() {
    run_asan_smoke(common::find_fixture("conditional_move"), "conditional_move");
}

#[test]
fn asan_break_loop() {
    run_asan_smoke(common::find_fixture("break_loop"), "break_loop");
}

#[test]
fn asan_break_inside_loop_owner_no_double_free() {
    run_asan_smoke(
        common::find_fixture("break_inside_loop_owner"),
        "break_inside_loop_owner",
    );
}

#[test]
fn asan_pre_loop_owner_last_use_inside_loop_no_double_free() {
    run_asan_smoke(
        common::find_fixture("pre_loop_owner_last_use_inside_loop"),
        "pre_loop_owner_last_use_inside_loop",
    );
}

#[test]
fn asan_break_before_last_use() {
    run_asan_smoke(
        common::find_fixture("break_before_last_use"),
        "break_before_last_use",
    );
}

#[test]
fn asan_continue_before_last_use() {
    run_asan_smoke(
        common::find_fixture("continue_before_last_use"),
        "continue_before_last_use",
    );
}

#[test]
fn asan_break_in_else_arm_sibling_use() {
    run_asan_smoke(
        common::find_fixture("break_in_else_arm_sibling_use"),
        "break_in_else_arm_sibling_use",
    );
}

#[test]
fn asan_int_to_str_then_print() {
    run_asan_smoke(
        common::find_fixture("int_to_str_then_print"),
        "int_to_str_then_print",
    );
}

#[test]
fn asan_inout_str_reassign_in_callee() {
    run_asan_smoke(
        common::find_fixture("inout_str_reassign_in_callee"),
        "inout_str_reassign_in_callee",
    );
}

#[test]
fn asan_inout_str_reborrow() {
    run_asan_smoke(
        common::find_fixture("inout_str_reborrow"),
        "inout_str_reborrow",
    );
}

#[test]
fn asan_str_push_growth() {
    run_asan_smoke(common::find_fixture("str_push_growth"), "str_push_growth");
}

#[test]
fn asan_reassign_inside_if() {
    run_asan_smoke(
        common::find_fixture("reassign_inside_if"),
        "reassign_inside_if",
    );
}
