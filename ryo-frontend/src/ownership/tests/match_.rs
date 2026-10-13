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
    // Union merge machinery: the scrutinee survives the `_` arm, so a
    // branch-gated Free of the scrutinee owner must be scheduled for
    // that arm (the whole-enum free covers the runtime payload).
    let f = main_sidecar(&mut sidecar, &pool);
    let m = find_match(&tirs[0]);
    let arms = f.match_branches[m.index()]
        .as_ref()
        .expect("match_branches entry for the Match inst")
        .arms
        .clone();
    assert_eq!(arms.len(), 2, "one BranchId per arm");
    let gated: Vec<_> = f
        .free_schedule
        .iter()
        .filter(|fp| fp.branch.is_some())
        .collect();
    assert_eq!(
        gated.len(),
        1,
        "exactly one arm-gated Free (the scrutinee on the untouched arm); got {gated:?}"
    );
    assert_eq!(
        gated[0].branch,
        Some(arms[1]),
        "the gated Free belongs to the `_` arm"
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
    // drops, no branch-gated frees, and the scrutinee is not consumed.
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
        f.match_arm_drops.is_empty(),
        "borrowed scrutinee: wildcard payload fields drop nothing; got {:?}",
        f.match_arm_drops
    );
    assert!(
        f.free_schedule.iter().all(|fp| fp.branch.is_none()),
        "borrowed scrutinee: no arm-gated frees; got {:?}",
        f.free_schedule
    );
}

#[test]
fn anon_owned_partial_move_registers_arm_start_drop() {
    // Bullet 5: anon-pattern match on an owned anonymous struct. The arm
    // binds `name` by move (consuming the scrutinee on that arm) and
    // wildcards `tag`; the unbound needs-drop field registers as an
    // arm-start drop for codegen (codegen emits it at arm start). The
    // `_` arm leaves
    // the scrutinee valid — the union merge stamps it moved after the
    // match, so a branch-gated whole-value Free covers that arm.
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
    let arms = f.match_branches[m.index()]
        .as_ref()
        .expect("match_branches entry for the Match inst")
        .arms
        .clone();
    assert_eq!(arms.len(), 2, "one BranchId per arm");
    assert_eq!(
        f.match_arm_drops.len(),
        1,
        "the unbound `tag` field registers exactly one arm-start drop; got {:?}",
        f.match_arm_drops
    );
    let drop = &f.match_arm_drops[0];
    assert_eq!(drop.match_inst, m);
    assert_eq!(drop.arm, arms[0], "the drop belongs to the consuming arm");
    assert_eq!(drop.field_index, 1, "`tag` is field 1 in declaration order");
    assert_eq!(drop.ty, pool.str_(), "the dropped field is the str payload");
    let gated: Vec<_> = f
        .free_schedule
        .iter()
        .filter(|fp| fp.branch.is_some())
        .collect();
    assert_eq!(
        gated.len(),
        1,
        "exactly one arm-gated Free (the scrutinee on the `_` arm); got {gated:?}"
    );
    assert_eq!(gated[0].branch, Some(arms[1]));
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
    let arms = f.match_branches[m.index()]
        .as_ref()
        .expect("match_branches entry for the Match inst")
        .arms
        .clone();
    assert_eq!(arms.len(), 1, "single exhaustive arm");
    assert_eq!(
        f.match_arm_drops.len(),
        1,
        "the wildcard str payload registers one arm-start drop; got {:?}",
        f.match_arm_drops
    );
    let drop = &f.match_arm_drops[0];
    assert_eq!(drop.arm, arms[0]);
    assert_eq!(drop.field_index, 1);
    assert_eq!(drop.ty, pool.str_());
    // The bound field `x` registered as a fresh owner keyed on its
    // owner-token FieldAccess (the inst immediately before the Match);
    // its last-use Free is anchored after the in-arm `print(x)` read.
    let token = ryo_core::tir::TirRef::from_raw((m.index() - 1) as u32);
    assert!(
        matches!(tirs[0].inst(token).tag, ryo_core::tir::TirTag::FieldAccess),
        "the owner token is the FieldAccess before the Match"
    );
    assert!(
        f.free_schedule.iter().any(|fp| fp.target == token),
        "the bound field's owner-token Free is scheduled; got {:?}",
        f.free_schedule
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
    let (diags, mut sidecar, _tirs, pool) = check_src_full(src);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != ryo_core::diag::Severity::Error),
        "no-consumption match must be error-free; got {diags:?}"
    );
    let f = main_sidecar(&mut sidecar, &pool);
    assert!(
        f.match_arm_drops.is_empty(),
        "no consuming arm → no arm-start drops; got {:?}",
        f.match_arm_drops
    );
    assert!(
        f.free_schedule.iter().all(|fp| fp.branch.is_none()),
        "no conditional moves → no arm-gated frees; got {:?}",
        f.free_schedule
    );
    assert!(
        !f.free_schedule.is_empty(),
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
    let token_frees: Vec<_> = f
        .free_schedule
        .iter()
        .filter(|fp| fp.target == token)
        .collect();
    assert_eq!(
        token_frees.len(),
        1,
        "exactly one Free for the dead arm bind (the return epilogue); got {token_frees:?}"
    );
    assert!(
        token_frees[0].branch.is_none(),
        "the epilogue Free is ungated, anchored at the return"
    );
    assert!(
        diags.iter().any(|d| d.code == DiagCode::DeadStore),
        "the never-read bind still warns W0001; got {diags:?}"
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
    use chumsky::span::{SimpleSpan, Span as _};
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
    let span = SimpleSpan::new((), 0..0);
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let ov = tb.var(o, opt_ty, span);
    // A payload field read off the enum value, consumed by a VarDecl —
    // the guard's enum arm must reject it.
    let fa = tb.field_access(ov, 0, str_ty, span);
    let decl = tb.var_decl(x, false, str_ty, fa, span);
    let tir = tb.finish(&[decl]);
    let mut sink = DiagSink::new();
    let _sc = check(std::slice::from_ref(&tir), &pool, &mut sink);
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
        f.free_schedule
            .iter()
            .any(|fp| fp.target == init && fp.after == brk),
        "the break inside the match arm gets a loop-exit Free for the \
         loop-local str; got {:?}",
        f.free_schedule
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
        f.free_schedule
            .iter()
            .any(|fp| fp.target == init && fp.after == brk),
        "the break inside the match arm gets a loop-exit Free for the \
         loop-local str even when the sibling arm is an expression arm; \
         got {:?}",
        f.free_schedule
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
        f.free_schedule
            .iter()
            .filter(|fp| fp.target == init)
            .count(),
        1,
        "exactly one Free for the arm-local named str; got {:?}",
        f.free_schedule
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
        f.free_schedule
            .iter()
            .any(|fp| fp.target == init && fp.after == brk),
        "the break inside the value-position match arm gets a loop-exit \
         Free for the loop-local str; got {:?}",
        f.free_schedule
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
        f.promotion_frees.iter().any(|pf| pf.after == inner_return),
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
    let arms = f.match_branches[m.index()]
        .as_ref()
        .expect("match_branches entry for the Match inst")
        .arms
        .clone();
    let token = tirs[0]
        .match_by_move_owner_refs(m, &pool)
        .into_iter()
        .next()
        .expect("one by-move owner token for `v`");
    let body = match tirs[0].match_view(m).arms().next().unwrap().body {
        ryo_core::tir::MatchBodyView::Expr(e) => e,
        ryo_core::tir::MatchBodyView::Stmts(_) => panic!("`Some(v) => 1` is an expression arm"),
    };
    let frees: Vec<_> = f
        .free_schedule
        .iter()
        .filter(|fp| fp.target == token)
        .collect();
    assert_eq!(
        frees.len(),
        1,
        "exactly one Free for the dead payload bind; got {frees:?}"
    );
    assert_eq!(frees[0].branch, Some(arms[0]), "gated to the `Some(v)` arm");
    assert_eq!(
        frees[0].after, body,
        "anchored at the arm-body expression (fired inside the arm)"
    );
}

#[test]
fn borrowed_param_arm_result_rejected_i210() {
    // P1 (memory-safety): an expression arm yielding a borrowed
    // parameter hands caller-owned storage to the match result binding
    // (whose cleanup then frees it — observed: corrupted stdout +
    // SIGTRAP). The ownership pass rejects the borrowed-source family
    // at compile time, naming I-210; codegen's guard cannot see it
    // (borrowed owners carry no scheduled drop).
    let src = "fn pick(s: str, b: bool):\n\tx = match b:\n\t\ttrue: s\n\t\tfalse: \"no\"\n\tprint(x)\n\nfn main():\n\ts = \"0123456789012345678901234567890123456789\" + \"!\"\n\tpick(s, true)\n\tprint(s)\n";
    let diags = check_src(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::MatchValueArmBorrowed)
        .expect("the borrowed arm result must be rejected");
    assert!(
        diag.message.contains("I-210"),
        "the message must name I-210, got {:?}",
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
fn borrowed_param_field_projection_arm_result_rejected_i210() {
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
        diag.message.contains("I-210"),
        "the message must name I-210, got {:?}",
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
