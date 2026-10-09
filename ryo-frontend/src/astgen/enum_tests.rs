//! Enum astgen tests (M11): enum declarations lower to `UirEnumDecl`
//! with resolved payload types; cross-kind forward references resolve;
//! by-value cycles, empty enums, and duplicate variants are diagnosed;
//! variant construction lowers to `EnumLit` with declaration-order
//! field indices. Split out of `astgen.rs`'s inline tests: astgen.rs
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
    let v = uir.var_decl_view(uir.body_stmts(main)[0]);
    assert!(matches!(uir.inst(v.initializer).tag, InstTag::EnumLit));
    let lit = uir.enum_lit_view(v.initializer);
    assert_eq!(lit.ty, uir.enum_decls[0].ty);
    assert_eq!(lit.variant_index, 0, "Circle is the first declared variant");
    assert!(lit.positional, "parens form records the positional flag");
    let args: Vec<_> = lit.fields().collect();
    assert_eq!(args.len(), 1);
    assert_eq!(args[0].0, 0, "positional arg 0 maps to field idx 0");
    match uir.inst(args[0].1).data {
        InstData::Float(f) => assert_eq!(f, 5.0),
        other => panic!("expected Float arg, got {other:?}"),
    }
}

#[test]
fn variant_construct_named_canonicalizes_against_decl_order() {
    // Source order (height, width) is reversed against the payload
    // declaration (width, height): the EnumLit pairs stay in source
    // order but each carries its declaration-order field index.
    let src = "enum Shape:\n\tRectangle(width: float, height: float)\n\n\
               r = Shape.Rectangle{height=2.0, width=1.0}\n";
    let (uir, pool) = parse_and_lower(src).unwrap();
    let main = body_named(&uir, &pool, "main");
    let v = uir.var_decl_view(uir.body_stmts(main)[0]);
    let lit = uir.enum_lit_view(v.initializer);
    assert_eq!(lit.variant_index, 0);
    assert!(
        !lit.positional,
        "braces form leaves the positional flag clear"
    );
    let args: Vec<_> = lit.fields().collect();
    assert_eq!(args.len(), 2);
    assert_eq!(args[0].0, 1, "height is payload field 1");
    assert_eq!(args[1].0, 0, "width is payload field 0");
    match (uir.inst(args[0].1).data, uir.inst(args[1].1).data) {
        (InstData::Float(h), InstData::Float(w)) => {
            assert_eq!(h, 2.0);
            assert_eq!(w, 1.0);
        }
        other => panic!("expected Float args, got {other:?}"),
    }
}
