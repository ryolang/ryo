//! Surface-syntax parser.
//!
//! Built on chumsky over the lexer's `Token` type. Identifiers,
//! type names, and string literals come pre-interned as `StringId`
//! handles, so the parser only ever copies handles out of tokens —
//! no `to_string` allocations, no `&'a str` slicing into source.

use chumsky::{input::ValueInput, prelude::*, span::SimpleSpan};

use crate::ast::*;
use crate::lexer::Token;

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
fn require_newlines<'a, I>() -> impl Parser<'a, I, (), extra::Err<Rich<'a, Token>>> + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    select! { Token::Newline => () }
        .repeated()
        .at_least(1)
        .to(())
}

/// Parse a complete Ryo program with multiple statements.
pub fn program_parser<'a, I>() -> impl Parser<'a, I, Program, extra::Err<Rich<'a, Token>>> + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    // Statements are newline-separated. Leading/trailing newlines
    // are tolerated; consecutive statements must have at least one
    // newline between them.
    skip_newlines()
        .ignore_then(
            statement_parser()
                .separated_by(require_newlines())
                .allow_trailing()
                .collect::<Vec<_>>(),
        )
        .then_ignore(skip_newlines())
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
    skip_newlines()
        .ignore_then(
            stmt.then_ignore(skip_newlines())
                .repeated()
                .at_least(1)
                .collect::<Vec<_>>(),
        )
        .delimited_by(
            skip_newlines().ignore_then(just(Token::Indent)),
            just(Token::Dedent),
        )
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

        let if_stmt = if_stmt_parser(body_stmt).map_with(|if_s, e| Statement {
            span: e.span(),
            kind: StmtKind::IfStmt(if_s),
        });

        let expr_stmt = expression_parser().map_with(|expr, e| Statement {
            span: e.span(),
            kind: StmtKind::ExprStmt(expr),
        });

        choice((return_stmt, var_decl, if_stmt, expr_stmt))
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
-> impl Parser<'a, I, Statement, extra::Err<Rich<'a, Token>>> + 'a
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

fn statement_parser<'a, I>() -> impl Parser<'a, I, Statement, extra::Err<Rich<'a, Token>>> + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    top_level_statement_parser()
}

fn function_def_parser<'a, I>() -> impl Parser<'a, I, FunctionDef, extra::Err<Rich<'a, Token>>> + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    let ident =
        select! { Token::Ident(name) => name }.map_with(|name, e| Ident::new(name, e.span()));

    let type_expr =
        select! { Token::Ident(name) => name }.map_with(|name, e| TypeExpr::new(name, e.span()));

    let param = select! { Token::Ident(name) => name }
        .map_with(|name, e| Ident::new(name, e.span()))
        .then_ignore(just(Token::Colon))
        .then(type_expr)
        .map_with(|(name, type_annotation), e| Param {
            name,
            type_annotation,
            span: e.span(),
        });

    let params = param
        .separated_by(just(Token::Comma))
        .allow_trailing()
        .collect::<Vec<_>>()
        .delimited_by(just(Token::LParen), just(Token::RParen));

    let return_type = just(Token::Arrow).ignore_then(type_expr).or_not();

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

    let type_annotation = just(Token::Colon)
        .ignore_then(
            select! { Token::Ident(name) => name }
                .map_with(|name, e| TypeExpr::new(name, e.span())),
        )
        .or_not();

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

            let parenthesized = expr.delimited_by(just(Token::LParen), just(Token::RParen));

            call.or(ident_expr).or(literal).or(parenthesized)
        };

        let unary_op = choice((
            just(Token::Sub).to(UnaryOperator::Neg),
            just(Token::Not).to(UnaryOperator::Not),
        ));

        let unary =
            unary_op
                .repeated()
                .collect::<Vec<_>>()
                .then(atom)
                .map_with(|(ops, expr), e| {
                    let mut result = expr;
                    for op in ops.into_iter().rev() {
                        result = Expression::new(ExprKind::UnaryOp(op, Box::new(result)), e.span());
                    }
                    result
                });

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
    use crate::types::InternPool;
    use chumsky::Parser;
    use chumsky::input::Input;

    fn lex_and_parse(input: &str) -> Result<(Program, InternPool), Vec<Rich<'static, Token>>> {
        let mut pool = InternPool::new();
        let tokens = lex(input, &mut pool).map_err(|e| vec![Rich::custom(e.span, e.message)])?;
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
}
