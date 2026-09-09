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
