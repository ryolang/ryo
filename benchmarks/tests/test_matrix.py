"""Tests for the cross-suite matrix rendering cell helpers: column keys,
ratio emoji buckets, and ratio formatting."""

import json
from datetime import datetime, timezone
from pathlib import Path

from run import Arm, Store, _format_ratio, _matrix_column_key, _ratio_emoji

EPOCH_MS = int(datetime(2026, 10, 21, tzinfo=timezone.utc).timestamp() * 1000)


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
