//! Shared test-only strict recorder and production case driver; no live transport here.
use super::*;
use std::collections::BTreeSet;

pub(super) const LABEL: &str = "ac2-pro-medium-pro-minimal-pro-minimal-r4-compute-low";
pub(super) const PRO: &str = "ep-20261003130756-trhj6";
pub(super) const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/eval");

pub(super) fn replay_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/eval-replay")
        .join(LABEL)
}

pub(super) fn default_run_output(dir: &Path, recording: bool) -> PathBuf {
    if recording {
        dir.join("run.json")
    } else {
        replay_dir().join("run-replay.json")
    }
}

pub(super) fn read_target_plan(dir: &Path) -> Result<Option<Value>, String> {
    let path = dir.join("target-plan.json");
    if !path.exists() {
        return Ok(None);
    }
    let plan: Value = serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    if plan["version"] != "eval.e2e.targeted@1" {
        return Err("unknown targeted plan version".into());
    }
    Ok(Some(plan))
}

pub(super) fn stage_name(request: &Value) -> &str {
    match request
        .pointer("/text/format/name")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
    {
        "w1_v1" | "w1_v2" | "w1_v4" => "W1",
        "w3_v1" | "w3_v2" | "w3_v4" => "W3",
        "w4_v2" | "w4_v3" | "w4_v5" => "W4",
        "r1_v1" | "r1_v2" => "R1",
        "r4_v3" | "r4_v4" | "r4_v5" | "r4_v7" => "R4",
        other => other,
    }
}

/// Exact reservation ceiling; future model-dependent sends remain unknown.
pub(super) fn target_ceiling(dir: &Path, cases: &[Value]) -> Result<Option<u64>, String> {
    let Some(plan) = read_target_plan(dir)? else {
        return Ok(None);
    };
    let wanted: BTreeSet<_> = cases
        .iter()
        .flat_map(|c| {
            c["questions"].as_array().unwrap().iter().map(|q| {
                format!(
                    "{}/{}",
                    c["id"].as_str().unwrap(),
                    q["id"].as_str().unwrap()
                )
            })
        })
        .collect();
    let selected: BTreeSet<_> = plan["questions"]
        .as_array()
        .ok_or("invalid selected questions")?
        .iter()
        .map(|q| q.as_str().map(String::from).ok_or("invalid question key"))
        .collect::<Result<_, _>>()?;
    let planned = plan["cases"].as_object().ok_or("invalid planned cases")?;
    if wanted != selected || cases.len() != planned.len() {
        return Err("整批拒绝：定点计划与所选题目不符".into());
    }
    let mut total = 0;
    let mut stages = BTreeMap::from([("W1", 0u64), ("W3", 0), ("W4", 0), ("R1", 0), ("R4", 0)]);
    let mut cfg = config("configuration", "", 1);
    cfg.model_budget = None;
    let configuration = serde_json::to_value(cfg).unwrap();
    for case in cases {
        let cid = case["id"].as_str().unwrap();
        let entry = planned.get(cid).ok_or("case not planned")?;
        let mut ops: BTreeSet<_> = case["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| format!("write:{}", m["id"].as_str().unwrap()))
            .collect();
        ops.extend(
            case["questions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|q| format!("recall:{}", q["id"].as_str().unwrap())),
        );
        let prefixes = entry["prefixes"]
            .as_object()
            .ok_or("invalid planned prefixes")?;
        let slots = entry["slots"].as_object().ok_or("invalid planned slots")?;
        if prefixes.keys().cloned().collect::<BTreeSet<_>>() != ops
            || slots.keys().cloned().collect::<BTreeSet<_>>() != ops
        {
            return Err("整批拒绝：计划操作范围不符".into());
        }
        let doc: Value = serde_json::from_slice(
            &std::fs::read(dir.join(format!("{cid}.json"))).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        if recording_configuration(&doc["configuration"]) != configuration {
            return Err(format!("整批拒绝：录制配置不符 {cid}"));
        }
        for op in ops {
            let prefix = prefixes[&op].as_array().ok_or("invalid prefix")?;
            for (i, hash) in prefix.iter().enumerate() {
                if !hash.is_string() || doc["operations"][&op][i]["request"] != *hash {
                    return Err(format!("整批拒绝：保留前缀已改变 {cid}/{op}/{i}"));
                }
            }
            let reserved = slots[&op].as_array().ok_or("invalid slots")?;
            let unique: BTreeSet<_> = reserved.iter().filter_map(Value::as_str).collect();
            if unique.len() != reserved.len()
                || unique
                    .iter()
                    .any(|s| !["W1", "W3", "W4", "R1", "R4"].contains(s))
            {
                return Err("invalid reserved stages".into());
            }
            total += reserved.len() as u64;
            for stage in unique {
                *stages.get_mut(stage).unwrap() += 1;
            }
        }
    }
    if plan["call_ceiling"].as_u64() != Some(total) || plan["reserved"] != json!(stages) {
        return Err("整批拒绝：计划调用上限不符".into());
    }
    Ok(Some(total))
}

pub(super) fn config(case: &str, address: &str, calls: u64) -> ChatConfig {
    let mut config: ChatConfig = serde_json::from_value(json!({
        "write_pipeline":"w4_runtime_v1", "recall_pipeline":"recall_v3",
        "enrichment_mode":"deferred", "fact_index_enabled":true,
        "background_operation_limit":3,
        "write_models":{"w1":PRO,"w3":PRO,"w4":PRO,
            "reasoning_effort":{"w1":"medium","w3":"minimal","w4":"medium"}},
        "recall_models":{"r1":PRO,"r4":PRO,
            "r4_compute_reasoning_effort":"low",
            "reasoning_effort":{"r1":"minimal","r4":"minimal"}},
        "model_budget":{"group_id":format!("eval-v3-{case}"),
            "max_calls":calls.max(1),"max_operation_calls":2,
            "max_input_bytes":256000,"query_output_tokens":4096,"review_output_tokens":16384}
    }))
    .unwrap();
    config.model_base_url = Some(format!("http://{address}/v1/case/{case}"));
    config
}

pub(super) fn cases() -> Vec<Value> {
    let questions = std::env::var("EVAL_QUESTIONS").unwrap_or_else(|_| {
        std::env::var_os("EVAL_V3_DIR")
            .and_then(|dir| read_target_plan(Path::new(&dir)).unwrap())
            .map(|p| {
                p["questions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|s| s.as_str().unwrap())
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default()
    });
    let requested: BTreeSet<_> = questions
        .split(',')
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_owned())
        .collect();
    let only = std::env::var("EVAL_CASES").unwrap_or_default();
    let only = if only == "quick" {
        QUICK.join(",")
    } else {
        only
    };
    let mut paths: Vec<_> = std::fs::read_dir(Path::new(ROOT).join("cases"))
        .unwrap()
        .map(|p| p.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .filter(|p| {
            only.is_empty()
                || only.split(',').any(|s| {
                    p.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with(s.trim())
                })
        })
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "no eval cases");
    let mut cases: Vec<Value> = paths
        .iter()
        .map(|p| serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap())
        .collect();
    if !requested.is_empty() {
        let mut found = BTreeSet::new();
        for case in &mut cases {
            let id = case["id"].as_str().unwrap().to_owned();
            case["questions"].as_array_mut().unwrap().retain(|q| {
                let key = format!("{id}/{}", q["id"].as_str().unwrap());
                requested.contains(&key) && {
                    found.insert(key);
                    true
                }
            });
            let end = case["questions"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|q| q["after"].as_u64())
                .max()
                .unwrap_or(0) as usize;
            case["messages"].as_array_mut().unwrap().truncate(end);
        }
        assert_eq!(
            found, requested,
            "EVAL_QUESTIONS contains unavailable questions"
        );
        cases.retain(|c| !c["questions"].as_array().unwrap().is_empty());
    }
    cases
}

pub(super) fn counts(case: &Value) -> [u64; 5] {
    let m = case["messages"].as_array().unwrap().len() as u64;
    let q = case["questions"].as_array().unwrap().len() as u64;
    // Fresh databases contain only entities, never facts: the first message cannot call W4.
    [m, m, m.saturating_sub(1), q, q]
}

/// Only volatile *structural* source identifiers are canonicalized. Never rewrite quotes,
/// sentences, values, USER text/history, or arbitrary string leaves. IDs in database records
/// and relations are deterministic counters and remain fully fingerprinted.
pub(super) fn ids(value: &mut Value, mapping: &BTreeMap<String, String>) {
    match value {
        Value::Array(a) => {
            for v in a {
                ids(v, mapping);
            }
        }
        Value::Object(o) => {
            if let Some(Value::String(id)) = o.get_mut("source_id")
                && let Some(stable) = mapping.get(id)
            {
                *id = stable.clone();
            }
            if let Some(Value::Array(snapshot)) = o.get_mut("source_snapshot")
                && let Some(Value::String(id)) = snapshot.first_mut()
                && let Some(stable) = mapping.get(id)
            {
                *id = stable.clone();
            }
            if let Some(Value::Object(sources)) = o.get_mut("sources") {
                *sources = std::mem::take(sources)
                    .into_iter()
                    .map(|(k, v)| (mapping.get(&k).cloned().unwrap_or(k), v))
                    .collect();
            }
            for v in o.values_mut() {
                ids(v, mapping);
            }
        }
        _ => {}
    }
}

pub(super) fn normalized(
    request: &Value,
    mapping: &BTreeMap<String, String>,
) -> Result<Value, String> {
    if !request.is_object() {
        return Err("proxy request must be an object".into());
    }
    let mut value = request.clone();
    let mut input: Value = serde_json::from_str(
        value["input"]
            .as_str()
            .ok_or("proxy input must be JSON text")?,
    )
    .map_err(|e| format!("proxy input: {e}"))?;
    ids(&mut input, mapping);
    value["input"] = input;
    Ok(value)
}

pub(super) fn response_ids(body: &str, mapping: &BTreeMap<String, String>) -> String {
    // Preserve failed, non-JSON and partial model responses verbatim for diagnostics/replay.
    let Ok(mut envelope) = serde_json::from_str::<Value>(body) else {
        return body.into();
    };
    if let Some(output) = envelope.get_mut("output").and_then(Value::as_array_mut) {
        for message in output {
            if let Some(content) = message.get_mut("content").and_then(Value::as_array_mut) {
                for part in content {
                    if part["type"] == "output_text"
                        && let Some(text) = part["text"].as_str()
                        && let Ok(mut value) = serde_json::from_str::<Value>(text)
                    {
                        ids(&mut value, mapping);
                        part["text"] = json!(value.to_string());
                    }
                }
            }
        }
    }
    envelope.to_string()
}

#[derive(Default)]
pub(super) struct Operation {
    pub(super) id: String,
    pub(super) position: usize,
    pub(super) missing: Option<String>,
    // runtime -> stable message ID; registered after T1, before W1.
    pub(super) ids: BTreeMap<String, String>,
}

// Retired execution settings are inert for the only supported pipelines.
// This archive-metadata comparison never changes requests, fingerprints or recordings.
fn recording_configuration(value: &Value) -> Value {
    let mut value = value.clone();
    if let Some(fields) = value.as_object_mut() {
        for field in [
            "retrieval_mode",
            "local_recall_enabled",
            "local_recall_budget",
            "is_a",
        ] {
            fields.remove(field);
        }
    }
    value
}

#[test]
fn retired_recording_configuration_keeps_active_bindings_strict() {
    let current = json!({"recall_pipeline":"recall_v3","write_pipeline":"w4_runtime_v1", "predicates":["fact"], "utc_offset_seconds":28800, "recall_models":{"r4":"endpoint"}});
    let mut archived = current.clone();
    for field in [
        "retrieval_mode",
        "local_recall_enabled",
        "local_recall_budget",
        "is_a",
    ] {
        archived[field] = json!({"archived":"inert"});
    }
    assert_eq!(recording_configuration(&archived), current);
    for field in [
        "recall_pipeline",
        "write_pipeline",
        "predicates",
        "utc_offset_seconds",
        "recall_models",
    ] {
        let mut changed = archived.clone();
        changed[field] = json!("changed");
        assert_ne!(recording_configuration(&changed), current, "{field}");
    }
}

// This core is also compiled into the offline library replay tests. The opt-in
// integration runner supplies these hooks without pulling its live runner into the library.
pub(super) trait AbHooks: Send + Sync + std::panic::RefUnwindSafe {
    fn config(&self, case: &str, address: &str, calls: u64) -> ChatConfig;
    fn upstream(&self, request: &Value) -> Result<&(String, String), String>;
    fn invalidate(&self);
    fn checkpoint(
        &self,
        recorder: &Recorder,
        case: &str,
        key: &str,
        db: &TempDb,
        ids: &BTreeMap<String, String>,
    ) -> Result<(), String>;
    #[allow(clippy::too_many_arguments)]
    fn respond(
        &self,
        recorder: &Recorder,
        case: &str,
        op: &str,
        position: usize,
        request: &Value,
        canonical: &Value,
        mapping: &BTreeMap<String, String>,
        send: &mut dyn FnMut() -> Result<(u16, String), String>,
    ) -> Result<(u16, String), String>;
}

pub(super) struct Recorder {
    pub(super) dir: PathBuf,
    pub(super) configuration: Value,
    pub(super) allowance: Mutex<Option<u64>>,
    pub(super) live: Option<(String, String)>,
    pub(super) operations: Mutex<HashMap<String, Operation>>,
    pub(super) documents: Mutex<HashMap<String, Value>>,
    pub(super) observations: Mutex<HashMap<String, Vec<Value>>>,
    // prepare saves T1 before its foreground call. Evaluator-only database
    // binding registers those IDs before canonicalizing that first request.
    source_databases: Mutex<HashMap<String, PathBuf>>,
    pub(super) run_id: String,
    pub(super) send_log: Mutex<Value>,
    pub(super) log_dir: PathBuf,
    pub(super) sent: AtomicUsize,
    pub(super) ab: Option<Arc<dyn AbHooks>>,
    target_plan: Option<Value>,
    target_refused: std::sync::atomic::AtomicBool,
    target_used: Mutex<BTreeSet<(String, String, String)>>,
}

impl Recorder {
    pub(super) fn seed_mvp(&mut self, document: Value, plan: Value) {
        self.documents
            .get_mut()
            .unwrap()
            .insert("m15_mvp".into(), document);
        self.target_plan = Some(plan);
    }
    pub(super) fn new(dir: PathBuf, limit: Option<u64>, live: Option<(String, String)>) -> Self {
        let target_plan = read_target_plan(&dir).unwrap();
        let mut cfg = config("configuration", "", 1);
        cfg.model_budget = None;
        let run_id = diagnostics::new_id();
        let log_dir = if limit.is_some() {
            dir.join("runs")
        } else {
            replay_dir().join("runs")
        };
        Self {
            dir,
            configuration: serde_json::to_value(cfg).unwrap(),
            allowance: Mutex::new(limit),
            live,
            operations: Mutex::default(),
            documents: Mutex::default(),
            observations: Mutex::default(),
            source_databases: Mutex::default(),
            send_log: Mutex::new(json!({"version":"eval.e2e.sends@1","run_id":run_id,
                "limit":limit,"attempts":[],"proxy_errors":[]})),
            log_dir,
            run_id,
            sent: AtomicUsize::new(0),
            ab: None,
            target_plan,
            target_refused: std::sync::atomic::AtomicBool::new(false),
            target_used: Mutex::default(),
        }
    }
    pub(super) fn begin(&self, case: &str, operation: &str, source_ids: &BTreeMap<String, String>) {
        self.operations.lock().unwrap().insert(
            case.into(),
            Operation {
                id: operation.into(),
                ids: source_ids
                    .iter()
                    .map(|(stable, runtime)| (runtime.clone(), stable.clone()))
                    .collect(),
                ..Operation::default()
            },
        );
        let mut docs = self.documents.lock().unwrap();
        if !docs.contains_key(case) {
            let path = self.dir.join(format!("{case}.json"));
            let doc = if path.exists() {
                serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
            } else {
                json!({"version":"eval.e2e.v3@1","configuration":self.configuration,"operations":{}})
            };
            assert_eq!(
                recording_configuration(&doc["configuration"]),
                self.configuration,
                "配置标签已绑定其他配置，必须使用单独目录"
            );
            docs.insert(case.into(), doc);
        }
    }
    pub(super) fn missing(&self, case: &str) -> Option<String> {
        self.operations.lock().unwrap()[case].missing.clone()
    }
    pub(super) fn bind_source_database(&self, case: &str, path: &Path) {
        self.source_databases
            .lock()
            .unwrap()
            .insert(case.into(), path.into());
    }
    pub(super) fn calls(&self, case: &str) -> Vec<Value> {
        self.observations
            .lock()
            .unwrap()
            .get(case)
            .cloned()
            .unwrap_or_default()
    }
    pub(super) fn send_log_path(&self) -> PathBuf {
        self.log_dir.join(format!("{}.json", self.run_id))
    }
    // Persist BEFORE entering the physical send. An interrupted `started` entry is a
    // possibly billed attempt, not proof the remote endpoint received it.
    pub(super) fn start_send(&self, mut attempt: Value) -> Result<usize, String> {
        let mut log = self.send_log.lock().map_err(|_| "send log lock poisoned")?;
        let mut next = log.clone();
        let attempts = next["attempts"].as_array_mut().ok_or("invalid send log")?;
        let id = attempts.len();
        attempt["id"] = json!(id);
        attempt["state"] = json!("started");
        attempts.push(attempt);
        try_write_json(&self.send_log_path(), &next)?;
        *log = next;
        self.sent.fetch_add(1, Ordering::Relaxed);
        Ok(id)
    }
    pub(super) fn finish_send(&self, id: usize, recording: &Value) -> Result<(), String> {
        let mut log = self.send_log.lock().map_err(|_| "send log lock poisoned")?;
        let mut next = log.clone();
        let attempt = next["attempts"].get_mut(id).ok_or("send entry missing")?;
        attempt["state"] = json!("recorded");
        for key in [
            "http_status",
            "status",
            "usage",
            "elapsed_ms",
            "incomplete_details",
            "transport_error",
        ] {
            attempt[key] = recording[key].clone();
        }
        try_write_json(&self.send_log_path(), &next)?;
        *log = next;
        Ok(())
    }
    pub(super) fn proxy_error(&self, case: Option<&str>, error: String) -> (u16, String) {
        eprintln!("EVAL_PROXY_ERROR: {error}");
        if let Ok(mut log) = self.send_log.lock() {
            if let Some(errors) = log["proxy_errors"].as_array_mut() {
                errors.push(json!({"case":case,"error":error}));
            }
            if let Err(e) = try_write_json(&self.send_log_path(), &log) {
                eprintln!("EVAL_PROXY_DIAGNOSTIC_WRITE_FAILED: {e}");
            }
        }
        (
            503,
            json!({"error":"eval_proxy_failed","detail":error}).to_string(),
        )
    }
    pub(super) fn observe(
        &self,
        case: &str,
        operation: &str,
        request: &Value,
        recording: &Value,
        origin: &str,
        elapsed: Duration,
        reason: Option<&str>,
    ) {
        let stage = stage_name(request);
        let body: Value = recording["body"]
            .as_str()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        if let Ok(mut observations) = self.observations.lock() {
            observations.entry(case.into()).or_default().push(json!({
            "operation":operation,"stage":stage,"origin":origin,"sends":usize::from(origin=="live"),
            "elapsed_ms":elapsed.as_millis(),"recorded_elapsed_ms":recording["elapsed_ms"],
            "http_status":recording["http_status"],"response_status":body["status"],"usage":body["usage"],
            "incomplete_details":body["incomplete_details"],"transport_error":recording["transport_error"],
            "error":body["error"],"reason":reason}));
        }
    }
    /// Inject the *send* only in harness unit tests; production requests are never constructed here.
    pub(super) fn respond(
        &self,
        case: &str,
        request: &Value,
        send: impl FnOnce() -> Result<(u16, String), String>,
    ) -> (u16, String) {
        match self.try_respond(case, request, send) {
            Ok(response) => response,
            Err(error) => {
                if let Ok(mut ops) = self.operations.lock()
                    && let Some(op) = ops.get_mut(case)
                {
                    op.missing = Some(format!("代理错误：{error}"));
                }
                self.proxy_error(Some(case), error)
            }
        }
    }
    pub(super) fn try_respond(
        &self,
        case: &str,
        request: &Value,
        send: impl FnOnce() -> Result<(u16, String), String>,
    ) -> Result<(u16, String), String> {
        let handling = Instant::now();
        if let Some(path) = self
            .source_databases
            .lock()
            .map_err(|_| "source database lock poisoned")?
            .get(case)
        {
            let state = SqliteMemoryStore::open(path, UserId::from(OWNER))
                .and_then(|s| s.read())
                .map_err(|e| e.to_string())?;
            let mut operations = self
                .operations
                .lock()
                .map_err(|_| "operation lock poisoned")?;
            let op = operations
                .get_mut(case)
                .ok_or("proxy case has no active operation")?;
            for source in state.sources.values().filter(|s| s.request.is_some()) {
                op.ids.insert(source.id.clone(), source.turn_id.clone());
            }
        }
        let (operation, position, mapping) = {
            let mut ops = self
                .operations
                .lock()
                .map_err(|_| "operation lock poisoned")?;
            let op = ops
                .get_mut(case)
                .ok_or("proxy case has no active operation")?;
            let result = (op.id.clone(), op.position, op.ids.clone());
            op.position += 1;
            result
        };
        let canonical = normalized(request, &mapping)?;
        if let Some(arm) = &self.ab {
            let mut send = Some(send);
            let response = arm.respond(
                self,
                case,
                &operation,
                position,
                request,
                &canonical,
                &mapping,
                &mut || send.take().unwrap()(),
            );
            if response.is_err() {
                arm.invalidate();
            }
            return response;
        }
        let fp = recall_models::fingerprint(&canonical);
        let reverse = mapping
            .iter()
            .map(|(a, b)| (b.clone(), a.clone()))
            .collect();
        let saved = self
            .documents
            .lock()
            .map_err(|_| "recording lock poisoned")?
            .get(case)
            .ok_or("proxy case has no recording document")?["operations"][&operation][position]
            .clone();
        if let Some(plan) = &self.target_plan {
            let prefixes = plan["cases"][case]["prefixes"][&operation]
                .as_array()
                .ok_or("unplanned operation")?;
            let slots = plan["cases"][case]["slots"][&operation]
                .as_array()
                .ok_or("unplanned operation")?;
            let mismatch = prefixes
                .get(position)
                .is_some_and(|expected| expected != &json!(fp) || saved["request"] != *expected);
            let unplanned =
                position >= prefixes.len() && !slots.iter().any(|s| s == stage_name(request));
            if mismatch || unplanned || self.target_refused.load(Ordering::SeqCst) {
                self.target_refused.store(true, Ordering::SeqCst);
                return Err(format!(
                    "定点计划拒绝：保留前缀指纹改变或未预留阶段 {case}/{operation}/{position}；不补发"
                ));
            }
        }
        if saved["request"] == fp {
            let status = saved["http_status"]
                .as_u64()
                .and_then(|s| u16::try_from(s).ok())
                .ok_or("invalid recorded HTTP status")?;
            let body = saved["body"].as_str().ok_or("invalid recorded body")?;
            self.observe(
                case,
                &operation,
                request,
                &saved,
                "replay",
                handling.elapsed(),
                None,
            );
            return Ok((status, response_ids(body, &reverse)));
        }
        if self.target_plan.is_some()
            && self
                .allowance
                .lock()
                .map_err(|_| "allowance lock poisoned")?
                .is_some()
            && !self
                .target_used
                .lock()
                .map_err(|_| "target slot lock poisoned")?
                .insert((case.into(), operation.clone(), stage_name(request).into()))
        {
            self.target_refused.store(true, Ordering::SeqCst);
            return Err("定点计划拒绝：同一预留阶段不得重复发送".into());
        }
        // One global reservation, made under the lock BEFORE invoking the send closure.
        // Replays never reserve. Even concurrent failures consume their reserved attempt.
        let (send_id, exhausted) = {
            let mut limit = self
                .allowance
                .lock()
                .map_err(|_| "allowance lock poisoned")?;
            match limit.as_mut() {
                Some(n) if *n > 0 => {
                    let id = self.start_send(json!({"case":case,"operation":operation,"position":position,
                        "stage":request.pointer("/text/format/name"),"request":fp,"endpoint":request["model"]}))?;
                    *n -= 1;
                    (Some(id), false)
                }
                Some(_) => (None, true),
                None => (None, false),
            }
        };
        let Some(send_id) = send_id else {
            let reason = if exhausted {
                "额度耗尽（EVAL_RECORD）"
            } else if saved.is_null() {
                "未录制"
            } else {
                "录制过期（请求指纹不符）"
            };
            self.operations
                .lock()
                .map_err(|_| "operation lock poisoned")?
                .get_mut(case)
                .ok_or("proxy case has no active operation")?
                .missing = Some(reason.into());
            self.observe(
                case,
                &operation,
                request,
                &Value::Null,
                "not_recorded",
                handling.elapsed(),
                Some(reason),
            );
            return Ok((503, json!({"error":reason}).to_string()));
        };
        let started = Instant::now();
        let (status, body, failure) = match send() {
            Ok((s, b)) => (s, b, None),
            Err(e) => (
                503,
                json!({"error":"eval_transport_failed"}).to_string(),
                Some(e),
            ),
        };
        let envelope: Value = serde_json::from_str(&body).unwrap_or_default();
        let recording = json!({"schema":request.pointer("/text/format/name"),"endpoint":request["model"],
            "send_run":self.run_id,"send_index":send_id,
            "request":fp,"request_content":canonical,"http_status":status,"body":response_ids(&body,&mapping),
            "usage":envelope["usage"],"elapsed_ms":started.elapsed().as_millis(),
            "status":envelope["status"],"incomplete_details":envelope["incomplete_details"],"transport_error":failure});
        self.observe(
            case,
            &operation,
            request,
            &recording,
            "live",
            handling.elapsed(),
            None,
        );
        let mut docs = self
            .documents
            .lock()
            .map_err(|_| "recording lock poisoned")?;
        let mut doc = docs.get(case).ok_or("recording document missing")?.clone();
        let ops = doc["operations"]
            .as_object_mut()
            .ok_or("invalid recording operations")?;
        let list = ops
            .entry(operation)
            .or_insert(json!([]))
            .as_array_mut()
            .ok_or("invalid recorded operation")?;
        // A changed prefix invalidates subsequent stages; never reuse their old output by index.
        list.truncate(position);
        list.push(recording.clone());
        let target = self.dir.join(format!("{case}.json"));
        // A response is durable before returning it to production, not at case completion.
        try_write_json(&target, &doc)?;
        docs.insert(case.into(), doc);
        drop(docs);
        if let Err(e) = self.finish_send(send_id, &recording) {
            // The per-call recording is already durable; reports reconcile this window.
            self.proxy_error(Some(case), e);
        }
        Ok((status, body))
    }
}

pub(super) fn run_case(case: &Value, recorder: &Recorder, address: &str) -> Value {
    let case_id = case["id"].as_str().unwrap();
    let db = TempDb(std::env::temp_dir().join(format!(
        "eval-v3-{case_id}-{}.sqlite3",
        diagnostics::new_id()
    )));
    let service = ChatMemoryService::new(
        db.open(),
        recorder.ab.as_ref().map_or_else(
            || config(case_id, address, counts(case).iter().sum()),
            |arm| arm.config(case_id, address, counts(case).iter().sum()),
        ),
    )
    .unwrap();
    run_case_on(case, recorder, service, db)
}

pub(super) fn opaque_id(value: &Value) -> String {
    recall_models::fingerprint(value).replace(':', "_")
}

pub(super) fn run_case_on(
    case: &Value,
    recorder: &Recorder,
    mut service: ChatMemoryService,
    db: TempDb,
) -> Value {
    let case_id = case["id"].as_str().unwrap();
    service.call("memory_recall",json!({"query":"全局回答要求","bootstrap":true,"request_id":format!("{case_id}-bootstrap"),"at":CLOCK}),|| false).unwrap();
    for entity in case["entities"].as_array().unwrap() {
        service
            .call("memory_entity_upsert", entity.clone(), || false)
            .unwrap();
    }
    let messages = case["messages"].as_array().unwrap();
    let mut source_ids = BTreeMap::<String, String>::new();
    let mut steps = Vec::new();
    let mut blocked = None;
    for (index, message) in messages.iter().enumerate() {
        let id = message["id"].as_str().unwrap();
        if blocked.is_none() {
            let history: Vec<_> = messages[..index]
                .iter()
                .filter(|m| m.get("task_id") == message.get("task_id"))
                .map(|m| m["text"].clone())
                .collect();
            let session = if recorder.ab.is_some() {
                opaque_id(&json!(["session", case_id]))
            } else {
                format!("eval-{case_id}")
            };
            let turn = if recorder.ab.is_some() {
                opaque_id(&json!(["turn", case_id, id]))
            } else {
                id.to_owned()
            };
            let mut args = json!({"text":message["text"],"session_id":session,"turn_id":turn,
                "history":history,"capture_source":true,"observed_at":message["observed_at"].as_i64().unwrap_or(CLOCK+60*index as i64)});
            if let Some(task) = message.get("task_id") {
                args["task_id"] = task.clone();
            }
            let received = service.call("memory_remember", args, || false).unwrap();
            let source = received
                .pointer("/source/source_id")
                .and_then(Value::as_str)
                .expect("T1 source ID");
            source_ids.insert(id.into(), source.into());
            recorder.begin(case_id, &format!("write:{id}"), &source_ids);
            let (result, model_diagnostics) = diagnostics::capture_model_events(|| {
                service.enrich_source(source).map_err(|e| e.to_string())
            });
            blocked = recorder.missing(case_id);
            if let Some(arm) = &recorder.ab {
                arm.checkpoint(recorder, case_id, &format!("write:{id}"), &db, &source_ids)
                    .unwrap();
            }
            steps.push(json!({"step":"message","message":id,"received":received,"result":result,"model_diagnostics":model_diagnostics,"missing":blocked}));
        }
        for question in case["questions"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|q| q["after"].as_u64() == Some(index as u64 + 1))
        {
            let id = question["id"].as_str().unwrap();
            let at = question["at"]
                .as_i64()
                .unwrap_or(CLOCK + 60 * index as i64 + 30);
            let request_id = if recorder.ab.is_some() {
                opaque_id(&json!(["recall", case_id, id]))
            } else {
                format!("eval-{case_id}-{id}")
            };
            let mut args = json!({"query":question["query"],"context":question["context"],"at":at,
                "request_id":request_id});
            if let Some(task) = question.get("task_id") {
                args["task_id"] = task.clone();
            }
            if let Some(reason) = &blocked {
                steps.push(json!({"step":"question","question":id,"state":"not_recorded","reason":reason,"at":at}));
                continue;
            }
            recorder.begin(case_id, &format!("recall:{id}"), &source_ids);
            if let Some(arm) = &recorder.ab {
                arm.checkpoint(recorder, case_id, &format!("before:{id}"), &db, &source_ids)
                    .unwrap();
            }
            let (result, model_diagnostics) = diagnostics::capture_model_events(|| {
                service
                    .call("memory_recall", args, || false)
                    .unwrap_or_else(|e| panic!("{case_id}/{id}: {e}"))
            });
            if recorder.ab.is_some() {
                assert_eq!(
                    result.pointer("/retrieval/status"),
                    Some(&json!("disabled")),
                    "AF255 requires retrieval disabled"
                );
            }
            let missing = recorder.missing(case_id);
            let state = db.open().read().unwrap();
            let source_state: BTreeMap<_, _> = source_ids
                .iter()
                .map(|(stable, runtime)| {
                    let live = state
                        .sources
                        .values()
                        .any(|s| &s.id == runtime && s.request.is_some());
                    (stable, json!({"linkable":live,"deleted":!live}))
                })
                .collect();
            steps.push(json!({"step":"question","question":id,"at":at,
                "state":if missing.is_some() {"not_recorded"} else {"completed"},"reason":missing,
                "result":result,"model_diagnostics":model_diagnostics,"source_ids":source_ids,"source_state":source_state}));
        }
    }
    json!({"case":case_id,"steps":steps})
}

pub(super) fn try_write_json(path: &Path, value: &Value) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    let temporary = path.with_extension("json.tmp");
    let mut file = std::fs::File::create(&temporary).map_err(|e| e.to_string())?;
    serde_json::to_writer_pretty(&mut file, value).map_err(|e| e.to_string())?;
    file.write_all(b"\n").map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    std::fs::rename(&temporary, path).map_err(|e| format!("record {}: {e}", path.display()))?;
    std::fs::File::open(parent)
        .and_then(|dir| dir.sync_all())
        .map_err(|e| e.to_string())
}

pub(super) fn write_json(path: &Path, value: &Value) {
    try_write_json(path, value).unwrap();
}
