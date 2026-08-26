//! Shared test fixtures and helpers for smoke testing.

// Each integration-test binary pulls in this module via `mod common;`
// but uses only a subset of the shared helpers; the rest would trip
// `dead_code` (an error under CI's `-Dwarnings`).
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

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
        "runtime archive missing at {} — it is built by ryo-backend's build.rs; run `cargo build` first",
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
        // A heap temp (`p + "x"`) produced in an if condition inside a
        // loop: non-matching iterations take the not-taken path, which
        // must still free the temp.
        "cond_heap_temp_in_loop",
        "\
fn main():
\tmut s: str = \"\"
\tfor i in range(0, 4):
\t\tstr_push(&s, \"fox \")
\t\tstr_push(&s, \"bar \")
\tmut p: str = \"f\"
\tp = p + \"o\"
\tmut i = 0
\tmut count = 0
\twhile i + 3 <= s.len():
\t\tif s[i:i+3] == p + \"x\":
\t\t\tcount += 1
\t\ti += 1
\tassert(count == 4, \"count\")
\tprint(\"ok\\n\")
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
    (
        "break_before_last_use",
        "\
fn main():
\tmut i: int = 0
\twhile i < 3:
\t\ts: str = int_to_str(i)
\t\tif i == 1:
\t\t\tbreak
\t\tprint(s)
\t\ti += 1
",
    ),
    (
        "continue_before_last_use",
        "\
fn main():
\tmut i: int = 0
\twhile i < 3:
\t\ts: str = int_to_str(i)
\t\ti += 1
\t\tif i == 2:
\t\t\tcontinue
\t\tprint(s)
",
    ),
    (
        "break_in_else_arm_sibling_use",
        "\
fn main():
\tmut i: int = 0
\twhile i < 3:
\t\ts: str = int_to_str(i)
\t\tif i < 2:
\t\t\tprint(s)
\t\telse:
\t\t\tbreak
\t\ti += 1
",
    ),
    (
        // The callee reassigns the inout str param; the replacement
        // escapes via the write-back (callee must not free it), and the
        // caller's old buffer is dropped exactly once.
        "inout_str_reassign_in_callee",
        "\
fn set(inout s: str):
\ts = \"new\"

fn main():
\tmut s = \"old\"
\tset(&s)
\tprint(s)
",
    ),
    (
        // User-fn inout str + reborrow through str_push.
        "inout_str_reborrow",
        "\
fn app(inout s: str):
\tstr_push(&s, \"!\")

fn main():
\tmut s = \"hi\"
\tapp(&s)
\tprint(s)
",
    ),
    (
        // Growth forces a realloc move; the caller must free the
        // write-back triple, not the stale pre-call one (double-free).
        "str_push_growth",
        "\
fn main():
\tmut s = \"hi\"
\tstr_push(&s, \"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\")
\tprint(s)
",
    ),
    (
        // Pre-existing M8.1 bug: reassignment inside a branch,
        // read after the join. The taken arm drops the old buffer
        // (free_on_reassign); the merged value is freed at last use.
        "reassign_inside_if",
        "\
fn main():
\tmut s = \"a\"
\tc = true
\tif c:
\t\ts = \"b\"
\tprint(s)
",
    ),
    (
        // Dead conditional reassign, taken path — the old buffer
        // is dropped by free_on_reassign and the new one by the
        // dead-store Free. Both must be freed exactly once.
        "dead_reassign_if_taken",
        "\
fn main():
\tmut s = \"a\"
\tc = true
\tif c:
\t\ts = \"b\"
",
    ),
    (
        // Dead conditional reassign, NOT-taken path — the
        // reassign never happens; the original buffer must be freed by
        // the arm-gated conditional DeadDrop in the fall-through.
        "dead_reassign_if_fallthrough",
        "\
fn main():
\tmut s = \"a\"
\tc = false
\tif c:
\t\ts = \"b\"
",
    ),
    (
        // Dead reassign in a loop body, taken path — every
        // iteration's old buffer drops via free_on_reassign, and the
        // final value is freed by the after-loop anchor (not a second
        // in-body Free).
        "dead_reassign_while_taken",
        "\
fn main():
\tmut s = \"a\"
\tmut i = 0
\twhile i < 2:
\t\ts = \"b\"
\t\ti += 1
",
    ),
    (
        // Dead reassign in a loop body, ZERO iterations — the
        // pre-loop buffer must still be freed by the after-loop anchor.
        "dead_reassign_while_zero",
        "\
fn main():
\tmut s = \"a\"
\tc = false
\twhile c:
\t\ts = \"b\"
",
    ),
    (
        // Same shape through a for-range loop with an empty
        // range (zero iterations).
        "dead_reassign_for_zero",
        "\
fn main():
\tmut s = \"a\"
\tfor i in range(0, 0):
\t\ts = \"b\"
",
    ),
    (
        // Conditional last use: the binding's last read is inside the
        // loop body. The Free must fire at the loop exit — freeing in
        // the body is a use-after-free on the next iteration.
        "last_use_in_loop",
        "\
fn main():
\tmut s = \"a\"
\tfor i in range(0, 3):
\t\tprint(s)
",
    ),
    (
        // Conditional last use through an if: the read is inside an
        // arm that is NOT taken. The value must still be freed at the
        // merge point.
        "last_use_in_if_fallthrough",
        "\
fn main():
\tmut s = \"a\"
\td = false
\tif d:
\t\tprint(s)
",
    ),
    (
        // Return-epilogue: the else path returns while `s` is still
        // live and its last-use Free anchors in the sibling arm. The
        // early return must destroy the function's locals on ITS path.
        "early_return_live_local",
        "\
fn f():
\tmut s = \"a\"
\td = false
\tif d:
\t\tprint(s)
\telse:
\t\treturn

fn main():
\tf()
",
    ),
    (
        // M8.4: a view must not be freed (it owns nothing), and the
        // owner is freed once at its own last use — after the view's.
        "slice_view_no_free",
        "\
fn main():
\ts: str = int_to_str(42)
\tv = s[0:1]
\tprint(v)
",
    ),
    (
        // The owner outlives the view: s's Free anchors after the
        // last read of EITHER s or its projection v — freeing s at
        // print(s) would be a use-after-free on print(v).
        "slice_owner_freed_after_view",
        "\
fn main():
\ts: str = int_to_str(12345)
\tv = s[0:3]
\tprint(s)
\tprint(v)
",
    ),
    (
        // Slicing a string literal projects rodata (cap=0 sentinel):
        // nothing to free on either side, and no double-free.
        "slice_of_literal",
        "\
fn main():
\tprint(\"hello\"[1:3])
",
    ),
    (
        // A view created before a branch and read inside it: the
        // branch must not prune the projection (P2 freeze survives
        // the if-join), and the owner frees once after the join.
        "slice_across_blocks",
        "\
fn main():
\ts: str = int_to_str(7)
\tv = s[0:1]
\tif s[0:1] == \"7\":
\t\tprint(v)
\tprint(s)
",
    ),
    (
        // M8.4.1.2: str(view) materialization frees. The bound copy x
        // is a defensive copy (the source is mutated after the
        // materialize point, so W0003 stays silent) freed at its last
        // use; the temp copy moves into `eat` (a move param, so no
        // re-borrow redundancy warning) and is freed there. Both the
        // named-init and anon-temp Free paths must release the copy's
        // buffer exactly once.
        "str_materialize_copy",
        "\
fn eat(move text: str):
\tprint(text)

fn main():
\tmut s: str = \"hello\"
\tx: str = str(s[0:2])
\tstr_push(&s, \"!\")
\tprint(x)
\teat(str(s[0:2]))
",
    ),
];

// Test-helper module, not `cfg(test)`-gated, so clippy.toml's
// `allow-panic-in-tests` does not recognize it.
#[allow(clippy::panic)]
pub fn find_fixture(name: &str) -> &'static str {
    RYO_FIXTURES
        .iter()
        .find(|&&(n, _)| n == name)
        .map(|&(_, s)| s)
        .unwrap_or_else(|| panic!("fixture {name} not found"))
}

// Helper function to run ryo compiler and capture output
pub fn run_ryo_command(
    args: &[&str],
    file_path: &Path,
) -> Result<std::process::Output, std::io::Error> {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ryo"));
    cmd.args(&args[..args.len() - 1]) // All args except the filename
        .arg(file_path); // Use absolute path for the file
    cmd.output()
}

// Helper function to create a temporary test file
pub fn create_test_file(dir: &Path, filename: &str, content: &str) -> std::path::PathBuf {
    let file_path = dir.join(filename);
    std::fs::write(&file_path, content).expect("Failed to write test file");
    file_path
}

/// Path to an AOT-built binary, with the platform `.exe` suffix.
pub fn exe_path(dir: &Path, stem: &str) -> PathBuf {
    dir.join(format!("{stem}{}", std::env::consts::EXE_SUFFIX))
}

pub fn assert_ryo_runs(test_name: &str, code: &str) {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), test_name, code);
    let output =
        run_ryo_command(&["run", test_name], &test_file).expect("Failed to run ryo command");
    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A `never` value anywhere but a bare statement is a compile error
/// — `panic` diverges and produces no value to bind, return, pass,
/// or operate on. Assert on the user-facing message, not the exit
/// code alone.
pub fn assert_never_rejected(file_name: &str, code: &str) {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), file_name, code);

    let output =
        run_ryo_command(&["run", file_name], &test_file).expect("Failed to run ryo run command");

    assert!(
        !output.status.success(),
        "never-binding should be rejected. stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("'never' value"),
        "stderr should name the never-binding error, got: {}",
        stderr
    );
}

/// Run `ryo build` and return the path to the compiled binary.
///
/// The AOT pipeline writes the binary next to the source file. Tests
/// place (or copy) the source into a dedicated output directory so the
/// artifact lands somewhere predictable and is cleaned up with the
/// `TempDir`.
pub fn run_ryo_build(source_file: &Path, out_dir: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_ryo"))
        .arg("build")
        .arg(source_file)
        .current_dir(out_dir)
        .output()
        .expect("Failed to run ryo build command")
}

/// Runs `code` via the JIT and asserts an "integer overflow" panic
/// (stderr message + nonzero exit).
pub fn assert_int_overflow_panics(name: &str, code: &str) {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), name, code);

    let output =
        run_ryo_command(&["run", name], &test_file).expect("Failed to run ryo run command");

    assert_eq!(
        output.status.code(),
        Some(101),
        "integer overflow should exit 101. stdout: {}",
        String::from_utf8_lossy(&output.stdout),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("integer overflow"),
        "stderr should contain overflow message, got: {}",
        stderr
    );
}

/// Runs `code` via the JIT and asserts it completes successfully.
pub fn assert_program_succeeds(name: &str, code: &str) {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), name, code);

    let output =
        run_ryo_command(&["run", name], &test_file).expect("Failed to run ryo run command");

    assert!(
        output.status.success(),
        "{} should succeed. STDERR: {}",
        name,
        String::from_utf8_lossy(&output.stderr)
    );
}
