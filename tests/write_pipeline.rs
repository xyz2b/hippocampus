//! ④-5：配置、T1 绑定、跨进程 CLI 和 SQLite 重开。
use hippocampus::chat::{ChatConfig, ChatMemoryService, WritePipeline};
use hippocampus::*;
use serde_json::{Value, json};
use std::{path::PathBuf, process::Command};

struct Db(PathBuf);
impl Db {
    fn new() -> Self {
        Self(
            std::env::temp_dir()
                .join(format!("write-pipeline-{}", diagnostics::new_id()))
                .join("memory.sqlite3"),
        )
    }
    fn open(&self) -> SqliteMemoryStore {
        SqliteMemoryStore::open(&self.0, UserId::from("alice")).unwrap()
    }
    fn service(&self, pipeline: &str, mode: &str) -> ChatMemoryService {
        ChatMemoryService::new(self.open(), config(pipeline, mode)).unwrap()
    }
    fn source(&self, turn: &str) -> (String, SourceRecord) {
        self.open()
            .read()
            .unwrap()
            .sources
            .into_iter()
            .find(|(_, s)| s.turn_id == turn)
            .unwrap()
    }
}
impl Drop for Db {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.0.parent().unwrap());
    }
}
fn config(_pipeline: &str, mode: &str) -> ChatConfig {
    serde_json::from_value(json!({"enrichment_mode":mode,
        "model_budget":{"group_id":"write-pipeline-fixture","max_calls":20}}))
    .unwrap()
}

fn turn(id: &str) -> Value {
    json!({"session_id":"pipeline","turn_id":id,"text":"我正在研究数据库索引。","capture_source":true,"observed_at":1790899200})
}
fn save(service: &mut ChatMemoryService, id: &str) -> Value {
    service.call("memory_remember", turn(id), || false).unwrap()
}
fn frozen(result: &Value, status: &str) {
    assert_eq!(result.as_object().unwrap().len(), 2, "{result}");
    assert_eq!(result["source"]["status"], "stored");
    assert_eq!(result["enrichment"]["status"], status);
    assert_eq!(result["enrichment"].as_object().unwrap().len(), 4);
    assert!(result["enrichment"]["candidates"].is_array());
    assert!(result["enrichment"]["memory_requests"].is_array());
    assert_eq!(result["enrichment"]["retention_pending"], false);
}

#[test]
fn write_pipeline_defaults_and_strict_configuration() {
    let defaults: ChatConfig = serde_json::from_str("{}").unwrap();
    assert_eq!(defaults.write_pipeline, WritePipeline::W4RuntimeV1);
    defaults.validate().unwrap();
    assert_eq!(
        serde_json::to_value(defaults).unwrap()["write_pipeline"],
        "w4_runtime_v1"
    );
    for value in [json!("legacy"), json!("unknown"), json!(null), json!(true)] {
        assert!(serde_json::from_value::<ChatConfig>(json!({"write_pipeline":value})).is_err());
    }
    assert!(
        serde_json::from_str::<ChatConfig>(
            r#"{"write_pipeline":"legacy","write_pipeline":"w4_runtime_v1"}"#
        )
        .is_err()
    );
    for mode in ["off", "deferred", "inline"] {
        config("w4_runtime_v1", mode).validate().unwrap();
    }
}

#[test]
fn write_pipeline_cli_inline_captures_and_queues_without_starting_worker() {
    let db = Db::new();
    let settings = db.0.with_extension("json");
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    std::fs::write(
        &settings,
        r#"{"write_pipeline":"w4_runtime_v1","enrichment_mode":"inline"}"#,
    )
    .unwrap();
    let arguments = db.0.with_extension("turn.json");
    std::fs::write(&arguments, turn("inline-cli").to_string()).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_pmr-mcp"))
        .args([
            "--db",
            db.0.to_str().unwrap(),
            "--owner",
            "alice",
            "--config",
            settings.to_str().unwrap(),
            "--call",
            "memory_remember",
            "--arguments",
            arguments.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    frozen(&serde_json::from_slice(&output.stdout).unwrap(), "queued");
    let (_, source) = db.source("inline-cli");
    assert_eq!(source.task.accepted_mode.as_deref(), Some("inline"));
    assert_eq!(
        source.task.processing_version.as_deref(),
        Some(W4_PROCESSING_VERSION)
    );
    assert_eq!(source.task.round, None);
}

#[test]
fn write_pipeline_t1_binds_off_and_deferred_and_requires_capture() {
    let db = Db::new();
    for (mode, status) in [("off", "not_requested"), ("deferred", "queued")] {
        let mut service = db.service("w4_runtime_v1", mode);
        frozen(&save(&mut service, mode), status);
        let (_, source) = db.source(mode);
        assert_eq!(
            source.task.processing_version.as_deref(),
            Some(W4_PROCESSING_VERSION)
        );
        assert_eq!(source.task.accepted_mode.as_deref(), Some(mode));
        assert_eq!(source.task.round, None);
        let before = serde_json::to_value(db.open().read().unwrap()).unwrap();
        let mut request = turn("unauthorized");
        request["capture_source"] = json!(false);
        assert!(
            service
                .call("memory_remember", request, || false)
                .unwrap_err()
                .to_string()
                .contains("capture_source_required")
        );
        assert_eq!(
            serde_json::to_value(db.open().read().unwrap()).unwrap(),
            before
        );
    }
}

#[test]
fn write_pipeline_reopen_and_replay_preserve_each_bound_version() {
    let db = Db::new();
    save(&mut db.service("w4_runtime_v1", "deferred"), "old");
    db.open()
        .transaction(|state| {
            let source = state.sources.values_mut().next().unwrap();
            source.task.processing_version = Some("s5_enrichment_v1".into());
            source.result = Some(json!({"enrichment_mode":"deferred"}));
            Ok(())
        })
        .unwrap();
    let legacy = save(&mut db.service("w4_runtime_v1", "off"), "old");
    assert_eq!(legacy["status"], "queued");
    assert_eq!(legacy["model_calls"], json!([]));
    let new = save(&mut db.service("w4_runtime_v1", "deferred"), "new");
    let before = serde_json::to_value(db.open().read().unwrap()).unwrap();
    let replay = save(&mut db.service("w4_runtime_v1", "inline"), "old");
    assert_eq!(replay["source"], legacy["source"]);
    assert_eq!(replay["enrichment"], legacy["enrichment"]);
    assert_eq!(replay["original_result"], legacy["original_result"]);
    assert_eq!(replay["model_calls"], json!([]));
    assert_eq!(
        replay["index"]["source_version"],
        legacy["index"]["source_version"]
    );
    assert_eq!(
        replay["index"]["source_revision"],
        before["source_revision"]
    );
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["enrichment_mode"], "deferred");
    assert_eq!(save(&mut db.service("w4_runtime_v1", "off"), "new"), new);
    assert_eq!(
        serde_json::to_value(db.open().read().unwrap()).unwrap(),
        before
    );
    assert_eq!(
        db.source("old").1.task.processing_version.as_deref(),
        Some("s5_enrichment_v1")
    );
    assert_eq!(
        db.source("new").1.task.processing_version.as_deref(),
        Some(W4_PROCESSING_VERSION)
    );
    assert_eq!(db.open().read().unwrap().sources.len(), 2);
    let (_, source) = db.source("old");
    assert!(
        db.service("w4_runtime_v1", "inline")
            .enrich_source(&source.id)
            .unwrap_err()
            .to_string()
            .contains("unsupported_processing_version")
    );
    assert_eq!(
        serde_json::to_value(db.open().read().unwrap()).unwrap(),
        before
    );
}

#[test]
fn write_pipeline_terminal_and_active_rounds_do_not_rejudge_or_call_models() {
    let db = Db::new();
    save(&mut db.service("w4_runtime_v1", "deferred"), "terminal");
    let (key, source) = db.source("terminal");
    w4_start_round(&mut db.open(), &key, "round-zero", false).unwrap();
    for status in [
        "needs_rejudge",
        "committed",
        "no_candidates_unreviewed",
        "awaiting_confirmation",
    ] {
        db.open()
            .transaction(|state| {
                state.sources.get_mut(&key).unwrap().enrichment = status.into();
                Ok(())
            })
            .unwrap();
        let before = serde_json::to_value(db.open().read().unwrap()).unwrap();
        frozen(
            &db.service("legacy", "deferred")
                .enrich_source(&source.id)
                .unwrap(),
            status,
        );
        frozen(
            &save(&mut db.service("legacy", "inline"), "terminal"),
            status,
        );
        assert_eq!(
            db.service("legacy", "deferred")
                .drain_enrichment(1)
                .unwrap()["attempted"],
            0
        );
        assert_eq!(
            serde_json::to_value(db.open().read().unwrap()).unwrap(),
            before
        );
    }
    assert_eq!(
        w4_start_round(&mut db.open(), &key, "explicit-rejudge", true).unwrap(),
        1
    );
    let _claim = w4_claim_round(&mut db.open(), &key).unwrap();
    let before = serde_json::to_value(db.open().read().unwrap()).unwrap();
    frozen(
        &db.service("legacy", "off")
            .enrich_source(&source.id)
            .unwrap(),
        "running",
    );
    assert_eq!(
        serde_json::to_value(db.open().read().unwrap()).unwrap(),
        before
    );
}

#[test]
fn write_pipeline_deleted_replay_is_rejected_without_recreating_the_source() {
    let db = Db::new();
    let saved = save(&mut db.service("w4_runtime_v1", "off"), "deleted");
    let mut service = db.service("legacy", "off");
    service
        .call(
            "memory_forget",
            json!({"source_id":saved["source"]["source_id"]}),
            || false,
        )
        .unwrap();
    let before = serde_json::to_value(db.open().read().unwrap()).unwrap();
    assert!(
        service
            .call("memory_remember", turn("deleted"), || false)
            .is_err()
    );
    let response = service.call_with_global_context("memory_remember", turn("deleted"), || false);
    assert_eq!(response.is_error, Some(true));
    let receipt = response.structured_content.unwrap();
    assert_eq!(receipt["source"]["status"], "deleted");
    assert_eq!(receipt["enrichment"]["status"], "deleted");
    assert!(!receipt.to_string().contains("我正在研究数据库索引"));
    assert_eq!(
        serde_json::to_value(db.open().read().unwrap()).unwrap(),
        before
    );
}

#[test]
fn write_pipeline_cli_preserves_the_source_less_order_barrier() {
    let db = Db::new();
    save(&mut db.service("w4_runtime_v1", "deferred"), "older");
    db.open()
        .transaction(|state| {
            let mut entity = state.entities.get(&EntityId::from("self")).unwrap().clone();
            entity.aliases.push("后续本人称呼".into());
            state.entities.upsert(entity);
            Ok(())
        })
        .unwrap();
    let barrier = db.open().read().unwrap().source_less_barrier_sequence;
    assert!(barrier >= db.source("older").1.received_sequence);
    assert_eq!(
        db.service("legacy", "deferred")
            .drain_enrichment(1)
            .unwrap()["attempted"],
        0
    );
    let source_id = db.source("older").1.id;
    let settings = db.0.with_extension("json");
    std::fs::write(
        &settings,
        serde_json::to_vec(&config("w4_runtime_v1", "deferred")).unwrap(),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_pmr-mcp"))
        .args([
            "--db",
            db.0.to_str().unwrap(),
            "--owner",
            "alice",
            "--config",
            settings.to_str().unwrap(),
            "--enrich-source",
            &source_id,
        ])
        .env("DEEPSEEK_BASE_URL", "http://127.0.0.1:1/v1")
        .env("DEEPSEEK_API_KEY", "synthetic-test-key")
        .env("PMR_MODEL_NETWORK", "deny")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let out: Value = serde_json::from_slice(&output.stdout).unwrap();
    frozen(&out, "needs_rejudge");
    assert!(
        db.open()
            .read()
            .unwrap()
            .repository
            .list_by_owner(&UserId::from("alice"))
            .unwrap()
            .is_empty()
    );
    let (_, source) = db.source("older");
    assert_eq!(source.task.round, Some(0));
    assert!(source.task.attempt_id.is_none());
    assert_eq!(
        rusqlite::Connection::open(&db.0)
            .unwrap()
            .query_row("SELECT count(*) FROM model_calls", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        db.service("legacy", "deferred")
            .drain_enrichment(1)
            .unwrap()["attempted"],
        0
    );
    let (key, _) = db.source("older");
    assert_eq!(
        w4_start_round(&mut db.open(), &key, "explicit-rejudge-after-barrier", true).unwrap(),
        1
    );
}

#[test]
fn write_pipeline_order_barrier_precedes_unavailable_provider_setup() {
    for missing in ["credentials", "pro_model"] {
        let db = Db::new();
        save(&mut db.service("w4_runtime_v1", "deferred"), "older");
        db.open()
            .transaction(|state| {
                let mut entity = state.entities.get(&EntityId::from("self")).unwrap().clone();
                entity.aliases.push("后续本人称呼".into());
                state.entities.upsert(entity);
                Ok(())
            })
            .unwrap();
        let settings = db.0.with_extension("json");
        std::fs::write(
            &settings,
            serde_json::to_vec(&config("w4_runtime_v1", "deferred")).unwrap(),
        )
        .unwrap();
        let (_, source) = db.source("older");
        let mut command = Command::new(env!("CARGO_BIN_EXE_pmr-mcp"));
        command
            .args([
                "--db",
                db.0.to_str().unwrap(),
                "--owner",
                "alice",
                "--config",
                settings.to_str().unwrap(),
                "--enrich-source",
                &source.id,
            ])
            .env("PMR_MODEL_NETWORK", "offline")
            .env_remove("DEEPSEEK_PRO_MODEL");
        if missing == "credentials" {
            command
                .env_remove("DEEPSEEK_BASE_URL")
                .env_remove("DEEPSEEK_API_KEY");
        } else {
            command
                .env("DEEPSEEK_BASE_URL", "http://127.0.0.1:1/v1")
                .env("DEEPSEEK_API_KEY", "synthetic-unused");
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{missing}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        frozen(
            &serde_json::from_slice(&output.stdout).unwrap(),
            "needs_rejudge",
        );
        let state = db.open().read().unwrap();
        assert!(
            state
                .repository
                .list_by_owner(&UserId::from("alice"))
                .unwrap()
                .is_empty()
        );
        let (_, source) = db.source("older");
        assert_eq!(source.task.round, Some(0));
        assert_eq!(source.error.as_deref(), Some("needs_rejudge"));
        assert!(source.task.attempt_id.is_none() && source.task.lease_until.is_none());
        assert_eq!(
            rusqlite::Connection::open(&db.0)
                .unwrap()
                .query_row("SELECT count(*) FROM model_calls", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}

#[test]
fn write_pipeline_missing_credentials_do_not_repeat_the_first_job() {
    let db = Db::new();
    save(&mut db.service("w4_runtime_v1", "deferred"), "first");
    save(&mut db.service("w4_runtime_v1", "deferred"), "second");
    let settings = db.0.with_extension("json");
    std::fs::write(
        &settings,
        serde_json::to_vec(&config("w4_runtime_v1", "deferred")).unwrap(),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_pmr-mcp"))
        .args([
            "--db",
            db.0.to_str().unwrap(),
            "--owner",
            "alice",
            "--config",
            settings.to_str().unwrap(),
            "--drain-enrichment",
            "--max-jobs",
            "2",
        ])
        .env_remove("DEEPSEEK_BASE_URL")
        .env_remove("DEEPSEEK_API_KEY")
        .env("PMR_MODEL_NETWORK", "deny")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let out: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(out["attempted"], 2);
    for result in out["results"].as_array().unwrap() {
        frozen(&result["result"], "failed");
    }
    for id in ["first", "second"] {
        let (_, source) = db.source(id);
        assert_eq!(source.enrichment, "failed");
        assert_eq!(source.task.round, Some(0));
        assert_eq!(source.task.attempt_count, 1);
        assert!(source.task.attempt_id.is_none());
    }
}

fn http_cli(mode: &str) {
    let output = Command::new("python3")
        .args([
            "tests/write_pipeline_process.py",
            "--binary",
            env!("CARGO_BIN_EXE_pmr-mcp"),
            "--mode",
            mode,
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
#[test]
fn write_pipeline_http_enrich_source_roundtrip() {
    http_cli("explicit");
}
#[test]
fn write_pipeline_http_new_drain_roundtrip() {
    http_cli("drain");
}

#[test]
fn write_pipeline_default_preserves_old_queued_sources_without_automatic_conversion() {
    let db = Db::new();
    let mut service = db.service("w4_runtime_v1", "deferred");
    let saved = save(&mut service, "legacy-queued");
    db.open()
        .transaction(|state| {
            let source = state.sources.values_mut().next().unwrap();
            source.task.processing_version = Some("s5_enrichment_v1".into());
            source.result = Some(json!({"enrichment_mode":"deferred"}));
            Ok(())
        })
        .unwrap();
    let before = serde_json::to_value(db.open().read().unwrap()).unwrap();
    let mut service = db.service("w4_runtime_v1", "deferred");
    assert_eq!(service.drain_enrichment(2).unwrap()["attempted"], 0);
    assert!(
        service
            .enrich_source(saved["source"]["source_id"].as_str().unwrap())
            .unwrap_err()
            .to_string()
            .contains("unsupported_processing_version")
    );
    assert_eq!(
        serde_json::to_value(db.open().read().unwrap()).unwrap(),
        before
    );
    let out = service
        .call(
            "memory_recall",
            json!({"query":"旧排队原文","request_id":"read-old-queue","offline":true}),
            || false,
        )
        .unwrap();
    assert!(
        out["raw"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["source_id"] == saved["source"]["source_id"])
    );
    assert_eq!(
        rusqlite::Connection::open(&db.0)
            .unwrap()
            .query_row::<i64, _, _>("SELECT count(*) FROM model_calls", [], |r| r.get(0))
            .unwrap(),
        0
    );
}
