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
//! `recorded`（附 usage 以校准并发）；429、5xx 与连接错误退避重试，超出上限或重试耗尽的项
//! 仍记为 skipped。未设置时绝不发真实请求。
//!
//! EVAL_CASES=<文件名前缀,…> 只跑匹配的用例。用例并发执行（EVAL_JOBS，默认 8；限额 500 RPM、1,000,000 TPM），同一用例内部串行，
//! 每个用例经自己的 base URL 取自己的录制队列，汇总顺序与串行一致。
//!
//! 新录制带 `request` 指纹（去掉时钟读数、review_binding 与 max_output_tokens 后的请求哈希；
//! 时钟固定，所以当天日期保留在指纹里，按别的日期解析相对日期的录制会被判过期）。
//! 评测使用固定时钟（CLOCK，或用例自带的 observed_at / at），相对日期在任何一天回放都一致。
//!
//! 录制是冻结的测试集：默认宽松回放，代码改动让请求内容与录制时不同，也照样按用例、按 schema
//! 顺序回放冻结的输出，只按用例统计“输入漂移”，所以分数变化只来自代码。漂移多说明模型看到的
//! 内容已实质改变，冻结输出不再代表真实行为，届时按批重录。`EVAL_STRICT=1` 与录制模式按指纹
//! 判过期：回放记为 skipped，录制模式下重录。提示词或 schema 改动必须重录。
//!
//! 录制带 `model` 标识（可读的模型名，接入点 ID 另记为 `endpoint`）：每个模型配置各有一套
//! 冻结录制，并列保存；回放和重录只动当前模型（MODEL）那一套，没有标识的旧录制视为当前模型的。
//! 换模型时同时改 MODEL 与 DEEPSEEK_MODEL，再为它录一套。

use hippocampus::chat::{ChatConfig, ChatMemoryService, EnrichmentMode, RetrievalMode};
use hippocampus::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const OWNER: &str = "eval";
/// The model behind the DEEPSEEK_MODEL endpoint; recordings are labelled with it, so change it
/// together with the endpoint. An endpoint ID alone does not say which model answered.
const MODEL: &str = "DeepSeek-V4.1-Flash";
/// Fixed clock for cases without their own times: 2026-09-28 12:00 UTC+8, a minute per
/// message, so replays resolve relative dates the same way on any day. Cases may set
/// `observed_at` per message and `at` per question (Unix seconds).
const CLOCK: i64 = 1_790_568_000;

#[derive(Default)]
struct Replay {
    /// Loopback address; each case calls `http://{address}/v1/case/{case_id}`.
    address: String,
    cases: HashMap<String, Queue>,
    live: Option<Live>,
    /// EVAL_STRICT or recording: a recording made for a different request is stale.
    strict: bool,
    /// Loose replay: per case, how many frozen outputs answered a changed request.
    drift: BTreeMap<String, usize>,
}

/// One case's recordings for its current operation; a case's operations run serially.
#[derive(Default)]
struct Queue {
    queue: VecDeque<Value>,
    missed: Option<String>,
    /// Every recording used by the current operation, replayed or newly captured, in order.
    served: Vec<Value>,
    captured: bool,
}

/// Recording mode only: the real endpoint, the remaining real-request allowance and usage.
struct Live {
    url: String,
    key: String,
    remaining: u64,
    sent: u64,
    retries: u64,
    input_tokens: u64,
    output_tokens: u64,
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
    // EVAL_CASES=s6_,lme_0862 runs only case files whose names start with one of the prefixes.
    let only = std::env::var("EVAL_CASES").unwrap_or_default();
    let mut paths: Vec<_> =
        std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/eval/cases"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .filter(|path| {
                let name = path.file_name().unwrap().to_string_lossy();
                only.is_empty() || only.split(',').any(|prefix| name.starts_with(prefix.trim()))
            })
            .collect();
    paths.sort();
    assert!(!paths.is_empty(), "no eval cases");

    let jobs: usize = std::env::var("EVAL_JOBS").map_or(8, |jobs| {
        jobs.parse()
            .ok()
            .filter(|&jobs| jobs > 0)
            .expect("EVAL_JOBS must be a positive number")
    });
    let started = Instant::now();
    let next = AtomicUsize::new(0);
    let per_case: Vec<Mutex<Vec<Row>>> = paths.iter().map(|_| Mutex::default()).collect();
    std::thread::scope(|scope| {
        for _ in 0..jobs.min(paths.len()) {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(path) = paths.get(index) else { break };
                    let mut case: Value =
                        serde_json::from_str(&std::fs::read_to_string(path).unwrap())
                            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
                    let mut rows = Vec::new();
                    let updates = run_case(&case, &replay, &mut rows);
                    *per_case[index].lock().unwrap() = rows;
                    if !updates.is_empty() {
                        for (list, index, recorded) in updates {
                            case[list][index]["recorded"] = Value::Array(recorded);
                        }
                        let text = serde_json::to_string_pretty(&case).unwrap() + "\n";
                        std::fs::write(path, text).unwrap();
                        println!("录制已写回 {}", path.display());
                    }
                }
            });
        }
    });
    let rows: Vec<Row> = per_case
        .into_iter()
        .flat_map(|rows| rows.into_inner().unwrap())
        .collect();
    if let Some(live) = &replay.lock().unwrap().live {
        let minutes = started.elapsed().as_secs_f64() / 60.0;
        println!(
            "录制模式：真实请求 {} 次（另有重试 {} 次），剩余额度 {}；usage 输入 {} / 输出 {} token，\
             用时 {minutes:.1} 分钟，约 {:.0} RPM、{:.0} TPM",
            live.sent,
            live.retries,
            live.remaining,
            live.input_tokens,
            live.output_tokens,
            (live.sent + live.retries) as f64 / minutes,
            (live.input_tokens + live.output_tokens) as f64 / minutes,
        );
    }
    let drift = std::mem::take(&mut replay.lock().unwrap().drift);
    if !drift.is_empty() {
        println!(
            "输入漂移：{} 次冻结输出回答了已变化的请求，涉及 {} 个用例 {:?}",
            drift.values().sum::<usize>(),
            drift.len(),
            drift
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
            "input":json!({"now":now,"review_binding":now,"text":text}).to_string()})
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

#[test]
fn forward_retries_rate_limits_and_dropped_connections() {
    // Upstream: drops the first connection, rate-limits the second, answers the third.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/responses", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for (attempt, stream) in listener.incoming().enumerate() {
            let mut stream = stream.unwrap();
            let mut buffer = [0; 4096];
            let _ = stream.read(&mut buffer);
            let (status, extra) = match attempt {
                0 => continue,
                1 => ("429 Too Many Requests", "Retry-After: 0\r\n"),
                _ => ("200 OK", ""),
            };
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\n{extra}Content-Length: 2\r\nConnection: close\r\n\r\nok"
            );
        }
    });
    // No proxy: a system proxy would answer the dropped connection with its own 502.
    let http = reqwest::blocking::Client::builder()
        .no_proxy()
        .build()
        .unwrap();
    let (result, retries) = forward(&http, &url, "k", &json!({}));
    assert_eq!(result.unwrap(), (reqwest::StatusCode::OK, "ok".to_owned()));
    assert_eq!(retries, 2);
}

/// Offline, no model calls: re-validates every recorded extraction and review output under the
/// current code, so a validation change is judged on all real outputs instead of a re-record.
/// Candidates are decoded one by one, so each keeps its own rejection reason. Compare two code
/// versions by diffing the output: `cargo test --test eval validation_corpus -- --nocapture`.
#[test]
fn validation_corpus() {
    let config = ChatConfig::default();
    let owner = UserId::from(OWNER);
    let mut rejected: BTreeMap<String, usize> = BTreeMap::new();
    let mut reviews: BTreeMap<String, usize> = BTreeMap::new();
    let (mut kept_total, mut candidate_total) = (0, 0);
    let mut messages: BTreeMap<&str, usize> = BTreeMap::new();
    let mut details = Vec::new();
    // What accepted candidates would store, not just whether they pass.
    let mut stored: BTreeMap<&str, usize> = BTreeMap::new();
    let mut paths: Vec<_> =
        std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/eval/cases"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .collect();
    paths.sort();
    // Variable parts (line/column, unknown ids) would split one cause into many rows.
    let reason = |error: &dyn std::fmt::Display| {
        let text = error.to_string();
        let text = text.split(" at line ").next().unwrap_or(&text).to_owned();
        match text.split_once("unknown companion entity id ") {
            Some((head, _)) => format!("{head}unknown companion entity id …"),
            None => text,
        }
    };
    for path in &paths {
        let case: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let case_id = case["id"].as_str().unwrap();
        let mut entities: Vec<Entity> = case["entities"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entity| {
                let mut entity = entity.clone();
                entity["owner_id"] = json!(OWNER);
                // memory_entity_upsert's input form: a bare "Other" is Other("Other").
                if entity["entity_type"] == "Other" {
                    entity["entity_type"] = json!({"Other":"Other"});
                }
                serde_json::from_value(entity).unwrap()
            })
            .collect();
        // ChatMemoryService registers the user as "self" before any turn.
        entities.push(Entity {
            id: EntityId::from("self"),
            owner_id: owner.clone(),
            entity_type: EntityType::Person,
            canonical_name: "用户本人".into(),
            aliases: vec![
                "我".into(),
                "自己".into(),
                "本人".into(),
                "I".into(),
                "me".into(),
            ],
        });
        let all = case["messages"].as_array().unwrap();
        for (index, message) in all.iter().enumerate() {
            let recorded = message["recorded"].as_array().cloned().unwrap_or_default();
            let Some(extraction) = recorded
                .iter()
                .find(|r| r["schema"] == "personal_memory_candidates_v1")
            else {
                continue;
            };
            let id = message["id"].as_str().unwrap();
            let text = message["text"].as_str().unwrap();
            let task = message.get("task_id");
            let history: Vec<&str> = all[..index]
                .iter()
                .filter(|earlier| earlier.get("task_id") == task)
                .filter_map(|earlier| earlier["text"].as_str())
                .collect();
            let now = message["observed_at"]
                .as_i64()
                .unwrap_or(CLOCK + 60 * index as i64);
            let mut context =
                OpenAiAdapterContext::new(owner.clone(), entities.clone(), Timestamp(now))
                    .unwrap()
                    .with_utc_offset_seconds(config.utc_offset_seconds)
                    .unwrap()
                    .with_semantic_catalog(config.predicates.clone(), config.domains.clone())
                    .unwrap()
                    .with_predicate_value_kinds(config.value_kinds.clone())
                    .unwrap()
                    .with_predicate_memory_types(config.memory_types.clone())
                    .unwrap();
            if let Some(task) = task.and_then(Value::as_str) {
                context = context.with_task_id(TaskId(task.to_owned())).unwrap();
            }
            let decode = |output: &str| {
                context.decode_extraction(output, text, &SessionId::from("s"), &TurnId::from(id))
            };
            let output = match &extraction["output"] {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            };
            let raw: Value = match serde_json::from_str(&output) {
                Ok(raw) => raw,
                Err(error) => {
                    *rejected
                        .entry(format!("输出不是 JSON：{}", reason(&error)))
                        .or_default() += 1;
                    *messages.entry("输出无法解析").or_default() += 1;
                    details.push(format!("DETAIL {case_id}/{id}: 输出无法解析"));
                    continue;
                }
            };
            let raw_candidates = raw["candidates"].as_array().cloned().unwrap_or_default();
            // Whole-output checks (omission_reason, wire schema) apply before any candidate.
            if let Err(error) = decode(&output)
                && !raw_candidates.is_empty()
                && raw_candidates.iter().all(|candidate| {
                    decode(&json!({"omission_reason":null,"candidates":[candidate]}).to_string())
                        .is_ok()
                })
            {
                *rejected
                    .entry(format!("整条输出：{}", reason(&error)))
                    .or_default() += 1;
            }
            let mut kept = Vec::new();
            for candidate in &raw_candidates {
                candidate_total += 1;
                match decode(&json!({"omission_reason":null,"candidates":[candidate]}).to_string())
                {
                    Ok(mut one) => kept.append(&mut one),
                    Err(error) => *rejected.entry(reason(&error)).or_default() += 1,
                }
            }
            kept_total += kept.len();
            for candidate in &kept {
                for (label, present) in [
                    ("保留期限", candidate.temporal.expires_at.is_some()),
                    (
                        "期限待核标记",
                        candidate
                            .tags
                            .iter()
                            .any(|t| t == "retention_deadline_unverified"),
                    ),
                    ("要求记住", candidate.explicit_memory_request),
                    (
                        "事实起止时间",
                        candidate.temporal.valid_from.is_some()
                            || candidate.temporal.valid_to.is_some(),
                    ),
                    ("未解析的时间原文", candidate.temporal.reference.is_some()),
                ] {
                    *stored.entry(label).or_default() += usize::from(present);
                }
            }
            let verdict = if raw_candidates.is_empty() {
                "无候选"
            } else if kept.is_empty() {
                "候选全部被拒"
            } else {
                "有候选通过"
            };
            *messages.entry(verdict).or_default() += 1;
            let review = recorded
                .iter()
                .find(|r| r["schema"] == "personal_memory_source_support_v1");
            let review_verdict = match review {
                None => "无审核录制".to_owned(),
                Some(review)
                    if review["output"]["reviews"].as_array().map(Vec::len) != Some(kept.len()) =>
                {
                    "候选集已变化，审核录制不适用".to_owned()
                }
                Some(review) => {
                    let mut output = review["output"].clone();
                    output["review_binding"] =
                        json!(context.source_support_binding(&kept, text, &history));
                    match context.verify_source_support(
                        kept.clone(),
                        text,
                        &history,
                        &output.to_string(),
                    ) {
                        Ok(verified) => format!(
                            "通过（{}/{} 个候选获得支持）",
                            verified.iter().filter(|c| c.source_support_valid()).count(),
                            verified.len()
                        ),
                        Err(OpenAiAdapterError::SourceCoverageIncomplete) => {
                            "覆盖缺口（生产会保留已获支持的候选）".to_owned()
                        }
                        Err(error) => format!("被拒：{}", reason(&error)),
                    }
                }
            };
            let bucket = review_verdict.split('（').next().unwrap().to_owned();
            *reviews.entry(bucket).or_default() += 1;
            let times: Vec<String> = kept
                .iter()
                .map(|c| {
                    format!(
                        "{}{}{}",
                        if c.temporal.expires_at.is_some() {
                            "期限"
                        } else {
                            ""
                        },
                        if c.explicit_memory_request {
                            "记住"
                        } else {
                            ""
                        },
                        if c.temporal.valid_from.is_some() || c.temporal.reference.is_some() {
                            "时间"
                        } else {
                            ""
                        }
                    )
                })
                .collect();
            details.push(format!(
                "DETAIL {case_id}/{id}: 候选 {}/{} 通过 {times:?}；审核 {review_verdict}",
                kept.len(),
                raw_candidates.len()
            ));
        }
    }
    println!(
        "\n校验语料：{} 个用例，提取候选 {candidate_total} 个，通过 {kept_total} 个",
        paths.len()
    );
    println!("消息：{messages:?}");
    println!("审核：{reviews:?}");
    println!("通过的候选带有：{stored:?}");
    let mut rejected: Vec<_> = rejected.into_iter().collect();
    rejected.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    for (reason, count) in rejected {
        println!("REJECT {count:>4}  {reason}");
    }
    for detail in details {
        println!("{detail}");
    }
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
                        case_id,
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
            let observed_at = message["observed_at"]
                .as_i64()
                .unwrap_or(CLOCK + 60 * index as i64);
            let mut turn = json!({"text":message["text"],"session_id":format!("eval-{case_id}"),
                "turn_id":id,"capture_source":true,"history":history,"observed_at":observed_at});
            if let Some(task) = task {
                turn["task_id"] = task.clone();
            }
            let recorded = message.get("recorded").cloned().unwrap_or(json!([]));
            let (result, captured) = call(
                &db,
                Phase::Intake,
                replay,
                case_id,
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
            let at = question["at"]
                .as_i64()
                .unwrap_or(CLOCK + 60 * index as i64 + 30);
            let mut arguments =
                json!({"query":question["query"],"context":question["context"],"at":at});
            if let Some(task) = question.get("task_id") {
                arguments["task_id"] = task.clone();
            }
            let recorded = question.get("recorded").cloned().unwrap_or(json!([]));
            let (result, captured) = call(
                &db,
                Phase::Recall,
                replay,
                case_id,
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
    case_id: &str,
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
        model_base_url: Some(format!(
            "http://{}/v1/case/{case_id}",
            replay.lock().unwrap().address
        )),
        ..ChatConfig::default()
    };
    // Each model configuration keeps its own frozen set side by side; only the current model's
    // recordings are replayed. Unlabelled recordings predate the label and belong to it.
    let (mine, others): (Vec<Value>, Vec<Value>) =
        recorded.as_array().unwrap().iter().cloned().partition(|recording| {
            recording
                .get("model")
                .is_none_or(|model| model == MODEL)
        });
    replay.lock().unwrap().cases.insert(
        case_id.to_owned(),
        Queue {
            queue: mine.into_iter().collect(),
            ..Queue::default()
        },
    );
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
    let queue = replay.lock().unwrap().cases.remove(case_id).unwrap();
    // Partial captures are kept: they are valid prefixes for the next replay.
    let captured = queue.captured.then(|| {
        let mut served = queue.served;
        for recording in &mut served {
            recording["model"] = json!(MODEL);
        }
        others.into_iter().chain(served).collect()
    });
    if let Some(schema) = queue.missed {
        return (Err(Missing(schema)), captured);
    }
    (
        Ok(result.unwrap_or_else(|error| panic!("{tool} failed: {error}"))),
        captured,
    )
}

/// Loopback Responses endpoint serving each case's recordings in order, one thread per request.
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
        retries: 0,
        input_tokens: 0,
        output_tokens: 0,
    });
    // SAFETY: only resident_eval_suite touches the environment, before any other thread starts;
    // the other test is pure scoring.
    unsafe {
        std::env::set_var("PMR_MODEL_NETWORK", "offline");
        std::env::set_var("DEEPSEEK_BASE_URL", format!("http://{address}/v1"));
        std::env::set_var("DEEPSEEK_API_KEY", "eval-replay");
    }
    let replay = Arc::new(Mutex::new(Replay {
        address: address.to_string(),
        // Recording must never keep an output made for another request.
        strict: live.is_some() || std::env::var_os("EVAL_STRICT").is_some(),
        live,
        ..Replay::default()
    }));
    let http = reqwest::blocking::Client::new();
    let shared = replay.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let (shared, http) = (shared.clone(), http.clone());
            std::thread::spawn(move || handle(stream.unwrap(), &shared, &http));
        }
    });
    replay
}

fn handle(mut stream: TcpStream, shared: &Mutex<Replay>, http: &reqwest::blocking::Client) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request_line = String::new();
    reader.read_line(&mut request_line).unwrap();
    // "POST /v1/case/{case_id}/responses HTTP/1.1"
    let case = request_line
        .split(' ')
        .nth(1)
        .and_then(|path| path.split('/').nth(3))
        .unwrap_or_default()
        .to_owned();
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
    let mut guard = shared.lock().unwrap();
    let Replay {
        cases,
        live,
        strict,
        drift,
        ..
    } = &mut *guard;
    let replay = cases.entry(case.clone()).or_default();
    let changed = replay.queue.front().is_some_and(|recording| {
        recording["schema"] == schema
            && recording
                .get("request")
                .is_some_and(|recorded| *recorded != fingerprint)
    });
    // Strict: a recording made for a different request is stale, never replayed. Loose (the
    // default): the frozen output is replayed anyway and the drift is counted.
    let stale = changed && *strict;
    if stale {
        replay.queue.pop_front();
    } else if changed {
        *drift.entry(case.clone()).or_default() += 1;
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
        _ if live.as_ref().is_some_and(|live| live.remaining > 0) => {
            let live = live.as_mut().unwrap();
            live.remaining -= 1;
            live.sent += 1;
            let (url, key) = (live.url.clone(), live.key.clone());
            drop(guard);
            let (forwarded, retries) = forward(http, &url, &key, &request);
            let mut guard = shared.lock().unwrap();
            let Replay { cases, live, .. } = &mut *guard;
            let live = live.as_mut().unwrap();
            live.retries += retries;
            let replay = cases.entry(case).or_default();
            match forwarded {
                Ok((status, body)) => {
                    let envelope = serde_json::from_str::<Value>(&body).unwrap_or_default();
                    let usage = &envelope["usage"];
                    live.input_tokens += usage["input_tokens"].as_u64().unwrap_or(0);
                    live.output_tokens += usage["output_tokens"].as_u64().unwrap_or(0);
                    let output = status
                        .is_success()
                        .then(|| output_text(&envelope))
                        .flatten();
                    match output {
                        Some(text) => {
                            let output = serde_json::from_str(&text).unwrap_or(Value::String(text));
                            replay
                                .served
                                .push(json!({"schema":schema,"model":MODEL,
                                "endpoint":request["model"],"request":fingerprint,
                                "output":output,"usage":usage}));
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
                    replay.missed = Some(format!(
                        "{schema_name}（真实请求失败，重试 {retries} 次后仍 {error}）"
                    ));
                    let body = json!({"error":{"message":error}});
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
            let body = json!({"error":{"message":format!("eval: no recording for {schema}")}});
            ("503 Service Unavailable".to_owned(), body.to_string())
        }
    };
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

/// Sends one recording request, backing off on 429, 5xx and connection errors. The production
/// client gives the loopback reply PMR_MODEL_TIMEOUT_SECS (600 s), so retrying stops 10 s
/// earlier. Returns the
/// final status and body (or error) and the number of retries.
fn forward(
    http: &reqwest::blocking::Client,
    url: &str,
    key: &str,
    request: &Value,
) -> (Result<(reqwest::StatusCode, String), String>, u64) {
    let timeout: u64 = std::env::var("PMR_MODEL_TIMEOUT_SECS")
        .ok()
        .and_then(|secs| secs.parse().ok())
        .unwrap_or(600);
    let deadline = Instant::now() + Duration::from_secs(timeout.saturating_sub(10));
    let mut retries = 0;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let attempt = http
            .post(url)
            .bearer_auth(key)
            .timeout(left)
            .json(request)
            .send()
            .and_then(|response| {
                let status = response.status();
                let retry_after = response
                    .headers()
                    .get("retry-after")
                    .and_then(|value| value.to_str().ok()?.parse().ok());
                Ok((status, response.text()?, retry_after))
            });
        let (result, retry_after) = match attempt {
            Ok((status, body, retry_after)) => {
                let retry = status.as_u16() == 429 || status.is_server_error();
                (Ok((status, body)), retry.then_some(retry_after))
            }
            Err(error) if error.is_timeout() => (Err(format!("超时 {error}")), Some(None)),
            Err(error) => (Err(error.to_string()), Some(None)),
        };
        let Some(retry_after) = retry_after else {
            return (result, retries);
        };
        let wait = Duration::from_secs(retry_after.unwrap_or(2u64.pow(retries as u32 + 1).min(30)));
        // Leave the next attempt time to answer; otherwise report the last failure.
        if wait + Duration::from_secs(30) > deadline.saturating_duration_since(Instant::now()) {
            return (result, retries);
        }
        std::thread::sleep(wait);
        retries += 1;
    }
}

/// Stable request identity for a recording. Per-run values (clock readings, review binding) are
/// removed; the eval clock is fixed, so the request date stays in and pins relative dates;
/// recordings without it predate fingerprints and still replay by schema order.
fn request_fingerprint(request: &Value) -> Value {
    fn strip(value: &mut Value) {
        match value {
            Value::Object(map) => {
                for key in ["now", "trusted_at", "review_binding", "observed_at"] {
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
