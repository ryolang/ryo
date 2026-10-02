//! Destructuring-pattern parser tests (M10): `(a, b) = e`, `a, b = e`,
//! `{q, r} = e`, `{x = quot} = e`, nesting, wildcards, trailing
//! commas, and the `(a) = x` targeted error. Split out of
//! `parser/tests.rs` to stay under the file-length limit; the shared
//! parse helpers live there as `pub(super)`.

use super::tests::*;
use super::*;
use chumsky::error::RichReason;

/// The `(target, value)` of a Destructure statement, panicking on any
/// other statement kind.
fn destructure(ast: &Ast, stmt: StmtId) -> (PatternId, ExprId) {
    match &ast.stmt(stmt).kind {
        StmtKind::Destructure { target, value } => (*target, *value),
        other => panic!("expected Destructure, got {other:?}"),
    }
}

/// The positional elements behind a `PatternKind::Positional`, panicking
/// on any other pattern kind.
fn positional_elems(ast: &Ast, pat: PatternId) -> Vec<PatternId> {
    match &ast.pattern(pat).kind {
        PatternKind::Positional(elems) => ast.pattern_list(*elems).to_vec(),
        other => panic!("expected Positional pattern, got {other:?}"),
    }
}

/// The bound name of a `PatternKind::Bind`, panicking on any other kind.
fn bind_name<'a>(ast: &'a Ast, pool: &'a InternPool, pat: PatternId) -> &'a str {
    match &ast.pattern(pat).kind {
        PatternKind::Bind(ident) => pool.str(ident.name),
        other => panic!("expected Bind pattern, got {other:?}"),
    }
}

fn assert_wildcard(ast: &Ast, pat: PatternId) {
    assert!(
        matches!(ast.pattern(pat).kind, PatternKind::Wildcard),
        "expected Wildcard pattern, got {:?}",
        ast.pattern(pat).kind
    );
}

#[test]
fn destructure_positional_parses() {
    // Paren form at the top level: `(a, b) = f()`.
    let (ast, pool) = lex_and_parse("(a, b) = f()\n").unwrap();
    let (target, value) = destructure(&ast, only_stmt(&ast));
    let elems = positional_elems(&ast, target);
    assert_eq!(elems.len(), 2);
    assert_eq!(bind_name(&ast, &pool, elems[0]), "a");
    assert_eq!(bind_name(&ast, &pool, elems[1]), "b");
    assert!(
        matches!(ast.expr(value).kind, ExprKind::Call(_, _)),
        "value should be the f() call, got {:?}",
        ast.expr(value).kind
    );

    // Paren-less form inside a function body: `x, y = f()`.
    let (ast, pool) = lex_and_parse("fn main():\n\tx, y = f()\n").unwrap();
    let f = fn_def(&ast, only_stmt(&ast));
    let (target, value) = destructure(&ast, fn_body(&ast, f)[0]);
    let elems = positional_elems(&ast, target);
    assert_eq!(elems.len(), 2);
    assert_eq!(bind_name(&ast, &pool, elems[0]), "x");
    assert_eq!(bind_name(&ast, &pool, elems[1]), "y");
    assert!(
        matches!(ast.expr(value).kind, ExprKind::Call(_, _)),
        "value should be the f() call, got {:?}",
        ast.expr(value).kind
    );
}

#[test]
fn destructure_named_pun_and_rename_parses() {
    // `{q, r} = e` — field puns: each field binds a local of the same name.
    let (ast, pool) = lex_and_parse("{q, r} = dm\n").unwrap();
    let (target, _) = destructure(&ast, only_stmt(&ast));
    let fields = match &ast.pattern(target).kind {
        PatternKind::Anon { fields } => ast.pattern_field_list(*fields),
        other => panic!("expected Anon pattern, got {other:?}"),
    };
    assert_eq!(fields.len(), 2);
    assert_eq!(pool.str(fields[0].name), "q");
    assert_eq!(pool.str(fields[0].binding.name), "q");
    assert_eq!(pool.str(fields[1].name), "r");
    assert_eq!(pool.str(fields[1].binding.name), "r");

    // `{x = quot} = e` — a rename: field `x` binds the local `quot`.
    let (ast, pool) = lex_and_parse("{x = quot} = dm\n").unwrap();
    let (target, _) = destructure(&ast, only_stmt(&ast));
    let fields = match &ast.pattern(target).kind {
        PatternKind::Anon { fields } => ast.pattern_field_list(*fields),
        other => panic!("expected Anon pattern, got {other:?}"),
    };
    assert_eq!(fields.len(), 1);
    assert_eq!(pool.str(fields[0].name), "x");
    assert_eq!(pool.str(fields[0].binding.name), "quot");
}

#[test]
fn destructure_nested_parses() {
    // `(a, (b, c)) = f()` — positional elements are full patterns.
    let (ast, pool) = lex_and_parse("(a, (b, c)) = f()\n").unwrap();
    let (target, _) = destructure(&ast, only_stmt(&ast));
    let outer = positional_elems(&ast, target);
    assert_eq!(outer.len(), 2);
    assert_eq!(bind_name(&ast, &pool, outer[0]), "a");
    let inner = positional_elems(&ast, outer[1]);
    assert_eq!(inner.len(), 2);
    assert_eq!(bind_name(&ast, &pool, inner[0]), "b");
    assert_eq!(bind_name(&ast, &pool, inner[1]), "c");
}

#[test]
fn destructure_wildcard_parses() {
    // `{q, _} = e` — the `_` field binds nothing.
    let (ast, pool) = lex_and_parse("{q, _} = dm\n").unwrap();
    let (target, _) = destructure(&ast, only_stmt(&ast));
    let fields = match &ast.pattern(target).kind {
        PatternKind::Anon { fields } => ast.pattern_field_list(*fields),
        other => panic!("expected Anon pattern, got {other:?}"),
    };
    assert_eq!(fields.len(), 2);
    assert_eq!(pool.str(fields[0].name), "q");
    assert_eq!(pool.str(fields[1].name), "_");

    // `(a, _, c) = e` — wildcard positional element.
    let (ast, pool) = lex_and_parse("(a, _, c) = f()\n").unwrap();
    let (target, _) = destructure(&ast, only_stmt(&ast));
    let elems = positional_elems(&ast, target);
    assert_eq!(elems.len(), 3);
    assert_eq!(bind_name(&ast, &pool, elems[0]), "a");
    assert_wildcard(&ast, elems[1]);
    assert_eq!(bind_name(&ast, &pool, elems[2]), "c");
}

#[test]
fn single_paren_not_a_pattern() {
    // `(a) = x` is the classic mistake: a one-element parenthesized
    // destructuring without the trailing comma. The parser reports the
    // targeted message and recovers the line to an Error statement.
    let (ok, ast, errs, _pool) = lex_and_parse_recovering("(a) = x\n");
    assert!(ok, "the broken line recovers to a partial program");
    assert_eq!(errs.len(), 1, "expected one diagnostic: {errs:?}");
    match errs[0].reason() {
        RichReason::Custom(pd @ ParseDiag::SingleElemDestructuring) => {
            assert_eq!(
                pd.to_string(),
                "single-element destructuring needs a trailing comma — \
                 `(a,)` — or write plain `a = x`"
            );
        }
        other => panic!("expected SingleElemDestructuring, got {other:?}"),
    }
    assert!(
        is_error_stmt(&ast, only_stmt(&ast)),
        "the line must recover to an Error statement"
    );

    // `(a)` WITHOUT `=` stays a plain parenthesized expression statement.
    let (ast, _pool) = lex_and_parse("(a)\n").unwrap();
    match &ast.stmt(only_stmt(&ast)).kind {
        StmtKind::ExprStmt(value) => assert!(
            matches!(ast.expr(*value).kind, ExprKind::Ident(_)),
            "expected the bare inner expression, got {:?}",
            ast.expr(*value).kind
        ),
        other => panic!("expected ExprStmt, got {other:?}"),
    }
}

#[test]
fn destructure_trailing_comma_forms() {
    // `(a, b,) = e` — trailing comma inside the parens.
    let (ast, pool) = lex_and_parse("(a, b,) = f()\n").unwrap();
    let (target, _) = destructure(&ast, only_stmt(&ast));
    let elems = positional_elems(&ast, target);
    assert_eq!(elems.len(), 2);
    assert_eq!(bind_name(&ast, &pool, elems[0]), "a");
    assert_eq!(bind_name(&ast, &pool, elems[1]), "b");

    // `(a,) = e` — the one-element positional destructuring; the
    // trailing comma is what makes it a pattern rather than a paren
    // expression.
    let (ast, pool) = lex_and_parse("(a,) = f()\n").unwrap();
    let (target, _) = destructure(&ast, only_stmt(&ast));
    let elems = positional_elems(&ast, target);
    assert_eq!(elems.len(), 1);
    assert_eq!(bind_name(&ast, &pool, elems[0]), "a");
}
