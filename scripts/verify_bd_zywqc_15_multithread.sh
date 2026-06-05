#!/usr/bin/env bash
set -euo pipefail

# bd-zywqc.15: Verify multi-threaded (intra-process) concurrency coverage.
#
# Runs the bd_zywqc_15_multithread test suite which validates:
#  - Per-task-connection pattern (each thread owns its Connection)
#  - Crash injection (panic in one thread doesn't break others)
#  - Throughput scaling (csqlite + fsqlite)
#  - Per-thread structured logging with unique span_id
#  - Hot-page contention handling
#  - Cross-engine read compatibility
#
# Usage:
#   ./scripts/verify_bd_zywqc_15_multithread.sh [--target-dir DIR]

BEAD_ID="bd-zywqc.15"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

TARGET_DIR="${CARGO_TARGET_DIR:-/tmp/fsqlite-zywqc15-target}"
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

LOG_FILE="$TARGET_DIR/bd_zywqc_15_verify.log"
RESULT_FILE="$TARGET_DIR/bd_zywqc_15_result.json"
mkdir -p "$TARGET_DIR"

echo "[$BEAD_ID] Running multi-threaded concurrency verification..."
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
        --test bd_zywqc_15_multithread \
        "$filter" -- --nocapture >>"$LOG_FILE" 2>&1; then
        echo "ok"
        TESTS_PASSED=$((TESTS_PASSED + 1))
    else
        echo "FAILED"
        TESTS_FAILED=$((TESTS_FAILED + 1))
    fi
}

: > "$LOG_FILE"

echo "[$BEAD_ID] Phase 1: Default 16-task run"
run_test_filter "m1_default_16_tasks_per_connection" "default-16t"

echo "[$BEAD_ID] Phase 2: Crash injection"
run_test_filter "m2_panic_in_one_thread_others_unaffected" "crash-inject"

echo "[$BEAD_ID] Phase 3: Throughput scaling"
run_test_filter "m3_throughput_scaling_csqlite" "scaling-csqlite"
run_test_filter "m4_throughput_scaling_fsqlite" "scaling-fsqlite"

echo "[$BEAD_ID] Phase 4: Structured logging"
run_test_filter "m5_per_thread_logs_distinguishable" "span-id"

echo "[$BEAD_ID] Phase 5: Contention and cross-check"
run_test_filter "m6_hot_page_contention" "hot-page"
run_test_filter "m7_fsqlite_output_readable_by_csqlite" "cross-check"

VERDICT="pass"
if [[ $TESTS_FAILED -gt 0 ]]; then
    VERDICT="fail"
fi

cat > "$RESULT_FILE" <<RESULT_EOF
{
  "bead_id": "bd-zywqc.15",
  "schema_version": "fsqlite-e2e.verify_script_result.v1",
  "tests_total": $TESTS_TOTAL,
  "tests_passed": $TESTS_PASSED,
  "tests_failed": $TESTS_FAILED,
  "verdict": "$VERDICT",
  "phases": [
    "Default 16-task run (per-task-connection, disjoint writes)",
    "Crash injection (panic in one thread, others unaffected)",
    "Throughput scaling (csqlite 1/2/4/8t, fsqlite 1/2/4/8t)",
    "Structured logging (unique span_id per thread, valid JSONL)",
    "Contention and cross-check (hot-page updates, csqlite reads fsqlite output)"
  ],
  "log_path": "$LOG_FILE",
  "acceptance_criteria_validated": [
    "AC1: tests/multithread runs 16-task default green",
    "AC3: Crash-injection (panic in task) leaves other tasks unaffected",
    "AC4: Throughput scaling test (1,2,4,8 threads)",
    "AC5: Per-task structured logs distinguishable via span_id"
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
