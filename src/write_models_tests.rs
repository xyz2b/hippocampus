use super::*;
use crate::chat::{ChatConfig, ChatMemoryService, EnrichmentMode};
use crate::enrichment::{RuntimeInputBudget, run_source_with_models};
use crate::*;
use std::collections::{BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

struct Db(PathBuf);
impl Db {
    fn new() -> Self {
        Self(
            std::env::temp_dir()
                .join(format!("write-models-{}", diagnostics::new_id()))
                .join("memory.sqlite3"),
        )
    }
    fn open(&self) -> SqliteMemoryStore {
        SqliteMemoryStore::open(&self.0, UserId::from("alice")).unwrap()
    }
    fn start(&self, text: &str) -> String {
        self.start_turn(text, "turn", &["我曾提到咖啡。"])
    }
    fn start_turn(&self, text: &str, turn: &str, history: &[&str]) -> String {
        let mut service = ChatMemoryService::new(
            self.open(),
            ChatConfig {
                enrichment_mode: EnrichmentMode::Off,
                ..Default::default()
            },
        )
        .unwrap();
        let result = service
            .call(
                "memory_remember",
                json!({"text":text,"history":history,
            "session_id":"chat","turn_id":turn,"capture_source":true,"observed_at":1790870400i64}),
                || false,
            )
            .unwrap();
        let state = self.open().read().unwrap();
        let key = state
            .sources
            .iter()
            .find(|(_, s)| s.id == result["source"]["source_id"])
            .unwrap()
            .0
            .clone();
        w4_start_round(
            &mut self.open(),
            &key,
            if turn == "turn" { "write-op" } else { turn },
            false,
        )
        .unwrap();
        key
    }
    fn calls(&self) -> i64 {
        let db = rusqlite::Connection::open(&self.0).unwrap();
        db.query_row("SELECT count(*) FROM model_calls", [], |r| r.get(0))
            .unwrap()
    }
}
impl Drop for Db {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.0.parent().unwrap());
    }
}

fn context() -> OpenAiAdapterContext {
    let mut ctx = OpenAiAdapterContext::new(
        UserId::from("alice"),
        vec![Entity {
            id: EntityId::from("self"),
            owner_id: UserId::from("alice"),
            entity_type: EntityType::Person,
            canonical_name: "用户本人".into(),
            aliases: vec!["我".into()],
        }],
        Timestamp(0),
    )
    .unwrap();
    ctx.utc_offset_seconds = 28_800;
    ctx
}
fn client(db: &Db) -> OpenAiClient {
    db.open();
    OpenAiClient::new("http://127.0.0.1:1", "fake-key")
        .unwrap()
        .with_budget(
            &db.0,
            "alice",
            ModelBudgetConfig {
                group_id: "authorized".into(),
                max_calls: 100,
                max_operation_calls: 3,
                ..Default::default()
            },
            "placeholder",
        )
        .unwrap()
}
fn envelope(output: &str) -> (u16, String) {
    (
        200,
        json!({"status":"completed","usage":{"input_tokens":12,"output_tokens":34},
        "output":[{"type":"message","content":[{"type":"output_text","text":output}]}]})
        .to_string(),
    )
}
fn w1(text: &str) -> Json {
    json!({"memory_requests":[],"candidates":[{
        "kind":"assertion","speech":"user_statement","subject":{"entity_id":"self"},
        "relation":"fact","memory_type":"fact","value":{"kind":"text","text":text,"quote":{"text":text}},
        "sentence":{"text":text},"polarity":"affirm","strength":null,"uncertainty":null,
        "conditions":[],"times":[]
    }]})
}
fn w3() -> Json {
    json!({"reviews":[{"candidate":0,"verdict":"supported","problems":[],"episode_value":"not_applicable"}],
        "coverage":{"status":"complete","missing":[]},"request_reviews":[]})
}
fn mocked(
    db: &Db,
    responses: Vec<Result<(u16, String), OpenAiAdapterError>>,
) -> (WriteModelProvider, Arc<Mutex<Vec<Json>>>) {
    let mut client = client(db);
    let responses = Arc::new(Mutex::new(VecDeque::from(responses)));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let sent = requests.clone();
    client.transport = Some(Arc::new(move |request| {
        sent.lock()
            .unwrap()
            .push(serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap());
        responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected automatic retry/send")
    }));
    (WriteModelProvider::new(client, context()), requests)
}
fn run(db: &Db, key: &str, provider: &WriteModelProvider) -> Json {
    run_source_with_models(
        &mut db.open(),
        key,
        RuntimeInputBudget::default(),
        provider,
        |_, _, _| panic!("no related records: W4 must be skipped"),
    )
    .unwrap()
}
fn inputs(store: &SqliteMemoryStore, key: &str, claim: &W4Claim) -> (ProviderContext, SourceInput) {
    let state = store.read().unwrap();
    let s = &state.sources[key];
    (
        ProviderContext {
            owner: store.owner().clone(),
            source_key: key.into(),
            source_id: s.id.clone(),
            source_binding: claim.source_binding().clone(),
            observed_at: s.observed_at,
            received_sequence: s.received_sequence,
            task_id: None,
            round: claim.round,
            operation_id: claim.operation_id.clone(),
            previous_w2: None,
        },
        SourceInput {
            text: s.request.as_ref().unwrap()["text"].as_str().unwrap().into(),
            history: serde_json::from_value(s.request.as_ref().unwrap()["history"].clone())
                .unwrap(),
        },
    )
}

#[test]
fn empty_w1_sends_once_and_nonempty_sends_twice_through_runtime() {
    for empty in [true, false] {
        let db = Db::new();
        let text = "我喜欢咖啡。";
        let key = db.start(text);
        let responses = if empty {
            vec![Ok(envelope(
                &json!({"memory_requests":[],"candidates":[]}).to_string(),
            ))]
        } else {
            vec![
                Ok(envelope(&w1(text).to_string())),
                Ok(envelope(&w3().to_string())),
            ]
        };
        let (provider, sent) = mocked(&db, responses);
        let result = run(&db, &key, &provider);
        assert_eq!(
            result["enrichment"]["status"],
            if empty {
                "no_candidates_unreviewed"
            } else {
                "committed"
            }
        );
        assert_eq!(
            provider.client.model_call_count(),
            if empty { 1 } else { 2 }
        );
        assert_eq!(db.calls(), if empty { 1 } else { 2 });
        let requests = sent.lock().unwrap();
        let input: Json = serde_json::from_str(requests[0]["input"].as_str().unwrap()).unwrap();
        assert_eq!(input["said_on"], "2026-10-02"); // Source reception, not context.now/execution day.
        assert_eq!(input["relations"][0]["id"], "fact");
        assert_eq!(input["user_history"], json!(["我曾提到咖啡。"]));
        for request in requests.iter() {
            assert_eq!(request["text"]["format"]["strict"], true);
        }
        if !empty {
            let state = db.open().read().unwrap();
            assert_eq!(
                state
                    .repository
                    .list_by_owner(&UserId::from("alice"))
                    .unwrap()
                    .len(),
                1
            );
            assert!(
                state.sources[&key].result.as_ref().unwrap()["w4_model_stages"]["w3"].is_object()
            );
        }
    }
}

#[test]
fn timeout_invalid_json_schema_and_http_failures_consume_without_retry() {
    let cases = vec![
        (
            Err(OpenAiAdapterError::Http("request timed out".into())),
            "model_timeout",
        ),
        (Ok(envelope("not JSON")), "w1_invalid_output"),
        (
            Ok(envelope(
                "{\"memory_requests\":[],\"candidates\":[],\"tags\":[]}",
            )),
            "w1_invalid_output",
        ),
        (
            Ok((503, "{\"error\":{\"message\":\"unavailable\"}}".into())),
            "attempt_failed",
        ),
    ];
    for (response, code) in cases {
        let db = Db::new();
        let key = db.start("我喜欢咖啡。");
        let (provider, _) = mocked(&db, vec![response]);
        assert_eq!(run(&db, &key, &provider)["enrichment"]["status"], "failed");
        let state = db.open().read().unwrap();
        let source = &state.sources[&key];
        assert_eq!(source.error.as_deref(), Some(code));
        assert!(source.request.is_some());
        assert_eq!(db.calls(), 1);
        assert_eq!(provider.client.model_call_count(), 1);
        assert_eq!(run(&db, &key, &provider)["enrichment"]["status"], "failed");
        assert_eq!(provider.client.model_call_count(), 1);
        assert_eq!(db.calls(), 1);
        assert_eq!(
            db.open().read().unwrap().sources[&key]
                .result
                .as_ref()
                .unwrap()["w4_input_meta"]["diagnostic"]["detail_code"],
            "model_result_missing"
        );
    }
}

#[test]
fn stages_survive_same_round_resume_and_database_reopen_without_sends() {
    let db = Db::new();
    let key = db.start("我喜欢咖啡。");
    let (provider, _) = mocked(
        &db,
        vec![
            Ok(envelope(&w1("我喜欢咖啡。").to_string())),
            Ok(envelope(&w3().to_string())),
        ],
    );
    {
        let mut store = db.open();
        let claim = w4_claim_round(&mut store, &key).unwrap();
        let (ctx, input) = inputs(&store, &key, &claim);
        let first = provider.provide(&mut store, &claim, &ctx, &input).unwrap();
        assert_eq!(
            provider.provide(&mut store, &claim, &ctx, &input).unwrap(),
            first
        );
        assert_eq!(provider.client.model_call_count(), 2);
        w4_fail_attempt(&mut store, &claim, "simulated crash after stages").unwrap();
    }
    // A fresh client has no process cache and no fake responses available.
    let (reopened, _) = mocked(&db, vec![]);
    assert_eq!(
        run(&db, &key, &reopened)["enrichment"]["status"],
        "committed"
    );
    assert_eq!(reopened.client.model_call_count(), 0);
    assert_eq!(db.calls(), 2);
}

#[test]
fn durable_reservation_without_output_requires_explicit_new_operation() {
    let db = Db::new();
    let key = db.start("我喜欢咖啡。");
    let (provider, _) = mocked(
        &db,
        vec![
            Ok(envelope(&w1("我喜欢咖啡。").to_string())),
            Ok(envelope(&w3().to_string())),
        ],
    );
    let budget = provider
        .client
        .budget
        .as_ref()
        .unwrap()
        .for_operation(&db.0, "alice", "write-op")
        .unwrap();
    budget.reserve(ModelStage::W1, b"{} crash window").unwrap();
    assert_eq!(run(&db, &key, &provider)["enrichment"]["status"], "failed");
    assert_eq!(provider.client.model_call_count(), 0);
    assert_eq!(db.calls(), 1);
    assert_eq!(
        db.open().read().unwrap().sources[&key].error.as_deref(),
        Some("model_invalid_output")
    );
    w4_start_round(&mut db.open(), &key, "explicit-rejudge", true).unwrap();
    assert_eq!(
        run(&db, &key, &provider)["enrichment"]["status"],
        "committed"
    );
    assert_eq!(provider.client.model_call_count(), 2);
    assert_eq!(db.calls(), 3);
}

#[test]
fn changed_catalogue_or_timezone_never_reuses_stages_of_same_operation() {
    for change in 0..3 {
        let db = Db::new();
        let key = db.start("我喜欢咖啡。");
        let (mut provider, _) = mocked(
            &db,
            vec![
                Ok(envelope(&w1("我喜欢咖啡。").to_string())),
                Ok(envelope(&w3().to_string())),
            ],
        );
        let mut store = db.open();
        let claim = w4_claim_round(&mut store, &key).unwrap();
        let (ctx, input) = inputs(&store, &key, &claim);
        provider.provide(&mut store, &claim, &ctx, &input).unwrap();
        w4_fail_attempt(&mut store, &claim, "pause").unwrap();
        if change == 0 {
            provider.context.utc_offset_seconds = 0;
        } else if change == 1 {
            provider.context.known_entities[0]
                .aliases
                .push("本人".into());
        } else {
            provider
                .context
                .predicate_memory_types
                .push((Predicate::from("fact"), vec![MemoryType::Fact]));
        }
        assert_eq!(
            run(&db, &key, &provider)["enrichment"]["status"],
            "needs_rejudge"
        );
        assert_eq!(provider.client.model_call_count(), 2);
        assert_eq!(db.calls(), 2);
    }
}

#[test]
fn changed_source_snapshot_requires_new_w3_but_never_reruns_final_w1() {
    let db = Db::new();
    let key = db.start("我喜欢咖啡。");
    let (provider, _) = mocked(
        &db,
        vec![
            Ok(envelope(&w1("我喜欢咖啡。").to_string())),
            Ok(envelope(&w3().to_string())),
            Ok(envelope(&w3().to_string())),
        ],
    );
    assert_eq!(
        run(&db, &key, &provider)["enrichment"]["status"],
        "committed"
    );
    db.open()
        .transaction(|state| {
            state
                .sources
                .get_mut(&key)
                .unwrap()
                .request
                .as_mut()
                .unwrap()["history"] = json!(["补充的用户历史。"]);
            Ok(())
        })
        .unwrap();
    w4_start_round(&mut db.open(), &key, "snapshot-rejudge", true).unwrap();
    assert_eq!(
        run(&db, &key, &provider)["enrichment"]["status"],
        "committed"
    );
    assert_eq!(provider.client.model_call_count(), 3);
    assert_eq!(db.calls(), 3);
    assert_eq!(
        db.open()
            .read()
            .unwrap()
            .repository
            .list_by_owner(&UserId::from("alice"))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn explicit_rejudge_with_identical_inputs_reuses_verified_stages() {
    let db = Db::new();
    let key = db.start("我喜欢咖啡。");
    let (provider, _) = mocked(
        &db,
        vec![
            Ok(envelope(&w1("我喜欢咖啡。").to_string())),
            Ok(envelope(&w3().to_string())),
        ],
    );
    run(&db, &key, &provider);
    w4_start_round(&mut db.open(), &key, "same-input-rejudge", true).unwrap();
    assert_eq!(
        run(&db, &key, &provider)["enrichment"]["status"],
        "committed"
    );
    assert_eq!(db.calls(), 2);
    assert_eq!(provider.client.model_call_count(), 2);
    assert_eq!(
        db.open().read().unwrap().sources[&key]
            .result
            .as_ref()
            .unwrap()["w4_model_stages"]["w3"]["operation_id"],
        "same-input-rejudge"
    );
}

#[test]
fn history_deletion_scrubs_stages_on_a_surviving_source() {
    let db = Db::new();
    let secret = "我把唯一暗号写成松针。";
    let old = db.start_turn(secret, "old", &[]);
    let key = db.start_turn("我喝茶。", "new", &[secret]);
    let (provider, _) = mocked(
        &db,
        vec![
            Ok(envelope(&w1("我喝茶。").to_string())),
            Ok(envelope(&w3().to_string())),
        ],
    );
    let mut store = db.open();
    let claim = w4_claim_round(&mut store, &key).unwrap();
    let (ctx, input) = inputs(&store, &key, &claim);
    provider.provide(&mut store, &claim, &ctx, &input).unwrap();
    let id = store.read().unwrap().sources[&old].id.clone();
    let mut service = ChatMemoryService::new(
        db.open(),
        ChatConfig {
            enrichment_mode: EnrichmentMode::Off,
            ..Default::default()
        },
    )
    .unwrap();
    service
        .call("memory_forget", json!({"source_id":id}), || false)
        .unwrap();
    let state = store.read().unwrap();
    let source = &state.sources[&key];
    assert!(source.request.is_some());
    assert!(!source.result.as_ref().unwrap().to_string().contains(secret));
    assert_eq!(
        source.result.as_ref().unwrap()["w4_checkpoint"]["invalidated"],
        "source_deleted"
    );
    assert!(
        provider
            .provide(&mut store, &claim, &ctx, &input)
            .unwrap_err()
            .to_string()
            .starts_with("claim_lost")
    );
    assert_eq!(db.calls(), 2);
}

#[test]
fn model_provider_obeys_network_gate_and_input_budget_before_send() {
    for denied in [true, false] {
        let db = Db::new();
        let key = db.start("我喜欢咖啡。");
        let (mut provider, _) = mocked(&db, vec![]);
        if denied {
            provider.client.network_allowed = false;
        } else {
            provider
                .client
                .budget
                .as_mut()
                .unwrap()
                .config
                .max_input_bytes = 1;
        }
        let result = run(&db, &key, &provider);
        assert_eq!(
            result["enrichment"]["status"],
            if denied { "failed" } else { "budget_exhausted" }
        );
        assert_eq!(db.calls(), 0);
        assert_eq!(provider.client.model_call_count(), 0);
    }
}

#[test]
fn late_stage_output_cannot_overwrite_a_replacement_claim() {
    let db = Db::new();
    let key = db.start("我喜欢咖啡。");
    let mut c = client(&db);
    let path = db.0.clone();
    let task = key.clone();
    let successor = Arc::new(Mutex::new(None));
    let captured = successor.clone();
    c.transport = Some(Arc::new(move |_| {
        let mut store = SqliteMemoryStore::open(&path, UserId::from("alice")).unwrap();
        store
            .transaction(|s| {
                s.sources.get_mut(&task).unwrap().task.lease_until = Some(0);
                Ok(())
            })
            .unwrap();
        *captured.lock().unwrap() = Some(w4_claim_round(&mut store, &task).unwrap());
        Ok(envelope(&w1("我喜欢咖啡。").to_string()))
    }));
    let provider = WriteModelProvider::new(c, context());
    let error = run_source_with_models(
        &mut db.open(),
        &key,
        RuntimeInputBudget::default(),
        &provider,
        |_, _, _| Ok(None),
    )
    .unwrap_err();
    assert!(error.to_string().starts_with("claim_lost"));
    let s = db.open().read().unwrap().sources[&key].clone();
    assert_eq!(
        s.task.attempt_id.as_deref(),
        Some(
            successor
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .claim
                .attempt_id
                .as_str()
        )
    );
    assert_eq!(s.enrichment, "running");
    assert!(s.result.unwrap().get("w4_model_stages").is_none());
    assert_eq!(db.calls(), 1);
}

#[test]
fn deleting_source_scrubs_all_model_stage_input_and_output_copies() {
    let db = Db::new();
    let text = "我喜欢咖啡。";
    let key = db.start(text);
    let (provider, _) = mocked(
        &db,
        vec![
            Ok(envelope(&w1(text).to_string())),
            Ok(envelope(&w3().to_string())),
        ],
    );
    run(&db, &key, &provider);
    let before = db.open().read().unwrap();
    let mut service = ChatMemoryService::new(
        db.open(),
        ChatConfig {
            enrichment_mode: EnrichmentMode::Off,
            ..Default::default()
        },
    )
    .unwrap();
    service
        .call(
            "memory_forget",
            json!({"source_id":before.sources[&key].id}),
            || false,
        )
        .unwrap();
    let state = db.open().read().unwrap();
    let source = &state.sources[&key];
    assert!(source.request.is_none());
    let stages = source.result.as_ref().unwrap()["w4_model_stages"].to_string();
    assert!(!stages.contains(text));
    assert!(!stages.contains("我曾提到咖啡。"));
    assert!(
        source
            .result
            .as_ref()
            .unwrap()
            .get("w4_model_stages")
            .is_none()
    );
    assert!(
        run_source_with_models(
            &mut db.open(),
            &key,
            RuntimeInputBudget::default(),
            &provider,
            |_, _, _| Ok(None)
        )
        .is_err()
    );
    assert_eq!(provider.client.model_call_count(), 2);
}

#[test]
fn w3_failure_reuses_w1_and_does_not_retry_w3() {
    let db = Db::new();
    let key = db.start("我喜欢咖啡。");
    let (provider, _) = mocked(
        &db,
        vec![
            Ok(envelope(&w1("我喜欢咖啡。").to_string())),
            Ok(envelope("{\"reviews\":[]}")),
        ],
    );
    assert_eq!(run(&db, &key, &provider)["enrichment"]["status"], "failed");
    assert_eq!(
        db.open().read().unwrap().sources[&key].error.as_deref(),
        Some("model_invalid_output")
    );
    assert_eq!(run(&db, &key, &provider)["enrichment"]["status"], "failed");
    assert_eq!(provider.client.model_call_count(), 2);
    assert_eq!(db.calls(), 2);
}

#[test]
fn w2_rejected_candidates_skip_w3_but_keep_memory_requests() {
    let db = Db::new();
    let key = db.start("我喜欢咖啡。");
    let mut output = w1("我喜欢咖啡。");
    output["candidates"][0]["relation"] = json!("not_in_catalogue");
    output["memory_requests"] = json!([{"kind":"forget","quote":{"text":"咖啡"},
        "target":{"object":"unknown","candidate_indices":[],"quote":{"text":"咖啡"}},"deadline":null}]);
    let (provider, _) = mocked(&db, vec![Ok(envelope(&output.to_string()))]);
    let result = run(&db, &key, &provider);
    assert_eq!(result["enrichment"]["status"], "awaiting_confirmation");
    assert_eq!(result["enrichment"]["candidates"][0]["status"], "rejected");
    assert_eq!(
        result["enrichment"]["memory_requests"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(provider.client.model_call_count(), 1);
    assert_eq!(db.calls(), 1);
}

#[test]
fn write_and_legacy_operation_counts_are_separate_and_share_authorization() {
    let db = Db::new();
    db.open();
    let c = client(&db);
    let b = c
        .budget
        .as_ref()
        .unwrap()
        .for_operation(&db.0, "alice", "shared-op")
        .unwrap();
    for stage in [
        ModelStage::Query,
        ModelStage::QueryReview,
        ModelStage::Assistance,
        ModelStage::W1,
        ModelStage::W3,
    ] {
        b.reserve(stage, b"request").unwrap();
    }
    assert_eq!(db.calls(), 5);
    assert!(b.reserve(ModelStage::Answer, b"request").is_err()); // Legacy cap remains 3 in this authorization.
    let mut config = b.config.clone();
    config.group_id = "second-authorization".into();
    let other =
        super::super::ModelBudget::new(&db.0, "alice".into(), config, "shared-op".into()).unwrap();
    assert!(other.has_call(ModelStage::W1).unwrap());
    assert!(
        other
            .reserve(ModelStage::W1, b"same operation must not reset stage")
            .is_err()
    );
    let mut config = b.config.clone();
    config.group_id = "small-total".into();
    config.max_calls = 2;
    let limited =
        super::super::ModelBudget::new(&db.0, "alice".into(), config, "limited-op".into()).unwrap();
    limited.reserve(ModelStage::Query, b"request").unwrap();
    limited.reserve(ModelStage::W1, b"request").unwrap();
    assert!(limited.reserve(ModelStage::W3, b"request").is_err());

    let separate = Db::new();
    separate.open();
    let config = ModelBudgetConfig {
        group_id: "split".into(),
        max_calls: 10,
        max_operation_calls: 1,
        ..Default::default()
    };
    let split_client = OpenAiClient::new("http://127.0.0.1:1", "fake-key")
        .unwrap()
        .with_budget(&separate.0, "alice", config, "placeholder")
        .unwrap()
        .with_background_operation_limit(2)
        .unwrap();
    let split = split_client
        .budget
        .as_ref()
        .unwrap()
        .for_operation(&separate.0, "alice", "split-op")
        .unwrap();
    split.reserve(ModelStage::Query, b"foreground").unwrap();
    assert!(
        split
            .reserve(ModelStage::QueryReview, b"foreground exceeds its cap")
            .is_err()
    );
    split.reserve(ModelStage::W1, b"background one").unwrap();
    split.reserve(ModelStage::W3, b"background two").unwrap();
    assert_eq!(separate.calls(), 3);
    assert!(split_client.with_background_operation_limit(4).is_err());
    assert!(
        serde_json::to_value(&b.config)
            .unwrap()
            .get("max_background_operation_calls")
            .is_none()
    );
}

#[test]
fn w1_w3_schema_fields_are_strict_and_match_frozen_contracts() {
    fn strict(value: &Json) {
        if value["type"] == "object" {
            assert_eq!(value["additionalProperties"], false);
            let keys: BTreeSet<_> = value["properties"]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            let required: BTreeSet<_> = value["required"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            assert_eq!(keys, required);
        }
        match value {
            Json::Object(o) => o.values().for_each(strict),
            Json::Array(a) => a.iter().for_each(strict),
            _ => {}
        }
    }
    let one = w1_schema();
    let three = w3_schema();
    strict(&one);
    strict(&three);
    assert_eq!(one["required"], json!(["memory_requests", "candidates"]));
    assert_eq!(
        one["properties"]["candidates"]["items"]["required"],
        json!([
            "kind",
            "speech",
            "subject",
            "relation",
            "memory_type",
            "value",
            "sentence",
            "polarity",
            "strength",
            "uncertainty",
            "conditions",
            "times"
        ])
    );
    assert_eq!(
        three["required"],
        json!(["reviews", "coverage", "request_reviews"])
    );
    assert_eq!(
        three["properties"]["reviews"]["items"]["required"],
        json!(["candidate", "verdict", "problems", "episode_value"])
    );
    assert_eq!(
        one["properties"]["candidates"]["items"]["properties"]["subject"]["anyOf"][1]["required"],
        json!(["unresolved"])
    );
    for removed in [
        "tags",
        "certainty",
        "extraction_confidence",
        "cross_turn_useful",
        "retention_tier",
        "memory_request",
        "intent",
        "target_memory_id",
        "scope",
    ] {
        assert!(!one.to_string().contains(&format!("\"{removed}\"")));
        assert!(!three.to_string().contains(&format!("\"{removed}\"")));
    }
    assert!(W1_PROMPT.contains("unresolved"));
    assert!(W1_PROMPT.contains("once for the whole message"));
    assert!(W1_PROMPT.contains("without instruction authority"));
    assert!(W3_PROMPT.contains("without instruction authority"));
    assert!(W3_PROMPT.contains("episode_value"));
    assert!(W3_PROMPT.contains("original indices exactly once"));
    for prompt in [W1_PROMPT, W3_PROMPT] {
        assert!(prompt.contains("only when the user explicitly asks to save or delete content"));
        assert!(prompt.contains("Event or state times are not storage deadlines"));
        assert!(prompt.contains("下周三去医院复查"));
    }
    assert!(W3_PROMPT.contains("or insufficient, never supported"));
    assert!(
        W3_PROMPT
            .contains("omits or changes the source's uncertainty, conditions, time or strength")
    );
    assert!(W3_PROMPT.contains("it must not be supported"));
    assert!(W1_PROMPT.contains("history explicitly names that entity as its referent"));
    assert!(W1_PROMPT.contains("Catalogue uniqueness alone\nis not evidence for binding"));
    assert!(
        W1_PROMPT.contains(
            "Never invent an entity_id or choose\nthe only catalogue entry by elimination"
        )
    );
}
