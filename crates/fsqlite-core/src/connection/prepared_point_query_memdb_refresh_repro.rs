//! Regression coverage for file-backed prepared point-query MemDB / publication
//! thrashing.
//!
//! A warmed prepared PK lookup must not full-reload MemDB on every query, and
//! an external connection's commit must become visible on the next prepared
//! statement boundary without requiring reopen.

#![cfg(test)]

use std::sync::{Mutex, MutexGuard};

use fsqlite_types::SqliteValue;
use tempfile::NamedTempFile;

use super::{
    Connection, hot_path_profile_enabled, hot_path_profile_snapshot, reset_hot_path_profile,
    set_hot_path_profile_enabled,
};

static PROFILE_LOCK: Mutex<()> = Mutex::new(());

struct ProfileGuard {
    _lock: MutexGuard<'static, ()>,
    previous_enabled: bool,
}

impl ProfileGuard {
    fn new() -> Self {
        let lock = PROFILE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous_enabled = hot_path_profile_enabled();
        set_hot_path_profile_enabled(true);
        reset_hot_path_profile();
        Self {
            _lock: lock,
            previous_enabled,
        }
    }
}

impl Drop for ProfileGuard {
    fn drop(&mut self) {
        reset_hot_path_profile();
        set_hot_path_profile_enabled(self.previous_enabled);
    }
}

fn open_seeded_wal_db(row_count: i64) -> (NamedTempFile, Connection, String) {
    let tmp = NamedTempFile::new().expect("tempfile");
    let path = tmp.path().to_str().expect("utf8 path").to_owned();
    let conn = Connection::open(&path).expect("open file-backed db");
    conn.execute("PRAGMA journal_mode = WAL;").expect("wal");
    conn.execute("PRAGMA synchronous = NORMAL;").expect("sync");
    let _ = conn.execute("PRAGMA fsqlite_capture_time_travel_snapshots=false;");
    conn.execute(
        "CREATE TABLE bench (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            value REAL NOT NULL
        );",
    )
    .expect("create");
    conn.execute("BEGIN;").expect("begin seed");
    let insert = conn
        .prepare("INSERT INTO bench VALUES (?1, ('user_' || ?1), (?1 * 0.137))")
        .expect("prepare insert");
    for id in 0..row_count {
        insert
            .execute_with_params(&[SqliteValue::Integer(id)])
            .expect("seed row");
    }
    drop(insert);
    conn.execute("COMMIT;").expect("commit seed");
    (tmp, conn, path)
}

fn open_seeded_point_db(row_count: i64, journal_mode: &str) -> (NamedTempFile, Connection, String) {
    let tmp = NamedTempFile::new().expect("tempfile");
    let path = tmp.path().to_str().expect("utf8 path").to_owned();
    let conn = Connection::open(&path).expect("open file-backed db");
    conn.execute(&format!("PRAGMA journal_mode = {journal_mode};"))
        .expect("journal mode");
    conn.execute("PRAGMA synchronous = NORMAL;").expect("sync");
    let _ = conn.execute("PRAGMA fsqlite_capture_time_travel_snapshots=false;");
    conn.execute(
        "CREATE TABLE point_bench (
            id INTEGER PRIMARY KEY,
            email TEXT NOT NULL UNIQUE,
            display_name TEXT NOT NULL,
            group_name TEXT NOT NULL
        );",
    )
    .expect("create point table");
    conn.execute("BEGIN;").expect("begin seed");
    let insert = conn
        .prepare(
            "INSERT INTO point_bench VALUES (
                ?1, ('user_' || ?1 || '@example.test'), ('name_' || ?1), ('g_' || (?1 % 7))
            )",
        )
        .expect("prepare seed insert");
    for id in 0..row_count {
        insert
            .execute_with_params(&[SqliteValue::Integer(id)])
            .expect("seed point row");
    }
    drop(insert);
    conn.execute("COMMIT;").expect("commit seed");
    (tmp, conn, path)
}

#[test]
fn test_prepared_point_query_api_pk_unique_no_row_and_schema_reprepare() {
    let (_tmp, conn, _path) = open_seeded_point_db(64, "WAL");

    let mut by_id = conn
        .prepare_point_query("SELECT id, display_name FROM point_bench WHERE id = ?1")
        .expect("prepare projected integer-PK point query")
        .expect("integer-PK plan must be eligible");
    assert_eq!(by_id.column_count(), 2);
    assert_eq!(by_id.column_names(), &["id", "display_name"]);
    let row = conn
        .query_point_with_params(&mut by_id, &[SqliteValue::Integer(17)])
        .expect("execute PK point query")
        .expect("PK row");
    assert_eq!(row.get(0), Some(&SqliteValue::Integer(17)));
    assert_eq!(
        conn.query_point_with_params(&mut by_id, &[SqliteValue::Integer(999_999)])
            .expect("exact no-row result"),
        None
    );

    let mut by_email = conn
        .prepare_point_query("SELECT id, display_name FROM point_bench WHERE email = ?1")
        .expect("prepare UNIQUE-index point query")
        .expect("UNIQUE-index plan must be eligible");
    let row = conn
        .query_point_with_params(
            &mut by_email,
            &[SqliteValue::Text("user_9@example.test".into())],
        )
        .expect("execute UNIQUE-index point query")
        .expect("unique row");
    assert_eq!(row.get(0), Some(&SqliteValue::Integer(9)));

    assert!(
        conn.prepare_point_query("SELECT id FROM point_bench WHERE group_name = ?1")
            .expect("ordinary non-point preparation is not an error")
            .is_none(),
        "a non-unique predicate must not enter the point-query API"
    );

    // Same-connection DDL invalidates the handle. Execution must reprepare the
    // SQL and preserve its metadata/result rather than using the stale plan.
    conn.execute("CREATE INDEX point_bench_group_idx ON point_bench(group_name);")
        .expect("schema generation change");
    let row = conn
        .query_point_with_params(&mut by_id, &[SqliteValue::Integer(23)])
        .expect("point handle reparses after DDL")
        .expect("row after reprepare");
    assert_eq!(row.get(0), Some(&SqliteValue::Integer(23)));
    assert_eq!(by_id.column_names(), &["id", "display_name"]);
}

fn assert_exact_100k_point_visibility(journal_mode: &str) {
    let row_count = 100_000_i64;
    let (_tmp, conn, path) = open_seeded_point_db(row_count, journal_mode);
    let mut point = conn
        .prepare_point_query("SELECT id, display_name FROM point_bench WHERE id = ?1")
        .expect("prepare exact 100k point query")
        .expect("100k integer-PK plan must be eligible");

    for id in [0, row_count / 2, row_count - 1] {
        let row = conn
            .query_point_with_params(&mut point, &[SqliteValue::Integer(id)])
            .expect("100k point lookup")
            .expect("seeded point row");
        assert_eq!(row.get(0), Some(&SqliteValue::Integer(id)));
    }
    assert_eq!(
        conn.query_point_with_params(&mut point, &[SqliteValue::Integer(row_count + 1)])
            .expect("100k no-row lookup"),
        None
    );

    let writer = Connection::open(&path).expect("open exact-workload writer");
    writer
        .execute(&format!("PRAGMA journal_mode = {journal_mode};"))
        .expect("writer journal mode");
    writer
        .execute("UPDATE point_bench SET display_name = 'external_100k' WHERE id = 50000;")
        .expect("external exact-workload update");
    drop(writer);

    let row = conn
        .query_point_with_params(&mut point, &[SqliteValue::Integer(50_000)])
        .expect("external commit visibility")
        .expect("externally updated row");
    assert_eq!(
        row.get(1),
        Some(&SqliteValue::Text("external_100k".into())),
        "the next point-query boundary must not return stale data in {journal_mode} mode"
    );
}

/// Qualification-scale exact rollback-journal workload. Kept ignored in the
/// default unit suite because constructing the 100k-row fixture is expensive;
/// qualification invokes it explicitly with `--ignored`.
#[test]
#[ignore = "qualification-scale exact 100k-row workload"]
fn test_prepared_point_query_exact_100k_rollback_visibility() {
    assert_exact_100k_point_visibility("DELETE");
}

/// Qualification-scale exact WAL workload; see the rollback counterpart.
#[test]
#[ignore = "qualification-scale exact 100k-row workload"]
fn test_prepared_point_query_exact_100k_wal_visibility() {
    assert_exact_100k_point_visibility("WAL");
}

/// Warmed file-backed prepared PK lookups must not full-reload MemDB and must
/// not republish an unchanged publication plane on every statement.
#[test]
fn test_warmed_file_backed_prepared_point_query_skips_memdb_reload_and_noop_republish() {
    let _guard = ProfileGuard::new();
    let row_count = 2_000_i64;
    let hot_iters = 12_u64;
    let (_tmp, conn, _path) = open_seeded_wal_db(row_count);

    let probe_id = row_count / 2;
    let params = [SqliteValue::Integer(probe_id)];
    let stmt = conn
        .prepare("SELECT * FROM bench WHERE id = ?1")
        .expect("prepare point query");

    // Warm: establish MemDB rows + publication binding.
    let warm = stmt.query_with_params(&params).expect("warm query");
    assert_eq!(warm.len(), 1);
    assert!(
        conn.memdb_rows_loaded.get(),
        "warm prepared point query should hydrate the clean file-backed MemDB image"
    );

    let pub_before = conn.pager.published_snapshot();
    let pub_writes_before = conn.pager.publication_write_count();
    let memdb_vis_before = conn.memdb_visible_commit_seq.borrow().get();
    reset_hot_path_profile();

    for i in 0..hot_iters {
        let rows = stmt
            .query_with_params(&params)
            .unwrap_or_else(|err| panic!("hot query {i}: {err}"));
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].get(0),
            Some(&SqliteValue::Integer(probe_id)),
            "hot query {i} must return the seeded primary key"
        );

        // Streaming entry used by many harnesses must share the same skip gate.
        let mut seen = 0_u32;
        stmt.query_with_params_for_each(&params, |row| {
            seen += 1;
            assert_eq!(row.get(0), Some(&SqliteValue::Integer(probe_id)));
            Ok(())
        })
        .unwrap_or_else(|err| panic!("hot for_each {i}: {err}"));
        assert_eq!(seen, 1, "for_each {i} must yield one row");
    }

    let profile = hot_path_profile_snapshot();
    let pub_after = conn.pager.published_snapshot();
    let pub_writes_after = conn.pager.publication_write_count();
    let memdb_vis_after = conn.memdb_visible_commit_seq.borrow().get();

    assert_eq!(
        profile.memdb_refresh_count, 0,
        "warm prepared point queries must not full-reload MemDB: {profile:?}"
    );
    assert!(
        conn.memdb_rows_loaded.get(),
        "MemDB rows must stay loaded across warm prepared point queries"
    );
    assert_eq!(
        memdb_vis_after, memdb_vis_before,
        "memdb_visible_commit_seq must stay stable on unchanged prepared reads"
    );
    assert_eq!(
        pub_after.visible_commit_seq, pub_before.visible_commit_seq,
        "published visible_commit_seq must stay stable on unchanged prepared reads"
    );
    assert_eq!(
        pub_after.snapshot_gen, pub_before.snapshot_gen,
        "unchanged durable identity must not republish / advance snapshot_gen on prepared reads \
         (before={}, after={}, pub_writes {}→{})",
        pub_before.snapshot_gen, pub_after.snapshot_gen, pub_writes_before, pub_writes_after,
    );
    assert_eq!(
        pub_writes_after, pub_writes_before,
        "unchanged durable identity must not increment publication_write_count on prepared reads"
    );
    assert!(
        profile.parser.fast_path_executions >= hot_iters,
        "point queries should stay on the prepared fast path: {profile:?}"
    );
}

/// A second connection's commit must be visible on the next prepared statement
/// and must force exactly one MemDB refresh at that boundary.
#[test]
fn test_prepared_point_query_observes_external_commit_on_next_statement() {
    let _guard = ProfileGuard::new();
    let row_count = 1_000_i64;
    let (_tmp, conn, path) = open_seeded_wal_db(row_count);

    let params_existing = [SqliteValue::Integer(1)];
    let stmt = conn
        .prepare("SELECT id, name FROM bench WHERE id = ?1")
        .expect("prepare point query");

    let warm = stmt
        .query_with_params(&params_existing)
        .expect("warm existing row");
    assert_eq!(warm.len(), 1);
    assert!(conn.memdb_rows_loaded.get());

    let writer = Connection::open(&path).expect("open writer");
    writer.execute("PRAGMA journal_mode = WAL;").ok();
    writer.execute("BEGIN;").expect("writer begin");
    writer
        .execute("INSERT INTO bench VALUES (999001, 'external', 1.5);")
        .expect("external insert");
    writer.execute("COMMIT;").expect("writer commit");
    drop(writer);

    reset_hot_path_profile();
    let external_params = [SqliteValue::Integer(999001)];
    let rows = stmt
        .query_with_params(&external_params)
        .expect("reader must observe external commit on next prepared statement");
    assert_eq!(rows.len(), 1, "external insert must be visible");
    assert_eq!(rows[0].get(0), Some(&SqliteValue::Integer(999001)));
    assert_eq!(rows[0].get(1), Some(&SqliteValue::Text("external".into())));

    let profile = hot_path_profile_snapshot();
    assert_eq!(
        profile.memdb_refresh_count, 1,
        "exactly one MemDB refresh is required to observe the external commit: {profile:?}"
    );
    assert!(
        conn.memdb_rows_loaded.get(),
        "MemDB must remain hydrated after the external-commit refresh"
    );

    // Steady-state after the external refresh must not keep reloading.
    reset_hot_path_profile();
    let pub_before = conn.pager.published_snapshot();
    for _ in 0..8 {
        let again = stmt
            .query_with_params(&external_params)
            .expect("post-refresh point query");
        assert_eq!(again.len(), 1);
    }
    let profile_after = hot_path_profile_snapshot();
    let pub_after = conn.pager.published_snapshot();
    assert_eq!(
        profile_after.memdb_refresh_count, 0,
        "after absorbing the external commit, warm prepared reads must not reload: {profile_after:?}"
    );
    assert_eq!(
        pub_after.snapshot_gen, pub_before.snapshot_gen,
        "post-refresh steady-state prepared reads must not no-op republish"
    );
}

/// Pager-level: idle clean-WAL publication probes must not advance snapshot_gen
/// when durable identity is unchanged, but must observe external header/WAL
/// advances (covered elsewhere for header replacement).
#[test]
fn test_pager_idle_clean_wal_refresh_skips_noop_republish() {
    use fsqlite_types::Cx;

    let _guard = ProfileGuard::new();
    let (_tmp, conn, _path) = open_seeded_wal_db(256);
    let cx = Cx::new();

    // Warm publication once.
    let _ = conn
        .pager
        .refresh_published_snapshot(&cx)
        .expect("initial refresh");
    let before = conn.pager.published_snapshot();
    let writes_before = conn.pager.publication_write_count();

    for _ in 0..5 {
        let after = conn
            .pager
            .refresh_published_snapshot_for_clean_wal_read(&cx)
            .expect("idle clean wal refresh");
        assert_eq!(
            after.visible_commit_seq, before.visible_commit_seq,
            "idle probe must not invent a new visible commit seq"
        );
        assert_eq!(
            after.snapshot_gen, before.snapshot_gen,
            "idle clean-WAL probe must not republish when durable identity is unchanged"
        );
    }

    assert_eq!(
        conn.pager.publication_write_count(),
        writes_before,
        "idle clean-WAL probes must not increment publication_write_count"
    );
}
