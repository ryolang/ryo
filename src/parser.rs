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
fn skip_newlines<'a, I>() -> impl Parser<'a, I, (), extra::Err<Rich<'a, Token>>> + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    select! { Token::Newline => () }.repeated().to(())
}

/// Parse a complete Ryo program with multiple statements.
pub fn program_parser<'a, I>() -> impl Parser<'a, I, Program, extra::Err<Rich<'a, Token>>> + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    skip_newlines()
        .ignore_then(
            statement_parser()
                .then(skip_newlines())
                .repeated()
                .collect::<Vec<_>>()
                .map(|v| v.into_iter().map(|(s, _)| s).collect::<Vec<_>>()),
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

/// Statements valid inside a function body.
fn body_statement_parser<'a, I>() -> impl Parser<'a, I, Statement, extra::Err<Rich<'a, Token>>> + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
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

    let expr_stmt = expression_parser().map_with(|expr, e| Statement {
        span: e.span(),
        kind: StmtKind::ExprStmt(expr),
    });

    choice((return_stmt, var_decl, expr_stmt))
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

    choice((function_def, var_decl))
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

    let body = skip_newlines()
        .ignore_then(
            body_statement_parser()
                .then(skip_newlines())
                .repeated()
                .at_least(1)
                .collect::<Vec<_>>()
                .map(|v| v.into_iter().map(|(s, _)| s).collect::<Vec<_>>()),
        )
        .delimited_by(
            skip_newlines().ignore_then(just(Token::Indent)),
            just(Token::Dedent),
        );

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

fn var_decl_parser<'a, I>() -> impl Parser<'a, I, VarDecl, extra::Err<Rich<'a, Token>>> + 'a
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

fn expression_parser<'a, I>() -> impl Parser<'a, I, Expression, extra::Err<Rich<'a, Token>>> + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    recursive(|expr| {
        let atom = {
            let literal = select! {
                Token::IntLit(n) => ExprKind::Literal(Literal::Int(n)),
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

        let unary = just(Token::Sub)
            .repeated()
            .collect::<Vec<_>>()
            .then(atom)
            .map_with(|(negs, expr), e| {
                let mut result = expr;
                for _ in negs {
                    result = Expression::new(
                        ExprKind::UnaryOp(UnaryOperator::Neg, Box::new(result)),
                        e.span(),
                    );
                }
                result
            });

        let term = unary.clone().foldl(
            choice((
                just(Token::Mul).to(BinaryOperator::Mul),
                just(Token::Div).to(BinaryOperator::Div),
            ))
            .then(unary)
            .repeated(),
            |left, (op, right)| {
                let start = left.span.start;
                let end = right.span.end;
                Expression::new(
                    ExprKind::BinaryOp(Box::new(left.clone()), op, Box::new(right.clone())),
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
                    ExprKind::BinaryOp(Box::new(left.clone()), op, Box::new(right.clone())),
                    SimpleSpan::new((), start..end),
                )
            },
        );

        // Equality is non-associative, lowest precedence.
        additive
            .clone()
            .then(
                choice((
                    just(Token::EqEq).to(BinaryOperator::Eq),
                    just(Token::NotEq).to(BinaryOperator::NotEq),
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
            })
    })
}

#[cfg(test)]
#[allow(irrefutable_let_patterns)]
mod tests {
    use super::*;
    use crate::lexer::lex;
    use crate::types::InternPool;
    use chumsky::Parser;
    use chumsky::input::Stream;

    fn lex_and_parse(input: &str) -> Result<(Program, InternPool), Vec<Rich<'static, Token>>> {
        let mut pool = InternPool::new();
        let tokens = lex(input, &mut pool).map_err(|e| vec![Rich::custom(e.span, e.message)])?;
        let token_stream =
            Stream::from_iter(tokens).map((0..input.len()).into(), |(t, s): (_, _)| (t, s));

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
    fn parse_chained_equality_is_rejected() {
        assert!(lex_and_parse("x = a == b == c").is_err());
    }

    #[test]
    fn string_literal_unescapes_at_lex_time() {
        let (program, pool) = lex_and_parse("x = \"hello\\n\"").unwrap();
        if let StmtKind::VarDecl(decl) = &program.statements[0].kind {
            if let ExprKind::Literal(Literal::Str(id)) = decl.initializer.kind {
                assert_eq!(pool.str(id), "hello\n");
            } else {
                panic!("expected Str literal");
            }
        } else {
            panic!("expected VarDecl");
        }
    }
}
