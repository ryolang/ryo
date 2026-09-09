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

use chumsky::span::{SimpleSpan, Span as _};
use ryo_core::ast;
use ryo_core::diag::{Diag, DiagCode, DiagSink};
use ryo_core::types::{InternPool, StringId, TypeId};
use ryo_core::uir::{InstRef, InstTag, Uir, UirBuilder, UirParam, UirStructDecl, UirStructField};
use std::collections::HashMap;

type Span = SimpleSpan;

fn synthetic_span() -> Span {
    SimpleSpan::new((), 0..0)
}

/// Pre-interned `StringId`s for the primitive type names.
///
/// Phase 2 made identifiers `StringId` handles, so `resolve_type`
/// used to call `pool.str(name)` on every type annotation just to
/// reach the &str-keyed match. Interning the names once at
/// the top of `generate` lets subsequent comparisons be a `StringId`
/// equality check (`u32` compare) instead of a `pool.str` lookup
/// followed by a string compare.
struct Primitives {
    int: StringId,
    str_: StringId,
    strview: StringId,
    bytes: StringId,
    bytesview: StringId,
    bool_: StringId,
    float: StringId,
}

impl Primitives {
    fn new(pool: &mut InternPool) -> Self {
        Primitives {
            int: pool.intern_str("int"),
            str_: pool.intern_str("str"),
            strview: pool.intern_str("strview"),
            bytes: pool.intern_str("bytes"),
            bytesview: pool.intern_str("bytesview"),
            bool_: pool.intern_str("bool"),
            float: pool.intern_str("float"),
        }
    }
}

/// Type-annotation resolution: primitive names plus the struct
/// declarations collected by `generate`'s pre-scan (M9).
///
/// Primitive names win over struct names, mirroring the historical
/// behavior where only primitives resolved — a struct named `int`
/// stays shadowed by the primitive.
struct TypeResolver {
    prims: Primitives,
    struct_types: HashMap<StringId, TypeId>,
}

impl TypeResolver {
    fn is_primitive(&self, name: StringId) -> bool {
        let p = &self.prims;
        name == p.int
            || name == p.str_
            || name == p.strview
            || name == p.bytes
            || name == p.bytesview
            || name == p.bool_
            || name == p.float
    }

    fn resolve(
        &self,
        name: StringId,
        is_view: bool,
        span: Span,
        pool: &InternPool,
        sink: &mut DiagSink,
    ) -> TypeId {
        if is_view {
            // Legacy `&name` type syntax (M8.4 pre-Q5). Only `&str` was ever
            // valid; it is now a targeted migration error (final spec Q5).
            let msg = if name == self.prims.str_ {
                "`&str` was renamed to `strview` (final spec Q5)".to_string()
            } else {
                format!(
                    "unknown view type: '&{}' (view types are named: `strview`)",
                    pool.str(name)
                )
            };
            sink.emit(Diag::error(span, DiagCode::UnknownType, msg));
            return pool.error_type();
        }
        let p = &self.prims;
        if name == p.int {
            pool.int()
        } else if name == p.str_ {
            pool.str_()
        } else if name == p.strview {
            pool.str_view()
        } else if name == p.bytes {
            pool.bytes()
        } else if name == p.bytesview {
            pool.bytes_view()
        } else if name == p.bool_ {
            pool.bool_()
        } else if name == p.float {
            pool.float()
        } else if let Some(&ty) = self.struct_types.get(&name) {
            ty
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
}

/// Lower an [`ast::Ast`] to UIR, accumulating diagnostics in `sink`.
///
/// Returns the lowered UIR even on error (using `pool.error_type()`
/// for any annotation that failed to resolve) so subsequent passes
/// can keep type-checking and surface their own diagnostics. The
/// driver decides whether to proceed based on `sink.has_errors()`.
pub fn generate(program: &ast::Ast, pool: &mut InternPool, sink: &mut DiagSink) -> Uir {
    let mut func_defs: Vec<&ast::FunctionDef> = Vec::new();
    let mut top_level: Vec<ast::StmtId> = Vec::new();

    let main_id = pool.intern_str("main");

    // Pre-scan (M9): declare every top-level struct before any field
    // type is resolved, so field annotations can name structs
    // declared later in the file — and so a struct can name itself
    // (which the define DFS below then accepts or rejects).
    let mut struct_types: HashMap<StringId, TypeId> = HashMap::new();
    let mut struct_entries: HashMap<StringId, (TypeId, ast::StructDef, Span)> = HashMap::new();
    let mut struct_order: Vec<StringId> = Vec::new();

    for &stmt in program.top_level_stmts() {
        match &program.stmt(stmt).kind {
            ast::StmtKind::FunctionDef(def) => func_defs.push(def),
            // Parser-recovery placeholder: already diagnosed at the
            // parse stage, and not a "top-level statement" for the
            // explicit-main check.
            ast::StmtKind::Error => {}
            // Struct declarations (M9) are declarations, not
            // executable top-level statements — they must not trip
            // the explicit-main check and must not reach
            // `gen_implicit_main`. They lower to `uir.struct_decls`
            // entries below, not to instructions.
            ast::StmtKind::StructDef(def) => {
                let name = def.name.name;
                if struct_entries.contains_key(&name) {
                    sink.emit(Diag::error(
                        def.name.span,
                        DiagCode::DuplicateDeclaration,
                        format!("duplicate struct declaration: '{}'", pool.str(name)),
                    ));
                    continue;
                }
                let ty = pool.declare_struct(name);
                struct_types.insert(name, ty);
                struct_entries.insert(name, (ty, *def, program.stmt_span(stmt)));
                struct_order.push(name);
            }
            _ => top_level.push(stmt),
        }
    }

    let types = TypeResolver {
        prims: Primitives::new(pool),
        struct_types,
    };

    // Define DFS with cycle detection (M9): a struct's field types
    // must all be fully defined before `pool.define_struct` computes
    // its layout, so definitions happen in dependency order. A
    // by-value self-reference (direct or transitive) has no finite
    // layout and is rejected with `InfiniteSize`.
    let mut definer = StructDefiner {
        entries: &struct_entries,
        types: &types,
        ast: program,
        states: struct_order
            .iter()
            .map(|&name| (name, DefState::Pending))
            .collect(),
        resolved: HashMap::new(),
    };
    for &name in &struct_order {
        definer.define(name, pool, sink);
    }

    let has_explicit_main = func_defs.iter().any(|def| def.name.name == main_id);

    if has_explicit_main && !top_level.is_empty() {
        // Anchor the diagnostic on the first stray top-level stmt;
        // pointing at "the program" with a 0..0 span is useless in
        // a renderer.
        let span = program.stmt_span(top_level[0]);
        sink.emit(Diag::error(
            span,
            DiagCode::TopLevelWithExplicitMain,
            "top-level statements are not allowed when fn main() is defined",
        ));
        // Fall through and lower anyway; sema can still report
        // problems inside `main`.
    }

    let mut b = UirBuilder::new();

    // Register defined structs in source order. Structs that failed
    // (cycle, unknown field type) stay declared-but-undefined in the
    // pool and are left out — the sink already holds their errors.
    for &name in &struct_order {
        if definer.states[&name] != DefState::Defined {
            continue;
        }
        let (ty, _, span) = struct_entries[&name];
        b.add_struct_decl(UirStructDecl {
            name,
            ty,
            fields: definer.resolved.remove(&name).unwrap_or_default(),
            span,
        });
    }

    for func in &func_defs {
        gen_function_def(&mut b, program, func, &types, pool, sink);
    }
    if !has_explicit_main {
        // Synthesize an implicit `main` from top-level statements.
        // User-defined helper functions still appear above;
        // without this, calls to them in top-level code would
        // dangle as "undefined function" errors in sema.
        gen_implicit_main(&mut b, program, &top_level, main_id, &types, pool, sink);
    }

    b.finish()
}

/// Definition progress for one declared struct in the define DFS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DefState {
    /// Declared in the pre-scan; definition not attempted yet.
    Pending,
    /// On the current DFS stack — a field pointing here is a
    /// by-value cycle.
    InProgress,
    /// `pool.define_struct` has run; the layout is in the pool.
    Defined,
    /// Definition was abandoned after a diagnostic (cycle or
    /// unresolvable field type); dependents must fail too.
    Failed,
}

/// Working state for the struct define DFS (M9): tracks each
/// pre-scanned declaration's definition progress and its resolved
/// field list.
struct StructDefiner<'a> {
    entries: &'a HashMap<StringId, (TypeId, ast::StructDef, Span)>,
    types: &'a TypeResolver,
    ast: &'a ast::Ast,
    states: HashMap<StringId, DefState>,
    resolved: HashMap<StringId, Vec<UirStructField>>,
}

impl StructDefiner<'_> {
    /// Define one struct's fields in dependency order, depth-first.
    ///
    /// Cycle and unknown-type paths format diagnostics from the names
    /// already in hand — never `pool.display(id)`, which panics on a
    /// declared-but-undefined struct.
    fn define(&mut self, name: StringId, pool: &mut InternPool, sink: &mut DiagSink) {
        if self.states[&name] != DefState::Pending {
            return;
        }
        self.states.insert(name, DefState::InProgress);
        let (ty, def, _) = self.entries[&name];
        let decl_fields = self.ast.struct_field_decls(def.fields);
        let mut fields: Vec<UirStructField> = Vec::new();
        let mut failed = false;
        for &(fname, texpr) in decl_fields {
            // Order/cycle handling applies only to by-value struct
            // fields; primitives resolve without layout recursion,
            // and `&name` view syntax is a targeted migration error
            // handled by `resolve` below.
            if !texpr.is_view
                && !self.types.is_primitive(texpr.name)
                && let Some(&fty) = self.types.struct_types.get(&texpr.name)
            {
                match self.states[&texpr.name] {
                    DefState::InProgress => {
                        sink.emit(Diag::error(
                            texpr.span,
                            DiagCode::InfiniteSize,
                            format!(
                                "struct '{}' cannot contain itself by value: field '{}' has \
                                 type '{}', which would make its size infinite",
                                pool.str(name),
                                pool.str(fname),
                                pool.str(texpr.name),
                            ),
                        ));
                        failed = true;
                        continue;
                    }
                    DefState::Pending => {
                        self.define(texpr.name, pool, sink);
                        if !pool.is_defined_struct(fty) {
                            // The dependency failed its own
                            // definition; defining `name` against it
                            // would have no valid layout.
                            failed = true;
                            continue;
                        }
                    }
                    DefState::Defined => {}
                    DefState::Failed => {
                        failed = true;
                        continue;
                    }
                }
            }
            let fty = self
                .types
                .resolve(texpr.name, texpr.is_view, texpr.span, pool, sink);
            if pool.is_error(fty) {
                failed = true;
            }
            fields.push(UirStructField {
                name: fname,
                ty: fty,
                span: texpr.span,
            });
        }
        if failed {
            self.states.insert(name, DefState::Failed);
            return;
        }
        let field_types: Vec<(StringId, TypeId)> = fields.iter().map(|f| (f.name, f.ty)).collect();
        pool.define_struct(ty, name, &field_types);
        self.states.insert(name, DefState::Defined);
        self.resolved.insert(name, fields);
    }
}

fn lower_block(
    b: &mut UirBuilder,
    ast: &ast::Ast,
    stmts: &[ast::StmtId],
    types: &TypeResolver,
    pool: &mut InternPool,
    sink: &mut DiagSink,
) -> Vec<InstRef> {
    let mut out = Vec::new();
    for &s in stmts {
        gen_stmt(b, ast, s, types, pool, sink, &mut out);
    }
    out
}

fn gen_implicit_main(
    b: &mut UirBuilder,
    ast: &ast::Ast,
    stmts: &[ast::StmtId],
    main_id: StringId,
    types: &TypeResolver,
    pool: &mut InternPool,
    sink: &mut DiagSink,
) {
    // Synthesize `fn main():` (void return). Codegen falls through
    // to an implicit `ret_void` if no explicit return is emitted —
    // we deliberately do *not* push a synthetic Return here so the
    // body shape matches an explicit `fn main():` written by the
    // user.
    let mut body_stmts: Vec<InstRef> = Vec::new();
    for &stmt in stmts {
        gen_stmt(b, ast, stmt, types, pool, sink, &mut body_stmts);
    }

    let void_ty = pool.void();
    b.add_function(main_id, vec![], void_ty, &body_stmts, synthetic_span());
}

fn gen_function_def(
    b: &mut UirBuilder,
    ast: &ast::Ast,
    func: &ast::FunctionDef,
    types: &TypeResolver,
    pool: &mut InternPool,
    sink: &mut DiagSink,
) {
    let params: Vec<UirParam> = func
        .params
        .iter()
        .map(|p| UirParam {
            name: p.name.name,
            ty: types.resolve(
                p.type_annotation.name,
                p.type_annotation.is_view,
                p.type_annotation.span,
                pool,
                sink,
            ),
            mode: p.mode,
            span: p.span,
        })
        .collect();

    let return_type = match &func.return_type {
        Some(ty) => types.resolve(ty.name, ty.is_view, ty.span, pool, sink),
        None => pool.void(),
    };

    // `fn main()` must be `fn main():` — no args, no return type.
    // Non-zero exit codes go through the future `exit(code)`
    // builtin (M24); main is always void in v0.1 and the C-ABI
    // shim emitted by codegen returns 0 to the OS.
    let main_id = pool.find_str("main");
    if Some(func.name.name) == main_id {
        if !func.params.is_empty() {
            sink.emit(Diag::error(
                func.name.span,
                DiagCode::MainSignature,
                "fn main() must take no arguments",
            ));
        }
        if let Some(ret) = &func.return_type {
            sink.emit(Diag::error(
                ret.span,
                DiagCode::MainSignature,
                "fn main() must have no return type (use exit(code) for non-zero exit codes)",
            ));
        }
    }

    let body_stmts = lower_block(b, ast, ast.stmt_list(func.body), types, pool, sink);

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
    ast: &ast::Ast,
    stmt: ast::StmtId,
    types: &TypeResolver,
    pool: &mut InternPool,
    sink: &mut DiagSink,
    out: &mut Vec<InstRef>,
) {
    let span = ast.stmt_span(stmt);
    match &ast.stmt(stmt).kind {
        ast::StmtKind::VarDecl(decl) => {
            let initializer = gen_expr(b, ast, decl.initializer);
            let ty = decl
                .type_annotation
                .as_ref()
                .map(|ann| types.resolve(ann.name, ann.is_view, ann.span, pool, sink));
            let r = b.var_decl(decl.name.name, decl.mutable, ty, initializer, span);
            out.push(r);
        }
        ast::StmtKind::Return(value) => match value {
            Some(expr) => {
                let value = gen_expr(b, ast, *expr);
                out.push(b.unary(InstTag::Return, value, span));
            }
            None => {
                out.push(b.return_void(span));
            }
        },
        ast::StmtKind::ExprStmt(value) => {
            let value = gen_expr(b, ast, *value);
            out.push(b.unary(InstTag::ExprStmt, value, span));
        }
        ast::StmtKind::FunctionDef(_) => {
            sink.emit(Diag::error(
                span,
                DiagCode::NestedFunctionDef,
                "nested function definitions are not supported",
            ));
        }
        ast::StmtKind::AssignOrDecl { target, value } => {
            let value_ref = gen_expr(b, ast, *value);
            let r = b.assign_or_decl(target.name, value_ref, span);
            out.push(r);
        }
        ast::StmtKind::CompoundAssign { target, op, value } => {
            let value_ref = gen_expr(b, ast, *value);
            let r = b.compound_assign(target.name, *op, value_ref, span);
            out.push(r);
        }
        ast::StmtKind::FieldAssign { target, value } => {
            let target_ref = gen_expr(b, ast, *target);
            let value_ref = gen_expr(b, ast, *value);
            let r = b.field_assign(target_ref, value_ref, span);
            out.push(r);
        }
        ast::StmtKind::CompoundFieldAssign { target, op, value } => {
            let target_ref = gen_expr(b, ast, *target);
            let value_ref = gen_expr(b, ast, *value);
            let r = b.compound_field_assign(target_ref, *op, value_ref, span);
            out.push(r);
        }
        ast::StmtKind::IfStmt(if_stmt) => {
            let cond = gen_expr(b, ast, if_stmt.cond);
            let then_stmts =
                lower_block(b, ast, ast.stmt_list(if_stmt.then_block), types, pool, sink);

            let elif_branches: Vec<_> = ast
                .elif_list(if_stmt.elif_branches)
                .iter()
                .map(|elif| {
                    let elif_cond = gen_expr(b, ast, elif.cond);
                    let elif_body =
                        lower_block(b, ast, ast.stmt_list(elif.block), types, pool, sink);
                    (elif_cond, elif_body)
                })
                .collect();

            let else_stmts = if_stmt
                .else_block
                .map(|stmts| lower_block(b, ast, ast.stmt_list(stmts), types, pool, sink));

            let r = b.if_stmt(
                cond,
                &then_stmts,
                &elif_branches,
                else_stmts.as_deref(),
                span,
            );
            out.push(r);
        }
        ast::StmtKind::WhileLoop { cond, body } => {
            let cond_ref = gen_expr(b, ast, *cond);
            let body_refs = lower_block(b, ast, ast.stmt_list(*body), types, pool, sink);
            let r = b.while_loop(cond_ref, &body_refs, span);
            out.push(r);
        }
        ast::StmtKind::ForRange {
            var,
            iterator,
            start,
            end,
            body,
        } => {
            if pool.str(iterator.name) != "range" {
                sink.emit(Diag::error(
                    iterator.span,
                    DiagCode::ParseError,
                    format!(
                        "only `range(...)` is supported in `for` loops in v0.1, got `{}`",
                        pool.str(iterator.name),
                    ),
                ));
            }
            let start_ref = gen_expr(b, ast, *start);
            let end_ref = gen_expr(b, ast, *end);
            let body_refs = lower_block(b, ast, ast.stmt_list(*body), types, pool, sink);
            let r = b.for_range(var.name, start_ref, end_ref, &body_refs, span);
            out.push(r);
        }
        ast::StmtKind::Break => {
            let r = b.break_stmt(span);
            out.push(r);
        }
        ast::StmtKind::Continue => {
            let r = b.continue_stmt(span);
            out.push(r);
        }
        // Unparseable statement recovered by the parser. The parse
        // diagnostic was already emitted; lower it to nothing so the
        // rest of the program still reaches sema.
        ast::StmtKind::Error => {}
        // Struct declarations are top-level only and are filtered
        // out before lowering (see `generate`); nothing to lower.
        ast::StmtKind::StructDef(_) => {}
    }
}

fn gen_expr(b: &mut UirBuilder, ast: &ast::Ast, expr: ast::ExprId) -> InstRef {
    let span = ast.expr_span(expr);
    match ast.expr(expr).kind {
        ast::ExprKind::Literal(lit) => match lit {
            ast::Literal::Int(n) => b.int_literal(n, span),
            ast::Literal::Str(id) => b.str_literal(id, span),
            ast::Literal::Bool(v) => b.bool_literal(v, span),
            ast::Literal::Float(v) => b.float_literal(v, span),
            ast::Literal::Bytes(id) => b.bytes_literal(id, span),
        },
        ast::ExprKind::Ident(name) => b.var_ref(name, span),
        ast::ExprKind::BinaryOp(lhs, op, rhs) => {
            let l = gen_expr(b, ast, lhs);
            let r = gen_expr(b, ast, rhs);
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
                ast::BinaryOperator::And => InstTag::And,
                ast::BinaryOperator::Or => InstTag::Or,
            };
            b.binary(tag, l, r, span)
        }
        ast::ExprKind::UnaryOp(op, operand) => {
            let s = gen_expr(b, ast, operand);
            let tag = match op {
                ast::UnaryOperator::Neg => InstTag::Neg,
                ast::UnaryOperator::Not => InstTag::Not,
            };
            b.unary(tag, s, span)
        }
        ast::ExprKind::Call(name, args) => {
            let arg_refs: Vec<InstRef> = ast
                .expr_list(args)
                .iter()
                .map(|&a| gen_expr(b, ast, a))
                .collect();
            b.call(name, &arg_refs, span)
        }
        ast::ExprKind::MethodCall {
            receiver,
            method,
            args,
        } => {
            let receiver_ref = gen_expr(b, ast, receiver);
            let arg_refs: Vec<InstRef> = ast
                .expr_list(args)
                .iter()
                .map(|&a| gen_expr(b, ast, a))
                .collect();
            b.method_call(receiver_ref, method, &arg_refs, span)
        }
        ast::ExprKind::Borrow(inner) => {
            let inner_ref = gen_expr(b, ast, inner);
            b.borrow(inner_ref, span)
        }
        ast::ExprKind::Slice { base, start, end } => {
            // Slice projection `base[start:end]` (final spec §3);
            // bounds are optional shorthands. Sema type-checks the
            // base and yields `strview`.
            let base_ref = gen_expr(b, ast, base);
            let start_ref = start.map(|e| gen_expr(b, ast, e));
            let end_ref = end.map(|e| gen_expr(b, ast, e));
            b.slice(base_ref, start_ref, end_ref, span)
        }
        ast::ExprKind::Index { base, index } => {
            // Scalar indexing `base[index]` (M8.4.2). Sema gates the
            // base to bytes/bytesview.
            let base_ref = gen_expr(b, ast, base);
            let index_ref = gen_expr(b, ast, index);
            b.index(base_ref, index_ref, span)
        }
        // Struct literal `Name{field=value, ...}` (M9). The field
        // initializers stay in source order; sema canonicalizes them
        // against the declaration in `uir.struct_decls`.
        ast::ExprKind::StructLiteral(lit) => {
            let fields: Vec<(StringId, InstRef)> = ast
                .struct_field_inits(lit.fields)
                .iter()
                .map(|&(fname, e)| (fname, gen_expr(b, ast, e)))
                .collect();
            b.struct_lit(lit.name.name, &fields, span)
        }
        // Field access `object.field` (M9); chains fold left in the
        // AST, so each access lowers against its own object.
        ast::ExprKind::FieldAccess { object, field } => {
            let obj = gen_expr(b, ast, object);
            b.field_access(obj, field.name, span)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::lex;
    use crate::parser::program_parser;
    use chumsky::Parser;
    use chumsky::input::Input;
    use ryo_core::uir::InstData;

    fn parse_and_lower(input: &str) -> Result<(Uir, InternPool), Vec<Diag>> {
        // Phase-2 lex pipeline: logos + indent + intern in one
        // pass; identifiers come back as `StringId`. Lex diagnostics
        // go through `DiagSink` like every other stage, so assert
        // the sink stayed clean instead of unwrapping a `Result`.
        let mut pool = InternPool::new();
        let mut lex_sink = DiagSink::new();
        let tokens = lex(input, &mut pool, &mut lex_sink);
        assert!(
            !lex_sink.has_errors(),
            "lex errors: {:?}",
            lex_sink.into_diags()
        );
        let token_stream = tokens[..].split_token_span((0..input.len()).into());
        let mut ast = ryo_core::ast::Ast::new();
        program_parser()
            .parse_with_state(token_stream, &mut ast)
            .into_result()
            .expect("parse ok");

        let mut sink = DiagSink::new();
        let uir = generate(&ast, &mut pool, &mut sink);
        if sink.has_errors() {
            Err(sink.into_diags())
        } else {
            Ok((uir, pool))
        }
    }

    /// Find a function body by name through the `InternPool`.
    fn body_named<'a>(uir: &'a Uir, pool: &InternPool, name: &str) -> &'a ryo_core::uir::FuncBody {
        let id = pool.find_str(name).expect("name should be interned");
        uir.func_bodies
            .iter()
            .find(|f| f.name == id)
            .unwrap_or_else(|| panic!("no function named {:?}", name))
    }

    /// Top-level statement at index `i` in `body`'s execution order.
    fn stmt_at(uir: &Uir, body: &ryo_core::uir::FuncBody, i: usize) -> InstRef {
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
    fn astgen_lowers_borrow() {
        let (uir, pool) = parse_and_lower("fn main():\n\tmut c = 0\n\tf(&c)\n").unwrap();
        let main = body_named(&uir, &pool, "main");
        // body is [VarDecl(c), ExprStmt(Call)]; the call's first arg
        // should be a Borrow inst whose operand is the Var(c) read.
        let stmts = uir.body_stmts(main);
        let expr_stmt = uir.inst(stmts[1]);
        let call_ref = match expr_stmt.data {
            InstData::UnOp(o) => o,
            _ => panic!("expected ExprStmt to carry UnOp"),
        };
        let view = uir.call_view(call_ref);
        assert_eq!(view.args.len(), 1);
        let arg = view.args[0];
        assert!(
            matches!(uir.inst(arg).tag, InstTag::Borrow),
            "call arg should lower to a Borrow inst"
        );
        let inner = match uir.inst(arg).data {
            InstData::Borrow(i) => i,
            _ => panic!("expected Borrow data"),
        };
        assert!(
            matches!(uir.inst(inner).tag, InstTag::Var),
            "borrow operand should be a Var read"
        );
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
        // Implicit-main is now void; codegen falls through to an
        // implicit `return 0` for the C-ABI shim, so the synthetic
        // Return is no longer materialised in the UIR body.
        assert_eq!(main.return_type, pool.void());

        let stmts = uir.body_stmts(main);
        assert_eq!(stmts.len(), 1);

        let v = uir.var_decl_view(stmts[0]);
        assert_eq!(pool.str(v.name), "x");
        assert!(!v.mutable);
        assert!(matches!(uir.inst(v.initializer).tag, InstTag::IntLiteral));
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
        let diags = parse_and_lower("x = 42\n\nfn main():\n\tprint(\"hi\")\n").unwrap_err();
        assert!(
            diags
                .iter()
                .any(|d| d.code == DiagCode::TopLevelWithExplicitMain)
        );
    }

    #[test]
    fn explicit_main_structural() {
        let (uir, pool) = parse_and_lower("fn main():\n\tprint(\"hi\")\n").unwrap();
        assert_eq!(uir.func_bodies.len(), 1);
        let main = body_named(&uir, &pool, "main");
        assert_eq!(main.return_type, pool.void());
        let stmts = uir.body_stmts(main);
        assert_eq!(stmts.len(), 1);
        assert!(matches!(uir.inst(stmts[0]).tag, InstTag::ExprStmt));
    }

    #[test]
    fn main_with_return_type_emits_diag() {
        let diags = parse_and_lower("fn main() -> int:\n\treturn 0\n").unwrap_err();
        assert!(diags.iter().any(|d| d.code == DiagCode::MainSignature));
    }

    #[test]
    fn main_with_params_emits_diag() {
        let diags = parse_and_lower("fn main(x: int):\n\tprint(\"hi\")\n").unwrap_err();
        assert!(diags.iter().any(|d| d.code == DiagCode::MainSignature));
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
            "fn add(a: int, b: int) -> int:\n\treturn a + b\n\nfn main():\n\tprint(\"hi\")\n";
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

    #[test]
    fn lower_and_operator() {
        let (uir, pool) = parse_and_lower("x = true and false").unwrap();
        let main = body_named(&uir, &pool, "main");
        let v = uir.var_decl_view(uir.body_stmts(main)[0]);
        assert!(matches!(uir.inst(v.initializer).tag, InstTag::And));
    }

    #[test]
    fn lower_or_operator() {
        let (uir, pool) = parse_and_lower("x = true or false").unwrap();
        let main = body_named(&uir, &pool, "main");
        let v = uir.var_decl_view(uir.body_stmts(main)[0]);
        assert!(matches!(uir.inst(v.initializer).tag, InstTag::Or));
    }

    #[test]
    fn lower_not_operator() {
        let (uir, pool) = parse_and_lower("x = not true").unwrap();
        let main = body_named(&uir, &pool, "main");
        let v = uir.var_decl_view(uir.body_stmts(main)[0]);
        assert!(matches!(uir.inst(v.initializer).tag, InstTag::Not));
    }

    #[test]
    fn lower_if_stmt() {
        let code = "fn main():\n\tif true:\n\t\tx = 1\n\telse:\n\t\tx = 2\n";
        let (uir, pool) = parse_and_lower(code).unwrap();
        let main = body_named(&uir, &pool, "main");
        let stmts = uir.body_stmts(main);
        assert_eq!(stmts.len(), 1);
        assert!(matches!(uir.inst(stmts[0]).tag, InstTag::IfStmt));
        let view = uir.if_stmt_view(stmts[0]);
        assert_eq!(view.then_stmts.len(), 1);
        assert!(view.else_stmts.is_some());
    }

    #[test]
    fn lower_assign_or_decl() {
        let (uir, pool) = parse_and_lower("fn main():\n\tx = 42\n").unwrap();
        let main = body_named(&uir, &pool, "main");
        let stmts = uir.body_stmts(main);
        assert_eq!(stmts.len(), 1);
        assert!(matches!(
            uir.inst(stmts[0]).tag,
            ryo_core::uir::InstTag::AssignOrDecl
        ));
        let v = uir.assign_or_decl_view(stmts[0]);
        assert_eq!(pool.str(v.name), "x");
    }

    #[test]
    fn lower_while_loop() {
        let (uir, pool) = parse_and_lower("fn main():\n\twhile true:\n\t\tbreak\n").unwrap();
        let dump = format!("{}", uir.dump(&pool));
        assert!(dump.contains("while_loop"), "got:\n{}", dump);
    }

    #[test]
    fn lower_break() {
        let (uir, pool) = parse_and_lower("fn main():\n\twhile true:\n\t\tbreak\n").unwrap();
        let dump = format!("{}", uir.dump(&pool));
        assert!(dump.contains("break"), "got:\n{}", dump);
    }

    #[test]
    fn lower_continue() {
        let (uir, pool) = parse_and_lower("fn main():\n\twhile true:\n\t\tcontinue\n").unwrap();
        let dump = format!("{}", uir.dump(&pool));
        assert!(dump.contains("continue"), "got:\n{}", dump);
    }

    #[test]
    fn lower_compound_assign() {
        let (uir, pool) = parse_and_lower("fn main():\n\tmut x = 10\n\tx += 1\n").unwrap();
        let main = body_named(&uir, &pool, "main");
        let stmts = uir.body_stmts(main);
        assert_eq!(stmts.len(), 2);
        assert!(matches!(
            uir.inst(stmts[1]).tag,
            ryo_core::uir::InstTag::CompoundAssign
        ));
        let v = uir.compound_assign_view(stmts[1]);
        assert_eq!(pool.str(v.name), "x");
        assert_eq!(v.op, ryo_core::ast::CompoundOp::Add);
    }

    #[test]
    fn lower_for_range() {
        let (uir, pool) =
            parse_and_lower("fn main():\n\tfor i in range(0, 10):\n\t\tprint(i)\n").unwrap();
        let dump = format!("{}", uir.dump(&pool));
        assert!(dump.contains("for_range"), "got:\n{}", dump);
    }

    #[test]
    fn for_non_range_iterator_rejected() {
        let result = parse_and_lower("fn main():\n\tfor i in something(0, 10):\n\t\tprint(i)\n");
        assert!(result.is_err(), "non-range iterator should be rejected");
    }

    #[test]
    fn lower_slice_expression() {
        // `s` is intentionally undefined — astgen is a purely
        // structural pass; name resolution is sema's job. (Final
        // spec §3: `base[start:end]` lowers to a Slice projection.)
        let (uir, _pool) = parse_and_lower("fn main():\n\tx = s[1:2]\n").unwrap();
        let has_slice = uir.instructions.iter().any(|i| i.tag == InstTag::Slice);
        assert!(has_slice, "expected a Slice instruction");
    }

    #[test]
    fn strview_param_resolves_to_view() {
        let (uir, pool) = parse_and_lower("fn f(text: strview):\n\tprint(text)\n").unwrap();
        assert_eq!(uir.func_bodies[0].params[0].ty, pool.str_view());
    }

    #[test]
    fn legacy_amp_str_is_a_migration_error() {
        let diags = parse_and_lower("fn f(text: &str):\n\tprint(text)\n").unwrap_err();
        assert!(
            diags
                .iter()
                .any(|d| d.message.contains("`&str` was renamed to `strview`"))
        );
    }

    #[test]
    fn lower_view_of_non_str_rejected() {
        // Legacy `&name` view syntax (M8.4 pre-Q5): only `&str` was ever
        // valid, and it is now a targeted rename error (final spec Q5);
        // `&int` must be an unknown-view-type diagnostic, not a silent
        // fallthrough to `int`.
        let diags = parse_and_lower("fn f(x: &int):\n\tprint(\"\")\n").unwrap_err();
        assert!(diags.iter().any(|d| d.code == DiagCode::UnknownType));
    }

    #[test]
    fn bytes_literal_lowers_to_uir() {
        let (uir, pool) = parse_and_lower("b = b\"A\\x00\"\n").expect("lower ok");
        let body = body_named(&uir, &pool, "main");
        let _ = body;
        let mut found = false;
        for idx in 1..uir.instructions.len() {
            let r = InstRef::from_raw(u32::try_from(idx).expect("idx fits u32"));
            let inst = uir.inst(r);
            if inst.tag == InstTag::BytesLiteral {
                let InstData::Str(id) = inst.data else {
                    panic!("BytesLiteral must carry InstData::Str");
                };
                assert_eq!(pool.bytes_payload(id), b"A\x00");
                found = true;
            }
        }
        assert!(found, "no BytesLiteral instruction emitted");
    }

    #[test]
    fn struct_decl_registered_with_resolved_fields() {
        let src =
            "struct Point:\n\tx: float\n\ty: float\n\nfn main():\n\tp = Point{x=1.0, y=2.0}\n";
        let (uir, pool) = parse_and_lower(src).unwrap();
        assert_eq!(uir.struct_decls.len(), 1);
        let decl = &uir.struct_decls[0];
        assert_eq!(pool.str(decl.name), "Point");
        assert_eq!(decl.fields.len(), 2);
        assert_eq!(decl.fields[0].ty, pool.float());
        assert!(pool.is_defined_struct(decl.ty));
    }

    #[test]
    fn struct_literal_and_field_access_lower() {
        let src = "struct Point:\n\tx: float\n\np = Point{x=1.0}\nq = p.x\n";
        let (uir, pool) = parse_and_lower(src).unwrap();
        let main = body_named(&uir, &pool, "main");
        let stmts = uir.body_stmts(main);
        let decl = uir.var_decl_view(stmts[0]);
        let lit = uir.struct_lit_view(decl.initializer);
        assert_eq!(pool.str(lit.name), "Point");
        assert_eq!(lit.fields.len(), 1);
        assert_eq!(pool.str(lit.fields[0].0), "x");
        let q = uir.var_decl_view(stmts[1]);
        assert!(matches!(uir.inst(q.initializer).tag, InstTag::FieldAccess));
    }

    #[test]
    fn struct_value_cycle_is_diagnosed() {
        let err = parse_and_lower("struct Node:\n\tnext: Node\n").unwrap_err();
        assert!(err.iter().any(|d| d.code == DiagCode::InfiniteSize));
    }

    #[test]
    fn mutual_struct_value_cycle_is_diagnosed() {
        let err = parse_and_lower("struct A:\n\tb: B\n\nstruct B:\n\ta: A\n").unwrap_err();
        assert!(err.iter().any(|d| d.code == DiagCode::InfiniteSize));
    }

    #[test]
    fn forward_struct_reference_resolves() {
        // Field types may name structs declared later in the file:
        // the pre-scan declares every struct before any is defined.
        let src = "struct Line:\n\tstart: Point\n\nstruct Point:\n\tx: float\n";
        let (uir, pool) = parse_and_lower(src).unwrap();
        assert_eq!(uir.struct_decls.len(), 2);
        let line = &uir.struct_decls[0];
        let point = &uir.struct_decls[1];
        assert_eq!(line.fields[0].ty, point.ty);
        assert!(pool.is_defined_struct(line.ty));
        assert!(pool.is_defined_struct(point.ty));
    }

    #[test]
    fn duplicate_struct_declaration_is_diagnosed() {
        let err = parse_and_lower("struct P:\n\tx: int\n\nstruct P:\n\ty: int\n").unwrap_err();
        assert!(err.iter().any(|d| d.code == DiagCode::DuplicateDeclaration));
    }

    #[test]
    fn unknown_struct_field_type_is_diagnosed_without_defining() {
        // An unresolvable field type must not reach
        // `pool.define_struct` (its layout computation has no
        // answer for the error type).
        let err = parse_and_lower("struct Bad:\n\tx: nope\n").unwrap_err();
        assert!(err.iter().any(|d| d.code == DiagCode::UnknownType));
    }

    #[test]
    fn struct_decl_does_not_trip_explicit_main_check() {
        parse_and_lower("struct Point:\n\tx: float\n\nfn main():\n\tprint(\"hi\")\n")
            .expect("struct decl alongside fn main should lower cleanly");
    }

    #[test]
    fn struct_only_file_lowers_to_empty_main() {
        let (uir, _pool) = parse_and_lower("struct Point:\n\tx: float\n").unwrap();
        assert_eq!(uir.struct_decls.len(), 1);
        assert_eq!(uir.func_bodies.len(), 1);
        assert!(uir.body_stmts(&uir.func_bodies[0]).is_empty());
    }

    #[test]
    fn bytes_annotations_resolve() {
        parse_and_lower("fn f(b: bytes, v: bytesview):\n\treturn\n").expect("lower ok");
    }

    #[test]
    fn bytes_index_lowers_to_uir() {
        let (uir, _pool) = parse_and_lower("b = b\"\\x01\"\nx = b[0]\n").expect("lower ok");
        let mut found = false;
        for idx in 1..uir.instructions.len() {
            let r = InstRef::from_raw(u32::try_from(idx).expect("idx fits u32"));
            if uir.inst(r).tag == InstTag::Index {
                found = true;
            }
        }
        assert!(found, "no Index instruction emitted");
        // Slice form must still parse as Slice (grammar disambiguation).
        let (uir2, _p2) = parse_and_lower("b = b\"\\x01\"\nv = b[0:1]\n").expect("lower ok");
        let mut slices = 0;
        for idx in 1..uir2.instructions.len() {
            let r = InstRef::from_raw(u32::try_from(idx).expect("idx fits u32"));
            if uir2.inst(r).tag == InstTag::Slice {
                slices += 1;
            }
        }
        assert_eq!(slices, 1, "b[0:1] must stay a Slice");
    }
}
