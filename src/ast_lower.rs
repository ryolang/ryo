//! AST → HIR structural translation.
//!
//! This module performs pure structural translation from AST to HIR.
//! It does *not* build scopes, resolve identifier types, check arity,
//! or infer expression types. Every `HirExpr` produced here has
//! `ty = None`; `sema::analyze` fills them in.
//!
//! The only type work done here is resolving syntactic type
//! annotations on parameters, return types, and variable declarations
//! (e.g. `"int"` → `pool.int()`), because those come from `TypeExpr`
//! nodes and cannot meaningfully exist in a "no types yet" state.

use crate::ast;
use crate::diag::{Diag, DiagCode, DiagSink};
use crate::hir::*;
use crate::types::InternPool;
use chumsky::span::{SimpleSpan, Span as _};

fn synthetic_span() -> Span {
    SimpleSpan::new((), 0..0)
}

/// Lower an AST `Program` to HIR, accumulating diagnostics in `sink`.
///
/// Returns the lowered HIR even on error (using `pool.error_type()`
/// for any annotation that failed to resolve) so subsequent passes
/// can keep type-checking and surface their own diagnostics. The
/// driver decides whether to proceed based on `sink.has_errors()`.
pub fn lower(program: &ast::Program, pool: &mut InternPool, sink: &mut DiagSink) -> HirProgram {
    let mut func_defs: Vec<&ast::FunctionDef> = Vec::new();
    let mut top_level: Vec<&ast::Statement> = Vec::new();

    for stmt in &program.statements {
        match &stmt.kind {
            ast::StmtKind::FunctionDef(f) => func_defs.push(f),
            _ => top_level.push(stmt),
        }
    }

    let has_explicit_main = func_defs.iter().any(|f| f.name.name == "main");

    if has_explicit_main && !top_level.is_empty() {
        // Anchor the diagnostic on the first stray top-level stmt;
        // pointing at "the program" with a 0..0 span is useless in
        // a renderer.
        let span = top_level[0].span;
        sink.emit(Diag::error(
            span,
            DiagCode::TopLevelWithExplicitMain,
            "top-level statements are not allowed when fn main() is defined",
        ));
        // Fall through and lower anyway; sema can still report
        // problems inside `main`.
    }

    let mut functions = Vec::new();
    for func in &func_defs {
        functions.push(lower_function_def(func, pool, sink));
    }
    if !has_explicit_main {
        // Synthesize an implicit `main` from top-level statements.
        // User-defined helper functions still appear above; without
        // this, calls to them in top-level code would dangle as
        // "undefined function" errors in sema.
        functions.push(lower_implicit_main(&top_level, pool, sink));
    }

    HirProgram { functions }
}

fn resolve_type(name: &str, span: Span, pool: &InternPool, sink: &mut DiagSink) -> TypeId {
    match name {
        "int" => pool.int(),
        "str" => pool.str_(),
        "bool" => pool.bool_(),
        _ => {
            sink.emit(Diag::error(
                span,
                DiagCode::UnknownType,
                format!("unknown type: '{}'", name),
            ));
            pool.error_type()
        }
    }
}

fn lower_implicit_main(
    stmts: &[&ast::Statement],
    pool: &mut InternPool,
    sink: &mut DiagSink,
) -> HirFunction {
    let mut body = Vec::new();

    for stmt in stmts {
        lower_stmt(stmt, pool, sink, &mut body);
    }

    let int_ty = pool.int();
    body.push(HirStmt::Return(
        Some(HirExpr {
            kind: HirExprKind::IntLiteral(0),
            ty: None,
            span: synthetic_span(),
        }),
        synthetic_span(),
    ));

    HirFunction {
        name: "main".to_string(),
        params: vec![],
        return_type: int_ty,
        body,
    }
}

fn lower_function_def(
    func: &ast::FunctionDef,
    pool: &mut InternPool,
    sink: &mut DiagSink,
) -> HirFunction {
    let params: Vec<HirParam> = func
        .params
        .iter()
        .map(|p| HirParam {
            name: p.name.name.clone(),
            ty: resolve_type(&p.type_annotation.name, p.type_annotation.span, pool, sink),
        })
        .collect();

    let return_type = match &func.return_type {
        Some(ty) => resolve_type(&ty.name, ty.span, pool, sink),
        None => pool.void(),
    };

    let mut body = Vec::new();
    for stmt in &func.body {
        lower_stmt(stmt, pool, sink, &mut body);
    }

    HirFunction {
        name: func.name.name.clone(),
        params,
        return_type,
        body,
    }
}

fn lower_stmt(
    stmt: &ast::Statement,
    pool: &mut InternPool,
    sink: &mut DiagSink,
    out: &mut Vec<HirStmt>,
) {
    match &stmt.kind {
        ast::StmtKind::VarDecl(decl) => {
            let initializer = lower_expr(&decl.initializer);
            let ty = decl
                .type_annotation
                .as_ref()
                .map(|ann| resolve_type(&ann.name, ann.span, pool, sink));
            out.push(HirStmt::VarDecl {
                name: decl.name.name.clone(),
                mutable: decl.mutable,
                ty,
                initializer,
                span: stmt.span,
            });
        }
        ast::StmtKind::Return(Some(expr)) => {
            let hir_expr = lower_expr(expr);
            out.push(HirStmt::Return(Some(hir_expr), stmt.span));
        }
        ast::StmtKind::Return(None) => {
            out.push(HirStmt::Return(None, stmt.span));
        }
        ast::StmtKind::ExprStmt(expr) => {
            let hir_expr = lower_expr(expr);
            out.push(HirStmt::Expr(hir_expr, stmt.span));
        }
        ast::StmtKind::FunctionDef(_) => {
            sink.emit(Diag::error(
                stmt.span,
                DiagCode::NestedFunctionDef,
                "nested function definitions are not supported",
            ));
        }
    }
}

fn lower_expr(expr: &ast::Expression) -> HirExpr {
    let span = expr.span;
    let kind = match &expr.kind {
        ast::ExprKind::Literal(ast::Literal::Int(n)) => HirExprKind::IntLiteral(*n),
        ast::ExprKind::Literal(ast::Literal::Str(s)) => HirExprKind::StrLiteral(s.clone()),
        ast::ExprKind::Literal(ast::Literal::Bool(b)) => HirExprKind::BoolLiteral(*b),
        ast::ExprKind::Ident(name) => HirExprKind::Var(name.clone()),
        ast::ExprKind::BinaryOp(lhs, op, rhs) => {
            let lhs = lower_expr(lhs);
            let rhs = lower_expr(rhs);
            let hir_op = match op {
                ast::BinaryOperator::Add => BinaryOp::Add,
                ast::BinaryOperator::Sub => BinaryOp::Sub,
                ast::BinaryOperator::Mul => BinaryOp::Mul,
                ast::BinaryOperator::Div => BinaryOp::Div,
                ast::BinaryOperator::Eq => BinaryOp::Eq,
                ast::BinaryOperator::NotEq => BinaryOp::NotEq,
            };
            HirExprKind::BinaryOp(Box::new(lhs), hir_op, Box::new(rhs))
        }
        ast::ExprKind::UnaryOp(ast::UnaryOperator::Neg, sub) => {
            let sub = lower_expr(sub);
            HirExprKind::UnaryOp(UnaryOp::Neg, Box::new(sub))
        }
        ast::ExprKind::Call(name, args) => {
            let lowered_args: Vec<HirExpr> = args.iter().map(lower_expr).collect();
            HirExprKind::Call(name.clone(), lowered_args)
        }
    };
    HirExpr {
        kind,
        ty: None,
        span,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indent;
    use crate::lexer::Token;
    use crate::parser::program_parser;
    use chumsky::Parser;
    use chumsky::input::{Input, Stream};
    use logos::Logos;

    fn parse_and_lower(input: &str) -> Result<(HirProgram, InternPool), Vec<Diag>> {
        let raw_tokens: Vec<_> = Token::lexer(input)
            .spanned()
            .map(|(tok, span)| match tok {
                Ok(tok) => (tok, span.into()),
                Err(()) => (Token::Error, span.into()),
            })
            .collect();

        let tokens = indent::process(raw_tokens).expect("indent ok");
        let token_stream =
            Stream::from_iter(tokens).map((0..input.len()).into(), |(t, s): (_, _)| (t, s));

        let program = program_parser()
            .parse(token_stream)
            .into_result()
            .expect("parse ok");

        let mut pool = InternPool::new();
        let mut sink = DiagSink::new();
        let hir = lower(&program, &mut pool, &mut sink);
        if sink.has_errors() {
            Err(sink.into_diags())
        } else {
            Ok((hir, pool))
        }
    }

    fn assert_all_expr_types_unresolved(hir: &HirProgram) {
        for func in &hir.functions {
            for stmt in &func.body {
                match stmt {
                    HirStmt::VarDecl { initializer, .. } => {
                        walk_unresolved(initializer);
                    }
                    HirStmt::Return(Some(e), _) => walk_unresolved(e),
                    HirStmt::Return(None, _) => {}
                    HirStmt::Expr(e, _) => walk_unresolved(e),
                }
            }
        }
    }

    fn walk_unresolved(e: &HirExpr) {
        assert!(
            e.ty.is_none(),
            "ast_lower must leave HirExpr.ty = None, got {:?}",
            e.ty
        );
        match &e.kind {
            HirExprKind::BinaryOp(l, _, r) => {
                walk_unresolved(l);
                walk_unresolved(r);
            }
            HirExprKind::UnaryOp(_, s) => walk_unresolved(s),
            HirExprKind::Call(_, args) => args.iter().for_each(walk_unresolved),
            _ => {}
        }
    }

    #[test]
    fn ast_lower_leaves_expression_types_unresolved() {
        let (hir, _) = parse_and_lower("x = 2 + 3 * 4\ny = x").unwrap();
        assert_all_expr_types_unresolved(&hir);
    }

    #[test]
    fn structural_shape_flat_integer_variable() {
        let (hir, _) = parse_and_lower("x = 42").unwrap();
        assert_eq!(hir.functions.len(), 1);
        let main = &hir.functions[0];
        assert_eq!(main.name, "main");
        assert_eq!(main.params.len(), 0);
        assert_eq!(main.body.len(), 2); // var decl + synthetic return
        match &main.body[0] {
            HirStmt::VarDecl { name, mutable, .. } => {
                assert_eq!(name, "x");
                assert!(!mutable);
            }
            _ => panic!("Expected VarDecl"),
        }
        match &main.body[1] {
            HirStmt::Return(Some(_), _) => {}
            _ => panic!("Expected synthetic Return(0)"),
        }
    }

    #[test]
    fn structural_shape_mutable_variable() {
        let (hir, _) = parse_and_lower("mut x = 42").unwrap();
        let main = &hir.functions[0];
        match &main.body[0] {
            HirStmt::VarDecl { mutable, .. } => assert!(*mutable),
            _ => panic!("Expected VarDecl"),
        }
    }

    #[test]
    fn structural_shape_binary_op() {
        let (hir, _) = parse_and_lower("x = 2 + 3 * 4").unwrap();
        let main = &hir.functions[0];
        match &main.body[0] {
            HirStmt::VarDecl { initializer, .. } => {
                assert!(matches!(
                    initializer.kind,
                    HirExprKind::BinaryOp(_, BinaryOp::Add, _)
                ));
            }
            _ => panic!("Expected VarDecl"),
        }
    }

    #[test]
    fn structural_shape_negation() {
        let (hir, _) = parse_and_lower("x = -42").unwrap();
        let main = &hir.functions[0];
        match &main.body[0] {
            HirStmt::VarDecl { initializer, .. } => {
                assert!(matches!(
                    initializer.kind,
                    HirExprKind::UnaryOp(UnaryOp::Neg, _)
                ));
            }
            _ => panic!("Expected VarDecl"),
        }
    }

    #[test]
    fn explicit_main_with_top_level_error() {
        let diags = parse_and_lower("x = 42\n\nfn main() -> int:\n\treturn 0\n").unwrap_err();
        assert!(
            diags
                .iter()
                .any(|d| d.code == DiagCode::TopLevelWithExplicitMain)
        );
    }

    #[test]
    fn explicit_main_structural() {
        let (hir, _) = parse_and_lower("fn main() -> int:\n\treturn 0\n").unwrap();
        assert_eq!(hir.functions.len(), 1);
        let main = &hir.functions[0];
        assert_eq!(main.name, "main");
        assert_eq!(main.body.len(), 1);
        assert!(matches!(main.body[0], HirStmt::Return(Some(_), _)));
    }

    #[test]
    fn unknown_type_annotation_rejected() {
        let diags = parse_and_lower("x: nope = 1").unwrap_err();
        assert!(diags.iter().any(|d| d.code == DiagCode::UnknownType));
    }

    #[test]
    fn helper_fn_with_top_level_lowers_both() {
        // Regression: previously the implicit-main path discarded
        // user-defined helper functions, leaving calls to them
        // dangling in sema.
        let (hir, _) =
            parse_and_lower("fn helper() -> int:\n\treturn 42\n\nx = helper()\n").unwrap();
        assert_eq!(hir.functions.len(), 2);
        assert!(hir.functions.iter().any(|f| f.name == "helper"));
        assert!(hir.functions.iter().any(|f| f.name == "main"));
    }

    #[test]
    fn two_functions_structural() {
        let code =
            "fn add(a: int, b: int) -> int:\n\treturn a + b\n\nfn main() -> int:\n\treturn 0\n";
        let (hir, _) = parse_and_lower(code).unwrap();
        assert_eq!(hir.functions.len(), 2);
        let add = hir.functions.iter().find(|f| f.name == "add").unwrap();
        assert_eq!(add.params.len(), 2);
        assert_eq!(add.params[0].name, "a");
        assert_eq!(add.params[1].name, "b");
    }
}
