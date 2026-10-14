//! [`Flow`] builder tests: canonical block shapes, place identity, and
//! the validator contracts, over programmatically built TIR bodies.

use super::*;
use crate::tir::{TirBuilder, TirTag, TypedMatchArm, TypedMatchBody};
use crate::types::InternPool;
use chumsky::span::{SimpleSpan, Span as _};

fn sp() -> Span {
    SimpleSpan::new((), 0..0)
}

struct Ctx {
    pool: InternPool,
    main: crate::types::StringId,
    int: crate::types::TypeId,
    void: crate::types::TypeId,
    never: crate::types::TypeId,
    x: crate::types::StringId,
    i: crate::types::StringId,
}

impl Ctx {
    fn new() -> Self {
        let mut pool = InternPool::new();
        let main = pool.intern_str("main");
        let int = pool.int();
        let void = pool.void();
        let never = pool.never();
        let x = pool.intern_str("x");
        let i = pool.intern_str("i");
        Ctx {
            pool,
            main,
            int,
            void,
            never,
            x,
            i,
        }
    }

    fn builder(&mut self) -> TirBuilder {
        TirBuilder::new(self.main, vec![], self.void, sp())
    }
}

fn step_insts(flow: &Flow, b: BlockId) -> Vec<crate::tir::TirRef> {
    flow.block_steps(b).iter().map(|s| s.inst).collect()
}

#[test]
fn linear_body_is_one_block_in_evaluation_order() {
    let mut cx = Ctx::new();
    let int = cx.int;
    let mut b = cx.builder();
    let one = b.int_const(1, int, sp());
    let two = b.int_const(2, int, sp());
    let sum = b.binary(TirTag::IAdd, int, one, two, sp());
    let decl = b.var_decl(cx.x, false, int, sum, sp());
    let tir = b.finish(&[decl]);

    let flow = Flow::build(&tir, &cx.pool);
    assert_eq!(flow.blocks.len(), 1);
    assert!(matches!(flow.term(flow.entry), Terminator::ReturnVoid));
    assert_eq!(step_insts(&flow, flow.entry), vec![one, two, sum, decl]);

    let place = flow.local_of_decl(decl).expect("decl place");
    assert!(matches!(flow.place(place), Place::Local { .. }));
    assert_eq!(flow.place_writes(place), &[decl]);
    let facts = flow.facts_of(decl).expect("decl facts");
    assert!(matches!(
        facts.write.expect("decl writes").kind,
        WriteKind::Init
    ));
}

#[test]
fn if_else_cascades_with_explicit_join_preds() {
    let mut cx = Ctx::new();
    let int = cx.int;
    let mut b = cx.builder();
    let zero = b.int_const(0, int, sp());
    let decl = b.var_decl(cx.x, true, int, zero, sp());
    let cond = b.var(cx.x, int, sp());
    let one = b.int_const(1, int, sp());
    let then_a = b.assign(cx.x, int, one, sp());
    let two = b.int_const(2, int, sp());
    let else_a = b.assign(cx.x, int, two, sp());
    let ifs = b.if_stmt(cond, &[then_a], &[], Some(&[else_a]), cx.void, sp());
    let tir = b.finish(&[decl, ifs]);

    let flow = Flow::build(&tir, &cx.pool);
    // entry, then, else, join
    assert_eq!(flow.blocks.len(), 4);
    assert!(matches!(
        flow.term(flow.entry),
        Terminator::Branch { cond: c, .. } if *c == cond
    ));
    let then_block = flow.successors(flow.entry)[0];
    let join = match flow.term(then_block) {
        Terminator::Goto { target } => *target,
        other => panic!("then arm must fall through to the join, got {:?}", other),
    };
    assert_eq!(flow.predecessors(join).len(), 2);
    assert!(
        flow.predecessors(join)
            .iter()
            .all(|p| matches!(flow.term(*p), Terminator::Goto { target } if *target == join))
    );

    // Both assigns resolve to the same place; lineage is in order.
    let place = flow.local_of_decl(decl).expect("decl place");
    assert_eq!(flow.place_writes(place), &[decl, then_a, else_a]);
}

#[test]
fn while_loop_backedge_and_break_continue_targets() {
    let mut cx = Ctx::new();
    let bool_ty = cx.pool.bool_();
    let mut b = cx.builder();
    let t = cx.pool.intern_str("t");
    let cond_if = b.var(t, bool_ty, sp());
    let cond_while = b.var(t, bool_ty, sp());
    let brk = b.break_stmt(cx.never, sp());
    let cont = b.continue_stmt(cx.never, sp());
    let body_stmt = b.if_stmt(cond_if, &[brk], &[], Some(&[cont]), cx.void, sp());
    let whl = b.while_loop(cond_while, &[body_stmt], cx.void, sp());
    let tir = b.finish(&[whl]);

    let flow = Flow::build(&tir, &cx.pool);
    assert!(matches!(flow.term(flow.entry), Terminator::While { .. }));
    let header = match flow.term(flow.entry) {
        Terminator::While { header, .. } => *header,
        _ => unreachable!(),
    };
    let (body_b, exit_b) = match flow.term(header) {
        Terminator::WhileTest { body, exit, .. } => (*body, *exit),
        _ => unreachable!(),
    };
    assert!(flow.predecessors(exit_b).contains(&header));
    let mut saw_break = false;
    let mut saw_continue = false;
    for blk in flow.forward_order() {
        match flow.term(blk) {
            Terminator::Break { target } => {
                assert_eq!(*target, exit_b);
                saw_break = true;
            }
            Terminator::Continue { target } => {
                assert_eq!(*target, header);
                saw_continue = true;
            }
            _ => {}
        }
    }
    // The body never completes normally (break/continue cover it),
    // so no backedge exists — and that's the correct shape.
    assert!(saw_break && saw_continue);
    assert!(
        flow.forward_order()
            .all(|blk| !matches!(flow.term(blk), Terminator::Backedge { .. }))
    );
    let _ = body_b;
}

#[test]
fn for_loop_binds_induction_place_in_body_scope() {
    let mut cx = Ctx::new();
    let int = cx.int;
    let mut b = cx.builder();
    let zero = b.int_const(0, int, sp());
    let ten = b.int_const(10, int, sp());
    let read_i = b.var(cx.i, int, sp());
    let expr = b.unary(TirTag::ExprStmt, cx.void, read_i, sp());
    let f = b.for_range(cx.i, zero, ten, &[expr], cx.void, sp());
    let tir = b.finish(&[f]);

    let flow = Flow::build(&tir, &cx.pool);
    let var_place = flow.loop_var(f).expect("loop var place");
    assert!(matches!(flow.place(var_place), Place::LoopVar { .. }));
    // The body's Var read resolves to the induction place.
    let facts = flow.facts_of(read_i).expect("read facts");
    assert_eq!(facts.read, Some(var_place));
    assert_eq!(flow.place_writes(var_place), &[f]);
    let header = match flow.term(flow.entry) {
        Terminator::For { header, .. } => *header,
        _ => unreachable!(),
    };
    assert!(matches!(
        flow.term(header),
        Terminator::ForTest { var, .. } if *var == var_place
    ));
}

#[test]
fn shadowing_mints_distinct_places() {
    let mut cx = Ctx::new();
    let int = cx.int;
    let mut b = cx.builder();
    let one = b.int_const(1, int, sp());
    let outer = b.var_decl(cx.x, false, int, one, sp());
    let cond = b.bool_const(true, cx.pool.bool_(), sp());
    let two = b.int_const(2, int, sp());
    let inner = b.var_decl(cx.x, false, int, two, sp());
    let then_read = b.var(cx.x, int, sp());
    let then_e = b.unary(TirTag::ExprStmt, cx.void, then_read, sp());
    let ifs = b.if_stmt(cond, &[inner, then_e], &[], None, cx.void, sp());
    let tir = b.finish(&[outer, ifs]);

    let flow = Flow::build(&tir, &cx.pool);
    let outer_place = flow.local_of_decl(outer).expect("outer place");
    let inner_place = flow.local_of_decl(inner).expect("inner place");
    assert_ne!(outer_place, inner_place);
    // The read inside the arm resolves to the inner (shadowing) place.
    let facts = flow.facts_of(then_read).expect("read facts");
    assert_eq!(facts.read, Some(inner_place));
    // The outer place's lineage is untouched by the shadow.
    assert_eq!(flow.place_writes(outer_place), &[outer]);
}

#[test]
fn view_place_protects_owner_place() {
    let mut cx = Ctx::new();
    let str_ty = cx.pool.str_();
    let view_ty = cx.pool.str_view();
    let v_name = cx.pool.intern_str("v");
    let s_name = cx.pool.intern_str("s");
    let mut b = cx.builder();
    let lit = b.str_const(cx.pool.intern_str("hello"), str_ty, sp());
    let decl = b.var_decl(s_name, false, str_ty, lit, sp());
    let zero = b.int_const(0, cx.int, sp());
    let three = b.int_const(3, cx.int, sp());
    let base = b.var(s_name, str_ty, sp());
    let sl = b.slice(base, Some(zero), Some(three), view_ty, sp());
    let vdecl = b.var_decl(v_name, false, view_ty, sl, sp());
    let tir = b.finish(&[decl, vdecl]);

    let flow = Flow::build(&tir, &cx.pool);
    let owner = flow.local_of_decl(decl).expect("owner place");
    let facts = flow.facts_of(sl).expect("slice facts");
    let view = facts.view.expect("view place");
    assert!(matches!(flow.place(view), Place::View { base, .. } if *base == owner));
}

#[test]
fn diverging_call_terminates_block_and_continues_unreachable() {
    let mut cx = Ctx::new();
    let mut b = cx.builder();
    let callee = cx.pool.intern_str("die");
    let call = b.call(callee, &[], &[], cx.never, sp());
    let expr = b.unary(TirTag::ExprStmt, cx.void, call, sp());
    let one = b.int_const(1, cx.int, sp());
    let dead = b.var_decl(cx.x, false, cx.int, one, sp());
    let tir = b.finish(&[expr, dead]);

    let flow = Flow::build(&tir, &cx.pool);
    assert!(matches!(
        flow.term(flow.entry),
        Terminator::Call { call: c, .. } if *c == call
    ));
    let next = match flow.term(flow.entry) {
        Terminator::Call { next, .. } => *next,
        _ => unreachable!(),
    };
    assert!(flow.blocks[next.0 as usize].unreachable);
    assert!(flow.predecessors(next).is_empty());
    // Dead code still has steps and a terminator.
    assert!(step_insts(&flow, next).contains(&dead));
}

#[test]
fn value_match_switch_with_join_and_result_place() {
    let mut cx = Ctx::new();
    let int = cx.int;
    let mut b = cx.builder();
    let scrut = b.int_const(1, int, sp());
    let body = b.int_const(2, int, sp());
    let m = b.match_expr(
        scrut,
        int,
        &[TypedMatchArm {
            discriminant: crate::tir::Discriminant::Wildcard,
            bindings: &[],
            guards: &[],
            body: TypedMatchBody::Expr(body),
        }],
        sp(),
    );
    let two = b.int_const(2, int, sp());
    let use_m = b.binary(TirTag::IAdd, int, m, two, sp());
    let decl = b.var_decl(cx.x, false, int, use_m, sp());
    let tir = b.finish(&[decl]);

    let flow = Flow::build(&tir, &cx.pool);
    assert!(matches!(
        flow.term(flow.entry),
        Terminator::Switch { match_inst, join: Some(_), .. } if *match_inst == m
    ));
    let (arms, join) = match flow.term(flow.entry) {
        Terminator::Switch { arms, join, .. } => (*arms, join.unwrap()),
        _ => unreachable!(),
    };
    assert_eq!(arms.as_range().count(), 1);
    let arm_block = BlockId(flow.extra[arms.offset as usize]);
    assert!(matches!(
        flow.term(arm_block),
        Terminator::Goto { target } if *target == join
    ));
    assert_eq!(flow.predecessors(join), &[arm_block]);
    let result = flow.match_result(m).expect("match result place");
    assert!(matches!(flow.place(result), Place::MatchResult { .. }));
    // The consuming add evaluates in the join block, after the join.
    let add_block = flow
        .forward_order()
        .find(|blk| step_insts(&flow, *blk).contains(&use_m))
        .expect("add step");
    assert_eq!(add_block, join);
}

#[test]
fn all_diverging_match_has_no_join_and_unreachable_continuation() {
    let mut cx = Ctx::new();
    let int = cx.int;
    let mut b = cx.builder();
    let scrut = b.int_const(1, int, sp());
    let die = cx.pool.intern_str("die");
    let call = b.call(die, &[], &[], cx.never, sp());
    let m = b.match_expr(
        scrut,
        cx.never,
        &[TypedMatchArm {
            discriminant: crate::tir::Discriminant::Wildcard,
            bindings: &[],
            guards: &[],
            body: TypedMatchBody::Expr(call),
        }],
        sp(),
    );
    let expr = b.unary(TirTag::ExprStmt, cx.void, m, sp());
    let one = b.int_const(1, int, sp());
    let dead = b.var_decl(cx.x, false, int, one, sp());
    let tir = b.finish(&[expr, dead]);

    let flow = Flow::build(&tir, &cx.pool);
    assert!(matches!(
        flow.term(flow.entry),
        Terminator::Switch { match_inst, join: None, .. } if *match_inst == m
    ));
    let arm_block = match flow.term(flow.entry) {
        Terminator::Switch { arms, .. } => BlockId(flow.extra[arms.offset as usize]),
        _ => unreachable!(),
    };
    assert!(matches!(flow.term(arm_block), Terminator::Call { .. }));
    // The continuation (the dead decl) is unreachable.
    let dead_block = flow
        .forward_order()
        .find(|blk| step_insts(&flow, *blk).contains(&dead))
        .expect("dead decl step");
    assert!(flow.blocks[dead_block.0 as usize].unreachable);
    assert!(flow.match_result(m).is_none());
}

#[test]
fn match_by_move_token_places_are_named_and_arm_scoped() {
    let mut cx = Ctx::new();
    let str_ty = cx.pool.str_();
    let v = cx.pool.intern_str("v");
    let f0n = cx.pool.intern_str("field0");
    let pair_ty = cx.pool.anon_struct(&[(f0n, str_ty)]);
    let mut b = cx.builder();
    let lit = b.str_const(cx.pool.intern_str("payload"), str_ty, sp());
    let c2 = b.str_const(cx.pool.intern_str("other"), str_ty, sp());
    let scrut = b.struct_lit(pair_ty, &[(0, lit), (1, c2)], sp());
    let body_read = b.var(v, str_ty, sp());
    let body_expr = b.unary(TirTag::ExprStmt, cx.void, body_read, sp());
    let body = [body_expr];
    // Owner token: FieldAccess immediately before the match.
    let token = b.field_access(scrut, 0, str_ty, sp());
    let m = b.match_expr(
        scrut,
        cx.void,
        &[TypedMatchArm {
            discriminant: crate::tir::Discriminant::Wildcard,
            bindings: &[crate::tir::MatchBinding {
                field_index: 0,
                name: v,
                ty: str_ty,
            }],
            guards: &[],
            body: TypedMatchBody::Stmts(&body),
        }],
        sp(),
    );
    let _ = token;
    let tir = b.finish(&[m]);

    let flow = Flow::build(&tir, &cx.pool);
    let tokens = flow.token_places(m);
    assert_eq!(tokens.len(), 1);
    assert!(matches!(flow.place(tokens[0]), Place::Token { name, .. } if *name == v));
    // The arm-body Var read of `v` resolves to the token place.
    let facts = flow.facts_of(body_read).expect("body read facts");
    assert_eq!(facts.read, Some(tokens[0]));
}

#[test]
fn elif_with_match_condition_records_branch_holding_blocks() {
    let mut cx = Ctx::new();
    let bool_ty = cx.pool.bool_();
    let mut b = cx.builder();
    let first_cond = b.bool_const(false, bool_ty, sp());
    let scrut = b.bool_const(true, bool_ty, sp());
    let arm_t = b.bool_const(true, bool_ty, sp());
    let arm_f = b.bool_const(false, bool_ty, sp());
    let m = b.match_expr(
        scrut,
        bool_ty,
        &[
            TypedMatchArm {
                discriminant: crate::tir::Discriminant::Bool(true),
                bindings: &[],
                guards: &[],
                body: TypedMatchBody::Expr(arm_t),
            },
            TypedMatchArm {
                discriminant: crate::tir::Discriminant::Bool(false),
                bindings: &[],
                guards: &[],
                body: TypedMatchBody::Expr(arm_f),
            },
        ],
        sp(),
    );
    let ifs = b.if_stmt(
        first_cond,
        &[],
        &[(m, Vec::new())],
        Some(&[]),
        cx.void,
        sp(),
    );
    let tir = b.finish(&[ifs]);

    let flow = Flow::build(&tir, &cx.pool);
    let arms = flow.branch_arm_blocks(ifs);
    // The recorded elif block holds the elif's Branch — for a match
    // condition that is the match's JOIN block, not the condition entry.
    let elif_branch_block = arms.elif_blocks[0];
    assert!(matches!(
        flow.term(elif_branch_block),
        Terminator::Branch { cond, .. } if *cond == m
    ));
    let match_block = flow.term_block_of(m).expect("match terminator block");
    let switch_join = match flow.term(match_block) {
        Terminator::Switch { join: Some(j), .. } => *j,
        other => panic!("match must dispatch with a join, got {:?}", other),
    };
    assert_eq!(switch_join, elif_branch_block);
    // The else chain: first Branch's else_ enters the elif's condition;
    // the recorded branch-holding block is reachable from there through
    // the condition's own blocks (its match join), and its Branch's
    // else_ is the else block.
    let first_branch_block = flow.term_block_of(ifs).expect("if terminator block");
    let cond_entry = match flow.term(first_branch_block) {
        Terminator::Branch { else_, .. } => *else_,
        other => panic!("if must branch, got {:?}", other),
    };
    let mut seen = std::collections::HashSet::new();
    let mut stack = vec![cond_entry];
    let mut reaches = false;
    while let Some(b) = stack.pop() {
        if !seen.insert(b) {
            continue;
        }
        if b == elif_branch_block {
            reaches = true;
            break;
        }
        stack.extend_from_slice(flow.successors(b));
    }
    assert!(
        reaches,
        "condition entry must reach the recorded elif block"
    );
    let else_target = match flow.term(elif_branch_block) {
        Terminator::Branch { else_, .. } => *else_,
        other => panic!("elif must branch, got {:?}", other),
    };
    assert_eq!(else_target, arms.else_block);
}

#[test]
fn bool_and_places_rhs_in_conditional_block() {
    let mut cx = Ctx::new();
    let bool_ty = cx.pool.bool_();
    let mut b = cx.builder();
    let probe = cx.pool.intern_str("probe");
    let f = b.bool_const(false, bool_ty, sp());
    let call = b.call(probe, &[], &[], bool_ty, sp());
    let and = b.binary(TirTag::BoolAnd, bool_ty, f, call, sp());
    let decl = b.var_decl(cx.x, false, bool_ty, and, sp());
    let tir = b.finish(&[decl]);

    let flow = Flow::build(&tir, &cx.pool);
    let (rhs_block, join) = match flow.term(flow.entry) {
        Terminator::Branch { cond, then_, else_ } => {
            assert_eq!(*cond, f);
            (*then_, *else_)
        }
        other => panic!("and must cut a conditional branch, got {:?}", other),
    };
    // The RHS runs only on the taken edge, never unconditionally.
    assert_eq!(flow.step_block_of(call), Some(rhs_block));
    assert_eq!(flow.predecessors(rhs_block), &[flow.entry]);
    // The merged value and its consumer live in the join block, whose
    // two predecessors are the short-circuit edge and the RHS block.
    assert_eq!(flow.predecessors(join).len(), 2);
    assert_eq!(flow.step_block_of(and), Some(join));
    assert_eq!(flow.step_block_of(decl), Some(join));
}

#[test]
fn bool_or_short_circuits_on_true() {
    let mut cx = Ctx::new();
    let bool_ty = cx.pool.bool_();
    let mut b = cx.builder();
    let probe = cx.pool.intern_str("probe");
    let t = b.bool_const(true, bool_ty, sp());
    let call = b.call(probe, &[], &[], bool_ty, sp());
    let or = b.binary(TirTag::BoolOr, bool_ty, t, call, sp());
    let decl = b.var_decl(cx.x, false, bool_ty, or, sp());
    let tir = b.finish(&[decl]);

    let flow = Flow::build(&tir, &cx.pool);
    let (join, rhs_block) = match flow.term(flow.entry) {
        Terminator::Branch { cond, then_, else_ } => {
            assert_eq!(*cond, t);
            (*then_, *else_)
        }
        other => panic!("or must cut a conditional branch, got {:?}", other),
    };
    assert_eq!(flow.step_block_of(call), Some(rhs_block));
    assert_eq!(flow.step_block_of(or), Some(join));
    assert_eq!(flow.predecessors(join).len(), 2);
}

#[test]
fn for_bounds_steps_live_in_preheader() {
    let mut cx = Ctx::new();
    let int = cx.int;
    let mut b = cx.builder();
    let start_name = cx.pool.intern_str("start");
    let start_call = b.call(start_name, &[], &[], int, sp());
    let end = b.int_const(3, int, sp());
    let f = b.for_range(cx.i, start_call, end, &[], cx.void, sp());
    let tir = b.finish(&[f]);

    let flow = Flow::build(&tir, &cx.pool);
    // Bounds evaluate once, in the block holding the `For` terminator.
    assert_eq!(flow.step_block_of(start_call), Some(flow.entry));
    assert_eq!(flow.step_block_of(end), Some(flow.entry));
    let header = match flow.term(flow.entry) {
        Terminator::For { header, .. } => *header,
        other => panic!("for must enter a header, got {:?}", other),
    };
    // The header holds only the test — the backedge must not re-run
    // bound evaluation.
    assert!(flow.block_steps(header).is_empty());
    assert!(matches!(flow.term(header), Terminator::ForTest { .. }));
    let header_preds = flow.predecessors(header);
    assert_eq!(header_preds.len(), 2);
    // The latch backedge names the induction increment explicitly.
    let latch = header_preds
        .iter()
        .copied()
        .find(|p| *p != flow.entry)
        .expect("latch predecessor");
    let var = flow.loop_var(f).expect("loop var place");
    assert!(matches!(
        flow.term(latch),
        Terminator::Backedge { to, inc } if *to == header && *inc == Some(var)
    ));
}

#[test]
fn field_assign_records_field_replace_fact() {
    let mut cx = Ctx::new();
    let str_ty = cx.pool.str_();
    let f0 = cx.pool.intern_str("f0");
    let f1 = cx.pool.intern_str("f1");
    let pair_ty = cx.pool.anon_struct(&[(f0, str_ty), (f1, str_ty)]);
    let mut b = cx.builder();
    let a = b.str_const(cx.pool.intern_str("a"), str_ty, sp());
    let c = b.str_const(cx.pool.intern_str("c"), str_ty, sp());
    let lit = b.struct_lit(pair_ty, &[(0, a), (1, c)], sp());
    let decl = b.var_decl(cx.x, true, pair_ty, lit, sp());
    let base = b.var(cx.x, pair_ty, sp());
    let target = b.field_access(base, 1, str_ty, sp());
    let newv = b.str_const(cx.pool.intern_str("d"), str_ty, sp());
    let fa = b.field_assign(target, newv, cx.void, sp());
    let tir = b.finish(&[decl, fa]);

    let flow = Flow::build(&tir, &cx.pool);
    let facts = flow.facts_of(fa).expect("field assign facts");
    let place = flow.local_of_decl(decl).expect("decl place");
    let write = facts.write.expect("write");
    assert!(matches!(write.kind, WriteKind::FieldReplace { field: 1 }));
    assert_eq!(write.place, place);
}

#[test]
fn bool_and_chain_split_pins_join_pred_shape() {
    let mut cx = Ctx::new();
    let bool_ty = cx.pool.bool_();
    let mut b = cx.builder();
    let probe = cx.pool.intern_str("probe");
    let f = b.bool_const(false, bool_ty, sp());
    let call1 = b.call(probe, &[], &[], bool_ty, sp());
    let inner = b.binary(TirTag::BoolAnd, bool_ty, f, call1, sp());
    let call2 = b.call(probe, &[], &[], bool_ty, sp());
    let outer = b.binary(TirTag::BoolAnd, bool_ty, inner, call2, sp());
    let decl = b.var_decl(cx.x, false, bool_ty, outer, sp());
    let tir = b.finish(&[decl]);

    let flow = Flow::build(&tir, &cx.pool);
    // Each split: the join's two predecessors are the split block (a
    // Branch whose other target is the rhs block) and the rhs block
    // (which falls through to the join); the merged value is the join
    // block's own first step.
    for merge in [inner, outer] {
        let join = flow.step_block_of(merge).expect("merge step block");
        let preds = flow.predecessors(join);
        assert_eq!(preds.len(), 2);
        let split = preds
            .iter()
            .copied()
            .find(|p| matches!(flow.term(*p), Terminator::Branch { .. }))
            .expect("split predecessor");
        let Terminator::Branch { then_, else_, .. } = flow.term(split) else {
            unreachable!()
        };
        let rhs = if *then_ == join { *else_ } else { *then_ };
        assert_ne!(rhs, join);
        assert!(matches!(flow.term(rhs), Terminator::Goto { target } if *target == join));
        assert!(preds.contains(&rhs));
        assert_eq!(flow.block_steps(join)[0].inst, merge);
    }
}

#[test]
fn bool_and_if_condition_branches_from_the_merge_join() {
    let mut cx = Ctx::new();
    let bool_ty = cx.pool.bool_();
    let mut b = cx.builder();
    let probe = cx.pool.intern_str("probe");
    let f = b.bool_const(false, bool_ty, sp());
    let call = b.call(probe, &[], &[], bool_ty, sp());
    let and = b.binary(TirTag::BoolAnd, bool_ty, f, call, sp());
    let one = b.int_const(1, cx.int, sp());
    let then_d = b.var_decl(cx.x, false, cx.int, one, sp());
    let two = b.int_const(2, cx.int, sp());
    let else_d = b.var_decl(cx.x, false, cx.int, two, sp());
    let ifs = b.if_stmt(and, &[then_d], &[], Some(&[else_d]), cx.void, sp());
    let tir = b.finish(&[ifs]);

    let flow = Flow::build(&tir, &cx.pool);
    // The IfStmt's Branch lives IN the and-join: the merged value is
    // stepped in the very block that branches on it, and the Branch's
    // targets are the arm blocks (never the join).
    let and_join = flow.step_block_of(and).expect("and merge step");
    let (cond, then_, else_) = match flow.term(and_join) {
        Terminator::Branch { cond, then_, else_ } => (*cond, *then_, *else_),
        other => panic!("if must branch from the and-join, got {:?}", other),
    };
    assert_eq!(cond, and);
    assert_ne!(then_, and_join);
    assert_ne!(else_, and_join);
    // The and-join still carries exactly the split's two predecessors.
    assert_eq!(flow.predecessors(and_join).len(), 2);
    assert!(flow.predecessors(and_join).iter().all(|p| matches!(
        flow.term(*p),
        Terminator::Branch { .. } | Terminator::Goto { .. }
    )));
}

#[test]
fn for_and_while_backedges_resolve_to_their_test_headers() {
    let mut cx = Ctx::new();
    let int = cx.int;
    let bool_ty = cx.pool.bool_();
    let mut b = cx.builder();
    let t = cx.pool.intern_str("t");
    let cond = b.var(t, bool_ty, sp());
    let zero = b.int_const(0, int, sp());
    let three = b.int_const(3, int, sp());
    let f = b.for_range(cx.i, zero, three, &[], cx.void, sp());
    let whl = b.while_loop(cond, &[f], cx.void, sp());
    let tir = b.finish(&[whl]);

    let flow = Flow::build(&tir, &cx.pool);
    // While shape: entry enters the header, which tests the condition.
    let w_header = match flow.term(flow.entry) {
        Terminator::While { header, .. } => *header,
        other => panic!("entry must enter the while header, got {:?}", other),
    };
    assert!(matches!(flow.term(w_header), Terminator::WhileTest { .. }));
    // For shape: the bounds are stepped in the block holding the `For`
    // terminator; the header holds only its ForTest.
    let (pre, f_header) = flow
        .forward_order()
        .find_map(|blk| match flow.term(blk) {
            Terminator::For { header, .. } => Some((blk, *header)),
            _ => None,
        })
        .expect("for pre-header");
    assert_eq!(flow.step_block_of(zero), Some(pre));
    assert_eq!(flow.step_block_of(three), Some(pre));
    assert!(flow.block_steps(f_header).is_empty());
    let var = flow.loop_var(f).expect("loop var place");
    assert!(matches!(
        flow.term(f_header),
        Terminator::ForTest { var: fv, .. } if *fv == var
    ));
    // Each backedge names its own loop's test shape.
    let mut saw_for_backedge = false;
    let mut saw_while_backedge = false;
    for blk in flow.forward_order() {
        match flow.term(blk) {
            Terminator::Backedge { to, inc: Some(v) } => {
                assert_eq!(*v, var);
                assert_eq!(*to, f_header);
                saw_for_backedge = true;
            }
            Terminator::Backedge { to, inc: None } => {
                assert_eq!(*to, w_header);
                saw_while_backedge = true;
            }
            _ => {}
        }
    }
    assert!(saw_for_backedge && saw_while_backedge);
}

#[test]
fn match_result_place_tracks_value_void_and_never_shapes() {
    let mut cx = Ctx::new();
    let int = cx.int;
    let mut b = cx.builder();
    // Value match: a join and a non-void, non-never type -> result place.
    let scrut_v = b.int_const(1, int, sp());
    let body_v = b.int_const(2, int, sp());
    let m_value = b.match_expr(
        scrut_v,
        int,
        &[TypedMatchArm {
            discriminant: crate::tir::Discriminant::Wildcard,
            bindings: &[],
            guards: &[],
            body: TypedMatchBody::Expr(body_v),
        }],
        sp(),
    );
    let two = b.int_const(2, int, sp());
    let use_m = b.binary(TirTag::IAdd, int, m_value, two, sp());
    let decl = b.var_decl(cx.x, false, int, use_m, sp());
    // Void statement match: a join, but no result place.
    let scrut_s = b.int_const(3, int, sp());
    let read_s = b.var(cx.x, int, sp());
    let stmt_s = b.unary(TirTag::ExprStmt, cx.void, read_s, sp());
    let stmts_s = [stmt_s];
    let m_void = b.match_expr(
        scrut_s,
        cx.void,
        &[TypedMatchArm {
            discriminant: crate::tir::Discriminant::Wildcard,
            bindings: &[],
            guards: &[],
            body: TypedMatchBody::Stmts(&stmts_s),
        }],
        sp(),
    );
    // Never match: every arm diverges -> no join, no result place.
    let scrut_n = b.int_const(4, int, sp());
    let die = cx.pool.intern_str("die");
    let call_n = b.call(die, &[], &[], cx.never, sp());
    let m_never = b.match_expr(
        scrut_n,
        cx.never,
        &[TypedMatchArm {
            discriminant: crate::tir::Discriminant::Wildcard,
            bindings: &[],
            guards: &[],
            body: TypedMatchBody::Expr(call_n),
        }],
        sp(),
    );
    let tir = b.finish(&[decl, m_void, m_never]);

    let flow = Flow::build(&tir, &cx.pool);
    // Value match: exactly one result place, typed at the match type.
    let place = flow
        .match_result(m_value)
        .expect("value match result place");
    assert!(matches!(flow.place(place), Place::MatchResult { ty, .. } if *ty == int));
    assert!(matches!(
        flow.term(flow.entry),
        Terminator::Switch { match_inst, join: Some(_), .. } if *match_inst == m_value
    ));
    // Void match: join but no result place.
    let void_switch = flow
        .forward_order()
        .find(|blk| matches!(flow.term(*blk), Terminator::Switch { match_inst, .. } if *match_inst == m_void))
        .expect("void match switch");
    assert!(matches!(
        flow.term(void_switch),
        Terminator::Switch { join: Some(_), .. }
    ));
    assert!(flow.match_result(m_void).is_none());
    // Never match: no join, no result place.
    let never_switch = flow
        .forward_order()
        .find(|blk| matches!(flow.term(*blk), Terminator::Switch { match_inst, .. } if *match_inst == m_never))
        .expect("never match switch");
    assert!(matches!(
        flow.term(never_switch),
        Terminator::Switch { join: None, .. }
    ));
    assert!(flow.match_result(m_never).is_none());
}

#[test]
fn bool_or_with_nested_and_rhs_reaches_join_through_nested_blocks() {
    let mut cx = Ctx::new();
    let bool_ty = cx.pool.bool_();
    let int = cx.int;
    let mut b = cx.builder();
    // `c == 45 or (c >= 48 and c <= 57)`: the or's rhs block holds the
    // nested and-split, so the or-join's fallthrough predecessor is the
    // nested and-join, and the or's rhs block reaches the or-join
    // through the nested construct's blocks (the rhs block itself does
    // NOT jump to the or-join).
    let cval1 = b.var(cx.x, int, sp());
    let c45 = b.int_const(45, int, sp());
    let eq45 = b.binary(TirTag::ICmpEq, bool_ty, cval1, c45, sp());
    let cval2 = b.var(cx.x, int, sp());
    let c48 = b.int_const(48, int, sp());
    let ge = b.binary(TirTag::ICmpGe, bool_ty, cval2, c48, sp());
    let cval3 = b.var(cx.x, int, sp());
    let c57 = b.int_const(57, int, sp());
    let le = b.binary(TirTag::ICmpLe, bool_ty, cval3, c57, sp());
    let and = b.binary(TirTag::BoolAnd, bool_ty, ge, le, sp());
    let or = b.binary(TirTag::BoolOr, bool_ty, eq45, and, sp());
    let decl = b.var_decl(cx.i, false, bool_ty, or, sp());
    let tir = b.finish(&[decl]);

    let flow = Flow::build(&tir, &cx.pool);
    // The and-join holds the merged and value; the or-join holds the
    // merged or value as its first step.
    let and_join = flow.step_block_of(and).expect("and merge step");
    let or_join = flow.step_block_of(or).expect("or merge step");
    assert_eq!(flow.block_steps(or_join)[0].inst, or);
    // The or-join's two predecessors: the or-split block and the
    // fallthrough from the nested and-join (not from the or's rhs
    // block directly).
    let or_preds = flow.predecessors(or_join);
    assert_eq!(or_preds.len(), 2);
    assert!(or_preds.contains(&and_join));
    assert!(matches!(flow.term(and_join), Terminator::Goto { target } if *target == or_join));
    // The or-split's rhs block is the nested and's split block; from it
    // the or-join is reachable through the nested construct.
    let split = or_preds
        .iter()
        .copied()
        .find(|p| matches!(flow.term(*p), Terminator::Branch { .. }))
        .expect("or split predecessor");
    let Terminator::Branch { then_, else_, .. } = flow.term(split) else {
        unreachable!()
    };
    let rhs = if *then_ == or_join { *else_ } else { *then_ };
    assert_ne!(rhs, and_join);
    let mut seen = std::collections::HashSet::new();
    let mut stack = vec![rhs];
    let mut reaches = false;
    while let Some(blk) = stack.pop() {
        if !seen.insert(blk) {
            continue;
        }
        if blk == or_join {
            reaches = true;
            break;
        }
        stack.extend_from_slice(flow.successors(blk));
    }
    assert!(reaches, "or rhs block must reach the or-join");
}

// ---------- fire_points (shared firing-point resolver) ----------

use super::fire_points::{AfterStepFire, FirePoint, FirePoints, FireVerdict};

fn pt(block: BlockId, ordinal: u32) -> FirePoint {
    FirePoint { block, ordinal }
}

#[test]
fn fire_points_basic_positions_resolve_exactly() {
    let mut cx = Ctx::new();
    let int = cx.int;
    let mut b = cx.builder();
    let one = b.int_const(1, int, sp());
    let two = b.int_const(2, int, sp());
    let sum = b.binary(TirTag::IAdd, int, one, two, sp());
    let decl = b.var_decl(cx.x, false, int, sum, sp());
    let tir = b.finish(&[decl]);

    let flow = Flow::build(&tir, &cx.pool);
    let fp = FirePoints::new(&flow);
    let entry = flow.entry;
    // Block entry, before-step, and the terminator point.
    assert_eq!(
        fp.resolve(DropPosition::BlockStart { block: entry }),
        FireVerdict::Resolved(pt(entry, 0))
    );
    assert_eq!(
        fp.resolve(DropPosition::BeforeStep { inst: sum }),
        FireVerdict::Resolved(pt(entry, 2))
    );
    assert_eq!(
        fp.resolve(DropPosition::PreTerminator { block: entry }),
        FireVerdict::Resolved(pt(entry, 4))
    );
}

#[test]
fn fire_points_after_step_inside_expression_reanchors_to_statement_root() {
    let mut cx = Ctx::new();
    let int = cx.int;
    let mut b = cx.builder();
    let one = b.int_const(1, int, sp());
    let two = b.int_const(2, int, sp());
    let sum = b.binary(TirTag::IAdd, int, one, two, sp());
    let decl = b.var_decl(cx.x, false, int, sum, sp());
    let tir = b.finish(&[decl]);

    let flow = Flow::build(&tir, &cx.pool);
    let fp = FirePoints::new(&flow);
    let entry = flow.entry;
    // The anchor is an operand deep inside the initializer: the event
    // resolves to the CONSUMING statement's boundary (after the
    // VarDecl step), never between the operand and its consumer.
    for anchor in [one, two, sum] {
        assert_eq!(
            fp.resolve_after_step(anchor),
            AfterStepFire::StatementBoundary(pt(entry, 4)),
            "anchor {anchor:?} resolves to the enclosing statement boundary"
        );
    }
    // The anchor IS the statement root: same boundary.
    assert_eq!(
        fp.resolve_after_step(decl),
        AfterStepFire::StatementBoundary(pt(entry, 4))
    );
    // resolve() exposes the same point as an exact verdict.
    assert_eq!(
        fp.resolve(DropPosition::AfterStep { inst: one }),
        FireVerdict::Resolved(pt(entry, 4))
    );
}

#[test]
fn fire_points_after_step_uses_first_root_at_or_after_anchor() {
    let mut cx = Ctx::new();
    let int = cx.int;
    let mut b = cx.builder();
    // Two statements: the anchor lives in the SECOND statement's
    // operand tree — the boundary is after statement 2, not statement 1.
    let a = b.int_const(1, int, sp());
    let decl_a = b.var_decl(cx.x, false, int, a, sp());
    let c = b.int_const(3, int, sp());
    let d = b.int_const(4, int, sp());
    let sum2 = b.binary(TirTag::IAdd, int, c, d, sp());
    let decl_b = b.var_decl(cx.i, false, int, sum2, sp());
    let tir = b.finish(&[decl_a, decl_b]);

    let flow = Flow::build(&tir, &cx.pool);
    let fp = FirePoints::new(&flow);
    let entry = flow.entry;
    // steps: [a, decl_a, c, d, sum2, decl_b] — decl_b at index 5.
    assert_eq!(
        fp.resolve_after_step(c),
        AfterStepFire::StatementBoundary(pt(entry, 6))
    );
    // An anchor in the FIRST statement resolves to the FIRST boundary.
    assert_eq!(
        fp.resolve_after_step(a),
        AfterStepFire::StatementBoundary(pt(entry, 2))
    );
    let _ = decl_a;
}

#[test]
fn fire_points_after_step_without_following_root_is_block_terminator() {
    let mut cx = Ctx::new();
    let bool_ty = cx.pool.bool_();
    let t = cx.pool.intern_str("t");
    let mut b = cx.builder();
    // A while condition block holds only operand steps (the condition
    // tree's parent is the loop payload): no root at/after the anchor.
    let cond = b.var(t, bool_ty, sp());
    let body_read = b.var(t, bool_ty, sp());
    let body_expr = b.unary(TirTag::ExprStmt, cx.void, body_read, sp());
    let whl = b.while_loop(cond, &[body_expr], cx.void, sp());
    let tir = b.finish(&[whl]);

    let flow = Flow::build(&tir, &cx.pool);
    let fp = FirePoints::new(&flow);
    let header = match flow.term(flow.entry) {
        Terminator::While { header, .. } => *header,
        other => panic!("entry terminates in the loop: {other:?}"),
    };
    // The header has one step (the condition read) and no statement
    // root: the anchor resolves to the header's terminator point.
    assert_eq!(
        fp.resolve_after_step(cond),
        AfterStepFire::BlockTerminator(pt(header, 1))
    );
    assert_eq!(
        fp.resolve(DropPosition::AfterStep { inst: cond }),
        FireVerdict::Resolved(pt(header, 1))
    );
}

#[test]
fn fire_points_if_payload_resolves_every_arm_continuation() {
    let mut cx = Ctx::new();
    let int = cx.int;
    let mut b = cx.builder();
    let zero = b.int_const(0, int, sp());
    let decl = b.var_decl(cx.x, true, int, zero, sp());
    let cond = b.var(cx.x, int, sp());
    let one = b.int_const(1, int, sp());
    let then_a = b.assign(cx.x, int, one, sp());
    let two = b.int_const(2, int, sp());
    let else_a = b.assign(cx.x, int, two, sp());
    let ifs = b.if_stmt(cond, &[then_a], &[], Some(&[else_a]), cx.void, sp());
    let tir = b.finish(&[decl, ifs]);

    let flow = Flow::build(&tir, &cx.pool);
    let fp = FirePoints::new(&flow);
    // Both arms fall through: one completion point — the join.
    let then_block = flow.successors(flow.entry)[0];
    let join = match flow.term(then_block) {
        Terminator::Goto { target } => *target,
        other => panic!("then arm must fall through: {other:?}"),
    };
    assert_eq!(
        fp.resolve_after_step(ifs),
        AfterStepFire::Payload(FireVerdict::ResolvedPerArm(vec![pt(join, 0)]))
    );
}

#[test]
fn fire_points_if_payload_partial_divergence_skips_diverging_arms() {
    let mut cx = Ctx::new();
    let int = cx.int;
    let mut b = cx.builder();
    let zero = b.int_const(0, int, sp());
    let decl = b.var_decl(cx.x, true, int, zero, sp());
    let cond = b.bool_const(true, cx.pool.bool_(), sp());
    // Then arm diverges (returns); else arm falls through.
    let ret = b.return_void(cx.void, sp());
    let one = b.int_const(1, int, sp());
    let else_a = b.assign(cx.x, int, one, sp());
    let ifs = b.if_stmt(cond, &[ret], &[], Some(&[else_a]), cx.void, sp());
    let tir = b.finish(&[decl, ifs]);

    let flow = Flow::build(&tir, &cx.pool);
    let fp = FirePoints::new(&flow);
    let else_block = match flow.term(flow.entry) {
        Terminator::Branch { else_, .. } => *else_,
        other => panic!("entry branches: {other:?}"),
    };
    let join = match flow.term(else_block) {
        Terminator::Goto { target } => *target,
        other => panic!("else arm falls through: {other:?}"),
    };
    // Only the falling-through arm contributes its continuation.
    assert_eq!(
        fp.resolve_after_step(ifs),
        AfterStepFire::Payload(FireVerdict::ResolvedPerArm(vec![pt(join, 0)]))
    );
}

#[test]
fn fire_points_if_payload_all_diverging_never_fires() {
    let mut cx = Ctx::new();
    let mut b = cx.builder();
    let cond = b.bool_const(true, cx.pool.bool_(), sp());
    let ret_then = b.return_void(cx.void, sp());
    let ret_else = b.return_void(cx.void, sp());
    let ifs = b.if_stmt(cond, &[ret_then], &[], Some(&[ret_else]), cx.void, sp());
    let tir = b.finish(&[ifs]);

    let flow = Flow::build(&tir, &cx.pool);
    let fp = FirePoints::new(&flow);
    // Every arm diverges: the payload can never fire (empty verdict).
    assert_eq!(
        fp.resolve_after_step(ifs),
        AfterStepFire::Payload(FireVerdict::ResolvedPerArm(vec![]))
    );
}

#[test]
fn fire_points_match_payload_resolves_the_join() {
    let mut cx = Ctx::new();
    let int = cx.int;
    let mut b = cx.builder();
    let scrut = b.int_const(1, int, sp());
    let body = b.int_const(2, int, sp());
    let m = b.match_expr(
        scrut,
        int,
        &[TypedMatchArm {
            discriminant: crate::tir::Discriminant::Wildcard,
            bindings: &[],
            guards: &[],
            body: TypedMatchBody::Expr(body),
        }],
        sp(),
    );
    let tir = b.finish(&[m]);

    let flow = Flow::build(&tir, &cx.pool);
    let fp = FirePoints::new(&flow);
    let Terminator::Switch {
        join: Some(join), ..
    } = flow.term(flow.entry)
    else {
        panic!("value match terminates in a switch with a join")
    };
    assert_eq!(
        fp.resolve_after_step(m),
        AfterStepFire::Payload(FireVerdict::Resolved(pt(*join, 0)))
    );
}

#[test]
fn fire_points_loop_payload_resolves_the_exit_block() {
    let mut cx = Ctx::new();
    let bool_ty = cx.pool.bool_();
    let t = cx.pool.intern_str("t");
    let mut b = cx.builder();
    let cond = b.var(t, bool_ty, sp());
    let body_read = b.var(t, bool_ty, sp());
    let body_expr = b.unary(TirTag::ExprStmt, cx.void, body_read, sp());
    let whl = b.while_loop(cond, &[body_expr], cx.void, sp());
    let tir = b.finish(&[whl]);

    let flow = Flow::build(&tir, &cx.pool);
    let fp = FirePoints::new(&flow);
    let header = match flow.term(flow.entry) {
        Terminator::While { header, .. } => *header,
        other => panic!("entry terminates in the loop: {other:?}"),
    };
    let exit = match flow.term(header) {
        Terminator::WhileTest { exit, .. } => *exit,
        other => panic!("header terminates in the loop test: {other:?}"),
    };
    assert_eq!(
        fp.resolve_after_step(whl),
        AfterStepFire::Payload(FireVerdict::Resolved(pt(exit, 0)))
    );

    // A for loop resolves the same way.
    let mut cx = Ctx::new();
    let int = cx.int;
    let mut b = cx.builder();
    let zero = b.int_const(0, int, sp());
    let ten = b.int_const(10, int, sp());
    let read_i = b.var(cx.i, int, sp());
    let expr = b.unary(TirTag::ExprStmt, cx.void, read_i, sp());
    let f = b.for_range(cx.i, zero, ten, &[expr], cx.void, sp());
    let tir = b.finish(&[f]);
    let flow = Flow::build(&tir, &cx.pool);
    let fp = FirePoints::new(&flow);
    let header = match flow.term(flow.entry) {
        Terminator::For { header, .. } => *header,
        other => panic!("entry terminates in the loop: {other:?}"),
    };
    let exit = match flow.term(header) {
        Terminator::ForTest { exit, .. } => *exit,
        other => panic!("header terminates in the loop test: {other:?}"),
    };
    assert_eq!(
        fp.resolve_after_step(f),
        AfterStepFire::Payload(FireVerdict::Resolved(pt(exit, 0)))
    );
}

#[test]
fn fire_points_while_compound_condition_is_non_exact() {
    let mut cx = Ctx::new();
    let bool_ty = cx.pool.bool_();
    let t = cx.pool.intern_str("t");
    let mut b = cx.builder();
    // `while t and t:` — the and-split terminates the header block, so
    // the header's terminator is not the loop test: the documented
    // non-exact fallback (the payload block's terminator point).
    let lhs = b.var(t, bool_ty, sp());
    let rhs = b.var(t, bool_ty, sp());
    let cond = b.binary(TirTag::BoolAnd, bool_ty, lhs, rhs, sp());
    let body_read = b.var(t, bool_ty, sp());
    let body_expr = b.unary(TirTag::ExprStmt, cx.void, body_read, sp());
    let whl = b.while_loop(cond, &[body_expr], cx.void, sp());
    let tir = b.finish(&[whl]);

    let flow = Flow::build(&tir, &cx.pool);
    let fp = FirePoints::new(&flow);
    assert!(
        matches!(flow.term(flow.entry), Terminator::While { .. }),
        "the loop payload lives on the entry block"
    );
    assert!(!matches!(
        flow.term(flow.entry),
        Terminator::WhileTest { .. }
    ),);
    let header = match flow.term(flow.entry) {
        Terminator::While { header, .. } => *header,
        other => panic!("entry terminates in the loop: {other:?}"),
    };
    assert!(
        matches!(flow.term(header), Terminator::Branch { .. }),
        "compound condition: the header is an and-split, not the loop test"
    );
    // Entry holds no steps: the fallback point is (entry, 0), marked
    // NON-exact.
    assert_eq!(
        fp.resolve_after_step(whl),
        AfterStepFire::Payload(FireVerdict::NonExact(pt(flow.entry, 0)))
    );
    assert_eq!(
        fp.resolve(DropPosition::AfterStep { inst: whl }),
        FireVerdict::NonExact(pt(flow.entry, 0))
    );
}

#[test]
fn fire_points_return_payload_resolves_the_payload_terminator() {
    let mut cx = Ctx::new();
    let int = cx.int;
    let mut b = cx.builder();
    let one = b.int_const(1, int, sp());
    let ret = b.unary(TirTag::Return, cx.void, one, sp());
    let tir = b.finish(&[ret]);

    let flow = Flow::build(&tir, &cx.pool);
    let fp = FirePoints::new(&flow);
    // The return operand is a deferred operand (parent = the payload):
    // no root at/after it → the block terminator, exact.
    assert_eq!(
        fp.resolve_after_step(one),
        AfterStepFire::BlockTerminator(pt(flow.entry, 1))
    );
    // The payload anchor itself: the payload block's terminator point.
    assert_eq!(
        fp.resolve_after_step(ret),
        AfterStepFire::Payload(FireVerdict::Resolved(pt(flow.entry, 1)))
    );
}

#[test]
fn fire_points_unresolved_never_guesses() {
    let mut cx = Ctx::new();
    let int = cx.int;
    let mut b = cx.builder();
    let one = b.int_const(1, int, sp());
    let decl = b.var_decl(cx.x, false, int, one, sp());
    // A stray instruction never emitted into the Flow: neither a step
    // nor a terminator payload.
    let stray = b.int_const(99, int, sp());
    let tir = b.finish(&[decl]);

    let flow = Flow::build(&tir, &cx.pool);
    let fp = FirePoints::new(&flow);
    assert_eq!(fp.resolve_after_step(stray), AfterStepFire::Unresolved);
    assert_eq!(
        fp.resolve(DropPosition::AfterStep { inst: stray }),
        FireVerdict::Unresolved
    );
    assert_eq!(
        fp.resolve(DropPosition::BeforeStep { inst: stray }),
        FireVerdict::Unresolved
    );
    assert_eq!(
        fp.resolve(DropPosition::BlockStart {
            block: BlockId(999)
        }),
        FireVerdict::Unresolved
    );
    assert_eq!(
        fp.resolve(DropPosition::PreTerminator {
            block: BlockId(999)
        }),
        FireVerdict::Unresolved
    );
}
