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
import re
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
