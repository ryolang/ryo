#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.15"
# ///
"""Centralized benchmark runner driven by per-suite suite.toml manifests.

Replaces the copy-pasted benchmarks/*/run_benchmarks.sh scripts with one
runner: `run` times every arm via hyperfine, `table` renders results, and
`update-readme` splices them into suite READMEs. See ISSUES.md I-174.

Subcommands: run, table, update-readme.
"""

import argparse
import json
import os
import re
import shutil
import subprocess
import tempfile
import tomllib
from dataclasses import dataclass
from pathlib import Path


class ManifestError(ValueError):
    """A suite.toml manifest is missing required data or violates runner rules."""


@dataclass
class Arm:
    name: str
    language: str
    run: str
    build: str | None
    expect: str | None
    group: str | None
    version: str | None


@dataclass
class Suite:
    name: str
    descriptor: str
    warmup: int
    default_expect: str | None
    arms: list[Arm]
    dir: Path


# Run strings never pass through a shell, so shell metacharacters are banned.
# Substitution points ($ryo, $HOME, ...) are scanned out first: they are
# expanded by the runner itself, not by a shell.
_RUN_METACHARS = ("|", "&&", ">", "<", ";")
_SUBSTITUTION_POINT = re.compile(r"\$[A-Za-z_][A-Za-z0-9_]*")


def substitute_run(cmd: str, ryo_bin: str) -> str:
    """Expand a manifest run string into a concrete command line.

    Replaces the literal ``$ryo`` substitution point with the ryo binary
    path, then expands environment variables. Run strings never pass
    through a shell, where an unset variable would expand to an empty
    string; here it is a manifest error instead.
    """
    expanded = os.path.expandvars(cmd.replace("$ryo", ryo_bin))
    leftover = _SUBSTITUTION_POINT.search(expanded)
    if leftover is not None:
        raise ManifestError(
            f"run string references unset variable {leftover.group(0)!r}: {cmd!r}"
        )
    return expanded


def load_suite(path: Path) -> Suite:
    with path.open("rb") as f:
        data = tomllib.load(f)

    name = data.get("name")
    if not isinstance(name, str) or not name:
        raise ManifestError(f"{path}: suite is missing required key 'name' (non-empty string)")

    descriptor = data.get("descriptor", "")
    if not isinstance(descriptor, str):
        raise ManifestError(f"{path}: 'descriptor' must be a string")

    warmup = data.get("warmup", 3)
    if not isinstance(warmup, int) or isinstance(warmup, bool):
        raise ManifestError(f"{path}: 'warmup' must be an integer, got {warmup!r}")

    default_expect = data.get("expect")
    if default_expect is not None and not isinstance(default_expect, str):
        raise ManifestError(f"{path}: 'expect' must be a string, got {default_expect!r}")

    raw_arms = data.get("arm", [])
    if not isinstance(raw_arms, list):
        raise ManifestError(f"{path}: 'arm' must be a list of tables")

    arms = [_load_arm(raw, i, default_expect, path) for i, raw in enumerate(raw_arms)]

    # The (group, name) pair is the store key for a history series: renaming
    # an arm intentionally starts a new series while old entries stay
    # archived under the old key, so keys must never collide within a suite.
    seen = set()
    for arm in arms:
        key = (arm.group or "", arm.name)
        if key in seen:
            raise ManifestError(
                f"{path}: duplicate arm with (group, name) {key!r}; "
                "renaming an arm starts a new history series, so keys must be unique"
            )
        seen.add(key)

    return Suite(
        name=name,
        descriptor=descriptor,
        warmup=warmup,
        default_expect=default_expect,
        arms=arms,
        dir=path.resolve().parent,
    )


def _load_arm(raw: object, index: int, default_expect: str | None, path: Path) -> Arm:
    where = f"{path}: arm #{index + 1}"
    if not isinstance(raw, dict):
        raise ManifestError(f"{where}: arm entry must be a table")

    for key in ("name", "language", "run"):
        if not isinstance(raw.get(key), str) or not raw[key]:
            raise ManifestError(f"{where}: arm is missing required key {key!r} (non-empty string)")

    name = raw["name"]
    if "/" in name:
        raise ManifestError(f"{where}: arm name must not contain '/': {name!r}")

    group = raw.get("group")
    if group is not None:
        if not isinstance(group, str):
            raise ManifestError(f"{where}: 'group' must be a string, got {group!r}")
        if "/" in group:
            raise ManifestError(f"{where}: group must not contain '/': {group!r}")

    run = raw["run"]
    scanned = _SUBSTITUTION_POINT.sub("", run)
    for metachar in _RUN_METACHARS:
        if metachar in scanned:
            raise ManifestError(
                f"{where}: run contains banned shell metacharacter "
                f"{metachar!r} (commands never pass through a shell): {run!r}"
            )

    if "expect" in raw:
        expect = raw["expect"]
        if not isinstance(expect, str):
            raise ManifestError(f"{where}: 'expect' must be a string, got {expect!r}")
    else:
        expect = default_expect

    build = raw.get("build")
    if build is not None and not isinstance(build, str):
        raise ManifestError(f"{where}: 'build' must be a string, got {build!r}")

    version = raw.get("version")
    if version is not None and not isinstance(version, str):
        raise ManifestError(f"{where}: 'version' must be a string, got {version!r}")

    return Arm(
        name=name,
        language=raw["language"],
        run=run,
        build=build,
        expect=expect,
        group=group,
        version=version,
    )


class HyperfineError(RuntimeError):
    """A hyperfine invocation failed for one benchmark arm."""

    def __init__(self, label: str, stderr: str) -> None:
        self.label = label
        self.stderr = stderr
        super().__init__(f"hyperfine failed for arm {label!r}: {stderr.strip()}")


@dataclass
class ArmTiming:
    time_ms_mean: float
    time_ms_stddev: float
    time_ms_min: float
    time_ms_max: float
    rss_mb_mean: float | None


# Hyperfine schema-2 exports always use base units (seconds, bytes). The
# unit fields in the JSON are read through these maps so a future unit
# change fails loudly at the lookup instead of silently mis-converting.
_TIME_UNIT_TO_MS = {"second": 1000.0}
_RSS_UNIT_TO_MB = {"byte": 1.0 / (1024.0 * 1024.0)}


def parse_summary(result: dict) -> ArmTiming:
    """Convert one schema-2 result's summary to display units (ms, MB)."""
    summary = result["summary"]
    wall = summary["time_wall_clock"]
    rss = summary.get("memory_peak_resident")
    to_ms = _TIME_UNIT_TO_MS[wall["unit"]]
    return ArmTiming(
        time_ms_mean=round(wall["mean"] * to_ms, 3),
        time_ms_stddev=round(wall["stddev"] * to_ms, 3),
        time_ms_min=round(wall["min"] * to_ms, 3),
        time_ms_max=round(wall["max"] * to_ms, 3),
        rss_mb_mean=None if rss is None else round(rss["mean"] * _RSS_UNIT_TO_MB[rss["unit"]], 3),
    )


def run_hyperfine(cmd: str, warmup: int, cwd: Path, label: str) -> dict:
    """Time one command with a single hyperfine invocation.

    Returns the schema-2 result dict for the command (``results[0]``).
    The export file lives in the system temp dir and is removed in a
    ``finally`` block so a failed run never litters the suite directory.
    """
    binary = shutil.which("hyperfine")
    if binary is None:
        raise RuntimeError("hyperfine executable not found on PATH; install hyperfine 2.x")

    export_file = tempfile.NamedTemporaryFile(mode="w", suffix=".json", delete=False)
    export_path = export_file.name
    export_file.close()
    try:
        proc = subprocess.run(
            [
                binary,
                "--warmup",
                str(warmup),
                "--shell=none",
                "--export-json",
                export_path,
                cmd,
            ],
            cwd=cwd,
            capture_output=True,
            text=True,
        )
        if proc.returncode != 0:
            raise HyperfineError(label, proc.stderr)
        with open(export_path, encoding="utf-8") as f:
            data = json.load(f)
    finally:
        Path(export_path).unlink(missing_ok=True)

    assert data["schema_version"] == 2, (
        f"{label}: hyperfine export has schema_version "
        f"{data.get('schema_version')!r}, expected 2"
    )
    return data["results"][0]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="benchmarks/run.py",
        description="Centralized benchmark runner driven by suite.toml manifests.",
    )
    subparsers = parser.add_subparsers(dest="command", required=True, metavar="command")
    subparsers.add_parser("run", help="Time every arm of a suite via hyperfine.")
    subparsers.add_parser("table", help="Render results tables from the data store.")
    subparsers.add_parser(
        "update-readme", help="Splice result tables into suite READMEs."
    )
    args = parser.parse_args(argv)

    if args.command == "run":
        raise SystemExit("not implemented yet")
    if args.command == "table":
        raise SystemExit("not implemented yet")
    raise SystemExit("not implemented yet")


if __name__ == "__main__":
    raise SystemExit(main())
