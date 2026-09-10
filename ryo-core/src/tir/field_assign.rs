//! M9 field-path assignment decode views — split from `tir.rs` to
//! keep that file under the 2000-line gate. Trusted-producer decoders:
//! the only writer of these payloads is [`super::TirBuilder`].

use super::{
    CompoundOp, Tir, TirData, TirRef, TirTag, compound_field_assign_extra, field_assign_extra,
};

/// Decoded view of a [`TirTag::FieldAssign`] payload (M9).
pub struct FieldAssignView {
    pub target: TirRef,
    pub value: TirRef,
}

/// Decoded view of a [`TirTag::CompoundFieldAssign`] payload (M9).
pub struct CompoundFieldAssignView {
    pub target: TirRef,
    pub op: CompoundOp,
    pub value: TirRef,
}

impl Tir {
    pub fn field_assign_view(&self, r: TirRef) -> FieldAssignView {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, TirTag::FieldAssign));
        let range = match inst.data {
            TirData::Extra(rng) => rng,
            _ => unreachable!("FieldAssign must carry TirData::Extra"),
        };
        let slice = &self.extra[range.as_range()];
        FieldAssignView {
            target: TirRef::from_raw(slice[field_assign_extra::TARGET]),
            value: TirRef::from_raw(slice[field_assign_extra::VALUE]),
        }
    }

    pub fn compound_field_assign_view(&self, r: TirRef) -> CompoundFieldAssignView {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, TirTag::CompoundFieldAssign));
        let range = match inst.data {
            TirData::Extra(rng) => rng,
            _ => unreachable!("CompoundFieldAssign must carry TirData::Extra"),
        };
        let slice = &self.extra[range.as_range()];
        CompoundFieldAssignView {
            target: TirRef::from_raw(slice[compound_field_assign_extra::TARGET]),
            op: CompoundOp::from_raw(slice[compound_field_assign_extra::OP]),
            value: TirRef::from_raw(slice[compound_field_assign_extra::VALUE]),
        }
    }
}
