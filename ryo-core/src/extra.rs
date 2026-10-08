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

/// Layout in `extra` for [`crate::uir::InstTag::EnumLit`] and
/// [`crate::tir::TirTag::EnumLit`] (M11) — both IRs share the wire
/// (UIR stores `InstRef.raw()` in the value slots, TIR stores
/// `TirRef.raw()`):
///
/// ```text
///   [0]  ty:      u32  (TypeId.raw())
///   [1]  variant: u32  (declaration-order variant index)
///   [2]  argc:    u32
///   [3..3+2*argc] per arg: [field_idx: u32, value: ref raw()]
/// ```
///
/// Unlike the name-keyed `struct_lit` layout, enum args are keyed by
/// declaration-order payload-field index: tuple variants carry the
/// synthesized `"0"`, `"1"`, … names, so an index keeps the wire
/// format name-free and uniform across unit/tuple/named variants.
/// Unit variants encode `argc = 0` (no trailing pairs).
pub mod enum_lit_extra {
    pub const TY: usize = 0;
    pub const VARIANT: usize = 1;
    pub const ARGC: usize = 2;
    pub const ARGS: usize = 3;
}
