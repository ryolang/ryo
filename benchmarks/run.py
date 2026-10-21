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
import shlex
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


# Version one-liners transcribed from the legacy benchmarks/*/run_benchmarks.sh
# scripts. Unlike run strings they run through a shell; failures read back as
# "unavailable" (or "dev"/"not installed" where the one-liner plans for an
# absent tool).
BUILTIN_VERSION_CMDS: dict[str, str] = {
    "rust": "rustc --version | cut -d' ' -f2",
    "go": "go version | awk '{print $3}' | sed 's/go//'",
    "swift": (
        "swiftc --version | head -1 | awk "
        "'{for (i = 1; i < NF; i++) if ($i == \"Swift\" && $(i+1) == \"version\") "
        "{ print $(i+2); exit }}'"
    ),
    "kotlin": r"kotlinc -version 2>&1 | grep -o '[0-9]\+\.[0-9]\+\.[0-9]\+' | head -1",
    "java": "java -version 2>&1 | head -1 | awk -F '\"' '{print $2}'",
    "python": "uv run --python 3.14 python --version | awk '{print $2}'",
    "bun": "~/.bun/bin/bun --version",
    "elixir": "elixir --version | grep Elixir | awk '{print $2}'",
    "ruby": "ruby --version | awk '{print $2}'",
    "julia": "julia --version 2>/dev/null | awk '{print $3}' || echo 'not installed'",
    "ryo": r"$ryo --version 2>&1 | grep -o '[0-9]\+\.[0-9]\+\.[0-9]\+' || echo 'dev'",
}

# Language -> the binary whose presence gates the arm. The "$ryo" marker is
# resolved against the ryo_bin path handed in by the caller; bun also accepts
# the ~/.bun/bin install location the version one-liner assumes.
TOOL_FOR_LANGUAGE: dict[str, str] = {
    "rust": "rustc",
    "go": "go",
    "swift": "swiftc",
    "kotlin": "kotlinc",
    "java": "java",
    "python": "uv",
    "bun": "bun",
    "elixir": "elixir",
    "ruby": "ruby",
    "julia": "julia",
    "ryo": "$ryo",
}


def missing_tools(suite: Suite, arms: list[Arm], ryo_bin: str) -> list[tuple[Arm, str]]:
    """Return (arm, missing binary) for selected arms whose toolchain is absent.

    The missing-toolchain policy belongs to the caller: the default run flow
    warns and skips loudly, --strict turns the list into a hard error.
    """
    missing = []
    for arm in arms:
        tool = TOOL_FOR_LANGUAGE.get(arm.language.lower())
        if tool is None:
            continue
        if tool == "$ryo":
            if not Path(ryo_bin).is_file() and shutil.which(ryo_bin) is None:
                missing.append((arm, ryo_bin))
        elif tool == "bun":
            fallback = Path.home() / ".bun" / "bin" / "bun"
            if shutil.which("bun") is None and not fallback.is_file():
                missing.append((arm, "bun"))
        elif shutil.which(tool) is None:
            missing.append((arm, tool))
    return missing


def capture_versions(suite: Suite, arms: list[Arm], ryo_bin: str) -> dict[str, str]:
    """Capture one version string per language across the selected arms.

    An arm-level ``version`` override wins over the builtin one-liner for its
    language, and the first arm of a language defines the entry. One-liners
    run through a shell in the suite directory; any failure reads back as
    "unavailable" instead of aborting the run.
    """
    versions: dict[str, str] = {}
    for arm in arms:
        if arm.language in versions:
            continue
        cmd = arm.version or BUILTIN_VERSION_CMDS.get(arm.language.lower())
        if cmd is None:
            continue
        cmd = cmd.replace("$ryo", ryo_bin)
        try:
            proc = subprocess.run(
                cmd, shell=True, cwd=suite.dir, capture_output=True, text=True
            )
        except OSError:
            versions[arm.language] = "unavailable"
            continue
        versions[arm.language] = (
            proc.stdout.strip() if proc.returncode == 0 else "unavailable"
        )
    return versions


class BuildError(RuntimeError):
    """An arm's build command failed; carries the combined build output."""

    def __init__(self, arm_name: str, output: str) -> None:
        self.arm_name = arm_name
        self.output = output
        super().__init__(f"build failed for arm {arm_name!r}: {output.strip()}")


_BUILD_TIMEOUT_SECONDS = 600


def build_arm(arm: Arm, suite: Suite, ryo_bin: str) -> None:
    """Run an arm's build command in the suite directory.

    Build strings pass through a shell (unlike run strings) and may use
    redirects, so only substitution points are expanded — the run-string
    metacharacter ban does not apply. A non-zero exit or timeout raises
    BuildError carrying the combined output; the legacy scripts sent it to
    /dev/null, the runner surfaces it.
    """
    if arm.build is None:
        return
    cmd = os.path.expandvars(arm.build.replace("$ryo", ryo_bin))
    try:
        proc = subprocess.run(
            cmd,
            shell=True,
            cwd=suite.dir,
            capture_output=True,
            text=True,
            timeout=_BUILD_TIMEOUT_SECONDS,
        )
    except subprocess.TimeoutExpired as exc:
        partial = ""
        if isinstance(exc.stdout, str):
            partial += exc.stdout
        if isinstance(exc.stderr, str):
            partial += exc.stderr
        raise BuildError(
            arm.name, f"timed out after {_BUILD_TIMEOUT_SECONDS}s\n{partial}"
        ) from exc
    if proc.returncode != 0:
        raise BuildError(arm.name, proc.stdout + proc.stderr)


def check_output(arm: Arm, suite: Suite, ryo_bin: str) -> tuple[bool, str]:
    """Run an arm once, directly (no shell), and check its output.

    With a non-empty expected string, success is an exact match against the
    stripped stdout. An empty expected string means exit-code-only: success is
    a zero return code and the output reads back as "".
    """
    argv = shlex.split(substitute_run(arm.run, ryo_bin))
    proc = subprocess.run(argv, cwd=suite.dir, capture_output=True, text=True)
    expect = arm.expect if arm.expect is not None else ""
    if expect == "":
        return proc.returncode == 0, ""
    stdout = proc.stdout.strip()
    return stdout == expect, stdout


_REPO_ROOT = Path(__file__).resolve().parents[1]
_DEFAULT_RYO_BIN = (_REPO_ROOT / "target" / "release" / "ryo").resolve()


@dataclass
class RyoInfo:
    path: str
    version: str
    built: bool


def resolve_ryo(explicit: str | None, needs_build: bool) -> RyoInfo:
    """Resolve the ryo binary, building the repo default on demand.

    Precedence: --ryo-bin > $RYO_BIN > the repo's target/release/ryo. Only
    the default repo binary is ever built here: when it wins and needs_build
    is set, ``cargo build --release`` runs with the working directory pinned
    to the repo root so the build lands in the expected target directory no
    matter where the runner was invoked from.
    """
    chosen = explicit or os.environ.get("RYO_BIN") or str(_DEFAULT_RYO_BIN)
    path = str(Path(chosen).resolve())
    built = False
    if needs_build and Path(path) == _DEFAULT_RYO_BIN:
        proc = subprocess.run(
            ["cargo", "build", "--release"],
            cwd=_REPO_ROOT,
            capture_output=True,
            text=True,
        )
        built = proc.returncode == 0
    return RyoInfo(path=path, version=_ryo_version(path), built=built)


def _ryo_version(path: str) -> str:
    """Best-effort ``ryo --version`` probe; "unavailable" on any failure."""
    try:
        proc = subprocess.run([path, "--version"], capture_output=True, text=True)
    except OSError:
        return "unavailable"
    if proc.returncode != 0:
        return "unavailable"
    return proc.stdout.strip() or "unavailable"


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
