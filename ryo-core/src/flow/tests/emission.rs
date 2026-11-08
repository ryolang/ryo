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
fn nested_match_arms_use_bounded_stack() {
    let mut cx = Ctx::new();
    let int = cx.int;
    let mut b = cx.builder();
    let mut root = b.int_const(0, int, sp());
    for value in 1..=50_000 {
        let scrutinee = b.int_const(value, int, sp());
        root = b.match_expr(
            scrutinee,
            int,
            &[TypedMatchArm {
                span: sp(),
                discriminant: crate::tir::Discriminant::Wildcard,
                bindings: &[],
                guards: &[],
                body: TypedMatchBody::Expr(root),
            }],
            sp(),
        );
    }
    let expr_stmt = b.unary(TirTag::ExprStmt, cx.void, root, sp());
    let tir = b.finish(&[expr_stmt]);

    let flow = Flow::build(&tir, &cx.pool);
    assert_eq!(flow.blocks.len(), 2 * 50_000 + 1);
    assert!(flow.match_result(root).is_some());
}

#[test]
fn sibling_match_arms_keep_independent_token_state() {
    let mut cx = Ctx::new();
    let str_ty = cx.pool.str_();
    let left = cx.pool.intern_str("left");
    let right = cx.pool.intern_str("right");
    let field = cx.pool.intern_str("field");
    let pair_ty = cx.pool.anon_struct(&[(field, str_ty), (field, str_ty)]);
    let mut b = cx.builder();
    let first = b.str_const(cx.pool.intern_str("first"), str_ty, sp());
    let second = b.str_const(cx.pool.intern_str("second"), str_ty, sp());
    let scrutinee = b.struct_lit(pair_ty, &[(0, first), (1, second)], sp());
    let left_read = b.var(left, str_ty, sp());
    let left_stmt = b.unary(TirTag::ExprStmt, cx.void, left_read, sp());
    let right_read = b.var(right, str_ty, sp());
    let right_stmt = b.unary(TirTag::ExprStmt, cx.void, right_read, sp());
    let left_token = b.field_access(scrutinee, 0, str_ty, sp());
    let right_token = b.field_access(scrutinee, 1, str_ty, sp());
    let m = b.match_expr(
        scrutinee,
        cx.void,
        &[
            TypedMatchArm {
                span: sp(),
                discriminant: crate::tir::Discriminant::Variant(0),
                bindings: &[crate::tir::MatchBinding {
                    field_index: 0,
                    name: left,
                    ty: str_ty,
                }],
                guards: &[],
                body: TypedMatchBody::Stmts(&[left_stmt]),
            },
            TypedMatchArm {
                span: sp(),
                discriminant: crate::tir::Discriminant::Variant(1),
                bindings: &[crate::tir::MatchBinding {
                    field_index: 1,
                    name: right,
                    ty: str_ty,
                }],
                guards: &[],
                body: TypedMatchBody::Stmts(&[right_stmt]),
            },
        ],
        sp(),
    );
    let tir = b.finish(&[m]);

    let flow = Flow::build(&tir, &cx.pool);
    let tokens = flow.token_places(m);
    assert_eq!(tokens.len(), 2);
    assert_ne!(tokens[0], tokens[1]);
    assert!(matches!(
        flow.place(tokens[0]),
        Place::Token { inst, name, .. } if *inst == left_token && *name == left
    ));
    assert!(matches!(
        flow.place(tokens[1]),
        Place::Token { inst, name, .. } if *inst == right_token && *name == right
    ));
    assert_eq!(flow.facts_of(left_read).unwrap().read, Some(tokens[0]));
    assert_eq!(flow.facts_of(right_read).unwrap().read, Some(tokens[1]));
}

#[test]
fn nested_projection_resolution_uses_bounded_stack() {
    let mut cx = Ctx::new();
    let int = cx.int;
    let mut b = cx.builder();
    let init = b.int_const(1, int, sp());
    let decl = b.var_decl(cx.x, false, int, init, sp());
    let mut projection = b.var(cx.x, int, sp());
    for _ in 0..50_000 {
        projection = b.field_access(projection, 0, int, sp());
    }
    let tir = b.finish(&[decl, projection]);

    let flow = Flow::build(&tir, &cx.pool);
    let base = flow.local_of_decl(decl).expect("local place");
    let final_step = flow
        .block_steps(flow.entry)
        .last()
        .expect("projection step");
    assert_eq!(final_step.inst, projection);
    assert_eq!(final_step.facts.read, Some(base));
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
fn deep_linear_expression_preserves_postorder() {
    let mut cx = Ctx::new();
    let int = cx.int;
    let mut b = cx.builder();
    let mut expr = b.int_const(0, int, sp());
    let mut expected = vec![expr];
    for value in 1..=50_000 {
        let rhs = b.int_const(value, int, sp());
        expr = b.binary(TirTag::IAdd, int, expr, rhs, sp());
        expected.push(rhs);
        expected.push(expr);
    }
    let decl = b.var_decl(cx.x, false, int, expr, sp());
    expected.push(decl);
    let tir = b.finish(&[decl]);

    let flow = Flow::build(&tir, &cx.pool);
    assert_eq!(flow.blocks.len(), 1);
    assert_eq!(step_insts(&flow, flow.entry), expected);
}

#[test]
fn statement_completion_preserves_binding_order() {
    let mut cx = Ctx::new();
    let int = cx.int;
    let field = cx.pool.intern_str("field");
    let pair = cx.pool.anon_struct(&[(field, int)]);
    let y = cx.pool.intern_str("y");
    let mut b = cx.builder();

    let initial = b.int_const(1, int, sp());
    let initial_lit = b.struct_lit(pair, &[(0, initial)], sp());
    let decl = b.var_decl(cx.x, true, pair, initial_lit, sp());

    let replacement = b.int_const(2, int, sp());
    let replacement_lit = b.struct_lit(pair, &[(0, replacement)], sp());
    let assign = b.assign(cx.x, pair, replacement_lit, sp());

    let target_base = b.var(cx.x, pair, sp());
    let target = b.field_access(target_base, 0, int, sp());
    let field_value = b.int_const(3, int, sp());
    let field_assign = b.field_assign(target, field_value, cx.void, sp());

    let rhs_value = b.int_const(4, int, sp());
    let rhs = b.struct_lit(pair, &[(0, rhs_value)], sp());
    let _owner_token = b.field_access(rhs, 0, int, sp());
    let destructure = b.destructure(rhs, &[(0, Some(y), int)], sp());

    let expr_read = b.var(y, int, sp());
    let expr_stmt = b.unary(TirTag::ExprStmt, cx.void, expr_read, sp());
    let return_read = b.var(y, int, sp());
    let ret = b.unary(TirTag::Return, cx.void, return_read, sp());
    let tir = b.finish(&[decl, assign, field_assign, destructure, expr_stmt, ret]);

    let flow = Flow::build(&tir, &cx.pool);
    assert_eq!(flow.blocks.len(), 1);
    assert_eq!(
        step_insts(&flow, flow.entry),
        [
            initial,
            initial_lit,
            decl,
            replacement,
            replacement_lit,
            assign,
            target_base,
            target,
            field_value,
            field_assign,
            rhs_value,
            rhs,
            destructure,
            expr_read,
            expr_stmt,
            return_read,
        ]
    );
    let local = flow.local_of_decl(decl).expect("declared local");
    let token = flow.token_places(destructure);
    assert_eq!(token.len(), 1);
    assert_eq!(flow.facts_of(assign).unwrap().write.unwrap().place, local);
    assert_eq!(
        flow.facts_of(field_assign).unwrap().write.unwrap().place,
        local
    );
    assert_eq!(flow.facts_of(expr_read).unwrap().read, Some(token[0]));
    assert_eq!(flow.facts_of(return_read).unwrap().read, Some(token[0]));
    assert!(matches!(
        flow.term(flow.entry),
        Terminator::Return { value } if *value == return_read
    ));
}

#[test]
fn error_shaped_duplicate_terminator_keeps_dead_continuation() {
    let mut cx = Ctx::new();
    let error = cx.pool.error_type();
    let int = cx.int;
    let mut b = cx.builder();
    let unreachable = b.unreachable(error, sp());
    let return_value = b.int_const(7, int, sp());
    let ret = b.unary(TirTag::Return, cx.void, return_value, sp());
    let tir = b.finish(&[unreachable, ret]);

    let flow = Flow::build(&tir, &cx.pool);
    let dead = BlockId(1);
    assert_eq!(flow.blocks.len(), 2);
    assert!(matches!(flow.term(flow.entry), Terminator::Unreachable));
    assert_eq!(step_insts(&flow, dead), [return_value]);
    assert!(matches!(
        flow.term(dead),
        Terminator::Return { value } if *value == return_value
    ));
    assert!(flow.blocks[dead.0 as usize].unreachable);
    assert!(flow.predecessors(dead).is_empty());
    assert_eq!(flow.term_block_of(unreachable), Some(flow.entry));
    assert_eq!(flow.term_block_of(ret), Some(dead));
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
