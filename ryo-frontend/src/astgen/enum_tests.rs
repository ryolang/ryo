//! Enum astgen tests (M11): enum declarations lower to `UirEnumDecl`
//! with unresolved payloads; cross-kind references, cycles, empty enums,
//! and duplicate variants remain available for Sema;
//! variant construction lowers to `EnumLit` with unresolved source names
//! and selectors. Split out of `astgen.rs`'s inline tests: astgen.rs
//! sits at the 2000-line CI cap, so new enum tests live here; the
//! shared lowering helpers remain there as `pub(super)`.

use super::tests::{body_named, parse_and_lower};
use super::*;
use ryo_core::types::VariantKind;
use ryo_core::uir::InstData;

#[test]
fn enum_decl_lowers_with_unresolved_payload_types() {
    let src = "enum Shape:\n\tCircle(float)\n\tRectangle(width: float, height: float)\n";
    let (uir, pool) = parse_and_lower(src).unwrap();
    let decl = &uir.enum_decls[0];
    assert_eq!(pool.str(decl.name), "Shape");
    assert_eq!(decl.variants.len(), 2);
    let circle = &decl.variants[0];
    assert_eq!(pool.str(circle.name), "Circle");
    assert_eq!(circle.kind, VariantKind::Tuple);
    assert_eq!(pool.str(circle.fields[0].name), "0");
    for variant in &decl.variants {
        for field in &variant.fields {
            assert!(matches!(
                uir.type_expr(field.type_expr).kind,
                ryo_core::uir::UirTypeExprKind::Name { name, .. } if pool.str(name) == "float"
            ));
        }
    }
    assert_eq!(decl.variants[1].kind, VariantKind::Named);
}

#[test]
fn enum_payload_forward_struct_reference_is_preserved() {
    let src = "enum Wrapper:\n\tBox(p: Point)\n\nstruct Point:\n\tx: float\n";
    let (uir, pool) = parse_and_lower(src).unwrap();
    assert!(matches!(
        uir.type_expr(uir.enum_decls[0].variants[0].fields[0].type_expr).kind,
        ryo_core::uir::UirTypeExprKind::Name { name, .. } if pool.str(name) == "Point"
    ));
}

#[test]
fn struct_field_forward_enum_reference_is_preserved() {
    let src = "struct Holder:\n\tc: Color\n\nenum Color:\n\tRed\n\tGreen\n";
    let (uir, pool) = parse_and_lower(src).unwrap();
    assert!(matches!(
        uir.type_expr(uir.struct_decls[0].fields[0].type_expr).kind,
        ryo_core::uir::UirTypeExprKind::Name { name, .. } if pool.str(name) == "Color"
    ));
}

#[test]
fn enum_value_cycle_is_preserved_for_sema() {
    let (uir, _pool) = parse_and_lower("enum L:\n\tCons(v: int, next: L)\n\tNil\n")
        .expect("AstGen does not check type cycles");
    assert_eq!(uir.enum_decls[0].variants.len(), 2);
}

#[test]
fn cross_kind_value_cycle_is_preserved_for_sema() {
    let (uir, _pool) = parse_and_lower("struct A:\n\te: E\n\nenum E:\n\tAa(a: A)\n")
        .expect("AstGen does not check cross-kind cycles");
    assert_eq!(uir.struct_decls.len(), 1);
    assert_eq!(uir.enum_decls.len(), 1);
}

#[test]
fn recursive_enum_inside_anon_field_type_is_preserved() {
    let (uir, _pool) =
        parse_and_lower("enum L:\n\tCons(next: L)\n\tNil\n\nstruct S:\n\tx: {e: L}\n")
            .expect("AstGen preserves all nominal declarations");
    assert_eq!(uir.enum_decls.len(), 1);
    assert_eq!(uir.struct_decls.len(), 1);
}

#[test]
fn empty_enum_is_preserved_for_sema() {
    let (uir, _pool) =
        parse_and_lower("enum Empty:\n").expect("AstGen does not diagnose empty enums");
    assert!(uir.enum_decls[0].variants.is_empty());
}

#[test]
fn duplicate_variants_are_preserved_for_sema() {
    let (uir, pool) = parse_and_lower("enum Color:\n\tRed\n\tRed\n")
        .expect("duplicates remain available for Sema");
    assert_eq!(uir.enum_decls[0].variants.len(), 2);
    assert!(
        uir.enum_decls[0]
            .variants
            .iter()
            .all(|variant| pool.str(variant.name) == "Red")
    );
}

#[test]
fn variant_construct_positional_lowers_to_enum_lit() {
    let src = "enum Shape:\n\tCircle(float)\n\tRectangle(width: float, height: float)\n\n\
               s = Shape.Circle(5.0)\n";
    let (uir, pool) = parse_and_lower(src).unwrap();
    let main = body_named(&uir, &pool, "main");
    let v = uir.var_decl_view(uir.body_stmts(main).at(0));
    assert!(matches!(uir.inst(v.initializer).tag, InstTag::EnumLit));
    let lit = uir.enum_lit_view(v.initializer);
    assert_eq!(pool.str(lit.enum_name), "Shape");
    assert_eq!(pool.str(lit.variant), "Circle");
    assert!(lit.form.positional, "parens form records the source form");
    let args: Vec<_> = lit.fields().collect();
    assert_eq!(args.len(), 1);
    assert_eq!(args[0].0, 0, "the source argument keeps its position");
    match uir.inst(args[0].1).data {
        InstData::Float(f) => assert_eq!(f, 5.0),
        other => panic!("expected Float arg, got {other:?}"),
    }
}

#[test]
fn variant_construct_named_preserves_source_order_and_names() {
    let src = "enum Shape:\n\tRectangle(width: float, height: float)\n\n\
               r = Shape.Rectangle{height=2.0, width=1.0}\n";
    let (uir, pool) = parse_and_lower(src).unwrap();
    let main = body_named(&uir, &pool, "main");
    let v = uir.var_decl_view(uir.body_stmts(main).at(0));
    let lit = uir.enum_lit_view(v.initializer);
    assert_eq!(pool.str(lit.variant), "Rectangle");
    assert!(!lit.form.positional);
    let args: Vec<_> = lit.fields().collect();
    assert_eq!(args.len(), 2);
    assert_eq!(
        pool.str(ryo_core::types::StringId::from_raw(args[0].0)),
        "height"
    );
    assert_eq!(
        pool.str(ryo_core::types::StringId::from_raw(args[1].0)),
        "width"
    );
    match (uir.inst(args[0].1).data, uir.inst(args[1].1).data) {
        (InstData::Float(h), InstData::Float(w)) => {
            assert_eq!(h, 2.0);
            assert_eq!(w, 1.0);
        }
        other => panic!("expected Float args, got {other:?}"),
    }
}

#[test]
fn unknown_variant_construct_preserves_candidate_for_sema() {
    let input = "enum Shape:\n\tCircle(float)\n\ns = Shape.Hexagon(1.0)\n";
    let (uir, pool) = parse_and_lower(input).expect("AstGen performs no variant lookup");
    let main = body_named(&uir, &pool, "main");
    let v = uir.var_decl_view(uir.body_stmts(main).at(0));
    let lit = uir.enum_lit_view(v.initializer);
    assert_eq!(pool.str(lit.enum_name), "Shape");
    assert_eq!(pool.str(lit.variant), "Hexagon");
    assert_eq!(lit.fields().count(), 1);
}

#[test]
fn non_enum_uppercase_paren_receiver_preserves_candidate() {
    let src = "S = \"hi\"\nprint(S.len())\n";
    let (uir, pool) = parse_and_lower(src).unwrap();
    let idx = uir
        .instructions
        .iter()
        .position(|i| i.tag == InstTag::EnumLit)
        .expect("uppercase-led source remains an unresolved candidate");
    let view = uir.enum_lit_view(InstRef::from_raw(idx as u32));
    assert_eq!(pool.str(view.enum_name), "S");
    assert_eq!(pool.str(view.variant), "len");
    assert!(view.form.positional);
    assert!(view.fields().next().is_none());
}

#[test]
fn astgen_preserves_enum_error_and_match_declaration_names() {
    let src = "enum Shape:\n\tRectangle(width: float, height: float)\nerror Named(code: int, message: str)\nerrors IoError:\n\tEof\n\tReadFailed(str)\nfn main():\n\trect = Shape.Rectangle{height=2.0, width=1.0}\n\tnominal = Named(message=\"bad\", code=1)\n\tqualified = IoError.ReadFailed(\"bad\")\n\tmatch qualified:\n\t\tIoError.Eof: 0\n\t\t_: 1\n";
    let (uir, pool) = parse_and_lower(src).expect("structural lowering should succeed");
    let main = body_named(&uir, &pool, "main");
    let statements = uir.body_stmts(main);
    let initializer = |index| {
        let statement = statements.at(index);
        match uir.inst(statement).tag {
            InstTag::VarDecl => uir.var_decl_view(statement).initializer,
            InstTag::AssignOrDecl => uir.assign_or_decl_view(statement).value,
            other => panic!("expected a binding statement, got {other:?}"),
        }
    };

    let rect = uir.enum_lit_view(initializer(0));
    assert_eq!(pool.str(rect.enum_name), "Shape");
    assert_eq!(pool.str(rect.variant), "Rectangle");
    let form: ryo_core::uir::UirVariantForm = rect.form;
    assert!(!form.positional);
    assert!(!form.named_parens);
    assert!(!form.mixed_fields);
    let fields: Vec<_> = rect.fields().collect();
    assert_eq!(fields.len(), 2);
    assert_eq!(
        pool.str(ryo_core::types::StringId::from_raw(fields[0].0)),
        "height"
    );
    assert_eq!(
        pool.str(ryo_core::types::StringId::from_raw(fields[1].0)),
        "width"
    );

    let nominal = initializer(1);
    let (name, family_root, family_member, nominal_view) = uir.error_construct_view(nominal);
    assert_eq!(pool.str(name), "Named");
    assert_eq!(family_root, None);
    assert_eq!(family_member, None);
    assert!(nominal_view.form.named_parens);
    assert!(!nominal_view.form.positional);
    let fields: Vec<_> = nominal_view.fields().collect();
    assert_eq!(
        pool.str(ryo_core::types::StringId::from_raw(fields[0].0)),
        "message"
    );
    assert_eq!(
        pool.str(ryo_core::types::StringId::from_raw(fields[1].0)),
        "code"
    );

    let qualified_view = uir.enum_lit_view(initializer(2));
    assert_eq!(pool.str(qualified_view.enum_name), "IoError");
    assert_eq!(pool.str(qualified_view.variant), "ReadFailed");
    assert!(qualified_view.form.positional);

    let matched = match uir.inst(statements.at(3)).data {
        InstData::UnOp(operand) => operand,
        other => panic!("match statement should wrap its expression, got {other:?}"),
    };
    let matched = uir.match_view(matched);
    assert_eq!(
        matched.arms[0].pattern,
        ryo_core::uir::UirPatternView::Variant {
            enum_name: pool.find_str("IoError"),
            variant: pool.find_str("Eof").expect("member name"),
            args: None,
        }
    );

    let map = uir.compute_body_map().expect("argument ownership");
    for value in rect
        .fields()
        .map(|(_, value)| value)
        .chain(nominal_view.fields().map(|(_, value)| value))
        .chain(qualified_view.fields().map(|(_, value)| value))
    {
        assert!(
            map.slot(value).is_some(),
            "argument %{} is orphaned",
            value.raw()
        );
    }
}
