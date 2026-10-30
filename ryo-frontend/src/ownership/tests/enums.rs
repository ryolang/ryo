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
    // arm-conditional Free or it leaks on the taken path's complement.
    let src = "enum Opt:\n\tSome(str)\n\tEmpty\n\nfn take(move o: Opt):\n\tprint(o)\n\nfn main():\n\tmut o = Opt.Some(\"alice\")\n\tc = true\n\tif c:\n\t\ttake(o)\n\telse:\n\t\tprint(o)\n";
    let (diags, mut sidecar, tirs, pool) = check_src_full(src);
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
    // Arm-conditional = the scrutinee owner's drop anchored inside an
    // arm's Flow IR block (block membership gates it to the arm's path).
    // Arm-body temp frees also anchor there, so the filter pins the
    // TARGET: the enum owner (the init's EnumLit).
    let enum_owner = (1..tirs[main_idx].instructions.len())
        .map(|i| ryo_core::tir::TirRef::from_raw(i as u32))
        .find(|&r| tirs[main_idx].inst(r).tag == ryo_core::tir::TirTag::EnumLit)
        .expect("the enum owner's EnumLit");
    let flow = build_flow(&tirs[main_idx], &pool);
    let if_stmt = (1..tirs[main_idx].instructions.len())
        .map(|i| ryo_core::tir::TirRef::from_raw(i as u32))
        .find(|&r| tirs[main_idx].inst(r).tag == ryo_core::tir::TirTag::IfStmt)
        .expect("the conditional if");
    let arm_blocks = flow.branch_arm_blocks(if_stmt);
    let gated: Vec<_> = f
        .drops
        .iter()
        .filter(|d| {
            d.target == enum_owner
                && flow.step_block_of(drop_anchor(d)) == Some(arm_blocks.else_block)
        })
        .collect();
    assert_eq!(
        gated.len(),
        1,
        "exactly one arm-conditional Free for the conditionally-moved enum; got {gated:?}"
    );
}

#[test]
fn conditional_enum_reassign_schedules_fallthrough_drop() {
    // (c, reassign shape) `mut o = ...; if c: o = Opt.Empty` with `o`
    // never read after: the taken arm drops the pre-reassign buffer via
    // `free_on_reassign`; the NOT-taken path must free it too — via an
    // arm-gated block-start dead drop for the pre-branch owner.
    let src = "enum Opt:\n\tSome(str)\n\tEmpty\n\nfn main():\n\tmut o = Opt.Some(\"alice\")\n\tc = true\n\tif c:\n\t\to = Opt.Empty\n";
    let (diags, mut sidecar, _tirs, _pool) = check_src_full(src);
    assert!(
        !diags
            .iter()
            .any(|d| d.severity == ryo_core::diag::Severity::Error),
        "no errors expected; got {diags:?}"
    );
    let f = take_function_sidecar(&mut sidecar, 0);
    let block_start = block_start_drops(&f);
    assert_eq!(
        block_start.len(),
        1,
        "expected one block-start dead drop for the pre-branch enum owner; got {:?}",
        f.drops
    );
    assert!(
        matches!(
            block_start[0].position,
            ryo_core::flow::DropPosition::BlockStart { .. }
        ),
        "the drop must fire at an untouched arm block (the fall-through)"
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

#[test]
fn moving_viewed_payload_into_enum_is_rejected() {
    // Constructing an enum moves its non-Copy payload. A live view of
    // that payload must prevent the move, or the enum could own and
    // eventually free the storage while the view still points at it.
    let src = "enum Opt:\n\tSome(str)\n\tEmpty\n\nfn main():\n\tmut s = \"alice\"\n\tv = s[0:2]\n\to = Opt.Some(s)\n\tprint(v)\n";
    let diags = check_src(src);
    assert!(
        diags.iter().any(|d| {
            d.code == DiagCode::SourceProjected
                && d.message.contains("`s`")
                && d.message.contains("move")
        }),
        "expected a move diagnostic naming the projected owner; got {diags:?}"
    );
}

#[test]
fn moving_viewed_payload_into_enum_on_one_branch_is_rejected() {
    // The view is read after the join, so it is live on the branch that
    // moves its owner into the enum. The diagnostic must identify that
    // branch's enum construction as the invalidating operation.
    let src = "enum Opt:\n\tSome(str)\n\tEmpty\n\nfn main():\n\tmut s = \"alice\"\n\tv = s[0:2]\n\tmut o = Opt.Empty\n\tc = true\n\tif c:\n\t\to = Opt.Some(s)\n\tprint(v)\n";
    let diags = check_src(src);
    assert!(
        diags.iter().any(|d| {
            d.code == DiagCode::SourceProjected
                && d.message.contains("`s`")
                && d.message.contains("move")
        }),
        "expected a branch-local move diagnostic naming the projected owner; got {diags:?}"
    );
}

#[test]
fn try_early_return_cleans_locals_once_without_freeing_propagated_payload() {
    let src = r#"error A(str)
error B
fn inner() -> A!str:
	return A(int_to_str(7) + "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
fn outer() -> (A | B)!str:
	local = int_to_str(8) + "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
	value = try inner()
	print(local)
	return value
"#;
    let (diags, mut sidecar, tirs, pool) = check_src_full(src);
    assert!(
        !diags
            .iter()
            .any(|d| d.severity == ryo_core::diag::Severity::Error),
        "try propagation must be ownership-clean; got {diags:?}"
    );
    let outer_index = tirs
        .iter()
        .position(|tir| pool.str(tir.name) == "outer")
        .expect("outer TIR");
    let outer = &tirs[outer_index];
    let sidecar = take_function_sidecar(&mut sidecar, outer_index);
    let refs =
        || (1..outer.instructions.len()).map(|index| ryo_core::tir::TirRef::from_raw(index as u32));
    let local_init = refs()
        .find(|&r| {
            outer.inst(r).tag == ryo_core::tir::TirTag::VarDecl
                && pool.str(outer.var_decl_view(r).name) == "local"
        })
        .map(|r| outer.var_decl_view(r).initializer)
        .expect("local initializer");
    let try_match = refs()
        .find(|&r| outer.inst(r).tag == ryo_core::tir::TirTag::Match)
        .expect("try dispatch");
    let view = outer.match_view(try_match);
    let early_return = view.arm(1).body.stmts().next().expect("error arm return");
    assert_eq!(outer.inst(early_return).tag, ryo_core::tir::TirTag::Return);
    let local_cleanup: Vec<_> = sidecar
        .drops
        .iter()
        .filter(|drop| {
            drop.target == local_init && drop.position.anchor_inst() == Some(early_return)
        })
        .collect();
    assert_eq!(
        local_cleanup.len(),
        1,
        "the ordinary return epilogue must clean the live local exactly once; got {:?}",
        sidecar.drops
    );
    let tokens = outer.match_by_move_owner_refs(try_match, &pool);
    assert_eq!(tokens.len(), 2, "success and error payloads each move once");
    let error_token = tokens[1];
    assert!(
        !sidecar.drops.iter().any(|drop| drop.target == error_token),
        "the propagated payload must move into the return, not also be freed: {:?}",
        sidecar.drops
    );
}
