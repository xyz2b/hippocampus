#[test]
fn explicit_modes_off_capture_and_bounded_inline_through_mcp() {
    let output = std::process::Command::new("python3")
        .args([
            "tests/s5_process.py",
            "--binary",
            env!("CARGO_BIN_EXE_pmr-mcp"),
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("python3 is required for the isolated S5 MCP checks");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
