// ---------- Variable-payload encoding ----------

/// Layout in `extra` for [`TirTag::Call`]:
///
/// ```text
///   [0]         name:  StringId
///   [1]         argc:  u32
///   [2..2+argc] args:  TirRef.raw()
///   [2+argc..2+2*argc] modes: ParamMode (one u32 per arg)
/// ```
///
/// `modes` is stamped by sema from each callee parameter's `mode`
/// field (user functions) or all-`Borrow` (builtins). The ownership
/// pass consumes these modes directly from the `CallView` payload to
/// determine parameter move/borrow conventions.
pub mod call_extra {
    pub const NAME: usize = 0;
    pub const ARGC: usize = 1;
    pub const ARGS: usize = 2;
    // MODES occupies ARGS+argc .. ARGS+2*argc (one u32 per arg).
}

/// Layout in `extra` for [`TirTag::VarDecl`]:
///
/// ```text
///   [0]  name:    StringId
///   [1]  flags:   u32  (bit 0 = mutable)
///   [2]  init:    TirRef.raw()
/// ```
///
/// Unlike UIR's `VarDecl`, there is no `TY_NONE_SENTINEL`: the
/// resolved variable type lives in the `TypedInst.ty` slot, never
/// `Option`-shaped at this layer.
pub mod var_decl_extra {
    pub const NAME: usize = 0;
    pub const FLAGS: usize = 1;
    pub const INIT: usize = 2;
    pub const LEN: usize = 3;

    pub const FLAG_MUTABLE: u32 = 1 << 0;
}

/// Layout in `extra` for [`TirTag::Assign`]:
///
/// ```text
///   [0]  name:  StringId
///   [1]  value: TirRef.raw()
/// ```
pub mod assign_extra {
    pub const NAME: usize = 0;
    pub const VALUE: usize = 1;
    pub const LEN: usize = 2;
}

/// Layout in `extra` for [`TirTag::CompoundAssign`]:
///
/// ```text
///   [0]  name:  StringId
///   [1]  op:    u32 (CompoundOp discriminant)
///   [2]  value: TirRef.raw()
/// ```
pub mod compound_assign_extra {
    pub const NAME: usize = 0;
    pub const OP: usize = 1;
    pub const VALUE: usize = 2;
    pub const LEN: usize = 3;
}

/// Layout in `extra` for [`TirTag::WhileLoop`]:
///
/// ```text
///   [0]       cond:       TirRef.raw()
///   [1]       body_count: u32
///   [2..2+n]  body stmts: TirRef.raw() each
/// ```
pub mod while_loop_extra {
    pub const COND: usize = 0;
    pub const BODY_COUNT: usize = 1;
    pub const BODY_START: usize = 2;
}

/// Layout in `extra` for [`TirTag::ForRange`]:
///
/// ```text
///   [0]       var_name:   StringId
///   [1]       start:      TirRef.raw()
///   [2]       end:        TirRef.raw()
///   [3]       body_count: u32
///   [4..4+n]  body stmts: TirRef.raw() each
/// ```
pub mod for_range_extra {
    pub const VAR_NAME: usize = 0;
    pub const START: usize = 1;
    pub const END: usize = 2;
    pub const BODY_COUNT: usize = 3;
    pub const BODY_START: usize = 4;
}

/// Layout in `extra` for [`TirTag::StructLit`] (M9):
///
/// ```text
///   [0]         struct_ty: TypeId raw
///   [1]         n_fields:  u32
///   [2..2+2*n]  (field_index: u32, value: TirRef.raw()) pairs,
///               sorted by field_index (canonical declaration order;
///               literal field order is irrelevant after sema)
/// ```
pub mod struct_lit_extra {
    pub const TY: usize = 0;
    pub const N_FIELDS: usize = 1;
    pub const FIELDS: usize = 2;
}

/// Layout in `extra` for [`TirTag::FieldAssign`] (M9):
///
/// ```text
///   [0]  target: TirRef.raw() (FieldAccess chain)
///   [1]  value:  TirRef.raw()
/// ```
pub mod field_assign_extra {
    pub const TARGET: usize = 0;
    pub const VALUE: usize = 1;
    pub const LEN: usize = 2;
}

/// Layout in `extra` for [`TirTag::CompoundFieldAssign`] (M9):
///
/// ```text
///   [0]  target: TirRef.raw() (FieldAccess chain)
///   [1]  op:     u32 (CompoundOp discriminant)
///   [2]  value:  TirRef.raw()
/// ```
pub mod compound_field_assign_extra {
    pub const TARGET: usize = 0;
    pub const OP: usize = 1;
    pub const VALUE: usize = 2;
    pub const LEN: usize = 3;
}
