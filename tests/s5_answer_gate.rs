mod common;

use hippocampus::chat::{
    ChatConfig, ChatMemoryService, EnrichmentMode, RetrievalMode, memory_tools,
};
use hippocampus::*;
use serde_json::{Value, json};
use std::cell::Cell;

struct Db(std::path::PathBuf);

impl Db {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("s5-answer-gate-{}.sqlite3", diagnostics::new_id())))
    }

    fn open(&self) -> SqliteMemoryStore {
        SqliteMemoryStore::open(&self.0, UserId::from("alice")).unwrap()
    }
}

impl Drop for Db {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn source(id: &str, text: &str, sequence: u64) -> SourceRecord {
    serde_json::from_value(json!({
        "id":id,"session_id":"session","turn_id":id,"observed_at":100,
        "received_sequence":sequence,
        "request":{"text":text,"capture_source":true},
        "enrichment":"committed","error":null,"result":null,"base_revision":0
    }))
    .unwrap()
}

fn fixture() -> (Db, ChatMemoryService) {
    let db = Db::new();
    let mut store = db.open();
    let rows = [
        (
            "E1",
            "松石47",
            "项目松石47选用 SQLite，Redis 暂时不引入。",
            "项目松石47选用 SQLite",
            "技术选择",
            "SQLite",
        ),
        (
            "E2",
            "松石47",
            "松石47当前只部署在一台机器上；暂不引入 Redis，是为了减少维护成本。",
            "松石47当前只部署在一台机器上",
            "部署现状",
            "一台机器",
        ),
        (
            "D1",
            "赤铜82",
            "赤铜82的技术选择是 PostgreSQL。",
            "赤铜82的技术选择是 PostgreSQL",
            "技术选择",
            "PostgreSQL",
        ),
    ];
    store
        .transaction(|state| {
            for name in ["松石47", "赤铜82"] {
                state.entities.upsert(Entity {
                    id: EntityId::from(name),
                    owner_id: UserId::from("alice"),
                    entity_type: EntityType::Other("project".into()),
                    canonical_name: name.into(),
                    aliases: Vec::new(),
                });
            }
            for (index, (id, _, text, _, _, _)) in rows.iter().enumerate() {
                state
                    .sources
                    .insert((*id).into(), source(id, text, index as u64 + 1));
            }
            state.with_engine(InMemoryPredicateRegistry::default(), |engine, _| {
                for (id, subject, _, quote, predicate, value) in rows {
                    engine.ingest(common::grounded(CandidateMemory::new(
                        UserId::from("alice"),
                        EntityId::from(subject),
                        MemoryType::Fact,
                        Predicate::from(predicate),
                        MemoryValue::Text(value.into()),
                        Timestamp(100),
                        Evidence::new(
                            EvidenceId(id.into()),
                            SessionId::from("session"),
                            TurnId::from(id),
                            quote,
                        ),
                    )))?;
                }
                Ok(())
            })
        })
        .unwrap();
    let service = ChatMemoryService::new(
        db.open(),
        ChatConfig {
            retrieval_mode: Some(RetrievalMode::Local),
            ..ChatConfig::default()
        },
    )
    .unwrap();
    (db, service)
}

fn rejected(value: &Value) {
    assert_eq!(value["approved"], false, "{value}");
    assert_eq!(value["approved_source_ids"], json!([]), "{value}");
    let safe = value["safe_answer"].as_str().unwrap();
    assert!(
        safe.contains("证据不足") || safe.contains("无法判断"),
        "{value}"
    );
}

fn contract_shape(value: &Value) {
    assert_eq!(value["version"], 1, "{value}");
    assert!(value["query"].is_string(), "{value}");
    assert!(
        value["context"]
            .as_array()
            .unwrap()
            .iter()
            .all(Value::is_string)
    );
    for field in ["task_id", "read_revision", "source_revision", "safe_answer"] {
        assert!(value.get(field).is_some(), "missing {field}: {value}");
    }
    let ids = value["safe_source_ids"].as_array().unwrap();
    let sources = value["sources"].as_array().unwrap();
    assert!(ids.iter().all(Value::is_string));
    assert_eq!(ids.len(), sources.len(), "{value}");
    for (id, source) in ids.iter().zip(sources) {
        assert_eq!(id, &source["source_id"]);
        assert!(source["quote"].is_string());
    }
}

#[test]
fn final_gate_rechecks_the_original_question_instead_of_trusting_narrow_sources() {
    assert!(
        memory_tools()
            .iter()
            .any(|tool| tool.name == "memory_validate_answer")
    );
    let (_db, mut service) = fixture();
    let answer = service
        .call(
            "memory_validate_answer",
            json!({
                "query":"松石47的技术选择和部署限制是否一致？",
                "context":[],
                "draft_answer":"一致。松石47选用了 SQLite，目前部署在一台机器上。",
                "source_ids":["E1","E2"]
            }),
            || false,
        )
        .unwrap();
    rejected(&answer);
    assert!(
        answer["safe_answer"].as_str().unwrap().contains("部署限制"),
        "{answer}"
    );
}

#[test]
fn final_gate_never_cites_an_unrelated_source_for_an_unanswered_question() {
    let (_db, mut service) = fixture();
    let answer = service
        .call(
            "memory_validate_answer",
            json!({
                "query":"琥珀93的维护预算是多少？",
                "context":[],
                "draft_answer":"琥珀93的维护预算是 500 元。",
                "source_ids":["E1"]
            }),
            || false,
        )
        .unwrap();
    rejected(&answer);
    assert!(!answer["safe_answer"].as_str().unwrap().contains("E1"));
}

#[test]
fn final_gate_asks_for_an_unbound_subject() {
    let (_db, mut service) = fixture();
    let answer = service
        .call(
            "memory_validate_answer",
            json!({
                "query":"先搁置的那种内存缓存服务叫什么？",
                "context":[],
                "draft_answer":"Redis。",
                "source_ids":["E1"]
            }),
            || false,
        )
        .unwrap();
    assert_eq!(answer["approved"], false, "{answer}");
    assert_eq!(answer["safe_source_ids"], json!([]));
    assert_eq!(answer["status"], "clarification_required", "{answer}");
}

#[test]
fn final_gate_fails_closed_when_fresh_recall_fails() {
    let (_db, mut service) = fixture();
    let checks = Cell::new(0);
    let answer = service
        .call(
            "memory_validate_answer",
            json!({"query":"松石47的技术选择是什么？",
                "draft_answer":"SQLite。","source_ids":["E1"]}),
            || {
                let count = checks.get();
                checks.set(count + 1);
                count > 0
            },
        )
        .unwrap();
    assert_eq!(answer["approved"], false, "{answer}");
    assert_eq!(answer["status"], "recall_failed", "{answer}");
    assert_eq!(answer["safe_source_ids"], json!([]));
    contract_shape(&answer);
}

#[test]
fn final_gate_cannot_approve_a_decoy_even_when_a_relevant_source_exists() {
    let (_db, mut service) = fixture();
    let answer = service
        .call(
            "memory_validate_answer",
            json!({
                "query":"松石47的技术选择是什么？",
                "context":[],
                "draft_answer":"松石47选用 SQLite。",
                "source_ids":["E1","D1"]
            }),
            || false,
        )
        .unwrap();
    assert!(answer["approved"].is_boolean(), "{answer}");
    assert!(
        !answer["approved_source_ids"]
            .as_array()
            .unwrap()
            .contains(&json!("D1")),
        "{answer}"
    );
}

#[test]
fn recall_returns_safe_answers_without_a_separate_validation_call() {
    let (_db, mut service) = fixture();
    for (query, status) in [
        ("先搁置的那种内存缓存服务叫什么？", "clarification_required"),
        (
            "松石47的技术选择和部署限制是否一致？",
            "comparison_evidence_incomplete",
        ),
        ("琥珀93的维护预算是多少？", "insufficient_evidence"),
        ("松石47的技术选择是什么？", "answer_not_verified"),
    ] {
        let recalled = service
            .call("memory_recall", json!({"query":query}), || false)
            .unwrap();
        let contract = &recalled["answer_contract"];
        contract_shape(contract);
        assert_eq!(contract["status"], status, "{query}: {recalled}");
        assert_eq!(contract["query"], query);
        assert_eq!(contract["context"], json!([]));
        assert_eq!(contract["safe_source_ids"], json!([]));
        assert_eq!(contract["sources"], json!([]));
        assert_eq!(contract["read_revision"], recalled["read_revision"]);
        assert_eq!(recalled["model_calls"], json!([]));
        assert!(
            contract["safe_answer"]
                .as_str()
                .is_some_and(|s| !s.is_empty())
        );
        if status == "comparison_evidence_incomplete" {
            assert!(
                contract["safe_answer"]
                    .as_str()
                    .unwrap()
                    .contains("部署限制")
            );
        }
        if status == "answer_not_verified" {
            assert!(!recalled["source_evidence"].as_array().unwrap().is_empty());
        }
    }
}

#[test]
fn recall_contract_does_not_guess_person_object_event_or_negated_subjects() {
    let (_db, mut service) = fixture();
    for (query, context) in [
        ("他喜欢什么？", Vec::<&str>::new()),
        ("那件东西存在哪里？", vec![]),
        ("那次预约改到什么时候？", vec![]),
        ("先搁置的那种内存缓存服务叫什么？", vec!["不是松石47。"]),
    ] {
        let recalled = service
            .call(
                "memory_recall",
                json!({"query":query,"context":context}),
                || false,
            )
            .unwrap();
        let contract = &recalled["answer_contract"];
        assert_eq!(contract["status"], "clarification_required", "{recalled}");
        assert_eq!(contract["safe_source_ids"], json!([]));
        assert_eq!(contract["sources"], json!([]));
        assert_eq!(contract["context"], json!(context));
        assert!(!contract["safe_answer"].as_str().unwrap().contains("Redis"));
        assert_eq!(recalled["model_calls"], json!([]));
    }
}

#[test]
fn prepare_returns_the_same_contract_without_conflating_capture_and_recall() {
    let (db, _) = fixture();
    let mut service = ChatMemoryService::new(
        db.open(),
        ChatConfig {
            retrieval_mode: Some(RetrievalMode::Local),
            enrichment_mode: EnrichmentMode::Off,
            ..ChatConfig::default()
        },
    )
    .unwrap();
    for (turn_id, query, status) in [
        ("question", "那件东西存在哪里？", "clarification_required"),
        ("statement", "我今天继续整理书房。", "not_applicable"),
    ] {
        let prepared = service
            .call(
                "memory_prepare",
                json!({"user_turn":{"session_id":"contract","turn_id":turn_id,
                    "text":query,"capture_source":true}}),
                || false,
            )
            .unwrap();
        assert_eq!(prepared["remember"]["status"], "completed", "{prepared}");
        assert_eq!(prepared["remember"]["result"]["source"]["status"], "stored");
        assert_eq!(
            prepared["remember"]["result"]["enrichment"]["status"],
            "not_requested"
        );
        assert_eq!(prepared["recall"]["status"], "completed");
        let contract = &prepared["recall"]["result"]["answer_contract"];
        contract_shape(contract);
        assert_eq!(contract["status"], status, "{prepared}");
        assert_eq!(contract["query"], query);
        assert_eq!(contract["safe_source_ids"], json!([]));
        assert_eq!(contract["sources"], json!([]));
        if status == "not_applicable" {
            assert_eq!(contract["safe_answer"], Value::Null);
        }
        assert_eq!(prepared["recall"]["result"]["model_calls"], json!([]));
    }
}

#[test]
fn bootstrap_and_non_questions_do_not_force_a_factual_answer() {
    let (db, _) = fixture();
    for mode in [RetrievalMode::Local, RetrievalMode::Assisted] {
        let mut service = ChatMemoryService::new(
            db.open(),
            ChatConfig {
                retrieval_mode: Some(mode),
                ..ChatConfig::default()
            },
        )
        .unwrap();
        for arguments in [
            json!({"query":"全局回答要求","bootstrap":true}),
            json!({"query":"松石47 技术选择"}),
        ] {
            let recalled = service.call("memory_recall", arguments, || false).unwrap();
            let contract = &recalled["answer_contract"];
            contract_shape(contract);
            assert_eq!(contract["status"], "not_applicable", "{recalled}");
            assert_eq!(contract["safe_answer"], Value::Null);
            assert_eq!(contract["safe_source_ids"], json!([]));
            assert_eq!(contract["sources"], json!([]));
            assert_eq!(recalled["model_calls"], json!([]));
        }
    }
}

#[test]
fn failed_recall_keeps_the_mcp_error_and_cannot_offer_sources() {
    let (_db, mut service) = fixture();
    let result = service.call_with_global_context(
        "memory_recall",
        json!({"query":"松石47的技术选择是什么？"}),
        || true,
    );
    assert_eq!(result.is_error, Some(true));
    let body = result.structured_content.unwrap();
    assert!(body["error"].is_string(), "{body}");
    let contract = &body["answer_contract"];
    contract_shape(contract);
    assert_eq!(contract["status"], "recall_failed", "{body}");
    assert_eq!(contract["safe_source_ids"], json!([]));
    assert_eq!(contract["sources"], json!([]));
    assert_eq!(body["model_calls"], json!([]));
}

#[test]
fn legacy_validation_remains_unsupported_without_a_model_request() {
    let (db, _) = fixture();
    let mut service = ChatMemoryService::new(
        db.open(),
        ChatConfig {
            retrieval_mode: Some(RetrievalMode::Legacy),
            ..ChatConfig::default()
        },
    )
    .unwrap();
    let answer = service
        .call(
            "memory_validate_answer",
            json!({"query":"松石47的技术选择是什么？",
                "draft_answer":"SQLite。","source_ids":["E1"]}),
            || false,
        )
        .unwrap();
    assert_eq!(answer["status"], "unsupported_retrieval_mode");
    assert_eq!(answer["approved"], false);
    assert_eq!(answer["approved_source_ids"], json!([]));
    assert_eq!(answer["safe_source_ids"], json!([]));
    assert_eq!(answer["model_calls"], json!([]));
    contract_shape(&answer);
}

#[test]
fn v1_contract_binds_the_same_question_context_and_scope_across_tools() {
    let (_db, mut service) = fixture();
    for request in [
        json!({"query":"琥珀93的维护预算是多少？"}),
        json!({"query":"那件东西存在哪里？","context":["不是松石47。"],"task_id":"松石47"}),
    ] {
        let recalled = service
            .call("memory_recall", request.clone(), || false)
            .unwrap();
        let contract = &recalled["answer_contract"];
        let mut validation = request.clone();
        validation["draft_answer"] = contract["safe_answer"].clone();
        validation["source_ids"] = contract["safe_source_ids"].clone();
        let answer = service
            .call("memory_validate_answer", validation, || false)
            .unwrap();
        contract_shape(&answer);
        for (field, value) in contract.as_object().unwrap() {
            assert_eq!(&answer[field], value, "{field}: {answer}");
        }
        assert_eq!(answer["task_id"], request["task_id"]);
        // Approval can authorize an exact clarification/refusal, without approving any factual answer.
        assert_eq!(answer["approved"], true);
        assert_eq!(answer["approved_source_ids"], json!([]));
    }
    let answer = service
        .call(
            "memory_validate_answer",
            json!({"query":"松石47 技术选择","draft_answer":"SQLite。"}),
            || false,
        )
        .unwrap();
    contract_shape(&answer);
    assert_eq!(answer["status"], "question_required");
    assert_eq!(answer["approved"], false);
    assert_eq!(answer["read_revision"], Value::Null);
    assert_eq!(answer["source_revision"], Value::Null);
}

#[test]
fn invalid_inputs_do_not_produce_answer_contracts_or_approval() {
    let (_db, mut service) = fixture();
    for request in [
        json!({"query":123}),
        json!({"query":""}),
        json!({"query":"他喜欢什么？","context":"松石47。"}),
        json!({"query":"他喜欢什么？","context":[" "]}),
        json!({"query":"他喜欢什么？","context":vec!["松石47。";9]}),
        json!({"query":"他喜欢什么？","context":["字".repeat(2001)]}),
        json!({"query":"他喜欢什么？","task_id":""}),
    ] {
        for tool in ["memory_recall", "memory_validate_answer"] {
            let mut input = request.clone();
            if tool == "memory_validate_answer" {
                input["draft_answer"] = json!("不知道。");
            }
            let result = service.call_with_global_context(tool, input, || false);
            assert_eq!(result.is_error, Some(true), "{tool}: {request}");
            let body = result.structured_content.unwrap();
            assert!(body.get("answer_contract").is_none(), "{body}");
            assert!(body.get("approved").is_none(), "{body}");
        }
    }
    // The per-message context bound counts characters, not UTF-8 bytes.
    let recalled = service
        .call(
            "memory_recall",
            json!({"query":"琥珀93的维护预算是多少？","context":["字".repeat(2000)]}),
            || false,
        )
        .unwrap();
    contract_shape(&recalled["answer_contract"]);
}

#[test]
fn prepare_reports_saved_source_separately_when_recall_input_exceeds_bounds() {
    let (db, _) = fixture();
    let mut service = ChatMemoryService::new(
        db.open(),
        ChatConfig {
            retrieval_mode: Some(RetrievalMode::Local),
            enrichment_mode: EnrichmentMode::Off,
            ..ChatConfig::default()
        },
    )
    .unwrap();
    let result = service
        .call(
            "memory_prepare",
            json!({"user_turn":{
                "session_id":"contract","turn_id":"long-history","capture_source":true,
                "text":"我今天继续整理书房。","history":vec!["我在整理书房。";9]
            }}),
            || false,
        )
        .unwrap();
    assert_eq!(result["remember"]["result"]["source"]["status"], "stored");
    assert_eq!(result["recall"]["status"], "failed");
    assert!(result["recall"].get("answer_contract").is_none());
}

#[test]
fn prepare_cache_invalidates_source_only_answers_on_either_revision_change() {
    use rmcp::model::CallToolResult;
    let (db, service) = fixture();
    let arguments = json!({"user_turn":{"session_id":"contract","turn_id":"cached",
        "text":"先搁置的那种内存缓存服务叫什么？"}});
    for source_only in [false, true] {
        let state = db.open().read().unwrap();
        // No complete Memory objects: the old recursive record check cannot see these sources.
        let cached = CallToolResult::structured(json!({
            "remember":{"status":"completed","result":{"outcomes":[]}},
            "recall":{"status":"completed","result":{
                "read_revision":state.revision,"source_revision":state.source_revision,
                "result":{"facts":[]},"answer_contract":{"safe_answer":"Redis。",
                    "sources":[{"source_id":"E1","quote":"Redis 暂时不引入。"}]}
            }}
        }));
        assert_eq!(
            service
                .validate_prepare_response(&arguments, cached.clone())
                .structured_content,
            cached.structured_content
        );
        db.open()
            .transaction(|state| {
                if source_only {
                    state.sources.get_mut("E1").unwrap().request = None;
                } else {
                    state.entities.upsert(Entity {
                        id: EntityId::from("new-subject"),
                        owner_id: UserId::from("alice"),
                        entity_type: EntityType::Person,
                        canonical_name: "林青".into(),
                        aliases: Vec::new(),
                    });
                }
                Ok(())
            })
            .unwrap();
        let current = db.open().read().unwrap();
        if source_only {
            assert_eq!(current.revision, state.revision);
            assert!(current.source_revision > state.source_revision);
        } else {
            assert!(current.revision > state.revision);
            assert_eq!(current.source_revision, state.source_revision);
        }
        let refreshed = service.validate_prepare_response(&arguments, cached);
        let body = refreshed.structured_content.as_ref().unwrap();
        assert_eq!(body["remember"]["status"], "completed");
        assert_eq!(body["recall"]["status"], "not_requested");
        assert_eq!(body["recall"]["reason"], "state_changed_after_prepare");
        assert!(!serde_json::to_string(&refreshed).unwrap().contains("Redis"));
    }
}

fn generic_fact_fixture(values: &[(&str, &str)]) -> (Db, ChatMemoryService) {
    let db = Db::new();
    db.open()
        .transaction(|state| {
            state.entities.upsert(Entity {
                id: EntityId::from("云杉会议"),
                owner_id: UserId::from("alice"),
                entity_type: EntityType::Other("event".into()),
                canonical_name: "云杉会议".into(),
                aliases: Vec::new(),
            });
            for (index, (id, value)) in values.iter().enumerate() {
                let text = format!("云杉会议的签到编号是{value}。");
                let mut record = source(id, &text, index as u64 + 1);
                record.request.as_mut().unwrap()["task_id"] = json!("task-a");
                state.sources.insert((*id).into(), record);
            }
            state.with_engine(InMemoryPredicateRegistry::default(), |engine, _| {
                for (id, value) in values {
                    let mut candidate = CandidateMemory::new(
                        UserId::from("alice"),
                        EntityId::from("云杉会议"),
                        MemoryType::Fact,
                        Predicate::from("fact"),
                        MemoryValue::Text(format!("云杉会议的签到编号是{value}。")),
                        Timestamp(100),
                        Evidence::new(
                            EvidenceId((*id).into()),
                            SessionId::from("session"),
                            TurnId::from(*id),
                            format!("云杉会议的签到编号是{value}。"),
                        ),
                    );
                    candidate.scope.conditions = vec![ScopeCondition::Task(TaskId::from("task-a"))];
                    engine.ingest(common::grounded(candidate))?;
                }
                Ok(())
            })
        })
        .unwrap();
    let service = ChatMemoryService::new(
        db.open(),
        ChatConfig {
            retrieval_mode: Some(RetrievalMode::Local),
            local_recall_budget: LocalRecallBudget {
                candidate_k: 1,
                source_limit: 1,
                ..LocalRecallBudget::default()
            },
            ..ChatConfig::default()
        },
    )
    .unwrap();
    (db, service)
}

#[test]
fn generic_fact_gate_checks_full_snapshot_revisions_drafts_and_task_scope() {
    let query = "云杉会议的签到编号是什么？";
    let (db, mut service) = generic_fact_fixture(&[("F1", "Q71")]);
    let recall = service
        .call(
            "memory_recall",
            json!({"query":query,"task_id":"task-a","limit":1}),
            || false,
        )
        .unwrap();
    assert_eq!(
        recall["answer_contract"]["status"], "source_linked_fact",
        "{recall}"
    );
    assert_eq!(recall["answer_contract"]["safe_answer"], "Q71。");
    assert_eq!(recall["answer_contract"]["safe_source_ids"], json!(["F1"]));
    assert_eq!(recall["model_calls"], json!([]));
    let approved = service
        .call(
            "memory_validate_answer",
            json!({"query":query,"task_id":"task-a","draft_answer":"Q71。","source_ids":["F1"]}),
            || false,
        )
        .unwrap();
    assert_eq!(approved["approved"], true, "{approved}");
    for (draft, ids) in [
        ("Q72。", vec!["F1"]),
        ("Q71。", vec!["wrong-source"]),
        ("Q71。会议预算是500元。", vec!["F1"]),
    ] {
        let rejected = service
            .call(
                "memory_validate_answer",
                json!({"query":query,"task_id":"task-a","draft_answer":draft,"source_ids":ids}),
                || false,
            )
            .unwrap();
        assert_eq!(rejected["approved"], false, "{rejected}");
        assert_eq!(rejected["approved_source_ids"], json!([]));
        assert_eq!(rejected["safe_answer"], "Q71。");
        assert_eq!(rejected["model_calls"], json!([]));
    }
    let different_task = service
        .call(
            "memory_validate_answer",
            json!({"query":query,"task_id":"task-b","draft_answer":"Q71。","source_ids":["F1"]}),
            || false,
        )
        .unwrap();
    assert_eq!(different_task["approved"], false, "{different_task}");
    assert_eq!(different_task["safe_source_ids"], json!([]));
    assert_eq!(different_task["sources"], json!([]));

    db.open()
        .transaction(|state| {
            let mut pending = source("F2", "我撤回云杉会议的签到编号是Q71的说法。", 2);
            pending.enrichment = "not_requested".into();
            pending.request.as_mut().unwrap()["task_id"] = json!("task-a");
            state.sources.insert("F2".into(), pending);
            Ok(())
        })
        .unwrap();
    let withdrawn = service
        .call(
            "memory_validate_answer",
            json!({"query":query,"task_id":"task-a","draft_answer":"Q71。","source_ids":["F1"]}),
            || false,
        )
        .unwrap();
    assert_eq!(withdrawn["approved"], false, "{withdrawn}");
    assert_eq!(withdrawn["safe_source_ids"], json!([]));
    assert_eq!(withdrawn["sources"], json!([]));

    let (conflict_db, mut conflict_service) = generic_fact_fixture(&[("F1", "Q71"), ("F2", "Q72")]);
    assert_eq!(
        conflict_db
            .open()
            .read()
            .unwrap()
            .repository
            .list_by_owner(&UserId::from("alice"))
            .unwrap()
            .iter()
            .filter(|memory| memory.status == MemoryStatus::Active)
            .count(),
        2
    );
    let limited = conflict_service
        .call(
            "memory_recall",
            json!({"query":query,"task_id":"task-a","limit":1}),
            || false,
        )
        .unwrap();
    assert_ne!(
        limited["answer_contract"]["status"], "source_linked_fact",
        "{limited}"
    );
    assert_eq!(limited["answer_contract"]["safe_source_ids"], json!([]));
    assert_eq!(limited["answer_contract"]["sources"], json!([]));
    for draft in ["Q71。", "Q72。"] {
        let answer = conflict_service
            .call(
                "memory_validate_answer",
                json!({"query":query,"task_id":"task-a","draft_answer":draft,"source_ids":["F1"]}),
                || false,
            )
            .unwrap();
        assert_eq!(answer["approved"], false, "{answer}");
        assert_eq!(answer["safe_source_ids"], json!([]));
    }

    // A model-extracted exact substring cannot discard the source's conditional framing.
    let (hypothetical_db, mut hypothetical_service) = generic_fact_fixture(&[("F1", "Q71")]);
    hypothetical_db
        .open()
        .transaction(|state| {
            state
                .sources
                .get_mut("F1")
                .unwrap()
                .request
                .as_mut()
                .unwrap()["text"] = json!("如果云杉会议的签到编号是Q71。那么还需要核实。");
            Ok(())
        })
        .unwrap();
    let hypothetical = hypothetical_service
        .call(
            "memory_validate_answer",
            json!({
                "query":query,"task_id":"task-a","draft_answer":"Q71。","source_ids":["F1"]
            }),
            || false,
        )
        .unwrap();
    assert_eq!(hypothetical["approved"], false, "{hypothetical}");
    assert_eq!(hypothetical["safe_source_ids"], json!([]));
}

#[test]
fn pending_update_state_blocks_a_unique_current_answer() {
    // W5 标为更新待定的记录不能被核准为唯一的当前结论；judged 或没有条目时照旧。
    let query = "云杉会议的签到编号是什么？";
    let pending = |reason| UpdateState {
        status: UpdateStatus::Pending,
        reasons: vec![reason],
    };
    for (label, update, expect_answer) in [
        ("none", None, true),
        (
            "judged",
            Some(UpdateState {
                status: UpdateStatus::Judged,
                reasons: vec![],
            }),
            true,
        ),
        (
            "scope",
            Some(pending(UpdateReason::UpdateScopeIncomplete)),
            false,
        ),
        (
            "target",
            Some(pending(UpdateReason::UpdateTargetUnknown)),
            false,
        ),
        (
            "relation",
            Some(pending(UpdateReason::UpdateRelationUnclear)),
            false,
        ),
    ] {
        let (db, mut service) = generic_fact_fixture(&[("F1", "Q71")]);
        if let Some(update) = update {
            db.open()
                .transaction(|state| {
                    let id = state.repository.list_by_owner(&UserId::from("alice"))?[0].id;
                    state.record_updates.insert(id, update);
                    Ok(())
                })
                .unwrap();
            service = ChatMemoryService::new(
                db.open(),
                ChatConfig {
                    retrieval_mode: Some(RetrievalMode::Local),
                    local_recall_budget: LocalRecallBudget {
                        candidate_k: 1,
                        source_limit: 1,
                        ..LocalRecallBudget::default()
                    },
                    ..ChatConfig::default()
                },
            )
            .unwrap();
        }
        let recall = service
            .call(
                "memory_recall",
                json!({"query":query,"task_id":"task-a","limit":1}),
                || false,
            )
            .unwrap();
        let validated = service
            .call(
                "memory_validate_answer",
                json!({"query":query,"task_id":"task-a","draft_answer":"Q71。","source_ids":["F1"]}),
                || false,
            )
            .unwrap();
        if expect_answer {
            assert_eq!(
                recall["answer_contract"]["status"], "source_linked_fact",
                "{label}"
            );
            assert_eq!(validated["approved"], true, "{label}");
        } else {
            assert_eq!(
                recall["answer_contract"]["status"], "answer_not_verified",
                "{label}: {recall}"
            );
            assert_eq!(
                recall["answer_contract"]["safe_source_ids"],
                json!([]),
                "{label}"
            );
            assert_eq!(validated["approved"], false, "{label}: {validated}");
            assert_eq!(validated["approved_source_ids"], json!([]), "{label}");
        }
        assert_eq!(recall["model_calls"], json!([]), "{label}");
    }
}

/// 本机假模型：对补答请求（personal_memory_answer_v1）返回 `answer`，其他请求一律 500。
fn fake_answer_model(answer: Value) -> String {
    use std::io::{BufRead, BufReader, Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut length = 0;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0; length];
            let _ = reader.read_exact(&mut body);
            let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            let response = if request["text"]["format"]["name"] == "personal_memory_answer_v1" {
                json!({"id":"fake","status":"completed","model":"fake",
                    "usage":{"input_tokens":10,"output_tokens":10,"total_tokens":20},
                    "output":[{"type":"message","content":[
                        {"type":"output_text","text":answer.to_string()}]}]})
                .to_string()
            } else {
                String::new()
            };
            let status = if response.is_empty() {
                "500 Internal Server Error"
            } else {
                "200 OK"
            };
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                response.len()
            );
        }
    });
    format!("http://{address}/v1")
}

/// 主体以别名“YS会”提问；问题不与原文字面重合，原文不进入本地证据包，只能由补答候选取得。
/// 同一原文 F1 可拆出多条独立断言（`quotes`），各自成为一条记录，按顺序编号。
fn assisted_fixture(text: &str, quotes: &[&str]) -> Db {
    let db = Db::new();
    db.open()
        .transaction(|state| {
            state.entities.upsert(Entity {
                id: EntityId::from("云杉会议"),
                owner_id: UserId::from("alice"),
                entity_type: EntityType::Other("event".into()),
                canonical_name: "云杉会议".into(),
                aliases: vec!["YS会".into()],
            });
            state.sources.insert("F1".into(), source("F1", text, 1));
            state.with_engine(InMemoryPredicateRegistry::default(), |engine, _| {
                for (index, quote) in quotes.iter().enumerate() {
                    engine.ingest(common::grounded(CandidateMemory::new(
                        UserId::from("alice"),
                        EntityId::from("云杉会议"),
                        MemoryType::Fact,
                        Predicate::from("fact"),
                        MemoryValue::Text((*quote).into()),
                        Timestamp(100),
                        Evidence::new(
                            EvidenceId(format!("F1-{index}")),
                            SessionId::from("session"),
                            TurnId::from("F1"),
                            *quote,
                        ),
                    )))?;
                }
                Ok(())
            })
        })
        .unwrap();
    db
}

fn assisted_service(db: &Db, base_url: String) -> ChatMemoryService {
    ChatMemoryService::new(
        db.open(),
        ChatConfig {
            retrieval_mode: Some(RetrievalMode::Assisted),
            model_budget: Some(ModelBudgetConfig {
                group_id: format!("s5-pending-{}", diagnostics::new_id()),
                max_calls: 100,
                max_operation_calls: 4,
                allow_coverage_repair: false,
                allow_extraction_repair: false,
                max_input_bytes: 65_536,
                extraction_output_tokens: 1_024,
                review_output_tokens: 1_024,
                query_output_tokens: 1_024,
            }),
            model_base_url: Some(base_url),
            ..ChatConfig::default()
        },
    )
    .unwrap()
}

/// 把记录（按 `assisted_fixture` 的顺序）标为更新待定。
fn mark_pending(db: &Db, index: usize) {
    db.open()
        .transaction(|state| {
            let id = state.repository.list_by_owner(&UserId::from("alice"))?[index].id;
            state.record_updates.insert(
                id,
                UpdateState {
                    status: UpdateStatus::Pending,
                    reasons: vec![UpdateReason::UpdateScopeIncomplete],
                },
            );
            Ok(())
        })
        .unwrap();
}

struct AssistedCase {
    label: &'static str,
    text: &'static str,
    quotes: &'static [&'static str],
    pending: Option<usize>,
    query: &'static str,
    proposal: Value,
    /// 核准时的状态；None 表示应被拦下。
    approved_status: Option<&'static str>,
    /// 空串表示按模型提出的数值与日期组成计算答案的文本。
    draft: &'static str,
}

/// `enrichment` 为原文任务状态：committed 时补答候选带记录 ID；running / queued 时同一原文
/// 以未整理原文的形式提供（W5 组已提交、任务尚未收尾的窗口），仍须绑定到已提交的记录。
fn run_assisted_case(case: &AssistedCase, enrichment: &str) {
    let label = format!("{} [{enrichment}]", case.label);
    let db = assisted_fixture(case.text, case.quotes);
    if let Some(index) = case.pending {
        mark_pending(&db, index);
    }
    db.open()
        .transaction(|state| {
            state.sources.get_mut("F1").unwrap().enrichment = enrichment.into();
            Ok(())
        })
        .unwrap();
    let mut service = assisted_service(&db, fake_answer_model(case.proposal.clone()));
    let recall = service
        .call(
            "memory_recall",
            json!({"query":case.query,"limit":1}),
            || false,
        )
        .unwrap();
    let contract = &recall["answer_contract"];
    // 原文不在本地证据包里：拦截只能来自补答候选携带的状态。
    assert_eq!(recall["source_evidence"], json!([]), "{label}: {recall}");
    assert_eq!(
        recall["model_answer"]["source_id"], "F1",
        "{label}: {recall}"
    );
    // 待定时用核准时同样的文本试图通过验证。
    let draft = if case.draft.is_empty() {
        let proposed = &recall["model_answer"];
        format!(
            "{}（{}）。",
            proposed["value"].as_str().unwrap(),
            proposed["event_date"].as_str().unwrap()
        )
    } else {
        case.draft.to_owned()
    };
    let validated = service
        .call(
            "memory_validate_answer",
            json!({"query":case.query,"draft_answer":draft,"source_ids":["F1"]}),
            || false,
        )
        .unwrap();
    match case.approved_status {
        Some(status) => {
            assert_eq!(contract["status"], status, "{label}: {recall}");
            assert!(recall["model_answer"].get("update").is_none(), "{label}");
            assert_eq!(validated["approved"], true, "{label}: {validated}");
        }
        None => {
            assert_eq!(
                contract["status"], "answer_not_verified",
                "{label}: {recall}"
            );
            assert_eq!(contract["safe_source_ids"], json!([]), "{label}");
            assert_eq!(
                recall["model_answer"]["update"]["status"], "pending",
                "{label}"
            );
            assert_eq!(validated["approved"], false, "{label}: {validated}");
        }
    }
}

/// 模型客户端从进程环境读取密钥。测试线程里改环境不安全（`set_var` 要求无其他线程并发读写
/// 环境），所以补答用例在子进程里跑：父测试用 `Command::env` 注入密钥，再以 `--exact
/// --ignored` 只运行下面这个子测试。
const ASSISTED_CHILD: &str = "PMR_S5_ASSISTED_CHILD";

#[test]
fn pending_update_state_blocks_model_assisted_answers() {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "assisted_answer_cases_in_child_process",
            "--exact",
            "--ignored",
            "--test-threads=1",
        ])
        .env(ASSISTED_CHILD, "1")
        .env("DEEPSEEK_API_KEY", "local-fixture-key")
        .env("PMR_MODEL_NETWORK", "offline")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(
        stdout.contains("1 passed"),
        "the child must actually run the cases: {stdout}"
    );
}

#[test]
#[ignore = "run by pending_update_state_blocks_model_assisted_answers in a child process"]
fn assisted_answer_cases_in_child_process() {
    // 直接用 --ignored 运行时没有父进程注入的环境：明确失败，而不是悄悄跳过。
    assert_eq!(
        std::env::var(ASSISTED_CHILD).as_deref(),
        Ok("1"),
        "run through pending_update_state_blocks_model_assisted_answers"
    );
    let answer = json!({"verdict":"answer","candidate_index":0,"answer_text":"Q71",
        "event_date":null,"unit":null});
    let elapsed = json!({"verdict":"elapsed","candidate_index":0,"answer_text":"昨天",
        "event_date":"1969-12-31","unit":"day"});
    let q71: &[&str] = &["云杉会议的签到编号是Q71。"];
    let yesterday: &[&str] = &["我昨天参加了云杉会议。"];
    // 同一原文拆出的两条独立断言：编号（第 0 条，补答候选引用它）与入口（第 1 条）。
    let both_text = "云杉会议的签到编号是Q71，入口在东门。";
    let both: &[&str] = &["云杉会议的签到编号是Q71", "入口在东门"];
    for case in [
        AssistedCase {
            label: "model answer, judged or unknown",
            text: q71[0],
            quotes: q71,
            pending: None,
            query: "YS会的号码？",
            proposal: answer.clone(),
            approved_status: Some("source_linked_model_answer"),
            draft: "Q71。",
        },
        AssistedCase {
            label: "model answer, pending",
            text: q71[0],
            quotes: q71,
            pending: Some(0),
            query: "YS会的号码？",
            proposal: answer.clone(),
            approved_status: None,
            draft: "Q71。",
        },
        AssistedCase {
            label: "computed elapsed, judged or unknown",
            text: yesterday[0],
            quotes: yesterday,
            pending: None,
            query: "YS会过去多久了？",
            proposal: elapsed.clone(),
            approved_status: Some("source_linked_computed_elapsed"),
            draft: "",
        },
        AssistedCase {
            label: "computed elapsed, pending",
            text: yesterday[0],
            quotes: yesterday,
            pending: Some(0),
            query: "YS会过去多久了？",
            proposal: elapsed,
            approved_status: None,
            draft: "",
        },
        // 同源的另一条独立断言待定，不影响被引用记录的答案。
        AssistedCase {
            label: "same source, other assertion pending",
            text: both_text,
            quotes: both,
            pending: Some(1),
            query: "YS会的号码？",
            proposal: answer.clone(),
            approved_status: Some("source_linked_model_answer"),
            draft: "Q71。",
        },
        AssistedCase {
            label: "same source, quoted assertion pending",
            text: both_text,
            quotes: both,
            pending: Some(0),
            query: "YS会的号码？",
            proposal: answer,
            approved_status: None,
            draft: "Q71。",
        },
    ] {
        for enrichment in ["committed", "running", "queued"] {
            run_assisted_case(&case, enrichment);
        }
    }
}
