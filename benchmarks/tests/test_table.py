"""Tests for the `table` subcommand: canonical Markdown tables rendered from store data."""

import argparse
import json
from datetime import datetime, timezone
from pathlib import Path

import pytest

import run
from run import Store, _delta_pct, cmd_table, load_suite, render_suite_table

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

UNMEASURED_FIRST_SUITE = """
name = "synthetic"
warmup = 3

[[arm]]
name = "Swift"
language = "swift"
run = "/bin/echo b"
expect = "b"

[[arm]]
name = "Rust"
language = "rust"
run = "/bin/echo a"
expect = "a"
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
        "| Language or Arm | Version | Mean Time | Speed vs Best | Memory | Δ prev |\n"
        "|---|---|---|---|---|---|\n"
        "| Swift | 6.3.3 | 7 ms | 1.00x | — | — |\n"
        "| Rust | 1.85.0 | 12.3 ms | 1.76x slower | ⭐ 4.5 MB | — |\n"
        "\n"
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
        "| Language or Arm | Version | Mean Time | Speed vs Best | Memory | Δ prev |\n"
        "|---|---|---|---|---|---|\n"
        "| Ryo (AOT) | 0.1.0 | 2.7 ms | 1.00x | ⭐ 2.75 MB | — |\n"
        "\n"
        "Measured on Darwin/arm64/Apple M3 Max, 2026-10-21 "
        "(commit abc1234, hyperfine 2.0.0, warmup 5)\n"
        "\n"
        "#### stdlib\n"
        "| Language or Arm | Version | Mean Time | Speed vs Best | Memory | Δ prev |\n"
        "|---|---|---|---|---|---|\n"
        "| Go | 1.24.0 | 1.6 ms | 1.00x | ⭐ 3 MB | — |\n"
        "\n"
        "Measured on Darwin/arm64/Apple M3 Max, 2026-10-21 "
        "(commit abc1234, hyperfine 2.0.0, warmup 5)"
    )


def test_missing_memory_bench_renders_dash(tmp_path, fixed_platform):
    """A hyperfine export without memory_peak_resident stores rss value None;
    the Memory cell must render as — while time still renders."""
    suite = write_suite(tmp_path, TWO_ARM_SUITE)
    store = write_store(
        tmp_path / "data" / "synthetic.json",
        {
            "synthetic/Rust": [make_entry(12.3, 4.5, toolchain="rustc 1.85.0")],
            "synthetic/Swift": [make_entry(7.0, None, toolchain="swiftc 6.3.3")],
        },
    )
    table = render_suite_table(suite, store)
    assert "| Swift | 6.3.3 | 7 ms | 1.00x | — | — |" in table
    assert "| Rust | 1.85.0 | 12.3 ms | 1.76x slower | ⭐ 4.5 MB | — |" in table


def test_version_cell_is_bare_version_stripped_of_tool_name(tmp_path, fixed_platform):
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
    assert "| Ryo (AOT) | 0.1.0-dev.20261021+63078ac | 2.7 ms | 1.00x | ⭐ 2.75 MB | — |" in table


def test_table_uses_historical_format_and_sorts_fastest_first(tmp_path, fixed_platform):
    suite = write_suite(tmp_path, TWO_ARM_SUITE)
    store = write_store(
        tmp_path / "data" / "synthetic.json",
        {
            "synthetic/Rust": [make_entry(100.0, 1.5, toolchain="rustc 1.98.0")],
            "synthetic/Swift": [
                make_entry(200.0, 1.4, toolchain="ryo 0.1.0-dev.20261021+63078ac")
            ],
        },
    )
    text = render_suite_table(suite, store)
    assert "| Language or Arm | Version | Mean Time | Speed vs Best | Memory | Δ prev |" in text
    assert "| Rust | 1.98.0 | 100 ms | 1.00x | 1.5 MB | — |" in text
    assert "| Swift | 0.1.0-dev.20261021+63078ac | 200 ms | 2.00x slower | ⭐ 1.4 MB | — |" in text
    assert text.index("| Rust |") < text.index("| Swift |")


def test_table_stars_leanest_memory_only(tmp_path, fixed_platform):
    """The lowest measured RSS cell carries ⭐; every other Memory cell does not."""
    suite = write_suite(tmp_path, TWO_ARM_SUITE)
    store = write_store(
        tmp_path / "data" / "synthetic.json",
        {
            "synthetic/Rust": [make_entry(100.0, 1.5, toolchain="rustc 1.98.0")],
            "synthetic/Swift": [make_entry(50.0, 1.4, toolchain="swiftc 6.3.3")],
        },
    )
    text = render_suite_table(suite, store)
    # Swift is fastest now, so ⭐ must ride on memory, not on row order.
    assert "| Swift | 6.3.3 | 50 ms | 1.00x | ⭐ 1.4 MB | — |" in text
    assert "| Rust | 1.98.0 | 100 ms | 2.00x slower | 1.5 MB | — |" in text
    assert text.count("⭐") == 1


def test_unmeasured_arm_renders_dashes_and_sorts_last(tmp_path, fixed_platform):
    """An arm with no store entry renders — in every cell, after the measured
    arms, regardless of its manifest position."""
    suite = write_suite(tmp_path, UNMEASURED_FIRST_SUITE)
    store = write_store(
        tmp_path / "data" / "synthetic.json",
        {"synthetic/Rust": [make_entry(12.3, 4.5, toolchain="rustc 1.85.0")]},
    )
    text = render_suite_table(suite, store)
    assert "| Swift | — | — | — | — | — |" in text
    assert "| Rust | 1.85.0 | 12.3 ms | 1.00x | ⭐ 4.5 MB | — |" in text
    assert text.index("| Rust |") < text.index("| Swift |")


def _patch_discovery(monkeypatch, tmp_path: Path, suite: run.Suite) -> None:
    monkeypatch.setattr(run, "_select_suites", lambda raw, **kwargs: [suite])
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
    assert out.startswith("| Language or Arm | Version | Mean Time | Speed vs Best | Memory | Δ prev |\n")
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
    assert text.startswith(
        "#### hand-rolled\n"
        "| Language or Arm | Version | Mean Time | Speed vs Best | Memory | Δ prev |"
    )
    assert "(commit abc1234, hyperfine 2.0.0, warmup 5)" in text
    assert text.endswith("\n")
    assert capsys.readouterr().out == ""


def test_delta_prev_column_two_entries(tmp_path, fixed_platform):
    """Two store entries per key: the last cell is the signed time delta vs the
    previous entry and the Memory cell appends the signed RSS delta."""
    suite = write_suite(tmp_path, TWO_ARM_SUITE)
    store = write_store(
        tmp_path / "data" / "synthetic.json",
        {
            "synthetic/Rust": [
                make_entry(100.0, 1.5, toolchain="rustc 1.98.0"),
                make_entry(90.0, 1.4, toolchain="rustc 1.98.0"),
            ],
            "synthetic/Swift": [make_entry(200.0, 1.3, toolchain="swiftc 6.3.3")],
        },
    )
    text = render_suite_table(suite, store)
    assert "| Language or Arm | Version | Mean Time | Speed vs Best | Memory | Δ prev |" in text
    assert "| Rust | 1.98.0 | 90 ms | 1.00x | 1.4 MB (-6.7%) | -10.0% |" in text


def test_delta_prev_single_entry_renders_dash(tmp_path, fixed_platform):
    """One entry per key: no previous exists, so the delta column renders —."""
    suite = write_suite(tmp_path, TWO_ARM_SUITE)
    store = write_store(
        tmp_path / "data" / "synthetic.json",
        {
            "synthetic/Rust": [make_entry(100.0, 1.5, toolchain="rustc 1.98.0")],
            "synthetic/Swift": [make_entry(200.0, 1.4, toolchain="swiftc 6.3.3")],
        },
    )
    text = render_suite_table(suite, store)
    assert "| Rust | 1.98.0 | 100 ms | 1.00x | 1.5 MB | — |" in text


def test_delta_pct_zero_previous_renders_dash():
    assert _delta_pct(100.0, 0.0) == "—"
    assert _delta_pct(None, 1.0) == "—"
    assert _delta_pct(1.0, None) == "—"
    assert _delta_pct(90.0, 100.0) == "-10.0%"
    assert _delta_pct(104.392, 111.626) == "-6.5%"


def test_star_and_memory_delta_coexist(tmp_path, fixed_platform):
    """The leanest-memory arm is not the fastest: its Memory cell carries both
    the ⭐ marker and the parenthesized RSS delta."""
    suite = write_suite(tmp_path, TWO_ARM_SUITE)
    store = write_store(
        tmp_path / "data" / "synthetic.json",
        {
            "synthetic/Rust": [make_entry(50.0, 2.0, toolchain="rustc 1.98.0")],
            "synthetic/Swift": [
                make_entry(100.0, 1.5, toolchain="swiftc 6.3.3"),
                make_entry(200.0, 1.4, toolchain="swiftc 6.3.3"),
            ],
        },
    )
    text = render_suite_table(suite, store)
    assert "| Swift | 6.3.3 | 200 ms | 4.00x slower | ⭐ 1.4 MB (-6.7%) | +100.0% |" in text
