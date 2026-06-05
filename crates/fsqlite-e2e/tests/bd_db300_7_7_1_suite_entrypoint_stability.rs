//! Suite entrypoint stability validation (bd-db300.7.7.1).
//!
//! Proves that the `realdb-e2e verify-suite` one-command entrypoints produce
//! stable, complete, and parseable artifacts across all mode/profile/depth
//! combinations. Each test invokes the binary as a subprocess and validates
//! the structured output.
//!
//! ## Scenarios
//!
//! | ID | Name | Verified property |
//! |----|------|-------------------|
//! | S1 | help_exits_cleanly | verify-suite --help exits 0 |
//! | S2 | all_modes_produce_valid_package | Each of 3 modes → valid package JSON |
//! | S3 | placement_profiles_stable | 3 placement profiles produce distinct packages |
//! | S4 | quick_vs_full_depth_honored | Depth selector affects package content |
//! | S5 | shadow_modes_coverage | All 4 shadow modes accepted |
//! | S6 | activation_regimes_coverage | All 5 activation regimes accepted |
//! | S7 | structured_log_fields_complete | JSONL log has all required fields |
//! | S8 | kill_switch_tripped_emits_counterexample | Tripped state triggers bundle |
//! | S9 | invalid_selectors_fail_fast | Unknown mode/profile/regime → exit 2 |
//! | S10 | rerun_entrypoints_present | Package includes rerun + focused rerun commands |
//!
//! ## Run
//!
//! ```sh
//! cargo test -p fsqlite-e2e --test bd_db300_7_7_1_suite_entrypoint_stability -- --nocapture
//! ```

#![allow(clippy::too_many_lines)]

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;

use serde_json::{Value, json};

const BEAD_ID: &str = "bd-db300.7.7.1";

fn emit_log(scenario_id: &str, phase: &str, data: serde_json::Value) {
    eprintln!(
        "SUITE_ENTRYPOINT_STABILITY:{}",
        json!({
            "bead_id": BEAD_ID,
            "scenario_id": scenario_id,
            "phase": phase,
            "data": data,
        })
    );
}

fn realdb_e2e_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_realdb-e2e"))
}

fn run_verify_suite(args: &[&str]) -> (i32, String, String) {
    let binary = realdb_e2e_binary();
    let mut cmd = Command::new(&binary);
    cmd.arg("verify-suite");
    for arg in args {
        cmd.arg(arg);
    }
    let output = cmd
        .output()
        .unwrap_or_else(|e| panic!("failed to run {} verify-suite: {e}", binary.display()));
    let code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    (code, stdout, stderr)
}

fn run_with_output_dir(args: &[&str], output_dir: &std::path::Path) -> (i32, String, String) {
    let mut full_args: Vec<&str> = args.to_vec();
    let dir_str = output_dir.to_str().expect("output dir is valid UTF-8");
    full_args.push("--output-dir");
    full_args.push(dir_str);
    run_verify_suite(&full_args)
}

fn load_package_json(output_dir: &std::path::Path) -> Value {
    let path = output_dir.join("suite_package.json");
    let raw =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

fn load_log_jsonl(output_dir: &std::path::Path) -> Vec<Value> {
    let path = output_dir.join("logs/verify_suite.jsonl");
    let raw =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("parse JSONL: {e}")))
        .collect()
}

// ── S1: Help exits cleanly ────────────────────────────────────────────

#[test]
fn s1_help_exits_cleanly() {
    let binary = realdb_e2e_binary();
    let output = Command::new(&binary)
        .args(["verify-suite", "--help"])
        .output()
        .expect("run binary");
    let code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(code, 0, "help exits cleanly");
    assert!(
        stdout.contains("verify-suite"),
        "help output mentions verify-suite"
    );

    emit_log(
        "S1",
        "result",
        json!({
            "exit_code": code,
            "mentions_verify_suite": true,
        }),
    );
}

// ── S2: All modes produce valid package ───────────────────────────────

#[test]
fn s2_all_modes_produce_valid_package() {
    let modes = ["sqlite_reference", "fsqlite_mvcc", "fsqlite_single_writer"];

    for mode in &modes {
        let tempdir = tempfile::tempdir().expect("create tempdir");
        let output_dir = tempdir.path().join(format!("vs_{mode}"));

        let (code, _stdout, stderr) = run_with_output_dir(
            &["--mode", mode, "--verification-depth", "quick"],
            &output_dir,
        );
        assert_eq!(
            code, 0,
            "verify-suite --mode {mode} should succeed; stderr: {stderr}"
        );

        let package = load_package_json(&output_dir);
        assert_eq!(
            package["mode"].as_str(),
            Some(*mode),
            "package mode matches for {mode}"
        );
        assert!(
            package["schema_version"].as_str().is_some(),
            "schema_version present for {mode}"
        );
        assert!(
            package["trace_id"].as_str().is_some(),
            "trace_id present for {mode}"
        );
        assert!(
            package["scenario_id"].as_str().is_some(),
            "scenario_id present for {mode}"
        );
        assert!(
            package["suite_id"].as_str().is_some(),
            "suite_id present for {mode}"
        );
        assert!(
            package["rerun_entrypoint"].as_str().is_some(),
            "rerun_entrypoint present for {mode}"
        );
    }

    emit_log(
        "S2",
        "result",
        json!({
            "modes_tested": modes,
            "all_valid": true,
        }),
    );
}

// ── S3: Placement profiles stable ─────────────────────────────────────

#[test]
fn s3_placement_profiles_stable() {
    let profiles = [
        "baseline_unpinned",
        "recommended_pinned",
        "adversarial_cross_node",
    ];

    let mut profile_ids = Vec::new();
    for profile in &profiles {
        let tempdir = tempfile::tempdir().expect("create tempdir");
        let output_dir = tempdir.path().join(format!("vs_{profile}"));

        let (code, _stdout, stderr) =
            run_with_output_dir(&["--placement-profile", profile], &output_dir);
        assert_eq!(
            code, 0,
            "placement-profile {profile} should succeed; stderr: {stderr}"
        );

        let package = load_package_json(&output_dir);
        let pid = package["placement_profile_id"]
            .as_str()
            .expect("placement_profile_id present")
            .to_owned();
        assert_eq!(pid, *profile, "profile ID matches");
        profile_ids.push(pid);
    }

    let unique: HashSet<&str> = profile_ids.iter().map(String::as_str).collect();
    assert_eq!(
        unique.len(),
        profiles.len(),
        "all placement profiles produce distinct IDs"
    );

    emit_log(
        "S3",
        "result",
        json!({
            "profiles_tested": profiles,
            "all_distinct": true,
        }),
    );
}

// ── S4: Quick vs full depth honored ───────────────────────────────────

#[test]
fn s4_quick_vs_full_depth_honored() {
    for depth in &["quick", "full"] {
        let tempdir = tempfile::tempdir().expect("create tempdir");
        let output_dir = tempdir.path().join(format!("vs_{depth}"));

        let (code, _stdout, stderr) =
            run_with_output_dir(&["--verification-depth", depth], &output_dir);
        assert_eq!(
            code, 0,
            "verification-depth {depth} should succeed; stderr: {stderr}"
        );

        let package = load_package_json(&output_dir);
        assert_eq!(
            package["verification_depth"].as_str(),
            Some(*depth),
            "depth {depth} recorded in package"
        );
    }

    emit_log(
        "S4",
        "result",
        json!({
            "depths_tested": ["quick", "full"],
            "both_honored": true,
        }),
    );
}

// ── S5: Shadow modes coverage ─────────────────────────────────────────

#[test]
fn s5_shadow_modes_coverage() {
    let shadow_modes = ["off", "forced", "sampled", "shadow_canary"];

    for shadow_mode in &shadow_modes {
        let tempdir = tempfile::tempdir().expect("create tempdir");
        let output_dir = tempdir.path().join(format!("vs_shadow_{shadow_mode}"));

        let (code, _stdout, stderr) =
            run_with_output_dir(&["--shadow-mode", shadow_mode], &output_dir);
        assert_eq!(
            code, 0,
            "shadow-mode {shadow_mode} should succeed; stderr: {stderr}"
        );

        let package = load_package_json(&output_dir);
        assert_eq!(
            package["shadow_mode"].as_str(),
            Some(*shadow_mode),
            "shadow_mode recorded for {shadow_mode}"
        );
    }

    emit_log(
        "S5",
        "result",
        json!({
            "shadow_modes_tested": shadow_modes,
            "all_accepted": true,
        }),
    );
}

// ── S6: Activation regimes coverage ───────────────────────────────────

#[test]
fn s6_activation_regimes_coverage() {
    let regimes = [
        "red_path_correctness",
        "low_concurrency_fixed_cost",
        "mid_concurrency_scaling",
        "many_core_parallel",
        "hostile_or_unclassified",
    ];

    for regime in &regimes {
        let tempdir = tempfile::tempdir().expect("create tempdir");
        let output_dir = tempdir.path().join(format!("vs_regime_{regime}"));

        let (code, _stdout, stderr) =
            run_with_output_dir(&["--activation-regime", regime], &output_dir);
        assert_eq!(
            code, 0,
            "activation-regime {regime} should succeed; stderr: {stderr}"
        );

        let package = load_package_json(&output_dir);
        assert_eq!(
            package["activation_regime"].as_str(),
            Some(*regime),
            "activation_regime recorded for {regime}"
        );
    }

    emit_log(
        "S6",
        "result",
        json!({
            "regimes_tested": regimes,
            "all_accepted": true,
        }),
    );
}

// ── S7: Structured log fields complete ────────────────────────────────

#[test]
fn s7_structured_log_fields_complete() {
    let tempdir = tempfile::tempdir().expect("create tempdir");
    let output_dir = tempdir.path().join("vs_log_check");

    let (code, _stdout, stderr) = run_with_output_dir(
        &[
            "--mode",
            "fsqlite_mvcc",
            "--placement-profile",
            "recommended_pinned",
            "--verification-depth",
            "full",
            "--activation-regime",
            "mid_concurrency_scaling",
            "--shadow-mode",
            "forced",
            "--db",
            "frankensqlite",
            "--workload",
            "mixed_read_write",
            "--concurrency",
            "4,8",
        ],
        &output_dir,
    );
    assert_eq!(
        code, 0,
        "full-featured verify-suite should succeed; stderr: {stderr}"
    );

    let log_entries = load_log_jsonl(&output_dir);
    assert!(
        !log_entries.is_empty(),
        "structured log should have at least one entry"
    );

    let required_fields = [
        "trace_id",
        "scenario_id",
        "suite_id",
        "mode",
        "placement_profile_id",
        "verification_depth",
        "activation_regime",
        "shadow_mode",
        "artifact_root",
        "rerun_entrypoint",
        "pass_fail_signature",
    ];

    for entry in &log_entries {
        let mut missing = Vec::new();
        for field in &required_fields {
            if entry.get(*field).is_none() {
                missing.push(*field);
            }
        }
        assert!(missing.is_empty(), "log entry missing fields: {missing:?}");
    }

    emit_log(
        "S7",
        "result",
        json!({
            "log_entries": log_entries.len(),
            "required_fields_checked": required_fields.len(),
            "all_present": true,
        }),
    );
}

// ── S8: Kill switch tripped emits counterexample ──────────────────────

#[test]
fn s8_kill_switch_tripped_emits_counterexample() {
    let tempdir = tempfile::tempdir().expect("create tempdir");
    let output_dir = tempdir.path().join("vs_tripped");

    let (code, _stdout, stderr) = run_with_output_dir(
        &[
            "--shadow-mode",
            "forced",
            "--shadow-verdict",
            "diverged",
            "--kill-switch-state",
            "tripped",
            "--first-failure-diagnostics",
            "test: state hash mismatch on page 42",
        ],
        &output_dir,
    );
    assert_eq!(
        code, 0,
        "tripped kill-switch should succeed; stderr: {stderr}"
    );

    let package = load_package_json(&output_dir);
    assert_eq!(
        package["kill_switch_state"].as_str(),
        Some("tripped"),
        "kill_switch_state recorded"
    );
    assert_eq!(
        package["shadow_verdict"].as_str(),
        Some("diverged"),
        "shadow_verdict recorded"
    );

    let counterexample_file = output_dir.join("counterexamples/shadow_counterexample_bundle.json");
    assert!(
        counterexample_file.exists(),
        "counterexample bundle file exists at {:?}",
        counterexample_file
    );

    assert!(
        package["pass_fail_signature"]
            .as_str()
            .is_some_and(|sig| sig.contains("fail")),
        "pass_fail_signature indicates failure: {:?}",
        package["pass_fail_signature"]
    );

    emit_log(
        "S8",
        "result",
        json!({
            "kill_switch_state": "tripped",
            "counterexample_present": true,
            "pass_fail_contains_fail": true,
        }),
    );
}

// ── S9: Invalid selectors fail fast ───────────────────────────────────

#[test]
fn s9_invalid_selectors_fail_fast() {
    let invalid_cases: &[(&str, &str)] = &[
        ("--mode", "bogus_mode"),
        ("--placement-profile", "nonexistent_profile"),
        ("--activation-regime", "unknown_regime"),
        ("--shadow-mode", "invalid_shadow"),
        ("--verification-depth", "medium"),
        ("--execution-context", "staging"),
    ];

    for (flag, value) in invalid_cases {
        let tempdir = tempfile::tempdir().expect("create tempdir");
        let output_dir = tempdir.path().join("vs_invalid");

        let (code, _stdout, _stderr) = run_with_output_dir(&[flag, value], &output_dir);
        assert_eq!(code, 2, "invalid {flag} {value} should exit with code 2");
    }

    emit_log(
        "S9",
        "result",
        json!({
            "invalid_cases_tested": invalid_cases.len(),
            "all_rejected_with_code_2": true,
        }),
    );
}

// ── S10: Rerun entrypoints present ────────────────────────────────────

#[test]
fn s10_rerun_entrypoints_present() {
    let tempdir = tempfile::tempdir().expect("create tempdir");
    let output_dir = tempdir.path().join("vs_rerun");

    let (code, _stdout, stderr) = run_with_output_dir(
        &["--mode", "fsqlite_mvcc", "--verification-depth", "quick"],
        &output_dir,
    );
    assert_eq!(code, 0, "verify-suite should succeed; stderr: {stderr}");

    let package = load_package_json(&output_dir);

    let rerun = package["rerun_entrypoint"]
        .as_str()
        .expect("rerun_entrypoint present");
    assert!(
        rerun.contains("realdb-e2e"),
        "rerun entrypoint references realdb-e2e"
    );

    let focused = package["focused_rerun_entrypoint"]
        .as_str()
        .expect("focused_rerun_entrypoint present");
    assert!(
        focused.contains("--repeat 1"),
        "focused rerun uses --repeat 1"
    );

    let contract = package["contract_entrypoint"]
        .as_str()
        .expect("contract_entrypoint present");
    assert!(!contract.is_empty(), "contract entrypoint is non-empty");

    let local = package["local_entrypoint"]
        .as_str()
        .expect("local_entrypoint present");
    assert!(!local.is_empty(), "local entrypoint is non-empty");

    let ci = package["ci_entrypoint"]
        .as_str()
        .expect("ci_entrypoint present");
    assert!(!ci.is_empty(), "CI entrypoint is non-empty");

    let rerun_script = output_dir.join("rerun_entrypoint.sh");
    let focused_rerun_script = output_dir.join("focused_rerun_entrypoint.sh");
    assert!(rerun_script.exists(), "rerun_entrypoint.sh script exists");
    assert!(
        focused_rerun_script.exists(),
        "focused_rerun_entrypoint.sh script exists"
    );

    emit_log(
        "S10",
        "result",
        json!({
            "rerun_present": true,
            "focused_rerun_present": true,
            "contract_entrypoint_present": true,
            "local_entrypoint_present": true,
            "ci_entrypoint_present": true,
            "rerun_script_exists": true,
            "focused_rerun_script_exists": true,
        }),
    );
}
