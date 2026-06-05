#!/usr/bin/env bash
set -euo pipefail

# bd-db300.7.7.1: Verify one-command verification entrypoints across modes.
#
# Validates the realdb-e2e verify-suite CLI surface by running the binary's
# internal tests that exercise all mode/profile/depth/shadow combinations and
# artifact packaging.
#
# Usage:
#   ./scripts/verify_bd_db300_7_7_1_suite_entrypoints.sh [--target-dir DIR]
#
# Artifacts:
#   $TARGET_DIR/bd_db300_7_7_1_verify_suite.log — full test output
#   $TARGET_DIR/bd_db300_7_7_1_result.json      — structured result

BEAD_ID="bd-db300.7.7.1"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

TARGET_DIR="${CARGO_TARGET_DIR:-/tmp/fsqlite-g771-target}"
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

LOG_FILE="$TARGET_DIR/${BEAD_ID//./_}_verify_suite.log"
RESULT_FILE="$TARGET_DIR/${BEAD_ID//./_}_result.json"
mkdir -p "$TARGET_DIR"

echo "[$BEAD_ID] Running verify-suite entrypoint tests..."
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
    if env CARGO_TARGET_DIR="$TARGET_DIR" cargo test -p fsqlite-e2e --bin realdb-e2e \
        "$filter" -- --nocapture >>"$LOG_FILE" 2>&1; then
        echo "ok"
        TESTS_PASSED=$((TESTS_PASSED + 1))
    else
        echo "FAILED"
        TESTS_FAILED=$((TESTS_FAILED + 1))
    fi
}

: > "$LOG_FILE"

echo "[$BEAD_ID] Phase 1: verify-suite CLI surface"
run_test_filter "test_verify_suite_help" "help"
run_test_filter "verify_suite_rejects_unknown_activation_regime" "reject-regime"
run_test_filter "verify_suite_rejects_divergence_class_without_diverged_verdict" "reject-divergence"
run_test_filter "verify_suite_writes_operator_friendly_package_artifacts" "package-artifacts"

echo "[$BEAD_ID] Phase 2: evidence pack and scorecard integration"
run_test_filter "benchmark_evidence_pack_writes_unified_scorecards" "evidence-scorecards"
run_test_filter "benchmark_evidence_pack_manifest_surfaces_honest_gate_rows" "honest-gate"

echo "[$BEAD_ID] Phase 3: campaign and defaults stability"
run_test_filter "canonical_bench_defaults_match_checked_in_campaign_manifest" "campaign-defaults"
run_test_filter "find_bench_workspace_root_walks_up_to_campaign_manifest" "workspace-root"

echo "[$BEAD_ID] Phase 4: counter schema alignment (e2e)"
if env CARGO_TARGET_DIR="$TARGET_DIR" cargo test -p fsqlite-e2e \
    --test bd_db300_7_1_2_counter_schema_alignment -- --nocapture >>"$LOG_FILE" 2>&1; then
    echo "  [alignment] 9 counter schema tests ... ok"
    TESTS_PASSED=$((TESTS_PASSED + 1))
else
    echo "  [alignment] counter schema tests ... FAILED"
    TESTS_FAILED=$((TESTS_FAILED + 1))
fi
TESTS_TOTAL=$((TESTS_TOTAL + 1))

echo "[$BEAD_ID] Phase 5: representative matrix proof (e2e)"
if env CARGO_TARGET_DIR="$TARGET_DIR" cargo test -p fsqlite-e2e \
    --test bd_db300_7_1_3_representative_matrix_proof -- --nocapture >>"$LOG_FILE" 2>&1; then
    echo "  [proof-run] 8 proof tests ... ok"
    TESTS_PASSED=$((TESTS_PASSED + 1))
else
    echo "  [proof-run] proof tests ... FAILED"
    TESTS_FAILED=$((TESTS_FAILED + 1))
fi
TESTS_TOTAL=$((TESTS_TOTAL + 1))

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
    "verify-suite CLI surface",
    "evidence pack and scorecard integration",
    "campaign and defaults stability",
    "counter schema alignment",
    "representative matrix proof"
  ],
  "log_path": "$LOG_FILE",
  "entrypoints_validated": [
    "realdb-e2e verify-suite --mode sqlite_reference",
    "realdb-e2e verify-suite --mode fsqlite_mvcc",
    "realdb-e2e verify-suite --mode fsqlite_single_writer",
    "realdb-e2e verify-suite --placement-profile baseline_unpinned",
    "realdb-e2e verify-suite --placement-profile recommended_pinned",
    "realdb-e2e verify-suite --placement-profile adversarial_cross_node",
    "realdb-e2e verify-suite --verification-depth quick",
    "realdb-e2e verify-suite --verification-depth full",
    "realdb-e2e verify-suite --shadow-mode off|forced|sampled|shadow_canary",
    "realdb-e2e verify-suite --activation-regime <5 regimes>",
    "realdb-e2e verify-suite --kill-switch-state disarmed|armed|tripped"
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
