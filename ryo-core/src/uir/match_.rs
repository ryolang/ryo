//! M12 `match` expression — `InstTag::Match` wire encoding, builder, and
//! decode view; split from `uir.rs` to keep that file under the 2000-line
//! CI gate. Astgen is the only producer; sema is the only consumer. The
//! UIR and TIR match wires are designed independently per layer (R3) —
//! do not assume the TIR layout mirrors this one.
//!
//! ## Wire layout (in `extra`, addressed by `InstData::Match.arms`)
//!
//! The scrutinee ref is inline in [`InstData::Match`]; `extra` holds the
//! arm list:
//!
//! ```text
//! [0]      arm_count: u32
//! [1..]    arms, back to back, each length-prefixed and self-delimiting
//!
//!   per arm (offsets relative to the arm's first word):
//!   [0]      arm_len:      u32 — words in this arm AFTER the arm_len
//!                           word itself (span + pattern payload + body)
//!   [1..2]   pattern span start/end: u32 offsets
//!   [3]      pattern_kind: u32 (0 = wildcard, 1 = literal, 2 = variant,
//!            3 = anon)
//!   [4..]    pattern payload:
//!     wildcard: (no words)
//!     literal:  lit_kind u32 (0 = int, 1 = str, 2 = bool), then
//!               int:  lo u32, hi u32 — the i64 split into two words
//!                     (value = (hi << 32) | lo)
//!               str:  string_id u32 (StringId.raw())
//!               bool: 0 | 1
//!     anon:     flags u32 (bit 0 = POSITIONAL — the tuple-sugar
//!               spelling `(0, _)`, where selectors are element
//!               indices; clear for `{x=0, y=_}`, where selectors are
//!               field-name StringId.raw() — the M10 destructure-plan
//!               selector convention)
//!               fieldc u32
//!               per field, variable length, in written order:
//!                 selector u32
//!                 target_kind u32 (0 = wildcard, 1 = bind, 2 = literal)
//!                 wildcard: (no words)
//!                 bind:   name u32 (StringId.raw())
//!                 literal: lit_kind u32 + literal value words
//!                     (same words as the top-level literal)
//!     variant:  enum_name u32 (0 = unqualified — sema infers the enum;
//!               else StringId.raw() + 1: raw 0 is a real interned
//!               string, so the None sentinel lives outside the raw id
//!               space — the encoding destructure_extra::encode_bind
//!               already uses)
//!               variant u32 (StringId.raw())
//!               flags   u32 (bit 0 = HAS_ARGS, bit 1 = BY_POSITION)
//!               present only when HAS_ARGS:
//!                 argc u32
//!                 per arg, 2 words: [name u32 (0 = positional bind, else
//!                   StringId.raw() + 1), target u32 (0 = wildcard, else
//!                   StringId.raw() + 1 = bind)]
//!   body (immediately after the pattern payload):
//!   [b]      body_kind: u32 (0 = expr, 1 = stmts)
//!   expr:  [b+1]     expr: InstRef.raw()
//!   stmts: [b+1]     stmt_count: u32, [b+2..b+2+n] refs: InstRef.raw()
//!                      each — the IfStmt inline-count convention
//! ```
//!
//! Every arm is length-prefixed, so arms decode independently of their
//! neighbors and two `Match` instructions share the `extra` arena
//! without crosstalk. Decoding allocates nothing beyond the view's arm
//! `Vec` (I-091 discipline): [`UirVariantArgsView`] and
//! [`UirAnonFieldsView`] borrow their wire slices and yield decoded
//! pairs on demand ([`EnumLitView`][crate::tir::EnumLitView]
//! precedent), and [`UirArmBodyView::Stmts`] is the stmt-refs slice
//! itself — its `len()` is the slice length, since the decoder already
//! consumed the count word.

use super::destructure_extra;
use super::{ExtraRange, InstData, InstRef, InstTag, Span, Uir, UirBuilder};
use crate::ast::LitPattern;
use crate::types::StringId;
use chumsky::span::Span as _;

/// Layout constants for the [`InstTag::Match`] wire; see the module
/// header for the full annotated layout.
pub mod match_extra {
    /// `[0] = arm_count`; arms start right after.
    pub const ARM_COUNT: usize = 0;
    pub const ARMS_START: usize = 1;

    /// Per-arm header, relative to the arm's first word.
    pub const ARM_LEN: usize = 0;
    pub const PATTERN_SPAN_START: usize = 1;
    pub const PATTERN_SPAN_END: usize = 2;
    pub const PATTERN_KIND: usize = 3;
    pub const PATTERN_START: usize = 4;

    /// `PATTERN_KIND` discriminants.
    pub const PATTERN_WILDCARD: u32 = 0;
    pub const PATTERN_LITERAL: u32 = 1;
    pub const PATTERN_VARIANT: u32 = 2;
    pub const PATTERN_ANON: u32 = 3;

    /// `PATTERN_LITERAL` payload discriminants (first payload word).
    pub const LIT_INT: u32 = 0;
    pub const LIT_STR: u32 = 1;
    pub const LIT_BOOL: u32 = 2;
    /// Literal payload word counts (discriminant included).
    pub const LIT_INT_WORDS: usize = 3;
    pub const LIT_STR_WORDS: usize = 2;
    pub const LIT_BOOL_WORDS: usize = 2;

    /// Variant payload, relative to the pattern-payload start.
    pub const V_ENUM_NAME: usize = 0;
    pub const V_VARIANT: usize = 1;
    pub const V_FLAGS: usize = 2;
    /// Present only when [`FLAG_HAS_ARGS`] is set.
    pub const V_ARGC: usize = 3;
    pub const V_ARGS: usize = 4;

    /// `V_FLAGS` bits.
    pub const FLAG_HAS_ARGS: u32 = 1 << 0;
    pub const FLAG_BY_POSITION: u32 = 1 << 1;

    /// Anon payload, relative to the pattern-payload start.
    pub const A_FLAGS: usize = 0;
    pub const A_FIELDC: usize = 1;
    pub const A_FIELDS: usize = 2;

    /// `A_FLAGS` bits.
    pub const FLAG_POSITIONAL: u32 = 1 << 0;

    /// Per-field target discriminants (the word after the selector).
    pub const FIELD_WILDCARD: u32 = 0;
    pub const FIELD_BIND: u32 = 1;
    pub const FIELD_LITERAL: u32 = 2;

    /// Body discriminants (first body word, after the pattern payload).
    pub const BODY_EXPR: u32 = 0;
    pub const BODY_STMTS: u32 = 1;
}

// ---------- Builder input types ----------

/// What a variant payload argument targets (M12): `_` skips the slot, a
/// name binds it. Mirrors
/// [`PatternVariantArgTarget`][crate::ast::PatternVariantArgTarget]; sema
/// resolves binds against the variant's payload fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UirArgTarget {
    /// `_` — the payload slot is matched but not bound.
    Wildcard,
    /// `v` / `x = px` — binds a local under the given name.
    Bind(StringId),
}

/// What one field/element of an anonymous-struct or tuple-sugar arm
/// pattern targets (M12): a name binds the slot, `_` skips it, a
/// literal matches an exact value. Mirrors the M10-side
/// [`PatternFieldKind`][crate::ast::PatternFieldKind] pun/rename pair
/// collapsed to one bind case (the pun's name IS the selector).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UirFieldTarget {
    /// `{q}` / `{x = quot}` / `(a, ...)` — binds a local under the
    /// given name.
    Bind(StringId),
    /// `{y = _}` / `(a, _)` — binds nothing.
    Wildcard,
    /// `{x = 0}` / `(0, ...)` — the slot must equal the literal.
    Literal(LitPattern),
}

/// Builder input for one field/element of a [`UirPattern::Anon`]
/// pattern (M12). `selector` is the field name's interned id for the
/// brace spelling and the element index for the tuple sugar — the same
/// selector convention the M10 `Destructure` plan uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UirAnonField {
    /// `{x = ...}`: `x`'s interned id; `(0, ...)`: the element index.
    pub selector: u32,
    /// What the field/element does: bind, skip, or match a literal.
    pub target: UirFieldTarget,
}

/// Builder input for the payload args of a [`UirPattern::Variant`]
/// (M12): `Circle(radius)` is `by_position`, `Coords{x=px}` is named.
/// [`UirBuilder::match_expr`] copies the pairs into `extra`; the
/// decoded view is [`UirVariantArgsView`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UirVariantArgs<'a> {
    pub by_position: bool,
    pub args: &'a [(Option<StringId>, UirArgTarget)],
}

/// Builder input for one `match` arm's pattern (M12): `_`, one exact
/// literal, an anonymous-struct/tuple-sugar shape with literal fields,
/// or one enum variant. Mirrors [`PatternKind`][crate::ast::PatternKind]
/// restricted to the match-arm grammar — bare binds are rejected at
/// parse (whole-arm) or bind one payload slot (variant args, tuple
/// elements), and nested patterns inside tuple elements are M12.5
/// (astgen diagnoses them).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UirPattern<'a> {
    /// `_` — matches anything, binds nothing.
    Wildcard,
    /// `42` / `"quit"` / `true` — one exact literal value.
    Literal(LitPattern),
    /// `{x=0, y=_}` / the tuple sugar `(0, _)`. `positional` is the
    /// paren spelling, where `selector`s are element indices instead
    /// of field-name ids; literal fields ride directly in the pattern
    /// encoding (never via `InstTag::Destructure`).
    Anon {
        positional: bool,
        fields: &'a [UirAnonField],
    },
    /// `Option.Some(v)` / `Red` / `Coords{x=px}`. `enum_name` is `None`
    /// for the unqualified spelling (`Red`), leaving the enum for sema
    /// to infer; `args` is `None` for unit variants.
    Variant {
        enum_name: Option<StringId>,
        variant: StringId,
        args: Option<UirVariantArgs<'a>>,
    },
}

/// Builder input for one `match` arm (M12): `pattern: body`. Astgen
/// lowers each [`MatchArm`][crate::ast::MatchArm] to this; the builder
/// copies every list into `extra`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UirMatchArm<'a> {
    pub span: Span,
    pub pattern: UirPattern<'a>,
    pub body: UirArmBody<'a>,
}

/// Builder input for an arm's body (M12). A single-line expression arm
/// stays an [`Expr`][UirArmBody::Expr]; block arms and single-line
/// statement arms normalize to a one-or-more-element statement list —
/// see [`MatchArmBody`][crate::ast::MatchArmBody].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UirArmBody<'a> {
    Expr(InstRef),
    Stmts(&'a [InstRef]),
}

// ---------- Decoded view types ----------

/// Decoded view of an [`InstTag::Match`] payload (M12). Borrows the
/// `extra` arena; the only allocation is the arms vector.
#[derive(Debug, PartialEq, Eq)]
pub struct UirMatchView<'a> {
    pub scrutinee: InstRef,
    pub arms: Vec<UirMatchArmView<'a>>,
}

/// Decoded view of one match arm; see [`UirMatchArm`].
#[derive(Debug, PartialEq, Eq)]
pub struct UirMatchArmView<'a> {
    pub span: Span,
    pub pattern: UirPatternView<'a>,
    pub body: UirArmBodyView<'a>,
}

/// Decoded view of one arm's pattern; see [`UirPattern`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UirPatternView<'a> {
    Wildcard,
    Literal(LitPattern),
    Anon {
        fields: UirAnonFieldsView<'a>,
    },
    Variant {
        enum_name: Option<StringId>,
        variant: StringId,
        args: Option<UirVariantArgsView<'a>>,
    },
}

/// Decoded view of an anon/tuple arm pattern's fields (M12). Wraps the
/// variable-length wire region and yields decoded `(selector, target)`
/// pairs on demand — no allocation per decode
/// ([`EnumLitView`][crate::tir::EnumLitView] precedent). `positional`
/// mirrors the builder's flag: `true` when selectors are element
/// indices (the tuple sugar) rather than field-name ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UirAnonFieldsView<'a> {
    pub positional: bool,
    wire: &'a [u32],
    count: usize,
}

impl UirAnonFieldsView<'_> {
    /// Number of fields/elements.
    pub fn len(&self) -> usize {
        self.count
    }

    /// `true` when there are no fields.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// `(selector, target)` pairs in written order; `selector` is the
    /// field-name id, or the element index when `positional`.
    pub fn fields(&self) -> impl Iterator<Item = (u32, UirFieldTarget)> + '_ {
        let mut wire = self.wire;
        std::iter::from_fn(move || {
            if wire.is_empty() {
                return None;
            }
            let selector = wire[0];
            let (target, used) = match wire[1] {
                match_extra::FIELD_WILDCARD => (UirFieldTarget::Wildcard, 2),
                match_extra::FIELD_BIND => (UirFieldTarget::Bind(StringId::from_raw(wire[2])), 3),
                match_extra::FIELD_LITERAL => {
                    let (lit, words) = decode_lit_at(wire, 2);
                    (UirFieldTarget::Literal(lit), 2 + words)
                }
                other => unreachable!("unknown anon field target kind {other}"),
            };
            wire = &wire[used..];
            Some((selector, target))
        })
    }
}

/// Decoded view of a variant pattern's payload args (M12). Wraps the
/// wire slice (2 words per arg) and yields decoded `(name, target)`
/// pairs on demand — no allocation per decode
/// ([`EnumLitView`][crate::tir::EnumLitView] precedent).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UirVariantArgsView<'a> {
    pub by_position: bool,
    wire: &'a [u32],
}

impl UirVariantArgsView<'_> {
    /// Number of payload args.
    pub fn len(&self) -> usize {
        self.wire.len() / 2
    }

    /// `true` when there are no payload args.
    pub fn is_empty(&self) -> bool {
        self.wire.is_empty()
    }

    /// `(name, target)` pairs in written order; `name` is `None` for a
    /// positional (parenthesized) arg, `Some` for a named (braced) arg.
    pub fn args(&self) -> impl Iterator<Item = (Option<StringId>, UirArgTarget)> + '_ {
        self.wire.as_chunks::<2>().0.iter().map(|&[name, target]| {
            (
                destructure_extra::decode_bind(name),
                match destructure_extra::decode_bind(target) {
                    None => UirArgTarget::Wildcard,
                    Some(s) => UirArgTarget::Bind(s),
                },
            )
        })
    }
}

/// Decoded view of an arm's body (M12). `Stmts` borrows the wire slice
/// of stmt refs directly: the decoder consumed the count word, so the
/// slice length IS the statement count and iterating allocates nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UirArmBodyView<'a> {
    Expr(InstRef),
    Stmts(&'a [u32]),
}

impl UirArmBodyView<'_> {
    /// The stmt refs of a statement-list body in order (empty for an
    /// expr body).
    pub fn stmts(&self) -> impl Iterator<Item = InstRef> + '_ {
        let words: &[u32] = match self {
            UirArmBodyView::Expr(_) => &[],
            UirArmBodyView::Stmts(w) => w,
        };
        words.iter().copied().map(InstRef::from_raw)
    }
}

// ---------- Builder ----------

impl UirBuilder {
    /// Emit a `match scrutinee:` expression (M12). `arms` is copied
    /// verbatim into `extra` — see the module header for the wire
    /// layout. Sema decodes with [`Uir::match_view`].
    pub fn match_expr(
        &mut self,
        scrutinee: InstRef,
        arms: &[UirMatchArm<'_>],
        span: Span,
    ) -> InstRef {
        let offset = self.extra_offset();
        self.uir.extra.push(Self::len_u32(arms.len()));
        for arm in arms {
            let arm_len_slot = self.uir.extra.len();
            self.uir.extra.push(0); // arm_len, patched after the arm
            self.uir.extra.push(
                u32::try_from(arm.span.start).expect("pattern span start exceeds UIR wire range"),
            );
            self.uir.extra.push(
                u32::try_from(arm.span.end).expect("pattern span end exceeds UIR wire range"),
            );
            self.encode_match_pattern(&arm.pattern);
            self.encode_match_body(&arm.body);
            let arm_len = Self::len_u32(self.uir.extra.len() - arm_len_slot - 1);
            self.uir.extra[arm_len_slot] = arm_len;
        }
        let len = Self::len_u32(self.uir.extra.len() - offset as usize);
        self.push(
            InstTag::Match,
            InstData::Match {
                scrutinee,
                arms: ExtraRange { offset, len },
            },
            span,
        )
    }

    fn encode_match_pattern(&mut self, pattern: &UirPattern<'_>) {
        match pattern {
            UirPattern::Wildcard => self.uir.extra.push(match_extra::PATTERN_WILDCARD),
            UirPattern::Literal(lit) => {
                self.uir.extra.push(match_extra::PATTERN_LITERAL);
                self.encode_lit_value(lit);
            }
            UirPattern::Anon { positional, fields } => {
                self.uir.extra.push(match_extra::PATTERN_ANON);
                self.uir.extra.push(if *positional {
                    match_extra::FLAG_POSITIONAL
                } else {
                    0
                });
                self.uir.extra.push(Self::len_u32(fields.len()));
                for field in *fields {
                    self.uir.extra.push(field.selector);
                    match field.target {
                        UirFieldTarget::Wildcard => {
                            self.uir.extra.push(match_extra::FIELD_WILDCARD)
                        }
                        UirFieldTarget::Bind(name) => {
                            self.uir.extra.push(match_extra::FIELD_BIND);
                            self.uir.extra.push(name.raw());
                        }
                        UirFieldTarget::Literal(lit) => {
                            self.uir.extra.push(match_extra::FIELD_LITERAL);
                            self.encode_lit_value(&lit);
                        }
                    }
                }
            }
            UirPattern::Variant {
                enum_name,
                variant,
                args,
            } => {
                self.uir.extra.push(match_extra::PATTERN_VARIANT);
                self.uir
                    .extra
                    .push(destructure_extra::encode_bind(*enum_name));
                self.uir.extra.push(variant.raw());
                match args {
                    None => self.uir.extra.push(0),
                    Some(args) => {
                        let flags = match_extra::FLAG_HAS_ARGS
                            | if args.by_position {
                                match_extra::FLAG_BY_POSITION
                            } else {
                                0
                            };
                        self.uir.extra.push(flags);
                        self.uir.extra.push(Self::len_u32(args.args.len()));
                        for (name, target) in args.args {
                            self.uir.extra.push(destructure_extra::encode_bind(*name));
                            let bind = match target {
                                UirArgTarget::Wildcard => None,
                                UirArgTarget::Bind(s) => Some(*s),
                            };
                            self.uir.extra.push(destructure_extra::encode_bind(bind));
                        }
                    }
                }
            }
        }
    }

    /// Encode one [`LitPattern`] value (kind word + value words); shared
    /// by top-level literal arms and anon literal fields.
    fn encode_lit_value(&mut self, lit: &LitPattern) {
        match lit {
            LitPattern::Int(v) => {
                self.uir.extra.push(match_extra::LIT_INT);
                let bits = *v as u64;
                self.uir.extra.push(bits as u32);
                self.uir.extra.push((bits >> 32) as u32);
            }
            LitPattern::Str(s) => {
                self.uir.extra.push(match_extra::LIT_STR);
                self.uir.extra.push(s.raw());
            }
            LitPattern::Bool(b) => {
                self.uir.extra.push(match_extra::LIT_BOOL);
                self.uir.extra.push(u32::from(*b));
            }
        }
    }

    fn encode_match_body(&mut self, body: &UirArmBody<'_>) {
        match body {
            UirArmBody::Expr(r) => {
                self.uir.extra.push(match_extra::BODY_EXPR);
                self.uir.extra.push(r.raw());
            }
            UirArmBody::Stmts(stmts) => {
                self.uir.extra.push(match_extra::BODY_STMTS);
                self.uir.extra.push(Self::len_u32(stmts.len()));
                for r in *stmts {
                    self.uir.extra.push(r.raw());
                }
            }
        }
    }
}

// ---------- Decoder ----------

impl Uir {
    /// Decode an [`InstTag::Match`] payload (M12). Borrows `extra`; the
    /// only allocation is the arms vector. Per-arm pattern payloads
    /// decode on demand — see [`UirVariantArgsView`].
    pub fn match_view(&self, r: InstRef) -> UirMatchView<'_> {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, InstTag::Match));
        let (scrutinee, range) = match inst.data {
            InstData::Match { scrutinee, arms } => (scrutinee, arms),
            _ => unreachable!("Match must carry InstData::Match"),
        };
        let slice = &self.extra[range.as_range()];
        let arm_count = slice[match_extra::ARM_COUNT] as usize;
        let mut arms = Vec::with_capacity(arm_count);
        let mut pos = match_extra::ARMS_START;
        for _ in 0..arm_count {
            let arm_len = slice[pos] as usize;
            // Includes the arm_len word itself: the arm decoder works
            // in arm-relative offsets (see `match_extra`).
            let arm = &slice[pos..pos + 1 + arm_len];
            arms.push(decode_match_arm(arm));
            pos += 1 + arm_len;
        }
        debug_assert_eq!(pos, slice.len(), "match payload length mismatch");
        UirMatchView { scrutinee, arms }
    }
}

/// Decode one [`LitPattern`] whose kind word sits at `slice[pos]`;
/// returns the value and the total words it occupies (kind included).
fn decode_lit_at(slice: &[u32], pos: usize) -> (LitPattern, usize) {
    match slice[pos] {
        match_extra::LIT_INT => {
            let bits = (slice[pos + 2] as u64) << 32 | slice[pos + 1] as u64;
            (LitPattern::Int(bits as i64), match_extra::LIT_INT_WORDS)
        }
        match_extra::LIT_STR => (
            LitPattern::Str(StringId::from_raw(slice[pos + 1])),
            match_extra::LIT_STR_WORDS,
        ),
        match_extra::LIT_BOOL => (
            LitPattern::Bool(slice[pos + 1] != 0),
            match_extra::LIT_BOOL_WORDS,
        ),
        other => unreachable!("unknown match literal kind {other}"),
    }
}

/// Decode one length-prefixed arm. `slice` is the full arm —
/// `arm_len` word first, then exactly `arm_len` payload words — so a
/// cursor that ends at `slice.len()` is a trusted-producer invariant.
fn decode_match_arm(slice: &[u32]) -> UirMatchArmView<'_> {
    let span = Span::new(
        (),
        slice[match_extra::PATTERN_SPAN_START] as usize
            ..slice[match_extra::PATTERN_SPAN_END] as usize,
    );
    let mut pos = match_extra::PATTERN_START;
    let pattern = match slice[match_extra::PATTERN_KIND] {
        match_extra::PATTERN_WILDCARD => UirPatternView::Wildcard,
        match_extra::PATTERN_LITERAL => {
            let (lit, words) = decode_lit_at(slice, pos);
            pos += words;
            UirPatternView::Literal(lit)
        }
        match_extra::PATTERN_ANON => {
            let flags = slice[pos + match_extra::A_FLAGS];
            let fieldc = slice[pos + match_extra::A_FIELDC] as usize;
            // Fields are variable-length (literal payloads differ), so
            // scan past them to find where the body starts; the slice
            // between is the fields view's borrowed wire.
            let mut scan = pos + match_extra::A_FIELDS;
            for _ in 0..fieldc {
                let target_kind = slice[scan + 1];
                scan += 2; // selector + target_kind
                match target_kind {
                    match_extra::FIELD_WILDCARD => {}
                    match_extra::FIELD_BIND => scan += 1,
                    match_extra::FIELD_LITERAL => scan += decode_lit_at(slice, scan).1,
                    other => unreachable!("unknown anon field target kind {other}"),
                }
            }
            let fields = UirAnonFieldsView {
                positional: flags & match_extra::FLAG_POSITIONAL != 0,
                wire: &slice[pos + match_extra::A_FIELDS..scan],
                count: fieldc,
            };
            pos = scan;
            UirPatternView::Anon { fields }
        }
        match_extra::PATTERN_VARIANT => {
            let enum_name = destructure_extra::decode_bind(slice[pos + match_extra::V_ENUM_NAME]);
            let variant = StringId::from_raw(slice[pos + match_extra::V_VARIANT]);
            let flags = slice[pos + match_extra::V_FLAGS];
            pos += match_extra::V_ARGC;
            let args = if flags & match_extra::FLAG_HAS_ARGS != 0 {
                let argc = slice[pos] as usize;
                pos += 1;
                let wire = &slice[pos..pos + argc * 2];
                pos += argc * 2;
                Some(UirVariantArgsView {
                    by_position: flags & match_extra::FLAG_BY_POSITION != 0,
                    wire,
                })
            } else {
                None
            };
            UirPatternView::Variant {
                enum_name,
                variant,
                args,
            }
        }
        other => unreachable!("unknown match pattern kind {other}"),
    };
    let body = match slice[pos] {
        match_extra::BODY_EXPR => {
            let body = UirArmBodyView::Expr(InstRef::from_raw(slice[pos + 1]));
            pos += 2;
            body
        }
        match_extra::BODY_STMTS => {
            let count = slice[pos + 1] as usize;
            let body = UirArmBodyView::Stmts(&slice[pos + 2..pos + 2 + count]);
            pos += 2 + count;
            body
        }
        other => unreachable!("unknown match body kind {other}"),
    };
    debug_assert_eq!(pos, slice.len(), "match arm length mismatch");
    UirMatchArmView {
        span,
        pattern,
        body,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::InternPool;
    use chumsky::span::SimpleSpan;

    fn sp() -> Span {
        SimpleSpan::new((), 0..0)
    }

    #[test]
    fn wildcard_arm_with_expr_body_round_trips() {
        let mut b = UirBuilder::new();
        let scrut = b.int_literal(1, sp());
        let body = b.int_literal(2, sp());
        let m = b.match_expr(
            scrut,
            &[UirMatchArm {
                span: sp(),
                pattern: UirPattern::Wildcard,
                body: UirArmBody::Expr(body),
            }],
            sp(),
        );

        let uir = b.finish();
        let view = uir.match_view(m);
        assert_eq!(view.scrutinee, scrut);
        assert_eq!(view.arms.len(), 1);
        assert_eq!(view.arms[0].pattern, UirPatternView::Wildcard);
        assert!(
            matches!(view.arms[0].body, UirArmBodyView::Expr(r) if r == body),
            "expr body must decode to the emitted ref"
        );
    }

    #[test]
    fn variant_arm_with_positional_args_and_stmt_body_round_trips() {
        let mut pool = InternPool::new();
        let option = pool.intern_str("Option");
        let some = pool.intern_str("Some");
        let v = pool.intern_str("v");

        let mut b = UirBuilder::new();
        let scrut = b.var_ref(option, sp());
        let s1 = b.int_literal(1, sp());
        let s2 = b.int_literal(2, sp());
        let arm = UirMatchArm {
            span: sp(),
            pattern: UirPattern::Variant {
                enum_name: Some(option),
                variant: some,
                args: Some(UirVariantArgs {
                    by_position: true,
                    args: &[
                        (None, UirArgTarget::Bind(v)),
                        (None, UirArgTarget::Wildcard),
                    ],
                }),
            },
            body: UirArmBody::Stmts(&[s1, s2]),
        };
        let m = b.match_expr(scrut, &[arm], sp());

        let uir = b.finish();
        let view = uir.match_view(m);
        assert_eq!(view.scrutinee, scrut);
        assert_eq!(view.arms.len(), 1);
        let UirMatchArmView { pattern, body, .. } = &view.arms[0];
        match pattern {
            UirPatternView::Variant {
                enum_name,
                variant,
                args,
            } => {
                assert_eq!(*enum_name, Some(option));
                assert_eq!(*variant, some);
                let args = args.as_ref().expect("variant arm carries args");
                assert!(args.by_position);
                assert_eq!(args.len(), 2);
                assert!(!args.is_empty());
                assert_eq!(
                    args.args().collect::<Vec<_>>(),
                    [
                        (None, UirArgTarget::Bind(v)),
                        (None, UirArgTarget::Wildcard)
                    ]
                );
            }
            other => panic!("expected variant pattern, got {other:?}"),
        }
        let UirArmBodyView::Stmts(words) = body else {
            panic!("expected stmts body, got {body:?}");
        };
        assert_eq!(words.len(), 2);
        assert_eq!(body.stmts().collect::<Vec<_>>(), [s1, s2]);
    }

    #[test]
    fn literal_arms_round_trip() {
        let mut pool = InternPool::new();
        let quit = pool.intern_str("quit");

        let mut b = UirBuilder::new();
        let scrut = b.str_literal(quit, sp());
        let e1 = b.int_literal(10, sp());
        let e2 = b.int_literal(20, sp());
        let e3 = b.int_literal(30, sp());
        let m = b.match_expr(
            scrut,
            &[
                UirMatchArm {
                    span: sp(),
                    pattern: UirPattern::Literal(LitPattern::Int(-7)),
                    body: UirArmBody::Expr(e1),
                },
                UirMatchArm {
                    span: sp(),
                    pattern: UirPattern::Literal(LitPattern::Str(quit)),
                    body: UirArmBody::Expr(e2),
                },
                UirMatchArm {
                    span: sp(),
                    pattern: UirPattern::Literal(LitPattern::Bool(true)),
                    body: UirArmBody::Expr(e3),
                },
            ],
            sp(),
        );

        let uir = b.finish();
        let view = uir.match_view(m);
        assert_eq!(view.scrutinee, scrut);
        assert_eq!(view.arms.len(), 3);
        assert_eq!(
            view.arms[0].pattern,
            UirPatternView::Literal(LitPattern::Int(-7))
        );
        assert_eq!(
            view.arms[1].pattern,
            UirPatternView::Literal(LitPattern::Str(quit))
        );
        assert_eq!(
            view.arms[2].pattern,
            UirPatternView::Literal(LitPattern::Bool(true))
        );
        let bodies = view
            .arms
            .iter()
            .map(|a| a.body)
            .collect::<Vec<UirArmBodyView>>();
        assert_eq!(
            bodies,
            [
                UirArmBodyView::Expr(e1),
                UirArmBodyView::Expr(e2),
                UirArmBodyView::Expr(e3)
            ]
        );
    }

    #[test]
    fn two_match_insts_decode_independently() {
        let mut pool = InternPool::new();
        let color = pool.intern_str("Color");
        let red = pool.intern_str("Red");
        let blue = pool.intern_str("Blue");
        let x = pool.intern_str("x");
        let px = pool.intern_str("px");

        let mut b = UirBuilder::new();
        // m1: wildcard + negative-int literal arms with expr bodies.
        let s1 = b.int_literal(0, sp());
        let e1 = b.int_literal(1, sp());
        let e2 = b.int_literal(2, sp());
        let m1 = b.match_expr(
            s1,
            &[
                UirMatchArm {
                    span: sp(),
                    pattern: UirPattern::Wildcard,
                    body: UirArmBody::Expr(e1),
                },
                UirMatchArm {
                    span: sp(),
                    pattern: UirPattern::Literal(LitPattern::Int(-1)),
                    body: UirArmBody::Expr(e2),
                },
            ],
            sp(),
        );
        // m2: unqualified unit variant with a stmt body, then a named
        // (braced) variant arg with an expr body.
        let s2 = b.bool_literal(true, sp());
        let st = b.return_void(sp());
        let e3 = b.int_literal(3, sp());
        let m2 = b.match_expr(
            s2,
            &[
                UirMatchArm {
                    span: sp(),
                    pattern: UirPattern::Variant {
                        enum_name: None,
                        variant: red,
                        args: None,
                    },
                    body: UirArmBody::Stmts(&[st]),
                },
                UirMatchArm {
                    span: sp(),
                    pattern: UirPattern::Variant {
                        enum_name: Some(color),
                        variant: blue,
                        args: Some(UirVariantArgs {
                            by_position: false,
                            args: &[(Some(x), UirArgTarget::Bind(px))],
                        }),
                    },
                    body: UirArmBody::Expr(e3),
                },
            ],
            sp(),
        );

        let uir = b.finish();

        let v1 = uir.match_view(m1);
        assert_eq!(v1.scrutinee, s1);
        assert_eq!(v1.arms.len(), 2);
        assert_eq!(v1.arms[0].pattern, UirPatternView::Wildcard);
        assert_eq!(
            v1.arms[1].pattern,
            UirPatternView::Literal(LitPattern::Int(-1))
        );
        assert_eq!(v1.arms[0].body, UirArmBodyView::Expr(e1));
        assert_eq!(v1.arms[1].body, UirArmBodyView::Expr(e2));

        let v2 = uir.match_view(m2);
        assert_eq!(v2.scrutinee, s2);
        assert_eq!(v2.arms.len(), 2);
        assert_eq!(
            v2.arms[0].pattern,
            UirPatternView::Variant {
                enum_name: None,
                variant: red,
                args: None,
            }
        );
        assert_eq!(v2.arms[0].body.stmts().collect::<Vec<_>>(), [st]);
        match &v2.arms[1].pattern {
            UirPatternView::Variant {
                enum_name,
                variant,
                args,
            } => {
                assert_eq!(*enum_name, Some(color));
                assert_eq!(*variant, blue);
                let args = args.as_ref().expect("named arm carries args");
                assert!(!args.by_position);
                assert_eq!(args.len(), 1);
                assert_eq!(
                    args.args().collect::<Vec<_>>(),
                    [(Some(x), UirArgTarget::Bind(px))]
                );
            }
            other => panic!("expected variant pattern, got {other:?}"),
        }
        assert_eq!(v2.arms[1].body, UirArmBodyView::Expr(e3));

        // The ranges are independent: m1 still decodes exactly after
        // m2 was decoded (and vice versa — views borrow, so both stay
        // valid together).
        let v1_again = uir.match_view(m1);
        assert_eq!(v1_again.arms.len(), 2);
        assert_eq!(
            v1_again.arms[1].pattern,
            UirPatternView::Literal(LitPattern::Int(-1))
        );
    }

    #[test]
    fn anon_arms_round_trip() {
        let mut pool = InternPool::new();
        let x = pool.intern_str("x");
        let y = pool.intern_str("y");
        let q = pool.intern_str("q");
        let quot = pool.intern_str("quot");

        let mut b = UirBuilder::new();
        let scrut = b.var_ref(q, sp());
        let e1 = b.int_literal(1, sp());
        let e2 = b.int_literal(2, sp());
        // m1: brace anon with a literal and a wildcard field.
        let brace_fields = [
            UirAnonField {
                selector: x.raw(),
                target: UirFieldTarget::Literal(LitPattern::Int(0)),
            },
            UirAnonField {
                selector: y.raw(),
                target: UirFieldTarget::Wildcard,
            },
        ];
        // m2: tuple sugar with a bind, a wildcard, and a str literal —
        // index selectors, exercising every target kind.
        let paren_fields = [
            UirAnonField {
                selector: 0,
                target: UirFieldTarget::Bind(q),
            },
            UirAnonField {
                selector: 1,
                target: UirFieldTarget::Wildcard,
            },
            UirAnonField {
                selector: 2,
                target: UirFieldTarget::Literal(LitPattern::Str(quot)),
            },
        ];
        let m1 = b.match_expr(
            scrut,
            &[UirMatchArm {
                span: sp(),
                pattern: UirPattern::Anon {
                    positional: false,
                    fields: &brace_fields,
                },
                body: UirArmBody::Expr(e1),
            }],
            sp(),
        );
        let m2 = b.match_expr(
            scrut,
            &[UirMatchArm {
                span: sp(),
                pattern: UirPattern::Anon {
                    positional: true,
                    fields: &paren_fields,
                },
                body: UirArmBody::Expr(e2),
            }],
            sp(),
        );

        let uir = b.finish();

        let v1 = uir.match_view(m1);
        match &v1.arms[0].pattern {
            UirPatternView::Anon { fields } => {
                assert!(!fields.positional);
                assert_eq!(fields.len(), 2);
                assert!(!fields.is_empty());
                assert_eq!(
                    fields.fields().collect::<Vec<_>>(),
                    [
                        (x.raw(), UirFieldTarget::Literal(LitPattern::Int(0))),
                        (y.raw(), UirFieldTarget::Wildcard),
                    ]
                );
            }
            other => panic!("expected anon pattern, got {other:?}"),
        }
        assert_eq!(v1.arms[0].body, UirArmBodyView::Expr(e1));

        let v2 = uir.match_view(m2);
        match &v2.arms[0].pattern {
            UirPatternView::Anon { fields } => {
                assert!(fields.positional);
                assert_eq!(fields.len(), 3);
                assert_eq!(
                    fields.fields().collect::<Vec<_>>(),
                    [
                        (0, UirFieldTarget::Bind(q)),
                        (1, UirFieldTarget::Wildcard),
                        (2, UirFieldTarget::Literal(LitPattern::Str(quot))),
                    ]
                );
            }
            other => panic!("expected anon pattern, got {other:?}"),
        }
        assert_eq!(v2.arms[0].body, UirArmBodyView::Expr(e2));
    }

    #[test]
    fn match_dump_renders_scrutinee_patterns_and_bodies() {
        let mut pool = InternPool::new();
        let option = pool.intern_str("Option");
        let some = pool.intern_str("Some");
        let color = pool.intern_str("Color");
        let red = pool.intern_str("Red");
        let x = pool.intern_str("x");
        let y = pool.intern_str("y");
        let px = pool.intern_str("px");
        let quit = pool.intern_str("quit");

        let mut b = UirBuilder::new();
        let scrut = b.var_ref(option, sp());
        let e0 = b.int_literal(0, sp());
        let e1 = b.int_literal(1, sp());
        let e2 = b.int_literal(2, sp());
        let e3 = b.int_literal(3, sp());
        let e4 = b.int_literal(4, sp());
        let st = b.return_void(sp());
        let brace_fields = [
            UirAnonField {
                selector: x.raw(),
                target: UirFieldTarget::Literal(LitPattern::Int(0)),
            },
            UirAnonField {
                selector: y.raw(),
                target: UirFieldTarget::Wildcard,
            },
        ];
        b.match_expr(
            scrut,
            &[
                UirMatchArm {
                    span: sp(),
                    pattern: UirPattern::Wildcard,
                    body: UirArmBody::Expr(e1),
                },
                UirMatchArm {
                    span: sp(),
                    pattern: UirPattern::Literal(LitPattern::Int(-7)),
                    body: UirArmBody::Expr(e0),
                },
                UirMatchArm {
                    span: sp(),
                    pattern: UirPattern::Literal(LitPattern::Str(quit)),
                    body: UirArmBody::Expr(e2),
                },
                UirMatchArm {
                    span: sp(),
                    pattern: UirPattern::Variant {
                        enum_name: Some(option),
                        variant: some,
                        args: Some(UirVariantArgs {
                            by_position: true,
                            args: &[
                                (None, UirArgTarget::Bind(x)),
                                (None, UirArgTarget::Wildcard),
                            ],
                        }),
                    },
                    body: UirArmBody::Expr(e3),
                },
                UirMatchArm {
                    span: sp(),
                    pattern: UirPattern::Variant {
                        enum_name: Some(color),
                        variant: red,
                        args: Some(UirVariantArgs {
                            by_position: false,
                            args: &[(Some(x), UirArgTarget::Bind(px))],
                        }),
                    },
                    body: UirArmBody::Stmts(&[st]),
                },
                UirMatchArm {
                    span: sp(),
                    pattern: UirPattern::Anon {
                        positional: false,
                        fields: &brace_fields,
                    },
                    body: UirArmBody::Expr(e4),
                },
            ],
            sp(),
        );

        let uir = b.finish();
        let dump = format!("{}", uir.dump(&pool));
        assert!(
            dump.contains(&format!("match %{}", scrut.index())),
            "scrutinee missing, got:\n{dump}"
        );
        assert!(
            dump.contains("_: expr"),
            "wildcard arm missing, got:\n{dump}"
        );
        assert!(
            dump.contains("-7: expr"),
            "int literal arm missing, got:\n{dump}"
        );
        assert!(
            dump.contains("\"quit\": expr"),
            "str literal arm missing, got:\n{dump}"
        );
        assert!(
            dump.contains("Option.Some(x, _)"),
            "positional variant arm missing, got:\n{dump}"
        );
        assert!(
            dump.contains(&format!("Color.Red{{x: px}}: stmts [%{}]", st.index())),
            "named variant stmts arm missing, got:\n{dump}"
        );
        assert!(
            dump.contains("{x: 0, y: _}"),
            "anon fields arm missing, got:\n{dump}"
        );
    }
}
