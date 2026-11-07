//! Expression lowering: the recursive `gen_expr`/`gen_expr_d` and the
//! iterative deep-nesting fallback `gen_expr_machine`.
//!
//! Split out of `astgen.rs` to keep that file under the line-count
//! gate; see the module docs there for the lowering pipeline.

use super::{TypeResolver, errors, match_, variants};
use ryo_core::ast;
use ryo_core::diag::DiagSink;
use ryo_core::spine::ChainSpine;
use ryo_core::types::{InternPool, StringId, TypeId};
use ryo_core::uir::{InstRef, InstTag, UirBuilder};
use std::collections::HashSet;

/// Expression nesting depth the recursive lowering may consume before
/// handing the remaining subtree to the iterative machine. Far below
/// the compiler thread's 32 MiB stack (a frame per link is kilobytes in
/// debug), far above anything a hand-written expression nests.
const EXPR_DEPTH_LIMIT: u32 = 64;

#[cfg(test)]
std::thread_local! {
    static TEST_EXPR_LIMIT: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
    static TEST_MACHINE_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_machine_call_count() {
    TEST_MACHINE_CALLS.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn machine_call_count() -> usize {
    TEST_MACHINE_CALLS.with(|count| count.get())
}

#[cfg(test)]
pub(crate) fn with_expr_limit<T>(limit: u32, f: impl FnOnce() -> T) -> T {
    struct Reset(Option<u32>);
    impl Drop for Reset {
        fn drop(&mut self) {
            TEST_EXPR_LIMIT.with(|current| current.set(self.0));
        }
    }

    let previous = TEST_EXPR_LIMIT.with(|current| current.replace(Some(limit)));
    let _reset = Reset(previous);
    f()
}

fn expr_depth_limit() -> u32 {
    #[cfg(test)]
    if let Some(limit) = TEST_EXPR_LIMIT.with(|limit| limit.get()) {
        return limit;
    }
    EXPR_DEPTH_LIMIT
}

/// Lower one expression to a UIR inst.
#[inline]
pub(super) fn gen_expr(
    b: &mut UirBuilder,
    ast: &ast::Ast,
    expr: ast::ExprId,
    types: &TypeResolver,
    defined_layouts: &HashSet<TypeId>,
    pool: &mut InternPool,
    sink: &mut DiagSink,
) -> InstRef {
    gen_expr_d(b, ast, expr, types, defined_layouts, pool, sink, 0)
}

#[allow(clippy::too_many_arguments)]
fn gen_expr_d(
    b: &mut UirBuilder,
    ast: &ast::Ast,
    expr: ast::ExprId,
    types: &TypeResolver,
    defined_layouts: &HashSet<TypeId>,
    pool: &mut InternPool,
    sink: &mut DiagSink,
    depth: u32,
) -> InstRef {
    if depth == expr_depth_limit() {
        // Deep enough to threaten the stack — finish the subtree
        // iteratively (see `gen_expr_machine`).
        return gen_expr_machine(b, ast, expr, types, defined_layouts, pool, sink);
    }
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
            let l = gen_expr_d(b, ast, lhs, types, defined_layouts, pool, sink, depth + 1);
            let r = gen_expr_d(b, ast, rhs, types, defined_layouts, pool, sink, depth + 1);
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
            let s = gen_expr_d(
                b,
                ast,
                operand,
                types,
                defined_layouts,
                pool,
                sink,
                depth + 1,
            );
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
                .map(|&a| gen_expr_d(b, ast, a, types, defined_layouts, pool, sink, depth + 1))
                .collect();
            b.call(name, &arg_refs, span)
        }
        ast::ExprKind::MethodCall {
            receiver,
            method,
            args,
        } => {
            let receiver_ref = gen_expr_d(
                b,
                ast,
                receiver,
                types,
                defined_layouts,
                pool,
                sink,
                depth + 1,
            );
            let arg_refs: Vec<InstRef> = ast
                .expr_list(args)
                .iter()
                .map(|&a| gen_expr_d(b, ast, a, types, defined_layouts, pool, sink, depth + 1))
                .collect();
            b.method_call(receiver_ref, method, &arg_refs, span)
        }
        ast::ExprKind::Borrow(inner) => {
            let inner_ref =
                gen_expr_d(b, ast, inner, types, defined_layouts, pool, sink, depth + 1);
            b.borrow(inner_ref, span)
        }
        ast::ExprKind::Slice { base, start, end } => {
            // Slice projection `base[start:end]` (final spec §3);
            // bounds are optional shorthands. Sema type-checks the
            // base and yields `strview`.
            let base_ref = gen_expr_d(b, ast, base, types, defined_layouts, pool, sink, depth + 1);
            let start_ref =
                start.map(|e| gen_expr_d(b, ast, e, types, defined_layouts, pool, sink, depth + 1));
            let end_ref =
                end.map(|e| gen_expr_d(b, ast, e, types, defined_layouts, pool, sink, depth + 1));
            b.slice(base_ref, start_ref, end_ref, span)
        }
        ast::ExprKind::Index { base, index } => {
            // Scalar indexing `base[index]` (M8.4.2). Sema gates the
            // base to bytes/bytesview.
            let base_ref = gen_expr_d(b, ast, base, types, defined_layouts, pool, sink, depth + 1);
            let index_ref =
                gen_expr_d(b, ast, index, types, defined_layouts, pool, sink, depth + 1);
            b.index(base_ref, index_ref, span)
        }
        // Struct literal `Name{field=value, ...}` (M9) or the
        // anonymous `{field=value, ...}` (M10). The field
        // initializers stay in source order; sema canonicalizes them
        // against the declaration in `uir.struct_decls` (named) or
        // infers the structural type from the field values
        // (anonymous — the UIR name slot carries the
        // `UIR_ANON_STRUCT_NAME` sentinel).
        ast::ExprKind::StructLiteral(lit) => {
            let fields: Vec<(StringId, InstRef)> = ast
                .struct_field_inits(lit.fields)
                .iter()
                .map(|&(fname, e)| {
                    (
                        fname,
                        gen_expr_d(b, ast, e, types, defined_layouts, pool, sink, depth + 1),
                    )
                })
                .collect();
            let name = lit.name.map(|ident| ident.name);
            b.struct_lit(name, &fields, span)
        }
        // Field access `object.field` (M9); chains fold left in the
        // AST, so each access lowers against its own object.
        ast::ExprKind::FieldAccess { object, field } => {
            if let ast::ExprKind::Ident(root) = ast.expr(object).kind
                && types.error_family_members.contains_key(&root)
            {
                errors::lower_qualified_error_construct(
                    b,
                    ast,
                    ast::Ident::new(root, ast.expr_span(object)),
                    field,
                    None,
                    types,
                    defined_layouts,
                    pool,
                    sink,
                    span,
                )
            } else {
                let obj = gen_expr_d(
                    b,
                    ast,
                    object,
                    types,
                    defined_layouts,
                    pool,
                    sink,
                    depth + 1,
                );
                b.field_access(obj, field.name, span)
            }
        }
        // Variant construction (M11) lowers to `EnumLit`.
        ast::ExprKind::VariantConstruct(c) => {
            if types.error_family_members.contains_key(&c.enum_name.name) {
                errors::lower_qualified_error_construct(
                    b,
                    ast,
                    c.enum_name,
                    c.variant,
                    c.args,
                    types,
                    defined_layouts,
                    pool,
                    sink,
                    span,
                )
            } else {
                variants::lower_variant_construct(
                    b,
                    ast,
                    c,
                    types,
                    defined_layouts,
                    pool,
                    sink,
                    span,
                )
            }
        }
        // Match expressions (M12) lower to the UIR `Match` wire; see
        // `astgen/match_.rs` for the pattern/body mapping.
        ast::ExprKind::Match { scrutinee, arms } => match_::gen_match(
            b,
            ast,
            scrutinee,
            arms,
            types,
            defined_layouts,
            pool,
            sink,
            span,
        ),
        ast::ExprKind::ErrorConstruct(c) => {
            errors::lower_error_construct(b, ast, c, types, defined_layouts, pool, sink, span)
        }
        ast::ExprKind::Try(operand) => {
            let operand = gen_expr_d(
                b,
                ast,
                operand,
                types,
                defined_layouts,
                pool,
                sink,
                depth + 1,
            );
            b.unary(InstTag::Try, operand, span)
        }
        ast::ExprKind::Catch(catch) => {
            match_::gen_catch(b, ast, catch, types, defined_layouts, pool, sink, span)
        }
    }
}

/// One unit of expr-lowering work for [`gen_expr`]'s explicit stack.
///
/// `Enter` visits a node, scheduling its children and an `Exit`;
/// `Exit` emits the node's own instruction from the children's
/// lowered refs (source-order post-order, exactly as the old
/// recursive descent produced).
enum ExprTask {
    Enter(ast::ExprId),
    Exit(ast::ExprId),
    Variant(variants::VariantState),
    Error(errors::ErrorState),
}

/// Iterative lowering machine used as the deep-nesting fallback for
/// [`gen_expr`]: once expression nesting passes [`EXPR_DEPTH_LIMIT`],
/// the recursion hands the remaining subtree to this explicit-stack
/// walker (a 100k-term operator chain nests one link per term — deep
/// enough to overflow the compiler's stack long before sema sees it).
/// Common shapes never reach here; they stay on the recursive path.
///
/// The AST is a tree of arbitrary depth (a 100k-term operator chain
/// nests 100k `BinaryOp` lhs links), so a recursive descent overflows
/// the stack long before sema sees the program. The machine below
/// keeps the recursion on the explicit `stack`; each `Exit` finds its
/// children's refs on `results` in source order. Variant and error
/// constructors retain their argument state on that same stack. Match
/// and catch still delegate to their helpers, which re-enter `gen_expr`
/// per sub-expression.
fn gen_expr_machine(
    b: &mut UirBuilder,
    ast: &ast::Ast,
    expr: ast::ExprId,
    types: &TypeResolver,
    defined_layouts: &HashSet<TypeId>,
    pool: &mut InternPool,
    sink: &mut DiagSink,
) -> InstRef {
    #[cfg(test)]
    TEST_MACHINE_CALLS.with(|count| count.set(count.get() + 1));
    // Both stacks stay inline for expression-sized shapes; deep chains
    // spill to the heap. No allocation at all for ordinary expressions.
    let mut stack: ChainSpine<ExprTask> = ChainSpine::new();
    stack.push(ExprTask::Enter(expr));
    // Children refs, consumed by `Exit` in source order. One slot per
    // node on the current path, so the footprint mirrors the old
    // recursion's locals.
    let mut results: ChainSpine<InstRef> = ChainSpine::new();
    // Scratch for variadic `Exit` arms (call/struct-literal args); no
    // heap until a variadic node actually pops.
    let mut arity_buf: Vec<InstRef> = Vec::new();
    while let Some(task) = stack.pop() {
        match task {
            ExprTask::Enter(e) => {
                let span = ast.expr_span(e);
                match ast.expr(e).kind {
                    ast::ExprKind::Literal(lit) => results.push(match lit {
                        ast::Literal::Int(n) => b.int_literal(n, span),
                        ast::Literal::Str(id) => b.str_literal(id, span),
                        ast::Literal::Bool(v) => b.bool_literal(v, span),
                        ast::Literal::Float(v) => b.float_literal(v, span),
                        ast::Literal::Bytes(id) => b.bytes_literal(id, span),
                    }),
                    ast::ExprKind::Ident(name) => results.push(b.var_ref(name, span)),
                    ast::ExprKind::BinaryOp(lhs, _, rhs) => {
                        stack.push(ExprTask::Exit(e));
                        stack.push(ExprTask::Enter(rhs));
                        stack.push(ExprTask::Enter(lhs));
                    }
                    ast::ExprKind::UnaryOp(_, operand) => {
                        stack.push(ExprTask::Exit(e));
                        stack.push(ExprTask::Enter(operand));
                    }
                    ast::ExprKind::Call(_, args) => {
                        stack.push(ExprTask::Exit(e));
                        for &a in ast.expr_list(args).iter().rev() {
                            stack.push(ExprTask::Enter(a));
                        }
                    }
                    ast::ExprKind::MethodCall { receiver, args, .. } => {
                        stack.push(ExprTask::Exit(e));
                        for &a in ast.expr_list(args).iter().rev() {
                            stack.push(ExprTask::Enter(a));
                        }
                        stack.push(ExprTask::Enter(receiver));
                    }
                    ast::ExprKind::Borrow(inner) => {
                        stack.push(ExprTask::Exit(e));
                        stack.push(ExprTask::Enter(inner));
                    }
                    ast::ExprKind::Slice { base, start, end } => {
                        // Slice projection `base[start:end]` (final spec §3);
                        // bounds are optional shorthands. Sema type-checks the
                        // base and yields `strview`.
                        stack.push(ExprTask::Exit(e));
                        if let Some(end) = end {
                            stack.push(ExprTask::Enter(end));
                        }
                        if let Some(start) = start {
                            stack.push(ExprTask::Enter(start));
                        }
                        stack.push(ExprTask::Enter(base));
                    }
                    ast::ExprKind::Index { base, index } => {
                        // Scalar indexing `base[index]` (M8.4.2). Sema gates the
                        // base to bytes/bytesview.
                        stack.push(ExprTask::Exit(e));
                        stack.push(ExprTask::Enter(index));
                        stack.push(ExprTask::Enter(base));
                    }
                    // Struct literal `Name{field=value, ...}` (M9) or the
                    // anonymous `{field=value, ...}` (M10). The field
                    // initializers stay in source order; sema canonicalizes them
                    // against the declaration in `uir.struct_decls` (named) or
                    // infers the structural type from the field values
                    // (anonymous — the UIR name slot carries the
                    // `UIR_ANON_STRUCT_NAME` sentinel).
                    ast::ExprKind::StructLiteral(lit) => {
                        stack.push(ExprTask::Exit(e));
                        for &(_, field_expr) in ast.struct_field_inits(lit.fields).iter().rev() {
                            stack.push(ExprTask::Enter(field_expr));
                        }
                    }
                    // Field access `object.field` (M9); chains fold left in the
                    // AST, so each access lowers against its own object.
                    ast::ExprKind::FieldAccess { object, field } => {
                        if let ast::ExprKind::Ident(root) = ast.expr(object).kind
                            && types.error_family_members.contains_key(&root)
                        {
                            let step = errors::prepare_qualified_error_construct(
                                b,
                                ast,
                                ast::Ident::new(root, ast.expr_span(object)),
                                field,
                                None,
                                types,
                                pool,
                                sink,
                                span,
                            );
                            match step {
                                errors::ErrorStep::Complete(result) => results.push(result),
                                errors::ErrorStep::Child(state, arg) => {
                                    stack.push(ExprTask::Error(state));
                                    stack.push(ExprTask::Enter(arg));
                                }
                            }
                        } else {
                            stack.push(ExprTask::Exit(e));
                            stack.push(ExprTask::Enter(object));
                        }
                    }
                    // Variant construction (M11) lowers to `EnumLit`.
                    ast::ExprKind::VariantConstruct(c) => {
                        if types.error_family_members.contains_key(&c.enum_name.name) {
                            let step = errors::prepare_qualified_error_construct(
                                b,
                                ast,
                                c.enum_name,
                                c.variant,
                                c.args,
                                types,
                                pool,
                                sink,
                                span,
                            );
                            match step {
                                errors::ErrorStep::Complete(result) => results.push(result),
                                errors::ErrorStep::Child(state, arg) => {
                                    stack.push(ExprTask::Error(state));
                                    stack.push(ExprTask::Enter(arg));
                                }
                            }
                        } else {
                            let step = variants::prepare_variant_construct(
                                b,
                                ast,
                                c,
                                types,
                                defined_layouts,
                                pool,
                                sink,
                                span,
                            );
                            match step {
                                variants::VariantStep::Complete(result) => results.push(result),
                                variants::VariantStep::Child(state, arg) => {
                                    stack.push(ExprTask::Variant(state));
                                    stack.push(ExprTask::Enter(arg));
                                }
                            }
                        }
                    }
                    // Match expressions (M12) lower to the UIR `Match` wire; see
                    // `astgen/match_.rs` for the pattern/body mapping.
                    ast::ExprKind::Match { scrutinee, arms } => results.push(match_::gen_match(
                        b,
                        ast,
                        scrutinee,
                        arms,
                        types,
                        defined_layouts,
                        pool,
                        sink,
                        span,
                    )),
                    ast::ExprKind::ErrorConstruct(c) => {
                        let step =
                            errors::prepare_error_construct(b, ast, c, types, pool, sink, span);
                        match step {
                            errors::ErrorStep::Complete(result) => results.push(result),
                            errors::ErrorStep::Child(state, arg) => {
                                stack.push(ExprTask::Error(state));
                                stack.push(ExprTask::Enter(arg));
                            }
                        }
                    }
                    ast::ExprKind::Try(operand) => {
                        stack.push(ExprTask::Exit(e));
                        stack.push(ExprTask::Enter(operand));
                    }
                    ast::ExprKind::Catch(catch) => results.push(match_::gen_catch(
                        b,
                        ast,
                        catch,
                        types,
                        defined_layouts,
                        pool,
                        sink,
                        span,
                    )),
                }
            }
            ExprTask::Variant(state) => {
                let value = results.pop().expect("variant argument lowered");
                let step =
                    variants::resume_variant_construct(state, value, b, ast, types, pool, sink);
                match step {
                    variants::VariantStep::Complete(result) => results.push(result),
                    variants::VariantStep::Child(state, arg) => {
                        stack.push(ExprTask::Variant(state));
                        stack.push(ExprTask::Enter(arg));
                    }
                }
            }
            ExprTask::Error(state) => {
                let value = results.pop().expect("error argument lowered");
                let step = errors::resume_error_construct(state, value, b, ast, pool);
                match step {
                    errors::ErrorStep::Complete(result) => results.push(result),
                    errors::ErrorStep::Child(state, arg) => {
                        stack.push(ExprTask::Error(state));
                        stack.push(ExprTask::Enter(arg));
                    }
                }
            }
            ExprTask::Exit(e) => {
                let span = ast.expr_span(e);
                match ast.expr(e).kind {
                    ast::ExprKind::BinaryOp(_, op, _) => {
                        let r = results.pop().expect("rhs lowered");
                        let l = results.pop().expect("lhs lowered");
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
                        results.push(b.binary(tag, l, r, span));
                    }
                    ast::ExprKind::UnaryOp(op, _) => {
                        let s = results.pop().expect("operand lowered");
                        let tag = match op {
                            ast::UnaryOperator::Neg => InstTag::Neg,
                            ast::UnaryOperator::Not => InstTag::Not,
                        };
                        results.push(b.unary(tag, s, span));
                    }
                    ast::ExprKind::Call(name, args) => {
                        let args = ast.expr_list(args);
                        results.pop_n(args.len(), &mut arity_buf);
                        results.push(b.call(name, &arity_buf, span));
                    }
                    ast::ExprKind::MethodCall {
                        receiver: _,
                        method,
                        args,
                    } => {
                        let args = ast.expr_list(args);
                        results.pop_n(args.len(), &mut arity_buf);
                        let receiver_ref = results.pop().expect("receiver lowered");
                        results.push(b.method_call(receiver_ref, method, &arity_buf, span));
                    }
                    ast::ExprKind::Borrow(_) => {
                        let inner_ref = results.pop().expect("borrow target lowered");
                        results.push(b.borrow(inner_ref, span));
                    }
                    ast::ExprKind::Slice { start, end, .. } => {
                        let end_ref = end.map(|_| results.pop().expect("end bound lowered"));
                        let start_ref = start.map(|_| results.pop().expect("start bound lowered"));
                        let base_ref = results.pop().expect("slice base lowered");
                        results.push(b.slice(base_ref, start_ref, end_ref, span));
                    }
                    ast::ExprKind::Index { .. } => {
                        let index_ref = results.pop().expect("index lowered");
                        let base_ref = results.pop().expect("index base lowered");
                        results.push(b.index(base_ref, index_ref, span));
                    }
                    ast::ExprKind::StructLiteral(lit) => {
                        let inits = ast.struct_field_inits(lit.fields);
                        results.pop_n(inits.len(), &mut arity_buf);
                        let fields: Vec<(StringId, InstRef)> = inits
                            .iter()
                            .map(|&(fname, _)| fname)
                            .zip(arity_buf.iter().copied())
                            .collect();
                        let name = lit.name.map(|ident| ident.name);
                        results.push(b.struct_lit(name, &fields, span));
                    }
                    ast::ExprKind::FieldAccess { field, .. } => {
                        let obj = results.pop().expect("field object lowered");
                        results.push(b.field_access(obj, field.name, span));
                    }
                    ast::ExprKind::Try(_) => {
                        let operand = results.pop().expect("try operand lowered");
                        results.push(b.unary(InstTag::Try, operand, span));
                    }
                    other => unreachable!("non-deferred kind reached gen_expr Exit: {other:?}"),
                }
            }
        }
    }
    results.pop().expect("one root ref remains")
}
