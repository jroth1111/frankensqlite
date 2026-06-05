#!/usr/bin/env bash
# verify_t6_7_group_commit.sh — bd-1dp9.6.7.9.3 evidence pack runner
#
# Runs the group-commit durability, fairness, and failure-injection test suite
# and captures structured JSON-line logs for audit.
#
# Usage:
#   ./scripts/verify_t6_7_group_commit.sh [--target-dir DIR]
#
# Artifacts:
#   ci-artifacts/group-commit-evidence/  — test output + structured logs

set -euo pipefail

BEAD_ID="bd-1dp9.6.7.9.3"
ARTIFACT_DIR="ci-artifacts/group-commit-evidence"
TARGET_DIR="${1:---target-dir}"

if [[ "$TARGET_DIR" == "--target-dir" ]]; then
    TARGET_DIR="${CARGO_TARGET_DIR:-/tmp/cc3-target-v2}"
    shift 2>/dev/null || true
    if [[ "${1:-}" != "" ]]; then
        TARGET_DIR="$1"
    fi
fi

echo "=== ${BEAD_ID}: Group-Commit Evidence Pack ==="
echo "Target dir: ${TARGET_DIR}"
echo "Artifact dir: ${ARTIFACT_DIR}"
echo ""

mkdir -p "${ARTIFACT_DIR}"

# ── Build ──
echo "[1/3] Building test binary..."
CARGO_TARGET_DIR="${TARGET_DIR}" cargo test -p fsqlite-e2e \
    --test bd_1dp9_6_7_9_3_group_commit_durability --no-run 2>&1 \
    | tee "${ARTIFACT_DIR}/build.log" \
    | tail -3

# ── Run ──
echo ""
echo "[2/3] Running 12 group-commit scenarios (--test-threads=1)..."
CARGO_TARGET_DIR="${TARGET_DIR}" cargo test -p fsqlite-e2e \
    --test bd_1dp9_6_7_9_3_group_commit_durability \
    -- --test-threads=1 --nocapture 2>&1 \
    | tee "${ARTIFACT_DIR}/test_output.log" \
    | grep -E "^(test |running |test result:)" || true

# ── Extract structured logs ──
echo ""
echo "[3/3] Extracting structured logs..."
grep "^GROUP_COMMIT_DURABILITY:" "${ARTIFACT_DIR}/test_output.log" \
    | sed 's/^GROUP_COMMIT_DURABILITY://' \
    > "${ARTIFACT_DIR}/structured_logs.jsonl" 2>/dev/null || true

LOG_COUNT=$(wc -l < "${ARTIFACT_DIR}/structured_logs.jsonl" 2>/dev/null || echo 0)
echo "  Structured log entries: ${LOG_COUNT}"

# ── Summary ──
echo ""
echo "=== Evidence Pack Complete ==="
echo "Artifacts:"
echo "  ${ARTIFACT_DIR}/build.log"
echo "  ${ARTIFACT_DIR}/test_output.log"
echo "  ${ARTIFACT_DIR}/structured_logs.jsonl"
echo ""

# Check for failures in the test output
if grep -q "^test result: FAILED" "${ARTIFACT_DIR}/test_output.log" 2>/dev/null; then
    echo "FAIL: Some tests failed. See ${ARTIFACT_DIR}/test_output.log"
    exit 1
fi

PASS_COUNT=$(grep -c "^test .* ok$" "${ARTIFACT_DIR}/test_output.log" 2>/dev/null || echo 0)
echo "All ${PASS_COUNT} tests passed."
echo ""
echo "Replay command:"
echo "  cargo test -p fsqlite-e2e --test bd_1dp9_6_7_9_3_group_commit_durability -- --nocapture --test-threads=1"
