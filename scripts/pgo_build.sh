#!/usr/bin/env bash
# Build and measure `comprehensive-bench` with Profile-Guided Optimization.
#
# Pipeline:
#   1. Build a release-perf baseline with the `pgo` cargo feature enabled.
#   2. Build an instrumented binary with `-Cprofile-generate=<dir>`.
#   3. Run `comprehensive-bench --quick --no-html` to collect profiles.
#   4. Merge `.profraw` files with `llvm-profdata merge`.
#   5. Rebuild release-perf with `-Cprofile-use=<merged.profdata>`.
#   6. Measure baseline vs PGO using `hyperfine --export-json`.
#   7. Optional BOLT post-link optimization if `llvm-bolt` is available.
#
# Outputs into a unique run directory under `/tmp/frankensqlite-pgo/` by
# default, and mirrors small proof artifacts into $PGO_LOCAL_ARTIFACT_DIR.
# Override with $PGO_DIR and $PGO_RUN_ID.
#
# Usage:
#   scripts/pgo_build.sh                   # full PGO + hyperfine pipeline
#   scripts/pgo_build.sh --skip-training   # reuse $PGO_PROFDATA
#   scripts/pgo_build.sh --bolt            # also run BOLT after PGO
#   scripts/pgo_build.sh --no-measure      # skip hyperfine
#   scripts/pgo_build.sh --no-rch          # use local cargo instead of rch
#
# Environment:
#   CARGO_TARGET_DIR=/tmp/rch_target_fsqlite_cod5
#   PGO_DIR=/tmp/frankensqlite-pgo
#   PGO_RUN_ID=manual-001
#   PGO_PROFDATA=/tmp/frankensqlite-pgo/manual-001/merged.profdata
#   PGO_LOCAL_ARTIFACT_DIR=reports/pgo-artifacts/manual-001
#   PGO_FEATURE=pgo
#   TRAINING_ARGS="--quick --no-html"
#   BENCH_ARGS="--quick --no-html"
#   HYPERFINE_RUNS=5
#   HYPERFINE_WARMUP=1

set -euo pipefail
shopt -s nullglob

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${WORKSPACE_ROOT}"

PGO_DIR="${PGO_DIR:-/tmp/frankensqlite-pgo}"
PGO_RUN_ID="${PGO_RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)-$$}"
RUN_DIR="${PGO_DIR}/${PGO_RUN_ID}"
PROFRAW_DIR="${RUN_DIR}/profraw"
PROFDATA="${PGO_PROFDATA:-${RUN_DIR}/merged.profdata}"
LOCAL_ARTIFACT_DIR="${PGO_LOCAL_ARTIFACT_DIR:-${WORKSPACE_ROOT}/reports/pgo-artifacts/${PGO_RUN_ID}}"
REQUESTED_CARGO_TARGET_BASE="${CARGO_TARGET_DIR:-${TMPDIR:-/tmp}/rch_target_fsqlite_pgo}"
PGO_FEATURE="${PGO_FEATURE:-pgo}"
TRAINING_ARGS="${TRAINING_ARGS:---quick --no-html}"
BENCH_ARGS="${BENCH_ARGS:---quick --no-html}"
HYPERFINE_RUNS="${HYPERFINE_RUNS:-5}"
HYPERFINE_WARMUP="${HYPERFINE_WARMUP:-1}"
HYPERFINE_JSON="${HYPERFINE_JSON:-${RUN_DIR}/hyperfine-pgo.json}"

SKIP_TRAINING=0
RUN_BOLT=0
RUN_MEASURE=1
USE_RCH=1
for arg in "$@"; do
    case "$arg" in
        --skip-training) SKIP_TRAINING=1 ;;
        --bolt) RUN_BOLT=1 ;;
        --no-measure) RUN_MEASURE=0 ;;
        --measure) RUN_MEASURE=1 ;;
        --no-rch) USE_RCH=0 ;;
        -h|--help)
            sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "unknown arg: $arg" >&2
            exit 2
            ;;
    esac
done

if [[ "${SKIP_TRAINING}" -eq 0 && -e "${RUN_DIR}" ]]; then
    echo "error: run directory already exists: ${RUN_DIR}" >&2
    echo "set a fresh PGO_RUN_ID or PGO_DIR; this script will not overwrite prior artifacts" >&2
    exit 1
fi

# Locate llvm-profdata that matches this rustc's LLVM version.
RUSTC_SYSROOT="$(rustc --print sysroot)"
LLVM_PROFDATA="${RUSTC_SYSROOT}/lib/rustlib/x86_64-unknown-linux-gnu/bin/llvm-profdata"
if [[ ! -x "${LLVM_PROFDATA}" ]]; then
    LLVM_PROFDATA="$(command -v llvm-profdata || true)"
fi
if [[ -z "${LLVM_PROFDATA}" ]]; then
    echo "error: llvm-profdata not found. Install with: rustup component add llvm-tools-preview" >&2
    exit 1
fi

CARGO_TARGET_BASE="${REQUESTED_CARGO_TARGET_BASE}"
if [[ "${USE_RCH}" -eq 1 && "${REQUESTED_CARGO_TARGET_BASE}" != "${WORKSPACE_ROOT}"/* ]]; then
    CARGO_TARGET_BASE="${LOCAL_ARTIFACT_DIR}/targets"
fi
BASELINE_TARGET="${CARGO_TARGET_BASE}/baseline"
GENERATE_TARGET="${CARGO_TARGET_BASE}/pgo-generate"
PGO_TARGET="${CARGO_TARGET_BASE}/pgo-use"

CARGO_FEATURE_ARGS=()
if [[ -n "${PGO_FEATURE}" ]]; then
    CARGO_FEATURE_ARGS=(--features "${PGO_FEATURE}")
fi

read -r -a TRAINING_ARGV <<<"${TRAINING_ARGS}"
read -r -a BENCH_ARGV <<<"${BENCH_ARGS}"

run_cargo() {
    local target_dir="$1"
    local rustflags="$2"
    shift 2

    if [[ "${USE_RCH}" -eq 1 && -x "${HOME}/.local/bin/rch" ]]; then
        "${HOME}/.local/bin/rch" exec -- env \
            CARGO_TARGET_DIR="${target_dir}" \
            RUSTFLAGS="${rustflags}" \
            cargo "$@"
    elif [[ "${USE_RCH}" -eq 1 && -x "$(command -v rch || true)" ]]; then
        rch exec -- env \
            CARGO_TARGET_DIR="${target_dir}" \
            RUSTFLAGS="${rustflags}" \
            cargo "$@"
    else
        env CARGO_TARGET_DIR="${target_dir}" RUSTFLAGS="${rustflags}" cargo "$@"
    fi
}

quote_cmd() {
    printf "%q " "$@"
}

copy_artifact_if_distinct() {
    local src="$1"
    local dst="$2"

    if [[ ! -f "${src}" ]]; then
        return 0
    fi
    if [[ "$(realpath -m "${src}")" == "$(realpath -m "${dst}")" ]]; then
        return 0
    fi
    cp "${src}" "${dst}"
}

publish_local_artifacts() {
    mkdir -p "${LOCAL_ARTIFACT_DIR}"
    {
        echo "run_dir=${RUN_DIR}"
        echo "profdata=${PROFDATA}"
        echo "baseline_bin=${BASELINE_BIN}"
        echo "pgo_bin=${PGO_BIN}"
        echo "training_args=${TRAINING_ARGS}"
        echo "bench_args=${BENCH_ARGS}"
        echo "hyperfine_json=${HYPERFINE_JSON}"
    } > "${LOCAL_ARTIFACT_DIR}/manifest.txt"

    copy_artifact_if_distinct \
        "${RUN_DIR}/training_run.log" \
        "${LOCAL_ARTIFACT_DIR}/training_run.log"
    copy_artifact_if_distinct \
        "${HYPERFINE_JSON}" \
        "${LOCAL_ARTIFACT_DIR}/hyperfine-pgo.json"
}

mkdir -p "${RUN_DIR}" "${PROFRAW_DIR}"

BASELINE_BIN="${BASELINE_TARGET}/release-perf/comprehensive-bench"
GENERATE_BIN="${GENERATE_TARGET}/release-perf/comprehensive-bench"
PGO_BIN="${PGO_TARGET}/release-perf/comprehensive-bench"

echo "run_dir=${RUN_DIR}"
echo "cargo_feature=${PGO_FEATURE:-<none>}"
echo "baseline_target=${BASELINE_TARGET}"
echo "generate_target=${GENERATE_TARGET}"
echo "pgo_target=${PGO_TARGET}"
echo "local_artifact_dir=${LOCAL_ARTIFACT_DIR}"

echo "[0/5] Baseline build (release-perf, cargo feature gate)..."
run_cargo "${BASELINE_TARGET}" "" \
    build --profile release-perf -p fsqlite-e2e --bin comprehensive-bench \
    "${CARGO_FEATURE_ARGS[@]}"

if [[ "${SKIP_TRAINING}" -eq 0 ]]; then
    echo "[1/5] Instrumented build (-Cprofile-generate)..."
    run_cargo "${GENERATE_TARGET}" "-Cprofile-generate=${PROFRAW_DIR}" \
        build --profile release-perf -p fsqlite-e2e --bin comprehensive-bench \
        "${CARGO_FEATURE_ARGS[@]}"

    echo "[2/5] Training run (comprehensive-bench ${TRAINING_ARGS})..."
    LLVM_PROFILE_FILE="${PROFRAW_DIR}/default_%m_%p.profraw" \
        "${GENERATE_BIN}" "${TRAINING_ARGV[@]}" > "${RUN_DIR}/training_run.log" 2>&1
    echo "      training profiles:"
    find "${PROFRAW_DIR}" -maxdepth 1 -name '*.profraw' -type f -printf '%s %p\n' \
        | sort -n | tail -5

    profraw_files=("${PROFRAW_DIR}"/*.profraw)
    if [[ "${#profraw_files[@]}" -eq 0 ]]; then
        echo "error: training generated no .profraw files; see ${RUN_DIR}/training_run.log" >&2
        exit 1
    fi

    echo "[3/5] Merging profdata..."
    "${LLVM_PROFDATA}" merge -output="${PROFDATA}" "${profraw_files[@]}"
    echo "      merged: ${PROFDATA} ($(stat -c '%s' "${PROFDATA}") bytes)"
else
    if [[ ! -f "${PROFDATA}" ]]; then
        echo "error: --skip-training given but ${PROFDATA} does not exist" >&2
        exit 1
    fi
    echo "[1-3/5] Skipped (reusing ${PROFDATA})"
fi

echo "[4/5] PGO-optimized build (-Cprofile-use)..."
run_cargo "${PGO_TARGET}" "-Cprofile-use=${PROFDATA}" \
    build --profile release-perf -p fsqlite-e2e --bin comprehensive-bench \
    "${CARGO_FEATURE_ARGS[@]}"

if [[ "${RUN_MEASURE}" -eq 1 ]]; then
    if ! command -v hyperfine >/dev/null 2>&1; then
        echo "error: hyperfine not found; install it or pass --no-measure" >&2
        exit 1
    fi
    echo "[5/5] Measuring baseline vs PGO with hyperfine..."
    BASELINE_CMD="$(quote_cmd "${BASELINE_BIN}" "${BENCH_ARGV[@]}")"
    PGO_CMD="$(quote_cmd "${PGO_BIN}" "${BENCH_ARGV[@]}")"
    hyperfine \
        --warmup "${HYPERFINE_WARMUP}" \
        --runs "${HYPERFINE_RUNS}" \
        --export-json "${HYPERFINE_JSON}" \
        --command-name "baseline" "${BASELINE_CMD}" \
        --command-name "pgo" "${PGO_CMD}"

    if command -v jq >/dev/null 2>&1; then
        jq -r --arg hyperfine_json "${HYPERFINE_JSON}" '
          .results as $r
          | ($r[] | select(.command == "baseline") | .mean) as $baseline
          | ($r[] | select(.command == "pgo") | .mean) as $pgo
          | "hyperfine_json=\($hyperfine_json)\nbaseline_mean_s=\($baseline)\npgo_mean_s=\($pgo)\nspeedup=\($baseline / $pgo)"' \
          "${HYPERFINE_JSON}"
    else
        echo "hyperfine_json=${HYPERFINE_JSON}"
    fi
else
    echo "[5/5] Measurement skipped (--no-measure)"
fi

if [[ "${RUN_BOLT}" -eq 1 ]]; then
    if ! command -v llvm-bolt >/dev/null 2>&1; then
        echo "warning: llvm-bolt not found on PATH; skipping BOLT stage" >&2
    else
        echo "[BOLT] Post-link optimization..."
        BOLT_PROFILE="${RUN_DIR}/bolt.fdata"
        BOLT_BIN="${PGO_BIN}.bolt"
        if perf record -e cycles:u -j any,u -o /dev/null -- /bin/true 2>/dev/null; then
            echo "      using LBR branch sampling"
            perf record -e cycles:u -j any,u -o "${RUN_DIR}/perf.data" -- \
                "${PGO_BIN}" "${BENCH_ARGV[@]}" > "${RUN_DIR}/bolt_training.log" 2>&1
            PERF2BOLT_FLAGS=()
        else
            echo "      LBR unavailable; falling back to basic sampling (no-LBR mode)"
            perf record -o "${RUN_DIR}/perf.data" -- \
                "${PGO_BIN}" "${BENCH_ARGV[@]}" > "${RUN_DIR}/bolt_training.log" 2>&1
            PERF2BOLT_FLAGS=(-nl)
        fi
        perf2bolt "${PERF2BOLT_FLAGS[@]}" -p "${RUN_DIR}/perf.data" -o "${BOLT_PROFILE}" "${PGO_BIN}"
        llvm-bolt "${PGO_BIN}" -o "${BOLT_BIN}" -data="${BOLT_PROFILE}" \
            -reorder-blocks=ext-tsp -reorder-functions=hfsort+ -split-functions -icf=1
        echo "      BOLT output: ${BOLT_BIN}"
    fi
fi

publish_local_artifacts

echo
echo "Done."
echo "Baseline binary: ${BASELINE_BIN}"
echo "PGO binary: ${PGO_BIN}"
echo "Artifacts: ${RUN_DIR}"
echo "Local proof artifacts: ${LOCAL_ARTIFACT_DIR}"
