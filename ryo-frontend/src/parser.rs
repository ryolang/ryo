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
//! as unreachable orphans (the `Inspector` hooks on `Ast` are
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

// The `Inspector` hooks stay no-ops — the rewind-truncation rationale
// lives on the `Ast` impl in `ryo-core` (snapshotting at every
// choice/repeated boundary cost ~20% of parse time; orphan nodes are
// unreachable from `top_level`).
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

/// Helper: skip zero or more newline tokens.
fn skip_newlines<'a, I>() -> impl Parser<'a, I, (), PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    select! { Token::Newline => () }.repeated().to(())
}

/// Helper: require at least one newline. Used between consecutive
/// statements so two statements on the same line are a parse error
/// (matters now that bare expression statements are allowed at the
/// top level — without this, `x 42` would silently parse as two
/// separate expression statements).
fn require_newlines<'a, I>() -> impl Parser<'a, I, (), PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    select! { Token::Newline => () }
        .repeated()
        .at_least(1)
        .to(())
}

/// Whether a statement line ended cleanly (newline-terminated) or
/// needed garbage-skipping recovery to reach the line boundary.
#[derive(Clone)]
enum LineTail {
    Clean,
    Garbage,
}

/// Positive lookahead for a statement-list terminator: succeed
/// (consuming nothing) only when `token` is next. Block-final
/// statements must not eat the `Dedent` that the surrounding
/// `delimited_by` expects.
fn peek_terminator<'a, I>(token: Token) -> impl Parser<'a, I, (), PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    empty().and_is(just(token)).ignored()
}

/// One garbage token for line-level recovery: anything except the
/// statement boundaries where resynchronization can happen — and
/// except `Indent`, which the indent pre-processor emits *before* the
/// newline that opens a block. Keeping `Indent` out of the garbage
/// set preserves the signal that a broken line was a block header, so
/// recovery can swallow its body (see `swallow_block`).
fn garbage_token<'a, I>() -> impl Parser<'a, I, (), PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    none_of([Token::Newline, Token::Indent, Token::Dedent]).ignored()
}

/// Skip at least one non-boundary token. Recovering over zero tokens
/// at a clean boundary would emit a spurious error.
fn skip_garbage<'a, I>() -> impl Parser<'a, I, (), PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    garbage_token().repeated().at_least(1).ignored()
}

/// Skip one balanced `Indent` … `Dedent` region, including any blocks
/// nested inside it.
///
/// Line recovery uses this right after a failed statement: when the
/// garbage on the broken line is followed by an `Indent`, the line was
/// almost certainly a block header (`fn`/`if`/`while`/…), so its body
/// is swallowed as part of the same error region. Without this the
/// body lines would go on to parse at the enclosing scope — silently
/// mis-nested — and the block's closing `Dedent` would be left
/// dangling for the enclosing list to trip over.
///
/// Blank lines between the broken header and its body are tolerated:
/// the pre-processor emits their newlines before the `Indent`.
fn swallow_block<'a, I>() -> impl Parser<'a, I, (), PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    let balanced = recursive(|region| {
        choice((
            // A nested block, consumed whole so its `Dedent` does not
            // terminate the outer region early.
            just(Token::Indent)
                .ignore_then(region)
                .then_ignore(just(Token::Dedent)),
            // Any token that cannot change the nesting depth.
            none_of([Token::Indent, Token::Dedent]).ignored(),
        ))
        .repeated()
        .ignored()
    });

    skip_newlines()
        .ignore_then(just(Token::Indent))
        .ignore_then(balanced)
        .then_ignore(just(Token::Dedent))
        // The pre-processor emits the block-closing `Dedent` *before*
        // the line-break newline, so that newline (and any blank
        // lines) sit between the swallowed block and the next
        // statement. Consume them: the swallowed region has no tail
        // of its own, and the statement-list loop only skips newlines
        // ahead of its first line.
        .then_ignore(skip_newlines())
}

/// A list of newline-separated statements with per-line error
/// recovery (R10).
///
/// Shape: blank lines, then `line*`, where each line is `stmt tail`
/// and the tail is either a newline run or the list `terminator`
/// (`end()` at top level, `peek_terminator(Dedent)` in blocks). The
/// terminator doubles as a line end because the lexer emits no
/// Newline before an end-of-input `Dedent`: a block-final statement
/// sits directly against it, so that one line has no newline tail.
///
/// * if the statement parses but garbage follows on the same line
///   (`y = = 1`), the tail recovery skips to the boundary and the
///   whole line collapses to one `Error` node — a half-parsed
///   statement prefix never survives into the AST;
/// * if `stmt` itself fails and the broken line is a block header
///   (`fn foo(` followed by an indented body), the recovery skips the
///   header's garbage and swallows the whole indented block as one
///   `Error` region (see `swallow_block`), so the body cannot
///   mis-nest into the enclosing scope and no `Dedent` dangles;
/// * any other broken line is skipped to the line boundary and
///   becomes an `Error` node.
///
/// Every skip uses `at_least(1)`: recovering over zero tokens at a
/// clean boundary would emit a spurious error. Errors emitted by a
/// recovery whose surrounding line later fails are rolled back by
/// chumsky's rewind, so each broken line reports exactly once.
/// (Arena nodes pushed inside a failed region are *not* rolled back
/// — they stay as unreachable orphans, which is safe because no kept
/// node references them; see the `Inspector` impl on `Ast`.)
///
/// `not_at_boundary` decides where the loop stops without running the
/// statement grammar: the grammar is deep and a failed alternative
/// carries chumsky's `Rich` expected-set bookkeeping, so letting the
/// loop fail its way through the whole of `stmt` against the
/// `Dedent`/end-of-input that ends the list costs more than parsing a
/// real statement. Lines the guard admits parse exactly as before.
fn statement_list<'a, I>(
    stmt: impl Parser<'a, I, StmtId, PExtra<'a>> + Clone + 'a,
    terminator: impl Parser<'a, I, (), PExtra<'a>> + Clone + 'a,
) -> impl Parser<'a, I, Vec<StmtId>, PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    let line_end = require_newlines().or(terminator);

    let tail = line_end
        .clone()
        .to(LineTail::Clean)
        .recover_with(via_parser(
            skip_garbage()
                .then_ignore(line_end.clone())
                .to(LineTail::Garbage),
        ));

    // Zero-width: succeeds only when another line can follow.
    let not_at_boundary = empty().and_is(none_of([Token::Dedent])).ignored();

    // The recoveries attach to the whole line, not to `stmt`: the
    // block-swallowing variant consumes the block's closing `Dedent`,
    // which doubles as the line end, so no tail follows it.
    let line = not_at_boundary
        .ignore_then(stmt)
        .then(tail)
        .map_with(|(s, tail), e: &mut Mx<'a, '_, I>| match tail {
            LineTail::Clean => s,
            LineTail::Garbage => {
                let span = e.span();
                e.state().error_stmt(span)
            }
        })
        .recover_with(via_parser(
            skip_garbage()
                .then_ignore(swallow_block())
                .map_with(|_, e: &mut Mx<'a, '_, I>| {
                    let span = e.span();
                    e.state().error_stmt(span)
                }),
        ))
        .recover_with(via_parser(skip_garbage().then_ignore(line_end).map_with(
            |_, e: &mut Mx<'a, '_, I>| {
                let span = e.span();
                e.state().error_stmt(span)
            },
        )));

    skip_newlines()
        .ignore_then(line.repeated().collect::<Vec<_>>())
        .then_ignore(skip_newlines())
        .boxed()
}

/// Parse a complete Ryo program. The arena is the parser state: on
/// success (including recovered partial parses) its `top_level` list
/// holds the program's statements; the output `()` carries nothing.
pub fn program_parser<'a, I>() -> impl Parser<'a, I, (), PExtra<'a>> + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    // Statements are newline-separated. Leading/trailing newlines
    // are tolerated; consecutive statements must have at least one
    // newline between them. Unparseable lines recover to `Error`
    // nodes (see `statement_list`), so a syntax error never discards
    // the rest of the file.
    statement_list(statement_parser(), end())
        .then_ignore(end())
        .map_with(|statements, e: &mut Mx<'a, '_, I>| e.state().set_top_level(statements))
        .boxed()
}

/// Parse an indented block of one or more statements.
fn indented_block<'a, I>(
    stmt: impl Parser<'a, I, StmtId, PExtra<'a>> + Clone + 'a,
) -> impl Parser<'a, I, Vec<StmtId>, PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    statement_list(stmt, peek_terminator(Token::Dedent))
        .delimited_by(
            skip_newlines().ignore_then(just(Token::Indent)),
            just(Token::Dedent),
        )
        .boxed()
}

/// Assignment target: a bare identifier or a `.field` path rooted at
/// one (`p`, `p.x`, `a.b.c`, `pair.0`). The segments are folded into a
/// `FieldAccess` chain by the caller; an empty segment list keeps the
/// bare-identifier path byte-identical to before M9. Field keys accept
/// positional (`IntLit`) hops, canonicalized like expression access.
fn assign_target_parser<'a, I>() -> impl Parser<'a, I, (Ident, Vec<Ident>), PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    select! { Token::Ident(s) => s }
        .map_with(|s, e: &mut Mx<'a, '_, I>| Ident::new(s, e.span()))
        .then(
            just(Token::Dot)
                .ignore_then(field_key_ident())
                .repeated()
                .collect::<Vec<_>>(),
        )
}

/// Build the `FieldAccess` chain expression for a parsed assignment
/// target. Only called when `fields` is non-empty.
fn field_access_chain(ast: &mut Ast, root: Ident, fields: &[Ident]) -> ExprId {
    let mut target = ast.ident(root.name, root.span);
    for field in fields {
        let span = SimpleSpan::new((), root.span.start..field.span.end);
        target = ast.field_access(target, *field, span);
    }
    target
}

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

fn assign_or_decl_parser<'a, I>(
    expr: impl Parser<'a, I, ExprId, PExtra<'a>> + Clone + 'a,
) -> impl Parser<'a, I, StmtId, PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    assign_target_parser()
        .then_ignore(just(Token::Assign))
        .then(expr)
        .map_with(|((target, fields), value), e: &mut Mx<'a, '_, I>| {
            let span = e.span();
            if fields.is_empty() {
                e.state().assign_or_decl(target, value, span)
            } else {
                let chain = field_access_chain(e.state(), target, &fields);
                e.state().field_assign(chain, value, span)
            }
        })
        .boxed()
}

fn compound_assign_parser<'a, I>(
    expr: impl Parser<'a, I, ExprId, PExtra<'a>> + Clone + 'a,
) -> impl Parser<'a, I, StmtId, PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    let op = choice((
        just(Token::PlusAssign).to(CompoundOp::Add),
        just(Token::MinusAssign).to(CompoundOp::Sub),
        just(Token::StarAssign).to(CompoundOp::Mul),
        just(Token::SlashAssign).to(CompoundOp::Div),
        just(Token::PercentAssign).to(CompoundOp::Mod),
    ));

    assign_target_parser()
        .then(op)
        .then(expr)
        .map_with(|(((target, fields), op), value), e: &mut Mx<'a, '_, I>| {
            let span = e.span();
            if fields.is_empty() {
                e.state().compound_assign(target, op, value, span)
            } else {
                let chain = field_access_chain(e.state(), target, &fields);
                e.state().compound_field_assign(chain, op, value, span)
            }
        })
        .boxed()
}

/// Push a bind-or-wildcard pattern node: `_` lexes as an ordinary
/// identifier, so the wildcard is recognized by name here.
fn bind_pattern(state: &mut ParseState, name: StringId, span: SimpleSpan) -> PatternId {
    if state.pool.str(name) == "_" {
        state.pattern_wildcard(span)
    } else {
        state.pattern_bind(Ident::new(name, span), span)
    }
}

/// A destructuring statement `pattern = value` (M10). One parse per
/// form, ordered choice, no `attempt()` — the alternatives are
/// disjoint after their first token and a failing alternative rewinds
/// without pushing arena nodes (the same mechanism as the existing
/// Ident-led statements):
///
/// - paren-less `a, b = e` — bindings only; the comma after the first
///   binding distinguishes the form from plain `a = e`, which stays
///   `assign_or_decl` (`destructure` is tried first in the statement
///   dispatch, so the paren-less form claims `Ident ,` lines before
///   `assign_or_decl` sees them);
/// - parenthesized `(a, b) = e`, `(a,) = e` — a positional pattern;
///   the mandatory comma inside the parens distinguishes the form
///   from a parenthesized-expression statement, and nested elements
///   like `(a, (b, c))` parse through the recursive pattern handle;
/// - braced `{q, r} = e`, `{x = quot} = e` — an anonymous field
///   pattern; the `= value` after the closing brace distinguishes it
///   from an anonymous struct literal expression statement, so bare
///   `{x=1}` keeps its meaning (a failed `shaped` alternative rewinds
///   before the `=` and `expr_stmt` claims the line);
/// - `(a) = x` — a one-element parenthesized pattern without the
///   trailing comma is unspellable (`(a)` is an expression), so the
///   shape parses whole and reports the targeted
///   `SingleElemDestructuring` diagnostic, recovering the line to an
///   `Error` statement — the `MisplacedAttribute` shape, so the
///   message stands alone instead of degrading into a generic
///   "expected newline" tail error.
///
/// Note: when `shaped` parses a well-formed pattern that turns out
/// not to be followed by `=` (a bare `(a, b)` or `{x=1}` expression
/// statement), the pattern nodes pushed before the failure stay in
/// the arenas as unreachable orphans — deliberate (the `Inspector`
/// hooks on `Ast` are no-ops; see `statement_list`).
///
/// `value` is the right-hand-side expression grammar: the caller's
/// shared expression parser (see `top_level_statement_parser`), so
/// this rule does not construct — and later drop — full expression
/// grammars of its own on every `program_parser()` call.
fn destructure_stmt_parser<'a, I>(
    value: impl Parser<'a, I, ExprId, PExtra<'a>> + Clone + 'a,
) -> impl Parser<'a, I, StmtId, PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    // The recursive pattern handle. `Recursive` (declare/define, what
    // `recursive()` is sugar for) is what lets the anon and
    // positional sub-parsers be shared between the full pattern
    // grammar — positional elements recurse through `pat` — and the
    // statement forms below.
    let mut pat = Recursive::declare();

    // A bare binding target: an identifier, or `_` (by name). Yields
    // the name with its span; the pattern node is pushed by the
    // consumer, which decides between `bind` and the paren-less form.
    let bind_target = select! { Token::Ident(name) => name }
        .map_with(|name, e: &mut Mx<'a, '_, I>| (name, e.span()));

    let bind = bind_target
        .map_with(|(name, span), e: &mut Mx<'a, '_, I>| bind_pattern(e.state(), name, span));

    // One field of an anonymous pattern: a pun (`q`), a wildcard
    // field (`_`), or a rename (`x = quot`). A field named `_` binds
    // nothing, so renaming it (`{_ = x}`) is rejected. Yields the
    // scalar `PatternField`; the arena push happens at `pattern_anon`.
    let field = bind_target
        .then(just(Token::Assign).ignore_then(bind_target).or_not())
        .try_map_with(|((name, span), rename), e: &mut Mx<'a, '_, I>| {
            if e.state().pool.str(name) == "_" && rename.is_some() {
                return Err(Rich::custom(
                    span,
                    ParseDiag::Message("wildcard field `_` binds nothing; drop the `= ...`".into()),
                ));
            }
            let binding = match rename {
                Some((bind_name, bind_span)) => Ident::new(bind_name, bind_span),
                None => Ident::new(name, span),
            };
            Ok(PatternField { name, binding })
        });

    let anon = field
        .separated_by(just(Token::Comma))
        .allow_trailing()
        .at_least(1)
        .collect::<Vec<_>>()
        .delimited_by(just(Token::LBrace), just(Token::RBrace))
        .map_with(|fields, e: &mut Mx<'a, '_, I>| {
            let span = e.span();
            e.state().pattern_anon(&fields, span)
        });

    // Positional pattern: the comma after the first element is
    // mandatory — exactly like the value-side tuple sugar and the
    // positional type sugar, `(a)` never parses here.
    let positional = pat
        .clone()
        .then_ignore(just(Token::Comma))
        .then(
            pat.clone()
                .separated_by(just(Token::Comma))
                .allow_trailing()
                .collect::<Vec<_>>(),
        )
        .delimited_by(just(Token::LParen), just(Token::RParen))
        .map_with(|(first, rest), e: &mut Mx<'a, '_, I>| {
            let span = e.span();
            let mut elems = Vec::with_capacity(rest.len() + 1);
            elems.push(first);
            elems.extend(rest);
            e.state().pattern_positional(&elems, span)
        });

    pat.define(choice((positional.clone(), anon, bind)).boxed());

    // Paren-less form: `a, b = e`. Bindings only — nesting belongs to
    // the parenthesized form.
    let paren_less = bind_target
        .then_ignore(just(Token::Comma))
        .then(
            bind_target
                .separated_by(just(Token::Comma))
                .allow_trailing()
                .at_least(1)
                .collect::<Vec<_>>(),
        )
        .then_ignore(just(Token::Assign))
        .then(value.clone())
        .map_with(|((first, rest), value), e: &mut Mx<'a, '_, I>| {
            let span = e.span();
            let start = first.1.start;
            let end = rest.last().map(|(_, s)| s.end).unwrap_or(first.1.end);
            let mut elems = Vec::with_capacity(rest.len() + 1);
            elems.push(bind_pattern(e.state(), first.0, first.1));
            for (name, sp) in rest {
                elems.push(bind_pattern(e.state(), name, sp));
            }
            let target = e
                .state()
                .pattern_positional(&elems, SimpleSpan::new((), start..end));
            e.state().destructure(target, value, span)
        });

    // Braced / parenthesized forms: a self-delimiting pattern followed
    // by `= value`.
    let shaped = choice((anon, positional))
        .then_ignore(just(Token::Assign))
        .then(value.clone())
        .map_with(|(target, value), e: &mut Mx<'a, '_, I>| {
            let span = e.span();
            e.state().destructure(target, value, span)
        });

    // `(a) = x`: one binding, no trailing comma, followed by `=`. The
    // shape is a targeted error (E0112), but recover as a PLAIN
    // binding of `a` rather than an error statement: downstream uses
    // of `a` then resolve instead of cascading one 'undefined
    // variable' per use — E0112 already fails the build.
    let single_no_comma = just(Token::LParen)
        .ignore_then(bind_target)
        .then_ignore(just(Token::RParen))
        .then_ignore(just(Token::Assign))
        .then(value)
        .validate(
            |((name, name_span), value), e: &mut Mx<'a, '_, I>, emitter| {
                emitter.emit(Rich::custom(e.span(), ParseDiag::SingleElemDestructuring));
                ((name, name_span), value)
            },
        )
        .map_with(|((name, name_span), value), e: &mut Mx<'a, '_, I>| {
            let span = e.span();
            let target = Ident::new(name, name_span);
            e.state().assign_or_decl(target, value, span)
        });

    choice((paren_less, shaped, single_no_comma)).boxed()
}

/// Statements valid inside a function body.
fn body_statement_parser<'a, I>(
    expr: impl Parser<'a, I, ExprId, PExtra<'a>> + Clone + 'a,
) -> impl Parser<'a, I, StmtId, PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    recursive(|body_stmt| {
        let return_stmt = just(Token::Return)
            .ignore_then(expr.clone().or_not())
            .map_with(|expr, e: &mut Mx<'a, '_, I>| {
                let span = e.span();
                e.state().return_stmt(expr, span)
            });

        let break_stmt = just(Token::Break).map_with(|_, e: &mut Mx<'a, '_, I>| {
            let span = e.span();
            e.state().break_stmt(span)
        });

        let continue_stmt = just(Token::Continue).map_with(|_, e: &mut Mx<'a, '_, I>| {
            let span = e.span();
            e.state().continue_stmt(span)
        });

        let while_stmt = just(Token::While)
            .ignore_then(expr.clone())
            .then(block_header(
                "while",
                "the condition",
                indented_block(body_stmt.clone()),
            ))
            .map_with(|(cond, body), e: &mut Mx<'a, '_, I>| {
                let span = e.span();
                e.state().while_loop(cond, &body, span)
            });

        let for_range_stmt = just(Token::For)
            .ignore_then(
                select! { Token::Ident(s) => s }.map_with(|s, e: &mut Mx<'a, '_, I>| Ident {
                    name: s,
                    span: e.span(),
                }),
            )
            .then_ignore(just(Token::In))
            .then(
                select! { Token::Ident(s) => s }.map_with(|s, e: &mut Mx<'a, '_, I>| Ident {
                    name: s,
                    span: e.span(),
                }),
            )
            .then_ignore(just(Token::LParen))
            .then(
                expr.clone()
                    .separated_by(just(Token::Comma))
                    .collect::<Vec<_>>(),
            )
            .then_ignore(just(Token::RParen))
            .then(block_header(
                "for",
                "the for clause",
                indented_block(body_stmt.clone()),
            ))
            .try_map_with(|(((var, iterator), args), body), e: &mut Mx<'a, '_, I>| {
                if args.len() != 2 {
                    return Err(Rich::custom(
                        e.span(),
                        ParseDiag::RangeArity { found: args.len() },
                    ));
                }
                let mut args = args.into_iter();
                let start = args
                    .next()
                    .expect("arity checked above: range() has exactly 2 arguments");
                let end = args
                    .next()
                    .expect("arity checked above: range() has exactly 2 arguments");
                let span = e.span();
                Ok(e.state().for_range(var, iterator, start, end, &body, span))
            });

        let expr_stmt = expr.clone().map_with(|expr, e: &mut Mx<'a, '_, I>| {
            let span = e.span();
            e.state().expr_stmt(expr, span)
        });

        // Boxed to keep the concrete type small: this parser is stored
        // inside `Recursive`, which keeps the full type in its symbol
        // names (see the note in `expression_parser`).
        //
        // `destructure_stmt_parser` leads the choice: its forms start
        // with `Ident ,`, `(`, or `{` — the same leading tokens as
        // `compound_assign` / `assign_or_decl` / `var_decl` /
        // `expr_stmt` — so it must claim those lines before any of
        // them can misparse a trailing `= value` as garbage.
        choice((
            destructure_stmt_parser(expr.clone()),
            return_stmt,
            compound_assign_parser(expr.clone()),
            assign_or_decl_parser(expr.clone()),
            var_decl_parser(expr.clone()),
            if_stmt_parser(body_stmt, expr),
            while_stmt,
            for_range_stmt,
            break_stmt,
            continue_stmt,
            expr_stmt,
        ))
        .boxed()
    })
}

/// Block header for `if` / `elif` / `else` / `while` / `for` / `fn`:
/// the colon plus the two Python-transcription failure shapes:
/// a same-line statement after the colon, and a statement keyword
/// where the colon belongs.
///
/// Three token-disjoint arms:
///
///  1. `:` then a *required* indented block — the normal path,
///     byte-identical to the old `just(Token::Colon).then(block)`.
///     An absent block fails the whole statement exactly as before:
///     the error paths below must not make a body-less header parse
///     as an empty body.
///  2. `:` then a statement on the same line — emit "Ryo doesn't
///     support one-line '…' bodies" spanning the colon and the
///     swallowed statement, then recover with the block when one
///     exists or an empty body. One mistake, one diagnostic. Fires
///     only when the token after the colon is not `<newline>` /
///     `<indent>` / `<dedent>` — the indent preprocessor emits
///     `<indent>` *before* the newline at block starts, so those
///     three tokens mark every legitimate non-statement position.
///  3. No `:` and a statement-start keyword sits where the colon
///     belongs — emit "expected ':' after …" at that token and
///     recover the same way. Any other missing-colon shape fails
///     here so the generic error machinery reports as before.
///
/// Yields the body statements.
fn block_header<'a, I>(
    keyword: &'static str,
    after: &'static str,
    block: impl Parser<'a, I, Vec<StmtId>, PExtra<'a>> + Clone + 'a,
) -> impl Parser<'a, I, Vec<StmtId>, PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    // Normal path: colon, required block.
    let normal = just(Token::Colon)
        .then(block.clone())
        .map(|(_, stmts)| stmts);

    // Same-line body: diagnose at the colon, swallow the statement.
    // The swallow must stop at <indent> as well as <newline>: the
    // indent preprocessor emits <indent> before the newline when the
    // following line is deeper-indented, and eating it orphans the
    // matching <dedent>, mis-nesting every enclosing block.
    let same_line = just(Token::Colon)
        .then(
            empty()
                .and_is(none_of([Token::Newline, Token::Indent, Token::Dedent]))
                .then_ignore(none_of([Token::Newline, Token::Indent]).repeated()),
        )
        .then(block.clone().or(empty().to(Vec::new())))
        .validate(move |(_, stmts), e: &mut Mx<'a, '_, I>, emitter| {
            emitter.emit(Rich::custom(
                e.span(),
                ParseDiag::Message(format!(
                    "Ryo doesn't support one-line '{keyword}' bodies — \
                     put the statement on its own line, indented"
                )),
            ));
            stmts
        });

    // Missing colon: only claim shapes that look like a statement —
    // anything else keeps the generic colon error. Same <indent>
    // swallow discipline as the same-line arm.
    let missing_colon = select! {
        Token::Return | Token::Break | Token::Continue | Token::If
        | Token::While | Token::For | Token::Ident(_) => ()
    }
    .then_ignore(none_of([Token::Newline, Token::Indent]).repeated())
    .then(block.clone().or(empty().to(Vec::new())))
    .validate(move |(_, stmts), e: &mut Mx<'a, '_, I>, emitter| {
        emitter.emit(Rich::custom(
            e.span(),
            ParseDiag::Message(format!("expected ':' after {after}")),
        ));
        stmts
    });

    choice((normal, same_line, missing_colon)).boxed()
}

fn if_stmt_parser<'a, I>(
    body_stmt: impl Parser<'a, I, StmtId, PExtra<'a>> + Clone + 'a,
    expr: impl Parser<'a, I, ExprId, PExtra<'a>> + Clone + 'a,
) -> impl Parser<'a, I, StmtId, PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    let block = indented_block(body_stmt);

    let elif_branch = skip_newlines()
        .ignore_then(just(Token::Elif))
        .ignore_then(expr.clone())
        .then(block_header("elif", "the condition", block.clone()));

    let else_block = skip_newlines()
        .ignore_then(just(Token::Else))
        .ignore_then(block_header("else", "else", block.clone()));

    just(Token::If)
        .ignore_then(expr)
        .then(block_header("if", "the condition", block))
        .then(elif_branch.repeated().collect::<Vec<_>>())
        .then(else_block.or_not())
        .map_with(
            |(((cond, then_block), elif_branches), else_block), e: &mut Mx<'a, '_, I>| {
                // Push each elif body into the `stmt_lists` arena up
                // front: `if_stmt` takes `(ExprId, StmtList)` pairs,
                // so no owned `Vec` crosses the builder API.
                let elif_branches: Vec<(ExprId, StmtList)> = elif_branches
                    .into_iter()
                    .map(|(elif_cond, block)| (elif_cond, e.state().push_stmt_list(&block)))
                    .collect();
                let span = e.span();
                e.state().if_stmt(
                    cond,
                    &then_block,
                    &elif_branches,
                    else_block.as_deref(),
                    span,
                )
            },
        )
        .boxed()
}

/// The tail of a `struct` declaration after the `struct` keyword:
/// `Name:` followed by an indented block of `field: type` lines (M9).
/// Yields the name and the raw field list; both struct-declaration
/// forms (plain and attributed) build their `StructDef` node from this.
fn struct_tail_parser<'a, I>()
-> impl Parser<'a, I, (Ident, Vec<(StringId, TypeExpr)>), PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    let field = select! { Token::Ident(name) => name }
        .then_ignore(just(Token::Colon))
        .then(type_expr_parser());

    // Field lines mirror `statement_list`'s newline structure: at
    // least one newline between fields, blank lines tolerated, and
    // the block-final field may sit directly against the `Dedent`
    // (files without a trailing newline).
    let field_line = field.then_ignore(require_newlines().or(peek_terminator(Token::Dedent)));

    // Same delimiting shape as `indented_block`: blank lines between
    // the header and the body land *before* the `Indent`.
    let body = skip_newlines()
        .ignore_then(field_line.repeated().collect::<Vec<_>>())
        .delimited_by(
            skip_newlines().ignore_then(just(Token::Indent)),
            just(Token::Dedent),
        )
        .map(Some);

    // No indented block follows the header: the body is empty. Peek
    // (consuming nothing) so the enclosing statement list still sees
    // the line's newline tail; `validate` below emits the targeted
    // diagnostic. A *malformed* indented block fails both
    // alternatives and falls to statement recovery as a generic
    // parse error instead of misreporting as an empty body.
    let no_body = empty()
        .and_is(require_newlines().then_ignore(just(Token::Indent).not()))
        .to(None);

    select! { Token::Ident(name) => name }
        .map_with(|name, e: &mut Mx<'a, '_, I>| Ident::new(name, e.span()))
        .then_ignore(just(Token::Colon))
        .then(body.or(no_body))
        .validate(|(name, fields), e: &mut Mx<'a, '_, I>, emitter| {
            let fields = fields.unwrap_or_default();
            if fields.is_empty() {
                emitter.emit(Rich::custom(e.span(), ParseDiag::EmptyStructBody));
            }
            (name, fields)
        })
        .boxed()
}

/// A `struct` declaration: `struct Name:` plus the field block (M9).
fn struct_decl_parser<'a, I>() -> impl Parser<'a, I, StmtId, PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    just(Token::Struct)
        .ignore_then(struct_tail_parser())
        .map_with(|(name, fields), e: &mut Mx<'a, '_, I>| {
            let span = e.span();
            e.state()
                .struct_def(name, &fields, StructAttrs::default(), span)
        })
        .boxed()
}

/// One `#[...]` attribute group (M9.1): `#[` ident (`(` ident
/// (`,` ident)* `)`)? `]`. Yields the head identifier, the optional
/// argument list, and the group's span (for diagnostics).
fn attr_group_parser<'a, I>()
-> impl Parser<'a, I, (StringId, Option<Vec<StringId>>, SimpleSpan), PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    let args = just(Token::LParen)
        .ignore_then(
            select! { Token::Ident(name) => name }
                .separated_by(just(Token::Comma))
                .collect::<Vec<_>>(),
        )
        .then_ignore(just(Token::RParen));

    just(Token::HashBracket)
        .ignore_then(select! { Token::Ident(name) => name })
        .then(args.or_not())
        .then_ignore(just(Token::RBracket))
        .map_with(|(name, args), e: &mut Mx<'a, '_, I>| (name, args, e.span()))
        .boxed()
}

/// Top-level statements: struct/enum declarations, function defs, and
/// var decls (plus bare expression statements for flat scripts).
/// Attribute placement lives in `enums::attributed_type_decl_parser`
/// (M9.1, widened to enums in M11).
fn top_level_statement_parser<'a, I>() -> impl Parser<'a, I, StmtId, PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    // Bare expression statements at top level (e.g. `print("hi")`)
    // get wrapped into the synthesized implicit-main body by
    // astgen. This is what makes Pythonic flat scripts feel
    // natural — no `_ = ...` binding required.
    //
    // The expression grammar is built ONCE per `program_parser()`
    // and shared (an `Rc` clone of the `Recursive` handle) by every
    // rule that needs it — function bodies included. Building a full
    // expression grammar allocates the whole combinator tree (and
    // drops it again after the parse), so per-rule
    // `expression_parser()` calls are a measurable fixed cost on
    // every parse.
    let expr = expression_parser();
    // The enum-decl tail embeds `type_expr_parser()` subtrees; build it
    // once here and share it between the plain and attributed
    // enum-declaration rules (same build-once discipline as `expr`).
    let enum_tail = enums::enum_tail_parser();
    let expr_stmt = expr.clone().map_with(|expr, e: &mut Mx<'a, '_, I>| {
        let span = e.span();
        e.state().expr_stmt(expr, span)
    });

    // `struct` / `enum` open with unique keywords, so trying them
    // first is safe and keeps speculation cheap. An attributed
    // declaration starts with `#[` (attribute groups before the
    // declaration, M9.1, widened to enums in M11) and fails just as
    // cheaply anywhere else. Destructuring statements
    // (`a, b = e`, `(a, b) = e`, `{q, r} = e`, M10) overlap only with
    // `var_decl` / `expr_stmt` (both Ident/`(`/`{`-led), so they sit
    // before those two but after the keyword-led forms.
    //
    // Assignment to a field (or positional) target is a body
    // statement only. At the top level, recognize the shape —
    // `ident (.field)+` followed by `=` or a compound-assign op —
    // and fail with a targeted message instead of chumsky's raw
    // expectation dump. The `at_least(1)` segment guard
    // keeps bare `ident = value` a valid top-level var decl
    // (`var_decl_parser`, ahead of us, claims it; the guard is
    // self-defensive regardless of choice order). Deliberately NOT
    // added to `body_statement_parser` — assignments are legal there.
    let assignment_guard = select! { Token::Ident(s) => s }
        .map_with(|s, e: &mut Mx<'a, '_, I>| Ident::new(s, e.span()))
        .then(
            just(Token::Dot)
                .ignore_then(field_key_ident())
                .repeated()
                .at_least(1)
                .collect::<Vec<_>>(),
        )
        .then(choice((
            just(Token::Assign),
            just(Token::PlusAssign),
            just(Token::MinusAssign),
            just(Token::StarAssign),
            just(Token::SlashAssign),
            just(Token::PercentAssign),
        )))
        // Swallow the RHS through end of line: the diagnostic already
        // explains the statement, and a stray expression tail would
        // surface as a second generic parse error. Stop at <indent>
        // — eating it would orphan the matching <dedent> of a
        // deeper-indented following line (same discipline as
        // block_header's swallows).
        .then_ignore(none_of([Token::Newline, Token::Indent]).repeated())
        .validate(|_, e: &mut Mx<'a, '_, I>, emitter| {
            emitter.emit(Rich::custom(
                e.span(),
                ParseDiag::Message("assignment is only valid inside a function body".to_string()),
            ));
        })
        .map_with(|_, e: &mut Mx<'a, '_, I>| {
            let span = e.span();
            e.state().error_stmt(span)
        });

    choice((
        enums::attributed_type_decl_parser(enum_tail.clone()),
        struct_decl_parser(),
        enums::enum_decl_parser(enum_tail),
        function_def_parser(expr.clone()),
        destructure_stmt_parser(expr.clone()),
        var_decl_parser(expr.clone()),
        assignment_guard,
        expr_stmt,
    ))
    .boxed()
}

fn statement_parser<'a, I>() -> impl Parser<'a, I, StmtId, PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    top_level_statement_parser().boxed()
}

/// A plain-name type expression: `str`, `int`, `Point`, or the legacy
/// `&name` view form (M8.4 pre-Q5). Struct declaration fields use
/// this rule — they stay name-only — while the compound anonymous
/// form is added on top in [`type_expr_parser`] for the function
/// signature / variable annotation positions.
fn named_type_expr_parser<'a, I>() -> impl Parser<'a, I, TypeExpr, PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    let view = just(Token::Amp)
        .ignore_then(select! { Token::Ident(name) => name })
        .map_with(|name, e: &mut Mx<'a, '_, I>| TypeExpr::view(name, e.span()));
    let plain = select! { Token::Ident(name) => name }
        .map_with(|name, e: &mut Mx<'a, '_, I>| TypeExpr::new(name, e.span()));
    // The view alternative comes first so the `&` is consumed before
    // the plain form can reject it.
    view.or(plain).boxed()
}

/// Type annotation: a plain name (`str`, `int`, ...), the legacy
/// `&name` view form (M8.4 pre-Q5), a compound anonymous struct
/// type literal `{q: int, r: int}` (M10), or the positional sugar
/// `(int, str)` (M10, ≡ `{0: int, 1: str}`). Post-M8.4.1 the `&`
/// form is retired syntax — it survives here only so astgen can
/// emit the targeted migration error.
///
/// The alternatives are token-disjoint: the name form opens with an
/// identifier (or `&`), the compound form with `{`, the positional
/// form with `(`; chumsky rewinds a failed alternative without
/// consuming input. `{}` stays reserved for the future empty map
/// literal, exactly like the value literal: diagnosed here and
/// recovered as an empty field list so the rest of the file still
/// parses.
///
/// Yields a plain `TypeExpr` value, not a node: annotations are
/// packed into their parent node's `extra` header.
fn type_expr_parser<'a, I>() -> impl Parser<'a, I, TypeExpr, PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    recursive(|ty| {
        // `field_key`, not a bare Ident: brace type literals accept
        // numeric keys (`{0: int}`) with the same canonicalization
        // as value literals, so the brace form spells the same
        // structural type as the `(int, str)` paren sugar.
        let field = field_key().then_ignore(just(Token::Colon)).then(ty.clone());
        let anon = field
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
                let span = e.span();
                e.state().type_expr_anon(&fields, span)
            });
        // Positional sugar `(int, str)` — ≡ `{0: int, 1: str}`. The
        // comma is mandatory even for one element (`(int,)`), so the
        // plain `(int)` spelling never parses here (and stays a
        // syntax error, exactly as before this form existed). Mirrors
        // the value atom: one parse, no speculation — the first
        // element parses once, then the comma decides. Yields
        // `TypeExprKind::Positional`; astgen's resolve path interns
        // the "0"/"1" names pre-dedup.
        let positional = ty
            .clone()
            .then_ignore(just(Token::Comma))
            .then(
                ty.clone()
                    .separated_by(just(Token::Comma))
                    .allow_trailing()
                    .collect::<Vec<_>>(),
            )
            .delimited_by(just(Token::LParen), just(Token::RParen))
            .map_with(|(first, rest), e: &mut Mx<'a, '_, I>| {
                let span = e.span();
                let mut elems = Vec::with_capacity(rest.len() + 1);
                elems.push(first);
                elems.extend(rest);
                e.state().type_expr_positional(&elems, span)
            });
        named_type_expr_parser().or(anon).or(positional).boxed()
    })
}

fn function_def_parser<'a, I>(
    expr: impl Parser<'a, I, ExprId, PExtra<'a>> + Clone + 'a,
) -> impl Parser<'a, I, StmtId, PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    let ident = select! { Token::Ident(name) => name }
        .map_with(|name, e: &mut Mx<'a, '_, I>| Ident::new(name, e.span()));

    let param_mode = choice((
        just(Token::Move).to(ParamMode::Move),
        just(Token::Inout).to(ParamMode::Inout),
    ))
    .or_not()
    .map(|m| m.unwrap_or(ParamMode::Borrow));

    let param = param_mode
        .then(
            select! { Token::Ident(name) => name }
                .map_with(|name, e: &mut Mx<'a, '_, I>| Ident::new(name, e.span())),
        )
        .then_ignore(just(Token::Colon))
        .then(type_expr_parser())
        .map_with(
            |((mode, name), type_annotation), e: &mut Mx<'a, '_, I>| Param {
                name,
                type_annotation,
                mode,
                span: e.span(),
            },
        );

    let params = param
        .separated_by(just(Token::Comma))
        .allow_trailing()
        .collect::<Vec<_>>()
        .delimited_by(just(Token::LParen), just(Token::RParen));

    let return_type = just(Token::Arrow).ignore_then(type_expr_parser()).or_not();

    let body = indented_block(body_statement_parser(expr));

    just(Token::Fn)
        .ignore_then(ident)
        .then(params)
        .then(return_type)
        .then(block_header("fn", "the signature", body))
        .map_with(
            |(((name, params), return_type), body), e: &mut Mx<'a, '_, I>| {
                let span = e.span();
                e.state()
                    .function_def(name, &params, return_type, &body, span)
            },
        )
        .boxed()
}

fn var_decl_parser<'a, I>(
    expr: impl Parser<'a, I, ExprId, PExtra<'a>> + Clone + 'a,
) -> impl Parser<'a, I, StmtId, PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    let mutable = just(Token::Mut).or_not().map(|m| m.is_some());

    let ident = select! { Token::Ident(name) => name }
        .map_with(|name, e: &mut Mx<'a, '_, I>| Ident::new(name, e.span()));

    let type_annotation = just(Token::Colon).ignore_then(type_expr_parser()).or_not();

    mutable
        .then(ident)
        .then(type_annotation)
        .then_ignore(just(Token::Assign))
        .then(expr)
        .map_with(
            |(((mutable, name), type_annotation), initializer), e: &mut Mx<'a, '_, I>| {
                let span = e.span();
                e.state()
                    .var_decl(mutable, name, type_annotation, initializer, span)
            },
        )
        .boxed()
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
        .map_with(|name, e: &mut Mx<'a, '_, I>| Ident::new(name, e.span()))
        .or(
            select! { Token::IntLit(n) => n }.map_with(|n, e: &mut Mx<'a, '_, I>| {
                let name = positional_field_name(n, &mut e.state().pool);
                Ident::new(name, e.span())
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
        .map_with(|name, e: &mut Mx<'a, '_, I>| Ident::new(name, e.span()))
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
    let named_key = select! { Token::Ident(name) => name }
        .map_with(|name, e: &mut Mx<'a, '_, I>| AccessKey::Named(Ident::new(name, e.span())));
    let positional_key = select! { Token::IntLit(n) => n }.map_with(|n, e: &mut Mx<'a, '_, I>| {
        let name = positional_field_name(n, &mut e.state().pool);
        AccessKey::Positional(Ident::new(name, e.span()))
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
                                SimpleSpan::new((), start..span.end),
                            )
                        }
                        None => e.state().method_call(
                            receiver,
                            method.name,
                            &positional,
                            SimpleSpan::new((), start..span.end),
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
                        SimpleSpan::new((), start..span.end),
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
                        SimpleSpan::new((), start..span.end),
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
                        SimpleSpan::new((), start..span.end),
                    )
                }
                PostfixOp::Field(field, span) => {
                    e.state()
                        .field_access(receiver, field, SimpleSpan::new((), start..span.end))
                }
                PostfixOp::Missing => receiver,
                PostfixOp::Slice(lo, hi, span) => {
                    e.state()
                        .slice(receiver, lo, hi, SimpleSpan::new((), start..span.end))
                }
                PostfixOp::Index(index, span) => {
                    e.state()
                        .index(receiver, index, SimpleSpan::new((), start..span.end))
                }
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
                let span = e.span();
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
                    let span = e.span();
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
                .map_with(|name, e: &mut Mx<'a, '_, I>| Ident::new(name, e.span()))
                .then(
                    field_init
                        .clone()
                        .separated_by(just(Token::Comma))
                        .allow_trailing()
                        .collect::<Vec<_>>()
                        .delimited_by(just(Token::LBrace), just(Token::RBrace)),
                )
                .map_with(|(name, fields), e: &mut Mx<'a, '_, I>| {
                    let span = e.span();
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
                    let span = e.span();
                    e.state().struct_literal(None, &fields, span)
                });

            let ident_expr =
                select! { Token::Ident(name) => name }.map_with(|name, e: &mut Mx<'a, '_, I>| {
                    let span = e.span();
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
                    let span = e.span();
                    let mut target = inner;
                    for field in fields {
                        let start = e.state().expr_span(target).start;
                        target = e.state().field_access(
                            target,
                            field,
                            SimpleSpan::new((), start..field.span.end),
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
                        let span = e.span();
                        e.state().struct_literal(None, &[], span)
                    }
                    Some((first, None)) => first,
                    Some((first, Some(rest))) => {
                        let span = e.span();
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
                    let span = e.span();
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
                    let span = e.span();
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
            e.state()
                .binary(op, left, right, SimpleSpan::new((), start..end))
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

#[cfg(test)]
mod tests;

#[cfg(test)]
mod destructure_tests;

#[cfg(test)]
mod enum_tests;
