mod common;

use hippocampus::*;
use serde_json::{Value, json};
use std::collections::BTreeSet;

struct Db(std::path::PathBuf);

impl Db {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("s5-assisted-{}.sqlite3", diagnostics::new_id())))
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
        "enrichment":"not_requested","error":null,"result":null,"base_revision":0
    }))
    .unwrap()
}

fn context() -> RuntimeContext {
    RuntimeContext::current(UserId::from("alice"), Timestamp(200))
}

fn single_registry() -> InMemoryPredicateRegistry {
    let mut predicates = InMemoryPredicateRegistry::default();
    predicates.register(Predicate::from("uses"), Cardinality::Single);
    predicates
}

fn local(
    store: &SqliteMemoryStore,
    query: &str,
    budget: &LocalRecallBudget,
) -> (Value, LocalSnapshotSignature) {
    store
        .recall_local_with_signature(
            query,
            &[],
            &context(),
            InMemoryPredicateRegistry::default(),
            &InMemorySemanticRelationRegistry::default(),
            budget,
        )
        .unwrap()
}

fn assisted(
    store: &SqliteMemoryStore,
    query: &str,
    budget: &LocalRecallBudget,
    queries: &[String],
    signature: &LocalSnapshotSignature,
) -> (Value, bool) {
    store
        .recall_assisted_local(
            query,
            &[],
            &context(),
            InMemoryPredicateRegistry::default(),
            &InMemorySemanticRelationRegistry::default(),
            budget,
            queries,
            signature,
        )
        .unwrap()
}

fn candidate_source_ids(answer: &Value) -> BTreeSet<String> {
    answer["source_decisions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|decision| decision["segment_id"].as_str())
        .filter_map(|id| id.split_once(':').map(|(source, _)| source.to_owned()))
        .collect()
}

#[test]
fn proposed_single_relation_requires_one_user_bound_subject_and_linked_assertion() {
    let db = Db::new();
    let mut store = db.open();
    let rows = [
        (
            "E1",
            "松石47",
            "项目松石47选用 SQLite，Redis 暂时不引入。",
            "Redis",
        ),
        (
            "D1",
            "赤铜82",
            "项目赤铜82选用 PostgreSQL，Memcached 暂时不引入。",
            "Memcached",
        ),
    ];
    store
        .transaction(|state| {
            for (_, subject, _, _) in rows {
                state.entities.upsert(Entity {
                    id: EntityId::from(subject),
                    owner_id: UserId::from("alice"),
                    entity_type: EntityType::Other("project".into()),
                    canonical_name: subject.into(),
                    aliases: Vec::new(),
                });
            }
            for (sequence, (id, _, text, _)) in rows.iter().enumerate() {
                let mut record = source(id, text, sequence as u64 + 1);
                record.enrichment = "committed".into();
                state.sources.insert(format!("record-{id}"), record);
            }
            state.with_engine(InMemoryPredicateRegistry::default(), |engine, _| {
                for (id, subject, quote, value) in rows {
                    engine.ingest(common::grounded(CandidateMemory::new(
                        UserId::from("alice"),
                        EntityId::from(subject),
                        MemoryType::Fact,
                        Predicate::from("暂缓技术"),
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
    let memories = store
        .read()
        .unwrap()
        .repository
        .list_by_owner(store.owner())
        .unwrap();
    let memory_id = memories
        .iter()
        .find(|memory| memory.subject_id == EntityId::from("松石47"))
        .unwrap()
        .id;
    let budget = LocalRecallBudget::default();
    let context = vec!["松石47。".into()];
    let recall = |store: &SqliteMemoryStore,
                  query: &str,
                  context: &[String],
                  hint: &SingleRelationEvidenceHint| {
        let (_, signature) = store
            .recall_local_with_signature(
                query,
                context,
                &self::context(),
                InMemoryPredicateRegistry::default(),
                &InMemorySemanticRelationRegistry::default(),
                &budget,
            )
            .unwrap();
        store
            .recall_assisted_single_relation(
                query,
                context,
                &self::context(),
                InMemoryPredicateRegistry::default(),
                &InMemorySemanticRelationRegistry::default(),
                &budget,
                hint,
                &signature,
            )
            .unwrap()
    };
    for (query, query_relation) in [
        ("先搁置的那种内存缓存服务叫什么？", "先搁置"),
        ("先搁置的那种内存缓存服务叫什么？", "先搁置的"),
        ("先搁置的缓存服务是什么？", "先搁置"),
        ("之前暂缓引入的是哪种缓存服务？", "之前暂缓引入"),
    ] {
        let hint = SingleRelationEvidenceHint {
            query_relation: query_relation.into(),
            source_id: "E1".into(),
            memory_id,
            evidence_relation: "暂时不引入".into(),
        };
        let (answer, changed) = recall(&store, query, &context, &hint);
        assert!(!changed);
        assert_eq!(
            answer["reference_resolution"]["needs_clarification"], false,
            "{query}: {answer}"
        );
        assert_eq!(
            answer["source_evidence"][0]["source_id"], "E1",
            "{query}: {answer}"
        );
        assert_eq!(answer["source_evidence"].as_array().unwrap().len(), 1);
        assert_eq!(
            answer["source_evidence"][0]["semantic_status"],
            "model_proposed_relation_checked_against_committed_fact"
        );
    }
    let query = "先搁置的那种内存缓存服务叫什么？";
    let hint = SingleRelationEvidenceHint {
        query_relation: "先搁置".into(),
        source_id: "E1".into(),
        memory_id,
        evidence_relation: "暂时不引入".into(),
    };
    for (question, relation) in [
        (query, "的"),
        ("先搁置再考虑的那种内存缓存服务叫什么？", "先搁置的"),
    ] {
        let unsafe_hint = SingleRelationEvidenceHint {
            query_relation: relation.into(),
            ..hint.clone()
        };
        assert_eq!(
            recall(&store, question, &context, &unsafe_hint).0["source_evidence"],
            json!([]),
            "{question}: {relation}"
        );
    }
    for bad_context in [
        vec!["赤铜82。".into()],
        vec!["松石47或赤铜82。".into()],
        vec!["不是松石47。".into()],
    ] {
        let (blocked, _) = recall(&store, query, &bad_context, &hint);
        assert_eq!(
            blocked["source_evidence"],
            json!([]),
            "{bad_context:?}: {blocked}"
        );
    }
    for wrong_answer_role in [
        "先搁置的原因是什么？",
        "先搁置的时间是什么？",
        "先搁置的预算是什么？",
        "先搁置的状态是什么？",
        "先搁置的数据库是什么？",
        "先搁置的不是 Redis 的那种缓存服务叫什么？",
        "先搁置的缓存服务和数据库分别是什么？",
        "没搁置的缓存服务叫什么？",
        "未搁置的缓存服务叫什么？",
        "不再搁置的缓存服务叫什么？",
        "先搁置但后来没搁置的缓存服务叫什么？",
        "先搁置但是没搁置的缓存服务叫什么？",
        "先搁置后来取消搁置的缓存服务叫什么？",
    ] {
        let negative_relation_hint = SingleRelationEvidenceHint {
            query_relation: "搁置".into(),
            ..hint.clone()
        };
        let proposed = if wrong_answer_role.starts_with(['没', '未', '不']) {
            &negative_relation_hint
        } else {
            &hint
        };
        let (blocked, _) = recall(&store, wrong_answer_role, &context, proposed);
        if wrong_answer_role.starts_with(['没', '未', '不']) {
            assert_ne!(
                blocked["reference_resolution"]["semantic_resolution"],
                "model_proposed_relation_not_independently_proven",
                "{wrong_answer_role}: {blocked}"
            );
        }
        assert_eq!(
            blocked["source_evidence"],
            json!([]),
            "{wrong_answer_role}: {blocked}"
        );
    }
    store
        .transaction(|state| {
            let mixed = "项目松石47选用 SQLite，赤铜82采用 PostgreSQL，Redis 暂时不引入。";
            let mut record = source("M1", mixed, 3);
            record.enrichment = "committed".into();
            state.sources.insert("M1".into(), record);
            state.with_engine(InMemoryPredicateRegistry::default(), |engine, _| {
                engine.ingest(common::grounded(CandidateMemory::new(
                    UserId::from("alice"),
                    EntityId::from("松石47"),
                    MemoryType::Fact,
                    Predicate::from("暂缓技术"),
                    MemoryValue::Text("Redis".into()),
                    Timestamp(100),
                    Evidence::new(
                        EvidenceId("M1".into()),
                        SessionId::from("session"),
                        TurnId::from("M1"),
                        mixed,
                    ),
                )))?;
                Ok(())
            })
        })
        .unwrap();
    let wrong_memory = store
        .read()
        .unwrap()
        .repository
        .list_by_owner(store.owner())
        .unwrap()
        .into_iter()
        .find(|memory| memory.evidence.iter().any(|e| e.turn_id.0 == "M1"))
        .unwrap()
        .id;
    let wrong_hint = SingleRelationEvidenceHint {
        source_id: "M1".into(),
        memory_id: wrong_memory,
        ..hint.clone()
    };
    assert_eq!(
        recall(&store, query, &context, &wrong_hint).0["source_evidence"],
        json!([])
    );
    for (source_id, quote, relation) in [
        (
            "M2",
            "项目松石47选用 SQLite。Redis 暂时不引入。",
            "暂时不引入",
        ),
        ("M3", "松石47不再暂缓引入 Redis。", "暂缓引入"),
        (
            "M4",
            "松石47选用 SQLite，赤铜83的 Redis 暂时不引入。",
            "暂时不引入",
        ),
        (
            "M5",
            "松石47选用 SQLite，数据库 Redis 暂时不引入。",
            "暂时不引入",
        ),
        ("M6", "松石47未暂缓引入 Redis。", "暂缓引入"),
        ("M7", "松石47不暂缓引入 Redis。", "暂缓引入"),
        ("M8", "曾经松石47暂时不引入 Redis。", "暂时不引入"),
    ] {
        store
            .transaction(|state| {
                let mut record = source(source_id, quote, 4);
                record.enrichment = "committed".into();
                state.sources.insert(format!("record-{source_id}"), record);
                state.with_engine(InMemoryPredicateRegistry::default(), |engine, _| {
                    engine.ingest(common::grounded(CandidateMemory::new(
                        UserId::from("alice"),
                        EntityId::from("松石47"),
                        MemoryType::Fact,
                        Predicate::from("暂缓技术"),
                        MemoryValue::Text("Redis".into()),
                        Timestamp(100),
                        Evidence::new(
                            EvidenceId(source_id.into()),
                            SessionId::from("session"),
                            TurnId::from(source_id),
                            quote,
                        ),
                    )))?;
                    Ok(())
                })
            })
            .unwrap();
        let memory_id = store
            .read()
            .unwrap()
            .repository
            .list_by_owner(store.owner())
            .unwrap()
            .into_iter()
            .find(|memory| memory.evidence.iter().any(|e| e.turn_id.0 == source_id))
            .unwrap()
            .id;
        let unsafe_hint = SingleRelationEvidenceHint {
            source_id: source_id.into(),
            memory_id,
            evidence_relation: relation.into(),
            ..hint.clone()
        };
        let (blocked, _) = recall(&store, query, &context, &unsafe_hint);
        assert_eq!(blocked["source_evidence"], json!([]), "{source_id}");
        assert_ne!(
            blocked["reference_resolution"]["semantic_resolution"],
            "model_proposed_relation_not_independently_proven",
            "{source_id}: {blocked}"
        );
    }
    store
        .transaction(|state| {
            let second = "松石47暂不引入 Memcached。";
            let mut record = source("E2", second, 4);
            record.enrichment = "committed".into();
            state.sources.insert("E2".into(), record);
            state.with_engine(InMemoryPredicateRegistry::default(), |engine, _| {
                engine.ingest(common::grounded(CandidateMemory::new(
                    UserId::from("alice"),
                    EntityId::from("松石47"),
                    MemoryType::Fact,
                    Predicate::from("暂缓技术"),
                    MemoryValue::Text("Memcached".into()),
                    Timestamp(100),
                    Evidence::new(
                        EvidenceId("E2".into()),
                        SessionId::from("session"),
                        TurnId::from("E2"),
                        second,
                    ),
                )))?;
                Ok(())
            })
        })
        .unwrap();
    assert_eq!(
        recall(&store, query, &context, &hint).0["source_evidence"],
        json!([])
    );
}

#[test]
fn proposed_single_relation_binds_a_device_without_project_assumptions() {
    let db = Db::new();
    let mut store = db.open();
    let quote = "厨房净水器暂不引入活性炭滤芯。";
    store
        .transaction(|state| {
            state.entities.upsert(Entity {
                id: EntityId::from("厨房净水器"),
                owner_id: UserId::from("alice"),
                entity_type: EntityType::Other("device".into()),
                canonical_name: "厨房净水器".into(),
                aliases: Vec::new(),
            });
            let mut record = source("F1", quote, 1);
            record.enrichment = "committed".into();
            state.sources.insert("record-F1".into(), record);
            state.with_engine(InMemoryPredicateRegistry::default(), |engine, _| {
                engine.ingest(common::grounded(CandidateMemory::new(
                    UserId::from("alice"),
                    EntityId::from("厨房净水器"),
                    MemoryType::Fact,
                    Predicate::from("暂缓维护"),
                    MemoryValue::Text("活性炭滤芯".into()),
                    Timestamp(100),
                    Evidence::new(
                        EvidenceId("F1".into()),
                        SessionId::from("session"),
                        TurnId::from("F1"),
                        quote,
                    ),
                )))?;
                Ok(())
            })
        })
        .unwrap();
    let memory_id = store
        .read()
        .unwrap()
        .repository
        .list_by_owner(store.owner())
        .unwrap()[0]
        .id;
    let query = "先搁置的那种设备组件是什么？";
    let context = vec!["厨房净水器。".into()];
    let budget = LocalRecallBudget::default();
    let (_, signature) = store
        .recall_local_with_signature(
            query,
            &context,
            &self::context(),
            InMemoryPredicateRegistry::default(),
            &InMemorySemanticRelationRegistry::default(),
            &budget,
        )
        .unwrap();
    let (answer, changed) = store
        .recall_assisted_single_relation(
            query,
            &context,
            &self::context(),
            InMemoryPredicateRegistry::default(),
            &InMemorySemanticRelationRegistry::default(),
            &budget,
            &SingleRelationEvidenceHint {
                query_relation: "先搁置".into(),
                source_id: "F1".into(),
                memory_id,
                evidence_relation: "暂不引入".into(),
            },
            &signature,
        )
        .unwrap();
    assert!(!changed);
    assert_eq!(answer["source_evidence"][0]["source_id"], "F1", "{answer}");
}

#[test]
fn committed_choice_answers_natural_single_relation_questions_only() {
    let db = Db::new();
    let mut store = db.open();
    let rows = [
        (
            "E1",
            "松石47",
            "项目松石47选用 SQLite，Redis 暂时不引入。",
            "SQLite",
        ),
        ("D1", "赤铜82", "项目赤铜82选用 PostgreSQL。", "PostgreSQL"),
        ("N1", "蓝石22", "蓝石22未选用 MariaDB。", "MariaDB"),
    ];
    store
        .transaction(|state| {
            for (_, subject, _, _) in rows {
                state.entities.upsert(Entity {
                    id: EntityId::from(subject),
                    owner_id: UserId::from("alice"),
                    entity_type: EntityType::Other("project".into()),
                    canonical_name: subject.into(),
                    aliases: Vec::new(),
                });
            }
            for (sequence, (id, _, text, _)) in rows.iter().enumerate() {
                let mut record = source(id, text, sequence as u64 + 1);
                record.enrichment = "committed".into();
                state.sources.insert((*id).into(), record);
            }
            state.with_engine(InMemoryPredicateRegistry::default(), |engine, _| {
                for (id, subject, text, value) in rows {
                    engine.ingest(common::grounded(CandidateMemory::new(
                        UserId::from("alice"),
                        EntityId::from(subject),
                        MemoryType::Fact,
                        Predicate::from("技术选择"),
                        MemoryValue::Text(value.into()),
                        Timestamp(100),
                        Evidence::new(
                            EvidenceId(id.into()),
                            SessionId::from("session"),
                            TurnId::from(id),
                            text,
                        ),
                    )))?;
                }
                Ok(())
            })
        })
        .unwrap();
    let budget = LocalRecallBudget::default();
    let cases = [
        ("松石47选用什么数据库？", vec![], "E1"),
        ("松石47的数据库选择是什么？", vec![], "E1"),
        ("松石47的技术选择是什么？", vec![], "E1"),
        (
            "它选用什么数据库？",
            vec!["我说的是项目松石47。".to_owned()],
            "E1",
        ),
        ("数据库选用什么？", vec!["松石47。".to_owned()], "E1"),
        ("数据库选用什么？", vec!["赤铜82。".to_owned()], "D1"),
        ("赤铜82选用什么数据库？", vec![], "D1"),
    ];
    for (query, context, expected) in cases {
        let answer = store
            .recall_local(
                query,
                &context,
                &self::context(),
                InMemoryPredicateRegistry::default(),
                &InMemorySemanticRelationRegistry::default(),
                &budget,
            )
            .unwrap();
        assert_eq!(
            answer["source_evidence"][0]["source_id"], expected,
            "{query}: {answer}"
        );
        assert_eq!(
            answer["source_evidence"].as_array().unwrap().len(),
            1,
            "{query}: {answer}"
        );
    }
    for query in [
        "数据库选用什么？",
        "蓝石22选用什么数据库？",
        "松石47不选用什么数据库？",
        "松石47使用什么数据库？",
        "松石47选用什么方案？",
        "松石47选用什么框架？",
        "松石47的部署限制是什么？",
        "松石47的技术选择和部署限制是否一致？",
        "琥珀93的维护预算是多少？",
    ] {
        let (answer, _) = local(&store, query, &budget);
        assert_eq!(answer["source_evidence"], json!([]), "{query}: {answer}");
    }
    let ambiguous = store
        .recall_local(
            "数据库选用什么？",
            &["松石47。".into(), "赤铜82。".into()],
            &self::context(),
            InMemoryPredicateRegistry::default(),
            &InMemorySemanticRelationRegistry::default(),
            &budget,
        )
        .unwrap();
    assert_eq!(ambiguous["source_evidence"], json!([]));
    assert_eq!(
        ambiguous["reference_resolution"]["needs_clarification"],
        true
    );
}

#[test]
fn soft_query_appends_after_original_under_the_same_budget() {
    let db = Db::new();
    let mut store = db.open();
    store
        .transaction(|state| {
            state.sources.insert(
                "original".into(),
                source("original", "cobalt database uses PostgreSQL.", 1),
            );
            state.sources.insert(
                "supplement".into(),
                source("supplement", "mint tea is preferred.", 2),
            );
            Ok(())
        })
        .unwrap();
    let budget = LocalRecallBudget {
        source_limit: 2,
        ..Default::default()
    };
    let (original, signature) = local(&store, "cobalt", &budget);
    assert_eq!(original["source_evidence"].as_array().unwrap().len(), 1);
    let (answer, changed) = assisted(&store, "cobalt", &budget, &["mint".into()], &signature);
    assert!(!changed);
    assert_eq!(answer["source_evidence"].as_array().unwrap().len(), 2);
    assert_eq!(answer["source_evidence"][0], original["source_evidence"][0]);
    assert_eq!(answer["source_evidence"][1]["source_id"], "supplement");
    assert_eq!(
        answer["source_evidence"][1]["channels"],
        json!(["llm_inferred"])
    );
    assert!(answer["budget"]["content_bytes"].as_u64().unwrap() <= budget.content_bytes as u64);

    let tight = LocalRecallBudget {
        source_limit: 1,
        ..budget
    };
    let (original, signature) = local(&store, "cobalt", &tight);
    let (answer, changed) = assisted(&store, "cobalt", &tight, &["mint".into()], &signature);
    assert!(!changed);
    assert_eq!(answer["source_evidence"], original["source_evidence"]);
    assert!(
        answer["source_decisions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| {
                d["reason"] == "source_limit"
                    && d["segment_id"].as_str().unwrap().starts_with("supplement:")
            })
    );

    let exact = LocalRecallBudget {
        content_bytes: original["budget"]["content_bytes"].as_u64().unwrap() as usize,
        source_limit: 2,
        ..Default::default()
    };
    let (original, signature) = local(&store, "cobalt", &exact);
    let (answer, changed) = assisted(&store, "cobalt", &exact, &["mint".into()], &signature);
    assert!(!changed);
    assert_eq!(answer["source_evidence"], original["source_evidence"]);
    assert!(
        answer["budget"]["excluded"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| {
                d["reason"] == "content_budget"
                    && d["reference"]["segment_id"]
                        .as_str()
                        .is_some_and(|id| id.starts_with("supplement:"))
            })
    );
}

#[test]
fn changed_source_or_index_discards_soft_queries_and_uses_fresh_local() {
    let db = Db::new();
    let mut store = db.open();
    store
        .transaction(|state| {
            state.sources.insert(
                "original".into(),
                source("original", "cobalt database uses PostgreSQL.", 1),
            );
            state.sources.insert(
                "supplement".into(),
                source("supplement", "mint tea is preferred.", 2),
            );
            Ok(())
        })
        .unwrap();
    let budget = LocalRecallBudget::default();
    let (_, signature) = local(&store, "cobalt", &budget);
    store.rebuild_source_index().unwrap();
    let (answer, changed) = assisted(&store, "cobalt", &budget, &["mint".into()], &signature);
    assert!(changed, "index generation alone must invalidate the plan");
    assert_eq!(answer["source_evidence"].as_array().unwrap().len(), 1);

    let (_, signature) = local(&store, "cobalt", &budget);
    store
        .transaction(|state| {
            state.sources.get_mut("original").unwrap().request = None;
            Ok(())
        })
        .unwrap();
    let (answer, changed) = assisted(&store, "cobalt", &budget, &["mint".into()], &signature);
    assert!(changed);
    assert!(answer["source_evidence"].as_array().unwrap().is_empty());
    assert_eq!(
        answer["conditions_provenance"][0]["value"]["at"],
        json!(200)
    );
}

#[test]
fn invalid_soft_queries_leave_original_evidence_intact() {
    let db = Db::new();
    let mut store = db.open();
    store
        .transaction(|state| {
            state.sources.insert(
                "original".into(),
                source("original", "cobalt database uses PostgreSQL.", 1),
            );
            Ok(())
        })
        .unwrap();
    let budget = LocalRecallBudget::default();
    let (original, signature) = local(&store, "cobalt", &budget);
    let (answer, changed) = assisted(&store, "cobalt", &budget, &["x".repeat(201)], &signature);
    assert!(!changed);
    assert_eq!(
        answer["assistance_search"]["status"],
        "invalid_soft_queries"
    );
    assert_eq!(answer["source_evidence"], original["source_evidence"]);

    let mut moved_clock = context();
    moved_clock.at = Timestamp(201);
    assert!(
        store
            .recall_assisted_local(
                "cobalt",
                &[],
                &moved_clock,
                InMemoryPredicateRegistry::default(),
                &InMemorySemanticRelationRegistry::default(),
                &budget,
                &["mint".into()],
                &signature,
            )
            .is_err()
    );
}

#[test]
fn soft_relevant_conflicts_include_history_after_original_evidence() {
    let db = Db::new();
    let mut store = db.open();
    store
        .transaction(|state| {
            state.sources.insert(
                "original".into(),
                source("original", "cobalt database uses PostgreSQL.", 1),
            );
            state.with_engine(single_registry(), |engine, _| {
                let first = CandidateMemory::new(
                    UserId::from("alice"),
                    EntityId::from("self"),
                    MemoryType::Fact,
                    Predicate::from("uses"),
                    MemoryValue::Text("mint tea".into()),
                    Timestamp(100),
                    Evidence::new(
                        EvidenceId("one".into()),
                        SessionId::from("session"),
                        TurnId::from("one"),
                        "mint tea",
                    ),
                );
                let first_id = match engine.ingest(common::grounded(first))? {
                    IngestOutcome::Applied {
                        memory_id: Some(id),
                        ..
                    } => id,
                    other => panic!("{other:?}"),
                };
                let mut second = engine.repository.get(first_id)?.unwrap();
                second.id = engine.repository.allocate_id()?;
                second.value = MemoryValue::Text("mint coffee".into());
                engine.repository.insert(second)?;
                Ok(())
            })
        })
        .unwrap();
    let budget = LocalRecallBudget::default();
    let (original, signature) = store
        .recall_local_with_signature(
            "cobalt",
            &[],
            &context(),
            single_registry(),
            &InMemorySemanticRelationRegistry::default(),
            &budget,
        )
        .unwrap();
    assert!(
        original["historical_or_conflicting"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let (answer, changed) = store
        .recall_assisted_local(
            "cobalt",
            &[],
            &context(),
            single_registry(),
            &InMemorySemanticRelationRegistry::default(),
            &budget,
            &["mint".into()],
            &signature,
        )
        .unwrap();
    assert!(!changed);
    assert_eq!(answer["source_evidence"], original["source_evidence"]);
    assert_eq!(
        answer["historical_or_conflicting"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(answer["result"]["facts"], json!([]));
}

#[test]
fn soft_query_cannot_add_another_named_project() {
    let db = Db::new();
    let mut store = db.open();
    store
        .transaction(|state| {
            for name in ["松石47", "赤铜82"] {
                state.entities.upsert(Entity {
                    id: EntityId::from(name),
                    owner_id: UserId::from("alice"),
                    entity_type: EntityType::Other("Other".into()),
                    canonical_name: name.into(),
                    aliases: Vec::new(),
                });
            }
            for (index, (id, text)) in [
                ("E1", "项目松石47选用 SQLite，Redis 暂时不引入。"),
                (
                    "E2",
                    "松石47当前只部署在一台机器上；暂不引入 Redis，是为了减少维护成本。",
                ),
                ("D1", "项目赤铜82选用 PostgreSQL，部署在三台机器上。"),
            ]
            .into_iter()
            .enumerate()
            {
                state
                    .sources
                    .insert(id.into(), source(id, text, index as u64 + 1));
            }
            Ok(())
        })
        .unwrap();
    let budget = LocalRecallBudget {
        candidate_k: 30,
        source_limit: 10,
        content_bytes: 24_000,
    };
    let query = "松石47为什么先不加缓存？";
    let (original, signature) = local(&store, query, &budget);
    assert!(
        original["source_evidence"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["source_id"] != "D1")
    );
    let (answer, changed) = assisted(
        &store,
        query,
        &budget,
        &["松石47 单机部署 SQLite 缓解决策".into()],
        &signature,
    );
    assert!(!changed);
    assert!(
        answer["source_evidence"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["source_id"] != "D1")
    );
    assert_eq!(
        answer["assistance_search"]["filtered_other_named_entity"],
        1
    );
    let (full, changed) = assisted(
        &store,
        query,
        &budget,
        &[
            "松石47 不引入缓存 原因".into(),
            "松石47 Redis 维护成本".into(),
            "松石47 单机部署 SQLite 缓解决策".into(),
        ],
        &signature,
    );
    assert!(!changed);
    assert_eq!(full["source_evidence"], original["source_evidence"]);
    let (comparison, _) = local(&store, "松石47和赤铜82各用什么数据库？", &budget);
    assert_eq!(comparison["source_evidence"], json!([]));
    assert!(candidate_source_ids(&comparison).contains("D1"));
    assert_eq!(
        comparison["reference_resolution"]["needs_clarification"],
        false
    );
}

#[test]
fn soft_query_cannot_add_other_project_memory_or_unnamed_later_segment() {
    let db = Db::new();
    let mut store = db.open();
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
            state
                .sources
                .insert("A1".into(), source("A1", "松石47暂不引入缓存。", 1));
            state.sources.insert(
                "B1".into(),
                source(
                    "B1",
                    &format!("赤铜82。{}部署在三台机器上。", "无".repeat(800)),
                    2,
                ),
            );
            state.with_engine(InMemoryPredicateRegistry::default(), |engine, _| {
                engine.ingest(common::grounded(CandidateMemory::new(
                    UserId::from("alice"),
                    EntityId::from("赤铜82"),
                    MemoryType::Fact,
                    Predicate::from("deploys"),
                    MemoryValue::Text("三台机器".into()),
                    Timestamp(100),
                    Evidence::new(
                        EvidenceId("B-memory".into()),
                        SessionId::from("session"),
                        TurnId::from("B1"),
                        "赤铜82部署在三台机器上",
                    ),
                )))?;
                Ok(())
            })
        })
        .unwrap();
    let budget = LocalRecallBudget::default();
    let query = "松石47为什么先不加缓存？";
    let (original, signature) = local(&store, query, &budget);
    assert!(original["result"]["facts"].as_array().unwrap().is_empty());
    assert!(
        original["source_evidence"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v["source_id"] != "B1")
    );
    let (answer, changed) = assisted(
        &store,
        query,
        &budget,
        &["松石47 单机部署 SQLite 缓解决策".into()],
        &signature,
    );
    assert!(!changed);
    assert!(
        answer["source_evidence"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v["source_id"] != "B1")
    );
    assert!(answer["result"]["facts"].as_array().unwrap().is_empty());
    assert_eq!(
        answer["assistance_search"]["filtered_other_named_entity"],
        1
    );

    let comparison = "松石47和赤铜82怎么比较？";
    let (original, signature) = local(&store, comparison, &budget);
    assert!(
        original["source_evidence"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v["source_id"] != "B1" || v["byte_start"].as_u64() == Some(0))
    );
    let (answer, changed) = assisted(&store, comparison, &budget, &["部署".into()], &signature);
    assert!(!changed);
    assert!(
        answer["source_evidence"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| { v["source_id"] != "B1" || v["byte_start"].as_u64() == Some(0) })
    );
}

#[test]
fn soft_cache_query_discovers_both_possible_projects() {
    let budget = LocalRecallBudget::default();
    let query = "先搁置的那种内存缓存服务叫什么？";
    let source_ids = |answer: &Value| {
        answer["source_evidence"]
            .as_array()
            .unwrap()
            .iter()
            .map(|source| source["source_id"].as_str().unwrap().to_owned())
            .collect::<BTreeSet<_>>()
    };
    for include_second_project in [false, true] {
        let db = Db::new();
        let mut store = db.open();
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
                let mut sources = vec![
                    ("E1", "项目松石47选用 SQLite，Redis 暂时不引入。"),
                    (
                        "E2",
                        "松石47当前只部署在一台机器上；暂不引入 Redis，是为了减少维护成本。",
                    ),
                    ("D1", "项目赤铜82选用 PostgreSQL，部署在三台机器上。"),
                    ("D2", "周末计划去公园散步。"),
                ];
                if include_second_project {
                    sources.push(("E3", "赤铜82暂不引入 Memcached，也是为了减少维护成本。"));
                }
                for (index, (id, text)) in sources.into_iter().enumerate() {
                    state
                        .sources
                        .insert(id.into(), source(id, text, index as u64 + 1));
                }
                Ok(())
            })
            .unwrap();

        let (original, signature) = local(&store, query, &budget);
        assert!(source_ids(&original).is_empty());
        let (answer, changed) = assisted(&store, query, &budget, &["不引入".into()], &signature);
        assert!(!changed);
        let candidate_ids = candidate_source_ids(&answer);
        if include_second_project {
            assert_eq!(answer["assistance_search"]["candidate_count"], 3);
            assert_eq!(
                candidate_ids,
                BTreeSet::from(["E1".into(), "E2".into(), "E3".into()])
            );

            let named_query = "松石47先搁置的内存缓存服务叫什么？";
            let (_, signature) = local(&store, named_query, &budget);
            let (named, changed) =
                assisted(&store, named_query, &budget, &["不引入".into()], &signature);
            assert!(!changed);
            assert_ne!(named["evidence_status"], "ambiguous");
            assert!(source_ids(&named).is_empty());
            assert_eq!(
                candidate_source_ids(&named),
                BTreeSet::from(["E1".into(), "E2".into()])
            );
        } else {
            assert_eq!(answer["assistance_search"]["candidate_count"], 2);
            assert_eq!(candidate_ids, BTreeSet::from(["E1".into(), "E2".into()]));
            assert!(source_ids(&answer).is_empty());
            assert_eq!(answer["reference_resolution"]["needs_clarification"], true);

            for soft_queries in [Vec::new(), vec!["x".repeat(201)]] {
                let (fallback, changed) =
                    assisted(&store, query, &budget, &soft_queries, &signature);
                assert!(!changed);
                assert_eq!(fallback["source_evidence"], original["source_evidence"]);
                assert_eq!(
                    fallback["reference_resolution"],
                    original["reference_resolution"]
                );
                assert_eq!(fallback["evidence_status"], original["evidence_status"]);
            }

            let (distracted, changed) = assisted(
                &store,
                query,
                &budget,
                &["不引入".into(), "PostgreSQL".into()],
                &signature,
            );
            assert!(!changed);
            assert_ne!(distracted["reference_resolution"]["status"], "ambiguous");
        }
    }
}

#[test]
fn unbound_referents_prompt_without_treating_answer_variables_as_missing() {
    let db = Db::new();
    let mut store = db.open();
    store
        .transaction(|state| {
            state.entities.upsert(Entity {
                id: EntityId::from("松石47"),
                owner_id: UserId::from("alice"),
                entity_type: EntityType::Other("project".into()),
                canonical_name: "松石47".into(),
                aliases: Vec::new(),
            });
            for (index, (id, text)) in [
                ("E1", "项目松石47选用 SQLite，Redis 暂时不引入。"),
                (
                    "E2",
                    "松石47当前只部署在一台机器上；暂不引入 Redis，是为了减少维护成本。",
                ),
                ("E3", "赤铜82暂不引入 Memcached，也是为了减少维护成本。"),
                ("A1", "我取消了牙医预约，改到周二上午。"),
                ("A2", "我取消了理发预约，改到周五下午。"),
                ("P1", "我喜欢绿茶。"),
            ]
            .into_iter()
            .enumerate()
            {
                state
                    .sources
                    .insert(id.into(), source(id, text, index as u64 + 1));
            }
            Ok(())
        })
        .unwrap();
    let budget = LocalRecallBudget::default();
    let requires_clarification = |result: &Value| {
        assert_eq!(result["reference_resolution"]["needs_clarification"], true);
        assert!(
            result["reference_resolution"]["clarification_question"]
                .as_str()
                .is_some_and(|question| !question.trim().is_empty())
        );
        assert_eq!(result["source_evidence"], json!([]));
    };

    let query = "先搁置的那种内存缓存服务叫什么？";
    let (unbound, signature) = local(&store, query, &budget);
    requires_clarification(&unbound);
    let (supplemented, changed) = assisted(
        &store,
        query,
        &budget,
        &["不引入".into(), "Memcached".into()],
        &signature,
    );
    assert!(!changed);
    requires_clarification(&supplemented);

    let appointment = store
        .recall_local(
            "那次取消的预约改到什么时候？",
            &[],
            &context(),
            InMemoryPredicateRegistry::default(),
            &InMemorySemanticRelationRegistry::default(),
            &budget,
        )
        .unwrap();
    requires_clarification(&appointment);

    let bound = store
        .recall_local(
            query,
            &["继续聊松石47的内存缓存服务。".into()],
            &context(),
            InMemoryPredicateRegistry::default(),
            &InMemorySemanticRelationRegistry::default(),
            &budget,
        )
        .unwrap();
    assert_eq!(bound["reference_resolution"]["needs_clarification"], false);
    assert_eq!(bound["source_evidence"], json!([]));
    assert!(candidate_source_ids(&bound).contains("E1"));

    let (liking, _) = local(&store, "我之前说的我喜欢的是什么？", &budget);
    assert_eq!(liking["reference_resolution"]["needs_clarification"], false);
    assert_eq!(liking["source_evidence"], json!([]));
    assert!(candidate_source_ids(&liking).contains("P1"));
}

#[test]
fn clarification_binds_the_asked_role_not_any_entity_mention() {
    let db = Db::new();
    let mut store = db.open();
    store
        .transaction(|state| {
            state.entities.upsert(Entity {
                id: EntityId::from("松石47"),
                owner_id: UserId::from("alice"),
                entity_type: EntityType::Other("project".into()),
                canonical_name: "松石47".into(),
                aliases: Vec::new(),
            });
            for (index, (id, text)) in [
                ("A1", "我取消了牙医预约，改到周二上午。"),
                ("A2", "我取消了理发预约，改到周五下午。"),
                ("F1", "松石47的部署故障由张三处理。"),
                ("F2", "松石47的数据库故障由李四处理。"),
                ("E1", "项目松石47选用 SQLite，Redis 暂时不引入。"),
                ("E2", "赤铜82暂不引入 Memcached。"),
                ("P1", "王阿姨上个月离职。"),
            ]
            .into_iter()
            .enumerate()
            {
                state
                    .sources
                    .insert(id.into(), source(id, text, index as u64 + 1));
            }
            Ok(())
        })
        .unwrap();
    let budget = LocalRecallBudget::default();
    let recall = |query: &str, user_context: &[&str]| {
        store
            .recall_local(
                query,
                &user_context
                    .iter()
                    .map(|text| (*text).into())
                    .collect::<Vec<_>>(),
                &context(),
                InMemoryPredicateRegistry::default(),
                &InMemorySemanticRelationRegistry::default(),
                &budget,
            )
            .unwrap()
    };

    for (case, query, user_context) in [
        (
            "unrelated entity in USER context",
            "那次取消的预约改到什么时候？",
            &["继续聊松石47的技术选型。"] as &[&str],
        ),
        (
            "project named but event unbound",
            "松石47的那次故障是谁处理的？",
            &[],
        ),
        ("bare he", "他什么时候离职？", &[]),
        ("bare she", "她什么时候离职？", &[]),
        (
            "unbound event without demonstrative",
            "先搁置的内存缓存服务叫什么？",
            &[],
        ),
        (
            "second referent remains unbound",
            "那次牙医预约结束后，那位医生为什么离职？",
            &["继续说牙医预约。"],
        ),
        (
            "two appointments do not bind one referent",
            "那次预约改到什么时候？",
            &["继续说牙医预约和理发预约。"],
        ),
        (
            "two separate appointments do not bind one referent",
            "那次预约改到什么时候？",
            &["继续说牙医预约。理发预约。"],
        ),
    ] {
        let result = recall(query, user_context);
        assert_eq!(
            result["reference_resolution"]["needs_clarification"], true,
            "{case}: {query}"
        );
        assert!(
            result["reference_resolution"]["clarification_question"]
                .as_str()
                .is_some_and(|question| !question.trim().is_empty()),
            "{case}: missing clarification question"
        );
        assert_eq!(result["source_evidence"], json!([]), "{case}");
    }

    let bound = recall("那次预约改到什么时候？", &["继续说牙医预约。"]);
    assert_eq!(bound["reference_resolution"]["needs_clarification"], false);
    assert_eq!(bound["source_evidence"], json!([]));
    assert!(candidate_source_ids(&bound).contains("A1"));
    let car = recall("那辆车是什么型号？", &["继续说车牌沪A12345的那辆车。"]);
    assert_eq!(car["reference_resolution"]["needs_clarification"], false);
}

#[test]
fn source_linked_relation_returns_only_the_verified_quote() {
    const QUERY: &str = "松石47的维护预算是多少？";
    const QUOTE: &str = "松石47的维护预算是 500 元";
    const MIXED: &str = "松石47的维护预算是 500 元；赤铜82的维护成本是 300 元。";
    let budget = LocalRecallBudget::default();
    let db = Db::new();
    let mut store = db.open();
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
            let mut mixed = source("mixed", MIXED, 1);
            mixed.enrichment = "committed".into();
            state.sources.insert("mixed".into(), mixed);
            state.with_engine(InMemoryPredicateRegistry::default(), |engine, _| {
                for (subject, predicate, value, quote, evidence_id) in [
                    ("松石47", "maintenance_budget", "500 元", QUOTE, "budget"),
                    (
                        "赤铜82",
                        "maintenance_cost",
                        "300 元",
                        "赤铜82的维护成本是 300 元",
                        "cost",
                    ),
                ] {
                    engine.ingest(common::grounded(CandidateMemory::new(
                        UserId::from("alice"),
                        EntityId::from(subject),
                        MemoryType::Fact,
                        Predicate::from(predicate),
                        MemoryValue::Text(value.into()),
                        Timestamp(100),
                        Evidence::new(
                            EvidenceId(evidence_id.into()),
                            SessionId::from("session"),
                            TurnId::from("mixed"),
                            quote,
                        ),
                    )))?;
                }
                Ok(())
            })
        })
        .unwrap();
    let (local_result, signature) = local(&store, QUERY, &budget);
    assert_eq!(local_result["result"]["facts"].as_array().unwrap().len(), 1);
    assert_eq!(local_result["source_evidence"].as_array().unwrap().len(), 1);
    assert_eq!(local_result["source_evidence"][0]["source_id"], "mixed");
    assert_eq!(local_result["source_evidence"][0]["text"], QUOTE);
    for rewritten in ["松石47的维护预算为多少？", "维护预算，松石47是多少？"]
    {
        let (answer, _) = local(&store, rewritten, &budget);
        assert_eq!(answer["source_evidence"][0]["text"], QUOTE, "{rewritten}");
    }
    let (missing_subject, _) = local(&store, "维护预算是多少？", &budget);
    assert_eq!(
        missing_subject["reference_resolution"]["needs_clarification"],
        true
    );
    assert_eq!(missing_subject["source_evidence"], json!([]));
    let bound = store
        .recall_local(
            "维护预算是多少？",
            &["继续说松石47。".into()],
            &context(),
            InMemoryPredicateRegistry::default(),
            &InMemorySemanticRelationRegistry::default(),
            &budget,
        )
        .unwrap();
    assert_eq!(bound["reference_resolution"]["needs_clarification"], false);
    assert_eq!(bound["source_evidence"][0]["text"], QUOTE);
    let ambiguous_context = store
        .recall_local(
            "维护预算是多少？",
            &["继续说松石47和赤铜82。".into()],
            &context(),
            InMemoryPredicateRegistry::default(),
            &InMemorySemanticRelationRegistry::default(),
            &budget,
        )
        .unwrap();
    assert_eq!(
        ambiguous_context["reference_resolution"]["needs_clarification"],
        true
    );
    assert_eq!(ambiguous_context["source_evidence"], json!([]));
    for mismatch in ["松石47的维护成本为多少？", "赤铜82的维护预算为多少？"] {
        let (answer, _) = local(&store, mismatch, &budget);
        assert_eq!(answer["source_evidence"], json!([]), "{mismatch}");
    }
    let (assisted_result, changed) =
        assisted(&store, QUERY, &budget, &["维护预算".into()], &signature);
    assert!(!changed);
    assert_eq!(
        assisted_result["source_evidence"],
        local_result["source_evidence"]
    );

    let raw_db = Db::new();
    let mut raw_store = raw_db.open();
    raw_store
        .transaction(|state| {
            state
                .sources
                .insert("mixed".into(), source("mixed", MIXED, 1));
            Ok(())
        })
        .unwrap();
    let (raw_only, _) = local(&raw_store, QUERY, &budget);
    assert_eq!(raw_only["source_evidence"], json!([]));
}

#[test]
fn original_n05_generic_sentences_do_not_license_comparison_sources() {
    const QUESTION: &str = "松石47的技术选择和部署限制是否一致？";
    let db = Db::new();
    let mut store = db.open();
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
            let rows = [
                (
                    "E1",
                    "松石47",
                    "项目松石47选用 SQLite，Redis 暂时不引入。",
                    "项目松石47选用 SQLite",
                    "SQLite",
                ),
                (
                    "E2",
                    "松石47",
                    "松石47当前只部署在一台机器上；暂不引入 Redis，是为了减少维护成本。",
                    "松石47当前只部署在一台机器上",
                    "一台机器",
                ),
                (
                    "D1",
                    "赤铜82",
                    "赤铜82的技术选择是 PostgreSQL；赤铜82的部署限制是三台机器。",
                    "赤铜82的技术选择是 PostgreSQL；赤铜82的部署限制是三台机器",
                    "PostgreSQL",
                ),
            ];
            for (index, (id, _, text, _, _)) in rows.iter().enumerate() {
                let mut record = source(id, text, index as u64 + 1);
                record.enrichment = "committed".into();
                state.sources.insert((*id).into(), record);
            }
            state.with_engine(InMemoryPredicateRegistry::default(), |engine, _| {
                for (id, subject, text, _, _) in rows {
                    engine.ingest(common::grounded(CandidateMemory::new(
                        UserId::from("alice"),
                        EntityId::from(subject),
                        MemoryType::Fact,
                        Predicate::from("fact"),
                        MemoryValue::Text(text.split('；').next().unwrap().into()),
                        Timestamp(100),
                        Evidence::new(
                            EvidenceId(id.into()),
                            SessionId::from("session"),
                            TurnId::from(id),
                            text,
                        ),
                    )))?;
                }
                Ok(())
            })
        })
        .unwrap();

    let budget = LocalRecallBudget {
        candidate_k: 30,
        ..Default::default()
    };
    for query in [
        QUESTION,
        "松石47的技术选择和部署限制是否一致？请同时列出你查到的相关记忆。",
        "松石47的负责人是谁？",
    ] {
        let (answer, signature) = local(&store, query, &budget);
        assert_eq!(answer["source_evidence"], json!([]), "{query}: {answer}");
        if query.starts_with(QUESTION) {
            assert!(
                answer["degraded_reasons"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("comparison_evidence_incomplete")),
                "{query}: {answer}"
            );
            assert!(
                answer["comparison_evidence"]["missing_relations"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("部署限制")),
                "{query}: {answer}"
            );
        }
        let (assisted_answer, changed) = assisted(
            &store,
            query,
            &budget,
            &[
                "项目松石47选用 SQLite".into(),
                "松石47当前只部署在一台机器上".into(),
                "赤铜82 部署限制".into(),
            ],
            &signature,
        );
        assert!(!changed);
        if query == QUESTION {
            let candidates = candidate_source_ids(&assisted_answer);
            assert!(candidates.contains("E1"), "{assisted_answer}");
            assert!(candidates.contains("E2"), "{assisted_answer}");
            assert!(candidates.contains("D1"), "{assisted_answer}");
        }
        assert_eq!(
            assisted_answer["source_evidence"],
            json!([]),
            "{query}: {assisted_answer}"
        );
    }

    use hippocampus::chat::{ChatConfig, ChatMemoryService, EnrichmentMode, RetrievalMode};
    let mut service = ChatMemoryService::new(
        db.open(),
        ChatConfig {
            retrieval_mode: Some(RetrievalMode::Assisted),
            enrichment_mode: EnrichmentMode::Off,
            ..ChatConfig::default()
        },
    )
    .unwrap();
    for query in ["松石47 技术选择", "松石47", "松石47 部署限制"] {
        let answer = service
            .call("memory_recall", json!({"query":query}), || false)
            .unwrap();
        assert_eq!(answer["source_evidence"], json!([]), "{query}: {answer}");
        assert_eq!(answer["result"]["facts"], json!([]), "{query}: {answer}");
        assert_eq!(answer["model_calls"], json!([]), "{query}: {answer}");
    }
    let prepared = service
        .call(
            "memory_prepare",
            json!({"user_turn":{"text":"继续说松石47。","session_id":"session","turn_id":"followup","capture_source":true}}),
            || false,
        )
        .unwrap();
    assert_eq!(prepared["recall"]["status"], "completed", "{prepared}");
    assert!(
        !prepared["recall"]["result"]["source_evidence"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{prepared}"
    );
}

#[test]
fn comparison_requires_both_source_linked_relations_for_the_same_subject() {
    const QUESTION: &str = "松石47的技术选择和部署限制是否一致？";
    const CHOICE: &str = "松石47的技术选择是 SQLite。";
    const DEPLOYMENT: &str = "松石47的部署限制是一台机器。";
    const DECOY: &str = "赤铜82的技术选择是 PostgreSQL。";

    for include_deployment in [true, false] {
        let db = Db::new();
        let mut store = db.open();
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
                let rows = [
                    ("choice", "松石47", "技术选择", "SQLite", CHOICE),
                    ("deployment", "松石47", "部署限制", "一台机器", DEPLOYMENT),
                    ("decoy", "赤铜82", "技术选择", "PostgreSQL", DECOY),
                ];
                for (sequence, (id, _, _, _, quote)) in rows
                    .iter()
                    .filter(|(id, ..)| include_deployment || *id != "deployment")
                    .enumerate()
                {
                    let mut record = source(id, quote, sequence as u64 + 1);
                    record.enrichment = "committed".into();
                    state.sources.insert((*id).into(), record);
                }
                state.with_engine(InMemoryPredicateRegistry::default(), |engine, _| {
                    for (id, subject, property, value, quote) in rows
                        .into_iter()
                        .filter(|(id, ..)| include_deployment || *id != "deployment")
                    {
                        engine.ingest(common::grounded(CandidateMemory::new(
                            UserId::from("alice"),
                            EntityId::from(subject),
                            MemoryType::Fact,
                            Predicate::from(property),
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

        let budget = LocalRecallBudget::default();
        let (answer, signature) = local(&store, QUESTION, &budget);
        assert!(candidate_source_ids(&answer).contains("decoy"));
        let expected = if include_deployment {
            BTreeSet::from([("choice", CHOICE), ("deployment", DEPLOYMENT)])
        } else {
            BTreeSet::new()
        };
        let actual: BTreeSet<_> = answer["source_evidence"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| {
                (
                    item["source_id"].as_str().unwrap(),
                    item["text"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(actual, expected, "{QUESTION}: {answer}");
        if include_deployment {
            assert_eq!(
                answer["comparison_evidence"]["missing_relations"],
                json!([])
            );
        } else {
            assert_eq!(
                answer["comparison_evidence"]["missing_relations"],
                json!(["部署限制"])
            );
        }
        let (assisted_answer, changed) = assisted(
            &store,
            QUESTION,
            &budget,
            &["PostgreSQL".into()],
            &signature,
        );
        assert!(!changed);
        let assisted_actual: BTreeSet<_> = assisted_answer["source_evidence"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| {
                (
                    item["source_id"].as_str().unwrap(),
                    item["text"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(assisted_actual, expected, "assisted: {assisted_answer}");

        if include_deployment {
            let tight = LocalRecallBudget {
                source_limit: 1,
                ..budget
            };
            let (limited, _) = local(&store, QUESTION, &tight);
            assert_eq!(limited["source_evidence"], json!([]), "{limited}");
            assert_eq!(limited["result"]["facts"], json!([]), "{limited}");
        }

        let (absent, absent_signature) = local(&store, "琥珀93的维护预算是多少？", &budget);
        assert_eq!(absent["source_evidence"], json!([]), "{absent}");
        let (assisted_absent, changed) = assisted(
            &store,
            "琥珀93的维护预算是多少？",
            &budget,
            &["SQLite".into()],
            &absent_signature,
        );
        assert!(!changed);
        assert_eq!(
            assisted_absent["source_evidence"],
            json!([]),
            "{assisted_absent}"
        );
    }
}

#[test]
fn comparison_does_not_treat_question_wording_as_a_property_assertion() {
    for quote in [
        "松石47的技术选择是否为 SQLite；松石47的部署限制是一台机器。",
        "松石47的技术选择是 SQLite 还是 Redis；松石47的部署限制是一台机器。",
    ] {
        let db = Db::new();
        let mut store = db.open();
        store
            .transaction(|state| {
                state.entities.upsert(Entity {
                    id: EntityId::from("松石47"),
                    owner_id: UserId::from("alice"),
                    entity_type: EntityType::Other("project".into()),
                    canonical_name: "松石47".into(),
                    aliases: Vec::new(),
                });
                let mut record = source("questionish", quote, 1);
                record.enrichment = "committed".into();
                state.sources.insert("questionish".into(), record);
                state.with_engine(InMemoryPredicateRegistry::default(), |engine, _| {
                    engine.ingest(common::grounded(CandidateMemory::new(
                        UserId::from("alice"),
                        EntityId::from("松石47"),
                        MemoryType::Fact,
                        Predicate::from("部署限制"),
                        MemoryValue::Text("一台机器".into()),
                        Timestamp(100),
                        Evidence::new(
                            EvidenceId("deployment".into()),
                            SessionId::from("session"),
                            TurnId::from("questionish"),
                            quote,
                        ),
                    )))?;
                    Ok(())
                })
            })
            .unwrap();
        let (answer, _) = local(
            &store,
            "松石47的技术选择和部署限制是否一致？",
            &LocalRecallBudget::default(),
        );
        assert_eq!(answer["source_evidence"], json!([]), "{quote}: {answer}");
        assert_eq!(answer["result"]["facts"], json!([]), "{quote}: {answer}");
    }
}

#[test]
fn comparison_does_not_promote_invalidated_property_assertions() {
    const QUESTION: &str = "松石47的技术选择和部署限制是否一致？";
    const DEPLOYMENT: &str = "松石47的部署限制是一台机器。";
    for invalid_choice in [
        "松石47的技术选择是 SQLite 的说法已被否定。",
        "松石47的技术选择是 SQLite；这条记录无效。",
    ] {
        let db = Db::new();
        let mut store = db.open();
        store
            .transaction(|state| {
                state.entities.upsert(Entity {
                    id: EntityId::from("松石47"),
                    owner_id: UserId::from("alice"),
                    entity_type: EntityType::Other("project".into()),
                    canonical_name: "松石47".into(),
                    aliases: Vec::new(),
                });
                for (sequence, (id, property, value, quote)) in [
                    ("choice", "技术选择", "SQLite", invalid_choice),
                    ("deployment", "部署限制", "一台机器", DEPLOYMENT),
                ]
                .into_iter()
                .enumerate()
                {
                    let mut record = source(id, quote, sequence as u64 + 1);
                    record.enrichment = "committed".into();
                    state.sources.insert(id.into(), record);
                    state.with_engine(InMemoryPredicateRegistry::default(), |engine, _| {
                        engine.ingest(common::grounded(CandidateMemory::new(
                            UserId::from("alice"),
                            EntityId::from("松石47"),
                            MemoryType::Fact,
                            Predicate::from(property),
                            MemoryValue::Text(value.into()),
                            Timestamp(100),
                            Evidence::new(
                                EvidenceId(id.into()),
                                SessionId::from("session"),
                                TurnId::from(id),
                                quote,
                            ),
                        )))?;
                        Ok(())
                    })?;
                }
                Ok(())
            })
            .unwrap();
        let budget = LocalRecallBudget::default();
        let (answer, signature) = local(&store, QUESTION, &budget);
        assert_eq!(
            answer["source_evidence"],
            json!([]),
            "{invalid_choice}: {answer}"
        );
        let (assisted_answer, _) = assisted(
            &store,
            QUESTION,
            &budget,
            &["松石47 SQLite 一台机器".into()],
            &signature,
        );
        assert_eq!(
            assisted_answer["source_evidence"],
            json!([]),
            "{invalid_choice}: {assisted_answer}"
        );
    }
}

#[test]
fn comparison_requires_a_memory_link_for_each_property() {
    const QUOTE: &str = "松石47的技术选择是 SQLite；松石47的部署限制是一台机器。";
    let db = Db::new();
    let mut store = db.open();
    store
        .transaction(|state| {
            state.entities.upsert(Entity {
                id: EntityId::from("松石47"),
                owner_id: UserId::from("alice"),
                entity_type: EntityType::Other("project".into()),
                canonical_name: "松石47".into(),
                aliases: Vec::new(),
            });
            let mut record = source("mixed", QUOTE, 1);
            record.enrichment = "committed".into();
            state.sources.insert("mixed".into(), record);
            state.with_engine(InMemoryPredicateRegistry::default(), |engine, _| {
                engine.ingest(common::grounded(CandidateMemory::new(
                    UserId::from("alice"),
                    EntityId::from("松石47"),
                    MemoryType::Fact,
                    Predicate::from("技术选择"),
                    MemoryValue::Text("SQLite".into()),
                    Timestamp(100),
                    Evidence::new(
                        EvidenceId("choice".into()),
                        SessionId::from("session"),
                        TurnId::from("mixed"),
                        QUOTE,
                    ),
                )))?;
                Ok(())
            })
        })
        .unwrap();

    let (answer, _) = local(
        &store,
        "松石47的技术选择和部署限制是否一致？",
        &LocalRecallBudget::default(),
    );
    assert_eq!(answer["source_evidence"], json!([]), "{answer}");
    assert_eq!(answer["result"]["facts"], json!([]), "{answer}");
}

#[test]
fn newer_uncommitted_relation_without_correction_word_blocks_comparison() {
    const QUESTION: &str = "松石47的技术选择和部署限制是否一致？";
    let db = Db::new();
    let mut store = db.open();
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
            for (sequence, (id, property, value, quote)) in [
                (
                    "choice",
                    "技术选择",
                    "SQLite",
                    "松石47的技术选择是 SQLite。",
                ),
                (
                    "deployment",
                    "部署限制",
                    "一台机器",
                    "松石47的部署限制是一台机器。",
                ),
            ]
            .into_iter()
            .enumerate()
            {
                let mut record = source(id, quote, sequence as u64 + 1);
                record.enrichment = "committed".into();
                state.sources.insert(id.into(), record);
                state.with_engine(InMemoryPredicateRegistry::default(), |engine, _| {
                    engine.ingest(common::grounded(CandidateMemory::new(
                        UserId::from("alice"),
                        EntityId::from("松石47"),
                        MemoryType::Fact,
                        Predicate::from(property),
                        MemoryValue::Text(value.into()),
                        Timestamp(100),
                        Evidence::new(
                            EvidenceId(id.into()),
                            SessionId::from("session"),
                            TurnId::from(id),
                            quote,
                        ),
                    )))?;
                    Ok(())
                })?;
            }
            let mut other = source("other_pending", "赤铜82的部署限制是两台机器。", 3);
            other.enrichment = "extraction_incomplete".into();
            state.sources.insert("other_pending".into(), other);
            Ok(())
        })
        .unwrap();

    let (unaffected, _) = local(&store, QUESTION, &LocalRecallBudget::default());
    assert_eq!(
        unaffected["source_evidence"].as_array().unwrap().len(),
        2,
        "{unaffected}"
    );

    store
        .transaction(|state| {
            let mut pending = source("pending", "松石47的部署限制是两台机器。", 4);
            pending.enrichment = "extraction_incomplete".into();
            state.sources.insert("pending".into(), pending);
            Ok(())
        })
        .unwrap();
    let (answer, _) = local(&store, QUESTION, &LocalRecallBudget::default());
    assert_eq!(answer["source_evidence"], json!([]), "{answer}");
    assert_eq!(
        answer["evidence_status"], "uncommitted_newer_revision",
        "{answer}"
    );
}

#[test]
fn comparison_reserves_source_slots_for_both_properties() {
    let db = Db::new();
    let mut store = db.open();
    store
        .transaction(|state| {
            state.entities.upsert(Entity {
                id: EntityId::from("松石47"),
                owner_id: UserId::from("alice"),
                entity_type: EntityType::Other("project".into()),
                canonical_name: "松石47".into(),
                aliases: Vec::new(),
            });
            let rows = [
                (
                    "choice_new",
                    "松石47的技术选择是 SQLite；关于部署限制尚无结论。",
                    "松石47的技术选择是 SQLite",
                    "技术选择",
                    "SQLite",
                    3,
                ),
                (
                    "choice_old",
                    "松石47的技术选择是 SQLite；部署限制以后再说。",
                    "松石47的技术选择是 SQLite",
                    "技术选择",
                    "SQLite",
                    2,
                ),
                (
                    "deployment",
                    "松石47的部署限制是一台机器。",
                    "松石47的部署限制是一台机器",
                    "部署限制",
                    "一台机器",
                    1,
                ),
            ];
            for (id, text, _, _, _, sequence) in rows {
                let mut record = source(id, text, sequence);
                record.enrichment = "committed".into();
                state.sources.insert(id.into(), record);
            }
            state.with_engine(InMemoryPredicateRegistry::default(), |engine, _| {
                for (id, _, quote, property, value, _) in rows {
                    engine.ingest(common::grounded(CandidateMemory::new(
                        UserId::from("alice"),
                        EntityId::from("松石47"),
                        MemoryType::Fact,
                        Predicate::from(property),
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
    let budget = LocalRecallBudget {
        source_limit: 2,
        ..Default::default()
    };
    let (answer, _) = local(&store, "松石47的技术选择和部署限制是否一致？", &budget);
    let sources: BTreeSet<_> = answer["source_evidence"]
        .as_array()
        .unwrap()
        .iter()
        .map(|source| source["source_id"].as_str().unwrap())
        .collect();
    assert_eq!(sources.len(), 2, "{answer}");
    assert!(sources.contains("deployment"), "{answer}");
    assert!(
        sources.contains("choice_new") || sources.contains("choice_old"),
        "{answer}"
    );
}

#[test]
fn pending_revision_of_one_compared_property_blocks_old_mixed_quote() {
    const OLD: &str = "松石47的技术选择是 SQLite；松石47的部署限制是一台机器。";
    const DEPLOYMENT: &str = "松石47的部署限制是一台机器";
    let db = Db::new();
    let mut store = db.open();
    store
        .transaction(|state| {
            state.entities.upsert(Entity {
                id: EntityId::from("松石47"),
                owner_id: UserId::from("alice"),
                entity_type: EntityType::Other("project".into()),
                canonical_name: "松石47".into(),
                aliases: Vec::new(),
            });
            let mut old = source("old", OLD, 1);
            old.enrichment = "committed".into();
            state.sources.insert("old".into(), old);
            let mut pending = source("pending", "更正：松石47的部署限制是两台机器。", 2);
            pending.enrichment = "extraction_incomplete".into();
            state.sources.insert("pending".into(), pending);
            state.with_engine(InMemoryPredicateRegistry::default(), |engine, _| {
                for (id, property, value, quote) in [
                    ("choice", "技术选择", "SQLite", OLD),
                    ("deployment", "部署限制", "一台机器", DEPLOYMENT),
                ] {
                    engine.ingest(common::grounded(CandidateMemory::new(
                        UserId::from("alice"),
                        EntityId::from("松石47"),
                        MemoryType::Fact,
                        Predicate::from(property),
                        MemoryValue::Text(value.into()),
                        Timestamp(100),
                        Evidence::new(
                            EvidenceId(id.into()),
                            SessionId::from("session"),
                            TurnId::from("old"),
                            quote,
                        ),
                    )))?;
                }
                Ok(())
            })
        })
        .unwrap();
    let (answer, _) = local(
        &store,
        "松石47的技术选择和部署限制是否一致？",
        &LocalRecallBudget::default(),
    );
    assert_eq!(answer["source_evidence"], json!([]), "{answer}");
    assert_eq!(answer["result"]["facts"], json!([]), "{answer}");
    assert_eq!(answer["historical_or_conflicting"], json!([]), "{answer}");
}

#[test]
fn truncated_source_links_cannot_hide_a_pending_compared_revision() {
    const OLD: &str = "松石47的技术选择是 SQLite；松石47的部署限制是一台机器。";
    const DEPLOYMENT: &str = "松石47的部署限制是一台机器";
    let db = Db::new();
    let mut store = db.open();
    store
        .transaction(|state| {
            state.entities.upsert(Entity {
                id: EntityId::from("松石47"),
                owner_id: UserId::from("alice"),
                entity_type: EntityType::Other("project".into()),
                canonical_name: "松石47".into(),
                aliases: Vec::new(),
            });
            let mut old = source("old", OLD, 1);
            old.enrichment = "committed".into();
            state.sources.insert("old".into(), old);
            let mut pending = source("pending", "更正：松石47的部署限制是两台机器。", 2);
            pending.enrichment = "extraction_incomplete".into();
            state.sources.insert("pending".into(), pending);
            state.with_engine(InMemoryPredicateRegistry::default(), |engine, _| {
                let mut rows = vec![(
                    "choice".to_owned(),
                    "技术选择".to_owned(),
                    "SQLite".to_owned(),
                    OLD,
                )];
                rows.extend((0..19).map(|i| {
                    (
                        format!("filler_{i}"),
                        format!("filler_{i}"),
                        "x".to_owned(),
                        OLD,
                    )
                }));
                rows.push((
                    "deployment".to_owned(),
                    "部署限制".to_owned(),
                    "一台机器".to_owned(),
                    DEPLOYMENT,
                ));
                for (id, property, value, quote) in rows {
                    engine.ingest(common::grounded(CandidateMemory::new(
                        UserId::from("alice"),
                        EntityId::from("松石47"),
                        MemoryType::Fact,
                        Predicate::from(property.as_str()),
                        MemoryValue::Text(value),
                        Timestamp(100),
                        Evidence::new(
                            EvidenceId(id),
                            SessionId::from("session"),
                            TurnId::from("old"),
                            quote,
                        ),
                    )))?;
                }
                Ok(())
            })
        })
        .unwrap();
    let search = store.search_sources("松石47", None, 30).unwrap();
    let old_candidate = search["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["source_id"] == "old")
        .unwrap();
    assert_eq!(old_candidate["mapping_uncertain"], true, "{old_candidate}");
    assert_eq!(
        old_candidate["known_memories"].as_array().unwrap().len(),
        20
    );
    assert!(
        old_candidate["known_memories"]
            .as_array()
            .unwrap()
            .iter()
            .all(|link| { link["quote_byte_range"] == json!([0, OLD.len()]) })
    );

    let (answer, _) = local(
        &store,
        "松石47的技术选择和部署限制是否一致？",
        &LocalRecallBudget::default(),
    );
    assert_eq!(answer["source_evidence"], json!([]), "{answer}");
    assert_eq!(answer["result"]["facts"], json!([]), "{answer}");
}

#[test]
fn committed_self_preference_answers_what_i_like_without_clarification() {
    const QUOTE: &str = "我喜欢绿茶";
    let db = Db::new();
    let mut store = db.open();
    store
        .transaction(|state| {
            let mut committed = source("preference", "我喜欢绿茶。", 1);
            committed.enrichment = "committed".into();
            state.sources.insert("preference".into(), committed);
            state.with_engine(InMemoryPredicateRegistry::default(), |engine, _| {
                engine.ingest(common::grounded(CandidateMemory::new(
                    UserId::from("alice"),
                    EntityId::from("self"),
                    MemoryType::Preference,
                    Predicate::from("likes"),
                    MemoryValue::Text("绿茶".into()),
                    Timestamp(100),
                    Evidence::new(
                        EvidenceId("likes-green-tea".into()),
                        SessionId::from("session"),
                        TurnId::from("preference"),
                        QUOTE,
                    ),
                )))?;
                Ok(())
            })
        })
        .unwrap();

    let (result, _) = local(
        &store,
        "我之前说的我喜欢的是什么？",
        &LocalRecallBudget::default(),
    );
    assert_eq!(result["reference_resolution"]["needs_clarification"], false);
    assert_eq!(result["source_evidence"][0]["text"], QUOTE);
    assert_eq!(
        result["result"]["preferences"][0]["value"],
        json!({"Text":"绿茶"})
    );
}

#[test]
fn linked_quotes_prove_cause_time_and_current_correction_without_cross_claims() {
    let db = Db::new();
    let mut store = db.open();
    let statements = [
        ("phone", "王阿姨的手机坏了，因为进水。"),
        ("phone_decoy", "王阿姨的手机坏了，旁边设备因为进水。"),
        ("appointment", "牙医预约改到了周二上午。"),
        ("appointment_decoy", "牙医预约改了，体检预约改到周二上午。"),
        ("budget_old", "松石47的维护预算是 500 元。"),
        (
            "budget_new",
            "更正：松石47的维护预算是 600 元，之前的 500 元作废。",
        ),
    ];
    store
        .transaction(|state| {
            for (name, kind) in [
                ("王阿姨", EntityType::Person),
                ("李阿姨", EntityType::Person),
                ("牙医预约", EntityType::Other("event".into())),
                ("松石47", EntityType::Other("project".into())),
            ] {
                state.entities.upsert(Entity {
                    id: EntityId::from(name),
                    owner_id: UserId::from("alice"),
                    entity_type: kind,
                    canonical_name: name.into(),
                    aliases: Vec::new(),
                });
            }
            for (sequence, (id, text)) in statements.iter().enumerate() {
                let mut record = source(id, text, sequence as u64 + 1);
                record.enrichment = "committed".into();
                state.sources.insert((*id).into(), record);
            }
            let mut predicates = InMemoryPredicateRegistry::default();
            predicates.register(Predicate::from("maintenance_budget"), Cardinality::Single);
            state.with_engine(predicates, |engine, _| {
                let make = |source_id: &str, subject: &str, predicate: &str, value: &str| {
                    let quote = statements
                        .iter()
                        .find(|(id, _)| *id == source_id)
                        .unwrap()
                        .1;
                    CandidateMemory::new(
                        UserId::from("alice"),
                        EntityId::from(subject),
                        MemoryType::Fact,
                        Predicate::from(predicate),
                        MemoryValue::Text(value.into()),
                        Timestamp(100),
                        Evidence::new(
                            EvidenceId(source_id.into()),
                            SessionId::from("session"),
                            TurnId::from(source_id),
                            quote,
                        ),
                    )
                };
                for source_id in ["phone", "phone_decoy"] {
                    engine.ingest(common::grounded(make(
                        source_id,
                        "王阿姨",
                        "phone_failure_cause",
                        "手机因为进水坏了",
                    )))?;
                }
                for source_id in ["appointment", "appointment_decoy"] {
                    engine.ingest(common::grounded(make(
                        source_id,
                        "牙医预约",
                        "appointment_time",
                        "周二上午",
                    )))?;
                }
                let old = engine.ingest(common::grounded(make(
                    "budget_old",
                    "松石47",
                    "maintenance_budget",
                    "500 元",
                )))?;
                let IngestOutcome::Applied {
                    memory_id: Some(old_id),
                    ..
                } = old
                else {
                    panic!("old budget did not commit");
                };
                let mut corrected = make("budget_new", "松石47", "maintenance_budget", "600 元");
                corrected.intent = CandidateIntent::Supersede {
                    target: Some(old_id),
                    reason: SupersedeReason::Correction,
                };
                engine.ingest(common::grounded(corrected))?;
                Ok(())
            })
        })
        .unwrap();

    for (query, expected) in [
        ("王阿姨的手机为什么坏了？", "phone"),
        ("牙医预约改到什么时候？", "appointment"),
        ("松石47现在的维护预算是多少？", "budget_new"),
    ] {
        let (answer, _) = local(&store, query, &LocalRecallBudget::default());
        assert_eq!(
            answer["source_evidence"].as_array().unwrap().len(),
            1,
            "{query}: {answer}"
        );
        assert_eq!(
            answer["source_evidence"][0]["text"],
            statements.iter().find(|(id, _)| *id == expected).unwrap().1,
            "{query}: {answer}"
        );
    }
    for query in ["为什么坏了？", "改到什么时候？"] {
        let (answer, _) = local(&store, query, &LocalRecallBudget::default());
        assert_eq!(answer["reference_resolution"]["needs_clarification"], true);
        assert_eq!(answer["source_evidence"], json!([]));
    }
    for (query, user_context, expected) in [
        ("手机为什么坏了？", "王阿姨的手机。", Some("phone")),
        ("改到什么时候？", "牙医预约。", Some("appointment")),
        ("手机为什么坏了？", "李阿姨的手机。", None),
        ("手机为什么坏了？", "王阿姨的车。", None),
    ] {
        let answer = store
            .recall_local(
                query,
                &[user_context.into()],
                &context(),
                InMemoryPredicateRegistry::default(),
                &InMemorySemanticRelationRegistry::default(),
                &LocalRecallBudget::default(),
            )
            .unwrap();
        match expected {
            Some(id) => assert_eq!(
                answer["source_evidence"][0]["text"],
                statements.iter().find(|(key, _)| *key == id).unwrap().1,
                "{query} + {user_context}: {answer}"
            ),
            None => assert_eq!(
                answer["source_evidence"],
                json!([]),
                "{query} + {user_context}: {answer}"
            ),
        }
        if user_context == "王阿姨的车。" {
            assert_eq!(answer["reference_resolution"]["needs_clarification"], true);
        }
    }
    for (query, expected) in [
        ("手机为什么坏了？王阿姨的手机。", Some("phone")),
        ("改到什么时候？牙医预约。", Some("appointment")),
        ("维护预算是多少？松石47。", Some("budget_new")),
        ("手机为什么坏了？李阿姨的手机。", None),
        ("手机为什么坏了？王阿姨的车。", None),
        ("手机为什么坏了？王阿姨的手机、李阿姨的手机。", None),
        ("手机为什么坏了？不是王阿姨的手机。", None),
        ("手机为什么坏了？王阿姨的手机以外。", None),
        ("手机为什么坏了？王阿姨的手机为什么没电？", None),
        ("维护预算是多少？松石47以外。", None),
        ("维护预算是多少？松石47的其他项目。", None),
        ("维护预算是多少？松石47的竞争对手。", None),
        ("松石47的负责人是谁？", None),
    ] {
        let (answer, _) = local(&store, query, &LocalRecallBudget::default());
        match expected {
            Some(id) => assert_eq!(
                answer["source_evidence"][0]["text"],
                statements.iter().find(|(key, _)| *key == id).unwrap().1,
                "{query}: {answer}"
            ),
            None => assert_eq!(answer["source_evidence"], json!([]), "{query}: {answer}"),
        }
    }
}

#[test]
fn uncommitted_newer_corrections_do_not_leave_old_current_answers() {
    let db = Db::new();
    let mut store = db.open();
    let old = [
        (
            "phone",
            "王阿姨",
            "手机号码",
            "13800000000",
            "王阿姨的手机号码是 13800000000。",
        ),
        (
            "email",
            "李叔叔",
            "邮箱",
            "old@example.com",
            "李叔叔的邮箱是 old@example.com。",
        ),
        ("address", "王阿姨", "住址", "北京", "王阿姨的住址是北京。"),
        (
            "other_address",
            "李叔叔",
            "住址",
            "杭州",
            "李叔叔的住址是杭州。",
        ),
        (
            "software",
            "陈老师",
            "常用软件",
            "旧版套件",
            "陈老师的常用软件是旧版套件。",
        ),
    ];
    store
        .transaction(|state| {
            for name in ["王阿姨", "李叔叔", "陈老师"] {
                state.entities.upsert(Entity {
                    id: EntityId::from(name),
                    owner_id: UserId::from("alice"),
                    entity_type: EntityType::Person,
                    canonical_name: name.into(),
                    aliases: Vec::new(),
                });
            }
            for (index, (id, _, _, _, quote)) in old.iter().enumerate() {
                let mut record = source(id, quote, index as u64 + 1);
                record.enrichment = "committed".into();
                state.sources.insert((*id).into(), record);
            }
            for (id, text, sequence) in [
                (
                    "phone_fix",
                    "更正：王阿姨的手机号码是 13900000000，之前的号码作废。",
                    6,
                ),
                (
                    "email_retract",
                    "撤回：李叔叔的邮箱信息，之前的 old@example.com 作废。",
                    7,
                ),
            ] {
                let mut record = source(id, text, sequence);
                record.enrichment = "extraction_incomplete".into();
                state.sources.insert(id.into(), record);
            }
            state.with_engine(InMemoryPredicateRegistry::default(), |engine, _| {
                for (id, subject, property, value, quote) in old {
                    engine.ingest(common::grounded(CandidateMemory::new(
                        UserId::from("alice"),
                        EntityId::from(subject),
                        MemoryType::Fact,
                        Predicate::from(property),
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

    for query in ["王阿姨现在的手机号码是多少？", "李叔叔目前的邮箱是什么？"]
    {
        let (answer, _) = local(&store, query, &LocalRecallBudget::default());
        assert_eq!(answer["source_evidence"], json!([]), "{query}: {answer}");
        for lane in [
            "mandatory",
            "constraints",
            "preferences",
            "facts",
            "episodes",
        ] {
            assert_eq!(
                answer["result"][lane],
                json!([]),
                "{query}: {lane}: {answer}"
            );
        }
        assert_eq!(
            answer["historical_or_conflicting"],
            json!([]),
            "{query}: {answer}"
        );
        assert_eq!(answer["evidence_status"], "uncommitted_newer_revision");
        assert!(
            answer["degraded_reasons"]
                .as_array()
                .unwrap()
                .iter()
                .any(|reason| reason == "uncommitted_newer_revision"),
            "{query}: {answer}"
        );
    }
    let (unrelated, _) = local(
        &store,
        "王阿姨的住址是什么？",
        &LocalRecallBudget::default(),
    );
    assert_eq!(unrelated["source_evidence"][0]["text"], old[2].4);
    assert_eq!(
        unrelated["result"]["facts"][0]["value"],
        json!({"Text":"北京"})
    );

    store
        .transaction(|state| {
            for (id, text, sequence) in [
                ("address_change", "王阿姨的住址改为上海。", 8),
                ("software_stop", "陈老师的常用软件不再使用旧版套件。", 9),
            ] {
                let mut record = source(id, text, sequence);
                record.enrichment = "extraction_incomplete".into();
                state.sources.insert(id.into(), record);
            }
            Ok(())
        })
        .unwrap();
    for query in ["王阿姨的住址是什么？", "陈老师现在的常用软件是什么？"] {
        let (answer, _) = local(&store, query, &LocalRecallBudget::default());
        assert_eq!(answer["source_evidence"], json!([]), "{query}: {answer}");
        assert_eq!(answer["result"]["facts"], json!([]), "{query}: {answer}");
        assert_eq!(answer["evidence_status"], "uncommitted_newer_revision");
    }
    let (still_unrelated, _) = local(
        &store,
        "李叔叔的住址是什么？",
        &LocalRecallBudget::default(),
    );
    assert_eq!(still_unrelated["source_evidence"][0]["text"], old[3].4);

    store
        .transaction(|state| {
            let mut record = source("unbound_fix", "更正：之前那条记录作废。", 10);
            record.enrichment = "extraction_incomplete".into();
            state.sources.insert("unbound_fix".into(), record);
            Ok(())
        })
        .unwrap();
    let (unbound, _) = local(
        &store,
        "李叔叔的住址是什么？",
        &LocalRecallBudget::default(),
    );
    assert_eq!(unbound["source_evidence"], json!([]));
    assert_eq!(unbound["result"]["facts"], json!([]));
    assert_eq!(unbound["evidence_status"], "uncommitted_newer_revision");
}

#[test]
fn older_pending_revision_does_not_hide_newer_committed_answer() {
    let db = Db::new();
    let mut store = db.open();
    store
        .transaction(|state| {
            state.entities.upsert(Entity {
                id: EntityId::from("赵姐"),
                owner_id: UserId::from("alice"),
                entity_type: EntityType::Person,
                canonical_name: "赵姐".into(),
                aliases: Vec::new(),
            });
            let mut pending = source("pending", "更正：赵姐的邮箱改为 stale@example.com。", 2);
            pending.enrichment = "extraction_incomplete".into();
            state.sources.insert("pending".into(), pending);
            let mut committed = source("committed", "赵姐的邮箱是 fresh@example.com。", 3);
            committed.enrichment = "committed".into();
            state.sources.insert("committed".into(), committed);
            state.with_engine(InMemoryPredicateRegistry::default(), |engine, _| {
                engine.ingest(common::grounded(CandidateMemory::new(
                    UserId::from("alice"),
                    EntityId::from("赵姐"),
                    MemoryType::Fact,
                    Predicate::from("邮箱"),
                    MemoryValue::Text("fresh@example.com".into()),
                    Timestamp(100),
                    Evidence::new(
                        EvidenceId("committed".into()),
                        SessionId::from("session"),
                        TurnId::from("committed"),
                        "赵姐的邮箱是 fresh@example.com。",
                    ),
                )))?;
                Ok(())
            })
        })
        .unwrap();
    let query = "赵姐现在的邮箱是什么？";
    let (answer, _) = local(&store, query, &LocalRecallBudget::default());
    assert_eq!(answer["source_evidence"][0]["source_id"], "committed");
    assert_eq!(
        answer["result"]["facts"][0]["value"],
        json!({"Text":"fresh@example.com"})
    );
    assert_eq!(answer["evidence_status"], "sufficiency_not_verified");
}

#[test]
fn uncommitted_revision_blocks_legacy_memory_without_source_sequence() {
    let db = Db::new();
    let mut store = db.open();
    store
        .transaction(|state| {
            state.entities.upsert(Entity {
                id: EntityId::from("赵姐"),
                owner_id: UserId::from("alice"),
                entity_type: EntityType::Person,
                canonical_name: "赵姐".into(),
                aliases: Vec::new(),
            });
            let mut pending = source("new_pending", "赵姐的邮箱改为 final@example.com。", 4);
            pending.enrichment = "extraction_incomplete".into();
            state.sources.insert("new_pending".into(), pending);
            state.with_engine(InMemoryPredicateRegistry::default(), |engine, _| {
                engine.ingest(common::grounded(CandidateMemory::new(
                    UserId::from("alice"),
                    EntityId::from("赵姐"),
                    MemoryType::Fact,
                    Predicate::from("邮箱"),
                    MemoryValue::Text("old@example.com".into()),
                    Timestamp(100),
                    Evidence::new(
                        EvidenceId("legacy".into()),
                        SessionId::from("session"),
                        TurnId::from("legacy"),
                        "赵姐的邮箱是 old@example.com。",
                    ),
                )))?;
                Ok(())
            })
        })
        .unwrap();
    let (answer, _) = local(&store, "赵姐的邮箱是什么？", &LocalRecallBudget::default());
    assert_eq!(answer["result"]["facts"], json!([]));
    assert_eq!(answer["source_evidence"], json!([]));
    assert_eq!(answer["evidence_status"], "uncommitted_newer_revision");
}

#[test]
fn incidental_person_mention_does_not_hide_their_unrelated_fact() {
    let db = Db::new();
    let mut store = db.open();
    let old = [
        ("address", "王阿姨", "住址", "北京", "王阿姨的住址是北京。"),
        (
            "email",
            "李叔叔",
            "邮箱",
            "old@example.com",
            "李叔叔的邮箱是 old@example.com。",
        ),
        (
            "electronic_email",
            "陈老师",
            "电子邮箱",
            "old2@example.com",
            "陈老师的电子邮箱是 old2@example.com。",
        ),
    ];
    store
        .transaction(|state| {
            for name in ["王阿姨", "李叔叔", "陈老师"] {
                state.entities.upsert(Entity {
                    id: EntityId::from(name),
                    owner_id: UserId::from("alice"),
                    entity_type: EntityType::Person,
                    canonical_name: name.into(),
                    aliases: Vec::new(),
                });
            }
            for (index, (id, _, _, _, quote)) in old.iter().enumerate() {
                let mut record = source(id, quote, index as u64 + 1);
                record.enrichment = "committed".into();
                state.sources.insert((*id).into(), record);
            }
            let mut revision = source(
                "mixed",
                "更正：王阿姨说，李叔叔的邮箱是 new@example.com。",
                4,
            );
            revision.enrichment = "extraction_incomplete".into();
            state.sources.insert("mixed".into(), revision);
            state.with_engine(InMemoryPredicateRegistry::default(), |engine, _| {
                for (id, subject, property, value, quote) in old {
                    engine.ingest(common::grounded(CandidateMemory::new(
                        UserId::from("alice"),
                        EntityId::from(subject),
                        MemoryType::Fact,
                        Predicate::from(property),
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
    let (address, _) = local(
        &store,
        "王阿姨的住址是什么？",
        &LocalRecallBudget::default(),
    );
    assert_eq!(address["source_evidence"][0]["text"], old[0].4);
    let (email, _) = local(
        &store,
        "李叔叔的邮箱是什么？",
        &LocalRecallBudget::default(),
    );
    assert_eq!(email["source_evidence"], json!([]));
    assert_eq!(email["result"]["facts"], json!([]));
    assert_eq!(email["evidence_status"], "uncommitted_newer_revision");

    store
        .transaction(|state| {
            let mut revision = source(
                "two_subjects",
                "更正：王阿姨的住址是上海，陈老师的邮箱是 new2@example.com。",
                5,
            );
            revision.enrichment = "extraction_incomplete".into();
            state.sources.insert("two_subjects".into(), revision);
            Ok(())
        })
        .unwrap();
    for query in ["王阿姨的住址是什么？", "陈老师的电子邮箱是什么？"] {
        let (answer, _) = local(&store, query, &LocalRecallBudget::default());
        assert_eq!(answer["source_evidence"], json!([]), "{query}: {answer}");
        assert_eq!(answer["result"]["facts"], json!([]), "{query}: {answer}");
    }
}

#[test]
fn committed_source_with_unapplied_correction_cannot_keep_old_slot_current() {
    use hippocampus::chat::{ChatConfig, ChatMemoryService, RetrievalMode};

    let db = Db::new();
    let mut store = db.open();
    let old = "赵姐的邮箱是 old@example.com。";
    let revised = "更正：赵姐的邮箱改为 new@example.com。赵姐的职业是教师。";
    store
        .transaction(|state| {
            state.entities.upsert(Entity {
                id: EntityId::from("赵姐"),
                owner_id: UserId::from("alice"),
                entity_type: EntityType::Person,
                canonical_name: "赵姐".into(),
                aliases: Vec::new(),
            });
            for (id, text, sequence) in [("old", old, 1), ("partial", revised, 2)] {
                let mut record = source(id, text, sequence);
                record.enrichment = "committed".into();
                state.sources.insert(id.into(), record);
            }
            state.with_engine(InMemoryPredicateRegistry::default(), |engine, _| {
                for (id, predicate, value, quote) in [
                    ("old", "邮箱", "old@example.com", old),
                    ("partial", "职业", "教师", "赵姐的职业是教师。"),
                ] {
                    engine.ingest(common::grounded(CandidateMemory::new(
                        UserId::from("alice"),
                        EntityId::from("赵姐"),
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
    let (email, _) = local(
        &store,
        "赵姐现在的邮箱是什么？",
        &LocalRecallBudget::default(),
    );
    assert_eq!(email["source_evidence"], json!([]));
    assert_eq!(email["result"]["facts"], json!([]));
    assert_eq!(email["evidence_status"], "uncommitted_newer_revision");
    let (job, _) = local(&store, "赵姐的职业是什么？", &LocalRecallBudget::default());
    assert_eq!(job["source_evidence"][0]["text"], "赵姐的职业是教师。");

    let mut service = ChatMemoryService::new(
        db.open(),
        ChatConfig {
            retrieval_mode: Some(RetrievalMode::Assisted),
            ..ChatConfig::default()
        },
    )
    .unwrap();
    let assisted = service
        .call(
            "memory_recall",
            json!({"query":"赵姐现在的邮箱是什么？"}),
            || false,
        )
        .unwrap();
    assert_eq!(
        assisted["assistance"],
        json!({"status":"not_requested","reason":"uncommitted_newer_revision"})
    );
    assert_eq!(assisted["model_calls"], json!([]));
}
