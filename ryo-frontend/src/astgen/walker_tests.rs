use super::expr::{machine_call_count, reset_machine_call_count, with_expr_limit};
use super::*;
use ryo_core::diag::Diag;

fn lower(src: &str) -> (Uir, Vec<Diag>) {
    let mut pool = InternPool::new();
    let (ast, parse_diags) = crate::parse::parse_source(src, &mut pool).expect("parse recovers");
    let mut sink = DiagSink::new();
    for diag in parse_diags {
        sink.emit(diag);
    }
    let uir = generate(&ast, &mut pool, &mut sink);
    (uir, sink.into_diags())
}

fn lower_with_expr_limit(src: &str, limit: u32) -> (Uir, Vec<Diag>) {
    with_expr_limit(limit, || lower(src))
}

fn snapshot((uir, diags): (Uir, Vec<Diag>)) -> String {
    let mut direct_catch_matches: Vec<u32> = uir
        .direct_catch_matches
        .iter()
        .map(|inst| inst.raw())
        .collect();
    direct_catch_matches.sort_unstable();
    format!(
        "{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}",
        uir.instructions, uir.extra, uir.func_bodies, uir.spans, diags, direct_catch_matches
    )
}

#[test]
fn shallow_lowering_does_not_start_allocating_machine() {
    let src = "fn main():\n\tprint(1+2*3-4)\n";
    reset_machine_call_count();
    let (_, diags) = lower(src);
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(machine_call_count(), 0);
    let (_, diags) = lower_with_expr_limit(src, 0);
    assert!(diags.is_empty(), "{diags:?}");
    assert!(machine_call_count() > 0);
}

#[test]
fn scalar_lowering_matches_at_every_depth_limit() {
    let src = "fn main():\n\tprint(1+2*3-4)\n";
    let baseline = snapshot(lower(src));
    for limit in [0, 1, 64] {
        assert_eq!(
            snapshot(lower_with_expr_limit(src, limit)),
            baseline,
            "scalar lowering differed at expression depth limit {limit}"
        );
    }
}

fn constructor_match_cases() -> Vec<(&'static str, String)> {
    vec![
        (
            "named_variant",
            r#"enum Shape:
	Rectangle(width: int)
fn main():
	value = Shape.Rectangle{width=(match 1:
		1: Shape.Rectangle{width=1}
		_: Shape.Rectangle{width=2}
	)}
"#
            .to_string(),
        ),
        (
            "tuple_variant",
            r#"enum Shape:
	Circle(int)
fn main():
	value = Shape.Circle((match 1:
		1: Shape.Circle(1)
		_: Shape.Circle(2)
	))
"#
            .to_string(),
        ),
        (
            "declared_error",
            r#"error E(int)
fn main():
	value = E((match 1:
		1: E(1)
		_: E(2)
	))
"#
            .to_string(),
        ),
        (
            "qualified_error_family_member",
            r#"errors IoError:
	Eof(int)
fn main():
	value = IoError.Eof((match 1:
		1: IoError.Eof(1)
		_: IoError.Eof(2)
	))
"#
            .to_string(),
        ),
        (
            "invalid_payload",
            r#"enum Shape:
	Rectangle(width: int)
fn main():
	value = Shape.Rectangle{unknown=(match 1:
		1: Shape.Rectangle{unknown=1}
		_: Shape.Rectangle{width=2}
	)}
"#
            .to_string(),
        ),
    ]
}

fn assert_single_machine_at_limit_zero(name: &str, src: &str) {
    let baseline = snapshot(lower(src));
    reset_machine_call_count();
    let lowered = snapshot(lower_with_expr_limit(src, 0));
    assert_eq!(
        machine_call_count(),
        1,
        "{name} started nested lowering machines"
    );
    assert_eq!(lowered, baseline, "{name} changed forced-limit lowering");
}

fn nested_constructor_cases() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "named_variant",
            r#"enum Shape:
	Rectangle(width: int)
fn main():
	value = Shape.Rectangle{width=Shape.Rectangle{width=1}}
"#,
        ),
        (
            "tuple_variant",
            r#"enum Shape:
	Circle(int)
fn main():
	value = Shape.Circle(Shape.Circle(1))
"#,
        ),
        (
            "declared_error",
            r#"error E(int)
fn main():
	value = E(E(1))
"#,
        ),
        (
            "qualified_error_family_member",
            r#"errors IoError:
	Eof(int)
fn main():
	value = IoError.Eof(IoError.Eof(1))
"#,
        ),
        (
            "invalid_payload",
            r#"enum Shape:
	Rectangle(width: int)
fn main():
	value = Shape.Rectangle{unknown=Shape.Rectangle{unknown=1}}
"#,
        ),
    ]
}

#[test]
fn nested_constructor_lowering_uses_one_machine() {
    for (name, src) in nested_constructor_cases() {
        assert_single_machine_at_limit_zero(name, src);
    }
}

#[test]
#[ignore = "I-270: enable with astgen traversal"]
fn alternating_constructor_match_lowering_uses_one_machine() {
    for (name, src) in constructor_match_cases() {
        assert_single_machine_at_limit_zero(name, &src);
    }
}

#[test]
fn alternating_constructor_match_lowering_matches_at_every_depth_limit() {
    for (name, src) in constructor_match_cases() {
        let baseline = snapshot(lower(&src));
        for limit in [0, 1, 64] {
            assert_eq!(
                snapshot(lower_with_expr_limit(&src, limit)),
                baseline,
                "{name} differed at expression depth limit {limit}"
            );
        }
    }
}

fn non_simple_statement_cases() -> Vec<(&'static str, String)> {
    vec![
        (
            "declaration",
            "fn main():\n\tmatch 1:\n\t\t1:\n\t\t\tvalue = 1\n\t\t\tprint(value)\n\t\t_: 0\n"
                .to_string(),
        ),
        (
            "assignment",
            "fn main():\n\tmut value = 0\n\tmatch 1:\n\t\t1:\n\t\t\tvalue = 1\n\t\t_: 0\n"
                .to_string(),
        ),
        (
            "return",
            "fn choose() -> int:\n\tmatch 1:\n\t\t1:\n\t\t\treturn 1\n\t\t_:\n\t\t\treturn 0\nfn main():\n\tprint(choose())\n"
                .to_string(),
        ),
        (
            "if_elif_else",
            "fn main():\n\tmatch 1:\n\t\t1:\n\t\t\tif true:\n\t\t\t\tprint(1)\n\t\t\telif false:\n\t\t\t\tprint(2)\n\t\t\telse:\n\t\t\t\tprint(3)\n\t\t_: 0\n"
                .to_string(),
        ),
        (
            "while",
            "fn main():\n\tmatch 1:\n\t\t1:\n\t\t\twhile false:\n\t\t\t\tprint(1)\n\t\t_: 0\n"
                .to_string(),
        ),
        (
            "for_range_break_continue",
            "fn main():\n\tmatch 1:\n\t\t1:\n\t\t\tfor i in range(3):\n\t\t\t\tif i == 1:\n\t\t\t\t\tcontinue\n\t\t\t\tif i == 2:\n\t\t\t\t\tbreak\n\t\t\t\tprint(i)\n\t\t_: 0\n"
                .to_string(),
        ),
        (
            "parser_recovery",
            "fn main():\n\tmatch 1:\n\t\t1:\n\t\t\tvalue = = 1\n\t\t_: 0\n".to_string(),
        ),
    ]
}

#[test]
#[ignore = "I-270: enable with astgen traversal"]
fn non_simple_statement_lowering_uses_one_machine() {
    for (name, src) in non_simple_statement_cases() {
        assert_single_machine_at_limit_zero(name, &src);
    }
}
