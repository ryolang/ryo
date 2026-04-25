//! Semantic analysis: scope + type resolution over an already-built HIR.
//!
//! On entry, `HirExpr.ty` is `None` everywhere. On successful return,
//! every `HirExpr.ty` is `Some(...)` and codegen can safely unwrap.

use crate::builtins;
use crate::hir::*;
use crate::types::{InternPool, TypeKind};
use std::collections::HashMap;

struct FunctionSig {
    return_type: TypeId,
}

struct Scope<'a> {
    parent: Option<&'a Scope<'a>>,
    bindings: HashMap<String, TypeId>,
}

impl<'a> Scope<'a> {
    fn new() -> Self {
        Scope {
            parent: None,
            bindings: HashMap::new(),
        }
    }

    fn insert(&mut self, name: String, ty: TypeId) {
        self.bindings.insert(name, ty);
    }

    fn lookup(&self, name: &str) -> Option<TypeId> {
        self.bindings
            .get(name)
            .copied()
            .or_else(|| self.parent?.lookup(name))
    }
}

pub fn analyze(program: &mut HirProgram, pool: &mut InternPool) -> Result<(), String> {
    let mut signatures: HashMap<String, FunctionSig> = HashMap::new();
    for func in &program.functions {
        signatures.insert(
            func.name.clone(),
            FunctionSig {
                return_type: func.return_type,
            },
        );
    }

    for func in &mut program.functions {
        analyze_function(func, &signatures, pool)?;
    }

    Ok(())
}

fn analyze_function(
    func: &mut HirFunction,
    signatures: &HashMap<String, FunctionSig>,
    pool: &mut InternPool,
) -> Result<(), String> {
    let mut scope = Scope::new();
    for param in &func.params {
        scope.insert(param.name.clone(), param.ty);
    }

    for stmt in &mut func.body {
        analyze_stmt(stmt, &mut scope, signatures, pool)?;
    }

    Ok(())
}

fn analyze_stmt(
    stmt: &mut HirStmt,
    scope: &mut Scope,
    signatures: &HashMap<String, FunctionSig>,
    pool: &mut InternPool,
) -> Result<(), String> {
    match stmt {
        HirStmt::VarDecl {
            name,
            ty,
            initializer,
            ..
        } => {
            analyze_expr(initializer, scope, signatures, pool)?;
            let init_ty = initializer.expect_ty();
            // `ty` was set to pool.void() by ast_lower as a sentinel if
            // there was no annotation. Replace it with the inferred type.
            if *ty == pool.void() {
                *ty = init_ty;
            }
            scope.insert(name.clone(), *ty);
        }
        HirStmt::Return(Some(expr), _) => {
            analyze_expr(expr, scope, signatures, pool)?;
        }
        HirStmt::Return(None, _) => {}
        HirStmt::Expr(expr, _) => {
            analyze_expr(expr, scope, signatures, pool)?;
        }
    }
    Ok(())
}

fn analyze_expr(
    expr: &mut HirExpr,
    scope: &Scope,
    signatures: &HashMap<String, FunctionSig>,
    pool: &mut InternPool,
) -> Result<(), String> {
    let ty = match &mut expr.kind {
        HirExprKind::IntLiteral(_) => pool.int(),
        HirExprKind::StrLiteral(_) => pool.str_(),
        HirExprKind::BoolLiteral(_) => pool.bool_(),
        HirExprKind::Var(name) => scope
            .lookup(name.as_str())
            .ok_or_else(|| format!("Undefined variable: '{}'", name))?,
        HirExprKind::BinaryOp(lhs, op, rhs) => {
            analyze_expr(lhs, scope, signatures, pool)?;
            analyze_expr(rhs, scope, signatures, pool)?;
            let lhs_ty = lhs.expect_ty();
            let rhs_ty = rhs.expect_ty();
            if lhs_ty != rhs_ty {
                return Err(format!(
                    "type mismatch in '{}': left is '{}', right is '{}'",
                    binop_symbol(*op),
                    pool.display(lhs_ty),
                    pool.display(rhs_ty),
                ));
            }

            let is_equality = matches!(op, BinaryOp::Eq | BinaryOp::NotEq);
            if is_equality {
                match pool.kind(lhs_ty) {
                    TypeKind::Int | TypeKind::Bool => pool.bool_(),
                    TypeKind::Str => {
                        return Err(format!(
                            "equality operator '{}' not supported for type 'str' (yet)",
                            binop_symbol(*op),
                        ));
                    }
                    TypeKind::Void => {
                        return Err(format!(
                            "equality operator '{}' not supported for type 'void'",
                            binop_symbol(*op),
                        ));
                    }
                }
            } else {
                match pool.kind(lhs_ty) {
                    TypeKind::Int => pool.int(),
                    _ => {
                        return Err(format!(
                            "arithmetic operator '{}' not supported for type '{}'",
                            binop_symbol(*op),
                            pool.display(lhs_ty),
                        ));
                    }
                }
            }
        }
        HirExprKind::UnaryOp(UnaryOp::Neg, sub) => {
            analyze_expr(sub, scope, signatures, pool)?;
            pool.int()
        }
        HirExprKind::Call(name, args) => {
            for arg in args.iter_mut() {
                analyze_expr(arg, scope, signatures, pool)?;
            }
            if let Some(builtin) = builtins::lookup(name) {
                builtin.return_type(pool)
            } else {
                signatures
                    .get(name.as_str())
                    .map(|sig| sig.return_type)
                    .ok_or_else(|| format!("Undefined function: '{}'", name))?
            }
        }
    };
    expr.ty = Some(ty);
    Ok(())
}

fn binop_symbol(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Add => "+",
        BinaryOp::Sub => "-",
        BinaryOp::Mul => "*",
        BinaryOp::Div => "/",
        BinaryOp::Eq => "==",
        BinaryOp::NotEq => "!=",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast_lower;
    use crate::indent;
    use crate::lexer::Token;
    use crate::parser::program_parser;
    use chumsky::Parser;
    use chumsky::input::{Input, Stream};
    use logos::Logos;

    fn run(input: &str) -> Result<(HirProgram, InternPool), String> {
        let raw_tokens: Vec<_> = Token::lexer(input)
            .spanned()
            .map(|(tok, span)| match tok {
                Ok(tok) => (tok, span.into()),
                Err(()) => (Token::Error, span.into()),
            })
            .collect();

        let tokens = indent::process(raw_tokens)?;
        let token_stream =
            Stream::from_iter(tokens).map((0..input.len()).into(), |(t, s): (_, _)| (t, s));
        let program = program_parser()
            .parse(token_stream)
            .into_result()
            .map_err(|errs| format!("Parse errors: {:?}", errs))?;

        let mut pool = InternPool::new();
        let mut hir = ast_lower::lower(&program, &mut pool)?;
        analyze(&mut hir, &mut pool)?;
        Ok((hir, pool))
    }

    fn assert_all_expr_types_resolved(hir: &HirProgram) {
        for func in &hir.functions {
            for stmt in &func.body {
                match stmt {
                    HirStmt::VarDecl { initializer, .. } => walk(initializer),
                    HirStmt::Return(Some(e), _) => walk(e),
                    HirStmt::Return(None, _) => {}
                    HirStmt::Expr(e, _) => walk(e),
                }
            }
        }
        fn walk(e: &HirExpr) {
            assert!(e.ty.is_some(), "unresolved HirExpr.ty after sema");
            match &e.kind {
                HirExprKind::BinaryOp(l, _, r) => {
                    walk(l);
                    walk(r);
                }
                HirExprKind::UnaryOp(_, s) => walk(s),
                HirExprKind::Call(_, args) => args.iter().for_each(walk),
                _ => {}
            }
        }
    }

    #[test]
    fn fills_types_on_flat_integer_var() {
        let (hir, pool) = run("x = 42").unwrap();
        assert_all_expr_types_resolved(&hir);
        let main = &hir.functions[0];
        assert_eq!(main.return_type, pool.int());
        match &main.body[0] {
            HirStmt::VarDecl { ty, .. } => assert_eq!(*ty, pool.int()),
            _ => panic!(),
        }
    }

    #[test]
    fn infers_string_literal_type() {
        let (hir, pool) = run("x = \"hello\"").unwrap();
        match &hir.functions[0].body[0] {
            HirStmt::VarDecl { ty, .. } => assert_eq!(*ty, pool.str_()),
            _ => panic!(),
        }
    }

    #[test]
    fn typed_variable_annotation_honored() {
        let (hir, pool) = run("x: int = 42").unwrap();
        match &hir.functions[0].body[0] {
            HirStmt::VarDecl { ty, .. } => assert_eq!(*ty, pool.int()),
            _ => panic!(),
        }
    }

    #[test]
    fn bool_annotation_resolves() {
        let (hir, pool) = run("x: bool = true").unwrap();
        match &hir.functions[0].body[0] {
            HirStmt::VarDecl { ty, .. } => assert_eq!(*ty, pool.bool_()),
            _ => panic!(),
        }
    }

    #[test]
    fn variable_reference_type_resolved() {
        let (hir, pool) = run("x = 42\ny = x").unwrap();
        match &hir.functions[0].body[1] {
            HirStmt::VarDecl {
                ty, initializer, ..
            } => {
                assert_eq!(*ty, pool.int());
                assert_eq!(initializer.expect_ty(), pool.int());
                assert!(matches!(initializer.kind, HirExprKind::Var(_)));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn undefined_variable_rejected() {
        let err = run("x = y").unwrap_err();
        assert!(err.contains("Undefined variable"));
    }

    #[test]
    fn undefined_function_rejected() {
        let err = run("x = not_a_fn()").unwrap_err();
        assert!(err.contains("Undefined function"));
    }

    #[test]
    fn function_call_return_type_resolved() {
        let code =
            "fn double(x: int) -> int:\n\treturn x * 2\n\nfn main() -> int:\n\treturn double(3)\n";
        let (hir, pool) = run(code).unwrap();
        let main = hir.functions.iter().find(|f| f.name == "main").unwrap();
        match &main.body[0] {
            HirStmt::Return(Some(e), _) => {
                assert_eq!(e.expect_ty(), pool.int());
                assert!(matches!(e.kind, HirExprKind::Call(_, _)));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn void_function_signature() {
        let code = "fn greet():\n\tprint(\"hi\")\n\nfn main() -> int:\n\tgreet()\n\treturn 0\n";
        let (hir, pool) = run(code).unwrap();
        let greet = hir.functions.iter().find(|f| f.name == "greet").unwrap();
        assert_eq!(greet.return_type, pool.void());
    }

    #[test]
    fn print_call_has_int_type() {
        let (hir, pool) = run("msg = print(\"Hello\\n\")").unwrap();
        match &hir.functions[0].body[0] {
            HirStmt::VarDecl {
                ty, initializer, ..
            } => {
                assert_eq!(*ty, pool.int());
                assert_eq!(initializer.expect_ty(), pool.int());
            }
            _ => panic!(),
        }
    }

    #[test]
    fn int_equality_yields_bool() {
        let (hir, pool) = run("x = 1 == 2").unwrap();
        match &hir.functions[0].body[0] {
            HirStmt::VarDecl {
                ty, initializer, ..
            } => {
                assert_eq!(*ty, pool.bool_());
                assert_eq!(initializer.expect_ty(), pool.bool_());
            }
            _ => panic!(),
        }
    }

    #[test]
    fn mixed_type_equality_rejected() {
        let err = run("x = 1 == true").unwrap_err();
        assert!(err.contains("type mismatch in '=='"));
    }

    #[test]
    fn string_equality_rejected() {
        let err = run("x = \"a\" == \"b\"").unwrap_err();
        assert!(err.contains("not supported for type 'str'") && err.contains("(yet)"));
    }

    #[test]
    fn bool_arithmetic_rejected() {
        let err = run("x = true + 1").unwrap_err();
        assert!(err.contains("type mismatch"));
    }

    #[test]
    fn bool_literal_true_type() {
        let (hir, pool) = run("x = true").unwrap();
        match &hir.functions[0].body[0] {
            HirStmt::VarDecl {
                ty, initializer, ..
            } => {
                assert_eq!(*ty, pool.bool_());
                assert!(matches!(initializer.kind, HirExprKind::BoolLiteral(true)));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn nested_expression_types_all_filled() {
        let (hir, _) = run("x = (1 + 2) * -3").unwrap();
        assert_all_expr_types_resolved(&hir);
    }
}
