//! 常驻端到端评测：默认 v3，离线严格回放 AC2；只有显式 EVAL_RECORD 才允许录制。
//! 旧执行运行器已退出；0517000 历史报告与评分适配器保留在 tests/eval/scoring。
//! 运行配置、干跑与定点重录说明见 tests/eval/README-v3.md。

use hippocampus::chat::{ChatConfig, ChatMemoryService};
use hippocampus::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const OWNER: &str = "eval";
const CLOCK: i64 = 1_790_568_000;
const QUICK: &[&str] = &[
    "s6_explicit_correction",
    "s6_unresolved_conflict",
    "s6_task_isolation",
    "s6_device_default_paper",
    "s6_absolute_date_ranges",
    "lme_5e1b23de",
    "lme_577d4d32",
    "lme_c8c3f81d",
    "lme_0862e8bf_abs",
];

#[path = "eval/v3.rs"]
mod v3;

fn validate_pipeline(value: Option<&str>) -> Result<(), String> {
    match value {
        None | Some("v3") => Ok(()),
        Some("legacy") => Err("EVAL_PIPELINE=legacy is retired; use v3; historical baseline and scoring adapters remain in tests/eval/scoring".into()),
        Some(other) => Err(format!("unknown EVAL_PIPELINE: {other}")),
    }
}

#[test]
fn resident_eval_suite() {
    validate_pipeline(std::env::var("EVAL_PIPELINE").ok().as_deref())
        .unwrap_or_else(|error| panic!("{error}"));
    v3::run();
}

#[test]
fn eval_defaults_to_v3_and_rejects_retired_execution() {
    assert!(validate_pipeline(None).is_ok());
    assert!(validate_pipeline(Some("v3")).is_ok());
    assert!(
        validate_pipeline(Some("legacy"))
            .unwrap_err()
            .contains("retired")
    );
    assert!(validate_pipeline(Some("invalid")).is_err());
}

struct TempDb(std::path::PathBuf);

impl TempDb {
    fn open(&self) -> SqliteMemoryStore {
        SqliteMemoryStore::open(&self.0, UserId::from(OWNER)).unwrap()
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
        }
    }
}
