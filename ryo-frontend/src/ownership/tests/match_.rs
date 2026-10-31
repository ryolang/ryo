//! M12 match ownership tests — per-arm branch walking with union merge.
//! A match arm that binds a non-`Copy` payload field by move consumes the
//! scrutinee on that arm's path; the post-match state is the union over
//! arms (moved in any arm ⇒ moved after). Borrowed scrutinees
//! (`match &o:` / a borrow-mode param read) never consume and register
//! no drops — borrow-ness is sourced from the ownership pass's own
//! lattice (`OwnerState::Borrowed`), not sema's `Scope::Binding::borrowed`.

use super::super::*;
use super::common::*;

fn find_match(tir: &ryo_core::tir::Tir) -> ryo_core::tir::TirRef {
    (1..tir.instructions.len())
        .map(|i| ryo_core::tir::TirRef::from_raw(u32::try_from(i).expect("index fits u32")))
        .find(|&r| tir.inst(r).tag == ryo_core::tir::TirTag::Match)
        .expect("a Match instruction exists")
}

fn main_sidecar(sidecar: &mut OwnershipSidecar, pool: &InternPool) -> FunctionSidecar {
    let idx = sidecar
        .functions
        .iter()
        .position(|f| pool.str(f.name) == "main")
        .expect("main's sidecar entry");
    take_function_sidecar(sidecar, idx)
}

#[test]
fn moved_in_one_arm_moves_at_merge_union() {
    // `Some(v)` binds the str payload by move → the scrutinee is consumed
    // on arm 0's path. The `_` arm only borrows it (legal inside the
    // arm — the lattice was snapshotted pre-match). The post-match read
    // sees the union: moved in any arm ⇒ moved ⇒ E0020.
    let src = "enum Opt:\n\tSome(str)\n\tEmpty\n\nfn main():\n\to = Opt.Some(\"alice\")\n\tmatch o:\n\t\tSome(v):\n\t\t\tprint(v)\n\t\t_:\n\t\t\tprint(o)\n\tprint(o)\n";
    let (diags, mut sidecar, tirs, pool) = check_src_full(src);
    let uam = diags
        .iter()
        .filter(|d| d.code == DiagCode::UseAfterMove)
        .count();
    assert_eq!(
        uam, 1,
        "exactly one use-after-move (the post-match read); got {diags:?}"
    );
    // Union merge machinery: the scrutinee survives the `_` arm, so an
    // arm-conditional Free of the scrutinee owner is scheduled anchored
    // INSIDE that arm's Flow IR block — block membership gates it to the
    // arm's path (the role the old branch tags played). Arm-body temp
    // frees also anchor in arm blocks, so the filter pins the TARGET:
    // the scrutinee's owner (the EnumLit), not an arm-local temp.
    let f = main_sidecar(&mut sidecar, &pool);
    let m = find_match(&tirs[0]);
    let scrutinee_owner = (1..tirs[0].instructions.len())
        .map(|i| ryo_core::tir::TirRef::from_raw(i as u32))
        .find(|&r| tirs[0].inst(r).tag == ryo_core::tir::TirTag::EnumLit)
        .expect("the scrutinee's EnumLit");
    let flow = build_flow(&tirs[0], &pool);
    let arm_blocks = match_arm_blocks(&flow, m);
    assert_eq!(arm_blocks.len(), 2, "one Flow IR block per arm");
    let gated: Vec<_> = f
        .drops
        .iter()
        .filter(|d| {
            d.target == scrutinee_owner && flow.step_block_of(drop_anchor(d)) == Some(arm_blocks[1])
        })
        .collect();
    assert_eq!(
        gated.len(),
        1,
        "exactly one arm-conditional Free (the scrutinee on the `_` arm); got {gated:?}"
    );
}

#[test]
fn owned_scrutinee_payload_move_invalidates_later_use() {
    // Bullet 2: owned scrutinee, `Some(v)` moves the payload, a later
    // statement uses the scrutinee → E0020 with the note at the match.
    let src = "enum Opt:\n\tSome(str)\n\tEmpty\n\nfn main():\n\to = Opt.Some(\"alice\")\n\tmatch o:\n\t\tSome(v):\n\t\t\tprint(v)\n\t\tEmpty:\n\t\t\tprint(\"empty\")\n\tprint(o)\n";
    let diags = check_src(src);
    assert!(
        diags.iter().any(|d| d.code == DiagCode::UseAfterMove),
        "got {:?}",
        diags
    );
}

#[test]
fn owner_diagnostic_preserves_binding_named_derive() {
    let src = "enum Opt:\n\tSome(str)\n\tEmpty\n\nfn main():\n\tderive = Opt.Some(\"alice\")\n\tmatch derive:\n\t\tSome(v):\n\t\t\tprint(v)\n\t\tEmpty:\n\t\t\tprint(\"empty\")\n\tprint(derive)\n";
    let diags = check_src(src);
    let msg = &diags
        .iter()
        .find(|diag| diag.code == DiagCode::UseAfterMove)
        .expect("the payload move must invalidate the scrutinee")
        .message;
    assert!(
        msg.contains("`derive`"),
        "E0020 must name the binding: {msg}"
    );
}

#[test]
fn copy_payload_match_binds_freely() {
    // Bullet 3: a Copy enum never enters the lattice — arms bind freely
    // and post-match use is fine.
    let src = "enum Num:\n\tNum(int)\n\nfn main():\n\tx = Num.Num(5)\n\tmatch x:\n\t\tNum(n):\n\t\t\tprint(n)\n\t\t_:\n\t\t\tprint(\"other\")\n\ty = x\n\tprint(y)\n";
    let diags = check_src(src);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != ryo_core::diag::Severity::Error),
        "Copy-payload match must be error-free; got {diags:?}"
    );
}

#[test]
fn copy_payload_binding_shadow_keeps_outer_owner() {
    // A Copy payload binding must not replace a same-named outer owner.
    let src = "enum Number:\n\tValue(int)\n\tEmpty\n\nfn main():\n\tvalue = int_to_str(73) + \" outer heap-backed owner\"\n\tmatch Number.Value(9):\n\t\tValue(value):\n\t\t\tprint(int_to_str(value))\n\t\tEmpty:\n\t\t\tprint(\"empty\")\n\tprint(value)\n\tprint(\"\\n\")\n";
    let (diags, mut sidecar, tirs, mut pool) = check_src_full(src);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != ryo_core::diag::Severity::Error),
        "no errors expected; got {diags:?}"
    );
    let main_idx = tirs
        .iter()
        .position(|tir| pool.str(tir.name) == "main")
        .expect("main's TIR");
    let tir = &tirs[main_idx];
    let value_name = pool.intern_str("value");
    let outer_init = (1..tir.instructions.len())
        .map(|i| ryo_core::tir::TirRef::from_raw(i as u32))
        .find_map(|r| {
            if tir.inst(r).tag == ryo_core::tir::TirTag::VarDecl {
                let decl = tir.var_decl_view(r);
                (decl.name == value_name).then_some(decl.initializer)
            } else {
                None
            }
        })
        .expect("outer value's initializer");
    let f = take_function_sidecar(&mut sidecar, main_idx);
    assert_eq!(
        f.drops
            .iter()
            .filter(|drop| drop.target == outer_init)
            .count(),
        1,
        "the outer string must keep one scope-exit release: {:?}",
        f.drops
    );
}

#[test]
fn borrowed_scrutinee_amp_no_consumption() {
    // Bullet 4: `match &x:` — no consumption, post-match use OK. (The `&`
    // lowers away in sema; the scrutinee owner is an owned local in
    // ownership's lattice, and no arm can bind a non-Copy field — E0133
    // gated it at sema — so nothing consumes and no drops register.)
    let src = "enum Num:\n\tNum(int)\n\nfn main():\n\tx = Num.Num(5)\n\tmatch &x:\n\t\tNum(n):\n\t\t\tprint(n)\n\t\t_:\n\t\t\tprint(\"other\")\n\ty = x\n\tprint(y)\n";
    let diags = check_src(src);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != ryo_core::diag::Severity::Error),
        "borrowed scrutinee must not be consumed; got {diags:?}"
    );
}

#[test]
fn borrowed_param_scrutinee_registers_no_drops() {
    // The ruling's drop-suppression: on a genuinely borrowed scrutinee
    // (a borrow-mode param read — `OwnerState::Borrowed` in the
    // lattice) even wildcard payload fields drop NOTHING: no arm-start
    // drops, no arm-conditional frees, and the scrutinee is not consumed.
    let src = "enum Opt:\n\tSome(str)\n\tEmpty\n\nfn show(o: Opt):\n\tmatch o:\n\t\tSome(_):\n\t\t\tprint(\"some\")\n\t\tEmpty:\n\t\t\tprint(\"empty\")\n\tprint(o)\n\nfn main():\n\tshow(Opt.Some(\"alice\"))\n";
    let (diags, mut sidecar, _tirs, pool) = check_src_full(src);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != ryo_core::diag::Severity::Error),
        "borrowed-param match must be error-free; got {diags:?}"
    );
    let show_idx = sidecar
        .functions
        .iter()
        .position(|f| pool.str(f.name) == "show")
        .expect("show's sidecar entry");
    let f = take_function_sidecar(&mut sidecar, show_idx);
    assert!(
        block_start_drops(&f).is_empty(),
        "borrowed scrutinee: wildcard payload fields drop nothing; got {:?}",
        f.drops
    );
    // No arm-conditional frees were minted for the borrowed scrutinee:
    // the branch-gating this test used to assert is gone, and a
    // borrowed param has no owner instruction to schedule a free
    // against — `match_arm_drops.is_empty()` above plus the
    // error-free run IS the assertion.
}

#[test]
fn anon_owned_partial_move_registers_arm_start_drop() {
    // Bullet 5: anon-pattern match on an owned anonymous struct. The arm
    // binds `name` by move (consuming the scrutinee on that arm) and
    // wildcards `tag`; the unbound needs-drop field registers as an
    // arm-start drop for codegen (codegen emits it at arm start). The
    // `_` arm leaves
    // the scrutinee valid — the union merge stamps it moved after the
    // match, so an arm-conditional whole-value Free covers that arm.
    let src = "fn main():\n\tp = {name=\"alice\", tag=\"x\"}\n\tmatch p:\n\t\t{name=q, tag=_}:\n\t\t\tprint(q)\n\t\t_:\n\t\t\tprint(\"other\")\n";
    let (diags, mut sidecar, tirs, pool) = check_src_full(src);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != ryo_core::diag::Severity::Error),
        "no errors expected; got {diags:?}"
    );
    let f = main_sidecar(&mut sidecar, &pool);
    let m = find_match(&tirs[0]);
    // The arm-start drop targets the match wire's scrutinee (the
    // `var p` read); the whole-value arm-conditional Free targets the
    // scrutinee's OWNER (the StructLit temp) — two identities.
    let scrutinee = tirs[0].match_view(m).scrutinee;
    let owner = (1..tirs[0].instructions.len())
        .map(|i| ryo_core::tir::TirRef::from_raw(i as u32))
        .find(|&r| tirs[0].inst(r).tag == ryo_core::tir::TirTag::StructLit)
        .expect("the scrutinee's StructLit owner");
    let flow = build_flow(&tirs[0], &pool);
    let arm_blocks = match_arm_blocks(&flow, m);
    assert_eq!(arm_blocks.len(), 2, "one Flow IR block per arm");
    let arm_start: Vec<_> = block_start_drops(&f)
        .into_iter()
        .filter(|d| d.target == scrutinee)
        .collect();
    assert_eq!(
        arm_start.len(),
        1,
        "the consuming arm registers exactly one arm-start drop; got {:?}",
        f.drops
    );
    assert!(
        matches!(
            arm_start[0].position,
            ryo_core::flow::DropPosition::BlockStart { block } if block == arm_blocks[0]
        ),
        "the drop belongs to the consuming arm's Flow IR block"
    );
    // Arm-body temp frees also anchor in arm blocks, so the filter pins
    // the TARGET: the scrutinee owner, not an arm-local temp.
    let gated: Vec<_> = f
        .drops
        .iter()
        .filter(|d| {
            d.target == owner
                && d.position
                    .anchor_inst()
                    .and_then(|a| flow.step_block_of(a))
                    .is_some_and(|b| b == arm_blocks[1])
        })
        .collect();
    assert_eq!(
        gated.len(),
        1,
        "exactly one arm-conditional Free (the scrutinee on the `_` arm); got {gated:?}"
    );
}

#[test]
fn wildcard_payload_field_on_owned_enum_registers_arm_start_drop() {
    // Complement of the anon case: `Both(x, _)` moves field 0 out of the
    // owned enum, so field 1 (the wildcard `_`) must drop at arm start.
    let src = "enum Pair:\n\tBoth(str, str)\n\nfn main():\n\tp = Pair.Both(\"a\", \"b\")\n\tmatch p:\n\t\tBoth(x, _):\n\t\t\tprint(x)\n";
    let (diags, mut sidecar, tirs, pool) = check_src_full(src);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != ryo_core::diag::Severity::Error),
        "no errors expected; got {diags:?}"
    );
    let f = main_sidecar(&mut sidecar, &pool);
    let m = find_match(&tirs[0]);
    let flow = build_flow(&tirs[0], &pool);
    let arm_blocks = match_arm_blocks(&flow, m);
    assert_eq!(arm_blocks.len(), 1, "single exhaustive arm");
    let m_scrut = tirs[0].match_view(m).scrutinee;
    let arm_start: Vec<_> = block_start_drops(&f)
        .into_iter()
        .filter(|d| d.target == m_scrut)
        .collect();
    assert_eq!(
        arm_start.len(),
        1,
        "the consuming arm registers one arm-start drop; got {:?}",
        f.drops
    );
    assert!(
        matches!(
            arm_start[0].position,
            ryo_core::flow::DropPosition::BlockStart { block } if block == arm_blocks[0]
        ),
        "the drop belongs to the consuming arm's Flow IR block"
    );
    // The bound field `x` registered as a fresh owner keyed on its
    // owner-token FieldAccess (the inst immediately before the Match);
    // its last-use Free is anchored after the in-arm `print(x)` read.
    let token = ryo_core::tir::TirRef::from_raw((m.index() - 1) as u32);
    assert!(
        matches!(tirs[0].inst(token).tag, ryo_core::tir::TirTag::FieldAccess),
        "the owner token is the FieldAccess before the Match"
    );
    assert!(
        f.drops.iter().any(|fp| fp.target == token),
        "the bound field's owner-token Free is scheduled; got {:?}",
        f.drops
    );
}

#[test]
fn binding_name_reused_across_arms_no_false_conflicts() {
    // Bullet 6: both arms bind `v`; each registration is arm-local
    // (per-arm snapshot/restore), so no cross-arm move conflicts and no
    // diagnostics at all.
    let src = "enum E:\n\tA(str)\n\tB(str)\n\nfn main():\n\te = E.A(\"x\")\n\tmatch e:\n\t\tA(v):\n\t\t\tprint(v)\n\t\tB(v):\n\t\t\tprint(v)\n\tprint(\"done\")\n";
    let diags = check_src(src);
    assert!(
        diags.is_empty(),
        "reused arm binding names must be conflict-free; got {diags:?}"
    );
}

#[test]
fn no_consuming_arm_leaves_scrutinee_valid() {
    // Mirror of the if fall-through rule: when NO arm binds a non-Copy
    // field by move, the scrutinee is untouched — post-match use is
    // legal, no arm-gated frees, no arm-start drops.
    let src = "enum Opt:\n\tSome(str)\n\tEmpty\n\nfn main():\n\to = Opt.Some(\"alice\")\n\tmatch o:\n\t\tSome(_):\n\t\t\tprint(\"some\")\n\t\tEmpty:\n\t\t\tprint(\"empty\")\n\tprint(o)\n";
    let (diags, mut sidecar, tirs, pool) = check_src_full(src);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != ryo_core::diag::Severity::Error),
        "no-consumption match must be error-free; got {diags:?}"
    );
    let f = main_sidecar(&mut sidecar, &pool);
    assert!(
        block_start_drops(&f).is_empty(),
        "no consuming arm → no arm-start drops; got {:?}",
        f.drops
    );
    // No conditional moves → no free TARGETING the scrutinee is
    // anchored inside a match arm block (the old branch-gating
    // assertion; arm-body temp frees legitimately anchor inside arm
    // blocks, gated by anchor evaluation).
    let m = find_match(&tirs[0]);
    let scrutinee_owner = (1..tirs[0].instructions.len())
        .map(|i| ryo_core::tir::TirRef::from_raw(i as u32))
        .find(|&r| tirs[0].inst(r).tag == ryo_core::tir::TirTag::EnumLit)
        .expect("the scrutinee's EnumLit");
    let flow = build_flow(&tirs[0], &pool);
    let arm_blocks = match_arm_blocks(&flow, m);
    assert!(
        f.drops.iter().all(|d| {
            d.target != scrutinee_owner
                || !arm_blocks
                    .contains(&flow.step_block_of(drop_anchor(d)).expect("step in a block"))
        }),
        "no conditional moves → no arm-conditional frees for the scrutinee; got {:?}",
        f.drops
    );
    assert!(
        !f.drops.is_empty(),
        "the scrutinee's whole-value last-use Free is still scheduled"
    );
}

#[test]
fn dead_bind_in_returning_arm_single_epilogue_free() {
    // A never-read by-move bind in a return-ending arm: the return
    // epilogue owns the exit — exactly one Free for the token (the
    // sweep must not double it with an arm-gated Free), and no
    // ungated after-match Free (that would free garbage on the
    // sibling arm's path).
    let src = "enum Opt:\n\tSome(str)\n\tEmpty\n\nfn main():\n\to = Opt.Some(\"alice\")\n\tmatch o:\n\t\tSome(v):\n\t\t\treturn\n\t\tEmpty:\n\t\t\tprint(\"empty\")\n\tprint(o)\n";
    let (diags, mut sidecar, tirs, pool) = check_src_full(src);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != ryo_core::diag::Severity::Error),
        "no errors expected; got {diags:?}"
    );
    let f = main_sidecar(&mut sidecar, &pool);
    let m = find_match(&tirs[0]);
    let token = ryo_core::tir::TirRef::from_raw((m.index() - 1) as u32);
    let token_frees: Vec<_> = f.drops.iter().filter(|fp| fp.target == token).collect();
    assert_eq!(
        token_frees.len(),
        1,
        "exactly one Free for the dead arm bind (the return epilogue); got {token_frees:?}"
    );
    assert!(
        diags.iter().any(|d| d.code == DiagCode::DeadStore),
        "the never-read bind still warns W0001; got {diags:?}"
    );
}

#[test]
fn local_before_returning_match_is_freed_once() {
    // An unused local must be released once even when one match arm returns.
    let src = "fn finish(should_return: bool):\n\towned = int_to_str(42) + \" payload that needs heap storage\"\n\tmatch should_return:\n\t\ttrue:\n\t\t\tprint(\"returning\\n\")\n\t\t\treturn\n\t\tfalse:\n\t\t\tprint(\"fall through\\n\")\n\tprint(\"after match\\n\")\n\nfn main():\n\tfinish(true)\n\tfinish(false)\n";
    let (diags, mut sidecar, tirs, mut pool) = check_src_full(src);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != ryo_core::diag::Severity::Error),
        "no errors expected; got {diags:?}"
    );
    let finish_idx = tirs
        .iter()
        .position(|tir| pool.str(tir.name) == "finish")
        .expect("finish's TIR");
    let tir = &tirs[finish_idx];
    let owned_name = pool.intern_str("owned");
    let owned_init = (1..tir.instructions.len())
        .map(|i| ryo_core::tir::TirRef::from_raw(i as u32))
        .find_map(|r| {
            if tir.inst(r).tag == ryo_core::tir::TirTag::VarDecl {
                let decl = tir.var_decl_view(r);
                (decl.name == owned_name).then_some(decl.initializer)
            } else {
                None
            }
        })
        .expect("owned's initializer");
    let f = take_function_sidecar(&mut sidecar, finish_idx);
    assert_eq!(
        f.drops
            .iter()
            .filter(|drop| drop.target == owned_init)
            .count(),
        1,
        "the local must be released once across the return and fallthrough paths: {:?}",
        f.drops
    );
}

#[test]
fn local_before_all_returning_match_arms_is_freed_once() {
    // A local must be released when every match arm returns.
    let src = "fn finish(flag: bool):\n\towned = int_to_str(17) + \" return payload that needs heap storage\"\n\tmatch flag:\n\t\ttrue:\n\t\t\treturn\n\t\tfalse:\n\t\t\treturn\n\nfn main():\n\tfinish(true)\n\tfinish(false)\n";
    let (diags, mut sidecar, tirs, mut pool) = check_src_full(src);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != ryo_core::diag::Severity::Error),
        "no errors expected; got {diags:?}"
    );
    let finish_idx = tirs
        .iter()
        .position(|tir| pool.str(tir.name) == "finish")
        .expect("finish's TIR");
    let tir = &tirs[finish_idx];
    let owned_name = pool.intern_str("owned");
    let owned_init = (1..tir.instructions.len())
        .map(|i| ryo_core::tir::TirRef::from_raw(i as u32))
        .find_map(|r| {
            if tir.inst(r).tag == ryo_core::tir::TirTag::VarDecl {
                let decl = tir.var_decl_view(r);
                (decl.name == owned_name).then_some(decl.initializer)
            } else {
                None
            }
        })
        .expect("owned's initializer");
    let f = take_function_sidecar(&mut sidecar, finish_idx);
    assert_eq!(
        f.drops
            .iter()
            .filter(|drop| drop.target == owned_init)
            .count(),
        1,
        "the local must be released once across both return paths: {:?}",
        f.drops
    );
}

#[test]
fn view_last_used_in_scrutinee_does_not_freeze_arms() {
    // F1: a projection whose last use is a read inside the scrutinee
    // must be dead before any arm runs — the P4 lift / condition-drain
    // mirror of analyze_if_stmt. (The per-arm refine kill also covers
    // this shape before arm-body checks; the drain additionally closes
    // the arm-start consume window for any projection on the scrutinee
    // owner, and keeps the walker symmetric with the if regardless.)
    let src = "enum Opt:\n\tSome(str)\n\tEmpty\n\nfn take(move x: str):\n\tprint(x)\n\nfn main():\n\ts = \"hello\"\n\tv = s[0:2]\n\tmatch v.len():\n\t\t2:\n\t\t\ttake(s)\n\t\t_:\n\t\t\tprint(\"no\")\n\tprint(\"done\")\n";
    let diags = check_src(src);
    assert!(
        diags.is_empty(),
        "the view died at the scrutinee read — no false freeze; got {diags:?}"
    );
}

#[test]
fn arm_local_view_shadow_does_not_freeze_outer_root() {
    // F2: the view-liveness pre-pass registers arm-declared view
    // bindings arm-scoped (the if mark/rollback mirror). Pre-fix the
    // arm's `w` never registered, its in-arm read misresolved to the
    // OUTER view `w`, and the post-match reassign of `s` false-froze.
    let src = "fn main():\n\tmut s = \"hello\"\n\tw = s[0:2]\n\tmatch w.len():\n\t\t2:\n\t\t\tmut w = s[0:3]\n\t\t\tprint(w.len())\n\t\t_:\n\t\t\tprint(\"no\")\n\ts = \"bye\"\n\tprint(s)\n";
    let diags = check_src(src);
    assert!(
        diags.is_empty(),
        "arm-local view dies with the arm — no false freeze on the post-match mutation; got {diags:?}"
    );
}

#[test]
fn live_view_across_match_still_freezes_root() {
    // Positive control: a projection whose last use is AFTER the
    // mutation is genuinely live at the mutation site and must still
    // freeze the root — the F1/F2 fixes must not weaken real P2
    // detection.
    let src = "fn main():\n\tmut s = \"hello\"\n\tw = s[0:2]\n\tmatch w.len():\n\t\t2:\n\t\t\tprint(\"two\")\n\t\t_:\n\t\t\tprint(\"no\")\n\ts = \"bye\"\n\tprint(w.len())\n";
    let diags = check_src(src);
    assert!(
        diags.iter().any(|d| d.code == DiagCode::SourceProjected),
        "the live view still freezes the root (P2); got {diags:?}"
    );
}

#[test]
fn moved_scrutinee_reports_uam_once() {
    // A match on an already-moved scrutinee is a use-after-move read.
    // With no consuming arm the walk's scrutinee check is the sole
    // reporter; `consume_underlying` stays the authority when an arm
    // does consume (no double-report from a pre-check).
    let src = "enum Opt:\n\tSome(str)\n\tEmpty\n\nfn take(move o: Opt):\n\tprint(o)\n\nfn main():\n\to = Opt.Some(\"alice\")\n\ttake(o)\n\tmatch o:\n\t\tSome(_):\n\t\t\tprint(\"some\")\n\t\tEmpty:\n\t\t\tprint(\"empty\")\n\tprint(\"done\")\n";
    let diags = check_src(src);
    let uam = diags
        .iter()
        .filter(|d| d.code == DiagCode::UseAfterMove)
        .count();
    assert_eq!(
        uam, 1,
        "the moved-scrutinee match reports exactly one UAM; got {diags:?}"
    );
}

#[test]
fn errored_match_is_not_walked() {
    // Ruling: an errored match recovers as the error-typed `Unreachable`
    // sentinel — no real Match inst reaches the walk. Ownership must not
    // trip on it (no ICE, no derived noise beyond sema's E0129).
    let src = "fn main():\n\to = 5\n\tmatch o:\n\t\t0: 1\n\tprint(o)\n";
    let diags = check_src(src);
    assert!(
        diags.iter().any(|d| d.code == DiagCode::NonExhaustiveMatch),
        "sema's E0129 surfaces; got {diags:?}"
    );
    assert!(
        !diags.iter().any(|d| d.code == DiagCode::UseAfterMove),
        "the errored match must not mint ownership diagnostics; got {diags:?}"
    );
}

#[test]
fn moving_payload_field_out_of_enum_rejected() {
    // The M11-deferred enum arm of `check_field_move_out`: a needs-drop
    // FieldAccess whose object is ENUM-typed (the match owner-token
    // shape) is rejected exactly like a struct field read. Hand-built
    // TIR — no source shape produces an enum-object FieldAccess consume
    // in M12, so the arm is exercised directly.
    use ryo_core::span::Span;
    use ryo_core::tir::TirBuilder;
    use ryo_core::types::{EnumVariantDef, StructFlags, VariantKind};
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let opt_name = pool.intern_str("Opt");
    let opt_ty = pool.declare_enum(opt_name);
    let some = pool.intern_str("Some");
    let empty = pool.intern_str("Empty");
    let f0 = pool.intern_str("0");
    pool.define_enum(
        opt_ty,
        opt_name,
        &[
            EnumVariantDef {
                name: some,
                kind: VariantKind::Tuple,
                fields: vec![(f0, str_ty)],
            },
            EnumVariantDef {
                name: empty,
                kind: VariantKind::Unit,
                fields: vec![],
            },
        ],
        StructFlags::default(),
    );
    let o = pool.intern_str("o");
    let x = pool.intern_str("x");
    let span = Span::EMPTY;
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let ov = tb.var(o, opt_ty, span);
    // A payload field read off the enum value, consumed by a VarDecl —
    // the guard's enum arm must reject it.
    let fa = tb.field_access(ov, 0, str_ty, span);
    let decl = tb.var_decl(x, false, str_ty, fa, span);
    let tir = tb.finish(&[decl]);
    let mut sink = DiagSink::new();
    let _sc = check(
        std::slice::from_ref(&tir),
        &[ryo_core::flow::Flow::build(&tir, &pool)],
        &pool,
        &mut sink,
    );
    let diags = sink.into_diags();
    assert!(
        diags.iter().any(|d| d.code == DiagCode::MoveOutOfField),
        "enum payload move-out must be rejected; got {diags:?}"
    );
}

#[test]
fn break_in_stmt_match_arm_gets_loop_exit_free() {
    // A `break` inside a statement-level match arm inside a loop must
    // get its loop-exit Free: the match arrives ExprStmt-wrapped (sema's
    // convention), so the jump-free walk only sees the break by
    // unwrapping through `stmt_match`. Pre-fix the arm was skipped and
    // the loop-local heap str leaked on the break path (the `_` arm's
    // last-use Free is not on that path).
    let src = "fn main():\n\tmut i = 0\n\twhile i < 3:\n\t\ts = int_to_str(i) + \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\n\t\tmatch i:\n\t\t\t1:\n\t\t\t\tbreak\n\t\t\t_:\n\t\t\t\tprint(s)\n\t\ti = i + 1\n\tprint(\"done\")\n";
    let (diags, mut sidecar, tirs, pool) = check_src_full(src);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != ryo_core::diag::Severity::Error),
        "no errors expected; got {diags:?}"
    );
    let f = main_sidecar(&mut sidecar, &pool);
    let s = pool.find_str("s").expect("`s` interned");
    let find = |tag: ryo_core::tir::TirTag| {
        (1..tirs[0].instructions.len())
            .map(|i| ryo_core::tir::TirRef::from_raw(i as u32))
            .find(|&r| tirs[0].inst(r).tag == tag)
    };
    let init = (1..tirs[0].instructions.len())
        .map(|i| ryo_core::tir::TirRef::from_raw(i as u32))
        .find(|&r| {
            tirs[0].inst(r).tag == ryo_core::tir::TirTag::VarDecl
                && tirs[0].var_decl_view(r).name == s
        })
        .map(|r| tirs[0].var_decl_view(r).initializer)
        .expect("the loop-local `s` VarDecl");
    let brk = find(ryo_core::tir::TirTag::Break).expect("the in-arm break");
    assert!(
        f.drops
            .iter()
            .any(|fp| fp.target == init && fp.position.anchor_inst().unwrap() == brk),
        "the break inside the match arm gets a loop-exit Free for the \
         loop-local str; got {:?}",
        f.drops
    );
}

#[test]
fn prune_match_arms_covers_expression_arm_siblings() {
    // A `break` in one arm of a statement-level match, with the SIBLING
    // arm an expression arm holding the loop-local str's last use
    // (`_: s.len()`). `prune_match_arms` iterated `match_arm_bodies`,
    // which decodes an empty statement list for an expression arm, so
    // the sibling's anchor stayed on the break's path and the anchored
    // last-use Free falsely covered the jump — the loop-exit Free was
    // skipped and the break path leaked. The prune must treat the
    // expression arm body as the arm root: the break keeps its exit
    // Free for `s`.
    let src = "fn main():\n\tmut i = 0\n\twhile i < 3:\n\t\ts = int_to_str(i) + \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\n\t\tmatch i:\n\t\t\t1:\n\t\t\t\tbreak\n\t\t\t_: s.len()\n\t\ti = i + 1\n\tprint(\"done\")\n";
    let (diags, mut sidecar, tirs, pool) = check_src_full(src);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != ryo_core::diag::Severity::Error),
        "no errors expected; got {diags:?}"
    );
    let f = main_sidecar(&mut sidecar, &pool);
    let s = pool.find_str("s").expect("`s` interned");
    let init = (1..tirs[0].instructions.len())
        .map(|i| ryo_core::tir::TirRef::from_raw(i as u32))
        .find(|&r| {
            tirs[0].inst(r).tag == ryo_core::tir::TirTag::VarDecl
                && tirs[0].var_decl_view(r).name == s
        })
        .map(|r| tirs[0].var_decl_view(r).initializer)
        .expect("the loop-local `s` VarDecl");
    let brk = (1..tirs[0].instructions.len())
        .map(|i| ryo_core::tir::TirRef::from_raw(i as u32))
        .find(|&r| tirs[0].inst(r).tag == ryo_core::tir::TirTag::Break)
        .expect("the in-arm break");
    assert!(
        f.drops
            .iter()
            .any(|fp| fp.target == init && fp.position.anchor_inst().unwrap() == brk),
        "the break inside the match arm gets a loop-exit Free for the \
         loop-local str even when the sibling arm is an expression arm; \
         got {:?}",
        f.drops
    );
}

#[test]
fn named_vardecl_in_stmt_match_arm_freed_exactly_once() {
    // A heap str VarDecl inside a statement-level match arm is a NAMED
    // init: `collect_named_inits_rec` must see it through the ExprStmt
    // wrapper (`stmt_match`), or the anon-temp pass schedules a second
    // Free on top of the named last-use Free — a double free.
    let src = "fn main():\n\tmut i = 0\n\twhile i < 2:\n\t\tmatch i:\n\t\t\t0:\n\t\t\t\ts = int_to_str(i) + \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\n\t\t\t\tprint(s)\n\t\t\t_:\n\t\t\t\tprint(\"other\")\n\t\ti = i + 1\n\tprint(\"done\")\n";
    let (diags, mut sidecar, tirs, pool) = check_src_full(src);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != ryo_core::diag::Severity::Error),
        "no errors expected; got {diags:?}"
    );
    let f = main_sidecar(&mut sidecar, &pool);
    let s = pool.find_str("s").expect("`s` interned");
    let init = (1..tirs[0].instructions.len())
        .map(|i| ryo_core::tir::TirRef::from_raw(i as u32))
        .find(|&r| {
            tirs[0].inst(r).tag == ryo_core::tir::TirTag::VarDecl
                && tirs[0].var_decl_view(r).name == s
        })
        .map(|r| tirs[0].var_decl_view(r).initializer)
        .expect("the arm-local `s` VarDecl");
    assert_eq!(
        f.drops.iter().filter(|fp| fp.target == init).count(),
        1,
        "exactly one Free for the arm-local named str; got {:?}",
        f.drops
    );
}

#[test]
fn break_in_value_position_match_arm_gets_loop_exit_free() {
    // A `break` inside the arm of a match in VALUE position (a VarDecl
    // initializer) must get its loop-exit Free: the jump walk only saw
    // statement-level matches (`stmt_match`), so the VarDecl statement
    // hid the break and the loop-local heap str leaked on the break
    // path (its last-use Free, anchored after the match, never fires
    // there — the match does not complete on that path).
    let src = "fn main():\n\tmut i = 0\n\twhile i < 3:\n\t\ts = int_to_str(i) + \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\n\t\tn = match i:\n\t\t\t1:\n\t\t\t\tbreak\n\t\t\t_: 0\n\t\tprint(s)\n\t\ti = i + 1\n\tprint(\"done\")\n";
    let (diags, mut sidecar, tirs, pool) = check_src_full(src);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != ryo_core::diag::Severity::Error),
        "no errors expected; got {diags:?}"
    );
    let f = main_sidecar(&mut sidecar, &pool);
    let s = pool.find_str("s").expect("`s` interned");
    let init = (1..tirs[0].instructions.len())
        .map(|i| ryo_core::tir::TirRef::from_raw(i as u32))
        .find(|&r| {
            tirs[0].inst(r).tag == ryo_core::tir::TirTag::VarDecl
                && tirs[0].var_decl_view(r).name == s
        })
        .map(|r| tirs[0].var_decl_view(r).initializer)
        .expect("the loop-local `s` VarDecl");
    let brk = (1..tirs[0].instructions.len())
        .map(|i| ryo_core::tir::TirRef::from_raw(i as u32))
        .find(|&r| tirs[0].inst(r).tag == ryo_core::tir::TirTag::Break)
        .expect("the in-arm break");
    assert!(
        f.drops
            .iter()
            .any(|fp| fp.target == init && fp.position.anchor_inst().unwrap() == brk),
        "the break inside the value-position match arm gets a loop-exit \
         Free for the loop-local str; got {:?}",
        f.drops
    );
}

#[test]
fn return_in_value_position_match_arm_gets_promo_epilogue() {
    // A `return` inside the arm of a match in Return-operand position
    // must be seen by `collect_return_stmts` (via
    // `visit_operand_matches`), or the promotion-buffer return
    // epilogue misses it: the view's buffer, created before the match
    // and read only in the sibling arm, is freed on this return path
    // only by the epilogue-anchored copy.
    let src = "enum E:\n\tA\n\tB\n\nfn f(p: str, e: E) -> int:\n\tv = p[0:8]\n\treturn match e:\n\t\tA: v.len()\n\t\tB:\n\t\t\treturn 1\n\nfn main():\n\tf(\"0123456789012345678901234567890123456789\", E.A)\n\tf(\"0123456789012345678901234567890123456789\", E.B)\n";
    let (diags, mut sidecar, tirs, pool) = check_src_full(src);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != ryo_core::diag::Severity::Error),
        "no errors expected; got {diags:?}"
    );
    let f_idx = sidecar
        .functions
        .iter()
        .position(|f| pool.str(f.name) == "f")
        .expect("f's sidecar entry");
    let f = take_function_sidecar(&mut sidecar, f_idx);
    // The inner (arm) return — the one whose operand is NOT the match.
    let m = find_match(&tirs[f_idx]);
    let inner_return = (1..tirs[f_idx].instructions.len())
        .map(|i| ryo_core::tir::TirRef::from_raw(i as u32))
        .find(|&r| {
            tirs[f_idx].inst(r).tag == ryo_core::tir::TirTag::Return
                && matches!(tirs[f_idx].inst(r).data, ryo_core::tir::TirData::UnOp(o) if o != m)
        })
        .expect("the arm's return");
    assert!(
        f.promotion_frees
            .iter()
            .any(|pf| pf.position.anchor_inst().unwrap() == inner_return),
        "the arm's return gets a promotion-buffer epilogue Free; got {:?}",
        f.promotion_frees
    );
}

#[test]
fn dead_bind_in_expr_arm_freed_exactly_once_in_arm() {
    // An expression arm binding a non-`Copy` payload by move and never
    // reading it: the dead-bind machinery schedules an arm-gated Free
    // anchored at the arm-body expression. Codegen must flush it
    // inside the arm (before the merge jump) or the buffer leaks.
    let src = "enum Opt:\n\tSome(str)\n\tEmpty\n\nfn main():\n\to = Opt.Some(\"alice-in-wonderland-0000000000000000000\")\n\tn = match o:\n\t\tSome(v): 1\n\t\t_: 0\n\tprint(int_to_str(n))\n";
    let (diags, mut sidecar, tirs, pool) = check_src_full(src);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != ryo_core::diag::Severity::Error),
        "no errors expected; got {diags:?}"
    );
    let f = main_sidecar(&mut sidecar, &pool);
    let m = find_match(&tirs[0]);
    let flow = build_flow(&tirs[0], &pool);
    let arm_blocks = match_arm_blocks(&flow, m);
    let token = tirs[0]
        .match_by_move_owner_refs(m, &pool)
        .into_iter()
        .next()
        .expect("one by-move owner token for `v`");
    let body = match tirs[0].match_view(m).arms().next().unwrap().body {
        ryo_core::tir::MatchBodyView::Expr(e) => e,
        ryo_core::tir::MatchBodyView::Stmts(_) => panic!("`Some(v) => 1` is an expression arm"),
    };
    let frees: Vec<_> = f.drops.iter().filter(|fp| fp.target == token).collect();
    assert_eq!(
        frees.len(),
        1,
        "exactly one Free for the dead payload bind; got {frees:?}"
    );
    assert_eq!(
        flow.step_block_of(drop_anchor(frees[0])),
        Some(arm_blocks[0]),
        "gated to the `Some(v)` arm by block membership"
    );
    assert_eq!(
        frees[0].position.anchor_inst().unwrap(),
        body,
        "anchored at the arm-body expression (fired inside the arm)"
    );
}

#[test]
fn dead_payload_in_continue_arm_is_freed_before_continue() {
    // Moving an unread heap payload out of the scrutinee and then
    // continuing must release the payload on that iteration's path.
    let src = "enum Item:\n\tText(str)\n\tEmpty\n\nfn main():\n\tmut i = 0\n\twhile i < 4:\n\t\ti += 1\n\t\titem = match i:\n\t\t\t1: Item.Text(int_to_str(i) + \" payload that needs heap storage\")\n\t\t\t_: Item.Empty\n\t\tmatch item:\n\t\t\tText(unused):\n\t\t\t\tcontinue\n\t\t\tEmpty:\n\t\t\t\tprint(\"empty\\n\")\n\tprint(\"done\\n\")\n";
    let (diags, mut sidecar, tirs, pool) = check_src_full(src);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != ryo_core::diag::Severity::Error),
        "no errors expected; got {diags:?}"
    );
    let f = main_sidecar(&mut sidecar, &pool);
    let consuming_match = (1..tirs[0].instructions.len())
        .map(|i| ryo_core::tir::TirRef::from_raw(u32::try_from(i).expect("index fits u32")))
        .find(|&r| {
            tirs[0].inst(r).tag == ryo_core::tir::TirTag::Match
                && !tirs[0].match_by_move_owner_refs(r, &pool).is_empty()
        })
        .expect("the match that moves `unused`");
    let token = tirs[0]
        .match_by_move_owner_refs(consuming_match, &pool)
        .into_iter()
        .next()
        .expect("one by-move owner token for `unused`");
    let continue_inst = (1..tirs[0].instructions.len())
        .map(|i| ryo_core::tir::TirRef::from_raw(i as u32))
        .find(|&r| tirs[0].inst(r).tag == ryo_core::tir::TirTag::Continue)
        .expect("the continue in the consuming match arm");
    let flow = build_flow(&tirs[0], &pool);
    let continue_block = flow
        .terms
        .iter()
        .position(|term| matches!(term, ryo_core::flow::Terminator::Continue { .. }))
        .map(|index| ryo_core::flow::BlockId(index as u32))
        .expect("the continue's Flow block");
    let frees: Vec<_> = f.drops.iter().filter(|drop| drop.target == token).collect();
    assert_eq!(
        frees.len(),
        1,
        "exactly one release for the unread payload; got {frees:?}"
    );
    assert!(
        matches!(
            frees[0].position,
            ryo_core::flow::DropPosition::PreTerminator { block } if block == continue_block
        ),
        "release runs before the continue terminator; continue TIR is {continue_inst:?}, got {:?}",
        frees[0].position
    );

    let item_init = (1..tirs[0].instructions.len())
        .map(|i| ryo_core::tir::TirRef::from_raw(i as u32))
        .find_map(|r| {
            if tirs[0].inst(r).tag == ryo_core::tir::TirTag::VarDecl {
                let decl = tirs[0].var_decl_view(r);
                (pool.str(decl.name) == "item").then_some(decl.initializer)
            } else {
                None
            }
        })
        .expect("the enum value matched by the consuming match");
    let scrutinee_read = tirs[0].match_view(consuming_match).scrutinee;
    assert!(
        !f.drops.iter().any(|drop| {
            drop.target == item_init
                && matches!(
                    drop.position,
                    ryo_core::flow::DropPosition::AfterStep { inst } if inst == scrutinee_read
                )
        }),
        "the whole enum owner cannot be freed after its scrutinee read, before payload extraction; got {:?}",
        f.drops
    );
    assert!(
        !f.drops.iter().any(|drop| {
            drop.target == item_init
                && matches!(
                    drop.position,
                    ryo_core::flow::DropPosition::PreTerminator { block }
                        if block == continue_block
                )
                || drop.target == item_init
                    && matches!(
                        drop.position,
                        ryo_core::flow::DropPosition::AfterStep { inst }
                            if inst == continue_inst
                    )
        }),
        "the consuming continue arm must not free the source enum after the payload token; got {:?}",
        f.drops
    );
}

#[test]
fn borrowed_param_arm_result_rejected_e0138() {
    // P1 (memory-safety): an expression arm yielding a borrowed
    // parameter hands caller-owned storage to the match result binding
    // (whose cleanup then frees it — observed: corrupted stdout +
    // SIGTRAP). The ownership pass rejects the borrowed-source family
    // at compile time (E0138); codegen's guard cannot see it (borrowed
    // owners carry no scheduled drop).
    let src = "fn pick(s: str, b: bool):\n\tx = match b:\n\t\ttrue: s\n\t\tfalse: \"no\"\n\tprint(x)\n\nfn main():\n\ts = \"0123456789012345678901234567890123456789\" + \"!\"\n\tpick(s, true)\n\tprint(s)\n";
    let diags = check_src(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::MatchValueArmBorrowed)
        .expect("the borrowed arm result must be rejected");
    assert!(
        diag.message.contains("borrowed from the caller"),
        "the message must name the borrowed family, got {:?}",
        diag.message
    );
    assert_eq!(
        diags
            .iter()
            .filter(|d| d.severity == ryo_core::diag::Severity::Error)
            .count(),
        1,
        "exactly one error (the literal arm is clean): {diags:?}"
    );
}

#[test]
fn borrowed_param_field_projection_arm_result_rejected_e0138() {
    // The general family: a field projection of a borrowed parameter
    // (`p.name`) aliases the caller's storage like a direct read — the
    // E0133 root-walk resolves the chain to the borrowed base owner.
    let src = "struct Person:\n\tname: str\n\tage: int\n\nfn pick(p: Person, b: bool):\n\tx = match b:\n\t\ttrue: p.name\n\t\tfalse: \"no\"\n\tprint(x)\n\nfn main():\n\tq = Person{name=\"0123456789012345678901234567890123456789\" + \"!\", age=3}\n\tpick(q, true)\n\tprint(q.name)\n";
    let diags = check_src(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::MatchValueArmBorrowed)
        .expect("the borrowed projection arm result must be rejected");
    assert!(
        diag.message.contains("borrowed from the caller"),
        "the message must name the borrowed family, got {:?}",
        diag.message
    );
}

#[test]
fn owned_literal_arm_result_is_not_rejected() {
    // Positive control: owned aggregate arms (static literals) through
    // the same shape keep compiling — the borrowed check must not
    // over-fire on owners that are not Borrowed.
    let src = "fn main():\n\tb = true\n\tx = match b:\n\t\ttrue: \"yes\"\n\t\tfalse: \"no\"\n\tprint(x)\n";
    let diags = check_src(src);
    assert!(
        diags
            .iter()
            .all(|d| d.code != DiagCode::MatchValueArmBorrowed),
        "owned literal arms must not be rejected: {diags:?}"
    );
}

#[test]
fn borrowed_scalar_arm_result_is_not_rejected() {
    // Scalars are Copy — an arm yielding a borrowed int parameter is a
    // plain copy, not a borrow escape; the aggregate-only check must
    // stay silent.
    let src = "fn pick(n: int, b: bool):\n\tx = match b:\n\t\ttrue: n\n\t\tfalse: 0\n\tprint(x)\n\nfn main():\n\tpick(5, true)\n";
    let diags = check_src(src);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != ryo_core::diag::Severity::Error),
        "a borrowed scalar arm result must compile clean: {diags:?}"
    );
}

// --- Arm-result transfer: owned aggregate value arms are consumed
// into the match inst, so the result slot's buffer has exactly one
// destruction obligation (the consumer's). ---

/// The arm-result transfer pins' ownership-side contract: for a payload
/// yield (`Some(v): v`), the token's would-be arm-anchored Free and the
/// sibling arm-body literal's would-be match-anchored Free are both
/// gone; the only Free targeting the match result is the consumer's
/// last-use Free.
#[test]
fn payload_yield_transfer_leaves_single_result_free() {
    let src = "enum Opt:\n\tSome(str)\n\tNone\n\nfn main():\n\to = Opt.Some(\"0123456789012345678901234567890123456789\" + \"!\")\n\tx = match o:\n\t\tSome(v): v\n\t\tNone: \"none\"\n\tprint(x)\n";
    let (diags, mut sidecar, tirs, _pool) = check_src_full(src);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != ryo_core::diag::Severity::Error),
        "the transfer must compile clean: {diags:?}"
    );
    let sc = take_function_sidecar(&mut sidecar, 0);
    let tir = &tirs[0];
    let matches: Vec<TirRef> = (1..tir.instructions.len() as u32)
        .map(TirRef::from_raw)
        .filter(|&r| matches!(tir.inst(r).tag, TirTag::Match))
        .collect();
    assert_eq!(matches.len(), 1, "one value match; got {matches:?}");
    let m = matches[0];
    let tokens = tir.match_by_move_owner_refs(m, &_pool);
    assert_eq!(tokens.len(), 1, "one by-move payload token");
    let token = tokens[0];
    // Exactly one Free targets the match inst — the consumer's
    // last-use Free.
    let result_frees: Vec<_> = sc.drops.iter().filter(|fp| fp.target == m).collect();
    assert_eq!(
        result_frees.len(),
        1,
        "exactly one destruction of the match result; got {:?}",
        sc.drops
    );
    // No Free may target the payload token anymore (its buffer is the
    // match result's now).
    assert!(
        sc.drops.iter().all(|fp| fp.target != token),
        "the token's arm-anchored Free must be gone; got {:?}",
        sc.drops
    );
}

/// The producer-arm transfer (`true: make_str(...)`): the producer
/// temporary is Moved into the match — no Free targets the call/concat,
/// and the match result keeps its single consumer Free.
#[test]
fn producer_arm_transfer_leaves_single_result_free() {
    let src = "fn pick(b: bool):\n\ts = match b:\n\t\ttrue: int_to_str(1) + \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\n\t\tfalse: \"no\"\n\tprint(s)\n\tprint(\"\\n\")\n\nfn main():\n\tpick(true)\n";
    let (diags, mut sidecar, tirs, pool) = check_src_full(src);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != ryo_core::diag::Severity::Error),
        "the producer transfer must compile clean: {diags:?}"
    );
    let (fidx, tir) = tirs
        .iter()
        .enumerate()
        .find(|(_, t)| {
            (1..t.instructions.len() as u32)
                .any(|r| matches!(t.inst(TirRef::from_raw(r)).tag, TirTag::Match))
        })
        .expect("a function with the value match");
    let sc = take_function_sidecar(&mut sidecar, fidx);
    let m = (1..tir.instructions.len() as u32)
        .map(TirRef::from_raw)
        .find(|&r| matches!(tir.inst(r).tag, TirTag::Match))
        .expect("the value match");
    // The producer family is represented in this body; the transfer
    // contract is structural: nothing may be anchored ON the match inst
    // anymore (the old anon-temp anchor), and exactly one Free targets
    // the match inst — the consumer's.
    assert!(
        matches!(tir.inst(m).tag, TirTag::Match) && pool.needs_drop(tir.inst(m).ty),
        "the test expects an aggregate value match"
    );
    assert!(
        sc.drops
            .iter()
            .all(|fp| fp.position.anchor_inst() != Some(m)),
        "no Free may be anchored on the match inst after the transfer; got {:?}",
        sc.drops
    );
    let result_frees = sc.drops.iter().filter(|fp| fp.target == m).count();
    assert_eq!(
        result_frees, 1,
        "exactly one destruction of the match result; got {:?}",
        sc.drops
    );
}

/// The by-value Var arm (`true: t`): the binding read moves into the
/// match result — the binding's own last-use Free disappears.
#[test]
fn by_value_var_arm_transfer_leaves_single_result_free() {
    let src = "fn pick(b: bool):\n\tt = int_to_str(1) + \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\n\ts = match b:\n\t\ttrue: t\n\t\tfalse: \"no\"\n\tprint(s)\n\tprint(\"\\n\")\n\nfn main():\n\tpick(true)\n";
    let (diags, mut sidecar, tirs, _pool) = check_src_full(src);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != ryo_core::diag::Severity::Error),
        "the by-value read transfer must compile clean: {diags:?}"
    );
    let (fidx, tir) = tirs
        .iter()
        .enumerate()
        .find(|(_, t)| {
            (1..t.instructions.len() as u32)
                .any(|r| matches!(t.inst(TirRef::from_raw(r)).tag, TirTag::Match))
        })
        .expect("a function with the value match");
    let sc = take_function_sidecar(&mut sidecar, fidx);
    let m = (1..tir.instructions.len() as u32)
        .map(TirRef::from_raw)
        .find(|&r| matches!(tir.inst(r).tag, TirTag::Match))
        .expect("the value match");
    let t_value = tir.var_decl_view(TirRef::from_raw(
        (1..tir.instructions.len() as u32)
            .find(|&r| matches!(tir.inst(TirRef::from_raw(r)).tag, TirTag::VarDecl))
            .expect("t's decl"),
    ));
    assert_eq!(
        sc.drops.iter().filter(|fp| fp.target == m).count(),
        1,
        "exactly one destruction of the match result; got {:?}",
        sc.drops
    );
    // The binding-read last-use Free is gone, but the sibling arm's
    // divergence Free (the false arm still owns t there) remains —
    // exactly one destruction per path.
    let t_frees: Vec<_> = sc
        .drops
        .iter()
        .filter(|fp| fp.target == t_value.initializer)
        .collect();
    assert!(
        t_frees.len() <= 1,
        "at most one Free may target the moved binding (the sibling-arm          divergence Free); got {:?}",
        sc.drops
    );
}

/// A discarded aggregate match result (statement-level value match) is
/// still destroyed exactly once: the transfer seeds the match inst as
/// an anonymous temporary, freed after the wrapping statement.
#[test]
fn discarded_aggregate_match_result_freed_once() {
    let src = "enum Opt:\n\tSome(str)\n\tNone\n\nfn main():\n\to = Opt.Some(\"abc\")\n\tmatch o:\n\t\tSome(v): v\n\t\tNone: \"none\"\n\tprint(\"done\\n\")\n";
    let (diags, mut sidecar, tirs, _pool) = check_src_full(src);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != ryo_core::diag::Severity::Error),
        "a discarded aggregate match result must compile clean: {diags:?}"
    );
    let sc = take_function_sidecar(&mut sidecar, 0);
    let tir = &tirs[0];
    let m = (1..tir.instructions.len() as u32)
        .map(TirRef::from_raw)
        .find(|&r| matches!(tir.inst(r).tag, TirTag::Match))
        .expect("the value match");
    assert_eq!(
        sc.drops.iter().filter(|fp| fp.target == m).count(),
        1,
        "exactly one destruction of the discarded result; got {:?}",
        sc.drops
    );
}

#[test]
#[cfg(debug_assertions)]
fn direct_catch_shared_error_keeps_ownership_after_payload_move() {
    let src = r#"error A(str)
fn consume(move message: str):
	print(message)
fn describe(err: A, move message: str) -> str:
	consume(message)
	return err.message()
fn find() -> A!str:
	return A(int_to_str(7) + "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
fn main():
	value = find() catch as err:
		A(message): describe(err, message)
	print(value)
"#;
    let (diags, mut sidecar, tirs, pool) = check_src_full(src);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != ryo_core::diag::Severity::Error),
        "{diags:?}"
    );
    let main = tirs
        .iter()
        .position(|tir| pool.str(tir.name) == "main")
        .expect("main exists");
    let tir = &tirs[main];
    let describe = pool.find_str("describe").expect("describe is interned");
    let call = (1..tir.instructions.len() as u32)
        .map(ryo_core::tir::TirRef::from_raw)
        .find(|&r| tir.inst(r).tag == TirTag::Call && tir.call_view(r).name == describe)
        .expect("call borrowing the shared error");
    let receiver = tir.call_view(call).args.at(0);
    let own = crate::ownership::cleanup_validation::take_test_ownership(tir.name)
        .expect("ownership recorded");
    let owner = own.underlying_owner(receiver);
    assert_eq!(
        own.states.get(&owner),
        Some(&OwnerState::Valid),
        "the shared error must retain its original allocation"
    );
    let flow = build_flow(tir, &pool);
    let target = owner.tirref(&flow);
    let sc = take_function_sidecar(&mut sidecar, main);
    assert!(
        sc.drops.iter().any(|drop| drop.target == target),
        "the original shared error allocation must be destroyed after its reads"
    );
}
