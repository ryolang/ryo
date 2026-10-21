"""Tests for the `update-readme` subcommand: marker-block splicing, the root
summary and matrix tables, and stale-prose warnings."""

import argparse
import json
from datetime import datetime, timezone
from pathlib import Path

import pytest

import run
from run import (
    BENCH_END,
    BENCH_START,
    MATRIX_END,
    MATRIX_START,
    SUMMARY_END,
    SUMMARY_START,
    MarkerError,
    Store,
    cmd_update_readme,
    load_suite,
    render_matrix,
    render_summary_table,
    splice_block,
    stale_prose_warnings,
)

PLATFORM = "Darwin/arm64/Apple M3 Max"
EPOCH_MS = int(datetime(2026, 10, 21, tzinfo=timezone.utc).timestamp() * 1000)
DAY_MS = 86_400_000
OLDER_EPOCH_MS = EPOCH_MS - DAY_MS

TWO_ARM_SUITE = """
name = "synthetic"
warmup = 3

[[arm]]
name = "Rust"
language = "rust"
run = "/bin/echo a"
expect = "a"

[[arm]]
name = "Ryo (AOT)"
language = "ryo"
run = "/bin/echo b"
expect = "b"
"""

THREE_ARM_SUITE = TWO_ARM_SUITE + """

[[arm]]
name = "Ryo (JIT)"
language = "ryo"
run = "/bin/echo c"
expect = "c"
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


def _patch_discovery(monkeypatch, tmp_path: Path, suite: run.Suite) -> None:
    monkeypatch.setattr(run, "_select_suites", lambda raw, **kwargs: [suite])
    monkeypatch.setattr(run, "_REPO_ROOT", tmp_path)


def test_splice_block_replaces_content_between_markers():
    text = (
        "# Synthetic\n"
        "\n"
        "<!-- BENCH:START -->\n"
        "stale table\n"
        "<!-- BENCH:END -->\n"
        "\n"
        "Trailing prose.\n"
    )
    assert splice_block(text, BENCH_START, BENCH_END, "fresh table") == (
        "# Synthetic\n"
        "\n"
        "<!-- BENCH:START -->\n"
        "fresh table\n"
        "<!-- BENCH:END -->\n"
        "\n"
        "Trailing prose.\n"
    )


def test_splice_block_is_idempotent():
    text = "<!-- BENCH:START -->\nold\n<!-- BENCH:END -->\n"
    once = splice_block(text, BENCH_START, BENCH_END, "new")
    assert splice_block(once, BENCH_START, BENCH_END, "new") == once


def test_splice_block_missing_markers_raise_marker_error():
    with pytest.raises(MarkerError):
        splice_block("no markers at all", BENCH_START, BENCH_END, "x")
    with pytest.raises(MarkerError):
        splice_block("<!-- BENCH:START -->\n", BENCH_START, BENCH_END, "x")
    # An end marker appearing only before the start marker is still missing.
    with pytest.raises(MarkerError):
        splice_block("<!-- BENCH:END -->\n<!-- BENCH:START -->\n", BENCH_START, BENCH_END, "x")


SUMMARY_HEADER = (
    "| Suite | Languages | Ryo AOT | Peak RSS | vs. fastest | vs. leanest | Version | Measured |"
)
SUMMARY_SEPARATOR = "|---|---|---|---|---|---|---|---|"


def test_render_summary_table_anchors_to_ryo_aot_arm(tmp_path):
    suite = write_suite(tmp_path / "suite", THREE_ARM_SUITE)
    store = write_store(
        tmp_path / "data" / "synthetic.json",
        {
            "synthetic/Rust": [make_entry(1.6, 4.5, toolchain="rustc 1.85.0")],
            "synthetic/Ryo (AOT)": [make_entry(2.7, 2.75, toolchain="ryo 0.1.0")],
            "synthetic/Ryo (JIT)": [make_entry(55.0, 9.0, toolchain="ryo 0.1.0")],
        },
    )
    assert render_summary_table([suite], {"synthetic": store}) == (
        f"{SUMMARY_HEADER}\n"
        f"{SUMMARY_SEPARATOR}\n"
        "| [synthetic](./suite/) | Rust, Ryo (AOT), Ryo (JIT) | 2.7 ms | 2.75 MB "
        "| 1.69x | 1.00x | ryo 0.1.0 / rustc 1.85.0 | 2026-10-21 (commit abc1234) |"
    )


def test_render_summary_table_falls_back_to_ryo_toolchain_without_aot_name(tmp_path):
    suite = write_suite(
        tmp_path / "suite",
        """
name = "synthetic"
warmup = 3

[[arm]]
name = "Rust"
language = "rust"
run = "/bin/echo a"
expect = "a"

[[arm]]
name = "Ryo (JIT)"
language = "ryo"
run = "/bin/echo b"
expect = "b"
""",
    )
    store = write_store(
        tmp_path / "data" / "synthetic.json",
        {
            "synthetic/Rust": [make_entry(1.6, 4.5, toolchain="rustc 1.85.0")],
            "synthetic/Ryo (JIT)": [make_entry(55.0, 9.0, toolchain="ryo 0.1.0")],
        },
    )
    table = render_summary_table([suite], {"synthetic": store})
    # The JIT arm anchors the row, and the ratios run against Rust (same run).
    assert (
        "| [synthetic](./suite/) | Rust, Ryo (JIT) | 55 ms | 9 MB "
        "| 34.38x | 2.00x | ryo 0.1.0 / rustc 1.85.0 | 2026-10-21 (commit abc1234) |"
    ) in table


def test_render_summary_table_languages_come_from_the_manifest(tmp_path):
    """The Languages cell lists the manifest's arm names in manifest order,
    independent of which arms have store entries."""
    suite = write_suite(
        tmp_path / "reversed",
        """
name = "reversed"
warmup = 3

[[arm]]
name = "Swift"
language = "swift"
run = "/bin/echo a"
expect = "a"

[[arm]]
name = "Rust"
language = "rust"
run = "/bin/echo b"
expect = "b"

[[arm]]
name = "Ryo (AOT)"
language = "ryo"
run = "/bin/echo c"
expect = "c"
""",
    )
    store = write_store(
        tmp_path / "data" / "reversed.json",
        {
            "reversed/Rust": [make_entry(1.6, 4.5, toolchain="rustc 1.85.0")],
            "reversed/Ryo (AOT)": [make_entry(2.7, 2.75, toolchain="ryo 0.1.0")],
        },
    )
    table = render_summary_table([suite], {"reversed": store})
    assert "| [reversed](./reversed/) | Swift, Rust, Ryo (AOT) |" in table


def test_render_summary_table_ratio_dash_when_fastest_is_from_older_run(tmp_path):
    """The fastest arm's latest entry predates the Ryo-AOT run; a ratio across
    different runs is dishonest, so the cell renders —. The Rust arm is also
    from that older run, so its toolchain drops out of the Version cell too."""
    suite = write_suite(tmp_path / "suite", TWO_ARM_SUITE)
    store = write_store(
        tmp_path / "data" / "synthetic.json",
        {
            "synthetic/Rust": [make_entry(1.6, 4.5, toolchain="rustc 1.85.0", date=OLDER_EPOCH_MS)],
            "synthetic/Ryo (AOT)": [make_entry(2.7, 2.75, toolchain="ryo 0.1.0")],
        },
    )
    table = render_summary_table([suite], {"synthetic": store})
    assert (
        "| [synthetic](./suite/) | Rust, Ryo (AOT) | 2.7 ms | 2.75 MB "
        "| — | 1.00x | ryo 0.1.0 | 2026-10-21 (commit abc1234) |"
    ) in table


def test_render_summary_table_leanest_ratio_and_dash_cases(tmp_path):
    """vs. leanest mirrors vs. fastest on memory_peak_rss: filled only when the
    leanest arm's latest entry shares the anchor's run."""
    suite = write_suite(tmp_path / "suite", THREE_ARM_SUITE)
    store = write_store(
        tmp_path / "data" / "synthetic.json",
        {
            # Same run as the anchor: fastest arm, not the leanest.
            "synthetic/Rust": [make_entry(1.6, 4.5, toolchain="rustc 1.85.0")],
            "synthetic/Ryo (AOT)": [make_entry(2.7, 2.75, toolchain="ryo 0.1.0")],
            # Leaner than everything, but from an older run: the ratio is —.
            "synthetic/Ryo (JIT)": [make_entry(55.0, 0.5, toolchain="ryo 0.1.0", date=OLDER_EPOCH_MS)],
        },
    )
    table = render_summary_table([suite], {"synthetic": store})
    assert "| 1.69x | — |" in table


def test_render_summary_table_leanest_dash_when_min_rss_is_zero(tmp_path):
    """A zero min RSS would divide by zero; the cell renders — instead."""
    suite = write_suite(tmp_path / "suite", THREE_ARM_SUITE)
    store = write_store(
        tmp_path / "data" / "synthetic.json",
        {
            "synthetic/Rust": [make_entry(1.6, 4.5, toolchain="rustc 1.85.0")],
            "synthetic/Ryo (AOT)": [make_entry(2.7, 2.75, toolchain="ryo 0.1.0")],
            "synthetic/Ryo (JIT)": [make_entry(55.0, 0.0, toolchain="ryo 0.1.0")],
        },
    )
    table = render_summary_table([suite], {"synthetic": store})
    assert "| 1.69x | — |" in table


def test_render_summary_table_version_ryo_first_then_manifest_order(tmp_path):
    """Version composes toolchains from same-run entries only: ryo first, then
    manifest arm order, deduped (the two ryo arms share one version string)."""
    suite = write_suite(tmp_path / "suite", THREE_ARM_SUITE)
    store = write_store(
        tmp_path / "data" / "synthetic.json",
        {
            "synthetic/Rust": [make_entry(1.6, 4.5, toolchain="rustc 1.85.0")],
            "synthetic/Ryo (AOT)": [make_entry(2.7, 2.75, toolchain="ryo 0.1.0")],
            "synthetic/Ryo (JIT)": [make_entry(55.0, 9.0, toolchain="ryo 0.1.0")],
        },
    )
    table = render_summary_table([suite], {"synthetic": store})
    assert "ryo 0.1.0 / rustc 1.85.0" in table
    assert table.count("ryo 0.1.0") == 1


def test_render_summary_table_version_omits_missing_and_other_run_entries(tmp_path):
    """An arm whose entry is missing, or whose latest entry is from another
    run, contributes no toolchain to the Version cell."""
    suite = write_suite(tmp_path / "suite", THREE_ARM_SUITE)
    store = write_store(
        tmp_path / "data" / "synthetic.json",
        {
            "synthetic/Rust": [make_entry(1.6, 4.5, toolchain="rustc 1.85.0", date=OLDER_EPOCH_MS)],
            "synthetic/Ryo (AOT)": [make_entry(2.7, 2.75, toolchain="ryo 0.1.0")],
            # No synthetic/Ryo (JIT) entry at all.
        },
    )
    table = render_summary_table([suite], {"synthetic": store})
    row = "| [synthetic](./suite/) | Rust, Ryo (AOT), Ryo (JIT) | 2.7 ms | 2.75 MB "
    assert row + "| — | 1.00x | ryo 0.1.0 | 2026-10-21 (commit abc1234) |" in table
    assert "rustc" not in table


def test_render_summary_table_skips_anchorless_suites(tmp_path):
    """Suites with no Ryo-AOT anchor entry render no row at all."""
    suite = write_suite(
        tmp_path / "anchored",
        TWO_ARM_SUITE.replace('name = "synthetic"', 'name = "anchored"'),
    )
    anchored = write_store(
        tmp_path / "data" / "anchored.json",
        {
            "anchored/Rust": [make_entry(3.0, 4.5, toolchain="rustc 1.85.0")],
            "anchored/Ryo (AOT)": [make_entry(2.7, 2.75, toolchain="ryo 0.1.0")],
        },
    )
    anchorless = write_store(
        tmp_path / "data" / "anchorless.json",
        {"anchorless/Rust": [make_entry(1.6, 4.5, toolchain="rustc 1.85.0")]},
    )
    anchorless_suite = write_suite(
        tmp_path / "anchorless",
        TWO_ARM_SUITE.replace('name = "synthetic"', 'name = "anchorless"'),
    )
    assert render_summary_table(
        [anchorless_suite, suite], {"anchorless": anchorless, "anchored": anchored}
    ) == (
        f"{SUMMARY_HEADER}\n"
        f"{SUMMARY_SEPARATOR}\n"
        "| [anchored](./anchored/) | Rust, Ryo (AOT) | 2.7 ms | 2.75 MB "
        "| 1.00x | 1.00x | ryo 0.1.0 / rustc 1.85.0 | 2026-10-21 (commit abc1234) |"
    )


def test_stale_prose_warnings_flag_values_absent_from_table():
    readme = (
        "# Synthetic\n"
        "\n"
        "In practice the suite needs 1.34 MB extra and runs 5x slower under load.\n"
        "\n"
        "<!-- BENCH:START -->\n"
        "| Arm | Time |\n"
        "| ignored 999 ms row inside markers |\n"
        "<!-- BENCH:END -->\n"
    )
    table = "| Arm | Time |\n|---|---|\n| Rust | 12.3 ms |"
    warnings = stale_prose_warnings(readme, table)
    assert any("1.34" in w for w in warnings)
    assert any('"5x"' in w for w in warnings)
    # Values inside marker blocks are generated content, not prose.
    assert not any("999" in w for w in warnings)
    assert not any("12.3" in w for w in warnings)


def test_stale_prose_warnings_silent_when_value_appears_in_table():
    readme = "Peak RSS lands at 4.5 MB today.\n"
    table = "| Arm | Peak RSS |\n|---|---|\n| Rust | 4.5 MB |"
    assert stale_prose_warnings(readme, table) == []


def test_cmd_update_readme_splices_suite_and_root_readmes(
    tmp_path, fixed_platform, monkeypatch
):
    suite = write_suite(tmp_path / "suite", TWO_ARM_SUITE)
    store = write_store(
        tmp_path / "benchmarks" / "data" / "synthetic.json",
        {
            "synthetic/Rust": [make_entry(12.3, 4.5, toolchain="rustc 1.85.0")],
            "synthetic/Ryo (AOT)": [make_entry(2.7, 2.75, toolchain="ryo 0.1.0")],
        },
    )
    suite_readme = tmp_path / "suite" / "README.md"
    suite_readme.write_text(
        "# Synthetic\n"
        "\n"
        "Rust takes 12.3 ms here.\n"
        "\n"
        "<!-- BENCH:START -->\n"
        "<!-- BENCH:END -->\n"
        "\n"
        "## How to run\n",
        encoding="utf-8",
    )
    root_readme = tmp_path / "benchmarks" / "README.md"
    root_readme.write_text(
        "# Benchmarks\n"
        "\n"
        "<!-- BENCH-SUMMARY:START -->\n"
        "<!-- BENCH-SUMMARY:END -->\n"
        "\n"
        "<!-- BENCH-MATRIX:START -->\n"
        "<!-- BENCH-MATRIX:END -->\n"
        "\n"
        "Curated prose stays.\n",
        encoding="utf-8",
    )
    _patch_discovery(monkeypatch, tmp_path, suite)

    assert cmd_update_readme(argparse.Namespace(benchmarks=None)) == 0

    assert suite_readme.read_text(encoding="utf-8") == (
        "# Synthetic\n"
        "\n"
        "Rust takes 12.3 ms here.\n"
        "\n"
        "<!-- BENCH:START -->\n"
        "| Language or Arm | Version | Mean Time | Speed vs Best | Memory |\n"
        "|---|---|---|---|---|\n"
        "| Ryo (AOT) | 0.1.0 | 2.7 ms | 1.00x | ⭐ 2.75 MB |\n"
        "| Rust | 1.85.0 | 12.3 ms | 4.56x slower | 4.5 MB |\n"
        "Measured on Darwin/arm64/Apple M3 Max, 2026-10-21 "
        "(commit abc1234, hyperfine 2.0.0, warmup 3)\n"
        "<!-- BENCH:END -->\n"
        "\n"
        "## How to run\n"
    )
    assert root_readme.read_text(encoding="utf-8") == (
        "# Benchmarks\n"
        "\n"
        "<!-- BENCH-SUMMARY:START -->\n"
        f"{SUMMARY_HEADER}\n"
        f"{SUMMARY_SEPARATOR}\n"
        "| [synthetic](./suite/) | Rust, Ryo (AOT) | 2.7 ms | 2.75 MB "
        "| 1.00x | 1.00x | ryo 0.1.0 / rustc 1.85.0 | 2026-10-21 (commit abc1234) |\n"
        "<!-- BENCH-SUMMARY:END -->\n"
        "\n"
        "<!-- BENCH-MATRIX:START -->\n"
        f"{render_matrix([suite], {'synthetic': store})}\n"
        "<!-- BENCH-MATRIX:END -->\n"
        "\n"
        "Curated prose stays.\n"
    )

    # Idempotent: a second run rewrites nothing.
    suite_before = suite_readme.read_text(encoding="utf-8")
    root_before = root_readme.read_text(encoding="utf-8")
    assert cmd_update_readme(argparse.Namespace(benchmarks=None)) == 0
    assert suite_readme.read_text(encoding="utf-8") == suite_before
    assert root_readme.read_text(encoding="utf-8") == root_before


def test_update_readme_splices_matrix_block(tmp_path, fixed_platform, monkeypatch):
    """The root README's BENCH-MATRIX block is re-spliced from every discovered
    suite (the same skip-ignored rule as the summary), and a second run is a
    byte-identical no-op."""
    suite = write_suite(tmp_path / "suite", TWO_ARM_SUITE)
    write_store(
        tmp_path / "benchmarks" / "data" / "synthetic.json",
        {
            "synthetic/Rust": [make_entry(12.3, 4.5, toolchain="rustc 1.85.0")],
            "synthetic/Ryo (AOT)": [make_entry(2.7, 2.75, toolchain="ryo 0.1.0")],
        },
    )
    suite_readme = tmp_path / "suite" / "README.md"
    suite_readme.write_text(
        "# Synthetic\n\n<!-- BENCH:START -->\n<!-- BENCH:END -->\n", encoding="utf-8"
    )
    root_readme = tmp_path / "benchmarks" / "README.md"
    root_readme.write_text(
        "# Benchmarks\n"
        "\n"
        "<!-- BENCH-SUMMARY:START -->\n"
        "<!-- BENCH-SUMMARY:END -->\n"
        "\n"
        "<!-- BENCH-MATRIX:START -->\n"
        "<!-- BENCH-MATRIX:END -->\n",
        encoding="utf-8",
    )
    _patch_discovery(monkeypatch, tmp_path, suite)

    assert cmd_update_readme(argparse.Namespace(benchmarks=None)) == 0

    text = root_readme.read_text(encoding="utf-8")
    block = text.split(MATRIX_START)[1].split(MATRIX_END)[0]
    assert "**Summary (geomean)**" in block
    assert "[synthetic](./suite/)" in block

    # Idempotent: a second run rewrites nothing.
    before = root_readme.read_text(encoding="utf-8")
    assert cmd_update_readme(argparse.Namespace(benchmarks=None)) == 0
    assert root_readme.read_text(encoding="utf-8") == before


def test_cmd_update_readme_leaves_dataless_suite_readme_untouched(
    tmp_path, fixed_platform, monkeypatch
):
    """A discovered suite with no store data is skipped, not dash-filled.

    Bare ``update-readme`` on a fresh clone must reproduce the committed
    READMEs exactly: suites nobody has measured keep their blocks as-is.
    """
    measured = write_suite(tmp_path / "suite", TWO_ARM_SUITE)
    dataless = write_suite(
        tmp_path / "dataless",
        """
name = "dataless"
warmup = 0

[[arm]]
name = "Rust"
language = "rust"
run = "/bin/echo a"
expect = "a"
""",
    )
    write_store(
        tmp_path / "benchmarks" / "data" / "synthetic.json",
        {
            "synthetic/Rust": [make_entry(12.3, 4.5, toolchain="rustc 1.85.0")],
            "synthetic/Ryo (AOT)": [make_entry(2.7, 2.75, toolchain="ryo 0.1.0")],
        },
    )
    suite_readme = tmp_path / "suite" / "README.md"
    suite_readme.write_text(
        "# Synthetic\n\n<!-- BENCH:START -->\n<!-- BENCH:END -->\n", encoding="utf-8"
    )
    dataless_text = "# Dataless\n\n<!-- BENCH:START -->\n<!-- BENCH:END -->\n"
    dataless_readme = tmp_path / "dataless" / "README.md"
    dataless_readme.write_text(dataless_text, encoding="utf-8")
    root_readme = tmp_path / "benchmarks" / "README.md"
    root_readme.write_text(
        "# Benchmarks\n"
        "\n"
        "<!-- BENCH-SUMMARY:START -->\n"
        "<!-- BENCH-SUMMARY:END -->\n"
        "\n"
        "<!-- BENCH-MATRIX:START -->\n"
        "<!-- BENCH-MATRIX:END -->\n",
        encoding="utf-8",
    )
    monkeypatch.setattr(run, "_select_suites", lambda raw, **kwargs: [measured, dataless])
    monkeypatch.setattr(run, "_REPO_ROOT", tmp_path)

    assert cmd_update_readme(argparse.Namespace(benchmarks=None)) == 0

    # The unmeasured suite's README and block are byte-identical afterwards.
    assert dataless_readme.read_text(encoding="utf-8") == dataless_text
    summary = root_readme.read_text(encoding="utf-8")
    assert "synthetic" in summary
    assert "dataless" not in summary


def test_cmd_update_readme_benchmarks_filter_keeps_all_suites_in_root_summary(
    tmp_path, fixed_platform, monkeypatch
):
    """--benchmarks scopes the per-suite splices only; the root summary must
    still be rebuilt from every discovered suite, or a filtered run would
    silently delete the other suites' committed rows."""
    suite_one = write_suite(tmp_path / "suite", TWO_ARM_SUITE)
    suite_two = write_suite(
        tmp_path / "two", TWO_ARM_SUITE.replace('name = "synthetic"', 'name = "two"')
    )
    write_store(
        tmp_path / "benchmarks" / "data" / "synthetic.json",
        {
            "synthetic/Rust": [make_entry(12.3, 4.5, toolchain="rustc 1.85.0")],
            "synthetic/Ryo (AOT)": [make_entry(2.7, 2.75, toolchain="ryo 0.1.0")],
        },
    )
    write_store(
        tmp_path / "benchmarks" / "data" / "two.json",
        {
            "two/Rust": [make_entry(9.1, 4.0, toolchain="rustc 1.85.0")],
            "two/Ryo (AOT)": [make_entry(3.3, 2.5, toolchain="ryo 0.1.0")],
        },
    )
    suite_readme = tmp_path / "suite" / "README.md"
    suite_readme.write_text(
        "# Synthetic\n\n<!-- BENCH:START -->\n<!-- BENCH:END -->\n", encoding="utf-8"
    )
    two_text = "# Two\n\n<!-- BENCH:START -->\n<!-- BENCH:END -->\n"
    two_readme = tmp_path / "two" / "README.md"
    two_readme.write_text(two_text, encoding="utf-8")
    root_readme = tmp_path / "benchmarks" / "README.md"
    root_readme.write_text(
        "# Benchmarks\n"
        "\n"
        "<!-- BENCH-SUMMARY:START -->\n"
        "<!-- BENCH-SUMMARY:END -->\n"
        "\n"
        "<!-- BENCH-MATRIX:START -->\n"
        "<!-- BENCH-MATRIX:END -->\n",
        encoding="utf-8",
    )
    all_suites = [suite_one, suite_two]

    def fake_select(raw, **kwargs):
        if raw is None:
            return all_suites
        wanted = set(raw.split(","))
        return [suite for suite in all_suites if suite.name in wanted]

    monkeypatch.setattr(run, "_select_suites", fake_select)
    monkeypatch.setattr(run, "_REPO_ROOT", tmp_path)

    assert cmd_update_readme(argparse.Namespace(benchmarks="synthetic")) == 0

    # Only the selected suite's README is spliced.
    assert "| Ryo (AOT) | 0.1.0 | 2.7 ms |" in suite_readme.read_text(encoding="utf-8")
    assert two_readme.read_text(encoding="utf-8") == two_text
    # The root summary still carries both suites' rows.
    summary = root_readme.read_text(encoding="utf-8")
    assert "| [synthetic](./suite/) |" in summary
    assert "| [two](./two/) |" in summary


def test_cmd_update_readme_missing_suite_markers_is_hard_error_with_zero_mutation(
    tmp_path, fixed_platform, monkeypatch, capsys
):
    suite = write_suite(tmp_path / "suite", TWO_ARM_SUITE)
    write_store(
        tmp_path / "benchmarks" / "data" / "synthetic.json",
        {"synthetic/Rust": [make_entry(1.6, 4.5, toolchain="rustc 1.85.0")]},
    )
    suite_readme = tmp_path / "suite" / "README.md"
    suite_readme.write_text("# Synthetic\n\nNo markers here.\n", encoding="utf-8")
    root_readme = tmp_path / "benchmarks" / "README.md"
    root_text = (
        "# Benchmarks\n"
        "\n"
        "<!-- BENCH-SUMMARY:START -->\n"
        "<!-- BENCH-SUMMARY:END -->\n"
        "\n"
        "<!-- BENCH-MATRIX:START -->\n"
        "<!-- BENCH-MATRIX:END -->\n"
    )
    root_readme.write_text(root_text, encoding="utf-8")
    _patch_discovery(monkeypatch, tmp_path, suite)

    assert cmd_update_readme(argparse.Namespace(benchmarks=None)) == 1

    captured = capsys.readouterr()
    assert "error" in captured.err
    assert captured.out == ""
    assert suite_readme.read_text(encoding="utf-8") == "# Synthetic\n\nNo markers here.\n"
    assert root_readme.read_text(encoding="utf-8") == root_text


def test_cmd_update_readme_missing_root_markers_leaves_suite_readme_untouched(
    tmp_path, fixed_platform, monkeypatch, capsys
):
    suite = write_suite(tmp_path / "suite", TWO_ARM_SUITE)
    write_store(tmp_path / "benchmarks" / "data" / "synthetic.json", {})
    suite_readme = tmp_path / "suite" / "README.md"
    suite_text = "# Synthetic\n\n<!-- BENCH:START -->\n<!-- BENCH:END -->\n"
    suite_readme.write_text(suite_text, encoding="utf-8")
    root_readme = tmp_path / "benchmarks" / "README.md"
    root_readme.write_text("# Benchmarks\n\nNo markers here.\n", encoding="utf-8")
    _patch_discovery(monkeypatch, tmp_path, suite)

    assert cmd_update_readme(argparse.Namespace(benchmarks=None)) == 1

    assert "error" in capsys.readouterr().err
    assert suite_readme.read_text(encoding="utf-8") == suite_text
    assert root_readme.read_text(encoding="utf-8") == "# Benchmarks\n\nNo markers here.\n"


def test_cmd_update_readme_missing_readme_file_is_marker_error(
    tmp_path, fixed_platform, monkeypatch, capsys
):
    suite = write_suite(tmp_path / "suite", TWO_ARM_SUITE)
    write_store(
        tmp_path / "benchmarks" / "data" / "synthetic.json",
        {"synthetic/Rust": [make_entry(1.6, 4.5, toolchain="rustc 1.85.0")]},
    )
    (tmp_path / "benchmarks" / "README.md").write_text(
        "# Benchmarks\n"
        "\n"
        "<!-- BENCH-SUMMARY:START -->\n"
        "<!-- BENCH-SUMMARY:END -->\n"
        "\n"
        "<!-- BENCH-MATRIX:START -->\n"
        "<!-- BENCH-MATRIX:END -->\n",
        encoding="utf-8",
    )
    # No suite README.md is written at all.
    _patch_discovery(monkeypatch, tmp_path, suite)

    assert cmd_update_readme(argparse.Namespace(benchmarks=None)) == 1
    assert "README" in capsys.readouterr().err


def test_cmd_update_readme_prints_stale_prose_warnings_to_stderr(
    tmp_path, fixed_platform, monkeypatch, capsys
):
    suite = write_suite(tmp_path / "suite", TWO_ARM_SUITE)
    write_store(
        tmp_path / "benchmarks" / "data" / "synthetic.json",
        {
            "synthetic/Rust": [make_entry(12.3, 4.5, toolchain="rustc 1.85.0")],
            "synthetic/Ryo (AOT)": [make_entry(2.7, 2.75, toolchain="ryo 0.1.0")],
        },
    )
    (tmp_path / "suite" / "README.md").write_text(
        "# Synthetic\n\nPeak RSS lands at 1.34 MB.\n\n"
        "<!-- BENCH:START -->\n<!-- BENCH:END -->\n",
        encoding="utf-8",
    )
    (tmp_path / "benchmarks" / "README.md").write_text(
        "# Benchmarks\n"
        "\n"
        "<!-- BENCH-SUMMARY:START -->\n"
        "<!-- BENCH-SUMMARY:END -->\n"
        "\n"
        "<!-- BENCH-MATRIX:START -->\n"
        "<!-- BENCH-MATRIX:END -->\n",
        encoding="utf-8",
    )
    _patch_discovery(monkeypatch, tmp_path, suite)

    assert cmd_update_readme(argparse.Namespace(benchmarks=None)) == 0

    err = capsys.readouterr().err
    assert "warning" in err
    assert "1.34" in err
