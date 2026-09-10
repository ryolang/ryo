mod common;
use common::*;

use std::process::Command;
use tempfile::TempDir;

// =============================================================================
// M9 Structs — JIT end-to-end tests
// =============================================================================

#[test]
fn struct_define_construct_access_jit() {
    assert_ryo_runs(
        "struct_point",
        "struct Point:\n\tx: float\n\ty: float\n\nfn main():\n\tp = Point{x=1.0, y=2.0}\n\tprint(float_to_str(p.x))\n\tprint(float_to_str(p.y))\n",
    );
}

#[test]
fn struct_field_mutation_jit() {
    // Field assignment and compound assignment through a `mut` binding.
    assert_ryo_output(
        "struct_mut",
        "struct Point:\n\tx: float\n\nfn main():\n\tmut p = Point{x=1.0}\n\tp.x = 42.0\n\tp.x += 1.0\n\tprint(float_to_str(p.x))\n",
        "43.0",
    );
}

#[test]
fn struct_pass_and_return_jit() {
    assert_ryo_output(
        "struct_area",
        "struct Rectangle:\n\twidth: float\n\theight: float\n\nfn area(rect: Rectangle) -> float:\n\treturn rect.width * rect.height\n\nfn main():\n\tr = Rectangle{width=10.0, height=5.0}\n\tprint(float_to_str(area(r)))\n",
        "50.0",
    );
}

#[test]
fn struct_return_end_to_end_jit() {
    // A function returning a struct by value (sret ABI), field read
    // on the returned value at the call site.
    assert_ryo_output(
        "struct_make",
        "struct Point:\n\tx: float\n\nfn make() -> Point:\n\treturn Point{x=4.0}\n\nfn main():\n\tp = make()\n\tprint(float_to_str(p.x))\n",
        "4.0",
    );
}

#[test]
fn struct_with_str_field_moves_and_drops_jit() {
    assert_ryo_runs(
        "struct_person",
        "struct Person:\n\tname: str\n\tage: int\n\nfn main():\n\tp = Person{name=\"alice\", age=30}\n\tq = p\n\tprint(q.name)\n\tprint(int_to_str(q.age))\n",
    );
}

#[test]
fn nested_struct_jit() {
    assert_ryo_output(
        "struct_nested",
        "struct Point:\n\tx: float\n\nstruct Line:\n\tstart: Point\n\tend: Point\n\nfn main():\n\tl = Line{start=Point{x=0.0}, end=Point{x=1.0}}\n\tprint(float_to_str(l.end.x))\n",
        "1.0",
    );
}

#[test]
fn struct_field_inout_borrow_jit() {
    // A struct field is a valid `&` borrow target: the callee mutates
    // `p.x` in place through the inout write-back ABI.
    assert_ryo_output(
        "struct_inout_field",
        "struct Point:\n\tx: float\n\nfn bump(inout v: float):\n\tv += 1.0\n\nfn main():\n\tmut p = Point{x=1.0}\n\tbump(&p.x)\n\tprint(float_to_str(p.x))\n",
        "2.0",
    );
}

// =============================================================================
// AOT exact output
// =============================================================================

#[test]
fn struct_person_aot_exact_output() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "struct Person:\n\tname: str\n\tage: int\n\nfn main():\n\tp = Person{name=\"alice\", age=30}\n\tq = p\n\tprint(q.name)\n\tprint(int_to_str(q.age))\n";
    let test_file = create_test_file(temp_dir.path(), "struct_person_aot.ryo", code);

    let build_output = run_ryo_build(&test_file, temp_dir.path());
    assert!(
        build_output.status.success(),
        "ryo build failed. STDERR: {}",
        String::from_utf8_lossy(&build_output.stderr)
    );

    let binary_path = exe_path(temp_dir.path(), "struct_person_aot");
    let run_output = Command::new(&binary_path)
        .output()
        .expect("Failed to execute compiled binary");

    assert!(run_output.status.success(), "compiled binary should exit 0");
    let stdout = String::from_utf8_lossy(&run_output.stdout);
    // `print` appends no newline, so the two prints concatenate.
    assert_eq!(
        stdout, "alice30",
        "binary stdout must be exactly the printed bytes"
    );
}
