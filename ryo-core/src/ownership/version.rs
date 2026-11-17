//! Reject cleanup edits after analysis publishes its result.

use super::FunctionSidecar;
use crate::artifact::StampHasher;
use std::hash::{Hash, Hasher};

impl FunctionSidecar {
    fn representation_stamp(&self) -> u64 {
        let mut hash = StampHasher::default();
        self.name.hash(&mut hash);
        self.free_on_reassign.hash(&mut hash);
        self.assign_binding.hash(&mut hash);
        self.consumed_concat_lhs.hash(&mut hash);
        self.field_free_on_reassign.hash(&mut hash);
        self.transfers.hash(&mut hash);
        self.drops.len().hash(&mut hash);
        for drop in &self.drops {
            drop.position.hash(&mut hash);
            drop.target.hash(&mut hash);
            drop.span.source.hash(&mut hash);
            (drop.span.start, drop.span.end).hash(&mut hash);
        }
        self.promotion_frees.len().hash(&mut hash);
        self.promo_allocs.len().hash(&mut hash);
        self.validation.hash(&mut hash);
        for promotion in self.promotion_frees.iter().chain(&self.promo_allocs) {
            promotion.position.hash(&mut hash);
            promotion.base.hash(&mut hash);
            promotion.span.source.hash(&mut hash);
            (promotion.span.start, promotion.span.end).hash(&mut hash);
        }
        hash.finish()
    }

    /// Publish only after the owning analysis and its available validators finish.
    pub fn seal(&mut self) {
        self.content_stamp = Some(self.representation_stamp());
    }

    pub(super) fn require_unchanged(&self) -> Result<(), String> {
        if self.content_stamp != Some(self.representation_stamp()) {
            return Err("cleanup changed after publication or was not elaborated".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::{BlockId, DropOp, DropPosition};
    use crate::source::SourceId;
    use crate::span::Span;
    use crate::tir::TirRef;
    use crate::types::StringId;

    #[test]
    fn source_identity_changes_cleanup_representation_stamp() {
        let mut sidecar = FunctionSidecar::new(StringId::from_raw(1), 2);
        sidecar.drops.push(DropOp {
            position: DropPosition::BlockStart { block: BlockId(0) },
            target: TirRef::from_raw(1),
            span: Span::in_source(SourceId::from_raw_parts(1, 0), 0, 1),
        });
        let first = sidecar.representation_stamp();
        sidecar.drops[0].span = Span::in_source(SourceId::from_raw_parts(2, 0), 0, 1);
        assert_ne!(first, sidecar.representation_stamp());
    }
}
