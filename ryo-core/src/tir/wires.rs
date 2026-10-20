// ---------- Variable-payload encoding ----------

/// Current `extra.len()` as a checked `u32` — the same overflow
/// rejection `TirBuilder::extra_offset` performs, re-homed with the
/// wire codecs so encoders live outside the builder. `pub(super)` so
/// sibling wire homes (`destructure`) share the checked conversion.
pub(super) fn extra_offset(extra: &[u32]) -> u32 {
    u32::try_from(extra.len()).expect("TIR extra arena exceeded u32::MAX words")
}

/// Convert a length-shaped `usize` (e.g. `args.len()`) to `u32`,
/// panicking on overflow like `TirBuilder::len_u32`.
pub(super) fn len_u32(len: usize) -> u32 {
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
    use super::{extra_offset, len_u32};
    use crate::extra::{ExtraList, ExtraRange, ExtraValue};
    use crate::tir::{ParamMode, TirRef};
    use crate::types::StringId;

    pub const NAME: usize = 0;
    pub const ARGC: usize = 1;
    pub const ARGS: usize = 2;
    // MODES occupies ARGS+argc .. ARGS+2*argc (one u32 per arg).

    pub struct CallView<'a> {
        pub name: StringId,
        pub args: ExtraList<'a, TirRef>,
        /// Per-argument call convention, parallel to `args`. Stamped by
        /// sema from each callee parameter's `mode` field.
        pub modes: ExtraList<'a, ParamMode>,
    }

    /// Encode a `Call` payload into `extra`, returning its range.
    pub fn encode(
        extra: &mut Vec<u32>,
        name: StringId,
        args: &[TirRef],
        modes: &[ParamMode],
    ) -> ExtraRange {
        assert_eq!(
            modes.len(),
            args.len(),
            "TirBuilder::call: one ParamMode per arg"
        );
        let offset = extra_offset(extra);
        extra.push(name.raw());
        extra.push(len_u32(args.len()));
        extra.extend(args.iter().map(|a| a.to_extra()));
        extra.extend(modes.iter().map(|m| m.to_extra()));
        ExtraRange {
            offset,
            len: len_u32(ARGS + 2 * args.len()),
        }
    }

    /// Decode a `Call` payload from its words.
    pub fn decode<'a>(words: &'a [u32]) -> CallView<'a> {
        let name = StringId::from_raw(words[NAME]);
        let argc = words[ARGC] as usize;
        let args = ExtraList::new(&words[ARGS..ARGS + argc]);
        let modes = ExtraList::new(&words[ARGS + argc..ARGS + 2 * argc]);
        CallView { name, args, modes }
    }
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
    use super::{extra_offset, len_u32};
    use crate::extra::{ExtraList, ExtraRange, ExtraValue};
    use crate::tir::TirRef;

    pub const COND: usize = 0;
    pub const BODY_COUNT: usize = 1;
    pub const BODY_START: usize = 2;

    pub struct WhileLoopView<'a> {
        pub cond: TirRef,
        pub body: ExtraList<'a, TirRef>,
    }

    /// Encode a `WhileLoop` payload into `extra`, returning its range.
    pub fn encode(extra: &mut Vec<u32>, cond: TirRef, body: &[TirRef]) -> ExtraRange {
        let offset = extra_offset(extra);
        extra.push(cond.raw());
        extra.push(len_u32(body.len()));
        extra.extend(body.iter().map(|s| s.to_extra()));
        ExtraRange {
            offset,
            len: len_u32(BODY_START + body.len()),
        }
    }

    /// Decode a `WhileLoop` payload from its words.
    pub fn decode<'a>(words: &'a [u32]) -> WhileLoopView<'a> {
        WhileLoopView {
            cond: TirRef::from_raw(words[COND]),
            body: ExtraList::new(&words[BODY_START..]),
        }
    }
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
    use super::{extra_offset, len_u32};
    use crate::extra::{ExtraList, ExtraRange, ExtraValue};
    use crate::tir::TirRef;
    use crate::types::StringId;

    pub const VAR_NAME: usize = 0;
    pub const START: usize = 1;
    pub const END: usize = 2;
    pub const BODY_COUNT: usize = 3;
    pub const BODY_START: usize = 4;

    pub struct ForRangeView<'a> {
        pub var_name: StringId,
        pub start: TirRef,
        pub end: TirRef,
        pub body: ExtraList<'a, TirRef>,
    }

    /// Encode a `ForRange` payload into `extra`, returning its range.
    pub fn encode(
        extra: &mut Vec<u32>,
        var_name: StringId,
        start: TirRef,
        end: TirRef,
        body: &[TirRef],
    ) -> ExtraRange {
        let offset = extra_offset(extra);
        extra.push(var_name.raw());
        extra.push(start.raw());
        extra.push(end.raw());
        extra.push(len_u32(body.len()));
        extra.extend(body.iter().map(|s| s.to_extra()));
        ExtraRange {
            offset,
            len: len_u32(BODY_START + body.len()),
        }
    }

    /// Decode a `ForRange` payload from its words.
    pub fn decode<'a>(words: &'a [u32]) -> ForRangeView<'a> {
        ForRangeView {
            var_name: StringId::from_raw(words[VAR_NAME]),
            start: TirRef::from_raw(words[START]),
            end: TirRef::from_raw(words[END]),
            body: ExtraList::new(&words[BODY_START..]),
        }
    }
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
    use super::{extra_offset, len_u32};
    use crate::extra::ExtraRange;
    use crate::tir::TirRef;
    use crate::types::TypeId;

    pub const TY: usize = 0;
    pub const N_FIELDS: usize = 1;
    pub const FIELDS: usize = 2;

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

    /// Encode a `StructLit` payload into `extra`, returning its range.
    /// `fields` must be sorted by field index — sema sorts before
    /// calling.
    pub fn encode(extra: &mut Vec<u32>, ty: TypeId, fields: &[(u32, TirRef)]) -> ExtraRange {
        debug_assert!(
            fields.windows(2).all(|w| w[0].0 < w[1].0),
            "TirBuilder::struct_lit: fields must be sorted by field index"
        );
        let offset = extra_offset(extra);
        extra.push(ty.raw());
        extra.push(len_u32(fields.len()));
        for &(idx, v) in fields {
            extra.push(idx);
            extra.push(v.raw());
        }
        ExtraRange {
            offset,
            len: len_u32(FIELDS + 2 * fields.len()),
        }
    }

    /// Decode a `StructLit` payload from its words.
    pub fn decode(words: &[u32]) -> StructLitView<'_> {
        let ty = TypeId::from_raw(words[TY]);
        let n = words[N_FIELDS] as usize;
        StructLitView {
            ty,
            fields: &words[FIELDS..FIELDS + 2 * n],
        }
    }
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

/// Layout in `extra` for [`TirTag::IfStmt`]: the condition, a counted
/// then-list, an elif count, each elif condition followed by a counted
/// body, an else marker (`0` = absent, `1` = present), and the counted
/// else list when present.
pub mod if_stmt_extra {
    use super::{extra_offset, len_u32};
    use crate::extra::{ExtraList, ExtraRange};
    use crate::tir::TirRef;
    use crate::tir::{read_ref_list_view, skip_ref_list};

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

    /// Encode an `IfStmt` payload into `extra`, returning its range.
    pub fn encode(
        extra: &mut Vec<u32>,
        cond: TirRef,
        then_stmts: &[TirRef],
        elif_branches: &[(TirRef, Vec<TirRef>)],
        else_stmts: Option<&[TirRef]>,
    ) -> ExtraRange {
        let elif_words: usize = elif_branches.iter().map(|(_, body)| 2 + body.len()).sum();
        let total =
            1 + 1 + then_stmts.len() + 1 + elif_words + 1 + else_stmts.map_or(0, |s| 1 + s.len());
        extra.reserve(total);

        let offset = extra_offset(extra);
        extra.push(cond.raw());
        push_ref_list(extra, then_stmts);
        extra.push(len_u32(elif_branches.len()));
        for (elif_cond, elif_body) in elif_branches {
            extra.push(elif_cond.raw());
            push_ref_list(extra, elif_body);
        }
        match else_stmts {
            Some(stmts) => {
                extra.push(1);
                push_ref_list(extra, stmts);
            }
            None => {
                extra.push(0);
            }
        }
        ExtraRange {
            offset,
            len: len_u32(extra.len() - offset as usize),
        }
    }

    /// Decode an `IfStmt` payload from its words. The elif region is
    /// pre-scanned to locate the else marker; per-branch refs decode
    /// lazily through the borrowed lists.
    pub fn decode(words: &[u32]) -> TirIfStmtView<'_> {
        let mut pos = 1;
        let cond = TirRef::from_raw(words[0]);
        let then_stmts = read_ref_list_view(words, &mut pos);
        let elif_count = words[pos] as usize;
        pos += 1;
        let elif_start = pos;
        for _ in 0..elif_count {
            pos += 1;
            skip_ref_list(words, &mut pos);
        }
        let elif_branches = TirElifList {
            words: &words[elif_start..pos],
            count: elif_count,
        };
        let has_else = words[pos] != 0;
        pos += 1;
        let else_stmts = has_else.then(|| read_ref_list_view(words, &mut pos));
        TirIfStmtView {
            cond,
            then_stmts,
            elif_branches,
            else_stmts,
        }
    }

    fn push_ref_list(extra: &mut Vec<u32>, refs: &[TirRef]) {
        extra.push(len_u32(refs.len()));
        for r in refs {
            extra.push(r.raw());
        }
    }
}
