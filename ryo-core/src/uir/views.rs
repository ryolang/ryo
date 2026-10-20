pub use super::wires::assign_or_decl_extra::AssignOrDeclView;
pub use super::wires::call_extra::CallView;
pub use super::wires::compound_assign_extra::CompoundAssignView;
pub use super::wires::compound_field_assign_extra::CompoundFieldAssignView;
pub use super::wires::field_assign_extra::FieldAssignView;
pub use super::wires::for_range_extra::ForRangeView;
pub use super::wires::method_call_extra::MethodCallView;
pub use super::wires::struct_lit_extra::StructLitView;
pub use super::wires::var_decl_extra::VarDeclView;
pub use super::wires::while_loop_extra::WhileLoopView;
use super::{
    ExtraList, InstData, InstRef, InstTag, TypeId, Uir, assign_or_decl_extra, call_extra,
    compound_assign_extra, compound_field_assign_extra, enum_lit_extra, field_assign_extra,
    for_range_extra, method_call_extra, struct_lit_extra, var_decl_extra, while_loop_extra,
};

// ---------- Read-side helpers ----------

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
        call_extra::decode(&self.extra[range.as_range()])
    }

    pub fn var_decl_view(&self, r: InstRef) -> VarDeclView {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, InstTag::VarDecl));
        let range = match inst.data {
            InstData::Extra(rng) => rng,
            _ => unreachable!("VarDecl must carry InstData::Extra"),
        };
        var_decl_extra::decode(&self.extra[range.as_range()])
    }

    pub fn assign_or_decl_view(&self, r: InstRef) -> AssignOrDeclView {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, InstTag::AssignOrDecl));
        let range = match inst.data {
            InstData::Extra(rng) => rng,
            _ => unreachable!("AssignOrDecl must carry InstData::Extra"),
        };
        assign_or_decl_extra::decode(&self.extra[range.as_range()])
    }

    pub fn compound_assign_view(&self, r: InstRef) -> CompoundAssignView {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, InstTag::CompoundAssign));
        let range = match inst.data {
            InstData::Extra(rng) => rng,
            _ => unreachable!("CompoundAssign must carry InstData::Extra"),
        };
        compound_assign_extra::decode(&self.extra[range.as_range()])
    }

    pub fn field_assign_view(&self, r: InstRef) -> FieldAssignView {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, InstTag::FieldAssign));
        let range = match inst.data {
            InstData::Extra(rng) => rng,
            _ => unreachable!("FieldAssign must carry InstData::Extra"),
        };
        field_assign_extra::decode(&self.extra[range.as_range()])
    }

    pub fn compound_field_assign_view(&self, r: InstRef) -> CompoundFieldAssignView {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, InstTag::CompoundFieldAssign));
        let range = match inst.data {
            InstData::Extra(rng) => rng,
            _ => unreachable!("CompoundFieldAssign must carry InstData::Extra"),
        };
        compound_field_assign_extra::decode(&self.extra[range.as_range()])
    }

    pub fn while_loop_view(&self, r: InstRef) -> WhileLoopView<'_> {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, InstTag::WhileLoop));
        let range = match inst.data {
            InstData::Extra(rng) => rng,
            _ => unreachable!("WhileLoop must carry InstData::Extra"),
        };
        while_loop_extra::decode(&self.extra[range.as_range()])
    }

    pub fn for_range_view(&self, r: InstRef) -> ForRangeView<'_> {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, InstTag::ForRange));
        let range = match inst.data {
            InstData::Extra(rng) => rng,
            _ => unreachable!("ForRange must carry InstData::Extra"),
        };
        for_range_extra::decode(&self.extra[range.as_range()])
    }

    pub fn method_call_view(&self, r: InstRef) -> MethodCallView<'_> {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, InstTag::MethodCall));
        let range = match inst.data {
            InstData::Extra(rng) => rng,
            _ => unreachable!("MethodCall must carry InstData::Extra"),
        };
        method_call_extra::decode(&self.extra[range.as_range()])
    }

    pub fn struct_lit_view(&self, r: InstRef) -> StructLitView<'_> {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, InstTag::StructLit));
        let range = match inst.data {
            InstData::Extra(rng) => rng,
            _ => unreachable!("StructLit must carry InstData::Extra"),
        };
        struct_lit_extra::decode(&self.extra[range.as_range()])
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
