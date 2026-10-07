//! Error-path lint gating (I-203). Warnings must never pile onto a
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
