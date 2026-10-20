// ---------- Variable-payload encoding ----------

/// Current `extra.len()` as a checked `u32` — the same overflow
/// rejection `UirBuilder::extra_offset` performs, re-homed with the
/// wire codecs so encoders live outside the builder. `pub(super)` so
/// sibling wire homes (`destructure`) share the checked conversion.
pub(super) fn extra_offset(extra: &[u32]) -> u32 {
    u32::try_from(extra.len()).expect("UIR extra arena exceeded u32::MAX words")
}

/// Convert a length-shaped `usize` (e.g. `args.len()`) to `u32`,
/// panicking on overflow like `UirBuilder::len_u32`.
pub(super) fn len_u32(len: usize) -> u32 {
    u32::try_from(len).expect("UIR list length exceeded u32::MAX")
}

/// Layout in `extra` for [`crate::uir::InstTag::Call`]:
///
/// ```text
///   [0]  name:  StringId
///   [1]  argc:  u32
///   [2..2+argc] args: InstRef.raw()
/// ```
pub mod call_extra {
    use super::{extra_offset, len_u32};
    use crate::extra::{ExtraList, ExtraRange, ExtraValue};
    use crate::types::StringId;
    use crate::uir::InstRef;

    /// Decoded view of an [`crate::uir::InstTag::Call`] payload.
    pub struct CallView<'a> {
        pub name: StringId,
        pub args: ExtraList<'a, InstRef>,
    }

    /// Encode a `Call` payload into `extra`, returning its range.
    pub fn encode(extra: &mut Vec<u32>, name: StringId, args: &[InstRef]) -> ExtraRange {
        let offset = extra_offset(extra);
        extra.push(name.raw());
        extra.push(len_u32(args.len()));
        extra.extend(args.iter().map(|a| a.to_extra()));
        ExtraRange {
            offset,
            len: len_u32(2 + args.len()),
        }
    }

    /// Decode a `Call` payload from its words.
    pub fn decode<'a>(words: &'a [u32]) -> CallView<'a> {
        let name = StringId::from_raw(words[0]);
        let argc = words[1] as usize;
        let args = ExtraList::new(&words[2..2 + argc]);
        CallView { name, args }
    }
}

/// Layout in `extra` for [`crate::uir::InstTag::VarDecl`]:
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
/// a TIR instruction whose `ty` slot is real), so codegen never sees
/// `TY_NONE_SENTINEL`.
pub mod var_decl_extra {
    use super::{extra_offset, len_u32};
    use crate::extra::ExtraRange;
    use crate::types::{StringId, TypeId};
    use crate::uir::InstRef;

    pub const FLAG_MUTABLE: u32 = 1 << 0;
    pub const TY_NONE_SENTINEL: u32 = u32::MAX;

    /// Decoded view of an [`crate::uir::InstTag::VarDecl`] payload. `ty` is `None`
    /// when the source had no annotation.
    pub struct VarDeclView {
        pub name: StringId,
        pub mutable: bool,
        pub ty: Option<TypeId>,
        pub initializer: InstRef,
    }

    /// Encode a `VarDecl` header into `extra`, returning its range.
    pub fn encode(
        extra: &mut Vec<u32>,
        name: StringId,
        mutable: bool,
        ty: Option<TypeId>,
        init: InstRef,
    ) -> ExtraRange {
        let offset = extra_offset(extra);
        extra.push(name.raw());
        extra.push(if mutable { FLAG_MUTABLE } else { 0 });
        extra.push(ty.map_or(TY_NONE_SENTINEL, |t| t.raw()));
        extra.push(init.raw());
        ExtraRange {
            offset,
            len: len_u32(4),
        }
    }

    /// Decode a `VarDecl` header from its payload words.
    pub fn decode(words: &[u32]) -> VarDeclView {
        let name = StringId::from_raw(words[0]);
        let mutable = words[1] & FLAG_MUTABLE != 0;
        let ty_raw = words[2];
        let ty = if ty_raw == TY_NONE_SENTINEL {
            None
        } else {
            Some(TypeId::from_raw(ty_raw))
        };
        let initializer = InstRef::from_raw(words[3]);
        VarDeclView {
            name,
            mutable,
            ty,
            initializer,
        }
    }
}

/// Layout in `extra` for [`crate::uir::InstTag::AssignOrDecl`]:
///
/// ```text
///   [0]  name:  StringId
///   [1]  value: InstRef.raw()
/// ```
pub mod assign_or_decl_extra {
    use super::{extra_offset, len_u32};
    use crate::extra::ExtraRange;
    use crate::types::StringId;
    use crate::uir::InstRef;

    pub struct AssignOrDeclView {
        pub name: StringId,
        pub value: InstRef,
    }

    /// Encode an `AssignOrDecl` payload into `extra`, returning its range.
    pub fn encode(extra: &mut Vec<u32>, name: StringId, value: InstRef) -> ExtraRange {
        let offset = extra_offset(extra);
        extra.push(name.raw());
        extra.push(value.raw());
        ExtraRange {
            offset,
            len: len_u32(2),
        }
    }

    /// Decode an `AssignOrDecl` payload from its words.
    pub fn decode(words: &[u32]) -> AssignOrDeclView {
        AssignOrDeclView {
            name: StringId::from_raw(words[0]),
            value: InstRef::from_raw(words[1]),
        }
    }
}

/// Layout in `extra` for [`crate::uir::InstTag::CompoundAssign`]:
///
/// ```text
///   [0]  name:  StringId
///   [1]  op:    u32 (CompoundOp discriminant)
///   [2]  value: InstRef.raw()
/// ```
pub mod compound_assign_extra {
    use super::{extra_offset, len_u32};
    use crate::ast::CompoundOp;
    use crate::extra::ExtraRange;
    use crate::types::StringId;
    use crate::uir::InstRef;

    pub struct CompoundAssignView {
        pub name: StringId,
        pub op: CompoundOp,
        pub value: InstRef,
    }

    /// Encode a `CompoundAssign` payload into `extra`, returning its range.
    pub fn encode(
        extra: &mut Vec<u32>,
        name: StringId,
        op: CompoundOp,
        value: InstRef,
    ) -> ExtraRange {
        let offset = extra_offset(extra);
        extra.push(name.raw());
        extra.push(op as u32);
        extra.push(value.raw());
        ExtraRange {
            offset,
            len: len_u32(3),
        }
    }

    /// Decode a `CompoundAssign` payload from its words.
    pub fn decode(words: &[u32]) -> CompoundAssignView {
        CompoundAssignView {
            name: StringId::from_raw(words[0]),
            op: CompoundOp::from_raw(words[1]),
            value: InstRef::from_raw(words[2]),
        }
    }
}

/// Layout in `extra` for [`crate::uir::InstTag::MethodCall`]:
///
/// ```text
///   [0]  receiver: InstRef.raw()
///   [1]  name:     StringId.raw()
///   [2]  argc:     u32
///   [3..3+argc] args: InstRef.raw()
/// ```
pub mod method_call_extra {
    use super::{extra_offset, len_u32};
    use crate::extra::{ExtraList, ExtraRange, ExtraValue};
    use crate::types::StringId;
    use crate::uir::InstRef;

    /// Decoded view of an [`crate::uir::InstTag::MethodCall`] payload.
    pub struct MethodCallView<'a> {
        pub receiver: InstRef,
        pub name: StringId,
        pub args: ExtraList<'a, InstRef>,
    }

    /// Encode a `MethodCall` payload into `extra`, returning its range.
    pub fn encode(
        extra: &mut Vec<u32>,
        receiver: InstRef,
        name: StringId,
        args: &[InstRef],
    ) -> ExtraRange {
        let offset = extra_offset(extra);
        extra.push(receiver.raw());
        extra.push(name.raw());
        extra.push(len_u32(args.len()));
        extra.extend(args.iter().map(|a| a.to_extra()));
        ExtraRange {
            offset,
            len: len_u32(3 + args.len()),
        }
    }

    /// Decode a `MethodCall` payload from its words.
    pub fn decode<'a>(words: &'a [u32]) -> MethodCallView<'a> {
        let receiver = InstRef::from_raw(words[0]);
        let name = StringId::from_raw(words[1]);
        let argc = words[2] as usize;
        let args = ExtraList::new(&words[3..3 + argc]);
        MethodCallView {
            receiver,
            name,
            args,
        }
    }
}

/// Layout in `extra` for [`crate::uir::InstTag::WhileLoop`]:
///
/// ```text
///   [0]       cond:       InstRef.raw()
///   [1]       body_count: u32
///   [2..2+n]  body stmts: InstRef.raw() each
/// ```
pub mod while_loop_extra {
    use super::{extra_offset, len_u32};
    use crate::extra::{ExtraList, ExtraRange, ExtraValue};
    use crate::uir::InstRef;

    pub struct WhileLoopView<'a> {
        pub cond: InstRef,
        pub body: ExtraList<'a, InstRef>,
    }

    /// Encode a `WhileLoop` payload into `extra`, returning its range.
    pub fn encode(extra: &mut Vec<u32>, cond: InstRef, body: &[InstRef]) -> ExtraRange {
        let offset = extra_offset(extra);
        extra.push(cond.raw());
        extra.push(len_u32(body.len()));
        extra.extend(body.iter().map(|s| s.to_extra()));
        ExtraRange {
            offset,
            len: len_u32(2 + body.len()),
        }
    }

    /// Decode a `WhileLoop` payload from its words.
    pub fn decode<'a>(words: &'a [u32]) -> WhileLoopView<'a> {
        WhileLoopView {
            cond: InstRef::from_raw(words[0]),
            body: ExtraList::new(&words[2..]),
        }
    }
}

/// Layout in `extra` for [`crate::uir::InstTag::ForRange`]:
///
/// ```text
///   [0]       var_name:   StringId.raw()
///   [1]       start:      InstRef.raw()
///   [2]       end:        InstRef.raw()
///   [3]       body_count: u32
///   [4..4+n]  body stmts: InstRef.raw() each
/// ```
pub mod for_range_extra {
    use super::{extra_offset, len_u32};
    use crate::extra::{ExtraList, ExtraRange, ExtraValue};
    use crate::types::StringId;
    use crate::uir::InstRef;

    pub struct ForRangeView<'a> {
        pub var_name: StringId,
        pub start: InstRef,
        pub end: InstRef,
        pub body: ExtraList<'a, InstRef>,
    }

    /// Encode a `ForRange` payload into `extra`, returning its range.
    pub fn encode(
        extra: &mut Vec<u32>,
        var_name: StringId,
        start: InstRef,
        end: InstRef,
        body: &[InstRef],
    ) -> ExtraRange {
        let offset = extra_offset(extra);
        extra.push(var_name.raw());
        extra.push(start.raw());
        extra.push(end.raw());
        extra.push(len_u32(body.len()));
        extra.extend(body.iter().map(|s| s.to_extra()));
        ExtraRange {
            offset,
            len: len_u32(4 + body.len()),
        }
    }

    /// Decode a `ForRange` payload from its words.
    pub fn decode<'a>(words: &'a [u32]) -> ForRangeView<'a> {
        ForRangeView {
            var_name: StringId::from_raw(words[0]),
            start: InstRef::from_raw(words[1]),
            end: InstRef::from_raw(words[2]),
            body: ExtraList::new(&words[4..]),
        }
    }
}

/// Layout in `extra` for [`crate::uir::InstTag::StructLit`]:
///
/// ```text
///   [0]  name:     StringId (or `UIR_ANON_STRUCT_NAME` for an
///                  anonymous literal — there is no declared name)
///   [1]  n_fields: u32
///   [2..2+2*n]     per field: [field_name: StringId.raw(), value: InstRef.raw()]
/// ```
pub mod struct_lit_extra {
    use super::{extra_offset, len_u32};
    use crate::extra::ExtraRange;
    use crate::types::StringId;
    use crate::uir::InstRef;

    /// Anonymous-literal marker for the `0` slot (M10), mirroring
    /// [`var_decl_extra::TY_NONE_SENTINEL`]: `u32::MAX` is outside any
    /// plausible `StringId` range, so it can never collide with a real
    /// (or user-interned) name. Sema infers the structural type from
    /// the field values instead of looking the name up.
    pub const UIR_ANON_STRUCT_NAME: u32 = u32::MAX;

    /// Decoded borrowed view of an [`crate::uir::InstTag::StructLit`] payload. Field
    /// initializers are in source order. `name` of `None` is the M10
    /// anonymous literal (encoded as
    /// `UIR_ANON_STRUCT_NAME`); sema infers
    /// the structural type from the field values instead of resolving a
    /// name.
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

    /// Encode a `StructLit` payload into `extra`, returning its range.
    pub fn encode(
        extra: &mut Vec<u32>,
        name: Option<StringId>,
        fields: &[(StringId, InstRef)],
    ) -> ExtraRange {
        let offset = extra_offset(extra);
        extra.push(name.map_or(UIR_ANON_STRUCT_NAME, |n| n.raw()));
        extra.push(len_u32(fields.len()));
        for &(fname, value) in fields {
            extra.push(fname.raw());
            extra.push(value.raw());
        }
        ExtraRange {
            offset,
            len: len_u32(2 + 2 * fields.len()),
        }
    }

    /// Decode a `StructLit` payload from its words.
    pub fn decode(words: &[u32]) -> StructLitView<'_> {
        let name = match words[0] {
            UIR_ANON_STRUCT_NAME => None,
            raw => Some(StringId::from_raw(raw)),
        };
        let n = words[1] as usize;
        StructLitView {
            name,
            fields: &words[2..2 + 2 * n],
        }
    }
}

/// Layout in `extra` for [`crate::uir::InstTag::FieldAssign`]:
///
/// ```text
///   [0]  target: InstRef.raw() (FieldAccess chain)
///   [1]  value:  InstRef.raw()
/// ```
pub mod field_assign_extra {
    use super::{extra_offset, len_u32};
    use crate::extra::ExtraRange;
    use crate::uir::InstRef;

    /// Decoded view of an [`crate::uir::InstTag::FieldAssign`] payload (M9).
    pub struct FieldAssignView {
        pub target: InstRef,
        pub value: InstRef,
    }

    /// Encode a `FieldAssign` payload into `extra`, returning its range.
    pub fn encode(extra: &mut Vec<u32>, target: InstRef, value: InstRef) -> ExtraRange {
        let offset = extra_offset(extra);
        extra.push(target.raw());
        extra.push(value.raw());
        ExtraRange {
            offset,
            len: len_u32(2),
        }
    }

    /// Decode a `FieldAssign` payload from its words.
    pub fn decode(words: &[u32]) -> FieldAssignView {
        FieldAssignView {
            target: InstRef::from_raw(words[0]),
            value: InstRef::from_raw(words[1]),
        }
    }
}

/// Layout in `extra` for [`crate::uir::InstTag::CompoundFieldAssign`]:
///
/// ```text
///   [0]  target: InstRef.raw() (FieldAccess chain)
///   [1]  op:     u32 (CompoundOp discriminant)
///   [2]  value:  InstRef.raw()
/// ```
pub mod compound_field_assign_extra {
    use super::{extra_offset, len_u32};
    use crate::ast::CompoundOp;
    use crate::extra::ExtraRange;
    use crate::uir::InstRef;

    /// Decoded view of an [`crate::uir::InstTag::CompoundFieldAssign`] payload (M9).
    pub struct CompoundFieldAssignView {
        pub target: InstRef,
        pub op: CompoundOp,
        pub value: InstRef,
    }

    /// Encode a `CompoundFieldAssign` payload into `extra`, returning its
    /// range.
    pub fn encode(
        extra: &mut Vec<u32>,
        target: InstRef,
        op: CompoundOp,
        value: InstRef,
    ) -> ExtraRange {
        let offset = extra_offset(extra);
        extra.push(target.raw());
        extra.push(op as u32);
        extra.push(value.raw());
        ExtraRange {
            offset,
            len: len_u32(3),
        }
    }

    /// Decode a `CompoundFieldAssign` payload from its words.
    pub fn decode(words: &[u32]) -> CompoundFieldAssignView {
        CompoundFieldAssignView {
            target: InstRef::from_raw(words[0]),
            op: CompoundOp::from_raw(words[1]),
            value: InstRef::from_raw(words[2]),
        }
    }
}

/// Layout in `extra` for [`crate::uir::InstTag::IfStmt`]: the condition, a counted
/// then-list, an elif count, each elif condition followed by a counted
/// body, an else marker (`0` = absent, `1` = present), and the counted
/// else list when present.
pub mod if_stmt_extra {
    use super::{extra_offset, len_u32};
    use crate::extra::{ExtraList, ExtraRange};
    use crate::uir::InstRef;

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

    /// Encode an `IfStmt` payload into `extra`, returning its range.
    pub fn encode(
        extra: &mut Vec<u32>,
        cond: InstRef,
        then_stmts: &[InstRef],
        elif_branches: &[(InstRef, Vec<InstRef>)],
        else_stmts: Option<&[InstRef]>,
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
    pub fn decode(words: &[u32]) -> IfStmtView<'_> {
        let mut pos = 0;
        let cond = InstRef::from_raw(words[pos]);
        pos += 1;
        let then_stmts = read_ref_list(words, &mut pos);
        let elif_count = words[pos] as usize;
        pos += 1;
        let elif_start = pos;
        for _ in 0..elif_count {
            let body_len = words[pos + 1] as usize;
            pos += 2 + body_len;
        }
        let elif_branches = ElifBranches {
            words: &words[elif_start..pos],
            remaining: elif_count,
        };
        let has_else = words[pos] != 0;
        pos += 1;
        let else_stmts = if has_else {
            Some(read_ref_list(words, &mut pos))
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

    fn push_ref_list(extra: &mut Vec<u32>, refs: &[InstRef]) {
        extra.push(len_u32(refs.len()));
        for r in refs {
            extra.push(r.raw());
        }
    }

    fn read_ref_list<'a>(slice: &'a [u32], pos: &mut usize) -> ExtraList<'a, InstRef> {
        let count = slice[*pos] as usize;
        *pos += 1;
        let refs = ExtraList::new(&slice[*pos..*pos + count]);
        *pos += count;
        refs
    }
}
