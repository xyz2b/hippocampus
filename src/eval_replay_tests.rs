//! The same strict recorder/case driver as the HTTP harness, using the existing test transport.
use super::*;
use crate::chat::{ChatConfig, ChatMemoryService};
use crate::*;
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicUsize;
use std::time::Instant;

const OWNER: &str = "eval";
const CLOCK: i64 = 1_790_568_000;
const QUICK: &[&str] = &[
    "s6_explicit_correction",
    "s6_device_default_paper",
    "s6_person_sketch_pen",
];

struct TempDb(PathBuf);
impl TempDb {
    fn open(&self) -> SqliteMemoryStore {
        SqliteMemoryStore::open(&self.0, OWNER.into()).unwrap()
    }
}
impl Drop for TempDb {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
        }
    }
}

#[allow(dead_code)]
#[path = "../tests/eval/v3_core.rs"]
mod core;
use core::*;

#[test]
fn recorded_full_e2e_replay_never_sends() {
    let recorder = Arc::new(Recorder::new(
        Path::new(ROOT).join("end_to_end").join(LABEL),
        None,
        None,
    ));
    let started = Instant::now();
    let mut results = Vec::new();
    for case in cases() {
        let case_id = case["id"].as_str().unwrap();
        let db = TempDb(
            std::env::temp_dir().join(format!("eval-no-port-{}.sqlite3", diagnostics::new_id())),
        );
        let mut service = ChatMemoryService::new(
            db.open(),
            config(case_id, "offline.invalid", counts(&case).iter().sum()),
        )
        .unwrap();
        let mut client = OpenAiClient::new("http://offline.invalid/v1", "fixture-only").unwrap();
        client.network_allowed = true;
        let captured = recorder.clone();
        let id = case_id.to_owned();
        client.transport = Some(Arc::new(move |request| {
            let request_body: Value =
                serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap();
            if matches!(
                request_body
                    .pointer("/text/format/name")
                    .and_then(Value::as_str),
                Some("w1_v1" | "w3_v1" | "w4_v3")
            ) {
                assert_eq!(request.timeout().copied(), Some(Duration::from_secs(360)));
            }
            Ok(captured.respond(&id, &request_body, || {
                panic!("offline replay must never send")
            }))
        }));
        service.inject_model_client(client);
        let mut result = run_case_on(&case, &recorder, service, db);
        result["calls"] = json!(recorder.calls(case_id));
        assert!(
            result["calls"]
                .as_array()
                .unwrap()
                .iter()
                .all(|call| call["sends"] == 0 && call["origin"] != "live")
        );
        results.push(result);
    }
    assert_eq!(recorder.sent.load(Ordering::Relaxed), 0);
    if let Some(path) = std::env::var_os("EVAL_V3_UNIT_OUTPUT") {
        let path = PathBuf::from(path);
        if path.exists() {
            let previous: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!(previous["real_calls"], 0, "回放不得覆盖 live 输出");
        }
        write_json(
            &path,
            &json!({"pipeline":"v3","label":LABEL,"execution_mode":"replay",
            "transport":"injected_test_only","configuration":recorder.configuration,"cases":results,
            "real_calls":0,"jobs":1,"elapsed_ms":started.elapsed().as_millis()}),
        );
    }
}
