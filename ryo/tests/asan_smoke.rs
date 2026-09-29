//! ASan leak-detection smoke tests for M8.1c.
//!
//! Compiles representative .ryo programs, then re-links the object
//! file with `-fsanitize=address` via the host C compiler and runs the
//! binary. Any ASan-detected leak or memory error fails the test.
//!
//! Linux only, and deliberately re-linked with `cc` rather than the
//! managed Zig toolchain: zig cc accepts `-fsanitize=address` without
//! complaint but links no ASan runtime on any platform (verified on
//! macOS and linux-x86_64/aarch64 — the entire lane was vacuous from
//! its introduction until the symbol assertion below), while the host
//! gcc on the glibc CI runners ships a working ASan. The re-link is
//! native (no `-target`), so the sanitizer binaries are glibc-linked;
//! the musl AOT default never applies to this path.

#![cfg(target_os = "linux")]

mod common;

use std::process::Command;

fn run_asan_smoke(source: &str, name: &str) {
    let (_tmp, exe) = common::build_and_link_host_cc(source, name, &["-fsanitize=address"]);

    // Liveness guard: a passing suite is only meaningful if the binary
    // actually carries the ASan runtime. `zig cc -fsanitize=address`
    // accepts the flag without complaint even when no runtime is
    // linked (observed on macOS) — fail loudly instead of passing
    // vacuously.
    let nm = Command::new("nm")
        .arg(&exe)
        .output()
        .expect("run nm on test binary");
    let syms = String::from_utf8_lossy(&nm.stdout);
    assert!(
        syms.contains("__asan_init"),
        "binary {name} has no ASan runtime symbols — the sanitizer link is vacuous.\n\
         nm exit: {:?}, stderr: {}",
        nm.status,
        String::from_utf8_lossy(&nm.stderr)
    );

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
fn asan_cond_heap_temp_in_loop() {
    run_asan_smoke(
        common::find_fixture("cond_heap_temp_in_loop"),
        "cond_heap_temp_in_loop",
    );
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

#[test]
fn asan_dead_reassign_if_taken() {
    run_asan_smoke(
        common::find_fixture("dead_reassign_if_taken"),
        "dead_reassign_if_taken",
    );
}

#[test]
fn asan_dead_reassign_if_fallthrough() {
    run_asan_smoke(
        common::find_fixture("dead_reassign_if_fallthrough"),
        "dead_reassign_if_fallthrough",
    );
}

#[test]
fn asan_dead_reassign_while_taken() {
    run_asan_smoke(
        common::find_fixture("dead_reassign_while_taken"),
        "dead_reassign_while_taken",
    );
}

#[test]
fn asan_dead_reassign_while_zero() {
    run_asan_smoke(
        common::find_fixture("dead_reassign_while_zero"),
        "dead_reassign_while_zero",
    );
}

#[test]
fn asan_dead_reassign_for_zero() {
    run_asan_smoke(
        common::find_fixture("dead_reassign_for_zero"),
        "dead_reassign_for_zero",
    );
}

#[test]
fn asan_last_use_in_loop() {
    run_asan_smoke(common::find_fixture("last_use_in_loop"), "last_use_in_loop");
}

#[test]
fn asan_last_use_in_if_fallthrough() {
    run_asan_smoke(
        common::find_fixture("last_use_in_if_fallthrough"),
        "last_use_in_if_fallthrough",
    );
}

#[test]
fn asan_early_return_live_local() {
    run_asan_smoke(
        common::find_fixture("early_return_live_local"),
        "early_return_live_local",
    );
}

#[test]
fn asan_last_use_in_returning_arm_fallthrough() {
    run_asan_smoke(
        common::find_fixture("last_use_in_returning_arm_fallthrough"),
        "last_use_in_returning_arm_fallthrough",
    );
}

#[test]
fn asan_last_use_in_fallthrough_arm_sibling_returns() {
    run_asan_smoke(
        common::find_fixture("last_use_in_fallthrough_arm_sibling_returns"),
        "last_use_in_fallthrough_arm_sibling_returns",
    );
}

#[test]
fn asan_slice_view_no_free() {
    run_asan_smoke(
        common::find_fixture("slice_view_no_free"),
        "slice_view_no_free",
    );
}

#[test]
fn asan_slice_owner_freed_after_view() {
    run_asan_smoke(
        common::find_fixture("slice_owner_freed_after_view"),
        "slice_owner_freed_after_view",
    );
}

#[test]
fn asan_slice_of_literal() {
    run_asan_smoke(common::find_fixture("slice_of_literal"), "slice_of_literal");
}

#[test]
fn asan_slice_across_blocks() {
    run_asan_smoke(
        common::find_fixture("slice_across_blocks"),
        "slice_across_blocks",
    );
}

#[test]
fn asan_slice_of_struct_field_inline() {
    run_asan_smoke(
        common::find_fixture("slice_of_struct_field_inline"),
        "slice_of_struct_field_inline",
    );
}

#[test]
fn asan_slice_of_struct_field_heap() {
    run_asan_smoke(
        common::find_fixture("slice_of_struct_field_heap"),
        "slice_of_struct_field_heap",
    );
}

#[test]
fn asan_bytes_ops() {
    run_asan_smoke(common::find_fixture("bytes_ops"), "bytes_ops");
}

#[test]
fn asan_struct_leak_check() {
    run_asan_smoke(
        common::find_fixture("struct_leak_check"),
        "struct_leak_check",
    );
}

#[test]
fn asan_self_assign_str_no_double_free() {
    run_asan_smoke(common::find_fixture("self_assign_str"), "self_assign_str");
}

#[test]
fn asan_self_assign_struct_no_double_free() {
    run_asan_smoke(
        common::find_fixture("self_assign_struct"),
        "self_assign_struct",
    );
}

#[test]
fn asan_loop_carried_concat_break_no_double_free() {
    run_asan_smoke(
        common::find_fixture("loop_carried_concat_break"),
        "loop_carried_concat_break",
    );
}

#[test]
fn asan_loop_carried_concat_in_loop_no_double_free() {
    run_asan_smoke(
        common::find_fixture("loop_carried_concat_in_loop"),
        "loop_carried_concat_in_loop",
    );
}

#[test]
fn asan_int_to_str_with_long_live_str() {
    run_asan_smoke(
        common::find_fixture("int_to_str_with_long_live_str"),
        "int_to_str_with_long_live_str",
    );
}

#[test]
fn asan_loop_local_reassign_break_no_leak() {
    run_asan_smoke(
        common::find_fixture("loop_local_reassign_break_leak"),
        "loop_local_reassign_break_leak",
    );
}

#[test]
fn asan_early_return_owned_value_from_loop() {
    run_asan_smoke(
        common::find_fixture("early_return_owned_value_from_loop"),
        "early_return_owned_value_from_loop",
    );
}

#[test]
fn asan_debug_repr_struct_print_frees() {
    run_asan_smoke(
        common::find_fixture("debug_repr_struct_print"),
        "debug_repr_struct_print",
    );
}

#[test]
fn asan_debug_repr_nested_struct_frees() {
    run_asan_smoke(
        common::find_fixture("debug_repr_nested_struct"),
        "debug_repr_nested_struct",
    );
}

#[test]
fn asan_debug_repr_primitives_frees() {
    run_asan_smoke(
        common::find_fixture("debug_repr_primitives"),
        "debug_repr_primitives",
    );
}

#[test]
fn asan_struct_eq_heap_str_fields() {
    run_asan_smoke(
        common::find_fixture("struct_eq_heap_str_fields"),
        "struct_eq_heap_str_fields",
    );
}
