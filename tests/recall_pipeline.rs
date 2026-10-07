//! 召回⑤：真实 SQLite、配置、离线 CLI 与 MCP 分发；无真实模型调用。
use hippocampus::chat::{ChatConfig, ChatMemoryService, RecallPipeline};
use hippocampus::*;
use serde_json::{Value, json};
use std::{path::PathBuf, process::Command};
const AT: i64 = 1791000000;
struct Db(PathBuf);
impl Db {
    fn new() -> Self {
        Self(
            std::env::temp_dir()
                .join(format!("recall-dispatch-{}", diagnostics::new_id()))
                .join("memory.sqlite3"),
        )
    }
    fn open(&self) -> SqliteMemoryStore {
        SqliteMemoryStore::open(&self.0, "recall-fixture".into()).unwrap()
    }
    fn service(&self, settings: Value) -> ChatMemoryService {
        ChatMemoryService::new(self.open(), serde_json::from_value(settings).unwrap()).unwrap()
    }
}
impl Drop for Db {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.0.parent().unwrap());
    }
}
fn query(id: &str) -> Value {
    json!({"query":"我记录了哪些内容？","recall_version":"recall.v6","request_id":id,"at":AT,"offline":true})
}
fn turn(id: &str) -> Value {
    json!({"text":"我在学习陶艺。","session_id":"recall-fixture","turn_id":id,"capture_source":true,"observed_at":AT})
}
fn calls(db: &Db) -> i64 {
    rusqlite::Connection::open(&db.0)
        .unwrap()
        .query_row("SELECT count(*) FROM model_calls", [], |r| r.get(0))
        .unwrap()
}
#[test]
fn recall_pipeline_strict_defaults_and_independent_write_switch() {
    let defaults: ChatConfig = serde_json::from_str("{}").unwrap();
    assert_eq!(defaults.recall_pipeline, RecallPipeline::RecallV3);
    assert_eq!(
        defaults.write_pipeline,
        hippocampus::chat::WritePipeline::W4RuntimeV1
    );
    for value in [json!(null), json!(true), json!("typo")] {
        assert!(serde_json::from_value::<ChatConfig>(json!({"recall_pipeline":value})).is_err());
    }
    for value in [
        json!(null),
        json!({"unknown":"x"}),
        json!({"r1":" "}),
        json!({"reasoning_effort":{"r4":"typo"}}),
    ] {
        assert!(serde_json::from_value::<ChatConfig>(json!({"recall_models":value})).is_err());
    }
    assert!(
        serde_json::from_str::<ChatConfig>(
            r#"{"recall_pipeline":"legacy","recall_pipeline":"recall_v3"}"#
        )
        .is_err()
    );
    for (field, value) in [("write_pipeline", "legacy"), ("recall_pipeline", "legacy")] {
        assert!(serde_json::from_value::<ChatConfig>(json!({field:value})).is_err());
    }
    serde_json::from_value::<ChatConfig>(json!({"enrichment_mode":"off"}))
        .unwrap()
        .validate()
        .unwrap();
    let recommended: ChatConfig =
        serde_json::from_str(include_str!("../examples/recall-v3.json")).unwrap();
    recommended.validate().unwrap();
    assert!(recommended.recall_models.is_none());
}
#[test]
fn recall_pipeline_explicit_version_single_output_and_offline_replay() {
    let db = Db::new();
    let mut service = db.service(json!({"enrichment_mode":"off"}));
    service
        .call("memory_remember", turn("saved"), || false)
        .unwrap();
    let out = service
        .call("memory_recall", query("one"), || false)
        .unwrap();
    let frozen: hippocampus::contract::RecallOutput = serde_json::from_value(out.clone()).unwrap();
    assert_eq!(frozen.version, "recall.v6");
    assert!(out.get("answer_contract").is_none());
    assert_eq!(out["semantic_judgment"], "skipped");
    assert_eq!(out["raw"].as_array().unwrap().len(), 1);
    let mut retry = query("one");
    retry.as_object_mut().unwrap().remove("at");
    assert_eq!(service.call("memory_recall", retry, || false).unwrap(), out);
    assert_eq!(db.open().read().unwrap().sources.len(), 1);
    let mut legacy = query("old");
    legacy["recall_version"] = json!("answer_contract.v1");
    legacy.as_object_mut().unwrap().remove("offline");
    assert!(
        service
            .call("memory_recall", legacy, || false)
            .unwrap_err()
            .to_string()
            .contains("contract_version_unsupported")
    );
    let mut bad = query("bad");
    bad["recall_version"] = json!("recall.v999");
    assert!(
        service
            .call("memory_recall", bad, || false)
            .unwrap_err()
            .to_string()
            .contains("contract_version_unsupported")
    );
    let mut missing = query("bad");
    missing.as_object_mut().unwrap().remove("request_id");
    assert!(service.call("memory_recall", missing, || false).is_err());
    assert_eq!(calls(&db), 0);
    drop(service);
    let mut reopened = db.service(json!({"recall_pipeline":"recall_v3"}));
    assert!(
        reopened
            .call("memory_recall", query("one"), || false)
            .unwrap_err()
            .to_string()
            .contains("not_retained")
    );
    assert_eq!(calls(&db), 0);
}
#[test]
fn recall_pipeline_prepare_three_fragments_default_and_explicit_new_write() {
    for settings in [
        json!({"enrichment_mode":"off"}),
        json!({"write_pipeline":"w4_runtime_v1","recall_pipeline":"recall_v3","enrichment_mode":"off"}),
    ] {
        let db = Db::new();
        let mut service = db.service(settings);
        let arguments = json!({"user_turn":turn("prepare"),"offline":true});
        let out = service
            .call("memory_prepare", arguments.clone(), || false)
            .unwrap();
        assert_eq!(out.as_object().unwrap().len(), 3);
        assert_eq!(out["source"]["status"], "stored");
        assert_eq!(out["enrichment"]["status"], "not_requested");
        assert_eq!(out["recall"]["version"], "recall.v6");
        serde_json::from_value::<hippocampus::contract::RecallOutput>(out["recall"].clone())
            .unwrap();
        assert!(out.get("remember").is_none());
        assert!(out["recall"].get("answer_contract").is_none());
        assert_eq!(
            service.call("memory_prepare", arguments, || false).unwrap(),
            out
        );
        let mut changed = json!({"user_turn":turn("prepare"),"offline":true});
        changed["user_turn"]["text"] = json!("我在学习木工。");
        let rejected = service.call("memory_prepare", changed, || false).unwrap();
        assert_eq!(rejected["source"]["status"], "rejected");
        assert_eq!(rejected["recall"]["status"], "not_requested");
        assert_eq!(db.open().read().unwrap().sources.len(), 1);
        assert_eq!(calls(&db), 0);
    }
}
#[test]
fn recall_pipeline_rejected_prepare_does_not_recall_or_claim_storage() {
    let db = Db::new();
    let mut service = db.service(json!({"source_capture_enabled":false,"recall_pipeline":"recall_v3","enrichment_mode":"off"}));
    let out = service
        .call(
            "memory_prepare",
            json!({"user_turn":turn("rejected"),"offline":true}),
            || false,
        )
        .unwrap();
    assert_ne!(out["source"]["status"], "stored");
    assert_eq!(out["recall"]["status"], "not_requested");
    assert!(db.open().read().unwrap().sources.is_empty());
    assert_eq!(calls(&db), 0);
}
#[test]
fn recall_pipeline_offline_validator_rejects_source_only_claim_and_reopen() {
    let db = Db::new();
    let mut service = db.service(json!({"recall_pipeline":"recall_v3","enrichment_mode":"off"}));
    let saved = service
        .call("memory_remember", turn("saved"), || false)
        .unwrap();
    service
        .call("memory_recall", query("validation"), || false)
        .unwrap();
    let validate = json!({"request_id":"validation","query":"我记录了哪些内容？","draft_answer":"我已经成为陶艺专家。",
        "source_ids":[saved["source"]["source_id"]],"at":AT});
    let out = service
        .call("memory_validate_answer", validate.clone(), || false)
        .unwrap();
    assert_eq!(out["version"], "recall.v6");
    assert_eq!(out["approved"], false);
    assert!(out.get("safe_answer").is_none());
    let mut mismatch = validate.clone();
    mismatch["query"] = json!("我是什么专家？");
    assert!(
        service
            .call("memory_validate_answer", mismatch, || false)
            .unwrap_err()
            .to_string()
            .contains("mismatch")
    );
    drop(service);
    let mut reopened = db.service(json!({"recall_pipeline":"recall_v3"}));
    assert!(
        reopened
            .call("memory_validate_answer", validate, || false)
            .unwrap_err()
            .to_string()
            .contains("not_retained")
    );
    assert_eq!(calls(&db), 0);
}
#[test]
fn recall_pipeline_cli_offline_dispatch_and_bootstrap() {
    let db = Db::new();
    db.open();
    let root = db.0.parent().unwrap();
    let config = root.join("config.json");
    let args = root.join("args.json");
    std::fs::write(&config, r#"{"recall_pipeline":"recall_v3"}"#).unwrap();
    for bootstrap in [false, true] {
        let mut request = query(if bootstrap { "bootstrap" } else { "cli" });
        request["bootstrap"] = json!(bootstrap);
        std::fs::write(&args, request.to_string()).unwrap();
        let result = Command::new(env!("CARGO_BIN_EXE_pmr-mcp"))
            .args([
                "--db",
                db.0.to_str().unwrap(),
                "--owner",
                "recall-fixture",
                "--config",
                config.to_str().unwrap(),
                "--call",
                "memory_recall",
                "--arguments",
                args.to_str().unwrap(),
            ])
            .env("PMR_MODEL_NETWORK", "deny")
            .env_remove("DEEPSEEK_API_KEY")
            .env_remove("DEEPSEEK_BASE_URL")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let out: Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(out["version"], "recall.v6");
        assert_eq!(out["semantic_judgment"], "skipped");
        assert!(out.get("answer_contract").is_none());
        if bootstrap {
            assert_eq!(out["current_message"], Value::Null);
            assert_eq!(out["raw"], json!([]));
        }
    }
    assert_eq!(calls(&db), 0);
}
#[test]
fn recall_pipeline_http_cli_end_to_end() {
    let out = Command::new("python3")
        .args([
            "tests/recall_pipeline_process.py",
            "--binary",
            env!("CARGO_BIN_EXE_pmr-mcp"),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn recall_pipeline_explicit_legacy_prepare_and_validation_are_rejected_without_writing() {
    let db = Db::new();
    let mut service = db.service(json!({"enrichment_mode":"off"}));
    for tool in ["memory_prepare", "memory_validate_answer"] {
        assert!(service.call(tool,json!({"user_turn":turn("legacy-override"),"recall_version":"answer_contract.v1","request_id":"old-output","offline":true}),||false).unwrap_err().to_string().contains("contract_version_unsupported"));
    }
    assert!(db.open().read().unwrap().sources.is_empty());
    assert_eq!(calls(&db), 0);
}

#[test]
fn recall_pipeline_prepare_keeps_authorized_source_when_recall_context_exceeds_budget() {
    let db = Db::new();
    let mut service = db.service(json!({"recall_pipeline":"recall_v3","enrichment_mode":"off"}));
    let mut user_turn = turn("long-context");
    user_turn["history"] = json!(vec!["先前用户原话。"; 9]);
    let out = service
        .call(
            "memory_prepare",
            json!({"user_turn":user_turn,"offline":true}),
            || false,
        )
        .unwrap();
    assert_eq!(out["source"]["status"], "stored");
    assert_eq!(out["enrichment"]["status"], "not_requested");
    assert_eq!(out["recall"]["status"], "failed");
    let state = db.open().read().unwrap();
    assert_eq!(state.sources.len(), 1);
    assert_eq!(
        state
            .sources
            .values()
            .next()
            .unwrap()
            .request
            .as_ref()
            .unwrap()["history"]
            .as_array()
            .unwrap()
            .len(),
        9
    );
    assert_eq!(calls(&db), 0);
}

#[test]
fn mcp_online_recall_http_concurrency_barriers() {
    let out = Command::new("python3")
        .args([
            "tests/mcp_recall_concurrency.py",
            "--binary",
            env!("CARGO_BIN_EXE_pmr-mcp"),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
