//! M10 destructuring assignment — `TirTag::Destructure` encoding,
//! builder, and decode views; split from `tir.rs` to keep that file
//! under the 2000-line CI gate. Sema is the only producer; codegen
//! and the ownership pass are the consumers.

use super::{ExtraRange, Span, Tir, TirBuilder, TirData, TirRef, TirTag};
use crate::types::{StringId, TypeId};

/// Layout in `extra` for [`TirTag::Destructure`]:
///
/// ```text
///   [0]          rhs:      TirRef.raw()
///   [1]          n_fields: u32
///   [2 + 3i]     field:    field_index: u32,
///                         bind: u32 (0 = wildcard; else StringId.raw() + 1
///                              — the +1 keeps 0 free, since raw 0 is a
///                              real interned string, "derive"),
///                         ty: TypeId raw
/// ```
///
/// `TypedInst.ty` carries the rhs struct type. Entries are in written
/// (pattern) order, resolved by sema to canonical field indices.
pub mod destructure_extra {
    use crate::types::StringId;

    pub const RHS: usize = 0;
    pub const N_FIELDS: usize = 1;
    pub const FIELDS: usize = 2;
    pub const FIELD_LEN: usize = 3;

    /// Encode a bind name: 0 for a wildcard, else `raw + 1`.
    pub fn encode_bind(bind: Option<StringId>) -> u32 {
        bind.map_or(0, |s| s.raw() + 1)
    }

    /// Decode a bind word produced by [`Self::encode_bind`].
    pub fn decode_bind(word: u32) -> Option<StringId> {
        (word > 0).then(|| StringId::from_raw(word - 1))
    }
}

/// One field of a [`TirTag::Destructure`] plan (M10): which struct
/// field, what it binds (`None` = wildcard — destroyed, not bound),
/// and the field's resolved type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DestructureField {
    pub field_index: u32,
    pub bind: Option<StringId>,
    pub ty: TypeId,
}

/// Decoded view of a [`TirTag::Destructure`] payload (M10).
pub struct DestructureView {
    pub rhs: TirRef,
    pub fields: Vec<DestructureField>,
}

impl TirBuilder {
    /// Emit a `Destructure` (M10). `fields` are `(field_index, bind,
    /// ty)` in written order; the instruction's type is the rhs's
    /// struct type (`ty_of(rhs)`).
    ///
    /// OWNER-TOKEN CONVENTION (sema's obligation, checked by
    /// [`Tir::destructure_bound_owner_refs`]): immediately before this
    /// instruction, sema emits exactly one `FieldAccess` instruction
    /// per BOUND field (wildcards skipped), in plan order — one
    /// `TirRef` per bound field, carrying the field type. The
    /// ownership pass adopts those refs as the fresh owners of the
    /// moved-out fields; they are never evaluated by codegen.
    pub fn destructure(
        &mut self,
        rhs: TirRef,
        fields: &[(u32, Option<StringId>, TypeId)],
        span: Span,
    ) -> TirRef {
        debug_assert!(
            !fields.is_empty(),
            "TirBuilder::destructure: a struct always has at least one field"
        );
        debug_assert!(
            fields
                .iter()
                .zip(fields.iter().skip(1))
                .all(|(a, b)| a.0 != b.0),
            "TirBuilder::destructure: duplicate field index in plan"
        );
        let offset = self.extra_offset();
        self.extra.push(rhs.raw());
        self.extra.push(Self::len_u32(fields.len()));
        for &(field_index, bind, ty) in fields {
            self.extra.push(field_index);
            self.extra.push(destructure_extra::encode_bind(bind));
            self.extra.push(ty.raw());
        }
        let len = Self::len_u32(destructure_extra::FIELDS + fields.len() * 3);
        let ty = self.ty_of(rhs);
        self.push(
            TirTag::Destructure,
            ty,
            TirData::Extra(ExtraRange { offset, len }),
            span,
        )
    }
}

impl Tir {
    pub fn destructure_view(&self, r: TirRef) -> DestructureView {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, TirTag::Destructure));
        let range = match inst.data {
            TirData::Extra(rng) => rng,
            _ => unreachable!("Destructure must carry TirData::Extra"),
        };
        let slice = &self.extra[range.as_range()];
        let rhs = TirRef::from_raw(slice[destructure_extra::RHS]);
        let n = slice[destructure_extra::N_FIELDS] as usize;
        debug_assert_eq!(
            slice.len(),
            destructure_extra::FIELDS + n * destructure_extra::FIELD_LEN,
            "destructure plan length mismatch"
        );
        let fields = (0..n)
            .map(|i| {
                let base = destructure_extra::FIELDS + i * destructure_extra::FIELD_LEN;
                DestructureField {
                    field_index: slice[base],
                    bind: destructure_extra::decode_bind(slice[base + 1]),
                    ty: TypeId::from_raw(slice[base + 2]),
                }
            })
            .collect();
        DestructureView { rhs, fields }
    }

    /// The owner tokens of a `Destructure` (M10): one [`TirRef`] per
    /// BOUND field, in plan order — the `FieldAccess` instructions sema
    /// emitted immediately before the `Destructure` inst (see the
    /// builder's convention). The ownership pass registers these as
    /// the fresh owners of the moved-out fields; codegen's free-path
    /// redirect maps them to the bindings' slots.
    pub fn destructure_bound_owner_refs(&self, r: TirRef) -> Vec<TirRef> {
        let view = self.destructure_view(r);
        let n_binds = view.fields.iter().filter(|f| f.bind.is_some()).count();
        debug_assert!(
            n_binds <= r.index(),
            "destructure owner tokens precede the instruction"
        );
        let mut cursor = r.index() - n_binds;
        let mut out = Vec::with_capacity(n_binds);
        for field in &view.fields {
            let Some(_) = field.bind else { continue };
            let owner = TirRef::from_index(cursor);
            cursor += 1;
            debug_assert!(
                matches!(self.inst(owner).tag, TirTag::FieldAccess),
                "destructure owner token must be a FieldAccess"
            );
            debug_assert_eq!(
                self.inst(owner).ty,
                field.ty,
                "destructure owner token carries the field type"
            );
            out.push(owner);
        }
        out
    }
}
