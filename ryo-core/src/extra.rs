//! Shared side-arena range handle for the flat-arena IRs (UIR/TIR).
//!
//! Variable-size instruction payloads live in an `extra: Vec<u32>` arena
//! beside the fixed-size instruction stream; an [`ExtraRange`] addresses one
//! payload as a `(offset, len)` pair. Both IRs use the identical encoding, so
//! the type lives here once rather than being byte-duplicated per module.

/// A `[offset, offset+len)` slice of an `extra: Vec<u32>` arena.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtraRange {
    pub offset: u32,
    pub len: u32,
}

impl ExtraRange {
    pub fn as_range(self) -> std::ops::Range<usize> {
        let start = self.offset as usize;
        start..start + self.len as usize
    }
}
