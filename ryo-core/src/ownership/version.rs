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
            (drop.span.start, drop.span.end).hash(&mut hash);
        }
        self.promotion_frees.len().hash(&mut hash);
        self.promo_allocs.len().hash(&mut hash);
        self.validation.hash(&mut hash);
        for promotion in self.promotion_frees.iter().chain(&self.promo_allocs) {
            promotion.position.hash(&mut hash);
            promotion.base.hash(&mut hash);
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
