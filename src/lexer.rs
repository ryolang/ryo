use logos::Logos;
use std::fmt;

#[derive(Logos, Debug, PartialEq, Eq, Hash, Clone)]
pub enum Token<'a> {
    Error,

    // Literals
    #[regex(r"[0-9]+")]
    Int(&'a str),
    //#[regex(r"[+-]?([0-9]*[.])?[0-9]+")]
    //Float(&'a str),

    // Keywords
    #[token("fn")]
    Fn,
    #[token("if")]
    If,
    #[token("else")]
    Else,
    #[token("return")]
    Return,
    #[token("mut")]
    Mut,
    #[token("struct")]
    Struct,
    #[token("enum")]
    Enum,
    #[token("match")]
    Match,

    // Identifiers (must come after keywords)
    #[regex(r"[a-zA-Z_][a-zA-Z0-9_]*")]
    Ident(&'a str),

    // Operators - Arithmetic
    #[token("+")]
    Add,
    #[token("-")]
    Sub,
    #[token("*")]
    Mul,
    #[token("/")]
    Div,

    // Operators - Assignment and Type Annotation
    #[token("=")]
    Assign,
    #[token(":")]
    Colon,

    // Punctuation
    #[token("(")]
    LParen,
    #[token(")")]
    RParen,
    #[token("{")]
    LBrace,
    #[token("}")]
    RBrace,

    // Comments (skip to end of line)
    #[regex(r"#[^\n]*", logos::skip)]
    Comment,

    // Whitespace (skip)
    #[regex(r"[ \t\f\n]+", logos::skip)]
    Whitespace,
}

impl fmt::Display for Token<'_> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Int(s) => write!(f, "{}", s),
            //Self::Float(s) => write!(f, "{}", s),

            // Keywords
            Self::Fn => write!(f, "fn"),
            Self::If => write!(f, "if"),
            Self::Else => write!(f, "else"),
            Self::Return => write!(f, "return"),
            Self::Mut => write!(f, "mut"),
            Self::Struct => write!(f, "struct"),
            Self::Enum => write!(f, "enum"),
            Self::Match => write!(f, "match"),

            // Identifiers
            Self::Ident(s) => write!(f, "{}", s),

            // Operators - Arithmetic
            Self::Add => write!(f, "+"),
            Self::Sub => write!(f, "-"),
            Self::Mul => write!(f, "*"),
            Self::Div => write!(f, "/"),

            // Operators - Assignment and Type Annotation
            Self::Assign => write!(f, "="),
            Self::Colon => write!(f, ":"),

            // Punctuation
            Self::LParen => write!(f, "("),
            Self::RParen => write!(f, ")"),
            Self::LBrace => write!(f, "{{"),
            Self::RBrace => write!(f, "}}"),

            // Comments and Whitespace
            Self::Comment => write!(f, "<comment>"),
            Self::Whitespace => write!(f, "<whitespace>"),
            Self::Error => write!(f, "<error>"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper function to tokenize a string and collect all tokens (except skipped ones)
    fn tokenize(input: &str) -> Vec<Token<'_>> {
        Token::lexer(input)
            .filter_map(|result| result.ok())
            .collect()
    }

    #[test]
    fn lex_keywords() {
        let tokens = tokenize("fn if else return mut struct enum match");
        assert_eq!(tokens.len(), 8);
        assert_eq!(tokens[0], Token::Fn);
        assert_eq!(tokens[1], Token::If);
        assert_eq!(tokens[2], Token::Else);
        assert_eq!(tokens[3], Token::Return);
        assert_eq!(tokens[4], Token::Mut);
        assert_eq!(tokens[5], Token::Struct);
        assert_eq!(tokens[6], Token::Enum);
        assert_eq!(tokens[7], Token::Match);
    }

    #[test]
    fn lex_simple_identifier() {
        let tokens = tokenize("foo");
        assert_eq!(tokens.len(), 1);
        assert!(matches!(tokens[0], Token::Ident("foo")));
    }

    #[test]
    fn lex_identifier_with_underscores() {
        let tokens = tokenize("my_var _private __dunder");
        assert_eq!(tokens.len(), 3);
        assert!(matches!(tokens[0], Token::Ident("my_var")));
        assert!(matches!(tokens[1], Token::Ident("_private")));
        assert!(matches!(tokens[2], Token::Ident("__dunder")));
    }

    #[test]
    fn lex_identifier_with_numbers() {
        let tokens = tokenize("var1 test42 x9y8z7");
        assert_eq!(tokens.len(), 3);
        assert!(matches!(tokens[0], Token::Ident("var1")));
        assert!(matches!(tokens[1], Token::Ident("test42")));
        assert!(matches!(tokens[2], Token::Ident("x9y8z7")));
    }

    #[test]
    fn lex_assignment_operator() {
        let tokens = tokenize("x = 5");
        assert_eq!(tokens.len(), 3);
        assert!(matches!(tokens[0], Token::Ident("x")));
        assert_eq!(tokens[1], Token::Assign);
        assert!(matches!(tokens[2], Token::Int("5")));
    }

    #[test]
    fn lex_colon_operator() {
        let tokens = tokenize("x: int");
        assert_eq!(tokens.len(), 3);
        assert!(matches!(tokens[0], Token::Ident("x")));
        assert_eq!(tokens[1], Token::Colon);
        assert!(matches!(tokens[2], Token::Ident("int")));
    }

    #[test]
    fn lex_curly_braces() {
        let tokens = tokenize("{ }");
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[0], Token::LBrace);
        assert_eq!(tokens[1], Token::RBrace);
    }

    #[test]
    fn lex_struct_literal() {
        let tokens = tokenize("Point(x=1 y=2)");
        assert_eq!(tokens.len(), 9);
        assert!(matches!(tokens[0], Token::Ident("Point")));
        assert_eq!(tokens[1], Token::LParen);
        assert!(matches!(tokens[2], Token::Ident("x")));
        assert_eq!(tokens[3], Token::Assign);
        assert!(matches!(tokens[4], Token::Int("1")));
        assert!(matches!(tokens[5], Token::Ident("y")));
        assert_eq!(tokens[6], Token::Assign);
        assert!(matches!(tokens[7], Token::Int("2")));
        assert_eq!(tokens[8], Token::RParen);
    }

    #[test]
    fn lex_comment_single_line() {
        let tokens = tokenize("x = 5 # this is a comment");
        assert_eq!(tokens.len(), 3);
        assert!(matches!(tokens[0], Token::Ident("x")));
        assert_eq!(tokens[1], Token::Assign);
        assert!(matches!(tokens[2], Token::Int("5")));
    }

    #[test]
    fn lex_comment_entire_line() {
        let tokens = tokenize("# this is just a comment");
        assert_eq!(tokens.len(), 0);
    }

    #[test]
    fn lex_function_definition() {
        let tokens = tokenize("fn add(a: int, b: int) -> int:");
        assert!(tokens.iter().any(|t| matches!(t, Token::Fn)));
        assert!(tokens.iter().any(|t| matches!(t, Token::Ident("add"))));
        assert!(tokens.iter().any(|t| t == &Token::Colon));
    }

    #[test]
    fn lex_variable_declaration() {
        let tokens = tokenize("mut counter: int = 0");
        assert_eq!(tokens[0], Token::Mut);
        assert!(matches!(tokens[1], Token::Ident("counter")));
        assert_eq!(tokens[2], Token::Colon);
        assert!(matches!(tokens[3], Token::Ident("int")));
        assert_eq!(tokens[4], Token::Assign);
        assert!(matches!(tokens[5], Token::Int("0")));
    }

    #[test]
    fn lex_if_statement() {
        let tokens = tokenize("if x > 0:");
        assert_eq!(tokens[0], Token::If);
        assert!(matches!(tokens[1], Token::Ident("x")));
        assert_eq!(tokens[3], Token::Colon);
    }

    #[test]
    fn lex_arithmetic_mixed_with_identifiers() {
        let tokens = tokenize("2 + 3 * 4");
        assert_eq!(tokens.len(), 5);
        assert!(matches!(tokens[0], Token::Int("2")));
        assert_eq!(tokens[1], Token::Add);
        assert!(matches!(tokens[2], Token::Int("3")));
        assert_eq!(tokens[3], Token::Mul);
        assert!(matches!(tokens[4], Token::Int("4")));
    }

    #[test]
    fn lex_whitespace_handling() {
        let tokens = tokenize("x   =   5");
        assert_eq!(tokens.len(), 3);
        assert!(matches!(tokens[0], Token::Ident("x")));
        assert_eq!(tokens[1], Token::Assign);
        assert!(matches!(tokens[2], Token::Int("5")));
    }

    #[test]
    fn lex_newline_handling() {
        let tokens = tokenize("x = 5\ny = 10");
        assert_eq!(tokens.len(), 6);
        assert!(matches!(tokens[0], Token::Ident("x")));
        assert_eq!(tokens[1], Token::Assign);
        assert!(matches!(tokens[2], Token::Int("5")));
        assert!(matches!(tokens[3], Token::Ident("y")));
        assert_eq!(tokens[4], Token::Assign);
        assert!(matches!(tokens[5], Token::Int("10")));
    }

    #[test]
    fn lex_keywords_not_part_of_identifier() {
        // Keywords should not match if they're part of a larger identifier
        let tokens = tokenize("function ifx returnx");
        assert_eq!(tokens.len(), 3);
        assert!(matches!(tokens[0], Token::Ident("function")));
        assert!(matches!(tokens[1], Token::Ident("ifx")));
        assert!(matches!(tokens[2], Token::Ident("returnx")));
    }

    #[test]
    fn lex_match_expression() {
        let tokens = tokenize("match value:");
        assert_eq!(tokens[0], Token::Match);
        assert!(matches!(tokens[1], Token::Ident("value")));
        assert_eq!(tokens[2], Token::Colon);
    }

    #[test]
    fn lex_struct_definition() {
        let tokens = tokenize("struct Point:");
        assert_eq!(tokens[0], Token::Struct);
        assert!(matches!(tokens[1], Token::Ident("Point")));
        assert_eq!(tokens[2], Token::Colon);
    }

    #[test]
    fn lex_enum_definition() {
        let tokens = tokenize("enum Color:");
        assert_eq!(tokens[0], Token::Enum);
        assert!(matches!(tokens[1], Token::Ident("Color")));
        assert_eq!(tokens[2], Token::Colon);
    }

    #[test]
    fn lex_return_statement() {
        let tokens = tokenize("return x");
        assert_eq!(tokens[0], Token::Return);
        assert!(matches!(tokens[1], Token::Ident("x")));
    }
}
