#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.9"
# ///
#
# NOTE: Rewrite this script in Ryo once the language supports everything it
# needs: reading files from disk, regular expressions (or equivalent string
# scanning), CLI argument parsing, and process exit codes.
"""File, inspect, and delete issue entries in ISSUES.md.

Usage:
    uv run scripts/issue.py I-032          # full text of issue I-032
    uv run scripts/issue.py 32             # same (bare numbers ok)
    uv run scripts/issue.py next           # next issue id (highest ever + 1)
    uv run scripts/issue.py list           # all ids, line ranges, and titles
    uv run scripts/issue.py file ...       # append a new entry (see --help)
    uv run scripts/issue.py delete I-032   # remove an entry (asks to confirm)

Subcommands: next, list, file, delete. Anything else in the first position is
treated as an issue id to print.

`file` takes --title/--severity/--area/--files/--summary/--resolution; any field
not given on the command line is prompted for interactively (so agents should
pass everything as flags; plain `file` with no flags walks a human through it).
`list --area <area>` filters the listing by area.
"""

import argparse
import re
import subprocess
import sys
from pathlib import Path

ENTRY_RE = re.compile(r"^###\s+(I-(\d+))\s+—\s+(.*)$")
BOUNDARY_RE = re.compile(r"^(#{1,3}\s|---\s*$)")
FIELD_RE = r"^\*\*{}\:\*\*\s*(.+?)\s*$"
SEVERITY_RE = re.compile(FIELD_RE.format("Severity"))
AREA_RE = re.compile(FIELD_RE.format("Area"))

SEVERITIES = ("Blocking", "Correctness / Hygiene", "Cleanup")
SEVERITY_ALIASES = {
    "blocking": "Blocking",
    "correctness": "Correctness / Hygiene",
    "hygiene": "Correctness / Hygiene",
    "correctness/hygiene": "Correctness / Hygiene",
    "correctness / hygiene": "Correctness / Hygiene",
    "cleanup": "Cleanup",
}

AREAS = (
    "frontend-lexer", "frontend-parser", "sema", "ownership", "codegen",
    "runtime", "linker-toolchain", "driver-cli", "core-ir", "docs-spec",
    "ci-benchmarks", "tooling",
)
AREA_ALIASES = {
    "lexer": "frontend-lexer",
    "frontend-lexer": "frontend-lexer",
    "parser": "frontend-parser",
    "frontend-parser": "frontend-parser",
    "ast": "frontend-parser",
    "sema": "sema",
    "semantic": "sema",
    "builtins": "sema",
    "ownership": "ownership",
    "borrowck": "ownership",
    "codegen": "codegen",
    "backend": "codegen",
    "runtime": "runtime",
    "linker": "linker-toolchain",
    "toolchain": "linker-toolchain",
    "linker-toolchain": "linker-toolchain",
    "driver": "driver-cli",
    "cli": "driver-cli",
    "driver-cli": "driver-cli",
    "core": "core-ir",
    "uir": "core-ir",
    "tir": "core-ir",
    "core-ir": "core-ir",
    "diag": "core-ir",
    "docs": "docs-spec",
    "spec": "docs-spec",
    "docs-spec": "docs-spec",
    "ci": "ci-benchmarks",
    "benchmarks": "ci-benchmarks",
    "ci-benchmarks": "ci-benchmarks",
    "codspeed": "ci-benchmarks",
    "tooling": "tooling",
    "scripts": "tooling",
}

FIELDS = ("title", "severity", "area", "files", "summary", "resolution")


def parse_entries(text):
    """Yield (issue_id, title, start_line, end_line, body, severity, area).

    start_line/end_line are 1-based and inclusive; body is the entry's lines
    including the heading; severity/area come from the entry's **Severity:** /
    **Area:** fields (None if an entry predates the field).
    """
    lines = text.splitlines()
    entries = []
    current = None  # [id, title, start_line_index]

    def close(end):
        # end is the exclusive 0-based stop; trim trailing blank lines
        while end > current[2] + 1 and not lines[end - 1].strip():
            end -= 1
        body = lines[current[2]:end]
        severity = area = None
        for line in body:
            m = SEVERITY_RE.match(line)
            if m:
                severity = m.group(1)
                continue
            m = AREA_RE.match(line)
            if m:
                area = m.group(1)
        entries.append((current[0], current[1], current[2] + 1, end, body, severity, area))

    for i, line in enumerate(lines):
        m = ENTRY_RE.match(line)
        if m:
            if current:
                close(i)
            current = (m.group(1), m.group(3).strip(), i)
        elif current and BOUNDARY_RE.match(line):
            close(i)
            current = None
    if current:
        close(len(lines))
    return entries


def normalize_id(raw):
    m = re.fullmatch(r"(?:I-)?(\d+)", raw.strip(), re.IGNORECASE)
    if not m:
        return None
    return f"I-{int(m.group(1)):03d}"


def max_id_ever(entries, issues_file):
    """Highest issue number in the live file AND its git history.

    Resolved entries are deleted from ISSUES.md but their ids stay retired, so
    the file alone can under-report. Scan added/removed entry headings in the
    file's history; fall back to the live file (with a warning) if git fails.
    """
    highest = max((int(e[0][2:]) for e in entries), default=0)
    try:
        log = subprocess.run(
            ["git", "log", "-p", "--format=", "--", str(issues_file)],
            capture_output=True, text=True, check=True,
        ).stdout
    except (OSError, subprocess.CalledProcessError) as exc:
        print(f"warning: cannot read git history ({exc}); using live file only", file=sys.stderr)
        return highest
    for m in re.finditer(r"^[+-]###\s+I-(\d+)", log, re.MULTILINE):
        highest = max(highest, int(m.group(1)))
    return highest


def cmd_next(entries, issues_file, _args):
    print(f"I-{max_id_ever(entries, issues_file) + 1:03d}")


def cmd_list(entries, issues_file, args):
    area_filter = AREA_ALIASES.get(args.area.lower()) if args.area else None
    if args.area and area_filter is None:
        sys.exit(f"error: invalid area {args.area!r} (expected one of: {', '.join(AREAS)})")

    severity_counts = {}
    area_counts = {}
    shown = 0
    for issue_id, title, start, end, _, severity, area in entries:
        if area_filter and area != area_filter:
            continue
        print(f"{issue_id} (lines {start}-{end}) — {title}")
        shown += 1
        if severity:
            severity_counts[severity] = severity_counts.get(severity, 0) + 1
        if area:
            area_counts[area] = area_counts.get(area, 0) + 1

    print(f"\nTotal: {shown} issue(s)")
    for sev in SEVERITIES:
        if sev in severity_counts:
            print(f"  {sev}: {severity_counts[sev]}")
    for a in AREAS:
        if a in area_counts:
            print(f"  {a}: {area_counts[a]}")
    unclassified = shown - sum(area_counts.values())
    if unclassified:
        print(f"  (no area field): {unclassified}")


def cmd_show(entries, issues_file, args):
    issue_id = normalize_id(args.command)
    if issue_id is None:
        sys.exit(f"error: invalid issue id: {args.command!r}")

    for eid, _title, start, end, body, _severity, _area in entries:
        if eid == issue_id:
            print(f"{issues_file}:{start}-{end}")
            print("\n".join(body).strip())
            return
    sys.exit(f"error: {issue_id} not found in {issues_file}")


def prompt_field(name):
    if not sys.stdin.isatty():
        sys.exit(f"error: --{name} is required (non-interactive stdin; pass it as a flag)")
    labels = {
        "severity": "severity (Blocking / Correctness / Hygiene / Cleanup)",
        "area": f"area ({' / '.join(AREAS)})",
    }
    value = input(f"{labels.get(name, name)}: ").strip()
    if not value:
        sys.exit(f"error: {name} cannot be empty")
    return value


def cmd_file(entries, issues_file, args):
    values = {}
    for field in FIELDS:
        value = getattr(args, field)
        if value is None:
            value = prompt_field(field)
        if field == "severity":
            canonical = SEVERITY_ALIASES.get(value.lower())
            if canonical is None:
                sys.exit(f"error: invalid severity {value!r} (expected one of: {', '.join(SEVERITIES)})")
            value = canonical
        elif field == "area":
            canonical = AREA_ALIASES.get(value.lower())
            if canonical is None:
                sys.exit(f"error: invalid area {value!r} (expected one of: {', '.join(AREAS)})")
            value = canonical
        values[field] = value

    issue_id = f"I-{max_id_ever(entries, issues_file) + 1:03d}"
    entry = (
        f"### {issue_id} — {values['title']}\n"
        f"\n"
        f"**Severity:** {values['severity']}\n"
        f"**Area:** {values['area']}\n"
        f"\n"
        f"**Files:** {values['files']}\n"
        f"\n"
        f"**Summary:** {values['summary']}\n"
        f"\n"
        f"**Resolution:** {values['resolution']}\n"
    )

    text = issues_file.read_text(encoding="utf-8")
    anchor = "## Cross-References"
    idx = text.find(anchor)
    if idx != -1:
        # insert before the trailing cross-references section, keeping its
        # preceding "---" separator as the new entry's own
        head, tail = text[:idx], text[idx:]
        head = head.rstrip("\n")
        if not head.endswith("---"):
            head += "\n\n---"
        text = head + "\n\n" + entry + "\n---\n\n" + tail
    else:
        text = text.rstrip("\n") + "\n\n---\n\n" + entry

    issues_file.write_text(text, encoding="utf-8")
    line_no = text[:text.find(f"### {issue_id}")].count("\n") + 1
    print(f"filed {issue_id} at {issues_file}:{line_no}")


def cmd_delete(entries, issues_file, args):
    if args.target is None:
        sys.exit("error: delete needs an issue id, e.g. delete I-032")
    issue_id = normalize_id(args.target)
    if issue_id is None:
        sys.exit(f"error: invalid issue id: {args.target!r}")

    match = next((e for e in entries if e[0] == issue_id), None)
    if match is None:
        sys.exit(f"error: {issue_id} not found in {issues_file}")
    _eid, title, start, end, _body, _severity, _area = match

    if not args.yes:
        if not sys.stdin.isatty():
            sys.exit("error: refusing to delete without confirmation (pass --yes)")
        answer = input(f"delete {issue_id} — {title}? [y/N] ")
        if answer.strip().lower() not in ("y", "yes"):
            print("aborted")
            return

    lines = issues_file.read_text(encoding="utf-8").splitlines()
    # entry occupies lines[start-1:end]; also swallow its trailing separator
    # and blank lines, or its leading separator if it is the last entry
    del_end = end
    while del_end < len(lines) and not lines[del_end].strip():
        del_end += 1
    if del_end < len(lines) and lines[del_end].strip() == "---":
        del_end += 1
        while del_end < len(lines) and not lines[del_end].strip():
            del_end += 1
    else:
        del_start = start - 1
        while del_start > 0 and not lines[del_start - 1].strip():
            del_start -= 1
        if del_start > 0 and lines[del_start - 1].strip() == "---":
            del_start -= 1
        start = del_start + 1

    remaining = lines[:start - 1] + lines[del_end:]
    # collapse any separator gap left at the seam
    seam = "\n".join(remaining)
    seam = re.sub(r"---\n(?:\n---\n)+", "---\n", seam)
    issues_file.write_text(seam.rstrip("\n") + "\n", encoding="utf-8")
    print(f"deleted {issue_id} (id stays retired; do not reuse it)")


COMMANDS = {"next": cmd_next, "list": cmd_list, "file": cmd_file, "delete": cmd_delete}


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("command", nargs="?", help="issue id (I-032 or 32) or subcommand: next, list, file, delete")
    ap.add_argument("target", nargs="?", help="issue id, for: delete")
    ap.add_argument("--title", help="file: entry title")
    ap.add_argument("--severity", help="file: Blocking / Correctness / Hygiene / Cleanup")
    ap.add_argument("--area", help="file: compiler area (see --help for the list); also list: filter by area")
    ap.add_argument("--files", help="file: affected files field")
    ap.add_argument("--summary", help="file: summary field")
    ap.add_argument("--resolution", help="file: resolution field")
    ap.add_argument("--yes", action="store_true", help="delete: skip the confirmation prompt")
    ap.add_argument("--file", default="ISSUES.md", type=Path, help="Path to ISSUES.md (default: ./ISSUES.md)")
    args = ap.parse_args()

    if not args.file.is_file():
        sys.exit(f"error: {args.file} not found")

    entries = parse_entries(args.file.read_text(encoding="utf-8"))
    if not entries and args.command != "file":
        sys.exit(f"error: no issue entries found in {args.file}")

    handler = COMMANDS.get(args.command) if args.command else None
    if handler is not None:
        handler(entries, args.file, args)
    elif args.command:
        cmd_show(entries, args.file, args)
    else:
        ap.error("give an issue id or a subcommand: next, list, file, delete")


if __name__ == "__main__":
    main()
