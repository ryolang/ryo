use crate::parse::parse_source_with_source;
use ryo_core::ast::StmtKind;
use ryo_core::compilation::CompilationContext;
use ryo_core::source::SourceId;
use ryo_core::span::Span;
use ryo_core::types::InternPool;
use std::path::PathBuf;

#[test]
fn parser_and_recovered_ast_nodes_use_registered_source() {
    let input = "fn main():\n\tx = 1\n\ty = = 2\n\tz = 3\n";
    let mut context = CompilationContext::new();
    let id = context.register_source(PathBuf::from("parser.ryo"), input.into());
    let source = context.sources().get(id).unwrap();
    let mut pool = InternPool::new();

    let (program, diags) = parse_source_with_source(source, &mut pool).expect("parse recovers");

    assert_eq!(program.span().source, id);
    assert!(diags.iter().all(|diag| diag.span.source == id));
    let function = match &program.stmt(program.top_level_stmts()[0]).kind {
        StmtKind::FunctionDef(function) => function,
        other => panic!("expected function, got {other:?}"),
    };
    let mut saw_error = false;
    for &stmt_id in program.stmt_list(function.body) {
        let stmt = program.stmt(stmt_id);
        assert_eq!(stmt.span.source, id);
        assert_ne!(stmt.span.source, SourceId::NONE);
        match stmt.kind {
            StmtKind::VarDecl(decl) => {
                assert_eq!(program.expr_span(decl.initializer).source, id);
            }
            StmtKind::Error => saw_error = true,
            _ => {}
        }
    }
    assert!(saw_error);
}

#[test]
fn statement_less_programs_retain_source_with_empty_offsets() {
    for input in ["", "\n\n", "# comment\n", "# comment"] {
        let mut context = CompilationContext::new();
        let id = context.register_source(PathBuf::from("empty.ryo"), input.into());
        let source = context.sources().get(id).unwrap();
        let mut pool = InternPool::new();

        let (program, diags) = parse_source_with_source(source, &mut pool).expect("empty parse");
        assert!(diags.is_empty(), "{input:?}: {diags:?}");
        assert!(program.top_level_stmts().is_empty(), "{input:?}");
        assert_eq!(program.span(), Span::in_source(id, 0, 0), "{input:?}");
    }
}
