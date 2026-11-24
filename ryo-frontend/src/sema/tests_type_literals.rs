//! M10 anonymous struct type literals (`{q: int, r: int}`) — sema
//! tests: structural identity at the three annotation positions
//! (return type, param, var annotation), view-field rejection through
//! the M9 `ViewFieldType` path, and the §9-bar TypeMismatch
//! enrichment: a field-level diff note when both sides are
//! struct-kinded (did-you-mean for name typos, the field named for
//! type diffs) plus the graduation fix-it when a named struct is
//! expected and an anonymous shape is found.

use super::tests::*;
use super::*;
use ryo_core::types::TypeKind;

fn mismatch_diags(diags: &[Diag]) -> Vec<&Diag> {
    diags
        .iter()
        .filter(|d| d.code == DiagCode::TypeMismatch)
        .collect()
}

fn note_text(d: &Diag) -> String {
    d.notes
        .iter()
        .map(|n| n.message.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn type_literal_return_type_checks() {
    let src = "fn divmod(a: int, b: int) -> {q: int, r: int}:\n\treturn {q=a / b, r=a % b}\n";
    let (tirs, pool) = run(src).expect("sema ok");
    let divmod = tir_named(&tirs, &pool, "divmod");
    assert!(
        matches!(pool.kind(divmod.return_type), TypeKind::AnonStruct),
        "return annotation must resolve to an anonymous struct"
    );
}

#[test]
fn type_literal_param_type_checks() {
    let src = "fn sum(p: {x: int, y: int}) -> int:\n\treturn p.x + p.y\n\nfn main():\n\tprint(sum({x=1, y=2}))\n";
    assert!(run(src).is_ok());
}

#[test]
fn type_literal_var_annotation_checks() {
    let src = "fn main():\n\tdm: {q: int, r: int} = {q=1, r=2}\n\tprint(dm.q)\n";
    assert!(run(src).is_ok());
}

#[test]
fn type_literal_duplicate_fields_rejected() {
    // A repeated field name has no structural meaning — diagnosed
    // exactly like the value literal's duplicate, and the annotation
    // absorbs to the error sentinel.
    let src = "fn f() -> {q: int, q: int}:\n\treturn 1\n";
    let (_t, diags, _pool) = run_with_errors(src);
    assert!(
        any_code(&diags, DiagCode::DuplicateStructField),
        "got {diags:?}"
    );
    let d = diags
        .iter()
        .find(|d| d.code == DiagCode::DuplicateStructField)
        .unwrap();
    assert_eq!(d.message, "field 'q' is specified more than once");
}

#[test]
fn type_literal_numeric_keys_match_tuple_sugar() {
    // The brace spelling with numeric keys IS the paren sugar's
    // structural type, so a tuple value satisfies the annotation.
    let src = "fn f() -> {0: int, 1: str}:\n\treturn (1, \"a\")\n\nfn main():\n\tprint(f())\n";
    assert!(run(src).is_ok());
}

#[test]
fn type_literal_view_field_rejected() {
    // Rule 6 holds inside type literals too: a view-typed field is a
    // projection and is rejected with the same ViewFieldType diagnostic
    // as a named struct declaration / anonymous value literal.
    let src = "fn f(p: {v: strview}) -> int:\n\treturn 1\n";
    let (_t, diags, pool) = run_with_errors(src);
    assert!(any_code(&diags, DiagCode::ViewFieldType), "got {diags:?}");
    let d = diags
        .iter()
        .find(|d| d.code == DiagCode::ViewFieldType)
        .unwrap();
    assert_eq!(
        d.message,
        format!(
            "struct fields must be owned values; '{}' is a projection (Rule 6)",
            pool.display(pool.str_view()),
        )
    );
}

#[test]
fn shape_typo_emits_field_diff_note_with_did_you_mean() {
    // §9 bar (structural shape typo): the error shows both structural
    // shapes and adds a field-level diff note naming the differing
    // field, with a did-you-mean for the name typo (`rr` vs `r`).
    let src = "fn divmod(a: int, b: int) -> {q: int, rr: int}:\n\treturn {q=a / b, r=a % b}\n";
    let (_t, diags, _pool) = run_with_errors(src);
    let mismatches = mismatch_diags(&diags);
    assert_eq!(mismatches.len(), 1, "got {diags:?}");
    let d = mismatches[0];
    assert!(
        d.message.contains("expects '{q: int, rr: int}'"),
        "error must show the expected shape, got: {}",
        d.message
    );
    assert!(
        d.message.contains("got '{q: int, r: int}'"),
        "error must show the found shape, got: {}",
        d.message
    );
    let notes = note_text(d);
    assert!(
        notes.contains("expected 'rr', found 'r'"),
        "note must name the differing fields, got: {notes}"
    );
    assert!(
        notes.contains("did you mean 'r'?"),
        "name typos get a did-you-mean, got: {notes}"
    );
}

#[test]
fn type_diff_note_names_the_field() {
    let src = "fn f() -> {q: int}:\n\treturn {q=\"a\"}\n";
    let (_t, diags, _pool) = run_with_errors(src);
    let mismatches = mismatch_diags(&diags);
    assert_eq!(mismatches.len(), 1, "got {diags:?}");
    let notes = note_text(mismatches[0]);
    assert!(
        notes.contains("field 'q': expected 'int', found 'str'"),
        "type diffs name the field, got: {notes}"
    );
}

#[test]
fn field_count_diff_note_names_missing_field() {
    let src = "fn f() -> {q: int, r: int}:\n\treturn {q=1}\n";
    let (_t, diags, _pool) = run_with_errors(src);
    let mismatches = mismatch_diags(&diags);
    assert_eq!(mismatches.len(), 1, "got {diags:?}");
    let notes = note_text(mismatches[0]);
    assert!(
        notes.contains("the found shape is missing field(s) 'r'"),
        "count diffs name the missing field, got: {notes}"
    );
}

#[test]
fn named_expected_anon_found_graduation_fixit() {
    // §9 bar (graduation): passing an anonymous shape where a named
    // struct is expected gets a fix-it that teaches explicit
    // construction with the real struct name and field names.
    let src = "struct Point:\n\tx: int\n\ty: int\n\nfn take(p: Point):\n\tprint(p.x)\n\nfn main():\n\ttake({x=1, y=2})\n";
    let (_t, diags, _pool) = run_with_errors(src);
    let mismatches = mismatch_diags(&diags);
    assert_eq!(mismatches.len(), 1, "got {diags:?}");
    let d = mismatches[0];
    assert!(
        d.message.contains("expected 'Point'"),
        "error must name the expected struct, got: {}",
        d.message
    );
    assert!(
        d.message.contains("has type '{x: int, y: int}'"),
        "error must show the found shape, got: {}",
        d.message
    );
    assert!(
        d.notes
            .iter()
            .any(|n| n.message == "help: construct explicitly: `Point{x=…, y=…}`"),
        "expected the graduation fix-it, got: {}",
        note_text(d)
    );
}

#[test]
fn one_element_tuple_comma_note_at_boundaries() {
    // The `(x)` vs `(x,)` pitfall: expected `(T,)`, found `T`. All
    // three annotation positions funnel through the same TypeMismatch
    // enrichment, so each gets the comma note.
    let cases: &[&str] = &[
        // return boundary
        "fn f() -> (str,):\n\treturn \"zero\"\n\nfn main():\n\tprint(f())\n",
        // call-argument boundary (the parenthesized-grouping spelling)
        "fn g(x: (str,)):\n\tprint(x)\n\nfn main():\n\tg((\"zero\"))\n",
        // var-annotation boundary
        "fn main():\n\tz: (str,) = \"zero\"\n\tprint(z)\n",
    ];
    for src in cases {
        let (_t, diags, _p) = run_with_errors(src);
        let m = mismatch_diags(&diags);
        assert_eq!(
            m.len(),
            1,
            "expected one mismatch for {src:?}, got {diags:?}"
        );
        assert!(
            note_text(m[0]).contains("write (value,) instead of (value)"),
            "expected the one-element-tuple note for {src:?}, got {}",
            note_text(m[0]),
        );
    }
}

#[test]
fn one_element_tuple_comma_note_not_a_false_positive() {
    // A two-field expected shape is a genuine arity mismatch, and a
    // wrong-typed value is a genuine type mismatch — neither is the
    // comma pitfall, so neither gets the note.
    let cases: &[&str] = &[
        "fn f() -> (str, str):\n\treturn \"zero\"\n\nfn main():\n\tprint(f())\n",
        "fn f() -> (str,):\n\treturn 5\n\nfn main():\n\tprint(f())\n",
    ];
    for src in cases {
        let (_t, diags, _p) = run_with_errors(src);
        let m = mismatch_diags(&diags);
        assert_eq!(
            m.len(),
            1,
            "expected one mismatch for {src:?}, got {diags:?}"
        );
        assert!(
            !note_text(m[0]).contains("trailing comma"),
            "note must not suggest the comma for {src:?}, got {}",
            note_text(m[0]),
        );
    }
}

#[test]
fn positional_access_on_non_struct_points_at_the_comma() {
    // `z = ("zero")` groups: z is str. The positional access is the
    // tell, and the note connects it back to the missing comma.
    let src = "fn main():\n\tz = (\"zero\")\n\tprint(z.0)\n";
    let (_t, diags, _p) = run_with_errors(src);
    let d = diags
        .iter()
        .find(|d| d.code == DiagCode::NotAStruct)
        .expect("E0041");
    assert!(
        d.notes.iter().any(|n| n.message.contains("trailing comma")),
        "expected the comma note, got {diags:?}",
    );

    // Named-key access on a non-struct carries no tuple suspicion —
    // no note.
    let src = "fn main():\n\tz = \"zero\"\n\tprint(z.len)\n";
    let (_t, diags, _p) = run_with_errors(src);
    let d = diags
        .iter()
        .find(|d| d.code == DiagCode::NotAStruct)
        .expect("E0041");
    assert!(
        d.notes.is_empty(),
        "named-key access must not get the comma note, got {diags:?}",
    );
}

#[test]
fn sema_resolves_annotations_and_inferred_unions_from_uir() {
    let src = "fn forward(value: Later) -> Later:\n\treturn value\n\
               fn structural(value: {item: (int, str)}) -> {result: (bool, bytes)}:\n\tlocal: {nested: (Later, int)} = {nested=(Later{value=1}, 2)}\n\treturn {result=(true, b\"x\")}\n\
               fn positional(value: (int, str)) -> (int, str):\n\treturn value\n\
               fn explicit() -> Failure!int:\n\treturn 1\n\
               fn inferred() -> !int:\n\treturn 1\n\
               struct Later:\n\tvalue: int\n\
               error Failure\n";
    let (tirs, pool) = run(src).expect("Sema resolves every unresolved annotation form");

    let forward = tir_named(&tirs, &pool, "forward");
    assert_eq!(forward.params[0].ty, forward.return_type);
    assert_eq!(pool.display(forward.return_type).to_string(), "Later");

    let structural = tir_named(&tirs, &pool, "structural");
    let parameter = pool.struct_view(structural.params[0].ty);
    assert_eq!(pool.str(parameter.fields[0].name), "item");
    let item = pool.struct_view(parameter.fields[0].ty);
    assert_eq!(item.fields[0].ty, pool.int());
    assert_eq!(item.fields[1].ty, pool.str_());
    let result = pool.struct_view(structural.return_type);
    assert_eq!(pool.str(result.fields[0].name), "result");
    let result = pool.struct_view(result.fields[0].ty);
    assert_eq!(result.fields[0].ty, pool.bool_());
    assert_eq!(result.fields[1].ty, pool.bytes());
    let local = structural.inst(stmt_at(structural, 0)).ty;
    let local = pool.struct_view(local);
    assert_eq!(pool.str(local.fields[0].name), "nested");
    let nested = pool.struct_view(local.fields[0].ty);
    assert_eq!(pool.display(nested.fields[0].ty).to_string(), "Later");
    assert_eq!(nested.fields[1].ty, pool.int());

    let positional = tir_named(&tirs, &pool, "positional");
    assert_eq!(positional.params[0].ty, positional.return_type);
    let positional_type = pool.struct_view(positional.return_type);
    let names: Vec<_> = positional_type
        .fields
        .iter()
        .map(|field| pool.str(field.name))
        .collect();
    assert_eq!(names, ["0", "1"]);
    assert_eq!(positional_type.fields[0].ty, pool.int());
    assert_eq!(positional_type.fields[1].ty, pool.str_());

    let explicit = tir_named(&tirs, &pool, "explicit");
    assert_eq!(pool.kind(explicit.return_type), TypeKind::ErrorUnion);
    assert_eq!(
        pool.display(explicit.return_type).to_string(),
        "Failure!int"
    );
    let inferred = tir_named(&tirs, &pool, "inferred");
    assert_eq!(pool.kind(inferred.return_type), TypeKind::ErrorUnion);
    assert_eq!(pool.display(inferred.return_type).to_string(), "!int");
}

#[test]
fn positional_type_expr_resolves_to_same_type_as_anon_fields() {
    let src = "fn positional() -> (int, str):\n\treturn (1, \"a\")\n\
               fn anon() -> {0: int, 1: str}:\n\treturn (1, \"a\")\n";
    let (tirs, pool) = run(src).expect("both structural spellings resolve");
    let positional = tir_named(&tirs, &pool, "positional").return_type;
    let anon = tir_named(&tirs, &pool, "anon").return_type;
    assert_eq!(positional, anon);
    assert!(matches!(pool.kind(positional), TypeKind::AnonStruct));
    let view = pool.struct_view(positional);
    let names: Vec<&str> = view.fields.iter().map(|f| pool.str(f.name)).collect();
    assert_eq!(names, ["0", "1"]);
    assert_eq!(view.fields[0].ty, pool.int());
    assert_eq!(view.fields[1].ty, pool.str_());
}

#[test]
fn anonymous_field_unknown_type_does_not_compute_layout() {
    let (_tirs, diags, _pool) = run_with_errors("fn f(value: {x: Missing}):\n\treturn\n");
    assert_eq!(count_code(&diags, DiagCode::UnknownType), 1, "{diags:?}");
}

#[test]
fn return_only_types_in_nominal_fields_are_diagnosed() {
    for ty in ["void", "never"] {
        let src = format!("struct Holder:\n\tvalue: {ty}\nfn main():\n\treturn\n");
        let (_tirs, diags, _pool) = run_with_errors(&src);
        assert_eq!(
            count_code(&diags, DiagCode::UnknownType),
            1,
            "{ty}: {diags:?}"
        );
    }
}

#[test]
fn deeply_nested_nominal_field_type_resolves_without_stack_overflow() {
    let mut pool = InternPool::new();
    let int = pool.intern_str("int");
    let field = pool.intern_str("value");
    let mut builder = ryo_core::uir::UirBuilder::new();
    let mut annotation = builder.type_name(int, false, Span::EMPTY);
    for _ in 0..50_000 {
        annotation = builder.type_anon(&[(field, annotation)], Span::EMPTY);
    }
    builder.add_struct_decl(ryo_core::uir::UirStructDecl {
        name: pool.intern_str("Holder"),
        name_span: Span::EMPTY,
        attrs: ryo_core::ast::StructAttrs::default(),
        fields: vec![ryo_core::uir::UirStructField {
            name: field,
            type_expr: annotation,
            span: Span::EMPTY,
        }],
        span: Span::EMPTY,
        source_index: 0,
    });
    builder.add_function(pool.intern_str("main"), vec![], None, &[], Span::EMPTY);
    let uir = builder.finish().with_layout_context(&pool);
    let mut sink = DiagSink::new();
    let mut context = ryo_core::compilation::CompilationContext::new();

    let _tirs = analyze(
        &uir,
        &mut pool,
        &mut sink,
        "deeply nested nominal field",
        std::path::Path::new("<test>"),
        &mut context,
    );
    assert!(!sink.has_errors(), "{:?}", sink.into_diags());
}
