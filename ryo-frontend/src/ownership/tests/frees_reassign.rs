use super::super::*;
use super::common::*;

#[test]
fn loop_carried_struct_reassign_pre_owner_not_double_freed() {
    // `mut child = f(); while c: child = g()` with child never read
    // after the loop (the natural do-while parser-loop shape): the
    // pre-reassign owner's last-use Free must be suppressed — the
    // reassign-Free releases it at the first displacing store, and the
    // loop-exit Free of the reseated value (codegen redirects it to
    // the binding's home slot) releases the final buffer. Pre-fix the
    // owner was scheduled BOTH a last-use Free and the reassign-Free,
    // and the return epilogue double-freed the final value through the
    // same home slot — natural do-while parser loops (a JSON
    // parse_array) SIGABRT'd on exactly this composition.
    let src = r#"
struct P:
	text: str
	next: int

fn make(x: int) -> P:
	return P{text=int_to_str(x) + "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", next=x}

fn main():
	mut i = 0
	mut child = make(1)
	while i < 2:
		child = make(2)
		i += 1
	print("done")
"#;
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

    let child = pool.intern_str("child");
    let mut pre_owner = None;
    let mut in_loop_value = None;
    let mut assign_ref = None;
    for r in 1..tir.instructions.len() {
        let rr = TirRef::from_raw(r as u32);
        match tir.instructions[r].tag {
            TirTag::VarDecl => {
                let v = tir.var_decl_view(rr);
                if v.name == child {
                    pre_owner = Some(v.initializer);
                }
            }
            TirTag::Assign => {
                let v = tir.assign_view(rr);
                if v.name == child {
                    assign_ref = Some(rr);
                    in_loop_value = Some(v.value);
                }
            }
            _ => {}
        }
    }
    let pre_owner = pre_owner.expect("child decl found");
    let in_loop_value = in_loop_value.expect("child reassign found");
    let assign_ref = assign_ref.expect("child assign found");

    // Displacement coverage exists.
    assert_eq!(
        sc.free_on_reassign[assign_ref.index()],
        Some(pre_owner),
        "reassign-Free must release the pre-reassign owner"
    );
    // The pre-reassign owner has NO scheduled Free: its only release
    // is the reassign-Free (loop ran) or the loop-exit slot redirect
    // (zero iterations) — never both, never a last-use Free anchored
    // before the reassign.
    assert!(
        !sc.drops.iter().any(|fp| fp.target == pre_owner),
        "pre-reassign owner must not be in free_schedule: {:?}",
        sc.drops
    );
    // The final buffer gets exactly one release: the loop-exit Free of
    // the reseated value (redirected to the home slot by codegen).
    let final_frees = sc
        .drops
        .iter()
        .filter(|fp| fp.target == in_loop_value)
        .count();
    assert_eq!(
        final_frees, 1,
        "reseated value must be freed exactly once: {:?}",
        sc.drops
    );
}

#[test]
fn shadowed_binding_reassign_does_not_suppress_outer_owner_free() {
    // A same-named shadow is a DIFFERENT binding: its reassigns must
    // not enter the outer binding's reassign set. Pre-fix the
    // anchor-order suppression grouped reassigns by name, so the
    // shadow's `x = "s1"` — anchored after the outer owner's last use —
    // suppressed the outer owner's only surviving Free. The outer
    // buffer then leaked on every run (the `if false:` reassign-Free
    // never fires to release it).
    let src = "fn main():\n\tmut x = \"outer_a\"\n\tif false:\n\t\tx = \"outer_b\"\n\tprint(x)\n\tif true:\n\t\tmut x = \"s0\"\n\t\tx = \"s1\"\n\tprint(\"end\")\n";
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
    // free_on_reassign covers the taken path only; a scheduled Free
    // must cover the not-taken path. Pre-fix there was none.
    assert!(
        sc.drops.iter().any(|fp| fp.target == outer_init),
        "outer owner must have a scheduled Free (the shadow's reassign \
         must not suppress it): {:?}",
        sc.drops
    );
}

#[test]
fn sibling_scope_bindings_do_not_share_reassign_sets() {
    // Same failure mode without shadowing: two sibling scopes declare
    // their own `x`. An arm's reassign must not count toward the other
    // arm's binding suppression set.
    let src = "fn main():\n\tif true:\n\t\tmut x = \"a1\"\n\t\tx = \"a2\"\n\telse:\n\t\tmut x = \"b1\"\n\t\tx = \"b2\"\n\tprint(1)\n";
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

    // Every arm value must be released exactly through the machinery
    // (reassign-Free or a scheduled Free); in particular the b-arm's
    // values must not be dropped from scheduling by the a-arm's
    // same-name reassigns.
    let x = pool.intern_str("x");
    let mut values = Vec::new();
    for r in 1..tir.instructions.len() {
        if tir.instructions[r].tag == TirTag::VarDecl {
            let v = tir.var_decl_view(TirRef::from_raw(r as u32));
            if v.name == x {
                values.push(v.initializer);
            }
        }
    }
    assert_eq!(values.len(), 2, "two sibling bindings of x");
    for v in values {
        let freed =
            sc.drops.iter().any(|fp| fp.target == v) || sc.free_on_reassign.contains(&Some(v));
        assert!(
            freed,
            "sibling binding value {v:?} must be freed: {:?}",
            sc.drops
        );
    }
}

#[test]
fn early_return_after_loop_carried_reassign_keeps_outer_epilogue_free() {
    // `while ...: s = g(); ...; if n > 1: return s; return s + "!"` —
    // a Free anchored at the INNER return must not count as covering
    // the OUTER return's epilogue Free for the same binding:
    // `collect_jump_path` marks a non-containing loop's whole subtree
    // on-path, but codegen cannot sweep after a terminator, so an
    // inner-return-anchored Free never fires on the outer return's
    // path. When the binding-covering check accepted it, the epilogue
    // Free was suppressed and codegen's leak-direction assert tripped
    // ("frees anchored to unmaterialized instructions were dropped").
    let src = r#"
fn make(x: int) -> str:
	return int_to_str(x) + "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

fn f(n: int) -> str:
	mut s = make(0)
	mut i = 0
	while i < n:
		s = make(1)
		i += 1
	if n > 1:
		return s
	return s + "!"

fn main():
	print(f(3))
"#;
    let (diags, mut sidecar, tirs, mut pool) = check_src_full(src);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != ryo_core::diag::Severity::Error),
        "expected no errors: {diags:?}"
    );
    let idx = tirs.iter().position(|t| pool.str(t.name) == "f").unwrap();
    let tir = &tirs[idx];
    let sc = take_function_sidecar(&mut sidecar, idx);

    let mut outer_return = None;
    let mut inner_return = None;
    for r in 1..tir.instructions.len() {
        if tir.instructions[r].tag == TirTag::Return {
            let rr = TirRef::from_raw(r as u32);
            match inner_return {
                None => inner_return = Some(rr),
                _ => outer_return = Some(rr),
            }
        }
    }
    let outer_return = outer_return.expect("two returns (inner if-arm, outer fallthrough)");
    let s = pool.intern_str("s");
    let outer_epilogue = sc.drops.iter().any(|fp| {
        fp.position.anchor_inst().unwrap() == outer_return
            && (tir.instructions[fp.target.index()].tag == TirTag::Call
                || tir.instructions[fp.target.index()].tag == TirTag::Var)
            && {
                // the free targets a value of `s` (decl init or reassign value)
                let mut found = false;
                for rr2 in 1..tir.instructions.len() {
                    let t2 = TirRef::from_raw(rr2 as u32);
                    match tir.instructions[rr2].tag {
                        TirTag::VarDecl => {
                            let v = tir.var_decl_view(t2);
                            if v.name == s && v.initializer == fp.target {
                                found = true;
                            }
                        }
                        TirTag::Assign => {
                            let v = tir.assign_view(t2);
                            if v.name == s && v.value == fp.target {
                                found = true;
                            }
                        }
                        _ => {}
                    }
                }
                found
            }
    });
    assert!(
        outer_epilogue,
        "the outer return's epilogue Free for `s` must survive \
         (inner-return-anchored frees do not cover it): {:?}",
        sc.drops
    );
    let _ = inner_return;
}
