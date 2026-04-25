use crate::ast;
use crate::astgen;
use crate::codegen;
use crate::diag::{Diag, DiagCode, DiagSink, Severity};
use crate::errors::CompilerError;
use crate::hir;
use crate::lexer::{self, Token};
use crate::linker;
use crate::parser::program_parser;
use crate::sema;
use crate::types::InternPool;
use ariadne::{Color, Label, Report, ReportKind, Source};
use chumsky::span::Span as _;
use chumsky::{Parser, input::Stream, prelude::*};
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
    display_tokens(&input, file)
}

fn display_tokens(input: &str, file: &Path) -> Result<(), CompilerError> {
    let mut pool = InternPool::new();
    let tokens = lexer::lex(input, &mut pool).map_err(|e| {
        // Route lex failures through the same Diag pipeline as
        // parse / sema errors so `ryo lex` matches the rest of the
        // CLI's exit-code and rendering behaviour. Previously this
        // path silently `eprintln!`d and returned `Ok(())`, which
        // hid lex errors from CI.
        let diag = Diag::error(e.span, DiagCode::ParseError, e.message);
        let name = source_name(file);
        render_diags(std::slice::from_ref(&diag), input, &name);
        CompilerError::Diagnostics(vec![diag])
    })?;

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

pub(crate) fn parse_command(file: &Path) -> Result<(), CompilerError> {
    let input = read_source_file(file)?;
    let mut pool = InternPool::new();
    let name = source_name(file);
    let program = parse_source(&input, &mut pool, &name)?;
    display_ast(&program, &pool);
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
) -> Result<ast::Program, CompilerError> {
    // Phase 2's `lexer::lex` runs logos + indent processing + string
    // and integer interning in a single pass and returns either the
    // typed `Token` stream or a `LexError` carrying a span pointing
    // at the offending byte range. Phase 1 wraps that as a single
    // structured `Diag` so the same Ariadne renderer handles lex,
    // parse, and middle-end diagnostics.
    let tokens = lexer::lex(input, pool).map_err(|e| {
        let diag = Diag::error(e.span, DiagCode::ParseError, e.message);
        render_diags(std::slice::from_ref(&diag), input, source_name);
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
            render_diags(&diags, input, source_name);
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

fn display_ast(program: &ast::Program, pool: &InternPool) {
    println!("[AST]");
    program.pretty_print(pool);
}

pub(crate) fn ir_command(file: &Path) -> Result<(), CompilerError> {
    let input = read_source_file(file)?;
    let mut pool = InternPool::new();
    let name = source_name(file);
    let program = parse_source(&input, &mut pool, &name)?;

    display_ast(&program, &pool);
    println!();

    let hir = lower_and_analyze(&program, &mut pool, &input, &name)?;
    generate_and_display_ir(&hir, &pool)?;

    Ok(())
}

/// Run the front-end (ast_lower + sema) and return a fully typed HIR
/// alongside its `InternPool`. Centralized so the three driver
/// commands (`ir`, `run`, `build`) stay in lockstep when future
/// pre-codegen passes are added.
fn lower_and_analyze(
    program: &ast::Program,
    pool: &mut InternPool,
    input: &str,
    source_name: &str,
) -> Result<hir::HirProgram, CompilerError> {
    let mut sink = DiagSink::new();
    // Phase 3 commit 3: astgen emits UIR; sema consumes UIR and
    // returns a per-instruction `TypeTable`. Codegen still drives
    // HIR, so the typed shim `uir_to_hir_typed` reconstructs an
    // equivalent typed tree from `(uir, types)`. Both the shim and
    // the HIR layer disappear in commit 4 (codegen-on-UIR) and
    // commit 5 (delete `hir.rs`).
    let uir = astgen::generate(program, pool, &mut sink);
    // Run sema even if astgen emitted errors: the Error sentinel
    // keeps cascades in check, and surfacing every problem in one
    // run is the whole point of the structured-diagnostics phase.
    let types = sema::analyze(&uir, pool, &mut sink);
    if sink.has_errors() {
        let diags = sink.into_diags();
        render_diags(&diags, input, source_name);
        return Err(CompilerError::Diagnostics(diags));
    }
    Ok(astgen::uir_to_hir_typed(&uir, &types))
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
    let mut pool = InternPool::new();
    let name = source_name(file);
    let program = parse_source(&input, &mut pool, &name)?;

    println!("[Input Source]");
    println!("{}", input);
    println!();
    display_ast(&program, &pool);
    println!();

    let hir = lower_and_analyze(&program, &mut pool, &input, &name)?;

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
    let mut pool = InternPool::new();
    let name = source_name(file);
    let program = parse_source(&input, &mut pool, &name)?;
    let hir = lower_and_analyze(&program, &mut pool, &input, &name)?;

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
