use super::*;

#[test]
fn float_primitive_has_stable_id() {
    let pool = InternPool::new();
    // Stability contract: `float` is at slot 4, `error` at 5.
    assert_eq!(pool.float().raw(), 4);
    assert_eq!(pool.error_type().raw(), 5);
    // Distinctness from peer primitives.
    assert_ne!(pool.float(), pool.int());
    assert_ne!(pool.float(), pool.bool_());
    assert_ne!(pool.float(), pool.error_type());
    // Kind dispatch routes through `Tag::Float`.
    assert_eq!(pool.kind(pool.float()), TypeKind::Float);
}

#[test]
fn float_displays_as_float() {
    let pool = InternPool::new();
    assert_eq!(format!("{}", pool.display(pool.float())), "float");
}

#[test]
fn float_not_compatible_with_int() {
    let pool = InternPool::new();
    assert!(!pool.compatible(pool.float(), pool.int()));
    assert!(pool.compatible(pool.float(), pool.float()));
    assert!(pool.compatible(pool.float(), pool.error_type()));
}

#[test]
fn primitives_have_stable_ids() {
    let pool = InternPool::new();
    assert_ne!(pool.void(), pool.bool_());
    assert_ne!(pool.int(), pool.str_());
    assert_ne!(pool.void(), pool.error_type());
}

#[test]
fn primitive_kind_lookup_matches() {
    let pool = InternPool::new();
    assert_eq!(pool.kind(pool.int()), TypeKind::Int);
    assert_eq!(pool.kind(pool.bool_()), TypeKind::Bool);
    assert_eq!(pool.kind(pool.str_()), TypeKind::Str);
    assert_eq!(pool.kind(pool.void()), TypeKind::Void);
    assert_eq!(pool.kind(pool.error_type()), TypeKind::Error);
}

#[test]
fn display_round_trips_primitives() {
    let pool = InternPool::new();
    assert_eq!(format!("{}", pool.display(pool.int())), "int");
    assert_eq!(format!("{}", pool.display(pool.str_())), "str");
    assert_eq!(format!("{}", pool.display(pool.void())), "void");
    assert_eq!(format!("{}", pool.display(pool.bool_())), "bool");
    assert_eq!(format!("{}", pool.display(pool.error_type())), "<error>");
}

#[test]
fn str_view_is_copy_and_displays() {
    let pool = InternPool::new();
    let v = pool.str_view();
    assert_eq!(pool.kind(v), TypeKind::View(ViewKind::Str));
    assert!(pool.is_copy(v));
    assert!(pool.is_view(v));
    // The pool has no char/u8 primitive at M8.4 — the UTF-8 code
    // unit is not interned as a type, so `view_elem` reports
    // `None` for `strview` until D2/M21 gives elements a pool
    // representation.
    assert_eq!(pool.view_elem(v), None);
    assert_eq!(pool.display(v).to_string(), "strview");
    assert_ne!(v, pool.str_());
}

#[test]
fn view_helpers_reject_non_views() {
    let pool = InternPool::new();
    for t in [pool.int(), pool.str_(), pool.bool_(), pool.float()] {
        assert!(!pool.is_view(t));
        assert_eq!(pool.view_elem(t), None);
    }
}

#[test]
fn owner_view_round_trip() {
    let pool = InternPool::new();
    assert_eq!(pool.owner_view(pool.str_()), Some(pool.str_view()));
    assert_eq!(pool.owner_view(pool.int()), None);
}

#[test]
fn compatible_absorbs_error_sentinel() {
    let pool = InternPool::new();
    assert!(pool.compatible(pool.int(), pool.int()));
    assert!(!pool.compatible(pool.int(), pool.bool_()));
    assert!(pool.compatible(pool.int(), pool.error_type()));
    assert!(pool.compatible(pool.error_type(), pool.bool_()));
}

#[test]
fn anon_struct_dedups_by_ordered_name_type_pairs() {
    let mut pool = InternPool::new();
    let q = pool.intern_str("q");
    let r = pool.intern_str("r");
    let a = pool.anon_struct(&[(q, pool.int()), (r, pool.int())]);
    let b = pool.anon_struct(&[(q, pool.int()), (r, pool.int())]);
    assert_eq!(a, b);
    assert_eq!(pool.kind(a), TypeKind::AnonStruct);
    // Reordered pairs -> different structural identity.
    let c = pool.anon_struct(&[(r, pool.int()), (q, pool.int())]);
    assert_ne!(a, c);
    // Same order, different field type -> distinct type.
    let d = pool.anon_struct(&[(q, pool.int()), (r, pool.str_())]);
    assert_ne!(a, d);
}

#[test]
fn anon_struct_copy_and_eq_capability_inference() {
    let mut pool = InternPool::new();
    let (a, b) = (pool.intern_str("a"), pool.intern_str("b"));

    // All-primitive fields: Copy and Eq-capable.
    let all_prim = pool.anon_struct(&[(a, pool.int()), (b, pool.float())]);
    assert!(pool.is_copy(all_prim));
    assert!(!pool.needs_drop(all_prim));
    assert!(pool.is_eq_capable(all_prim));

    // A `str` field: not Copy (needs drop), but `str` is
    // Eq-capable so the anon struct stays Eq-capable.
    let with_str = pool.anon_struct(&[(a, pool.int()), (b, pool.str_())]);
    assert!(!pool.is_copy(with_str));
    assert!(pool.needs_drop(with_str));
    assert!(pool.is_eq_capable(with_str));

    // Views are Copy but not Eq-capable.
    let with_view = pool.anon_struct(&[(a, pool.str_view())]);
    assert!(pool.is_copy(with_view));
    assert!(!pool.is_eq_capable(with_view));

    // Nested anon structs classify recursively.
    let nested_copy = pool.anon_struct(&[(a, all_prim), (b, pool.bool_())]);
    assert!(pool.is_copy(nested_copy));
    assert!(!pool.needs_drop(nested_copy));
    assert!(pool.is_eq_capable(nested_copy));
    let nested_move = pool.anon_struct(&[(a, with_str)]);
    assert!(!pool.is_copy(nested_move));
    assert!(pool.needs_drop(nested_move));
}

#[test]
fn anon_struct_display_paren_and_brace_forms() {
    let mut pool = InternPool::new();
    // Fields named "0", "1", … in written order render as the
    // tuple paren form.
    let (f0, f1) = (pool.intern_str("0"), pool.intern_str("1"));
    let pair = pool.anon_struct(&[(f0, pool.int()), (f1, pool.str_())]);
    assert_eq!(format!("{}", pool.display(pair)), "(int, str)");

    // Any other names render in brace form, written order.
    let (q, r) = (pool.intern_str("q"), pool.intern_str("r"));
    let named = pool.anon_struct(&[(q, pool.int()), (r, pool.int())]);
    assert_eq!(format!("{}", pool.display(named)), "{q: int, r: int}");

    // Mixed numeric/non-numeric names force brace form.
    let x = pool.intern_str("x");
    let mixed = pool.anon_struct(&[(f0, pool.int()), (x, pool.str_())]);
    assert_eq!(format!("{}", pool.display(mixed)), "{0: int, x: str}");

    // A one-field paren form keeps the mandatory trailing comma.
    let one = pool.anon_struct(&[(f0, pool.int())]);
    assert_eq!(format!("{}", pool.display(one)), "(int,)");
}

#[test]
fn struct_view_anon_sentinel_name_and_written_order_offsets() {
    let mut pool = InternPool::new();
    let (q, r) = (pool.intern_str("q"), pool.intern_str("r"));
    let id = pool.anon_struct(&[(q, pool.str_()), (r, pool.int())]);
    let view = pool.struct_view(id);
    // Anon structs carry no declared name: the payload records
    // the interned "" sentinel and default (attribute-free) flags.
    assert_eq!(pool.str(view.name), "");
    assert!(!view.is_eq());
    assert!(!view.is_repr_c());
    // Layout is written order with natural alignment: str (24b) at
    // 0, int at 24.
    assert_eq!(view.fields.len(), 2);
    assert_eq!(view.fields[0].name, q);
    assert_eq!(view.fields[0].ty, pool.str_());
    assert_eq!(view.fields[0].offset, 0);
    assert_eq!(view.fields[0].idx, 0);
    assert_eq!(view.fields[1].name, r);
    assert_eq!(view.fields[1].ty, pool.int());
    assert_eq!(view.fields[1].offset, 24);
    assert_eq!(view.fields[1].idx, 1);
    assert_eq!((view.size, view.align), (32, 8));
    assert_eq!(pool.size_align(id), (32, 8));
    assert_eq!(pool.struct_field(id, r).unwrap().offset, 24);
}

#[test]
fn intern_str_dedups() {
    let mut pool = InternPool::new();
    let a = pool.intern_str("hello");
    let b = pool.intern_str("hello");
    assert_eq!(a, b);
    let c = pool.intern_str("world");
    assert_ne!(a, c);
    assert_eq!(pool.str(a), "hello");
    assert_eq!(pool.str(c), "world");
}

#[test]
fn intern_str_handles_empty_and_unicode() {
    let mut pool = InternPool::new();
    let e = pool.intern_str("");
    let u = pool.intern_str("ηλο 🌍");
    assert_eq!(pool.str(e), "");
    assert_eq!(pool.str(u), "ηλο 🌍");
}

#[test]
fn intern_str_does_not_duplicate_bytes() {
    // The dedup table stores `StringId` handles, not owned
    // `String` keys, so the byte content lives exactly once —
    // in `string_bytes`. Asserts the storage shape: after
    // interning N distinct strings, the arena grows by exactly
    // the sum of their byte lengths and the table by N (measured
    // as deltas: `new` pre-interns the well-known attribute
    // vocabulary at fixed ids, a nonzero baseline).
    let mut pool = InternPool::new();
    let bytes_before = pool.string_bytes.len();
    let table_before = pool.string_dedup.len();
    let inputs = ["alpha", "beta", "gamma", "δελτα"];
    for s in &inputs {
        pool.intern_str(s);
    }
    let expected: usize = inputs.iter().map(|s| s.len()).sum();
    assert_eq!(pool.string_bytes.len() - bytes_before, expected);
    assert_eq!(pool.string_dedup.len() - table_before, inputs.len());
}

#[test]
fn intern_str_repeated_does_not_grow_arena() {
    // Probe path: repeated `intern_str` of the same string
    // returns the same `StringId` and appends nothing to the
    // arena or the dedup table.
    let mut pool = InternPool::new();
    let first = pool.intern_str("identifier");
    let bytes_len = pool.string_bytes.len();
    let table_len = pool.string_dedup.len();
    for _ in 0..1000 {
        assert_eq!(pool.intern_str("identifier"), first);
    }
    assert_eq!(pool.string_bytes.len(), bytes_len);
    assert_eq!(pool.string_dedup.len(), table_len);
}

#[test]
fn error_type_is_distinct_from_primitives() {
    let pool = InternPool::new();
    let err = pool.error_type();
    assert_ne!(err, pool.void());
    assert_ne!(err, pool.bool_());
    assert_ne!(err, pool.int());
    assert_ne!(err, pool.str_());
    // Stable across repeated calls.
    assert_eq!(err, pool.error_type());
}

#[test]
fn is_error_only_true_for_error_sentinel() {
    let pool = InternPool::new();
    assert!(pool.is_error(pool.error_type()));
    assert!(!pool.is_error(pool.int()));
    assert!(!pool.is_error(pool.bool_()));
    assert!(!pool.is_error(pool.void()));
    assert!(!pool.is_error(pool.str_()));
}

#[test]
fn compatible_is_reflexive_and_distinguishes_primitives() {
    let pool = InternPool::new();
    assert!(pool.compatible(pool.int(), pool.int()));
    assert!(pool.compatible(pool.bool_(), pool.bool_()));
    assert!(!pool.compatible(pool.int(), pool.bool_()));
    assert!(!pool.compatible(pool.str_(), pool.int()));
}

#[test]
fn compatible_absorbs_error_sentinel_in_either_position() {
    let pool = InternPool::new();
    let err = pool.error_type();
    // Symmetry: Error compatible with every primitive on both sides.
    for &t in &[pool.int(), pool.bool_(), pool.str_(), pool.void()] {
        assert!(pool.compatible(err, t), "Error vs {:?}", pool.kind(t));
        assert!(pool.compatible(t, err), "{:?} vs Error", pool.kind(t));
    }
    // Error vs Error is also compatible (trivially via reflexivity).
    assert!(pool.compatible(err, err));
}

#[test]
fn never_primitive_has_stable_id() {
    let pool = InternPool::new();
    assert_eq!(pool.never().raw(), 6);
    assert_eq!(pool.kind(pool.never()), TypeKind::Never);
    assert_ne!(pool.never(), pool.void());
    assert_ne!(pool.never(), pool.error_type());
}

#[test]
fn never_displays_as_never() {
    let pool = InternPool::new();
    assert_eq!(format!("{}", pool.display(pool.never())), "never");
}

#[test]
fn never_is_compatible_with_anything() {
    let pool = InternPool::new();
    let n = pool.never();
    assert!(pool.compatible(n, pool.int()));
    assert!(pool.compatible(n, pool.bool_()));
    assert!(pool.compatible(n, pool.str_()));
    assert!(pool.compatible(n, pool.void()));
    assert!(pool.compatible(n, pool.float()));
    assert!(pool.compatible(n, pool.error_type()));
    assert!(pool.compatible(pool.error_type(), n));
    assert!(pool.compatible(pool.int(), n));
    assert!(pool.compatible(n, n));
}

#[test]
fn bytes_and_bytesview_are_interned_at_fixed_ids() {
    let pool = InternPool::new();
    assert_eq!(pool.kind(pool.bytes()), TypeKind::Bytes);
    assert_eq!(
        pool.kind(pool.bytes_view()),
        TypeKind::View(ViewKind::Bytes)
    );
    // Stable singletons across calls.
    assert_eq!(pool.bytes(), pool.bytes());
    assert_eq!(pool.bytes_view(), pool.bytes_view());
}

#[test]
fn owner_view_table_covers_str_and_bytes() {
    let pool = InternPool::new();
    assert_eq!(pool.owner_view(pool.str_()), Some(pool.str_view()));
    assert_eq!(pool.owner_view(pool.bytes()), Some(pool.bytes_view()));
    assert_eq!(pool.owner_view(pool.int()), None);
    // Inverse table.
    assert_eq!(pool.view_owner(pool.str_view()), Some(pool.str_()));
    assert_eq!(pool.view_owner(pool.bytes_view()), Some(pool.bytes()));
    assert_eq!(pool.view_owner(pool.str_()), None);
}

#[test]
fn bytes_display_and_copy_classification() {
    let pool = InternPool::new();
    assert_eq!(format!("{}", pool.display(pool.bytes())), "bytes");
    assert_eq!(format!("{}", pool.display(pool.bytes_view())), "bytesview");
    // bytes is an owner (move type); bytesview is a Copy projection.
    assert!(!pool.is_copy(pool.bytes()));
    assert!(pool.is_copy(pool.bytes_view()));
    assert!(pool.is_view(pool.bytes_view()));
    assert_eq!(pool.view_elem(pool.bytes_view()), None); // u8 lands at M17.1
}

#[test]
fn intern_bytes_round_trips_non_utf8() {
    let mut pool = InternPool::new();
    let id = pool.intern_bytes(&[0x41, 0x00, 0xff]);
    assert_eq!(pool.bytes_payload(id), &[0x41, 0x00, 0xff]);
    // Dedups on content.
    assert_eq!(pool.intern_bytes(&[0x41, 0x00, 0xff]), id);
    // Same bytes interned as a str share the id (same byte content).
    assert_eq!(pool.intern_str("A"), pool.intern_bytes(b"A"));
}

#[test]
fn well_known_attribute_strings_have_fixed_ids() {
    // The pool-less parser recognizes attribute vocabulary by id,
    // so the intern order in `new` is a stability contract.
    let mut pool = InternPool::new();
    assert_eq!(pool.str(StringId::ATTR_DERIVE), "derive");
    assert_eq!(pool.str(StringId::ATTR_EQ), "Eq");
    assert_eq!(pool.str(StringId::ATTR_REPR), "repr");
    assert_eq!(pool.str(StringId::ATTR_C), "C");
    // Re-interning returns the same id; user strings land after.
    assert_eq!(pool.intern_str("derive"), StringId::ATTR_DERIVE);
    let user = pool.intern_str("Point");
    assert!(user.raw() > StringId::ATTR_C.raw());
}

#[test]
fn struct_declare_define_round_trip() {
    let mut pool = InternPool::new();
    let name = pool.intern_str("Point");
    let (x, y) = (pool.intern_str("x"), pool.intern_str("y"));
    let id = pool.declare_struct(name);
    assert!(!pool.is_defined_struct(id));
    pool.define_struct(
        id,
        name,
        &[(x, pool.float()), (y, pool.float())],
        StructFlags::default(),
    );
    assert!(pool.is_defined_struct(id));
    let view = pool.struct_view(id);
    assert_eq!(pool.str(view.name), "Point");
    assert_eq!(view.fields.len(), 2);
    assert_eq!(view.fields[0].offset, 0);
    assert_eq!(view.fields[1].offset, 8);
    assert_eq!((view.size, view.align), (16, 8));
    assert!(view.is_copy);
    assert!(pool.is_copy(id));
    assert!(!pool.needs_drop(id));
    assert_eq!(pool.display(id).to_string(), "Point");
}

#[test]
fn struct_layout_mixed_alignment_and_copy_inference() {
    let mut pool = InternPool::new();
    let name = pool.intern_str("Person");
    let (n, a) = (pool.intern_str("name"), pool.intern_str("age"));
    let id = pool.declare_struct(name);
    pool.define_struct(
        id,
        name,
        &[(n, pool.str_()), (a, pool.int())],
        StructFlags::default(),
    );
    let view = pool.struct_view(id);
    assert_eq!(view.fields[0].offset, 0); // str: 24 bytes at 0
    assert_eq!(view.fields[1].offset, 24); // int: 8 bytes at 24
    assert_eq!((view.size, view.align), (32, 8));
    assert!(!view.is_copy); // str field => move type
    assert!(pool.needs_drop(id));
}

#[test]
fn struct_flags_round_trip() {
    // M9.1: `#[repr(C)]` / `#[derive(Eq)]` ride in the payload's
    // flags word — layout is identical, the bits read back clean.
    let mut pool = InternPool::new();
    let name = pool.intern_str("Mixed");
    let (a, b, c) = (
        pool.intern_str("a"),
        pool.intern_str("b"),
        pool.intern_str("c"),
    );
    let id = pool.declare_struct(name);
    pool.define_struct(
        id,
        name,
        &[(a, pool.int()), (b, pool.float()), (c, pool.int())],
        StructFlags {
            derive_eq: true,
            repr_c: true,
        },
    );
    let view = pool.struct_view(id);
    assert_eq!(view.fields[0].offset, 0);
    assert_eq!(view.fields[1].offset, 8);
    assert_eq!(view.fields[2].offset, 16);
    assert!(view.is_repr_c());
    assert!(view.is_eq());
    assert!(pool.is_eq_capable(id));
}

#[test]
fn struct_names_dedup_and_nested_layout() {
    let mut pool = InternPool::new();
    let p = pool.intern_str("Point");
    let (x, y) = (pool.intern_str("x"), pool.intern_str("y"));
    let a = pool.declare_struct(p);
    assert_eq!(a, pool.declare_struct(p)); // same name, same TypeId
    pool.define_struct(
        a,
        p,
        &[(x, pool.float()), (y, pool.float())],
        StructFlags::default(),
    );
    let line = pool.intern_str("Line");
    let (s, e) = (pool.intern_str("start"), pool.intern_str("end"));
    let b = pool.declare_struct(line);
    pool.define_struct(b, line, &[(s, a), (e, a)], StructFlags::default());
    let view = pool.struct_view(b);
    assert_eq!(view.fields[1].offset, 16);
    assert_eq!((view.size, view.align), (32, 8));
    assert!(view.is_copy); // nested Copy struct stays Copy
}

fn unit_variant(name: StringId) -> EnumVariantDef {
    EnumVariantDef {
        name,
        kind: VariantKind::Unit,
        fields: vec![],
    }
}

fn tuple_variant(name: StringId, fields: Vec<(StringId, TypeId)>) -> EnumVariantDef {
    EnumVariantDef {
        name,
        kind: VariantKind::Tuple,
        fields,
    }
}

fn named_variant(name: StringId, fields: Vec<(StringId, TypeId)>) -> EnumVariantDef {
    EnumVariantDef {
        name,
        kind: VariantKind::Named,
        fields,
    }
}

const EQ_FLAGS: StructFlags = StructFlags {
    derive_eq: true,
    repr_c: false,
};

#[test]
fn enum_unit_variants_layout_copy_nominal_and_eq_flag() {
    // (a)+(c) unit variants are (4,4) Copy; distinct nominal ids.
    let mut pool = InternPool::new();
    let color = pool.intern_str("Color");
    let red = pool.intern_str("Red");
    let green = pool.intern_str("Green");
    let blue = pool.intern_str("Blue");
    let id = pool.declare_enum(color);
    assert_eq!(id, pool.declare_enum(color)); // nominal dedup
    let r = unit_variant(red);
    let g = unit_variant(green);
    let b = unit_variant(blue);
    pool.define_enum(id, color, &[r.clone(), g, b], StructFlags::default());
    assert_eq!(pool.size_align(id), (4, 4));
    assert!(pool.is_copy(id));
    assert_eq!(pool.display(id).to_string(), "Color");
    // Same variant shape under a different name: nominal identity.
    let shade = pool.intern_str("Shade");
    let other = pool.declare_enum(shade);
    pool.define_enum(other, shade, &[r], EQ_FLAGS);
    assert_ne!(id, other);
    assert!(pool.enum_view(other).is_eq());
}

#[test]
fn enum_mixed_payload_layout_offsets() {
    // (b) Result{Success(int), Error(str)}: both payloads align to 8
    // (align_up(4, payload_align)) — no small-payload packing.
    let mut pool = InternPool::new();
    let result = pool.intern_str("Result");
    let (success, error) = (pool.intern_str("Success"), pool.intern_str("Error"));
    let f0 = pool.intern_str("0");
    let (int, str_) = (pool.int(), pool.str_());
    let id = pool.declare_enum(result);
    let s = tuple_variant(success, vec![(f0, int)]);
    let e = tuple_variant(error, vec![(f0, str_)]);
    pool.define_enum(id, result, &[s, e], StructFlags::default());
    assert!(!pool.is_copy(id));
    assert!(pool.needs_drop(id));
    assert_eq!(pool.size_align(id), (32, 8));
    let variants: Vec<_> = pool.enum_view(id).variants().collect();
    assert_eq!(variants[0].offset, 8);
    assert_eq!((variants[1].name, variants[1].offset), (error, 8));
    assert_eq!(variants[1].fields[0].offset, 8);
}

#[test]
fn enum_view_iterates_variants_offsets_and_tuple_index_names() {
    // (d)+(f) synthesized "0"/"1" tuple names; decl-order offsets.
    let mut pool = InternPool::new();
    let shape = pool.intern_str("Shape");
    let (rect, pair) = (pool.intern_str("Rectangle"), pool.intern_str("Pair"));
    let (w, h) = (pool.intern_str("w"), pool.intern_str("h"));
    let (n0, n1) = (pool.intern_str("0"), pool.intern_str("1"));
    let (int, str_, float) = (pool.int(), pool.str_(), pool.float());
    let id = pool.declare_enum(shape);
    let rv = named_variant(rect, vec![(w, float), (h, float)]);
    let pv = tuple_variant(pair, vec![(n0, int), (n1, str_)]);
    pool.define_enum(id, shape, &[rv, pv], StructFlags::default());
    // Pair's 32-byte payload aligns to 8, pushing the enum to 40.
    assert_eq!(pool.size_align(id), (40, 8));
    let variants: Vec<_> = pool.enum_view(id).variants().collect();
    assert_eq!(variants[0].kind, VariantKind::Named);
    assert_eq!(variants[0].offset, 8);
    assert_eq!(variants[0].fields[1].offset, 16);
    assert_eq!(variants[1].offset, 8);
    assert_eq!(variants[1].fields[0].name, n0);
    assert_eq!(variants[1].fields[1].name, n1);
}

#[test]
#[should_panic(expected = "at least one variant")]
fn enum_define_with_zero_variants_panics() {
    let mut pool = InternPool::new();
    let name = pool.intern_str("Empty");
    let id = pool.declare_enum(name);
    pool.define_enum(id, name, &[], StructFlags::default());
}
