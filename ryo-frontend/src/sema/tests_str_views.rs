//! M8.4 `strview` sema tests: slices and shorthand forms yield views,
//! view params/annotations/bindings and mixed `str`/`strview` equality,
//! view misuse (returns, concat, int slices, non-int bounds) is
//! rejected, builtins accept views, and `str(...)` materialization plus
//! its W0003 redundant-materialize warnings. Harness shared with
//! `tests.rs` via its `pub(super)` helpers (the 2000-line file cap
//! keeps these out of `tests.rs`).

use super::tests::*;
use super::*;
use ryo_core::tir::TirTag;

// ---- M8.4: strview slices / views (final spec §3) ----

#[test]
fn slice_yields_str_view() {
    let (tirs, diags, _pool) = run_with_errors("fn main():\n\ts: str = \"hello\"\n\tx = s[1:3]\n");
    assert!(diags.is_empty(), "got {:?}", diags);
    let main = &tirs[0];
    assert!(main.instructions.iter().any(|i| i.tag == TirTag::Slice));
}

#[test]
fn slice_shorthand_forms_yield_views() {
    // `s[a:]`, `s[:b]`, `s[:]` — bounds omitted (§3.1).
    let (tirs, diags, pool) =
        run_with_errors("fn main():\n\ts: str = \"hello\"\n\ta = s[1:]\n\tb = s[:2]\n\tc = s[:]\n");
    assert!(diags.is_empty(), "got {:?}", diags);
    let main = tir_named(&tirs, &pool, "main");
    let slices: Vec<_> = main
        .instructions
        .iter()
        .filter(|i| i.tag == TirTag::Slice)
        .collect();
    assert_eq!(slices.len(), 3, "one Slice inst per slice expression");
    assert!(
        slices.iter().all(|i| i.ty == pool.str_view()),
        "every slice is typed strview"
    );
}

#[test]
fn view_param_declared_type_and_borrow_mode() {
    let (tirs, diags, pool) = run_with_errors("fn f(x: strview):\n\tprint(x)\n");
    assert!(diags.is_empty(), "got {:?}", diags);
    let f = tir_named(&tirs, &pool, "f");
    assert_eq!(f.params[0].ty, pool.str_view());
    assert_eq!(f.params[0].mode, ParamMode::Borrow);
}

#[test]
fn view_param_accepts_owned_str_via_conversion() {
    let (tirs, diags, pool) = run_with_errors(
        "fn shout(text: strview):\n\tprint(text)\n\nfn main():\n\ts: str = \"hi\"\n\tshout(s)\n",
    );
    assert!(diags.is_empty(), "got {:?}", diags);
    let main = tir_named(&tirs, &pool, "main");
    assert!(
        main.instructions.iter().any(|i| i.tag == TirTag::ToView),
        "owned str → strview param must insert a ToView conversion (§3.4)"
    );
}

#[test]
fn view_param_accepts_view_arg_directly() {
    // E4: a view argument to a view parameter needs no conversion.
    let (tirs, diags, pool) = run_with_errors(
        "fn shout(text: strview):\n\tprint(text)\n\nfn main():\n\ts: str = \"hi\"\n\tshout(s[0:1])\n",
    );
    assert!(diags.is_empty(), "got {:?}", diags);
    let main = tir_named(&tirs, &pool, "main");
    assert!(
        !main.instructions.iter().any(|i| i.tag == TirTag::ToView),
        "view → view must not insert ToView"
    );
}

#[test]
fn view_passes_to_owned_str_param_via_reborrow() {
    // P6': a view passed to an owned `str` parameter re-borrows —
    // sema inserts a ViewAsOwner conversion (cap=0 triple at codegen,
    // no allocation, call-scoped).
    let (tirs, diags, pool) = run_with_errors(
        "fn show(s: str):\n\tprint(s)\n\nfn main():\n\ts: str = \"hi\"\n\tshow(s[0:1])\n",
    );
    assert!(diags.is_empty(), "got {:?}", diags);
    let main = tir_named(&tirs, &pool, "main");
    assert!(
        main.instructions
            .iter()
            .any(|i| i.tag == TirTag::ViewAsOwner),
        "strview → str param must insert a ViewAsOwner re-borrow (P6')"
    );
}

#[test]
fn view_binding_to_str_still_rejected() {
    // The re-borrow is call-scoped only: binding-form conversion
    // (`x: str = view`) stays E0012.
    let (_tirs, diags, _pool) =
        run_with_errors("fn main():\n\ts: str = \"hi\"\n\tx: str = s[0:1]\n");
    assert!(any_code(&diags, DiagCode::TypeMismatch), "got {:?}", diags);
}

#[test]
fn view_to_move_str_param_still_rejected() {
    // E2 (final spec §3.3): the re-borrow is borrow-mode only — a
    // `move` str parameter would let the view escape the call.
    let (_tirs, diags, _pool) = run_with_errors(
        "fn eat(move s: str):\n\tprint(s)\n\nfn main():\n\ts: str = \"hi\"\n\teat(s[0:1])\n",
    );
    assert!(any_code(&diags, DiagCode::TypeMismatch), "got {:?}", diags);
}

#[test]
fn view_return_rejected() {
    let (_tirs, diags, _pool) = run_with_errors("fn bad(s: str) -> strview:\n\treturn s[0:1]\n");
    assert!(
        any_code(&diags, DiagCode::ReturnBorrowedValue),
        "got {:?}",
        diags
    );
}

#[test]
fn move_and_inout_view_params_rejected() {
    let (_tirs, diags, _pool) = run_with_errors("fn f(move x: strview):\n\tprint(x)\n");
    assert!(
        diags
            .iter()
            .any(|d| d.code == DiagCode::TypeMismatch && d.message.contains("already a borrow")),
        "got {:?}",
        diags
    );
    let (_tirs, diags, _pool) = run_with_errors("fn g(inout x: strview):\n\tprint(x)\n");
    assert!(
        diags
            .iter()
            .any(|d| d.code == DiagCode::TypeMismatch && d.message.contains("already a borrow")),
        "got {:?}",
        diags
    );
}

#[test]
fn owned_annotation_over_slice_rejected() {
    // P6: an owned copy of viewed contents needs an explicit copy
    // API (future work); plain annotation is a mismatch.
    let (_tirs, diags, _pool) =
        run_with_errors("fn main():\n\ts: str = \"hi\"\n\tx: str = s[0:1]\n");
    assert!(any_code(&diags, DiagCode::TypeMismatch), "got {:?}", diags);
}

#[test]
fn owned_to_view_binding_rejected() {
    let (_tirs, diags, _pool) =
        run_with_errors("fn main():\n\ts: str = \"hi\"\n\tx: strview = s\n");
    assert!(any_code(&diags, DiagCode::TypeMismatch), "got {:?}", diags);
}

#[test]
fn view_annotation_over_slice_ok() {
    let (tirs, diags, pool) =
        run_with_errors("fn main():\n\ts: str = \"hi\"\n\tx: strview = s[0:1]\n");
    assert!(diags.is_empty(), "got {:?}", diags);
    let main = tir_named(&tirs, &pool, "main");
    assert_eq!(main.inst(stmt_at(main, 1)).ty, pool.str_view());
}

#[test]
fn print_and_len_accept_views() {
    let (_tirs, diags, _pool) = run_with_errors(
        "fn main():\n\ts: str = \"hello\"\n\tprint(s[0:2])\n\tprint(int_to_str(s[0:2].len()))\n",
    );
    assert!(diags.is_empty(), "got {:?}", diags);
}

#[test]
fn is_empty_accepts_view() {
    let (_tirs, diags, _pool) = run_with_errors(
        "fn main():\n\ts: str = \"hi\"\n\tif s[0:1].is_empty():\n\t\tprint(\"e\")\n",
    );
    assert!(diags.is_empty(), "got {:?}", diags);
}

#[test]
fn mixed_str_view_equality_accepted() {
    let (_tirs, diags, _pool) =
        run_with_errors("fn main():\n\ts: str = \"he\"\n\tif s == s[0:2]:\n\t\tprint(\"eq\")\n");
    assert!(diags.is_empty(), "got {:?}", diags);
}

#[test]
fn mixed_equality_wraps_str_side_in_view() {
    // §3.3/§3.4: the owned side of a mixed comparison is converted.
    let (tirs, diags, pool) = run_with_errors("fn main():\n\ts: str = \"he\"\n\tx = s == s[0:2]\n");
    assert!(diags.is_empty(), "got {:?}", diags);
    let main = tir_named(&tirs, &pool, "main");
    assert!(main.instructions.iter().any(|i| i.tag == TirTag::ToView));
    assert!(main.instructions.iter().any(|i| i.tag == TirTag::StrCmpEq));
}

#[test]
fn view_view_equality_accepted() {
    let (tirs, diags, pool) = run_with_errors(
        "fn main():\n\ts: str = \"he\"\n\tif s[0:1] != s[1:2]:\n\t\tprint(\"ne\")\n",
    );
    assert!(diags.is_empty(), "got {:?}", diags);
    let main = tir_named(&tirs, &pool, "main");
    assert!(main.instructions.iter().any(|i| i.tag == TirTag::StrCmpNe));
    assert!(
        !main.instructions.iter().any(|i| i.tag == TirTag::ToView),
        "view vs view needs no conversion"
    );
}

#[test]
fn view_concat_rejected() {
    // `+` is owned-str concatenation only; a view operand falls
    // through to E0015 UnsupportedOperator.
    let (_tirs, diags, _pool) =
        run_with_errors("fn main():\n\ts: str = \"ab\"\n\tx = s[0:1] + s[0:1]\n");
    assert!(
        any_code(&diags, DiagCode::UnsupportedOperator),
        "got {:?}",
        diags
    );
}

#[test]
fn slice_of_int_rejected() {
    let (_tirs, diags, _pool) = run_with_errors("fn main():\n\tn: int = 5\n\tx = n[0:1]\n");
    assert!(any_code(&diags, DiagCode::TypeMismatch), "got {:?}", diags);
}

#[test]
fn slice_bound_must_be_int() {
    let (_tirs, diags, _pool) = run_with_errors("fn main():\n\ts: str = \"hi\"\n\tx = s[0.5:1]\n");
    assert!(any_code(&diags, DiagCode::TypeMismatch), "got {:?}", diags);
}

#[test]
fn str_push_accepts_view_suffix() {
    // 4b: str_push's suffix parameter is `strview`; a slice passes
    // directly, keeping [Inout, Borrow] modes.
    let (_tirs, diags, _pool) =
        run_with_errors("fn main():\n\tmut s = \"hi\"\n\tstr_push(&s, s[0:1])\n");
    assert!(diags.is_empty(), "got {:?}", diags);
}

#[test]
fn str_materialize_typechecks_to_owned_str() {
    // M8.4.1.2: `str(view)` materializes an owned copy — the result
    // type-checks as `str` and lowers to the synthesized
    // `__ryo_str_from_view` call.
    let (tirs, diags, pool) =
        run_with_errors("fn main():\n\ts: str = \"hi\"\n\tx: str = str(s[0:1])\n\tprint(x)\n");
    assert!(diags.is_empty(), "got {:?}", diags);
    let main = tir_named(&tirs, &pool, "main");
    let init = main.var_decl_view(stmt_at(main, 1)).initializer;
    let call = main.inst(init);
    assert!(matches!(call.tag, TirTag::Call), "got {:?}", call.tag);
    assert_eq!(call.ty, pool.str_(), "materialize result is owned str");
    assert_eq!(pool.str(main.call_view(init).name), "__ryo_str_from_view");
}

#[test]
fn str_materialize_rejects_non_view_arg() {
    // An owned `str` argument is NOT accepted — materializing it
    // would be a same-type copy, which is the future `Clone` trait's
    // job; the call form is `strview`-only.
    let (_tirs, diags, _pool) =
        run_with_errors("fn main():\n\ts: str = \"hi\"\n\tx: str = str(s)\n");
    assert!(any_code(&diags, DiagCode::TypeMismatch), "got {:?}", diags);
    assert!(
        first_msg(&diags).contains("str() argument must be strview"),
        "got {:?}",
        diags
    );
}

#[test]
fn str_materialize_arity_mismatch() {
    let (_tirs, diags, _pool) =
        run_with_errors("fn main():\n\ts: str = \"hi\"\n\tx: str = str(s[0:1], s[1:2])\n");
    assert!(any_code(&diags, DiagCode::ArityMismatch), "got {:?}", diags);
}

#[test]
fn str_materialize_borrow_arg_rejected() {
    // Builtins never take `inout`: `&` is rejected exactly like the
    // table builtins (mirrors `int_to_str(&c)`).
    let (_tirs, diags, _pool) =
        run_with_errors("fn main():\n\tmut s: str = \"hi\"\n\tx: str = str(&s)\n");
    assert!(
        any_code(&diags, DiagCode::BorrowMismatch),
        "got {:?}",
        diags
    );
}

#[test]
fn user_str_function_shadows_materialize_intercept() {
    // Least surprise: a user-defined `fn str` wins over the call-form
    // intercept — `str(41)` resolves to the user function, so an int
    // argument is fine and no strview diagnostic fires.
    let (_tirs, diags, _pool) = run_with_errors(
        "fn str(x: int) -> int:\n\treturn x + 1\n\nfn main():\n\ty: int = str(41)\n\tprint(int_to_str(y))\n",
    );
    assert!(diags.is_empty(), "got {:?}", diags);
}

#[test]
fn w0003_materialize_arg_to_borrowed_str_param_warns() {
    // W0003 case A: `str(view)` fed straight into a borrowed `str`
    // parameter is redundant — the view would pass via the P6'
    // re-borrow (cap=0, no allocation).
    let (_tirs, diags, _pool) = run_with_errors(
        "fn show(s: str):\n\tprint(s)\n\nfn main():\n\ts: str = \"hi\"\n\tshow(str(s[0:1]))\n",
    );
    assert_eq!(
        count_code(&diags, DiagCode::RedundantMaterialize),
        1,
        "expected exactly one W0003; got {:?}",
        diags
    );
}

#[test]
fn w0003_materialize_arg_to_move_param_does_not_warn() {
    // A `move` parameter cannot be served by the re-borrow — the
    // materialized copy is legitimate there.
    let (_tirs, diags, _pool) = run_with_errors(
        "fn eat(move s: str):\n\tprint(s)\n\nfn main():\n\ts: str = \"hi\"\n\teat(str(s[0:1]))\n",
    );
    assert!(
        !any_code(&diags, DiagCode::RedundantMaterialize),
        "got {:?}",
        diags
    );
}

#[test]
fn w0003_materialize_arg_to_inout_param_does_not_warn() {
    // An `inout` parameter cannot be served by the re-borrow either
    // (its argument requires `&`) — the copy is legitimate there.
    // The call below is rejected for the missing `&`; W0003 must
    // stay silent on that error path too.
    let (_tirs, diags, _pool) = run_with_errors(
        "fn eat(inout s: str):\n\tprint(s)\n\nfn main():\n\ts: str = \"hi\"\n\teat(str(s[0:1]))\n",
    );
    assert!(
        !any_code(&diags, DiagCode::RedundantMaterialize),
        "got {:?}",
        diags
    );
}

#[test]
fn w0003_materialize_arg_to_print_warns() {
    // W0003 case A, builtin shape: `print` accepts `strview`
    // arguments directly — materializing first buys nothing.
    let (_tirs, diags, _pool) =
        run_with_errors("fn main():\n\ts: str = \"hi\"\n\tprint(str(s[0:1]))\n");
    assert_eq!(
        count_code(&diags, DiagCode::RedundantMaterialize),
        1,
        "expected exactly one W0003; got {:?}",
        diags
    );
}

#[test]
fn w0003_materialize_arg_to_io_eprint_warns() {
    // W0003 case A, builtin shape: `io_eprint` (M9.2) has full `print`
    // type parity — it accepts `strview` arguments directly, so
    // materializing first is a redundant allocation.
    let (_tirs, diags, _pool) =
        run_with_errors("fn main():\n\ts: str = \"hi\"\n\tio_eprint(str(s[0:1]))\n");
    assert_eq!(
        count_code(&diags, DiagCode::RedundantMaterialize),
        1,
        "expected exactly one W0003; got {:?}",
        diags
    );
}

#[test]
fn w0003_materialize_suffix_to_str_push_warns() {
    // W0003 case A, builtin shape: str_push's suffix is `strview` —
    // materializing it first is a redundant allocation. (Source and
    // suffix come from DISTINCT strings so Rule 7 stays quiet.)
    let (_tirs, diags, _pool) = run_with_errors(
        "fn main():\n\tmut s: str = \"hi\"\n\tt: str = \"yo\"\n\tstr_push(&s, str(t[0:1]))\n",
    );
    assert_eq!(
        count_code(&diags, DiagCode::RedundantMaterialize),
        1,
        "expected exactly one W0003; got {:?}",
        diags
    );
}
