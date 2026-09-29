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
// M9.1 Debug repr through print() — sema rewrites print(x) to
// print(DebugRepr(x)); codegen renders primitives bare and structs as
// Name{f=v, f=v}. print appends no newline, so each program ends with
// an explicit print("\n") to terminate the final line.
// =============================================================================

#[test]
fn print_primitive_debug_repr_jit() {
    assert_ryo_output(
        "debug_primitives",
        "fn main():\n\tprint(42)\n\tprint(\"\\n\")\n\tprint(-7)\n\tprint(\"\\n\")\n\tprint(3.14)\n\tprint(\"\\n\")\n\tprint(1.0)\n\tprint(\"\\n\")\n\tprint(true)\n\tprint(\"\\n\")\n",
        "42\n-7\n3.14\n1.0\ntrue\n",
    );
}

#[test]
fn print_struct_debug_repr_jit() {
    assert_ryo_output(
        "debug_struct_point",
        "struct Point:\n\tx: float\n\ty: float\n\nfn main():\n\tp = Point{x=1.0, y=2.0}\n\tprint(p)\n\tprint(\"\\n\")\n",
        "Point{x=1.0, y=2.0}\n",
    );
}

#[test]
fn print_nested_struct_debug_repr_jit() {
    assert_ryo_output(
        "debug_struct_line",
        "struct Point:\n\tx: float\n\ty: float\n\nstruct Line:\n\ta: Point\n\tb: Point\n\nfn main():\n\tl = Line{a=Point{x=1.0, y=2.0}, b=Point{x=3.0, y=4.0}}\n\tprint(l)\n\tprint(\"\\n\")\n",
        "Line{a=Point{x=1.0, y=2.0}, b=Point{x=3.0, y=4.0}}\n",
    );
}

#[test]
fn print_struct_str_field_debug_repr_jit() {
    // str fields render quoted with raw (unescaped) content.
    assert_ryo_output(
        "debug_struct_user",
        "struct User:\n\tname: str\n\nfn main():\n\tu = User{name=\"alice\"}\n\tprint(u)\n\tprint(\"\\n\")\n",
        "User{name=\"alice\"}\n",
    );
}

#[test]
fn print_struct_int_bool_fields_debug_repr_jit() {
    assert_ryo_output(
        "debug_struct_flags",
        "struct Flags:\n\tcount: int\n\ton: bool\n\nfn main():\n\tf = Flags{count=42, on=true}\n\tprint(f)\n\tprint(\"\\n\")\n",
        "Flags{count=42, on=true}\n",
    );
}

#[test]
fn print_struct_then_reuse_fields_jit() {
    // Borrow proof: DebugRepr borrows the struct — field reads after
    // the print must still see the original values.
    assert_ryo_output(
        "debug_struct_borrow",
        "struct Point:\n\tx: float\n\ty: float\n\nfn main():\n\tp = Point{x=1.0, y=2.0}\n\tprint(p)\n\tprint(\"\\n\")\n\tprint(float_to_str(p.x + p.y))\n\tprint(\"\\n\")\n",
        "Point{x=1.0, y=2.0}\n3.0\n",
    );
}

// =============================================================================
// M9.1 memberwise struct equality (#[derive(Eq)]) — `==`/`!=` lower to
// per-field compares: icmp/fcmp for scalars, ryo_str_eq for str fields,
// recursion for nested derived structs. `==` borrows both operands.
// =============================================================================

#[test]
fn struct_eq_equal_and_unequal_jit() {
    assert_ryo_output(
        "struct_eq_basic",
        "#[derive(Eq)] struct Point:\n\tx: int\n\ty: int\n\nfn main():\n\tp = Point{x=1, y=2}\n\tq = Point{x=1, y=2}\n\tr = Point{x=1, y=3}\n\tprint(p == q)\n\tprint(\"\\n\")\n\tprint(p == r)\n\tprint(\"\\n\")\n\tprint(p != r)\n\tprint(\"\\n\")\n",
        "true\nfalse\ntrue\n",
    );
}

#[test]
fn struct_eq_field_order_matters_jit() {
    // The same values in swapped field positions are unequal —
    // comparison is memberwise in declaration order, not set-like.
    assert_ryo_output(
        "struct_eq_field_order",
        "#[derive(Eq)] struct Pair:\n\ta: int\n\tb: int\n\nfn main():\n\tp = Pair{a=1, b=2}\n\tq = Pair{a=2, b=1}\n\tprint(p == q)\n\tprint(\"\\n\")\n",
        "false\n",
    );
}

#[test]
fn struct_eq_nested_derived_jit() {
    // A derived struct field compares through the recursion into the
    // nested type's own memberwise equality.
    assert_ryo_output(
        "struct_eq_nested",
        "#[derive(Eq)] struct Point:\n\tx: float\n\ty: float\n\n#[derive(Eq)] struct Line:\n\tstart: Point\n\tend: Point\n\nfn main():\n\tl1 = Line{start=Point{x=0.0, y=0.0}, end=Point{x=1.0, y=1.0}}\n\tl2 = Line{start=Point{x=0.0, y=0.0}, end=Point{x=1.0, y=1.0}}\n\tl3 = Line{start=Point{x=0.0, y=0.0}, end=Point{x=1.0, y=2.0}}\n\tprint(l1 == l2)\n\tprint(\"\\n\")\n\tprint(l1 == l3)\n\tprint(\"\\n\")\n\tprint(l1 != l3)\n\tprint(\"\\n\")\n",
        "true\nfalse\ntrue\n",
    );
}

#[test]
fn struct_eq_str_field_jit() {
    // str fields compare by content. "al" + "ice" produces an inline
    // (SSO) field while the literal is static — the compare must
    // extract both (ptr, len) pairs through the slot home.
    assert_ryo_output(
        "struct_eq_str_field",
        "#[derive(Eq)] struct User:\n\tname: str\n\tage: int\n\nfn main():\n\ta = User{name=\"alice\", age=30}\n\tb = User{name=\"al\" + \"ice\", age=30}\n\tc = User{name=int_to_str(42), age=30}\n\tprint(a == b)\n\tprint(\"\\n\")\n\tprint(a == c)\n\tprint(\"\\n\")\n\tprint(a != c)\n\tprint(\"\\n\")\n",
        "true\nfalse\ntrue\n",
    );
}

#[test]
fn struct_eq_nan_field_never_equals_jit() {
    // IEEE: NaN != NaN, so fcmp eq on a NaN field is false even when
    // both operands are the very same struct. NaN is built arithmetically
    // — float division does not trap (0.0 / 0.0).
    assert_ryo_output(
        "struct_eq_nan",
        "#[derive(Eq)] struct Point:\n\tx: float\n\ty: float\n\nfn main():\n\tp = Point{x=0.0 / 0.0, y=1.0}\n\tprint(p == p)\n\tprint(\"\\n\")\n\tprint(p != p)\n\tprint(\"\\n\")\n",
        "false\ntrue\n",
    );
}

#[test]
fn struct_eq_operands_remain_usable_jit() {
    // Borrow proof: `==` roots borrows of both operands — a struct with
    // needs-drop fields must survive the comparison fully usable.
    assert_ryo_output(
        "struct_eq_borrow",
        "#[derive(Eq)] struct User:\n\tname: str\n\tage: int\n\nfn main():\n\tp = User{name=\"alice\", age=30}\n\tq = User{name=\"bob\", age=30}\n\tprint(p == q)\n\tprint(\"\\n\")\n\tprint(p.name)\n\tprint(q.name)\n\tprint(int_to_str(p.age))\n\tprint(\"\\n\")\n\tprint(p != q)\n\tprint(\"\\n\")\n",
        "false\nalicebob30\ntrue\n",
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

#[test]
fn print_struct_debug_repr_aot_exact_output() {
    // M9.1: Debug repr synthesis must produce identical output through
    // the AOT pipeline (object emission + Zig link).
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "struct Point:\n\tx: float\n\ty: float\n\nfn main():\n\tp = Point{x=1.0, y=2.0}\n\tprint(p)\n\tprint(\"\\n\")\n";
    let test_file = create_test_file(temp_dir.path(), "debug_struct_aot.ryo", code);

    let build_output = run_ryo_build(&test_file, temp_dir.path());
    assert!(
        build_output.status.success(),
        "ryo build failed. STDERR: {}",
        String::from_utf8_lossy(&build_output.stderr)
    );

    let binary_path = exe_path(temp_dir.path(), "debug_struct_aot");
    let run_output = Command::new(&binary_path)
        .output()
        .expect("Failed to execute compiled binary");

    assert!(run_output.status.success(), "compiled binary should exit 0");
    let stdout = String::from_utf8_lossy(&run_output.stdout);
    assert_eq!(
        stdout, "Point{x=1.0, y=2.0}\n",
        "binary stdout must be exactly the printed bytes"
    );
}
