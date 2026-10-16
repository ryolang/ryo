//! Match astgen tests (M12): `match` expressions lower to one UIR
//! `Match` instruction — the scrutinee ref, per-arm patterns
//! (variant/literal/wildcard shapes), and `Expr`/`Stmts` bodies all
//! decode through `Uir::match_view`. Split out of `astgen.rs` per the
//! file-length gate (R2), mirroring `astgen/enum_tests.rs`; the shared
//! lowering lives in `astgen/match_.rs`.

use super::tests::parse_and_lower;
use super::*;
use ryo_core::uir::{InstData, UirArgTarget, UirArmBodyView, UirFieldTarget, UirPatternView};

/// Every `Match` instruction in the program, in arena order.
fn match_insts(uir: &Uir) -> Vec<InstRef> {
    (1..uir.instructions.len())
        .map(|i| InstRef::from_raw(u32::try_from(i).expect("idx fits u32")))
        .filter(|&r| uir.inst(r).tag == InstTag::Match)
        .collect()
}

/// Decode the only `Match` inst `src` must lower to.
fn only_match_view(src: &str) -> (Uir, InternPool, InstRef) {
    let (uir, pool) = parse_and_lower(src).unwrap();
    let matches = match_insts(&uir);
    assert_eq!(matches.len(), 1, "expected exactly one Match inst");
    (uir, pool, matches[0])
}

#[test]
fn variant_and_wildcard_arms_lower_to_match() {
    let (uir, pool, m) = only_match_view("match value:\n\tOption.Some(v): v\n\t_: 0\n");
    let view = uir.match_view(m);

    match uir.inst(view.scrutinee).data {
        InstData::Var(name) => assert_eq!(pool.str(name), "value"),
        other => panic!("expected Var scrutinee, got {other:?}"),
    }
    assert_eq!(view.arms.len(), 2);

    let option = pool.find_str("Option").unwrap();
    let some = pool.find_str("Some").unwrap();
    let v = pool.find_str("v").unwrap();
    match &view.arms[0].pattern {
        UirPatternView::Variant {
            enum_name,
            variant,
            args,
        } => {
            assert_eq!(*enum_name, Some(option));
            assert_eq!(*variant, some);
            let args = args.as_ref().expect("Some(v) carries one positional arg");
            assert!(args.by_position);
            assert_eq!(args.len(), 1);
            assert_eq!(
                args.args().collect::<Vec<_>>(),
                [(None, UirArgTarget::Bind(v))]
            );
        }
        other => panic!("expected variant pattern, got {other:?}"),
    }
    match view.arms[0].body {
        UirArmBodyView::Expr(r) => match uir.inst(r).data {
            InstData::Var(name) => assert_eq!(pool.str(name), "v"),
            other => panic!("arm body should be the `v` read, got {other:?}"),
        },
        other => panic!("expected expr body, got {other:?}"),
    }

    assert_eq!(view.arms[1].pattern, UirPatternView::Wildcard);
    match view.arms[1].body {
        UirArmBodyView::Expr(r) => assert!(
            matches!(uir.inst(r).data, InstData::Int(0)),
            "wildcard body should be int 0, got {:?}",
            uir.inst(r).data
        ),
        other => panic!("expected expr body, got {other:?}"),
    }
}

#[test]
fn literal_arms_lower_with_typed_patterns() {
    let (uir, pool, m) =
        only_match_view("match n:\n\t42: \"a\"\n\t-1: \"b\"\n\t\"quit\": 0\n\ttrue: 1\n\t_: 2\n");
    let view = uir.match_view(m);
    assert_eq!(view.arms.len(), 5);

    let quit = pool.find_str("quit").unwrap();
    assert_eq!(
        view.arms[0].pattern,
        UirPatternView::Literal(ast::LitPattern::Int(42))
    );
    assert_eq!(
        view.arms[1].pattern,
        UirPatternView::Literal(ast::LitPattern::Int(-1))
    );
    assert_eq!(
        view.arms[2].pattern,
        UirPatternView::Literal(ast::LitPattern::Str(quit))
    );
    assert_eq!(
        view.arms[3].pattern,
        UirPatternView::Literal(ast::LitPattern::Bool(true))
    );
    assert_eq!(view.arms[4].pattern, UirPatternView::Wildcard);

    let bodies: Vec<InstRef> = view
        .arms
        .iter()
        .map(|arm| match arm.body {
            UirArmBodyView::Expr(r) => r,
            other => panic!("all arms here are expr bodies, got {other:?}"),
        })
        .collect();
    match uir.inst(bodies[0]).data {
        InstData::Str(s) => assert_eq!(pool.str(s), "a"),
        other => panic!("expected str body, got {other:?}"),
    }
    match uir.inst(bodies[1]).data {
        InstData::Str(s) => assert_eq!(pool.str(s), "b"),
        other => panic!("expected str body, got {other:?}"),
    }
    assert!(matches!(uir.inst(bodies[2]).data, InstData::Int(0)));
    assert!(matches!(uir.inst(bodies[3]).data, InstData::Int(1)));
    assert!(matches!(uir.inst(bodies[4]).data, InstData::Int(2)));
}

#[test]
fn unit_and_named_variant_arms_lower() {
    let (uir, pool, m) = only_match_view("match c:\n\tRed: 0\n\tCoords{x=px, y=_}: px\n");
    let view = uir.match_view(m);
    assert_eq!(view.arms.len(), 2);

    let red = pool.find_str("Red").unwrap();
    assert_eq!(
        view.arms[0].pattern,
        UirPatternView::Variant {
            enum_name: None,
            variant: red,
            args: None,
        }
    );

    let coords = pool.find_str("Coords").unwrap();
    let x = pool.find_str("x").unwrap();
    let y = pool.find_str("y").unwrap();
    let px = pool.find_str("px").unwrap();
    match &view.arms[1].pattern {
        UirPatternView::Variant {
            enum_name,
            variant,
            args,
        } => {
            assert_eq!(*enum_name, None, "unqualified spelling stays None");
            assert_eq!(*variant, coords);
            let args = args.as_ref().expect("brace args present");
            assert!(!args.by_position, "braced args are named");
            assert_eq!(args.len(), 2);
            assert_eq!(
                args.args().collect::<Vec<_>>(),
                [
                    (Some(x), UirArgTarget::Bind(px)),
                    (Some(y), UirArgTarget::Wildcard)
                ]
            );
        }
        other => panic!("expected variant pattern, got {other:?}"),
    }
}

#[test]
fn statement_arm_bodies_lower_to_stmt_lists() {
    let src = "fn f():\n\tmatch v:\n\t\tSome(a): return a\n\t\t_:\n\t\t\tb = 1\n\t\t\tb\n";
    let (uir, _pool, m) = only_match_view(src);
    let view = uir.match_view(m);
    assert_eq!(view.arms.len(), 2);

    // Single-line statement arm: one wrapped `return a` stmt.
    let stmts: Vec<InstRef> = view.arms[0].body.stmts().collect();
    assert_eq!(stmts.len(), 1);
    assert!(
        matches!(uir.inst(stmts[0]).tag, InstTag::Return),
        "single-line statement arm wraps the return"
    );

    // Block arm: both statements in order (VarDecl then ExprStmt).
    let stmts: Vec<InstRef> = view.arms[1].body.stmts().collect();
    assert_eq!(stmts.len(), 2);
    assert!(
        matches!(uir.inst(stmts[0]).tag, InstTag::AssignOrDecl),
        "function-position `b = 1` is an AssignOrDecl (VarDecl is top level only)"
    );
    assert!(
        matches!(uir.inst(stmts[1]).tag, InstTag::ExprStmt),
        "block arm second stmt is the ExprStmt"
    );
}

#[test]
fn anon_pattern_arm_lowers_field_targets() {
    let (uir, pool, m) = only_match_view("match p:\n\t{x=0, y=_}: 1\n\t_: 2\n");
    let view = uir.match_view(m);
    assert_eq!(view.arms.len(), 2);

    let x = pool.find_str("x").unwrap();
    let y = pool.find_str("y").unwrap();
    match &view.arms[0].pattern {
        UirPatternView::Anon { fields } => {
            assert!(!fields.positional, "brace spelling selects by field name");
            assert_eq!(fields.len(), 2);
            assert_eq!(
                fields.fields().collect::<Vec<_>>(),
                [
                    (x.raw(), UirFieldTarget::Literal(ast::LitPattern::Int(0))),
                    (y.raw(), UirFieldTarget::Wildcard),
                ]
            );
        }
        other => panic!("expected anon pattern, got {other:?}"),
    }
    assert_eq!(view.arms[1].pattern, UirPatternView::Wildcard);
}

#[test]
fn positional_tuple_arm_lowers_with_index_selectors() {
    // Tuple sugar `(0, _)`: selectors are element indices, not names.
    let (uir, _pool, m) = only_match_view("match t:\n\t(0, _): 1\n\t_: 2\n");
    let view = uir.match_view(m);
    assert_eq!(view.arms.len(), 2);

    match &view.arms[0].pattern {
        UirPatternView::Anon { fields } => {
            assert!(fields.positional, "paren spelling selects by index");
            assert_eq!(fields.len(), 2);
            assert_eq!(
                fields.fields().collect::<Vec<_>>(),
                [
                    (0, UirFieldTarget::Literal(ast::LitPattern::Int(0))),
                    (1, UirFieldTarget::Wildcard),
                ]
            );
        }
        other => panic!("expected positional pattern, got {other:?}"),
    }
}

#[test]
fn anon_pun_and_rename_fields_bind() {
    let (uir, pool, m) = only_match_view("match p:\n\t{q, x = quot}: q\n\t_: 0\n");
    let view = uir.match_view(m);
    let q = pool.find_str("q").unwrap();
    let x = pool.find_str("x").unwrap();
    let quot = pool.find_str("quot").unwrap();
    match &view.arms[0].pattern {
        UirPatternView::Anon { fields } => {
            assert!(!fields.positional);
            assert_eq!(
                fields.fields().collect::<Vec<_>>(),
                [
                    (q.raw(), UirFieldTarget::Bind(q)),
                    (x.raw(), UirFieldTarget::Bind(quot)),
                ]
            );
        }
        other => panic!("expected anon pattern, got {other:?}"),
    }
}

#[test]
fn nested_pattern_in_arm_is_diagnosed() {
    // Nested shapes inside tuple elements are M12.5-deferred: diagnose
    // and recover the element as a wildcard so the arm still lowers.
    let err = parse_and_lower("match t:\n\t(a, (b, c)): 1\n").unwrap_err();
    assert!(
        err.iter().any(|d| d.code == DiagCode::ParseError),
        "expected the nested-pattern diagnostic, got {err:?}"
    );
}
