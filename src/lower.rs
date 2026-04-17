use crate::ast;
use crate::hir::*;
use chumsky::span::{SimpleSpan, Span as _};
use std::collections::HashMap;

struct FunctionSig {
    return_type: Type,
}

struct Scope<'a> {
    parent: Option<&'a Scope<'a>>,
    bindings: HashMap<String, Type>,
}

impl<'a> Scope<'a> {
    fn new() -> Self {
        Scope {
            parent: None,
            bindings: HashMap::new(),
        }
    }

    fn child(parent: &'a Scope<'a>) -> Self {
        Scope {
            parent: Some(parent),
            bindings: HashMap::new(),
        }
    }

    fn insert(&mut self, name: String, ty: Type) {
        self.bindings.insert(name, ty);
    }

    fn lookup(&self, name: &str) -> Option<Type> {
        self.bindings
            .get(name)
            .copied()
            .or_else(|| self.parent?.lookup(name))
    }
}

fn synthetic_span() -> Span {
    SimpleSpan::new((), 0..0)
}

pub fn lower(program: &ast::Program) -> Result<HirProgram, String> {
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
        return Err("Top-level statements are not allowed when fn main() is defined".to_string());
    }

    let mut signatures: HashMap<String, FunctionSig> = HashMap::new();
    for func in &func_defs {
        let return_type = match &func.return_type {
            Some(ty) => resolve_type(&ty.name)?,
            None => Type::Void,
        };
        signatures.insert(func.name.name.clone(), FunctionSig { return_type });
    }
    if !has_explicit_main {
        signatures.insert(
            "main".to_string(),
            FunctionSig {
                return_type: Type::Int,
            },
        );
    }

    let mut functions = Vec::new();

    if has_explicit_main {
        for func in &func_defs {
            functions.push(lower_function_def(func, &signatures)?);
        }
    } else {
        functions.push(lower_implicit_main(&top_level, &signatures)?);
    }

    Ok(HirProgram { functions })
}

fn resolve_type(name: &str) -> Result<Type, String> {
    match name {
        "int" => Ok(Type::Int),
        "str" => Ok(Type::Str),
        _ => Err(format!("Unknown type: '{}'", name)),
    }
}

fn lower_implicit_main(
    stmts: &[&ast::Statement],
    signatures: &HashMap<String, FunctionSig>,
) -> Result<HirFunction, String> {
    let mut scope = Scope::new();
    let mut body = Vec::new();

    for stmt in stmts {
        lower_stmt(stmt, &mut scope, signatures, &mut body)?;
    }

    body.push(HirStmt::Return(
        Some(HirExpr {
            kind: HirExprKind::IntLiteral(0),
            ty: Type::Int,
            span: synthetic_span(),
        }),
        synthetic_span(),
    ));

    Ok(HirFunction {
        name: "main".to_string(),
        params: vec![],
        return_type: Type::Int,
        body,
    })
}

fn lower_function_def(
    func: &ast::FunctionDef,
    signatures: &HashMap<String, FunctionSig>,
) -> Result<HirFunction, String> {
    let params: Vec<HirParam> = func
        .params
        .iter()
        .map(|p| {
            Ok(HirParam {
                name: p.name.name.clone(),
                ty: resolve_type(&p.type_annotation.name)?,
            })
        })
        .collect::<Result<_, String>>()?;

    let return_type = match &func.return_type {
        Some(ty) => resolve_type(&ty.name)?,
        None => Type::Void,
    };

    let mut scope = Scope::new();
    for param in &params {
        scope.insert(param.name.clone(), param.ty);
    }

    let mut body = Vec::new();
    for stmt in &func.body {
        lower_stmt(stmt, &mut scope, signatures, &mut body)?;
    }

    Ok(HirFunction {
        name: func.name.name.clone(),
        params,
        return_type,
        body,
    })
}

fn lower_stmt(
    stmt: &ast::Statement,
    scope: &mut Scope,
    signatures: &HashMap<String, FunctionSig>,
    out: &mut Vec<HirStmt>,
) -> Result<(), String> {
    match &stmt.kind {
        ast::StmtKind::VarDecl(decl) => {
            let initializer = lower_expr(&decl.initializer, scope, signatures)?;
            let ty = match &decl.type_annotation {
                Some(ann) => resolve_type(&ann.name)?,
                None => initializer.ty,
            };
            scope.insert(decl.name.name.clone(), ty);
            out.push(HirStmt::VarDecl {
                name: decl.name.name.clone(),
                mutable: decl.mutable,
                ty,
                initializer,
                span: stmt.span,
            });
        }
        ast::StmtKind::Return(Some(expr)) => {
            let hir_expr = lower_expr(expr, scope, signatures)?;
            out.push(HirStmt::Return(Some(hir_expr), stmt.span));
        }
        ast::StmtKind::Return(None) => {
            out.push(HirStmt::Return(None, stmt.span));
        }
        ast::StmtKind::ExprStmt(expr) => {
            let hir_expr = lower_expr(expr, scope, signatures)?;
            out.push(HirStmt::Expr(hir_expr, stmt.span));
        }
        ast::StmtKind::FunctionDef(_) => {
            return Err("Nested function definitions are not supported".to_string());
        }
    }
    Ok(())
}

fn lower_expr(
    expr: &ast::Expression,
    scope: &Scope,
    signatures: &HashMap<String, FunctionSig>,
) -> Result<HirExpr, String> {
    let span = expr.span;
    match &expr.kind {
        ast::ExprKind::Literal(ast::Literal::Int(n)) => Ok(HirExpr {
            kind: HirExprKind::IntLiteral(*n),
            ty: Type::Int,
            span,
        }),
        ast::ExprKind::Literal(ast::Literal::Str(s)) => Ok(HirExpr {
            kind: HirExprKind::StrLiteral(s.clone()),
            ty: Type::Str,
            span,
        }),
        ast::ExprKind::Ident(name) => {
            let ty = scope
                .lookup(name.as_str())
                .ok_or_else(|| format!("Undefined variable: '{}'", name))?;
            Ok(HirExpr {
                kind: HirExprKind::Var(name.clone()),
                ty,
                span,
            })
        }
        ast::ExprKind::BinaryOp(lhs, op, rhs) => {
            let lhs = lower_expr(lhs, scope, signatures)?;
            let rhs = lower_expr(rhs, scope, signatures)?;
            let hir_op = match op {
                ast::BinaryOperator::Add => BinaryOp::Add,
                ast::BinaryOperator::Sub => BinaryOp::Sub,
                ast::BinaryOperator::Mul => BinaryOp::Mul,
                ast::BinaryOperator::Div => BinaryOp::Div,
            };
            Ok(HirExpr {
                kind: HirExprKind::BinaryOp(Box::new(lhs), hir_op, Box::new(rhs)),
                ty: Type::Int,
                span,
            })
        }
        ast::ExprKind::UnaryOp(ast::UnaryOperator::Neg, sub) => {
            let sub = lower_expr(sub, scope, signatures)?;
            Ok(HirExpr {
                kind: HirExprKind::UnaryOp(UnaryOp::Neg, Box::new(sub)),
                ty: Type::Int,
                span,
            })
        }
        ast::ExprKind::Call(name, args) => {
            let lowered_args: Vec<HirExpr> = args
                .iter()
                .map(|a| lower_expr(a, scope, signatures))
                .collect::<Result<_, _>>()?;

            let return_ty = if let Some(builtin) = crate::builtins::lookup(name) {
                builtin.return_type
            } else {
                signatures
                    .get(name.as_str())
                    .map(|sig| sig.return_type)
                    .ok_or_else(|| format!("Undefined function: '{}'", name))?
            };

            Ok(HirExpr {
                kind: HirExprKind::Call(name.clone(), lowered_args),
                ty: return_ty,
                span,
            })
        }
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

    fn parse_and_lower(input: &str) -> Result<HirProgram, String> {
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

        lower(&program)
    }

    #[test]
    fn lower_flat_integer_variable() {
        let hir = parse_and_lower("x = 42").unwrap();
        assert_eq!(hir.functions.len(), 1);
        let main = &hir.functions[0];
        assert_eq!(main.name, "main");
        assert_eq!(main.return_type, Type::Int);
        assert_eq!(main.params.len(), 0);
        assert_eq!(main.body.len(), 2);
        match &main.body[0] {
            HirStmt::VarDecl {
                name, ty, mutable, ..
            } => {
                assert_eq!(name, "x");
                assert_eq!(*ty, Type::Int);
                assert!(!mutable);
            }
            _ => panic!("Expected VarDecl"),
        }
        match &main.body[1] {
            HirStmt::Return(Some(_), _) => {}
            _ => panic!("Expected Return(0)"),
        }
    }

    #[test]
    fn lower_typed_variable() {
        let hir = parse_and_lower("x: int = 42").unwrap();
        let main = &hir.functions[0];
        match &main.body[0] {
            HirStmt::VarDecl { name, ty, .. } => {
                assert_eq!(name, "x");
                assert_eq!(*ty, Type::Int);
            }
            _ => panic!("Expected VarDecl"),
        }
    }

    #[test]
    fn lower_string_variable() {
        let hir = parse_and_lower("x = \"hello\"").unwrap();
        let main = &hir.functions[0];
        match &main.body[0] {
            HirStmt::VarDecl { ty, .. } => {
                assert_eq!(*ty, Type::Str);
            }
            _ => panic!("Expected VarDecl"),
        }
    }

    #[test]
    fn lower_mutable_variable() {
        let hir = parse_and_lower("mut x = 42").unwrap();
        let main = &hir.functions[0];
        match &main.body[0] {
            HirStmt::VarDecl { mutable, .. } => {
                assert!(*mutable);
            }
            _ => panic!("Expected VarDecl"),
        }
    }

    #[test]
    fn lower_arithmetic_expression() {
        let hir = parse_and_lower("x = 2 + 3 * 4").unwrap();
        let main = &hir.functions[0];
        match &main.body[0] {
            HirStmt::VarDecl { initializer, .. } => {
                assert_eq!(initializer.ty, Type::Int);
                assert!(matches!(
                    initializer.kind,
                    HirExprKind::BinaryOp(_, BinaryOp::Add, _)
                ));
            }
            _ => panic!("Expected VarDecl"),
        }
    }

    #[test]
    fn lower_negation() {
        let hir = parse_and_lower("x = -42").unwrap();
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
    fn lower_variable_reference() {
        let hir = parse_and_lower("x = 42\ny = x").unwrap();
        let main = &hir.functions[0];
        match &main.body[1] {
            HirStmt::VarDecl {
                initializer, ty, ..
            } => {
                assert!(matches!(initializer.kind, HirExprKind::Var(_)));
                assert_eq!(*ty, Type::Int);
            }
            _ => panic!("Expected VarDecl"),
        }
    }

    #[test]
    fn lower_explicit_main() {
        let hir = parse_and_lower("fn main() -> int:\n\treturn 0\n").unwrap();
        assert_eq!(hir.functions.len(), 1);
        let main = &hir.functions[0];
        assert_eq!(main.name, "main");
        assert_eq!(main.return_type, Type::Int);
        assert_eq!(main.body.len(), 1);
        assert!(matches!(main.body[0], HirStmt::Return(Some(_), _)));
    }

    #[test]
    fn lower_explicit_main_with_top_level_error() {
        let result = parse_and_lower("x = 42\n\nfn main() -> int:\n\treturn 0\n");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Top-level statements"));
    }

    #[test]
    fn lower_two_functions() {
        let code = "fn add(a: int, b: int) -> int:\n\treturn a + b\n\nfn main() -> int:\n\treturn add(2, 3)\n";
        let hir = parse_and_lower(code).unwrap();
        assert_eq!(hir.functions.len(), 2);

        let add = hir.functions.iter().find(|f| f.name == "add").unwrap();
        assert_eq!(add.params.len(), 2);
        assert_eq!(add.params[0].name, "a");
        assert_eq!(add.params[0].ty, Type::Int);
        assert_eq!(add.params[1].name, "b");
        assert_eq!(add.params[1].ty, Type::Int);
        assert_eq!(add.return_type, Type::Int);

        let main = hir.functions.iter().find(|f| f.name == "main").unwrap();
        assert_eq!(main.return_type, Type::Int);
    }

    #[test]
    fn lower_function_call_type_resolved() {
        let code =
            "fn double(x: int) -> int:\n\treturn x * 2\n\nfn main() -> int:\n\treturn double(3)\n";
        let hir = parse_and_lower(code).unwrap();
        let main = hir.functions.iter().find(|f| f.name == "main").unwrap();
        match &main.body[0] {
            HirStmt::Return(Some(expr), _) => {
                assert_eq!(expr.ty, Type::Int);
                assert!(matches!(expr.kind, HirExprKind::Call(_, _)));
            }
            _ => panic!("Expected Return with Call"),
        }
    }

    #[test]
    fn lower_void_function() {
        let code = "fn greet():\n\tprint(\"hi\")\n\nfn main() -> int:\n\tgreet()\n\treturn 0\n";
        let hir = parse_and_lower(code).unwrap();
        let greet = hir.functions.iter().find(|f| f.name == "greet").unwrap();
        assert_eq!(greet.return_type, Type::Void);
    }

    #[test]
    fn lower_print_expression_statement() {
        let hir = parse_and_lower("msg = print(\"Hello\\n\")").unwrap();
        let main = &hir.functions[0];
        match &main.body[0] {
            HirStmt::VarDecl {
                initializer, ty, ..
            } => {
                assert_eq!(*ty, Type::Int);
                assert!(matches!(initializer.kind, HirExprKind::Call(_, _)));
            }
            _ => panic!("Expected VarDecl"),
        }
    }
}
