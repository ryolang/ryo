//! M10 anonymous struct literals — JIT end-to-end tests.
//!
//! An anonymous literal `{x=1, y=2}` infers a structural type from its
//! field initializers (ordered (name, type) pairs, deduped in the
//! pool), reads fields by name like a named struct, and prints through
//! the same Debug repr with the name omitted: `{x=1, y="a"}`.

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
    // Nameless Debug repr: `{f=v, ...}` with no `Name` prefix. str
    // fields quote, exactly like a named struct's — the M10 ruling
    // makes the anon repr match named structs and Python container
    // repr.
    assert_ryo_output(
        "anon_debug_print",
        "fn main():\n\tprint({x=1, y=\"a\"})\n\tprint(\"\\n\")\n",
        "{x=1, y=\"a\"}\n",
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

// ---- M10: anonymous struct type literals (`{q: int, r: int}`) ----

#[test]
fn divmod_named_shape_jit() {
    // A compound type expression in the return type: the anonymous
    // literal's inferred shape must structurally match the annotation,
    // and field access reads through the signature type.
    assert_ryo_output(
        "divmod_named_shape",
        "fn divmod(a: int, b: int) -> {q: int, r: int}:\n\treturn {q=a / b, r=a % b}\n\nfn main():\n\tdm = divmod(10, 3)\n\tprint(dm.q)\n",
        "3",
    );
}

#[test]
fn type_literal_view_field_rejected() {
    // Rule 6 holds inside type literals: a view-typed field is
    // rejected with the same ViewFieldType diagnostic as everywhere
    // else.
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn f(p: {v: strview}) -> int:\n\treturn 1\n\nfn main():\n\tprint(1)\n";
    let test_file = create_test_file(temp_dir.path(), "type_lit_view.ryo", code);

    let output = run_ryo_command(&["run", "type_lit_view.ryo"], &test_file)
        .expect("Failed to run ryo command");

    assert!(
        !output.status.success(),
        "a view-typed field in a type literal must be rejected"
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
fn shape_typo_field_diff_message() {
    // §9 bar: expected + found structural displays, plus a field-level
    // diff note naming `rr` vs `r` with a did-you-mean.
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn divmod(a: int, b: int) -> {q: int, rr: int}:\n\treturn {q=a / b, r=a % b}\n\nfn main():\n\tdm = divmod(10, 3)\n\tprint(dm.q)\n";
    let test_file = create_test_file(temp_dir.path(), "shape_typo.ryo", code);

    let output =
        run_ryo_command(&["run", "shape_typo.ryo"], &test_file).expect("Failed to run ryo command");

    assert!(!output.status.success(), "a shape typo must be rejected");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("{q: int, rr: int}"),
        "error should show the expected shape, got: {}",
        stderr
    );
    assert!(
        stderr.contains("{q: int, r: int}"),
        "error should show the found shape, got: {}",
        stderr
    );
    assert!(
        stderr.contains("did you mean"),
        "a name typo gets a did-you-mean, got: {}",
        stderr
    );
}

#[test]
fn graduation_fixit_message() {
    // §9 bar: an anonymous shape passed where a named struct is
    // expected gets the graduation fix-it naming the struct and its
    // real field names.
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "struct Point:\n\tx: int\n\ty: int\n\nfn take(p: Point):\n\tprint(p.x)\n\nfn main():\n\ttake({x=1, y=2})\n";
    let test_file = create_test_file(temp_dir.path(), "graduation.ryo", code);

    let output =
        run_ryo_command(&["run", "graduation.ryo"], &test_file).expect("Failed to run ryo command");

    assert!(
        !output.status.success(),
        "anon-into-named must be rejected (no implicit coercion)"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("construct explicitly: `Point{x=…, y=…}`"),
        "error should teach explicit construction, got: {}",
        stderr
    );
}

// ---- M10: structural equality (`==` / `!=`) ----

#[test]
fn anon_eq_nested_shape_jit() {
    // A shape nested in a shape compares recursively: the memberwise
    // path descends into the inner shape's fields (the same
    // recursion named structs get from their derived Eq).
    assert_ryo_output(
        "anon_eq_nested",
        "fn main():\n\tp = {a=1, n={v=2}}\n\tq = {a=1, n={v=2}}\n\tr = {a=1, n={v=3}}\n\tprint(p == q)\n\tprint(\"\\n\")\n\tprint(p == r)\n\tprint(\"\\n\")\n\tprint(p != r)\n\tprint(\"\\n\")\n",
        "true\nfalse\ntrue\n",
    );
}

#[test]
fn anon_eq_nan_field_never_equals_jit() {
    // IEEE: NaN != NaN, so fcmp eq on a NaN field is false even when
    // both operands are the very same value — the same documented NaN
    // behavior as named structs (M9.1). NaN is built arithmetically:
    // float division does not trap (0.0 / 0.0).
    assert_ryo_output(
        "anon_eq_nan",
        "fn main():\n\tp = {x=0.0 / 0.0, y=1.0}\n\tprint(p == p)\n\tprint(\"\\n\")\n\tprint(p != p)\n\tprint(\"\\n\")\n",
        "false\ntrue\n",
    );
}

#[test]
fn anon_ordering_rejected() {
    // Shapes have no ordering: `p < q` hits the same
    // UnsupportedOperator arm as named structs.
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tp = {x=1}\n\tq = {x=2}\n\tprint(p < q)\n";
    let test_file = create_test_file(temp_dir.path(), "anon_ordering.ryo", code);

    let output = run_ryo_command(&["run", "anon_ordering.ryo"], &test_file)
        .expect("Failed to run ryo command");

    assert!(
        !output.status.success(),
        "ordering on a shape must be rejected"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("E0015"),
        "should emit E0015 (UnsupportedOperator), got: {}",
        stderr
    );
    assert!(
        stderr.contains("ordering operator '<' not supported"),
        "error should carry the unsupported-operator message, got: {}",
        stderr
    );
}

#[test]
fn anon_field_types_in_named_struct_jit() {
    // The last nesting direction: anonymous type literals as named
    // struct field types. Exercises construction, positional access
    // through the field, Debug rendering of tuple fields, and the
    // free path for a heap-owning field inside the anonymous field
    // type (Label's str, freed with the struct).
    assert_ryo_output(
        "anon_field_types",
        "struct Line:\n\tstart: (int, int)\n\tend: (int, int)\n\nstruct Label:\n\ttext: (str, int)\n\nfn main():\n\tl = Line{start=(0, 0), end=(3, 4)}\n\tprint(l.start.0 + l.end.1)\n\tprint(\"\\n\")\n\tprint(l)\n\tprint(\"\\n\")\n\tlab = Label{text=(\"age\", 42)}\n\tprint(lab.text.0)\n\tprint(lab.text.1)\n\tprint(\"\\n\")\n\tlab2 = Label{text=(\"bob\", 7)}\n\tprint(lab2.text.0)\n\tprint(\"\\n\")\n",
        "4\nLine{start=(0, 0), end=(3, 4)}\nage42\nbob\n",
    );
}
