use super::common::all_borrow;
use super::*;
use ryo_core::flow::{BlockId, Flow, Terminator};
use ryo_core::span::Span;
use ryo_core::tir::{Discriminant, TirBuilder, TypedMatchArm, TypedMatchBody};
use std::process::Command;

const DEEP_LIVENESS_CHILD: &str = "RYO_DEEP_LIVENESS_REPRO_CHILD";

struct Fixture {
    pool: InternPool,
    builder: TirBuilder,
    declarations: [TirRef; 2],
    slice: TirRef,
    root: TirRef,
}

fn base_fixture() -> Fixture {
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let view_ty = pool.str_view();
    let int_ty = pool.int();
    let void = pool.void();
    let main = pool.intern_str("main");
    let source = pool.intern_str("s");
    let view = pool.intern_str("v");
    let print = pool.intern_str("print");
    let span = Span::EMPTY;
    let mut builder = TirBuilder::new(main, vec![], void, span);
    let text = builder.str_const(pool.intern_str("source"), str_ty, span);
    let source_decl = builder.var_decl(source, false, str_ty, text, span);
    let source_read = builder.var(source, str_ty, span);
    let zero = builder.int_const(0, int_ty, span);
    let one = builder.int_const(1, int_ty, span);
    let slice = builder.slice(source_read, Some(zero), Some(one), view_ty, span);
    let view_decl = builder.var_decl(view, false, view_ty, slice, span);
    let deep_read = builder.var(view, view_ty, span);
    let deep_call = builder.call(print, &[deep_read], &all_borrow(&[deep_read]), void, span);
    Fixture {
        pool,
        builder,
        declarations: [source_decl, view_decl],
        slice,
        root: deep_call,
    }
}

fn finish_fixture(fixture: Fixture, final_call: TirRef, final_read: TirRef) {
    let Fixture {
        pool,
        builder,
        declarations,
        slice,
        root,
    } = fixture;
    let roots = [declarations[0], declarations[1], root, final_call];
    let tir = builder.finish(&roots);
    let flow = Flow::build(&tir, &pool);
    let liveness = collect_view_liveness(&tir, &flow, &pool, &LoopNesting::default());
    assert_eq!(
        liveness.last_use[slice.index()],
        Some(final_read),
        "the document-order read after the nested region must remain the last use"
    );
    assert!(
        liveness
            .arm_last_reads
            .values()
            .any(|reads| matches!(reads, Analysis::Present(_))),
        "nested match arms must retain their read analysis"
    );
    let mut sink = DiagSink::new();
    check(std::slice::from_ref(&tir), &[flow], &pool, &mut sink);
    assert!(
        sink.is_empty(),
        "deep control ownership must remain clean: {:?}",
        sink.into_diags()
    );
}

fn wrap_in_match(fixture: &mut Fixture, root: TirRef) -> TirRef {
    let scrutinee = fixture
        .builder
        .int_const(1, fixture.pool.int(), Span::EMPTY);
    fixture.builder.match_expr(
        scrutinee,
        fixture.pool.void(),
        &[TypedMatchArm {
            span: Span::EMPTY,
            discriminant: Discriminant::Wildcard,
            bindings: &[],
            guards: &[],
            body: TypedMatchBody::Stmts(&[root]),
        }],
        Span::EMPTY,
    )
}

fn add_final_read(fixture: &mut Fixture) -> (TirRef, TirRef) {
    let final_read = fixture.builder.var(
        fixture.pool.intern_str("v"),
        fixture.pool.str_view(),
        Span::EMPTY,
    );
    let final_call = fixture.builder.call(
        fixture.pool.intern_str("print"),
        &[final_read],
        &all_borrow(&[final_read]),
        fixture.pool.void(),
        Span::EMPTY,
    );
    (final_call, final_read)
}

fn nested_match_liveness(depth: usize) {
    let mut fixture = base_fixture();
    let mut root = fixture.root;
    for _ in 0..depth {
        root = wrap_in_match(&mut fixture, root);
    }
    fixture.root = root;
    let (final_call, final_read) = add_final_read(&mut fixture);
    finish_fixture(fixture, final_call, final_read);
}

fn alternating_control_liveness(depth: usize) {
    let mut fixture = base_fixture();
    let mut root = fixture.root;
    for _ in 0..depth {
        let loop_cond = fixture
            .builder
            .bool_const(false, fixture.pool.bool_(), Span::EMPTY);
        let loop_stmt =
            fixture
                .builder
                .while_loop(loop_cond, &[], fixture.pool.void(), Span::EMPTY);
        let if_cond = fixture
            .builder
            .bool_const(true, fixture.pool.bool_(), Span::EMPTY);
        root = fixture.builder.if_stmt(
            if_cond,
            &[loop_stmt, root],
            &[],
            None,
            fixture.pool.void(),
            Span::EMPTY,
        );
        root = wrap_in_match(&mut fixture, root);
    }
    fixture.root = root;
    let (final_call, final_read) = add_final_read(&mut fixture);
    finish_fixture(fixture, final_call, final_read);
}

fn run_deep_repro(test_name: &str, exercise: impl FnOnce()) {
    if std::env::var_os(DEEP_LIVENESS_CHILD).is_some() {
        exercise();
        return;
    }
    let output = Command::new(std::env::current_exe().expect("current test executable"))
        .args([test_name, "--exact", "--nocapture"])
        .env(DEEP_LIVENESS_CHILD, "1")
        .output()
        .expect("spawn deep liveness child");
    assert!(
        output.status.success(),
        "deep liveness child failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn nested_match_liveness_uses_bounded_stack() {
    run_deep_repro(
        "ownership::tests::liveness::nested_match_liveness_uses_bounded_stack",
        || nested_match_liveness(2_000),
    );
}

#[test]
fn alternating_control_liveness_uses_bounded_stack() {
    run_deep_repro(
        "ownership::tests::liveness::alternating_control_liveness_uses_bounded_stack",
        || alternating_control_liveness(2_000),
    );
}

#[test]
fn read_after_nested_if_stays_in_outer_arm_liveness() {
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let view_ty = pool.str_view();
    let int_ty = pool.int();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let source = pool.intern_str("s");
    let view = pool.intern_str("v");
    let print = pool.intern_str("print");
    let span = Span::EMPTY;
    let mut builder = TirBuilder::new(main, vec![], void, span);
    let text = builder.str_const(pool.intern_str("source"), str_ty, span);
    let source_decl = builder.var_decl(source, false, str_ty, text, span);
    let source_read = builder.var(source, str_ty, span);
    let zero = builder.int_const(0, int_ty, span);
    let one = builder.int_const(1, int_ty, span);
    let slice = builder.slice(source_read, Some(zero), Some(one), view_ty, span);
    let view_decl = builder.var_decl(view, false, view_ty, slice, span);
    let if_cond = builder.bool_const(true, bool_ty, span);
    let nested_if = builder.if_stmt(if_cond, &[], &[], None, void, span);
    let final_read = builder.var(view, view_ty, span);
    let final_call = builder.call(print, &[final_read], &all_borrow(&[final_read]), void, span);
    let scrutinee = builder.int_const(1, int_ty, span);
    let match_inst = builder.match_expr(
        scrutinee,
        void,
        &[TypedMatchArm {
            span,
            discriminant: Discriminant::Wildcard,
            bindings: &[],
            guards: &[],
            body: TypedMatchBody::Stmts(&[view_decl, nested_if, final_call]),
        }],
        span,
    );
    let tir = builder.finish(&[source_decl, match_inst]);
    let flow = Flow::build(&tir, &pool);
    let switch = flow.term_block_of(match_inst).expect("match block");
    let Terminator::Switch { arms, .. } = *flow.term(switch) else {
        unreachable!("match is a switch")
    };
    let arm = BlockId(flow.extra[arms.as_range().start]);

    let liveness = collect_view_liveness(&tir, &flow, &pool, &LoopNesting::default());

    let Some(Analysis::Present(reads)) = liveness.arm_last_reads.get(&arm) else {
        panic!("outer match arm must retain its read analysis")
    };
    assert_eq!(reads.get(&slice), Some(&final_read));
}
