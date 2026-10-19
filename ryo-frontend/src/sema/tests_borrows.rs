//! Borrow/`inout` sema tests: the `&x` borrow expression typechecks to
//! the inner type, borrows are rejected outside call-argument position
//! and on immutable/move-already targets, and builtin signatures
//! (`str_push`, `print`, conversion builtins) reject borrow arguments.
//! Harness shared with `tests.rs` via its `pub(super)` helpers (the
//! 2000-line file cap keeps these out of `tests.rs`).

use super::tests::*;
use super::*;

#[test]
fn inout_param_is_assignable() {
    // Mutating an inout param by name must NOT raise ImmutableAssign.
    let (_tirs, diags, _pool) = run_with_errors("fn inc(inout x: int):\n\tx += 1\n");
    assert!(
        !any_code(&diags, DiagCode::ImmutableAssign),
        "inout param must be mutable; got {:?}",
        diags
    );
}

#[test]
fn borrow_typechecks_to_inner_type() {
    // &c where c: int and callee expects inout int — no type error.
    let (_tirs, diags, _pool) =
        run_with_errors("fn inc(inout x: int):\n\tx += 1\nfn main():\n\tmut c = 0\n\tinc(&c)\n");
    assert!(
        !any_code(&diags, DiagCode::TypeMismatch),
        "&c into inout int must typecheck; got {:?}",
        diags
    );
}

#[test]
fn borrow_without_inout_rejected() {
    // `&c` passed to a plain (non-inout) `int` parameter.
    let (_tirs, diags, _pool) = run_with_errors(
        "fn f(x: int):\n\tprint(int_to_str(x))\nfn main():\n\tmut c = 0\n\tf(&c)\n",
    );
    assert!(
        any_code(&diags, DiagCode::BorrowMismatch),
        "&c into a non-inout param must be rejected; got {:?}",
        diags
    );
}

#[test]
fn inout_without_borrow_rejected() {
    // plain `c` passed to an `inout int` parameter.
    let (_tirs, diags, _pool) =
        run_with_errors("fn inc(inout x: int):\n\tx += 1\nfn main():\n\tmut c = 0\n\tinc(c)\n");
    assert!(
        any_code(&diags, DiagCode::BorrowMismatch),
        "inout param without & must be rejected; got {:?}",
        diags
    );
}

#[test]
fn borrow_of_immutable_rejected() {
    // `&c` where c is not declared `mut`.
    let (_tirs, diags, _pool) =
        run_with_errors("fn inc(inout x: int):\n\tx += 1\nfn main():\n\tc = 0\n\tinc(&c)\n");
    assert!(
        any_code(&diags, DiagCode::BorrowMismatch),
        "& of an immutable binding must be rejected; got {:?}",
        diags
    );
}

#[test]
fn borrow_of_mut_local_ok() {
    // `&c` where c is `mut` — the happy path.
    let (_tirs, diags, _pool) =
        run_with_errors("fn inc(inout x: int):\n\tx += 1\nfn main():\n\tmut c = 0\n\tinc(&c)\n");
    assert!(
        !any_code(&diags, DiagCode::BorrowMismatch),
        "& of a mut local must be allowed; got {:?}",
        diags
    );
}

#[test]
fn borrow_outside_call_rejected() {
    // `x = &c` — a `&` that is not a call argument is not a
    // mutation marker for anything; it must be an error, not a
    // silent no-op.
    let (_tirs, diags, _pool) = run_with_errors("fn main():\n\tmut c = 5\n\tx = &c\n");
    assert!(
        any_code(&diags, DiagCode::BorrowMismatch),
        "& outside call-argument position must be rejected; got {:?}",
        diags
    );
}

#[test]
fn borrow_nested_in_binop_rejected() {
    // `inc(1 + &c)` — the `&` is buried inside an expression;
    // it is not the call argument itself, so it is rejected here (and
    // check_call separately reports the missing `&` on the inout arg).
    let (_tirs, diags, _pool) = run_with_errors(
        "fn inc(inout x: int):\n\tx += 1\nfn main():\n\tmut c = 1\n\tinc(1 + &c)\n",
    );
    assert!(
        any_code(&diags, DiagCode::BorrowMismatch),
        "& nested inside a larger expression must be rejected; got {:?}",
        diags
    );
}

#[test]
fn borrow_as_inout_arg_still_ok() {
    // Regression guard: a direct call argument keeps working.
    let (_tirs, diags, _pool) =
        run_with_errors("fn inc(inout x: int):\n\tx += 1\nfn main():\n\tmut c = 0\n\tinc(&c)\n");
    assert!(
        !any_code(&diags, DiagCode::BorrowMismatch),
        "& as a direct inout call argument must stay valid; got {:?}",
        diags
    );
}

#[test]
fn borrow_of_borrowed_param_rejected() {
    // Review coverage: `&` on a plain (borrowed) param — immutable.
    let (_tirs, diags, _pool) =
        run_with_errors("fn f(s: str):\n\tstr_push(&s, \"x\")\nfn main():\n\tf(\"hi\")\n");
    assert!(
        any_code(&diags, DiagCode::BorrowMismatch),
        "& of a borrowed param must be rejected; got {:?}",
        diags
    );
}

#[test]
fn borrow_of_move_param_rejected() {
    // Review coverage: `&` on a `move` param — immutable binding.
    let (_tirs, diags, _pool) =
        run_with_errors("fn f(move s: str):\n\tstr_push(&s, \"x\")\nfn main():\n\tf(\"hi\")\n");
    assert!(
        any_code(&diags, DiagCode::BorrowMismatch),
        "& of a move param must be rejected; got {:?}",
        diags
    );
}

#[test]
fn borrow_of_for_loop_var_rejected() {
    // Review coverage: `&` on a for-loop variable — immutable,
    // block-scoped.
    let (_tirs, diags, _pool) = run_with_errors(
        "fn inc(inout x: int):\n\tx += 1\nfn main():\n\tfor i in range(0, 3):\n\t\tinc(&i)\n",
    );
    assert!(
        any_code(&diags, DiagCode::BorrowMismatch),
        "& of a for-loop variable must be rejected; got {:?}",
        diags
    );
}

#[test]
fn str_push_immutable_first_arg_rejected() {
    // Review coverage: str_push replays the lvalue check (builtins
    // bypass check_call) — `&s` with a non-`mut` s must be E0033.
    let (_tirs, diags, _pool) =
        run_with_errors("fn main():\n\ts = \"hi\"\n\tstr_push(&s, \"x\")\n");
    assert!(
        any_code(&diags, DiagCode::BorrowMismatch),
        "str_push(&immutable, ..) must be rejected; got {:?}",
        diags
    );
}

#[test]
fn print_with_borrow_arg_rejected() {
    // `print(&s)` — print's parameter is not `inout`, so the `&`
    // marker must be rejected, not silently discarded.
    let (_tirs, diags, _pool) = run_with_errors("fn main():\n\tmut s = \"hi\"\n\tprint(&s)\n");
    assert!(
        any_code(&diags, DiagCode::BorrowMismatch),
        "print(&s) must be rejected (param is not inout); got {:?}",
        diags
    );
}

#[test]
fn print_rejected_type_message_lists_accepted_types() {
    // `print(print("x"))` — the inner print is `void`, not a printable
    // type; the TypeMismatch message pins the full accept set (M10
    // added anonymous structs / tuples; M11 added enums).
    let (_tirs, diags, _pool) = run_with_errors("fn main():\n\tprint(print(\"x\"))\n");
    assert!(
        any_code(&diags, DiagCode::TypeMismatch),
        "print(void) must be rejected; got {diags:?}"
    );
    assert_eq!(
        first_msg(&diags),
        "print() argument must be str, strview, bytes, bytesview, int, float, bool, struct, enum, or tuple, got void"
    );
}

#[test]
fn conversion_builtin_with_borrow_arg_rejected() {
    // `int_to_str(&c)` — conversion builtins are not `inout` either.
    let (_tirs, diags, _pool) =
        run_with_errors("fn main():\n\tmut c = 1\n\tprint(int_to_str(&c))\n");
    assert!(
        any_code(&diags, DiagCode::BorrowMismatch),
        "int_to_str(&c) must be rejected; got {:?}",
        diags
    );
}

#[test]
fn str_push_borrow_suffix_rejected() {
    // `str_push(&s, &x)` — the suffix parameter is Borrow, not
    // `inout`, so `&` on it must be rejected through the same check.
    let (_tirs, diags, _pool) =
        run_with_errors("fn main():\n\tmut s = \"hi\"\n\tmut x = \"yo\"\n\tstr_push(&s, &x)\n");
    assert!(
        any_code(&diags, DiagCode::BorrowMismatch),
        "str_push's suffix must reject `&`; got {:?}",
        diags
    );
}

#[test]
fn str_push_valid_still_ok() {
    // Regression guard: the valid form stays clean.
    let (_tirs, diags, _pool) =
        run_with_errors("fn main():\n\tmut s = \"hi\"\n\tstr_push(&s, \"x\")\n");
    assert!(
        !any_code(&diags, DiagCode::BorrowMismatch),
        "str_push(&s, \"x\") must stay valid; got {:?}",
        diags
    );
}
