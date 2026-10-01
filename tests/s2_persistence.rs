mod common;

use hippocampus::*;
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, Barrier},
};

struct TestDb(PathBuf);
impl TestDb {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("pmr-s2-{}", hippocampus::diagnostics::new_id()));
        std::fs::create_dir(&directory).unwrap();
        Self(directory.join("memory.sqlite3"))
    }

    fn open(&self, owner: &str) -> SqliteMemoryStore {
        SqliteMemoryStore::open(&self.0, UserId::from(owner)).unwrap()
    }
}
impl Drop for TestDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.0.parent().unwrap());
    }
}

fn source(turn: &str) -> SourceRecord {
    SourceRecord {
        id: format!("source:{turn}"),
        session_id: "session".into(),
        turn_id: turn.into(),
        observed_at: 100,
        received_sequence: 1,
        request: Some(json!({"text":"我住在上海","capture_source":true})),
        enrichment: "pending".into(),
        error: None,
        result: None,
        base_revision: 0,
        index: Value::Null,
        task: Default::default(),
    }
}

fn ingest(state: &mut PersistedMemoryState) -> PersistenceResult<()> {
    let mut predicates = InMemoryPredicateRegistry::default();
    predicates.register(Predicate::from("lives_in"), Cardinality::Single);
    state.with_engine(predicates, |engine, _| {
        let outcome = engine.ingest(common::grounded(CandidateMemory::new(
            UserId::from("alice"),
            EntityId::from("self"),
            MemoryType::Fact,
            Predicate::from("lives_in"),
            MemoryValue::Text("上海".into()),
            Timestamp(100),
            Evidence::new(
                EvidenceId::from("one"),
                SessionId::from("session"),
                TurnId::from("one"),
                "我住在上海",
            ),
        )))?;
        assert!(matches!(outcome, IngestOutcome::Applied { .. }));
        Ok(())
    })
}

#[test]
fn migration_preserves_old_snapshots_and_is_repeatable() {
    let db = TestDb::new();
    let mut old = serde_json::to_value(PersistedMemoryState::default()).unwrap();
    old.as_object_mut().unwrap().remove("sources");
    old.as_object_mut().unwrap().remove("revision");
    old.as_object_mut()
        .unwrap()
        .remove("last_applied_source_sequence");
    old.as_object_mut()
        .unwrap()
        .remove("source_less_barrier_sequence");
    let connection = Connection::open(&db.0).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE memory_profiles (owner_id TEXT PRIMARY KEY, state_json TEXT NOT NULL);
             PRAGMA user_version = 1;",
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO memory_profiles VALUES (?1, ?2)",
            params!["alice", old.to_string()],
        )
        .unwrap();
    for _ in 0..2 {
        let state = db.open("alice").read().unwrap();
        assert!(state.sources.is_empty());
        assert_eq!(state.revision, 0);
    }
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        6
    );
    let saved: String = connection
        .query_row("SELECT state_json FROM memory_profiles", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(serde_json::from_str::<Value>(&saved).unwrap(), old);
    connection.pragma_update(None, "user_version", 7).unwrap();
    assert!(SqliteMemoryStore::open(&db.0, UserId::from("alice")).is_err());
}

#[test]
fn v4_upgrade_preserves_legacy_pending_without_queueing_it() {
    let db = TestDb::new();
    let mut store = db.open("alice");
    store
        .transaction(|state| {
            state.sources.insert("pending".into(), source("pending"));
            let mut failed = source("failed");
            failed.enrichment = "failed".into();
            failed.error = Some("model_request_failed".into());
            state.sources.insert("failed".into(), failed);
            Ok(())
        })
        .unwrap();
    drop(store);
    let connection = Connection::open(&db.0).unwrap();
    let raw: String = connection
        .query_row("SELECT state_json FROM memory_profiles", [], |row| {
            row.get(0)
        })
        .unwrap();
    let mut old: Value = serde_json::from_str(&raw).unwrap();
    let object = old.as_object_mut().unwrap();
    object.remove("last_applied_source_sequence");
    object.remove("source_less_barrier_sequence");
    for source in old["sources"].as_object_mut().unwrap().values_mut() {
        source.as_object_mut().unwrap().remove("task");
    }
    let old_json = old.to_string();
    connection
        .execute("UPDATE memory_profiles SET state_json=?1", [&old_json])
        .unwrap();
    connection.pragma_update(None, "user_version", 4).unwrap();
    let readonly = SqliteMemoryStore::open_read_only(&db.0, UserId::from("alice")).unwrap();
    let before = readonly.read().unwrap();
    assert_eq!(before.sources["pending"].enrichment, "pending");
    assert_eq!(before.sources["failed"].enrichment, "failed");
    assert_eq!(before.sources["pending"].task, EnrichmentTask::default());
    assert_eq!(before.sources["failed"].task, EnrichmentTask::default());
    assert_eq!(before.last_applied_source_sequence, 0);
    assert_eq!(before.source_less_barrier_sequence, 0);
    drop(readonly);
    for _ in 0..2 {
        let state = db.open("alice").read().unwrap();
        assert_eq!(state.sources["pending"].task, EnrichmentTask::default());
        assert_eq!(state.sources["failed"].task, EnrichmentTask::default());
    }
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        6
    );
    let saved: String = connection
        .query_row("SELECT state_json FROM memory_profiles", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        saved, old_json,
        "migration must not enqueue historical sources"
    );
    SqliteMemoryStore::open_read_only(&db.0, UserId::from("alice")).unwrap();
}

#[test]
fn v4_upgrade_backfills_source_order_for_every_owner() {
    let db = TestDb::new();
    for owner in ["alice", "bob"] {
        let mut store = db.open(owner);
        store
            .transaction(|state| {
                let mut failed = source("failed");
                failed.enrichment = "failed".into();
                state.sources.insert("failed".into(), failed);
                let mut committed = source("committed");
                committed.received_sequence = 2;
                committed.enrichment = "committed".into();
                state.sources.insert("committed".into(), committed);
                Ok(())
            })
            .unwrap();
    }
    let connection = Connection::open(&db.0).unwrap();
    for owner in ["alice", "bob"] {
        let raw: String = connection
            .query_row(
                "SELECT state_json FROM memory_profiles WHERE owner_id=?1",
                [owner],
                |row| row.get(0),
            )
            .unwrap();
        let mut old: Value = serde_json::from_str(&raw).unwrap();
        old.as_object_mut()
            .unwrap()
            .remove("last_applied_source_sequence");
        old.as_object_mut()
            .unwrap()
            .remove("source_less_barrier_sequence");
        for source in old["sources"].as_object_mut().unwrap().values_mut() {
            source.as_object_mut().unwrap().remove("task");
        }
        connection
            .execute(
                "UPDATE memory_profiles SET state_json=?2 WHERE owner_id=?1",
                params![owner, old.to_string()],
            )
            .unwrap();
    }
    connection.pragma_update(None, "user_version", 4).unwrap();
    assert_eq!(
        db.open("alice")
            .read()
            .unwrap()
            .last_applied_source_sequence,
        2
    );
    assert_eq!(
        db.open("bob").read().unwrap().last_applied_source_sequence,
        2
    );
    assert_eq!(
        db.open("alice").read().unwrap().sources["failed"].enrichment,
        "failed"
    );
    assert_eq!(
        db.open("bob").read().unwrap().sources["failed"].task,
        EnrichmentTask::default()
    );
}

#[test]
fn s5_migration_keeps_v3_sources_facts_and_index_read_only_until_opened_for_write() {
    let db = TestDb::new();
    let mut store = db.open("alice");
    store
        .transaction(|state| {
            state.sources.insert("one".into(), source("one"));
            ingest(state)
        })
        .unwrap();
    let before = serde_json::to_value(store.read().unwrap()).unwrap();
    drop(store);
    let connection = Connection::open(&db.0).unwrap();
    // The S4 schema had the same business/source tables, without the S5 call ledger.
    connection
        .execute_batch(
            "DROP TABLE model_calls; DROP TABLE model_budget_groups; PRAGMA user_version=3;",
        )
        .unwrap();
    let readonly = SqliteMemoryStore::open_read_only(&db.0, UserId::from("alice")).unwrap();
    assert_eq!(
        serde_json::to_value(readonly.read().unwrap()).unwrap(),
        before
    );
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        3
    );
    drop(readonly);
    for _ in 0..2 {
        assert_eq!(
            serde_json::to_value(db.open("alice").read().unwrap()).unwrap(),
            before
        );
    }
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        6
    );
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM model_calls", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM source_segments", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn received_source_survives_failed_fact_transaction_and_restart() {
    let db = TestDb::new();
    let mut store = db.open("alice");
    store
        .transaction(|state| {
            state.sources.insert("one".into(), source("one"));
            Ok(())
        })
        .unwrap();
    let received = serde_json::to_value(store.read().unwrap()).unwrap();
    let failed: PersistenceResult<()> = store.transaction(|state| {
        ingest(state)?;
        let record = state.sources.get_mut("one").unwrap();
        record.enrichment = "completed".into();
        record.result = Some(json!({"outcomes":[]}));
        Err("injected failure before fact commit".into())
    });
    assert!(failed.is_err());
    assert_eq!(
        serde_json::to_value(db.open("alice").read().unwrap()).unwrap(),
        received
    );
    store
        .transaction(|state| {
            ingest(state)?;
            let record = state.sources.get_mut("one").unwrap();
            record.enrichment = "completed".into();
            record.result = Some(json!({"outcomes":["saved"]}));
            Ok(())
        })
        .unwrap();
    let committed = db.open("alice").read().unwrap();
    assert_eq!(committed.revision, 1);
    assert_eq!(committed.sources["one"].enrichment, "completed");
    assert_eq!(
        committed.sources["one"].result,
        Some(json!({"outcomes":["saved"]}))
    );
    assert_eq!(
        committed
            .repository
            .list_by_owner(&UserId::from("alice"))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn source_and_receipt_changes_do_not_advance_business_revision() {
    let db = TestDb::new();
    let mut store = db.open("alice");
    store.transaction(ingest).unwrap();
    store
        .transaction(|state| {
            state.sources.insert("one".into(), source("one"));
            let mut snapshot = serde_json::to_value(&*state)?;
            snapshot["receipts"]["one"] = json!({"request":{},"result":{}});
            *state = serde_json::from_value(snapshot)?;
            Ok(())
        })
        .unwrap();
    store.transaction(|_| Ok(())).unwrap();
    assert_eq!(store.read().unwrap().revision, 1);
    let status = store.read().unwrap().sources["one"].status();
    assert_eq!(status["source"]["status"], "stored");
    assert_eq!(status["index"]["status"], "ready");
    store
        .transaction(|state| {
            state.sources.get_mut("one").unwrap().request = None;
            Ok(())
        })
        .unwrap();
    let deleted = store.read().unwrap();
    assert_eq!(deleted.revision, 1);
    assert_eq!(
        deleted.sources["one"].status()["source"]["status"],
        "deleted"
    );
}

#[test]
fn concurrent_source_commits_preserve_each_other_and_isolate_owners() {
    let db = TestDb::new();
    drop(db.open("alice"));
    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = (0..2)
        .map(|worker| {
            let path = db.0.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let mut store = SqliteMemoryStore::open(path, UserId::from("alice")).unwrap();
                barrier.wait();
                for turn in 0..4 {
                    let key = format!("{worker}:{turn}");
                    store
                        .transaction(|state| {
                            state.sources.insert(key.clone(), source(&key));
                            Ok(())
                        })
                        .unwrap();
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }
    let alice = db.open("alice").read().unwrap();
    assert_eq!(alice.sources.len(), 8);
    assert_eq!(alice.revision, 0);
    let mut bob = db.open("bob");
    assert!(bob.read().unwrap().sources.is_empty());
    bob.transaction(|state| {
        state.sources.insert("0:0".into(), source("bob"));
        Ok(())
    })
    .unwrap();
    assert_eq!(
        db.open("alice").read().unwrap().sources["0:0"].turn_id,
        "0:0"
    );
    assert_eq!(bob.read().unwrap().sources["0:0"].turn_id, "bob");
}

#[test]
fn revision_overflow_rolls_back_all_changes() {
    let db = TestDb::new();
    let mut store = db.open("alice");
    let mut state = store.read().unwrap();
    state.revision = u64::MAX;
    let before = serde_json::to_value(state).unwrap();
    Connection::open(&db.0)
        .unwrap()
        .execute(
            "INSERT INTO memory_profiles VALUES (?1, ?2)",
            params!["alice", before.to_string()],
        )
        .unwrap();
    let failed = store.transaction(|state| {
        state.sources.insert("one".into(), source("one"));
        ingest(state)
    });
    assert!(
        failed
            .unwrap_err()
            .to_string()
            .contains("revision overflow")
    );
    assert_eq!(serde_json::to_value(store.read().unwrap()).unwrap(), before);
}

#[test]
fn disabled_capture_and_cancellation_do_not_store_or_start_model_processing() {
    use hippocampus::chat::{ChatConfig, ChatMemoryService};

    let db = TestDb::new();
    let config = ChatConfig {
        source_capture_enabled: false,
        ..ChatConfig::default()
    };
    let mut service = ChatMemoryService::new(db.open("alice"), config).unwrap();
    let before = serde_json::to_value(db.open("alice").read().unwrap()).unwrap();
    let turn =
        json!({"text":"我住在上海","session_id":"chat","turn_id":"one","capture_source":true});
    let error = service
        .call("memory_remember", turn.clone(), || false)
        .unwrap_err();
    assert_eq!(error.to_string(), "source capture is disabled");
    let prepared = service
        .call("memory_prepare", json!({"user_turn":turn}), || false)
        .unwrap();
    assert_eq!(prepared["remember"]["source"]["status"], "rejected");
    assert_eq!(
        prepared["remember"]["enrichment"]["status"],
        "not_requested"
    );
    assert_eq!(prepared["recall"]["status"], "not_requested");
    assert_eq!(
        serde_json::to_value(db.open("alice").read().unwrap()).unwrap(),
        before
    );

    let mut service = ChatMemoryService::new(db.open("alice"), ChatConfig::default()).unwrap();
    for (tool, arguments) in [
        ("memory_remember", turn.clone()),
        ("memory_prepare", json!({"user_turn":turn})),
    ] {
        let error = service.call(tool, arguments, || true).unwrap_err();
        assert_eq!(error.to_string(), "request cancelled before commit");
        assert_eq!(
            serde_json::to_value(db.open("alice").read().unwrap()).unwrap(),
            before
        );
    }
}

#[test]
fn capture_storage_read_failure_stops_prepare_before_models() {
    use hippocampus::chat::{ChatConfig, ChatMemoryService};

    let db = TestDb::new();
    let mut service = ChatMemoryService::new(db.open("alice"), ChatConfig::default()).unwrap();
    Connection::open(&db.0)
        .unwrap()
        .execute_batch("DROP TABLE memory_profiles")
        .unwrap();
    let result = service
        .call(
            "memory_prepare",
            json!({"user_turn":{"text":"我住在上海","session_id":"chat","turn_id":"one","capture_source":true}}),
            || false,
        )
        .unwrap();
    assert_eq!(result["remember"]["status"], "failed");
    assert_eq!(result["remember"]["source"]["status"], "failed");
    assert_eq!(result["remember"]["enrichment"]["status"], "not_requested");
    assert_eq!(result["recall"]["status"], "not_requested");
}

#[test]
fn completed_prepare_cache_checks_source_deletion_without_revision_change() {
    use hippocampus::chat::{ChatConfig, ChatMemoryService};
    use rmcp::model::CallToolResult;

    let db = TestDb::new();
    let mut service = ChatMemoryService::new(db.open("alice"), ChatConfig::default()).unwrap();
    let arguments = json!({"user_turn":{"text":"原文不产生事实","session_id":"session","turn_id":"one","capture_source":true}});
    let key = serde_json::to_string(&("memory_remember", "session", "one")).unwrap();
    let mut record = source("one");
    record.request = Some(arguments["user_turn"].clone());
    record.enrichment = "completed_no_facts".into();
    record.result = Some(json!({"outcomes":[]}));
    db.open("alice")
        .transaction(|state| {
            state.sources.insert(key.clone(), record.clone());
            Ok(())
        })
        .unwrap();
    let cached = CallToolResult::structured(json!({
        "remember":{"status":"completed","result":record.status()},
        "recall":{"status":"completed","result":{"facts":[]}},
        "global_context":{"status":"available","result":{"facts":[]}}
    }));
    assert_eq!(
        service
            .validate_prepare_response(&arguments, cached.clone())
            .structured_content,
        cached.structured_content
    );
    let before_revision = db.open("alice").read().unwrap().revision;
    service
        .call("memory_forget", json!({"source_id":record.id}), || false)
        .unwrap();
    let state = db.open("alice").read().unwrap();
    assert_eq!(state.revision, before_revision);
    assert!(state.sources[&key].request.is_none());
    let refreshed = service
        .validate_prepare_response(&arguments, cached)
        .structured_content
        .unwrap();
    assert_eq!(refreshed["remember"]["status"], "failed");
    assert_eq!(refreshed["remember"]["source"]["status"], "deleted");
    assert_eq!(refreshed["recall"]["status"], "not_requested");
    assert_eq!(refreshed["global_context"]["status"], "available");
}

#[test]
fn completed_prepare_cache_removes_deleted_recall_and_global_context() {
    use hippocampus::chat::{ChatConfig, ChatMemoryService};
    use rmcp::model::CallToolResult;

    let db = TestDb::new();
    db.open("alice").transaction(ingest).unwrap();
    let mut service = ChatMemoryService::new(db.open("alice"), ChatConfig::default()).unwrap();
    let memory = db
        .open("alice")
        .read()
        .unwrap()
        .repository
        .list_by_owner(&UserId::from("alice"))
        .unwrap()
        .remove(0);
    let arguments =
        json!({"user_turn":{"text":"我住在哪里？","session_id":"other-session","turn_id":"query"}});
    let cached = CallToolResult::structured(json!({
        "remember":{"status":"completed","result":{"outcomes":[]}},
        "recall":{"status":"completed","result":{"facts":[memory]}},
        "global_context":{"status":"available","result":{"facts":[memory]}}
    }));
    assert_eq!(
        service
            .validate_prepare_response(&arguments, cached.clone())
            .structured_content,
        cached.structured_content
    );
    service
        .call("memory_forget", json!({"memory_id":memory.id}), || false)
        .unwrap();
    let refreshed = service.validate_prepare_response(&arguments, cached);
    let body = refreshed.structured_content.as_ref().unwrap();
    assert_eq!(body["remember"]["status"], "completed");
    assert_eq!(body["recall"]["status"], "not_requested");
    assert_eq!(body["global_context"]["status"], "available");
    // Both structured and text content must be rebuilt; the old evidence cannot survive in either.
    assert!(!serde_json::to_string(&refreshed).unwrap().contains("上海"));
}
