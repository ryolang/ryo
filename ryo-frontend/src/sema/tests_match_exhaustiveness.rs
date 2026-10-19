//! Match exhaustiveness (E0129) and redundant-arm (W0005) tests.
//! Split out of `tests_match.rs` to stay under the file-length limit.
//! `run_buffered` below is this section's harness: it surfaces
//! sema's buffered warnings even when errors exist in the same unit
//! (the plain harness drops buffered warnings next to errors, but
//! combined E0129+W0005 inputs need both sides visible).

use super::tests::*;
use super::tests_match::match_stmt_operand;
use super::*;
use crate::astgen;
use crate::lexer::lex;
use crate::parser::{ParseState, program_parser};
use chumsky::span::Span as _;
use chumsky::{Parser, input::Input};
use ryo_core::tir::TirTag;
use std::path::Path;

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
