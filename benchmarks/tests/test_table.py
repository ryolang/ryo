"""Tests for the `table` subcommand: canonical Markdown tables rendered from store data."""

import argparse
import json
from datetime import datetime, timezone
from pathlib import Path

import pytest

import run
from run import Store, cmd_table, load_suite, render_suite_table

PLATFORM = "Darwin/arm64/Apple M3 Max"
EPOCH_MS = int(datetime(2026, 10, 21, tzinfo=timezone.utc).timestamp() * 1000)

TWO_ARM_SUITE = """
name = "synthetic"
warmup = 3

[[arm]]
name = "Rust"
language = "rust"
run = "/bin/echo a"
expect = "a"

[[arm]]
name = "Swift"
language = "swift"
run = "/bin/echo b"
expect = "b"
"""

GROUPED_SUITE = """
name = "grouped"
warmup = 5

[[arm]]
name = "Ryo (AOT)"
language = "ryo"
group = "hand-rolled"
run = "/bin/echo a"
expect = "a"

[[arm]]
name = "Go"
language = "go"
group = "stdlib"
run = "/bin/echo b"
expect = "b"
"""


def make_entry(
    time_ms: float,
    rss_mb: float | None,
    *,
    toolchain: str = "rustc 1.85.0",
    hyperfine: str = "2.0.0",
    commit: str = "abc1234",
    date: int = EPOCH_MS,
) -> dict:
    """One store entry in the exact shape Store._entry writes."""
    return {
        "commit": {"id": commit, "message": "msg", "timestamp": 0, "url": ""},
        "date": date,
        "tool": "custom",
        "benches": [
            {
                "name": "time",
                "value": time_ms,
                "unit": "ms",
                "extra": f"mean wall clock; hyperfine {hyperfine}; {toolchain}",
            },
            {
                "name": "memory_peak_rss",
                "value": rss_mb,
                "unit": "MB",
                "extra": "max resident set size; mean of runs",
            },
        ],
    }


def write_store(path: Path, entries: dict[str, list[dict]]) -> Store:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps({"lastUpdate": 0, "repoUrl": "https://example.test/ryo", "entries": entries}),
        encoding="utf-8",
    )
    return Store.load(path)


def write_suite(tmp_path: Path, text: str) -> run.Suite:
    tmp_path.mkdir(parents=True, exist_ok=True)
    manifest = tmp_path / "suite.toml"
    manifest.write_text(text.strip(), encoding="utf-8")
    return load_suite(manifest)


@pytest.fixture
def fixed_platform(monkeypatch):
    monkeypatch.setattr(run, "_platform_string", lambda: PLATFORM)


def test_ungrouped_suite_renders_exact_table(tmp_path, fixed_platform):
    suite = write_suite(tmp_path, TWO_ARM_SUITE)
    store = write_store(
        tmp_path / "data" / "synthetic.json",
        {
            "synthetic/Rust": [make_entry(12.3, 4.5, toolchain="rustc 1.85.0")],
            "synthetic/Swift": [make_entry(7.0, None, toolchain="swiftc 6.3.3")],
        },
    )
    assert render_suite_table(suite, store) == (
        "| Arm | Time | Peak RSS | Version |\n"
        "|---|---|---|---|\n"
        "| Rust | 12.3 ms | 4.5 MB | rustc 1.85.0 |\n"
        "| Swift | 7 ms | — | swiftc 6.3.3 |\n"
        "Measured on Darwin/arm64/Apple M3 Max, 2026-10-21 "
        "(commit abc1234, hyperfine 2.0.0, warmup 3)"
    )


def test_grouped_suite_renders_heading_per_group(tmp_path, fixed_platform):
    suite = write_suite(tmp_path, GROUPED_SUITE)
    store = write_store(
        tmp_path / "data" / "grouped.json",
        {
            "grouped/hand-rolled/Ryo (AOT)": [make_entry(2.7, 2.75, toolchain="ryo 0.1.0")],
            "grouped/stdlib/Go": [make_entry(1.6, 3.0, toolchain="go 1.24.0")],
        },
    )
    assert render_suite_table(suite, store) == (
        "#### hand-rolled\n"
        "| Arm | Time | Peak RSS | Version |\n"
        "|---|---|---|---|\n"
        "| Ryo (AOT) | 2.7 ms | 2.75 MB | ryo 0.1.0 |\n"
        "Measured on Darwin/arm64/Apple M3 Max, 2026-10-21 "
        "(commit abc1234, hyperfine 2.0.0, warmup 5)\n"
        "\n"
        "#### stdlib\n"
        "| Arm | Time | Peak RSS | Version |\n"
        "|---|---|---|---|\n"
        "| Go | 1.6 ms | 3 MB | go 1.24.0 |\n"
        "Measured on Darwin/arm64/Apple M3 Max, 2026-10-21 "
        "(commit abc1234, hyperfine 2.0.0, warmup 5)"
    )


def test_missing_memory_bench_renders_dash(tmp_path, fixed_platform):
    """A hyperfine export without memory_peak_resident stores rss value None;
    the Peak RSS cell must render as — while time still renders."""
    suite = write_suite(tmp_path, TWO_ARM_SUITE)
    store = write_store(
        tmp_path / "data" / "synthetic.json",
        {
            "synthetic/Rust": [make_entry(12.3, 4.5, toolchain="rustc 1.85.0")],
            "synthetic/Swift": [make_entry(7.0, None, toolchain="swiftc 6.3.3")],
        },
    )
    table = render_suite_table(suite, store)
    assert "| Swift | 7 ms | — | swiftc 6.3.3 |" in table
    assert "| Rust | 12.3 ms | 4.5 MB | rustc 1.85.0 |" in table


def test_version_cell_comes_from_store_extra_toolchain_string(tmp_path, fixed_platform):
    toolchain = "ryo 0.1.0-dev.20261021+63078ac"
    suite = write_suite(tmp_path, GROUPED_SUITE)
    store = write_store(
        tmp_path / "data" / "grouped.json",
        {
            "grouped/hand-rolled/Ryo (AOT)": [make_entry(2.7, 2.75, toolchain=toolchain)],
            "grouped/stdlib/Go": [make_entry(1.6, 3.0, toolchain="go 1.24.0")],
        },
    )
    table = render_suite_table(suite, store)
    assert f"| Ryo (AOT) | 2.7 ms | 2.75 MB | {toolchain} |" in table


def _patch_discovery(monkeypatch, tmp_path: Path, suite: run.Suite) -> None:
    monkeypatch.setattr(run, "_select_suites", lambda raw: [suite])
    monkeypatch.setattr(run, "_REPO_ROOT", tmp_path)


def test_cmd_table_prints_to_stdout(tmp_path, fixed_platform, monkeypatch, capsys):
    suite = write_suite(tmp_path / "suite", TWO_ARM_SUITE)
    write_store(
        tmp_path / "benchmarks" / "data" / "synthetic.json",
        {"synthetic/Rust": [make_entry(12.3, 4.5, toolchain="rustc 1.85.0")]},
    )
    _patch_discovery(monkeypatch, tmp_path, suite)

    assert cmd_table(argparse.Namespace(benchmarks=None, out=None)) == 0

    out = capsys.readouterr().out
    assert out.startswith("| Arm | Time | Peak RSS | Version |\n")
    assert "(commit abc1234, hyperfine 2.0.0, warmup 3)" in out
    assert out.endswith("\n")


def test_cmd_table_writes_out_file_instead_of_stdout(
    tmp_path, fixed_platform, monkeypatch, capsys
):
    suite = write_suite(tmp_path / "suite", GROUPED_SUITE)
    write_store(
        tmp_path / "benchmarks" / "data" / "grouped.json",
        {"grouped/hand-rolled/Ryo (AOT)": [make_entry(2.7, 2.75, toolchain="ryo 0.1.0")]},
    )
    _patch_discovery(monkeypatch, tmp_path, suite)

    out_path = tmp_path / "table.md"
    assert cmd_table(argparse.Namespace(benchmarks=None, out=str(out_path))) == 0

    text = out_path.read_text(encoding="utf-8")
    assert text.startswith("#### hand-rolled\n| Arm | Time | Peak RSS | Version |")
    assert "(commit abc1234, hyperfine 2.0.0, warmup 5)" in text
    assert text.endswith("\n")
    assert capsys.readouterr().out == ""
