//! Injected provider + real SQLite, never a socket or model.
use super::*;
use crate::chat::{ChatConfig, ChatMemoryService, EnrichmentMode};
use serde_json::json;
use std::{
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicI64, AtomicUsize},
        mpsc,
    },
    time::Duration,
};

struct Db(PathBuf);
impl Db {
    fn new() -> Self {
        Self(
            std::env::temp_dir()
                .join(format!("embedding-worker-{}", crate::diagnostics::new_id()))
                .join("memory.sqlite3"),
        )
    }
    fn open(&self) -> SqliteMemoryStore {
        SqliteMemoryStore::open(&self.0, "worker".into()).unwrap()
    }
    fn seed(&self, values: &[(&str, &str)]) {
        self.open()
            .transaction(|s| {
                for (id, text) in values {
                    s.sources.insert(
                        id.to_string(),
                        serde_json::from_value(json!({
                            "id":id,"session_id":"s","turn_id":id,"observed_at":0,
                            "request":{"text":text,"capture_source":true},"enrichment":"pending",
                            "error":null,"result":null,"base_revision":0
                        }))?,
                    );
                }
                Ok(())
            })
            .unwrap();
    }
    fn vectors(&self) -> i64 {
        self.open()
            .connection
            .query_row("SELECT count(*) FROM source_vectors", [], |r| r.get(0))
            .unwrap()
    }
}
impl Drop for Db {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.0.parent().unwrap());
    }
}

fn model_binding(model: embedding::Model, digest: &str) -> embedding::Binding {
    embedding::Binding::from_tags(
        model,
        &json!({"models":[{"name":model.tag(),"digest":digest.repeat(64)}]}),
    )
    .unwrap()
}
fn vectors(count: usize) -> embedding::Batch {
    let mut v = vec![0.; embedding::DIMENSIONS];
    v[0] = 1.;
    embedding::Batch {
        vectors: vec![v; count],
        input_tokens: Some(count as u64 * 7),
        total_duration_ns: None,
        load_duration_ns: None,
        elapsed_ms: 1.,
    }
}
type Hook = Arc<dyn Fn() + Send + Sync>;
#[derive(Default)]
struct Fake {
    metadata: AtomicUsize,
    batches: Mutex<Vec<Vec<String>>>,
    bad_metadata: AtomicBool,
    bad_source: Mutex<Option<String>>,
    digest: Mutex<Option<String>>,
    on_resolve: Option<Hook>,
    on_embed: Option<Hook>,
}
impl embedding::Provider for Fake {
    fn resolve(
        &self,
        model: embedding::Model,
        _: &embedding::RequestControl,
    ) -> embedding::Result<embedding::Binding> {
        self.metadata.fetch_add(1, Ordering::SeqCst);
        if let Some(hook) = &self.on_resolve {
            hook();
        }
        if self.bad_metadata.load(Ordering::SeqCst) {
            return Err("fake_metadata_failure".into());
        }
        Ok(model_binding(
            model,
            self.digest.lock().unwrap().as_deref().unwrap_or("a"),
        ))
    }
    fn embed(
        &self,
        _: &embedding::Binding,
        inputs: &[String],
        _: &embedding::RequestControl,
    ) -> embedding::Result<embedding::Batch> {
        self.batches.lock().unwrap().push(inputs.to_vec());
        if let Some(hook) = &self.on_embed {
            hook();
        }
        if self
            .bad_source
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|text| inputs.iter().any(|s| s == text))
        {
            return Err("fake_embedding_failure".into());
        }
        Ok(vectors(inputs.len()))
    }
}
fn runner(fake: Arc<Fake>, now: Arc<AtomicI64>) -> Runner {
    Runner::injected(
        embedding::Config {
            enabled: true,
            ..Default::default()
        },
        fake,
        Arc::new(move || now.load(Ordering::SeqCst)),
        Arc::new(|| true),
        Arc::new(AtomicBool::new(false)),
    )
}
fn advance(now: &AtomicI64) {
    now.fetch_add(MIN_BATCH_INTERVAL_MS, Ordering::SeqCst);
}

#[test]
fn embedding_background_bounded_batches_fairness_and_neutral_business_snapshot() {
    let db = Db::new();
    let long = "虚构分段正文。".repeat(2400);
    db.seed(&[("a", &long), ("b", "短来源")]);
    let mut store = db.open();
    let snapshot = serde_json::to_value(store.read().unwrap()).unwrap();
    let fake = Arc::new(Fake::default());
    let now = Arc::new(AtomicI64::new(10_000));
    let run = runner(fake.clone(), now.clone());
    let first = run.once(&mut store).unwrap();
    assert_eq!(first.source_id.as_deref(), Some("a"));
    assert_eq!(first.segments, 16);
    assert_eq!(run.once(&mut store).unwrap().status, "backoff");
    assert_eq!(fake.metadata.load(Ordering::SeqCst), 1);
    advance(&now);
    let second = run.once(&mut store).unwrap();
    assert_eq!(second.source_id.as_deref(), Some("b"));
    advance(&now);
    let third = run.once(&mut store).unwrap();
    assert_eq!(third.source_id.as_deref(), Some("a"));
    assert!(third.segments <= 16);
    assert_eq!(
        snapshot,
        serde_json::to_value(store.read().unwrap()).unwrap()
    );
    assert!(fake.batches.lock().unwrap().iter().all(|b| b.len() <= 16));
    let stats = store.embedding_stats().unwrap();
    assert_eq!(stats.embedding_calls, 3);
    assert_eq!(stats.failed_calls, 0);
    assert!(stats.input_tokens > 0);
    let generative_calls: i64 = store
        .connection
        .query_row("SELECT count(*) FROM model_calls", [], |r| r.get(0))
        .unwrap();
    assert_eq!(generative_calls, 0);
    assert!(run.drain(&mut store, 0).is_err());
    assert!(run.drain(&mut store, 101).is_err());
}

#[test]
fn embedding_background_source_backoff_does_not_starve_other_sources_and_survives_restart() {
    let db = Db::new();
    db.seed(&[("a", "坏来源"), ("b", "好来源")]);
    let fake = Arc::new(Fake::default());
    *fake.bad_source.lock().unwrap() = Some("坏来源".into());
    let now = Arc::new(AtomicI64::new(10_000));
    let run = runner(fake.clone(), now.clone());
    assert_eq!(run.once(&mut db.open()).unwrap().status, "failed");
    advance(&now);
    let good = run.once(&mut db.open()).unwrap();
    assert_eq!(good.status, "completed");
    assert_eq!(good.source_id.as_deref(), Some("b"));
    advance(&now);
    assert_eq!(run.once(&mut db.open()).unwrap().status, "idle");
    assert_eq!(fake.batches.lock().unwrap().len(), 2);
    now.store(40_000, Ordering::SeqCst);
    assert_eq!(
        runner(fake.clone(), now.clone())
            .once(&mut db.open())
            .unwrap()
            .status,
        "failed"
    );
    let control = load_control(&db.open().connection, &"worker".into()).unwrap();
    assert_eq!(control.failures["a"].attempts, 2);
    assert_eq!(control.failures["a"].retry_at_ms, 100_000);
    assert_eq!(db.open().embedding_stats().unwrap().failed_calls, 2);
    assert_eq!(
        [
            backoff(1),
            backoff(2),
            backoff(3),
            backoff(4),
            backoff(5),
            backoff(u32::MAX)
        ],
        [30_000, 60_000, 120_000, 240_000, 300_000, 300_000]
    );
}

#[test]
fn embedding_background_metadata_backoff_and_owner_rate_limit_are_durable() {
    let db = Db::new();
    db.seed(&[("a", "虚构正文")]);
    let fake = Arc::new(Fake::default());
    fake.bad_metadata.store(true, Ordering::SeqCst);
    let now = Arc::new(AtomicI64::new(10_000));
    let run = runner(fake.clone(), now.clone());
    assert_eq!(run.once(&mut db.open()).unwrap().status, "metadata_failed");
    for time in [10_001, 15_000, 39_999] {
        now.store(time, Ordering::SeqCst);
        assert_eq!(run.once(&mut db.open()).unwrap().status, "backoff");
    }
    assert_eq!(fake.metadata.load(Ordering::SeqCst), 1);
    now.store(40_000, Ordering::SeqCst);
    assert_eq!(
        runner(fake.clone(), now)
            .once(&mut db.open())
            .unwrap()
            .status,
        "metadata_failed"
    );
    assert_eq!(db.open().embedding_stats().unwrap().metadata_calls, 2);
    let store = db.open();
    let control = load_control(&store.connection, &store.owner).unwrap();
    assert_eq!(control.next_attempt_ms, 100_000);
}

#[test]
fn embedding_background_two_connections_lease_expiry_fences_old_writes_and_counters() {
    let db = Db::new();
    db.seed(&[("a", "虚构正文")]);
    let mut old = db.open();
    let mut new = db.open();
    let now = Arc::new(AtomicI64::new(10_000));
    let clock: Clock = {
        let now = now.clone();
        Arc::new(move || now.load(Ordering::SeqCst))
    };
    let lease = old.claim_embedding(10_000, clock.clone()).unwrap().unwrap();
    old.begin_embedding_call(&lease, false).unwrap();
    let token = old
        .leased_binding(
            Binding::new(model_binding(embedding::Model::Qwen, "a")).unwrap(),
            &lease,
        )
        .unwrap();
    let input = old.embedding_source_input(&token, "a").unwrap();
    assert_eq!(
        new.claim_embedding(10_000, clock.clone())
            .unwrap()
            .unwrap_err()
            .status,
        "busy"
    );
    now.store(25_000, Ordering::SeqCst);
    let fresh = new.claim_embedding(10_000, clock).unwrap().unwrap();
    let _ = new
        .leased_binding(
            Binding::new(model_binding(embedding::Model::Qwen, "b")).unwrap(),
            &fresh,
        )
        .unwrap();
    assert_eq!(
        old.write_embedding_vectors(&input, &vectors(1).vectors)
            .unwrap_err()
            .to_string(),
        "embedding_lease_lost"
    );
    assert_eq!(
        old.leased_binding(
            Binding::new(model_binding(embedding::Model::Qwen, "a")).unwrap(),
            &lease
        )
        .unwrap_err()
        .to_string(),
        "embedding_lease_lost"
    );
    old.end_embedding_call(false, 23, Some(7)).unwrap();
    let state = load_control(&new.connection, &new.owner).unwrap();
    assert_eq!(state.holder.as_deref(), Some(fresh.holder.as_str()));
    assert_eq!(state.epoch, fresh.epoch);
    assert_eq!(state.stats.embedding_calls, 1);
    assert_eq!(state.stats.completed_calls, 1);
    assert_eq!(state.stats.elapsed_ms, 23);
    assert_eq!(db.vectors(), 0);
}

#[test]
fn embedding_background_restart_recovers_an_interrupted_reserved_call_without_resetting_stats() {
    let db = Db::new();
    db.seed(&[("a", "虚构正文")]);
    let now = Arc::new(AtomicI64::new(10_000));
    {
        let mut store = db.open();
        let lease = store
            .claim_embedding(10_000, {
                let now = now.clone();
                Arc::new(move || now.load(Ordering::SeqCst))
            })
            .unwrap()
            .unwrap();
        store.begin_embedding_call(&lease, true).unwrap();
    }
    let run = runner(Arc::new(Fake::default()), now.clone());
    assert_eq!(run.once(&mut db.open()).unwrap().status, "busy");
    now.store(25_000, Ordering::SeqCst);
    assert_eq!(run.once(&mut db.open()).unwrap().status, "completed");
    let stats = db.open().embedding_stats().unwrap();
    assert_eq!(stats.metadata_calls, 2);
    assert_eq!(stats.completed_calls, 2);
    assert_eq!(stats.embedding_calls, 1);
}

#[test]
fn embedding_background_binding_rebuild_cache_corruption_and_new_source_wake_scan() {
    let db = Db::new();
    db.seed(&[("a", "虚构正文")]);
    let fake = Arc::new(Fake::default());
    let now = Arc::new(AtomicI64::new(10_000));
    let run = runner(fake.clone(), now.clone());
    assert_eq!(run.once(&mut db.open()).unwrap().status, "completed");
    advance(&now);
    assert_eq!(run.once(&mut db.open()).unwrap().status, "idle");
    advance(&now);
    assert_eq!(run.once(&mut db.open()).unwrap().status, "idle");
    assert_eq!(fake.metadata.load(Ordering::SeqCst), 2);
    db.seed(&[("b", "新增来源")]);
    assert_eq!(run.once(&mut db.open()).unwrap().status, "completed");
    advance(&now);
    db.open()
        .connection
        .execute(
            "UPDATE source_vectors SET vector=x'00' WHERE source_id='a'",
            [],
        )
        .unwrap();
    assert_eq!(run.once(&mut db.open()).unwrap().status, "completed");
    advance(&now);
    db.open()
        .connection
        .execute(
            "UPDATE source_vectors SET vector='invalid-type' WHERE source_id='a'",
            [],
        )
        .unwrap();
    assert_eq!(run.once(&mut db.open()).unwrap().status, "completed");
    *fake.digest.lock().unwrap() = Some("b".into());
    now.store(80_000, Ordering::SeqCst);
    assert_eq!(run.once(&mut db.open()).unwrap().status, "completed");
    assert_eq!(db.vectors(), 3); // one source in the new namespace; old namespace retained until complete.
    advance(&now);
    assert_eq!(run.once(&mut db.open()).unwrap().status, "completed");
    advance(&now);
    assert_eq!(run.once(&mut db.open()).unwrap().status, "idle");
    assert_eq!(db.vectors(), 2);
    let store = db.open();
    let token = store.embedding_binding().unwrap().unwrap();
    assert_eq!(token.binding.embedding.digest, "b".repeat(64));
}

#[test]
fn embedding_background_disabled_bootstrap_offline_and_deny_are_zero_requests() {
    let db = Db::new();
    db.seed(&[("a", "虚构正文")]);
    let fake = Arc::new(Fake::default());
    let mut config = ChatConfig {
        enrichment_mode: EnrichmentMode::Off,
        ..Default::default()
    };
    config.embedding.enabled = true;
    let mut service = ChatMemoryService::new(db.open(), config.clone()).unwrap();
    assert!(
        !embedding::runtime_network_allowed(),
        "run injected tests offline"
    );
    assert!(service.start_embedding_background().unwrap().is_none()); // test process is offline/deny, never live
    let denied = Runner::injected(
        config.embedding.clone(),
        fake.clone(),
        Arc::new(|| 10_000),
        Arc::new(|| false),
        Arc::new(AtomicBool::new(false)),
    );
    assert_eq!(denied.once(&mut db.open()).unwrap().status, "disabled");
    for args in [
        json!({"query":"全局回答要求","bootstrap":true,"request_id":"boot"}),
        json!({"query":"虚构查询","offline":true,"request_id":"offline"}),
    ] {
        service.call("memory_recall", args, || false).unwrap();
    }
    assert_eq!(fake.metadata.load(Ordering::SeqCst), 0);
    assert!(fake.batches.lock().unwrap().is_empty());
    assert_eq!(service.drain_embeddings(1).unwrap()["attempted_batches"], 0);
    let exists: bool = db
        .open()
        .connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='source_embedding_control')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!exists);
}

#[test]
fn embedding_background_shutdown_does_not_block_foreground_and_discards_late_result() {
    let db = Db::new();
    db.seed(&[("a", "虚构正文")]);
    let (entered, rx) = mpsc::channel();
    let (release, held) = mpsc::channel();
    let held = Mutex::new(held);
    let fake = Arc::new(Fake {
        on_embed: Some(Arc::new(move || {
            entered.send(()).unwrap();
            held.lock().unwrap().recv().unwrap();
        })),
        ..Default::default()
    });
    let now = Arc::new(AtomicI64::new(10_000));
    let run = runner(fake, now);
    let stopped = run.stopped.clone();
    let mut config = ChatConfig {
        enrichment_mode: EnrichmentMode::Off,
        ..Default::default()
    };
    config.embedding.enabled = true;
    let mut service = ChatMemoryService::new(db.open(), config).unwrap();
    assert!(service.start_background().unwrap().is_none());
    let worker = service.spawn_embedding(run, stopped).unwrap();
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let start = Instant::now();
    service
        .call(
            "memory_recall",
            json!({"query":"虚构查询","request_id":"read","offline":true}),
            || false,
        )
        .unwrap();
    assert!(start.elapsed() < Duration::from_secs(1));
    let start = Instant::now();
    drop(worker);
    assert!(start.elapsed() < Duration::from_millis(100));
    release.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while db.open().embedding_stats().unwrap().discarded_results < 1 && Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert_eq!(db.vectors(), 0);
    let store = db.open();
    let control = load_control(&store.connection, &store.owner).unwrap();
    assert_eq!(control.stats.discarded_results, 1);
    assert!(control.holder.is_none());
}

#[test]
fn embedding_background_real_delete_and_body_edit_reject_late_outputs() {
    for delete in [false, true] {
        let db = Db::new();
        db.seed(&[("a", "原始虚构正文")]);
        let (entered, rx) = mpsc::channel();
        let (release, held) = mpsc::channel();
        let held = Mutex::new(held);
        let fake = Arc::new(Fake {
            on_embed: Some(Arc::new(move || {
                entered.send(()).unwrap();
                held.lock().unwrap().recv().unwrap();
            })),
            ..Default::default()
        });
        let run = runner(fake, Arc::new(AtomicI64::new(10_000)));
        let path = db.0.clone();
        let join = std::thread::spawn(move || {
            run.once(&mut SqliteMemoryStore::open_operation(path, "worker".into()).unwrap())
                .unwrap()
        });
        rx.recv_timeout(Duration::from_secs(5)).unwrap();
        if delete {
            let mut service = ChatMemoryService::new(
                db.open(),
                ChatConfig {
                    enrichment_mode: EnrichmentMode::Off,
                    ..Default::default()
                },
            )
            .unwrap();
            let reply = service
                .call("memory_forget", json!({"source_id":"a"}), || false)
                .unwrap();
            assert!(reply["removed"].as_bool().unwrap());
        } else {
            db.seed(&[("a", "修改后的虚构正文")]);
        }
        release.send(()).unwrap();
        assert_eq!(join.join().unwrap().status, "discarded");
        assert_eq!(db.vectors(), 0);
        assert_eq!(db.open().embedding_stats().unwrap().discarded_results, 1);
    }
}

#[test]
fn embedding_background_process_helper() {
    let Ok(path) = std::env::var("AF184_TEST_DB") else {
        return;
    };
    let time: i64 = std::env::var("AF184_TEST_TIME").unwrap().parse().unwrap();
    let run = runner(Arc::new(Fake::default()), Arc::new(AtomicI64::new(time)));
    let mut store = SqliteMemoryStore::open_operation(path, "worker".into()).unwrap();
    let status = run.once(&mut store).unwrap().status;
    assert_eq!(status, std::env::var("AF184_TEST_STATUS").unwrap());
}

#[test]
fn embedding_background_multiple_processes_share_lease_and_cadence() {
    let db = Db::new();
    db.seed(&[("a", "进程竞争的虚构正文")]);
    let mut old = db.open();
    let lease = old
        .claim_embedding(10_000, Arc::new(|| 10_000))
        .unwrap()
        .unwrap();
    let token = old
        .leased_binding(
            Binding::new(model_binding(embedding::Model::Qwen, "a")).unwrap(),
            &lease,
        )
        .unwrap();
    let input = old.embedding_source_input(&token, "a").unwrap();
    for (now, expected) in [(10_000, "busy"), (25_000, "completed"), (25_001, "backoff")] {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "embedding_index::background_tests::embedding_background_process_helper",
                "--nocapture",
            ])
            .env("AF184_TEST_DB", &db.0)
            .env("AF184_TEST_TIME", now.to_string())
            .env("AF184_TEST_STATUS", expected)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stdout)
        );
    }
    assert_eq!(
        old.write_embedding_vectors(&input, &vectors(1).vectors)
            .unwrap_err()
            .to_string(),
        "embedding_lease_lost"
    );
    assert_eq!(db.vectors(), 1);
    let stats = old.embedding_stats().unwrap();
    assert_eq!(stats.embedding_calls, 1);
    assert_eq!(stats.metadata_calls, 1);
}

#[test]
fn embedding_background_capture_wakes_index_even_when_enrichment_is_off() {
    let db = Db::new();
    let (done, rx) = mpsc::channel();
    let fake = Arc::new(Fake {
        on_embed: Some(Arc::new(move || {
            done.send(()).unwrap();
        })),
        ..Default::default()
    });
    let run = runner(fake, Arc::new(AtomicI64::new(10_000)));
    let stopped = run.stopped.clone();
    let mut config = ChatConfig {
        enrichment_mode: EnrichmentMode::Off,
        ..Default::default()
    };
    config.embedding.enabled = true;
    let mut service = ChatMemoryService::new(db.open(), config).unwrap();
    let worker = service.spawn_embedding(run, stopped).unwrap();
    worker.wait_idle();
    let reply=service.call("memory_remember",json!({"text":"这是一条授权的虚构正文。","capture_source":true,"session_id":"wake","turn_id":"one"}),||false).unwrap();
    assert_eq!(reply["source"]["status"], "stored");
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
    worker.wait_idle();
    worker.stop_and_join().unwrap();
    assert_eq!(db.vectors(), 1);
    let state = db.open().read().unwrap();
    assert!(
        crate::MemoryRepository::list_by_owner(&state.repository, &"worker".into())
            .unwrap()
            .is_empty()
    );
}
