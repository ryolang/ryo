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

/// JIT-run an inlined repro source from a private temp dir.
fn run_bug_report(name: &str, src: &str) -> Output {
    // TempDir keeps the path exclusive across concurrent test processes
    // and is removed on drop (the DirTemp outlives the command below).
    let dir = tempfile::TempDir::new().expect("create repro temp dir");
    let path = dir.path().join(format!("{name}.ryo"));
    let mut f = std::fs::File::create(&path).expect("write repro temp file");
    f.write_all(src.as_bytes()).expect("write repro temp file");
    run_ryo_command(&["run", "name"], &path).expect("run ryo")
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

const NATURAL_LOOP_STR_ACCUMULATOR: &str = "\
struct Parsed:
\ttext: str
\tnext: int

fn skip_ws(b: bytesview, at: int) -> int:
\tmut p = at
\twhile p < b.len():
\t\tc = b[p]
\t\tif c == 32 or c == 9 or c == 10 or c == 13:
\t\t\tp += 1
\t\telse:
\t\t\treturn p
\treturn p

fn take_string(b: bytesview, s: str, at: int) -> Parsed:
\tmut i = at + 1
\tmut end = -1
\twhile i < b.len() and end < 0:
\t\tc = b[i]
\t\tif c == 92:
\t\t\ti += 2
\t\telif c == 34:
\t\t\tend = i + 1
\t\telse:
\t\t\ti += 1
\tif end < 0:
\t\treturn Parsed{text=\"<error>\", next=b.len()}
\treturn Parsed{text=str(s[at:end]), next=end}

fn take_number(b: bytesview, s: str, at: int) -> Parsed:
\tmut i = at
\tmut go = true
\twhile go and i < b.len():
\t\tc = b[i]
\t\tif (c >= 48 and c <= 57) or c == 45 or c == 43 or c == 46 or c == 101 or c == 69:
\t\t\ti += 1
\t\telse:
\t\t\tgo = false
\treturn Parsed{text=str(s[at:i]), next=i}

fn parse_array(b: bytesview, s: str, at: int) -> Parsed:
\tmut p = skip_ws(b, at + 1)
\tif p < b.len() and b[p] == 93:
\t\treturn Parsed{text=\"[]\", next=p + 1}
\tmut out = \"[\"
\tmut child = parse_value(b, s, p)
\tstr_push(&out, child.text)
\tp = skip_ws(b, child.next)
\twhile p < b.len() and b[p] == 44:
\t\tstr_push(&out, \",\")
\t\tchild = parse_value(b, s, p + 1)
\t\tstr_push(&out, child.text)
\t\tp = skip_ws(b, child.next)
\tif p < b.len() and b[p] == 93:
\t\tp += 1
\t\tstr_push(&out, \"]\")
\treturn Parsed{text=out, next=p}

fn parse_object(b: bytesview, s: str, at: int) -> Parsed:
\tmut p = skip_ws(b, at + 1)
\tif p < b.len() and b[p] == 125:
\t\treturn Parsed{text=\"{}\", next=p + 1}
\tmut out = \"{\"
\tmut first = true
\tmut go = true
\twhile go:
\t\tif not first:
\t\t\tstr_push(&out, \",\")
\t\tfirst = false
\t\tp = skip_ws(b, p)
\t\tk = take_string(b, s, p)
\t\tstr_push(&out, k.text)
\t\tstr_push(&out, \":\")
\t\tp = skip_ws(b, k.next)
\t\tif p >= b.len() or b[p] != 58:
\t\t\tgo = false
\t\telse:
\t\t\tchild = parse_value(b, s, p + 1)
\t\t\tstr_push(&out, child.text)
\t\t\tp = skip_ws(b, child.next)
\t\t\tif p >= b.len():
\t\t\t\tgo = false
\t\t\telse:
\t\t\t\tc = b[p]
\t\t\t\tif c == 44:
\t\t\t\t\tp += 1
\t\t\t\telif c == 125:
\t\t\t\t\tp += 1
\t\t\t\t\tstr_push(&out, \"}\")
\t\t\t\t\tgo = false
\t\t\t\telse:
\t\t\t\t\tgo = false
\treturn Parsed{text=out, next=p}

fn parse_value(b: bytesview, s: str, at: int) -> Parsed:
\tp = skip_ws(b, at)
\tif p >= b.len():
\t\treturn Parsed{text=\"<error>\", next=p}
\tc = b[p]
\tif c == 34:
\t\treturn take_string(b, s, p)
\tif c == 123:
\t\treturn parse_object(b, s, p)
\tif c == 91:
\t\treturn parse_array(b, s, p)
\tif c == 116:
\t\treturn Parsed{text=\"true\", next=p + 4}
\tif c == 102:
\t\treturn Parsed{text=\"false\", next=p + 5}
\tif c == 110:
\t\treturn Parsed{text=\"null\", next=p + 4}
\tif c == 45 or (c >= 48 and c <= 57):
\t\treturn take_number(b, s, p)
\treturn Parsed{text=\"<error>\", next=p}

fn render(doc: str) -> int:
\tb = doc.as_bytes()
\tr = parse_value(b, doc, 0)
\treturn r.text.len()

fn main():
\t# Zero comma-iterations: the pre-fix build double-freed the
\t# pre-loop child struct through codegen's binding-path redirect
\t# (last-use Free anchored before the in-loop reassign).
\tone = render(\"[{\\\"id\\\":0,\\\"name\\\":\\\"useruseruser\\\"}]\")
\t# Two comma-iterations: exercises the reassign-free displacement
\t# chain across loop back-edges.
\tmut two = render(\"[{\\\"a\\\":1},{\\\"b\\\":2}]\")
\ttwo += render(\"[{\\\"a\\\":1},{\\\"b\\\":2}]\")
\tprint(int_to_str(one))
\tprint(\" \")
\tprint(int_to_str(two))
\tprint(\"\\n\")
";

#[test]
fn bug_natural_loop_str_accumulator() {
    // Pre-fix: SIGABRT (exit 134) from a bad ryo_str_free — the
    // loop-carried `child` binding's pre-reassign owner was freed by
    // BOTH a last-use Free (anchored before the in-loop reassign) and
    // the reassign-free, and the return epilogue double-freed the
    // binding's final value through the same home slot.
    let output = run_bug_report("natural_loop_str_accumulator", NATURAL_LOOP_STR_ACCUMULATOR);
    assert_success(&output, "bug_natural_loop_str_accumulator");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout, "32 34\n");
}

const EARLY_RETURN_AFTER_LOOP_REASSIGN: &str = "\
fn f(n: int) -> str:
\tmut s = \"aaaaaaaaaaaaaaaa\"
\tmut i = 0
\twhile i < n:
\t\ts = \"bbbbbbbbbbbbbbbb\"
\t\ti += 1
\tif n > 1:
\t\treturn s
\treturn s + \"!\"

fn main():
\tprint(f(3))
\tprint(\"\\n\")
";

#[test]
fn bug_early_return_after_loop_reassign() {
    // Pre-fix (binding-covering without the terminator-anchor
    // exclusion): the inner-return-anchored Free counted as covering
    // the outer return's epilogue Free, codegen's leak-direction
    // assert tripped ("frees anchored to unmaterialized instructions
    // were dropped"), and the debug compiler aborted on this natural
    // guard shape.
    let output = run_bug_report(
        "early_return_after_loop_reassign",
        EARLY_RETURN_AFTER_LOOP_REASSIGN,
    );
    assert_success(&output, "bug_early_return_after_loop_reassign");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout, "bbbbbbbbbbbbbbbb\n");
}

const STR_PUSH_STRUCT_FIELD_NOOP: &str = "\
struct Rec:
\ttext: str
\tbuf: bytes
\tn: int

fn bump(inout s: str):
\tstr_push(&s, \"!\")

fn main():
\tmut a = Rec{text=\"x\", buf=\"\".to_bytes(), n=0}
\tbump(&a.text)
\tprint(a.text)
\tprint(\"\\n\")

\tmut r = Rec{text=\"y\", buf=\"\".to_bytes(), n=0}
\tstr_push(&r.text, \"?\")
\tprint(r.text)
\tprint(\"\\n\")

\tbytes_push(&r.buf, 65)
\tbytes_push(&r.buf, 66)
\tprint(int_to_str(r.buf.len()))
\tprint(\"\\n\")
";

#[test]
fn bug_str_push_struct_field_noop() {
    // Pre-fix: the str_push/bytes_push builtin intercepts assumed arg 0
    // lowered to Var(name), so a FieldAccess place was evaluated into a
    // snapshot triple spilled to a temp slot; the runtime mutated the
    // temp, and the reload was gated on local_name_of(field), which is
    // None — the write-back was silently dropped (compiles, exit 0,
    // prints "x!\ny\n0"). Post-fix the field's address in the root
    // struct's slot is passed directly, like the generic inout path.
    let output = run_bug_report("str_push_struct_field_noop", STR_PUSH_STRUCT_FIELD_NOOP);
    assert_success(&output, "bug_str_push_struct_field_noop");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout, "x!\ny?\n2\n");
}
