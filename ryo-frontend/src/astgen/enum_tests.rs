//! Enum astgen tests (M11): enum declarations lower to `UirEnumDecl`
//! with resolved payload types; cross-kind forward references resolve;
//! by-value cycles, empty enums, and duplicate variants are diagnosed;
//! variant construction lowers to `EnumLit` with unresolved source names
//! and selectors. Split out of `astgen.rs`'s inline tests: astgen.rs
//! sits at the 2000-line CI cap, so new enum tests live here; the
//! shared lowering helpers remain there as `pub(super)`.

use super::tests::{body_named, parse_and_lower};
use super::*;
use ryo_core::types::VariantKind;
use ryo_core::uir::InstData;

#[test]
fn enum_decl_lowers_with_resolved_payload_types() {
    let src = "enum Shape:\n\tCircle(float)\n\tRectangle(width: float, height: float)\n";
    let (uir, pool) = parse_and_lower(src).unwrap();
    assert_eq!(uir.enum_decls.len(), 1);
    let decl = &uir.enum_decls[0];
    assert_eq!(pool.str(decl.name), "Shape");
    assert_eq!(decl.variants.len(), 2);

    let circle = &decl.variants[0];
    assert_eq!(pool.str(circle.name), "Circle");
    assert_eq!(circle.kind, VariantKind::Tuple);
    assert_eq!(circle.fields.len(), 1);
    // Tuple payloads carry the synthesized "0" name (M10 convention).
    assert_eq!(pool.str(circle.fields[0].name), "0");
    assert_eq!(circle.fields[0].ty, pool.float());

    let rect = &decl.variants[1];
    assert_eq!(pool.str(rect.name), "Rectangle");
    assert_eq!(rect.kind, VariantKind::Named);
    assert_eq!(rect.fields.len(), 2);
    assert_eq!(rect.fields[0].ty, pool.float());
    assert_eq!(rect.fields[1].ty, pool.float());

    // The nominal enum type is fully defined in the pool.
    assert_eq!(pool.enum_view(decl.ty).len(), 2);
}

#[test]
fn enum_payload_forward_struct_reference_resolves() {
    // Cross-kind forward reference: the pre-scan declares both kinds
    // before either is defined, so an enum payload may name a struct
    // declared below it.
    let src = "enum Wrapper:\n\tBox(p: Point)\n\nstruct Point:\n\tx: float\n";
    let (uir, pool) = parse_and_lower(src).unwrap();
    let wrapper = &uir.enum_decls[0];
    assert_eq!(wrapper.variants[0].fields[0].ty, uir.struct_decls[0].ty);
    assert!(pool.is_defined_struct(uir.struct_decls[0].ty));
    assert_eq!(pool.enum_view(wrapper.ty).len(), 1);
}

#[test]
fn struct_field_forward_enum_reference_resolves() {
    // The mirror direction: a struct field may name an enum declared
    // below it; one DFS orders the cross-kind definitions.
    let src = "struct Holder:\n\tc: Color\n\nenum Color:\n\tRed\n\tGreen\n";
    let (uir, pool) = parse_and_lower(src).unwrap();
    let holder = &uir.struct_decls[0];
    assert_eq!(holder.fields[0].ty, uir.enum_decls[0].ty);
    assert!(pool.is_defined_struct(holder.ty));
    assert_eq!(pool.enum_view(uir.enum_decls[0].ty).len(), 2);
}

#[test]
fn enum_value_cycle_is_diagnosed() {
    // `next: L` inside Cons is a by-value self-reference: declaring L
    // before defining it lets the reference resolve far enough to be
    // rejected, and the define DFS must reject it (InfiniteSize).
    let err = parse_and_lower("enum L:\n\tCons(v: int, next: L)\n\tNil\n").unwrap_err();
    assert!(err.iter().any(|d| d.code == DiagCode::InfiniteSize));
}

#[test]
fn cross_kind_value_cycle_is_diagnosed() {
    let err = parse_and_lower("struct A:\n\te: E\n\nenum E:\n\tAa(a: A)\n").unwrap_err();
    assert!(
        err.iter().any(|d| d.code == DiagCode::InfiniteSize),
        "struct↔enum by-value cycle must be InfiniteSize, got {err:?}"
    );
}

#[test]
fn failed_enum_inside_anon_field_type_is_absorbed() {
    // A failed enum referenced inside an anonymous struct field type
    // must absorb to the error type (the anon type's layout cannot
    // name the undefined enum) instead of crashing the interner.
    let err =
        parse_and_lower("enum L:\n\tCons(next: L)\n\tNil\n\nstruct S:\n\tx: {e: L}\n").unwrap_err();
    assert!(err.iter().any(|d| d.code == DiagCode::InfiniteSize));
}

#[test]
fn empty_enum_is_diagnosed() {
    let err = parse_and_lower("enum Empty:\n").unwrap_err();
    assert!(err.iter().any(|d| d.code == DiagCode::EmptyEnum));
}

#[test]
fn duplicate_variant_is_diagnosed() {
    let err = parse_and_lower("enum Color:\n\tRed\n\tRed\n").unwrap_err();
    assert!(
        err.iter().any(|d| d.code == DiagCode::DuplicateVariant),
        "expected DuplicateVariant, got {err:?}"
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
