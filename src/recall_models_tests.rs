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
fn replay_preserves_observed_enrichment_when_only_progress_changes() {
    let db = Db::new();
    let mut rt = RecallRuntime::default();
    let first = run(&mut rt, &db, "stale", None).unwrap();
    let before = db.open().read().unwrap().source_revision;
    db.open()
        .transaction(|s| {
            s.sources.get_mut("s-purple").unwrap().enrichment = "needs_rejudge".into();
            Ok(())
        })
        .unwrap();
    assert_eq!(db.open().read().unwrap().source_revision, before);
    assert_eq!(run(&mut rt, &db, "stale", None).unwrap(), first);
    assert_eq!(rt.retained(&db.open(), "stale").unwrap(), first);
    assert_eq!(
        run(&mut rt, &db, "fresh", None).unwrap().raw[0].enrichment,
        EnrichmentStatus::NeedsRejudge
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
                for prompt in [
                    R1_PROMPT,
                    R4_PROMPT,
                    crate::write_models::W1_PROMPT,
                    crate::write_models::W3_PROMPT,
                    crate::write_models::W4_PROMPT,
                ] {
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
    // 全量用例也可能泄漏；只扫描原话、问题、上下文和说明，不把通用契约枚举当标签。
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/eval/cases");
    let mut count = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        count += 1;
        let case: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let mut texts = vec![];
        let mut ids = vec![case["id"].as_str().unwrap()];
        for key in ["messages", "questions"] {
            for row in case[key].as_array().unwrap() {
                ids.push(row["id"].as_str().unwrap());
                for key in ["text", "query", "history", "context", "focus", "note"] {
                    strings(&row[key], &mut texts);
                }
                strings(&row["expect_common"]["annotator_note"], &mut texts);
            }
        }
        for prompt in [
            R1_PROMPT,
            R4_PROMPT,
            crate::write_models::W1_PROMPT,
            crate::write_models::W3_PROMPT,
            crate::write_models::W4_PROMPT,
        ] {
            for text in texts.iter().filter(|s| !s.is_empty()) {
                assert!(!prompt.contains(text), "{} leaked {text:?}", path.display());
            }
            // q 等短 ID 检查完整词元；不能因 quote 含字母 q 就判泄漏。
            for id in &ids {
                assert!(
                    !prompt
                        .split(|c: char| !c.is_ascii_alphanumeric() && !matches!(c, '_' | '-'))
                        .any(|token| token == *id),
                    "{} leaked ID {id}",
                    path.display()
                );
            }
        }
    }
    assert_eq!(count, 35);
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
fn host_dispatch_online_replay_validator_and_mcp_share_one_result() {
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
    let online = service.call_with_global_context("memory_recall", args.clone(), || false);
    assert_ne!(online.is_error, Some(true));
    assert_eq!(
        online.structured_content.unwrap()["recall"]["status"],
        "answered"
    );
    assert_eq!(sent.lock().unwrap().len(), 2);
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

#[test]
fn shared_operation_services_replay_validate_and_forget_without_extra_sends() {
    let db = Db::new();
    let (p, sent) = mocked(&db, vec![envelope(r1_out("judge")), envelope(r4_out())]);
    let mut service = crate::chat::ChatMemoryService::new(
        db.open(),
        serde_json::from_value(json!({"recall_pipeline":"recall_v3","enrichment_mode":"off"}))
            .unwrap(),
    )
    .unwrap();
    service.inject_recall_provider(p);
    let operation = service.operation();
    drop(service);
    let mut first = operation.connect().unwrap();
    let args = json!({"query":"请核对我的灯色。","request_id":"shared-operation","at":AT});
    let result = first.call("memory_recall", args.clone(), || false).unwrap();
    assert_eq!(result["status"], "answered");
    drop(first);
    let mut second = operation.connect().unwrap();
    assert_eq!(
        second
            .call("memory_recall", args.clone(), || false)
            .unwrap(),
        result
    );
    let validation = second
        .call(
            "memory_validate_answer",
            json!({
                "query":"请核对我的灯色。","request_id":"shared-operation",
                "draft_answer":TEXT,"source_ids":["s-purple"]
            }),
            || false,
        )
        .unwrap();
    assert_eq!(validation["approved"], true);
    let absent = second
        .call("memory_forget", json!({"staged_id":999999}), || false)
        .unwrap();
    assert_eq!(absent["removed"], false);
    assert_eq!(
        second
            .call("memory_recall", args.clone(), || false)
            .unwrap(),
        result
    );
    second
        .call("memory_forget", json!({"source_id":"s-purple"}), || false)
        .unwrap();
    assert!(
        operation
            .connect()
            .unwrap()
            .call("memory_recall", args, || false)
            .unwrap_err()
            .to_string()
            .contains("snapshot_stale")
    );
    assert_eq!(sent.lock().unwrap().len(), 2);
    assert_eq!(db.calls(), 2);
}

// 在 R1 的物理发送处保持未完成；无端口、无真实请求、无固定 sleep。
fn held_r1(
    db: &Db,
) -> (
    RecallModelProvider,
    Arc<Mutex<Vec<Value>>>,
    std::sync::mpsc::Receiver<()>,
    std::sync::mpsc::Sender<()>,
) {
    held_model(db, "r1-route-sentinel")
}

fn held_model(
    db: &Db,
    model: &'static str,
) -> (
    RecallModelProvider,
    Arc<Mutex<Vec<Value>>>,
    std::sync::mpsc::Receiver<()>,
    std::sync::mpsc::Sender<()>,
) {
    let (mut p, sent) = mocked(db, vec![envelope(r1_out("judge")), envelope(r4_out())]);
    let transport = p.client.transport.take().unwrap();
    let (entered, receiver) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    let released = Mutex::new(released);
    p.client.transport = Some(Arc::new(move |request| {
        let body: Value =
            serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap();
        if body["model"] == model {
            entered.send(()).unwrap();
            released
                .lock()
                .unwrap()
                .recv_timeout(std::time::Duration::from_secs(15))
                .unwrap();
        }
        transport(request)
    }));
    (p, sent, receiver, release)
}

fn wait_for_same_request(rt: &RecallRuntime) {
    let flight = Arc::clone(rt.in_flight.lock().unwrap().values().next().unwrap());
    let mut result = flight.result.lock().unwrap();
    while flight.waiters.load(Ordering::SeqCst) == 0 {
        let (next, timeout) = flight
            .ready
            .wait_timeout(result, std::time::Duration::from_secs(5))
            .unwrap();
        result = next;
        assert!(
            !timeout.timed_out(),
            "second request did not join the in-flight result"
        );
    }
}

#[test]
fn shared_runtime_same_id_waits_once_rejects_other_args_and_does_not_lock_model_wait() {
    let db = Db::new();
    let (p, sent, entered, release) = held_r1(&db);
    let rt = Arc::new(RecallRuntime::default());
    std::thread::scope(|scope| {
        let mut first = SqliteMemoryStore::open_operation(&db.0, OWNER.into()).unwrap();
        let runtime = Arc::clone(&rt);
        let input = request("concurrent");
        let provider = p.clone();
        let original = scope.spawn(move || {
            runtime.run_with_audit(&mut first, &input, Budget::default(), Some(&provider))
        });
        entered
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let mut second = SqliteMemoryStore::open_operation(&db.0, OWNER.into()).unwrap();
        let runtime = Arc::clone(&rt);
        let input = request("concurrent");
        let provider = p.clone();
        let waiter = scope.spawn(move || {
            runtime.run_with_audit(&mut second, &input, Budget::default(), Some(&provider))
        });
        wait_for_same_request(&rt);
        let mut changed = request("concurrent");
        changed.binding.query.push('!');
        let mut other = SqliteMemoryStore::open_operation(&db.0, OWNER.into()).unwrap();
        assert!(
            rt.run(&mut other, &changed, Budget::default(), Some(&p))
                .unwrap_err()
                .to_string()
                .contains("request_mismatch")
        );
        // A distinct offline request finishes before the held R1 is released.
        assert!(
            rt.run(&mut other, &request("other"), Budget::default(), None)
                .is_ok()
        );
        release.send(()).unwrap();
        let result = original.join().unwrap().unwrap();
        assert_eq!(waiter.join().unwrap().unwrap(), result);
        assert_eq!(result.0.status, RecallStatus::Answered);
        assert!(result.1.is_some());
    });
    assert!(rt.in_flight.lock().unwrap().is_empty());
    assert_eq!(sent.lock().unwrap().len(), 2);
    assert_eq!(db.calls(), 2);
}

#[test]
fn shared_runtime_deletion_wakes_waiters_without_retaining_late_source_text() {
    let db = Db::new();
    let (p, sent, entered, release) = held_r1(&db);
    let rt = Arc::new(RecallRuntime::default());
    std::thread::scope(|scope| {
        let mut first = SqliteMemoryStore::open_operation(&db.0, OWNER.into()).unwrap();
        let runtime = Arc::clone(&rt);
        let input = request("concurrent-delete");
        let provider = p.clone();
        let original = scope
            .spawn(move || runtime.run(&mut first, &input, Budget::default(), Some(&provider)));
        entered
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let mut second = SqliteMemoryStore::open_operation(&db.0, OWNER.into()).unwrap();
        let runtime = Arc::clone(&rt);
        let input = request("concurrent-delete");
        let provider = p.clone();
        let waiter = scope
            .spawn(move || runtime.run(&mut second, &input, Budget::default(), Some(&provider)));
        wait_for_same_request(&rt);
        let mut deleting = SqliteMemoryStore::open_operation(&db.0, OWNER.into()).unwrap();
        deleting
            .transaction(|s| crate::forget::forget(s, &OWNER.into(), None, None, Some("s-purple")))
            .unwrap();
        rt.invalidate_owner(&deleting);
        release.send(()).unwrap();
        let error = original.join().unwrap().unwrap_err().to_string();
        // Existing deletion cleaning invalidates the persistent claim as well as the snapshot.
        assert_eq!(error, "recall_claim_lost");
        assert_eq!(waiter.join().unwrap().unwrap_err().to_string(), error);
    });
    assert!(rt.cache.lock().unwrap().is_empty());
    assert!(rt.in_flight.lock().unwrap().is_empty());
    assert_eq!(sent.lock().unwrap().len(), 1);
    assert_eq!(db.calls(), 1);
    assert!(
        !serde_json::to_string(&db.open().read().unwrap())
            .unwrap()
            .contains(TEXT)
    );
}

#[test]
fn shared_runtime_owner_and_database_namespaces_isolate_results_and_deletion() {
    let db = Db::new();
    let other_db = Db::new();
    let rt = Arc::new(RecallRuntime::default());
    let (p, sent) = mocked(&db, vec![envelope(r1_out("judge")), envelope(r4_out())]);
    let req = request("same-id");
    rt.run(&mut db.open(), &req, Budget::default(), Some(&p))
        .unwrap();
    let mut bob = SqliteMemoryStore::open(&db.0, "bob".into()).unwrap();
    assert!(rt.retained(&bob, "same-id").is_err());
    let mut bob_request = request("same-id");
    bob_request.owner = "bob".into();
    let bob_result = rt
        .run(&mut bob, &bob_request, Budget::default(), None)
        .unwrap();
    assert!(
        bob_result.raw.is_empty() && bob_result.records.is_empty() && bob_result.facts.is_empty()
    );
    assert!(
        rt.run(&mut bob, &req, Budget::default(), None)
            .unwrap_err()
            .to_string()
            .contains("owner_mismatch")
    );
    assert!(rt.retained(&other_db.open(), "same-id").is_err());
    let other_result = rt
        .run(&mut other_db.open(), &req, Budget::default(), None)
        .unwrap();
    let mut deleting = db.open();
    deleting
        .transaction(|s| crate::forget::forget(s, &OWNER.into(), None, None, Some("s-purple")))
        .unwrap();
    rt.invalidate_owner(&deleting);
    assert_eq!(rt.cache.lock().unwrap().len(), 2);
    assert_eq!(rt.retained(&bob, "same-id").unwrap(), bob_result);
    assert_eq!(
        rt.retained(&other_db.open(), "same-id").unwrap(),
        other_result
    );
    assert!(rt.cache.lock().unwrap().values().all(|c| c.r4.is_none()));
    assert_eq!(sent.lock().unwrap().len(), 2);
}

#[test]
fn shared_runtime_executor_panic_wakes_waiter_and_never_reissues_a_reserved_send() {
    let db = Db::new();
    let (mut p, _, entered, release) = held_r1(&db);
    let held = p.client.transport.take().unwrap();
    p.client.transport = Some(Arc::new(move |request| {
        let _ = held(request);
        panic!("controlled executor loss");
    }));
    let rt = Arc::new(RecallRuntime::default());
    std::thread::scope(|scope| {
        let mut first = SqliteMemoryStore::open_operation(&db.0, OWNER.into()).unwrap();
        let runtime = Arc::clone(&rt);
        let provider = p.clone();
        let original = scope.spawn(move || {
            runtime.run(
                &mut first,
                &request("panic-waiter"),
                Budget::default(),
                Some(&provider),
            )
        });
        entered
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let mut second = SqliteMemoryStore::open_operation(&db.0, OWNER.into()).unwrap();
        let runtime = Arc::clone(&rt);
        let provider = p.clone();
        let waiter = scope.spawn(move || {
            runtime.run(
                &mut second,
                &request("panic-waiter"),
                Budget::default(),
                Some(&provider),
            )
        });
        wait_for_same_request(&rt);
        release.send(()).unwrap();
        assert!(original.join().is_err());
        assert_eq!(
            waiter.join().unwrap().unwrap_err().to_string(),
            "recall_execution_aborted:no_model_retry"
        );
    });
    assert!(rt.in_flight.lock().unwrap().is_empty());
    assert!(rt.cache.lock().unwrap().is_empty());
    let (p, sent) = mocked(&db, vec![]);
    assert!(
        rt.run(
            &mut db.open(),
            &request("panic-waiter"),
            Budget::default(),
            Some(&p)
        )
        .unwrap_err()
        .to_string()
        .contains("result_not_retained:no_model_retry")
    );
    assert_eq!(db.calls(), 1);
    assert!(sent.lock().unwrap().is_empty());
}

fn mcp_service(db: &Db, provider: RecallModelProvider) -> Mutex<crate::chat::ChatMemoryService> {
    let mut service = crate::chat::ChatMemoryService::new(
        db.open(),
        serde_json::from_value(json!({"recall_pipeline":"recall_v3","enrichment_mode":"off"}))
            .unwrap(),
    )
    .unwrap();
    service.inject_recall_provider(provider);
    Mutex::new(service)
}

fn mcp_call(
    service: &Mutex<crate::chat::ChatMemoryService>,
    name: &str,
    args: Value,
    cancelled: impl Fn() -> bool,
) -> rmcp::model::CallToolResult {
    crate::chat::ChatMemoryService::call_shared(
        service,
        name,
        args,
        cancelled,
        &diagnostics::new_id(),
    )
    .unwrap()
}

fn mcp_query(id: &str) -> Value {
    json!({"query":"请核对我的灯色。","request_id":id,"at":AT})
}

#[test]
fn mcp_online_r1_and_r4_wait_outside_lock_and_same_id_sends_once() {
    for stage in ["r1-route-sentinel", "r4-route-sentinel"] {
        let db = Db::new();
        let (p, sent, entered, release) = held_model(&db, stage);
        let service = mcp_service(&db, p);
        let runtime = service.lock().unwrap().shared_recall_runtime();
        std::thread::scope(|scope| {
            let first =
                scope.spawn(|| mcp_call(&service, "memory_recall", mcp_query("same"), || false));
            entered
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            let duplicate =
                scope.spawn(|| mcp_call(&service, "memory_recall", mcp_query("same"), || false));
            wait_for_same_request(&runtime);
            let (done, received) = std::sync::mpsc::channel();
            let service = &service;
            scope.spawn(move || {
                done.send(mcp_call(&service, "memory_list", json!({}), || false))
                    .unwrap()
            });
            assert_ne!(
                received
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap()
                    .is_error,
                Some(true)
            );
            let mut mismatched = mcp_query("same");
            mismatched["query"] = json!("另一问题");
            let rejected = mcp_call(&service, "memory_recall", mismatched, || false);
            assert!(
                rejected.structured_content.unwrap()["error"]
                    .as_str()
                    .unwrap()
                    .contains("request_mismatch")
            );
            let mut offline = mcp_query("other");
            offline["offline"] = json!(true);
            assert_ne!(
                mcp_call(&service, "memory_recall", offline, || false).is_error,
                Some(true)
            );
            release.send(()).unwrap();
            let result = first.join().unwrap();
            assert_ne!(result.is_error, Some(true), "{result:?}");
            assert_eq!(
                result.structured_content.as_ref().unwrap()["recall"]["status"],
                "answered"
            );
            assert_eq!(
                duplicate.join().unwrap().structured_content,
                result.structured_content
            );
        });
        assert_eq!(sent.lock().unwrap().len(), 2);
        assert_eq!(db.calls(), 2);
    }
}

#[test]
fn mcp_online_delete_invalidates_but_new_sources_preserve_both_model_stages_without_retry() {
    for stage in ["r1-route-sentinel", "r4-route-sentinel"] {
        for delete in [true, false] {
            let db = Db::new();
            let (p, sent, entered, release) = held_model(&db, stage);
            let service = mcp_service(&db, p);
            let runtime = service.lock().unwrap().shared_recall_runtime();
            std::thread::scope(|scope| {
                let first = scope
                    .spawn(|| mcp_call(&service, "memory_recall", mcp_query("changed"), || false));
                entered
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
                let duplicate = scope
                    .spawn(|| mcp_call(&service, "memory_recall", mcp_query("changed"), || false));
                wait_for_same_request(&runtime);
                let (tool, args) = if delete {
                    ("memory_forget", json!({"source_id":"s-purple"}))
                } else {
                    (
                        "memory_remember",
                        json!({"text":"我有一把绿伞。","session_id":"mcp","turn_id":"change","capture_source":true,"observed_at":AT}),
                    )
                };
                assert_ne!(
                    mcp_call(&service, tool, args, || false).is_error,
                    Some(true)
                );
                release.send(()).unwrap();
                let results = [first.join().unwrap(), duplicate.join().unwrap()];
                if delete {
                    for result in results {
                        assert_eq!(result.is_error, Some(true));
                        let body = result.structured_content.unwrap();
                        assert!(
                            body["error"].as_str().unwrap().contains(if delete {
                                "recall_claim_lost"
                            } else {
                                "recall_snapshot_changed"
                            }),
                            "{body}"
                        );
                        assert!(!body.to_string().contains(TEXT));
                    }
                } else {
                    assert_eq!(results[0].structured_content, results[1].structured_content);
                    for result in results {
                        assert_ne!(result.is_error, Some(true));
                        assert_eq!(
                            result.structured_content.unwrap()["recall"]["status"],
                            "answered"
                        );
                    }
                }
            });
            if delete {
                assert!(runtime.cache.lock().unwrap().is_empty());
            } else {
                assert!(!runtime.cache.lock().unwrap().is_empty());
            }
            assert!(runtime.in_flight.lock().unwrap().is_empty());
            let state = db.open().read().unwrap();
            assert!(
                !serde_json::to_string(&state.receipts)
                    .unwrap()
                    .contains(TEXT)
            );
            if delete {
                assert!(!serde_json::to_string(&state).unwrap().contains(TEXT));
            }
            let expected = if delete {
                assert_eq!(
                    mcp_call(&service, "memory_recall", mcp_query("changed"), || false).is_error,
                    Some(true)
                );
                if stage == "r1-route-sentinel" { 1 } else { 2 }
            } else {
                assert_ne!(
                    mcp_call(&service, "memory_recall", mcp_query("changed"), || false).is_error,
                    Some(true)
                );
                2
            };
            assert_eq!(sent.lock().unwrap().len(), expected);
            assert_eq!(db.calls(), expected as i64);
        }
    }
}

#[test]
fn mcp_online_executor_cancellation_discards_late_output_and_never_retries() {
    for stage in ["r1-route-sentinel", "r4-route-sentinel"] {
        let db = Db::new();
        let (p, sent, entered, release) = held_model(&db, stage);
        let service = mcp_service(&db, p);
        let runtime = service.lock().unwrap().shared_recall_runtime();
        let cancelled = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let first = scope.spawn(|| {
                mcp_call(&service, "memory_recall", mcp_query("cancelled"), || {
                    cancelled.load(Ordering::SeqCst)
                })
            });
            entered
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            cancelled.store(true, Ordering::SeqCst);
            release.send(()).unwrap();
            let result = first.join().unwrap();
            assert_eq!(result.is_error, Some(true));
            let body = result.structured_content.unwrap();
            assert!(body["error"].as_str().unwrap().contains("cancelled"));
            assert!(!body.to_string().contains(TEXT));
        });
        assert!(runtime.cache.lock().unwrap().is_empty());
        assert!(runtime.in_flight.lock().unwrap().is_empty());
        assert_eq!(
            mcp_call(&service, "memory_recall", mcp_query("cancelled"), || false).is_error,
            Some(true)
        );
        let expected = if stage == "r1-route-sentinel" { 1 } else { 2 };
        assert_eq!(sent.lock().unwrap().len(), expected);
        assert_eq!(db.calls(), expected as i64);
    }
}

#[test]
fn mcp_online_cancel_replay_after_prior_delete_does_not_mark_snapshot_stale() {
    for stage in ["r1-route-sentinel", "r4-route-sentinel"] {
        for prior_delete in [false, true] {
            let db = Db::new();
            let (p, sent, entered, release) = held_model(&db, stage);
            let service = mcp_service(&db, p);
            let runtime = service.lock().unwrap().shared_recall_runtime();
            let source_id = if prior_delete {
                assert_ne!(
                    mcp_call(
                        &service,
                        "memory_forget",
                        json!({"source_id":"s-purple"}),
                        || false
                    )
                    .is_error,
                    Some(true)
                );
                let saved = mcp_call(
                    &service,
                    "memory_remember",
                    json!({
                        "text":TEXT,"session_id":"mcp","turn_id":"reseed",
                        "capture_source":true,"observed_at":AT
                    }),
                    || false,
                );
                assert_ne!(saved.is_error, Some(true));
                saved.structured_content.unwrap()["source"]["source_id"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            } else {
                "s-purple".to_owned()
            };
            let cancelled = AtomicBool::new(false);
            std::thread::scope(|scope| {
                let first = scope.spawn(|| {
                    mcp_call(
                        &service,
                        "memory_recall",
                        mcp_query("cancel-replay"),
                        || cancelled.load(Ordering::SeqCst),
                    )
                });
                entered
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
                let state = db.open().read().unwrap();
                let receipt = state
                    .receipts
                    .values()
                    .find(|r| r.request["form"] == "recall_metadata_v1")
                    .unwrap();
                let ids = receipt.result["source_ids"].as_array().unwrap();
                assert!(ids.contains(&json!(source_id)));
                if prior_delete {
                    assert!(!ids.contains(&json!("s-purple")));
                }
                cancelled.store(true, Ordering::SeqCst);
                release.send(()).unwrap();
                let result = first.join().unwrap();
                assert_eq!(result.is_error, Some(true));
                let body = result.structured_content.unwrap();
                assert!(
                    body["error"].as_str().unwrap().contains("cancelled"),
                    "{body}"
                );
                assert!(!body.to_string().contains(TEXT));
            });
            let replay = mcp_call(
                &service,
                "memory_recall",
                mcp_query("cancel-replay"),
                || false,
            );
            assert_eq!(replay.is_error, Some(true));
            let body = replay.structured_content.unwrap();
            assert!(
                body["error"].as_str().unwrap().contains("not_retained"),
                "stage={stage}, prior_delete={prior_delete}: {body}"
            );
            assert!(!body.to_string().contains(TEXT));
            let state = db.open().read().unwrap();
            let receipt = state
                .receipts
                .values()
                .find(|r| r.request["form"] == "recall_metadata_v1")
                .unwrap();
            assert_eq!(receipt.result["state"], "failed");
            assert!(runtime.cache.lock().unwrap().is_empty());
            assert!(runtime.in_flight.lock().unwrap().is_empty());
            let expected = if stage == "r1-route-sentinel" { 1 } else { 2 };
            assert_eq!(sent.lock().unwrap().len(), expected);
            assert_eq!(db.calls(), expected as i64);
        }
    }
}

#[test]
fn mcp_online_cancelled_duplicate_leaves_executor_and_budget_unchanged() {
    let db = Db::new();
    let (p, sent, entered, release) = held_r1(&db);
    let service = mcp_service(&db, p);
    let runtime = service.lock().unwrap().shared_recall_runtime();
    let cancelled = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let first = scope.spawn(|| {
            mcp_call(
                &service,
                "memory_recall",
                mcp_query("cancel-waiter"),
                || false,
            )
        });
        entered
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let (done, received) = std::sync::mpsc::channel();
        let service = &service;
        let cancelled = &cancelled;
        scope.spawn(move || {
            done.send(mcp_call(
                &service,
                "memory_recall",
                mcp_query("cancel-waiter"),
                || cancelled.load(Ordering::SeqCst),
            ))
            .unwrap()
        });
        wait_for_same_request(&runtime);
        cancelled.store(true, Ordering::SeqCst);
        let result = received
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert_eq!(result.is_error, Some(true));
        assert!(
            result.structured_content.unwrap()["error"]
                .as_str()
                .unwrap()
                .contains("cancelled")
        );
        release.send(()).unwrap();
        assert_ne!(first.join().unwrap().is_error, Some(true));
    });
    assert_ne!(
        mcp_call(
            &service,
            "memory_recall",
            mcp_query("cancel-waiter"),
            || false
        )
        .is_error,
        Some(true)
    );
    assert_eq!(sent.lock().unwrap().len(), 2);
    assert_eq!(db.calls(), 2);
}

#[test]
fn mcp_online_prepare_saves_before_model_wait_and_revalidates_shared_result() {
    let db = Db::new();
    let (p, sent, entered, release) = held_r1(&db);
    let service = mcp_service(&db, p);
    let args = json!({"user_turn":{"text":TEXT,"session_id":"mcp","turn_id":"prepared","capture_source":true,"observed_at":AT}});
    let result = std::thread::scope(|scope| {
        let first = scope.spawn(|| mcp_call(&service, "memory_prepare", args.clone(), || false));
        entered
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert_eq!(db.open().read().unwrap().sources.len(), 2);
        assert_ne!(
            mcp_call(&service, "memory_list", json!({}), || false).is_error,
            Some(true)
        );
        release.send(()).unwrap();
        first.join().unwrap()
    });
    let body = result.structured_content.as_ref().unwrap();
    assert_eq!(body["source"]["status"], "stored");
    assert_eq!(body["recall"]["status"], "answered");
    assert_eq!(
        mcp_call(&service, "memory_prepare", args.clone(), || false).structured_content,
        result.structured_content
    );
    mcp_call(
        &service,
        "memory_forget",
        json!({"source_id":"s-purple"}),
        || false,
    );
    let checked = service
        .lock()
        .unwrap()
        .validate_prepare_response(&args, result);
    assert_eq!(
        checked.structured_content.unwrap()["recall"]["status"],
        "not_requested"
    );
    assert_eq!(sent.lock().unwrap().len(), 2);
}

#[test]
fn mcp_online_timeout_replays_degraded_without_a_new_send_and_precancel_sends_zero() {
    let db = Db::new();
    let (p, sent) = mocked(
        &db,
        vec![
            envelope(r1_out("judge")),
            Err(OpenAiAdapterError::Http("timed out".into())),
        ],
    );
    let service = mcp_service(&db, p);
    let before = mcp_call(&service, "memory_recall", mcp_query("not-started"), || true);
    assert_eq!(before.is_error, Some(true));
    assert_eq!(sent.lock().unwrap().len(), 0);
    assert!(db.open().read().unwrap().receipts.is_empty());
    let first = mcp_call(&service, "memory_recall", mcp_query("timeout"), || false);
    assert_ne!(first.is_error, Some(true));
    let body = first.structured_content.as_ref().unwrap();
    assert_eq!(body["recall"]["status"], "degraded");
    assert_eq!(body["recall"]["semantic_judgment"], "failed");
    assert!(
        body["recall"]["degraded_reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "model_timeout")
    );
    assert_eq!(
        mcp_call(&service, "memory_recall", mcp_query("timeout"), || false).structured_content,
        first.structured_content
    );
    assert_eq!(sent.lock().unwrap().len(), 2);
    assert_eq!(db.calls(), 2);
}

#[test]
fn omitted_clock_concurrent_before_receipt_freezes_first_day_without_weakening_explicit_at() {
    let db = Db::new();
    let runtime = RecallRuntime::default();
    let (entered, waiting) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    let released = Mutex::new(released);
    let checks = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|scope| {
        let first = scope.spawn(|| {
            runtime.run_cancellable(
                &mut db.open(),
                &request("omitted-at"),
                Budget::default(),
                None,
                true,
                &|| {
                    if checks.fetch_add(1, Ordering::SeqCst) == 1 {
                        entered.send(()).unwrap();
                        released
                            .lock()
                            .unwrap()
                            .recv_timeout(std::time::Duration::from_secs(15))
                            .unwrap();
                    }
                    false
                },
            )
        });
        waiting
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert!(db.open().read().unwrap().receipts.is_empty());
        let second = scope.spawn(|| {
            let mut later = request("omitted-at");
            freeze_clock(&mut later, AT + 86_400).unwrap();
            runtime.run_cancellable(
                &mut db.open(),
                &later,
                Budget::default(),
                None,
                true,
                &|| false,
            )
        });
        wait_for_same_request(&runtime);
        let mut explicit = request("omitted-at");
        freeze_clock(&mut explicit, AT + 86_400).unwrap();
        assert!(
            runtime
                .run(&mut db.open(), &explicit, Budget::default(), None)
                .unwrap_err()
                .to_string()
                .contains("request_mismatch")
        );
        release.send(()).unwrap();
        let output = first.join().unwrap().unwrap();
        assert_eq!(output.request.at, AT);
        assert_eq!(second.join().unwrap().unwrap(), output);
        let mut later = request("omitted-at");
        freeze_clock(&mut later, AT + 86_400).unwrap();
        assert_eq!(
            runtime
                .run_cancellable(
                    &mut db.open(),
                    &later,
                    Budget::default(),
                    None,
                    true,
                    &|| false
                )
                .unwrap(),
            output
        );
    });
    assert_eq!(db.calls(), 0);
}

#[test]
fn input_and_control_changes_during_either_model_stage_fail_without_retry() {
    for stage in ["r1-route-sentinel", "r4-route-sentinel"] {
        for change in ["text", "history", "date", "task", "control", "entity"] {
            let db = Db::new();
            let (p, sent, entered, release) = held_model(&db, stage);
            let rt = RecallRuntime::default();
            std::thread::scope(|scope| {
                let run = scope.spawn(|| {
                    rt.run(
                        &mut db.open(),
                        &request("binding"),
                        Budget::default(),
                        Some(&p),
                    )
                });
                entered
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
                db.open().transaction(|state| {
                    if change == "entity" {
                        let mut e = state.entities.get(&EntityId::from("self")).unwrap().clone();
                        e.owner_id = "other".into();
                        state.entities.upsert(e);
                    } else {
                        let source = state.sources.get_mut("s-purple").unwrap();
                        match change {
                            "text" => source.request.as_mut().unwrap()["text"] = json!("修改后的灯色原文。"),
                            "history" => source.request.as_mut().unwrap()["history"] = json!(["不同的前文。"]),
                            "date" => source.observed_at += 86400,
                            "task" => source.request.as_mut().unwrap()["task_id"] = json!("other-task"),
                            "control" => source.result = Some(json!({"w4_controls":[{
                                "kind":"forget","quote":{"text":TEXT},"target":{"object":"source","candidate_indices":[],"quote":null},
                                "deadline":null,"status":"awaiting_confirmation"}]})),
                            _ => unreachable!(),
                        }
                    }
                    Ok(())
                }).unwrap();
                release.send(()).unwrap();
                assert!(
                    run.join()
                        .unwrap()
                        .unwrap_err()
                        .to_string()
                        .contains("recall_snapshot_changed"),
                    "{stage}/{change}"
                );
            });
            assert!(rt.cache.lock().unwrap().is_empty());
            assert_eq!(
                sent.lock().unwrap().len(),
                if stage.starts_with("r1") { 1 } else { 2 }
            );
        }
    }
}

fn add_legacy(db: &Db, text: &str) -> MemoryId {
    db.open()
        .transaction(|state| {
            let evidence = Evidence::new(
                "source-less-evidence".into(),
                "source-less".into(),
                "legacy-turn".into(),
                text,
            );
            let c = CandidateMemory::new(
                OWNER.into(),
                "self".into(),
                MemoryType::Fact,
                "fact".into(),
                MemoryValue::Text(text.into()),
                Timestamp(AT),
                evidence,
            );
            let id = MemoryId(
                state
                    .repository
                    .list_by_owner(&OWNER.into())?
                    .iter()
                    .map(|m| m.id.0)
                    .max()
                    .unwrap_or(0)
                    + 1,
            );
            state
                .repository
                .insert(Memory::from_candidate(id, &c, None))?;
            Ok(id)
        })
        .unwrap()
}

#[test]
fn ordinary_commits_during_models_and_after_completion_keep_read_snapshot_and_replay() {
    for stage in ["r1-route-sentinel", "r4-route-sentinel"] {
        let db = Db::new();
        let (p, sent, entered, release) = held_model(&db, stage);
        let rt = RecallRuntime::default();
        let initial = db.open().read().unwrap().revision;
        let out = std::thread::scope(|scope| {
            let run = scope.spawn(|| {
                rt.run(
                    &mut db.open(),
                    &request("commit"),
                    Budget::default(),
                    Some(&p),
                )
            });
            entered
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            add_legacy(&db, "独立的已提交背景。 ");
            db.open()
                .transaction(|s| {
                    s.sources.get_mut("s-purple").unwrap().enrichment = "running".into();
                    Ok(())
                })
                .unwrap();
            release.send(()).unwrap();
            run.join().unwrap().unwrap()
        });
        assert_eq!(out.status, RecallStatus::Answered);
        assert_eq!(
            out.snapshot.read_revision,
            if stage.starts_with("r1") {
                initial + 1
            } else {
                initial
            }
        );
        let received = out.raw[0].enrichment;
        assert_eq!(
            received,
            if stage.starts_with("r1") {
                EnrichmentStatus::Running
            } else {
                EnrichmentStatus::Failed
            }
        );
        add_legacy(&db, "完成以后新增的背景。 ");
        assert_eq!(rt.retained(&db.open(), "commit").unwrap(), out);
        assert_eq!(
            rt.run(
                &mut db.open(),
                &request("commit"),
                Budget::default(),
                Some(&p)
            )
            .unwrap(),
            out
        );
        let fresh = rt
            .run(&mut db.open(), &request("fresh"), Budget::default(), None)
            .unwrap();
        assert!(fresh.snapshot.read_revision > out.snapshot.read_revision);
        assert_eq!(sent.lock().unwrap().len(), 2);
        assert_eq!(db.calls(), 2);
    }
}

#[test]
fn mcp_validation_and_delivery_preserve_snapshot_after_version_progress_but_reject_delete() {
    let db = Db::new();
    let legacy = add_legacy(&db, "离线附加状态。");
    let (p, sent) = mocked(&db, vec![envelope(r1_out("judge")), envelope(r4_out())]);
    let service = mcp_service(&db, p);
    let first = mcp_call(
        &service,
        "memory_recall",
        mcp_query("validated-snapshot"),
        || false,
    );
    assert_ne!(first.is_error, Some(true));
    let original = first.structured_content.as_ref().unwrap()["recall"].clone();
    db.open()
        .transaction(|state| {
            let mut memory = state.repository.get(legacy)?.unwrap();
            // 普通版本终止与有效期推导不改收到原话的时间锚点。
            memory.status = MemoryStatus::Superseded;
            memory.temporal.valid_from = Some(DatedValue {
                at: Timestamp(AT),
                source: TemporalSource::Explicit,
            });
            memory.temporal.valid_to = Some(DatedValue {
                at: Timestamp(AT + 1),
                source: TemporalSource::Explicit,
            });
            state.repository.update(memory)?;
            Ok(())
        })
        .unwrap();
    let validation = json!({"query":original["request"]["query"],"request_id":"validated-snapshot",
        "draft_answer":TEXT,"source_ids":["s-purple"],"at":AT});
    let checked = mcp_call(
        &service,
        "memory_validate_answer",
        validation.clone(),
        || false,
    );
    assert_ne!(checked.is_error, Some(true));
    let checked = checked.structured_content.unwrap();
    assert_eq!(checked["approved"], true);
    assert_eq!(checked["snapshot"], original["snapshot"]);
    assert_eq!(
        mcp_call(
            &service,
            "memory_recall",
            mcp_query("validated-snapshot"),
            || false
        )
        .structured_content
        .unwrap()["recall"],
        original
    );
    let mut fresh = mcp_query("fresh-version");
    fresh["offline"] = json!(true);
    let fresh = mcp_call(&service, "memory_recall", fresh, || false);
    assert!(
        fresh.structured_content.unwrap()["recall"]["snapshot"]["read_revision"]
            .as_u64()
            .unwrap()
            > original["snapshot"]["read_revision"].as_u64().unwrap()
    );
    db.open()
        .transaction(|state| crate::forget::forget(state, &OWNER.into(), Some(legacy), None, None))
        .unwrap();
    let deleted = mcp_call(&service, "memory_validate_answer", validation, || false);
    assert_eq!(deleted.is_error, Some(true));
    assert!(!serde_json::to_string(&deleted).unwrap().contains(TEXT));
    assert_eq!(sent.lock().unwrap().len(), 2);
    assert_eq!(db.calls(), 2);
}

#[test]
fn source_less_legacy_deletion_invalidates_models_and_completed_cache() {
    for stage in ["r1-route-sentinel", "r4-route-sentinel", "finished"] {
        let db = Db::new();
        let id = add_legacy(&db, "没有来源记录的私密旧值。 ");
        let rt = RecallRuntime::default();
        if stage == "finished" {
            rt.run(
                &mut db.open(),
                &request("legacy-delete"),
                Budget::default(),
                None,
            )
            .unwrap();
            db.open()
                .transaction(|s| crate::forget::forget(s, &OWNER.into(), Some(id), None, None))
                .unwrap();
            assert!(rt.retained(&db.open(), "legacy-delete").is_err());
            assert!(
                rt.run(
                    &mut db.open(),
                    &request("legacy-delete"),
                    Budget::default(),
                    None
                )
                .is_err()
            );
        } else {
            let (p, sent, entered, release) = held_model(&db, stage);
            std::thread::scope(|scope| {
                let run = scope.spawn(|| {
                    rt.run(
                        &mut db.open(),
                        &request("legacy-delete"),
                        Budget::default(),
                        Some(&p),
                    )
                });
                entered
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
                db.open()
                    .transaction(|s| crate::forget::forget(s, &OWNER.into(), Some(id), None, None))
                    .unwrap();
                release.send(()).unwrap();
                assert!(run.join().unwrap().is_err());
            });
            assert_eq!(
                sent.lock().unwrap().len(),
                if stage.starts_with("r1") { 1 } else { 2 }
            );
        }
        assert!(rt.cache.lock().unwrap().is_empty());
    }
}

#[test]
fn delete_cannot_commit_between_final_check_and_cache_insertion() {
    let db = Db::new();
    let (p, _, entered, release) = held_model(&db, "r4-route-sentinel");
    let rt = RecallRuntime::default();
    std::thread::scope(|scope| {
        let run = scope.spawn(|| {
            rt.run(
                &mut db.open(),
                &request("cache-window"),
                Budget::default(),
                Some(&p),
            )
        });
        entered
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let cache = rt.cache.lock().unwrap();
        release.send(()).unwrap();
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if rt.in_flight.try_lock().is_err() {
                break;
            }
            assert!(
                std::time::Instant::now() < until,
                "executor never reached publication"
            );
            std::thread::yield_now();
        }
        let mut other = SqliteMemoryStore::open_operation(&db.0, OWNER.into()).unwrap();
        other
            .connection
            .busy_timeout(std::time::Duration::ZERO)
            .unwrap();
        assert!(
            other
                .transaction(|s| crate::forget::forget(
                    s,
                    &OWNER.into(),
                    None,
                    None,
                    Some("s-purple")
                ))
                .is_err()
        );
        drop(cache);
        run.join().unwrap().unwrap();
        other
            .transaction(|s| crate::forget::forget(s, &OWNER.into(), None, None, Some("s-purple")))
            .unwrap();
        assert!(rt.retained(&other, "cache-window").is_err());
    });
    assert!(rt.cache.lock().unwrap().is_empty());
}

#[test]
fn prepare_publication_blocks_delete_then_delayed_delivery_discards_all_copies() {
    let db = Db::new();
    let (p, _) = mocked(&db, vec![]);
    let mut service = crate::chat::ChatMemoryService::new(
        db.open(),
        serde_json::from_value(json!({"recall_pipeline":"recall_v3","enrichment_mode":"off"}))
            .unwrap(),
    )
    .unwrap();
    service.inject_recall_provider(p);
    let args = json!({"user_turn":{"text":"待交付的私密原文。","session_id":"publish","turn_id":"window","capture_source":true,"observed_at":AT},"offline":true});
    let response = service.call_with_global_context("memory_prepare", args.clone(), || false);
    let id = response.structured_content.as_ref().unwrap()["source"]["source_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut deleting = SqliteMemoryStore::open_operation(&db.0, OWNER.into()).unwrap();
    deleting
        .connection
        .busy_timeout(std::time::Duration::ZERO)
        .unwrap();
    let published = service
        .publish_response(response, |out| {
            assert!(
                deleting
                    .transaction(|s| crate::forget::forget(s, &OWNER.into(), None, None, Some(&id)))
                    .is_err()
            );
            out
        })
        .unwrap();
    deleting
        .transaction(|s| crate::forget::forget(s, &OWNER.into(), None, None, Some(&id)))
        .unwrap();
    let checked = service.validate_prepare_response(&args, published);
    let body = checked.structured_content.unwrap();
    assert_eq!(body["source"]["status"], "deleted");
    assert_eq!(body["recall"]["status"], "not_requested");
    assert!(!body.to_string().contains("待交付的私密原文"));
    assert_eq!(db.calls(), 0);
}

#[test]
fn sources_received_during_r1_bind_the_r3_receipt_and_later_deletion() {
    let db = Db::new();
    let (p, _, entered, release) = held_r1(&db);
    let rt = RecallRuntime::default();
    std::thread::scope(|scope| {
        let run = scope.spawn(|| {
            rt.run(
                &mut db.open(),
                &request("r3-sources"),
                Budget::default(),
                Some(&p),
            )
        });
        entered
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        db.open()
            .transaction(|state| {
                let mut source = state.sources["s-purple"].clone();
                source.id = "after-r1".into();
                source.turn_id = "after-r1".into();
                source.received_sequence = 2;
                source.request.as_mut().unwrap()["text"] = json!("R1 等待期间新接收的原文。 ");
                state.sources.insert("after-r1".into(), source);
                Ok(())
            })
            .unwrap();
        release.send(()).unwrap();
        let out = run.join().unwrap().unwrap();
        assert!(out.raw.iter().any(|r| r.source_id == "after-r1"));
    });
    let state = db.open().read().unwrap();
    let receipt = state
        .receipts
        .values()
        .find(|r| r.request["form"] == "recall_metadata_v1")
        .unwrap();
    assert!(
        receipt.result["source_ids"]
            .as_array()
            .unwrap()
            .contains(&json!("after-r1"))
    );
    db.open()
        .transaction(|s| crate::forget::forget(s, &OWNER.into(), None, None, Some("after-r1")))
        .unwrap();
    assert!(rt.retained(&db.open(), "r3-sources").is_err());
    assert!(
        rt.run(
            &mut db.open(),
            &request("r3-sources"),
            Budget::default(),
            Some(&p)
        )
        .is_err()
    );
    assert!(rt.cache.lock().unwrap().is_empty());
    assert_eq!(db.calls(), 2);
}

#[test]
fn completed_cache_rejects_controls_and_changed_receipt_identity() {
    for change in ["control", "attempt", "binding", "stamp"] {
        let db = Db::new();
        let rt = RecallRuntime::default();
        rt.run(
            &mut db.open(),
            &request("identity-guard"),
            Budget::default(),
            None,
        )
        .unwrap();
        db.open().transaction(|s| {
            if change == "control" {
                s.sources.get_mut("s-purple").unwrap().result = Some(json!({"w4_controls":[{
                    "kind":"forget","quote":{"text":TEXT},"target":{"object":"source","candidate_indices":[],"quote":null},
                    "deadline":null,"status":"awaiting_confirmation"}]}));
            } else {
                let receipt = s.receipts.values_mut().find(|r| r.request["form"] == "recall_metadata_v1").unwrap();
                match change {
                    "attempt" => receipt.result["attempt_id"] = json!("new-claim"),
                    "binding" => receipt.request["fingerprint"] = json!("different-request"),
                    "stamp" => receipt.result["stamp"]["revision"] = json!(99999),
                    _ => unreachable!(),
                }
            }
            Ok(())
        }).unwrap();
        assert!(
            rt.retained(&db.open(), "identity-guard").is_err(),
            "{change}"
        );
        assert!(
            rt.run(
                &mut db.open(),
                &request("identity-guard"),
                Budget::default(),
                None
            )
            .is_err(),
            "{change}"
        );
        assert!(rt.cache.lock().unwrap().is_empty());
        assert_eq!(db.calls(), 0);
    }
}

#[test]
fn remember_acknowledgment_is_progress_but_retention_control_is_safety() {
    let db = Db::new();
    let rt = RecallRuntime::default();
    let first = rt
        .run(
            &mut db.open(),
            &request("requests"),
            Budget::default(),
            None,
        )
        .unwrap();
    let control = |kind| {
        json!({"w4_controls":[{"kind":kind,"quote":{"text":TEXT},
        "target":{"object":"source","candidate_indices":[],"quote":null},"deadline":null,"status":"supported"}]})
    };
    db.open()
        .transaction(|s| {
            s.sources.get_mut("s-purple").unwrap().result = Some(control("remember"));
            Ok(())
        })
        .unwrap();
    assert_eq!(rt.retained(&db.open(), "requests").unwrap(), first);
    db.open()
        .transaction(|s| {
            s.sources.get_mut("s-purple").unwrap().result = Some(control("retention"));
            Ok(())
        })
        .unwrap();
    assert!(rt.retained(&db.open(), "requests").is_err());
    assert!(rt.cache.lock().unwrap().is_empty());
    assert_eq!(db.calls(), 0);
}
