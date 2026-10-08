//! Enum parser tests (M11): `enum Name:` declarations with unit, tuple,
//! and named payloads; `EnumName.Variant(...)` / `EnumName.Variant{...}`
//! construction; the unit-variant field-access disambiguation; and the
//! attribute-placement widening (I-193 slice) with the `#[repr(C)]`
//! rejection. Split out of `parser/tests.rs` per the R2 ruling —
//! parser.rs has no headroom for inline tests; the shared parse
//! helpers live there as `pub(super)`.

use super::tests::*;
use super::*;
use chumsky::error::RichReason;

/// The `EnumDef` of a statement, panicking on any other kind.
fn enum_def(ast: &Ast, stmt: StmtId) -> &EnumDef {
    match &ast.stmt(stmt).kind {
        StmtKind::EnumDef(def) => def,
        other => panic!("expected EnumDef, got {other:?}"),
    }
}

/// The `ExprId` of an `ExprStmt`, panicking on any other kind.
fn expr_stmt(ast: &Ast, stmt: StmtId) -> ExprId {
    match &ast.stmt(stmt).kind {
        StmtKind::ExprStmt(e) => *e,
        other => panic!("expected ExprStmt, got {other:?}"),
    }
}

/// The `VariantConstruct` of an expression, panicking on any other kind.
fn variant_construct(ast: &Ast, expr: ExprId) -> &VariantConstruct {
    match &ast.expr(expr).kind {
        ExprKind::VariantConstruct(c) => c,
        other => panic!("expected VariantConstruct, got {other:?}"),
    }
}

/// The `FieldAccess` fields of an expression, panicking on any other kind.
fn field_access(ast: &Ast, expr: ExprId) -> (ExprId, Ident) {
    match ast.expr(expr).kind {
        ExprKind::FieldAccess { object, field } => (object, field),
        other => panic!("expected FieldAccess, got {other:?}"),
    }
}

/// The name of a plain `TypeExprKind::Name`, panicking otherwise.
fn type_name(texpr: &TypeExpr) -> StringId {
    match texpr.kind {
        TypeExprKind::Name {
            name,
            is_view: false,
        } => name,
        other => panic!("expected plain Name type expression, got {other:?}"),
    }
}

#[test]
fn enum_unit_variants_parse() {
    let (ast, pool) = lex_and_parse("enum Color:\n\tRed\n\tGreen\n").unwrap();
    let def = enum_def(&ast, only_stmt(&ast));
    assert_eq!(pool.str(def.name.name), "Color");
    assert!(!def.attrs.derive_eq);
    let variants = ast.enum_variants(def.variants);
    assert_eq!(variants.len(), 2);
    for (v, want) in variants.iter().zip(["Red", "Green"]) {
        assert_eq!(pool.str(v.name.name), want);
        assert_eq!(v.payload.kind, VariantKind::Unit);
        assert!(ast.struct_field_decls(v.payload.fields).is_empty());
    }
}

#[test]
fn enum_tuple_and_named_payloads_parse() {
    // Brace Law (D11): payload fields are declared with PARENS —
    // `Rectangle(width: float, ...)`; braces are construction-only.
    let (ast, pool) =
        lex_and_parse("enum Shape:\n\tCircle(float)\n\tRectangle(width: float, height: float)\n")
            .unwrap();
    let def = enum_def(&ast, only_stmt(&ast));
    let variants = ast.enum_variants(def.variants);
    assert_eq!(variants.len(), 2);

    let circle = &variants[0];
    assert_eq!(pool.str(circle.name.name), "Circle");
    assert_eq!(circle.payload.kind, VariantKind::Tuple);
    let circle_fields = ast.struct_field_decls(circle.payload.fields);
    assert_eq!(circle_fields.len(), 1);
    // Tuple payloads store bare types under synthesized "0", "1", …
    // names (the M10 tuple-sugar convention).
    assert_eq!(pool.str(circle_fields[0].0), "0");
    assert_eq!(pool.str(type_name(&circle_fields[0].1)), "float");

    let rect = &variants[1];
    assert_eq!(pool.str(rect.name.name), "Rectangle");
    assert_eq!(rect.payload.kind, VariantKind::Named);
    let rect_fields = ast.struct_field_decls(rect.payload.fields);
    assert_eq!(rect_fields.len(), 2);
    assert_eq!(pool.str(rect_fields[0].0), "width");
    assert_eq!(pool.str(type_name(&rect_fields[0].1)), "float");
    assert_eq!(pool.str(rect_fields[1].0), "height");
    assert_eq!(pool.str(type_name(&rect_fields[1].1)), "float");
}

#[test]
fn enum_tuple_payload_multi_field_synthesizes_indices() {
    let (ast, pool) = lex_and_parse("enum Pair:\n\tBoth(int, str)\n").unwrap();
    let def = enum_def(&ast, only_stmt(&ast));
    let variants = ast.enum_variants(def.variants);
    let fields = ast.struct_field_decls(variants[0].payload.fields);
    assert_eq!(variants[0].payload.kind, VariantKind::Tuple);
    assert_eq!(fields.len(), 2);
    assert_eq!(pool.str(fields[0].0), "0");
    assert_eq!(pool.str(type_name(&fields[0].1)), "int");
    assert_eq!(pool.str(fields[1].0), "1");
    assert_eq!(pool.str(type_name(&fields[1].1)), "str");
}

#[test]
fn variant_construct_positional_parses() {
    let (ast, pool) = lex_and_parse("Shape.Circle(5.0)\n").unwrap();
    let c = variant_construct(&ast, expr_stmt(&ast, only_stmt(&ast)));
    assert_eq!(pool.str(c.enum_name.name), "Shape");
    assert_eq!(pool.str(c.variant.name), "Circle");
    let args = c.args.expect("construct carries args");
    let positional = args.positional.expect("paren form is positional");
    assert!(args.named.is_none());
    let args = ast.expr_list(positional);
    assert_eq!(args.len(), 1);
    assert!(matches!(
        ast.expr(args[0]).kind,
        ExprKind::Literal(Literal::Float(v)) if v == 5.0
    ));
}

#[test]
fn variant_construct_named_parses() {
    let (ast, pool) = lex_and_parse("Shape.Rectangle{width=1.0}\n").unwrap();
    let c = variant_construct(&ast, expr_stmt(&ast, only_stmt(&ast)));
    assert_eq!(pool.str(c.enum_name.name), "Shape");
    assert_eq!(pool.str(c.variant.name), "Rectangle");
    let args = c.args.expect("construct carries args");
    let named = args.named.expect("brace form is named");
    assert!(args.positional.is_none());
    let inits = ast.struct_field_inits(named);
    assert_eq!(inits.len(), 1);
    assert_eq!(pool.str(inits[0].0), "width");
    assert!(matches!(
        ast.expr(inits[0].1).kind,
        ExprKind::Literal(Literal::Float(v)) if v == 1.0
    ));
}

#[test]
fn variant_construct_empty_positional_parses() {
    // `Name.Variant()` is a positional construct with an empty list;
    // arity is sema's job.
    let (ast, _) = lex_and_parse("Shape.Circle()\n").unwrap();
    let c = variant_construct(&ast, expr_stmt(&ast, only_stmt(&ast)));
    let args = c.args.expect("construct carries args");
    assert!(
        ast.expr_list(args.positional.expect("paren form"))
            .is_empty()
    );
    assert!(args.named.is_none());
}

#[test]
fn unit_variant_reference_stays_field_access() {
    // Disambiguation contract: bare `Ident.Ident` is an ordinary field
    // access; unit-variant meaning is sema's job (M11).
    let (ast, pool) = lex_and_parse("Color.Red\n").unwrap();
    let e = expr_stmt(&ast, only_stmt(&ast));
    let (object, field) = field_access(&ast, e);
    assert!(matches!(ast.expr(object).kind, ExprKind::Ident(_)));
    assert_eq!(pool.str(field.name), "Red");
}

#[test]
fn field_access_and_method_call_unaffected() {
    // `obj.field` keeps its field-access meaning …
    let (ast, pool) = lex_and_parse("obj.field\n").unwrap();
    let e = expr_stmt(&ast, only_stmt(&ast));
    let (_, field) = field_access(&ast, e);
    assert_eq!(pool.str(field.name), "field");

    // … and `obj.field()` keeps its method-call meaning — the variant
    // rule only claims uppercase-led receivers (the spec §1 PascalCase
    // type convention), so `s.len()`-style calls are untouched.
    let (ast, pool) = lex_and_parse("obj.field()\n").unwrap();
    let e = expr_stmt(&ast, only_stmt(&ast));
    match &ast.expr(e).kind {
        ExprKind::MethodCall { method, .. } => assert_eq!(pool.str(*method), "field"),
        other => panic!("expected MethodCall, got {other:?}"),
    }
}

#[test]
fn uppercase_receiver_with_parens_is_variant_construct() {
    // The mirror of the method-call case: an uppercase-led receiver
    // followed by parens can only be variant construction.
    let (ast, pool) = lex_and_parse("Url.parse(x)\n").unwrap();
    let c = variant_construct(&ast, expr_stmt(&ast, only_stmt(&ast)));
    assert_eq!(pool.str(c.enum_name.name), "Url");
    assert_eq!(pool.str(c.variant.name), "parse");
}

#[test]
fn lowercase_brace_after_field_access_stays_an_error() {
    // `obj.field{...}` does NOT become a construct: the brace form is
    // only claimed for uppercase-led receivers, so this stays the
    // historical field-access-then-garbage syntax error.
    let (ok, ast, errs, _pool) = lex_and_parse_recovering("obj.field{x=1}\n");
    assert!(ok, "recovery must produce a partial program");
    assert!(!errs.is_empty(), "expected a parse error, got none");
    assert!(
        matches!(ast.stmt(only_stmt(&ast)).kind, StmtKind::Error),
        "expected the broken line to recover to an Error statement"
    );
}

#[test]
fn mixed_payload_kinds_are_a_syntax_error() {
    // The first payload element decides the kind; mixing named and
    // tuple elements fails the line.
    let (_ok, _ast, errs, _pool) =
        lex_and_parse_recovering("enum Bad:\n\tCircle(float, radius: float)\n");
    assert!(
        errs.iter()
            .any(|e| { matches!(e.reason(), RichReason::ExpectedFound { .. }) }),
        "expected a generic parse error for the mixed payload, got: {errs:?}"
    );
}

#[test]
fn derive_eq_before_enum_parses() {
    // I-193 slice: attributes now accept enum definitions, not just
    // structs. `#[derive(Eq)]` carries over; `#[repr(C)]` does not
    // (Enums carry no repr bit).
    let (ast, _pool) = lex_and_parse("#[derive(Eq)]\nenum Color:\n\tRed\n").unwrap();
    let def = enum_def(&ast, only_stmt(&ast));
    assert!(def.attrs.derive_eq);
    assert_eq!(ast.enum_variants(def.variants).len(), 1);
}

#[test]
fn misplaced_attribute_before_fn_is_still_rejected() {
    // Widened placement still refuses functions; the E0108 note text
    // ("struct and enum definitions") is pinned end-to-end in
    // ryo-driver's pipeline tests.
    let (ok, _ast, errs, _pool) =
        lex_and_parse_recovering("#[derive(Eq)]\nfn f():\n\tpass_through = 1\n");
    assert!(ok, "recovery must produce a partial program");
    assert_eq!(
        errs.len(),
        1,
        "expected exactly the misplaced-attribute diagnostic: {errs:?}"
    );
    assert!(
        matches!(
            errs[0].reason(),
            RichReason::Custom(ParseDiag::MisplacedAttribute)
        ),
        "expected MisplacedAttribute, got {:?}",
        errs[0].reason()
    );
}

#[test]
fn repr_c_on_enum_is_rejected() {
    // M11 rejects `#[repr(C)]` on enums; the diagnostic names the
    // allowed attribute. The declaration still builds (recovery by
    // diagnostic) so the rest of the file parses.
    let (ok, ast, errs, _pool) = lex_and_parse_recovering("#[repr(C)]\nenum Color:\n\tRed\n");
    assert!(ok, "recovery must produce a partial program");
    assert_eq!(errs.len(), 1, "expected exactly one diagnostic: {errs:?}");
    match errs[0].reason() {
        RichReason::Custom(pd @ ParseDiag::ReprCOnEnum) => {
            assert!(
                pd.to_string().contains("derive(Eq)"),
                "diagnostic should name the allowed attribute, got: {pd}"
            );
        }
        other => panic!("expected ReprCOnEnum, got {other:?}"),
    }
    let def = enum_def(&ast, only_stmt(&ast));
    assert!(!def.attrs.derive_eq);
}

#[test]
fn repr_c_on_struct_still_parses() {
    // The enum rejection must not disturb the struct form.
    let (ast, _pool) = lex_and_parse("#[repr(C)]\nstruct Point:\n\tx: int\n").unwrap();
    match &ast.stmt(only_stmt(&ast)).kind {
        StmtKind::StructDef(def) => assert!(def.attrs.repr_c),
        other => panic!("expected StructDef, got {other:?}"),
    }
}
