use crate::ast;
use crate::ast_lower;
use crate::codegen;
use crate::diag::{Diag, DiagCode, DiagSink, Severity};
use crate::errors::CompilerError;
use crate::hir;
use crate::indent;
use crate::lexer::Token;
use crate::linker;
use crate::parser::program_parser;
use crate::sema;
use crate::types::InternPool;
use ariadne::{Color, Label, Report, ReportKind, Source};
use chumsky::span::Span as _;
use chumsky::{Parser, input::Stream, prelude::*};
use logos::Logos;
use std::fs;
use std::path::Path;
use target_lexicon::Triple;

// Helper function to generate output filenames
fn get_output_filenames(input_file: &Path) -> (String, String) {
    let stem = input_file
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("output");

    let obj_filename = format!("{}.{}", stem, if cfg!(windows) { "obj" } else { "o" });
    let exe_filename = format!("{}{}", stem, std::env::consts::EXE_SUFFIX);

    (obj_filename, exe_filename)
}

pub(crate) fn lex_command(file: &Path) -> Result<(), CompilerError> {
    let input = read_source_file(file)?;
    display_tokens(&input, file);
    Ok(())
}

fn display_tokens(input: &str, file: &Path) {
    let token_iter = Token::lexer(input).spanned();

    println!("Token stream for '{}':", file.display());
    println!();

    for (result, span) in token_iter {
        match result {
            Ok(token) => {
                println!("{:?} @ {}..{}", token, span.start, span.end);
            }
            Err(()) => {
                println!("Error @ {}..{}", span.start, span.end);
            }
        }
    }
}

pub(crate) fn parse_command(file: &Path) -> Result<(), CompilerError> {
    let input = read_source_file(file)?;
    let program = parse_source(&input, source_name(file))?;
    display_ast(&program);
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

fn parse_source(input: &str, source_name: String) -> Result<ast::Program, CompilerError> {
    let raw_tokens: Vec<_> = Token::lexer(input)
        .spanned()
        .map(|(tok, span)| match tok {
            Ok(tok) => (tok, span.into()),
            Err(()) => (Token::Error, span.into()),
        })
        .collect();

    // TODO: have `indent::process` return a span/offset for the
    // offending newline so we can point Ariadne at the exact line.
    // For now, anchor on the last raw token's span (which is at
    // least *near* where the indent went wrong) and fall back to
    // 0..0 only for the empty-input case.
    let fallback_span = raw_tokens
        .last()
        .map(|(_, s)| *s)
        .unwrap_or_else(|| chumsky::span::SimpleSpan::new((), 0..0));
    let tokens = indent::process(raw_tokens).map_err(|e| {
        let diag = Diag::error(fallback_span, DiagCode::ParseError, e.to_string());
        render_diags(std::slice::from_ref(&diag), input, &source_name);
        CompilerError::Diagnostics(vec![diag])
    })?;

    let token_stream =
        Stream::from_iter(tokens).map((0..input.len()).into(), |(t, s): (_, _)| (t, s));

    match program_parser().parse(token_stream).into_result() {
        Ok(program) => Ok(program),
        Err(errs) => {
            let diags: Vec<Diag> = errs
                .iter()
                .map(|e| {
                    Diag::error(
                        chumsky::span::SimpleSpan::new((), e.span().start..e.span().end),
                        DiagCode::ParseError,
                        e.reason().to_string(),
                    )
                })
                .collect();
            render_diags(&diags, input, &source_name);
            Err(CompilerError::Diagnostics(diags))
        }
    }
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
    let mut report = Report::build(kind, (source_name, d.span.start..d.span.end))
        .with_code(code)
        .with_message(&d.message)
        .with_label(
            Label::new((source_name, d.span.start..d.span.end))
                .with_message(&d.message)
                .with_color(label_color),
        );
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
    report
        .finish()
        .eprint((source_name, source))
        .expect("diag render");
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
        DiagCode::UndefinedVariable => "E0010",
        DiagCode::UndefinedFunction => "E0011",
        DiagCode::TypeMismatch => "E0012",
        DiagCode::ArityMismatch => "E0013",
        DiagCode::BuiltinArgKind => "E0014",
        DiagCode::UnsupportedOperator => "E0015",
        DiagCode::ParseError => "E0100",
        DiagCode::TooManyDiagnostics => "E0101",
        DiagCode::ConstEvalFailure => "E0200",
        DiagCode::CycleInComptime => "E0201",
        DiagCode::GenericInstantiation => "E0202",
    }
}

fn display_ast(program: &ast::Program) {
    println!("[AST]");
    program.pretty_print();
}

pub(crate) fn ir_command(file: &Path) -> Result<(), CompilerError> {
    let input = read_source_file(file)?;
    let name = source_name(file);
    let program = parse_source(&input, name.clone())?;

    display_ast(&program);
    println!();

    let (hir, pool) = lower_and_analyze(&program, &input, &name)?;
    generate_and_display_ir(&hir, &pool)?;

    Ok(())
}

/// Run the front-end (ast_lower + sema) and return a fully typed HIR
/// alongside its `InternPool`. Centralized so the three driver
/// commands (`ir`, `run`, `build`) stay in lockstep when future
/// pre-codegen passes are added.
fn lower_and_analyze(
    program: &ast::Program,
    input: &str,
    source_name: &str,
) -> Result<(hir::HirProgram, InternPool), CompilerError> {
    let mut pool = InternPool::new();
    let mut sink = DiagSink::new();
    let mut hir = ast_lower::lower(program, &mut pool, &mut sink);
    // Run sema even if ast_lower emitted errors: the Error sentinel
    // keeps cascades in check, and surfacing every problem in one
    // run is the whole point of the structured-diagnostics phase.
    sema::analyze(&mut hir, &mut pool, &mut sink);
    if sink.has_errors() {
        let diags = sink.into_diags();
        render_diags(&diags, input, source_name);
        return Err(CompilerError::Diagnostics(diags));
    }
    Ok((hir, pool))
}

fn generate_and_display_ir(hir: &hir::HirProgram, pool: &InternPool) -> Result<(), CompilerError> {
    let target = Triple::host();
    let mut codegen = codegen::Codegen::new_aot(target).map_err(CompilerError::CodegenError)?;
    let ir = codegen
        .compile_and_dump_ir(hir, pool)
        .map_err(CompilerError::CodegenError)?;

    println!("[Cranelift IR]");
    print!("{}", ir);

    Ok(())
}

pub(crate) fn run_file(file: &Path) -> Result<(), CompilerError> {
    let input = read_source_file(file)?;
    let name = source_name(file);
    let program = parse_source(&input, name.clone())?;

    println!("[Input Source]");
    println!("{}", input);
    println!();
    display_ast(&program);
    println!();

    let (hir, pool) = lower_and_analyze(&program, &input, &name)?;

    println!("[Codegen]");
    let mut codegen = codegen::Codegen::new_jit().map_err(CompilerError::CodegenError)?;
    let main_id = codegen
        .compile(&hir, &pool)
        .map_err(CompilerError::CodegenError)?;
    let result = codegen
        .execute(main_id)
        .map_err(CompilerError::ExecutionError)?;

    display_result(result);

    Ok(())
}

pub(crate) fn build_file(file: &Path) -> Result<(), CompilerError> {
    let input = read_source_file(file)?;
    let name = source_name(file);
    let program = parse_source(&input, name.clone())?;
    let (hir, pool) = lower_and_analyze(&program, &input, &name)?;

    let (obj_filename, exe_filename) = get_output_filenames(file);

    println!("[Codegen]");
    let target = Triple::host();
    let mut codegen = codegen::Codegen::new_aot(target).map_err(CompilerError::CodegenError)?;
    codegen
        .compile(&hir, &pool)
        .map_err(CompilerError::CodegenError)?;
    let obj_bytes = codegen.finish().map_err(CompilerError::CodegenError)?;

    fs::write(&obj_filename, obj_bytes).map_err(CompilerError::from)?;
    println!("Generated object file: {}", obj_filename);

    linker::link_executable(&obj_filename, &exe_filename)?;
    let _ = fs::remove_file(&obj_filename);

    println!("Built: {}", exe_filename);
    Ok(())
}

fn display_result(result: i32) {
    println!("[Result] => {}", result);
}
