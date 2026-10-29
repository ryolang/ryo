//! Error declaration, error-union, constructor, try, and catch grammar tests.

use super::tests::*;
use super::*;
use chumsky::error::RichReason;
use ryo_core::types::VariantKind;

fn error_def(ast: &Ast, stmt: StmtId) -> ErrorDef {
    match &ast.stmt(stmt).kind {
        StmtKind::ErrorDef(def) => *def,
        other => panic!("expected ErrorDef, got {other:?}"),
    }
}

fn expr_stmt(ast: &Ast, stmt: StmtId) -> ExprId {
    match &ast.stmt(stmt).kind {
        StmtKind::ExprStmt(expr) => *expr,
        other => panic!("expected ExprStmt, got {other:?}"),
    }
}

fn error_construct(ast: &Ast, expr: ExprId) -> ErrorConstruct {
    match ast.expr(expr).kind {
        ExprKind::ErrorConstruct(construct) => construct,
        other => panic!("expected ErrorConstruct, got {other:?}"),
    }
}

fn catch_expr(ast: &Ast, expr: ExprId) -> CatchExpr {
    match ast.expr(expr).kind {
        ExprKind::Catch(catch) => catch,
        other => panic!("expected Catch, got {other:?}"),
    }
}

fn assert_call(ast: &Ast, pool: &InternPool, expr: ExprId, expected: &str) {
    match ast.expr(expr).kind {
        ExprKind::Call(name, _) => assert_eq!(pool.str(name), expected),
        other => panic!("expected Call({expected}), got {other:?}"),
    }
}

#[test]
fn error_declarations_parse_all_payload_forms() {
    let (ast, pool) = lex_and_parse(
        "error Timeout\nerror NotFound(str)\nerror HttpError(status: int, message: str,)\n",
    )
    .unwrap();
    let stmts = ast.top_level_stmts();
    assert_eq!(stmts.len(), 3);

    let unit = error_def(&ast, stmts[0]);
    assert_eq!(pool.str(unit.name.name), "Timeout");
    assert_eq!(unit.payload.kind, VariantKind::Unit);
    assert!(ast.struct_field_decls(unit.payload.fields).is_empty());

    let tuple = error_def(&ast, stmts[1]);
    assert_eq!(pool.str(tuple.name.name), "NotFound");
    assert_eq!(tuple.payload.kind, VariantKind::Tuple);
    let fields = ast.struct_field_decls(tuple.payload.fields);
    assert_eq!(fields.len(), 1);
    assert_eq!(pool.str(fields[0].0), "0");
    assert_eq!(pool.str(type_name(&fields[0].1).0), "str");

    let named = error_def(&ast, stmts[2]);
    assert_eq!(pool.str(named.name.name), "HttpError");
    assert_eq!(named.payload.kind, VariantKind::Named);
    let fields = ast.struct_field_decls(named.payload.fields);
    assert_eq!(fields.len(), 2);
    assert_eq!(pool.str(fields[0].0), "status");
    assert_eq!(pool.str(type_name(&fields[0].1).0), "int");
    assert_eq!(pool.str(fields[1].0), "message");
    assert_eq!(pool.str(type_name(&fields[1].1).0), "str");
}

#[test]
fn error_declaration_duplicate_payload_fields_survive_for_sema() {
    let (ast, pool) = lex_and_parse("error Duplicate(field: int, field: str)\n").unwrap();
    let def = error_def(&ast, only_stmt(&ast));
    let fields = ast.struct_field_decls(def.payload.fields);
    assert_eq!(fields.len(), 2);
    assert_eq!(pool.str(fields[0].0), "field");
    assert_eq!(pool.str(fields[1].0), "field");
}

#[test]
fn error_declaration_rejects_missing_and_mixed_payloads() {
    assert!(lex_and_parse("error Missing(\n").is_err());
    assert!(lex_and_parse("error Mixed(first: int, str)\n").is_err());
    assert!(lex_and_parse("error Mixed(str, first: int)\n").is_err());
}

#[test]
fn explicit_and_inferred_error_union_types_parse() {
    let (ast, pool) = lex_and_parse(
        "fn one() -> IoError!str:\n\treturn \"x\"\n\
         fn two() -> (IoError | ParseError)!Data:\n\treturn data\n\
         fn three() -> (IoError)!int:\n\treturn 1\n\
         fn four() -> !bool:\n\treturn true\n",
    )
    .unwrap();
    let stmts = ast.top_level_stmts();
    assert_eq!(stmts.len(), 4);

    let one = fn_def(&ast, stmts[0]);
    let TypeExprKind::ErrorUnion {
        error_set,
        success,
        inferred,
    } = one.return_type.expect("return type").kind
    else {
        panic!("expected an explicit error union");
    };
    assert!(!inferred);
    let errors = ast.type_expr_list(error_set);
    assert_eq!(errors.len(), 1);
    assert_eq!(pool.str(type_name(&errors[0]).0), "IoError");
    assert_eq!(pool.str(type_name(&ast.type_expr(success)).0), "str");

    let two = fn_def(&ast, stmts[1]);
    let TypeExprKind::ErrorUnion {
        error_set,
        success,
        inferred,
    } = two.return_type.expect("return type").kind
    else {
        panic!("expected a grouped error union");
    };
    assert!(!inferred);
    let errors = ast.type_expr_list(error_set);
    assert_eq!(errors.len(), 2);
    assert_eq!(pool.str(type_name(&errors[0]).0), "IoError");
    assert_eq!(pool.str(type_name(&errors[1]).0), "ParseError");
    assert_eq!(pool.str(type_name(&ast.type_expr(success)).0), "Data");

    let three = fn_def(&ast, stmts[2]);
    let TypeExprKind::ErrorUnion { error_set, .. } = three.return_type.expect("return type").kind
    else {
        panic!("expected a parenthesized single-error union");
    };
    assert_eq!(ast.type_expr_list(error_set).len(), 1);

    let four = fn_def(&ast, stmts[3]);
    let TypeExprKind::ErrorUnion {
        error_set,
        success,
        inferred,
    } = four.return_type.expect("return type").kind
    else {
        panic!("expected an inferred error union");
    };
    assert!(inferred);
    assert!(ast.type_expr_list(error_set).is_empty());
    assert_eq!(pool.str(type_name(&ast.type_expr(success)).0), "bool");
}

#[test]
fn error_constructor_candidates_parse_unit_positional_and_named_forms() {
    let (ast, pool) =
        lex_and_parse("Timeout\nNotFound(\"missing\")\nHttpError(status=404)\n").unwrap();
    let stmts = ast.top_level_stmts();

    let unit = error_construct(&ast, expr_stmt(&ast, stmts[0]));
    assert_eq!(pool.str(unit.name.name), "Timeout");
    assert!(unit.args.is_none());

    let positional = error_construct(&ast, expr_stmt(&ast, stmts[1]));
    assert_eq!(pool.str(positional.name.name), "NotFound");
    let args = positional.args.expect("positional args");
    assert_eq!(ast.expr_list(args.positional.expect("paren list")).len(), 1);
    assert!(args.named.is_none());

    let named = error_construct(&ast, expr_stmt(&ast, stmts[2]));
    assert_eq!(pool.str(named.name.name), "HttpError");
    let args = named.args.expect("named args");
    assert!(args.positional.is_none());
    let fields = ast.struct_field_inits(args.named.expect("named list"));
    assert_eq!(fields.len(), 1);
    assert_eq!(pool.str(fields[0].0), "status");
    assert_int_lit(&ast, fields[0].1, 404);
}

#[test]
fn named_parens_are_only_valid_on_error_constructor_candidates() {
    lex_and_parse("Foo(x=1)\n").expect("uppercase candidate accepts named parens");

    let (ok, _ast, errs, _pool) = lex_and_parse_recovering("foo(x=1)\n");
    assert!(ok);
    assert_eq!(
        errs.len(),
        1,
        "expected the named-argument diagnostic: {errs:?}"
    );
    assert!(matches!(
        errs[0].reason(),
        RichReason::Custom(ParseDiag::NamedArgInParens {
            enum_name: None,
            variant: None,
        })
    ));
}

#[test]
fn hidden_ok_and_err_source_syntax_is_rejected() {
    for src in [
        "Ok\n",
        "Err\n",
        "Ok(1)\n",
        "Err(1)\n",
        "match value:\n\tOk(_): 0\n",
        "match value:\n\tErr(_): 0\n",
    ] {
        assert!(
            lex_and_parse(src).is_err(),
            "hidden alternative accepted: {src}"
        );
    }
}

#[test]
fn catch_as_parses_direct_arms() {
    let (ast, pool) =
        lex_and_parse("value = find_user() catch as err:\n\tNotFound: \"unknown\"\n\t_: err\n")
            .unwrap();
    let catch = catch_expr(&ast, decl_init(&ast));
    assert_call(&ast, &pool, catch.operand, "find_user");
    assert_eq!(pool.str(catch.binding.expect("error binding").name), "err");
    let CatchBody::Arms(list) = catch.body else {
        panic!("expected direct catch arms");
    };
    let arms = ast.match_arms(list);
    assert_eq!(arms.len(), 2);
    assert!(matches!(
        ast.pattern(arms[0].pattern).kind,
        PatternKind::Variant {
            enum_name: None,
            variant,
            ..
        } if pool.str(variant) == "NotFound"
    ));
    assert!(matches!(
        ast.pattern(arms[1].pattern).kind,
        PatternKind::Wildcard
    ));
}

#[test]
fn catch_as_parses_nested_match_handler() {
    let (ast, pool) = lex_and_parse(
        "value = find_user() catch as err:\n\tmatch err:\n\t\tNotFound: 0\n\t\t_: 1\n",
    )
    .unwrap();
    let catch = catch_expr(&ast, decl_init(&ast));
    assert_eq!(pool.str(catch.binding.expect("error binding").name), "err");
    let CatchBody::Handler(MatchArmBody::Expr(handler)) = catch.body else {
        panic!("expected a nested match handler");
    };
    let ExprKind::Match { scrutinee, arms } = ast.expr(handler).kind else {
        panic!("expected the handler to be a match expression");
    };
    assert!(matches!(
        ast.expr(scrutinee).kind,
        ExprKind::Ident(name) if pool.str(name) == "err"
    ));
    assert_eq!(ast.match_arms(arms).len(), 2);
}

#[test]
fn catch_as_parses_ordinary_handler() {
    let (ast, _pool) = lex_and_parse("value = f() catch as err:\n\t0\n").unwrap();
    let catch = catch_expr(&ast, decl_init(&ast));
    let CatchBody::Handler(MatchArmBody::Expr(handler)) = catch.body else {
        panic!("expected an ordinary expression handler");
    };
    assert_int_lit(&ast, handler, 0);
}

#[test]
fn bare_catch_parses_only_direct_arms() {
    let (ast, _pool) = lex_and_parse("value = f() catch:\n\tNotFound: 0\n").unwrap();
    let catch = catch_expr(&ast, decl_init(&ast));
    assert!(catch.binding.is_none());
    let CatchBody::Arms(arms) = catch.body else {
        panic!("expected direct arms on bare catch");
    };
    assert_eq!(ast.match_arms(arms).len(), 1);

    assert!(lex_and_parse("value = f() catch:\n\t0\n").is_err());
}

#[test]
fn non_exhaustive_catch_fallback_is_rejected() {
    let (ok, ast, errs, _pool) = lex_and_parse_recovering("value = f() catch 0\n");
    assert!(ok);
    assert!(!errs.is_empty());
    assert!(matches!(ast.stmt(only_stmt(&ast)).kind, StmtKind::Error));
}

#[test]
fn try_and_catch_have_narrow_operands() {
    // `try f() + g()` is BinaryOp(Try(Call(f)), +, Call(g)).
    // `f() + g() catch as e: ...` is BinaryOp(Call(f), +, Catch(Call(g), ...)).
    let (ast, pool) =
        lex_and_parse("x = try f() + g()\ny = f() + g() catch as e:\n\t_: 0\n").unwrap();
    let stmts = ast.top_level_stmts();

    let x = var_decl(&ast, stmts[0]).initializer;
    let ExprKind::BinaryOp(lhs, BinaryOperator::Add, rhs) = ast.expr(x).kind else {
        panic!("expected try expression below addition");
    };
    let ExprKind::Try(operand) = ast.expr(lhs).kind else {
        panic!("expected Try on the left of addition");
    };
    assert_call(&ast, &pool, operand, "f");
    assert_call(&ast, &pool, rhs, "g");

    let y = var_decl(&ast, stmts[1]).initializer;
    let ExprKind::BinaryOp(lhs, BinaryOperator::Add, rhs) = ast.expr(y).kind else {
        panic!("expected catch expression below addition");
    };
    assert_call(&ast, &pool, lhs, "f");
    let catch = catch_expr(&ast, rhs);
    assert_call(&ast, &pool, catch.operand, "g");
}

#[test]
fn malformed_catch_recovers_at_next_declaration_without_orphans() {
    // A broken handler followed by `fn next(): ...` leaves a parseable next
    // declaration, one recovered Error statement, and no reachable orphan nodes.
    let (ok, ast, errs, _pool) = lex_and_parse_recovering(
        "broken = f() catch as err:\n\tNotFound:\nfn next():\n\treturn 0\n",
    );
    assert!(ok, "recovery should produce a partial program: {errs:?}");
    assert!(!errs.is_empty());
    let stmts = ast.top_level_stmts();
    assert_eq!(stmts.len(), 2, "expected one Error and one FunctionDef");
    assert!(matches!(ast.stmt(stmts[0]).kind, StmtKind::Error));
    let next = fn_def(&ast, stmts[1]);
    assert_eq!(fn_body(&ast, next).len(), 1);

    let (reachable_exprs, reachable_stmts) = super::arena_tests::reachable_node_counts(&ast);
    assert_eq!(
        reachable_exprs, 1,
        "only next()'s return literal is reachable"
    );
    assert_eq!(
        reachable_stmts, 3,
        "only the Error, FunctionDef, and Return statements are reachable"
    );
}
