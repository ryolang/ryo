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

use ariadne::{Color, Label, Report, ReportKind, Source};
use chumsky::error::{Rich, RichPattern, RichReason};
use chumsky::span::Span as _;
use chumsky::{Parser, prelude::*};
use ryo_backend::codegen;
use ryo_backend::linker;
use ryo_backend::runtime_lib;
use ryo_core::ast;
use ryo_core::diag::{Diag, DiagCode, DiagSink, Severity};
use ryo_core::errors::CompilerError;
use ryo_core::tir::{self, Tir};
use ryo_core::types::InternPool;
use ryo_core::uir::Uir;
use ryo_frontend::astgen;
use ryo_frontend::lexer::{self, Token};
use ryo_frontend::parser::program_parser;
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
) -> Result<(ast::Program, Vec<Diag>), CompilerError> {
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
    let (program, errs) = program_parser().parse(token_stream).into_output_errors();
    for e in &errs {
        sink.emit(Diag::error(
            chumsky::span::SimpleSpan::new((), e.span().start..e.span().end),
            DiagCode::ParseError,
            rich_error_message(e, pool),
        ));
    }
    match program {
        Some(program) => Ok((program, sink.into_diags())),
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
/// instead of the opaque `<id#0>` handle.
fn rich_error_message(e: &Rich<'_, Token>, pool: &InternPool) -> String {
    match e.reason() {
        RichReason::Custom(msg) => msg.clone(),
        RichReason::ExpectedFound { .. } => {
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
                    expected.last().unwrap()
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
    let (truncation, regular): (Vec<&Diag>, Vec<&Diag>) = diags
        .iter()
        .partition(|d| d.code == DiagCode::TooManyDiagnostics);

    let mut sorted = regular;
    sorted.sort_by_key(|d| (d.span.start, d.span.end));
    for d in sorted {
        emit_one(d, source_name, &source);
    }
    for d in truncation {
        emit_one(d, source_name, &source);
    }
}

fn emit_one(d: &Diag, source_name: &str, source: &Source<&str>) {
    let kind = match d.severity {
        Severity::Error => ReportKind::Error,
        Severity::Warning => ReportKind::Warning,
        Severity::Note => ReportKind::Advice,
    };
    let label_color = color_for_severity(d.severity);
    let code = diag_code_str(d.code);
    // The full message goes in the report header only; the label
    // carries no text so the message isn't printed twice.
    let mut report = Report::build(kind, (source_name, d.span.start..d.span.end))
        .with_code(code)
        .with_message(&d.message)
        .with_label(Label::new((source_name, d.span.start..d.span.end)).with_color(label_color));
    for note in &d.notes {
        if let Some(span) = note.span {
            report = report.with_label(
                Label::new((source_name, span.start..span.end))
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
        DiagCode::UseAfterMove => "E0020",
        DiagCode::MoveOutOfBorrowedParam => "E0021",
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
        DiagCode::ParseError => "E0100",
        DiagCode::TooManyDiagnostics => "E0101",
        DiagCode::InvalidCharacter => "E0102",
        DiagCode::UnknownEscape => "E0103",
        DiagCode::ConstEvalFailure => "E0200",
        DiagCode::CycleInComptime => "E0201",
        DiagCode::GenericInstantiation => "E0202",
    }
}

fn display_ast(program: &ast::Program, pool: &InternPool) {
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

    let want = EmitSet::from_args(emit);
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
    let tirs = sema::analyze(&uir, &mut pool, &mut sink, &input, file);
    let sidecar = ryo_frontend::ownership::check(&tirs, &pool, &mut sink);

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
/// is intentionally discarded.
#[derive(Debug, Clone, Copy, Default)]
struct EmitSet {
    ast: bool,
    uir: bool,
    tir: bool,
    clif: bool,
}

impl EmitSet {
    fn from_args(emit: &[EmitKind]) -> Self {
        if emit.is_empty() {
            // Legacy default: AST + Cranelift IR. Anyone who wants
            // UIR / TIR opts in explicitly via `--emit=...`. We can
            // flip to "all four" once the docs advertise it.
            return EmitSet {
                ast: true,
                clif: true,
                ..Default::default()
            };
        }
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

/// Run the front-end (astgen + sema) and return the typed TIR
/// per-function. Used by `run` and `build` (which require a clean
/// front-end before codegen). `ryo ir` does its own staging so it
/// can print partial UIR / TIR after a failure.
fn lower_and_analyze(
    program: &ast::Program,
    pool: &mut InternPool,
    input: &str,
    source_name: &str,
    file_path: &Path,
    parse_diags: Vec<Diag>,
) -> Result<(Vec<Tir>, ryo_core::ownership::OwnershipSidecar), CompilerError> {
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
    let tirs = sema::analyze(&uir, pool, &mut sink, input, file_path);
    let sidecar = ryo_frontend::ownership::check(&tirs, pool, &mut sink);
    // Single tail block: render-if-non-empty, Err iff any errors.
    // Same shape as `ir_command` so warnings (`W0001` DeadStore,
    // `W0002` RedundantMove, …) surface on the success path
    // without a separate render block that could drift from the
    // error path.
    finalize_diags(sink.into_diags(), input, source_name)?;
    Ok((tirs, sidecar))
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

pub fn run_file(file: &Path) -> Result<(), CompilerError> {
    let input = read_source_file(file)?;
    let mut pool = InternPool::new();
    let name = source_name(file);
    let (program, parse_diags) = parse_source(&input, &mut pool, &name)?;

    println!("[Input Source]");
    println!("{}", input);
    println!();
    display_ast(&program, &pool);
    println!();

    let (tirs, sidecar) = lower_and_analyze(&program, &mut pool, &input, &name, file, parse_diags)?;

    println!("[Codegen]");
    let mut codegen = codegen::Codegen::new_jit().map_err(CompilerError::CodegenError)?;
    let main_id = codegen
        .compile(&tirs, &pool, &sidecar)
        .map_err(CompilerError::CodegenError)?;
    let result = codegen
        .execute(main_id)
        .map_err(CompilerError::ExecutionError)?;

    display_result(result);

    Ok(())
}

pub fn build_file(file: &Path) -> Result<(), CompilerError> {
    let input = read_source_file(file)?;
    let mut pool = InternPool::new();
    let name = source_name(file);
    let (program, parse_diags) = parse_source(&input, &mut pool, &name)?;
    let (tirs, sidecar) = lower_and_analyze(&program, &mut pool, &input, &name, file, parse_diags)?;

    let (obj_filename, exe_filename) = get_output_filenames(file);

    println!("[Codegen]");
    let target = Triple::host();
    let mut codegen = codegen::Codegen::new_aot(target).map_err(CompilerError::CodegenError)?;
    codegen
        .compile(&tirs, &pool, &sidecar)
        .map_err(CompilerError::CodegenError)?;
    let obj_bytes = codegen.finish().map_err(CompilerError::CodegenError)?;

    fs::write(&obj_filename, obj_bytes).map_err(CompilerError::from)?;
    println!("Generated object file: {}", obj_filename.display());

    // Extract embedded runtime archive and link
    let runtime_path = runtime_lib::extract_runtime_to_temp()
        .map_err(|e| CompilerError::LinkError(format!("Failed to extract runtime: {e}")))?;

    let link_result = linker::link_executable(&obj_filename, &exe_filename, &runtime_path);

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

    println!("Built: {}", exe_filename.display());
    Ok(())
}

fn display_result(result: i32) {
    println!("[Result] => {}", result);
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
            (DiagCode::ParseError, "E0100"),
            (DiagCode::TooManyDiagnostics, "E0101"),
            (DiagCode::InvalidCharacter, "E0102"),
            (DiagCode::UnknownEscape, "E0103"),
            (DiagCode::ConstEvalFailure, "E0200"),
            (DiagCode::CycleInComptime, "E0201"),
            (DiagCode::GenericInstantiation, "E0202"),
            (DiagCode::DeadStore, "W0001"),
            (DiagCode::RedundantMove, "W0002"),
            (DiagCode::RedundantMaterialize, "W0003"),
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
                | DiagCode::UseAfterMove
                | DiagCode::MoveOutOfBorrowedParam
                | DiagCode::ReturnBorrowedValue
                | DiagCode::MoveWhileBorrowedInCall
                | DiagCode::BorrowMismatch
                | DiagCode::MutableAliasingViolation
                | DiagCode::DeadStore
                | DiagCode::ViewEscape
                | DiagCode::SourceProjected
                | DiagCode::MissingReturn
                | DiagCode::DivisionByZero
                | DiagCode::CycleInResolution
                | DiagCode::ParseError
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
        assert_eq!(program.statements.len(), 3);
        assert!(matches!(
            program.statements[0].kind,
            ast::StmtKind::VarDecl(_)
        ));
        assert!(matches!(program.statements[1].kind, ast::StmtKind::Error));
        assert!(matches!(
            program.statements[2].kind,
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
