"""Tests for run-string substitution, hyperfine driving, and schema-2 parsing."""

import json
import os
from pathlib import Path

import pytest

from run import (
    HyperfineError,
    ManifestError,
    parse_summary,
    run_hyperfine,
    substitute_run,
)

FIXTURE = Path(__file__).parent / "fixtures" / "hyperfine_schema2.json"


def _results() -> list[dict]:
    return json.loads(FIXTURE.read_text(encoding="utf-8"))["results"]


def test_substitute_ryo_and_home():
    assert substitute_run("$ryo run fib.ryo", "/x/ryo") == "/x/ryo run fib.ryo"
    expanded = substitute_run("$HOME/.bun/bin/bun run fib.ts", "/x/ryo")
    assert expanded == f"{os.environ['HOME']}/.bun/bin/bun run fib.ts"


def test_substitute_unset_var_rejected():
    with pytest.raises(ManifestError, match="unset variable"):
        substitute_run("$NO_SUCH_VAR_12345 run fib.ryo", "/x/ryo")


def test_parse_summary_converts_units():
    result = _results()[0]
    timing = parse_summary(result)
    assert timing.time_ms_mean == 412.3
    assert timing.rss_mb_mean == 13.2


def test_parse_summary_missing_memory_is_none():
    result = next(r for r in _results() if "memory_peak_resident" not in r["summary"])
    timing = parse_summary(result)
    assert timing.rss_mb_mean is None


def test_run_hyperfine_success(tmp_path):
    result = run_hyperfine("/bin/echo hi", 1, tmp_path, "echo")
    assert "time_wall_clock" in result["summary"]


def test_run_hyperfine_failure_names_label(tmp_path):
    with pytest.raises(HyperfineError) as exc_info:
        run_hyperfine("exit 3", 1, tmp_path, "BoomArm")
    assert exc_info.value.label == "BoomArm"
    assert list(tmp_path.iterdir()) == []
