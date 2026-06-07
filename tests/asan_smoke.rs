//! ASan leak-detection smoke tests for M8.1c.
//!
//! Compiles representative .ryo programs, then re-links the object
//! file with `-fsanitize=address` via zig cc and runs the binary.
//! Any ASan-detected leak or memory error fails the test.
//! Skipped on platforms where AOT linking isn't supported.

#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::path::PathBuf;
use std::process::Command;

fn ryo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn runtime_lib_path() -> PathBuf {
    // Prefer release build (CI's prerequisite step in ci.yml). Fall
    // back to debug if release isn't there.
    let release = ryo_root().join("target/release/libryo_runtime.a");
    if release.exists() {
        return release;
    }
    ryo_root().join("target/debug/libryo_runtime.a")
}

fn zig_path() -> Option<PathBuf> {
    // toolchain.rs installs zig at ~/.ryo/toolchain/zig-<version>/zig.
    // Probe that directory first; fall back to PATH lookup.
    if let Ok(home) = std::env::var("HOME") {
        let toolchain_dir = PathBuf::from(home).join(".ryo/toolchain");
        if let Ok(entries) = std::fs::read_dir(&toolchain_dir) {
            for entry in entries.flatten() {
                let zig = entry.path().join("zig");
                if zig.exists() {
                    return Some(zig);
                }
            }
        }
    }
    Some(PathBuf::from("zig"))
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

    let zig =
        zig_path().expect("zig binary not found — install via `cargo run -- toolchain install`");
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
