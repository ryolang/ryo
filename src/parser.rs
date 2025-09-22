use chumsky::{
    input::ValueInput,
    prelude::*,
};

use crate::ast::Expr;
use crate::lexer::Token;

// This function signature looks complicated, but don't fear! We're just saying that this function is generic over
// inputs that:
//     - Can have tokens pulled out of them by-value, by cloning (`ValueInput`)
//     - Gives us access to slices of the original input (`SliceInput`)
//     - Produces tokens of type `Token`, the type we defined above (`Token = Token<'a>`)
//     - Produces spans of type `SimpleSpan`, a built-in span type provided by chumsky (`Span = SimpleSpan`)
// The function then returns a parser that:
//     - Has an input type of type `I`, the one we declared as a type parameter
//     - Produces an `Expr` as its output
//     - Uses `Rich`, a built-in error type provided by chumsky, for error generation
pub fn parser<'a, I>() -> impl Parser<'a, I, Expr, extra::Err<Rich<'a, Token<'a>>>>
where
    I: ValueInput<'a, Token = Token<'a>, Span = SimpleSpan>,
{
    recursive(|p| {
        let atom = {
            let parenthesized = p.delimited_by(just(Token::LParen), just(Token::RParen));
            let integer = select! {
                Token::Int(n) => Expr::Int(n.parse().unwrap()),
            };
            parenthesized.or(integer)
        };

        // Unary operators (like negation) have the highest precedence
        let unary = just(Token::Sub)
            .repeated()
            .collect::<Vec<_>>()
            .or_not()
            .then(atom)
            .map(|(maybe_ops, value)| match maybe_ops {
                Some(ops) => ops
                    .into_iter()
                    .rfold(value, |acc, _op| Expr::Neg(Box::new(acc))),
                None => value,
            });

        // Multiplication and division have higher precedence
        let op1 = choice((
            just(Token::Mul).to(Expr::Mul as fn(Box<Expr>, Box<Expr>) -> Expr),
            just(Token::Div).to(Expr::Div as fn(Box<Expr>, Box<Expr>) -> Expr),
        ));
        let binary_1 = unary.clone().foldl(
            op1.then(unary).repeated(), // Operator followed by operand, repeated
            |lhs, (op_fn, rhs)| op_fn(Box::new(lhs), Box::new(rhs)), // Folding function
        );

        // Addition and subtraction have lower precedence
        let op2 = choice((
            just(Token::Add).to(Expr::Add as fn(Box<Expr>, Box<Expr>) -> Expr),
            just(Token::Sub).to(Expr::Sub as fn(Box<Expr>, Box<Expr>) -> Expr),
        ));
        let binary_2 = binary_1.clone().foldl(
            op2.then(binary_1).repeated(), // Operator followed by operand (binary_1), repeated
            |lhs, (op_fn, rhs)| op_fn(Box::new(lhs), Box::new(rhs)), // Folding function
        );

        binary_2
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chumsky::Parser;

    fn parser_test_helper(tokens: Vec<Token>) -> Result<Expr, Vec<Rich<'static, Token<'static>>>> {
        let static_tokens: Vec<Token<'static>> = tokens
            .into_iter()
            .map(|t| match t {
                Token::Int(s) => {
                    let leaked_str: &'static str = Box::leak(s.to_string().into_boxed_str());
                    Token::Int(leaked_str)
                }
                Token::Add => Token::Add,
                Token::Sub => Token::Sub,
                Token::Mul => Token::Mul,
                Token::Div => Token::Div,
                Token::LParen => Token::LParen,
                Token::RParen => Token::RParen,
                Token::Error => Token::Error,
                Token::Whitespace => Token::Whitespace,
            })
            .collect();

        parser()
            .parse(Stream::from_iter(static_tokens))
            .into_result()
            .map_err(|e| e.into_iter().map(|rich| rich.into_owned()).collect())
    }

    #[test]
    fn parse_positive_integer() {
        let tokens = vec![Token::Int("42")];
        assert_eq!(parser_test_helper(tokens), Ok(Expr::Int(42)));
    }

    #[test]
    fn parse_unary_negation() {
        let tokens = vec![Token::Sub, Token::Int("42")];
        assert_eq!(
            parser_test_helper(tokens),
            Ok(Expr::Neg(Box::new(Expr::Int(42))))
        );
    }

    #[test]
    fn parse_nested_negation() {
        let tokens = vec![
            Token::Sub,
            Token::LParen,
            Token::Sub,
            Token::Int("42"),
            Token::RParen,
        ];
        assert_eq!(
            parser_test_helper(tokens),
            Ok(Expr::Neg(Box::new(Expr::Neg(Box::new(Expr::Int(42))))))
        );
    }

    #[test]
    fn parse_multiple_negation() {
        let tokens = vec![Token::Sub, Token::Sub, Token::Int("42")];
        assert_eq!(
            parser_test_helper(tokens),
            Ok(Expr::Neg(Box::new(Expr::Neg(Box::new(Expr::Int(42))))))
        );
    }

    #[test]
    fn parse_addition() {
        let tokens = vec![Token::Int("1"), Token::Add, Token::Int("2")];
        assert_eq!(
            parser_test_helper(tokens),
            Ok(Expr::Add(Box::new(Expr::Int(1)), Box::new(Expr::Int(2))))
        );
    }

    #[test]
    fn parse_subtraction() {
        let tokens = vec![Token::Int("1"), Token::Sub, Token::Int("2")];
        assert_eq!(
            parser_test_helper(tokens),
            Ok(Expr::Sub(Box::new(Expr::Int(1)), Box::new(Expr::Int(2))))
        );
    }

    #[test]
    fn parse_multiplication() {
        let tokens = vec![Token::Int("2"), Token::Mul, Token::Int("3")];
        assert_eq!(
            parser_test_helper(tokens),
            Ok(Expr::Mul(Box::new(Expr::Int(2)), Box::new(Expr::Int(3))))
        );
    }

    #[test]
    fn parse_division() {
        let tokens = vec![Token::Int("4"), Token::Div, Token::Int("2")];
        assert_eq!(
            parser_test_helper(tokens),
            Ok(Expr::Div(Box::new(Expr::Int(4)), Box::new(Expr::Int(2))))
        );
    }

    #[test]
    fn parse_precedence() {
        let tokens = vec![
            Token::Int("1"),
            Token::Add,
            Token::Int("2"),
            Token::Mul,
            Token::Int("3"),
        ];
        assert_eq!(
            parser_test_helper(tokens),
            Ok(Expr::Add(
                Box::new(Expr::Int(1)),
                Box::new(Expr::Mul(Box::new(Expr::Int(2)), Box::new(Expr::Int(3))))
            ))
        );
    }

    #[test]
    fn parse_precedence_with_negation() {
        let tokens = vec![
            Token::Int("1"),
            Token::Add,
            Token::Sub,
            Token::Int("2"),
            Token::Mul,
            Token::Int("3"),
        ];
        assert_eq!(
            parser_test_helper(tokens),
            Ok(Expr::Add(
                Box::new(Expr::Int(1)),
                Box::new(Expr::Mul(
                    Box::new(Expr::Neg(Box::new(Expr::Int(2)))),
                    Box::new(Expr::Int(3))
                ))
            ))
        );
    }

    #[test]
    fn parse_negation_precedence() {
        let tokens = vec![Token::Sub, Token::Int("1"), Token::Mul, Token::Int("2")];
        assert_eq!(
            parser_test_helper(tokens),
            Ok(Expr::Mul(
                Box::new(Expr::Neg(Box::new(Expr::Int(1)))),
                Box::new(Expr::Int(2))
            ))
        );
    }

    #[test]
    fn parse_parentheses() {
        let tokens = vec![
            Token::LParen,
            Token::Int("1"),
            Token::Add,
            Token::Int("2"),
            Token::RParen,
            Token::Mul,
            Token::Int("3"),
        ];
        assert_eq!(
            parser_test_helper(tokens),
            Ok(Expr::Mul(
                Box::new(Expr::Add(Box::new(Expr::Int(1)), Box::new(Expr::Int(2)))),
                Box::new(Expr::Int(3))
            ))
        );
    }

    #[test]
    fn parse_complex_expression() {
        let tokens = vec![
            Token::Sub,
            Token::LParen,
            Token::Int("1"),
            Token::Add,
            Token::Int("2"),
            Token::RParen,
            Token::Mul,
            Token::Int("3"),
            Token::Div,
            Token::Sub,
            Token::Int("4"),
        ];
        assert_eq!(
            parser_test_helper(tokens),
            Ok(Expr::Div(
                Box::new(Expr::Mul(
                    Box::new(Expr::Neg(Box::new(Expr::Add(
                        Box::new(Expr::Int(1)),
                        Box::new(Expr::Int(2))
                    )))),
                    Box::new(Expr::Int(3))
                )),
                Box::new(Expr::Neg(Box::new(Expr::Int(4))))
            ))
        );
    }
}
