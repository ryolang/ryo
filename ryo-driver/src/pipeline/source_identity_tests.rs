use super::compilation_from_source;
use super::diagnostics::render_diags_with_writer;
use ryo_core::compilation::CompilationContext;
use ryo_core::diag::{Diag, DiagCode};
use ryo_core::errors::CompilerError;
use ryo_core::span::Span;
use std::path::{Path, PathBuf};

fn plain_output(bytes: &[u8]) -> String {
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    let mut plain = String::new();
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.next() == Some('[') {
            for code in chars.by_ref() {
                if code.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            plain.push(ch);
        }
    }
    plain
}

#[test]
fn cross_file_labels_use_their_own_sources() {
    let mut context = CompilationContext::new();
    let first = context.register_source(PathBuf::from("first.ryo"), "éname\n".into());
    let second = context.register_source(PathBuf::from("second.ryo"), "🌍🌍other\n".into());
    let diag = Diag::error(
        Span::in_source(first, 2, 6),
        DiagCode::TypeMismatch,
        "primary message",
    )
    .with_note(Some(Span::in_source(second, 8, 13)), "defined here");
    let mut output = Vec::new();

    render_diags_with_writer(&[diag], context.sources(), &mut output).unwrap();
    let output = plain_output(&output);

    assert!(output.contains("first.ryo:1:2"), "{output}");
    assert!(output.contains("second.ryo:1:3"), "{output}");
    assert!(output.contains("éname"), "{output}");
    assert!(output.contains("🌍🌍other"), "{output}");
    assert!(output.contains("defined here"), "{output}");
}

#[test]
fn source_less_diagnostic_does_not_borrow_a_filename() {
    let mut context = CompilationContext::new();
    context.register_source(PathBuf::from("do-not-use.ryo"), "x = 1\n".into());
    let diag = Diag::error(
        Span::EMPTY,
        DiagCode::TooManyDiagnostics,
        "synthetic message",
    );
    let mut output = Vec::new();

    render_diags_with_writer(&[diag], context.sources(), &mut output).unwrap();
    let output = plain_output(&output);

    assert!(output.contains("E0101: synthetic message"), "{output}");
    assert!(!output.contains("do-not-use.ryo"), "{output}");
}

#[test]
fn same_index_foreign_source_is_an_internal_error() {
    let mut first = CompilationContext::new();
    let mut second = CompilationContext::new();
    let first_id = first.register_source(PathBuf::from("first.ryo"), "first".into());
    second.register_source(PathBuf::from("second.ryo"), "second".into());
    let diag = Diag::error(
        Span::in_source(first_id, 0, 1),
        DiagCode::TypeMismatch,
        "wrong context",
    );
    let mut output = Vec::new();

    let result = render_diags_with_writer(&[diag], second.sources(), &mut output);
    assert!(matches!(result, Err(CompilerError::InternalError(_))));
}

#[test]
fn stale_revision_maps_to_internal_error() {
    let mut context = CompilationContext::new();
    let old = context.register_source(PathBuf::from("main.ryo"), "old".into());
    context.replace_source(old, "new".into()).unwrap();

    let error = context.sources().require_current(old).unwrap_err();
    let error: CompilerError = error.into();
    assert!(matches!(error, CompilerError::InternalError(_)));
}

#[test]
fn malformed_spans_are_internal_errors() {
    let mut context = CompilationContext::new();
    let id = context.register_source(PathBuf::from("main.ryo"), "😀x".into());
    for span in [
        Span::in_source(id, 0, 99),
        Span::in_source(id, 1, 4),
        Span::new(0, 1),
    ] {
        let diag = Diag::error(span, DiagCode::TypeMismatch, "bad span");
        let mut output = Vec::new();
        let result = render_diags_with_writer(&[diag], context.sources(), &mut output);
        assert!(matches!(result, Err(CompilerError::InternalError(_))));
    }
}

#[test]
fn single_file_registration_is_current() {
    let (context, id) = compilation_from_source(Path::new("single.ryo"), "x = 1\n".into()).unwrap();
    let source = context.sources().require_current(id).unwrap();
    assert_eq!(source.name(), Path::new("single.ryo"));
    assert_eq!(source.text(), "x = 1\n");
}

#[test]
fn internal_error_display_is_stable() {
    let error = CompilerError::InternalError("bad source".into());
    assert_eq!(format!("{error}"), "internal compiler error: bad source");
}
