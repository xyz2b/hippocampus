//! Read-only conversion of runtime JSONL records into the Inspector TraceBundle.
use super::{digest, fingerprint, python_json, redact, utc};
use serde_json::{Map, Value, json};
use std::{collections::HashSet, path::Path};

const LANES: [&str; 5] = [
    "mandatory",
    "constraints",
    "preferences",
    "facts",
    "episodes",
];
type ExportResult<T> = Result<T, String>;

thread_local! {
    static FIELD_PATHS: std::cell::RefCell<std::collections::HashMap<usize, String>> = Default::default();
}
fn index_paths(root: &Value) {
    fn visit(v: &Value, path: String, paths: &mut std::collections::HashMap<usize, String>) {
        paths.insert(v as *const Value as usize, path.clone());
        match v {
            Value::Object(map) => {
                for (key, value) in map {
                    let dynamic = [
                        "sources",
                        "receipts",
                        "memories",
                        "record_updates",
                        "relations",
                        "entities",
                    ]
                    .iter()
                    .any(|name| path.ends_with(&format!(".{name}")));
                    let key = if dynamic
                        || key.len() > 48
                        || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    {
                        "*"
                    } else {
                        key
                    };
                    visit(value, format!("{path}.{key}"), paths);
                }
            }
            Value::Array(items) => {
                for (i, value) in items.iter().enumerate() {
                    visit(value, format!("{path}[{i}]"), paths);
                }
            }
            _ => {}
        }
    }
    FIELD_PATHS.with(|paths| {
        let mut paths = paths.borrow_mut();
        paths.clear();
        visit(root, "$".into(), &mut paths);
    });
}
fn field_path(value: &Value) -> String {
    FIELD_PATHS.with(|p| {
        p.borrow()
            .get(&(value as *const Value as usize))
            .cloned()
            .unwrap_or("$".into())
    })
}
fn required<'a>(value: &'a Value, key: &str) -> ExportResult<&'a Value> {
    value
        .as_object()
        .ok_or_else(|| format!("{}: expected object", field_path(value)))?
        .get(key)
        .ok_or_else(|| format!("{}.{}: missing", field_path(value), key))
}
fn string(value: &Value) -> ExportResult<&str> {
    value
        .as_str()
        .ok_or_else(|| format!("{}: expected string", field_path(value)))
}
fn array(value: &Value) -> ExportResult<&Vec<Value>> {
    value
        .as_array()
        .ok_or_else(|| format!("{}: expected array", field_path(value)))
}
fn object(value: &Value) -> ExportResult<&Map<String, Value>> {
    value
        .as_object()
        .ok_or_else(|| format!("{}: expected object", field_path(value)))
}
fn event_error(row: &Value, error: String) -> Value {
    json!({"event_id":row["event_id"].as_str(),"kind":row["kind"].as_str(),"field":error,
        "status":"event_isolated"})
}
fn require(condition: bool, field: &str) -> ExportResult<()> {
    if condition {
        Ok(())
    } else {
        Err(format!("{field}: invariant mismatch"))
    }
}
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64() != Some(0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}
fn business(value: Option<&Value>) -> ExportResult<Value> {
    let mut result = Map::new();
    if let Some(value) = value.filter(|value| truthy(value)) {
        for (key, value) in object(value)? {
            if key != "retrieval_count" && key != "last_retrieved_at" {
                result.insert(key.clone(), value.clone());
            }
        }
    }
    Ok(Value::Object(result))
}

#[derive(Clone)]
struct Captured {
    owner: Value,
    sources: Map<String, Value>,
    memories: Map<String, Value>,
}
impl Captured {
    fn persisted_sources(&mut self, state: &Value) -> ExportResult<bool> {
        let mut enabled = false;
        if let Some(records) = state.get("sources") {
            let records = object(records)?;
            enabled = !records.is_empty();
            for record in records.values() {
                let request = required(record, "request")?;
                if request.is_null() {
                    continue;
                }
                let id = string(required(record, "id")?)?;
                self.sources.insert(id.into(), json!({"id":id,"kind":"persisted_source","text":required(request,"text")?,"session_id":record.get("session_id"),"message_id":record.get("turn_id"),"verification":"诊断快照中的独立原文留存；不代表当前数据库状态或网页来源认证"}));
            }
        }
        Ok(enabled)
    }
    fn memory(&mut self, memory: &Value) -> ExportResult<String> {
        require(
            required(memory, "owner_id")? == &self.owner,
            &format!("{}.owner_id", field_path(memory)),
        )?;
        let version = fingerprint(memory)[..16].to_owned();
        let id = required(memory, "id")?;
        let id_text = id
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| python_json(id, false));
        let reference = format!("{id_text}@{version}");
        let mut source_ids = Vec::new();
        for evidence in array(required(memory, "evidence")?)? {
            let source_id = format!("evidence:{}", &fingerprint(evidence)[..16]);
            self.sources.insert(
                source_id.clone(),
                json!({
                    "id":source_id, "kind":"persisted_evidence",
                    "text":required(evidence, "quote")?, "evidence":evidence
                }),
            );
            source_ids.push(source_id);
        }
        let temporal = required(memory, "temporal")?;
        object(temporal)?;
        let valid_to = temporal.get("valid_to").unwrap_or(&Value::Null);
        let mut captured = json!({
            "id":id_text, "version":version,
            "content":format!("{} · {}", string(required(memory,"predicate")?)?, python_json(required(memory,"value")?, false)),
            "status":string(required(memory,"status")?)?.to_lowercase(),
            "source_ids":source_ids, "owner":self.owner,
            "subject":required(memory,"subject_id")?, "memory_type":required(memory,"memory_type")?,
            "time_horizon":required(memory,"retention_tier")?, "scope":required(memory,"scope")?, "raw":memory,
            "version_source":"record_content_sha256; not native revision",
            "valid_to_unbounded":!valid_to.is_null() && utc(valid_to)?.is_null(),
            "supersedes_id":memory.get("supersedes"), "superseded_by_id":memory.get("superseded_by")
        });
        for key in ["observed_at", "valid_from", "valid_to", "expires_at"] {
            let value = temporal.get(key).unwrap_or(&Value::Null);
            captured[key] = if value.is_null() {
                Value::Null
            } else {
                utc(value)?
            };
        }
        self.memories.insert(reference.clone(), captured);
        Ok(reference)
    }
    fn result_refs(&mut self, result: &Value) -> ExportResult<Vec<String>> {
        object(result)?;
        let mut references = Vec::new();
        for lane in LANES {
            if let Some(memories) = result.get(lane) {
                for memory in array(memories)? {
                    references.push(self.memory(memory)?);
                }
            }
        }
        Ok(references)
    }
    fn response_refs(&mut self, value: &Value) -> ExportResult<Vec<String>> {
        let Some(value) = value.as_object() else {
            return Ok(Vec::new());
        };
        let mut references = Vec::new();
        if value.contains_key("mandatory")
            && value.contains_key("constraints")
            && value.contains_key("episodes")
        {
            references = self.result_refs(&Value::Object(value.clone()))?;
        }
        for child in value.values().filter(|value| value.is_object()) {
            references.extend(self.response_refs(child)?);
        }
        let mut seen = HashSet::new();
        references.retain(|reference| seen.insert(reference.clone()));
        Ok(references)
    }
}

fn collect_source_ids(value: &Value, ids: &mut Vec<Value>) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                if key == "source_snapshot" && value.is_array() {
                    // Runtime source_binding is an explicit tuple with source_id at index zero.
                    if let Some(id) = value.get(0).filter(|v| v.is_string()) {
                        if !ids.contains(id) {
                            ids.push(id.clone());
                        }
                    }
                } else if key == "source_id" && value.is_string() && !ids.contains(value) {
                    ids.push(value.clone());
                } else if key == "input" && value.is_string() {
                    if let Ok(input) = serde_json::from_str::<Value>(value.as_str().unwrap()) {
                        collect_source_ids(&input, ids);
                    }
                } else {
                    collect_source_ids(value, ids);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_source_ids(value, ids);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
pub(super) fn export_runtime(path: &Path, allow_partial: bool) -> ExportResult<Value> {
    export_with_policy(path, allow_partial, None)
}
pub(super) fn export_filtered(
    path: &Path,
    policy: &super::privacy::Policy<'_>,
) -> ExportResult<Value> {
    export_with_policy(path, true, Some(policy))
}
fn export_with_policy(
    path: &Path,
    allow_partial: bool,
    policy: Option<&super::privacy::Policy<'_>>,
) -> ExportResult<Value> {
    let raw = std::fs::read(path).map_err(|error| {
        match error.kind() {
            std::io::ErrorKind::NotFound => "FileNotFoundError",
            std::io::ErrorKind::PermissionDenied => "PermissionError",
            _ => "OSError",
        }
        .to_owned()
    })?;
    let complete = if !allow_partial || raw.ends_with(b"\n") {
        &raw[..]
    } else {
        &raw[..raw.iter().rposition(|byte| *byte == b'\n').unwrap_or(0)]
    };
    let origin_rows: Vec<Value> = complete
        .split(|b| *b == b'\n')
        .filter_map(|b| serde_json::from_slice(b).ok())
        .collect();
    let origins = super::privacy::Origins::collect(&origin_rows);
    drop(origin_rows);
    // A mixed-owner file cannot lend unbound model events to the requested owner.
    let foreign_owner = policy.is_some_and(|p| {
        complete.split(|b| *b == b'\n').any(|b| {
            serde_json::from_slice::<Value>(b)
                .ok()
                .is_some_and(|r| p.foreign_owner(&r))
        })
    });
    let mut filtered_events = Vec::new();
    let mut rows: Vec<Value> = Vec::new();
    let mut parse_errors = Vec::new();
    let mut event_ids = HashSet::new();
    let mut owners = Vec::new();
    for (line, bytes) in complete.split(|b| *b == b'\n').enumerate() {
        if bytes.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let mut row: Value = match serde_json::from_slice(bytes) {
            Ok(row) => row,
            Err(_) => {
                parse_errors.push(json!({"event_id":null,"kind":null,"line":line+1,"field":"$: invalid JSON","status":"event_isolated"}));
                continue;
            }
        };
        if let Some(policy) = policy {
            if let Some(reason) = if foreign_owner {
                Some("owner_mismatch")
            } else {
                policy.reason_with_origin(&row, origins.get(&row))
            } {
                filtered_events.push(super::privacy::metadata(&row, reason));
                continue;
            }
            if let Some(summary) =
                super::privacy::summary(row["kind"].as_str().unwrap_or(""), &row["body"])
            {
                row["body"] = summary;
            }
        }
        if row["trace_version"] == "2" {
            if !row["meta"]["trace_id"].is_string()
                || !row["content_deps"].is_object()
                || !row["body"].is_object()
            {
                parse_errors.push(event_error(
                    &row,
                    "$.meta.trace_id / $.content_deps / $.body: invalid v2 envelope".into(),
                ));
                continue;
            }
            row["run_id"] = row["meta"]["trace_id"].clone();
            row["data"] = row["body"].clone();
        }
        index_paths(&row);
        // Collect owner evidence even from malformed events; never discard conflicting ownership.
        if let Some(owner) = row["data"].get("owner").filter(|v| v.is_string()) {
            if !owners.contains(owner) {
                owners.push(owner.clone());
            }
        }
        let checked: ExportResult<()> = (|| {
            if !matches!(required(&row, "trace_version")?.as_str(), Some("1" | "2")) {
                return Err("$.trace_version: unsupported".into());
            }
            for key in ["event_id", "kind", "run_id"] {
                string(required(&row, key)?)?;
            }
            if !event_ids.insert(row["event_id"].as_str().unwrap().to_owned()) {
                return Err("$.event_id: duplicate".into());
            }
            Ok(())
        })();
        match checked {
            Ok(()) => rows.push(row),
            Err(error) => parse_errors.push(event_error(&row, error)),
        }
    }
    require(
        owners.len() <= 1,
        "$.data.owner (privacy: conflicting owners)",
    )?;
    let owner = owners.pop().unwrap_or(Value::Null);
    let mut bundle = json!({
        "schema_version":"0.1.0", "meta":{
            "title":"S1 运行时诊断记录", "data_kind":"recorded", "trace_sha256":digest(&raw),
            "deletion_epoch":policy.map(|p|p.summary()),"filtered_events":filtered_events,"tail_status":if complete == raw {"complete"}else{"partial_line"}, "parse_errors":parse_errors,
            "channels":{"source_capture":"not_enabled", "lexical_index":"not_enabled"},
            "notes":"实际运行记录；是否使用本地合成模型桩由验收 manifest 标注。费用未知；读取不执行模型。"
        },
        "memory_snapshot":{"completeness":"not_captured", "scope":owner, "revision":null},
        "sources":[], "memories":[], "runs":[], "llm_calls":[]
    });
    let mut captured = Captured {
        owner,
        sources: Map::new(),
        memories: Map::new(),
    };
    let mut groups: Vec<(String, Vec<(usize, &Value)>)> = Vec::new();
    for (position, row) in rows.iter().enumerate() {
        let run_id = string(required(row, "run_id")?)?;
        if let Some((_, events)) = groups.iter_mut().find(|(id, _)| id == run_id) {
            events.push((position, row));
        } else {
            groups.push((run_id.to_owned(), vec![(position, row)]));
        }
    }
    let mut runs = Vec::new();
    let mut all_calls = Vec::new();
    let mut latest_snapshot_position = None;
    for (run_id, events) in groups {
        let starts: Vec<_> = events
            .iter()
            .filter(|(_, row)| row["kind"] == "run_start")
            .collect();
        let orphan = starts.is_empty();
        let fallback = json!({"at":events[0].1["at"],"data":{"arguments":{},"tool":"background","entrypoint":"internal_cli"}});
        let start = starts.first().map(|entry| entry.1).unwrap_or(&fallback);
        let empty_args = json!({});
        let args = start["data"]
            .get("arguments")
            .filter(|v| v.is_object())
            .unwrap_or(&empty_args);
        let tool = start["data"]["tool"].as_str().unwrap_or("unknown");
        let internal = start["data"]["entrypoint"] == "internal_cli"
            || ["lexical_search", "source_index_rebuild"].contains(&tool);
        let source_enrichment = internal && tool == "source_enrichment";
        object(args)?;
        let user_turn = args
            .get("user_turn")
            .filter(|v| v.is_object())
            .unwrap_or(args);
        let empty_text = json!("");
        let text = user_turn
            .get("text")
            .unwrap_or_else(|| args.get("query").unwrap_or(&empty_text));
        let text = if internal && !text.is_string() {
            &empty_text
        } else {
            text
        };
        let source_id = format!("{run_id}:source");
        captured.sources.insert(
            source_id.clone(),
            json!({"id":source_id,"kind":"mcp_arguments","text":text,
            "verification":"服务端实际工具参数；不是网页原话认证，也不是独立 source 留存"}),
        );
        if internal {
            captured.sources[&source_id]["kind"] = json!("internal_arguments");
            captured.sources[&source_id]["verification"] = json!(if source_enrichment {
                "内部原文整理参数；不是 MCP 接收原文或保存记录"
            } else {
                "内部索引操作参数；不是 MCP 接收原文或保存记录"
            });
        }
        let mut run = json!({
            "id":run_id, "operation_id":run_id, "tool":tool, "entrypoint":if internal {"internal_cli"} else {"mcp"}, "status":"incomplete", "evidence_status":"evidence_incomplete",
            "started_at":start.get("at"), "source_ids":[source_id], "session_id":user_turn.get("session_id"),
            "input":{"mcp_text":text,"arguments":args}, "save_status":"unknown", "recall_status":"unknown",
            "read_revision":null, "committed_revision":null, "spans":[], "write_changes":[],
            "retrieval":{"query":null,"query_status":"unknown","scanned_count":null,"candidates":null,"selected_refs":null},
            "mcp_response":{"recorded":false,"returned_refs":null,"body":null,"delivery_status":"not_captured"},
            "host":{"input_observability":"unavailable","request":null},
            "notes":"memory version 是该观测记录内容指纹；事务修订号仅按实际采集值展示。轮询共用 operation_id；RPC 响应不是模型尝试。"
        });
        if source_enrichment {
            run["input"]["mcp_text"] = Value::Null;
        }
        run["missing_run_start"] = json!(orphan);
        run["linked_source_ids"] = json!([]);
        let mut calls: Vec<Value> = Vec::new();
        let mut spans: Vec<Value> = Vec::new();
        let mut model_spans: Vec<(String, usize)> = Vec::new();
        let mut latest_call: Option<usize> = None;
        let mut current_state: Option<&Value> = None;
        let mut state_event: Option<(usize, &Value)> = None;
        let mut retrieval: Option<&Value> = None;
        let mut ended = false;
        let mut write_changes = Vec::new();
        run["parse_errors"] = json!([]);
        for (position, row) in events {
            index_paths(row);
            let prior_state = current_state;
            let prior_event = state_event;
            let span_count = spans.len();
            let prior_run = run.clone();
            let prior_channels = bundle["meta"]["channels"].clone();
            let prior_captured = matches!(
                row["kind"].as_str(),
                Some("validation" | "transaction_committed" | "retrieval" | "run_end")
            )
            .then(|| captured.clone());
            let prior_call = calls
                .iter()
                .position(|c| c["id"] == row["data"]["call_id"])
                .map(|i| (i, calls[i].clone()));
            let call_count = calls.len();
            let model_span_count = model_spans.len();
            let prior_latest = latest_call;
            let prior_retrieval = retrieval;
            let prior_ended = ended;
            let write_count = write_changes.len();
            let processed: ExportResult<()> = (|| {
                let kind = string(required(row, "kind")?)?;
                let data = required(row, "data")?;
                object(data)?;
                collect_source_ids(data, run["linked_source_ids"].as_array_mut().unwrap());
                collect_source_ids(
                    &row["meta"],
                    run["linked_source_ids"].as_array_mut().unwrap(),
                );
                let mut span = json!({"id":required(row,"event_id")?,"name":kind,"status":"completed","recorded_at":required(row,"at")?,"output":data,"llm_call_ids":[]});
                if row["trace_version"] == "2" {
                    span["content_deps"] = row["content_deps"].clone();
                    span["trace_meta"] = row["meta"].clone();
                    run["trace_context"] = row["meta"].clone();
                    for key in ["generation", "round", "attempt_id"] {
                        if !row["meta"][key].is_null() {
                            run[key] = row["meta"][key].clone();
                        }
                    }
                }

                if let Some(duration) = data.get("duration_ms") {
                    span["duration_ms"] = duration.clone();
                }
                match kind {
                    "source_receipt" => {
                        for key in ["source", "index", "enrichment"] {
                            if let Some(value) = data.get(key) {
                                run[key] = value.clone();
                            }
                        }
                        if matches!(
                            data["source"]["status"].as_str(),
                            Some("stored" | "deleted")
                        ) {
                            bundle["meta"]["channels"]["source_capture"] = json!("enabled");
                        }
                        if data["index"]["status"] == "ready" {
                            bundle["meta"]["channels"]["lexical_index"] = json!("enabled");
                        }
                        span["status"] = json!(if matches!(
                            data["source"]["status"].as_str(),
                            Some("failed" | "rejected")
                        ) {
                            "failed"
                        } else {
                            "completed"
                        });
                    }
                    "model_start" => {
                        let call_id = string(required(data, "call_id")?)?;
                        require(
                            !calls.iter().any(|call| call["id"] == call_id),
                            "$.data.call_id (duplicate start)",
                        )?;
                        let event_stage = string(required(data, "stage")?)?;
                        let request = required(data, "request")?;
                        let stage = request
                            .pointer("/text/format/name")
                            .and_then(Value::as_str)
                            .unwrap_or(event_stage);
                        let attempt = required(data, "attempt")?;
                        let call = json!({
                            "id":call_id,"run_id":run_id,"operation_id":run_id,"stage":stage,"status":"unknown",
                            "attempt":attempt,"started_at":required(row,"at")?,"provider":data.get("provider"),"model":request.get("model"),
                            "effort":request.pointer("/reasoning/effort"),"schema_name":request.pointer("/text/format/name"),
                            "checks":[],"verified_final":null,
                            "request":request,"response":null,"parsed_result":null,"usage":{},
                            "cost":{"amount":null,"currency":null,"source":"unknown"},
                            "prompt_version":format!("request-sha256:{}",fingerprint(request))
                        });
                        latest_call = Some(calls.len());
                        calls.push(call);
                        span["status"] = json!("in_progress");
                        span["input"] = request.clone();
                        span["output"] = Value::Null;
                        span["llm_call_ids"] = json!([call_id]);
                        let name = match stage {
                            "personal_memory_candidates_v1" => "模型抽取候选",
                            "personal_memory_candidates_verification_v1" => "抽取复核",
                            "personal_memory_runtime_context_v1" => "模型规划查询",
                            "personal_memory_runtime_context_verification_v1" => "查询复核",
                            other => other,
                        };
                        span["name"] = json!(format!(
                            "{name} · 第 {} 次发送",
                            attempt
                                .as_str()
                                .map(str::to_owned)
                                .unwrap_or_else(|| python_json(attempt, false))
                        ));
                        model_spans.push((call_id.to_owned(), spans.len()));
                    }
                    "model_end" => {
                        let call_id = string(required(data, "call_id")?)?;
                        let call_index = if let Some(index) =
                            calls.iter().position(|call| call["id"] == call_id)
                        {
                            index
                        } else {
                            calls.push(
                                json!({"id":call_id,"run_id":run_id,"stage":data.get("stage"),
                            "status":"unknown","missing_model_start":true,"usage":{},"checks":[],
                            "verified_final":null,"cost":{"source":"unknown"}}),
                            );
                            model_spans.push((call_id.to_owned(), spans.len()));
                            spans.push(span.clone());
                            calls.len() - 1
                        };
                        require(
                            calls[call_index]["status"] == "unknown",
                            "$.data.call_id (duplicate end)",
                        )?;
                        let empty = json!({});
                        let envelope = data.get("response").filter(|v| truthy(v)).unwrap_or(&empty);
                        object(envelope)?;
                        let usage = envelope
                            .get("usage")
                            .filter(|v| truthy(v))
                            .unwrap_or(&empty);
                        object(usage)?;
                        let details = envelope
                            .get("incomplete_details")
                            .filter(|v| truthy(v))
                            .unwrap_or(&empty);
                        object(details)?;
                        let output_details = usage
                            .get("output_tokens_details")
                            .filter(|v| truthy(v))
                            .unwrap_or(&empty);
                        let input_details = usage
                            .get("input_tokens_details")
                            .filter(|v| truthy(v))
                            .unwrap_or(&empty);
                        object(output_details)?;
                        object(input_details)?;
                        let call = &mut calls[call_index];
                        call["status"] = data
                            .get("status")
                            .filter(|v| v.is_string())
                            .cloned()
                            .unwrap_or(json!("unknown"));
                        call["duration_ms"] = data["duration_ms"].clone();
                        call["missing_fields"] = json!(
                            [
                                "status",
                                "duration_ms",
                                "response",
                                "accounting",
                                "parsed_result"
                            ]
                            .into_iter()
                            .filter(|key| data.get(*key).is_none_or(Value::is_null))
                            .collect::<Vec<_>>()
                        );
                        call["response"] = envelope.clone();
                        if !envelope["model"].is_null() {
                            call["model"] = envelope["model"].clone();
                        }
                        call["local_outcome"] = data["local_outcome"].clone();
                        call["error"] = data["error"].clone();
                        call["http_status"] = data["http_status"].clone();
                        call["accounting"] = data["accounting"].clone();
                        call["provider_request_id"] = envelope["id"].clone();
                        call["parsed_result"] = data["parsed_result"].clone();
                        call["parsing_status"] = data["parsing_status"].clone();
                        call["finish_reason"] = details
                            .get("reason")
                            .filter(|v| truthy(v))
                            .unwrap_or(&data["error"])
                            .clone();
                        call["usage"] = json!({"input_tokens":usage.get("input_tokens").or_else(||data["accounting"].get("input_tokens")),"output_tokens":usage.get("output_tokens").or_else(||data["accounting"].get("output_tokens")),
                        "reasoning_tokens":output_details.get("reasoning_tokens"),"cached_input_tokens":input_details.get("cached_tokens"),
                        "source":if truthy(usage){"recorded_response"}else if data["accounting"]["input_tokens"].is_number(){"recorded_accounting"}else{"unknown"}});
                        let span_index = model_spans
                            .iter()
                            .find(|(id, _)| id == call_id)
                            .ok_or("$.data.call_id: missing model span")?
                            .1;
                        let model_span = &mut spans[span_index];
                        model_span["status"] =
                            data.get("status").cloned().unwrap_or(json!("unknown"));
                        model_span["output"] = data.clone();
                        model_span["duration_ms"] = data["duration_ms"].clone();
                        if truthy(&data["error"]) {
                            model_span["error"] = json!({"reason":data["error"]});
                        }
                        return Ok(());
                    }
                    "model_check" | "model_transport_error" => {
                        span["status"] = data.get("status").cloned().unwrap_or(json!("failed"));
                        let matches: Vec<_> = calls
                            .iter()
                            .enumerate()
                            .filter(|(_, call)| {
                                if let Some(id) = data.get("call_id") {
                                    return call["id"] == *id;
                                }
                                let stage = call["stage"]
                                    .as_str()
                                    .unwrap_or("")
                                    .split('_')
                                    .next()
                                    .unwrap_or("");
                                stage.eq_ignore_ascii_case(
                                    data["stage"].as_str().unwrap_or("unknown"),
                                )
                            })
                            .map(|(i, _)| i)
                            .collect();
                        // No time-based association: repeated stage calls without call IDs stay unbound.
                        if matches.len() == 1 {
                            let call = &mut calls[matches[0]];
                            span["llm_call_ids"] = json!([call["id"]]);
                            if kind == "model_check" {
                                call["checks"].as_array_mut().unwrap().push(data.clone());
                                if let Some(final_output) =
                                    data.get("final").filter(|_| data["status"] == "completed")
                                {
                                    call["verified_final"] = final_output.clone();
                                }
                            } else {
                                call["transport_error"] = data.clone();
                            }
                        }
                    }
                    "validation" => {
                        let stage = string(required(data, "stage")?)?;
                        span["name"] = json!(format!("{stage} / Rust 校验"));
                        span["status"] = required(data, "status")?.clone();
                        if stage == "initial_snapshot" && data["status"] == "completed" {
                            current_state = Some(required(data, "parsed_result")?);
                            state_event = Some((position, row));
                        } else if let Some(index) = latest_call {
                            span["llm_call_ids"] = json!([calls[index]["id"]]);
                            calls[index]["domain_validation"] = data.clone();
                        }
                    }
                    "transaction_committed" => {
                        if let Some(owner) = data.get("owner") {
                            require(owner == &captured.owner, "$.data.owner")?;
                        } else if row["trace_version"] != "2" {
                            required(data, "owner")?;
                        }
                        for key in ["read_revision", "committed_revision"] {
                            if let Some(revision) = data.get(key).filter(|value| !value.is_null()) {
                                run[key] = json!(
                                    revision
                                        .as_str()
                                        .map(str::to_owned)
                                        .unwrap_or_else(|| revision.to_string())
                                );
                            }
                        }
                        if data["after"].is_null() {
                            require(
                                data["committed_revision"].is_u64()
                                    && data["revision_observability"] == "persisted",
                                "$.data.committed_revision/revision_observability",
                            )?;
                            span["recognition"] = json!("transaction_summary");
                            spans.push(span);
                            return Ok(());
                        }
                        let before = required(data, "before")?;
                        let after = required(data, "after")?;
                        current_state = Some(after);
                        state_event = Some((position, row));
                        let empty = json!({});
                        let before_memories = if truthy(before) {
                            before
                                .get("repository")
                                .unwrap_or(&empty)
                                .get("memories")
                                .unwrap_or(&empty)
                        } else {
                            &empty
                        };
                        for memory in object(before_memories)?.values() {
                            captured.memory(memory)?;
                        }
                        let after_memories = required(required(after, "repository")?, "memories")?;
                        for (key, memory) in object(after_memories)? {
                            let old = if truthy(before) {
                                required(required(before, "repository")?, "memories")?.get(key)
                            } else {
                                None
                            };
                            if business(old)? != business(Some(memory))? {
                                let reference = captured.memory(memory)?;
                                let before_receipts = if truthy(before) {
                                    before.get("receipts").unwrap_or(&empty)
                                } else {
                                    &empty
                                };
                                object(before_receipts)?;
                                let mut receipts = Vec::new();
                                for (receipt_id, receipt) in object(required(after, "receipts")?)? {
                                    if before_receipts.get(receipt_id).is_some() {
                                        continue;
                                    }
                                    let Some(result) =
                                        receipt.get("result").filter(|v| v.is_object())
                                    else {
                                        continue;
                                    };
                                    if let Some(outcomes) =
                                        result.get("outcomes").and_then(Value::as_array)
                                    {
                                        for outcome in outcomes {
                                            object(outcome)?;
                                            if let Some(ingested) =
                                                outcome.get("Ingested").filter(|v| v.is_object())
                                            {
                                                let applied =
                                                    ingested.get("Applied").unwrap_or(&empty);
                                                object(applied)?;
                                                if applied.get("memory_id")
                                                    == Some(required(memory, "id")?)
                                                {
                                                    receipts.push(receipt_id.clone());
                                                    break;
                                                }
                                            }
                                        }
                                    }
                                }
                                let extraction = calls.iter().rev().find(|call| {
                                    call["stage"]
                                        .as_str()
                                        .is_some_and(|s| s.contains("candidates"))
                                        && call["domain_validation"]["status"] == "completed"
                                });
                                let candidates = extraction.and_then(|call| {
                                    call["domain_validation"].get("parsed_result")
                                });
                                let mut candidate = None;
                                if let Some(candidates) = candidates {
                                    for value in array(candidates)? {
                                        if required(value, "value")? != required(memory, "value")? {
                                            continue;
                                        }
                                        let evidence = array(required(value, "evidence")?)?;
                                        let memory_evidence = array(required(memory, "evidence")?)?;
                                        let candidate_ids: Vec<_> = evidence
                                            .iter()
                                            .map(|e| required(e, "id"))
                                            .collect::<ExportResult<_>>()?;
                                        let memory_ids: Vec<_> = memory_evidence
                                            .iter()
                                            .map(|e| required(e, "id"))
                                            .collect::<ExportResult<_>>()?;
                                        if candidate_ids.iter().any(|id| memory_ids.contains(id)) {
                                            candidate = Some(value);
                                            break;
                                        }
                                    }
                                }
                                write_changes.push(json!({"candidate_id":key,"candidate":candidate,"before":old,"after":memory,
                                "commit_status":"committed","memory_ref":reference,"source_ids":captured.memories[&reference]["source_ids"],
                                "receipt_id":if receipts.len()==1 {Some(&receipts[0])}else{None},"validation":{"basis":"post_commit_snapshot_diff"},
                                "llm_call_ids":if candidate.is_some() {vec![extraction.map(|c| c["id"].clone()).unwrap_or(Value::Null)]}else{vec![]}}));
                            }
                        }
                    }
                    "transaction_rollback" => {
                        span["status"] = json!("failed");
                    }
                    "candidate_decision" => {
                        span["name"] = json!("候选规则裁决（提交结果见事务）");
                        span["input"] = required(data, "candidate")?.clone();
                        span["status"] = json!(if truthy(&data["error"]) {
                            "failed"
                        } else {
                            "completed"
                        });
                    }
                    "lexical_raw" => {
                        require(data["channel"] == "lexical_raw", "$.data.channel")?;
                        let status = string(required(data, "status")?)?;
                        span["status"] = json!(match status {
                            "completed" | "incomplete" | "failed" => status,
                            "disabled" => "not_run",
                            _ => "unknown",
                        });
                        if status == "completed"
                            || data.get("generation").is_some_and(|v| !v.is_null())
                        {
                            bundle["meta"]["channels"]["lexical_index"] = json!("enabled");
                        }
                        run["lexical_raw"] = data.clone();
                    }
                    "local_recall" => {
                        run["local_recall"] = data.clone();
                        run["retrieval"]["query_model_output"] = Value::Null;
                        run["retrieval"]["conditions_provenance"] =
                            data["conditions_provenance"].clone();
                        run["read_revision"] = json!(data["read_revision"].to_string());
                    }
                    "retrieval" => {
                        if data.get("result").is_none() || data.get("runtime").is_none() {
                            run["retrieval_diagnostics"] = data.clone();
                            spans.push(span);
                            return Ok(());
                        }
                        retrieval = Some(data);
                        let result = required(data, "result")?;
                        let references = captured.result_refs(result)?;
                        run["retrieval"]["query"] = required(data, "runtime")?.clone();
                        run["retrieval"]["query_status"] = json!("completed");
                        run["retrieval"]["scanned_count"] =
                            json!(array(required(result, "trace")?)?.len());
                        run["retrieval"]["selected_refs"] = json!(references);
                        run["retrieval"]["query_model_output"] = latest_call
                            .map(|index| calls[index]["parsed_result"].clone())
                            .unwrap_or(Value::Null);
                        run["retrieval"]["conditions_provenance"] = json!(
                            "pre-retriever runtime; owner/at/limit injected locally; model conditions retained as legacy filters; internal temporal resolution not separately captured"
                        );
                        let result = required(data, "result")?;
                        let mut candidates = Vec::new();
                        for entry in array(required(result, "trace")?)? {
                            let decision = required(entry, "decision")?;
                            let memory_id = required(entry, "memory_id")?;
                            let selected =
                                matches!(decision.as_str(), Some("Selected" | "Mandatory"));
                            let mut memory = None;
                            for lane in LANES {
                                for value in result
                                    .get(lane)
                                    .map(array)
                                    .transpose()?
                                    .into_iter()
                                    .flatten()
                                {
                                    if required(value, "id")? == memory_id {
                                        memory = Some(value);
                                        break;
                                    }
                                }
                                if memory.is_some() {
                                    break;
                                }
                            }
                            if memory.is_none()
                                && let Some(memories) = data.get("memories").filter(|v| truthy(v))
                            {
                                for value in array(memories)? {
                                    if required(value, "id")? == memory_id {
                                        memory = Some(value);
                                        break;
                                    }
                                }
                            }
                            let memory =
                                memory.ok_or("$.data.result.trace[*].memory_id: unresolved")?;
                            candidates.push(json!({"memory_ref":captured.memory(memory)?,"outcome":if selected{"selected"}else{"filtered"},
                    "reason_codes":[decision.as_str().map(str::to_owned).unwrap_or_else(||python_json(decision,false))],"rank":null,"score":null}));
                        }
                        run["retrieval"]["candidates"] = json!(candidates);
                    }
                    "run_end" => {
                        require(!ended, "$.kind (duplicate run_end)")?;
                        ended = true;
                        if internal {
                            run["status"] = json!(if data["status"] == "failed" {
                                "failed"
                            } else {
                                match run["lexical_raw"]["status"]
                                    .as_str()
                                    .or_else(|| data["status"].as_str())
                                {
                                    Some("completed" | "disabled") => "completed",
                                    Some("incomplete") => "incomplete",
                                    Some("failed") => "failed",
                                    _ => "unknown",
                                }
                            });
                            run["evidence_status"] =
                                json!(if data["evidence_status"] == "complete" {
                                    "complete"
                                } else {
                                    "evidence_incomplete"
                                });
                            run["duration_ms"] = data["duration_ms"].clone();
                            if source_enrichment {
                                for key in ["source", "index", "enrichment"] {
                                    if let Some(value) = data["result"].get(key) {
                                        run[key] = value.clone();
                                    }
                                }
                                run["save_status"] =
                                    run["enrichment"]["status"].as_str().map_or_else(
                                        || {
                                            json!(if data["status"] == "failed" {
                                                "not_committed"
                                            } else {
                                                "unknown"
                                            })
                                        },
                                        |status| json!(status),
                                    );
                                run["notes"] = json!(
                                    "CLI 显式处理原文的记录；可按相同 source_id 查找接收记录。此处不是 MCP 响应，也不证明宿主已使用整理后的记忆。"
                                );
                                span["name"] = json!("internal_enrichment_run_end");
                            } else {
                                run["save_status"] = json!("not_applicable");
                                run["notes"] = json!(
                                    "内部索引操作记录；原文候选不是结构化记忆，不代表正式 MCP 回答路径已接入。索引状态与候选仅代表记录时刻。"
                                );
                                span["name"] = json!("internal_run_end");
                            }
                            run["recall_status"] = json!("not_applicable");
                            run["mcp_response"]["delivery_status"] = json!("not_applicable");
                            run["internal_result"] = data["result"].clone();
                            span["status"] = run["status"].clone();
                            if truthy(&data["error"]) {
                                span["error"] = json!({"reason":data["error"]});
                            }
                            spans.push(span);
                            return Ok(());
                        }
                        let response = required(data, "response")?;
                        object(response)?;
                        let empty = json!({});
                        let body = response.get("structuredContent").unwrap_or(&empty);
                        object(body)?;
                        let receipt = body.get("remember").unwrap_or(body);
                        for key in ["source", "index", "enrichment"] {
                            if let Some(value) = receipt.get(key) {
                                run[key] = value.clone();
                            }
                        }
                        if matches!(run["source"]["status"].as_str(), Some("stored" | "deleted")) {
                            bundle["meta"]["channels"]["source_capture"] = json!("enabled");
                        }
                        if run["index"]["status"] == "ready" {
                            bundle["meta"]["channels"]["lexical_index"] = json!("enabled");
                        }
                        let mut failed = truthy(&response["isError"]);
                        for key in ["remember", "recall"] {
                            if let Some(part) = body.get(key) {
                                object(part)?;
                                failed |= part["status"] == "failed";
                            }
                        }
                        run["status"] = json!(if failed {
                            if calls.iter().any(|call| call["status"] == "incomplete") {
                                "incomplete"
                            } else {
                                "failed"
                            }
                        } else {
                            "completed"
                        });
                        run["evidence_status"] = data
                            .get("evidence_status")
                            .cloned()
                            .unwrap_or(json!("evidence_incomplete"));
                        run["duration_ms"] = data["duration_ms"].clone();
                        run["mcp_response"]["recorded"] = json!(true);
                        run["mcp_response"]["body"] = response.clone();
                        run["mcp_response"]["returned_refs"] = json!(captured.response_refs(body)?);
                        let recalled = body
                            .get("recall")
                            .map(|r| r.get("result").unwrap_or(r))
                            .unwrap_or(body);
                        let historical = recalled["historical_or_conflicting"].as_array();
                        let source_count =
                            recalled["source_evidence"].as_array().map_or(0, Vec::len)
                                + historical.map_or(0, |items| {
                                    items
                                        .iter()
                                        .filter(|item| item.get("source").is_some())
                                        .count()
                                });
                        run["mcp_response"]["returned_source_count"] = json!(source_count);
                        run["mcp_response"]["delivery_status"] =
                            json!("service_response_constructed; host_receipt_unknown");
                        run["save_status"] = json!(if !truthy(&body["error"])
                            && body["remember"]["status"] != "failed"
                        {
                            if write_changes.is_empty() {
                                "no_memory_change"
                            } else {
                                "committed"
                            }
                        } else {
                            "not_committed"
                        });
                        if ![
                            "memory_prepare",
                            "memory_remember",
                            "memory_clarify",
                            "memory_forget",
                        ]
                        .contains(&tool)
                        {
                            run["save_status"] = json!("not_applicable");
                        } else if matches!(
                            run["enrichment"]["status"].as_str(),
                            Some("queued" | "running" | "pending" | "stale_requires_review")
                        ) && run["source"]["status"] == "stored"
                        {
                            run["save_status"] = run["enrichment"]["status"].clone();
                        }
                        if body["recall"]["status"] == "not_requested" {
                            run["recall_status"] = json!("not_requested");
                            run["retrieval"]["query_status"] = json!("not_requested");
                        } else if retrieval.is_some_and(truthy) {
                            run["recall_status"] =
                                json!(if truthy(&run["retrieval"]["selected_refs"])
                                    || source_count > 0
                                    || historical.is_some_and(|items| !items.is_empty())
                                    || truthy(&recalled["result"]["conflicts"])
                                {
                                    "completed"
                                } else {
                                    "completed_empty"
                                });
                        } else if ["memory_prepare", "memory_recall"].contains(&tool) {
                            let incomplete = calls.iter().any(|call| {
                                call["status"] == "incomplete"
                                    && call["stage"]
                                        .as_str()
                                        .is_some_and(|s| s.contains("runtime_context"))
                            });
                            run["recall_status"] = json!(if failed {
                                if incomplete {
                                    "planning_incomplete"
                                } else {
                                    "failed"
                                }
                            } else {
                                "unknown"
                            });
                            run["retrieval"]["query_status"] =
                                json!(if run["recall_status"] == "planning_incomplete" {
                                    "incomplete"
                                } else {
                                    "unknown"
                                });
                        }
                        if recalled
                            .get("version")
                            .and_then(Value::as_str)
                            .is_some_and(|v| v.starts_with("recall."))
                        {
                            run["recall_status"] =
                                recalled.get("status").cloned().unwrap_or(json!("unknown"));
                        }
                        span["status"] = run["status"].clone();
                    }
                    "rpc_response" => {
                        let empty = json!({});
                        let response = data
                            .get("response")
                            .filter(|value| truthy(value))
                            .unwrap_or(&empty);
                        object(response)?;
                        let body = response.get("structuredContent").unwrap_or(&empty);
                        object(body)?;
                        span["status"] = json!(if body["status"] == "in_progress" {
                            "in_progress"
                        } else {
                            "completed"
                        });
                    }
                    "run_start" => {
                        string(required(data, "tool")?)?;
                        object(required(data, "arguments")?)?;
                        if let Some(turn) = data["arguments"].get("user_turn") {
                            object(turn)?;
                        }
                    }
                    "global_context" => {}
                    "w2_summary" | "w3_skipped" | "w4_plan" | "w5_commit" | "r3_evidence"
                    | "r5_final" => {
                        span["recognition"] = json!("summary");
                    }
                    _ => {
                        span["recognition"] = json!("unrecognized");
                    }
                }
                if truthy(&data["error"]) {
                    span["error"] = json!({"reason":data["error"]});
                }
                spans.push(span);
                if let Some(state) = current_state.filter(|state| truthy(state))
                    && matches!(kind, "validation" | "transaction_committed")
                {
                    if captured.persisted_sources(state)? {
                        bundle["meta"]["channels"]["source_capture"] = json!("enabled");
                    }
                    if state["sources"].as_object().is_some_and(|records| {
                        records
                            .values()
                            .any(|record| record["index"]["status"] == "ready")
                    }) {
                        bundle["meta"]["channels"]["lexical_index"] = json!("enabled");
                    }
                    for memory in
                        object(required(required(state, "repository")?, "memories")?)?.values()
                    {
                        captured.memory(memory)?;
                    }
                }
                Ok(())
            })();
            if let Err(error) = processed {
                current_state = prior_state;
                state_event = prior_event;
                run = prior_run;
                bundle["meta"]["channels"] = prior_channels;
                if let Some(prior) = prior_captured {
                    captured = prior;
                }
                calls.truncate(call_count);
                if let Some((i, call)) = prior_call {
                    calls[i] = call;
                }
                model_spans.truncate(model_span_count);
                latest_call = prior_latest;
                retrieval = prior_retrieval;
                ended = prior_ended;
                write_changes.truncate(write_count);
                spans.truncate(span_count);
                let error = event_error(row, error);
                run["parse_errors"]
                    .as_array_mut()
                    .unwrap()
                    .push(error.clone());
                spans.push(json!({"id":row["event_id"],"name":row["kind"],"status":"incomplete",
                    "recorded_at":row.get("at"),"parse_error":error,"output":null,"llm_call_ids":[]}));
            }
        }
        if let (Some(state), Some((position, event))) =
            (current_state.filter(|state| truthy(state)), state_event)
            && latest_snapshot_position.is_none_or(|latest| position > latest)
        {
            latest_snapshot_position = Some(position);
            bundle["memory_snapshot"]["completeness"] = json!("complete");
            bundle["memory_snapshot"]["state"] = state.clone();
            bundle["memory_snapshot"]["captured_at"] = event["at"].clone();
            bundle["memory_snapshot"]["snapshot_sha256"] = json!(fingerprint(state));
            bundle["memory_snapshot"]["identity_note"] =
                json!("last observed snapshot, not necessarily current DB; revision unknown");
            if let Some(revision) = state.get("revision") {
                bundle["memory_snapshot"]["revision"] = json!(
                    revision
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| revision.to_string())
                );
                bundle["memory_snapshot"]["identity_note"] =
                    json!("last observed owner transaction revision, not necessarily current DB");
            }
        }
        if calls.iter().any(|call| call["status"] == "unknown") {
            run["evidence_status"] = json!("evidence_incomplete");
        }
        if let Some(source_id) = run["source"]["source_id"].as_str().map(str::to_owned)
            && run["source"]["status"] == "stored"
            && captured.sources.contains_key(&source_id)
        {
            if let Some(task) = current_state
                .and_then(|state| state["sources"].as_object())
                .and_then(|records| {
                    records.values().find(|record| {
                        record["id"].as_str() == Some(source_id.as_str())
                            && record["enrichment"] == run["enrichment"]["status"]
                    })
                })
                .and_then(|record| record.get("task"))
            {
                run["enrichment_task"] = task.clone();
            }
            run["source_ids"]
                .as_array_mut()
                .unwrap()
                .push(json!(source_id));
        }
        if !run["parse_errors"].as_array().unwrap().is_empty()
            || orphan
            || !ended
            || calls
                .iter()
                .any(|c| c["status"] == "unknown" || c["missing_model_start"] == true)
        {
            run["evidence_status"] = json!("evidence_incomplete");
        }
        run["spans"] = json!(spans);
        if spans.iter().any(|s| {
            s["name"] == "w5_commit"
                && s["status"] == "completed"
                && s["output"]["product_ids"]
                    .as_array()
                    .is_some_and(|ids| !ids.is_empty())
        }) {
            run["save_status"] = json!("committed");
        }
        run["write_changes"] = json!(write_changes);
        all_calls.extend(calls);
        runs.push(run);
    }
    bundle["sources"] = json!(captured.sources.into_values().collect::<Vec<_>>());
    bundle["memories"] = json!(captured.memories.into_values().collect::<Vec<_>>());
    bundle["runs"] = json!(runs);
    bundle["llm_calls"] = json!(all_calls);
    Ok(redact(&bundle))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn export_fixture(name: &str, events: Vec<(&str, &str, Value)>) -> Value {
        let path =
            std::env::temp_dir().join(format!("pmr-af363-{name}-{}.jsonl", std::process::id()));
        let text = events
            .into_iter()
            .enumerate()
            .map(|(i, (run, kind, data))| {
                format!(
                    "{}\n",
                    json!({"trace_version":"1","event_id":i.to_string(),"run_id":run,
                "kind":kind,"at":"2026-10-16T00:00:00Z","data":data})
                )
            })
            .collect::<String>();
        std::fs::write(&path, text).unwrap();
        let result = export_runtime(&path, true);
        std::fs::remove_file(path).unwrap();
        result.unwrap()
    }

    #[test]
    fn af383_pipeline_metadata_and_skip_summaries_survive_export() {
        let path =
            std::env::temp_dir().join(format!("af383-inspector-{}.jsonl", std::process::id()));
        let data = [
            (
                "D",
                None,
                Some("s"),
                None,
                "source_receipt",
                json!({"source":{"source_id":"s","status":"stored"}}),
            ),
            (
                "A",
                Some("D"),
                Some("s"),
                Some("attempt"),
                "w3_skipped",
                json!({"reason":"no_candidates","candidates":0}),
            ),
            (
                "A",
                Some("D"),
                Some("s"),
                Some("attempt"),
                "w4_plan",
                json!({"groups":0,"held":0,"model_called":false,"reason":"no_supported_candidates","decisions":{}}),
            ),
            (
                "B",
                Some("D"),
                Some("s"),
                Some("attempt"),
                "w5_commit",
                json!({"product_ids":[1],"decision_types":["new"]}),
            ),
            (
                "C",
                Some("D"),
                None,
                Some("recall"),
                "r5_final",
                json!({"status":"supported","facts":1}),
            ),
        ];
        let rows=data.iter().enumerate().map(|(i,(trace,parent,source,attempt,kind,body))|
            format!("{}\n",json!({"trace_version":"2","event_id":i.to_string(),"at":"fixture",
                "kind":kind,"body":body,"content_deps":{},
                "meta":{"trace_id":trace,"parent_trace_id":parent,"source_id":source,"attempt_id":attempt}}))).collect::<String>();
        std::fs::write(&path, rows).unwrap();
        let out = export_runtime(&path, true).unwrap();
        assert_eq!(out["meta"]["parse_errors"], json!([]));
        let runs = out["runs"].as_array().unwrap();
        assert_eq!(runs.len(), 4);
        for r in runs {
            for span in r["spans"].as_array().unwrap() {
                assert_eq!(span["status"], "completed");
                if span["trace_meta"]["trace_id"] != "D" {
                    assert_eq!(span["trace_meta"]["parent_trace_id"], "D");
                }
            }
        }
        let plan = runs
            .iter()
            .flat_map(|r| r["spans"].as_array().unwrap())
            .find(|s| s["name"] == "w4_plan")
            .unwrap();
        assert_eq!(plan["recognition"], "summary");
        assert_eq!(plan["output"]["model_called"], false);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn af381_v2_summary_commit_and_pipeline_summaries_are_completed() {
        let path = std::env::temp_dir().join(format!("af381-{}.jsonl", std::process::id()));
        let events = vec![
            (
                "transaction_committed",
                json!({"before":null,"after":null,"business_changed":true,
                "sources_changed":true,"committed_revision":8,"read_revision":7,
                "counts":{"relations":1,"sources":2,"updates":3},"duration_ms":2,"revision_observability":"persisted"}),
            ),
            (
                "w2_summary",
                json!({"total":2,"admitted":1,"unresolved":1,"rejected":0,"reasons":{}}),
            ),
            (
                "w5_commit",
                json!({"decision_types":["new"],"product_ids":[12],"ids_truncated":false,"revision":8}),
            ),
            (
                "r3_evidence",
                json!({"records":1,"raw":2,"record_ids":[12],"source_ids":["s"],
                "ids_truncated":false,"id_limit":128,"completeness":{}}),
            ),
            (
                "r5_final",
                json!({"status":"insufficient","facts":0,"computed":0,"records":1,"raw":2}),
            ),
            (
                "retrieval",
                json!({"status":"unavailable","query_error":"connect","index_state":"pending",
                "query_elapsed_ms":3,"cache_elapsed_ms":1,"sources_total":2,"sources_indexed":0,
                "segments_total":4,"segments_indexed":0}),
            ),
        ];
        let rows: Vec<_> = events
            .iter()
            .enumerate()
            .map(|(i, (kind, body))| {
                json!({"trace_version":"2","event_id":i.to_string(),"kind":kind,"at":"fixture",
                "meta":{"trace_id":"internal","attempt_id":"attempt"},"content_deps":{},"body":body})
            })
            .collect();
        std::fs::write(
            &path,
            rows.iter().map(|r| format!("{r}\n")).collect::<String>(),
        )
        .unwrap();
        let bundle = export_runtime(&path, true).unwrap();
        assert_eq!(bundle["meta"]["parse_errors"], json!([]));
        let run = &bundle["runs"][0];
        assert_eq!(run["committed_revision"], "8");
        assert_eq!(run["save_status"], "committed");
        let spans = run["spans"].as_array().unwrap();
        assert_eq!(spans.len(), 6);
        assert!(
            spans
                .iter()
                .all(|s| s["status"] == "completed" && s.get("error").is_none())
        );
        assert_eq!(spans[0]["recognition"], "transaction_summary");
        assert_eq!(spans[2]["output"]["product_ids"], json!([12]));
        // No persisted revision => no invented successful commit.
        let mut invalid = rows[0].clone();
        invalid["body"]["committed_revision"] = Value::Null;
        std::fs::write(&path, format!("{invalid}\n")).unwrap();
        let invalid = export_runtime(&path, true).unwrap();
        assert_eq!(invalid["runs"][0]["spans"][0]["status"], "incomplete");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn af368_v2_envelope_and_v1_remain_readable() {
        let path = std::env::temp_dir().join(format!("af368-trace-{}", std::process::id()));
        let row = json!({"trace_version":"2","event_id":"event","kind":"model_start","at":"fixture",
            "meta":{"trace_id":"trace-internal","source_id":"internal-source","round":1,"attempt_id":"attempt"},
            "content_deps":{"deps_complete":false,"sources":[],"memories":[]},
            "body":{"stage":"w1_v5","call_id":"call","attempt":1,"request":{"model":"fixture"}}});
        std::fs::write(&path, format!("{row}\n")).unwrap();
        let out = export_runtime(&path, false).unwrap();
        std::fs::remove_file(path).unwrap();
        assert_eq!(out["llm_calls"][0]["stage"], "w1_v5");
        assert_eq!(out["runs"][0]["id"], "trace-internal");
        assert_eq!(out["runs"][0]["round"], 1);
        assert_eq!(out["runs"][0]["attempt_id"], "attempt");
        assert_eq!(
            out["runs"][0]["spans"][0]["content_deps"]["deps_complete"],
            false
        );
        assert_eq!(
            out["runs"][0]["linked_source_ids"],
            json!(["internal-source"])
        );
        assert_eq!(out["runs"][0]["evidence_status"], "evidence_incomplete");
    }

    #[test]
    fn af364_all_shape_kinds_keep_current_calls_and_opaque_receipt_keys() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/af364-event-shapes.jsonl");
        let bundle = export_runtime(&path, false).unwrap();
        assert_eq!(bundle["meta"]["parse_errors"], json!([]));
        assert_eq!(bundle["runs"][0]["parse_errors"], json!([]));
        assert_eq!(bundle["llm_calls"].as_array().unwrap().len(), 1);
        assert_eq!(bundle["llm_calls"][0]["usage"]["input_tokens"], 10);
        assert_eq!(bundle["runs"][0]["recall_status"], "supported");
        assert_eq!(bundle["memories"].as_array().unwrap().len(), 1);
        assert_eq!(
            bundle["runs"][0]["mcp_response"]["body"]["structuredContent"]["recall"]["facts"][0]["record_ids"],
            json!([1])
        );
    }

    #[test]
    fn af364_bad_events_are_isolated_with_paths_and_no_body_in_errors() {
        let bundle = export_fixture(
            "isolation",
            vec![
                (
                    "r",
                    "run_start",
                    json!({"owner":"owner","tool":"memory_recall","arguments":null}),
                ),
                (
                    "r",
                    "validation",
                    json!({"stage":"initial_snapshot","status":"completed","parsed_result":{
                "repository":{"memories":{}},"sources":{"private-host-key":{"id":"s","request":{"history":["private-body"]}}}}}),
                ),
                (
                    "r",
                    "model_start",
                    json!({"call_id":"c","stage":"r4_v7","attempt":1,"request":{"model":"fixture"}}),
                ),
                (
                    "r",
                    "model_end",
                    json!({"call_id":"c","response":"private-body","status":"completed","duration_ms":1}),
                ),
                (
                    "r",
                    "model_end",
                    json!({"call_id":"c","status":"failed","response":null,"duration_ms":2,
                "accounting":{"input_tokens":5,"output_tokens":0},"parsed_result":null}),
                ),
                (
                    "r",
                    "retrieval",
                    json!({"runtime":{},"result":{"facts":[],"trace":"private-body"}}),
                ),
                ("r", "future_event", json!({"secret":"private-body"})),
            ],
        );
        let errors = bundle["runs"][0]["parse_errors"].as_array().unwrap();
        assert_eq!(errors.len(), 4);
        assert_eq!(errors[0]["event_id"], "0");
        assert_eq!(errors[0]["kind"], "run_start");
        assert_eq!(errors[0]["field"], "$.data.arguments: expected object");
        assert_eq!(
            errors[1]["field"],
            "$.data.parsed_result.sources.*.request.text: missing"
        );
        assert_eq!(errors[2]["field"], "$.data.response: expected object");
        assert_eq!(errors[3]["field"], "$.data.result.trace: expected array");
        assert!(!serde_json::to_string(errors).unwrap().contains("private"));
        assert_eq!(bundle["llm_calls"][0]["status"], "failed");
        assert_eq!(bundle["llm_calls"][0]["usage"]["input_tokens"], 5);
        assert_eq!(
            bundle["llm_calls"][0]["usage"]["source"],
            "recorded_accounting"
        );
        assert!(
            bundle["llm_calls"][0]["missing_fields"]
                .as_array()
                .unwrap()
                .contains(&json!("response"))
        );
        assert_eq!(bundle["runs"][0]["evidence_status"], "evidence_incomplete");
        assert!(
            !bundle["sources"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s["id"] == "s")
        );
    }

    #[test]
    fn af364_invalid_json_and_envelope_do_not_discard_good_events() {
        let path = std::env::temp_dir().join(format!("af364-lines-{}", std::process::id()));
        std::fs::write(&path,concat!(
            "{invalid private body}\n",
            "{\"trace_version\":\"1\",\"event_id\":\"bad\",\"kind\":\"model_start\",\"data\":{}}\n",
            "{\"trace_version\":\"1\",\"event_id\":\"ok\",\"kind\":\"future\",\"run_id\":\"r\",\"at\":\"fixture\",\"data\":{}}\n"
        )).unwrap();
        let bundle = export_runtime(&path, false).unwrap();
        std::fs::remove_file(path).unwrap();
        assert_eq!(bundle["meta"]["parse_errors"].as_array().unwrap().len(), 2);
        assert_eq!(
            bundle["meta"]["parse_errors"][1]["field"],
            "$.run_id: missing"
        );
        assert_eq!(bundle["runs"].as_array().unwrap().len(), 1);
        assert!(
            !bundle["meta"]["parse_errors"]
                .to_string()
                .contains("private")
        );
    }

    #[test]
    fn af364_current_facts_are_not_legacy_memories() {
        let bundle = export_fixture(
            "current-fact",
            vec![
                (
                    "r",
                    "run_start",
                    json!({"owner":"owner","tool":"memory_prepare","arguments":{}}),
                ),
                (
                    "r",
                    "run_end",
                    json!({"duration_ms":1,"evidence_status":"complete","response":{
                "structuredContent":{"recall":{"version":"recall.v11","status":"supported",
                "facts":[{"statement":"虚构陈述","record_ids":[1],"evidence":[]}],
                "records":[],"raw":[],"statement_snapshot":{"records":[]}}}}}),
                ),
            ],
        );
        assert_eq!(
            bundle["runs"][0]["mcp_response"]["body"]["structuredContent"]["recall"]["facts"][0]["record_ids"],
            json!([1])
        );
        assert_eq!(bundle["memories"], json!([]));
    }

    #[test]
    fn af363_current_background_events_keep_raw_checks_transport_and_unknown_separate() {
        let bundle = export_fixture(
            "background",
            vec![
                (
                    "op",
                    "model_start",
                    json!({"call_id":"c","stage":"w1_v123","attempt":1,
                "request":{"model":"fictional-model","reasoning":{"effort":"low"},"text":{"format":{"name":"w1_v123"}},
                "input":"{\"source_id\":\"synthetic-source\"}"}}),
                ),
                (
                    "op",
                    "model_end",
                    json!({"call_id":"c","status":"completed","local_outcome":"completed","duration_ms":12,
                "response":{"usage":{"input_tokens":12,"output_tokens":7,"input_tokens_details":{"cached_tokens":4},"output_tokens_details":{"reasoning_tokens":2}}},
                "parsed_result":{"candidates":["raw"]},"parsing_status":"parsed"}),
                ),
                (
                    "op",
                    "model_check",
                    json!({"stage":"W1","phase":"decode","status":"completed"}),
                ),
                (
                    "op",
                    "model_check",
                    json!({"stage":"W1","phase":"validate","status":"failed","rule":"quote","field":"candidates[0]"}),
                ),
                (
                    "op",
                    "model_transport_error",
                    json!({"call_id":"c","stage":"w1_v123","error_chain":["connect"],"elapsed_ms":9}),
                ),
                (
                    "op",
                    "future_event",
                    json!({"source_id":"synthetic-source","value":42}),
                ),
                (
                    "op",
                    "retrieval",
                    json!({"status":"unavailable","query_error":"connect","index_state":"pending","sources_total":4,"segments_indexed":0,"query_elapsed_ms":9}),
                ),
            ],
        );
        let call = &bundle["llm_calls"][0];
        assert_eq!(call["stage"], "w1_v123");
        assert_eq!(call["schema_name"], "w1_v123");
        assert_eq!(call["model"], "fictional-model");
        assert_eq!(call["effort"], "low");
        assert_eq!(call["usage"]["cached_input_tokens"], 4);
        assert_eq!(call["usage"]["reasoning_tokens"], 2);
        assert_eq!(call["parsed_result"]["candidates"][0], "raw");
        assert!(call["verified_final"].is_null());
        assert_eq!(call["checks"][1]["rule"], "quote");
        assert_eq!(call["transport_error"]["error_chain"][0], "connect");
        let run = &bundle["runs"][0];
        assert_eq!(run["missing_run_start"], true);
        assert_eq!(run["evidence_status"], "evidence_incomplete");
        assert_eq!(run["linked_source_ids"], json!(["synthetic-source"]));
        assert_eq!(run["retrieval_diagnostics"]["query_error"], "connect");
        assert!(
            run["spans"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s["recognition"] == "unrecognized")
        );
        assert!(bundle["memory_snapshot"]["scope"].is_null());
    }

    #[test]
    fn af363_orphan_end_and_explicit_final_are_not_invented_from_raw() {
        let bundle = export_fixture(
            "end",
            vec![
                (
                    "orphan",
                    "model_end",
                    json!({"call_id":"c","stage":"r4_v999","status":"completed","duration_ms":1,"parsed_result":{"facts":["raw"]}}),
                ),
                (
                    "orphan",
                    "model_check",
                    json!({"call_id":"c","stage":"R4","status":"completed","phase":"validate","final":{"facts":["verified"]}}),
                ),
            ],
        );
        assert_eq!(bundle["llm_calls"][0]["missing_model_start"], true);
        assert_eq!(
            bundle["llm_calls"][0]["verified_final"]["facts"][0],
            "verified"
        );
        assert_eq!(bundle["llm_calls"][0]["parsed_result"]["facts"][0], "raw");
        assert_eq!(bundle["runs"][0]["evidence_status"], "evidence_incomplete");
    }

    #[test]
    fn af363_current_recall_response_preserves_snapshot_and_missing_tail() {
        let bundle = export_fixture(
            "response",
            vec![
                (
                    "r",
                    "run_start",
                    json!({"owner":"owner","tool":"memory_recall","arguments":{"query":"fictional"}}),
                ),
                (
                    "r",
                    "run_end",
                    json!({"duration_ms":1,"evidence_status":"complete","response":{"structuredContent":{
                "contract":"recall.v11","status":"supported","statement_snapshot":{"records":[]},"facts":[],
                "source_evidence":[],"historical_or_conflicting":[]}}}),
                ),
            ],
        );
        assert_eq!(
            bundle["runs"][0]["mcp_response"]["body"]["structuredContent"]["statement_snapshot"],
            json!({"records":[]})
        );
        assert_eq!(bundle["runs"][0]["status"], "completed");
    }

    #[test]
    fn local_raw_response_is_not_reported_as_empty() {
        let path =
            std::env::temp_dir().join(format!("pmr-s4-web-trace-{}.jsonl", std::process::id()));
        for (sources, historical, expected) in [
            (
                json!([{"source_id":"s1","text":"raw"}]),
                json!([]),
                "completed",
            ),
            (json!([]), json!([]), "completed_empty"),
            (
                json!([]),
                json!([{"kind":"source_with_state_caveats","source":{"source_id":"s1"}}]),
                "completed",
            ),
        ] {
            let result = json!({"mandatory":[],"constraints":[],"preferences":[],"facts":[],"episodes":[],"conflicts":[],"trace":[]});
            let response = json!({"result":result,"source_evidence":sources,"historical_or_conflicting":historical,"retrieval_mode":"local"});
            for prepare in [false, true] {
                let events = [
                    (
                        "run_start",
                        json!({"owner":"owner","tool":if prepare {"memory_prepare"} else {"memory_recall"},"arguments":{}}),
                    ),
                    (
                        "retrieval",
                        json!({"runtime":{},"result":result,"memories":[]}),
                    ),
                    ("local_recall", response.clone()),
                    (
                        "run_end",
                        json!({"evidence_status":"complete","duration_ms":1,"response":{"structuredContent":if prepare {
                        json!({"remember":{"status":"failed"},"recall":{"status":"completed","result":response}})
                    } else {response.clone()}}}),
                    ),
                ];
                let raw = events.iter().enumerate().map(|(i,(kind,data))|json!({"trace_version":"1","event_id":format!("e{i}"),"run_id":"run","at":"2026-09-23T00:00:00Z","kind":kind,"data":data}).to_string()+"\n").collect::<String>();
                std::fs::write(&path, raw).unwrap();
                let bundle = export_runtime(&path, false).unwrap();
                assert_eq!(bundle["runs"][0]["recall_status"], expected);
                assert_eq!(
                    bundle["runs"][0]["mcp_response"]["returned_source_count"],
                    sources.as_array().unwrap().len() + historical.as_array().unwrap().len()
                );
            }
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn lexical_candidates_remain_raw_evidence_without_mcp_or_memory_claims() {
        let path = std::env::temp_dir().join(format!("pmr-s3-trace-{}.jsonl", std::process::id()));
        for status in ["completed", "incomplete", "disabled", "failed"] {
            let lexical = json!({"channel":"lexical_raw","query":"合成项目 SQLite","status":status,"processing_version":"fixture-v1","generation":if status == "disabled" {Value::Null}else{json!(1)},"source_revision":3,"candidate_count":1,"coverage_complete":status == "completed","duration_ms":2,"candidates":[{"source_id":"source-fixture","segment_id":"segment-fixture","segment_version":"version-fixture","byte_start":0,"byte_end":6,"text":"合成","mapping_uncertain":true,"known_memories":[]}]});
            let events = [
                (
                    "run_start",
                    json!({"owner":"owner","tool":"lexical_search","arguments":{"query":"合成项目 SQLite"}}),
                ),
                ("lexical_raw", lexical.clone()),
                (
                    "run_end",
                    json!({"status":"completed","evidence_status":"complete","duration_ms":2,"result":lexical,"error":null}),
                ),
            ];
            let raw = events.iter().enumerate().map(|(i,(kind,data))|json!({"trace_version":"1","event_id":format!("e{i}"),"run_id":"run","at":"2026-09-22T00:00:00Z","kind":kind,"data":data}).to_string()+"\n").collect::<String>();
            std::fs::write(&path, raw).unwrap();
            let bundle = export_runtime(&path, false).unwrap();
            let run = &bundle["runs"][0];
            assert_eq!(run["lexical_raw"], lexical);
            assert_eq!(
                run["spans"][1]["status"],
                if status == "disabled" {
                    "not_run"
                } else {
                    status
                }
            );
            assert_eq!(
                bundle["meta"]["channels"]["lexical_index"],
                if status == "disabled" {
                    "not_enabled"
                } else {
                    "enabled"
                }
            );
            assert!(run["retrieval"]["candidates"].is_null());
            assert!(run["mcp_response"]["returned_refs"].is_null());
            assert_eq!(run["mcp_response"]["recorded"], false);
            assert_eq!(bundle["memories"], json!([]));
            assert_eq!(bundle["llm_calls"], json!([]));
            assert!(run["lexical_raw"]["candidates"][0].get("score").is_none());
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn deferred_capture_and_cli_processing_keep_distinct_statuses() {
        let path = std::env::temp_dir().join(format!(
            "pmr-s5-trace-{}-{}.jsonl",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let source = json!({"status":"stored","source_id":"s1"});
        let events = [
            (
                "capture",
                "run_start",
                json!({"owner":"owner","tool":"memory_remember","arguments":{"text":"fact","session_id":"session","turn_id":"turn","capture_source":true}}),
            ),
            (
                "capture",
                "source_receipt",
                json!({"source":source,"index":{"status":"ready"},"enrichment":{"status":"queued","error":null}}),
            ),
            (
                "capture",
                "run_end",
                json!({"evidence_status":"complete","duration_ms":1,"response":{"structuredContent":{"source":source,"index":{"status":"ready"},"enrichment":{"status":"queued","error":null},"status":"queued"}}}),
            ),
            (
                "process",
                "run_start",
                json!({"owner":"owner","tool":"source_enrichment","entrypoint":"internal_cli","arguments":{"source_id":"s1","attempt_id":"attempt"}}),
            ),
            (
                "process",
                "source_receipt",
                json!({"source":source,"index":{"status":"ready"},"enrichment":{"status":"committed","error":null}}),
            ),
            (
                "process",
                "run_end",
                json!({"status":"completed","evidence_status":"complete","duration_ms":1,"result":{"source":source,"index":{"status":"ready"},"enrichment":{"status":"committed","error":null},"status":"completed"}}),
            ),
        ];
        let raw = events.iter().enumerate().map(|(i,(run,kind,data))|json!({"trace_version":"1","event_id":format!("e{i}"),"run_id":run,"at":"2026-09-24T00:00:00Z","kind":kind,"data":data}).to_string()+"\n").collect::<String>();
        std::fs::write(&path, raw).unwrap();
        let bundle = export_runtime(&path, false).unwrap();
        let capture = &bundle["runs"][0];
        let process = &bundle["runs"][1];
        assert_eq!(capture["save_status"], "queued");
        assert_eq!(process["entrypoint"], "internal_cli");
        assert_eq!(
            process["source"]["source_id"],
            capture["source"]["source_id"]
        );
        assert_eq!(process["save_status"], "committed");
        assert_eq!(
            process["spans"].as_array().unwrap().last().unwrap()["name"],
            "internal_enrichment_run_end"
        );
        assert_eq!(process["mcp_response"]["recorded"], false);
        assert!(process["notes"].as_str().unwrap().contains("显式处理原文"));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn source_status_and_real_revisions_are_optional_trace_fields() {
        let path = std::env::temp_dir().join(format!(
            "pmr-s2-trace-{}-{}.jsonl",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let receipt = json!({"source":{"status":"stored","source_id":"s1","provenance":"mcp_received","message_id_trust":"client_supplied"},"index":{"status":"not_requested"},"enrichment":{"status":"pending","error":null}});
        let events = [
            (
                "run_start",
                json!({"owner":"owner","tool":"memory_remember","arguments":{"session_id":"s","turn_id":"t","text":"a fact"}}),
            ),
            ("source_receipt", receipt.clone()),
            (
                "transaction_committed",
                json!({"owner":"owner","before":null,"after":{"revision":2,"repository":{"memories":{}},"receipts":{},"sources":{"turn-key":{"id":"s1","request":{"text":"a fact"}}}},"read_revision":1,"committed_revision":2}),
            ),
            (
                "run_end",
                json!({"evidence_status":"complete","duration_ms":1,"response":{"structuredContent":{"remember":{"status":"failed","source":receipt["source"],"index":receipt["index"],"enrichment":{"status":"failed","error":"model failed"}},"recall":{"status":"not_requested"}}}}),
            ),
        ];
        let raw = events.iter().enumerate().map(|(i,(kind,data))|json!({"trace_version":"1","event_id":format!("e{i}"),"run_id":"run","at":"2026-09-22T00:00:00Z","kind":kind,"data":data}).to_string()+"\n").collect::<String>();
        std::fs::write(&path, raw).unwrap();
        let bundle = export_runtime(&path, false).unwrap();
        let run = &bundle["runs"][0];
        assert_eq!(run["source"]["status"], "stored");
        assert_eq!(run["spans"][0]["recorded_at"], "2026-09-22T00:00:00Z");
        assert_eq!(run["source_ids"], json!(["run:source", "s1"]));
        assert_eq!(
            bundle["sources"]
                .as_array()
                .unwrap()
                .iter()
                .find(|source| source["id"] == "s1")
                .unwrap()["kind"],
            "persisted_source"
        );
        assert_eq!(run["enrichment"]["status"], "failed");
        assert_eq!(run["recall_status"], "not_requested");
        assert_eq!(run["retrieval"]["query_status"], "not_requested");
        assert_eq!(run["read_revision"], "1");
        assert_eq!(run["committed_revision"], "2");
        assert_eq!(bundle["meta"]["channels"]["source_capture"], "enabled");
        assert_eq!(bundle["memory_snapshot"]["revision"], "2");
        std::fs::remove_file(path).unwrap();
    }
}
