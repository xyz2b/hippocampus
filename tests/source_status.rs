//! M1.5 source visibility: real SQLite, injected W2/W3/W4, no HTTP or model calls.
mod common;
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
                .join(format!("source-status-{}", diagnostics::new_id()))
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
    let mut store = db.open("alice");
    w4_start_round(&mut store, key, key, false).unwrap();
    run_source(
        &mut store,
        key,
        RuntimeInputBudget::default(),
        |ctx, input| {
            let config = ChatConfig::default();
            let catalog = w2::relation_catalog(&config.predicates, &config.memory_types)?;
            let batch = w2::validate(
                W1Output {
                    candidates: candidates.clone(),
                    memory_requests: vec![],
                },
                &w2::Context {
                    text: &input.text,
                    entities: &[EntityId::from("self")],
                    relations: &catalog,
                    said_on: Some("2026-10-02"),
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
                        verdict: Verdict::Supported,
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

#[test]
fn status_reads_v9_fields_without_text_mutation_or_model_calls() {
    let db = Db::new();
    let (key, id) = db.capture("one", FIRST);
    let before = serde_json::to_value(db.open("alice").read().unwrap()).unwrap();
    let out = db.status(&id);
    assert_eq!(out["source"]["status"], "stored");
    assert_eq!(out["enrichment"]["status"], "not_requested");
    assert_eq!(out["generation"], 0);
    assert!(out["round"].is_null());
    assert_eq!(
        out["provenance"],
        json!({"kind":"mcp_received", "session_id":"source-status",
        "turn_id":"one", "parent_source_id":null,"history_index":null})
    );
    assert_eq!(
        out["time_anchor"],
        json!({"observed_at":AT,"said_on":"2026-10-02","utc_offset_seconds":28800})
    );
    assert_eq!(out["uncommitted_candidates"], json!([]));
    assert_eq!(out["model_calls"], json!([]));
    assert!(out["state_token"].as_str().unwrap().starts_with("sha256:"));
    assert!(!out.to_string().contains(FIRST));
    assert_eq!(
        before,
        serde_json::to_value(db.open("alice").read().unwrap()).unwrap()
    );
    assert_eq!(db.status(&id), out);
    assert_eq!(
        db.open("alice").read().unwrap().sources[&key].task.round,
        None
    );
    assert_eq!(db.calls(), 0);
}

#[test]
fn state_token_tracks_only_this_source_and_its_processing_state() {
    let db = Db::new();
    let (key, id) = db.capture("one", FIRST);
    let initial = db.status(&id)["state_token"].clone();
    db.capture("other", "我使用蓝色笔筒。");
    db.service("alice")
        .call("memory_list", json!({}), || false)
        .unwrap();
    assert_eq!(db.status(&id)["state_token"], initial);
    for edit in 0..3 {
        let before = db.status(&id)["state_token"].clone();
        db.open("alice")
            .transaction(|state| {
                let s = state.sources.get_mut(&key).unwrap();
                match edit {
                    0 => s.enrichment = "queued".into(),
                    1 => {
                        s.task.attempt_id = Some("lease".into());
                        s.task.lease_until = Some(AT);
                    }
                    _ => s.request.as_mut().unwrap()["history"] = json!(["我明确补充了主体。"]),
                }
                Ok(())
            })
            .unwrap();
        assert_ne!(db.status(&id)["state_token"], before);
    }
    assert_eq!(db.calls(), 0);
}

#[test]
fn other_owner_unknown_and_deleted_sources_are_not_exposed() {
    let db = Db::new();
    let (_, id) = db.capture("one", FIRST);
    let mut bob = db.service("bob");
    for absent in [&id, "unknown"] {
        let out = bob.call_with_global_context(
            "memory_source_status",
            json!({"source_id":absent}),
            || false,
        );
        assert_eq!(out.is_error, Some(true));
        assert_eq!(out.structured_content.unwrap()["error"], "source_not_found");
    }
    assert_eq!(
        bob.call("memory_list", json!({}), || false).unwrap()["pending_source_total"],
        0
    );
    db.service("alice")
        .call("memory_forget", json!({"source_id":id}), || false)
        .unwrap();
    let out = db.service("alice").call_with_global_context(
        "memory_source_status",
        json!({"source_id":id}),
        || false,
    );
    assert_eq!(out.is_error, Some(true));
    assert_eq!(out.structured_content.unwrap()["error"], "source_deleted");
    assert_eq!(
        db.service("alice")
            .call("memory_list", json!({}), || false)
            .unwrap()["pending_source_total"],
        0
    );
    assert!(
        bob.call("memory_source_status", json!({"source_id":id}), || false)
            .unwrap_err()
            .to_string()
            .contains("source_not_found")
    );
    assert_eq!(db.calls(), 0);
}

#[test]
fn legacy_staging_and_mixed_source_candidates_have_independent_pages() {
    let db = Db::new();
    let (key, id) = db.capture("mixed", &format!("{FIRST}{SECOND}"));
    let before = db.status(&id)["state_token"].clone();
    let receipt = process(
        &db,
        &key,
        vec![candidate(FIRST, false), candidate(SECOND, true)],
        false,
    );
    assert_eq!(receipt["enrichment"]["status"], "pending");
    assert_eq!(
        receipt["enrichment"]["candidates"][0]["status"],
        "committed"
    );
    let status = db.status(&id);
    assert_ne!(status["state_token"], before);
    assert_eq!(status["round"], 0);
    assert_eq!(
        status["uncommitted_candidates"],
        json!([{"index":1,"status":"unresolved","reasons":["unresolved_subject"]}])
    );
    for turn in ["a", "z"] {
        db.capture(turn, "我使用蓝色笔筒。");
    }
    for turn in ["staged-a", "staged-b"] {
        let mut staged = CandidateMemory::new(
            "alice".into(),
            "self".into(),
            MemoryType::Fact,
            "fact".into(),
            MemoryValue::Text("旧暂存私密值".into()),
            Timestamp(AT),
            Evidence::new(
                turn.into(),
                "source-status".into(),
                turn.into(),
                "旧暂存原文",
            ),
        );
        staged.subject = CandidateSubject::Unresolved("另一个人".into());
        common::stage_fixture(&db.open("alice"), staged);
    }
    let mut service = db.service("alice");
    let all = service.call("memory_list", json!({}), || false).unwrap();
    assert_eq!(all["total"], 1);
    assert_eq!(all["pending_total"], 2);
    assert_eq!(all["pending_source_total"], 3);
    let page = service
        .call(
            "memory_list",
            json!({"offset":1,"limit":1,"source_offset":0,"source_limit":2}),
            || false,
        )
        .unwrap();
    assert_eq!(page["pending"], json!([{"id":2,"status":"Pending"}]));
    assert_eq!(
        page["pending_sources"],
        json!(all["pending_sources"].as_array().unwrap()[..2])
    );
    let next = service
        .call(
            "memory_list",
            json!({"source_offset":2,"source_limit":1}),
            || false,
        )
        .unwrap();
    assert_eq!(next["pending"], all["pending"]);
    assert_eq!(next["pending_sources"][0], all["pending_sources"][2]);
    for private in [FIRST, SECOND, "旧暂存私密值", "旧暂存原文"] {
        assert!(!all.to_string().contains(private));
    }
    for limit in [0, 101] {
        assert!(
            service
                .call("memory_list", json!({"source_limit":limit}), || false)
                .is_err()
        );
    }
    assert!(
        service
            .call(
                "memory_source_status",
                json!({"source_id":id,"owner_id":"bob"}),
                || false
            )
            .is_err()
    );
    assert_eq!(db.calls(), 0);
}

#[test]
fn committed_pending_updates_are_visible_but_not_called_uncommitted() {
    let db = Db::new();
    let (old, _) = db.capture("old", FIRST);
    process(&db, &old, vec![candidate(FIRST, false)], false);
    let (key, id) = db.capture("new", "我使用绿色书架。");
    process(&db, &key, vec![candidate("我使用绿色书架。", false)], true);
    let out = db.status(&id);
    assert_eq!(out["enrichment"]["status"], "committed");
    assert_eq!(out["uncommitted_candidates"], json!([]));
    assert_eq!(
        out["pending_update_candidates"],
        json!([{"index":0,"status":"committed","reasons":["update_scope_incomplete"]}])
    );
    let list = db
        .service("alice")
        .call("memory_list", json!({}), || false)
        .unwrap();
    assert_eq!(list["pending_source_total"], 1);
    assert_eq!(list["pending_sources"][0]["source_id"], id);
    assert_eq!(db.calls(), 0);
}

#[test]
fn source_status_is_advertised_as_read_only_and_requires_source_id() {
    let tool = memory_tools()
        .into_iter()
        .find(|t| t.name == "memory_source_status")
        .unwrap();
    let shape = serde_json::to_value(tool).unwrap();
    assert_eq!(shape["annotations"]["readOnlyHint"], true);
    assert_eq!(shape["annotations"]["openWorldHint"], false);
    assert_eq!(shape["inputSchema"]["required"], json!(["source_id"]));
}

#[test]
fn cli_source_status_is_read_only_and_sends_no_models() {
    let db = Db::new();
    let (_, id) = db.capture("cli-status", FIRST);
    let before = serde_json::to_value(db.open("alice").read().unwrap()).unwrap();
    let args = db.0.with_extension("args.json");
    std::fs::write(&args, serde_json::to_vec(&json!({"source_id":id})).unwrap()).unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_pmr-mcp"))
        .args([
            "--db",
            db.0.to_str().unwrap(),
            "--owner",
            "alice",
            "--call",
            "memory_source_status",
            "--arguments",
            args.to_str().unwrap(),
        ])
        .env("PMR_MODEL_NETWORK", "deny")
        .env_remove("DEEPSEEK_API_KEY")
        .env_remove("DEEPSEEK_PRO_MODEL")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Json = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["source"]["source_id"], id);
    assert_eq!(result["model_calls"], json!([]));
    assert!(!result.to_string().contains(FIRST));
    assert_eq!(
        serde_json::to_value(db.open("alice").read().unwrap()).unwrap(),
        before
    );
    assert_eq!(db.calls(), 0);
}
