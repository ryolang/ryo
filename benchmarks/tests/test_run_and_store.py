"""Tests for run orchestration, the agent-facing run JSON, and the historical store."""

import json
from pathlib import Path

import pytest

import run
from run import (
    Arm,
    ArmResult,
    ArmTiming,
    RyoInfo,
    Store,
    Suite,
    SuiteResult,
    load_suite,
    run_suites,
    store_key,
)


def make_result(
    tmp_path: Path,
    suite_name: str = "synthetic",
    arm_name: str = "A",
    group: str | None = None,
    language: str = "C",
    mean: float = 1.0,
    rss: float | None = 2.0,
) -> SuiteResult:
    arm = Arm(
        name=arm_name,
        language=language,
        run="/bin/echo a",
        build=None,
        expect="a",
        group=group,
        version=None,
    )
    suite = Suite(
        name=suite_name,
        descriptor="d",
        warmup=0,
        default_expect=None,
        arms=[arm],
        dir=tmp_path,
    )
    timing = ArmTiming(
        time_ms_mean=mean,
        time_ms_stddev=0.1,
        time_ms_min=mean,
        time_ms_max=mean,
        rss_mb_mean=rss,
    )
    return SuiteResult(
        suite=suite,
        date="2026-10-21T12:00:00Z",
        git_commit="abc1234",
        platform="Darwin/arm64/Apple M3 Max",
        hyperfine_version="2.0.0",
        ryo=RyoInfo(path="/x/ryo", version="0.1.0", built=False),
        versions={language: "1.0"},
        arm_results=[ArmResult(arm=arm, timing=timing)],
    )


def write_suite(tmp_path: Path, text: str) -> Suite:
    manifest = tmp_path / "suite.toml"
    manifest.write_text(text.strip(), encoding="utf-8")
    return load_suite(manifest)


TWO_ECHO_ARMS = """
name = "synthetic"
warmup = 0

[[arm]]
name = "Echo A"
language = "C"
run = "/bin/echo a"
expect = "a"

[[arm]]
name = "Echo B"
language = "C"
run = "/bin/echo b"
expect = "b"
"""

FAKE_RYO = RyoInfo(path="/nonexistent/ryo", version="unavailable", built=False)


def run_one(suite: Suite, tmp_path: Path, **overrides) -> run.RunSummary:
    options = {
        "languages": None,
        "ryo": FAKE_RYO,
        "store_dir": tmp_path / "data",
        "json_path": tmp_path / "run.json",
        "dry_run": False,
        "strict": False,
    }
    options.update(overrides)
    return run_suites([suite], **options)


# --- Store unit tests (fixture entries, no hyperfine) ---


def test_append_prunes_to_20(tmp_path):
    store = Store.load(tmp_path / "data" / "synthetic.json")
    for i in range(25):
        store.append(make_result(tmp_path, mean=float(i)))
    series = store.entries["synthetic/A"]
    assert len(series) == 20
    assert series[-1]["benches"][0]["value"] == 24.0


def test_key_grammar_with_group(tmp_path):
    store = Store.load(tmp_path / "data" / "json_parse.json")
    store.append(
        make_result(tmp_path, suite_name="json_parse", group="hand-rolled", arm_name="Ryo (AOT)")
    )
    assert set(store.entries) == {"json_parse/hand-rolled/Ryo (AOT)"}
    plain = make_result(tmp_path)
    assert store_key(plain.suite, plain.arm_results[0].arm) == "synthetic/A"


def test_store_file_shape_matches_spec(tmp_path):
    store = Store.load(tmp_path / "data" / "synthetic.json")
    store.append(make_result(tmp_path))
    store.save()

    data = json.loads((tmp_path / "data" / "synthetic.json").read_text(encoding="utf-8"))
    assert set(data) == {"lastUpdate", "repoUrl", "entries"}
    assert isinstance(data["lastUpdate"], int)
    (entry,) = data["entries"]["synthetic/A"]
    assert set(entry) == {"commit", "date", "tool", "benches"}
    assert set(entry["commit"]) == {"id", "message", "timestamp", "url"}
    assert entry["tool"] == "custom"
    assert [b["name"] for b in entry["benches"]] == ["time", "memory_peak_rss"]
    assert [b["unit"] for b in entry["benches"]] == ["ms", "MB"]
    assert {"name", "value", "unit", "extra"} <= set(entry["benches"][0])


def test_roundtrip(tmp_path):
    path = tmp_path / "data" / "synthetic.json"
    store = Store.load(path)
    store.append(make_result(tmp_path, mean=1.0))
    store.append(make_result(tmp_path, mean=2.0))
    store.save()

    loaded = Store.load(path)
    assert set(loaded.entries) == {"synthetic/A"}
    series = loaded.entries["synthetic/A"]
    assert len(series) == 2
    assert loaded.latest("synthetic/A") == series[-1]
    assert loaded.latest("missing/key") is None

    loaded.append(make_result(tmp_path, mean=3.0))
    loaded.save()
    reloaded = Store.load(path)
    assert len(reloaded.entries["synthetic/A"]) == 3
    assert reloaded.latest("synthetic/A")["benches"][0]["value"] == 3.0


# --- Orchestration tests (real /bin/echo arms + real hyperfine) ---


def test_failed_arm_keeps_healthy_results_but_writes_no_store(tmp_path):
    suite = write_suite(
        tmp_path,
        """
name = "synthetic"
warmup = 0

[[arm]]
name = "Echo A"
language = "C"
run = "/bin/echo a"
expect = "a"

[[arm]]
name = "Boom"
language = "C"
run = "exit 9"
expect = ""
""",
    )
    summary = run_one(suite, tmp_path)

    assert summary.exit_code == 1
    assert len(summary.suites) == 1
    assert len(summary.suites[0].arm_results) == 1
    assert any("Boom" in failure for failure in summary.failures)
    # The suite hard-failed: no store file, but the healthy arm kept its
    # result in the incremental run JSON (Review Focus #1).
    assert not (tmp_path / "data" / "synthetic.json").exists()
    run_json = json.loads((tmp_path / "run.json").read_text(encoding="utf-8"))
    assert len(run_json["suites"][0]["arms"]) == 1
    assert run_json["suites"][0]["arms"][0]["name"] == "Echo A"


def test_missing_toolchain_skips_arm_with_partial_status(tmp_path, monkeypatch, capsys):
    suite = write_suite(
        tmp_path,
        """
name = "synthetic"
warmup = 0

[[arm]]
name = "Rust"
language = "Rust"
run = "/bin/echo a"
expect = "a"

[[arm]]
name = "Plain C"
language = "C"
run = "/bin/echo b"
expect = "b"
""",
    )
    monkeypatch.setitem(run.TOOL_FOR_LANGUAGE, "rust", "ryo-test-missing-tool")

    summary = run_one(suite, tmp_path)

    assert summary.exit_code == 2
    assert len(summary.suites[0].arm_results) == 1
    assert summary.skipped == [
        {
            "arm": "Rust",
            "language": "Rust",
            "reason": "missing toolchain: ryo-test-missing-tool",
        }
    ]
    assert "skipping arm 'Rust'" in capsys.readouterr().err

    run_json = json.loads((tmp_path / "run.json").read_text(encoding="utf-8"))
    assert run_json["status"] == "partial"
    assert run_json["skipped"] == summary.skipped

    store_file = tmp_path / "data" / "synthetic.json"
    assert store_file.is_file()
    assert set(json.loads(store_file.read_text(encoding="utf-8"))["entries"]) == {
        "synthetic/Plain C"
    }


def test_strict_mode_fails_on_missing_toolchain(tmp_path, monkeypatch):
    suite = write_suite(
        tmp_path,
        """
name = "synthetic"
warmup = 0

[[arm]]
name = "Rust"
language = "Rust"
run = "/bin/echo a"
expect = "a"

[[arm]]
name = "Plain C"
language = "C"
run = "/bin/echo b"
expect = "b"
""",
    )
    monkeypatch.setitem(run.TOOL_FOR_LANGUAGE, "rust", "ryo-test-missing-tool")

    def no_benchmark(*args, **kwargs):
        raise AssertionError("strict mode must not benchmark")

    monkeypatch.setattr(run, "run_hyperfine", no_benchmark)
    summary = run_one(suite, tmp_path, strict=True)

    assert summary.exit_code == 1
    assert summary.suites == []
    assert not (tmp_path / "data").exists()
    assert not (tmp_path / "run.json").exists()


def test_unknown_language_name_errors(tmp_path, capsys):
    suite = write_suite(tmp_path, TWO_ECHO_ARMS)
    summary = run_one(suite, tmp_path, languages=["c", "nosuchlang"])

    assert summary.exit_code == 1
    assert "nosuchlang" in capsys.readouterr().err
    assert not (tmp_path / "data").exists()
    assert not (tmp_path / "run.json").exists()


def test_full_suite_writes_store_and_json(tmp_path):
    suite = write_suite(tmp_path, TWO_ECHO_ARMS)
    summary = run_one(suite, tmp_path)

    assert summary.exit_code == 0
    assert summary.skipped == []
    store_file = tmp_path / "data" / "synthetic.json"
    data = json.loads(store_file.read_text(encoding="utf-8"))
    assert set(data["entries"]) == {"synthetic/Echo A", "synthetic/Echo B"}

    run_json = json.loads((tmp_path / "run.json").read_text(encoding="utf-8"))
    assert run_json["git_commit"]
    assert run_json["status"] == "complete"
    assert run_json["platform"]["os"]
    assert run_json["hyperfine_version"]
    arm = run_json["suites"][0]["arms"][0]
    assert isinstance(arm["time_ms"]["mean"], float)
    assert {"mean", "stddev", "min", "max"} == set(arm["time_ms"])
    assert "previous" not in arm

    # Second run: each arm's JSON carries the previous store entry.
    summary2 = run_one(suite, tmp_path)
    assert summary2.exit_code == 0
    run_json2 = json.loads((tmp_path / "run.json").read_text(encoding="utf-8"))
    previous = run_json2["suites"][0]["arms"][0]["previous"]
    assert previous["time_ms_mean"] == arm["time_ms"]["mean"]
    assert "peak_rss_mb_mean" in previous


def test_dry_run_builds_and_checks_only(tmp_path, monkeypatch):
    suite = write_suite(tmp_path, TWO_ECHO_ARMS)

    def no_benchmark(*args, **kwargs):
        raise AssertionError("dry-run must not invoke hyperfine")

    monkeypatch.setattr(run, "run_hyperfine", no_benchmark)
    summary = run_one(suite, tmp_path, dry_run=True)

    assert summary.exit_code == 0
    assert summary.suites == []
    assert not (tmp_path / "data").exists()
    assert not (tmp_path / "run.json").exists()
