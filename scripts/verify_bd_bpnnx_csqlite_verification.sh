#!/usr/bin/env bash
set -euo pipefail

# bd-bpnnx: Verify stock C SQLite verification helper contracts.
#
# Runs the integration tests that validate the VerifyReport struct,
# verify_with_c_sqlite() function, tiered fallback, metadata reads,
# JSON round-trip, and Display trait.
#
# Usage:
#   ./scripts/verify_bd_bpnnx_csqlite_verification.sh [--target-dir DIR]
#
# Artifacts:
#   $TARGET_DIR/bd_bpnnx_verify.log    — full test output
#   $TARGET_DIR/bd_bpnnx_result.json   — structured result

BEAD_ID="bd-bpnnx"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

TARGET_DIR="${CARGO_TARGET_DIR:-/tmp/fsqlite-bpnnx-target}"
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

echo "[$BEAD_ID] Running C SQLite verification helper tests..."
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
        --test bd_bpnnx_csqlite_verification \
        "$filter" -- --nocapture >>"$LOG_FILE" 2>&1; then
        echo "ok"
        TESTS_PASSED=$((TESTS_PASSED + 1))
    else
        echo "FAILED"
        TESTS_FAILED=$((TESTS_FAILED + 1))
    fi
}

: > "$LOG_FILE"

echo "[$BEAD_ID] Phase 1: basic pass/fail"
run_test_filter "t1_clean_populated_db_passes_all_checks" "clean-db"
run_test_filter "t2_nonexistent_file_returns_file_not_found_error" "not-found"
run_test_filter "t3_garbage_file_produces_fail_or_skipped_report" "garbage"

echo "[$BEAD_ID] Phase 2: WAL-specific scenarios"
run_test_filter "t4_wal_truncated_db_still_readable" "wal-truncated"
run_test_filter "t5_non_wal_journal_mode_skips_checkpoint" "non-wal"

echo "[$BEAD_ID] Phase 3: metadata correctness"
run_test_filter "t6_metadata_reflects_actual_schema" "metadata"
run_test_filter "t7_empty_db_has_zero_tables" "empty-db"

echo "[$BEAD_ID] Phase 4: serialization round-trip"
run_test_filter "t8_json_round_trip_preserves_all_fields" "json-rt"
run_test_filter "t9_check_result_enum_serializes_correctly" "check-serde"

echo "[$BEAD_ID] Phase 5: concurrent-write artifact"
run_test_filter "t10_multi_writer_artifact_verified" "multi-writer"

echo "[$BEAD_ID] Phase 6: timing sanity"
run_test_filter "t11_timings_are_non_negative" "timings"

echo "[$BEAD_ID] Phase 7: Display trait"
run_test_filter "t12_display_pass_contains_key_info" "display-pass"
run_test_filter "t13_display_fail_shows_check_results" "display-fail"

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
    "basic pass/fail (clean, nonexistent, garbage)",
    "WAL-specific (truncated WAL, non-WAL journal mode)",
    "metadata correctness (schema, tables, page size)",
    "serialization round-trip (JSON, CheckResult enum)",
    "concurrent-write artifact verification",
    "timing sanity",
    "Display trait (pass and fail formats)"
  ],
  "log_path": "$LOG_FILE",
  "contracts_validated": [
    "verify_with_c_sqlite returns VerifyReport with ok/quick_check/integrity_check fields",
    "FileNotFound error on nonexistent paths",
    "Corrupted files produce Fail or Skipped (not Pass)",
    "Truncated WAL does not crash verification",
    "Non-WAL journals skip checkpoint gracefully",
    "Metadata (schema_version, page_count, page_size, table_count) matches actual schema",
    "VerifyReport JSON round-trips with all fields preserved",
    "CheckResult enum serializes tagged correctly (Pass/Fail/Skipped)",
    "Multi-writer artifact (4 tables, 400 rows) verified successfully",
    "Timings are non-negative and total >= open",
    "Display trait shows VERIFY PASSED/FAILED with key info"
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
