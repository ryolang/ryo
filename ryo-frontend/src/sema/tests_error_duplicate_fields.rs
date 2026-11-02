use super::tests::*;
use super::*;

fn assert_only_duplicate_field(src: &str) {
    let (_tirs, diags, _pool) = run_with_errors(src);
    assert_eq!(
        count_code(&diags, DiagCode::DuplicateStructField),
        1,
        "expected one E0040 for {src:?}: {diags:?}"
    );
    assert_eq!(diags.len(), 1, "unexpected cascade for {src:?}: {diags:?}");
}

#[test]
fn duplicate_error_field_declaration_emits_only_e0040() {
    assert_only_duplicate_field("error E(x:int,x:int)\n");
}

#[test]
fn constructor_after_duplicate_error_field_recovers() {
    assert_only_duplicate_field("error E(x:int,x:int)\nfn main():\n\tE\n");
}

#[test]
fn return_after_duplicate_error_field_recovers() {
    assert_only_duplicate_field("error E(x:int,x:int)\nfn fail() -> E!int:\n\treturn E\n");
}

#[test]
fn match_after_duplicate_error_field_recovers() {
    assert_only_duplicate_field(
        "error E(x:int,x:int)\nfn main():\n\tmatch E:\n\t\tE: print(\"x\")\n",
    );
}

#[test]
fn message_after_duplicate_error_field_recovers() {
    assert_only_duplicate_field("error E(x:int,x:int)\nfn main():\n\tprint(E.message())\n");
}
