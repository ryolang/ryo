//! M11 enum ownership tests — whole-value model. An enum value is ONE
//! `Owner`, exactly like a struct (the pass is pool-type-driven: a
//! needs-drop enum enters the lattice via `needs_drop`, and every
//! consume/reassign/branch rule applies unchanged). What construction
//! adds is payload consumption: a needs-drop payload arg of an
//! `EnumLit` moves, mirroring `StructLit` field consumption.

use super::super::*;
use super::common::*;

#[test]
fn enum_lit_str_payload_moves_source() {
    // (a) `Opt.Some(s)` consumes `s`: a later read is E0020.
    // Mirrors `moving_str_into_literal_consumes_source` for structs.
    let src = "enum Opt:\n\tSome(str)\n\tEmpty\n\nfn main():\n\ts = \"alice\"\n\to = Opt.Some(s)\n\tt = s\n";
    let diags = check_src(src);
    assert!(
        diags.iter().any(|d| d.code == DiagCode::UseAfterMove),
        "got {:?}",
        diags
    );
}

#[test]
fn enum_var_reassign_schedules_old_free() {
    // (b) Reassigning an enum-typed binding drops the old value:
    // `free_on_reassign` must record the displacement at the Assign.
    // Mirrors `str_field_reassign_frees_old_value`.
    let src = "enum Opt:\n\tSome(str)\n\tEmpty\n\nfn main():\n\tmut o = Opt.Some(\"alice\")\n\to = Opt.Empty\n\tprint(o)\n";
    let (diags, mut sidecar, _tirs, _pool) = check_src_full(src);
    assert!(
        !diags
            .iter()
            .any(|d| d.severity == ryo_core::diag::Severity::Error),
        "no errors expected; got {diags:?}"
    );
    let f = take_function_sidecar(&mut sidecar, 0);
    assert!(
        f.free_on_reassign.iter().any(|e| e.is_some()),
        "reassigning an enum var must schedule the old value's free; got {:?}",
        f.free_on_reassign
    );
}

#[test]
fn conditional_enum_move_schedules_branch_gated_free() {
    // (c) `if c: take(o) else: print(o)` — the then-arm MOVES `o`, the
    // else-arm only reads it. The post-if state is Moved (any-Moved-wins
    // over arms that reach the merge), so the function-exit last-use pass
    // skips the owner: the else-arm's still-Valid buffer needs an
    // arm-gated Free or it leaks on the taken path's complement.
    let src = "enum Opt:\n\tSome(str)\n\tEmpty\n\nfn take(move o: Opt):\n\tprint(o)\n\nfn main():\n\tmut o = Opt.Some(\"alice\")\n\tc = true\n\tif c:\n\t\ttake(o)\n\telse:\n\t\tprint(o)\n";
    let (diags, mut sidecar, _tirs, pool) = check_src_full(src);
    assert!(
        !diags
            .iter()
            .any(|d| d.severity == ryo_core::diag::Severity::Error),
        "no errors expected; got {diags:?}"
    );
    let main_idx = sidecar
        .functions
        .iter()
        .position(|f| pool.str(f.name) == "main")
        .expect("main's sidecar entry");
    let f = take_function_sidecar(&mut sidecar, main_idx);
    let gated: Vec<_> = f
        .free_schedule
        .iter()
        .filter(|fp| fp.branch.is_some())
        .collect();
    assert_eq!(
        gated.len(),
        1,
        "exactly one arm-gated Free for the conditionally-moved enum; got {gated:?}"
    );
}

#[test]
fn conditional_enum_reassign_schedules_fallthrough_drop() {
    // (c, reassign shape) `mut o = ...; if c: o = Opt.Empty` with `o`
    // never read after: the taken arm drops the pre-reassign buffer via
    // `free_on_reassign`; the NOT-taken path must free it too — via an
    // arm-gated ConditionalDeadDrop for the pre-branch owner.
    let src = "enum Opt:\n\tSome(str)\n\tEmpty\n\nfn main():\n\tmut o = Opt.Some(\"alice\")\n\tc = true\n\tif c:\n\t\to = Opt.Empty\n";
    let (diags, mut sidecar, _tirs, _pool) = check_src_full(src);
    assert!(
        !diags
            .iter()
            .any(|d| d.severity == ryo_core::diag::Severity::Error),
        "no errors expected; got {diags:?}"
    );
    let f = take_function_sidecar(&mut sidecar, 0);
    assert_eq!(
        f.conditional_dead_drops.len(),
        1,
        "expected one ConditionalDeadDrop for the pre-branch enum owner; got {:?}",
        f.conditional_dead_drops
    );
    assert!(
        !f.conditional_dead_drops[0].arms.is_empty(),
        "the drop must name at least one untouched arm (the fall-through)"
    );
}

#[test]
fn move_enum_out_of_param_invalidates() {
    // (d) A `move` param of a needs-drop enum type moves the caller's
    // value: using the source binding after the call is E0020.
    let src = "enum Opt:\n\tSome(str)\n\tEmpty\n\nfn take(move o: Opt):\n\tprint(o)\n\nfn main():\n\to = Opt.Some(\"alice\")\n\ttake(o)\n\tp = o\n";
    let diags = check_src(src);
    assert!(
        diags.iter().any(|d| d.code == DiagCode::UseAfterMove),
        "got {:?}",
        diags
    );
}

#[test]
fn copy_enum_copies_freely() {
    // (e) An enum whose variants are all Copy is itself Copy
    // (`needs_drop` is pool-computed): it never enters the lattice, so
    // repeated whole-value "moves" are plain copies.
    let src = "enum Color:\n\tRed\n\tGreen\n\nfn main():\n\tc = Color.Red\n\td = c\n\te = c\n\tprint(d)\n\tprint(e)\n";
    let diags = check_src(src);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != ryo_core::diag::Severity::Error),
        "Copy enums must copy freely; got {diags:?}"
    );
}
