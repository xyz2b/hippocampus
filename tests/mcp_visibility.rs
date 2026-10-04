//! Seven visibility scenarios through MCP stdio + local synthetic Responses HTTP.
use std::process::Command;

fn run(case: &str) {
    let output = Command::new("python3")
        .args([
            "tests/mcp_visibility.py",
            "--binary",
            env!("CARGO_BIN_EXE_pmr-mcp"),
            "--case",
            case,
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
fn mcp_visibility_a_inline_raw_then_committed() {
    run("a");
}
#[test]
fn mcp_visibility_b_hypothesis() {
    run("b");
}
#[test]
fn mcp_visibility_c_background_failure() {
    run("c");
}
#[test]
fn mcp_visibility_d_truncated_new_raw() {
    run("d");
}
#[test]
fn mcp_visibility_e_out_of_order_updates() {
    run("e");
}
#[test]
fn mcp_visibility_f_cross_day_relative_time() {
    run("f");
}
#[test]
fn mcp_visibility_g_delete_during_both_models() {
    run("g");
}
