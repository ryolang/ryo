use super::emission::{BodyCursor, Task};
use super::{BlockId, FlowBuilder, Place, PlaceId, Terminator};
use crate::extra::{ExtraList, ExtraRange};
use crate::spine::ChainSpine;
use crate::tir::{MatchBodyView, TirRef, TirTag};
use crate::types::StringId;

pub(super) enum ControlTask {
    Match(MatchFrame),
}

pub(super) struct MatchFrame {
    stmt: TirRef,
    phase: MatchPhase,
    arm_blocks: Box<[BlockId]>,
    next_arm: usize,
    join: Option<BlockId>,
    owner_tokens: Box<[PlaceId]>,
    next_token: usize,
}

#[derive(Clone, Copy)]
enum MatchPhase {
    AfterScrutinee,
    AfterArm,
}

pub(super) fn schedule_control(
    builder: &mut FlowBuilder<'_>,
    stmt: TirRef,
    tasks: &mut ChainSpine<Task<'_>>,
) {
    match builder.tir.inst(stmt).tag {
        TirTag::Match => {
            let scrutinee = builder.tir.match_view(stmt).scrutinee;
            tasks.push(Task::Control(ControlTask::Match(MatchFrame {
                stmt,
                phase: MatchPhase::AfterScrutinee,
                arm_blocks: Box::new([]),
                next_arm: 0,
                join: None,
                owner_tokens: Box::new([]),
                next_token: 0,
            })));
            tasks.push(Task::EnterExpr(scrutinee));
        }
        _ => unreachable!("typed control scheduling for non-match instruction"),
    }
}

pub(super) fn resume_control<'a>(
    builder: &mut FlowBuilder<'a>,
    control: ControlTask,
    tasks: &mut ChainSpine<Task<'a>>,
) {
    match control {
        ControlTask::Match(frame) => resume_match(builder, frame, tasks),
    }
}

fn resume_match<'a>(
    builder: &mut FlowBuilder<'a>,
    mut frame: MatchFrame,
    tasks: &mut ChainSpine<Task<'a>>,
) {
    match frame.phase {
        MatchPhase::AfterScrutinee => prepare_match(builder, &mut frame),
        MatchPhase::AfterArm => finish_arm(builder, &mut frame),
    }
    schedule_next_arm(builder, frame, tasks);
}

fn prepare_match(builder: &mut FlowBuilder<'_>, frame: &mut MatchFrame) {
    let stmt = frame.stmt;
    let view = builder.tir.match_view(stmt);
    let match_ty = view.ty;
    let tokens = builder.tir.match_by_move_owner_refs(stmt, builder.pool);
    let names: Vec<StringId> = view
        .arms()
        .flat_map(|arm| arm.bindings())
        .filter(|binding| !builder.pool.is_copy(binding.ty))
        .map(|binding| binding.name)
        .collect();
    debug_assert_eq!(tokens.len(), names.len(), "token/binding mismatch");
    builder.register_tokens(stmt, &tokens, &names);

    let mut any_falls_through = false;
    for arm in view.arms() {
        let terminates = match arm.body {
            MatchBodyView::Expr(body) => builder.pool.is_never(builder.tir.inst(body).ty),
            MatchBodyView::Stmts(_) => {
                let stmts: Vec<TirRef> = arm.body.stmts().collect();
                builder
                    .tir
                    .block_definitely_terminates(&stmts, builder.pool)
            }
        };
        any_falls_through |= !terminates;
    }

    let arm_start = builder.extra.len() as u32;
    let arm_blocks: Vec<BlockId> = (0..view.len()).map(|_| builder.new_block(false)).collect();
    for &block in &arm_blocks {
        builder.extra.push(block.0);
    }
    frame.join = any_falls_through.then(|| builder.new_block(false));

    builder.set_term(
        builder.cur,
        Terminator::Switch {
            match_inst: stmt,
            scrutinee: view.scrutinee,
            arms: ExtraRange {
                offset: arm_start,
                len: builder.extra.len() as u32 - arm_start,
            },
            join: frame.join,
        },
        Some(stmt),
    );

    if !builder.pool.is_never(match_ty) && match_ty != builder.pool.void() {
        let place = builder.push_place(Place::MatchResult {
            match_inst: stmt,
            ty: match_ty,
        });
        builder.inst_meta[stmt.index()].match_result = Some(place);
    }

    frame.arm_blocks = arm_blocks.into_boxed_slice();
    frame.owner_tokens = builder.token_places_of(stmt).to_vec().into_boxed_slice();
}

fn finish_arm(builder: &mut FlowBuilder<'_>, frame: &mut MatchFrame) {
    let arm_block = frame.arm_blocks[frame.next_arm];
    match frame.join {
        Some(join) => builder.close_region(builder.cur, join),
        None => {
            debug_assert!(
                builder.terminated(arm_block),
                "all-diverging match arm fell through"
            );
            if !builder.terminated(builder.cur) {
                debug_assert!(
                    !builder.has_pred[builder.cur.0 as usize],
                    "live fall-through in an all-diverging match"
                );
                builder.set_term(builder.cur, Terminator::Unreachable, None);
            }
        }
    }
    builder.pop_scope();
    frame.next_arm += 1;
}

fn schedule_next_arm<'a>(
    builder: &mut FlowBuilder<'a>,
    mut frame: MatchFrame,
    tasks: &mut ChainSpine<Task<'a>>,
) {
    if frame.next_arm == frame.arm_blocks.len() {
        debug_assert_eq!(frame.next_token, frame.owner_tokens.len());
        builder.cur = match frame.join {
            Some(join) => join,
            None => builder.new_block(true),
        };
        return;
    }

    let arm = builder.tir.match_view(frame.stmt).arm(frame.next_arm);
    builder.cur = frame.arm_blocks[frame.next_arm];
    builder.push_scope();
    for binding in arm.bindings() {
        if !builder.pool.is_copy(binding.ty) {
            let place = frame.owner_tokens[frame.next_token];
            frame.next_token += 1;
            builder.bind(binding.name, place);
        }
    }

    frame.phase = MatchPhase::AfterArm;
    let body = arm.body;
    tasks.push(Task::Control(ControlTask::Match(frame)));
    match body {
        MatchBodyView::Expr(body) => tasks.push(Task::EnterExpr(body)),
        MatchBodyView::Stmts(words) => {
            tasks.push(Task::Body(BodyCursor::new(ExtraList::new(words))))
        }
    }
}
