//! Surface-syntax parser.
//!
//! Built on chumsky over the lexer's `Token` type. Identifiers,
//! type names, and string literals come pre-interned as `StringId`
//! handles — the parser only copies handles out of those tokens.
//! The one exception is positional field keys (`{0=17}`, `pair.0`):
//! the canonical field name is the numeric VALUE in decimal (`007`
//! → `"7"`), a string the lexer never sees, so the parse state
//! carries the intern pool to mint it (see [`ParseState`]).
//!
//! The parser builds directly into the [`Ast`] arenas: the state
//! threaded through `parse_with_state` is [`ParseState`], which
//! derefs to the `Ast`, and node-producing combinators push
//! `Expr`/`Stmt` values through `e.state()` inside
//! `map_with`/`foldl_with` closures, yielding [`ExprId`]s /
//! [`StmtId`]s. The arenas are append-only: a backtracking
//! alternative that pushed nodes before failing leaves them behind
//! as unreachable orphans (the `Inspector` hooks on `ParseState` are
//! no-ops by design — snapshotting and truncating on every rewind
//! was measured to cost ~20% of parse time, and orphan nodes are
//! never reachable from `top_level`).

use chumsky::{
    input::{MapExtra, ValueInput},
    prelude::*,
    recovery::via_parser,
    span::SimpleSpan,
};

use crate::lexer::Token;
use ryo_core::ast::*;
use ryo_core::diag::ParseDiag;
use ryo_core::tir::ParamMode;
use ryo_core::types::{InternPool, StringId, VariantKind};

// The statement/declaration grammar lives in `statements.rs`; its
// `pub(super)` items are re-globbed here so the sibling grammar
// modules (`match_`, `enums`, …) keep resolving them through their
// existing `use super::*;`.
use statements::*;

// Stable entry point — external callers use `crate::parser::program_parser`.
pub use statements::program_parser;

/// Chumsky parse state: the [`Ast`] arenas under construction plus
/// the compilation's intern pool. Identifiers and string literals
/// arrive pre-interned from the lexer, but positional field keys
/// mint NEW strings — the canonical decimal field name — so the
/// state carries the pool to intern them.
///
/// [`Deref`]/[`DerefMut`] to `Ast` keep the existing
/// `e.state().<builder>(...)` call sites unchanged.
#[derive(Debug)]
pub struct ParseState {
    ast: Ast,
    pool: InternPool,
    /// Receiver-shape hint for the M11 postfix gates: `Some(name)`
    /// while the expression the postfix ops are folding is a bare
    /// uppercase-led identifier (`EnumName.Variant{...}` candidates
    /// and enum-flavor argument-list recovery), `None` otherwise.
    /// The postfix fold sets this after the head atom and re-derives
    /// it after every folded op; only the `.name{`-gated postfix op
    /// and the paren-arg recovery read it, and only right after such
    /// a refresh (a `{` / `=` can only follow `.name` through those
    /// ops — a speculative inner parse that wrote the hint is always
    /// followed by the next fold refresh), so the value is never
    /// stale at a read.
    postfix_head_enum: Option<StringId>,
}

impl ParseState {
    /// Fresh arenas over the lexer's pool (ownership moves back out
    /// through [`ParseState::into_parts`]).
    pub fn new(pool: InternPool) -> Self {
        ParseState {
            ast: Ast::new(),
            pool,
            postfix_head_enum: None,
        }
    }

    pub fn into_parts(self) -> (Ast, InternPool) {
        (self.ast, self.pool)
    }
}

impl std::ops::Deref for ParseState {
    type Target = Ast;
    fn deref(&self) -> &Self::Target {
        &self.ast
    }
}

impl std::ops::DerefMut for ParseState {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.ast
    }
}

/// Chumsky parser-state integration: the rewind-truncation no-op
/// `Inspector` hooks. The parser builds directly into the `Ast`
/// arenas via `map_with`/`foldl_with` closures that call
/// `e.state()`; the `ParseState` is the arena half of the parser
/// state, paired with the intern pool.
///
/// The `Inspector` hooks are deliberately no-ops. A failed
/// alternative (`or`/`choice` backtracking, `repeated` iteration
/// rollbacks, error recovery) can push nodes before it fails, and
/// those nodes stay in the arenas as unreachable orphans: the arenas
/// are append-only and ids are stable, so a rewind can never
/// invalidate a node that a kept id still references — the failed
/// branch's ids simply never escape into a kept node. Orphans cost
/// nothing downstream: consumers reach nodes only by following ids
/// out of `top_level`, and the whole `Ast` is dropped once astgen
/// has lowered it. On valid inputs speculation almost never pushes
/// before failing (alternatives die on their first token), so the
/// orphan count is zero in practice; the parser tests in
/// `arena_tests.rs` assert that over the example corpus.
///
/// The alternative — snapshotting the arena lengths in `on_save` and
/// truncating in `on_rewind`, chumsky's `TruncateState` pattern —
/// was measured to cost ~20% of total parse time
/// (`parse_large` in `ryo-frontend/benches/frontend.rs`): chumsky
/// checkpoints at every `choice`/`repeated` boundary, so the hooks
/// fire constantly even though they almost always truncate nothing.
/// Exact arenas are not worth that; the `Ast` is a transient
/// parse-to-astgen buffer, not a long-lived structure.
impl<'src, I: chumsky::input::Input<'src>> chumsky::inspector::Inspector<'src, I> for ParseState {
    type Checkpoint = ();

    fn on_token(&mut self, _: &I::Token) {}

    fn on_save<'parse>(&self, _: &chumsky::input::Cursor<'src, 'parse, I>) -> Self::Checkpoint {}

    fn on_rewind<'parse>(
        &mut self,
        _: &chumsky::input::Checkpoint<'src, 'parse, I, Self::Checkpoint>,
    ) {
    }
}

/// Parser extra: `Rich` errors carrying a typed [`ParseDiag`] payload
/// (chumsky 0.13's `RichReason::Custom(C)` parameter), the [`Ast`]
/// arena as state, no context. Every grammar rule below is
/// parameterized over it.
type PExtra<'a> = extra::Full<Rich<'a, Token, SimpleSpan, ParseDiag>, ParseState, ()>;

/// `MapExtra` with our extra config. Annotating `map_with` closure
/// parameters with it pins the `E` type parameter that `e.state()`
/// otherwise cannot infer.
type Mx<'a, 'b, I> = MapExtra<'a, 'b, I, PExtra<'a>>;

/// If `expr` is a bare identifier led by an uppercase ASCII byte —
/// the spec §1 PascalCase type convention — return it as an
/// [`Ident`]. This is the M11 disambiguation (R4): only such a
/// receiver can open enum variant construction, so
/// `Url.parse(x)`/`Shape.Rectangle{..}` construct while
/// `obj.field(...)` stays an ordinary method call. Reads the intern
/// pool, so it takes `MapExtra`, not just the `Ast`.
fn type_name_ident<'a, 'b, I>(expr: ExprId, e: &mut Mx<'a, 'b, I>) -> Option<Ident>
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    let name = match e.state().expr(expr).kind {
        ExprKind::Ident(name) => name,
        _ => return None,
    };
    let is_type_name = e
        .state()
        .pool
        .str(name)
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_uppercase);
    is_type_name.then(|| Ident::new(name, e.state().expr_span(expr)))
}

/// Canonical field name for a positional key: the numeric VALUE in
/// decimal, so `{0=17}` and `pair.007` both name field `"0"` / `"7"`
/// (the literal text is irrelevant). This is the one string the
/// lexer cannot pre-intern — canonicalization happens at parse time
/// against the state's pool.
fn positional_field_name(n: i64, pool: &mut InternPool) -> StringId {
    pool.intern_str(&n.to_string())
}

/// A struct-literal field key: an identifier, or an integer literal
/// canonicalized to its numeric value (a positional key).
fn field_key<'a, I>() -> impl Parser<'a, I, StringId, PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    select! { Token::Ident(name) => name }.or(select! { Token::IntLit(n) => n }
        .map_with(|n, e: &mut Mx<'a, '_, I>| positional_field_name(n, &mut e.state().pool)))
}

/// A field key as an `Ident` (name + span attached): an identifier, or
/// an integer literal canonicalized to its numeric value (a positional
/// key). For parsers that build `FieldAccess` chains — assignment
/// targets and `&` borrow targets — rather than struct-literal field
/// lists.
fn field_key_ident<'a, I>() -> impl Parser<'a, I, Ident, PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    select! { Token::Ident(name) => name }
        .map_with(|name, e: &mut Mx<'a, '_, I>| Ident::new(name, crate::spans::cspan(e.span())))
        .or(
            select! { Token::IntLit(n) => n }.map_with(|n, e: &mut Mx<'a, '_, I>| {
                let name = positional_field_name(n, &mut e.state().pool);
                Ident::new(name, crate::spans::cspan(e.span()))
            }),
        )
}
enum PostfixOp {
    /// `.name(args)`. The name keeps its token span: an
    /// uppercase-led receiver folds this into a
    /// `VariantConstruct` whose variant span must be the name
    /// token's, exactly like the standalone form. Args carry
    /// the `name = value` recovery markers; the fold emits the
    /// targeted diagnostic and unwraps them.
    Method(Ident, Vec<enums::ParenArg>, SimpleSpan),
    /// `.name{inits}` (M11 named variant construction); emitted
    /// only when the receiver gate passes.
    NamedConstruct(Ident, Vec<(StringId, ExprId)>, SimpleSpan),
    /// `.name{value, ...}` (M11): positional values in the brace
    /// form; E0127 fired when the op parsed, the values are
    /// promoted as positional arguments.
    RecoveredPositionalBraces(Ident, Vec<ExprId>, SimpleSpan),
    /// `.name <expr-start on the same line>` (M11): a variant
    /// construction with the parens missing (E0126); the trailing
    /// expression is recovered as the payload argument.
    RecoveredMissingParens(Ident, ExprId, SimpleSpan),
    Field(Ident, SimpleSpan),
    /// A diagnosed stray float after `.` (`pair.0.1`): the
    /// receiver is kept unchanged.
    Missing,
    Slice(Option<ExprId>, Option<ExprId>, SimpleSpan),
    Index(ExprId, SimpleSpan),
}

/// Postfix operators over a finished atom: method calls (`s.len()`),
/// field access (`p.x`, M9), slice projections `s[start:end]` (M8.4),
/// scalar indexing `s[i]` (M8.4.2), and — folded at apply time — M11
/// enum variant construction. Either slice bound may be omitted
/// (`s[start:]`, `s[:end]`, `s[:]`); `s[]` is rejected.
///
/// `EnumName.Variant(...)` / `EnumName.Variant{...}` share their token
/// shapes with method calls and field accesses, so instead of adding
/// an atom-level alternative (tried for every identifier in a
/// program), the receiver is re-examined when the op is folded: only
/// a bare uppercase-led receiver constructs (R4 — see
/// `type_name_ident`). The paren form reuses the `method_op` parse
/// and swaps the node at fold time; the brace form is its own op,
/// gated on `postfix_head_is_type` BEFORE the `{` is consumed so a
/// lowercase receiver keeps its historical
/// field-access-then-garbage parse.
///
/// Extracted from `expression_parser` (which takes the shared `expr`
/// handle, the shared `field_init`, and the atom parser as
/// parameters) to stay under the `too_many_lines` ratchet (R2).
fn postfix_parser<'a, I>(
    expr: impl Parser<'a, I, ExprId, PExtra<'a>> + Clone + 'a,
    field_init: impl Parser<'a, I, (StringId, ExprId), PExtra<'a>> + Clone + 'a,
    atom: impl Parser<'a, I, ExprId, PExtra<'a>> + Clone + 'a,
) -> impl Parser<'a, I, ExprId, PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    // The `.`-led ops parse as tails after one shared `.` (`dot_op`):
    // every atom ends its postfix loop on a failed op, and one failed
    // `.` is far cheaper than one per alternative (`with_span` below).
    let method_op = select! { Token::Ident(name) => name }
        .map_with(|name, e: &mut Mx<'a, '_, I>| Ident::new(name, crate::spans::cspan(e.span())))
        .then(
            enums::paren_arg(expr.clone())
                .separated_by(just(Token::Comma))
                .allow_trailing()
                .collect::<Vec<_>>()
                .delimited_by(just(Token::LParen), just(Token::RParen)),
        )
        .map_with(|(method, args), e: &mut Mx<'a, '_, I>| {
            PostfixOp::Method(method, args, e.span())
        });

    let named_construct_op = enums::named_construct_op(expr.clone(), field_init.clone());
    let missing_args_op = enums::missing_args_op(expr.clone());

    // Field access `p.x` (M9) and positional access `pair.0`
    // (M10). Tried after `method_op`: the method rule has the
    // longer required match (parens), so chumsky backtracks to
    // this one when no `(` follows the name.
    //
    // A chained positional access can only be spelled with the
    // inner access parenthesized — `(pair.0).1`. Bare
    // `pair.0.1` lexes as `pair . <float 0.1>`: maximal munch
    // eats `0.1` whole, and the merged float lands exactly where
    // the field key would be. The stray float is consumed, the
    // well-formed receiver is kept, and the diagnostic points at
    // the float with the parenthesized spelling.
    enum AccessKey {
        Named(Ident),
        Positional(Ident),
        StrayFloat(SimpleSpan),
    }
    let named_key =
        select! { Token::Ident(name) => name }.map_with(|name, e: &mut Mx<'a, '_, I>| {
            AccessKey::Named(Ident::new(name, crate::spans::cspan(e.span())))
        });
    let positional_key = select! { Token::IntLit(n) => n }.map_with(|n, e: &mut Mx<'a, '_, I>| {
        let name = positional_field_name(n, &mut e.state().pool);
        AccessKey::Positional(Ident::new(name, crate::spans::cspan(e.span())))
    });
    let stray_float_key = select! { Token::FloatLit(_) => () }
        .map_with(|_, e: &mut Mx<'a, '_, I>| AccessKey::StrayFloat(e.span()));
    let field_op = named_key.or(positional_key).or(stray_float_key).validate(
        |key, e: &mut Mx<'a, '_, I>, emitter| match key {
            AccessKey::StrayFloat(fspan) => {
                emitter.emit(Rich::custom(fspan, ParseDiag::ChainedPositionalAccess));
                PostfixOp::Missing
            }
            AccessKey::Named(field) | AccessKey::Positional(field) => {
                PostfixOp::Field(field, e.span())
            }
        },
    );

    // One bracket parse, no speculation: the optional leading
    // expression is parsed exactly once, then `:` (slice) vs `]`
    // (index) disambiguates — each input parses exactly one way.
    // Trying slice before index (or vice versa) would push the
    // leading expression's arena nodes and then fail on the
    // missing `:`/`]`, leaking orphan nodes.
    let bracket_op = just(Token::LBracket)
        .ignore_then(expr.clone().or_not())
        .then(
            just(Token::Colon)
                .ignore_then(expr.clone().or_not())
                .then_ignore(just(Token::RBracket))
                .map(Some)
                .or(just(Token::RBracket).to(None)),
        )
        .try_map_with(|(start, end), e: &mut Mx<'a, '_, I>| {
            let span = e.span();
            match (start, end) {
                (start, Some(end)) => Ok(PostfixOp::Slice(start, end, span)),
                (Some(index), None) => Ok(PostfixOp::Index(index, span)),
                // `s[]` is rejected — the colon is mandatory for a
                // slice, the expression for an index.
                (None, None) => Err(Rich::custom(span, ParseDiag::EmptyBrackets)),
            }
        });

    // Seed the receiver gate (see `ParseState::postfix_head_enum`)
    // from the head atom; the fold re-derives the hint after every
    // folded op before the next op parses. In check mode `map_with`
    // skips its closure, but no rule in this grammar runs an
    // expression in check mode (check-mode uses are token-level:
    // `and_is`, `not`, statement-list probing), so the hint cannot
    // go stale at a read — the gated ops only reach it on an
    // emit-mode postfix iteration.
    let atom = atom.map_with(|head, e: &mut Mx<'a, '_, I>| {
        e.state().postfix_head_enum = type_name_ident(head, e).map(|id| id.name);
        head
    });

    let dot_op = just(Token::Dot)
        .ignore_then(choice((
            method_op,
            named_construct_op,
            missing_args_op,
            field_op,
        )))
        .map_with(|op: PostfixOp, e: &mut Mx<'a, '_, I>| op.with_span(e.span()));

    atom.foldl_with(
        choice((dot_op, bracket_op)).repeated(),
        |receiver, op, e: &mut Mx<'a, '_, I>| {
            let start = e.state().expr_span(receiver).start;
            let result = match op {
                PostfixOp::Method(method, args, span) => {
                    let receiver_type = type_name_ident(receiver, e);
                    // Recover each `name = value` arg with one
                    // targeted diagnostic (E0124): the enum flavor
                    // names the enum and variant and shows both
                    // correct spellings; the method flavor says
                    // named arguments are unsupported. The value
                    // passes through as a positional argument so its
                    // own type errors still surface.
                    let mut positional = Vec::with_capacity(args.len());
                    for arg in &args {
                        match arg {
                            enums::ParenArg::Expr(id) => positional.push(*id),
                            enums::ParenArg::NamedSyntax { span: nspan, value } => {
                                let diag = match &receiver_type {
                                    Some(enum_id) => ParseDiag::NamedArgInParens {
                                        enum_name: Some(enum_id.name),
                                        variant: Some(method.name),
                                    },
                                    None => ParseDiag::NamedArgInParens {
                                        enum_name: None,
                                        variant: None,
                                    },
                                };
                                e.emit(Rich::custom(*nspan, diag));
                                positional.push(*value);
                            }
                        }
                    }
                    match receiver_type {
                        Some(_enum_id) => {
                            // R4: a paren call on a bare uppercase-led
                            // receiver can only be variant construction
                            // (`Url.parse(x)`); the lowercase mirror
                            // (`obj.field()`) keeps its method-call
                            // meaning. The receiver `Ident` node is
                            // promoted in place (no orphan); args are
                            // collected, then pushed contiguously and
                            // sealed (I-205: a start→seal window cannot
                            // span the arg parses — nested construction
                            // writes the same arena).
                            let list_start = e.state().expr_lists_len();
                            for &arg in &positional {
                                e.state().push_expr_list_item(arg);
                            }
                            let list = e.state().expr_list_from(list_start);
                            e.state().promote_ident_to_variant_construct(
                                receiver,
                                method,
                                Some(VariantArgs {
                                    positional: Some(list),
                                    named: None,
                                }),
                                crate::spans::cspan(SimpleSpan::new((), start..span.end)),
                            )
                        }
                        None => e.state().method_call(
                            receiver,
                            method.name,
                            &positional,
                            crate::spans::cspan(SimpleSpan::new((), start..span.end)),
                        ),
                    }
                }
                PostfixOp::NamedConstruct(variant, inits, span) => {
                    // The parse-time gate already rejected
                    // non-type receivers; the receiver `Ident`
                    // node is promoted in place (no orphan).
                    // Same contiguous-push seal rationale as
                    // the paren form.
                    debug_assert!(
                        type_name_ident(receiver, e).is_some(),
                        "named construct is gated on a type receiver"
                    );
                    let list_start = e.state().struct_field_inits_len();
                    for &(name, value) in &inits {
                        e.state().push_struct_field_init_item(name, value);
                    }
                    let list = e.state().struct_field_init_list_from(list_start);
                    e.state().promote_ident_to_variant_construct(
                        receiver,
                        variant,
                        Some(VariantArgs {
                            positional: None,
                            named: Some(list),
                        }),
                        crate::spans::cspan(SimpleSpan::new((), start..span.end)),
                    )
                }
                PostfixOp::RecoveredPositionalBraces(variant, values, span) => {
                    // E0127; promote the values as positional args (a
                    // named variant then gets its own E0123).
                    let enum_name = type_name_ident(receiver, e)
                        .expect("brace construct is gated on a type receiver")
                        .name;
                    e.emit(Rich::custom(
                        span,
                        ParseDiag::PositionalArgsInBraces { enum_name },
                    ));
                    let list_start = e.state().expr_lists_len();
                    for &value in &values {
                        e.state().push_expr_list_item(value);
                    }
                    let list = e.state().expr_list_from(list_start);
                    e.state().promote_ident_to_variant_construct(
                        receiver,
                        variant,
                        Some(VariantArgs {
                            positional: Some(list),
                            named: None,
                        }),
                        crate::spans::cspan(SimpleSpan::new((), start..span.end)),
                    )
                }
                PostfixOp::RecoveredMissingParens(variant, arg, span) => {
                    // E0126 fires here (enum and variant names are
                    // fold-time knowledge); the trailing expression
                    // becomes the payload argument, so a valid payload
                    // type-checks and the enclosing statement still
                    // declares its bindings.
                    let enum_id = type_name_ident(receiver, e)
                        .expect("missing-parens op is gated on a type receiver");
                    e.emit(Rich::custom(
                        SimpleSpan::new((), start..span.end),
                        ParseDiag::MissingArgListOnVariant {
                            enum_name: enum_id.name,
                            variant: variant.name,
                        },
                    ));
                    let list_start = e.state().expr_lists_len();
                    e.state().push_expr_list_item(arg);
                    let list = e.state().expr_list_from(list_start);
                    e.state().promote_ident_to_variant_construct(
                        receiver,
                        variant,
                        Some(VariantArgs {
                            positional: Some(list),
                            named: None,
                        }),
                        crate::spans::cspan(SimpleSpan::new((), start..span.end)),
                    )
                }
                PostfixOp::Field(field, span) => e.state().field_access(
                    receiver,
                    field,
                    crate::spans::cspan(SimpleSpan::new((), start..span.end)),
                ),
                PostfixOp::Missing => receiver,
                PostfixOp::Slice(lo, hi, span) => e.state().slice(
                    receiver,
                    lo,
                    hi,
                    crate::spans::cspan(SimpleSpan::new((), start..span.end)),
                ),
                PostfixOp::Index(index, span) => e.state().index(
                    receiver,
                    index,
                    crate::spans::cspan(SimpleSpan::new((), start..span.end)),
                ),
            };
            e.state().postfix_head_enum = type_name_ident(result, e).map(|id| id.name);
            result
        },
    )
}

fn expression_parser<'a, I>() -> impl Parser<'a, I, ExprId, PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    recursive(|expr| {
        // One `key = value` field initializer, shared by the struct
        // literal atoms and the M11 named-variant-construct postfix
        // op. Field keys are identifiers or integer literals: a
        // positional key canonicalizes to its numeric value
        // (`{0=17, 1="alice"}` names fields "0", "1" — the brace
        // spelling of tuple sugar). Named literals with numeric
        // keys parse the same way and are rejected later, in
        // sema, as unknown fields.
        let field_init = field_key()
            .then_ignore(just(Token::Assign))
            .then(expr.clone());

        let atom = {
            let literal = select! {
                Token::IntLit(n) => Literal::Int(n),
                Token::FloatLit(bits) => Literal::Float(f64::from_bits(bits)),
                Token::StrLit(id) => Literal::Str(id),
                Token::BytesLit(id) => Literal::Bytes(id),
                Token::True => Literal::Bool(true),
                Token::False => Literal::Bool(false),
            }
            .map_with(|lit, e: &mut Mx<'a, '_, I>| {
                let span = crate::spans::cspan(e.span());
                e.state().literal(lit, span)
            });

            let call = select! { Token::Ident(name) => name }
                .then(
                    enums::paren_arg(expr.clone())
                        .separated_by(just(Token::Comma))
                        .allow_trailing()
                        .collect::<Vec<_>>()
                        .delimited_by(just(Token::LParen), just(Token::RParen)),
                )
                .map_with(|(name, args), e: &mut Mx<'a, '_, I>| {
                    let span = crate::spans::cspan(e.span());
                    // `name = value` in a plain call's parens: named
                    // arguments are unsupported (method flavor — an
                    // atom call has no variant to name).
                    let args = enums::unwrap_paren_args(args, e);
                    e.state().call(name, &args, span)
                });

            // Struct literal `Name{field=value, ...}` (M9). Both this
            // and `call` open with `Ident`; the `{` vs `(` delimiter
            // disambiguates, so the two alternatives cannot both
            // consume input. Field order stays in source order —
            // sema canonicalizes against the declaration.
            let struct_literal = select! { Token::Ident(name) => name }
                .map_with(|name, e: &mut Mx<'a, '_, I>| {
                    Ident::new(name, crate::spans::cspan(e.span()))
                })
                .then(
                    field_init
                        .clone()
                        .separated_by(just(Token::Comma))
                        .allow_trailing()
                        .collect::<Vec<_>>()
                        .delimited_by(just(Token::LBrace), just(Token::RBrace)),
                )
                .map_with(|(name, fields), e: &mut Mx<'a, '_, I>| {
                    let span = crate::spans::cspan(e.span());
                    e.state().struct_literal(Some(name), &fields, span)
                });

            // Anonymous struct literal `{field=value, ...}` (M10):
            // the same field list with no `Name` in front — the shape
            // is the identity. Tried after `struct_literal`; the two
            // open with different tokens, so they cannot both
            // consume input. `{}` stays reserved for the future
            // empty map literal: diagnosed, and recovered as an
            // empty literal so the rest of the file still parses.
            let anon_struct_literal = field_init
                .clone()
                .separated_by(just(Token::Comma))
                .allow_trailing()
                .collect::<Vec<_>>()
                .delimited_by(just(Token::LBrace), just(Token::RBrace))
                .validate(|fields, e: &mut Mx<'a, '_, I>, emitter| {
                    if fields.is_empty() {
                        emitter.emit(Rich::custom(e.span(), ParseDiag::EmptyAnonStruct));
                    }
                    fields
                })
                .map_with(|fields, e: &mut Mx<'a, '_, I>| {
                    let span = crate::spans::cspan(e.span());
                    e.state().struct_literal(None, &fields, span)
                });

            let ident_expr =
                select! { Token::Ident(name) => name }.map_with(|name, e: &mut Mx<'a, '_, I>| {
                    let span = crate::spans::cspan(e.span());
                    e.state().ident(name, span)
                });

            // `&ident` / `&ident.field...` — call-site mutable-borrow
            // marker (M8.3, extended to field chains in M9). Field hops
            // are folded INTO the borrow target (`&p.x` is `&(p.x)`,
            // never `(&p).x`), so the outer postfix loop never sees
            // them. Sema validates the target is an assignable lvalue
            // (`mut` local or `inout` param at the chain's root).
            let borrow = just(Token::Amp)
                .ignore_then(ident_expr)
                .then(
                    just(Token::Dot)
                        .ignore_then(field_key_ident())
                        .repeated()
                        .collect::<Vec<_>>(),
                )
                .map_with(|(inner, fields), e: &mut Mx<'a, '_, I>| {
                    let span = crate::spans::cspan(e.span());
                    let mut target = inner;
                    for field in fields {
                        let start = e.state().expr_span(target).start;
                        target = e.state().field_access(
                            target,
                            field,
                            crate::spans::cspan(SimpleSpan::new((), start..field.span.end)),
                        );
                    }
                    e.state().borrow(target, span)
                });

            // `(e)` stays a plain parenthesized expression — the inner
            // node is returned as-is, exactly like the historical
            // `delimited_by(LParen, RParen)` form. `(e, …)` —
            // including the single-element `(e,)` — is tuple sugar
            // over the anonymous struct literal: `(17, "alice")` ≡
            // `{0=17, 1="alice"}`, fields "0", "1", … minted in
            // written order against the state's pool (the same
            // canonicalization as numeric brace keys). `()` is the
            // unit: diagnosed (UnitParen — `void` is the unit type)
            // and recovered as an empty anonymous literal, exactly
            // like `{}` (E0109).
            //
            // Single parse, no speculation: after `(` either `)`
            // matches immediately or the first expression parses
            // once, then `,` vs `)` disambiguates tuple from paren.
            // A failed alternative never leaves orphan arena nodes
            // behind (the Ast checkpoint hooks are deliberate no-ops
            // — see `successful_parses_leave_no_orphan_nodes`).
            let paren_or_tuple = just(Token::LParen)
                .ignore_then(
                    just(Token::RParen).to(None).or(expr
                        .clone()
                        .then(
                            just(Token::Comma)
                                .ignore_then(
                                    expr.clone()
                                        .separated_by(just(Token::Comma))
                                        .allow_trailing()
                                        .collect::<Vec<_>>(),
                                )
                                .then_ignore(just(Token::RParen))
                                .map(Some)
                                .or(just(Token::RParen).to(None)),
                        )
                        .map(Some)),
                )
                .validate(|body, e: &mut Mx<'a, '_, I>, emitter| {
                    if body.is_none() {
                        emitter.emit(Rich::custom(e.span(), ParseDiag::UnitParen));
                    }
                    body
                })
                .map_with(|body, e: &mut Mx<'a, '_, I>| match body {
                    None => {
                        // Recovered `()`: an empty anonymous literal.
                        // Sema tolerates the empty field list silently
                        // (the E0109 contract), so E0111 stands alone.
                        let span = crate::spans::cspan(e.span());
                        e.state().struct_literal(None, &[], span)
                    }
                    Some((first, None)) => first,
                    Some((first, Some(rest))) => {
                        let span = crate::spans::cspan(e.span());
                        let mut fields = Vec::with_capacity(rest.len() + 1);
                        fields.push((positional_field_name(0, &mut e.state().pool), first));
                        for (i, value) in rest.into_iter().enumerate() {
                            let index = i64::try_from(i)
                                .expect("tuple arity fits i64")
                                .checked_add(1)
                                .expect("tuple arity fits i64");
                            fields.push((positional_field_name(index, &mut e.state().pool), value));
                        }
                        e.state().struct_literal(None, &fields, span)
                    }
                });

            borrow
                .or(call)
                .or(struct_literal)
                .or(anon_struct_literal)
                .or(ident_expr)
                .or(literal)
                .or(paren_or_tuple)
                .or(match_::match_expression_parser(expr.clone()))
        };

        let postfix = postfix_parser(expr.clone(), field_init.clone(), atom);

        let unary_op = choice((
            just(Token::Sub).to(UnaryOperator::Neg),
            just(Token::Not).to(UnaryOperator::Not),
        ));

        // `- IntLitMin` is folded to the `i64::MIN` literal at parse
        // time: the positive form `9223372036854775808` overflows
        // `i64`, so the lexer marks it with a dedicated token that is
        // only grammatical directly after unary `-`.
        let neg_min =
            just(Token::Sub)
                .then(just(Token::IntLitMin))
                .map_with(|_, e: &mut Mx<'a, '_, I>| {
                    let span = crate::spans::cspan(e.span());
                    e.state().literal_int(i64::MIN, span)
                });

        // Each precedence level is `.boxed()` to erase the concrete
        // combinator type. Without this the levels nest into each other
        // (term contains unary, additive contains term, ...) and the
        // demangled symbol names grow to hundreds of kilobytes, which
        // breaks some profiling tooling (and compile times).
        let unary = neg_min
            .or(unary_op
                .repeated()
                .collect::<Vec<_>>()
                .then(postfix)
                .map_with(|(ops, expr), e: &mut Mx<'a, '_, I>| {
                    let span = crate::spans::cspan(e.span());
                    let mut result = expr;
                    for op in ops.into_iter().rev() {
                        result = e.state().unary(op, result, span);
                    }
                    result
                }))
            .boxed();

        // Fold `left op right` into a BinaryOp node spanning both
        // operands — the same span rule at every precedence level.
        fn fold_binary<'a, I>(
            left: ExprId,
            (op, right): (BinaryOperator, ExprId),
            e: &mut Mx<'a, '_, I>,
        ) -> ExprId
        where
            I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
        {
            let start = e.state().expr_span(left).start;
            let end = e.state().expr_span(right).end;
            e.state().binary(
                op,
                left,
                right,
                crate::spans::cspan(SimpleSpan::new((), start..end)),
            )
        }

        let term = unary.clone().foldl_with(
            choice((
                just(Token::Mul).to(BinaryOperator::Mul),
                just(Token::Div).to(BinaryOperator::Div),
                just(Token::Percent).to(BinaryOperator::Mod),
            ))
            .then(unary)
            .repeated(),
            fold_binary,
        );

        let term = term.boxed();

        let additive = term.clone().foldl_with(
            choice((
                just(Token::Add).to(BinaryOperator::Add),
                just(Token::Sub).to(BinaryOperator::Sub),
            ))
            .then(term)
            .repeated(),
            fold_binary,
        );

        let additive = additive.boxed();

        // Non-associative levels (ordering, equality) share one
        // shape: `foldl_with` over a bare `repeated` — the same
        // zero-allocation shape as the additive/term levels. The
        // accumulator carries a `seen` flag: a second operator at the
        // same level is a chained comparison (`a < b < c`), soft-
        // rejected with a secondary error via `MapExtra::emit`
        // pointing at the extra operator (span captured with
        // `spanned`); its operand is dropped so the AST keeps only
        // the well-formed first comparison and sema sees a clean
        // tree. The emitted error still fails the parse overall.
        // (Earlier shapes were slower: two speculative stages —
        // `or_not` + `repeated` — regressed parse benches ~15%, and a
        // `repeated().collect::<Vec<_>>()` stage ~5%. Detecting the
        // chain from the left node's operator instead of a flag
        // mis-fires on parenthesized comparisons like `(a < b) < c`,
        // which must keep parsing and fail in sema as before.)
        fn fold_non_assoc<'a, I>(
            (left, seen): (ExprId, bool),
            (op, right): (chumsky::span::Spanned<BinaryOperator>, ExprId),
            e: &mut Mx<'a, '_, I>,
        ) -> (ExprId, bool)
        where
            I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
        {
            if seen {
                e.emit(Rich::custom(op.span, ParseDiag::ChainedComparison));
                return (left, true);
            }
            (fold_binary(left, (op.inner, right), e), true)
        }

        // Ordering (non-associative) sits between additive and equality.
        let ordering_op = choice((
            just(Token::LtEq).to(BinaryOperator::LtEq),
            just(Token::GtEq).to(BinaryOperator::GtEq),
            just(Token::Lt).to(BinaryOperator::Lt),
            just(Token::Gt).to(BinaryOperator::Gt),
        ))
        .spanned();

        let ordering = additive
            .clone()
            .map(|left| (left, false))
            .foldl_with(
                ordering_op.then(additive).repeated(),
                |acc, op_right, e: &mut Mx<'a, '_, I>| fold_non_assoc(acc, op_right, e),
            )
            .map(|(left, _)| left)
            .boxed();

        // Equality is non-associative.
        let equality_op = choice((
            just(Token::EqEq).to(BinaryOperator::Eq),
            just(Token::NotEq).to(BinaryOperator::NotEq),
        ))
        .spanned();

        let equality = ordering
            .clone()
            .map(|left| (left, false))
            .foldl_with(
                equality_op.then(ordering).repeated(),
                |acc, op_right, e: &mut Mx<'a, '_, I>| fold_non_assoc(acc, op_right, e),
            )
            .map(|(left, _)| left)
            .boxed();

        // Logical AND binds tighter than OR, below equality.
        let logical_and = equality.clone().foldl_with(
            just(Token::And)
                .to(BinaryOperator::And)
                .then(equality)
                .repeated(),
            fold_binary,
        );

        let logical_and = logical_and.boxed();

        // Logical OR is the lowest precedence.
        logical_and
            .clone()
            .foldl_with(
                just(Token::Or)
                    .to(BinaryOperator::Or)
                    .then(logical_and)
                    .repeated(),
                fold_binary,
            )
            .boxed()
    })
}

mod enums;

mod destructure;

mod match_;

mod statements;

#[cfg(test)]
mod arena_tests;

#[cfg(test)]
mod destructure_tests;

#[cfg(test)]
mod enum_tests;

#[cfg(test)]
mod match_tests;

#[cfg(test)]
mod recovery_tests;

#[cfg(test)]
mod struct_tests;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod type_tests;
