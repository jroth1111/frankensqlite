#!/usr/bin/env bash
set -euo pipefail

# bd-aiica: CI integrity-check job.
#
# Walks a directory for .fsqlite artifacts and verifies each via rusqlite
# (C SQLite) PRAGMA integrity_check. Runs checks in parallel via xargs.
# Reports results as a markdown summary table.
#
# Usage:
#   ./scripts/ci_integrity_check.sh [--artifact-dir DIR] [--cache-file PATH]
#                                   [--parallel N] [--upload-failures]
#
# Exit: 0 if all pass, 1 if any fail, 2 on usage error.

BEAD_ID="bd-aiica"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

ARTIFACT_DIR="${1:-target/test-artifacts/concurrent}"
CACHE_FILE="${CACHE_FILE:-target/test-artifacts/.verify-cache.json}"
PARALLEL="${PARALLEL:-$(nproc 2>/dev/null || echo 4)}"
RESULT_DIR="${RESULT_DIR:-/tmp/fsqlite-integrity-results}"
SUMMARY_MD="${SUMMARY_MD:-$RESULT_DIR/summary.md}"

mkdir -p "$RESULT_DIR"

echo "[$BEAD_ID] Cross-engine integrity check"
echo "[$BEAD_ID] Artifact dir: $ARTIFACT_DIR"
echo "[$BEAD_ID] Parallel: $PARALLEL"

if [[ ! -d "$ARTIFACT_DIR" ]]; then
    echo "[$BEAD_ID] No artifact directory at $ARTIFACT_DIR — skipping (no artifacts to check)"
    exit 0
fi

# Collect all .fsqlite files
mapfile -t DB_FILES < <(find "$ARTIFACT_DIR" -name '*.fsqlite' -o -name '*.db' -o -name '*.sqlite' 2>/dev/null | sort)
TOTAL=${#DB_FILES[@]}

if [[ $TOTAL -eq 0 ]]; then
    echo "[$BEAD_ID] No database artifacts found in $ARTIFACT_DIR"
    exit 0
fi

echo "[$BEAD_ID] Found $TOTAL database artifacts"

PASSED=0
FAILED=0
FAILED_PATHS=()
RESULTS=()

check_one_db() {
    local db_path="$1"
    local result_file="$2"
    local basename
    basename="$(basename "$db_path")"

    if [[ ! -f "$db_path" ]]; then
        echo "SKIP|$basename|file not found" > "$result_file"
        return
    fi

    local file_size
    file_size="$(stat -c%s "$db_path" 2>/dev/null || echo 0)"

    # Zero-byte files are valid empty databases
    if [[ "$file_size" -eq 0 ]]; then
        echo "OK|$basename|empty file (valid)" > "$result_file"
        return
    fi

    local output
    if output=$(sqlite3 "$db_path" "PRAGMA integrity_check;" 2>&1); then
        if echo "$output" | grep -q "^ok$"; then
            echo "OK|$basename|integrity_check passed" > "$result_file"
        else
            echo "FAIL|$basename|$output" > "$result_file"
        fi
    else
        echo "FAIL|$basename|sqlite3 error: $output" > "$result_file"
    fi
}
export -f check_one_db

# Run checks in parallel
echo "[$BEAD_ID] Running integrity checks..."
T0=$(date +%s%N)

for i in "${!DB_FILES[@]}"; do
    db="${DB_FILES[$i]}"
    result_file="$RESULT_DIR/result_${i}.txt"
    check_one_db "$db" "$result_file" &
    # Throttle parallelism
    if (( (i + 1) % PARALLEL == 0 )); then
        wait
    fi
done
wait

T1=$(date +%s%N)
ELAPSED_MS=$(( (T1 - T0) / 1000000 ))

# Collect results
{
    echo "# Cross-Engine Integrity Check Results"
    echo ""
    echo "| # | Status | Artifact | Detail |"
    echo "|---|--------|----------|--------|"
} > "$SUMMARY_MD"

for i in "${!DB_FILES[@]}"; do
    result_file="$RESULT_DIR/result_${i}.txt"
    if [[ -f "$result_file" ]]; then
        IFS='|' read -r status name detail < "$result_file"
        if [[ "$status" == "OK" ]]; then
            PASSED=$((PASSED + 1))
            echo "| $((i+1)) | :white_check_mark: | \`$name\` | $detail |" >> "$SUMMARY_MD"
        elif [[ "$status" == "FAIL" ]]; then
            FAILED=$((FAILED + 1))
            FAILED_PATHS+=("${DB_FILES[$i]}")
            echo "| $((i+1)) | :x: | \`$name\` | $detail |" >> "$SUMMARY_MD"
        else
            echo "| $((i+1)) | :fast_forward: | \`$name\` | $detail |" >> "$SUMMARY_MD"
        fi
    fi
done

{
    echo ""
    echo "**Total**: $TOTAL artifacts, $PASSED passed, $FAILED failed, ${ELAPSED_MS}ms"
} >> "$SUMMARY_MD"

echo ""
echo "[$BEAD_ID] Results: $PASSED/$TOTAL passed, $FAILED failed (${ELAPSED_MS}ms)"
echo "[$BEAD_ID] Summary: $SUMMARY_MD"

if [[ $FAILED -gt 0 ]]; then
    echo ""
    echo "[$BEAD_ID] FAILED artifacts:"
    for path in "${FAILED_PATHS[@]}"; do
        echo "  - $path"
    done
    echo ""
    echo "[$BEAD_ID] FAILED — $FAILED integrity check failure(s)"
    exit 1
fi

echo "[$BEAD_ID] PASSED — all $TOTAL artifacts verified"
