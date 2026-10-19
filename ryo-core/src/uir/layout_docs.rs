// ---------- Variable-payload encoding ----------

/// Layout in `extra` for [`InstTag::Call`]:
///
/// ```text
///   [0]  name:  StringId
///   [1]  argc:  u32
///   [2..2+argc] args: InstRef.raw()
/// ```
pub mod call_extra {
    pub const NAME: usize = 0;
    pub const ARGC: usize = 1;
    pub const ARGS: usize = 2;
}

/// Layout in `extra` for [`InstTag::VarDecl`]:
///
/// ```text
///   [0]  name:  StringId
///   [1]  flags: u32  (bit 0 = mutable)
///   [2]  ty:    u32  (TypeId, or `TY_NONE_SENTINEL` if no annotation)
///   [3]  init:  InstRef.raw()
/// ```
///
/// `TY_NONE_SENTINEL` is `u32::MAX`, which is outside any plausible
/// `TypeId` range. Sema replaces it with the inferred type (and emits
/// a TIR instruction whose `ty` slot is real); codegen never sees
/// `TY_NONE_SENTINEL` after Phase 4 lands.
pub mod var_decl_extra {
    pub const NAME: usize = 0;
    pub const FLAGS: usize = 1;
    pub const TY: usize = 2;
    pub const INIT: usize = 3;
    pub const LEN: usize = 4;

    pub const FLAG_MUTABLE: u32 = 1 << 0;
    pub const TY_NONE_SENTINEL: u32 = u32::MAX;
}

/// Layout in `extra` for [`InstTag::AssignOrDecl`]:
///
/// ```text
///   [0]  name:  StringId
///   [1]  value: InstRef.raw()
/// ```
pub mod assign_or_decl_extra {
    pub const NAME: usize = 0;
    pub const VALUE: usize = 1;
    pub const LEN: usize = 2;
}

/// Layout in `extra` for [`InstTag::CompoundAssign`]:
///
/// ```text
///   [0]  name:  StringId
///   [1]  op:    u32 (CompoundOp discriminant)
///   [2]  value: InstRef.raw()
/// ```
pub mod compound_assign_extra {
    pub const NAME: usize = 0;
    pub const OP: usize = 1;
    pub const VALUE: usize = 2;
    pub const LEN: usize = 3;
}

/// Layout in `extra` for [`InstTag::MethodCall`]:
///
/// ```text
///   [0]  receiver: InstRef.raw()
///   [1]  name:     StringId.raw()
///   [2]  argc:     u32
///   [3..3+argc] args: InstRef.raw()
/// ```
pub mod method_call_extra {
    pub const RECEIVER: usize = 0;
    pub const NAME: usize = 1;
    pub const ARGC: usize = 2;
    pub const ARGS: usize = 3;
}

/// Layout in `extra` for [`InstTag::WhileLoop`]:
///
/// ```text
///   [0]       cond:       InstRef.raw()
///   [1]       body_count: u32
///   [2..2+n]  body stmts: InstRef.raw() each
/// ```
pub mod while_loop_extra {
    pub const COND: usize = 0;
    pub const BODY_COUNT: usize = 1;
    pub const BODY_START: usize = 2;
}

/// Layout in `extra` for [`InstTag::ForRange`]:
///
/// ```text
///   [0]       var_name:   StringId.raw()
///   [1]       start:      InstRef.raw()
///   [2]       end:        InstRef.raw()
///   [3]       body_count: u32
///   [4..4+n]  body stmts: InstRef.raw() each
/// ```
pub mod for_range_extra {
    pub const VAR_NAME: usize = 0;
    pub const START: usize = 1;
    pub const END: usize = 2;
    pub const BODY_COUNT: usize = 3;
    pub const BODY_START: usize = 4;
}

/// Layout in `extra` for [`InstTag::StructLit`]:
///
/// ```text
///   [0]  name:     StringId (or `UIR_ANON_STRUCT_NAME` for an
///                  anonymous literal — there is no declared name)
///   [1]  n_fields: u32
///   [2..2+2*n]     per field: [field_name: StringId.raw(), value: InstRef.raw()]
/// ```
pub mod struct_lit_extra {
    pub const NAME: usize = 0;
    pub const NFIELDS: usize = 1;
    pub const FIELDS: usize = 2;
    /// Anonymous-literal marker for the `NAME` slot (M10), mirroring
    /// [`var_decl_extra::TY_NONE_SENTINEL`]: `u32::MAX` is outside any
    /// plausible `StringId` range, so it can never collide with a real
    /// (or user-interned) name. Sema infers the structural type from
    /// the field values instead of looking the name up.
    pub const UIR_ANON_STRUCT_NAME: u32 = u32::MAX;
}

/// Layout in `extra` for [`InstTag::FieldAssign`]:
///
/// ```text
///   [0]  target: InstRef.raw() (FieldAccess chain)
///   [1]  value:  InstRef.raw()
/// ```
pub mod field_assign_extra {
    pub const TARGET: usize = 0;
    pub const VALUE: usize = 1;
    pub const LEN: usize = 2;
}

/// Layout in `extra` for [`InstTag::CompoundFieldAssign`]:
///
/// ```text
///   [0]  target: InstRef.raw() (FieldAccess chain)
///   [1]  op:     u32 (CompoundOp discriminant)
///   [2]  value:  InstRef.raw()
/// ```
pub mod compound_field_assign_extra {
    pub const TARGET: usize = 0;
    pub const OP: usize = 1;
    pub const VALUE: usize = 2;
    pub const LEN: usize = 3;
}
