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
        self.binding_places.len().hash(&mut hash);
        for entry in &self.binding_places {
            match entry {
                Some(place) => u64::from(place.0).hash(&mut hash),
                None => (1u64 << 32).hash(&mut hash),
            }
        }
        self.extra.hash(&mut hash);
        hash.finish()
    }
}
