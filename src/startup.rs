//! Shared checks for the two command-line entry points, before opening any data.
use std::path::{Path, PathBuf};

pub fn reject_legacy_env() -> Result<(), String> {
    const SUFFIXES: &[&str] = &[
        "MODEL_NETWORK",
        "TRACE_DIR",
        "INSPECTOR_BINARY",
        "TRACE_FULL_SNAPSHOTS",
        "OPENAI_TRACE",
        "OPENAI_TRACE_CONTENT",
        "MODEL_OPERATION_ID",
        "MODEL_BUDGET_DB",
        "MODEL_BUDGET_OWNER",
        "MODEL_BUDGET_CONFIG",
        "MODEL_TIMEOUT_SECS",
        "BUDGET_TEST_DB",
        "BACKGROUND_TRACE_TEST",
    ];
    // Only names are inspected; values must never be used or included in errors.
    for (name, _) in std::env::vars_os() {
        for suffix in SUFFIXES {
            if name == std::ffi::OsStr::new(&format!("PMR_{suffix}")) {
                return Err(format!(
                    "PMR_{suffix} is no longer supported; use HIPPOCAMPUS_{suffix}"
                ));
            }
        }
    }
    Ok(())
}

pub fn default_db(home: &Path) -> std::io::Result<PathBuf> {
    let old = home.join(".local/share/personal-memory-runtime/memory.sqlite3");
    let new = home.join(".local/share/hippocampus/memory.sqlite3");
    if old.try_exists()? && !new.try_exists()? {
        return Err(std::io::Error::other(
            "Legacy default database exists but the new default database is missing. Stop all related processes before moving the entire directory. Only if the destination directory does not exist, run: `mv ~/.local/share/personal-memory-runtime ~/.local/share/hippocampus`. If the destination already exists, resolve it manually first; do not overwrite it. Update explicit database/config/trace paths as well. No data was moved.",
        ));
    }
    Ok(new)
}
