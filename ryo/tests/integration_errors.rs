mod common;
use common::*;

use std::process::Command;
use tempfile::TempDir;

const ERROR_RETURN: &str = "error NotFound\n\nfn fail() -> NotFound!int:\n\treturn NotFound\n\nfn main():\n\tresult = fail()\n\tprint(\"ok\\n\")\n";

#[test]
fn error_return_constructs_and_runs_jit() {
    assert_ryo_output("error_return_jit.ryo", ERROR_RETURN, "ok\n");
}

#[test]
fn error_return_constructs_and_runs_aot() {
    let temp_dir = TempDir::new().expect("temp");
    let test_file = create_test_file(temp_dir.path(), "error_return_aot.ryo", ERROR_RETURN);
    let build = run_ryo_build(&test_file, temp_dir.path());
    assert!(
        build.status.success(),
        "ryo build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(exe_path(temp_dir.path(), "error_return_aot"))
        .output()
        .expect("run AOT error-return binary");
    assert!(
        run.status.success(),
        "AOT binary failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "ok\n");
}

#[test]
fn active_union_alternatives_widen_with_heap_payload_jit() {
    let pad = "a".repeat(40);
    let src = format!(
        "error A(str)\nerror B(int)\n\
         fn narrow(fail: bool) -> A!int:\n\tif fail:\n\t\treturn A(int_to_str(7) + \"{pad}\")\n\treturn 7\n\
         fn widen(fail: bool) -> (A | B)!int:\n\tresult: A!int = narrow(fail)\n\treturn result\n\
         fn main():\n\tok = widen(false)\n\terr = widen(true)\n\tprint(\"ok\\n\")\n"
    );
    assert_ryo_output("error_widening_jit.ryo", &src, "ok\n");
}

#[test]
fn void_success_return_runs_jit() {
    let src = "error A\nfn succeed() -> A!void:\n\treturn\nfn main():\n\tresult = succeed()\n\tprint(\"ok\\n\")\n";
    assert_ryo_output("error_void_success_jit.ryo", src, "ok\n");
}

const DECLARED_ERROR_MATCH: &str = "error NotFound(str)\n\
     fn main():\n\terr = NotFound(int_to_str(7) + \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\")\n\tmatch err:\n\t\tNotFound(message): print(message)\n";
const DECLARED_ERROR_MATCH_OUTPUT: &str = "7aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

#[test]
fn declared_error_match_moves_heap_payload_jit() {
    assert_ryo_output(
        "declared_error_match_jit.ryo",
        DECLARED_ERROR_MATCH,
        DECLARED_ERROR_MATCH_OUTPUT,
    );
}

#[test]
fn declared_error_match_moves_heap_payload_aot() {
    let temp_dir = TempDir::new().expect("temp");
    let test_file = create_test_file(
        temp_dir.path(),
        "declared_error_match_aot.ryo",
        DECLARED_ERROR_MATCH,
    );
    let build = run_ryo_build(&test_file, temp_dir.path());
    assert!(
        build.status.success(),
        "ryo build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(exe_path(temp_dir.path(), "declared_error_match_aot"))
        .output()
        .expect("run AOT declared-error match binary");
    assert!(
        run.status.success(),
        "AOT binary failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        DECLARED_ERROR_MATCH_OUTPUT
    );
}
