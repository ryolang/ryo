//! Struct parser tests: struct declarations with attributes, struct and
//! anonymous struct literals, positional field access, and field access
//! and assignment targets. Split out of `parser/tests.rs` to stay under
//! the file-length limit; the shared parse helpers live there as
//! `pub(super)`.

use super::tests::*;
use super::*;
use chumsky::error::RichReason;

#[test]
fn parse_struct_declaration() {
    let (ast, pool) = lex_and_parse("struct Point:\n\tx: float\n\ty: float\n").unwrap();
    let def = struct_def_stmt(&ast, only_stmt(&ast));
    assert_eq!(pool.str(def.name.name), "Point");
    let fields = ast.struct_field_decls(def.fields);
    assert_eq!(fields.len(), 2);
    assert_eq!(pool.str(fields[0].0), "x");
    assert_eq!(pool.str(type_name(&fields[0].1).0), "float");
    assert_eq!(pool.str(fields[1].0), "y");
}

#[test]
fn parse_struct_declaration_no_trailing_newline() {
    let (ast, pool) = lex_and_parse("struct Point:\n\tx: float").unwrap();
    let def = struct_def_stmt(&ast, only_stmt(&ast));
    assert_eq!(pool.str(def.name.name), "Point");
    assert_eq!(ast.struct_field_decls(def.fields).len(), 1);
}

#[test]
fn parse_struct_declaration_with_blank_lines() {
    let (ast, pool) = lex_and_parse("struct Point:\n\n\tx: float\n\n\ty: float\n").unwrap();
    let def = struct_def_stmt(&ast, only_stmt(&ast));
    assert_eq!(ast.struct_field_decls(def.fields).len(), 2);
    let fields = ast.struct_field_decls(def.fields);
    assert_eq!(pool.str(fields[0].0), "x");
    assert_eq!(pool.str(fields[1].0), "y");
}

#[test]
fn struct_declaration_alongside_fn_main() {
    // A struct declaration is a declaration, not a top-level
    // executable statement: it must coexist with an explicit main.
    let (ast, pool) =
        lex_and_parse("struct Point:\n\tx: float\nfn main():\n\tpass_through = 1\n").unwrap();
    assert_eq!(ast.top_level_stmts().len(), 2);
    let def = struct_def_stmt(&ast, ast.top_level_stmts()[0]);
    assert_eq!(pool.str(def.name.name), "Point");
}

#[test]
fn parse_struct_with_derive_eq_attribute() {
    let (ast, _pool) = lex_and_parse("#[derive(Eq)] struct P:\n\tx: int\n").unwrap();
    let def = struct_def_stmt(&ast, only_stmt(&ast));
    assert!(def.attrs.derive_eq);
    assert!(!def.attrs.repr_c);
}

#[test]
fn parse_struct_with_repr_c_attribute() {
    let (ast, _pool) = lex_and_parse("#[repr(C)] struct P:\n\tx: int\n").unwrap();
    let def = struct_def_stmt(&ast, only_stmt(&ast));
    assert!(def.attrs.repr_c);
    assert!(!def.attrs.derive_eq);
}

#[test]
fn parse_struct_with_stacked_attributes() {
    // Attribute groups stack on their own lines before `struct`.
    let (ast, _pool) = lex_and_parse("#[derive(Eq)]\n#[repr(C)]\nstruct P:\n\tx: int\n").unwrap();
    let def = struct_def_stmt(&ast, only_stmt(&ast));
    assert!(def.attrs.derive_eq);
    assert!(def.attrs.repr_c);
}

/// Assert the parse reports exactly one `UnknownAttribute` diagnostic
/// whose name/args resolve (through the pool) to the given spelling.
fn assert_unknown_attribute(errs: &[TestErr], pool: &InternPool, name: &str, args: &[&str]) {
    assert_eq!(errs.len(), 1, "expected one diagnostic: {errs:?}");
    match errs[0].reason() {
        RichReason::Custom(ParseDiag::UnknownAttribute {
            name: attr_name,
            args: attr_args,
        }) => {
            assert_eq!(pool.str(*attr_name), name);
            let got: Vec<&str> = attr_args.iter().map(|&a| pool.str(a)).collect();
            assert_eq!(got, args);
        }
        other => panic!("expected UnknownAttribute, got {other:?}"),
    }
}

#[test]
fn unknown_derive_argument_is_rejected() {
    let (_ok, _ast, errs, pool) =
        lex_and_parse_recovering("#[derive(Bogus)] struct P:\n\tx: int\n");
    assert_unknown_attribute(&errs, &pool, "derive", &["Bogus"]);
}

#[test]
fn unknown_attribute_name_is_rejected() {
    let (_ok, _ast, errs, pool) = lex_and_parse_recovering("#[bogus] struct P:\n\tx: int\n");
    assert_unknown_attribute(&errs, &pool, "bogus", &[]);
}

#[test]
fn derive_with_extra_arguments_is_rejected() {
    // Only `derive(Eq)` is known for now: a second argument turns the
    // whole group into the unknown-attribute diagnostic.
    let (_ok, _ast, errs, pool) =
        lex_and_parse_recovering("#[derive(Eq, Other)] struct P:\n\tx: int\n");
    assert_unknown_attribute(&errs, &pool, "derive", &["Eq", "Other"]);
}

#[test]
fn misplaced_attribute_before_fn_is_rejected() {
    // Attributes parsed but no `struct` follows: the fn still parses,
    // the attribute line recovers to an error statement.
    let (ok, ast, errs, _pool) =
        lex_and_parse_recovering("#[derive(Eq)]\nfn f():\n\tpass_through = 1\n");
    assert!(ok, "recovery must produce a partial program");
    assert_eq!(
        errs.len(),
        1,
        "expected exactly the misplaced-attribute diagnostic: {errs:?}"
    );
    match errs[0].reason() {
        RichReason::Custom(pd @ ParseDiag::MisplacedAttribute) => {
            // Headline only — the driver attaches the mandated
            // "attributes are only supported on struct and enum
            // definitions" sentence as a structured note (pinned
            // end-to-end in ryo-driver's pipeline tests).
            assert!(
                pd.to_string().contains("unexpected attribute"),
                "diagnostic should headline the misplaced attribute, got: {pd}"
            );
        }
        other => panic!("expected MisplacedAttribute, got {other:?}"),
    }
    assert_eq!(ast.top_level_stmts().len(), 2);
}

#[test]
fn empty_struct_body_is_rejected() {
    let (_ok, _ast, errs, _pool) =
        lex_and_parse_recovering("struct Empty:\nfn main():\n\tpass_through = 1\n");
    assert!(
        errs.iter()
            .any(|e| matches!(e.reason(), RichReason::Custom(ParseDiag::EmptyStructBody))),
        "expected an EmptyStructBody diagnostic, got: {errs:?}"
    );
}

#[test]
fn malformed_struct_body_does_not_report_empty_body() {
    // An indented but unparseable field line is a generic parse
    // error (recovered to an Error node), not "empty struct body".
    let (_ok, _ast, errs, _pool) = lex_and_parse_recovering("struct S:\n\tgarbage line\n");
    assert!(!errs.is_empty());
    assert!(
        !errs
            .iter()
            .any(|e| matches!(e.reason(), RichReason::Custom(ParseDiag::EmptyStructBody))),
        "malformed body must not report EmptyStructBody: {errs:?}"
    );
}

#[test]
fn parse_struct_literal() {
    let (ast, pool) = lex_and_parse("p = Point{x=1.0, y=2.0}\n").unwrap();
    let init = decl_init(&ast);
    match &ast.expr(init).kind {
        ExprKind::StructLiteral(lit) => {
            assert_eq!(pool.str(lit.name.unwrap().name), "Point");
            let fields = ast.struct_field_inits(lit.fields);
            assert_eq!(fields.len(), 2);
            assert_eq!(pool.str(fields[0].0), "x");
            assert_eq!(pool.str(fields[1].0), "y");
            // Field order is source order; sema canonicalizes.
            assert!(matches!(
                ast.expr(fields[0].1).kind,
                ExprKind::Literal(Literal::Float(_))
            ));
        }
        other => panic!("expected StructLiteral, got {:?}", other),
    }
}

#[test]
fn parse_struct_literal_trailing_comma() {
    let (ast, pool) = lex_and_parse("p = Point{x=1.0,}\n").unwrap();
    let init = decl_init(&ast);
    match &ast.expr(init).kind {
        ExprKind::StructLiteral(lit) => {
            assert_eq!(pool.str(lit.name.unwrap().name), "Point")
        }
        other => panic!("expected StructLiteral, got {:?}", other),
    }
}

#[test]
fn parse_struct_literal_empty_fields() {
    let (ast, pool) = lex_and_parse("p = Point{}\n").unwrap();
    let init = decl_init(&ast);
    match &ast.expr(init).kind {
        ExprKind::StructLiteral(lit) => {
            assert_eq!(pool.str(lit.name.unwrap().name), "Point");
            assert_eq!(ast.struct_field_inits(lit.fields).len(), 0);
        }
        other => panic!("expected StructLiteral, got {:?}", other),
    }
}

#[test]
fn struct_literal_does_not_shadow_call_or_ident() {
    // `call` keeps winning for `f(...)`; a bare ident stays an Ident.
    let (ast, _) = lex_and_parse("a = f(1)\nb = g\n").unwrap();
    let stmts = ast.top_level_stmts();
    let call_init = var_decl(&ast, stmts[0]).initializer;
    assert!(matches!(ast.expr(call_init).kind, ExprKind::Call(_, _)));
    let ident_init = var_decl(&ast, stmts[1]).initializer;
    assert!(matches!(ast.expr(ident_init).kind, ExprKind::Ident(_)));
}

#[test]
fn anon_struct_literal_parses() {
    let (ast, pool) = lex_and_parse("p = {x=1, y=2}\n").unwrap();
    let init = decl_init(&ast);
    match &ast.expr(init).kind {
        ExprKind::StructLiteral(lit) => {
            // No declared name — the shape IS the identity.
            assert!(lit.name.is_none());
            let fields = ast.struct_field_inits(lit.fields);
            assert_eq!(fields.len(), 2);
            assert_eq!(pool.str(fields[0].0), "x");
            assert_eq!(pool.str(fields[1].0), "y");
            assert_int_lit(&ast, fields[0].1, 1);
            assert_int_lit(&ast, fields[1].1, 2);
        }
        other => panic!("expected StructLiteral, got {:?}", other),
    }
}

#[test]
fn anon_literal_numeric_field_keys() {
    // Positional keys canonicalize to their numeric value, so the
    // brace spelling of a tuple — `{0=17, 1="alice"}` — names the
    // same fields as `(17, "alice")` sugar.
    let (ast, pool) = lex_and_parse("p = {0=17, 1=\"alice\"}\n").unwrap();
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
fn anon_literal_rejects_empty_braces() {
    // `{}` stays reserved for the future empty map literal.
    let errs = lex_and_parse("p = {}\n").expect_err("empty braces must be rejected");
    let msg = errs
        .iter()
        .find_map(|e| match e.reason() {
            RichReason::Custom(pd @ ParseDiag::EmptyAnonStruct) => Some(pd.to_string()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("expected an EmptyAnonStruct diagnostic, got: {errs:?}"));
    assert!(msg.contains("map literal"), "{msg}");
    assert!(msg.contains("none"), "{msg}");
    assert!(msg.contains("named struct"), "{msg}");
}

#[test]
fn positional_field_access_parses() {
    let (ast, pool) = lex_and_parse("x = pair.0\n").unwrap();
    let init = decl_init(&ast);
    match ast.expr(init).kind {
        ExprKind::FieldAccess { object, field } => {
            assert_eq!(pool.str(field.name), "0");
            assert!(matches!(ast.expr(object).kind, ExprKind::Ident(_)));
        }
        other => panic!("expected FieldAccess, got {:?}", other),
    }

    // A chained positional access needs the inner access
    // parenthesized — bare `pair.0.1` is the lexer's `0.1` float, so
    // `(pair.0).1` is the spellable form.
    let (ast, pool) = lex_and_parse("y = (pair.0).1\n").unwrap();
    let init = decl_init(&ast);
    match ast.expr(init).kind {
        ExprKind::FieldAccess { object, field } => {
            assert_eq!(pool.str(field.name), "1");
            match ast.expr(object).kind {
                ExprKind::FieldAccess {
                    object: inner,
                    field: inner_field,
                } => {
                    assert_eq!(pool.str(inner_field.name), "0");
                    assert!(matches!(ast.expr(inner).kind, ExprKind::Ident(_)));
                }
                other => panic!("expected nested FieldAccess, got {:?}", other),
            }
        }
        other => panic!("expected FieldAccess, got {:?}", other),
    }
}

#[test]
fn positional_access_canonicalizes_value() {
    // The field name is the numeric VALUE, not the literal text:
    // `pair.007` accesses field "7".
    let (ast, pool) = lex_and_parse("x = pair.007\n").unwrap();
    let init = decl_init(&ast);
    match ast.expr(init).kind {
        ExprKind::FieldAccess { field, .. } => assert_eq!(pool.str(field.name), "7"),
        other => panic!("expected FieldAccess, got {:?}", other),
    }
}

#[test]
fn bare_chained_positional_errors_with_paren_hint() {
    // Maximal munch: `pair.0.1` lexes as `pair . <float 0.1>` — the
    // float regex eats `0.1` whole, so the positional chain cannot
    // continue bare. The parser consumes the stray float, keeps the
    // well-formed receiver, and suggests the parenthesized spelling.
    let (ok, ast, errs, _pool) = lex_and_parse_recovering("x = pair.0.1\n");
    assert!(ok, "statement should recover to a partial program");
    let msg = errs
        .iter()
        .find_map(|e| match e.reason() {
            RichReason::Custom(pd @ ParseDiag::ChainedPositionalAccess) => Some(pd.to_string()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("expected a ChainedPositionalAccess diagnostic, got: {errs:?}"));
    assert!(msg.contains("(pair.0).1"), "{msg}");
    // The merged float consumed the whole `.0.1` tail, so the
    // recovered expression is the bare receiver.
    let init = decl_init(&ast);
    assert!(
        matches!(ast.expr(init).kind, ExprKind::Ident(_)),
        "expected the partial AST to keep the receiver, got {:?}",
        ast.expr(init).kind
    );
}

#[test]
fn parse_field_access() {
    let (ast, pool) = lex_and_parse("fn main():\n\ty = p.x\n").unwrap();
    let f = fn_def(&ast, only_stmt(&ast));
    let init = assign_value(&ast, fn_body(&ast, f)[0]);
    match ast.expr(init).kind {
        ExprKind::FieldAccess { object, field } => {
            assert_eq!(pool.str(field.name), "x");
            assert!(matches!(ast.expr(object).kind, ExprKind::Ident(_)));
        }
        other => panic!("expected FieldAccess, got {:?}", other),
    }
}

#[test]
fn parse_chained_field_access() {
    let (ast, pool) = lex_and_parse("fn main():\n\tv = a.b.c\n").unwrap();
    let f = fn_def(&ast, only_stmt(&ast));
    let init = assign_value(&ast, fn_body(&ast, f)[0]);
    // `a.b.c` parses as FieldAccess(FieldAccess(a, b), c).
    match ast.expr(init).kind {
        ExprKind::FieldAccess { object, field } => {
            assert_eq!(pool.str(field.name), "c");
            match ast.expr(object).kind {
                ExprKind::FieldAccess {
                    object: inner,
                    field: inner_field,
                } => {
                    assert_eq!(pool.str(inner_field.name), "b");
                    assert!(matches!(ast.expr(inner).kind, ExprKind::Ident(_)));
                }
                other => panic!("expected nested FieldAccess, got {:?}", other),
            }
        }
        other => panic!("expected FieldAccess, got {:?}", other),
    }
}

#[test]
fn method_call_still_parses_as_method_call() {
    // `method_op` wins over `field_op` when parens follow the name.
    let (ast, _) = lex_and_parse("fn main():\n\tn = s.len()\n").unwrap();
    let f = fn_def(&ast, only_stmt(&ast));
    let init = assign_value(&ast, fn_body(&ast, f)[0]);
    assert!(matches!(ast.expr(init).kind, ExprKind::MethodCall { .. }));
}
#[test]
fn parse_field_assignment() {
    let (ast, _pool) = lex_and_parse("fn main():\n\tmut p = Point{x=1.0}\n\tp.x = 2.0\n").unwrap();
    let body = fn_body(&ast, fn_def(&ast, only_stmt(&ast)));
    match &ast.stmt(body[1]).kind {
        StmtKind::FieldAssign { target, .. } => {
            assert!(matches!(
                ast.expr(*target).kind,
                ExprKind::FieldAccess { .. }
            ))
        }
        other => panic!("expected FieldAssign, got {:?}", other),
    }
}

#[test]
fn parse_compound_field_assignment() {
    let (ast, _pool) = lex_and_parse("fn main():\n\tmut p = Point{x=1.0}\n\tp.x += 2.0\n").unwrap();
    let body = fn_body(&ast, fn_def(&ast, only_stmt(&ast)));
    assert!(matches!(
        ast.stmt(body[1]).kind,
        StmtKind::CompoundFieldAssign { .. }
    ));
}

#[test]
fn parse_nested_field_assignment_chain() {
    let (ast, _pool) = lex_and_parse("fn main():\n\tmut a = X{b=Y{c=1}}\n\ta.b.c = 2\n").unwrap();
    let body = fn_body(&ast, fn_def(&ast, only_stmt(&ast)));
    match &ast.stmt(body[1]).kind {
        StmtKind::FieldAssign { target, .. } => match ast.expr(*target).kind {
            ExprKind::FieldAccess { object, .. } => {
                assert!(matches!(
                    ast.expr(object).kind,
                    ExprKind::FieldAccess { .. }
                ))
            }
            other => panic!("expected nested FieldAccess, got {:?}", other),
        },
        other => panic!("expected FieldAssign, got {:?}", other),
    }
}

#[test]
fn bare_ident_assignment_still_assign_or_decl() {
    let (ast, _pool) = lex_and_parse("fn main():\n\tx = 1\n").unwrap();
    let body = fn_body(&ast, fn_def(&ast, only_stmt(&ast)));
    assert!(matches!(
        ast.stmt(body[0]).kind,
        StmtKind::AssignOrDecl { .. }
    ));
}

#[test]
fn bare_ident_compound_assignment_still_compound_assign() {
    let (ast, _pool) = lex_and_parse("fn main():\n\tx = 1\n\tx += 2\n").unwrap();
    let body = fn_body(&ast, fn_def(&ast, only_stmt(&ast)));
    assert!(matches!(
        ast.stmt(body[1]).kind,
        StmtKind::CompoundAssign { .. }
    ));
}
