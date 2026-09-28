//! 常驻评测集：tests/eval/cases/*.json，离线回放录制的模型输出，按维度汇总通过/失败/跳过。
//!
//! 与 recorded_llm_cases 一样只回放录制的 strict JSON 输出、不访问外网；区别是这里走生产
//! memory_prepare / memory_recall 全链路（原文保存、来源核验、召回、答案契约），不经
//! common::grounded，因此入库忠实度的结论能随运行时修复而变化。回放经本机 loopback 端点
//! 喂给生产 OpenAiClient。运行时请求了未录制的输出 → 该项记为 skipped，不算通过。
//!
//! 任一项 fail 即测试失败（已知失败也不例外）；skip 不影响结果。查看汇总：
//! `cargo test --test eval -- --nocapture`
//!
//! 录制模式：`EVAL_RECORD=<真实请求上限> cargo test --test eval -- --nocapture`。
//! 未录制的请求转发到 DEEPSEEK_BASE_URL 的真实端点，成功的输出按顺序写回用例 JSON 的
//! `recorded`；超出上限或真实请求失败的项仍记为 skipped。未设置时绝不发真实请求。
//!
//! 新录制带 `request` 指纹（去掉时钟与当天日期、review_binding 与 max_output_tokens 后的请求哈希）。
//! 评测使用系统时钟：按录制当天解析相对日期（明天、昨天）的输出，换一天回放会被本地校验拒绝。运行时改变请求后，
//! 指纹不符的录制视为过期：回放记为 skipped，录制模式下重录。无指纹的旧录制仍按 schema 顺序回放。

use hippocampus::chat::{ChatConfig, ChatMemoryService, EnrichmentMode, RetrievalMode};
use hippocampus::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

const OWNER: &str = "eval";

#[derive(Default)]
struct Replay {
    queue: VecDeque<Value>,
    missed: Option<String>,
    /// Every recording used by the current operation, replayed or newly captured, in order.
    served: Vec<Value>,
    captured: bool,
    live: Option<Live>,
}

/// Recording mode only: the real endpoint and the remaining real-request allowance.
struct Live {
    url: String,
    key: String,
    remaining: u64,
    sent: u64,
}

#[derive(Clone, Copy, PartialEq)]
enum Outcome {
    Pass,
    Fail,
    Skip,
}

struct Row {
    dimension: &'static str,
    id: String,
    outcome: Outcome,
    detail: String,
}

#[test]
fn resident_eval_suite() {
    let replay = serve_replay();
    let mut paths: Vec<_> =
        std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/eval/cases"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .collect();
    paths.sort();
    assert!(!paths.is_empty(), "no eval cases");

    let mut rows = Vec::new();
    for path in &paths {
        let mut case: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap())
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        let updates = run_case(&case, &replay, &mut rows);
        if !updates.is_empty() {
            for (list, index, recorded) in updates {
                case[list][index]["recorded"] = Value::Array(recorded);
            }
            let text = serde_json::to_string_pretty(&case).unwrap() + "\n";
            std::fs::write(path, text).unwrap();
            println!("录制已写回 {}", path.display());
        }
    }
    if let Some(live) = &replay.lock().unwrap().live {
        println!(
            "录制模式：真实请求 {} 次，剩余额度 {}",
            live.sent, live.remaining
        );
    }
    let failed = report(paths.len(), &rows);
    assert_eq!(failed, 0, "{failed} 项评测失败，见上方 FAIL 列表");
}

#[test]
fn scoring_rules() {
    let ids = HashMap::from([("s1".to_owned(), json!("a")), ("s2".to_owned(), json!("b"))]);
    let answer = json!({"kind":"answer","answer":["36"],"cite":["s1"],"forbid_cite":["s2"]});
    let contract = |status: &str, text: &str, cited: Value| json!({"status":status,"safe_answer":text,"safe_source_ids":cited});
    let score = |expect: &Value, contract: Value| score_question(expect, &contract, &ids).0;
    assert!(
        score(
            &answer,
            contract("source_linked_fact", "36元。", json!(["a"]))
        ) == Outcome::Pass
    );
    assert!(
        score(
            &answer,
            contract("source_linked_fact", "36元。", json!(["a", "b"]))
        ) == Outcome::Fail
    );
    assert!(
        score(
            &answer,
            contract("source_linked_fact", "58元。", json!(["a"]))
        ) == Outcome::Fail
    );
    assert!(
        score(
            &answer,
            contract("insufficient_evidence", "现有记录证据不足", json!([]))
        ) == Outcome::Fail
    );
    let refuse = json!({"kind":"insufficient_evidence"});
    assert!(
        score(
            &refuse,
            contract("insufficient_evidence", "现有记录证据不足", json!([]))
        ) == Outcome::Pass
    );
    assert!(
        score(
            &refuse,
            contract("clarification_required", "你说的是哪个", json!([]))
        ) == Outcome::Fail
    );
    assert!(
        score(
            &json!({"kind":"clarify"}),
            contract("clarification_required", "你说的是哪个", json!([]))
        ) == Outcome::Pass
    );
    assert!(
        score(
            &json!({"kind":"conflict"}),
            contract("insufficient_evidence", "现有记录证据不足", json!([]))
        ) == Outcome::Fail
    );
    let request = |now: i64, text: &str| {
        json!({"model":"m","max_output_tokens":now,
            "input":json!({"now":now,"current_date":format!("2026-09-{now:02}"),"review_binding":now,"text":text}).to_string()})
    };
    assert_eq!(
        request_fingerprint(&request(1, "a")),
        request_fingerprint(&request(2, "a"))
    );
    assert_ne!(
        request_fingerprint(&request(1, "a")),
        request_fingerprint(&request(1, "b"))
    );
    assert_eq!(scalar(&json!({"Boolean": false})), "false");
    assert_eq!(compact_value(&json!({"Text": "128 GB"})), "128gb");
}

/// Returns newly recorded outputs to write back: (list, index, recorded).
fn run_case(
    case: &Value,
    replay: &Arc<Mutex<Replay>>,
    rows: &mut Vec<Row>,
) -> Vec<(&'static str, usize, Vec<Value>)> {
    let mut updates = Vec::new();
    let case_id = case["id"].as_str().unwrap();
    let db = TempDb(std::env::temp_dir().join(format!(
        "pmr-eval-{case_id}-{}.sqlite3",
        diagnostics::new_id()
    )));
    let messages = case["messages"].as_array().unwrap();
    let questions = case["questions"].as_array().unwrap();
    let mut source_ids = HashMap::new();
    // A missing recording leaves the store in an unknown state; later steps cannot be scored.
    let mut blocked: Option<String> = None;

    for (index, message) in messages.iter().enumerate() {
        let id = message["id"].as_str().unwrap();
        if let Some(reason) = &blocked {
            rows.push(row("intake", case_id, id, Outcome::Skip, reason));
        } else {
            if index == 0 {
                for entity in case["entities"].as_array().unwrap() {
                    call(
                        &db,
                        Phase::Intake,
                        replay,
                        &json!([]),
                        "memory_entity_upsert",
                        entity.clone(),
                    )
                    .0
                    .unwrap();
                }
            }
            let task = message.get("task_id");
            let history: Vec<_> = messages[..index]
                .iter()
                .filter(|earlier| earlier.get("task_id") == task)
                .map(|earlier| earlier["text"].clone())
                .collect();
            let mut turn = json!({"text":message["text"],"session_id":format!("eval-{case_id}"),
                "turn_id":id,"capture_source":true,"history":history});
            if let Some(task) = task {
                turn["task_id"] = task.clone();
            }
            let recorded = message.get("recorded").cloned().unwrap_or(json!([]));
            let (result, captured) = call(
                &db,
                Phase::Intake,
                replay,
                &recorded,
                "memory_prepare",
                json!({"user_turn":turn}),
            );
            updates.extend(captured.map(|recorded| ("messages", index, recorded)));
            match result {
                Err(Missing(schema)) => {
                    rows.push(row(
                        "intake",
                        case_id,
                        id,
                        Outcome::Skip,
                        &format!("缺录制 {schema}"),
                    ));
                    blocked = Some(format!("{id} 缺录制"));
                }
                Ok(result) => {
                    if let Some(source_id) = receipt(&result).pointer("/source/source_id") {
                        source_ids.insert(id.to_owned(), source_id.clone());
                    }
                    let (outcome, detail) = score_intake(message, &result, &db);
                    rows.push(row("intake", case_id, id, outcome, &detail));
                }
            }
        }

        for (question_index, question) in questions
            .iter()
            .enumerate()
            .filter(|(_, q)| q["after"].as_u64() == Some(index as u64 + 1))
        {
            let id = question["id"].as_str().unwrap();
            let dimension = if question["expect"]["kind"] == "answer" {
                "recall"
            } else {
                "refusal"
            };
            if let Some(reason) = &blocked {
                rows.push(row(dimension, case_id, id, Outcome::Skip, reason));
                continue;
            }
            let mut arguments = json!({"query":question["query"],"context":question["context"]});
            if let Some(task) = question.get("task_id") {
                arguments["task_id"] = task.clone();
            }
            let recorded = question.get("recorded").cloned().unwrap_or(json!([]));
            let (result, captured) = call(
                &db,
                Phase::Recall,
                replay,
                &recorded,
                "memory_recall",
                arguments,
            );
            updates.extend(captured.map(|recorded| ("questions", question_index, recorded)));
            let (outcome, detail) = match result {
                Err(Missing(schema)) => (Outcome::Skip, format!("缺录制 {schema}")),
                Ok(result) => {
                    score_question(&question["expect"], &result["answer_contract"], &source_ids)
                }
            };
            rows.push(row(dimension, case_id, id, outcome, &detail));
        }
    }
    updates
}

fn score_intake(message: &Value, result: &Value, db: &TempDb) -> (Outcome, String) {
    let mut problems = Vec::new();
    if receipt(result).pointer("/source/status") != Some(&json!("stored")) {
        problems.push("原文未保存".to_owned());
    }
    let state = serde_json::to_value(db.open().read().unwrap()).unwrap();
    let memories: Vec<&Value> = match &state["repository"]["memories"] {
        Value::Object(map) => map.values().collect(),
        Value::Array(list) => list.iter().collect(),
        _ => Vec::new(),
    };
    let from_message: Vec<_> = memories
        .into_iter()
        .filter(|memory| {
            memory["evidence"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|e| e["turn_id"] == message["id"])
        })
        .collect();
    let matches = |fact: &Value, memory: &Value| {
        memory["subject_id"] == fact["subject"]
            && compact(&scalar(&memory["value"])).contains(&compact_value(&fact["value"]))
    };
    for fact in message["expect"]["store"].as_array().into_iter().flatten() {
        if !from_message
            .iter()
            .any(|m| m["status"] == "Active" && matches(fact, m))
        {
            problems.push(format!("未入库 {}={}", fact["subject"], fact["value"]));
        }
    }
    for fact in message["expect"]["forbid"].as_array().into_iter().flatten() {
        if from_message.iter().any(|m| matches(fact, m)) {
            problems.push(format!(
                "禁止入库却入库 {}={}",
                fact["subject"], fact["value"]
            ));
        }
    }
    if problems.is_empty() {
        return (Outcome::Pass, String::new());
    }
    let committed: Vec<_> = from_message
        .iter()
        .map(|m| {
            format!(
                "{}/{}={} [{}]",
                m["subject_id"].as_str().unwrap_or("?"),
                m["predicate"].as_str().unwrap_or("?"),
                m["value"],
                m["status"].as_str().unwrap_or("?")
            )
        })
        .collect();
    let enrichment = &receipt(result)["enrichment"];
    (
        Outcome::Fail,
        format!(
            "{}；整理 {}/{}；实际入库 {committed:?}",
            problems.join("；"),
            enrichment["status"],
            enrichment["error"]
        ),
    )
}

/// memory_prepare nests a completed receipt under remember.result, a failed one directly in remember.
fn receipt(result: &Value) -> &Value {
    let remember = &result["remember"];
    remember.get("result").unwrap_or(remember)
}

fn score_question(
    expect: &Value,
    contract: &Value,
    source_ids: &HashMap<String, Value>,
) -> (Outcome, String) {
    let status = contract["status"].as_str().unwrap_or("missing");
    let answer = contract["safe_answer"].as_str().unwrap_or("");
    let cited: Vec<&Value> = contract["safe_source_ids"]
        .as_array()
        .into_iter()
        .flatten()
        .collect();
    let refs = |key: &str| -> Vec<(String, Option<&Value>)> {
        expect[key]
            .as_array()
            .into_iter()
            .flatten()
            .map(|r| {
                (
                    r.as_str().unwrap().to_owned(),
                    source_ids.get(r.as_str().unwrap()),
                )
            })
            .collect()
    };
    let mut problems = Vec::new();
    for (name, id) in refs("forbid_cite") {
        if id.is_some_and(|id| cited.contains(&id)) {
            problems.push(format!("引用了禁止来源 {name}"));
        }
    }
    match expect["kind"].as_str().unwrap() {
        "answer" => {
            if cited.is_empty() {
                problems.push("未核准任何来源".to_owned());
            }
            for value in expect["answer"].as_array().unwrap() {
                if !compact(answer).contains(&compact_value(value)) {
                    problems.push(format!("答案缺少 {value}"));
                }
            }
            for (name, id) in refs("cite") {
                if !id.is_some_and(|id| cited.contains(&id)) {
                    problems.push(format!("未引用 {name}"));
                }
            }
        }
        kind => {
            let ok = cited.is_empty()
                && match kind {
                    "clarify" => status == "clarification_required",
                    "insufficient_evidence" => status == "insufficient_evidence",
                    "conflict" => answer.contains("冲突"),
                    other => panic!("unknown expect.kind {other}"),
                };
            if !ok {
                problems.push(format!("期望 {kind}"));
            }
        }
    }
    if problems.is_empty() {
        (Outcome::Pass, String::new())
    } else {
        (
            Outcome::Fail,
            format!(
                "{}；实际 status={status} safe_answer={answer:?}",
                problems.join("；")
            ),
        )
    }
}

fn scalar(value: &Value) -> String {
    // MemoryValue serializes as {"Text": ..} / {"Boolean": ..} etc.; score on the inner scalar.
    match value {
        Value::Object(map) if map.len() == 1 => scalar(map.values().next().unwrap()),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn compact_value(value: &Value) -> String {
    compact(&scalar(value))
}

fn compact(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_lowercase()
}

fn row(dimension: &'static str, case_id: &str, id: &str, outcome: Outcome, detail: &str) -> Row {
    Row {
        dimension,
        id: format!("{case_id}/{id}"),
        outcome,
        detail: detail.to_owned(),
    }
}

/// Prints the summary and returns the number of failed items.
fn report(case_count: usize, rows: &[Row]) -> usize {
    let mut totals: BTreeMap<&str, [usize; 3]> = BTreeMap::new();
    for row in rows {
        totals.entry(row.dimension).or_default()[row.outcome as usize] += 1;
    }
    println!("\n常驻评测集：{case_count} 个用例，{} 项", rows.len());
    for (dimension, label) in [
        ("intake", "入库忠实度"),
        ("recall", "召回"),
        ("refusal", "拒答"),
    ] {
        let [pass, fail, skip] = totals.get(dimension).copied().unwrap_or_default();
        println!("{label}: pass={pass} fail={fail} skip={skip}");
    }
    for row in rows.iter().filter(|row| row.outcome == Outcome::Fail) {
        println!("FAIL [{}] {}: {}", row.dimension, row.id, row.detail);
    }
    for row in rows.iter().filter(|row| row.outcome == Outcome::Skip) {
        println!("SKIP [{}] {}: {}", row.dimension, row.id, row.detail);
    }
    totals.values().map(|[_, fail, _]| fail).sum()
}

// ---- 生产服务调用与录制回放 ----

#[derive(Clone, Copy)]
enum Phase {
    Intake,
    Recall,
}

#[derive(Debug)]
struct Missing(String);

/// Mirrors the S6 runner: a fresh service per operation, S6 phase config, bootstrap first.
/// Also returns the operation's full recording list when recording mode captured new outputs.
fn call(
    db: &TempDb,
    phase: Phase,
    replay: &Arc<Mutex<Replay>>,
    recorded: &Value,
    tool: &str,
    arguments: Value,
) -> (Result<Value, Missing>, Option<Vec<Value>>) {
    // Intake of long USER turns can exceed 32k output tokens (conflict_s2's review,
    // LongMemEval extractions did).
    let (extraction_output_tokens, review_output_tokens) = match phase {
        Phase::Intake => (65_536, 65_536),
        Phase::Recall => (16_384, 16_384),
    };
    let (retrieval, enrichment, per_operation, group) = match phase {
        Phase::Intake => (
            RetrievalMode::Local,
            EnrichmentMode::Inline,
            2,
            "eval-intake",
        ),
        // Assisted recall: soft search queries, then the answer check when local checks fail.
        Phase::Recall => (
            RetrievalMode::Assisted,
            EnrichmentMode::Off,
            2,
            "eval-recall",
        ),
    };
    let config = ChatConfig {
        retrieval_mode: Some(retrieval),
        enrichment_mode: enrichment,
        fact_index_enabled: true,
        model_budget: Some(ModelBudgetConfig {
            group_id: group.into(),
            max_calls: 1_000,
            max_operation_calls: per_operation,
            allow_coverage_repair: false,
            allow_extraction_repair: false,
            max_input_bytes: 65_536,
            extraction_output_tokens,
            review_output_tokens,
            query_output_tokens: 8_192,
        }),
        ..ChatConfig::default()
    };
    {
        let mut replay = replay.lock().unwrap();
        replay.queue = recorded.as_array().unwrap().iter().cloned().collect();
        replay.missed = None;
        replay.served.clear();
        replay.captured = false;
    }
    let mut service = ChatMemoryService::new(db.open(), config).unwrap();
    service
        .call(
            "memory_recall",
            json!({"query":"全局回答要求","bootstrap":true}),
            || false,
        )
        .unwrap();
    let mut result = service.call(tool, arguments.clone(), || false);
    while matches!(&result, Ok(value) if value["status"] == "in_progress") {
        result = service.call(tool, arguments.clone(), || false);
    }
    let mut replay = replay.lock().unwrap();
    // Partial captures are kept: they are valid prefixes for the next replay.
    let captured = replay.captured.then(|| std::mem::take(&mut replay.served));
    if let Some(schema) = replay.missed.take() {
        return (Err(Missing(schema)), captured);
    }
    (
        Ok(result.unwrap_or_else(|error| panic!("{tool} failed: {error}"))),
        captured,
    )
}

/// Loopback Responses endpoint serving the current operation's recordings in order.
fn serve_replay() -> Arc<Mutex<Replay>> {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    // Read the real endpoint before the environment is pointed at the loopback stub.
    let live = std::env::var("EVAL_RECORD").ok().map(|limit| Live {
        url: format!(
            "{}/responses",
            std::env::var("DEEPSEEK_BASE_URL")
                .expect("EVAL_RECORD requires DEEPSEEK_BASE_URL")
                .trim()
                .trim_end_matches('/')
        ),
        key: std::env::var("DEEPSEEK_API_KEY").expect("EVAL_RECORD requires DEEPSEEK_API_KEY"),
        remaining: limit
            .parse()
            .expect("EVAL_RECORD must be the real-request limit"),
        sent: 0,
    });
    // SAFETY: only resident_eval_suite touches the environment; the other test is pure scoring.
    unsafe {
        std::env::set_var("PMR_MODEL_NETWORK", "offline");
        std::env::set_var("DEEPSEEK_BASE_URL", format!("http://{address}/v1"));
        std::env::set_var("DEEPSEEK_API_KEY", "eval-replay");
    }
    let replay = Arc::new(Mutex::new(Replay {
        live,
        ..Replay::default()
    }));
    let http = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()
        .unwrap();
    let shared = replay.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let request: Value = serde_json::from_slice(&body).unwrap_or_default();
            let schema = request
                .pointer("/text/format/name")
                .cloned()
                .unwrap_or_default();
            let schema_name = schema.as_str().unwrap_or("?").to_owned();
            let fingerprint = request_fingerprint(&request);
            let mut replay = shared.lock().unwrap();
            // A recording made for a different request is stale: never replay it.
            let stale = replay.queue.front().is_some_and(|recording| {
                recording["schema"] == schema
                    && recording
                        .get("request")
                        .is_some_and(|recorded| *recorded != fingerprint)
            });
            if stale {
                replay.queue.pop_front();
            }
            let (status, body) = match replay.queue.front() {
                Some(recording) if recording["schema"] == schema => {
                    let recording = replay.queue.pop_front().unwrap();
                    let mut output = recording["output"].clone();
                    // review_binding is a per-run request fingerprint the model is told to copy
                    // verbatim; echo the current one, as the model would. Content is untouched.
                    let input: Value = request["input"]
                        .as_str()
                        .and_then(|input| serde_json::from_str(input).ok())
                        .unwrap_or_default();
                    if output.get("review_binding").is_some()
                        && let Some(binding) = input.get("review_binding")
                    {
                        output["review_binding"] = binding.clone();
                    }
                    let output = match output {
                        Value::String(text) => text,
                        other => other.to_string(),
                    };
                    replay.served.push(recording);
                    let response = json!({"id":"eval-replay","status":"completed",
                        "output":[{"type":"message","content":[{"type":"output_text","text":output}]}],
                        "usage":{"input_tokens":0,"output_tokens":0}});
                    ("200 OK".to_owned(), response.to_string())
                }
                _ if replay.live.as_ref().is_some_and(|live| live.remaining > 0) => {
                    let live = replay.live.as_mut().unwrap();
                    live.remaining -= 1;
                    live.sent += 1;
                    let (url, key) = (live.url.clone(), live.key.clone());
                    drop(replay);
                    let forwarded = http.post(url).bearer_auth(key).json(&request).send();
                    let mut replay = shared.lock().unwrap();
                    match forwarded.and_then(|response| Ok((response.status(), response.text()?))) {
                        Ok((status, body)) => {
                            let envelope = serde_json::from_str::<Value>(&body).unwrap_or_default();
                            let output = status
                                .is_success()
                                .then(|| output_text(&envelope))
                                .flatten();
                            match output {
                                Some(text) => {
                                    let output =
                                        serde_json::from_str(&text).unwrap_or(Value::String(text));
                                    replay.served.push(
                                        json!({"schema":schema,"request":fingerprint,"output":output}),
                                    );
                                    replay.captured = true;
                                }
                                None => {
                                    replay.missed = Some(format!(
                                        "{schema_name}（真实请求未完成 {status} status={} {}）",
                                        envelope["status"], envelope["incomplete_details"]
                                    ))
                                }
                            }
                            (status.to_string(), body)
                        }
                        Err(error) => {
                            replay.missed = Some(format!("{schema_name}（真实请求失败 {error}）"));
                            let body = json!({"error":{"message":error.to_string()}});
                            ("502 Bad Gateway".to_owned(), body.to_string())
                        }
                    }
                }
                _ => {
                    replay.missed = Some(if stale {
                        format!("{schema_name}（录制过期：请求已变化）")
                    } else {
                        schema_name
                    });
                    let body =
                        json!({"error":{"message":format!("eval: no recording for {schema}")}});
                    ("503 Service Unavailable".to_owned(), body.to_string())
                }
            };
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    replay
}

/// Stable request identity for a recording. Per-run values (clock and date, review binding) are removed;
/// recordings without it predate fingerprints and still replay by schema order.
fn request_fingerprint(request: &Value) -> Value {
    fn strip(value: &mut Value) {
        match value {
            Value::Object(map) => {
                for key in ["now", "current_date", "trusted_at", "review_binding", "observed_at"] {
                    map.remove(key);
                }
                map.values_mut().for_each(strip);
            }
            Value::Array(items) => items.iter_mut().for_each(strip),
            _ => {}
        }
    }
    let mut request = request.clone();
    // An output allowance does not change what the model is asked; completed outputs fit it.
    request.as_object_mut().map(|fields| fields.remove("max_output_tokens"));
    if let Some(input) = request["input"].as_str()
        && let Ok(parsed) = serde_json::from_str::<Value>(input)
    {
        request["input"] = parsed;
    }
    strip(&mut request);
    // serde_json maps are sorted, so this serialization is canonical.
    let hash = request.to_string().bytes().fold(0xcbf29ce484222325u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    });
    json!(format!("fnv1a64:{hash:016x}"))
}

/// Completed Responses output text; incomplete or refused responses are not recorded.
fn output_text(envelope: &Value) -> Option<String> {
    if envelope["status"] != "completed" {
        return None;
    }
    let text: String = envelope["output"]
        .as_array()?
        .iter()
        .flat_map(|item| item["content"].as_array().into_iter().flatten())
        .filter(|part| part["type"] == "output_text")
        .filter_map(|part| part["text"].as_str())
        .collect();
    (!text.is_empty()).then_some(text)
}

struct TempDb(std::path::PathBuf);

impl TempDb {
    fn open(&self) -> SqliteMemoryStore {
        SqliteMemoryStore::open(&self.0, UserId::from(OWNER)).unwrap()
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
        }
    }
}
