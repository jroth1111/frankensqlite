#!/usr/bin/env bash
set -euo pipefail

# bd-wsw3p: Verify concurrent-write-only benchmark validation.
#
# Runs the concurrent write showcase tests that validate FrankenSQLite's
# page-level MVCC scaling advantage over C SQLite's serialized writers.
#
# Usage:
#   ./scripts/verify_bd_wsw3p_concurrent_showcase.sh [--target-dir DIR]
#
# Artifacts:
#   $TARGET_DIR/bd_wsw3p_verify.log   — full test output
#   $TARGET_DIR/bd_wsw3p_result.json  — structured result

BEAD_ID="bd-wsw3p"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

TARGET_DIR="${CARGO_TARGET_DIR:-/tmp/fsqlite-wsw3p-target}"
while [[ $# -gt 0 ]]; do
    case "$1" in
        --target-dir)
            TARGET_DIR="$2"
            shift 2
            ;;
        *)
            echo "Unknown option: $1" >&2
            exit 2
            ;;
    esac
done

LOG_FILE="$TARGET_DIR/${BEAD_ID//-/_}_verify.log"
RESULT_FILE="$TARGET_DIR/${BEAD_ID//-/_}_result.json"
mkdir -p "$TARGET_DIR"

echo "[$BEAD_ID] Running concurrent write showcase tests..."
echo "[$BEAD_ID] Target dir: $TARGET_DIR"
echo "[$BEAD_ID] Log: $LOG_FILE"

TESTS_PASSED=0
TESTS_FAILED=0
TESTS_TOTAL=0

run_test_filter() {
    local filter="$1"
    local label="$2"
    TESTS_TOTAL=$((TESTS_TOTAL + 1))
    echo -n "  [$label] $filter ... "
    if env CARGO_TARGET_DIR="$TARGET_DIR" cargo test -p fsqlite-e2e \
        --test bd_wsw3p_concurrent_write_showcase \
        "$filter" -- --nocapture >>"$LOG_FILE" 2>&1; then
        echo "ok"
        TESTS_PASSED=$((TESTS_PASSED + 1))
    else
        echo "FAILED"
        TESTS_FAILED=$((TESTS_FAILED + 1))
    fi
}

: > "$LOG_FILE"

echo "[$BEAD_ID] Phase 1: correctness"
run_test_filter "t1_csqlite_concurrent_writes_produce_correct_data" "csqlite-data"
run_test_filter "t2_fsqlite_concurrent_writes_produce_correct_data" "fsqlite-data"
run_test_filter "t6_rusqlite_verification_catches_data_on_fsqlite_db" "rusqlite-verify"

echo "[$BEAD_ID] Phase 2: structured output"
run_test_filter "t3_structured_json_has_required_fields" "json-fields"
run_test_filter "t7_each_thread_reports_nonzero_wall_time" "thread-timing"

echo "[$BEAD_ID] Phase 3: scaling advantage"
run_test_filter "t4_fsqlite_scales_better_than_csqlite_at_4_threads" "scaling-4t"
run_test_filter "t5_full_showcase_4_8_produces_artifact_bundle" "showcase-bundle"

VERDICT="pass"
if [[ $TESTS_FAILED -gt 0 ]]; then
    VERDICT="fail"
fi

cat > "$RESULT_FILE" <<RESULT_EOF
{
  "bead_id": "$BEAD_ID",
  "schema_version": "fsqlite-e2e.verify_script_result.v1",
  "tests_total": $TESTS_TOTAL,
  "tests_passed": $TESTS_PASSED,
  "tests_failed": $TESTS_FAILED,
  "verdict": "$VERDICT",
  "phases": [
    "correctness (csqlite + fsqlite data integrity, rusqlite cross-check)",
    "structured output (JSON fields, per-thread timing)",
    "scaling advantage (1t→4t scaling ratio, full 4/8t showcase)"
  ],
  "log_path": "$LOG_FILE",
  "contracts_validated": [
    "Both engines produce correct row counts per thread",
    "FrankenSQLite MVCC scales better than C SQLite WAL_WRITE_LOCK at 4+ threads",
    "Structured JSON output contains engine, n_threads, throughput, per_thread arrays",
    "Per-thread metrics include thread_id, rows_inserted, wall_ms, retries",
    "Artifact bundle written to temp directory",
    "rusqlite cross-verification of fsqlite DB file integrity"
  ]
}
RESULT_EOF

echo ""
echo "[$BEAD_ID] Result: $VERDICT ($TESTS_PASSED/$TESTS_TOTAL passed)"
echo "[$BEAD_ID] Structured result: $RESULT_FILE"

if [[ "$VERDICT" = "fail" ]]; then
    echo "[$BEAD_ID] FAILED — see $LOG_FILE for details"
    exit 1
fi
