pub use super::wires::assign_extra::AssignView;
pub use super::wires::call_extra::CallView;
pub use super::wires::compound_assign_extra::CompoundAssignView;
pub use super::wires::compound_field_assign_extra::CompoundFieldAssignView;
pub use super::wires::field_assign_extra::FieldAssignView;
pub use super::wires::for_range_extra::ForRangeView;
pub use super::wires::if_stmt_extra::{TirElifIter, TirElifList, TirElifView, TirIfStmtView};
pub use super::wires::struct_lit_extra::StructLitView;
pub use super::wires::var_decl_extra::VarDeclView;
pub use super::wires::while_loop_extra::WhileLoopView;
use super::{
    ExtraList, Tir, TirData, TirRef, TirTag, assign_extra, call_extra, compound_assign_extra,
    compound_field_assign_extra, enum_lit_extra, field_assign_extra, for_range_extra,
    if_stmt_extra, struct_lit_extra, var_decl_extra, while_loop_extra,
};

// ---------- Read-side helpers ----------

/// Decoded view of a [`TirTag::EnumLit`] payload (M11) — the wire is
/// shared with UIR; see [`crate::extra::enum_lit_extra`].
pub type EnumLitView<'a> = crate::extra::enum_lit_extra::EnumLitView<'a, TirRef>;

impl Tir {
    pub fn call_view(&self, r: TirRef) -> CallView<'_> {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, TirTag::Call));
        let range = match inst.data {
            TirData::Extra(rng) => rng,
            _ => unreachable!("Call must carry TirData::Extra"),
        };
        call_extra::decode(&self.extra[range.as_range()])
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
        if_stmt_extra::decode(&self.extra[range.as_range()])
    }

    pub fn while_loop_view(&self, r: TirRef) -> WhileLoopView<'_> {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, TirTag::WhileLoop));
        let range = match inst.data {
            TirData::Extra(rng) => rng,
            _ => unreachable!("WhileLoop must carry TirData::Extra"),
        };
        while_loop_extra::decode(&self.extra[range.as_range()])
    }

    /// Borrow the statement refs in a `WhileLoop` body from the extra arena.
    pub fn while_loop_body_view(&self, r: TirRef) -> ExtraList<'_, TirRef> {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, TirTag::WhileLoop));
        let range = match inst.data {
            TirData::Extra(rng) => rng,
            _ => unreachable!("WhileLoop must carry TirData::Extra"),
        };
        while_loop_extra::decode(&self.extra[range.as_range()]).body
    }

    pub fn for_range_view(&self, r: TirRef) -> ForRangeView<'_> {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, TirTag::ForRange));
        let range = match inst.data {
            TirData::Extra(rng) => rng,
            _ => unreachable!("ForRange must carry TirData::Extra"),
        };
        for_range_extra::decode(&self.extra[range.as_range()])
    }

    /// Borrow the statement refs in a `ForRange` body from the extra arena.
    pub fn for_range_body_view(&self, r: TirRef) -> ExtraList<'_, TirRef> {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, TirTag::ForRange));
        let range = match inst.data {
            TirData::Extra(rng) => rng,
            _ => unreachable!("ForRange must carry TirData::Extra"),
        };
        for_range_extra::decode(&self.extra[range.as_range()]).body
    }

    pub fn struct_lit_view(&self, r: TirRef) -> StructLitView<'_> {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, TirTag::StructLit));
        let range = match inst.data {
            TirData::Extra(rng) => rng,
            _ => unreachable!("StructLit must carry TirData::Extra"),
        };
        struct_lit_extra::decode(&self.extra[range.as_range()])
    }

    pub fn enum_lit_view(&self, r: TirRef) -> EnumLitView<'_> {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, TirTag::EnumLit));
        let range = match inst.data {
            TirData::Extra(rng) => rng,
            _ => unreachable!("EnumLit must carry TirData::Extra"),
        };
        enum_lit_extra::decode(&self.extra[range.as_range()])
    }
}
