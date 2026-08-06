//! Surface-syntax parser.
//!
//! Built on chumsky over the lexer's `Token` type. Identifiers,
//! type names, and string literals come pre-interned as `StringId`
//! handles, so the parser only ever copies handles out of tokens —
//! no `to_string` allocations, no `&'a str` slicing into source.

use chumsky::{input::ValueInput, prelude::*, recovery::via_parser, span::SimpleSpan};

use crate::lexer::Token;
use ryo_core::ast::*;
use ryo_core::tir::ParamMode;
use ryo_core::types::StringId;

/// Helper: skip zero or more newline tokens.
fn skip_newlines<'a, I>() -> impl Parser<'a, I, (), extra::Err<Rich<'a, Token>>> + Clone + 'a
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
fn require_newlines<'a, I>() -> impl Parser<'a, I, (), extra::Err<Rich<'a, Token>>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    select! { Token::Newline => () }
        .repeated()
        .at_least(1)
        .to(())
}

/// Placeholder statement for a line the parser could not parse. The
/// diagnostic comes from the recovered error itself; this node only
/// keeps the partial AST well-formed (astgen lowers it to nothing).
fn error_stmt(span: SimpleSpan) -> Statement {
    Statement {
        span,
        kind: StmtKind::Error,
    }
}

/// Whether a statement line ended cleanly (newline-terminated) or
/// needed garbage-skipping recovery to reach the line boundary.
#[derive(Clone)]
enum LineTail {
    Clean,
    Garbage,
}

/// Positive lookahead: succeed (consuming nothing) only when `token`
/// is next. Block-final statements must not eat the `Dedent` that the
/// surrounding `delimited_by` expects.
fn peek<'a, I>(token: Token) -> impl Parser<'a, I, (), extra::Err<Rich<'a, Token>>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    empty().and_is(just(token)).ignored()
}

/// One garbage token for line-level recovery: anything except the
/// statement boundaries where resynchronization can happen.
fn garbage_token<'a, I>() -> impl Parser<'a, I, (), extra::Err<Rich<'a, Token>>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    none_of([Token::Newline, Token::Dedent]).ignored()
}

/// Skip at least one non-boundary token. Recovering over zero tokens
/// at a clean boundary would emit a spurious error.
fn skip_garbage<'a, I>() -> impl Parser<'a, I, (), extra::Err<Rich<'a, Token>>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    garbage_token().repeated().at_least(1).ignored()
}

/// A list of newline-separated statements with per-line error
/// recovery (R10).
///
/// Shape: blank lines, then `line*`, then an optional unterminated
/// final statement right before `terminator` (`end()` at top level,
/// `peek(Dedent)` in blocks — the lexer emits no Newline before an
/// end-of-input `Dedent`, so block-final statements sit directly
/// against it).
///
/// Each line is `stmt tail`:
/// * if `stmt` itself fails, recovery skips the offending tokens and
///   yields an `Error` node;
/// * if the statement parses but garbage follows on the same line
///   (`y = = 1`), the tail recovery skips to the boundary and the
///   whole line collapses to one `Error` node — a half-parsed
///   statement prefix never survives into the AST.
///
/// Every skip uses `at_least(1)`: recovering over zero tokens at a
/// clean boundary would emit a spurious error. Errors emitted by a
/// recovery whose surrounding line later fails are rolled back by
/// chumsky's rewind, so each broken line reports exactly once.
fn statement_list<'a, I>(
    stmt: impl Parser<'a, I, Statement, extra::Err<Rich<'a, Token>>> + Clone + 'a,
    terminator: impl Parser<'a, I, (), extra::Err<Rich<'a, Token>>> + Clone + 'a,
) -> impl Parser<'a, I, Vec<Statement>, extra::Err<Rich<'a, Token>>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    let stmt_rec = stmt.clone().recover_with(via_parser(
        skip_garbage().map_with(|_, e| error_stmt(e.span())),
    ));

    let tail = require_newlines()
        .to(LineTail::Clean)
        .recover_with(via_parser(
            skip_garbage()
                .then_ignore(require_newlines().or(terminator.clone()))
                .to(LineTail::Garbage),
        ));

    let line = stmt_rec.then(tail).map_with(|(s, tail), e| match tail {
        LineTail::Clean => s,
        LineTail::Garbage => error_stmt(e.span()),
    });

    // Final statement with no terminating newline. Recovery covers a
    // broken final line: skip whatever remains up to the terminator.
    let last = stmt
        .then_ignore(terminator.clone())
        .recover_with(via_parser(
            skip_garbage()
                .then_ignore(terminator)
                .map_with(|_, e| error_stmt(e.span())),
        ))
        .or_not();

    skip_newlines()
        .ignore_then(line.repeated().collect::<Vec<_>>())
        .then(skip_newlines().ignore_then(last))
        .map(|(mut lines, last)| {
            lines.extend(last);
            lines
        })
}

/// Parse a complete Ryo program with multiple statements.
pub fn program_parser<'a, I>() -> impl Parser<'a, I, Program, extra::Err<Rich<'a, Token>>> + 'a
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
        .map_with(|statements, _e| {
            let span = if statements.is_empty() {
                SimpleSpan::new((), 0..0)
            } else {
                let start = statements.first().unwrap().span.start;
                let end = statements.last().unwrap().span.end;
                SimpleSpan::new((), start..end)
            };
            Program { statements, span }
        })
}

/// Parse an indented block of one or more statements.
fn indented_block<'a, I>(
    stmt: impl Parser<'a, I, Statement, extra::Err<Rich<'a, Token>>> + Clone + 'a,
) -> impl Parser<'a, I, Vec<Statement>, extra::Err<Rich<'a, Token>>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    statement_list(stmt, peek(Token::Dedent)).delimited_by(
        skip_newlines().ignore_then(just(Token::Indent)),
        just(Token::Dedent),
    )
}

fn assign_or_decl_parser<'a, I>()
-> impl Parser<'a, I, Statement, extra::Err<Rich<'a, Token>>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    select! { Token::Ident(s) => s }
        .map_with(|s, e| Ident {
            name: s,
            span: e.span(),
        })
        .then_ignore(just(Token::Assign))
        .then(expression_parser())
        .map_with(|(target, value), e| Statement {
            span: e.span(),
            kind: StmtKind::AssignOrDecl { target, value },
        })
}

fn compound_assign_parser<'a, I>()
-> impl Parser<'a, I, Statement, extra::Err<Rich<'a, Token>>> + Clone + 'a
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
        .map_with(|s, e| Ident {
            name: s,
            span: e.span(),
        })
        .then(op)
        .then(expression_parser())
        .map_with(|((target, op), value), e| Statement {
            span: e.span(),
            kind: StmtKind::CompoundAssign { target, op, value },
        })
}

/// Statements valid inside a function body.
fn body_statement_parser<'a, I>()
-> impl Parser<'a, I, Statement, extra::Err<Rich<'a, Token>>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    recursive(|body_stmt| {
        let return_stmt = just(Token::Return)
            .ignore_then(expression_parser().or_not())
            .map_with(|expr, e| Statement {
                span: e.span(),
                kind: StmtKind::Return(expr),
            });

        let var_decl = var_decl_parser().map_with(|kind, e| Statement {
            span: e.span(),
            kind: StmtKind::VarDecl(kind),
        });

        let break_stmt = just(Token::Break).map_with(|_, e| Statement {
            span: e.span(),
            kind: StmtKind::Break,
        });

        let continue_stmt = just(Token::Continue).map_with(|_, e| Statement {
            span: e.span(),
            kind: StmtKind::Continue,
        });

        let while_stmt = just(Token::While)
            .ignore_then(expression_parser())
            .then_ignore(just(Token::Colon))
            .then(indented_block(body_stmt.clone()))
            .map_with(|(cond, body), e| Statement {
                span: e.span(),
                kind: StmtKind::WhileLoop { cond, body },
            });

        let for_range_stmt = just(Token::For)
            .ignore_then(select! { Token::Ident(s) => s }.map_with(|s, e| Ident {
                name: s,
                span: e.span(),
            }))
            .then_ignore(just(Token::In))
            .then(select! { Token::Ident(s) => s }.map_with(|s, e| Ident {
                name: s,
                span: e.span(),
            }))
            .then_ignore(just(Token::LParen))
            .then(
                expression_parser()
                    .separated_by(just(Token::Comma))
                    .collect::<Vec<_>>(),
            )
            .then_ignore(just(Token::RParen))
            .then_ignore(just(Token::Colon))
            .then(indented_block(body_stmt.clone()))
            .try_map_with(|(((var, iterator), args), body), e| {
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
                let start = args.next().unwrap();
                let end = args.next().unwrap();
                Ok(Statement {
                    span: e.span(),
                    kind: StmtKind::ForRange {
                        var,
                        iterator,
                        start,
                        end,
                        body,
                    },
                })
            });

        let if_stmt = if_stmt_parser(body_stmt).map_with(|if_s, e| Statement {
            span: e.span(),
            kind: StmtKind::IfStmt(if_s),
        });

        let expr_stmt = expression_parser().map_with(|expr, e| Statement {
            span: e.span(),
            kind: StmtKind::ExprStmt(expr),
        });

        choice((
            return_stmt,
            compound_assign_parser(),
            assign_or_decl_parser(),
            var_decl,
            if_stmt,
            while_stmt,
            for_range_stmt,
            break_stmt,
            continue_stmt,
            expr_stmt,
        ))
    })
}

fn if_stmt_parser<'a, I>(
    body_stmt: impl Parser<'a, I, Statement, extra::Err<Rich<'a, Token>>> + Clone + 'a,
) -> impl Parser<'a, I, IfStmt, extra::Err<Rich<'a, Token>>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    let block = indented_block(body_stmt);

    let elif_branch = skip_newlines()
        .ignore_then(just(Token::Elif))
        .ignore_then(expression_parser())
        .then_ignore(just(Token::Colon))
        .then(block.clone())
        .map(|(cond, block)| ElifBranch { cond, block });

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
        .map(|(((cond, then_block), elif_branches), else_block)| IfStmt {
            cond,
            then_block,
            elif_branches,
            else_block,
        })
}

/// Top-level statements: only function defs and var decls.
fn top_level_statement_parser<'a, I>()
-> impl Parser<'a, I, Statement, extra::Err<Rich<'a, Token>>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    let function_def = function_def_parser().map_with(|func, e| Statement {
        span: e.span(),
        kind: StmtKind::FunctionDef(func),
    });

    let var_decl = var_decl_parser().map_with(|kind, e| Statement {
        span: e.span(),
        kind: StmtKind::VarDecl(kind),
    });

    // Bare expression statements at top level (e.g. `print("hi")`)
    // get wrapped into the synthesized implicit-main body by
    // astgen. This is what makes Pythonic flat scripts feel
    // natural — no `_ = ...` binding required.
    let expr_stmt = expression_parser().map_with(|expr, e| Statement {
        span: e.span(),
        kind: StmtKind::ExprStmt(expr),
    });

    choice((function_def, var_decl, expr_stmt))
}

fn statement_parser<'a, I>()
-> impl Parser<'a, I, Statement, extra::Err<Rich<'a, Token>>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    top_level_statement_parser()
}

/// Type annotation: a plain name (`str`, `int`, ...) or the legacy
/// `&name` view form (M8.4 pre-Q5). Post-M8.4.1 the `&` form is retired
/// syntax — it survives here only so astgen can emit the targeted
/// migration error. The view alternative comes first so the `&` is
/// consumed before the plain form can reject it.
fn type_expr_parser<'a, I>()
-> impl Parser<'a, I, TypeExpr, extra::Err<Rich<'a, Token>>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    let view = just(Token::Amp)
        .ignore_then(select! { Token::Ident(name) => name })
        .map_with(|name, e| TypeExpr::view(name, e.span()));
    let plain =
        select! { Token::Ident(name) => name }.map_with(|name, e| TypeExpr::new(name, e.span()));
    view.or(plain)
}

fn function_def_parser<'a, I>()
-> impl Parser<'a, I, FunctionDef, extra::Err<Rich<'a, Token>>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    let ident =
        select! { Token::Ident(name) => name }.map_with(|name, e| Ident::new(name, e.span()));

    let param_mode = choice((
        just(Token::Move).to(ParamMode::Move),
        just(Token::Inout).to(ParamMode::Inout),
    ))
    .or_not()
    .map(|m| m.unwrap_or(ParamMode::Borrow));

    let param = param_mode
        .then(select! { Token::Ident(name) => name }.map_with(|name, e| Ident::new(name, e.span())))
        .then_ignore(just(Token::Colon))
        .then(type_expr_parser())
        .map_with(|((mode, name), type_annotation), e| Param {
            name,
            type_annotation,
            mode,
            span: e.span(),
        });

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
        .map(|(((name, params), return_type), body)| FunctionDef {
            name,
            params,
            return_type,
            body,
        })
}

fn var_decl_parser<'a, I>() -> impl Parser<'a, I, VarDecl, extra::Err<Rich<'a, Token>>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    let mutable = just(Token::Mut).or_not().map(|m| m.is_some());

    let ident =
        select! { Token::Ident(name) => name }.map_with(|name, e| Ident::new(name, e.span()));

    let type_annotation = just(Token::Colon).ignore_then(type_expr_parser()).or_not();

    mutable
        .then(ident)
        .then(type_annotation)
        .then_ignore(just(Token::Assign))
        .then(expression_parser())
        .map(
            |(((mutable, name), type_annotation), initializer)| VarDecl {
                mutable,
                name,
                type_annotation,
                initializer,
            },
        )
}

fn expression_parser<'a, I>()
-> impl Parser<'a, I, Expression, extra::Err<Rich<'a, Token>>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    recursive(|expr| {
        let atom = {
            let literal = select! {
                Token::IntLit(n) => ExprKind::Literal(Literal::Int(n)),
                Token::FloatLit(bits) => ExprKind::Literal(Literal::Float(f64::from_bits(bits))),
                Token::StrLit(id) => ExprKind::Literal(Literal::Str(id)),
                Token::True => ExprKind::Literal(Literal::Bool(true)),
                Token::False => ExprKind::Literal(Literal::Bool(false)),
            }
            .map_with(|kind, e| Expression::new(kind, e.span()));

            let call = select! { Token::Ident(name) => name }
                .then(
                    expr.clone()
                        .separated_by(just(Token::Comma))
                        .allow_trailing()
                        .collect::<Vec<_>>()
                        .delimited_by(just(Token::LParen), just(Token::RParen)),
                )
                .map_with(|(name, args), e| Expression::new(ExprKind::Call(name, args), e.span()));

            let ident_expr = select! { Token::Ident(name) => name }
                .map_with(|name, e| Expression::new(ExprKind::Ident(name), e.span()));

            // `&ident` — call-site mutable-borrow marker (M8.3). Restricted to
            // a bare identifier in v0.1; sema validates the target is an
            // assignable lvalue (`mut` local or `inout` param).
            let borrow = just(Token::Amp)
                .ignore_then(ident_expr)
                .map_with(|inner, e| Expression::new(ExprKind::Borrow(Box::new(inner)), e.span()));

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
            Method(StringId, Vec<Expression>, SimpleSpan),
            Slice(Option<Expression>, Option<Expression>, SimpleSpan),
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
            .map_with(|(method, args), e| PostfixOp::Method(method, args, e.span()));

        let slice_op = just(Token::LBracket)
            .ignore_then(expr.clone().or_not())
            .then_ignore(just(Token::Colon))
            .then(expr.clone().or_not())
            .then_ignore(just(Token::RBracket))
            .map_with(|(start, end), e| PostfixOp::Slice(start, end, e.span()));

        let postfix = atom
            .foldl(choice((method_op, slice_op)).repeated(), |receiver, op| {
                let start = receiver.span.start;
                match op {
                    PostfixOp::Method(method, args, span) => Expression::new(
                        ExprKind::MethodCall {
                            receiver: Box::new(receiver),
                            method,
                            args,
                        },
                        SimpleSpan::new((), start..span.end),
                    ),
                    PostfixOp::Slice(lo, hi, span) => Expression::new(
                        ExprKind::Slice {
                            base: Box::new(receiver),
                            start: lo.map(Box::new),
                            end: hi.map(Box::new),
                        },
                        SimpleSpan::new((), start..span.end),
                    ),
                }
            })
            .boxed();

        let unary_op = choice((
            just(Token::Sub).to(UnaryOperator::Neg),
            just(Token::Not).to(UnaryOperator::Not),
        ));

        // `- IntLitMin` is folded to the `i64::MIN` literal at parse
        // time: the positive form `9223372036854775808` overflows
        // `i64`, so the lexer marks it with a dedicated token that is
        // only grammatical directly after unary `-`.
        let neg_min = just(Token::Sub)
            .then(just(Token::IntLitMin))
            .map_with(|_, e| Expression::new(ExprKind::Literal(Literal::Int(i64::MIN)), e.span()));

        let unary = neg_min.or(unary_op
            .repeated()
            .collect::<Vec<_>>()
            .then(postfix)
            .map_with(|(ops, expr), e| {
                let mut result = expr;
                for op in ops.into_iter().rev() {
                    result = Expression::new(ExprKind::UnaryOp(op, Box::new(result)), e.span());
                }
                result
            }));

        let term = unary.clone().foldl(
            choice((
                just(Token::Mul).to(BinaryOperator::Mul),
                just(Token::Div).to(BinaryOperator::Div),
                just(Token::Percent).to(BinaryOperator::Mod),
            ))
            .then(unary)
            .repeated(),
            |left, (op, right)| {
                let start = left.span.start;
                let end = right.span.end;
                Expression::new(
                    ExprKind::BinaryOp(Box::new(left), op, Box::new(right)),
                    SimpleSpan::new((), start..end),
                )
            },
        );

        let additive = term.clone().foldl(
            choice((
                just(Token::Add).to(BinaryOperator::Add),
                just(Token::Sub).to(BinaryOperator::Sub),
            ))
            .then(term)
            .repeated(),
            |left, (op, right)| {
                let start = left.span.start;
                let end = right.span.end;
                Expression::new(
                    ExprKind::BinaryOp(Box::new(left), op, Box::new(right)),
                    SimpleSpan::new((), start..end),
                )
            },
        );

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
            .map(|(left, maybe_rhs)| match maybe_rhs {
                None => left,
                Some((op, right)) => {
                    let start = left.span.start;
                    let end = right.span.end;
                    Expression::new(
                        ExprKind::BinaryOp(Box::new(left), op, Box::new(right)),
                        SimpleSpan::new((), start..end),
                    )
                }
            });

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
            .map(|(left, maybe_rhs)| match maybe_rhs {
                None => left,
                Some((op, right)) => {
                    let start = left.span.start;
                    let end = right.span.end;
                    Expression::new(
                        ExprKind::BinaryOp(Box::new(left), op, Box::new(right)),
                        SimpleSpan::new((), start..end),
                    )
                }
            });

        // Logical AND binds tighter than OR, below equality.
        let logical_and = equality.clone().foldl(
            just(Token::And)
                .to(BinaryOperator::And)
                .then(equality)
                .repeated(),
            |left, (op, right)| {
                let start = left.span.start;
                let end = right.span.end;
                Expression::new(
                    ExprKind::BinaryOp(Box::new(left), op, Box::new(right)),
                    SimpleSpan::new((), start..end),
                )
            },
        );

        // Logical OR is the lowest precedence.
        logical_and.clone().foldl(
            just(Token::Or)
                .to(BinaryOperator::Or)
                .then(logical_and)
                .repeated(),
            |left, (op, right)| {
                let start = left.span.start;
                let end = right.span.end;
                Expression::new(
                    ExprKind::BinaryOp(Box::new(left), op, Box::new(right)),
                    SimpleSpan::new((), start..end),
                )
            },
        )
    })
}

#[cfg(test)]
#[allow(irrefutable_let_patterns)]
mod tests {
    use super::*;
    use crate::lexer::lex;
    use chumsky::Parser;
    use chumsky::input::Input;
    use ryo_core::types::InternPool;

    fn lex_and_parse(input: &str) -> Result<(Program, InternPool), Vec<Rich<'static, Token>>> {
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

        let program = program_parser()
            .parse(token_stream)
            .into_result()
            .map_err(|e| {
                e.into_iter()
                    .map(|rich| rich.into_owned())
                    .collect::<Vec<_>>()
            })?;
        Ok((program, pool))
    }

    fn ident_text<'a>(id: &Ident, pool: &'a InternPool) -> &'a str {
        pool.str(id.name)
    }

    #[test]
    fn parse_simple_variable_declaration() {
        let (program, pool) = lex_and_parse("x = 42").unwrap();
        assert_eq!(program.statements.len(), 1);
        if let StmtKind::VarDecl(decl) = &program.statements[0].kind {
            assert!(!decl.mutable);
            assert_eq!(ident_text(&decl.name, &pool), "x");
            assert!(decl.type_annotation.is_none());
            assert!(matches!(
                decl.initializer.kind,
                ExprKind::Literal(Literal::Int(42))
            ));
        } else {
            panic!("Expected VarDecl");
        }
    }

    #[test]
    fn parse_variable_with_type_annotation() {
        let (program, pool) = lex_and_parse("x: int = 42").unwrap();
        if let StmtKind::VarDecl(decl) = &program.statements[0].kind {
            assert_eq!(ident_text(&decl.name, &pool), "x");
            assert_eq!(pool.str(decl.type_annotation.as_ref().unwrap().name), "int");
        } else {
            panic!("Expected VarDecl");
        }
    }

    #[test]
    fn parse_mutable_variable() {
        let (program, pool) = lex_and_parse("mut x = 42").unwrap();
        if let StmtKind::VarDecl(decl) = &program.statements[0].kind {
            assert!(decl.mutable);
            assert_eq!(ident_text(&decl.name, &pool), "x");
        } else {
            panic!("Expected VarDecl");
        }
    }

    #[test]
    fn parse_mutable_with_type() {
        let (program, pool) = lex_and_parse("mut counter: int = 0").unwrap();
        if let StmtKind::VarDecl(decl) = &program.statements[0].kind {
            assert!(decl.mutable);
            assert_eq!(ident_text(&decl.name, &pool), "counter");
            assert_eq!(pool.str(decl.type_annotation.as_ref().unwrap().name), "int");
            assert!(matches!(
                decl.initializer.kind,
                ExprKind::Literal(Literal::Int(0))
            ));
        } else {
            panic!("Expected VarDecl");
        }
    }

    #[test]
    fn parse_expression_addition() {
        let (program, _) = lex_and_parse("x = 1 + 2").unwrap();
        if let StmtKind::VarDecl(decl) = &program.statements[0].kind {
            match &decl.initializer.kind {
                ExprKind::BinaryOp(left, BinaryOperator::Add, right) => {
                    assert!(matches!(left.kind, ExprKind::Literal(Literal::Int(1))));
                    assert!(matches!(right.kind, ExprKind::Literal(Literal::Int(2))));
                }
                _ => panic!("Expected BinaryOp(Add)"),
            }
        } else {
            panic!("Expected VarDecl");
        }
    }

    #[test]
    fn parse_expression_precedence() {
        let (program, _) = lex_and_parse("x = 2 + 3 * 4").unwrap();
        if let StmtKind::VarDecl(decl) = &program.statements[0].kind {
            match &decl.initializer.kind {
                ExprKind::BinaryOp(left, BinaryOperator::Add, right) => {
                    assert!(matches!(left.kind, ExprKind::Literal(Literal::Int(2))));
                    assert!(matches!(
                        right.kind,
                        ExprKind::BinaryOp(_, BinaryOperator::Mul, _)
                    ));
                }
                _ => panic!("Expected BinaryOp(Add)"),
            }
        } else {
            panic!("Expected VarDecl");
        }
    }

    #[test]
    fn parse_expression_negation() {
        let (program, _) = lex_and_parse("x = -42").unwrap();
        if let StmtKind::VarDecl(decl) = &program.statements[0].kind {
            match &decl.initializer.kind {
                ExprKind::UnaryOp(UnaryOperator::Neg, expr) => {
                    assert!(matches!(expr.kind, ExprKind::Literal(Literal::Int(42))));
                }
                _ => panic!("Expected UnaryOp(Neg)"),
            }
        } else {
            panic!("Expected VarDecl");
        }
    }

    #[test]
    fn parse_i64_min_literal() {
        // `-9223372036854775808` (i64::MIN): the lexer emits IntLitMin
        // for the overflowing positive form and the parser folds
        // `- IntLitMin` directly to the literal — no UnaryOp.
        let (program, _) = lex_and_parse("x = -9223372036854775808").unwrap();
        if let StmtKind::VarDecl(decl) = &program.statements[0].kind {
            assert!(
                matches!(
                    decl.initializer.kind,
                    ExprKind::Literal(Literal::Int(i64::MIN))
                ),
                "expected folded i64::MIN literal, got {:?}",
                decl.initializer.kind
            );
        } else {
            panic!("Expected VarDecl");
        }
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
        let (program, _) = lex_and_parse("x = (2 + 3) * 4").unwrap();
        if let StmtKind::VarDecl(decl) = &program.statements[0].kind {
            assert!(matches!(
                decl.initializer.kind,
                ExprKind::BinaryOp(_, BinaryOperator::Mul, _)
            ));
        } else {
            panic!("Expected VarDecl");
        }
    }

    #[test]
    fn parse_multiple_statements() {
        let (program, _) = lex_and_parse("x = 42\ny = 10").unwrap();
        assert_eq!(program.statements.len(), 2);
    }

    #[test]
    fn parse_multiple_with_types() {
        let (program, _) = lex_and_parse("x: int = 42\nmut y: float = 3\nz = 1 + 2").unwrap();
        assert_eq!(program.statements.len(), 3);
    }

    #[test]
    fn parse_empty_program() {
        let (program, _) = lex_and_parse("").unwrap();
        assert_eq!(program.statements.len(), 0);
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
        let (program, _) = lex_and_parse("x = 1\ny = 2").unwrap();
        assert_eq!(program.statements.len(), 2);
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
        let (program, _) = lex_and_parse("x = 1\n\ny = 2").unwrap();
        assert_eq!(program.statements.len(), 2);
    }

    #[test]
    fn parse_true_false_literals() {
        let (program, _) = lex_and_parse("x = true\ny = false").unwrap();
        assert_eq!(program.statements.len(), 2);
        match &program.statements[0].kind {
            StmtKind::VarDecl(decl) => {
                assert!(matches!(
                    decl.initializer.kind,
                    ExprKind::Literal(Literal::Bool(true))
                ));
            }
            other => panic!("expected VarDecl, got {:?}", other),
        }
        match &program.statements[1].kind {
            StmtKind::VarDecl(decl) => {
                assert!(matches!(
                    decl.initializer.kind,
                    ExprKind::Literal(Literal::Bool(false))
                ));
            }
            other => panic!("expected VarDecl, got {:?}", other),
        }
    }

    #[test]
    fn parse_equality_expression() {
        let (program, _) = lex_and_parse("x = 1 == 2").unwrap();
        match &program.statements[0].kind {
            StmtKind::VarDecl(decl) => assert!(matches!(
                decl.initializer.kind,
                ExprKind::BinaryOp(_, BinaryOperator::Eq, _)
            )),
            other => panic!("expected VarDecl, got {:?}", other),
        }
    }

    #[test]
    fn parse_not_equal_expression() {
        let (program, _) = lex_and_parse("x = 1 != 2").unwrap();
        match &program.statements[0].kind {
            StmtKind::VarDecl(decl) => assert!(matches!(
                decl.initializer.kind,
                ExprKind::BinaryOp(_, BinaryOperator::NotEq, _)
            )),
            other => panic!("expected VarDecl, got {:?}", other),
        }
    }

    #[test]
    fn parse_equality_has_lower_precedence_than_addition() {
        let (program, _) = lex_and_parse("x = a + b == c + d").unwrap();
        match &program.statements[0].kind {
            StmtKind::VarDecl(decl) => match &decl.initializer.kind {
                ExprKind::BinaryOp(lhs, BinaryOperator::Eq, rhs) => {
                    assert!(matches!(
                        lhs.kind,
                        ExprKind::BinaryOp(_, BinaryOperator::Add, _)
                    ));
                    assert!(matches!(
                        rhs.kind,
                        ExprKind::BinaryOp(_, BinaryOperator::Add, _)
                    ));
                }
                other => panic!("expected top-level BinaryOp(Eq), got {:?}", other),
            },
            other => panic!("expected VarDecl, got {:?}", other),
        }
    }

    #[test]
    fn parse_float_literal() {
        let (program, _) = lex_and_parse("x = 2.5").unwrap();
        match &program.statements[0].kind {
            StmtKind::VarDecl(decl) => match &decl.initializer.kind {
                ExprKind::Literal(Literal::Float(v)) => assert!((*v - 2.5).abs() < 1e-12),
                other => panic!("expected Float literal, got {:?}", other),
            },
            other => panic!("expected VarDecl, got {:?}", other),
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
            let (program, _) = lex_and_parse(src).unwrap();
            match &program.statements[0].kind {
                StmtKind::VarDecl(decl) => match &decl.initializer.kind {
                    ExprKind::BinaryOp(_, op, _) => assert_eq!(op, expected_op),
                    other => panic!("expected BinaryOp, got {:?}", other),
                },
                other => panic!("expected VarDecl, got {:?}", other),
            }
        }
    }

    #[test]
    fn parse_modulo_at_multiplicative_precedence() {
        let (program, _) = lex_and_parse("x = a + b % c").unwrap();
        match &program.statements[0].kind {
            StmtKind::VarDecl(decl) => match &decl.initializer.kind {
                ExprKind::BinaryOp(_, BinaryOperator::Add, rhs) => {
                    assert!(matches!(
                        rhs.kind,
                        ExprKind::BinaryOp(_, BinaryOperator::Mod, _)
                    ));
                }
                other => panic!("expected top-level BinaryOp(Add), got {:?}", other),
            },
            other => panic!("expected VarDecl, got {:?}", other),
        }
    }

    #[test]
    fn parse_ordering_below_additive_precedence() {
        let (program, _) = lex_and_parse("x = a + b < c + d").unwrap();
        match &program.statements[0].kind {
            StmtKind::VarDecl(decl) => match &decl.initializer.kind {
                ExprKind::BinaryOp(lhs, BinaryOperator::Lt, rhs) => {
                    assert!(matches!(
                        lhs.kind,
                        ExprKind::BinaryOp(_, BinaryOperator::Add, _)
                    ));
                    assert!(matches!(
                        rhs.kind,
                        ExprKind::BinaryOp(_, BinaryOperator::Add, _)
                    ));
                }
                other => panic!("expected top-level BinaryOp(Lt), got {:?}", other),
            },
            other => panic!("expected VarDecl, got {:?}", other),
        }
    }

    #[test]
    fn parse_equality_below_ordering_precedence() {
        let (program, _) = lex_and_parse("x = a < b == c < d").unwrap();
        match &program.statements[0].kind {
            StmtKind::VarDecl(decl) => match &decl.initializer.kind {
                ExprKind::BinaryOp(lhs, BinaryOperator::Eq, rhs) => {
                    assert!(matches!(
                        lhs.kind,
                        ExprKind::BinaryOp(_, BinaryOperator::Lt, _)
                    ));
                    assert!(matches!(
                        rhs.kind,
                        ExprKind::BinaryOp(_, BinaryOperator::Lt, _)
                    ));
                }
                other => panic!("expected top-level BinaryOp(Eq), got {:?}", other),
            },
            other => panic!("expected VarDecl, got {:?}", other),
        }
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
        let (program, pool) = lex_and_parse(src).expect("parse ok");
        match &program.statements[0].kind {
            StmtKind::VarDecl(decl) => match decl.initializer.kind {
                ExprKind::Literal(Literal::Str(id)) => pool.str(id).to_string(),
                ref other => panic!("expected Str literal, got {:?}", other),
            },
            other => panic!("expected VarDecl, got {:?}", other),
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
        let (program, _) = lex_and_parse("x = true and false").unwrap();
        match &program.statements[0].kind {
            StmtKind::VarDecl(decl) => assert!(matches!(
                decl.initializer.kind,
                ExprKind::BinaryOp(_, BinaryOperator::And, _)
            )),
            other => panic!("expected VarDecl, got {:?}", other),
        }
    }

    #[test]
    fn parse_or_operator() {
        let (program, _) = lex_and_parse("x = true or false").unwrap();
        match &program.statements[0].kind {
            StmtKind::VarDecl(decl) => assert!(matches!(
                decl.initializer.kind,
                ExprKind::BinaryOp(_, BinaryOperator::Or, _)
            )),
            other => panic!("expected VarDecl, got {:?}", other),
        }
    }

    #[test]
    fn parse_not_operator() {
        let (program, _) = lex_and_parse("x = not true").unwrap();
        match &program.statements[0].kind {
            StmtKind::VarDecl(decl) => assert!(matches!(
                decl.initializer.kind,
                ExprKind::UnaryOp(UnaryOperator::Not, _)
            )),
            other => panic!("expected VarDecl, got {:?}", other),
        }
    }

    #[test]
    fn parse_and_binds_tighter_than_or() {
        // a or b and c  =>  a or (b and c)
        let (program, _) = lex_and_parse("x = true or false and true").unwrap();
        match &program.statements[0].kind {
            StmtKind::VarDecl(decl) => match &decl.initializer.kind {
                ExprKind::BinaryOp(_, BinaryOperator::Or, rhs) => {
                    assert!(matches!(
                        rhs.kind,
                        ExprKind::BinaryOp(_, BinaryOperator::And, _)
                    ));
                }
                other => panic!("expected top-level Or, got {:?}", other),
            },
            other => panic!("expected VarDecl, got {:?}", other),
        }
    }

    #[test]
    fn parse_not_binds_tighter_than_and() {
        // not a and b  =>  (not a) and b
        let (program, _) = lex_and_parse("x = not true and false").unwrap();
        match &program.statements[0].kind {
            StmtKind::VarDecl(decl) => match &decl.initializer.kind {
                ExprKind::BinaryOp(lhs, BinaryOperator::And, _) => {
                    assert!(matches!(lhs.kind, ExprKind::UnaryOp(UnaryOperator::Not, _)));
                }
                other => panic!("expected top-level And, got {:?}", other),
            },
            other => panic!("expected VarDecl, got {:?}", other),
        }
    }

    #[test]
    fn parse_not_not_chains() {
        let (program, _) = lex_and_parse("x = not not true").unwrap();
        match &program.statements[0].kind {
            StmtKind::VarDecl(decl) => match &decl.initializer.kind {
                ExprKind::UnaryOp(UnaryOperator::Not, inner) => {
                    assert!(matches!(
                        inner.kind,
                        ExprKind::UnaryOp(UnaryOperator::Not, _)
                    ));
                }
                other => panic!("expected outer Not, got {:?}", other),
            },
            other => panic!("expected VarDecl, got {:?}", other),
        }
    }

    #[test]
    fn parse_simple_if() {
        let input = "fn main():\n\tif true:\n\t\tx = 1\n";
        let (program, _) = lex_and_parse(input).unwrap();
        match &program.statements[0].kind {
            StmtKind::FunctionDef(f) => {
                assert_eq!(f.body.len(), 1);
                assert!(matches!(f.body[0].kind, StmtKind::IfStmt(_)));
            }
            other => panic!("expected FunctionDef, got {:?}", other),
        }
    }

    #[test]
    fn parse_if_else() {
        let input = "fn main():\n\tif true:\n\t\tx = 1\n\telse:\n\t\tx = 2\n";
        let (program, _) = lex_and_parse(input).unwrap();
        match &program.statements[0].kind {
            StmtKind::FunctionDef(f) => match &f.body[0].kind {
                StmtKind::IfStmt(if_stmt) => {
                    assert!(if_stmt.else_block.is_some());
                    assert!(if_stmt.elif_branches.is_empty());
                }
                other => panic!("expected IfStmt, got {:?}", other),
            },
            other => panic!("expected FunctionDef, got {:?}", other),
        }
    }

    #[test]
    fn parse_if_elif_else() {
        let input =
            "fn main():\n\tif true:\n\t\tx = 1\n\telif false:\n\t\tx = 2\n\telse:\n\t\tx = 3\n";
        let (program, _) = lex_and_parse(input).unwrap();
        match &program.statements[0].kind {
            StmtKind::FunctionDef(f) => match &f.body[0].kind {
                StmtKind::IfStmt(if_stmt) => {
                    assert_eq!(if_stmt.elif_branches.len(), 1);
                    assert!(if_stmt.else_block.is_some());
                }
                other => panic!("expected IfStmt, got {:?}", other),
            },
            other => panic!("expected FunctionDef, got {:?}", other),
        }
    }

    #[test]
    fn parse_multiple_elif() {
        let input = "fn main():\n\tif true:\n\t\tx = 1\n\telif false:\n\t\tx = 2\n\telif true:\n\t\tx = 3\n\telse:\n\t\tx = 4\n";
        let (program, _) = lex_and_parse(input).unwrap();
        match &program.statements[0].kind {
            StmtKind::FunctionDef(f) => match &f.body[0].kind {
                StmtKind::IfStmt(if_stmt) => {
                    assert_eq!(if_stmt.elif_branches.len(), 2);
                    assert!(if_stmt.else_block.is_some());
                }
                other => panic!("expected IfStmt, got {:?}", other),
            },
            other => panic!("expected FunctionDef, got {:?}", other),
        }
    }

    #[test]
    fn parse_if_without_else() {
        let input = "fn main():\n\tif true:\n\t\tx = 1\n\tprint(\"done\")\n";
        let (program, _) = lex_and_parse(input).unwrap();
        match &program.statements[0].kind {
            StmtKind::FunctionDef(f) => {
                assert_eq!(f.body.len(), 2);
                match &f.body[0].kind {
                    StmtKind::IfStmt(if_stmt) => {
                        assert!(if_stmt.else_block.is_none());
                        assert!(if_stmt.elif_branches.is_empty());
                    }
                    other => panic!("expected IfStmt, got {:?}", other),
                }
            }
            other => panic!("expected FunctionDef, got {:?}", other),
        }
    }

    #[test]
    fn parse_assign_or_decl() {
        let (program, pool) = lex_and_parse("fn main():\n\tx = 42\n").unwrap();
        let func = match &program.statements[0].kind {
            StmtKind::FunctionDef(f) => f,
            other => panic!("expected FunctionDef, got {:?}", other),
        };
        match &func.body[0].kind {
            StmtKind::AssignOrDecl { target, value } => {
                assert_eq!(pool.str(target.name), "x");
                match &value.kind {
                    ExprKind::Literal(Literal::Int(42)) => {}
                    other => panic!("expected Int(42), got {:?}", other),
                }
            }
            other => panic!("expected AssignOrDecl, got {:?}", other),
        }
    }

    #[test]
    fn parse_compound_assign_plus() {
        let (program, pool) = lex_and_parse("fn main():\n\tx += 1\n").unwrap();
        let func = match &program.statements[0].kind {
            StmtKind::FunctionDef(f) => f,
            other => panic!("expected FunctionDef, got {:?}", other),
        };
        match &func.body[0].kind {
            StmtKind::CompoundAssign { target, op, .. } => {
                assert_eq!(pool.str(target.name), "x");
                assert_eq!(*op, CompoundOp::Add);
            }
            other => panic!("expected CompoundAssign, got {:?}", other),
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
            let (program, _pool) = lex_and_parse(&code).unwrap();
            let func = match &program.statements[0].kind {
                StmtKind::FunctionDef(f) => f,
                other => panic!("expected FunctionDef, got {:?}", other),
            };
            match &func.body[0].kind {
                StmtKind::CompoundAssign { op, .. } => {
                    assert_eq!(*op, expected_op, "failed for: {}", src);
                }
                other => panic!("expected CompoundAssign for '{}', got {:?}", src, other),
            }
        }
    }

    #[test]
    fn vardecl_still_works_with_mut() {
        let (program, pool) = lex_and_parse("fn main():\n\tmut x = 10\n").unwrap();
        let func = match &program.statements[0].kind {
            StmtKind::FunctionDef(f) => f,
            other => panic!("expected FunctionDef, got {:?}", other),
        };
        match &func.body[0].kind {
            StmtKind::VarDecl(decl) => {
                assert!(decl.mutable);
                assert_eq!(pool.str(decl.name.name), "x");
            }
            other => panic!("expected VarDecl, got {:?}", other),
        }
    }

    #[test]
    fn vardecl_with_type_annotation_still_works() {
        let (program, _pool) = lex_and_parse("fn main():\n\tx: int = 10\n").unwrap();
        let func = match &program.statements[0].kind {
            StmtKind::FunctionDef(f) => f,
            other => panic!("expected FunctionDef, got {:?}", other),
        };
        match &func.body[0].kind {
            StmtKind::VarDecl(decl) => {
                assert!(!decl.mutable);
                assert!(decl.type_annotation.is_some());
            }
            other => panic!("expected VarDecl, got {:?}", other),
        }
    }

    #[test]
    fn parse_while_loop() {
        let code = "fn main():\n\twhile true:\n\t\tbreak\n";
        let (program, _pool) = lex_and_parse(code).unwrap();
        match &program.statements[0].kind {
            StmtKind::FunctionDef(f) => {
                assert_eq!(f.body.len(), 1);
                match &f.body[0].kind {
                    StmtKind::WhileLoop { body, .. } => {
                        assert_eq!(body.len(), 1);
                    }
                    other => panic!("expected WhileLoop, got {:?}", other),
                }
            }
            other => panic!("expected FunctionDef, got {:?}", other),
        }
    }

    #[test]
    fn parse_break_statement() {
        let code = "fn main():\n\twhile true:\n\t\tbreak\n";
        let (program, _pool) = lex_and_parse(code).unwrap();
        match &program.statements[0].kind {
            StmtKind::FunctionDef(f) => match &f.body[0].kind {
                StmtKind::WhileLoop { body, .. } => {
                    assert!(matches!(body[0].kind, StmtKind::Break));
                }
                other => panic!("expected WhileLoop, got {:?}", other),
            },
            other => panic!("expected FunctionDef, got {:?}", other),
        }
    }

    #[test]
    fn parse_continue_statement() {
        let code = "fn main():\n\twhile true:\n\t\tcontinue\n";
        let (program, _pool) = lex_and_parse(code).unwrap();
        match &program.statements[0].kind {
            StmtKind::FunctionDef(f) => match &f.body[0].kind {
                StmtKind::WhileLoop { body, .. } => {
                    assert!(matches!(body[0].kind, StmtKind::Continue));
                }
                other => panic!("expected WhileLoop, got {:?}", other),
            },
            other => panic!("expected FunctionDef, got {:?}", other),
        }
    }

    #[test]
    fn parse_nested_while() {
        let code = "fn main():\n\twhile true:\n\t\twhile false:\n\t\t\tbreak\n";
        let (program, _pool) = lex_and_parse(code).unwrap();
        match &program.statements[0].kind {
            StmtKind::FunctionDef(f) => match &f.body[0].kind {
                StmtKind::WhileLoop { body, .. } => {
                    assert_eq!(body.len(), 1);
                    assert!(matches!(body[0].kind, StmtKind::WhileLoop { .. }));
                }
                other => panic!("expected WhileLoop, got {:?}", other),
            },
            other => panic!("expected FunctionDef, got {:?}", other),
        }
    }

    #[test]
    fn parse_logical_below_equality() {
        // a == b and c == d  =>  (a == b) and (c == d)
        let (program, _) = lex_and_parse("x = 1 == 2 and 3 == 4").unwrap();
        match &program.statements[0].kind {
            StmtKind::VarDecl(decl) => match &decl.initializer.kind {
                ExprKind::BinaryOp(lhs, BinaryOperator::And, rhs) => {
                    assert!(matches!(
                        lhs.kind,
                        ExprKind::BinaryOp(_, BinaryOperator::Eq, _)
                    ));
                    assert!(matches!(
                        rhs.kind,
                        ExprKind::BinaryOp(_, BinaryOperator::Eq, _)
                    ));
                }
                other => panic!("expected top-level And, got {:?}", other),
            },
            other => panic!("expected VarDecl, got {:?}", other),
        }
    }

    #[test]
    fn parse_for_range() {
        let code = "fn main():\n\tfor i in range(0, 10):\n\t\tprint(i)\n";
        let (program, pool) = lex_and_parse(code).unwrap();
        let func = match &program.statements[0].kind {
            StmtKind::FunctionDef(f) => f,
            other => panic!("expected FunctionDef, got {:?}", other),
        };
        let stmts = &func.body;
        assert_eq!(stmts.len(), 1);
        match &stmts[0].kind {
            StmtKind::ForRange {
                var,
                iterator,
                body,
                ..
            } => {
                assert_eq!(pool.str(var.name), "i");
                assert_eq!(pool.str(iterator.name), "range");
                assert_eq!(body.len(), 1);
            }
            other => panic!("expected ForRange, got {:?}", other),
        }
    }

    #[test]
    fn parse_for_range_with_expressions() {
        let code = "fn main():\n\tfor x in range(1 + 2, 10 - 3):\n\t\tprint(x)\n";
        let (program, pool) = lex_and_parse(code).unwrap();
        let func = match &program.statements[0].kind {
            StmtKind::FunctionDef(f) => f,
            other => panic!("expected FunctionDef, got {:?}", other),
        };
        let stmts = &func.body;
        match &stmts[0].kind {
            StmtKind::ForRange { var, iterator, .. } => {
                assert_eq!(pool.str(var.name), "x");
                assert_eq!(pool.str(iterator.name), "range");
            }
            other => panic!("expected ForRange, got {:?}", other),
        }
    }

    #[test]
    fn parse_for_range_nested() {
        let code =
            "fn main():\n\tfor i in range(0, 5):\n\t\tfor j in range(0, 3):\n\t\t\tprint(i)\n";
        let (program, _pool) = lex_and_parse(code).unwrap();
        let func = match &program.statements[0].kind {
            StmtKind::FunctionDef(f) => f,
            other => panic!("expected FunctionDef, got {:?}", other),
        };
        let stmts = &func.body;
        match &stmts[0].kind {
            StmtKind::ForRange { body, .. } => {
                assert_eq!(body.len(), 1);
                assert!(matches!(body[0].kind, StmtKind::ForRange { .. }));
            }
            other => panic!("expected ForRange, got {:?}", other),
        }
    }

    #[test]
    fn parse_for_break_continue() {
        let code = "fn main():\n\tfor i in range(0, 10):\n\t\tif i == 5:\n\t\t\tbreak\n\t\tif i == 3:\n\t\t\tcontinue\n";
        let (program, _pool) = lex_and_parse(code).unwrap();
        let func = match &program.statements[0].kind {
            StmtKind::FunctionDef(f) => f,
            other => panic!("expected FunctionDef, got {:?}", other),
        };
        let stmts = &func.body;
        assert!(matches!(stmts[0].kind, StmtKind::ForRange { .. }));
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
        let (program, pool) = lex_and_parse("fn consume(move s: str):\n\tprint(s)\n").unwrap();
        let func = match &program.statements[0].kind {
            StmtKind::FunctionDef(f) => f,
            other => panic!("expected FunctionDef, got {:?}", other),
        };
        assert_eq!(func.params.len(), 1);
        assert!(
            func.params[0].mode == ParamMode::Move,
            "param `s` should be marked move"
        );
        assert_eq!(pool.str(func.params[0].name.name), "s");
        assert_eq!(pool.str(func.params[0].type_annotation.name), "str");
    }

    #[test]
    fn parse_default_parameter_is_not_move() {
        let (program, pool) = lex_and_parse("fn read(s: str):\n\tprint(s)\n").unwrap();
        let func = match &program.statements[0].kind {
            StmtKind::FunctionDef(f) => f,
            other => panic!("expected FunctionDef, got {:?}", other),
        };
        assert_eq!(func.params.len(), 1);
        assert!(
            func.params[0].mode == ParamMode::Borrow,
            "bare param `s` should default to Borrow mode"
        );
        assert_eq!(pool.str(func.params[0].name.name), "s");
        assert_eq!(pool.str(func.params[0].type_annotation.name), "str");
    }

    #[test]
    fn parse_inout_param() {
        let (program, _pool) = lex_and_parse("fn f(inout x: int):\n\tx += 1\n").unwrap();
        let func = match &program.statements[0].kind {
            StmtKind::FunctionDef(f) => f,
            other => panic!("expected FunctionDef, got {:?}", other),
        };
        assert_eq!(func.params.len(), 1);
        assert_eq!(
            func.params[0].mode,
            ParamMode::Inout,
            "param `x` should be marked inout"
        );
    }

    #[test]
    fn parse_borrow_arg() {
        let (program, _pool) = lex_and_parse("fn main():\n\tmut c = 0\n\tf(&c)\n").unwrap();
        let func = match &program.statements[0].kind {
            StmtKind::FunctionDef(f) => f,
            other => panic!("expected FunctionDef, got {:?}", other),
        };
        // body[1] is the `f(&c)` expression statement.
        let expr = match &func.body[1].kind {
            StmtKind::ExprStmt(e) => e,
            other => panic!("expected ExprStmt, got {:?}", other),
        };
        let args = match &expr.kind {
            ExprKind::Call(_name, args) => args,
            other => panic!("expected Call, got {:?}", other),
        };
        assert_eq!(args.len(), 1);
        assert!(
            matches!(args[0].kind, ExprKind::Borrow(_)),
            "call argument should be a Borrow expression"
        );
    }

    #[test]
    fn parse_slice_full() {
        let (program, _pool) = lex_and_parse("fn main():\n\tx = s[1:2]\n").unwrap();
        // `fn main():` wraps the body: statements[0] is the FunctionDef.
        // A bare `x = ...` in a body surfaces as AssignOrDecl (see
        // `parse_assign_or_decl`); the slice sits in its value.
        let func = match &program.statements[0].kind {
            StmtKind::FunctionDef(f) => f,
            other => panic!("expected FunctionDef, got {:?}", other),
        };
        let StmtKind::AssignOrDecl { value, .. } = &func.body[0].kind else {
            panic!("expected AssignOrDecl");
        };
        match &value.kind {
            ExprKind::Slice { base, start, end } => {
                assert!(matches!(base.kind, ExprKind::Ident(_)));
                assert!(start.is_some() && end.is_some());
            }
            other => panic!("expected Slice, got {:?}", other),
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
            let (program, _pool) = lex_and_parse(&snippet).unwrap();
            let func = match &program.statements[0].kind {
                StmtKind::FunctionDef(f) => f,
                other => panic!("expected FunctionDef for {}, got {:?}", src, other),
            };
            let StmtKind::AssignOrDecl { value, .. } = &func.body[0].kind else {
                panic!("expected AssignOrDecl for {}", src);
            };
            match &value.kind {
                ExprKind::Slice { start, end, .. } => {
                    assert_eq!(start.is_some(), want_start, "{}: start", src);
                    assert_eq!(end.is_some(), want_end, "{}: end", src);
                }
                other => panic!("{}: expected Slice, got {:?}", src, other),
            }
        }
    }

    #[test]
    fn parse_slice_after_method_call() {
        let (program, _pool) = lex_and_parse("fn main():\n\tx = s.len()[0:1]\n").unwrap();
        let func = match &program.statements[0].kind {
            StmtKind::FunctionDef(f) => f,
            other => panic!("expected FunctionDef, got {:?}", other),
        };
        let StmtKind::AssignOrDecl { value, .. } = &func.body[0].kind else {
            panic!("expected AssignOrDecl");
        };
        match &value.kind {
            ExprKind::Slice { base, .. } => {
                assert!(matches!(base.kind, ExprKind::MethodCall { .. }));
            }
            other => panic!("expected Slice over MethodCall, got {:?}", other),
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
        let (program, pool) = lex_and_parse("fn first(text: &str):\n\tprint(text)\n").unwrap();
        let StmtKind::FunctionDef(f) = &program.statements[0].kind else {
            panic!("expected FunctionDef");
        };
        assert!(f.params[0].type_annotation.is_view);
        assert_eq!(pool.str(f.params[0].type_annotation.name), "str");
    }

    /// Recovery-aware variant of `lex_and_parse`: returns the partial
    /// program (if one could be produced) alongside every parse error.
    fn lex_and_parse_recovering(
        input: &str,
    ) -> (Option<Program>, Vec<Rich<'static, Token>>, InternPool) {
        let mut pool = InternPool::new();
        let mut sink = ryo_core::diag::DiagSink::new();
        let tokens = lex(input, &mut pool, &mut sink);
        assert!(!sink.has_errors(), "test input must lex cleanly");
        let token_stream = tokens[..].split_token_span((0..input.len()).into());
        let (program, errs) = program_parser().parse(token_stream).into_output_errors();
        (
            program,
            errs.into_iter().map(|rich| rich.into_owned()).collect(),
            pool,
        )
    }

    #[test]
    fn recovers_from_bad_statement_between_good_ones() {
        let (program, errs, _pool) = lex_and_parse_recovering("x = 1\ny = = 2\nz = 3\n");
        assert_eq!(errs.len(), 1, "expected one parse error: {errs:?}");
        let program = program.expect("recovery must produce a partial program");
        assert_eq!(program.statements.len(), 3);
        assert!(matches!(
            program.statements[0].kind,
            StmtKind::VarDecl(_) | StmtKind::AssignOrDecl { .. }
        ));
        assert!(matches!(program.statements[1].kind, StmtKind::Error));
        assert!(matches!(
            program.statements[2].kind,
            StmtKind::VarDecl(_) | StmtKind::AssignOrDecl { .. }
        ));
    }

    #[test]
    fn recovers_inside_function_body() {
        let (program, errs, _pool) =
            lex_and_parse_recovering("fn main():\n\tx = 1\n\ty = = 2\n\tz = 3\n");
        assert_eq!(errs.len(), 1, "expected one parse error: {errs:?}");
        let program = program.expect("recovery must produce a partial program");
        let StmtKind::FunctionDef(func) = &program.statements[0].kind else {
            panic!("expected FunctionDef");
        };
        assert_eq!(func.body.len(), 3);
        assert!(matches!(func.body[1].kind, StmtKind::Error));
        assert!(matches!(func.body[2].kind, StmtKind::AssignOrDecl { .. }));
    }

    #[test]
    fn reports_multiple_parse_errors_in_one_file() {
        let (program, errs, _pool) = lex_and_parse_recovering("x = = 1\ny = 2\nz = = 3\nw = 4\n");
        assert_eq!(errs.len(), 2, "expected two parse errors: {errs:?}");
        let program = program.expect("recovery must produce a partial program");
        assert_eq!(program.statements.len(), 4);
        assert!(matches!(program.statements[0].kind, StmtKind::Error));
        assert!(matches!(
            program.statements[1].kind,
            StmtKind::VarDecl(_) | StmtKind::AssignOrDecl { .. }
        ));
        assert!(matches!(program.statements[2].kind, StmtKind::Error));
        assert!(matches!(
            program.statements[3].kind,
            StmtKind::VarDecl(_) | StmtKind::AssignOrDecl { .. }
        ));
    }

    #[test]
    fn recovers_from_trailing_garbage_without_newline_at_eof() {
        // File does not end with a newline: the last line is broken.
        let (program, errs, _pool) = lex_and_parse_recovering("x = 1\ny = = 2");
        assert_eq!(errs.len(), 1, "expected one parse error: {errs:?}");
        let program = program.expect("recovery must produce a partial program");
        assert_eq!(program.statements.len(), 2);
        assert!(matches!(
            program.statements[0].kind,
            StmtKind::VarDecl(_) | StmtKind::AssignOrDecl { .. }
        ));
        assert!(matches!(program.statements[1].kind, StmtKind::Error));
    }

    #[test]
    fn recovers_from_broken_block_final_statement_before_dedent() {
        // The last body line is broken and the following top-level
        // statement triggers the block's `Dedent`.
        let (program, errs, _pool) =
            lex_and_parse_recovering("fn main():\n\tx = 1\n\ty = = 2\nz = 3\n");
        assert_eq!(errs.len(), 1, "expected one parse error: {errs:?}");
        let program = program.expect("recovery must produce a partial program");
        let StmtKind::FunctionDef(func) = &program.statements[0].kind else {
            panic!("expected FunctionDef");
        };
        assert_eq!(func.body.len(), 2);
        assert!(matches!(func.body[1].kind, StmtKind::Error));
        assert_eq!(program.statements.len(), 2);
    }

    #[test]
    fn recovers_from_broken_block_final_statement_at_eof() {
        // The broken final body line sits directly against the
        // zero-width end-of-input `Dedent` (no terminating newline of
        // its own) — the `peek(Dedent)` terminator path.
        let (program, errs, _pool) = lex_and_parse_recovering("fn main():\n\tx = 1\n\ty = = 2");
        assert_eq!(errs.len(), 1, "expected one parse error: {errs:?}");
        let program = program.expect("recovery must produce a partial program");
        let StmtKind::FunctionDef(func) = &program.statements[0].kind else {
            panic!("expected FunctionDef");
        };
        assert_eq!(func.body.len(), 2);
        assert!(matches!(func.body[1].kind, StmtKind::Error));
        assert_eq!(program.statements.len(), 1);
    }

    #[test]
    fn two_statements_on_one_line_still_error_with_recovery() {
        // The no-two-statements-per-line rule must survive recovery:
        // this is one parse error, not two silent statements.
        let (program, errs, _pool) = lex_and_parse_recovering("x = 1 y = 2\n");
        assert_eq!(errs.len(), 1, "expected one parse error: {errs:?}");
        let program = program.expect("recovery must produce a partial program");
        assert_eq!(program.statements.len(), 1);
        assert!(matches!(program.statements[0].kind, StmtKind::Error));
    }
}
