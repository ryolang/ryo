//! AST → UIR structural translation.
//!
//! Renamed from `ast_lower` for symmetry with Zig's `AstGen.zig` —
//! the responsibility (lowering an AST into the first IR) is the
//! same. Pure structural translation: resolves syntactic type
//! annotations (`int`/`bool`/`str`) to `TypeId` because those come
//! from `TypeExpr` nodes and have no useful "no types yet"
//! representation. No types are attached to instructions — those are
//! filled in by sema in a later pass (Phase 3 stores them in a
//! sidecar; Phase 4 emits TIR).
//!
//! Identifier names come pre-interned as `StringId` from the parser,
//! so this stage is allocation-light: it copies handles around.
//!
//! ## Two outputs during the transition
//!
//! [`generate`] returns the canonical [`Uir`]. While sema and codegen
//! still consume HIR (commits 3 and 4 of the Phase 3 plan replace
//! them), the driver also calls [`uir_to_hir`] to reconstruct an
//! equivalent [`HirProgram`]. That shim is the only thing keeping
//! the pipeline working end-to-end during the cutover; commit 5
//! deletes it together with `src/hir.rs`.

use crate::ast;
use crate::diag::{Diag, DiagCode, DiagSink};
use crate::types::{InternPool, StringId, TypeId};
use crate::uir::{InstRef, InstTag, Uir, UirBuilder, UirParam};
#[cfg(test)]
use crate::{
    hir,
    uir::{FuncBody, InstData},
};
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
}

impl Primitives {
    fn new(pool: &mut InternPool) -> Self {
        Primitives {
            int: pool.intern_str("int"),
            str_: pool.intern_str("str"),
            bool_: pool.intern_str("bool"),
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

// ---------- Temporary UIR → HIR shim (test-only) ----------
//
// Reconstructs the legacy tree-shaped `HirProgram` from a freshly
// emitted `Uir` plus a sema-produced `types` side-table indexed by
// [`InstRef`]. Production codegen consumes UIR + the side-table
// directly (Phase 3 commit 4); this shim survives only so astgen
// and sema unit tests can keep matching against `HirStmt` /
// `HirExpr` shapes while the cutover settles. Commit 5 deletes
// the entire shim along with `src/hir.rs`.
//
// This is not a fast path — it allocates a fresh `HirExpr`/`HirStmt`
// tree for every function. That's fine: tests aren't hot.

/// Rebuild a `HirProgram` from `uir` plus a `types` side-table
/// produced by `sema::analyze`. `types[r.index()]` is the resolved
/// type of the instruction at [`InstRef`] `r`; entries of `None`
/// flow through to `HirExpr.ty = None` (used by astgen tests to
/// assert astgen alone leaves expressions untyped).
///
/// `VarDecl.ty` reads `types[var_decl_inst.index()]` (which sema
/// fills with the post-annotation/inference resolved type). Other
/// `HirExpr.ty` slots read `types[expr_inst.index()]`.
#[cfg(test)]
pub fn uir_to_hir_typed(uir: &Uir, types: &[Option<TypeId>]) -> hir::HirProgram {
    uir_to_hir_with_types(uir, types)
}

#[cfg(test)]
fn uir_to_hir_with_types(uir: &Uir, types: &[Option<TypeId>]) -> hir::HirProgram {
    let functions = uir
        .func_bodies
        .iter()
        .map(|fb| body_to_hir(uir, fb, types))
        .collect();
    hir::HirProgram { functions }
}

#[cfg(test)]
fn body_to_hir(uir: &Uir, body: &FuncBody, types: &[Option<TypeId>]) -> hir::HirFunction {
    let params = body
        .params
        .iter()
        .map(|p| hir::HirParam {
            name: p.name,
            ty: p.ty,
        })
        .collect();
    let stmts = uir
        .body_stmts(body)
        .into_iter()
        .map(|r| stmt_to_hir(uir, r, types))
        .collect();
    hir::HirFunction {
        name: body.name,
        params,
        return_type: body.return_type,
        body: stmts,
    }
}

#[cfg(test)]
fn stmt_to_hir(uir: &Uir, r: InstRef, types: &[Option<TypeId>]) -> hir::HirStmt {
    let inst = uir.inst(r);
    let span = uir.span(r);
    match (inst.tag, inst.data) {
        (InstTag::VarDecl, InstData::Extra(_)) => {
            let view = uir.var_decl_view(r);
            // Prefer the sema-resolved type stored in the
            // side-table at the VarDecl's own slot (which reconciles
            // annotation vs. inference). When the table entry is
            // `None` (astgen-only test path), fall back to whatever
            // was annotated in source — there's nothing else to use.
            let ty = types.get(r.index()).copied().flatten().or(view.ty);
            hir::HirStmt::VarDecl {
                name: view.name,
                mutable: view.mutable,
                ty,
                initializer: expr_to_hir(uir, view.initializer, types),
                span,
            }
        }
        (InstTag::Return, InstData::UnOp(operand)) => {
            hir::HirStmt::Return(Some(expr_to_hir(uir, operand, types)), span)
        }
        (InstTag::ReturnVoid, _) => hir::HirStmt::Return(None, span),
        (InstTag::ExprStmt, InstData::UnOp(operand)) => {
            hir::HirStmt::Expr(expr_to_hir(uir, operand, types), span)
        }
        (tag, data) => panic!(
            "uir_to_hir: instruction at %{} is not a statement (tag={:?}, data={:?})",
            r.index(),
            tag,
            data
        ),
    }
}

#[cfg(test)]
fn expr_to_hir(uir: &Uir, r: InstRef, types: &[Option<TypeId>]) -> hir::HirExpr {
    let inst = uir.inst(r);
    let span = uir.span(r);
    let kind = match (inst.tag, inst.data) {
        (InstTag::IntLiteral, InstData::Int(v)) => hir::HirExprKind::IntLiteral(v),
        (InstTag::StrLiteral, InstData::Str(s)) => hir::HirExprKind::StrLiteral(s),
        (InstTag::BoolLiteral, InstData::Bool(v)) => hir::HirExprKind::BoolLiteral(v),
        (InstTag::Var, InstData::Var(s)) => hir::HirExprKind::Var(s),
        (InstTag::Neg, InstData::UnOp(operand)) => hir::HirExprKind::UnaryOp(
            hir::UnaryOp::Neg,
            Box::new(expr_to_hir(uir, operand, types)),
        ),
        (op, InstData::BinOp { lhs, rhs })
            if matches!(
                op,
                InstTag::Add
                    | InstTag::Sub
                    | InstTag::Mul
                    | InstTag::Div
                    | InstTag::Eq
                    | InstTag::NotEq
            ) =>
        {
            let hir_op = match op {
                InstTag::Add => hir::BinaryOp::Add,
                InstTag::Sub => hir::BinaryOp::Sub,
                InstTag::Mul => hir::BinaryOp::Mul,
                InstTag::Div => hir::BinaryOp::Div,
                InstTag::Eq => hir::BinaryOp::Eq,
                InstTag::NotEq => hir::BinaryOp::NotEq,
                _ => unreachable!(),
            };
            hir::HirExprKind::BinaryOp(
                Box::new(expr_to_hir(uir, lhs, types)),
                hir_op,
                Box::new(expr_to_hir(uir, rhs, types)),
            )
        }
        (InstTag::Call, InstData::Extra(_)) => {
            let view = uir.call_view(r);
            hir::HirExprKind::Call(
                view.name,
                view.args
                    .into_iter()
                    .map(|a| expr_to_hir(uir, a, types))
                    .collect(),
            )
        }
        (tag, data) => panic!(
            "uir_to_hir: instruction at %{} is not an expression (tag={:?}, data={:?})",
            r.index(),
            tag,
            data
        ),
    };
    let ty = types.get(r.index()).copied().flatten();
    hir::HirExpr { kind, ty, span }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hir::{HirExpr, HirExprKind, HirProgram, HirStmt};
    use crate::lexer::lex;
    use crate::parser::program_parser;
    use chumsky::Parser;
    use chumsky::input::{Input, Stream};

    fn parse_and_lower(input: &str) -> Result<(HirProgram, InternPool), Vec<Diag>> {
        // Phase-2 lex pipeline: logos + indent + intern in one
        // pass; identifiers come back as `StringId`. Phase-1
        // diagnostics are still threaded through `DiagSink` so
        // astgen can keep going past errors.
        let mut pool = InternPool::new();
        let tokens = lex(input, &mut pool).expect("lex ok");
        let token_stream =
            Stream::from_iter(tokens).map((0..input.len()).into(), |(t, s): (_, _)| (t, s));
        let program = program_parser()
            .parse(token_stream)
            .into_result()
            .expect("parse ok");

        let mut sink = DiagSink::new();
        let uir = generate(&program, &mut pool, &mut sink);
        if sink.has_errors() {
            return Err(sink.into_diags());
        }
        // Empty types table — astgen tests assert that astgen alone
        // leaves every expression's `ty` unresolved. Sema fills the
        // table in production; see sema's tests for the typed shim
        // exercising real type resolution.
        let types: Vec<Option<TypeId>> = vec![None; uir.instructions.len()];
        let hir = uir_to_hir_typed(&uir, &types);
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
        assert!(e.ty.is_none(), "astgen must leave HirExpr.ty = None");
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
    fn astgen_leaves_expression_types_unresolved() {
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
                HirExprKind::BinaryOp(_, hir::BinaryOp::Add, _)
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
                HirExprKind::UnaryOp(hir::UnaryOp::Neg, _)
            )),
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
        let (hir, pool) = parse_and_lower("fn main() -> int:\n\treturn 0\n").unwrap();
        assert_eq!(hir.functions.len(), 1);
        let main = &hir.functions[0];
        assert_eq!(pool.str(main.name), "main");
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

    #[test]
    fn uir_round_trip_call_payload() {
        // Sanity-check the UIR → HIR shim on a function with a call.
        let mut pool = InternPool::new();
        let tokens = lex(
            "fn add(a: int, b: int) -> int:\n\treturn a + b\n\nx = add(1, 2)\n",
            &mut pool,
        )
        .unwrap();
        let stream = Stream::from_iter(tokens).map((0..0usize).into(), |(t, s): (_, _)| (t, s));
        let program = program_parser().parse(stream).into_result().unwrap();
        let mut sink = DiagSink::new();
        let uir = generate(&program, &mut pool, &mut sink);
        assert!(!sink.has_errors());

        // Walk: implicit main contains a VarDecl whose initializer
        // is a Call(add, [1, 2]).
        let main_body = uir
            .func_bodies
            .iter()
            .find(|f| pool.str(f.name) == "main")
            .unwrap();
        let stmts = uir.body_stmts(main_body);
        let var_decl = uir.var_decl_view(stmts[0]);
        let init = uir.inst(var_decl.initializer);
        assert!(matches!(init.tag, InstTag::Call));
        let call = uir.call_view(var_decl.initializer);
        assert_eq!(pool.str(call.name), "add");
        assert_eq!(call.args.len(), 2);
    }
}
