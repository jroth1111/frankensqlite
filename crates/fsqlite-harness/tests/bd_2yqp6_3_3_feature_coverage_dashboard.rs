//! Contract tests for bd-2yqp6.3.3 feature coverage accounting.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use fsqlite_harness::feature_coverage_dashboard::{
    DEFAULT_FEATURE_COVERAGE_MANIFEST_PATH, FeatureCoverageRunMetadata, FeatureCoverageStatus,
    ReleaseGateOutcome, build_feature_coverage_dashboard,
};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root should be canonicalizable")
}

#[test]
fn canonical_manifest_builds_machine_checkable_dashboard() {
    let workspace_root = workspace_root();
    let dashboard = build_feature_coverage_dashboard(
        &workspace_root,
        Path::new(DEFAULT_FEATURE_COVERAGE_MANIFEST_PATH),
        FeatureCoverageRunMetadata::deterministic(),
    )
    .expect("canonical feature coverage dashboard should build");

    assert_eq!(dashboard.schema_version, "1.0.0");
    assert_eq!(dashboard.bead_id, "bd-2yqp6.3.3");
    assert_eq!(
        dashboard.source_manifest_path,
        DEFAULT_FEATURE_COVERAGE_MANIFEST_PATH
    );
    assert_eq!(dashboard.source_manifest_sha256.len(), 64);
    assert_eq!(dashboard.coverage_signature_sha256.len(), 64);
    assert_eq!(dashboard.release_gate.outcome, ReleaseGateOutcome::Pass);
    assert_eq!(dashboard.release_gate.missing_feature_count, 0);
    assert_eq!(dashboard.required_feature_count, 25);
    assert_eq!(dashboard.families.len(), 3);
    assert!(
        dashboard
            .features
            .iter()
            .any(|row| row.status == FeatureCoverageStatus::Partial),
        "canonical dashboard should expose seed-only partial coverage"
    );
}

#[test]
fn canonical_dashboard_accounts_by_feature_family() {
    let workspace_root = workspace_root();
    let dashboard = build_feature_coverage_dashboard(
        &workspace_root,
        Path::new(DEFAULT_FEATURE_COVERAGE_MANIFEST_PATH),
        FeatureCoverageRunMetadata::deterministic(),
    )
    .expect("canonical feature coverage dashboard should build");

    let by_family = dashboard
        .families
        .iter()
        .map(|family| (family.family.as_str(), family))
        .collect::<BTreeMap<_, _>>();

    assert_eq!(by_family["FUN"].total_features, 5);
    assert_eq!(by_family["PGM"].total_features, 4);
    assert_eq!(by_family["SQL"].total_features, 16);
    assert_eq!(by_family["FUN"].none_count, 0);
    assert_eq!(by_family["PGM"].none_count, 0);
    assert_eq!(by_family["SQL"].none_count, 0);
    assert!(by_family["SQL"].partial_count > 0);
    assert!(by_family["SQL"].full_count > by_family["SQL"].partial_count);
}
