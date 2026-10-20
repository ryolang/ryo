//! Anonymous type-literal and tuple-sugar parser tests (M10): `{q: int,
//! r: int}` type literals in every annotation position, `(a, b)` tuple
//! literals, `(T1, T2)` positional type sugar, and positional field
//! assignment and borrow targets. Split out of `grammar/tests.rs` to stay
//! under the file-length limit; the shared parse helpers live there as
//! `pub(super)`.

use super::tests::*;
use super::*;
use chumsky::error::RichReason;

// ---- M10: anonymous struct type literals (`{q: int, r: int}`) ----

/// The `TypeExprKind::Anon` field list behind a compound type
/// expression, panicking on any other kind.
fn anon_type_fields<'a>(ast: &'a Ast, texpr: &TypeExpr) -> &'a [(StringId, TypeExpr)] {
    match &texpr.kind {
        TypeExprKind::Anon { fields } => ast.type_field_list(*fields),
        other => panic!("expected Anon type literal, got {other:?}"),
    }
}

#[test]
fn type_literal_in_return_type() {
    let (ast, pool) =
        lex_and_parse("fn divmod(a: int, b: int) -> {q: int, r: int}:\n\treturn {q=1, r=2}\n")
            .unwrap();
    let f = fn_def(&ast, only_stmt(&ast));
    let fields = anon_type_fields(&ast, f.return_type.as_ref().unwrap());
    assert_eq!(fields.len(), 2);
    assert_eq!(pool.str(fields[0].0), "q");
    assert_eq!(pool.str(fields[1].0), "r");
    match fields[1].1.kind {
        TypeExprKind::Name {
            name,
            is_view: false,
        } => assert_eq!(pool.str(name), "int"),
        other => panic!("expected plain `int` name, got {other:?}"),
    }
}

#[test]
fn type_literal_numeric_keys() {
    // Brace type literals accept numeric keys with the value literal's
    // canonicalization — `{0: int, 1: str}` is the same structural
    // type as the `(int, str)` paren sugar.
    let (ast, pool) = lex_and_parse("fn f() -> {0: int, 1: str}:\n\treturn (1, \"a\")\n").unwrap();
    let f = fn_def(&ast, only_stmt(&ast));
    let fields = anon_type_fields(&ast, f.return_type.as_ref().unwrap());
    assert_eq!(fields.len(), 2);
    assert_eq!(pool.str(fields[0].0), "0");
    assert_eq!(pool.str(fields[1].0), "1");
}

#[test]
fn type_literal_in_param() {
    let (ast, pool) =
        lex_and_parse("fn dist(p: {x: float, y: float}) -> float:\n\treturn p.x\n").unwrap();
    let f = fn_def(&ast, only_stmt(&ast));
    let fields = anon_type_fields(&ast, &f.params[0].type_annotation);
    assert_eq!(fields.len(), 2);
    assert_eq!(pool.str(fields[0].0), "x");
    match fields[1].1.kind {
        TypeExprKind::Name { name, .. } => assert_eq!(pool.str(name), "float"),
        other => panic!("expected Name, got {other:?}"),
    }
}

#[test]
fn type_literal_in_var_annotation() {
    let (ast, _pool) = lex_and_parse("fn main():\n\tdm: {q: int, r: int} = {q=1, r=2}\n").unwrap();
    let f = fn_def(&ast, only_stmt(&ast));
    let decl = match &ast.stmt(fn_body(&ast, f)[0]).kind {
        StmtKind::VarDecl(decl) => decl,
        other => panic!("expected VarDecl, got {other:?}"),
    };
    let fields = anon_type_fields(&ast, decl.type_annotation.as_ref().unwrap());
    assert_eq!(fields.len(), 2);
}

#[test]
fn type_literal_rejects_empty_braces() {
    // `{}` stays reserved for the future empty map literal in type
    // position too — there is no empty anonymous struct.
    let errs = lex_and_parse("p: {} = {x=1}\n").expect_err("empty type braces must be rejected");
    let msg = errs
        .iter()
        .find_map(|e| match e.reason() {
            RichReason::Custom(pd @ ParseDiag::EmptyAnonStruct) => Some(pd.to_string()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("expected an EmptyAnonStruct diagnostic, got: {errs:?}"));
    assert!(msg.contains("map literal"), "{msg}");
}

#[test]
fn type_literal_accepted_in_struct_decl_field() {
    // Struct declaration fields take full type expressions: anonymous
    // type literals (M10) in both spellings nest inside named structs,
    // closing the last nesting gap. The define DFS walks their inner
    // named references for by-value cycles.
    let (ast, _pool) = lex_and_parse("struct P:\n\tf: {x: int}\n\tg: (int, str)\n").unwrap();
    let def = struct_def_stmt(&ast, only_stmt(&ast));
    let fields = ast.struct_field_decls(def.fields);
    assert!(
        matches!(fields[0].1.kind, ryo_core::ast::TypeExprKind::Anon { .. }),
        "brace literal field, got {:?}",
        fields[0].1.kind
    );
    assert!(
        matches!(fields[1].1.kind, ryo_core::ast::TypeExprKind::Positional(_)),
        "paren literal field, got {:?}",
        fields[1].1.kind
    );
}

// ---- M10: tuple sugar — `(a, b)` literals, `(x,)`, `(T1, T2)` types ----

#[test]
fn tuple_literal_parses() {
    // Tuple sugar: `(17, "alice")` is the anonymous struct literal
    // `{0=17, 1="alice"}` — fields "0"/"1" minted in written order
    // against the state's pool, exactly like numeric brace keys.
    let (ast, pool) = lex_and_parse("p = (17, \"alice\")\n").unwrap();
    let init = decl_init(&ast);
    match &ast.expr(init).kind {
        ExprKind::StructLiteral(lit) => {
            assert!(lit.name.is_none());
            let fields = ast.struct_field_inits(lit.fields);
            assert_eq!(fields.len(), 2);
            assert_eq!(pool.str(fields[0].0), "0");
            assert_eq!(pool.str(fields[1].0), "1");
            assert_int_lit(&ast, fields[0].1, 17);
            assert!(matches!(
                ast.expr(fields[1].1).kind,
                ExprKind::Literal(Literal::Str(_))
            ));
        }
        other => panic!("expected StructLiteral, got {:?}", other),
    }
}

#[test]
fn single_element_tuple_requires_comma() {
    // `(x)` stays a plain parenthesized expression — the inner node
    // is returned as-is. `(x,)` (trailing comma mandatory) is the
    // one-element tuple: an anonymous literal with field "0".
    let (ast, _) = lex_and_parse("x = (v)\n").unwrap();
    let init = decl_init(&ast);
    assert!(
        matches!(ast.expr(init).kind, ExprKind::Ident(_)),
        "expected the bare inner expression, got {:?}",
        ast.expr(init).kind
    );

    let (ast, pool) = lex_and_parse("x = (v,)\n").unwrap();
    let init = decl_init(&ast);
    match &ast.expr(init).kind {
        ExprKind::StructLiteral(lit) => {
            assert!(lit.name.is_none());
            let fields = ast.struct_field_inits(lit.fields);
            assert_eq!(fields.len(), 1);
            assert_eq!(pool.str(fields[0].0), "0");
            assert!(matches!(ast.expr(fields[0].1).kind, ExprKind::Ident(_)));
        }
        other => panic!("expected StructLiteral, got {:?}", other),
    }
}

#[test]
fn empty_parens_unit_error() {
    // `()` is the unit — there is no empty tuple. Diagnosed
    // (UnitParen: `void` is the unit type; write `none` or a
    // one-element tuple `(x,)`) and recovered as an empty anonymous
    // literal, exactly like `{}` (E0109).
    let errs = lex_and_parse("p = ()\n").expect_err("empty parens must be rejected");
    let msg = errs
        .iter()
        .find_map(|e| match e.reason() {
            RichReason::Custom(pd @ ParseDiag::UnitParen) => Some(pd.to_string()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("expected a UnitParen diagnostic, got: {errs:?}"));
    assert!(msg.contains("void"), "{msg}");
    assert!(msg.contains("none"), "{msg}");
    assert!(msg.contains("(x,)"), "{msg}");
}

#[test]
fn positional_type_sugar_parses() {
    // `(int, str)` in a return type is TypeExprKind::Positional —
    // sugar over the anonymous `{0: int, 1: str}`. astgen's resolve
    // path interns the "0"/"1" names so every spelling of the shape
    // lands on one TypeId (pinned in astgen's type-literal tests).
    let (ast, pool) =
        lex_and_parse("fn pair() -> (int, str):\n\treturn (17, \"alice\")\n").unwrap();
    let f = fn_def(&ast, only_stmt(&ast));
    let ret = f.return_type.as_ref().expect("return type");
    let elems = match &ret.kind {
        TypeExprKind::Positional(elems) => ast.type_expr_list(*elems),
        other => panic!("expected Positional type expression, got {other:?}"),
    };
    assert_eq!(elems.len(), 2);
    assert_eq!(pool.str(type_name(&elems[0]).0), "int");
    assert_eq!(pool.str(type_name(&elems[1]).0), "str");
}

#[test]
fn positional_type_sugar_single_element() {
    // `(int,)` is the one-element positional type — field "0".
    let (ast, pool) = lex_and_parse("fn f() -> (int,):\n\treturn (7,)\n").unwrap();
    let f = fn_def(&ast, only_stmt(&ast));
    let ret = f.return_type.as_ref().expect("return type");
    let elems = match &ret.kind {
        TypeExprKind::Positional(elems) => ast.type_expr_list(*elems),
        other => panic!("expected Positional type expression, got {other:?}"),
    };
    assert_eq!(elems.len(), 1);
    assert_eq!(pool.str(type_name(&elems[0]).0), "int");
}

#[test]
fn pos_field_assign_target_parses() {
    // `p.0 = v` — positional WRITE shares the read side's field-key
    // syntax: the hop canonicalizes to its numeric value, so tuple and
    // anonymous-struct fields are assignable by position. (Assignment
    // is a body statement; top level is declarations only.)
    let (ast, pool) = lex_and_parse("fn main():\n\tp.0 = 2\n").unwrap();
    let def = fn_def(&ast, only_stmt(&ast));
    let target = match &ast.stmt(fn_body(&ast, def)[0]).kind {
        StmtKind::FieldAssign { target, .. } => *target,
        other => panic!("expected FieldAssign, got {other:?}"),
    };
    let (root, field) = match ast.expr(target).kind {
        ExprKind::FieldAccess { object, field } => (object, field),
        other => panic!("expected FieldAccess target, got {other:?}"),
    };
    assert_eq!(pool.str(field.name), "0");
    match ast.expr(root).kind {
        ExprKind::Ident(name) => assert_eq!(pool.str(name), "p"),
        other => panic!("expected Ident root, got {other:?}"),
    }

    // Leading zeros canonicalize exactly like expression access.
    let (ast, pool) = lex_and_parse("fn main():\n\tpair.007 = 1\n").unwrap();
    let def = fn_def(&ast, only_stmt(&ast));
    let target = match &ast.stmt(fn_body(&ast, def)[0]).kind {
        StmtKind::FieldAssign { target, .. } => *target,
        other => panic!("expected FieldAssign, got {other:?}"),
    };
    match ast.expr(target).kind {
        ExprKind::FieldAccess { field, .. } => assert_eq!(pool.str(field.name), "7"),
        other => panic!("expected FieldAccess target, got {other:?}"),
    }
}

#[test]
fn pos_field_borrow_target_parses() {
    // `&t.0` — the inout borrow target folds the positional hop INTO
    // the borrow (`&(t.0)`, never `(&t).0`).
    let (ast, pool) = lex_and_parse("x = f(&t.0)\n").unwrap();
    let init = decl_init(&ast);
    let arg = match ast.expr(init).kind {
        ExprKind::Call(_, args) => ast.expr_list(args)[0],
        other => panic!("expected Call, got {other:?}"),
    };
    let inner = match ast.expr(arg).kind {
        ExprKind::Borrow(inner) => inner,
        other => panic!("expected Borrow arg, got {other:?}"),
    };
    let (root, field) = match ast.expr(inner).kind {
        ExprKind::FieldAccess { object, field } => (object, field),
        other => panic!("expected &t.0 to borrow the field access, got {other:?}"),
    };
    assert_eq!(pool.str(field.name), "0");
    match ast.expr(root).kind {
        ExprKind::Ident(name) => assert_eq!(pool.str(name), "t"),
        other => panic!("expected Ident root, got {other:?}"),
    }
}
