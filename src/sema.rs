//! Semantic analysis: scope + type resolution over UIR.
//!
//! Sema consumes the flat [`Uir`] produced by `astgen` and writes
//! resolved types into a sidecar [`TypeTable`] (`Vec<Option<TypeId>>`)
//! indexed by [`InstRef`]. `pipeline.rs` hands the same
//! `(uir, types)` pair straight to `codegen::compile` — there is no
//! intermediate tree-shaped IR.
//!
//! Why a sidecar rather than mutating instructions in place: UIR is
//! an immutable structural snapshot from astgen. Adding a typed slot
//! to every `Inst` would either (a) mutate UIR — breaking the "input
//! to sema is read-only" invariant we'll need for incremental and
//! comptime — or (b) require copying the whole stream. The
//! `TypeTable` is the deliberate interim shape from
//! pipeline_alignment.md §3.3; Phase 4 replaces it with freshly
//! emitted TIR.
//!
//! Errors are accumulated through a [`DiagSink`] so analysis can
//! continue past the first problem and surface several in one run. A
//! failed expression substitutes `pool.error_type()` for its result
//! type and downstream checks treat that sentinel as compatible with
//! anything via `InternPool::compatible`.

use crate::builtins;
use crate::diag::{Diag, DiagCode, DiagSink};
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

/// Per-instruction resolved-type table.
///
/// Indexed by `InstRef::index()`; entry 0 is the reserved sentinel
/// slot from `Uir::new` and is always `None`. Statement-shaped
/// instructions (`Return`, `ReturnVoid`, `ExprStmt`) write `Some(void)`
/// — they have no useful "value type" but `Some(_)` keeps consumers
/// from having to special-case statement vs. expression refs.
/// `VarDecl` writes the *resolved variable type* (post annotation /
/// inference) so codegen reconstruction can read it back without
/// recomputing.
pub type TypeTable = Vec<Option<TypeId>>;

/// Analyze `uir`, returning a per-instruction type table.
///
/// Diagnostics are accumulated into `sink`. Sema continues past
/// errors: a single bad expression substitutes `pool.error_type()`
/// for its result type and downstream checks treat that sentinel as
/// compatible with anything (see `InternPool::compatible`). The
/// driver consults `sink.has_errors()` to decide whether to proceed
/// to codegen.
pub fn analyze(uir: &Uir, pool: &mut InternPool, sink: &mut DiagSink) -> TypeTable {
    let mut types: TypeTable = vec![None; uir.instructions.len()];

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

    for body in &uir.func_bodies {
        analyze_function(uir, body, &mut types, &signatures, pool, sink);
    }

    types
}

fn analyze_function(
    uir: &Uir,
    body: &FuncBody,
    types: &mut TypeTable,
    signatures: &HashMap<StringId, FunctionSig>,
    pool: &mut InternPool,
    sink: &mut DiagSink,
) {
    let mut scope = Scope::new();
    for param in &body.params {
        scope.insert(param.name, param.ty);
    }

    let fn_return_type = body.return_type;
    for stmt_ref in uir.body_stmts(body) {
        analyze_stmt(
            uir,
            stmt_ref,
            types,
            &mut scope,
            signatures,
            pool,
            sink,
            fn_return_type,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn analyze_stmt(
    uir: &Uir,
    r: InstRef,
    types: &mut TypeTable,
    scope: &mut Scope,
    signatures: &HashMap<StringId, FunctionSig>,
    pool: &mut InternPool,
    sink: &mut DiagSink,
    fn_return_type: TypeId,
) {
    let inst = uir.inst(r);
    let span = uir.span(r);
    match inst.tag {
        InstTag::VarDecl => {
            let view = uir.var_decl_view(r);
            analyze_expr(uir, view.initializer, types, scope, signatures, pool, sink);
            let inferred = expect_ty(types, view.initializer);
            let resolved = resolve_var_decl_type(&view, inferred, uir, pool, sink);
            types[r.index()] = Some(resolved);
            scope.insert(view.name, resolved);
        }
        InstTag::Return => {
            let operand = match inst.data {
                InstData::UnOp(o) => o,
                _ => unreachable!("Return must carry InstData::UnOp"),
            };
            analyze_expr(uir, operand, types, scope, signatures, pool, sink);
            let actual = expect_ty(types, operand);
            if fn_return_type == pool.void() {
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
            } else if !pool.compatible(actual, fn_return_type) {
                sink.emit(Diag::error(
                    span,
                    DiagCode::TypeMismatch,
                    format!(
                        "return type mismatch: function expects '{}', got '{}'",
                        pool.display(fn_return_type),
                        pool.display(actual),
                    ),
                ));
            }
            types[r.index()] = Some(pool.void());
        }
        InstTag::ReturnVoid => {
            if fn_return_type != pool.void() && !pool.is_error(fn_return_type) {
                sink.emit(Diag::error(
                    span,
                    DiagCode::TypeMismatch,
                    format!(
                        "missing return value: function expects '{}'",
                        pool.display(fn_return_type),
                    ),
                ));
            }
            types[r.index()] = Some(pool.void());
        }
        InstTag::ExprStmt => {
            let operand = match inst.data {
                InstData::UnOp(o) => o,
                _ => unreachable!("ExprStmt must carry InstData::UnOp"),
            };
            analyze_expr(uir, operand, types, scope, signatures, pool, sink);
            types[r.index()] = Some(pool.void());
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
    types: &mut TypeTable,
    scope: &Scope,
    signatures: &HashMap<StringId, FunctionSig>,
    pool: &mut InternPool,
    sink: &mut DiagSink,
) {
    // Memoize: an InstRef already analyzed in this function (e.g. a
    // shared sub-expression in a future SSA shape) doesn't need to
    // be re-typed. Today UIR is tree-shaped (one parent per inst),
    // so this is purely defensive — but it's the right invariant to
    // establish before TIR / lazy sema land.
    if types[r.index()].is_some() {
        return;
    }

    let inst = uir.inst(r);
    let span = uir.span(r);
    let ty = match inst.tag {
        InstTag::IntLiteral => pool.int(),
        InstTag::StrLiteral => pool.str_(),
        InstTag::BoolLiteral => pool.bool_(),
        InstTag::Var => {
            let name = match inst.data {
                InstData::Var(s) => s,
                _ => unreachable!("Var must carry InstData::Var"),
            };
            match scope.lookup(name) {
                Some(t) => t,
                None => {
                    sink.emit(Diag::error(
                        span,
                        DiagCode::UndefinedVariable,
                        format!("undefined variable: '{}'", pool.str(name)),
                    ));
                    pool.error_type()
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
            analyze_expr(uir, lhs, types, scope, signatures, pool, sink);
            analyze_expr(uir, rhs, types, scope, signatures, pool, sink);
            let lhs_ty = expect_ty(types, lhs);
            let rhs_ty = expect_ty(types, rhs);
            check_binary_op(inst.tag, lhs_ty, rhs_ty, span, pool, sink)
        }
        InstTag::Neg => {
            let operand = match inst.data {
                InstData::UnOp(o) => o,
                _ => unreachable!("Neg must carry InstData::UnOp"),
            };
            analyze_expr(uir, operand, types, scope, signatures, pool, sink);
            let sub_ty = expect_ty(types, operand);
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
        InstTag::Call => {
            let view = uir.call_view(r);
            for arg in &view.args {
                analyze_expr(uir, *arg, types, scope, signatures, pool, sink);
            }
            check_call(uir, &view, types, span, signatures, pool, sink)
        }
        other => panic!(
            "analyze_expr: instruction at %{} is not an expression (tag={:?})",
            r.index(),
            other
        ),
    };
    types[r.index()] = Some(ty);
}

fn check_binary_op(
    tag: InstTag,
    lhs_ty: TypeId,
    rhs_ty: TypeId,
    span: Span,
    pool: &mut InternPool,
    sink: &mut DiagSink,
) -> TypeId {
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
        return pool.error_type();
    }
    // Pick the non-error operand for the kind check, if any.
    let kind_ty = if pool.is_error(lhs_ty) {
        rhs_ty
    } else {
        lhs_ty
    };
    let is_equality = matches!(tag, InstTag::Eq | InstTag::NotEq);
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
                        bin_op_symbol(tag),
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
                        bin_op_symbol(tag),
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
                        bin_op_symbol(tag),
                        pool.display(kind_ty),
                    ),
                ));
                pool.error_type()
            }
        }
    }
}

fn check_call(
    uir: &Uir,
    view: &CallView,
    types: &TypeTable,
    span: Span,
    signatures: &HashMap<StringId, FunctionSig>,
    pool: &mut InternPool,
    sink: &mut DiagSink,
) -> TypeId {
    // Resolve the name once: a single `pool.str` call gives us the
    // &str needed for both the builtin lookup and the diagnostic
    // messages, while `name_id` keeps the signature-table lookup at
    // a `StringId` (u32) compare.
    let name_id = view.name;
    let name_str = pool.str(name_id);
    if let Some(builtin) = builtins::lookup(name_str) {
        check_builtin_call(name_str, uir, &view.args, span, sink);
        return builtin.return_type(pool);
    }
    let Some(sig) = signatures.get(&name_id) else {
        sink.emit(Diag::error(
            span,
            DiagCode::UndefinedFunction,
            format!("undefined function: '{}'", name_str),
        ));
        return pool.error_type();
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
        for (idx, (arg_ref, &expected)) in view.args.iter().zip(sig.params.iter()).enumerate() {
            let actual = expect_ty(types, *arg_ref);
            if !pool.compatible(actual, expected) {
                sink.emit(Diag::error(
                    uir.span(*arg_ref),
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

fn expect_ty(types: &TypeTable, r: InstRef) -> TypeId {
    types[r.index()].expect("analyze_expr must fill the type for every visited inst")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::astgen;
    use crate::lexer::lex;
    use crate::parser::program_parser;
    use crate::uir::FuncBody;
    use chumsky::Parser;
    use chumsky::input::{Input, Stream};

    type RunOk = (Uir, Vec<Option<TypeId>>, InternPool);

    /// Lex + parse + astgen + sema. Returns either the typed UIR
    /// (alongside the side-table and pool) or the diagnostics that
    /// stopped one of those stages.
    ///
    /// Tests inspect UIR directly: stmt/expr "kind" comes from
    /// `uir.inst(r).tag`, payloads from `uir.var_decl_view(r)` etc.,
    /// and resolved types from `types[r.index()]`. There is no HIR
    /// reconstruction step \u2014 production codegen takes the same
    /// `(uir, types)` pair this helper hands back.
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
        let types = analyze(&uir, &mut pool, &mut sink);
        if sink.has_errors() {
            return Err(sink.into_diags());
        }
        Ok((uir, types, pool))
    }

    fn first_msg(diags: &[Diag]) -> &str {
        &diags[0].message
    }

    fn any_code(diags: &[Diag], code: DiagCode) -> bool {
        diags.iter().any(|d| d.code == code)
    }

    fn body_named<'a>(uir: &'a Uir, pool: &InternPool, name: &str) -> &'a FuncBody {
        let id = pool.find_str(name).expect("name should be interned");
        uir.func_bodies
            .iter()
            .find(|f| f.name == id)
            .unwrap_or_else(|| panic!("no function named {:?}", name))
    }

    fn stmt_at(uir: &Uir, body: &FuncBody, i: usize) -> InstRef {
        uir.body_stmts(body)[i]
    }

    /// Assert every reachable instruction in every function body
    /// has a resolved type. Slot 0 (the reserved UIR sentinel) is
    /// allowed to stay `None`.
    fn assert_all_inst_types_resolved(uir: &Uir, types: &[Option<TypeId>]) {
        for body in &uir.func_bodies {
            for stmt_ref in uir.body_stmts(body) {
                walk(uir, types, stmt_ref);
            }
        }
        fn walk(uir: &Uir, types: &[Option<TypeId>], r: InstRef) {
            assert!(
                types[r.index()].is_some(),
                "no type recorded for inst %{}",
                r.index()
            );
            let inst = uir.inst(r);
            match inst.data {
                InstData::None
                | InstData::Int(_)
                | InstData::Str(_)
                | InstData::Bool(_)
                | InstData::Var(_) => {}
                InstData::UnOp(o) => walk(uir, types, o),
                InstData::BinOp { lhs, rhs } => {
                    walk(uir, types, lhs);
                    walk(uir, types, rhs);
                }
                InstData::Extra(_) => match inst.tag {
                    InstTag::Call => uir
                        .call_view(r)
                        .args
                        .iter()
                        .for_each(|a| walk(uir, types, *a)),
                    InstTag::VarDecl => walk(uir, types, uir.var_decl_view(r).initializer),
                    _ => {}
                },
            }
        }
    }

    #[test]
    fn fills_types_on_flat_integer_var() {
        let (uir, types, pool) = run("x = 42").unwrap();
        assert_all_inst_types_resolved(&uir, &types);
        let main = body_named(&uir, &pool, "main");
        assert_eq!(main.return_type, pool.int());
        let var_decl = stmt_at(&uir, main, 0);
        assert_eq!(types[var_decl.index()], Some(pool.int()));
    }

    #[test]
    fn infers_string_literal_type() {
        let (uir, types, pool) = run("x = \"hello\"").unwrap();
        let var_decl = stmt_at(&uir, &uir.func_bodies[0], 0);
        assert_eq!(types[var_decl.index()], Some(pool.str_()));
    }

    #[test]
    fn typed_variable_annotation_honored() {
        let (uir, types, pool) = run("x: int = 42").unwrap();
        let var_decl = stmt_at(&uir, &uir.func_bodies[0], 0);
        assert_eq!(types[var_decl.index()], Some(pool.int()));
    }

    #[test]
    fn bool_annotation_resolves() {
        let (uir, types, pool) = run("x: bool = true").unwrap();
        let var_decl = stmt_at(&uir, &uir.func_bodies[0], 0);
        assert_eq!(types[var_decl.index()], Some(pool.bool_()));
    }

    #[test]
    fn variable_reference_type_resolved() {
        let (uir, types, pool) = run("x = 42\ny = x").unwrap();
        let stmts = uir.body_stmts(&uir.func_bodies[0]);
        // Second stmt is `y = x`; its initializer should be a Var
        // with int type.
        let v = uir.var_decl_view(stmts[1]);
        assert_eq!(types[stmts[1].index()], Some(pool.int()));
        assert_eq!(types[v.initializer.index()], Some(pool.int()));
        assert!(matches!(uir.inst(v.initializer).tag, InstTag::Var));
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
        let (uir, types, pool) = run(code).unwrap();
        let main = body_named(&uir, &pool, "main");
        let ret = stmt_at(&uir, main, 0);
        // `Return x` carries the operand in InstData::UnOp.
        let operand = match uir.inst(ret).data {
            InstData::UnOp(o) => o,
            other => panic!("expected Return UnOp, got {:?}", other),
        };
        assert_eq!(types[operand.index()], Some(pool.int()));
        assert!(matches!(uir.inst(operand).tag, InstTag::Call));
    }

    #[test]
    fn void_function_signature() {
        let (uir, _types, pool) =
            run("fn greet():\n\tprint(\"hi\")\n\nfn main() -> int:\n\tgreet()\n\treturn 0\n")
                .unwrap();
        let greet = body_named(&uir, &pool, "greet");
        assert_eq!(greet.return_type, pool.void());
    }

    #[test]
    fn print_call_has_int_type() {
        let (uir, types, pool) = run("msg = print(\"Hello\\n\")").unwrap();
        let var_decl = stmt_at(&uir, &uir.func_bodies[0], 0);
        let v = uir.var_decl_view(var_decl);
        assert_eq!(types[var_decl.index()], Some(pool.int()));
        assert_eq!(types[v.initializer.index()], Some(pool.int()));
    }

    #[test]
    fn int_equality_yields_bool() {
        let (uir, types, pool) = run("x = 1 == 2").unwrap();
        let var_decl = stmt_at(&uir, &uir.func_bodies[0], 0);
        let v = uir.var_decl_view(var_decl);
        assert_eq!(types[var_decl.index()], Some(pool.bool_()));
        assert_eq!(types[v.initializer.index()], Some(pool.bool_()));
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
        let (uir, types, pool) = run("x = true").unwrap();
        let var_decl = stmt_at(&uir, &uir.func_bodies[0], 0);
        let v = uir.var_decl_view(var_decl);
        assert_eq!(types[var_decl.index()], Some(pool.bool_()));
        assert!(matches!(uir.inst(v.initializer).data, InstData::Bool(true)));
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
        let (uir, types, _) = run("x = (1 + 2) * -3").unwrap();
        assert_all_inst_types_resolved(&uir, &types);
    }

    #[test]
    fn type_table_indexed_by_inst_ref() {
        // Side-table is the same length as `instructions`; slot 0
        // (the reserved sentinel) stays None; every reachable inst
        // ends up Some.
        let (uir, types, _) = run("x = 1 + 2").unwrap();
        assert_eq!(types.len(), uir.instructions.len());
        assert_eq!(types[0], None, "slot 0 is the reserved sentinel");
        for (idx, t) in types.iter().enumerate().skip(1) {
            assert!(t.is_some(), "no type recorded for inst %{}", idx);
        }
    }
}
