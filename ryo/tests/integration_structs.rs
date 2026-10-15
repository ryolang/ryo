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
fn struct_eq_bytes_field_jit() {
    // bytes fields compare by content via ryo_bytes_eq. The literal is
    // static while `b"al" + b"ice"` is a fresh heap buffer — the compare
    // must extract both (ptr, len) pairs through the slot home.
    assert_ryo_output(
        "struct_eq_bytes_field",
        "#[derive(Eq)] struct Blob:\n\tdata: bytes\n\ttag: int\n\nfn main():\n\ta = Blob{data=b\"alice\", tag=1}\n\tb = Blob{data=b\"al\" + b\"ice\", tag=1}\n\tc = Blob{data=b\"bob\", tag=1}\n\tprint(a == b)\n\tprint(\"\\n\")\n\tprint(a == c)\n\tprint(\"\\n\")\n\tprint(a != c)\n\tprint(\"\\n\")\n",
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

const NATURAL_LOOP_STR_ACCUMULATOR: &str = "\
struct Parsed:
\ttext: str
\tnext: int

fn skip_ws(b: bytesview, at: int) -> int:
\tmut p = at
\twhile p < b.len():
\t\tc = b[p]
\t\tif c == 32 or c == 9 or c == 10 or c == 13:
\t\t\tp += 1
\t\telse:
\t\t\treturn p
\treturn p

fn take_string(b: bytesview, s: str, at: int) -> Parsed:
\tmut i = at + 1
\tmut end = -1
\twhile i < b.len() and end < 0:
\t\tc = b[i]
\t\tif c == 92:
\t\t\ti += 2
\t\telif c == 34:
\t\t\tend = i + 1
\t\telse:
\t\t\ti += 1
\tif end < 0:
\t\treturn Parsed{text=\"<error>\", next=b.len()}
\treturn Parsed{text=str(s[at:end]), next=end}

fn take_number(b: bytesview, s: str, at: int) -> Parsed:
\tmut i = at
\tmut go = true
\twhile go and i < b.len():
\t\tc = b[i]
\t\tif (c >= 48 and c <= 57) or c == 45 or c == 43 or c == 46 or c == 101 or c == 69:
\t\t\ti += 1
\t\telse:
\t\t\tgo = false
\treturn Parsed{text=str(s[at:i]), next=i}

fn parse_array(b: bytesview, s: str, at: int) -> Parsed:
\tmut p = skip_ws(b, at + 1)
\tif p < b.len() and b[p] == 93:
\t\treturn Parsed{text=\"[]\", next=p + 1}
\tmut out = \"[\"
\tmut child = parse_value(b, s, p)
\tstr_push(&out, child.text)
\tp = skip_ws(b, child.next)
\twhile p < b.len() and b[p] == 44:
\t\tstr_push(&out, \",\")
\t\tchild = parse_value(b, s, p + 1)
\t\tstr_push(&out, child.text)
\t\tp = skip_ws(b, child.next)
\tif p < b.len() and b[p] == 93:
\t\tp += 1
\t\tstr_push(&out, \"]\")
\treturn Parsed{text=out, next=p}

fn parse_object(b: bytesview, s: str, at: int) -> Parsed:
\tmut p = skip_ws(b, at + 1)
\tif p < b.len() and b[p] == 125:
\t\treturn Parsed{text=\"{}\", next=p + 1}
\tmut out = \"{\"
\tmut first = true
\tmut go = true
\twhile go:
\t\tif not first:
\t\t\tstr_push(&out, \",\")
\t\tfirst = false
\t\tp = skip_ws(b, p)
\t\tk = take_string(b, s, p)
\t\tstr_push(&out, k.text)
\t\tstr_push(&out, \":\")
\t\tp = skip_ws(b, k.next)
\t\tif p >= b.len() or b[p] != 58:
\t\t\tgo = false
\t\telse:
\t\t\tchild = parse_value(b, s, p + 1)
\t\t\tstr_push(&out, child.text)
\t\t\tp = skip_ws(b, child.next)
\t\t\tif p >= b.len():
\t\t\t\tgo = false
\t\t\telse:
\t\t\t\tc = b[p]
\t\t\t\tif c == 44:
\t\t\t\t\tp += 1
\t\t\t\telif c == 125:
\t\t\t\t\tp += 1
\t\t\t\t\tstr_push(&out, \"}\")
\t\t\t\t\tgo = false
\t\t\t\telse:
\t\t\t\t\tgo = false
\treturn Parsed{text=out, next=p}

fn parse_value(b: bytesview, s: str, at: int) -> Parsed:
\tp = skip_ws(b, at)
\tif p >= b.len():
\t\treturn Parsed{text=\"<error>\", next=p}
\tc = b[p]
\tif c == 34:
\t\treturn take_string(b, s, p)
\tif c == 123:
\t\treturn parse_object(b, s, p)
\tif c == 91:
\t\treturn parse_array(b, s, p)
\tif c == 116:
\t\treturn Parsed{text=\"true\", next=p + 4}
\tif c == 102:
\t\treturn Parsed{text=\"false\", next=p + 5}
\tif c == 110:
\t\treturn Parsed{text=\"null\", next=p + 4}
\tif c == 45 or (c >= 48 and c <= 57):
\t\treturn take_number(b, s, p)
\treturn Parsed{text=\"<error>\", next=p}

fn render(doc: str) -> int:
\tb = doc.as_bytes()
\tr = parse_value(b, doc, 0)
\treturn r.text.len()

fn main():
\t# Zero comma-iterations: the pre-fix build double-freed the
\t# pre-loop child struct through codegen's binding-path redirect
\t# (last-use Free anchored before the in-loop reassign).
\tone = render(\"[{\\\"id\\\":0,\\\"name\\\":\\\"useruseruser\\\"}]\")
\t# Two comma-iterations: exercises the reassign-free displacement
\t# chain across loop back-edges.
\tmut two = render(\"[{\\\"a\\\":1},{\\\"b\\\":2}]\")
\ttwo += render(\"[{\\\"a\\\":1},{\\\"b\\\":2}]\")
\tprint(int_to_str(one))
\tprint(\" \")
\tprint(int_to_str(two))
\tprint(\"\\n\")
";

#[test]
fn natural_loop_str_accumulator() {
    // Pre-fix: SIGABRT (exit 134) from a bad ryo_str_free — the
    // loop-carried `child` binding's pre-reassign owner was freed by
    // BOTH a last-use Free (anchored before the in-loop reassign) and
    // the reassign-free, and the return epilogue double-freed the
    // binding's final value through the same home slot.
    let temp_dir = tempfile::TempDir::new().expect("create source directory");
    let test_file = create_test_file(
        temp_dir.path(),
        "bug_natural_loop_str_accumulator.ryo",
        NATURAL_LOOP_STR_ACCUMULATOR,
    );
    let output = run_ryo_command(&["run", "bug_natural_loop_str_accumulator.ryo"], &test_file)
        .expect("run ryo");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout, "32 34\n");
}

const STR_PUSH_STRUCT_FIELD_NOOP: &str = "\
struct Rec:
\ttext: str
\tbuf: bytes
\tn: int

fn bump(inout s: str):
\tstr_push(&s, \"!\")

fn main():
\tmut a = Rec{text=\"x\", buf=\"\".to_bytes(), n=0}
\tbump(&a.text)
\tprint(a.text)
\tprint(\"\\n\")

\tmut r = Rec{text=\"y\", buf=\"\".to_bytes(), n=0}
\tstr_push(&r.text, \"?\")
\tprint(r.text)
\tprint(\"\\n\")

\tbytes_push(&r.buf, 65)
\tbytes_push(&r.buf, 66)
\tprint(int_to_str(r.buf.len()))
\tprint(\"\\n\")
";

#[test]
fn str_push_struct_field_noop() {
    // Pre-fix: the str_push/bytes_push builtin intercepts assumed arg 0
    // lowered to Var(name), so a FieldAccess place was evaluated into a
    // snapshot triple spilled to a temp slot; the runtime mutated the
    // temp, and the reload was gated on local_name_of(field), which is
    // None — the write-back was silently dropped (compiles, exit 0,
    // prints "x!\ny\n0"). Post-fix the field's address in the root
    // struct's slot is passed directly, like the generic inout path.
    let temp_dir = tempfile::TempDir::new().expect("create source directory");
    let test_file = create_test_file(
        temp_dir.path(),
        "bug_str_push_struct_field_noop.ryo",
        STR_PUSH_STRUCT_FIELD_NOOP,
    );
    let output = run_ryo_command(&["run", "bug_str_push_struct_field_noop.ryo"], &test_file)
        .expect("run ryo");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout, "x!\ny?\n2\n");
}

#[test]
fn mixed_abi_modes_expand_consistently_at_multiple_calls() {
    assert_ryo_output(
        "mixed_call_abi.ryo",
        include_str!("fixtures/mixed_call_abi.ryo"),
        "abcd47abcd93start_0123456789012345678901234567890123456789!efghijkl",
    );
}
