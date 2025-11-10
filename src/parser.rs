use chumsky::{
    input::ValueInput,
    prelude::*,
    span::SimpleSpan,
};

use crate::ast::*;
use crate::lexer::Token;

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
pub fn program_parser<'a, I>() -> impl Parser<'a, I, Program, extra::Err<Rich<'a, Token<'a>>>> + 'a
where
    I: ValueInput<'a, Token = Token<'a>, Span = SimpleSpan>,
{
    statement_parser()
        .repeated()
        .collect::<Vec<_>>()
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
        .then_ignore(end())
}

/// Parse a single statement
fn statement_parser<'a, I>() -> impl Parser<'a, I, Statement, extra::Err<Rich<'a, Token<'a>>>> + 'a
where
    I: ValueInput<'a, Token = Token<'a>, Span = SimpleSpan>,
{
    var_decl_parser().map_with(|kind, e| Statement {
        span: e.span(),
        kind: StmtKind::VarDecl(kind),
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
        .map(|(((mutable, name), type_annotation), initializer)| VarDecl {
            mutable,
            name,
            type_annotation,
            initializer,
        })
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
                    .delimited_by(just(Token::LParen), just(Token::RParen))
            )
            .map_with(|(name, args), e| {
                Expression::new(ExprKind::Call(name, args), e.span())
            });

            let parenthesized = expr
                .delimited_by(just(Token::LParen), just(Token::RParen));

            call.or(literal).or(parenthesized)
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
    use chumsky::Parser;
    use chumsky::input::Stream;
    use logos::Logos;

    fn lex_and_parse(input: &str) -> Result<Program, Vec<Rich<'static, Token<'static>>>> {
        use crate::lexer::Token;

        let tokens: Vec<_> = Token::lexer(input)
            .filter_map(|result| result.ok())
            .collect();

        let static_tokens: Vec<Token<'static>> = tokens
            .into_iter()
            .map(|t| match t {
                Token::Int(s) => {
                    let leaked_str: &'static str = Box::leak(s.to_string().into_boxed_str());
                    Token::Int(leaked_str)
                }
                Token::Str(s) => {
                    let leaked_str: &'static str = Box::leak(s.to_string().into_boxed_str());
                    Token::Str(leaked_str)
                }
                Token::Ident(s) => {
                    let leaked_str: &'static str = Box::leak(s.to_string().into_boxed_str());
                    Token::Ident(leaked_str)
                }
                Token::Fn => Token::Fn,
                Token::If => Token::If,
                Token::Else => Token::Else,
                Token::Return => Token::Return,
                Token::Mut => Token::Mut,
                Token::Struct => Token::Struct,
                Token::Enum => Token::Enum,
                Token::Match => Token::Match,
                Token::Add => Token::Add,
                Token::Sub => Token::Sub,
                Token::Mul => Token::Mul,
                Token::Div => Token::Div,
                Token::Assign => Token::Assign,
                Token::Colon => Token::Colon,
                Token::LParen => Token::LParen,
                Token::RParen => Token::RParen,
                Token::LBrace => Token::LBrace,
                Token::RBrace => Token::RBrace,
                Token::Comma => Token::Comma,
                Token::Comment => Token::Comment,
                Token::Whitespace => Token::Whitespace,
                Token::Error => Token::Error,
            })
            .collect();

        program_parser()
            .parse(Stream::from_iter(static_tokens))
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
                ExprKind::UnaryOp(UnaryOperator::Neg, expr) => {
                    match &expr.kind {
                        ExprKind::Literal(Literal::Int(42)) => {}
                        _ => panic!("Expected Int(42)"),
                    }
                }
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
}
