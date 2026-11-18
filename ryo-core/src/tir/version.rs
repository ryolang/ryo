//! Content versions protect analyses from edits to cloned typed arenas.

use super::{Tir, TirData};
use crate::artifact::StampHasher;
use std::hash::{Hash, Hasher};

impl Tir {
    pub(crate) fn representation_stamp(&self) -> u64 {
        let mut hash = StampHasher::default();
        self.layout_context.hash(&mut hash);
        self.decl.hash(&mut hash);
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
            param.span.source.hash(&mut hash);
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
            span.source.hash(&mut hash);
            span.start.hash(&mut hash);
            span.end.hash(&mut hash);
        }
        self.span.source.hash(&mut hash);
        self.span.start.hash(&mut hash);
        self.span.end.hash(&mut hash);
        hash.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::SourceId;
    use crate::span::Span;
    use crate::tir::TirBuilder;
    use crate::types::InternPool;

    fn body(span: Span) -> Tir {
        let mut pool = InternPool::new();
        let name = pool.intern_str("main");
        let mut builder = TirBuilder::new(name, vec![], pool.void(), span);
        let end = builder.return_void(pool.void(), span);
        builder.finish(&[end])
    }

    #[test]
    fn source_identity_changes_tir_representation_stamp() {
        let first = body(Span::in_source(SourceId::from_raw_parts(1, 0), 0, 1));
        let second = body(Span::in_source(SourceId::from_raw_parts(2, 0), 0, 1));
        assert_ne!(first.representation_stamp(), second.representation_stamp());
    }

    #[test]
    fn declaration_identity_changes_tir_representation_stamp() {
        fn identified_body(decl: crate::declaration::DeclId) -> Tir {
            let mut pool = InternPool::new();
            let name = pool.intern_str("main");
            let mut builder =
                TirBuilder::new(name, vec![], pool.void(), Span::EMPTY).with_decl(decl);
            let end = builder.return_void(pool.void(), Span::EMPTY);
            builder.finish(&[end])
        }

        let first = identified_body(crate::declaration::DeclId::from_raw_parts(1, 0));
        let second = identified_body(crate::declaration::DeclId::from_raw_parts(1, 1));
        assert_ne!(first.representation_stamp(), second.representation_stamp());
    }
}
