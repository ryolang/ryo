//! Error-path lint gating. Warnings must never pile onto a
//! failing compilation unit — the primary error is the only message
//! the user can act on.

use super::super::*;
use super::common::*;

#[test]
fn w0001_suppressed_when_an_error_already_fired() {
    // pair's only use (pair.2) fails with the unknown-field error;
    // the dead-store W0001 on that error path is noise and must not
    // fire.
    let diags = check_src("fn main():\n\tpair = (17, \"alice\")\n\tprint(pair.2)\n");
    assert!(
        diags.iter().any(|d| d.code == DiagCode::UnknownField),
        "primary error must survive: {diags:?}"
    );
    assert!(
        !diags.iter().any(|d| d.code == DiagCode::DeadStore),
        "W0001 must not pile onto the error path: {diags:?}"
    );
}

#[test]
fn w0001_suppressed_when_ownership_pass_emits_the_error() {
    // `t = s` moves s, so the use-after-move error fires DURING the
    // ownership walk — after an entry-time quiet capture would have
    // run. The dead store `dead` must not still warn W0001.
    let diags = check_src("fn main():\n\ts = \"abc\"\n\tt = s\n\tprint(s)\n\tdead = \"x\"\n");
    assert!(
        diags.iter().any(|d| d.code == DiagCode::UseAfterMove),
        "ownership error must survive: {diags:?}"
    );
    assert!(
        !diags.iter().any(|d| d.code == DiagCode::DeadStore),
        "W0001 must not pile onto the ownership error: {diags:?}"
    );
}
