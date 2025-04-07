use std::fmt;
use logos::Logos;

#[derive(Logos, Debug, PartialEq, Eq, Hash, Clone)]
pub enum Token<'a> {
    Error,

    #[regex(r"[0-9]+")]
    Int(&'a str),
    //#[regex(r"[+-]?([0-9]*[.])?[0-9]+")]
    //Float(&'a str),

    #[token("+")]
    Add,
    #[token("-")]
    Sub,
    #[token("*")]
    Mul,
    #[token("/")]
    Div,

    #[token("(")]
    LParen,
    #[token(")")]
    RParen,

    #[regex(r"[ \t\f\n]+", logos::skip)]
    Whitespace,
}

impl fmt::Display for Token<'_> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Int(s) => write!(f, "{}", s),
            //Self::Float(s) => write!(f, "{}", s),
            Self::Add => write!(f, "+"),
            Self::Sub => write!(f, "-"),
            Self::Mul => write!(f, "*"),
            Self::Div => write!(f, "/"),
            Self::LParen => write!(f, "("),
            Self::RParen => write!(f, ")"),
            Self::Whitespace => write!(f, "<whitespace>"),
            Self::Error => write!(f, "<error>"),
        }
    }
}
