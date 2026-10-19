//! Statement and declaration grammar: newline handling, per-line error
//! recovery, the statement-list driver, and every statement/declaration
//! rule (assignment, var decls, if/while/for, fn defs, struct decls,
//! type annotations). Split out of `parser.rs` to stay under the
//! file-length limit (R2). The expression grammar — atoms, literals,
//! binary precedence, and the postfix fold — stays in `parser.rs`,
//! along with the field-key helpers both sides share.

use super::*;

/// Helper: skip zero or more newline tokens.
pub(super) fn skip_newlines<'a, I>() -> impl Parser<'a, I, (), PExtra<'a>> + Clone + 'a
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
pub(super) fn require_newlines<'a, I>() -> impl Parser<'a, I, (), PExtra<'a>> + Clone + 'a
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
pub(super) fn peek_terminator<'a, I>(
    token: Token,
) -> impl Parser<'a, I, (), PExtra<'a>> + Clone + 'a
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
pub(super) fn statement_list<'a, I>(
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

// The M10 destructuring-statement grammar (`bind_pattern`,
// `destructure_stmt_parser`) lives in `parser/destructure.rs`, and the
// M12 match-expression grammar in `parser/match_.rs` — both split out
// of this file to stay under the file-length limit (R2). The statement
// dispatch below references them through those modules.

/// Statements valid inside a function body.
pub(super) fn body_statement_parser<'a, I>(
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
        // `destructure::destructure_stmt_parser` leads the choice: its
        // forms start with `Ident ,`, `(`, or `{` — the same leading
        // tokens as `compound_assign` / `assign_or_decl` /
        // `var_decl` / `expr_stmt` — so it must claim those lines
        // before any of them can misparse a trailing `= value` as
        // garbage.
        choice((
            destructure::destructure_stmt_parser(expr.clone()),
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
pub(super) fn struct_tail_parser<'a, I>()
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
pub(super) fn attr_group_parser<'a, I>()
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
        destructure::destructure_stmt_parser(expr.clone()),
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
pub(super) fn type_expr_parser<'a, I>() -> impl Parser<'a, I, TypeExpr, PExtra<'a>> + Clone + 'a
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
