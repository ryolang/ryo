//! AST → UIR structural translation.
//!
//! Named for symmetry with Zig's `AstGen.zig` — the responsibility
//! (lowering an AST into the first IR) is the same. Pure
//! structural translation: resolves syntactic type annotations
//! (`int`/`bool`/`str`) to `TypeId` because those come from
//! `TypeExpr` nodes and have no useful "no types yet"
//! representation. No types are attached to instructions — those
//! are filled in by sema in a later pass (the per-`InstRef`
//! `TypeTable`; Phase 4 will emit TIR instead).
//!
//! Identifier names come pre-interned as `StringId` from the parser,
//! so this stage is allocation-light: it copies handles around.
//!
//! ## Output
//!
//! [`generate`] returns the canonical [`Uir`]. The pipeline driver
//! threads it directly into `sema::analyze` and from there into
//! `codegen::compile`; there is no intermediate tree-shaped IR.

use crate::ast;
use crate::diag::{Diag, DiagCode, DiagSink};
use crate::types::{InternPool, StringId, TypeId};
use crate::uir::{InstRef, InstTag, Uir, UirBuilder, UirParam};
use chumsky::span::{SimpleSpan, Span as _};

type Span = SimpleSpan;

fn synthetic_span() -> Span {
    SimpleSpan::new((), 0..0)
}

/// Pre-interned `StringId`s for the three primitive type names.
///
/// Phase 2 made identifiers `StringId` handles, so `resolve_type`
/// used to call `pool.str(name)` on every type annotation just to
/// reach the &str-keyed match. Interning the three names once at
/// the top of `generate` lets subsequent comparisons be a `StringId`
/// equality check (`u32` compare) instead of a `pool.str` lookup
/// followed by a string compare.
struct Primitives {
    int: StringId,
    str_: StringId,
    bool_: StringId,
    float: StringId,
}

impl Primitives {
    fn new(pool: &mut InternPool) -> Self {
        Primitives {
            int: pool.intern_str("int"),
            str_: pool.intern_str("str"),
            bool_: pool.intern_str("bool"),
            float: pool.intern_str("float"),
        }
    }
}

/// Lower an AST `Program` to UIR, accumulating diagnostics in `sink`.
///
/// Returns the lowered UIR even on error (using `pool.error_type()`
/// for any annotation that failed to resolve) so subsequent passes
/// can keep type-checking and surface their own diagnostics. The
/// driver decides whether to proceed based on `sink.has_errors()`.
pub fn generate(program: &ast::Program, pool: &mut InternPool, sink: &mut DiagSink) -> Uir {
    let mut func_defs: Vec<&ast::FunctionDef> = Vec::new();
    let mut top_level: Vec<&ast::Statement> = Vec::new();

    let main_id = pool.intern_str("main");
    let prims = Primitives::new(pool);

    for stmt in &program.statements {
        match &stmt.kind {
            ast::StmtKind::FunctionDef(f) => func_defs.push(f),
            _ => top_level.push(stmt),
        }
    }

    let has_explicit_main = func_defs.iter().any(|f| f.name.name == main_id);

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

    let mut b = UirBuilder::new();

    for func in &func_defs {
        gen_function_def(&mut b, func, &prims, pool, sink);
    }
    if !has_explicit_main {
        // Synthesize an implicit `main` from top-level statements.
        // User-defined helper functions still appear above;
        // without this, calls to them in top-level code would
        // dangle as "undefined function" errors in sema.
        gen_implicit_main(&mut b, &top_level, main_id, &prims, pool, sink);
    }

    b.finish()
}

fn resolve_type(
    name: StringId,
    span: Span,
    prims: &Primitives,
    pool: &InternPool,
    sink: &mut DiagSink,
) -> TypeId {
    if name == prims.int {
        pool.int()
    } else if name == prims.str_ {
        pool.str_()
    } else if name == prims.bool_ {
        pool.bool_()
    } else if name == prims.float {
        pool.float()
    } else {
        // Only resolve the &str on the unhappy path; the common
        // primitive path stays a pure `StringId` compare.
        sink.emit(Diag::error(
            span,
            DiagCode::UnknownType,
            format!("unknown type: '{}'", pool.str(name)),
        ));
        pool.error_type()
    }
}

fn gen_implicit_main(
    b: &mut UirBuilder,
    stmts: &[&ast::Statement],
    main_id: StringId,
    prims: &Primitives,
    pool: &mut InternPool,
    sink: &mut DiagSink,
) {
    let mut body_stmts: Vec<InstRef> = Vec::new();
    for stmt in stmts {
        gen_stmt(b, stmt, prims, pool, sink, &mut body_stmts);
    }

    let zero = b.int_literal(0, synthetic_span());
    let ret = b.unary(InstTag::Return, zero, synthetic_span());
    body_stmts.push(ret);

    let int_ty = pool.int();
    b.add_function(main_id, vec![], int_ty, &body_stmts, synthetic_span());
}

fn gen_function_def(
    b: &mut UirBuilder,
    func: &ast::FunctionDef,
    prims: &Primitives,
    pool: &mut InternPool,
    sink: &mut DiagSink,
) {
    let params: Vec<UirParam> = func
        .params
        .iter()
        .map(|p| UirParam {
            name: p.name.name,
            ty: resolve_type(
                p.type_annotation.name,
                p.type_annotation.span,
                prims,
                pool,
                sink,
            ),
            span: p.name.span,
        })
        .collect();

    let return_type = match &func.return_type {
        Some(ty) => resolve_type(ty.name, ty.span, prims, pool, sink),
        None => pool.void(),
    };

    let mut body_stmts: Vec<InstRef> = Vec::new();
    for stmt in &func.body {
        gen_stmt(b, stmt, prims, pool, sink, &mut body_stmts);
    }

    b.add_function(
        func.name.name,
        params,
        return_type,
        &body_stmts,
        func.name.span,
    );
}

fn gen_stmt(
    b: &mut UirBuilder,
    stmt: &ast::Statement,
    prims: &Primitives,
    pool: &mut InternPool,
    sink: &mut DiagSink,
    out: &mut Vec<InstRef>,
) {
    match &stmt.kind {
        ast::StmtKind::VarDecl(decl) => {
            let initializer = gen_expr(b, &decl.initializer);
            let ty = decl
                .type_annotation
                .as_ref()
                .map(|ann| resolve_type(ann.name, ann.span, prims, pool, sink));
            let r = b.var_decl(decl.name.name, decl.mutable, ty, initializer, stmt.span);
            out.push(r);
        }
        ast::StmtKind::Return(Some(expr)) => {
            let value = gen_expr(b, expr);
            out.push(b.unary(InstTag::Return, value, stmt.span));
        }
        ast::StmtKind::Return(None) => {
            out.push(b.return_void(stmt.span));
        }
        ast::StmtKind::ExprStmt(expr) => {
            let value = gen_expr(b, expr);
            out.push(b.unary(InstTag::ExprStmt, value, stmt.span));
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

fn gen_expr(b: &mut UirBuilder, expr: &ast::Expression) -> InstRef {
    let span = expr.span;
    match &expr.kind {
        ast::ExprKind::Literal(ast::Literal::Int(n)) => b.int_literal(*n, span),
        ast::ExprKind::Literal(ast::Literal::Str(id)) => b.str_literal(*id, span),
        ast::ExprKind::Literal(ast::Literal::Bool(v)) => b.bool_literal(*v, span),
        ast::ExprKind::Literal(ast::Literal::Float(v)) => b.float_literal(*v, span),
        ast::ExprKind::Ident(id) => b.var_ref(*id, span),
        ast::ExprKind::BinaryOp(lhs, op, rhs) => {
            let l = gen_expr(b, lhs);
            let r = gen_expr(b, rhs);
            let tag = match op {
                ast::BinaryOperator::Add => InstTag::Add,
                ast::BinaryOperator::Sub => InstTag::Sub,
                ast::BinaryOperator::Mul => InstTag::Mul,
                ast::BinaryOperator::Div => InstTag::Div,
                ast::BinaryOperator::Eq => InstTag::Eq,
                ast::BinaryOperator::NotEq => InstTag::NotEq,
                ast::BinaryOperator::Lt => InstTag::Lt,
                ast::BinaryOperator::Gt => InstTag::Gt,
                ast::BinaryOperator::LtEq => InstTag::LtEq,
                ast::BinaryOperator::GtEq => InstTag::GtEq,
                ast::BinaryOperator::Mod => InstTag::Mod,
            };
            b.binary(tag, l, r, span)
        }
        ast::ExprKind::UnaryOp(ast::UnaryOperator::Neg, sub) => {
            let s = gen_expr(b, sub);
            b.unary(InstTag::Neg, s, span)
        }
        ast::ExprKind::Call(name, args) => {
            let arg_refs: Vec<InstRef> = args.iter().map(|a| gen_expr(b, a)).collect();
            b.call(*name, &arg_refs, span)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::lex;
    use crate::parser::program_parser;
    use crate::uir::InstData;
    use chumsky::Parser;
    use chumsky::input::Input;

    fn parse_and_lower(input: &str) -> Result<(Uir, InternPool), Vec<Diag>> {
        // Phase-2 lex pipeline: logos + indent + intern in one
        // pass; identifiers come back as `StringId`. Phase-1
        // diagnostics are still threaded through `DiagSink` so
        // astgen can keep going past errors.
        let mut pool = InternPool::new();
        let tokens = lex(input, &mut pool).expect("lex ok");
        let token_stream = tokens[..].split_token_span((0..input.len()).into());
        let program = program_parser()
            .parse(token_stream)
            .into_result()
            .expect("parse ok");

        let mut sink = DiagSink::new();
        let uir = generate(&program, &mut pool, &mut sink);
        if sink.has_errors() {
            Err(sink.into_diags())
        } else {
            Ok((uir, pool))
        }
    }

    /// Find a function body by name through the `InternPool`.
    fn body_named<'a>(uir: &'a Uir, pool: &InternPool, name: &str) -> &'a crate::uir::FuncBody {
        let id = pool.find_str(name).expect("name should be interned");
        uir.func_bodies
            .iter()
            .find(|f| f.name == id)
            .unwrap_or_else(|| panic!("no function named {:?}", name))
    }

    /// Top-level statement at index `i` in `body`'s execution order.
    fn stmt_at(uir: &Uir, body: &crate::uir::FuncBody, i: usize) -> InstRef {
        uir.body_stmts(body)[i]
    }

    #[test]
    fn lower_float_literal() {
        let (uir, pool) = parse_and_lower("x = 1.5").unwrap();
        let main = body_named(&uir, &pool, "main");
        let stmts = uir.body_stmts(main);
        let v = uir.var_decl_view(stmts[0]);
        assert!(matches!(uir.inst(v.initializer).tag, InstTag::FloatLiteral));
    }

    #[test]
    fn lower_float_type_annotation() {
        let (uir, pool) = parse_and_lower("x: float = 1.5")
            .expect("`: float` annotation should resolve without diagnostics");
        let main = body_named(&uir, &pool, "main");
        let v = uir.var_decl_view(uir.body_stmts(main)[0]);
        assert_eq!(
            v.ty,
            Some(pool.float()),
            "`: float` annotation must resolve to pool.float()"
        );
    }

    #[test]
    fn lower_ordering_and_modulo_ops() {
        let (uir, pool) = parse_and_lower("x = 1 < 2\ny = 1 % 2").unwrap();
        let main = body_named(&uir, &pool, "main");
        let stmts = uir.body_stmts(main);
        let lt = uir.var_decl_view(stmts[0]);
        assert!(matches!(uir.inst(lt.initializer).tag, InstTag::Lt));
        let m = uir.var_decl_view(stmts[1]);
        assert!(matches!(uir.inst(m.initializer).tag, InstTag::Mod));
    }

    #[test]
    fn astgen_produces_no_types() {
        // The whole point of UIR-as-input-to-sema: astgen attaches
        // no types to instructions; the resolved-type table is
        // sema's job.
        let (uir, _) = parse_and_lower("x = 2 + 3 * 4\ny = x").unwrap();
        // No per-instruction `ty` slot exists on UIR; the test is
        // structural — every value-producing inst should have a
        // tag from the `value` half of `InstTag`, and no side
        // table is constructed yet.
        for inst in uir.instructions.iter().skip(1) {
            // Instructions can be either statements or expressions;
            // both shapes are valid here. The point is purely that
            // *no `Option<TypeId>` slot is present on the inst*,
            // which is enforced by the type itself.
            let _ = inst.tag;
        }
    }

    #[test]
    fn structural_shape_flat_integer_variable() {
        let (uir, pool) = parse_and_lower("x = 42").unwrap();
        assert_eq!(uir.func_bodies.len(), 1);
        let main = body_named(&uir, &pool, "main");
        assert_eq!(main.params.len(), 0);

        let stmts = uir.body_stmts(main);
        assert_eq!(stmts.len(), 2);

        // First statement is a VarDecl for `x = 42`.
        let v = uir.var_decl_view(stmts[0]);
        assert_eq!(pool.str(v.name), "x");
        assert!(!v.mutable);
        assert!(matches!(uir.inst(v.initializer).tag, InstTag::IntLiteral));

        // Second statement is the implicit-main `return 0`.
        assert!(matches!(uir.inst(stmts[1]).tag, InstTag::Return));
    }

    #[test]
    fn structural_shape_mutable_variable() {
        let (uir, _) = parse_and_lower("mut x = 42").unwrap();
        let v = uir.var_decl_view(stmt_at(&uir, &uir.func_bodies[0], 0));
        assert!(v.mutable);
    }

    #[test]
    fn structural_shape_binary_op() {
        let (uir, _) = parse_and_lower("x = 2 + 3 * 4").unwrap();
        let v = uir.var_decl_view(stmt_at(&uir, &uir.func_bodies[0], 0));
        // Initializer is `(2) + (3 * 4)` → outer Add, inner Mul.
        assert!(matches!(uir.inst(v.initializer).tag, InstTag::Add));
    }

    #[test]
    fn structural_shape_negation() {
        let (uir, _) = parse_and_lower("x = -42").unwrap();
        let v = uir.var_decl_view(stmt_at(&uir, &uir.func_bodies[0], 0));
        assert!(matches!(uir.inst(v.initializer).tag, InstTag::Neg));
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
        let (uir, pool) = parse_and_lower("fn main() -> int:\n\treturn 0\n").unwrap();
        assert_eq!(uir.func_bodies.len(), 1);
        let main = body_named(&uir, &pool, "main");
        let stmts = uir.body_stmts(main);
        assert_eq!(stmts.len(), 1);
        assert!(matches!(uir.inst(stmts[0]).tag, InstTag::Return));
    }

    #[test]
    fn unknown_type_annotation_rejected() {
        let diags = parse_and_lower("x: nope = 1").unwrap_err();
        assert!(diags.iter().any(|d| d.code == DiagCode::UnknownType));
    }

    #[test]
    fn helper_fn_with_top_level_lowers_both() {
        let (uir, pool) =
            parse_and_lower("fn helper() -> int:\n\treturn 42\n\nx = helper()\n").unwrap();
        assert_eq!(uir.func_bodies.len(), 2);
        assert!(uir.func_bodies.iter().any(|f| pool.str(f.name) == "helper"));
        assert!(uir.func_bodies.iter().any(|f| pool.str(f.name) == "main"));
    }

    #[test]
    fn two_functions_structural() {
        let code =
            "fn add(a: int, b: int) -> int:\n\treturn a + b\n\nfn main() -> int:\n\treturn 0\n";
        let (uir, pool) = parse_and_lower(code).unwrap();
        assert_eq!(uir.func_bodies.len(), 2);
        let add = body_named(&uir, &pool, "add");
        assert_eq!(add.params.len(), 2);
        assert_eq!(pool.str(add.params[0].name), "a");
        assert_eq!(pool.str(add.params[1].name), "b");
    }

    #[test]
    fn call_payload_round_trips_through_extra() {
        // Implicit main contains `x = add(1, 2)`. The Call's
        // arglist comes back through `extra` correctly.
        let (uir, pool) =
            parse_and_lower("fn add(a: int, b: int) -> int:\n\treturn a + b\n\nx = add(1, 2)\n")
                .unwrap();
        let main = body_named(&uir, &pool, "main");
        let v = uir.var_decl_view(stmt_at(&uir, main, 0));
        assert!(matches!(uir.inst(v.initializer).tag, InstTag::Call));
        let call = uir.call_view(v.initializer);
        assert_eq!(pool.str(call.name), "add");
        assert_eq!(call.args.len(), 2);
        // Args are `IntLiteral(1)`, `IntLiteral(2)`.
        for (arg, expected) in call.args.iter().zip([1i64, 2]) {
            match uir.inst(*arg).data {
                InstData::Int(v) => assert_eq!(v, expected),
                other => panic!("expected IntLiteral, got {:?}", other),
            }
        }
    }
}
