//! M8.4.2: the projection side tables are type-agnostic — these tests
//! pin that `bytes` owners get the same P2/P4/P5 treatment as `str`.

use super::*;

#[test]
fn bytes_owner_classified_as_move_type() {
    let pool = InternPool::new();
    assert!(is_move_type(pool.bytes(), &pool));
    assert!(!is_move_type(pool.bytes_view(), &pool));
}

#[test]
fn bytes_projection_freezes_owner() {
    // P2: mutating (reassigning) the owner while a projection is live
    // is SourceProjected — same as str.
    let diags = check_src(
        "fn main():\n\tmut b = b\"\\x01\\x02\"\n\tv = b[0:1]\n\tb = b\"\\x03\"\n\tprint(v)\n",
    );
    assert!(
        diags.iter().any(|d| d.code == DiagCode::SourceProjected),
        "expected SourceProjected, got {:?}",
        diags.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
}

#[test]
fn bytes_owner_free_defers_past_live_projection() {
    // P5: the owner's destruction defers past the view's last use —
    // no diagnostics, exactly one successful analysis.
    let diags =
        check_src("fn main():\n\tb = b\"\\x01\\x02\"\n\tv = b[0:1]\n\tprint(v)\n\tprint(b[1:])\n");
    assert!(
        diags.is_empty(),
        "unexpected diags: {:?}",
        diags.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
}

#[test]
fn bytes_reslice_projects_root_owner() {
    // P3: a slice of a slice still freezes the root owner.
    let diags = check_src(
        "fn main():\n\tmut b = b\"\\x01\\x02\\x03\"\n\tv = b[0:2]\n\tw = v[1:]\n\tb = b\"\\x04\"\n\tprint(w)\n",
    );
    assert!(
        diags.iter().any(|d| d.code == DiagCode::SourceProjected),
        "expected SourceProjected, got {:?}",
        diags.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
}

#[test]
fn bytes_conditional_reassign_emits_dead_drop() {
    // The sidecar half of the codegen dead-drop path: a bytes owner
    // conditionally reassigned and never read afterwards yields a
    // ConditionalDeadDrop whose target is Bytes-typed — codegen reads
    // the target's type to pick `ryo_bytes_free` over `ryo_str_free`.
    // (The dead reassign warns W0001; only the sidecar is pinned here.)
    let src = "fn main():\n\tmut b = \"AB\".to_bytes()\n\tflag = false\n\tif flag:\n\t\tb = \"CD\".to_bytes()\n";
    let (_diags, mut sidecar, tirs, pool) = check_src_full(src);
    let sc = take_function_sidecar(&mut sidecar, 0);
    assert_eq!(
        sc.conditional_dead_drops.len(),
        1,
        "expected one ConditionalDeadDrop for the pre-if bytes owner; got {:?}",
        sc.conditional_dead_drops
    );
    let target = sc.conditional_dead_drops[0].target;
    assert!(
        matches!(pool.kind(tirs[0].inst(target).ty), TypeKind::Bytes),
        "dead-drop target must be Bytes-typed so codegen selects ryo_bytes_free"
    );
}

#[test]
fn w0003_case_b_warns_for_bytes() {
    // Bound `c = bytes(v)` whose copy never escapes and whose source
    // is never mutated — the view could have been used directly.
    let diags = check_src(
        "fn main():\n\traw = b\"\\x01\\x02\"\n\tv = raw[0:1]\n\tc = bytes(v)\n\tprint(c)\n",
    );
    assert_eq!(
        w0003_count(&diags),
        1,
        "got: {:?}",
        diags.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
    assert!(
        diags
            .iter()
            .any(|d| d.message.contains("`bytes(...)` copy never escapes")),
        "message must name bytes(...): {:?}",
        diags.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
}

#[test]
fn w0003_case_b_str_message_unchanged() {
    let diags = check_src("fn main():\n\traw = \"ab\"\n\tv = raw[0:1]\n\tc = str(v)\n\tprint(c)\n");
    assert!(
        diags
            .iter()
            .any(|d| d.message.contains("`str(...)` copy never escapes")),
        "got: {:?}",
        diags.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
}
