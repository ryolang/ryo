#!/usr/bin/env bash
set -euo pipefail

# check_file_length.sh — fail if any Rust source file exceeds MAX_LINES lines.
# File-level navigability gate (rust-lang src/tools/tidy convention, tests
# included). Run it locally before pushing; CI runs the same script.
# Split oversized files — there is no exemption list.
#
# Usage: ./scripts/check_file_length.sh

MAX_LINES=2000

main() {
    cd "$(dirname "$0")/.."

    local failed=0
    local file lines

    while IFS= read -r file; do
        lines=$(awk 'END {print NR}' "$file")
        if [ "$lines" -gt "$MAX_LINES" ]; then
            echo "Error: $file has $lines lines (limit: $MAX_LINES)" >&2
            failed=1
        fi
    done < <(find . -name '*.rs' -not -path './target/*')

    if [ "$failed" -ne 0 ]; then
        echo "Split oversized files; there is no allowlist." >&2
        exit 1
    fi

    echo "All Rust source files are within the $MAX_LINES-line limit."
}

main "$@"
