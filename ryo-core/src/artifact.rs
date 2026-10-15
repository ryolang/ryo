//! Process-local identities for immutable compiler representations.

use std::sync::atomic::{AtomicU64, Ordering};

/// A published representation version. A rebuilt body receives a fresh identity.
/// Clones retain identity and must remain immutable while analyses refer to them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArtifactId(u64);

impl ArtifactId {
    pub(crate) fn fresh() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}
