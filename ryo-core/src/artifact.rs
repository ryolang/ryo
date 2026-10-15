//! Process-local identities for immutable compiler representations.

use std::hash::Hasher;
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

/// Fast, deterministic hasher for representation content stamps.
///
/// Stamps guard against accidental edits to published artifacts, not
/// adversarial input, so they do not need SipHash's DoS resistance.
/// Each word folds in through a bijective rotate-xor-multiply step
/// (the FxHash mix), so changing any single word always changes the
/// stamp. This keeps stamping cheap on every compile, where SipHash
/// over whole bodies was a measurable share of ownership analysis.
#[derive(Default)]
pub(crate) struct StampHasher(u64);

impl StampHasher {
    const SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;

    #[inline]
    fn add(&mut self, word: u64) {
        self.0 = (self.0.rotate_left(5) ^ word).wrapping_mul(Self::SEED);
    }
}

impl Hasher for StampHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let (chunks, rest) = bytes.as_chunks::<8>();
        for chunk in chunks {
            self.add(u64::from_le_bytes(*chunk));
        }
        if !rest.is_empty() {
            let mut tail = [0u8; 8];
            tail[..rest.len()].copy_from_slice(rest);
            self.add(u64::from_le_bytes(tail));
        }
        // Length-tag the write so adjacent byte runs cannot alias.
        self.add(bytes.len() as u64);
    }

    #[inline]
    fn write_u8(&mut self, i: u8) {
        self.add(u64::from(i));
    }

    #[inline]
    fn write_u16(&mut self, i: u16) {
        self.add(u64::from(i));
    }

    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.add(u64::from(i));
    }

    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.add(i);
    }

    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.add(i as u64);
    }

    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }
}
