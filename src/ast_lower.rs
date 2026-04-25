//! AST → HIR structural translation.
//!
//! Pure structural translation. Resolves syntactic type annotations
//! (`int`/`bool`/`str`) to `TypeId` because those come from
//! `TypeExpr` nodes and have no useful "no types yet" representation.
//! Every other expression's `ty` is left `None` for `sema` to fill.
//!
//! Identifier names come pre-interned as `StringId` from the parser,
//! so this stage is allocation-light: it copies handles around.

use crate::ast;
use crate::hir::*;
use crate::types::{InternPool, StringId};
use chumsky::span::{SimpleSpan, Span as _};

fn synthetic_span() -> Span {
    SimpleSpan::new((), 0..0)
}

pub fn lower(program: &ast::Program, pool: &mut InternPool) -> Result<HirProgram, String> {
    let mut func_defs: Vec<&ast::FunctionDef> = Vec::new();
    let mut top_level: Vec<&ast::Statement> = Vec::new();

    let main_id = pool.intern_str("main");

    for stmt in &program.statements {
        match &stmt.kind {
            ast::StmtKind::FunctionDef(f) => func_defs.push(f),
            _ => top_level.push(stmt),
        }
    }

    let has_explicit_main = func_defs.iter().any(|f| f.name.name == main_id);

    if has_explicit_main && !top_level.is_empty() {
        return Err("Top-level statements are not allowed when fn main() is defined".to_string());
    }

    let mut functions = Vec::new();
    for func in &func_defs {
        functions.push(lower_function_def(func, pool)?);
    }
    if !has_explicit_main {
        functions.push(lower_implicit_main(&top_level, main_id, pool)?);
    }

    Ok(HirProgram { functions })
}

fn resolve_type(name: StringId, pool: &InternPool) -> Result<TypeId, String> {
    match pool.str(name) {
        "int" => Ok(pool.int()),
        "str" => Ok(pool.str_()),
        "bool" => Ok(pool.bool_()),
        other => Err(format!("Unknown type: '{}'", other)),
    }
}

fn lower_implicit_main(
    stmts: &[&ast::Statement],
    main_id: StringId,
    pool: &mut InternPool,
) -> Result<HirFunction, String> {
    let mut body = Vec::new();
    for stmt in stmts {
        lower_stmt(stmt, pool, &mut body)?;
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

    Ok(HirFunction {
        name: main_id,
        params: vec![],
        return_type: int_ty,
        body,
    })
}

fn lower_function_def(
    func: &ast::FunctionDef,
    pool: &mut InternPool,
) -> Result<HirFunction, String> {
    let params: Vec<HirParam> = func
        .params
        .iter()
        .map(|p| {
            Ok(HirParam {
                name: p.name.name,
                ty: resolve_type(p.type_annotation.name, pool)?,
            })
        })
        .collect::<Result<_, String>>()?;

    let return_type = match &func.return_type {
        Some(ty) => resolve_type(ty.name, pool)?,
        None => pool.void(),
    };

    let mut body = Vec::new();
    for stmt in &func.body {
        lower_stmt(stmt, pool, &mut body)?;
    }

    Ok(HirFunction {
        name: func.name.name,
        params,
        return_type,
        body,
    })
}

fn lower_stmt(
    stmt: &ast::Statement,
    pool: &mut InternPool,
    out: &mut Vec<HirStmt>,
) -> Result<(), String> {
    match &stmt.kind {
        ast::StmtKind::VarDecl(decl) => {
            let initializer = lower_expr(&decl.initializer);
            let ty = match &decl.type_annotation {
                Some(ann) => Some(resolve_type(ann.name, pool)?),
                None => None,
            };
            out.push(HirStmt::VarDecl {
                name: decl.name.name,
                mutable: decl.mutable,
                ty,
                initializer,
                span: stmt.span,
            });
        }
        ast::StmtKind::Return(Some(expr)) => {
            out.push(HirStmt::Return(Some(lower_expr(expr)), stmt.span));
        }
        ast::StmtKind::Return(None) => {
            out.push(HirStmt::Return(None, stmt.span));
        }
        ast::StmtKind::ExprStmt(expr) => {
            out.push(HirStmt::Expr(lower_expr(expr), stmt.span));
        }
        ast::StmtKind::FunctionDef(_) => {
            return Err("Nested function definitions are not supported".to_string());
        }
    }
    Ok(())
}

fn lower_expr(expr: &ast::Expression) -> HirExpr {
    let span = expr.span;
    let kind = match &expr.kind {
        ast::ExprKind::Literal(ast::Literal::Int(n)) => HirExprKind::IntLiteral(*n),
        ast::ExprKind::Literal(ast::Literal::Str(id)) => HirExprKind::StrLiteral(*id),
        ast::ExprKind::Literal(ast::Literal::Bool(b)) => HirExprKind::BoolLiteral(*b),
        ast::ExprKind::Ident(id) => HirExprKind::Var(*id),
        ast::ExprKind::BinaryOp(lhs, op, rhs) => {
            let hir_op = match op {
                ast::BinaryOperator::Add => BinaryOp::Add,
                ast::BinaryOperator::Sub => BinaryOp::Sub,
                ast::BinaryOperator::Mul => BinaryOp::Mul,
                ast::BinaryOperator::Div => BinaryOp::Div,
                ast::BinaryOperator::Eq => BinaryOp::Eq,
                ast::BinaryOperator::NotEq => BinaryOp::NotEq,
            };
            HirExprKind::BinaryOp(Box::new(lower_expr(lhs)), hir_op, Box::new(lower_expr(rhs)))
        }
        ast::ExprKind::UnaryOp(ast::UnaryOperator::Neg, sub) => {
            HirExprKind::UnaryOp(UnaryOp::Neg, Box::new(lower_expr(sub)))
        }
        ast::ExprKind::Call(name, args) => {
            HirExprKind::Call(*name, args.iter().map(lower_expr).collect())
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
    use crate::lexer::lex;
    use crate::parser::program_parser;
    use chumsky::Parser;
    use chumsky::input::{Input, Stream};

    fn parse_and_lower(input: &str) -> Result<(HirProgram, InternPool), String> {
        let mut pool = InternPool::new();
        let tokens = lex(input, &mut pool).map_err(|e| e.message)?;
        let token_stream =
            Stream::from_iter(tokens).map((0..input.len()).into(), |(t, s): (_, _)| (t, s));
        let program = program_parser()
            .parse(token_stream)
            .into_result()
            .map_err(|errs| format!("Parse errors: {:?}", errs))?;
        let hir = lower(&program, &mut pool)?;
        Ok((hir, pool))
    }

    fn assert_all_expr_types_unresolved(hir: &HirProgram) {
        for func in &hir.functions {
            for stmt in &func.body {
                match stmt {
                    HirStmt::VarDecl { initializer, .. } => walk_unresolved(initializer),
                    HirStmt::Return(Some(e), _) => walk_unresolved(e),
                    HirStmt::Return(None, _) => {}
                    HirStmt::Expr(e, _) => walk_unresolved(e),
                }
            }
        }
    }

    fn walk_unresolved(e: &HirExpr) {
        assert!(e.ty.is_none(), "ast_lower must leave HirExpr.ty = None");
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
        let (hir, pool) = parse_and_lower("x = 42").unwrap();
        assert_eq!(hir.functions.len(), 1);
        let main = &hir.functions[0];
        assert_eq!(pool.str(main.name), "main");
        assert_eq!(main.params.len(), 0);
        assert_eq!(main.body.len(), 2);
        match &main.body[0] {
            HirStmt::VarDecl { name, mutable, .. } => {
                assert_eq!(pool.str(*name), "x");
                assert!(!mutable);
            }
            _ => panic!("Expected VarDecl"),
        }
        assert!(matches!(main.body[1], HirStmt::Return(Some(_), _)));
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
            HirStmt::VarDecl { initializer, .. } => assert!(matches!(
                initializer.kind,
                HirExprKind::BinaryOp(_, BinaryOp::Add, _)
            )),
            _ => panic!("Expected VarDecl"),
        }
    }

    #[test]
    fn structural_shape_negation() {
        let (hir, _) = parse_and_lower("x = -42").unwrap();
        let main = &hir.functions[0];
        match &main.body[0] {
            HirStmt::VarDecl { initializer, .. } => assert!(matches!(
                initializer.kind,
                HirExprKind::UnaryOp(UnaryOp::Neg, _)
            )),
            _ => panic!("Expected VarDecl"),
        }
    }

    #[test]
    fn explicit_main_with_top_level_error() {
        let err = parse_and_lower("x = 42\n\nfn main() -> int:\n\treturn 0\n").unwrap_err();
        assert!(err.contains("Top-level statements are not allowed"));
    }

    #[test]
    fn explicit_main_structural() {
        let (hir, pool) = parse_and_lower("fn main() -> int:\n\treturn 0\n").unwrap();
        assert_eq!(hir.functions.len(), 1);
        let main = &hir.functions[0];
        assert_eq!(pool.str(main.name), "main");
        assert_eq!(main.body.len(), 1);
        assert!(matches!(main.body[0], HirStmt::Return(Some(_), _)));
    }

    #[test]
    fn unknown_type_annotation_rejected() {
        let err = parse_and_lower("x: nope = 1").unwrap_err();
        assert!(err.contains("Unknown type"));
    }

    #[test]
    fn helper_fn_with_top_level_lowers_both() {
        let (hir, pool) =
            parse_and_lower("fn helper() -> int:\n\treturn 42\n\nx = helper()\n").unwrap();
        assert_eq!(hir.functions.len(), 2);
        assert!(hir.functions.iter().any(|f| pool.str(f.name) == "helper"));
        assert!(hir.functions.iter().any(|f| pool.str(f.name) == "main"));
    }

    #[test]
    fn two_functions_structural() {
        let code =
            "fn add(a: int, b: int) -> int:\n\treturn a + b\n\nfn main() -> int:\n\treturn 0\n";
        let (hir, pool) = parse_and_lower(code).unwrap();
        assert_eq!(hir.functions.len(), 2);
        let add = hir
            .functions
            .iter()
            .find(|f| pool.str(f.name) == "add")
            .unwrap();
        assert_eq!(add.params.len(), 2);
        assert_eq!(pool.str(add.params[0].name), "a");
        assert_eq!(pool.str(add.params[1].name), "b");
    }
}
