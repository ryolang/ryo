//! Semantic analysis: scope + type resolution over an already-built HIR.
//!
//! On entry, `HirExpr.ty` is `None` everywhere. On successful return,
//! every `HirExpr.ty` is `Some(...)` and codegen can safely unwrap.
//!
//! Scopes and signatures are keyed on `StringId`, not `String`:
//! identifiers are interned once at lex time and propagated as
//! `Copy` handles all the way through codegen.

use crate::builtins;
use crate::hir::*;
use crate::types::{InternPool, StringId, TypeKind};
use std::collections::HashMap;

struct FunctionSig {
    params: Vec<TypeId>,
    return_type: TypeId,
}

struct Scope<'a> {
    parent: Option<&'a Scope<'a>>,
    bindings: HashMap<StringId, TypeId>,
}

impl<'a> Scope<'a> {
    fn new() -> Self {
        Scope {
            parent: None,
            bindings: HashMap::new(),
        }
    }

    fn insert(&mut self, name: StringId, ty: TypeId) {
        self.bindings.insert(name, ty);
    }

    fn lookup(&self, name: StringId) -> Option<TypeId> {
        self.bindings
            .get(&name)
            .copied()
            .or_else(|| self.parent?.lookup(name))
    }
}

pub fn analyze(program: &mut HirProgram, pool: &mut InternPool) -> Result<(), String> {
    let mut signatures: HashMap<StringId, FunctionSig> = HashMap::new();
    for func in &program.functions {
        signatures.insert(
            func.name,
            FunctionSig {
                params: func.params.iter().map(|p| p.ty).collect(),
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
    signatures: &HashMap<StringId, FunctionSig>,
    pool: &mut InternPool,
) -> Result<(), String> {
    let mut scope = Scope::new();
    for param in &func.params {
        scope.insert(param.name, param.ty);
    }

    let fn_return_type = func.return_type;
    for stmt in &mut func.body {
        analyze_stmt(stmt, &mut scope, signatures, pool, fn_return_type)?;
    }

    Ok(())
}

fn analyze_stmt(
    stmt: &mut HirStmt,
    scope: &mut Scope,
    signatures: &HashMap<StringId, FunctionSig>,
    pool: &mut InternPool,
    fn_return_type: TypeId,
) -> Result<(), String> {
    match stmt {
        HirStmt::VarDecl {
            name,
            ty,
            initializer,
            ..
        } => {
            analyze_expr(initializer, scope, signatures, pool)?;
            let inferred = initializer.expect_ty();
            let resolved = match *ty {
                Some(annotated) if !pool.compatible(annotated, inferred) => {
                    return Err(format!(
                        "type mismatch: '{}' annotated '{}', initializer is '{}'",
                        pool.str(*name),
                        pool.display(annotated),
                        pool.display(inferred),
                    ));
                }
                Some(annotated) => annotated,
                None => inferred,
            };
            *ty = Some(resolved);
            scope.insert(*name, resolved);
        }
        HirStmt::Return(Some(expr), _) => {
            analyze_expr(expr, scope, signatures, pool)?;
            if fn_return_type == pool.void() {
                return Err(format!(
                    "cannot return a value from a function with return type 'void' (got '{}')",
                    pool.display(expr.expect_ty()),
                ));
            }
            let actual = expr.expect_ty();
            if !pool.compatible(actual, fn_return_type) {
                return Err(format!(
                    "return type mismatch: function expects '{}', got '{}'",
                    pool.display(fn_return_type),
                    pool.display(actual),
                ));
            }
        }
        HirStmt::Return(None, _) => {
            if fn_return_type != pool.void() {
                return Err(format!(
                    "missing return value: function expects '{}'",
                    pool.display(fn_return_type),
                ));
            }
        }
        HirStmt::Expr(expr, _) => {
            analyze_expr(expr, scope, signatures, pool)?;
        }
    }
    Ok(())
}

fn analyze_expr(
    expr: &mut HirExpr,
    scope: &Scope,
    signatures: &HashMap<StringId, FunctionSig>,
    pool: &mut InternPool,
) -> Result<(), String> {
    let ty = match &mut expr.kind {
        HirExprKind::IntLiteral(_) => pool.int(),
        HirExprKind::StrLiteral(_) => pool.str_(),
        HirExprKind::BoolLiteral(_) => pool.bool_(),
        HirExprKind::Var(name) => scope
            .lookup(*name)
            .ok_or_else(|| format!("Undefined variable: '{}'", pool.str(*name)))?,
        HirExprKind::BinaryOp(lhs, op, rhs) => {
            analyze_expr(lhs, scope, signatures, pool)?;
            analyze_expr(rhs, scope, signatures, pool)?;
            let lhs_ty = lhs.expect_ty();
            let rhs_ty = rhs.expect_ty();
            if !pool.compatible(lhs_ty, rhs_ty) {
                return Err(format!(
                    "type mismatch in '{}': left is '{}', right is '{}'",
                    op,
                    pool.display(lhs_ty),
                    pool.display(rhs_ty),
                ));
            }

            let kind_ty = if pool.is_error(lhs_ty) {
                rhs_ty
            } else {
                lhs_ty
            };
            let is_equality = matches!(op, BinaryOp::Eq | BinaryOp::NotEq);
            if is_equality {
                match pool.kind(kind_ty) {
                    TypeKind::Int | TypeKind::Bool => pool.bool_(),
                    TypeKind::Error => pool.error_type(),
                    TypeKind::Str => {
                        return Err(format!(
                            "equality operator '{}' not supported for type 'str' (yet)",
                            op,
                        ));
                    }
                    TypeKind::Void | TypeKind::Tuple => {
                        return Err(format!(
                            "equality operator '{}' not supported for type '{}'",
                            op,
                            pool.display(kind_ty),
                        ));
                    }
                }
            } else {
                match pool.kind(kind_ty) {
                    TypeKind::Int => pool.int(),
                    TypeKind::Error => pool.error_type(),
                    _ => {
                        return Err(format!(
                            "arithmetic operator '{}' not supported for type '{}'",
                            op,
                            pool.display(kind_ty),
                        ));
                    }
                }
            }
        }
        HirExprKind::UnaryOp(UnaryOp::Neg, sub) => {
            analyze_expr(sub, scope, signatures, pool)?;
            let sub_ty = sub.expect_ty();
            match pool.kind(sub_ty) {
                TypeKind::Int => pool.int(),
                TypeKind::Error => pool.error_type(),
                _ => {
                    return Err(format!(
                        "unary operator '-' not supported for type '{}'",
                        pool.display(sub_ty),
                    ));
                }
            }
        }
        HirExprKind::Call(name, args) => {
            for arg in args.iter_mut() {
                analyze_expr(arg, scope, signatures, pool)?;
            }
            let name_str = pool.str(*name);
            if let Some(builtin) = builtins::lookup(name_str) {
                check_builtin_call(name_str, args)?;
                builtin.return_type(pool)
            } else if let Some(sig) = signatures.get(name) {
                if args.len() != sig.params.len() {
                    return Err(format!(
                        "call to '{}' has wrong arity: expected {} argument(s), got {}",
                        pool.str(*name),
                        sig.params.len(),
                        args.len(),
                    ));
                }
                for (idx, (arg, &expected)) in args.iter().zip(sig.params.iter()).enumerate() {
                    let actual = arg.expect_ty();
                    if !pool.compatible(actual, expected) {
                        return Err(format!(
                            "call to '{}': argument {} has type '{}', expected '{}'",
                            pool.str(*name),
                            idx + 1,
                            pool.display(actual),
                            pool.display(expected),
                        ));
                    }
                }
                sig.return_type
            } else {
                return Err(format!("Undefined function: '{}'", pool.str(*name)));
            }
        }
    };
    expr.ty = Some(ty);
    Ok(())
}

/// Front-end validation for builtin calls. These checks go away once
/// `print` moves to a runtime crate (ISSUES.md I-006).
fn check_builtin_call(name: &str, args: &[HirExpr]) -> Result<(), String> {
    if name == "print" {
        if args.len() != 1 {
            return Err(format!(
                "print() takes exactly 1 argument, got {}",
                args.len()
            ));
        }
        if !matches!(args[0].kind, HirExprKind::StrLiteral(_)) {
            return Err("print() argument must be a string literal".to_string());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast_lower;
    use crate::lexer::lex;
    use crate::parser::program_parser;
    use chumsky::Parser;
    use chumsky::input::{Input, Stream};

    fn run(input: &str) -> Result<(HirProgram, InternPool), String> {
        let mut pool = InternPool::new();
        let tokens = lex(input, &mut pool).map_err(|e| e.message)?;
        let token_stream =
            Stream::from_iter(tokens).map((0..input.len()).into(), |(t, s): (_, _)| (t, s));
        let program = program_parser()
            .parse(token_stream)
            .into_result()
            .map_err(|errs| format!("Parse errors: {:?}", errs))?;
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
            HirStmt::VarDecl { ty, .. } => assert_eq!(*ty, Some(pool.int())),
            _ => panic!(),
        }
    }

    #[test]
    fn infers_string_literal_type() {
        let (hir, pool) = run("x = \"hello\"").unwrap();
        match &hir.functions[0].body[0] {
            HirStmt::VarDecl { ty, .. } => assert_eq!(*ty, Some(pool.str_())),
            _ => panic!(),
        }
    }

    #[test]
    fn typed_variable_annotation_honored() {
        let (hir, pool) = run("x: int = 42").unwrap();
        match &hir.functions[0].body[0] {
            HirStmt::VarDecl { ty, .. } => assert_eq!(*ty, Some(pool.int())),
            _ => panic!(),
        }
    }

    #[test]
    fn bool_annotation_resolves() {
        let (hir, pool) = run("x: bool = true").unwrap();
        match &hir.functions[0].body[0] {
            HirStmt::VarDecl { ty, .. } => assert_eq!(*ty, Some(pool.bool_())),
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
                assert_eq!(*ty, Some(pool.int()));
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
        let main = hir
            .functions
            .iter()
            .find(|f| pool.str(f.name) == "main")
            .unwrap();
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
        let greet = hir
            .functions
            .iter()
            .find(|f| pool.str(f.name) == "greet")
            .unwrap();
        assert_eq!(greet.return_type, pool.void());
    }

    #[test]
    fn print_call_has_int_type() {
        let (hir, pool) = run("msg = print(\"Hello\\n\")").unwrap();
        match &hir.functions[0].body[0] {
            HirStmt::VarDecl {
                ty, initializer, ..
            } => {
                assert_eq!(*ty, Some(pool.int()));
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
                assert_eq!(*ty, Some(pool.bool_()));
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
                assert_eq!(*ty, Some(pool.bool_()));
                assert!(matches!(initializer.kind, HirExprKind::BoolLiteral(true)));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn print_with_non_literal_rejected_in_sema() {
        let err = run("x = \"hi\"\n_ = print(x)").unwrap_err();
        assert!(err.contains("print() argument must be a string literal"));
    }

    #[test]
    fn print_arity_rejected_in_sema() {
        let err = run("_ = print(\"a\", \"b\")").unwrap_err();
        assert!(err.contains("print() takes exactly 1 argument"));
    }

    #[test]
    fn return_type_mismatch_rejected() {
        let err = run("fn main() -> int:\n\treturn \"hello\"\n").unwrap_err();
        assert!(
            err.contains("return type mismatch") && err.contains("'int'") && err.contains("'str'")
        );
    }

    #[test]
    fn missing_return_value_rejected() {
        let err = run("fn f() -> int:\n\treturn\n\nfn main() -> int:\n\treturn 0\n").unwrap_err();
        assert!(err.contains("missing return value"));
    }

    #[test]
    fn return_value_from_void_function_rejected() {
        let err = run("fn g():\n\treturn 1\n\nfn main() -> int:\n\treturn 0\n").unwrap_err();
        assert!(err.contains("cannot return a value from a function with return type 'void'"));
    }

    #[test]
    fn call_arity_mismatch_rejected() {
        let code = "fn add(a: int, b: int) -> int:\n\treturn a + b\n\nfn main() -> int:\n\treturn add(1, 2, 3)\n";
        let err = run(code).unwrap_err();
        assert!(err.contains("wrong arity") && err.contains("expected 2") && err.contains("got 3"));
    }

    #[test]
    fn call_argument_type_mismatch_rejected() {
        let code = "fn f(a: int) -> int:\n\treturn a\n\nfn main() -> int:\n\treturn f(true)\n";
        let err = run(code).unwrap_err();
        assert!(err.contains("argument 1") && err.contains("'bool'") && err.contains("'int'"));
    }

    #[test]
    fn vardecl_annotation_initializer_mismatch_rejected() {
        let err = run("x: int = \"hello\"").unwrap_err();
        assert!(
            err.contains("type mismatch")
                && err.contains("'x'")
                && err.contains("'int'")
                && err.contains("'str'")
        );
    }

    #[test]
    fn neg_on_bool_rejected() {
        let err = run("x = -true").unwrap_err();
        assert!(err.contains("unary operator '-'") && err.contains("'bool'"));
    }

    #[test]
    fn nested_expression_types_all_filled() {
        let (hir, _) = run("x = (1 + 2) * -3").unwrap();
        assert_all_expr_types_resolved(&hir);
    }
}
