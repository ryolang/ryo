//! Content checks at supported execution-representation boundaries.

use super::Flow;
use crate::artifact::StampHasher;
use std::hash::{Hash, Hasher};

impl Flow {
    pub(super) fn representation_stamp(&self) -> u64 {
        let mut hash = StampHasher::default();
        self.entry.hash(&mut hash);
        self.implicit_fallthrough.hash(&mut hash);
        self.blocks.hash(&mut hash);
        self.steps.hash(&mut hash);
        self.terms.hash(&mut hash);
        self.places.hash(&mut hash);
        for entry in &self.binding_places {
            match entry {
                Some(place) => place.0.hash(&mut hash),
                None => {
                    u32::MAX.hash(&mut hash);
                    u32::MAX.hash(&mut hash);
                }
            }
        }
        self.extra.hash(&mut hash);
        hash.finish()
    }
}
