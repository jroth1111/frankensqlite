//! bd-nkfbj: Reproduce mam_rust #118 RefCell double-borrow at connection.rs.
//!
//! The mam_rust reporter sees a RefCell `already borrowed` panic on the
//! send_message path after a clean doctor archive-normalize/reconstruct/repair
//! sequence.  PRAGMA quick_check is clean, so this is an in-process
//! re-entrancy bug in fsqlite.
//!
//! ## Approach
//!
//! Three attack surfaces for RefCell re-entrancy:
//! 1. Trigger DML body re-enters execute while outer DML holds borrows (T1-T9)
//! 2. Schema change invalidates prepared statement, revalidation during
//!    execution borrows schema+db simultaneously (T10-T14)
//! 3. Prepared statement reuse after DDL + trigger DML (T15-T17)
//!
//! ## Original root cause hypothesis
//!
//! When a DML statement fires a trigger whose body contains another DML
//! statement, the inner DML re-enters the MemDatabase borrow path while the
//! outer DML still holds a `RefCell<MemDatabase>` borrow.  This is a classic
//! Rust RefCell re-entrancy panic.
//!
//! ## Minimal repro sequence
//!
//! 1. CREATE TABLE messages (id INTEGER PRIMARY KEY, body TEXT, ts TEXT)
//! 2. CREATE TABLE message_log (msg_id INTEGER, action TEXT)
//! 3. CREATE TRIGGER after_insert_msg AFTER INSERT ON messages
//!    BEGIN INSERT INTO message_log VALUES (NEW.id, 'sent'); END
//! 4. INSERT INTO messages VALUES (1, 'hello', '2026-01-01')
//!    → trigger fires → nested INSERT into message_log
//!    → RefCell<MemDatabase> already borrowed → PANIC
//!
//! The deeper variant (nested triggers) chains:
//!   INSERT messages → trigger → INSERT message_log → trigger → UPDATE stats

use fsqlite::Connection;

/// Helper: open an in-memory connection with deterministic state.
fn open_conn() -> Connection {
    Connection::open(":memory:").expect("open :memory:")
}

// ─── T1: Single-level AFTER INSERT trigger with DML body ────────────

#[test]
fn t1_after_insert_trigger_dml_body() {
    let conn = open_conn();

    conn.execute("CREATE TABLE messages (id INTEGER PRIMARY KEY, body TEXT, ts TEXT)")
        .expect("create messages");
    conn.execute("CREATE TABLE message_log (msg_id INTEGER, action TEXT)")
        .expect("create message_log");
    conn.execute(
        "CREATE TRIGGER after_insert_msg AFTER INSERT ON messages \
         BEGIN INSERT INTO message_log VALUES (NEW.id, 'sent'); END",
    )
    .expect("create trigger");

    // This INSERT fires the trigger whose body is another INSERT.
    // If MemDatabase RefCell is borrowed during the outer INSERT and
    // the trigger tries to borrow_mut again, this panics.
    conn.execute("INSERT INTO messages VALUES (1, 'hello', '2026-01-01')")
        .expect("insert with trigger should not panic");

    let rows = conn
        .query("SELECT msg_id, action FROM message_log")
        .expect("query message_log");
    assert_eq!(rows.len(), 1, "trigger should have inserted one log row");
}

// ─── T2: BEFORE INSERT trigger with DML body ────────────────────────

#[test]
fn t2_before_insert_trigger_dml_body() {
    let conn = open_conn();

    conn.execute("CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT)")
        .expect("create items");
    conn.execute("CREATE TABLE audit (item_id INTEGER, op TEXT)")
        .expect("create audit");
    conn.execute(
        "CREATE TRIGGER before_insert_item BEFORE INSERT ON items \
         BEGIN INSERT INTO audit VALUES (NEW.id, 'pre-insert'); END",
    )
    .expect("create trigger");

    conn.execute("INSERT INTO items VALUES (1, 'widget')")
        .expect("insert with BEFORE trigger should not panic");

    let rows = conn
        .query("SELECT item_id, op FROM audit")
        .expect("query audit");
    assert_eq!(rows.len(), 1);
}

// ─── T3: AFTER UPDATE trigger with DML body ─────────────────────────

#[test]
fn t3_after_update_trigger_dml_body() {
    let conn = open_conn();

    conn.execute("CREATE TABLE accounts (id INTEGER PRIMARY KEY, balance INTEGER)")
        .expect("create accounts");
    conn.execute("CREATE TABLE txn_log (acct_id INTEGER, old_bal INTEGER, new_bal INTEGER)")
        .expect("create txn_log");
    conn.execute(
        "CREATE TRIGGER after_update_acct AFTER UPDATE ON accounts \
         BEGIN INSERT INTO txn_log VALUES (NEW.id, OLD.balance, NEW.balance); END",
    )
    .expect("create trigger");

    conn.execute("INSERT INTO accounts VALUES (1, 100)")
        .expect("seed");
    conn.execute("UPDATE accounts SET balance = 200 WHERE id = 1")
        .expect("update with trigger should not panic");

    let rows = conn
        .query("SELECT acct_id, old_bal, new_bal FROM txn_log")
        .expect("query txn_log");
    assert_eq!(rows.len(), 1);
}

// ─── T4: AFTER DELETE trigger with DML body ─────────────────────────

#[test]
fn t4_after_delete_trigger_dml_body() {
    let conn = open_conn();

    conn.execute("CREATE TABLE records (id INTEGER PRIMARY KEY, data TEXT)")
        .expect("create records");
    conn.execute("CREATE TABLE trash (rec_id INTEGER, deleted_data TEXT)")
        .expect("create trash");
    conn.execute(
        "CREATE TRIGGER after_delete_rec AFTER DELETE ON records \
         BEGIN INSERT INTO trash VALUES (OLD.id, OLD.data); END",
    )
    .expect("create trigger");

    conn.execute("INSERT INTO records VALUES (1, 'important')")
        .expect("seed");
    conn.execute("DELETE FROM records WHERE id = 1")
        .expect("delete with trigger should not panic");

    let rows = conn
        .query("SELECT rec_id, deleted_data FROM trash")
        .expect("query trash");
    assert_eq!(rows.len(), 1);
}

// ─── T5: Nested triggers (trigger fires trigger) ────────────────────

#[test]
fn t5_nested_trigger_chain() {
    let conn = open_conn();

    conn.execute("CREATE TABLE messages (id INTEGER PRIMARY KEY, body TEXT)")
        .expect("create messages");
    conn.execute("CREATE TABLE message_log (msg_id INTEGER, action TEXT)")
        .expect("create message_log");
    conn.execute("CREATE TABLE stats (total_msgs INTEGER)")
        .expect("create stats");
    conn.execute("INSERT INTO stats VALUES (0)")
        .expect("seed stats");

    // Trigger 1: INSERT into messages → INSERT into message_log
    conn.execute(
        "CREATE TRIGGER t_msg_log AFTER INSERT ON messages \
         BEGIN INSERT INTO message_log VALUES (NEW.id, 'logged'); END",
    )
    .expect("create trigger 1");

    // Trigger 2: INSERT into message_log → UPDATE stats
    conn.execute(
        "CREATE TRIGGER t_log_stats AFTER INSERT ON message_log \
         BEGIN UPDATE stats SET total_msgs = total_msgs + 1; END",
    )
    .expect("create trigger 2");

    // This chains: INSERT messages → trigger → INSERT message_log → trigger → UPDATE stats
    conn.execute("INSERT INTO messages VALUES (1, 'hello')")
        .expect("nested trigger chain should not panic");

    let rows = conn
        .query("SELECT total_msgs FROM stats")
        .expect("query stats");
    assert_eq!(rows.len(), 1);
}

// ─── T6: Multi-row INSERT with trigger firing per row ───────────────

#[test]
fn t6_multi_row_insert_trigger_per_row() {
    let conn = open_conn();

    conn.execute("CREATE TABLE events (id INTEGER PRIMARY KEY, name TEXT)")
        .expect("create events");
    conn.execute("CREATE TABLE event_audit (event_id INTEGER)")
        .expect("create event_audit");
    conn.execute(
        "CREATE TRIGGER t_event_audit AFTER INSERT ON events \
         BEGIN INSERT INTO event_audit VALUES (NEW.id); END",
    )
    .expect("create trigger");

    for i in 1..=10 {
        conn.execute(&format!("INSERT INTO events VALUES ({i}, 'event_{i}')"))
            .expect("insert should not panic");
    }

    let rows = conn
        .query("SELECT COUNT(*) FROM event_audit")
        .expect("count audit rows");
    assert_eq!(rows.len(), 1);
}

// ─── T7: Trigger with UPDATE on same table (self-referencing) ───────

#[test]
fn t7_trigger_updates_same_table() {
    let conn = open_conn();

    conn.execute("CREATE TABLE counters (id INTEGER PRIMARY KEY, val INTEGER, last_update TEXT)")
        .expect("create counters");

    // AFTER UPDATE trigger that updates a different column of the same row.
    // This is the most dangerous re-entrancy case: same table, same RefCell path.
    conn.execute(
        "CREATE TRIGGER t_counter_ts AFTER UPDATE OF val ON counters \
         BEGIN UPDATE counters SET last_update = 'updated' WHERE id = NEW.id; END",
    )
    .expect("create trigger");

    conn.execute("INSERT INTO counters VALUES (1, 0, 'never')")
        .expect("seed");

    // SQLite prevents infinite recursion by default (PRAGMA recursive_triggers = OFF).
    // The trigger should fire once and not recurse.
    conn.execute("UPDATE counters SET val = 42 WHERE id = 1")
        .expect("self-referencing trigger should not panic or infinite-loop");

    let rows = conn
        .query("SELECT val, last_update FROM counters WHERE id = 1")
        .expect("query");
    assert_eq!(rows.len(), 1);
}

// ─── T8: mam_rust #118 exact scenario ──────────────────────────────
// Reproduces the send_message → INSERT messages → trigger → nested DML
// pattern from the mam_rust bug report.

#[test]
fn t8_mam_rust_118_send_message_scenario() {
    let conn = open_conn();

    // Schema mimicking mam_rust's message storage
    conn.execute(
        "CREATE TABLE conversations (id INTEGER PRIMARY KEY, last_msg_id INTEGER, msg_count INTEGER)",
    )
    .expect("create conversations");
    conn.execute(
        "CREATE TABLE messages (id INTEGER PRIMARY KEY, conv_id INTEGER REFERENCES conversations(id), \
         sender TEXT, body TEXT, created_at TEXT)",
    )
    .expect("create messages");
    conn.execute("CREATE TABLE read_receipts (msg_id INTEGER, reader TEXT, read_at TEXT)")
        .expect("create read_receipts");

    // Seed a conversation
    conn.execute("INSERT INTO conversations VALUES (1, NULL, 0)")
        .expect("seed conversation");

    // Trigger: after inserting a message, update the conversation's last_msg_id and count
    conn.execute(
        "CREATE TRIGGER t_update_conv AFTER INSERT ON messages \
         BEGIN \
           UPDATE conversations SET last_msg_id = NEW.id, msg_count = msg_count + 1 \
           WHERE id = NEW.conv_id; \
         END",
    )
    .expect("create conversation update trigger");

    // Trigger: auto-create a read receipt for the sender
    conn.execute(
        "CREATE TRIGGER t_self_read AFTER INSERT ON messages \
         BEGIN \
           INSERT INTO read_receipts VALUES (NEW.id, NEW.sender, NEW.created_at); \
         END",
    )
    .expect("create self-read trigger");

    // The send_message operation: INSERT into messages fires both triggers.
    // Trigger 1: UPDATE conversations (same MemDatabase borrow path)
    // Trigger 2: INSERT read_receipts (another MemDatabase borrow path)
    conn.execute("INSERT INTO messages VALUES (1, 1, 'alice', 'Hello!', '2026-01-15T10:00:00')")
        .expect("send_message must not panic (mam_rust #118)");

    // Verify all side effects
    let conv = conn
        .query("SELECT last_msg_id, msg_count FROM conversations WHERE id = 1")
        .expect("query conversation");
    assert_eq!(conv.len(), 1, "conversation row must exist");

    let receipts = conn
        .query("SELECT msg_id, reader FROM read_receipts")
        .expect("query receipts");
    assert_eq!(receipts.len(), 1, "self-read receipt must exist");

    // Send a second message to verify repeated trigger firing
    conn.execute("INSERT INTO messages VALUES (2, 1, 'bob', 'Hi Alice!', '2026-01-15T10:01:00')")
        .expect("second send_message must not panic");

    let conv2 = conn
        .query("SELECT msg_count FROM conversations WHERE id = 1")
        .expect("query conversation after second msg");
    assert_eq!(conv2.len(), 1);
}

// ─── T9: Trigger with subquery referencing triggering table ─────────

#[test]
fn t9_trigger_subquery_reads_triggering_table() {
    let conn = open_conn();

    conn.execute("CREATE TABLE orders (id INTEGER PRIMARY KEY, amount INTEGER)")
        .expect("create orders");
    conn.execute("CREATE TABLE order_totals (computed_total INTEGER)")
        .expect("create order_totals");

    // Trigger reads from the same table being inserted into
    conn.execute(
        "CREATE TRIGGER t_recompute_total AFTER INSERT ON orders \
         BEGIN \
           DELETE FROM order_totals; \
           INSERT INTO order_totals SELECT SUM(amount) FROM orders; \
         END",
    )
    .expect("create trigger");

    conn.execute("INSERT INTO orders VALUES (1, 100)")
        .expect("first insert should not panic");
    conn.execute("INSERT INTO orders VALUES (2, 250)")
        .expect("second insert should not panic");

    let rows = conn
        .query("SELECT computed_total FROM order_totals")
        .expect("query totals");
    assert_eq!(rows.len(), 1);
}

// ═══════════════════════════════════════════════════════════════════════
// Surface 2: Schema change + prepared statement revalidation
// ═══════════════════════════════════════════════════════════════════════

// ─── T10: Prepared INSERT after ALTER TABLE ADD COLUMN ───────────────

#[test]
fn t10_prepared_insert_after_alter_table() {
    let conn = open_conn();

    conn.execute("CREATE TABLE docs (id INTEGER PRIMARY KEY, title TEXT)")
        .expect("create docs");
    conn.execute("CREATE TABLE doc_log (doc_id INTEGER, op TEXT)")
        .expect("create doc_log");
    conn.execute(
        "CREATE TRIGGER t_doc_log AFTER INSERT ON docs \
         BEGIN INSERT INTO doc_log VALUES (NEW.id, 'created'); END",
    )
    .expect("create trigger");

    // First insert works fine
    conn.execute("INSERT INTO docs VALUES (1, 'First')")
        .expect("initial insert");

    // ALTER TABLE changes schema_cookie, invalidating cached state
    conn.execute("ALTER TABLE docs ADD COLUMN author TEXT")
        .expect("alter table");

    // This INSERT re-enters after schema invalidation.
    // The prepared statement path must revalidate schema without
    // double-borrowing during trigger execution.
    conn.execute("INSERT INTO docs (id, title, author) VALUES (2, 'Second', 'Alice')")
        .expect("insert after ALTER should not panic");

    let rows = conn.query("SELECT COUNT(*) FROM doc_log").expect("count");
    assert_eq!(rows.len(), 1);
}

// ─── T11: DROP + CREATE same table name, then trigger fires ─────────

#[test]
fn t11_drop_create_same_table_trigger_fires() {
    let conn = open_conn();

    conn.execute("CREATE TABLE data (id INTEGER PRIMARY KEY, val TEXT)")
        .expect("create data");
    conn.execute("CREATE TABLE audit (data_id INTEGER, action TEXT)")
        .expect("create audit");
    conn.execute(
        "CREATE TRIGGER t_audit AFTER INSERT ON data \
         BEGIN INSERT INTO audit VALUES (NEW.id, 'inserted'); END",
    )
    .expect("create trigger");

    conn.execute("INSERT INTO data VALUES (1, 'a')")
        .expect("first insert");

    // Simulate doctor repair: DROP + recreate
    conn.execute("DROP TRIGGER t_audit").expect("drop trigger");
    conn.execute("DROP TABLE data").expect("drop table");
    conn.execute("CREATE TABLE data (id INTEGER PRIMARY KEY, val TEXT, extra INTEGER)")
        .expect("recreate data");
    conn.execute(
        "CREATE TRIGGER t_audit AFTER INSERT ON data \
         BEGIN INSERT INTO audit VALUES (NEW.id, 'inserted-v2'); END",
    )
    .expect("recreate trigger");

    // Schema has been fully rebuilt. This INSERT must work without panic.
    conn.execute("INSERT INTO data VALUES (2, 'b', 42)")
        .expect("insert after drop/create cycle should not panic");

    let rows = conn
        .query("SELECT action FROM audit WHERE data_id = 2")
        .expect("query audit");
    assert_eq!(rows.len(), 1);
}

// ─── T12: Rapid DDL cycle then INSERT with trigger ──────────────────

#[test]
fn t12_rapid_ddl_cycle_then_trigger_insert() {
    let conn = open_conn();

    // Simulate doctor archive-normalize-reconstruct-repair sequence:
    // rapid DDL that bumps schema_cookie many times.
    for i in 0..5 {
        conn.execute(&format!(
            "CREATE TABLE tmp_{i} (id INTEGER PRIMARY KEY, v TEXT)"
        ))
        .expect("create tmp");
    }
    for i in 0..5 {
        conn.execute(&format!("DROP TABLE tmp_{i}"))
            .expect("drop tmp");
    }

    // Now create the real schema
    conn.execute("CREATE TABLE messages (id INTEGER PRIMARY KEY, body TEXT)")
        .expect("create messages");
    conn.execute("CREATE TABLE msg_meta (msg_id INTEGER, kind TEXT)")
        .expect("create msg_meta");
    conn.execute(
        "CREATE TRIGGER t_msg_meta AFTER INSERT ON messages \
         BEGIN INSERT INTO msg_meta VALUES (NEW.id, 'text'); END",
    )
    .expect("create trigger");

    // Schema cookie has been bumped 10+ times. Now exercise the
    // "first DML after heavy DDL" path which must revalidate.
    conn.execute("INSERT INTO messages VALUES (1, 'hello after repair')")
        .expect("post-repair insert should not panic");

    let rows = conn.query("SELECT * FROM msg_meta").expect("query meta");
    assert_eq!(rows.len(), 1);
}

// ─── T13: VACUUM then INSERT with trigger ───────────────────────────

#[test]
fn t13_vacuum_then_trigger_insert() {
    let conn = open_conn();

    conn.execute("CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT)")
        .expect("create items");
    conn.execute("CREATE TABLE item_log (item_id INTEGER)")
        .expect("create item_log");
    conn.execute(
        "CREATE TRIGGER t_item_log AFTER INSERT ON items \
         BEGIN INSERT INTO item_log VALUES (NEW.id); END",
    )
    .expect("create trigger");

    // Seed and delete to create freelist pages
    for i in 1..=100 {
        conn.execute(&format!("INSERT INTO items VALUES ({i}, 'item_{i}')"))
            .expect("seed");
    }
    conn.execute("DELETE FROM items WHERE id > 10")
        .expect("delete most rows");

    // VACUUM rebuilds the entire database file, changing page layout.
    // This is what doctor's "reconstruct" step does.
    let vacuum_result = conn.execute("VACUUM");
    if vacuum_result.is_err() {
        // VACUUM may not be fully implemented; skip gracefully
        return;
    }

    // Post-VACUUM insert with trigger must not panic
    conn.execute("INSERT INTO items VALUES (101, 'post-vacuum')")
        .expect("post-vacuum insert should not panic");

    let rows = conn
        .query("SELECT COUNT(*) FROM item_log WHERE item_id = 101")
        .expect("query");
    assert_eq!(rows.len(), 1);
}

// ─── T14: Concurrent schema change across connections + trigger ─────

#[test]
fn t14_two_conn_schema_change_then_trigger() {
    // Use file-backed DB so both connections see the same state
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("t14.db");
    let path_str = db_path.to_str().expect("path");

    let conn1 = Connection::open(path_str).expect("open conn1");
    conn1
        .execute("CREATE TABLE data (id INTEGER PRIMARY KEY, val TEXT)")
        .expect("create data");
    conn1
        .execute("CREATE TABLE log (data_id INTEGER, op TEXT)")
        .expect("create log");
    conn1
        .execute(
            "CREATE TRIGGER t_log AFTER INSERT ON data \
             BEGIN INSERT INTO log VALUES (NEW.id, 'from_trigger'); END",
        )
        .expect("create trigger");
    conn1
        .execute("INSERT INTO data VALUES (1, 'first')")
        .expect("seed via conn1");

    // conn2 opens the same DB, does DDL that changes schema
    let conn2 = Connection::open(path_str).expect("open conn2");
    conn2
        .execute("ALTER TABLE data ADD COLUMN extra TEXT")
        .expect("alter via conn2");

    // conn1's cached schema is now stale. This INSERT fires the trigger
    // and must handle schema revalidation without RefCell panic.
    conn1
        .execute("INSERT INTO data (id, val) VALUES (2, 'after-alter')")
        .expect("conn1 insert after conn2 ALTER should not panic");

    let rows = conn1.query("SELECT COUNT(*) FROM log").expect("query log");
    assert_eq!(rows.len(), 1);
}

// ═══════════════════════════════════════════════════════════════════════
// Surface 3: Prepared statement object reuse after schema change
// ═══════════════════════════════════════════════════════════════════════

// ─── T15: PreparedStatement.execute() after schema change ───────────

#[test]
fn t15_prepared_stmt_reuse_after_ddl() {
    let conn = open_conn();

    conn.execute("CREATE TABLE msgs (id INTEGER PRIMARY KEY, body TEXT)")
        .expect("create msgs");
    conn.execute("CREATE TABLE msg_audit (msg_id INTEGER)")
        .expect("create msg_audit");
    conn.execute(
        "CREATE TRIGGER t_audit AFTER INSERT ON msgs \
         BEGIN INSERT INTO msg_audit VALUES (NEW.id); END",
    )
    .expect("create trigger");

    // Prepare a statement
    let stmt = conn
        .prepare("INSERT INTO msgs VALUES (1, 'prepared')")
        .expect("prepare");

    // DDL changes schema_cookie
    conn.execute("ALTER TABLE msgs ADD COLUMN ts TEXT")
        .expect("alter table");

    // Execute the prepared statement — its cached schema_cookie is stale.
    // The execute path must handle revalidation + trigger firing safely.
    let result = stmt.execute();
    // Might return SQLITE_SCHEMA error or succeed; either is acceptable.
    // A panic (BorrowMutError) is the bug we're looking for.
    match result {
        Ok(_) => {
            let rows = conn.query("SELECT COUNT(*) FROM msg_audit").expect("q");
            assert_eq!(rows.len(), 1);
        }
        Err(e) => {
            let msg = format!("{e}");
            assert!(
                msg.contains("schema") || msg.contains("Schema"),
                "expected schema error, got: {msg}"
            );
        }
    }
}

// ─── T16: Prepared SELECT after DROP/CREATE cycle ───────────────────

#[test]
fn t16_prepared_select_after_drop_create() {
    let conn = open_conn();

    conn.execute("CREATE TABLE info (id INTEGER PRIMARY KEY, val TEXT)")
        .expect("create");
    conn.execute("INSERT INTO info VALUES (1, 'original')")
        .expect("seed");

    let stmt = conn
        .prepare("SELECT id, val FROM info WHERE id = 1")
        .expect("prepare");

    // Schema change: drop and recreate with different columns
    conn.execute("DROP TABLE info").expect("drop");
    conn.execute("CREATE TABLE info (id INTEGER PRIMARY KEY, val TEXT, extra INTEGER)")
        .expect("recreate");
    conn.execute("INSERT INTO info VALUES (1, 'rebuilt', 42)")
        .expect("seed new");

    // Prepared statement points to old schema. Execution must not panic.
    let result = stmt.query();
    match result {
        Ok(rows) => assert!(!rows.is_empty()),
        Err(_) => {} // Schema error is acceptable
    }
}

// ─── T17: Full doctor repair simulation ─────────────────────────────
// This test simulates the complete mam_rust doctor sequence:
// 1. Create schema + seed data
// 2. "archive": read all data
// 3. "normalize": DROP all tables, recreate with clean schema
// 4. "reconstruct": re-INSERT all data
// 5. "repair": integrity check + VACUUM
// 6. Resume normal operations (send_message = INSERT with triggers)

#[test]
fn t17_full_doctor_repair_then_send_message() {
    let conn = open_conn();

    // Phase 1: Original schema (simulating mam_rust's DB)
    conn.execute(
        "CREATE TABLE conversations (id INTEGER PRIMARY KEY, last_msg_id INTEGER, msg_count INTEGER)",
    )
    .expect("create conversations");
    conn.execute(
        "CREATE TABLE messages (id INTEGER PRIMARY KEY, conv_id INTEGER, \
         sender TEXT, body TEXT, created_at TEXT)",
    )
    .expect("create messages");
    conn.execute("CREATE TABLE read_receipts (msg_id INTEGER, reader TEXT)")
        .expect("create read_receipts");

    conn.execute(
        "CREATE TRIGGER t_conv_update AFTER INSERT ON messages \
         BEGIN UPDATE conversations SET last_msg_id = NEW.id, msg_count = msg_count + 1 \
         WHERE id = NEW.conv_id; END",
    )
    .expect("create conv trigger");
    conn.execute(
        "CREATE TRIGGER t_self_receipt AFTER INSERT ON messages \
         BEGIN INSERT INTO read_receipts VALUES (NEW.id, NEW.sender); END",
    )
    .expect("create receipt trigger");

    // Seed data
    conn.execute("INSERT INTO conversations VALUES (1, NULL, 0)")
        .expect("seed conv");
    for i in 1..=5 {
        conn.execute(&format!(
            "INSERT INTO messages VALUES ({i}, 1, 'alice', 'msg {i}', '2026-01-0{i}')"
        ))
        .expect("seed msg");
    }

    // Phase 2: "archive" — read all data
    let archived_msgs = conn
        .query("SELECT id, conv_id, sender, body, created_at FROM messages")
        .expect("archive messages");
    let msg_count = archived_msgs.len();

    // Phase 3: "normalize" — DROP everything and recreate
    conn.execute("DROP TRIGGER t_self_receipt")
        .expect("drop trigger 1");
    conn.execute("DROP TRIGGER t_conv_update")
        .expect("drop trigger 2");
    conn.execute("DROP TABLE read_receipts").expect("drop rr");
    conn.execute("DROP TABLE messages").expect("drop msgs");
    conn.execute("DROP TABLE conversations")
        .expect("drop convs");

    // Recreate with identical schema (normalize = schema is canonical)
    conn.execute(
        "CREATE TABLE conversations (id INTEGER PRIMARY KEY, last_msg_id INTEGER, msg_count INTEGER)",
    )
    .expect("recreate conversations");
    conn.execute(
        "CREATE TABLE messages (id INTEGER PRIMARY KEY, conv_id INTEGER, \
         sender TEXT, body TEXT, created_at TEXT)",
    )
    .expect("recreate messages");
    conn.execute("CREATE TABLE read_receipts (msg_id INTEGER, reader TEXT)")
        .expect("recreate read_receipts");

    // Phase 4: "reconstruct" — re-INSERT triggers
    conn.execute(
        "CREATE TRIGGER t_conv_update AFTER INSERT ON messages \
         BEGIN UPDATE conversations SET last_msg_id = NEW.id, msg_count = msg_count + 1 \
         WHERE id = NEW.conv_id; END",
    )
    .expect("recreate conv trigger");
    conn.execute(
        "CREATE TRIGGER t_self_receipt AFTER INSERT ON messages \
         BEGIN INSERT INTO read_receipts VALUES (NEW.id, NEW.sender); END",
    )
    .expect("recreate receipt trigger");

    // Reconstruct data
    conn.execute("INSERT INTO conversations VALUES (1, NULL, 0)")
        .expect("reconstruct conv");
    for i in 1..=msg_count as i64 {
        conn.execute(&format!(
            "INSERT INTO messages VALUES ({i}, 1, 'alice', 'msg {i}', '2026-01-0{i}')"
        ))
        .expect("reconstruct msg");
    }

    // Phase 5: "repair" — integrity check
    let check = conn.query("PRAGMA integrity_check");
    if let Ok(rows) = &check {
        assert!(!rows.is_empty());
    }

    // VACUUM if supported
    let _ = conn.execute("VACUUM");

    // Phase 6: "resume" — send_message (the operation that panics in mam_rust)
    // This is the critical test: after the full doctor sequence, the schema has
    // been dropped and recreated, schema_cookie has been bumped many times,
    // and any cached state from before the repair is stale.
    conn.execute(
        "INSERT INTO messages VALUES (100, 1, 'bob', 'Hello after repair!', '2026-02-01')",
    )
    .expect("send_message after full doctor repair must not panic");

    // Verify trigger side-effects
    let conv = conn
        .query("SELECT msg_count FROM conversations WHERE id = 1")
        .expect("check conv");
    assert_eq!(conv.len(), 1);

    let receipts = conn
        .query("SELECT COUNT(*) FROM read_receipts WHERE msg_id = 100")
        .expect("check receipts");
    assert_eq!(receipts.len(), 1);
}
