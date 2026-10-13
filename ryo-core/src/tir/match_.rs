//! M12 `match` expression — `TirTag::Match` wire encoding, builder,
//! typed decode views, and the by-move owner-token recovery; split
//! from `tir.rs` to keep that file under the 2000-line CI gate. Sema
//! is the only producer; codegen and the ownership pass are the
//! consumers. The TIR wire is designed independently of the UIR
//! layer's (`uir::match_`): UIR patterns carry unresolved names and
//! source shapes, while every TIR arm is a resolved [`Discriminant`]
//! plus typed [`MatchBinding`]s.
//!
//! ## Wire layout (in `extra`, addressed by `TirData::Extra`)
//!
//! The scrutinee ref heads the payload; `TypedInst.ty` carries the
//! match expression's result type (never duplicated on the wire —
//! same discipline as `var_decl_extra`):
//!
//! ```text
//! [0]      scrutinee: TirRef.raw()
//! [1]      arm_count: u32
//! [2..]    arms, back to back, each length-prefixed and self-delimiting
//!
//!   per arm (offsets relative to the arm's first word):
//!   [0]      arm_len:           u32 — words in this arm AFTER the
//!                                arm_len word itself
//!   [1]      discriminant_kind: u32 (0 = variant, 1 = int, 2 = str,
//!                                3 = bool, 4 = wildcard)
//!   [2..]    discriminant payload:
//!     variant:  variant_index u32 (declaration-order, the
//!               `enum_lit_extra` convention — sema resolved the name)
//!     int:      lo u32, hi u32 — the i64 split into two words
//!               (value = (hi << 32) | lo)
//!     str:      string_id u32 (StringId.raw())
//!     bool:     0 | 1
//!     wildcard: (no words)
//!   [b]      binding_count: u32
//!   [b+1..]  per binding, 3 words: field_index u32,
//!                                  name u32 (StringId.raw()),
//!                                  ty u32 (TypeId.raw())
//!   [g]      guard_count: u32 — anon-arm literal constraints
//!            (`{x=0}`, `(0, _)`), empty for every other arm
//!   [g+1..]  per guard, variable length: field_index u32, lit_kind u32
//!            (0 = int, 1 = str, 2 = bool), then the literal payload:
//!            int: lo u32, hi u32 (the i64 split, discriminant-style)
//!            str: string_id u32 (StringId.raw())
//!            bool: 0 | 1
//!   body (immediately after the guards):
//!   [x]      body_kind: u32 (0 = expr, 1 = stmts)
//!   expr:  [x+1]     expr: TirRef.raw()
//!   stmts: [x+1]     stmt_count: u32, [x+2..x+2+n] refs:
//!                      TirRef.raw() each — the IfStmt inline-count
//!                      convention; the decoded `Stmts` slice length IS
//!                      the count
//! ```
//!
//! Every arm is length-prefixed, so arms decode independently of
//! their neighbors and two `Match` instructions share the `extra`
//! arena without crosstalk. Decoding allocates nothing (I-091
//! discipline): [`MatchView`] borrows the arm region and decodes arms
//! on demand by index — [`MatchArmView::bindings`] wraps the binding
//! words and yields decoded [`MatchBinding`]s on demand
//! ([`EnumLitView`][crate::tir::EnumLitView] precedent), and
//! [`MatchBodyView::Stmts`] is the stmt-refs slice itself.

use super::{ChildKind, ExtraRange, Span, Tir, TirBuilder, TirData, TirRef, TirTag};
use crate::ast::LitPattern;
use crate::types::{InternPool, StringId, TypeId};

/// Layout constants for the [`TirTag::Match`] wire; see the module
/// header for the full annotated layout.
pub mod match_extra {
    /// `[0] = scrutinee`, `[1] = arm_count`; arms start right after.
    pub const SCRUTINEE: usize = 0;
    pub const ARM_COUNT: usize = 1;
    pub const ARMS_START: usize = 2;

    /// Per-arm header, relative to the arm's first word.
    pub const ARM_LEN: usize = 0;
    pub const DISC_KIND: usize = 1;
    pub const DISC_PAYLOAD: usize = 2;

    /// `DISC_KIND` discriminants.
    pub const DISC_VARIANT: u32 = 0;
    pub const DISC_INT: u32 = 1;
    pub const DISC_STR: u32 = 2;
    pub const DISC_BOOL: u32 = 3;
    pub const DISC_WILDCARD: u32 = 4;

    /// Binding list, relative to the first word after the
    /// discriminant payload.
    pub const BIND_COUNT: usize = 0;
    pub const BINDINGS: usize = 1;
    /// Words per binding: `field_index`, `name`, `ty`.
    pub const BINDING_LEN: usize = 3;

    /// Guard list (anon-arm literal constraints), relative to the
    /// first guard-list word — which sits immediately after the
    /// bindings and before the body.
    pub const GUARD_COUNT: usize = 0;
    /// Per-guard words, relative to the guard's first word.
    pub const GUARD_FIELD: usize = 0;
    pub const GUARD_KIND: usize = 1;
    /// Total words per guard (header + payload).
    pub const GUARD_INT_WORDS: usize = 4;
    pub const GUARD_STR_WORDS: usize = 3;
    pub const GUARD_BOOL_WORDS: usize = 3;

    /// `GUARD_KIND` literal kinds.
    pub const GUARD_INT: u32 = 0;
    pub const GUARD_STR: u32 = 1;
    pub const GUARD_BOOL: u32 = 2;

    /// Body discriminants (first body word, after the guards).
    pub const BODY_EXPR: u32 = 0;
    pub const BODY_STMTS: u32 = 1;
}

// ---------- Builder input types ----------

/// The resolved arm selector of a typed `match` (M12): what value one
/// arm matches, after sema resolved source names and literals.
/// `Wildcard` is the catch-all `_` arm, present from the start so arm
/// coverage analysis can rely on it. Deliberately carries no
/// source-name fields — the UIR layer's unresolved `enum_name` /
/// `by_position` shapes are resolved away by sema, and this wire is
/// designed independently of that one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Discriminant {
    /// An enum variant arm, keyed by declaration-order variant index
    /// (the [`crate::tir::enum_lit_extra`] convention — sema resolved
    /// the variant name against the scrutinee's enum type).
    Variant(u32),
    /// An exact `int` literal arm.
    Int(i64),
    /// An exact `str` literal arm.
    Str(StringId),
    /// An exact `bool` literal arm.
    Bool(bool),
    /// The catch-all `_` arm: matches anything, binds nothing.
    Wildcard,
}

/// One payload binding of a typed match arm (M12): which scrutinee
/// field the arm's pattern binds (canonical declaration-order index,
/// the M10 destructure-plan convention), the local it binds under,
/// and the field's resolved type.
///
/// By-move vs copy is deliberately NOT stored: it is derivable from
/// [`InternPool::is_copy`], and the owner-token convention (see
/// [`TirBuilder::match_expr`]) is the only consumer of that bit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatchBinding {
    pub field_index: u32,
    pub name: StringId,
    pub ty: TypeId,
}

/// Builder input for a typed match arm's body (M12); decoded form is
/// [`MatchBodyView`]. A single-line expression arm stays an
/// [`Expr`][TypedMatchBody::Expr]; block arms and single-line
/// statement arms normalize to a one-or-more-element statement list —
/// see [`MatchArmBody`][crate::ast::MatchArmBody].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypedMatchBody<'a> {
    Expr(TirRef),
    Stmts(&'a [TirRef]),
}

/// Builder input for one typed match arm (M12): resolved
/// `discriminant`, payload `bindings`, anon-shape literal `guards`, and
/// `body`. Sema lowers each sema-resolved arm to this;
/// [`TirBuilder::match_expr`] copies every list into `extra`.
///
/// `guards` is the anon-pattern field-equality constraint list
/// (`{x=0}`, `(0, _)`): `(canonical field index, literal)` in written
/// order. Empty for wildcard / literal / variant arms — those encode
/// their matching entirely in the `discriminant`. Kept orthogonal to
/// the `Discriminant::Wildcard` anon placeholder so coverage semantics
/// and guard semantics evolve independently (M12.5 lowers guards to
/// field-equality tests in codegen).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TypedMatchArm<'a> {
    pub discriminant: Discriminant,
    pub bindings: &'a [MatchBinding],
    pub guards: &'a [(u32, LitPattern)],
    pub body: TypedMatchBody<'a>,
}

// ---------- Decoded view types ----------

/// Decoded view of a [`TirTag::Match`] payload (M12). Borrows the
/// `extra` arena and decodes arms on demand — construction and
/// iteration allocate nothing (I-091 discipline; contrast with
/// [`DestructureView`][crate::tir::DestructureView]'s owned fields
/// `Vec`). `ty` is the match expression's result type, read from the
/// `TypedInst.ty` slot.
#[derive(Debug)]
pub struct MatchView<'a> {
    pub scrutinee: TirRef,
    pub ty: TypeId,
    /// Arm words: `[arm_len, ...arm payload]` x `arm_count`, back to
    /// back.
    wire: &'a [u32],
    arm_count: usize,
}

impl<'a> MatchView<'a> {
    /// Number of arms.
    pub fn len(&self) -> usize {
        self.arm_count
    }

    /// `true` when there are no arms (sema diagnoses source-level;
    /// well-formed TIR always has at least the catch-all).
    pub fn is_empty(&self) -> bool {
        self.arm_count == 0
    }

    /// Decoded arm at `index` (`index < len`), in written order.
    pub fn arm(&self, index: usize) -> MatchArmView<'a> {
        debug_assert!(index < self.arm_count, "match arm index out of range");
        let mut wire = self.wire;
        for _ in 0..index {
            wire = &wire[1 + wire[match_extra::ARM_LEN] as usize..];
        }
        decode_match_arm(wire)
    }

    /// Arms in written order.
    pub fn arms(&self) -> impl Iterator<Item = MatchArmView<'a>> + 'a {
        let mut wire = self.wire;
        std::iter::from_fn(move || {
            if wire.is_empty() {
                return None;
            }
            let arm = decode_match_arm(wire);
            wire = &wire[1 + wire[match_extra::ARM_LEN] as usize..];
            Some(arm)
        })
        .take(self.arm_count)
    }
}

/// Decoded view of one typed match arm; see [`TypedMatchArm`]. The
/// binding words are borrowed from the `extra` arena; the decoded
/// [`MatchBinding`] triples are materialized on demand by
/// [`bindings`][MatchArmView::bindings] — no per-arm allocation
/// ([`EnumLitView`][crate::tir::EnumLitView] precedent).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatchArmView<'a> {
    pub discriminant: Discriminant,
    /// `3 * binding_count` binding words (`field_index, name, ty`
    /// triples).
    bindings: &'a [u32],
    /// The guard words after the consumed `guard_count` word —
    /// variable-length `(field_index, kind, payload…)` records, walked
    /// by [`guards`][MatchArmView::guards].
    guards: &'a [u32],
    guard_count: usize,
    pub body: MatchBodyView<'a>,
}

impl<'a> MatchArmView<'a> {
    /// Number of payload bindings.
    pub fn binding_count(&self) -> usize {
        self.bindings.len() / match_extra::BINDING_LEN
    }

    /// Bindings in written order. The returned iterator borrows the
    /// wire (lifetime `'a`, independent of the arm view value), so
    /// cross-arm adapters like `view.arms().flat_map(|a| a.bindings())`
    /// type-check; decoding is lazy and allocates nothing.
    pub fn bindings(&self) -> MatchBindingsIter<'a> {
        MatchBindingsIter {
            chunks: self
                .bindings
                .as_chunks::<{ match_extra::BINDING_LEN }>()
                .0
                .iter(),
        }
    }

    /// Number of anon-pattern literal guards (`{x=0}` field
    /// constraints). Zero for wildcard / literal / variant arms.
    pub fn guard_count(&self) -> usize {
        self.guard_count
    }

    /// Guards in written order — `(canonical field index, literal)`
    /// pairs, decoded lazily with no allocation (the guard words are
    /// variable-length, so the count rides the view rather than the
    /// slice). The iterator borrows the wire with lifetime `'a`, the
    /// [`bindings`][MatchArmView::bindings] discipline.
    pub fn guards(&self) -> MatchGuardIter<'a> {
        MatchGuardIter { wire: self.guards }
    }
}

/// Iterator over one arm's decoded [`MatchBinding`]s; see
/// [`MatchArmView::bindings`]. Wraps the borrowed binding words and
/// materializes each `(field_index, name, ty)` triple on demand.
pub struct MatchBindingsIter<'a> {
    chunks: std::slice::Iter<'a, [u32; match_extra::BINDING_LEN]>,
}

impl Iterator for MatchBindingsIter<'_> {
    type Item = MatchBinding;

    fn next(&mut self) -> Option<MatchBinding> {
        let &[field_index, name, ty] = self.chunks.next()?;
        Some(MatchBinding {
            field_index,
            name: StringId::from_raw(name),
            ty: TypeId::from_raw(ty),
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.chunks.size_hint()
    }
}

impl ExactSizeIterator for MatchBindingsIter<'_> {}

/// Iterator over one arm's decoded anon-pattern literal guards (M12);
/// see [`MatchArmView::guards`]. Wraps the borrowed guard words and
/// materializes each `(field_index, LitPattern)` pair on demand —
/// variable-length records (int payloads are two words), so iteration
/// walks widths rather than chunking.
pub struct MatchGuardIter<'a> {
    wire: &'a [u32],
}

impl Iterator for MatchGuardIter<'_> {
    type Item = (u32, LitPattern);

    fn next(&mut self) -> Option<Self::Item> {
        if self.wire.is_empty() {
            return None;
        }
        let field_index = self.wire[match_extra::GUARD_FIELD];
        let (lit, used) = match self.wire[match_extra::GUARD_KIND] {
            match_extra::GUARD_INT => {
                let lo = self.wire[2] as u64;
                let hi = self.wire[3] as u64;
                (
                    LitPattern::Int((hi << 32 | lo) as i64),
                    match_extra::GUARD_INT_WORDS,
                )
            }
            match_extra::GUARD_STR => (
                LitPattern::Str(StringId::from_raw(self.wire[2])),
                match_extra::GUARD_STR_WORDS,
            ),
            match_extra::GUARD_BOOL => (
                LitPattern::Bool(self.wire[2] != 0),
                match_extra::GUARD_BOOL_WORDS,
            ),
            other => unreachable!("unknown match guard literal kind {other}"),
        };
        self.wire = &self.wire[used..];
        Some((field_index, lit))
    }
}

/// Decoded view of a typed match arm's body (M12). `Stmts` borrows the
/// wire slice of stmt refs directly: the decoder consumed the count
/// word, so the slice length IS the statement count and iterating
/// allocates nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchBodyView<'a> {
    Expr(TirRef),
    Stmts(&'a [u32]),
}

impl MatchBodyView<'_> {
    /// The stmt refs of a statement-list body in order (empty for an
    /// expr body).
    pub fn stmts(&self) -> impl Iterator<Item = TirRef> + '_ {
        let words: &[u32] = match self {
            MatchBodyView::Expr(_) => &[],
            MatchBodyView::Stmts(w) => w,
        };
        words.iter().copied().map(TirRef::from_raw)
    }
}

// ---------- Builder ----------

impl TirBuilder {
    /// Emit a typed `match scrutinee:` expression (M12). `ty` is the
    /// match expression's result type (the join of the arm bodies);
    /// `arms` is copied verbatim into `extra` — see the module header
    /// for the wire layout. Sema decodes with [`Tir::match_view`].
    ///
    /// OWNER-TOKEN CONVENTION (sema's obligation, checked by
    /// [`Tir::match_by_move_owner_refs`]): immediately before this
    /// instruction, sema emits exactly one `FieldAccess` instruction
    /// per BY-MOVE binding — `!pool.is_copy(binding.ty)` — in arm
    /// order, then binding order within each arm:
    /// `FieldAccess(scrutinee, binding.field_index, binding.ty)`, one
    /// `TirRef` per by-move binding. Copy bindings (e.g. `int`
    /// payloads) skip the token: they copy out of the scrutinee rather
    /// than move. The ownership pass adopts those refs as the fresh
    /// owners of the moved-out fields; codegen never evaluates them.
    pub fn match_expr(
        &mut self,
        scrutinee: TirRef,
        ty: TypeId,
        arms: &[TypedMatchArm<'_>],
        span: Span,
    ) -> TirRef {
        // The only Match producer: stamps `contains_match` on the built
        // body so post-sema analyses can skip every match-shaped scan
        // on match-free bodies at the cost of one bool load.
        self.saw_match = true;
        let offset = self.extra_offset();
        self.extra.push(scrutinee.raw());
        self.extra.push(Self::len_u32(arms.len()));
        for arm in arms {
            let arm_len_slot = self.extra.len();
            self.extra.push(0); // arm_len, patched after the arm
            match arm.discriminant {
                Discriminant::Variant(index) => {
                    self.extra.push(match_extra::DISC_VARIANT);
                    self.extra.push(index);
                }
                Discriminant::Int(v) => {
                    self.extra.push(match_extra::DISC_INT);
                    let bits = v as u64;
                    self.extra.push(bits as u32);
                    self.extra.push((bits >> 32) as u32);
                }
                Discriminant::Str(s) => {
                    self.extra.push(match_extra::DISC_STR);
                    self.extra.push(s.raw());
                }
                Discriminant::Bool(b) => {
                    self.extra.push(match_extra::DISC_BOOL);
                    self.extra.push(u32::from(b));
                }
                Discriminant::Wildcard => {
                    self.extra.push(match_extra::DISC_WILDCARD);
                }
            }
            self.extra.push(Self::len_u32(arm.bindings.len()));
            for b in arm.bindings {
                self.extra.push(b.field_index);
                self.extra.push(b.name.raw());
                self.extra.push(b.ty.raw());
            }
            self.extra.push(Self::len_u32(arm.guards.len()));
            for &(field_index, lit) in arm.guards {
                self.extra.push(field_index);
                match lit {
                    LitPattern::Int(v) => {
                        self.extra.push(match_extra::GUARD_INT);
                        let bits = v as u64;
                        self.extra.push(bits as u32);
                        self.extra.push((bits >> 32) as u32);
                    }
                    LitPattern::Str(s) => {
                        self.extra.push(match_extra::GUARD_STR);
                        self.extra.push(s.raw());
                    }
                    LitPattern::Bool(b) => {
                        self.extra.push(match_extra::GUARD_BOOL);
                        self.extra.push(u32::from(b));
                    }
                }
            }
            match arm.body {
                TypedMatchBody::Expr(r) => {
                    self.extra.push(match_extra::BODY_EXPR);
                    self.extra.push(r.raw());
                }
                TypedMatchBody::Stmts(stmts) => {
                    self.extra.push(match_extra::BODY_STMTS);
                    self.extra.push(Self::len_u32(stmts.len()));
                    for r in stmts {
                        self.extra.push(r.raw());
                    }
                }
            }
            let arm_len = Self::len_u32(self.extra.len() - arm_len_slot - 1);
            self.extra[arm_len_slot] = arm_len;
        }
        let len = Self::len_u32(self.extra.len() - offset as usize);
        self.push(
            TirTag::Match,
            ty,
            TirData::Extra(ExtraRange { offset, len }),
            span,
        )
    }
}

// ---------- Decoder ----------

impl Tir {
    /// Decode a [`TirTag::Match`] payload (M12). Borrows `extra`;
    /// allocates nothing — arms decode on demand by index (see
    /// [`MatchView`]).
    pub fn match_view(&self, r: TirRef) -> MatchView<'_> {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, TirTag::Match));
        let range = match inst.data {
            TirData::Extra(rng) => rng,
            _ => unreachable!("Match must carry TirData::Extra"),
        };
        let slice = &self.extra[range.as_range()];
        let scrutinee = TirRef::from_raw(slice[match_extra::SCRUTINEE]);
        let arm_count = slice[match_extra::ARM_COUNT] as usize;
        let wire = &slice[match_extra::ARMS_START..];
        #[cfg(debug_assertions)]
        {
            let mut pos = 0;
            for _ in 0..arm_count {
                debug_assert!(pos < wire.len(), "match arm_count exceeds wire");
                pos += 1 + wire[pos] as usize;
            }
            debug_assert_eq!(pos, wire.len(), "match payload length mismatch");
        }
        MatchView {
            scrutinee,
            ty: inst.ty,
            wire,
            arm_count,
        }
    }

    /// The owner tokens of a typed `Match` (M12): one [`TirRef`] per
    /// by-move binding (`!pool.is_copy(binding.ty)`), in arm order,
    /// then binding order within each arm — the `FieldAccess`
    /// instructions sema emitted immediately before the Match inst
    /// (see [`TirBuilder::match_expr`]'s convention). The ownership
    /// pass registers these as the fresh owners of the moved-out
    /// fields; codegen's free-path redirect maps them to the bindings'
    /// slots.
    pub fn match_by_move_owner_refs(&self, r: TirRef, pool: &InternPool) -> Vec<TirRef> {
        let view = self.match_view(r);
        let n_moves = view
            .arms()
            .flat_map(|arm| arm.bindings())
            .filter(|b| !pool.is_copy(b.ty))
            .count();
        debug_assert!(
            n_moves <= r.index(),
            "match owner tokens precede the instruction"
        );
        let mut cursor = r.index() - n_moves;
        let mut out = Vec::with_capacity(n_moves);
        for arm in view.arms() {
            for b in arm.bindings() {
                if pool.is_copy(b.ty) {
                    continue;
                }
                let owner = TirRef::from_index(cursor);
                cursor += 1;
                debug_assert!(
                    matches!(self.inst(owner).tag, TirTag::FieldAccess),
                    "match owner token must be a FieldAccess"
                );
                debug_assert_eq!(
                    self.inst(owner).ty,
                    b.ty,
                    "match owner token carries the binding type"
                );
                out.push(owner);
            }
        }
        out
    }

    /// The per-body memo backing [`Tir::subtree_has_match`]: slot `i` is
    /// `true` when the operand subtree rooted at instruction `i`
    /// contains a `Match`. Computed once per body on first query — a
    /// single forward pass exploiting the arena's def-before-use order
    /// (every operand child has a smaller index than its parent, which
    /// [`TirBuilder::finish`]'s tree-shape validation plus codegen's
    /// recursive evaluation both rely on). Not serialized: a clone
    /// recomputes on first query.
    pub fn operand_match_memo(&self) -> &[bool] {
        self.operand_match_memo
            .get_or_init(|| {
                let mut memo = vec![false; self.instructions.len()];
                for i in 1..self.instructions.len() {
                    let inst = &self.instructions[i];
                    if inst.tag == TirTag::Match {
                        memo[i] = true;
                        continue;
                    }
                    let r = TirRef::from_index(i);
                    let mut has = false;
                    self.walk_operands(r, &mut |_parent, child, kind| {
                        debug_assert!(
                            child.index() < i,
                            "operand child precedes its parent (def-before-use)"
                        );
                        if kind == ChildKind::Operand {
                            has |= memo[child.index()];
                        }
                    });
                    memo[i] = has;
                }
                memo.into_boxed_slice()
            })
            .as_ref()
    }

    /// `true` when the operand subtree rooted at `r` contains a
    /// `Match`. The shared traversal contract for every post-sema
    /// analysis that must notice value-position matches (a `VarDecl`
    /// initializer, an `Assign` value, a `Return` operand, a call
    /// argument): query this before any operand-descent scan so
    /// match-free subtrees cost one lookup instead of a full walk.
    /// Statement-arm bodies are NOT operand-reachable and are never
    /// flagged; statement-list walkers reach those through their own
    /// recursion ([`Tir::match_view`] arms).
    pub fn subtree_has_match(&self, r: TirRef) -> bool {
        self.operand_match_memo()[r.index()]
    }
}

/// Decode one length-prefixed arm out of the arm region. `slice` is
/// the remaining arm words starting at this arm — its first word is
/// `arm_len`, and exactly `arm_len` payload words follow — so the
/// decoded arm occupies `slice[..=arm_len]` and the next arm (if any)
/// begins at `slice[1 + arm_len..]`. A cursor that ends at
/// `1 + arm_len` is a trusted-producer invariant.
fn decode_match_arm(slice: &[u32]) -> MatchArmView<'_> {
    let arm_words = 1 + slice[match_extra::ARM_LEN] as usize;
    debug_assert!(
        slice.len() >= arm_words,
        "match arm_len exceeds the remaining wire"
    );
    let mut pos = match_extra::DISC_PAYLOAD;
    let discriminant = match slice[match_extra::DISC_KIND] {
        match_extra::DISC_VARIANT => {
            let d = Discriminant::Variant(slice[pos]);
            pos += 1;
            d
        }
        match_extra::DISC_INT => {
            let lo = slice[pos] as u64;
            let hi = slice[pos + 1] as u64;
            pos += 2;
            Discriminant::Int((hi << 32 | lo) as i64)
        }
        match_extra::DISC_STR => {
            let d = Discriminant::Str(StringId::from_raw(slice[pos]));
            pos += 1;
            d
        }
        match_extra::DISC_BOOL => {
            let d = Discriminant::Bool(slice[pos] != 0);
            pos += 1;
            d
        }
        match_extra::DISC_WILDCARD => Discriminant::Wildcard,
        other => unreachable!("unknown match discriminant kind {other}"),
    };
    let n_bindings = slice[pos + match_extra::BIND_COUNT] as usize;
    let bindings_start = pos + match_extra::BINDINGS;
    let bindings_end = bindings_start + n_bindings * match_extra::BINDING_LEN;
    let bindings = &slice[bindings_start..bindings_end];
    pos = bindings_end;
    let guard_count = slice[pos + match_extra::GUARD_COUNT] as usize;
    let guards_start = pos + 1;
    let mut guards_end = guards_start;
    for _ in 0..guard_count {
        // Guards are variable-width — walk the widths to find the body.
        guards_end += match slice[guards_end + match_extra::GUARD_KIND] {
            match_extra::GUARD_INT => match_extra::GUARD_INT_WORDS,
            match_extra::GUARD_STR => match_extra::GUARD_STR_WORDS,
            match_extra::GUARD_BOOL => match_extra::GUARD_BOOL_WORDS,
            other => unreachable!("unknown match guard literal kind {other}"),
        };
    }
    let guards = &slice[guards_start..guards_end];
    pos = guards_end;
    let body = match slice[pos] {
        match_extra::BODY_EXPR => {
            let body = MatchBodyView::Expr(TirRef::from_raw(slice[pos + 1]));
            pos += 2;
            body
        }
        match_extra::BODY_STMTS => {
            let count = slice[pos + 1] as usize;
            let body = MatchBodyView::Stmts(&slice[pos + 2..pos + 2 + count]);
            pos += 2 + count;
            body
        }
        other => unreachable!("unknown match body kind {other}"),
    };
    debug_assert_eq!(pos, arm_words, "match arm length mismatch");
    MatchArmView {
        discriminant,
        bindings,
        guards,
        guard_count,
        body,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tir::ChildKind;
    use chumsky::span::{SimpleSpan, Span as _};

    fn sp() -> Span {
        SimpleSpan::new((), 0..0)
    }

    #[test]
    fn wildcard_arm_with_expr_body_round_trips() {
        let mut pool = InternPool::new();
        let int_ty = pool.int();
        let main = pool.intern_str("main");

        let mut b = TirBuilder::new(main, vec![], int_ty, sp());
        let scrut = b.int_const(1, int_ty, sp());
        let body = b.int_const(2, int_ty, sp());
        let m = b.match_expr(
            scrut,
            int_ty,
            &[TypedMatchArm {
                discriminant: Discriminant::Wildcard,
                bindings: &[],
                guards: &[],
                body: TypedMatchBody::Expr(body),
            }],
            sp(),
        );

        let tir = b.finish(&[m]);
        assert!(matches!(tir.inst(m).tag, TirTag::Match));
        assert_eq!(tir.inst(m).ty, int_ty);
        let view = tir.match_view(m);
        assert_eq!(view.scrutinee, scrut);
        assert_eq!(view.ty, int_ty);
        assert_eq!(view.len(), 1);
        assert!(!view.is_empty());

        let arm = view.arm(0);
        assert_eq!(arm.discriminant, Discriminant::Wildcard);
        assert_eq!(arm.binding_count(), 0);
        assert!(arm.bindings().next().is_none());
        assert_eq!(arm.body, MatchBodyView::Expr(body));
        assert_eq!(arm.body.stmts().count(), 0);

        // The iterator API agrees with indexed access.
        let arms = view.arms().collect::<Vec<_>>();
        assert_eq!(arms.len(), 1);
        assert_eq!(arms[0], arm);

        // Wire header is pinned: [scrutinee, arm_count, arm_len,
        // disc_kind, binding_count, guard_count, body_kind, body_ref] —
        // a binding-less, guard-less wildcard arm is 5 payload words.
        let inst = tir.inst(m);
        let TirData::Extra(rng) = inst.data else {
            panic!("expected TirData::Extra, got {:?}", inst.data);
        };
        let wire = &tir.extra[rng.as_range()];
        assert_eq!(wire[match_extra::SCRUTINEE], scrut.raw());
        assert_eq!(wire[match_extra::ARM_COUNT], 1);
        assert_eq!(wire[match_extra::ARMS_START + match_extra::ARM_LEN], 5);
        assert_eq!(
            wire[match_extra::ARMS_START + match_extra::DISC_KIND],
            match_extra::DISC_WILDCARD
        );
    }

    #[test]
    fn variant_arm_with_bindings_and_stmt_body_round_trips() {
        let mut pool = InternPool::new();
        let int_ty = pool.int();
        let str_ty = pool.str_();
        let main = pool.intern_str("main");
        let shape = pool.intern_str("Shape");
        let v = pool.intern_str("v");
        let w = pool.intern_str("w");

        let mut b = TirBuilder::new(main, vec![], int_ty, sp());
        let scrut = b.var(shape, int_ty, sp());
        let s1 = b.int_const(1, int_ty, sp());
        let s2 = b.int_const(2, int_ty, sp());
        let bindings = [
            MatchBinding {
                field_index: 0,
                name: v,
                ty: str_ty,
            },
            MatchBinding {
                field_index: 1,
                name: w,
                ty: int_ty,
            },
        ];
        let m = b.match_expr(
            scrut,
            int_ty,
            &[TypedMatchArm {
                discriminant: Discriminant::Variant(1),
                bindings: &bindings,
                guards: &[],
                body: TypedMatchBody::Stmts(&[s1, s2]),
            }],
            sp(),
        );

        let tir = b.finish(&[m]);
        let view = tir.match_view(m);
        assert_eq!(view.scrutinee, scrut);
        assert_eq!(view.ty, int_ty);
        assert_eq!(view.len(), 1);

        let arm = view.arm(0);
        assert_eq!(arm.discriminant, Discriminant::Variant(1));
        assert_eq!(arm.binding_count(), 2);
        assert_eq!(
            arm.bindings().collect::<Vec<_>>(),
            [
                MatchBinding {
                    field_index: 0,
                    name: v,
                    ty: str_ty
                },
                MatchBinding {
                    field_index: 1,
                    name: w,
                    ty: int_ty
                }
            ]
        );
        let MatchBodyView::Stmts(words) = arm.body else {
            panic!("expected stmts body, got {:?}", arm.body);
        };
        assert_eq!(words.len(), 2);
        assert_eq!(arm.body.stmts().collect::<Vec<_>>(), [s1, s2]);

        // walk_operands announces the scrutinee as an operand and the
        // statement arm's stmts as body statements.
        let mut edges = Vec::new();
        tir.walk_operands(m, &mut |_parent, child, kind| edges.push((child, kind)));
        assert_eq!(edges.len(), 3);
        assert!(edges[0].0 == scrut && matches!(edges[0].1, ChildKind::Operand));
        assert!(edges[1].0 == s1 && matches!(edges[1].1, ChildKind::BodyStmt));
        assert!(edges[2].0 == s2 && matches!(edges[2].1, ChildKind::BodyStmt));
    }

    #[test]
    fn literal_arms_round_trip() {
        let mut pool = InternPool::new();
        let int_ty = pool.int();
        let str_ty = pool.str_();
        let quit = pool.intern_str("quit");
        let main = pool.intern_str("main");

        let mut b = TirBuilder::new(main, vec![], int_ty, sp());
        let scrut = b.str_const(quit, str_ty, sp());
        let e1 = b.int_const(10, int_ty, sp());
        let e2 = b.int_const(20, int_ty, sp());
        let e3 = b.int_const(30, int_ty, sp());
        let m = b.match_expr(
            scrut,
            int_ty,
            &[
                TypedMatchArm {
                    discriminant: Discriminant::Int(-7),
                    bindings: &[],
                    guards: &[],
                    body: TypedMatchBody::Expr(e1),
                },
                TypedMatchArm {
                    discriminant: Discriminant::Str(quit),
                    bindings: &[],
                    guards: &[],
                    body: TypedMatchBody::Expr(e2),
                },
                TypedMatchArm {
                    discriminant: Discriminant::Bool(true),
                    bindings: &[],
                    guards: &[],
                    body: TypedMatchBody::Expr(e3),
                },
            ],
            sp(),
        );

        let tir = b.finish(&[m]);
        let view = tir.match_view(m);
        assert_eq!(view.len(), 3);
        let arms = view.arms().collect::<Vec<_>>();
        assert_eq!(arms[0].discriminant, Discriminant::Int(-7));
        assert_eq!(arms[1].discriminant, Discriminant::Str(quit));
        assert_eq!(arms[2].discriminant, Discriminant::Bool(true));
        assert_eq!(arms[0].body, MatchBodyView::Expr(e1));
        assert_eq!(arms[1].body, MatchBodyView::Expr(e2));
        assert_eq!(arms[2].body, MatchBodyView::Expr(e3));
        // walk_operands announces the scrutinee plus the three expr
        // bodies, all as operands.
        let mut edges = Vec::new();
        tir.walk_operands(m, &mut |_parent, child, kind| edges.push((child, kind)));
        assert_eq!(edges.len(), 4);
        assert!(edges[0].0 == scrut && matches!(edges[0].1, ChildKind::Operand));
        for (i, &e) in [e1, e2, e3].iter().enumerate() {
            assert!(edges[i + 1].0 == e && matches!(edges[i + 1].1, ChildKind::Operand));
        }
    }

    #[test]
    fn anon_arm_guards_round_trip_variable_width_literals() {
        let mut pool = InternPool::new();
        let int_ty = pool.int();
        let quit = pool.intern_str("quit");
        let main = pool.intern_str("main");
        let px = pool.intern_str("px");

        let mut b = TirBuilder::new(main, vec![], int_ty, sp());
        let scrut = b.var(main, int_ty, sp());
        let body = b.int_const(1, int_ty, sp());
        let fallback = b.int_const(0, int_ty, sp());
        let guards = [
            (0, LitPattern::Int(-7)),
            (1, LitPattern::Str(quit)),
            (2, LitPattern::Bool(true)),
        ];
        let anon_bindings = [MatchBinding {
            field_index: 3,
            name: px,
            ty: int_ty,
        }];
        let m = b.match_expr(
            scrut,
            int_ty,
            &[
                TypedMatchArm {
                    discriminant: Discriminant::Wildcard,
                    bindings: &anon_bindings,
                    guards: &guards,
                    body: TypedMatchBody::Expr(body),
                },
                TypedMatchArm {
                    discriminant: Discriminant::Wildcard,
                    bindings: &[],
                    guards: &[],
                    body: TypedMatchBody::Expr(fallback),
                },
            ],
            sp(),
        );
        let tir = b.finish(&[m]);

        let view = tir.match_view(m);
        let arm0 = view.arm(0);
        assert_eq!(arm0.guard_count(), 3);
        assert_eq!(
            arm0.guards().collect::<Vec<_>>(),
            [
                (0, LitPattern::Int(-7)),
                (1, LitPattern::Str(quit)),
                (2, LitPattern::Bool(true)),
            ],
            "guards decode in written order, negative ints pre-folded"
        );
        assert_eq!(
            arm0.bindings().collect::<Vec<_>>(),
            [MatchBinding {
                field_index: 3,
                name: px,
                ty: int_ty
            }],
            "bindings and guards coexist on one arm"
        );
        // The non-anon arm decodes an empty guard list.
        let arm1 = view.arm(1);
        assert_eq!(arm1.guard_count(), 0);
        assert!(arm1.guards().next().is_none());
        // Guards carry no operand edges — walk_operands sees the
        // scrutinee plus the two expr bodies only.
        let mut edges = Vec::new();
        tir.walk_operands(m, &mut |_parent, child, kind| edges.push((child, kind)));
        assert_eq!(edges.len(), 3);
    }

    #[test]
    fn two_match_insts_decode_independently() {
        let mut pool = InternPool::new();
        let int_ty = pool.int();
        let str_ty = pool.str_();
        let v = pool.intern_str("v");
        let main = pool.intern_str("main");

        let mut b = TirBuilder::new(main, vec![], int_ty, sp());
        // m1: wildcard + negative-int literal arms with expr bodies.
        let s1 = b.int_const(0, int_ty, sp());
        let e1 = b.int_const(1, int_ty, sp());
        let e2 = b.int_const(2, int_ty, sp());
        let m1 = b.match_expr(
            s1,
            int_ty,
            &[
                TypedMatchArm {
                    discriminant: Discriminant::Wildcard,
                    bindings: &[],
                    guards: &[],
                    body: TypedMatchBody::Expr(e1),
                },
                TypedMatchArm {
                    discriminant: Discriminant::Int(-1),
                    bindings: &[],
                    guards: &[],
                    body: TypedMatchBody::Expr(e2),
                },
            ],
            sp(),
        );
        // m2: unit variant with a stmt body, then a variant with a
        // by-move str binding and an expr body.
        let s2 = b.bool_const(true, pool.bool_(), sp());
        let st = b.return_void(pool.void(), sp());
        let e3 = b.int_const(3, int_ty, sp());
        let binding = [MatchBinding {
            field_index: 0,
            name: v,
            ty: str_ty,
        }];
        let m2 = b.match_expr(
            s2,
            int_ty,
            &[
                TypedMatchArm {
                    discriminant: Discriminant::Variant(0),
                    bindings: &[],
                    guards: &[],
                    body: TypedMatchBody::Stmts(&[st]),
                },
                TypedMatchArm {
                    discriminant: Discriminant::Variant(1),
                    bindings: &binding,
                    guards: &[],
                    body: TypedMatchBody::Expr(e3),
                },
            ],
            sp(),
        );

        let tir = b.finish(&[m1, m2]);

        let v1 = tir.match_view(m1);
        assert_eq!(v1.scrutinee, s1);
        assert_eq!(v1.len(), 2);
        assert_eq!(v1.arm(0).discriminant, Discriminant::Wildcard);
        assert_eq!(v1.arm(1).discriminant, Discriminant::Int(-1));
        assert_eq!(v1.arm(0).body, MatchBodyView::Expr(e1));
        assert_eq!(v1.arm(1).body, MatchBodyView::Expr(e2));

        let v2 = tir.match_view(m2);
        assert_eq!(v2.scrutinee, s2);
        assert_eq!(v2.len(), 2);
        assert_eq!(v2.arm(0).discriminant, Discriminant::Variant(0));
        assert_eq!(v2.arm(0).body.stmts().collect::<Vec<_>>(), [st]);
        let arm1 = v2.arm(1);
        assert_eq!(arm1.discriminant, Discriminant::Variant(1));
        assert_eq!(
            arm1.bindings().collect::<Vec<_>>(),
            [MatchBinding {
                field_index: 0,
                name: v,
                ty: str_ty
            }]
        );
        assert_eq!(arm1.body, MatchBodyView::Expr(e3));

        // The ranges are independent: m1 still decodes exactly after
        // m2 was decoded (and vice versa — views borrow, so both stay
        // valid together).
        let v1_again = tir.match_view(m1);
        assert_eq!(v1_again.len(), 2);
        assert_eq!(v1_again.arm(1).discriminant, Discriminant::Int(-1));
    }

    #[test]
    fn match_by_move_owner_tokens_recovered_by_counting_back() {
        let mut pool = InternPool::new();
        let int_ty = pool.int();
        let str_ty = pool.str_();
        let main = pool.intern_str("main");
        let shape = pool.intern_str("Shape");
        let v = pool.intern_str("v");
        let w = pool.intern_str("w");
        let x = pool.intern_str("x");

        let mut b = TirBuilder::new(main, vec![], int_ty, sp());
        let scrut = b.var(shape, int_ty, sp());
        let body1 = b.int_const(1, int_ty, sp());
        let body2 = b.int_const(2, int_ty, sp());
        let arm0_bindings = [
            MatchBinding {
                field_index: 0,
                name: v,
                ty: str_ty, // by-move: not Copy
            },
            MatchBinding {
                field_index: 1,
                name: w,
                ty: int_ty, // copy: no owner token
            },
        ];
        let arm1_bindings = [MatchBinding {
            field_index: 0,
            name: x,
            ty: str_ty,
        }];
        // Owner tokens: one FieldAccess per by-move binding, arm order
        // then binding order, immediately before the Match.
        let tok0 = b.field_access(scrut, 0, str_ty, sp());
        let tok1 = b.field_access(scrut, 0, str_ty, sp());
        let m = b.match_expr(
            scrut,
            int_ty,
            &[
                TypedMatchArm {
                    discriminant: Discriminant::Variant(0),
                    bindings: &arm0_bindings,
                    guards: &[],
                    body: TypedMatchBody::Expr(body1),
                },
                TypedMatchArm {
                    discriminant: Discriminant::Variant(1),
                    bindings: &arm1_bindings,
                    guards: &[],
                    body: TypedMatchBody::Expr(body2),
                },
            ],
            sp(),
        );
        assert_eq!(tok0.index(), m.index() - 2);
        assert_eq!(tok1.index(), m.index() - 1);

        let tir = b.finish(&[m]);
        let owners = tir.match_by_move_owner_refs(m, &pool);
        assert_eq!(owners, vec![tok0, tok1]);
    }
}
