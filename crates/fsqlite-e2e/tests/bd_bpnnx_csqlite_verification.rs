//! bd-bpnnx: Stock C SQLite verification helper integration tests.
//!
//! Validates that `verify_with_c_sqlite` correctly produces structured
//! VerifyReports across all tiers: clean DB, corrupted DB, unreadable DB,
//! WAL-truncated DB, multi-table DB, and concurrent-write artifact.

use std::path::Path;

use fsqlite_e2e::verify_csqlite::{CheckResult, VerifyError, VerifyReport, verify_with_c_sqlite};
use tempfile::TempDir;

fn fresh_dir() -> TempDir {
    tempfile::tempdir().expect("failed to create temp dir")
}

fn create_populated_db(path: &Path) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA page_size=4096;
         CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, email TEXT);
         CREATE TABLE orders (id INTEGER PRIMARY KEY, user_id INTEGER REFERENCES users(id), amount REAL);
         CREATE INDEX idx_orders_user ON orders(user_id);
         INSERT INTO users VALUES (1, 'Alice', 'alice@test.com');
         INSERT INTO users VALUES (2, 'Bob', 'bob@test.com');
         INSERT INTO users VALUES (3, 'Carol', 'carol@test.com');
         INSERT INTO orders VALUES (100, 1, 49.99);
         INSERT INTO orders VALUES (101, 2, 99.99);
         INSERT INTO orders VALUES (102, 1, 29.99);
         INSERT INTO orders VALUES (103, 3, 149.99);",
    )
    .unwrap();
}

// ─── Phase 1: basic pass/fail ──────────────────────────────────────────

#[test]
fn t1_clean_populated_db_passes_all_checks() {
    let dir = fresh_dir();
    let db_path = dir.path().join("clean.db");
    create_populated_db(&db_path);

    let report = verify_with_c_sqlite(&db_path).unwrap();
    assert!(report.ok, "clean DB must pass: {report}");
    assert!(report.quick_check.is_pass());
    assert!(report.integrity_check.is_pass());
    assert_eq!(report.table_count, 2);
    assert!(report.page_count > 0);
    assert_eq!(report.page_size, 4096);
    assert!(report.wal_mode);
}

#[test]
fn t2_nonexistent_file_returns_file_not_found_error() {
    let result = verify_with_c_sqlite(Path::new("/tmp/bpnnx_does_not_exist_42.db"));
    assert!(result.is_err());
    match result.unwrap_err() {
        VerifyError::FileNotFound(p) => assert!(p.contains("bpnnx_does_not_exist")),
        other => panic!("expected FileNotFound, got: {other}"),
    }
}

#[test]
fn t3_garbage_file_produces_fail_or_skipped_report() {
    let dir = fresh_dir();
    let db_path = dir.path().join("garbage.db");
    std::fs::write(&db_path, b"GARBAGE DATA NOT A SQLITE DB").unwrap();

    let report = verify_with_c_sqlite(&db_path).unwrap();
    assert!(!report.ok, "garbage file must not pass");
    let any_non_pass = !report.quick_check.is_pass() || !report.integrity_check.is_pass();
    assert!(any_non_pass, "at least one check must be non-pass");
}

// ─── Phase 2: WAL-specific scenarios ───────────────────────────────────

#[test]
fn t4_wal_truncated_db_still_readable() {
    let dir = fresh_dir();
    let db_path = dir.path().join("wal_trunc.db");
    create_populated_db(&db_path);

    let wal_path = dir.path().join("wal_trunc.db-wal");
    if wal_path.exists() {
        std::fs::write(&wal_path, b"").unwrap();
    }

    let report = verify_with_c_sqlite(&db_path).unwrap();
    assert!(
        report.quick_check.is_pass() || report.quick_check.is_skipped(),
        "truncated WAL should still allow quick_check: {}",
        report.quick_check
    );
}

#[test]
fn t5_non_wal_journal_mode_skips_checkpoint() {
    let dir = fresh_dir();
    let db_path = dir.path().join("delete_mode.db");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute_batch(
        "PRAGMA journal_mode=DELETE;
         CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT);
         INSERT INTO t VALUES (1, 'test');",
    )
    .unwrap();
    drop(conn);

    let report = verify_with_c_sqlite(&db_path).unwrap();
    assert!(report.ok);
    assert!(!report.wal_mode);
}

// ─── Phase 3: metadata correctness ────────────────────────────────────

#[test]
fn t6_metadata_reflects_actual_schema() {
    let dir = fresh_dir();
    let db_path = dir.path().join("meta.db");
    create_populated_db(&db_path);

    let report = verify_with_c_sqlite(&db_path).unwrap();
    assert_eq!(report.table_count, 2, "should see users + orders");
    assert!(
        report.schema_version_u32 > 0,
        "schema_version must be positive"
    );
    assert!(report.page_size >= 512, "page_size must be at least 512");
}

#[test]
fn t7_empty_db_has_zero_tables() {
    let dir = fresh_dir();
    let db_path = dir.path().join("empty.db");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute_batch("PRAGMA journal_mode=WAL;").unwrap();
    drop(conn);

    let report = verify_with_c_sqlite(&db_path).unwrap();
    assert!(report.ok);
    assert_eq!(report.table_count, 0);
}

// ─── Phase 4: serialization round-trip ─────────────────────────────────

#[test]
fn t8_json_round_trip_preserves_all_fields() {
    let dir = fresh_dir();
    let db_path = dir.path().join("roundtrip.db");
    create_populated_db(&db_path);

    let report = verify_with_c_sqlite(&db_path).unwrap();
    let json = serde_json::to_string_pretty(&report).unwrap();

    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["ok"], true);
    assert!(parsed["quick_check"].is_object());
    assert!(parsed["integrity_check"].is_object());
    assert!(parsed["timings"]["total_ms"].is_number());
    assert!(parsed["page_count"].is_number());
    assert!(parsed["table_count"].is_number());

    let roundtrip: VerifyReport = serde_json::from_str(&json).unwrap();
    assert_eq!(roundtrip.ok, report.ok);
    assert_eq!(roundtrip.page_count, report.page_count);
    assert_eq!(roundtrip.page_size, report.page_size);
    assert_eq!(roundtrip.table_count, report.table_count);
    assert_eq!(roundtrip.schema_version_u32, report.schema_version_u32);
}

#[test]
fn t9_check_result_enum_serializes_correctly() {
    let pass_json = serde_json::to_string(&CheckResult::Pass).unwrap();
    assert!(pass_json.contains("Pass"));

    let fail_json = serde_json::to_string(&CheckResult::Fail("bad page".to_owned())).unwrap();
    assert!(fail_json.contains("Fail"));
    assert!(fail_json.contains("bad page"));

    let skip_json = serde_json::to_string(&CheckResult::Skipped("no WAL".to_owned())).unwrap();
    assert!(skip_json.contains("Skipped"));

    let rt: CheckResult = serde_json::from_str(&pass_json).unwrap();
    assert!(rt.is_pass());
    let rt: CheckResult = serde_json::from_str(&fail_json).unwrap();
    assert!(rt.is_fail());
    let rt: CheckResult = serde_json::from_str(&skip_json).unwrap();
    assert!(rt.is_skipped());
}

// ─── Phase 5: concurrent-write artifact verification ───────────────────

#[test]
fn t10_multi_writer_artifact_verified() {
    let dir = fresh_dir();
    let db_path = dir.path().join("concurrent.db");

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")
        .unwrap();

    for i in 0..4 {
        conn.execute_batch(&format!(
            "CREATE TABLE writer_{i} (id INTEGER PRIMARY KEY, val TEXT);",
        ))
        .unwrap();
        for j in 0..100 {
            conn.execute(
                &format!("INSERT INTO writer_{i} VALUES (?, ?)"),
                rusqlite::params![j, format!("w{i}_row{j}")],
            )
            .unwrap();
        }
    }
    drop(conn);

    let report = verify_with_c_sqlite(&db_path).unwrap();
    assert!(report.ok, "concurrent-style DB must pass: {report}");
    assert_eq!(report.table_count, 4);
    assert!(report.page_count > 4);
}

// ─── Phase 6: timing sanity ───────────────────────────────────────────

#[test]
fn t11_timings_are_non_negative() {
    let dir = fresh_dir();
    let db_path = dir.path().join("timing.db");
    create_populated_db(&db_path);

    let report = verify_with_c_sqlite(&db_path).unwrap();
    assert!(report.timings.open_ms >= 0.0);
    assert!(report.timings.quick_check_ms >= 0.0);
    assert!(report.timings.integrity_check_ms >= 0.0);
    assert!(report.timings.metadata_ms >= 0.0);
    assert!(report.timings.total_ms >= 0.0);
    assert!(report.timings.total_ms >= report.timings.open_ms);
}

// ─── Phase 7: Display trait ───────────────────────────────────────────

#[test]
fn t12_display_pass_contains_key_info() {
    let dir = fresh_dir();
    let db_path = dir.path().join("disp.db");
    create_populated_db(&db_path);

    let report = verify_with_c_sqlite(&db_path).unwrap();
    let display = format!("{report}");
    assert!(display.contains("VERIFY PASSED"), "display: {display}");
    assert!(display.contains("2 tables"), "display: {display}");
}

#[test]
fn t13_display_fail_shows_check_results() {
    let dir = fresh_dir();
    let db_path = dir.path().join("disp_fail.db");
    std::fs::write(&db_path, b"not sqlite").unwrap();

    let report = verify_with_c_sqlite(&db_path).unwrap();
    let display = format!("{report}");
    assert!(
        display.contains("VERIFY FAILED") || display.contains("Skipped"),
        "display: {display}"
    );
}
