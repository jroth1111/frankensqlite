#!/usr/bin/env bash
set -euo pipefail

# bd-zuzb8: Verify WAL integrity cross-check with tiered fallback.
#
# Runs the bd_zuzb8_wal_integrity integration test suite which validates:
#  - Tier 1: rusqlite PRAGMA checks on clean/corrupt/multi-writer DBs
#  - Tier 2: raw-page diagnostics when rusqlite can't open
#  - Artifact bundle serialization round-trip
#  - Concurrent writer post-verification pattern
#
# Usage:
#   ./scripts/verify_bd_zuzb8_wal_integrity.sh [--target-dir DIR]
#
# Artifacts:
#   $TARGET_DIR/bd_zuzb8_verify.log      — full test output
#   $TARGET_DIR/bd_zuzb8_result.json     — structured result

BEAD_ID="bd-zuzb8"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

TARGET_DIR="${CARGO_TARGET_DIR:-/tmp/fsqlite-zuzb8-target}"
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

echo "[$BEAD_ID] Running WAL integrity cross-check verification..."
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
        --test bd_zuzb8_wal_integrity \
        "$filter" -- --nocapture >>"$LOG_FILE" 2>&1; then
        echo "ok"
        TESTS_PASSED=$((TESTS_PASSED + 1))
    else
        echo "FAILED"
        TESTS_FAILED=$((TESTS_FAILED + 1))
    fi
}

: > "$LOG_FILE"

echo "[$BEAD_ID] Phase 1: Tier-1 clean DB verification"
run_test_filter "t1_clean_db_returns_ok_no_artifact" "tier1-clean"
run_test_filter "t2_multi_writer_db_passes" "tier1-multi"

echo "[$BEAD_ID] Phase 2: Tier-2 fallback verification"
run_test_filter "t3_wal_truncated_produces_raw_diagnostics" "tier2-wal-trunc"
run_test_filter "t4_corrupt_page1_produces_artifact_with_raw_diag" "tier2-corrupt"
run_test_filter "t5_corrupt_page1_raw_diag_reads_wal_info" "tier2-wal-info"

echo "[$BEAD_ID] Phase 3: Serialization"
run_test_filter "t6_verify_artifact_json_round_trip" "serde-artifact"
run_test_filter "t7_raw_page_diagnostics_json_round_trip" "serde-diag"
run_test_filter "t8_write_artifact_bundle_creates_file" "bundle-write"

echo "[$BEAD_ID] Phase 4: Edge cases"
run_test_filter "t9_nonexistent_file_propagates_error" "edge-nofile"
run_test_filter "t10_completely_empty_file_handled_gracefully" "edge-empty"
run_test_filter "t11_garbage_file_produces_artifact" "edge-garbage"

echo "[$BEAD_ID] Phase 5: Integration pattern"
run_test_filter "t12_verify_after_concurrent_rusqlite_writes" "integration"

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
    "Tier-1 clean DB (rusqlite PRAGMA checks, no artifact on pass)",
    "Tier-2 fallback (WAL truncation, corrupt page-1 → raw diagnostics)",
    "Serialization (VerifyArtifact + RawPageDiagnostics JSON round-trip)",
    "Edge cases (nonexistent, empty, garbage files)",
    "Integration pattern (concurrent rusqlite writes → cross-check)"
  ],
  "log_path": "$LOG_FILE",
  "acceptance_criteria_validated": [
    "AC1: verify_with_c_sqlite(path) exists and returns VerifyReport",
    "AC2: verify_concurrency_artifact wires cross-check as post-run step",
    "AC3: Tiered fallback verified (WAL-truncate, corrupt page-1)",
    "AC5: Dump format (VerifyArtifact) parses with serde round-trip"
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
