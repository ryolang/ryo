//! Semantic analysis: scope + type resolution over an already-built HIR.
//!
//! On entry, `HirExpr.ty` is `None` everywhere. On successful return,
//! every `HirExpr.ty` is `Some(...)` and codegen can safely unwrap.
//!
//! Scopes and signatures are keyed on `StringId`, not `String`:
//! identifiers are interned once at lex time and propagated as
//! `Copy` handles all the way through codegen (Phase 2).
//!
//! Errors are accumulated through a `DiagSink` (Phase 1) so analysis
//! can continue past the first problem and surface several in one
//! run. A failed expression substitutes `pool.error_type()` for its
//! result type and downstream checks treat that sentinel as
//! compatible with anything via `InternPool::compatible`.

use crate::builtins;
use crate::diag::{Diag, DiagCode, DiagSink};
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

/// Analyze `program`, threading types onto every `HirExpr` and
/// emitting diagnostics into `sink` for any problem found.
///
/// Sema continues past errors: a single bad expression substitutes
/// `pool.error_type()` for its result type and downstream checks
/// treat that sentinel as compatible with anything (see
/// `InternPool::compatible`). The driver consults
/// `sink.has_errors()` to decide whether to proceed to codegen.
pub fn analyze(program: &mut HirProgram, pool: &mut InternPool, sink: &mut DiagSink) {
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
        analyze_function(func, &signatures, pool, sink);
    }
}

fn analyze_function(
    func: &mut HirFunction,
    signatures: &HashMap<StringId, FunctionSig>,
    pool: &mut InternPool,
    sink: &mut DiagSink,
) {
    let mut scope = Scope::new();
    for param in &func.params {
        scope.insert(param.name, param.ty);
    }

    let fn_return_type = func.return_type;
    for stmt in &mut func.body {
        analyze_stmt(stmt, &mut scope, signatures, pool, sink, fn_return_type);
    }
}

fn analyze_stmt(
    stmt: &mut HirStmt,
    scope: &mut Scope,
    signatures: &HashMap<StringId, FunctionSig>,
    pool: &mut InternPool,
    sink: &mut DiagSink,
    fn_return_type: TypeId,
) {
    match stmt {
        HirStmt::VarDecl {
            name,
            ty,
            initializer,
            span: _,
            mutable: _,
        } => {
            analyze_expr(initializer, scope, signatures, pool, sink);
            let inferred = initializer.expect_ty();
            let resolved = match *ty {
                Some(annotated) if !pool.compatible(annotated, inferred) => {
                    // Anchor the squiggle on the offending value
                    // (the initializer) rather than on the whole
                    // `[mut] name [: type] = expr` decl span — the
                    // type came from the annotation but the
                    // *mismatch* is the initializer's fault.
                    sink.emit(Diag::error(
                        initializer.span,
                        DiagCode::TypeMismatch,
                        format!(
                            "type mismatch: '{}' annotated '{}', initializer is '{}'",
                            pool.str(*name),
                            pool.display(annotated),
                            pool.display(inferred),
                        ),
                    ));
                    annotated
                }
                Some(annotated) => annotated,
                None => inferred,
            };
            *ty = Some(resolved);
            scope.insert(*name, resolved);
        }
        HirStmt::Return(Some(expr), span) => {
            analyze_expr(expr, scope, signatures, pool, sink);
            let actual = expr.expect_ty();
            if fn_return_type == pool.void() {
                if !pool.is_error(actual) {
                    sink.emit(Diag::error(
                        *span,
                        DiagCode::TypeMismatch,
                        format!(
                            "cannot return a value from a function with return type 'void' (got '{}')",
                            pool.display(actual),
                        ),
                    ));
                }
            } else if !pool.compatible(actual, fn_return_type) {
                sink.emit(Diag::error(
                    *span,
                    DiagCode::TypeMismatch,
                    format!(
                        "return type mismatch: function expects '{}', got '{}'",
                        pool.display(fn_return_type),
                        pool.display(actual),
                    ),
                ));
            }
        }
        HirStmt::Return(None, span) => {
            if fn_return_type != pool.void() && !pool.is_error(fn_return_type) {
                sink.emit(Diag::error(
                    *span,
                    DiagCode::TypeMismatch,
                    format!(
                        "missing return value: function expects '{}'",
                        pool.display(fn_return_type),
                    ),
                ));
            }
        }
        HirStmt::Expr(expr, _) => {
            analyze_expr(expr, scope, signatures, pool, sink);
        }
    }
}

fn analyze_expr(
    expr: &mut HirExpr,
    scope: &Scope,
    signatures: &HashMap<StringId, FunctionSig>,
    pool: &mut InternPool,
    sink: &mut DiagSink,
) {
    let span = expr.span;
    let ty = match &mut expr.kind {
        HirExprKind::IntLiteral(_) => pool.int(),
        HirExprKind::StrLiteral(_) => pool.str_(),
        HirExprKind::BoolLiteral(_) => pool.bool_(),
        HirExprKind::Var(name) => match scope.lookup(*name) {
            Some(t) => t,
            None => {
                sink.emit(Diag::error(
                    span,
                    DiagCode::UndefinedVariable,
                    format!("undefined variable: '{}'", pool.str(*name)),
                ));
                pool.error_type()
            }
        },
        HirExprKind::BinaryOp(lhs, op, rhs) => {
            analyze_expr(lhs, scope, signatures, pool, sink);
            analyze_expr(rhs, scope, signatures, pool, sink);
            let lhs_ty = lhs.expect_ty();
            let rhs_ty = rhs.expect_ty();
            if !pool.compatible(lhs_ty, rhs_ty) {
                sink.emit(Diag::error(
                    span,
                    DiagCode::TypeMismatch,
                    format!(
                        "type mismatch in '{}': left is '{}', right is '{}'",
                        op,
                        pool.display(lhs_ty),
                        pool.display(rhs_ty),
                    ),
                ));
                pool.error_type()
            } else {
                // Pick the non-error operand for the kind check, if any.
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
                            sink.emit(Diag::error(
                                span,
                                DiagCode::UnsupportedOperator,
                                format!(
                                    "equality operator '{}' not supported for type 'str' (yet)",
                                    op,
                                ),
                            ));
                            pool.error_type()
                        }
                        TypeKind::Void | TypeKind::Tuple => {
                            sink.emit(Diag::error(
                                span,
                                DiagCode::UnsupportedOperator,
                                format!(
                                    "equality operator '{}' not supported for type '{}'",
                                    op,
                                    pool.display(kind_ty),
                                ),
                            ));
                            pool.error_type()
                        }
                    }
                } else {
                    match pool.kind(kind_ty) {
                        TypeKind::Int => pool.int(),
                        TypeKind::Error => pool.error_type(),
                        _ => {
                            sink.emit(Diag::error(
                                span,
                                DiagCode::UnsupportedOperator,
                                format!(
                                    "arithmetic operator '{}' not supported for type '{}'",
                                    op,
                                    pool.display(kind_ty),
                                ),
                            ));
                            pool.error_type()
                        }
                    }
                }
            }
        }
        HirExprKind::UnaryOp(UnaryOp::Neg, sub) => {
            analyze_expr(sub, scope, signatures, pool, sink);
            let sub_ty = sub.expect_ty();
            match pool.kind(sub_ty) {
                TypeKind::Int => pool.int(),
                TypeKind::Error => pool.error_type(),
                _ => {
                    sink.emit(Diag::error(
                        span,
                        DiagCode::UnsupportedOperator,
                        format!(
                            "unary operator '-' not supported for type '{}'",
                            pool.display(sub_ty),
                        ),
                    ));
                    pool.error_type()
                }
            }
        }
        HirExprKind::Call(name, args) => {
            for arg in args.iter_mut() {
                analyze_expr(arg, scope, signatures, pool, sink);
            }
            // Resolve the name once: a single `pool.str` call gives
            // us the &str needed for both the builtin lookup and
            // the diagnostic messages, while `name_id` keeps the
            // signature-table lookup at a `StringId` (u32) compare.
            let name_id = *name;
            let name_str = pool.str(name_id);
            if let Some(builtin) = builtins::lookup(name_str) {
                check_builtin_call(name_str, args, span, sink);
                builtin.return_type(pool)
            } else if let Some(sig) = signatures.get(&name_id) {
                if args.len() != sig.params.len() {
                    sink.emit(Diag::error(
                        span,
                        DiagCode::ArityMismatch,
                        format!(
                            "call to '{}' has wrong arity: expected {} argument(s), got {}",
                            name_str,
                            sig.params.len(),
                            args.len(),
                        ),
                    ));
                } else {
                    for (idx, (arg, &expected)) in args.iter().zip(sig.params.iter()).enumerate() {
                        let actual = arg.expect_ty();
                        if !pool.compatible(actual, expected) {
                            sink.emit(Diag::error(
                                arg.span,
                                DiagCode::TypeMismatch,
                                format!(
                                    "call to '{}': argument {} has type '{}', expected '{}'",
                                    name_str,
                                    idx + 1,
                                    pool.display(actual),
                                    pool.display(expected),
                                ),
                            ));
                        }
                    }
                }
                sig.return_type
            } else {
                sink.emit(Diag::error(
                    span,
                    DiagCode::UndefinedFunction,
                    format!("undefined function: '{}'", name_str),
                ));
                pool.error_type()
            }
        }
    };
    expr.ty = Some(ty);
}

/// Front-end validation for builtin calls.
///
/// These checks are builtin-specific and temporary: once `print`
/// moves to a runtime crate and is called through a normal
/// signature (see ISSUES.md I-006), they go away.
fn check_builtin_call(name: &str, args: &[HirExpr], span: Span, sink: &mut DiagSink) {
    if name == "print" {
        if args.len() != 1 {
            sink.emit(Diag::error(
                span,
                DiagCode::ArityMismatch,
                format!("print() takes exactly 1 argument, got {}", args.len()),
            ));
            return;
        }
        if !matches!(args[0].kind, HirExprKind::StrLiteral(_)) {
            sink.emit(Diag::error(
                args[0].span,
                DiagCode::BuiltinArgKind,
                "print() argument must be a string literal",
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast_lower;
    use crate::lexer::lex;
    use crate::parser::program_parser;
    use chumsky::Parser;
    use chumsky::input::{Input, Stream};

    fn run(input: &str) -> Result<(HirProgram, InternPool), Vec<Diag>> {
        let mut pool = InternPool::new();
        let tokens = lex(input, &mut pool).expect("lex ok");
        let token_stream =
            Stream::from_iter(tokens).map((0..input.len()).into(), |(t, s): (_, _)| (t, s));
        let program = program_parser()
            .parse(token_stream)
            .into_result()
            .expect("parse ok");

        let mut sink = DiagSink::new();
        let mut hir = ast_lower::lower(&program, &mut pool, &mut sink);
        if sink.has_errors() {
            return Err(sink.into_diags());
        }
        analyze(&mut hir, &mut pool, &mut sink);
        if sink.has_errors() {
            Err(sink.into_diags())
        } else {
            Ok((hir, pool))
        }
    }

    fn first_msg(diags: &[Diag]) -> &str {
        &diags[0].message
    }

    fn any_code(diags: &[Diag], code: DiagCode) -> bool {
        diags.iter().any(|d| d.code == code)
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
        let diags = run("x = y").unwrap_err();
        assert!(any_code(&diags, DiagCode::UndefinedVariable));
    }

    #[test]
    fn undefined_function_rejected() {
        let diags = run("x = not_a_fn()").unwrap_err();
        assert!(any_code(&diags, DiagCode::UndefinedFunction));
    }

    #[test]
    fn sema_continues_past_first_error_and_collects_multiple() {
        // Two independent undefined variables in one function.
        let diags = run("a = x\nb = y\n").unwrap_err();
        let undefs = diags
            .iter()
            .filter(|d| d.code == DiagCode::UndefinedVariable)
            .count();
        assert_eq!(undefs, 2, "got: {:#?}", diags);
    }

    #[test]
    fn unknown_type_does_not_cascade() {
        let diags = run("x: nope = 1").unwrap_err();
        assert!(any_code(&diags, DiagCode::UnknownType));
        assert!(
            !any_code(&diags, DiagCode::TypeMismatch),
            "unexpected cascade: {:#?}",
            diags
        );
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
        let diags = run("x = 1 == true").unwrap_err();
        assert!(any_code(&diags, DiagCode::TypeMismatch));
        assert!(first_msg(&diags).contains("type mismatch in '=='"));
    }

    #[test]
    fn string_equality_rejected() {
        let diags = run("x = \"a\" == \"b\"").unwrap_err();
        assert!(any_code(&diags, DiagCode::UnsupportedOperator));
        assert!(
            first_msg(&diags).contains("not supported for type 'str'")
                && first_msg(&diags).contains("(yet)")
        );
    }

    #[test]
    fn bool_arithmetic_rejected() {
        let diags = run("x = true + 1").unwrap_err();
        assert!(any_code(&diags, DiagCode::TypeMismatch));
    }

    #[test]
    fn bool_arithmetic_same_type_rejected_as_unsupported_op() {
        // Both operands are bool: compatibility check passes,
        // we hit the non-equality arithmetic kind-check arm — which
        // should fire UnsupportedOperator, not TypeMismatch.
        let diags = run("x = true + false").unwrap_err();
        assert!(
            any_code(&diags, DiagCode::UnsupportedOperator),
            "expected UnsupportedOperator, got: {:#?}",
            diags
        );
        assert!(
            !any_code(&diags, DiagCode::TypeMismatch),
            "unexpected TypeMismatch: {:#?}",
            diags
        );
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
        let diags = run("x = \"hi\"\n_ = print(x)").unwrap_err();
        assert!(any_code(&diags, DiagCode::BuiltinArgKind));
    }

    #[test]
    fn print_arity_rejected_in_sema() {
        let diags = run("_ = print(\"a\", \"b\")").unwrap_err();
        assert!(any_code(&diags, DiagCode::ArityMismatch));
    }

    #[test]
    fn return_type_mismatch_rejected() {
        let diags = run("fn main() -> int:\n\treturn \"hello\"\n").unwrap_err();
        assert!(any_code(&diags, DiagCode::TypeMismatch));
        let m = first_msg(&diags);
        assert!(m.contains("return type mismatch") && m.contains("'int'") && m.contains("'str'"));
    }

    #[test]
    fn missing_return_value_rejected() {
        let diags = run("fn f() -> int:\n\treturn\n\nfn main() -> int:\n\treturn 0\n").unwrap_err();
        assert!(first_msg(&diags).contains("missing return value"));
    }

    #[test]
    fn return_value_from_void_function_rejected() {
        let diags = run("fn g():\n\treturn 1\n\nfn main() -> int:\n\treturn 0\n").unwrap_err();
        assert!(
            first_msg(&diags)
                .contains("cannot return a value from a function with return type 'void'")
        );
    }

    #[test]
    fn call_arity_mismatch_rejected() {
        let code = "fn add(a: int, b: int) -> int:\n\treturn a + b\n\nfn main() -> int:\n\treturn add(1, 2, 3)\n";
        let diags = run(code).unwrap_err();
        assert!(any_code(&diags, DiagCode::ArityMismatch));
        let m = first_msg(&diags);
        assert!(m.contains("wrong arity") && m.contains("expected 2") && m.contains("got 3"));
    }

    #[test]
    fn call_argument_type_mismatch_rejected() {
        let code = "fn f(a: int) -> int:\n\treturn a\n\nfn main() -> int:\n\treturn f(true)\n";
        let diags = run(code).unwrap_err();
        assert!(any_code(&diags, DiagCode::TypeMismatch));
        let m = first_msg(&diags);
        assert!(m.contains("argument 1") && m.contains("'bool'") && m.contains("'int'"));
    }

    #[test]
    fn vardecl_annotation_initializer_mismatch_rejected() {
        let diags = run("x: int = \"hello\"").unwrap_err();
        assert!(any_code(&diags, DiagCode::TypeMismatch));
        let m = first_msg(&diags);
        assert!(
            m.contains("type mismatch")
                && m.contains("'x'")
                && m.contains("'int'")
                && m.contains("'str'")
        );
    }

    #[test]
    fn neg_on_bool_rejected() {
        let diags = run("x = -true").unwrap_err();
        assert!(any_code(&diags, DiagCode::UnsupportedOperator));
        let m = first_msg(&diags);
        assert!(m.contains("unary operator '-'") && m.contains("'bool'"));
    }

    #[test]
    fn nested_expression_types_all_filled() {
        let (hir, _) = run("x = (1 + 2) * -3").unwrap();
        assert_all_expr_types_resolved(&hir);
    }
}
