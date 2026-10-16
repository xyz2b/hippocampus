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

fn required<'a>(value: &'a Value, key: &str) -> ExportResult<&'a Value> {
    value
        .as_object()
        .ok_or("TypeError")?
        .get(key)
        .ok_or_else(|| "KeyError".into())
}
fn string(value: &Value) -> ExportResult<&str> {
    value.as_str().ok_or_else(|| "TypeError".into())
}
fn array(value: &Value) -> ExportResult<&Vec<Value>> {
    value.as_array().ok_or_else(|| "TypeError".into())
}
fn object(value: &Value) -> ExportResult<&Map<String, Value>> {
    value.as_object().ok_or_else(|| "AttributeError".into())
}
fn require(condition: bool) -> ExportResult<()> {
    if condition {
        Ok(())
    } else {
        Err("ValueError".into())
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
        require(required(memory, "owner_id")? == &self.owner)?;
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
        let mut references = self.result_refs(&Value::Object(value.clone()))?;
        for child in value.values().filter(|value| value.is_object()) {
            references.extend(self.response_refs(child)?);
        }
        let mut seen = HashSet::new();
        references.retain(|reference| seen.insert(reference.clone()));
        Ok(references)
    }
}

pub(super) fn export_runtime(path: &Path, allow_partial: bool) -> ExportResult<Value> {
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
    // bytes.splitlines() treats CRLF as one boundary, and also accepts bare CR.
    let mut rows: Vec<Value> = Vec::new();
    let mut start = 0;
    let mut offset = 0;
    while offset < complete.len() {
        if matches!(complete[offset], b'\r' | b'\n') {
            rows.push(
                serde_json::from_slice(&complete[start..offset]).map_err(|_| "JSONDecodeError")?,
            );
            if complete[offset] == b'\r' && complete.get(offset + 1) == Some(&b'\n') {
                offset += 1;
            }
            start = offset + 1;
        }
        offset += 1;
    }
    if start < complete.len() {
        rows.push(serde_json::from_slice(&complete[start..]).map_err(|_| "JSONDecodeError")?);
    }
    require(!rows.is_empty())?;
    let mut event_ids = HashSet::new();
    let mut owners = Vec::new();
    for row in &rows {
        require(required(row, "trace_version")? == "1")?;
        require(event_ids.insert(string(required(row, "event_id")?)?.to_owned()))?;
        if required(row, "kind")? == "run_start" {
            let owner = required(required(row, "data")?, "owner")?;
            if !owners.contains(owner) {
                owners.push(owner.clone());
            }
        }
    }
    require(owners.len() == 1)?;
    let owner = owners.remove(0);
    let mut bundle = json!({
        "schema_version":"0.1.0", "meta":{
            "title":"S1 运行时诊断记录", "data_kind":"recorded", "trace_sha256":digest(&raw),
            "tail_status":if complete == raw {"complete"}else{"partial_line"},
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
        require(starts.len() == 1)?;
        let start = starts[0].1;
        let args = required(required(start, "data")?, "arguments")?;
        let tool = string(required(required(start, "data")?, "tool")?)?;
        let internal = start["data"]["entrypoint"] == "internal_cli"
            || ["lexical_search", "source_index_rebuild"].contains(&tool);
        let source_enrichment = internal && tool == "source_enrichment";
        object(args)?;
        let user_turn = args.get("user_turn").unwrap_or(args);
        object(user_turn)?;
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
            "started_at":required(start,"at")?, "source_ids":[source_id], "session_id":user_turn.get("session_id"),
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
        let mut calls: Vec<Value> = Vec::new();
        let mut spans: Vec<Value> = Vec::new();
        let mut model_spans: Vec<(String, usize)> = Vec::new();
        let mut latest_call: Option<usize> = None;
        let mut current_state: Option<&Value> = None;
        let mut state_event: Option<(usize, &Value)> = None;
        let mut retrieval: Option<&Value> = None;
        let mut ended = false;
        let mut write_changes = Vec::new();
        for (position, row) in events {
            let kind = string(required(row, "kind")?)?;
            let data = required(row, "data")?;
            object(data)?;
            let mut span = json!({"id":required(row,"event_id")?,"name":kind,"status":"completed","recorded_at":required(row,"at")?,"output":data,"llm_call_ids":[]});
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
                    require(!calls.iter().any(|call| call["id"] == call_id))?;
                    let stage = string(required(data, "stage")?)?;
                    let request = required(data, "request")?;
                    let attempt = required(data, "attempt")?;
                    let call = json!({
                        "id":call_id,"run_id":run_id,"operation_id":run_id,"stage":stage,"status":"unknown",
                        "attempt":attempt,"started_at":required(row,"at")?,"provider":data.get("provider"),"model":null,
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
                    let call_index = calls
                        .iter()
                        .position(|call| call["id"] == call_id)
                        .ok_or("ValueError")?;
                    require(calls[call_index]["status"] == "unknown")?;
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
                    for key in ["status", "duration_ms"] {
                        call[key] = required(data, key)?.clone();
                    }
                    call["response"] = envelope.clone();
                    call["model"] = envelope["model"].clone();
                    call["provider_request_id"] = envelope["id"].clone();
                    call["parsed_result"] = data["parsed_result"].clone();
                    call["parsing_status"] = data["parsing_status"].clone();
                    call["finish_reason"] = details
                        .get("reason")
                        .filter(|v| truthy(v))
                        .unwrap_or(&data["error"])
                        .clone();
                    call["usage"] = json!({"input_tokens":usage.get("input_tokens"),"output_tokens":usage.get("output_tokens"),
                        "reasoning_tokens":output_details.get("reasoning_tokens"),"cached_input_tokens":input_details.get("cached_tokens"),
                        "source":if truthy(usage){"recorded_response"}else{"unknown"}});
                    let span_index = model_spans
                        .iter()
                        .find(|(id, _)| id == call_id)
                        .ok_or("ValueError")?
                        .1;
                    let model_span = &mut spans[span_index];
                    model_span["status"] = required(data, "status")?.clone();
                    model_span["output"] = data.clone();
                    model_span["duration_ms"] = required(data, "duration_ms")?.clone();
                    if truthy(&data["error"]) {
                        model_span["error"] = json!({"reason":data["error"]});
                    }
                    continue;
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
                    require(required(data, "owner")? == &captured.owner)?;
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
                                let result = required(receipt, "result")?;
                                object(result)?;
                                if let Some(outcomes) = result.get("outcomes") {
                                    for outcome in array(outcomes)? {
                                        object(outcome)?;
                                        if let Some(ingested) =
                                            outcome.get("Ingested").filter(|v| v.is_object())
                                        {
                                            let applied = ingested.get("Applied").unwrap_or(&empty);
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
                            let candidates = extraction
                                .and_then(|call| call["domain_validation"].get("parsed_result"));
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
                    require(data["channel"] == "lexical_raw")?;
                    let status = string(required(data, "status")?)?;
                    span["status"] = json!(match status {
                        "completed" | "incomplete" | "failed" => status,
                        "disabled" => "not_run",
                        _ => "unknown",
                    });
                    if status == "completed" || data.get("generation").is_some_and(|v| !v.is_null())
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
                        return Err("trace_retrieval_schema_unsupported: 当前 retrieval 是覆盖/错误诊断，旧解析器要求 result/runtime；本期未恢复此 Trace 正文".into());
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
                }
                "run_end" => {
                    require(!ended)?;
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
                        run["evidence_status"] = json!(if data["evidence_status"] == "complete" {
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
                            run["save_status"] = run["enrichment"]["status"].as_str().map_or_else(
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
                        continue;
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
                    run["evidence_status"] = required(data, "evidence_status")?.clone();
                    run["duration_ms"] = required(data, "duration_ms")?.clone();
                    run["mcp_response"]["recorded"] = json!(true);
                    run["mcp_response"]["body"] = response.clone();
                    run["mcp_response"]["returned_refs"] = json!(captured.response_refs(body)?);
                    let recalled = body
                        .get("recall")
                        .and_then(|r| r.get("result"))
                        .unwrap_or(body);
                    let historical = recalled["historical_or_conflicting"].as_array();
                    let source_count = recalled["source_evidence"].as_array().map_or(0, Vec::len)
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
                        run["recall_status"] = json!(if truthy(&run["retrieval"]["selected_refs"])
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
                _ => {}
            }
            if truthy(&data["error"]) {
                span["error"] = json!({"reason":data["error"]});
            }
            spans.push(span);
            if let Some(state) = current_state.filter(|state| truthy(state)) {
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
        }
        if let (Some(state), Some((position, event))) =
            (current_state.filter(|state| truthy(state)), state_event)
            && latest_snapshot_position.is_none_or(|latest| position > latest)
        {
            latest_snapshot_position = Some(position);
            bundle["memory_snapshot"]["completeness"] = json!("complete");
            bundle["memory_snapshot"]["state"] = state.clone();
            bundle["memory_snapshot"]["captured_at"] = required(event, "at")?.clone();
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
        if let Some(retrieval) = retrieval.filter(|value| truthy(value)) {
            let result = required(retrieval, "result")?;
            let mut candidates = Vec::new();
            for entry in array(required(result, "trace")?)? {
                let decision = required(entry, "decision")?;
                let memory_id = required(entry, "memory_id")?;
                let selected = matches!(decision.as_str(), Some("Selected" | "Mandatory"));
                let mut memory = None;
                for lane in LANES {
                    for value in array(required(result, lane)?)? {
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
                    && let Some(memories) = retrieval.get("memories").filter(|v| truthy(v))
                {
                    for value in array(memories)? {
                        if required(value, "id")? == memory_id {
                            memory = Some(value);
                            break;
                        }
                    }
                }
                let memory = memory.ok_or("ValueError")?;
                candidates.push(json!({"memory_ref":captured.memory(memory)?,"outcome":if selected{"selected"}else{"filtered"},
                    "reason_codes":[decision.as_str().map(str::to_owned).unwrap_or_else(||python_json(decision,false))],"rank":null,"score":null}));
            }
            run["retrieval"]["candidates"] = json!(candidates);
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
        run["spans"] = json!(spans);
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
