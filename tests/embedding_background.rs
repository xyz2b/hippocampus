//! CLI maintenance and opt-in defaults; no provider/port/model.
use serde_json::{Value, json};
use std::{path::PathBuf, process::Command};

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "embedding-cli-{}",
            hippocampus::diagnostics::new_id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn cli(args: &[&str], mode: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_pmr-mcp"))
        .args(args)
        .env("PMR_MODEL_NETWORK", mode)
        .output()
        .unwrap()
}
#[test]
fn embedding_cli_prints_opt_in_defaults() {
    let output = cli(&["--print-default-config"], "deny");
    assert!(output.status.success());
    let config: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        config["embedding"],
        json!({"enabled":false,"model":"qwen3-embedding:0.6b","base_url":"http://127.0.0.1:11434","batch":16,"query_timeout_ms":10000})
    );
    serde_json::from_value::<hippocampus::chat::ChatConfig>(config)
        .unwrap()
        .validate()
        .unwrap();
}
#[test]
fn embedding_cli_requires_bounds_but_no_generative_budget_and_denies_requests_offline() {
    let root = Root::new();
    let cfg = root.0.join("config.json");
    let db = root.0.join("memory.sqlite3");
    std::fs::write(
        &cfg,
        serde_json::to_vec(&json!({"enrichment_mode":"off","embedding":{"enabled":true}})).unwrap(),
    )
    .unwrap();
    let db_arg = db.to_str().unwrap();
    let cfg_arg = cfg.to_str().unwrap();
    for (args, error) in [
        (
            vec!["--drain-embeddings", "--max-batches", "1"],
            "explicit --db",
        ),
        (
            vec![
                "--db",
                db_arg,
                "--owner",
                "worker",
                "--config",
                cfg_arg,
                "--drain-embeddings",
                "--max-batches",
                "0",
            ],
            "between 1 and 100",
        ),
        (
            vec![
                "--db",
                db_arg,
                "--owner",
                "worker",
                "--config",
                cfg_arg,
                "--drain-embeddings",
                "--max-batches",
                "101",
            ],
            "between 1 and 100",
        ),
        (
            vec![
                "--db",
                db_arg,
                "--owner",
                "worker",
                "--config",
                cfg_arg,
                "--drain-embeddings",
                "--max-batches",
                "1",
                "--enrich-source",
                "a",
            ],
            "another command",
        ),
    ] {
        let result = cli(&args, "deny");
        assert!(!result.status.success());
        assert!(
            String::from_utf8_lossy(&result.stderr).contains(error),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(!db.exists());
    }
    for gate in ["offline", "deny"] {
        let result = cli(
            &[
                "--db",
                db_arg,
                "--owner",
                "worker",
                "--config",
                cfg_arg,
                "--drain-embeddings",
                "--max-batches",
                "2",
            ],
            gate,
        );
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let output: Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(output["status"], "disabled");
        assert_eq!(output["attempted_batches"], 0);
    }
    let connection = rusqlite::Connection::open(&db).unwrap();
    let tables: i64 = connection
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name='source_embedding_control'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tables, 0);
    let calls: i64 = connection
        .query_row("SELECT count(*) FROM model_calls", [], |r| r.get(0))
        .unwrap();
    assert_eq!(calls, 0);
}
