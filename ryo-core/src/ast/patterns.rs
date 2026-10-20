//! Destructuring and match patterns (M10, M12).
//!
//! Split out of `ast.rs` to stay under the file-length limit (R2).
//! Patterns live in the `patterns` arena declared on [`Ast`]; this
//! module holds the pattern node types, their list-range handles, and
//! the `Ast` builder/reader methods for them.
//!
//! M10 brought destructuring patterns (`_`, bindings, `{q, r}` / `{x =
//! quot}` field shapes, positional `(a, b)`). M12 widens the same nodes
//! for match arms: literal patterns (`42`, `"quit"`, `true`), enum
//! variant patterns (`Option.Some(v)`, `Red`, `Coords{x=px}`), and
//! literal/wildcard field targets in anonymous shapes (`{x=0, y=_}`).
//! The M10 destructuring STATEMENT keeps its bind-only grammar —
//! literal and variant kinds parse only in match arms.

use super::{Ast, ExprId, Ident, PatternId, StmtList};
use crate::span::Span;
use crate::types::StringId;

/// A `[offset, offset+len)` slice of the `pattern_lists` side arena —
/// the element list of a [`PatternKind::Positional`] pattern `(a, b)`,
/// in written order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PatternList {
    offset: u32,
    len: u32,
}

impl PatternList {
    pub(super) fn as_range(self) -> std::ops::Range<usize> {
        let start = self.offset as usize;
        start..start + self.len as usize
    }
}

/// A `[offset, offset+len)` slice of the `pattern_field_lists` side
/// arena — the field list of a [`PatternKind::Anon`] pattern
/// `{q, r}` / `{x = quot}`, in written order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PatternFieldList {
    offset: u32,
    len: u32,
}

impl PatternFieldList {
    pub(super) fn as_range(self) -> std::ops::Range<usize> {
        let start = self.offset as usize;
        start..start + self.len as usize
    }
}

/// A `[offset, offset+len)` slice of the `variant_arg_lists` side
/// arena — the payload argument list of a [`PatternKind::Variant`]
/// pattern (`Circle(radius)`, `Coords{x=px}`), in written order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PatternVariantArgList {
    offset: u32,
    len: u32,
}

impl PatternVariantArgList {
    pub(super) fn as_range(self) -> std::ops::Range<usize> {
        let start = self.offset as usize;
        start..start + self.len as usize
    }
}

/// A `[offset, offset+len)` slice of the `match_arms` side arena — the
/// arm list of an [`ExprKind::Match`][super::ExprKind::Match], in
/// written order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatchArmList {
    offset: u32,
    len: u32,
}

impl MatchArmList {
    pub(super) fn as_range(self) -> std::ops::Range<usize> {
        let start = self.offset as usize;
        start..start + self.len as usize
    }
}

/// A single destructuring or match pattern: kind plus inline source
/// span. Mirrors [`Expr`][super::Expr]: the arena node carries its
/// span, and consumers match on `ast.pattern(id).kind`.
///
/// Grammar (M10): `_`, a binding, `{q, r}` / `{x = quot}` field
/// patterns, and positional `(a, b)` — including the statement-level
/// paren-less form `a, b = e`, which the parser desugars into a
/// positional pattern. M12 adds literal patterns (`42`, `"quit"`,
/// `true`) and enum variant patterns (`Option.Some(v)`, `Red`) for
/// match arms.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pattern {
    pub kind: PatternKind,
    pub span: Span,
}

/// The kind of a destructuring or match pattern. Child lists are
/// [`PatternList`] / [`PatternFieldList`] / [`PatternVariantArgList`]
/// ranges into the `pattern_lists` / `pattern_field_lists` /
/// `variant_arg_lists` side arenas; like
/// [`ExprKind`][super::ExprKind], every payload field is `Copy`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PatternKind {
    /// `_` — binds nothing, skips the field/element. `_` lexes as an
    /// ordinary identifier; the parser recognizes it by name.
    Wildcard,
    /// A fresh binding for one field/element (`a`). Whole-arm bare
    /// bindings are rejected in match arms (only payload positions
    /// bind); the statement-level destructure grammar keeps them.
    Bind(Ident),
    /// `{q, r}` / `{x = quot}` / `{x=0, y=_}` — an anonymous struct
    /// shape: bare fields are puns (field `q` binds local `q`), `name
    /// = binding` renames, `name = literal` matches a literal value,
    /// `name = _` skips the field. The field list lives in the
    /// `pattern_field_lists` side arena, in written order.
    Anon { fields: PatternFieldList },
    /// `(a, b)` / `(0, _)` — a positional shape; elements are full
    /// patterns, so nesting like `(a, (b, c))` parses. The element
    /// list lives in the `pattern_lists` side arena, in written order.
    Positional(PatternList),
    /// `42`, `-1`, `"quit"`, `true` (M12) — matches one exact literal
    /// value. Match arms only.
    Literal(LitPattern),
    /// `Option.Some(v)`, `Red`, `Coords{x=px}` (M12) — matches one
    /// enum variant. `enum_name` is `None` for the unqualified
    /// spelling (`Red`), leaving the enum for sema to infer. `args` is
    /// `None` for unit variants; otherwise the payload bind list lives
    /// in the `variant_arg_lists` side arena, in written order.
    Variant {
        enum_name: Option<StringId>,
        variant: StringId,
        args: Option<PatternVariantArgs>,
    },
}

/// A literal match pattern (M12): an exact `int` / `str` / `bool`
/// value. Match arms only — the destructuring statement grammar cannot
/// produce this kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LitPattern {
    Int(i64),
    Str(StringId),
    Bool(bool),
}

/// The payload argument list of a [`PatternKind::Variant`] pattern
/// (M12): parenthesized positional binds (`Circle(radius)`) or braced
/// named binds (`Coords{x=px}`). `by_position` records which spelling
/// the arm used; the elements live in the `variant_arg_lists` side
/// arena, in written order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PatternVariantArgs {
    pub by_position: bool,
    pub list: PatternVariantArgList,
}

/// One payload argument of a [`PatternKind::Variant`] pattern. Paren
/// (positional) binds carry `name: None`; brace (named) binds carry the
/// field name. Payload literals are deferred with nested patterns —
/// the target binds or skips only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PatternVariantArg {
    pub name: Option<StringId>,
    pub target: PatternVariantArgTarget,
}

/// What a [`PatternVariantArg`] does with its payload slot: `_` skips
/// it, an identifier binds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatternVariantArgTarget {
    Wildcard,
    Bind(StringId),
}

/// One field of a [`PatternKind::Anon`] pattern. A pun (`{q, r}`)
/// records the field with [`PatternFieldKind::Pun`]; `name = binding`
/// renames ([`PatternFieldKind::Rename`]), `name = literal` matches a
/// value (M12, match arms only), and `name = _` (or a field spelled
/// `_`) skips the field ([`PatternFieldKind::Wildcard`], the same
/// name-based rule as [`PatternKind::Wildcard`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PatternField {
    /// The struct field being destructured.
    pub name: StringId,
    /// What the field does: pun, rename, literal match, or wildcard.
    pub kind: PatternFieldKind,
}

/// The target of one [`PatternField`] (M12 widens the M10
/// bind-or-wildcard pair): a pun binds a local named after the field,
/// a rename binds a differently-named local, a literal matches an
/// exact value, and a wildcard binds nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatternFieldKind {
    /// `{q}` — the field binds a local of the same name.
    Pun,
    /// `{x = quot}` — field `x` binds the local `quot`.
    Rename(StringId),
    /// `{x = 0}` (M12) — field `x` must equal the literal. Match arms
    /// only: the destructuring statement grammar cannot produce this.
    Literal(LitPattern),
    /// `{y = _}` or a field spelled `_` — binds nothing.
    Wildcard,
}

/// One arm of an [`ExprKind::Match`][super::ExprKind::Match]
/// (M12): `pattern: body`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MatchArm {
    pub pattern: PatternId,
    pub body: MatchArmBody,
}

/// The body of a [`MatchArm`] (M12). A single-line expression arm
/// (`Some(v): v + 1`) stays an [`Expr`][MatchArmBody::Expr]; statement
/// arms (`Err(e): return e`, and every indented block arm) normalize
/// to a one-or-more-element statement list — the parser wraps
/// single-line statements, so no `return`-as-expression form is
/// needed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MatchArmBody {
    Expr(ExprId),
    Stmts(StmtList),
}

impl Ast {
    /// Lookup a destructuring pattern by id.
    pub fn pattern(&self, id: PatternId) -> &Pattern {
        &self.patterns[id.index()]
    }

    /// Source span attached to a destructuring pattern.
    pub fn pattern_span(&self, id: PatternId) -> Span {
        self.patterns[id.index()].span
    }

    /// The element slice behind a [`PatternList`] range — the
    /// elements of a [`PatternKind::Positional`] pattern — in written
    /// order.
    pub fn pattern_list(&self, list: PatternList) -> &[PatternId] {
        &self.pattern_lists[list.as_range()]
    }

    /// The field slice behind a [`PatternFieldList`] range — the
    /// fields of a [`PatternKind::Anon`] pattern — in written order.
    pub fn pattern_field_list(&self, list: PatternFieldList) -> &[PatternField] {
        &self.pattern_field_lists[list.as_range()]
    }

    /// The argument slice behind a [`PatternVariantArgList`] range —
    /// the payload binds of a [`PatternKind::Variant`] pattern — in
    /// written order.
    pub fn variant_arg_list(&self, list: PatternVariantArgList) -> &[PatternVariantArg] {
        &self.variant_arg_lists[list.as_range()]
    }

    /// The arm slice behind a [`MatchArmList`] range, in written
    /// order.
    pub fn match_arms(&self, list: MatchArmList) -> &[MatchArm] {
        &self.match_arms[list.as_range()]
    }

    /// Push a pattern with its span and return its id; see
    /// [`Ast::push_expr`]. Slot 0 of `patterns` is the reserved
    /// sentinel.
    fn push_pattern(&mut self, kind: PatternKind, span: Span) -> PatternId {
        let idx = self.patterns.len();
        self.patterns.push(Pattern { kind, span });
        PatternId::from_index(idx)
    }

    /// Copy a positional pattern's element list into the
    /// `pattern_lists` side arena; see [`Ast::push_expr_list`] for
    /// the checked-conversion rationale.
    fn push_pattern_list(&mut self, items: &[PatternId]) -> PatternList {
        let offset = u32::try_from(self.pattern_lists.len())
            .expect("AST pattern_lists arena exceeded u32::MAX");
        self.pattern_lists.extend_from_slice(items);
        PatternList {
            offset,
            len: u32::try_from(items.len()).expect("AST pattern list length exceeded u32::MAX"),
        }
    }

    /// Copy an anonymous pattern's field list into the
    /// `pattern_field_lists` side arena; see [`Ast::push_expr_list`]
    /// for the checked-conversion rationale.
    fn push_pattern_field_list(&mut self, items: &[PatternField]) -> PatternFieldList {
        let offset = u32::try_from(self.pattern_field_lists.len())
            .expect("AST pattern_field_lists arena exceeded u32::MAX");
        self.pattern_field_lists.extend_from_slice(items);
        PatternFieldList {
            offset,
            len: u32::try_from(items.len())
                .expect("AST pattern field list length exceeded u32::MAX"),
        }
    }

    /// Copy a variant pattern's payload argument list into the
    /// `variant_arg_lists` side arena; see [`Ast::push_expr_list`]
    /// for the checked-conversion rationale.
    fn push_variant_arg_list(&mut self, items: &[PatternVariantArg]) -> PatternVariantArgList {
        let offset = u32::try_from(self.variant_arg_lists.len())
            .expect("AST variant_arg_lists arena exceeded u32::MAX");
        self.variant_arg_lists.extend_from_slice(items);
        PatternVariantArgList {
            offset,
            len: u32::try_from(items.len()).expect("AST variant arg list length exceeded u32::MAX"),
        }
    }

    /// Copy a match arm list into the `match_arms` side arena; see
    /// [`Ast::push_expr_list`] for the checked-conversion rationale.
    fn push_match_arm_list(&mut self, items: &[MatchArm]) -> MatchArmList {
        let offset =
            u32::try_from(self.match_arms.len()).expect("AST match_arms arena exceeded u32::MAX");
        self.match_arms.extend_from_slice(items);
        MatchArmList {
            offset,
            len: u32::try_from(items.len()).expect("AST match arm list length exceeded u32::MAX"),
        }
    }

    /// `_` wildcard pattern (M10): binds nothing.
    pub fn pattern_wildcard(&mut self, span: Span) -> PatternId {
        self.push_pattern(PatternKind::Wildcard, span)
    }

    /// A fresh binding pattern `name` (M10).
    pub fn pattern_bind(&mut self, name: Ident, span: Span) -> PatternId {
        self.push_pattern(PatternKind::Bind(name), span)
    }

    /// `{q, r}` / `{x = quot}` / `{x=0, y=_}` anonymous struct
    /// pattern (M10, literal fields M12); the field list is copied
    /// into the `pattern_field_lists` side arena in written order.
    pub fn pattern_anon(&mut self, fields: &[PatternField], span: Span) -> PatternId {
        let fields = self.push_pattern_field_list(fields);
        self.push_pattern(PatternKind::Anon { fields }, span)
    }

    /// `(a, b)` positional pattern (M10) — also the shape the parser
    /// builds for the paren-less `a, b = e` statement form; the
    /// element list is copied into the `pattern_lists` side arena in
    /// written order.
    pub fn pattern_positional(&mut self, elems: &[PatternId], span: Span) -> PatternId {
        let elems = self.push_pattern_list(elems);
        self.push_pattern(PatternKind::Positional(elems), span)
    }

    /// A literal match pattern `42` / `-1` / `"quit"` / `true` (M12);
    /// match arms only.
    pub fn literal_pattern(&mut self, lit: LitPattern, span: Span) -> PatternId {
        self.push_pattern(PatternKind::Literal(lit), span)
    }

    /// An enum variant match pattern `Option.Some(v)` / `Red` /
    /// `Coords{x=px}` (M12); the payload argument list is copied into
    /// the `variant_arg_lists` side arena in written order.
    pub fn variant_pattern(
        &mut self,
        enum_name: Option<StringId>,
        variant: StringId,
        args: Option<PatternVariantArgs>,
        span: Span,
    ) -> PatternId {
        self.push_pattern(
            PatternKind::Variant {
                enum_name,
                variant,
                args,
            },
            span,
        )
    }

    /// The payload argument list of a variant match pattern (M12):
    /// `Circle(radius)` is `by_position`, `Coords{x=px}` is named. The
    /// argument list is copied into the `variant_arg_lists` side arena
    /// in written order.
    pub fn variant_pattern_args(
        &mut self,
        by_position: bool,
        args: &[PatternVariantArg],
    ) -> PatternVariantArgs {
        PatternVariantArgs {
            by_position,
            list: self.push_variant_arg_list(args),
        }
    }

    /// One arm of a `match` expression (M12): `pattern: body`. Scalar
    /// payload — the [`Ast::match_expr`] builder copies the arm list
    /// into the `match_arms` side arena.
    pub fn match_arm(&mut self, pattern: PatternId, body: MatchArmBody, _span: Span) -> MatchArm {
        MatchArm { pattern, body }
    }

    /// `match scrutinee:` expression (M12); the arm list is copied
    /// into the `match_arms` side arena in written order.
    pub fn match_expr(&mut self, scrutinee: ExprId, arms: &[MatchArm], span: Span) -> ExprId {
        let arms = self.push_match_arm_list(arms);
        self.push_expr(super::ExprKind::Match { scrutinee, arms }, span)
    }
}
