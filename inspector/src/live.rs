use super::{fingerprint, now, python_json, safe, trace::export_runtime, utc};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::{Value, json};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::Duration,
};

pub(super) struct Config {
    pub db: PathBuf,
    pub owner: String,
    pub logs: Vec<PathBuf>,
    pub trace_dir: Option<PathBuf>,
}

fn read_state(db: &Path, owner: &str) -> Result<Value, String> {
    let conn =
        Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(sql_error)?;
    conn.busy_timeout(Duration::from_secs(1))
        .map_err(sql_error)?;
    conn.execute_batch("PRAGMA query_only=ON; BEGIN")
        .map_err(sql_error)?;
    let row: Option<String> = conn
        .query_row(
            "SELECT state_json FROM memory_profiles WHERE owner_id=?",
            [owner],
            |r| r.get(0),
        )
        .optional()
        .map_err(sql_error)?;
    let raw = row.ok_or("owner 尚无持久快照；不能当作已观测空库")?;
    if raw.chars().count() > 16 * 1024 * 1024 {
        return Err("owner 快照超过 16 MiB，需按范围读取".into());
    }
    let state: Value = serde_json::from_str(&raw).map_err(|e| super::json_error(&raw, &e))?;
    for (section, field) in [("repository", "memories"), ("staging", "pending")] {
        let section_value = get(&state, section)?;
        let value = get(section_value, field)?;
        if !value.is_object() {
            return Err("不兼容的 SQLite 快照结构".into());
        }
    }
    if !get(&state, "receipts")?.is_object()
        || state
            .get("sources")
            .is_some_and(|sources| !sources.is_object())
    {
        return Err("不兼容的回执结构".into());
    }
    Ok(state)
}
fn sql_error(e: rusqlite::Error) -> String {
    match e {
        rusqlite::Error::SqliteFailure(code, _) if code.code == rusqlite::ErrorCode::CannotOpen => {
            "unable to open database file".into()
        }
        rusqlite::Error::SqliteFailure(_, Some(message)) => message,
        other => other.to_string(),
    }
}
fn io_class(error: &std::io::Error) -> &'static str {
    match error.kind() {
        std::io::ErrorKind::NotFound => "FileNotFoundError",
        std::io::ErrorKind::PermissionDenied => "PermissionError",
        std::io::ErrorKind::IsADirectory => "IsADirectoryError",
        _ => "OSError",
    }
}
fn log_tail(path: &Path) -> Value {
    let mut result = json!({"name":path.file_name().unwrap_or_default().to_string_lossy(),"path":path.to_string_lossy(),"status":"available","lines":[]});
    let read = (|| -> std::io::Result<_> {
        let mut stream = File::open(path)?;
        let meta = stream.metadata()?;
        let start = meta.len().saturating_sub(65536);
        stream.seek(SeekFrom::Start(start))?;
        let mut raw = Vec::new();
        stream.take(65536).read_to_end(&mut raw)?;
        if start > 0 {
            raw = raw
                .iter()
                .position(|b| *b == b'\n')
                .map(|i| raw[i + 1..].to_vec())
                .unwrap_or_default()
        }
        Ok((meta, start, raw))
    })();
    match read {
        Ok((meta, start, raw)) => {
            let decoded = String::from_utf8_lossy(&raw);
            // str.splitlines also recognizes lone CR and Unicode line separators.
            let lines = python_lines(&decoded);
            #[cfg(unix)]
            let identity = {
                use std::os::unix::fs::MetadataExt;
                format!("{}:{}", meta.dev(), meta.ino())
            };
            #[cfg(not(unix))]
            let identity = String::from("unavailable");
            result["size_bytes"] = json!(meta.len());
            result["identity"] = json!(identity);
            result["modified_at"] = meta
                .modified()
                .ok()
                .and_then(|t| super::compat::file_time(t).ok())
                .unwrap_or(Value::Null);
            result["truncated"] = json!(start > 0 || lines.len() > 100);
            result["final_line_complete"] = json!(raw.is_empty() || raw.ends_with(b"\n"));
            result["lines"] = Value::Array(
                lines
                    .into_iter()
                    .rev()
                    .take(100)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .map(|s| safe(&json!(s)))
                    .collect(),
            );
        }
        Err(e) => {
            result["status"] = json!("unavailable");
            result["error"] = json!(io_class(&e));
        }
    }
    result
}
fn python_lines(text: &str) -> Vec<&str> {
    let mut result = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if matches!(
            c,
            '\n' | '\r'
                | '\x0b'
                | '\x0c'
                | '\x1c'
                | '\x1d'
                | '\x1e'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        ) {
            result.push(&text[start..i]);
            start = i + c.len_utf8();
            if c == '\r' && chars.peek().is_some_and(|(_, c)| *c == '\n') {
                let (i, _) = chars.next().unwrap();
                start = i + 1;
            }
        }
    }
    if start < text.len() {
        result.push(&text[start..])
    }
    result
}
fn get<'a>(v: &'a Value, key: &str) -> Result<&'a Value, String> {
    match v {
        Value::Object(map) => map.get(key).ok_or_else(|| format!("'{key}'")),
        Value::Array(_) => Err("list indices must be integers or slices, not str".into()),
        Value::String(_) => Err("string indices must be integers, not 'str'".into()),
        _ => Err(format!("'{}' object is not subscriptable", python_type(v))),
    }
}
fn python_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
    }
}
fn array(v: &Value) -> Result<&Vec<Value>, String> {
    v.as_array()
        .ok_or_else(|| format!("'{}' object is not iterable", python_type(v)))
}
fn object(v: &Value) -> Result<&serde_json::Map<String, Value>, String> {
    v.as_object().ok_or("TypeError".into())
}
fn string(v: &Value) -> Result<&str, String> {
    v.as_str().ok_or("TypeError".into())
}
fn id(v: &Value) -> String {
    v.as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| v.to_string())
}

pub(super) fn live_bundle(config: &Config) -> Result<Value, String> {
    let Config {
        db,
        owner,
        logs,
        trace_dir,
    } = config;
    let captured = now();
    let mut privacy_filtered = false;
    let mut state_available = false;
    let mut result = json!({"schema_version":"0.1.0","meta":{"title":"当前本机记忆与服务日志","data_kind":"recorded","notes":"当前数据库快照及持久保存回执；回执不是完整 MCP 轨迹。未记录的模型请求、失败操作与宿主输入保持未知。"},
        "memory_snapshot":{"completeness":"not_captured","scope":owner,"revision":null,"captured_at":captured},
        "sources":[],"memories":[],"runs":[],"llm_calls":[],
        "live":{"captured_at":captured,"database":db.to_string_lossy(),"owner":owner,"status":"connected","errors":[],"logs":logs.iter().map(|p|log_tail(p)).collect::<Vec<_>>(),"refresh_seconds":2,"observability":"正式服务未采集完整 MCP / 模型请求链。这里读取已有记录，不触发业务调用。"}});
    match read_state(db, owner) {
        Err(e) => result["live"]["errors"]
            .as_array_mut()
            .unwrap()
            .push(json!(format!("数据库不可读取：{e}"))),
        Ok(state) => {
            state_available = true;
            let memories = object(&state["repository"]["memories"])?;
            let receipts = object(&state["receipts"])?;
            let snapshot = &mut result["memory_snapshot"];
            snapshot["completeness"] = json!("complete");
            snapshot["snapshot_sha256"] = json!(fingerprint(&state));
            snapshot["identity_note"] = json!(
                "数据库没有原生revision；版本字段是记录内容指纹，不是事务版本或历史读取证明。"
            );
            snapshot["pending"] = json!(
                object(&state["staging"]["pending"])?
                    .values()
                    .collect::<Vec<_>>()
            );
            snapshot["receipt_count"] = json!(receipts.len());
            if let Some(revision) = state.get("revision") {
                snapshot["revision"] = json!(id(revision));
                snapshot["identity_note"] =
                    json!("SQLite 当前 owner 的事务修订号；记忆版本仍是记录内容指纹。");
            }
            let current_refs: std::collections::HashMap<_, _> = memories
                .values()
                .map(|m| {
                    Ok((
                        id(get(m, "id")?),
                        format!("{}@{}", id(&m["id"]), &fingerprint(m)[..16]),
                    ))
                })
                .collect::<Result<_, String>>()?;
            let mut sources = serde_json::Map::new();
            result["meta"]["channels"] =
                json!({"source_capture":"unknown","lexical_index":"unknown"});
            if let Some(records) = state.get("sources") {
                if !object(records)?.is_empty() {
                    result["meta"]["channels"]["source_capture"] = json!("enabled");
                }
                for record in object(records)?.values() {
                    let sid = string(get(record, "id")?)?;
                    let request = get(record, "request")?;
                    let deleted = request.is_null();
                    privacy_filtered |= deleted;
                    let index = record
                        .get("index")
                        .cloned()
                        .unwrap_or_else(|| json!({"status":"not_requested"}));
                    if index["status"] == "ready" {
                        result["meta"]["channels"]["lexical_index"] = json!("enabled");
                    }
                    let status = json!({"source":{"status":if deleted{"deleted"}else{"stored"},"source_id":sid,"provenance":"mcp_received","message_id_trust":"client_supplied","received_sequence":record.get("received_sequence")},"index":index,"enrichment":{"status":get(record,"enrichment")?,"error":if deleted{Value::Null}else{record["error"].clone()}}});
                    if !deleted {
                        sources.insert(sid.into(), json!({"id":sid,"kind":"persisted_source","text":request["text"],"session_id":record["session_id"],"message_id":record["turn_id"],"captured_at":utc(&record["observed_at"])?,"verification":"SQLite 独立留存的 MCP 接收原文；客户端提供消息标识，不代表网页来源认证"}));
                    }
                    result["runs"].as_array_mut().unwrap().push(json!({
                        "id":format!("source-receipt:{sid}"),"tool":"memory_remember · 原文接收回执","status":"completed","started_at":utc(&record["observed_at"])?,"session_id":record["session_id"],"source_ids":if deleted{vec![]}else{vec![sid]},
                        "source":status["source"],"index":status["index"],"enrichment":status["enrichment"],"enrichment_task":record.get("task"),
                        "save_status":record["enrichment"],"recall_status":"not_applicable","input":{"mcp_text":if deleted{Value::Null}else{request["text"].clone()},"arguments":null},
                        "spans":[{"id":"source-receipt","name":"source_receipt","status":"completed","output":status,"llm_call_ids":[]}],"write_changes":[],
                        "retrieval":{"query":null,"query_status":"unknown","scanned_count":null,"candidates":null,"selected_refs":null},
                        "mcp_response":{"recorded":false,"returned_refs":null,"body":null,"delivery_status":"unknown"},"host":{"input_observability":"unavailable","request":null},
                        "notes":"来自 SQLite 的原文接收与整理状态；原文保存独立于模型处理。回执不是完整执行轨迹，索引状态仅按实际记录展示。"
                    }));
                }
            }
            for m in memories.values() {
                if get(m, "owner_id")? != owner {
                    return Err("快照 owner 不一致".into());
                }
                if m["status"]
                    .as_str()
                    .is_some_and(|status| status.eq_ignore_ascii_case("deleted"))
                {
                    continue;
                }
                let mut source_ids = Vec::new();
                for evidence in array(get(m, "evidence")?)? {
                    let sid = format!("evidence:{}", string(get(evidence, "id")?)?);
                    sources.insert(sid.clone(),json!({"id":sid,"kind":"persisted_evidence","text":get(evidence,"quote")?,"verification":"持久记忆的原始证据；不是宿主完整输入","evidence":evidence}));
                    source_ids.push(sid);
                }
                let temporal = get(m, "temporal")?;
                object(temporal).map_err(|_| {
                    format!("'{}' object has no attribute 'get'", python_type(temporal))
                })?;
                let mut memory = json!({"id":id(get(m,"id")?),"version":&fingerprint(m)[..16],
                    "content":format!("{} · {} · {}",string(get(m,"subject_id")?)?,string(get(m,"predicate")?)?,python_json(get(m,"value")?,false)),
                    "status":string(get(m,"status")?)?.to_lowercase(),"memory_type":get(m,"memory_type")?,"time_horizon":get(m,"retention_tier")?,
                    "owner":owner,"subject":get(m,"subject_id")?,"scope":get(m,"scope")?,"source_ids":source_ids,
                    "superseded_by":current_refs.get(&id(&m["superseded_by"])),"supersedes":current_refs.get(&id(&m["supersedes"])),"raw":m,"valid_to_unbounded":false});
                for key in ["observed_at", "valid_from", "valid_to", "expires_at"] {
                    memory[key] = if temporal[key].is_null() {
                        Value::Null
                    } else {
                        if temporal[key].is_object() && temporal[key].get("at").is_none() {
                            return Err("'at'".into());
                        }
                        utc(&temporal[key])?
                    };
                }
                memory["valid_to_unbounded"] =
                    json!(!temporal["valid_to"].is_null() && memory["valid_to"].is_null());
                result["memories"].as_array_mut().unwrap().push(memory);
            }
            for (key, receipt) in receipts {
                if privacy_filtered {
                    continue;
                }
                let sid = format!("receipt-source:{}", &fingerprint(&json!(key))[..16]);
                let request = get(receipt, "request")?;
                sources.insert(sid.clone(),json!({"id":sid,"kind":"receipt_request","text":request.get("text").unwrap_or(&json!("")),"verification":"SQLite 保存回执中的规范化参数；非原始 MCP wire 输入"}));
                let mut changes = Vec::new();
                if let Some(outcomes) = receipt["result"].get("outcomes") {
                    for outcome in array(outcomes)? {
                        if let Some(applied) = outcome
                            .get("Ingested")
                            .and_then(|v| v.get("Applied"))
                            .filter(|v| super::truthy(v))
                        {
                            let memory_id = get(applied, "memory_id")?;
                            changes.push(json!({"candidate_id":id(memory_id),"commit_status":"committed","source_ids":[sid],"candidate":null,"before":null,"after":null,"memory_ref":null,"receipt_id":key,"committed_memory_id":memory_id,"validation":outcome,"note":"回执证明当时已提交，当前记录可能已变化，不能冒充当时的after版本。"}));
                        }
                    }
                }
                result["runs"].as_array_mut().unwrap().push(json!({"id":format!("receipt:{}",&fingerprint(&json!(key))[..16]),"tool":"memory_remember · 持久回执","status":"completed","session_id":request["session_id"],"source_ids":[sid],"save_status":if changes.is_empty(){"unknown"}else{"committed"},"recall_status":"unknown",
                    "input":{"mcp_text":null,"arguments":null,"receipt_request":request},"spans":[{"id":"receipt","name":"持久保存回执","status":"completed","input":request,"output":get(receipt,"result")?,"llm_call_ids":[]}],
                    "write_changes":changes,"retrieval":{"query":null,"query_status":"unknown","scanned_count":null,"candidates":null,"selected_refs":null},
                    "mcp_response":{"recorded":false,"returned_refs":null,"body":null,"delivery_status":"unknown"},"host":{"input_observability":"unavailable","request":null},
                    "notes":"来自SQLite回执；完整工具输入/响应、召回及模型调用未采集。不能用当前版本还原历史提交。"}));
            }
            result["sources"] = json!(sources.into_values().collect::<Vec<_>>());
        }
    }
    let mut trace_hashes = Vec::new();
    result["live"]["trace_status"] = json!(if trace_dir.is_some() {
        "empty"
    } else {
        "not_enabled"
    });
    // ponytail: any source deletion hides all old diagnostics; add source-level redaction only if history visibility matters.
    if privacy_filtered || !state_available {
        result["live"]["trace_status"] = json!("privacy_filtered");
        result["live"]["observability"] = json!(if state_available {
            "检测到原文删除标记，已隐藏历史 Trace、旧保存回执和日志正文，避免旧快照或模型请求重新显示被删内容；不是没有执行记录。已导出的离线文件不会自动撤销，诊断磁盘文件仍按原审计保留策略保存。"
        } else {
            "当前数据库快照不可读取，无法核验删除状态；已隐藏历史 Trace 和日志正文，不能解释为空库或没有执行记录。"
        });
        for log in result["live"]["logs"].as_array_mut().unwrap() {
            log["lines"] = json!([]);
            log["status"] = json!("privacy_filtered");
        }
    } else if let Some(dir) = trace_dir {
        result["live"]["observability"] =
            json!("执行采集已接入；等待新的工具调用。旧持久回执不能补出历史执行过程。");
        let traces = (|| -> Result<(), String> {
            if !dir.is_dir() {
                return Err("ValueError".into());
            }
            let mut files = std::fs::read_dir(dir)
                .map_err(|e| io_class(&e))?
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|s| s.to_str())
                        .is_some_and(|s| s.starts_with("trace-") && s.ends_with(".jsonl"))
                })
                .collect::<Vec<_>>();
            files.sort();
            let mut total = 0;
            for path in files {
                total += path.metadata().map_err(|e| io_class(&e))?.len();
                if total > 16 * 1024 * 1024 {
                    return Err("ValueError".into());
                }
                let trace = export_runtime(&path, true)?;
                if trace["memory_snapshot"]["scope"] != *owner {
                    continue;
                }
                trace_hashes.push(trace["meta"]["trace_sha256"].clone());
                for field in ["sources", "memories", "runs", "llm_calls"] {
                    let items = result[field].as_array_mut().unwrap();
                    for item in array(&trace[field])? {
                        if let Some(index) = items.iter().position(|v| {
                            v["id"] == item["id"]
                                && (field != "memories" || v["version"] == item["version"])
                        }) {
                            if field != "sources" || items[index]["kind"] != "persisted_source" {
                                items[index] = item.clone()
                            }
                        } else {
                            items.push(item.clone())
                        }
                    }
                }
                for channel in ["source_capture", "lexical_index"] {
                    if trace["meta"]["channels"][channel] == "enabled" {
                        result["meta"]["channels"][channel] = json!("enabled");
                    }
                }
                result["live"]["trace_status"] = json!("available");
                if trace["meta"]["tail_status"] == "partial_line"
                    || array(&trace["runs"])?
                        .iter()
                        .any(|r| r["evidence_status"] == "evidence_incomplete")
                {
                    result["live"]["errors"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!(format!(
                            "evidence_incomplete：{} 含未结束或缺失的阶段",
                            path.file_name().unwrap_or_default().to_string_lossy()
                        )));
                }
            }
            if !trace_hashes.is_empty() {
                result["meta"]["notes"] =
                    json!("当前 SQLite 快照、持久回执及已采集执行 Trace；历史版本按当时记录展示。");
                result["live"]["observability"] = json!(
                    "只读原有 SQLite、日志与运行时 Trace；费用、原生修订及宿主输入仍可能未知。"
                );
            }
            Ok(())
        })();
        if let Err(e) = traces {
            result["live"]["trace_status"] = json!("evidence_incomplete");
            result["live"]["errors"]
                .as_array_mut()
                .unwrap()
                .push(json!(format!(
                    "evidence_incomplete：诊断记录不可完整读取（{e}）"
                )));
        }
    }
    let log_errors = result["live"]["logs"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|l| l["status"] != "available" && l["status"] != "privacy_filtered")
        .map(|l| {
            json!(format!(
                "日志不可读取：{} ({})",
                l["name"].as_str().unwrap(),
                l["error"].as_str().unwrap()
            ))
        })
        .collect::<Vec<_>>();
    result["live"]["errors"]
        .as_array_mut()
        .unwrap()
        .extend(log_errors);
    result["runs"].as_array_mut().unwrap().sort_by(|a, b| {
        b["started_at"]
            .as_str()
            .unwrap_or("")
            .cmp(a["started_at"].as_str().unwrap_or(""))
    });
    if !result["live"]["errors"].as_array().unwrap().is_empty() {
        result["live"]["status"] = json!("partial")
    }
    let mut content = safe(&result);
    content["live"]["revision"] = json!(fingerprint(
        &json!({"snapshot":content["memory_snapshot"]["snapshot_sha256"],"logs":content["live"]["logs"],"errors":content["live"]["errors"],"traces":trace_hashes})
    ));
    Ok(content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_receipts_survive_model_failure_and_hide_deleted_diagnostics() {
        let dir = std::env::temp_dir().join(format!(
            "pmr-s2-inspector-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&dir).unwrap();
        let db = dir.join("memory.sqlite");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE memory_profiles(owner_id TEXT PRIMARY KEY, state_json TEXT NOT NULL)",
        )
        .unwrap();
        let mut state = json!({"repository":{"memories":{}},"staging":{"pending":{}},"receipts":{},"revision":3,"sources":{"turn-key":{"id":"source-one","session_id":"session","turn_id":"turn","observed_at":1,"request":{"text":"delete-this-original","history":[]},"enrichment":"review_incomplete","error":"model incomplete","result":null,"base_revision":0,"received_sequence":1}}});
        let save = |state: &Value| {
            conn.execute(
                "INSERT OR REPLACE INTO memory_profiles VALUES ('owner', ?1)",
                [state.to_string()],
            )
            .unwrap()
        };
        save(&state);
        let mut config = Config {
            db,
            owner: "owner".into(),
            logs: vec![],
            trace_dir: None,
        };
        let mut empty = state.clone();
        empty["sources"] = json!({});
        save(&empty);
        assert_eq!(
            live_bundle(&config).unwrap()["meta"]["channels"]["source_capture"],
            "unknown",
            "no source receipts cannot prove capture is disabled"
        );
        save(&state);
        let bundle = live_bundle(&config).unwrap();
        assert_eq!(bundle["sources"][0]["kind"], "persisted_source");
        assert_eq!(bundle["sources"][0]["text"], "delete-this-original");
        assert_eq!(bundle["runs"][0]["source"]["status"], "stored");
        assert_eq!(bundle["runs"][0]["source"]["received_sequence"], 1);
        assert_eq!(
            bundle["runs"][0]["enrichment"]["status"],
            "review_incomplete"
        );
        assert_eq!(bundle["meta"]["channels"]["source_capture"], "enabled");
        assert_eq!(bundle["memory_snapshot"]["revision"], "3");
        assert!(bundle["runs"][0]["input"].get("receipt_request").is_none());
        state["sources"]["turn-key"]["index"] = json!({"status":"ready","processing_version":"fixture-v1","generation":1,"source_revision":3});
        save(&state);
        let indexed = live_bundle(&config).unwrap();
        assert_eq!(indexed["runs"][0]["index"]["status"], "ready");
        assert_eq!(
            indexed["runs"][0]["index"]["processing_version"],
            "fixture-v1"
        );
        assert_eq!(indexed["meta"]["channels"]["lexical_index"], "enabled");
        let mut events = vec![
            (
                "run_start",
                json!({"owner":"owner","tool":"memory_remember","arguments":{"text":"delete-this-original"}}),
            ),
            (
                "transaction_rollback",
                json!({"before":{"embedded":"delete-this-original"},"after":{"embedded":"delete-this-original"}}),
            ),
            (
                "run_end",
                json!({"evidence_status":"complete","duration_ms":1,"response":{"structuredContent":{}}}),
            ),
        ];
        let raw = events.iter().enumerate().map(|(i,(kind,data))|json!({"trace_version":"1","event_id":format!("e{i}"),"run_id":"old-run","at":"2026-09-22T00:00:00Z","kind":kind,"data":data}).to_string()+"\n").collect::<String>();
        std::fs::write(dir.join("trace-old.jsonl"), raw).unwrap();
        config.trace_dir = Some(dir.clone());
        save(&empty);
        assert_eq!(
            live_bundle(&config).unwrap()["meta"]["channels"]["source_capture"],
            "unknown",
            "legacy traces cannot prove the current capture capability is disabled"
        );
        save(&state);
        let with_trace = live_bundle(&config).unwrap();
        assert_eq!(with_trace["live"]["trace_status"], "available");
        assert_eq!(with_trace["runs"].as_array().unwrap().len(), 2);
        assert_eq!(
            with_trace["meta"]["channels"]["source_capture"], "enabled",
            "old traces cannot override source capability"
        );
        assert_eq!(
            with_trace["meta"]["channels"]["lexical_index"], "enabled",
            "old traces cannot override observed index capability"
        );
        let mut old_state = state.clone();
        old_state["sources"]["turn-key"]["request"]["text"] = json!("older-snapshot-text");
        events.insert(
            1,
            (
                "validation",
                json!({"stage":"initial_snapshot","status":"completed","parsed_result":old_state}),
            ),
        );
        events.insert(2, ("source_receipt", json!({"source":{"status":"stored","source_id":"source-one"},"enrichment":{"status":"pending"}})));
        events.insert(3, ("lexical_raw", json!({"channel":"lexical_raw","query":"delete-this-original","status":"completed","coverage_complete":true,"candidates":[{"source_id":"source-one","text":"delete-this-original"}]})));
        let raw = events.iter().enumerate().map(|(i,(kind,data))|json!({"trace_version":"1","event_id":format!("e{i}"),"run_id":"old-run","at":"2026-09-22T00:00:00Z","kind":kind,"data":data}).to_string()+"\n").collect::<String>();
        std::fs::write(dir.join("trace-old.jsonl"), raw).unwrap();
        let merged = live_bundle(&config).unwrap();
        assert_eq!(
            merged["sources"]
                .as_array()
                .unwrap()
                .iter()
                .find(|source| source["id"] == "source-one")
                .unwrap()["text"],
            "delete-this-original",
            "current persisted sources remain authoritative"
        );
        assert_eq!(
            merged["runs"]
                .as_array()
                .unwrap()
                .iter()
                .find(|run| run["id"] == "old-run")
                .unwrap()["enrichment"]["status"],
            "pending",
            "historical status stays on its own run"
        );
        let mut legacy = state.clone();
        legacy.as_object_mut().unwrap().remove("sources");
        save(&legacy);
        assert_eq!(
            live_bundle(&config).unwrap()["live"]["trace_status"],
            "available",
            "valid v1 snapshots still allow legacy diagnostics"
        );
        state["sources"]["turn-key"]["request"] = Value::Null;
        state["sources"]["turn-key"]["enrichment"] = json!("deleted");
        state["sources"]["turn-key"]["index"] = json!({"status":"deleted"});
        state["sources"]["turn-key"]["error"] = json!("delete-this-original");
        state["receipts"]["old"] = json!({"request":{"text":"delete-this-original"},"result":{"value":"delete-this-original"}});
        state["repository"]["memories"]["1"] =
            json!({"id":1,"owner_id":"owner","status":"Deleted","value":"delete-this-original"});
        save(&state);
        let log = dir.join("runtime.log");
        std::fs::write(&log, "delete-this-original\n").unwrap();
        config.logs.push(log);
        let bundle = live_bundle(&config).unwrap();
        assert!(!bundle.to_string().contains("delete-this-original"));
        assert_eq!(bundle["live"]["trace_status"], "privacy_filtered");
        assert_eq!(bundle["live"]["logs"][0]["status"], "privacy_filtered");
        assert_eq!(bundle["runs"][0]["source"]["status"], "deleted");
        assert_eq!(bundle["runs"][0]["index"]["status"], "deleted");
        assert_eq!(bundle["memories"], json!([]));
        conn.execute("UPDATE memory_profiles SET state_json='invalid-json'", [])
            .unwrap();
        let unreadable = live_bundle(&config).unwrap();
        assert_eq!(unreadable["live"]["status"], "partial");
        assert_eq!(unreadable["live"]["trace_status"], "privacy_filtered");
        assert!(!unreadable.to_string().contains("delete-this-original"));
        drop(conn);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
