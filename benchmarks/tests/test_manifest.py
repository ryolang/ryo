"""Tests for suite.toml manifest loading and validation in benchmarks/run.py."""

from pathlib import Path

import pytest

from run import Arm, ManifestError, Suite, load_suite


def write_suite(tmp_path: Path, body: str) -> Path:
    path = tmp_path / "suite.toml"
    path.write_text(body)
    return path


def test_load_minimal_suite(tmp_path):
    path = write_suite(
        tmp_path,
        """
name = "fibonacci"

[[arm]]
name = "Rust"
language = "Rust"
run = "./fib_rs"
expect = "102334155"
""".strip(),
    )
    suite = load_suite(path)
    assert isinstance(suite, Suite)
    assert suite.name == "fibonacci"
    assert suite.warmup == 3
    assert suite.default_expect is None
    assert suite.dir == tmp_path
    assert len(suite.arms) == 1
    arm = suite.arms[0]
    assert isinstance(arm, Arm)
    assert arm.expect == "102334155"


def test_suite_default_expect_inherited(tmp_path):
    path = write_suite(
        tmp_path,
        """
name = "suite"
expect = "checksum-abc"

[[arm]]
name = "NoOwnExpect"
language = "Python"
run = "python a.py"

[[arm]]
name = "OwnExpect"
language = "Python"
run = "python b.py"
expect = "own-output"
""".strip(),
    )
    suite = load_suite(path)
    assert suite.default_expect == "checksum-abc"
    assert suite.arms[0].expect == "checksum-abc"
    assert suite.arms[1].expect == "own-output"


def test_empty_expect_means_exit_code_only(tmp_path):
    path = write_suite(
        tmp_path,
        """
name = "suite"

[[arm]]
name = "Silent"
language = "C"
run = "./silent"
expect = ""
""".strip(),
    )
    suite = load_suite(path)
    assert suite.arms[0].expect == ""


def test_warmup_key_honored(tmp_path):
    path = write_suite(
        tmp_path,
        """
name = "suite"
warmup = 20

[[arm]]
name = "A"
language = "C"
run = "./a"
""".strip(),
    )
    assert load_suite(path).warmup == 20


def test_category_loads(tmp_path):
    path = write_suite(
        tmp_path,
        'name = "s"\ncategory = "Strings"\n\n[[arm]]\nname = "A"\nlanguage = "rust"\nrun = "/bin/echo a"\nexpect = "a"\n',
    )
    assert load_suite(path).category == "Strings"


def test_category_defaults_to_none(tmp_path):
    path = write_suite(
        tmp_path,
        'name = "s"\n\n[[arm]]\nname = "A"\nlanguage = "rust"\nrun = "/bin/echo a"\nexpect = "a"\n',
    )
    assert load_suite(path).category is None


def test_category_must_be_string(tmp_path):
    path = write_suite(
        tmp_path,
        'name = "s"\ncategory = 42\n\n[[arm]]\nname = "A"\nlanguage = "rust"\nrun = "/bin/echo a"\nexpect = "a"\n',
    )
    with pytest.raises(ManifestError):
        load_suite(path)


def test_group_key_loads(tmp_path):
    path = write_suite(
        tmp_path,
        """
name = "suite"

[[arm]]
name = "simdjson"
language = "C++"
run = "./jp_simdjson"
group = "library"
""".strip(),
    )
    assert load_suite(path).arms[0].group == "library"


def test_slash_in_arm_name_rejected(tmp_path):
    path = write_suite(
        tmp_path,
        """
name = "suite"

[[arm]]
name = "Go encoding/csv"
language = "Go"
run = "./csv_go"
""".strip(),
    )
    with pytest.raises(ManifestError):
        load_suite(path)

    path = write_suite(
        tmp_path,
        """
name = "suite"

[[arm]]
name = "A"
language = "C"
run = "./a"
group = "lib/x"
""".strip(),
    )
    with pytest.raises(ManifestError):
        load_suite(path)


def test_shell_metachar_in_run_rejected(tmp_path):
    for run in ("./a | tee log", "./a && ./b", "./a > log", "./a < in", "./a ; ./b"):
        path = write_suite(
            tmp_path,
            f"""
name = "suite"

[[arm]]
name = "A"
language = "C"
run = "{run}"
""".strip(),
        )
        with pytest.raises(ManifestError, match="metachar"):
            load_suite(path)

    # Substitution points ($ryo, $HOME) are allowed and load untouched.
    path = write_suite(
        tmp_path,
        """
name = "suite"

[[arm]]
name = "Ryo"
language = "Ryo"
run = "$ryo run fib.ryo"

[[arm]]
name = "Bun"
language = "TypeScript"
run = "$HOME/.bun/bin/bun run fib.ts"
""".strip(),
    )
    suite = load_suite(path)
    assert [arm.run for arm in suite.arms] == [
        "$ryo run fib.ryo",
        "$HOME/.bun/bin/bun run fib.ts",
    ]


def test_missing_required_key_rejected(tmp_path):
    for missing in ("name", "language", "run"):
        keys = {"name": '"A"', "language": '"C"', "run": '"./a"'}
        del keys[missing]
        body = 'name = "suite"\n\n[[arm]]\n' + "".join(
            f"{key} = {value}\n" for key, value in keys.items()
        )
        path = write_suite(tmp_path, body)
        with pytest.raises(ManifestError, match="required"):
            load_suite(path)


def test_duplicate_group_name_rejected(tmp_path):
    def suite_toml(second_group: str) -> str:
        return f"""
name = "json_parse"

[[arm]]
name = "Ryo (AOT)"
language = "Ryo"
group = "compiled"
run = "./jp_ryo_aot"

[[arm]]
name = "Ryo (AOT)"
language = "Ryo"
group = "{second_group}"
run = "./jp_ryo_jit"
""".strip()

    # Same (group, name) pair twice: store-key collision.
    with pytest.raises(ManifestError, match="duplicate"):
        load_suite(write_suite(tmp_path, suite_toml("compiled")))

    # Same name across different groups is the store-keyed identity: fine.
    suite = load_suite(write_suite(tmp_path, suite_toml("jit")))
    assert [(arm.group, arm.name) for arm in suite.arms] == [
        ("compiled", "Ryo (AOT)"),
        ("jit", "Ryo (AOT)"),
    ]
