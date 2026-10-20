//! AST arena-accounting tests: after a successful parse every arena
//! slot except the slot-0 sentinel must be reachable from `top_level`,
//! which is what makes the `Inspector` no-op hooks safe (see the
//! `Inspector` impl on `ParseState` in `grammar.rs`). Split out of
//! `grammar/tests.rs` to stay under the file-length limit; the shared
//! parse helpers live there as `pub(super)`.

use super::tests::*;
use super::*;

/// Count nodes reachable from `top_level` by following child ids.
fn reachable_node_counts(ast: &Ast) -> (usize, usize) {
    let mut expr_seen = vec![false; ast.expr_count()];
    let mut stmt_seen = vec![false; ast.stmt_count()];
    let mut expr_work: Vec<ExprId> = Vec::new();
    let mut stmt_work: Vec<StmtId> = ast.top_level_stmts().to_vec();
    while let Some(stmt) = stmt_work.pop() {
        if stmt_seen[stmt.index()] {
            continue;
        }
        stmt_seen[stmt.index()] = true;
        match &ast.stmt(stmt).kind {
            StmtKind::VarDecl(decl) => expr_work.push(decl.initializer),
            StmtKind::FunctionDef(def) => {
                stmt_work.extend_from_slice(ast.stmt_list(def.body));
            }
            StmtKind::Return(value) => {
                if let Some(value) = value {
                    expr_work.push(*value);
                }
            }
            StmtKind::ExprStmt(value) => expr_work.push(*value),
            StmtKind::IfStmt(if_stmt) => {
                expr_work.push(if_stmt.cond);
                stmt_work.extend_from_slice(ast.stmt_list(if_stmt.then_block));
                for elif in ast.elif_list(if_stmt.elif_branches) {
                    expr_work.push(elif.cond);
                    stmt_work.extend_from_slice(ast.stmt_list(elif.block));
                }
                if let Some(else_block) = if_stmt.else_block {
                    stmt_work.extend_from_slice(ast.stmt_list(else_block));
                }
            }
            StmtKind::AssignOrDecl { value, .. } | StmtKind::CompoundAssign { value, .. } => {
                expr_work.push(*value);
            }
            StmtKind::Destructure { value, .. } => {
                // The pattern lives in its own arenas and holds no
                // expressions; only the value is reachable here.
                expr_work.push(*value);
            }
            StmtKind::FieldAssign { target, value }
            | StmtKind::CompoundFieldAssign { target, value, .. } => {
                expr_work.push(*target);
                expr_work.push(*value);
            }
            StmtKind::WhileLoop { cond, body } => {
                expr_work.push(*cond);
                stmt_work.extend_from_slice(ast.stmt_list(*body));
            }
            StmtKind::ForRange {
                start, end, body, ..
            } => {
                expr_work.push(*start);
                expr_work.push(*end);
                stmt_work.extend_from_slice(ast.stmt_list(*body));
            }
            StmtKind::Break | StmtKind::Continue | StmtKind::Error => {}
            // Field declarations are scalar metadata in a side
            // arena, not node children — nothing reachable to follow.
            StmtKind::StructDef(_) | StmtKind::EnumDef(_) => {}
        }
        while let Some(expr) = expr_work.pop() {
            if expr_seen[expr.index()] {
                continue;
            }
            expr_seen[expr.index()] = true;
            match ast.expr(expr).kind {
                ExprKind::Literal(_) | ExprKind::Ident(_) => {}
                ExprKind::BinaryOp(lhs, _, rhs) => {
                    expr_work.push(lhs);
                    expr_work.push(rhs);
                }
                ExprKind::UnaryOp(_, operand) | ExprKind::Borrow(operand) => {
                    expr_work.push(operand);
                }
                ExprKind::Call(_, args) => {
                    expr_work.extend_from_slice(ast.expr_list(args));
                }
                ExprKind::MethodCall { receiver, args, .. } => {
                    expr_work.push(receiver);
                    expr_work.extend_from_slice(ast.expr_list(args));
                }
                ExprKind::Slice { base, start, end } => {
                    expr_work.push(base);
                    if let Some(start) = start {
                        expr_work.push(start);
                    }
                    if let Some(end) = end {
                        expr_work.push(end);
                    }
                }
                ExprKind::Index { base, index } => {
                    expr_work.push(base);
                    expr_work.push(index);
                }
                ExprKind::StructLiteral(lit) => {
                    for &(_, value) in ast.struct_field_inits(lit.fields) {
                        expr_work.push(value);
                    }
                }
                ExprKind::FieldAccess { object, .. } => {
                    expr_work.push(object);
                }
                ExprKind::VariantConstruct(c) => match &c.args {
                    Some(VariantArgs {
                        positional: Some(list),
                        ..
                    }) => expr_work.extend_from_slice(ast.expr_list(*list)),
                    Some(VariantArgs {
                        named: Some(inits), ..
                    }) => {
                        for &(_, value) in ast.struct_field_inits(*inits) {
                            expr_work.push(value);
                        }
                    }
                    _ => {}
                },
                // Match arms hold expressions (scrutinee, Expr bodies)
                // and statement lists; patterns hold no expressions.
                ExprKind::Match { scrutinee, arms } => {
                    expr_work.push(scrutinee);
                    for arm in ast.match_arms(arms) {
                        match arm.body {
                            MatchArmBody::Expr(body) => expr_work.push(body),
                            MatchArmBody::Stmts(list) => {
                                stmt_work.extend_from_slice(ast.stmt_list(list));
                            }
                        }
                    }
                }
            }
        }
    }
    (
        expr_seen.iter().filter(|&&seen| seen).count(),
        stmt_seen.iter().filter(|&&seen| seen).count(),
    )
}

#[test]
fn successful_parses_leave_no_orphan_nodes() {
    // The `Inspector` hooks on `ParseState` are deliberate no-ops for
    // performance (a truncating checkpoint cost ~20% of parse
    // time on `parse_large`). This guards the invariant that
    // makes that safe in practice: on valid input, speculative
    // alternatives fail on their first token without pushing, so
    // every arena slot except the slot-0 sentinel is reachable
    // from `top_level`.
    let examples = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../examples");
    let mut checked = 0;
    for entry in std::fs::read_dir(&examples).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("ryo") {
            continue;
        }
        let src = std::fs::read_to_string(&path).unwrap();
        let Ok((ast, _)) = lex_and_parse(&src) else {
            continue;
        };
        let (exprs, stmts) = reachable_node_counts(&ast);
        assert_eq!(
            exprs + 1,
            ast.expr_count(),
            "orphan expressions in {}",
            path.display()
        );
        assert_eq!(
            stmts + 1,
            ast.stmt_count(),
            "orphan statements in {}",
            path.display()
        );
        checked += 1;
    }
    assert!(checked > 5, "expected to check several example files");
}

#[test]
fn scalar_indexing_parse_leaves_no_orphan_nodes() {
    // Direct pin for the `b[i]` speculative-parse orphan leak (fixed in
    // e088c2e by parsing brackets in one pass) — previously the only
    // coverage was the examples sweep above happening to parse
    // `examples/bytes.ryo`.
    let (ast, _) = lex_and_parse("b = b\"\\x01\"\nx = b[0]\n").expect("parse ok");
    let (exprs, stmts) = reachable_node_counts(&ast);
    assert_eq!(exprs + 1, ast.expr_count(), "orphan expressions");
    assert_eq!(stmts + 1, ast.stmt_count(), "orphan statements");
}
