use super::emission::{BodyCursor, Task};
use super::{BlockId, FlowBuilder, Place, PlaceId, Terminator};
use crate::extra::{ExtraList, ExtraRange};
use crate::spine::ChainSpine;
use crate::tir::{MatchBodyView, TirRef, TirTag};
use crate::types::StringId;

pub(super) enum ControlTask {
    If(IfFrame),
    Match(MatchFrame),
}

pub(super) struct IfFrame {
    stmt: TirRef,
    phase: IfPhase,
    then_block: BlockId,
    elif_condition_blocks: Box<[BlockId]>,
    elif_branch_blocks: Vec<BlockId>,
    else_block: BlockId,
    join: BlockId,
    next_branch: usize,
}

#[derive(Clone, Copy)]
enum IfPhase {
    InitialCondition,
    Then,
    ElifCondition,
    ElifBody,
    Else,
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
        TirTag::IfStmt => {
            let cond = builder.tir.if_stmt_view(stmt).cond;
            tasks.push(Task::Control(ControlTask::If(IfFrame {
                stmt,
                phase: IfPhase::InitialCondition,
                then_block: BlockId::NONE,
                elif_condition_blocks: Box::new([]),
                elif_branch_blocks: Vec::new(),
                else_block: BlockId::NONE,
                join: BlockId::NONE,
                next_branch: 0,
            })));
            tasks.push(Task::EnterExpr(cond));
        }
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
        _ => unreachable!("typed control scheduling for non-control instruction"),
    }
}

pub(super) fn resume_control<'a>(
    builder: &mut FlowBuilder<'a>,
    control: ControlTask,
    tasks: &mut ChainSpine<Task<'a>>,
) {
    match control {
        ControlTask::If(frame) => resume_if(builder, frame, tasks),
        ControlTask::Match(frame) => resume_match(builder, frame, tasks),
    }
}

fn resume_if<'a>(
    builder: &mut FlowBuilder<'a>,
    mut frame: IfFrame,
    tasks: &mut ChainSpine<Task<'a>>,
) {
    match frame.phase {
        IfPhase::InitialCondition => {
            prepare_if(builder, &mut frame);
            schedule_then(builder, frame, tasks);
        }
        IfPhase::Then => {
            builder.pop_scope();
            builder.close_region(builder.cur, frame.join);
            schedule_elif_or_else(builder, frame, tasks);
        }
        IfPhase::ElifCondition => {
            let branch = frame.next_branch;
            frame.elif_branch_blocks.push(builder.cur);
            let body_block = builder.new_block(false);
            let next = frame
                .elif_condition_blocks
                .get(branch + 1)
                .copied()
                .unwrap_or(frame.else_block);
            let elif = builder
                .tir
                .if_stmt_view(frame.stmt)
                .elif_branches
                .iter()
                .nth(branch)
                .expect("elif branch");
            builder.set_term(
                builder.cur,
                Terminator::Branch {
                    cond: elif.cond,
                    then_: body_block,
                    else_: next,
                },
                None,
            );
            builder.cur = body_block;
            builder.push_scope();
            frame.phase = IfPhase::ElifBody;
            tasks.push(Task::Control(ControlTask::If(frame)));
            tasks.push(Task::Body(BodyCursor::new(elif.body)));
        }
        IfPhase::ElifBody => {
            builder.pop_scope();
            builder.close_region(builder.cur, frame.join);
            frame.next_branch += 1;
            schedule_elif_or_else(builder, frame, tasks);
        }
        IfPhase::Else => {
            builder.pop_scope();
            finish_if(builder, frame);
        }
    }
}

fn prepare_if(builder: &mut FlowBuilder<'_>, frame: &mut IfFrame) {
    let view = builder.tir.if_stmt_view(frame.stmt);
    frame.then_block = builder.new_block(false);
    frame.elif_condition_blocks = (0..view.elif_branches.len())
        .map(|_| builder.new_block(false))
        .collect();
    frame.else_block = builder.new_block(false);
    frame.join = builder.new_block(false);
    frame.elif_branch_blocks = Vec::with_capacity(view.elif_branches.len());
    let first_else = frame
        .elif_condition_blocks
        .first()
        .copied()
        .unwrap_or(frame.else_block);
    builder.set_term(
        builder.cur,
        Terminator::Branch {
            cond: view.cond,
            then_: frame.then_block,
            else_: first_else,
        },
        Some(frame.stmt),
    );
}

fn schedule_then<'a>(
    builder: &mut FlowBuilder<'a>,
    mut frame: IfFrame,
    tasks: &mut ChainSpine<Task<'a>>,
) {
    let body = builder.tir.if_stmt_view(frame.stmt).then_stmts;
    builder.cur = frame.then_block;
    builder.push_scope();
    frame.phase = IfPhase::Then;
    tasks.push(Task::Control(ControlTask::If(frame)));
    tasks.push(Task::Body(BodyCursor::new(body)));
}

fn schedule_elif_or_else<'a>(
    builder: &mut FlowBuilder<'a>,
    mut frame: IfFrame,
    tasks: &mut ChainSpine<Task<'a>>,
) {
    if frame.next_branch < frame.elif_condition_blocks.len() {
        let cond = builder
            .tir
            .if_stmt_view(frame.stmt)
            .elif_branches
            .iter()
            .nth(frame.next_branch)
            .expect("elif branch")
            .cond;
        builder.cur = frame.elif_condition_blocks[frame.next_branch];
        frame.phase = IfPhase::ElifCondition;
        tasks.push(Task::Control(ControlTask::If(frame)));
        tasks.push(Task::EnterExpr(cond));
        return;
    }

    builder.cur = frame.else_block;
    if let Some(body) = builder.tir.if_stmt_view(frame.stmt).else_stmts {
        builder.push_scope();
        frame.phase = IfPhase::Else;
        tasks.push(Task::Control(ControlTask::If(frame)));
        tasks.push(Task::Body(BodyCursor::new(body)));
    } else {
        finish_if(builder, frame);
    }
}

fn finish_if(builder: &mut FlowBuilder<'_>, frame: IfFrame) {
    builder.close_region(builder.cur, frame.join);
    builder.branch_arms.insert(
        frame.stmt,
        crate::flow::BranchArms {
            then_block: frame.then_block,
            elif_blocks: frame.elif_branch_blocks,
            else_block: frame.else_block,
            join: frame.join,
        },
    );
    builder.cur = frame.join;
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
            MatchBodyView::Stmts(_) => builder
                .tir
                .block_definitely_terminates(arm.body.stmts(), builder.pool),
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
