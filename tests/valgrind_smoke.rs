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
//! `malloc`/`free` call regardless of how the binary was compiled. See
//! ISSUES.md (I-058 entry) for the full diagnostic.
//!
//! This harness is Linux-only — Valgrind on macOS lags upstream by
//! several years and is unreliable on recent Darwin releases.

#![cfg(target_os = "linux")]

use std::path::PathBuf;
use std::process::Command;

fn runtime_lib_path() -> PathBuf {
    PathBuf::from(env!("RYO_RUNTIME_LIB"))
}

fn zig_path() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        let toolchain_dir = PathBuf::from(home).join(".ryo/toolchain");
        if let Ok(entries) = std::fs::read_dir(&toolchain_dir) {
            let mut candidates: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("zig-"))
                })
                .collect();
            candidates.sort();
            if let Some(latest) = candidates.last() {
                let zig = latest.join("zig");
                if zig.exists() {
                    return zig;
                }
            }
        }
    }
    PathBuf::from("zig")
}

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

fn run_valgrind_smoke(source: &str, name: &str) {
    if !valgrind_available() {
        eprintln!("skipping {name}: valgrind not installed");
        return;
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let src_path = tmp.path().join(format!("{name}.ryo"));
    std::fs::write(&src_path, source).expect("write source");

    // Step 1: ryo build (keep obj for relink).
    let status = Command::new(env!("CARGO_BIN_EXE_ryo"))
        .arg("build")
        .arg(&src_path)
        .env("RYO_KEEP_OBJ", "1")
        .current_dir(tmp.path())
        .status()
        .expect("ryo build");
    assert!(status.success(), "ryo build failed for {name}");

    // Step 2: link a plain (no-sanitizer) binary. Valgrind does the
    // instrumentation at runtime; compile-time flags would only get in
    // the way.
    let obj = tmp.path().join(format!("{name}.o"));
    let exe = tmp.path().join(format!("{name}_valgrind"));

    let runtime_lib = runtime_lib_path();
    assert!(
        runtime_lib.exists(),
        "runtime archive missing at {} — run `cargo build -p ryo-runtime --release` first",
        runtime_lib.display()
    );

    let zig = zig_path();
    let mut cmd = Command::new(&zig);
    cmd.args(["cc", "-o"]);
    cmd.arg(&exe);
    cmd.arg(&obj);
    cmd.arg(&runtime_lib);
    cmd.arg("-lunwind");
    let out = cmd.output().expect("zig cc");
    assert!(
        out.status.success(),
        "zig cc failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

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

// ---- Negative checks: the M8.1c-correct programs should be leak-free. ----
// These mirror the asan_smoke fixtures so we'd notice a regression that
// LSan misses but valgrind catches.

#[test]
fn valgrind_simple_hello() {
    run_valgrind_smoke(
        "\
fn main():
\ts: str = \"hello\"
\tprint(s)
",
        "simple_hello",
    );
}

#[test]
fn valgrind_int_to_str_then_print() {
    run_valgrind_smoke(
        "\
fn main():
\ts: str = int_to_str(42)
\tprint(s)
",
        "int_to_str_then_print",
    );
}

#[test]
fn valgrind_mut_reassign() {
    run_valgrind_smoke(
        "\
fn main():
\tmut s: str = int_to_str(42)
\ts = int_to_str(100)
\tprint(s)
",
        "mut_reassign",
    );
}

#[test]
fn valgrind_break_inside_loop_owner_after_read() {
    // Inside-loop owner whose read happens BEFORE the break — the
    // last-use Free fires every iteration so there's nothing to leak.
    // Pairs with the I-058 limitation: when the read is BEFORE the
    // break the leak doesn't manifest; only the read-AFTER-break
    // shape leaks (see I-058 in ISSUES.md).
    run_valgrind_smoke(
        "\
fn main():
\tmut i: int = 0
\twhile i < 3:
\t\ts: str = int_to_str(i)
\t\tprint(s)
\t\tif i == 1:
\t\t\tbreak
\t\ti += 1
",
        "break_inside_loop_owner_after_read",
    );
}

#[test]
fn valgrind_pre_loop_owner_last_use_inside_loop() {
    // Pre-loop owner read inside the loop. Last-use Free is anchored
    // at the print(); break path executes the print first, so no
    // leak. Same shape as asan_smoke's regression for Bug 3, but
    // verified through Valgrind instead of ASan.
    run_valgrind_smoke(
        "\
fn main():
\ts: str = int_to_str(7)
\tmut i: int = 0
\twhile i < 3:
\t\tprint(s)
\t\tif i == 0:
\t\t\tbreak
\t\ti += 1
",
        "pre_loop_owner_last_use_inside_loop",
    );
}
