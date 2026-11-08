//! Expression, statement, and block lowering: recursive `gen_expr_d` and
//! statement helpers hand deep nesting to the shared iterative `gen_machine`.
//! Expressions, statement bodies, and block continuations stay on one explicit
//! stack so nested matches and catches do not start fresh lowering machines.
//!
//! Split out of `astgen.rs` to keep that file under the line-count
//! gate; see the module docs there for the lowering pipeline.

use super::{TypeResolver, errors, lower_destructure_pattern, match_, variants};
use ryo_core::ast;
use ryo_core::diag::{Diag, DiagCode, DiagSink};
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

pub(super) fn expr_depth_limit() -> u32 {
    #[cfg(test)]
    if let Some(limit) = TEST_EXPR_LIMIT.with(|limit| limit.get()) {
        return limit;
    }
    EXPR_DEPTH_LIMIT
}

#[allow(clippy::too_many_arguments)]
pub(super) fn gen_expr_d(
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
                    depth,
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
                    depth,
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
                    depth,
                )
            }
        }
        // Match expressions (M12) lower to the UIR `Match` wire; see
        // `astgen/match_.rs` for the pattern/body mapping.
        ast::ExprKind::Match { scrutinee, arms } => {
            if depth == 0 {
                match_::gen_match(
                    b,
                    ast,
                    scrutinee,
                    arms,
                    types,
                    defined_layouts,
                    pool,
                    sink,
                    span,
                )
            } else {
                match_::gen_match_d(
                    b,
                    ast,
                    scrutinee,
                    arms,
                    types,
                    defined_layouts,
                    pool,
                    sink,
                    span,
                    depth,
                )
            }
        }
        ast::ExprKind::ErrorConstruct(c) => errors::lower_error_construct(
            b,
            ast,
            c,
            types,
            defined_layouts,
            pool,
            sink,
            span,
            depth,
        ),
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
            if depth == 0 {
                match_::gen_catch(b, ast, catch, types, defined_layouts, pool, sink, span)
            } else {
                match_::gen_catch_d(
                    b,
                    ast,
                    catch,
                    types,
                    defined_layouts,
                    pool,
                    sink,
                    span,
                    depth,
                )
            }
        }
    }
}

struct BlockState {
    list: Option<ast::StmtList>,
    next_stmt: usize,
    refs: Vec<InstRef>,
}

impl BlockState {
    fn new(list: ast::StmtList) -> Self {
        Self {
            list: Some(list),
            next_stmt: 0,
            refs: Vec::new(),
        }
    }

    fn statement_root() -> Self {
        Self {
            list: None,
            next_stmt: 0,
            refs: Vec::new(),
        }
    }
}

struct StmtState {
    stmt: ast::StmtId,
    block: BlockState,
    cond: Option<InstRef>,
    value: Option<InstRef>,
    then_stmts: Option<Vec<InstRef>>,
    elif_branches: Vec<(InstRef, Vec<InstRef>)>,
    next_elif: usize,
}

impl StmtState {
    fn new(stmt: ast::StmtId, block: BlockState) -> Self {
        Self {
            stmt,
            block,
            cond: None,
            value: None,
            then_stmts: None,
            elif_branches: Vec::new(),
            next_elif: 0,
        }
    }
}

/// One unit of expression, statement, or block work on the active stack.
enum ExprTask {
    Enter(ast::ExprId),
    Exit(ast::ExprId),
    Variant(variants::VariantState),
    Error(errors::ErrorState),
    MatchScrutinee(match_::MatchState),
    MatchBody(match_::MatchState),
    MatchStmts(match_::MatchState),
    CatchOperand(match_::CatchState),
    CatchBody(match_::CatchState),
    CatchStmts(match_::CatchState),
    CatchMatch(match_::CatchState),
    BlockEnter(ast::StmtList),
    BlockNext(BlockState),
    StmtEnter(StmtState),
    StmtValue(StmtState),
    StmtTarget(StmtState),
    StmtBody(StmtState),
    ElifCond(StmtState),
    ElifBody(StmtState),
    IfElse(StmtState),
    ForStart(StmtState),
}

fn statement_done(stack: &mut ChainSpine<ExprTask>, mut state: StmtState, result: Option<InstRef>) {
    if let Some(result) = result {
        state.block.refs.push(result);
    }
    stack.push(ExprTask::BlockNext(state.block));
}

fn advance_block(
    stack: &mut ChainSpine<ExprTask>,
    block_results: &mut ChainSpine<Vec<InstRef>>,
    ast: &ast::Ast,
    mut block: BlockState,
) {
    if let Some(list) = block.list {
        let stmts = ast.stmt_list(list);
        if block.next_stmt < stmts.len() {
            let stmt = stmts[block.next_stmt];
            block.next_stmt += 1;
            stack.push(ExprTask::StmtEnter(StmtState::new(stmt, block)));
            return;
        }
    }
    block_results.push(block.refs);
}

fn finish_if(
    state: &mut StmtState,
    else_stmts: Option<Vec<InstRef>>,
    b: &mut UirBuilder,
    ast: &ast::Ast,
) -> InstRef {
    let then_stmts = state.then_stmts.take().expect("if then block lowered");
    b.if_stmt(
        state.cond.take().expect("if condition lowered"),
        &then_stmts,
        &state.elif_branches,
        else_stmts.as_deref(),
        ast.stmt_span(state.stmt),
    )
}

fn schedule_if_branches(
    stack: &mut ChainSpine<ExprTask>,
    mut state: StmtState,
    b: &mut UirBuilder,
    ast: &ast::Ast,
) {
    let ast::StmtKind::IfStmt(if_stmt) = &ast.stmt(state.stmt).kind else {
        unreachable!("if continuation for non-if statement")
    };
    let branches = ast.elif_list(if_stmt.elif_branches);
    if state.next_elif < branches.len() {
        let cond = branches[state.next_elif].cond;
        stack.push(ExprTask::ElifCond(state));
        stack.push(ExprTask::Enter(cond));
    } else if let Some(else_block) = if_stmt.else_block {
        stack.push(ExprTask::IfElse(state));
        stack.push(ExprTask::BlockEnter(else_block));
    } else {
        let result = finish_if(&mut state, None, b, ast);
        statement_done(stack, state, Some(result));
    }
}

fn push_match_step(
    stack: &mut ChainSpine<ExprTask>,
    results: &mut ChainSpine<InstRef>,
    step: match_::MatchStep,
) {
    match step {
        match_::MatchStep::Complete(result) => results.push(result),
        match_::MatchStep::Body(state, expr) => {
            stack.push(ExprTask::MatchBody(state));
            stack.push(ExprTask::Enter(expr));
        }
        match_::MatchStep::Stmts(state, list) => {
            stack.push(ExprTask::MatchStmts(state));
            stack.push(ExprTask::BlockEnter(list));
        }
    }
}

#[derive(Clone, Copy)]
enum MachineRoot {
    Expr(ast::ExprId),
    Stmt(ast::StmtId),
    Block(ast::StmtList),
}

enum MachineResult {
    Expr(InstRef),
    Stmts(Vec<InstRef>),
}

fn gen_expr_machine(
    b: &mut UirBuilder,
    ast: &ast::Ast,
    expr: ast::ExprId,
    types: &TypeResolver,
    defined_layouts: &HashSet<TypeId>,
    pool: &mut InternPool,
    sink: &mut DiagSink,
) -> InstRef {
    match gen_machine(
        b,
        ast,
        MachineRoot::Expr(expr),
        types,
        defined_layouts,
        pool,
        sink,
    ) {
        MachineResult::Expr(result) => result,
        MachineResult::Stmts(_) => unreachable!("expression root returned statements"),
    }
}

pub(super) fn gen_stmt_machine(
    b: &mut UirBuilder,
    ast: &ast::Ast,
    stmt: ast::StmtId,
    types: &TypeResolver,
    defined_layouts: &HashSet<TypeId>,
    pool: &mut InternPool,
    sink: &mut DiagSink,
) -> Vec<InstRef> {
    match gen_machine(
        b,
        ast,
        MachineRoot::Stmt(stmt),
        types,
        defined_layouts,
        pool,
        sink,
    ) {
        MachineResult::Stmts(result) => result,
        MachineResult::Expr(_) => unreachable!("statement root returned an expression"),
    }
}

pub(super) fn gen_block_machine(
    b: &mut UirBuilder,
    ast: &ast::Ast,
    list: ast::StmtList,
    types: &TypeResolver,
    defined_layouts: &HashSet<TypeId>,
    pool: &mut InternPool,
    sink: &mut DiagSink,
) -> Vec<InstRef> {
    match gen_machine(
        b,
        ast,
        MachineRoot::Block(list),
        types,
        defined_layouts,
        pool,
        sink,
    ) {
        MachineResult::Stmts(result) => result,
        MachineResult::Expr(_) => unreachable!("block root returned an expression"),
    }
}

/// Deep expression, statement, and block nesting shares this explicit-stack
/// walker. Ordinary shallow input stays on the recursive path.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn gen_machine(
    b: &mut UirBuilder,
    ast: &ast::Ast,
    root: MachineRoot,
    types: &TypeResolver,
    defined_layouts: &HashSet<TypeId>,
    pool: &mut InternPool,
    sink: &mut DiagSink,
) -> MachineResult {
    #[cfg(test)]
    TEST_MACHINE_CALLS.with(|count| count.set(count.get() + 1));
    let mut stack: ChainSpine<ExprTask> = ChainSpine::new();
    match root {
        MachineRoot::Expr(expr) => stack.push(ExprTask::Enter(expr)),
        MachineRoot::Stmt(stmt) => stack.push(ExprTask::StmtEnter(StmtState::new(
            stmt,
            BlockState::statement_root(),
        ))),
        MachineRoot::Block(list) => stack.push(ExprTask::BlockEnter(list)),
    }
    let mut results: ChainSpine<InstRef> = ChainSpine::new();
    let mut block_results: ChainSpine<Vec<InstRef>> = ChainSpine::new();
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
                    ast::ExprKind::Match { scrutinee, arms } => {
                        stack.push(ExprTask::MatchScrutinee(match_::prepare_match(arms, span)));
                        stack.push(ExprTask::Enter(scrutinee));
                    }
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
                    ast::ExprKind::Catch(catch) => {
                        stack.push(ExprTask::CatchOperand(match_::prepare_catch(catch, span)));
                        stack.push(ExprTask::Enter(catch.operand));
                    }
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
            ExprTask::MatchScrutinee(state) => {
                let scrutinee = results.pop().expect("match scrutinee lowered");
                let step = match_::resume_match_scrutinee(
                    state,
                    scrutinee,
                    b,
                    ast,
                    types,
                    defined_layouts,
                    pool,
                    sink,
                );
                push_match_step(&mut stack, &mut results, step);
            }
            ExprTask::MatchBody(state) => {
                let body = results.pop().expect("match body lowered");
                let step = match_::resume_match_body(
                    state,
                    body,
                    b,
                    ast,
                    types,
                    defined_layouts,
                    pool,
                    sink,
                );
                push_match_step(&mut stack, &mut results, step);
            }
            ExprTask::MatchStmts(state) => {
                let body = block_results.pop().expect("match statement body lowered");
                let step = match_::resume_match_stmts(
                    state,
                    body,
                    b,
                    ast,
                    types,
                    defined_layouts,
                    pool,
                    sink,
                );
                push_match_step(&mut stack, &mut results, step);
            }
            ExprTask::CatchOperand(state) => {
                let operand = results.pop().expect("catch operand lowered");
                let step = match_::resume_catch_operand(
                    state,
                    operand,
                    b,
                    ast,
                    types,
                    defined_layouts,
                    pool,
                    sink,
                );
                match step {
                    match_::CatchStep::Body(state, expr) => {
                        stack.push(ExprTask::CatchBody(state));
                        stack.push(ExprTask::Enter(expr));
                    }
                    match_::CatchStep::Stmts(state, list) => {
                        stack.push(ExprTask::CatchStmts(state));
                        stack.push(ExprTask::BlockEnter(list));
                    }
                    match_::CatchStep::Match(state, step) => {
                        stack.push(ExprTask::CatchMatch(state));
                        push_match_step(&mut stack, &mut results, step);
                    }
                }
            }
            ExprTask::CatchBody(state) => {
                let body = results.pop().expect("catch body lowered");
                results.push(match_::resume_catch_body(state, body, b));
            }
            ExprTask::CatchStmts(state) => {
                let body = block_results.pop().expect("catch statement body lowered");
                results.push(match_::resume_catch_stmts(state, body, b));
            }
            ExprTask::CatchMatch(state) => {
                let direct = results.pop().expect("direct catch match lowered");
                results.push(match_::resume_catch_match(state, direct, b));
            }
            ExprTask::BlockEnter(list) => {
                advance_block(&mut stack, &mut block_results, ast, BlockState::new(list));
            }
            ExprTask::BlockNext(block) => {
                advance_block(&mut stack, &mut block_results, ast, block);
            }
            ExprTask::StmtEnter(state) => {
                let span = ast.stmt_span(state.stmt);
                match &ast.stmt(state.stmt).kind {
                    ast::StmtKind::VarDecl(decl) => {
                        stack.push(ExprTask::StmtValue(state));
                        stack.push(ExprTask::Enter(decl.initializer));
                    }
                    ast::StmtKind::Return(Some(expr)) => {
                        stack.push(ExprTask::StmtValue(state));
                        stack.push(ExprTask::Enter(*expr));
                    }
                    ast::StmtKind::Return(None) => {
                        let result = b.return_void(span);
                        statement_done(&mut stack, state, Some(result));
                    }
                    ast::StmtKind::ExprStmt(expr) => {
                        stack.push(ExprTask::StmtValue(state));
                        stack.push(ExprTask::Enter(*expr));
                    }
                    ast::StmtKind::FunctionDef(_) => {
                        sink.emit(Diag::error(
                            span,
                            DiagCode::NestedFunctionDef,
                            "nested function definitions are not supported",
                        ));
                        statement_done(&mut stack, state, None);
                    }
                    ast::StmtKind::AssignOrDecl { value, .. }
                    | ast::StmtKind::CompoundAssign { value, .. } => {
                        stack.push(ExprTask::StmtValue(state));
                        stack.push(ExprTask::Enter(*value));
                    }
                    ast::StmtKind::FieldAssign { target, .. }
                    | ast::StmtKind::CompoundFieldAssign { target, .. } => {
                        stack.push(ExprTask::StmtTarget(state));
                        stack.push(ExprTask::Enter(*target));
                    }
                    ast::StmtKind::Destructure { value, .. } => {
                        stack.push(ExprTask::StmtValue(state));
                        stack.push(ExprTask::Enter(*value));
                    }
                    ast::StmtKind::IfStmt(if_stmt) => {
                        stack.push(ExprTask::StmtValue(state));
                        stack.push(ExprTask::Enter(if_stmt.cond));
                    }
                    ast::StmtKind::WhileLoop { cond, .. } => {
                        stack.push(ExprTask::StmtValue(state));
                        stack.push(ExprTask::Enter(*cond));
                    }
                    ast::StmtKind::ForRange {
                        iterator, start, ..
                    } => {
                        if iterator.name != types.range {
                            sink.emit(Diag::error(
                                iterator.span,
                                DiagCode::ParseError,
                                format!(
                                    "only `range(...)` is supported in `for` loops in v0.1, got `{}`",
                                    pool.str(iterator.name),
                                ),
                            ));
                        }
                        stack.push(ExprTask::ForStart(state));
                        stack.push(ExprTask::Enter(*start));
                    }
                    ast::StmtKind::Break => {
                        let result = b.break_stmt(span);
                        statement_done(&mut stack, state, Some(result));
                    }
                    ast::StmtKind::Continue => {
                        let result = b.continue_stmt(span);
                        statement_done(&mut stack, state, Some(result));
                    }
                    ast::StmtKind::Error => {
                        let result = b.unreachable(span);
                        statement_done(&mut stack, state, Some(result));
                    }
                    ast::StmtKind::StructDef(_)
                    | ast::StmtKind::EnumDef(_)
                    | ast::StmtKind::ErrorDef(_) => {
                        statement_done(&mut stack, state, None);
                    }
                    ast::StmtKind::ErrorFamilyDef(def) => {
                        sink.emit(Diag::error(
                            def.name.span,
                            DiagCode::ParseError,
                            "nested error family declarations are not supported",
                        ));
                        statement_done(&mut stack, state, None);
                    }
                }
            }
            ExprTask::StmtValue(mut state) => {
                let value = results.pop().expect("statement expression lowered");
                let span = ast.stmt_span(state.stmt);
                match &ast.stmt(state.stmt).kind {
                    ast::StmtKind::VarDecl(decl) => {
                        let ty = decl
                            .type_annotation
                            .as_ref()
                            .map(|ann| types.resolve_type(ann, ast, pool, sink, defined_layouts));
                        let result = b.var_decl(decl.name.name, decl.mutable, ty, value, span);
                        statement_done(&mut stack, state, Some(result));
                    }
                    ast::StmtKind::Return(Some(_)) => {
                        let result = b.unary(InstTag::Return, value, span);
                        statement_done(&mut stack, state, Some(result));
                    }
                    ast::StmtKind::ExprStmt(_) => {
                        let result = b.unary(InstTag::ExprStmt, value, span);
                        statement_done(&mut stack, state, Some(result));
                    }
                    ast::StmtKind::AssignOrDecl { target, .. } => {
                        let result = b.assign_or_decl(target.name, value, span);
                        statement_done(&mut stack, state, Some(result));
                    }
                    ast::StmtKind::CompoundAssign { target, op, .. } => {
                        let result = b.compound_assign(target.name, *op, value, span);
                        statement_done(&mut stack, state, Some(result));
                    }
                    ast::StmtKind::FieldAssign { .. } => {
                        let target = state.value.take().expect("assignment target lowered");
                        let result = b.field_assign(target, value, span);
                        statement_done(&mut stack, state, Some(result));
                    }
                    ast::StmtKind::CompoundFieldAssign { op, .. } => {
                        let target = state.value.take().expect("assignment target lowered");
                        let result = b.compound_field_assign(target, *op, value, span);
                        statement_done(&mut stack, state, Some(result));
                    }
                    ast::StmtKind::Destructure { target, .. } => {
                        lower_destructure_pattern(
                            b,
                            ast,
                            pool,
                            *target,
                            value,
                            span,
                            sink,
                            &mut state.block.refs,
                        );
                        statement_done(&mut stack, state, None);
                    }
                    ast::StmtKind::IfStmt(if_stmt) => {
                        state.cond = Some(value);
                        stack.push(ExprTask::StmtBody(state));
                        stack.push(ExprTask::BlockEnter(if_stmt.then_block));
                    }
                    ast::StmtKind::WhileLoop { body, .. } => {
                        state.cond = Some(value);
                        stack.push(ExprTask::StmtBody(state));
                        stack.push(ExprTask::BlockEnter(*body));
                    }
                    ast::StmtKind::ForRange { body, .. } => {
                        state.value = Some(value);
                        stack.push(ExprTask::StmtBody(state));
                        stack.push(ExprTask::BlockEnter(*body));
                    }
                    other => unreachable!("unexpected statement value continuation: {other:?}"),
                }
            }
            ExprTask::StmtTarget(mut state) => {
                let target = results.pop().expect("assignment target lowered");
                state.value = Some(target);
                let value = match &ast.stmt(state.stmt).kind {
                    ast::StmtKind::FieldAssign { value, .. }
                    | ast::StmtKind::CompoundFieldAssign { value, .. } => *value,
                    other => unreachable!("unexpected assignment target continuation: {other:?}"),
                };
                stack.push(ExprTask::StmtValue(state));
                stack.push(ExprTask::Enter(value));
            }
            ExprTask::StmtBody(mut state) => {
                let body = block_results.pop().expect("statement body lowered");
                let span = ast.stmt_span(state.stmt);
                match &ast.stmt(state.stmt).kind {
                    ast::StmtKind::IfStmt(_) => {
                        state.then_stmts = Some(body);
                        schedule_if_branches(&mut stack, state, b, ast);
                    }
                    ast::StmtKind::WhileLoop { .. } => {
                        let cond = state.cond.take().expect("while condition lowered");
                        let result = b.while_loop(cond, &body, span);
                        statement_done(&mut stack, state, Some(result));
                    }
                    ast::StmtKind::ForRange { var, .. } => {
                        let start = state.cond.take().expect("range start lowered");
                        let end = state.value.take().expect("range end lowered");
                        let result = b.for_range(var.name, start, end, &body, span);
                        statement_done(&mut stack, state, Some(result));
                    }
                    other => unreachable!("unexpected statement body continuation: {other:?}"),
                }
            }
            ExprTask::ElifCond(mut state) => {
                let cond = results.pop().expect("elif condition lowered");
                state.value = Some(cond);
                let ast::StmtKind::IfStmt(if_stmt) = &ast.stmt(state.stmt).kind else {
                    unreachable!("elif continuation for non-if statement")
                };
                let block = ast.elif_list(if_stmt.elif_branches)[state.next_elif].block;
                stack.push(ExprTask::ElifBody(state));
                stack.push(ExprTask::BlockEnter(block));
            }
            ExprTask::ElifBody(mut state) => {
                let body = block_results.pop().expect("elif body lowered");
                let cond = state.value.take().expect("elif condition lowered");
                state.elif_branches.push((cond, body));
                state.next_elif += 1;
                schedule_if_branches(&mut stack, state, b, ast);
            }
            ExprTask::IfElse(mut state) => {
                let body = block_results.pop().expect("else body lowered");
                let result = finish_if(&mut state, Some(body), b, ast);
                statement_done(&mut stack, state, Some(result));
            }
            ExprTask::ForStart(mut state) => {
                let start = results.pop().expect("range start lowered");
                state.cond = Some(start);
                let ast::StmtKind::ForRange { end, .. } = &ast.stmt(state.stmt).kind else {
                    unreachable!("range continuation for non-range statement")
                };
                stack.push(ExprTask::StmtValue(state));
                stack.push(ExprTask::Enter(*end));
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
                    other => unreachable!("non-deferred kind reached gen_machine Exit: {other:?}"),
                }
            }
        }
    }
    match root {
        MachineRoot::Expr(_) => MachineResult::Expr(results.pop().expect("one root ref remains")),
        MachineRoot::Stmt(_) | MachineRoot::Block(_) => {
            MachineResult::Stmts(block_results.pop().expect("one root block remains"))
        }
    }
}
