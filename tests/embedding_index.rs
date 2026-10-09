//! Real SQLite cache invariants; no embedding client or model is instantiated.
use std::{path::PathBuf, sync::mpsc};

use hippocampus::{
    chat::{ChatConfig, ChatMemoryService, EnrichmentMode},
    embedding,
    embedding_index::{self, BindingToken, SourceInput},
    *,
};
use rusqlite::Connection;
use serde_json::{Value, json};

struct Db(PathBuf);
impl Db {
    fn new() -> Self {
        Self(
            std::env::temp_dir()
                .join(format!("embedding-index-{}", diagnostics::new_id()))
                .join("memory.sqlite3"),
        )
    }
    fn open(&self, owner: &str) -> SqliteMemoryStore {
        SqliteMemoryStore::open(&self.0, owner.into()).unwrap()
    }
    fn sql(&self) -> Connection {
        Connection::open(&self.0).unwrap()
    }
    fn count(&self, owner: &str) -> i64 {
        self.sql()
            .query_row(
                "SELECT count(*) FROM source_vectors WHERE owner_id=?1",
                [owner],
                |r| r.get(0),
            )
            .unwrap()
    }
    fn snapshot(&self, owner: &str) -> String {
        self.sql()
            .query_row(
                "SELECT state_json FROM memory_profiles WHERE owner_id=?1",
                [owner],
                |r| r.get(0),
            )
            .unwrap()
    }
}
impl Drop for Db {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.0.parent().unwrap());
    }
}

fn binding(model: embedding::Model, digest: &str) -> embedding_index::Binding {
    embedding_index::Binding::new(
        embedding::Binding::from_tags(
            model,
            &json!({"models":[{
                "name":model.tag(), "digest":digest.repeat(64)
            }]}),
        )
        .unwrap(),
    )
    .unwrap()
}
fn source(id: &str, text: &str) -> SourceRecord {
    SourceRecord {
        management: Default::default(),
        id: id.into(),
        session_id: "session".into(),
        turn_id: id.into(),
        observed_at: 100,
        received_sequence: 7,
        request: Some(json!({"text":text,"capture_source":true})),
        enrichment: "queued".into(),
        error: None,
        result: None,
        base_revision: 0,
        index: Value::Null,
        task: EnrichmentTask {
            processing_version: Some("w4_runtime_v1".into()),
            ..Default::default()
        },
    }
}
fn seed(db: &Db, owner: &str, text: &str) {
    db.open(owner)
        .transaction(|s| {
            s.sources
                .insert("internal-key".into(), source("source-one", text));
            s.sources.insert(
                "other-key".into(),
                source("source-two", "另一个来源保持不变。"),
            );
            s.last_applied_source_sequence = 3;
            s.source_less_barrier_sequence = 2;
            Ok(())
        })
        .unwrap();
}
fn vectors(input: &SourceInput) -> Vec<Vec<f32>> {
    input
        .segments
        .iter()
        .map(|_| {
            let mut v = vec![0.0; embedding::DIMENSIONS];
            v[0] = 1.0;
            v
        })
        .collect()
}
fn cache(store: &mut SqliteMemoryStore, token: &BindingToken, id: &str) -> SourceInput {
    let input = store.embedding_source_input(token, id).unwrap();
    store
        .write_embedding_vectors(&input, &vectors(&input))
        .unwrap();
    input
}

#[test]
fn v10_migration_only_creates_empty_cache_for_all_owners_and_reopens() {
    let db = Db::new();
    seed(&db, "alice", "迁移后仍保留的来源。");
    seed(&db, "bob", "另一位所有者的来源。");
    let before = [db.snapshot("alice"), db.snapshot("bob")];
    db.sql()
        .execute_batch(
            "DROP TABLE source_vectors; DROP TABLE source_vector_bindings; PRAGMA user_version=10;",
        )
        .unwrap();
    let readonly = SqliteMemoryStore::open_read_only(&db.0, "alice".into()).unwrap();
    assert_eq!(readonly.read().unwrap().sources.len(), 2);
    assert_eq!(
        db.sql()
            .pragma_query_value::<i64, _>(None, "user_version", |r| r.get(0))
            .unwrap(),
        10
    );
    drop(readonly);
    for _ in 0..2 {
        drop(db.open("alice"));
        drop(SqliteMemoryStore::open_operation(&db.0, "bob".into()).unwrap());
        assert_eq!(
            db.sql()
                .pragma_query_value::<i64, _>(None, "user_version", |r| r.get(0))
                .unwrap(),
            11
        );
        assert_eq!([db.snapshot("alice"), db.snapshot("bob")], before);
        assert_eq!(db.count("alice") + db.count("bob"), 0);
        assert_eq!(
            db.sql()
                .query_row::<i64, _, _>("SELECT count(*) FROM source_vector_bindings", [], |r| r
                    .get(0))
                .unwrap(),
            0
        );
        assert_eq!(
            db.sql()
                .query_row::<i64, _, _>("SELECT count(*) FROM model_calls", [], |r| r.get(0))
                .unwrap(),
            0
        );
    }
}

#[test]
fn cache_writes_are_revision_neutral_unique_little_endian_and_persist_on_reopen() {
    let db = Db::new();
    seed(&db, "alice", &"一段多字节正文。".repeat(220));
    let before = db.snapshot("alice");
    let mut store = db.open("alice");
    let token = store
        .set_embedding_binding(binding(embedding::Model::Qwen, "a"))
        .unwrap();
    let input = cache(&mut store, &token, "source-one");
    assert!(input.segments.len() > 1);
    store
        .write_embedding_vectors(&input, &vectors(&input))
        .unwrap();
    assert_eq!(db.count("alice"), input.segments.len() as i64);
    assert_eq!(db.snapshot("alice"), before);
    let blob: Vec<u8> = db
        .sql()
        .query_row("SELECT vector FROM source_vectors LIMIT 1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(&blob[..4], &1.0f32.to_le_bytes());
    drop(store);
    let readonly = SqliteMemoryStore::open_read_only(&db.0, "alice".into()).unwrap();
    let restored_token = readonly.embedding_binding().unwrap().unwrap();
    assert_eq!(restored_token.binding(), token.binding());
    let loaded = readonly
        .read_embedding_vectors(&restored_token, "source-one")
        .unwrap();
    assert_eq!(loaded.len(), input.segments.len());
    for (segment, cached) in input.segments.iter().zip(&loaded) {
        assert_eq!(
            (cached.index, cached.start, cached.end),
            (segment.index, segment.start, segment.end)
        );
        assert_eq!(cached.vector, vectors(&input)[0]);
    }
    assert_eq!(db.snapshot("alice"), before);
}

#[test]
fn binding_fields_are_strict_and_switches_reject_late_writes_including_aba() {
    let db = Db::new();
    seed(&db, "alice", "绑定测试正文。");
    let mut store = db.open("alice");
    let a = binding(embedding::Model::Qwen, "a");
    let token = store.set_embedding_binding(a.clone()).unwrap();
    let input = cache(&mut store, &token, "source-one");
    // Every format/dimension/normalization/encoding field is mechanically checked.
    for field in [
        "digest",
        "dimensions",
        "query_format",
        "document_format",
        "segment_version",
        "normalization_version",
        "blob_version",
    ] {
        let mut invalid = serde_json::to_value(&a).unwrap();
        if field == "dimensions" {
            invalid["embedding"][field] = json!(5);
        } else if ["normalization_version", "blob_version"].contains(&field) {
            invalid[field] = json!("unsupported");
        } else {
            invalid["embedding"][field] = json!("unsupported");
        }
        let invalid = serde_json::from_value(invalid).unwrap();
        assert!(store.set_embedding_binding(invalid).is_err(), "{field}");
    }
    let same = store.set_embedding_binding(a.clone()).unwrap();
    assert_eq!(
        store
            .read_embedding_vectors(&same, "source-one")
            .unwrap()
            .len(),
        1
    );
    let b = store
        .set_embedding_binding(binding(embedding::Model::Bge, "b"))
        .unwrap();
    assert!(
        store
            .write_embedding_vectors(&input, &vectors(&input))
            .unwrap_err()
            .to_string()
            .contains("binding_changed")
    );
    assert!(store.read_embedding_vectors(&token, "source-one").is_err());
    assert!(
        store
            .read_embedding_vectors(&b, "source-one")
            .unwrap()
            .is_empty()
    );
    cache(&mut store, &b, "source-one");
    assert_eq!(db.count("alice"), 2); // independent namespaces
    let new_a = store.set_embedding_binding(a).unwrap();
    assert!(
        store
            .write_embedding_vectors(&input, &vectors(&input))
            .is_err()
    );
    assert_eq!(
        store
            .read_embedding_vectors(&new_a, "source-one")
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn owner_scope_cannot_read_or_write_another_owners_source_or_token() {
    let db = Db::new();
    seed(&db, "alice", "甲的原话。");
    seed(&db, "bob", "乙的原话。");
    let mut a = db.open("alice");
    let mut b = db.open("bob");
    let ta = a
        .set_embedding_binding(binding(embedding::Model::Qwen, "a"))
        .unwrap();
    let tb = b
        .set_embedding_binding(binding(embedding::Model::Qwen, "a"))
        .unwrap();
    let input = cache(&mut a, &ta, "source-one");
    assert!(
        b.write_embedding_vectors(&input, &vectors(&input))
            .unwrap_err()
            .to_string()
            .contains("owner_mismatch")
    );
    assert!(b.read_embedding_vectors(&ta, "source-one").is_err());
    assert!(
        b.read_embedding_vectors(&tb, "source-one")
            .unwrap()
            .is_empty()
    );
    assert!(a.embedding_source_input(&ta, "unknown-source").is_err());
    assert_eq!(db.count("alice"), 1);
    assert_eq!(db.count("bob"), 0);
}

#[test]
fn body_changes_deletion_removal_and_consent_revoke_clear_every_binding_atomically() {
    for operation in ["body", "deleted", "removed", "consent"] {
        let db = Db::new();
        seed(&db, "alice", "待失效的原话。");
        seed(&db, "bob", "相同 ID 的其他所有者原话。");
        let mut store = db.open("alice");
        store.set_source_search_enabled(false);
        for model in [embedding::Model::Qwen, embedding::Model::Bge] {
            let token = store.set_embedding_binding(binding(model, "a")).unwrap();
            cache(&mut store, &token, "source-one");
            cache(&mut store, &token, "source-two");
        }
        let token = store
            .set_embedding_binding(binding(embedding::Model::Bge, "a"))
            .unwrap();
        let input = store.embedding_source_input(&token, "source-one").unwrap();
        let mut bob = db.open("bob");
        let bob_token = bob
            .set_embedding_binding(binding(embedding::Model::Qwen, "a"))
            .unwrap();
        cache(&mut bob, &bob_token, "source-one");
        let edit = |s: &mut PersistedMemoryState| {
            if operation == "removed" {
                s.sources.remove("internal-key");
            } else {
                let source = s.sources.get_mut("internal-key").unwrap();
                match operation {
                    "body" => source.request.as_mut().unwrap()["text"] = json!("正文已经改变。"),
                    "deleted" => {
                        source.request = None;
                        source.enrichment = "deleted".into();
                    }
                    "consent" => source.request.as_mut().unwrap()["capture_source"] = json!(false),
                    _ => unreachable!(),
                }
            }
        };
        let rollback = store.transaction::<()>(|s| {
            edit(s);
            Err("rollback".into())
        });
        assert!(rollback.is_err());
        assert_eq!(db.count("alice"), 4);
        if operation == "removed" {
            // Existing source identity/tombstone protection is not relaxed for this cache.
            let before = db.snapshot("alice");
            assert!(
                store
                    .transaction(|s| {
                        edit(s);
                        Ok(())
                    })
                    .unwrap_err()
                    .to_string()
                    .contains("source tombstone must be retained")
            );
            assert_eq!(db.snapshot("alice"), before);
            assert_eq!(db.count("alice"), 4);
            continue;
        }
        store
            .transaction(|s| {
                edit(s);
                Ok(())
            })
            .unwrap();
        assert_eq!(db.count("alice"), 2, "{operation}");
        assert_eq!(db.count("bob"), 1);
        assert!(
            store
                .write_embedding_vectors(&input, &vectors(&input))
                .is_err(),
            "{operation}"
        );
        assert_eq!(
            store
                .read_embedding_vectors(&token, "source-two")
                .unwrap()
                .len(),
            1
        );
    }
}

#[test]
fn status_and_unrelated_business_changes_do_not_invalidate_vectors_or_late_writes() {
    let db = Db::new();
    seed(&db, "alice", "原话没有发生变化。");
    let mut store = db.open("alice");
    let token = store
        .set_embedding_binding(binding(embedding::Model::Qwen, "a"))
        .unwrap();
    let input = cache(&mut store, &token, "source-one");
    let revision = store.read().unwrap().revision;
    store
        .transaction(|s| {
            s.entities.upsert(Entity {
                id: "other-entity".into(),
                owner_id: "alice".into(),
                entity_type: EntityType::Person,
                canonical_name: "另一个虚构主体".into(),
                aliases: vec![],
            });
            s.sources.get_mut("internal-key").unwrap().enrichment = "pending".into();
            s.sources
                .get_mut("other-key")
                .unwrap()
                .request
                .as_mut()
                .unwrap()["text"] = json!("其他来源正文变化。");
            Ok(())
        })
        .unwrap();
    assert!(store.read().unwrap().revision > revision);
    let before = db.snapshot("alice");
    store
        .write_embedding_vectors(&input, &vectors(&input))
        .unwrap();
    assert_eq!(db.snapshot("alice"), before);
    assert_eq!(
        store
            .read_embedding_vectors(&token, "source-one")
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn cache_cleanup_failure_rolls_back_source_and_lexical_changes_together() {
    let db = Db::new();
    seed(&db, "alice", "清理失败时保留的旧原话。");
    let mut store = db.open("alice");
    let token = store
        .set_embedding_binding(binding(embedding::Model::Qwen, "a"))
        .unwrap();
    cache(&mut store, &token, "source-one");
    let before = db.snapshot("alice");
    let segments: String = db
        .sql()
        .query_row(
            "SELECT group_concat(text) FROM source_segments WHERE owner_id='alice'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    db.sql().execute_batch("CREATE TRIGGER fail_vector_cleanup BEFORE DELETE ON source_vectors BEGIN SELECT RAISE(ABORT, 'injected cache cleanup failure'); END;").unwrap();
    let error = store
        .transaction(|s| {
            s.sources.get_mut("internal-key").unwrap().request = None;
            Ok(())
        })
        .unwrap_err();
    assert!(error.to_string().contains("injected cache cleanup failure"));
    assert_eq!(db.snapshot("alice"), before);
    assert_eq!(db.count("alice"), 1);
    assert_eq!(
        store
            .read_embedding_vectors(&token, "source-one")
            .unwrap()
            .len(),
        1
    );
    let after: String = db
        .sql()
        .query_row(
            "SELECT group_concat(text) FROM source_segments WHERE owner_id='alice'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(after, segments);
}

#[test]
fn malformed_batch_or_tampered_input_never_partially_writes() {
    let db = Db::new();
    seed(&db, "alice", &"多段来源。".repeat(500));
    let mut store = db.open("alice");
    let token = store
        .set_embedding_binding(binding(embedding::Model::Qwen, "a"))
        .unwrap();
    let input = store.embedding_source_input(&token, "source-one").unwrap();
    let mut batch = vectors(&input);
    assert!(batch.len() > 1);
    assert!(store.write_embedding_vectors(&input, &batch[..1]).is_err());
    let last = batch.len() - 1;
    for invalid in [
        vec![0.0; embedding::DIMENSIONS],
        vec![f32::NAN; embedding::DIMENSIONS],
        vec![f32::INFINITY; embedding::DIMENSIONS],
        vec![1.0; 2],
    ] {
        batch[last] = invalid;
        assert!(store.write_embedding_vectors(&input, &batch).is_err());
        assert_eq!(db.count("alice"), 0);
    }
    let mut changed = input.clone();
    changed.segments[0].text.push('改');
    assert!(
        store
            .write_embedding_vectors(&changed, &vectors(&input))
            .is_err()
    );
    assert_eq!(db.count("alice"), 0);
}

#[test]
fn corrupt_blobs_and_row_metadata_are_refused_instead_of_returned() {
    let db = Db::new();
    seed(&db, "alice", "损坏向量测试正文。");
    let mut store = db.open("alice");
    let token = store
        .set_embedding_binding(binding(embedding::Model::Qwen, "a"))
        .unwrap();
    let input = cache(&mut store, &token, "source-one");
    let blob = |n: f32| n.to_le_bytes().repeat(embedding::DIMENSIONS);
    for bad in [
        vec![0; 3],
        blob(0.0),
        blob(f32::NAN),
        blob(f32::INFINITY),
        blob(2.0),
    ] {
        db.sql()
            .execute("UPDATE source_vectors SET vector=?1", [bad])
            .unwrap();
        assert!(store.read_embedding_vectors(&token, "source-one").is_err());
        store
            .write_embedding_vectors(&input, &vectors(&input))
            .unwrap();
    }
    for change in [
        "dims=1",
        "blob_version='unknown'",
        "binding='{}'",
        "source_content_hash='wrong'",
        "content_hash='wrong'",
        "start_byte=1",
        "end_byte=end_byte+1",
        "segment_index=1000",
    ] {
        db.sql()
            .execute(&format!("UPDATE source_vectors SET {change}"), [])
            .unwrap();
        assert!(
            store.read_embedding_vectors(&token, "source-one").is_err(),
            "{change}"
        );
        db.sql().execute("DELETE FROM source_vectors", []).unwrap();
        store
            .write_embedding_vectors(&input, &vectors(&input))
            .unwrap();
    }
    // A corrupted binding control record is fail-closed too.
    db.sql()
        .execute("UPDATE source_vector_bindings SET binding='{}'", [])
        .unwrap();
    assert!(store.embedding_binding().is_err());
    assert!(store.read_embedding_vectors(&token, "source-one").is_err());
    assert!(
        store
            .write_embedding_vectors(&input, &vectors(&input))
            .is_err()
    );
}

#[test]
fn real_forget_deletes_cached_source_and_rejects_inflight_result_after_reopen() {
    let db = Db::new();
    let mut service = ChatMemoryService::new(
        db.open("alice"),
        ChatConfig {
            enrichment_mode: EnrichmentMode::Off,
            ..Default::default()
        },
    )
    .unwrap();
    let out=service.call("memory_remember",json!({"text":"明确授权保存的虚构原话。","session_id":"cache-delete","turn_id":"one","capture_source":true,"observed_at":100}),||false).unwrap();
    let id = out["source"]["source_id"].as_str().unwrap();
    let mut store = db.open("alice");
    let token = store
        .set_embedding_binding(binding(embedding::Model::Qwen, "a"))
        .unwrap();
    let input = cache(&mut store, &token, id);
    service
        .call("memory_forget", json!({"source_id":id}), || false)
        .unwrap();
    drop(store);
    let mut reopened = db.open("alice");
    assert_eq!(db.count("alice"), 0);
    assert!(reopened.read_embedding_vectors(&token, id).is_err());
    assert!(
        reopened
            .write_embedding_vectors(&input, &vectors(&input))
            .is_err()
    );
    assert_eq!(db.count("alice"), 0);
}

#[test]
fn concurrent_late_writer_waits_for_delete_or_body_change_commit_then_is_rejected() {
    for change in ["delete", "body"] {
        let db = Db::new();
        seed(&db, "alice", "并发删除的虚构来源。");
        let mut deleter = db.open("alice");
        let token = deleter
            .set_embedding_binding(binding(embedding::Model::Qwen, "a"))
            .unwrap();
        let input = cache(&mut deleter, &token, "source-one");
        let mut late = SqliteMemoryStore::open_operation(&db.0, "alice".into()).unwrap();
        let (entered, wait) = mpsc::channel();
        let (result, done) = mpsc::channel();
        let writer = std::thread::spawn(move || {
            wait.recv().unwrap();
            result
                .send(
                    late.write_embedding_vectors(&input, &vectors(&input))
                        .map_err(|e| e.to_string()),
                )
                .unwrap();
        });
        deleter
            .transaction(|s| {
                let source = s.sources.get_mut("internal-key").unwrap();
                if change == "delete" {
                    source.request = None;
                } else {
                    source.request.as_mut().unwrap()["text"] = json!("并发修改后的来源。");
                }
                entered.send(()).unwrap();
                // The late writer cannot complete while the deleting transaction owns the write lock.
                assert!(matches!(
                    done.recv_timeout(std::time::Duration::from_millis(100)),
                    Err(mpsc::RecvTimeoutError::Timeout)
                ));
                Ok(())
            })
            .unwrap();
        assert!(
            done.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap()
                .unwrap_err()
                .contains(if change == "delete" {
                    "source_unavailable"
                } else {
                    "source_changed"
                })
        );
        writer.join().unwrap();
        assert_eq!(db.count("alice"), 0);
    }
}
