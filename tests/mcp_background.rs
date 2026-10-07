//! MCP background execution and foreground recall with local synthetic HTTP only.
use std::process::Command;

#[test]
fn mcp_background_http_deferred_startup_and_foreground_barrier() {
    let output = Command::new("python3")
        .args([
            "tests/mcp_background.py",
            "--binary",
            env!("CARGO_BIN_EXE_pmr-mcp"),
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
