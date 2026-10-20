//! M10 destructuring assignment — `InstTag::Destructure` plan encoding,
//! builder, and decode view; split from `uir.rs` to keep that file under
//! the 2000-line CI gate. Astgen is the only producer (see
//! `astgen.rs::lower_destructure_pattern`); sema is the only consumer.

use super::{InstData, InstRef, InstTag, Span, Uir, UirBuilder};
use crate::types::StringId;

/// Layout in `extra` for [`InstTag::Destructure`]:
///
/// ```text
///   [0]        n_entries: u32
///   [1 + 2i]   selector:  u32  (by_position: element index; named:
///                              field name StringId.raw())
///   [2 + 2i]   bind:      u32  (0 = wildcard; else StringId.raw() + 1 —
///                              the +1 keeps 0 free, since raw 0 is a
///                              real interned string, "derive")
/// ```
///
/// `by_position` lives in [`InstData::Destructure`]. Entries are in
/// written (pattern) order; for positional patterns the selector
/// already IS the canonical field index, for brace patterns sema
/// resolves the name against the struct shape.
pub mod destructure_extra {
    use super::UirDestructureView;
    use crate::extra::ExtraRange;
    use crate::types::StringId;
    use crate::uir::InstRef;
    use crate::uir::wires::{extra_offset, len_u32};

    /// Encode a bind name: 0 for a wildcard, else `raw + 1` (raw 0 is
    /// a real interned string — the pool pre-interns "derive" — so the
    /// wildcard sentinel must live outside the raw id space).
    pub fn encode_bind(bind: Option<StringId>) -> u32 {
        bind.map_or(0, |s| s.raw() + 1)
    }

    /// Decode a bind word produced by [`Self::encode_bind`].
    pub fn decode_bind(word: u32) -> Option<StringId> {
        (word > 0).then(|| StringId::from_raw(word - 1))
    }

    /// Encode a destructure plan into `extra`, returning its range.
    /// The value ref and `by_position` are inline in
    /// [`InstData::Destructure`](crate::uir::InstData::Destructure),
    /// not on the wire.
    pub fn encode(extra: &mut Vec<u32>, plan: &[(u32, Option<StringId>)]) -> ExtraRange {
        let offset = extra_offset(extra);
        extra.push(len_u32(plan.len()));
        for &(selector, bind) in plan {
            extra.push(selector);
            extra.push(encode_bind(bind));
        }
        ExtraRange {
            offset,
            len: len_u32(1 + plan.len() * 2),
        }
    }

    /// Decode a destructure plan from its words; `value` and
    /// `by_position` come from the instruction's inline data.
    pub fn decode(words: &[u32], value: InstRef, by_position: bool) -> UirDestructureView {
        let n = words[0] as usize;
        debug_assert_eq!(words.len(), 1 + n * 2, "destructure plan length mismatch");
        let plan = (0..n)
            .map(|i| {
                let base = 1 + i * 2;
                (words[base], decode_bind(words[base + 1]))
            })
            .collect();
        UirDestructureView {
            value,
            by_position,
            plan,
        }
    }
}

/// Decoded view of an [`InstTag::Destructure`] payload (M10). `plan`
/// carries `(selector, bind)` in written order; `bind == None` is a
/// wildcard — the field is destroyed, not bound.
pub struct UirDestructureView {
    pub value: InstRef,
    pub by_position: bool,
    pub plan: Vec<(u32, Option<StringId>)>,
}

impl UirBuilder {
    /// Emit a `Destructure` (M10). `plan` is `(selector, bind)` in
    /// written order — `by_position == true` means selectors are
    /// element indices, false means field-name ids. The value
    /// expression is lowered separately and carried inline.
    pub fn destructure(
        &mut self,
        value: InstRef,
        by_position: bool,
        plan: &[(u32, Option<StringId>)],
        span: Span,
    ) -> InstRef {
        let range = destructure_extra::encode(&mut self.uir.extra, plan);
        self.push(
            InstTag::Destructure,
            InstData::Destructure {
                value,
                plan: range,
                by_position,
            },
            span,
        )
    }

    /// Mint the next compiler-temp name index for a nested
    /// destructuring sub-pattern (`__ryo_destructure_N`, interned by
    /// the caller). Monotonic per builder — names only need uniqueness
    /// within one function body, and one builder covers the whole
    /// program.
    pub fn next_destructure_temp(&mut self) -> u32 {
        let n = self.destructure_temps_minted;
        self.destructure_temps_minted += 1;
        n
    }
}

impl Uir {
    pub fn destructure_view(&self, r: InstRef) -> UirDestructureView {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, InstTag::Destructure));
        let (value, plan_range, by_position) = match inst.data {
            InstData::Destructure {
                value,
                plan,
                by_position,
            } => (value, plan, by_position),
            _ => unreachable!("Destructure must carry InstData::Destructure"),
        };
        destructure_extra::decode(&self.extra[plan_range.as_range()], value, by_position)
    }
}
