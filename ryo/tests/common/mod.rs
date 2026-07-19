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
        // I-112: the callee reassigns the inout str param; the replacement
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
        // I-112: user-fn inout str + reborrow through str_push.
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
        // I-112: growth forces a realloc move; the caller must free the
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
        // I-112 / pre-existing M8.1 bug: reassignment inside a branch,
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
        // I-117: dead conditional reassign, taken path — the old buffer
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
        // I-117: dead conditional reassign, NOT-taken path — the
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
        // I-118: dead reassign in a loop body, taken path — every
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
        // I-118: dead reassign in a loop body, ZERO iterations — the
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
        // I-118: same shape through a for-range loop with an empty
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
];

pub fn find_fixture(name: &str) -> &'static str {
    RYO_FIXTURES
        .iter()
        .find(|&&(n, _)| n == name)
        .map(|&(_, s)| s)
        .unwrap_or_else(|| panic!("fixture {name} not found"))
}
