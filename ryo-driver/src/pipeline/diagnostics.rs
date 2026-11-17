mod source_diagnostics;
use ryo_core::diag::{Diag, DiagCode, Severity};
use ryo_core::errors::CompilerError;
use ryo_frontend::ownership::OwnershipError;
#[cfg(test)]
pub(super) use source_diagnostics::render_diags_with_writer;

pub(super) fn ownership_error_diag(error: OwnershipError) -> Diag {
    match error {
        OwnershipError::LoopDidNotConverge {
            function,
            span,
            passes,
        } => Diag::error(
            span,
            DiagCode::InternalError,
            "internal compiler error: ownership analysis could not prove loop convergence",
        )
        .with_note(None, format!("function: {function}"))
        .with_note(None, format!("loop propagation exhausted {passes} passes")),
        OwnershipError::IncompatibleArtifacts(detail) => Diag::error(
            ryo_core::span::Span::EMPTY,
            DiagCode::InternalError,
            "internal compiler error: ownership analysis received incompatible artifacts",
        )
        .with_note(None, detail),
    }
}

/// Render + wrap for paths whose diagnostics are always error
/// severity (lex / parse failures), where `finalize_diags` therefore
/// always returns `Err`. Keeps the render-and-wrap shape in one
/// place while letting callers keep their own error type.
pub(super) fn fail_with_diags(
    diags: Vec<Diag>,
    sources: &ryo_core::source::SourceDb,
) -> CompilerError {
    finalize_diags(diags, sources)
        .expect_err("fail_with_diags requires an error-severity diagnostic")
}

fn render_diags(diags: &[Diag], sources: &ryo_core::source::SourceDb) -> Result<(), CompilerError> {
    source_diagnostics::render_diags(diags, sources)
}

fn diag_code_str(code: DiagCode) -> &'static str {
    match code {
        DiagCode::InternalError => "ICE",
        DiagCode::UnknownType => "E0001",
        DiagCode::NestedFunctionDef => "E0002",
        DiagCode::TopLevelWithExplicitMain => "E0003",
        DiagCode::MainSignature => "E0004",
        DiagCode::InfiniteSize => "E0005",
        DiagCode::DeriveFieldNotEq => "E0006",
        DiagCode::EmptyEnum => "E0008",
        DiagCode::DuplicateVariant => "E0009",
        DiagCode::EqDeriveRequired => "E0007",
        DiagCode::UndefinedVariable => "E0010",
        DiagCode::UndefinedFunction => "E0011",
        DiagCode::TypeMismatch => "E0012",
        DiagCode::ReservedIdentifier => "E0019",
        DiagCode::ArityMismatch => "E0013",
        DiagCode::BuiltinArgKind => "E0014",
        DiagCode::UnsupportedOperator => "E0015",
        DiagCode::VoidValueInExpression => "E0017",
        DiagCode::ConditionNotBool => "E0018",
        DiagCode::ImmutableAssign => "E0028",
        DiagCode::DuplicateDeclaration => "E0029",
        DiagCode::UndefinedAssignTarget => "E0030",
        DiagCode::FloatModulo => "E0023",
        DiagCode::BreakOutsideLoop => "E0024",
        DiagCode::ContinueOutsideLoop => "E0025",
        DiagCode::RangeArgType => "E0026",
        DiagCode::ReservedBuiltinName => "E0027",
        DiagCode::RedundantMove => "W0002",
        DiagCode::RedundantMaterialize => "W0003",
        DiagCode::RedundantToBytes => "W0004",
        DiagCode::UseAfterMove => "E0020",
        DiagCode::MoveOutOfBorrowedParam => "E0021",
        DiagCode::MoveOutOfField => "E0043",
        DiagCode::ReturnBorrowedValue => "E0022",
        DiagCode::MoveWhileBorrowedInCall => "E0031",
        DiagCode::BorrowMismatch => "E0033",
        DiagCode::MutableAliasingViolation => "E0032",
        DiagCode::DeadStore => "W0001",
        DiagCode::ViewEscape => "E0034",
        DiagCode::SourceProjected => "E0035",
        DiagCode::CycleInResolution => "E0016",
        DiagCode::MissingReturn => "E0036",
        DiagCode::DivisionByZero => "E0037",
        DiagCode::UnknownField => "E0038",
        DiagCode::MissingStructFields => "E0039",
        DiagCode::DuplicateStructField => "E0040",
        DiagCode::NotAStruct => "E0041",
        DiagCode::ViewFieldType => "E0042",
        DiagCode::UnknownEnum => "E0118",
        DiagCode::UnknownVariant => "E0119",
        DiagCode::MissingVariantFields => "E0120",
        DiagCode::DuplicateVariantField => "E0121",
        DiagCode::UnknownVariantField => "E0122",
        DiagCode::PositionalConstructOnNamedVariant => "E0123",
        DiagCode::ParseError => "E0100",
        DiagCode::ChainedComparison => "E0104",
        DiagCode::RangeArity => "E0105",
        DiagCode::EmptyBrackets => "E0106",
        DiagCode::EmptyStructBody => "E0107",
        DiagCode::UnknownAttribute => "E0108",
        DiagCode::EmptyAnonStruct => "E0109",
        DiagCode::ChainedPositionalAccess => "E0110",
        DiagCode::UnitParen => "E0111",
        DiagCode::SingleElemDestructuring => "E0112",
        DiagCode::DestructureArity => "E0113",
        DiagCode::DestructureUnknownField => "E0114",
        DiagCode::DestructurePositionalOnNamed => "E0115",
        DiagCode::AnonFieldNotEq => "E0116",
        DiagCode::ReprCOnEnum => "E0117",
        DiagCode::NamedArgInParens => "E0124",
        DiagCode::UnitVariantConstructed => "E0125",
        DiagCode::MissingArgListOnVariant => "E0126",
        DiagCode::PositionalArgsInBraces => "E0127",
        DiagCode::CommaSeparatedVariants => "E0128",
        DiagCode::NonExhaustiveMatch => "E0129",
        DiagCode::UnreachableArm => "W0005",
        DiagCode::PatternTypeMismatch => "E0131",
        DiagCode::UnknownMatchVariant => "E0132",
        DiagCode::MoveOutOfBorrowedScrutinee => "E0133",
        DiagCode::ArmTypeMismatch => "E0134",
        DiagCode::ArmNoValue => "E0135",
        DiagCode::InvalidMatchScrutinee => "E0136",
        DiagCode::MatchViewResult => "E0137",
        DiagCode::MatchValueArmBorrowed => "E0138",
        DiagCode::NestingLimitExceeded => "E0140",
        DiagCode::TooManyDiagnostics => "E0101",
        DiagCode::InvalidCharacter => "E0102",
        DiagCode::UnknownEscape => "E0103",
        DiagCode::LexerInputTooLarge => "E0130",
        DiagCode::ConstEvalFailure => "E0200",
        DiagCode::CycleInComptime => "E0201",
        DiagCode::GenericInstantiation => "E0202",
    }
}

/// Render a batch of diagnostics and translate into a terminal
/// pipeline result.
///
/// Single tail-block used by every front-end driver (`ryo run`,
/// `ryo build`, `ryo ir`) and by the lex/parse error paths so that:
///
/// * warnings (`W0001` DeadStore, `W0002` RedundantMove, …) reach
///   the user on otherwise-successful runs, and
/// * the success and error paths render the *same* diagnostics
///   exactly once — never via two separate `render_diags` calls
///   that could drift out of sync, and
/// * the `Severity::Error` check lives in exactly one place (the
///   lex/parse paths previously assumed every diag they built was
///   error severity without enforcing it).
///
/// Sink-using stages feed this via `sink.into_diags()`. Returns
/// `Err(CompilerError::Diagnostics(_))` iff at least one diagnostic
/// has `Severity::Error`; warnings/notes alone do not fail the build.
pub(super) fn finalize_diags(
    diags: Vec<Diag>,
    sources: &ryo_core::source::SourceDb,
) -> Result<(), CompilerError> {
    let has_errors = diags.iter().any(|d| d.severity == Severity::Error);
    if !diags.is_empty() {
        render_diags(&diags, sources)?;
    }
    if has_errors {
        Err(CompilerError::Diagnostics(diags))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn diag_code_strings_are_stable_and_unique() {
        let expected: &[(DiagCode, &str)] = &[
            (DiagCode::InternalError, "ICE"),
            (DiagCode::UnknownType, "E0001"),
            (DiagCode::NestedFunctionDef, "E0002"),
            (DiagCode::TopLevelWithExplicitMain, "E0003"),
            (DiagCode::MainSignature, "E0004"),
            (DiagCode::InfiniteSize, "E0005"),
            (DiagCode::DeriveFieldNotEq, "E0006"),
            (DiagCode::EmptyEnum, "E0008"),
            (DiagCode::DuplicateVariant, "E0009"),
            (DiagCode::EqDeriveRequired, "E0007"),
            (DiagCode::UndefinedVariable, "E0010"),
            (DiagCode::UndefinedFunction, "E0011"),
            (DiagCode::TypeMismatch, "E0012"),
            (DiagCode::ArityMismatch, "E0013"),
            (DiagCode::BuiltinArgKind, "E0014"),
            (DiagCode::UnsupportedOperator, "E0015"),
            (DiagCode::CycleInResolution, "E0016"),
            (DiagCode::VoidValueInExpression, "E0017"),
            (DiagCode::ConditionNotBool, "E0018"),
            (DiagCode::ReservedIdentifier, "E0019"),
            (DiagCode::UseAfterMove, "E0020"),
            (DiagCode::MoveOutOfBorrowedParam, "E0021"),
            (DiagCode::ReturnBorrowedValue, "E0022"),
            (DiagCode::FloatModulo, "E0023"),
            (DiagCode::BreakOutsideLoop, "E0024"),
            (DiagCode::ContinueOutsideLoop, "E0025"),
            (DiagCode::RangeArgType, "E0026"),
            (DiagCode::ReservedBuiltinName, "E0027"),
            (DiagCode::ImmutableAssign, "E0028"),
            (DiagCode::DuplicateDeclaration, "E0029"),
            (DiagCode::UndefinedAssignTarget, "E0030"),
            (DiagCode::MoveWhileBorrowedInCall, "E0031"),
            (DiagCode::MutableAliasingViolation, "E0032"),
            (DiagCode::BorrowMismatch, "E0033"),
            (DiagCode::ViewEscape, "E0034"),
            (DiagCode::SourceProjected, "E0035"),
            (DiagCode::MissingReturn, "E0036"),
            (DiagCode::DivisionByZero, "E0037"),
            (DiagCode::UnknownField, "E0038"),
            (DiagCode::MissingStructFields, "E0039"),
            (DiagCode::DuplicateStructField, "E0040"),
            (DiagCode::NotAStruct, "E0041"),
            (DiagCode::ViewFieldType, "E0042"),
            (DiagCode::UnknownEnum, "E0118"),
            (DiagCode::UnknownVariant, "E0119"),
            (DiagCode::MissingVariantFields, "E0120"),
            (DiagCode::DuplicateVariantField, "E0121"),
            (DiagCode::UnknownVariantField, "E0122"),
            (DiagCode::PositionalConstructOnNamedVariant, "E0123"),
            (DiagCode::MoveOutOfField, "E0043"),
            (DiagCode::ParseError, "E0100"),
            (DiagCode::TooManyDiagnostics, "E0101"),
            (DiagCode::InvalidCharacter, "E0102"),
            (DiagCode::UnknownEscape, "E0103"),
            (DiagCode::LexerInputTooLarge, "E0130"),
            (DiagCode::ChainedComparison, "E0104"),
            (DiagCode::RangeArity, "E0105"),
            (DiagCode::EmptyBrackets, "E0106"),
            (DiagCode::EmptyStructBody, "E0107"),
            (DiagCode::UnknownAttribute, "E0108"),
            (DiagCode::EmptyAnonStruct, "E0109"),
            (DiagCode::ChainedPositionalAccess, "E0110"),
            (DiagCode::UnitParen, "E0111"),
            (DiagCode::SingleElemDestructuring, "E0112"),
            (DiagCode::DestructureArity, "E0113"),
            (DiagCode::DestructureUnknownField, "E0114"),
            (DiagCode::DestructurePositionalOnNamed, "E0115"),
            (DiagCode::AnonFieldNotEq, "E0116"),
            (DiagCode::ReprCOnEnum, "E0117"),
            (DiagCode::NamedArgInParens, "E0124"),
            (DiagCode::UnitVariantConstructed, "E0125"),
            (DiagCode::MissingArgListOnVariant, "E0126"),
            (DiagCode::PositionalArgsInBraces, "E0127"),
            (DiagCode::CommaSeparatedVariants, "E0128"),
            (DiagCode::NonExhaustiveMatch, "E0129"),
            (DiagCode::UnreachableArm, "W0005"),
            (DiagCode::PatternTypeMismatch, "E0131"),
            (DiagCode::UnknownMatchVariant, "E0132"),
            (DiagCode::MoveOutOfBorrowedScrutinee, "E0133"),
            (DiagCode::ArmTypeMismatch, "E0134"),
            (DiagCode::ArmNoValue, "E0135"),
            (DiagCode::InvalidMatchScrutinee, "E0136"),
            (DiagCode::MatchViewResult, "E0137"),
            (DiagCode::MatchValueArmBorrowed, "E0138"),
            (DiagCode::NestingLimitExceeded, "E0140"),
            (DiagCode::ConstEvalFailure, "E0200"),
            (DiagCode::CycleInComptime, "E0201"),
            (DiagCode::GenericInstantiation, "E0202"),
            (DiagCode::DeadStore, "W0001"),
            (DiagCode::RedundantMove, "W0002"),
            (DiagCode::RedundantMaterialize, "W0003"),
            (DiagCode::RedundantToBytes, "W0004"),
        ];
        let mut seen = HashSet::new();
        for (code, s) in expected {
            assert_eq!(diag_code_str(*code), *s, "{code:?} moved");
            assert!(seen.insert(*s), "duplicate code string {s}");
        }
        // Maintenance tripwire: a match naming every DiagCode
        // variant with no wildcard arm fails to compile the moment a
        // variant is added, forcing the author to this test — extend
        // the table above in the same edit or the new code goes
        // untested. (Rust cannot iterate enum variants, so the table
        // stays the enumeration source; the match just makes "forgot
        // the test" a compile error instead of a silent drop.)
        for code in expected.iter().map(|(c, _)| c) {
            match code {
                DiagCode::UnknownType
                | DiagCode::NestedFunctionDef
                | DiagCode::TopLevelWithExplicitMain
                | DiagCode::MainSignature
                | DiagCode::InfiniteSize
                | DiagCode::DeriveFieldNotEq
                | DiagCode::EmptyEnum
                | DiagCode::DuplicateVariant
                | DiagCode::EqDeriveRequired
                | DiagCode::UndefinedVariable
                | DiagCode::UndefinedFunction
                | DiagCode::TypeMismatch
                | DiagCode::ArityMismatch
                | DiagCode::BuiltinArgKind
                | DiagCode::UnsupportedOperator
                | DiagCode::VoidValueInExpression
                | DiagCode::ConditionNotBool
                | DiagCode::ReservedIdentifier
                | DiagCode::ImmutableAssign
                | DiagCode::DuplicateDeclaration
                | DiagCode::UndefinedAssignTarget
                | DiagCode::FloatModulo
                | DiagCode::BreakOutsideLoop
                | DiagCode::ContinueOutsideLoop
                | DiagCode::RangeArgType
                | DiagCode::ReservedBuiltinName
                | DiagCode::RedundantMove
                | DiagCode::RedundantMaterialize
                | DiagCode::RedundantToBytes
                | DiagCode::UseAfterMove
                | DiagCode::MoveOutOfBorrowedParam
                | DiagCode::MoveOutOfField
                | DiagCode::ReturnBorrowedValue
                | DiagCode::MoveWhileBorrowedInCall
                | DiagCode::BorrowMismatch
                | DiagCode::MutableAliasingViolation
                | DiagCode::DeadStore
                | DiagCode::ViewEscape
                | DiagCode::SourceProjected
                | DiagCode::MissingReturn
                | DiagCode::DivisionByZero
                | DiagCode::UnknownField
                | DiagCode::MissingStructFields
                | DiagCode::DuplicateStructField
                | DiagCode::NotAStruct
                | DiagCode::ViewFieldType
                | DiagCode::UnknownEnum
                | DiagCode::UnknownVariant
                | DiagCode::MissingVariantFields
                | DiagCode::DuplicateVariantField
                | DiagCode::UnknownVariantField
                | DiagCode::PositionalConstructOnNamedVariant
                | DiagCode::CycleInResolution
                | DiagCode::ParseError
                | DiagCode::ChainedComparison
                | DiagCode::RangeArity
                | DiagCode::EmptyBrackets
                | DiagCode::EmptyStructBody
                | DiagCode::UnknownAttribute
                | DiagCode::EmptyAnonStruct
                | DiagCode::ChainedPositionalAccess
                | DiagCode::UnitParen
                | DiagCode::SingleElemDestructuring
                | DiagCode::DestructureArity
                | DiagCode::DestructureUnknownField
                | DiagCode::DestructurePositionalOnNamed
                | DiagCode::AnonFieldNotEq
                | DiagCode::ReprCOnEnum
                | DiagCode::NamedArgInParens
                | DiagCode::UnitVariantConstructed
                | DiagCode::MissingArgListOnVariant
                | DiagCode::PositionalArgsInBraces
                | DiagCode::CommaSeparatedVariants
                | DiagCode::NonExhaustiveMatch
                | DiagCode::UnreachableArm
                | DiagCode::PatternTypeMismatch
                | DiagCode::UnknownMatchVariant
                | DiagCode::MoveOutOfBorrowedScrutinee
                | DiagCode::ArmTypeMismatch
                | DiagCode::ArmNoValue
                | DiagCode::InvalidMatchScrutinee
                | DiagCode::MatchViewResult
                | DiagCode::MatchValueArmBorrowed
                | DiagCode::NestingLimitExceeded
                | DiagCode::TooManyDiagnostics
                | DiagCode::InvalidCharacter
                | DiagCode::UnknownEscape
                | DiagCode::LexerInputTooLarge
                | DiagCode::ConstEvalFailure
                | DiagCode::CycleInComptime
                | DiagCode::GenericInstantiation
                | DiagCode::InternalError => {}
            }
        }
    }

    #[test]
    fn loop_nonconvergence_diag_is_source_located_ice() {
        let span = ryo_core::span::Span::new(4, 9);
        let diag = ownership_error_diag(
            ryo_frontend::ownership::OwnershipError::LoopDidNotConverge {
                function: "main".into(),
                span,
                passes: 0,
            },
        );

        assert_eq!(diag.code, DiagCode::InternalError);
        assert_eq!(diag.span, span);
        assert_eq!(
            diag.message,
            "internal compiler error: ownership analysis could not prove loop convergence"
        );
        let notes: Vec<_> = diag
            .notes
            .iter()
            .map(|note| note.message.as_str())
            .collect();
        assert!(notes.contains(&"function: main"));
        assert!(notes.contains(&"loop propagation exhausted 0 passes"));
    }

    struct FailFirstWrite {
        kind: std::io::ErrorKind,
        failed: bool,
        output: Vec<u8>,
    }

    impl FailFirstWrite {
        fn new(kind: std::io::ErrorKind) -> Self {
            Self {
                kind,
                failed: false,
                output: Vec::new(),
            }
        }
    }

    impl std::io::Write for FailFirstWrite {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.failed {
                self.output.extend_from_slice(buf);
                Ok(buf.len())
            } else {
                self.failed = true;
                Err(std::io::Error::new(self.kind, "render failed"))
            }
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn non_pipe_report_write_failure_uses_plain_fallback() {
        let mut context = ryo_core::compilation::CompilationContext::new();
        let id =
            context.register_source(std::path::PathBuf::from("test.ryo"), "print(name)\n".into());
        let diag = Diag::error(
            ryo_core::span::Span::in_source(id, 6, 10),
            DiagCode::UseAfterMove,
            "use of moved value `name`",
        );
        let mut writer = FailFirstWrite::new(std::io::ErrorKind::Other);

        render_diags_with_writer(&[diag], context.sources(), &mut writer)
            .expect("fallback write should succeed");

        assert_eq!(
            String::from_utf8(writer.output).expect("fallback should be UTF-8"),
            "E0020: use of moved value `name`\n"
        );
    }

    #[test]
    fn closed_pipe_report_write_does_not_use_fallback() {
        let mut context = ryo_core::compilation::CompilationContext::new();
        let id =
            context.register_source(std::path::PathBuf::from("test.ryo"), "print(name)\n".into());
        let diag = Diag::error(
            ryo_core::span::Span::in_source(id, 6, 10),
            DiagCode::UseAfterMove,
            "use of moved value `name`",
        );
        let mut writer = FailFirstWrite::new(std::io::ErrorKind::BrokenPipe);

        render_diags_with_writer(&[diag], context.sources(), &mut writer)
            .expect("closed pipe should be ignored");

        assert!(writer.output.is_empty());
    }
}
