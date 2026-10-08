//! Warning-buffer gating for sema. Warnings (W0002/W0003 case A) are
//! buffered per unit and flushed only when no error fired — a warning
//! from an early decl must never pile onto an error discovered while
//! analyzing a later one.

use super::{count_code, run_with_errors};
use ryo_core::diag::DiagCode;

#[test]
fn w0002_suppressed_when_a_later_decl_errors() {
    // Control half: a clean unit flushes the buffered W0002.
    let (_tirs, diags, _pool) = run_with_errors("fn a(move x: int) -> int:\n\treturn x\n");
    assert_eq!(
        count_code(&diags, DiagCode::RedundantMove),
        1,
        "clean unit must show W0002; got {:?}",
        diags
    );

    // Erroring half: `b`'s type error is discovered after `a`'s
    // warning was buffered; the warning must be dropped, not rendered
    // next to the error.
    let (_tirs, diags, _pool) = run_with_errors(
        "fn a(move x: int) -> int:\n\treturn x\n\nfn b() -> int:\n\treturn \"s\" + 1\n",
    );
    assert!(
        diags
            .iter()
            .any(|d| d.severity == ryo_core::diag::Severity::Error),
        "the type error must survive: {diags:?}"
    );
    assert_eq!(
        count_code(&diags, DiagCode::RedundantMove),
        0,
        "W0002 from an earlier decl must not survive a later error: {diags:?}"
    );
}
