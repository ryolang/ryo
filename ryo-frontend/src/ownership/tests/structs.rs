use super::super::*;
use super::common::*;

#[test]
fn move_struct_classified() {
    let mut pool = InternPool::new();
    let name = pool.intern_str("Person");
    let id = pool.declare_struct(name);
    let field_name = pool.intern_str("name");
    let str_ty = pool.str_();
    pool.define_struct(id, name, &[(field_name, str_ty)]);
    assert!(is_move_type(id, &pool));

    let point_name = pool.intern_str("Point");
    let point = pool.declare_struct(point_name);
    let x = pool.intern_str("x");
    let float_ty = pool.float();
    pool.define_struct(point, point_name, &[(x, float_ty)]);
    assert!(!is_move_type(point, &pool));
}

#[test]
fn struct_holding_str_freed_after_last_use() {
    let src = "struct Person:\n\tname: str\n\nfn main():\n\tp = Person{name=\"alice\"}\n\tx = 1\n";
    let (_diags, mut sidecar, _tirs, _pool) = check_src_full(src);
    let f = take_function_sidecar(&mut sidecar, 0);
    assert!(
        !f.free_schedule.is_empty(),
        "struct with str field must be freed"
    );
}

#[test]
fn moving_str_into_literal_consumes_source() {
    let src = "struct Person:\n\tname: str\n\nfn main():\n\ts = \"alice\"\n\tp = Person{name=s}\n\tt = s\n";
    let diags = check_src(src);
    assert!(
        diags.iter().any(|d| d.code == DiagCode::UseAfterMove),
        "got {:?}",
        diags
    );
}

#[test]
fn move_out_of_field_rejected() {
    let src =
        "struct Person:\n\tname: str\n\nfn main():\n\tp = Person{name=\"alice\"}\n\tn = p.name\n";
    let diags = check_src(src);
    assert!(
        diags.iter().any(|d| d.code == DiagCode::MoveOutOfField),
        "got {:?}",
        diags
    );
}

#[test]
fn field_borrow_freezes_root_during_call() {
    // The trailing `show(q.name)` doubles as the liveness read of `q`
    // (a moved-into binding never read again would be W0001).
    let src = "struct Person:\n\tname: str\n\nfn show(s: str):\n\tprint(s)\n\nfn main():\n\tp = Person{name=\"alice\"}\n\tshow(p.name)\n\tq = p\n\tshow(q.name)\n";
    let diags = check_src(src);
    assert!(
        diags.is_empty(),
        "borrow ends with the call; whole-struct move after is fine: {:?}",
        diags
    );
}

#[test]
fn whole_struct_move_consumes() {
    let src = "struct Person:\n\tname: str\n\nfn main():\n\tp = Person{name=\"alice\"}\n\tq = p\n\tr = p\n";
    let diags = check_src(src);
    assert!(
        diags.iter().any(|d| d.code == DiagCode::UseAfterMove),
        "got {:?}",
        diags
    );
}

#[test]
fn str_field_reassign_frees_old_value() {
    let src = "struct Person:\n\tname: str\n\nfn main():\n\tmut p = Person{name=\"alice\"}\n\tp.name = \"bob\"\n";
    let (diags, mut sidecar, _tirs, _pool) = check_src_full(src);
    assert!(diags.is_empty(), "got {:?}", diags);
    let f = take_function_sidecar(&mut sidecar, 0);
    assert!(f.field_free_on_reassign.iter().any(|e| e.is_some()));
}

#[test]
fn field_inout_borrow_accepted_no_diags() {
    // `inc(&p.x)` on a `mut` root is a valid inout pass (M9): the
    // field borrow is call-scoped, the root stays usable afterwards.
    let src = "struct Point:\n\tx: int\n\ty: int\n\nfn inc(inout v: int):\n\tv += 1\n\nfn main():\n\tmut p = Point{x=1, y=2}\n\tinc(&p.x)\n\tq = p.y\n";
    let diags = check_src(src);
    assert!(diags.is_empty(), "got {diags:?}");
}

#[test]
fn two_field_inout_borrows_same_root_rejected() {
    // set2(&p.x, &p.y) — distinct fields, but ONE root owner: two
    // mutable borrows of it in the same call (Rule 7 case 1).
    let src = "struct Point:\n\tx: int\n\ty: int\n\nfn set2(inout a: int, inout b: int):\n\ta = b\n\nfn main():\n\tmut p = Point{x=1, y=2}\n\tset2(&p.x, &p.y)\n";
    let diags = check_src(src);
    let count = diags
        .iter()
        .filter(|d| d.code == DiagCode::MutableAliasingViolation)
        .count();
    assert_eq!(count, 1, "got {diags:?}");
}

#[test]
fn field_inout_plus_whole_struct_borrow_rejected() {
    // f(&p.x, p) — a mutable borrow of the root via the field plus an
    // immutable borrow of the whole struct (Rule 7 case 2).
    let src = "struct Point:\n\tx: int\n\ty: int\n\nfn f(inout a: int, q: Point):\n\ta = q.y\n\nfn main():\n\tmut p = Point{x=1, y=2}\n\tf(&p.x, p)\n";
    let diags = check_src(src);
    assert!(
        diags
            .iter()
            .any(|d| d.code == DiagCode::MutableAliasingViolation),
        "got {diags:?}"
    );
}

#[test]
fn field_inout_plus_whole_struct_move_rejected() {
    // f(&p.name, p) with a move-mode param — mutable borrow of the
    // root via the field plus a whole-struct move (Rule 7 case 3).
    let src = "struct Person:\n\tname: str\n\nfn f(inout s: str, move q: Person):\n\tprint(s)\n\nfn main():\n\tmut p = Person{name=\"alice\"}\n\tf(&p.name, p)\n";
    let diags = check_src(src);
    assert!(
        diags
            .iter()
            .any(|d| d.code == DiagCode::MutableAliasingViolation),
        "got {diags:?}"
    );
}

#[test]
fn field_inout_rule7_names_root_binding() {
    // E0032 must name the root binding `p`, not the generic "value".
    let src = "struct Point:\n\tx: int\n\ty: int\n\nfn set2(inout a: int, inout b: int):\n\ta = b\n\nfn main():\n\tmut p = Point{x=1, y=2}\n\tset2(&p.x, &p.y)\n";
    let diags = check_src(src);
    let msg = &diags
        .iter()
        .find(|d| d.code == DiagCode::MutableAliasingViolation)
        .expect("E0032 must fire")
        .message;
    assert!(msg.contains("`p`"), "E0032 must name `p`; got: {msg}");
}

#[test]
fn inout_field_and_copy_field_read_same_root_rejected() {
    // f(&p.x, p.y) — inout borrow of the root via a field plus a
    // Copy-typed field read of the same root in one call. The coarse
    // root-freeze rule (an `inout` of any field borrows the whole
    // struct for the call) must flag it, same as f(&p.x, p).
    let src = "struct Point:\n\tx: float\n\ty: float\n\nfn f(inout a: float, b: float):\n\ta += b\n\nfn main():\n\tmut p = Point{x=1.0, y=2.0}\n\tf(&p.x, p.y)\n\tprint(float_to_str(p.x))\n";
    let diags = check_src(src);
    assert!(
        diags
            .iter()
            .any(|d| d.code == DiagCode::MutableAliasingViolation),
        "expected E0032 MutableAliasingViolation; got {diags:?}"
    );
}

#[test]
fn inout_field_and_copy_field_read_different_roots_ok() {
    // f(&p.x, q.y) — different roots, no overlap: must NOT fire.
    let src = "struct Point:\n\tx: float\n\ty: float\n\nfn f(inout a: float, b: float):\n\ta += b\n\nfn main():\n\tmut p = Point{x=1.0, y=2.0}\n\tq = Point{x=3.0, y=4.0}\n\tf(&p.x, q.y)\n\tprint(float_to_str(p.x))\n";
    let diags = check_src(src);
    assert!(
        !diags
            .iter()
            .any(|d| d.code == DiagCode::MutableAliasingViolation),
        "no E0032 expected for different roots; got {diags:?}"
    );
}
