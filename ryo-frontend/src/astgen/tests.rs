//! Shared astgen lowering test harness: `parse_and_lower` runs the
//! lex-parse-lower pipeline and returns the UIR (or the diagnostics),
//! and `body_named` looks a function body up by name through the pool.
//! Split out of `astgen.rs`'s inline tests; feature-specific lowering
//! tests live in `enum_tests.rs`, `match_tests.rs`, and
//! `type_literal_tests`.

use super::*;
use crate::lexer::lex;
use crate::parser::{ParseState, program_parser};
use chumsky::Parser;
use chumsky::input::Input;
use ryo_core::uir::InstData;

pub(super) fn parse_and_lower(input: &str) -> Result<(Uir, InternPool), Vec<Diag>> {
    // Phase-2 lex pipeline: logos + indent + intern in one
    // pass; identifiers come back as `StringId`. Lex diagnostics
    // go through `DiagSink` like every other stage, so assert
    // the sink stayed clean instead of unwrapping a `Result`.
    let mut pool = InternPool::new();
    let mut lex_sink = DiagSink::new();
    let tokens = lex(input, &mut pool, &mut lex_sink);
    assert!(
        !lex_sink.has_errors(),
        "lex errors: {:?}",
        lex_sink.into_diags()
    );
    let token_stream = tokens[..].split_token_span((0..input.len()).into());
    let mut state = ParseState::new(pool);
    program_parser()
        .parse_with_state(token_stream, &mut state)
        .into_result()
        .expect("parse ok");
    let (ast, mut pool) = state.into_parts();

    let mut sink = DiagSink::new();
    let uir = generate(&ast, &mut pool, &mut sink);
    if sink.has_errors() {
        Err(sink.into_diags())
    } else {
        Ok((uir, pool))
    }
}

/// Find a function body by name through the `InternPool`.
pub(super) fn body_named<'a>(
    uir: &'a Uir,
    pool: &InternPool,
    name: &str,
) -> &'a ryo_core::uir::FuncBody {
    let id = pool.find_str(name).expect("name should be interned");
    uir.func_bodies
        .iter()
        .find(|f| f.name == id)
        .unwrap_or_else(|| panic!("no function named {:?}", name))
}

/// Top-level statement at index `i` in `body`'s execution order.
fn stmt_at(uir: &Uir, body: &ryo_core::uir::FuncBody, i: usize) -> InstRef {
    uir.body_stmts(body).at(i)
}

#[test]
fn lower_float_literal() {
    let (uir, pool) = parse_and_lower("x = 1.5").unwrap();
    let main = body_named(&uir, &pool, "main");
    let stmts = uir.body_stmts(main);
    let v = uir.var_decl_view(stmts.at(0));
    assert!(matches!(uir.inst(v.initializer).tag, InstTag::FloatLiteral));
}

#[test]
fn astgen_lowers_borrow() {
    let (uir, pool) = parse_and_lower("fn main():\n\tmut c = 0\n\tf(&c)\n").unwrap();
    let main = body_named(&uir, &pool, "main");
    // body is [VarDecl(c), ExprStmt(Call)]; the call's first arg
    // should be a Borrow inst whose operand is the Var(c) read.
    let stmts = uir.body_stmts(main);
    let expr_stmt = uir.inst(stmts.at(1));
    let call_ref = match expr_stmt.data {
        InstData::UnOp(o) => o,
        _ => panic!("expected ExprStmt to carry UnOp"),
    };
    let view = uir.call_view(call_ref);
    assert_eq!(view.args.len(), 1);
    let arg = view.args.at(0);
    assert!(
        matches!(uir.inst(arg).tag, InstTag::Borrow),
        "call arg should lower to a Borrow inst"
    );
    let inner = match uir.inst(arg).data {
        InstData::Borrow(i) => i,
        _ => panic!("expected Borrow data"),
    };
    assert!(
        matches!(uir.inst(inner).tag, InstTag::Var),
        "borrow operand should be a Var read"
    );
}

#[test]
fn nested_destructure_reads_temp_as_tempvar() {
    // The sub-pattern's value read must carry the TempVar tag —
    // the provenance that keeps user `Var` references away from
    // the compiler temp table (they reject the `__ryo_` prefix
    // instead of resolving through it).
    let (uir, pool) = parse_and_lower("fn main():\n\t(a, (b, c)) = (1, (2, 3))\n").unwrap();
    let main = body_named(&uir, &pool, "main");
    let stmts = uir.body_stmts(main);
    assert_eq!(
        stmts.len(),
        2,
        "nested lowering emits the sub-pattern as its own statement"
    );
    let sub = uir.destructure_view(stmts.at(1));
    let value = uir.inst(sub.value);
    assert!(
        matches!(value.tag, InstTag::TempVar),
        "sub-pattern value must be a TempVar read, got {:?}",
        value.tag
    );
    match value.data {
        InstData::Var(name) => assert_eq!(pool.str(name), "__ryo_destructure_0"),
        other => panic!("TempVar must carry InstData::Var, got {other:?}"),
    }
}

#[test]
fn lower_float_type_annotation() {
    let (uir, pool) = parse_and_lower("x: float = 1.5")
        .expect("`: float` annotation should resolve without diagnostics");
    let main = body_named(&uir, &pool, "main");
    let v = uir.var_decl_view(uir.body_stmts(main).at(0));
    assert_eq!(
        v.ty,
        Some(pool.float()),
        "`: float` annotation must resolve to pool.float()"
    );
}

#[test]
fn lower_ordering_and_modulo_ops() {
    let (uir, pool) = parse_and_lower("x = 1 < 2\ny = 1 % 2").unwrap();
    let main = body_named(&uir, &pool, "main");
    let stmts = uir.body_stmts(main);
    let lt = uir.var_decl_view(stmts.at(0));
    assert!(matches!(uir.inst(lt.initializer).tag, InstTag::Lt));
    let m = uir.var_decl_view(stmts.at(1));
    assert!(matches!(uir.inst(m.initializer).tag, InstTag::Mod));
}

#[test]
fn astgen_produces_no_types() {
    // The whole point of UIR-as-input-to-sema: astgen attaches
    // no types to instructions; the resolved-type table is
    // sema's job.
    let (uir, _) = parse_and_lower("x = 2 + 3 * 4\ny = x").unwrap();
    // No per-instruction `ty` slot exists on UIR; the test is
    // structural — every value-producing inst should have a
    // tag from the `value` half of `InstTag`, and no side
    // table is constructed yet.
    for inst in uir.instructions.iter().skip(1) {
        // Instructions can be either statements or expressions;
        // both shapes are valid here. The point is purely that
        // *no `Option<TypeId>` slot is present on the inst*,
        // which is enforced by the type itself.
        let _ = inst.tag;
    }
}

#[test]
fn structural_shape_flat_integer_variable() {
    let (uir, pool) = parse_and_lower("x = 42").unwrap();
    assert_eq!(uir.func_bodies.len(), 1);
    let main = body_named(&uir, &pool, "main");
    assert_eq!(main.params.len(), 0);
    // Implicit-main is now void; codegen falls through to an
    // implicit `return 0` for the C-ABI shim, so the synthetic
    // Return is no longer materialised in the UIR body.
    assert_eq!(main.return_type, pool.void());

    let stmts = uir.body_stmts(main);
    assert_eq!(stmts.len(), 1);

    let v = uir.var_decl_view(stmts.at(0));
    assert_eq!(pool.str(v.name), "x");
    assert!(!v.mutable);
    assert!(matches!(uir.inst(v.initializer).tag, InstTag::IntLiteral));
}

#[test]
fn structural_shape_mutable_variable() {
    let (uir, _) = parse_and_lower("mut x = 42").unwrap();
    let v = uir.var_decl_view(stmt_at(&uir, &uir.func_bodies[0], 0));
    assert!(v.mutable);
}

#[test]
fn structural_shape_binary_op() {
    let (uir, _) = parse_and_lower("x = 2 + 3 * 4").unwrap();
    let v = uir.var_decl_view(stmt_at(&uir, &uir.func_bodies[0], 0));
    // Initializer is `(2) + (3 * 4)` → outer Add, inner Mul.
    assert!(matches!(uir.inst(v.initializer).tag, InstTag::Add));
}

#[test]
fn structural_shape_negation() {
    let (uir, _) = parse_and_lower("x = -42").unwrap();
    let v = uir.var_decl_view(stmt_at(&uir, &uir.func_bodies[0], 0));
    assert!(matches!(uir.inst(v.initializer).tag, InstTag::Neg));
}

#[test]
fn explicit_main_with_top_level_error() {
    let diags = parse_and_lower("x = 42\n\nfn main():\n\tprint(\"hi\")\n").unwrap_err();
    assert!(
        diags
            .iter()
            .any(|d| d.code == DiagCode::TopLevelWithExplicitMain)
    );
}

#[test]
fn explicit_main_structural() {
    let (uir, pool) = parse_and_lower("fn main():\n\tprint(\"hi\")\n").unwrap();
    assert_eq!(uir.func_bodies.len(), 1);
    let main = body_named(&uir, &pool, "main");
    assert_eq!(main.return_type, pool.void());
    let stmts = uir.body_stmts(main);
    assert_eq!(stmts.len(), 1);
    assert!(matches!(uir.inst(stmts.at(0)).tag, InstTag::ExprStmt));
}

#[test]
fn main_with_return_type_emits_diag() {
    let diags = parse_and_lower("fn main() -> int:\n\treturn 0\n").unwrap_err();
    assert!(diags.iter().any(|d| d.code == DiagCode::MainSignature));
}

#[test]
fn main_with_params_emits_diag() {
    let diags = parse_and_lower("fn main(x: int):\n\tprint(\"hi\")\n").unwrap_err();
    assert!(diags.iter().any(|d| d.code == DiagCode::MainSignature));
}

#[test]
fn unknown_type_annotation_rejected() {
    let diags = parse_and_lower("x: nope = 1").unwrap_err();
    assert!(diags.iter().any(|d| d.code == DiagCode::UnknownType));
}

#[test]
fn helper_fn_with_top_level_lowers_both() {
    let (uir, pool) =
        parse_and_lower("fn helper() -> int:\n\treturn 42\n\nx = helper()\n").unwrap();
    assert_eq!(uir.func_bodies.len(), 2);
    assert!(uir.func_bodies.iter().any(|f| pool.str(f.name) == "helper"));
    assert!(uir.func_bodies.iter().any(|f| pool.str(f.name) == "main"));
}

#[test]
fn two_functions_structural() {
    let code = "fn add(a: int, b: int) -> int:\n\treturn a + b\n\nfn main():\n\tprint(\"hi\")\n";
    let (uir, pool) = parse_and_lower(code).unwrap();
    assert_eq!(uir.func_bodies.len(), 2);
    let add = body_named(&uir, &pool, "add");
    assert_eq!(add.params.len(), 2);
    assert_eq!(pool.str(add.params[0].name), "a");
    assert_eq!(pool.str(add.params[1].name), "b");
}

#[test]
fn call_payload_round_trips_through_extra() {
    // Implicit main contains `x = add(1, 2)`. The Call's
    // arglist comes back through `extra` correctly.
    let (uir, pool) =
        parse_and_lower("fn add(a: int, b: int) -> int:\n\treturn a + b\n\nx = add(1, 2)\n")
            .unwrap();
    let main = body_named(&uir, &pool, "main");
    let v = uir.var_decl_view(stmt_at(&uir, main, 0));
    assert!(matches!(uir.inst(v.initializer).tag, InstTag::Call));
    let call = uir.call_view(v.initializer);
    assert_eq!(pool.str(call.name), "add");
    assert_eq!(call.args.len(), 2);
    // Args are `IntLiteral(1)`, `IntLiteral(2)`.
    for (arg, expected) in call.args.iter().zip([1i64, 2]) {
        match uir.inst(arg).data {
            InstData::Int(v) => assert_eq!(v, expected),
            other => panic!("expected IntLiteral, got {:?}", other),
        }
    }
}

#[test]
fn lower_and_operator() {
    let (uir, pool) = parse_and_lower("x = true and false").unwrap();
    let main = body_named(&uir, &pool, "main");
    let v = uir.var_decl_view(uir.body_stmts(main).at(0));
    assert!(matches!(uir.inst(v.initializer).tag, InstTag::And));
}

#[test]
fn lower_or_operator() {
    let (uir, pool) = parse_and_lower("x = true or false").unwrap();
    let main = body_named(&uir, &pool, "main");
    let v = uir.var_decl_view(uir.body_stmts(main).at(0));
    assert!(matches!(uir.inst(v.initializer).tag, InstTag::Or));
}

#[test]
fn lower_not_operator() {
    let (uir, pool) = parse_and_lower("x = not true").unwrap();
    let main = body_named(&uir, &pool, "main");
    let v = uir.var_decl_view(uir.body_stmts(main).at(0));
    assert!(matches!(uir.inst(v.initializer).tag, InstTag::Not));
}

#[test]
fn lower_if_stmt() {
    let code = "fn main():\n\tif true:\n\t\tx = 1\n\telse:\n\t\tx = 2\n";
    let (uir, pool) = parse_and_lower(code).unwrap();
    let main = body_named(&uir, &pool, "main");
    let stmts = uir.body_stmts(main);
    assert_eq!(stmts.len(), 1);
    assert!(matches!(uir.inst(stmts.at(0)).tag, InstTag::IfStmt));
    let view = uir.if_stmt_view(stmts.at(0));
    assert_eq!(view.then_stmts.len(), 1);
    assert!(view.else_stmts.is_some());
}

#[test]
fn lower_assign_or_decl() {
    let (uir, pool) = parse_and_lower("fn main():\n\tx = 42\n").unwrap();
    let main = body_named(&uir, &pool, "main");
    let stmts = uir.body_stmts(main);
    assert_eq!(stmts.len(), 1);
    assert!(matches!(
        uir.inst(stmts.at(0)).tag,
        ryo_core::uir::InstTag::AssignOrDecl
    ));
    let v = uir.assign_or_decl_view(stmts.at(0));
    assert_eq!(pool.str(v.name), "x");
}

#[test]
fn lower_while_loop() {
    let (uir, pool) = parse_and_lower("fn main():\n\twhile true:\n\t\tbreak\n").unwrap();
    let dump = format!("{}", uir.dump(&pool));
    assert!(dump.contains("while_loop"), "got:\n{}", dump);
}

#[test]
fn lower_break() {
    let (uir, pool) = parse_and_lower("fn main():\n\twhile true:\n\t\tbreak\n").unwrap();
    let dump = format!("{}", uir.dump(&pool));
    assert!(dump.contains("break"), "got:\n{}", dump);
}

#[test]
fn lower_continue() {
    let (uir, pool) = parse_and_lower("fn main():\n\twhile true:\n\t\tcontinue\n").unwrap();
    let dump = format!("{}", uir.dump(&pool));
    assert!(dump.contains("continue"), "got:\n{}", dump);
}

#[test]
fn lower_compound_assign() {
    let (uir, pool) = parse_and_lower("fn main():\n\tmut x = 10\n\tx += 1\n").unwrap();
    let main = body_named(&uir, &pool, "main");
    let stmts = uir.body_stmts(main);
    assert_eq!(stmts.len(), 2);
    assert!(matches!(
        uir.inst(stmts.at(1)).tag,
        ryo_core::uir::InstTag::CompoundAssign
    ));
    let v = uir.compound_assign_view(stmts.at(1));
    assert_eq!(pool.str(v.name), "x");
    assert_eq!(v.op, ryo_core::ast::CompoundOp::Add);
}

#[test]
fn lower_for_range() {
    let (uir, pool) =
        parse_and_lower("fn main():\n\tfor i in range(0, 10):\n\t\tprint(i)\n").unwrap();
    let dump = format!("{}", uir.dump(&pool));
    assert!(dump.contains("for_range"), "got:\n{}", dump);
}

#[test]
fn for_non_range_iterator_rejected() {
    let result = parse_and_lower("fn main():\n\tfor i in something(0, 10):\n\t\tprint(i)\n");
    assert!(result.is_err(), "non-range iterator should be rejected");
}

#[test]
fn lower_slice_expression() {
    // `s` is intentionally undefined — astgen is a purely
    // structural pass; name resolution is sema's job. (Final
    // spec §3: `base[start:end]` lowers to a Slice projection.)
    let (uir, _pool) = parse_and_lower("fn main():\n\tx = s[1:2]\n").unwrap();
    let has_slice = uir.instructions.iter().any(|i| i.tag == InstTag::Slice);
    assert!(has_slice, "expected a Slice instruction");
}

#[test]
fn strview_param_resolves_to_view() {
    let (uir, pool) = parse_and_lower("fn f(text: strview):\n\tprint(text)\n").unwrap();
    assert_eq!(uir.func_bodies[0].params[0].ty, pool.str_view());
}

#[test]
fn legacy_amp_str_is_a_migration_error() {
    let diags = parse_and_lower("fn f(text: &str):\n\tprint(text)\n").unwrap_err();
    assert!(
        diags
            .iter()
            .any(|d| d.message.contains("`&str` was renamed to `strview`"))
    );
}

#[test]
fn lower_view_of_non_str_rejected() {
    // Legacy `&name` view syntax (M8.4 pre-Q5): only `&str` was ever
    // valid, and it is now a targeted rename error (final spec Q5);
    // `&int` must be an unknown-view-type diagnostic, not a silent
    // fallthrough to `int`.
    let diags = parse_and_lower("fn f(x: &int):\n\tprint(\"\")\n").unwrap_err();
    assert!(diags.iter().any(|d| d.code == DiagCode::UnknownType));
}

#[test]
fn bytes_literal_lowers_to_uir() {
    let (uir, pool) = parse_and_lower("b = b\"A\\x00\"\n").expect("lower ok");
    let body = body_named(&uir, &pool, "main");
    let _ = body;
    let mut found = false;
    for idx in 1..uir.instructions.len() {
        let r = InstRef::from_raw(u32::try_from(idx).expect("idx fits u32"));
        let inst = uir.inst(r);
        if inst.tag == InstTag::BytesLiteral {
            let InstData::Str(id) = inst.data else {
                panic!("BytesLiteral must carry InstData::Str");
            };
            assert_eq!(pool.bytes_payload(id), b"A\x00");
            found = true;
        }
    }
    assert!(found, "no BytesLiteral instruction emitted");
}

#[test]
fn struct_decl_registered_with_resolved_fields() {
    let src = "struct Point:\n\tx: float\n\ty: float\n\nfn main():\n\tp = Point{x=1.0, y=2.0}\n";
    let (uir, pool) = parse_and_lower(src).unwrap();
    assert_eq!(uir.struct_decls.len(), 1);
    let decl = &uir.struct_decls[0];
    assert_eq!(pool.str(decl.name), "Point");
    assert_eq!(decl.fields.len(), 2);
    assert_eq!(decl.fields[0].ty, pool.float());
    assert!(pool.is_defined_struct(decl.ty));
}

#[test]
fn struct_literal_and_field_access_lower() {
    let src = "struct Point:\n\tx: float\n\np = Point{x=1.0}\nq = p.x\n";
    let (uir, pool) = parse_and_lower(src).unwrap();
    let main = body_named(&uir, &pool, "main");
    let stmts = uir.body_stmts(main);
    let decl = uir.var_decl_view(stmts.at(0));
    let lit = uir.struct_lit_view(decl.initializer);
    assert_eq!(lit.name.map(|n| pool.str(n)), Some("Point"));
    let fields: Vec<_> = lit.fields().collect();
    assert_eq!(fields.len(), 1);
    assert_eq!(pool.str(fields[0].0), "x");
    let q = uir.var_decl_view(stmts.at(1));
    assert!(matches!(uir.inst(q.initializer).tag, InstTag::FieldAccess));
}

#[test]
fn struct_value_cycle_is_diagnosed() {
    let err = parse_and_lower("struct Node:\n\tnext: Node\n").unwrap_err();
    assert!(err.iter().any(|d| d.code == DiagCode::InfiniteSize));
}

#[test]
fn mutual_struct_value_cycle_is_diagnosed() {
    let err = parse_and_lower("struct A:\n\tb: B\n\nstruct B:\n\ta: A\n").unwrap_err();
    assert!(err.iter().any(|d| d.code == DiagCode::InfiniteSize));
}

#[test]
fn value_cycle_through_anon_field_type_is_diagnosed() {
    // A self-reference hidden inside an anonymous field type is
    // still by-value and still infinite-size: `data: {next: Node,
    // v: int}` must trip the same E0005 as a plain `next: Node`.
    let err = parse_and_lower("struct Node:\n\tdata: {next: Node, v: int}\n").unwrap_err();
    assert!(err.iter().any(|d| d.code == DiagCode::InfiniteSize));
}

#[test]
fn anon_field_type_in_struct_decl_resolves() {
    // Struct fields take full type expressions; the declaration's
    // resolved field type is the structural anonymous struct.
    let src = "struct Line:\n\tstart: (int, int)\n\nfn main():\n\tl = Line{start=(0, 0)}\n";
    let (uir, pool) = parse_and_lower(src).unwrap();
    let decl = &uir.struct_decls[0];
    assert_eq!(decl.fields.len(), 1);
    assert!(
        matches!(pool.kind(decl.fields[0].ty), TypeKind::AnonStruct),
        "field type must be the anonymous struct, got {:?}",
        pool.kind(decl.fields[0].ty)
    );
}

#[test]
fn forward_struct_reference_resolves() {
    // Field types may name structs declared later in the file:
    // the pre-scan declares every struct before any is defined.
    let src = "struct Line:\n\tstart: Point\n\nstruct Point:\n\tx: float\n";
    let (uir, pool) = parse_and_lower(src).unwrap();
    assert_eq!(uir.struct_decls.len(), 2);
    let line = &uir.struct_decls[0];
    let point = &uir.struct_decls[1];
    assert_eq!(line.fields[0].ty, point.ty);
    assert!(pool.is_defined_struct(line.ty));
    assert!(pool.is_defined_struct(point.ty));
}

#[test]
fn duplicate_struct_declaration_is_diagnosed() {
    let err = parse_and_lower("struct P:\n\tx: int\n\nstruct P:\n\ty: int\n").unwrap_err();
    assert!(err.iter().any(|d| d.code == DiagCode::DuplicateDeclaration));
}

#[test]
fn unknown_struct_field_type_is_diagnosed_without_defining() {
    // An unresolvable field type must not reach
    // `pool.define_struct` (its layout computation has no
    // answer for the error type).
    let err = parse_and_lower("struct Bad:\n\tx: nope\n").unwrap_err();
    assert!(err.iter().any(|d| d.code == DiagCode::UnknownType));
}

#[test]
fn struct_decl_does_not_trip_explicit_main_check() {
    parse_and_lower("struct Point:\n\tx: float\n\nfn main():\n\tprint(\"hi\")\n")
        .expect("struct decl alongside fn main should lower cleanly");
}

#[test]
fn struct_only_file_lowers_to_empty_main() {
    let (uir, _pool) = parse_and_lower("struct Point:\n\tx: float\n").unwrap();
    assert_eq!(uir.struct_decls.len(), 1);
    assert_eq!(uir.func_bodies.len(), 1);
    assert!(uir.body_stmts(&uir.func_bodies[0]).is_empty());
}

#[test]
fn bytes_annotations_resolve() {
    parse_and_lower("fn f(b: bytes, v: bytesview):\n\treturn\n").expect("lower ok");
}

#[test]
fn bytes_index_lowers_to_uir() {
    let (uir, _pool) = parse_and_lower("b = b\"\\x01\"\nx = b[0]\n").expect("lower ok");
    let mut found = false;
    for idx in 1..uir.instructions.len() {
        let r = InstRef::from_raw(u32::try_from(idx).expect("idx fits u32"));
        if uir.inst(r).tag == InstTag::Index {
            found = true;
        }
    }
    assert!(found, "no Index instruction emitted");
    // Slice form must still parse as Slice (grammar disambiguation).
    let (uir2, _p2) = parse_and_lower("b = b\"\\x01\"\nv = b[0:1]\n").expect("lower ok");
    let mut slices = 0;
    for idx in 1..uir2.instructions.len() {
        let r = InstRef::from_raw(u32::try_from(idx).expect("idx fits u32"));
        if uir2.inst(r).tag == InstTag::Slice {
            slices += 1;
        }
    }
    assert_eq!(slices, 1, "b[0:1] must stay a Slice");
}
