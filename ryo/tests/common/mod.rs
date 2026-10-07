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

/// Which compiler performs the fixture re-link.
///
/// `Zig` is the default (matches the AOT pipeline's managed toolchain).
/// `HostCc` is the host C compiler (`cc`): required for sanitizer
/// re-links, because zig cc accepts `-fsanitize=address` without
/// complaint but links no ASan runtime on any platform (verified on
/// macOS and linux-x86_64/aarch64 — the symbols assertion in
/// asan_smoke.rs exists because of this), while gcc/clang on a glibc
/// host ship a working ASan.
pub enum TestLinker {
    Zig,
    HostCc,
}

/// Compiles a Ryo program and re-links the object file.
///
/// Returns the temporary directory (which must be kept alive by the caller)
/// and the path to the compiled executable.
pub fn build_and_link(
    source: &str,
    name: &str,
    extra_link_args: &[&str],
) -> (tempfile::TempDir, PathBuf) {
    build_and_link_with(source, name, extra_link_args, TestLinker::Zig)
}

/// Same as [`build_and_link`] but re-links with the host C compiler —
/// see [`TestLinker::HostCc`]. Used by the sanitizer smoke suites.
pub fn build_and_link_host_cc(
    source: &str,
    name: &str,
    extra_link_args: &[&str],
) -> (tempfile::TempDir, PathBuf) {
    build_and_link_with(source, name, extra_link_args, TestLinker::HostCc)
}

fn build_and_link_with(
    source: &str,
    name: &str,
    extra_link_args: &[&str],
    linker: TestLinker,
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

    // Step 2: relink (object extension matches the AOT pipeline:
    // `.obj` on Windows, `.o` elsewhere — see pipeline.rs
    // get_output_filenames)
    let obj = tmp.path().join(format!(
        "{name}.{}",
        if cfg!(windows) { "obj" } else { "o" }
    ));
    let exe = tmp.path().join(format!("{name}_test_binary"));

    let runtime_lib = runtime_lib_path();
    assert!(
        runtime_lib.exists(),
        "runtime archive missing at {} — it is built by ryo-backend's build.rs; run `cargo build` first",
        runtime_lib.display()
    );

    let (compiler, compiler_name): (PathBuf, &str) = match linker {
        TestLinker::Zig => (zig_path(), "zig cc"),
        TestLinker::HostCc => (PathBuf::from("cc"), "host cc"),
    };
    let mut cmd = Command::new(&compiler);
    if matches!(linker, TestLinker::Zig) {
        cmd.arg("cc");
    }
    cmd.args(extra_link_args);
    cmd.arg("-o");
    cmd.arg(&exe);
    cmd.arg(&obj);
    cmd.arg(&runtime_lib);
    let out = cmd.output().expect("relink fixture");
    assert!(
        out.status.success(),
        "{compiler_name} failed with args {:?}:\nstdout: {}\nstderr: {}",
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
        // Conditional last use inside an arm that RETURNS: the in-arm
        // anchor covers the taken path (with the return epilogue), and
        // the branch-exit anchor must cover the not-taken path, which
        // falls through to the function end with the owner still live.
        // The heap-backed initializer (runtime concat, > SSO inline
        // capacity) gives ryo_str_free a real allocation to release.
        "last_use_in_returning_arm_fallthrough",
        "\
fn f():
\ts: str = int_to_str(42) + \"abcdefghijklmnopqrstuvwxyz0123456789\"
\td = false
\tif d:
\t\tprint(s)
\t\treturn

fn main():
\tf()
",
    ),
    (
        // Same family, mirrored arms: the last use sits in an arm that
        // FALLS THROUGH while a sibling arm returns. The implicit-else
        // path reaches the merge with the owner still live — only the
        // branch-exit anchor frees it there.
        "last_use_in_fallthrough_arm_sibling_returns",
        "\
fn f(x: int):
\ts: str = int_to_str(42) + \"abcdefghijklmnopqrstuvwxyz0123456789\"
\tif x == 1:
\t\tprint(s)
\telif x == 2:
\t\treturn

fn main():
\tf(3)
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
        // Field-base slice of an inline (SSO) field: promote-on-view
        // promotes the field in place (the struct's slot is the
        // owner-side storage), the view reads the promoted buffer,
        // and the struct drop frees it exactly once.
        "slice_of_struct_field_inline",
        "\
struct Person:
\tname: str

fn main():
\tp = Person{name=int_to_str(42)}
\tv = p.name[0:1]
\tprint(v)
\tprint(p.name)
",
    ),
    (
        // Field-base slice of a heap field: the view projects the
        // STRUCT's storage, so the struct outlives the view (P2
        // freeze) — a drop at the field read would dangle the view.
        "slice_of_struct_field_heap",
        "\
struct Person:
\tname: str

fn main():
\tp = Person{name=\"the quick brown fox\" + int_to_str(7)}
\tv = p.name[0:3]
\tprint(v)
",
    ),
    (
        // M8.4.2: bytes owner + view + materialize. bytes_push grows
        // the buffer in place (realloc); v is a non-owning view into
        // it; c is a fresh owning copy. The owner frees once at its
        // last use, the view frees nothing, and the copy frees at its
        // own last use — no leak, no double-free.
        "bytes_ops",
        "\
fn main():
\tmut b = b\"\\x01\\x02\"
\tbytes_push(&b, 3)
\tv = b[0:2]
\tc = bytes(v)
\tprint(int_to_str(c.len()))
\tprint(b)
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
    (
        // M9: a struct owning a heap `str` field — destruction must
        // recurse into the field and free the buffer exactly once.
        // `int_to_str` forces cap != 0 heap buffers leak detection can
        // observe; the reassign exercises field_free_on_reassign, the
        // `q = p` move the whole-struct ownership transfer.
        "struct_leak_check",
        "\
struct Person:
\tname: str

fn main():
\tmut p = Person{name=int_to_str(42)}
\tp.name = int_to_str(7)
\tq = p
\tprint(q.name)
",
    ),
    (
        // Self-assignment of a needs-drop binding is a liveness no-op:
        // no reassign Free, a single Free at the last use.
        "self_assign_str",
        "\
fn main():
\tmut s: str = int_to_str(42)
\ts = s
\tprint(s)
",
    ),
    (
        // Whole-struct self-assignment with a heap `str` field: the
        // same no-op rule, recursive field destruction fires once.
        "self_assign_struct",
        "\
struct Person:
\tname: str

fn main():
\tmut p = Person{name=int_to_str(42)}
\tp = p
\tprint(p.name)
",
    ),
    (
        // Slicing a borrowed str param whose argument is inline (SSO)
        // promotes a heap buffer that must be freed.
        "slice_borrowed_param_inline",
        "\
fn scan(s: str):
\tv = s[0:1]
\tprint(v)

fn main():
\tx: str = int_to_str(7)
\tscan(x)
\tscan(x)
",
    ),
    (
        // Heap argument (> 23 B): promotion is a no-op pass-through;
        // the scheduled free must not touch the caller's buffer.
        "slice_borrowed_param_heap",
        "\
fn scan(s: str):
\tv = s[0:2]
\tprint(v)

fn main():
\tx: str = int_to_str(123456789)
\ty: str = x + x + x + x
\tscan(y)
",
    ),
    (
        // The view's last use is inside the return operand, so the
        // free's anchor lands on a sub-inst of the Return — and the
        // end-of-statement sweep is skipped on terminators. Only the
        // return-epilogue promo free releases the promotion buffer on
        // this path. `print(int_to_str(...))` proves the slice executed
        // (a panic would mask the leak under Valgrind).
        "slice_borrowed_param_return_last_use",
        "\
fn scan(s: str) -> int:
\tv = s[0:2]
\treturn v.len()

fn main():
\tx: str = int_to_str(654321)
\tprint(int_to_str(scan(x)))
",
    ),
    (
        // Loop-deferred view (created before the loop, read inside it)
        // with a `return` inside the loop: the loop-exit anchor is
        // bypassed on the return path — only the return-epilogue promo
        // free releases the promotion buffer. The in-loop `print(v)`
        // proves the slice executed.
        "slice_borrowed_param_return_in_loop",
        "\
fn scan(s: str) -> int:
\tv = s[0:2]
\tfor i in range(0, 4):
\t\tprint(v)
\t\treturn 1
\treturn 0

fn main():
\tx: str = int_to_str(654321)
\tscan(x)
",
    ),
    (
        // View declared before the loop, rebound inside it, read only
        // after it: the in-loop slice gets no recorded last use (the
        // liveness pre-pass attributes the post-loop read to the
        // pre-loop slice), so its promo free falls to the
        // bound-never-read fallback. Anchoring that fallback at the
        // loop exit releases the final iteration's buffer right before
        // the post-loop read — Valgrind flags the read as a
        // use-after-free. The post-loop `print(v)` (prints `4`) proves
        // the slice path executed.
        "slice_borrowed_param_rebind_loop_read_after",
        "\
fn scan(s: str):
\tmut v = s[0:1]
\tfor i in range(0, 3):
\t\tv = s[i:i+1]
\tprint(v)

fn main():
\tx: str = int_to_str(654321)
\tscan(x)
",
    ),
    (
        // View declared before the loop and rebound inside it, with
        // the read BEFORE the rebind: the in-loop slice's promotion
        // buffer must survive until the loop exit — freeing it at the
        // rebind statement releases the buffer the just-rebound view
        // points into (the next iteration's read is a use-after-free).
        // The in-loop `print(v)` (prints `65655443`) proves the slice
        // path executed.
        "slice_borrowed_param_rebind_loop",
        "\
fn scan(s: str):
\tmut v = s[0:2]
\tfor i in range(0, 4):
\t\tprint(v)
\t\tv = s[i:i+2]

fn main():
\tx: str = int_to_str(654321)
\tscan(x)
",
    ),
    (
        // View created before an if whose only use is inside a
        // returning arm: the conditional-last-use re-anchor refuses a
        // branch whose arm returns, so the normal anchor stays in-arm
        // and the not-taken path falls through to the function's
        // synthesized return with the promotion buffer still live.
        // Only the fallthrough backstop anchor releases it on that
        // path. The final `print` (prints `done`) proves the
        // fallthrough path executed.
        "slice_borrowed_param_last_use_in_returning_arm",
        "\
fn scan(cond: bool, s: str):
\tv = s[0:1]
\tif cond:
\t\tprint(v)
\t\treturn

fn main():
\tx: str = int_to_str(654321)
\tscan(false, x)
\tprint(\"done\")
",
    ),
    (
        // Loop-carried str concat past the 23-byte inline boundary with
        // an immediate `break` (dead conditional after it, as in the
        // original repro): the loop-exit anchor must not free the
        // loop-carried owner a second time — pre-fix this double-freed
        // (glibc "double free", macOS SIGTRAP/SIGABRT) and the print
        // past the inline boundary emitted garbage.
        "loop_carried_concat_break",
        "\
fn main():
\tmut total = \"😀😀😀😀😀😀😀\"
\twhile true:
\t\ttotal = total + \"🦊\"
\t\tbreak
\t\tif total.len() > 5000:
\t\t\tbreak
\tprint(total)
\tprint(\"\\n\")
",
    ),
    (
        // Same family with a live conditional exit: the accumulator
        // grows past 5000 bytes over many iterations, so each
        // superseded buffer drops exactly once (in-loop reassign) and
        // the final buffer drops exactly once at the last use.
        "loop_carried_concat_in_loop",
        "\
fn main():
\tmut total = \"😀😀😀😀😀😀😀\"
\twhile true:
\t\ttotal = total + \"🦊\"
\t\tif total.len() > 5000:
\t\t\tbreak
\tprint(total)
\tprint(\"\\n\")
",
    ),
    (
        // int_to_str formatting while a >23-byte str is live: the
        // formatted buffer's length field must not pick up the live
        // string's state — pre-fix print emitted ~32 garbage bytes
        // after the digits and the process double-freed at exit.
        "int_to_str_with_long_live_str",
        "\
fn main():
\tmut total = \"😀😀😀😀😀😀😀\"
\twhile true:
\t\ttotal = total + \"🦊\"
\t\tbreak
\tprint(int_to_str(total.len()))
\tprint(total)
\tprint(\"\\n\")
",
    ),
    (
        // Two owned str locals inside a loop body with an inout call
        // between them and an early `return v` of one from the loop.
        // Pins: the return epilogue frees only owners live on the
        // return's path (pre-fix codegen aborted "no ValueRepr cached"
        // on the loop-local recorded by the backedge-seeded state), and
        // both buffers are freed exactly once on every path.
        "early_return_owned_value_from_loop",
        "\
fn adv(inout p: int):
\tp += 1

fn slice_at(s: str, at: int) -> str:
\treturn str(s[at:at + 2])

fn find(b: bytesview, s: str, key: str) -> str:
\tmut p = 0
\twhile true:
\t\tif p >= b.len():
\t\t\treturn \"<none>\"
\t\tk = slice_at(s, p)
\t\tadv(&p)
\t\tv = slice_at(s, p)
\t\tif k == key:
\t\t\treturn v
\t\tadv(&p)
\treturn \"<none>\"

fn main():
\tdoc = \"hello world\"
\tb = doc.as_bytes()
\tprint(find(b, doc, \"he\"))
\tprint(\"\\n\")
",
    ),
    (
        // M9.1: print() renders a struct via DebugRepr — the 28-byte repr
        // (Person{name="42", age=12345}) goes to the heap, and the
        // per-field render temps must free exactly once.
        "debug_repr_struct_print",
        "\
struct Person:
\tname: str
\tage: int

fn main():
\tp = Person{name=int_to_str(42), age=12345}
\tprint(p)
\tprint(\"\\n\")
",
    ),
    (
        // M9.1: nested struct print — the nested repr temp and the
        // per-field render temps all free; the borrowed struct fields
        // are untouched.
        "debug_repr_nested_struct",
        "\
struct Point:
\tx: float
\ty: float

struct Line:
\ta: Point
\tb: Point

fn main():
\tl = Line{a=Point{x=1.0, y=2.0}, b=Point{x=3.0, y=4.0}}
\tprint(l)
\tprint(\"\\n\")
",
    ),
    (
        // M9.1: primitive prints through DebugRepr — bare rendering, no
        // braces; each repr temp frees after its print.
        "debug_repr_primitives",
        "\
fn main():
\tprint(42)
\tprint(\"\\n\")
\tprint(-7)
\tprint(\"\\n\")
\tprint(3.14)
\tprint(\"\\n\")
\tprint(1.0)
\tprint(\"\\n\")
\tprint(true)
\tprint(\"\\n\")
",
    ),
    (
        // A loop-LOCAL mut str reassigned inside the body (concat
        // crossing the 23-byte inline boundary) and broken out of
        // while live. Loop-local bindings are NOT loop-carried: the
        // break-exit Free must still fire — pre-fix the two
        // definitions of "loop-carried" disagreed and this leaked
        // the final iteration's buffer on the break path.
        "loop_local_reassign_break_leak",
        "\
fn main():
\tmut i = 0
\twhile i < 3:
\t\tmut s = \"ab\"
\t\ts = s + \"🦊🦊🦊🦊🦊🦊\"
\t\ti += 1
\t\tif i == 2:
\t\t\tbreak
\tprint(\"ok\\n\")
",
    ),
    (
        // M9.1: memberwise `==` on a needs-drop struct — the compare
        // borrows both operands (nothing drops at the comparison), the
        // heap str fields drop exactly once at scope end, and the field
        // reassign drops the old buffer before the overwrite.
        "struct_eq_heap_str_fields",
        "\
#[derive(Eq)] struct User:
\tname: str
\tage: int

fn main():
\tp = User{name=int_to_str(42), age=30}
\tmut q = User{name=int_to_str(7), age=30}
\tprint(p == q)
\tprint(\"\\n\")
\tprint(p != q)
\tprint(\"\\n\")
\tq.name = int_to_str(42)
\tprint(p == q)
\tprint(\"\\n\")
\tprint(p.name)
\tprint(q.name)
\tprint(\"\\n\")
",
    ),
    (
        // M9.1: bytes fields compare by content (ryo_bytes_eq) — the
        // literal is static while `b"al" + b"ice"` is a fresh heap
        // buffer; both extract through the slot home.
        "struct_eq_bytes_field",
        "\
#[derive(Eq)] struct Blob:
	data: bytes
	tag: int

fn main():
	a = Blob{data=b\"alice\", tag=1}
	b = Blob{data=b\"al\" + b\"ice\", tag=1}
	c = Blob{data=b\"bob\", tag=1}
	print(a == b)
	print(\"\\n\")
	print(a == c)
	print(\"\\n\")
	print(a != c)
	print(\"\\n\")
",
    ),
    (
        // Same-name shadow, not-taken reassign path. The inner
        // `mut x` is a different binding: its reassigns must not
        // suppress the outer binding's cleanup. When they did (the
        // free-suppression grouped reassigns by name), the outer
        // buffer's only Free was dropped — the `if false` reassign
        // never fires to release it — and the outer heap buffer
        // leaked on every run. Heap strings via make(): programs
        // that only touch short literals never allocate (SSO cap 23)
        // and are leak-invisible under Valgrind.
        "shadow_binding_reassign_leak",
        "\
fn make(tag: int) -> str:
\treturn int_to_str(tag) + \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"

fn main():
\tmut x = make(1)
\tif false:
\t\tx = make(2)
\tprint(x)
\tif true:
\t\tmut x = make(3)
\t\tx = make(4)
\tprint(\"done\\n\")
",
    ),
    (
        // Same-name shadow + TAKEN reassign path: the mirror of
        // shadow_binding_reassign_leak. Codegen's binding-path
        // redirect resolves a Free through the binding's CURRENT home
        // slot; the lookup was keyed by NAME, so the shadow scope's
        // later writes clobbered the outer binding's "most recent
        // write" and the redirect was rejected for the outer owner's
        // last-use Free. The fallback freed the owner's stale cached
        // triple (an invalid free — the buffer was already released
        // by the taken arm's free_on_reassign) while the slot's
        // path-correct buffer (the taken reseat's value) leaked.
        // Both defects are heap-only (SSO short literals never
        // allocate), hence make().
        "shadow_taken_arm_double_free",
        "\
fn make(tag: int) -> str:
\treturn int_to_str(tag) + \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"

fn main():
\tmut x = make(1)
\tif true:
\t\tx = make(2)
\tprint(x)
\tif true:
\t\tmut x = make(3)
\t\tx = make(4)
\tprint(\"done\\n\")
",
    ),
    (
        // The taken-arm + shadow composition with RUNTIME conditions,
        // both directions in one run: the x shape exercises the taken
        // reseat + taken shadow scope, the y shape the not-taken
        // reseat + not-taken shadow scope. The not-taken shadow side
        // pins the binding-aware ConditionalDeadDrop honoring: a dead
        // shadow-scope reseat value must not mint a drop against the
        // outer binding's home slot (the outer owner's own Free
        // already released it — the drop double-freed the slot's
        // buffer on this exact path).
        "shadow_scope_runtime_cond",
        "\
fn make(tag: int) -> str:
\treturn int_to_str(tag) + \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"

fn main():
\tcond = \"ab\".len() > 0
\tmut x = make(1)
\tif cond:
\t\tx = make(2)
\tprint(x)
\tif cond:
\t\tmut x = make(3)
\t\tx = make(4)
\tmut y = make(5)
\tif not cond:
\t\ty = make(6)
\tprint(y)
\tif not cond:
\t\tmut y = make(7)
\t\ty = make(8)
\tmut z = make(9)
\tif cond:
\t\tz = make(10)
\t\tmut z = make(11)
\tmut w = make(12)
\tif not cond:
\t\tw = make(13)
\t\tmut w = make(14)
\tprint(\"done\\n\")
",
    ),
    (
        // Nested reseat-then-shadow: the arm reseats the OUTER binding
        // inside a nested conditional, then shadows the name. The
        // enclosing arm's reseat record must capture the nested reseat
        // (the arm scan descends into nested bodies) or the
        // pre-branch buffer leaks on every run where the enclosing
        // arm was skipped — 64 bytes under Valgrind for heap strings.
        // The x shape (enclosing arm skipped) pins the leak; the y
        // shape (enclosing taken, nested skipped) pins the nested if's
        // own fall-through drop.
        "nested_reseat_then_shadow_leak",
        "\
fn make(tag: int) -> str:
\treturn int_to_str(tag) + \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"

fn main():
\tcond = \"ab\".len() > 0
\tmut x = make(1)
\tif not cond:
\t\tif cond:
\t\t\tx = make(2)
\t\tmut x = make(3)
\tmut y = make(4)
\tif cond:
\t\tif not cond:
\t\t\ty = make(5)
\t\tmut y = make(6)
\tprint(\"done\\n\")
",
    ),
    (
        // Sibling scopes (no shadowing): each arm declares its own
        // `x`. An arm's reassigns must not enter the sibling arm's
        // binding — name-keyed suppression dropped a sibling buffer.
        "sibling_scope_binding_reassign",
        "\
fn make(tag: int) -> str:
\treturn int_to_str(tag) + \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"

fn main():
\tif true:
\t\tmut x = make(1)
\t\tx = make(2)
\telse:
\t\tmut x = make(3)
\t\tx = make(4)
\tprint(\"done\\n\")
",
    ),
    (
        // Taken conditional reseat + later branch whose read arm
        // is skipped + fall-through exit. The pre-branch owner's
        // re-anchored last-use Free used to target the displacement-
        // released owner; codegen's stale-target filter rejected the
        // redirect and the cached-value fallback double-freed (valgrind
        // Invalid free). The exit Free must target the binding's last
        // write so the redirect frees the slot's path-correct content.
        "reseat_fallthrough_skipped_read_arm",
        "\
fn make(tag: int) -> str:
\treturn int_to_str(tag) + \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"

fn f(c1: bool, c2: bool) -> int:
\tmut s = make(0)
\tif c1:
\t\ts = make(1)
\tif c2:
\t\tprint(s)
\t\treturn 0
\treturn 1

fn main():
\tf(true, true)
\tf(false, true)
\tf(true, false)
\tf(false, false)
\tprint(\"done\\n\")
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

/// Assert the program compiles, runs under JIT, and prints exactly
/// `expected` on stdout. In default mode stdout IS the program's own
/// output — the compiler prints nothing else (`print` appends no
/// newline, so multiple prints concatenate).
pub fn assert_ryo_output(test_name: &str, code: &str, expected: &str) {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), test_name, code);
    let output =
        run_ryo_command(&["run", test_name], &test_file).expect("Failed to run ryo command");
    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout, expected, "stdout mismatch");
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
