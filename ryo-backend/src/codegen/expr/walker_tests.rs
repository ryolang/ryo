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
