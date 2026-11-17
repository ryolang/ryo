use crate::lexer::{Token, lex};
use ryo_core::diag::{DiagCode, DiagSink};
use ryo_core::types::InternPool;

#[test]
fn lex_int_overflow_emits_error() {
    let mut pool = InternPool::new();
    let mut sink = DiagSink::new();
    let toks = lex("99999999999999999999", &mut pool, &mut sink);
    let diags = sink.into_diags();
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].code, DiagCode::ParseError);
    assert_eq!(
        diags[0].message,
        "invalid integer literal: '99999999999999999999'"
    );
    assert!(
        toks.iter().any(|(t, _)| *t == Token::IntLit(0)),
        "recovery token IntLit(0) present: {:?}",
        toks
    );
}

#[test]
fn lex_invalid_character_emits_diag_and_continues() {
    let mut pool = InternPool::new();
    let mut sink = DiagSink::new();
    let toks = lex("x = @\ny = 2", &mut pool, &mut sink);
    let diags = sink.into_diags();
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].code, DiagCode::InvalidCharacter);
    assert_eq!(diags[0].message, "invalid character '@'");
    assert_eq!(diags[0].span, ryo_core::span::Span::new(4, 5));
    assert!(toks.iter().any(|(t, _)| matches!(t, Token::Error)));
    let y = pool.intern_str("y");
    assert!(
        toks.iter().any(|(t, _)| *t == Token::Ident(y)),
        "tokens after the invalid character still lexed: {:?}",
        toks
    );
}
