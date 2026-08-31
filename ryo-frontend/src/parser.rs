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

        // Postfix operators: method calls (`s.len()`) and slice
        // projections `s[start:end]` (M8.4). Either slice bound may be
        // omitted (`s[start:]`, `s[:end]`, `s[:]`). `s[]` is rejected
        // grammatically — the colon is mandatory — so no extra
        // validation is needed for the empty slice.
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

        let slice_op = just(Token::LBracket)
            .ignore_then(expr.clone().or_not())
            .then_ignore(just(Token::Colon))
            .then(expr.clone().or_not())
            .then_ignore(just(Token::RBracket))
            .map_with(|(start, end), e: &mut Mx<'a, '_, I>| PostfixOp::Slice(start, end, e.span()));

        // `slice_op` precedes `index_op` because both start with `[`;
        // the colon disambiguates — each input parses exactly one way.
        let index_op = just(Token::LBracket)
            .ignore_then(expr.clone())
            .then_ignore(just(Token::RBracket))
            .map_with(|index, e: &mut Mx<'a, '_, I>| PostfixOp::Index(index, e.span()));

        let postfix = atom
            .foldl_with(
                choice((method_op, slice_op, index_op)).repeated(),
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
mod tests {
    use super::*;
    use crate::lexer::lex;
    use chumsky::Parser;
    use chumsky::input::Input;
    use ryo_core::types::InternPool;

    fn lex_and_parse(input: &str) -> Result<(Ast, InternPool), Vec<Rich<'static, Token>>> {
        let mut pool = InternPool::new();
        let mut sink = ryo_core::diag::DiagSink::new();
        let tokens = lex(input, &mut pool, &mut sink);
        if sink.has_errors() {
            return Err(sink
                .into_diags()
                .into_iter()
                .map(|d| Rich::custom(d.span, d.message))
                .collect());
        }
        let token_stream = tokens[..].split_token_span((0..input.len()).into());

        let mut ast = Ast::new();
        program_parser()
            .parse_with_state(token_stream, &mut ast)
            .into_result()
            .map_err(|e| {
                e.into_iter()
                    .map(|rich| rich.into_owned())
                    .collect::<Vec<_>>()
            })?;
        Ok((ast, pool))
    }

    /// The single top-level statement of a parsed snippet.
    fn only_stmt(ast: &Ast) -> StmtId {
        let stmts = ast.top_level_stmts();
        assert_eq!(stmts.len(), 1);
        stmts[0]
    }

    fn var_decl(ast: &Ast, stmt: StmtId) -> &VarDecl {
        match &ast.stmt(stmt).kind {
            StmtKind::VarDecl(decl) => decl,
            other => panic!("expected VarDecl, got {other:?}"),
        }
    }

    fn fn_def(ast: &Ast, stmt: StmtId) -> &FunctionDef {
        match &ast.stmt(stmt).kind {
            StmtKind::FunctionDef(def) => def,
            other => panic!("expected FunctionDef, got {other:?}"),
        }
    }

    /// Body statements of a parsed function definition.
    fn fn_body<'a>(ast: &'a Ast, def: &FunctionDef) -> &'a [StmtId] {
        ast.stmt_list(def.body)
    }

    fn if_stmt(ast: &Ast, stmt: StmtId) -> &IfStmt {
        match &ast.stmt(stmt).kind {
            StmtKind::IfStmt(if_stmt) => if_stmt,
            other => panic!("expected IfStmt, got {other:?}"),
        }
    }

    /// Body statements of a `while` statement.
    fn while_body(ast: &Ast, stmt: StmtId) -> &[StmtId] {
        match &ast.stmt(stmt).kind {
            StmtKind::WhileLoop { body, .. } => ast.stmt_list(*body),
            other => panic!("expected WhileLoop, got {other:?}"),
        }
    }

    /// `(var, iterator, body)` of a `for` statement.
    fn for_range(ast: &Ast, stmt: StmtId) -> (Ident, Ident, &[StmtId]) {
        match &ast.stmt(stmt).kind {
            StmtKind::ForRange {
                var,
                iterator,
                body,
                ..
            } => (*var, *iterator, ast.stmt_list(*body)),
            other => panic!("expected ForRange, got {other:?}"),
        }
    }

    /// The value of an `x = <value>` AssignOrDecl statement.
    fn assign_value(ast: &Ast, stmt: StmtId) -> ExprId {
        match &ast.stmt(stmt).kind {
            StmtKind::AssignOrDecl { value, .. } => *value,
            other => panic!("expected AssignOrDecl, got {other:?}"),
        }
    }

    /// Initializer of a top-level `x = <init>` declaration.
    fn decl_init(ast: &Ast) -> ExprId {
        var_decl(ast, only_stmt(ast)).initializer
    }

    fn bin_op(ast: &Ast, id: ExprId) -> (ExprId, BinaryOperator, ExprId) {
        match ast.expr(id).kind {
            ExprKind::BinaryOp(lhs, op, rhs) => (lhs, op, rhs),
            other => panic!("expected BinaryOp, got {other:?}"),
        }
    }

    fn assert_int_lit(ast: &Ast, id: ExprId, expected: i64) {
        assert!(
            matches!(ast.expr(id).kind, ExprKind::Literal(Literal::Int(v)) if v == expected),
            "expected Int({expected}), got {:?}",
            ast.expr(id).kind
        );
    }

    #[test]
    fn parse_simple_variable_declaration() {
        let (ast, pool) = lex_and_parse("x = 42").unwrap();
        assert_eq!(ast.top_level_stmts().len(), 1);
        let decl = var_decl(&ast, only_stmt(&ast));
        assert!(!decl.mutable);
        assert_eq!(pool.str(decl.name.name), "x");
        assert!(decl.type_annotation.is_none());
        assert_int_lit(&ast, decl.initializer, 42);
    }

    #[test]
    fn parse_variable_with_type_annotation() {
        let (ast, pool) = lex_and_parse("x: int = 42").unwrap();
        let decl = var_decl(&ast, only_stmt(&ast));
        assert_eq!(pool.str(decl.name.name), "x");
        assert_eq!(pool.str(decl.type_annotation.unwrap().name), "int");
    }

    #[test]
    fn parse_mutable_variable() {
        let (ast, pool) = lex_and_parse("mut x = 42").unwrap();
        let decl = var_decl(&ast, only_stmt(&ast));
        assert!(decl.mutable);
        assert_eq!(pool.str(decl.name.name), "x");
    }

    #[test]
    fn parse_mutable_with_type() {
        let (ast, pool) = lex_and_parse("mut counter: int = 0").unwrap();
        let decl = var_decl(&ast, only_stmt(&ast));
        assert!(decl.mutable);
        assert_eq!(pool.str(decl.name.name), "counter");
        assert_eq!(pool.str(decl.type_annotation.unwrap().name), "int");
        assert_int_lit(&ast, decl.initializer, 0);
    }

    #[test]
    fn parse_expression_addition() {
        let (ast, _) = lex_and_parse("x = 1 + 2").unwrap();
        let (lhs, op, rhs) = bin_op(&ast, decl_init(&ast));
        assert_eq!(op, BinaryOperator::Add);
        assert_int_lit(&ast, lhs, 1);
        assert_int_lit(&ast, rhs, 2);
    }

    #[test]
    fn parse_expression_precedence() {
        let (ast, _) = lex_and_parse("x = 2 + 3 * 4").unwrap();
        let (lhs, op, rhs) = bin_op(&ast, decl_init(&ast));
        assert_eq!(op, BinaryOperator::Add);
        assert_int_lit(&ast, lhs, 2);
        assert_eq!(bin_op(&ast, rhs).1, BinaryOperator::Mul);
    }

    #[test]
    fn parse_expression_negation() {
        let (ast, _) = lex_and_parse("x = -42").unwrap();
        match ast.expr(decl_init(&ast)).kind {
            ExprKind::UnaryOp(op, operand) => {
                assert_eq!(op, UnaryOperator::Neg);
                assert_int_lit(&ast, operand, 42);
            }
            other => panic!("expected UnaryOp, got {other:?}"),
        }
    }

    #[test]
    fn parse_i64_min_literal() {
        // `-9223372036854775808` (i64::MIN): the lexer emits IntLitMin
        // for the overflowing positive form and the parser folds
        // `- IntLitMin` directly to the literal — no UnaryOp.
        let (ast, _) = lex_and_parse("x = -9223372036854775808").unwrap();
        let init = decl_init(&ast);
        assert!(
            matches!(
                ast.expr(init).kind,
                ExprKind::Literal(Literal::Int(i64::MIN))
            ),
            "expected folded i64::MIN literal, got {:?}",
            ast.expr(init).kind
        );
    }

    #[test]
    fn bare_i64_min_magnitude_literal_rejected() {
        // Without the unary `-`, the positive form overflows `i64`
        // and must not parse as an expression.
        assert!(lex_and_parse("x = 9223372036854775808").is_err());
        // …including after a binary minus, where the literal is a
        // fresh operand, not the operand of a unary negation.
        assert!(lex_and_parse("y = 1\nx = y - 9223372036854775808").is_err());
    }

    #[test]
    fn parse_expression_parenthesized() {
        let (ast, _) = lex_and_parse("x = (2 + 3) * 4").unwrap();
        assert_eq!(bin_op(&ast, decl_init(&ast)).1, BinaryOperator::Mul);
    }

    #[test]
    fn parse_multiple_statements() {
        let (ast, _) = lex_and_parse("x = 42\ny = 10").unwrap();
        assert_eq!(ast.top_level_stmts().len(), 2);
    }

    #[test]
    fn parse_multiple_with_types() {
        let (ast, _) = lex_and_parse("x: int = 42\nmut y: float = 3\nz = 1 + 2").unwrap();
        assert_eq!(ast.top_level_stmts().len(), 3);
    }

    #[test]
    fn parse_empty_program() {
        let (ast, _) = lex_and_parse("").unwrap();
        assert_eq!(ast.top_level_stmts().len(), 0);
    }

    #[test]
    fn reject_two_expressions_same_line() {
        assert!(lex_and_parse("x 42").is_err());
    }

    #[test]
    fn reject_multiple_expressions_same_line() {
        assert!(lex_and_parse("x y 42").is_err());
    }

    #[test]
    fn accept_statements_on_separate_lines() {
        let (ast, _) = lex_and_parse("x = 1\ny = 2").unwrap();
        assert_eq!(ast.top_level_stmts().len(), 2);
    }

    #[test]
    fn accept_statement_with_no_trailing_newline() {
        assert!(lex_and_parse("x = 42").is_ok());
    }

    #[test]
    fn accept_statement_with_trailing_newline() {
        assert!(lex_and_parse("x = 42\n").is_ok());
    }

    #[test]
    fn accept_blank_lines_between_statements() {
        let (ast, _) = lex_and_parse("x = 1\n\ny = 2").unwrap();
        assert_eq!(ast.top_level_stmts().len(), 2);
    }

    #[test]
    fn parse_true_false_literals() {
        let (ast, _) = lex_and_parse("x = true\ny = false").unwrap();
        let stmts = ast.top_level_stmts();
        let first = var_decl(&ast, stmts[0]);
        assert!(matches!(
            ast.expr(first.initializer).kind,
            ExprKind::Literal(Literal::Bool(true))
        ));
        let second = var_decl(&ast, stmts[1]);
        assert!(matches!(
            ast.expr(second.initializer).kind,
            ExprKind::Literal(Literal::Bool(false))
        ));
    }

    #[test]
    fn parse_equality_expression() {
        let (ast, _) = lex_and_parse("x = 1 == 2").unwrap();
        assert_eq!(bin_op(&ast, decl_init(&ast)).1, BinaryOperator::Eq);
    }

    #[test]
    fn parse_not_equal_expression() {
        let (ast, _) = lex_and_parse("x = 1 != 2").unwrap();
        assert_eq!(bin_op(&ast, decl_init(&ast)).1, BinaryOperator::NotEq);
    }

    #[test]
    fn parse_equality_has_lower_precedence_than_addition() {
        let (ast, _) = lex_and_parse("x = a + b == c + d").unwrap();
        let (lhs, op, rhs) = bin_op(&ast, decl_init(&ast));
        assert_eq!(op, BinaryOperator::Eq);
        assert_eq!(bin_op(&ast, lhs).1, BinaryOperator::Add);
        assert_eq!(bin_op(&ast, rhs).1, BinaryOperator::Add);
    }

    #[test]
    fn parse_float_literal() {
        let (ast, _) = lex_and_parse("x = 2.5").unwrap();
        match ast.expr(decl_init(&ast)).kind {
            ExprKind::Literal(Literal::Float(v)) => assert!((v - 2.5).abs() < 1e-12),
            other => panic!("expected Float literal, got {:?}", other),
        }
    }

    #[test]
    fn parse_ordering_operators() {
        for (src, expected_op) in &[
            ("x = a < b", BinaryOperator::Lt),
            ("x = a > b", BinaryOperator::Gt),
            ("x = a <= b", BinaryOperator::LtEq),
            ("x = a >= b", BinaryOperator::GtEq),
        ] {
            let (ast, _) = lex_and_parse(src).unwrap();
            assert_eq!(bin_op(&ast, decl_init(&ast)).1, *expected_op);
        }
    }

    #[test]
    fn parse_modulo_at_multiplicative_precedence() {
        let (ast, _) = lex_and_parse("x = a + b % c").unwrap();
        let (_, op, rhs) = bin_op(&ast, decl_init(&ast));
        assert_eq!(op, BinaryOperator::Add);
        assert_eq!(bin_op(&ast, rhs).1, BinaryOperator::Mod);
    }

    #[test]
    fn parse_ordering_below_additive_precedence() {
        let (ast, _) = lex_and_parse("x = a + b < c + d").unwrap();
        let (lhs, op, rhs) = bin_op(&ast, decl_init(&ast));
        assert_eq!(op, BinaryOperator::Lt);
        assert_eq!(bin_op(&ast, lhs).1, BinaryOperator::Add);
        assert_eq!(bin_op(&ast, rhs).1, BinaryOperator::Add);
    }

    #[test]
    fn parse_equality_below_ordering_precedence() {
        let (ast, _) = lex_and_parse("x = a < b == c < d").unwrap();
        let (lhs, op, rhs) = bin_op(&ast, decl_init(&ast));
        assert_eq!(op, BinaryOperator::Eq);
        assert_eq!(bin_op(&ast, lhs).1, BinaryOperator::Lt);
        assert_eq!(bin_op(&ast, rhs).1, BinaryOperator::Lt);
    }

    #[test]
    fn parse_chained_ordering_is_rejected() {
        assert!(lex_and_parse("x = a < b < c").is_err());
    }

    #[test]
    fn parse_chained_equality_is_rejected() {
        assert!(lex_and_parse("x = a == b == c").is_err());
    }

    /// Helper for the escape-table tests: parse a single
    /// `x = "..."` declaration and return the interned bytes of
    /// its string literal.
    fn parse_str_literal(src: &str) -> String {
        let (ast, pool) = lex_and_parse(src).expect("parse ok");
        match ast.expr(decl_init(&ast)).kind {
            ExprKind::Literal(Literal::Str(id)) => pool.str(id).to_string(),
            other => panic!("expected Str literal, got {:?}", other),
        }
    }

    #[test]
    fn string_literal_unescapes_at_lex_time() {
        // Sanity check on the historical case (newline) before
        // sweeping the rest of the escape table below.
        assert_eq!(parse_str_literal("x = \"hello\\n\""), "hello\n");
    }

    #[test]
    fn string_literal_decodes_full_escape_table() {
        // Locks the escape semantics down at the lex layer so the
        // parser can stay a pure pass-through for `Literal::Str`.
        // If a new escape lands (or an existing one changes), it
        // surfaces here rather than at codegen time.
        assert_eq!(parse_str_literal(r#"x = "\n""#), "\n");
        assert_eq!(parse_str_literal(r#"x = "\t""#), "\t");
        assert_eq!(parse_str_literal(r#"x = "\r""#), "\r");
        assert_eq!(parse_str_literal(r#"x = "\\""#), "\\");
        assert_eq!(parse_str_literal(r#"x = "\"""#), "\"");
        assert_eq!(parse_str_literal("x = \"\\0\""), "\0");
    }

    #[test]
    fn parse_and_operator() {
        let (ast, _) = lex_and_parse("x = true and false").unwrap();
        assert_eq!(bin_op(&ast, decl_init(&ast)).1, BinaryOperator::And);
    }

    #[test]
    fn parse_or_operator() {
        let (ast, _) = lex_and_parse("x = true or false").unwrap();
        assert_eq!(bin_op(&ast, decl_init(&ast)).1, BinaryOperator::Or);
    }

    #[test]
    fn parse_not_operator() {
        let (ast, _) = lex_and_parse("x = not true").unwrap();
        let init = decl_init(&ast);
        assert!(
            matches!(
                ast.expr(init).kind,
                ExprKind::UnaryOp(UnaryOperator::Not, _)
            ),
            "expected UnaryOp(Not), got {:?}",
            ast.expr(init).kind
        );
    }

    #[test]
    fn parse_and_binds_tighter_than_or() {
        // a or b and c  =>  a or (b and c)
        let (ast, _) = lex_and_parse("x = true or false and true").unwrap();
        let (_, op, rhs) = bin_op(&ast, decl_init(&ast));
        assert_eq!(op, BinaryOperator::Or);
        assert_eq!(bin_op(&ast, rhs).1, BinaryOperator::And);
    }

    #[test]
    fn parse_not_binds_tighter_than_and() {
        // not a and b  =>  (not a) and b
        let (ast, _) = lex_and_parse("x = not true and false").unwrap();
        let (lhs, op, _) = bin_op(&ast, decl_init(&ast));
        assert_eq!(op, BinaryOperator::And);
        assert!(matches!(
            ast.expr(lhs).kind,
            ExprKind::UnaryOp(UnaryOperator::Not, _)
        ));
    }

    #[test]
    fn parse_not_not_chains() {
        let (ast, _) = lex_and_parse("x = not not true").unwrap();
        let inner = match ast.expr(decl_init(&ast)).kind {
            ExprKind::UnaryOp(op, operand) => {
                assert_eq!(op, UnaryOperator::Not);
                operand
            }
            other => panic!("expected UnaryOp, got {other:?}"),
        };
        assert!(matches!(
            ast.expr(inner).kind,
            ExprKind::UnaryOp(UnaryOperator::Not, _)
        ));
    }

    #[test]
    fn parse_simple_if() {
        let input = "fn main():\n\tif true:\n\t\tx = 1\n";
        let (ast, _) = lex_and_parse(input).unwrap();
        let f = fn_def(&ast, only_stmt(&ast));
        let body = fn_body(&ast, f);
        assert_eq!(body.len(), 1);
        assert!(matches!(ast.stmt(body[0]).kind, StmtKind::IfStmt(_)));
    }

    #[test]
    fn parse_if_else() {
        let input = "fn main():\n\tif true:\n\t\tx = 1\n\telse:\n\t\tx = 2\n";
        let (ast, _) = lex_and_parse(input).unwrap();
        let f = fn_def(&ast, only_stmt(&ast));
        let view = if_stmt(&ast, fn_body(&ast, f)[0]);
        assert!(view.else_block.is_some());
        assert!(ast.elif_list(view.elif_branches).is_empty());
    }

    #[test]
    fn parse_if_elif_else() {
        let input =
            "fn main():\n\tif true:\n\t\tx = 1\n\telif false:\n\t\tx = 2\n\telse:\n\t\tx = 3\n";
        let (ast, _) = lex_and_parse(input).unwrap();
        let f = fn_def(&ast, only_stmt(&ast));
        let view = if_stmt(&ast, fn_body(&ast, f)[0]);
        assert_eq!(ast.elif_list(view.elif_branches).len(), 1);
        assert!(view.else_block.is_some());
    }

    #[test]
    fn parse_multiple_elif() {
        let input = "fn main():\n\tif true:\n\t\tx = 1\n\telif false:\n\t\tx = 2\n\telif true:\n\t\tx = 3\n\telse:\n\t\tx = 4\n";
        let (ast, _) = lex_and_parse(input).unwrap();
        let f = fn_def(&ast, only_stmt(&ast));
        let view = if_stmt(&ast, fn_body(&ast, f)[0]);
        assert_eq!(ast.elif_list(view.elif_branches).len(), 2);
        assert!(view.else_block.is_some());
    }

    #[test]
    fn parse_if_without_else() {
        let input = "fn main():\n\tif true:\n\t\tx = 1\n\tprint(\"done\")\n";
        let (ast, _) = lex_and_parse(input).unwrap();
        let f = fn_def(&ast, only_stmt(&ast));
        assert_eq!(fn_body(&ast, f).len(), 2);
        let view = if_stmt(&ast, fn_body(&ast, f)[0]);
        assert!(view.else_block.is_none());
        assert!(ast.elif_list(view.elif_branches).is_empty());
    }

    #[test]
    fn parse_assign_or_decl() {
        let (ast, pool) = lex_and_parse("fn main():\n\tx = 42\n").unwrap();
        let f = fn_def(&ast, only_stmt(&ast));
        let stmt = fn_body(&ast, f)[0];
        let value = assign_value(&ast, stmt);
        match &ast.stmt(stmt).kind {
            StmtKind::AssignOrDecl { target, .. } => {
                assert_eq!(pool.str(target.name), "x");
            }
            other => panic!("expected AssignOrDecl, got {other:?}"),
        }
        assert_int_lit(&ast, value, 42);
    }

    #[test]
    fn parse_compound_assign_plus() {
        let (ast, pool) = lex_and_parse("fn main():\n\tx += 1\n").unwrap();
        let f = fn_def(&ast, only_stmt(&ast));
        match &ast.stmt(fn_body(&ast, f)[0]).kind {
            StmtKind::CompoundAssign { target, op, .. } => {
                assert_eq!(pool.str(target.name), "x");
                assert_eq!(*op, CompoundOp::Add);
            }
            other => panic!("expected CompoundAssign, got {other:?}"),
        }
    }

    #[test]
    fn parse_all_compound_ops() {
        for (src, expected_op) in [
            ("x += 1", CompoundOp::Add),
            ("x -= 1", CompoundOp::Sub),
            ("x *= 1", CompoundOp::Mul),
            ("x /= 1", CompoundOp::Div),
            ("x %= 1", CompoundOp::Mod),
        ] {
            let code = format!("fn main():\n\t{}\n", src);
            let (ast, _pool) = lex_and_parse(&code).unwrap();
            let f = fn_def(&ast, only_stmt(&ast));
            match &ast.stmt(fn_body(&ast, f)[0]).kind {
                StmtKind::CompoundAssign { op, .. } => {
                    assert_eq!(*op, expected_op, "failed for: {}", src);
                }
                other => panic!("expected CompoundAssign, got {other:?}"),
            }
        }
    }

    #[test]
    fn vardecl_still_works_with_mut() {
        let (ast, pool) = lex_and_parse("fn main():\n\tmut x = 10\n").unwrap();
        let f = fn_def(&ast, only_stmt(&ast));
        let decl = var_decl(&ast, fn_body(&ast, f)[0]);
        assert!(decl.mutable);
        assert_eq!(pool.str(decl.name.name), "x");
    }

    #[test]
    fn vardecl_with_type_annotation_still_works() {
        let (ast, _pool) = lex_and_parse("fn main():\n\tx: int = 10\n").unwrap();
        let f = fn_def(&ast, only_stmt(&ast));
        let decl = var_decl(&ast, fn_body(&ast, f)[0]);
        assert!(!decl.mutable);
        assert!(decl.type_annotation.is_some());
    }

    #[test]
    fn parse_while_loop() {
        let code = "fn main():\n\twhile true:\n\t\tbreak\n";
        let (ast, _pool) = lex_and_parse(code).unwrap();
        let f = fn_def(&ast, only_stmt(&ast));
        let body = fn_body(&ast, f);
        assert_eq!(body.len(), 1);
        assert_eq!(while_body(&ast, body[0]).len(), 1);
    }

    #[test]
    fn parse_break_statement() {
        let code = "fn main():\n\twhile true:\n\t\tbreak\n";
        let (ast, _pool) = lex_and_parse(code).unwrap();
        let f = fn_def(&ast, only_stmt(&ast));
        let body = while_body(&ast, fn_body(&ast, f)[0]);
        assert!(matches!(ast.stmt(body[0]).kind, StmtKind::Break));
    }

    #[test]
    fn parse_continue_statement() {
        let code = "fn main():\n\twhile true:\n\t\tcontinue\n";
        let (ast, _pool) = lex_and_parse(code).unwrap();
        let f = fn_def(&ast, only_stmt(&ast));
        let body = while_body(&ast, fn_body(&ast, f)[0]);
        assert!(matches!(ast.stmt(body[0]).kind, StmtKind::Continue));
    }

    #[test]
    fn parse_nested_while() {
        let code = "fn main():\n\twhile true:\n\t\twhile false:\n\t\t\tbreak\n";
        let (ast, _pool) = lex_and_parse(code).unwrap();
        let f = fn_def(&ast, only_stmt(&ast));
        let body = while_body(&ast, fn_body(&ast, f)[0]);
        assert_eq!(body.len(), 1);
        assert!(matches!(ast.stmt(body[0]).kind, StmtKind::WhileLoop { .. }));
    }

    #[test]
    fn parse_logical_below_equality() {
        // a == b and c == d  =>  (a == b) and (c == d)
        let (ast, _) = lex_and_parse("x = 1 == 2 and 3 == 4").unwrap();
        let (lhs, op, rhs) = bin_op(&ast, decl_init(&ast));
        assert_eq!(op, BinaryOperator::And);
        assert_eq!(bin_op(&ast, lhs).1, BinaryOperator::Eq);
        assert_eq!(bin_op(&ast, rhs).1, BinaryOperator::Eq);
    }

    #[test]
    fn parse_for_range() {
        let code = "fn main():\n\tfor i in range(0, 10):\n\t\tprint(i)\n";
        let (ast, pool) = lex_and_parse(code).unwrap();
        let f = fn_def(&ast, only_stmt(&ast));
        let body = fn_body(&ast, f);
        assert_eq!(body.len(), 1);
        let (var, iterator, loop_body) = for_range(&ast, body[0]);
        assert_eq!(pool.str(var.name), "i");
        assert_eq!(pool.str(iterator.name), "range");
        assert_eq!(loop_body.len(), 1);
    }

    #[test]
    fn parse_for_range_with_expressions() {
        let code = "fn main():\n\tfor x in range(1 + 2, 10 - 3):\n\t\tprint(x)\n";
        let (ast, pool) = lex_and_parse(code).unwrap();
        let f = fn_def(&ast, only_stmt(&ast));
        let (var, iterator, _) = for_range(&ast, fn_body(&ast, f)[0]);
        assert_eq!(pool.str(var.name), "x");
        assert_eq!(pool.str(iterator.name), "range");
    }

    #[test]
    fn parse_for_range_nested() {
        let code =
            "fn main():\n\tfor i in range(0, 5):\n\t\tfor j in range(0, 3):\n\t\t\tprint(i)\n";
        let (ast, _pool) = lex_and_parse(code).unwrap();
        let f = fn_def(&ast, only_stmt(&ast));
        let (_, _, body) = for_range(&ast, fn_body(&ast, f)[0]);
        assert_eq!(body.len(), 1);
        assert!(matches!(ast.stmt(body[0]).kind, StmtKind::ForRange { .. }));
    }

    #[test]
    fn parse_for_break_continue() {
        let code = "fn main():\n\tfor i in range(0, 10):\n\t\tif i == 5:\n\t\t\tbreak\n\t\tif i == 3:\n\t\t\tcontinue\n";
        let (ast, _pool) = lex_and_parse(code).unwrap();
        let f = fn_def(&ast, only_stmt(&ast));
        assert!(matches!(
            ast.stmt(fn_body(&ast, f)[0]).kind,
            StmtKind::ForRange { .. }
        ));
    }

    #[test]
    fn parse_for_range_wrong_arity_one_arg() {
        let code = "fn main():\n\tfor i in range(5):\n\t\tprint(i)\n";
        let result = lex_and_parse(code);
        assert!(result.is_err(), "range(5) should fail with arity error");
    }

    #[test]
    fn parse_for_range_wrong_arity_three_args() {
        let code = "fn main():\n\tfor i in range(0, 10, 2):\n\t\tprint(i)\n";
        let result = lex_and_parse(code);
        assert!(
            result.is_err(),
            "range(0, 10, 2) should fail with arity error"
        );
    }

    #[test]
    fn parse_for_range_wrong_arity_zero_args() {
        let code = "fn main():\n\tfor i in range():\n\t\tprint(i)\n";
        let result = lex_and_parse(code);
        assert!(result.is_err(), "range() should fail with arity error");
    }

    #[test]
    fn parse_move_parameter() {
        let (ast, pool) = lex_and_parse("fn consume(move s: str):\n\tprint(s)\n").unwrap();
        let f = fn_def(&ast, only_stmt(&ast));
        assert_eq!(f.params.len(), 1);
        assert!(
            f.params[0].mode == ParamMode::Move,
            "param `s` should be marked move"
        );
        assert_eq!(pool.str(f.params[0].name.name), "s");
        assert_eq!(pool.str(f.params[0].type_annotation.name), "str");
    }

    #[test]
    fn parse_default_parameter_is_not_move() {
        let (ast, pool) = lex_and_parse("fn read(s: str):\n\tprint(s)\n").unwrap();
        let f = fn_def(&ast, only_stmt(&ast));
        assert_eq!(f.params.len(), 1);
        assert!(
            f.params[0].mode == ParamMode::Borrow,
            "bare param `s` should default to Borrow mode"
        );
        assert_eq!(pool.str(f.params[0].name.name), "s");
        assert_eq!(pool.str(f.params[0].type_annotation.name), "str");
    }

    #[test]
    fn parse_inout_param() {
        let (ast, _pool) = lex_and_parse("fn f(inout x: int):\n\tx += 1\n").unwrap();
        let f = fn_def(&ast, only_stmt(&ast));
        assert_eq!(f.params.len(), 1);
        assert_eq!(
            f.params[0].mode,
            ParamMode::Inout,
            "param `x` should be marked inout"
        );
    }

    #[test]
    fn parse_borrow_arg() {
        let (ast, _pool) = lex_and_parse("fn main():\n\tmut c = 0\n\tf(&c)\n").unwrap();
        let f = fn_def(&ast, only_stmt(&ast));
        // body[1] is the `f(&c)` expression statement.
        let call = match &ast.stmt(fn_body(&ast, f)[1]).kind {
            StmtKind::ExprStmt(value) => *value,
            other => panic!("expected ExprStmt, got {other:?}"),
        };
        match ast.expr(call).kind {
            ExprKind::Call(_, args) => {
                let args = ast.expr_list(args);
                assert_eq!(args.len(), 1);
                assert!(
                    matches!(ast.expr(args[0]).kind, ExprKind::Borrow(_)),
                    "call argument should be a Borrow expression"
                );
            }
            other => panic!("expected Call, got {other:?}"),
        }
    }

    #[test]
    fn parse_slice_full() {
        let (ast, _pool) = lex_and_parse("fn main():\n\tx = s[1:2]\n").unwrap();
        // `fn main():` wraps the body: the slice sits in the value of
        // the body's AssignOrDecl (see `parse_assign_or_decl`).
        let f = fn_def(&ast, only_stmt(&ast));
        let value = assign_value(&ast, fn_body(&ast, f)[0]);
        match ast.expr(value).kind {
            ExprKind::Slice { base, start, end } => {
                assert!(matches!(ast.expr(base).kind, ExprKind::Ident(_)));
                assert!(start.is_some() && end.is_some());
            }
            other => panic!("expected Slice, got {other:?}"),
        }
    }

    #[test]
    fn parse_slice_shorthands() {
        for (src, want_start, want_end) in [
            ("x = s[1:]", true, false),
            ("x = s[:2]", false, true),
            ("x = s[:]", false, false),
        ] {
            let snippet = format!("fn main():\n\t{}\n", src);
            let (ast, _pool) = lex_and_parse(&snippet).unwrap();
            let f = fn_def(&ast, only_stmt(&ast));
            let value = assign_value(&ast, fn_body(&ast, f)[0]);
            match ast.expr(value).kind {
                ExprKind::Slice { start, end, .. } => {
                    assert_eq!(start.is_some(), want_start, "{}: start", src);
                    assert_eq!(end.is_some(), want_end, "{}: end", src);
                }
                other => panic!("expected Slice, got {other:?}"),
            }
        }
    }

    #[test]
    fn parse_slice_after_method_call() {
        let (ast, _pool) = lex_and_parse("fn main():\n\tx = s.len()[0:1]\n").unwrap();
        let f = fn_def(&ast, only_stmt(&ast));
        let value = assign_value(&ast, fn_body(&ast, f)[0]);
        match ast.expr(value).kind {
            ExprKind::Slice { base, .. } => {
                assert!(matches!(ast.expr(base).kind, ExprKind::MethodCall { .. }));
            }
            other => panic!("expected Slice, got {other:?}"),
        }
    }

    #[test]
    fn parse_slice_empty_rejected() {
        assert!(lex_and_parse("fn main():\n\tx = s[]\n").is_err());
    }

    #[test]
    fn parse_slice_three_part_rejected() {
        assert!(lex_and_parse("fn main():\n\tx = s[1:2:3]\n").is_err());
    }

    #[test]
    fn parse_view_param_annotation() {
        let (ast, pool) = lex_and_parse("fn first(text: &str):\n\tprint(text)\n").unwrap();
        let f = fn_def(&ast, only_stmt(&ast));
        assert!(f.params[0].type_annotation.is_view);
        assert_eq!(pool.str(f.params[0].type_annotation.name), "str");
    }

    /// Recovery-aware variant of `lex_and_parse`: returns whether a
    /// (possibly partial) program could be produced, the arena it was
    /// built into, every parse error, and the pool.
    fn lex_and_parse_recovering(input: &str) -> (bool, Ast, Vec<Rich<'static, Token>>, InternPool) {
        let mut pool = InternPool::new();
        let mut sink = ryo_core::diag::DiagSink::new();
        let tokens = lex(input, &mut pool, &mut sink);
        assert!(!sink.has_errors(), "test input must lex cleanly");
        let token_stream = tokens[..].split_token_span((0..input.len()).into());
        let mut ast = Ast::new();
        let (out, errs) = program_parser()
            .parse_with_state(token_stream, &mut ast)
            .into_output_errors();
        (
            out.is_some(),
            ast,
            errs.into_iter().map(|rich| rich.into_owned()).collect(),
            pool,
        )
    }

    /// The statement is a parser-recovery placeholder.
    fn is_error_stmt(ast: &Ast, stmt: StmtId) -> bool {
        matches!(ast.stmt(stmt).kind, StmtKind::Error)
    }

    /// The statement is a variable declaration of either form.
    fn is_decl_stmt(ast: &Ast, stmt: StmtId) -> bool {
        matches!(
            ast.stmt(stmt).kind,
            StmtKind::VarDecl(_) | StmtKind::AssignOrDecl { .. }
        )
    }

    #[test]
    fn recovers_from_bad_statement_between_good_ones() {
        let (ok, ast, errs, _pool) = lex_and_parse_recovering("x = 1\ny = = 2\nz = 3\n");
        assert_eq!(errs.len(), 1, "expected one parse error: {errs:?}");
        assert!(ok, "recovery must produce a partial program");
        let stmts = ast.top_level_stmts();
        assert_eq!(stmts.len(), 3);
        assert!(is_decl_stmt(&ast, stmts[0]));
        assert!(is_error_stmt(&ast, stmts[1]));
        assert!(is_decl_stmt(&ast, stmts[2]));
    }

    #[test]
    fn recovers_inside_function_body() {
        let (ok, ast, errs, _pool) =
            lex_and_parse_recovering("fn main():\n\tx = 1\n\ty = = 2\n\tz = 3\n");
        assert_eq!(errs.len(), 1, "expected one parse error: {errs:?}");
        assert!(ok, "recovery must produce a partial program");
        let func = fn_def(&ast, only_stmt(&ast));
        let body = fn_body(&ast, func);
        assert_eq!(body.len(), 3);
        assert!(is_error_stmt(&ast, body[1]));
        assert!(matches!(
            ast.stmt(body[2]).kind,
            StmtKind::AssignOrDecl { .. }
        ));
    }

    #[test]
    fn reports_multiple_parse_errors_in_one_file() {
        let (ok, ast, errs, _pool) = lex_and_parse_recovering("x = = 1\ny = 2\nz = = 3\nw = 4\n");
        assert_eq!(errs.len(), 2, "expected two parse errors: {errs:?}");
        assert!(ok, "recovery must produce a partial program");
        let stmts = ast.top_level_stmts();
        assert_eq!(stmts.len(), 4);
        assert!(is_error_stmt(&ast, stmts[0]));
        assert!(is_decl_stmt(&ast, stmts[1]));
        assert!(is_error_stmt(&ast, stmts[2]));
        assert!(is_decl_stmt(&ast, stmts[3]));
    }

    #[test]
    fn recovers_from_trailing_garbage_without_newline_at_eof() {
        // File does not end with a newline: the last line is broken.
        let (ok, ast, errs, _pool) = lex_and_parse_recovering("x = 1\ny = = 2");
        assert_eq!(errs.len(), 1, "expected one parse error: {errs:?}");
        assert!(ok, "recovery must produce a partial program");
        let stmts = ast.top_level_stmts();
        assert_eq!(stmts.len(), 2);
        assert!(is_decl_stmt(&ast, stmts[0]));
        assert!(is_error_stmt(&ast, stmts[1]));
    }

    #[test]
    fn recovers_from_broken_block_final_statement_before_dedent() {
        // The last body line is broken and the following top-level
        // statement triggers the block's `Dedent`.
        let (ok, ast, errs, _pool) =
            lex_and_parse_recovering("fn main():\n\tx = 1\n\ty = = 2\nz = 3\n");
        assert_eq!(errs.len(), 1, "expected one parse error: {errs:?}");
        assert!(ok, "recovery must produce a partial program");
        let stmts = ast.top_level_stmts();
        let func = fn_def(&ast, stmts[0]);
        let body = fn_body(&ast, func);
        assert_eq!(body.len(), 2);
        assert!(is_error_stmt(&ast, body[1]));
        assert_eq!(stmts.len(), 2);
    }

    #[test]
    fn recovers_from_broken_block_final_statement_at_eof() {
        // The broken final body line sits directly against the
        // zero-width end-of-input `Dedent` (no terminating newline of
        // its own) — the `peek_terminator(Dedent)` terminator path.
        let (ok, ast, errs, _pool) = lex_and_parse_recovering("fn main():\n\tx = 1\n\ty = = 2");
        assert_eq!(errs.len(), 1, "expected one parse error: {errs:?}");
        assert!(ok, "recovery must produce a partial program");
        let func = fn_def(&ast, only_stmt(&ast));
        let body = fn_body(&ast, func);
        assert_eq!(body.len(), 2);
        assert!(is_error_stmt(&ast, body[1]));
        assert_eq!(ast.top_level_stmts().len(), 1);
    }

    #[test]
    fn parses_unterminated_final_statement_in_block() {
        // The block-final statement has no terminating newline: it
        // ends directly at the list terminator (the end-of-input
        // `Dedent`). Valid input, so no error node and no diagnostic.
        let (ok, ast, errs, _pool) = lex_and_parse_recovering("fn main():\n\tx = 1");
        assert!(errs.is_empty(), "expected a clean parse: {errs:?}");
        assert!(ok, "expected a program");
        let func = fn_def(&ast, only_stmt(&ast));
        let body = fn_body(&ast, func);
        assert_eq!(body.len(), 1);
        assert!(is_decl_stmt(&ast, body[0]));
    }

    #[test]
    fn two_statements_on_one_line_still_error_with_recovery() {
        // The no-two-statements-per-line rule must survive recovery:
        // this is one parse error, not two silent statements.
        let (ok, ast, errs, _pool) = lex_and_parse_recovering("x = 1 y = 2\n");
        assert_eq!(errs.len(), 1, "expected one parse error: {errs:?}");
        assert!(ok, "recovery must produce a partial program");
        let stmts = ast.top_level_stmts();
        assert_eq!(stmts.len(), 1);
        assert!(is_error_stmt(&ast, stmts[0]));
    }

    #[test]
    fn broken_block_header_swallows_body_at_top_level() {
        // The `fn` header is unparseable; its indented body must be
        // swallowed as one region — not parse silently as top-level
        // statements, and not leave a dangling `Dedent` behind.
        let (ok, ast, errs, _pool) =
            lex_and_parse_recovering("fn main(:\n\tx = 1\n\ty = 2\nz = 3\n");
        assert_eq!(errs.len(), 1, "expected one parse error: {errs:?}");
        assert!(ok, "recovery must produce a partial program");
        let stmts = ast.top_level_stmts();
        assert_eq!(stmts.len(), 2);
        assert!(is_error_stmt(&ast, stmts[0]));
        assert!(is_decl_stmt(&ast, stmts[1]));
    }

    #[test]
    fn broken_block_header_inside_block_swallows_body() {
        // Same mis-nesting one level down: the broken `if` header must
        // swallow its body inside the enclosing `fn` block, and the
        // following body line must still parse in that block.
        let (ok, ast, errs, _pool) =
            lex_and_parse_recovering("fn main():\n\tif x(:\n\t\ty = 1\n\tz = 2\n");
        assert_eq!(errs.len(), 1, "expected one parse error: {errs:?}");
        assert!(ok, "recovery must produce a partial program");
        let func = fn_def(&ast, only_stmt(&ast));
        let body = fn_body(&ast, func);
        assert_eq!(body.len(), 2);
        assert!(is_error_stmt(&ast, body[0]));
        assert!(matches!(
            ast.stmt(body[1]).kind,
            StmtKind::AssignOrDecl { .. }
        ));
        assert_eq!(ast.top_level_stmts().len(), 1);
    }

    #[test]
    fn broken_block_header_swallows_nested_blocks() {
        // The swallowed region must be balanced: the inner block's
        // `Dedent` does not end the swallow early.
        let (ok, ast, errs, _pool) =
            lex_and_parse_recovering("while x(:\n\tif y:\n\t\tz = 1\n\tw = 2\nv = 3\n");
        assert_eq!(errs.len(), 1, "expected one parse error: {errs:?}");
        assert!(ok, "recovery must produce a partial program");
        let stmts = ast.top_level_stmts();
        assert_eq!(stmts.len(), 2);
        assert!(is_error_stmt(&ast, stmts[0]));
        assert!(is_decl_stmt(&ast, stmts[1]));
    }

    #[test]
    fn broken_block_header_with_body_at_eof() {
        // No trailing statement: the swallowed body runs into the
        // end-of-input `Dedent`, and that is still a single error.
        let (ok, ast, errs, _pool) = lex_and_parse_recovering("fn main(:\n\tx = 1");
        assert_eq!(errs.len(), 1, "expected one parse error: {errs:?}");
        assert!(ok, "recovery must produce a partial program");
        assert_eq!(ast.top_level_stmts().len(), 1);
        assert!(is_error_stmt(&ast, ast.top_level_stmts()[0]));
    }

    /// Count nodes reachable from `top_level` by following child ids.
    fn reachable_node_counts(ast: &Ast) -> (usize, usize) {
        let mut expr_seen = vec![false; ast.expr_count()];
        let mut stmt_seen = vec![false; ast.stmt_count()];
        let mut expr_work: Vec<ExprId> = Vec::new();
        let mut stmt_work: Vec<StmtId> = ast.top_level_stmts().to_vec();
        while let Some(stmt) = stmt_work.pop() {
            if stmt_seen[stmt.index()] {
                continue;
            }
            stmt_seen[stmt.index()] = true;
            match &ast.stmt(stmt).kind {
                StmtKind::VarDecl(decl) => expr_work.push(decl.initializer),
                StmtKind::FunctionDef(def) => {
                    stmt_work.extend_from_slice(ast.stmt_list(def.body));
                }
                StmtKind::Return(value) => {
                    if let Some(value) = value {
                        expr_work.push(*value);
                    }
                }
                StmtKind::ExprStmt(value) => expr_work.push(*value),
                StmtKind::IfStmt(if_stmt) => {
                    expr_work.push(if_stmt.cond);
                    stmt_work.extend_from_slice(ast.stmt_list(if_stmt.then_block));
                    for elif in ast.elif_list(if_stmt.elif_branches) {
                        expr_work.push(elif.cond);
                        stmt_work.extend_from_slice(ast.stmt_list(elif.block));
                    }
                    if let Some(else_block) = if_stmt.else_block {
                        stmt_work.extend_from_slice(ast.stmt_list(else_block));
                    }
                }
                StmtKind::AssignOrDecl { value, .. } | StmtKind::CompoundAssign { value, .. } => {
                    expr_work.push(*value);
                }
                StmtKind::WhileLoop { cond, body } => {
                    expr_work.push(*cond);
                    stmt_work.extend_from_slice(ast.stmt_list(*body));
                }
                StmtKind::ForRange {
                    start, end, body, ..
                } => {
                    expr_work.push(*start);
                    expr_work.push(*end);
                    stmt_work.extend_from_slice(ast.stmt_list(*body));
                }
                StmtKind::Break | StmtKind::Continue | StmtKind::Error => {}
            }
            while let Some(expr) = expr_work.pop() {
                if expr_seen[expr.index()] {
                    continue;
                }
                expr_seen[expr.index()] = true;
                match ast.expr(expr).kind {
                    ExprKind::Literal(_) | ExprKind::Ident(_) => {}
                    ExprKind::BinaryOp(lhs, _, rhs) => {
                        expr_work.push(lhs);
                        expr_work.push(rhs);
                    }
                    ExprKind::UnaryOp(_, operand) | ExprKind::Borrow(operand) => {
                        expr_work.push(operand);
                    }
                    ExprKind::Call(_, args) => {
                        expr_work.extend_from_slice(ast.expr_list(args));
                    }
                    ExprKind::MethodCall { receiver, args, .. } => {
                        expr_work.push(receiver);
                        expr_work.extend_from_slice(ast.expr_list(args));
                    }
                    ExprKind::Slice { base, start, end } => {
                        expr_work.push(base);
                        if let Some(start) = start {
                            expr_work.push(start);
                        }
                        if let Some(end) = end {
                            expr_work.push(end);
                        }
                    }
                    ExprKind::Index { base, index } => {
                        expr_work.push(base);
                        expr_work.push(index);
                    }
                }
            }
        }
        (
            expr_seen.iter().filter(|&&seen| seen).count(),
            stmt_seen.iter().filter(|&&seen| seen).count(),
        )
    }

    #[test]
    fn successful_parses_leave_no_orphan_nodes() {
        // The `Inspector` hooks on `Ast` are deliberate no-ops for
        // performance (a truncating checkpoint cost ~20% of parse
        // time on `parse_large`). This guards the invariant that
        // makes that safe in practice: on valid input, speculative
        // alternatives fail on their first token without pushing, so
        // every arena slot except the slot-0 sentinel is reachable
        // from `top_level`.
        let examples = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../examples");
        let mut checked = 0;
        for entry in std::fs::read_dir(&examples).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("ryo") {
                continue;
            }
            let src = std::fs::read_to_string(&path).unwrap();
            let Ok((ast, _)) = lex_and_parse(&src) else {
                continue;
            };
            let (exprs, stmts) = reachable_node_counts(&ast);
            assert_eq!(
                exprs + 1,
                ast.expr_count(),
                "orphan expressions in {}",
                path.display()
            );
            assert_eq!(
                stmts + 1,
                ast.stmt_count(),
                "orphan statements in {}",
                path.display()
            );
            checked += 1;
        }
        assert!(checked > 5, "expected to check several example files");
    }
}
