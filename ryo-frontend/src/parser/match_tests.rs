//! Match-expression parser tests (M12): `match value:` with variant,
//! literal, wildcard, anonymous, and positional arm patterns; expression
//! and statement arm bodies; and match in expression position. Split out
//! of `parser/tests.rs` per the R2 ruling — parser.rs has no headroom for
//! inline tests; the shared parse helpers live there as `pub(super)`.

use super::tests::*;
use super::*;

/// The `(scrutinee, arms)` of a `ExprKind::Match`, panicking on any
/// other expression kind.
fn match_expr(ast: &Ast, expr: ExprId) -> (ExprId, MatchArmList) {
    match ast.expr(expr).kind {
        ExprKind::Match { scrutinee, arms } => (scrutinee, arms),
        other => panic!("expected Match, got {other:?}"),
    }
}

/// The `ExprId` of an `ExprStmt`, panicking on any other kind.
fn expr_stmt(ast: &Ast, stmt: StmtId) -> ExprId {
    match &ast.stmt(stmt).kind {
        StmtKind::ExprStmt(e) => *e,
        other => panic!("expected ExprStmt, got {other:?}"),
    }
}

/// The `PatternKind::Variant` payload of a pattern, panicking on any
/// other pattern kind.
fn as_variant(
    ast: &Ast,
    pat: PatternId,
) -> (Option<StringId>, StringId, Option<PatternVariantArgs>) {
    match ast.pattern(pat).kind {
        PatternKind::Variant {
            enum_name,
            variant,
            args,
        } => (enum_name, variant, args),
        other => panic!("expected Variant pattern, got {other:?}"),
    }
}

/// The `LitPattern` payload of a `PatternKind::Literal` pattern,
/// panicking on any other pattern kind.
fn literal(ast: &Ast, pat: PatternId) -> LitPattern {
    match ast.pattern(pat).kind {
        PatternKind::Literal(lit) => lit,
        other => panic!("expected Literal pattern, got {other:?}"),
    }
}

/// The `PatternField` slice of a `PatternKind::Anon` pattern, panicking
/// on any other pattern kind.
fn anon_fields(ast: &Ast, pat: PatternId) -> Vec<PatternField> {
    match ast.pattern(pat).kind {
        PatternKind::Anon { fields } => ast.pattern_field_list(fields).to_vec(),
        other => panic!("expected Anon pattern, got {other:?}"),
    }
}

/// The positional element patterns of a `PatternKind::Positional`
/// pattern, panicking on any other pattern kind.
fn positional_elems(ast: &Ast, pat: PatternId) -> Vec<PatternId> {
    match ast.pattern(pat).kind {
        PatternKind::Positional(elems) => ast.pattern_list(elems).to_vec(),
        other => panic!("expected Positional pattern, got {other:?}"),
    }
}

#[test]
fn match_variants_and_wildcard_parse() {
    let (ast, pool) =
        lex_and_parse("match value:\n\tOption.Some(v): v + 1\n\tOption.None: 0\n\t_: 0\n").unwrap();
    let (scrutinee, arm_list) = match_expr(&ast, expr_stmt(&ast, only_stmt(&ast)));
    assert!(matches!(ast.expr(scrutinee).kind, ExprKind::Ident(n) if pool.str(n) == "value"));
    let arms = ast.match_arms(arm_list);
    assert_eq!(arms.len(), 3);

    // `Option.Some(v): v + 1` — qualified variant, positional bind, Expr body.
    let (enum_name, variant, args) = as_variant(&ast, arms[0].pattern);
    assert_eq!(enum_name.map(|n| pool.str(n)), Some("Option"));
    assert_eq!(pool.str(variant), "Some");
    let args = args.expect("Some carries a paren arg list");
    assert!(args.by_position);
    let argv = ast.variant_arg_list(args.list);
    assert_eq!(argv.len(), 1);
    assert_eq!(argv[0].name, None);
    assert!(
        matches!(argv[0].target, PatternVariantArgTarget::Bind(n) if pool.str(n) == "v"),
        "expected Bind(v), got {:?}",
        argv[0].target
    );
    assert!(
        matches!(arms[0].body, MatchArmBody::Expr(_)),
        "single-line expression arms keep an Expr body, got {:?}",
        arms[0].body
    );

    // `Option.None: 0` — qualified unit variant, no args.
    let (enum_name, variant, args) = as_variant(&ast, arms[1].pattern);
    assert_eq!(enum_name.map(|n| pool.str(n)), Some("Option"));
    assert_eq!(pool.str(variant), "None");
    assert!(args.is_none());
    assert!(matches!(arms[1].body, MatchArmBody::Expr(_)));

    // `_: 0` — the catch-all wildcard.
    assert!(
        matches!(ast.pattern(arms[2].pattern).kind, PatternKind::Wildcard),
        "expected Wildcard, got {:?}",
        ast.pattern(arms[2].pattern).kind
    );
}

#[test]
fn match_in_expression_position_parses() {
    // Match is an atom: it nests anywhere an expression does. Inside
    // a function body, `x = ...` stays an AssignOrDecl (top level it
    // would be a VarDecl).
    let (ast, pool) =
        lex_and_parse("fn f():\n\tx = match opt:\n\t\tSome(v): v\n\t\tNone: 0\n").unwrap();
    let def = fn_def(&ast, only_stmt(&ast));
    let value = assign_value(&ast, fn_body(&ast, def)[0]);
    let (scrutinee, arm_list) = match_expr(&ast, value);
    assert!(matches!(ast.expr(scrutinee).kind, ExprKind::Ident(n) if pool.str(n) == "opt"));
    assert_eq!(ast.match_arms(arm_list).len(), 2);
}

#[test]
fn match_literal_arms_parse() {
    let (ast, pool) =
        lex_and_parse("match cmd:\n\t42: \"a\"\n\t-1: \"b\"\n\t\"quit\": 0\n\ttrue: 1\n").unwrap();
    let (_, arm_list) = match_expr(&ast, expr_stmt(&ast, only_stmt(&ast)));
    let arms = ast.match_arms(arm_list);
    assert_eq!(arms.len(), 4);
    assert_eq!(literal(&ast, arms[0].pattern), LitPattern::Int(42));
    assert_eq!(literal(&ast, arms[1].pattern), LitPattern::Int(-1));
    assert_eq!(
        literal(&ast, arms[2].pattern),
        LitPattern::Str(intern(&pool, "quit"))
    );
    assert_eq!(literal(&ast, arms[3].pattern), LitPattern::Bool(true));
}

#[test]
fn match_variant_forms_parse() {
    // Unqualified unit variant and qualified unit variant.
    let (ast, pool) = lex_and_parse("match c:\n\tRed: 1\n\tColor.Green: 2\n").unwrap();
    let (_, arm_list) = match_expr(&ast, expr_stmt(&ast, only_stmt(&ast)));
    let arms = ast.match_arms(arm_list);
    let (enum_name, variant, args) = as_variant(&ast, arms[0].pattern);
    assert_eq!(enum_name, None);
    assert_eq!(pool.str(variant), "Red");
    assert!(args.is_none());
    let (enum_name, variant, args) = as_variant(&ast, arms[1].pattern);
    assert_eq!(enum_name.map(|n| pool.str(n)), Some("Color"));
    assert_eq!(pool.str(variant), "Green");
    assert!(args.is_none());

    // Parenthesized positional binds, one and two elements.
    let (ast, pool) =
        lex_and_parse("match s:\n\tCircle(radius): radius\n\tPair(a, b): a\n").unwrap();
    let (_, arm_list) = match_expr(&ast, expr_stmt(&ast, only_stmt(&ast)));
    let arms = ast.match_arms(arm_list);
    let (_, variant, args) = as_variant(&ast, arms[0].pattern);
    assert_eq!(pool.str(variant), "Circle");
    let args = args.expect("Circle carries a paren arg list");
    assert!(args.by_position);
    let argv = ast.variant_arg_list(args.list);
    assert_eq!(argv.len(), 1);
    assert!(matches!(argv[0].target, PatternVariantArgTarget::Bind(n) if pool.str(n) == "radius"));
    let (_, variant, args) = as_variant(&ast, arms[1].pattern);
    assert_eq!(pool.str(variant), "Pair");
    let argv = ast.variant_arg_list(args.expect("Pair carries a paren arg list").list);
    assert_eq!(argv.len(), 2);
    assert!(matches!(argv[0].target, PatternVariantArgTarget::Bind(n) if pool.str(n) == "a"));
    assert!(matches!(argv[1].target, PatternVariantArgTarget::Bind(n) if pool.str(n) == "b"));

    // Braced named binds on a variant: `Coords{x=px, y=py}`.
    let (ast, pool) = lex_and_parse("match p:\n\tCoords{x=px, y=py}: px\n").unwrap();
    let (_, arm_list) = match_expr(&ast, expr_stmt(&ast, only_stmt(&ast)));
    let arms = ast.match_arms(arm_list);
    let (enum_name, variant, args) = as_variant(&ast, arms[0].pattern);
    assert_eq!(enum_name, None);
    assert_eq!(pool.str(variant), "Coords");
    let args = args.expect("Coords carries a brace arg list");
    assert!(!args.by_position);
    let argv = ast.variant_arg_list(args.list);
    assert_eq!(argv.len(), 2);
    assert_eq!(argv[0].name.map(|n| pool.str(n)), Some("x"));
    assert!(matches!(argv[0].target, PatternVariantArgTarget::Bind(n) if pool.str(n) == "px"));
    assert_eq!(argv[1].name.map(|n| pool.str(n)), Some("y"));
    assert!(matches!(argv[1].target, PatternVariantArgTarget::Bind(n) if pool.str(n) == "py"));

    // Punning anonymous struct pattern `{x, y}` — binds fields x and y.
    let (ast, pool) = lex_and_parse("match p:\n\t{x, y}: x\n").unwrap();
    let (_, arm_list) = match_expr(&ast, expr_stmt(&ast, only_stmt(&ast)));
    let arms = ast.match_arms(arm_list);
    let fields = anon_fields(&ast, arms[0].pattern);
    assert_eq!(fields.len(), 2);
    assert_eq!(pool.str(fields[0].name), "x");
    assert_eq!(pool.str(fields[1].name), "y");
    assert!(
        matches!(fields[0].kind, PatternFieldKind::Pun),
        "expected pun, got {:?}",
        fields[0].kind
    );
    assert!(
        matches!(fields[1].kind, PatternFieldKind::Pun),
        "expected pun, got {:?}",
        fields[1].kind
    );
}

#[test]
fn match_literal_field_patterns_parse() {
    // Anonymous struct with a literal field and a wildcard field.
    let (ast, pool) = lex_and_parse("match p:\n\t{x=0, y=_}: x\n").unwrap();
    let (_, arm_list) = match_expr(&ast, expr_stmt(&ast, only_stmt(&ast)));
    let arms = ast.match_arms(arm_list);
    let fields = anon_fields(&ast, arms[0].pattern);
    assert_eq!(fields.len(), 2);
    assert_eq!(pool.str(fields[0].name), "x");
    assert_eq!(
        fields[0].kind,
        PatternFieldKind::Literal(LitPattern::Int(0))
    );
    assert_eq!(pool.str(fields[1].name), "y");
    assert_eq!(fields[1].kind, PatternFieldKind::Wildcard);

    // Positional with a literal element and a wildcard element.
    let (ast, _pool) = lex_and_parse("match t:\n\t(0, _): 1\n").unwrap();
    let (_, arm_list) = match_expr(&ast, expr_stmt(&ast, only_stmt(&ast)));
    let arms = ast.match_arms(arm_list);
    let elems = positional_elems(&ast, arms[0].pattern);
    assert_eq!(elems.len(), 2);
    assert_eq!(literal(&ast, elems[0]), LitPattern::Int(0));
    assert!(
        matches!(ast.pattern(elems[1]).kind, PatternKind::Wildcard),
        "expected Wildcard, got {:?}",
        ast.pattern(elems[1]).kind
    );
}

#[test]
fn match_statement_arms_parse() {
    // Single-line statement arm: `return e` fails the expression
    // alternative and wraps into a one-element StmtList.
    let (ast, _pool) =
        lex_and_parse("fn f():\n\tmatch r:\n\t\tErr(e): return e\n\t\tOk(v): v\n").unwrap();
    let def = fn_def(&ast, only_stmt(&ast));
    let m = match_expr(&ast, expr_stmt(&ast, fn_body(&ast, def)[0]));
    let arms = ast.match_arms(m.1);
    assert_eq!(arms.len(), 2);
    let body = arms[0].body;
    let list = match body {
        MatchArmBody::Stmts(list) => list,
        other => panic!("expected Stmts body for `return e`, got {other:?}"),
    };
    let stmts = ast.stmt_list(list);
    assert_eq!(
        stmts.len(),
        1,
        "single-line statement arms wrap to one stmt"
    );
    assert!(
        matches!(ast.stmt(stmts[0]).kind, StmtKind::Return(_)),
        "expected Return, got {:?}",
        ast.stmt(stmts[0]).kind
    );
    assert!(
        matches!(arms[1].body, MatchArmBody::Expr(_)),
        "expression arms keep an Expr body, got {:?}",
        arms[1].body
    );

    // Block arm: an indented body after the colon.
    let (ast, _pool) = lex_and_parse(
        "fn f():\n\tmatch r:\n\t\tSome(v):\n\t\t\tprint(v)\n\t\t\treturn v\n\t\tNone: 0\n",
    )
    .unwrap();
    let def = fn_def(&ast, only_stmt(&ast));
    let m = match_expr(&ast, expr_stmt(&ast, fn_body(&ast, def)[0]));
    let arms = ast.match_arms(m.1);
    assert_eq!(arms.len(), 2);
    let list = match arms[0].body {
        MatchArmBody::Stmts(list) => list,
        other => panic!("expected Stmts body for a block arm, got {other:?}"),
    };
    let stmts = ast.stmt_list(list);
    assert_eq!(stmts.len(), 2);
    assert!(matches!(ast.stmt(stmts[0]).kind, StmtKind::ExprStmt(_)));
    assert!(matches!(ast.stmt(stmts[1]).kind, StmtKind::Return(_)));
    assert!(
        matches!(arms[1].body, MatchArmBody::Expr(_)),
        "trailing expression arm, got {:?}",
        arms[1].body
    );
}

#[test]
fn match_blank_lines_and_nested_in_call_parse() {
    // Blank lines between arms are tolerated.
    let (ast, _pool) = lex_and_parse("match v:\n\tSome(v): v\n\n\tNone: 0\n").unwrap();
    let (_, arm_list) = match_expr(&ast, expr_stmt(&ast, only_stmt(&ast)));
    assert_eq!(ast.match_arms(arm_list).len(), 2);

    // Match nested in a call argument. The call's `)` closes on its
    // own line: the arm block's `Dedent` precedes that line's newline
    // (the preprocessor emits dedents only at line boundaries), and
    // the match absorbs the newline before the `)`.
    let (ast, _pool) = lex_and_parse("print(match opt:\n\tSome(v): v\n\tNone: 0\n)\n").unwrap();
    let call = match ast.expr(expr_stmt(&ast, only_stmt(&ast))).kind {
        ExprKind::Call(_, args) => args,
        other => panic!("expected Call, got {other:?}"),
    };
    let args = ast.expr_list(call);
    assert_eq!(args.len(), 1);
    let (_, arm_list) = match_expr(&ast, args[0]);
    assert_eq!(ast.match_arms(arm_list).len(), 2);
}

/// Bare lowercase identifier as a WHOLE arm pattern is rejected with a
/// targeted diagnostic (bindings belong inside variant/payload
/// positions); nested bind positions stay legal.
#[test]
fn match_bare_identifier_arm_rejected() {
    let (_ok, _ast, errs, _pool) = lex_and_parse_recovering("match v:\n\tfoo: 1\n\t_: 0\n");
    assert_eq!(errs.len(), 1, "expected one diagnostic: {errs:?}");
    match errs[0].reason() {
        chumsky::error::RichReason::Custom(ParseDiag::Message(msg)) => {
            assert!(
                msg.contains("`foo`") && msg.contains("Some(foo)"),
                "diagnostic should name the identifier and the variant suggestion, got: {msg}"
            );
        }
        other => panic!("expected the bare-identifier diagnostic, got {other:?}"),
    }
}

/// Nested binds stay legal: positional and anonymous elements bind.
#[test]
fn match_nested_binds_parse() {
    let (ast, pool) = lex_and_parse("match t:\n\t(a, b): a\n\t{x, y}: x\n").unwrap();
    let (_, arm_list) = match_expr(&ast, expr_stmt(&ast, only_stmt(&ast)));
    let arms = ast.match_arms(arm_list);
    let elems = positional_elems(&ast, arms[0].pattern);
    assert_eq!(elems.len(), 2);
    for (elem, want) in elems.iter().zip(["a", "b"]) {
        match ast.pattern(*elem).kind {
            PatternKind::Bind(ident) => assert_eq!(pool.str(ident.name), want),
            other => panic!("expected Bind({want}), got {other:?}"),
        }
    }
    let fields = anon_fields(&ast, arms[1].pattern);
    assert_eq!(fields.len(), 2);
    assert!(
        matches!(fields[0].kind, PatternFieldKind::Pun) && fields[1].kind == PatternFieldKind::Pun
    );
}

/// Helper: intern a string in a pool that came out of a parse (the
/// literal's payload id is what the lexer interned).
fn intern(pool: &InternPool, s: &str) -> StringId {
    pool.find_str(s)
        .unwrap_or_else(|| panic!("{s:?} should be interned"))
}
