pub use super::wires::assign_extra::AssignView;
pub use super::wires::compound_assign_extra::CompoundAssignView;
pub use super::wires::compound_field_assign_extra::CompoundFieldAssignView;
pub use super::wires::field_assign_extra::FieldAssignView;
pub use super::wires::var_decl_extra::VarDeclView;
use super::{
    ExtraList, ParamMode, StringId, Tir, TirData, TirRef, TirTag, TypeId, assign_extra, call_extra,
    compound_assign_extra, compound_field_assign_extra, enum_lit_extra, field_assign_extra,
    for_range_extra, read_ref_list_view, skip_ref_list, struct_lit_extra, var_decl_extra,
    while_loop_extra,
};

// ---------- Read-side helpers ----------

pub struct CallView<'a> {
    pub name: StringId,
    pub args: ExtraList<'a, TirRef>,
    /// Per-argument call convention, parallel to `args`. Stamped by
    /// sema from each callee parameter's `mode` field.
    pub modes: ExtraList<'a, ParamMode>,
}

#[derive(Clone, Copy)]
pub struct TirElifView<'a> {
    pub cond: TirRef,
    pub body: ExtraList<'a, TirRef>,
}

#[derive(Clone, Copy)]
pub struct TirElifList<'a> {
    words: &'a [u32],
    count: usize,
}

impl<'a> TirElifList<'a> {
    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn iter(&self) -> TirElifIter<'a> {
        TirElifIter {
            words: self.words,
            pos: 0,
            remaining: self.count,
        }
    }
}

pub struct TirElifIter<'a> {
    words: &'a [u32],
    pos: usize,
    remaining: usize,
}

impl<'a> Iterator for TirElifIter<'a> {
    type Item = TirElifView<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let cond = TirRef::from_raw(self.words[self.pos]);
        self.pos += 1;
        let body = read_ref_list_view(self.words, &mut self.pos);
        self.remaining -= 1;
        Some(TirElifView { cond, body })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl ExactSizeIterator for TirElifIter<'_> {}

impl<'a> IntoIterator for TirElifList<'a> {
    type Item = TirElifView<'a>;
    type IntoIter = TirElifIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        TirElifIter {
            words: self.words,
            pos: 0,
            remaining: self.count,
        }
    }
}

impl<'words> IntoIterator for &TirElifList<'words> {
    type Item = TirElifView<'words>;
    type IntoIter = TirElifIter<'words>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

pub struct TirIfStmtView<'a> {
    pub cond: TirRef,
    pub then_stmts: ExtraList<'a, TirRef>,
    pub elif_branches: TirElifList<'a>,
    pub else_stmts: Option<ExtraList<'a, TirRef>>,
}

pub struct WhileLoopView<'a> {
    pub cond: TirRef,
    pub body: ExtraList<'a, TirRef>,
}

pub struct ForRangeView<'a> {
    pub var_name: StringId,
    pub start: TirRef,
    pub end: TirRef,
    pub body: ExtraList<'a, TirRef>,
}

/// Borrowed view of a [`TirTag::StructLit`] payload (M9). Fields are
/// `(field_index, value)` pairs in canonical declaration order.
pub struct StructLitView<'a> {
    pub ty: TypeId,
    fields: &'a [u32],
}

impl StructLitView<'_> {
    /// `(field_index, value)` pairs in canonical declaration order.
    pub fn fields(&self) -> impl ExactSizeIterator<Item = (u32, TirRef)> + '_ {
        self.fields
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| (pair[0], TirRef::from_raw(pair[1])))
    }
}

/// Decoded view of a [`TirTag::EnumLit`] payload (M11). Borrows the
/// `extra` arena directly — decoding allocates nothing; `fields()` yields `(field_index,
/// value)` pairs keyed by declaration-order payload-field index, in
/// the order sema encoded them.
pub struct EnumLitView<'a> {
    pub ty: TypeId,
    pub variant_index: u32,
    /// Construction's source form as sema recorded it (wire parity
    /// with UIR; codegen and ownership ignore it).
    pub positional: bool,
    args: &'a [u32],
}

impl EnumLitView<'_> {
    /// `(field_index, value)` pairs, in the order sema encoded them.
    pub fn fields(&self) -> impl Iterator<Item = (u32, TirRef)> + '_ {
        self.args
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&[field_idx, raw]| (field_idx, TirRef::from_raw(raw)))
    }
}

impl Tir {
    pub fn call_view(&self, r: TirRef) -> CallView<'_> {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, TirTag::Call));
        let range = match inst.data {
            TirData::Extra(rng) => rng,
            _ => unreachable!("Call must carry TirData::Extra"),
        };
        let slice = &self.extra[range.as_range()];
        let name = StringId::from_raw(slice[call_extra::NAME]);
        let argc = slice[call_extra::ARGC] as usize;
        let args = ExtraList::new(&slice[call_extra::ARGS..call_extra::ARGS + argc]);
        let modes = ExtraList::new(&slice[call_extra::ARGS + argc..call_extra::ARGS + 2 * argc]);
        CallView { name, args, modes }
    }

    pub fn var_decl_view(&self, r: TirRef) -> VarDeclView {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, TirTag::VarDecl));
        let range = match inst.data {
            TirData::Extra(rng) => rng,
            _ => unreachable!("VarDecl must carry TirData::Extra"),
        };
        var_decl_extra::decode(&self.extra[range.as_range()])
    }

    pub fn assign_view(&self, r: TirRef) -> AssignView {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, TirTag::Assign));
        let range = match inst.data {
            TirData::Extra(rng) => rng,
            _ => unreachable!("Assign must carry TirData::Extra"),
        };
        assign_extra::decode(&self.extra[range.as_range()])
    }

    pub fn compound_assign_view(&self, r: TirRef) -> CompoundAssignView {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, TirTag::CompoundAssign));
        let range = match inst.data {
            TirData::Extra(rng) => rng,
            _ => unreachable!("CompoundAssign must carry TirData::Extra"),
        };
        compound_assign_extra::decode(&self.extra[range.as_range()])
    }

    pub fn field_assign_view(&self, r: TirRef) -> FieldAssignView {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, TirTag::FieldAssign));
        let range = match inst.data {
            TirData::Extra(rng) => rng,
            _ => unreachable!("FieldAssign must carry TirData::Extra"),
        };
        field_assign_extra::decode(&self.extra[range.as_range()])
    }

    pub fn compound_field_assign_view(&self, r: TirRef) -> CompoundFieldAssignView {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, TirTag::CompoundFieldAssign));
        let range = match inst.data {
            TirData::Extra(rng) => rng,
            _ => unreachable!("CompoundFieldAssign must carry TirData::Extra"),
        };
        compound_field_assign_extra::decode(&self.extra[range.as_range()])
    }

    /// Borrow the branch statement lists in an `IfStmt` without collecting them.
    pub fn if_stmt_view(&self, r: TirRef) -> TirIfStmtView<'_> {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, TirTag::IfStmt));
        let range = match inst.data {
            TirData::Extra(rng) => rng,
            _ => unreachable!("IfStmt must carry TirData::Extra"),
        };
        let slice = &self.extra[range.as_range()];
        let mut pos = 1;
        let cond = TirRef::from_raw(slice[0]);
        let then_stmts = read_ref_list_view(slice, &mut pos);
        let elif_count = slice[pos] as usize;
        pos += 1;
        let elif_start = pos;
        for _ in 0..elif_count {
            pos += 1;
            skip_ref_list(slice, &mut pos);
        }
        let elif_branches = TirElifList {
            words: &slice[elif_start..pos],
            count: elif_count,
        };
        let has_else = slice[pos] != 0;
        pos += 1;
        let else_stmts = has_else.then(|| read_ref_list_view(slice, &mut pos));

        TirIfStmtView {
            cond,
            then_stmts,
            elif_branches,
            else_stmts,
        }
    }

    pub fn while_loop_view(&self, r: TirRef) -> WhileLoopView<'_> {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, TirTag::WhileLoop));
        let range = match inst.data {
            TirData::Extra(rng) => rng,
            _ => unreachable!("WhileLoop must carry TirData::Extra"),
        };
        let slice = &self.extra[range.as_range()];
        let cond = TirRef::from_raw(slice[while_loop_extra::COND]);
        let body = ExtraList::new(&slice[while_loop_extra::BODY_START..]);
        WhileLoopView { cond, body }
    }

    /// Borrow the statement refs in a `WhileLoop` body from the extra arena.
    pub fn while_loop_body_view(&self, r: TirRef) -> ExtraList<'_, TirRef> {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, TirTag::WhileLoop));
        let range = match inst.data {
            TirData::Extra(rng) => rng,
            _ => unreachable!("WhileLoop must carry TirData::Extra"),
        };
        let slice = &self.extra[range.as_range()];
        ExtraList::new(&slice[while_loop_extra::BODY_START..])
    }

    pub fn for_range_view(&self, r: TirRef) -> ForRangeView<'_> {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, TirTag::ForRange));
        let range = match inst.data {
            TirData::Extra(rng) => rng,
            _ => unreachable!("ForRange must carry TirData::Extra"),
        };
        let slice = &self.extra[range.as_range()];
        let var_name = StringId::from_raw(slice[for_range_extra::VAR_NAME]);
        let start = TirRef::from_raw(slice[for_range_extra::START]);
        let end = TirRef::from_raw(slice[for_range_extra::END]);
        let body = ExtraList::new(&slice[for_range_extra::BODY_START..]);
        ForRangeView {
            var_name,
            start,
            end,
            body,
        }
    }

    /// Borrow the statement refs in a `ForRange` body from the extra arena.
    pub fn for_range_body_view(&self, r: TirRef) -> ExtraList<'_, TirRef> {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, TirTag::ForRange));
        let range = match inst.data {
            TirData::Extra(rng) => rng,
            _ => unreachable!("ForRange must carry TirData::Extra"),
        };
        let slice = &self.extra[range.as_range()];
        ExtraList::new(&slice[for_range_extra::BODY_START..])
    }

    pub fn struct_lit_view(&self, r: TirRef) -> StructLitView<'_> {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, TirTag::StructLit));
        let range = match inst.data {
            TirData::Extra(rng) => rng,
            _ => unreachable!("StructLit must carry TirData::Extra"),
        };
        let slice = &self.extra[range.as_range()];
        let ty = TypeId::from_raw(slice[struct_lit_extra::TY]);
        let n = slice[struct_lit_extra::N_FIELDS] as usize;
        let fields_end = struct_lit_extra::FIELDS + 2 * n;
        StructLitView {
            ty,
            fields: &slice[struct_lit_extra::FIELDS..fields_end],
        }
    }

    pub fn enum_lit_view(&self, r: TirRef) -> EnumLitView<'_> {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, TirTag::EnumLit));
        let range = match inst.data {
            TirData::Extra(rng) => rng,
            _ => unreachable!("EnumLit must carry TirData::Extra"),
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
}
