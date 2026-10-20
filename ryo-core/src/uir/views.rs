pub use super::wires::assign_or_decl_extra::AssignOrDeclView;
pub use super::wires::call_extra::CallView;
pub use super::wires::compound_assign_extra::CompoundAssignView;
pub use super::wires::compound_field_assign_extra::CompoundFieldAssignView;
pub use super::wires::field_assign_extra::FieldAssignView;
pub use super::wires::for_range_extra::ForRangeView;
pub use super::wires::if_stmt_extra::{ElifBranches, ElifView, IfStmtView};
pub use super::wires::method_call_extra::MethodCallView;
pub use super::wires::struct_lit_extra::StructLitView;
pub use super::wires::var_decl_extra::VarDeclView;
pub use super::wires::while_loop_extra::WhileLoopView;
use super::{
    InstData, InstRef, InstTag, Uir, assign_or_decl_extra, call_extra, compound_assign_extra,
    compound_field_assign_extra, enum_lit_extra, field_assign_extra, for_range_extra,
    if_stmt_extra, method_call_extra, struct_lit_extra, var_decl_extra, while_loop_extra,
};

// ---------- Read-side helpers ----------

/// Decoded view of an [`InstTag::EnumLit`] payload (M11) — the wire is
/// shared with TIR; see [`crate::extra::enum_lit_extra`].
pub type EnumLitView<'a> = crate::extra::enum_lit_extra::EnumLitView<'a, InstRef>;

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
        enum_lit_extra::decode(&self.extra[range.as_range()])
    }

    pub fn if_stmt_view(&self, r: InstRef) -> IfStmtView<'_> {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, InstTag::IfStmt));
        let range = match inst.data {
            InstData::Extra(rng) => rng,
            _ => unreachable!("IfStmt must carry InstData::Extra"),
        };
        if_stmt_extra::decode(&self.extra[range.as_range()])
    }
}
