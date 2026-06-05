//! bd-i0tpf — Oracle-parity e2e: SQLite collation sequences (BINARY/NOCASE/RTRIM)
//! across comparisons, ORDER BY, GROUP BY, DISTINCT, UNIQUE indexes, and explicit
//! COLLATE overrides, checked against rusqlite (real SQLite).
//!
//! Collation is a classic clean-room divergence source: the default is BINARY,
//! a column may declare a default collation, an expression may override it with
//! `COLLATE`, and the rules for which operand's collation wins in a comparison
//! are specific (left operand's explicit/column collation takes precedence).
//! These tests pin that behavior end-to-end through the public API.

use fsqlite::Connection;
use fsqlite_types::SqliteValue;

fn render_frank(v: &SqliteValue) -> String {
    match v {
        SqliteValue::Null => "NULL".to_owned(),
        SqliteValue::Integer(n) => n.to_string(),
        SqliteValue::Float(f) => format!("{f}"),
        SqliteValue::Text(s) => format!("'{s}'"),
        SqliteValue::Blob(b) => format!(
            "X'{}'",
            b.iter().map(|x| format!("{x:02X}")).collect::<String>()
        ),
    }
}

fn frank_rows(conn: &Connection, sql: &str) -> Result<Vec<Vec<String>>, String> {
    let rows = conn.query(sql).map_err(|e| e.to_string())?;
    Ok(rows
        .iter()
        .map(|row| row.values().iter().map(render_frank).collect())
        .collect())
}

fn sqlite_rows(conn: &rusqlite::Connection, sql: &str) -> Result<Vec<Vec<String>>, String> {
    let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
    let n = stmt.column_count();
    stmt.query_map([], |row| {
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let v: rusqlite::types::Value = row.get_unwrap(i);
            out.push(match v {
                rusqlite::types::Value::Null => "NULL".to_owned(),
                rusqlite::types::Value::Integer(x) => x.to_string(),
                rusqlite::types::Value::Real(f) => format!("{f}"),
                rusqlite::types::Value::Text(s) => format!("'{s}'"),
                rusqlite::types::Value::Blob(b) => format!(
                    "X'{}'",
                    b.iter().map(|x| format!("{x:02X}")).collect::<String>()
                ),
            });
        }
        Ok(out)
    })
    .map_err(|e| e.to_string())?
    .collect::<Result<Vec<_>, _>>()
    .map_err(|e| e.to_string())
}

/// Apply identical DDL/DML to a fresh FrankenSQLite and rusqlite in-memory db.
fn setup(stmts: &[&str]) -> (Connection, rusqlite::Connection) {
    let fconn = Connection::open(":memory:").expect("open frank");
    let rconn = rusqlite::Connection::open_in_memory().expect("open rusqlite");
    for s in stmts {
        fconn
            .execute(s)
            .unwrap_or_else(|e| panic!("frank `{s}`: {e}"));
        rconn
            .execute_batch(s)
            .unwrap_or_else(|e| panic!("rusqlite `{s}`: {e}"));
    }
    (fconn, rconn)
}

fn assert_parity(fconn: &Connection, rconn: &rusqlite::Connection, queries: &[&str], label: &str) {
    let mut mismatches = Vec::new();
    for q in queries {
        match (frank_rows(fconn, q), sqlite_rows(rconn, q)) {
            (Ok(f), Ok(s)) if f == s => {}
            (Ok(f), Ok(s)) => {
                mismatches.push(format!("MISMATCH: {q}\n  frank: {f:?}\n  csql:  {s:?}"));
            }
            (Err(fe), Ok(s)) => {
                mismatches.push(format!(
                    "FRANK_ERR: {q}\n  frank: ERROR({fe})\n  csql:  {s:?}"
                ));
            }
            (Ok(f), Err(se)) => {
                mismatches.push(format!(
                    "CSQL_ERR: {q}\n  frank: {f:?}\n  csql: ERROR({se})"
                ));
            }
            (Err(_), Err(_)) => {}
        }
    }
    assert!(
        mismatches.is_empty(),
        "{label}: {} mismatch(es)\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}

#[test]
fn collation_column_default_nocase() {
    let (fconn, rconn) = setup(&[
        "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT COLLATE NOCASE)",
        "INSERT INTO t VALUES (1,'Alice'),(2,'BOB'),(3,'alice'),(4,'bob'),(5,'Carol')",
    ]);
    assert_parity(
        &fconn,
        &rconn,
        &[
            // NOCASE column: equality is case-insensitive.
            "SELECT id FROM t WHERE name = 'alice' ORDER BY id",
            "SELECT id FROM t WHERE name = 'BOB' ORDER BY id",
            // ORDER BY on a NOCASE column groups case-insensitively.
            "SELECT id, name FROM t ORDER BY name, id",
            // IN uses the column collation.
            "SELECT id FROM t WHERE name IN ('ALICE','carol') ORDER BY id",
        ],
        "collation_column_default_nocase",
    );
}

#[test]
fn collation_explicit_override_in_expression() {
    let (fconn, rconn) = setup(&[
        "CREATE TABLE t (id INTEGER PRIMARY KEY, s TEXT)",
        "INSERT INTO t VALUES (1,'Apple'),(2,'apple'),(3,'BANANA'),(4,'banana')",
    ]);
    assert_parity(
        &fconn,
        &rconn,
        &[
            // Default BINARY: case-sensitive equality.
            "SELECT id FROM t WHERE s = 'apple' ORDER BY id",
            // Explicit COLLATE NOCASE override on the comparison.
            "SELECT id FROM t WHERE s = 'apple' COLLATE NOCASE ORDER BY id",
            "SELECT id FROM t WHERE s COLLATE NOCASE = 'BANANA' ORDER BY id",
            // ORDER BY with explicit collation override.
            "SELECT id, s FROM t ORDER BY s COLLATE NOCASE, id",
            "SELECT id, s FROM t ORDER BY s, id",
        ],
        "collation_explicit_override_in_expression",
    );
}

#[test]
fn collation_rtrim_semantics() {
    let (fconn, rconn) = setup(&[
        "CREATE TABLE t (id INTEGER PRIMARY KEY, s TEXT COLLATE RTRIM)",
        "INSERT INTO t VALUES (1,'abc'),(2,'abc   '),(3,'abc'),(4,'abcd'),(5,'  abc')",
    ]);
    assert_parity(
        &fconn,
        &rconn,
        &[
            // RTRIM: trailing spaces ignored, leading spaces significant.
            "SELECT id FROM t WHERE s = 'abc' ORDER BY id",
            "SELECT id FROM t WHERE s = 'abc ' ORDER BY id",
            "SELECT count(DISTINCT s) FROM t",
            "SELECT id, s FROM t ORDER BY s, id",
        ],
        "collation_rtrim_semantics",
    );
}

#[test]
fn collation_distinct_and_group_by() {
    let (fconn, rconn) = setup(&[
        "CREATE TABLE t (id INTEGER PRIMARY KEY, tag TEXT COLLATE NOCASE, n INTEGER)",
        "INSERT INTO t VALUES (1,'red',10),(2,'RED',20),(3,'Blue',5),(4,'blue',7),(5,'GREEN',3)",
    ]);
    assert_parity(
        &fconn,
        &rconn,
        &[
            // DISTINCT on a NOCASE column collapses case variants.
            "SELECT DISTINCT tag FROM t ORDER BY tag",
            // GROUP BY on a NOCASE column groups case-insensitively.
            "SELECT tag, count(*), sum(n) FROM t GROUP BY tag ORDER BY tag",
            // count(DISTINCT) under NOCASE.
            "SELECT count(DISTINCT tag) FROM t",
        ],
        "collation_distinct_and_group_by",
    );
}

#[test]
fn collation_unique_index_nocase() {
    // A UNIQUE NOCASE index must treat 'abc' and 'ABC' as a conflict; the
    // differential confirms FrankenSQLite rejects the dup exactly like rusqlite.
    let (fconn, rconn) = setup(&[
        "CREATE TABLE t (id INTEGER PRIMARY KEY, k TEXT)",
        "CREATE UNIQUE INDEX idx_k_nocase ON t (k COLLATE NOCASE)",
        "INSERT INTO t VALUES (1,'Hello')",
    ]);
    // Both engines must reject the case-variant duplicate.
    let f = fconn.execute("INSERT INTO t VALUES (2,'HELLO')");
    let r = rconn.execute_batch("INSERT INTO t VALUES (2,'HELLO')");
    assert!(
        f.is_err() && r.is_err(),
        "NOCASE UNIQUE dup: frank ok={:?}, rusqlite ok={:?} (both must reject)",
        f.is_ok(),
        r.is_ok()
    );
    // A genuinely distinct key inserts on both.
    fconn
        .execute("INSERT INTO t VALUES (3,'World')")
        .expect("frank distinct insert");
    rconn
        .execute_batch("INSERT INTO t VALUES (3,'World')")
        .expect("rusqlite distinct insert");
    assert_parity(
        &fconn,
        &rconn,
        &["SELECT id, k FROM t ORDER BY id"],
        "collation_unique_index_nocase",
    );
}

#[test]
fn collation_like_is_case_insensitive_regardless() {
    // LIKE is ASCII case-insensitive in SQLite independent of column collation;
    // GLOB is case-sensitive. Verify both match rusqlite.
    let (fconn, rconn) = setup(&[
        "CREATE TABLE t (id INTEGER PRIMARY KEY, s TEXT)",
        "INSERT INTO t VALUES (1,'Apple'),(2,'apple'),(3,'APPLE'),(4,'banana')",
    ]);
    assert_parity(
        &fconn,
        &rconn,
        &[
            "SELECT id FROM t WHERE s LIKE 'app%' ORDER BY id",
            "SELECT id FROM t WHERE s GLOB 'app*' ORDER BY id",
            "SELECT id FROM t WHERE s GLOB 'A*' ORDER BY id",
        ],
        "collation_like_is_case_insensitive_regardless",
    );
}

#[test]
fn collation_min_max_respects_column_collation() {
    let (fconn, rconn) = setup(&[
        "CREATE TABLE t (id INTEGER PRIMARY KEY, s TEXT COLLATE NOCASE)",
        "INSERT INTO t VALUES (1,'banana'),(2,'Apple'),(3,'cherry'),(4,'APPLE')",
    ]);
    assert_parity(
        &fconn,
        &rconn,
        &[
            // min/max over a NOCASE column use the column collation.
            "SELECT min(s), max(s) FROM t",
            // Comparison in WHERE against a NOCASE column.
            "SELECT id FROM t WHERE s < 'b' ORDER BY id",
        ],
        "collation_min_max_respects_column_collation",
    );
}
