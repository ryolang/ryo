use crate::{astgen, sema};
use ryo_core::compilation::CompilationContext;
use ryo_core::diag::{DiagCode, DiagSink};
use ryo_core::types::InternPool;
use std::path::{Path, PathBuf};

#[test]
fn direct_catch_widened_span_preserves_source() {
    let input = "error A(str)\nfn find() -> A!int:\n\treturn A(\"failure\")\nfn main():\n\tvalue = find() catch as err:\n\t\tA(err): 0\n\tprint(value)\n";
    let mut context = CompilationContext::new();
    let id = context.register_source(PathBuf::from("catch.ryo"), input.into());
    let source = context.sources().get(id).unwrap();
    let mut pool = InternPool::new();
    let (program, parse_diags) =
        crate::parse::parse_source_with_source(source, &mut pool).expect("source should parse");
    assert!(parse_diags.is_empty());
    let mut sink = DiagSink::new();
    let uir = astgen::generate(&program, &mut pool, &mut sink);
    let _tirs = sema::analyze(&uir, &mut pool, &mut sink, input, Path::new("catch.ryo"));
    let diags = sink.into_diags();
    let warning = diags
        .iter()
        .find(|diag| diag.code == DiagCode::DeadStore)
        .expect("unused catch binding should warn");

    assert_eq!(warning.span.source, id);
    assert_eq!(warning.span.start, input.find("as err").unwrap());
}
