//! M10 destructuring assignment — `TirTag::Destructure` encoding,
//! builder, and decode views; split from `tir.rs` to keep that file
//! under the 2000-line CI gate. Sema is the only producer; codegen
//! and the ownership pass are the consumers.

use super::{Span, Tir, TirBuilder, TirData, TirRef, TirTag};
use crate::tir::BindingId;
use crate::types::TypeId;

/// Layout in `extra` for [`TirTag::Destructure`]:
///
/// ```text
///   [0]          rhs:      TirRef.raw()
///   [1]          n_fields: u32
///   [2 + 3i]     field:    field_index: u32,
///                         bind: u32 (0 = wildcard; else BindingId.raw() + 1
///                              — the +1 keeps 0 free for the wildcard),
///                         ty: TypeId raw
/// ```
///
/// `TypedInst.ty` carries the rhs struct type. Entries are in written
/// (pattern) order, resolved by sema to canonical field indices.
pub mod destructure_extra {
    use super::{DestructureField, DestructureView};
    use crate::extra::ExtraRange;
    use crate::tir::BindingId;
    use crate::tir::TirRef;
    use crate::tir::wires::{extra_offset, len_u32};
    use crate::types::TypeId;

    /// Encode a binding ID: 0 for a wildcard, else `raw + 1`.
    pub fn encode_bind(bind: Option<BindingId>) -> u32 {
        bind.map_or(0, |s| s.raw() + 1)
    }

    /// Decode a bind word produced by [`Self::encode_bind`].
    pub fn decode_bind(word: u32) -> Option<BindingId> {
        (word > 0).then(|| BindingId::from_index((word - 1) as usize))
    }

    /// Encode a `Destructure` payload into `extra`, returning its
    /// range. The rhs ref is word 0 of the wire; the instruction's
    /// type is the rhs's struct type (`ty_of(rhs)`) — the caller keeps
    /// it on the `TypedInst.ty` slot.
    pub fn encode(
        extra: &mut Vec<u32>,
        rhs: TirRef,
        fields: &[(u32, Option<BindingId>, TypeId)],
    ) -> ExtraRange {
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
        let offset = extra_offset(extra);
        extra.push(rhs.raw());
        extra.push(len_u32(fields.len()));
        for &(field_index, bind, ty) in fields {
            extra.push(field_index);
            extra.push(encode_bind(bind));
            extra.push(ty.raw());
        }
        ExtraRange {
            offset,
            len: len_u32(2 + fields.len() * 3),
        }
    }

    /// Decode a `Destructure` payload from its words.
    pub fn decode(words: &[u32]) -> DestructureView {
        let rhs = TirRef::from_raw(words[0]);
        let n = words[1] as usize;
        debug_assert_eq!(words.len(), 2 + n * 3, "destructure plan length mismatch");
        let fields = (0..n)
            .map(|i| {
                let base = 2 + i * 3;
                DestructureField {
                    field_index: words[base],
                    bind: decode_bind(words[base + 1]),
                    ty: TypeId::from_raw(words[base + 2]),
                }
            })
            .collect();
        DestructureView { rhs, fields }
    }
}

/// One field of a [`TirTag::Destructure`] plan (M10): which struct
/// field, what it binds (`None` = wildcard — destroyed, not bound),
/// and the field's resolved type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DestructureField {
    pub field_index: u32,
    pub bind: Option<BindingId>,
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
        fields: &[(u32, Option<BindingId>, TypeId)],
        span: Span,
    ) -> TirRef {
        let range = destructure_extra::encode(&mut self.extra, rhs, fields);
        let ty = self.ty_of(rhs);
        self.push(TirTag::Destructure, ty, TirData::Extra(range), span)
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
        destructure_extra::decode(&self.extra[range.as_range()])
    }

    /// The owner tokens of a `Destructure` (M10): one [`TirRef`] per
    /// BOUND field, in plan order — the `FieldAccess` instructions sema
    /// emitted immediately before the `Destructure` inst (see the
    /// builder's convention). The ownership pass registers these as
    /// the fresh owners of the moved-out fields; codegen's free-path
    /// redirect maps them to the bindings' slots.
    pub fn destructure_bound_owner_refs(&self, r: TirRef) -> Vec<TirRef> {
        self.destructure_owner_tokens(r)
            .map(|tokens| tokens.iter().map(|token| token.owner).collect())
            .expect("valid destructure owner tokens")
    }
}
