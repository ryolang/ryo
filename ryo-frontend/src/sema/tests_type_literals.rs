//! M10 anonymous struct type literals (`{q: int, r: int}`) — sema
//! tests: structural identity at the three annotation positions
//! (return type, param, var annotation), view-field rejection through
//! the M9 `ViewFieldType` path, and the §9-bar TypeMismatch
//! enrichment: a field-level diff note when both sides are
//! struct-kinded (did-you-mean for name typos, the field named for
//! type diffs) plus the graduation fix-it when a named struct is
//! expected and an anonymous shape is found.

use super::tests::*;
use super::*;
use ryo_core::types::TypeKind;

fn mismatch_diags(diags: &[Diag]) -> Vec<&Diag> {
    diags
        .iter()
        .filter(|d| d.code == DiagCode::TypeMismatch)
        .collect()
}

fn note_text(d: &Diag) -> String {
    d.notes
        .iter()
        .map(|n| n.message.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn type_literal_return_type_checks() {
    let src = "fn divmod(a: int, b: int) -> {q: int, r: int}:\n\treturn {q=a / b, r=a % b}\n";
    let (tirs, pool) = run(src).expect("sema ok");
    let divmod = tir_named(&tirs, &pool, "divmod");
    assert!(
        matches!(pool.kind(divmod.return_type), TypeKind::AnonStruct),
        "return annotation must resolve to an anonymous struct"
    );
}

#[test]
fn type_literal_param_type_checks() {
    let src = "fn sum(p: {x: int, y: int}) -> int:\n\treturn p.x + p.y\n\nfn main():\n\tprint(sum({x=1, y=2}))\n";
    assert!(run(src).is_ok());
}

#[test]
fn type_literal_var_annotation_checks() {
    let src = "fn main():\n\tdm: {q: int, r: int} = {q=1, r=2}\n\tprint(dm.q)\n";
    assert!(run(src).is_ok());
}

#[test]
fn type_literal_duplicate_fields_rejected() {
    // A repeated field name has no structural meaning — diagnosed
    // exactly like the value literal's duplicate, and the annotation
    // absorbs to the error sentinel.
    let src = "fn f() -> {q: int, q: int}:\n\treturn 1\n";
    let (_t, diags, _pool) = run_with_errors(src);
    assert!(
        any_code(&diags, DiagCode::DuplicateStructField),
        "got {diags:?}"
    );
    let d = diags
        .iter()
        .find(|d| d.code == DiagCode::DuplicateStructField)
        .unwrap();
    assert_eq!(d.message, "field 'q' is specified more than once");
}

#[test]
fn type_literal_numeric_keys_match_tuple_sugar() {
    // The brace spelling with numeric keys IS the paren sugar's
    // structural type, so a tuple value satisfies the annotation.
    let src = "fn f() -> {0: int, 1: str}:\n\treturn (1, \"a\")\n\nfn main():\n\tprint(f())\n";
    assert!(run(src).is_ok());
}

#[test]
fn type_literal_view_field_rejected() {
    // Rule 6 holds inside type literals too: a view-typed field is a
    // projection and is rejected with the same ViewFieldType diagnostic
    // as a named struct declaration / anonymous value literal.
    let src = "fn f(p: {v: strview}) -> int:\n\treturn 1\n";
    let (_t, diags, pool) = run_with_errors(src);
    assert!(any_code(&diags, DiagCode::ViewFieldType), "got {diags:?}");
    let d = diags
        .iter()
        .find(|d| d.code == DiagCode::ViewFieldType)
        .unwrap();
    assert_eq!(
        d.message,
        format!(
            "struct fields must be owned values; '{}' is a projection (Rule 6)",
            pool.display(pool.str_view()),
        )
    );
}

#[test]
fn shape_typo_emits_field_diff_note_with_did_you_mean() {
    // §9 bar (structural shape typo): the error shows both structural
    // shapes and adds a field-level diff note naming the differing
    // field, with a did-you-mean for the name typo (`rr` vs `r`).
    let src = "fn divmod(a: int, b: int) -> {q: int, rr: int}:\n\treturn {q=a / b, r=a % b}\n";
    let (_t, diags, _pool) = run_with_errors(src);
    let mismatches = mismatch_diags(&diags);
    assert_eq!(mismatches.len(), 1, "got {diags:?}");
    let d = mismatches[0];
    assert!(
        d.message.contains("expects '{q: int, rr: int}'"),
        "error must show the expected shape, got: {}",
        d.message
    );
    assert!(
        d.message.contains("got '{q: int, r: int}'"),
        "error must show the found shape, got: {}",
        d.message
    );
    let notes = note_text(d);
    assert!(
        notes.contains("expected 'rr', found 'r'"),
        "note must name the differing fields, got: {notes}"
    );
    assert!(
        notes.contains("did you mean 'r'?"),
        "name typos get a did-you-mean, got: {notes}"
    );
}

#[test]
fn type_diff_note_names_the_field() {
    let src = "fn f() -> {q: int}:\n\treturn {q=\"a\"}\n";
    let (_t, diags, _pool) = run_with_errors(src);
    let mismatches = mismatch_diags(&diags);
    assert_eq!(mismatches.len(), 1, "got {diags:?}");
    let notes = note_text(mismatches[0]);
    assert!(
        notes.contains("field 'q': expected 'int', found 'str'"),
        "type diffs name the field, got: {notes}"
    );
}

#[test]
fn field_count_diff_note_names_missing_field() {
    let src = "fn f() -> {q: int, r: int}:\n\treturn {q=1}\n";
    let (_t, diags, _pool) = run_with_errors(src);
    let mismatches = mismatch_diags(&diags);
    assert_eq!(mismatches.len(), 1, "got {diags:?}");
    let notes = note_text(mismatches[0]);
    assert!(
        notes.contains("the found shape is missing field(s) 'r'"),
        "count diffs name the missing field, got: {notes}"
    );
}

#[test]
fn named_expected_anon_found_graduation_fixit() {
    // §9 bar (graduation): passing an anonymous shape where a named
    // struct is expected gets a fix-it that teaches explicit
    // construction with the real struct name and field names.
    let src = "struct Point:\n\tx: int\n\ty: int\n\nfn take(p: Point):\n\tprint(p.x)\n\nfn main():\n\ttake({x=1, y=2})\n";
    let (_t, diags, _pool) = run_with_errors(src);
    let mismatches = mismatch_diags(&diags);
    assert_eq!(mismatches.len(), 1, "got {diags:?}");
    let d = mismatches[0];
    assert!(
        d.message.contains("expected 'Point'"),
        "error must name the expected struct, got: {}",
        d.message
    );
    assert!(
        d.message.contains("has type '{x: int, y: int}'"),
        "error must show the found shape, got: {}",
        d.message
    );
    assert!(
        d.notes
            .iter()
            .any(|n| n.message == "help: construct explicitly: `Point{x=…, y=…}`"),
        "expected the graduation fix-it, got: {}",
        note_text(d)
    );
}
