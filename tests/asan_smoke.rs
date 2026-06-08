//! ASan leak-detection smoke tests for M8.1c.
//!
//! Compiles representative .ryo programs, then re-links the object
//! file with `-fsanitize=address` via zig cc and runs the binary.
//! Any ASan-detected leak or memory error fails the test.
//! Skipped on platforms where AOT linking isn't supported.

#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::path::PathBuf;
use std::process::Command;

fn runtime_lib_path() -> PathBuf {
    PathBuf::from(env!("RYO_RUNTIME_LIB"))
}

fn zig_path() -> PathBuf {
    // toolchain.rs installs zig at ~/.ryo/toolchain/zig-<version>/zig.
    // Probe that directory first; fall back to PATH lookup. When
    // multiple zig-* dirs exist (local dev with stale installs),
    // pick the lexicographically-largest — semver-compatible for
    // "zig-0.X.Y" within a major. Determinism matters: read_dir
    // order is filesystem-dependent.
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
    // Final fallback: assume `zig` is on PATH. The downstream
    // `Command::new(&zig).output()` will surface a clear error if
    // it isn't.
    PathBuf::from("zig")
}

fn run_asan_smoke(source: &str, name: &str) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let src_path = tmp.path().join(format!("{name}.ryo"));
    std::fs::write(&src_path, source).expect("write source");

    // Step 1: ryo build (keep obj)
    let status = Command::new(env!("CARGO_BIN_EXE_ryo"))
        .arg("build")
        .arg(&src_path)
        .env("RYO_KEEP_OBJ", "1")
        .current_dir(tmp.path())
        .status()
        .expect("ryo build");
    assert!(status.success(), "ryo build failed for {name}");

    // Step 2: relink with ASan
    let obj = tmp.path().join(format!("{name}.o"));
    let exe = tmp.path().join(format!("{name}_asan"));

    let runtime_lib = runtime_lib_path();
    assert!(
        runtime_lib.exists(),
        "runtime archive missing at {} — run `cargo build -p ryo-runtime --release` first",
        runtime_lib.display()
    );

    let zig = zig_path();
    let mut cmd = Command::new(&zig);
    cmd.args(["cc", "-fsanitize=address", "-o"]);
    cmd.arg(&exe);
    cmd.arg(&obj);
    cmd.arg(&runtime_lib);
    if cfg!(target_os = "linux") {
        cmd.arg("-lunwind");
    }
    let out = cmd.output().expect("zig cc");
    assert!(
        out.status.success(),
        "zig cc -fsanitize=address failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
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
    run_asan_smoke(
        "\
fn main():
\ts: str = \"hello\"
\tprint(s)
",
        "simple_hello",
    );
}

#[test]
fn asan_concat_chain() {
    run_asan_smoke(
        "\
fn main():
\ta: str = \"hello\"
\tb: str = \"world\"
\tprint(a + \", \" + b)
",
        "concat_chain",
    );
}

#[test]
fn asan_mut_reassign() {
    run_asan_smoke(
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
fn asan_conditional_move() {
    run_asan_smoke(
        "\
fn consume(move s: str):
\tprint(s)

fn main():
\ts: str = int_to_str(42)
\tflag: bool = false
\tif flag:
\t\tconsume(s)
\telse:
\t\tprint(s)
",
        "conditional_move",
    );
}

#[test]
fn asan_break_loop() {
    run_asan_smoke(
        "\
fn main():
\ts: str = int_to_str(7)
\tmut i: int = 0
\twhile i < 10:
\t\tprint(s)
\t\tif i == 0:
\t\t\tbreak
\t\ti = i + 1
",
        "break_loop",
    );
}

#[test]
fn asan_break_inside_loop_owner_no_double_free() {
    // Regression for Bug 3 in the M8.1c review.
    // After Bug 1 & 2 are fixed, an inside-loop `s` gets scheduled
    // for a last-use Free anchored after `print(s)`. Without the
    // schedule_break_continue_frees tightening, the break site
    // also schedules a Free for `s` — double free under ASan.
    run_asan_smoke(
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
        "break_inside_loop_owner",
    );
}

#[test]
fn asan_pre_loop_owner_last_use_inside_loop_no_double_free() {
    // Regression: the last-use of `s` is the print() inside the
    // loop body. The post-Task-4 schedule registers a last-use
    // Free anchored at that print. The break site previously also
    // freed `s` because schedule_break_continue_frees only guarded
    // against last-uses *after* the loop instruction.
    run_asan_smoke(
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
