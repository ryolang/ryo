//! Literal wire fixtures for the fused UIR payload layouts.
//!
//! Encode fixtures pin the exact words a builder appends (and the
//! `ExtraRange`) against a seeded arena; decode fixtures feed
//! independently supplied literal words to the views — never
//! encoder output.

use super::*;

fn sp() -> Span {
    SimpleSpan::new((), 0..0)
}

#[test]
fn var_decl_wire_fixture() {
    let mut b = UirBuilder::new();
    b.uir.extra.extend([0xAAAA_AAAA; 5]);
    let name = StringId::from_raw(7);
    let init = InstRef::from_raw(11);
    let ty = TypeId::from_raw(3);

    let annotated = b.var_decl(name, true, Some(ty), init, sp());
    let InstData::Extra(range) = b.uir.inst(annotated).data else {
        panic!("VarDecl must carry InstData::Extra");
    };
    assert_eq!(range, ExtraRange { offset: 5, len: 4 });
    assert_eq!(&b.uir.extra[5..9], &[7, 1, 3, 11]);
    assert_eq!(b.uir.extra.len(), 9);

    let inferred = b.var_decl(name, false, None, init, sp());
    let InstData::Extra(range2) = b.uir.inst(inferred).data else {
        panic!("VarDecl must carry InstData::Extra");
    };
    assert_eq!(range2, ExtraRange { offset: 9, len: 4 });
    assert_eq!(&b.uir.extra[9..13], &[7, 0, u32::MAX, 11]);
}

#[test]
fn var_decl_decode_fixture() {
    // Literal words, never encoder output: payloads and ranges are
    // supplied independently of any builder push sequence.
    let mut uir = Uir::new();
    uir.extra.extend([7, 1, 3, 11, 7, 0, u32::MAX, 11]);
    uir.instructions.push(Inst {
        tag: InstTag::VarDecl,
        data: InstData::Extra(ExtraRange { offset: 0, len: 4 }),
    });
    uir.spans.push(sp());
    uir.instructions.push(Inst {
        tag: InstTag::VarDecl,
        data: InstData::Extra(ExtraRange { offset: 4, len: 4 }),
    });
    uir.spans.push(sp());

    let name = StringId::from_raw(7);
    let init = InstRef::from_raw(11);
    let ty = TypeId::from_raw(3);

    let v1 = uir.var_decl_view(InstRef::from_index(1));
    assert_eq!(v1.name, name);
    assert!(v1.mutable);
    assert_eq!(v1.ty, Some(ty));
    assert_eq!(v1.initializer, init);

    let v2 = uir.var_decl_view(InstRef::from_index(2));
    assert_eq!(v2.name, name);
    assert!(!v2.mutable);
    assert_eq!(v2.ty, None);
    assert_eq!(v2.initializer, init);
}

#[test]
fn assign_or_decl_wire_fixture() {
    let mut b = UirBuilder::new();
    b.uir.extra.extend([0xAAAA_AAAA; 5]);
    let name = StringId::from_raw(7);
    let value = InstRef::from_raw(11);

    let r = b.assign_or_decl(name, value, sp());
    let InstData::Extra(range) = b.uir.inst(r).data else {
        panic!("AssignOrDecl must carry InstData::Extra");
    };
    assert_eq!(range, ExtraRange { offset: 5, len: 2 });
    assert_eq!(&b.uir.extra[5..7], &[7, 11]);

    let mut uir = Uir::new();
    uir.extra.extend([7, 11]);
    uir.instructions.push(Inst {
        tag: InstTag::AssignOrDecl,
        data: InstData::Extra(ExtraRange { offset: 0, len: 2 }),
    });
    uir.spans.push(sp());
    let v = uir.assign_or_decl_view(InstRef::from_index(1));
    assert_eq!(v.name, name);
    assert_eq!(v.value, value);
}

#[test]
fn compound_assign_wire_fixture() {
    let mut b = UirBuilder::new();
    b.uir.extra.extend([0xAAAA_AAAA; 5]);
    let name = StringId::from_raw(7);
    let value = InstRef::from_raw(11);

    let r = b.compound_assign(name, CompoundOp::Sub, value, sp());
    let InstData::Extra(range) = b.uir.inst(r).data else {
        panic!("CompoundAssign must carry InstData::Extra");
    };
    assert_eq!(range, ExtraRange { offset: 5, len: 3 });
    assert_eq!(&b.uir.extra[5..8], &[7, 1, 11]);

    let mut uir = Uir::new();
    uir.extra.extend([7, 1, 11]);
    uir.instructions.push(Inst {
        tag: InstTag::CompoundAssign,
        data: InstData::Extra(ExtraRange { offset: 0, len: 3 }),
    });
    uir.spans.push(sp());
    let v = uir.compound_assign_view(InstRef::from_index(1));
    assert_eq!(v.name, name);
    assert!(matches!(v.op, CompoundOp::Sub));
    assert_eq!(v.value, value);
}

#[test]
fn field_assign_wire_fixture() {
    let mut b = UirBuilder::new();
    b.uir.extra.extend([0xAAAA_AAAA; 5]);
    let target = InstRef::from_raw(21);
    let value = InstRef::from_raw(22);

    let r = b.field_assign(target, value, sp());
    let InstData::Extra(range) = b.uir.inst(r).data else {
        panic!("FieldAssign must carry InstData::Extra");
    };
    assert_eq!(range, ExtraRange { offset: 5, len: 2 });
    assert_eq!(&b.uir.extra[5..7], &[21, 22]);

    let mut uir = Uir::new();
    uir.extra.extend([21, 22]);
    uir.instructions.push(Inst {
        tag: InstTag::FieldAssign,
        data: InstData::Extra(ExtraRange { offset: 0, len: 2 }),
    });
    uir.spans.push(sp());
    let v = uir.field_assign_view(InstRef::from_index(1));
    assert_eq!(v.target, target);
    assert_eq!(v.value, value);
}

#[test]
fn compound_field_assign_wire_fixture() {
    let mut b = UirBuilder::new();
    b.uir.extra.extend([0xAAAA_AAAA; 5]);
    let target = InstRef::from_raw(21);
    let value = InstRef::from_raw(22);

    let r = b.compound_field_assign(target, CompoundOp::Sub, value, sp());
    let InstData::Extra(range) = b.uir.inst(r).data else {
        panic!("CompoundFieldAssign must carry InstData::Extra");
    };
    assert_eq!(range, ExtraRange { offset: 5, len: 3 });
    assert_eq!(&b.uir.extra[5..8], &[21, 1, 22]);

    let mut uir = Uir::new();
    uir.extra.extend([21, 1, 22]);
    uir.instructions.push(Inst {
        tag: InstTag::CompoundFieldAssign,
        data: InstData::Extra(ExtraRange { offset: 0, len: 3 }),
    });
    uir.spans.push(sp());
    let v = uir.compound_field_assign_view(InstRef::from_index(1));
    assert_eq!(v.target, target);
    assert!(matches!(v.op, CompoundOp::Sub));
    assert_eq!(v.value, value);
}

fn shell_view(words: &[u32], tag: InstTag, len: u32) -> Uir {
    let mut uir = Uir::new();
    let start = uir.extra.len() as u32;
    uir.extra.extend_from_slice(words);
    uir.instructions.push(Inst {
        tag,
        data: InstData::Extra(ExtraRange { offset: start, len }),
    });
    uir.spans.push(sp());
    uir
}

fn shell_ref(uir: &Uir) -> InstRef {
    InstRef::from_index(uir.instructions.len() - 1)
}

#[test]
fn call_wire_fixture() {
    let mut b = UirBuilder::new();
    b.uir.extra.extend([0xAAAA_AAAA; 5]);
    let name = StringId::from_raw(7);
    let a = InstRef::from_raw(21);
    let bb = InstRef::from_raw(22);

    let empty = b.call(name, &[], sp());
    let InstData::Extra(range) = b.uir.inst(empty).data else {
        panic!("Call must carry InstData::Extra");
    };
    assert_eq!(range, ExtraRange { offset: 5, len: 2 });
    assert_eq!(&b.uir.extra[5..7], &[7, 0]);

    let pop = b.call(name, &[a, bb], sp());
    let InstData::Extra(range2) = b.uir.inst(pop).data else {
        panic!("Call must carry InstData::Extra");
    };
    assert_eq!(range2, ExtraRange { offset: 7, len: 4 });
    assert_eq!(&b.uir.extra[7..11], &[7, 2, 21, 22]);

    let uir = shell_view(&[7, 0], InstTag::Call, 2);
    let v = uir.call_view(shell_ref(&uir));
    assert_eq!(v.name, name);
    assert_eq!(v.args.len(), 0);

    let uir = shell_view(&[7, 2, 21, 22], InstTag::Call, 4);
    let v = uir.call_view(shell_ref(&uir));
    assert_eq!(v.args.len(), 2);
    assert_eq!(v.args.at(1), bb);
}

#[test]
fn method_call_wire_fixture() {
    let mut b = UirBuilder::new();
    b.uir.extra.extend([0xAAAA_AAAA; 5]);
    let receiver = InstRef::from_raw(11);
    let name = StringId::from_raw(7);
    let a = InstRef::from_raw(21);
    let bb = InstRef::from_raw(22);

    let empty = b.method_call(receiver, name, &[], sp());
    let InstData::Extra(range) = b.uir.inst(empty).data else {
        panic!("MethodCall must carry InstData::Extra");
    };
    assert_eq!(range, ExtraRange { offset: 5, len: 3 });
    assert_eq!(&b.uir.extra[5..8], &[11, 7, 0]);

    let pop = b.method_call(receiver, name, &[a, bb], sp());
    let InstData::Extra(range2) = b.uir.inst(pop).data else {
        panic!("MethodCall must carry InstData::Extra");
    };
    assert_eq!(range2, ExtraRange { offset: 8, len: 5 });
    assert_eq!(&b.uir.extra[8..13], &[11, 7, 2, 21, 22]);

    let uir = shell_view(&[11, 7, 0], InstTag::MethodCall, 3);
    let v = uir.method_call_view(shell_ref(&uir));
    assert_eq!(v.receiver, receiver);
    assert_eq!(v.name, name);
    assert_eq!(v.args.len(), 0);

    let uir = shell_view(&[11, 7, 2, 21, 22], InstTag::MethodCall, 5);
    let v = uir.method_call_view(shell_ref(&uir));
    assert_eq!(v.args.len(), 2);
    assert_eq!(v.args.at(0), a);
}

#[test]
fn while_loop_wire_fixture() {
    let mut b = UirBuilder::new();
    b.uir.extra.extend([0xAAAA_AAAA; 5]);
    let cond = InstRef::from_raw(11);
    let s1 = InstRef::from_raw(21);
    let s2 = InstRef::from_raw(22);

    let empty = b.while_loop(cond, &[], sp());
    let InstData::Extra(range) = b.uir.inst(empty).data else {
        panic!("WhileLoop must carry InstData::Extra");
    };
    assert_eq!(range, ExtraRange { offset: 5, len: 2 });
    assert_eq!(&b.uir.extra[5..7], &[11, 0]);

    let pop = b.while_loop(cond, &[s1, s2], sp());
    let InstData::Extra(range2) = b.uir.inst(pop).data else {
        panic!("WhileLoop must carry InstData::Extra");
    };
    assert_eq!(range2, ExtraRange { offset: 7, len: 4 });
    assert_eq!(&b.uir.extra[7..11], &[11, 2, 21, 22]);

    let uir = shell_view(&[11, 0], InstTag::WhileLoop, 2);
    let v = uir.while_loop_view(shell_ref(&uir));
    assert_eq!(v.cond, cond);
    assert_eq!(v.body.len(), 0);

    let uir = shell_view(&[11, 2, 21, 22], InstTag::WhileLoop, 4);
    let v = uir.while_loop_view(shell_ref(&uir));
    assert_eq!(v.body.len(), 2);
    assert_eq!(v.body.at(1), s2);
}

#[test]
fn for_range_wire_fixture() {
    let mut b = UirBuilder::new();
    b.uir.extra.extend([0xAAAA_AAAA; 5]);
    let var_name = StringId::from_raw(7);
    let start = InstRef::from_raw(5);
    let end = InstRef::from_raw(9);
    let s1 = InstRef::from_raw(21);
    let s2 = InstRef::from_raw(22);

    let empty = b.for_range(var_name, start, end, &[], sp());
    let InstData::Extra(range) = b.uir.inst(empty).data else {
        panic!("ForRange must carry InstData::Extra");
    };
    assert_eq!(range, ExtraRange { offset: 5, len: 4 });
    assert_eq!(&b.uir.extra[5..9], &[7, 5, 9, 0]);

    let pop = b.for_range(var_name, start, end, &[s1, s2], sp());
    let InstData::Extra(range2) = b.uir.inst(pop).data else {
        panic!("ForRange must carry InstData::Extra");
    };
    assert_eq!(range2, ExtraRange { offset: 9, len: 6 });
    assert_eq!(&b.uir.extra[9..15], &[7, 5, 9, 2, 21, 22]);

    let uir = shell_view(&[7, 5, 9, 0], InstTag::ForRange, 4);
    let v = uir.for_range_view(shell_ref(&uir));
    assert_eq!(v.var_name, var_name);
    assert_eq!(v.start, start);
    assert_eq!(v.end, end);
    assert_eq!(v.body.len(), 0);

    let uir = shell_view(&[7, 5, 9, 2, 21, 22], InstTag::ForRange, 6);
    let v = uir.for_range_view(shell_ref(&uir));
    assert_eq!(v.body.len(), 2);
    assert_eq!(v.body.at(0), s1);
}

#[test]
fn struct_lit_wire_fixture() {
    let mut b = UirBuilder::new();
    b.uir.extra.extend([0xAAAA_AAAA; 5]);
    let name = StringId::from_raw(7);
    let f1 = StringId::from_raw(101);
    let f2 = StringId::from_raw(102);
    let v1 = InstRef::from_raw(21);
    let v2 = InstRef::from_raw(22);

    let named = b.struct_lit(Some(name), &[(f1, v1), (f2, v2)], sp());
    let InstData::Extra(range) = b.uir.inst(named).data else {
        panic!("StructLit must carry InstData::Extra");
    };
    assert_eq!(range, ExtraRange { offset: 5, len: 6 });
    assert_eq!(&b.uir.extra[5..11], &[7, 2, 101, 21, 102, 22]);

    let anon = b.struct_lit(None, &[], sp());
    let InstData::Extra(range2) = b.uir.inst(anon).data else {
        panic!("StructLit must carry InstData::Extra");
    };
    assert_eq!(range2, ExtraRange { offset: 11, len: 2 });
    assert_eq!(&b.uir.extra[11..13], &[u32::MAX, 0]);

    let uir = shell_view(&[7, 2, 101, 21, 102, 22], InstTag::StructLit, 6);
    let v = uir.struct_lit_view(shell_ref(&uir));
    assert_eq!(v.name, Some(name));
    assert_eq!(v.fields().collect::<Vec<_>>(), vec![(f1, v1), (f2, v2)]);

    let uir = shell_view(&[u32::MAX, 0], InstTag::StructLit, 2);
    let v = uir.struct_lit_view(shell_ref(&uir));
    assert_eq!(v.name, None);
    assert_eq!(v.fields().count(), 0);
}

#[test]
fn enum_lit_wire_fixture() {
    let mut b = UirBuilder::new();
    b.uir.extra.extend([0xAAAA_AAAA; 5]);
    let ty = TypeId::from_raw(3);
    let w = InstRef::from_raw(21);
    let h = InstRef::from_raw(22);

    let two = b.enum_lit(ty, 1, false, &[(0, w), (1, h)], sp());
    let InstData::Extra(range) = b.uir.inst(two).data else {
        panic!("EnumLit must carry InstData::Extra");
    };
    assert_eq!(range, ExtraRange { offset: 5, len: 8 });
    assert_eq!(&b.uir.extra[5..13], &[3, 1, 2, 0, 0, 21, 1, 22]);

    let unit = b.enum_lit(ty, 1, false, &[], sp());
    let InstData::Extra(range2) = b.uir.inst(unit).data else {
        panic!("EnumLit must carry InstData::Extra");
    };
    assert_eq!(range2, ExtraRange { offset: 13, len: 4 });
    assert_eq!(&b.uir.extra[13..17], &[3, 1, 0, 0]);

    let pos = b.enum_lit(ty, 1, true, &[(0, w)], sp());
    let InstData::Extra(range3) = b.uir.inst(pos).data else {
        panic!("EnumLit must carry InstData::Extra");
    };
    assert_eq!(range3, ExtraRange { offset: 17, len: 6 });
    assert_eq!(&b.uir.extra[17..23], &[3, 1, 1, 1, 0, 21]);

    let uir = shell_view(&[3, 1, 2, 0, 0, 21, 1, 22], InstTag::EnumLit, 8);
    let v = uir.enum_lit_view(shell_ref(&uir));
    assert_eq!(v.ty, ty);
    assert_eq!(v.variant_index, 1);
    assert!(!v.positional);
    assert_eq!(v.fields().collect::<Vec<_>>(), vec![(0, w), (1, h)]);

    let uir = shell_view(&[3, 1, 0, 0], InstTag::EnumLit, 4);
    let v = uir.enum_lit_view(shell_ref(&uir));
    assert_eq!(v.fields().count(), 0);

    let uir = shell_view(&[3, 1, 1, 1, 0, 21], InstTag::EnumLit, 6);
    let v = uir.enum_lit_view(shell_ref(&uir));
    assert!(v.positional);
    assert_eq!(v.fields().collect::<Vec<_>>(), vec![(0, w)]);
}

#[test]
fn destructure_wire_fixture() {
    let mut b = UirBuilder::new();
    b.uir.extra.extend([0xAAAA_AAAA; 5]);
    let value = InstRef::from_raw(11);
    let plan = [
        (0u32, None),
        (1, Some(StringId::from_raw(0))),
        (2, Some(StringId::from_raw(5))),
    ];

    let r = b.destructure(value, true, &plan, sp());
    let InstData::Destructure {
        plan: range,
        by_position,
        ..
    } = b.uir.inst(r).data
    else {
        panic!("Destructure must carry InstData::Destructure");
    };
    assert!(by_position);
    assert_eq!(range, ExtraRange { offset: 5, len: 7 });
    assert_eq!(&b.uir.extra[5..12], &[3, 0, 0, 1, 1, 2, 6]);

    let empty = b.destructure(value, false, &[], sp());
    let InstData::Destructure {
        plan: range2,
        by_position: bp2,
        ..
    } = b.uir.inst(empty).data
    else {
        panic!("Destructure must carry InstData::Destructure");
    };
    assert!(!bp2);
    assert_eq!(range2, ExtraRange { offset: 12, len: 1 });
    assert_eq!(&b.uir.extra[12..13], &[0]);

    let mut uir = Uir::new();
    uir.extra.extend([3, 0, 0, 1, 1, 2, 6]);
    uir.instructions.push(Inst {
        tag: InstTag::Destructure,
        data: InstData::Destructure {
            value,
            plan: ExtraRange { offset: 0, len: 7 },
            by_position: true,
        },
    });
    uir.spans.push(sp());
    let v = uir.destructure_view(InstRef::from_index(1));
    assert_eq!(v.value, value);
    assert!(v.by_position);
    assert_eq!(
        v.plan,
        vec![
            (0, None),
            (1, Some(StringId::from_raw(0))),
            (2, Some(StringId::from_raw(5))),
        ]
    );
}

#[test]
fn if_stmt_wire_fixture() {
    let mut b = UirBuilder::new();
    b.uir.extra.extend([0xAAAA_AAAA; 5]);
    let cond = InstRef::from_raw(11);
    let t1 = InstRef::from_raw(21);
    let c12 = InstRef::from_raw(12);
    let b22 = InstRef::from_raw(22);
    let c13 = InstRef::from_raw(13);
    let e23 = InstRef::from_raw(23);

    let no_else = b.if_stmt(cond, &[t1], &[], None, sp());
    let InstData::Extra(range) = b.uir.inst(no_else).data else {
        panic!("IfStmt must carry InstData::Extra");
    };
    assert_eq!(range, ExtraRange { offset: 5, len: 5 });
    assert_eq!(&b.uir.extra[5..10], &[11, 1, 21, 0, 0]);

    let empty_else = b.if_stmt(cond, &[t1], &[], Some(&[]), sp());
    let InstData::Extra(range2) = b.uir.inst(empty_else).data else {
        panic!("IfStmt must carry InstData::Extra");
    };
    assert_eq!(range2, ExtraRange { offset: 10, len: 6 });
    assert_eq!(&b.uir.extra[10..16], &[11, 1, 21, 0, 1, 0]);

    let all_empty = b.if_stmt(cond, &[], &[], None, sp());
    let InstData::Extra(range3) = b.uir.inst(all_empty).data else {
        panic!("IfStmt must carry InstData::Extra");
    };
    assert_eq!(range3, ExtraRange { offset: 16, len: 4 });
    assert_eq!(&b.uir.extra[16..20], &[11, 0, 0, 0]);

    let full = b.if_stmt(
        cond,
        &[t1],
        &[(c12, vec![b22]), (c13, vec![])],
        Some(&[e23]),
        sp(),
    );
    let InstData::Extra(range4) = b.uir.inst(full).data else {
        panic!("IfStmt must carry InstData::Extra");
    };
    assert_eq!(
        range4,
        ExtraRange {
            offset: 20,
            len: 12
        }
    );
    assert_eq!(
        &b.uir.extra[20..32],
        &[11, 1, 21, 2, 12, 1, 22, 13, 0, 1, 1, 23]
    );

    let uir = shell_view(&[11, 1, 21, 0, 0], InstTag::IfStmt, 5);
    let v = uir.if_stmt_view(shell_ref(&uir));
    assert_eq!(v.cond, cond);
    assert_eq!(v.then_stmts.len(), 1);
    assert_eq!(v.then_stmts.at(0), t1);
    assert!(v.else_stmts.is_none());
    assert_eq!(v.elif_branches.len(), 0);

    let uir = shell_view(&[11, 1, 21, 0, 1, 0], InstTag::IfStmt, 6);
    let v = uir.if_stmt_view(shell_ref(&uir));
    assert_eq!(v.else_stmts.unwrap().len(), 0);

    let uir = shell_view(
        &[11, 1, 21, 2, 12, 1, 22, 13, 0, 1, 1, 23],
        InstTag::IfStmt,
        12,
    );
    let v = uir.if_stmt_view(shell_ref(&uir));
    let elifs = v
        .elif_branches
        .map(|e| (e.cond, e.body.len()))
        .collect::<Vec<_>>();
    assert_eq!(elifs, vec![(c12, 1), (c13, 0)]);
    assert_eq!(v.else_stmts.unwrap().at(0), e23);
}
