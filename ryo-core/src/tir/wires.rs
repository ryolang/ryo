// ---------- Variable-payload encoding ----------

/// Current `extra.len()` as a checked `u32` — the same overflow
/// rejection `TirBuilder::extra_offset` performs, re-homed with the
/// wire codecs so encoders live outside the builder.
fn extra_offset(extra: &[u32]) -> u32 {
    u32::try_from(extra.len()).expect("TIR extra arena exceeded u32::MAX words")
}

/// Convert a length-shaped `usize` (e.g. `args.len()`) to `u32`,
/// panicking on overflow like `TirBuilder::len_u32`.
fn len_u32(len: usize) -> u32 {
    u32::try_from(len).expect("TIR list length exceeded u32::MAX")
}

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
    use super::{extra_offset, len_u32};
    use crate::extra::ExtraRange;
    use crate::tir::TirRef;
    use crate::types::StringId;

    pub const NAME: usize = 0;
    pub const FLAGS: usize = 1;
    pub const INIT: usize = 2;
    pub const LEN: usize = 3;

    pub const FLAG_MUTABLE: u32 = 1 << 0;

    /// Decoded view of a [`TirTag::VarDecl`] payload.
    pub struct VarDeclView {
        pub name: StringId,
        pub mutable: bool,
        pub initializer: TirRef,
    }

    /// Encode a `VarDecl` header into `extra`, returning its range.
    /// The resolved variable type travels in the `TypedInst.ty` slot,
    /// never on the wire.
    pub fn encode(extra: &mut Vec<u32>, name: StringId, mutable: bool, init: TirRef) -> ExtraRange {
        let offset = extra_offset(extra);
        extra.push(name.raw());
        extra.push(if mutable { FLAG_MUTABLE } else { 0 });
        extra.push(init.raw());
        ExtraRange {
            offset,
            len: len_u32(LEN),
        }
    }

    /// Decode a `VarDecl` header from its payload words.
    pub fn decode(words: &[u32]) -> VarDeclView {
        let name = StringId::from_raw(words[NAME]);
        let mutable = words[FLAGS] & FLAG_MUTABLE != 0;
        let initializer = TirRef::from_raw(words[INIT]);
        VarDeclView {
            name,
            mutable,
            initializer,
        }
    }
}

/// Layout in `extra` for [`TirTag::Assign`]:
///
/// ```text
///   [0]  name:  StringId
///   [1]  value: TirRef.raw()
/// ```
pub mod assign_extra {
    use super::{extra_offset, len_u32};
    use crate::extra::ExtraRange;
    use crate::tir::TirRef;
    use crate::types::StringId;

    pub const NAME: usize = 0;
    pub const VALUE: usize = 1;
    pub const LEN: usize = 2;

    pub struct AssignView {
        pub name: StringId,
        pub value: TirRef,
    }

    /// Encode an `Assign` payload into `extra`, returning its range.
    /// The assigned type travels in the `TypedInst.ty` slot, not here.
    pub fn encode(extra: &mut Vec<u32>, name: StringId, value: TirRef) -> ExtraRange {
        let offset = extra_offset(extra);
        extra.push(name.raw());
        extra.push(value.raw());
        ExtraRange {
            offset,
            len: len_u32(LEN),
        }
    }

    /// Decode an `Assign` payload from its words.
    pub fn decode(words: &[u32]) -> AssignView {
        AssignView {
            name: StringId::from_raw(words[NAME]),
            value: TirRef::from_raw(words[VALUE]),
        }
    }
}

/// Layout in `extra` for [`TirTag::CompoundAssign`]:
///
/// ```text
///   [0]  name:  StringId
///   [1]  op:    u32 (CompoundOp discriminant)
///   [2]  value: TirRef.raw()
/// ```
pub mod compound_assign_extra {
    use super::{extra_offset, len_u32};
    use crate::ast::CompoundOp;
    use crate::extra::ExtraRange;
    use crate::tir::TirRef;
    use crate::types::StringId;

    pub const NAME: usize = 0;
    pub const OP: usize = 1;
    pub const VALUE: usize = 2;
    pub const LEN: usize = 3;

    pub struct CompoundAssignView {
        pub name: StringId,
        pub op: CompoundOp,
        pub value: TirRef,
    }

    /// Encode a `CompoundAssign` payload into `extra`, returning its
    /// range. The assigned type travels in the `TypedInst.ty` slot.
    pub fn encode(
        extra: &mut Vec<u32>,
        name: StringId,
        op: CompoundOp,
        value: TirRef,
    ) -> ExtraRange {
        let offset = extra_offset(extra);
        extra.push(name.raw());
        extra.push(op as u32);
        extra.push(value.raw());
        ExtraRange {
            offset,
            len: len_u32(LEN),
        }
    }

    /// Decode a `CompoundAssign` payload from its words.
    pub fn decode(words: &[u32]) -> CompoundAssignView {
        CompoundAssignView {
            name: StringId::from_raw(words[NAME]),
            op: CompoundOp::from_raw(words[OP]),
            value: TirRef::from_raw(words[VALUE]),
        }
    }
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
    use super::{extra_offset, len_u32};
    use crate::extra::ExtraRange;
    use crate::tir::TirRef;

    pub const TARGET: usize = 0;
    pub const VALUE: usize = 1;
    pub const LEN: usize = 2;

    /// Decoded view of a [`TirTag::FieldAssign`] payload (M9).
    pub struct FieldAssignView {
        pub target: TirRef,
        pub value: TirRef,
    }

    /// Encode a `FieldAssign` payload into `extra`, returning its range.
    /// The field type travels in the `TypedInst.ty` slot.
    pub fn encode(extra: &mut Vec<u32>, target: TirRef, value: TirRef) -> ExtraRange {
        let offset = extra_offset(extra);
        extra.push(target.raw());
        extra.push(value.raw());
        ExtraRange {
            offset,
            len: len_u32(LEN),
        }
    }

    /// Decode a `FieldAssign` payload from its words.
    pub fn decode(words: &[u32]) -> FieldAssignView {
        FieldAssignView {
            target: TirRef::from_raw(words[TARGET]),
            value: TirRef::from_raw(words[VALUE]),
        }
    }
}

/// Layout in `extra` for [`TirTag::CompoundFieldAssign`] (M9):
///
/// ```text
///   [0]  target: TirRef.raw() (FieldAccess chain)
///   [1]  op:     u32 (CompoundOp discriminant)
///   [2]  value:  TirRef.raw()
/// ```
pub mod compound_field_assign_extra {
    use super::{extra_offset, len_u32};
    use crate::ast::CompoundOp;
    use crate::extra::ExtraRange;
    use crate::tir::TirRef;

    pub const TARGET: usize = 0;
    pub const OP: usize = 1;
    pub const VALUE: usize = 2;
    pub const LEN: usize = 3;

    /// Decoded view of a [`TirTag::CompoundFieldAssign`] payload (M9).
    pub struct CompoundFieldAssignView {
        pub target: TirRef,
        pub op: CompoundOp,
        pub value: TirRef,
    }

    /// Encode a `CompoundFieldAssign` payload into `extra`, returning
    /// its range. The field type travels in the `TypedInst.ty` slot.
    pub fn encode(
        extra: &mut Vec<u32>,
        target: TirRef,
        op: CompoundOp,
        value: TirRef,
    ) -> ExtraRange {
        let offset = extra_offset(extra);
        extra.push(target.raw());
        extra.push(op as u32);
        extra.push(value.raw());
        ExtraRange {
            offset,
            len: len_u32(LEN),
        }
    }

    /// Decode a `CompoundFieldAssign` payload from its words.
    pub fn decode(words: &[u32]) -> CompoundFieldAssignView {
        CompoundFieldAssignView {
            target: TirRef::from_raw(words[TARGET]),
            op: CompoundOp::from_raw(words[OP]),
            value: TirRef::from_raw(words[VALUE]),
        }
    }
}
