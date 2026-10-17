//! M12 `match` sema tests — pattern resolution (E0131-E0137 family),
//! exhaustiveness / redundancy (E0129 / W0005), and arm typing with
//! typed TIR `Match` emission (E0133-E0135, E0137). Harness shared with
//! `tests.rs` via its `pub(super)` helpers (the 2000-line file cap keeps
//! these out of `tests.rs`).
//!
//! The suite follows the sema analysis passes: resolution (scrutinee
//! validation E0136, literal-kind checks E0131, variant resolution
//! E0132 + payload shape checks E0120 / E0121 / E0122 / E0125 reuse,
//! anon/tuple shape checks E0113 / E0114 / E0040 reuse), then
//! exhaustiveness (E0129) and unreachable-arm warnings (W0005), then
//! arm typing — per-arm binding scopes, the E0133-E0135 diagnostics —
//! and the typed `TirTag::Match` emission (owner tokens + joined
//! result type).

use super::tests::*;
use super::*;
use crate::astgen;
use crate::lexer::lex;
use crate::parser::{ParseState, program_parser};
use chumsky::span::Span as _;
use chumsky::{Parser, input::Input};
use ryo_core::ast::LitPattern;
use ryo_core::tir::{Discriminant, MatchBinding, MatchBodyView, TirData, TirRef, TirTag};

/// The operand of the statement at `stmt_idx` of `main` when it is an
/// expression statement — a statement-position match instruction.
fn match_stmt_operand(tir: &ryo_core::tir::Tir, stmt_idx: usize) -> TirRef {
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
fn match_in_var_init(tir: &ryo_core::tir::Tir, stmt_idx: usize) -> TirRef {
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
        diag.span.start(),
        scrut_start,
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

// ---------- Exhaustiveness (E0129) + redundancy (W0005) ----------

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

    // Warning-only unit: sema succeeds and the match still types as the
    // joined arm type (both arms yield int) — warnings must not poison
    // the emission.
    let (tirs, pool) = run(src).expect("a redundant arm is a warning, not an error");
    let main = tir_named(&tirs, &pool, "main");
    let m = match_stmt_operand(main, 0);
    assert!(
        matches!(main.inst(m).tag, TirTag::Match),
        "warning-only match still emits TirTag::Match, got {:?}",
        main.inst(m).tag
    );
    assert_eq!(main.inst(m).ty, pool.int());
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

// ---------- Arm typing (E0133-E0135) + typed TIR Match ----------

#[test]
fn value_join_emits_typed_match_with_arm_bindings() {
    // `match opt: Opt.Some(v): v / Opt.None: 0` — the match types as
    // `int` (the joined arm type) and arm 0 carries its payload binding
    // on the wire: (field 0, `v`, int) in declaration-order index form.
    let src = "enum Opt:\n\tSome(value: int)\n\tNone\n\nfn main():\n\to = Opt.Some{value=5}\n\tx = match o:\n\t\tOpt.Some(v): v\n\t\tOpt.None: 0\n";
    let (tirs, pool) = run(src).expect("typed match compiles");
    let main = tir_named(&tirs, &pool, "main");
    let m = match_in_var_init(main, 1);
    assert!(matches!(main.inst(m).tag, TirTag::Match));
    assert_eq!(main.inst(m).ty, pool.int());
    let view = main.match_view(m);
    let scrutinee = view.scrutinee;
    assert!(
        matches!(main.inst(scrutinee).tag, TirTag::Var),
        "the scrutinee is the `o` binding's read, got {:?}",
        main.inst(scrutinee).tag
    );
    assert_eq!(view.ty, pool.int());
    assert_eq!(view.len(), 2);
    assert_eq!(view.arm(0).discriminant, Discriminant::Variant(0));
    assert_eq!(view.arm(1).discriminant, Discriminant::Variant(1));
    let v = pool.find_str("v").expect("`v` interned");
    assert_eq!(
        view.arm(0).bindings().collect::<Vec<_>>(),
        [MatchBinding {
            field_index: 0,
            name: v,
            ty: pool.int(),
        }],
        "arm 0 binds field 0 as `v: int`"
    );
    assert_eq!(view.arm(1).binding_count(), 0);
    let MatchBodyView::Expr(arm0_body) = view.arm(0).body else {
        panic!("arm 0 is an expr body, got {:?}", view.arm(0).body);
    };
    assert_eq!(main.inst(arm0_body).ty, pool.int());
}

#[test]
fn never_joins_freely_and_statement_arm_bodies_wire_as_stmts() {
    // A `return` arm contributes `never`: the match still types as int.
    // The statement-arm body rides the wire as a stmt list (the Return
    // inst), not an expr operand.
    let src = "enum Opt:\n\tSome(value: int)\n\tNone\n\nfn f(o: Opt) -> int:\n\tx = match o:\n\t\tOpt.Some(v): return v\n\t\tOpt.None: 0\n\treturn x\n";
    let (tirs, pool) = run(src).expect("never-arm join compiles");
    let f = tir_named(&tirs, &pool, "f");
    let m = match_in_var_init(f, 0);
    assert_eq!(f.inst(m).ty, pool.int());
    let view = f.match_view(m);
    let arm0 = view.arm(0);
    let stmts: Vec<TirRef> = arm0.body.stmts().collect();
    assert_eq!(stmts.len(), 1, "single-line `return` arm is one stmt");
    assert!(
        matches!(f.inst(stmts[0]).tag, TirTag::Return),
        "the stmt body is the Return inst, got {:?}",
        f.inst(stmts[0]).tag
    );
    let MatchBodyView::Expr(none_body) = view.arm(1).body else {
        panic!("arm 1 is an expr body, got {:?}", view.arm(1).body);
    };
    assert_eq!(f.inst(none_body).ty, pool.int());
}

#[test]
fn all_never_match_types_as_never() {
    // Every arm returns: the match expression itself is `never`, and a
    // void function whose body is that match satisfies return-flow
    // analysis through the ExprStmt-never rule.
    let src = "enum Opt:\n\tSome(value: int)\n\tNone\n\nfn f(o: Opt) -> int:\n\tmatch o:\n\t\tOpt.Some(v):\n\t\t\treturn v\n\t\tOpt.None:\n\t\t\treturn 0\n";
    let (tirs, pool) = run(src).expect("all-never match compiles");
    let f = tir_named(&tirs, &pool, "f");
    let m = match_stmt_operand(f, 0);
    assert_eq!(f.inst(m).ty, pool.never(), "all-never match is never");
}

#[test]
fn panic_expr_arm_joins_as_diverging_no_e0017() {
    // Reviewer case: an inline `panic(...)` expression arm is analyzed through the
    // never-tolerant entry point, so it joins as DIVERGING (the match
    // still types as int from the value arm) — not the E0017
    // value-position rejection a never-typed expression usually gets.
    let src = "fn main():\n\tx = match true:\n\t\ttrue: 1\n\t\tfalse: panic(\"no\")\n\tprint(x)\n";
    let (tirs, diags, pool) = run_with_errors(src);
    assert!(
        diags
            .iter()
            .all(|d| d.code != DiagCode::VoidValueInExpression),
        "no E0017 for the panic arm: {diags:?}"
    );
    assert_eq!(diags.len(), 0, "the mixed value/diverging match is clean");
    let main = tir_named(&tirs, &pool, "main");
    let m = match_in_var_init(main, 0);
    assert_eq!(
        main.inst(m).ty,
        pool.int(),
        "the value arm sets the join type"
    );
    let view = main.match_view(m);
    let MatchBodyView::Expr(diverging) = view.arm(1).body else {
        panic!("arm 1 is an expr body, got {:?}", view.arm(1).body);
    };
    assert_eq!(
        main.inst(diverging).ty,
        pool.never(),
        "the panic arm body stays never-typed on the wire"
    );
}

#[test]
fn all_diverging_panic_and_return_arms_type_never() {
    // Reviewer-round pair: `panic(...)` (expr arm) and `return` (statement arm)
    // in one match — both diverge, the join is `never`, no E0017.
    let src = "fn f(b: bool) -> int:\n\tmatch b:\n\t\ttrue: panic(\"a\")\n\t\tfalse: return 7\n\nfn main():\n\tprint(f(false))\n";
    let (tirs, diags, pool) = run_with_errors(src);
    assert!(
        diags
            .iter()
            .all(|d| d.code != DiagCode::VoidValueInExpression),
        "no E0017 for the panic arm: {diags:?}"
    );
    assert_eq!(diags.len(), 0, "the all-diverging match is clean");
    let f = tir_named(&tirs, &pool, "f");
    let m = match_stmt_operand(f, 0);
    assert_eq!(f.inst(m).ty, pool.never(), "all-diverging match is never");
}

#[test]
fn view_result_is_rejected_with_e0137() {
    // P2c: every value arm agreeing on a view type passes the E0134 join
    // but no merge shape exists for a view result — E0137 names the view
    // type and the deferral, and the match recovers error-typed (so
    // codegen's eval_inst_view can never see a Match).
    let src = "fn main():\n\ts = \"hello\"\n\tv = match true:\n\t\ttrue: s[0:2]\n\t\tfalse: s[2:4]\n\tprint(v)\n";
    let (tirs, diags, pool) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::MatchViewResult)
        .expect("E0137 must fire");
    for needle in ["'strview'", "not yet supported"] {
        assert!(
            diag.message.contains(needle),
            "message must contain {needle}, got {:?}",
            diag.message
        );
    }
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
    let main = tir_named(&tirs, &pool, "main");
    let m = match_in_var_init(main, 1);
    assert!(
        matches!(main.inst(m).tag, TirTag::Unreachable),
        "errored match recovers with Unreachable, got {:?}",
        main.inst(m).tag
    );
}

#[test]
fn view_result_with_e0134_offender_reports_only_e0134() {
    // A view candidate against a disagreeing value arm is a plain E0134
    // — piling the E0137 deferral on top would double-report.
    let src = "fn main():\n\ts = \"hello\"\n\tv = match true:\n\t\ttrue: s[0:2]\n\t\tfalse: 1\n\tprint(v)\n";
    let (_t, diags, _p) = run_with_errors(src);
    assert!(
        diags.iter().any(|d| d.code == DiagCode::ArmTypeMismatch),
        "E0134 must fire: {diags:?}"
    );
    assert!(
        diags.iter().all(|d| d.code != DiagCode::MatchViewResult),
        "no E0137 pile-on when a plain mismatch exists: {diags:?}"
    );
}

#[test]
fn arm_type_mismatch_is_e0134_naming_both_types() {
    // Arms yielding `int` and `str`: one E0134, primary span on the
    // offending arm's body, and the errored match recovers with the
    // error-typed sentinel.
    let src = "enum Opt:\n\tSome(value: int)\n\tNone\n\nfn main():\n\to = Opt.Some{value=5}\n\tx = match o:\n\t\tOpt.Some(v): v\n\t\tOpt.None: \"s\"\n";
    let (tirs, diags, pool) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::ArmTypeMismatch)
        .expect("E0134 must fire");
    for needle in ["'int'", "'str'"] {
        assert!(
            diag.message.contains(needle),
            "message must name both arm types ({needle}), got {:?}",
            diag.message
        );
    }
    let body = src.find("\"s\"").expect("the str arm body");
    assert_eq!(
        diag.span.start(),
        body,
        "E0134 anchors on the offending arm body, got {:?}",
        diag.span
    );
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
    // Recovery shape: an errored match is the Unreachable sentinel with
    // the error type — never a partially-typed Match.
    let main = tir_named(&tirs, &pool, "main");
    let m = match_in_var_init(main, 1);
    assert!(
        matches!(main.inst(m).tag, TirTag::Unreachable),
        "errored match recovers with Unreachable, got {:?}",
        main.inst(m).tag
    );
    assert_eq!(main.inst(m).ty, pool.error_type());
}

#[test]
fn arm_type_mismatch_notes_every_later_offender() {
    // Three value arms with three types: the primary squiggle lands on
    // the FIRST offending arm body, a NOTE carries each later one.
    let src =
        "fn main():\n\tn = 5\n\tx = match n:\n\t\t0: 1\n\t\t1: \"a\"\n\t\t2: true\n\t\t_: 0\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::ArmTypeMismatch)
        .expect("E0134 must fire");
    let first = src.find("\"a\"").expect("the str arm body");
    assert_eq!(diag.span.start(), first, "primary on the first offender");
    assert_eq!(diag.notes.len(), 1, "one note for the other arm: {diag:?}");
    let later = src.find("true\n").expect("the bool arm body");
    assert_eq!(
        diag.notes[0].span.map(|s| s.start()),
        Some(later),
        "the note anchors on the later offender's body, got {:?}",
        diag.notes[0]
    );
    assert!(
        diag.notes[0].message.contains("'bool'"),
        "the note names its arm's type, got {:?}",
        diag.notes[0].message
    );
    assert_eq!(diags.len(), 1, "exactly one diagnostic: {diags:?}");
}

#[test]
fn void_expr_arm_in_value_match_is_e0134() {
    // A void-producing expr arm (a bare `print` call) participates in
    // the join like any arm type: against value arms it mismatches.
    let src = "fn main():\n\tn = 5\n\tx = match n:\n\t\t0: 1\n\t\t_: print(\"x\")\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::ArmTypeMismatch)
        .expect("E0134 must fire for the void arm");
    for needle in ["'int'", "'void'"] {
        assert!(
            diag.message.contains(needle),
            "message must name {needle}, got {:?}",
            diag.message
        );
    }
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
}

#[test]
fn falling_statement_arm_in_value_match_is_e0135() {
    // `Some(v): x = 5` — a statement arm that falls through while the
    // other arm produces a value: E0135 at the falling arm, suggesting
    // the fix.
    let src = "enum Opt:\n\tSome(value: int)\n\tNone\n\nfn main():\n\tmut x = 0\n\to = Opt.Some{value=5}\n\tmatch o:\n\t\tOpt.Some(v): x = 5\n\t\tOpt.None: 0\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::ArmNoValue)
        .expect("E0135 must fire");
    let body = src.find("x = 5").expect("the falling arm body");
    assert_eq!(
        diag.span.start(),
        body,
        "E0135 anchors on the falling arm body, got {:?}",
        diag.span
    );
    assert!(
        diag.notes
            .iter()
            .any(|n| n.message.contains("value") && n.message.contains("return")),
        "the help must suggest ending with a value or `return`, got {:?}",
        diag.notes
    );
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
}

#[test]
fn all_void_match_allows_fallthrough_and_types_void() {
    // Every arm produces no value (void expr arm / falling statement
    // arm): no E0135, and the match types as void. Both arm shapes —
    // the single-line void call and a block arm that falls through.
    let src = "fn main():\n\tmatch \"a\":\n\t\t_: print(\"x\")\n";
    let (tirs, pool) = run(src).expect("void match compiles");
    let main = tir_named(&tirs, &pool, "main");
    let m = match_stmt_operand(main, 0);
    assert_eq!(main.inst(m).ty, pool.void(), "void arms join to void");

    let src = "fn main():\n\tmatch \"a\":\n\t\t_:\n\t\t\tprint(\"x\")\n";
    let (tirs, pool) = run(src).expect("fallthrough void match compiles");
    let main = tir_named(&tirs, &pool, "main");
    let m = match_stmt_operand(main, 0);
    assert_eq!(
        main.inst(m).ty,
        pool.void(),
        "a falling statement arm is legal when no arm produces a value"
    );
}

#[test]
fn borrowed_scrutinee_binding_non_copy_field_is_e0133() {
    // `match &o:` (M8.2 borrow) with `Some(v):` binding the str payload:
    // moving a non-Copy field out of a borrow is E0133 — names the
    // binding and its type, with the workaround help note.
    let src = "enum Opt:\n\tSome(value: str)\n\tNone\n\nfn main():\n\to = Opt.Some{value=\"hi\"}\n\tmatch &o:\n\t\tOpt.Some(v): print(v)\n\t\tOpt.None: print(\"none\")\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::MoveOutOfBorrowedScrutinee)
        .expect("E0133 must fire");
    for needle in ["'v'", "'str'"] {
        assert!(
            diag.message.contains(needle),
            "message must name the binding and its type ({needle}), got {:?}",
            diag.message
        );
    }
    assert!(
        !diag.notes.is_empty(),
        "E0133 must carry the workaround help note: {diag:?}"
    );
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
}

#[test]
fn owned_scrutinee_binds_non_copy_field_and_emits_owner_token() {
    // The same match without `&` is fine: the str binding is a by-move
    // bind — exactly one FieldAccess owner token immediately before the
    // Match, keyed on the binding (never evaluated by codegen).
    let src = "enum Opt:\n\tSome(value: str)\n\tNone\n\nfn main():\n\to = Opt.Some{value=\"hi\"}\n\tmatch o:\n\t\tOpt.Some(v): print(v)\n\t\tOpt.None: print(\"none\")\n";
    let (tirs, pool) = run(src).expect("owned scrutinee binds by move");
    let main = tir_named(&tirs, &pool, "main");
    let m = match_stmt_operand(main, 1);
    assert_eq!(main.inst(m).ty, pool.void());
    let owners = main.match_by_move_owner_refs(m, &pool);
    assert_eq!(owners.len(), 1, "one owner token for the str binding");
    let token = owners[0];
    assert_eq!(token.index(), m.index() - 1, "token immediately precedes");
    assert!(
        matches!(main.inst(token).tag, TirTag::FieldAccess),
        "owner token is a FieldAccess, got {:?}",
        main.inst(token).tag
    );
    assert_eq!(
        main.inst(token).ty,
        pool.str_(),
        "token carries the field type"
    );
}

#[test]
fn copy_bindings_skip_owner_tokens() {
    // Copy-typed payload binds copy out of the scrutinee — no owner
    // tokens at all, even on a borrowed scrutinee.
    let src = "enum Opt:\n\tSome(value: int)\n\tNone\n\nfn main():\n\to = Opt.Some{value=5}\n\tx = match &o:\n\t\tOpt.Some(v): v\n\t\tOpt.None: 0\n";
    let (tirs, pool) = run(src).expect("Copy payload on a borrow is fine");
    let main = tir_named(&tirs, &pool, "main");
    let m = match_in_var_init(main, 1);
    assert_eq!(main.inst(m).ty, pool.int());
    assert!(
        main.match_by_move_owner_refs(m, &pool).is_empty(),
        "Copy bindings emit no owner tokens"
    );
}

#[test]
fn wildcard_arm_on_borrow_suppresses_no_binds() {
    // `Some(_):` on a borrowed scrutinee binds nothing — no E0133, and
    // no owner tokens or bindings on the wire.
    let src = "enum Opt:\n\tSome(value: str)\n\tNone\n\nfn main():\n\to = Opt.Some{value=\"hi\"}\n\tmatch &o:\n\t\tOpt.Some(_): print(\"some\")\n\t\tOpt.None: print(\"none\")\n";
    let (tirs, pool) = run(src).expect("wildcard arm on a borrow is fine");
    let main = tir_named(&tirs, &pool, "main");
    let m = match_stmt_operand(main, 1);
    assert_eq!(main.inst(m).ty, pool.void());
    assert_eq!(main.match_view(m).arm(0).binding_count(), 0);
}

#[test]
fn borrowed_param_scrutinee_binding_non_copy_is_e0133() {
    // The other borrowed shape: a plain (borrow-mode) parameter used as
    // the scrutinee. Binding its non-Copy payload is E0133 without any
    // `&` marker. (Void function: an errored match's error-typed
    // recovery does not satisfy return-flow analysis, so a value-
    // returning fn would cascade a spurious MissingReturn on top.)
    let src = "enum Opt:\n\tSome(value: str)\n\tNone\n\nfn f(o: Opt):\n\tmatch o:\n\t\tOpt.Some(v): print(v)\n\t\tOpt.None: print(\"\")\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::MoveOutOfBorrowedScrutinee)
        .expect("E0133 must fire for a borrowed param scrutinee");
    assert!(
        diag.message.contains("'v'"),
        "message names the binding, got {:?}",
        diag.message
    );
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
}

#[test]
fn borrowed_param_field_projection_scrutinee_binding_non_copy_is_e0133() {
    // `match w.inner:` — a FieldAccess chain rooted at a borrow-mode
    // parameter. The projection borrows with its root, so binding the
    // non-Copy payload (`Msg` carries a str field) is E0133 exactly
    // like a bare `w` scrutinee.
    let src = "struct Msg:\n\ttext: str\n\nenum Note:\n\tInfo(Msg)\n\tBlank\n\nstruct Wrapper:\n\tinner: Note\n\nfn f(w: Wrapper):\n\tmatch w.inner:\n\t\tNote.Info(m): print(m.text)\n\t\tNote.Blank: print(\"\")\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::MoveOutOfBorrowedScrutinee)
        .expect("E0133 must fire for a borrowed-param projection scrutinee");
    assert!(
        diag.message.contains("'m'"),
        "message names the binding, got {:?}",
        diag.message
    );
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
}

#[test]
fn move_param_field_projection_scrutinee_binds_non_copy_freely() {
    // A `move` parameter is owned, so the same projection match is
    // legal — the borrowed-root walk must not over-reject.
    let src = "struct Msg:\n\ttext: str\n\nenum Note:\n\tInfo(Msg)\n\tBlank\n\nstruct Wrapper:\n\tinner: Note\n\nfn f(move w: Wrapper):\n\tmatch w.inner:\n\t\tNote.Info(m): print(m.text)\n\t\tNote.Blank: print(\"\")\n";
    run(src).expect("move param projection is an owned scrutinee");
}

#[test]
fn move_param_scrutinee_binds_non_copy_freely() {
    // A `move` parameter is owned: the same binding is legal.
    let src = "enum Opt:\n\tSome(value: str)\n\tNone\n\nfn f(move o: Opt) -> str:\n\tmatch o:\n\t\tOpt.Some(v): return v\n\t\tOpt.None: return \"\"\n";
    run(src).expect("move param is an owned scrutinee");
}

#[test]
fn e0133_fires_per_arm_not_per_match() {
    // Review-focus pin: one match where arm A binds a Copy field (ok)
    // and arm B binds a str field (E0133) — exactly one E0133, and the
    // Copy arm is untouched.
    let src = "enum Mix:\n\tA(n: int)\n\tB(s: str)\n\nfn main():\n\tm = Mix.A{n=1}\n\tmatch &m:\n\t\tMix.A(n): print(n)\n\t\tMix.B(s): print(s)\n";
    let (_t, diags, _p) = run_with_errors(src);
    assert_eq!(
        count_code(&diags, DiagCode::MoveOutOfBorrowedScrutinee),
        1,
        "exactly one E0133 for the str-binding arm: {diags:?}"
    );
    assert_eq!(diags.len(), 1, "no other diagnostics: {diags:?}");
}

#[test]
fn arm_bindings_do_not_leak_past_the_match() {
    // `v` is visible inside its arm and unresolvable after the match —
    // the arm scope dies with the arm (E0010-style unresolved name).
    let src = "enum Opt:\n\tSome(value: int)\n\tNone\n\nfn main():\n\to = Opt.Some{value=5}\n\tx = match o:\n\t\tOpt.Some(v): v + 1\n\t\tOpt.None: 0\n\tprint(v)\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::UndefinedVariable)
        .expect("the post-match reference must not resolve");
    let use_site = src.rfind("print(v)").expect("the post-match use") + "print(".len();
    assert_eq!(
        diag.span.start(),
        use_site,
        "the unresolved use anchors on `v`, got {:?}",
        diag.span
    );
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
}

#[test]
fn same_binding_name_in_different_arms_is_allowed() {
    let src = "enum Two:\n\tA(v: int)\n\tB(v: int)\n\nfn main():\n\tt = Two.A{v=1}\n\tx = match t:\n\t\tTwo.A(v): v\n\t\tTwo.B(v): v * 2\n";
    let (tirs, pool) = run(src).expect("per-arm scopes never collide");
    let main = tir_named(&tirs, &pool, "main");
    let m = match_in_var_init(main, 1);
    let view = main.match_view(m);
    let v = pool.find_str("v").expect("`v` interned");
    for arm in view.arms() {
        assert_eq!(
            arm.bindings().collect::<Vec<_>>(),
            [MatchBinding {
                field_index: 0,
                name: v,
                ty: pool.int(),
            }]
        );
    }
}

#[test]
fn duplicate_binding_name_within_one_arm_is_rejected() {
    // `P.Both(v, v):` — the same name twice in ONE arm's pattern hits
    // the duplicate-binding diagnostic (the scope-insertion check), and
    // the errored match recovers.
    let src = "enum P:\n\tBoth(a: int, b: int)\n\nfn main():\n\tp = P.Both{a=1, b=2}\n\tx = match p:\n\t\tP.Both(v, v): v\n\t\t_: 0\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::DuplicateDeclaration)
        .expect("the duplicate binding must fire");
    assert!(
        diag.message.contains("'v'"),
        "message names the duplicated binding, got {:?}",
        diag.message
    );
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
}

#[test]
fn owner_tokens_follow_arm_then_binding_order() {
    // Two by-move bindings across two arms: the tokens land immediately
    // before the Match in arm order, then binding order within each arm
    // — the exact layout `match_by_move_owner_refs` recovers.
    let src = "enum E:\n\tPair(a: str, b: str)\n\tSolo(s: str)\n\nfn main():\n\te = E.Pair{a=\"a\", b=\"b\"}\n\tx = match e:\n\t\tE.Pair(a, b): 0\n\t\tE.Solo(s): 1\n";
    let (tirs, pool) = run(src).expect("multi-bind match compiles");
    let main = tir_named(&tirs, &pool, "main");
    let m = match_in_var_init(main, 1);
    let owners = main.match_by_move_owner_refs(m, &pool);
    assert_eq!(owners.len(), 3, "one token per by-move binding");
    for (i, token) in owners.iter().enumerate() {
        assert_eq!(
            token.index(),
            m.index() - 3 + i,
            "token {i} immediately precedes the Match"
        );
        assert!(
            matches!(main.inst(*token).tag, TirTag::FieldAccess),
            "token {i} is a FieldAccess, got {:?}",
            main.inst(*token).tag
        );
        assert_eq!(
            main.inst(*token).ty,
            pool.str_(),
            "token {i} carries the field type"
        );
    }
    // The wire order mirrors it: arm 0's bindings (a, b) then arm 1's (s).
    let a = pool.find_str("a").unwrap();
    let b = pool.find_str("b").unwrap();
    let s = pool.find_str("s").unwrap();
    let view = main.match_view(m);
    assert_eq!(
        view.arm(0).bindings().collect::<Vec<_>>(),
        [
            MatchBinding {
                field_index: 0,
                name: a,
                ty: pool.str_(),
            },
            MatchBinding {
                field_index: 1,
                name: b,
                ty: pool.str_(),
            },
        ]
    );
    assert_eq!(
        view.arm(1).bindings().collect::<Vec<_>>(),
        [MatchBinding {
            field_index: 0,
            name: s,
            ty: pool.str_(),
        }]
    );
}

#[test]
fn literal_discriminants_round_trip_folded_values() {
    // Negative ints arrive pre-folded; str/bool literals keep their
    // folded forms; the wildcard arm is the catch-all encoding.
    let src =
        "fn main():\n\tn = 5\n\tx = match n:\n\t\t0: \"zero\"\n\t\t-1: \"neg\"\n\t\t_: \"other\"\n";
    let (tirs, pool) = run(src).expect("literal arms type");
    let main = tir_named(&tirs, &pool, "main");
    let m = match_in_var_init(main, 1);
    assert_eq!(main.inst(m).ty, pool.str_());
    let view = main.match_view(m);
    assert_eq!(view.arm(0).discriminant, Discriminant::Int(0));
    assert_eq!(view.arm(1).discriminant, Discriminant::Int(-1));
    assert_eq!(view.arm(2).discriminant, Discriminant::Wildcard);

    let src = "fn main():\n\tb = true\n\tx = match b:\n\t\ttrue: 1\n\t\tfalse: 0\n";
    let (tirs, pool) = run(src).expect("bool arms type");
    let main = tir_named(&tirs, &pool, "main");
    let m = match_in_var_init(main, 1);
    let view = main.match_view(m);
    assert_eq!(view.arm(0).discriminant, Discriminant::Bool(true));
    assert_eq!(view.arm(1).discriminant, Discriminant::Bool(false));

    let src = "fn main():\n\ts = \"a\"\n\tx = match s:\n\t\t\"quit\": 1\n\t\t_: 0\n";
    let (tirs, pool) = run(src).expect("str arms type");
    let main = tir_named(&tirs, &pool, "main");
    let m = match_in_var_init(main, 1);
    let view = main.match_view(m);
    let quit = pool.find_str("quit").expect("`quit` interned");
    assert_eq!(view.arm(0).discriminant, Discriminant::Str(quit));
}

#[test]
fn anon_arm_encodes_placeholder_wildcard_discriminant() {
    // Anon patterns carry no M12 discriminant (structural lowering is
    // M12.5): the arm binds its targets and rides the wire with the
    // Wildcard placeholder encoding, documented at the emission site.
    // A clean M12 anon match necessarily has a real `_:` arm (E0129
    // otherwise).
    let src = "fn main():\n\tp = {x=1, y=2}\n\tx = match p:\n\t\t{x=px, y=_}: px\n\t\t_: 0\n";
    let (tirs, pool) = run(src).expect("anon-arm match compiles");
    let main = tir_named(&tirs, &pool, "main");
    let m = match_in_var_init(main, 1);
    assert_eq!(main.inst(m).ty, pool.int());
    let view = main.match_view(m);
    let px = pool.find_str("px").expect("`px` interned");
    assert_eq!(
        view.arm(0).discriminant,
        Discriminant::Wildcard,
        "anon arms encode the Wildcard placeholder"
    );
    assert_eq!(
        view.arm(0).bindings().collect::<Vec<_>>(),
        [MatchBinding {
            field_index: 0,
            name: px,
            ty: pool.int(),
        }]
    );
    assert_eq!(view.arm(1).discriminant, Discriminant::Wildcard);
    assert_eq!(view.arm(1).binding_count(), 0);
}

#[test]
fn statement_arm_block_bodies_analyze_in_the_arm_scope() {
    // A block arm's statements see the arm bindings; declarations inside
    // the arm do not leak either.
    let src = "enum Opt:\n\tSome(value: int)\n\tNone\n\nfn f(o: Opt) -> int:\n\tmatch o:\n\t\tOpt.Some(v):\n\t\t\tw = v + 1\n\t\t\treturn w\n\t\tOpt.None:\n\t\t\treturn 0\n";
    run(src).expect("block arm bodies analyze against the arm bindings");

    // `w` is arm-scoped: using it in the sibling arm is unresolved.
    let src = "enum Opt:\n\tSome(value: int)\n\tNone\n\nfn f(o: Opt) -> int:\n\tmatch o:\n\t\tOpt.Some(v):\n\t\t\tw = v + 1\n\t\t\treturn w\n\t\tOpt.None:\n\t\t\treturn w\n";
    let (_t, diags, _p) = run_with_errors(src);
    assert!(
        any_code(&diags, DiagCode::UndefinedVariable),
        "the sibling arm must not see `w`: {diags:?}"
    );
    assert_eq!(
        count_code(&diags, DiagCode::UndefinedVariable),
        1,
        "exactly one unresolved use: {diags:?}"
    );
}

#[test]
fn nested_matches_keep_their_owner_tokens_separate() {
    // An inner match (with its own by-move binding) inside an outer
    // match's arm body: each Match's count-back recovery lands on its
    // own tokens — the inner emission is fully inside the outer arm's
    // body region.
    let src = "enum Opt:\n\tSome(value: str)\n\tNone\n\nfn main():\n\ta = Opt.Some{value=\"a\"}\n\tb = Opt.Some{value=\"b\"}\n\tmatch a:\n\t\tOpt.Some(v):\n\t\t\ty = match b:\n\t\t\t\tOpt.Some(w): w\n\t\t\t\tOpt.None: \"\"\n\t\t\tprint(y)\n\t\t\tprint(v)\n\t\tOpt.None:\n\t\t\tprint(\"none\")\n";
    let (tirs, pool) = run(src).expect("nested matches compile");
    let main = tir_named(&tirs, &pool, "main");
    let outer = match_stmt_operand(main, 2);
    let view = main.match_view(outer);
    assert_eq!(view.len(), 2);
    let outer_owners = main.match_by_move_owner_refs(outer, &pool);
    assert_eq!(outer_owners.len(), 1, "outer match: one by-move bind (v)");
    assert_eq!(
        outer_owners[0].index(),
        outer.index() - 1,
        "the outer token immediately precedes the outer Match"
    );
    // Reach the inner match through the outer arm's stmt list.
    let arm0_stmts: Vec<TirRef> = view.arm(0).body.stmts().collect();
    let inner = main.var_decl_view(arm0_stmts[0]).initializer;
    assert!(
        matches!(main.inst(inner).tag, TirTag::Match),
        "the arm's first stmt binds the inner match, got {:?}",
        main.inst(inner).tag
    );
    let inner_owners = main.match_by_move_owner_refs(inner, &pool);
    assert_eq!(inner_owners.len(), 1, "inner match: one by-move bind (w)");
    assert_eq!(
        inner_owners[0].index(),
        inner.index() - 1,
        "the inner token immediately precedes the inner Match"
    );
    // The inner Match and its token sit before the outer Match in the
    // arena (emitted during the outer arm's body analysis).
    assert!(inner.index() < outer.index());
}

#[test]
fn diverging_inner_match_makes_the_statement_arm_never() {
    // A statement arm whose body ends in an all-returning inner match
    // diverges: the arm is `never`, no E0135, and an all-never outer
    // match types as never.
    let src = "enum Opt:\n\tSome(value: int)\n\tNone\n\nfn f(o: Opt) -> int:\n\tmatch o:\n\t\tOpt.Some(v):\n\t\t\tmatch v:\n\t\t\t\t0: return 0\n\t\t\t\t_: return 1\n\t\tOpt.None:\n\t\t\treturn 2\n";
    let (tirs, pool) = run(src).expect("diverging inner match compiles");
    let f = tir_named(&tirs, &pool, "f");
    let outer = match_stmt_operand(f, 0);
    assert_eq!(
        f.inst(outer).ty,
        pool.never(),
        "every arm diverges (inner all-return match included)"
    );
}

// ---------- R12/R13 fix coverage: anon-arm guards, jump-terminated arms ----------

#[test]
fn anon_arm_literal_guards_reach_the_typed_wire() {
    // R12: `{x=0, y=py}:` — the literal constraint rides the arm's
    // guard list; the `y` bind rides the bindings. Both decode from the
    // typed Match view.
    let src = "fn main():\n\tp = {x=1, y=2}\n\tr = match p:\n\t\t{x=0, y=py}: py\n\t\t_: 0\n";
    let (tirs, pool) = run(src).expect("guarded anon arm compiles");
    let main = tir_named(&tirs, &pool, "main");
    let m = match_in_var_init(main, 1);
    assert_eq!(main.inst(m).ty, pool.int());
    let view = main.match_view(m);
    let arm0 = view.arm(0);
    assert_eq!(
        arm0.discriminant,
        Discriminant::Wildcard,
        "coverage placeholder kept orthogonal to the guards"
    );
    assert_eq!(
        arm0.guards().collect::<Vec<_>>(),
        [(0, LitPattern::Int(0))],
        "the literal field constraint reaches the wire"
    );
    let py = pool.find_str("py").expect("`py` interned");
    assert_eq!(
        arm0.bindings().collect::<Vec<_>>(),
        [MatchBinding {
            field_index: 1,
            name: py,
            ty: pool.int(),
        }],
        "the `y` bind coexists with the guard"
    );
    // The wildcard fallback arm carries no guards.
    assert_eq!(view.arm(1).guard_count(), 0);
    assert!(view.arm(1).guards().next().is_none());
}

#[test]
fn positional_anon_arm_guards_reach_the_typed_wire() {
    // `(0, _):` — tuple sugar; element 0 is the literal constraint.
    let src = "fn main():\n\tp = {x=1, y=2}\n\tr = match p:\n\t\t(0, _): 1\n\t\t_: 0\n";
    let (tirs, pool) = run(src).expect("positional guarded arm compiles");
    let main = tir_named(&tirs, &pool, "main");
    let m = match_in_var_init(main, 1);
    let view = main.match_view(m);
    let arm0 = view.arm(0);
    assert_eq!(arm0.guards().collect::<Vec<_>>(), [(0, LitPattern::Int(0))]);
    assert_eq!(arm0.binding_count(), 0);
}

#[test]
fn non_anon_arms_carry_empty_guard_lists() {
    // Variant and literal arms encode their matching entirely in the
    // discriminant — their guard lists are empty.
    let src = "enum Opt:\n\tSome(value: int)\n\tNone\n\nfn main():\n\to = Opt.Some{value=5}\n\tr = match o:\n\t\tOpt.Some(v): v\n\t\tOpt.None: 0\n";
    let (tirs, pool) = run(src).expect("variant arms compile");
    let main = tir_named(&tirs, &pool, "main");
    let m = match_in_var_init(main, 1);
    let view = main.match_view(m);
    assert_eq!(view.arm(0).guard_count(), 0);
    assert_eq!(view.arm(1).guard_count(), 0);

    let src = "fn main():\n\tn = 5\n\tr = match n:\n\t\t0: 1\n\t\t_: 0\n";
    let (tirs, pool) = run(src).expect("literal arms compile");
    let main = tir_named(&tirs, &pool, "main");
    let m = match_in_var_init(main, 1);
    let view = main.match_view(m);
    assert_eq!(view.arm(0).guard_count(), 0);
    assert_eq!(view.arm(1).guard_count(), 0);
}

#[test]
fn break_arm_terminates_the_value_match_arm() {
    // R13: inside a loop, `Some(v): break` is a terminating arm — the
    // match types as int (never-compatible at the join), no E0135.
    let src = "enum Opt:\n\tSome(value: int)\n\tNone\n\nfn main():\n\to = Opt.Some{value=5}\n\tmut x = 0\n\twhile true:\n\t\tx = match o:\n\t\t\tOpt.Some(v): break\n\t\t\tOpt.None: 0\n\t\tx += 1\n";
    let (tirs, pool) = run(src).expect("break arm is terminating");
    let main = tir_named(&tirs, &pool, "main");
    let while_stmt = stmt_at(main, 2);
    let while_body = main.while_loop_view(while_stmt).body;
    let assign = while_body.at(0);
    let m = main.assign_view(assign).value;
    assert_eq!(main.inst(m).ty, pool.int(), "break arm joins freely");
    let view = main.match_view(m);
    let arm0_stmts: Vec<TirRef> = view.arm(0).body.stmts().collect();
    assert!(
        matches!(main.inst(arm0_stmts[0]).tag, TirTag::Break),
        "the break arm body is the Break stmt, got {:?}",
        main.inst(arm0_stmts[0]).tag
    );
}

#[test]
fn continue_arm_terminates_the_value_match_arm() {
    let src = "enum Opt:\n\tSome(value: int)\n\tNone\n\nfn main():\n\to = Opt.Some{value=5}\n\tmut x = 0\n\twhile true:\n\t\tx = match o:\n\t\t\tOpt.Some(v): continue\n\t\t\tOpt.None: 0\n\t\tx += 1\n";
    let (tirs, pool) = run(src).expect("continue arm is terminating");
    let main = tir_named(&tirs, &pool, "main");
    let while_stmt = stmt_at(main, 2);
    let while_body = main.while_loop_view(while_stmt).body;
    let m = main.assign_view(while_body.at(0)).value;
    assert_eq!(main.inst(m).ty, pool.int());
}

#[test]
fn fallthrough_assignment_arm_in_loop_is_still_e0135() {
    // R13's flip side: a statement arm that can REALLY fall through is
    // still E0135 in a value match, loop or not.
    let src = "enum Opt:\n\tSome(value: int)\n\tNone\n\nfn main():\n\to = Opt.Some{value=5}\n\tmut x = 0\n\twhile true:\n\t\tx = match o:\n\t\t\tOpt.Some(v): x = 5\n\t\t\tOpt.None: 0\n\t\tx += 1\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::ArmNoValue)
        .expect("E0135 must still fire for the falling arm");
    assert!(
        diag.message.contains("'int'"),
        "message names the candidate type, got {:?}",
        diag.message
    );
    assert_eq!(
        count_code(&diags, DiagCode::ArmNoValue),
        1,
        "exactly one E0135: {diags:?}"
    );
}
