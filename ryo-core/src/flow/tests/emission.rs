use super::*;

#[test]
fn value_match_has_exact_block_step_and_place_layout() {
    let mut cx = Ctx::new();
    let int = cx.int;
    let mut b = cx.builder();
    let scrut = b.int_const(1, int, sp());
    let body = b.int_const(2, int, sp());
    let m = b.match_expr(
        scrut,
        int,
        &[TypedMatchArm {
            span: sp(),
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
    let entry = BlockId(0);
    let arm = BlockId(1);
    let join = BlockId(2);
    assert_eq!(flow.entry, entry);
    assert_eq!(flow.blocks.len(), 3);
    assert_eq!(flow.extra, [1]);
    assert_eq!(step_insts(&flow, entry), [scrut]);
    assert_eq!(step_insts(&flow, arm), [body]);
    assert_eq!(step_insts(&flow, join), [two, use_m, decl]);
    assert!(matches!(
        flow.term(entry),
        Terminator::Switch {
            match_inst,
            scrutinee,
            arms,
            join: Some(BlockId(2)),
        } if *match_inst == m && *scrutinee == scrut && arms.offset == 0 && arms.len == 1
    ));
    assert!(matches!(
        flow.term(arm),
        Terminator::Goto { target: BlockId(2) }
    ));
    assert!(matches!(flow.term(join), Terminator::ReturnVoid));
    assert_eq!(flow.term_block_of(m), Some(entry));
    assert_eq!(flow.step_block_of(use_m), Some(join));

    let result = PlaceId(0);
    let local = PlaceId(1);
    assert_eq!(flow.places.len(), 2);
    assert_eq!(flow.match_result(m), Some(result));
    assert!(matches!(
        flow.place(result),
        Place::MatchResult { match_inst, ty } if *match_inst == m && *ty == int
    ));
    assert_eq!(flow.local_of_decl(decl), Some(local));
    assert!(matches!(
        flow.place(local),
        Place::Local {
            decl: d,
            name,
            ty,
            mutable: false,
        } if *d == decl && *name == cx.x && *ty == int
    ));
    assert_eq!(flow.place_writes(result), &[]);
    assert_eq!(flow.place_writes(local), &[decl]);
}

#[test]
fn elif_match_condition_has_exact_branch_metadata() {
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
                span: sp(),
                discriminant: crate::tir::Discriminant::Bool(true),
                bindings: &[],
                guards: &[],
                body: TypedMatchBody::Expr(arm_t),
            },
            TypedMatchArm {
                span: sp(),
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
    assert_eq!(flow.entry, BlockId(0));
    assert_eq!(flow.blocks.len(), 9);
    assert_eq!(flow.extra, [5, 6]);
    let expected_steps: [&[TirRef]; 9] = [
        &[first_cond],
        &[],
        &[scrut],
        &[],
        &[],
        &[arm_t],
        &[arm_f],
        &[],
        &[],
    ];
    for (block, steps) in expected_steps.into_iter().enumerate() {
        assert_eq!(step_insts(&flow, BlockId(block as u32)), steps);
    }
    assert!(matches!(
        flow.term(BlockId(0)),
        Terminator::Branch {
            cond,
            then_: BlockId(1),
            else_: BlockId(2),
        } if *cond == first_cond
    ));
    assert!(matches!(
        flow.term(BlockId(1)),
        Terminator::Goto { target: BlockId(4) }
    ));
    assert!(matches!(
        flow.term(BlockId(2)),
        Terminator::Switch {
            match_inst,
            scrutinee,
            arms,
            join: Some(BlockId(7)),
        } if *match_inst == m && *scrutinee == scrut && arms.offset == 0 && arms.len == 2
    ));
    assert!(matches!(
        flow.term(BlockId(3)),
        Terminator::Goto { target: BlockId(4) }
    ));
    assert!(matches!(flow.term(BlockId(4)), Terminator::ReturnVoid));
    assert!(matches!(
        flow.term(BlockId(5)),
        Terminator::Goto { target: BlockId(7) }
    ));
    assert!(matches!(
        flow.term(BlockId(6)),
        Terminator::Goto { target: BlockId(7) }
    ));
    assert!(matches!(
        flow.term(BlockId(7)),
        Terminator::Branch {
            cond,
            then_: BlockId(8),
            else_: BlockId(3),
        } if *cond == m
    ));
    assert!(matches!(
        flow.term(BlockId(8)),
        Terminator::Goto { target: BlockId(4) }
    ));

    let arms = flow.branch_arm_blocks(ifs);
    assert_eq!(arms.then_block, BlockId(1));
    assert_eq!(arms.elif_blocks, [BlockId(7)]);
    assert_eq!(arms.else_block, BlockId(3));
    assert_eq!(arms.join, BlockId(4));
    assert_eq!(flow.term_block_of(ifs), Some(BlockId(0)));
    assert_eq!(flow.term_block_of(m), Some(BlockId(2)));
    assert_eq!(flow.places.len(), 1);
    assert_eq!(flow.match_result(m), Some(PlaceId(0)));
    assert!(matches!(
        flow.place(PlaceId(0)),
        Place::MatchResult { match_inst, ty } if *match_inst == m && *ty == bool_ty
    ));
    assert_eq!(flow.place_writes(PlaceId(0)), &[]);
}

#[test]
fn nested_loop_has_exact_terminator_and_write_layout() {
    let mut cx = Ctx::new();
    let int = cx.int;
    let bool_ty = cx.pool.bool_();
    let t = cx.pool.intern_str("t");
    let mut b = cx.builder();
    let cond = b.var(t, bool_ty, sp());
    let zero = b.int_const(0, int, sp());
    let three = b.int_const(3, int, sp());
    let f = b.for_range(cx.i, zero, three, &[], cx.void, sp());
    let whl = b.while_loop(cond, &[f], cx.void, sp());
    let tir = b.finish(&[whl]);

    let flow = Flow::build(&tir, &cx.pool);
    assert_eq!(flow.entry, BlockId(0));
    assert_eq!(flow.blocks.len(), 7);
    assert!(flow.extra.is_empty());
    let expected_steps: [&[TirRef]; 7] = [&[], &[cond], &[zero, three], &[], &[], &[], &[]];
    for (block, steps) in expected_steps.into_iter().enumerate() {
        assert_eq!(step_insts(&flow, BlockId(block as u32)), steps);
    }
    assert!(matches!(
        flow.term(BlockId(0)),
        Terminator::While {
            loop_inst,
            header: BlockId(1),
        } if *loop_inst == whl
    ));
    assert!(matches!(
        flow.term(BlockId(1)),
        Terminator::WhileTest {
            loop_inst,
            cond: c,
            body: BlockId(2),
            exit: BlockId(3),
        } if *loop_inst == whl && *c == cond
    ));
    assert!(matches!(
        flow.term(BlockId(2)),
        Terminator::For {
            loop_inst,
            header: BlockId(4),
        } if *loop_inst == f
    ));
    assert!(matches!(flow.term(BlockId(3)), Terminator::ReturnVoid));
    assert!(matches!(
        flow.term(BlockId(4)),
        Terminator::ForTest {
            loop_inst,
            var: PlaceId(0),
            end,
            body: BlockId(5),
            exit: BlockId(6),
        } if *loop_inst == f && *end == three
    ));
    assert!(matches!(
        flow.term(BlockId(5)),
        Terminator::Backedge {
            to: BlockId(4),
            inc: Some(PlaceId(0)),
        }
    ));
    assert!(matches!(
        flow.term(BlockId(6)),
        Terminator::Backedge {
            to: BlockId(1),
            inc: None,
        }
    ));
    assert_eq!(flow.term_block_of(whl), Some(BlockId(0)));
    assert_eq!(flow.term_block_of(f), Some(BlockId(2)));

    assert_eq!(flow.places.len(), 1);
    assert_eq!(flow.loop_var(f), Some(PlaceId(0)));
    assert!(matches!(
        flow.place(PlaceId(0)),
        Place::LoopVar {
            loop_inst,
            name,
            ty,
        } if *loop_inst == f && *name == cx.i && *ty == cx.void
    ));
    assert_eq!(flow.place_writes(PlaceId(0)), &[f]);
}
