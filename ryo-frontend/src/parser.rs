//! Surface-syntax parser.
//!
//! Built on chumsky over the lexer's `Token` type. Identifiers,
//! type names, and string literals come pre-interned as `StringId`
//! handles, so the parser only ever copies handles out of tokens —
//! no `to_string` allocations, no `&'a str` slicing into source.
//!
//! The parser builds directly into the [`Ast`] arenas: the `Ast` is
//! threaded as chumsky parser state (`extra::Full<_, Ast, _>`,
//! entered via `parse_with_state`), and node-producing combinators
//! push `Expr`/`Stmt` values through `e.state()` inside
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
use ryo_core::tir::ParamMode;
use ryo_core::types::StringId;

/// Parser extra: `Rich` errors, the [`Ast`] arena as state, no
/// context. Every grammar rule below is parameterized over it.
type PExtra<'a> = extra::Full<Rich<'a, Token>, Ast, ()>;

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

fn assign_or_decl_parser<'a, I>() -> impl Parser<'a, I, StmtId, PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    select! { Token::Ident(s) => s }
        .map_with(|s, e: &mut Mx<'a, '_, I>| Ident {
            name: s,
            span: e.span(),
        })
        .then_ignore(just(Token::Assign))
        .then(expression_parser())
        .map_with(|(target, value), e: &mut Mx<'a, '_, I>| {
            let span = e.span();
            e.state().assign_or_decl(target, value, span)
        })
        .boxed()
}

fn compound_assign_parser<'a, I>() -> impl Parser<'a, I, StmtId, PExtra<'a>> + Clone + 'a
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

    select! { Token::Ident(s) => s }
        .map_with(|s, e: &mut Mx<'a, '_, I>| Ident {
            name: s,
            span: e.span(),
        })
        .then(op)
        .then(expression_parser())
        .map_with(|((target, op), value), e: &mut Mx<'a, '_, I>| {
            let span = e.span();
            e.state().compound_assign(target, op, value, span)
        })
        .boxed()
}

/// Statements valid inside a function body.
fn body_statement_parser<'a, I>() -> impl Parser<'a, I, StmtId, PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    recursive(|body_stmt| {
        let return_stmt = just(Token::Return)
            .ignore_then(expression_parser().or_not())
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
            .ignore_then(expression_parser())
            .then_ignore(just(Token::Colon))
            .then(indented_block(body_stmt.clone()))
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
                expression_parser()
                    .separated_by(just(Token::Comma))
                    .collect::<Vec<_>>(),
            )
            .then_ignore(just(Token::RParen))
            .then_ignore(just(Token::Colon))
            .then(indented_block(body_stmt.clone()))
            .try_map_with(|(((var, iterator), args), body), e: &mut Mx<'a, '_, I>| {
                if args.len() != 2 {
                    return Err(Rich::custom(
                        e.span(),
                        format!(
                            "range() requires two arguments: range(start, end), got {}",
                            args.len()
                        ),
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

        let expr_stmt = expression_parser().map_with(|expr, e: &mut Mx<'a, '_, I>| {
            let span = e.span();
            e.state().expr_stmt(expr, span)
        });

        // Boxed to keep the concrete type small: this parser is stored
        // inside `Recursive`, which keeps the full type in its symbol
        // names (see the note in `expression_parser`).
        choice((
            return_stmt,
            compound_assign_parser(),
            assign_or_decl_parser(),
            var_decl_parser(),
            if_stmt_parser(body_stmt),
            while_stmt,
            for_range_stmt,
            break_stmt,
            continue_stmt,
            expr_stmt,
        ))
        .boxed()
    })
}

fn if_stmt_parser<'a, I>(
    body_stmt: impl Parser<'a, I, StmtId, PExtra<'a>> + Clone + 'a,
) -> impl Parser<'a, I, StmtId, PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    let block = indented_block(body_stmt);

    let elif_branch = skip_newlines()
        .ignore_then(just(Token::Elif))
        .ignore_then(expression_parser())
        .then_ignore(just(Token::Colon))
        .then(block.clone());

    let else_block = skip_newlines()
        .ignore_then(just(Token::Else))
        .ignore_then(just(Token::Colon))
        .ignore_then(block.clone());

    just(Token::If)
        .ignore_then(expression_parser())
        .then_ignore(just(Token::Colon))
        .then(block)
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

/// Top-level statements: only function defs and var decls.
fn top_level_statement_parser<'a, I>() -> impl Parser<'a, I, StmtId, PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    // Bare expression statements at top level (e.g. `print("hi")`)
    // get wrapped into the synthesized implicit-main body by
    // astgen. This is what makes Pythonic flat scripts feel
    // natural — no `_ = ...` binding required.
    let expr_stmt = expression_parser().map_with(|expr, e: &mut Mx<'a, '_, I>| {
        let span = e.span();
        e.state().expr_stmt(expr, span)
    });

    choice((function_def_parser(), var_decl_parser(), expr_stmt)).boxed()
}

fn statement_parser<'a, I>() -> impl Parser<'a, I, StmtId, PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    top_level_statement_parser().boxed()
}

/// Type annotation: a plain name (`str`, `int`, ...) or the legacy
/// `&name` view form (M8.4 pre-Q5). Post-M8.4.1 the `&` form is retired
/// syntax — it survives here only so astgen can emit the targeted
/// migration error. The view alternative comes first so the `&` is
/// consumed before the plain form can reject it.
///
/// Yields a plain `TypeExpr` value, not a node: annotations are
/// packed into their parent node's `extra` header.
fn type_expr_parser<'a, I>() -> impl Parser<'a, I, TypeExpr, PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    let view = just(Token::Amp)
        .ignore_then(select! { Token::Ident(name) => name })
        .map_with(|name, e: &mut Mx<'a, '_, I>| TypeExpr::view(name, e.span()));
    let plain = select! { Token::Ident(name) => name }
        .map_with(|name, e: &mut Mx<'a, '_, I>| TypeExpr::new(name, e.span()));
    view.or(plain).boxed()
}

fn function_def_parser<'a, I>() -> impl Parser<'a, I, StmtId, PExtra<'a>> + Clone + 'a
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

    let body = indented_block(body_statement_parser());

    just(Token::Fn)
        .ignore_then(ident)
        .then(params)
        .then(return_type)
        .then_ignore(just(Token::Colon))
        .then(body)
        .map_with(
            |(((name, params), return_type), body), e: &mut Mx<'a, '_, I>| {
                let span = e.span();
                e.state()
                    .function_def(name, &params, return_type, &body, span)
            },
        )
        .boxed()
}

fn var_decl_parser<'a, I>() -> impl Parser<'a, I, StmtId, PExtra<'a>> + Clone + 'a
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
        .then(expression_parser())
        .map_with(
            |(((mutable, name), type_annotation), initializer), e: &mut Mx<'a, '_, I>| {
                let span = e.span();
                e.state()
                    .var_decl(mutable, name, type_annotation, initializer, span)
            },
        )
        .boxed()
}

fn expression_parser<'a, I>() -> impl Parser<'a, I, ExprId, PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    recursive(|expr| {
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
                    expr.clone()
                        .separated_by(just(Token::Comma))
                        .allow_trailing()
                        .collect::<Vec<_>>()
                        .delimited_by(just(Token::LParen), just(Token::RParen)),
                )
                .map_with(|(name, args), e: &mut Mx<'a, '_, I>| {
                    let span = e.span();
                    e.state().call(name, &args, span)
                });

            let ident_expr =
                select! { Token::Ident(name) => name }.map_with(|name, e: &mut Mx<'a, '_, I>| {
                    let span = e.span();
                    e.state().ident(name, span)
                });

            // `&ident` — call-site mutable-borrow marker (M8.3). Restricted to
            // a bare identifier in v0.1; sema validates the target is an
            // assignable lvalue (`mut` local or `inout` param).
            let borrow = just(Token::Amp).ignore_then(ident_expr).map_with(
                |inner, e: &mut Mx<'a, '_, I>| {
                    let span = e.span();
                    e.state().borrow(inner, span)
                },
            );

            let parenthesized = expr
                .clone()
                .delimited_by(just(Token::LParen), just(Token::RParen));

            borrow.or(call).or(ident_expr).or(literal).or(parenthesized)
        };

        // Postfix operators: method calls (`s.len()`), slice
        // projections `s[start:end]` (M8.4), and scalar indexing
        // `s[i]` (M8.4.2). Either slice bound may be omitted
        // (`s[start:]`, `s[:end]`, `s[:]`); `s[]` is rejected.
        enum PostfixOp {
            Method(StringId, Vec<ExprId>, SimpleSpan),
            Slice(Option<ExprId>, Option<ExprId>, SimpleSpan),
            Index(ExprId, SimpleSpan),
        }

        let method_op = just(Token::Dot)
            .ignore_then(select! { Token::Ident(name) => name })
            .then(
                expr.clone()
                    .separated_by(just(Token::Comma))
                    .allow_trailing()
                    .collect::<Vec<_>>()
                    .delimited_by(just(Token::LParen), just(Token::RParen)),
            )
            .map_with(|(method, args), e: &mut Mx<'a, '_, I>| {
                PostfixOp::Method(method, args, e.span())
            });

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
                    (None, None) => Err(Rich::custom(
                        span,
                        "empty brackets: use s[i] to index or s[start:end] to slice",
                    )),
                }
            });

        let postfix = atom
            .foldl_with(
                choice((method_op, bracket_op)).repeated(),
                |receiver, op, e: &mut Mx<'a, '_, I>| {
                    let start = e.state().expr_span(receiver).start;
                    match op {
                        PostfixOp::Method(method, args, span) => e.state().method_call(
                            receiver,
                            method,
                            &args,
                            SimpleSpan::new((), start..span.end),
                        ),
                        PostfixOp::Slice(lo, hi, span) => {
                            e.state()
                                .slice(receiver, lo, hi, SimpleSpan::new((), start..span.end))
                        }
                        PostfixOp::Index(index, span) => {
                            e.state()
                                .index(receiver, index, SimpleSpan::new((), start..span.end))
                        }
                    }
                },
            )
            .boxed();

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

        // Ordering (non-associative) sits between additive and equality.
        let ordering = additive
            .clone()
            .then(
                choice((
                    just(Token::LtEq).to(BinaryOperator::LtEq),
                    just(Token::GtEq).to(BinaryOperator::GtEq),
                    just(Token::Lt).to(BinaryOperator::Lt),
                    just(Token::Gt).to(BinaryOperator::Gt),
                ))
                .then(additive)
                .or_not(),
            )
            .map_with(|(left, maybe_rhs), e: &mut Mx<'a, '_, I>| match maybe_rhs {
                None => left,
                Some((op, right)) => fold_binary(left, (op, right), e),
            })
            .boxed();

        // Equality is non-associative.
        let equality = ordering
            .clone()
            .then(
                choice((
                    just(Token::EqEq).to(BinaryOperator::Eq),
                    just(Token::NotEq).to(BinaryOperator::NotEq),
                ))
                .then(ordering)
                .or_not(),
            )
            .map_with(|(left, maybe_rhs), e: &mut Mx<'a, '_, I>| match maybe_rhs {
                None => left,
                Some((op, right)) => fold_binary(left, (op, right), e),
            })
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

#[cfg(test)]
mod tests;
