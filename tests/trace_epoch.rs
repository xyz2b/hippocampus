// Synthetic deployment fixture only; no model/provider or real diagnostic directory.
use serde_json::{Value, json};
use std::process::Command;

#[test]
fn af378_cli_requires_explicit_scope_and_initializes_without_writing_database() {
    let dir =
        std::env::temp_dir().join(format!("af378-cli-{}", hippocampus::diagnostics::new_id()));
    std::fs::create_dir(&dir).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let db = dir.join("synthetic.sqlite");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("CREATE TABLE memory_profiles(owner_id TEXT PRIMARY KEY,state_json TEXT NOT NULL); PRAGMA user_version=11;").unwrap();
    let mut state = serde_json::to_value(hippocampus::PersistedMemoryState::default()).unwrap();
    state["sources"]["gone"] = json!({"id":"gone","session_id":"fixture","turn_id":"fixture",
        "observed_at":1,"request":null,"enrichment":"deleted","error":null,"result":null,
        "base_revision":0,"index":{},"task":{}});
    conn.execute(
        "INSERT INTO memory_profiles VALUES('fixture-owner',?)",
        [state.to_string()],
    )
    .unwrap();
    drop(conn);
    let before = std::fs::read(&db).unwrap();
    let command = || {
        let mut c = Command::new(env!("CARGO_BIN_EXE_hippo-mcp"));
        c.env("HIPPOCAMPUS_TRACE_DIR", &dir)
            .env("HIPPOCAMPUS_MODEL_NETWORK", "deny")
            .arg("--init-trace-epoch");
        c
    };
    assert!(!command().output().unwrap().status.success());
    assert!(
        !command()
            .arg("--db")
            .arg(&db)
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(
        !command()
            .args(["--owner", "fixture-owner"])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(
        !command()
            .arg("--db")
            .arg(&db)
            .args(["--owner", "fixture-owner"])
            .env_remove("HIPPOCAMPUS_TRACE_DIR")
            .output()
            .unwrap()
            .status
            .success()
    );
    std::fs::write(dir.join("trace-old.jsonl"), b"fixture").unwrap();
    for status in ["initialized", "already_initialized"] {
        let output = command()
            .arg("--db")
            .arg(&db)
            .args(["--owner", "fixture-owner"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["status"], status);
        assert_eq!(result["cleanup_status"], "complete");
    }
    assert!(!dir.join("trace-old.jsonl").exists());
    assert_eq!(std::fs::read(&db).unwrap(), before);
    std::fs::remove_dir_all(dir).unwrap();
}
