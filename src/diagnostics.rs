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

pub fn new_id() -> String {
    format!(
        "{}-{}-{}",
        std::process::id(),
        time::OffsetDateTime::now_utc().unix_timestamp_nanos(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

pub fn enabled() -> bool {
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
    if !enabled() {
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
    let mut value = data();
    scrub(&mut value);
    let row = json!({"trace_version":"1", "event_id":new_id(), "run_id":run_id, "kind":kind,
        "at":time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339).ok(), "data":value});
    let Ok(mut guard) = writer.lock() else {
        failed();
        return;
    };
    let Some(file) = guard.as_mut() else {
        return;
    };
    // ponytail: synchronous JSONL and full snapshots for bounded local diagnostics;
    // use a bounded collector/deltas if measured profile size makes this expensive.
    if writeln!(file, "{row}").and_then(|_| file.flush()).is_err() {
        failed();
    }
}

pub(crate) fn record(kind: &str, data: impl FnOnce() -> Value) {
    let collect = MODEL_EVENTS.with(|events| events.borrow().is_some())
        && matches!(kind, "model_start" | "model_end" | "model_check");
    if !enabled() && !collect {
        return;
    }
    let mut data = data();
    if collect {
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
    RUN.with(|run| {
        if let Some(id) = run.borrow().as_ref() {
            record_for(id, kind, || std::mem::take(&mut data));
        }
    });
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
