//! bd-e6zfc: Non-atomic schema cache reloads across 3 RefCells (views,
//! triggers, schema_by_name) — concurrent DDL during reload can diverge
//! caches.
//!
//! ## Bug hypothesis
//!
//! Schema cache is stored in 3 separate RefCells: views, triggers, and
//! schema_by_name. When the cache is reloaded (e.g., after DDL from another
//! connection), each RefCell is updated independently. If a concurrent DDL
//! occurs between updates to these RefCells, the caches can diverge:
//! - views might reflect the new schema while triggers reflects the old
//! - schema_by_name might contain a table that triggers doesn't know about
//!
//! ## Test approach
//!
//! Exercise concurrent DDL + DML to detect divergence between schema caches.
//! Since this is a single-connection issue (RefCells are per-connection),
//! the test uses multiple connections with concurrent DDL.
//!
//! - S1: Concurrent CREATE VIEW + SELECT through view
//! - S2: Concurrent CREATE TRIGGER + trigger-firing DML
//! - S3: Rapid schema changes (ALTER TABLE) with cached queries
//! - S4: View + trigger interaction under concurrent DDL
//! - S5: Schema cache coherence after concurrent DDL storm

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use fsqlite::Connection;

const STRESS_DURATION: Duration = Duration::from_secs(2);

fn test_tmpdir() -> tempfile::TempDir {
    tempfile::tempdir_in(std::env::temp_dir())
        .or_else(|_| tempfile::tempdir_in("."))
        .expect("tempdir")
}

// ─── S1: Concurrent CREATE VIEW + SELECT ───────────────────────────

#[test]
fn s1_concurrent_create_view_select() {
    let dir = test_tmpdir();
    let db_path = dir.path().join("s1.db");
    let path_str = db_path.to_str().expect("path");

    {
        let conn = Connection::open(path_str).expect("open");
        conn.execute("CREATE TABLE base (id INTEGER PRIMARY KEY, val TEXT)")
            .expect("create");
        conn.execute("BEGIN").expect("begin");
        for i in 1..=100 {
            conn.execute(&format!("INSERT INTO base VALUES ({i}, 'row_{i}')"))
                .expect("seed");
        }
        conn.execute("COMMIT").expect("commit");
    }

    let stop = Arc::new(AtomicBool::new(false));

    // DDL: creates and drops views
    let d_path = path_str.to_string();
    let d_stop = Arc::clone(&stop);
    let ddl = std::thread::spawn(move || {
        let conn = Connection::open(&d_path).expect("d open");
        let mut ops = 0u64;
        while !d_stop.load(Ordering::Relaxed) {
            let vname = format!("v_{}", ops % 5);
            conn.execute(&format!(
                "CREATE VIEW IF NOT EXISTS {vname} AS SELECT * FROM base WHERE id <= {}",
                (ops % 100) + 1
            ))
            .ok();
            conn.execute(&format!("DROP VIEW IF EXISTS {vname}")).ok();
            ops += 1;
        }
        ops
    });

    // Reader: queries views and base table
    let r_path = path_str.to_string();
    let r_stop = Arc::clone(&stop);
    let reader = std::thread::spawn(move || {
        let conn = Connection::open(&r_path).expect("r open");
        let mut reads = 0u64;
        let mut errors = 0u64;
        while !r_stop.load(Ordering::Relaxed) {
            // Try to read through view (may not exist)
            let vname = format!("v_{}", reads % 5);
            match conn.query(&format!("SELECT * FROM {vname}")) {
                Ok(_) => reads += 1,
                Err(_) => errors += 1,
            }
            // Base table should always work
            if let Ok(rows) = conn.query("SELECT * FROM base") {
                assert!(
                    rows.len() >= 100,
                    "base table corrupted: {} rows",
                    rows.len()
                );
            }
        }
        (reads, errors)
    });

    std::thread::sleep(STRESS_DURATION);
    stop.store(true, Ordering::Relaxed);

    let ddl_ops = ddl.join().expect("DDL must not panic");
    let (reads, errors) = reader.join().expect("reader must not panic");

    eprintln!("S1: {ddl_ops} DDL ops, {reads} view reads, {errors} expected errors");
}

// ─── S2: Concurrent CREATE TRIGGER + DML ───────────────────────────

#[test]
fn s2_concurrent_create_trigger_dml() {
    let dir = test_tmpdir();
    let db_path = dir.path().join("s2.db");
    let path_str = db_path.to_str().expect("path");

    {
        let conn = Connection::open(path_str).expect("open");
        conn.execute("CREATE TABLE events (id INTEGER PRIMARY KEY, name TEXT)")
            .expect("create events");
        conn.execute("CREATE TABLE log (event_id INTEGER)")
            .expect("create log");
    }

    let stop = Arc::new(AtomicBool::new(false));

    // DDL: creates and drops triggers
    let d_path = path_str.to_string();
    let d_stop = Arc::clone(&stop);
    let ddl = std::thread::spawn(move || {
        let conn = Connection::open(&d_path).expect("d open");
        let mut ops = 0u64;
        while !d_stop.load(Ordering::Relaxed) {
            conn.execute(
                "CREATE TRIGGER IF NOT EXISTS t_log AFTER INSERT ON events \
                 BEGIN INSERT INTO log VALUES (NEW.id); END",
            )
            .ok();
            std::thread::sleep(Duration::from_millis(50));
            conn.execute("DROP TRIGGER IF EXISTS t_log").ok();
            std::thread::sleep(Duration::from_millis(50));
            ops += 1;
        }
        ops
    });

    // Writer: inserts into events (may or may not fire trigger)
    let w_path = path_str.to_string();
    let w_stop = Arc::clone(&stop);
    let writer = std::thread::spawn(move || {
        let conn = Connection::open(&w_path).expect("w open");
        let mut inserted = 0u64;
        while !w_stop.load(Ordering::Relaxed) {
            if conn.execute("BEGIN").is_ok() {
                conn.execute(&format!(
                    "INSERT INTO events VALUES ({inserted}, 'e_{inserted}')"
                ))
                .ok();
                if conn.execute("COMMIT").is_err() {
                    conn.execute("ROLLBACK").ok();
                }
                inserted += 1;
            }
        }
        inserted
    });

    std::thread::sleep(STRESS_DURATION);
    stop.store(true, Ordering::Relaxed);

    let ddl_ops = ddl.join().expect("DDL must not panic");
    let inserted = writer.join().expect("writer must not panic");

    // The key assertion: no panics from schema cache divergence
    let verify = Connection::open(path_str).expect("verify");
    let events = verify.query("SELECT * FROM events").expect("events").len();
    let logs = verify.query("SELECT * FROM log").expect("log").len();

    // Logs can be less than events (trigger was sometimes dropped)
    assert!(
        logs <= events,
        "more logs ({logs}) than events ({events}) — trigger fired extra times?"
    );
    eprintln!("S2: {ddl_ops} trigger DDL ops, {inserted} inserts, {events} events, {logs} logs");
}

// ─── S3: Rapid ALTER TABLE with cached queries ─────────────────────

#[test]
fn s3_alter_table_cached_queries() {
    let dir = test_tmpdir();
    let db_path = dir.path().join("s3.db");
    let path_str = db_path.to_str().expect("path");

    {
        let conn = Connection::open(path_str).expect("open");
        conn.execute("CREATE TABLE evolving (id INTEGER PRIMARY KEY, base_col TEXT)")
            .expect("create");
        conn.execute("INSERT INTO evolving VALUES (1, 'initial')")
            .expect("seed");
    }

    let stop = Arc::new(AtomicBool::new(false));

    // DDL: adds columns
    let d_path = path_str.to_string();
    let d_stop = Arc::clone(&stop);
    let ddl = std::thread::spawn(move || {
        let conn = Connection::open(&d_path).expect("d open");
        let mut ops = 0u64;
        while !d_stop.load(Ordering::Relaxed) {
            let col = format!("col_{ops}");
            conn.execute(&format!(
                "ALTER TABLE evolving ADD COLUMN {col} TEXT DEFAULT 'def'"
            ))
            .ok();
            ops += 1;
            if ops > 50 {
                break; // Don't add infinite columns
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        ops
    });

    // Reader: continuously queries with SELECT *
    let r_path = path_str.to_string();
    let r_stop = Arc::clone(&stop);
    let reader = std::thread::spawn(move || {
        let conn = Connection::open(&r_path).expect("r open");
        let mut reads = 0u64;
        while !r_stop.load(Ordering::Relaxed) {
            if conn.query("SELECT * FROM evolving").is_ok() {
                reads += 1;
            }
        }
        reads
    });

    std::thread::sleep(STRESS_DURATION);
    stop.store(true, Ordering::Relaxed);

    let alter_ops = ddl.join().expect("DDL must not panic");
    let reads = reader.join().expect("reader must not panic");

    eprintln!("S3: {alter_ops} ALTER TABLE ADD COLUMN ops, {reads} reads — no crash");
}

// ─── S4: View + trigger interaction under DDL ──────────────────────

#[test]
fn s4_view_trigger_interaction_ddl() {
    let conn = Connection::open(":memory:").expect("open");

    conn.execute("CREATE TABLE data (id INTEGER PRIMARY KEY, val TEXT)")
        .expect("create data");
    conn.execute("CREATE TABLE audit (data_id INTEGER, action TEXT)")
        .expect("create audit");

    // Cycle: create view → create trigger → insert (fires trigger, visible via view) →
    //        drop trigger → drop view → verify cleanup
    for round in 0..10 {
        conn.execute("CREATE VIEW v_audit AS SELECT data_id, action FROM audit")
            .expect("create view");
        conn.execute(
            "CREATE TRIGGER t_audit AFTER INSERT ON data \
             BEGIN INSERT INTO audit VALUES (NEW.id, 'added'); END",
        )
        .expect("create trigger");

        let id = round + 1;
        conn.execute(&format!("INSERT INTO data VALUES ({id}, 'round_{round}')"))
            .expect("insert");

        // View should show the trigger's insertion
        let view_rows = conn.query("SELECT * FROM v_audit").expect("view");
        assert_eq!(
            view_rows.len(),
            id as usize,
            "round {round}: view should show {id} rows"
        );

        conn.execute("DROP TRIGGER t_audit").expect("drop trigger");
        conn.execute("DROP VIEW v_audit").expect("drop view");

        // Audit table should still have data (trigger was dropped, not rolled back)
        let audit_rows = conn.query("SELECT * FROM audit").expect("audit");
        assert_eq!(
            audit_rows.len(),
            id as usize,
            "round {round}: audit should still have {id} rows after trigger/view drop"
        );
    }
    eprintln!("S4: 10 rounds of view+trigger create/drop cycles — schema coherent");
}

// ─── S5: Schema cache coherence after DDL storm ────────────────────

#[test]
#[ignore = "CONFIRMED BUG bd-e6zfc: schema_by_name diverges from schema under concurrent DDL — assertion 'schema_by_name size 8 != schema len 9' in connection.rs:26571"]
fn s5_schema_coherence_after_storm() {
    let dir = test_tmpdir();
    let db_path = dir.path().join("s5.db");
    let path_str = db_path.to_str().expect("path");

    {
        let conn = Connection::open(path_str).expect("open");
        conn.execute("CREATE TABLE anchor (id INTEGER PRIMARY KEY)")
            .expect("create anchor");
    }

    let stop = Arc::new(AtomicBool::new(false));
    let total_ddl = Arc::new(AtomicU64::new(0));

    // 4 DDL threads creating/dropping tables, views, triggers
    let threads: Vec<_> = (0..4)
        .map(|tid| {
            let path = path_str.to_string();
            let s = Arc::clone(&stop);
            let td = Arc::clone(&total_ddl);
            std::thread::spawn(move || {
                let conn = Connection::open(&path).expect("open");
                let mut ops = 0u64;
                while !s.load(Ordering::Relaxed) {
                    let name = format!("obj_{tid}_{}", ops % 3);
                    // Create table
                    conn.execute(&format!(
                        "CREATE TABLE IF NOT EXISTS {name} (id INTEGER PRIMARY KEY, v TEXT)"
                    ))
                    .ok();
                    // Create view on it
                    conn.execute(&format!(
                        "CREATE VIEW IF NOT EXISTS v_{name} AS SELECT * FROM {name}"
                    ))
                    .ok();
                    // Insert
                    conn.execute(&format!(
                        "INSERT OR IGNORE INTO {name} VALUES ({ops}, 'data')"
                    ))
                    .ok();
                    // Query via view
                    conn.query(&format!("SELECT * FROM v_{name}")).ok();
                    // Drop view
                    conn.execute(&format!("DROP VIEW IF EXISTS v_{name}")).ok();
                    // Drop table
                    conn.execute(&format!("DROP TABLE IF EXISTS {name}")).ok();
                    ops += 1;
                }
                td.fetch_add(ops, Ordering::Relaxed);
            })
        })
        .collect();

    std::thread::sleep(STRESS_DURATION);
    stop.store(true, Ordering::Relaxed);

    for t in threads {
        t.join()
            .expect("thread must not panic (schema cache divergence?)");
    }

    let ddl = total_ddl.load(Ordering::Relaxed);

    // Anchor table must survive
    let verify = Connection::open(path_str).expect("verify");
    let anchor = verify.query("SELECT * FROM anchor");
    assert!(anchor.is_ok(), "anchor table missing after DDL storm");
    eprintln!("S5: {ddl} DDL cycles across 4 threads, anchor table intact");
}
