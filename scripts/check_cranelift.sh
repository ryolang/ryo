#!/usr/bin/env bash
set -euo pipefail

# check_cranelift.sh — show what changed in Cranelift between the version Ryo
# currently uses (parsed from Cargo.lock) and any other version (default: the
# latest release). Resolves Ryo's Cranelift dependency version, queries
# crates.io for the exact commit SHAs, and prints the history of commits
# touching the cranelift/ directory (handling parallel release-branch
# history). Usage: ./scripts/check_cranelift.sh [target-version]
#
# GitHub API authentication: uses $GITHUB_TOKEN if set, otherwise falls back
# to `gh auth token` when the gh CLI is authenticated. Unauthenticated calls
# are limited to 60 requests/hour, which this script can exceed.

GITHUB_API="https://api.github.com/repos/bytecodealliance/wasmtime"
TOKEN=""

# Extract cranelift version from Cargo.lock string
get_cranelift_version_from_lockfile() {
    local lock_content="$1"
    # Parse Cargo.lock format for name = "cranelift" and output the following version line
    echo "$lock_content" | awk '
        /name = "cranelift"/ { found=1; next }
        found && /version = / {
            gsub(/"/, "", $3);
            print $3;
            exit
        }
    '
}

# Extract SHA for a specific version from crates.io JSON
get_sha_for_version() {
    local version="$1"
    local crates_io_json="$2"
    echo "$crates_io_json" | jq -r --arg ver "$version" '
        .versions[] | select(.num == $ver) | .trustpub_data.sha // empty
    '
}

# Extract latest version from crates.io JSON (highest semver)
get_latest_version() {
    local crates_io_json="$1"
    echo "$crates_io_json" | jq -r '
        [.versions[] | select(.yanked == false) | select(.num | test("^[0-9]+\\.[0-9]+\\.[0-9]+$"))] |
        sort_by(.num | split(".") | map(tonumber)) |
        last | .num
    '
}

# Resolve a GitHub token from the environment or the gh CLI
resolve_token() {
    if [ -n "${GITHUB_TOKEN:-}" ]; then
        TOKEN="$GITHUB_TOKEN"
    elif command -v gh &>/dev/null && gh auth status &>/dev/null; then
        TOKEN=$(gh auth token)
    fi
}

# Perform an API call, printing the response body on success. On HTTP errors,
# print a diagnostic to stderr and return 1.
fetch_api() {
    local url="$1"
    local headers=("-H" "User-Agent: ryo-compiler-dev-agent")
    if [ -n "$TOKEN" ]; then
        headers+=("-H" "Authorization: Bearer $TOKEN")
    fi

    local body http_code
    body=$(curl -sS --max-time 30 "${headers[@]}" -w $'\n%{http_code}' "$url") || {
        echo "Error: Failed to connect to $url" >&2
        return 1
    }
    http_code="${body##*$'\n'}"
    body="${body%$'\n'*}"

    if [ "$http_code" != "200" ]; then
        local msg
        msg=$(echo "$body" | jq -r '.message // empty' 2>/dev/null || true)
        echo "Error: API request failed (HTTP $http_code): ${msg:-$url}" >&2
        if [ "$http_code" = "403" ] && [ -z "$TOKEN" ]; then
            echo "Hint: unauthenticated GitHub API access is limited to 60 requests/hour." >&2
            echo "      Set GITHUB_TOKEN or authenticate the gh CLI to raise the limit." >&2
        fi
        return 1
    fi

    printf '%s' "$body"
}

# Echo "yes" if commit $1 is an ancestor of commit $2 (or equal to it),
# "no" otherwise. Uses the GitHub compare API: status "ahead"/"identical"
# means the base is reachable from the head. Returns 1 on API error.
check_ancestor() {
    local probe="$1" base="$2"
    local json status
    json=$(fetch_api "$GITHUB_API/compare/${probe}...${base}")
    status=$(echo "$json" | jq -r '.status')
    if [ "$status" = "ahead" ] || [ "$status" = "identical" ]; then
        echo "yes"
    else
        echo "no"
    fi
}

# Print formatted commit lines from a commits JSON array, for indices
# $2..$3 inclusive (0-based, newest first).
print_commits() {
    local commits_json="$1" from="$2" to="$3"
    [ "$from" -gt "$to" ] && return 0
    echo "$commits_json" | jq -r --argjson from "$from" --argjson to "$to" '
        .[$from : ($to + 1)][] |
        "\(.sha[0:7])\t\(.commit.committer.date | split("T")[0])\t\(.commit.author.name[0:15])\t\(.commit.message | split("\n")[0])"
    ' | while IFS=$'\t' read -r short_sha date author msg; do
        printf "[%s] %s | %-15s | %s\n" "$short_sha" "$date" "$author" "$msg"
    done
}

main() {
    if ! command -v curl &>/dev/null; then
        echo "Error: curl is required to run this script." >&2
        exit 1
    fi
    if ! command -v jq &>/dev/null; then
        echo "Error: jq is required to run this script." >&2
        exit 1
    fi

    resolve_token

    local start_ver=""
    local end_ver=""

    # 1. Resolve start version from Cargo.lock if not provided
    if [ $# -lt 2 ]; then
        local lock_file="Cargo.lock"
        if [ ! -f "$lock_file" ]; then
            echo "Error: Cargo.lock not found in current directory. Run from the Ryo workspace root." >&2
            exit 1
        fi
        local lock_content
        lock_content=$(cat "$lock_file")
        start_ver=$(get_cranelift_version_from_lockfile "$lock_content")
        if [ -z "$start_ver" ]; then
            echo "Error: Could not resolve current cranelift version from Cargo.lock." >&2
            exit 1
        fi
    else
        start_ver="$1"
    fi

    # 2. Get crates.io metadata
    echo "Fetching Cranelift package information from crates.io..." >&2
    local crates_io_json
    crates_io_json=$(fetch_api "https://crates.io/api/v1/crates/cranelift")

    # 3. Resolve end version
    if [ $# -eq 0 ]; then
        end_ver=$(get_latest_version "$crates_io_json")
    elif [ $# -eq 1 ]; then
        end_ver="$1"
    else
        end_ver="$2"
    fi

    if [ "$start_ver" = "$end_ver" ]; then
        echo "Installed Cranelift version ($start_ver) is already up-to-date with target version ($end_ver)."
        exit 0
    fi

    # 4. Resolve SHAs
    local start_sha end_sha
    start_sha=$(get_sha_for_version "$start_ver" "$crates_io_json")
    end_sha=$(get_sha_for_version "$end_ver" "$crates_io_json")

    if [ -z "$start_sha" ]; then
        echo "Error: Could not find Git commit SHA for Cranelift version $start_ver on crates.io." >&2
        exit 1
    fi
    if [ -z "$end_sha" ]; then
        echo "Error: Could not find Git commit SHA for Cranelift version $end_ver on crates.io." >&2
        exit 1
    fi

    echo ""
    echo "Cranelift Release Changes Tracker"
    echo "================================="
    echo "Comparing cranelift: $start_ver (${start_sha:0:7}) -> $end_ver (${end_sha:0:7})"
    echo ""
    echo "Commits touching cranelift/:"
    echo "---------------------------"

    # 5. Walk the end-branch history (newest first) page by page. Release
    # branches diverge, so stop at the first commit that is also an ancestor
    # of the start release. Each page costs one ancestor probe; the boundary
    # page is pinpointed with a binary search (~7 extra probes).
    local page=1
    while true; do
        local commits_json length
        commits_json=$(fetch_api "$GITHUB_API/commits?path=cranelift&sha=${end_sha}&per_page=100&page=${page}")
        length=$(echo "$commits_json" | jq 'length')

        if [ "$length" -eq 0 ]; then
            echo "Error: Reached end of commit history without finding commits shared with $start_ver." >&2
            exit 1
        fi

        local oldest
        oldest=$(echo "$commits_json" | jq -r '.[-1].sha')
        if [ "$(check_ancestor "$oldest" "$start_sha")" = "no" ]; then
            # The whole page is newer than the shared history: print and continue.
            print_commits "$commits_json" 0 $((length - 1))
            page=$((page + 1))
            continue
        fi

        # The boundary between release-only and shared commits is on this
        # page. Binary search for the first commit that is an ancestor of the
        # start release (invariant: commit at index hi is an ancestor).
        local lo=0 hi=$((length - 1))
        while [ "$lo" -lt "$hi" ]; do
            local mid=$(( (lo + hi) / 2 ))
            local mid_sha
            mid_sha=$(echo "$commits_json" | jq -r ".[$mid].sha")
            if [ "$(check_ancestor "$mid_sha" "$start_sha")" = "yes" ]; then
                hi=$mid
            else
                lo=$((mid + 1))
            fi
        done
        print_commits "$commits_json" 0 $((lo - 1))
        break
    done

    echo ""
    echo "Done! Comparison finished."
}

# Main execution entrypoint
if [ "${BASH_SOURCE[0]}" = "$0" ]; then
    main "$@"
fi
