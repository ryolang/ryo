use super::super::*;
use super::common::*;

#[test]
fn nested_control_dead_store_pass_uses_binding_generations() {
    let src = concat!(
        "fn nested(n: int) -> int:\n",
        "\tmut total = 0\n",
        "\tmut i = 0\n",
        "\tmut unused = 7\n",
        "\twhile i < n:\n",
        "\t\tif i % 2 == 0:\n",
        "\t\t\ttotal += i\n",
        "\t\t\ti += 1\n",
        "\t\telse:\n",
        "\t\t\tif i % 3 == 0:\n",
        "\t\t\t\tprint(total)\n",
        "\t\t\telse:\n",
        "\t\t\t\tmut shadow = 9\n",
        "\t\t\t\tshadow = 10\n",
        "\t\t\ti += 1\n",
        "\treturn total\n",
    );
    let (diags, _sidecar, tirs, pool) = check_src_full(src);
    let mut dead: Vec<_> = diags
        .iter()
        .filter(|d| d.code == DiagCode::DeadStore)
        .map(|d| d.message.as_str())
        .collect();
    dead.sort_unstable();
    assert_eq!(
        dead,
        vec![
            "value `shadow` is declared but never used",
            "value `unused` is declared but never used",
        ],
        "diagnostics: {diags:?}"
    );

    let flow = Flow::build(&tirs[0], &pool);
    let analysis = DeadStoreAnalysis::analyze(&tirs[0], &flow, &pool);
    let mut pending: Vec<_> = analysis
        .pending()
        .map(|(_, entry)| pool.str(entry.0))
        .collect();
    pending.sort_unstable();
    assert_eq!(pending, vec!["shadow", "unused"]);
}

#[test]
fn unread_match_results_are_freed() {
    let src = concat!(
        "fn main():\n",
        "\tb = true\n",
        "\tx = match b:\n",
        "\t\ttrue: \"x\"\n",
        "\t\tfalse: \"y\"\n",
        "\tmut y = \"old\"\n",
        "\ty = match b:\n",
        "\t\ttrue: \"p\"\n",
        "\t\tfalse: \"q\"\n",
    );
    let (diags, mut sidecar, tirs, _pool) = check_src_full(src);
    assert_eq!(
        diags
            .iter()
            .filter(|d| d.code == DiagCode::DeadStore)
            .count(),
        2,
        "diagnostics: {diags:?}"
    );
    let matches: Vec<_> = (1..tirs[0].instructions.len() as u32)
        .map(TirRef::from_raw)
        .filter(|&r| tirs[0].inst(r).tag == TirTag::Match)
        .collect();
    assert_eq!(matches.len(), 2);
    let sidecar = take_function_sidecar(&mut sidecar, 0);
    for result in matches {
        assert!(
            sidecar.drops.iter().any(|drop| drop.target == result),
            "match result must have a dead-store free: {sidecar:?}"
        );
    }
}

#[test]
fn tracked_self_assignment_counts_as_a_read() {
    let src = concat!(
        "struct Point:\n",
        "\tname: str\n",
        "fn main():\n",
        "\tmut s = \"abcdefghijklmnopqrstuvwxyz\"\n",
        "\ts = s\n",
        "\tmut p = Point{name=\"point\"}\n",
        "\tp = p\n",
    );
    let diags = check_src(src);
    assert!(
        !diags.iter().any(|d| d.code == DiagCode::DeadStore),
        "self-assignment must not warn: {diags:?}"
    );
}

#[test]
fn match_arm_read_on_return_path_counts_on_fallthrough_path() {
    let src = concat!(
        "enum Opt:\n",
        "\tSome(str)\n",
        "\tNone\n",
        "fn f(c: bool) -> int:\n",
        "\to = Opt.Some(\"value\")\n",
        "\tmatch o:\n",
        "\t\tSome(v):\n",
        "\t\t\tif c:\n",
        "\t\t\t\tprint(v)\n",
        "\t\t\t\treturn 0\n",
        "\t\t\tprint(\"fallthrough\")\n",
        "\t\tNone:\n",
        "\t\t\tprint(\"none\")\n",
        "\tdead = 0\n",
        "\treturn 1\n",
    );
    let diags = check_src(src);
    assert!(
        !diags.iter().any(|d| {
            d.code == DiagCode::DeadStore && d.message == "value `v` is declared but never used"
        }),
        "the returning branch's read must suppress W0001: {diags:?}"
    );
}

#[test]
fn dead_predecessor_does_not_hide_live_dead_store() {
    let src = concat!(
        "fn f(c: bool, d: bool) -> int:\n",
        "\ts = \"unused\"\n",
        "\tif c:\n",
        "\t\treturn 1\n",
        "\t\tif d:\n",
        "\t\t\tprint(\"dead\")\n",
        "\treturn 0\n",
    );
    let (diags, mut sidecar, _, _) = check_src_full(src);
    assert_eq!(
        diags
            .iter()
            .filter(|d| {
                d.code == DiagCode::DeadStore && d.message == "value `s` is declared but never used"
            })
            .count(),
        1,
        "diagnostics: {diags:?}"
    );
    let dead_span = diags
        .iter()
        .find(|d| d.code == DiagCode::DeadStore)
        .expect("dead-store diagnostic")
        .span;
    let sidecar = take_function_sidecar(&mut sidecar, 0);
    assert!(
        sidecar.drops.iter().any(|drop| drop.span == dead_span),
        "sidecar: {sidecar:?}"
    );
}
