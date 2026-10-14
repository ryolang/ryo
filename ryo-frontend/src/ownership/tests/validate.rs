//! Debug-only cleanup-validator pins: the debug-only cleanup-completeness validator
//! (`ownership::validate`) accepts every legitimate schedule shape and
//! panics on hand-corrupted ones. Every fixture also exercises the
//! validator implicitly: `check` runs it at the end of
//! `analyze_function` for every body in debug builds, so a fixture that
//! trips an invariant fails here regardless of the explicit calls.

use super::super::validate as cv;
use super::super::*;
use super::common::*;

/// Positive pin: `check` + both own-free checks accept a straight-line
/// last-use free.
#[test]
fn validator_accepts_straight_line_last_use() {
    let (diags, mut sidecar, tirs, pool) =
        check_src_full("fn main():\n\ts = \"hello\"\n\tprint(s)\n");
    assert!(diags.is_empty(), "clean program; got {diags:?}");
    let sc = take_function_sidecar(&mut sidecar, 0);
    assert!(
        !sc.drops.is_empty(),
        "fixture must schedule at least one free"
    );
    let flow = build_flow(&tirs[0], &pool);
    let places = cv::TargetPlaces::build(&tirs[0], &flow, &pool);
    let fire_points = ryo_core::flow::fire_points::FirePoints::new(&flow);
    cv::validate_positions(&flow, &fire_points, &sc);
    cv::validate_no_duplicate_destruction(&places, &flow, &sc);
}

/// Positive pin: if/else with borrows in both arms.
#[test]
fn validator_accepts_if_else_borrows() {
    let src = "fn main():\n\ts = \"a\"\n\tif true:\n\t\tprint(s)\n\telse:\n\t\tprint(s)\n";
    let (diags, mut sidecar, tirs, pool) = check_src_full(src);
    assert!(diags.is_empty(), "clean program; got {diags:?}");
    let sc = take_function_sidecar(&mut sidecar, 0);
    let flow = build_flow(&tirs[0], &pool);
    let places = cv::TargetPlaces::build(&tirs[0], &flow, &pool);
    let fire_points = ryo_core::flow::fire_points::FirePoints::new(&flow);
    cv::validate_positions(&flow, &fire_points, &sc);
    cv::validate_no_duplicate_destruction(&places, &flow, &sc);
}

/// Positive pin: match with a by-move arm binding (token places +
/// arm-start drops). `Both(x, _)` moves field 0 out of the owned enum,
/// so the wildcard field 1 registers an arm-start drop.
#[test]
fn validator_accepts_match_by_move_arm() {
    let src = "enum Pair:\n\tBoth(str, str)\n\nfn main():\n\tp = Pair.Both(\"a\", \"b\")\n\tmatch p:\n\t\tBoth(x, _):\n\t\t\tprint(x)\n";
    let (diags, mut sidecar, tirs, pool) = check_src_full(src);
    assert!(diags.is_empty(), "clean program; got {diags:?}");
    let sc = take_function_sidecar(&mut sidecar, 0);
    assert!(
        !block_start_drops(&sc).is_empty(),
        "fixture must schedule at least one arm drop"
    );
    let flow = build_flow(&tirs[0], &pool);
    let places = cv::TargetPlaces::build(&tirs[0], &flow, &pool);
    let fire_points = ryo_core::flow::fire_points::FirePoints::new(&flow);
    cv::validate_positions(&flow, &fire_points, &sc);
    cv::validate_no_duplicate_destruction(&places, &flow, &sc);
}

/// Positive pin: loop body with a per-iteration move free + loop-exit
/// frees.
#[test]
fn validator_accepts_loop_body_frees() {
    let src =
        "fn main():\n\tmut i = 0\n\twhile i < 3:\n\t\ts = \"x\"\n\t\tprint(s)\n\t\ti = i + 1\n";
    let (diags, mut sidecar, tirs, pool) = check_src_full(src);
    assert!(diags.is_empty(), "clean program; got {diags:?}");
    let sc = take_function_sidecar(&mut sidecar, 0);
    let flow = build_flow(&tirs[0], &pool);
    let places = cv::TargetPlaces::build(&tirs[0], &flow, &pool);
    let fire_points = ryo_core::flow::fire_points::FirePoints::new(&flow);
    cv::validate_positions(&flow, &fire_points, &sc);
    cv::validate_no_duplicate_destruction(&places, &flow, &sc);
}

/// Negative pin (positions): a drop firing in a block the Flow does not
/// have is a violation.
#[test]
fn validator_rejects_drop_in_nonexistent_block() {
    let (diags, mut sidecar, tirs, pool) =
        check_src_full("fn main():\n\ts = \"hello\"\n\tprint(s)\n");
    assert!(diags.is_empty(), "clean program; got {diags:?}");
    let mut sc = take_function_sidecar(&mut sidecar, 0);
    let flow = build_flow(&tirs[0], &pool);
    let real = sc.drops[0];
    sc.drops.push(DropOp {
        position: DropPosition::PreTerminator {
            block: ryo_core::flow::BlockId(999),
        },
        target: real.target,
        span: real.span,
    });
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let fire_points = ryo_core::flow::fire_points::FirePoints::new(&flow);
        cv::validate_positions(&flow, &fire_points, &sc);
    }));
    assert!(r.is_err(), "a PreTerminator drop in block 999 must panic");
}

/// Negative pin (positions): a conditional dead drop listing a block
/// OUTSIDE its IfStmt's recorded cascade arms is a violation. The entry
/// block (the cascade's condition block) is never an arm body.
#[test]
fn validator_rejects_conditional_dead_drop_outside_recorded_arms() {
    let src = "fn main():\n\tmut s = \"a\"\n\tif s.len() > 0:\n\t\ts = \"b\"\n";
    let (diags, mut sidecar, tirs, pool) = check_src_full(src);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != ryo_core::diag::Severity::Error),
        "clean program (warnings allowed — s is dead after); got {diags:?}"
    );
    let mut sc = take_function_sidecar(&mut sidecar, 0);
    let flow = build_flow(&tirs[0], &pool);
    let if_stmt = (1..tirs[0].instructions.len())
        .map(|i| ryo_core::tir::TirRef::from_raw(u32::try_from(i).expect("index fits u32")))
        .find(|&r| tirs[0].inst(r).tag == ryo_core::tir::TirTag::IfStmt)
        .expect("an IfStmt exists");
    sc.drops.push(ryo_core::flow::DropOp {
        position: ryo_core::flow::DropPosition::BlockStart { block: flow.entry },
        target: sc.drops[0].target,
        span: tirs[0].span(if_stmt),
    });
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let fire_points = ryo_core::flow::fire_points::FirePoints::new(&flow);
        cv::validate_positions(&flow, &fire_points, &sc);
    }));
    assert!(
        r.is_err(),
        "a block-start dead drop on the entry block (not a recorded arm, not a match arm) must panic"
    );
}

/// Negative pin (duplicates): two DIFFERENT target TirRefs resolving to
/// the SAME place at the SAME position escape codegen's raw-TirRef
/// dedup — a double-free. The corruption finds two instructions that
/// alias one place (a binding's `VarDecl` and a `Var` read of it share
/// one place) and schedules both at one position.
#[test]
fn validator_rejects_distinct_targets_aliasing_one_place_at_one_position() {
    let (diags, mut sidecar, tirs, pool) =
        check_src_full("fn main():\n\ts = \"hello\"\n\tprint(s)\n");
    assert!(diags.is_empty(), "clean program; got {diags:?}");
    let mut sc = take_function_sidecar(&mut sidecar, 0);
    let flow = build_flow(&tirs[0], &pool);
    let places = cv::TargetPlaces::build(&tirs[0], &flow, &pool);
    // Any two distinct instructions naming one place.
    let refs: Vec<ryo_core::tir::TirRef> = (1..tirs[0].instructions.len())
        .map(|i| ryo_core::tir::TirRef::from_raw(u32::try_from(i).expect("index fits u32")))
        .collect();
    let mut pair = None;
    'outer: for (a, &r1) in refs.iter().enumerate() {
        for &r2 in &refs[a + 1..] {
            if places.place(&flow, r1).is_some()
                && places.place(&flow, r1) == places.place(&flow, r2)
            {
                pair = Some((r1, r2));
                break 'outer;
            }
        }
    }
    let Some((r1, r2)) = pair else {
        panic!("fixture no longer has two instructions aliasing one place");
    };
    let pos = sc.drops[0].position;
    let span = sc.drops[0].span;
    sc.drops.push(DropOp {
        position: pos,
        target: r1,
        span,
    });
    sc.drops.push(DropOp {
        position: pos,
        target: r2,
        span,
    });
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        cv::validate_no_duplicate_destruction(&places, &flow, &sc);
    }));
    assert!(
        r.is_err(),
        "two distinct targets aliasing one place at one position must panic"
    );
}

/// Positive pin (duplicates): the ownership pass's deliberate
/// double-anchor (consumer + return epilogue naming the SAME target at
/// the SAME position) is deduped by codegen's per-bucket `freed_targets`
/// check — `codegen/frees.rs` documents it as a no-op by design, so the
/// validator must accept it. Fixture: an unconsumed move param — the
/// param policy Free and the return epilogue both anchor after the
/// return statement.
#[test]
fn validator_accepts_same_target_double_anchor_by_design() {
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::{ParamMode, TirBuilder, TirParam};
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let void = pool.void();
    let consume = pool.intern_str("consume");
    let s_name = pool.intern_str("s");
    let span = SimpleSpan::new((), 0..0);
    let mut tb = TirBuilder::new(
        consume,
        vec![TirParam {
            name: s_name,
            ty: str_ty,
            mode: ParamMode::Move,
            span,
        }],
        void,
        span,
    );
    let ret = tb.return_void(void, span);
    let tir = tb.finish(&[ret]);
    let mut sink = DiagSink::new();
    let mut sidecar = check(
        std::slice::from_ref(&tir),
        &[build_flow(&tir, &pool)],
        &pool,
        &mut sink,
    );
    assert!(sink.is_empty(), "clean body; got {:?}", sink.into_diags());
    let sc = take_function_sidecar(&mut sidecar, 0);
    // The fixture's whole point: the schedule really does contain the
    // same-target same-position pair.
    let anchors: Vec<_> = sc
        .drops
        .iter()
        .filter(|d| d.target == ryo_core::tir::TirRef::param(0))
        .map(|d| d.position)
        .collect();
    assert!(
        anchors.len() >= 2 && anchors.windows(2).any(|w| w[0] == w[1]),
        "fixture must contain the by-design double-anchor; got {anchors:?}"
    );
    let flow = build_flow(&tir, &pool);
    let places = cv::TargetPlaces::build(&tir, &flow, &pool);
    let fire_points = ryo_core::flow::fire_points::FirePoints::new(&flow);
    cv::validate_positions(&flow, &fire_points, &sc);
    cv::validate_no_duplicate_destruction(&places, &flow, &sc);
}

// ─── Path-aware obligation validation (P2a) ─────────────────────────────

/// Test helper: run `check` on `src`, then hand the caller the captured
/// final ownership state, the rebuilt flow, and the extracted sidecar
/// for `name` — `corrupt` mutates the sidecar before the path checks.
fn path_check_harness(
    src: &str,
    name: &str,
) -> (
    Vec<ryo_core::tir::Tir>,
    InternPool,
    ryo_core::tir::TirRef,
    Flow,
    Ownership,
    FunctionSidecar,
) {
    let (diags, mut sidecar, tirs, pool) = check_src_full(src);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != ryo_core::diag::Severity::Error),
        "fixture must compile clean: {diags:?}"
    );
    let idx = tirs
        .iter()
        .position(|t| pool.str(t.name) == name)
        .expect("fixture function exists");
    let own = cv::take_test_ownership(tirs[idx].name).expect("captured ownership");
    let sc = take_function_sidecar(&mut sidecar, idx);
    let flow = build_flow(&tirs[idx], &pool);
    let main_ref = tirs[idx]
        .body_stmts()
        .first()
        .copied()
        .unwrap_or(ryo_core::tir::TirRef::from_raw(1));
    (tirs, pool, main_ref, flow, own, sc)
}

fn run_path_checks(
    tirs: &[ryo_core::tir::Tir],
    pool: &InternPool,
    flow: &Flow,
    own: &Ownership,
    sc: &FunctionSidecar,
    no_escapes: bool,
) {
    let tir = tirs
        .iter()
        .find(|t| build_flow(t, pool).blocks.len() == flow.blocks.len())
        .expect("the fixture's tir");
    let places = cv::TargetPlaces::build(tir, flow, pool);
    let idx = cv::StepIndex::build(flow);
    let fire_points = ryo_core::flow::fire_points::FirePoints::new(flow);
    let events = cv::collect_events(tir, &places, flow, &fire_points, sc, own);
    let table = cv::generation_table(tir, flow, pool, &idx, sc);
    let mut summary = cv::ValidationSummary::default();
    cv::validate_no_path_double_destruction(tir, flow, own, sc, &events, &table, &mut summary);
    cv::validate_path_coverage(tir, flow, pool, own, sc, &events, &table, &mut summary);
    crate::ownership::gaps::validate_view_protection(
        tir,
        flow,
        own,
        sc,
        &events,
        &table,
        &mut summary,
    );
    crate::ownership::gaps::validate_displacement(tir, flow, own, sc, &mut summary);
    crate::ownership::gaps::validate_promo(flow, &fire_points, sc, &mut summary);
    let _ = no_escapes; // the escape predicate is gone — coverage is per-path events
}

/// Negative pin (reviewer a): cleanup removed from one branch while the
/// sibling branch retains its drop. `mut s = "a"; if true: s = "b"` —
/// the pre-branch buffer's fall-through-path drop (a BlockStart
/// conditional dead drop) is removed: the fall-through path leaks with
/// no escape.
#[test]
fn path_coverage_flags_branch_cleanup_removed() {
    let src = "fn main():\n\tmut s = \"a\"\n\tif s.len() > 0:\n\t\ts = \"b\"\n";
    let (tirs, pool, _main, flow, own, mut sc) = path_check_harness(src, "main");
    // Remove the BlockStart conditional dead drop (the sibling branch's
    // fall-through cleanup); keep everything else.
    sc.drops
        .retain(|d| !matches!(d.position, DropPosition::BlockStart { .. }));
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_path_checks(&tirs, &pool, &flow, &own, &sc, true);
    }));
    assert!(
        r.is_err(),
        "removing one branch's cleanup while the sibling keeps its drop must flag"
    );
    // Positive pin: the intact schedule is accepted (real escapes).
    let (tirs, pool, _main, flow, own, sc) = path_check_harness(src, "main");
    run_path_checks(&tirs, &pool, &flow, &own, &sc, false);
}

/// Negative pin (reviewer b): destruction scheduled before a branch AND
/// again at its reachable join — the join copy is reachable from the
/// pre-branch event with no intervening write, so both fire on one
/// path. Constructed by cloning the conditional dead drop into the
/// post-if join block (both BlockStart events — exact positions). The
/// buffer is a concatenation result, NOT a static literal: the
/// elided-literal exemption (a twice-scheduled literal emits nothing)
/// would correctly pass a literal pair — the corruption this pin guards
/// must carry a real target.
#[test]
fn path_double_destruction_flags_before_branch_and_at_join() {
    let src = "fn main():\n\tmut s = \"a\" + \"b\"\n\tif s.len() > 0:\n\t\ts = \"c\" + \"d\"\n";
    let (tirs, pool, _main, flow, own, mut sc) = path_check_harness(src, "main");
    // The reassign's value is the binding's last write; its free is the
    // one to clone. Find the Assign step's value inst, then its free.
    let tir = tirs
        .iter()
        .find(|t| pool.str(t.name) == "main")
        .expect("the fixture's body");
    let reassign = (1..tir.instructions.len() as u32)
        .map(ryo_core::tir::TirRef::from_raw)
        .find(|&r| matches!(tir.inst(r).tag, ryo_core::tir::TirTag::Assign))
        .expect("an Assign exists");
    let last_value = tir.assign_view(reassign).value;
    let last_use = *sc
        .drops
        .iter()
        .find(|d| d.target == last_value)
        .expect("the last-write value's free");
    let arm_block = flow
        .step_block_of(reassign)
        .expect("the reassign is a step");
    // The clone fires at the arm's continuation (the reseated value is
    // still current there — a different block than the original free,
    // so codegen's per-block dedup cannot consume it), and the arm
    // flows straight into it — both fire on one path, a real
    // double-free.
    let join = flow
        .successors(arm_block)
        .first()
        .copied()
        .expect("the arm falls through to the join");
    sc.drops.push(DropOp {
        position: DropPosition::BlockStart { block: join },
        target: last_use.target,
        span: last_use.span,
    });
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_path_checks(&tirs, &pool, &flow, &own, &sc, false);
    }));
    assert!(
        r.is_err(),
        "destruction before a branch and again at its reachable join must flag"
    );
}

/// Negative pin (reviewer c): a return-path drop removed while the
/// recorded return-epilogue observation remains — the observation is
/// NOT an escape; the return path leaks.
#[test]
fn path_coverage_flags_return_path_drop_removed() {
    let src = "fn main():\n\ts = \"a\"\n\tif true:\n\t\treturn\n\tprint(s)\n";
    let (tirs, pool, _main, flow, own, mut sc) = path_check_harness(src, "main");
    // The epilogue free is the drop anchored on the return (a terminator
    // payload — the anchor is not a stepped instruction). The
    // fall-through last-use free's anchor IS a step — keep it.
    let idx = cv::StepIndex::build(&flow);
    let before = sc.drops.len();
    sc.drops.retain(|d| match d.position {
        DropPosition::AfterStep { inst } => idx.get(inst).is_some(),
        _ => true,
    });
    assert_eq!(
        sc.drops.len(),
        before - 1,
        "exactly the epilogue drop is removed"
    );
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_path_checks(&tirs, &pool, &flow, &own, &sc, true);
    }));
    assert!(
        r.is_err(),
        "removing the return-path drop while the epilogue observation remains must flag"
    );
    // Positive pin: the intact schedule (both paths covered) passes.
    let (tirs, pool, _main, flow, own, sc) = path_check_harness(src, "main");
    run_path_checks(&tirs, &pool, &flow, &own, &sc, false);
}

/// Positive pin: sibling-arm one-drop-per-arm and conditional events on
/// disjoint paths are NOT flagged.
#[test]
fn path_checks_accept_sibling_arm_drops() {
    let src = "fn main():\n\ts = \"a\"\n\tif true:\n\t\tprint(s)\n\telse:\n\t\tprint(s)\n";
    let (tirs, pool, _main, flow, own, sc) = path_check_harness(src, "main");
    run_path_checks(&tirs, &pool, &flow, &own, &sc, false);
}

/// Positive pin: the designed same-target double-anchor (consumer +
/// return epilogue at the same position) is not flagged by the path
/// checks.
#[test]
fn path_checks_accept_by_design_double_anchor() {
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::{ParamMode, TirBuilder, TirParam};
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let void = pool.void();
    let consume = pool.intern_str("consume");
    let s_name = pool.intern_str("s");
    let span = SimpleSpan::new((), 0..0);
    let mut tb = TirBuilder::new(
        consume,
        vec![TirParam {
            name: s_name,
            ty: str_ty,
            mode: ParamMode::Move,
            span,
        }],
        void,
        span,
    );
    let ret = tb.return_void(void, span);
    let tir = tb.finish(&[ret]);
    let mut sink = DiagSink::new();
    let flow = build_flow(&tir, &pool);
    let mut sidecar = check(std::slice::from_ref(&tir), &[flow], &pool, &mut sink);
    assert!(sink.is_empty(), "clean body; got {:?}", sink.into_diags());
    let own = cv::take_test_ownership(tir.name).expect("captured ownership");
    let sc = take_function_sidecar(&mut sidecar, 0);
    let flow = build_flow(&tir, &pool);
    run_path_checks(std::slice::from_ref(&tir), &pool, &flow, &own, &sc, false);
}

/// Gap 1 pin: the `choose` reproducer. A value returned on one path
/// and destroyed on the sibling path: coverage discharges the
/// obligation per PATH — the return transfer covers the taken path,
/// the last-use release covers the false path. Removing the
/// false-path cleanup leaves a leak the must-analysis proves.
#[test]
fn path_coverage_choose_false_path_cleanup_removed() {
    let src = "fn choose(flag: bool) -> str:\n\ts = \"hello\"\n\tif flag:\n\t\treturn s\n\tprint(s)\n\treturn \"fallback\"\n";
    // Intact schedule: accepted (both paths discharged).
    let (_t, _p, _m, flow, own, sc) = path_check_harness(src, "choose");
    run_path_checks(&_t, &_p, &flow, &own, &sc, false);
    // Hand corruption: drop every step-anchored free (the false-path
    // last-use cleanup; the return transfers are payload-anchored and
    // stay). The false path now leaks — PROVEN INVALID.
    let idx = cv::StepIndex::build(&flow);
    let mut sc = sc;
    let before = sc.drops.len();
    sc.drops.retain(|d| match d.position {
        DropPosition::AfterStep { inst } => idx.get(inst).is_none(),
        _ => true,
    });
    assert!(
        sc.drops.len() < before,
        "the corruption must remove the false-path cleanup"
    );
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_path_checks(&_t, &_p, &flow, &own, &sc, false);
    }));
    assert!(
        r.is_err(),
        "removing the false-path cleanup of a branch-returned value must flag"
    );
}

/// Gap 4 pin (UAF direction): a base release cloned BEFORE a reachable
/// view use — the free can fire before the view's final read.
#[test]
fn view_protection_flags_release_before_view_use() {
    let src = "fn main():\n\ts = \"hello world\"\n\tv = s[0:5]\n\tprint(v)\n\tprint(s)\n";
    let (_t, _p, _m, flow, own, mut sc) = path_check_harness(src, "main");
    // The base's last-use free (after `print(s)`) cloned to the
    // block START — before the `print(v)` read of the view.
    let base_free = *sc
        .drops
        .iter()
        .find(|d| matches!(d.position, DropPosition::AfterStep { .. }))
        .expect("the base's last-use free");
    let view_read_block = flow
        .forward_order()
        .find(|b| {
            flow.block_steps(*b).iter().any(|s| {
                let tag = _t
                    .iter()
                    .find(|t| {
                        // the fixture's single body
                        !t.body_stmts().is_empty()
                    })
                    .map(|t| t.inst(s.inst).tag);
                matches!(tag, Some(ryo_core::tir::TirTag::Var))
            })
        })
        .expect("a block holds the view read");
    sc.drops.push(DropOp {
        position: DropPosition::BlockStart {
            block: view_read_block,
        },
        target: base_free.target,
        span: base_free.span,
    });
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_path_checks(&_t, &_p, &flow, &own, &sc, false);
    }));
    assert!(
        r.is_err(),
        "a base release before a reachable view use must flag (UAF direction)"
    );
}

/// Gap 6 pin: a generation-ending write establishes the new generation
/// but is NOT a release — the old generation needs its displacement
/// release. Removing the release while the write stays leaves the old
/// buffer unreleased; the walk-recorded Mutate hazard is the
/// corruption-proof evidence.
#[test]
fn displacement_release_removed_while_write_stays() {
    let src = "fn main():\n\tmut s = \"a\" + \"x\"\n\ts = \"b\" + \"y\"\n\tprint(s)\n";
    let (_t, _p, _m, flow, own, sc) = path_check_harness(src, "main");
    // Intact: accepted (the displacement release exists).
    run_path_checks(&_t, &_p, &flow, &own, &sc, false);
    // Corruption: remove the displacement release (keep the write).
    let mut sc = sc;
    let before = sc.free_on_reassign.iter().flatten().count();
    assert!(before > 0, "fixture must schedule a displacement release");
    for slot in sc.free_on_reassign.iter_mut() {
        *slot = None;
    }
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_path_checks(&_t, &_p, &flow, &own, &sc, false);
    }));
    assert!(
        r.is_err(),
        "a displacement release removed while the write stays must flag"
    );
}

/// Gap 6 positive pin: release-before-overwrite is the sound order —
/// the intact schedule is accepted, and the new value keeps its own
/// eventual obligation (freed by its last use after `print`).
#[test]
fn displacement_release_before_overwrite_accepted() {
    let src = "fn main():\n\tmut s = \"a\" + \"x\"\n\tprint(s)\n\ts = \"b\" + \"y\"\n\tprint(s)\n";
    let (_t, _p, _m, flow, own, sc) = path_check_harness(src, "main");
    run_path_checks(&_t, &_p, &flow, &own, &sc, false);
}

/// Gap 5 pin: a promotion buffer's release removed (all anchors of the
/// base) — the conditional alloc's obligation is never discharged; the
/// pairing check proves the leak. Mutually exclusive conditional
/// releases and the no-promotion case are accepted (the suite's promo
/// pins stay green).
#[test]
fn promo_release_removed_flags() {
    let src = "fn f(p: str):\n\tv = p[0:8]\n\tprint(v)\n\nfn main():\n\tf(\"0123456789012345678901234567890123456789\")\n";
    let (_t, _p, _m, flow, own, sc) = path_check_harness(src, "f");
    // Intact: accepted.
    run_path_checks(&_t, &_p, &flow, &own, &sc, false);
    // Corruption: remove EVERY release of the base (the normal anchor
    // and the return-epilogue copies) — the alloc leaks.
    let mut sc = sc;
    assert!(
        !sc.promotion_frees.is_empty(),
        "fixture must schedule promotion frees"
    );
    assert!(
        !sc.promo_allocs.is_empty(),
        "fixture must record the promotion alloc"
    );
    sc.promotion_frees.clear();
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_path_checks(&_t, &_p, &flow, &own, &sc, false);
    }));
    assert!(
        r.is_err(),
        "removing every promotion release of a base must flag (the alloc leaks)"
    );
}

/// Gap 5 pin: an EXTRA release of the base on the same path — the
/// flag-guard makes the designed epilogue copies no-ops, but a release
/// placed where no alloc-path needs it still pairs; the soundness pin
/// here is the INTACT schedule accepted with mutually exclusive
/// anchors (branch-dependent releases).
#[test]
fn promo_branch_conditional_releases_accepted() {
    let src = "fn f(p: str, c: bool):\n\tv = p[0:8]\n\tif c:\n\t\tprint(v)\n\telse:\n\t\tprint(v)\n\nfn main():\n\tf(\"0123456789012345678901234567890123456789\", true)\n\tf(\"0123456789012345678901234567890123456789\", false)\n";
    let (_t, _p, _m, flow, own, sc) = path_check_harness(src, "f");
    run_path_checks(&_t, &_p, &flow, &own, &sc, false);
}
