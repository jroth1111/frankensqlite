//! bd-zywqc.2: Concurrent-write performance regression gate tests.
//!
//! Validates the regression detection logic and baseline management
//! without requiring full benchmark runs. Uses synthetic baseline/current
//! data to verify thresholds are enforced correctly.

use serde::{Deserialize, Serialize};
use tempfile::TempDir;

const BEAD_ID: &str = "bd-zywqc.2";
const SINGLE_WRITER_THRESHOLD: f64 = 0.05;
const EIGHT_WRITER_THRESHOLD: f64 = 0.0;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RegressionDelta {
    threads: usize,
    current_wps: f64,
    baseline_wps: f64,
    delta_pct: f64,
    threshold_pct: f64,
    status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RegressionResult {
    bead_id: String,
    verdict: String,
    deltas: Vec<RegressionDelta>,
}

fn compute_delta(threads: usize, current_wps: f64, baseline_wps: f64) -> RegressionDelta {
    let threshold = if threads == 1 {
        SINGLE_WRITER_THRESHOLD
    } else {
        EIGHT_WRITER_THRESHOLD
    };
    let delta_pct = if baseline_wps > 0.0 {
        (current_wps - baseline_wps) / baseline_wps
    } else {
        0.0
    };
    let status = if delta_pct < -threshold {
        "regression".to_owned()
    } else {
        "ok".to_owned()
    };
    RegressionDelta {
        threads,
        current_wps,
        baseline_wps,
        delta_pct: (delta_pct * 10000.0).round() / 100.0,
        threshold_pct: (-threshold * 10000.0).round() / 100.0,
        status,
    }
}

fn gate_verdict(deltas: &[RegressionDelta]) -> &'static str {
    if deltas.iter().any(|d| d.status == "regression") {
        "failed"
    } else {
        "passed"
    }
}

fn fresh_dir() -> TempDir {
    tempfile::tempdir().expect("tempdir")
}

// ─── Threshold mechanics ────────────────────────────────────────────

#[test]
fn t1_no_regression_passes() {
    let d1 = compute_delta(1, 50000.0, 50000.0);
    let d8 = compute_delta(8, 100000.0, 100000.0);
    assert_eq!(d1.status, "ok");
    assert_eq!(d8.status, "ok");
    assert_eq!(gate_verdict(&[d1, d8]), "passed");
}

#[test]
fn t2_single_writer_4pct_regression_passes() {
    let d1 = compute_delta(1, 48000.0, 50000.0); // -4%, under 5% threshold
    assert_eq!(d1.status, "ok");
}

#[test]
fn t3_single_writer_6pct_regression_fails() {
    let d1 = compute_delta(1, 47000.0, 50000.0); // -6%, over 5% threshold
    assert_eq!(d1.status, "regression");
    assert_eq!(gate_verdict(&[d1]), "failed");
}

#[test]
fn t4_eight_writer_any_regression_fails() {
    let d8 = compute_delta(8, 99999.0, 100000.0); // -0.001%, any regression fails
    assert_eq!(d8.status, "regression");
    assert_eq!(gate_verdict(&[d8]), "failed");
}

#[test]
fn t5_improvement_always_passes() {
    let d1 = compute_delta(1, 60000.0, 50000.0); // +20%
    let d8 = compute_delta(8, 150000.0, 100000.0); // +50%
    assert_eq!(d1.status, "ok");
    assert_eq!(d8.status, "ok");
    assert_eq!(gate_verdict(&[d1, d8]), "passed");
}

#[test]
fn t6_mixed_pass_fail() {
    let d1 = compute_delta(1, 55000.0, 50000.0); // +10% improvement
    let d8 = compute_delta(8, 95000.0, 100000.0); // -5% regression
    assert_eq!(d1.status, "ok");
    assert_eq!(d8.status, "regression");
    assert_eq!(gate_verdict(&[d1, d8]), "failed");
}

// ─── Baseline JSON format ───────────────────────────────────────────

#[test]
fn t7_baseline_json_round_trip() {
    let result = RegressionResult {
        bead_id: BEAD_ID.to_owned(),
        verdict: "passed".to_owned(),
        deltas: vec![
            compute_delta(1, 50000.0, 48000.0),
            compute_delta(8, 200000.0, 190000.0),
        ],
    };

    let json = serde_json::to_string_pretty(&result).unwrap();
    let parsed: RegressionResult = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.verdict, "passed");
    assert_eq!(parsed.deltas.len(), 2);
    assert_eq!(parsed.deltas[0].threads, 1);
    assert_eq!(parsed.deltas[1].threads, 8);
}

#[test]
fn t8_baseline_file_write_read() {
    let dir = fresh_dir();
    let path = dir.path().join("baseline.json");

    let result = RegressionResult {
        bead_id: BEAD_ID.to_owned(),
        verdict: "passed".to_owned(),
        deltas: vec![compute_delta(1, 50000.0, 50000.0)],
    };

    let json = serde_json::to_string_pretty(&result).unwrap();
    std::fs::write(&path, &json).unwrap();

    let read_back = std::fs::read_to_string(&path).unwrap();
    let parsed: RegressionResult = serde_json::from_str(&read_back).unwrap();
    assert_eq!(parsed.bead_id, BEAD_ID);
}

// ─── Synthetic regression detection (AC2) ───────────────────────────

#[test]
fn t9_synthetic_regression_detected() {
    // Simulate what happens when someone adds an artificial sleep:
    // baseline: 50000 wps -> current: 25000 wps (50% regression)
    let d1 = compute_delta(1, 25000.0, 50000.0);
    assert_eq!(d1.status, "regression");
    assert!(d1.delta_pct < -5.0, "delta must be < -5%: {}", d1.delta_pct);
    assert_eq!(gate_verdict(&[d1]), "failed");
}

#[test]
fn t10_zero_baseline_handled() {
    let d = compute_delta(1, 50000.0, 0.0);
    assert_eq!(d.status, "ok");
    assert_eq!(d.delta_pct, 0.0);
}

// ─── Edge: exact threshold boundary ─────────────────────────────────

#[test]
fn t11_exact_5pct_single_writer_passes() {
    // Exactly -5%: 47500 / 50000 = 0.95, delta = -0.05
    let d = compute_delta(1, 47500.0, 50000.0);
    // -5% is the threshold, not exceeded (< not <=)
    assert_eq!(d.status, "ok");
}

#[test]
fn t12_just_over_5pct_single_writer_fails() {
    // -5.01%: 47495 / 50000 = 0.9499, delta = -0.0501
    let d = compute_delta(1, 47495.0, 50000.0);
    assert_eq!(d.status, "regression");
}
