//! Reserved-name enforcement — sema tests: the `__ryo_` prefix and the
//! `range` builtin are rejected at EVERY user binding-declaration path
//! (function name, variable, parameter, loop variable, pattern
//! binding, rename target). The prefix reservation is what makes the
//! destructuring temp side table sound: user source can never mint a
//! name the compiler also mints, so the temp lookup that resolves
//! before the scope walk can never hijack a user binding.

use super::tests::*;
use super::*;

#[test]
fn reserved_ryo_prefix_rejected() {
    let errors = run_with_errors("fn __ryo_hack():\n\tprint(\"nope\")\n").1;
    assert!(any_code(&errors, DiagCode::ReservedIdentifier));
}

#[test]
fn reserved_ryo_prefix_rejected_in_all_binding_forms() {
    // The reservation must cover EVERY user binding path, not just
    // function names — a temp-colliding user binding would otherwise
    // be shadowed by the destructure side table.
    let cases = [
        (
            "variable",
            "fn main():\n\t__ryo_destructure_0 = 5\n\tprint(__ryo_destructure_0)\n",
        ),
        (
            "parameter",
            "fn f(__ryo_p: int) -> int:\n\treturn __ryo_p\n\nfn main():\n\tprint(f(1))\n",
        ),
        (
            "loop variable",
            "fn main():\n\tfor __ryo_i in range(0, 3):\n\t\tprint(1)\n",
        ),
        (
            "pattern binding",
            "fn main():\n\t(__ryo_a, b) = (1, \"x\")\n\tprint(b)\n",
        ),
        (
            "rename target",
            "fn main():\n\t{q = __ryo_b, r = _} = {q=1, r=2}\n\tprint(__ryo_b)\n",
        ),
    ];
    for (form, src) in cases {
        let diags = run_with_errors(src).1;
        assert!(
            any_code(&diags, DiagCode::ReservedIdentifier),
            "{form} binding should reject the '__ryo_' prefix, got: {diags:?}"
        );
    }
}

#[test]
fn destructure_temp_hijack_program_rejected() {
    // The exact demonstration program must now fail at the
    // declaration instead of silently binding the wrong value.
    let src = "fn f() -> {p: int, q: {u: int, v: int}}:\n\
               \treturn {p=1, q={u=2, v=3}}\n\n\
               fn main():\n\
               \t__ryo_destructure_0 = 5\n\
               \t(a, (b, c)) = f()\n\
               \tprint(__ryo_destructure_0)\n";
    let diags = run_with_errors(src).1;
    assert!(
        any_code(&diags, DiagCode::ReservedIdentifier),
        "the hijack program must be rejected, got: {diags:?}"
    );
}
