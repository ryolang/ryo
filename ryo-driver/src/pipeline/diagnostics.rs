use ariadne::{Color, Label, Report, ReportKind, Source};
use ryo_core::diag::{Diag, DiagCode, Severity};
use ryo_core::errors::CompilerError;

/// Render + wrap for paths whose diagnostics are always error
/// severity (lex / parse failures), where `finalize_diags` therefore
/// always returns `Err`. Keeps the render-and-wrap shape in one
/// place while letting callers keep their own error type.
pub(super) fn fail_with_diags(diags: Vec<Diag>, input: &str, source_name: &str) -> CompilerError {
    finalize_diags(diags, input, source_name)
        .expect_err("fail_with_diags requires an error-severity diagnostic")
}

/// Render a slice of diagnostics to stderr through Ariadne.
///
/// `source_name` is the user-visible identifier the renderer puts
/// in the report header (e.g. `"examples/hello.ryo"`).
///
/// Regular diagnostics are sorted by start span first to keep output
/// stable regardless of emission order — important once Sema
/// continues past errors and emits several at once. The
/// `TooManyDiagnostics` truncation note carries a synthetic 0..0
/// span and would otherwise sort to the top; it's rendered
/// out-of-band after the sorted sweep so the suppression marker
/// always lands at the bottom of the report.
fn render_diags(diags: &[Diag], input: &str, source_name: &str) {
    let source = Source::from(input);
    // Ariadne 0.6 indexes `Source` by CHARACTER offset, but every span
    // in the compiler is a BYTE offset (logos + `str` slicing). On pure
    // ASCII the two coincide; with any multi-byte character before the
    // span, the byte offset overruns the char length and the squiggle
    // drifts to a later line — or ariadne drops the report entirely
    // when the offset passes the source's char count. Convert once
    // here so every diagnostic (and its note labels) renders at its
    // true position.
    let byte_to_char = byte_to_char_offsets(input);
    let (truncation, regular): (Vec<&Diag>, Vec<&Diag>) = diags
        .iter()
        .partition(|d| d.code == DiagCode::TooManyDiagnostics);

    let mut sorted = regular;
    sorted.sort_by_key(|d| (d.span.start, d.span.end));
    for d in sorted {
        emit_one(d, source_name, &source, &byte_to_char);
    }
    for d in truncation {
        emit_one(d, source_name, &source, &byte_to_char);
    }
}

/// Prefix table mapping byte offsets to character offsets: entry `i`
/// holds the char offset of the char starting at byte `i`. The final
/// entry holds the total char count. Compiler spans always sit on char
/// boundaries (the lexer slices tokens on them), so interior bytes of a
/// multi-byte char — left at their zero-initialized value — are never
/// queried; out-of-range lookups fall back to the char length.
fn byte_to_char_offsets(input: &str) -> Vec<usize> {
    let mut offsets = vec![0; input.len() + 1];
    let mut chars = 0;
    for (byte, _) in input.char_indices() {
        offsets[byte] = chars;
        chars += 1;
    }
    offsets[input.len()] = chars;
    offsets
}

/// Convert a byte span to the char span ariadne's `Source` expects.
fn to_char_span(offsets: &[usize], span: std::ops::Range<usize>) -> std::ops::Range<usize> {
    let fallback = *offsets
        .last()
        .expect("the table always has the len+1 entry");
    offsets.get(span.start).copied().unwrap_or(fallback)
        ..offsets.get(span.end).copied().unwrap_or(fallback)
}

fn emit_one(d: &Diag, source_name: &str, source: &Source<&str>, byte_to_char: &[usize]) {
    let kind = match d.severity {
        Severity::Error => ReportKind::Error,
        Severity::Warning => ReportKind::Warning,
        Severity::Note => ReportKind::Advice,
    };
    let label_color = color_for_severity(d.severity);
    let code = diag_code_str(d.code);
    let span = to_char_span(byte_to_char, d.span.start..d.span.end);
    // The full message goes in the report header only; the label
    // carries no text so the message isn't printed twice.
    let mut report = Report::build(kind, (source_name, span.clone()))
        .with_code(code)
        .with_message(&d.message)
        .with_label(Label::new((source_name, span)).with_color(label_color));
    for note in &d.notes {
        if let Some(span) = note.span {
            report = report.with_label(
                Label::new((
                    source_name,
                    to_char_span(byte_to_char, span.start..span.end),
                ))
                .with_message(&note.message)
                .with_color(Color::Cyan),
            );
        } else {
            report = report.with_note(&note.message);
        }
    }
    if report.finish().eprint((source_name, source)).is_err() {
        // Ariadne can fail on out-of-range spans or stderr write
        // errors; fall back to a plain line rather than panicking
        // mid-report and suppressing the remaining diagnostics.
        eprintln!("{}: {}", code, d.message);
    }
}

/// Map severity to a label color so the squiggle hue matches the
/// report-header `ReportKind`. Red has been overloaded onto every
/// label historically; that made warnings and notes look like
/// errors.
fn color_for_severity(s: Severity) -> Color {
    match s {
        Severity::Error => Color::Red,
        Severity::Warning => Color::Yellow,
        Severity::Note => Color::Blue,
    }
}

fn diag_code_str(code: DiagCode) -> &'static str {
    match code {
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
        DiagCode::TooManyDiagnostics => "E0101",
        DiagCode::InvalidCharacter => "E0102",
        DiagCode::UnknownEscape => "E0103",
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
    input: &str,
    source_name: &str,
) -> Result<(), CompilerError> {
    let has_errors = diags.iter().any(|d| d.severity == Severity::Error);
    if !diags.is_empty() {
        render_diags(&diags, input, source_name);
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
                | DiagCode::TooManyDiagnostics
                | DiagCode::InvalidCharacter
                | DiagCode::UnknownEscape
                | DiagCode::ConstEvalFailure
                | DiagCode::CycleInComptime
                | DiagCode::GenericInstantiation => {}
            }
        }
    }

    #[test]
    fn byte_to_char_offsets_map_boundaries() {
        // Pure ASCII: byte offset == char offset.
        let ascii = byte_to_char_offsets("ab\ncd");
        assert_eq!(ascii, vec![0, 1, 2, 3, 4, 5]);

        // Multi-byte chars: byte offsets of later chars exceed their
        // char offsets (emoji are 4 bytes each).
        let emoji = "\u{1F600}x";
        let table = byte_to_char_offsets(emoji);
        assert_eq!(table.len(), emoji.len() + 1);
        assert_eq!(to_char_span(&table, 0..4), 0..1);
        assert_eq!(to_char_span(&table, 4..5), 1..2);
        assert_eq!(to_char_span(&table, 0..emoji.len()), 0..2);

        // Out-of-range byte offsets fall back to the char length
        // instead of panicking.
        assert_eq!(to_char_span(&table, 5..9), 2..2);
    }
}
