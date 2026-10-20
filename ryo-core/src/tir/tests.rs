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
fn borrowed_struct_lit_view_iterates_canonical_fields() {
    let mut pool = InternPool::new();
    let int_ty = pool.int();
    let main = pool.intern_str("main");
    let mut b = TirBuilder::new(main, vec![], int_ty, sp());
    let first = b.int_const(1, int_ty, sp());
    let second = b.int_const(2, int_ty, sp());
    let lit = b.struct_lit(int_ty, &[(0, first), (1, second)], sp());
    let tir = b.finish(&[lit]);

    let view = tir.struct_lit_view(lit);
    assert_eq!(view.ty, int_ty);
    assert_eq!(
        view.fields().collect::<Vec<_>>(),
        vec![(0, first), (1, second)]
    );
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

    assert_eq!(tir.body_stmts().into_iter().collect::<Vec<_>>(), vec![ret]);
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
    assert_eq!(view.args.iter().collect::<Vec<_>>(), vec![a, bb]);
    assert_eq!(
        view.modes.iter().collect::<Vec<_>>(),
        vec![ParamMode::Borrow, ParamMode::Move]
    );
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

    let _ = tir.call_view(call).modes.iter().next();
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
fn var_decl_wire_fixture() {
    let mut pool = InternPool::new();
    let int_ty = pool.int();
    let main = pool.intern_str("main");
    let mut b = TirBuilder::new(main, vec![], int_ty, sp());
    b.extra.extend([0xAAAA_AAAA; 5]);
    let name = StringId::from_raw(7);
    let init = TirRef::from_raw(13);

    let decl = b.var_decl(name, true, int_ty, init, sp());
    let TirData::Extra(range) = b.instructions[decl.index()].data else {
        panic!("VarDecl must carry TirData::Extra");
    };
    assert_eq!(range, ExtraRange { offset: 5, len: 3 });
    assert_eq!(&b.extra[5..8], &[7, 1, 13]);
    assert_eq!(b.extra.len(), 8);

    let decl2 = b.var_decl(name, false, int_ty, init, sp());
    let TirData::Extra(range2) = b.instructions[decl2.index()].data else {
        panic!("VarDecl must carry TirData::Extra");
    };
    assert_eq!(range2, ExtraRange { offset: 8, len: 3 });
    assert_eq!(&b.extra[8..11], &[7, 0, 13]);
}

#[test]
fn var_decl_decode_fixture() {
    // Literal words, never encoder output: the payload words are
    // replaced with independent literals before the view reads them.
    let mut pool = InternPool::new();
    let int_ty = pool.int();
    let main = pool.intern_str("main");
    let mut b = TirBuilder::new(main, vec![], int_ty, sp());
    let dummy = b.int_const(0, int_ty, sp());
    let x = pool.intern_str("x");
    let decl = b.var_decl(x, false, int_ty, dummy, sp());
    let mut tir = b.finish(&[decl]);

    let TirData::Extra(range) = tir.inst(decl).data else {
        panic!("VarDecl must carry TirData::Extra");
    };
    let start = range.as_range().start;
    tir.extra[start] = 7; // name
    tir.extra[start + 1] = 1; // mutable
    tir.extra[start + 2] = 13; // initializer

    let v = tir.var_decl_view(decl);
    assert_eq!(v.name, StringId::from_raw(7));
    assert!(v.mutable);
    assert_eq!(v.initializer, TirRef::from_raw(13));
}

fn shell_with(extra_words: &[u32], tag: TirTag, len: u32) -> (Tir, TirRef) {
    // Build a minimal single-instruction `Tir` whose payload is the
    // caller-supplied literal words — never encoder output.
    let mut pool = InternPool::new();
    let int_ty = pool.int();
    let main = pool.intern_str("main");
    let mut b = TirBuilder::new(main, vec![], int_ty, sp());
    let zero = b.int_const(0, int_ty, sp());
    let mut tir = b.finish(&[zero]);
    let start = tir.extra.len() as u32;
    tir.extra.extend_from_slice(extra_words);
    tir.instructions.push(TypedInst {
        tag,
        ty: int_ty,
        data: TirData::Extra(ExtraRange { offset: start, len }),
    });
    tir.spans.push(sp());
    let r = TirRef::from_raw((tir.instructions.len() - 1) as u32);
    (tir, r)
}

#[test]
fn assign_wire_fixture() {
    let mut pool = InternPool::new();
    let int_ty = pool.int();
    let main = pool.intern_str("main");
    let mut b = TirBuilder::new(main, vec![], int_ty, sp());
    b.extra.extend([0xAAAA_AAAA; 5]);
    let name = StringId::from_raw(7);
    let value = TirRef::from_raw(13);

    let r = b.assign(name, int_ty, value, sp());
    let TirData::Extra(range) = b.instructions[r.index()].data else {
        panic!("Assign must carry TirData::Extra");
    };
    assert_eq!(range, ExtraRange { offset: 5, len: 2 });
    assert_eq!(&b.extra[5..7], &[7, 13]);
    assert_eq!(b.instructions[r.index()].ty, int_ty);

    let (tir, r) = shell_with(&[7, 13], TirTag::Assign, 2);
    let v = tir.assign_view(r);
    assert_eq!(v.name, name);
    assert_eq!(v.value, value);
}

#[test]
fn compound_assign_wire_fixture() {
    let mut pool = InternPool::new();
    let int_ty = pool.int();
    let main = pool.intern_str("main");
    let mut b = TirBuilder::new(main, vec![], int_ty, sp());
    b.extra.extend([0xAAAA_AAAA; 5]);
    let name = StringId::from_raw(7);
    let value = TirRef::from_raw(13);

    let r = b.compound_assign(name, CompoundOp::Sub, int_ty, value, sp());
    let TirData::Extra(range) = b.instructions[r.index()].data else {
        panic!("CompoundAssign must carry TirData::Extra");
    };
    assert_eq!(range, ExtraRange { offset: 5, len: 3 });
    assert_eq!(&b.extra[5..8], &[7, 1, 13]);

    let (tir, r) = shell_with(&[7, 1, 13], TirTag::CompoundAssign, 3);
    let v = tir.compound_assign_view(r);
    assert_eq!(v.name, name);
    assert!(matches!(v.op, CompoundOp::Sub));
    assert_eq!(v.value, value);
}

#[test]
fn field_assign_wire_fixture() {
    let mut pool = InternPool::new();
    let int_ty = pool.int();
    let main = pool.intern_str("main");
    let mut b = TirBuilder::new(main, vec![], int_ty, sp());
    b.extra.extend([0xAAAA_AAAA; 5]);
    let target = TirRef::from_raw(21);
    let value = TirRef::from_raw(22);

    let r = b.field_assign(target, value, int_ty, sp());
    let TirData::Extra(range) = b.instructions[r.index()].data else {
        panic!("FieldAssign must carry TirData::Extra");
    };
    assert_eq!(range, ExtraRange { offset: 5, len: 2 });
    assert_eq!(&b.extra[5..7], &[21, 22]);

    let (tir, r) = shell_with(&[21, 22], TirTag::FieldAssign, 2);
    let v = tir.field_assign_view(r);
    assert_eq!(v.target, target);
    assert_eq!(v.value, value);
}

#[test]
fn compound_field_assign_wire_fixture() {
    let mut pool = InternPool::new();
    let int_ty = pool.int();
    let main = pool.intern_str("main");
    let mut b = TirBuilder::new(main, vec![], int_ty, sp());
    b.extra.extend([0xAAAA_AAAA; 5]);
    let target = TirRef::from_raw(21);
    let value = TirRef::from_raw(22);

    let r = b.compound_field_assign(target, CompoundOp::Sub, int_ty, value, sp());
    let TirData::Extra(range) = b.instructions[r.index()].data else {
        panic!("CompoundFieldAssign must carry TirData::Extra");
    };
    assert_eq!(range, ExtraRange { offset: 5, len: 3 });
    assert_eq!(&b.extra[5..8], &[21, 1, 22]);

    let (tir, r) = shell_with(&[21, 1, 22], TirTag::CompoundFieldAssign, 3);
    let v = tir.compound_field_assign_view(r);
    assert_eq!(v.target, target);
    assert!(matches!(v.op, CompoundOp::Sub));
    assert_eq!(v.value, value);
}

#[test]
fn call_wire_fixture() {
    let mut pool = InternPool::new();
    let int_ty = pool.int();
    let main = pool.intern_str("main");
    let mut b = TirBuilder::new(main, vec![], int_ty, sp());
    b.extra.extend([0xAAAA_AAAA; 5]);
    let name = StringId::from_raw(7);
    let arg1 = TirRef::from_raw(31);
    let arg2 = TirRef::from_raw(32);

    let empty = b.call(name, &[], &[], int_ty, sp());
    let TirData::Extra(range) = b.instructions[empty.index()].data else {
        panic!("Call must carry TirData::Extra");
    };
    assert_eq!(range, ExtraRange { offset: 5, len: 2 });
    assert_eq!(&b.extra[5..7], &[7, 0]);

    let pop = b.call(
        name,
        &[arg1, arg2],
        &[ParamMode::Move, ParamMode::Inout],
        int_ty,
        sp(),
    );
    let TirData::Extra(range2) = b.instructions[pop.index()].data else {
        panic!("Call must carry TirData::Extra");
    };
    assert_eq!(range2, ExtraRange { offset: 7, len: 6 });
    assert_eq!(&b.extra[7..13], &[7, 2, 31, 32, 1, 2]);

    let borrow = b.call(name, &[arg1], &[ParamMode::Borrow], int_ty, sp());
    let TirData::Extra(range3) = b.instructions[borrow.index()].data else {
        panic!("Call must carry TirData::Extra");
    };
    assert_eq!(range3, ExtraRange { offset: 13, len: 4 });
    assert_eq!(&b.extra[13..17], &[7, 1, 31, 0]);

    let (tir, r) = shell_with(&[7, 2, 31, 32, 1, 2], TirTag::Call, 6);
    let v = tir.call_view(r);
    assert_eq!(v.name, name);
    assert_eq!(v.args.len(), 2);
    assert_eq!(v.args.at(1), arg2);
    assert_eq!(v.modes.len(), 2);
    assert_eq!(v.modes.at(0), ParamMode::Move);
    assert_eq!(v.modes.at(1), ParamMode::Inout);

    let (tir, r) = shell_with(&[7, 0], TirTag::Call, 2);
    let v = tir.call_view(r);
    assert_eq!(v.args.len(), 0);
    assert_eq!(v.modes.len(), 0);
}

#[test]
fn while_loop_wire_fixture() {
    let mut pool = InternPool::new();
    let int_ty = pool.int();
    let main = pool.intern_str("main");
    let mut b = TirBuilder::new(main, vec![], int_ty, sp());
    b.extra.extend([0xAAAA_AAAA; 5]);
    let cond = TirRef::from_raw(11);
    let s1 = TirRef::from_raw(21);
    let s2 = TirRef::from_raw(22);

    let empty = b.while_loop(cond, &[], int_ty, sp());
    let TirData::Extra(range) = b.instructions[empty.index()].data else {
        panic!("WhileLoop must carry TirData::Extra");
    };
    assert_eq!(range, ExtraRange { offset: 5, len: 2 });
    assert_eq!(&b.extra[5..7], &[11, 0]);

    let pop = b.while_loop(cond, &[s1, s2], int_ty, sp());
    let TirData::Extra(range2) = b.instructions[pop.index()].data else {
        panic!("WhileLoop must carry TirData::Extra");
    };
    assert_eq!(range2, ExtraRange { offset: 7, len: 4 });
    assert_eq!(&b.extra[7..11], &[11, 2, 21, 22]);

    let (tir, r) = shell_with(&[11, 2, 21, 22], TirTag::WhileLoop, 4);
    let v = tir.while_loop_view(r);
    assert_eq!(v.cond, cond);
    assert_eq!(v.body.len(), 2);
    assert_eq!(v.body.at(1), s2);
    assert_eq!(tir.while_loop_body_view(r).len(), 2);

    let (tir, r) = shell_with(&[11, 0], TirTag::WhileLoop, 2);
    let v = tir.while_loop_view(r);
    assert_eq!(v.body.len(), 0);
    assert_eq!(tir.while_loop_body_view(r).len(), 0);
}

#[test]
fn for_range_wire_fixture() {
    let mut pool = InternPool::new();
    let int_ty = pool.int();
    let main = pool.intern_str("main");
    let mut b = TirBuilder::new(main, vec![], int_ty, sp());
    b.extra.extend([0xAAAA_AAAA; 5]);
    let var_name = StringId::from_raw(7);
    let start = TirRef::from_raw(5);
    let end = TirRef::from_raw(9);
    let s1 = TirRef::from_raw(21);
    let s2 = TirRef::from_raw(22);

    let empty = b.for_range(var_name, start, end, &[], int_ty, sp());
    let TirData::Extra(range) = b.instructions[empty.index()].data else {
        panic!("ForRange must carry TirData::Extra");
    };
    assert_eq!(range, ExtraRange { offset: 5, len: 4 });
    assert_eq!(&b.extra[5..9], &[7, 5, 9, 0]);

    let pop = b.for_range(var_name, start, end, &[s1, s2], int_ty, sp());
    let TirData::Extra(range2) = b.instructions[pop.index()].data else {
        panic!("ForRange must carry TirData::Extra");
    };
    assert_eq!(range2, ExtraRange { offset: 9, len: 6 });
    assert_eq!(&b.extra[9..15], &[7, 5, 9, 2, 21, 22]);

    let (tir, r) = shell_with(&[7, 5, 9, 2, 21, 22], TirTag::ForRange, 6);
    let v = tir.for_range_view(r);
    assert_eq!(v.var_name, var_name);
    assert_eq!(v.start, start);
    assert_eq!(v.end, end);
    assert_eq!(v.body.len(), 2);
    assert_eq!(tir.for_range_body_view(r).at(0), s1);

    let (tir, r) = shell_with(&[7, 5, 9, 0], TirTag::ForRange, 4);
    let v = tir.for_range_view(r);
    assert_eq!(v.body.len(), 0);
    assert_eq!(tir.for_range_body_view(r).len(), 0);
}

#[test]
fn struct_lit_wire_fixture() {
    let mut pool = InternPool::new();
    let int_ty = pool.int();
    let main = pool.intern_str("main");
    let mut b = TirBuilder::new(main, vec![], int_ty, sp());
    b.extra.extend([0xAAAA_AAAA; 5]);
    let ty = TypeId::from_raw(3);
    let value = TirRef::from_raw(31);

    let one = b.struct_lit(ty, &[(0, value)], sp());
    let TirData::Extra(range) = b.instructions[one.index()].data else {
        panic!("StructLit must carry TirData::Extra");
    };
    assert_eq!(range, ExtraRange { offset: 5, len: 4 });
    assert_eq!(&b.extra[5..9], &[3, 1, 0, 31]);

    let empty = b.struct_lit(ty, &[], sp());
    let TirData::Extra(range2) = b.instructions[empty.index()].data else {
        panic!("StructLit must carry TirData::Extra");
    };
    assert_eq!(range2, ExtraRange { offset: 9, len: 2 });
    assert_eq!(&b.extra[9..11], &[3, 0]);

    let (tir, r) = shell_with(&[3, 1, 0, 31], TirTag::StructLit, 4);
    let v = tir.struct_lit_view(r);
    assert_eq!(v.ty, ty);
    assert_eq!(v.fields().collect::<Vec<_>>(), vec![(0, value)]);

    let (tir, r) = shell_with(&[3, 0], TirTag::StructLit, 2);
    let v = tir.struct_lit_view(r);
    assert_eq!(v.fields().count(), 0);
}

#[test]
fn enum_lit_wire_fixture() {
    let mut pool = InternPool::new();
    let int_ty = pool.int();
    let main = pool.intern_str("main");
    let mut b = TirBuilder::new(main, vec![], int_ty, sp());
    b.extra.extend([0xAAAA_AAAA; 5]);
    let ty = TypeId::from_raw(3);
    let w = TirRef::from_raw(21);
    let h = TirRef::from_raw(22);

    let two = b.enum_lit(ty, 1, false, &[(0, w), (1, h)], sp());
    let TirData::Extra(range) = b.instructions[two.index()].data else {
        panic!("EnumLit must carry TirData::Extra");
    };
    assert_eq!(range, ExtraRange { offset: 5, len: 8 });
    assert_eq!(&b.extra[5..13], &[3, 1, 2, 0, 0, 21, 1, 22]);

    let unit = b.enum_lit(ty, 1, false, &[], sp());
    let TirData::Extra(range2) = b.instructions[unit.index()].data else {
        panic!("EnumLit must carry TirData::Extra");
    };
    assert_eq!(range2, ExtraRange { offset: 13, len: 4 });
    assert_eq!(&b.extra[13..17], &[3, 1, 0, 0]);

    let pos = b.enum_lit(ty, 1, true, &[(0, w)], sp());
    let TirData::Extra(range3) = b.instructions[pos.index()].data else {
        panic!("EnumLit must carry TirData::Extra");
    };
    assert_eq!(range3, ExtraRange { offset: 17, len: 6 });
    assert_eq!(&b.extra[17..23], &[3, 1, 1, 1, 0, 21]);

    let (tir, r) = shell_with(&[3, 1, 2, 0, 0, 21, 1, 22], TirTag::EnumLit, 8);
    let v = tir.enum_lit_view(r);
    assert_eq!(v.ty, ty);
    assert_eq!(v.variant_index, 1);
    assert!(!v.positional);
    assert_eq!(v.fields().collect::<Vec<_>>(), vec![(0, w), (1, h)]);

    let (tir, r) = shell_with(&[3, 1, 0, 0], TirTag::EnumLit, 4);
    let v = tir.enum_lit_view(r);
    assert_eq!(v.fields().count(), 0);

    let (tir, r) = shell_with(&[3, 1, 1, 1, 0, 21], TirTag::EnumLit, 6);
    let v = tir.enum_lit_view(r);
    assert!(v.positional);
    assert_eq!(v.fields().collect::<Vec<_>>(), vec![(0, w)]);
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
    assert_eq!(
        view.then_stmts.into_iter().collect::<Vec<_>>(),
        vec![then_ret]
    );
    assert!(view.elif_branches.is_empty());
    assert_eq!(
        view.else_stmts
            .map(|stmts| stmts.into_iter().collect::<Vec<_>>()),
        Some(vec![else_ret])
    );
}

#[test]
fn borrowed_if_view_iterates_all_branches_without_losing_empty_else() {
    let mut pool = InternPool::new();
    let bool_ty = pool.bool_();
    let int_ty = pool.int();
    let void_ty = pool.void();
    let main = pool.intern_str("main");
    let mut b = TirBuilder::new(main, vec![], void_ty, sp());
    let cond = b.bool_const(true, bool_ty, sp());
    let then_stmt = b.int_const(1, int_ty, sp());
    let elif_cond_a = b.bool_const(false, bool_ty, sp());
    let elif_stmt = b.int_const(2, int_ty, sp());
    let elif_cond_b = b.bool_const(true, bool_ty, sp());
    let empty_else_cond = b.bool_const(false, bool_ty, sp());
    let with_empty_else = b.if_stmt(
        cond,
        &[then_stmt],
        &[(elif_cond_a, vec![elif_stmt]), (elif_cond_b, vec![])],
        Some(&[]),
        void_ty,
        sp(),
    );
    let without_else = b.if_stmt(empty_else_cond, &[], &[], None, void_ty, sp());
    let tir = b.finish(&[with_empty_else, without_else]);

    let view = tir.if_stmt_view(with_empty_else);
    assert_eq!(view.cond, cond);
    assert_eq!(
        view.then_stmts.into_iter().collect::<Vec<_>>(),
        vec![then_stmt]
    );
    let elifs: Vec<_> = view
        .elif_branches
        .iter()
        .map(|arm| (arm.cond, arm.body.into_iter().collect::<Vec<_>>()))
        .collect();
    assert_eq!(
        elifs,
        vec![(elif_cond_a, vec![elif_stmt]), (elif_cond_b, vec![])]
    );
    assert_eq!(view.else_stmts.unwrap().len(), 0);

    let view = tir.if_stmt_view(without_else);
    assert!(view.elif_branches.is_empty());
    assert!(view.else_stmts.is_none());
}

#[test]
fn borrowed_loop_views_preserve_headers_and_bodies() {
    let mut pool = InternPool::new();
    let bool_ty = pool.bool_();
    let int_ty = pool.int();
    let void_ty = pool.void();
    let main = pool.intern_str("main");
    let name = pool.intern_str("i");
    let mut b = TirBuilder::new(main, vec![], void_ty, sp());
    let cond = b.bool_const(true, bool_ty, sp());
    let while_stmt = b.int_const(1, int_ty, sp());
    let start = b.int_const(0, int_ty, sp());
    let end = b.int_const(4, int_ty, sp());
    let for_stmt = b.int_const(2, int_ty, sp());
    let while_loop = b.while_loop(cond, &[while_stmt], void_ty, sp());
    let for_loop = b.for_range(name, start, end, &[for_stmt], void_ty, sp());
    let tir = b.finish(&[while_loop, for_loop]);

    let view = tir.while_loop_view(while_loop);
    assert_eq!(view.cond, cond);
    assert_eq!(view.body.into_iter().collect::<Vec<_>>(), vec![while_stmt]);
    let view = tir.for_range_view(for_loop);
    assert_eq!(view.var_name, name);
    assert_eq!(view.start, start);
    assert_eq!(view.end, end);
    assert_eq!(view.body.into_iter().collect::<Vec<_>>(), vec![for_stmt]);
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
fn slice_and_to_view_round_trip_and_dump() {
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
    let view = b.to_view(base2, view_ty, sp());
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
    assert!(matches!(tir.inst(view).tag, TirTag::ToView));
    assert_eq!(tir.inst(view).ty, view_ty);

    let out = format!("{}", dump(std::slice::from_ref(&tir), &pool));
    assert!(out.contains("= slice %1, %2.._"), "got:\n{}", out);
    assert!(out.contains("= to_view %4"), "got:\n{}", out);
}

#[test]
fn destructure_round_trips_and_owner_tokens() {
    let mut pool = InternPool::new();
    let int_ty = pool.int();
    let str_ty = pool.str_();
    let main = pool.intern_str("main");
    let q = pool.intern_str("q");
    let s = pool.intern_str("s");
    let pair_ty = pool.anon_struct(&[(q, int_ty), (s, str_ty)]);

    let mut b = TirBuilder::new(main, vec![], pool.void(), sp());
    // A struct-typed value: simulate the call result with a Var read
    // (the builder does not enforce producer shapes).
    let rhs = b.var(main, pair_ty, sp());
    // Owner tokens: one FieldAccess per BOUND field, immediately before
    // the Destructure, plan order (field 1 is a wildcard here).
    let owner0 = b.field_access(rhs, 0, int_ty, sp());
    let destr = b.destructure(rhs, &[(0, Some(q), int_ty), (1, None, str_ty)], sp());
    assert_eq!(owner0.index(), destr.index() - 1);
    let tir = b.finish(&[destr]);

    assert_eq!(tir.inst(destr).tag, TirTag::Destructure);
    assert_eq!(tir.inst(destr).ty, pair_ty);
    let view = tir.destructure_view(destr);
    assert_eq!(view.rhs, rhs);
    assert_eq!(view.fields.len(), 2);
    assert_eq!(view.fields[0].field_index, 0);
    assert_eq!(view.fields[0].bind, Some(q));
    assert_eq!(view.fields[0].ty, int_ty);
    assert_eq!(view.fields[1].field_index, 1);
    assert_eq!(view.fields[1].bind, None);
    assert_eq!(view.fields[1].ty, str_ty);

    // The +1 bind encoding must survive a raw-0 string id: "derive" is
    // pre-interned at raw 0 and is a legal (if unconventional) binding.
    let derive = pool.intern_str("derive");
    assert_eq!(derive.raw(), 0);
    let mut b = TirBuilder::new(main, vec![], pool.void(), sp());
    let rhs = b.var(main, pair_ty, sp());
    let _owner0 = b.field_access(rhs, 0, int_ty, sp());
    let _owner1 = b.field_access(rhs, 1, str_ty, sp());
    let destr = b.destructure(
        rhs,
        &[(0, Some(derive), int_ty), (1, Some(q), str_ty)],
        sp(),
    );
    let tir = b.finish(&[destr]);
    let view = tir.destructure_view(destr);
    assert_eq!(view.fields[0].bind, Some(derive));
    assert_eq!(view.fields[1].bind, Some(q));

    let owners = tir.destructure_bound_owner_refs(destr);
    assert_eq!(owners.len(), 2);
}

#[test]
fn enum_lit_round_trips_through_extra() {
    let mut pool = InternPool::new();
    let shape = pool.intern_str("Shape");
    let enum_ty = pool.declare_enum(shape);
    let float_ty = pool.float();

    let mut b = TirBuilder::new(shape, vec![], pool.void(), sp());
    let w = b.float_const(3.0, float_ty, sp());
    let h = b.float_const(4.0, float_ty, sp());
    let lit = b.enum_lit(enum_ty, 1, false, &[(0, w), (1, h)], sp());
    let tir = b.finish(&[lit]);

    let view = tir.enum_lit_view(lit);
    assert_eq!(view.ty, enum_ty);
    assert_eq!(view.variant_index, 1);
    assert!(!view.positional);
    assert_eq!(view.fields().collect::<Vec<_>>(), vec![(0, w), (1, h)]);
    assert_eq!(tir.inst(lit).ty, enum_ty);

    // Wire layout is pinned: [TY, VARIANT, FLAGS, ARGC, (FIELD_IDX, REF) x ARGC].
    let inst = tir.inst(lit);
    let TirData::Extra(rng) = inst.data else {
        panic!("expected TirData::Extra, got {:?}", inst.data);
    };
    let slice = &tir.extra[rng.as_range()];
    assert_eq!(slice[enum_lit_extra::TY], enum_ty.raw());
    assert_eq!(slice[enum_lit_extra::VARIANT], 1);
    assert_eq!(slice[enum_lit_extra::FLAGS], 0);
    assert_eq!(slice[enum_lit_extra::ARGC], 2);
    assert_eq!(slice[enum_lit_extra::ARGS], 0);
    assert_eq!(slice[enum_lit_extra::ARGS + 1], w.raw());
    assert_eq!(slice[enum_lit_extra::ARGS + 2], 1);
    assert_eq!(slice[enum_lit_extra::ARGS + 3], h.raw());
}

#[test]
fn enum_lit_positional_flag_round_trips() {
    let mut pool = InternPool::new();
    let shape = pool.intern_str("Shape");
    let enum_ty = pool.declare_enum(shape);
    let float_ty = pool.float();

    let mut b = TirBuilder::new(shape, vec![], pool.void(), sp());
    let v = b.float_const(5.0, float_ty, sp());
    let lit = b.enum_lit(enum_ty, 0, true, &[(0, v)], sp());
    let tir = b.finish(&[lit]);

    let view = tir.enum_lit_view(lit);
    assert!(view.positional, "parenthesized form sets the flag");
    assert_eq!(view.fields().collect::<Vec<_>>(), vec![(0, v)]);
}

#[test]
fn enum_lit_unit_variant_round_trips_empty_args() {
    let mut pool = InternPool::new();
    let shape = pool.intern_str("Shape");
    let enum_ty = pool.declare_enum(shape);

    let mut b = TirBuilder::new(shape, vec![], pool.void(), sp());
    let lit = b.enum_lit(enum_ty, 0, false, &[], sp());
    let tir = b.finish(&[lit]);

    let view = tir.enum_lit_view(lit);
    assert_eq!(view.ty, enum_ty);
    assert_eq!(view.variant_index, 0);
    assert!(!view.positional);
    assert_eq!(view.fields().count(), 0);
}

#[test]
fn walk_operands_visits_enum_lit_fields_and_enum_eq_ne_operands() {
    let mut pool = InternPool::new();
    let shape = pool.intern_str("Shape");
    let enum_ty = pool.declare_enum(shape);
    let float_ty = pool.float();

    let mut b = TirBuilder::new(shape, vec![], pool.void(), sp());
    let w = b.float_const(3.0, float_ty, sp());
    let h = b.float_const(4.0, float_ty, sp());
    let lit = b.enum_lit(enum_ty, 1, false, &[(0, w), (1, h)], sp());
    // Fresh refs per comparison: the body is tree-shaped, so no
    // instruction may feed two parents.
    let a = b.enum_lit(enum_ty, 0, false, &[], sp());
    let c = b.enum_lit(enum_ty, 0, false, &[], sp());
    let eq = b.binary(TirTag::EnumEq, pool.bool_(), a, c, sp());
    let d = b.enum_lit(enum_ty, 1, false, &[], sp());
    let e = b.enum_lit(enum_ty, 1, false, &[], sp());
    let ne = b.binary(TirTag::EnumNe, pool.bool_(), d, e, sp());
    let tir = b.finish(&[lit, eq, ne]);

    let mut operands = Vec::new();
    tir.walk_operands(lit, &mut |_parent, child, kind| {
        assert!(matches!(kind, ChildKind::Operand));
        operands.push(child);
    });
    assert_eq!(operands, vec![w, h]);

    let mut operands = Vec::new();
    tir.walk_operands(eq, &mut |_parent, child, kind| {
        assert!(matches!(kind, ChildKind::Operand));
        operands.push(child);
    });
    assert_eq!(operands, vec![a, c]);

    let mut operands = Vec::new();
    tir.walk_operands(ne, &mut |_parent, child, kind| {
        assert!(matches!(kind, ChildKind::Operand));
        operands.push(child);
    });
    assert_eq!(operands, vec![d, e]);
}

#[test]
fn walk_operands_visits_if_and_loop_payloads_in_order() {
    let mut pool = InternPool::new();
    let name = pool.intern_str("main");
    let int_ty = pool.int();
    let void_ty = pool.void();
    let mut b = TirBuilder::new(name, vec![], void_ty, sp());
    let refs: Vec<_> = (0..12)
        .map(|value| b.int_const(value, int_ty, sp()))
        .collect();
    let [
        cond,
        then_a,
        then_b,
        elif_cond,
        elif_stmt,
        else_stmt,
        while_cond,
        while_a,
        while_b,
        start,
        end,
        for_stmt,
    ] = refs[..]
    else {
        unreachable!()
    };

    let if_stmt = b.if_stmt(
        cond,
        &[then_a, then_b],
        &[(elif_cond, vec![elif_stmt])],
        Some(&[else_stmt]),
        void_ty,
        sp(),
    );
    let while_loop = b.while_loop(while_cond, &[while_a, while_b], void_ty, sp());
    let for_loop = b.for_range(name, start, end, &[for_stmt], void_ty, sp());
    let tir = b.finish(&[if_stmt, while_loop, for_loop]);

    assert_eq!(
        tir.loop_body_view(while_loop)
            .unwrap()
            .into_iter()
            .collect::<Vec<_>>(),
        vec![while_a, while_b]
    );
    assert_eq!(
        tir.loop_body_view(for_loop)
            .unwrap()
            .into_iter()
            .collect::<Vec<_>>(),
        vec![for_stmt]
    );

    let mut edges = Vec::new();
    tir.walk_operands(if_stmt, &mut |_parent, child, kind| {
        edges.push((child, kind))
    });
    assert_eq!(
        edges,
        vec![
            (cond, ChildKind::Operand),
            (then_a, ChildKind::BodyStmt),
            (then_b, ChildKind::BodyStmt),
            (elif_cond, ChildKind::Operand),
            (elif_stmt, ChildKind::BodyStmt),
            (else_stmt, ChildKind::BodyStmt),
        ]
    );

    edges.clear();
    tir.walk_operands(while_loop, &mut |_parent, child, kind| {
        edges.push((child, kind))
    });
    assert_eq!(
        edges,
        vec![
            (while_cond, ChildKind::Operand),
            (while_a, ChildKind::BodyStmt),
            (while_b, ChildKind::BodyStmt),
        ]
    );

    edges.clear();
    tir.walk_operands(for_loop, &mut |_parent, child, kind| {
        edges.push((child, kind))
    });
    assert_eq!(
        edges,
        vec![
            (start, ChildKind::Operand),
            (end, ChildKind::Operand),
            (for_stmt, ChildKind::BodyStmt),
        ]
    );
}

#[test]
fn collect_jump_path_iter_stops_after_the_target() {
    let mut pool = InternPool::new();
    let name = pool.intern_str("main");
    let int_ty = pool.int();
    let void_ty = pool.void();
    let mut b = TirBuilder::new(name, vec![], void_ty, sp());
    let before = b.int_const(1, int_ty, sp());
    let target = b.int_const(2, int_ty, sp());
    let after = b.int_const(3, int_ty, sp());
    let tir = b.finish(&[before, target, after]);

    let mut path = std::collections::HashSet::new();
    assert!(tir.collect_jump_path_iter([before, target, after], target, &mut path));
    assert!(path.contains(&before));
    assert!(path.contains(&target));
    assert!(!path.contains(&after));
}

#[test]
fn collect_jump_path_iter_borrows_nested_loop_bodies() {
    let mut pool = InternPool::new();
    let name = pool.intern_str("main");
    let loop_var = pool.intern_str("i");
    let int_ty = pool.int();
    let bool_ty = pool.bool_();
    let void_ty = pool.void();
    let mut b = TirBuilder::new(name, vec![], void_ty, sp());

    let cond = b.bool_const(true, bool_ty, sp());
    let while_before = b.int_const(1, int_ty, sp());
    let while_target = b.int_const(2, int_ty, sp());
    let while_after = b.int_const(3, int_ty, sp());
    let while_loop = b.while_loop(
        cond,
        &[while_before, while_target, while_after],
        void_ty,
        sp(),
    );

    let start = b.int_const(0, int_ty, sp());
    let end = b.int_const(4, int_ty, sp());
    let for_before = b.int_const(5, int_ty, sp());
    let for_target = b.int_const(6, int_ty, sp());
    let for_after = b.int_const(7, int_ty, sp());
    let for_loop = b.for_range(
        loop_var,
        start,
        end,
        &[for_before, for_target, for_after],
        void_ty,
        sp(),
    );
    let tir = b.finish(&[while_loop, for_loop]);

    let mut path = std::collections::HashSet::new();
    assert!(tir.collect_jump_path_iter([while_loop], while_target, &mut path));
    for reachable in [while_loop, cond, while_before, while_target] {
        assert!(path.contains(&reachable));
    }
    assert!(!path.contains(&while_after));

    path.clear();
    assert!(tir.collect_jump_path_iter([for_loop], for_target, &mut path));
    for reachable in [for_loop, start, end, for_before, for_target] {
        assert!(path.contains(&reachable));
    }
    assert!(!path.contains(&for_after));
}

#[test]
fn destructure_dump_lists_fields() {
    let mut pool = InternPool::new();
    let int_ty = pool.int();
    let main = pool.intern_str("main");
    let q = pool.intern_str("q");
    let pair_ty = pool.anon_struct(&[(q, int_ty)]);

    let mut b = TirBuilder::new(main, vec![], pool.void(), sp());
    let rhs = b.var(main, pair_ty, sp());
    let owner = b.field_access(rhs, 0, int_ty, sp());
    let _ = owner;
    let destr = b.destructure(rhs, &[(0, Some(q), int_ty)], sp());
    let tir = b.finish(&[destr]);

    let out = format!("{}", dump(std::slice::from_ref(&tir), &pool));
    assert!(
        out.contains("= destructure %1 {0: q: int}"),
        "got:\n{}",
        out
    );
}
