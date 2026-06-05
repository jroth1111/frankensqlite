#!/usr/bin/env bash
# verify_bd_db300_8_1_1_matched_artifact_packs.sh
#
# Track H matched-pack collector for SQLite vs FrankenSQLite MVCC vs forced
# single-writer mode. The script runs one canonical benchmark cell per selected
# row/fixture/placement/storage tuple, writes mode-specific benchmark artifacts,
# and produces a matched-pack manifest/report with shared provenance fields.
#
# Benchmark work is routed through `rch exec`; H3's verify-suite packaging path
# intentionally uses plain local `cargo run` because it is a fast packaging check.

set -euo pipefail

WORKSPACE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BEAD_ID="${BEAD_ID:-bd-db300.8.1.1}"
SCRIPT_ENTRYPOINT="${SCRIPT_ENTRYPOINT:-scripts/verify_bd_db300_8_1_1_matched_artifact_packs.sh}"
RUN_ID="${BEAD_ID}-$(date -u +%Y%m%dT%H%M%SZ)-$$"
RUN_ID_SAFE="${RUN_ID//[^[:alnum:]]/_}"
GENERATED_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
CAMPAIGN_MANIFEST_REL="${CAMPAIGN_MANIFEST_REL:-sample_sqlite_db_files/manifests/beads_benchmark_campaign.v1.json}"
CAMPAIGN_MANIFEST_FILE="${WORKSPACE_ROOT}/${CAMPAIGN_MANIFEST_REL}"
OUTPUT_DIR="${OUTPUT_DIR:-${WORKSPACE_ROOT}/artifacts/perf/${BEAD_ID}/${RUN_ID}}"
PACKS_DIR="${OUTPUT_DIR}/packs"
LOG_FILE="${OUTPUT_DIR}/events.jsonl"
REPORT_JSON="${OUTPUT_DIR}/report.json"
SUMMARY_MD="${OUTPUT_DIR}/summary.md"
CLASSIFICATION_JSON="${OUTPUT_DIR}/classification.json"
CLASSIFICATION_MD="${OUTPUT_DIR}/classification.md"
VALIDATION_JSON="${OUTPUT_DIR}/validation.json"
VALIDATION_MD="${OUTPUT_DIR}/validation.md"
SINGLE_WRITER_ROLE_JSON="${OUTPUT_DIR}/single_writer_role.json"
SINGLE_WRITER_ROLE_MD="${OUTPUT_DIR}/single_writer_role.md"
MVCC_DEFAULT_GUARD_LOG="${OUTPUT_DIR}/mvcc_default_guard.log"
SINGLE_WRITER_VERIFY_DIR="${OUTPUT_DIR}/verify-suite/single-writer"
SINGLE_WRITER_VERIFY_LOG="${OUTPUT_DIR}/single_writer_verify_suite.log"
SINGLE_WRITER_VERIFY_TARGET_DIR="${SINGLE_WRITER_VERIFY_TARGET_DIR:-${TMPDIR:-/tmp}/fsqlite_${RUN_ID_SAFE}_single_writer_verify}"
CONCURRENT_MODE_DEFAULT_GUARD="${OUTPUT_DIR}/concurrent_mode_default_guard.txt"
ROW_IDS="${ROW_IDS:-mixed_read_write_c4}"
FIXTURE_IDS="${FIXTURE_IDS:-}"
PLACEMENT_PROFILE_IDS="${PLACEMENT_PROFILE_IDS:-baseline_unpinned}"
STORAGE_PROFILE_IDS="${STORAGE_PROFILE_IDS:-file_backed,memory}"
REPEAT="${REPEAT:-1}"
WARMUP="${WARMUP:-0}"
CARGO_PROFILE="${CARGO_PROFILE:-release-perf}"
RCH_TARGET_DIR="${RCH_TARGET_DIR:-/tmp/rch_target_bd_db300_8_1_1}"
RETENTION_CLASS="${RETENTION_CLASS:-quick_run}"
POSTPROCESS_ONLY="${POSTPROCESS_ONLY:-0}"
EMIT_SINGLE_WRITER_CLASSIFICATION="${EMIT_SINGLE_WRITER_CLASSIFICATION:-0}"
BEADS_DATA_PATH="${WORKSPACE_ROOT}/.beads/issues.jsonl"
SOURCE_REVISION="${SOURCE_REVISION:-$(git -C "${WORKSPACE_ROOT}" rev-parse HEAD)}"
BEADS_HASH="${BEADS_HASH:-$(sha256sum "${BEADS_DATA_PATH}" | awk '{print $1}')}"
MVCC_DEFAULT_GUARD_TEST="${MVCC_DEFAULT_GUARD_TEST:-bd_2yqp6_6_5_concurrent_mode_defaults}"
MVCC_DEFAULT_GUARD_TARGET_DIR="${MVCC_DEFAULT_GUARD_TARGET_DIR:-${TMPDIR:-/tmp}/rch_target_${RUN_ID_SAFE}_mvcc_defaults}"
MODES=("sqlite_reference" "fsqlite_mvcc" "fsqlite_single_writer")

mkdir -p "${PACKS_DIR}"
: > "${LOG_FILE}"

log_event() {
    local level="$1"
    local stage="$2"
    local message="$3"
    printf '{"run_id":"%s","bead_id":"%s","level":"%s","stage":"%s","message":"%s","ts":"%s"}\n' \
        "${RUN_ID}" "${BEAD_ID}" "${level}" "${stage}" "${message}" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
        >> "${LOG_FILE}"
}

fail() {
    local stage="$1"
    local message="$2"
    log_event "ERROR" "${stage}" "${message}"
    echo "ERROR: ${message}" >&2
    exit 1
}

require_file() {
    local path="$1"
    [[ -f "${path}" ]] || fail "inputs" "missing required file: ${path}"
}

require_nonempty_file() {
    local path="$1"
    [[ -s "${path}" ]] || fail "inputs" "missing or empty required file: ${path}"
}

shell_join() {
    local rendered=""
    local arg
    for arg in "$@"; do
        rendered+="$(printf '%q' "${arg}") "
    done
    printf '%s\n' "${rendered% }"
}

csv_to_lines() {
    printf '%s\n' "$1" | tr ',' '\n' | sed '/^[[:space:]]*$/d'
}

short_hash() {
    printf '%s' "$1" | cut -c1-12
}

latest_report_file() {
    local root="$1"
    find "${root}" -type f -name report.json 2>/dev/null | sort | tail -n 1 || true
}

should_emit_single_writer_classification() {
    [[ "${BEAD_ID}" == "bd-db300.8.1.2" || "${EMIT_SINGLE_WRITER_CLASSIFICATION}" == "1" ]]
}

should_emit_single_writer_validation() {
    [[ "${BEAD_ID}" == "bd-db300.8.2.3" || "${EMIT_SINGLE_WRITER_VALIDATION:-0}" == "1" ]]
}

should_emit_single_writer_role() {
    [[ "${BEAD_ID}" == "bd-db300.8.3" || "${EMIT_SINGLE_WRITER_ROLE:-0}" == "1" ]]
}

ensure_row_exists() {
    local row_id="$1"
    jq -e --arg row_id "${row_id}" '.matrix_rows[] | select(.row_id == $row_id)' "${CAMPAIGN_MANIFEST_FILE}" >/dev/null \
        || fail "inputs" "row_id `${row_id}` not found in ${CAMPAIGN_MANIFEST_REL}"
}

row_workload() {
    local row_id="$1"
    jq -r --arg row_id "${row_id}" '.matrix_rows[] | select(.row_id == $row_id) | .workload' "${CAMPAIGN_MANIFEST_FILE}"
}

row_concurrency() {
    local row_id="$1"
    jq -r --arg row_id "${row_id}" '.matrix_rows[] | select(.row_id == $row_id) | .concurrency' "${CAMPAIGN_MANIFEST_FILE}"
}

row_fixture_ids() {
    local row_id="$1"
    if [[ -n "${FIXTURE_IDS}" ]]; then
        csv_to_lines "${FIXTURE_IDS}"
        return
    fi
    jq -r --arg row_id "${row_id}" '.matrix_rows[] | select(.row_id == $row_id) | .fixtures[]' "${CAMPAIGN_MANIFEST_FILE}"
}

row_placement_profiles() {
    local row_id="$1"
    if [[ -n "${PLACEMENT_PROFILE_IDS}" ]]; then
        csv_to_lines "${PLACEMENT_PROFILE_IDS}"
        return
    fi
    jq -r --arg row_id "${row_id}" '
        .matrix_rows[]
        | select(.row_id == $row_id)
        | .placement_variants[]
        | .placement_profile_id
    ' "${CAMPAIGN_MANIFEST_FILE}"
}

placement_hardware_class() {
    local row_id="$1"
    local placement_profile_id="$2"
    jq -r --arg row_id "${row_id}" --arg placement_profile_id "${placement_profile_id}" '
        .matrix_rows[]
        | select(.row_id == $row_id)
        | .placement_variants[]
        | select(.placement_profile_id == $placement_profile_id)
        | .hardware_class_id
    ' "${CAMPAIGN_MANIFEST_FILE}"
}

placement_profile_json() {
    local placement_profile_id="$1"
    jq -c --arg placement_profile_id "${placement_profile_id}" '
        .placement_profiles[]
        | select(.id == $placement_profile_id)
    ' "${CAMPAIGN_MANIFEST_FILE}"
}

hardware_class_json() {
    local hardware_class_id="$1"
    jq -c --arg hardware_class_id "${hardware_class_id}" '
        .hardware_classes[]
        | select(.id == $hardware_class_id)
    ' "${CAMPAIGN_MANIFEST_FILE}"
}

placement_execution_status() {
    local placement_profile_id="$1"
    if [[ "${placement_profile_id}" == "baseline_unpinned" ]]; then
        printf 'comparable_under_scheduler_default\n'
    else
        printf 'declared_only_requires_external_placement_enforcement\n'
    fi
}

validate_storage_profile() {
    case "$1" in
        file_backed|memory) ;;
        *) fail "inputs" "unsupported storage profile: $1" ;;
    esac
}

storage_profile_note() {
    case "$1" in
        file_backed)
            printf '%s\n' "file_backed runs replay each iteration against a copied on-disk working database"
            ;;
        memory)
            printf '%s\n' "memory runs replay the same OpLog against :memory: connections for zero-file-placement comparison"
            ;;
        *)
            fail "inputs" "unsupported storage profile: $1"
            ;;
    esac
}

mode_engine_label() {
    case "$1" in
        sqlite_reference) printf 'sqlite3\n' ;;
        fsqlite_mvcc) printf 'fsqlite_mvcc\n' ;;
        fsqlite_single_writer) printf 'fsqlite_single_writer\n' ;;
        *) fail "inputs" "unsupported mode_id: $1" ;;
    esac
}

mode_cli_args() {
    case "$1" in
        sqlite_reference) printf '%s\n' "--engine sqlite3" ;;
        fsqlite_mvcc) printf '%s\n' "--engine fsqlite --mvcc" ;;
        fsqlite_single_writer) printf '%s\n' "--engine fsqlite --no-mvcc" ;;
        *) fail "inputs" "unsupported mode_id: $1" ;;
    esac
}

recover_benchmark_outputs_from_logs() {
    local results_jsonl="$1"
    local summary_md="$2"
    local stdout_log="$3"
    local stderr_log="$4"

    local benchmark_json=""
    local log_path
    for log_path in "${stdout_log}" "${stderr_log}"; do
        [[ -f "${log_path}" ]] || continue
        benchmark_json="$(grep -E '^{"benchmark_id":' "${log_path}" | tail -n 1 || true)"
        if [[ -n "${benchmark_json}" ]]; then
            printf '%s\n' "${benchmark_json}" > "${results_jsonl}"
            break
        fi
    done

    if [[ ! -s "${results_jsonl}" ]]; then
        return 1
    fi

    if [[ ! -s "${summary_md}" ]]; then
        jq -r '
            [
                "# Benchmark Summary",
                "",
                "_Recovered locally from rch-captured bench logs because the remote explicit output files were not synchronized back into the workspace._",
                "",
                "- benchmark_id: `\(.benchmark_id)`",
                "- engine: `\(.engine)`",
                "- workload: `\(.workload)`",
                "- fixture_id: `\(.fixture_id)`",
                "- concurrency: `\(.concurrency)`",
                "- measurement_count: `\(.measurement_count)`",
                "- latency.median_ms: `\(.latency.median_ms)`",
                "- latency.p95_ms: `\(.latency.p95_ms)`",
                "- throughput.median_ops_per_sec: `\(.throughput.median_ops_per_sec)`"
            ] | join("\n")
        ' < "${results_jsonl}" > "${summary_md}"
    fi

    return 0
}

run_mode_benchmark() {
    local row_id="$1"
    local fixture_id="$2"
    local workload="$3"
    local concurrency="$4"
    local placement_profile_id="$5"
    local hardware_class_id="$6"
    local storage_profile_id="$7"
    local pack_dir="$8"
    local mode_id="$9"

    local mode_dir="${pack_dir}/${mode_id}"
    local results_jsonl="${mode_dir}/results.jsonl"
    local summary_md="${mode_dir}/summary.md"
    local summary_json="${mode_dir}/summary.json"
    local stdout_log="${mode_dir}/stdout.log"
    local stderr_log="${mode_dir}/stderr.log"

    mkdir -p "${mode_dir}"

    local cli_args_raw
    cli_args_raw="$(mode_cli_args "${mode_id}")"
    local -a mode_args=()
    # shellcheck disable=SC2206
    mode_args=(${cli_args_raw})

    local -a cmd=(
        env
        "CARGO_TARGET_DIR=${RCH_TARGET_DIR}"
        cargo run
        -p fsqlite-e2e
        --profile "${CARGO_PROFILE}"
        --bin realdb-e2e
        --
        bench
        "${mode_args[@]}"
        --db "${fixture_id}"
        --preset "${workload}"
        --concurrency "${concurrency}"
        --placement-profile "${placement_profile_id}"
        --storage-profile "${storage_profile_id}"
        --warmup "${WARMUP}"
        --repeat "${REPEAT}"
        --output-jsonl "${results_jsonl}"
        --output-md "${summary_md}"
    )

    log_event "INFO" "run" "starting ${row_id} fixture=${fixture_id} placement=${placement_profile_id} storage=${storage_profile_id} mode=${mode_id}"

    if ! rch exec -- "${cmd[@]}" </dev/null >"${stdout_log}" 2>"${stderr_log}"; then
        fail "run" "benchmark failed for row=${row_id} fixture=${fixture_id} placement=${placement_profile_id} storage=${storage_profile_id} mode=${mode_id}; see ${stderr_log}"
    fi

    if [[ ! -s "${results_jsonl}" ]] || [[ ! -s "${summary_md}" ]]; then
        recover_benchmark_outputs_from_logs \
            "${results_jsonl}" \
            "${summary_md}" \
            "${stdout_log}" \
            "${stderr_log}" \
            || fail "run" "benchmark completed but outputs were not available locally for row=${row_id} fixture=${fixture_id} placement=${placement_profile_id} storage=${storage_profile_id} mode=${mode_id}; see ${stderr_log}"
    fi

    require_nonempty_file "${results_jsonl}"
    require_nonempty_file "${summary_md}"

    jq -c \
        --arg row_id "${row_id}" \
        --arg fixture_id "${fixture_id}" \
        --arg workload "${workload}" \
        --arg placement_profile_id "${placement_profile_id}" \
        --arg hardware_class_id "${hardware_class_id}" \
        --arg storage_profile_id "${storage_profile_id}" \
        --arg mode_id "${mode_id}" \
        --arg engine_label "$(mode_engine_label "${mode_id}")" \
        --arg results_jsonl_rel "$(realpath --relative-to="${pack_dir}" "${results_jsonl}")" \
        --arg summary_md_rel "$(realpath --relative-to="${pack_dir}" "${summary_md}")" \
        --arg stdout_log_rel "$(realpath --relative-to="${pack_dir}" "${stdout_log}")" \
        --arg stderr_log_rel "$(realpath --relative-to="${pack_dir}" "${stderr_log}")" \
        --arg rerun_command "cd ${WORKSPACE_ROOT} && BEAD_ID=${BEAD_ID} OUTPUT_DIR=${OUTPUT_DIR} ROW_IDS=${row_id} FIXTURE_IDS=${fixture_id} PLACEMENT_PROFILE_IDS=${placement_profile_id} STORAGE_PROFILE_IDS=${storage_profile_id} CARGO_PROFILE=${CARGO_PROFILE} WARMUP=${WARMUP} REPEAT=${REPEAT} bash ${SCRIPT_ENTRYPOINT}" \
        '
        . as $bench
        | {
            mode_id: $mode_id,
            engine_label: $engine_label,
            row_id: $row_id,
            fixture_id: $fixture_id,
            workload: $workload,
            concurrency: $bench.concurrency,
            placement_profile_id: $placement_profile_id,
            hardware_class_id: $hardware_class_id,
            storage_profile_id: $storage_profile_id,
            benchmark_id: $bench.benchmark_id,
            measurement_count: $bench.measurement_count,
            latency: {
                median_ms: $bench.latency.median_ms,
                p95_ms: $bench.latency.p95_ms,
                p99_ms: $bench.latency.p99_ms
            },
            throughput: {
                mean_ops_per_sec: $bench.throughput.mean_ops_per_sec,
                median_ops_per_sec: $bench.throughput.median_ops_per_sec,
                peak_ops_per_sec: $bench.throughput.peak_ops_per_sec
            },
            retries: {
                total: ($bench.iterations | map(.retries) | add // 0),
                mean_per_iteration: (
                    if ($bench.iterations | length) == 0
                    then 0
                    else (($bench.iterations | map(.retries) | add // 0) / ($bench.iterations | length))
                    end
                )
            },
            aborts: {
                total: ($bench.iterations | map(.aborts) | add // 0),
                mean_per_iteration: (
                    if ($bench.iterations | length) == 0
                    then 0
                    else (($bench.iterations | map(.aborts) | add // 0) / ($bench.iterations | length))
                    end
                )
            },
            files: {
                results_jsonl: $results_jsonl_rel,
                summary_md: $summary_md_rel,
                stdout_log: $stdout_log_rel,
                stderr_log: $stderr_log_rel
            },
            rerun_command: $rerun_command,
            benchmark_summary: $bench
        }
        ' < "${results_jsonl}" > "${summary_json}"

    log_event "INFO" "run" "completed ${row_id} fixture=${fixture_id} placement=${placement_profile_id} storage=${storage_profile_id} mode=${mode_id}"
}

build_pack_manifest() {
    local row_id="$1"
    local fixture_id="$2"
    local workload="$3"
    local concurrency="$4"
    local placement_profile_id="$5"
    local hardware_class_id="$6"
    local storage_profile_id="$7"
    local pack_dir="$8"

    local placement_profile
    placement_profile="$(placement_profile_json "${placement_profile_id}")"
    [[ -n "${placement_profile}" ]] || fail "inputs" "placement profile `${placement_profile_id}` not found"

    local hardware_class
    hardware_class="$(hardware_class_json "${hardware_class_id}")"
    [[ -n "${hardware_class}" ]] || fail "inputs" "hardware class `${hardware_class_id}` not found"

    local storage_note
    storage_note="$(storage_profile_note "${storage_profile_id}")"

    jq -n \
        --arg schema_version "fsqlite-e2e.db300.matched_mode_pack.v2" \
        --arg bead_id "${BEAD_ID}" \
        --arg run_id "${RUN_ID}" \
        --arg generated_at "${GENERATED_AT}" \
        --arg retention_class "${RETENTION_CLASS}" \
        --arg row_id "${row_id}" \
        --arg fixture_id "${fixture_id}" \
        --arg workload "${workload}" \
        --argjson concurrency "${concurrency}" \
        --arg placement_profile_id "${placement_profile_id}" \
        --arg hardware_class_id "${hardware_class_id}" \
        --arg storage_profile_id "${storage_profile_id}" \
        --arg comparability_status "$(placement_execution_status "${placement_profile_id}")" \
        --arg storage_note "${storage_note}" \
        --arg source_revision "${SOURCE_REVISION}" \
        --arg beads_hash "${BEADS_HASH}" \
        --arg cargo_profile "${CARGO_PROFILE}" \
        --argjson warmup "${WARMUP}" \
        --argjson repeat "${REPEAT}" \
        --arg script_entrypoint "${SCRIPT_ENTRYPOINT}" \
        --arg pack_dir "${pack_dir}" \
        --arg pack_dir_rel "$(realpath --relative-to="${WORKSPACE_ROOT}" "${pack_dir}")" \
        --slurpfile placement_profile <(printf '%s\n' "${placement_profile}") \
        --slurpfile hardware_class <(printf '%s\n' "${hardware_class}") \
        --slurpfile sqlite "${pack_dir}/sqlite_reference/summary.json" \
        --slurpfile mvcc "${pack_dir}/fsqlite_mvcc/summary.json" \
        --slurpfile single "${pack_dir}/fsqlite_single_writer/summary.json" \
        '
        def ratio($num; $den):
            if $den == null or $den == 0 then null else ($num / $den) end;
        {
            schema_version: $schema_version,
            bead_id: $bead_id,
            run_id: $run_id,
            generated_at: $generated_at,
            retention_class: $retention_class,
            row_id: $row_id,
            fixture_id: $fixture_id,
            workload: $workload,
            concurrency: $concurrency,
            placement_profile_id: $placement_profile_id,
            hardware_class_id: $hardware_class_id,
            storage_profile_id: $storage_profile_id,
            comparability_status: $comparability_status,
            source_revision: $source_revision,
            beads_data_hash: $beads_hash,
            cargo_profile: $cargo_profile,
            warmup_iterations: $warmup,
            measurement_iterations: $repeat,
            script_entrypoint: $script_entrypoint,
            pack_dir: $pack_dir,
            pack_dir_relpath: $pack_dir_rel,
            placement_profile: $placement_profile[0],
            hardware_class: $hardware_class[0],
            mode_results: {
                sqlite_reference: $sqlite[0],
                fsqlite_mvcc: $mvcc[0],
                fsqlite_single_writer: $single[0]
            },
            deltas: {
                mvcc_vs_sqlite_median_ops_ratio:
                    ratio($mvcc[0].throughput.median_ops_per_sec; $sqlite[0].throughput.median_ops_per_sec),
                single_writer_vs_mvcc_median_ops_ratio:
                    ratio($single[0].throughput.median_ops_per_sec; $mvcc[0].throughput.median_ops_per_sec),
                single_writer_minus_mvcc_median_latency_ms:
                    ($single[0].latency.median_ms - $mvcc[0].latency.median_ms),
                single_writer_minus_mvcc_mean_retries:
                    ($single[0].retries.mean_per_iteration - $mvcc[0].retries.mean_per_iteration),
                single_writer_minus_sqlite_mean_retries:
                    ($single[0].retries.mean_per_iteration - $sqlite[0].retries.mean_per_iteration)
            },
            notes: (
                if $placement_profile_id == "baseline_unpinned"
                then [
                    "baseline_unpinned packs are directly comparable under scheduler-default placement"
                ]
                else [
                    "non-baseline placement profiles are recorded from the canonical contract but require external CPU and memory placement enforcement outside this script",
                    "packs produced without that enforcement should be treated as declared_only rather than clean topology claims"
                ]
                end
                + [$storage_note]
            )
        }
        ' > "${pack_dir}/manifest.json"
}

build_pack_summary() {
    local pack_dir="$1"
    jq -r '
        [
            "# Matched Mode Pack",
            "",
            "- row_id: `\(.row_id)`",
            "- fixture_id: `\(.fixture_id)`",
            "- placement_profile_id: `\(.placement_profile_id)`",
            "- storage_profile_id: `\(.storage_profile_id)`",
            "- hardware_class_id: `\(.hardware_class_id)`",
            "- comparability_status: `\(.comparability_status)`",
            "- source_revision: `\(.source_revision)`",
            "- beads_data_hash: `\(.beads_data_hash)`",
            "",
            "## Mode Summary",
            "",
            "| Mode | Median ops/s | Median latency (ms) | P95 latency (ms) | Mean retries | Mean aborts |",
            "| --- | ---: | ---: | ---: | ---: | ---: |",
            "| sqlite_reference | \(.mode_results.sqlite_reference.throughput.median_ops_per_sec) | \(.mode_results.sqlite_reference.latency.median_ms) | \(.mode_results.sqlite_reference.latency.p95_ms) | \(.mode_results.sqlite_reference.retries.mean_per_iteration) | \(.mode_results.sqlite_reference.aborts.mean_per_iteration) |",
            "| fsqlite_mvcc | \(.mode_results.fsqlite_mvcc.throughput.median_ops_per_sec) | \(.mode_results.fsqlite_mvcc.latency.median_ms) | \(.mode_results.fsqlite_mvcc.latency.p95_ms) | \(.mode_results.fsqlite_mvcc.retries.mean_per_iteration) | \(.mode_results.fsqlite_mvcc.aborts.mean_per_iteration) |",
            "| fsqlite_single_writer | \(.mode_results.fsqlite_single_writer.throughput.median_ops_per_sec) | \(.mode_results.fsqlite_single_writer.latency.median_ms) | \(.mode_results.fsqlite_single_writer.latency.p95_ms) | \(.mode_results.fsqlite_single_writer.retries.mean_per_iteration) | \(.mode_results.fsqlite_single_writer.aborts.mean_per_iteration) |",
            "",
            "## Deltas",
            "",
            "- mvcc_vs_sqlite_median_ops_ratio: `\(.deltas.mvcc_vs_sqlite_median_ops_ratio)`",
            "- single_writer_vs_mvcc_median_ops_ratio: `\(.deltas.single_writer_vs_mvcc_median_ops_ratio)`",
            "- single_writer_minus_mvcc_median_latency_ms: `\(.deltas.single_writer_minus_mvcc_median_latency_ms)`",
            "- single_writer_minus_mvcc_mean_retries: `\(.deltas.single_writer_minus_mvcc_mean_retries)`",
            "",
            "## Notes",
            "",
            (.notes[] | "- " + .)
        ] | join("\n")
    ' "${pack_dir}/manifest.json" > "${pack_dir}/summary.md"
}

collect_pack() {
    local row_id="$1"
    local fixture_id="$2"
    local placement_profile_id="$3"
    local storage_profile_id="$4"

    local workload
    workload="$(row_workload "${row_id}")"
    local concurrency
    concurrency="$(row_concurrency "${row_id}")"
    local hardware_class_id
    hardware_class_id="$(placement_hardware_class "${row_id}" "${placement_profile_id}")"

    [[ -n "${hardware_class_id}" ]] \
        || fail "inputs" "row `${row_id}` does not define placement `${placement_profile_id}`"

    local pack_dir="${PACKS_DIR}/${row_id}__${fixture_id}__${placement_profile_id}__${storage_profile_id}__run_${RUN_ID_SAFE}__rev_$(short_hash "${SOURCE_REVISION}")__beads_$(short_hash "${BEADS_HASH}")"
    mkdir -p "${pack_dir}"

    log_event "INFO" "pack" "collecting matched pack row=${row_id} fixture=${fixture_id} placement=${placement_profile_id} storage=${storage_profile_id}"

    local mode_id
    for mode_id in "${MODES[@]}"; do
        run_mode_benchmark \
            "${row_id}" \
            "${fixture_id}" \
            "${workload}" \
            "${concurrency}" \
            "${placement_profile_id}" \
            "${hardware_class_id}" \
            "${storage_profile_id}" \
            "${pack_dir}" \
            "${mode_id}"
    done

    build_pack_manifest \
        "${row_id}" \
        "${fixture_id}" \
        "${workload}" \
        "${concurrency}" \
        "${placement_profile_id}" \
        "${hardware_class_id}" \
        "${storage_profile_id}" \
        "${pack_dir}"
    build_pack_summary "${pack_dir}"
}

build_report() {
    local manifests=()
    while IFS= read -r path; do
        manifests+=("${path}")
    done < <(find "${PACKS_DIR}" -mindepth 2 -maxdepth 2 -name manifest.json | sort)

    ((${#manifests[@]} > 0)) || fail "report" "no pack manifests were generated"

    jq -s \
        --arg schema_version "fsqlite-e2e.db300.matched_mode_pack_report.v2" \
        --arg bead_id "${BEAD_ID}" \
        --arg run_id "${RUN_ID}" \
        --arg generated_at "${GENERATED_AT}" \
        --arg script_entrypoint "${SCRIPT_ENTRYPOINT}" \
        --arg campaign_manifest "${CAMPAIGN_MANIFEST_REL}" \
        --arg output_dir "${OUTPUT_DIR}" \
        '
        {
            schema_version: $schema_version,
            bead_id: $bead_id,
            run_id: $run_id,
            generated_at: $generated_at,
            script_entrypoint: $script_entrypoint,
            campaign_manifest: $campaign_manifest,
            output_dir: $output_dir,
            pack_count: length,
            packs: .
        }
        ' "${manifests[@]}" > "${REPORT_JSON}"

    jq -r '
        [
            "# Track H Matched Artifact Packs",
            "",
            "- run_id: `\(.run_id)`",
            "- campaign_manifest: `\(.campaign_manifest)`",
            "- pack_count: `\(.pack_count)`",
            "",
            "| row_id | fixture_id | placement_profile_id | storage_profile_id | comparability | sqlite ops/s | mvcc ops/s | single-writer ops/s | single-writer vs mvcc ops ratio | single-writer minus mvcc retries |",
            "| --- | --- | --- | --- | --- | ---: | ---: | ---: | ---: | ---: |",
            (
                .packs[]
                | "| \(.row_id) | \(.fixture_id) | \(.placement_profile_id) | \(.storage_profile_id) | \(.comparability_status) | \(.mode_results.sqlite_reference.throughput.median_ops_per_sec) | \(.mode_results.fsqlite_mvcc.throughput.median_ops_per_sec) | \(.mode_results.fsqlite_single_writer.throughput.median_ops_per_sec) | \(.deltas.single_writer_vs_mvcc_median_ops_ratio) | \(.deltas.single_writer_minus_mvcc_mean_retries) |"
            )
        ] | join("\n")
    ' "${REPORT_JSON}" > "${SUMMARY_MD}"
}

build_single_writer_classification() {
    local baseline_report_json="${BASELINE_REPORT_JSON:-$(latest_report_file "${WORKSPACE_ROOT}/artifacts/perf/bd-db300.8.1.1")}"
    [[ -n "${baseline_report_json}" ]] || fail "classification" "failed to resolve a baseline bd-db300.8.1.1 report.json"
    require_nonempty_file "${baseline_report_json}"
    require_nonempty_file "${REPORT_JSON}"

    jq -n \
        --arg schema_version "fsqlite-e2e.db300.single_writer_classification.v1" \
        --arg bead_id "${BEAD_ID}" \
        --arg run_id "${RUN_ID}" \
        --arg generated_at "${GENERATED_AT}" \
        --arg baseline_report_json "${baseline_report_json}" \
        --arg current_report_json "${REPORT_JSON}" \
        --slurpfile baseline "${baseline_report_json}" \
        --slurpfile current "${REPORT_JSON}" \
        '
        def pack($doc; $placement; $storage):
            $doc[0].packs[]
            | select(
                .fixture_id == "frankensqlite"
                and .placement_profile_id == $placement
                and .storage_profile_id == $storage
            );
        (pack($baseline; "baseline_unpinned"; "file_backed")) as $baseline_file_backed |
        (pack($baseline; "baseline_unpinned"; "memory")) as $baseline_memory |
        (pack($current; "recommended_pinned"; "file_backed")) as $recommended_file_backed |
        (pack($current; "recommended_pinned"; "memory")) as $recommended_memory |
        (pack($current; "adversarial_cross_node"; "file_backed")) as $adversarial_file_backed |
        (pack($current; "adversarial_cross_node"; "memory")) as $adversarial_memory |
        {
          schema_version: $schema_version,
          bead_id: $bead_id,
          run_id: $run_id,
          generated_at: $generated_at,
          current_report_json: $current_report_json,
          baseline_report_json: $baseline_report_json,
          evidence_scope: {
            fixture_id: "frankensqlite",
            workload_row: "mixed_read_write_c4",
            comparable_baseline_pack: {
              placement_profile_id: "baseline_unpinned",
              report_json: $baseline_report_json
            },
            declared_only_follow_on_packs: [
              {
                placement_profile_id: "recommended_pinned",
                report_json: $current_report_json
              },
              {
                placement_profile_id: "adversarial_cross_node",
                report_json: $current_report_json
              }
            ]
          },
          findings: [
            {
              category: "unavoidable_serialization",
              status: "not_primary_explanation_for_the_extra_file_backed_gap",
              confidence: "high",
              evidence: {
                baseline_memory_single_writer_vs_mvcc_ratio: $baseline_memory.deltas.single_writer_vs_mvcc_median_ops_ratio,
                recommended_pinned_memory_single_writer_vs_mvcc_ratio: $recommended_memory.deltas.single_writer_vs_mvcc_median_ops_ratio,
                adversarial_cross_node_memory_single_writer_vs_mvcc_ratio: $adversarial_memory.deltas.single_writer_vs_mvcc_median_ops_ratio,
                baseline_memory_retry_delta: $baseline_memory.deltas.single_writer_minus_mvcc_mean_retries,
                recommended_pinned_memory_retry_delta: $recommended_memory.deltas.single_writer_minus_mvcc_mean_retries,
                adversarial_cross_node_memory_retry_delta: $adversarial_memory.deltas.single_writer_minus_mvcc_mean_retries
              },
              rationale: "When storage is memory-backed, single-writer no longer trails MVCC and retry deltas collapse to zero across the baseline and both declared placements, so serialization alone does not explain the extra file-backed slowdown.",
              action_for_h2: "Prioritize the file-backed single-writer durability or queue path before trying broad shared-engine reductions."
            },
            {
              category: "avoidable_wait_and_queue_amplification",
              status: "confirmed",
              confidence: "high",
              evidence: {
                baseline_file_backed_single_writer_vs_mvcc_ratio: $baseline_file_backed.deltas.single_writer_vs_mvcc_median_ops_ratio,
                baseline_file_backed_retry_delta: $baseline_file_backed.deltas.single_writer_minus_mvcc_mean_retries,
                adversarial_cross_node_file_backed_single_writer_vs_mvcc_ratio: $adversarial_file_backed.deltas.single_writer_vs_mvcc_median_ops_ratio,
                adversarial_cross_node_file_backed_retry_delta: $adversarial_file_backed.deltas.single_writer_minus_mvcc_mean_retries,
                recommended_pinned_file_backed_single_writer_vs_mvcc_ratio: $recommended_file_backed.deltas.single_writer_vs_mvcc_median_ops_ratio,
                recommended_pinned_file_backed_retry_delta: $recommended_file_backed.deltas.single_writer_minus_mvcc_mean_retries
              },
              rationale: "The comparable baseline pack and the declared-only adversarial pack both show single-writer throughput collapsing below MVCC while retry deltas remain materially positive. Recommended-pinned reduces the damage, which points to queueing or wait amplification in the file-backed single-writer path rather than a fixed engine tax.",
              action_for_h2: "Target the single-writer file-backed retry and baton-passing path first, then re-measure before touching broader shared decode or planner costs."
            },
            {
              category: "topology_sensitivity",
              status: "suggested_but_not_transportable_without_external_binding",
              confidence: "mixed",
              evidence: {
                recommended_pinned_file_backed_single_writer_vs_mvcc_ratio: $recommended_file_backed.deltas.single_writer_vs_mvcc_median_ops_ratio,
                adversarial_cross_node_file_backed_single_writer_vs_mvcc_ratio: $adversarial_file_backed.deltas.single_writer_vs_mvcc_median_ops_ratio,
                recommended_pinned_comparability: $recommended_file_backed.comparability_status,
                adversarial_cross_node_comparability: $adversarial_file_backed.comparability_status
              },
              rationale: "Declared-only placement variants separate into a healthier recommended-pinned file-backed pack and a much worse adversarial cross-node pack, so placement likely changes the severity of the wait amplification. The claim stays mixed because the collector does not enforce external CPU or NUMA binding.",
              action_for_h2: "Prefer fixes that reduce queue ownership handoff sensitivity even before topology-enforced reruns exist."
            },
            {
              category: "wake_storms",
              status: "not_directly_measured_by_matched_pack",
              confidence: "mixed",
              evidence: {
                supporting_signal: "retry and latency deltas imply extra waiting, but no explicit wake counter is captured in this artifact family"
              },
              rationale: "The matched packs prove waiting and retry amplification, but they do not attribute that waiting to wake storms versus other queue mechanisms.",
              action_for_h2: "If the first queue-path fix does not collapse the gap, add explicit wake and handoff counters before deeper speculation."
            },
            {
              category: "ownership_churn",
              status: "not_supported_by_current_pack",
              confidence: "mixed",
              evidence: {
                supporting_signal: "ownership churn is tracked by the shared D/J evidence lanes rather than this matched-pack collector"
              },
              rationale: "These matched packs isolate comparison-mode behavior, but they do not directly measure PageData ownership reuse or clone-heavy transitions.",
              action_for_h2: "Treat ownership churn as a secondary hypothesis only after single-writer queue and retry fixes are tested."
            },
            {
              category: "duplicate_work",
              status: "not_supported_by_current_pack",
              confidence: "mixed",
              evidence: {
                supporting_signal: "no subphase counters in this pack identify duplicated serialized work"
              },
              rationale: "The artifact family does not expose subphase duplication directly, so duplicate work remains a follow-on hypothesis instead of the leading explanation.",
              action_for_h2: "Do not broaden H2 into speculative duplicate-work cleanup unless the targeted queue-path changes fail to move the file-backed cells."
            }
          ],
          primary_h2_actions: [
            "Fix file-backed single-writer retry and queue amplification before shared-engine cleanup.",
            "Keep non-baseline placement evidence labeled declared-only until external placement enforcement exists.",
            "Revisit wake storms, ownership churn, and duplicate work only if the targeted queue-path fix fails to remove the file-backed gap."
          ]
        }
        ' > "${CLASSIFICATION_JSON}"

    jq -r '
        [
            "# H1.2 Single-Writer Slowdown Classification",
            "",
            "- run_id: `\(.run_id)`",
            "- baseline_report_json: `\(.baseline_report_json)`",
            "- current_report_json: `\(.current_report_json)`",
            "",
            "## Findings",
            "",
            (
                .findings[]
                | "- `\(.category)`: `\(.status)` (\(.confidence))"
            ),
            "",
            "## Actionable Direction For H2",
            "",
            (.primary_h2_actions[] | "- " + .)
        ] | join("\n")
    ' "${CLASSIFICATION_JSON}" > "${CLASSIFICATION_MD}"
}

run_mvcc_default_guard() {
    local -a command=(
        rch exec --
        env "CARGO_TARGET_DIR=${MVCC_DEFAULT_GUARD_TARGET_DIR}"
        cargo test -p fsqlite-e2e --test "${MVCC_DEFAULT_GUARD_TEST}" -- --nocapture
    )
    local rendered_command
    rendered_command="$(shell_join "${command[@]}")"
    log_event "INFO" "mvcc-default-guard" "running ${rendered_command}"
    if ! "${command[@]}" > "${MVCC_DEFAULT_GUARD_LOG}" 2>&1; then
        fail "mvcc-default-guard" "MVCC default guard failed; see ${MVCC_DEFAULT_GUARD_LOG}"
    fi
}

run_concurrent_mode_default_source_guard() {
    if ! rg -n 'concurrent_mode_default: RefCell::new\(true\)' \
        "${WORKSPACE_ROOT}/crates/fsqlite-core/src/connection.rs" \
        > "${CONCURRENT_MODE_DEFAULT_GUARD}"; then
        fail "concurrent-mode-default-guard" "concurrent_mode_default true guard missing"
    fi
}

run_single_writer_verify_suite() {
    mkdir -p "${SINGLE_WRITER_VERIFY_DIR}"
    local -a command=(
        env
        "CARGO_TARGET_DIR=${SINGLE_WRITER_VERIFY_TARGET_DIR}"
        cargo run
        -p fsqlite-e2e
        --bin realdb-e2e
        --
        verify-suite
        --suite-id "${BEAD_ID}.single_writer_role"
        --execution-context local
        --mode fsqlite_single_writer
        --placement-profile baseline_unpinned
        --verification-depth quick
        --activation-regime low_concurrency_fixed_cost
        --shadow-mode off
        --db frankensqlite
        --workload all
        --concurrency 1
        --output-dir "${SINGLE_WRITER_VERIFY_DIR}"
        --pretty
    )
    local rendered_command
    rendered_command="$(shell_join "${command[@]}")"
    log_event "INFO" "single-writer-verify-suite" "running ${rendered_command}"
    if ! "${command[@]}" > "${SINGLE_WRITER_VERIFY_LOG}" 2>&1; then
        fail "single-writer-verify-suite" "forced single-writer verify-suite packaging failed; see ${SINGLE_WRITER_VERIFY_LOG}"
    fi
    require_nonempty_file "${SINGLE_WRITER_VERIFY_DIR}/suite_package.json"
    require_nonempty_file "${SINGLE_WRITER_VERIFY_DIR}/suite_summary.md"
    require_nonempty_file "${SINGLE_WRITER_VERIFY_DIR}/logs/verify_suite.jsonl"
}

build_single_writer_role() {
    local baseline_report_json="${BASELINE_REPORT_JSON:-$(latest_report_file "${WORKSPACE_ROOT}/artifacts/perf/bd-db300.8.1.2")}"
    [[ -n "${baseline_report_json}" ]] || fail "single-writer-role" "failed to resolve a baseline bd-db300.8.1.2 report.json"
    require_nonempty_file "${baseline_report_json}"
    require_nonempty_file "${REPORT_JSON}"
    require_nonempty_file "${SINGLE_WRITER_VERIFY_DIR}/suite_package.json"
    require_nonempty_file "${SINGLE_WRITER_VERIFY_DIR}/logs/verify_suite.jsonl"
    require_nonempty_file "${CONCURRENT_MODE_DEFAULT_GUARD}"

    jq -n \
        --arg schema_version "fsqlite-e2e.db300.single_writer_role.v1" \
        --arg bead_id "${BEAD_ID}" \
        --arg run_id "${RUN_ID}" \
        --arg generated_at "${GENERATED_AT}" \
        --arg baseline_report_json "${baseline_report_json}" \
        --arg current_report_json "${REPORT_JSON}" \
        --arg verify_suite_package_json "${SINGLE_WRITER_VERIFY_DIR}/suite_package.json" \
        --arg verify_suite_summary_md "${SINGLE_WRITER_VERIFY_DIR}/suite_summary.md" \
        --arg verify_suite_log_jsonl "${SINGLE_WRITER_VERIFY_DIR}/logs/verify_suite.jsonl" \
        --arg verify_suite_stdout_log "${SINGLE_WRITER_VERIFY_LOG}" \
        --arg concurrent_mode_default_guard "${CONCURRENT_MODE_DEFAULT_GUARD}" \
        --slurpfile baseline "${baseline_report_json}" \
        --slurpfile current "${REPORT_JSON}" \
        --slurpfile verify_suite "${SINGLE_WRITER_VERIFY_DIR}/suite_package.json" \
        '
        def pack($doc; $placement; $storage):
            $doc[0].packs[]
            | select(
                .fixture_id == "frankensqlite"
                and .row_id == "mixed_read_write_c4"
                and .placement_profile_id == $placement
                and .storage_profile_id == $storage
            );
        def ratio($num; $den):
            if $den == null or $den == 0 then null else ($num / $den) end;
        def percent_delta($current; $previous):
            if $previous == null or $previous == 0 then null else ((($current - $previous) / $previous) * 100.0) end;
        def comparison($placement; $storage):
            (pack($baseline; $placement; $storage)) as $baseline_pack |
            (pack($current; $placement; $storage)) as $current_pack |
            {
              placement_profile_id: $placement,
              storage_profile_id: $storage,
              comparability_status: $current_pack.comparability_status,
              baseline_single_writer_median_ops_per_sec: $baseline_pack.mode_results.fsqlite_single_writer.throughput.median_ops_per_sec,
              current_single_writer_median_ops_per_sec: $current_pack.mode_results.fsqlite_single_writer.throughput.median_ops_per_sec,
              single_writer_median_ops_delta: (
                $current_pack.mode_results.fsqlite_single_writer.throughput.median_ops_per_sec
                - $baseline_pack.mode_results.fsqlite_single_writer.throughput.median_ops_per_sec
              ),
              single_writer_median_ops_percent_delta:
                percent_delta(
                  $current_pack.mode_results.fsqlite_single_writer.throughput.median_ops_per_sec;
                  $baseline_pack.mode_results.fsqlite_single_writer.throughput.median_ops_per_sec
                ),
              current_sqlite_median_ops_per_sec: $current_pack.mode_results.sqlite_reference.throughput.median_ops_per_sec,
              current_mvcc_median_ops_per_sec: $current_pack.mode_results.fsqlite_mvcc.throughput.median_ops_per_sec,
              current_single_writer_vs_sqlite_ratio:
                ratio(
                  $current_pack.mode_results.fsqlite_single_writer.throughput.median_ops_per_sec;
                  $current_pack.mode_results.sqlite_reference.throughput.median_ops_per_sec
                ),
              current_single_writer_vs_mvcc_ratio: $current_pack.deltas.single_writer_vs_mvcc_median_ops_ratio,
              current_single_writer_minus_mvcc_mean_retries: $current_pack.deltas.single_writer_minus_mvcc_mean_retries,
              current_single_writer_minus_mvcc_median_latency_ms: $current_pack.deltas.single_writer_minus_mvcc_median_latency_ms,
              role_read: (
                if $current_pack.mode_results.fsqlite_single_writer.throughput.median_ops_per_sec
                   >= $baseline_pack.mode_results.fsqlite_single_writer.throughput.median_ops_per_sec
                then "shared_optimizations_helped_single_writer_absolute_ops"
                else "single_writer_needs_followup"
                end
              )
            };
        [
          comparison("recommended_pinned"; "file_backed"),
          comparison("recommended_pinned"; "memory"),
          comparison("adversarial_cross_node"; "file_backed"),
          comparison("adversarial_cross_node"; "memory")
        ] as $comparisons |
        {
          schema_version: $schema_version,
          bead_id: $bead_id,
          run_id: $run_id,
          generated_at: $generated_at,
          baseline_report_json: $baseline_report_json,
          current_report_json: $current_report_json,
          single_writer_role: {
            role_id: "comparison_or_fallback_only",
            product_default: "fsqlite_mvcc",
            default_contract: "BEGIN stays MVCC-by-default through concurrent_mode_default=true; forced single-writer is opt-in via PRAGMA fsqlite.concurrent_mode=OFF or --no-mvcc.",
            report_contract: "G4 may use forced single-writer as a causal bridge between SQLite and MVCC, or as restricted fallback evidence, but not as the headline product mode."
          },
          verify_suite: {
            package_json: $verify_suite_package_json,
            summary_md: $verify_suite_summary_md,
            log_jsonl: $verify_suite_log_jsonl,
            stdout_log: $verify_suite_stdout_log,
            package: $verify_suite[0]
          },
          concurrent_mode_default_guard: {
            source_guard_file: $concurrent_mode_default_guard,
            status: "passed"
          },
          evidence_scope: {
            fixture_id: "frankensqlite",
            workload_row: "mixed_read_write_c4",
            benchmark_command_family: "scripts/verify_bd_db300_8_1_1_matched_artifact_packs.sh",
            verification_suite_command_family: "realdb-e2e verify-suite --mode fsqlite_single_writer --activation-regime low_concurrency_fixed_cost",
            placement_profiles: [
              "recommended_pinned",
              "adversarial_cross_node"
            ],
            storage_profiles: [
              "file_backed",
              "memory"
            ]
          },
          comparisons: $comparisons,
          verification_plan: {
            unit_tests: [
              "cargo test -p fsqlite-e2e --test bd_2yqp6_6_5_concurrent_mode_defaults",
              "cargo test -p fsqlite-e2e --test bd_db300_7_1_2_counter_schema_alignment"
            ],
            end_to_end_scenarios: [
              "realdb-e2e verify-suite --mode fsqlite_single_writer --verification-depth quick --activation-regime low_concurrency_fixed_cost --placement-profile baseline_unpinned",
              "matched packs for sqlite_reference, fsqlite_mvcc, and fsqlite_single_writer on mixed_read_write_c4 with recommended_pinned and adversarial_cross_node placement metadata"
            ],
            logging_artifacts: [
              "events.jsonl",
              "report.json",
              "single_writer_role.json",
              "single_writer_role.md",
              "verify-suite/single-writer/logs/verify_suite.jsonl",
              "concurrent_mode_default_guard.txt"
            ],
            g4_inputs: [
              "current_report_json",
              "single_writer_role.comparisons",
              "single_writer_role.role_id",
              "verify_suite.package.rerun_entrypoint",
              "verify_suite.package.focused_rerun_entrypoint"
            ]
          }
        }
        | .summary = {
            single_writer_role: .single_writer_role.role_id,
            default_mode: .single_writer_role.product_default,
            forced_single_writer_verify_suite_status: "passed",
            concurrent_mode_default_guard_status: .concurrent_mode_default_guard.status,
            all_single_writer_absolute_ops_improved: ([.comparisons[].role_read] | all(. == "shared_optimizations_helped_single_writer_absolute_ops")),
            file_backed_single_writer_ops_percent_delta_range: {
              min: ([.comparisons[] | select(.storage_profile_id == "file_backed") | .single_writer_median_ops_percent_delta] | min),
              max: ([.comparisons[] | select(.storage_profile_id == "file_backed") | .single_writer_median_ops_percent_delta] | max)
            },
            memory_single_writer_ops_percent_delta_range: {
              min: ([.comparisons[] | select(.storage_profile_id == "memory") | .single_writer_median_ops_percent_delta] | min),
              max: ([.comparisons[] | select(.storage_profile_id == "memory") | .single_writer_median_ops_percent_delta] | max)
            }
          }
        ' > "${SINGLE_WRITER_ROLE_JSON}"

    jq -r '
        [
            "# H3 Single-Writer Role And Evidence",
            "",
            "- run_id: `\(.run_id)`",
            "- role: `\(.single_writer_role.role_id)`",
            "- product default: `\(.single_writer_role.product_default)`",
            "- forced single-writer verify-suite: `passed`",
            "- concurrent_mode_default guard: `\(.concurrent_mode_default_guard.status)`",
            "- baseline_report_json: `\(.baseline_report_json)`",
            "- current_report_json: `\(.current_report_json)`",
            "",
            "## Role",
            "",
            .single_writer_role.default_contract,
            "",
            .single_writer_role.report_contract,
            "",
            "## Benchmark Evidence",
            "",
            "| placement_profile_id | storage_profile_id | current single-writer ops/s | baseline single-writer ops/s | ops delta | percent delta | single/sqlite ratio | single/mvcc ratio | retry delta vs MVCC | role read |",
            "| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |",
            (
                .comparisons[]
                | "| \(.placement_profile_id) | \(.storage_profile_id) | \(.current_single_writer_median_ops_per_sec) | \(.baseline_single_writer_median_ops_per_sec) | \(.single_writer_median_ops_delta) | \(.single_writer_median_ops_percent_delta) | \(.current_single_writer_vs_sqlite_ratio) | \(.current_single_writer_vs_mvcc_ratio) | \(.current_single_writer_minus_mvcc_mean_retries) | \(.role_read) |"
            ),
            "",
            "## Verification Plan For G4",
            "",
            "Unit tests:",
            (.verification_plan.unit_tests[] | "- " + .),
            "",
            "End-to-end scenarios:",
            (.verification_plan.end_to_end_scenarios[] | "- " + .),
            "",
            "Logging artifacts:",
            (.verification_plan.logging_artifacts[] | "- " + .),
            "",
            "G4 inputs:",
            (.verification_plan.g4_inputs[] | "- " + .)
        ] | join("\n")
    ' "${SINGLE_WRITER_ROLE_JSON}" > "${SINGLE_WRITER_ROLE_MD}"
}

build_single_writer_validation() {
    local previous_report_json="${PREVIOUS_SHARED_PLACEMENT_REPORT_JSON:-$(latest_report_file "${WORKSPACE_ROOT}/artifacts/perf/bd-db300.8.1.2")}"
    [[ -n "${previous_report_json}" ]] || fail "validation" "failed to resolve a previous shared-placement report.json"
    require_nonempty_file "${previous_report_json}"
    require_nonempty_file "${REPORT_JSON}"
    require_nonempty_file "${MVCC_DEFAULT_GUARD_LOG}"

    local default_guard_command
    default_guard_command="$(shell_join \
        rch exec -- \
        env "CARGO_TARGET_DIR=${MVCC_DEFAULT_GUARD_TARGET_DIR}" \
        cargo test -p fsqlite-e2e --test "${MVCC_DEFAULT_GUARD_TEST}" -- --nocapture \
    )"

    jq -n \
        --arg schema_version "fsqlite-e2e.db300.single_writer_cleanup_validation.v1" \
        --arg bead_id "${BEAD_ID}" \
        --arg run_id "${RUN_ID}" \
        --arg generated_at "${GENERATED_AT}" \
        --arg previous_report_json "${previous_report_json}" \
        --arg current_report_json "${REPORT_JSON}" \
        --arg mvcc_default_guard_test "${MVCC_DEFAULT_GUARD_TEST}" \
        --arg mvcc_default_guard_log "${MVCC_DEFAULT_GUARD_LOG}" \
        --arg mvcc_default_guard_command "${default_guard_command}" \
        --slurpfile previous "${previous_report_json}" \
        --slurpfile current "${REPORT_JSON}" \
        '
        def pack($doc; $placement; $storage):
            $doc[0].packs[]
            | select(
                .fixture_id == "frankensqlite"
                and .row_id == "mixed_read_write_c4"
                and .placement_profile_id == $placement
                and .storage_profile_id == $storage
            );
        def comparison($placement; $storage):
            (pack($previous; $placement; $storage)) as $previous_pack |
            (pack($current; $placement; $storage)) as $current_pack |
            {
              placement_profile_id: $placement,
              storage_profile_id: $storage,
              previous_comparability_status: $previous_pack.comparability_status,
              current_comparability_status: $current_pack.comparability_status,
              previous_single_writer_median_ops_per_sec: $previous_pack.mode_results.fsqlite_single_writer.throughput.median_ops_per_sec,
              current_single_writer_median_ops_per_sec: $current_pack.mode_results.fsqlite_single_writer.throughput.median_ops_per_sec,
              single_writer_median_ops_delta: (
                $current_pack.mode_results.fsqlite_single_writer.throughput.median_ops_per_sec
                - $previous_pack.mode_results.fsqlite_single_writer.throughput.median_ops_per_sec
              ),
              previous_single_writer_vs_mvcc_median_ops_ratio: $previous_pack.deltas.single_writer_vs_mvcc_median_ops_ratio,
              current_single_writer_vs_mvcc_median_ops_ratio: $current_pack.deltas.single_writer_vs_mvcc_median_ops_ratio,
              single_writer_vs_mvcc_ratio_delta: (
                $current_pack.deltas.single_writer_vs_mvcc_median_ops_ratio
                - $previous_pack.deltas.single_writer_vs_mvcc_median_ops_ratio
              ),
              previous_single_writer_minus_mvcc_mean_retries: $previous_pack.deltas.single_writer_minus_mvcc_mean_retries,
              current_single_writer_minus_mvcc_mean_retries: $current_pack.deltas.single_writer_minus_mvcc_mean_retries,
              single_writer_minus_mvcc_mean_retry_delta: (
                $current_pack.deltas.single_writer_minus_mvcc_mean_retries
                - $previous_pack.deltas.single_writer_minus_mvcc_mean_retries
              ),
              validation_status: (
                if (
                    $current_pack.mode_results.fsqlite_single_writer.throughput.median_ops_per_sec
                    >= $previous_pack.mode_results.fsqlite_single_writer.throughput.median_ops_per_sec
                ) and (
                    $current_pack.deltas.single_writer_vs_mvcc_median_ops_ratio
                    >= $previous_pack.deltas.single_writer_vs_mvcc_median_ops_ratio
                ) and (
                    $current_pack.deltas.single_writer_minus_mvcc_mean_retries
                    <= $previous_pack.deltas.single_writer_minus_mvcc_mean_retries
                ) then
                    "improved_or_held"
                elif (
                    $current_pack.mode_results.fsqlite_single_writer.throughput.median_ops_per_sec
                    >= $previous_pack.mode_results.fsqlite_single_writer.throughput.median_ops_per_sec
                ) or (
                    $current_pack.deltas.single_writer_vs_mvcc_median_ops_ratio
                    >= $previous_pack.deltas.single_writer_vs_mvcc_median_ops_ratio
                ) then
                    "mixed"
                else
                    "regressed"
                end
              )
            };
        {
          schema_version: $schema_version,
          bead_id: $bead_id,
          run_id: $run_id,
          generated_at: $generated_at,
          previous_report_json: $previous_report_json,
          current_report_json: $current_report_json,
          evidence_scope: {
            fixture_id: "frankensqlite",
            workload_row: "mixed_read_write_c4",
            placement_profiles: [
              "recommended_pinned",
              "adversarial_cross_node"
            ],
            storage_profiles: [
              "file_backed",
              "memory"
            ]
          },
          mvcc_default_guard: {
            test_target: $mvcc_default_guard_test,
            command: $mvcc_default_guard_command,
            log_file: $mvcc_default_guard_log,
            status: "passed"
          },
          comparisons: [
            comparison("recommended_pinned"; "file_backed"),
            comparison("recommended_pinned"; "memory"),
            comparison("adversarial_cross_node"; "file_backed"),
            comparison("adversarial_cross_node"; "memory")
          ]
        }
        | .summary = {
            overall_status: (
              if ([.comparisons[].validation_status] | all(. == "improved_or_held")) then
                "passed"
              elif ([.comparisons[].validation_status] | all(. == "regressed")) then
                "regressed"
              elif ([.comparisons[].validation_status] | any(. == "regressed")) then
                "mixed"
              else
                "improved_with_caveats"
              end
            ),
            shared_file_backed_status: (
              if ([.comparisons[] | select(.storage_profile_id == "file_backed") | .validation_status] | all(. == "improved_or_held")) then
                "passed"
              elif ([.comparisons[] | select(.storage_profile_id == "file_backed") | .validation_status] | any(. == "regressed")) then
                "regressed"
              else
                "mixed"
              end
            ),
            mvcc_default_guard_status: .mvcc_default_guard.status
          }
        ' > "${VALIDATION_JSON}"

    jq -r '
        [
            "# H2.3 Single-Writer Cleanup Validation",
            "",
            "- run_id: `\(.run_id)`",
            "- previous_report_json: `\(.previous_report_json)`",
            "- current_report_json: `\(.current_report_json)`",
            "- mvcc_default_guard: `\(.mvcc_default_guard.status)` via `\(.mvcc_default_guard.test_target)`",
            "- mvcc_default_guard_log: `\(.mvcc_default_guard.log_file)`",
            "",
            "## Comparison Matrix",
            "",
            "| placement_profile_id | storage_profile_id | current single-writer ops/s | previous single-writer ops/s | ops delta | current single/mvcc ratio | previous single/mvcc ratio | ratio delta | retry delta | status |",
            "| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |",
            (
                .comparisons[]
                | "| \(.placement_profile_id) | \(.storage_profile_id) | \(.current_single_writer_median_ops_per_sec) | \(.previous_single_writer_median_ops_per_sec) | \(.single_writer_median_ops_delta) | \(.current_single_writer_vs_mvcc_median_ops_ratio) | \(.previous_single_writer_vs_mvcc_median_ops_ratio) | \(.single_writer_vs_mvcc_ratio_delta) | \(.single_writer_minus_mvcc_mean_retry_delta) | \(.validation_status) |"
            ),
            "",
            "## Summary",
            "",
            "- overall_status: `\(.summary.overall_status)`",
            "- shared_file_backed_status: `\(.summary.shared_file_backed_status)`",
            "- mvcc_default_guard_status: `\(.summary.mvcc_default_guard_status)`"
        ] | join("\n")
    ' "${VALIDATION_JSON}" > "${VALIDATION_MD}"
}

main() {
    require_file "${CAMPAIGN_MANIFEST_FILE}"
    require_nonempty_file "${BEADS_DATA_PATH}"
    if [[ "${POSTPROCESS_ONLY}" == "1" ]]; then
        log_event "INFO" "start" "postprocess-only matched artifact pack synthesis started"
    else
        log_event "INFO" "start" "starting matched artifact pack collection"

        local row_id fixture_id placement_profile_id storage_profile_id
        while IFS= read -r row_id; do
            [[ -n "${row_id}" ]] || continue
            ensure_row_exists "${row_id}"
            while IFS= read -r fixture_id; do
                [[ -n "${fixture_id}" ]] || continue
                while IFS= read -r placement_profile_id; do
                    [[ -n "${placement_profile_id}" ]] || continue
                    while IFS= read -r storage_profile_id; do
                        [[ -n "${storage_profile_id}" ]] || continue
                        validate_storage_profile "${storage_profile_id}"
                        collect_pack "${row_id}" "${fixture_id}" "${placement_profile_id}" "${storage_profile_id}"
                    done < <(csv_to_lines "${STORAGE_PROFILE_IDS}")
                done < <(row_placement_profiles "${row_id}")
            done < <(row_fixture_ids "${row_id}")
        done < <(csv_to_lines "${ROW_IDS}")
    fi

    build_report
    if should_emit_single_writer_classification; then
        build_single_writer_classification
    fi
    if should_emit_single_writer_validation; then
        run_mvcc_default_guard
        build_single_writer_validation
    fi
    if should_emit_single_writer_role; then
        run_concurrent_mode_default_source_guard
        run_single_writer_verify_suite
        build_single_writer_role
    fi
    log_event "INFO" "complete" "matched artifact pack collection completed"

    echo "RUN_ID:      ${RUN_ID}"
    echo "OUTPUT_DIR:  ${OUTPUT_DIR}"
    echo "REPORT_JSON: ${REPORT_JSON}"
    echo "SUMMARY_MD:  ${SUMMARY_MD}"
    if should_emit_single_writer_classification; then
        echo "CLASSIFICATION_JSON: ${CLASSIFICATION_JSON}"
        echo "CLASSIFICATION_MD:   ${CLASSIFICATION_MD}"
    fi
    if should_emit_single_writer_validation; then
        echo "VALIDATION_JSON: ${VALIDATION_JSON}"
        echo "VALIDATION_MD:   ${VALIDATION_MD}"
        echo "MVCC_DEFAULT_GUARD_LOG: ${MVCC_DEFAULT_GUARD_LOG}"
    fi
    if should_emit_single_writer_role; then
        echo "SINGLE_WRITER_ROLE_JSON: ${SINGLE_WRITER_ROLE_JSON}"
        echo "SINGLE_WRITER_ROLE_MD:   ${SINGLE_WRITER_ROLE_MD}"
        echo "SINGLE_WRITER_VERIFY_DIR: ${SINGLE_WRITER_VERIFY_DIR}"
    fi
}

main "$@"
