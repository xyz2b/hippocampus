mod common;
use common::{FixtureOutcome, LegacyFixture};

use hippocampus::*;
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, Barrier},
    time::Instant,
};

struct TestDb(PathBuf);

impl TestDb {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("pmr-s3-{}", hippocampus::diagnostics::new_id()));
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

fn source(turn: &str, text: &str, task: Option<&str>) -> SourceRecord {
    serde_json::from_value(json!({
        "id":format!("source:{turn}"), "session_id":"session", "turn_id":turn,
        "observed_at":100, "received_sequence":1, "base_revision":0,
        "request":{"text":text,"task_id":task,"capture_source":true,
            "session_id":"session","turn_id":turn,"history":["不能检索的历史副本"]},
        "enrichment":"failed", "error":"model_unavailable", "result":null
    }))
    .unwrap()
}

fn insert(store: &mut SqliteMemoryStore, turn: &str, text: &str, task: Option<&str>) {
    store
        .transaction(|state| {
            state.sources.insert(turn.into(), source(turn, text, task));
            Ok(())
        })
        .unwrap();
}

fn candidates(result: &Value) -> &[Value] {
    let candidates = result["candidates"].as_array().unwrap();
    assert_eq!(result["candidate_count"], candidates.len());
    candidates
}

fn search(store: &SqliteMemoryStore, query: &str) -> Value {
    let result = store.search_sources(query, None, 20).unwrap();
    assert_eq!(result["status"], "completed");
    assert_eq!(result["coverage_complete"], true);
    assert_eq!(result["backend_generation_calls"], 0);
    result
}

#[test]
fn lexical_trace_records_actual_expression_and_only_matching_phrases() {
    let db = TestDb::new();
    let mut store = db.open("alice");
    insert(&mut store, "trace", "青榆7319使用DuckDB。", Some("project"));
    let result = store
        .search_sources("青榆7319 DuckDB SQLite 731", Some("project"), 20)
        .unwrap();
    let phrases = result["query_phrases"].as_array().unwrap();
    assert_eq!(
        result["match_expression"],
        phrases
            .iter()
            .map(|p| format!("\"{}\"", p.as_str().unwrap()))
            .collect::<Vec<_>>()
            .join(" OR ")
    );
    assert_eq!(result["match_mode"], "any_phrase");
    let hits = &candidates(&result)[0]["matched_query_phrases"];
    assert_eq!(hits.as_array().unwrap().len(), 2);
    assert!(
        hits.as_array()
            .unwrap()
            .contains(&json!("c9752 c6986 w000037000033000031000039"))
    );
    assert!(
        hits.as_array()
            .unwrap()
            .contains(&json!("w00006400007500006300006b000064000062"))
    );
    // A prefix of the number token and the absent SQLite term are not matches.
    assert!(
        !hits
            .as_array()
            .unwrap()
            .contains(&json!("w000037000033000031"))
    );
}

#[test]
fn chinese_mixed_text_and_failed_enrichment_return_verbatim_source_without_models() {
    let db = TestDb::new();
    let mut store = db.open("alice");
    let original = "张小明的孩子住在这套房子里。松石47项目用Rust和SQLite数据库，Redis暂不引入。";
    insert(&mut store, "mixed", original, None);
    for query in [
        "孩子",
        "房子",
        "数据库",
        "sqlite",
        "REDIS",
        "Rust",
        "张小明",
        "松石47",
        "子",
        "项目用Rust",
        "47",
        "SQLite不存在 Rust",
    ] {
        let result = search(&store, query);
        assert!(
            candidates(&result)
                .iter()
                .any(|candidate| candidate["source_id"] == "source:mixed"),
            "query {query:?}: {result}"
        );
        for candidate in candidates(&result) {
            let start = candidate["byte_start"].as_u64().unwrap() as usize;
            let end = candidate["byte_end"].as_u64().unwrap() as usize;
            assert_eq!(candidate["text"], &original[start..end]);
            assert!(!candidate["segment_id"].is_null());
            assert!(!candidate["segment_version"].is_null());
        }
    }
    assert!(candidates(&search(&store, "不能检索的历史副本")).is_empty());
    assert!(candidates(&search(&store, "没有发生的事情")).is_empty());
    let state = store.read().unwrap();
    assert_eq!(state.sources["mixed"].enrichment, "failed");
    assert_eq!(state.sources["mixed"].status()["index"]["status"], "ready");
    assert!(
        state
            .repository
            .list_by_owner(&UserId::from("alice"))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn fts_operators_are_data_and_query_complexity_is_bounded() {
    let db = TestDb::new();
    let mut store = db.open("alice");
    insert(&mut store, "operators", "Rust OR NOT NEAR AND SQLite", None);
    for query in [
        "OR", "NOT", "NEAR", "AND", "\"Rust\"", "(Rust)", "Rust*", "Rust:",
    ] {
        let result = store.search_sources(query, None, 20).unwrap();
        assert!(!candidates(&result).is_empty(), "{query:?}: {result}");
    }
    for query in ["", "  ", "\"(){}:*+-", "\0"] {
        assert!(store.search_sources(query, None, 20).is_err(), "{query:?}");
    }
    assert!(store.search_sources("Rust:SQLite", None, 20).is_ok());
    let longest_query = format!(
        "{} {} {} {}",
        "a".repeat(127),
        "b".repeat(127),
        "c".repeat(127),
        "d".repeat(128)
    );
    assert_eq!(longest_query.len(), 512);
    assert!(store.search_sources(&longest_query, None, 20).is_ok());
    assert!(
        store
            .search_sources(&format!(" {longest_query}"), None, 20)
            .is_err()
    );
    assert!(store.search_sources(&"x".repeat(128), None, 20).is_ok());
    assert!(store.search_sources(&"x".repeat(129), None, 20).is_err());
    assert!(store.search_sources(&"x".repeat(513), None, 20).is_err());
    assert!(store.search_sources(&"字".repeat(128), None, 20).is_ok());
    assert!(store.search_sources(&"字".repeat(129), None, 20).is_err());
    assert!(store.search_sources(&"Rust ".repeat(16), None, 20).is_ok());
    assert!(
        store
            .search_sources(&"字".repeat(100_000), None, 20)
            .is_err()
    );
    assert!(
        store
            .search_sources(&"x ".repeat(100_000), None, 20)
            .is_err()
    );
    assert!(store.search_sources(&"x ".repeat(17), None, 20).is_err());
    assert!(store.search_sources("Rust", None, 0).is_err());
    assert!(store.search_sources("Rust", None, 51).is_err());
    assert!(store.search_sources("Rust", None, usize::MAX).is_err());
    assert!(store.search_sources("Rust", Some(""), 20).is_err());
    assert!(
        store
            .search_sources("Rust", Some(&"项".repeat(201)), 20)
            .is_err()
    );
}

#[test]
fn owner_and_task_filtering_happen_before_top_k_and_visible_counts() {
    let db = TestDb::new();
    let mut alice = db.open("alice");
    let mut bob = db.open("bob");
    bob.transaction(|state| {
        for i in 0..100 {
            let turn = format!("bob:{i}");
            state.sources.insert(
                turn.clone(),
                source(&turn, "松石47 松石47 BOB_PRIVATE", Some("target")),
            );
        }
        Ok(())
    })
    .unwrap();
    alice
        .transaction(|state| {
            for i in 0..100 {
                let turn = format!("other:{i}");
                state.sources.insert(
                    turn.clone(),
                    source(&turn, "松石47 松石47 OTHER_PROJECT", Some("other")),
                );
            }
            state.sources.insert(
                "target".into(),
                source("target", "松石47 ALICE_ALLOWED", Some("target")),
            );
            Ok(())
        })
        .unwrap();
    let started = Instant::now();
    let result = alice.search_sources("松石47", Some("target"), 1).unwrap();
    assert_eq!(candidates(&result).len(), 1);
    assert_eq!(result["candidates"][0]["source_id"], "source:target");
    assert_eq!(result["candidates"][0]["task_id"], "target");
    let encoded = result.to_string();
    assert!(!encoded.contains("BOB_PRIVATE"));
    assert!(!encoded.contains("OTHER_PROJECT"));
    assert_eq!(result["candidate_count"], 1);
    eprintln!(
        "S3 fixture_sources=201 scoped_candidates=1 query_us={}",
        started.elapsed().as_micros()
    );
    assert!(candidates(&alice.search_sources("松石47", Some("missing"), 1).unwrap()).is_empty());
    assert!(
        candidates(
            &db.open("charlie")
                .search_sources("松石47", None, 1)
                .unwrap()
        )
        .is_empty()
    );
}

#[test]
fn source_edits_deletes_rebuilds_and_restarts_keep_index_in_sync() {
    let db = TestDb::new();
    let mut store = db.open("alice");
    insert(&mut store, "one", "原来使用SQLite", None);
    let before = search(&store, "SQLite");
    let business_revision = store.read().unwrap().revision;
    store
        .transaction(|state| {
            state
                .sources
                .get_mut("one")
                .unwrap()
                .request
                .as_mut()
                .unwrap()["text"] = json!("现在使用Redis");
            Ok(())
        })
        .unwrap();
    assert_eq!(store.read().unwrap().revision, business_revision);
    assert!(candidates(&search(&store, "SQLite")).is_empty());
    let after = search(&store, "Redis");
    assert_eq!(candidates(&after).len(), 1);
    assert_ne!(
        before["candidates"][0]["segment_version"],
        after["candidates"][0]["segment_version"]
    );
    assert_ne!(before["source_revision"], after["source_revision"]);
    store.rebuild_source_index().unwrap();
    let rebuilt = search(&store, "Redis");
    assert_eq!(candidates(&rebuilt).len(), 1);
    assert_ne!(after["generation"], rebuilt["generation"]);
    drop(store);
    let mut store = db.open("alice");
    assert_eq!(candidates(&search(&store, "Redis")).len(), 1);
    let stale = store.read().unwrap().sources["one"].clone();
    store
        .transaction(|state| {
            let source = state.sources.get_mut("one").unwrap();
            source.request = None;
            source.enrichment = "deleted".into();
            Ok(())
        })
        .unwrap();
    assert!(candidates(&search(&store, "Redis")).is_empty());
    store.rebuild_source_index().unwrap();
    assert!(
        store
            .transaction(|state| {
                state.sources.insert("one".into(), stale);
                Ok(())
            })
            .is_err()
    );
    assert!(store.read().unwrap().sources["one"].request.is_none());
    drop(store);
    assert!(candidates(&search(&db.open("alice"), "Redis")).is_empty());
}

#[test]
fn failed_transaction_rolls_back_source_and_search_projection_together() {
    let db = TestDb::new();
    let mut store = db.open("alice");
    insert(&mut store, "one", "SQLite", None);
    let before = search(&store, "SQLite");
    let failure: PersistenceResult<()> = store.transaction(|state| {
        state
            .sources
            .get_mut("one")
            .unwrap()
            .request
            .as_mut()
            .unwrap()["text"] = json!("Redis");
        state
            .sources
            .insert("two".into(), source("two", "Rust", None));
        Err("injected failure".into())
    });
    assert!(failure.is_err());
    let after = search(&store, "SQLite");
    assert_eq!(before["candidates"], after["candidates"]);
    assert_eq!(before["source_revision"], after["source_revision"]);
    assert!(candidates(&search(&store, "Redis")).is_empty());
    assert!(candidates(&search(&store, "Rust")).is_empty());
}

#[test]
fn v2_migration_backfills_live_sources_but_read_only_open_does_not_migrate() {
    let db = TestDb::new();
    let mut old = PersistedMemoryState::default();
    old.sources
        .insert("live".into(), source("live", "迁移后找到孩子", None));
    let mut unauthorized = source("unauthorized", "未授权的秘密关键词", None);
    unauthorized.request.as_mut().unwrap()["capture_source"] = json!(false);
    old.sources.insert("unauthorized".into(), unauthorized);
    for (turn, authorization) in [
        ("null", Some(Value::Null)),
        ("string", Some(json!("true"))),
        ("missing", None),
    ] {
        let mut unapproved = source(turn, "未授权的秘密关键词", None);
        let request = unapproved
            .request
            .as_mut()
            .unwrap()
            .as_object_mut()
            .unwrap();
        if let Some(value) = authorization {
            request.insert("capture_source".into(), value);
        } else {
            request.remove("capture_source");
        }
        old.sources.insert(turn.into(), unapproved);
    }
    let mut deleted = source("deleted", "已经删除的房子", None);
    deleted.request = None;
    old.sources.insert("deleted".into(), deleted);
    let mut old_json = serde_json::to_value(old).unwrap();
    for source in old_json["sources"].as_object_mut().unwrap().values_mut() {
        source.as_object_mut().unwrap().remove("index");
    }
    let connection = Connection::open(&db.0).unwrap();
    connection.execute_batch("CREATE TABLE memory_profiles (owner_id TEXT PRIMARY KEY, state_json TEXT NOT NULL); PRAGMA user_version=2;").unwrap();
    connection
        .execute(
            "INSERT INTO memory_profiles VALUES (?1,?2)",
            params!["alice", old_json.to_string()],
        )
        .unwrap();
    assert!(SqliteMemoryStore::open_read_only(&db.0, UserId::from("alice")).is_err());
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 2);
    let store = db.open("alice");
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 10);
    assert_eq!(candidates(&search(&store, "孩子")).len(), 1);
    assert!(candidates(&search(&store, "房子")).is_empty());
    assert!(candidates(&search(&store, "秘密关键词")).is_empty());
    for turn in ["unauthorized", "null", "string", "missing"] {
        assert_eq!(
            store.read().unwrap().sources[turn].status()["index"]["status"],
            "not_requested"
        );
    }
    assert!(candidates(&search(&store, "不能检索的历史副本")).is_empty());
}

#[test]
fn read_only_search_and_disabled_channel_never_change_source_state() {
    let db = TestDb::new();
    let mut writer = db.open("alice");
    insert(&mut writer, "one", "Rust 数据库", None);
    let before = serde_json::to_value(writer.read().unwrap()).unwrap();
    let mut reader = SqliteMemoryStore::open_read_only(&db.0, UserId::from("alice")).unwrap();
    assert_eq!(candidates(&search(&reader, "Rust")).len(), 1);
    assert!(
        reader
            .transaction(|state| {
                state.sources.clear();
                Ok(())
            })
            .is_err()
    );
    assert!(reader.rebuild_source_index().is_err());
    reader.set_source_search_enabled(false);
    let disabled = reader.search_sources("Rust", None, 20).unwrap();
    assert_eq!(disabled["status"], "disabled");
    assert!(candidates(&disabled).is_empty());
    reader.set_source_search_enabled(true);
    assert_eq!(candidates(&search(&reader, "Rust")).len(), 1);
    assert_eq!(
        serde_json::to_value(writer.read().unwrap()).unwrap(),
        before
    );
}

#[test]
fn long_unicode_source_segments_are_bounded_and_keep_exact_byte_offsets() {
    let db = TestDb::new();
    let mut store = db.open("alice");
    let original = format!("{}唯一尾部关键词松石47", "前文🙂孩子的房子。\n".repeat(400));
    insert(&mut store, "long", &original, None);
    let result = search(&store, "松石47");
    assert!(!candidates(&result).is_empty());
    for candidate in candidates(&result) {
        let start = candidate["byte_start"].as_u64().unwrap() as usize;
        let end = candidate["byte_end"].as_u64().unwrap() as usize;
        let text = candidate["text"].as_str().unwrap();
        assert_eq!(text, &original[start..end]);
        assert!(
            text.len() <= 4096,
            "unbounded segment: {} bytes",
            text.len()
        );
        assert!(text.contains("松石47"));
    }
}

#[test]
fn concurrent_writers_and_rebuild_preserve_every_committed_source() {
    let db = TestDb::new();
    drop(db.open("alice"));
    let barrier = Arc::new(Barrier::new(3));
    let workers: Vec<_> = (0..3)
        .map(|worker| {
            let path = db.0.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let mut store = SqliteMemoryStore::open(path, UserId::from("alice")).unwrap();
                barrier.wait();
                if worker == 2 {
                    store.rebuild_source_index().unwrap();
                } else {
                    for i in 0..4 {
                        let turn = format!("{worker}:{i}");
                        insert(&mut store, &turn, &format!("共享关键词 {turn}"), None);
                    }
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    let store = db.open("alice");
    assert_eq!(store.read().unwrap().sources.len(), 8);
    assert_eq!(candidates(&search(&store, "共享关键词")).len(), 8);
}

#[test]
fn stale_projection_cannot_return_an_authoritatively_deleted_source() {
    let db = TestDb::new();
    let mut store = db.open("alice");
    insert(&mut store, "one", "必须删除的SQLite原文", None);
    let mut authoritative = store.read().unwrap();
    authoritative
        .sources
        .get_mut("one")
        .unwrap()
        .request
        .as_mut()
        .unwrap()["text"] = json!("索引尚未覆盖的Redis原文");
    let connection = Connection::open(&db.0).unwrap();
    connection
        .execute(
            "UPDATE memory_profiles SET state_json=?1 WHERE owner_id=?2",
            params![serde_json::to_string(&authoritative).unwrap(), "alice"],
        )
        .unwrap();
    let missing_projection = store.search_sources("Redis", None, 20).unwrap();
    assert!(candidates(&missing_projection).is_empty());
    assert_eq!(missing_projection["coverage_complete"], false);
    assert_eq!(missing_projection["status"], "incomplete");
    authoritative.sources.get_mut("one").unwrap().request = None;
    // Simulate an old projection surviving a source deletion outside the normal atomic writer.
    Connection::open(&db.0)
        .unwrap()
        .execute(
            "UPDATE memory_profiles SET state_json=?1 WHERE owner_id=?2",
            params![serde_json::to_string(&authoritative).unwrap(), "alice"],
        )
        .unwrap();
    let result = store.search_sources("SQLite", None, 20).unwrap();
    assert!(candidates(&result).is_empty());
    assert_eq!(result["coverage_complete"], false);
    assert_eq!(result["status"], "incomplete");
    assert!(!result.to_string().contains("必须删除的SQLite原文"));
    store.rebuild_source_index().unwrap();
    assert!(candidates(&search(&store, "SQLite")).is_empty());
}

#[test]
fn registered_aliases_expand_only_within_the_owner_without_rebuilding_sources() {
    let db = TestDb::new();
    let mut alice = db.open("alice");
    insert(
        &mut alice,
        "one",
        "松石47项目使用SQLite。我住在上海。",
        None,
    );
    let before = search(&alice, "松石47");
    assert!(candidates(&search(&alice, "小石项目")).is_empty());
    alice
        .transaction(|state| {
            state.entities.upsert(Entity {
                id: EntityId::from("project"),
                owner_id: UserId::from("alice"),
                entity_type: EntityType::Other("project".into()),
                canonical_name: "松石47".into(),
                aliases: vec!["小石项目".into()],
            });
            Ok(())
        })
        .unwrap();
    let expanded = search(&alice, "小石项目");
    assert_eq!(candidates(&expanded).len(), 1);
    assert_eq!(expanded["candidates"][0]["source_id"], "source:one");
    assert_eq!(before["generation"], expanded["generation"]);
    assert_eq!(before["source_revision"], expanded["source_revision"]);
    assert_ne!(before["read_revision"], expanded["read_revision"]);
    assert!(candidates(&search(&alice, "华东")).is_empty());
    let mut bob = db.open("bob");
    bob.transaction(|state| {
        state.entities.upsert(Entity {
            id: EntityId::from("project"),
            owner_id: UserId::from("bob"),
            entity_type: EntityType::Other("project".into()),
            canonical_name: "松石47".into(),
            aliases: vec!["旧项目".into()],
        });
        state
            .sources
            .insert("bob".into(), source("bob", "松石47 BOB_PRIVATE", None));
        Ok(())
    })
    .unwrap();
    assert!(candidates(&search(&alice, "旧项目")).is_empty());
    let bob_result = search(&bob, "旧项目");
    assert_eq!(candidates(&bob_result).len(), 1);
    assert_eq!(bob_result["candidates"][0]["source_id"], "source:bob");
    assert!(candidates(&search(&bob, "小石项目")).is_empty());
}

#[test]
fn partial_correction_preserves_distinct_atomic_links_and_exact_quote_positions() {
    let db = TestDb::new();
    let mut store = db.open("alice");
    let original = "我住在上海，使用Rust。";
    let fact = |turn: &str, predicate: &str, value: &str, quote: &str, at| {
        CandidateMemory::new(
            UserId::from("alice"),
            EntityId::from("self"),
            MemoryType::Fact,
            Predicate::from(predicate),
            MemoryValue::Text(value.into()),
            Timestamp(at),
            Evidence::new(
                EvidenceId(format!("{turn}:{predicate}")),
                SessionId::from("session"),
                TurnId::from(turn),
                quote,
            ),
        )
    };
    let id = |outcome| match outcome {
        FixtureOutcome::Applied {
            memory_id: Some(id),
            ..
        } => id,
        other => panic!("unexpected ingest: {other:?}"),
    };
    let (old_id, rust_id, new_id) = store
        .transaction(|state| {
            state
                .sources
                .insert("one".into(), source("one", original, None));
            state
                .sources
                .insert("two".into(), source("two", "纠正一下，我住在苏州。", None));
            let mut registry = InMemoryPredicateRegistry::default();
            registry.register(Predicate::from("lives_in"), Cardinality::Single);
            registry.register(Predicate::from("uses"), Cardinality::Single);
            state.with_engine(registry, |engine, _| {
                let old_id = id(engine.seed_fixture(common::grounded(fact(
                    "one",
                    "lives_in",
                    "上海",
                    "我住在上海",
                    100,
                )))?);
                let rust_id = id(engine.seed_fixture(common::grounded(fact(
                    "one",
                    "uses",
                    "Rust",
                    "使用Rust",
                    100,
                )))?);
                let mut corrected = fact("two", "lives_in", "苏州", "我住在苏州", 101);
                corrected.intent = CandidateIntent::Supersede {
                    target: Some(old_id),
                    reason: SupersedeReason::Correction,
                };
                let new_id = id(engine.seed_fixture(common::grounded(corrected))?);
                Ok((old_id, rust_id, new_id))
            })
        })
        .unwrap();
    let result = search(&store, "上海");
    assert_eq!(candidates(&result).len(), 1);
    let candidate = &result["candidates"][0];
    assert_eq!(candidate["text"], original);
    assert_eq!(candidate["mapping_uncertain"], false);
    let links = candidate["known_memories"].as_array().unwrap();
    assert_eq!(links.len(), 2);
    let old = links
        .iter()
        .find(|link| link["memory_id"] == json!(old_id))
        .unwrap();
    let rust = links
        .iter()
        .find(|link| link["memory_id"] == json!(rust_id))
        .unwrap();
    assert_eq!(old["status"], "Superseded");
    assert_eq!(old["supersede_reason"], "Correction");
    assert_eq!(old["superseded_by"], json!(new_id));
    assert_eq!(old["association"], "exact_quote");
    assert_eq!(old["quote_byte_range"], json!([0, "我住在上海".len()]));
    assert_eq!(rust["status"], "Active");
    assert!(rust["supersede_reason"].is_null());
    let rust_start = original.find("使用Rust").unwrap();
    assert_eq!(
        rust["quote_byte_range"],
        json!([rust_start, rust_start + "使用Rust".len()])
    );

    store
        .transaction(|state| {
            state
                .sources
                .get_mut("one")
                .unwrap()
                .request
                .as_mut()
                .unwrap()["text"] = json!("我住在上海，使用Rust。原文重复：使用Rust。");
            Ok(())
        })
        .unwrap();
    let ambiguous = search(&store, "Rust");
    let candidate = &ambiguous["candidates"][0];
    assert_eq!(candidate["mapping_uncertain"], true);
    let rust = candidate["known_memories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|link| link["memory_id"] == json!(rust_id))
        .unwrap();
    assert_eq!(rust["association"], "same_turn_uncertain");
    assert!(rust["quote_byte_range"].is_null());
}

#[test]
fn overlapping_segments_preserve_boundary_phrases_without_inventing_partial_words() {
    let db = TestDb::new();
    let mut store = db.open("alice");
    insert(
        &mut store,
        "end",
        &format!("{}SQLite{}", "前".repeat(765), "后".repeat(50)),
        None,
    );
    assert_eq!(candidates(&search(&store, "SQLite")).len(), 1);
    assert!(candidates(&search(&store, "SQL")).is_empty());
    insert(
        &mut store,
        "start",
        &format!("{}abcdefghijklmnop{}", "前".repeat(635), "后".repeat(800)),
        None,
    );
    assert_eq!(candidates(&search(&store, "abcdefghijklmnop")).len(), 1);
    assert!(candidates(&search(&store, "fghijklmnop")).is_empty());
    insert(
        &mut store,
        "chinese",
        &format!("{}孩子的房子松石47{}", "前".repeat(767), "后".repeat(50)),
        None,
    );
    let chinese = search(&store, "孩子的房子松石47");
    assert_eq!(candidates(&chinese).len(), 1);
    assert!(
        chinese["candidates"][0]["text"]
            .as_str()
            .unwrap()
            .contains("孩子的房子松石47")
    );
    insert(&mut store, "repeated-han", &"子".repeat(1800), None);
    let repeated = search(&store, &"子".repeat(128));
    assert_eq!(candidates(&repeated).len(), 3);
    for candidate in candidates(&repeated) {
        assert_eq!(candidate["source_id"], "source:repeated-han");
        assert!(
            candidate["text"]
                .as_str()
                .unwrap()
                .contains(&"子".repeat(128))
        );
    }
    insert(&mut store, "repeated-ascii", &"x".repeat(1800), None);
    assert!(candidates(&search(&store, &"x".repeat(128))).is_empty());
}

#[test]
fn index_write_errors_roll_back_source_edits_and_rebuild_generation_switch() {
    let db = TestDb::new();
    let mut store = db.open("alice");
    insert(&mut store, "one", "SQLite", None);
    insert(&mut store, "two", "Rust", None);
    let before = serde_json::to_value(store.read().unwrap()).unwrap();
    let old_search = search(&store, "SQLite Rust");
    let connection = Connection::open(&db.0).unwrap();
    connection.execute_batch("CREATE TRIGGER injected_index_failure BEFORE INSERT ON source_segments
        WHEN new.source_id='source:two' BEGIN SELECT RAISE(ABORT, 'injected index write failure'); END;").unwrap();
    assert!(
        store
            .transaction(|state| {
                state
                    .sources
                    .get_mut("one")
                    .unwrap()
                    .request
                    .as_mut()
                    .unwrap()["text"] = json!("Redis");
                state
                    .sources
                    .get_mut("two")
                    .unwrap()
                    .request
                    .as_mut()
                    .unwrap()["text"] = json!("Python");
                Ok(())
            })
            .is_err()
    );
    assert_eq!(serde_json::to_value(store.read().unwrap()).unwrap(), before);
    assert!(store.rebuild_source_index().is_err());
    assert_eq!(serde_json::to_value(store.read().unwrap()).unwrap(), before);
    let after_search = search(&store, "SQLite Rust");
    assert_eq!(after_search["generation"], old_search["generation"]);
    assert_eq!(after_search["candidates"], old_search["candidates"]);
    assert!(candidates(&search(&store, "Redis Python")).is_empty());
    connection
        .execute_batch("DROP TRIGGER injected_index_failure")
        .unwrap();
    store.rebuild_source_index().unwrap();
    assert_eq!(candidates(&search(&store, "SQLite Rust")).len(), 2);
}

#[test]
fn outdated_index_processor_cannot_block_forget_or_keep_deleted_generations() {
    use hippocampus::chat::{ChatConfig, ChatMemoryService};

    let db = TestDb::new();
    let mut store = db.open("alice");
    insert(&mut store, "one", "删除这条SQLite原文", None);
    insert(&mut store, "two", "保留这条Rust原文", None);
    let mut service = ChatMemoryService::new(store, ChatConfig::default()).unwrap();
    let connection = Connection::open(&db.0).unwrap();
    connection.execute_batch("UPDATE source_indexes SET processing_version='old-processor' WHERE owner_id='alice';
        INSERT INTO source_segments(owner_id,source_id,source_version,generation,task_id,received_sequence,byte_start,byte_end,text,index_text)
        SELECT owner_id,source_id,source_version,generation+100,task_id,received_sequence,byte_start,byte_end,text,index_text
        FROM source_segments WHERE owner_id='alice' AND source_id='source:one';").unwrap();
    let count = || {
        connection.query_row("SELECT count(*) FROM source_segments WHERE owner_id='alice' AND source_id='source:one'", [], |row| row.get::<_,i64>(0)).unwrap()
    };
    assert_eq!(count(), 2);
    let forgotten = service
        .call("memory_forget", json!({"source_id":"source:one"}), || false)
        .unwrap();
    assert_eq!(forgotten["removed"], true);
    assert_eq!(forgotten["deleted_source_ids"], json!(["source:one"]));
    assert_eq!(count(), 0);
    let mut reader = db.open("alice");
    let state = reader.read().unwrap();
    assert!(state.sources["one"].request.is_none());
    assert_eq!(state.sources["one"].status()["index"]["status"], "deleted");
    assert!(state.sources["two"].request.is_some());
    let incomplete = reader.search_sources("Rust", None, 20).unwrap();
    assert_eq!(incomplete["status"], "incomplete");
    assert_eq!(incomplete["coverage_complete"], false);
    assert!(candidates(&incomplete).is_empty());
    reader.rebuild_source_index().unwrap();
    assert_eq!(candidates(&search(&reader, "Rust")).len(), 1);
    assert!(candidates(&search(&reader, "SQLite")).is_empty());
}

#[test]
fn missing_fts_postings_report_incomplete_instead_of_no_memories() {
    let db = TestDb::new();
    let mut store = db.open("alice");
    insert(&mut store, "one", "松石47项目使用SQLite", None);
    assert_eq!(candidates(&search(&store, "SQLite")).len(), 1);
    let before = serde_json::to_value(store.read().unwrap()).unwrap();
    let connection = Connection::open(&db.0).unwrap();
    connection
        .execute_batch("INSERT INTO source_fts(source_fts) VALUES('delete-all')")
        .unwrap();
    let result = store.search_sources("SQLite", None, 20).unwrap();
    assert_eq!(result["status"], "incomplete");
    assert_eq!(result["coverage_complete"], false);
    assert!(candidates(&result).is_empty());
    assert_eq!(serde_json::to_value(store.read().unwrap()).unwrap(), before);
    let rows: i64 = connection
        .query_row("SELECT count(*) FROM source_segments", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 1);
}
