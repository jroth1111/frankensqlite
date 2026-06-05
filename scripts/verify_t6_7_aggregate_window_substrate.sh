#!/usr/bin/env bash
set -euo pipefail

# bd-1dp9.6.7.5.1: aggregate/window storage substrate verification.
#
# Runs deterministic file-backed GROUP BY, DISTINCT/ORDER BY, NULL/empty-group,
# window-stage, and structured-log tests with strict fallback rejection enabled.
#
# Usage:
#   ./scripts/verify_t6_7_aggregate_window_substrate.sh
#
# Artifacts:
#   artifacts/bd-1dp9.6.7.5.1/<run_id>/events.jsonl
#   artifacts/bd-1dp9.6.7.5.1/<run_id>/fsqlite-core-test.log
#   artifacts/bd-1dp9.6.7.5.1/<run_id>/report.json

BEAD_ID="bd-1dp9.6.7.5.1"
SCENARIO_ID="t6_7_aggregate_window_substrate"
SEED=6751

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
TIMESTAMP_UTC="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_ID="${BEAD_ID}-${TIMESTAMP_UTC}-${SEED}"
TRACE_ID="trace-${RUN_ID}"
ARTIFACT_ROOT="${ARTIFACT_ROOT:-artifacts/${BEAD_ID}}"
ARTIFACT_DIR="${ARTIFACT_DIR:-${ARTIFACT_ROOT}/${RUN_ID}}"
EVENTS_JSONL="${ARTIFACT_DIR}/events.jsonl"
LOG_FILE="${ARTIFACT_DIR}/fsqlite-core-test.log"
REPORT_JSON="${ARTIFACT_DIR}/report.json"
RUST_LOG_VALUE="${FSQLITE_VERIFY_RUST_LOG:-fsqlite.storage_wiring=debug}"

mkdir -p "${ARTIFACT_DIR}"
: > "${EVENTS_JSONL}"

emit_event() {
  local phase="$1"
  local event_type="$2"
  local outcome="$3"
  local message="$4"
  printf '{"trace_id":"%s","run_id":"%s","scenario_id":"%s","seed":%d,"phase":"%s","event_type":"%s","outcome":"%s","timestamp":"%s","message":"%s"}\n' \
    "${TRACE_ID}" "${RUN_ID}" "${SCENARIO_ID}" "${SEED}" "${phase}" "${event_type}" "${outcome}" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "${message}" \
    >> "${EVENTS_JSONL}"
}

sha256_file() {
  sha256sum "$1" | awk '{print $1}'
}

cd "${REPO_ROOT}"

REPLAY_COMMAND="RUN_ID=${RUN_ID} SCENARIO_ID=${SCENARIO_ID} RUST_LOG=${RUST_LOG_VALUE} rch exec -- cargo test -p fsqlite-core test_t6751 -- --nocapture"

echo "=== ${BEAD_ID}: aggregate/window storage substrate verification ==="
echo "run_id=${RUN_ID}"
echo "trace_id=${TRACE_ID}"
echo "scenario_id=${SCENARIO_ID}"
echo "artifact_dir=${ARTIFACT_DIR}"
echo "replay=${REPLAY_COMMAND}"

emit_event "bootstrap" "start" "running" "verification started"
emit_event "core_t6751" "start" "running" "running strict file-backed aggregate/window substrate tests"

if env RUN_ID="${RUN_ID}" SCENARIO_ID="${SCENARIO_ID}" RUST_LOG="${RUST_LOG_VALUE}" \
  rch exec -- cargo test -p fsqlite-core test_t6751 -- --nocapture >"${LOG_FILE}" 2>&1; then
  RESULT="pass"
  emit_event "core_t6751" "pass" "pass" "strict aggregate/window substrate tests passed"
else
  RESULT="fail"
  emit_event "core_t6751" "fail" "fail" "strict aggregate/window substrate tests failed"
fi

emit_event "finalize" "artifact" "${RESULT}" "writing structured verification report"

LOG_SHA256="$(sha256_file "${LOG_FILE}")"
EVENTS_SHA256="$(sha256_file "${EVENTS_JSONL}")"

cat > "${REPORT_JSON}" <<EOF
{
  "schema_version": "fsqlite.verify_t6_7_aggregate_window_substrate.v1",
  "bead_id": "${BEAD_ID}",
  "trace_id": "${TRACE_ID}",
  "run_id": "${RUN_ID}",
  "scenario_id": "${SCENARIO_ID}",
  "seed": ${SEED},
  "result": "${RESULT}",
  "fallback_rejection": "strict parity-cert mode enabled inside file-backed tests",
  "replay_command": "${REPLAY_COMMAND}",
  "artifacts": {
    "events_jsonl": {
      "path": "${EVENTS_JSONL}",
      "sha256": "${EVENTS_SHA256}"
    },
    "test_log": {
      "path": "${LOG_FILE}",
      "sha256": "${LOG_SHA256}"
    }
  },
  "workloads": [
    "file-backed GROUP BY aggregate storage substrate",
    "file-backed DISTINCT plus GROUP BY-prefix ORDER BY storage substrate",
    "file-backed NULL grouping order invariant",
    "file-backed empty grouped input",
    "file-backed empty implicit aggregate",
    "file-backed window stage accounting",
    "file-backed GROUP BY plus window pipeline"
  ],
  "structured_log_fields": [
    "trace_id",
    "run_id",
    "scenario_id",
    "backend_identity",
    "fallback_policy",
    "group_key_count",
    "aggregate_kind_set",
    "window_stage_count",
    "temp_store_strategy",
    "spill_bytes",
    "rows_in",
    "rows_out",
    "elapsed_ns",
    "first_failure_diag"
  ]
}
EOF

echo "result=${RESULT}"
echo "events=${EVENTS_JSONL}"
echo "log=${LOG_FILE}"
echo "report=${REPORT_JSON}"

if [[ "${RESULT}" != "pass" ]]; then
  tail -80 "${LOG_FILE}" || true
  exit 1
fi
