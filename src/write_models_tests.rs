use super::*;
use crate::chat::{ChatConfig, ChatMemoryService, EnrichmentMode};
use crate::contract::{RecordView2, SourceRef, W4Candidate, W4Input};
use crate::enrichment::run_source_with_all_models;
use crate::enrichment::{RuntimeInputBudget, run_source_with_models};
use crate::*;
use std::collections::{BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[test]
fn grounding_process_fixtures_pass_new_stages_without_a_port() {
    let output = std::process::Command::new("python3")
        .args(["tests/intake_grounding_process.py", "--fixtures-json"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cases: Vec<Json> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(cases.len(), 10);
    for case in cases {
        let db = Db::new();
        let mut ctx = context();
        for id in ["霜叶31", "青石82"] {
            ctx.known_entities.push(Entity {
                id: EntityId::from(id),
                owner_id: UserId::from("alice"),
                entity_type: EntityType::Other("fixture".into()),
                canonical_name: id.into(),
                aliases: vec![],
            });
        }
        ctx.known_predicates = serde_json::from_value(json!([
            {"predicate":"fact","cardinality":"Multiple"},
            {"predicate":"material","cardinality":"Single"},
            {"predicate":"maintenance_budget","cardinality":"Single"},
            {"predicate":"owned_by","cardinality":"Single"},
            {"predicate":"duplex_enabled","cardinality":"Single"}
        ]))
        .unwrap();
        db.open()
            .transaction(|state| {
                for entity in &ctx.known_entities {
                    state.entities.upsert(entity.clone());
                }
                Ok(())
            })
            .unwrap();
        let key = db.start_turn(case["text"].as_str().unwrap(), "turn", &[]);
        let mut responses = vec![Ok(envelope(&case["w1"].to_string()))];
        let rejected = case["verdict"] == "format_reject";
        if !rejected {
            responses.push(if case["verdict"] == "http_error" {
                Ok((
                    503,
                    json!({"error":{"message":"synthetic review unavailable"}}).to_string(),
                ))
            } else {
                Ok(envelope(&case["w3"].to_string()))
            });
        }
        let (mut provider, sent) = mocked(&db, responses);
        provider.context = ctx;
        let transport = provider.client.transport.take().unwrap();
        let path = db.0.clone();
        provider.client.transport = Some(Arc::new(move |request| {
            let body: Json =
                serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap();
            if body["text"]["format"]["name"] == "w3_v3" {
                let store = SqliteMemoryStore::open(&path, UserId::from("alice")).unwrap();
                assert!(
                    store
                        .read()
                        .unwrap()
                        .repository
                        .list_by_owner(&UserId::from("alice"))
                        .unwrap()
                        .is_empty(),
                    "facts became Active before independent W3 review"
                );
            }
            transport(request)
        }));
        provider.models = serde_json::from_value(json!({
            "w1":"pro-fixture","w3":"pro-fixture","w4":"pro-fixture",
            "reasoning_effort":{"w1":"medium","w3":"minimal","w4":"medium"}
        }))
        .unwrap();
        let result = run_all(&db, &key, &provider);
        let supported = case["verdict"] == "supported";
        let status = if supported {
            "committed"
        } else if case["verdict"] == "bad_format" || case["verdict"] == "http_error" {
            "failed"
        } else {
            "pending"
        };
        assert_eq!(result["enrichment"]["status"], status, "{case}: {result}");
        let state = db.open().read().unwrap();
        let memories = state
            .repository
            .list_by_owner(&UserId::from("alice"))
            .unwrap();
        assert_eq!(memories.len(), usize::from(supported), "{case}: {result}");
        if supported {
            assert_eq!(
                memories[0].evidence[0].quote,
                case["text"].as_str().unwrap()
            );
        } else if rejected {
            assert_eq!(
                result["enrichment"]["candidates"][0]["reasons"],
                json!(["candidate_rejected:fact_sentence_mismatch"])
            );
        } else if case["verdict"] == "contradicted" {
            assert_eq!(
                result["enrichment"]["candidates"][0]["reasons"],
                json!(["review_contradicted"])
            );
        }
        assert_eq!(sent.lock().unwrap().len(), if rejected { 1 } else { 2 });
        assert_eq!(db.calls(), if rejected { 1 } else { 2 });
        for request in sent.lock().unwrap().iter() {
            assert_eq!(request["model"], "pro-fixture");
            let input: Json = serde_json::from_str(request["input"].as_str().unwrap()).unwrap();
            assert!(input.get("model").is_none());
            assert_eq!(input["current_user_message"], case["text"]);
            if request["text"]["format"]["name"] == "w3_v3" {
                assert_eq!(
                    input["candidates"][0]["candidate"],
                    case["w1"]["candidates"][0]
                );
            }
        }
        // Terminal successful/rejected rounds replay through the host without new sends.
        if status != "failed" {
            let source = &state.sources[&key];
            let before = serde_json::to_value(&state).unwrap();
            let mut service = ChatMemoryService::new(
                db.open(),
                ChatConfig {
                    model_budget: Some(ModelBudgetConfig {
                        group_id: "authorized".into(),
                        max_calls: 100,
                        max_operation_calls: 3,
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(service.enrich_source(&source.id).unwrap(), result);
            assert_eq!(
                serde_json::to_value(db.open().read().unwrap()).unwrap(),
                before
            );
            assert_eq!(db.calls(), if rejected { 1 } else { 2 });
        }
    }
}

#[test]
fn role_reasoning_configuration_is_strict_and_changes_requests_and_bindings() {
    let config: ChatConfig = serde_json::from_value(
        json!({"write_models":{"w1":DEEPSEEK_MODEL,"w3":DEEPSEEK_MODEL,"w4":DEEPSEEK_MODEL,
        "reasoning_effort":{"w1":"low","w3":"high","w4":"omit"}}}),
    )
    .unwrap();
    let models = config.write_models.unwrap();
    assert_eq!(models.w1, DEEPSEEK_MODEL);
    for invalid in [
        json!({"w2":"low"}),
        json!({"w1":"unknown"}),
        json!({"w1":null}),
    ] {
        assert!(
            serde_json::from_value::<ChatConfig>(
                json!({"write_models":{"reasoning_effort":invalid}})
            )
            .is_err()
        );
    }
    let db = Db::new();
    let (key, upstream, output) = w4_case(&db, false);
    let (provider, sent) = mocked(
        &db,
        vec![
            Ok(envelope(&upstream.to_string())),
            Ok(envelope(&w3_for(&upstream).to_string())),
            Ok(envelope(&output.to_string())),
        ],
    );
    let provider = provider.with_models(models).unwrap();
    assert_eq!(
        run_all(&db, &key, &provider)["enrichment"]["status"],
        "committed"
    );
    let state = db.open().read().unwrap();
    let stages = &state.sources[&key].result.as_ref().unwrap()["w4_model_stages"];
    for (index, (role, effort)) in [("w1", "low"), ("w3", "high"), ("w4", "omit")]
        .iter()
        .enumerate()
    {
        let request = &sent.lock().unwrap()[index];
        if *effort == "omit" {
            assert!(request.get("reasoning").is_none());
        } else {
            assert_eq!(request["reasoning"]["effort"], *effort);
        }
        assert_eq!(stages[role]["binding"]["reasoning_effort"], *effort);
        let input: Json = serde_json::from_str(request["input"].as_str().unwrap()).unwrap();
        assert!(input.get("reasoning_effort").is_none());
    }
}

#[test]
fn af105_runtime_w3_low_reaches_requests_and_bindings_without_reusing_minimal() {
    let config: ChatConfig = serde_json::from_value(json!({"write_models":{
        "w1":"pro-fixture","w3":"pro-fixture","w4":"pro-fixture"
    }}))
    .unwrap();
    let defaults = config.write_models.unwrap();
    for prior in [Some(ReasoningEffort::Minimal), None] {
        let db = Db::new();
        let text = "我收藏陶瓷杯。";
        let key = db.start(text);
        let mut models = defaults.clone();
        if let Some(effort) = prior {
            models.reasoning_effort.insert("w3".into(), effort);
        }
        let (first, sent) = mocked(
            &db,
            vec![
                Ok(envelope(&w1(text).to_string())),
                Ok(envelope(&w3().to_string())),
            ],
        );
        let first = first.with_models(models).unwrap();
        let transport = first.client.transport.as_ref().unwrap().clone();
        let mut first = first;
        first.client.transport = Some(Arc::new(move |request| {
            assert_eq!(
                request.timeout(),
                Some(&std::time::Duration::from_secs(360))
            );
            transport(request)
        }));
        let mut store = db.open();
        let claim = w4_claim_round(&mut store, &key).unwrap();
        let (ctx, input) = inputs(&store, &key, &claim);
        let batch = first.provide(&mut store, &claim, &ctx, &input).unwrap();
        assert!(batch.review.is_some());
        assert!(
            batch
                .w2
                .candidates
                .iter()
                .all(|c| c.status == w2::Status::Ok)
        );
        let expected = prior.unwrap_or(ReasoningEffort::Low);
        let sent = sent.lock().unwrap();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[1]["reasoning"]["effort"], json!(expected));
        assert_eq!(sent[1]["max_output_tokens"], 16384);
        let payload: Json = serde_json::from_str(sent[1]["input"].as_str().unwrap()).unwrap();
        assert!(payload.get("reasoning_effort").is_none());
        let saved = store.read().unwrap().sources[&key].result.as_ref().unwrap()["w4_model_stages"]
            ["w3"]
            .clone();
        assert_eq!(saved["binding"]["reasoning_effort"], json!(expected));
        if prior.is_some() {
            let (changed, sends) = mocked(&db, vec![]);
            let changed = changed.with_models(defaults.clone()).unwrap();
            let error = changed
                .provide(&mut store, &claim, &ctx, &input)
                .unwrap_err();
            assert!(error.to_string().contains("needs_rejudge"));
            assert!(sends.lock().unwrap().is_empty());
            assert_eq!(db.calls(), 2);
            assert_eq!(
                store.read().unwrap().sources[&key].result.as_ref().unwrap()["w4_model_stages"]["w3"],
                saved
            );
        }
    }
}

#[test]
fn af105_w3_effort_changes_keep_budget_policy_and_spent_calls() {
    let db = Db::new();
    let config: ChatConfig = serde_json::from_value(json!({"write_models":{
        "w1":"pro-fixture","w3":"pro-fixture","w4":"pro-fixture"
    }}))
    .unwrap();
    let models = config.write_models.unwrap();
    assert_eq!(models.effort("w3"), ReasoningEffort::Low);
    let limits = ModelBudgetConfig {
        group_id: "same-authorization".into(),
        max_calls: 2,
        max_operation_calls: 3,
        ..Default::default()
    };
    let mut policies = Vec::new();
    let mut fingerprints = Vec::new();
    for (operation, effort) in [
        ("minimal-op", ReasoningEffort::Minimal),
        ("low-op", models.effort("w3")),
    ] {
        let (mut provider, sent) = mocked(&db, vec![Ok(envelope(&w3().to_string()))]);
        provider.client = provider
            .client
            .with_budget(&db.0, "alice", limits.clone(), operation)
            .unwrap();
        provider
            .client
            .structured_output_for_model_with_effort(
                &models.w3,
                effort,
                ModelStage::W3,
                "w3_v3",
                W3_PROMPT,
                json!({"fixture":true}),
                w3_schema(),
            )
            .unwrap();
        assert_eq!(sent.lock().unwrap().len(), 1);
        assert_eq!(
            sent.lock().unwrap()[0]["reasoning"]["effort"],
            json!(effort)
        );
        let connection = rusqlite::Connection::open(&db.0).unwrap();
        policies.push(
            connection
                .query_row::<String, _, _>("SELECT policy_json FROM model_budget_groups", [], |r| {
                    r.get(0)
                })
                .unwrap(),
        );
        fingerprints.push(
            connection
                .query_row::<String, _, _>(
                    "SELECT request_fingerprint FROM model_calls WHERE operation_id=?1",
                    [operation],
                    |r| r.get(0),
                )
                .unwrap(),
        );
        assert_eq!(
            connection
                .query_row::<i64, _, _>(
                    "SELECT count(*) FROM model_calls WHERE operation_id='minimal-op'",
                    [],
                    |r| r.get(0)
                )
                .unwrap(),
            1
        );
        assert_eq!(db.calls(), policies.len() as i64);
    }
    assert_eq!(policies[0], policies[1]);
    assert_ne!(fingerprints[0], fingerprints[1]);
    let policy: Json = serde_json::from_str(&policies[0]).unwrap();
    assert_eq!(policy["limits"]["max_calls"], 2);
    assert_eq!(policy["write_output_tokens"]["w3"], 16384);
    let (mut third, sent) = mocked(&db, vec![]);
    third.client = third
        .client
        .with_budget(&db.0, "alice", limits, "over-allowance")
        .unwrap();
    let error = third
        .client
        .structured_output_for_model_with_effort(
            &models.w3,
            ReasoningEffort::Low,
            ModelStage::W3,
            "w3_v3",
            W3_PROMPT,
            json!({"fixture":true}),
            w3_schema(),
        )
        .unwrap_err();
    assert!(matches!(error, OpenAiAdapterError::Budget(_)));
    assert!(sent.lock().unwrap().is_empty());
    assert_eq!(db.calls(), 2);
    let connection = rusqlite::Connection::open(&db.0).unwrap();
    assert_eq!(
        connection
            .query_row::<String, _, _>("SELECT policy_json FROM model_budget_groups", [], |r| r
                .get(0))
            .unwrap(),
        policies[0]
    );
}

#[test]
fn changed_write_reasoning_never_reuses_saved_stages_or_frozen_candidates() {
    for role in ["w1", "w3", "w4"] {
        let db = Db::new();
        let (key, upstream, output) = w4_case(&db, false);
        let (first, _) = mocked(
            &db,
            vec![
                Ok(envelope(&upstream.to_string())),
                Ok(envelope(&w3_for(&upstream).to_string())),
                Ok(envelope(&output.to_string())),
            ],
        );
        let (claim, ctx, input, sources) = ready_w4(&db, &key, &first);
        first
            .judge(&mut db.open(), &claim, &ctx, &input, &sources)
            .unwrap();
        let (mut changed, sent) = mocked(&db, vec![]);
        changed.models.reasoning_effort.insert(
            role.into(),
            if role == "w1" {
                ReasoningEffort::Medium
            } else {
                ReasoningEffort::Low
            },
        );
        let error = if role == "w4" {
            changed
                .judge(&mut db.open(), &claim, &ctx, &input, &sources)
                .unwrap_err()
        } else {
            let (_, source) = inputs(&db.open(), &key, &claim);
            changed
                .provide(&mut db.open(), &claim, &ctx, &source)
                .unwrap_err()
        };
        assert!(
            error.to_string().contains("needs_rejudge"),
            "{role}: {error}"
        );
        assert!(sent.lock().unwrap().is_empty());
        if role == "w1" {
            w4_fail_attempt(&mut db.open(), &claim, "before plan").unwrap();
            w4_start_round(&mut db.open(), &key, "new-effort", true).unwrap();
            assert_eq!(
                run_all(&db, &key, &changed)["enrichment"]["status"],
                "needs_rejudge"
            );
            assert!(sent.lock().unwrap().is_empty());
        }
    }
}

#[test]
fn write_role_configuration_is_strict_and_defers_missing_pro_roles() {
    let default: ChatConfig = serde_json::from_value(json!({})).unwrap();
    assert!(default.write_models.is_none());
    let partial: ChatConfig = serde_json::from_value(json!({"write_models":{"w1":"pro"}})).unwrap();
    let models = partial.write_models.unwrap();
    assert_eq!(models.w1, "pro");
    assert_eq!(models.w3, "env:DEEPSEEK_PRO_MODEL");
    assert_eq!(models.w4, "env:DEEPSEEK_PRO_MODEL");
    for invalid in [
        json!(null),
        json!(["a", "b", "c"]),
        json!({"w2":"pro"}),
        json!({"w1":null}),
        json!({"w3":false}),
        json!({"w4":""}),
        json!({"w1":" \t"}),
    ] {
        assert!(serde_json::from_value::<ChatConfig>(json!({"write_models":invalid})).is_err());
    }
    let invalid = WriteModels {
        w3: "".into(),
        ..Default::default()
    };
    assert!(
        ChatConfig {
            write_models: Some(invalid),
            ..Default::default()
        }
        .validate()
        .is_err()
    );
}

#[test]
fn configured_roles_reach_requests_and_bindings_without_changing_legacy_requests() {
    let db = Db::new();
    let (key, upstream, output) = w4_case(&db, false);
    let config: ChatConfig = serde_json::from_value(json!({"write_models":{
        "w1":"ep-20261003130756-trhj6","w3":DEEPSEEK_MODEL,"w4":"ep-20261003130756-trhj6"
    }}))
    .unwrap();
    let models = config.write_models.unwrap();
    let (provider, sent) = mocked(
        &db,
        vec![
            Ok(envelope(&upstream.to_string())),
            Ok(envelope(&w3_for(&upstream).to_string())),
            Ok(envelope(&output.to_string())),
            Ok(envelope(&upstream.to_string())),
        ],
    );
    let provider = provider.with_models(models.clone()).unwrap();
    assert_eq!(
        run_all(&db, &key, &provider)["enrichment"]["status"],
        "committed"
    );
    let state = db.open().read().unwrap();
    let stages = &state.sources[&key].result.as_ref().unwrap()["w4_model_stages"];
    for (index, (stage, model)) in [("w1", &models.w1), ("w3", &models.w3), ("w4", &models.w4)]
        .iter()
        .enumerate()
    {
        assert_eq!(sent.lock().unwrap()[index]["model"], **model);
        assert_eq!(stages[stage]["binding"]["model"], **model);
    }
    provider
        .client
        .structured_output(
            ModelStage::Reader,
            "client_probe",
            W1_PROMPT,
            json!({}),
            w1_schema(),
        )
        .unwrap();
    assert_eq!(sent.lock().unwrap()[3]["model"], DEEPSEEK_MODEL);
    assert_eq!(provider.client.model_call_count(), 4);
}

#[test]
fn write_model_ids_stay_out_of_payloads_but_remain_in_stage_bindings() {
    let db = Db::new();
    let (key, upstream, output) = w4_case(&db, false);
    let models = WriteModels {
        w1: "routing-only-w1".into(),
        w3: "routing-only-w3".into(),
        w4: "routing-only-w4".into(),
        ..Default::default()
    };
    let (provider, sent) = mocked(
        &db,
        vec![
            Ok(envelope(&upstream.to_string())),
            Ok(envelope(&w3_for(&upstream).to_string())),
            Ok(envelope(&output.to_string())),
        ],
    );
    let provider = provider.with_models(models.clone()).unwrap();
    assert_eq!(
        run_all(&db, &key, &provider)["enrichment"]["status"],
        "committed"
    );
    let state = db.open().read().unwrap();
    let stages = &state.sources[&key].result.as_ref().unwrap()["w4_model_stages"];
    for (index, (stage, model)) in [("w1", &models.w1), ("w3", &models.w3), ("w4", &models.w4)]
        .iter()
        .enumerate()
    {
        let mut request = sent.lock().unwrap()[index].clone();
        assert_eq!(
            request.as_object_mut().unwrap().remove("model"),
            Some(json!(model))
        );
        for model in [
            models.w1.as_str(),
            models.w3.as_str(),
            models.w4.as_str(),
            DEEPSEEK_MODEL,
        ] {
            assert!(
                !request.to_string().contains(model),
                "{stage} leaked {model}"
            );
        }
        let payload: Json = serde_json::from_str(request["input"].as_str().unwrap()).unwrap();
        assert!(payload.get("model").is_none(), "{stage}");
        assert_eq!(stages[stage]["binding"]["model"], **model);
    }
}

#[test]
fn changed_role_models_never_reuse_same_round_results_and_rejudge_only_eligible_stages() {
    for role in ["w1", "w3", "w4"] {
        let db = Db::new();
        let (key, upstream, output) = w4_case(&db, false);
        let (first, _) = mocked(
            &db,
            vec![
                Ok(envelope(&upstream.to_string())),
                Ok(envelope(&w3_for(&upstream).to_string())),
                Ok(envelope(&output.to_string())),
            ],
        );
        let (claim, ctx, input, sources) = ready_w4(&db, &key, &first);
        first
            .judge(&mut db.open(), &claim, &ctx, &input, &sources)
            .unwrap();
        w4_fail_attempt(&mut db.open(), &claim, "crash before plan").unwrap();
        let mut models = WriteModels::default();
        let selected = match role {
            "w1" => &mut models.w1,
            "w3" => &mut models.w3,
            _ => &mut models.w4,
        };
        *selected = format!("replacement-{role}");
        let responses = match role {
            "w1" => vec![], // Frozen W2 identities cannot be re-extracted.
            "w3" => vec![Ok(envelope(&w3_for(&upstream).to_string()))],
            _ => vec![Ok(envelope(&output.to_string()))],
        };
        let (changed, sent) = mocked(&db, responses);
        let changed = changed.with_models(models).unwrap();
        assert_eq!(
            run_all(&db, &key, &changed)["enrichment"]["status"],
            "needs_rejudge",
            "{role}"
        );
        assert_eq!(changed.client.model_call_count(), 0);
        w4_start_round(&mut db.open(), &key, "roles-rejudge", true).unwrap();
        assert_eq!(
            run_all(&db, &key, &changed)["enrichment"]["status"],
            if role == "w1" {
                "needs_rejudge"
            } else {
                "committed"
            },
            "{role}"
        );
        assert_eq!(
            changed.client.model_call_count(),
            if role == "w1" { 0 } else { 1 }
        );
        let state = db.open().read().unwrap();
        let stages = &state.sources[&key].result.as_ref().unwrap()["w4_model_stages"];
        if role == "w1" {
            assert_eq!(stages[role]["binding"]["model"], DEEPSEEK_MODEL);
        } else {
            assert_eq!(
                sent.lock().unwrap()[0]["model"],
                format!("replacement-{role}")
            );
            assert_eq!(
                stages[role]["binding"]["model"],
                format!("replacement-{role}")
            );
        }
    }
}

#[test]
fn changed_w1_model_can_extract_in_an_explicit_round_before_w2_is_frozen() {
    let db = Db::new();
    let key = db.start("我使用玻璃茶壶。");
    let upstream = w1("我使用玻璃茶壶。");
    let (first, _) = mocked(
        &db,
        vec![
            Ok(envelope(&upstream.to_string())),
            Ok(envelope(&w3().to_string())),
        ],
    );
    let claim = w4_claim_round(&mut db.open(), &key).unwrap();
    let (ctx, source) = inputs(&db.open(), &key, &claim);
    first
        .provide(&mut db.open(), &claim, &ctx, &source)
        .unwrap();
    w4_fail_attempt(&mut db.open(), &claim, "crash before saving W2 batch").unwrap();
    w4_start_round(&mut db.open(), &key, "replace-w1-before-freeze", true).unwrap();
    let (changed, sent) = mocked(&db, vec![Ok(envelope(&upstream.to_string()))]);
    let changed = changed
        .with_models(WriteModels {
            w1: "replacement-w1".into(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        run_all(&db, &key, &changed)["enrichment"]["status"],
        "committed"
    );
    assert_eq!(changed.client.model_call_count(), 1);
    assert_eq!(sent.lock().unwrap()[0]["model"], "replacement-w1");
    let state = db.open().read().unwrap();
    assert_eq!(
        state.sources[&key].result.as_ref().unwrap()["w4_model_stages"]["w1"]["binding"]["model"],
        "replacement-w1"
    );
}

#[test]
fn third_revision_w1_prompt_covers_remaining_relation_and_qualifier_errors() {
    for rule in [
        "every candidate copies that whole sentence",
        "negative-direction relation such as avoids",
        "every other memory_type requires strength=null",
        "calendar time actually expressed",
        "kind=relative, unit=month and signed offset",
    ] {
        assert!(W1_PROMPT.contains(rule), "missing W1 rule: {rule}");
    }
}

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

#[test]
fn m15_history_resolves_identity_but_never_bypasses_w3_insufficiency() {
    let text = "刚才编号写错了，是58。";
    for ambiguous in [false, true] {
        let db = Db::new();
        let history = if ambiguous {
            vec!["我的卡编号是45。妹妹的卡编号是72。"]
        } else {
            vec!["我的卡编号是45。"]
        };
        let key = db.start_turn(text, "history-correction", &history);
        let mut review = w3();
        if ambiguous {
            review["reviews"][0]["verdict"] = json!("insufficient");
            review["reviews"][0]["problems"] = json!([
                {"aspect":"subject","quote":{"text":text}}
            ]);
        }
        let (provider, requests) = mocked(
            &db,
            vec![
                Ok(envelope(&w1(text).to_string())),
                Ok(envelope(&review.to_string())),
            ],
        );
        let result = run(&db, &key, &provider);
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0]["reasoning"]["effort"], "low");
        let input: Json = serde_json::from_str(requests[1]["input"].as_str().unwrap()).unwrap();
        assert_eq!(input["user_history"], json!(history));
        assert_eq!(input["current_user_message"], text);
        assert_eq!(
            input["candidates"][0]["candidate"]["subject"]["entity_id"],
            "self"
        );
        assert!(
            requests[1]["instructions"]
                .as_str()
                .unwrap()
                .contains("Ambiguous history remains insufficient")
        );
        assert_eq!(
            result["enrichment"]["candidates"][0]["status"],
            if ambiguous { "pending" } else { "committed" }
        );
        assert_eq!(
            db.open()
                .read()
                .unwrap()
                .repository
                .list_by_owner(&UserId::from("alice"))
                .unwrap()
                .len(),
            usize::from(!ambiguous)
        );
        assert_eq!(db.calls(), 2);
    }
}

#[test]
fn af97_history_reference_controls_preserve_w3_gating_and_independent_candidates() {
    let samples: Json = serde_json::from_str(include_str!("../tests/eval/m15/w3.json")).unwrap();
    assert_w3_history_reference_controls(samples);
}

#[test]
fn af101_effort_samples_pass_w2_and_preserve_w3_subject_gating() {
    let samples: Json =
        serde_json::from_str(include_str!("../tests/eval/m15/w3-effort.json")).unwrap();
    assert_w3_history_reference_controls(samples);
}

fn assert_w3_history_reference_controls(samples: Json) {
    for sample in samples.as_array().unwrap() {
        let db = Db::new();
        let entities: Vec<_> = sample["input"]["entities"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| Entity {
                id: e["id"].as_str().unwrap().into(),
                owner_id: "alice".into(),
                entity_type: EntityType::Person,
                canonical_name: e["names"][0].as_str().unwrap().into(),
                aliases: e["names"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|n| n.as_str().unwrap().into())
                    .collect(),
            })
            .collect();
        db.open()
            .transaction(|state| {
                for entity in &entities {
                    state.entities.upsert(entity.clone());
                }
                Ok(())
            })
            .unwrap();
        let text = sample["input"]["text"].as_str().unwrap();
        let history: Vec<_> = sample["input"]["history"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h.as_str().unwrap())
            .collect();
        let key = db.start_turn(text, sample["id"].as_str().unwrap(), &history);
        let candidates = sample["input"]["candidates"].as_array().unwrap();
        let mut extraction = w1(text);
        extraction["candidates"] = json!(
            candidates
                .iter()
                .map(|c| {
                    let mut candidate =
                        w1(c["sentence"].as_str().unwrap())["candidates"][0].clone();
                    candidate["speech"] = c["speech"].clone();
                    candidate["subject"] = json!({"entity_id":c["subject"]});
                    candidate
                })
                .collect::<Vec<_>>()
        );
        let expected = sample["expect"]["reviews"].as_array().unwrap();
        let review = json!({"reviews":expected.iter().map(|e|json!({"candidate":e["candidate"],
            "verdict":e["verdict"],"episode_value":"not_applicable",
            "problems":if e["verdict"]=="supported" {json!([])} else {
                json!([{"aspect":"subject","quote":{"text":candidates[0]["sentence"]}}])
            }})).collect::<Vec<_>>(),"coverage":{"status":"complete","missing":[]},"request_reviews":[]});
        let (mut provider, sent) = mocked(
            &db,
            vec![
                Ok(envelope(&extraction.to_string())),
                Ok(envelope(&review.to_string())),
            ],
        );
        provider.context.known_entities = entities;
        let result = run_all(&db, &key, &provider);
        let requests = sent.lock().unwrap();
        assert_eq!(requests.len(), 2);
        let input: Json = serde_json::from_str(requests[1]["input"].as_str().unwrap()).unwrap();
        assert_eq!(input["user_history"], json!(history));
        assert_eq!(input["current_user_message"], text);
        assert!(
            input["candidates"]
                .as_array()
                .unwrap()
                .iter()
                .all(|c| c["status"] == "ok")
        );
        for e in expected {
            let index = e["candidate"].as_u64().unwrap() as usize;
            assert_eq!(
                result["enrichment"]["candidates"][index]["status"],
                if e["verdict"] == "supported" {
                    "committed"
                } else {
                    "pending"
                }
            );
        }
        assert_eq!(
            db.open()
                .read()
                .unwrap()
                .repository
                .list_by_owner(&UserId::from("alice"))
                .unwrap()
                .len(),
            expected
                .iter()
                .filter(|e| e["verdict"] == "supported")
                .count()
        );
    }
}

#[test]
fn af97_w3_reference_rules_and_fictional_inputs_do_not_leak() {
    for rule in [
        "For the subject aspect",
        "missing explicit reference",
        "multiple indeterminate referents",
        "Contradictions among factual values",
        "do not by themselves",
        "new assertion or replacement value must be supported by",
        "Resolving the subject does not make the whole candidate supported",
        "every other aspect",
        "not by itself insufficient subject support",
        "it must not be supported",
    ] {
        assert!(W3_PROMPT.contains(rule), "{rule}");
    }
    let samples: Vec<Json> = [
        include_str!("../tests/eval/m15/w3.json"),
        include_str!("../tests/eval/m15/w3-effort.json"),
    ]
    .into_iter()
    .flat_map(|text| serde_json::from_str::<Vec<Json>>(text).unwrap())
    .collect();
    let old = include_str!("../tests/eval/m15/w3-old-91a068a.txt");
    for sample in &samples {
        let mut texts = vec![
            sample["id"].as_str().unwrap(),
            sample["focus"].as_str().unwrap(),
            sample["input"]["text"].as_str().unwrap(),
        ];
        texts.extend(
            sample["input"]["history"]
                .as_array()
                .unwrap()
                .iter()
                .map(|h| h.as_str().unwrap()),
        );
        texts.extend(
            sample["input"]["candidates"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| c["sentence"].as_str().unwrap()),
        );
        for text in texts {
            for prompt in [W1_PROMPT, W3_PROMPT, W4_PROMPT, old] {
                assert!(
                    !prompt.contains(text),
                    "AF96 input/label leaked into prompt"
                );
            }
        }
    }
}

#[test]
fn m15_w1_low_default_preserves_explicit_override_and_rule_only_prompts() {
    let mut models = WriteModels::default();
    assert_eq!(models.effort("w1"), ReasoningEffort::Low);
    assert_eq!(models.effort("w3"), ReasoningEffort::Medium);
    assert_eq!(models.effort("w4"), ReasoningEffort::Medium);
    models
        .reasoning_effort
        .insert("w1".into(), ReasoningEffort::Medium);
    assert_eq!(models.effort("w1"), ReasoningEffort::Medium);
    assert!(W1_PROMPT.contains("replacement value is one assertion"));
    assert!(W1_PROMPT.contains("Conditions and uncertainty do not by themselves"));
    assert!(W1_PROMPT.contains("identify its already named"));
    assert!(W4_PROMPT.contains("when the intention changes"));
    assert!(W4_PROMPT.contains("effective=null"));
    let samples: Json = serde_json::from_str(include_str!("../tests/eval/m15/w1.json")).unwrap();
    let mvp: Json = serde_json::from_str(include_str!("../tests/eval/m15/mvp.json")).unwrap();
    let originals = samples
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["input"]["text"].as_str().unwrap())
        .chain(
            mvp["messages"]
                .as_array()
                .unwrap()
                .iter()
                .map(|m| m["text"].as_str().unwrap()),
        )
        .chain(
            mvp["questions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|q| q["query"].as_str().unwrap()),
        );
    for original in originals {
        for prompt in [
            W1_PROMPT,
            W3_PROMPT,
            W4_PROMPT,
            crate::recall_models::R1_PROMPT,
            crate::recall_models::R4_PROMPT,
        ] {
            assert!(!prompt.contains(original), "M1.5 sample leaked into prompt");
        }
    }
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
            provenance: None,
            time_anchor: None,
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
fn runtime_model_diagnostics_distinguish_transport_decode_and_validation_failures() {
    let cases = [
        (Err(OpenAiAdapterError::Http("request timed out".into())), "timeout", None),
        (Ok((200, json!({"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"usage":{"output_tokens":16384}}).to_string())), "output_limit", None),
        (Ok((200, json!({"status":"incomplete","incomplete_details":{"reason":"length"},"usage":{"output_tokens":16384}}).to_string())), "output_limit", None),
        (Ok(envelope("not JSON")), "completed", Some("decode")),
        (Ok(envelope(&json!({"reviews":[],"coverage":{"status":"complete","missing":[]},"request_reviews":[]}).to_string())), "completed", Some("validate")),
        (Ok(envelope(&w3().to_string())), "completed", None),
    ];
    for (response, local_outcome, failed_phase) in cases {
        let db = Db::new();
        let text = "我喜欢咖啡。";
        let key = db.start(text);
        let (provider, sent) = mocked(&db, vec![Ok(envelope(&w1(text).to_string())), response]);
        let (result, events) = diagnostics::capture_model_events(|| run(&db, &key, &provider));
        assert_eq!(sent.lock().unwrap().len(), 2, "no automatic retry");
        let end = events
            .iter()
            .filter(|e| e["kind"] == "model_end")
            .last()
            .unwrap();
        assert_eq!(end["data"]["local_outcome"], local_outcome);
        let failed: Vec<_> = events
            .iter()
            .filter(|e| e["kind"] == "model_check" && e["data"]["status"] == "failed")
            .collect();
        assert_eq!(
            failed.first().and_then(|e| e["data"]["phase"].as_str()),
            failed_phase
        );
        assert_eq!(
            result["enrichment"]["status"],
            if local_outcome == "completed" && failed_phase.is_none() {
                "committed"
            } else {
                "failed"
            }
        );
        assert!(
            events
                .iter()
                .all(|e| e["data"].get("request").is_none() && e["data"].get("response").is_none())
        );
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
        assert!(!prompt.contains("For example"));
        assert!(!prompt.contains("下周三去医院复查"));
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

fn run_all(db: &Db, key: &str, provider: &WriteModelProvider) -> Json {
    run_source_with_all_models(&mut db.open(), key, RuntimeInputBudget::default(), provider)
        .unwrap()
}

// Use the sample's model response, rebinding only database-assigned source/record IDs.
fn w4_case(db: &Db, extra: bool) -> (String, Json, Json) {
    let sample: Json = serde_json::from_str(include_str!(
        "../tests/eval/w4/samples/w4_10_reinforce.json"
    ))
    .unwrap();
    let old_text = sample["sources"]["s-111"]["text"].as_str().unwrap();
    let old_key = db.start_turn(old_text, "seed", &[]);
    let old_w1 =
        json!({"memory_requests":[],"candidates":[sample["input"]["records"][0]["candidate"]]});
    let (seed, _) = mocked(
        db,
        vec![
            Ok(envelope(&old_w1.to_string())),
            Ok(envelope(&w3().to_string())),
        ],
    );
    let old = run_all(db, &old_key, &seed)["enrichment"]["candidates"][0]["memory_id"].clone();
    assert!(old.is_number());
    let text = sample["sources"]["s-320"]["text"].as_str().unwrap();
    let text = if extra {
        format!("{text}我收藏陶瓷杯。")
    } else {
        text.into()
    };
    let key = db.start(&text);
    let state = db.open().read().unwrap();
    let mut upstream =
        json!({"memory_requests":[],"candidates":[sample["input"]["candidates"][0]["candidate"]]});
    let mut output: Json = serde_json::from_str(include_str!(
        "../tests/eval/w4/fixtures/w4_10_reinforce.output.json"
    ))
    .unwrap();
    output["decisions"][0]["targets"] = json!([old]);
    output["decisions"][0]["basis"][0]["source"]["source_id"] = json!(state.sources[&old_key].id);
    output["decisions"][0]["basis"][1]["source"]["source_id"] = json!(state.sources[&key].id);
    if extra {
        upstream["candidates"]
            .as_array_mut()
            .unwrap()
            .push(w1("我收藏陶瓷杯。")["candidates"][0].clone());
        output["decisions"].as_array_mut().unwrap().push(json!({
            "candidate":1,"status":"decided","action":"new","targets":[],"resolves_relations":[],
            "basis":[{"source":{"kind":"stored","source_id":state.sources[&key].id},"quote":{"text":"我收藏陶瓷杯。"}}],
            "effective":null,"relation_scope":null,"pending_reason":null
        }));
    }
    (key, upstream, output)
}

fn w3_for(upstream: &Json) -> Json {
    let mut output = w3();
    output["reviews"] = json!(
        upstream["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
            .map(|(i, _)| {
                let mut r = w3()["reviews"][0].clone();
                r["candidate"] = json!(i);
                r
            })
            .collect::<Vec<_>>()
    );
    output
}

#[test]
fn entity_registration_during_each_model_preserves_independent_candidates() {
    for stage in ["w1_v3", "w3_v3", "w4_v4"] {
        let db = Db::new();
        let first = "我喜欢浅蓝围巾。";
        let old_key = db.start_turn(first, "seed-entity", &[]);
        let (seed, _) = mocked(
            &db,
            vec![
                Ok(envelope(&w1(first).to_string())),
                Ok(envelope(&w3().to_string())),
            ],
        );
        let old = run_all(&db, &old_key, &seed)["enrichment"]["candidates"][0]["memory_id"].clone();
        assert!(old.is_number());
        let text = "我仍喜欢浅蓝围巾。家人使用长笛。";
        let key = db.start_turn(text, "mixed-entity", &[]);
        let state = db.open().read().unwrap();
        let source_id = state.sources[&key].id.clone();
        let mut extraction = w1("我仍喜欢浅蓝围巾。");
        let mut unresolved = w1("家人使用长笛。")["candidates"][0].clone();
        unresolved["subject"] = json!({"unresolved":{"text":"家人"}});
        extraction["candidates"]
            .as_array_mut()
            .unwrap()
            .push(unresolved);
        let decision = json!({"decisions":[{"candidate":0,"status":"decided","action":"reinforce",
            "targets":[old],"resolves_relations":[],"basis":[
                {"source":{"kind":"stored","source_id":state.sources[&old_key].id},"quote":{"text":first}},
                {"source":{"kind":"stored","source_id":source_id},"quote":{"text":"我仍喜欢浅蓝围巾。"}}],
            "effective":null,"relation_scope":null,"pending_reason":null}]});
        let (mut provider, sent) = mocked(
            &db,
            vec![
                Ok(envelope(&extraction.to_string())),
                Ok(envelope(&w3_for(&extraction).to_string())),
                Ok(envelope(&decision.to_string())),
            ],
        );
        let transport = provider.client.transport.take().unwrap();
        let path = db.0.clone();
        let source_key = key.clone();
        provider.client.transport = Some(Arc::new(move |request| {
            let body: Json =
                serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap();
            if body["text"]["format"]["name"] == stage {
                let mut store = SqliteMemoryStore::open(&path, "alice".into()).unwrap();
                let before = store.read().unwrap();
                store
                    .transaction(|s| {
                        s.entities.upsert(Entity {
                            id: "family-new".into(),
                            owner_id: "alice".into(),
                            entity_type: EntityType::Person,
                            canonical_name: "新增亲属".into(),
                            aliases: vec!["家人".into()],
                        });
                        Ok(())
                    })
                    .unwrap();
                let after = store.read().unwrap();
                assert_eq!(after.revision, before.revision + 1);
                assert_eq!(
                    after.source_less_barrier_sequence,
                    before.source_less_barrier_sequence
                );
                assert_eq!(after.sources[&source_key].enrichment, "running");
            }
            transport(request)
        }));
        let result = run_all(&db, &key, &provider);
        assert_eq!(
            result["enrichment"]["status"], "pending",
            "{stage}: {result}"
        );
        assert_eq!(result["enrichment"]["candidates"][0]["status"], "committed");
        assert_eq!(
            result["enrichment"]["candidates"][1]["status"],
            "unresolved"
        );
        assert_eq!(
            result["enrichment"]["candidates"][1]["reasons"],
            json!(["unresolved_subject"])
        );
        assert_eq!(sent.lock().unwrap().len(), 3);
        let final_state = db.open().read().unwrap();
        assert_eq!(
            final_state
                .repository
                .list_by_owner(&"alice".into())
                .unwrap()
                .len(),
            1
        );
        assert_eq!(final_state.sources[&key].task.round, Some(0));
    }
}

#[test]
fn appended_directory_after_reopen_does_not_bypass_stage_binding() {
    let db = Db::new();
    let text = "我偏好墨绿色帽子。";
    let key = db.start_turn(text, "directory-reopen", &[]);
    let (first, _) = mocked(
        &db,
        vec![
            Ok(envelope(&w1(text).to_string())),
            Err(OpenAiAdapterError::Http("synthetic interrupted W3".into())),
        ],
    );
    assert_eq!(run_all(&db, &key, &first)["enrichment"]["status"], "failed");
    let before = db.open().read().unwrap();
    db.open()
        .transaction(|s| {
            s.entities.upsert(Entity {
                id: "new-person".into(),
                owner_id: "alice".into(),
                entity_type: EntityType::Person,
                canonical_name: "新人物".into(),
                aliases: vec![],
            });
            Ok(())
        })
        .unwrap();
    assert_eq!(
        db.open().read().unwrap().source_less_barrier_sequence,
        before.source_less_barrier_sequence
    );
    let (mut next, sent) = mocked(&db, vec![]);
    next.context.known_entities = db
        .open()
        .read()
        .unwrap()
        .entities
        .list_by_owner(&"alice".into());
    assert_eq!(
        run_all(&db, &key, &next)["enrichment"]["status"],
        "needs_rejudge"
    );
    assert!(sent.lock().unwrap().is_empty());
    assert_eq!(db.calls(), 2);
    assert_eq!(db.open().read().unwrap().sources[&key].task.round, Some(0));
}

fn ready_w4(
    db: &Db,
    key: &str,
    provider: &WriteModelProvider,
) -> (
    W4Claim,
    ProviderContext,
    W4Input,
    BTreeMap<String, SourceInput>,
) {
    let mut store = db.open();
    let claim = w4_claim_round(&mut store, key).unwrap();
    let (ctx, text) = inputs(&store, key, &claim);
    let batch = provider.provide(&mut store, &claim, &ctx, &text).unwrap();
    let candidates: Vec<_> = batch
        .w2
        .candidates
        .iter()
        .map(|c| W4Candidate {
            index: c.index,
            committed_id: None,
            candidate: c.candidate.clone(),
            source: SourceRef::Stored {
                source_id: ctx.source_id.clone(),
            },
            said_on: batch.w2.said_on.clone(),
            time_resolutions: c.time_resolutions.clone(),
        })
        .collect();
    let admissions = candidates
        .iter()
        .map(|c| {
            (
                c.index,
                w4::Admission {
                    episode_useful: false,
                    remember_bound: false,
                },
            )
        })
        .collect();
    crate::commit::w4_save_batch(&mut store, &claim, &candidates, &admissions, &batch, &[])
        .unwrap();
    let pending: Vec<_> = candidates
        .iter()
        .map(|c| w4::Pending {
            index: c.index,
            committed_id: c.committed_id,
            contract: crate::contract::ContractCandidate {
                candidate: c.candidate.clone(),
                source: c.source.clone(),
                said_on: c.said_on.clone(),
                time_resolutions: c.time_resolutions.clone(),
            },
        })
        .collect();
    let (state, assembly) = w4::assemble(
        &store,
        key,
        None,
        &pending,
        RuntimeInputBudget::default().w4,
    )
    .unwrap();
    assert!(assembly.call_w4);
    let sources = state
        .sources
        .values()
        .map(|s| {
            (
                s.id.clone(),
                SourceInput {
                    provenance: None,
                    time_anchor: None,
                    text: crate::lexical::body(s).unwrap().into(),
                    history: serde_json::from_value(s.request.as_ref().unwrap()["history"].clone())
                        .unwrap(),
                },
            )
        })
        .collect();
    (claim, ctx, assembly.input, sources)
}

#[test]
fn all_models_runtime_skips_w4_for_empty_or_unrelated_inputs() {
    for empty in [true, false] {
        let db = Db::new();
        let key = db.start("我使用铅笔。");
        let upstream = if empty {
            json!({"memory_requests":[],"candidates":[]})
        } else {
            w1("我使用铅笔。")
        };
        let mut responses = vec![Ok(envelope(&upstream.to_string()))];
        if !empty {
            responses.push(Ok(envelope(&w3().to_string())));
        }
        let (provider, _) = mocked(&db, responses);
        assert_eq!(
            run_all(&db, &key, &provider)["enrichment"]["status"],
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
    }
}

#[test]
fn w4_fixture_closes_the_model_pipeline_with_three_sends_and_reopen() {
    let db = Db::new();
    let (key, upstream, output) = w4_case(&db, false);
    let (provider, sent) = mocked(
        &db,
        vec![
            Ok(envelope(&upstream.to_string())),
            Ok(envelope(&w3_for(&upstream).to_string())),
            Ok(envelope(&output.to_string())),
        ],
    );
    let result = run_all(&db, &key, &provider);
    assert_eq!(result["enrichment"]["status"], "committed");
    assert_eq!(provider.client.model_call_count(), 3);
    assert_eq!(db.calls(), 5); // The seed operation used two calls.
    let state = db.open().read().unwrap();
    let memories = state
        .repository
        .list_by_owner(&UserId::from("alice"))
        .unwrap();
    assert_eq!(memories.len(), 1);
    assert_eq!(memories[0].reinforcement_count, 1);
    assert_eq!(memories[0].evidence.len(), 2);
    assert_eq!(
        result["enrichment"]["candidates"][0]["memory_id"],
        json!(memories[0].id)
    );
    let request = &sent.lock().unwrap()[2];
    assert_eq!(request["text"]["format"]["name"], "w4_v4");
    assert_eq!(request["text"]["format"]["strict"], true);
    let input: Json = serde_json::from_str(request["input"].as_str().unwrap()).unwrap();
    assert_eq!(input["contract"], "w4.v4");
    let mut old = request.clone();
    old["text"]["format"]["name"] = json!("w4_v2");
    let mut old_input = input.clone();
    old_input["contract"] = json!("w4.v2");
    old["input"] = json!(old_input.to_string());
    assert_ne!(
        crate::openai::recall_models::fingerprint(request),
        crate::openai::recall_models::fingerprint(&old),
        "v2 recordings naturally expire"
    );

    let stage = &state.sources[&key].result.as_ref().unwrap()["w4_model_stages"]["w4"];
    let mut binding_input = stage["binding"].clone();
    assert_eq!(
        binding_input.as_object_mut().unwrap().remove("model"),
        Some(json!(DEEPSEEK_MODEL))
    );
    assert_eq!(binding_input, input);
    assert_eq!(stage["output"]["raw_output"], output);
    assert_eq!(stage["output"]["final_output"], output);
    assert_eq!(
        crate::enrichment::receipt(&db.open(), &key).unwrap(),
        result
    );
}

#[test]
fn w4_cache_survives_reopen_and_rejects_every_changed_input_component() {
    let db = Db::new();
    let (key, upstream, output) = w4_case(&db, false);
    let (provider, _) = mocked(
        &db,
        vec![
            Ok(envelope(&upstream.to_string())),
            Ok(envelope(&w3_for(&upstream).to_string())),
            Ok(envelope(&output.to_string())),
        ],
    );
    let (claim, ctx, input, sources) = ready_w4(&db, &key, &provider);
    let first = provider
        .judge(&mut db.open(), &claim, &ctx, &input, &sources)
        .unwrap();
    w4_fail_attempt(&mut db.open(), &claim, "crash after W4 before plan").unwrap();
    let (reopened, _) = mocked(&db, vec![]);
    let claim = w4_claim_round(&mut db.open(), &key).unwrap();
    assert_eq!(
        reopened
            .judge(&mut db.open(), &claim, &ctx, &input, &sources)
            .unwrap(),
        first
    );
    for change in 0..4 {
        let mut changed = input.clone();
        let mut texts = sources.clone();
        match change {
            0 => {
                let crate::contract::RecordView2::Current(record) = &mut changed.records[0] else {
                    unreachable!()
                };
                record.candidate.polarity = crate::contract::Polarity::Negate;
            }
            1 => changed.relations.push(crate::contract::RelationLink {
                id: "changed-relation".into(),
                kind: crate::contract::RelationKind::Conflict,
                members: vec![input.records[0].id()],
                conditions: vec![],
                times: vec![],
                basis: vec![],
                status: crate::contract::RelationStatus::Open,
                resolution: None,
            }),
            2 => changed.coverage.omitted += 1,
            _ => texts
                .values_mut()
                .next()
                .unwrap()
                .history
                .push("新增历史".into()),
        }
        assert!(
            reopened
                .judge(&mut db.open(), &claim, &ctx, &changed, &texts)
                .unwrap_err()
                .to_string()
                .starts_with("needs_rejudge")
        );
    }
    assert_eq!(reopened.client.model_call_count(), 0);
    w4_fail_attempt(&mut db.open(), &claim, "resume runtime").unwrap();
    assert_eq!(
        run_all(&db, &key, &reopened)["enrichment"]["status"],
        "committed"
    );
    assert_eq!(reopened.client.model_call_count(), 0);
    assert_eq!(db.calls(), 5);
}

#[test]
fn w4_invalid_output_rejects_the_whole_batch_without_retry_or_business_writes() {
    for missing in [0, 1, 2] {
        let db = Db::new();
        let (key, upstream, mut output) = w4_case(&db, true);
        let before = db.open().read().unwrap().repository;
        if missing == 0 {
            output["decisions"][0]["basis"]
                .as_array_mut()
                .unwrap()
                .remove(0);
        } else if missing == 1 {
            output["decisions"][0]["basis"][1]["quote"]["text"] = json!("nonexistent evidence");
        } else {
            output["decisions"][0]["unexpected"] = json!(true);
        }
        let (provider, _) = mocked(
            &db,
            vec![
                Ok(envelope(&upstream.to_string())),
                Ok(envelope(&w3_for(&upstream).to_string())),
                Ok(envelope(&output.to_string())),
            ],
        );
        assert_eq!(
            run_all(&db, &key, &provider)["enrichment"]["status"],
            "failed"
        );
        let state = db.open().read().unwrap();
        assert_eq!(
            serde_json::to_value(&state.repository).unwrap(),
            serde_json::to_value(&before).unwrap()
        );
        assert_eq!(
            state.sources[&key].error.as_deref(),
            Some("w4_invalid_output")
        );
        assert!(!w4_progress(&db.open(), &key).unwrap().plan_ready);
        let (reopened, _) = mocked(&db, vec![]);
        assert_eq!(
            run_all(&db, &key, &reopened)["enrichment"]["status"],
            "failed"
        );
        assert_eq!(reopened.client.model_call_count(), 0);
        assert_eq!(provider.client.model_call_count(), 3);
        assert_eq!(db.calls(), 5);
    }
}

#[test]
fn w4_call_without_a_durable_result_never_resends() {
    let db = Db::new();
    let (key, upstream, _) = w4_case(&db, false);
    let (provider, _) = mocked(
        &db,
        vec![
            Ok(envelope(&upstream.to_string())),
            Ok(envelope(&w3_for(&upstream).to_string())),
        ],
    );
    let (claim, ctx, _, _) = ready_w4(&db, &key, &provider);
    provider
        .operation_client(&db.open(), &ctx)
        .unwrap()
        .budget
        .unwrap()
        .reserve(ModelStage::W4, b"reserved before crash")
        .unwrap();
    w4_fail_attempt(&mut db.open(), &claim, "crash before result checkpoint").unwrap();
    let (reopened, _) = mocked(&db, vec![]);
    assert_eq!(
        run_all(&db, &key, &reopened)["enrichment"]["status"],
        "failed"
    );
    let state = db.open().read().unwrap();
    assert_eq!(
        state.sources[&key].error.as_deref(),
        Some("model_invalid_output")
    );
    assert_eq!(
        state.sources[&key].result.as_ref().unwrap()["w4_input_meta"]["diagnostic"]["detail_code"],
        "model_result_missing"
    );
    assert_eq!(reopened.client.model_call_count(), 0);
    assert_eq!(db.calls(), 5);
}

#[test]
fn w4_saved_plan_and_partial_commits_resume_without_model_sends() {
    for committed in 0..=2 {
        let db = Db::new();
        let (key, upstream, output) = w4_case(&db, true);
        let (provider, _) = mocked(
            &db,
            vec![
                Ok(envelope(&upstream.to_string())),
                Ok(envelope(&w3_for(&upstream).to_string())),
                Ok(envelope(&output.to_string())),
            ],
        );
        let (claim, ctx, input, sources) = ready_w4(&db, &key, &provider);
        let output = provider
            .judge(&mut db.open(), &claim, &ctx, &input, &sources)
            .unwrap();
        let state = db.open().read().unwrap();
        let admissions = input
            .candidates
            .iter()
            .map(|c| {
                (
                    c.index,
                    w4::Admission {
                        episode_useful: false,
                        remember_bound: false,
                    },
                )
            })
            .collect();
        let plan = w4::plan(
            &w4::W4Runtime {
                state: &state,
                owner: ctx.owner,
                source_key: &key,
                round: claim.round,
                task_id: None,
                admissions,
            },
            &input,
            &output,
        )
        .unwrap();
        assert_eq!(plan.groups.len(), 2);
        w4_save_plan(&mut db.open(), &claim, &input.candidates, &plan).unwrap();
        for group in plan.groups.iter().take(committed) {
            w4_commit_group(&mut db.open(), &claim, group).unwrap();
        }
        w4_fail_attempt(&mut db.open(), &claim, "crash before source summary").unwrap();
        let (reopened, _) = mocked(&db, vec![]);
        assert_eq!(
            run_all(&db, &key, &reopened)["enrichment"]["status"],
            "committed"
        );
        let state = db.open().read().unwrap();
        let memories = state
            .repository
            .list_by_owner(&UserId::from("alice"))
            .unwrap();
        assert_eq!(memories.len(), 2);
        assert_eq!(memories[0].reinforcement_count, 1);
        assert_eq!(memories[0].evidence.len(), 2);
        assert!(w4_progress(&db.open(), &key).unwrap().remaining.is_empty());
        assert_eq!(reopened.client.model_call_count(), 0);
        assert_eq!(db.calls(), 5);
    }
}

#[test]
fn w4_schema_is_strict_and_matches_the_frozen_decision_fields() {
    let schema = w4_schema();
    let mut pending = vec![&schema];
    while let Some(value) = pending.pop() {
        match value {
            Json::Object(map) => {
                if value["type"] == "object" {
                    assert_eq!(value["additionalProperties"], false);
                    let fields: BTreeSet<_> = value["properties"]
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
                    assert_eq!(fields, required);
                }
                pending.extend(map.values());
            }
            Json::Array(array) => pending.extend(array),
            _ => {}
        }
    }
    assert_eq!(schema["required"], json!(["decisions"]));
    let decision = &schema["properties"]["decisions"]["items"];
    assert_eq!(
        decision["required"],
        json!([
            "candidate",
            "status",
            "action",
            "targets",
            "resolves_relations",
            "basis",
            "effective",
            "relation_scope",
            "pending_reason"
        ])
    );
    assert_eq!(
        decision["properties"]["action"]["anyOf"][0]["enum"],
        json!([
            "new",
            "reinforce",
            "state_change",
            "correction",
            "refine",
            "retract",
            "coexist",
            "conflict"
        ])
    );
    for nullable in ["action", "effective", "relation_scope", "pending_reason"] {
        assert_eq!(decision["properties"][nullable]["anyOf"][1]["type"], "null");
    }
    let scope = &decision["properties"]["relation_scope"]["anyOf"][0];
    assert_eq!(scope["required"], json!(["conditions", "times"]));
    assert_eq!(
        scope["properties"]["conditions"]["items"]["required"],
        json!(["of", "index"])
    );
}

#[test]
fn prompts_never_include_evaluation_source_text_ids_or_labels() {
    fn labels(value: &Json, out: &mut Vec<String>) {
        match value {
            Json::Object(map) => {
                for (key, value) in map {
                    if matches!(key.as_str(), "focus" | "note" | "why" | "must_not") {
                        fn text(value: &Json, out: &mut Vec<String>) {
                            match value {
                                Json::String(s) => out.push(s.clone()),
                                Json::Array(a) => {
                                    for v in a {
                                        text(v, out);
                                    }
                                }
                                _ => {}
                            }
                        }
                        text(value, out);
                    } else {
                        labels(value, out);
                    }
                }
            }
            Json::Array(a) => {
                for v in a {
                    labels(v, out);
                }
            }
            _ => {}
        }
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/eval");
    for stage in ["w1", "w3", "w4"] {
        let mut count = 0;
        for entry in std::fs::read_dir(root.join(stage).join("samples")).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            count += 1;
            let sample: Json =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            let mut texts: Vec<String> = if stage == "w4" {
                sample["sources"]
                    .as_object()
                    .unwrap()
                    .values()
                    .map(|s| s["text"].as_str().unwrap().into())
                    .collect()
            } else {
                vec![sample["input"]["text"].as_str().unwrap().into()]
            };
            let inputs: Vec<_> = if stage == "w4" {
                sample["sources"].as_object().unwrap().values().collect()
            } else {
                vec![&sample["input"]]
            };
            for input in inputs {
                if let Some(history) = input["history"].as_array() {
                    texts.extend(history.iter().map(|s| s.as_str().unwrap().to_owned()));
                }
            }
            texts.push(sample["id"].as_str().unwrap().into());
            labels(&sample, &mut texts);
            for text in texts {
                assert!(!text.is_empty());
                for prompt in [W1_PROMPT, W3_PROMPT, W4_PROMPT] {
                    assert!(
                        !prompt.contains(&text),
                        "evaluation leak: {} contains {text:?}",
                        path.display()
                    );
                }
            }
        }
        assert!(count > 0);
    }
}

#[test]
fn recommended_background_config_uses_environment_role_defaults() {
    let config: crate::chat::ChatConfig =
        serde_json::from_str(include_str!("../examples/background-recall.json")).unwrap();
    config.validate().unwrap();
    assert!(config.write_models.is_none());
    assert!(config.recall_models.is_none());
    assert_eq!(
        config.write_pipeline,
        crate::chat::WritePipeline::W4RuntimeV1
    );
    assert_eq!(
        config.recall_pipeline,
        crate::chat::RecallPipeline::RecallV3
    );
    // Stage-only explicit providers keep their historic requests; chat resolves AC2 defaults.
    assert_eq!(WriteModels::default().w1, DEEPSEEK_MODEL);
}

#[test]
fn second_revision_prompts_cover_value_qualifiers_and_group_decisions() {
    for rule in [
        "Only relation=fact",
        "memory_type=fact does not trigger",
        "Polarity is the assertion's direction",
        "conditions, not times",
        "range.start/end_inclusive",
        "times=[]",
        "Request quote includes the control words",
    ] {
        assert!(W1_PROMPT.contains(rule), "missing W1 rule: {rule}");
    }
    for rule in [
        "unsupported narrowing",
        "exact continuous current-source Quote",
        "actual relation, value and qualifiers",
        "gaps requires at least one",
    ] {
        assert!(W3_PROMPT.contains(rule), "missing W3 rule: {rule}");
    }
    for rule in [
        "including pending",
        "source.source_id",
        "Every target of a decided action",
        "changed preference intensity",
        "scope_incomplete",
        "not force pending",
        "at most once across the whole output",
        "surviving member's superseded version",
    ] {
        assert!(W4_PROMPT.contains(rule), "missing W4 rule: {rule}");
    }
}

#[test]
fn w4_cached_output_is_revalidated_without_sending() {
    let db = Db::new();
    let (key, upstream, output) = w4_case(&db, false);
    let (provider, _) = mocked(
        &db,
        vec![
            Ok(envelope(&upstream.to_string())),
            Ok(envelope(&w3_for(&upstream).to_string())),
            Ok(envelope(&output.to_string())),
        ],
    );
    let (claim, ctx, input, sources) = ready_w4(&db, &key, &provider);
    provider
        .judge(&mut db.open(), &claim, &ctx, &input, &sources)
        .unwrap();
    db.open()
        .transaction(|state| {
            state
                .sources
                .get_mut(&key)
                .unwrap()
                .result
                .as_mut()
                .unwrap()["w4_model_stages"]["w4"]["output"]["final_output"]["decisions"][0]["basis"] = json!([]);
            Ok(())
        })
        .unwrap();
    let (reopened, _) = mocked(&db, vec![]);
    assert!(
        reopened
            .judge(&mut db.open(), &claim, &ctx, &input, &sources)
            .unwrap_err()
            .to_string()
            .starts_with("w4_invalid_output")
    );
    assert_eq!(reopened.client.model_call_count(), 0);
    assert_eq!(db.calls(), 5);
}

#[test]
fn w4_background_cap_counts_the_first_two_stages() {
    let db = Db::new();
    let (key, upstream, _) = w4_case(&db, false);
    let (mut provider, _) = mocked(
        &db,
        vec![
            Ok(envelope(&upstream.to_string())),
            Ok(envelope(&w3_for(&upstream).to_string())),
        ],
    );
    provider.client.budget.as_mut().unwrap().config.group_id = "background-two".into();
    provider.client = provider.client.with_background_operation_limit(2).unwrap();
    let before = db.open().read().unwrap().repository;
    assert_eq!(
        run_all(&db, &key, &provider)["enrichment"]["status"],
        "budget_exhausted"
    );
    assert_eq!(provider.client.model_call_count(), 2);
    assert_eq!(db.calls(), 4);
    assert_eq!(
        serde_json::to_value(&db.open().read().unwrap().repository).unwrap(),
        serde_json::to_value(&before).unwrap()
    );
    let mut other = client(&db);
    other.budget.as_mut().unwrap().config.group_id = "different-background-two".into();
    other = other.with_background_operation_limit(2).unwrap();
    let operation = other
        .budget
        .unwrap()
        .for_operation(&db.0, "alice", "write-op")
        .unwrap();
    assert!(
        operation
            .reserve(
                ModelStage::W4,
                b"changing group cannot reset background cap"
            )
            .is_err()
    );
}

#[test]
fn w4_deletion_scrubs_related_raw_sources_and_invalidates_the_cache() {
    let db = Db::new();
    let (key, upstream, output) = w4_case(&db, false);
    let (provider, _) = mocked(
        &db,
        vec![
            Ok(envelope(&upstream.to_string())),
            Ok(envelope(&w3_for(&upstream).to_string())),
            Ok(envelope(&output.to_string())),
        ],
    );
    let (claim, ctx, input, sources) = ready_w4(&db, &key, &provider);
    provider
        .judge(&mut db.open(), &claim, &ctx, &input, &sources)
        .unwrap();
    let old = output["decisions"][0]["basis"][0]["source"]["source_id"]
        .as_str()
        .unwrap();
    let text = &sources[old].text;
    let mut service = ChatMemoryService::new(
        db.open(),
        ChatConfig {
            enrichment_mode: EnrichmentMode::Off,
            ..Default::default()
        },
    )
    .unwrap();
    service
        .call("memory_forget", json!({"source_id":old}), || false)
        .unwrap();
    let state = db.open().read().unwrap();
    let result = state.sources[&key].result.as_ref().unwrap();
    assert!(!result.to_string().contains(text));
    assert_eq!(result["w4_checkpoint"]["invalidated"], "source_deleted");
    assert_eq!(
        result["w4_model_stages"]["w4"]["output"]["final_output"]["decisions"][0]["action"],
        "reinforce"
    );
    assert!(
        provider
            .judge(&mut db.open(), &claim, &ctx, &input, &sources)
            .unwrap_err()
            .to_string()
            .starts_with("claim_lost")
    );
    assert_eq!(db.calls(), 5);
}

#[test]
fn w4_late_output_cannot_overwrite_a_successor_or_restore_deleted_content() {
    for delete in [false, true] {
        let db = Db::new();
        let (key, upstream, output) = w4_case(&db, false);
        let (provider, _) = mocked(
            &db,
            vec![
                Ok(envelope(&upstream.to_string())),
                Ok(envelope(&w3_for(&upstream).to_string())),
            ],
        );
        let (claim, ctx, input, sources) = ready_w4(&db, &key, &provider);
        let path = db.0.clone();
        let task = key.clone();
        let id = ctx.source_id.clone();
        let successor = Arc::new(Mutex::new(None));
        let captured = successor.clone();
        let mut c = client(&db);
        c.transport = Some(Arc::new(move |_| {
            let mut store = SqliteMemoryStore::open(&path, UserId::from("alice")).unwrap();
            if delete {
                let mut service = ChatMemoryService::new(
                    store,
                    ChatConfig {
                        enrichment_mode: EnrichmentMode::Off,
                        ..Default::default()
                    },
                )
                .unwrap();
                service
                    .call("memory_forget", json!({"source_id":id}), || false)
                    .unwrap();
            } else {
                store
                    .transaction(|s| {
                        s.sources.get_mut(&task).unwrap().task.lease_until = Some(0);
                        Ok(())
                    })
                    .unwrap();
                *captured.lock().unwrap() = Some(w4_claim_round(&mut store, &task).unwrap());
            }
            Ok(envelope(&output.to_string()))
        }));
        let late = WriteModelProvider::new(c, context());
        let error = late
            .judge(&mut db.open(), &claim, &ctx, &input, &sources)
            .unwrap_err()
            .to_string();
        let state = db.open().read().unwrap();
        let source = &state.sources[&key];
        if delete {
            assert!(error == "source_deleted" || error.starts_with("claim_lost"));
            assert!(source.request.is_none());
        } else {
            assert!(error.starts_with("claim_lost"));
            assert_eq!(
                source.task.attempt_id.as_deref(),
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
        }
        assert!(
            source
                .result
                .as_ref()
                .is_none_or(|r| r["w4_model_stages"]["w4"].is_null())
        );
        assert_eq!(late.client.model_call_count(), 1);
        assert_eq!(db.calls(), 5);
    }
}

#[test]
fn w4_missing_new_basis_is_assembled_saved_and_resumed_without_sends() {
    let db = Db::new();
    let (key, upstream, mut raw) = w4_case(&db, false);
    let (provider, _) = mocked(
        &db,
        vec![
            Ok(envelope(&upstream.to_string())),
            Ok(envelope(&w3_for(&upstream).to_string())),
            // Replace the response below after binding the canonical input source.
            Ok(envelope("{}")),
        ],
    );
    // w4_case returns old and new evidence in that order; retain only the old side.
    raw["decisions"][0]["basis"]
        .as_array_mut()
        .unwrap()
        .remove(1);
    let (claim, ctx, input, sources) = ready_w4(&db, &key, &provider);
    let (w4_provider, _) = mocked(&db, vec![Ok(envelope(&raw.to_string()))]);
    let final_output = w4_provider
        .judge(&mut db.open(), &claim, &ctx, &input, &sources)
        .unwrap();
    let state = db.open().read().unwrap();
    let checkpoint =
        &state.sources[&key].result.as_ref().unwrap()["w4_model_stages"]["w4"]["output"];
    assert_eq!(checkpoint["raw_output"], raw);
    assert_eq!(
        checkpoint["final_output"],
        serde_json::to_value(&final_output).unwrap()
    );
    assert_eq!(final_output.decisions[0].basis.len(), 2);
    assert_eq!(provider.client.model_call_count(), 2);
    assert_eq!(w4_provider.client.model_call_count(), 1);
    w4_fail_attempt(&mut db.open(), &claim, "crash after assembled checkpoint").unwrap();
    let (reopened, _) = mocked(&db, vec![]);
    assert_eq!(
        run_all(&db, &key, &reopened)["enrichment"]["status"],
        "committed"
    );
    assert_eq!(reopened.client.model_call_count(), 0);
    assert_eq!(db.calls(), 5);
    assert_eq!(
        db.open()
            .read()
            .unwrap()
            .repository
            .list_by_owner(&UserId::from("alice"))
            .unwrap()[0]
            .reinforcement_count,
        1
    );
}

#[test]
fn w4_invalid_raw_is_audited_but_explicit_rejudge_can_call_again() {
    let db = Db::new();
    let (key, upstream, good) = w4_case(&db, false);
    let mut invalid = good.clone();
    invalid["decisions"][0]["basis"]
        .as_array_mut()
        .unwrap()
        .remove(0);
    let (provider, _) = mocked(
        &db,
        vec![
            Ok(envelope(&upstream.to_string())),
            Ok(envelope(&w3_for(&upstream).to_string())),
            Ok(envelope(&invalid.to_string())),
        ],
    );
    assert_eq!(
        run_all(&db, &key, &provider)["enrichment"]["status"],
        "failed"
    );
    let state = db.open().read().unwrap();
    let stage = &state.sources[&key].result.as_ref().unwrap()["w4_model_stages"]["w4"];
    assert_eq!(stage["output"]["raw_output"], invalid);
    assert_eq!(stage["output"]["valid"], false);
    assert!(stage["output"]["final_output"].is_object());
    let (resumed, _) = mocked(&db, vec![]);
    assert_eq!(
        run_all(&db, &key, &resumed)["enrichment"]["status"],
        "failed"
    );
    assert_eq!(resumed.client.model_call_count(), 0);
    w4_start_round(&mut db.open(), &key, "explicit-after-invalid", true).unwrap();
    let (rejudged, _) = mocked(&db, vec![Ok(envelope(&good.to_string()))]);
    assert_eq!(
        run_all(&db, &key, &rejudged)["enrichment"]["status"],
        "committed"
    );
    assert_eq!(rejudged.client.model_call_count(), 1);
    assert_eq!(db.calls(), 6);
}

#[test]
fn w4_raw_quote_layout_is_scrubbed_when_old_source_is_deleted() {
    for mode in ["valid", "invalid", "undecodable"] {
        let db = Db::new();
        let (key, upstream, mut raw) = w4_case(&db, false);
        let old = raw["decisions"][0]["basis"][0]["source"]["source_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let quote = raw["decisions"][0]["basis"][0]["quote"]["text"]
            .as_str()
            .unwrap()
            .to_owned();
        let variant = quote
            .chars()
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        raw["decisions"][0]["basis"][0]["quote"]["text"] = json!(variant);
        if mode == "invalid" {
            raw["decisions"][0]["targets"] = json!([]);
        }
        let response = if mode == "undecodable" {
            format!("broken JSON: {variant}")
        } else {
            raw.to_string()
        };
        let (provider, _) = mocked(
            &db,
            vec![
                Ok(envelope(&upstream.to_string())),
                Ok(envelope(&w3_for(&upstream).to_string())),
                Ok(envelope(&response)),
            ],
        );
        let (claim, ctx, input, sources) = ready_w4(&db, &key, &provider);
        let judged = provider.judge(&mut db.open(), &claim, &ctx, &input, &sources);
        if mode == "valid" {
            let state = db.open().read().unwrap();
            let plan = w4::plan(
                &w4::W4Runtime {
                    state: &state,
                    owner: ctx.owner,
                    source_key: &key,
                    round: claim.round,
                    task_id: None,
                    admissions: input
                        .candidates
                        .iter()
                        .map(|c| {
                            (
                                c.index,
                                w4::Admission {
                                    episode_useful: false,
                                    remember_bound: false,
                                },
                            )
                        })
                        .collect(),
                },
                &input,
                &judged.unwrap(),
            )
            .unwrap();
            w4_save_plan(&mut db.open(), &claim, &input.candidates, &plan).unwrap();
        } else {
            assert!(judged.is_err());
        }
        let before = db.open().read().unwrap();
        let audit =
            &before.sources[&key].result.as_ref().unwrap()["w4_model_stages"]["w4"]["output"];
        assert!(audit["raw_output"].to_string().contains(&variant));
        if mode == "invalid" {
            assert!(audit["final_output"].to_string().contains(&variant));
        }
        let mut service = ChatMemoryService::new(
            db.open(),
            ChatConfig {
                enrichment_mode: EnrichmentMode::Off,
                ..Default::default()
            },
        )
        .unwrap();
        service
            .call("memory_forget", json!({"source_id":old}), || false)
            .unwrap();
        let state = db.open().read().unwrap();
        let result = state.sources[&key].result.as_ref().unwrap();
        let output = &result["w4_model_stages"]["w4"]["output"];
        assert!(output["raw_output"].is_null(), "{mode}");
        if mode != "valid" {
            assert!(output["final_output"].is_null(), "{mode}");
        }
        for fragment in [&quote, &variant, &sources[&old].text] {
            assert!(
                !result.to_string().contains(fragment),
                "deleted quote survives in audit, cache or checkpoint: {mode}"
            );
        }
        assert!(result["w4_checkpoint"]["plan"].is_null());
        assert_eq!(result["w4_checkpoint"]["invalidated"], "source_deleted");
        assert!(
            crate::commit::w4_load_model_stage(
                &db.open(),
                &claim,
                "w4",
                &result["w4_model_stages"]["w4"]["binding"],
            )
            .is_err(),
            "deleted source cannot be replayed from a cached stage"
        );
        assert_eq!(provider.client.model_call_count(), 3);
    }
}

#[test]
fn w4_unrelated_source_and_audit_survive_source_deletion() {
    let db = Db::new();
    let old_key = db.start_turn("我订阅植物杂志。", "forget", &[]);
    let key = db.start_turn("我收藏木质拼图。", "unrelated", &[]);
    let state = db.open().read().unwrap();
    let old = state.sources[&old_key].id.clone();
    let id = &state.sources[&key].id;
    let text = crate::lexical::body(&state.sources[&key]).unwrap();
    let binding = json!({"contract":"w4.v4","sources":{id:{"text":text,"history":[]}}});
    let decision = json!({"decisions":[{
        "candidate":0,"status":"decided","action":"new","targets":[],"resolves_relations":[],
        "basis":[{"source":{"kind":"stored","source_id":id},"quote":{"text":text}}],
        "effective":null,"relation_scope":null,"pending_reason":null
    }]});
    let audit = json!({"raw_output":decision,"final_output":decision,
        "valid":true,"validation_error":null});
    let claim = w4_claim_round(&mut db.open(), &key).unwrap();
    crate::commit::w4_save_model_stage(&mut db.open(), &claim, "w4", &binding, &audit).unwrap();
    let before = serde_json::to_value(&db.open().read().unwrap().sources[&key]).unwrap();
    let mut service = ChatMemoryService::new(
        db.open(),
        ChatConfig {
            enrichment_mode: EnrichmentMode::Off,
            ..Default::default()
        },
    )
    .unwrap();
    service
        .call("memory_forget", json!({"source_id":old}), || false)
        .unwrap();
    let state = db.open().read().unwrap();
    let after = serde_json::to_value(&state.sources[&key]).unwrap();
    // 删除会重建索引并更新全局修订号；来源内容、任务和审计须逐字不变。
    for field in [
        "id",
        "session_id",
        "turn_id",
        "observed_at",
        "received_sequence",
        "request",
        "result",
        "task",
        "enrichment",
        "error",
    ] {
        assert_eq!(after[field], before[field], "{field}");
    }
    assert_eq!(
        crate::commit::w4_load_model_stage(&db.open(), &claim, "w4", &binding).unwrap(),
        Some(audit)
    );
    assert_eq!(db.calls(), 0);
}

#[test]
fn w4_late_old_source_deletion_cannot_restore_raw_audit_or_input_copies() {
    for change in ["deleted", "text_changed", "history_changed"] {
        for response in ["valid", "invalid"] {
            let db = Db::new();
            let (key, upstream, mut raw) = w4_case(&db, false);
            let old = raw["decisions"][0]["basis"][0]["source"]["source_id"]
                .as_str()
                .unwrap()
                .to_owned();
            let quote = raw["decisions"][0]["basis"][0]["quote"]["text"]
                .as_str()
                .unwrap()
                .to_owned();
            let variant = quote
                .chars()
                .map(|c| c.to_string())
                .collect::<Vec<_>>()
                .join(" ");
            raw["decisions"][0]["basis"][0]["quote"]["text"] = json!(variant);
            raw["decisions"][0]["targets"] = json!([]);
            if response == "valid" {
                // 不引用旧方的有效决定也不能保存已过期的输入原文副本。
                raw["decisions"][0]["action"] = json!("coexist");
                raw["decisions"][0]["basis"]
                    .as_array_mut()
                    .unwrap()
                    .remove(0);
            }
            let (provider, _) = mocked(
                &db,
                vec![
                    Ok(envelope(&upstream.to_string())),
                    Ok(envelope(&w3_for(&upstream).to_string())),
                ],
            );
            let (claim, _, input, sources) = ready_w4(&db, &key, &provider);
            let decoded = serde_json::from_value(raw.clone()).unwrap();
            assert_eq!(
                w4::prepare_output(&input, &decoded, &|id| {
                    sources.get(id).map(|s| s.text.clone())
                })
                .is_ok(),
                response == "valid"
            );
            w4_fail_attempt(&mut db.open(), &claim, "resume through runtime").unwrap();
            let path = db.0.clone();
            let mut c = client(&db);
            c.transport = Some(Arc::new(move |_| {
                let mut store = SqliteMemoryStore::open(&path, UserId::from("alice")).unwrap();
                if change == "deleted" {
                    let mut service = ChatMemoryService::new(
                        store,
                        ChatConfig {
                            enrichment_mode: EnrichmentMode::Off,
                            ..Default::default()
                        },
                    )
                    .unwrap();
                    service
                        .call("memory_forget", json!({"source_id":old}), || false)
                        .unwrap();
                } else {
                    store
                        .transaction(|state| {
                            let source = state.sources.values_mut().find(|s| s.id == old).unwrap();
                            let request = source.request.as_mut().unwrap();
                            if change == "text_changed" {
                                request["text"] = json!("原文已更改。");
                            } else {
                                request["history"] = json!(["已更改的历史。"]);
                            }
                            Ok(())
                        })
                        .unwrap();
                }
                Ok(envelope(&raw.to_string()))
            }));
            let late = WriteModelProvider::new(c, context());
            assert_eq!(
                run_all(&db, &key, &late)["enrichment"]["status"],
                "needs_rejudge",
                "{change}/{response}"
            );
            let state = db.open().read().unwrap();
            let source = &state.sources[&key];
            assert_eq!(source.error.as_deref(), Some("needs_rejudge"));
            let result = source.result.as_ref().unwrap();
            assert!(result["w4_model_stages"]["w4"].is_null());
            assert_eq!(
                result["w4_input_meta"]["diagnostic"],
                json!({"reason":"needs_rejudge"})
            );
            for fragment in [&quote, &variant] {
                assert!(
                    !result.to_string().contains(fragment),
                    "late W4 output restores old source content: {change}/{response}"
                );
            }
            assert_eq!(late.client.model_call_count(), 1);
            let (resumed, _) = mocked(&db, vec![]);
            assert!(
                run_source_with_all_models(
                    &mut db.open(),
                    &key,
                    RuntimeInputBudget::default(),
                    &resumed,
                )
                .is_err(),
                "needs_rejudge cannot resume without an explicit new round"
            );
            assert_eq!(resumed.client.model_call_count(), 0);
        }
    }
}

#[test]
fn prompts_distinguish_assertions_and_intentions_without_rewriting_quotes() {
    for rule in [
        "value.quote.text exactly",
        "including final punctuation",
        "never by shortening a generic fact value",
        "Output each assertion",
        "each copying the whole",
        "does not merge different types or qualifiers",
        "do not use a relation describing an accomplished",
    ] {
        assert!(W1_PROMPT.contains(rule), "{rule}");
    }
    for prompt in [W1_PROMPT, W3_PROMPT] {
        for rule in [
            "without expressing the user's actual intention",
            "question or",
            "An expressed plan can also ask for advice",
            "conditions and uncertainty",
            "never question marks or keywords",
        ] {
            assert!(prompt.contains(rule), "{rule}");
        }
        assert!(!prompt.contains("For example"));
    }
}

#[test]
fn m15_prompts_pin_plan_and_subject_semantics_without_example_sentences() {
    for (prompt, rules) in [
        (
            W1_PROMPT,
            vec![
                "w1.v3",
                "speech=plan and memory_type=fact",
                "completed booking or purchase",
                "Kinship refers",
                "Every expressed qualifier",
                "Passing an event date",
            ],
        ),
        (
            W3_PROMPT,
            vec![
                "w3.v3",
                "user really",
                "qualifier present only in sentence",
                "cannot be supported",
            ],
        ),
        (
            W4_PROMPT,
            vec![
                "w4.v4",
                "plan change or cancellation",
                "automatically cancels a booking",
                "normally uses state_change",
            ],
        ),
    ] {
        assert!(!prompt.contains("For example"));
        for rule in rules {
            assert!(prompt.contains(rule), "missing rule {rule}");
        }
    }
}

#[test]
fn m15_write_contract_versions_bind_stages_and_change_requests_without_model_id_in_payload() {
    let db = Db::new();
    let text = "我偏爱无糖热茶。";
    let key = db.start(text);
    let (provider, sent) = mocked(
        &db,
        vec![
            Ok(envelope(&w1(text).to_string())),
            Ok(envelope(&w3().to_string())),
        ],
    );
    let result = run_all(&db, &key, &provider);
    assert_eq!(result["enrichment"]["status"], "committed");
    let requests = sent.lock().unwrap();
    assert_eq!(requests.len(), 2);
    let state = db.open().read().unwrap();
    for (i, (stage, version, schema, old_schema)) in [
        ("w1", "w1.v3", "w1_v3", "w1_v2"),
        ("w3", "w3.v3", "w3_v3", "w3_v2"),
    ]
    .iter()
    .enumerate()
    {
        assert_eq!(requests[i]["text"]["format"]["name"], *schema);
        let input: Json = serde_json::from_str(requests[i]["input"].as_str().unwrap()).unwrap();
        assert_eq!(input["contract"], *version);
        assert!(input.get("model").is_none());
        assert_eq!(
            state.sources[&key].result.as_ref().unwrap()["w4_model_stages"][stage]["binding"]["contract"],
            *version
        );
        let mut prior = requests[i].clone();
        prior["text"]["format"]["name"] = json!(old_schema);
        assert_ne!(
            crate::recall_models::fingerprint(&requests[i]),
            crate::recall_models::fingerprint(&prior)
        );
    }
}

#[test]
fn af90_injected_date_interpretations_survive_review_storage_and_reopen() {
    // Model decisions are injected: these checks cover transport/storage, not semantic inference.
    let cases = [
        (
            "我计划11月18日修篱笆。",
            "11月18日",
            "plan",
            json!({"kind":"date","date":"2026-11-18"}),
        ),
        (
            "我曾在8月6日修过篱笆。",
            "8月6日",
            "user_statement",
            json!({"kind":"date","date":"2026-08-06"}),
        ),
        (
            "我计划2028年11月18日修篱笆。",
            "2028年11月18日",
            "plan",
            json!({"kind":"date","date":"2028-11-18"}),
        ),
        (
            "我计划11月18日修篱笆。",
            "11月18日",
            "plan",
            json!({"kind":"date","date":"2028-11-18"}),
        ),
        (
            "我已预订11月18日的修缮服务。",
            "11月18日",
            "user_statement",
            json!({"kind":"date","date":"2026-11-18"}),
        ),
        (
            "我记得11月18日这件事。",
            "11月18日",
            "user_statement",
            json!({"kind":"unknown"}),
        ),
        (
            "我计划1月8日修篱笆。",
            "1月8日",
            "plan",
            json!({"kind":"unknown"}),
        ),
        (
            "我计划10月2日修篱笆。",
            "10月2日",
            "plan",
            json!({"kind":"unknown"}),
        ),
        (
            "我计划10月修篱笆。",
            "10月",
            "plan",
            json!({"kind":"unknown"}),
        ),
        (
            "我计划11月修篱笆。",
            "11月",
            "plan",
            json!({"kind":"month","year_offset":0,"month":11}),
        ),
        (
            "我计划11月18日至11月20日修篱笆。",
            "11月18日至11月20日",
            "plan",
            json!({"kind":"range","start":"2026-11-18","end_inclusive":"2026-11-20"}),
        ),
        (
            "我计划12月30日至1月2日修篱笆。",
            "12月30日至1月2日",
            "plan",
            json!({"kind":"unknown"}),
        ),
    ];
    for (text, quote, speech, interp) in cases {
        let db = Db::new();
        let history = if quote == "11月18日" && interp["date"] == "2028-11-18" {
            vec!["我已确定2028年的修缮安排。"]
        } else {
            vec![]
        };
        let key = db.start_turn(text, "date-boundary", &history);
        let time =
            json!({"role":"event","quote":{"text":quote},"anchor":"said_on","interp":interp});
        let mut extraction = w1(text);
        extraction["candidates"][0]["speech"] = json!(speech);
        extraction["candidates"][0]["times"] = json!([time]);
        let (provider, sent) = mocked(
            &db,
            vec![
                Ok(envelope(&extraction.to_string())),
                Ok(envelope(&w3().to_string())),
            ],
        );
        let result = run_all(&db, &key, &provider);
        assert_eq!(
            result["enrichment"]["status"], "committed",
            "{text}: {result}"
        );
        let requests = sent.lock().unwrap();
        let reviewed: Json = serde_json::from_str(requests[1]["input"].as_str().unwrap()).unwrap();
        assert_eq!(reviewed["said_on"], "2026-10-02");
        assert_eq!(reviewed["user_history"], json!(history));
        assert_eq!(
            reviewed["candidates"][0]["candidate"]["times"],
            json!([time])
        );
        assert_eq!(
            reviewed["candidates"][0]["time_resolutions"][0]["status"],
            if interp["kind"] == "unknown" {
                "unknown"
            } else {
                "resolved"
            }
        );
        // Reopening must retain the model's interpretation, including unknowns and coarse precision.
        let state = db.open().read().unwrap();
        let id = result["enrichment"]["candidates"][0]["memory_id"]
            .as_u64()
            .unwrap();
        let RecordView2::Current(view) = crate::w4::record_view(&state, MemoryId(id)).unwrap()
        else {
            panic!("current record required")
        };
        assert_eq!(
            serde_json::to_value(view.candidate.times).unwrap(),
            json!([time])
        );
        assert_eq!(db.calls(), 2);
    }
    let db = Db::new();
    let text = "我计划11月18日修篱笆，11月20日刷栅栏。";
    let key = db.start_turn(text, "independent-events", &[]);
    let mut extraction = w1(text);
    extraction["candidates"][0]["speech"] = json!("plan");
    extraction["candidates"][0]["times"] = json!([
        {"role":"event","quote":{"text":"11月18日"},"anchor":"said_on","interp":{"kind":"date","date":"2026-11-18"}},
        {"role":"event","quote":{"text":"11月20日"},"anchor":"said_on","interp":{"kind":"date","date":"2026-11-20"}}
    ]);
    let (provider, sent) = mocked(
        &db,
        vec![
            Ok(envelope(&extraction.to_string())),
            Ok(envelope(&w3().to_string())),
        ],
    );
    assert_eq!(
        run_all(&db, &key, &provider)["enrichment"]["status"],
        "committed"
    );
    let reviewed: Json =
        serde_json::from_str(sent.lock().unwrap()[1]["input"].as_str().unwrap()).unwrap();
    assert_eq!(
        reviewed["candidates"][0]["candidate"]["times"],
        extraction["candidates"][0]["times"]
    );
    assert_eq!(
        reviewed["candidates"][0]["time_resolutions"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn af90_w3_conflict_is_scoped_and_undated_plan_commits_without_lost_qualifiers() {
    let db = Db::new();
    let claims = [
        "我的卡号是31，又说是47。",
        "我打算修篱笆。",
        "如果雨停，我可能刷栅栏。",
    ];
    let text = claims.concat();
    let key = db.start_turn(&text, "scoped-review", &[]);
    let mut extraction = w1(&text);
    extraction["candidates"] = json!(
        claims
            .iter()
            .map(|text| w1(text)["candidates"][0].clone())
            .collect::<Vec<_>>()
    );
    extraction["candidates"][1]["speech"] = json!("plan");
    extraction["candidates"][2]["speech"] = json!("plan");
    // Missing applicable uncertainty/condition is rejected by the model, never inferred in code.
    let review = json!({"reviews":[
        {"candidate":0,"verdict":"insufficient","problems":[{"aspect":"value","quote":{"text":claims[0]}}],"episode_value":"not_applicable"},
        {"candidate":1,"verdict":"supported","problems":[],"episode_value":"not_applicable"},
        {"candidate":2,"verdict":"contradicted","problems":[{"aspect":"uncertainty","quote":{"text":"可能"}},{"aspect":"condition","quote":{"text":"如果雨停"}}],"episode_value":"not_applicable"}
    ],"coverage":{"status":"complete","missing":[]},"request_reviews":[]});
    let (provider, sent) = mocked(
        &db,
        vec![
            Ok(envelope(&extraction.to_string())),
            Ok(envelope(&review.to_string())),
        ],
    );
    let result = run_all(&db, &key, &provider);
    assert_eq!(
        result["enrichment"]["candidates"][0]["reasons"],
        json!(["review_insufficient"])
    );
    assert_eq!(result["enrichment"]["candidates"][1]["status"], "committed");
    assert_eq!(
        result["enrichment"]["candidates"][2]["reasons"],
        json!(["review_contradicted"])
    );
    let records = db
        .open()
        .read()
        .unwrap()
        .repository
        .list_by_owner(&UserId::from("alice"))
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].evidence[0].quote, claims[1]);
    assert_eq!(sent.lock().unwrap().len(), 2);
}

#[test]
fn af90_write_prompts_pin_date_direction_and_candidate_scoped_review() {
    for prompt in [W1_PROMPT, W3_PROMPT] {
        for rule in [
            "event direction, not speech",
            "explicit years",
            "said_on",
            "continuous interval as a whole",
            "precision",
            "unknown",
        ] {
            assert!(prompt.contains(rule), "{rule}");
        }
        assert!(!prompt.contains("For example"));
    }
    for rule in [
        "plan with no expressed calendar time",
        "times=[]",
        "unclear which one is wrong",
        "unrelated candidates",
        "Explicit refutation or correction",
        "applicable expressed qualifier",
    ] {
        assert!(W3_PROMPT.contains(rule), "{rule}");
    }
}

#[test]
fn subject_confirmation_calls_fresh_w3_without_w1_and_replay_spends_nothing() {
    let db = Db::new();
    let text = "孩子常用琥珀色书签。";
    let key = db.start_turn(text, "unresolved", &[]);
    let mut extraction = w1(text);
    extraction["candidates"][0]["subject"] = json!({"unresolved":{"text":"孩子"}});
    let (provider, sent) = mocked(
        &db,
        vec![
            Ok(envelope(&extraction.to_string())),
            Ok(envelope(&w3().to_string())),
        ],
    );
    assert_eq!(
        run(&db, &key, &provider)["enrichment"]["candidates"][0]["status"],
        "unresolved"
    );
    assert_eq!(sent.lock().unwrap().len(), 2);
    let confirming = db.start_turn("我确认这里的孩子是青芽。", "identity-confirmation", &[]);
    let entity = Entity {
        id: "child".into(),
        owner_id: "alice".into(),
        entity_type: EntityType::Person,
        canonical_name: "青芽".into(),
        aliases: vec!["孩子".into()],
    };
    db.open()
        .transaction(|state| {
            state.entities.upsert(entity.clone());
            Ok(())
        })
        .unwrap();
    let state = db.open().read().unwrap();
    let id = &state.sources[&key].id;
    let citation = &state.sources[&confirming].id;
    let mut service = ChatMemoryService::new(
        db.open(),
        ChatConfig {
            enrichment_mode: EnrichmentMode::Deferred,
            model_budget: Some(ModelBudgetConfig {
                group_id: "authorized".into(),
                max_calls: 100,
                max_operation_calls: 3,
                ..Default::default()
            }),
            ..Default::default()
        },
    )
    .unwrap();
    let status = service
        .call("memory_source_status", json!({"source_id":id}), || false)
        .unwrap();
    let args = json!({"source_id":id,"request_id":"confirmation","expected_state_token":status["state_token"],
        "mode":"confirm_subject","candidate_index":0,"entity_id":"child","enqueue":true,
        "confirmation":{"source":{"kind":"stored","source_id":citation},"quote":{"text":"这里的孩子是青芽"}}});
    let (mut final_provider, final_sent) = mocked(&db, vec![Ok(envelope(&w3().to_string()))]);
    final_provider.context.known_entities.push(entity);
    final_provider.context.utc_offset_seconds = 0; // Changing server timezone doesn't change the frozen anchor.
    service.inject_model_client(final_provider.client.clone());
    let accepted = service
        .call("memory_source_reprocess", args.clone(), || false)
        .unwrap();
    assert_eq!(accepted["enqueued"], true);
    assert_eq!(accepted["generation"], 1);
    assert_eq!(db.calls(), 2);
    assert!(final_sent.lock().unwrap().is_empty()); // enqueue isn't a synchronous model call.
    assert_eq!(
        run(&db, &key, &final_provider)["enrichment"]["status"],
        "committed"
    );
    assert_eq!(final_sent.lock().unwrap().len(), 1);
    assert_eq!(db.calls(), 3);
    let request = &final_sent.lock().unwrap()[0];
    assert_eq!(request["text"]["format"]["name"], "w3_v3");
    let payload: Json = serde_json::from_str(request["input"].as_str().unwrap()).unwrap();
    assert_eq!(payload["generation"], 1);
    assert_eq!(payload["utc_offset_seconds"], 28_800);
    assert_eq!(payload["subject_confirmations"][0]["entity_id"], "child");
    assert_eq!(
        payload["user_history"].as_array().unwrap().last().unwrap(),
        "这里的孩子是青芽"
    );
    assert_eq!(payload["current_user_message"], text);
    assert_eq!(payload["candidates"][0]["candidate"]["value"]["text"], text);
    assert!(payload.get("model").is_none());
    assert_eq!(
        service
            .call("memory_source_reprocess", args, || false)
            .unwrap()["replayed"],
        true
    );
    assert_eq!(db.calls(), 3);
}

#[test]
fn history_import_models_receive_null_original_time_and_frozen_before_parent_provenance() {
    for (text, quote, interp) in [
        (
            "我计划明天参加石溪展览。",
            "明天",
            json!({"kind":"relative","unit":"day","offset":1}),
        ),
        (
            "我计划11月18日修篱笆。",
            "11月18日",
            json!({"kind":"unknown"}),
        ),
    ] {
        let db = Db::new();
        let parent_key = db.start_turn("我补充展览安排。", "import-parent", &[text]);
        let parent = db.open().read().unwrap().sources[&parent_key].id.clone();
        let mut service = ChatMemoryService::new(
            db.open(),
            ChatConfig {
                enrichment_mode: EnrichmentMode::Off,
                model_budget: Some(ModelBudgetConfig {
                    group_id: "authorized".into(),
                    max_calls: 100,
                    max_operation_calls: 3,
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .unwrap();
        let status = service
            .call("memory_source_status", json!({"source_id":parent}), || {
                false
            })
            .unwrap();
        let args = json!({"source_id":parent,"history_index":0,
        "expected_content_fingerprint":status["history_entries"][0]["content_fingerprint"],
        "request_id":"import-model-input","capture_source":true});
        let imported = service
            .call("memory_source_import_history", args.clone(), || false)
            .unwrap();
        let child = imported["source"]["source_id"].as_str().unwrap();
        let key = db
            .open()
            .read()
            .unwrap()
            .sources
            .iter()
            .find(|(_, s)| s.id == child)
            .unwrap()
            .0
            .clone();
        let status = service
            .call("memory_source_status", json!({"source_id":child}), || false)
            .unwrap();
        let accepted = service
            .call(
                "memory_source_reprocess",
                json!({"source_id":child,
        "request_id":"import-model-round","expected_state_token":status["state_token"],
        "mode":"reextract","enqueue":false}),
                || false,
            )
            .unwrap();
        assert_eq!(accepted["generation"], 1);
        assert_eq!(accepted["time_anchor"]["said_on"], Json::Null);
        assert_eq!(accepted["enqueued"], false);
        let mut extraction = w1(text);
        extraction["candidates"][0]["speech"] = json!("plan");
        extraction["candidates"][0]["times"] = json!([{"role":"event","quote":{"text":quote},
        "anchor":"said_on","interp":interp}]);
        let (provider, sent) = mocked(
            &db,
            vec![
                Ok(envelope(&extraction.to_string())),
                Ok(envelope(&w3().to_string())),
            ],
        );
        let result = run(&db, &key, &provider);
        assert_eq!(result["enrichment"]["status"], "committed", "{result}");
        let requests = sent.lock().unwrap();
        assert_eq!(requests.len(), 2);
        for request in requests.iter() {
            let payload: Json = serde_json::from_str(request["input"].as_str().unwrap()).unwrap();
            assert_eq!(payload["said_on"], Json::Null);
            assert_eq!(payload["time_anchor"]["said_on"], Json::Null);
            assert_eq!(payload["source_provenance"]["before_source_id"], parent);
            assert_eq!(payload["current_user_message"], text);
            assert!(payload.get("model").is_none());
            if request["text"]["format"]["name"] == "w3_v3" {
                assert_eq!(
                    payload["candidates"][0]["time_resolutions"][0]["status"],
                    "unknown"
                );
            }
        }
        drop(requests);
        let state = db.open().read().unwrap();
        let record = state
            .repository
            .list_by_owner(&UserId::from("alice"))
            .unwrap()
            .pop()
            .unwrap();
        let crate::contract::RecordView2::Current(view) =
            crate::w4::record_view(&state, record.id).unwrap()
        else {
            panic!("current record required")
        };
        assert_eq!(view.said_on, None);
        assert_eq!(
            view.time_resolutions[0].status,
            crate::contract::ResolutionStatus::Unknown
        );
        assert_eq!(
            service
                .call("memory_source_import_history", args, || false)
                .unwrap()["replayed"],
            true
        );
        assert_eq!(service.enrich_source(child).unwrap(), result);
        assert_eq!(sent.lock().unwrap().len(), 2);
        assert_eq!(db.calls(), 2); // Mock transport only; replays never send again.
        service
            .call("memory_forget", json!({"source_id":parent}), || false)
            .unwrap();
        let deleted = db.open().read().unwrap();
        assert!(deleted.sources[&key].request.is_none());
        assert!(!serde_json::to_string(&deleted).unwrap().contains(text));
        assert!(crate::w4::record_view(&deleted, record.id).is_none());
    }
}

pub(crate) fn assert_nonempty_model_request(request: &Json) {
    use std::io::Write;
    use std::process::{Command, Stdio};
    assert_eq!(request["text"]["format"]["strict"], true);
    let mut check = Command::new("python3")
        .args([
            "-c",
            r#"
import json,re,sys
schema=json.load(sys.stdin)
patterns=[]
def check(node):
    if isinstance(node,dict):
        if node.get('type')=='string':
            if 'enum' in node:
                assert all(s.strip() for s in node['enum'])
            else:
                pattern=node['pattern']
                assert pattern==r'^[\s\S]*\S[\s\S]*$'
                for text in ['', ' \t\n\u3000']:
                    assert re.fullmatch(pattern,text) is None
                for text in ['中文', '第一行\n第二行', '\n中文\n', '0']:
                    assert re.fullmatch(pattern,text) is not None
                patterns.append(pattern)
        if node.get('type')=='null':
            assert 'pattern' not in node
        if node.get('type')=='array':
            assert node.get('minItems',0)==0
        for child in node.values(): check(child)
    elif isinstance(node,list):
        for child in node: check(child)
check(schema)
assert patterns
"#,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    check
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(&request["text"]["format"]["schema"]).unwrap())
        .unwrap();
    let result = check.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    fn remove_pattern(value: &mut Json) {
        match value {
            Json::Object(o) => {
                o.remove("pattern");
                for v in o.values_mut() {
                    remove_pattern(v);
                }
            }
            Json::Array(a) => a.iter_mut().for_each(remove_pattern),
            _ => (),
        }
    }
    let mut prior = request.clone();
    remove_pattern(&mut prior["text"]["format"]["schema"]);
    assert_ne!(
        crate::recall_models::fingerprint(request),
        crate::recall_models::fingerprint(&prior)
    );
}

#[test]
fn af83_all_write_stage_requests_bind_nonempty_schema() {
    let db = Db::new();
    let (key, upstream, output) = w4_case(&db, false);
    let (provider, sent) = mocked(
        &db,
        vec![
            Ok(envelope(&upstream.to_string())),
            Ok(envelope(&w3_for(&upstream).to_string())),
            Ok(envelope(&output.to_string())),
        ],
    );
    assert_eq!(
        run_all(&db, &key, &provider)["enrichment"]["status"],
        "committed"
    );
    let requests = sent.lock().unwrap();
    assert_eq!(requests.len(), 3);
    for (request, name) in requests.iter().zip(["w1_v3", "w3_v3", "w4_v4"]) {
        assert_eq!(request["text"]["format"]["name"], name);
        assert_nonempty_model_request(request);
    }
    for rule in [
        "assertion's holder or actor",
        "nonempty continuous reference span",
        "Never invent placeholder words",
        "history-only span",
        "plan keyword alone never establishes self",
    ] {
        assert!(W1_PROMPT.contains(rule), "{rule}");
    }
}

#[test]
fn af83_w1_diagnostics_are_private_and_do_not_retry() {
    if std::env::var_os("AF83_W1_TRACE_PROBE").is_none() {
        let root = std::env::temp_dir().join(format!("af83-w1-trace-{}", diagnostics::new_id()));
        std::fs::create_dir(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "af83_w1_diagnostics_are_private_and_do_not_retry",
                "--nocapture",
            ])
            .env("AF83_W1_TRACE_PROBE", "1")
            .env("PMR_TRACE_DIR", &root)
            .output()
            .unwrap();
        assert!(
            child.status.success(),
            "{}{}",
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr)
        );
        let rows: Vec<Json> = std::fs::read_dir(&root)
            .unwrap()
            .flat_map(|p| {
                std::fs::read_to_string(p.unwrap().path())
                    .unwrap()
                    .lines()
                    .map(|l| serde_json::from_str(l).unwrap())
                    .collect::<Vec<Json>>()
            })
            .collect();
        std::fs::remove_dir_all(root).unwrap();
        let failures: Vec<_> = rows
            .iter()
            .filter(|r| {
                r["kind"] == "model_check"
                    && r["data"]["stage"] == "W1"
                    && r["data"]["phase"] == "decode"
            })
            .collect();
        assert_eq!(failures.len(), 3);
        for event in failures {
            let d = &event["data"];
            assert_eq!(d["status"], "failed");
            assert_eq!(d["error"], "w1_invalid_output");
            assert!(!d.to_string().contains("private-diagnostic-marker"));
            assert!(!d.to_string().contains("我偏爱松木凳。"));
            assert!(d["rule"] == "schema" || d["rule"] == "nonblank");
        }
        return;
    }
    let text = "我偏爱松木凳。";
    let mut bad = w1(text);
    bad["candidates"]
        .as_array_mut()
        .unwrap()
        .push(w1(text)["candidates"][0].clone());
    bad["candidates"][1]["subject"] = json!({"unresolved":{"text":" "}});
    bad["memory_requests"] = json!([{"kind":"do_not_store","quote":{"text":"不要保存"},
        "target":{"object":"facts","candidate_indices":[0],"quote":null},"deadline":null}]);
    let mut bad_request = w1(text);
    bad_request["memory_requests"] = json!([{"kind":"do_not_store","quote":{"text":" "},
        "target":{"object":"facts","candidate_indices":[0],"quote":null},"deadline":null}]);
    let mut unknown = w1(text);
    unknown["private-diagnostic-marker"] = json!("private-diagnostic-marker");
    for output in [bad, bad_request, unknown] {
        let db = Db::new();
        let key = db.start(text);
        let (provider, sent) = mocked(&db, vec![Ok(envelope(&output.to_string()))]);
        let result = run_all(&db, &key, &provider);
        assert_eq!(result["enrichment"]["status"], "failed");
        let state = db.open().read().unwrap();
        assert_eq!(
            state.sources[&key].error.as_deref(),
            Some("w1_invalid_output")
        );
        assert!(state.sources[&key].request.is_some());
        assert!(
            state
                .repository
                .list_by_owner(&"alice".into())
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            run_all(&db, &key, &provider)["enrichment"]["status"],
            "failed"
        );
        assert_eq!(sent.lock().unwrap().len(), 1);
        assert_eq!(db.calls(), 1);
    }
    assert!(!diagnostics::incomplete());
}
