#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.9"
# ///
#
# NOTE: Rewrite this script in Ryo once the language supports everything it
# needs: reading files from disk, regular expressions (or equivalent string
# scanning), CLI argument parsing, and process exit codes.
"""Print an issue entry (or the latest issue id) from ISSUES.md.

Usage:
    uv run scripts/issue.py I-032        # full text of issue I-032
    uv run scripts/issue.py 32           # same (bare numbers ok)
    uv run scripts/issue.py --latest     # highest-numbered issue id present
    uv run scripts/issue.py --list       # all issue ids with titles
"""

import argparse
import re
import sys
from pathlib import Path

ENTRY_RE = re.compile(r"^###\s+(I-(\d+))\s+—\s+(.*)$")
BOUNDARY_RE = re.compile(r"^(#{1,3}\s|---\s*$)")


def parse_entries(text):
    """Yield (issue_id, title, start_line, end_line, body) per `### I-XXX` entry.

    start_line/end_line are 1-based and inclusive.
    """
    lines = text.splitlines()
    entries = []
    current = None  # [id, title, start_line_index]

    def close(end):
        # end is the exclusive 0-based stop; trim trailing blank lines
        while end > current[2] + 1 and not lines[end - 1].strip():
            end -= 1
        entries.append((current[0], current[1], current[2] + 1, end, lines[current[2]:end]))

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


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("issue", nargs="?", help="Issue id, e.g. I-032 or 32")
    ap.add_argument("--latest", action="store_true", help="Print the highest-numbered issue id")
    ap.add_argument("--list", action="store_true", help="List all issue ids and titles")
    ap.add_argument("--file", default="ISSUES.md", type=Path, help="Path to ISSUES.md (default: ./ISSUES.md)")
    args = ap.parse_args()

    if not args.file.is_file():
        sys.exit(f"error: {args.file} not found")

    entries = parse_entries(args.file.read_text(encoding="utf-8"))
    if not entries:
        sys.exit(f"error: no issue entries found in {args.file}")

    if args.latest:
        print(max(entries, key=lambda e: int(e[0][2:]))[0])
        return

    if args.list:
        for issue_id, title, start, end, _ in entries:
            print(f"{issue_id} (lines {start}-{end}) — {title}")
        return

    if not args.issue:
        ap.error("give an issue id, or use --latest / --list")

    issue_id = normalize_id(args.issue)
    if issue_id is None:
        sys.exit(f"error: invalid issue id: {args.issue!r}")

    for eid, _title, start, end, body in entries:
        if eid == issue_id:
            print(f"{args.file}:{start}-{end}")
            print("\n".join(body).strip())
            return
    sys.exit(f"error: {issue_id} not found in {args.file}")


if __name__ == "__main__":
    main()
