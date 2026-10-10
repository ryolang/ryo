//! M12 `match` sema tests — pattern resolution (E0131-E0136 family) and
//! exhaustiveness / redundancy (E0129 / W0005). Harness shared with
//! `tests.rs` via its `pub(super)` helpers (the 2000-line file cap keeps
//! these out of `tests.rs`).
//!
//! Task 5 scope: scrutinee validation (E0136), literal-kind checks
//! (E0131), variant resolution (E0132) + payload shape checks (E0120 /
//! E0121 / E0122 / E0125 reuse), anon/tuple shape checks (E0113 / E0114 /
//! E0040 reuse), and the resolution-stub return. Task 6 adds
//! exhaustiveness (E0129) and unreachable-arm warnings (W0005); arm
//! typing and binding insertion (E0133-E0135, TIR `Match`) land in
//! Task 7 — the stub tests below pin the interim contract those tasks
//! replace.

use super::tests::*;
use super::*;
use crate::astgen;
use crate::lexer::lex;
use crate::parser::{ParseState, program_parser};
use chumsky::span::Span as _;
use chumsky::{Parser, input::Input};
use ryo_core::tir::{TirData, TirTag};

/// The single statement of `main` wrapped around a statement-level
/// `match`, and the match's stub instruction — every Task 5 path emits
/// an `Unreachable` placeholder (void when resolution is clean, error
/// when any diagnostic fired).
fn match_stub(tir: &ryo_core::tir::Tir) -> ryo_core::tir::TirRef {
    let stmt = stmt_at(tir, 0);
    let TirData::UnOp(operand) = tir.inst(stmt).data else {
        panic!(
            "statement-level match must be an ExprStmt, got {:?}",
            tir.inst(stmt)
        );
    };
    operand
}

#[test]
fn int_match_with_literal_and_wildcard_arms_resolves() {
    // `match 5:` with an int-literal arm and a wildcard resolves with no
    // diagnostics; the stub returns void (Task 7 replaces it with the
    // typed TIR Match).
    let src = "fn main():\n\tmatch 5:\n\t\t42: 1\n\t\t_: 0\n";
    let (tirs, pool) = run(src).expect("sema ok");
    let main = tir_named(&tirs, &pool, "main");
    let stub = match_stub(main);
    assert!(
        matches!(main.inst(stub).tag, TirTag::Unreachable),
        "stub is an Unreachable placeholder, got {:?}",
        main.inst(stub).tag
    );
    assert_eq!(
        main.inst(stub).ty,
        pool.void(),
        "clean resolution stubs void until Task 7 emits the typed Match"
    );
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
        diag.span.start(),
        scrut_start,
        "E0136 must underline the scrutinee, got {:?}",
        diag.span
    );
    let main = tir_named(&tirs, &pool, "main");
    let stub = match_stub(main);
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
    assert!(diags[0].span.start() < diags[1].span.start());
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
    for needle in ["does not cover", "`y`"] {
        assert!(
            diag.message.contains(needle),
            "message should contain {needle}, got {:?}",
            diag.message
        );
    }
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

// ---------- Task 6: exhaustiveness (E0129) + redundancy (W0005) ----------

/// `run_with_errors` variant that surfaces sema's buffered warnings
/// (W0005) even when errors exist in the same unit. The plain harness
/// drops buffered warnings next to errors per the house rule that
/// warnings never pile onto errors; the driver instead flushes them
/// via `analyze_buffered` when the whole unit is clean. Combined
/// E0129+W0005 inputs need both sides visible.
fn run_buffered(input: &str) -> (Vec<Tir>, Vec<Diag>, InternPool) {
    let mut pool = InternPool::new();
    let mut lex_sink = DiagSink::new();
    let tokens = lex(input, &mut pool, &mut lex_sink);
    assert!(
        !lex_sink.has_errors(),
        "lex errors: {:?}",
        lex_sink.into_diags()
    );
    let token_stream = tokens[..].split_token_span((0..input.len()).into());
    let mut state = ParseState::new(pool);
    program_parser()
        .parse_with_state(token_stream, &mut state)
        .into_result()
        .expect("parse ok");
    let (ast, mut pool) = state.into_parts();

    let mut sink = DiagSink::new();
    let uir = astgen::generate(&ast, &mut pool, &mut sink);
    let (tirs, warnings) = analyze_buffered(&uir, &mut pool, &mut sink, input, Path::new("<test>"));
    let mut diags = sink.into_diags();
    diags.extend(warnings);
    (tirs, diags, pool)
}

#[test]
fn enum_match_covering_all_variants_is_exhaustive() {
    // Arms in declaration order covering every variant — no E0129.
    let src = "enum Color:\n\tRed\n\tGreen\n\nfn main():\n\tc = Color.Red\n\tmatch c:\n\t\tRed: 1\n\t\tGreen: 2\n";
    run(src).expect("full variant coverage is exhaustive");
}

#[test]
fn enum_match_missing_variant_lists_it_in_e0129() {
    let src =
        "enum Color:\n\tRed\n\tGreen\n\nfn main():\n\tc = Color.Red\n\tmatch c:\n\t\tRed: 1\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::NonExhaustiveMatch)
        .expect("E0129 must fire");
    assert!(
        diag.message.contains("'Green'"),
        "message must list the missing variant by name, got {:?}",
        diag.message
    );
    assert!(
        diag.message.contains("not covered"),
        "message must say what is wrong, got {:?}",
        diag.message
    );
    assert!(
        diag.notes
            .iter()
            .any(|n| n.message.contains("add a `_:` arm")),
        "help note must suggest a wildcard arm, got {:?}",
        diag.notes
    );
    // The enum scrutinee gets a secondary underline naming its type.
    assert!(
        diag.notes
            .iter()
            .any(|n| n.span.is_some() && n.message.contains("'Color'")),
        "a spanned note must name the enum on the scrutinee, got {:?}",
        diag.notes
    );
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
}

#[test]
fn enum_match_missing_variants_list_in_declaration_order() {
    // Missing 'Green' and 'Blue': the message lists them in
    // DECLARATION order (Green before Blue) regardless of arm order.
    let src = "enum Color:\n\tRed\n\tGreen\n\tBlue\n\tYellow\n\nfn main():\n\tc = Color.Red\n\tmatch c:\n\t\tYellow: 1\n\t\tRed: 2\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::NonExhaustiveMatch)
        .expect("E0129 must fire");
    let green = diag.message.find("'Green'").expect("lists 'Green'");
    let blue = diag.message.find("'Blue'").expect("lists 'Blue'");
    assert!(
        green < blue,
        "missing variants must be listed in declaration order, got {:?}",
        diag.message
    );
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
}

#[test]
fn e0129_anchor_is_the_match_expression() {
    // The diagnostic underlines the whole match, starting at the
    // `match` keyword (the R11 best-available anchor for a match-level
    // diagnostic — the wire carries no pattern spans).
    let src =
        "enum Color:\n\tRed\n\tGreen\n\nfn main():\n\tc = Color.Red\n\tmatch c:\n\t\tRed: 1\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::NonExhaustiveMatch)
        .expect("E0129 must fire");
    let match_start = src.find("\tmatch c:").expect("match line") + "\t".len();
    assert_eq!(
        diag.span.start(),
        match_start,
        "E0129 must start at the `match` keyword, got {:?}",
        diag.span
    );
}

#[test]
fn wildcard_arm_satisfies_exhaustiveness() {
    // A wildcard covers what the listed arms miss — enum, and `_:` alone.
    let src = "enum Color:\n\tRed\n\tGreen\n\nfn main():\n\tc = Color.Red\n\tmatch c:\n\t\tRed: 1\n\t\t_: 0\n";
    run(src).expect("wildcard satisfies enum exhaustiveness");
    let src = "enum Color:\n\tRed\n\tGreen\n\nfn main():\n\tc = Color.Red\n\tmatch c:\n\t\t_: 0\n";
    run(src).expect("a lone wildcard arm is exhaustive");
}

#[test]
fn bool_match_with_true_and_false_is_exhaustive() {
    let src = "fn main():\n\tmatch true:\n\t\ttrue: 1\n\t\tfalse: 2\n";
    run(src).expect("both bool literals cover the value space");
}

#[test]
fn bool_match_missing_false_is_e0129() {
    let src = "fn main():\n\tmatch true:\n\t\ttrue: 1\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::NonExhaustiveMatch)
        .expect("E0129 must fire");
    assert!(
        diag.message.contains("'false'"),
        "message must name the missing bool literal, got {:?}",
        diag.message
    );
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
}

#[test]
fn int_match_without_wildcard_is_e0129() {
    // Any literal set over int is uncoverable — the message names the
    // kind; the wildcard arm is the only way to be exhaustive.
    let src = "fn main():\n\tmatch 5:\n\t\t0: 1\n\t\t1: 2\n\t\t2: 3\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::NonExhaustiveMatch)
        .expect("E0129 must fire");
    assert!(
        diag.message.contains("'int'"),
        "message must name the kind, got {:?}",
        diag.message
    );
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
}

#[test]
fn str_match_without_wildcard_is_e0129() {
    let src = "fn main():\n\tmatch \"a\":\n\t\t\"a\": 1\n\t\t\"b\": 2\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::NonExhaustiveMatch)
        .expect("E0129 must fire");
    assert!(
        diag.message.contains("'str'"),
        "message must name the kind, got {:?}",
        diag.message
    );
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
}

#[test]
fn str_match_with_wildcard_is_exhaustive() {
    let src = "fn main():\n\tmatch \"a\":\n\t\t\"quit\": 1\n\t\t_: 0\n";
    run(src).expect("wildcard satisfies str exhaustiveness");
}

#[test]
fn anon_scrutinee_without_wildcard_is_e0129() {
    // M12 anon patterns carry no discriminant (structural lowering is
    // M12.5), so they cannot count toward coverage: without a `_:` arm
    // the match is non-exhaustive, and the message names the shape.
    let src = "fn main():\n\tp = {x=1, y=2}\n\tmatch p:\n\t\t{x=0, y=_}: 0\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::NonExhaustiveMatch)
        .expect("E0129 must fire");
    assert!(
        diag.message.contains("{x: int, y: int}"),
        "message must name the scrutinee shape, got {:?}",
        diag.message
    );
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
}

#[test]
fn arm_after_wildcard_is_w0005_warning() {
    // `_:` matches everything: a later arm can never run. Warning
    // severity, the pattern's source text quoted, no error — codegen
    // is unaffected and the stub stays void-typed.
    let src = "fn main():\n\tmatch 5:\n\t\t_: 0\n\t\t1: 2\n";
    let (_t, diags, _p) = run_with_errors(src);
    let warn = diags
        .iter()
        .find(|d| d.code == DiagCode::UnreachableArm)
        .expect("W0005 must fire");
    assert_eq!(
        warn.severity,
        ryo_core::diag::Severity::Warning,
        "W0005 is a warning, got {:?}",
        warn.severity
    );
    assert!(
        warn.message.contains("`1`"),
        "message must quote the redundant pattern's source text, got {:?}",
        warn.message
    );
    assert_eq!(diags.len(), 1, "exactly one diagnostic: {diags:?}");

    // Warning-only unit: sema succeeds and the stub stays void.
    let (tirs, pool) = run(src).expect("a redundant arm is a warning, not an error");
    let main = tir_named(&tirs, &pool, "main");
    assert_eq!(
        main.inst(match_stub(main)).ty,
        pool.void(),
        "warnings must not poison the stub's type"
    );
}

#[test]
fn arm_after_wildcard_is_w0005_for_every_shape() {
    // One W0005 per redundant arm — variant, literal, and anon shapes
    // all quote their source text.
    let src = "enum Color:\n\tRed\n\tGreen\n\nfn main():\n\tc = Color.Red\n\tmatch c:\n\t\t_: 0\n\t\tRed: 1\n\t\tGreen: 2\n";
    let (_t, diags, _p) = run_with_errors(src);
    let warns: Vec<_> = diags
        .iter()
        .filter(|d| d.code == DiagCode::UnreachableArm)
        .collect();
    assert_eq!(warns.len(), 2, "one W0005 per arm after `_:`: {diags:?}");
    assert!(
        warns[0].message.contains("`Red`"),
        "got {:?}",
        warns[0].message
    );
    assert!(
        warns[1].message.contains("`Green`"),
        "got {:?}",
        warns[1].message
    );
    assert!(
        warns[0].span.start() < warns[1].span.start(),
        "spans follow arm order"
    );
    assert_eq!(diags.len(), 2, "no other diagnostics: {diags:?}");
}

#[test]
fn anon_arm_after_wildcard_quotes_its_shape() {
    // Anon patterns carry no discriminant, but an anon arm after `_`
    // is still dead — W0005 quotes the shape spelling.
    let src =
        "fn main():\n\tp = {x=1, y=2}\n\tmatch p:\n\t\t_: 0\n\t\t{x=0, y=_}: 1\n\t\t(0, _): 2\n";
    let (_t, diags, _p) = run_with_errors(src);
    let warns: Vec<_> = diags
        .iter()
        .filter(|d| d.code == DiagCode::UnreachableArm)
        .collect();
    assert_eq!(warns.len(), 2, "one W0005 per arm after `_:`: {diags:?}");
    assert!(
        warns[0].message.contains("`{x=0, y=_}`"),
        "got {:?}",
        warns[0].message
    );
    assert!(
        warns[1].message.contains("`(0, _)`"),
        "got {:?}",
        warns[1].message
    );
    assert_eq!(diags.len(), 2, "no other diagnostics: {diags:?}");
}

#[test]
fn duplicate_literal_arm_is_w0005_on_the_second() {
    let src = "fn main():\n\tmatch 5:\n\t\t42: 1\n\t\t42: 2\n\t\t_: 0\n";
    let (_t, diags, _p) = run_with_errors(src);
    let warn = diags
        .iter()
        .find(|d| d.code == DiagCode::UnreachableArm)
        .expect("W0005 must fire");
    assert_eq!(warn.severity, ryo_core::diag::Severity::Warning);
    assert!(
        warn.message.contains("`42`"),
        "message must quote the duplicated literal, got {:?}",
        warn.message
    );
    // Anchored on the SECOND arm (the redundant one), via arm_span.
    let second = src.find("\t\t42: 2").expect("second arm line");
    let body = second + "\t\t42: ".len();
    assert_eq!(
        warn.span.start(),
        body,
        "W0005 must anchor on the redundant arm's body, got {:?}",
        warn.span
    );
    assert_eq!(diags.len(), 1, "exactly one diagnostic: {diags:?}");
}

#[test]
fn duplicate_variant_arm_is_w0005_on_the_second() {
    let src = "enum Color:\n\tRed\n\tGreen\n\nfn main():\n\tc = Color.Red\n\tmatch c:\n\t\tRed: 1\n\t\tRed: 2\n\t\t_: 0\n";
    let (_t, diags, _p) = run_with_errors(src);
    let warn = diags
        .iter()
        .find(|d| d.code == DiagCode::UnreachableArm)
        .expect("W0005 must fire");
    assert_eq!(warn.severity, ryo_core::diag::Severity::Warning);
    assert!(
        warn.message.contains("`Red`"),
        "message must quote the duplicated variant, got {:?}",
        warn.message
    );
    let second = src.find("\t\tRed: 2").expect("second arm line");
    let body = second + "\t\tRed: ".len();
    assert_eq!(
        warn.span.start(),
        body,
        "W0005 must anchor on the redundant arm's body, got {:?}",
        warn.span
    );
    assert_eq!(diags.len(), 1, "exactly one diagnostic: {diags:?}");
}

#[test]
fn wildcard_after_full_coverage_makes_later_arms_redundant_without_e0129() {
    // Full coverage BEFORE the wildcard: the wildcard satisfies
    // exhaustiveness, so E0129 must NOT fire, and every arm after it
    // is dead — W0005 on the trailing arm.
    let src = "fn main():\n\tmatch true:\n\t\ttrue: 1\n\t\tfalse: 2\n\t\t_: 0\n\t\ttrue: 3\n";
    let (_t, diags, _p) = run_with_errors(src);
    let warn = diags
        .iter()
        .find(|d| d.code == DiagCode::UnreachableArm)
        .expect("W0005 must fire on the arm after the wildcard");
    assert_eq!(warn.severity, ryo_core::diag::Severity::Warning);
    assert!(
        warn.message.contains("`true`"),
        "message must quote the redundant pattern, got {:?}",
        warn.message
    );
    assert_eq!(
        diags.len(),
        1,
        "exactly one diagnostic, no E0129: {diags:?}"
    );
}

#[test]
fn full_coverage_without_wildcard_makes_extra_arms_redundant_without_e0129() {
    // bool fully covered, then a duplicate arm: W0005 on the trailing
    // arm, and no E0129 — coverage is complete.
    let src = "fn main():\n\tmatch true:\n\t\ttrue: 1\n\t\tfalse: 2\n\t\ttrue: 3\n";
    let (_t, diags, _p) = run_with_errors(src);
    assert_eq!(
        count_code(&diags, DiagCode::UnreachableArm),
        1,
        "the trailing duplicate arm is redundant: {diags:?}"
    );
    assert_eq!(diags.len(), 1, "no E0129 — coverage is complete: {diags:?}");
}

#[test]
fn e0129_still_fires_alongside_redundant_arms() {
    // No wildcard, duplicate arm, coverage incomplete: BOTH the E0129
    // (missing 'Green') and the W0005 (second 'Red') fire. Warnings
    // buffer separately from errors, so surface them via the buffered
    // harness; the plain one shows the error side.
    let src = "enum Color:\n\tRed\n\tGreen\n\nfn main():\n\tc = Color.Red\n\tmatch c:\n\t\tRed: 1\n\t\tRed: 2\n";
    let (_t, diags, _p) = run_buffered(src);
    assert_eq!(
        count_code(&diags, DiagCode::NonExhaustiveMatch),
        1,
        "E0129 must fire: {diags:?}"
    );
    assert_eq!(
        count_code(&diags, DiagCode::UnreachableArm),
        1,
        "W0005 must fire: {diags:?}"
    );
    let e = diags
        .iter()
        .find(|d| d.code == DiagCode::NonExhaustiveMatch)
        .expect("E0129");
    assert!(e.message.contains("'Green'"), "got {:?}", e.message);
}
