//! M12 `match` sema tests — pattern resolution: scrutinee validation
//! (E0136), literal-kind checks (E0131), variant resolution (E0132)
//! with the payload-shape diagnostics it reuses (E0120 / E0121 /
//! E0122 / E0125), and anon/tuple pattern shape checks (E0113 / E0114
//! / E0040). Split by section out of the original `tests_match.rs`
//! to stay under the file-length limit: exhaustiveness and redundancy
//! live in `tests_match_exhaustiveness.rs`, arm typing in
//! `tests_match_typing.rs`, and arm guard/termination coverage in
//! `tests_match_arm_guards.rs`. Harness shared with `tests.rs` via
//! its `pub(super)` helpers; the match-view helpers below are
//! `pub(super)` for those sibling files.

use super::tests::*;
use super::*;
use ryo_core::tir::{Discriminant, TirData, TirRef, TirTag};

/// The operand of the statement at `stmt_idx` of `main` when it is an
/// expression statement — a statement-position match instruction.
pub(super) fn match_stmt_operand(tir: &ryo_core::tir::Tir, stmt_idx: usize) -> TirRef {
    let stmt = stmt_at(tir, stmt_idx);
    let TirData::UnOp(operand) = tir.inst(stmt).data else {
        panic!(
            "statement-level match must be an ExprStmt, got {:?}",
            tir.inst(stmt)
        );
    };
    operand
}

/// The initializer of the `x = ...` var decl at `stmt_idx` — the match
/// expression under test when it sits in value position.
pub(super) fn match_in_var_init(tir: &ryo_core::tir::Tir, stmt_idx: usize) -> TirRef {
    tir.var_decl_view(stmt_at(tir, stmt_idx)).initializer
}

#[test]
fn int_match_with_literal_and_wildcard_arms_resolves() {
    // `match 5:` with an int-literal arm and a wildcard: clean resolution
    // types as the joined arm type (both arms yield int).
    let src = "fn main():\n\tmatch 5:\n\t\t42: 1\n\t\t_: 0\n";
    let (tirs, pool) = run(src).expect("sema ok");
    let main = tir_named(&tirs, &pool, "main");
    let m = match_stmt_operand(main, 0);
    assert!(
        matches!(main.inst(m).tag, TirTag::Match),
        "clean match emits TirTag::Match, got {:?}",
        main.inst(m).tag
    );
    assert_eq!(
        main.inst(m).ty,
        pool.int(),
        "the match types as the joined arm type"
    );
    let view = main.match_view(m);
    assert_eq!(view.arm(0).discriminant, Discriminant::Int(42));
    assert_eq!(view.arm(1).discriminant, Discriminant::Wildcard);
}

#[test]
fn void_scrutinee_is_rejected_with_e0136() {
    // A void-returning call produces no value to match on — E0136 names
    // the scrutinee type and lists what IS matchable. The match recovers
    // with the error-typed sentinel.
    let src = "fn void_fn():\n\tprint(\"hi\")\n\nfn main():\n\tmatch void_fn():\n\t\t_: 0\n";
    let (tirs, diags, pool) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::InvalidMatchScrutinee)
        .expect("E0136 must fire");
    assert!(
        diag.message.contains("'void'"),
        "message must name the scrutinee type, got {:?}",
        diag.message
    );
    for needle in ["enums", "int", "bool", "str", "anonymous struct"] {
        assert!(
            diag.message.contains(needle),
            "message must list what is matchable ({needle}), got {:?}",
            diag.message
        );
    }
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
    // The diagnostic anchors on the scrutinee expression, not the match
    // keyword: `match void_fn():` — the scrutinee starts after "match ".
    let line_start = src.find("\tmatch void_fn():").expect("match line");
    let scrut_start = line_start + "\tmatch ".len();
    assert_eq!(
        diag.span.start, scrut_start,
        "E0136 must underline the scrutinee, got {:?}",
        diag.span
    );
    let main = tir_named(&tirs, &pool, "main");
    let stub = match_stmt_operand(main, 0);
    assert_eq!(
        main.inst(stub).ty,
        pool.error_type(),
        "errored match recovers with the error type"
    );
}

#[test]
fn named_struct_scrutinee_is_rejected_with_e0136() {
    // Named structs are deliberately not matchable in M12 (nominal
    // struct patterns are M12.5) — the message lists the matchable set.
    let src = "struct Point:\n\tx: int\n\ty: int\n\nfn main():\n\tp = Point{x=1, y=2}\n\tmatch p:\n\t\t_: 0\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::InvalidMatchScrutinee)
        .expect("E0136 must fire");
    assert!(
        diag.message.contains("'Point'"),
        "message must name the scrutinee type, got {:?}",
        diag.message
    );
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
}

#[test]
fn string_literal_arm_on_int_scrutinee_is_e0131() {
    // A literal pattern whose kind disagrees with the scrutinee type
    // names both sides.
    let src = "fn main():\n\tmatch 5:\n\t\t\"s\": 1\n\t\t_: 0\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::PatternTypeMismatch)
        .expect("E0131 must fire");
    for needle in ["string literal", "'int'"] {
        assert!(
            diag.message.contains(needle),
            "message should name {needle}, got {:?}",
            diag.message
        );
    }
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
}

#[test]
fn int_literal_arm_on_bool_scrutinee_is_e0131() {
    let src = "fn main():\n\tmatch true:\n\t\t1: 0\n\t\t_: false\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::PatternTypeMismatch)
        .expect("E0131 must fire");
    for needle in ["integer literal", "'bool'"] {
        assert!(
            diag.message.contains(needle),
            "message should name {needle}, got {:?}",
            diag.message
        );
    }
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
}

#[test]
fn literal_arm_on_enum_scrutinee_points_at_variant_matching() {
    // Literals never match enums — E0131 names both sides and the help
    // note teaches the enum-matching form.
    let src = "enum Color:\n\tRed\n\tGreen\n\nfn main():\n\tc = Color.Red\n\tmatch c:\n\t\t1: 0\n\t\t_: 1\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::PatternTypeMismatch)
        .expect("E0131 must fire");
    assert!(
        diag.message.contains("'Color'") || diag.notes.iter().any(|n| n.message.contains("Color")),
        "enum side must be named, got {:?} / {:?}",
        diag.message,
        diag.notes
    );
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
}

#[test]
fn variant_pattern_on_int_scrutinee_is_e0131() {
    // The mirror mismatch: a variant pattern where the scrutinee is not
    // an enum names the pattern kind and the scrutinee type.
    let src = "enum Color:\n\tRed\n\nfn main():\n\tmatch 5:\n\t\tColor.Red: 1\n\t\t_: 0\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::PatternTypeMismatch)
        .expect("E0131 must fire");
    for needle in ["variant pattern", "'int'"] {
        assert!(
            diag.message.contains(needle),
            "message should name {needle}, got {:?}",
            diag.message
        );
    }
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
}

#[test]
fn unknown_variant_arm_is_e0132_without_bogus_suggestion() {
    // `Orange` is not a Color variant and is far from both real names —
    // the diagnostic names the identifier and the scrutinee enum, with
    // no did-you-mean note.
    let src = "enum Color:\n\tRed\n\tGreen\n\nfn main():\n\tc = Color.Red\n\tmatch c:\n\t\tOrange: 1\n\t\t_: 0\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::UnknownMatchVariant)
        .expect("E0132 must fire");
    for needle in ["'Color'", "'Orange'"] {
        assert!(
            diag.message.contains(needle),
            "message should name {needle}, got {:?}",
            diag.message
        );
    }
    assert!(
        diag.notes
            .iter()
            .all(|n| !n.message.contains("did you mean")),
        "no close variant exists — no did-you-mean note, got {:?}",
        diag.notes
    );
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
}

#[test]
fn unknown_variant_arm_suggests_close_name() {
    // `Grean` is one edit from `Green` — E0132 carries a did-you-mean
    // help note over the scrutinee enum's variant list.
    let src = "enum Color:\n\tRed\n\tGreen\n\nfn main():\n\tc = Color.Red\n\tmatch c:\n\t\tGrean: 1\n\t\t_: 0\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::UnknownMatchVariant)
        .expect("E0132 must fire");
    assert!(
        diag.notes.iter().any(|n| n.message.contains("'Green'")),
        "did-you-mean note should suggest 'Green', got {:?}",
        diag.notes
    );
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
}

#[test]
fn unqualified_variant_resolves_against_scrutinee_enum_only() {
    // `Green:` unqualified on a Color scrutinee resolves — the name is
    // looked up in the scrutinee enum's variant list, never the scope.
    let src = "enum Color:\n\tRed\n\tGreen\n\nfn main():\n\tc = Color.Red\n\tmatch c:\n\t\tGreen: 1\n\t\t_: 0\n";
    run(src).expect("unqualified variant on the scrutinee enum resolves");
}

#[test]
fn qualified_variant_from_wrong_enum_is_e0132() {
    // `Other.Red` names a real variant of a real enum — but not the
    // scrutinee's. E0132 names both enums.
    let src = "enum Color:\n\tRed\n\tGreen\n\nenum Other:\n\tBlue\n\nfn main():\n\tc = Color.Red\n\tmatch c:\n\t\tOther.Red: 1\n\t\t_: 0\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::UnknownMatchVariant)
        .expect("E0132 must fire");
    for needle in ["'Other'", "'Color'"] {
        assert!(
            diag.message.contains(needle),
            "message should name both enums ({needle}), got {:?}",
            diag.message
        );
    }
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
}

#[test]
fn qualified_pattern_with_unknown_enum_name_is_unknown_type() {
    // A qualified path naming a never-declared type is UnknownType
    // (mirrors M11 construction's undeclared-enum handling); E0132 is
    // reserved for declared-but-different enums.
    let src = "enum Color:\n\tRed\n\nfn main():\n\tc = Color.Red\n\tmatch c:\n\t\tNope.Red: 1\n\t\t_: 0\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::UnknownType)
        .expect("UnknownType must fire for a never-declared enum name");
    assert!(
        diag.message.contains("'Nope'"),
        "message should name the identifier, got {:?}",
        diag.message
    );
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
}

#[test]
fn payload_positional_overflow_is_e0122() {
    // `Circle(radius, extra):` on a one-field tuple variant — the arity
    // error reuses the construction-side UnknownVariantField wording.
    let src = "enum Shape:\n\tCircle(float)\n\nfn main():\n\ts = Shape.Circle(1.0)\n\tmatch s:\n\t\tCircle(radius, extra): 1\n\t\t_: 0\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::UnknownVariantField)
        .expect("E0122 must fire");
    for needle in ["second", "takes 1"] {
        assert!(
            diag.message.contains(needle),
            "message should contain {needle}, got {:?}",
            diag.message
        );
    }
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
}

#[test]
fn tuple_payload_underflow_is_e0120() {
    // `Both(a):` leaves a payload slot unmatched — full payload coverage
    // is required (write `_` to skip a slot), mirroring construction.
    let src = "enum Pair:\n\tBoth(int, int)\n\nfn main():\n\tp = Pair.Both(1, 2)\n\tmatch p:\n\t\tBoth(a): 0\n\t\t_: 1\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::MissingVariantFields)
        .expect("E0120 must fire");
    for needle in ["'Both'", "takes 2", "got 1"] {
        assert!(
            diag.message.contains(needle),
            "message should contain {needle}, got {:?}",
            diag.message
        );
    }
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
}

#[test]
fn named_payload_missing_field_is_e0120() {
    let src = "enum Rect:\n\tBox(width: float, height: float)\n\nfn main():\n\tr = Rect.Box{width=1.0, height=2.0}\n\tmatch r:\n\t\tBox{width=ww}: 0\n\t\t_: 1\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::MissingVariantFields)
        .expect("E0120 must fire");
    for needle in ["'height'", "'Box'"] {
        assert!(
            diag.message.contains(needle),
            "message should name {needle}, got {:?}",
            diag.message
        );
    }
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
}

#[test]
fn named_payload_duplicate_field_is_e0121() {
    let src = "enum Rect:\n\tBox(width: float, height: float)\n\nfn main():\n\tr = Rect.Box{width=1.0, height=2.0}\n\tmatch r:\n\t\tBox{width=ww, height=hh, width=ww2}: 0\n\t\t_: 1\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::DuplicateVariantField)
        .expect("E0121 must fire");
    assert!(
        diag.message.contains("'width'"),
        "message should name the duplicate field, got {:?}",
        diag.message
    );
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
}

#[test]
fn unit_variant_with_payload_args_is_e0125() {
    // `Red(x):` — a unit variant takes no payload; the match-pattern
    // flavor of the construction-side unit-variant check.
    let src = "enum Color:\n\tRed\n\tGreen\n\nfn main():\n\tc = Color.Red\n\tmatch c:\n\t\tRed(x): 1\n\t\t_: 0\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::UnitVariantConstructed)
        .expect("E0125 must fire");
    for needle in ["unit variant", "'Red'"] {
        assert!(
            diag.message.contains(needle),
            "message should contain {needle}, got {:?}",
            diag.message
        );
    }
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
}

#[test]
fn statement_match_without_wildcard_is_exactly_one_e0129() {
    // An int match with no wildcard is non-exhaustive — exactly one
    // E0129 names the kind; resolution contributes no diagnostic of
    // its own (no double-report from the resolution pass).
    let src = "fn main():\n\tmatch 5:\n\t\t0: 1\n\t\t1: 2\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::NonExhaustiveMatch)
        .expect("E0129 must fire");
    assert!(
        diag.message.contains("'int'"),
        "message must name the scrutinee kind, got {:?}",
        diag.message
    );
    assert!(
        diag.notes
            .iter()
            .any(|n| n.message.contains("add a `_:` arm")),
        "help note must suggest a wildcard arm, got {:?}",
        diag.notes
    );
    assert_eq!(diags.len(), 1, "exactly one diagnostic: {diags:?}");
}

#[test]
fn two_broken_arms_both_report_and_anchor_per_arm() {
    // Continuity: one broken arm never suppresses the next, and each
    // diagnostic anchors on its own arm (later arm = later span).
    let src = "fn main():\n\tmatch 5:\n\t\t\"a\": 1\n\t\t\"b\": 2\n\t\t_: 0\n";
    let (_t, diags, _p) = run_with_errors(src);
    assert_eq!(
        count_code(&diags, DiagCode::PatternTypeMismatch),
        2,
        "one E0131 per broken arm: {diags:?}"
    );
    assert!(diags[0].span.start < diags[1].span.start);
}

#[test]
fn anon_struct_scrutinee_with_full_pattern_resolves() {
    // `{x=0, y=_}:` on a `{x: int, y: int}` scrutinee — literal field,
    // wildcard skip, full coverage: resolves with no diagnostics.
    let src = "fn main():\n\tp = {x=1, y=2}\n\tmatch p:\n\t\t{x=0, y=_}: 0\n\t\t_: 1\n";
    run(src).expect("full anon pattern resolves");
}

#[test]
fn tuple_sugar_pattern_on_anon_scrutinee_resolves() {
    // `(0, _):` — positional literal + wildcard over the two-element
    // anonymous shape.
    let src = "fn main():\n\tp = {x=1, y=2}\n\tmatch p:\n\t\t(0, _): 0\n\t\t_: 1\n";
    run(src).expect("tuple-sugar pattern resolves");
}

#[test]
fn anon_pattern_unknown_field_is_e0114() {
    let src = "fn main():\n\tp = {x=1, y=2}\n\tmatch p:\n\t\t{z=0, y=_}: 0\n\t\t_: 1\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::DestructureUnknownField)
        .expect("E0114 must fire");
    assert!(
        diag.message.contains("`z`"),
        "message should name the unknown field, got {:?}",
        diag.message
    );
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
}

#[test]
fn anon_pattern_duplicate_field_is_e0040() {
    let src = "fn main():\n\tp = {x=1, y=2}\n\tmatch p:\n\t\t{x=0, x=_}: 0\n\t\t_: 1\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::DuplicateStructField)
        .expect("E0040 must fire");
    assert!(
        diag.message.contains("'x'"),
        "message should name the duplicate field, got {:?}",
        diag.message
    );
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
}

#[test]
fn anon_pattern_partial_coverage_is_e0114() {
    // Match patterns keep the M10 full-coverage rule: every field is
    // matched, bound, or written `_`.
    let src = "fn main():\n\tp = {x=1, y=2}\n\tmatch p:\n\t\t{x=0}: 0\n\t\t_: 1\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::DestructureUnknownField)
        .expect("E0114 must fire");
    for needle in ["does not cover", "`y`", "add `y = _`"] {
        assert!(
            diag.message.contains(needle),
            "message should contain {needle}, got {:?}",
            diag.message
        );
    }
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
}

#[test]
fn anon_pattern_partial_coverage_suggestion_names_first_missing_field() {
    // The "add `{first} = _`" suggestion must name the first MISSING
    // field, not `shape.fields[0]` — for `{y=0}` on `{x, y}` that is
    // `x` (already-covered `y` must not be suggested).
    let src = "fn main():\n\tp = {x=1, y=2}\n\tmatch p:\n\t\t{y=0}: 0\n\t\t_: 1\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::DestructureUnknownField)
        .expect("E0114 must fire");
    assert!(
        diag.message.contains("add `x = _`"),
        "suggestion must name the first missing field `x`, got {:?}",
        diag.message
    );
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
}

#[test]
fn anon_pattern_literal_kind_mismatch_is_e0131() {
    // A literal field whose kind disagrees with the field type names
    // both sides, anchored on the arm.
    let src = "fn main():\n\tp = {x=1, y=2}\n\tmatch p:\n\t\t{x=\"s\", y=_}: 0\n\t\t_: 1\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::PatternTypeMismatch)
        .expect("E0131 must fire");
    for needle in ["string literal", "'x'", "'int'"] {
        assert!(
            diag.message.contains(needle),
            "message should name {needle}, got {:?}",
            diag.message
        );
    }
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
}

#[test]
fn positional_pattern_arity_mismatch_is_e0113() {
    let src = "fn main():\n\tp = {x=1, y=2}\n\tmatch p:\n\t\t(a, b, c): 0\n\t\t_: 1\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::DestructureArity)
        .expect("E0113 must fire");
    for needle in ["expected 2", "found 3"] {
        assert!(
            diag.message.contains(needle),
            "message should contain {needle}, got {:?}",
            diag.message
        );
    }
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
}
