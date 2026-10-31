//! Content versions protect analyses from edits to cloned typed arenas.

use super::{Tir, TirData};
use crate::artifact::StampHasher;
use std::hash::{Hash, Hasher};

impl Tir {
    pub(crate) fn representation_stamp(&self) -> u64 {
        let mut hash = StampHasher::default();
        self.name.hash(&mut hash);
        self.return_type.hash(&mut hash);
        self.body.offset.hash(&mut hash);
        self.body.len.hash(&mut hash);
        self.contains_match.hash(&mut hash);
        let mut direct_catch: Vec<_> = self.direct_catch_matches.iter().map(|r| r.raw()).collect();
        direct_catch.sort_unstable();
        direct_catch.hash(&mut hash);
        self.extra.hash(&mut hash);
        for param in &self.params {
            param.name.hash(&mut hash);
            param.ty.hash(&mut hash);
            (param.mode as u8).hash(&mut hash);
            param.span.start.hash(&mut hash);
            param.span.end.hash(&mut hash);
        }
        for inst in &self.instructions {
            (inst.tag as u8).hash(&mut hash);
            inst.ty.hash(&mut hash);
            std::mem::discriminant(&inst.data).hash(&mut hash);
            match inst.data {
                TirData::None => {}
                TirData::Int(value) => value.hash(&mut hash),
                TirData::Float(value) => value.to_bits().hash(&mut hash),
                TirData::Str(id) | TirData::Var(id) => id.hash(&mut hash),
                TirData::Bool(value) => value.hash(&mut hash),
                TirData::UnOp(value) => value.hash(&mut hash),
                TirData::BinOp { lhs, rhs } => (lhs, rhs).hash(&mut hash),
                TirData::Slice { base, start, end } => (base, start, end).hash(&mut hash),
                TirData::FieldAccess {
                    object,
                    field_index,
                } => (object, field_index).hash(&mut hash),
                TirData::Extra(range) => (range.offset, range.len).hash(&mut hash),
            }
        }
        for span in &self.spans {
            span.start.hash(&mut hash);
            span.end.hash(&mut hash);
        }
        self.span.start.hash(&mut hash);
        self.span.end.hash(&mut hash);
        hash.finish()
    }
}
