use chumsky::{select, Parser};
use chumsky::error::Simple;
use chumsky::primitive::{end, just};
use chumsky::recursive::recursive;
use crate::ast::Expr;
use crate::lexer::Token;

#[allow(clippy::let_and_return)]
pub fn parser() -> impl Parser<Token, Expr, Error = Simple<Token>> {
    recursive(|p| {
        let atom = {
            let parenthesized = p
                .clone()
                .delimited_by(just(Token::LParen), just(Token::RParen));

            let integer = select! {
                Token::Integer(n) => Expr::Int(n),
            };

            parenthesized.or(integer)
        };

        let unary = just(Token::Minus)
            .repeated()
            .then(atom)
            .foldr(|_op, rhs| Expr::Neg(Box::new(rhs)));

        let binary_1 = unary
            .clone()
            .then(
                just(Token::Multiply)
                    .or(just(Token::Divide))
                    .then(unary)
                    .repeated(),
            )
            .foldl(|lhs, (op, rhs)| match op {
                Token::Multiply => Expr::Mul(Box::new(lhs), Box::new(rhs)),
                Token::Divide => Expr::Div(Box::new(lhs), Box::new(rhs)),
                _ => unreachable!(),
            });

        let binary_2 = binary_1
            .clone()
            .then(
                just(Token::Plus)
                    .or(just(Token::Minus))
                    .then(binary_1)
                    .repeated(),
            )
            .foldl(|lhs, (op, rhs)| match op {
                Token::Plus => Expr::Add(Box::new(lhs), Box::new(rhs)),
                Token::Minus => Expr::Sub(Box::new(lhs), Box::new(rhs)),
                _ => unreachable!(),
            });

        binary_2
    })
        .then_ignore(end())
}