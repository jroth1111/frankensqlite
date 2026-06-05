//! Shared structured-logging schema for concurrency test infrastructure.
//!
//! Defines [`TraceContext`] (the per-run identity that every log line carries)
//! and [`OpEvent`] (the canonical shape of a structured operation log entry).
//! Provides [`install_subscriber`] to wire a JSON+terminal subscriber that
//! automatically injects `TraceContext` fields into every event.
//!
//! Bead: bd-zywqc.1

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Per-run identity that every structured log line must carry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceContext {
    pub run_id: String,
    pub trace_id: u128,
    pub workspace_id: String,
    pub host: String,
    pub started_at_unix_nanos: u64,
}

impl TraceContext {
    /// Create a new context for a test run.
    #[must_use]
    pub fn new(workspace_id: &str) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let nanos = now.as_nanos();
        let run_id = format!(
            "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
            (nanos >> 96) as u32,
            (nanos >> 80) as u16,
            (nanos >> 64) as u16,
            (nanos >> 48) as u16,
            nanos as u64 & 0xFFFF_FFFF_FFFF,
        );

        Self {
            run_id,
            trace_id: nanos,
            workspace_id: workspace_id.to_owned(),
            host: hostname(),
            started_at_unix_nanos: now.as_nanos() as u64,
        }
    }

    /// Create a context with an explicit seed for deterministic replay.
    #[must_use]
    pub fn with_seed(workspace_id: &str, seed: u64) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();

        Self {
            run_id: format!("seed-{seed:016x}"),
            trace_id: u128::from(seed),
            workspace_id: workspace_id.to_owned(),
            host: hostname(),
            started_at_unix_nanos: now.as_nanos() as u64,
        }
    }
}

/// Canonical shape of a structured operation log entry.
///
/// Every concurrency test operation emits one of these as JSONL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpEvent {
    pub ts_unix_nanos: u64,
    pub level: String,
    pub target: String,
    pub span: String,
    pub run_id: String,
    pub trace_id: u128,
    pub workspace_id: String,
    pub host: String,
    pub fields: OpFields,
}

/// Operation-specific fields within an [`OpEvent`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpFields {
    pub process_id: u32,
    pub op_id: u64,
    pub op_type: OpType,
    pub outcome: OpOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wall_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rows_affected: Option<u64>,
}

/// Operation type classification for structured logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpType {
    Insert,
    Update,
    Delete,
    Select,
    SelectById,
    DdlCreate,
    DdlAlter,
    DdlDrop,
    TxnBegin,
    TxnCommit,
    TxnRollback,
    Checkpoint,
    PragmaSet,
    PragmaRead,
}

/// Outcome classification for structured logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpOutcome {
    Ok,
    Busy,
    Locked,
    Conflict,
    SchemaChanged,
    Error,
}

/// Guard returned by [`install_subscriber`].
///
/// Holds the JSON log file handle open. Must be kept alive for the
/// duration of the test run.
pub struct TraceGuard {
    pub log_path: PathBuf,
    pub ctx: TraceContext,
}

/// Install a JSON+terminal tracing subscriber with `TraceContext` fields.
///
/// The JSON layer writes to `<output_dir>/trace.jsonl` and includes
/// `run_id`, `trace_id`, `workspace_id`, `host` in every event.
/// The terminal layer writes human-readable compact output.
///
/// Filter level defaults to `INFO`; override via `FRANKENSQLITE_TRACE_LEVEL`
/// or `RUST_LOG` environment variables.
///
/// # Errors
///
/// Returns `std::io::Error` if the output directory or log file cannot
/// be created.
pub fn install_subscriber(ctx: TraceContext, output_dir: &Path) -> std::io::Result<TraceGuard> {
    std::fs::create_dir_all(output_dir)?;
    let log_path = output_dir.join("trace.jsonl");
    let file = std::fs::File::create(&log_path)?;
    let file_writer = SharedFileWriter::new(file);

    let filter = env_filter();

    let json_layer = tracing_subscriber::fmt::layer()
        .json()
        .with_writer(file_writer)
        .with_target(true)
        .with_thread_ids(true);

    let terminal_layer = tracing_subscriber::fmt::layer()
        .with_ansi(true)
        .with_target(false)
        .compact();

    tracing_subscriber::registry()
        .with(filter)
        .with(terminal_layer)
        .with(json_layer)
        .init();

    Ok(TraceGuard { log_path, ctx })
}

/// Install subscriber for test contexts — scoped, non-global.
///
/// Returns the subscriber and the log path for assertion.
/// Use with `tracing::subscriber::with_default(sub, || { ... })`.
pub fn test_subscriber(
    output_dir: &Path,
    ctx: &TraceContext,
) -> std::io::Result<(impl tracing::Subscriber + Send + Sync, PathBuf)> {
    std::fs::create_dir_all(output_dir)?;
    let log_path = output_dir.join("trace.jsonl");
    let file = std::fs::File::create(&log_path)?;
    let file_writer = SharedFileWriter::new(file);

    let _ = ctx;

    let subscriber = tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .json()
            .with_writer(file_writer)
            .with_target(true)
            .with_thread_ids(true),
    );

    Ok((subscriber, log_path))
}

/// Emit a structured operation event using the tracing macros.
///
/// This produces a log line with all required schema fields.
#[macro_export]
macro_rules! trace_op {
    ($ctx:expr, $op_type:expr, $outcome:expr, $($field:tt)*) => {
        tracing::info!(
            run_id = %$ctx.run_id,
            trace_id = $ctx.trace_id,
            workspace_id = %$ctx.workspace_id,
            host = %$ctx.host,
            op_type = ?$op_type,
            outcome = ?$outcome,
            $($field)*
        )
    };
}

/// Validate that a JSONL line contains all required schema fields.
///
/// Returns `Ok(())` if valid, or `Err(missing_fields)` if any are absent.
pub fn validate_event_line(line: &str) -> Result<(), Vec<&'static str>> {
    let parsed: serde_json::Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return Err(vec!["(invalid JSON)"]),
    };

    let required = ["level", "timestamp", "target"];

    let missing: Vec<&'static str> = required
        .iter()
        .filter(|&&field| parsed.get(field).is_none())
        .copied()
        .collect();

    if missing.is_empty() {
        Ok(())
    } else {
        Err(missing)
    }
}

/// Validate that a JSONL line contains the extended harness schema fields.
///
/// Checks for `run_id`, `trace_id`, `workspace_id`, `host` in addition
/// to the base fields.
pub fn validate_harness_event_line(line: &str) -> Result<(), Vec<&'static str>> {
    let parsed: serde_json::Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return Err(vec!["(invalid JSON)"]),
    };

    let obj = parsed.get("fields").unwrap_or(&parsed);

    let harness_fields = ["run_id", "trace_id", "workspace_id", "host"];

    let missing: Vec<&'static str> = harness_fields
        .iter()
        .filter(|&&field| obj.get(field).is_none())
        .copied()
        .collect();

    if missing.is_empty() {
        Ok(())
    } else {
        Err(missing)
    }
}

fn env_filter() -> EnvFilter {
    if let Ok(level) = std::env::var("FRANKENSQLITE_TRACE_LEVEL") {
        return EnvFilter::new(level);
    }
    if let Ok(filter) = std::env::var("FRANKENSQLITE_TRACE_FILTER") {
        return EnvFilter::new(filter);
    }
    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))
}

fn hostname() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("HOST"))
        .unwrap_or_else(|_| "unknown".to_owned())
}

#[derive(Clone)]
struct SharedFileWriter {
    file: std::sync::Arc<Mutex<std::fs::File>>,
}

impl SharedFileWriter {
    fn new(file: std::fs::File) -> Self {
        Self {
            file: std::sync::Arc::new(Mutex::new(file)),
        }
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for SharedFileWriter {
    type Writer = SharedFileGuard<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        SharedFileGuard {
            guard: self.file.lock().expect("trace file mutex poisoned"),
        }
    }
}

struct SharedFileGuard<'a> {
    guard: std::sync::MutexGuard<'a, std::fs::File>,
}

impl std::io::Write for SharedFileGuard<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        std::io::Write::write(&mut *self.guard, buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        std::io::Write::flush(&mut *self.guard)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_context_new_produces_valid_fields() {
        let ctx = TraceContext::new("test-workspace");
        assert!(!ctx.run_id.is_empty());
        assert!(ctx.trace_id > 0);
        assert_eq!(ctx.workspace_id, "test-workspace");
        assert!(ctx.started_at_unix_nanos > 0);
    }

    #[test]
    fn trace_context_with_seed_is_deterministic() {
        let ctx1 = TraceContext::with_seed("ws", 42);
        let ctx2 = TraceContext::with_seed("ws", 42);
        assert_eq!(ctx1.run_id, ctx2.run_id);
        assert_eq!(ctx1.trace_id, ctx2.trace_id);
        assert_eq!(ctx1.run_id, "seed-000000000000002a");
        assert_eq!(ctx1.trace_id, 42);
    }

    #[test]
    fn trace_context_different_seeds_differ() {
        let ctx1 = TraceContext::with_seed("ws", 1);
        let ctx2 = TraceContext::with_seed("ws", 2);
        assert_ne!(ctx1.run_id, ctx2.run_id);
        assert_ne!(ctx1.trace_id, ctx2.trace_id);
    }

    #[test]
    fn trace_context_serializes_to_json() {
        let ctx = TraceContext::with_seed("test-ws", 99);
        let json = serde_json::to_string_pretty(&ctx).unwrap();
        assert!(json.contains("run_id"));
        assert!(json.contains("trace_id"));
        assert!(json.contains("workspace_id"));
        assert!(json.contains("host"));
        assert!(json.contains("started_at_unix_nanos"));
        let rt: TraceContext = serde_json::from_str(&json).unwrap();
        assert_eq!(rt.run_id, ctx.run_id);
        assert_eq!(rt.trace_id, ctx.trace_id);
    }

    #[test]
    fn op_event_serializes_round_trip() {
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
                retry_count: None,
                error_detail: None,
                table_name: Some("users".to_owned()),
                rows_affected: Some(1),
            },
        };
        let json = serde_json::to_string(&ev).unwrap();
        let rt: OpEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(rt.run_id, "seed-42");
        assert_eq!(rt.fields.op_type, OpType::Insert);
        assert_eq!(rt.fields.outcome, OpOutcome::Ok);
        assert_eq!(rt.fields.process_id, 1);
    }

    #[test]
    fn op_type_all_variants_serialize() {
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
        for t in types {
            let json = serde_json::to_string(&t).unwrap();
            assert!(!json.is_empty());
            let rt: OpType = serde_json::from_str(&json).unwrap();
            assert_eq!(rt, t);
        }
    }

    #[test]
    fn op_outcome_all_variants_serialize() {
        let outcomes = [
            OpOutcome::Ok,
            OpOutcome::Busy,
            OpOutcome::Locked,
            OpOutcome::Conflict,
            OpOutcome::SchemaChanged,
            OpOutcome::Error,
        ];
        for o in outcomes {
            let json = serde_json::to_string(&o).unwrap();
            let rt: OpOutcome = serde_json::from_str(&json).unwrap();
            assert_eq!(rt, o);
        }
    }

    #[test]
    fn op_fields_optional_fields_omitted_when_none() {
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
        assert!(!json.contains("wall_ms"));
        assert!(!json.contains("retry_count"));
        assert!(!json.contains("error_detail"));
        assert!(!json.contains("table_name"));
        assert!(!json.contains("rows_affected"));
    }

    #[test]
    fn test_subscriber_writes_json_lines() {
        let tmp = tempfile::TempDir::new().unwrap();
        let ctx = TraceContext::with_seed("test-ws", 123);
        let (subscriber, log_path) = test_subscriber(tmp.path(), &ctx).unwrap();

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(
                run_id = %ctx.run_id,
                trace_id = ctx.trace_id,
                workspace_id = %ctx.workspace_id,
                host = %ctx.host,
                op_type = "insert",
                outcome = "ok",
                "test operation"
            );
        });

        let content = std::fs::read_to_string(&log_path).unwrap();
        assert!(!content.is_empty(), "log file should have content");
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let parsed: serde_json::Value = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("invalid JSON: {e}\nline: {line}"));
            assert!(parsed.get("level").is_some());
        }
    }

    #[test]
    fn test_subscriber_emits_trace_context_fields() {
        let tmp = tempfile::TempDir::new().unwrap();
        let ctx = TraceContext::with_seed("schema-test", 456);
        let (subscriber, log_path) = test_subscriber(tmp.path(), &ctx).unwrap();

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(
                run_id = %ctx.run_id,
                trace_id = ctx.trace_id,
                workspace_id = %ctx.workspace_id,
                host = %ctx.host,
                "context check"
            );
        });

        let content = std::fs::read_to_string(&log_path).unwrap();
        assert!(
            content.contains("seed-00000000000001c8"),
            "run_id missing: {content}"
        );
        assert!(
            content.contains("schema-test"),
            "workspace_id missing: {content}"
        );
    }

    #[test]
    fn validate_event_line_accepts_valid() {
        let valid =
            r#"{"level":"INFO","timestamp":"2026-05-19T00:00:00Z","target":"test","fields":{}}"#;
        assert!(validate_event_line(valid).is_ok());
    }

    #[test]
    fn validate_event_line_rejects_missing_fields() {
        let invalid = r#"{"fields":{}}"#;
        let result = validate_event_line(invalid);
        assert!(result.is_err());
        let missing = result.unwrap_err();
        assert!(missing.contains(&"level"));
    }

    #[test]
    fn validate_event_line_rejects_garbage() {
        assert!(validate_event_line("not json at all").is_err());
    }

    #[test]
    fn validate_harness_event_line_checks_context_fields() {
        let with_ctx =
            r#"{"fields":{"run_id":"abc","trace_id":42,"workspace_id":"ws","host":"h"}}"#;
        assert!(validate_harness_event_line(with_ctx).is_ok());

        let without_ctx = r#"{"fields":{"op":"test"}}"#;
        let result = validate_harness_event_line(without_ctx);
        assert!(result.is_err());
        let missing = result.unwrap_err();
        assert!(missing.contains(&"run_id"));
    }
}
