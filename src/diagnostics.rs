//! Opt-in, local diagnostic evidence. Never participates in business transactions.
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    fs::File,
    io::Write,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);
static FAILED: AtomicBool = AtomicBool::new(false);
static WRITER: OnceLock<Mutex<Option<File>>> = OnceLock::new();
thread_local! { static RUN: RefCell<Option<String>> = const { RefCell::new(None) }; }
thread_local! { static MODEL_EVENTS: RefCell<Option<Vec<Value>>> = const { RefCell::new(None) }; }

/// Collect model outcome metadata on this thread without enabling file traces.
/// Requests, responses and personal text are excluded; business results are unchanged.
pub fn capture_model_events<T>(run: impl FnOnce() -> T) -> (T, Vec<Value>) {
    struct Restore(Option<Vec<Value>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            MODEL_EVENTS.with(|events| events.replace(self.0.take()));
        }
    }
    let _restore = Restore(MODEL_EVENTS.with(|events| events.replace(Some(Vec::new()))));
    let result = run();
    let events = MODEL_EVENTS.with(|events| events.borrow_mut().take().unwrap());
    (result, events)
}

/// Diagnostic-only context. Never serialized into model input, checkpoints or budget policy.
#[derive(Clone, Debug)]
pub struct TraceContext {
    trace_id: String,
    source_id: Option<String>,
    generation: Option<u64>,
    round: Option<u32>,
    attempt_id: Option<String>,
}
impl Default for TraceContext {
    fn default() -> Self {
        Self::new()
    }
}
impl TraceContext {
    pub(crate) fn attempt(attempt_id: &str) -> Self {
        Self {
            attempt_id: Some(attempt_id.into()),
            ..Self::new()
        }
    }

    pub fn new() -> Self {
        static TRACE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
        Self {
            trace_id: format!(
                "trace-{}-{}-{}",
                std::process::id(),
                time::OffsetDateTime::now_utc().unix_timestamp_nanos(),
                TRACE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ),
            source_id: None,
            generation: None,
            round: None,
            attempt_id: None,
        }
    }
    pub(crate) fn source(source_id: &str, generation: u64, round: u32, attempt_id: &str) -> Self {
        Self {
            source_id: Some(source_id.into()),
            generation: Some(generation),
            round: Some(round),
            attempt_id: Some(attempt_id.into()),
            ..Self::new()
        }
    }
    pub(crate) fn record(&self, kind: &str, data: impl FnOnce() -> Value) {
        record_context(self, kind, data);
    }
    pub(crate) fn checked<T, E: std::fmt::Display>(
        &self,
        stage: &str,
        phase: &str,
        result: Result<T, E>,
    ) -> Result<T, E> {
        self.record("model_check", || {
            json!({"stage":stage,"phase":phase,
            "status":if result.is_ok() {"completed"}else{"failed"},
            "error":result.as_ref().err().map(ToString::to_string)})
        });
        result
    }
    fn envelope(&self, kind: &str, mut body: Value) -> Value {
        scrub(&mut body);
        let mut meta = json!({"trace_id":self.trace_id,"source_id":self.source_id,
            "generation":self.generation,"round":self.round,"attempt_id":self.attempt_id});
        // Safe metadata is constructed, not copied wholesale from diagnostic bodies.
        for key in ["call_id", "stage", "phase", "status", "local_outcome"] {
            if let Some(text) = body.get(key).and_then(Value::as_str)
                && text.len() <= 128
                && text
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b))
            {
                meta[key] = json!(text);
            }
        }
        for key in ["duration_ms", "elapsed_ms", "timeout_ms"] {
            if body[key].is_u64() {
                meta[key] = body[key].clone();
            }
        }
        json!({"trace_version":"2","event_id":new_id(),"kind":kind,
            "at":time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339).ok(),
            "meta":meta,"content_deps":{"deps_complete":false,"reason":"not_proven",
                "sources":[],"memories":[]},"body":body})
    }
}

pub fn new_id() -> String {
    format!(
        "{}-{}-{}",
        std::process::id(),
        time::OffsetDateTime::now_utc().unix_timestamp_nanos(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

#[cfg(test)]
thread_local! {static TEST_WRITER:RefCell<Option<Box<dyn Write>>> = RefCell::new(None);}
#[cfg(test)]
pub(crate) fn with_test_writer<T>(writer: Box<dyn Write>, run: impl FnOnce() -> T) -> T {
    struct Restore(Option<Box<dyn Write>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            TEST_WRITER.with(|w| w.replace(self.0.take()));
        }
    }
    let _restore = Restore(TEST_WRITER.with(|w| w.replace(Some(writer))));
    run()
}
fn write_row(writer: &mut dyn Write, row: &Value) -> std::io::Result<()> {
    writeln!(writer, "{row}").and_then(|_| writer.flush())
}
pub fn enabled() -> bool {
    #[cfg(test)]
    if TEST_WRITER.with(|w| w.borrow().is_some()) {
        return true;
    }
    std::env::var_os("PMR_TRACE_DIR").is_some()
}
pub fn incomplete() -> bool {
    enabled() && FAILED.load(Ordering::Relaxed)
}

fn failed() {
    FAILED.store(true, Ordering::Relaxed);
    eprintln!(
        "[pmr-trace] evidence_incomplete: diagnostic write failed; business result unchanged"
    );
}

pub struct Operation(Option<String>);
impl Operation {
    pub fn enter(id: &str) -> Self {
        Self(RUN.with(|run| run.replace(Some(id.into()))))
    }
}
impl Drop for Operation {
    fn drop(&mut self) {
        RUN.with(|run| {
            run.replace(self.0.take());
        });
    }
}

// Credential headers are never captured. Scrub structured/embedded keys and common
// token spellings as a second boundary; free-form personal text stays private.
fn scrub(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                let key = key.to_ascii_lowercase().replace('-', "_");
                if [
                    "authorization",
                    "cookie",
                    "api_key",
                    "access_token",
                    "secret",
                    "password",
                ]
                .iter()
                .any(|part| key.contains(part))
                {
                    *value = json!("[REDACTED]");
                } else {
                    scrub(value);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(scrub),
        Value::String(text) => {
            if let Ok(mut embedded @ (Value::Object(_) | Value::Array(_))) =
                serde_json::from_str(text)
            {
                let original = embedded.clone();
                scrub(&mut embedded);
                if embedded != original {
                    *text = embedded.to_string();
                }
                return;
            }
            if let Ok(secret) = std::env::var("DEEPSEEK_API_KEY")
                && !secret.is_empty()
            {
                *text = text.replace(&secret, "[REDACTED]");
            }
            for marker in ["Bearer ", "bearer ", "sk-", "ark-"] {
                let mut offset = 0;
                while let Some(start) = text[offset..].find(marker).map(|n| n + offset) {
                    if start > 0
                        && text[..start]
                            .chars()
                            .next_back()
                            .is_some_and(|c| c.is_alphanumeric() || c == '_')
                    {
                        offset = start + marker.len();
                        continue;
                    }
                    let end = text[start..]
                        .find(|c: char| c.is_whitespace() || "\"'<>;,}".contains(c))
                        .map(|n| n + start)
                        .unwrap_or(text.len());
                    let end = if marker.ends_with(' ') {
                        let token = start + marker.len();
                        text[token..]
                            .find(|c: char| c.is_whitespace() || "\"'<>;,}".contains(c))
                            .map(|n| n + token)
                            .unwrap_or(text.len())
                    } else {
                        end
                    };
                    text.replace_range(start..end, "[REDACTED]");
                    offset = start + "[REDACTED]".len();
                }
            }
        }
        _ => (),
    }
}

pub fn record_for(run_id: &str, kind: &str, data: impl FnOnce() -> Value) {
    if !enabled() && !collecting(kind) {
        return;
    }
    // Legacy callers cannot prove dependencies. Never expose their host-supplied ID.
    let context = legacy_context(run_id);
    record_context(&context, kind, data);
}
fn legacy_context(run_id: &str) -> TraceContext {
    // Process-local opaque IDs preserve legacy correlation without retaining host strings.
    static IDS: OnceLock<Mutex<std::collections::BTreeMap<Vec<u8>, TraceContext>>> =
        OnceLock::new();
    let key = ring::digest::digest(&ring::digest::SHA256, run_id.as_bytes())
        .as_ref()
        .to_vec();
    let Ok(mut ids) = IDS.get_or_init(Default::default).lock() else {
        return TraceContext::new();
    };
    if let Some(context) = ids.get(&key) {
        return context.clone();
    }
    if ids.len() >= 4096 {
        ids.clear();
    } // Evicted legacy correlations become separate incomplete runs.
    let context = TraceContext::new();
    ids.insert(key, context.clone());
    context
}
fn record_context(context: &TraceContext, kind: &str, data: impl FnOnce() -> Value) {
    let collect = collecting(kind);
    if !enabled() && !collect {
        return;
    }
    let data = data();
    if collect {
        collect_metadata(kind, &data);
    }
    write_context(context, kind, || data);
}
fn write_context(context: &TraceContext, kind: &str, data: impl FnOnce() -> Value) {
    if !enabled() {
        return;
    }
    #[cfg(test)]
    if TEST_WRITER.with(|w| w.borrow().is_some()) {
        let row = context.envelope(kind, data());
        TEST_WRITER.with(|w| {
            if write_row(w.borrow_mut().as_mut().unwrap().as_mut(), &row).is_err() {
                failed();
            }
        });
        return;
    }
    let writer = WRITER.get_or_init(|| {
        let file = (|| -> std::io::Result<File> {
            let dir = std::path::PathBuf::from(std::env::var_os("PMR_TRACE_DIR").unwrap());
            // Caller supplies an existing private directory; never create paths or overwrite evidence.
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
                if std::fs::metadata(&dir)?.permissions().mode() & 0o077 != 0 {
                    return Err(std::io::Error::other("trace directory must be private"));
                }
                options.mode(0o600);
            }
            options.open(dir.join(format!("trace-{}.jsonl", new_id())))
        })();
        if file.is_err() {
            failed();
        }
        Mutex::new(file.ok())
    });
    let row = context.envelope(kind, data());
    let Ok(mut guard) = writer.lock() else {
        failed();
        return;
    };
    let Some(file) = guard.as_mut() else {
        return;
    };
    // ponytail: synchronous JSONL and full snapshots for bounded local diagnostics;
    // use a bounded collector/deltas if measured profile size makes this expensive.
    if write_row(file, &row).is_err() {
        failed();
    }
}

fn collecting(kind: &str) -> bool {
    MODEL_EVENTS.with(|events| events.borrow().is_some())
        && matches!(kind, "model_start" | "model_end" | "model_check")
}
fn collect_metadata(kind: &str, data: &Value) {
    let keys = [
        "call_id",
        "stage",
        "phase",
        "status",
        "local_outcome",
        "error",
        "timeout_ms",
        "duration_ms",
        "parsing_status",
        "field",
        "rule",
        "cause",
        "candidate_index",
        "reasons",
    ];
    let mut metadata = json!({});
    for key in keys {
        if let Some(value) = data.get(key) {
            metadata[key] = value.clone();
        }
    }
    scrub(&mut metadata);
    MODEL_EVENTS.with(|events| {
        events
            .borrow_mut()
            .as_mut()
            .unwrap()
            .push(json!({"kind":kind,"data":metadata}))
    });
}
pub(crate) fn record(kind: &str, data: impl FnOnce() -> Value) {
    if !enabled() && !collecting(kind) {
        return;
    }
    let id = RUN.with(|run| run.borrow().clone());
    if let Some(id) = id {
        record_context(&legacy_context(&id), kind, data);
    } else if collecting(kind) {
        collect_metadata(kind, &data());
    }
}

pub(crate) fn checked<T, E: std::fmt::Display>(
    stage: &str,
    phase: &str,
    result: Result<T, E>,
) -> Result<T, E> {
    record("model_check", || {
        json!({"stage":stage,"phase":phase,
        "status":if result.is_ok() {"completed"} else {"failed"},
        "error":result.as_ref().err().map(ToString::to_string)})
    });
    result
}

pub(crate) fn validation<T: Serialize, E: std::fmt::Display>(
    stage: &str,
    result: &Result<T, E>,
    started: std::time::Instant,
) {
    record("validation", || match result {
        Ok(value) => {
            json!({"stage":stage,"status":"completed","parsed_result":value,"duration_ms":started.elapsed().as_millis()})
        }
        Err(error) => {
            json!({"stage":stage,"status":"failed","error":error.to_string(),"duration_ms":started.elapsed().as_millis()})
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn af368_v2_context_is_explicit_private_and_cross_thread() {
        let context = TraceContext::source("internal-source", 2, 3, "internal-attempt");
        let body = json!({"stage":"w1_v5","call_id":"internal-call","operation_id":"host-private-operation",
            "session_id":"host-private-session","turn":"host-private-turn","request_id":"host-private-request",
            "request":{"input":"private original"}});
        let first = context.envelope("model_start", body.clone());
        let worker = context.clone();
        let other = std::thread::spawn(move || {
            let _unrelated = Operation::enter("different-host-operation");
            worker.envelope("model_end", body)
        })
        .join()
        .unwrap();
        assert_eq!(first["trace_version"], "2");
        assert_eq!(first["meta"]["trace_id"], other["meta"]["trace_id"]);
        assert_eq!(first["meta"]["generation"], 2);
        assert_eq!(first["meta"]["round"], 3);
        assert_eq!(first["meta"]["source_id"], "internal-source");
        assert_eq!(first["meta"]["attempt_id"], "internal-attempt");
        assert_eq!(first["content_deps"]["deps_complete"], false);
        assert!(!first["meta"].to_string().contains("host-private"));
        assert!(!first["meta"].to_string().contains("private original"));
        assert!(first.get("run_id").is_none());
        let retry = context.envelope("model_start", json!({"call_id":"new-internal-call"}));
        assert_eq!(retry["meta"]["trace_id"], first["meta"]["trace_id"]);
        assert_ne!(retry["meta"]["call_id"], first["meta"]["call_id"]);
        assert_ne!(
            TraceContext::source("internal-source", 2, 3, "new-attempt").trace_id,
            context.trace_id
        );
        let legacy = legacy_context("host-private-operation");
        assert_eq!(
            legacy.trace_id,
            legacy_context("host-private-operation").trace_id
        );
        assert!(
            !legacy.envelope("run_end", json!({}))["meta"]
                .to_string()
                .contains("host-private")
        );
    }

    #[test]
    fn model_capture_is_scoped_and_restored_after_panics() {
        let (_, outer) = capture_model_events(|| {
            assert!(std::panic::catch_unwind(|| capture_model_events(|| panic!("test"))).is_err());
            let _ = checked("W3", "decode", Err::<(), _>("bad JSON"));
            record("unrelated", || json!({"text":"excluded"}));
        });
        assert_eq!(outer.len(), 1);
        assert_eq!(outer[0]["data"]["phase"], "decode");
        assert!(MODEL_EVENTS.with(|events| events.borrow().is_none()));
    }

    #[test]
    fn redaction_preserves_nonsecret_request_bytes() {
        let original = json!({"input":"{\"previous_output\":\"{ \\\"candidates\\\": [] }\"}",
            "instructions":"task-specific work and landmark-based time"});
        let mut value = original.clone();
        scrub(&mut value);
        assert_eq!(value, original);
        let mut secrets = json!({"authorization":"credential", "input":"{\"api_key\":\"private\"}",
            "text":"Bearer token-one sk-token-two ark-token-three"});
        scrub(&mut secrets);
        assert_eq!(secrets["authorization"], "[REDACTED]");
        assert!(!secrets.to_string().contains("private"));
        assert!(!secrets.to_string().contains("token-"));
    }
}
