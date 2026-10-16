mod common;
use common::{create_test_file, run_ryo_command};

use std::process::Command;

fn run_ryo(source: &str, name: &str) -> String {
    let (_tmp, exe) = common::build_and_link(source, name, &[]);
    let out = Command::new(exe).output().expect("run");
    assert!(
        out.status.success(),
        "{name} exited {:?}: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("utf8 stdout")
}

#[test]
fn string_building_loop_is_correct() {
    // Consuming reassign-concat loop: the string_building benchmark shape.
    let src = "\
fn main():
\tmut s: str = \"\"
\tfor i in range(0, 1000):
\t\ts = s + \"x\"
\tassert(s.len() == 1000, \"len must be 1000\")
\tprint(\"ok\\n\")
";
    assert_eq!(run_ryo(src, "sso_string_building"), "ok\n");
}

#[test]
fn doubling_concat_stays_correct() {
    // Aliasing exclusion: s = s + s must keep the allocating path.
    let src = "\
fn main():
\tmut s: str = \"a\"
\tfor i in range(0, 5):
\t\ts = s + s
\tassert(s.len() == 32, \"len must be 32\")
\tprint(s)
\tprint(\"\\n\")
";
    assert_eq!(
        run_ryo(src, "sso_doubling"),
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n"
    );
}

#[test]
fn bytes_concat_and_push_across_representations() {
    // Consuming reassign-concat + bytes_push on an inline (SSO) bytes value:
    // both must read the inline data bytes, never a raw cap word.
    let src = "\
fn main():
\tmut b: bytes = b\"ab\"
\tb = b + b\"cd\"
\tbytes_push(&b, 101)
\tprint(b)
";
    assert_eq!(run_ryo(src, "sso_bytes_building"), "b\"abcde\"");
}

#[test]
fn bytes_slice_of_short_owner_is_stable() {
    // Slicing promotes the inline (SSO) base to heap before the view is
    // taken: the view reads b"bc" from stable memory, and the owner
    // still grows correctly afterwards. (Growing while the view is live
    // is a compile-time ownership error, so the view is consumed first.)
    let src = "\
fn main():
\tmut b: bytes = b\"abcdef\"
\tv = b[1:3]
\tprint(v)
\tprint(\"\\n\")
\tb = b + b\"ghijklmnopqr\"
\tprint(b)
";
    assert_eq!(
        run_ryo(src, "sso_bytes_slice"),
        "b\"bc\"\nb\"abcdefghijklmnopqr\""
    );
}

#[test]
fn mixed_static_inline_heap_concat() {
    // One expression mixing all three representations: "user" is a
    // static literal (cap==0), int_to_str(7) is inline (SSO), and the
    // 44-byte result of the second concat is heap-allocated.
    let src = "\
fn main():
\tname: str = \"user\" + int_to_str(7)
\tlong: str = name + \"-abcdefghijklmnopqrstuvwxyz0123456789\"
\tprint(name)
\tprint(\"\\n\")
\tprint(long)
\tprint(\"\\n\")
";
    assert_eq!(
        run_ryo(src, "sso_mixed_concat"),
        "user7\nuser7-abcdefghijklmnopqrstuvwxyz0123456789\n"
    );
}

#[test]
fn slice_of_inline_str_then_owner_grows() {
    // Slicing promotes the inline (SSO) base to heap before the view is
    // taken, so the view reads from stable memory. Growing the owner
    // while the view is live is a compile-time ownership error, so the
    // view is consumed (printed) before the consuming reassign-concat.
    let src = "\
fn main():
\tmut s: str = int_to_str(12345)
\tv = s[1:3]
\tprint(v)
\tprint(\"\\n\")
\ts = s + \"678901234567890123456789\"
\tprint(s)
\tprint(\"\\n\")
";
    assert_eq!(
        run_ryo(src, "sso_slice_stable"),
        "23\n12345678901234567890123456789\n"
    );
}

#[test]
fn nested_inline_concat_and_eq_read_correct_bytes() {
    // Inline (SSO) operands extracted inside a nested expression: the
    // outer operand's scratch spill must survive evaluating the nested
    // concat. `a + (b + c)` must read a's bytes, and `x == (y + z)`
    // must compare x's bytes — not whatever the nested extraction
    // spilled last.
    let src = "\
fn main():
\ta = int_to_str(1)
\tb = int_to_str(2)
\tc = int_to_str(3)
\ts = a + (b + c)
\tprint(s)
\tprint(\"\\n\")
\tx = int_to_str(12)
\ty = int_to_str(1)
\tz = int_to_str(2)
\tif x == (y + z):
\t\tprint(\"equal\\n\")
\telse:
\t\tprint(\"not equal\\n\")
";
    assert_eq!(run_ryo(src, "sso_nested_scratch"), "123\nequal\n");
}

#[test]
fn struct_with_short_str_fields() {
    // Inline (SSO) strings embedded in an aggregate: constructed from a
    // static+inline concat, moved through a function, field-reassigned
    // with an inline concat, and dropped. The struct drop glue's
    // (ptr@off, cap@off+16) free path must no-op on inline tags, and
    // the field-reassign free-on-reassign path must not free the old
    // inline value.
    let src = "\
struct Person:
\tname: str
\tage: int

fn birthday(move p: Person) -> Person:
\tmut r = p
\tr.age += 1
\treturn r

fn main():
\tp = Person{name=\"user\" + int_to_str(42), age=30}
\tmut q = birthday(p)
\tq.name = q.name + \"!\"
\tprint(q.name)
\tprint(\" \")
\tprint(int_to_str(q.age))
\tprint(\"\\n\")
";
    assert_eq!(run_ryo(src, "sso_struct_fields"), "user42! 31\n");
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
fn int_to_str_garbage_output() {
    // Pre-fix: print(int_to_str(...)) emitted ~32 garbage bytes after
    // the digits while a >23-byte str was live, then double-freed at
    // exit. Exact bytes: "32" + 7x😀 (28 B) + 🦊 (4 B) + "\n" = 35.
    let temp_dir = tempfile::TempDir::new().expect("create source directory");
    let test_file = create_test_file(
        temp_dir.path(),
        "bug_int_to_str_garbage_output.ryo",
        INT_TO_STR_GARBAGE,
    );
    let output = run_ryo_command(&["run", "bug_int_to_str_garbage_output.ryo"], &test_file)
        .expect("run ryo");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.starts_with("32"),
        "stdout must start with the digits '32', got: {stdout:?}"
    );
    assert_eq!(output.stdout.len(), 35, "stdout length mismatch");
    assert_eq!(stdout, "32😀😀😀😀😀😀😀🦊\n");
}
