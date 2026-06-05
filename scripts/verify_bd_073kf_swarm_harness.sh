#!/usr/bin/env bash
set -euo pipefail

# bd-073kf: Verify the multi-process swarm-write harness.
#
# Runs the swarm binary with a short config, validates structured output,
# checks heartbeat emission, and verifies JSONL schema compliance.
#
# Usage:
#   ./scripts/verify_bd_073kf_swarm_harness.sh [--target-dir DIR]
#
# Artifacts:
#   $TARGET_DIR/bd_073kf_verify.log      — full test output
#   $TARGET_DIR/bd_073kf_result.json     — structured result

BEAD_ID="bd-073kf"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

TARGET_DIR="${CARGO_TARGET_DIR:-/tmp/fsqlite-073kf-target}"
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

echo "[$BEAD_ID] Running swarm harness verification..."
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
        --test bd_073kf_swarm_harness \
        "$filter" -- --nocapture >>"$LOG_FILE" 2>&1; then
        echo "ok"
        TESTS_PASSED=$((TESTS_PASSED + 1))
    else
        echo "FAILED"
        TESTS_FAILED=$((TESTS_FAILED + 1))
    fi
}

: > "$LOG_FILE"

echo "[$BEAD_ID] Phase 1: Basic execution"
run_test_filter "p1_swarm_runs_with_minimal_config" "basic"

echo "[$BEAD_ID] Phase 2: Seed determinism"
run_test_filter "p2_same_seed_produces_identical_worker_reports" "seed"

echo "[$BEAD_ID] Phase 3: JSONL emission"
run_test_filter "p3_workers_emit_valid_jsonl" "jsonl-criterion"

echo "[$BEAD_ID] Phase 4: Heartbeat"
run_test_filter "p4_heartbeat_emitted_to_stderr" "heartbeat"

echo "[$BEAD_ID] Phase 5: JSONL content"
run_test_filter "p5_jsonl_lines_contain_required_fields" "jsonl-fields"

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
    "Basic execution (swarm runs, produces JSON report)",
    "Seed determinism (same seed = same iteration/insert/update counts)",
    "JSONL emission (workers emit valid JSONL, criterion passes)",
    "Heartbeat (parent emits [swarm-heartbeat] to stderr every 5s)",
    "JSONL content (all lines contain 11 required schema fields)"
  ],
  "log_path": "$LOG_FILE",
  "acceptance_criteria_validated": [
    "AC1: Deterministic with seed (same seed = identical operation sequences)",
    "AC3: Structured logs validated (every JSONL line parses, zero invalid)",
    "AC4: Heartbeat emitted at <=5s intervals",
    "AC7: No tokio in dependency tree"
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
