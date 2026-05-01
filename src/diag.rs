//! Structured diagnostics for the middle-end and beyond.
//!
//! Replaces ad-hoc `Result<_, String>` plumbing with a `Diag` value
//! that carries a span, a severity, a stable `DiagCode`, a human
//! message, and optional notes. A `DiagSink` accumulates diagnostics
//! so analysis passes can continue past the first error and surface
//! several problems in a single run.
//!
//! `DiagCode` is an enum (not a stringly-typed key) so renderers,
//! tests, and future LSP/JSON output can pattern-match without
//! scraping message text.
//!
//! Reserved variants below are intentionally listed but never
//! constructed today; they exist so that adding comptime, generics,
//! and cycle detection later is a pure additive change here instead
//! of another diagnostics rewrite.

use chumsky::span::{SimpleSpan, Span as _};

pub type Span = SimpleSpan;

/// Soft cap on how many diagnostics a single sink will retain.
///
/// Beyond this threshold further diagnostics are dropped and a single
/// trailing "too many errors" note is emitted on render. Mirrors
/// rustc's behaviour and keeps `DiagSink` from masking later real
/// errors with megabytes of cascading garbage.
pub const MAX_DIAGS: usize = 100;

/// A structured diagnostic.
///
/// Construction is deliberately verbose; helpers like `Diag::error`
/// exist for the common path.
#[derive(Debug, Clone)]
pub struct Diag {
    pub span: Span,
    pub severity: Severity,
    pub code: DiagCode,
    pub message: String,
    pub notes: Vec<DiagNote>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    #[allow(dead_code)]
    Warning,
    #[allow(dead_code)]
    Note,
}

/// Stable error identity. Renderers, tests, and future tooling
/// pattern-match on this rather than on `message`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum DiagCode {
    // --- astgen ---
    UnknownType,
    NestedFunctionDef,
    TopLevelWithExplicitMain,
    /// `fn main()` declared with parameters or a return type.
    /// `main` must be `fn main():` (no args, no return).
    /// Use the future `exit(code)` builtin for non-zero exit codes.
    MainSignature,

    // --- sema ---
    UndefinedVariable,
    UndefinedFunction,
    TypeMismatch,
    ArityMismatch,
    BuiltinArgKind,
    UnsupportedOperator,
    /// A `void` value (e.g. the result of a void-returning call
    /// like `print(...)`) appeared in an expression position that
    /// requires a real value (var decl initializer, return
    /// expression, operand of an arithmetic/equality/etc op).
    VoidValueInExpression,
    /// An `if` or `elif` condition expression evaluated to a non-bool
    /// type. Ryo requires conditions to be `bool` (no truthy coercion).
    ConditionNotBool,
    /// A declaration's resolution requires its own resolution to be
    /// already complete — e.g. a chain of decls whose types depend
    /// transitively on themselves. Today this is an infrastructure-
    /// only signal (mutual recursion through *bodies* doesn't
    /// trigger it because signatures resolve eagerly), but the
    /// worklist driver in Phase 5 catches the case so future
    /// comptime / inferred-return-type work doesn't stack-overflow.
    CycleInResolution,

    // --- parser ---
    ParseError,

    /// Emitted by `DiagSink::into_diags` when the sink dropped
    /// diagnostics past `MAX_DIAGS`. Distinct from `ParseError` so
    /// renderers / tests / future LSP code can identify the
    /// suppression note without scraping the message text.
    TooManyDiagnostics,

    // --- reserved (not constructed in Phase 1) ---
    ConstEvalFailure,
    CycleInComptime,
    GenericInstantiation,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct DiagNote {
    pub span: Option<Span>,
    pub message: String,
}

impl Diag {
    pub fn error(span: Span, code: DiagCode, message: impl Into<String>) -> Self {
        Diag {
            span,
            severity: Severity::Error,
            code,
            message: message.into(),
            notes: Vec::new(),
        }
    }

    #[allow(dead_code)]
    pub fn with_note(mut self, span: Option<Span>, message: impl Into<String>) -> Self {
        self.notes.push(DiagNote {
            span,
            message: message.into(),
        });
        self
    }
}

/// Accumulator for diagnostics emitted by a single compilation pass.
///
/// Passes thread `&mut DiagSink` through their internals and call
/// `emit` instead of returning `Err`. A pass returns `Ok(())` if
/// `sink.has_errors()` is false at the end; otherwise the driver
/// pulls diagnostics out via `into_diags` and renders them.
#[derive(Debug, Default)]
pub struct DiagSink {
    diags: Vec<Diag>,
    error_count: usize,
    truncated: bool,
}

impl DiagSink {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn emit(&mut self, d: Diag) {
        // Count errors *before* the truncation check so
        // `has_errors()` / `error_count()` reflect every reported
        // problem even when storage is capped — otherwise a pass
        // that overflowed `MAX_DIAGS` could quietly lose its
        // "failed" status. We still drop the `Diag` body once we're
        // at the cap, with a single trailing note appended in
        // `into_diags`.
        if d.severity == Severity::Error {
            self.error_count += 1;
        }
        if self.diags.len() >= MAX_DIAGS {
            self.truncated = true;
            return;
        }
        self.diags.push(d);
    }

    pub fn has_errors(&self) -> bool {
        self.error_count > 0
    }

    #[allow(dead_code)]
    pub fn error_count(&self) -> usize {
        self.error_count
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.diags.is_empty()
    }

    pub fn into_diags(mut self) -> Vec<Diag> {
        if self.truncated {
            // Surface the truncation at the end so the user knows
            // there were more problems than we kept. Tagged with
            // its own DiagCode so downstream tooling can spot the
            // suppression note reliably.
            self.diags.push(Diag {
                span: SimpleSpan::new((), 0..0),
                severity: Severity::Note,
                code: DiagCode::TooManyDiagnostics,
                message: format!(
                    "too many diagnostics; suppressed everything after the first {}",
                    MAX_DIAGS
                ),
                notes: Vec::new(),
            });
        }
        self.diags
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span() -> Span {
        SimpleSpan::new((), 0..0)
    }

    #[test]
    fn sink_accumulates_multiple_errors() {
        let mut sink = DiagSink::new();
        sink.emit(Diag::error(span(), DiagCode::TypeMismatch, "a"));
        sink.emit(Diag::error(span(), DiagCode::UndefinedVariable, "b"));
        assert!(sink.has_errors());
        assert_eq!(sink.error_count(), 2);
        assert_eq!(sink.into_diags().len(), 2);
    }

    #[test]
    fn warnings_do_not_count_as_errors() {
        let mut sink = DiagSink::new();
        sink.emit(Diag {
            span: span(),
            severity: Severity::Warning,
            code: DiagCode::ParseError,
            message: "w".into(),
            notes: vec![],
        });
        assert!(!sink.has_errors());
    }

    #[test]
    fn sink_caps_at_max_diags_and_appends_truncation_note() {
        let mut sink = DiagSink::new();
        for _ in 0..(MAX_DIAGS + 5) {
            sink.emit(Diag::error(span(), DiagCode::TypeMismatch, "x"));
        }
        // error_count must reflect every emitted error, even those
        // dropped past MAX_DIAGS — has_errors() should still be true
        // for a pass that overflowed the cap.
        assert!(sink.has_errors());
        assert_eq!(sink.error_count(), MAX_DIAGS + 5);
        let diags = sink.into_diags();
        // MAX_DIAGS retained + 1 truncation note tagged with its
        // own DiagCode.
        assert_eq!(diags.len(), MAX_DIAGS + 1);
        let last = diags.last().unwrap();
        assert_eq!(last.severity, Severity::Note);
        assert_eq!(last.code, DiagCode::TooManyDiagnostics);
    }
}
