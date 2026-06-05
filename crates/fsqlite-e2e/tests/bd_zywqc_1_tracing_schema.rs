//! bd-zywqc.1: Structured logging schema contract tests.
//!
//! Validates the TraceContext, OpEvent, subscriber installation, schema
//! validation, and JSONL emission contracts.

use fsqlite_e2e::tracing_schema::{
    OpEvent, OpFields, OpOutcome, OpType, TraceContext, test_subscriber, validate_event_line,
    validate_harness_event_line,
};
use tempfile::TempDir;

fn fresh_dir() -> TempDir {
    tempfile::tempdir().expect("failed to create temp dir")
}

// ─── Phase 1: TraceContext identity ────────────────────────────────────

#[test]
fn p1_trace_context_new_has_all_required_fields() {
    let ctx = TraceContext::new("fsqlite-swarm");
    assert!(!ctx.run_id.is_empty(), "run_id must be non-empty");
    assert!(ctx.trace_id > 0, "trace_id must be positive");
    assert_eq!(ctx.workspace_id, "fsqlite-swarm");
    assert!(!ctx.host.is_empty(), "host must be non-empty");
    assert!(ctx.started_at_unix_nanos > 0, "started_at must be positive");
}

#[test]
fn p1_seeded_context_is_reproducible() {
    let ctx1 = TraceContext::with_seed("ws", 42);
    let ctx2 = TraceContext::with_seed("ws", 42);
    assert_eq!(ctx1.run_id, ctx2.run_id);
    assert_eq!(ctx1.trace_id, ctx2.trace_id);
    assert_eq!(ctx1.workspace_id, ctx2.workspace_id);
}

#[test]
fn p1_different_seeds_produce_different_contexts() {
    let ctx1 = TraceContext::with_seed("ws", 1);
    let ctx2 = TraceContext::with_seed("ws", 2);
    assert_ne!(ctx1.run_id, ctx2.run_id);
    assert_ne!(ctx1.trace_id, ctx2.trace_id);
}

#[test]
fn p1_context_json_round_trip() {
    let ctx = TraceContext::with_seed("roundtrip", 99);
    let json = serde_json::to_string(&ctx).unwrap();
    let parsed: TraceContext = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.run_id, ctx.run_id);
    assert_eq!(parsed.trace_id, ctx.trace_id);
    assert_eq!(parsed.workspace_id, ctx.workspace_id);
    assert_eq!(parsed.started_at_unix_nanos, ctx.started_at_unix_nanos);
}

// ─── Phase 2: OpEvent schema ──────────────────────────────────────────

#[test]
fn p2_op_event_contains_all_16_required_fields() {
    let ev = OpEvent {
        ts_unix_nanos: 1_700_000_000_000_000_000,
        level: "INFO".to_owned(),
        target: "fsqlite_e2e::swarm".to_owned(),
        span: "txn_commit".to_owned(),
        run_id: "seed-42".to_owned(),
        trace_id: 42,
        workspace_id: "test".to_owned(),
        host: "localhost".to_owned(),
        fields: OpFields {
            process_id: 1,
            op_id: 100,
            op_type: OpType::Insert,
            outcome: OpOutcome::Ok,
            wall_ms: Some(1.5),
            retry_count: Some(0),
            error_detail: None,
            table_name: Some("users".to_owned()),
            rows_affected: Some(1),
        },
    };

    let json = serde_json::to_string_pretty(&ev).unwrap();
    let required = [
        "ts_unix_nanos",
        "level",
        "target",
        "span",
        "run_id",
        "trace_id",
        "workspace_id",
        "host",
        "process_id",
        "op_id",
        "op_type",
        "outcome",
    ];
    for field in required {
        assert!(
            json.contains(field),
            "missing required field: {field}\njson: {json}"
        );
    }
}

#[test]
fn p2_op_event_json_round_trip() {
    let ev = OpEvent {
        ts_unix_nanos: 1_700_000_000_000_000_000,
        level: "WARN".to_owned(),
        target: "test".to_owned(),
        span: "retry".to_owned(),
        run_id: "run-1".to_owned(),
        trace_id: 999,
        workspace_id: "ws".to_owned(),
        host: "h".to_owned(),
        fields: OpFields {
            process_id: 2,
            op_id: 50,
            op_type: OpType::Update,
            outcome: OpOutcome::Busy,
            wall_ms: Some(10.5),
            retry_count: Some(3),
            error_detail: Some("database is locked".to_owned()),
            table_name: Some("orders".to_owned()),
            rows_affected: None,
        },
    };
    let json = serde_json::to_string(&ev).unwrap();
    let rt: OpEvent = serde_json::from_str(&json).unwrap();
    assert_eq!(rt.fields.op_type, OpType::Update);
    assert_eq!(rt.fields.outcome, OpOutcome::Busy);
    assert_eq!(rt.fields.retry_count, Some(3));
    assert_eq!(
        rt.fields.error_detail.as_deref(),
        Some("database is locked")
    );
}

#[test]
fn p2_optional_fields_omitted_when_none() {
    let fields = OpFields {
        process_id: 1,
        op_id: 1,
        op_type: OpType::Select,
        outcome: OpOutcome::Ok,
        wall_ms: None,
        retry_count: None,
        error_detail: None,
        table_name: None,
        rows_affected: None,
    };
    let json = serde_json::to_string(&fields).unwrap();
    assert!(!json.contains("wall_ms"), "wall_ms should be omitted");
    assert!(
        !json.contains("retry_count"),
        "retry_count should be omitted"
    );
    assert!(
        !json.contains("error_detail"),
        "error_detail should be omitted"
    );
}

// ─── Phase 3: OpType + OpOutcome enums ─────────────────────────────────

#[test]
fn p3_all_14_op_types_serialize_distinctly() {
    let types = [
        OpType::Insert,
        OpType::Update,
        OpType::Delete,
        OpType::Select,
        OpType::SelectById,
        OpType::DdlCreate,
        OpType::DdlAlter,
        OpType::DdlDrop,
        OpType::TxnBegin,
        OpType::TxnCommit,
        OpType::TxnRollback,
        OpType::Checkpoint,
        OpType::PragmaSet,
        OpType::PragmaRead,
    ];
    let mut seen = std::collections::HashSet::new();
    for t in types {
        let json = serde_json::to_string(&t).unwrap();
        assert!(
            seen.insert(json.clone()),
            "duplicate serialization for {t:?}: {json}"
        );
        let rt: OpType = serde_json::from_str(&json).unwrap();
        assert_eq!(rt, t);
    }
    assert_eq!(seen.len(), 14);
}

#[test]
fn p3_all_6_outcomes_serialize_distinctly() {
    let outcomes = [
        OpOutcome::Ok,
        OpOutcome::Busy,
        OpOutcome::Locked,
        OpOutcome::Conflict,
        OpOutcome::SchemaChanged,
        OpOutcome::Error,
    ];
    let mut seen = std::collections::HashSet::new();
    for o in outcomes {
        let json = serde_json::to_string(&o).unwrap();
        assert!(
            seen.insert(json.clone()),
            "duplicate serialization for {o:?}: {json}"
        );
        let rt: OpOutcome = serde_json::from_str(&json).unwrap();
        assert_eq!(rt, o);
    }
    assert_eq!(seen.len(), 6);
}

// ─── Phase 4: subscriber + JSON emission ───────────────────────────────

#[test]
fn p4_test_subscriber_emits_valid_jsonl() {
    let dir = fresh_dir();
    let ctx = TraceContext::with_seed("emit-test", 777);
    let (subscriber, log_path) = test_subscriber(dir.path(), &ctx).unwrap();

    tracing::subscriber::with_default(subscriber, || {
        tracing::info!(
            run_id = %ctx.run_id,
            trace_id = ctx.trace_id,
            workspace_id = %ctx.workspace_id,
            host = %ctx.host,
            op_type = "insert",
            outcome = "ok",
            process_id = 1_u32,
            op_id = 42_u64,
            "test operation complete"
        );
        tracing::warn!(
            run_id = %ctx.run_id,
            trace_id = ctx.trace_id,
            workspace_id = %ctx.workspace_id,
            host = %ctx.host,
            op_type = "update",
            outcome = "busy",
            process_id = 2_u32,
            op_id = 43_u64,
            retry_count = 3_u32,
            "retried operation"
        );
    });

    let content = std::fs::read_to_string(&log_path).unwrap();
    let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
    assert!(
        lines.len() >= 2,
        "expected at least 2 log lines, got {}",
        lines.len()
    );

    for line in &lines {
        let parsed: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("invalid JSON line: {e}\nline: {line}"));
        assert!(parsed.get("level").is_some(), "missing 'level': {parsed}");
        assert!(
            parsed.get("timestamp").is_some(),
            "missing 'timestamp': {parsed}"
        );
    }
}

#[test]
fn p4_emitted_lines_contain_trace_context_fields() {
    let dir = fresh_dir();
    let ctx = TraceContext::with_seed("ctx-check", 888);
    let (subscriber, log_path) = test_subscriber(dir.path(), &ctx).unwrap();

    tracing::subscriber::with_default(subscriber, || {
        tracing::info!(
            run_id = %ctx.run_id,
            trace_id = ctx.trace_id,
            workspace_id = %ctx.workspace_id,
            host = %ctx.host,
            "context emission test"
        );
    });

    let content = std::fs::read_to_string(&log_path).unwrap();
    assert!(
        content.contains(&ctx.run_id),
        "run_id not in output: {content}"
    );
    assert!(
        content.contains(&ctx.workspace_id),
        "workspace_id not in output: {content}"
    );
}

// ─── Phase 5: schema validation ────────────────────────────────────────

#[test]
fn p5_validate_event_line_accepts_valid_json() {
    let valid =
        r#"{"level":"INFO","timestamp":"2026-05-19T00:00:00Z","target":"test","fields":{}}"#;
    assert!(validate_event_line(valid).is_ok());
}

#[test]
fn p5_validate_event_line_rejects_missing_level() {
    let invalid = r#"{"timestamp":"2026-05-19T00:00:00Z","target":"test"}"#;
    let result = validate_event_line(invalid);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains(&"level"));
}

#[test]
fn p5_validate_event_line_rejects_garbage() {
    assert!(validate_event_line("not json").is_err());
}

#[test]
fn p5_validate_harness_line_checks_all_4_context_fields() {
    let with_all = r#"{"fields":{"run_id":"r","trace_id":1,"workspace_id":"w","host":"h"}}"#;
    assert!(validate_harness_event_line(with_all).is_ok());

    let missing_run = r#"{"fields":{"trace_id":1,"workspace_id":"w","host":"h"}}"#;
    let err = validate_harness_event_line(missing_run).unwrap_err();
    assert!(err.contains(&"run_id"));

    let missing_all = r#"{"fields":{}}"#;
    let err = validate_harness_event_line(missing_all).unwrap_err();
    assert_eq!(err.len(), 4, "should report all 4 missing fields");
}

// ─── Phase 6: multi-event emission ─────────────────────────────────────

#[test]
fn p6_100_events_all_valid_jsonl() {
    let dir = fresh_dir();
    let ctx = TraceContext::with_seed("bulk-emit", 100);
    let (subscriber, log_path) = test_subscriber(dir.path(), &ctx).unwrap();

    tracing::subscriber::with_default(subscriber, || {
        for i in 0..100_u64 {
            tracing::info!(
                run_id = %ctx.run_id,
                trace_id = ctx.trace_id,
                workspace_id = %ctx.workspace_id,
                host = %ctx.host,
                op_id = i,
                "event {i}"
            );
        }
    });

    let content = std::fs::read_to_string(&log_path).unwrap();
    let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(
        lines.len(),
        100,
        "expected 100 log lines, got {}",
        lines.len()
    );

    let mut invalid_count = 0;
    for line in &lines {
        if serde_json::from_str::<serde_json::Value>(line).is_err() {
            invalid_count += 1;
        }
    }
    assert_eq!(invalid_count, 0, "all 100 lines must be valid JSON");
}
