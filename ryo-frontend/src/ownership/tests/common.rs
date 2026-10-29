use super::super::*;

/// Build an all-`Borrow` modes slice matching the length of an
/// argument list. The ownership pass reads `view.modes` directly,
/// so test/builtin call sites pass all-`Borrow` to avoid
/// accidentally moving arguments.
pub(super) fn all_borrow(args: &[TirRef]) -> Vec<ryo_core::tir::ParamMode> {
    vec![ryo_core::tir::ParamMode::Borrow; args.len()]
}

/// Positional replacement for the old name-keyed sidecar lookup:
/// find `name`'s index in `tirs` and take that entry.
/// Take the sidecar entry at `index`, positional with the `tirs`
/// slice handed to `check` — the same contract codegen relies on.
/// (Every test below checks a single function, so `index` is 0.)
pub(super) fn take_function_sidecar(
    sidecar: &mut OwnershipSidecar,
    index: usize,
) -> FunctionSidecar {
    let name = sidecar.functions[index].name;
    let arena_len = sidecar.functions[index].free_on_reassign.len();
    std::mem::replace(
        &mut sidecar.functions[index],
        FunctionSidecar::new(name, arena_len),
    )
}

/// The unified cleanup schedule (the unified cleanup schedule): every scheduled destruction
/// travels through `drops` — `BlockStart` entries are the old
/// `match_arm_drops` (arm-start payload destruction, one per consuming
/// arm) and `conditional_dead_drops` (one per untouched arm block).
/// Tests that pinned the old tables filter by position.
pub(super) fn block_start_drops(sidecar: &FunctionSidecar) -> Vec<&ryo_core::flow::DropOp> {
    sidecar
        .drops
        .iter()
        .filter(|d| matches!(d.position, ryo_core::flow::DropPosition::BlockStart { .. }))
        .collect()
}

/// Lex + parse + astgen + sema + ownership on a source snippet —
/// the full front-end, so the case-B tests read like the programs
/// users write. Returns every diagnostic from all four stages.
pub(super) fn check_src(input: &str) -> Vec<Diag> {
    check_src_full(input).0
}

/// `check_src` variant that also returns the ownership sidecar, the
/// TIRs, and the pool — for tests that pin Free anchors to specific
/// instructions.
pub(in crate::ownership) fn check_src_full(
    input: &str,
) -> (
    Vec<Diag>,
    OwnershipSidecar,
    Vec<ryo_core::tir::Tir>,
    InternPool,
) {
    use crate::grammar::ParseState;
    use chumsky::Parser as _;
    use chumsky::input::Input as _;
    let mut pool = InternPool::new();
    let mut lex_sink = DiagSink::new();
    let tokens = crate::lexer::lex(input, &mut pool, &mut lex_sink);
    assert!(
        !lex_sink.has_errors(),
        "lex errors: {:?}",
        lex_sink.into_diags()
    );
    let token_stream = tokens[..].split_token_span((0..input.len()).into());
    let mut state = ParseState::new(pool);
    crate::grammar::program_parser()
        .parse_with_state(token_stream, &mut state)
        .into_result()
        .expect("parse ok");
    let (ast, mut pool) = state.into_parts();
    let mut sink = DiagSink::new();
    let uir = crate::astgen::generate(&ast, &mut pool, &mut sink);
    let (tirs, sema_warnings) = crate::sema::analyze_buffered(
        &uir,
        &mut pool,
        &mut sink,
        input,
        std::path::Path::new("<test>"),
    );
    let flows: Vec<ryo_core::flow::Flow> = tirs
        .iter()
        .map(|t| ryo_core::flow::Flow::build(t, &pool))
        .collect();
    let sidecar = check(&tirs, &flows, &pool, &mut sink);
    // Match the driver's deferred flush: sema warnings surface only
    // when the whole unit (including the ownership pass) is clean.
    if !sink.has_errors() {
        for d in sema_warnings {
            sink.emit(d);
        }
    }
    (sink.into_diags(), sidecar, tirs, pool)
}

pub(super) fn w0003_count(diags: &[Diag]) -> usize {
    diags
        .iter()
        .filter(|d| d.code == DiagCode::RedundantMaterialize)
        .count()
}

pub(super) fn w0004_count(diags: &[Diag]) -> usize {
    diags
        .iter()
        .filter(|d| d.code == DiagCode::RedundantToBytes)
        .count()
}

/// Build the canonical Flow IR for a test's TIR — the same execution
/// structure the ownership pass and codegen share — for asserting
/// block membership of scheduled drops.
pub(super) fn build_flow(tir: &ryo_core::tir::Tir, pool: &InternPool) -> ryo_core::flow::Flow {
    ryo_core::flow::Flow::build(tir, pool)
}

/// The arm Flow IR blocks of the match at `m`, in wire (written) order —
/// the Switch arms of the match's terminator block.
pub(super) fn match_arm_blocks(
    flow: &ryo_core::flow::Flow,
    m: ryo_core::tir::TirRef,
) -> Vec<ryo_core::flow::BlockId> {
    let sw_block = flow.term_block_of(m).expect("match is a switch");
    let ryo_core::flow::Terminator::Switch { arms: rng, .. } = flow.term(sw_block) else {
        panic!("match is a switch")
    };
    rng.as_range()
        .map(|i| ryo_core::flow::BlockId(flow.extra[i]))
        .collect()
}
