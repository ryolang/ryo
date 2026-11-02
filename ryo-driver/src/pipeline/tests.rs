use super::*;
use ryo_core::diag::{DiagCode, DiagSink};
use ryo_frontend::{astgen, sema};

#[test]
fn parse_broken_only_return_does_not_cascade_missing_return() {
    // The parser recovers at the statement boundary, so the body
    // genuinely ends without a return — but that is the parse
    // error's fault, not the signature's. Exactly the parse
    // diagnostic may surface; a stacked E0036 would point the user
    // at the wrong place.
    let src = "fn f() -> int:\n\treturn 1 +\n";
    let mut pool = InternPool::new();
    let (program, parse_diags) = match ryo_frontend::parse::parse_source(src, &mut pool) {
        Ok(v) => v,
        Err(diags) => panic!("recovery should yield a partial program: {diags:?}"),
    };
    let mut sink = DiagSink::new();
    for d in parse_diags {
        sink.emit(d);
    }
    let uir = astgen::generate(&program, &mut pool, &mut sink);
    let _tirs = sema::analyze(&uir, &mut pool, &mut sink, src, Path::new("<test>"));
    let diags = sink.into_diags();
    assert_eq!(
        diags.len(),
        1,
        "exactly the parse diagnostic may surface: {diags:?}"
    );
    assert_eq!(diags[0].code, DiagCode::ParseError);
}

#[test]
fn sema_warnings_suppressed_by_ownership_errors() {
    // Cross-stage ordering hole: fn a's W0002 (redundant move on a
    // Copy param) is a sema-stage warning, and fn main's
    // use-after-move error only fires during the ownership pass —
    // after sema has already flushed. The deferred flush must drop
    // the warning: one failing stage silences warnings unit-wide.
    let src =
        "fn a(move x: int) -> int:\n\treturn x\n\nfn main():\n\ts = \"abc\"\n\tt = s\n\tprint(s)\n";
    let mut pool = InternPool::new();
    let (program, parse_diags) = match ryo_frontend::parse::parse_source(src, &mut pool) {
        Ok(v) => v,
        Err(diags) => panic!("source should parse cleanly: {diags:?}"),
    };
    assert!(parse_diags.is_empty());
    let mut sink = DiagSink::new();
    let uir = astgen::generate(&program, &mut pool, &mut sink);
    let (tirs, sema_warnings) =
        sema::analyze_buffered(&uir, &mut pool, &mut sink, src, Path::new("<test>"));
    let flows: Vec<ryo_core::flow::Flow> = tirs
        .iter()
        .map(|t| ryo_core::flow::Flow::build(t, &pool))
        .collect();
    let _sidecar = ryo_frontend::ownership::check(&tirs, &flows, &pool, &mut sink);
    if !sink.has_errors() {
        for d in sema_warnings {
            sink.emit(d);
        }
    }
    let diags = sink.into_diags();
    assert!(
        diags.iter().any(|d| d.code == DiagCode::UseAfterMove),
        "ownership error must survive: {diags:?}"
    );
    assert!(
        !diags.iter().any(|d| d.code == DiagCode::RedundantMove),
        "sema warning must not pile onto a later ownership error: {diags:?}"
    );
}

#[test]
fn parse_and_sema_diagnostics_co_surface() {
    // R9 + R10: a syntax error in one statement must not suppress
    // semantic diagnostics elsewhere in the file — both surface
    // in a single run.
    let mut pool = InternPool::new();
    let input = "fn main():\n\tx = = 1\n\ty: int = \"hi\"\n";
    let (program, parse_diags) = match ryo_frontend::parse::parse_source(input, &mut pool) {
        Ok(v) => v,
        Err(diags) => panic!("recovery should succeed: {diags:?}"),
    };
    let err = lower_and_analyze(
        &program,
        &mut pool,
        input,
        "<test>",
        Path::new("<test>"),
        parse_diags,
        false,
    )
    .expect_err("both a parse error and a type error must fail the compile");
    let CompilerError::Diagnostics(diags) = err else {
        panic!("expected Diagnostics error");
    };
    assert!(
        diags.iter().any(|d| d.code == DiagCode::ParseError),
        "parse diagnostic must survive: {diags:?}"
    );
    assert!(
        diags.iter().any(|d| d.code == DiagCode::TypeMismatch),
        "sema diagnostic must co-surface: {diags:?}"
    );
}
