//! Shared side-arena range handle for the flat-arena IRs (UIR/TIR).
//!
//! Variable-size instruction payloads live in an `extra: Vec<u32>` arena
//! beside the fixed-size instruction stream; an [`ExtraRange`] addresses one
//! payload as a `(offset, len)` pair. Both IRs use the identical encoding, so
//! the type lives here once rather than being byte-duplicated per module.

use std::marker::PhantomData;

/// Decode one word from a typed view over the `extra` arena.
pub trait ExtraValue: Copy {
    /// Convert one arena word to its typed value.
    fn from_extra(raw: u32) -> Self;

    /// Encode this value as one arena word. The inverse of
    /// [`Self::from_extra`] for every value each implementor writes.
    fn to_extra(self) -> u32;
}

/// A borrowed, lazily decoded list in the `extra` arena.
#[derive(Debug, Clone, Copy)]
pub struct ExtraList<'a, T> {
    words: &'a [u32],
    marker: PhantomData<T>,
}

impl<'a, T: ExtraValue> ExtraList<'a, T> {
    /// Create a borrowed view over arena words.
    pub fn new(words: &'a [u32]) -> Self {
        Self {
            words,
            marker: PhantomData,
        }
    }

    /// Return the number of words in the list.
    pub fn len(&self) -> usize {
        self.words.len()
    }

    /// Return `true` when the list has no words.
    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    /// Decode the item at `index`, if it exists.
    pub fn get(&self, index: usize) -> Option<T> {
        self.words.get(index).copied().map(T::from_extra)
    }

    /// Decode the item at `index`. Panic if the index is out of bounds.
    pub fn at(&self, index: usize) -> T {
        self.get(index).expect("extra list index out of bounds")
    }

    /// Return an iterator that decodes each word as it reads it.
    pub fn iter(&self) -> ExtraListIter<'_, T> {
        ExtraListIter {
            words: self.words.iter().copied(),
            marker: PhantomData,
        }
    }
}

/// An iterator that decodes words from an [`ExtraList`].
pub struct ExtraListIter<'a, T> {
    words: std::iter::Copied<std::slice::Iter<'a, u32>>,
    marker: PhantomData<T>,
}

impl<T: ExtraValue> Iterator for ExtraListIter<'_, T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        self.words.next().map(T::from_extra)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.words.size_hint()
    }
}

impl<T: ExtraValue> ExactSizeIterator for ExtraListIter<'_, T> {}

impl<T: ExtraValue> DoubleEndedIterator for ExtraListIter<'_, T> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.words.next_back().map(T::from_extra)
    }
}

impl<'list, T: ExtraValue> IntoIterator for &'list ExtraList<'_, T> {
    type Item = T;
    type IntoIter = ExtraListIter<'list, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a, T: ExtraValue> IntoIterator for ExtraList<'a, T> {
    type Item = T;
    type IntoIter = ExtraListIter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        ExtraListIter {
            words: self.words.iter().copied(),
            marker: PhantomData,
        }
    }
}

/// A `[offset, offset+len)` slice of an `extra: Vec<u32>` arena.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
///   [3]  flags:   u32  (FLAG_* bits)
///   [4..4+2*argc] per arg: [field_idx: u32, value: ref raw()]
/// ```
///
/// Unlike the name-keyed `struct_lit` layout, enum args are keyed by
/// declaration-order payload-field index: tuple variants carry the
/// synthesized `"0"`, `"1"`, … names, so an index keeps the wire
/// format name-free and uniform across unit/tuple/named variants. A
/// `field_idx` of [`ORPHANED_FIELD`] marks a braced arg whose name
/// astgen did not recognize (E0038 already emitted); it binds nothing.
/// Unit variants encode `argc = 0` (no trailing pairs).
pub mod enum_lit_extra {
    use super::{ExtraRange, ExtraValue};
    use crate::types::TypeId;
    use std::marker::PhantomData;

    pub const TY: usize = 0;
    pub const VARIANT: usize = 1;
    pub const ARGC: usize = 2;
    pub const FLAGS: usize = 3;
    pub const ARGS: usize = 4;

    /// Source used the parenthesized positional form `Variant(args)`,
    /// set by astgen; the braced named form clears it. Sema reads it
    /// to enforce the Brace Law on named payloads (D11: named variants
    /// construct with braces only); downstream TIR consumers ignore it.
    pub const FLAG_POSITIONAL: u32 = 1;

    /// `field_idx` sentinel for a braced named argument whose field
    /// astgen does not recognize: the pair's value is lowered (so sema
    /// analyzes it and its own diagnostics surface), but it binds no
    /// declared field — sema suppresses the derived missing-fields
    /// error the dropped binding would otherwise trigger.
    pub const ORPHANED_FIELD: u32 = u32::MAX;

    /// Current `extra.len()` as a checked `u32` — the same overflow
    /// rejection the per-IR wire modules perform (the panic text is
    /// shared because the codec no longer lives in one IR's builder).
    fn extra_offset(extra: &[u32]) -> u32 {
        u32::try_from(extra.len()).expect("extra arena exceeded u32::MAX words")
    }

    /// Convert a length-shaped `usize` to `u32`, panicking on overflow.
    fn len_u32(len: usize) -> u32 {
        u32::try_from(len).expect("extra list length exceeded u32::MAX")
    }

    /// Decoded view of an enum literal payload (M11) — shared by UIR
    /// (`InstRef` values) and TIR (`TirRef` values); the wires are
    /// byte-identical (each stores `Ref::raw()` in the value slots).
    /// Borrows the `extra` arena directly — decoding allocates nothing;
    /// `fields()` yields `(field_index, value)` pairs keyed by
    /// declaration-order payload-field index.
    pub struct EnumLitView<'a, R: ExtraValue> {
        pub ty: TypeId,
        pub variant_index: u32,
        /// Source used the parenthesized positional form — see
        /// [`FLAG_POSITIONAL`].
        pub positional: bool,
        args: &'a [u32],
        marker: PhantomData<R>,
    }

    impl<R: ExtraValue> EnumLitView<'_, R> {
        /// `(field_index, value)` pairs, in source order.
        pub fn fields(&self) -> impl Iterator<Item = (u32, R)> + '_ {
            self.args
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&[field_idx, raw]| (field_idx, R::from_extra(raw)))
        }
    }

    /// Encode an enum literal payload into `extra`, returning its range.
    pub fn encode<R: ExtraValue>(
        extra: &mut Vec<u32>,
        ty: TypeId,
        variant_index: u32,
        args: &[(u32, R)],
        positional: bool,
    ) -> ExtraRange {
        let offset = extra_offset(extra);
        extra.push(ty.raw());
        extra.push(variant_index);
        extra.push(len_u32(args.len()));
        extra.push(if positional { FLAG_POSITIONAL } else { 0 });
        for &(field_idx, value) in args {
            extra.push(field_idx);
            extra.push(value.to_extra());
        }
        ExtraRange {
            offset,
            len: len_u32(ARGS + 2 * args.len()),
        }
    }

    /// Decode an enum literal payload from its words.
    pub fn decode<R: ExtraValue>(words: &[u32]) -> EnumLitView<'_, R> {
        let ty = TypeId::from_raw(words[TY]);
        let variant_index = words[VARIANT];
        let positional = words[FLAGS] & FLAG_POSITIONAL != 0;
        let n = words[ARGC] as usize;
        EnumLitView {
            ty,
            variant_index,
            positional,
            args: &words[ARGS..ARGS + 2 * n],
            marker: PhantomData,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tir::{ParamMode, TirRef};
    use crate::uir::InstRef;

    #[test]
    fn extra_value_round_trips_through_to_extra() {
        for raw in [1u32, 7, u32::MAX / 2] {
            assert_eq!(InstRef::from_extra(raw).to_extra(), raw);
            assert_eq!(TirRef::from_extra(raw).to_extra(), raw);
        }
        assert_eq!(ParamMode::from_extra(0).to_extra(), 0);
        assert_eq!(ParamMode::from_extra(1).to_extra(), 1);
        assert_eq!(ParamMode::from_extra(2).to_extra(), 2);
    }
}
