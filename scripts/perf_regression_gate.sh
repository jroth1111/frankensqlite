#!/usr/bin/env bash
set -euo pipefail

# bd-zywqc.2: Concurrent-write performance regression gate.
#
# Runs mt-mvcc-bench at 1-thread and 8-thread, captures JSON baselines,
# and compares against the stored baseline. Fails CI if:
#   - Single-writer throughput regresses >5% (mean)
#   - 8-writer throughput regresses at all (median)
#
# Usage:
#   ./scripts/perf_regression_gate.sh [--capture-baseline] [--target-dir DIR]
#                                     [--baseline-dir DIR] [--rows N]
#
# Modes:
#   Default:           compare current run against stored baseline
#   --capture-baseline: store current run as the new baseline (no comparison)
#
# Artifacts:
#   $TARGET_DIR/regression_gate_current.json  — current run results
#   $TARGET_DIR/regression_gate_result.json   — gate verdict
#   $BASELINE_DIR/latest.json                 — stored baseline

BEAD_ID="bd-zywqc.2"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

TARGET_DIR="${CARGO_TARGET_DIR:-/tmp/fsqlite-reggate-target}"
BASELINE_DIR="$REPO_ROOT/tests/perf/baselines"
CAPTURE_BASELINE=false
ROWS_PER_THREAD=500

while [[ $# -gt 0 ]]; do
    case "$1" in
        --capture-baseline) CAPTURE_BASELINE=true; shift ;;
        --target-dir) TARGET_DIR="$2"; shift 2 ;;
        --baseline-dir) BASELINE_DIR="$2"; shift 2 ;;
        --rows) ROWS_PER_THREAD="$2"; shift 2 ;;
        *) echo "Unknown option: $1" >&2; exit 2 ;;
    esac
done

mkdir -p "$TARGET_DIR" "$BASELINE_DIR"

CURRENT_JSON="$TARGET_DIR/regression_gate_current.json"
RESULT_JSON="$TARGET_DIR/regression_gate_result.json"
BASELINE_JSON="$BASELINE_DIR/latest.json"
COMMIT_HASH="$(git -C "$REPO_ROOT" rev-parse --short HEAD 2>/dev/null || echo 'unknown')"

echo "[$BEAD_ID] Performance regression gate"
echo "[$BEAD_ID] Commit: $COMMIT_HASH"
echo "[$BEAD_ID] Rows/thread: $ROWS_PER_THREAD"
echo "[$BEAD_ID] Target: $TARGET_DIR"

# ── Step 1: Run the benchmark ────────────────────────────────────────

echo "[$BEAD_ID] Running mt-mvcc-bench --threads=1,8 --rows-per-thread=$ROWS_PER_THREAD ..."

if ! env CARGO_TARGET_DIR="$TARGET_DIR" cargo run -p fsqlite-e2e \
    --bin mt-mvcc-bench --release -- \
    --threads=1,8 \
    --rows-per-thread="$ROWS_PER_THREAD" \
    --iters=3 \
    --json-output="$CURRENT_JSON" 2>&1 | tee "$TARGET_DIR/regression_gate_bench.log"; then
    echo "[$BEAD_ID] WARNING: mt-mvcc-bench exited non-zero (may be pass-over-pass gate)"
fi

if [[ ! -f "$CURRENT_JSON" ]]; then
    echo "[$BEAD_ID] FATAL: no JSON output at $CURRENT_JSON"
    exit 2
fi

echo "[$BEAD_ID] Benchmark complete. Analyzing results..."

# ── Step 2: Compare or capture ───────────────────────────────────────

if [[ "$CAPTURE_BASELINE" = true ]]; then
    cp "$CURRENT_JSON" "$BASELINE_JSON"
    echo "[$BEAD_ID] Baseline captured: $BASELINE_JSON"
    cat > "$RESULT_JSON" <<EOF
{
  "bead_id": "$BEAD_ID",
  "mode": "capture_baseline",
  "commit": "$COMMIT_HASH",
  "baseline_path": "$BASELINE_JSON",
  "verdict": "captured"
}
EOF
    echo "[$BEAD_ID] CAPTURED — run without --capture-baseline to gate"
    exit 0
fi

if [[ ! -f "$BASELINE_JSON" ]]; then
    echo "[$BEAD_ID] No baseline at $BASELINE_JSON — capturing initial baseline"
    cp "$CURRENT_JSON" "$BASELINE_JSON"
    cat > "$RESULT_JSON" <<EOF
{
  "bead_id": "$BEAD_ID",
  "mode": "initial_baseline",
  "commit": "$COMMIT_HASH",
  "baseline_path": "$BASELINE_JSON",
  "verdict": "initial_capture"
}
EOF
    echo "[$BEAD_ID] INITIAL BASELINE — first run establishes the reference point"
    exit 0
fi

# ── Step 3: Delta analysis ───────────────────────────────────────────

python3 - "$CURRENT_JSON" "$BASELINE_JSON" "$RESULT_JSON" "$COMMIT_HASH" <<'PYEOF'
import json, sys

current_path = sys.argv[1]
baseline_path = sys.argv[2]
result_path = sys.argv[3]
commit = sys.argv[4]

SINGLE_WRITER_THRESHOLD = 0.05   # 5% regression gate
EIGHT_WRITER_THRESHOLD  = 0.00   # any regression fails

with open(current_path) as f:
    current = json.load(f)
with open(baseline_path) as f:
    baseline = json.load(f)

def extract_wps(report, threads):
    """Extract writes-per-second for a given thread count."""
    results = report.get("thread_results", [])
    for entry in results:
        if entry.get("threads") == threads:
            # Try fsqlite_wps first, then fall back to aggregated fields
            wps = entry.get("fsqlite_wps")
            if wps is None:
                stats = entry.get("fsqlite_stats", {})
                wps = stats.get("writes_per_sec", 0)
            return wps or 0
    return 0

verdict = "passed"
regressions = []
deltas = []

for threads in [1, 8]:
    curr_wps = extract_wps(current, threads)
    base_wps = extract_wps(baseline, threads)

    if base_wps == 0:
        deltas.append({
            "threads": threads,
            "current_wps": curr_wps,
            "baseline_wps": base_wps,
            "delta_pct": 0.0,
            "status": "no_baseline"
        })
        continue

    delta_pct = (curr_wps - base_wps) / base_wps
    threshold = SINGLE_WRITER_THRESHOLD if threads == 1 else EIGHT_WRITER_THRESHOLD

    status = "ok"
    if delta_pct < -threshold:
        status = "regression"
        verdict = "failed"
        regressions.append({
            "threads": threads,
            "current_wps": round(curr_wps, 1),
            "baseline_wps": round(base_wps, 1),
            "delta_pct": round(delta_pct * 100, 2),
            "threshold_pct": round(-threshold * 100, 2),
        })

    deltas.append({
        "threads": threads,
        "current_wps": round(curr_wps, 1),
        "baseline_wps": round(base_wps, 1),
        "delta_pct": round(delta_pct * 100, 2),
        "threshold_pct": round(-threshold * 100, 2),
        "status": status,
    })

result = {
    "bead_id": "bd-zywqc.2",
    "mode": "regression_gate",
    "commit": commit,
    "verdict": verdict,
    "single_writer_threshold_pct": SINGLE_WRITER_THRESHOLD * 100,
    "eight_writer_threshold_pct": EIGHT_WRITER_THRESHOLD * 100,
    "deltas": deltas,
    "regressions": regressions,
}

with open(result_path, "w") as f:
    json.dump(result, f, indent=2)

for d in deltas:
    tag = "REGRESSION" if d["status"] == "regression" else "ok"
    print(f'  [{tag}] {d["threads"]}t: {d["current_wps"]} wps (baseline {d["baseline_wps"]}, delta {d["delta_pct"]:+.1f}%)')

if verdict == "failed":
    print(f"\n[bd-zywqc.2] FAILED — {len(regressions)} regression(s) detected")
    sys.exit(1)
else:
    print(f"\n[bd-zywqc.2] PASSED — no regressions")
    sys.exit(0)
PYEOF
