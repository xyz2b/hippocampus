mod common;

use hippocampus::*;
use serde_json::{Value, json};

struct Db(std::path::PathBuf);
impl Db {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("s5-facts-{}.sqlite3", diagnostics::new_id())))
    }
    fn open(&self, owner: &str) -> SqliteMemoryStore {
        SqliteMemoryStore::open(&self.0, UserId::from(owner)).unwrap()
    }
    fn sql(&self, sql: &str) {
        rusqlite::Connection::open(&self.0)
            .unwrap()
            .execute_batch(sql)
            .unwrap();
    }
}
impl Drop for Db {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
fn registry() -> InMemoryPredicateRegistry {
    let mut r = InMemoryPredicateRegistry::default();
    r.register(Predicate::from("normalized_key"), Cardinality::Single);
    r
}
fn insert(
    store: &mut SqliteMemoryStore,
    turn: &str,
    raw: &str,
    quote: &str,
    value: &str,
    task: Option<&str>,
) -> MemoryId {
    let owner = store.owner().clone();
    store
        .transaction(|s| {
            s.sources.insert(
                turn.into(),
                serde_json::from_value(json!({"id":turn,"session_id":"session","turn_id":turn,
            "observed_at":100,"received_sequence":s.sources.len()+1,
            "request":{"text":raw,"capture_source":true,"task_id":task},
            "enrichment":"completed_pending","error":null,"result":null,"base_revision":0}))?,
            );
            let c = CandidateMemory::new(
                owner.clone(),
                EntityId::from(turn),
                MemoryType::Fact,
                Predicate::from("normalized_key"),
                MemoryValue::Text(value.into()),
                Timestamp(100),
                Evidence::new(
                    EvidenceId::from(turn),
                    SessionId::from("session"),
                    TurnId::from(turn),
                    quote,
                ),
            );
            s.with_engine(registry(), |e, _| match e.ingest(common::grounded(c))? {
                IngestOutcome::Applied {
                    memory_id: Some(id),
                    ..
                } => Ok(id),
                o => Err(format!("{o:?}").into()),
            })
        })
        .unwrap()
}
fn recall(store: &SqliteMemoryStore, query: &str, task: Option<&str>) -> Value {
    let mut ctx = RuntimeContext::current(store.owner().clone(), Timestamp(200));
    ctx.task_id = task.map(TaskId::from);
    store
        .recall_local(
            query,
            &[],
            &ctx,
            registry(),
            &InMemorySemanticRelationRegistry::default(),
            &LocalRecallBudget {
                candidate_k: 1,
                source_limit: 1,
                ..Default::default()
            },
        )
        .unwrap()
}
fn ids(v: &Value) -> Vec<String> {
    v["source_evidence"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["source_id"].as_str().unwrap().into())
        .collect()
}

#[test]
fn canonical_hints_and_old_renderer_require_explicit_rebuild() {
    let db = Db::new();
    let mut store = db.open("alice");
    store.set_fact_index_enabled(true);
    insert(
        &mut store,
        "canonical",
        "literal fallback",
        "literal fallback",
        "semanticbridge",
        None,
    );
    store
        .transaction(|state| {
            state.entities.upsert(Entity {
                id: EntityId::from("canonical"),
                owner_id: UserId::from("alice"),
                entity_type: EntityType::Person,
                canonical_name: "本人".into(),
                aliases: vec!["乙".into(), "甲".into()],
            });
            Ok(())
        })
        .unwrap();
    let encoded: String = rusqlite::Connection::open(&db.0)
        .unwrap()
        .query_row(
            "SELECT hints_json FROM fact_indexes WHERE owner_id='alice'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let hints: Value = serde_json::from_str(&encoded).unwrap();
    assert_eq!(
        hints[0]["text"],
        r#"{"polarity":null,"predicate":"normalized_key","scope":{"conditions":[],"domain":null},"status":"Active","strength":null,"subject":{"aliases":["乙","甲"],"canonical_name":"本人","entity_type":"Person","id":"canonical","owner_id":"alice"},"subject_id":"canonical","supersede_reason":null,"temporal":{"expires_at":null,"observed_at":100,"reference":null,"valid_from":null,"valid_to":null},"type":"Fact","value":{"Text":"semanticbridge"},"value_entity":null}"#
    );
    let business_before = serde_json::to_value(store.read().unwrap()).unwrap();
    drop(store);
    db.sql("UPDATE fact_indexes SET manifest_json=json_set(manifest_json,'$.binding.renderer_version','committed-fact-key-v1') WHERE owner_id='alice';");
    let before = std::fs::read(&db.0).unwrap();
    let mut ro = SqliteMemoryStore::open_read_only(&db.0, UserId::from("alice")).unwrap();
    ro.set_fact_index_enabled(true);
    let fallback = recall(&ro, "literal", None);
    assert_eq!(ids(&fallback), ["canonical"]);
    assert_eq!(fallback["fact_index"]["status"], "unavailable");
    assert_eq!(
        fallback["fact_index"]["reason"],
        "fact_index_version_mismatch"
    );
    assert!(ids(&recall(&ro, "semanticbridge", None)).is_empty());
    drop(ro);
    assert_eq!(before, std::fs::read(&db.0).unwrap());
    let mut store = db.open("alice");
    store.set_fact_index_enabled(true);
    assert_eq!(
        store.rebuild_fact_index().unwrap()["renderer_version"],
        "committed-fact-key-v2"
    );
    let restored = recall(&store, "semanticbridge", None);
    assert_eq!(restored["fact_index"]["status"], "ready");
    assert_eq!(ids(&restored), ["canonical"]);
    assert_eq!(
        business_before,
        serde_json::to_value(store.read().unwrap()).unwrap()
    );
}

#[test]
fn hints_expand_keys_keep_raw_values_and_respect_owner_task_and_rule_quota() {
    let db = Db::new();
    let mut store = db.open("alice");
    store.set_fact_index_enabled(true);
    insert(
        &mut store,
        "alice-good",
        "a literal statement",
        "a literal statement",
        "semanticbridge",
        Some("task-a"),
    );
    let excluded = insert(
        &mut store,
        "alice-excluded",
        "newer unrelated text",
        "newer unrelated text",
        "semanticbridge",
        Some("task-a"),
    );
    store
        .transaction(|s| {
            let mut m = s.repository.get(excluded)?.unwrap();
            m.status = MemoryStatus::Expired;
            s.repository.update(m)?;
            Ok(())
        })
        .unwrap();
    insert(
        &mut store,
        "alice-other-task",
        "newest foreign task",
        "newest foreign task",
        "semanticbridge",
        Some("task-b"),
    );
    let mut other = db.open("bob");
    other.set_fact_index_enabled(true);
    insert(
        &mut other,
        "bob-source",
        "private text",
        "private text",
        "semanticbridge",
        Some("task-a"),
    );
    store.set_fact_index_enabled(false);
    assert!(ids(&recall(&store, "semanticbridge", Some("task-a"))).is_empty());
    store.set_fact_index_enabled(true);
    let v = recall(&store, "semanticbridge", Some("task-a"));
    assert_eq!(ids(&v), ["alice-good"]);
    assert_eq!(v["fact_index"]["status"], "ready");
    assert_eq!(v["source_evidence"][0]["text"], "a literal statement");
    assert_eq!(
        v["source_evidence"][0]["fact_hint_matches"][0]["provenance"],
        "committed_memory_projection"
    );
    assert_eq!(v["backend_generation_calls_in_recall"], 0);
    assert!(
        store.search_sources("semanticbridge", None, 10).unwrap()["candidates"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn refresh_failure_corruption_and_new_versions_fall_back_until_explicit_rebuild() {
    let db = Db::new();
    let mut store = db.open("alice");
    store.set_fact_index_enabled(true);
    let id = insert(
        &mut store,
        "old",
        "literal fallback",
        "literal fallback",
        "beforeword",
        None,
    );
    db.sql("CREATE TRIGGER fail_fact_refresh BEFORE UPDATE ON fact_indexes BEGIN SELECT RAISE(ABORT,'fixture'); END;");
    store
        .transaction(|s| {
            let mut m = s.repository.get(id)?.unwrap();
            m.value = MemoryValue::from("afterword");
            s.repository.update(m)?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        store
            .read()
            .unwrap()
            .repository
            .get(id)
            .unwrap()
            .unwrap()
            .value,
        MemoryValue::from("afterword")
    );
    assert!(ids(&recall(&store, "beforeword", None)).is_empty());
    let raw = recall(&store, "literal", None);
    assert_eq!(ids(&raw), ["old"]);
    assert_eq!(raw["fact_index"]["status"], "unavailable");
    db.sql("DROP TRIGGER fail_fact_refresh;");
    store.rebuild_fact_index().unwrap();
    assert_eq!(ids(&recall(&store, "afterword", None)), ["old"]);
    db.sql("UPDATE fact_indexes SET hints_json='[]' WHERE owner_id='alice';");
    assert_eq!(
        recall(&store, "afterword", None)["fact_index"]["status"],
        "unavailable"
    );
    store.rebuild_fact_index().unwrap();
    store.set_fact_index_enabled(false);
    store
        .transaction(|s| {
            s.sources.get_mut("old").unwrap().enrichment = "clarified".into();
            Ok(())
        })
        .unwrap();
    store.set_fact_index_enabled(true);
    assert_eq!(
        recall(&store, "afterword", None)["fact_index"]["status"],
        "unavailable"
    );
    store.rebuild_fact_index().unwrap();
    store
        .transaction(|s| {
            let mut m = s.repository.get(id)?.unwrap();
            m.status = MemoryStatus::Deleted;
            s.repository.update(m)?;
            Ok(())
        })
        .unwrap();
    assert!(ids(&recall(&store, "afterword", None)).is_empty());
    assert_eq!(ids(&recall(&store, "literal", None)), ["old"]);
}

#[test]
fn exact_quotes_whole_items_and_read_only_no_repair() {
    let db = Db::new();
    let mut store = db.open("alice");
    store.set_fact_index_enabled(true);
    insert(
        &mut store,
        "ambiguous",
        "echo echo",
        "echo",
        "ambiguouskey",
        None,
    );
    insert(
        &mut store,
        "oversized",
        "short original",
        "short original",
        &"oversizedkey ".repeat(500),
        None,
    );
    let crossing = format!(
        "{}{}{}",
        "a".repeat(700),
        "中文".repeat(100),
        "b".repeat(700)
    );
    let quote: String = crossing.chars().skip(500).take(800).collect();
    insert(
        &mut store,
        "crossing",
        &crossing,
        &quote,
        "crossingkey",
        None,
    );
    let report = store.rebuild_fact_index().unwrap();
    assert!(
        report["coverage"]["skipped"]["missing_or_ambiguous_quote"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(
        report["coverage"]["skipped"]["oversized_memory"]
            .as_u64()
            .unwrap()
            > 0
    );
    drop(store);
    let before = std::fs::read(&db.0).unwrap();
    let mut ro = SqliteMemoryStore::open_read_only(&db.0, UserId::from("alice")).unwrap();
    ro.set_fact_index_enabled(true);
    for q in ["ambiguouskey", "oversizedkey", "crossingkey"] {
        assert!(ids(&recall(&ro, q, None)).is_empty());
    }
    assert!(ro.rebuild_fact_index().is_err());
    drop(ro);
    assert_eq!(before, std::fs::read(&db.0).unwrap());
    db.sql("DROP TABLE fact_indexes;");
    let mut ro = SqliteMemoryStore::open_read_only(&db.0, UserId::from("alice")).unwrap();
    ro.set_fact_index_enabled(true);
    assert_eq!(
        recall(&ro, "echo", None)["fact_index"]["status"],
        "unavailable"
    );
    drop(ro);
    db.sql("CREATE TABLE fact_indexes(owner_id TEXT PRIMARY KEY);");
    let mut ro = SqliteMemoryStore::open_read_only(&db.0, UserId::from("alice")).unwrap();
    ro.set_fact_index_enabled(true);
    assert!(
        ro.recall_local(
            "echo",
            &[],
            &RuntimeContext::current(UserId::from("alice"), Timestamp(300)),
            registry(),
            &InMemorySemanticRelationRegistry::default(),
            &LocalRecallBudget::default(),
        )
        .is_err(),
        "a malformed fact index table must still surface a SQLite error"
    );
}

#[test]
fn retention_clock_and_source_deletion_never_resurrect_keys() {
    let db = Db::new();
    let mut store = db.open("alice");
    store.set_fact_index_enabled(true);
    insert(
        &mut store,
        "stable",
        "stable literal",
        "stable literal",
        "sharedhint",
        None,
    );
    let expires = insert(
        &mut store,
        "expires",
        "temporary literal",
        "temporary literal",
        "sharedhint",
        None,
    );
    store
        .transaction(|s| {
            let mut m = s.repository.get(expires)?.unwrap();
            m.temporal.expires_at = Some(Timestamp(250));
            m.retention_tier = RetentionTier::ShortTerm;
            s.repository.update(m)?;
            Ok(())
        })
        .unwrap();
    assert_eq!(ids(&recall(&store, "sharedhint", None)), ["expires"]);
    let late = store
        .recall_local(
            "sharedhint",
            &[],
            &RuntimeContext::current(UserId::from("alice"), Timestamp(300)),
            registry(),
            &InMemorySemanticRelationRegistry::default(),
            &LocalRecallBudget {
                candidate_k: 1,
                source_limit: 1,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(late["fact_index"]["status"], "ready");
    assert_eq!(ids(&late), ["stable"]);
    assert_eq!(
        store
            .read()
            .unwrap()
            .repository
            .get(expires)
            .unwrap()
            .unwrap()
            .status,
        MemoryStatus::Active
    );
    store
        .transaction(|s| {
            s.sources.get_mut("stable").unwrap().request = None;
            Ok(())
        })
        .unwrap();
    store.rebuild_fact_index().unwrap();
    assert!(ids(&recall(&store, "stable", None)).is_empty());
    let state = store.read().unwrap();
    assert!(state.sources["stable"].request.is_none());
    assert!(
        !store.search_sources("stable", None, 10).unwrap()["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["source_id"] == "stable")
    );
}

#[test]
fn correction_and_complete_negative_scoped_rendering() {
    let db = Db::new();
    let mut store = db.open("alice");
    store.set_fact_index_enabled(true);
    let old = insert(
        &mut store,
        "old",
        "first literal",
        "first literal",
        "wrongbridge",
        None,
    );
    store
        .transaction(|s| {
            let mut source = s.sources["old"].clone();
            source.id = "corrected".into();
            source.turn_id = "corrected".into();
            source.received_sequence = 2;
            source.request.as_mut().unwrap()["text"] = json!("corrected literal");
            s.sources.insert("corrected".into(), source);
            s.with_engine(registry(), |e, _| {
                let mut c = CandidateMemory::new(
                    UserId::from("alice"),
                    EntityId::from("old"),
                    MemoryType::Fact,
                    Predicate::from("normalized_key"),
                    MemoryValue::from("rightbridge"),
                    Timestamp(110),
                    Evidence::new(
                        EvidenceId::from("corrected"),
                        SessionId::from("session"),
                        TurnId::from("corrected"),
                        "corrected literal",
                    ),
                );
                c.intent = CandidateIntent::Supersede {
                    target: Some(old),
                    reason: SupersedeReason::Correction,
                };
                assert!(matches!(
                    e.ingest(common::grounded(c))?,
                    IngestOutcome::Applied { .. }
                ));
                Ok(())
            })
        })
        .unwrap();
    assert!(ids(&recall(&store, "wrongbridge", None)).is_empty());
    assert_eq!(ids(&recall(&store, "rightbridge", None)), ["corrected"]);
    store
        .transaction(|s| {
            let mut m = s
                .repository
                .list_by_owner(&UserId::from("alice"))?
                .into_iter()
                .find(|m| m.status == MemoryStatus::Active)
                .unwrap();
            m.polarity = Some(Polarity::Negative);
            m.scope
                .conditions
                .push(ScopeCondition::Activity("business travel only".into()));
            s.repository.update(m)?;
            Ok(())
        })
        .unwrap();
    let encoded: String = rusqlite::Connection::open(&db.0)
        .unwrap()
        .query_row(
            "SELECT hints_json FROM fact_indexes WHERE owner_id='alice'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let hints: Value = serde_json::from_str(&encoded).unwrap();
    let rendered: Value = serde_json::from_str(
        hints
            .as_array()
            .unwrap()
            .iter()
            .find(|h| h["source_id"] == "corrected")
            .unwrap()["text"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(rendered["polarity"], "Negative");
    assert_eq!(
        rendered["scope"]["conditions"][0],
        json!({"Activity":"business travel only"})
    );
    assert_eq!(rendered["subject_id"], "old");
    assert_eq!(rendered["status"], "Active");
}
