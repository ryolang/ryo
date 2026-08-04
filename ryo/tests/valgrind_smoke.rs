//! Valgrind leak-detection smoke tests.
//!
//! Compiles representative .ryo programs, links them without any
//! sanitizer, and runs them under `valgrind --leak-check=full`. Any
//! "definitely lost" or "indirectly lost" block fails the test via
//! `--error-exitcode=42`.
//!
//! Why a separate harness from `asan_smoke.rs`: ASan's leak detector
//! (LSan) misses leaks originating from Cranelift-emitted code because
//! Cranelift output is not ASan-instrumented (no `__asan_init`, no
//! `.preinit_array` entry, no stack-root reporting). Valgrind
//! dynamically translates the binary at runtime so it sees every
//! `malloc`/`free` call regardless of how the binary was compiled.
//!
//! This harness is Linux-only — Valgrind on macOS lags upstream by
//! several years and is unreliable on recent Darwin releases.

#![cfg(target_os = "linux")]

mod common;

use std::process::Command;

/// Skip the test (without failing) if `valgrind` is not on PATH.
/// Local dev machines without Valgrind installed should not fail the
/// suite; CI's `valgrind` lane (Dockerfile + ci.yml) installs it
/// explicitly.
fn valgrind_available() -> bool {
    Command::new("valgrind")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Decide whether the smoke test may run. A missing valgrind is a
/// hard failure outside CI: a green local run that silently exercised
/// nothing is worse than a red one (I-085). Opt out explicitly with
/// `RYO_SKIP_VALGRIND=1`. In CI the dedicated `valgrind` lane installs
/// valgrind and runs this suite, so other lanes may skip.
fn require_valgrind(name: &str) -> bool {
    if valgrind_available() {
        return true;
    }
    if std::env::var_os("CI").is_some() {
        eprintln!(
            "skipping {name}: valgrind not installed \
             (covered by the dedicated CI valgrind lane)"
        );
        return false;
    }
    assert!(
        std::env::var_os("RYO_SKIP_VALGRIND").is_some(),
        "valgrind is not installed; these smoke tests exist because LSan \
         misses leaks from Cranelift-emitted code, so skipping them \
         silently would make a green run meaningless. Install valgrind, \
         or set RYO_SKIP_VALGRIND=1 to skip explicitly."
    );
    eprintln!("skipping {name}: valgrind not installed (RYO_SKIP_VALGRIND is set)");
    false
}

fn run_valgrind_smoke(source: &str, name: &str) {
    if !require_valgrind(name) {
        return;
    }

    let (_tmp, exe) = common::build_and_link(source, name, &[]);

    // Step 3: run under Valgrind. `--error-exitcode=42` makes the
    // process exit non-zero if any leak (or other valgrind-detected
    // error) is reported.
    let run = Command::new("valgrind")
        .arg("--leak-check=full")
        .arg("--errors-for-leak-kinds=definite,indirect")
        .arg("--error-exitcode=42")
        .arg("--quiet")
        .arg(&exe)
        .output()
        .expect("run binary under valgrind");
    assert!(
        run.status.success(),
        "binary {name} leaked under valgrind:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn valgrind_simple_hello() {
    run_valgrind_smoke(common::find_fixture("simple_hello"), "simple_hello");
}

#[test]
fn valgrind_int_to_str_then_print() {
    run_valgrind_smoke(
        common::find_fixture("int_to_str_then_print"),
        "int_to_str_then_print",
    );
}

#[test]
fn valgrind_mut_reassign() {
    run_valgrind_smoke(common::find_fixture("mut_reassign"), "mut_reassign");
}

#[test]
fn valgrind_break_inside_loop_owner_after_read() {
    run_valgrind_smoke(
        common::find_fixture("break_inside_loop_owner"),
        "break_inside_loop_owner",
    );
}

#[test]
fn valgrind_pre_loop_owner_last_use_inside_loop() {
    run_valgrind_smoke(
        common::find_fixture("pre_loop_owner_last_use_inside_loop"),
        "pre_loop_owner_last_use_inside_loop",
    );
}

#[test]
fn valgrind_break_before_last_use() {
    run_valgrind_smoke(
        common::find_fixture("break_before_last_use"),
        "break_before_last_use",
    );
}

#[test]
fn valgrind_continue_before_last_use() {
    run_valgrind_smoke(
        common::find_fixture("continue_before_last_use"),
        "continue_before_last_use",
    );
}

#[test]
fn valgrind_break_in_else_arm_sibling_use() {
    run_valgrind_smoke(
        common::find_fixture("break_in_else_arm_sibling_use"),
        "break_in_else_arm_sibling_use",
    );
}

#[test]
fn valgrind_concat_chain() {
    run_valgrind_smoke(common::find_fixture("concat_chain"), "concat_chain");
}

#[test]
fn valgrind_conditional_move() {
    run_valgrind_smoke(common::find_fixture("conditional_move"), "conditional_move");
}

#[test]
fn valgrind_break_loop() {
    run_valgrind_smoke(common::find_fixture("break_loop"), "break_loop");
}

#[test]
fn valgrind_inout_str_reassign_in_callee() {
    run_valgrind_smoke(
        common::find_fixture("inout_str_reassign_in_callee"),
        "inout_str_reassign_in_callee",
    );
}

#[test]
fn valgrind_inout_str_reborrow() {
    run_valgrind_smoke(
        common::find_fixture("inout_str_reborrow"),
        "inout_str_reborrow",
    );
}

#[test]
fn valgrind_str_push_growth() {
    run_valgrind_smoke(common::find_fixture("str_push_growth"), "str_push_growth");
}

#[test]
fn valgrind_reassign_inside_if() {
    run_valgrind_smoke(
        common::find_fixture("reassign_inside_if"),
        "reassign_inside_if",
    );
}

#[test]
fn valgrind_dead_reassign_if_taken() {
    run_valgrind_smoke(
        common::find_fixture("dead_reassign_if_taken"),
        "dead_reassign_if_taken",
    );
}

#[test]
fn valgrind_dead_reassign_if_fallthrough() {
    run_valgrind_smoke(
        common::find_fixture("dead_reassign_if_fallthrough"),
        "dead_reassign_if_fallthrough",
    );
}

#[test]
fn valgrind_dead_reassign_while_taken() {
    run_valgrind_smoke(
        common::find_fixture("dead_reassign_while_taken"),
        "dead_reassign_while_taken",
    );
}

#[test]
fn valgrind_dead_reassign_while_zero() {
    run_valgrind_smoke(
        common::find_fixture("dead_reassign_while_zero"),
        "dead_reassign_while_zero",
    );
}

#[test]
fn valgrind_dead_reassign_for_zero() {
    run_valgrind_smoke(
        common::find_fixture("dead_reassign_for_zero"),
        "dead_reassign_for_zero",
    );
}

#[test]
fn valgrind_last_use_in_loop() {
    run_valgrind_smoke(common::find_fixture("last_use_in_loop"), "last_use_in_loop");
}

#[test]
fn valgrind_last_use_in_if_fallthrough() {
    run_valgrind_smoke(
        common::find_fixture("last_use_in_if_fallthrough"),
        "last_use_in_if_fallthrough",
    );
}

#[test]
fn valgrind_early_return_live_local() {
    run_valgrind_smoke(
        common::find_fixture("early_return_live_local"),
        "early_return_live_local",
    );
}

#[test]
fn valgrind_slice_view_no_free() {
    run_valgrind_smoke(
        common::find_fixture("slice_view_no_free"),
        "slice_view_no_free",
    );
}

#[test]
fn valgrind_slice_owner_freed_after_view() {
    run_valgrind_smoke(
        common::find_fixture("slice_owner_freed_after_view"),
        "slice_owner_freed_after_view",
    );
}

#[test]
fn valgrind_slice_of_literal() {
    run_valgrind_smoke(common::find_fixture("slice_of_literal"), "slice_of_literal");
}

#[test]
fn valgrind_slice_across_blocks() {
    run_valgrind_smoke(
        common::find_fixture("slice_across_blocks"),
        "slice_across_blocks",
    );
}

#[test]
fn valgrind_str_materialize_copy() {
    run_valgrind_smoke(
        common::find_fixture("str_materialize_copy"),
        "str_materialize_copy",
    );
}
