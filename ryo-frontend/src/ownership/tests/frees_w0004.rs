use super::super::*;
use super::common::*;

// ---------------------------------------------------------------------------
// W0004 RedundantToBytes: a bound `to_bytes()` result that is never mutated
// and never escapes should be `as_bytes()` (zero-copy view).
// ---------------------------------------------------------------------------

#[test]
fn w0004_to_bytes_only_borrow_read_warns() {
    // The dogfood shape (benchmarks/json_validate): the copy is bound,
    // then only borrow-passed to a `bytesview` parameter — a use the
    // view itself serves with no allocation.
    let diags = check_src(
        "fn validate(b: bytesview) -> int:\n\treturn b.len()\n\nfn main():\n\ts = \"{}\"\n\traw = s.to_bytes()\n\tprint(int_to_str(validate(raw)))\n",
    );
    assert_eq!(
        w0004_count(&diags),
        1,
        "expected exactly one W0004; got: {diags:?}"
    );
    assert!(
        diags.iter().any(|d| d.code == DiagCode::RedundantToBytes
            && d.message
                .contains("use `as_bytes()` (zero-copy view) instead of `to_bytes()`")),
        "message must name the fix; got: {:?}",
        diags.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
}

#[test]
fn w0004_strview_receiver_also_warns() {
    // `to_bytes()` on a `strview` produces the same redundant copy.
    let diags = check_src(
        "fn main():\n\ts = \"abc\"\n\tb = s[0:2].to_bytes()\n\tprint(int_to_str(b.len()))\n",
    );
    assert_eq!(w0004_count(&diags), 1, "got: {diags:?}");
}

#[test]
fn w0004_mutated_does_not_warn() {
    let diags = check_src(
        "fn main():\n\tmut b = \"ab\".to_bytes()\n\tbytes_push(&b, 99)\n\tprint(int_to_str(b.len()))\n",
    );
    assert_eq!(
        w0004_count(&diags),
        0,
        "mutated copy must not warn; got: {diags:?}"
    );
}

#[test]
fn w0004_returned_does_not_warn() {
    let diags = check_src(
        "fn make() -> bytes:\n\tb = \"ab\".to_bytes()\n\treturn b\n\nfn main():\n\tprint(int_to_str(make().len()))\n",
    );
    assert_eq!(
        w0004_count(&diags),
        0,
        "returned copy must not warn; got: {diags:?}"
    );
}

#[test]
fn w0004_move_param_does_not_warn() {
    let diags = check_src(
        "fn eat(move b: bytes):\n\tprint(int_to_str(b.len()))\n\nfn main():\n\tb = \"ab\".to_bytes()\n\teat(b)\n",
    );
    assert_eq!(
        w0004_count(&diags),
        0,
        "moved copy must not warn; got: {diags:?}"
    );
}

#[test]
fn w0004_inout_pass_does_not_warn() {
    let diags = check_src(
        "fn grow(inout b: bytes):\n\tbytes_push(&b, 99)\n\nfn main():\n\tmut b = \"ab\".to_bytes()\n\tgrow(&b)\n\tprint(int_to_str(b.len()))\n",
    );
    assert_eq!(
        w0004_count(&diags),
        0,
        "inout-passed copy must not warn; got: {diags:?}"
    );
}

#[test]
fn w0004_stored_in_aggregate_does_not_warn() {
    let diags = check_src(
        "struct P:\n\tdata: bytes\n\nfn main():\n\tb = \"ab\".to_bytes()\n\tp = P{data = b}\n\tprint(int_to_str(p.data.len()))\n",
    );
    assert_eq!(
        w0004_count(&diags),
        0,
        "stored copy must not warn; got: {diags:?}"
    );
}

#[test]
fn w0004_move_chain_read_only_warns() {
    // Moves across local bindings are followed: `c = b` relocates the
    // copy's owner; `c` itself never escaping still warns.
    let diags =
        check_src("fn main():\n\tb = \"ab\".to_bytes()\n\tc = b\n\tprint(int_to_str(c.len()))\n");
    assert_eq!(
        w0004_count(&diags),
        1,
        "read-only move chain must warn; got: {diags:?}"
    );
}

#[test]
fn w0004_move_chain_escaping_target_does_not_warn() {
    let diags = check_src(
        "fn make() -> bytes:\n\tb = \"ab\".to_bytes()\n\tc = b\n\treturn c\n\nfn main():\n\tprint(int_to_str(make().len()))\n",
    );
    assert_eq!(
        w0004_count(&diags),
        0,
        "chain ending in an escape must not warn; got: {diags:?}"
    );
}

#[test]
fn w0004_move_chain_mutated_target_does_not_warn() {
    let diags = check_src(
        "fn main():\n\tb = \"ab\".to_bytes()\n\tmut c = b\n\tbytes_push(&c, 99)\n\tprint(int_to_str(c.len()))\n",
    );
    assert_eq!(
        w0004_count(&diags),
        0,
        "chain whose target is mutated must not warn; got: {diags:?}"
    );
}

#[test]
fn w0004_move_into_existing_mut_binding_does_not_warn() {
    // An Assign-site hazard is ambiguous (move-into-`mut` vs
    // reassign-drop of the old value) — conservative direction: suppress.
    let diags = check_src(
        "fn main():\n\tb = \"ab\".to_bytes()\n\tmut c = b\"\\x00\"\n\tc = b\n\tprint(int_to_str(c.len()))\n",
    );
    assert_eq!(w0004_count(&diags), 0, "got: {diags:?}");
}

#[test]
fn w0004_rebound_away_does_not_warn() {
    // Reassigning the binding drops the copy's buffer — the owned
    // allocation was load-bearing.
    let diags = check_src(
        "fn main():\n\tmut b = \"ab\".to_bytes()\n\tprint(int_to_str(b.len()))\n\tb = b\"\\x00\\x01\"\n\tprint(int_to_str(b.len()))\n",
    );
    assert_eq!(w0004_count(&diags), 0, "got: {diags:?}");
}

#[test]
fn w0004_never_read_is_w0001_not_w0004() {
    // A copy that is never read at all is a dead store — W0001's
    // jurisdiction, matching W0003 case B's split.
    let diags = check_src("fn main():\n\tb = \"ab\".to_bytes()\n\tprint(\"x\")\n");
    assert_eq!(w0004_count(&diags), 0, "got: {diags:?}");
    assert!(
        diags.iter().any(|d| d.code == DiagCode::DeadStore),
        "expected W0001 instead; got: {diags:?}"
    );
}

#[test]
fn w0004_transient_arg_does_not_warn() {
    // Unbound call-argument results are never collected (same split as
    // W0003 case B: argument positions are the sema-side shapes'
    // jurisdiction).
    let diags = check_src(
        "fn take(b: bytesview) -> int:\n\treturn b.len()\n\nfn main():\n\tprint(int_to_str(take(\"ab\".to_bytes())))\n",
    );
    assert_eq!(w0004_count(&diags), 0, "got: {diags:?}");
}

#[test]
fn w0004_as_bytes_and_plain_bytes_do_not_warn() {
    // `as_bytes()` lowers to a ToView (no call), and a `bytes` literal
    // is not a `to_bytes()` copy — neither is a lint site.
    let diags = check_src(
        "fn main():\n\ts = \"ab\"\n\tv = s.as_bytes()\n\tprint(int_to_str(v.len()))\n\tb = b\"\\x01\\x02\"\n\tprint(int_to_str(b.len()))\n",
    );
    assert_eq!(w0004_count(&diags), 0, "got: {diags:?}");
}

#[test]
fn w0004_receiver_mutated_after_copy_does_not_warn() {
    // `to_bytes()` snapshots; `as_bytes()` is a live view that freezes
    // its owner. Mutating the source AFTER the copy while the copy is
    // still live makes the suggested rewrite fail to compile (P2
    // freeze) — suppress.
    let diags = check_src(
        "fn main():\n\tmut s = \"hello\"\n\tb = s.to_bytes()\n\tstr_push(&s, \"!\")\n\tprint(int_to_str(b.len()))\n",
    );
    assert_eq!(
        w0004_count(&diags),
        0,
        "receiver mutated after the copy must not warn; got: {diags:?}"
    );
}

#[test]
fn w0004_receiver_mutated_after_last_use_warns() {
    // The copy's last read PRECEDES the mutation, so the replacement
    // view is already dead when the freeze would bite — the rewrite is
    // sound (verified: the `as_bytes()` form compiles and runs). Only
    // a hazard between the copy and the chain's last use suppresses.
    let diags = check_src(
        "fn main():\n\tmut s = \"hello\"\n\tb = s.to_bytes()\n\tprint(int_to_str(b.len()))\n\tstr_push(&s, \"!\")\n",
    );
    assert_eq!(
        w0004_count(&diags),
        1,
        "receiver mutated after the copy's last use must warn; got: {diags:?}"
    );
}

#[test]
fn w0004_receiver_moved_after_copy_does_not_warn() {
    // Moving the source after the copy is legal for the snapshot but
    // not for the live view — suppress.
    let diags = check_src(
        "fn eat(move s: str):\n\tprint(s)\n\nfn main():\n\ts = \"hello\"\n\tb = s.to_bytes()\n\teat(s)\n\tprint(int_to_str(b.len()))\n",
    );
    assert_eq!(
        w0004_count(&diags),
        0,
        "receiver moved after the copy must not warn; got: {diags:?}"
    );
}

#[test]
fn w0004_receiver_mutated_before_copy_still_warns() {
    // Ordering matters: mutations BEFORE the copy are fine — the view
    // created after them sees the same bytes the snapshot did (the
    // benchmarks/json_validate main shape: build a string with
    // str_push, then view it).
    let diags = check_src(
        "fn validate(b: bytesview) -> int:\n\treturn b.len()\n\nfn main():\n\tmut s = \"[\"\n\tfor i in range(0, 3):\n\t\tstr_push(&s, int_to_str(i))\n\traw = s.to_bytes()\n\tprint(int_to_str(validate(raw)))\n",
    );
    assert_eq!(
        w0004_count(&diags),
        1,
        "pre-copy mutations must not suppress; got: {diags:?}"
    );
}

#[test]
fn w0004_strview_receiver_root_hazarded_after_copy_does_not_warn() {
    // View receiver: the hazard that suppresses is on the view's ROOT
    // owner (the `str` behind the `strview`), resolved transitively.
    let diags = check_src(
        "fn main():\n\tmut s = \"abcdef\"\n\tw = s[0:2]\n\tb = w.to_bytes()\n\tstr_push(&s, \"!\")\n\tprint(int_to_str(b.len()))\n",
    );
    assert_eq!(
        w0004_count(&diags),
        0,
        "root of a view receiver hazarded after the copy must not warn; got: {diags:?}"
    );
}

#[test]
fn w0004_receiver_mutated_in_shared_loop_does_not_warn() {
    // A hazard inside a loop that also contains the copy re-executes
    // between iterations regardless of source order — suppress
    // (same shared-loop clause as W0003 case B's defensive check).
    let diags = check_src(
        "fn main():\n\tmut s = \"ab\"\n\tmut i = 0\n\twhile i < 3:\n\t\tb = s.to_bytes()\n\t\tstr_push(&s, \"!\")\n\t\tprint(int_to_str(b.len()))\n\t\ti += 1\n",
    );
    assert_eq!(
        w0004_count(&diags),
        0,
        "shared-loop receiver mutation must not warn; got: {diags:?}"
    );
}

#[test]
fn w0004_receiver_hazard_before_copy_in_shared_loop_warns() {
    // The hazard ranks BEFORE the copy in program order, so the rank
    // rule does not fire — only the shared-loop clause could suppress.
    // It must not: the copy is bound and read entirely within the loop
    // body, so the replacement view dies before the back-edge and the
    // next iteration's push collides with nothing. Verified against
    // the compiler: the `as_bytes()` form of this shape compiles and
    // runs.
    let diags = check_src(
        "fn main():\n\tmut s = \"ab\"\n\tmut i = 0\n\twhile i < 3:\n\t\tstr_push(&s, \"!\")\n\t\tb = s.to_bytes()\n\t\tprint(int_to_str(b.len()))\n\t\ti += 1\n",
    );
    assert_eq!(
        w0004_count(&diags),
        1,
        "pre-copy hazard in a shared loop with an iteration-local copy must warn; got: {diags:?}"
    );
}

#[test]
fn w0004_escaping_chain_in_shared_loop_does_not_warn() {
    // Probe B: the copy is reassigned into a `mut` binding declared
    // BEFORE the loop and read AFTER it — the last iteration's
    // replacement view would still be live at the next iteration's
    // push (and at the post-loop read), so the rewrite is invalid
    // (verified: the `as_bytes()` form fails with E0035). Both
    // to_bytes sites must stay silent: the outer one via the
    // reassign-drop hazard, the in-loop one via the refined
    // shared-loop clause.
    let diags = check_src(
        "fn main():\n\tmut s = \"ab\"\n\tmut b = s.to_bytes()\n\tmut i = 0\n\twhile i < 3:\n\t\tstr_push(&s, \"!\")\n\t\tb = s.to_bytes()\n\t\tprint(int_to_str(b.len()))\n\t\ti += 1\n\tprint(int_to_str(b.len()))\n",
    );
    assert_eq!(
        w0004_count(&diags),
        0,
        "a chain whose binding is read after the loop must not warn; got: {diags:?}"
    );
}

#[test]
fn w0004_preloop_copy_read_in_loop_mutated_in_loop_does_not_warn() {
    // The copy predates the loop and is re-read every iteration, so the
    // replacement view lives through the WHOLE loop (cyclic liveness,
    // same as `view_defer_loop`) — the in-loop mutation collides with
    // it even though it ranks after the last read lexically. Verified
    // against the compiler: the `as_bytes()` form fails with E0035.
    let diags = check_src(
        "fn main():\n\tmut s = \"ab\"\n\tb = s.to_bytes()\n\tmut i = 0\n\twhile i < 3:\n\t\tprint(int_to_str(b.len()))\n\t\tstr_push(&s, \"!\")\n\t\ti += 1\n",
    );
    assert_eq!(
        w0004_count(&diags),
        0,
        "pre-loop copy re-read inside a mutating loop must not warn; got: {diags:?}"
    );
}

#[test]
fn w0004_inloop_copy_read_then_mutated_same_iteration_warns() {
    // Guard against over-suppression: the copy is bound AND read within
    // one iteration, so the per-iteration view dies at the read and the
    // later in-iteration mutation collides with nothing — the rewrite
    // is sound and the warning must stand. Only a loop that encloses a
    // chain read but NOT the copy activates the loop-liveness clause.
    let diags = check_src(
        "fn main():\n\tmut s = \"ab\"\n\tmut i = 0\n\twhile i < 3:\n\t\tb = s.to_bytes()\n\t\tprint(int_to_str(b.len()))\n\t\tstr_push(&s, \"!\")\n\t\ti += 1\n",
    );
    assert_eq!(
        w0004_count(&diags),
        1,
        "in-loop copy read before the same-iteration mutation must warn; got: {diags:?}"
    );
}
