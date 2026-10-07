mod common;
use common::{FixtureOutcome, LegacyFixture};

use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};
static TEST_ID: AtomicU64 = AtomicU64::new(0);
use hippocampus::{
    chat::{ChatConfig, ChatMemoryService},
    *,
};
use serde_json::json;

struct TestDb(PathBuf);
impl TestDb {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "pmr-persistence-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            TEST_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        Self(directory.join("private/memory.sqlite3"))
    }
    fn open(&self, owner: &str) -> SqliteMemoryStore {
        SqliteMemoryStore::open(&self.0, UserId::from(owner)).unwrap()
    }
}
impl Drop for TestDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.0.parent().unwrap().parent().unwrap());
    }
}

fn registry() -> InMemoryPredicateRegistry {
    let mut registry = InMemoryPredicateRegistry::default();
    registry.register(Predicate::from("lives_in"), Cardinality::Single);
    registry
}

fn candidate(turn: &str, value: &str) -> CandidateMemory {
    CandidateMemory::new(
        UserId::from("alice"),
        EntityId::from("self"),
        MemoryType::Fact,
        Predicate::from("lives_in"),
        MemoryValue::Text(value.into()),
        Timestamp(100),
        Evidence::new(
            EvidenceId::from(turn),
            SessionId::from("chat"),
            TurnId::from(turn),
            format!("我住在{value}"),
        ),
    )
}

fn insert(store: &mut SqliteMemoryStore, candidate: CandidateMemory) -> MemoryId {
    store
        .transaction(|state| {
            state.with_engine(registry(), |engine, _| {
                match engine.seed_fixture(common::grounded(candidate))? {
                    FixtureOutcome::Applied {
                        memory_id: Some(id),
                        ..
                    } => Ok(id),
                    other => panic!("unexpected {other:?}"),
                }
            })
        })
        .unwrap()
}

#[test]
fn memory_list_is_a_management_directory_not_a_recall_fallback() {
    let db = TestDb::new();
    let saved = insert(&mut db.open("alice"), candidate("E2", "一台机器"));
    let mut pending = candidate("pending", "私密值");
    pending.subject = CandidateSubject::Unresolved("孩子".into());
    common::stage_fixture(&db.open("alice"), pending);

    let mut service = ChatMemoryService::new(db.open("alice"), ChatConfig::default()).unwrap();
    let listing = service
        .call("memory_list", json!({"limit":30}), || false)
        .unwrap();
    assert_eq!(listing["total"], 1);
    assert_eq!(
        listing["memories"],
        json!([{"id":saved.0,"status":"Active"}])
    );
    assert_eq!(listing["pending_total"], 1);
    assert_eq!(listing["pending"], json!([{"id":1,"status":"Pending"}]));
    assert!(!listing.to_string().contains("一台机器"), "{listing}");
    assert!(!listing.to_string().contains("私密值"), "{listing}");
    assert!(!listing.to_string().contains("我住在"), "{listing}");
    assert!(
        service
            .call("memory_list", json!({"include_details":true}), || false)
            .is_err()
    );
}

#[test]
fn restart_preserves_versions_evidence_ids_and_retrieval_counters() {
    let db = TestDb::new();
    let first = insert(&mut db.open("alice"), candidate("one", "上海"));
    let mut changed = candidate("two", "北京");
    changed.intent = CandidateIntent::Supersede {
        target: Some(first),
        reason: SupersedeReason::StateChanged,
    };
    let second = insert(&mut db.open("alice"), changed);
    assert!(second.0 > first.0);
    let mut store = db.open("alice");
    let state = store.read().unwrap();
    let old = state.repository.get(first).unwrap().unwrap();
    let current = state.repository.get(second).unwrap().unwrap();
    assert_eq!(old.status, MemoryStatus::Superseded);
    assert_eq!(old.superseded_by, Some(second));
    assert_eq!(current.supersedes, Some(first));
    assert_eq!(current.evidence[0].quote, "我住在北京");
    store
        .transaction(|state| {
            let mut memory = state.repository.get(second)?.unwrap();
            memory.retrieval_count = 1;
            memory.last_retrieved_at = Some(Timestamp(101));
            state.repository.update(memory)?;
            Ok(())
        })
        .unwrap();
    let mut service = ChatMemoryService::new(store, ChatConfig::default()).unwrap();
    let recalled = service
        .call(
            "memory_recall",
            json!({"query":"我住在哪？", "request_id":"legacy-reopen", "offline":true, "at":101}),
            || false,
        )
        .unwrap();
    assert!(
        recalled["records"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == second.0),
        "{recalled}"
    );
    let store = db.open("alice");
    drop(store);
    assert_eq!(
        db.open("alice")
            .read()
            .unwrap()
            .repository
            .get(second)
            .unwrap()
            .unwrap()
            .retrieval_count,
        1
    );
}

#[test]
fn bootstrap_uses_local_store_and_filters_unrelated_scoped_and_expired_memories() {
    let db = TestDb::new();
    let mut store = db.open("alice");
    let mut language = candidate("language", "中文");
    language.predicate = Predicate::from("response_language");
    language.memory_type = MemoryType::Constraint;
    language.retention_tier = RetentionTier::Durable;
    let mut style = candidate("style", "先给结论");
    style.predicate = Predicate::from("response_style");
    style.memory_type = MemoryType::Preference;
    let mut scoped = style.clone();
    scoped.evidence[0].id = EvidenceId::from("scoped");
    scoped.value = MemoryValue::Text("早餐专用".into());
    scoped.scope.conditions = vec![ScopeCondition::Activity("breakfast".into())];
    let mut expired = style.clone();
    expired.evidence[0].id = EvidenceId::from("expired");
    expired.value = MemoryValue::Text("已过期".into());
    expired.temporal.expires_at = Some(Timestamp(101));
    store
        .transaction(|state| {
            let mut registry = registry();
            registry.register(Predicate::from("response_language"), Cardinality::Single);
            registry.register(Predicate::from("response_style"), Cardinality::Multiple);
            state.with_engine(registry, |engine, _| {
                for value in [language, style, scoped, expired, candidate("home", "上海")] {
                    assert!(matches!(
                        engine.seed_fixture(common::grounded(value))?,
                        FixtureOutcome::Applied { .. }
                    ));
                }
                Ok(())
            })
        })
        .unwrap();
    let mut service = ChatMemoryService::new(
        store,
        ChatConfig {
            ..Default::default()
        },
    )
    .unwrap();
    let result = service
        .call_with_global_context(
            "memory_recall",
            json!({"query":"全局回答要求", "bootstrap":true, "request_id":"bootstrap"}),
            || false,
        )
        .structured_content
        .unwrap();
    assert_eq!(
        result["global_context"]["result"]["mandatory"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        result["global_context"]["result"]["preferences"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        result["global_context"]["result"]["preferences"][0]["value"],
        json!({"Text":"先给结论"})
    );
    assert_eq!(result["global_context"]["result"]["facts"], json!([]));
    service.call("memory_list", json!({}), || false).unwrap();
    assert!(
        db.open("alice")
            .read()
            .unwrap()
            .repository
            .list_by_owner(&UserId::from("alice"))
            .unwrap()
            .iter()
            .any(|m| m.status == MemoryStatus::Expired)
    );
}

#[test]
fn failed_batch_rolls_back_supersession_and_allocated_ids() {
    let db = TestDb::new();
    let mut store = db.open("alice");
    let first = insert(&mut store, candidate("one", "上海"));
    let before = serde_json::to_value(store.read().unwrap()).unwrap();
    let mut replacement = candidate("two", "北京");
    replacement.intent = CandidateIntent::Supersede {
        target: Some(first),
        reason: SupersedeReason::Correction,
    };
    let result: PersistenceResult<()> = store.transaction(|state| {
        state.with_engine(registry(), |engine, _| {
            engine.seed_fixture(common::grounded(replacement))?;
            // A later candidate or storage operation fails after ending the old version and inserting the new one.
            Err("injected failure before commit".into())
        })
    });
    assert!(result.is_err());
    assert_eq!(
        serde_json::to_value(db.open("alice").read().unwrap()).unwrap(),
        before
    );
    assert_eq!(
        store
            .transaction(|state| Ok(state.repository.allocate_id()?))
            .unwrap()
            .0,
        first.0 + 1
    );
}

#[test]
fn concurrent_connections_serialize_updates_and_isolate_owners() {
    let db = TestDb::new();
    drop(db.open("alice"));
    std::thread::scope(|scope| {
        for _ in 0..4 {
            let path = &db.0;
            scope.spawn(move || {
                let mut store = SqliteMemoryStore::open(path, UserId::from("alice")).unwrap();
                for _ in 0..10 {
                    store
                        .transaction(|state| Ok(state.repository.allocate_id()?))
                        .unwrap();
                }
            });
        }
    });
    assert_eq!(
        db.open("alice")
            .transaction(|state| Ok(state.repository.allocate_id()?))
            .unwrap(),
        MemoryId(41)
    );
    assert_eq!(
        db.open("bob")
            .transaction(|state| Ok(state.repository.allocate_id()?))
            .unwrap(),
        MemoryId(1)
    );
    insert(&mut db.open("alice"), candidate("one", "上海"));
    assert!(
        db.open("bob")
            .read()
            .unwrap()
            .repository
            .list_by_owner(&UserId::from("alice"))
            .unwrap()
            .is_empty()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&db.0).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(db.0.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
}

#[test]
fn pending_entities_survive_restart_and_clarification_is_unsupported() {
    let db = TestDb::new();
    let mut candidate = candidate("one", "上海");
    candidate.subject = CandidateSubject::Unresolved("孩子".into());
    common::stage_fixture(&db.open("alice"), candidate);
    let mut service = ChatMemoryService::new(db.open("alice"), ChatConfig::default()).unwrap();
    service.call("memory_entity_upsert", json!({"id":"child", "canonical_name":"小明", "aliases":["孩子"], "entity_type":"Person"}), || false).unwrap();
    drop(service);
    let mut service = ChatMemoryService::new(db.open("alice"), ChatConfig::default()).unwrap();
    let request = json!({"staged_id":1,"entity_reference":"小明", "text":"孩子指的是小明", "session_id":"chat","turn_id":"two"});
    let before = serde_json::to_value(db.open("alice").read().unwrap()).unwrap();
    assert!(
        service
            .call("memory_clarify", request.clone(), || false)
            .unwrap_err()
            .to_string()
            .contains("no longer supported")
    );
    drop(service);
    let mut service = ChatMemoryService::new(db.open("alice"), ChatConfig::default()).unwrap();
    assert!(service.call("memory_clarify", request, || false).is_err());
    let state = db.open("alice").read().unwrap();
    assert_eq!(serde_json::to_value(&state).unwrap(), before);
    assert_eq!(state.staging.list_by_owner(&UserId::from("alice")).len(), 1);
    assert!(
        state
            .repository
            .list_by_owner(&UserId::from("alice"))
            .unwrap()
            .is_empty()
    );
    service
        .call("memory_forget", json!({"staged_id":1}), || false)
        .unwrap();
    assert!(db.open("alice").read().unwrap().staging.is_empty());
}

#[test]
fn only_trusted_database_load_restores_matching_pending_source_support() {
    let db = TestDb::new();
    let mut pending = candidate("one", "上海");
    pending.subject = CandidateSubject::Unresolved("孩子".into());
    common::stage_fixture(&db.open("alice"), pending);
    let restored = db.open("alice").read().unwrap();
    assert!(
        restored
            .staging
            .get(StagedCandidateId(1))
            .unwrap()
            .candidate
            .source_support_valid()
    );
    let decoded: PersistedMemoryState =
        serde_json::from_value(serde_json::to_value(&restored).unwrap()).unwrap();
    assert!(
        !decoded
            .staging
            .get(StagedCandidateId(1))
            .unwrap()
            .candidate
            .source_support_valid()
    );

    let connection = rusqlite::Connection::open(&db.0).unwrap();
    let stored: String = connection
        .query_row(
            "SELECT state_json FROM memory_profiles WHERE owner_id='alice'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    for variant in ["missing", "changed", "old_unversioned"] {
        let mut snapshot: serde_json::Value = serde_json::from_str(&stored).unwrap();
        let candidate = &mut snapshot["staging"]["pending"]["1"]["candidate"];
        match variant {
            "changed" => candidate["value"] = json!({"Text":"北京"}),
            "old_unversioned" => {
                let mut old_proof = candidate.clone();
                old_proof.as_object_mut().unwrap().remove("source_support");
                candidate["source_support"] = old_proof;
            }
            _ => {
                candidate.as_object_mut().unwrap().remove("source_support");
            }
        }
        connection
            .execute(
                "UPDATE memory_profiles SET state_json=?1 WHERE owner_id='alice'",
                [snapshot.to_string()],
            )
            .unwrap();
        let state = SqliteMemoryStore::open_read_only(&db.0, UserId::from("alice"))
            .unwrap()
            .read()
            .unwrap();
        assert!(
            !state
                .staging
                .get(StagedCandidateId(1))
                .unwrap()
                .candidate
                .source_support_valid(),
            "variant={variant}"
        );
    }
}

#[test]
fn unsupported_clarification_preserves_each_pending_fact_for_deletion() {
    let db = TestDb::new();
    for (turn, predicate, value) in [("one", "lives_in", "上海"), ("two", "owns", "书包")] {
        let mut pending = candidate(turn, value);
        pending.predicate = Predicate::from(predicate);
        pending.subject = CandidateSubject::Unresolved("孩子".into());
        common::stage_fixture(&db.open("alice"), pending);
    }
    let mut service = ChatMemoryService::new(db.open("alice"), ChatConfig::default()).unwrap();
    service
        .call(
            "memory_entity_upsert",
            json!({"id":"child", "canonical_name":"小明", "entity_type":"Person"}),
            || false,
        )
        .unwrap();
    let before = serde_json::to_value(db.open("alice").read().unwrap()).unwrap();
    for id in [1, 2] {
        assert!(service.call("memory_clarify", json!({"staged_id":id,"entity_reference":"小明", "text":"孩子指的是小明", "session_id":"chat","turn_id":"three"}), || false).unwrap_err().to_string().contains("no longer supported"));
    }
    assert_eq!(
        serde_json::to_value(db.open("alice").read().unwrap()).unwrap(),
        before
    );
    service
        .call("memory_forget", json!({"staged_id":1}), || false)
        .unwrap();
    assert!(
        db.open("alice")
            .read()
            .unwrap()
            .staging
            .get(StagedCandidateId(2))
            .is_some()
    );
    service
        .call("memory_forget", json!({"staged_id":2}), || false)
        .unwrap();
    assert!(db.open("alice").read().unwrap().staging.is_empty());
}

#[test]
fn forget_hides_the_entire_chain_and_invalid_or_cancelled_calls_do_not_write() {
    let db = TestDb::new();
    let first = insert(&mut db.open("alice"), candidate("one", "上海"));
    let mut changed = candidate("two", "北京");
    changed.intent = CandidateIntent::Supersede {
        target: Some(first),
        reason: SupersedeReason::StateChanged,
    };
    let second = insert(&mut db.open("alice"), changed);
    let mut service = ChatMemoryService::new(db.open("alice"), ChatConfig::default()).unwrap();
    assert!(
        service
            .call("memory_forget", json!({"memory_id":second.0}), || true)
            .is_err()
    );
    assert!(
        service
            .call("memory_list", json!({"owner_id":"bob"}), || false)
            .is_err()
    );
    assert!(
        service
            .call("memory_list", json!({"limit":0}), || false)
            .is_err()
    );
    let result = service
        .call("memory_forget", json!({"memory_id":second.0}), || false)
        .unwrap();
    assert_eq!(result["forgotten_ids"], json!([first.0, second.0]));
    drop(service);
    let mut service = ChatMemoryService::new(db.open("alice"), ChatConfig::default()).unwrap();
    assert_eq!(
        service
            .call("memory_list", json!({"include_history":true}), || false)
            .unwrap()["total"],
        0
    );
    for memory in db
        .open("alice")
        .read()
        .unwrap()
        .repository
        .list_by_owner(&UserId::from("alice"))
        .unwrap()
    {
        assert_eq!(memory.status, MemoryStatus::Deleted);
    }
}

#[test]
fn corruption_and_newer_database_versions_fail_without_reset() {
    let db = TestDb::new();
    insert(&mut db.open("alice"), candidate("one", "上海"));
    let connection = rusqlite::Connection::open(&db.0).unwrap();
    connection
        .execute("UPDATE memory_profiles SET state_json = 'invalid-json'", [])
        .unwrap();
    assert!(SqliteMemoryStore::open(&db.0, UserId::from("alice")).is_err());
    let data: String = connection
        .query_row("SELECT state_json FROM memory_profiles", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(data, "invalid-json");
    connection.pragma_update(None, "user_version", 999).unwrap();
    assert!(
        SqliteMemoryStore::open(&db.0, UserId::from("bob"))
            .err()
            .unwrap()
            .to_string()
            .contains("version")
    );
}

#[test]
fn bundled_sqlite_supports_fts5_without_touching_profiles() {
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE VIRTUAL TABLE probe USING fts5(text, tokenize='trigram');
         INSERT INTO probe VALUES ('独立原文检索');",
        )
        .unwrap();
    let matches: i64 = connection
        .query_row(
            "SELECT count(*) FROM probe WHERE probe MATCH '原文检'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(matches, 1);
    println!(
        "sqlite_version={}; fts5=true; trigram=true; schema_version=1 (src/sqlite.rs)",
        rusqlite::version()
    );
}
