//! Same-name shadow bindings vs the outer binding's cleanup: the
//! free-schedule side of the taken-arm reseat + shadow double-free/leak
//! (the codegen redirect side lives in ryo-backend/src/codegen/frees.rs).

use super::super::*;
use super::common::*;

#[test]
fn shadow_scope_dead_reseat_mints_no_outer_dead_drop() {
    // The mirror composition: TAKEN-arm reseat of the outer binding,
    // read once, then a same-named shadow scope whose own reseat value
    // is never read (dead store). The shadow's dead reseat value is a
    // different BINDING: honoring the if's reseat record against it
    // minted a ConditionalDeadDrop targeting the OUTER binding's home
    // slot — a slot the outer owner's last-use Free (which redirects
    // through that slot) already released. The drop double-freed the
    // slot's current buffer on every path where it fired. No
    // ConditionalDeadDrop may target the outer owner here.
    let src = "fn main():\n\tmut x = \"outer_a\"\n\tif true:\n\t\tx = \"outer_b\"\n\tprint(x)\n\tif true:\n\t\tmut x = \"s0\"\n\t\tx = \"s1\"\n\tprint(\"end\")\n";
    let (diags, mut sidecar, tirs, mut pool) = check_src_full(src);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != ryo_core::diag::Severity::Error),
        "expected no errors: {diags:?}"
    );
    let idx = tirs
        .iter()
        .position(|t| pool.str(t.name) == "main")
        .unwrap();
    let tir = &tirs[idx];
    let sc = take_function_sidecar(&mut sidecar, idx);

    let x = pool.intern_str("x");
    let mut outer_init = None;
    for r in 1..tir.instructions.len() {
        if tir.instructions[r].tag == TirTag::VarDecl {
            let v = tir.var_decl_view(TirRef::from_raw(r as u32));
            if v.name == x && outer_init.is_none() {
                outer_init = Some(v.initializer);
            }
        }
    }
    let outer_init = outer_init.expect("outer decl found");
    assert!(
        sc.free_schedule.iter().any(|fp| fp.target == outer_init),
        "outer owner must keep its last-use Free: {:?}",
        sc.free_schedule
    );
    assert!(
        !sc.conditional_dead_drops
            .iter()
            .any(|d| d.target == outer_init),
        "the shadow binding's dead reseat value must not mint a dead \
         drop against the outer owner's home slot: {:?}",
        sc.conditional_dead_drops
    );
}

#[test]
fn arm_reseat_then_shadow_keeps_fallthrough_drop() {
    // `if c: x = "b"; mut x = "c"` — one arm reseats the pre-branch
    // binding and THEN shadows the name. The arm-end owner belongs to
    // the shadow, but the pre-branch binding was still reseated in
    // that arm: the record must capture that earlier reseat (scanning
    // the arm's top-level statements) so the dead-store drain honors
    // it and drops the pre-branch buffer "a" on the fall-through path.
    // Name-only honoring (via the shadow's dead value) minted the same
    // drop by accident; binding-aware honoring without the scan lost
    // it, leaking "a" whenever the arm did not run.
    let src = "fn main():\n\tmut x = \"a\"\n\tif true:\n\t\tx = \"b\"\n\t\tmut x = \"c\"\n\tprint(\"end\")\n";
    let (diags, mut sidecar, tirs, mut pool) = check_src_full(src);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != ryo_core::diag::Severity::Error),
        "expected no errors: {diags:?}"
    );
    let idx = tirs
        .iter()
        .position(|t| pool.str(t.name) == "main")
        .unwrap();
    let tir = &tirs[idx];
    let sc = take_function_sidecar(&mut sidecar, idx);

    let x = pool.intern_str("x");
    let mut outer_init = None;
    for r in 1..tir.instructions.len() {
        if tir.instructions[r].tag == TirTag::VarDecl {
            let v = tir.var_decl_view(TirRef::from_raw(r as u32));
            if v.name == x && outer_init.is_none() {
                outer_init = Some(v.initializer);
            }
        }
    }
    let outer_init = outer_init.expect("outer decl found");
    assert_eq!(
        sc.conditional_dead_drops
            .iter()
            .filter(|d| d.target == outer_init)
            .count(),
        1,
        "reseat-then-shadow arm must still mint the pre-branch \
         fall-through drop: {:?}",
        sc.conditional_dead_drops
    );
}

#[test]
fn nested_reseat_then_shadow_keeps_fallthrough_drop() {
    // `if c1: if c2: x = "b"; mut x = "c"` — the arm reseats the
    // pre-branch binding inside a NESTED conditional and then shadows
    // the name. The arm scan must descend into the nested body: the
    // reseat belongs to the enclosing arm's record (the nested if's own
    // drop only covers its fall-through), so without the descent the
    // pre-branch buffer "a" leaked whenever the ENCLOSING arm was
    // skipped (64 bytes under Valgrind with heap strings).
    let src = "fn main():\n\tmut x = \"a\"\n\tif true:\n\t\tif true:\n\t\t\tx = \"b\"\n\t\tmut x = \"c\"\n\tprint(\"end\")\n";
    let (diags, mut sidecar, tirs, mut pool) = check_src_full(src);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != ryo_core::diag::Severity::Error),
        "expected no errors: {diags:?}"
    );
    let idx = tirs
        .iter()
        .position(|t| pool.str(t.name) == "main")
        .unwrap();
    let tir = &tirs[idx];
    let sc = take_function_sidecar(&mut sidecar, idx);

    let x = pool.intern_str("x");
    let mut outer_init = None;
    for r in 1..tir.instructions.len() {
        if tir.instructions[r].tag == TirTag::VarDecl {
            let v = tir.var_decl_view(TirRef::from_raw(r as u32));
            if v.name == x && outer_init.is_none() {
                outer_init = Some(v.initializer);
            }
        }
    }
    let outer_init = outer_init.expect("outer decl found");
    assert!(
        sc.conditional_dead_drops
            .iter()
            .any(|d| d.target == outer_init),
        "nested reseat-then-shadow arm must still mint the pre-branch \
         fall-through drop: {:?}",
        sc.conditional_dead_drops
    );
}

#[test]
fn conditional_dead_reassign_still_minted_for_same_binding() {
    // Binding-aware honoring must not weaken the genuine shape: a dead
    // reseat of the SAME binding still honors the record and drops the
    // pre-branch buffer on the untouched arms.
    let src = "fn main():\n\tmut x = \"a\"\n\tif true:\n\t\tx = \"b\"\n\tprint(\"end\")\n";
    let (diags, mut sidecar, tirs, mut pool) = check_src_full(src);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != ryo_core::diag::Severity::Error),
        "expected no errors: {diags:?}"
    );
    let idx = tirs
        .iter()
        .position(|t| pool.str(t.name) == "main")
        .unwrap();
    let tir = &tirs[idx];
    let sc = take_function_sidecar(&mut sidecar, idx);

    let x = pool.intern_str("x");
    let mut outer_init = None;
    for r in 1..tir.instructions.len() {
        if tir.instructions[r].tag == TirTag::VarDecl {
            let v = tir.var_decl_view(TirRef::from_raw(r as u32));
            if v.name == x && outer_init.is_none() {
                outer_init = Some(v.initializer);
            }
        }
    }
    let outer_init = outer_init.expect("outer decl found");
    assert_eq!(
        sc.conditional_dead_drops
            .iter()
            .filter(|d| d.target == outer_init)
            .count(),
        1,
        "same-binding dead reseat must still mint the fall-through drop: {:?}",
        sc.conditional_dead_drops
    );
}
