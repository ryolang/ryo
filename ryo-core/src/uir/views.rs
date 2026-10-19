use super::{
    CompoundOp, ExtraList, InstData, InstRef, InstTag, StringId, TypeId, Uir, assign_or_decl_extra,
    call_extra, compound_assign_extra, compound_field_assign_extra, enum_lit_extra,
    field_assign_extra, for_range_extra, method_call_extra, struct_lit_extra, var_decl_extra,
    while_loop_extra,
};

// ---------- Read-side helpers ----------

/// Decoded view of an [`InstTag::Call`] payload.
pub struct CallView<'a> {
    pub name: StringId,
    pub args: ExtraList<'a, InstRef>,
}

/// Decoded view of an [`InstTag::VarDecl`] payload.
pub struct VarDeclView {
    pub name: StringId,
    pub mutable: bool,
    /// `None` when the source had no annotation.
    pub ty: Option<TypeId>,
    pub initializer: InstRef,
}

pub struct AssignOrDeclView {
    pub name: StringId,
    pub value: InstRef,
}

pub struct CompoundAssignView {
    pub name: StringId,
    pub op: CompoundOp,
    pub value: InstRef,
}

/// Decoded view of an [`InstTag::FieldAssign`] payload (M9).
pub struct FieldAssignView {
    pub target: InstRef,
    pub value: InstRef,
}

/// Decoded view of an [`InstTag::CompoundFieldAssign`] payload (M9).
pub struct CompoundFieldAssignView {
    pub target: InstRef,
    pub op: CompoundOp,
    pub value: InstRef,
}

pub struct WhileLoopView<'a> {
    pub cond: InstRef,
    pub body: ExtraList<'a, InstRef>,
}

pub struct ForRangeView<'a> {
    pub var_name: StringId,
    pub start: InstRef,
    pub end: InstRef,
    pub body: ExtraList<'a, InstRef>,
}

/// Decoded view of an [`InstTag::MethodCall`] payload.
pub struct MethodCallView<'a> {
    pub receiver: InstRef,
    pub name: StringId,
    pub args: ExtraList<'a, InstRef>,
}

/// Decoded borrowed view of an [`InstTag::StructLit`] payload. Field
/// initializers are in source order. `name` of `None` is the M10
/// anonymous literal (encoded as
/// [`struct_lit_extra::UIR_ANON_STRUCT_NAME`]); sema infers the
/// structural type from the field values instead of resolving a name.
pub struct StructLitView<'a> {
    pub name: Option<StringId>,
    fields: &'a [u32],
}

impl StructLitView<'_> {
    /// Field initializers in source order.
    pub fn fields(&self) -> impl ExactSizeIterator<Item = (StringId, InstRef)> + '_ {
        self.fields
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| (StringId::from_raw(pair[0]), InstRef::from_raw(pair[1])))
    }
}

/// Decoded view of an [`InstTag::EnumLit`] payload (M11). Borrows the
/// `extra` arena directly — decoding allocates nothing; `fields()` yields `(field_index,
/// value)` pairs keyed by declaration-order payload-field index, in
/// source order.
pub struct EnumLitView<'a> {
    pub ty: TypeId,
    pub variant_index: u32,
    /// Source used the parenthesized positional form (astgen-recorded;
    /// see [`enum_lit_extra::FLAG_POSITIONAL`]).
    pub positional: bool,
    args: &'a [u32],
}

impl EnumLitView<'_> {
    /// `(field_index, value)` pairs, in source order.
    pub fn fields(&self) -> impl Iterator<Item = (u32, InstRef)> + '_ {
        self.args
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&[field_idx, raw]| (field_idx, InstRef::from_raw(raw)))
    }
}

pub struct ElifView<'a> {
    pub cond: InstRef,
    pub body: ExtraList<'a, InstRef>,
}

pub struct ElifBranches<'a> {
    words: &'a [u32],
    remaining: usize,
}

impl<'a> Iterator for ElifBranches<'a> {
    type Item = ElifView<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;
        let cond = InstRef::from_raw(self.words[0]);
        let count = self.words[1] as usize;
        let body_end = 2 + count;
        let body = ExtraList::new(&self.words[2..body_end]);
        self.words = &self.words[body_end..];
        Some(ElifView { cond, body })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl ExactSizeIterator for ElifBranches<'_> {}

pub struct IfStmtView<'a> {
    pub cond: InstRef,
    pub then_stmts: ExtraList<'a, InstRef>,
    pub elif_branches: ElifBranches<'a>,
    pub else_stmts: Option<ExtraList<'a, InstRef>>,
}

impl Uir {
    pub fn call_view(&self, r: InstRef) -> CallView<'_> {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, InstTag::Call));
        let range = match inst.data {
            InstData::Extra(rng) => rng,
            _ => unreachable!("Call must carry InstData::Extra"),
        };
        let slice = &self.extra[range.as_range()];
        let name = StringId::from_raw(slice[call_extra::NAME]);
        let argc = slice[call_extra::ARGC] as usize;
        let args = ExtraList::new(&slice[call_extra::ARGS..call_extra::ARGS + argc]);
        CallView { name, args }
    }

    pub fn var_decl_view(&self, r: InstRef) -> VarDeclView {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, InstTag::VarDecl));
        let range = match inst.data {
            InstData::Extra(rng) => rng,
            _ => unreachable!("VarDecl must carry InstData::Extra"),
        };
        let slice = &self.extra[range.as_range()];
        let name = StringId::from_raw(slice[var_decl_extra::NAME]);
        let mutable = slice[var_decl_extra::FLAGS] & var_decl_extra::FLAG_MUTABLE != 0;
        let ty_raw = slice[var_decl_extra::TY];
        let ty = if ty_raw == var_decl_extra::TY_NONE_SENTINEL {
            None
        } else {
            Some(TypeId::from_raw(ty_raw))
        };
        let initializer = InstRef::from_raw(slice[var_decl_extra::INIT]);
        VarDeclView {
            name,
            mutable,
            ty,
            initializer,
        }
    }

    pub fn assign_or_decl_view(&self, r: InstRef) -> AssignOrDeclView {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, InstTag::AssignOrDecl));
        let range = match inst.data {
            InstData::Extra(rng) => rng,
            _ => unreachable!("AssignOrDecl must carry InstData::Extra"),
        };
        let slice = &self.extra[range.as_range()];
        AssignOrDeclView {
            name: StringId::from_raw(slice[assign_or_decl_extra::NAME]),
            value: InstRef::from_raw(slice[assign_or_decl_extra::VALUE]),
        }
    }

    pub fn compound_assign_view(&self, r: InstRef) -> CompoundAssignView {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, InstTag::CompoundAssign));
        let range = match inst.data {
            InstData::Extra(rng) => rng,
            _ => unreachable!("CompoundAssign must carry InstData::Extra"),
        };
        let slice = &self.extra[range.as_range()];
        CompoundAssignView {
            name: StringId::from_raw(slice[compound_assign_extra::NAME]),
            op: CompoundOp::from_raw(slice[compound_assign_extra::OP]),
            value: InstRef::from_raw(slice[compound_assign_extra::VALUE]),
        }
    }

    pub fn field_assign_view(&self, r: InstRef) -> FieldAssignView {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, InstTag::FieldAssign));
        let range = match inst.data {
            InstData::Extra(rng) => rng,
            _ => unreachable!("FieldAssign must carry InstData::Extra"),
        };
        let slice = &self.extra[range.as_range()];
        FieldAssignView {
            target: InstRef::from_raw(slice[field_assign_extra::TARGET]),
            value: InstRef::from_raw(slice[field_assign_extra::VALUE]),
        }
    }

    pub fn compound_field_assign_view(&self, r: InstRef) -> CompoundFieldAssignView {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, InstTag::CompoundFieldAssign));
        let range = match inst.data {
            InstData::Extra(rng) => rng,
            _ => unreachable!("CompoundFieldAssign must carry InstData::Extra"),
        };
        let slice = &self.extra[range.as_range()];
        CompoundFieldAssignView {
            target: InstRef::from_raw(slice[compound_field_assign_extra::TARGET]),
            op: CompoundOp::from_raw(slice[compound_field_assign_extra::OP]),
            value: InstRef::from_raw(slice[compound_field_assign_extra::VALUE]),
        }
    }

    pub fn while_loop_view(&self, r: InstRef) -> WhileLoopView<'_> {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, InstTag::WhileLoop));
        let range = match inst.data {
            InstData::Extra(rng) => rng,
            _ => unreachable!("WhileLoop must carry InstData::Extra"),
        };
        let slice = &self.extra[range.as_range()];
        let cond = InstRef::from_raw(slice[while_loop_extra::COND]);
        let body = ExtraList::new(&slice[while_loop_extra::BODY_START..]);
        WhileLoopView { cond, body }
    }

    pub fn for_range_view(&self, r: InstRef) -> ForRangeView<'_> {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, InstTag::ForRange));
        let range = match inst.data {
            InstData::Extra(rng) => rng,
            _ => unreachable!("ForRange must carry InstData::Extra"),
        };
        let slice = &self.extra[range.as_range()];
        let var_name = StringId::from_raw(slice[for_range_extra::VAR_NAME]);
        let start = InstRef::from_raw(slice[for_range_extra::START]);
        let end = InstRef::from_raw(slice[for_range_extra::END]);
        let body = ExtraList::new(&slice[for_range_extra::BODY_START..]);
        ForRangeView {
            var_name,
            start,
            end,
            body,
        }
    }

    pub fn method_call_view(&self, r: InstRef) -> MethodCallView<'_> {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, InstTag::MethodCall));
        let range = match inst.data {
            InstData::Extra(rng) => rng,
            _ => unreachable!("MethodCall must carry InstData::Extra"),
        };
        let slice = &self.extra[range.as_range()];
        let receiver = InstRef::from_raw(slice[method_call_extra::RECEIVER]);
        let name = StringId::from_raw(slice[method_call_extra::NAME]);
        let argc = slice[method_call_extra::ARGC] as usize;
        let args = ExtraList::new(&slice[method_call_extra::ARGS..method_call_extra::ARGS + argc]);
        MethodCallView {
            receiver,
            name,
            args,
        }
    }

    pub fn struct_lit_view(&self, r: InstRef) -> StructLitView<'_> {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, InstTag::StructLit));
        let range = match inst.data {
            InstData::Extra(rng) => rng,
            _ => unreachable!("StructLit must carry InstData::Extra"),
        };
        let slice = &self.extra[range.as_range()];
        let name = match slice[struct_lit_extra::NAME] {
            struct_lit_extra::UIR_ANON_STRUCT_NAME => None,
            raw => Some(StringId::from_raw(raw)),
        };
        let n = slice[struct_lit_extra::NFIELDS] as usize;
        let fields_end = struct_lit_extra::FIELDS + 2 * n;
        StructLitView {
            name,
            fields: &slice[struct_lit_extra::FIELDS..fields_end],
        }
    }

    pub fn enum_lit_view(&self, r: InstRef) -> EnumLitView<'_> {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, InstTag::EnumLit));
        let range = match inst.data {
            InstData::Extra(rng) => rng,
            _ => unreachable!("EnumLit must carry InstData::Extra"),
        };
        let slice = &self.extra[range.as_range()];
        let ty = TypeId::from_raw(slice[enum_lit_extra::TY]);
        let variant_index = slice[enum_lit_extra::VARIANT];
        let positional = slice[enum_lit_extra::FLAGS] & enum_lit_extra::FLAG_POSITIONAL != 0;
        let n = slice[enum_lit_extra::ARGC] as usize;
        let args = &slice[enum_lit_extra::ARGS..enum_lit_extra::ARGS + 2 * n];
        EnumLitView {
            ty,
            variant_index,
            positional,
            args,
        }
    }

    pub fn if_stmt_view(&self, r: InstRef) -> IfStmtView<'_> {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, InstTag::IfStmt));
        let range = match inst.data {
            InstData::Extra(rng) => rng,
            _ => unreachable!("IfStmt must carry InstData::Extra"),
        };
        let slice = &self.extra[range.as_range()];
        let mut pos = 0;

        let cond = InstRef::from_raw(slice[pos]);
        pos += 1;

        let then_stmts = read_ref_list(slice, &mut pos);

        let elif_count = slice[pos] as usize;
        pos += 1;
        let elif_start = pos;
        for _ in 0..elif_count {
            let body_len = slice[pos + 1] as usize;
            pos += 2 + body_len;
        }
        let elif_branches = ElifBranches {
            words: &slice[elif_start..pos],
            remaining: elif_count,
        };

        let has_else = slice[pos] != 0;
        pos += 1;
        let else_stmts = if has_else {
            Some(read_ref_list(slice, &mut pos))
        } else {
            None
        };

        IfStmtView {
            cond,
            then_stmts,
            elif_branches,
            else_stmts,
        }
    }
}

fn read_ref_list<'a>(slice: &'a [u32], pos: &mut usize) -> ExtraList<'a, InstRef> {
    let count = slice[*pos] as usize;
    *pos += 1;
    let refs = ExtraList::new(&slice[*pos..*pos + count]);
    *pos += count;
    refs
}
