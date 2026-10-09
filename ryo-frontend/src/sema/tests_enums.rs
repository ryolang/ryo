//! M11 enum sema tests. Harness shared with `tests.rs` via its
//! `pub(super)` helpers (the 2000-line file cap keeps these out of
//! `tests.rs`).

use super::tests::*;
use super::*;
use ryo_core::tir::TirTag;
use ryo_core::types::TypeKind;

#[test]
fn variant_construct_all_three_shapes_type_checks() {
    // (a) unit (via bare `Color.Red` reinterpretation), tuple, and
    // named-field constructions all produce typed EnumLit instructions.
    let src = "enum Color:\n\tRed\n\tGreen\n\nenum Result:\n\tSuccess(int)\n\tError(message: str)\n\nenum Shape:\n\tCircle(float)\n\tRectangle(width: float, height: float)\n\nfn main():\n\tc = Color.Red\n\tr = Result.Success(5)\n\ts = Shape.Circle(5.0)\n\tt = Shape.Rectangle{width=1.0, height=2.0}\n";
    let (tirs, pool) = run(src).expect("sema ok");
    let main = tir_named(&tirs, &pool, "main");
    let expect = [("c", 0usize), ("r", 1), ("s", 1), ("t", 2)];
    for (i, (name, argc)) in expect.iter().enumerate() {
        let decl = main.var_decl_view(stmt_at(main, i));
        let view = main.enum_lit_view(decl.initializer);
        assert!(
            matches!(pool.kind(view.ty), TypeKind::Enum),
            "{name}: initializer must be enum-typed"
        );
        assert_eq!(view.fields().count(), *argc, "{name}: payload arg count");
    }
    // Named construction canonicalizes to declaration order.
    let decl = main.var_decl_view(stmt_at(main, 3));
    let indices: Vec<u32> = main
        .enum_lit_view(decl.initializer)
        .fields()
        .map(|(i, _)| i)
        .collect();
    assert_eq!(indices, vec![0, 1], "named args in declaration order");
}

#[test]
fn unit_variant_access_is_typed_enum_lit() {
    // (b) `Color.Red` parses as a bare FieldAccess; sema reinterprets an
    // unbound uppercase-led object ident as an enum type and the member
    // as a unit variant, lowering to a zero-arg EnumLit.
    let src = "enum Color:\n\tRed\n\tGreen\n\nfn main():\n\tc = Color.Red\n";
    let (tirs, pool) = run(src).expect("sema ok");
    let main = tir_named(&tirs, &pool, "main");
    let decl = main.var_decl_view(stmt_at(main, 0));
    let view = main.enum_lit_view(decl.initializer);
    assert!(matches!(pool.kind(view.ty), TypeKind::Enum));
    assert_eq!(view.variant_index, 0, "Red is the first variant");
    assert_eq!(view.fields().count(), 0, "unit variant carries no args");
    assert_eq!(
        pool.str(pool.enum_view(view.ty).name()),
        "Color",
        "EnumLit type is the Color enum"
    );
}

#[test]
fn variant_construct_payload_type_mismatch_names_variant_and_position() {
    // The wrong-typed positional payload diagnostic names the enum,
    // the variant, and the 1-based argument position — the user wrote
    // a positional argument, not the synthesized "0" field name.
    let src = "enum Shape:\n\tCircle(float)\n\nfn main():\n\ts = Shape.Circle(\"x\")\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::TypeMismatch)
        .expect("payload type mismatch must fire");
    for needle in ["enum 'Shape'", "variant 'Circle'", "positional argument 1"] {
        assert!(
            diag.message.contains(needle),
            "message should name {needle}, got {:?}",
            diag.message
        );
    }
}

#[test]
fn named_variant_positional_construct_is_rejected_with_brace_hint() {
    // Brace Law (D11): `Shape.Rectangle(1.0, 2.0)` — parens on a
    // named-field variant — is one diagnostic naming the enum, the
    // variant, and the brace construction spelling with the declared
    // field names. The in-order brace form stays legal (covered by
    // `variant_construct_all_three_shapes_type_checks`).
    let src = "enum Shape:\n\tRectangle(width: float, height: float)\n\nfn main():\n\ts = Shape.Rectangle(1.0, 2.0)\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::PositionalConstructOnNamedVariant)
        .expect("PositionalConstructOnNamedVariant must fire");
    for needle in [
        "enum 'Shape'",
        "variant 'Rectangle'",
        "Shape.Rectangle{width=..., height=...}",
    ] {
        assert!(
            diag.message.contains(needle),
            "message should name {needle}, got {:?}",
            diag.message
        );
    }
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
}

#[test]
fn unit_variant_access_unknown_variant_is_diag() {
    // `Shape.Hexagon` — bare variant access names a variant the enum
    // does not declare.
    let src = "enum Shape:\n\tCircle(float)\n\nfn main():\n\ts = Shape.Hexagon\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::UnknownVariant)
        .expect("UnknownVariant must fire");
    assert!(
        diag.message.contains("enum 'Shape'") && diag.message.contains("'Hexagon'"),
        "got {:?}",
        diag.message
    );
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
}

#[test]
fn named_construct_missing_field_names_variant_and_enum() {
    // `Shape.Rectangle{width=1.0}` omits `height`.
    let src = "enum Shape:\n\tRectangle(width: float, height: float)\n\nfn main():\n\ts = Shape.Rectangle{width=1.0}\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::MissingVariantFields)
        .expect("MissingVariantFields must fire");
    for needle in ["enum 'Shape'", "variant 'Rectangle'", "'height'"] {
        assert!(
            diag.message.contains(needle),
            "message should name {needle}, got {:?}",
            diag.message
        );
    }
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
}

#[test]
fn named_construct_duplicate_field_is_caught_by_sema() {
    // Duplicate named args lower to two idx-0 pairs with no astgen
    // diagnostic — sema must catch the repeated field.
    let src = "enum Shape:\n\tRectangle(width: float, height: float)\n\nfn main():\n\ts = Shape.Rectangle{width=1.0, width=2.0}\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::DuplicateVariantField)
        .expect("DuplicateVariantField must fire");
    for needle in ["enum 'Shape'", "variant 'Rectangle'", "'width'"] {
        assert!(
            diag.message.contains(needle),
            "message should name {needle}, got {:?}",
            diag.message
        );
    }
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
}

#[test]
fn named_construct_unknown_field_is_frontlined_by_astgen() {
    // A typo'd named field is diagnosed at astgen (UnknownField,
    // E0038) and the bad arg is dropped before sema runs; sema then
    // sees `width` uncovered and reports it missing. Both messages
    // together explain the typo.
    let src = "enum Shape:\n\tRectangle(width: float, height: float)\n\nfn main():\n\ts = Shape.Rectangle{widht=1.0, height=2.0}\n";
    let (_t, diags, _p) = run_with_errors(src);
    assert!(
        any_code(&diags, DiagCode::UnknownField),
        "astgen frontlines the unknown field name: {diags:?}"
    );
    let missing = diags
        .iter()
        .find(|d| d.code == DiagCode::MissingVariantFields)
        .expect("the skipped arg leaves 'width' uncovered");
    assert!(
        missing.message.contains("'width'"),
        "got {:?}",
        missing.message
    );
}

#[test]
fn positional_construct_overflow_is_unknown_variant_field() {
    // `Circle(5.0, 3.0)` on a one-field tuple variant: astgen pairs
    // positional args against declaration-order indices without an
    // arity check, so sema rejects the out-of-range index — the
    // UnknownVariantField shape astgen cannot see (named typos are
    // frontlined before this point).
    let src = "enum Shape:\n\tCircle(float)\n\nfn main():\n\ts = Shape.Circle(5.0, 3.0)\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::UnknownVariantField)
        .expect("UnknownVariantField must fire");
    for needle in ["enum 'Shape'", "variant 'Circle'", "'1'"] {
        assert!(
            diag.message.contains(needle),
            "message should name {needle}, got {:?}",
            diag.message
        );
    }
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
}

#[test]
fn derived_enum_equality_lowers_to_enum_eq_and_ne() {
    // (d) `==` / `!=` on a `#[derive(Eq)]` enum lower to EnumEq/EnumNe
    // with bool results, mirroring the StructEq/StructNe gate.
    let src = "#[derive(Eq)] enum R:\n\tS(int)\n\tE(str)\n\nfn main():\n\ta = R.S(1)\n\tb = R.S(2)\n\tc = a == b\n\td = R.E(\"x\")\n\te = R.E(\"y\")\n\tf = d != e\n";
    let (tirs, pool) = run(src).expect("sema ok");
    let main = tir_named(&tirs, &pool, "main");
    let eq = main
        .instructions
        .iter()
        .find(|i| i.tag == TirTag::EnumEq)
        .expect("a == b must lower to an EnumEq inst");
    assert_eq!(eq.ty, pool.bool_(), "EnumEq result must be bool");
    let ne = main
        .instructions
        .iter()
        .find(|i| i.tag == TirTag::EnumNe)
        .expect("d != e must lower to an EnumNe inst");
    assert_eq!(ne.ty, pool.bool_(), "EnumNe result must be bool");
}

#[test]
fn enum_equality_without_derive_requires_eq() {
    // (d) Non-derived enum compared with `==`: the struct-family
    // EqDeriveRequired diagnostic, verbatim message and fix-it note.
    let src = "enum C:\n\tR\n\tG\n\nfn main():\n\tc = C.R\n\td = C.G\n\te = c == d\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::EqDeriveRequired)
        .expect("EqDeriveRequired must fire");
    assert_eq!(diag.message, "binary operator `==` requires `C` to be `Eq`");
    assert!(
        diag.notes
            .iter()
            .any(|n| n.message == "help: add `#[derive(Eq)]` to `C`"),
        "expected the derive fix-it help note; got {:?}",
        diag.notes
    );
}

#[test]
fn print_enum_rewrites_to_debug_repr() {
    // (e) Debug is automatic for enums (no derive): print() admits any
    // enum value and rewrites to print(DebugRepr(arg)) at the TIR
    // level, exactly like the struct/scalar path.
    let src = "enum Color:\n\tRed\n\tGreen\n\nfn main():\n\tprint(Color.Red)\n";
    let (tirs, pool) = run(src).expect("sema ok");
    let main = tir_named(&tirs, &pool, "main");
    assert!(
        main.instructions.iter().any(|i| i.tag == TirTag::DebugRepr),
        "print(enum) must lower to a DebugRepr inst"
    );
}

#[test]
fn view_typed_payload_field_rejected() {
    // (f) Rule 6: enum payload fields must be owned values, exactly
    // like struct fields — register_enums emits ViewFieldType.
    let cases = [
        "enum E:\n\tV(v: strview)\n",
        "enum E:\n\tV(v: bytesview)\n",
        "enum E:\n\tA(x: int)\n\tB(v: strview)\n",
    ];
    for src in cases {
        let (_t, diags, _p) = run_with_errors(src);
        assert!(
            any_code(&diags, DiagCode::ViewFieldType),
            "{src:?}: got {diags:?}"
        );
    }
}

#[test]
fn construct_of_failed_enum_recovers_quietly() {
    // A declared-but-failed enum (E0005 InfiniteSize here) already
    // carries its own diagnostic; both construction forms must recover
    // quietly instead of piling on.
    let src = "enum L:\n\tN(l: L)\n\nfn main():\n\te = L.N\n";
    let (_t, diags, _p) = run_with_errors(src);
    assert_eq!(diags.len(), 1, "expected exactly one error: {diags:?}");
    assert_eq!(diags[0].code, DiagCode::InfiniteSize);
}

#[test]
fn construct_of_never_declared_enum_still_reports_unknown() {
    // Undeclared enum name in construction: astgen's UnknownType
    // (E0001) fires and sema adds nothing on top.
    let src = "fn main():\n\te = Nope.V(1)\n";
    let (_t, diags, _p) = run_with_errors(src);
    assert_eq!(diags.len(), 1, "expected exactly one error: {diags:?}");
    assert_eq!(diags[0].code, DiagCode::UnknownType);
}

#[test]
fn payload_variant_as_bare_value_is_type_mismatch() {
    // `Shape.Circle` names a tuple variant, which is a constructor
    // pattern, not a value — sema's judgment call: TypeMismatch
    // (mirroring "wrong thing in value position"), naming both.
    let src = "enum Shape:\n\tCircle(float)\n\nfn main():\n\ts = Shape.Circle\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::TypeMismatch)
        .expect("bare payload-variant access must error");
    assert!(
        diag.message.contains("enum 'Shape'") && diag.message.contains("'Circle'"),
        "got {:?}",
        diag.message
    );
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
}

#[test]
fn struct_name_in_variant_position_is_unknown_enum() {
    // `Point.Red` where Point is a struct: the enum-specific wording
    // beats the misleading "undefined variable: 'Point'".
    let src = "struct Point:\n\tx: int\n\nfn main():\n\tp = Point.Red\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::UnknownEnum)
        .expect("UnknownEnum must fire");
    assert!(diag.message.contains("'Point'"), "got {:?}", diag.message);
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
}

#[test]
fn lowercase_enum_construct_names_enum_and_points_at_constructor() {
    // `color.red(5)` parses as a method call — the variant-construct
    // rule only claims uppercase-led receivers — and `color` is an
    // enum type, not a variable. The enum wording beats the misleading
    // "undefined variable: 'color'" and points at the PascalCase
    // constructor spelling. (A shadowing variable still wins.)
    let src = "enum color:\n\tred(v: int)\n\nfn main():\n\tc = color.red(5)\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::UnknownEnum)
        .expect("UnknownEnum must fire");
    assert!(
        diag.message.contains("'color'") && diag.message.contains("enum"),
        "got {:?}",
        diag.message
    );
    let help = diag
        .notes
        .iter()
        .map(|n| n.message.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        help.contains("'Color'") && help.contains("'Color.red(...)'"),
        "got {:?}",
        help
    );
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
}
