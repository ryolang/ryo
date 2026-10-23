//! Regression tests for the jump-exit defensive-emit double-discharge
//! (bug_reports/bug_str_concat_after_break.ryo and
//! bug_reports/bug_str_copy_concat_loop.ryo, same root cause).
//!
//! A `break` in one loop used to anchor frees of values produced in
//! LATER loops: the defensive sweep in `schedule_break_continue_frees`
//! emits a break-anchored Free for every Valid owner with no drop
//! anywhere, with no check that the owner's producer runs before the
//! break. The release then provably precedes the value's creation
//! (validator: "path double discharge of place union") and the bogus
//! target poisons CleanupPlan's Binding/Value redirect (release builds:
//! "cleanup target has no storage for binding").

use super::super::*;
use super::common::*;

/// Minimal reproducer: break loop, then a loop-carried str concat.
/// The concat temp has no drop of its own — it is covered indirectly by
/// the `"x"` last-use Free's binding redirect — so the defensive sweep
/// grabbed it.
const CONCAT_AFTER_BREAK: &str = "fn main():\n\tmut i = 0\n\twhile i < 10:\n\t\tbreak\n\tmut result = \"x\"\n\tmut j = 0\n\twhile j < 3:\n\t\tresult = result + \"0\"\n\t\tj += 1\n\tprint(result)\n";

/// Same shape with `bytes` — the emit is type-agnostic (BytesConcat).
const BYTES_AFTER_BREAK: &str = "fn main():\n\tmut i = 0\n\twhile i < 10:\n\t\tbreak\n\tmut b = b\"x\"\n\tmut j = 0\n\twhile j < 3:\n\t\tb = b + b\"0\"\n\t\tj += 1\n\tprint(b)\n";

/// Same shape with a Call value reassigned in the later loop — the emit
/// fires for any loop-carried reassignment, not just concats.
const CALL_AFTER_BREAK: &str = "fn main():\n\tmut i = 0\n\twhile i < 10:\n\t\tbreak\n\tmut s = \"x\"\n\tmut j = 0\n\twhile j < 3:\n\t\ts = int_to_str(j)\n\t\tj += 1\n\tprint(s)\n";

/// Negative control: `continue` never receives the defensive emit.
const CONCAT_AFTER_CONTINUE: &str = "fn main():\n\tmut i = 0\n\twhile i < 10:\n\t\ti += 1\n\t\tif i == 5:\n\t\t\tcontinue\n\tmut result = \"x\"\n\tmut j = 0\n\twhile j < 3:\n\t\tresult = result + \"0\"\n\t\tj += 1\n\tprint(result)\n";

/// The exact bug_reports/bug_str_copy_concat_loop.ryo source: a `break`
/// in the dot-search loop, then two more loops whose StrConcat temps
/// the sweep targeted (7 defensive emits at that break).
const FIXED7_AFTER_BREAK: &str = "fn pow10_int(exp: int) -> int:\n\tmut r = 1\n\tmut i = 0\n\twhile i < exp:\n\t\tr = r * 10\n\t\ti += 1\n\treturn r\n\nfn format_fixed_7(v: float) -> str:\n\ts = float_to_str(v)\n\tb = s.as_bytes()\n\tmut sign = \"\"\n\tmut start = 0\n\tif b[0] == 45:\n\t\tsign = \"-\"\n\t\tstart = 1\n\tmut dot = -1\n\tmut i = start\n\twhile i < b.len():\n\t\tif b[i] == 46:\n\t\t\tdot = i\n\t\t\tbreak\n\t\ti += 1\n\tmut int_str = \"\"\n\tmut frac_str = \"\"\n\tif dot < 0:\n\t\tint_str = str(s[start:b.len()])\n\t\tfrac_str = \"\"\n\telse:\n\t\tint_str = str(s[start:dot])\n\t\tfrac_str = str(s[dot + 1:b.len()])\n\tmut int_val = 0\n\tmut k = 0\n\twhile k < int_str.len():\n\t\tc = int_str.as_bytes()[k]\n\t\tint_val = int_val * 10 + (c - 48)\n\t\tk += 1\n\tmut frac_len = frac_str.len()\n\tmut frac_val = 0\n\tif frac_len > 0:\n\t\tmut take = frac_len\n\t\tif take > 8:\n\t\t\ttake = 8\n\t\tk = 0\n\t\twhile k < take:\n\t\t\tc = frac_str.as_bytes()[k]\n\t\t\tfrac_val = frac_val * 10 + (c - 48)\n\t\t\tk += 1\n\t\tif frac_len > 7:\n\t\t\tdigit8 = frac_val % 10\n\t\t\tfrac_val = frac_val / 10\n\t\t\tif digit8 >= 5:\n\t\t\t\tfrac_val += 1\n\t\telif frac_len < 7:\n\t\t\tfrac_val = frac_val * pow10_int(7 - frac_len)\n\tif frac_val >= 10000000:\n\t\tfrac_val = 0\n\t\tint_val += 1\n\tmut result = sign + int_to_str(int_val) + \".\"\n\tmut frac_out = int_to_str(frac_val)\n\tmut zeros = 7 - frac_out.len()\n\twhile zeros > 0:\n\t\tresult = result + \"0\"\n\t\tzeros -= 1\n\tresult = result + frac_out\n\treturn result\n\nfn main():\n\tprint(format_fixed_7(3.14159))\n\tprint(\"\\n\")\n";

fn assert_no_errors(diags: &[Diag]) {
    assert!(
        !diags
            .iter()
            .any(|d| d.severity == ryo_core::diag::Severity::Error),
        "no errors expected; got: {diags:?}"
    );
}

#[test]
fn break_loop_before_concat_loop_no_double_discharge() {
    // RED: check() panics in the validator's path double-discharge proof.
    let (diags, _sidecar, _tirs, _pool) = check_src_full(CONCAT_AFTER_BREAK);
    assert_no_errors(&diags);
}

#[test]
fn break_loop_before_concat_loop_schedule_shape() {
    let (_diags, mut sidecar, tirs, _pool) = check_src_full(CONCAT_AFTER_BREAK);
    let tir = &tirs[0];
    let loops: Vec<_> = tir
        .body_stmts()
        .iter()
        .filter(|&s| tir.inst(s).tag == TirTag::WhileLoop)
        .collect();
    assert_eq!(loops.len(), 2, "break loop + concat loop");
    let loop1 = loops[0];
    let loop2 = loops[1];
    let break_stmt = tir
        .while_loop_view(loop1)
        .body
        .iter()
        .find(|&s| tir.inst(s).tag == TirTag::Break)
        .expect("break in loop 1");
    let concat = tir
        .while_loop_view(loop2)
        .body
        .iter()
        .find(|&s| tir.inst(s).tag == TirTag::Assign)
        .map(|a| tir.assign_view(a).value)
        .expect("concat in loop 2");
    let x_const = tir
        .body_stmts()
        .iter()
        .filter(|&s| tir.inst(s).tag == TirTag::VarDecl)
        .map(|d| tir.var_decl_view(d).initializer)
        .find(|&i| tir.inst(i).tag == TirTag::StrConst)
        .expect("\"x\" const");
    let sc = take_function_sidecar(&mut sidecar, 0);
    assert!(
        !sc.drops
            .iter()
            .any(|d| d.position.anchor_inst() == Some(break_stmt) && d.target == concat),
        "no free of the post-loop concat anchored at the earlier break; schedule = {:?}",
        sc.drops
    );
    assert!(
        sc.drops.iter().any(|d| d.target == x_const),
        "the pre-reassign \"x\" owner keeps its last-use free; schedule = {:?}",
        sc.drops
    );
}

#[test]
fn search_break_before_later_concat_loops_ok() {
    let (diags, _sidecar, _tirs, _pool) = check_src_full(FIXED7_AFTER_BREAK);
    assert_no_errors(&diags);
}

#[test]
fn break_loop_before_bytes_concat_loop_ok() {
    let (diags, _sidecar, _tirs, _pool) = check_src_full(BYTES_AFTER_BREAK);
    assert_no_errors(&diags);
}

#[test]
fn break_loop_before_call_reassign_loop_ok() {
    let (diags, _sidecar, _tirs, _pool) = check_src_full(CALL_AFTER_BREAK);
    assert_no_errors(&diags);
}

#[test]
fn continue_loop_before_concat_loop_ok() {
    // Negative control: the defensive emit is break-only by design
    // (on `continue` the next iteration would re-read the freed buffer).
    let (diags, _sidecar, _tirs, _pool) = check_src_full(CONCAT_AFTER_CONTINUE);
    assert_no_errors(&diags);
}

/// A producer inside an `if` BEFORE the loop: textually (and in
/// emission order) earlier than the break, but its block does not
/// dominate the jump's block — the not-taken arm bypasses it. The
/// dominance gate must skip it anyway; an emission-order gate
/// (`order_of(r) < order_of(jump)`) would wrongly emit. Verified by
/// mutation: weakening the gate to emission order makes this test fail
/// with the path double-discharge panic.
const IF_BEFORE_LOOP_PRODUCER: &str = "fn main():\n\tmut s = \"x\"\n\tmut c = 0\n\tif c < 1:\n\t\ts = s + \"b\"\n\tmut i = 0\n\twhile i < 10:\n\t\tbreak\n\tprint(s)\n";

#[test]
fn break_gate_uses_cfg_dominance_not_textual_order() {
    let (diags, mut sidecar, tirs, _pool) = check_src_full(IF_BEFORE_LOOP_PRODUCER);
    assert_no_errors(&diags);
    let tir = &tirs[0];
    let if_stmt = tir
        .body_stmts()
        .iter()
        .find(|&s| tir.inst(s).tag == TirTag::IfStmt)
        .expect("preceding if");
    let producer = tir
        .if_stmt_view(if_stmt)
        .then_stmts
        .iter()
        .find(|&s| tir.inst(s).tag == TirTag::Assign)
        .map(|a| tir.assign_view(a).value)
        .expect("producer in the if arm");
    assert_eq!(
        tir.inst(producer).tag,
        TirTag::StrConcat,
        "producer is the if-arm concat"
    );
    let loop_inst = tir
        .body_stmts()
        .iter()
        .find(|&s| tir.inst(s).tag == TirTag::WhileLoop)
        .expect("loop with break");
    let break_stmt = tir
        .while_loop_view(loop_inst)
        .body
        .iter()
        .find(|&s| tir.inst(s).tag == TirTag::Break)
        .expect("break in loop");
    let sc = take_function_sidecar(&mut sidecar, 0);
    assert!(
        !sc.drops
            .iter()
            .any(|d| d.position.anchor_inst() == Some(break_stmt) && d.target == producer),
        "a non-dominating earlier producer must not be break-anchored; schedule = {:?}",
        sc.drops
    );
}
