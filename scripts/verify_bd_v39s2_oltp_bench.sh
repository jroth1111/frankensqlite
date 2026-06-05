#!/usr/bin/env bash
set -euo pipefail

# bd-v39s2: Verify mixed-read-write OLTP benchmark.
#
# Runs the mt-oltp-bench binary with two configurations:
#   1. 90/10 read/write split (9R/1W)
#   2. 50/50 balanced (4R/4W)
#
# Usage:
#   ./scripts/verify_bd_v39s2_oltp_bench.sh [--target-dir DIR]

BEAD_ID="bd-v39s2"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

TARGET_DIR="${CARGO_TARGET_DIR:-/tmp/fsqlite-v39s2-target}"
while [[ $# -gt 0 ]]; do
    case "$1" in
        --target-dir) TARGET_DIR="$2"; shift 2 ;;
        *) echo "Unknown option: $1" >&2; exit 2 ;;
    esac
done

LOG_FILE="$TARGET_DIR/bd_v39s2_verify.log"
RESULT_FILE="$TARGET_DIR/bd_v39s2_result.json"
mkdir -p "$TARGET_DIR"

echo "[$BEAD_ID] Mixed OLTP benchmark verification"
echo "[$BEAD_ID] Target dir: $TARGET_DIR"

TESTS_PASSED=0
TESTS_FAILED=0
TESTS_TOTAL=0

run_phase() {
    local label="$1"
    local extra_args="$2"
    TESTS_TOTAL=$((TESTS_TOTAL + 1))
    echo -n "  [$label] mt-oltp-bench $extra_args ... "
    if env CARGO_TARGET_DIR="$TARGET_DIR" cargo run -p fsqlite-e2e \
        --bin mt-oltp-bench --release -- \
        $extra_args >>"$LOG_FILE" 2>&1; then
        echo "ok"
        TESTS_PASSED=$((TESTS_PASSED + 1))
    else
        echo "FAILED"
        TESTS_FAILED=$((TESTS_FAILED + 1))
    fi
}

: > "$LOG_FILE"

echo "[$BEAD_ID] Phase 1: Unit tests"
TESTS_TOTAL=$((TESTS_TOTAL + 1))
echo -n "  [unit-tests] cargo test bd_v39s2_oltp_bench ... "
if env CARGO_TARGET_DIR="$TARGET_DIR" cargo test -p fsqlite-e2e \
    --test bd_v39s2_oltp_bench >>"$LOG_FILE" 2>&1; then
    echo "ok"
    TESTS_PASSED=$((TESTS_PASSED + 1))
else
    echo "FAILED"
    TESTS_FAILED=$((TESTS_FAILED + 1))
fi

echo "[$BEAD_ID] Phase 2: 90/10 read/write split (9R/1W)"
run_phase "90-10" "--seed-rows=500 --ops-per-thread=500 --readers=9 --writers=1 --iters=1 --json-output=$TARGET_DIR/oltp_90_10.json"

echo "[$BEAD_ID] Phase 3: 50/50 balanced (4R/4W)"
run_phase "50-50" "--seed-rows=500 --ops-per-thread=500 --readers=4 --writers=4 --iters=1 --json-output=$TARGET_DIR/oltp_50_50.json"

echo "[$BEAD_ID] Phase 4: Pure read (4R/0W baseline)"
run_phase "pure-read" "--seed-rows=500 --ops-per-thread=500 --readers=4 --writers=0 --iters=1 --json-output=$TARGET_DIR/oltp_pure_read.json"

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
    "Unit tests (Jain fairness, percentile, latency stats, JSON round-trip)",
    "90/10 read/write split (9R/1W, 500 seed, 500 ops/thread)",
    "50/50 balanced (4R/4W, 500 seed, 500 ops/thread)",
    "Pure read baseline (4R/0W, 500 seed, 500 ops/thread)"
  ],
  "log_path": "$LOG_FILE"
}
RESULT_EOF

echo ""
echo "[$BEAD_ID] Result: $VERDICT ($TESTS_PASSED/$TESTS_TOTAL passed)"
echo "[$BEAD_ID] Structured result: $RESULT_FILE"

if [[ "$VERDICT" = "fail" ]]; then
    echo "[$BEAD_ID] FAILED — see $LOG_FILE for details"
    exit 1
fi
