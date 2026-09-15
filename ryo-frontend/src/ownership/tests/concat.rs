use super::super::*;
use super::common::*;

/// Find the single `Assign` statement in `main`'s body.
fn single_assign(tir: &ryo_core::tir::Tir) -> TirRef {
    tir.body_stmts()
        .iter()
        .find(|&&s| tir.inst(s).tag == TirTag::Assign)
        .copied()
        .expect("assign stmt")
}

#[test]
fn consuming_concat_reassign_recorded() {
    // s = s + "b": the concat's lhs Var resolves to the dying owner,
    // the rhs is a different owner — the Assign is selected for
    // in-place append.
    let src = "fn main():\n\tmut s: str = \"a\"\n\ts = s + \"b\"\n\tprint(s)\n";
    let (diags, sidecar, tirs, _pool) = check_src_full(src);
    assert!(
        !diags
            .iter()
            .any(|d| d.severity == ryo_core::diag::Severity::Error),
        "no errors expected; got: {diags:?}"
    );
    let tir = &tirs[0];
    let assign = single_assign(tir);
    let concat = tir.assign_view(assign).value;
    assert_eq!(tir.inst(concat).tag, TirTag::StrConcat);

    let entries: Vec<(usize, TirRef)> = sidecar.functions[0]
        .consumed_concat_lhs
        .iter()
        .enumerate()
        .filter_map(|(i, e)| e.map(|v| (i, v)))
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "exactly one consumed_concat_lhs entry; got: {entries:?}"
    );
    assert_eq!(
        entries[0],
        (assign.index(), concat),
        "entry must be keyed at the Assign and point at the StrConcat"
    );
    // free_on_reassign still records the old owner — codegen (not the
    // ownership pass) is responsible for skipping that free when it
    // consumes the buffer in place.
    assert!(
        sidecar.functions[0].free_on_reassign[assign.index()].is_some(),
        "free_on_reassign must still be scheduled; got: {:?}",
        sidecar.functions[0].free_on_reassign
    );
}

#[test]
fn self_alias_concat_reassign_not_recorded() {
    // s = s + s: the rhs aliases the dying owner, so in-place append
    // would read the buffer being overwritten — the Assign must keep
    // the allocating path.
    let src = "fn main():\n\tmut s: str = \"a\"\n\ts = s + s\n\tprint(s)\n";
    let (diags, sidecar, tirs, _pool) = check_src_full(src);
    assert!(
        !diags
            .iter()
            .any(|d| d.severity == ryo_core::diag::Severity::Error),
        "no errors expected; got: {diags:?}"
    );
    let tir = &tirs[0];
    let assign = single_assign(tir);
    assert_eq!(
        tir.inst(tir.assign_view(assign).value).tag,
        TirTag::StrConcat
    );
    assert!(
        sidecar.functions[0]
            .consumed_concat_lhs
            .iter()
            .all(Option::is_none),
        "self-aliasing concat must not be selected; got: {:?}",
        sidecar.functions[0].consumed_concat_lhs
    );
    assert!(
        sidecar.functions[0].free_on_reassign[assign.index()].is_some(),
        "the allocating path still frees the old buffer; got: {:?}",
        sidecar.functions[0].free_on_reassign
    );
}

#[test]
fn plain_reassign_not_recorded() {
    // Control: a non-concat reassign never selects.
    let src = "fn main():\n\tmut s: str = \"a\"\n\ts = \"b\"\n\tprint(s)\n";
    let (diags, sidecar, tirs, _pool) = check_src_full(src);
    assert!(
        !diags
            .iter()
            .any(|d| d.severity == ryo_core::diag::Severity::Error),
        "no errors expected; got: {diags:?}"
    );
    let tir = &tirs[0];
    let assign = single_assign(tir);
    assert_ne!(
        tir.inst(tir.assign_view(assign).value).tag,
        TirTag::StrConcat
    );
    assert!(
        sidecar.functions[0]
            .consumed_concat_lhs
            .iter()
            .all(Option::is_none),
        "non-concat reassign must not be selected; got: {:?}",
        sidecar.functions[0].consumed_concat_lhs
    );
}
