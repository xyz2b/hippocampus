mod common;
use common::{FixtureOutcome, LegacyFixture};

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
            s.with_engine(registry(), |e, _| {
                match e.seed_fixture(common::grounded(c))? {
                    FixtureOutcome::Applied {
                        memory_id: Some(id),
                        ..
                    } => Ok(id),
                    o => Err(format!("{o:?}").into()),
                }
            })
        })
        .unwrap()
}

#[test]
fn canonical_hints_and_old_renderer_preserve_read_only_data_until_rebuild() {
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
    drop(ro);
    assert_eq!(before, std::fs::read(&db.0).unwrap());
    let mut store = db.open("alice");
    store.set_fact_index_enabled(true);
    assert_eq!(
        store.rebuild_fact_index().unwrap()["renderer_version"],
        "committed-fact-key-v2"
    );
    assert_eq!(
        business_before,
        serde_json::to_value(store.read().unwrap()).unwrap()
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
    // A broken auxiliary projection cannot roll back the committed authoritative value.
    let before = serde_json::to_value(store.read().unwrap()).unwrap();
    db.sql("DROP TRIGGER fail_fact_refresh;");
    assert_eq!(
        store.rebuild_fact_index().unwrap()["renderer_version"],
        "committed-fact-key-v2"
    );
    assert_eq!(before, serde_json::to_value(store.read().unwrap()).unwrap());
    db.sql("UPDATE fact_indexes SET hints_json='[]' WHERE owner_id='alice';");
    store.rebuild_fact_index().unwrap();
    let encoded: String = rusqlite::Connection::open(&db.0)
        .unwrap()
        .query_row(
            "SELECT hints_json FROM fact_indexes WHERE owner_id='alice'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(encoded.contains("afterword"));
    assert!(!encoded.contains("beforeword"));
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
    assert!(ro.rebuild_fact_index().is_err());
    drop(ro);
    assert_eq!(before, std::fs::read(&db.0).unwrap());
    db.sql("DROP TABLE fact_indexes;");
    // New recall ignores the old auxiliary fact index, even if it is absent.
    let ro = SqliteMemoryStore::open_read_only(&db.0, UserId::from("alice")).unwrap();
    let request = hippocampus::recall::Request {
        binding: hippocampus::contract::RecallRequest {
            request_id: "legacy-index-absent".into(),
            query: "全部记录".into(),
            context: vec![],
            task_id: None,
            at: 200,
        },
        owner: "alice".into(),
        subject_ids: None,
        said_on: "1970-01-01".into(),
        utc_offset_seconds: 0,
        bootstrap: false,
    };
    let plan = hippocampus::r1::not_understood(&hippocampus::r1::Context {
        query: &request.binding.query,
        context: &[],
        entities: &[],
        relations: &[],
        subject_ids: None,
        task_id: None,
        said_on: &request.said_on,
        utc_offset_seconds: 0,
        supported_units: &[],
    })
    .unwrap();
    let assembly = hippocampus::recall::assemble(&ro, &request, &plan, Default::default()).unwrap();
    assert_eq!(assembly.evidence.records.len(), 3);
    assert!(
        assembly
            .evidence
            .records
            .iter()
            .all(|v| matches!(v, hippocampus::contract::RecordView::Legacy(_)))
    );
    drop(ro);
    db.sql("CREATE TABLE fact_indexes(owner_id TEXT PRIMARY KEY);");
    let mut store = db.open("alice");
    assert!(
        store.rebuild_fact_index().is_err(),
        "explicit legacy-index maintenance must surface malformed schema"
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
                    e.seed_fixture(common::grounded(c))?,
                    FixtureOutcome::Applied { .. }
                ));
                Ok(())
            })
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
    let rendered: Vec<Value> = hints
        .as_array()
        .unwrap()
        .iter()
        .map(|h| serde_json::from_str(h["text"].as_str().unwrap()).unwrap())
        .collect();
    let corrected = rendered
        .iter()
        .find(|m| m["value"]["Text"] == "wrongbridge")
        .unwrap();
    assert_eq!(corrected["status"], "Superseded");
    assert_eq!(corrected["supersede_reason"], "Correction");
    assert!(
        rendered
            .iter()
            .any(|m| m["value"]["Text"] == "rightbridge" && m["status"] == "Active")
    );
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
