//! End-to-end regression tests for the bug_reports/*.ryo repro programs
//! (loop-exit double-free family, return-epilogue codegen abort, E0020
//! exiting-arm false positive). The repro sources are inlined because
//! bug_reports/ is gitignored local scratch; each test JIT-runs the
//! inlined source and pins its exact stdout bytes — the pre-fix builds
//! crashed, emitted garbage, or failed to compile.

mod common;
use common::*;

use std::io::Write;
use std::process::Output;

/// JIT-run an inlined repro source from a temp file.
fn run_bug_report(name: &str, src: &str) -> Output {
    let dir = std::env::temp_dir();
    // PID-guarded: concurrent cargo test processes must not share the
    // path (the file is deleted after the run).
    let path = dir.join(format!("ryo_test_{}_{name}.ryo", std::process::id()));
    let mut f = std::fs::File::create(&path).expect("write repro temp file");
    f.write_all(src.as_bytes()).expect("write repro temp file");
    let out = run_ryo_command(&["run", "name"], &path).expect("run ryo");
    std::fs::remove_file(&path).ok();
    out
}

fn assert_success(output: &Output, name: &str) {
    assert!(
        output.status.success(),
        "{name}: expected exit 0. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

const INT_TO_STR_GARBAGE: &str = "\
fn main():
\tmut total = \"😀😀😀😀😀😀😀\"
\twhile true:
\t\ttotal = total + \"🦊\"
\t\tbreak
\tprint(int_to_str(total.len()))
\tprint(total)
\tprint(\"\\n\")
";

#[test]
fn bug_int_to_str_garbage_output() {
    // Pre-fix: print(int_to_str(...)) emitted ~32 garbage bytes after
    // the digits while a >23-byte str was live, then double-freed at
    // exit. Exact bytes: "32" + 7x😀 (28 B) + 🦊 (4 B) + "\n" = 35.
    let output = run_bug_report("int_to_str_garbage_output", INT_TO_STR_GARBAGE);
    assert_success(&output, "bug_int_to_str_garbage_output");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.starts_with("32"),
        "stdout must start with the digits '32', got: {stdout:?}"
    );
    assert_eq!(output.stdout.len(), 35, "stdout length mismatch");
    assert_eq!(stdout, "32😀😀😀😀😀😀😀🦊\n");
}

const CONCAT_AFTER_BREAK: &str = "\
fn main():
\tmut total = \"😀😀😀😀😀😀😀\"
\twhile true:
\t\ttotal = total + \"🦊\"
\t\tbreak
\t\tif total.len() > 5000:
\t\t\tbreak
\tprint(total)
\tprint(\"\\n\")
";

#[test]
fn bug_double_free_str_concat_after_break() {
    // Pre-fix: SIGTRAP/SIGABRT (double free) with no output. The
    // explicit print("\n") is the only newline — print appends none.
    let output = run_bug_report("concat_after_break", CONCAT_AFTER_BREAK);
    assert_success(&output, "bug_double_free_str_concat_after_break");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.stdout.len(), 33, "stdout length mismatch");
    assert_eq!(stdout, "😀😀😀😀😀😀😀🦊\n");
}

const CONCAT_IN_LOOP: &str = "\
fn main():
\tmut total = \"😀😀😀😀😀😀😀\"
\twhile true:
\t\ttotal = total + \"🦊\"
\t\tif total.len() > 5000:
\t\t\tbreak
\tprint(total)
\tprint(\"\\n\")
";

#[test]
fn bug_double_free_str_concat_in_loop() {
    // Pre-fix: heap corruption / double free while growing the
    // accumulator in the loop. Post-fix the loop breaks once
    // total.len() > 5000: 7x😀 (28 B) + 1244x🦊 (4976 B) + "\n" =
    // 5005 bytes exactly.
    let output = run_bug_report("concat_in_loop", CONCAT_IN_LOOP);
    assert_success(&output, "bug_double_free_str_concat_in_loop");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.stdout.len(), 5005, "stdout length mismatch");
    assert!(stdout.starts_with("😀😀😀😀😀😀😀"), "7x😀 prefix");
    assert!(stdout.ends_with("🦊\n"), "🦊 + newline suffix");
    assert_eq!(stdout, format!("{}\n", "😀".repeat(7) + &"🦊".repeat(1244)));
}

const CODEGEN_FREE_NO_VALUEREPR: &str = "\
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
";

#[test]
fn bug_codegen_free_no_valuerepr() {
    // Pre-fix: codegen aborted with "ownership pass scheduled Free ...
    // but no ValueRepr cached". The lookup key is "he" (not the
    // original "el") so the match prints "el" — the original key never
    // matched because p steps by 2 over even offsets only, and the
    // program then panics out-of-bounds; flipping it exercises the
    // previously-crashing `return v` path. That is intentional, keep it.
    let output = run_bug_report("codegen_free_no_valuerepr", CODEGEN_FREE_NO_VALUEREPR);
    assert_success(&output, "bug_codegen_free_no_valuerepr");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout, "el\n");
}

const RETURN_ACCUMULATOR_IN_LOOP: &str = "\
fn build(n: int) -> str:
\tmut out = \"[\"
\tmut i = 0
\twhile i < n:
\t\tstr_push(&out, \"x\")
\t\tif i == 2:
\t\t\treturn out
\t\ti += 1
\treturn out

fn main():
\tprint(build(5))
\tprint(\"\\n\")
";

#[test]
fn bug_e0020_return_accumulator_in_loop() {
    // Pre-fix: three bogus E0020 "use of moved value" errors — the
    // flow-insensitive move analysis treated the in-loop `return out`
    // as moving the accumulator on every iteration.
    let output = run_bug_report("return_accumulator_in_loop", RETURN_ACCUMULATOR_IN_LOOP);
    assert_success(&output, "bug_e0020_return_accumulator_in_loop");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout, "[xxx\n");
}
