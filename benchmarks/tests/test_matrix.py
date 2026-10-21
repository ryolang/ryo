"""Tests for the cross-suite matrix rendering: cell helpers (column keys,
ratio emoji buckets, ratio formatting), the render_matrix assembly, the
cmd_matrix subcommand entry point, and the stale-prose regex treatment of
the matrix marker block."""

import argparse
import json
from datetime import datetime, timezone
from pathlib import Path

import run
from run import (
    Arm,
    Store,
    _format_ratio,
    _matrix_column_key,
    _ratio_emoji,
    cmd_matrix,
    load_suite,
    render_matrix,
    stale_prose_warnings,
)

EPOCH_MS = int(datetime(2026, 10, 21, tzinfo=timezone.utc).timestamp() * 1000)
DAY_MS = 86_400_000

# One Rust + one Ryo (AOT) arm; suite name/category are filled in per fixture.
RUST_RYO_TOML = """
name = "{name}"
{category}[[arm]]
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

TWO_RUST_TOML = """
name = "solo"

[[arm]]
name = "Rust"
language = "rust"
run = "/bin/echo a"
expect = "a"

[[arm]]
name = "Rust serde_json"
language = "rust"
run = "/bin/echo c"
expect = "c"
"""

RUST_NIM_TOML = """
name = "solo"

[[arm]]
name = "Rust"
language = "rust"
run = "/bin/echo a"
expect = "a"

[[arm]]
name = "Nim"
language = "nim"
run = "/bin/echo d"
expect = "d"
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


def write_suite(directory: Path, text: str):
    directory.mkdir(parents=True, exist_ok=True)
    manifest = directory / "suite.toml"
    manifest.write_text(text.strip(), encoding="utf-8")
    return load_suite(manifest)


def rust_ryo_suites(tmp_path: Path) -> tuple:
    """alpha (category Strings) and beta (category Algorithms), Rust + Ryo arms."""
    alpha = write_suite(
        tmp_path / "alpha",
        RUST_RYO_TOML.format(name="alpha", category='category = "Strings"\n'),
    )
    beta = write_suite(
        tmp_path / "beta",
        RUST_RYO_TOML.format(name="beta", category='category = "Algorithms"\n'),
    )
    return alpha, beta


def alpha_beta_stores(tmp_path: Path, ryo_ms: float) -> dict:
    stores = {}
    for name, ryo_rss in (("alpha", 1.4), ("beta", 1.4)):
        stores[name] = write_store(
            tmp_path / name / "store.json",
            {
                f"{name}/Rust": [make_entry(100.0, 1.5)],
                f"{name}/Ryo (AOT)": [
                    make_entry(ryo_ms, ryo_rss, toolchain="ryo 0.1.0")
                ],
            },
        )
    return stores


def test_ryo_column_key_variants():
    aot = Arm(name="Ryo (AOT, Eager)", language="ryo", run="x", build=None, expect=None, group=None, version=None)
    jit = Arm(name="Ryo (JIT)", language="ryo", run="x", build=None, expect=None, group=None, version=None)
    bare = Arm(name="Ryo", language="ryo", run="x", build=None, expect=None, group=None, version=None)
    assert _matrix_column_key(aot) == "ryo-aot"
    assert _matrix_column_key(jit) == "ryo-jit"
    assert _matrix_column_key(bare) == "ryo-aot"  # documented default


def test_ratio_emoji_buckets():
    assert _ratio_emoji(1.0) == "⭐"
    assert _ratio_emoji(1.05) == "🟩"
    assert _ratio_emoji(1.2) == "🟩"
    assert _ratio_emoji(1.21) == "🟨"
    assert _ratio_emoji(3.0) == "🟨"
    assert _ratio_emoji(3.01) == "🟥"


def test_format_ratio():
    assert _format_ratio(1.0) == "1.00×"
    assert _format_ratio(2.1413) == "2.14×"
    assert _format_ratio(18.94) == "18.9×"
    assert _format_ratio(138.4) == "138×"


def test_render_matrix_full_block(tmp_path):
    alpha, beta = rust_ryo_suites(tmp_path)
    stores = alpha_beta_stores(tmp_path, ryo_ms=200.0)
    stores["beta"] = write_store(
        tmp_path / "beta" / "store.json",
        {
            "beta/Rust": [make_entry(100.0, 1.5)],
            "beta/Ryo (AOT)": [make_entry(400.0, 1.4, toolchain="ryo 0.1.0")],
        },
    )
    text = render_matrix([alpha, beta], stores)
    assert "⭐ fastest in row" in text  # legend
    assert "**Runtime (mean)**" in text  # caption above the table, not a table row
    assert "| **Runtime (mean)** |" not in text
    # Ryo AOT is the first column.
    assert "| [alpha](./alpha/)<br>2026-10-21 | 🟨 200 [2.00×] (0.1.0) |" in text
    assert "| [beta](./beta/)<br>2026-10-21 | 🟥 400 [4.00×] (0.1.0) |" in text
    # sqrt(2*4)=2.83; 2.83 <= 3.0 -> 🟨.
    assert "| **Summary (geomean)** | 🟨 2.83× | ⭐ 1.00× |" in text
    assert "| **Algorithms** |" in text and "| **Strings** |" in text
    assert text.index("| **Algorithms** |") < text.index("| **Strings** |")
    assert "**Peak memory (max RSS)**" in text


def test_render_matrix_single_measured_arm_row(tmp_path):
    solo = write_suite(tmp_path / "solo", RUST_RYO_TOML.format(name="solo", category=""))
    store = write_store(
        tmp_path / "solo" / "store.json",
        {"solo/Rust": [make_entry(100.0, 1.5, toolchain="rustc 1.98.0")]},
    )
    text = render_matrix([solo], {"solo": store})
    assert "| [solo](./solo/)<br>2026-10-21 | ⭐ 100 [1.00×] (1.98.0) |" in text


def test_render_matrix_no_data_renders_placeholder(tmp_path):
    suite = write_suite(tmp_path / "solo", RUST_RYO_TOML.format(name="solo", category=""))
    empty_store = write_store(tmp_path / "solo" / "store.json", {})
    assert "*(no measurements yet)*" in render_matrix([suite], {suite.name: empty_store})


def test_render_matrix_fastest_arm_wins_language_column(tmp_path):
    solo = write_suite(tmp_path / "solo", TWO_RUST_TOML)
    store = write_store(
        tmp_path / "solo" / "store.json",
        {
            "solo/Rust": [make_entry(500.0, 2.0)],
            "solo/Rust serde_json": [make_entry(50.0, 2.0)],
        },
    )
    text = render_matrix([solo], {"solo": store})
    assert "| [solo](./solo/)<br>2026-10-21 | ⭐ 50 [1.00×] (1.85.0) |" in text
    assert "500" not in text


def test_render_matrix_newest_date_wins(tmp_path):
    solo = write_suite(tmp_path / "solo", RUST_RYO_TOML.format(name="solo", category=""))
    store = write_store(
        tmp_path / "solo" / "store.json",
        {
            "solo/Ryo (AOT)": [make_entry(200.0, 1.4, toolchain="ryo 0.1.0")],
            "solo/Rust": [make_entry(100.0, 1.5, date=EPOCH_MS - DAY_MS)],
        },
    )
    text = render_matrix([solo], {"solo": store})
    assert "| [solo](./solo/)<br>2026-10-21 |" in text


def test_render_matrix_missing_rss_keeps_time_cell(tmp_path):
    alpha, _ = rust_ryo_suites(tmp_path)
    gamma = write_suite(tmp_path / "gamma", RUST_RYO_TOML.format(name="gamma", category=""))
    stores = alpha_beta_stores(tmp_path, ryo_ms=200.0)
    stores["gamma"] = write_store(
        tmp_path / "gamma" / "store.json",
        {
            "gamma/Rust": [make_entry(100.0, 1.5, toolchain="rustc 1.98.0")],
            "gamma/Ryo (AOT)": [make_entry(200.0, None, toolchain="ryo 0.1.0")],
        },
    )
    text = render_matrix([alpha, gamma], stores)
    # Time cell intact even though the entry has no RSS.
    assert "| [gamma](./gamma/)<br>2026-10-21 | 🟨 200 [2.00×] (0.1.0) |" in text
    # Memory table: the RSS-less arm renders "—" in its column.
    assert "| [gamma](./gamma/)<br>2026-10-21 | — | ⭐ 1.5 [1.00×] (1.98.0) |" in text
    # gamma is uncategorized, so its group sorts after the real categories.
    assert "| **Other** |" in text
    assert text.index("| **Strings** |") < text.index("| **Other** |")


def test_render_matrix_unknown_language_appends_column(tmp_path):
    solo = write_suite(tmp_path / "solo", RUST_NIM_TOML)
    store = write_store(
        tmp_path / "solo" / "store.json",
        {
            "solo/Rust": [make_entry(100.0, 1.5)],
            "solo/Nim": [make_entry(400.0, 1.5, toolchain="nim 2.0.0")],
        },
    )
    text = render_matrix([solo], {"solo": store})
    assert "| Suite | Rust | Nim |" in text  # unknown language last, capitalized
    row = (
        "| [solo](./solo/)<br>2026-10-21 | ⭐ 100 [1.00×] (1.85.0) "
        "| 🟥 400 [4.00×] (2.0.0) |"
    )
    assert row in text


def _patch_discovery(monkeypatch, tmp_path: Path, suite) -> None:
    """Mirror test_table._patch_discovery: fixed selection, repo root at tmp."""
    monkeypatch.setattr(run, "_select_suites", lambda raw, **kwargs: [suite])
    monkeypatch.setattr(run, "_REPO_ROOT", tmp_path)


def _write_solo(tmp_path: Path):
    """One uncategorized suite with a store under benchmarks/data, as cmd_matrix finds it."""
    suite = write_suite(tmp_path / "suite", RUST_RYO_TOML.format(name="solo", category=""))
    store = write_store(
        tmp_path / "benchmarks" / "data" / "solo.json",
        {
            "solo/Rust": [make_entry(100.0, 1.5)],
            "solo/Ryo (AOT)": [make_entry(200.0, 1.4, toolchain="ryo 0.1.0")],
        },
    )
    return suite, store


def test_cmd_matrix_prints_to_stdout(tmp_path, monkeypatch, capsys):
    suite, store = _write_solo(tmp_path)
    _patch_discovery(monkeypatch, tmp_path, suite)

    assert cmd_matrix(argparse.Namespace(benchmarks=None, out=None, include_ignored=False)) == 0

    assert capsys.readouterr().out == render_matrix([suite], {"solo": store}) + "\n"


def test_cmd_matrix_writes_out_file_instead_of_stdout(
    tmp_path, monkeypatch, capsys
):
    suite, store = _write_solo(tmp_path)
    _patch_discovery(monkeypatch, tmp_path, suite)

    out_path = tmp_path / "matrix.md"
    assert cmd_matrix(
        argparse.Namespace(benchmarks=None, out=str(out_path), include_ignored=False)
    ) == 0

    text = out_path.read_text(encoding="utf-8")
    assert text == render_matrix([suite], {"solo": store}) + "\n"
    assert capsys.readouterr().out == ""


def test_cmd_matrix_unknown_benchmarks_name_errors(tmp_path, monkeypatch, capsys):
    write_suite(tmp_path / "benchmarks" / "solo", RUST_RYO_TOML.format(name="solo", category=""))
    monkeypatch.setattr(run, "_REPO_ROOT", tmp_path)

    assert cmd_matrix(
        argparse.Namespace(benchmarks="bogus", out=None, include_ignored=False)
    ) == 1

    assert "unknown --benchmarks name(s): bogus" in capsys.readouterr().err


def test_marker_block_regex_excludes_matrix():
    readme = "prose 1.7 ms\n<!-- BENCH-MATRIX:START -->\n| x |\n| 2.75 |\n<!-- BENCH-MATRIX:END -->\n"
    assert stale_prose_warnings(readme, "| 1.7 |") == []


def test_marker_block_regex_excludes_matrix_unit_numbers():
    """Unit-carrying numbers inside the matrix block are generated content,
    not prose — without the regex exclusion they scan as stale citations."""
    readme = "prose\n<!-- BENCH-MATRIX:START -->\n| 2.75 ms |\n<!-- BENCH-MATRIX:END -->\n"
    assert stale_prose_warnings(readme, "| 1.7 |") == []
