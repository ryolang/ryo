use super::*;
use crate::lexer::lex;
use chumsky::Parser;
use chumsky::error::RichReason;
use chumsky::input::Input;
use ryo_core::types::InternPool;

/// Owned parser error as threaded through the test helpers.
type TestErr = Rich<'static, Token, SimpleSpan, ParseDiag>;

fn lex_and_parse(input: &str) -> Result<(Ast, InternPool), Vec<TestErr>> {
    let mut pool = InternPool::new();
    let mut sink = ryo_core::diag::DiagSink::new();
    let tokens = lex(input, &mut pool, &mut sink);
    if sink.has_errors() {
        return Err(sink
            .into_diags()
            .into_iter()
            .map(|d| Rich::custom(d.span, ParseDiag::Message(d.message)))
            .collect());
    }
    let token_stream = tokens[..].split_token_span((0..input.len()).into());

    let mut ast = Ast::new();
    program_parser()
        .parse_with_state(token_stream, &mut ast)
        .into_result()
        .map_err(|e| {
            e.into_iter()
                .map(|rich| rich.into_owned())
                .collect::<Vec<_>>()
        })?;
    Ok((ast, pool))
}

/// The single top-level statement of a parsed snippet.
fn only_stmt(ast: &Ast) -> StmtId {
    let stmts = ast.top_level_stmts();
    assert_eq!(stmts.len(), 1);
    stmts[0]
}

fn var_decl(ast: &Ast, stmt: StmtId) -> &VarDecl {
    match &ast.stmt(stmt).kind {
        StmtKind::VarDecl(decl) => decl,
        other => panic!("expected VarDecl, got {other:?}"),
    }
}

fn fn_def(ast: &Ast, stmt: StmtId) -> &FunctionDef {
    match &ast.stmt(stmt).kind {
        StmtKind::FunctionDef(def) => def,
        other => panic!("expected FunctionDef, got {other:?}"),
    }
}

/// Body statements of a parsed function definition.
fn fn_body<'a>(ast: &'a Ast, def: &FunctionDef) -> &'a [StmtId] {
    ast.stmt_list(def.body)
}

fn if_stmt(ast: &Ast, stmt: StmtId) -> &IfStmt {
    match &ast.stmt(stmt).kind {
        StmtKind::IfStmt(if_stmt) => if_stmt,
        other => panic!("expected IfStmt, got {other:?}"),
    }
}

/// Body statements of a `while` statement.
fn while_body(ast: &Ast, stmt: StmtId) -> &[StmtId] {
    match &ast.stmt(stmt).kind {
        StmtKind::WhileLoop { body, .. } => ast.stmt_list(*body),
        other => panic!("expected WhileLoop, got {other:?}"),
    }
}

/// `(var, iterator, body)` of a `for` statement.
fn for_range(ast: &Ast, stmt: StmtId) -> (Ident, Ident, &[StmtId]) {
    match &ast.stmt(stmt).kind {
        StmtKind::ForRange {
            var,
            iterator,
            body,
            ..
        } => (*var, *iterator, ast.stmt_list(*body)),
        other => panic!("expected ForRange, got {other:?}"),
    }
}

/// The value of an `x = <value>` AssignOrDecl statement.
fn assign_value(ast: &Ast, stmt: StmtId) -> ExprId {
    match &ast.stmt(stmt).kind {
        StmtKind::AssignOrDecl { value, .. } => *value,
        other => panic!("expected AssignOrDecl, got {other:?}"),
    }
}

/// Initializer of a top-level `x = <init>` declaration.
fn decl_init(ast: &Ast) -> ExprId {
    var_decl(ast, only_stmt(ast)).initializer
}

fn bin_op(ast: &Ast, id: ExprId) -> (ExprId, BinaryOperator, ExprId) {
    match ast.expr(id).kind {
        ExprKind::BinaryOp(lhs, op, rhs) => (lhs, op, rhs),
        other => panic!("expected BinaryOp, got {other:?}"),
    }
}

fn assert_int_lit(ast: &Ast, id: ExprId, expected: i64) {
    assert!(
        matches!(ast.expr(id).kind, ExprKind::Literal(Literal::Int(v)) if v == expected),
        "expected Int({expected}), got {:?}",
        ast.expr(id).kind
    );
}

#[test]
fn parse_simple_variable_declaration() {
    let (ast, pool) = lex_and_parse("x = 42").unwrap();
    assert_eq!(ast.top_level_stmts().len(), 1);
    let decl = var_decl(&ast, only_stmt(&ast));
    assert!(!decl.mutable);
    assert_eq!(pool.str(decl.name.name), "x");
    assert!(decl.type_annotation.is_none());
    assert_int_lit(&ast, decl.initializer, 42);
}

#[test]
fn parse_variable_with_type_annotation() {
    let (ast, pool) = lex_and_parse("x: int = 42").unwrap();
    let decl = var_decl(&ast, only_stmt(&ast));
    assert_eq!(pool.str(decl.name.name), "x");
    assert_eq!(pool.str(decl.type_annotation.unwrap().name), "int");
}

#[test]
fn parse_mutable_variable() {
    let (ast, pool) = lex_and_parse("mut x = 42").unwrap();
    let decl = var_decl(&ast, only_stmt(&ast));
    assert!(decl.mutable);
    assert_eq!(pool.str(decl.name.name), "x");
}

#[test]
fn parse_mutable_with_type() {
    let (ast, pool) = lex_and_parse("mut counter: int = 0").unwrap();
    let decl = var_decl(&ast, only_stmt(&ast));
    assert!(decl.mutable);
    assert_eq!(pool.str(decl.name.name), "counter");
    assert_eq!(pool.str(decl.type_annotation.unwrap().name), "int");
    assert_int_lit(&ast, decl.initializer, 0);
}

#[test]
fn parse_expression_addition() {
    let (ast, _) = lex_and_parse("x = 1 + 2").unwrap();
    let (lhs, op, rhs) = bin_op(&ast, decl_init(&ast));
    assert_eq!(op, BinaryOperator::Add);
    assert_int_lit(&ast, lhs, 1);
    assert_int_lit(&ast, rhs, 2);
}

#[test]
fn parse_expression_precedence() {
    let (ast, _) = lex_and_parse("x = 2 + 3 * 4").unwrap();
    let (lhs, op, rhs) = bin_op(&ast, decl_init(&ast));
    assert_eq!(op, BinaryOperator::Add);
    assert_int_lit(&ast, lhs, 2);
    assert_eq!(bin_op(&ast, rhs).1, BinaryOperator::Mul);
}

#[test]
fn parse_expression_negation() {
    let (ast, _) = lex_and_parse("x = -42").unwrap();
    match ast.expr(decl_init(&ast)).kind {
        ExprKind::UnaryOp(op, operand) => {
            assert_eq!(op, UnaryOperator::Neg);
            assert_int_lit(&ast, operand, 42);
        }
        other => panic!("expected UnaryOp, got {other:?}"),
    }
}

#[test]
fn parse_i64_min_literal() {
    // `-9223372036854775808` (i64::MIN): the lexer emits IntLitMin
    // for the overflowing positive form and the parser folds
    // `- IntLitMin` directly to the literal — no UnaryOp.
    let (ast, _) = lex_and_parse("x = -9223372036854775808").unwrap();
    let init = decl_init(&ast);
    assert!(
        matches!(
            ast.expr(init).kind,
            ExprKind::Literal(Literal::Int(i64::MIN))
        ),
        "expected folded i64::MIN literal, got {:?}",
        ast.expr(init).kind
    );
}

#[test]
fn bare_i64_min_magnitude_literal_rejected() {
    // Without the unary `-`, the positive form overflows `i64`
    // and must not parse as an expression.
    assert!(lex_and_parse("x = 9223372036854775808").is_err());
    // …including after a binary minus, where the literal is a
    // fresh operand, not the operand of a unary negation.
    assert!(lex_and_parse("y = 1\nx = y - 9223372036854775808").is_err());
}

#[test]
fn parse_expression_parenthesized() {
    let (ast, _) = lex_and_parse("x = (2 + 3) * 4").unwrap();
    assert_eq!(bin_op(&ast, decl_init(&ast)).1, BinaryOperator::Mul);
}

#[test]
fn parse_multiple_statements() {
    let (ast, _) = lex_and_parse("x = 42\ny = 10").unwrap();
    assert_eq!(ast.top_level_stmts().len(), 2);
}

#[test]
fn parse_multiple_with_types() {
    let (ast, _) = lex_and_parse("x: int = 42\nmut y: float = 3\nz = 1 + 2").unwrap();
    assert_eq!(ast.top_level_stmts().len(), 3);
}

#[test]
fn parse_empty_program() {
    let (ast, _) = lex_and_parse("").unwrap();
    assert_eq!(ast.top_level_stmts().len(), 0);
}

#[test]
fn reject_two_expressions_same_line() {
    assert!(lex_and_parse("x 42").is_err());
}

#[test]
fn reject_multiple_expressions_same_line() {
    assert!(lex_and_parse("x y 42").is_err());
}

#[test]
fn accept_statements_on_separate_lines() {
    let (ast, _) = lex_and_parse("x = 1\ny = 2").unwrap();
    assert_eq!(ast.top_level_stmts().len(), 2);
}

#[test]
fn accept_statement_with_no_trailing_newline() {
    assert!(lex_and_parse("x = 42").is_ok());
}

#[test]
fn accept_statement_with_trailing_newline() {
    assert!(lex_and_parse("x = 42\n").is_ok());
}

#[test]
fn accept_blank_lines_between_statements() {
    let (ast, _) = lex_and_parse("x = 1\n\ny = 2").unwrap();
    assert_eq!(ast.top_level_stmts().len(), 2);
}

#[test]
fn parse_true_false_literals() {
    let (ast, _) = lex_and_parse("x = true\ny = false").unwrap();
    let stmts = ast.top_level_stmts();
    let first = var_decl(&ast, stmts[0]);
    assert!(matches!(
        ast.expr(first.initializer).kind,
        ExprKind::Literal(Literal::Bool(true))
    ));
    let second = var_decl(&ast, stmts[1]);
    assert!(matches!(
        ast.expr(second.initializer).kind,
        ExprKind::Literal(Literal::Bool(false))
    ));
}

#[test]
fn parse_equality_expression() {
    let (ast, _) = lex_and_parse("x = 1 == 2").unwrap();
    assert_eq!(bin_op(&ast, decl_init(&ast)).1, BinaryOperator::Eq);
}

#[test]
fn parse_not_equal_expression() {
    let (ast, _) = lex_and_parse("x = 1 != 2").unwrap();
    assert_eq!(bin_op(&ast, decl_init(&ast)).1, BinaryOperator::NotEq);
}

#[test]
fn parse_equality_has_lower_precedence_than_addition() {
    let (ast, _) = lex_and_parse("x = a + b == c + d").unwrap();
    let (lhs, op, rhs) = bin_op(&ast, decl_init(&ast));
    assert_eq!(op, BinaryOperator::Eq);
    assert_eq!(bin_op(&ast, lhs).1, BinaryOperator::Add);
    assert_eq!(bin_op(&ast, rhs).1, BinaryOperator::Add);
}

#[test]
fn parse_float_literal() {
    let (ast, _) = lex_and_parse("x = 2.5").unwrap();
    match ast.expr(decl_init(&ast)).kind {
        ExprKind::Literal(Literal::Float(v)) => assert!((v - 2.5).abs() < 1e-12),
        other => panic!("expected Float literal, got {:?}", other),
    }
}

#[test]
fn parse_ordering_operators() {
    for (src, expected_op) in &[
        ("x = a < b", BinaryOperator::Lt),
        ("x = a > b", BinaryOperator::Gt),
        ("x = a <= b", BinaryOperator::LtEq),
        ("x = a >= b", BinaryOperator::GtEq),
    ] {
        let (ast, _) = lex_and_parse(src).unwrap();
        assert_eq!(bin_op(&ast, decl_init(&ast)).1, *expected_op);
    }
}

#[test]
fn parse_modulo_at_multiplicative_precedence() {
    let (ast, _) = lex_and_parse("x = a + b % c").unwrap();
    let (_, op, rhs) = bin_op(&ast, decl_init(&ast));
    assert_eq!(op, BinaryOperator::Add);
    assert_eq!(bin_op(&ast, rhs).1, BinaryOperator::Mod);
}

#[test]
fn parse_ordering_below_additive_precedence() {
    let (ast, _) = lex_and_parse("x = a + b < c + d").unwrap();
    let (lhs, op, rhs) = bin_op(&ast, decl_init(&ast));
    assert_eq!(op, BinaryOperator::Lt);
    assert_eq!(bin_op(&ast, lhs).1, BinaryOperator::Add);
    assert_eq!(bin_op(&ast, rhs).1, BinaryOperator::Add);
}

#[test]
fn parse_equality_below_ordering_precedence() {
    let (ast, _) = lex_and_parse("x = a < b == c < d").unwrap();
    let (lhs, op, rhs) = bin_op(&ast, decl_init(&ast));
    assert_eq!(op, BinaryOperator::Eq);
    assert_eq!(bin_op(&ast, lhs).1, BinaryOperator::Lt);
    assert_eq!(bin_op(&ast, rhs).1, BinaryOperator::Lt);
}

/// Assert a chained comparison soft-rejects: the parse recovers
/// (partial program produced), the secondary diagnostic is the
/// structured `ChainedComparison` payload pointing at the second
/// operator, and the AST keeps only the well-formed first comparison.
fn assert_chained_comparison_soft_rejected(
    src: &str,
    expected_op: BinaryOperator,
    expected_op_span: std::ops::Range<usize>,
) {
    let (ok, ast, errs, _pool) = lex_and_parse_recovering(src);
    assert!(ok, "chained comparison should recover to a partial program");
    assert_eq!(errs.len(), 1);
    assert_eq!(
        errs[0].reason(),
        &RichReason::Custom(ParseDiag::ChainedComparison)
    );
    assert_eq!(
        errs[0].span().into_range(),
        expected_op_span,
        "diagnostic must point at the second operator"
    );
    let (_, op, _) = bin_op(&ast, decl_init(&ast));
    assert_eq!(op, expected_op);
}

#[test]
fn parse_chained_ordering_is_soft_rejected() {
    assert_chained_comparison_soft_rejected("x = a < b < c", BinaryOperator::Lt, 10..11);
}

#[test]
fn parse_chained_equality_is_soft_rejected() {
    assert_chained_comparison_soft_rejected("x = a == b == c", BinaryOperator::Eq, 11..13);
}

/// Helper for the escape-table tests: parse a single
/// `x = "..."` declaration and return the interned bytes of
/// its string literal.
fn parse_str_literal(src: &str) -> String {
    let (ast, pool) = lex_and_parse(src).expect("parse ok");
    match ast.expr(decl_init(&ast)).kind {
        ExprKind::Literal(Literal::Str(id)) => pool.str(id).to_string(),
        other => panic!("expected Str literal, got {:?}", other),
    }
}

#[test]
fn string_literal_unescapes_at_lex_time() {
    // Sanity check on the historical case (newline) before
    // sweeping the rest of the escape table below.
    assert_eq!(parse_str_literal("x = \"hello\\n\""), "hello\n");
}

#[test]
fn string_literal_decodes_full_escape_table() {
    // Locks the escape semantics down at the lex layer so the
    // parser can stay a pure pass-through for `Literal::Str`.
    // If a new escape lands (or an existing one changes), it
    // surfaces here rather than at codegen time.
    assert_eq!(parse_str_literal(r#"x = "\n""#), "\n");
    assert_eq!(parse_str_literal(r#"x = "\t""#), "\t");
    assert_eq!(parse_str_literal(r#"x = "\r""#), "\r");
    assert_eq!(parse_str_literal(r#"x = "\\""#), "\\");
    assert_eq!(parse_str_literal(r#"x = "\"""#), "\"");
    assert_eq!(parse_str_literal("x = \"\\0\""), "\0");
}

#[test]
fn parse_and_operator() {
    let (ast, _) = lex_and_parse("x = true and false").unwrap();
    assert_eq!(bin_op(&ast, decl_init(&ast)).1, BinaryOperator::And);
}

#[test]
fn parse_or_operator() {
    let (ast, _) = lex_and_parse("x = true or false").unwrap();
    assert_eq!(bin_op(&ast, decl_init(&ast)).1, BinaryOperator::Or);
}

#[test]
fn parse_not_operator() {
    let (ast, _) = lex_and_parse("x = not true").unwrap();
    let init = decl_init(&ast);
    assert!(
        matches!(
            ast.expr(init).kind,
            ExprKind::UnaryOp(UnaryOperator::Not, _)
        ),
        "expected UnaryOp(Not), got {:?}",
        ast.expr(init).kind
    );
}

#[test]
fn parse_and_binds_tighter_than_or() {
    // a or b and c  =>  a or (b and c)
    let (ast, _) = lex_and_parse("x = true or false and true").unwrap();
    let (_, op, rhs) = bin_op(&ast, decl_init(&ast));
    assert_eq!(op, BinaryOperator::Or);
    assert_eq!(bin_op(&ast, rhs).1, BinaryOperator::And);
}

#[test]
fn parse_not_binds_tighter_than_and() {
    // not a and b  =>  (not a) and b
    let (ast, _) = lex_and_parse("x = not true and false").unwrap();
    let (lhs, op, _) = bin_op(&ast, decl_init(&ast));
    assert_eq!(op, BinaryOperator::And);
    assert!(matches!(
        ast.expr(lhs).kind,
        ExprKind::UnaryOp(UnaryOperator::Not, _)
    ));
}

#[test]
fn parse_not_not_chains() {
    let (ast, _) = lex_and_parse("x = not not true").unwrap();
    let inner = match ast.expr(decl_init(&ast)).kind {
        ExprKind::UnaryOp(op, operand) => {
            assert_eq!(op, UnaryOperator::Not);
            operand
        }
        other => panic!("expected UnaryOp, got {other:?}"),
    };
    assert!(matches!(
        ast.expr(inner).kind,
        ExprKind::UnaryOp(UnaryOperator::Not, _)
    ));
}

#[test]
fn parse_simple_if() {
    let input = "fn main():\n\tif true:\n\t\tx = 1\n";
    let (ast, _) = lex_and_parse(input).unwrap();
    let f = fn_def(&ast, only_stmt(&ast));
    let body = fn_body(&ast, f);
    assert_eq!(body.len(), 1);
    assert!(matches!(ast.stmt(body[0]).kind, StmtKind::IfStmt(_)));
}

#[test]
fn parse_if_else() {
    let input = "fn main():\n\tif true:\n\t\tx = 1\n\telse:\n\t\tx = 2\n";
    let (ast, _) = lex_and_parse(input).unwrap();
    let f = fn_def(&ast, only_stmt(&ast));
    let view = if_stmt(&ast, fn_body(&ast, f)[0]);
    assert!(view.else_block.is_some());
    assert!(ast.elif_list(view.elif_branches).is_empty());
}

#[test]
fn parse_if_elif_else() {
    let input = "fn main():\n\tif true:\n\t\tx = 1\n\telif false:\n\t\tx = 2\n\telse:\n\t\tx = 3\n";
    let (ast, _) = lex_and_parse(input).unwrap();
    let f = fn_def(&ast, only_stmt(&ast));
    let view = if_stmt(&ast, fn_body(&ast, f)[0]);
    assert_eq!(ast.elif_list(view.elif_branches).len(), 1);
    assert!(view.else_block.is_some());
}

#[test]
fn parse_multiple_elif() {
    let input = "fn main():\n\tif true:\n\t\tx = 1\n\telif false:\n\t\tx = 2\n\telif true:\n\t\tx = 3\n\telse:\n\t\tx = 4\n";
    let (ast, _) = lex_and_parse(input).unwrap();
    let f = fn_def(&ast, only_stmt(&ast));
    let view = if_stmt(&ast, fn_body(&ast, f)[0]);
    assert_eq!(ast.elif_list(view.elif_branches).len(), 2);
    assert!(view.else_block.is_some());
}

#[test]
fn parse_if_without_else() {
    let input = "fn main():\n\tif true:\n\t\tx = 1\n\tprint(\"done\")\n";
    let (ast, _) = lex_and_parse(input).unwrap();
    let f = fn_def(&ast, only_stmt(&ast));
    assert_eq!(fn_body(&ast, f).len(), 2);
    let view = if_stmt(&ast, fn_body(&ast, f)[0]);
    assert!(view.else_block.is_none());
    assert!(ast.elif_list(view.elif_branches).is_empty());
}

#[test]
fn parse_assign_or_decl() {
    let (ast, pool) = lex_and_parse("fn main():\n\tx = 42\n").unwrap();
    let f = fn_def(&ast, only_stmt(&ast));
    let stmt = fn_body(&ast, f)[0];
    let value = assign_value(&ast, stmt);
    match &ast.stmt(stmt).kind {
        StmtKind::AssignOrDecl { target, .. } => {
            assert_eq!(pool.str(target.name), "x");
        }
        other => panic!("expected AssignOrDecl, got {other:?}"),
    }
    assert_int_lit(&ast, value, 42);
}

#[test]
fn parse_compound_assign_plus() {
    let (ast, pool) = lex_and_parse("fn main():\n\tx += 1\n").unwrap();
    let f = fn_def(&ast, only_stmt(&ast));
    match &ast.stmt(fn_body(&ast, f)[0]).kind {
        StmtKind::CompoundAssign { target, op, .. } => {
            assert_eq!(pool.str(target.name), "x");
            assert_eq!(*op, CompoundOp::Add);
        }
        other => panic!("expected CompoundAssign, got {other:?}"),
    }
}

#[test]
fn parse_all_compound_ops() {
    for (src, expected_op) in [
        ("x += 1", CompoundOp::Add),
        ("x -= 1", CompoundOp::Sub),
        ("x *= 1", CompoundOp::Mul),
        ("x /= 1", CompoundOp::Div),
        ("x %= 1", CompoundOp::Mod),
    ] {
        let code = format!("fn main():\n\t{}\n", src);
        let (ast, _pool) = lex_and_parse(&code).unwrap();
        let f = fn_def(&ast, only_stmt(&ast));
        match &ast.stmt(fn_body(&ast, f)[0]).kind {
            StmtKind::CompoundAssign { op, .. } => {
                assert_eq!(*op, expected_op, "failed for: {}", src);
            }
            other => panic!("expected CompoundAssign, got {other:?}"),
        }
    }
}

#[test]
fn vardecl_still_works_with_mut() {
    let (ast, pool) = lex_and_parse("fn main():\n\tmut x = 10\n").unwrap();
    let f = fn_def(&ast, only_stmt(&ast));
    let decl = var_decl(&ast, fn_body(&ast, f)[0]);
    assert!(decl.mutable);
    assert_eq!(pool.str(decl.name.name), "x");
}

#[test]
fn vardecl_with_type_annotation_still_works() {
    let (ast, _pool) = lex_and_parse("fn main():\n\tx: int = 10\n").unwrap();
    let f = fn_def(&ast, only_stmt(&ast));
    let decl = var_decl(&ast, fn_body(&ast, f)[0]);
    assert!(!decl.mutable);
    assert!(decl.type_annotation.is_some());
}

#[test]
fn parse_while_loop() {
    let code = "fn main():\n\twhile true:\n\t\tbreak\n";
    let (ast, _pool) = lex_and_parse(code).unwrap();
    let f = fn_def(&ast, only_stmt(&ast));
    let body = fn_body(&ast, f);
    assert_eq!(body.len(), 1);
    assert_eq!(while_body(&ast, body[0]).len(), 1);
}

#[test]
fn parse_break_statement() {
    let code = "fn main():\n\twhile true:\n\t\tbreak\n";
    let (ast, _pool) = lex_and_parse(code).unwrap();
    let f = fn_def(&ast, only_stmt(&ast));
    let body = while_body(&ast, fn_body(&ast, f)[0]);
    assert!(matches!(ast.stmt(body[0]).kind, StmtKind::Break));
}

#[test]
fn parse_continue_statement() {
    let code = "fn main():\n\twhile true:\n\t\tcontinue\n";
    let (ast, _pool) = lex_and_parse(code).unwrap();
    let f = fn_def(&ast, only_stmt(&ast));
    let body = while_body(&ast, fn_body(&ast, f)[0]);
    assert!(matches!(ast.stmt(body[0]).kind, StmtKind::Continue));
}

#[test]
fn parse_nested_while() {
    let code = "fn main():\n\twhile true:\n\t\twhile false:\n\t\t\tbreak\n";
    let (ast, _pool) = lex_and_parse(code).unwrap();
    let f = fn_def(&ast, only_stmt(&ast));
    let body = while_body(&ast, fn_body(&ast, f)[0]);
    assert_eq!(body.len(), 1);
    assert!(matches!(ast.stmt(body[0]).kind, StmtKind::WhileLoop { .. }));
}

#[test]
fn parse_logical_below_equality() {
    // a == b and c == d  =>  (a == b) and (c == d)
    let (ast, _) = lex_and_parse("x = 1 == 2 and 3 == 4").unwrap();
    let (lhs, op, rhs) = bin_op(&ast, decl_init(&ast));
    assert_eq!(op, BinaryOperator::And);
    assert_eq!(bin_op(&ast, lhs).1, BinaryOperator::Eq);
    assert_eq!(bin_op(&ast, rhs).1, BinaryOperator::Eq);
}

#[test]
fn parse_for_range() {
    let code = "fn main():\n\tfor i in range(0, 10):\n\t\tprint(i)\n";
    let (ast, pool) = lex_and_parse(code).unwrap();
    let f = fn_def(&ast, only_stmt(&ast));
    let body = fn_body(&ast, f);
    assert_eq!(body.len(), 1);
    let (var, iterator, loop_body) = for_range(&ast, body[0]);
    assert_eq!(pool.str(var.name), "i");
    assert_eq!(pool.str(iterator.name), "range");
    assert_eq!(loop_body.len(), 1);
}

#[test]
fn parse_for_range_with_expressions() {
    let code = "fn main():\n\tfor x in range(1 + 2, 10 - 3):\n\t\tprint(x)\n";
    let (ast, pool) = lex_and_parse(code).unwrap();
    let f = fn_def(&ast, only_stmt(&ast));
    let (var, iterator, _) = for_range(&ast, fn_body(&ast, f)[0]);
    assert_eq!(pool.str(var.name), "x");
    assert_eq!(pool.str(iterator.name), "range");
}

#[test]
fn parse_for_range_nested() {
    let code = "fn main():\n\tfor i in range(0, 5):\n\t\tfor j in range(0, 3):\n\t\t\tprint(i)\n";
    let (ast, _pool) = lex_and_parse(code).unwrap();
    let f = fn_def(&ast, only_stmt(&ast));
    let (_, _, body) = for_range(&ast, fn_body(&ast, f)[0]);
    assert_eq!(body.len(), 1);
    assert!(matches!(ast.stmt(body[0]).kind, StmtKind::ForRange { .. }));
}

#[test]
fn parse_for_break_continue() {
    let code = "fn main():\n\tfor i in range(0, 10):\n\t\tif i == 5:\n\t\t\tbreak\n\t\tif i == 3:\n\t\t\tcontinue\n";
    let (ast, _pool) = lex_and_parse(code).unwrap();
    let f = fn_def(&ast, only_stmt(&ast));
    assert!(matches!(
        ast.stmt(fn_body(&ast, f)[0]).kind,
        StmtKind::ForRange { .. }
    ));
}

#[test]
fn parse_for_range_wrong_arity_one_arg() {
    let code = "fn main():\n\tfor i in range(5):\n\t\tprint(i)\n";
    let result = lex_and_parse(code);
    assert!(result.is_err(), "range(5) should fail with arity error");
}

#[test]
fn parse_for_range_wrong_arity_three_args() {
    let code = "fn main():\n\tfor i in range(0, 10, 2):\n\t\tprint(i)\n";
    let result = lex_and_parse(code);
    assert!(
        result.is_err(),
        "range(0, 10, 2) should fail with arity error"
    );
}

#[test]
fn parse_for_range_wrong_arity_zero_args() {
    let code = "fn main():\n\tfor i in range():\n\t\tprint(i)\n";
    let result = lex_and_parse(code);
    assert!(result.is_err(), "range() should fail with arity error");
}

#[test]
fn parse_move_parameter() {
    let (ast, pool) = lex_and_parse("fn consume(move s: str):\n\tprint(s)\n").unwrap();
    let f = fn_def(&ast, only_stmt(&ast));
    assert_eq!(f.params.len(), 1);
    assert!(
        f.params[0].mode == ParamMode::Move,
        "param `s` should be marked move"
    );
    assert_eq!(pool.str(f.params[0].name.name), "s");
    assert_eq!(pool.str(f.params[0].type_annotation.name), "str");
}

#[test]
fn parse_default_parameter_is_not_move() {
    let (ast, pool) = lex_and_parse("fn read(s: str):\n\tprint(s)\n").unwrap();
    let f = fn_def(&ast, only_stmt(&ast));
    assert_eq!(f.params.len(), 1);
    assert!(
        f.params[0].mode == ParamMode::Borrow,
        "bare param `s` should default to Borrow mode"
    );
    assert_eq!(pool.str(f.params[0].name.name), "s");
    assert_eq!(pool.str(f.params[0].type_annotation.name), "str");
}

#[test]
fn parse_inout_param() {
    let (ast, _pool) = lex_and_parse("fn f(inout x: int):\n\tx += 1\n").unwrap();
    let f = fn_def(&ast, only_stmt(&ast));
    assert_eq!(f.params.len(), 1);
    assert_eq!(
        f.params[0].mode,
        ParamMode::Inout,
        "param `x` should be marked inout"
    );
}

#[test]
fn parse_borrow_arg() {
    let (ast, _pool) = lex_and_parse("fn main():\n\tmut c = 0\n\tf(&c)\n").unwrap();
    let f = fn_def(&ast, only_stmt(&ast));
    // body[1] is the `f(&c)` expression statement.
    let call = match &ast.stmt(fn_body(&ast, f)[1]).kind {
        StmtKind::ExprStmt(value) => *value,
        other => panic!("expected ExprStmt, got {other:?}"),
    };
    match ast.expr(call).kind {
        ExprKind::Call(_, args) => {
            let args = ast.expr_list(args);
            assert_eq!(args.len(), 1);
            assert!(
                matches!(ast.expr(args[0]).kind, ExprKind::Borrow(_)),
                "call argument should be a Borrow expression"
            );
        }
        other => panic!("expected Call, got {other:?}"),
    }
}

#[test]
fn parse_slice_full() {
    let (ast, _pool) = lex_and_parse("fn main():\n\tx = s[1:2]\n").unwrap();
    // `fn main():` wraps the body: the slice sits in the value of
    // the body's AssignOrDecl (see `parse_assign_or_decl`).
    let f = fn_def(&ast, only_stmt(&ast));
    let value = assign_value(&ast, fn_body(&ast, f)[0]);
    match ast.expr(value).kind {
        ExprKind::Slice { base, start, end } => {
            assert!(matches!(ast.expr(base).kind, ExprKind::Ident(_)));
            assert!(start.is_some() && end.is_some());
        }
        other => panic!("expected Slice, got {other:?}"),
    }
}

#[test]
fn parse_slice_shorthands() {
    for (src, want_start, want_end) in [
        ("x = s[1:]", true, false),
        ("x = s[:2]", false, true),
        ("x = s[:]", false, false),
    ] {
        let snippet = format!("fn main():\n\t{}\n", src);
        let (ast, _pool) = lex_and_parse(&snippet).unwrap();
        let f = fn_def(&ast, only_stmt(&ast));
        let value = assign_value(&ast, fn_body(&ast, f)[0]);
        match ast.expr(value).kind {
            ExprKind::Slice { start, end, .. } => {
                assert_eq!(start.is_some(), want_start, "{}: start", src);
                assert_eq!(end.is_some(), want_end, "{}: end", src);
            }
            other => panic!("expected Slice, got {other:?}"),
        }
    }
}

#[test]
fn parse_slice_after_method_call() {
    let (ast, _pool) = lex_and_parse("fn main():\n\tx = s.len()[0:1]\n").unwrap();
    let f = fn_def(&ast, only_stmt(&ast));
    let value = assign_value(&ast, fn_body(&ast, f)[0]);
    match ast.expr(value).kind {
        ExprKind::Slice { base, .. } => {
            assert!(matches!(ast.expr(base).kind, ExprKind::MethodCall { .. }));
        }
        other => panic!("expected Slice, got {other:?}"),
    }
}

#[test]
fn parse_slice_empty_rejected() {
    assert!(lex_and_parse("fn main():\n\tx = s[]\n").is_err());
}

#[test]
fn parse_slice_three_part_rejected() {
    assert!(lex_and_parse("fn main():\n\tx = s[1:2:3]\n").is_err());
}

#[test]
fn parse_view_param_annotation() {
    let (ast, pool) = lex_and_parse("fn first(text: &str):\n\tprint(text)\n").unwrap();
    let f = fn_def(&ast, only_stmt(&ast));
    assert!(f.params[0].type_annotation.is_view);
    assert_eq!(pool.str(f.params[0].type_annotation.name), "str");
}

/// Recovery-aware variant of `lex_and_parse`: returns whether a
/// (possibly partial) program could be produced, the arena it was
/// built into, every parse error, and the pool.
fn lex_and_parse_recovering(input: &str) -> (bool, Ast, Vec<TestErr>, InternPool) {
    let mut pool = InternPool::new();
    let mut sink = ryo_core::diag::DiagSink::new();
    let tokens = lex(input, &mut pool, &mut sink);
    assert!(!sink.has_errors(), "test input must lex cleanly");
    let token_stream = tokens[..].split_token_span((0..input.len()).into());
    let mut ast = Ast::new();
    let (out, errs) = program_parser()
        .parse_with_state(token_stream, &mut ast)
        .into_output_errors();
    (
        out.is_some(),
        ast,
        errs.into_iter().map(|rich| rich.into_owned()).collect(),
        pool,
    )
}

/// The statement is a parser-recovery placeholder.
fn is_error_stmt(ast: &Ast, stmt: StmtId) -> bool {
    matches!(ast.stmt(stmt).kind, StmtKind::Error)
}

/// The statement is a variable declaration of either form.
fn is_decl_stmt(ast: &Ast, stmt: StmtId) -> bool {
    matches!(
        ast.stmt(stmt).kind,
        StmtKind::VarDecl(_) | StmtKind::AssignOrDecl { .. }
    )
}

#[test]
fn recovers_from_bad_statement_between_good_ones() {
    let (ok, ast, errs, _pool) = lex_and_parse_recovering("x = 1\ny = = 2\nz = 3\n");
    assert_eq!(errs.len(), 1, "expected one parse error: {errs:?}");
    assert!(ok, "recovery must produce a partial program");
    let stmts = ast.top_level_stmts();
    assert_eq!(stmts.len(), 3);
    assert!(is_decl_stmt(&ast, stmts[0]));
    assert!(is_error_stmt(&ast, stmts[1]));
    assert!(is_decl_stmt(&ast, stmts[2]));
}

#[test]
fn recovers_inside_function_body() {
    let (ok, ast, errs, _pool) =
        lex_and_parse_recovering("fn main():\n\tx = 1\n\ty = = 2\n\tz = 3\n");
    assert_eq!(errs.len(), 1, "expected one parse error: {errs:?}");
    assert!(ok, "recovery must produce a partial program");
    let func = fn_def(&ast, only_stmt(&ast));
    let body = fn_body(&ast, func);
    assert_eq!(body.len(), 3);
    assert!(is_error_stmt(&ast, body[1]));
    assert!(matches!(
        ast.stmt(body[2]).kind,
        StmtKind::AssignOrDecl { .. }
    ));
}

#[test]
fn reports_multiple_parse_errors_in_one_file() {
    let (ok, ast, errs, _pool) = lex_and_parse_recovering("x = = 1\ny = 2\nz = = 3\nw = 4\n");
    assert_eq!(errs.len(), 2, "expected two parse errors: {errs:?}");
    assert!(ok, "recovery must produce a partial program");
    let stmts = ast.top_level_stmts();
    assert_eq!(stmts.len(), 4);
    assert!(is_error_stmt(&ast, stmts[0]));
    assert!(is_decl_stmt(&ast, stmts[1]));
    assert!(is_error_stmt(&ast, stmts[2]));
    assert!(is_decl_stmt(&ast, stmts[3]));
}

#[test]
fn recovers_from_trailing_garbage_without_newline_at_eof() {
    // File does not end with a newline: the last line is broken.
    let (ok, ast, errs, _pool) = lex_and_parse_recovering("x = 1\ny = = 2");
    assert_eq!(errs.len(), 1, "expected one parse error: {errs:?}");
    assert!(ok, "recovery must produce a partial program");
    let stmts = ast.top_level_stmts();
    assert_eq!(stmts.len(), 2);
    assert!(is_decl_stmt(&ast, stmts[0]));
    assert!(is_error_stmt(&ast, stmts[1]));
}

#[test]
fn recovers_from_broken_block_final_statement_before_dedent() {
    // The last body line is broken and the following top-level
    // statement triggers the block's `Dedent`.
    let (ok, ast, errs, _pool) =
        lex_and_parse_recovering("fn main():\n\tx = 1\n\ty = = 2\nz = 3\n");
    assert_eq!(errs.len(), 1, "expected one parse error: {errs:?}");
    assert!(ok, "recovery must produce a partial program");
    let stmts = ast.top_level_stmts();
    let func = fn_def(&ast, stmts[0]);
    let body = fn_body(&ast, func);
    assert_eq!(body.len(), 2);
    assert!(is_error_stmt(&ast, body[1]));
    assert_eq!(stmts.len(), 2);
}

#[test]
fn recovers_from_broken_block_final_statement_at_eof() {
    // The broken final body line sits directly against the
    // zero-width end-of-input `Dedent` (no terminating newline of
    // its own) — the `peek_terminator(Dedent)` terminator path.
    let (ok, ast, errs, _pool) = lex_and_parse_recovering("fn main():\n\tx = 1\n\ty = = 2");
    assert_eq!(errs.len(), 1, "expected one parse error: {errs:?}");
    assert!(ok, "recovery must produce a partial program");
    let func = fn_def(&ast, only_stmt(&ast));
    let body = fn_body(&ast, func);
    assert_eq!(body.len(), 2);
    assert!(is_error_stmt(&ast, body[1]));
    assert_eq!(ast.top_level_stmts().len(), 1);
}

#[test]
fn parses_unterminated_final_statement_in_block() {
    // The block-final statement has no terminating newline: it
    // ends directly at the list terminator (the end-of-input
    // `Dedent`). Valid input, so no error node and no diagnostic.
    let (ok, ast, errs, _pool) = lex_and_parse_recovering("fn main():\n\tx = 1");
    assert!(errs.is_empty(), "expected a clean parse: {errs:?}");
    assert!(ok, "expected a program");
    let func = fn_def(&ast, only_stmt(&ast));
    let body = fn_body(&ast, func);
    assert_eq!(body.len(), 1);
    assert!(is_decl_stmt(&ast, body[0]));
}

#[test]
fn two_statements_on_one_line_still_error_with_recovery() {
    // The no-two-statements-per-line rule must survive recovery:
    // this is one parse error, not two silent statements.
    let (ok, ast, errs, _pool) = lex_and_parse_recovering("x = 1 y = 2\n");
    assert_eq!(errs.len(), 1, "expected one parse error: {errs:?}");
    assert!(ok, "recovery must produce a partial program");
    let stmts = ast.top_level_stmts();
    assert_eq!(stmts.len(), 1);
    assert!(is_error_stmt(&ast, stmts[0]));
}

#[test]
fn broken_block_header_swallows_body_at_top_level() {
    // The `fn` header is unparseable; its indented body must be
    // swallowed as one region — not parse silently as top-level
    // statements, and not leave a dangling `Dedent` behind.
    let (ok, ast, errs, _pool) = lex_and_parse_recovering("fn main(:\n\tx = 1\n\ty = 2\nz = 3\n");
    assert_eq!(errs.len(), 1, "expected one parse error: {errs:?}");
    assert!(ok, "recovery must produce a partial program");
    let stmts = ast.top_level_stmts();
    assert_eq!(stmts.len(), 2);
    assert!(is_error_stmt(&ast, stmts[0]));
    assert!(is_decl_stmt(&ast, stmts[1]));
}

#[test]
fn broken_block_header_inside_block_swallows_body() {
    // Same mis-nesting one level down: the broken `if` header must
    // swallow its body inside the enclosing `fn` block, and the
    // following body line must still parse in that block.
    let (ok, ast, errs, _pool) =
        lex_and_parse_recovering("fn main():\n\tif x(:\n\t\ty = 1\n\tz = 2\n");
    assert_eq!(errs.len(), 1, "expected one parse error: {errs:?}");
    assert!(ok, "recovery must produce a partial program");
    let func = fn_def(&ast, only_stmt(&ast));
    let body = fn_body(&ast, func);
    assert_eq!(body.len(), 2);
    assert!(is_error_stmt(&ast, body[0]));
    assert!(matches!(
        ast.stmt(body[1]).kind,
        StmtKind::AssignOrDecl { .. }
    ));
    assert_eq!(ast.top_level_stmts().len(), 1);
}

#[test]
fn broken_block_header_swallows_nested_blocks() {
    // The swallowed region must be balanced: the inner block's
    // `Dedent` does not end the swallow early.
    let (ok, ast, errs, _pool) =
        lex_and_parse_recovering("while x(:\n\tif y:\n\t\tz = 1\n\tw = 2\nv = 3\n");
    assert_eq!(errs.len(), 1, "expected one parse error: {errs:?}");
    assert!(ok, "recovery must produce a partial program");
    let stmts = ast.top_level_stmts();
    assert_eq!(stmts.len(), 2);
    assert!(is_error_stmt(&ast, stmts[0]));
    assert!(is_decl_stmt(&ast, stmts[1]));
}

#[test]
fn broken_block_header_with_body_at_eof() {
    // No trailing statement: the swallowed body runs into the
    // end-of-input `Dedent`, and that is still a single error.
    let (ok, ast, errs, _pool) = lex_and_parse_recovering("fn main(:\n\tx = 1");
    assert_eq!(errs.len(), 1, "expected one parse error: {errs:?}");
    assert!(ok, "recovery must produce a partial program");
    assert_eq!(ast.top_level_stmts().len(), 1);
    assert!(is_error_stmt(&ast, ast.top_level_stmts()[0]));
}

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
    // The `Inspector` hooks on `Ast` are deliberate no-ops for
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
