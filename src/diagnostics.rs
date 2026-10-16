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

/// Bind only frozen, actually consumed content. Unknown text never becomes complete
/// merely because it contains an ID. Dependency lists are bounded independently of bodies.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct ContentDeps {
    deps_complete: bool,
    frozen_input_fingerprint: Option<String>,
    reasons: std::collections::BTreeSet<&'static str>,
    sources: std::collections::BTreeMap<String, Value>,
    memories: std::collections::BTreeMap<crate::MemoryId, Value>,
}
impl ContentDeps {
    pub(crate) fn complete() -> Self {
        Self {
            deps_complete: true,
            frozen_input_fingerprint: None,
            reasons: Default::default(),
            sources: Default::default(),
            memories: Default::default(),
        }
    }
    pub(crate) fn incomplete(reason: &'static str) -> Self {
        let mut deps = Self::complete();
        deps.block(reason);
        deps
    }
    pub(crate) fn block(&mut self, reason: &'static str) {
        self.deps_complete = false;
        self.reasons.insert(reason);
    }
    pub(crate) fn summary_ids(&mut self, count: usize) {
        if count > 128 {
            self.block("summary_ids_truncated");
        }
    }
    pub(crate) fn source(&mut self, owner: &crate::UserId, source: &crate::SourceRecord) {
        if self.sources.contains_key(&source.id) {
            return;
        }
        if self.sources.len() >= 128 {
            self.block("dependency_limit");
            return;
        }
        match crate::enrichment::SourceInput::of(source) {
            Ok(input) if source.enrichment != "deleted" => {
                if !input.history.is_empty() {
                    self.block("unbound_history");
                }
                if input.provenance.is_some() {
                    self.block("unbound_provenance");
                }
                self.sources.insert(source.id.clone(), json!({"owner":owner,"source_id":source.id,
                    "generation":source.task.generation,"fingerprint":digest(&json!([input,source.task.generation,source.request,source.management,source.observed_at,source.received_sequence]))}));
            }
            _ => self.block("source_unavailable"),
        }
    }
    pub(crate) fn references(
        &mut self,
        owner: &crate::UserId,
        state: &crate::PersistedMemoryState,
        value: &Value,
    ) {
        match value {
            Value::Object(fields) => {
                if let Some(id) = fields.get("source_id").and_then(Value::as_str) {
                    if let Some(source) = state.sources.values().find(|s| s.id == id) {
                        self.source(owner, source);
                    } else {
                        self.block("source_unavailable");
                    }
                }
                if fields.get("kind").and_then(Value::as_str) == Some("current") {
                    self.block("unbound_current_message");
                }
                for (key, value) in fields {
                    if matches!(key.as_str(), "history" | "members") {
                        if let Some(ids) = value.as_array() {
                            for id in ids {
                                if let Some(id) = id.as_u64() {
                                    self.memory(owner, state, crate::MemoryId(id));
                                }
                            }
                        }
                    }
                    self.references(owner, state, value);
                }
            }
            Value::Array(values) => {
                for value in values {
                    self.references(owner, state, value);
                }
            }
            _ => {}
        }
    }
    pub(crate) fn memory(
        &mut self,
        owner: &crate::UserId,
        state: &crate::PersistedMemoryState,
        id: crate::MemoryId,
    ) {
        if self.memories.contains_key(&id) {
            return;
        }
        if self.memories.len() >= 128 {
            self.block("dependency_limit");
            return;
        }
        use crate::MemoryRepository;
        let Some(memory) = state.repository.get(id).ok().flatten() else {
            self.block("record_unavailable");
            return;
        };
        if memory.owner_id != *owner || memory.status == crate::MemoryStatus::Deleted {
            self.block("record_unavailable");
            return;
        }
        if memory.evidence.is_empty() {
            self.block("record_evidence_unbound");
        }
        for evidence in &memory.evidence {
            let mut matches = state.sources.values().filter(|source| {
                source.session_id == evidence.session_id.0 && source.turn_id == evidence.turn_id.0
            });
            match (matches.next(), matches.next()) {
                (Some(source), None) => {
                    self.source(owner, source);
                    if !crate::lexical::body(source)
                        .is_some_and(|text| text.contains(&evidence.quote))
                    {
                        self.block("record_evidence_unbound");
                    }
                }
                _ => self.block("record_evidence_unbound"),
            }
        }
        let Some(view) = crate::w4::record_view(state, id) else {
            self.block("record_unavailable");
            return;
        };
        let view = json!(view);
        self.memories.insert(
            id,
            json!({"owner":owner,"memory_id":id,"fingerprint":digest(&json!([memory,view]))}),
        );
        if view["format"] == "legacy" {
            self.block("legacy_record");
        }
        self.references(owner, state, &view);
    }
}
fn digest(value: &Value) -> String {
    crate::openai::recall_models::fingerprint(value)
}
/// Expensive full snapshots are a separate, explicit local debug option.
pub(crate) fn full_snapshots() -> bool {
    enabled() && std::env::var("PMR_TRACE_FULL_SNAPSHOTS").as_deref() == Ok("1")
}

/// Diagnostic-only context. Never serialized into model input, checkpoints or budget policy.
#[derive(Clone, Debug)]
pub struct TraceContext {
    trace_id: String,
    deps: ContentDeps,
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
            deps: ContentDeps::incomplete("not_proven"),
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
    pub(crate) fn unbound(mut self, reason: &'static str) -> Self {
        self.deps.block(reason);
        self
    }
    pub(crate) fn frozen(mut self, input: &Value) -> Self {
        if enabled() {
            self.deps.frozen_input_fingerprint = Some(digest(input));
        }
        self
    }
    pub(crate) fn with_deps(mut self, deps: ContentDeps) -> Self {
        self.deps = deps;
        self
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
            "meta":meta,"content_deps":self.deps,"body":body})
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

fn unbound_reason(kind: &str) -> &'static str {
    match kind {
        "run_start" | "run_end" | "rpc_response" => "unbound_host_payload",
        "validation" | "transaction_committed" | "transaction_rollback" => {
            "snapshot_or_unbound_state"
        }
        "global_context" => "entity_directory_or_unbound_state",
        "source_receipt" => "unbound_receipt",
        "lexical_raw" => "unbound_retrieval",
        "retrieval" | "fact_index_refresh" => "unbound_diagnostic",
        _ => "not_proven",
    }
}
pub fn record_for(run_id: &str, kind: &str, data: impl FnOnce() -> Value) {
    if !enabled() && !collecting(kind) {
        return;
    }
    // Legacy callers cannot prove dependencies. Never expose their host-supplied ID.
    let context = legacy_context(run_id).with_deps(ContentDeps::incomplete(unbound_reason(kind)));
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
        record_context(
            &legacy_context(&id).with_deps(ContentDeps::incomplete(unbound_reason(kind))),
            kind,
            data,
        );
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

    fn source(history: Vec<&str>) -> crate::SourceRecord {
        serde_json::from_value(json!({"id":"s-fictional","session_id":"host-session","turn_id":"host-turn",
            "observed_at":1,"request":{"text":"虚构记录正文","history":history,"capture_source":true},"enrichment":"committed",
            "error":null,"result":null,"base_revision":0,"index":{},"task":{}})).unwrap()
    }
    #[test]
    fn af371_dependency_bindings_and_output_inheritance() {
        let owner = crate::UserId::from("fixture-owner");
        let mut source = source(vec![]);
        let mut deps = ContentDeps::complete();
        deps.source(&owner, &source);
        assert!(deps.deps_complete);
        let old = deps.sources[&source.id].clone();
        assert_eq!(old["owner"], "fixture-owner");
        source.task.generation += 1;
        let mut newer = ContentDeps::complete();
        newer.source(&owner, &source);
        assert_ne!(old["fingerprint"], newer.sources[&source.id]["fingerprint"]);
        source.request.as_mut().unwrap()["text"] = json!("另一段虚构正文");
        let mut changed = ContentDeps::complete();
        changed.source(&owner, &source);
        assert_ne!(
            newer.sources[&source.id]["fingerprint"],
            changed.sources[&source.id]["fingerprint"]
        );
        let ctx = TraceContext::new()
            .with_deps(deps)
            .frozen(&json!({"source":"s-fictional"}));
        let start = ctx.envelope("model_start", json!({}));
        let end =
            std::thread::spawn(move || ctx.envelope("model_end", json!({"output":"fictional"})))
                .join()
                .unwrap();
        assert_eq!(start["content_deps"], end["content_deps"]);
        assert!(!start["content_deps"].to_string().contains("虚构记录正文"));
    }
    #[test]
    fn af371_unbound_history_query_and_missing_dependencies_never_complete() {
        let owner = crate::UserId::from("fixture");
        let mut deps = ContentDeps::complete();
        deps.source(&owner, &source(vec!["未绑定的历史"]));
        assert!(!deps.deps_complete);
        assert!(deps.reasons.contains("unbound_history"));
        let mut deps = ContentDeps::complete();
        deps.references(
            &owner,
            &crate::PersistedMemoryState::default(),
            &json!({"source_id":"missing"}),
        );
        assert!(!deps.deps_complete);
        let mut deps = ContentDeps::complete();
        deps.references(&owner, &Default::default(), &json!({"kind":"current"}));
        assert!(deps.reasons.contains("unbound_current_message"));
        for reason in [
            "unbound_query_context",
            "entity_directory",
            "snapshot_or_unbound_state",
            "summary_ids_truncated",
        ] {
            let ctx = TraceContext::new().with_deps(ContentDeps::incomplete(reason));
            assert_eq!(
                ctx.envelope("test", json!({}))["content_deps"]["deps_complete"],
                false
            );
        }
        let mut deps = ContentDeps::complete();
        for n in 0..129 {
            let mut s = source(vec![]);
            s.id = format!("source-{n}");
            deps.source(&owner, &s);
        }
        assert_eq!(deps.sources.len(), 128);
        assert!(deps.reasons.contains("dependency_limit"));
    }

    #[test]
    fn af371_memory_binding_changes_and_deleted_records_are_not_proven() {
        use crate::*;
        let owner = UserId::from("fixture");
        let candidate = CandidateMemory::new(
            owner.clone(),
            EntityId::from("self"),
            MemoryType::Fact,
            Predicate::from("fact"),
            MemoryValue::Text("虚构断言".into()),
            Timestamp(1),
            Evidence::new(
                EvidenceId::from("e"),
                SessionId::from("host-session"),
                TurnId::from("host-turn"),
                "虚构记录正文",
            ),
        );
        let mut state = PersistedMemoryState::default();
        state.sources.insert("key".into(), source(vec![]));
        let mut memory = Memory::from_candidate(MemoryId(1), &candidate, None);
        state.repository.insert(memory.clone()).unwrap();
        let mut deps = ContentDeps::complete();
        deps.memory(&owner, &state, memory.id);
        let old = deps.memories[&memory.id].clone();
        assert!(deps.reasons.contains("legacy_record"));
        memory.value = MemoryValue::Text("改过的虚构断言".into());
        state.repository.update(memory.clone()).unwrap();
        let mut changed = ContentDeps::complete();
        changed.memory(&owner, &state, memory.id);
        assert_ne!(
            old["fingerprint"],
            changed.memories[&memory.id]["fingerprint"]
        );
        memory.status = MemoryStatus::Deleted;
        state.repository.update(memory.clone()).unwrap();
        let mut deleted = ContentDeps::complete();
        deleted.memory(&owner, &state, memory.id);
        assert!(!deleted.deps_complete);
        assert!(deleted.memories.is_empty());
        let mut bounded = ContentDeps::complete();
        bounded.summary_ids(128);
        assert!(bounded.deps_complete);
        bounded.summary_ids(129);
        assert!(!bounded.deps_complete);
    }

    #[test]
    fn af371_transaction_snapshot_is_summary_by_default() {
        use std::sync::{Arc, Mutex};
        struct Sink(Arc<Mutex<Vec<u8>>>);
        impl Write for Sink {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let ledger = crate::model_budget::tests::TestLedger::new();
        let mut store =
            crate::SqliteMemoryStore::open(&ledger.path, crate::UserId::from("fixture")).unwrap();
        let rows = Arc::new(Mutex::new(Vec::new()));
        with_test_writer(Box::new(Sink(rows.clone())), || {
            let _op = Operation::enter("private-host-id");
            store
                .transaction(|state| {
                    for n in 0..100 {
                        let mut source = source(vec![]);
                        source.id = format!("source-{n}");
                        source.request.as_mut().unwrap()["text"] =
                            json!("fictional-content-".repeat(60));
                        state.sources.insert(n.to_string(), source);
                    }
                    Ok(())
                })
                .unwrap();
        });
        let bytes = rows.lock().unwrap();
        let event: Value =
            serde_json::from_slice(bytes.split(|b| *b == b'\n').next().unwrap()).unwrap();
        assert_eq!(event["kind"], "transaction_committed");
        assert!(event["body"]["before"].is_null());
        assert!(event["body"]["after"].is_null());
        assert_eq!(event["body"]["counts"]["sources"], 100);
        assert!(!String::from_utf8_lossy(&bytes).contains("fictional-content"));
        let state = store.read().unwrap();
        let old_size = json!({"before":state,"after":state}).to_string().len();
        eprintln!(
            "AF371 synthetic steady-state transaction: full_snapshots={old_size} bytes; summary={} bytes",
            bytes.len()
        );
        assert!(bytes.len() * 10 < old_size);
    }

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
