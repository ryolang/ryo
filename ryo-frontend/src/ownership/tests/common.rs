use super::super::*;

/// Build an all-`Borrow` modes slice matching the length of an
/// argument list. The ownership pass reads `view.modes` directly,
/// so test/builtin call sites pass all-`Borrow` to avoid
/// accidentally moving arguments.
pub(super) fn all_borrow(args: &[TirRef]) -> Vec<ryo_core::tir::ParamMode> {
    vec![ryo_core::tir::ParamMode::Borrow; args.len()]
}

/// Positional replacement for the old name-keyed sidecar lookup:
/// find `name`'s index in `tirs` and take that entry.
/// Take the sidecar entry at `index`, positional with the `tirs`
/// slice handed to `check` — the same contract codegen relies on.
/// (Every test below checks a single function, so `index` is 0.)
pub(super) fn take_function_sidecar(
    sidecar: &mut OwnershipSidecar,
    index: usize,
) -> FunctionSidecar {
    let name = sidecar.functions[index].name;
    std::mem::replace(&mut sidecar.functions[index], FunctionSidecar::new(name))
}

/// Lex + parse + astgen + sema + ownership on a source snippet —
/// the full front-end, so the case-B tests read like the programs
/// users write. Returns every diagnostic from all four stages.
pub(super) fn check_src(input: &str) -> Vec<Diag> {
    use chumsky::Parser as _;
    use chumsky::input::Input as _;
    let mut pool = InternPool::new();
    let mut lex_sink = DiagSink::new();
    let tokens = crate::lexer::lex(input, &mut pool, &mut lex_sink);
    assert!(
        !lex_sink.has_errors(),
        "lex errors: {:?}",
        lex_sink.into_diags()
    );
    let token_stream = tokens[..].split_token_span((0..input.len()).into());
    let mut ast = ryo_core::ast::Ast::new();
    crate::parser::program_parser()
        .parse_with_state(token_stream, &mut ast)
        .into_result()
        .expect("parse ok");
    let mut sink = DiagSink::new();
    let uir = crate::astgen::generate(&ast, &mut pool, &mut sink);
    let tirs = crate::sema::analyze(
        &uir,
        &mut pool,
        &mut sink,
        input,
        std::path::Path::new("<test>"),
    );
    check(&tirs, &pool, &mut sink);
    sink.into_diags()
}

pub(super) fn w0003_count(diags: &[Diag]) -> usize {
    diags
        .iter()
        .filter(|d| d.code == DiagCode::RedundantMaterialize)
        .count()
}
