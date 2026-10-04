use super::*;
use crate::*;
use std::collections::{BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const OWNER: &str = "recall-provider-owner";
const TEXT: &str = "我的灯是紫色。";
const AT: i64 = 1_791_000_000;

#[test]
fn role_reasoning_is_sent_and_bound_and_changed_effort_cannot_replay() {
    let db = Db::new();
    let models: RecallModels =
        serde_json::from_value(json!({"reasoning_effort":{"r1":"low","r4":"high"}})).unwrap();
    models.validate().unwrap();
    assert_eq!(models.r1, DEEPSEEK_MODEL);
    assert_eq!(
        RecallModels::default().effort("r1"),
        ReasoningEffort::Medium
    );
    let invalid: RecallModels =
        serde_json::from_value(json!({"reasoning_effort":{"r3":"low"}})).unwrap();
    assert!(invalid.validate().is_err());
    assert!(
        serde_json::from_value::<RecallModels>(json!({"reasoning_effort":{"r1":"unknown"}}))
            .is_err()
    );
    let (mut p, sent) = mocked(&db, vec![envelope(r1_out("judge")), envelope(r4_out())]);
    p.recall_models = models;
    let rt = RecallRuntime::default();
    let req = request("effort-identity");
    let output = rt
        .run(&mut db.open(), &req, Budget::default(), Some(&p))
        .unwrap();
    assert_eq!(sent.lock().unwrap().len(), 2);
    assert_eq!(sent.lock().unwrap()[0]["reasoning"]["effort"], "low");
    assert_eq!(sent.lock().unwrap()[1]["reasoning"]["effort"], "high");
    let state = db.open().read().unwrap();
    let receipt = state
        .receipts
        .values()
        .find(|r| r.request["form"] == "recall_metadata_v1")
        .unwrap();
    assert_eq!(
        receipt.request["models"]["reasoning_effort"],
        json!({"r1":"low","r4":"high"})
    );
    assert_eq!(
        rt.run(&mut db.open(), &req, Budget::default(), Some(&p))
            .unwrap(),
        output
    );
    p.recall_models
        .reasoning_effort
        .insert("r1".into(), ReasoningEffort::Medium);
    assert!(
        rt.run(&mut db.open(), &req, Budget::default(), Some(&p))
            .unwrap_err()
            .to_string()
            .contains("recall_request_mismatch")
    );
    assert_eq!(sent.lock().unwrap().len(), 2);
    assert!(
        RecallRuntime::default()
            .run(&mut db.open(), &req, Budget::default(), Some(&p))
            .is_err()
    );
    assert_eq!(db.calls(), 2);
}
struct Db(PathBuf);
impl Db {
    fn new() -> Self {
        let db = Self(
            std::env::temp_dir().join(format!("recall-model-{}.sqlite3", diagnostics::new_id())),
        );
        db.open()
            .transaction(|s| {
                s.entities.upsert(Entity {
                    id: "self".into(),
                    owner_id: OWNER.into(),
                    entity_type: EntityType::Person,
                    canonical_name: "测试主体".into(),
                    aliases: vec![],
                });
                s.sources.insert(
                    "s-purple".into(),
                    SourceRecord {
                        id: "s-purple".into(),
                        session_id: "fixture".into(),
                        turn_id: "fixture".into(),
                        observed_at: AT,
                        received_sequence: 1,
                        request: Some(json!({"text":TEXT,"history":[],"capture_source":true})),
                        enrichment: "failed".into(),
                        error: None,
                        result: None,
                        base_revision: 0,
                        index: Value::Null,
                        task: Default::default(),
                    },
                );
                Ok(())
            })
            .unwrap();
        db
    }
    fn open(&self) -> SqliteMemoryStore {
        SqliteMemoryStore::open(&self.0, OWNER.into()).unwrap()
    }
    fn calls(&self) -> i64 {
        self.open()
            .connection
            .query_row("SELECT count(*) FROM model_calls", [], |r| r.get(0))
            .unwrap()
    }
}
impl Drop for Db {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
fn request(id: &str) -> Request {
    Request {
        binding: RecallRequest {
            request_id: id.into(),
            query: "请核对我的灯色，临时标记 Q9。".into(),
            context: vec!["当前查询不授权保存。".into()],
            task_id: None,
            at: AT,
        },
        owner: OWNER.into(),
        subject_ids: None,
        said_on: "2026-10-03".into(),
        utc_offset_seconds: 28_800,
        bootstrap: false,
    }
}
fn r1_out(need: &str) -> Value {
    json!({"need":need,"current_evidence":"query_only","type":"single","subject":{"entity_id":"self"},
        "relation":"灯色","conditions":[],"time":null,"plan":{"op":"lookup","filter":{"relation":"fact","time":null},"unit":null}})
}
fn fact() -> Value {
    json!({"statement":TEXT,"evidence":[{"source":{"kind":"stored","source_id":"s-purple"},"quote":{"text":TEXT}}],
        "record_ids":[],"polarity":"affirm","strength":null,"uncertainty":null,"conditions":[],"times":[],
        "current":"yes","applies":"yes","supersedes_record_ids":[]})
}
fn r4_out() -> Value {
    json!({"verdict":"supported","facts":[fact()],"compute":null})
}
fn envelope(output: Value) -> Result<(u16, String), OpenAiAdapterError> {
    Ok((200,json!({"status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":output.to_string()}]}],
        "usage":{"input_tokens":21,"output_tokens":34}}).to_string()))
}
fn client(db: &Db) -> OpenAiClient {
    let mut c = OpenAiClient::new("http://127.0.0.1:1/v1", "test-key")
        .unwrap()
        .with_budget(
            &db.0,
            OWNER,
            ModelBudgetConfig {
                group_id: "recall-test".into(),
                max_calls: 20,
                ..Default::default()
            },
            "unused",
        )
        .unwrap();
    c.network_allowed = true;
    c
}
fn provider(c: OpenAiClient, db: &Db) -> RecallModelProvider {
    RecallModelProvider::new(
        c,
        OpenAiAdapterContext::new(
            OWNER.into(),
            db.open()
                .read()
                .unwrap()
                .entities
                .list_by_owner(&OWNER.into()),
            Timestamp(AT),
        )
        .unwrap(),
    )
    .with_models(RecallModels {
        r1: "r1-route-sentinel".into(),
        r4: "r4-route-sentinel".into(),
        ..Default::default()
    })
    .unwrap()
}
fn mocked(
    db: &Db,
    responses: Vec<Result<(u16, String), OpenAiAdapterError>>,
) -> (RecallModelProvider, Arc<Mutex<Vec<Value>>>) {
    let requests = Arc::new(Mutex::new(vec![]));
    let sent = requests.clone();
    let responses = Arc::new(Mutex::new(VecDeque::from(responses)));
    let mut c = client(db);
    c.transport = Some(Arc::new(move |request| {
        sent.lock()
            .unwrap()
            .push(serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap());
        responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected retry/send")
    }));
    (provider(c, db), requests)
}
fn run(
    rt: &mut RecallRuntime,
    db: &Db,
    id: &str,
    p: Option<&RecallModelProvider>,
) -> PersistenceResult<RecallOutput> {
    rt.run(&mut db.open(), &request(id), Budget::default(), p)
}
#[test]
fn same_flow_sends_each_role_once_and_replays_without_retaining_readonly_text() {
    let db = Db::new();
    let (p, sent) = mocked(&db, vec![envelope(r1_out("judge")), envelope(r4_out())]);
    let mut rt = RecallRuntime::default();
    let first = run(&mut rt, &db, "same", Some(&p)).unwrap();
    assert_eq!(first.status, RecallStatus::Answered);
    assert_eq!(first.facts[0].grade, RecallGrade::Uncommitted);
    assert_eq!(run(&mut rt, &db, "same", Some(&p)).unwrap(), first);
    assert_eq!(sent.lock().unwrap().len(), 2);
    assert_eq!(db.calls(), 2);
    let requests = sent.lock().unwrap();
    for (i, model) in ["r1-route-sentinel", "r4-route-sentinel"]
        .into_iter()
        .enumerate()
    {
        assert_eq!(requests[i]["model"], model);
        let input = requests[i]["input"].as_str().unwrap();
        assert!(!input.contains(model));
        assert!(
            !requests[i]["instructions"]
                .as_str()
                .unwrap()
                .contains(model)
        );
    }
    let state = db.open().read().unwrap();
    let cache = serde_json::to_string(&state.receipts).unwrap();
    assert!(!cache.contains(&request("same").binding.query));
    assert!(!cache.contains("Q9"));
    assert!(!cache.contains("当前查询不授权保存"));
    assert!(!cache.contains(TEXT));
    assert!(cache.contains("r1-route-sentinel") && cache.contains("sha256:"));
    assert_eq!(state.sources.len(), 1);
}

#[test]
fn raw_final_audit_replays_together_and_is_not_retained_after_reopen_or_deletion() {
    let db = Db::new();
    let (p, sent) = mocked(&db, vec![envelope(r1_out("judge")), envelope(r4_out())]);
    let rt = RecallRuntime::default();
    let req = request("audit");
    let first = rt
        .run_with_audit(&mut db.open(), &req, Budget::default(), Some(&p))
        .unwrap();
    assert_eq!(first.0.status, RecallStatus::Answered);
    let audit = first.1.as_ref().unwrap();
    assert_eq!(audit.raw_output, r4_out().to_string());
    assert_eq!(
        serde_json::to_value(audit.final_output.as_ref().unwrap()).unwrap(),
        r4_out()
    );
    assert_eq!(
        rt.run_with_audit(&mut db.open(), &req, Budget::default(), Some(&p))
            .unwrap(),
        first
    );
    assert_eq!(sent.lock().unwrap().len(), 2);
    assert_eq!(db.calls(), 2);
    let receipts = serde_json::to_string(&db.open().read().unwrap().receipts).unwrap();
    assert!(
        !receipts.contains(TEXT)
            && !receipts.contains("raw_output")
            && !receipts.contains("final_output")
    );
    assert!(
        RecallRuntime::default()
            .run_with_audit(&mut db.open(), &req, Budget::default(), Some(&p))
            .unwrap_err()
            .to_string()
            .contains("result_not_retained:no_model_retry")
    );
    db.open()
        .transaction(|s| crate::forget::forget(s, &OWNER.into(), None, None, Some("s-purple")))
        .unwrap();
    assert!(
        rt.run_with_audit(&mut db.open(), &req, Budget::default(), Some(&p))
            .unwrap_err()
            .to_string()
            .contains("snapshot_stale")
    );
    assert!(rt.cache.lock().unwrap().is_empty());
    assert_eq!(sent.lock().unwrap().len(), 2);
    assert_eq!(db.calls(), 2);
}
#[test]
fn context_only_no_evidence_offline_and_bootstrap_have_exact_call_counts() {
    for (need, raw, offline, bootstrap, expected, reason) in [
        ("context_only", true, false, false, 1, "context_only"),
        ("judge", false, false, false, 1, "no_evidence"),
        ("judge", true, true, false, 0, "offline"),
        ("judge", true, false, true, 0, "offline"),
    ] {
        let db = Db::new();
        if !raw {
            db.open()
                .transaction(|s| {
                    let source = s.sources.get_mut("s-purple").unwrap();
                    source.request = None;
                    source.result = None;
                    source.enrichment = "deleted".into();
                    Ok(())
                })
                .unwrap();
        }
        let (mut p, sent) = mocked(&db, vec![envelope(r1_out(need))]);
        p.client.network_allowed = !offline;
        let mut req = request("counts");
        req.bootstrap = bootstrap;
        let out = RecallRuntime::default()
            .run(&mut db.open(), &req, Budget::default(), Some(&p))
            .unwrap();
        assert_eq!(sent.lock().unwrap().len(), expected);
        assert_eq!(db.calls(), expected as i64);
        assert!(
            out.degraded_reasons
                .contains(&format!("semantic_judgment_skipped:{reason}"))
        );
    }
}
#[test]
fn timeout_http_and_invalid_output_consume_attempts_without_retry() {
    for error in [
        Err(OpenAiAdapterError::Http("timed out".into())),
        Ok((503, "{}".into())),
        envelope(json!({"verdict":"supported","facts":[{"invented":"invalid"}],"compute":null})),
    ] {
        let db = Db::new();
        let (p, sent) = mocked(&db, vec![envelope(r1_out("judge")), error]);
        let mut rt = RecallRuntime::default();
        let out = run(&mut rt, &db, "failure", Some(&p)).unwrap();
        assert_eq!(out.status, RecallStatus::Degraded);
        assert_eq!(out.semantic_judgment, SemanticJudgment::Failed);
        assert!(out.facts.is_empty());
        run(&mut rt, &db, "failure", Some(&p)).unwrap();
        assert_eq!(sent.lock().unwrap().len(), 2);
        assert_eq!(db.calls(), 2);
        let reopened = run(&mut RecallRuntime::default(), &db, "failure", Some(&p)).unwrap_err();
        assert!(reopened.to_string().contains("result_not_retained"));
        assert_eq!(db.calls(), 2);
    }
}
#[test]
fn failed_r1_falls_back_to_broad_evidence_and_still_uses_one_r4() {
    let db = Db::new();
    let (p, sent) = mocked(
        &db,
        vec![envelope(json!({"bogus":true})), envelope(r4_out())],
    );
    let out = run(&mut RecallRuntime::default(), &db, "bad-r1", Some(&p)).unwrap();
    assert!(out.degraded_reasons.contains(&"r1_not_understood".into()));
    assert!(
        out.degraded_reasons
            .contains(&"model_invalid_output".into())
    );
    assert_eq!(sent.lock().unwrap().len(), 2);
    assert_eq!(db.calls(), 2);
}
#[test]
fn exhausted_total_budget_skips_r4_after_the_one_charged_r1() {
    let db = Db::new();
    let (p, sent) = mocked(&db, vec![envelope(r1_out("judge"))]);
    let mut p = p;
    p.client.budget.as_mut().unwrap().config.max_calls = 1;
    let out = run(&mut RecallRuntime::default(), &db, "limit", Some(&p)).unwrap();
    assert_eq!(db.calls(), 1);
    assert_eq!(sent.lock().unwrap().len(), 1);
    assert!(out.degraded_reasons.contains(&"budget_exhausted".into()));
    assert!(
        out.degraded_reasons
            .contains(&"semantic_judgment_skipped:budget".into())
    );
}
#[test]
fn same_id_rejects_changed_query_context_clock_scope_and_budget() {
    let db = Db::new();
    let mut rt = RecallRuntime::default();
    run(&mut rt, &db, "identity", None).unwrap();
    for change in 0..7 {
        let mut req = request("identity");
        let mut b = Budget::default();
        match change {
            0 => req.binding.query.push('!'),
            1 => req.binding.context.clear(),
            2 => req.binding.at += 1,
            3 => req.binding.task_id = Some("t".into()),
            4 => req.subject_ids = Some(vec![]),
            5 => req.utc_offset_seconds = 0,
            _ => b.raw = 1,
        }
        assert!(
            rt.run(&mut db.open(), &req, b, None)
                .unwrap_err()
                .to_string()
                .contains("request_mismatch")
        );
    }
    assert_eq!(db.calls(), 0);
}
#[test]
fn replay_checks_source_control_metadata_even_when_revisions_did_not_change() {
    let db = Db::new();
    let mut rt = RecallRuntime::default();
    run(&mut rt, &db, "stale", None).unwrap();
    let before = db.open().read().unwrap().source_revision;
    db.open()
        .transaction(|s| {
            s.sources.get_mut("s-purple").unwrap().enrichment = "needs_rejudge".into();
            Ok(())
        })
        .unwrap();
    assert_eq!(db.open().read().unwrap().source_revision, before);
    assert!(
        run(&mut rt, &db, "stale", None)
            .unwrap_err()
            .to_string()
            .contains("snapshot_stale")
    );
    assert_eq!(db.calls(), 0);
}
#[test]
fn deletion_invalidates_referenced_cache_and_removes_whitespace_variants() {
    let db = Db::new();
    let (p, _) = mocked(&db, vec![envelope(r1_out("judge")), envelope(r4_out())]);
    let mut rt = RecallRuntime::default();
    run(&mut rt, &db, "delete", Some(&p)).unwrap();
    db.open()
        .transaction(|s| crate::forget::forget(s, &OWNER.into(), None, None, Some("s-purple")))
        .unwrap();
    assert!(
        run(&mut rt, &db, "delete", Some(&p))
            .unwrap_err()
            .to_string()
            .contains("snapshot_stale")
    );
    assert!(rt.cache.lock().unwrap().is_empty());
    assert_eq!(db.calls(), 2);
    let state = db.open().read().unwrap();
    let serialized = serde_json::to_string(&state).unwrap();
    assert!(!serialized.contains(TEXT));
    assert!(!serialized.contains("紫色"));
    for receipt in state.receipts.values() {
        assert_eq!(receipt.result, json!({"state":"invalidated"}));
    }
    // 元数据以来源身份失效，不依赖原话或空白变体匹配；其他引用不受此清理影响。
    let mut s = crate::PersistedMemoryState::default();
    for (key, source) in [("a", "gone"), ("b", "other")] {
        s.receipts.insert(
            key.into(),
            crate::sqlite::TurnReceipt {
                request: json!({"form":"recall_metadata_v1"}),
                result: json!({"state":"finished","source_ids":[source],"raw":" 任 意 空 白 "}),
            },
        );
    }
    invalidate_deleted(&mut s, &BTreeSet::from(["gone"]));
    assert_eq!(s.receipts["a"].result, json!({"state":"invalidated"}));
    assert_eq!(s.receipts["b"].result["state"], "finished");
}
#[test]
fn late_delete_during_r4_does_not_publish_or_cache_original_output() {
    let db = Db::new();
    let path = db.0.clone();
    let mut c = client(&db);
    let count = Arc::new(Mutex::new(0));
    let n = count.clone();
    c.transport = Some(Arc::new(move |_| {
        let mut i = n.lock().unwrap();
        *i += 1;
        if *i == 1 {
            envelope(r1_out("judge"))
        } else {
            SqliteMemoryStore::open(&path, OWNER.into())
                .unwrap()
                .transaction(|s| {
                    crate::forget::forget(s, &OWNER.into(), None, None, Some("s-purple"))
                })
                .unwrap();
            envelope(r4_out())
        }
    }));
    let p = provider(c, &db);
    let mut rt = RecallRuntime::default();
    assert!(run(&mut rt, &db, "late", Some(&p)).is_err());
    assert!(rt.cache.lock().unwrap().is_empty());
    assert_eq!(db.calls(), 2);
    assert!(
        !serde_json::to_string(&db.open().read().unwrap())
            .unwrap()
            .contains("紫色")
    );
    assert!(run(&mut rt, &db, "late", Some(&p)).is_err());
    assert_eq!(db.calls(), 2);
}
#[test]
fn foreground_stage_limit_survives_switching_authorization_groups() {
    let db = Db::new();
    let mk = |group: &str| {
        crate::model_budget::ModelBudget::new(
            &db.0,
            OWNER.into(),
            ModelBudgetConfig {
                group_id: group.into(),
                max_calls: 20,
                ..Default::default()
            },
            "request-op".into(),
        )
        .unwrap()
    };
    let a = mk("a");
    let b = mk("b");
    a.reserve(ModelStage::R1, b"{}").unwrap();
    assert!(b.reserve(ModelStage::R1, b"{}").is_err());
    b.reserve(ModelStage::R4, b"{}").unwrap();
    assert!(mk("c").reserve(ModelStage::R4, b"{}").is_err());
    assert_eq!(db.calls(), 2);
}
#[test]
fn crash_after_first_reservation_never_reissues_that_request() {
    let db = Db::new();
    let mut c = client(&db);
    c.transport = Some(Arc::new(|_| {
        panic!("simulate process loss after reservation")
    }));
    let p = provider(c, &db);
    let mut rt = RecallRuntime::default();
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(
            &mut rt,
            &db,
            "crash",
            Some(&p)
        )))
        .is_err()
    );
    assert_eq!(db.calls(), 1);
    let (p, sent) = mocked(&db, vec![]);
    assert!(
        run(&mut RecallRuntime::default(), &db, "crash", Some(&p))
            .unwrap_err()
            .to_string()
            .contains("result_not_retained")
    );
    assert_eq!(db.calls(), 1);
    assert!(sent.lock().unwrap().is_empty());
}
#[test]
fn schema_has_all_required_fields_and_decode_rejects_unknown_or_missing_fields() {
    fn strict(s: &Value) {
        if s["type"] == "object" {
            assert_eq!(s["additionalProperties"], false);
            let required: BTreeSet<_> = s["required"]
                .as_array()
                .unwrap()
                .iter()
                .map(|k| k.as_str().unwrap())
                .collect();
            let fields: BTreeSet<_> = s["properties"]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            assert_eq!(required, fields);
        }
        match s {
            Value::Object(o) => o.values().for_each(strict),
            Value::Array(a) => a.iter().for_each(strict),
            _ => (),
        }
    }
    for schema in [r1_schema(), r4_schema()] {
        strict(&schema);
    }
    let r1 = r1_out("judge");
    let r4 = r4_out();
    assert_eq!(
        r1_schema()["properties"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<BTreeSet<_>>(),
        r1.as_object().unwrap().keys().collect()
    );
    for k in r1.as_object().unwrap().keys() {
        let mut bad = r1.clone();
        bad.as_object_mut().unwrap().remove(k);
        assert!(crate::r1::decode(&bad.to_string()).is_err());
    }
    for k in r4["facts"][0].as_object().unwrap().keys() {
        let mut bad = r4.clone();
        bad["facts"][0].as_object_mut().unwrap().remove(k);
        assert!(crate::r4::decode(&bad.to_string()).is_err());
    }
    let mut bad = r4;
    bad["result"] = json!(2);
    assert!(crate::r4::decode(&bad.to_string()).is_err());
}
#[test]
fn role_configuration_defaults_and_fingerprint_keep_model_outside_payload() {
    let models: RecallModels = serde_json::from_value(json!({"r1":"custom"})).unwrap();
    assert_eq!(models.r4, DEEPSEEK_MODEL);
    assert!(serde_json::from_value::<RecallModels>(json!({"r3":"x"})).is_err());
    assert!(
        RecallModels {
            r1: " ".into(),
            r4: "x".into(),
            ..Default::default()
        }
        .validate()
        .is_err()
    );
    assert_eq!(
        fingerprint(&json!({"a":1,"b":2})),
        fingerprint(&json!({"b":2,"a":1}))
    );
    assert_ne!(
        fingerprint(&json!(["x", AT])),
        fingerprint(&json!(["x", AT + 1]))
    );
}
#[test]
fn recommended_recall_config_is_valid_without_changing_defaults() {
    let recommended: RecallModels =
        serde_json::from_str(include_str!("../examples/recall-models.json")).unwrap();
    recommended.validate().unwrap();
    for role in ["r1", "r4"] {
        assert_eq!(recommended.effort(role), ReasoningEffort::Minimal);
        assert_eq!(
            RecallModels::default().effort(role),
            ReasoningEffort::Medium
        );
    }
    assert_eq!(recommended.r1, "ep-20261003130756-trhj6");
    assert_eq!(recommended.r4, recommended.r1);
    assert_eq!(RecallModels::default().r1, DEEPSEEK_MODEL);
    assert_eq!(RecallModels::default().r4, DEEPSEEK_MODEL);
}
#[test]
fn recall_prompts_do_not_contain_any_write_or_recall_sample_text_ids_or_labels() {
    fn strings(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::String(s) => out.push(s.clone()),
            Value::Array(a) => a.iter().for_each(|v| strings(v, out)),
            _ => (),
        }
    }
    fn scan(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::Object(o) => {
                for (k, v) in o {
                    if matches!(
                        k.as_str(),
                        "message"
                            | "query"
                            | "text"
                            | "sentence"
                            | "history"
                            | "context"
                            | "focus"
                            | "note"
                            | "why"
                            | "must_not"
                    ) {
                        strings(v, out);
                    }
                    scan(v, out);
                }
            }
            Value::Array(a) => a.iter().for_each(|v| scan(v, out)),
            _ => (),
        }
    }
    for stage in ["r1", "r4", "w1", "w3", "w4"] {
        let dir =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("tests/eval/{stage}/samples"));
        let mut count = 0;
        for e in std::fs::read_dir(dir).unwrap() {
            let path = e.unwrap().path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            count += 1;
            let s: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            let mut texts = vec![s["id"].as_str().unwrap().into()];
            scan(&s, &mut texts);
            for text in texts.into_iter().filter(|s| !s.is_empty()) {
                for prompt in [R1_PROMPT, R4_PROMPT] {
                    assert!(
                        !prompt.contains(&text),
                        "{} leaked {text:?}",
                        path.display()
                    );
                }
            }
        }
        assert!(count > 0);
    }
}
#[test]
fn recall_real_http_mock_two_sends_replay_and_reopen_zero_sends() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        for output in [r1_out("judge"), r4_out()] {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(10)))
                .unwrap();
            let mut bytes = vec![];
            let mut chunk = [0; 4096];
            loop {
                let n = stream.read(&mut chunk).unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&chunk[..n]);
                if let Some(end) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..end]);
                    let len: usize = headers
                        .lines()
                        .find_map(|l| {
                            l.to_lowercase()
                                .strip_prefix("content-length:")
                                .map(|s| s.trim().parse().unwrap())
                        })
                        .unwrap();
                    if bytes.len() >= end + 4 + len {
                        break;
                    }
                }
            }
            let body = envelope(output).unwrap().1;
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
        }
    });
    let db = Db::new();
    let mut c = OpenAiClient::new(format!("http://{address}/v1"), "test-key")
        .unwrap()
        .with_budget(
            &db.0,
            OWNER,
            ModelBudgetConfig {
                group_id: "http".into(),
                max_calls: 2,
                ..Default::default()
            },
            "unused",
        )
        .unwrap();
    c.network_allowed = true;
    let p = provider(c, &db);
    let mut rt = RecallRuntime::default();
    let out = run(&mut rt, &db, "http", Some(&p)).unwrap();
    server.join().unwrap();
    assert_eq!(run(&mut rt, &db, "http", Some(&p)).unwrap(), out);
    assert!(run(&mut RecallRuntime::default(), &db, "http", Some(&p)).is_err());
    assert_eq!(db.calls(), 2);
}

#[test]
fn same_request_in_progress_does_not_reclaim_or_hold_cache_lock_across_http() {
    let db = Db::new();
    let path = db.0.clone();
    let rt = Arc::new(RecallRuntime::default());
    let nested = rt.clone();
    let mut c = client(&db);
    c.transport = Some(Arc::new(move |_| {
        let mut store = SqliteMemoryStore::open(&path, OWNER.into()).unwrap();
        let error = nested
            .run(&mut store, &request("concurrent"), Budget::default(), None)
            .unwrap_err();
        assert!(error.to_string().contains("result_not_retained"));
        envelope(r1_out("context_only"))
    }));
    let p = provider(c, &db);
    let first = rt
        .run(
            &mut db.open(),
            &request("concurrent"),
            Budget::default(),
            Some(&p),
        )
        .unwrap();
    assert_eq!(
        rt.run(
            &mut db.open(),
            &request("concurrent"),
            Budget::default(),
            Some(&p)
        )
        .unwrap(),
        first
    );
    assert_eq!(db.calls(), 1);
}

#[test]
fn long_request_id_is_bound_without_persisting_its_original_words() {
    let db = Db::new();
    let (p, sent) = mocked(&db, vec![envelope(r1_out("context_only"))]);
    let id = format!("私人标记{}", "a".repeat(490));
    assert!(id.len() <= 512);
    let runtime = RecallRuntime::default();
    let first = runtime
        .run(&mut db.open(), &request(&id), Budget::default(), Some(&p))
        .unwrap();
    assert_eq!(first.status, RecallStatus::ContextOnly);
    assert_eq!(sent.lock().unwrap().len(), 1);
    let state = db.open().read().unwrap();
    assert!(
        !serde_json::to_string(&state.receipts)
            .unwrap()
            .contains("私人标记")
    );
    assert_eq!(
        runtime
            .run(&mut db.open(), &request(&id), Budget::default(), Some(&p))
            .unwrap(),
        first
    );
    assert_eq!(db.calls(), 1);
    assert_eq!(
        db.open()
            .connection
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        9
    );
}

#[test]
fn unresolved_subject_without_judgment_evidence_keeps_r1_clarification_and_skips_r4() {
    let db = Db::new();
    db.open()
        .transaction(|s| crate::forget::forget(s, &OWNER.into(), None, None, Some("s-purple")))
        .unwrap();
    let mut req = request("clarify");
    req.binding.query = "那个对象的灯色是什么？".into();
    let mut output = r1_out("judge");
    output["subject"] = json!({"unresolved":{"text":"那个对象"}});
    let (p, sent) = mocked(&db, vec![envelope(output)]);
    let result = RecallRuntime::default()
        .run(&mut db.open(), &req, Budget::default(), Some(&p))
        .unwrap();
    assert_eq!(result.status, RecallStatus::NeedsClarification);
    assert_eq!(result.semantic_judgment, SemanticJudgment::Skipped);
    assert!(result.clarification.unwrap().question.contains("那个对象"));
    assert!(result.facts.is_empty());
    assert_eq!(sent.lock().unwrap().len(), 1);
    assert_eq!(db.calls(), 1);
}

#[test]
fn host_dispatch_online_replay_validator_and_mcp_guard_share_one_result() {
    let db = Db::new();
    let (p, sent) = mocked(&db, vec![envelope(r1_out("judge")), envelope(r4_out())]);
    let mut service = crate::chat::ChatMemoryService::new(
        db.open(),
        serde_json::from_value(json!({"recall_pipeline":"recall_v3","enrichment_mode":"off"}))
            .unwrap(),
    )
    .unwrap();
    service.inject_recall_provider(p);
    let args = json!({"query":"我的灯色是什么，临时标记 Q9？","context":["当前查询不授权保存。"],
        "request_id":"host-online","at":AT});
    let blocked = service.call_with_global_context("memory_recall", args.clone(), || false);
    assert_eq!(blocked.is_error, Some(true));
    assert!(
        blocked.structured_content.unwrap()["error"]
            .as_str()
            .unwrap()
            .contains("recall_v3_mcp_online_disabled")
    );
    assert_eq!(sent.lock().unwrap().len(), 0);
    let prepare = json!({"user_turn":{"text":"新陈述不会在拒绝后保存。","session_id":"host","turn_id":"blocked","capture_source":true}});
    assert_eq!(
        service
            .call_with_global_context("memory_prepare", prepare, || false)
            .is_error,
        Some(true)
    );
    assert_eq!(db.open().read().unwrap().sources.len(), 1);
    let out = service
        .call("memory_recall", args.clone(), || false)
        .unwrap();
    assert_eq!(out["version"], "recall.v3");
    assert_eq!(out["status"], "answered", "{out}");
    assert!(out.get("answer_contract").is_none());
    assert_eq!(sent.lock().unwrap().len(), 2);
    assert_eq!(service.call("memory_recall", args, || false).unwrap(), out);
    let legacy = service
        .call(
            "memory_recall",
            json!({"query":out["request"]["query"],"context":out["request"]["context"],
        "request_id":"host-online","at":AT,"recall_version":"answer_contract.v1"}),
            || false,
        )
        .unwrap();
    assert_eq!(legacy["answer_contract"]["version"], 1);
    assert_eq!(legacy["answer_contract"]["status"], "insufficient_evidence");
    assert!(legacy.get("facts").is_none());
    assert!(legacy.get("version").is_none());
    assert_eq!(sent.lock().unwrap().len(), 2);
    let validation = json!({"query":out["request"]["query"],"context":out["request"]["context"],
        "request_id":"host-online","draft_answer":TEXT,"source_ids":["s-purple"],"at":AT});
    let approved = service
        .call("memory_validate_answer", validation.clone(), || false)
        .unwrap();
    assert_eq!(approved["approved"], true, "{approved}");
    let mcp =
        service.call_with_global_context("memory_validate_answer", validation.clone(), || false);
    assert_ne!(mcp.is_error, Some(true));
    assert_eq!(mcp.structured_content.unwrap()["approved"], true);
    for (key, value) in [
        ("draft_answer", json!("我的灯是红色。")),
        ("source_ids", json!(["wrong-source"])),
        ("context", json!(["不同的用户上下文"])),
        ("at", json!(AT - 1)),
        ("task_id", json!("other")),
    ] {
        let mut bad = validation.clone();
        bad[key] = value;
        let checked = service.call("memory_validate_answer", bad, || false);
        assert!(
            checked.is_err() || checked.unwrap()["approved"] == false,
            "{key}"
        );
    }
    assert_eq!(sent.lock().unwrap().len(), 2);
    assert_eq!(db.calls(), 2);
    let state = db.open().read().unwrap();
    assert_eq!(state.sources.len(), 1);
    let receipts = serde_json::to_string(&state.receipts).unwrap();
    assert!(!receipts.contains("临时标记 Q9"));
    db.open()
        .transaction(|s| crate::forget::forget(s, &OWNER.into(), None, None, Some("s-purple")))
        .unwrap();
    assert!(
        service
            .call("memory_validate_answer", validation, || false)
            .is_err()
    );
    assert_eq!(sent.lock().unwrap().len(), 2);
}

#[test]
fn host_dispatch_mcp_offline_and_bootstrap_do_not_use_injected_provider() {
    let db = Db::new();
    let (p, sent) = mocked(&db, vec![]);
    let mut service = crate::chat::ChatMemoryService::new(
        db.open(),
        serde_json::from_value(json!({"recall_pipeline":"recall_v3","enrichment_mode":"off"}))
            .unwrap(),
    )
    .unwrap();
    service.inject_recall_provider(p);
    for (id, more) in [
        ("host-offline", json!({"offline":true})),
        ("host-bootstrap", json!({"bootstrap":true})),
    ] {
        let mut args = json!({"query":"全局回答要求","request_id":id,"at":AT});
        args.as_object_mut()
            .unwrap()
            .extend(more.as_object().unwrap().clone());
        let result = service.call_with_global_context("memory_recall", args.clone(), || false);
        assert_eq!(
            service
                .call_with_global_context("memory_recall", args, || false)
                .structured_content,
            result.structured_content
        );
        assert_ne!(result.is_error, Some(true), "{result:?}");
        let result = result.structured_content.unwrap();
        assert_eq!(result["recall"]["version"], "recall.v3");
        assert!(result.get("answer_contract").is_none());
        assert!(result["recall"].get("answer_contract").is_none());
        serde_json::from_value::<RecallOutput>(result["recall"].clone()).unwrap();
        if id == "host-bootstrap" {
            assert_eq!(result["recall"]["raw"], json!([]));
            assert_eq!(result["recall"]["records"], json!([]));
        }
    }
    assert!(service.call("memory_recall", json!({"query":"全局回答要求","request_id":"host-bootstrap","at":AT,"offline":true}), ||false)
        .unwrap_err().to_string().contains("recall_request_mismatch"));
    let args = json!({"user_turn":{"text":"我在练习素描。","session_id":"host","turn_id":"offline-prepare", "capture_source":true,"observed_at":AT},"offline":true});
    let response = service.call_with_global_context("memory_prepare", args.clone(), || false);
    let body = response.structured_content.as_ref().unwrap();
    assert_eq!(body["source"]["status"], "stored");
    assert_eq!(body["recall"]["version"], "recall.v3");
    assert!(body.get("remember").is_none());
    assert_eq!(
        service
            .validate_prepare_response(&args, response.clone())
            .structured_content,
        response.structured_content
    );
    let id = body["source"]["source_id"].as_str().unwrap();
    db.open()
        .transaction(|s| crate::forget::forget(s, &OWNER.into(), None, None, Some(id)))
        .unwrap();
    let invalidated = service.validate_prepare_response(&args, response);
    let body = invalidated.structured_content.unwrap();
    assert_eq!(body["source"]["status"], "deleted");
    assert_eq!(body["enrichment"]["status"], "deleted");
    assert_eq!(body["recall"]["status"], "not_requested");
    assert_eq!(sent.lock().unwrap().len(), 0);
    assert_eq!(db.calls(), 0);
}

#[test]
fn host_validator_rejects_unknown_current_even_for_identical_statement_and_sources() {
    let db = Db::new();
    let mut r4 = r4_out();
    r4["facts"][0]["current"] = json!("unknown");
    let (p, sent) = mocked(&db, vec![envelope(r1_out("judge")), envelope(r4)]);
    let mut service = crate::chat::ChatMemoryService::new(
        db.open(),
        serde_json::from_value(json!({"recall_pipeline":"recall_v3","enrichment_mode":"off"}))
            .unwrap(),
    )
    .unwrap();
    service.inject_recall_provider(p);
    let out = service
        .call(
            "memory_recall",
            json!({"query":"请核对我的灯色。","request_id":"unknown-current","at":AT}),
            || false,
        )
        .unwrap();
    assert_eq!(out["facts"][0]["current"], "unknown");
    let out = service.call("memory_validate_answer",json!({"query":"请核对我的灯色。","request_id":"unknown-current","draft_answer":TEXT,"source_ids":["s-purple"]}),||false).unwrap();
    assert_eq!(out["approved"], false);
    assert_eq!(sent.lock().unwrap().len(), 2);
}

#[test]
fn host_validator_rejects_r5_visibility_gate_without_another_model_send() {
    let db = Db::new();
    db.open()
        .transaction(|s| {
            let template = s.sources["s-purple"].clone();
            // 33 个可信来源，紫灯原文完整进入 32 项预算，另一个未定位来源被遗漏。
            s.sources.get_mut("s-purple").unwrap().received_sequence = 34;
            for i in 0..32 {
                let mut source = template.clone();
                source.id = format!("unprocessed-{i}");
                source.turn_id = source.id.clone();
                source.received_sequence = i + 2;
                source.enrichment = "queued".into();
                source.request.as_mut().unwrap()["text"] = json!(if i == 0 {
                    "灯已经换成绿色。"
                } else {
                    "一条尚未整理的原文。"
                });
                s.sources.insert(source.id.clone(), source);
            }
            Ok(())
        })
        .unwrap();
    let (p, sent) = mocked(&db, vec![envelope(r1_out("judge")), envelope(r4_out())]);
    let mut service = crate::chat::ChatMemoryService::new(
        db.open(),
        serde_json::from_value(json!({"recall_pipeline":"recall_v3","enrichment_mode":"off"}))
            .unwrap(),
    )
    .unwrap();
    service.inject_recall_provider(p);
    let args = json!({"query":"请核对我的灯色。","request_id":"visibility-gate","at":AT});
    let out = service
        .call("memory_recall", args.clone(), || false)
        .unwrap();
    assert_eq!(out["raw"].as_array().unwrap().len(), 32);
    assert!(
        !out["raw"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["source_id"] == "unprocessed-0")
    );
    assert_eq!(out["completeness"]["pending_related_sources"], 33);
    assert_eq!(out["completeness"]["retrieval_truncated"], true);
    assert_eq!(out["completeness"]["plan_complete"], false);
    assert_eq!(out["facts"][0]["current"], "unknown");
    assert_eq!(out["status"], "insufficient");
    assert_eq!(out["degraded_reasons"], json!([]));
    let validation = service
        .call(
            "memory_validate_answer",
            json!({
                "query":"请核对我的灯色。","request_id":"visibility-gate",
                "draft_answer":TEXT,"source_ids":["s-purple"]
            }),
            || false,
        )
        .unwrap();
    assert_eq!(validation["approved"], false);
    assert_eq!(validation["approved_source_ids"], json!([]));
    assert_eq!(service.call("memory_recall", args, || false).unwrap(), out);
    assert_eq!(sent.lock().unwrap().len(), 2);
    assert_eq!(db.calls(), 2);
}
