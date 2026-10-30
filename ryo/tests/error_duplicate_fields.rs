mod common;
use common::*;

use tempfile::TempDir;

#[test]
fn duplicate_error_field_declaration_and_uses_emit_only_e0040() {
    let cases = [
        ("declaration", "error E(x:int,x:int)\n"),
        ("constructor", "error E(x:int,x:int)\nfn main():\n\tE\n"),
        (
            "return",
            "error E(x:int,x:int)\nfn fail() -> E!int:\n\treturn E\n",
        ),
        (
            "match",
            "error E(x:int,x:int)\nfn main():\n\tmatch E:\n\t\tE: print(\"x\")\n",
        ),
        (
            "message",
            "error E(x:int,x:int)\nfn main():\n\tprint(E.message())\n",
        ),
    ];
    let temp_dir = TempDir::new().expect("temp");
    for (name, src) in cases {
        let filename = format!("duplicate_error_field_{name}.ryo");
        let test_file = create_test_file(temp_dir.path(), &filename, src);
        let output = run_ryo_command(&["run", &filename], &test_file)
            .expect("run duplicate error field fixture");
        assert!(!output.status.success(), "{name} unexpectedly succeeded");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("panicked"),
            "{name} panicked instead of emitting E0040: {stderr}"
        );
        assert_eq!(
            stderr.matches("E0040").count(),
            1,
            "{name} should emit exactly one E0040: {stderr}"
        );
    }
}
