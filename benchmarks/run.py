#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.15"
# ///
"""Centralized benchmark runner driven by per-suite suite.toml manifests.

Replaces the copy-pasted benchmarks/*/run_benchmarks.sh scripts with one
runner: `run` times every arm via hyperfine, `table` renders results, and
`update-readme` splices them into suite READMEs and the root matrix table.
Replaces the copy-pasted per-suite run_benchmarks.sh scripts (deleted).

Subcommands: run, table, update-readme.
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import re
import shlex
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
import tomllib
from dataclasses import dataclass
from datetime import datetime, timezone
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
    category: str | None = None


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
        raise ManifestError(
            f"{path}: suite is missing required key 'name' (non-empty string)"
        )

    descriptor = data.get("descriptor", "")
    if not isinstance(descriptor, str):
        raise ManifestError(f"{path}: 'descriptor' must be a string")

    category = data.get("category")
    if category is not None and (not isinstance(category, str) or not category):
        raise ManifestError(f"{path}: 'category' must be a non-empty string")

    warmup = data.get("warmup", 3)
    if not isinstance(warmup, int) or isinstance(warmup, bool):
        raise ManifestError(f"{path}: 'warmup' must be an integer, got {warmup!r}")

    default_expect = data.get("expect")
    if default_expect is not None and not isinstance(default_expect, str):
        raise ManifestError(
            f"{path}: 'expect' must be a string, got {default_expect!r}"
        )

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
        category=category,
    )


def _load_arm(raw: object, index: int, default_expect: str | None, path: Path) -> Arm:
    where = f"{path}: arm #{index + 1}"
    if not isinstance(raw, dict):
        raise ManifestError(f"{where}: arm entry must be a table")

    for key in ("name", "language", "run"):
        if not isinstance(raw.get(key), str) or not raw[key]:
            raise ManifestError(
                f"{where}: arm is missing required key {key!r} (non-empty string)"
            )

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
        rss_mb_mean=None
        if rss is None
        else round(rss["mean"] * _RSS_UNIT_TO_MB[rss["unit"]], 3),
    )


def run_hyperfine(cmd: str, warmup: int, cwd: Path, label: str) -> dict:
    """Time one command with a single hyperfine invocation.

    Returns the schema-2 result dict for the command (``results[0]``).
    The export file lives in the system temp dir and is removed in a
    ``finally`` block so a failed run never litters the suite directory.
    """
    binary = shutil.which("hyperfine")
    if binary is None:
        raise RuntimeError(
            "hyperfine executable not found on PATH; install hyperfine 2.x"
        )

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
        '\'{for (i = 1; i < NF; i++) if ($i == "Swift" && $(i+1) == "version") '
        "{ print $(i+2); exit }}'"
    ),
    "kotlin": r"kotlinc -version 2>&1 | grep -o '[0-9]\+\.[0-9]\+\.[0-9]\+' | head -1",
    "java": "java -version 2>&1 | head -1 | awk -F '\"' '{print $2}'",
    "python": "uv run --python 3.15 python --version | awk '{print $2}'",
    "bun": "~/.bun/bin/bun --version",
    "elixir": "elixir --version | grep Elixir | awk '{print $2}'",
    "ruby": "ruby --version | awk '{print $2}'",
    "julia": "julia --version 2>/dev/null | awk '{print $3}' || echo 'not installed'",
    "ryo": r"($ryo --version 2>&1 || echo 'ryo dev') | sed -e 's/^ryo //'",
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

# Language -> the name a version string is displayed under. Where this map
# is silent, the gating binary's name is used. ``uv`` gates Python arms but
# "uv 3.14.4" mislabels a CPython version; ryo arms can override the builtin
# version one-liner with a raw ``$ryo --version``, which echoes the tool name.
_VERSION_DISPLAY_NAME: dict[str, str] = {
    "python": "python",
    "ryo": "ryo",
}


def _version_label(language: str, toolchain: str) -> str:
    """Version-cell label: display name + toolchain, de-duplicated.

    A toolchain string that already begins with the display name (raw
    ``$ryo --version`` overrides echo "ryo ...") loses exactly one leading
    duplicate, so the label never reads "ryo ryo 0.1.0-dev".
    """
    tool = TOOL_FOR_LANGUAGE.get(language.lower(), language)
    if tool == "$ryo":
        tool = "ryo"
    display = _VERSION_DISPLAY_NAME.get(language.lower(), tool)
    prefix = f"{display} "
    if toolchain.startswith(prefix):
        toolchain = toolchain[len(prefix) :]
    return f"{display} {toolchain}"


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


# Retention bound per store key: the historical store is append-only, and
# each series keeps only its newest entries so committed files stay small.
_STORE_RETENTION = 20


def _utc_now_iso() -> str:
    return (
        datetime.now(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")
    )


def _epoch_ms() -> int:
    return int(time.time() * 1000)


def _git(args: list[str]) -> str:
    """Run a read-only git query in the repo root; "" on any failure."""
    try:
        proc = subprocess.run(
            ["git", *args], cwd=_REPO_ROOT, capture_output=True, text=True
        )
    except OSError:
        return ""
    if proc.returncode != 0:
        return ""
    return proc.stdout.strip()


def _normalize_repo_url(remote: str) -> str:
    """Normalize scp-like (``git@github.com:org/repo``) and ``ssh://`` remotes
    to ``https://`` so the store's commit links resolve; https remotes (and
    anything unrecognized) pass through unchanged.
    """
    if remote.startswith("ssh://"):
        rest = remote.removeprefix("ssh://")
        if "@" in rest:
            rest = rest.split("@", 1)[1]
        return "https://" + rest.lstrip("/")
    if remote.startswith("git@") and ":" in remote:
        host, _, path = remote[len("git@") :].partition(":")
        return f"https://{host}/{path}"
    return remote


def _repo_url() -> str:
    return _normalize_repo_url(
        _git(["config", "--get", "remote.origin.url"]).removesuffix(".git")
    )


def _git_commit_id() -> str:
    return _git(["rev-parse", "--short", "HEAD"])


def _git_commit_message() -> str:
    return _git(["log", "-1", "--pretty=%s"])


def _git_commit_timestamp() -> int:
    stamp = _git(["log", "-1", "--pretty=%ct"])
    return int(stamp) if stamp.isdigit() else 0


def _platform_string() -> str:
    """Machine identity as os/arch/model, e.g. Darwin/arm64/Apple M3 Max.

    Bare-metal timings are only interpretable next to the machine they were
    captured on, so the model name is stamped via sysctl hw.model.
    """
    os_name = platform.system() or "unknown"
    arch = platform.machine() or "unknown"
    model = ""
    try:
        proc = subprocess.run(
            ["sysctl", "-n", "hw.model"], capture_output=True, text=True
        )
        if proc.returncode == 0:
            model = proc.stdout.strip()
    except OSError:
        pass
    return f"{os_name}/{arch}/{model}" if model else f"{os_name}/{arch}"


def _hyperfine_version() -> str:
    binary = shutil.which("hyperfine")
    if binary is None:
        return "unavailable"
    try:
        proc = subprocess.run([binary, "--version"], capture_output=True, text=True)
    except OSError:
        return "unavailable"
    if proc.returncode != 0:
        return "unavailable"
    return proc.stdout.strip().removeprefix("hyperfine").strip() or "unavailable"


def _write_json(path: Path, payload: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")


def store_key(suite: Suite, arm: Arm) -> str:
    """History-series key for one arm; the group segment is omitted when absent."""
    return f"{suite.name}/{arm.group + '/' if arm.group else ''}{arm.name}"


@dataclass
class ArmResult:
    """One measured arm: the manifest arm plus its parsed timing."""

    arm: Arm
    timing: ArmTiming


@dataclass
class SuiteResult:
    suite: Suite
    date: str
    git_commit: str
    platform: str
    hyperfine_version: str
    ryo: RyoInfo
    versions: dict[str, str]
    arm_results: list[ArmResult]

    def to_run_json(self, previous: Store | None) -> dict:
        """Agent-contract JSON for one suite.

        Per-arm ``previous`` (the store's latest entry for the arm's key) is
        included when the store has an earlier measurement.
        """
        arms = []
        for arm_result in self.arm_results:
            timing = arm_result.timing
            arm_json = {
                "name": arm_result.arm.name,
                "language": arm_result.arm.language,
                "time_ms": {
                    "mean": timing.time_ms_mean,
                    "stddev": timing.time_ms_stddev,
                    "min": timing.time_ms_min,
                    "max": timing.time_ms_max,
                },
                "peak_rss_mb": {"mean": timing.rss_mb_mean},
                "hyperfine": {},
            }
            if previous is not None:
                entry = previous.latest(store_key(self.suite, arm_result.arm))
                if entry is not None:
                    benches = {b["name"]: b["value"] for b in entry["benches"]}
                    arm_json["previous"] = {
                        "time_ms_mean": benches.get("time"),
                        "peak_rss_mb_mean": benches.get("memory_peak_rss"),
                    }
            arms.append(arm_json)
        return {
            "name": self.suite.name,
            "descriptor": self.suite.descriptor,
            "versions": self.versions,
            "arms": arms,
        }


@dataclass
class Store:
    """github-action-benchmark-compatible historical store for one suite.

    Shape: {"lastUpdate": <epoch ms>, "repoUrl": <git remote>, "entries":
    {key: [{commit: {id, message, timestamp, url}, date, tool: "custom",
    benches: [...]}]}}. ``append`` prunes each key's series to the newest
    ``_STORE_RETENTION`` entries; ``save`` rewrites the whole file.
    """

    path: Path
    repo_url: str
    entries: dict[str, list[dict]]
    last_update: int = 0

    @classmethod
    def load(cls, path: Path) -> Store:
        repo_url = _repo_url()
        if path.is_file():
            data = json.loads(path.read_text(encoding="utf-8"))
            return cls(
                path=path,
                repo_url=data.get("repoUrl") or repo_url,
                entries=data.get("entries") or {},
                last_update=data.get("lastUpdate") or 0,
            )
        return cls(path=path, repo_url=repo_url, entries={})

    def append(self, suite_result: SuiteResult) -> None:
        # One timestamp for the whole run: same-run comparisons (ratios,
        # version cells) match arms by exact date equality, and a per-arm
        # timestamp would never repeat across a run.
        run_date = _epoch_ms()
        for arm_result in suite_result.arm_results:
            key = store_key(suite_result.suite, arm_result.arm)
            series = self.entries.setdefault(key, [])
            series.append(self._entry(suite_result, arm_result, run_date))
            del series[:-_STORE_RETENTION]

    def latest(self, key: str) -> dict | None:
        series = self.entries.get(key)
        return series[-1] if series else None

    def previous(self, key: str) -> dict | None:
        """The entry before latest for a key, or None when fewer than 2 exist."""
        series = self.entries.get(key)
        return series[-2] if series and len(series) >= 2 else None

    def save(self) -> None:
        self.last_update = _epoch_ms()
        _write_json(
            self.path,
            {
                "lastUpdate": self.last_update,
                "repoUrl": self.repo_url,
                "entries": self.entries,
            },
        )

    def _entry(
        self, suite_result: SuiteResult, arm_result: ArmResult, run_date: int
    ) -> dict:
        commit_id = suite_result.git_commit
        language = arm_result.arm.language
        toolchain = _version_label(
            language, suite_result.versions.get(language, "unavailable")
        )
        timing = arm_result.timing
        return {
            "commit": {
                "id": commit_id,
                "message": _git_commit_message(),
                "timestamp": _git_commit_timestamp(),
                "url": f"{self.repo_url}/commit/{commit_id}" if self.repo_url else "",
            },
            "date": run_date,
            "tool": "custom",
            "benches": [
                {
                    "name": "time",
                    "value": timing.time_ms_mean,
                    "unit": "ms",
                    "extra": (
                        f"mean wall clock; hyperfine {suite_result.hyperfine_version}; "
                        f"{toolchain}"
                    ),
                },
                {
                    "name": "memory_peak_rss",
                    "value": timing.rss_mb_mean,
                    "unit": "MB",
                    "extra": "max resident set size; mean of runs",
                },
            ],
        }


# --- table rendering (the `table` subcommand) ---

_TABLE_HEADER = (
    "| Language or Arm | Version | Mean Time | Speed vs Best | Memory | Δ prev |"
)
_TABLE_SEPARATOR = "|---|---|---|---|---|---|"
_NO_MEASUREMENT = "—"


def _delta_pct(current: float | None, previous: float | None) -> str:
    """Signed one-decimal percent change at full precision; "—" when either
    side is missing or previous == 0. Negative = improvement (lower is better)."""
    if current is None or previous is None or previous == 0:
        return _NO_MEASUREMENT
    return f"{(current - previous) / previous * 100:+.1f}%"


def _format_cell_value(value: float) -> str:
    """Minimal decimal rendering: 7.0 -> "7", 2.75 -> "2.75" (no noisy .0 tail)."""
    return f"{value:g}"


def _toolchain_from_extra(extra: str | None) -> str:
    """Toolchain identity from a time bench's ``extra`` ("...; hyperfine X; rustc 1.85.0")."""
    if not extra:
        return _NO_MEASUREMENT
    return extra.split("; ")[-1].strip() or _NO_MEASUREMENT


def _bare_version(entry: dict | None) -> str:
    """Version from the time bench's ``extra`` toolchain, tool name stripped.

    "rustc 1.98.0" -> "1.98.0"; "ryo 0.1.0-dev.20261021+sha" keeps the full
    version after the tool name. "—" when the entry or its toolchain is absent.
    """
    bench = _time_bench(entry)
    if bench is None:
        return _NO_MEASUREMENT
    toolchain = _toolchain_from_extra(bench.get("extra"))
    if toolchain == _NO_MEASUREMENT:
        return toolchain
    tokens = toolchain.split(maxsplit=1)
    return tokens[1] if len(tokens) > 1 else tokens[0]


def _speed_vs_best(value: float, fastest: float | None) -> str:
    """Ratio of ``value`` to the group's fastest measured time."""
    if fastest is None:
        return _NO_MEASUREMENT
    if value == fastest:
        return "1.00x"
    return f"{value / fastest:.2f}x slower"


def _hyperfine_from_extra(extra: str | None) -> str | None:
    if not extra:
        return None
    for segment in extra.split("; "):
        if segment.startswith("hyperfine "):
            version = segment.removeprefix("hyperfine ").strip()
            return version or None
    return None


def _grouped_arms(arms: list[Arm]) -> list[tuple[str | None, list[Arm]]]:
    """Consecutive arms sharing a group key, in manifest order (None = ungrouped)."""
    groups: list[tuple[str | None, list[Arm]]] = []
    for arm in arms:
        key = arm.group or None
        if groups and groups[-1][0] == key:
            groups[-1][1].append(arm)
        else:
            groups.append((key, [arm]))
    return groups


def _table_cells(
    arm: Arm, entry: dict | None, speed: str, star_memory: bool, previous: dict | None
) -> list[str]:
    """One six-column row: arm, bare version, mean time, speed vs best, memory,
    time delta vs the previous store entry.

    ``entry`` is None for unmeasured arms, so every cell renders "—". The
    memory cell carries a leading "⭐ " when ``star_memory`` is set (the arm's
    RSS equals the group's lowest measured RSS) and appends the signed RSS
    delta vs ``previous`` when that delta is not "—". The last cell is the
    signed time delta vs ``previous`` ("—" with fewer than two store entries).
    """
    time_bench = _time_bench(entry)
    if time_bench is None:
        time_str = time_delta = _NO_MEASUREMENT
    else:
        time_str = f"{_format_cell_value(time_bench['value'])} ms"
        prev_time = _time_bench(previous)
        time_delta = _delta_pct(
            time_bench["value"], prev_time["value"] if prev_time is not None else None
        )
    rss_bench = _rss_bench(entry)
    if rss_bench is None:
        rss_str = _NO_MEASUREMENT
    else:
        rss_str = f"{_format_cell_value(rss_bench['value'])} MB"
        prev_rss = _rss_bench(previous)
        rss_delta = _delta_pct(
            rss_bench["value"], prev_rss["value"] if prev_rss is not None else None
        )
        if rss_delta != _NO_MEASUREMENT:
            rss_str = f"{rss_str} ({rss_delta})"
    if star_memory:
        rss_str = f"⭐ {rss_str}"
    return [arm.name, _bare_version(entry), time_str, speed, rss_str, time_delta]


def _caption(platform_str: str, newest: dict | None, warmup: int) -> str:
    """Generator-owned caption: machine, newest measurement's date/commit, hyperfine, warmup."""
    if newest is None:
        date = commit = hyperfine = _NO_MEASUREMENT
    else:
        date = datetime.fromtimestamp(newest["date"] / 1000, tz=timezone.utc).strftime(
            "%Y-%m-%d"
        )
        commit = newest["commit"]["id"]
        time_bench = next((b for b in newest["benches"] if b["name"] == "time"), None)
        hyperfine = (
            _hyperfine_from_extra(time_bench.get("extra") if time_bench else None)
            or "unavailable"
        )
    return (
        f"Measured on {platform_str}, {date} "
        f"(commit {commit}, hyperfine {hyperfine}, warmup {warmup})"
    )


def render_suite_table(suite: Suite, store: Store) -> str:
    """Render the canonical Markdown results tables for one suite from the store.

    One table per group of consecutive arms (ungrouped arms share a single
    table, grouped tables carry a ``#### <group>`` heading); every table is
    followed by the caption line. Within a group, measured arms sort by
    ascending mean time — fastest first, each row's Speed vs Best expressed
    against the group's fastest measured time ("1.00x" for the fastest) — and
    unmeasured arms (no store entry, or no measured time bench) render "—" in
    every cell and sort last in manifest order. The Memory cell of the arm
    whose RSS equals the group's lowest measured RSS carries a "⭐ ", and both
    the Memory cell and the trailing Δ prev cell carry the signed change vs
    the arm's previous store entry when one exists. The caption's
    date/commit/hyperfine come from the table's newest entry; the
    platform is stamped at render time (the store schema has no machine field)
    and the warmup comes from the manifest.
    """
    platform_str = _platform_string()
    sections = []
    for group, arms in _grouped_arms(suite.arms):
        lines = [f"#### {group}"] if group is not None else []
        lines.append(_TABLE_HEADER)
        lines.append(_TABLE_SEPARATOR)
        newest: dict | None = None
        measured: list[tuple[float, Arm, dict, dict | None]] = []
        unmeasured: list[tuple[Arm, dict | None, dict | None]] = []
        for arm in arms:
            key = store_key(suite, arm)
            entry = store.latest(key)
            previous = store.previous(key)
            if entry is not None and (newest is None or entry["date"] > newest["date"]):
                newest = entry
            bench = _time_bench(entry)
            if entry is None or bench is None:
                unmeasured.append((arm, entry, previous))
            else:
                measured.append((bench["value"], arm, entry, previous))
        measured.sort(key=lambda row: row[0])
        group_fastest = measured[0][0] if measured else None
        rss_values = [
            bench["value"]
            for _, _, entry, _ in measured
            if (bench := _rss_bench(entry)) is not None
        ]
        group_leanest = min(rss_values) if rss_values else None
        for time, arm, entry, previous in measured:
            rss_bench = _rss_bench(entry)
            star = (
                group_leanest is not None
                and rss_bench is not None
                and rss_bench["value"] == group_leanest
            )
            cells = _table_cells(
                arm, entry, _speed_vs_best(time, group_fastest), star, previous
            )
            lines.append("| " + " | ".join(cells) + " |")
        for arm, _, previous in unmeasured:
            cells = _table_cells(arm, None, _NO_MEASUREMENT, False, previous)
            lines.append("| " + " | ".join(cells) + " |")
        lines.append("")
        lines.append(_caption(platform_str, newest, suite.warmup))
        sections.append("\n".join(lines))
    return "\n\n".join(sections)


@dataclass
class RunSummary:
    exit_code: int
    suites: list[SuiteResult]
    failures: list[str]
    skipped: list[dict]


class SelectionError(ValueError):
    """A --benchmarks name matches no discovered suite."""


def _split_names(raw: str | None) -> list[str] | None:
    if raw is None:
        return None
    return [name.strip() for name in raw.split(",") if name.strip()]


def _validate_languages(suites: list[Suite], languages: list[str] | None) -> str | None:
    """Return an error string for --languages names no selected arm speaks.

    A typo'd language name is programmer error, not environment, so it fails
    immediately instead of silently running nothing.
    """
    if not languages:
        return None
    known = {arm.language.lower() for suite in suites for arm in suite.arms}
    unknown = [name for name in languages if name.lower() not in known]
    if unknown:
        available = ", ".join(sorted(known))
        return f"unknown --languages name(s): {', '.join(unknown)} (available: {available})"
    return None


def _is_git_ignored(path: Path) -> bool:
    """True when git considers the manifest local-only (ignored).

    ``table`` and ``update-readme`` exclude ignored manifests (the local
    csv_parse/json_parse/startup_latency sweeps) so committed artifacts only
    ever carry committed suites. When git cannot answer — missing binary, not
    a work tree — err on inclusion so behavior matches un-git-aware discovery.
    """
    try:
        proc = subprocess.run(
            ["git", "check-ignore", "-q", str(path)],
            cwd=_REPO_ROOT,
            capture_output=True,
        )
    except OSError:
        return False
    return proc.returncode == 0


def _select_suites(
    filter_raw: str | None, *, skip_ignored: bool = False
) -> list[Suite]:
    """Discover benchmarks/*/suite.toml and optionally filter by suite name.

    With ``skip_ignored`` (artifact subcommands), manifests that git marks
    ignored drop out unless the caller passes ``--include-ignored``. ``run``
    keeps the default so local sweeps stay runnable.
    """
    paths = sorted((_REPO_ROOT / "benchmarks").glob("*/suite.toml"))
    if skip_ignored:
        paths = [path for path in paths if not _is_git_ignored(path)]
    suites = [load_suite(path) for path in paths]
    wanted = _split_names(filter_raw)
    if wanted is None:
        return suites
    by_name = {suite.name: suite for suite in suites}
    unknown = [name for name in wanted if name not in by_name]
    if unknown:
        available = ", ".join(sorted(by_name))
        raise SelectionError(
            f"unknown --benchmarks name(s): {', '.join(unknown)} (available: {available})"
        )
    return [by_name[name] for name in wanted]


def _run_suite(
    suite: Suite,
    arms: list[Arm],
    ryo: RyoInfo,
    dry_run: bool,
    git_commit: str,
    platform_str: str,
    hyperfine_version: str,
) -> tuple[SuiteResult | None, str | None]:
    """Build, gate, and time each arm in manifest order.

    Arms are pipelined one at a time so a mid-suite failure — a later arm
    failing the correctness gate, or hyperfine dying on a flaky arm — leaves
    the already measured arms' results intact for the run JSON. The caller
    writes the store only when the returned failure is None.
    """
    arm_results: list[ArmResult] = []
    versions: dict[str, str] = {}
    versions_captured = False

    def snapshot() -> SuiteResult:
        return SuiteResult(
            suite=suite,
            date=_utc_now_iso(),
            git_commit=git_commit,
            platform=platform_str,
            hyperfine_version=hyperfine_version,
            ryo=ryo,
            versions=versions,
            arm_results=list(arm_results),
        )

    for arm in arms:
        try:
            build_arm(arm, suite, ryo.path)
        except BuildError as exc:
            return snapshot(), (
                f"suite '{suite.name}': build failed for arm '{arm.name}': {exc.output.strip()}"
            )
        try:
            matched, output = check_output(arm, suite, ryo.path)
        except OSError as exc:
            return snapshot(), (
                f"suite '{suite.name}': correctness gate failed for arm '{arm.name}': "
                f"cannot execute: {exc}"
            )
        if not matched:
            return snapshot(), (
                f"suite '{suite.name}': correctness gate failed for arm '{arm.name}': "
                f"expected {arm.expect!r}, got {output!r}"
            )
        if dry_run:
            continue
        if not versions_captured:
            versions = capture_versions(suite, arms, ryo.path)
            versions_captured = True
        label = f"{suite.name}/{arm.name}"
        try:
            raw = run_hyperfine(
                substitute_run(arm.run, ryo.path), suite.warmup, suite.dir, label
            )
        except HyperfineError as exc:
            return snapshot(), (
                f"suite '{suite.name}': hyperfine failed for arm '{arm.name}': "
                f"{exc.stderr.strip()}"
            )
        arm_results.append(ArmResult(arm=arm, timing=parse_summary(raw)))

    if dry_run:
        return None, None
    return snapshot(), None


def run_suites(
    suites: list[Suite],
    languages: list[str] | None = None,
    ryo: RyoInfo | None = None,
    store_dir: Path | None = None,
    json_path: Path | None = None,
    dry_run: bool = False,
    strict: bool = False,
) -> RunSummary:
    """Run the build/gate/benchmark pipeline for every suite and accumulate results.

    Arms whose toolchain is missing are skipped (warning to stderr, entry in
    the skipped list) unless strict, in which case the run fails before any
    benchmarking. The store is written only for suites with no hard failure;
    the run JSON is flushed after each suite in which at least one arm ran.
    Exit code: 0 = every selected arm ran, 2 = one or more arms skipped and
    none hard-failed, 1 = any hard failure.
    """
    language_error = _validate_languages(suites, languages)
    if language_error is not None:
        print(f"error: {language_error}", file=sys.stderr)
        return RunSummary(exit_code=1, suites=[], failures=[language_error], skipped=[])
    if ryo is None:
        ryo = resolve_ryo(None, needs_build=False)
    if store_dir is None:
        store_dir = _REPO_ROOT / "benchmarks" / "data"

    language_filter = {name.lower() for name in languages} if languages else None
    platform_str = _platform_string()
    hyperfine_version = _hyperfine_version()
    git_commit = _git_commit_id()
    generated_at = _utc_now_iso()
    platform_os, _, platform_rest = platform_str.partition("/")
    platform_arch = platform_rest.partition("/")[0]

    results: list[SuiteResult] = []
    suite_jsons: list[dict] = []
    failures: list[str] = []
    skipped: list[dict] = []

    selected_by_suite = [
        (
            suite,
            [
                arm
                for arm in suite.arms
                if language_filter is None or arm.language.lower() in language_filter
            ],
        )
        for suite in suites
    ]

    if strict:
        # Preflight every selected suite before benchmarking anything: with
        # --strict a missing toolchain anywhere fails the whole run up front.
        missing_all = [
            (suite, arm, tool)
            for suite, selected in selected_by_suite
            for arm, tool in missing_tools(suite, selected, ryo.path)
        ]
        if missing_all:
            detail = ", ".join(
                f"suite '{suite.name}' arm '{arm.name}' needs '{tool}'"
                for suite, arm, tool in missing_all
            )
            failure = f"missing toolchain(s) under --strict: {detail}"
            print(f"error: {failure}", file=sys.stderr)
            return RunSummary(exit_code=1, suites=[], failures=[failure], skipped=[])

    for suite, selected in selected_by_suite:
        missing = missing_tools(suite, selected, ryo.path)

        runnable: list[Arm] = []
        for arm in selected:
            tool = next((tool for candidate, tool in missing if candidate is arm), None)
            if tool is not None:
                print(
                    f"warning: suite '{suite.name}': skipping arm '{arm.name}' "
                    f"({arm.language}): missing toolchain '{tool}'",
                    file=sys.stderr,
                )
                skipped.append(
                    {
                        "arm": arm.name,
                        "language": arm.language,
                        "reason": f"missing toolchain: {tool}",
                    }
                )
            else:
                runnable.append(arm)
        if not runnable:
            continue

        result, failure = _run_suite(
            suite, runnable, ryo, dry_run, git_commit, platform_str, hyperfine_version
        )
        if failure is not None:
            failures.append(failure)
            print(f"error: {failure}", file=sys.stderr)
        if result is None:
            if failure is None:
                print(
                    f"[{suite.name}] dry-run OK: {len(runnable)} arm(s) built and passed "
                    "the correctness gate",
                    file=sys.stderr,
                )
            continue
        if not result.arm_results:
            # Nothing measured for this suite (e.g. the first arm failed the
            # gate); any failure is already recorded above.
            continue

        results.append(result)
        if failure is None:
            store = Store.load(store_dir / f"{suite.name}.json")
            suite_json = result.to_run_json(previous=store)
            store.append(result)
            store.save()
            print(
                f"[{suite.name}] {len(result.arm_results)} arm(s) measured; store updated",
                file=sys.stderr,
            )
        else:
            suite_json = result.to_run_json(previous=None)
        suite_jsons.append(suite_json)

        if json_path is not None:
            _write_json(
                json_path,
                {
                    "generated_at": generated_at,
                    "git_commit": git_commit,
                    "platform": {"os": platform_os, "arch": platform_arch},
                    "hyperfine_version": hyperfine_version,
                    "ryo": {
                        "path": ryo.path,
                        "version": ryo.version,
                        "built": ryo.built,
                    },
                    "status": "partial" if (failures or skipped) else "complete",
                    "skipped": skipped,
                    "suites": suite_jsons,
                },
            )

    exit_code = 1 if failures else (2 if skipped else 0)
    return RunSummary(
        exit_code=exit_code, suites=results, failures=failures, skipped=skipped
    )


def cmd_run(args: argparse.Namespace) -> int:
    """Entry point for the ``run`` subcommand: select, resolve, run, report."""
    try:
        suites = _select_suites(getattr(args, "benchmarks", None))
    except (SelectionError, ManifestError) as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1

    languages = _split_names(getattr(args, "languages", None))
    language_error = _validate_languages(suites, languages)
    if language_error is not None:
        print(f"error: {language_error}", file=sys.stderr)
        return 1

    ryo = resolve_ryo(getattr(args, "ryo_bin", None), needs_build=True)
    if ryo.built:
        print(f"ryo: built {ryo.path} ({ryo.version})", file=sys.stderr)
    else:
        print(f"ryo: {ryo.path} ({ryo.version})", file=sys.stderr)

    summary = run_suites(
        suites,
        languages=languages,
        ryo=ryo,
        json_path=Path(args.json_path) if getattr(args, "json_path", None) else None,
        dry_run=bool(getattr(args, "dry_run", False)),
        strict=bool(getattr(args, "strict", False)),
    )
    return summary.exit_code


def cmd_table(args: argparse.Namespace) -> int:
    """Entry point for the ``table`` subcommand: render canonical tables from the store."""
    try:
        suites = _select_suites(
            getattr(args, "benchmarks", None),
            skip_ignored=not getattr(args, "include_ignored", False),
        )
    except (SelectionError, ManifestError) as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1
    store_dir = _REPO_ROOT / "benchmarks" / "data"
    chunks = [
        render_suite_table(suite, Store.load(store_dir / f"{suite.name}.json"))
        for suite in suites
    ]
    text = "\n\n".join(chunks) + "\n"
    out = getattr(args, "out", None)
    if out:
        Path(out).write_text(text, encoding="utf-8")
    else:
        print(text, end="")
    return 0


def cmd_matrix(args: argparse.Namespace) -> int:
    """Entry point for the ``matrix`` subcommand: render the cross-suite matrix from the store."""
    try:
        suites = _select_suites(
            getattr(args, "benchmarks", None),
            skip_ignored=not getattr(args, "include_ignored", False),
        )
    except (SelectionError, ManifestError) as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1
    store_dir = _REPO_ROOT / "benchmarks" / "data"
    stores = {
        suite.name: Store.load(store_dir / f"{suite.name}.json") for suite in suites
    }
    text = render_matrix(suites, stores) + "\n"
    out = getattr(args, "out", None)
    if out:
        Path(out).write_text(text, encoding="utf-8")
    else:
        print(text, end="")
    return 0


# --- update-readme: splicing generated tables into READMEs ---

# Generated blocks live between these marker pairs; everything outside them is
# human-owned prose. Suite READMEs carry per-suite tables, the root
# benchmarks/README.md carries one per-language matrix across suites.
BENCH_START = "<!-- BENCH:START -->"
BENCH_END = "<!-- BENCH:END -->"
MATRIX_START = "<!-- BENCH-MATRIX:START -->"
MATRIX_END = "<!-- BENCH-MATRIX:END -->"


class MarkerError(ValueError):
    """A target README is missing its marker pair (or the file itself)."""


def splice_block(text: str, start: str, end: str, content: str) -> str:
    """Replace the block between ``start`` and ``end`` (markers kept) with ``content``.

    The markers themselves, and all text outside them, survive verbatim. Raises
    ``MarkerError`` when either marker is absent — including an ``end`` that
    only appears before ``start`` — and the caller must then leave the source
    file untouched.
    """
    start_idx = text.find(start)
    if start_idx == -1:
        raise MarkerError(f"missing start marker {start!r}")
    end_idx = text.find(end, start_idx + len(start))
    if end_idx == -1:
        raise MarkerError(f"missing end marker {end!r} after {start!r}")
    return text[: start_idx + len(start)] + "\n" + content + "\n" + text[end_idx:]


def _write_atomic(path: Path, text: str) -> None:
    """Write ``text`` to ``path`` via a same-directory temp file + rename.

    A reader never observes a half-written README, and a failed write leaves
    the original file in place (the temp file is unlinked).
    """
    fd, tmp_name = tempfile.mkstemp(
        dir=str(path.parent), prefix=f".{path.name}.", suffix=".tmp"
    )
    tmp_path = Path(tmp_name)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            f.write(text)
        os.replace(tmp_path, path)
    except BaseException:
        tmp_path.unlink(missing_ok=True)
        raise


def _read_readme(path: Path) -> str:
    if not path.is_file():
        raise MarkerError(f"{path}: README not found")
    return path.read_text(encoding="utf-8")


def _splice_readme(path: Path, text: str, start: str, end: str, content: str) -> str:
    try:
        return splice_block(text, start, end, content)
    except MarkerError as exc:
        raise MarkerError(f"{path}: {exc}") from exc


def _time_bench(entry: dict | None) -> dict | None:
    """The entry's time bench, or None when absent or never measured."""
    if entry is None:
        return None
    return next(
        (b for b in entry["benches"] if b["name"] == "time" and b["value"] is not None),
        None,
    )


def _rss_bench(entry: dict | None) -> dict | None:
    """The entry's memory_peak_rss bench, or None when absent or never measured."""
    if entry is None:
        return None
    return next(
        (
            b
            for b in entry["benches"]
            if b["name"] == "memory_peak_rss" and b["value"] is not None
        ),
        None,
    )


# Numbers carrying a benchmark unit (``12.3 ms``, ``4.5 MB``, ``1.7x``) — the
# shape prose uses when it cites a measurement inline.
_STALE_PROSE_VALUE = re.compile(r"(\d+(?:\.\d+)?)\s?(?:ms|MB|x)\b")
# A generated block, either marker pair, spanning to its end marker.
_MARKER_BLOCK = re.compile(
    r"<!-- BENCH(?:-MATRIX)?:START -->.*?<!-- BENCH(?:-MATRIX)?:END -->",
    re.DOTALL,
)
_TABLE_VALUE = re.compile(r"\d+(?:\.\d+)?")


def stale_prose_warnings(readme_text: str, table_text: str) -> list[str]:
    """Flag prose numbers that disagree with the generated table (warning only).

    Numbers with benchmark units (``12.3 ms``, ``4.5 MB``, ``1.7x``) are
    scanned in the README text outside the marker blocks; any whose numeric
    value appears in no cell of the spliced table is stale. Heuristic — it
    compares numeric tokens, never units, so false positives are tolerated.
    """
    prose = _MARKER_BLOCK.sub("", readme_text)
    table_values = set(_TABLE_VALUE.findall(table_text))
    warnings: list[str] = []
    flagged: set[str] = set()
    for match in _STALE_PROSE_VALUE.finditer(prose):
        value = match.group(1)
        if value in table_values or value in flagged:
            continue
        flagged.add(value)
        warnings.append(
            f'prose mentions "{match.group(0)}" but {value} '
            "appears in no generated-table cell"
        )
    return warnings


def cmd_update_readme(args: argparse.Namespace) -> int:
    """Entry point for the ``update-readme`` subcommand: splice tables into READMEs.

    Per suite README: the suite's canonical table into its ``BENCH`` block;
    root benchmarks/README.md: the per-language matrix into its
    ``BENCH-MATRIX`` block. ``--benchmarks`` scopes only the per-suite
    splices; the matrix is always rebuilt from every discovered suite, so a
    filtered run cannot silently delete other suites' committed rows. All
    splices are planned before anything is written, so a missing marker pair
    is a hard error with zero file mutation.
    Writes are in place and atomic (temp file + rename), and a write is
    skipped entirely when the content is unchanged. Stale-prose warnings go
    to stderr and never affect the exit code.
    """
    try:
        suites = _select_suites(
            getattr(args, "benchmarks", None),
            skip_ignored=not getattr(args, "include_ignored", False),
        )
        # The root matrix must reflect every discovered suite: building it
        # from the --benchmarks selection would rewrite the committed table
        # with only the selected rows, silently deleting the rest.
        all_suites = _select_suites(
            None,
            skip_ignored=not getattr(args, "include_ignored", False),
        )
    except (SelectionError, ManifestError) as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1
    store_dir = _REPO_ROOT / "benchmarks" / "data"
    stores = {
        suite.name: Store.load(store_dir / f"{suite.name}.json") for suite in all_suites
    }

    try:
        plans: list[tuple[Path, str, str, str]] = []
        for suite in suites:
            if not stores[suite.name].entries:
                # Nothing measured yet: leave the README's block untouched so
                # bare runs stay byte-identical on a fresh clone.
                continue
            readme_path = suite.dir / "README.md"
            text = _read_readme(readme_path)
            table = render_suite_table(suite, stores[suite.name])
            plans.append(
                (
                    readme_path,
                    text,
                    _splice_readme(readme_path, text, BENCH_START, BENCH_END, table),
                    table,
                )
            )
        root_path = _REPO_ROOT / "benchmarks" / "README.md"
        root_text = _read_readme(root_path)
        # The matrix block follows the all-discovered-suites rule above.
        matrix = render_matrix(all_suites, stores)
        plans.append(
            (
                root_path,
                root_text,
                _splice_readme(root_path, root_text, MATRIX_START, MATRIX_END, matrix),
                matrix,
            )
        )
    except MarkerError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1

    for path, old_text, new_text, table in plans:
        if new_text != old_text:
            _write_atomic(path, new_text)
        for warning in stale_prose_warnings(old_text, table):
            print(f"warning: {path}: {warning}", file=sys.stderr)
    return 0


# --- matrix rendering ---

_MATRIX_COLUMN_ORDER: list[str] = [
    "ryo-aot",
    "ryo-jit",
    "rust",
    "swift",
    "go",
    "python",
    "kotlin",
    "bun",
    "elixir",
    "ruby",
    "julia",
]
_MATRIX_COLUMN_LABELS: dict[str, str] = {
    "ryo-aot": "Ryo AOT",
    "ryo-jit": "Ryo JIT",
    "bun": "Bun (TS)",
}


def _matrix_column_key(arm: Arm) -> str:
    """Stable matrix column for an arm: the ryo arms split by JIT vs AOT
    (anything not named JIT is the AOT default), other arms use their language."""
    if arm.language.lower() == "ryo":
        return "ryo-jit" if "jit" in arm.name.lower() else "ryo-aot"
    return arm.language.lower()


def _ratio_emoji(ratio: float) -> str:
    """Speed-vs-best bucket: ⭐ fastest, 🟩 within 20%, 🟨 within 3x, 🟥 beyond."""
    if ratio <= 1.0 + 1e-9:
        return "⭐"
    if ratio <= 1.2:
        return "🟩"
    if ratio <= 3.0:
        return "🟨"
    return "🟥"


def _format_ratio(ratio: float) -> str:
    """Compact ratio suffix: two decimals under 10x, one under 100x, whole above."""
    if ratio < 10:
        return f"{ratio:.2f}×"
    if ratio < 100:
        return f"{ratio:.1f}×"
    return f"{ratio:.0f}×"


_MATRIX_LEGEND = "⭐ fastest in row · 🟩 within 20% · 🟨 within 3× · 🟥 beyond 3×"
_MATRIX_NO_DATA = "*(no measurements yet)*"
_MATRIX_FOOTNOTE = (
    "*Cells show each suite's best arm per language — mean time or peak RSS, ratio vs. "
    "the fastest arm in the row; the summary row is a per-column geomean of those ratios.*"
)
# (metric key, table caption, bench extractor); the extractors are the same
# store-inspection helpers the per-suite tables and matrix use.
_MATRIX_METRICS = (
    ("time", "**Runtime (mean)**", _time_bench),
    ("memory", "**Peak memory (max RSS)**", _rss_bench),
)


def _matrix_row(suite: Suite, store: Store) -> dict[str, dict[str, tuple[float, dict]]]:
    """Per-metric best cells for one suite: column -> (value, winning entry).

    Several arms can map to one column (e.g. two Rust arms); the lowest value
    wins — fastest time, leanest RSS — because the matrix shows the best a
    language achieves per suite. Entries whose bench is missing (never
    measured, or RSS not recorded by hyperfine) contribute nothing.
    """
    metrics: dict[str, dict[str, tuple[float, dict]]] = {
        metric: {} for metric, _, _ in _MATRIX_METRICS
    }
    for arm in suite.arms:
        entry = store.latest(store_key(suite, arm))
        column = _matrix_column_key(arm)
        for metric, _, extract in _MATRIX_METRICS:
            bench = extract(entry)
            if entry is None or bench is None:
                continue
            cell = metrics[metric].get(column)
            if cell is None or bench["value"] < cell[0]:
                metrics[metric][column] = (bench["value"], entry)
    return metrics


def _matrix_columns(rows: list[dict[str, tuple[float, dict]]]) -> list[str]:
    """Column order: known languages (_MATRIX_COLUMN_ORDER) first, the rest sorted."""
    present = {column for cells in rows for column in cells}
    return [c for c in _MATRIX_COLUMN_ORDER if c in present] + sorted(
        present - set(_MATRIX_COLUMN_ORDER)
    )


def _matrix_grouped(
    rows: list[tuple[Suite, dict[str, tuple[float, dict]]]],
) -> list[tuple[str, list[tuple[Suite, dict[str, tuple[float, dict]]]]]]:
    """Rows by category: alphabetical groups, uncategorized suites last as "Other"."""
    grouped: dict[str, list[tuple[Suite, dict[str, tuple[float, dict]]]]] = {}
    for suite, cells in sorted(rows, key=lambda row: row[0].name):
        grouped.setdefault(suite.category or "Other", []).append((suite, cells))
    return [
        (category, grouped[category])
        for category in sorted(grouped, key=lambda c: (c == "Other", c))
    ]


def _matrix_summary_row(
    metric_rows: list[tuple[Suite, dict[str, tuple[float, dict]]]],
    columns: list[str],
) -> list[str]:
    """Geomean of the cell ratios per column, badged against the best geomean."""
    geomeans: list[float | None] = []
    for column in columns:
        ratios = [
            cells[column][0] / min(value for value, _ in cells.values())
            for _, cells in metric_rows
            if column in cells
        ]
        geomeans.append(statistics.geometric_mean(ratios) if ratios else None)
    best = min(geo for geo in geomeans if geo is not None)
    return ["**Summary (geomean)**"] + [
        _NO_MEASUREMENT
        if geo is None
        else f"{_ratio_emoji(geo / best)} {_format_ratio(geo)}"
        for geo in geomeans
    ]


def _matrix_table(
    metric_rows: list[tuple[Suite, dict[str, tuple[float, dict]]]],
    columns: list[str],
) -> str:
    """One metric's Markdown table: header, category group rows, geomean summary."""
    header = (
        "| Suite | "
        + " | ".join(
            _MATRIX_COLUMN_LABELS.get(column, column.capitalize()) for column in columns
        )
        + " |"
    )
    lines = [header, "|" + "---|" * (len(columns) + 1)]
    for category, rows in _matrix_grouped(metric_rows):
        lines.append(
            "| " + " | ".join([f"**{category}**"] + [""] * len(columns)) + " |"
        )
        for suite, cells in rows:
            best = min(value for value, _ in cells.values())
            newest = max(entry["date"] for _, entry in cells.values())
            date = datetime.fromtimestamp(newest / 1000, tz=timezone.utc).strftime(
                "%Y-%m-%d"
            )
            row = [f"[{suite.name}](./{suite.dir.name}/)<br>{date}"]
            for column in columns:
                cell = cells.get(column)
                if cell is None:
                    row.append(_NO_MEASUREMENT)
                    continue
                value, _ = cell
                ratio = value / best
                row.append(
                    f"{_ratio_emoji(ratio)} {_format_cell_value(value)} "
                    f"[{_format_ratio(ratio)}]"
                )
            lines.append("| " + " | ".join(row) + " |")
    lines.append("| " + " | ".join(_matrix_summary_row(metric_rows, columns)) + " |")
    return "\n".join(lines)


def render_matrix(suites: list[Suite], stores: dict[str, Store]) -> str:
    """Render the cross-suite language matrix: legend, one table per metric, footnote.

    Rows are suites grouped by category (alphabetical groups, uncategorized
    suites last under "Other", alphabetical within a group); columns are
    languages — canonical order first, unknown languages appended sorted —
    and a column exists only while at least one suite has a measured cell in
    it. Each cell is the suite's best arm for that language: value and ratio
    vs. the row's fastest cell. Toolchain versions stay in the per-suite
    tables. The final row is a per-column geomean of the cell ratios over
    measured rows. Suites with
    nothing measured for a metric are omitted from that metric's table (a
    metric no suite measured gets no table at all); when nothing is measured
    anywhere, the legend and footnote frame the no-data placeholder.
    """
    per_metric: dict[str, list[tuple[Suite, dict[str, tuple[float, dict]]]]] = {
        metric: [] for metric, _, _ in _MATRIX_METRICS
    }
    for suite in sorted(suites, key=lambda s: s.name):
        cells = _matrix_row(suite, stores[suite.name])
        for metric, _, _ in _MATRIX_METRICS:
            if cells[metric]:
                per_metric[metric].append((suite, cells[metric]))

    if not any(per_metric.values()):
        return "\n".join([_MATRIX_LEGEND, "", _MATRIX_NO_DATA, "", _MATRIX_FOOTNOTE])

    sections = [_MATRIX_LEGEND]
    for metric, caption, _ in _MATRIX_METRICS:
        rows = per_metric[metric]
        if not rows:
            continue
        table = _matrix_table(rows, _matrix_columns([cells for _, cells in rows]))
        sections += [caption, table, ""]
    sections.append(_MATRIX_FOOTNOTE)
    return "\n".join(sections)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="benchmarks/run.py",
        description="Centralized benchmark runner driven by suite.toml manifests.",
    )
    subparsers = parser.add_subparsers(dest="command", required=True, metavar="command")
    run_parser = subparsers.add_parser(
        "run", help="Time every arm of a suite via hyperfine."
    )
    run_parser.add_argument(
        "--benchmarks",
        metavar="NAMES",
        help="Comma-separated suite names to run (default: every discovered suite).",
    )
    run_parser.add_argument(
        "--languages",
        metavar="NAMES",
        help="Comma-separated arm languages to run (default: every arm).",
    )
    run_parser.add_argument(
        "--json",
        dest="json_path",
        metavar="PATH",
        help="Write the incremental run JSON to PATH after each completed suite.",
    )
    run_parser.add_argument(
        "--ryo-bin",
        metavar="PATH",
        help="Path to the ryo binary (default: $RYO_BIN or target/release/ryo).",
    )
    run_parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Build and run the correctness gate only; never invoke hyperfine.",
    )
    run_parser.add_argument(
        "--strict",
        action="store_true",
        help="Fail before benchmarking when a selected toolchain is missing.",
    )
    table_parser = subparsers.add_parser(
        "table", help="Render results tables from the data store."
    )
    table_parser.add_argument(
        "--benchmarks",
        metavar="NAMES",
        help="Comma-separated suite names to render (default: every discovered suite).",
    )
    table_parser.add_argument(
        "--out",
        metavar="PATH",
        help="Write the rendered tables to PATH instead of printing to stdout.",
    )
    table_parser.add_argument(
        "--include-ignored",
        action="store_true",
        help="Also render git-ignored (local) suites such as csv_parse.",
    )
    matrix_parser = subparsers.add_parser(
        "matrix", help="Render the cross-suite matrix from the data store."
    )
    matrix_parser.add_argument(
        "--benchmarks",
        metavar="NAMES",
        help="Comma-separated suite names to render (default: every discovered suite).",
    )
    matrix_parser.add_argument(
        "--out",
        metavar="PATH",
        help="Write the rendered matrix to PATH instead of printing to stdout.",
    )
    matrix_parser.add_argument(
        "--include-ignored",
        action="store_true",
        help="Also render git-ignored (local) suites such as csv_parse.",
    )
    update_parser = subparsers.add_parser(
        "update-readme", help="Splice result tables into suite READMEs."
    )
    update_parser.add_argument(
        "--benchmarks",
        metavar="NAMES",
        help="Comma-separated suite names to update (default: every discovered suite).",
    )
    update_parser.add_argument(
        "--include-ignored",
        action="store_true",
        help="Also splice git-ignored (local) suites such as csv_parse.",
    )
    args = parser.parse_args(argv)

    if args.command == "run":
        return cmd_run(args)
    if args.command == "table":
        return cmd_table(args)
    if args.command == "matrix":
        return cmd_matrix(args)
    if args.command == "update-readme":
        return cmd_update_readme(args)
    raise SystemExit("not implemented yet")


if __name__ == "__main__":
    raise SystemExit(main())
