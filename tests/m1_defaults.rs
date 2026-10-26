//! M1 default cutover: real SQLite migration/read protection and model-free host entry checks.
use hippocampus::{
    chat::{ChatConfig, ChatMemoryService},
    *,
};
use serde_json::{Value, json};
use std::path::PathBuf;

struct Db(PathBuf);
impl Db {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("m1-defaults-{}", diagnostics::new_id()));
        std::fs::create_dir(&dir).unwrap();
        Self(dir.join("memory.sqlite3"))
    }
    fn open(&self, owner: &str) -> SqliteMemoryStore {
        SqliteMemoryStore::open(&self.0, owner.into()).unwrap()
    }
}
impl Drop for Db {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.0.parent().unwrap());
    }
}

fn assert_fields_preserved(before: &Value, after: &Value) {
    match before {
        Value::Object(fields) => {
            for (key, value) in fields {
                assert!(after.get(key).is_some(), "lost field {key}");
                assert_fields_preserved(value, &after[key]);
            }
        }
        _ => assert_eq!(after, before),
    }
}

#[test]
fn m1_old_v1_to_v8_profiles_survive_migration_reopen_and_save() {
    for version in 1..=8 {
        let db = Db::new();
        let connection = rusqlite::Connection::open(&db.0).unwrap();
        if version <= 4 {
            connection.execute_batch("CREATE TABLE memory_profiles(owner_id TEXT PRIMARY KEY, state_json TEXT NOT NULL)").unwrap();
            let profile: Value =
                serde_json::from_str(include_str!("fixtures/m1_legacy/source-less-profile.json"))
                    .unwrap();
            for owner in ["alice", "bob"] {
                let mut state = profile.clone();
                for m in state["repository"]["memories"]
                    .as_object_mut()
                    .unwrap()
                    .values_mut()
                {
                    m["owner_id"] = json!(owner);
                }
                connection
                    .execute(
                        "INSERT INTO memory_profiles VALUES (?1,?2)",
                        rusqlite::params![owner, state.to_string()],
                    )
                    .unwrap();
            }
            if version >= 3 {
                connection
                    .execute_batch(include_str!("fixtures/m1_legacy/schema-v3.sql"))
                    .unwrap();
            }
            if version >= 4 {
                connection
                    .execute_batch(include_str!("fixtures/m1_legacy/schema-v4-budget.sql"))
                    .unwrap();
            }
            connection
                .pragma_update(None, "user_version", version)
                .unwrap();
        } else {
            let name = match version {
                5 => "v5",
                6 => "v6-baa1ff9",
                7 => "v7-ca16b64",
                _ => "v8-9933ece",
            };
            let sql = std::fs::read_to_string(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/w5_storage")
                    .join(format!("{name}.sql")),
            )
            .unwrap();
            connection.execute_batch(&sql).unwrap();
            // Another owner's valid source-less history must survive a migration of Alice's receipts.
            let mut bob: Value =
                serde_json::from_str(include_str!("fixtures/m1_legacy/source-less-profile.json"))
                    .unwrap();
            for m in bob["repository"]["memories"]
                .as_object_mut()
                .unwrap()
                .values_mut()
            {
                m["owner_id"] = json!("bob");
            }
            connection
                .execute(
                    "INSERT INTO memory_profiles VALUES ('bob',?1)",
                    [bob.to_string()],
                )
                .unwrap();
        }
        let originals: Vec<(String, String)> = connection
            .prepare("SELECT owner_id,state_json FROM memory_profiles ORDER BY owner_id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        drop(connection);
        for (owner, raw) in originals {
            let before: Value = serde_json::from_str(&raw).unwrap();
            for _ in 0..2 {
                let mut store = db.open(&owner);
                let state = serde_json::to_value(store.read().unwrap()).unwrap();
                assert_eq!(
                    state["repository"], before["repository"],
                    "v{version}/{owner}"
                );
                assert_eq!(state["staging"], before["staging"], "v{version}/{owner}");
                if version <= 4 {
                    assert_eq!(state["receipts"], before["receipts"]);
                }
                for field in [
                    "entities",
                    "receipts",
                    "w5_receipts",
                    "relations",
                    "updates",
                    "w4_audits",
                    "w4_checkpoints",
                ] {
                    if let Some(original) = before.get(field) {
                        assert_fields_preserved(original, &state[field]);
                    }
                }
                if let Some(sources) = before.get("sources").and_then(Value::as_object) {
                    for (key, source) in sources {
                        let mut original = source.clone();
                        let mut restored = state["sources"][key].clone();
                        // The derived FTS receipt is rebuilt; business/authorization fields must survive.
                        original.as_object_mut().unwrap().remove("index");
                        restored.as_object_mut().unwrap().remove("index");
                        assert_fields_preserved(&original, &restored);
                    }
                }
                // This transaction serializes and saves the profile even without business edits.
                // Adding an entity would correctly invalidate old running tasks via the authority barrier.
                store.transaction(|_| Ok(())).unwrap();
            }
        }
        assert_eq!(
            rusqlite::Connection::open(&db.0)
                .unwrap()
                .pragma_query_value::<i64, _>(None, "user_version", |r| r.get(0))
                .unwrap(),
            11
        );
    }
}

#[test]
fn m1_legacy_source_less_recall_bootstrap_lifecycle_and_delete_remain_local() {
    let db = Db::new();
    let mut state: Value =
        serde_json::from_str(include_str!("fixtures/m1_legacy/source-less-profile.json")).unwrap();
    let candidate = CandidateMemory::new(
        "alice".into(),
        "self".into(),
        MemoryType::Fact,
        "fact".into(),
        MemoryValue::Text("旧暂存值".into()),
        Timestamp(100),
        Evidence::new(
            "old-pending".into(),
            "old-session".into(),
            "old-turn".into(),
            "旧暂存值",
        ),
    );
    state["staging"] =
        json!({"next_id":1,"pending":{"1":{"id":1,"candidate":candidate,"resolution":"NotFound"}}});
    let connection = rusqlite::Connection::open(&db.0).unwrap();
    connection.execute_batch("CREATE TABLE memory_profiles(owner_id TEXT PRIMARY KEY, state_json TEXT NOT NULL); PRAGMA user_version=1;").unwrap();
    connection
        .execute(
            "INSERT INTO memory_profiles VALUES ('alice',?1)",
            [state.to_string()],
        )
        .unwrap();
    drop(connection);
    let mut service = ChatMemoryService::new(db.open("alice"), ChatConfig::default()).unwrap();
    let boot = service
        .call(
            "memory_recall",
            json!({"query":"全局回答要求","bootstrap":true,"request_id":"boot"}),
            || false,
        )
        .unwrap();
    assert_eq!(boot["version"], "recall.v11");
    assert_eq!(boot["semantic_judgment"], "skipped");
    assert!(boot.to_string().contains("中文"));
    assert!(!boot.to_string().contains("限定场景"));
    assert!(!boot.to_string().contains("广州"));
    let recall = service
        .call_with_global_context(
            "memory_recall",
            json!({"query":"旧记录","request_id":"old-read","offline":true}),
            || false,
        )
        .structured_content
        .unwrap();
    assert_eq!(recall["global_context"]["status"], "available");
    let records = recall["recall"]["records"].as_array().unwrap();
    let old = records.iter().find(|r| r["id"] == 1).unwrap();
    assert_eq!(old["format"], "legacy");
    assert_eq!(old["grade"], "committed");
    assert!(old["evidence"].as_array().unwrap().is_empty());
    assert!(old["update"].is_null());
    assert!(
        old["gaps"]
            .as_array()
            .unwrap()
            .contains(&json!("sentence_missing"))
    );
    assert!(
        old["gaps"]
            .as_array()
            .unwrap()
            .contains(&json!("qualifiers_unmapped"))
    );
    assert!(
        old["gaps"]
            .as_array()
            .unwrap()
            .contains(&json!("source_missing"))
    );
    assert_eq!(old["data"]["retrieval_count"], 7);
    let listing = service.call("memory_list", json!({}), || false).unwrap();
    assert_eq!(listing["pending_total"], 1);
    assert!(
        !listing["memories"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["id"] == 4)
    );
    let before = serde_json::to_value(db.open("alice").read().unwrap()).unwrap();
    assert!(
        service
            .call("memory_clarify", json!({}), || false)
            .unwrap_err()
            .to_string()
            .contains("no longer supported")
    );
    assert_eq!(
        serde_json::to_value(db.open("alice").read().unwrap()).unwrap(),
        before
    );
    service
        .call("memory_forget", json!({"memory_id":1}), || false)
        .unwrap();
    service
        .call("memory_forget", json!({"staged_id":1}), || false)
        .unwrap();
    assert!(
        service
            .call(
                "memory_recall",
                json!({"query":"旧记录","request_id":"old-read","offline":true}),
                || false
            )
            .is_err()
    );
    let fresh = service
        .call(
            "memory_recall",
            json!({"query":"旧记录","request_id":"after-delete","offline":true}),
            || false,
        )
        .unwrap();
    assert!(!fresh.to_string().contains("广州"));
    assert!(db.open("alice").read().unwrap().staging.is_empty());
}

#[test]
fn m1_default_capture_permission_version_rejection_and_immediate_recall() {
    let db = Db::new();
    let mut service =
        ChatMemoryService::new(db.open("alice"), serde_json::from_value(json!({})).unwrap())
            .unwrap();
    assert!(service.start_background().unwrap().is_none()); // no budget authorizes no worker
    for value in [
        json!({"text":"我有一把蓝伞","session_id":"s","turn_id":"t"}),
        json!({"text":"我有一把蓝伞","session_id":"s","turn_id":"t","capture_source":false}),
    ] {
        assert!(service.call("memory_remember", value, || false).is_err());
        assert!(db.open("alice").read().unwrap().sources.is_empty());
    }
    for name in ["memory_recall", "memory_prepare", "memory_validate_answer"] {
        for version in ["answer_contract.v1", "recall.v3", "recall.v4", "recall.v5"] {
            let before = serde_json::to_value(db.open("alice").read().unwrap()).unwrap();
            let arguments = match name {
                "memory_prepare" => {
                    json!({"user_turn":{"text":"我有一把蓝伞","session_id":"s","turn_id":"v3-rejected","capture_source":true},"offline":true})
                }
                "memory_validate_answer" => {
                    json!({"query":"旧记录","draft_answer":"旧记录","source_ids":[],"request_id":"v3-rejected"})
                }
                _ => json!({"query":"全局回答要求","bootstrap":true,"request_id":"v3-rejected"}),
            };
            let mut arguments = arguments;
            arguments["recall_version"] = json!(version);
            assert_eq!(
                service
                    .call(name, arguments, || false)
                    .unwrap_err()
                    .to_string(),
                "contract_version_unsupported",
                "{name}/{version}"
            );
            assert_eq!(
                serde_json::to_value(db.open("alice").read().unwrap()).unwrap(),
                before
            );
        }
        assert!(db.open("alice").read().unwrap().sources.is_empty());
    }
    assert!(
        service
            .call(
                "memory_recall",
                json!({"query":"全局回答要求","bootstrap":true}),
                || false
            )
            .unwrap_err()
            .to_string()
            .contains("request_id")
    );
    let input = json!({"user_turn":{"text":"我有一把蓝伞","session_id":"s","turn_id":"t","capture_source":true},"offline":true});
    let prepared = service
        .call("memory_prepare", input.clone(), || false)
        .unwrap();
    assert_eq!(prepared["source"]["status"], "stored");
    assert_eq!(prepared["enrichment"]["status"], "queued");
    assert_eq!(prepared["recall"]["version"], "recall.v11");
    assert!(prepared["recall"].to_string().contains("我有一把蓝伞"));
    assert_eq!(
        service.call("memory_prepare", input, || false).unwrap(),
        prepared
    );
    let state = db.open("alice").read().unwrap();
    assert_eq!(state.sources.len(), 1);
    assert_eq!(
        state
            .sources
            .values()
            .next()
            .unwrap()
            .task
            .processing_version
            .as_deref(),
        Some(W4_PROCESSING_VERSION)
    );
    assert_eq!(
        rusqlite::Connection::open(&db.0)
            .unwrap()
            .query_row::<i64, _, _>("SELECT count(*) FROM model_calls", [], |r| r.get(0))
            .unwrap(),
        0
    );
}

#[test]
fn m1_legacy_file_configuration_is_rejected_before_creating_database() {
    for field in ["write_pipeline", "recall_pipeline"] {
        let db = Db::new();
        let config = db.0.with_extension("json");
        std::fs::write(&config, json!({field:"legacy"}).to_string()).unwrap();
        let result = std::process::Command::new(env!("CARGO_BIN_EXE_pmr-mcp"))
            .args([
                "--db",
                db.0.to_str().unwrap(),
                "--config",
                config.to_str().unwrap(),
            ])
            .env("PMR_MODEL_NETWORK", "deny")
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains("no longer supported"));
        assert!(!db.0.exists());
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            json!({field:"legacy"}).to_string()
        );
    }
}

#[test]
fn m1_default_remember_keeps_committed_raw_receipt_when_global_snapshot_breaks() {
    let db = Db::new();
    let mut service = ChatMemoryService::new(
        db.open("alice"),
        ChatConfig {
            enrichment_mode: hippocampus::chat::EnrichmentMode::Off,
            ..Default::default()
        },
    )
    .unwrap();
    rusqlite::Connection::open(&db.0).unwrap().execute_batch("CREATE TRIGGER break_context AFTER UPDATE ON memory_profiles BEGIN UPDATE memory_profiles SET state_json='invalid-json' WHERE owner_id=NEW.owner_id; END").unwrap();
    let response = service.call_with_global_context(
        "memory_remember",
        json!({"text":"我有一把蓝伞","session_id":"s","turn_id":"commit","capture_source":true}),
        || false,
    );
    assert_ne!(response.is_error, Some(true));
    let out = response.structured_content.unwrap();
    assert_eq!(out["source"]["status"], "stored");
    assert_eq!(out["enrichment"]["status"], "not_requested");
    assert_eq!(out["global_context"]["status"], "unavailable");
    let connection = rusqlite::Connection::open(&db.0).unwrap();
    assert_eq!(
        connection
            .query_row::<i64, _, _>("SELECT count(*) FROM model_calls", [], |r| r.get(0))
            .unwrap(),
        0
    );
    assert_eq!(
        connection
            .query_row::<String, _, _>(
                "SELECT state_json FROM memory_profiles WHERE owner_id='alice'",
                [],
                |r| r.get(0)
            )
            .unwrap(),
        "invalid-json"
    );
}

#[test]
fn m1_missing_pro_endpoint_fails_online_before_any_physical_send() {
    let db = Db::new();
    let config = db.0.with_extension("json");
    std::fs::write(
        &config,
        r#"{"enrichment_mode":"off","model_budget":{"group_id":"missing-pro","max_calls":4}}"#,
    )
    .unwrap();
    let arguments = db.0.with_extension("args.json");
    std::fs::write(
        &arguments,
        r#"{"query":"我住在哪？","request_id":"online"}"#,
    )
    .unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_pmr-mcp"))
        .args([
            "--db",
            db.0.to_str().unwrap(),
            "--owner",
            "alice",
            "--config",
            config.to_str().unwrap(),
            "--call",
            "memory_recall",
            "--arguments",
            arguments.to_str().unwrap(),
        ])
        .env_remove("DEEPSEEK_PRO_MODEL")
        .env("DEEPSEEK_BASE_URL", "http://127.0.0.1:1/v1")
        .env("DEEPSEEK_API_KEY", "synthetic-unused")
        .env("PMR_MODEL_NETWORK", "offline")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("DEEPSEEK_PRO_MODEL"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        rusqlite::Connection::open(&db.0)
            .unwrap()
            .query_row::<i64, _, _>("SELECT count(*) FROM model_calls", [], |r| r.get(0))
            .unwrap(),
        0
    );
}

#[test]
fn m1_retired_recall_config_is_rejected_before_database_open_without_rewriting() {
    for (field, value) in [
        ("retrieval_mode", json!("local")),
        ("local_recall_enabled", json!(false)),
        ("local_recall_budget", json!({})),
        ("is_a", json!([])),
    ] {
        let db = Db::new();
        let config = db.0.with_extension("json");
        let original = json!({field:value}).to_string();
        std::fs::write(&config, &original).unwrap();
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_pmr-mcp"))
            .args([
                "--db",
                db.0.to_str().unwrap(),
                "--config",
                config.to_str().unwrap(),
            ])
            .env("PMR_MODEL_NETWORK", "deny")
            .output()
            .unwrap();
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains(field) && error.contains("retired"),
            "{error}"
        );
        assert!(!db.0.exists());
        assert_eq!(std::fs::read_to_string(config).unwrap(), original);
    }
}
