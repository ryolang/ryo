use crate::lexer::{Token, lex_with_source};
use ryo_core::compilation::CompilationContext;
use ryo_core::diag::DiagSink;
use ryo_core::source::SourceId;
use ryo_core::types::InternPool;
use std::path::PathBuf;

#[test]
fn tokens_and_diagnostics_use_registered_source() {
    let mut context = CompilationContext::new();
    let id = context.register_source(PathBuf::from("lexer.ryo"), "x = @\ny = 2".into());
    let source = context.sources().get(id).unwrap();
    let mut pool = InternPool::new();
    let mut sink = DiagSink::new();

    let tokens = lex_with_source(source, &mut pool, &mut sink);
    let diags = sink.into_diags();

    assert!(
        tokens
            .iter()
            .any(|(token, _)| matches!(token, Token::Error))
    );
    assert!(tokens.iter().all(|(_, span)| span.context == id));
    assert!(diags.iter().all(|diag| diag.span.source == id));
    assert!(
        tokens
            .iter()
            .all(|(_, span)| span.context != SourceId::NONE)
    );
}
