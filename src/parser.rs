use chumsky::{input::ValueInput, prelude::*, span::SimpleSpan};

use crate::ast::*;
use crate::lexer::Token;

/// Helper: skip zero or more newline tokens
fn skip_newlines<'a, I>() -> impl Parser<'a, I, (), extra::Err<Rich<'a, Token<'a>>>> + 'a
where
    I: ValueInput<'a, Token = Token<'a>, Span = SimpleSpan>,
{
    select! { Token::Newline(_) => () }
        .repeated()
        .to(())
}



/// Unescape a string literal (handle \n, \t, \", \\, etc.)
fn unescape_string(s: &str) -> String {
    let mut result = String::new();
    let mut chars = s.chars();

    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('n') => result.push('\n'),
                Some('t') => result.push('\t'),
                Some('r') => result.push('\r'),
                Some('\\') => result.push('\\'),
                Some('"') => result.push('"'),
                Some('0') => result.push('\0'),
                Some(c) => {
                    // Unknown escape sequence, keep backslash and character
                    result.push('\\');
                    result.push(c);
                }
                None => result.push('\\'),
            }
        } else {
            result.push(ch);
        }
    }

    result
}

/// Parse a complete Ryo program with multiple statements
/// Statements must be separated by newlines (no two statements on same line)
pub fn program_parser<'a, I>() -> impl Parser<'a, I, Program, extra::Err<Rich<'a, Token<'a>>>> + 'a
where
    I: ValueInput<'a, Token = Token<'a>, Span = SimpleSpan>,
{
    skip_newlines()
        .ignore_then(
            statement_parser()
                .then(skip_newlines())
                .repeated()
                .collect::<Vec<_>>()
                .map(|v| v.into_iter().map(|(s, _)| s).collect::<Vec<_>>())
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

/// Parse a statement that can appear inside a function body (no nested functions)
/// Includes expression statements, return statements, and variable declarations
fn body_statement_parser<'a, I>()
-> impl Parser<'a, I, Statement, extra::Err<Rich<'a, Token<'a>>>> + 'a
where
    I: ValueInput<'a, Token = Token<'a>, Span = SimpleSpan>,
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

/// Parse a top-level statement (no expression statements allowed at module level)
/// Only function definitions and variable declarations are valid
fn top_level_statement_parser<'a, I>() -> impl Parser<'a, I, Statement, extra::Err<Rich<'a, Token<'a>>>> + 'a
where
    I: ValueInput<'a, Token = Token<'a>, Span = SimpleSpan>,
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

/// Parse a top-level statement (includes function definitions)
fn statement_parser<'a, I>() -> impl Parser<'a, I, Statement, extra::Err<Rich<'a, Token<'a>>>> + 'a
where
    I: ValueInput<'a, Token = Token<'a>, Span = SimpleSpan>,
{
    top_level_statement_parser()
}

/// Parse a function definition: fn name(params) [-> type]: INDENT body DEDENT
fn function_def_parser<'a, I>()
-> impl Parser<'a, I, FunctionDef, extra::Err<Rich<'a, Token<'a>>>> + 'a
where
    I: ValueInput<'a, Token = Token<'a>, Span = SimpleSpan>,
{
    let ident = select! {
        Token::Ident(name) => name.to_string()
    }
    .map_with(|name, e| Ident::new(name, e.span()));

    let type_expr = select! {
        Token::Ident(name) => name.to_string()
    }
    .map_with(|name, e| TypeExpr::new(name, e.span()));

    let param = select! {
        Token::Ident(name) => name.to_string()
    }
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
                .map(|v| v.into_iter().map(|(s, _)| s).collect::<Vec<_>>())
        )
        .delimited_by(skip_newlines().ignore_then(just(Token::Indent)), just(Token::Dedent));

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

/// Parse a variable declaration: [mut] ident [: type] = expression
/// Examples:
///   x = 42
///   x: int = 42
///   mut x = 42
///   mut counter: int = 0
fn var_decl_parser<'a, I>() -> impl Parser<'a, I, VarDecl, extra::Err<Rich<'a, Token<'a>>>> + 'a
where
    I: ValueInput<'a, Token = Token<'a>, Span = SimpleSpan>,
{
    // Optional 'mut' keyword
    let mutable = just(Token::Mut).or_not().map(|m| m.is_some());

    // Identifier (variable name)
    let ident = select! {
        Token::Ident(name) => name.to_string()
    }
    .map_with(|name, e| Ident::new(name, e.span()));

    // Optional type annotation: `: type_name`
    let type_annotation = just(Token::Colon)
        .ignore_then(
            select! {
                Token::Ident(name) => name.to_string()
            }
            .map_with(|name, e| TypeExpr::new(name, e.span())),
        )
        .or_not();

    // Build the parser
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

/// Parse an expression with precedence handling
fn expression_parser<'a, I>() -> impl Parser<'a, I, Expression, extra::Err<Rich<'a, Token<'a>>>> + 'a
where
    I: ValueInput<'a, Token = Token<'a>, Span = SimpleSpan>,
{
    recursive(|expr| {
        // Atomic expressions: literals, calls, and parenthesized expressions
        let atom = {
            let literal = select! {
                Token::Int(s) => {
                    let n: isize = s.parse().unwrap();
                    ExprKind::Literal(Literal::Int(n))
                },
                Token::Str(s) => {
                    // Remove surrounding quotes and unescape
                    let unquoted = &s[1..s.len()-1];
                    let unescaped = unescape_string(unquoted);
                    ExprKind::Literal(Literal::Str(unescaped))
                },
            }
            .map_with(|kind, e| Expression::new(kind, e.span()));

            // Function call: identifier(arg1, arg2, ...)
            let call = select! {
                Token::Ident(name) => name.to_string()
            }
            .then(
                expr.clone()
                    .separated_by(just(Token::Comma))
                    .allow_trailing()
                    .collect::<Vec<_>>()
                    .delimited_by(just(Token::LParen), just(Token::RParen)),
            )
            .map_with(|(name, args), e| Expression::new(ExprKind::Call(name, args), e.span()));

            let ident_expr = select! {
                Token::Ident(name) => name.to_string()
            }
            .map_with(|name, e| Expression::new(ExprKind::Ident(name), e.span()));

            let parenthesized = expr.delimited_by(just(Token::LParen), just(Token::RParen));

            call.or(ident_expr).or(literal).or(parenthesized)
        };

        // Unary operators (negation has highest precedence)
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

        // Multiplication and division (higher precedence)
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

        // Addition and subtraction (lower precedence)
        term.clone().foldl(
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
        )
    })
}

#[cfg(test)]
#[allow(irrefutable_let_patterns)]
mod tests {
    use super::*;
    use crate::lexer::leak_token;
    use chumsky::Parser;
    use chumsky::input::Stream;
    use logos::Logos;

    fn lex_and_parse(input: &str) -> Result<Program, Vec<Rich<'static, Token<'static>>>> {
        use crate::lexer::Token;
        use crate::indent;

        let raw_tokens: Vec<_> = Token::lexer(input)
            .spanned()
            .map(|(tok, span)| match tok {
                Ok(tok) => (tok, span.into()),
                Err(()) => (Token::Error, span.into()),
            })
            .collect();

        let tokens = indent::process(raw_tokens).map_err(|e| vec![Rich::custom(SimpleSpan::new((), 0..0), e)])?;

        let static_tokens: Vec<(Token<'static>, SimpleSpan)> =
            tokens.into_iter().map(|(t, s)| (leak_token(t), s)).collect();

        let token_stream = Stream::from_iter(static_tokens)
            .map((0..input.len()).into(), |(t, s): (_, _)| (t, s));

        program_parser()
            .parse(token_stream)
            .into_result()
            .map_err(|e| e.into_iter().map(|rich| rich.into_owned()).collect())
    }

    #[test]
    fn parse_simple_variable_declaration() {
        let result = lex_and_parse("x = 42");
        assert!(result.is_ok());
        let program = result.unwrap();
        assert_eq!(program.statements.len(), 1);

        if let StmtKind::VarDecl(decl) = &program.statements[0].kind {
            assert!(!decl.mutable);
            assert_eq!(decl.name.name, "x");
            assert!(decl.type_annotation.is_none());
            match &decl.initializer.kind {
                ExprKind::Literal(Literal::Int(42)) => {}
                _ => panic!("Expected Int(42)"),
            }
        } else {
            panic!("Expected VarDecl");
        }
    }

    #[test]
    fn parse_variable_with_type_annotation() {
        let result = lex_and_parse("x: int = 42");
        assert!(result.is_ok());
        let program = result.unwrap();
        assert_eq!(program.statements.len(), 1);

        if let StmtKind::VarDecl(decl) = &program.statements[0].kind {
            assert!(!decl.mutable);
            assert_eq!(decl.name.name, "x");
            assert!(decl.type_annotation.is_some());
            assert_eq!(decl.type_annotation.as_ref().unwrap().name, "int");
        } else {
            panic!("Expected VarDecl");
        }
    }

    #[test]
    fn parse_mutable_variable() {
        let result = lex_and_parse("mut x = 42");
        assert!(result.is_ok());
        let program = result.unwrap();
        assert_eq!(program.statements.len(), 1);

        if let StmtKind::VarDecl(decl) = &program.statements[0].kind {
            assert!(decl.mutable);
            assert_eq!(decl.name.name, "x");
        } else {
            panic!("Expected VarDecl");
        }
    }

    #[test]
    fn parse_mutable_with_type() {
        let result = lex_and_parse("mut counter: int = 0");
        assert!(result.is_ok());
        let program = result.unwrap();
        assert_eq!(program.statements.len(), 1);

        if let StmtKind::VarDecl(decl) = &program.statements[0].kind {
            assert!(decl.mutable);
            assert_eq!(decl.name.name, "counter");
            assert_eq!(decl.type_annotation.as_ref().unwrap().name, "int");
            match &decl.initializer.kind {
                ExprKind::Literal(Literal::Int(0)) => {}
                _ => panic!("Expected Int(0)"),
            }
        } else {
            panic!("Expected VarDecl");
        }
    }

    #[test]
    fn parse_expression_addition() {
        let result = lex_and_parse("x = 1 + 2");
        assert!(result.is_ok());
        let program = result.unwrap();

        if let StmtKind::VarDecl(decl) = &program.statements[0].kind {
            match &decl.initializer.kind {
                ExprKind::BinaryOp(left, BinaryOperator::Add, right) => {
                    match &left.kind {
                        ExprKind::Literal(Literal::Int(1)) => {}
                        _ => panic!("Expected left = 1"),
                    }
                    match &right.kind {
                        ExprKind::Literal(Literal::Int(2)) => {}
                        _ => panic!("Expected right = 2"),
                    }
                }
                _ => panic!("Expected BinaryOp(Add)"),
            }
        } else {
            panic!("Expected VarDecl");
        }
    }

    #[test]
    fn parse_expression_precedence() {
        let result = lex_and_parse("x = 2 + 3 * 4");
        assert!(result.is_ok());
        let program = result.unwrap();

        if let StmtKind::VarDecl(decl) = &program.statements[0].kind {
            // Should parse as: 2 + (3 * 4)
            match &decl.initializer.kind {
                ExprKind::BinaryOp(left, BinaryOperator::Add, right) => {
                    // left = 2
                    match &left.kind {
                        ExprKind::Literal(Literal::Int(2)) => {}
                        _ => panic!("Expected left = 2"),
                    }
                    // right = 3 * 4
                    match &right.kind {
                        ExprKind::BinaryOp(_, BinaryOperator::Mul, _) => {}
                        _ => panic!("Expected right = BinaryOp(Mul)"),
                    }
                }
                _ => panic!("Expected BinaryOp(Add)"),
            }
        } else {
            panic!("Expected VarDecl");
        }
    }

    #[test]
    fn parse_expression_negation() {
        let result = lex_and_parse("x = -42");
        assert!(result.is_ok());
        let program = result.unwrap();

        if let StmtKind::VarDecl(decl) = &program.statements[0].kind {
            match &decl.initializer.kind {
                ExprKind::UnaryOp(UnaryOperator::Neg, expr) => match &expr.kind {
                    ExprKind::Literal(Literal::Int(42)) => {}
                    _ => panic!("Expected Int(42)"),
                },
                _ => panic!("Expected UnaryOp(Neg)"),
            }
        } else {
            panic!("Expected VarDecl");
        }
    }

    #[test]
    fn parse_expression_parenthesized() {
        let result = lex_and_parse("x = (2 + 3) * 4");
        assert!(result.is_ok());
        let program = result.unwrap();

        if let StmtKind::VarDecl(decl) = &program.statements[0].kind {
            // Should parse as: (2 + 3) * 4
            match &decl.initializer.kind {
                ExprKind::BinaryOp(_left, BinaryOperator::Mul, _right) => {
                    // Parenthesized expression forces precedence
                }
                _ => panic!("Expected BinaryOp(Mul)"),
            }
        } else {
            panic!("Expected VarDecl");
        }
    }

    #[test]
    fn parse_multiple_statements() {
        let result = lex_and_parse("x = 42\ny = 10");
        assert!(result.is_ok());
        let program = result.unwrap();
        assert_eq!(program.statements.len(), 2);
    }

    #[test]
    fn parse_multiple_with_types() {
        let result = lex_and_parse("x: int = 42\nmut y: float = 3\nz = 1 + 2");
        assert!(result.is_ok());
        let program = result.unwrap();
        assert_eq!(program.statements.len(), 3);
    }

    #[test]
    fn parse_empty_program() {
        let result = lex_and_parse("");
        assert!(result.is_ok());
        let program = result.unwrap();
        assert_eq!(program.statements.len(), 0);
    }

    // Tests for newline-delimited statements (no two statements on same line)

    #[test]
    fn reject_two_expressions_same_line() {
        let result = lex_and_parse("x 42");
        assert!(result.is_err());
    }

    #[test]
    fn reject_multiple_expressions_same_line() {
        let result = lex_and_parse("x y 42");
        assert!(result.is_err());
    }

    #[test]
    fn accept_statements_on_separate_lines() {
        let result = lex_and_parse("x = 1\ny = 2");
        assert!(result.is_ok());
        assert_eq!(result.unwrap().statements.len(), 2);
    }

    #[test]
    fn accept_statement_with_no_trailing_newline() {
        let result = lex_and_parse("x = 42");
        assert!(result.is_ok());
    }

    #[test]
    fn accept_statement_with_trailing_newline() {
        let result = lex_and_parse("x = 42\n");
        assert!(result.is_ok());
    }

    #[test]
    fn accept_blank_lines_between_statements() {
        let result = lex_and_parse("x = 1\n\ny = 2");
        assert!(result.is_ok());
        assert_eq!(result.unwrap().statements.len(), 2);
    }
}
