//! M10 anonymous struct literals — JIT end-to-end tests.
//!
//! An anonymous literal `{x=1, y=2}` infers a structural type from its
//! field initializers (ordered (name, type) pairs, deduped in the
//! pool), reads fields by name like a named struct, and prints through
//! the same Debug repr with the name omitted: `{x=1, y=a}`.

mod common;
use common::*;

use tempfile::TempDir;

#[test]
fn anon_literal_and_field_jit() {
    assert_ryo_output(
        "anon_literal_and_field",
        "fn main():\n\tp = {x=1, y=2}\n\tprint(p.x)\n\tprint(\"\\n\")\n\tprint(p.y)\n\tprint(\"\\n\")\n",
        "1\n2\n",
    );
}

#[test]
fn anon_debug_print_jit() {
    // Nameless Debug repr: `{f=v, ...}` with no `Name` prefix. The str
    // field renders bare (no quotes) at the top level, exactly like a
    // named struct's str field renders quoted only inside the braces.
    assert_ryo_output(
        "anon_debug_print",
        "fn main():\n\tprint({x=1, y=\"a\"})\n\tprint(\"\\n\")\n",
        "{x=1, y=a}\n",
    );
}

#[test]
fn anon_duplicate_field_rejected() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tp = {x=1, x=2}\n";
    let test_file = create_test_file(temp_dir.path(), "anon_dup.ryo", code);

    let output =
        run_ryo_command(&["run", "anon_dup.ryo"], &test_file).expect("Failed to run ryo command");

    assert!(
        !output.status.success(),
        "a duplicate anon field must be rejected"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("E0040"),
        "should emit E0040 for the duplicate field, got: {}",
        stderr
    );
    assert!(
        stderr.contains("field 'x' is specified more than once"),
        "error should name the duplicated field, got: {}",
        stderr
    );
}

#[test]
fn anon_view_field_rejected() {
    // Rule 6 holds for anonymous literals too: a field initializer that
    // is a view (a projection, not an owned value) is rejected with the
    // same ViewFieldType diagnostic as a named struct declaration.
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\ts: str = \"hi\"\n\tp = {x=s[0:1]}\n";
    let test_file = create_test_file(temp_dir.path(), "anon_view.ryo", code);

    let output =
        run_ryo_command(&["run", "anon_view.ryo"], &test_file).expect("Failed to run ryo command");

    assert!(
        !output.status.success(),
        "a view-typed anon field must be rejected"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("E0042"),
        "should emit E0042 (ViewFieldType), got: {}",
        stderr
    );
    assert!(
        stderr.contains("struct fields must be owned values"),
        "error should carry the view-field message, got: {}",
        stderr
    );
}

#[test]
fn anon_empty_braces_report_only_e0109() {
    // `{}` stays reserved for the future empty map literal: the parser
    // diagnoses E0109 and recovers the literal as empty, and sema must
    // not pile a follow-up "unknown struct: ''" placeholder error on
    // top. One diagnostic total, and it is E0109.
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tp = {}\n";
    let test_file = create_test_file(temp_dir.path(), "anon_empty.ryo", code);

    let output =
        run_ryo_command(&["run", "anon_empty.ryo"], &test_file).expect("Failed to run ryo command");

    assert!(!output.status.success(), "empty braces must be rejected");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stderr.matches("[E0109]").count(),
        1,
        "expected exactly the E0109 diagnostic, got: {}",
        stderr
    );
    assert!(
        !stderr.contains("unknown struct"),
        "no follow-up placeholder diagnostic, got: {}",
        stderr
    );
}
