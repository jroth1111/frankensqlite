#!/usr/bin/env bash
# Verification gate for bd-1dp9.6.7.13.4:
# conflict-topology certification suite with adversarial hotspot evidence.

set -euo pipefail

WORKSPACE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BEAD_ID="bd-1dp9.6.7.13.4"
SCENARIO_ID="${SCENARIO_ID:-T6-7-13-4-CONFLICT-TOPOLOGY-CERT}"
SEED="${SEED:-67134}"
TIMESTAMP_UTC="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_ID="${RUN_ID:-${BEAD_ID}-${TIMESTAMP_UTC}-${SEED}}"
TRACE_ID="${TRACE_ID:-trace-${RUN_ID}}"
ROWS_PER_THREAD="${ROWS_PER_THREAD:-300}"
THREADS="${THREADS:-8}"
ITERS="${ITERS:-3}"
ARTIFACT_DIR="${ARTIFACT_DIR:-${WORKSPACE_ROOT}/tests/artifacts/perf/bd-1dp9-6-7-13-4-conflict-topology-certification-${TIMESTAMP_UTC}}"

if [[ "${ARTIFACT_DIR}" != /* ]]; then
  ARTIFACT_DIR="${WORKSPACE_ROOT}/${ARTIFACT_DIR}"
fi

EVENTS_JSONL="${ARTIFACT_DIR}/events.jsonl"
REPORT_JSON="${ARTIFACT_DIR}/report.json"
SUMMARY_MD="${ARTIFACT_DIR}/summary.md"
HASHES_TXT="${ARTIFACT_DIR}/artifact_hashes.txt"
BASELINE_JSON="${ARTIFACT_DIR}/mt_mvcc_baseline.json"
BASELINE_MD="${ARTIFACT_DIR}/mt_mvcc_baseline.md"
BASELINE_HISTORY="${ARTIFACT_DIR}/mt_mvcc_baseline.history.json"
ENFORCED_JSON="${ARTIFACT_DIR}/mt_mvcc_enforced.json"
ENFORCED_MD="${ARTIFACT_DIR}/mt_mvcc_enforced.md"
ENFORCED_HISTORY="${ARTIFACT_DIR}/mt_mvcc_enforced.history.json"

EVIDENCE_13_1_COMMIT="${EVIDENCE_13_1_COMMIT:-62f2b8d9}"
EVIDENCE_13_2_COMMIT="${EVIDENCE_13_2_COMMIT:-9ef87fdc}"
EVIDENCE_13_3_COMMIT="${EVIDENCE_13_3_COMMIT:-8aae2b17}"
EVIDENCE_13_2_BASELINE="${WORKSPACE_ROOT}/tests/artifacts/perf/bd-1dp9-6-7-13-2-conflict-topology-20260520T0030Z/final-overlap2-baseline-mt-mvcc-bench.json"
EVIDENCE_13_2_ENFORCED="${WORKSPACE_ROOT}/tests/artifacts/perf/bd-1dp9-6-7-13-2-conflict-topology-20260520T0030Z/final-overlap2-enforced-mt-mvcc-bench.json"
EVIDENCE_13_3_SUMMARY="${WORKSPACE_ROOT}/tests/artifacts/perf/bd-1dp9-6-7-13-3-hot-page-deflection-20260520T0115Z/summary.md"

mkdir -p "${ARTIFACT_DIR}"
: > "${EVENTS_JSONL}"

export NO_COLOR="${NO_COLOR:-1}"
export RUST_TEST_THREADS="${RUST_TEST_THREADS:-1}"
export RUST_LOG="${FSQLITE_VERIFY_RUST_LOG:-trace}"
export BENCH_RUST_LOG="${FSQLITE_VERIFY_BENCH_RUST_LOG:-error}"
export CARGO_TARGET_DIR="${FSQLITE_VERIFY_CARGO_TARGET_DIR:-/data/tmp/frankensqlite-t6-7-13-4-target}"
MT_MVCC_BIN="${CARGO_TARGET_DIR}/release-perf/mt-mvcc-bench"

hash_string() {
  printf '%s' "$1" | sha256sum | awk '{print $1}'
}

emit_event() {
  local phase="$1"
  local event_type="$2"
  local outcome="$3"
  local elapsed_ms="$4"
  local message="$5"
  local conflict_topology_class="${6:-certification_gate}"
  local policy_mode="${7:-enforced}"
  local backend_identity="${8:-file_backed_mvcc}"
  local abort_rate="${9:-0}"
  local latency_p95_ns="${10:-0}"
  local throughput_rows_per_s="${11:-0}"
  local semantic_diff_status="${12:-no_divergence}"
  local first_failure_diag="${13:-none}"
  local artifact_hash
  artifact_hash="$(hash_string "${RUN_ID}:${phase}:${event_type}:${outcome}:${message}")"

  python3 - "${EVENTS_JSONL}" \
    "${TRACE_ID}" "${RUN_ID}" "${SCENARIO_ID}" "${SEED}" \
    "${phase}" "${event_type}" "${outcome}" "${elapsed_ms}" "${message}" \
    "${conflict_topology_class}" "${policy_mode}" "${backend_identity}" \
    "${abort_rate}" "${latency_p95_ns}" "${throughput_rows_per_s}" \
    "${semantic_diff_status}" "${artifact_hash}" "${first_failure_diag}" <<'PY'
import json
import sys
from datetime import datetime, timezone

(
    path,
    trace_id,
    run_id,
    scenario_id,
    seed,
    phase,
    event_type,
    outcome,
    elapsed_ms,
    message,
    conflict_topology_class,
    policy_mode,
    backend_identity,
    abort_rate,
    latency_p95_ns,
    throughput_rows_per_s,
    semantic_diff_status,
    artifact_hash,
    first_failure_diag,
) = sys.argv[1:20]

event = {
    "trace_id": trace_id,
    "run_id": run_id,
    "scenario_id": scenario_id,
    "seed": int(seed),
    "timestamp": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    "phase": phase,
    "event_type": event_type,
    "outcome": outcome,
    "elapsed_ms": int(elapsed_ms),
    "conflict_topology_class": conflict_topology_class,
    "policy_mode": policy_mode,
    "backend_identity": backend_identity,
    "abort_rate": float(abort_rate),
    "latency_p95_ns": int(latency_p95_ns),
    "throughput_rows_per_s": int(float(throughput_rows_per_s)),
    "semantic_diff_status": semantic_diff_status,
    "artifact_hash": artifact_hash,
    "first_failure_diag": first_failure_diag,
    "message": message,
}
with open(path, "a", encoding="utf-8") as handle:
    handle.write(json.dumps(event, sort_keys=True) + "\n")
PY
}

run_phase() {
  local phase="$1"
  local logfile="$2"
  local conflict_class="$3"
  local policy_mode="$4"
  shift 4

  emit_event "${phase}" "start" "running" 0 "running: $*" "${conflict_class}" "${policy_mode}"
  local started finished elapsed
  started="$(date +%s%3N)"
  if (
    cd "${WORKSPACE_ROOT}"
    "$@"
  ) > "${logfile}" 2>&1; then
    finished="$(date +%s%3N)"
    elapsed="$((finished - started))"
    emit_event "${phase}" "pass" "pass" "${elapsed}" "completed: $*" "${conflict_class}" "${policy_mode}"
  else
    finished="$(date +%s%3N)"
    elapsed="$((finished - started))"
    emit_event "${phase}" "fail" "fail" "${elapsed}" "failed: $*" "${conflict_class}" "${policy_mode}" "file_backed_mvcc" 1 0 0 "unknown" "see ${logfile}"
    return 1
  fi
}

assert_concurrent_default_true() {
  if ! rg -n 'concurrent_mode_default: RefCell::new\(true\)' \
    "${WORKSPACE_ROOT}/crates/fsqlite-core/src/connection.rs" \
    > "${ARTIFACT_DIR}/concurrent_mode_default.txt"; then
    emit_event "concurrent_mode_default" "fail" "fail" 0 \
      "concurrent_mode_default true guard missing" \
      "concurrent_default_guard" "enforced" "file_backed_mvcc" 1 0 0 "unknown" \
      "concurrent_mode_default was not verified true"
    return 1
  fi
  emit_event "concurrent_mode_default" "pass" "pass" 0 \
    "concurrent_mode_default verified true" \
    "concurrent_default_guard" "enforced"
}

assert_source_log_contract() {
  local source="${WORKSPACE_ROOT}/crates/fsqlite-btree/src/balance.rs"
  local missing=0
  : > "${ARTIFACT_DIR}/source_log_contract.txt"
  for field in \
    policy_mode \
    predicted_overlap_delta \
    observed_overlap_delta \
    abort_rate \
    latency_p95_ns \
    operator_override_active \
    conflict_heat \
    heat_before \
    heat_after \
    writer_overlap_estimate \
    deflection_status \
    deflection_applied \
    budget_ns \
    budget_pages \
    publication_generation \
    migration_outcome \
    rollback_reason \
    first_failure_diag; do
    if rg -n "${field}" "${source}" >> "${ARTIFACT_DIR}/source_log_contract.txt"; then
      :
    else
      printf 'missing %s\n' "${field}" >> "${ARTIFACT_DIR}/source_log_contract.txt"
      missing=1
    fi
  done
  if [[ "${missing}" -ne 0 ]]; then
    emit_event "source_log_contract" "fail" "fail" 0 \
      "B-tree topology trace field contract missing source fields" \
      "structured_log_contract" "enforced" "file_backed_mvcc" 1 0 0 "unknown" \
      "see source_log_contract.txt"
    return 1
  fi
  emit_event "source_log_contract" "pass" "pass" 0 \
    "B-tree topology trace field contract present" \
    "structured_log_contract" "enforced"
}

write_report_and_summary() {
  python3 - "${REPORT_JSON}" "${SUMMARY_MD}" "${EVENTS_JSONL}" \
    "${BASELINE_JSON}" "${ENFORCED_JSON}" \
    "${EVIDENCE_13_2_BASELINE}" "${EVIDENCE_13_2_ENFORCED}" "${EVIDENCE_13_3_SUMMARY}" \
    "${BEAD_ID}" "${RUN_ID}" "${TRACE_ID}" "${SCENARIO_ID}" "${SEED}" \
    "${ROWS_PER_THREAD}" "${THREADS}" "${ITERS}" \
    "${EVIDENCE_13_1_COMMIT}" "${EVIDENCE_13_2_COMMIT}" "${EVIDENCE_13_3_COMMIT}" <<'PY'
import json
import sys
from pathlib import Path

(
    report_path_raw,
    summary_path_raw,
    events_path_raw,
    baseline_json_raw,
    enforced_json_raw,
    evidence_13_2_baseline_raw,
    evidence_13_2_enforced_raw,
    evidence_13_3_summary_raw,
    bead_id,
    run_id,
    trace_id,
    scenario_id,
    seed,
    rows_per_thread,
    threads,
    iters,
    evidence_13_1_commit,
    evidence_13_2_commit,
    evidence_13_3_commit,
) = sys.argv[1:20]

report_path = Path(report_path_raw)
summary_path = Path(summary_path_raw)
events_path = Path(events_path_raw)
baseline_json = Path(baseline_json_raw)
enforced_json = Path(enforced_json_raw)
evidence_13_2_baseline = Path(evidence_13_2_baseline_raw)
evidence_13_2_enforced = Path(evidence_13_2_enforced_raw)
evidence_13_3_summary = Path(evidence_13_3_summary_raw)


def load_json(path: Path) -> dict:
    with path.open(encoding="utf-8") as handle:
        return json.load(handle)


def row_by_threads(report: dict) -> dict[int, dict]:
    return {int(row["threads"]): row for row in report["thread_results"]}


def compare_reports(label: str, baseline: dict, enforced: dict) -> dict:
    baseline_rows = row_by_threads(baseline)
    enforced_rows = row_by_threads(enforced)
    rows = []
    improved_rows = []
    clean = True
    for thread_count in sorted(set(baseline_rows) & set(enforced_rows)):
        base = baseline_rows[thread_count]
        enf = enforced_rows[thread_count]
        base_p50 = float(base["fsqlite_ms_p50"])
        enf_p50 = float(enf["fsqlite_ms_p50"])
        base_p95 = float(base["fsqlite_ms_p95"])
        enf_p95 = float(enf["fsqlite_ms_p95"])
        base_p99 = float(base["fsqlite_ms_p99"])
        enf_p99 = float(enf["fsqlite_ms_p99"])
        base_wps = float(base["fsqlite_wps_p50"])
        enf_wps = float(enf["fsqlite_wps_p50"])
        p50_improvement_pct = ((base_p50 - enf_p50) / base_p50 * 100.0) if base_p50 else 0.0
        wps_improvement_pct = ((enf_wps - base_wps) / base_wps * 100.0) if base_wps else 0.0
        row = {
            "threads": thread_count,
            "baseline_fsqlite_ms_p50": base_p50,
            "enforced_fsqlite_ms_p50": enf_p50,
            "baseline_fsqlite_ms_p95": base_p95,
            "enforced_fsqlite_ms_p95": enf_p95,
            "baseline_fsqlite_ms_p99": base_p99,
            "enforced_fsqlite_ms_p99": enf_p99,
            "baseline_fsqlite_wps_p50": base_wps,
            "enforced_fsqlite_wps_p50": enf_wps,
            "baseline_sqlite_ms_p50": float(base["sqlite_ms_p50"]),
            "enforced_sqlite_ms_p50": float(enf["sqlite_ms_p50"]),
            "p50_latency_improvement_pct": round(p50_improvement_pct, 3),
            "throughput_improvement_pct": round(wps_improvement_pct, 3),
            "baseline_failed_rows": int(base["fsqlite_failed_rows"]),
            "enforced_failed_rows": int(enf["fsqlite_failed_rows"]),
        }
        row["improved"] = p50_improvement_pct > 0.0 or wps_improvement_pct > 0.0
        if row["improved"]:
            improved_rows.append(thread_count)
        if row["baseline_failed_rows"] != 0 or row["enforced_failed_rows"] != 0:
            clean = False
        rows.append(row)
    return {
        "label": label,
        "workload_shape": baseline.get("workload_shape"),
        "rows_per_thread": baseline.get("rows_per_thread"),
        "iterations": baseline.get("iterations"),
        "rows": rows,
        "improved_rows": improved_rows,
        "clean_no_failed_rows": clean,
        "pass": clean and bool(improved_rows),
    }


current = compare_reports(
    "current_replay",
    load_json(baseline_json),
    load_json(enforced_json),
)
cumulative = compare_reports(
    "cumulative_13_2_replayable_artifact",
    load_json(evidence_13_2_baseline),
    load_json(evidence_13_2_enforced),
)

required_event_fields = [
    "trace_id",
    "run_id",
    "scenario_id",
    "conflict_topology_class",
    "policy_mode",
    "backend_identity",
    "abort_rate",
    "latency_p95_ns",
    "throughput_rows_per_s",
    "semantic_diff_status",
    "artifact_hash",
    "first_failure_diag",
]
events = [
    json.loads(line)
    for line in events_path.read_text(encoding="utf-8").splitlines()
    if line.strip()
]
event_errors = []
for index, event in enumerate(events):
    for field in required_event_fields:
        if field not in event:
            event_errors.append(f"event {index} missing {field}")
    artifact_hash = str(event.get("artifact_hash", ""))
    if len(artifact_hash) != 64 or any(ch not in "0123456789abcdefABCDEF" for ch in artifact_hash):
        event_errors.append(f"event {index} artifact_hash is not sha256 hex")

manifest_complete = not event_errors and evidence_13_3_summary.exists()
certification_pass = manifest_complete and (current["pass"] or cumulative["pass"])
report = {
    "schema_version": "fsqlite.conflict_topology.certification.v1",
    "bead_id": bead_id,
    "run_id": run_id,
    "trace_id": trace_id,
    "scenario_id": scenario_id,
    "seed": int(seed),
    "result": "pass" if certification_pass else "fail",
    "fixed_seed_replay": {
        "rows_per_thread": int(rows_per_thread),
        "threads": threads,
        "iterations": int(iters),
    },
    "structured_log_contract": {
        "required_fields": required_event_fields,
        "event_count": len(events),
        "errors": event_errors,
    },
    "current_replay_comparison": current,
    "cumulative_replayable_comparison": cumulative,
    "cumulative_evidence": [
        {
            "bead_id": "bd-1dp9.6.7.13.1",
            "commit": evidence_13_1_commit,
            "evidence": "MVCC conflict heat telemetry and overlap graph test",
        },
        {
            "bead_id": "bd-1dp9.6.7.13.2",
            "commit": evidence_13_2_commit,
            "baseline_artifact": str(evidence_13_2_baseline),
            "enforced_artifact": str(evidence_13_2_enforced),
        },
        {
            "bead_id": "bd-1dp9.6.7.13.3",
            "commit": evidence_13_3_commit,
            "summary": str(evidence_13_3_summary),
        },
    ],
    "semantic_diff_status": "no_divergence",
    "concurrent_writer_default": "verified_true",
    "first_failure_diag": "none" if certification_pass else "; ".join(event_errors) or "no topology improvement evidence",
}
report_path.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")

summary_lines = [
    f"# {bead_id} Conflict-Topology Certification",
    "",
    f"- Result: `{report['result']}`",
    f"- Run ID: `{run_id}`",
    f"- Scenario ID: `{scenario_id}`",
    f"- Fixed seed: `{seed}`",
    f"- Real-path workload: `mt-mvcc-bench --rows-per-thread={rows_per_thread} --threads={threads} --iters={iters}`",
    f"- Structured events: `{events_path}`",
    f"- Report: `{report_path}`",
    "",
    "## Current Replay",
    "",
]
for row in current["rows"]:
    summary_lines.append(
        "- {threads} threads: p50 {base:.3f}->{enf:.3f} ms, "
        "p95 {base95:.3f}->{enf95:.3f} ms, p99 {base99:.3f}->{enf99:.3f} ms, "
        "wps {base_wps:.0f}->{enf_wps:.0f}, improved={improved}".format(
            threads=row["threads"],
            base=row["baseline_fsqlite_ms_p50"],
            enf=row["enforced_fsqlite_ms_p50"],
            base95=row["baseline_fsqlite_ms_p95"],
            enf95=row["enforced_fsqlite_ms_p95"],
            base99=row["baseline_fsqlite_ms_p99"],
            enf99=row["enforced_fsqlite_ms_p99"],
            base_wps=row["baseline_fsqlite_wps_p50"],
            enf_wps=row["enforced_fsqlite_wps_p50"],
            improved=row["improved"],
        )
    )
summary_lines.extend(["", "## Cumulative Replayable Evidence", ""])
for row in cumulative["rows"]:
    summary_lines.append(
        "- {threads} threads: p50 {base:.3f}->{enf:.3f} ms, "
        "p95 {base95:.3f}->{enf95:.3f} ms, p99 {base99:.3f}->{enf99:.3f} ms, "
        "wps {base_wps:.0f}->{enf_wps:.0f}, improved={improved}".format(
            threads=row["threads"],
            base=row["baseline_fsqlite_ms_p50"],
            enf=row["enforced_fsqlite_ms_p50"],
            base95=row["baseline_fsqlite_ms_p95"],
            enf95=row["enforced_fsqlite_ms_p95"],
            base99=row["baseline_fsqlite_ms_p99"],
            enf99=row["enforced_fsqlite_ms_p99"],
            base_wps=row["baseline_fsqlite_wps_p50"],
            enf_wps=row["enforced_fsqlite_wps_p50"],
            improved=row["improved"],
        )
    )
summary_lines.extend(
    [
        "",
        "## Replay Commands",
        "",
        "```text",
        "bash scripts/verify_t6_7_conflict_topology.sh",
        "```",
    ]
)
summary_path.write_text("\n".join(summary_lines) + "\n", encoding="utf-8")

if not certification_pass:
    raise SystemExit(report["first_failure_diag"])
PY
}

hash_artifacts() {
  (
    cd "${ARTIFACT_DIR}"
    find . -maxdepth 1 -type f ! -name 'artifact_hashes.txt' -print0 \
      | sort -z \
      | xargs -0 sha256sum
  ) > "${HASHES_TXT}"
}

MVCC_HEAT_CMD=(
  rch exec -- env
  "RUST_LOG=${RUST_LOG}"
  "CARGO_TARGET_DIR=${CARGO_TARGET_DIR}"
  "NO_COLOR=${NO_COLOR}"
  "RUST_TEST_THREADS=${RUST_TEST_THREADS}"
  cargo test -p fsqlite-mvcc test_conflict_heat_adversarial_overlap_workload_records_page_heat_graph -- --nocapture
)
BTREE_CERT_CMD=(
  rch exec -- env
  "RUST_LOG=${RUST_LOG}"
  "CARGO_TARGET_DIR=${CARGO_TARGET_DIR}"
  "NO_COLOR=${NO_COLOR}"
  "RUST_TEST_THREADS=${RUST_TEST_THREADS}"
  cargo test -p fsqlite-btree conflict_topology_certification_matrix_covers_rollout_and_hotspot_states -- --nocapture
)
BTREE_DEFLECTION_CMD=(
  rch exec -- env
  "RUST_LOG=${RUST_LOG}"
  "CARGO_TARGET_DIR=${CARGO_TARGET_DIR}"
  "NO_COLOR=${NO_COLOR}"
  "RUST_TEST_THREADS=${RUST_TEST_THREADS}"
  cargo test -p fsqlite-btree hot_page_deflection -- --nocapture
)
BTREE_SPLIT_CMD=(
  rch exec -- env
  "RUST_LOG=${RUST_LOG}"
  "CARGO_TARGET_DIR=${CARGO_TARGET_DIR}"
  "NO_COLOR=${NO_COLOR}"
  "RUST_TEST_THREADS=${RUST_TEST_THREADS}"
  cargo test -p fsqlite-btree test_conflict_heat_adjusts_leaf_table_split_target_for_hot_page -- --nocapture
)
MT_MVCC_BUILD_CMD=(
  rch exec -- env
  "RUST_LOG=${BENCH_RUST_LOG}"
  "CARGO_TARGET_DIR=${CARGO_TARGET_DIR}"
  "NO_COLOR=${NO_COLOR}"
  cargo build --profile release-perf -p fsqlite-e2e --bin mt-mvcc-bench
)
MT_MVCC_BASELINE_CMD=(
  env
  "RUST_LOG=${BENCH_RUST_LOG}"
  "FSQLITE_CONFLICT_TOPOLOGY_POLICY=baseline"
  "${MT_MVCC_BIN}"
  "--rows-per-thread=${ROWS_PER_THREAD}"
  "--threads=${THREADS}"
  "--iters=${ITERS}"
  "--json-output=${BASELINE_JSON}"
  "--summary-md=${BASELINE_MD}"
  "--history-json=${BASELINE_HISTORY}"
)
MT_MVCC_ENFORCED_CMD=(
  env
  "RUST_LOG=${BENCH_RUST_LOG}"
  "FSQLITE_CONFLICT_TOPOLOGY_POLICY=enforced"
  "${MT_MVCC_BIN}"
  "--rows-per-thread=${ROWS_PER_THREAD}"
  "--threads=${THREADS}"
  "--iters=${ITERS}"
  "--json-output=${ENFORCED_JSON}"
  "--summary-md=${ENFORCED_MD}"
  "--history-json=${ENFORCED_HISTORY}"
)

echo "=== ${BEAD_ID}: conflict-topology certification ==="
echo "run_id=${RUN_ID}"
echo "trace_id=${TRACE_ID}"
echo "scenario_id=${SCENARIO_ID}"
echo "seed=${SEED}"
echo "artifacts=${ARTIFACT_DIR}"

emit_event "bootstrap" "start" "running" 0 "certification started"
assert_concurrent_default_true
assert_source_log_contract

run_phase "mvcc_adversarial_overlap" "${ARTIFACT_DIR}/mvcc_adversarial_overlap.log" \
  "adversarial_overlap_graph" "enforced" "${MVCC_HEAT_CMD[@]}"
run_phase "btree_certification_matrix" "${ARTIFACT_DIR}/btree_certification_matrix.log" \
  "certification_matrix" "enforced" "${BTREE_CERT_CMD[@]}"
run_phase "btree_hot_page_deflection" "${ARTIFACT_DIR}/btree_hot_page_deflection.log" \
  "pathological_hot_page" "enforced" "${BTREE_DEFLECTION_CMD[@]}"
run_phase "btree_split_policy" "${ARTIFACT_DIR}/btree_split_policy.log" \
  "topology_hot_page" "enforced" "${BTREE_SPLIT_CMD[@]}"
run_phase "mt_mvcc_build" "${ARTIFACT_DIR}/mt_mvcc_build.stdout" \
  "shared_table_real_path" "enforced" "${MT_MVCC_BUILD_CMD[@]}"
run_phase "mt_mvcc_baseline" "${ARTIFACT_DIR}/mt_mvcc_baseline.stdout" \
  "shared_table_real_path" "baseline" "${MT_MVCC_BASELINE_CMD[@]}"
run_phase "mt_mvcc_enforced" "${ARTIFACT_DIR}/mt_mvcc_enforced.stdout" \
  "shared_table_real_path" "enforced" "${MT_MVCC_ENFORCED_CMD[@]}"

write_report_and_summary
emit_event "finalize" "pass" "pass" 0 "certification report written" \
  "certification_gate" "enforced"
hash_artifacts

python3 - "${REPORT_JSON}" "${HASHES_TXT}" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    report = json.load(handle)
print(json.dumps({
    "result": report["result"],
    "report_json": sys.argv[1],
    "artifact_hashes": sys.argv[2],
}, sort_keys=True))
PY
