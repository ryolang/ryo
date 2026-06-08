//! Shared test fixtures and helpers for smoke testing.

use std::path::PathBuf;
use std::process::Command;

fn runtime_lib_path() -> PathBuf {
    PathBuf::from(env!("RYO_RUNTIME_LIB"))
}

fn zig_path() -> PathBuf {
    let output = Command::new(env!("CARGO_BIN_EXE_ryo"))
        .args(["toolchain", "status", "--path"])
        .output()
        .expect("failed to execute ryo toolchain status --path");
    assert!(
        output.status.success(),
        "failed to get zig path from ryo: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let path_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
    PathBuf::from(path_str)
}

/// Compiles a Ryo program and links it using the Zig linker.
///
/// Returns the temporary directory (which must be kept alive by the caller)
/// and the path to the compiled executable.
pub fn build_and_link(
    source: &str,
    name: &str,
    extra_link_args: &[&str],
) -> (tempfile::TempDir, PathBuf) {
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

    // Step 2: relink
    let obj = tmp.path().join(format!("{name}.o"));
    let exe = tmp.path().join(format!("{name}_test_binary"));

    let runtime_lib = runtime_lib_path();
    assert!(
        runtime_lib.exists(),
        "runtime archive missing at {} — run `cargo build -p ryo-runtime --release` first",
        runtime_lib.display()
    );

    let zig = zig_path();
    let mut cmd = Command::new(&zig);
    cmd.arg("cc");
    cmd.args(extra_link_args);
    cmd.arg("-o");
    cmd.arg(&exe);
    cmd.arg(&obj);
    cmd.arg(&runtime_lib);
    if cfg!(target_os = "linux") {
        cmd.arg("-lunwind");
    }
    let out = cmd.output().expect("zig cc");
    assert!(
        out.status.success(),
        "zig cc failed with args {:?}:\nstdout: {}\nstderr: {}",
        extra_link_args,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    (tmp, exe)
}

pub const RYO_FIXTURES: &[(&str, &str)] = &[
    (
        "simple_hello",
        "\
fn main():
\ts: str = \"hello\"
\tprint(s)
",
    ),
    (
        "concat_chain",
        "\
fn main():
\ta: str = \"hello\"
\tb: str = \"world\"
\tprint(a + \", \" + b)
",
    ),
    (
        "mut_reassign",
        "\
fn main():
\tmut s: str = int_to_str(42)
\ts = int_to_str(100)
\tprint(s)
",
    ),
    (
        "conditional_move",
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
    ),
    (
        "break_loop",
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
    ),
    (
        "break_inside_loop_owner",
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
    ),
    (
        "pre_loop_owner_last_use_inside_loop",
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
    ),
    (
        "int_to_str_then_print",
        "\
fn main():
\ts: str = int_to_str(42)
\tprint(s)
",
    ),
];
