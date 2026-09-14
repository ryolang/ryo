mod common;

use std::process::Command;

fn run_ryo(source: &str, name: &str) -> String {
    let (_tmp, exe) = common::build_and_link(source, name, &[]);
    let out = Command::new(exe).output().expect("run");
    assert!(
        out.status.success(),
        "{name} exited {:?}: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("utf8 stdout")
}

#[test]
fn string_building_loop_is_correct() {
    // Consuming reassign-concat loop: the string_building benchmark shape.
    let src = "\
fn main():
\tmut s: str = \"\"
\tfor i in range(0, 1000):
\t\ts = s + \"x\"
\tassert(s.len() == 1000, \"len must be 1000\")
\tprint(\"ok\\n\")
";
    assert_eq!(run_ryo(src, "sso_string_building"), "ok\n");
}

#[test]
fn doubling_concat_stays_correct() {
    // Aliasing exclusion: s = s + s must keep the allocating path.
    let src = "\
fn main():
\tmut s: str = \"a\"
\tfor i in range(0, 5):
\t\ts = s + s
\tassert(s.len() == 32, \"len must be 32\")
\tprint(s)
\tprint(\"\\n\")
";
    assert_eq!(
        run_ryo(src, "sso_doubling"),
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n"
    );
}
