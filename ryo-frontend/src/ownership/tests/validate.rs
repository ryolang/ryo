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
    cv::validate_positions(
        &flow,
        &fire_points,
        &sc,
        &mut cv::ValidationSummary::default(),
    );
    cv::validate_no_duplicate_destruction(&places, &flow, &sc);
}

#[test]
fn validator_accepts_static_string_comparison_before_loop_break() {
    let src = "fn main():\n\twhile true:\n\t\tif \"x\" == \"\":\n\t\t\tbreak\n";
    let (diags, _, _, _) = check_src_full(src);
    assert!(diags.is_empty(), "clean program; got {diags:?}");
}

#[test]
fn validator_accepts_echo_example() {
    let (diags, _, _, _) = check_src_full(include_str!("../../../../examples/echo.ryo"));
    assert!(diags.is_empty(), "clean example; got {diags:?}");
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
    cv::validate_positions(
        &flow,
        &fire_points,
        &sc,
        &mut cv::ValidationSummary::default(),
    );
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
    cv::validate_positions(
        &flow,
        &fire_points,
        &sc,
        &mut cv::ValidationSummary::default(),
    );
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
    cv::validate_positions(
        &flow,
        &fire_points,
        &sc,
        &mut cv::ValidationSummary::default(),
    );
    cv::validate_no_duplicate_destruction(&places, &flow, &sc);
}

/// Negative pin (positions): a drop firing in a block the Flow does not
/// have produces an invalid validation result.
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
    let fire_points = ryo_core::flow::fire_points::FirePoints::new(&flow);
    let mut summary = cv::ValidationSummary::default();
    cv::validate_positions(&flow, &fire_points, &sc, &mut summary);
    assert!(
        summary.invalid,
        "a PreTerminator drop in block 999 is invalid"
    );
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
        cv::validate_positions(
            &flow,
            &fire_points,
            &sc,
            &mut cv::ValidationSummary::default(),
        );
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
    use ryo_core::span::Span;
    use ryo_core::tir::{ParamMode, TirBuilder, TirParam};
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let void = pool.void();
    let consume = pool.intern_str("consume");
    let s_name = pool.intern_str("s");
    let span = Span::EMPTY;
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
    cv::validate_positions(
        &flow,
        &fire_points,
        &sc,
        &mut cv::ValidationSummary::default(),
    );
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
        .get(0)
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
) -> cv::ValidationSummary {
    let tir = tirs
        .iter()
        .find(|t| build_flow(t, pool).blocks.len() == flow.blocks.len())
        .expect("the fixture's tir");
    let places = cv::TargetPlaces::build(tir, flow, pool);
    let idx = cv::StepIndex::build(flow);
    let fire_points = ryo_core::flow::fire_points::FirePoints::new(flow);
    let events = cv::collect_events(tir, &places, flow, &fire_points, sc, own, pool);
    let table = cv::generation_table(tir, flow, pool, &idx, sc);
    let mut summary = cv::ValidationSummary {
        required_checks_ran: true,
        ..Default::default()
    };
    cv::validate_no_path_double_destruction(tir, flow, own, sc, &events, &table, &mut summary);
    cv::validate_path_coverage(tir, flow, pool, own, sc, &events, &table, &mut summary);
    let ctx = cv::Ctx {
        tir,
        flow,
        own,
        sidecar: sc,
        fire_points: &fire_points,
        events: &events,
        table: &table,
    };
    cv::validate_view_protection(&ctx, &mut summary);
    cv::validate_displacement(&ctx, &mut summary);
    cv::validate_promo(&ctx, &mut summary);
    let _ = no_escapes; // the escape predicate is gone — coverage is per-path events
    summary
}

/// Negative pin: cleanup removed from one branch while the
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

/// Negative pin: destruction scheduled before a branch AND
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

/// Negative pin: a release at the same point as a return transfer is
/// still invalid. A transfer-out does not mark the target as freed, so
/// codegen's same-block release dedup cannot make this pair safe.
#[test]
fn path_double_destruction_flags_release_and_return_transfer() {
    let src = "fn identity(move s: str) -> str:\n\tprint(\"before\")\n\treturn s\n";
    let (tirs, pool, _main, flow, own, sc) = path_check_harness(src, "identity");
    let tir = tirs
        .iter()
        .find(|t| pool.str(t.name) == "identity")
        .expect("the fixture's body");
    let places = cv::TargetPlaces::build(tir, &flow, &pool);
    let index = cv::StepIndex::build(&flow);
    let fire_points = ryo_core::flow::fire_points::FirePoints::new(&flow);
    let mut events = cv::collect_events(tir, &places, &flow, &fire_points, &sc, &own, &pool);
    let transfer = events
        .iter()
        .flat_map(|(&place, events)| events.iter().map(move |event| (place, event)))
        .find(|(_, event)| event.kind == cv::EventKind::TransferReturn)
        .map(|(place, event)| (place, *event))
        .expect("returning s creates a return-transfer event");
    let (place, mut release) = transfer;
    release.kind = cv::EventKind::Destroy;
    release.point.ordinal = release
        .point
        .ordinal
        .checked_sub(1)
        .expect("the print step precedes the return transfer");
    events
        .get_mut(&place)
        .expect("transfer place exists")
        .push(release);
    let table = cv::generation_table(tir, &flow, &pool, &index, &sc);
    let mut summary = cv::ValidationSummary::default();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        cv::validate_no_path_double_destruction(
            tir,
            &flow,
            &own,
            &sc,
            &events,
            &table,
            &mut summary,
        );
    }));
    assert!(
        result.is_err(),
        "a release at a return transfer point must flag"
    );
}

/// Negative pin: codegen clears its same-target free dedup on loop
/// re-entry, so distinct releases in one cyclic block must remain a
/// double-destruction candidate.
#[test]
fn path_double_destruction_checks_same_block_reentry() {
    let src = "fn main():\n\tmut i = 0\n\ts = \"a\" + \"b\"\n\twhile i < 2:\n\t\tprint(s)\n\t\ti = i + 1\n\tprint(s)\n";
    let (tirs, pool, _main, flow, own, sc) = path_check_harness(src, "main");
    let tir = tirs
        .iter()
        .find(|t| pool.str(t.name) == "main")
        .expect("the fixture's body");
    let places = cv::TargetPlaces::build(tir, &flow, &pool);
    let index = cv::StepIndex::build(&flow);
    let fire_points = ryo_core::flow::fire_points::FirePoints::new(&flow);
    let mut events = cv::collect_events(tir, &places, &flow, &fire_points, &sc, &own, &pool);
    let template = events
        .iter()
        .flat_map(|(&place, events)| events.iter().map(move |event| (place, event)))
        .find(|(_, event)| event.kind == cv::EventKind::Destroy)
        .map(|(place, event)| (place, *event))
        .expect("the allocated string has a scheduled release");
    let table = cv::generation_table(tir, &flow, &pool, &index, &sc);
    let roots = cv::place_roots(tir, &flow, &own, &sc);
    let reachable = cv::reachable_blocks(&flow);
    let (place, template) = template;
    let root = roots[place.0 as usize];

    // Pick a cyclic block with two reachable ordinals and no generation
    // cut for this storage at the first ordinal.
    let block = (0..flow.blocks.len())
        .map(|raw| ryo_core::flow::BlockId(raw as u32))
        .find(|&candidate| {
            if !reachable.contains(&candidate) {
                return false;
            }
            let steps = flow.block_steps(candidate).len();
            if steps < 2 {
                return false;
            }
            let mut seen = std::collections::HashSet::new();
            let mut stack = flow.successors(candidate).to_vec();
            let cyclic = stack.contains(&candidate);
            let mut reaches_self = cyclic;
            while !stack.is_empty() && !reaches_self {
                let next = stack.pop().expect("stack is not empty");
                if next == candidate {
                    reaches_self = true;
                } else if seen.insert(next) {
                    stack.extend_from_slice(flow.successors(next));
                }
            }
            let cut_at_first = events.keys().any(|member| {
                roots[member.0 as usize] == root
                    && table
                        .cut
                        .get(member)
                        .and_then(|by_block| by_block.get(&candidate))
                        .is_some_and(|ordinals| ordinals.contains(&0))
            });
            reaches_self && !cut_at_first
        })
        .expect("the loop body has a cyclic block with two uncut ordinals");

    let mut first = template;
    first.point = cv::Point { block, ordinal: 0 };
    first.pos = DropPosition::BlockStart { block };
    let mut second = template;
    second.point = cv::Point { block, ordinal: 1 };
    second.pos = DropPosition::PreTerminator { block };
    events
        .get_mut(&place)
        .expect("the release place exists")
        .extend([first, second]);

    let mut summary = cv::ValidationSummary::default();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        cv::validate_no_path_double_destruction(
            tir,
            &flow,
            &own,
            &sc,
            &events,
            &table,
            &mut summary,
        );
    }));
    assert!(
        result.is_err(),
        "distinct same-target releases inside a cyclic block must flag"
    );
}

/// Negative pin: a return-path drop removed while the
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
    use ryo_core::span::Span;
    use ryo_core::tir::{ParamMode, TirBuilder, TirParam};
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let void = pool.void();
    let consume = pool.intern_str("consume");
    let s_name = pool.intern_str("s");
    let span = Span::EMPTY;
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

/// Branch-returned value pin (the `choose` reproducer): a value returned on one path
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

/// UAF-direction pin: a base release cloned BEFORE a reachable
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

/// Verdict-honesty pin: a genuinely unresolvable
/// position — the loop's test terminator destroyed in a hand-corrupted
/// Flow, so the payload anchor's matching test is unreachable and the
/// resolver keeps its NON-exact fallback — yields NOT PROVED accounting
/// (counted in the summary), never a violation assertion: an estimated
/// lower bound cannot prove a hazard.
#[test]
fn view_protection_non_exact_release_counts_not_proved() {
    let src = "fn main():\n\ts = \"0123456789012345678901234567890123456789\"\n\tv = s[0:2]\n\tmut i = 0\n\twhile i < 2 and true:\n\t\tprint(v)\n\t\ti += 1\n";
    let (tirs, pool, _main, mut flow, own, sc) = path_check_harness(src, "main");
    // The intact compound-condition schedule resolves exactly and
    // passes with every check proved.
    let summary = run_path_checks(&tirs, &pool, &flow, &own, &sc, false);
    assert_eq!(
        summary.views_not_proved, 0,
        "the intact compound-condition schedule proves its view checks: {summary:?}"
    );
    // Corrupt the Flow: the loop's test block no longer holds the
    // matching WhileTest — the loop-completion position stays NON-exact.
    let test_block = flow
        .forward_order()
        .find(|&b| matches!(flow.term(b), ryo_core::flow::Terminator::WhileTest { .. }))
        .expect("the compound loop's test block");
    flow.terms[test_block.0 as usize] = ryo_core::flow::Terminator::ReturnVoid;
    // The corrupted schedule must NOT panic; the non-exact release is
    // counted as NOT PROVED (the only honest verdict for an estimate).
    let summary = run_path_checks(&tirs, &pool, &flow, &own, &sc, false);
    assert!(
        summary.views_not_proved > 0,
        "a non-exact release position must yield NOT PROVED accounting: {summary:?}"
    );
}

#[test]
fn unresolved_cleanup_position_is_reported_as_invalid() {
    let source = "fn main():\n\ts = \"cleanup position\"\n\tprint(s)\n";
    let (tirs, pool, _main, flow, own, mut sidecar) = path_check_harness(source, "main");
    let drop = sidecar
        .drops
        .first_mut()
        .expect("fixture has a cleanup drop");
    drop.position = DropPosition::BlockStart {
        block: ryo_core::flow::BlockId(u32::MAX),
    };

    let summary = cv::validate_cleanup(&tirs[0], &flow, &pool, &own, &sidecar, true);
    assert!(
        summary.invalid,
        "unresolved anchor must be invalid: {summary:?}"
    );
    assert!(!summary.proof_complete());
    sidecar.validation = Some(summary);
    assert_eq!(
        sidecar.validation_outcome(),
        ryo_core::ownership::ValidationOutcome::Invalid
    );
}

/// Displacement pin: a generation-ending write establishes the new generation
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

/// Displacement positive pin: release-before-overwrite is the sound order —
/// the intact schedule is accepted, and the new value keeps its own
/// eventual obligation (freed by its last use after `print`).
#[test]
fn displacement_release_before_overwrite_accepted() {
    let src = "fn main():\n\tmut s = \"a\" + \"x\"\n\tprint(s)\n\ts = \"b\" + \"y\"\n\tprint(s)\n";
    let (_t, _p, _m, flow, own, sc) = path_check_harness(src, "main");
    run_path_checks(&_t, &_p, &flow, &own, &sc, false);
}

/// Promotion pin: a promotion buffer's release removed (all anchors of the
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

/// Promotion pin: an EXTRA release of the base on the same path — the
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

// ─── View-backed coverage (the exemption is gone) ────────────────────────

/// Positive: view-touched branch-local cleanup — a pre-branch buffer
/// displaced on the taken arm (free_on_reassign) and dead-dropped on
/// the fall-through arm, with a view of it read before the branch. The
/// base group's coverage runs through the production must-analysis
/// (previously exempt as view-touched).
#[test]
fn view_base_coverage_accepts_intact_branch_cleanup() {
    let src = "fn f(c: bool):\n\tmut s = \"0123456789012345678901234567890123456789\"\n\tv = s[0:2]\n\tprint(v)\n\tif c:\n\t\ts = \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\n\nfn main():\n\tf(true)\n\tf(false)\n";
    let (_t, _p, _m, flow, own, sc) = path_check_harness(src, "f");
    let summary = run_path_checks(&_t, &_p, &flow, &own, &sc, false);
    assert!(
        summary.proof_complete(),
        "the intact view-touched schedule proves out: {summary:?}"
    );
}

/// Negative (view family): the taken arm's displacement
/// release removed (the view's last use is before the branch; the
/// branch's own destruction never fires on the taken path) — the taken
/// path leaks, the must-analysis proves it.
#[test]
fn view_base_coverage_flags_arm_displacement_removed() {
    let src = "fn f(c: bool):\n\tmut s = \"0123456789012345678901234567890123456789\"\n\tv = s[0:2]\n\tprint(v)\n\tif c:\n\t\ts = \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\n\nfn main():\n\tf(true)\n\tf(false)\n";
    let (_t, _p, _m, flow, own, mut sc) = path_check_harness(src, "f");
    // Remove the reassign's displacement release (keep the write).
    assert!(
        sc.free_on_reassign.iter().flatten().count() > 0,
        "fixture must schedule a displacement release"
    );
    for slot in sc.free_on_reassign.iter_mut() {
        *slot = None;
    }
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_path_checks(&_t, &_p, &flow, &own, &sc, false);
    }));
    assert!(
        r.is_err(),
        "removing one branch's displacement of a view-touched base must flag"
    );
}

/// Negative (view family): the fall-through arm's
/// conditional dead drop removed — the sibling branch never reads the
/// view, but the pre-branch buffer still leaks there; the sibling path
/// derived no coverage from the taken arm's releases.
#[test]
fn view_base_coverage_flags_sibling_dead_drop_removed() {
    let src = "fn f(c: bool):\n\tmut s = \"0123456789012345678901234567890123456789\"\n\tv = s[0:2]\n\tprint(v)\n\tif c:\n\t\ts = \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\n\nfn main():\n\tf(true)\n\tf(false)\n";
    let (_t, _p, _m, flow, own, mut sc) = path_check_harness(src, "f");
    let before = sc.drops.len();
    sc.drops
        .retain(|d| !matches!(d.position, DropPosition::BlockStart { .. }));
    assert!(
        sc.drops.len() < before,
        "the corruption must remove the fall-through dead drop"
    );
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_path_checks(&_t, &_p, &flow, &own, &sc, false);
    }));
    assert!(
        r.is_err(),
        "removing the sibling branch's cleanup of a view-touched base must flag"
    );
}

/// Positive: nested views (a reslice) — the base group covers both
/// projection lifetimes; the inner view's last use bounds the release.
#[test]
fn view_base_coverage_accepts_nested_views() {
    let src = "fn main():\n\ts = \"0123456789012345678901234567890123456789\"\n\tw = s[0:4]\n\tv = w[1:3]\n\tprint(v)\n";
    let (_t, _p, _m, flow, own, sc) = path_check_harness(src, "main");
    let summary = run_path_checks(&_t, &_p, &flow, &own, &sc, false);
    assert!(
        summary.proof_complete(),
        "the nested-view schedule proves out: {summary:?}"
    );
}

/// Positive: a shadowed then reassigned owner — the generation split is
/// respected (the shadow's reassigns do not touch the outer binding's
/// obligation; both buffers free per path).
#[test]
fn view_base_coverage_accepts_shadowed_reassigned_owner() {
    let src = "fn main():\n\tcond = \"ab\".len() > 0\n\tmut x = \"0123456789012345678901234567890123456789\"\n\tv = x[0:2]\n\tprint(v)\n\tif cond:\n\t\tmut x = \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\n\t\tx = \"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\"\n\tprint(x)\n";
    let (_t, _p, _m, flow, own, sc) = path_check_harness(src, "main");
    let summary = run_path_checks(&_t, &_p, &flow, &own, &sc, false);
    assert!(
        summary.proof_complete(),
        "the shadowed/reassigned schedule proves out: {summary:?}"
    );
}

/// Positive: loop-deferred cleanup — a view created before a
/// compound-condition loop, read inside it. The base's loop-completion
/// anchor resolves exactly (the shared resolver locates the test
/// through the condition's blocks), and the coverage must-analysis
/// proves the exit path.
#[test]
fn view_base_coverage_accepts_loop_deferred_cleanup() {
    let src = "fn main():\n\ts = \"0123456789012345678901234567890123456789\"\n\tv = s[0:2]\n\tmut i = 0\n\twhile i < 2 and true:\n\t\tprint(v)\n\t\ti += 1\n";
    let (_t, _p, _m, flow, own, sc) = path_check_harness(src, "main");
    let summary = run_path_checks(&_t, &_p, &flow, &own, &sc, false);
    assert!(
        summary.proof_complete(),
        "the loop-deferred schedule proves out: {summary:?}"
    );
}

/// Positive: valid transfers — the base returns (ownership moves out)
/// after the view's last use; the return transfer discharges the
/// obligation on that path.
#[test]
fn view_base_coverage_accepts_valid_transfer() {
    let src = "fn f() -> str:\n\ts = \"0123456789012345678901234567890123456789\"\n\tv = s[0:2]\n\tprint(v)\n\treturn s\n\nfn main():\n\tprint(f())\n\tprint(\"\\n\")\n";
    let (_t, _p, _m, flow, own, sc) = path_check_harness(src, "f");
    let summary = run_path_checks(&_t, &_p, &flow, &own, &sc, false);
    assert!(
        summary.proof_complete(),
        "the transfer schedule proves out: {summary:?}"
    );
}

/// Positive: independently owned promotion buffers — two borrowed-param
/// views in sibling arms free their own promoted copies (the pairing
/// checks' family; the borrowed original stays caller-owned).
#[test]
fn view_base_coverage_accepts_independent_promotion_buffers() {
    let src = "fn f(a: str, b: str, c: bool):\n\tif c:\n\t\tv = a[0:8]\n\t\tprint(v)\n\telse:\n\t\tw = b[8:16]\n\t\tprint(w)\n\nfn main():\n\tf(\"0123456789012345678901234567890123456789aaaa\", \"0123456789012345678901234567890123456789bbbb\", true)\n\tf(\"0123456789012345678901234567890123456789aaaa\", \"0123456789012345678901234567890123456789bbbb\", false)\n";
    let (_t, _p, _m, flow, own, sc) = path_check_harness(src, "f");
    let summary = run_path_checks(&_t, &_p, &flow, &own, &sc, false);
    assert!(
        summary.proof_complete(),
        "the independent promotion buffers prove out: {summary:?}"
    );
}

// ─── Cross-iteration view protection ─────────────────────────────────────

/// Run ONLY the view-protection check on a corrupted schedule (the
/// full pipeline would reject the same corruption earlier at the
/// double-discharge check — this pins the view-protection walk itself).
fn run_view_protection_only(
    tirs: &[ryo_core::tir::Tir],
    pool: &InternPool,
    flow: &Flow,
    own: &Ownership,
    sc: &FunctionSidecar,
) {
    let tir = tirs
        .iter()
        .find(|t| build_flow(t, pool).blocks.len() == flow.blocks.len())
        .expect("the fixture's tir");
    let places = cv::TargetPlaces::build(tir, flow, pool);
    let idx = cv::StepIndex::build(flow);
    let fire_points = ryo_core::flow::fire_points::FirePoints::new(flow);
    let events = cv::collect_events(tir, &places, flow, &fire_points, sc, own, pool);
    let table = cv::generation_table(tir, flow, pool, &idx, sc);
    let mut summary = cv::ValidationSummary::default();
    let ctx = cv::Ctx {
        tir,
        flow,
        own,
        sidecar: sc,
        fire_points: &fire_points,
        events: &events,
        table: &table,
    };
    cv::validate_view_protection(&ctx, &mut summary);
}

/// Negative (cross-iteration): the base's loop-exit release cloned
/// INSIDE the loop body, after the view's last use — with pre-loop
/// storage the release at the end of iteration N reaches the view's
/// use at the start of iteration N+1 through the backedge (no
/// generation boundary in between): a use-after-free the walk now
/// proves. Pre-fix the closing edge was not followed and this leaked
/// silently.
#[test]
fn view_protection_flags_release_before_next_iteration_use() {
    let src = "fn main():\n\ts = \"0123456789012345678901234567890123456789\"\n\tv = s[0:2]\n\tmut i = 0\n\twhile i < 3:\n\t\tprint(v)\n\t\ti += 1\n";
    let (_t, _p, _m, flow, own, mut sc) = path_check_harness(src, "main");
    let base_free = *sc
        .drops
        .iter()
        .find(|d| matches!(d.position, DropPosition::AfterStep { .. }))
        .expect("the base's loop-completion free");
    // Clone it at the body block's terminator point — after the view's
    // last use, before the backedge. The same-iteration use is behind
    // the walk; only the NEXT iteration's use is reachable — through
    // the closing edge.
    let backedge = flow
        .forward_order()
        .find(|&b| matches!(flow.term(b), ryo_core::flow::Terminator::Backedge { .. }))
        .expect("the loop's backedge block");
    sc.drops.push(DropOp {
        position: DropPosition::PreTerminator { block: backedge },
        target: base_free.target,
        span: base_free.span,
    });
    // The view-protection walk itself proves the UAF direction.
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_view_protection_only(&_t, &_p, &flow, &own, &sc);
    }));
    let err = r.expect_err("the release must reach the next iteration's use");
    let msg = panic_message(&err);
    assert!(
        msg.contains("view protection"),
        "the view-protection walk must flag the cross-iteration UAF: {msg}"
    );
    // And the full pipeline rejects the schedule too (the clone also
    // double-fires with the original at the exit).
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_path_checks(&_t, &_p, &flow, &own, &sc, false);
    }));
    assert!(r.is_err(), "the corrupted schedule must be rejected");
}

/// Negative (continue path): the cloned release sits at a `continue`
/// terminator — the walk follows the continue edge to the header and
/// around to the next iteration's use.
#[test]
fn view_protection_flags_release_at_continue_before_next_use() {
    let src = "fn main():\n\ts = \"0123456789012345678901234567890123456789\"\n\tv = s[0:2]\n\tmut i = 0\n\twhile i < 3:\n\t\tprint(v)\n\t\ti += 1\n\t\tif i == 2:\n\t\t\tcontinue\n";
    let (_t, _p, _m, flow, own, mut sc) = path_check_harness(src, "main");
    let base_free = *sc
        .drops
        .iter()
        .find(|d| matches!(d.position, DropPosition::AfterStep { .. }))
        .expect("the base's loop-completion free");
    let continue_block = flow
        .forward_order()
        .find(|&b| matches!(flow.term(b), ryo_core::flow::Terminator::Continue { .. }))
        .expect("the continue's block");
    sc.drops.push(DropOp {
        position: DropPosition::PreTerminator {
            block: continue_block,
        },
        target: base_free.target,
        span: base_free.span,
    });
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_view_protection_only(&_t, &_p, &flow, &own, &sc);
    }));
    let err = r.expect_err("the continue-path release must reach the next iteration's use");
    let msg = panic_message(&err);
    assert!(
        msg.contains("view protection"),
        "the view-protection walk must flag the continue-path UAF: {msg}"
    );
}

/// The panic message of a caught unwind (the payload is a `String` for
/// explicit panics, `&str` for assertion macros).
fn panic_message(err: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = err.downcast_ref::<String>() {
        s.clone()
    } else if let Some(s) = err.downcast_ref::<&str>() {
        s.to_string()
    } else {
        String::new()
    }
}

/// Positive: per-iteration destruction with genuine reinitialization —
/// the body establishes a fresh generation every iteration (its Init /
/// producer step is a boundary after the closing edge), so the
/// previous iteration's release cannot reach the next iteration's use.
#[test]
fn view_protection_accepts_per_iteration_reinitialization() {
    let src = "fn main():\n\tmut i = 0\n\twhile i < 3 and true:\n\t\tt = \"0123456789012345678901234567890123456789\" + int_to_str(i)\n\t\tv = t[0:2]\n\t\tprint(v)\n\t\ti += 1\n";
    let (_t, _p, _m, flow, own, sc) = path_check_harness(src, "main");
    let summary = run_path_checks(&_t, &_p, &flow, &own, &sc, false);
    assert!(
        summary.proof_complete(),
        "the per-iteration reinitialization proves out: {summary:?}"
    );
}

/// Positive: nested compound-condition loops with a pre-loop view —
/// the base's outer-loop-exit release cannot reach the in-body uses.
#[test]
fn view_protection_accepts_nested_loops() {
    let src = "fn main():\n\ts = \"0123456789012345678901234567890123456789\"\n\tv = s[0:2]\n\tmut i = 0\n\twhile i < 2 and true:\n\t\tmut j = 0\n\t\twhile j < 2 or false:\n\t\t\tprint(v)\n\t\t\tj += 1\n\t\ti += 1\n";
    let (_t, _p, _m, flow, own, sc) = path_check_harness(src, "main");
    let summary = run_path_checks(&_t, &_p, &flow, &own, &sc, false);
    assert!(
        summary.proof_complete(),
        "the nested-loop schedule proves out: {summary:?}"
    );
}

/// Positive: zero-iteration path — the base's release lands after the
/// loop; the zero-iteration path (test false on entry) reaches it
/// directly, and no in-loop obligation exists.
#[test]
fn view_protection_accepts_zero_iteration_path() {
    let src = "fn main():\n\ts = \"0123456789012345678901234567890123456789\"\n\tv = s[0:2]\n\tmut i = 0\n\twhile i < 2 and true:\n\t\ti += 1\n\tprint(v)\n";
    let (_t, _p, _m, flow, own, sc) = path_check_harness(src, "main");
    let summary = run_path_checks(&_t, &_p, &flow, &own, &sc, false);
    assert!(
        summary.proof_complete(),
        "the zero-iteration schedule proves out: {summary:?}"
    );
}

// ─── Proof accounting (three outcomes everywhere) ────────────────────────

/// Item-5 pin: a NON-exact promotion release yields NOT PROVED
/// accounting — counted in `promo_pairings_not_proved`, never treated
/// as Proven. Constructed by corrupting the Flow (the loop's test
/// terminator destroyed) so the loop-anchored promo frees resolve to
/// the resolver's NonExact fallback.
#[test]
fn promo_non_exact_release_counts_not_proved() {
    // A borrowed-param view whose promotion free is loop-anchored (the
    // view's last use is inside the loop; the freeze machinery defers
    // the free to the loop exit).
    let src = "fn scan(s: str):\n\tv = s[0:2]\n\tmut i = 0\n\twhile i < 2 and true:\n\t\tprint(v)\n\t\ti += 1\n\nfn main():\n\tscan(\"0123456789012345678901234567890123456789\")\n";
    let (tirs, pool, _m, mut flow, own, sc) = path_check_harness(src, "scan");
    // Intact: the loop-deferred promo schedule proves out.
    let summary = run_path_checks(&tirs, &pool, &flow, &own, &sc, false);
    assert!(
        summary.promo_pairings > 0,
        "fixture must schedule promotion pairings"
    );
    assert!(
        summary.proof_complete(),
        "the intact promo schedule proves out: {summary:?}"
    );
    // Corrupt the Flow: the compound-condition loop's test terminator
    // destroyed — the loop-anchored promo frees turn NonExact (the
    // resolver's unresolvable-loop fallback) while staying on the
    // covering path (the alloc's pre-header block terminator).
    let test_block = flow
        .forward_order()
        .find(|&b| matches!(flow.term(b), ryo_core::flow::Terminator::WhileTest { .. }))
        .expect("the while loop's test block");
    flow.terms[test_block.0 as usize] = ryo_core::flow::Terminator::ReturnVoid;
    // No check may assert from an estimated position — and the pairing
    // must be accounted as NOT PROVED.
    let summary = run_path_checks(&tirs, &pool, &flow, &own, &sc, false);
    assert!(
        summary.promo_pairings_not_proved > 0,
        "a non-exact promo release must yield NOT PROVED accounting: {summary:?}"
    );
    assert!(
        !summary.proof_complete(),
        "an unproved supported shape must keep the proof-completeness predicate false"
    );
}
/// Item-5 pin: unreachable events create no false obligations — a
/// release parked in a dead join block (Unreachable terminator) is
/// excluded and COUNTED (`events_unreachable`), never mistaken for
/// coverage and never a double-discharge pair; the body still proves
/// complete (unreachable is a separate category from unknown proof).
#[test]
fn unreachable_events_create_no_false_obligations() {
    let src = "fn f(c: bool, d: bool) -> int:\n\ts = \"0123456789012345678901234567890123456789\"\n\twhile c:\n\t\tif d:\n\t\t\treturn 1\n\t\telse:\n\t\t\tbreak\n\treturn 0\n\nfn main():\n\tprint(int_to_str(f(true, true)))\n\tprint(\"\\n\")\n";
    let (_t, _p, _m, flow, own, mut sc) = path_check_harness(src, "f");
    // The body's if join is dead (both arms diverge) — an Unreachable block.
    let dead = flow
        .forward_order()
        .find(|&b| matches!(flow.term(b), ryo_core::flow::Terminator::Unreachable))
        .expect("a dead join block exists");
    let parked = sc.drops[0];
    sc.drops.push(DropOp {
        position: DropPosition::PreTerminator { block: dead },
        target: parked.target,
        span: parked.span,
    });
    let summary = run_path_checks(&_t, &_p, &flow, &own, &sc, false);
    assert!(
        summary.events_unreachable >= 1,
        "the parked release is unreachable and must be counted: {summary:?}"
    );
    assert!(
        summary.proof_complete(),
        "unreachable exclusions must not count against proof completeness: {summary:?}"
    );
}

/// Item-5 pin: the corpus of clean fixtures satisfies the
/// proof-completeness predicate on every body, and the per-body
/// summaries PROPAGATE on `FunctionSidecar::validation` (the
/// deterministic accessor — no env printer involved).
#[test]
fn validator_corpus_satisfies_proof_completeness() {
    let fixtures: &[&str] = &[
        // Straight-line last use.
        "fn main():\n\ts = \"0123456789012345678901234567890123456789\"\n\tprint(s)\n",
        // If/else borrows in both arms.
        "fn main():\n\ts = \"a\"\n\tif true:\n\t\tprint(s)\n\telse:\n\t\tprint(s)\n",
        // Match by-move arm.
        "enum Pair:\n\tBoth(str, str)\n\nfn main():\n\tp = Pair.Both(\"a\", \"b\")\n\tmatch p:\n\t\tBoth(x, _):\n\t\t\tprint(x)\n",
        // Compound-condition loop view (the R5-A reproducer family).
        "fn main():\n\ts = \"0123456789012345678901234567890123456789\"\n\tv = s[0:2]\n\tmut i = 0\n\twhile i < 2 and true:\n\t\tprint(v)\n\t\ti += 1\n",
        // Value-position match with an arm-local view read (the
        // per-path re-anchor family).
        "fn main():\n\ts = \"0123456789012345678901234567890123456789\" + \"!\"\n\tv = s[0:3]\n\tx = match true:\n\t\ttrue: 1\n\t\tfalse: v.len()\n\tprint(x)\n\tprint(\"\\n\")\n",
        // Nested views.
        "fn main():\n\ts = \"0123456789012345678901234567890123456789\"\n\tw = s[0:4]\n\tv = w[1:3]\n\tprint(v)\n",
        // Loop-carried concat accumulator.
        "fn main():\n\tmut total = \"😀😀😀😀😀😀😀\"\n\twhile true:\n\t\ttotal = total + \"🦊\"\n\t\tbreak\n\tprint(total)\n\tprint(\"\\n\")\n",
        // Branch-local reassign cleanup (displacement + dead drop).
        "fn main():\n\tmut s = \"a\"\n\tif true:\n\t\ts = \"b\"\n\tprint(s)\n",
        // Borrowed-param view + promotion buffers.
        "fn f(p: str):\n\tv = p[0:8]\n\tprint(v)\n\nfn main():\n\tf(\"0123456789012345678901234567890123456789\")\n",
        // Early return with live locals.
        "fn f():\n\ts = \"a\"\n\tif true:\n\t\treturn\n\tprint(s)\n\nfn main():\n\tf()\n\tprint(\"done\\n\")\n",
    ];
    let mut totals = cv::ValidationSummary::default();
    for (n, src) in fixtures.iter().enumerate() {
        let (_diags, sidecar, _tirs, _pool) = check_src_full(src);
        for f in &sidecar.functions {
            let v = f
                .validation
                .expect("debug validation populates the per-body summary");
            assert!(
                v.proof_complete(),
                "corpus fixture {n} body {:?} must prove complete: {v:?}",
                _pool.str(f.name)
            );
            totals.positions += v.positions;
            totals.pairs_examined += v.pairs_examined;
            totals.pairs_not_proved += v.pairs_not_proved;
            totals.obligations += v.obligations;
            totals.obligations_not_proved += v.obligations_not_proved;
            totals.obligations_unreachable += v.obligations_unreachable;
            totals.events_unreachable += v.events_unreachable;
            totals.promo_pairings += v.promo_pairings;
            totals.promo_pairings_not_proved += v.promo_pairings_not_proved;
            totals.displacements += v.displacements;
            totals.view_checks += v.view_checks;
            totals.views_not_proved += v.views_not_proved;
            totals.views_unreachable += v.views_unreachable;
            totals.positions_not_proved += v.positions_not_proved;
        }
    }
    eprintln!("validator corpus totals: {totals:?}");
    assert!(
        totals.obligations > 0 && totals.view_checks > 0 && totals.positions > 0,
        "the corpus must exercise every check: {totals:?}"
    );
    assert_eq!(totals.pairs_not_proved, 0, "clean corpus: {totals:?}");
    assert_eq!(totals.obligations_not_proved, 0, "clean corpus: {totals:?}");
    assert_eq!(totals.views_not_proved, 0, "clean corpus: {totals:?}");
    assert_eq!(totals.positions_not_proved, 0, "clean corpus: {totals:?}");
    assert_eq!(
        totals.promo_pairings_not_proved, 0,
        "clean corpus: {totals:?}"
    );
}

#[test]
fn skipped_validation_does_not_prove_completeness() {
    assert!(!cv::ValidationSummary::default().proof_complete());
    let src = "fn main():\n\tprint(1)\n";
    let (_, mut sidecar, tirs, pool) = check_src_full(src);
    let sc = take_function_sidecar(&mut sidecar, 0);
    let flow = build_flow(&tirs[0], &pool);
    let own = cv::take_test_ownership(tirs[0].name).expect("ownership recorded");
    let summary = cv::validate_cleanup(&tirs[0], &flow, &pool, &own, &sc, false);
    assert!(!summary.required_checks_ran);
    assert!(!summary.proof_complete());
}
