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
