//! Destructuring-statement sema tests (M10): derived-coverage
//! suppression and arity messages that name the offending binding
//! instead of leaving the user counting. (The `(a) = x` recovery is
//! covered by parser tests plus an end-to-end test in
//! `ryo/tests/integration_destructuring.rs`.)

use super::tests::*;
use super::*;

#[test]
fn rename_typo_does_not_also_report_coverage() {
    // `{z = y} = {x=1}`: the unknown-field error is the mistake; the
    // coverage complaint is derived from it and must not fire too.
    let src = "fn main():\n\t{z = y} = {x=1}\n\tprint(y)\n";
    let (_t, diags, _p) = run_with_errors(src);
    assert_eq!(
        count_code(&diags, DiagCode::DestructureUnknownField),
        1,
        "expected exactly one E0114, got: {diags:?}"
    );
}

#[test]
fn arity_error_names_the_extra_binding() {
    // Too many positional bindings: the extras are named.
    let src = "fn main():\n\t(a, b, c) = (1, 2)\n\tprint(a)\n";
    let (_t, diags, _p) = run_with_errors(src);
    let d = diags
        .iter()
        .find(|d| d.code == DiagCode::DestructureArity)
        .expect("E0113");
    assert!(
        d.message.contains("no field left to bind 'c'"),
        "got: {}",
        d.message
    );
}

#[test]
fn arity_error_names_the_exhausted_pun() {
    // A brace pun that runs out of fields: the pun is named.
    let src = "fn main():\n\t{a, b} = {a=1}\n\tprint(a)\n";
    let (_t, diags, _p) = run_with_errors(src);
    let d = diags
        .iter()
        .find(|d| d.code == DiagCode::DestructureArity)
        .expect("E0113");
    assert!(
        d.message.contains("no field left to bind 'b'"),
        "got: {}",
        d.message
    );
}

#[test]
fn arity_error_names_the_uncovered_field() {
    // Too few bindings: the unbound shape fields are named.
    let src = "fn main():\n\t(a, b) = (1, 2, 3)\n\tprint(a)\n";
    let (_t, diags, _p) = run_with_errors(src);
    let d = diags
        .iter()
        .find(|d| d.code == DiagCode::DestructureArity)
        .expect("E0113");
    assert!(
        d.message.contains("field '2' is not covered"),
        "got: {}",
        d.message
    );
}
