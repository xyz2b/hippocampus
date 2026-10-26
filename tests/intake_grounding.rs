#[test]
fn frozen_candidates_need_independent_source_review_through_mcp() {
    let output = std::process::Command::new("python3")
        .args([
            "tests/intake_grounding_process.py",
            "--binary",
            env!("CARGO_BIN_EXE_hippo-mcp"),
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("python3 is required for the loopback MCP grounding checks");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
