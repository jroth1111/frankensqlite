#!/usr/bin/env bash
set -euo pipefail

# bd-zywqc.1: Verify structured-logging schema contracts.
#
# Runs the integration tests validating TraceContext, OpEvent,
# subscriber installation, schema validation, and JSONL emission.
#
# Usage:
#   ./scripts/verify_bd_zywqc_1_tracing_schema.sh [--target-dir DIR]
#
# Artifacts:
#   $TARGET_DIR/bd_zywqc_1_verify.log    — full test output
#   $TARGET_DIR/bd_zywqc_1_result.json   — structured result

BEAD_ID="bd-zywqc.1"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

TARGET_DIR="${CARGO_TARGET_DIR:-/tmp/fsqlite-zywqc1-target}"
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

LOG_FILE="$TARGET_DIR/${BEAD_ID//./_}_verify.log"
RESULT_FILE="$TARGET_DIR/${BEAD_ID//./_}_result.json"
mkdir -p "$TARGET_DIR"

echo "[$BEAD_ID] Running structured-logging schema tests..."
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
        --test bd_zywqc_1_tracing_schema \
        "$filter" -- --nocapture >>"$LOG_FILE" 2>&1; then
        echo "ok"
        TESTS_PASSED=$((TESTS_PASSED + 1))
    else
        echo "FAILED"
        TESTS_FAILED=$((TESTS_FAILED + 1))
    fi
}

: > "$LOG_FILE"

echo "[$BEAD_ID] Phase 1: TraceContext identity"
run_test_filter "p1_trace_context_new_has_all_required_fields" "ctx-new"
run_test_filter "p1_seeded_context_is_reproducible" "ctx-seed"
run_test_filter "p1_different_seeds_produce_different_contexts" "ctx-diff"
run_test_filter "p1_context_json_round_trip" "ctx-json"

echo "[$BEAD_ID] Phase 2: OpEvent schema"
run_test_filter "p2_op_event_contains_all_16_required_fields" "event-fields"
run_test_filter "p2_op_event_json_round_trip" "event-json"
run_test_filter "p2_optional_fields_omitted_when_none" "opt-fields"

echo "[$BEAD_ID] Phase 3: OpType + OpOutcome enums"
run_test_filter "p3_all_14_op_types_serialize_distinctly" "op-types"
run_test_filter "p3_all_6_outcomes_serialize_distinctly" "outcomes"

echo "[$BEAD_ID] Phase 4: subscriber + JSON emission"
run_test_filter "p4_test_subscriber_emits_valid_jsonl" "emit-jsonl"
run_test_filter "p4_emitted_lines_contain_trace_context_fields" "emit-ctx"

echo "[$BEAD_ID] Phase 5: schema validation"
run_test_filter "p5_validate_event_line_accepts_valid_json" "valid-line"
run_test_filter "p5_validate_event_line_rejects_missing_level" "reject-level"
run_test_filter "p5_validate_event_line_rejects_garbage" "reject-garbage"
run_test_filter "p5_validate_harness_line_checks_all_4_context_fields" "harness-ctx"

echo "[$BEAD_ID] Phase 6: bulk emission"
run_test_filter "p6_100_events_all_valid_jsonl" "bulk-100"

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
    "TraceContext identity (new, seeded, different, JSON round-trip)",
    "OpEvent schema (16 required fields, JSON round-trip, optional omission)",
    "OpType + OpOutcome enums (14 types, 6 outcomes, distinct serialization)",
    "subscriber + JSON emission (valid JSONL, trace context fields)",
    "schema validation (accept valid, reject missing, reject garbage, harness fields)",
    "bulk emission (100 events, zero invalid lines)"
  ],
  "log_path": "$LOG_FILE",
  "schema_contracts_validated": [
    "TraceContext: run_id, trace_id, workspace_id, host, started_at_unix_nanos",
    "TraceContext: seeded contexts are deterministic and reproducible",
    "OpEvent: 12+ required fields including run_id/trace_id/workspace_id/host",
    "OpFields: optional fields (wall_ms, retry_count, error_detail, table_name, rows_affected) omitted when None",
    "OpType: 14 variants with distinct snake_case serialization",
    "OpOutcome: 6 variants with distinct snake_case serialization",
    "JSON subscriber emits valid JSONL with timestamp, level, target fields",
    "Emitted lines contain TraceContext fields for cross-correlation",
    "Schema validator accepts conforming lines, rejects non-conforming",
    "100-event bulk emission produces zero invalid JSON lines"
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
