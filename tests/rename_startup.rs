#[test]
fn renamed_binaries_guard_startup() {
    let output = std::process::Command::new("python3")
        .args([
            "-B",
            "tests/rename_startup.py",
            env!("CARGO_BIN_EXE_hippo-mcp"),
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("python3 is required for startup checks");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
