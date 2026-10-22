"""Tests for prereq checks, version capture, builds, the correctness gate, and ryo resolution."""

import os
import re
import shutil
import subprocess
from pathlib import Path

import pytest

import run
from run import (
    Arm,
    BuildError,
    RyoInfo,
    Suite,
    _normalize_repo_url,
    build_arm,
    capture_versions,
    check_output,
    load_suite,
    missing_tools,
    resolve_ryo,
)


def make_arm(**overrides) -> Arm:
    fields = {
        "name": "A",
        "language": "C",
        "run": "./a",
        "build": None,
        "expect": None,
        "group": None,
        "version": None,
    }
    fields.update(overrides)
    return Arm(**fields)


def make_suite(tmp_path: Path, arms: list[Arm]) -> Suite:
    return Suite(
        name="suite", descriptor="", warmup=3, default_expect=None, arms=arms, dir=tmp_path
    )


def test_missing_tools_only_selected_languages(tmp_path, monkeypatch):
    manifest = tmp_path / "suite.toml"
    manifest.write_text(
        """
name = "fibonacci"

[[arm]]
name = "Rust"
language = "Rust"
run = "./fib_rs"

[[arm]]
name = "Go"
language = "Go"
run = "./fib_go"

[[arm]]
name = "Julia"
language = "Julia"
run = "julia fib.jl"
""".strip(),
        encoding="utf-8",
    )
    suite = load_suite(manifest)
    rust_arm = suite.arms[0]
    julia_arm = suite.arms[2]

    real_which = shutil.which
    monkeypatch.setattr(
        run.shutil, "which", lambda tool: None if tool == "julia" else real_which(tool)
    )

    # A missing toolchain for a deselected arm is not reported.
    assert missing_tools(suite, [rust_arm], "/nonexistent/ryo") == []
    # A missing toolchain for a selected arm is reported as (arm, binary).
    assert missing_tools(suite, [julia_arm], "/nonexistent/ryo") == [(julia_arm, "julia")]


def test_version_override_wins(tmp_path):
    arm = make_arm(name="Py", language="python", version="echo 1.2.3")
    versions = capture_versions(make_suite(tmp_path, [arm]), [arm], "/nonexistent/ryo")
    assert versions["python"] == "1.2.3"


def test_builtin_version_rust(tmp_path):
    if shutil.which("rustc") is None:
        pytest.skip("rustc not installed")
    arm = make_arm(name="Rust", language="Rust")
    versions = capture_versions(make_suite(tmp_path, [arm]), [arm], "/nonexistent/ryo")
    assert re.match(r"^\d+\.\d+", versions["Rust"])


def test_build_failure_raises_with_output(tmp_path):
    arm = make_arm(name="Boom", language="C", build="echo oops; exit 1")
    with pytest.raises(BuildError) as exc_info:
        build_arm(arm, make_suite(tmp_path, [arm]), "/nonexistent/ryo")
    assert "oops" in exc_info.value.output
    assert "Boom" in str(exc_info.value)


def test_correctness_match_and_mismatch(tmp_path):
    arm = make_arm(name="Echo", language="C", run="/bin/echo hello", expect="hello")
    assert check_output(arm, make_suite(tmp_path, [arm]), "/x/ryo") == (True, "hello")
    arm.expect = "bye"
    assert check_output(arm, make_suite(tmp_path, [arm]), "/x/ryo") == (False, "hello")


def test_empty_expect_checks_exit_code_only(tmp_path, monkeypatch):
    # "exit N" is a shell builtin, so the direct execution is mocked and only
    # the return code reaches the gate.
    def fake_run(argv, **kwargs):
        return subprocess.CompletedProcess(argv, returncode=int(argv[-1]), stdout="")

    monkeypatch.setattr(run.subprocess, "run", fake_run)
    ok = make_arm(name="Ok", language="C", run="exit 0", expect="")
    bad = make_arm(name="Bad", language="C", run="exit 1", expect="")
    assert check_output(ok, make_suite(tmp_path, [ok]), "/x/ryo") == (True, "")
    assert check_output(bad, make_suite(tmp_path, [bad]), "/x/ryo") == (False, "")


def test_resolve_ryo_explicit_skips_build(monkeypatch):
    real_run = subprocess.run

    def spy(cmd, *args, **kwargs):
        if isinstance(cmd, list) and cmd[0] == "cargo":
            raise AssertionError("cargo build must not run for an explicit --ryo-bin")
        return real_run(cmd, *args, **kwargs)

    monkeypatch.setattr(run.subprocess, "run", spy)
    info = resolve_ryo("/bin/false", needs_build=True)
    assert isinstance(info, RyoInfo)
    assert info.built is False
    assert info.path == str(Path("/bin/false").resolve())


def test_resolve_ryo_default_records_version():
    repo_root = Path(run.__file__).resolve().parents[1]
    default_bin = repo_root / "target" / "release" / "ryo"
    if not default_bin.is_file() and not os.environ.get("RYO_BIN"):
        pytest.skip("no ryo binary (target/release/ryo or $RYO_BIN) found")
    info = resolve_ryo(None, needs_build=False)
    assert info.built is False
    assert info.version != ""


# --- Discovery: committed vs. gitignored local suites ---

MINIMAL_MANIFEST = """
name = "{name}"
warmup = 0

[[arm]]
name = "A"
language = "C"
run = "/bin/echo a"
expect = "a"
"""


def _write_manifest(root: Path, suite_dir: str, name: str) -> None:
    directory = root / "benchmarks" / suite_dir
    directory.mkdir(parents=True)
    (directory / "suite.toml").write_text(
        MINIMAL_MANIFEST.format(name=name), encoding="utf-8"
    )


def test_select_suites_skip_ignored_drops_gitignored_manifests(tmp_path, monkeypatch):
    """Artifact subcommands see committed suites only unless told otherwise."""
    _write_manifest(tmp_path, "committed", "committed")
    _write_manifest(tmp_path, "local", "local")
    monkeypatch.setattr(run, "_REPO_ROOT", tmp_path)
    monkeypatch.setattr(run, "_is_git_ignored", lambda path: path.parent.name == "local")

    assert [s.name for s in run._select_suites(None, skip_ignored=True)] == ["committed"]
    assert {s.name for s in run._select_suites(None)} == {"committed", "local"}


def test_is_git_ignored_falls_back_to_including_when_git_cannot_answer(monkeypatch):
    def no_git(*args, **kwargs):
        raise OSError("git not found")

    monkeypatch.setattr(run.subprocess, "run", no_git)
    assert run._is_git_ignored(Path("suite.toml")) is False


@pytest.mark.skipif(
    not (Path(run.__file__).resolve().parent.parent / ".git").is_dir(),
    reason="needs the real repo checkout with its gitignored local sweeps",
)
def test_select_suites_skip_ignored_excludes_the_repo_local_sweeps():
    """The live failure this fixes: bare update-readme chokes on local suites."""
    names = [s.name for s in run._select_suites(None, skip_ignored=True)]
    assert "fibonacci" in names
    for local in ("csv_parse", "json_parse", "startup_latency"):
        assert local not in names


# --- Repo URL normalization for store commit links ---


def test_normalize_repo_url_converts_ssh_remotes_to_https():
    """scp-like and ssh:// remotes would yield non-resolvable commit URLs in
    the store; https remotes pass through unchanged."""
    assert _normalize_repo_url("git@github.com:ryolang/ryo") == "https://github.com/ryolang/ryo"
    assert (
        _normalize_repo_url("ssh://git@github.com/ryolang/ryo")
        == "https://github.com/ryolang/ryo"
    )
    assert (
        _normalize_repo_url("https://github.com/ryolang/ryo")
        == "https://github.com/ryolang/ryo"
    )
