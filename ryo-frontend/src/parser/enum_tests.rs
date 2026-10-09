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

#[test]
fn variant_construct_positional_with_nested_call_keeps_single_arg() {
    // I-205: a call inside a positional arg writes `expr_lists` while
    // the enclosing list is gathered. The sealed list must contain
    // exactly the written args — the nested call's own args must not
    // interleave into it.
    let (ast, pool) = lex_and_parse("Opt.Some(int_to_str(7))\n").unwrap();
    let c = variant_construct(&ast, expr_stmt(&ast, only_stmt(&ast)));
    assert_eq!(pool.str(c.enum_name.name), "Opt");
    let args = c.args.expect("construct carries args");
    let args = ast.expr_list(args.positional.expect("paren form is positional"));
    assert_eq!(
        args.len(),
        1,
        "one written arg; the nested call's args must not leak in"
    );
    // The single arg is the call node, and its own arg list is intact.
    match &ast.expr(args[0]).kind {
        ExprKind::Call(name, inner) => {
            assert_eq!(pool.str(*name), "int_to_str");
            let inner_args = ast.expr_list(*inner);
            assert_eq!(inner_args.len(), 1);
            assert!(matches!(
                ast.expr(inner_args[0]).kind,
                ExprKind::Literal(Literal::Int(7))
            ));
        }
        other => panic!("expected the arg to be the call node, got {other:?}"),
    }
}

#[test]
fn variant_construct_positional_with_nested_struct_literal_keeps_single_arg() {
    // The original I-205 repro shape: the positional arg is a struct
    // literal whose field value contains a call. The outer list must
    // seal at exactly one entry; the struct literal's init list is a
    // separate sealed range.
    let (ast, pool) = lex_and_parse("Note.Info(Msg{text=int_to_str(7)})\n").unwrap();
    let c = variant_construct(&ast, expr_stmt(&ast, only_stmt(&ast)));
    let args = c.args.expect("construct carries args");
    let args = ast.expr_list(args.positional.expect("paren form is positional"));
    assert_eq!(args.len(), 1, "one written arg, got {:?}", args);
    match &ast.expr(args[0]).kind {
        ExprKind::StructLiteral(lit) => {
            assert_eq!(pool.str(lit.name.expect("named literal").name), "Msg");
            let inits = ast.struct_field_inits(lit.fields);
            assert_eq!(inits.len(), 1);
            assert_eq!(pool.str(inits[0].0), "text");
            assert!(
                matches!(ast.expr(inits[0].1).kind, ExprKind::Call(_, _)),
                "field value is the call, not the interleaved int"
            );
        }
        other => panic!("expected the arg to be the struct literal, got {other:?}"),
    }
}

#[test]
fn variant_construct_named_with_nested_named_construct_keeps_written_pairs() {
    // I-205 (named arm): a nested named variant construction inside an
    // init value writes `struct_field_inits`; the sealed outer list
    // must contain exactly the written pairs, with the nested pairs on
    // their own sealed range.
    let (ast, pool) = lex_and_parse("Outer.Inner{value=Note.Info{text=int_to_str(7)}}\n").unwrap();
    let c = variant_construct(&ast, expr_stmt(&ast, only_stmt(&ast)));
    assert_eq!(pool.str(c.enum_name.name), "Outer");
    assert_eq!(pool.str(c.variant.name), "Inner");
    let args = c.args.expect("construct carries args");
    let named = args.named.expect("brace form is named");
    assert!(args.positional.is_none());
    let inits = ast.struct_field_inits(named);
    assert_eq!(
        inits.len(),
        1,
        "one written pair; nested init pairs must not leak in"
    );
    assert_eq!(pool.str(inits[0].0), "value");
    match &ast.expr(inits[0].1).kind {
        ExprKind::VariantConstruct(inner) => {
            assert_eq!(pool.str(inner.enum_name.name), "Note");
            let inner_args = inner.args.expect("inner construct carries args");
            let inner_inits = ast.struct_field_inits(inner_args.named.expect("brace form"));
            assert_eq!(inner_inits.len(), 1);
            assert_eq!(pool.str(inner_inits[0].0), "text");
        }
        other => panic!("expected the value to be the nested construct, got {other:?}"),
    }
}
#[test]
fn named_arg_in_parens_method_flavor() {
    // `f(a=b)`: a plain call's parens are positional — the method
    // flavor fires once, no names attached.
    let (ok, _ast, errs, _pool) = lex_and_parse_recovering("fn main():\n\tf(a=b)\n");
    assert!(ok, "recovery should still produce a partial program");
    assert_eq!(
        errs.len(),
        1,
        "expected exactly the named-arg diagnostic: {errs:?}"
    );
    match errs[0].reason() {
        RichReason::Custom(ParseDiag::NamedArgInParens { enum_name, variant }) => {
            assert!(
                enum_name.is_none() && variant.is_none(),
                "method flavor carries no names"
            );
        }
        other => panic!("expected NamedArgInParens, got {other:?}"),
    }
    assert_eq!(
        errs[0].reason().to_string(),
        "named arguments are not supported — pass arguments positionally"
    );
}

#[test]
fn named_arg_in_parens_enum_flavor() {
    // `Shape.Circle(radius=5.0)`: Circle is a tuple variant, so the
    // paren list is positional — the enum flavor names the variant
    // (and the enum), exactly once.
    let (ok, _ast, errs, pool) = lex_and_parse_recovering(
        "enum Shape:\n\tCircle(float)\nfn main():\n\tprint(Shape.Circle(radius=5.0))\n",
    );
    assert!(ok, "recovery should still produce a partial program");
    assert_eq!(
        errs.len(),
        1,
        "expected exactly the named-arg diagnostic: {errs:?}"
    );
    match errs[0].reason() {
        RichReason::Custom(ParseDiag::NamedArgInParens { enum_name, variant }) => {
            let (en, v) = (
                enum_name.expect("enum name"),
                variant.expect("variant name"),
            );
            assert_eq!(pool.str(en), "Shape");
            assert_eq!(pool.str(v), "Circle");
        }
        other => panic!("expected NamedArgInParens, got {other:?}"),
    }
    // The value survives as a positional argument: the recovered
    // construct is `Shape.Circle(5.0)`, a well-formed construct —
    // the positional spelling must of course still parse.
    lex_and_parse("enum Shape:\n\tCircle(float)\nfn main():\n\tx = Shape.Circle(5.0)\n")
        .expect("positional spelling must still parse");
}

#[test]
fn named_arg_recovery_leaves_equality_args_unaffected() {
    // `f(a == b)`: `==` is a single token, so the `Ident =` recovery
    // cannot fire; the argument parses as an ordinary comparison.
    let (_ast, _pool) = lex_and_parse("fn main():\n\tf(a == b)\n")
        .expect("equality inside an arg list must parse unchanged");
}

#[test]
fn missing_parens_construct_recovers_as_payload_arg() {
    // `x = Shape.Circle 5.0` (E0126): the trailing same-line
    // expression is recovered as the payload argument, so the
    // statement still declares `x` and exactly one diagnostic fires.
    let (ok, ast, errs, pool) = lex_and_parse_recovering(
        "enum Shape:\n\tCircle(float)\nfn main():\n\tx = Shape.Circle 5.0\n",
    );
    assert!(ok, "recovery should still produce a partial program");
    assert_eq!(
        errs.len(),
        1,
        "expected exactly the missing-parens diagnostic: {errs:?}"
    );
    match errs[0].reason() {
        RichReason::Custom(ParseDiag::MissingArgListOnVariant { enum_name, variant }) => {
            assert_eq!(pool.str(*enum_name), "Shape");
            assert_eq!(pool.str(*variant), "Circle");
        }
        other => panic!("expected MissingArgListOnVariant, got {other:?}"),
    }
    // The recovered initializer is `Shape.Circle(5.0)`.
    let stmts = ast.top_level_stmts();
    let main = fn_body(&ast, fn_def(&ast, stmts[1]));
    let value = match &ast.stmt(main[0]).kind {
        StmtKind::AssignOrDecl { value, .. } => *value,
        other => panic!("expected AssignOrDecl, got {other:?}"),
    };
    let c = variant_construct(&ast, value);
    let args = c.args.expect("construct carries args");
    let positional = ast.expr_list(args.positional.expect("paren form is positional"));
    assert_eq!(positional.len(), 1);
}

#[test]
fn positional_values_in_braces_recover_with_targeted_diag() {
    // `Shape.Rectangle{1.0, 2.0}` (E0127): values without `name =`
    // are recovered as positional arguments.
    let (ok, ast, errs, pool) = lex_and_parse_recovering(
        "enum Shape:\n\tRectangle(width: float, height: float)\nfn main():\n\tx = Shape.Rectangle{1.0, 2.0}\n",
    );
    assert!(ok, "recovery should still produce a partial program");
    assert_eq!(
        errs.len(),
        1,
        "expected exactly the brace-form diagnostic: {errs:?}"
    );
    match errs[0].reason() {
        RichReason::Custom(ParseDiag::PositionalArgsInBraces { enum_name }) => {
            assert_eq!(pool.str(*enum_name), "Shape");
        }
        other => panic!("expected PositionalArgsInBraces, got {other:?}"),
    }
    let stmts = ast.top_level_stmts();
    let main = fn_body(&ast, fn_def(&ast, stmts[1]));
    let value = match &ast.stmt(main[0]).kind {
        StmtKind::AssignOrDecl { value, .. } => *value,
        other => panic!("expected AssignOrDecl, got {other:?}"),
    };
    let c = variant_construct(&ast, value);
    let args = c.args.expect("construct carries args");
    let positional = ast.expr_list(args.positional.expect("recovered as positional"));
    assert_eq!(positional.len(), 2);
}

#[test]
fn comma_separated_variants_recover_the_line() {
    // `enum Color: Red, Green, Blue` (E0128): the comma form
    // recovers by declaring the variants anyway — one diagnostic,
    // three unit variants.
    let (ok, ast, errs, _pool) =
        lex_and_parse_recovering("enum Color: Red, Green, Blue\nfn main():\n\tpass_through = 1\n");
    assert!(ok, "recovery should still produce a partial program");
    assert_eq!(
        errs.len(),
        1,
        "expected exactly the comma-form diagnostic: {errs:?}"
    );
    assert!(
        matches!(
            errs[0].reason(),
            RichReason::Custom(ParseDiag::CommaSeparatedVariants)
        ),
        "expected CommaSeparatedVariants, got {:?}",
        errs[0].reason()
    );
    let def = enum_def(&ast, ast.top_level_stmts()[0]);
    let variants = ast.enum_variants(def.variants);
    assert_eq!(variants.len(), 3);
    assert!(variants.iter().all(|v| v.payload.kind == VariantKind::Unit));
}

#[test]
fn comma_variant_single_name_keeps_historical_parse() {
    // `enum Color: Red` — a single variant name on the header line
    // (no comma) is NOT the comma form; it keeps its historical
    // failing parse (empty body + astgen's empty-enum diagnostic).
    let (ok, _ast, errs, _pool) = lex_and_parse_recovering("enum Color: Red\n");
    assert!(ok, "recovery should still produce a partial program");
    assert!(
        errs.iter().all(|e| !matches!(
            e.reason(),
            RichReason::Custom(ParseDiag::CommaSeparatedVariants)
        )),
        "comma-form diagnostic must not fire without a comma: {errs:?}"
    );
}
