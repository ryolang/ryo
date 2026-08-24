use super::*;

fn sp() -> Span {
    SimpleSpan::new((), 0..0)
}

#[test]
fn tirref_option_is_one_word() {
    assert_eq!(
        std::mem::size_of::<Option<TirRef>>(),
        std::mem::size_of::<u32>()
    );
}

#[test]
fn param_ref_predicates() {
    let p = TirRef::param(3);
    assert!(p.is_param());
    assert_eq!(p.as_param_index(), Some(3));
    let real = TirRef::from_raw(7);
    assert!(!real.is_param());
    assert_eq!(real.as_param_index(), None);
}

#[test]
#[should_panic(expected = "Tir::inst called with a param sentinel ref")]
fn inst_rejects_param_sentinel_with_clear_message() {
    let mut pool = InternPool::new();
    let int_ty = pool.int();
    let main = pool.intern_str("main");
    let mut b = TirBuilder::new(main, vec![], int_ty, sp());
    let lit = b.int_const(1, int_ty, sp());
    let tir = b.finish(&[lit]);
    let _ = tir.inst(TirRef::param(0));
}

#[test]
#[should_panic(expected = "Tir::span called with a param sentinel ref")]
fn span_rejects_param_sentinel_with_clear_message() {
    let mut pool = InternPool::new();
    let int_ty = pool.int();
    let main = pool.intern_str("main");
    let mut b = TirBuilder::new(main, vec![], int_ty, sp());
    let lit = b.int_const(1, int_ty, sp());
    let tir = b.finish(&[lit]);
    let _ = tir.span(TirRef::param(0));
}

#[test]
#[should_panic(expected = "TirRef raw must be non-zero")]
fn from_raw_zero_rejected_at_construction() {
    // Slot 0 is the reserved arena sentinel; the NonZeroU32
    // newtype makes a raw-0 ref unconstructible, so `Tir::inst` /
    // `Tir::span` need no nonzero guard of their own.
    let _ = TirRef::from_raw(0);
}

#[test]
fn build_simple_function_and_dump() {
    let mut pool = InternPool::new();
    let int_ty = pool.int();
    let main = pool.intern_str("main");

    let mut b = TirBuilder::new(main, vec![], int_ty, sp());
    let lit1 = b.int_const(1, int_ty, sp());
    let lit2 = b.int_const(2, int_ty, sp());
    let add = b.binary(TirTag::IAdd, int_ty, lit1, lit2, sp());
    let ret = b.unary(TirTag::Return, pool.void(), add, sp());
    let tir = b.finish(&[ret]);

    assert_eq!(tir.body_stmts(), vec![ret]);
    let out = format!("{}", dump(std::slice::from_ref(&tir), &pool));
    assert!(out.contains("fn main() -> int"));
    assert!(out.contains("= iconst 1"));
    assert!(out.contains("= iadd %"));
    assert!(out.contains("= ret %"));
}

#[test]
fn call_payload_round_trips() {
    let mut pool = InternPool::new();
    let int_ty = pool.int();
    let foo = pool.intern_str("foo");
    let main = pool.intern_str("main");

    let mut b = TirBuilder::new(main, vec![], int_ty, sp());
    let a = b.int_const(1, int_ty, sp());
    let bb = b.int_const(2, int_ty, sp());
    // Mixed modes: first arg borrowed, second moved — exercises both
    // encodings through the extra arena and back through CallView.
    let modes = [ParamMode::Borrow, ParamMode::Move];
    let call = b.call(foo, &[a, bb], &modes, int_ty, sp());
    let ret = b.unary(TirTag::Return, pool.void(), call, sp());
    let tir = b.finish(&[ret]);

    let view = tir.call_view(call);
    assert_eq!(view.name, foo);
    assert_eq!(view.args, vec![a, bb]);
    assert_eq!(view.modes, vec![ParamMode::Borrow, ParamMode::Move]);
}

#[test]
fn param_mode_from_u32_is_strict() {
    for (word, mode) in [
        (0, ParamMode::Borrow),
        (1, ParamMode::Move),
        (2, ParamMode::Inout),
    ] {
        assert_eq!(ParamMode::from_u32(word), Some(mode));
        assert_eq!(mode.to_u32(), word);
    }
    assert_eq!(ParamMode::from_u32(3), None);
    assert_eq!(ParamMode::from_u32(u32::MAX), None);
}

#[test]
#[should_panic(expected = "call_extra mode word 99")]
fn call_view_panics_on_corrupt_mode_word() {
    let mut pool = InternPool::new();
    let int_ty = pool.int();
    let foo = pool.intern_str("foo");
    let main = pool.intern_str("main");

    let mut b = TirBuilder::new(main, vec![], int_ty, sp());
    let a = b.int_const(1, int_ty, sp());
    let call = b.call(foo, &[a], &[ParamMode::Borrow], int_ty, sp());
    let ret = b.unary(TirTag::Return, pool.void(), call, sp());
    let mut tir = b.finish(&[ret]);

    // Corrupt the single mode word in the extra arena.
    let inst = tir.inst(call);
    let TirData::Extra(rng) = inst.data else {
        panic!("Call must carry TirData::Extra")
    };
    let mode_idx = rng.as_range().start + call_extra::ARGS + 1;
    tir.extra[mode_idx] = 99;

    tir.call_view(call);
}

#[test]
fn var_decl_round_trips() {
    let mut pool = InternPool::new();
    let int_ty = pool.int();
    let x = pool.intern_str("x");
    let main = pool.intern_str("main");

    let mut b = TirBuilder::new(main, vec![], int_ty, sp());
    let init = b.int_const(42, int_ty, sp());
    let decl = b.var_decl(x, true, int_ty, init, sp());
    let zero = b.int_const(0, int_ty, sp());
    let ret = b.unary(TirTag::Return, pool.void(), zero, sp());
    let tir = b.finish(&[decl, ret]);

    let v = tir.var_decl_view(decl);
    assert_eq!(v.name, x);
    assert!(v.mutable);
    assert_eq!(v.initializer, init);
    assert_eq!(tir.inst(decl).ty, int_ty);
}

#[test]
fn float_const_and_fadd_round_trip() {
    let mut pool = InternPool::new();
    let main = pool.intern_str("main");
    let mut b = TirBuilder::new(main, vec![], pool.int(), sp());

    // Float operands feed the float ops; integer operands feed
    // the integer ops. The TIR builder doesn't enforce operand
    // shape (sema does), but mirroring real usage keeps the
    // test honest.
    let lit_f1 = b.float_const(1.5, pool.float(), sp());
    let lit_f2 = b.float_const(2.5, pool.float(), sp());
    let lit_i1 = b.int_const(1, pool.int(), sp());
    let lit_i2 = b.int_const(2, pool.int(), sp());

    let fadd = b.binary(TirTag::FAdd, pool.float(), lit_f1, lit_f2, sp());
    let icmp_lt = b.binary(TirTag::ICmpLt, pool.bool_(), lit_i1, lit_i2, sp());
    let imod = b.binary(TirTag::IMod, pool.int(), lit_i1, lit_i2, sp());

    // Result types match the operator category: float arithmetic
    // stays float, ordering produces bool, integer modulo stays int.
    assert_eq!(b.ty_of(fadd), pool.float());
    assert_eq!(b.ty_of(icmp_lt), pool.bool_());
    assert_eq!(b.ty_of(imod), pool.int());
}

#[test]
fn if_stmt_round_trips_through_extra() {
    let mut pool = InternPool::new();
    let bool_ty = pool.bool_();
    let int_ty = pool.int();
    let main = pool.intern_str("main");

    let mut b = TirBuilder::new(main, vec![], pool.void(), sp());
    let cond = b.bool_const(true, bool_ty, sp());
    let s1 = b.int_const(1, int_ty, sp());
    let then_ret = b.unary(TirTag::Return, pool.void(), s1, sp());
    let s2 = b.int_const(2, int_ty, sp());
    let else_ret = b.unary(TirTag::Return, pool.void(), s2, sp());

    let if_ref = b.if_stmt(cond, &[then_ret], &[], Some(&[else_ret]), pool.void(), sp());

    let tir = b.finish(&[if_ref]);
    let view = tir.if_stmt_view(if_ref);
    assert_eq!(view.cond, cond);
    assert_eq!(view.then_stmts, vec![then_ret]);
    assert!(view.elif_branches.is_empty());
    assert_eq!(view.else_stmts, Some(vec![else_ret]));
}

#[test]
fn assign_round_trips() {
    let mut pool = InternPool::new();
    let int_ty = pool.int();
    let x = pool.intern_str("x");
    let main = pool.intern_str("main");

    let mut b = TirBuilder::new(main, vec![], int_ty, sp());
    let init = b.int_const(42, int_ty, sp());
    let decl = b.var_decl(x, true, int_ty, init, sp());
    let new_val = b.int_const(99, int_ty, sp());
    let asgn = b.assign(x, int_ty, new_val, sp());
    let zero = b.int_const(0, int_ty, sp());
    let ret = b.unary(TirTag::Return, pool.void(), zero, sp());
    let tir = b.finish(&[decl, asgn, ret]);

    let v = tir.assign_view(asgn);
    assert_eq!(v.name, x);
    assert_eq!(v.value, new_val);
    assert_eq!(tir.inst(asgn).ty, int_ty);
}

#[test]
fn compound_assign_round_trips() {
    let mut pool = InternPool::new();
    let int_ty = pool.int();
    let x = pool.intern_str("x");
    let main = pool.intern_str("main");

    let mut b = TirBuilder::new(main, vec![], int_ty, sp());
    let init = b.int_const(10, int_ty, sp());
    let decl = b.var_decl(x, true, int_ty, init, sp());
    let delta = b.int_const(5, int_ty, sp());
    let ca = b.compound_assign(x, CompoundOp::Add, int_ty, delta, sp());
    let zero = b.int_const(0, int_ty, sp());
    let ret = b.unary(TirTag::Return, pool.void(), zero, sp());
    let tir = b.finish(&[decl, ca, ret]);

    let v = tir.compound_assign_view(ca);
    assert_eq!(v.name, x);
    assert_eq!(v.op, CompoundOp::Add);
    assert_eq!(v.value, delta);
    assert_eq!(tir.inst(ca).ty, int_ty);
}

#[test]
fn unreachable_inst_carries_error_type() {
    let mut pool = InternPool::new();
    let err_ty = pool.error_type();
    let main = pool.intern_str("main");

    let mut b = TirBuilder::new(main, vec![], pool.int(), sp());
    let u = b.unreachable(err_ty, sp());
    let tir = b.finish(&[u]);
    assert!(matches!(tir.inst(u).tag, TirTag::Unreachable));
    assert_eq!(tir.inst(u).ty, err_ty);
}

#[test]
fn slice_and_view_of_str_round_trip_and_dump() {
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let view_ty = pool.str_view();
    let main = pool.intern_str("main");
    let s = pool.intern_str("s");

    let mut b = TirBuilder::new(main, vec![], pool.void(), sp());
    let base = b.var(s, str_ty, sp());
    let lo = b.int_const(0, pool.int(), sp());
    // `s[0:]` — end omitted, exercising the `None` bound.
    let slice = b.slice(base, Some(lo), None, view_ty, sp());
    // `str → strview` conversion of the same variable — a fresh
    // read: each use of a binding emits its own `Var` instruction,
    // so the body stays tree-shaped.
    let base2 = b.var(s, str_ty, sp());
    let view = b.view_of_str(base2, view_ty, sp());
    let tir = b.finish(&[slice, view]);

    assert_eq!(tir.inst(slice).ty, view_ty);
    match tir.inst(slice).data {
        TirData::Slice {
            base: bb,
            start,
            end,
        } => {
            assert_eq!(bb, base);
            assert_eq!(start, Some(lo));
            assert_eq!(end, None);
        }
        other => panic!("expected TirData::Slice, got {:?}", other),
    }
    assert!(matches!(tir.inst(view).tag, TirTag::ViewOfStr));
    assert_eq!(tir.inst(view).ty, view_ty);

    let out = format!("{}", dump(std::slice::from_ref(&tir), &pool));
    assert!(out.contains("= slice %1, %2.._"), "got:\n{}", out);
    assert!(out.contains("= view_of_str %4"), "got:\n{}", out);
}
