#[test]
fn mcp_stdio_persistence_roundtrip() {
    let output = std::process::Command::new("python3")
        .args([
            "tests/mcp_smoke.py",
            "--binary",
            env!("CARGO_BIN_EXE_pmr-mcp"),
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("python3 is required for the stdlib MCP integration client");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn mcp_global_context_fallback() {
    let output = std::process::Command::new("python3")
        .args([
            "tests/mcp_global_context.py",
            "--binary",
            env!("CARGO_BIN_EXE_pmr-mcp"),
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("python3 is required for the stdio MCP regression");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn mcp_prepare_saves_before_recall() {
    let output = std::process::Command::new("python3")
        .args([
            "tests/mcp_prepare.py",
            "--binary",
            env!("CARGO_BIN_EXE_pmr-mcp"),
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("python3 is required for the stdio MCP regression");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn s1_diagnostic_evidence() {
    let output = std::process::Command::new("python3")
        .args([
            "tests/s1_trace.py",
            "--binary",
            env!("CARGO_BIN_EXE_pmr-mcp"),
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("python3 and node are required for S1 evidence checks");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn s2_independent_source_capture() {
    let output = std::process::Command::new("python3")
        .args([
            "tests/s2_capture.py",
            "--binary",
            env!("CARGO_BIN_EXE_pmr-mcp"),
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("python3 and node are required for S2 evidence checks");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn s4_local_mcp_and_frozen_development_cases() {
    let output = std::process::Command::new("python3")
        .args([
            "tests/s4_process.py",
            "--binary",
            env!("CARGO_BIN_EXE_pmr-mcp"),
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("python3 and the built Inspector are required for S4 contract checks");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
