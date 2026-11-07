//! Fresh-depth audit:
//! - Scalar, call, and aggregate child edges use `eval_inst_with` or the active machine.
//! - `Match`, `IfStmt`, and boolean joins are Flow/memo leaves, not eager postorder nodes.
//! - Statement and declaration evaluators are true roots; their nested calls use context.

use super::super::tests::clif_of;
use super::walker::with_expr_limit;

#[test]
fn scalar_and_call_clif_matches_at_every_depth_limit() {
    let cases = [
        "fn main():\n\tprint(1+2*3-4)\n",
        "fn id(x: int) -> int:\n\treturn x\nfn main():\n\tprint(id(1+id(2)))\n",
        "fn pair(a: int, b: int) -> int:\n\treturn a*10+b\nfn id(x: int) -> int:\n\treturn x\nfn main():\n\tprint(pair(id(1), id(2)))\n",
    ];
    for src in cases {
        let baseline = with_expr_limit(64, || clif_of(src));
        for limit in [0, 1] {
            assert_eq!(
                with_expr_limit(limit, || clif_of(src)),
                baseline,
                "CLIF differed at expression depth limit {limit}:\n{src}"
            );
        }
    }
}

#[test]
fn representation_clif_matches_at_every_depth_limit() {
    let cases = [
        "fn main():\n\ts = \"a\"+\"b\"\n\tprint(s)\n",
        "fn main():\n\ts = \"abc\"\n\tv = s[0:1]\n\tprint(v)\n",
        "struct Point:\n\tx: int\n\ty: int\nfn main():\n\tp = Point{x=1, y=2}\n\tprint(p)\n",
        "enum Shape:\n\tCircle(int)\nfn main():\n\ts = Shape.Circle(1)\n\tprint(s)\n",
        "fn main():\n\ts = \"abc\"\n\tprint(str(s[0:1]))\n",
        "error E(str)\nfn main():\n\te = E(\"bad\")\n\tprint(e.message())\n",
    ];
    for src in cases {
        let baseline = with_expr_limit(64, || clif_of(src));
        for limit in [0, 1] {
            assert_eq!(
                with_expr_limit(limit, || clif_of(src)),
                baseline,
                "CLIF differed at expression depth limit {limit}:\n{src}"
            );
        }
    }
}

#[test]
fn control_flow_clif_matches_at_every_depth_limit() {
    let cases = [
        "fn main():\n\tprint(true and false)\n",
        "fn main():\n\tvalue = match 1:\n\t\t1: 10\n\t\t_: 20\n\tprint(value)\n",
        "enum Opt:\n\tSome(value: int)\n\tNone\nfn main():\n\to = Opt.Some{value=1}\n\tvalue = match o:\n\t\tOpt.Some(v): v\n\t\tOpt.None: 0\n\tprint(value)\n",
        "fn f(b: bool) -> int:\n\tmatch b:\n\t\ttrue: panic(\"x\")\n\t\tfalse: return 7\nfn main():\n\tprint(f(false))\n",
        "fn main():\n\tassert(true, \"ok\")\n",
    ];
    for src in cases {
        let baseline = with_expr_limit(64, || clif_of(src));
        for limit in [0, 1] {
            assert_eq!(
                with_expr_limit(limit, || clif_of(src)),
                baseline,
                "CLIF differed at expression depth limit {limit}:\n{src}"
            );
        }
    }
}
