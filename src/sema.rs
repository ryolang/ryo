//! Semantic analysis: type-check UIR and emit TIR.
//!
//! Sema consumes the flat [`Uir`] produced by `astgen` and emits one
//! [`Tir`] per function body, fully typed. Codegen consumes the
//! resulting `&[Tir]` directly — there is no intermediate
//! tree-shaped IR and no per-program side-table any more.
//!
//! ## Why a translator, not a mutator
//!
//! The Phase-3 interim shape mutated a sidecar `Vec<Option<TypeId>>`
//! indexed by [`InstRef`]. That kept Sema small at the cost of (a)
//! making "type" a partial function over UIR (some entries `None`
//! forever) and (b) giving generics / inline expansion nowhere to
//! put extra typed copies of a body. Phase 4 (this file) replaces
//! both: a fresh, dense [`Tir`] per body, and "make N typed copies
//! from one untyped body" becomes idiomatic — that's the prereq for
//! comptime + monomorphization in Phase 5.
//!
//! ## Error handling
//!
//! Sema continues past errors. When an expression's type can't be
//! determined, a [`TirTag::Unreachable`] instruction is emitted in
//! its place with `ty = pool.error_type()`, downstream type
//! comparisons treat the error sentinel as compatible with anything
//! (`InternPool::compatible`), and the diagnostic flows into the
//! shared [`DiagSink`]. The driver consults `sink.has_errors()` to
//! decide whether to proceed to codegen — codegen itself must never
//! see an `Unreachable`.

use crate::builtins;
use crate::diag::{Diag, DiagCode, DiagSink};
use crate::tir::{Tir, TirBuilder, TirParam, TirRef, TirTag};
use crate::types::{InternPool, StringId, TypeId, TypeKind};
use crate::uir::{CallView, FuncBody, InstData, InstRef, InstTag, Span, Uir, VarDeclView};
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

/// Analyze `uir` and emit one [`Tir`] per function body.
///
/// Diagnostics are accumulated into `sink`. Even when errors are
/// emitted, every function still produces a well-formed `Tir` so
/// later passes (and `--emit=tir`) have something to inspect; the
/// driver consults `sink.has_errors()` before handing the result to
/// codegen.
pub fn analyze(uir: &Uir, pool: &mut InternPool, sink: &mut DiagSink) -> Vec<Tir> {
    // Function signatures resolve eagerly so out-of-order definitions
    // and recursive calls type-check without a worklist. Phase 5
    // replaces this two-pass shape with the lazy driver.
    let mut signatures: HashMap<StringId, FunctionSig> = HashMap::new();
    for body in &uir.func_bodies {
        signatures.insert(
            body.name,
            FunctionSig {
                params: body.params.iter().map(|p| p.ty).collect(),
                return_type: body.return_type,
            },
        );
    }

    let mut tirs = Vec::with_capacity(uir.func_bodies.len());
    for body in &uir.func_bodies {
        tirs.push(analyze_function(uir, body, &signatures, pool, sink));
    }
    tirs
}

fn analyze_function(
    uir: &Uir,
    body: &FuncBody,
    signatures: &HashMap<StringId, FunctionSig>,
    pool: &mut InternPool,
    sink: &mut DiagSink,
) -> Tir {
    let mut scope = Scope::new();
    for param in &body.params {
        scope.insert(param.name, param.ty);
    }

    let params: Vec<TirParam> = body
        .params
        .iter()
        .map(|p| TirParam {
            name: p.name,
            ty: p.ty,
            span: p.span,
        })
        .collect();

    let mut fcx = FuncCtx {
        builder: TirBuilder::new(body.name, params, body.return_type, body.span),
        // Mapping from UIR `InstRef` to the TIR ref Sema emitted for
        // it inside *this* function. Kept around so a UIR inst that
        // gets visited twice (e.g. a future SSA-shaped UIR with
        // shared sub-expressions) is translated exactly once. UIR is
        // tree-shaped today so this is defensive — but it's the
        // right invariant before Phase 5 lazy sema lands.
        inst_map: vec![None; uir.instructions.len()],
        return_type: body.return_type,
    };

    let mut stmt_refs: Vec<TirRef> = Vec::with_capacity(uir.body_stmts(body).len());
    for stmt_ref in uir.body_stmts(body) {
        stmt_refs.push(analyze_stmt(
            uir, stmt_ref, &mut fcx, &mut scope, signatures, pool, sink,
        ));
    }

    fcx.builder.finish(&stmt_refs)
}

/// Per-function emission state. Lives only for the duration of one
/// `analyze_function` call; the `inst_map` and `TirBuilder` arenas
/// are scoped to a single body.
struct FuncCtx {
    builder: TirBuilder,
    inst_map: Vec<Option<TirRef>>,
    return_type: TypeId,
}

#[allow(clippy::too_many_arguments)]
fn analyze_stmt(
    uir: &Uir,
    r: InstRef,
    fcx: &mut FuncCtx,
    scope: &mut Scope,
    signatures: &HashMap<StringId, FunctionSig>,
    pool: &mut InternPool,
    sink: &mut DiagSink,
) -> TirRef {
    let inst = uir.inst(r);
    let span = uir.span(r);
    match inst.tag {
        InstTag::VarDecl => {
            let view = uir.var_decl_view(r);
            let init_tir = analyze_expr(uir, view.initializer, fcx, scope, signatures, pool, sink);
            let inferred = fcx.builder_ty(init_tir);
            let resolved = resolve_var_decl_type(&view, inferred, uir, pool, sink);
            scope.insert(view.name, resolved);
            fcx.builder
                .var_decl(view.name, view.mutable, resolved, init_tir, span)
        }
        InstTag::Return => {
            let operand = match inst.data {
                InstData::UnOp(o) => o,
                _ => unreachable!("Return must carry InstData::UnOp"),
            };
            let val_tir = analyze_expr(uir, operand, fcx, scope, signatures, pool, sink);
            let actual = fcx.builder_ty(val_tir);
            if fcx.return_type == pool.void() {
                if !pool.is_error(actual) {
                    sink.emit(Diag::error(
                        span,
                        DiagCode::TypeMismatch,
                        format!(
                            "cannot return a value from a function with return type 'void' (got '{}')",
                            pool.display(actual),
                        ),
                    ));
                }
            } else if !pool.compatible(actual, fcx.return_type) {
                sink.emit(Diag::error(
                    span,
                    DiagCode::TypeMismatch,
                    format!(
                        "return type mismatch: function expects '{}', got '{}'",
                        pool.display(fcx.return_type),
                        pool.display(actual),
                    ),
                ));
            }
            fcx.builder
                .unary(TirTag::Return, pool.void(), val_tir, span)
        }
        InstTag::ReturnVoid => {
            if fcx.return_type != pool.void() && !pool.is_error(fcx.return_type) {
                sink.emit(Diag::error(
                    span,
                    DiagCode::TypeMismatch,
                    format!(
                        "missing return value: function expects '{}'",
                        pool.display(fcx.return_type),
                    ),
                ));
            }
            fcx.builder.return_void(pool.void(), span)
        }
        InstTag::ExprStmt => {
            let operand = match inst.data {
                InstData::UnOp(o) => o,
                _ => unreachable!("ExprStmt must carry InstData::UnOp"),
            };
            let val_tir = analyze_expr(uir, operand, fcx, scope, signatures, pool, sink);
            fcx.builder
                .unary(TirTag::ExprStmt, pool.void(), val_tir, span)
        }
        other => panic!(
            "analyze_stmt: instruction at %{} is not a statement (tag={:?})",
            r.index(),
            other
        ),
    }
}

fn resolve_var_decl_type(
    view: &VarDeclView,
    inferred: TypeId,
    uir: &Uir,
    pool: &InternPool,
    sink: &mut DiagSink,
) -> TypeId {
    match view.ty {
        Some(annotated) if !pool.compatible(annotated, inferred) => {
            // Anchor the squiggle on the offending value (the
            // initializer) rather than on the whole `[mut] name [:
            // type] = expr` decl span — the type came from the
            // annotation but the *mismatch* is the initializer's
            // fault.
            sink.emit(Diag::error(
                uir.span(view.initializer),
                DiagCode::TypeMismatch,
                format!(
                    "type mismatch: '{}' annotated '{}', initializer is '{}'",
                    pool.str(view.name),
                    pool.display(annotated),
                    pool.display(inferred),
                ),
            ));
            annotated
        }
        Some(annotated) => annotated,
        None => inferred,
    }
}

#[allow(clippy::too_many_arguments)]
fn analyze_expr(
    uir: &Uir,
    r: InstRef,
    fcx: &mut FuncCtx,
    scope: &Scope,
    signatures: &HashMap<StringId, FunctionSig>,
    pool: &mut InternPool,
    sink: &mut DiagSink,
) -> TirRef {
    if let Some(t) = fcx.inst_map[r.index()] {
        return t;
    }

    let inst = uir.inst(r);
    let span = uir.span(r);
    let emitted = match inst.tag {
        InstTag::IntLiteral => match inst.data {
            InstData::Int(v) => fcx.builder.int_const(v, pool.int(), span),
            _ => unreachable!("IntLiteral must carry InstData::Int"),
        },
        InstTag::StrLiteral => match inst.data {
            InstData::Str(s) => fcx.builder.str_const(s, pool.str_(), span),
            _ => unreachable!("StrLiteral must carry InstData::Str"),
        },
        InstTag::BoolLiteral => match inst.data {
            InstData::Bool(b) => fcx.builder.bool_const(b, pool.bool_(), span),
            _ => unreachable!("BoolLiteral must carry InstData::Bool"),
        },
        InstTag::Var => {
            let name = match inst.data {
                InstData::Var(s) => s,
                _ => unreachable!("Var must carry InstData::Var"),
            };
            match scope.lookup(name) {
                Some(t) => fcx.builder.var(name, t, span),
                None => {
                    sink.emit(Diag::error(
                        span,
                        DiagCode::UndefinedVariable,
                        format!("undefined variable: '{}'", pool.str(name)),
                    ));
                    fcx.builder.unreachable(pool.error_type(), span)
                }
            }
        }
        InstTag::Add
        | InstTag::Sub
        | InstTag::Mul
        | InstTag::Div
        | InstTag::Eq
        | InstTag::NotEq => {
            let (lhs, rhs) = match inst.data {
                InstData::BinOp { lhs, rhs } => (lhs, rhs),
                _ => unreachable!("binary op must carry InstData::BinOp"),
            };
            let l = analyze_expr(uir, lhs, fcx, scope, signatures, pool, sink);
            let r2 = analyze_expr(uir, rhs, fcx, scope, signatures, pool, sink);
            let lhs_ty = fcx.builder_ty(l);
            let rhs_ty = fcx.builder_ty(r2);
            check_binary_op(inst.tag, lhs_ty, rhs_ty, l, r2, span, fcx, pool, sink)
        }
        InstTag::Neg => {
            let operand = match inst.data {
                InstData::UnOp(o) => o,
                _ => unreachable!("Neg must carry InstData::UnOp"),
            };
            let sub = analyze_expr(uir, operand, fcx, scope, signatures, pool, sink);
            let sub_ty = fcx.builder_ty(sub);
            match pool.kind(sub_ty) {
                TypeKind::Int => fcx.builder.unary(TirTag::INeg, pool.int(), sub, span),
                TypeKind::Error => fcx.builder.unreachable(pool.error_type(), span),
                _ => {
                    sink.emit(Diag::error(
                        span,
                        DiagCode::UnsupportedOperator,
                        format!(
                            "unary operator '-' not supported for type '{}'",
                            pool.display(sub_ty),
                        ),
                    ));
                    fcx.builder.unreachable(pool.error_type(), span)
                }
            }
        }
        InstTag::Call => {
            let view = uir.call_view(r);
            // Translate args first (in source order) to fix their
            // TIR refs and types, *then* validate against the
            // signature so per-argument diagnostics carry the right
            // span and the call still emits a well-formed TIR Call.
            let mut arg_tirs = Vec::with_capacity(view.args.len());
            for a in &view.args {
                arg_tirs.push(analyze_expr(uir, *a, fcx, scope, signatures, pool, sink));
            }
            check_call(uir, &view, &arg_tirs, span, fcx, signatures, pool, sink)
        }
        other => panic!(
            "analyze_expr: instruction at %{} is not an expression (tag={:?})",
            r.index(),
            other
        ),
    };

    fcx.inst_map[r.index()] = Some(emitted);
    emitted
}

#[allow(clippy::too_many_arguments)]
fn check_binary_op(
    tag: InstTag,
    lhs_ty: TypeId,
    rhs_ty: TypeId,
    lhs: TirRef,
    rhs: TirRef,
    span: Span,
    fcx: &mut FuncCtx,
    pool: &mut InternPool,
    sink: &mut DiagSink,
) -> TirRef {
    if !pool.compatible(lhs_ty, rhs_ty) {
        sink.emit(Diag::error(
            span,
            DiagCode::TypeMismatch,
            format!(
                "type mismatch in '{}': left is '{}', right is '{}'",
                bin_op_symbol(tag),
                pool.display(lhs_ty),
                pool.display(rhs_ty),
            ),
        ));
        return fcx.builder.unreachable(pool.error_type(), span);
    }
    let kind_ty = if pool.is_error(lhs_ty) {
        rhs_ty
    } else {
        lhs_ty
    };
    let is_equality = matches!(tag, InstTag::Eq | InstTag::NotEq);
    if is_equality {
        match pool.kind(kind_ty) {
            TypeKind::Int | TypeKind::Bool => {
                let tir_tag = match tag {
                    InstTag::Eq => TirTag::ICmpEq,
                    InstTag::NotEq => TirTag::ICmpNe,
                    _ => unreachable!(),
                };
                fcx.builder.binary(tir_tag, pool.bool_(), lhs, rhs, span)
            }
            TypeKind::Error => fcx.builder.unreachable(pool.error_type(), span),
            TypeKind::Str => {
                sink.emit(Diag::error(
                    span,
                    DiagCode::UnsupportedOperator,
                    format!(
                        "equality operator '{}' not supported for type 'str' (yet)",
                        bin_op_symbol(tag),
                    ),
                ));
                fcx.builder.unreachable(pool.error_type(), span)
            }
            TypeKind::Void | TypeKind::Tuple => {
                sink.emit(Diag::error(
                    span,
                    DiagCode::UnsupportedOperator,
                    format!(
                        "equality operator '{}' not supported for type '{}'",
                        bin_op_symbol(tag),
                        pool.display(kind_ty),
                    ),
                ));
                fcx.builder.unreachable(pool.error_type(), span)
            }
        }
    } else {
        match pool.kind(kind_ty) {
            TypeKind::Int => {
                let tir_tag = match tag {
                    InstTag::Add => TirTag::IAdd,
                    InstTag::Sub => TirTag::ISub,
                    InstTag::Mul => TirTag::IMul,
                    InstTag::Div => TirTag::ISDiv,
                    _ => unreachable!(),
                };
                fcx.builder.binary(tir_tag, pool.int(), lhs, rhs, span)
            }
            TypeKind::Error => fcx.builder.unreachable(pool.error_type(), span),
            _ => {
                sink.emit(Diag::error(
                    span,
                    DiagCode::UnsupportedOperator,
                    format!(
                        "arithmetic operator '{}' not supported for type '{}'",
                        bin_op_symbol(tag),
                        pool.display(kind_ty),
                    ),
                ));
                fcx.builder.unreachable(pool.error_type(), span)
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn check_call(
    uir: &Uir,
    view: &CallView,
    arg_tirs: &[TirRef],
    span: Span,
    fcx: &mut FuncCtx,
    signatures: &HashMap<StringId, FunctionSig>,
    pool: &mut InternPool,
    sink: &mut DiagSink,
) -> TirRef {
    let name_id = view.name;
    let name_str = pool.str(name_id);

    if let Some(builtin) = builtins::lookup(name_str) {
        let ret_ty = builtin.return_type(pool);
        check_builtin_call(name_str, uir, &view.args, span, sink);
        return fcx.builder.call(name_id, arg_tirs, ret_ty, span);
    }

    let Some(sig) = signatures.get(&name_id) else {
        sink.emit(Diag::error(
            span,
            DiagCode::UndefinedFunction,
            format!("undefined function: '{}'", name_str),
        ));
        return fcx.builder.unreachable(pool.error_type(), span);
    };

    if view.args.len() != sig.params.len() {
        sink.emit(Diag::error(
            span,
            DiagCode::ArityMismatch,
            format!(
                "call to '{}' has wrong arity: expected {} argument(s), got {}",
                name_str,
                sig.params.len(),
                view.args.len(),
            ),
        ));
    } else {
        for (idx, ((arg_uir, arg_tir), &expected)) in view
            .args
            .iter()
            .zip(arg_tirs.iter())
            .zip(sig.params.iter())
            .enumerate()
        {
            let actual = fcx.builder_ty(*arg_tir);
            if !pool.compatible(actual, expected) {
                sink.emit(Diag::error(
                    uir.span(*arg_uir),
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
    fcx.builder.call(name_id, arg_tirs, sig.return_type, span)
}

/// Front-end validation for builtin calls.
///
/// These checks are builtin-specific and temporary: once `print`
/// moves to a runtime crate and is called through a normal
/// signature (see ISSUES.md I-006), they go away.
fn check_builtin_call(name: &str, uir: &Uir, args: &[InstRef], span: Span, sink: &mut DiagSink) {
    if name == "print" {
        if args.len() != 1 {
            sink.emit(Diag::error(
                span,
                DiagCode::ArityMismatch,
                format!("print() takes exactly 1 argument, got {}", args.len()),
            ));
            return;
        }
        if !matches!(uir.inst(args[0]).tag, InstTag::StrLiteral) {
            sink.emit(Diag::error(
                uir.span(args[0]),
                DiagCode::BuiltinArgKind,
                "print() argument must be a string literal",
            ));
        }
    }
}

fn bin_op_symbol(tag: InstTag) -> &'static str {
    match tag {
        InstTag::Add => "+",
        InstTag::Sub => "-",
        InstTag::Mul => "*",
        InstTag::Div => "/",
        InstTag::Eq => "==",
        InstTag::NotEq => "!=",
        _ => "?",
    }
}

impl FuncCtx {
    /// Look up the type the builder recorded for a freshly emitted
    /// TIR ref. Equivalent to `tir.inst(r).ty` but operates on the
    /// in-progress builder, which the finished `Tir` borrows from.
    fn builder_ty(&self, r: TirRef) -> TypeId {
        self.builder_inst(r).ty
    }

    fn builder_inst(&self, r: TirRef) -> &crate::tir::TypedInst {
        // The builder's instruction arena is private; reach in via
        // the same indexing convention the builder uses (slot 0
        // sentinel, 1-based refs).
        self.builder.inst_at(r.index())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::astgen;
    use crate::lexer::lex;
    use crate::parser::program_parser;
    use crate::tir::{Tir, TirData};
    use chumsky::Parser;
    use chumsky::input::{Input, Stream};

    type RunOk = (Vec<Tir>, InternPool);

    /// Lex + parse + astgen + sema. Returns either the typed TIR
    /// (alongside the pool) or the diagnostics that stopped one of
    /// those stages.
    fn run(input: &str) -> Result<RunOk, Vec<Diag>> {
        let mut pool = InternPool::new();
        let tokens = lex(input, &mut pool).expect("lex ok");
        let token_stream =
            Stream::from_iter(tokens).map((0..input.len()).into(), |(t, s): (_, _)| (t, s));
        let program = program_parser()
            .parse(token_stream)
            .into_result()
            .expect("parse ok");

        let mut sink = DiagSink::new();
        let uir = astgen::generate(&program, &mut pool, &mut sink);
        if sink.has_errors() {
            return Err(sink.into_diags());
        }
        let tirs = analyze(&uir, &mut pool, &mut sink);
        if sink.has_errors() {
            return Err(sink.into_diags());
        }
        Ok((tirs, pool))
    }

    /// Variant that returns TIR even when sema reported errors —
    /// used to assert the "Unreachable + diag" invariant from §4.5.
    fn run_with_errors(input: &str) -> (Vec<Tir>, Vec<Diag>, InternPool) {
        let mut pool = InternPool::new();
        let tokens = lex(input, &mut pool).expect("lex ok");
        let token_stream =
            Stream::from_iter(tokens).map((0..input.len()).into(), |(t, s): (_, _)| (t, s));
        let program = program_parser()
            .parse(token_stream)
            .into_result()
            .expect("parse ok");

        let mut sink = DiagSink::new();
        let uir = astgen::generate(&program, &mut pool, &mut sink);
        let tirs = analyze(&uir, &mut pool, &mut sink);
        (tirs, sink.into_diags(), pool)
    }

    fn first_msg(diags: &[Diag]) -> &str {
        &diags[0].message
    }

    fn any_code(diags: &[Diag], code: DiagCode) -> bool {
        diags.iter().any(|d| d.code == code)
    }

    fn tir_named<'a>(tirs: &'a [Tir], pool: &InternPool, name: &str) -> &'a Tir {
        let id = pool.find_str(name).expect("name should be interned");
        tirs.iter()
            .find(|t| t.name == id)
            .unwrap_or_else(|| panic!("no function named {:?}", name))
    }

    fn stmt_at(tir: &Tir, i: usize) -> TirRef {
        tir.body_stmts()[i]
    }

    #[test]
    fn fills_types_on_flat_integer_var() {
        let (tirs, pool) = run("x = 42").unwrap();
        let main = tir_named(&tirs, &pool, "main");
        assert_eq!(main.return_type, pool.int());
        let var_decl = stmt_at(main, 0);
        assert_eq!(main.inst(var_decl).ty, pool.int());
    }

    #[test]
    fn infers_string_literal_type() {
        let (tirs, pool) = run("x = \"hello\"").unwrap();
        let main = &tirs[0];
        let v = main.var_decl_view(stmt_at(main, 0));
        assert_eq!(main.inst(v.initializer).ty, pool.str_());
    }

    #[test]
    fn typed_variable_annotation_honored() {
        let (tirs, pool) = run("x: int = 42").unwrap();
        let main = &tirs[0];
        assert_eq!(main.inst(stmt_at(main, 0)).ty, pool.int());
    }

    #[test]
    fn bool_annotation_resolves() {
        let (tirs, pool) = run("x: bool = true").unwrap();
        let main = &tirs[0];
        assert_eq!(main.inst(stmt_at(main, 0)).ty, pool.bool_());
    }

    #[test]
    fn variable_reference_type_resolved() {
        let (tirs, pool) = run("x = 42\ny = x").unwrap();
        let main = &tirs[0];
        let stmts = main.body_stmts();
        let v = main.var_decl_view(stmts[1]);
        assert_eq!(main.inst(stmts[1]).ty, pool.int());
        assert_eq!(main.inst(v.initializer).ty, pool.int());
        assert!(matches!(main.inst(v.initializer).tag, TirTag::Var));
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
        let (tirs, pool) = run(code).unwrap();
        let main = tir_named(&tirs, &pool, "main");
        let ret = stmt_at(main, 0);
        let operand = match main.inst(ret).data {
            TirData::UnOp(o) => o,
            other => panic!("expected Return UnOp, got {:?}", other),
        };
        assert_eq!(main.inst(operand).ty, pool.int());
        assert!(matches!(main.inst(operand).tag, TirTag::Call));
    }

    #[test]
    fn void_function_signature() {
        let (tirs, pool) =
            run("fn greet():\n\tprint(\"hi\")\n\nfn main() -> int:\n\tgreet()\n\treturn 0\n")
                .unwrap();
        let greet = tir_named(&tirs, &pool, "greet");
        assert_eq!(greet.return_type, pool.void());
    }

    #[test]
    fn print_call_has_int_type() {
        let (tirs, pool) = run("msg = print(\"Hello\\n\")").unwrap();
        let main = &tirs[0];
        let v = main.var_decl_view(stmt_at(main, 0));
        assert_eq!(main.inst(stmt_at(main, 0)).ty, pool.int());
        assert_eq!(main.inst(v.initializer).ty, pool.int());
    }

    #[test]
    fn int_equality_yields_bool() {
        let (tirs, pool) = run("x = 1 == 2").unwrap();
        let main = &tirs[0];
        let v = main.var_decl_view(stmt_at(main, 0));
        assert_eq!(main.inst(v.initializer).ty, pool.bool_());
        assert!(matches!(main.inst(v.initializer).tag, TirTag::ICmpEq));
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
    }

    #[test]
    fn bool_arithmetic_rejected() {
        let diags = run("x = true + 1").unwrap_err();
        assert!(any_code(&diags, DiagCode::TypeMismatch));
    }

    #[test]
    fn bool_arithmetic_same_type_rejected_as_unsupported_op() {
        let diags = run("x = true + false").unwrap_err();
        assert!(any_code(&diags, DiagCode::UnsupportedOperator));
        assert!(!any_code(&diags, DiagCode::TypeMismatch));
    }

    #[test]
    fn bool_literal_true_type() {
        let (tirs, pool) = run("x = true").unwrap();
        let main = &tirs[0];
        let v = main.var_decl_view(stmt_at(main, 0));
        assert_eq!(main.inst(v.initializer).ty, pool.bool_());
        assert!(matches!(main.inst(v.initializer).data, TirData::Bool(true)));
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
    }

    #[test]
    fn call_arity_mismatch_rejected() {
        let code = "fn add(a: int, b: int) -> int:\n\treturn a + b\n\nfn main() -> int:\n\treturn add(1, 2, 3)\n";
        let diags = run(code).unwrap_err();
        assert!(any_code(&diags, DiagCode::ArityMismatch));
    }

    #[test]
    fn call_argument_type_mismatch_rejected() {
        let code = "fn f(a: int) -> int:\n\treturn a\n\nfn main() -> int:\n\treturn f(true)\n";
        let diags = run(code).unwrap_err();
        assert!(any_code(&diags, DiagCode::TypeMismatch));
    }

    #[test]
    fn vardecl_annotation_initializer_mismatch_rejected() {
        let diags = run("x: int = \"hello\"").unwrap_err();
        assert!(any_code(&diags, DiagCode::TypeMismatch));
    }

    #[test]
    fn neg_on_bool_rejected() {
        let diags = run("x = -true").unwrap_err();
        assert!(any_code(&diags, DiagCode::UnsupportedOperator));
    }

    #[test]
    fn nested_expression_types_all_filled() {
        let (tirs, _) = run("x = (1 + 2) * -3").unwrap();
        for tir in &tirs {
            for idx in 1..tir.instructions.len() {
                let inst = &tir.instructions[idx];
                // Every emitted instruction has *some* TypeId. The
                // error sentinel is allowed only at Unreachable; an
                // error-free run shouldn't have any.
                assert!(!matches!(inst.tag, TirTag::Unreachable));
            }
        }
    }

    /// §4.5 exit criterion: a UIR with a deliberate type error
    /// produces (a) TIR for the rest of the function, (b) an
    /// `Unreachable` instruction at the failure point, (c) exactly
    /// one diagnostic in the sink.
    #[test]
    fn type_error_emits_unreachable_and_keeps_going() {
        // `-true` is the sole error; the `42` return after it should
        // still appear in the function's TIR body.
        let src = "fn main() -> int:\n\tx = -true\n\treturn 42\n";
        let (tirs, diags, _pool) = run_with_errors(src);
        assert_eq!(diags.len(), 1, "got: {:#?}", diags);
        assert_eq!(diags[0].code, DiagCode::UnsupportedOperator);

        let main = &tirs[0];
        // Function body still has both statements.
        assert_eq!(main.body_stmts().len(), 2);

        // Find the Unreachable inserted in place of the failed
        // initializer.
        let mut saw_unreachable = false;
        for idx in 1..main.instructions.len() {
            if matches!(main.instructions[idx].tag, TirTag::Unreachable) {
                saw_unreachable = true;
                break;
            }
        }
        assert!(saw_unreachable, "expected an Unreachable instruction");
    }
}
