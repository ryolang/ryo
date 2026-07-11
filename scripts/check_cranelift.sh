#!/usr/bin/env bash
set -euo pipefail

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
        .versions[] | select(.num == $ver) | .trustpub_data.sha
    '
}

# Extract latest version from crates.io JSON (highest semver)
get_latest_version() {
    local crates_io_json="$1"
    echo "$crates_io_json" | jq -r '
        [.versions[] | select(.yanked == false)] |
        sort_by(.num | split(".") | map(tonumber)) |
        last | .num
    '
}

# Print commits from JSON, stop and return 0 if any SHA matches the stop_shas list.
# Otherwise, print all and return 1.
parse_and_print_commits() {
    local commits_json="$1"
    local stop_shas="$2"
    
    # We iterate using jq to get the output fields, formatted as a TSV/delimited string:
    # sha|date|author|message
    local IFS=$'\n'
    local found_start=1 # 1 is failure/not found in Bash return terms, 0 is success/found

    # Use a delimiter that is unlikely to be in fields
    local lines
    lines=$(echo "$commits_json" | jq -r '
        .[] | select(. != null) | 
        "\(.sha)\t\(.commit.committer.date)\t\(.commit.author.name)\t\(.commit.message | split("\n")[0])"
    ')

    for line in $lines; do
        local sha date author msg
        sha=$(echo "$line" | cut -d$'\t' -f1)
        date=$(echo "$line" | cut -d$'\t' -f2 | cut -d'T' -f1)
        author=$(echo "$line" | cut -d$'\t' -f3)
        msg=$(echo "$line" | cut -d$'\t' -f4)
        
        local short_sha="${sha:0:7}"
        
        # If this SHA is in our list of stop SHAs, we stop!
        # Wrap with spaces to prevent partial substring matches
        if [[ " $stop_shas " =~ " $sha " ]]; then
            found_start=0
            break
        fi

        # Print formatted commit line
        printf "[%s] %s | %-15s | %s\n" "$short_sha" "$date" "${author:0:15}" "$msg"
    done

    return $found_start
}

# Perform safe API curl calls with GitHub token if available
fetch_api() {
    local url="$1"
    local headers=("-H" "User-Agent: ryo-compiler-dev-agent")
    if [ -n "${GITHUB_TOKEN:-}" ]; then
        headers+=("-H" "Authorization: Bearer $GITHUB_TOKEN")
    fi
    curl -s --fail --max-time 30 "${headers[@]}" "$url"
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

    local start_ver=""
    local end_ver=""

    # 1. Resolve start version from Cargo.lock if not provided
    if [ $# -lt 2 ]; then
        local lock_file="Cargo.lock"
        if [ ! -f "$lock_file" ]; then
            echo "Error: Cargo.lock not found in current directory. Run from the Ryo workspace root." >&2
            exit 1
        fi
        local lock_content=$(cat "$lock_file")
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
    local crates_io_json=$(fetch_api "https://crates.io/api/v1/crates/cranelift")
    if [ -z "$crates_io_json" ] || [ "$crates_io_json" = "null" ]; then
        echo "Error: Failed to query crates.io API." >&2
        exit 1
    fi

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
    local start_sha=$(get_sha_for_version "$start_ver" "$crates_io_json")
    local end_sha=$(get_sha_for_version "$end_ver" "$crates_io_json")

    if [ -z "$start_sha" ] || [ "$start_sha" = "null" ]; then
        echo "Error: Could not find Git commit SHA for Cranelift version $start_ver on crates.io." >&2
        exit 1
    fi
    if [ -z "$end_sha" ] || [ "$end_sha" = "null" ]; then
        echo "Error: Could not find Git commit SHA for Cranelift version $end_ver on crates.io." >&2
        exit 1
    fi

    echo "Fetching starting commit history to find branch common ancestors..." >&2
    local stop_shas=""
    local stop_page=1
    local page_shas=""
    while true; do
        page_shas=$(fetch_api "https://api.github.com/repos/bytecodealliance/wasmtime/commits?path=cranelift&sha=${start_sha}&per_page=100&page=${stop_page}" | jq -r 'if (type == "array" and length > 0) then .[].sha else empty end' | tr '\n' ' ') || page_shas=""
        [ -z "$page_shas" ] && break
        stop_shas+="$page_shas"
        stop_page=$((stop_page + 1))
    done

    if [ -z "$stop_shas" ]; then
        echo "Error: Failed to fetch commit history for starting SHA $start_sha." >&2
        exit 1
    fi

    echo ""
    echo "Cranelift Release Changes Tracker"
    echo "================================="
    echo "Comparing cranelift: $start_ver (${start_sha:0:7}) -> $end_ver (${end_sha:0:7})"
    echo ""
    echo "Commits touching cranelift/:"
    echo "---------------------------"

    # 5. Fetch commits from GitHub with pagination
    local page=1
    local finished=1 # 1 means not finished, 0 means finished (found stop SHA)
    local total_count=0

    while [ $finished -ne 0 ]; do
        local url="https://api.github.com/repos/bytecodealliance/wasmtime/commits?path=cranelift&sha=${end_sha}&per_page=100&page=${page}"
        local commits_json=$(fetch_api "$url")
        
        if [ -z "$commits_json" ] || [ "$commits_json" = "null" ] || [ "$(echo "$commits_json" | jq -r 'type')" != "array" ] || [ "$(echo "$commits_json" | jq '. | length')" -eq 0 ]; then
            echo "Error: Reached end of GitHub commit history without finding starting SHA history." >&2
            break
        fi

        local page_count=$(echo "$commits_json" | jq '. | length')
        total_count=$((total_count + page_count))

        # parse_and_print_commits outputs lines and returns 0 if a stop SHA is reached
        if parse_and_print_commits "$commits_json" "$stop_shas"; then
            finished=0
        else
            page=$((page + 1))
        fi
    done

    echo ""
    echo "Done! Comparison finished."
}

# Main execution entrypoint
if [ "${BASH_SOURCE[0]}" = "$0" ]; then
    main "$@"
fi
