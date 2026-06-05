#!/usr/bin/env bash
set -euo pipefail

# bd-db300.7.7.3: Verify CI and operator packaging contracts.
#
# Validates retention class escalation, pass/fail signatures, artifact layout,
# structured log fields, counterexample bundles, and CI/local entrypoints.
#
# Usage:
#   ./scripts/verify_bd_db300_7_7_3_suite_packaging.sh [--target-dir DIR]
#
# Artifacts:
#   $TARGET_DIR/bd_db300_7_7_3_verify_packaging.log — full test output
#   $TARGET_DIR/bd_db300_7_7_3_result.json          — structured result

BEAD_ID="bd-db300.7.7.3"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

TARGET_DIR="${CARGO_TARGET_DIR:-/tmp/fsqlite-g773-target}"
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

LOG_FILE="$TARGET_DIR/${BEAD_ID//./_}_verify_packaging.log"
RESULT_FILE="$TARGET_DIR/${BEAD_ID//./_}_result.json"
mkdir -p "$TARGET_DIR"

echo "[$BEAD_ID] Running suite packaging contract tests..."
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
        --test bd_db300_7_7_3_suite_packaging \
        "$filter" -- --nocapture >>"$LOG_FILE" 2>&1; then
        echo "ok"
        TESTS_PASSED=$((TESTS_PASSED + 1))
    else
        echo "FAILED"
        TESTS_FAILED=$((TESTS_FAILED + 1))
    fi
}

: > "$LOG_FILE"

echo "[$BEAD_ID] Phase 1: retention class escalation"
run_test_filter "p1_quick_no_shadow_produces_quick_run_retention" "quick-run"
run_test_filter "p1_full_no_shadow_produces_full_proof_retention" "full-proof"
run_test_filter "p1_quick_with_shadow_forced_escalates_to_full_proof" "shadow-escalation"
run_test_filter "p1_diverged_verdict_produces_failure_bundle" "failure-bundle"
run_test_filter "p1_tripped_kill_switch_forces_failure_bundle" "tripped-ks"

echo "[$BEAD_ID] Phase 2: pass/fail signature completeness"
run_test_filter "p2_pending_shadow_execution_signature" "pending-sig"
run_test_filter "p2_all_five_signatures_distinct" "sig-distinct"

echo "[$BEAD_ID] Phase 3: CI vs local entrypoint prefixes"
run_test_filter "p3_local_context_has_no_rch_prefix" "local-prefix"
run_test_filter "p3_ci_context_rerun_uses_rch" "ci-prefix"

echo "[$BEAD_ID] Phase 4: artifact directory structure"
run_test_filter "p4_artifacts_written_to_output_dir" "artifacts"
run_test_filter "p4_rerun_scripts_are_executable" "exec-perms"

echo "[$BEAD_ID] Phase 5: structured JSONL log fields"
run_test_filter "p5_jsonl_log_contains_all_required_packaging_fields" "log-fields"

echo "[$BEAD_ID] Phase 6: counterexample bundle structure"
run_test_filter "p6_counterexample_bundle_has_required_fields" "cx-bundle"

echo "[$BEAD_ID] Phase 7: shadow contract validation"
run_test_filter "p7_shadow_off_rejects_non_not_run_verdict" "reject-shadow"
run_test_filter "p7_diverged_auto_generates_counterexample_bundle" "auto-cx"
run_test_filter "p7_divergence_class_without_diverged_verdict_rejected" "reject-div-class"

echo "[$BEAD_ID] Phase 8: mode coverage"
run_test_filter "p8_all_modes_produce_valid_packages" "all-modes"

echo "[$BEAD_ID] Phase 9: default output directory layout"
run_test_filter "p9_default_output_dir_encodes_parameters" "dir-layout"

echo "[$BEAD_ID] Phase 10: summary markdown"
run_test_filter "p10_summary_md_contains_key_fields" "summary-md"

echo "[$BEAD_ID] Phase 11: inline bundle emission"
run_test_filter "p11_emit_inline_bundle_on_stderr" "inline-bundle"

echo "[$BEAD_ID] Phase 12: activation regime enumeration"
run_test_filter "p12_all_five_activation_regimes_accepted" "regimes"

echo "[$BEAD_ID] Phase 13: package round-trip"
run_test_filter "p13_stdout_json_matches_file_package" "roundtrip"

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
    "retention class escalation",
    "pass/fail signature completeness",
    "CI vs local entrypoint prefixes",
    "artifact directory structure",
    "structured JSONL log fields",
    "counterexample bundle structure",
    "shadow contract validation",
    "mode coverage",
    "default output directory layout",
    "summary markdown",
    "inline bundle emission",
    "activation regime enumeration",
    "package round-trip"
  ],
  "log_path": "$LOG_FILE",
  "packaging_contracts_validated": [
    "retention_class: quick_run | full_proof | failure_bundle",
    "pass_fail_signature: pass.quick_contract | pass.full_contract | pending.shadow_execution | pass.shadow_clean | fail.shadow_divergence",
    "CI prefix: rch exec -- cargo ... vs local: cargo ...",
    "artifact layout: suite_package.json, suite_summary.md, logs/verify_suite.jsonl, rerun_entrypoint.sh, focused_rerun_entrypoint.sh",
    "counterexample bundle: counterexamples/shadow_counterexample_bundle.json",
    "structured JSONL log: 16+ required fields",
    "shadow contract: mode/verdict/kill_switch/divergence_class consistency",
    "inline bundle: VERIFY_SUITE_BUNDLE_JSON= on stderr",
    "5 activation regimes accepted",
    "3 modes: sqlite_reference, fsqlite_mvcc, fsqlite_single_writer"
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
