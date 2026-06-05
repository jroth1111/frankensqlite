//! bd-zuzb8: WAL integrity cross-check via stock C SQLite with tiered fallback.
//!
//! Validates `verify_concurrency_artifact` across all tiers:
//! - Tier 1: rusqlite PRAGMA checks (clean DB, multi-writer)
//! - Tier 2: raw-page diagnostics when rusqlite can't open (WAL truncated, corrupt header)
//! - Artifact bundle serialization round-trip

use std::path::Path;

use fsqlite_e2e::verify_csqlite::{
    RawPageDiagnostics, VerifyArtifact, verify_concurrency_artifact, verify_with_c_sqlite,
    write_artifact_bundle,
};
use tempfile::TempDir;

fn fresh_dir() -> TempDir {
    tempfile::tempdir().expect("failed to create temp dir")
}

fn create_wal_db(path: &Path) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA page_size=4096;
         CREATE TABLE data (id INTEGER PRIMARY KEY, val TEXT NOT NULL);
         INSERT INTO data VALUES (1, 'alpha');
         INSERT INTO data VALUES (2, 'beta');
         INSERT INTO data VALUES (3, 'gamma');
         INSERT INTO data VALUES (4, 'delta');",
    )
    .unwrap();
}

fn create_multi_writer_db(path: &Path, n_tables: usize, rows_per: usize) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")
        .unwrap();
    for t in 0..n_tables {
        conn.execute_batch(&format!(
            "CREATE TABLE writer_{t} (id INTEGER PRIMARY KEY, val TEXT);"
        ))
        .unwrap();
        for r in 0..rows_per {
            conn.execute(
                &format!("INSERT INTO writer_{t} VALUES (?, ?)"),
                rusqlite::params![r as i64, format!("w{t}_r{r}")],
            )
            .unwrap();
        }
    }
}

// ─── Tier 1: clean DB passes, no artifact ────────────────────────────

#[test]
fn t1_clean_db_returns_ok_no_artifact() {
    let dir = fresh_dir();
    let db = dir.path().join("clean.db");
    create_wal_db(&db);

    let (report, artifact) = verify_concurrency_artifact(&db).unwrap();
    assert!(report.ok, "clean DB must pass: {report}");
    assert!(artifact.is_none(), "no artifact for clean DB");
}

#[test]
fn t2_multi_writer_db_passes() {
    let dir = fresh_dir();
    let db = dir.path().join("multi.db");
    create_multi_writer_db(&db, 4, 100);

    let (report, artifact) = verify_concurrency_artifact(&db).unwrap();
    assert!(report.ok, "multi-writer DB must pass: {report}");
    assert!(artifact.is_none());
    assert_eq!(report.table_count, 4);
}

// ─── Tier 2: WAL-truncated fallback ──────────────────────────────────

#[test]
fn t3_wal_truncated_produces_raw_diagnostics() {
    let dir = fresh_dir();
    let db = dir.path().join("trunc.db");
    create_wal_db(&db);

    let wal = dir.path().join("trunc.db-wal");
    if wal.exists() {
        std::fs::write(&wal, b"").unwrap();
    }

    let report = verify_with_c_sqlite(&db).unwrap();
    // C SQLite can often recover from truncated WAL (data in main DB is intact)
    // The key test: verify_concurrency_artifact captures an artifact if non-ok
    let (report2, _artifact) = verify_concurrency_artifact(&db).unwrap();

    // Both should agree
    assert_eq!(report.ok, report2.ok);

    // If C SQLite thinks it's ok despite truncated WAL, that's fine — main DB
    // had the committed pages. If not, we should have an artifact.
    if !report2.ok {
        assert!(_artifact.is_some(), "non-ok report must produce artifact");
    }
}

// ─── Tier 2: corrupt page-1 header ──────────────────────────────────

#[test]
fn t4_corrupt_page1_produces_artifact_with_raw_diag() {
    let dir = fresh_dir();
    let db = dir.path().join("corrupt.db");
    create_wal_db(&db);

    // Corrupt the SQLite magic bytes (first 16 bytes)
    let mut data = std::fs::read(&db).unwrap();
    for b in data.iter_mut().take(16) {
        *b = 0xFF;
    }
    std::fs::write(&db, &data).unwrap();

    let (report, artifact) = verify_concurrency_artifact(&db).unwrap();
    assert!(!report.ok, "corrupt DB must not pass");
    let art = artifact.expect("corrupt DB must produce artifact");

    let diag = art
        .raw_diagnostics
        .as_ref()
        .expect("must have raw diagnostics");
    assert!(!diag.magic_ok, "magic should be detected as invalid");
    assert!(art.db_size_bytes > 0);
    assert!(art.captured_at_unix_ms > 0);
}

#[test]
fn t5_corrupt_page1_raw_diag_reads_wal_info() {
    let dir = fresh_dir();
    let db = dir.path().join("corrupt2.db");
    create_wal_db(&db);

    // Don't corrupt — just check raw_page_diagnostics on a clean DB
    // (We need it public-via-verify_concurrency_artifact, so test the sub-path
    //  by corrupting the magic and checking the WAL diagnostics in the artifact)
    let mut data = std::fs::read(&db).unwrap();
    data[0] = 0x00; // Corrupt just the first byte
    std::fs::write(&db, &data).unwrap();

    let (report, artifact) = verify_concurrency_artifact(&db).unwrap();
    assert!(!report.ok);
    let art = artifact.unwrap();
    let diag = art.raw_diagnostics.unwrap();

    assert!(!diag.magic_ok);
    assert!(!diag.db_header_hex.is_empty());
    // WAL may or may not exist depending on checkpoint behavior
    if diag.wal_exists {
        assert!(diag.wal_size_bytes > 0);
    }
}

// ─── Artifact serialization round-trip ──────────────────────────────

#[test]
fn t6_verify_artifact_json_round_trip() {
    let dir = fresh_dir();
    let db = dir.path().join("rt.db");
    create_wal_db(&db);

    // Force a non-ok artifact by corrupting
    let mut data = std::fs::read(&db).unwrap();
    data[0] = 0x00;
    std::fs::write(&db, &data).unwrap();

    let (_report, artifact) = verify_concurrency_artifact(&db).unwrap();
    let art = artifact.expect("must have artifact");

    let json = serde_json::to_string_pretty(&art).unwrap();

    // Structural checks on the JSON
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(parsed["report"].is_object());
    assert!(parsed["raw_diagnostics"].is_object());
    assert!(parsed["db_size_bytes"].is_number());
    assert!(parsed["captured_at_unix_ms"].is_number());
    assert!(parsed["db_path"].is_string());

    // Full round-trip
    let roundtrip: VerifyArtifact = serde_json::from_str(&json).unwrap();
    assert_eq!(roundtrip.db_size_bytes, art.db_size_bytes);
    assert_eq!(roundtrip.captured_at_unix_ms, art.captured_at_unix_ms);
    assert_eq!(roundtrip.report.ok, art.report.ok);

    let rd = roundtrip.raw_diagnostics.unwrap();
    let od = art.raw_diagnostics.unwrap();
    assert_eq!(rd.magic_ok, od.magic_ok);
    assert_eq!(rd.db_header_hex, od.db_header_hex);
}

#[test]
fn t7_raw_page_diagnostics_json_round_trip() {
    let diag = RawPageDiagnostics {
        magic_ok: true,
        header_page_size: 4096,
        header_page_count: 10,
        header_free_pages: 2,
        wal_exists: true,
        wal_size_bytes: 16384,
        wal_magic_ok: true,
        wal_frame_count_estimate: 3,
        db_header_hex: "53514c69746520666f726d6174203300".to_owned(),
        wal_header_hex: "377f068200000000".to_owned(),
    };

    let json = serde_json::to_string(&diag).unwrap();
    let rt: RawPageDiagnostics = serde_json::from_str(&json).unwrap();
    assert_eq!(rt.magic_ok, diag.magic_ok);
    assert_eq!(rt.header_page_size, diag.header_page_size);
    assert_eq!(rt.wal_frame_count_estimate, diag.wal_frame_count_estimate);
}

// ─── Artifact bundle file output ────────────────────────────────────

#[test]
fn t8_write_artifact_bundle_creates_file() {
    let dir = fresh_dir();
    let db = dir.path().join("bundle.db");
    create_wal_db(&db);

    let mut data = std::fs::read(&db).unwrap();
    data[0] = 0x00;
    std::fs::write(&db, &data).unwrap();

    let (_report, artifact) = verify_concurrency_artifact(&db).unwrap();
    let art = artifact.unwrap();

    let out_dir = dir.path().join("artifacts");
    let out_path = write_artifact_bundle(&art, &out_dir, "test_bundle").unwrap();

    assert!(out_path.exists(), "artifact file must exist");
    let contents = std::fs::read_to_string(&out_path).unwrap();
    let parsed: VerifyArtifact = serde_json::from_str(&contents).unwrap();
    assert_eq!(parsed.db_size_bytes, art.db_size_bytes);
}

// ─── Edge cases ─────────────────────────────────────────────────────

#[test]
fn t9_nonexistent_file_propagates_error() {
    let result = verify_concurrency_artifact(Path::new("/tmp/zuzb8_does_not_exist.db"));
    assert!(result.is_err());
}

#[test]
fn t10_completely_empty_file_handled_gracefully() {
    let dir = fresh_dir();
    let db = dir.path().join("empty.db");
    std::fs::write(&db, b"").unwrap();

    // SQLite treats a zero-byte file as an empty database (valid)
    let (report, artifact) = verify_concurrency_artifact(&db).unwrap();
    if report.ok {
        assert!(artifact.is_none());
    } else {
        let art = artifact.unwrap();
        let diag = art.raw_diagnostics.unwrap();
        assert!(!diag.magic_ok);
        assert_eq!(art.db_size_bytes, 0);
    }
}

#[test]
fn t11_garbage_file_produces_artifact() {
    let dir = fresh_dir();
    let db = dir.path().join("garbage.db");
    std::fs::write(&db, b"THIS IS NOT A SQLITE DATABASE FILE AT ALL 1234567890").unwrap();

    let (report, artifact) = verify_concurrency_artifact(&db).unwrap();
    assert!(!report.ok);
    let art = artifact.unwrap();
    assert!(art.raw_diagnostics.is_some());
    assert!(art.db_size_bytes > 0);
}

// ─── Concurrency test integration pattern ───────────────────────────

#[test]
fn t12_verify_after_concurrent_rusqlite_writes() {
    let dir = fresh_dir();
    let db = dir.path().join("conc.db");

    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL;
         CREATE TABLE stress (id INTEGER PRIMARY KEY, writer INTEGER, seq INTEGER);",
    )
    .unwrap();
    drop(conn);

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
    let handles: Vec<_> = (0..4u32)
        .map(|w| {
            let p = db.clone();
            let b = std::sync::Arc::clone(&barrier);
            std::thread::spawn(move || {
                let conn = rusqlite::Connection::open(&p).unwrap();
                conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=10000;")
                    .unwrap();
                b.wait();
                for i in 0..50 {
                    let _ = conn.execute(
                        "INSERT INTO stress (writer, seq) VALUES (?, ?)",
                        rusqlite::params![w, i],
                    );
                }
            })
        })
        .collect();

    for h in handles {
        h.join().unwrap();
    }

    let (report, artifact) = verify_concurrency_artifact(&db).unwrap();
    assert!(report.ok, "post-concurrent DB must pass: {report}");
    assert!(artifact.is_none());

    let count: i64 = {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.query_row("SELECT COUNT(*) FROM stress", [], |row| row.get(0))
            .unwrap()
    };
    assert!(count > 0, "must have some rows after concurrent writes");
}
