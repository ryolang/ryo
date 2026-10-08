#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum EmitKind {
    /// Pretty-printed AST (parser output).
    Ast,
    /// Untyped IR (astgen output, Zig-style ZIR analogue).
    Uir,
    /// Typed IR (sema output, Zig-style AIR analogue).
    Tir,
    /// Cranelift IR (codegen output).
    Clif,
}

/// Libc to link a produced Linux binary against (the `ryo build
/// --link` flag). Accepted on every host; on non-Linux hosts it has no
/// effect because zig cc already links natively there.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum LinkMode {
    /// Static musl: the binary runs on any Linux regardless of host
    /// glibc version (the default).
    #[default]
    Musl,
    /// Link natively against the host glibc. Binaries are only as
    /// portable as the build host's glibc, but they get the host's
    /// full NSS/getaddrinfo stack.
    Glibc,
}

impl From<LinkMode> for linker::LinkMode {
    fn from(mode: LinkMode) -> Self {
        match mode {
            LinkMode::Musl => linker::LinkMode::Musl,
            LinkMode::Glibc => linker::LinkMode::Glibc,
        }
    }
}

use ariadne::{Color, Label, Report, ReportKind, Source};
use chumsky::error::{Rich, RichPattern, RichReason};
use chumsky::span::Span as _;
use chumsky::{Parser, prelude::*};
use ryo_backend::codegen;
use ryo_backend::linker;
use ryo_backend::runtime_lib;
use ryo_core::ast;
use ryo_core::diag::{Diag, DiagCode, DiagSink, ParseDiag, Severity};
use ryo_core::errors::CompilerError;
use ryo_core::tir::{self, Tir};
use ryo_core::types::InternPool;
use ryo_core::uir::Uir;
use ryo_frontend::astgen;
use ryo_frontend::lexer::{self, Token};
use ryo_frontend::parser::{ParseState, program_parser};
use ryo_frontend::sema;
use std::fs;
use std::path::{Path, PathBuf};
use target_lexicon::Triple;

// Helper function to generate output filenames. Artifacts land next
// to the source file, not in the CWD, so two same-stem sources in
// different directories built from one CWD don't clobber each other.
// Paths are built by extension replacement (never through `str`), so
// non-UTF-8 source paths survive through object writing and linking.
fn get_output_filenames(input_file: &Path) -> (PathBuf, PathBuf) {
    let obj_filename = input_file.with_extension(if cfg!(windows) { "obj" } else { "o" });
    // EXE_SUFFIX is ".exe" on Windows and "" elsewhere; with_extension
    // takes the bare extension ("" clears it, leaving the stem).
    let exe_filename =
        input_file.with_extension(std::env::consts::EXE_SUFFIX.trim_start_matches('.'));

    (obj_filename, exe_filename)
}

pub fn lex_command(file: &Path) -> Result<(), CompilerError> {
    let input = read_source_file(file)?;
    display_tokens(&input, file)
}

fn display_tokens(input: &str, file: &Path) -> Result<(), CompilerError> {
    let mut pool = InternPool::new();
    let name = source_name(file);
    let mut sink = DiagSink::new();
    let tokens = lexer::lex(input, &mut pool, &mut sink);
    if !sink.is_empty() {
        // Route lex diagnostics through the same Diag pipeline as
        // parse / sema errors so `ryo lex` matches the rest of the
        // CLI's exit-code and rendering behaviour. Previously this
        // path silently `eprintln!`d and returned `Ok(())`, which
        // hid lex errors from CI.
        return finalize_diags(sink.into_diags(), input, &name);
    }

    println!("Token stream for '{}':", file.display());
    println!();

    // Render identifier and string-literal payloads through the
    // pool so the user sees the actual text rather than an opaque
    // handle id. Other variants format normally via Debug.
    for (tok, span) in &tokens {
        match tok {
            Token::Ident(id) => {
                println!("Ident({:?}) @ {}..{}", pool.str(*id), span.start, span.end)
            }
            Token::StrLit(id) => {
                println!("StrLit({:?}) @ {}..{}", pool.str(*id), span.start, span.end)
            }
            other => println!("{:?} @ {}..{}", other, span.start, span.end),
        }
    }
    Ok(())
}

pub fn parse_command(file: &Path) -> Result<(), CompilerError> {
    let input = read_source_file(file)?;
    let mut pool = InternPool::new();
    let name = source_name(file);
    let (program, diags) = parse_source(&input, &mut pool, &name)?;
    display_ast(&program, &pool);
    finalize_diags(diags, &input, &name)?;
    Ok(())
}

/// Resolve the user-facing source name for diagnostics.
fn source_name(file: &Path) -> String {
    file.to_str()
        .map(str::to_string)
        .unwrap_or_else(|| file.display().to_string())
}

fn read_source_file(file: &Path) -> Result<String, CompilerError> {
    fs::read_to_string(file).map_err(CompilerError::from)
}

fn parse_source(
    input: &str,
    pool: &mut InternPool,
    source_name: &str,
) -> Result<(ast::Ast, Vec<Diag>), CompilerError> {
    // `lexer::lex` runs logos + indent processing + string and
    // integer interning in a single pass and never fails hard: it
    // emits structured `Diag`s into the sink and recovers so the
    // parser still sees a well-formed stream. Lex and parse
    // diagnostics accumulate in the same sink so both can surface in
    // one run, rendered through the same Ariadne pipeline as the
    // middle-end diagnostics.
    let mut sink = DiagSink::new();
    let tokens = lexer::lex(input, pool, &mut sink);

    // Indentation failure: the lexer returned an empty stream because
    // without Indent/Dedent markers parsing is meaningless. Skip the
    // parser and report the lex diagnostics as-is.
    if tokens.is_empty() && sink.has_errors() {
        return Err(fail_with_diags(sink.into_diags(), input, source_name));
    }

    // chumsky 0.12 added `Input::split_token_span`, which collapses the previous
    // `Stream::from_iter(...).map(eoi, |(t, s)| (t, s))` boilerplate that we used to
    // pull `(Token, Span)` slices into a parser-friendly shape.
    let token_stream = tokens[..].split_token_span((0..input.len()).into());

    // R10: the parser recovers at statement boundaries, so a syntax
    // error yields a diagnostic AND a partial program (with `Error`
    // placeholder nodes). Callers thread the diagnostics into the
    // middle-end sink and keep analyzing, so one bad statement never
    // suppresses semantic diagnostics elsewhere in the file (R9).
    // The parser builds directly into the AST arenas, threaded as
    // chumsky parser state (the state also carries the intern pool
    // so positional field keys can mint their canonical names).
    let mut state = ParseState::new(std::mem::take(pool));
    let (out, errs) = program_parser()
        .parse_with_state(token_stream, &mut state)
        .into_output_errors();
    let (ast, parsed_pool) = state.into_parts();
    *pool = parsed_pool;
    for e in &errs {
        let span = chumsky::span::SimpleSpan::new((), e.span().start..e.span().end);
        match e.reason() {
            // Parser-emitted custom errors carry their own `DiagCode`.
            // Most are self-contained; `UnknownAttribute` carries
            // interned ids, so resolve its spelling through the pool
            // here (same pattern as `rich_error_message` below), and
            // `MisplacedAttribute` rides its mandated explanation as a
            // structured note on the headline message.
            RichReason::Custom(pd) => {
                let diag = Diag::error(span, pd.code(), pd.message(pool));
                let diag = match pd {
                    ParseDiag::MisplacedAttribute => diag.with_note(
                        None,
                        "attributes are only supported on struct and enum definitions",
                    ),
                    _ => diag,
                };
                sink.emit(diag);
            }
            RichReason::ExpectedFound { .. } => sink.emit(Diag::error(
                span,
                DiagCode::ParseError,
                rich_error_message(e, pool),
            )),
        }
    }
    match out {
        Some(()) => Ok((ast, sink.into_diags())),
        // Unrecoverable parse (no output even after recovery — e.g.
        // trailing garbage that swallows the end-of-input marker).
        None => Err(fail_with_diags(sink.into_diags(), input, source_name)),
    }
}

/// Render a token with the intern pool available: identifiers and
/// string literals show their actual text, everything else falls back
/// to the token's pool-free `Display` (which renders those payloads
/// as opaque `<id#N>` / `<str#N>` handles).
fn render_token_with_pool(tok: &Token, pool: &InternPool) -> String {
    match tok {
        Token::Ident(id) => pool.str(*id).to_string(),
        Token::StrLit(id) => format!("\"{}\"", pool.str(*id)),
        other => other.to_string(),
    }
}

/// Rebuild chumsky's `Rich` error message with identifier and
/// string-literal payloads resolved through the pool. Mirrors
/// chumsky's own phrasing ("found 'X' expected A, B, or C") so the
/// text stays familiar, but a parse error on `x = foo(` shows `foo`
/// instead of the opaque `<id#0>` handle. Only called for
/// `ExpectedFound` errors — `Custom` payloads carry their own code
/// and self-contained message, converted by the caller.
fn rich_error_message(e: &Rich<'_, Token, SimpleSpan, ParseDiag>, pool: &InternPool) -> String {
    match e.reason() {
        RichReason::Custom(_) => {
            unreachable!("custom parse errors are converted by the caller")
        }
        RichReason::ExpectedFound { .. } => {
            // Unclosed argument list: the parser stopped at a line break
            // while `)` was still a valid continuation. A token dump
            // ("found '<newline>' expected '.', '[', '*', …") sends the
            // user to the glossary instead of the paren — name the
            // problem and the two tokens that would continue the call.
            let at_line_break = matches!(e.found(), Some(Token::Newline | Token::Dedent));
            if at_line_break
                && e.expected().any(|p| match p {
                    RichPattern::Token(tok) => **tok == Token::RParen,
                    _ => false,
                })
            {
                return "unclosed argument list: expected ',' or ')' before the end of the line"
                    .to_string();
            }
            let found = match e.found() {
                Some(tok) => format!("found '{}'", render_token_with_pool(tok, pool)),
                None => "found end of input".to_string(),
            };
            let expected: Vec<String> = e
                .expected()
                .map(|p| match p {
                    RichPattern::Token(tok) => {
                        format!("'{}'", render_token_with_pool(tok, pool))
                    }
                    other => other.to_string(),
                })
                .collect();
            let expected_part = match expected.len() {
                0 => "something else".to_string(),
                1 => expected[0].clone(),
                _ => format!(
                    "{}, or {}",
                    expected[..expected.len() - 1].join(", "),
                    expected
                        .last()
                        .expect("this arm only runs when expected.len() >= 2")
                ),
            };
            format!("{} expected {}", found, expected_part)
        }
    }
}

/// Render + wrap for paths whose diagnostics are always error
/// severity (lex / parse failures), where `finalize_diags` therefore
/// always returns `Err`. Keeps the render-and-wrap shape in one
/// place while letting callers keep their own error type.
fn fail_with_diags(diags: Vec<Diag>, input: &str, source_name: &str) -> CompilerError {
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
        DiagCode::TooManyDiagnostics => "E0101",
        DiagCode::InvalidCharacter => "E0102",
        DiagCode::UnknownEscape => "E0103",
        DiagCode::ConstEvalFailure => "E0200",
        DiagCode::CycleInComptime => "E0201",
        DiagCode::GenericInstantiation => "E0202",
    }
}

fn display_ast(program: &ast::Ast, pool: &InternPool) {
    println!("[AST]");
    print!("{}", ryo_core::ast_pretty::render_program(program, pool));
}

/// Drive `ryo ir` with the requested set of IR sections.
///
/// `emit` is the user-supplied `--emit=<kind>[,<kind>...]` list.
/// Empty means "use the legacy default" (`Ast` + `Clif`) so
/// existing scripts that just call `ryo ir <file>` keep their
/// output.
///
/// Sections are normalized into pipeline order before printing
/// (AST → UIR → TIR → CLIF) so flag order is irrelevant. Stages
/// run only as far as the deepest requested section requires; an
/// `--emit=uir` invocation never reaches sema.
pub fn ir_command(file: &Path, emit: &[EmitKind]) -> Result<(), CompilerError> {
    let input = read_source_file(file)?;
    let name = source_name(file);
    let mut pool = InternPool::new();

    let want = if emit.is_empty() {
        // Default: all four sections, in pipeline order. (Until
        // 2026-09 this was AST + CLIF only, a leftover from when
        // those were the only two dumps.)
        EmitSet {
            ast: true,
            uir: true,
            tir: true,
            clif: true,
        }
    } else {
        EmitSet::from_args(emit)
    };
    let (program, parse_diags) = parse_source(&input, &mut pool, &name)?;

    if want.ast {
        display_ast(&program, &pool);
        println!();
    }

    // Lex/parse diagnostics accumulate with the middle-end's so a
    // recovered syntax error never hides UIR/TIR-stage problems.
    let mut sink = DiagSink::new();
    for d in parse_diags {
        sink.emit(d);
    }

    // UIR / TIR / CLIF gating. We always *run* astgen if any of
    // those is asked for; sema only if TIR or CLIF; codegen only
    // if CLIF. Each stage's print is independent.
    let need_uir = want.uir || want.tir || want.clif;
    if !need_uir {
        return finalize_diags(sink.into_diags(), &input, &name);
    }

    let uir = astgen::generate(&program, &mut pool, &mut sink);

    if want.uir {
        display_uir(&uir, &pool);
        println!();
    }

    if !(want.tir || want.clif) {
        // UIR-only run. Surface astgen diagnostics now, with a
        // non-zero exit if anything fired.
        return finalize_diags(sink.into_diags(), &input, &name);
    }

    // For TIR / CLIF we also run sema. Per the §4.5 design, sema
    // returns a well-formed TIR even with errors (Unreachable
    // slots), and `--emit=tir` deliberately prints that partial
    // TIR — the whole point of the flag is debugging sema.
    let (tirs, sema_warnings) = sema::analyze_buffered(&uir, &mut pool, &mut sink, &input, file);
    let sidecar = ryo_frontend::ownership::check(&tirs, &pool, &mut sink);
    // Sema warnings flush only after ownership has run: an ownership
    // error must suppress them exactly like a sema error would.
    if !sink.has_errors() {
        for d in sema_warnings {
            sink.emit(d);
        }
    }

    if want.tir {
        display_tir(&tirs, &pool);
        println!();
    }

    if want.clif {
        // Codegen asserts no Unreachable instructions. If sema
        // failed, surface the diagnostics and abort — we cannot
        // produce a meaningful CLIF dump from a broken TIR.
        if sink.has_errors() {
            return finalize_diags(sink.into_diags(), &input, &name);
        }
        generate_and_display_ir(&tirs, &pool, &sidecar)?;
    }

    // Tail block: drains the sink whether sema/ownership were
    // clean or only emitted warnings. Without this `ryo ir` would
    // silently swallow W0001/W0002 on success.
    finalize_diags(sink.into_diags(), &input, &name)
}

/// Resolve `--emit` flag values into a normalized set. Membership
/// is what governs printing; the source order on the command line
/// is intentionally discarded. An empty list selects nothing —
/// `ir_command` layers its legacy default (AST + CLIF) on top of
/// an empty list, while `run`/`build` default to silence.
#[derive(Debug, Clone, Copy, Default)]
struct EmitSet {
    ast: bool,
    uir: bool,
    tir: bool,
    clif: bool,
}

impl EmitSet {
    fn from_args(emit: &[EmitKind]) -> Self {
        let mut s = EmitSet::default();
        for k in emit {
            match k {
                EmitKind::Ast => s.ast = true,
                EmitKind::Uir => s.uir = true,
                EmitKind::Tir => s.tir = true,
                EmitKind::Clif => s.clif = true,
            }
        }
        s
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
fn finalize_diags(diags: Vec<Diag>, input: &str, source_name: &str) -> Result<(), CompilerError> {
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

fn display_uir(uir: &Uir, pool: &InternPool) {
    println!("[UIR]");
    print!("{}", uir.dump(pool));
}

fn display_tir(tirs: &[Tir], pool: &InternPool) {
    println!("[TIR]");
    print!("{}", tir::dump(tirs, pool));
}

/// Run the front-end (astgen + sema) and return the UIR plus the
/// typed TIR per-function with the ownership sidecar. Used by `run`
/// and `build` (which require a clean front-end before codegen); the
/// UIR comes back so `run --emit=uir` / `build --emit=uir` can dump
/// it. `ryo ir` does its own staging so it can print partial UIR /
/// TIR after a failure.
fn lower_and_analyze(
    program: &ast::Ast,
    pool: &mut InternPool,
    input: &str,
    source_name: &str,
    file_path: &Path,
    parse_diags: Vec<Diag>,
) -> Result<(Uir, Vec<Tir>, ryo_core::ownership::OwnershipSidecar), CompilerError> {
    let mut sink = DiagSink::new();
    // Lex/parse diagnostics come first so the final render preserves
    // pipeline order.
    for d in parse_diags {
        sink.emit(d);
    }
    let uir = astgen::generate(program, pool, &mut sink);
    // Run sema even if astgen emitted errors: the Error sentinel
    // keeps cascades in check, and surfacing every problem in one
    // run is the whole point of the structured-diagnostics phase.
    // Sema warnings are buffered and flushed after ownership so an
    // ownership error suppresses them exactly like a sema error.
    let (tirs, sema_warnings) = sema::analyze_buffered(&uir, pool, &mut sink, input, file_path);
    let sidecar = ryo_frontend::ownership::check(&tirs, pool, &mut sink);
    if !sink.has_errors() {
        for d in sema_warnings {
            sink.emit(d);
        }
    }
    // Single tail block: render-if-non-empty, Err iff any errors.
    // Same shape as `ir_command` so warnings (`W0001` DeadStore,
    // `W0002` RedundantMove, …) surface on the success path
    // without a separate render block that could drift from the
    // error path.
    finalize_diags(sink.into_diags(), input, source_name)?;
    Ok((uir, tirs, sidecar))
}

fn generate_and_display_ir(
    tirs: &[Tir],
    pool: &InternPool,
    sidecar: &ryo_core::ownership::OwnershipSidecar,
) -> Result<(), CompilerError> {
    let target = Triple::host();
    let mut codegen = codegen::Codegen::new_aot(target).map_err(CompilerError::CodegenError)?;
    let ir = codegen
        .compile_and_dump_ir(tirs, pool, sidecar)
        .map_err(CompilerError::CodegenError)?;

    println!("[Cranelift IR]");
    print!("{}", ir);

    Ok(())
}

/// JIT-compile and run `file`. The default output is exactly what
/// the compiled program writes to stdout/stderr — no compiler
/// banners — and the return value is the program's own exit code.
/// `--emit` additionally prints IR sections in pipeline order (AST
/// after parse; UIR/TIR after lowering; CLIF after codegen, before
/// execution), rendered identically to `ryo ir`. `program_args` are
/// forwarded to the program and published to the runtime's argv storage
/// by the entry shim before `main`'s first instruction; a synthetic
/// argv[0] — the source file path — is prepended so argc/argv match the
/// AOT binary's C-runtime table (argv[0] = invocation path, included in
/// the count). Callers with no program arguments pass `&[]`.
pub fn run_file(
    file: &Path,
    emit: &[EmitKind],
    program_args: &[String],
) -> Result<i32, CompilerError> {
    let input = read_source_file(file)?;
    let mut pool = InternPool::new();
    let name = source_name(file);
    let want = EmitSet::from_args(emit);
    let (program, parse_diags) = parse_source(&input, &mut pool, &name)?;

    if want.ast {
        display_ast(&program, &pool);
        println!();
    }

    let (uir, tirs, sidecar) =
        lower_and_analyze(&program, &mut pool, &input, &name, file, parse_diags)?;

    if want.uir {
        display_uir(&uir, &pool);
        println!();
    }
    if want.tir {
        display_tir(&tirs, &pool);
        println!();
    }

    let mut codegen = codegen::Codegen::new_jit().map_err(CompilerError::CodegenError)?;
    let (main_id, clif) = codegen
        .compile(&tirs, &pool, &sidecar, want.clif)
        .map_err(CompilerError::CodegenError)?;
    if want.clif {
        println!("[Cranelift IR]");
        print!("{clif}");
    }

    let mut argv = Vec::with_capacity(program_args.len() + 1);
    argv.push(file.to_string_lossy().into_owned());
    argv.extend_from_slice(program_args);
    codegen
        .execute(main_id, &argv)
        .map_err(CompilerError::ExecutionError)
}

/// AOT-compile `file` to a standalone binary next to the source.
/// Silent on success unless `--emit` requests IR sections (same
/// rendering and pipeline order as `ryo ir`; CLIF prints after
/// codegen, before linking). `link` selects the libc zig cc links
/// against on Linux (musl default); it is a no-op on other hosts.
pub fn build_file(file: &Path, emit: &[EmitKind], link: LinkMode) -> Result<(), CompilerError> {
    let input = read_source_file(file)?;
    let mut pool = InternPool::new();
    let name = source_name(file);
    let want = EmitSet::from_args(emit);
    let (program, parse_diags) = parse_source(&input, &mut pool, &name)?;

    if want.ast {
        display_ast(&program, &pool);
        println!();
    }

    let (uir, tirs, sidecar) =
        lower_and_analyze(&program, &mut pool, &input, &name, file, parse_diags)?;

    if want.uir {
        display_uir(&uir, &pool);
        println!();
    }
    if want.tir {
        display_tir(&tirs, &pool);
        println!();
    }

    let (obj_filename, exe_filename) = get_output_filenames(file);

    let target = Triple::host();
    let mut codegen = codegen::Codegen::new_aot(target).map_err(CompilerError::CodegenError)?;
    let (_main_id, clif) = codegen
        .compile(&tirs, &pool, &sidecar, want.clif)
        .map_err(CompilerError::CodegenError)?;
    if want.clif {
        println!("[Cranelift IR]");
        print!("{clif}");
    }
    let obj_bytes = codegen.finish().map_err(CompilerError::CodegenError)?;

    fs::write(&obj_filename, obj_bytes).map_err(CompilerError::from)?;

    // Extract embedded runtime archive and link
    let runtime_path = runtime_lib::extract_runtime_to_temp()
        .map_err(|e| CompilerError::LinkError(format!("Failed to extract runtime: {e}")))?;

    let link_result =
        linker::link_executable(&obj_filename, &exe_filename, &runtime_path, link.into());

    runtime_lib::cleanup_runtime_temp(&runtime_path);
    // Default: clean up the intermediate object file. Set
    // `RYO_KEEP_OBJ=1` to retain it — used by tooling that needs to
    // relink the same object with extra flags (e.g. the ASan smoke
    // tests in `tests/asan_smoke.rs` re-link with `-fsanitize=address`).
    // Runs on the link-failure path too, which previously leaked the
    // `.o` via the early `?`.
    if std::env::var_os("RYO_KEEP_OBJ").is_none() {
        let _ = fs::remove_file(&obj_filename);
    }
    link_result?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn output_filenames_land_next_to_source() {
        let obj_ext = if cfg!(windows) { "obj" } else { "o" };
        let exe_suffix = std::env::consts::EXE_SUFFIX;

        let (obj, exe) = get_output_filenames(Path::new("some/dir/hello.ryo"));
        assert_eq!(obj, PathBuf::from(format!("some/dir/hello.{obj_ext}")));
        assert_eq!(exe, PathBuf::from(format!("some/dir/hello{exe_suffix}")));

        // No directory component: output stays relative to the CWD,
        // exactly as before.
        let (obj, exe) = get_output_filenames(Path::new("hello.ryo"));
        assert_eq!(obj, PathBuf::from(format!("hello.{obj_ext}")));
        assert_eq!(exe, PathBuf::from(format!("hello{exe_suffix}")));
    }

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
                | DiagCode::ReservedIdentifier
                | DiagCode::ArityMismatch
                | DiagCode::BuiltinArgKind
                | DiagCode::UnsupportedOperator
                | DiagCode::VoidValueInExpression
                | DiagCode::ConditionNotBool
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

    #[test]
    fn parse_error_renders_identifiers_through_pool() {
        // The token's pool-free `Display` renders identifiers as
        // opaque `<id#N>` handles; the driver's parse-error path
        // must re-render them through the pool so the user sees the
        // actual identifier text.
        let mut pool = InternPool::new();
        let (_program, diags) = parse_source("x foo = 1", &mut pool, "<test>")
            .expect("recovery should yield a partial program");
        assert!(!diags.is_empty());
        let msg = &diags[0].message;
        assert!(
            msg.contains("foo"),
            "message should name the identifier text: {msg}"
        );
        assert!(
            !msg.contains("<id#"),
            "message must not leak opaque handle ids: {msg}"
        );
    }

    #[test]
    fn unclosed_call_gets_a_human_message() {
        let mut pool = InternPool::new();
        let (_program, diags) = parse_source(
            "fn main():\n\ttotal = \"hello\"\n\tprint(int_to_str(total.len())\n\tprint(\"done\")\n",
            &mut pool,
            "<test>",
        )
        .expect("recovery should yield a partial program");
        let e0100: Vec<_> = diags
            .iter()
            .filter(|d| d.code == DiagCode::ParseError)
            .collect();
        assert_eq!(e0100.len(), 1, "expected one parse error: {diags:?}");
        let msg = &e0100[0].message;
        assert!(
            msg.contains("unclosed") && msg.contains("')'"),
            "message should name the problem and the fix: {msg}"
        );
        assert!(
            !msg.contains("<newline>") && !msg.contains("something else"),
            "message must not leak parser internals: {msg}"
        );
    }

    #[test]
    fn top_level_assignment_gets_targeted_message() {
        let mut pool = InternPool::new();
        let (_program, diags) = parse_source(
            "struct Point:\n\tx: int\n\ty: int\n\nfn main():\n\tq = Point{x=1, y=2}\n\nq.x = 7\n",
            &mut pool,
            "<test>",
        )
        .expect("recovery should yield a partial program");
        assert_eq!(diags.len(), 1, "expected one diagnostic: {diags:?}");
        assert_eq!(diags[0].code, DiagCode::ParseError);
        assert!(
            diags[0].message.contains("inside a function body"),
            "message should explain the rule: {}",
            diags[0].message
        );
    }

    #[test]
    fn top_level_assignment_message_covers_positional_and_compound() {
        let mut pool = InternPool::new();
        let (_p, diags) = parse_source("q.0 = 7\n", &mut pool, "<test>")
            .expect("recovery should yield a partial program");
        assert_eq!(diags.len(), 1, "positional target: {diags:?}");
        assert!(
            diags
                .iter()
                .all(|d| d.message.contains("inside a function body")),
            "positional target: {diags:?}"
        );

        let mut pool = InternPool::new();
        let (_p, diags) = parse_source("q.x += 1\n", &mut pool, "<test>")
            .expect("recovery should yield a partial program");
        assert_eq!(diags.len(), 1, "compound assign: {diags:?}");
        assert!(
            diags
                .iter()
                .all(|d| d.message.contains("inside a function body")),
            "compound assign: {diags:?}"
        );
    }

    #[test]
    fn top_level_plain_decl_still_parses() {
        let mut pool = InternPool::new();
        parse_source("q = 7\nprint(q)\n", &mut pool, "<test>")
            .expect("top-level var decls and flat scripts stay valid");
    }

    #[test]
    fn parse_broken_only_return_does_not_cascade_missing_return() {
        // The parser recovers at the statement boundary, so the body
        // genuinely ends without a return — but that is the parse
        // error's fault, not the signature's. Exactly the parse
        // diagnostic may surface; a stacked E0036 would point the user
        // at the wrong place.
        let src = "fn f() -> int:\n\treturn 1 +\n";
        let mut pool = InternPool::new();
        let (program, parse_diags) = parse_source(src, &mut pool, "<test>")
            .expect("recovery should yield a partial program");
        let mut sink = DiagSink::new();
        for d in parse_diags {
            sink.emit(d);
        }
        let uir = astgen::generate(&program, &mut pool, &mut sink);
        let _tirs = sema::analyze(&uir, &mut pool, &mut sink, src, Path::new("<test>"));
        let diags = sink.into_diags();
        assert_eq!(
            diags.len(),
            1,
            "exactly the parse diagnostic may surface: {diags:?}"
        );
        assert_eq!(diags[0].code, DiagCode::ParseError);
    }

    #[test]
    fn sema_warnings_suppressed_by_ownership_errors() {
        // Cross-stage ordering hole: fn a's W0002 (redundant move on a
        // Copy param) is a sema-stage warning, and fn main's
        // use-after-move error only fires during the ownership pass —
        // after sema has already flushed. The deferred flush must drop
        // the warning: one failing stage silences warnings unit-wide.
        let src = "fn a(move x: int) -> int:\n\treturn x\n\nfn main():\n\ts = \"abc\"\n\tt = s\n\tprint(s)\n";
        let mut pool = InternPool::new();
        let (program, parse_diags) =
            parse_source(src, &mut pool, "<test>").expect("source should parse cleanly");
        assert!(parse_diags.is_empty());
        let mut sink = DiagSink::new();
        let uir = astgen::generate(&program, &mut pool, &mut sink);
        let (tirs, sema_warnings) =
            sema::analyze_buffered(&uir, &mut pool, &mut sink, src, Path::new("<test>"));
        let _sidecar = ryo_frontend::ownership::check(&tirs, &pool, &mut sink);
        if !sink.has_errors() {
            for d in sema_warnings {
                sink.emit(d);
            }
        }
        let diags = sink.into_diags();
        assert!(
            diags.iter().any(|d| d.code == DiagCode::UseAfterMove),
            "ownership error must survive: {diags:?}"
        );
        assert!(
            !diags.iter().any(|d| d.code == DiagCode::RedundantMove),
            "sema warning must not pile onto a later ownership error: {diags:?}"
        );
    }

    #[test]
    fn one_line_if_body_gets_targeted_message() {
        let mut pool = InternPool::new();
        let (_p, diags) = parse_source(
            "fn factorial(n: int) -> int:\n\tif (n <= 1): return 1\n\treturn n * factorial(n - 1)\n",
            &mut pool,
            "<test>",
        )
        .expect("recovery should yield a partial program");
        assert_eq!(diags.len(), 1, "expected one diagnostic: {diags:?}");
        assert_eq!(diags[0].code, DiagCode::ParseError);
        assert!(
            diags[0].message.contains("one-line") && diags[0].message.contains("own line"),
            "message should steer to the fix: {}",
            diags[0].message
        );
        assert!(
            !diags[0].message.contains("<indent>"),
            "message must not leak parser internals: {}",
            diags[0].message
        );
    }

    #[test]
    fn missing_colon_after_condition_gets_targeted_message() {
        let mut pool = InternPool::new();
        let (_p, diags) = parse_source(
            "fn f(n: int) -> int:\n\tif (n <= 1) return 1\n\treturn n\n",
            &mut pool,
            "<test>",
        )
        .expect("recovery should yield a partial program");
        assert!(
            diags
                .iter()
                .any(|d| d.message.contains("expected ':' after the condition")),
            "missing-colon shape should be named: {diags:?}"
        );
    }

    #[test]
    fn accepted_block_form_stays_silent() {
        let mut pool = InternPool::new();
        parse_source(
            "fn factorial(n: int) -> int:\n\tif (n <= 1):\n\t\treturn 1\n\treturn n * factorial(n - 1)\n\nfn main():\n\tprint(factorial(5))\n",
            &mut pool,
            "<test>",
        )
        .expect("newline + indented block must parse clean");
    }

    #[test]
    fn blank_line_between_header_and_body_is_not_a_one_line_body() {
        let mut pool = InternPool::new();
        parse_source(
            "fn f(n: int) -> int:\n\tif (n <= 1):\n\n\t\treturn 1\n\treturn n\n",
            &mut pool,
            "<test>",
        )
        .expect("blank-line tolerance must survive");
    }

    #[test]
    fn one_line_body_message_covers_all_block_headers() {
        // Same Python-transcription hazard on every header — the
        // shared helper must fire for each, with the keyword
        // interpolated.
        for (src, kw) in [
            (
                "fn f(n: int) -> int:\n\twhile n > 0: n -= 1\n\treturn n\n",
                "while",
            ),
            (
                "fn f(n: int) -> int:\n\tfor i in range(0, 2): print(i)\n\treturn n\n",
                "for",
            ),
        ] {
            let mut pool = InternPool::new();
            let (_p, diags) = parse_source(src, &mut pool, "<test>")
                .expect("recovery should yield a partial program");
            assert!(
                diags
                    .iter()
                    .any(|d| d.message.contains(&format!("one-line '{kw}'"))),
                "{kw}: targeted message missing: {diags:?}"
            );
        }
    }

    #[test]
    fn header_recovery_preserves_indented_structure() {
        // The line after a one-line-body error is deeper-indented.
        // The recovery swallow must stop at <indent> (the indent
        // preprocessor emits it before the newline) so the block
        // parser can claim the indented body — eating the <indent>
        // orphans its <dedent> and cascades extra parse errors.
        for src in [
            "fn f(n: int) -> int:\n\tif n <= 1: return 1\n\t\tprint(n)\n\treturn n\n",
            "fn f(n: int) -> int:\n\tif n <= 1 return 1\n\t\tprint(n)\n\treturn n\n",
        ] {
            let mut pool = InternPool::new();
            let (_p, diags) = parse_source(src, &mut pool, "<test>")
                .expect("recovery should yield a partial program");
            assert_eq!(diags.len(), 1, "expected one diagnostic: {diags:?}");
        }
    }

    #[test]
    fn unknown_attribute_diagnostic_names_the_attribute() {
        // The parser is pool-less, so it reports the unknown attribute
        // as interned ids; the driver must render the spelling (and
        // the known set) through the pool.
        let mut pool = InternPool::new();
        let (_program, diags) = parse_source(
            "#[derive(Bogus)] struct P:\n\tx: int\n",
            &mut pool,
            "<test>",
        )
        .expect("recovery should yield a partial program");
        assert_eq!(diags.len(), 1, "expected one diagnostic: {diags:?}");
        let diag = &diags[0];
        assert_eq!(diag.code, DiagCode::UnknownAttribute);
        assert!(
            diag.message.contains("derive(Bogus)"),
            "message should name the attribute: {}",
            diag.message
        );
        assert!(
            diag.message.contains("derive(Eq), repr(C)"),
            "message should list the known set: {}",
            diag.message
        );
    }

    #[test]
    fn misplaced_attribute_diagnostic_carries_the_note() {
        // The mandated explanation rides as a structured `DiagNote`
        // under the headline message, not as message text.
        let mut pool = InternPool::new();
        let (_program, diags) = parse_source(
            "#[derive(Eq)]\nfn f():\n\tpass_through = 1\n",
            &mut pool,
            "<test>",
        )
        .expect("recovery should yield a partial program");
        assert_eq!(diags.len(), 1, "expected one diagnostic: {diags:?}");
        let diag = &diags[0];
        assert_eq!(diag.code, DiagCode::UnknownAttribute);
        assert!(
            diag.message.contains("unexpected attribute"),
            "headline should name the misplaced attribute: {}",
            diag.message
        );
        assert!(
            diag.notes.iter().any(
                |n| n.message == "attributes are only supported on struct and enum definitions"
            ),
            "note should explain that attributes need a struct or enum definition: {:?}",
            diag.notes
        );
    }

    #[test]
    fn parse_source_recovers_and_returns_partial_program() {
        // R10: one syntax error must not discard the rest of the
        // file. The parser synchronizes at the next statement
        // boundary, reports the error, and yields a partial AST
        // with an `Error` placeholder node.
        let mut pool = InternPool::new();
        let (program, diags) = parse_source("x = 1\ny = = 2\nz = 3\n", &mut pool, "<test>")
            .expect("recovery should yield a partial program");
        assert_eq!(
            diags
                .iter()
                .filter(|d| d.code == DiagCode::ParseError)
                .count(),
            1,
            "expected exactly one parse diagnostic: {diags:?}"
        );
        let stmts = program.top_level_stmts();
        assert_eq!(stmts.len(), 3);
        assert!(matches!(
            program.stmt(stmts[0]).kind,
            ast::StmtKind::VarDecl(_)
        ));
        assert!(matches!(program.stmt(stmts[1]).kind, ast::StmtKind::Error));
        assert!(matches!(
            program.stmt(stmts[2]).kind,
            ast::StmtKind::VarDecl(_)
        ));
    }

    #[test]
    fn parse_and_sema_diagnostics_co_surface() {
        // R9 + R10: a syntax error in one statement must not suppress
        // semantic diagnostics elsewhere in the file — both surface
        // in a single run.
        let mut pool = InternPool::new();
        let input = "fn main():\n\tx = = 1\n\ty: int = \"hi\"\n";
        let (program, parse_diags) =
            parse_source(input, &mut pool, "<test>").expect("recovery should succeed");
        let err = lower_and_analyze(
            &program,
            &mut pool,
            input,
            "<test>",
            Path::new("<test>"),
            parse_diags,
        )
        .expect_err("both a parse error and a type error must fail the compile");
        let CompilerError::Diagnostics(diags) = err else {
            panic!("expected Diagnostics error");
        };
        assert!(
            diags.iter().any(|d| d.code == DiagCode::ParseError),
            "parse diagnostic must survive: {diags:?}"
        );
        assert!(
            diags.iter().any(|d| d.code == DiagCode::TypeMismatch),
            "sema diagnostic must co-surface: {diags:?}"
        );
    }
}
