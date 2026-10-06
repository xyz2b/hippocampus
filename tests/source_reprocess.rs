//! DB 10 source controls: real SQLite and injected stages, never HTTP or real models.
use hippocampus::chat::{ChatConfig, ChatMemoryService, EnrichmentMode, memory_tools};
use hippocampus::contract::*;
use hippocampus::enrichment::{RuntimeInputBudget, UpstreamBatch, run_source};
use hippocampus::*;
use serde_json::{Value as Json, json};
use std::path::PathBuf;

const AT: i64 = 1_790_899_200;
const FIRST: &str = "我使用银色书架。";
const SECOND: &str = "孩子常用橙色水壶。";

struct Db(PathBuf);
impl Db {
    fn new() -> Self {
        Self(
            std::env::temp_dir()
                .join(format!("source-controls-{}", diagnostics::new_id()))
                .join("memory.sqlite3"),
        )
    }
    fn open(&self, owner: &str) -> SqliteMemoryStore {
        SqliteMemoryStore::open(&self.0, owner.into()).unwrap()
    }
    fn service(&self, owner: &str) -> ChatMemoryService {
        ChatMemoryService::new(
            self.open(owner),
            ChatConfig {
                enrichment_mode: EnrichmentMode::Off,
                ..Default::default()
            },
        )
        .unwrap()
    }
    fn capture(&self, turn: &str, text: &str) -> (String, String) {
        let out = self
            .service("alice")
            .call(
                "memory_remember",
                json!({
                    "text":text,"session_id":"source-status","turn_id":turn,
                    "capture_source":true,"observed_at":AT
                }),
                || false,
            )
            .unwrap();
        let id = out["source"]["source_id"].as_str().unwrap().to_owned();
        let key = self
            .open("alice")
            .read()
            .unwrap()
            .sources
            .into_iter()
            .find(|(_, s)| s.id == id)
            .unwrap()
            .0;
        (key, id)
    }
    fn status(&self, id: &str) -> Json {
        self.service("alice")
            .call("memory_source_status", json!({"source_id":id}), || false)
            .unwrap()
    }
    fn calls(&self) -> i64 {
        rusqlite::Connection::open(&self.0)
            .unwrap()
            .query_row("SELECT count(*) FROM model_calls", [], |r| r.get(0))
            .unwrap()
    }
}
impl Drop for Db {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.0.parent().unwrap());
    }
}

fn candidate(text: &str, unresolved: bool) -> Candidate {
    serde_json::from_value(json!({"kind":"assertion","speech":"user_statement",
        "subject":if unresolved { json!({"unresolved":{"text":"孩子"}}) }
            else { json!({"entity_id":"self"}) },
        "relation":"fact","memory_type":"fact","value":{"kind":"text","text":text,"quote":{"text":text}},
        "sentence":{"text":text},"polarity":"affirm","strength":null,"uncertainty":null,"conditions":[],"times":[]
    })).unwrap()
}

fn process(db: &Db, key: &str, candidates: Vec<Candidate>, pending: bool) -> Json {
    process_review(db, key, candidates, pending, Verdict::Supported)
}
fn process_review(
    db: &Db,
    key: &str,
    candidates: Vec<Candidate>,
    pending: bool,
    verdict: Verdict,
) -> Json {
    let mut store = db.open("alice");
    if store.read().unwrap().sources[key].task.round.is_none() {
        w4_start_round(&mut store, key, key, false).unwrap();
    }
    let entities: Vec<_> = store
        .read()
        .unwrap()
        .entities
        .list_by_owner(&"alice".into())
        .iter()
        .map(|e| e.id.clone())
        .collect();
    run_source(
        &mut store,
        key,
        RuntimeInputBudget::default(),
        |ctx, input| {
            let config = ChatConfig::default();
            let catalog = w2::relation_catalog(&config.predicates, &config.memory_types)?;
            let batch = w2::validate(
                W1Output {
                    candidates: ctx.previous_w2.as_ref().map_or_else(
                        || candidates.clone(),
                        |b| b.candidates.iter().map(|c| c.candidate.clone()).collect(),
                    ),
                    memory_requests: vec![],
                },
                &w2::Context {
                    text: &input.text,
                    entities: &entities,
                    relations: &catalog,
                    said_on: "2026-10-02",
                    utc_offset_seconds: 28_800,
                    task_id: ctx.task_id.as_ref(),
                },
            )?;
            let reviews = W3Output {
                reviews: batch
                    .candidates
                    .iter()
                    .filter(|c| c.status != w2::Status::Rejected)
                    .map(|c| CandidateReview {
                        candidate: c.index,
                        verdict,
                        problems: vec![],
                        episode_value: EpisodeValue::NotApplicable,
                    })
                    .collect(),
                request_reviews: vec![],
                coverage: W3Coverage {
                    status: CoverageStatus::Complete,
                    missing: vec![],
                },
            }
            .validate(&batch, &ctx.source_binding, &input.text)?;
            Ok(UpstreamBatch {
                w2: batch,
                review: Some(reviews),
            })
        },
        |_, input, _| {
            Ok(Some(W4Output {
                decisions: input
                    .candidates
                    .iter()
                    .map(|c| W4Decision {
                        candidate: c.index,
                        status: if pending {
                            DecisionStatus::Pending
                        } else {
                            DecisionStatus::Decided
                        },
                        action: (!pending).then_some(Action::New),
                        targets: vec![],
                        resolves_relations: vec![],
                        basis: vec![EvidenceRef {
                            source: c.source.clone(),
                            quote: c.candidate.sentence.clone(),
                        }],
                        effective: None,
                        relation_scope: None,
                        pending_reason: pending.then_some(PendingReason::ScopeIncomplete),
                    })
                    .collect(),
            }))
        },
    )
    .unwrap()
}

fn register(db: &Db) {
    db.open("alice")
        .transaction(|state| {
            state.entities.upsert(Entity {
                id: "child".into(),
                owner_id: "alice".into(),
                entity_type: EntityType::Other("person".into()),
                canonical_name: "孩子".into(),
                aliases: vec![],
            });
            Ok(())
        })
        .unwrap();
}
fn command(db: &Db, id: &str, request: &str, mode: &str) -> Json {
    json!({"source_id":id,"request_id":request,"expected_state_token":db.status(id)["state_token"],"mode":mode})
}
fn confirmation(db: &Db, id: &str, request: &str, index: usize, citation: &str) -> Json {
    let mut input = command(db, id, request, "confirm_subject");
    input["candidate_index"] = json!(index);
    input["entity_id"] = json!("child");
    input["confirmation"] = json!({"source":{"kind":"stored","source_id":citation},"quote":{"text":"这个孩子是青芽。"}});
    input
}
fn control(db: &Db, input: Json) -> PersistenceResult<Json> {
    db.service("alice")
        .call("memory_source_reprocess", input, || false)
}
fn memories(db: &Db) -> Vec<Memory> {
    db.open("alice")
        .read()
        .unwrap()
        .repository
        .list_by_owner(&"alice".into())
        .unwrap()
}
fn mixed(db: &Db) -> (String, String, String) {
    let (key, id) = db.capture("mixed", &format!("{FIRST}{SECOND}"));
    let out = process(
        db,
        &key,
        vec![candidate(FIRST, false), candidate(SECOND, true)],
        false,
    );
    assert_eq!(out["enrichment"]["candidates"][0]["status"], "committed");
    assert_eq!(out["enrichment"]["candidates"][1]["status"], "unresolved");
    let (_, citation) = db.capture("confirmation", "这个孩子是青芽。");
    register(db);
    (key, id, citation)
}

#[test]
fn mixed_confirmation_is_stable_idempotent_and_only_submits_selected_candidate() {
    let db = Db::new();
    let (key, id, citation) = mixed(&db);
    let old = db.open("alice").read().unwrap().sources[&key].clone();
    let original = memories(&db)[0].clone();
    let input = confirmation(&db, &id, "confirm-one", 1, &citation);
    let out = control(&db, input.clone()).unwrap();
    assert_eq!(out["generation"], 1);
    assert_eq!(out["round"], 1);
    assert_eq!(out["enqueued"], false);
    assert_eq!(out["model_calls"], json!([]));
    assert!(!out.to_string().contains(SECOND));
    assert!(!out.to_string().contains("这个孩子是青芽。"));
    let current = db.open("alice").read().unwrap().sources[&key].clone();
    assert_eq!(current.received_sequence, old.received_sequence);
    assert_eq!(current.observed_at, old.observed_at);
    assert_eq!(current.request, old.request);
    assert!(current.order_sequence() > old.order_sequence());
    assert_eq!(current.management.audits.len(), 1);
    assert_eq!(current.management.confirmations[0].candidate_index, 1);
    assert_eq!(db.status(&id)["uncommitted_candidates"][0]["index"], 1);
    let before = serde_json::to_value(db.open("alice").read().unwrap()).unwrap();
    let replay = control(&db, input.clone()).unwrap();
    assert_eq!(replay["replayed"], true);
    assert_eq!(
        before,
        serde_json::to_value(db.open("alice").read().unwrap()).unwrap()
    );
    let mut conflict = input.clone();
    conflict["enqueue"] = json!(true);
    assert_eq!(
        control(&db, conflict).unwrap_err().to_string(),
        "request_id_conflict"
    );
    // Reopen the DB and finish from the persisted candidate array, with no new W1 extraction.
    let out = process(&db, &key, vec![], false);
    assert_eq!(out["enrichment"]["candidates"][0]["status"], "committed");
    assert_eq!(out["enrichment"]["candidates"][1]["status"], "committed");
    let stored = memories(&db);
    assert_eq!(stored.len(), 2);
    assert_eq!(
        serde_json::to_value(&stored[0]).unwrap(),
        serde_json::to_value(original).unwrap()
    );
    assert_eq!(stored[1].subject_id, EntityId::from("child"));
    assert_eq!(stored[1].evidence[0].quote, SECOND); // Identity evidence is not new fact-value evidence.
    assert_eq!(db.calls(), 0);
    w4_start_round(&mut db.open("alice"), &key, "selected-rejudge", true).unwrap();
    assert!(
        w4_progress(&db.open("alice"), &key)
            .unwrap()
            .candidates
            .iter()
            .all(|c| c.index == 1)
    );
    assert_eq!(
        control(&db, command(&db, &id, "reextract-mixed", "reextract"))
            .unwrap_err()
            .to_string(),
        "rebuild_requires_uncommitted_source"
    );
    assert_eq!(
        control(
            &db,
            confirmation(&db, &id, "confirm-committed", 1, &citation)
        )
        .unwrap_err()
        .to_string(),
        "invalid_confirmation"
    );
}

#[test]
fn invalid_controls_are_atomic_and_authority_is_owner_scoped() {
    let db = Db::new();
    let (key, id, citation) = mixed(&db);
    let valid = confirmation(&db, &id, "invalid-case", 1, &citation);
    for mutation in 0..7 {
        let mut input = valid.clone();
        match mutation {
            0 => input["confirmation"]["source"] = json!({"kind":"current"}),
            1 => input["confirmation"]["quote"]["text"] = json!("并未说过这句话"),
            2 => input["entity_id"] = json!("unknown"),
            3 => input["candidate_index"] = json!(0),
            4 => input["candidate_index"] = json!(500),
            5 => input["confirmation"]["source"]["source_id"] = json!("another-owner-id"),
            _ => input["confirmation"]["quote"]["text"] = json!(""),
        }
        let before = serde_json::to_value(db.open("alice").read().unwrap()).unwrap();
        assert_eq!(
            control(&db, input).unwrap_err().to_string(),
            "invalid_confirmation"
        );
        assert_eq!(
            before,
            serde_json::to_value(db.open("alice").read().unwrap()).unwrap()
        );
    }
    let mut stale = valid.clone();
    stale["expected_state_token"] = json!("stale");
    assert_eq!(
        control(&db, stale).unwrap_err().to_string(),
        "source_changed"
    );
    assert_eq!(
        db.service("bob")
            .call("memory_source_reprocess", valid.clone(), || false)
            .unwrap_err()
            .to_string(),
        "source_not_found"
    );
    db.open("alice")
        .transaction(|state| {
            let source = state.sources.get_mut(&key).unwrap();
            source.task.attempt_id = Some("active".into());
            source.task.lease_until = Some(i64::MAX);
            Ok(())
        })
        .unwrap();
    assert_eq!(control(&db, valid).unwrap_err().to_string(), "source_busy");
    assert_eq!(db.calls(), 0);
}

#[test]
fn reextract_starts_fresh_generation_and_refuses_any_committed_action_or_deleted_source() {
    let db = Db::new();
    let (key, id) = db.capture("pending", SECOND);
    process(&db, &key, vec![candidate(SECOND, true)], false);
    let old = db.open("alice").read().unwrap().sources[&key].clone();
    let input = command(&db, &id, "rebuild", "reextract");
    let out = control(&db, input.clone()).unwrap();
    assert_eq!(out["generation"], 1);
    assert!(
        db.status(&id)["uncommitted_candidates"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let rebuilt = process(&db, &key, vec![candidate(SECOND, false)], false);
    assert_eq!(rebuilt["enrichment"]["status"], "committed");
    assert_eq!(
        db.open("alice").read().unwrap().sources[&key].received_sequence,
        old.received_sequence
    );
    let replay = control(&db, input).unwrap();
    assert_eq!(replay["generation"], 1);
    assert_eq!(replay["replayed"], true);
    let fail = control(&db, command(&db, &id, "again", "reextract"))
        .unwrap_err()
        .to_string();
    assert_eq!(fail, "rebuild_requires_uncommitted_source");
    db.service("alice")
        .call("memory_forget", json!({"source_id":id}), || false)
        .unwrap();
    assert_eq!(
        control(&db, command_with_token(&id, "deleted", "reextract", "old"))
            .unwrap_err()
            .to_string(),
        "source_deleted"
    );
    assert_eq!(
        db.open("alice").read().unwrap().sources[&key].management,
        SourceManagement::default()
    );
    assert_eq!(db.calls(), 0);
}
fn command_with_token(id: &str, request: &str, mode: &str, token: &str) -> Json {
    json!({"source_id":id,"request_id":request,"mode":mode,"expected_state_token":token})
}

#[test]
fn expired_executor_cannot_write_into_successor_generation_or_relax_other_source_barriers() {
    let db = Db::new();
    let (older, _) = db.capture("older", FIRST);
    let (key, id) = db.capture("late", SECOND);
    w4_start_round(&mut db.open("alice"), &key, "old-op", false).unwrap();
    let claim = w4_claim_round(&mut db.open("alice"), &key).unwrap();
    db.open("alice")
        .transaction(|state| {
            state.sources.get_mut(&key).unwrap().task.lease_until = Some(0);
            Ok(())
        })
        .unwrap();
    let out = control(&db, command(&db, &id, "new-op", "reextract")).unwrap();
    assert_eq!(out["generation"], 1);
    let before = serde_json::to_value(db.open("alice").read().unwrap()).unwrap();
    assert!(
        w4_fail_attempt(&mut db.open("alice"), &claim, "failed")
            .unwrap_err()
            .to_string()
            .starts_with("claim_lost")
    );
    assert_eq!(
        before,
        serde_json::to_value(db.open("alice").read().unwrap()).unwrap()
    );
    assert_eq!(
        process(&db, &key, vec![candidate(SECOND, false)], false)["enrichment"]["status"],
        "committed"
    );
    let fail = process(&db, &older, vec![candidate(FIRST, false)], false);
    assert_eq!(fail["enrichment"]["status"], "needs_rejudge");
    assert_eq!(memories(&db).len(), 1);
}

#[test]
fn deleting_confirmation_invalidates_only_dependent_identity_and_never_replays_it() {
    let db = Db::new();
    let (key, id, citation) = mixed(&db);
    let input = confirmation(&db, &id, "confirm-delete", 1, &citation);
    control(&db, input.clone()).unwrap();
    process(&db, &key, vec![], false);
    let before = memories(&db)[0].clone();
    // A queued/failed generation audit contains the USER confirmation too.
    db.service("alice")
        .call("memory_forget", json!({"source_id":citation}), || false)
        .unwrap();
    let state = db.open("alice").read().unwrap();
    let source = &state.sources[&key];
    assert!(source.request.is_some());
    assert!(source.management.confirmations.is_empty());
    assert_eq!(source.enrichment, "needs_rejudge");
    assert_eq!(
        serde_json::to_value(&memories(&db)[0]).unwrap(),
        serde_json::to_value(before).unwrap()
    );
    assert_eq!(memories(&db)[1].status, MemoryStatus::Deleted);
    assert!(
        !serde_json::to_string(&state)
            .unwrap()
            .contains("这个孩子是青芽。")
    );
    assert_eq!(
        control(&db, input).unwrap_err().to_string(),
        "source_changed"
    );
    assert_eq!(db.calls(), 0);
}

#[test]
fn enqueue_false_never_auto_runs_and_true_requires_finite_network_authorization() {
    let db = Db::new();
    let (key, id) = db.capture("not-enqueued", SECOND);
    control(&db, command(&db, &id, "offline-rebuild", "reextract")).unwrap();
    let mut service = ChatMemoryService::new(
        db.open("alice"),
        ChatConfig {
            enrichment_mode: EnrichmentMode::Deferred,
            model_budget: Some(ModelBudgetConfig {
                group_id: "fake".into(),
                max_calls: 3,
                max_operation_calls: 3,
                ..Default::default()
            }),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!service.run_background_once().unwrap());
    assert_eq!(
        db.open("alice").read().unwrap().sources[&key]
            .task
            .accepted_mode
            .as_deref(),
        Some("off")
    );
    let mut input = command(&db, &id, "unauthorized", "reextract");
    input["enqueue"] = json!(true);
    assert_eq!(
        control(&db, input).unwrap_err().to_string(),
        "operation_not_authorized"
    );
    assert_eq!(db.calls(), 0);
}

#[test]
fn v9_fixture_migrates_all_owners_without_rewriting_old_receipts_or_enqueuing() {
    let db = Db::new();
    std::fs::create_dir_all(db.0.parent().unwrap()).unwrap();
    let sql = rusqlite::Connection::open(&db.0).unwrap();
    sql.execute_batch(include_str!("fixtures/w5_storage/v9-66f52cf.sql"))
        .unwrap();
    let before: Vec<(String, String)> = sql
        .prepare("SELECT owner_id,state_json FROM memory_profiles ORDER BY owner_id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(before.len(), 2);
    let read_only = SqliteMemoryStore::open_read_only(&db.0, "bob".into()).unwrap();
    assert_eq!(read_only.read().unwrap().sources.len(), 1);
    drop(read_only);
    for _ in 0..2 {
        for owner in ["alice", "bob"] {
            let mut store = db.open(owner);
            let state = store.read().unwrap();
            for source in state.sources.values() {
                assert_eq!(source.task.generation, 0);
                assert_eq!(source.management, SourceManagement::default());
                assert_eq!(source.enrichment, "not_requested");
            }
            // Explicit save has defaults only: no new controls, receipts or automatic task enrollment.
            store.transaction(|_| Ok(())).unwrap();
        }
    }
    assert_eq!(
        sql.pragma_query_value::<i64, _>(None, "user_version", |row| row.get(0))
            .unwrap(),
        10
    );
    for (owner, raw) in before {
        let old: Json = serde_json::from_str(&raw).unwrap();
        let current: Json = serde_json::to_value(db.open(&owner).read().unwrap()).unwrap();
        assert_eq!(current["receipts"], old["receipts"]);
        assert_eq!(current["sources"], old["sources"]);
    }
    assert_eq!(db.calls(), 0);
}

#[test]
fn confirmation_still_requires_semantic_w3_support_and_delete_blocks_late_commit() {
    let db = Db::new();
    let (key, id) = db.capture("unresolved", SECOND);
    process(&db, &key, vec![candidate(SECOND, true)], false);
    let (_, citation) = db.capture("confirming", "这个孩子是青芽。");
    register(&db);
    control(&db, confirmation(&db, &id, "reject-confirm", 0, &citation)).unwrap();
    let out = process_review(&db, &key, vec![], false, Verdict::Contradicted);
    assert_eq!(out["enrichment"]["candidates"][0]["status"], "rejected");
    assert_eq!(
        out["enrichment"]["candidates"][0]["reasons"],
        json!(["review_contradicted"])
    );
    assert!(memories(&db).is_empty());
    control(&db, confirmation(&db, &id, "fresh-confirm", 0, &citation)).unwrap();
    let old = w4_claim_round(&mut db.open("alice"), &key).unwrap();
    db.service("alice")
        .call("memory_forget", json!({"source_id":citation}), || false)
        .unwrap();
    let before = serde_json::to_value(db.open("alice").read().unwrap()).unwrap();
    assert!(w4_fail_attempt(&mut db.open("alice"), &old, "failed").is_err());
    assert!(w4_save_inputs(&mut db.open("alice"), &old, &[], &Default::default()).is_err());
    assert_eq!(
        before,
        serde_json::to_value(db.open("alice").read().unwrap()).unwrap()
    );
    assert!(!before.to_string().contains("这个孩子是青芽。"));
    assert_eq!(db.calls(), 0);
}

#[test]
fn source_less_authority_barrier_is_preserved_for_other_sources() {
    let db = Db::new();
    let (older, _) = db.capture("older-before-authority", FIRST);
    let (key, id) = db.capture("rebuild-before-authority", SECOND);
    register(&db);
    let barrier = db
        .open("alice")
        .read()
        .unwrap()
        .source_less_barrier_sequence;
    assert!(barrier >= 2);
    control(&db, command(&db, &id, "authorized-rebuild", "reextract")).unwrap();
    assert_eq!(
        db.open("alice")
            .read()
            .unwrap()
            .source_less_barrier_sequence,
        barrier
    );
    assert!(db.open("alice").read().unwrap().sources[&key].order_sequence() > barrier);
    assert_eq!(
        process(&db, &key, vec![candidate(SECOND, false)], false)["enrichment"]["status"],
        "committed"
    );
    assert_eq!(
        process(&db, &older, vec![candidate(FIRST, false)], false)["enrichment"]["status"],
        "needs_rejudge"
    );
    assert_eq!(memories(&db).len(), 1);
}

#[test]
fn reextract_refuses_committed_no_product_actions_even_without_source_summary() {
    let db = Db::new();
    let (key, id) = db.capture("committed-no-product", FIRST);
    process(&db, &key, vec![candidate(FIRST, false)], false);
    // Restore a durable committed-action/crashed-summary window; the guard must inspect receipts,
    // not just products or the source's presentation state.
    db.open("alice")
        .transaction(|state| {
            for (receipt_key, receipt) in &mut state.w5_receipts {
                if receipt_key.starts_with(&format!("{key}#")) {
                    receipt.products.clear();
                }
            }
            let source = state.sources.get_mut(&key).unwrap();
            source.enrichment = "running".into();
            source.task.attempt_id = None;
            source.task.lease_until = None;
            source
                .result
                .as_mut()
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove("w4_summary");
            Ok(())
        })
        .unwrap();
    assert_eq!(
        control(&db, command(&db, &id, "no-product-rebuild", "reextract"))
            .unwrap_err()
            .to_string(),
        "rebuild_requires_uncommitted_source"
    );
    assert_eq!(db.calls(), 0);
}

#[test]
fn tool_schema_exposes_controls_and_stored_only_confirmation_without_loose_json() {
    let tool = memory_tools()
        .into_iter()
        .find(|t| t.name == "memory_source_reprocess")
        .unwrap();
    let schema = serde_json::to_value(tool.input_schema).unwrap();
    assert_eq!(schema["additionalProperties"], false);
    assert!(
        schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "expected_state_token")
    );
    assert_eq!(
        schema["properties"]["confirmation"]["anyOf"][1]["properties"]["source"]["properties"]["kind"]
            ["const"],
        "stored"
    );
}

#[test]
fn legacy_committed_records_and_clarify_receipts_cannot_be_reextracted() {
    for only_receipt in [false, true] {
        let db = Db::new();
        let (key, id) = db.capture("legacy-action", FIRST);
        if !only_receipt {
            process(&db, &key, vec![candidate(FIRST, false)], false);
        }
        db.open("alice")
            .transaction(|state| {
                state.w5_receipts.clear();
                let source = state.sources.get_mut(&key).unwrap();
                source.task = Default::default();
                source.result = Some(json!({"outcomes":[{"Pending":{"staged_id":77}}]}));
                source.task.attempt_id = None;
                source.task.lease_until = None;
                source.enrichment = "pending".into();
                if only_receipt {
                    let mut snapshot = serde_json::to_value(&*state)?;
                    snapshot["receipts"]["legacy-clarification"] = json!({
                        "request":{"staged_id":77},"result":{"outcome":{"Ingested":{"Applied":{}}}}
                    });
                    *state = serde_json::from_value(snapshot)?;
                }
                Ok(())
            })
            .unwrap();
        assert_eq!(
            control(&db, command(&db, &id, "legacy-rebuild", "reextract"))
                .unwrap_err()
                .to_string(),
            "rebuild_requires_uncommitted_source"
        );
        assert_eq!(db.calls(), 0);
    }
}

#[test]
fn source_reprocess_invalidates_persisted_recall_receipts_only_for_its_source() {
    let db = Db::new();
    let (_, id) = db.capture("controlled", SECOND);
    let (_, other) = db.capture("independent", FIRST);
    db.open("alice")
        .transaction(|state| {
            let mut snapshot = serde_json::to_value(&*state)?;
            for (key, source_id) in [("related", &id), ("independent", &other)] {
                snapshot["receipts"][key] = json!({"request":{"form":"recall_metadata_v1"},
                    "result":{"state":"done","source_ids":[source_id]}});
            }
            *state = serde_json::from_value(snapshot)?;
            Ok(())
        })
        .unwrap();
    control(&db, command(&db, &id, "invalidate-recall", "reextract")).unwrap();
    let state = serde_json::to_value(db.open("alice").read().unwrap()).unwrap();
    assert_eq!(
        state["receipts"]["related"]["result"],
        json!({"state":"invalidated"})
    );
    assert_eq!(
        state["receipts"]["independent"]["result"],
        json!({"state":"done","source_ids":[other]})
    );
    assert_eq!(db.calls(), 0);
}
