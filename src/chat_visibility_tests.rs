//! target-design 2.6 through the MCP call dispatcher and injected physical sends (no ports).
use super::*;
use std::time::Instant;

#[test]
fn host_bootstrap_continues_with_first_turn_prepare_and_concrete_recall() {
    let db = Db::new();
    let mut service = service(&db);
    let (client, sent) = visible_client();
    service.inject_model_client(client);
    let shared = Mutex::new(service);
    let bootstrap = shared_call(
        &shared,
        "memory_recall",
        json!({
            "query":"全局回答要求", "bootstrap":true, "request_id":"boot:host-route", "at":AT
        }),
    );
    assert_eq!(bootstrap["recall"]["facts"], json!([]));
    assert!(sent.lock().unwrap().is_empty());
    let prepare = json!({"user_turn":{"session_id":"host-route", "turn_id":"first",
        "text":NEW, "observed_at":AT, "capture_source":true}});
    assert!(prepare.get("offline").is_none());
    let prepared = shared_call(&shared, "memory_prepare", prepare);
    assert_eq!(prepared["source"]["status"], "stored");
    assert_eq!(prepared["enrichment"]["status"], "queued");
    assert_eq!(sent.lock().unwrap().len(), 2);
    let recall = query("host-route:question");
    assert!(recall.get("bootstrap").is_none());
    assert!(recall.get("offline").is_none());
    let recalled = shared_call(&shared, "memory_recall", recall);
    assert_eq!(recalled["recall"]["status"], "answered");
    assert_eq!(
        recalled["recall"]["raw"][0]["source_id"],
        prepared["source"]["source_id"]
    );
    let inspected = shared_call(
        &shared,
        "memory_source_status",
        json!({
            "source_id":prepared["source"]["source_id"]
        }),
    );
    assert_eq!(inspected["enrichment"]["status"], "queued");
    assert_eq!(inspected["model_calls"], json!([]));
    let stages: Vec<_> = sent
        .lock()
        .unwrap()
        .iter()
        .map(|r| r["text"]["format"]["name"].clone())
        .collect();
    assert_eq!(
        stages,
        vec![
            json!("r1_v2"),
            json!("r4_v7"),
            json!("r1_v2"),
            json!("r4_v7")
        ]
    );
    assert_eq!(db.calls(), 4); // injected physical sends; bootstrap and inspection send none.

    for tool in crate::chat::memory_tools().into_iter().filter(|t| {
        matches!(
            t.name.as_ref(),
            "memory_recall" | "memory_prepare" | "memory_entity_upsert"
        )
    }) {
        let description = tool.description.unwrap();
        for rule in [
            "SAME current USER message",
            "empty bootstrap",
            "background, plans, additions or corrections",
            "call ordinary memory_recall",
            "BEFORE memory_prepare",
            "without a new question do not repeat",
            "Verification/correction queries",
            "does not prevent saving otherwise authorized raw text",
            "Do not add offline=true",
            "unregistered family member",
            "real name is not required",
            "property or event",
            "ask once",
            "memory_source_status",
        ] {
            assert!(description.contains(rule), "missing {rule}: {description}");
        }
    }
}

const SHANGHAI: &str = "我住上海。";
const NANJING: &str = "我已搬到南京。";
const HYPOTHESIS: &str = "假如我搬到南京，通勤会怎样？";
const RELATIVE: &str = "我昨天搬到南京。";

fn service(db: &Db) -> ChatMemoryService {
    let config: ChatConfig = serde_json::from_value(json!({
        "write_pipeline":"w4_runtime_v1","recall_pipeline":"recall_v3",
        "write_models":{"w1":DEEPSEEK_MODEL,"w3":DEEPSEEK_MODEL,"w4":DEEPSEEK_MODEL,
            "reasoning_effort":{"w1":"medium","w3":"medium","w4":"medium"}},
        "recall_models":{"r1":DEEPSEEK_MODEL,"r4":DEEPSEEK_MODEL,
            "reasoning_effort":{"r1":"medium","r4":"medium"}},
        "enrichment_mode":"inline","background_operation_limit":3,
        "model_budget":{"group_id":"joint-fixture","max_calls":500,"max_operation_calls":2}
    }))
    .unwrap();
    // Initialize once before opening an independent operation connection.
    let _ = SqliteMemoryStore::open(&db.0, OWNER.into()).unwrap();
    ChatMemoryService::new(db.open(), config).unwrap()
}
fn fact(
    statement: &str,
    evidence: Vec<Json>,
    ids: Vec<Json>,
    supersedes: Vec<Json>,
    times: Json,
) -> Json {
    json!({"statement":statement,"evidence":evidence,"record_ids":ids,
        "polarity":"affirm","strength":null,"uncertainty":null,"conditions":[],"times":times,
        "current":"yes","applies":"yes","supersedes_record_ids":supersedes})
}
fn visible_output(body: &Json) -> Json {
    let input: Json = serde_json::from_str(body["input"].as_str().unwrap()).unwrap();
    match body["text"]["format"]["name"].as_str().unwrap() {
        "w1_v5" => {
            let text = input["current_user_message"].as_str().unwrap();
            if text == HYPOTHESIS {
                return json!({"candidates":[],"memory_requests":[]});
            }
            if text.contains("失败") {
                return json!({"invalid":true});
            }
            let mut out = w1(text);
            if text == RELATIVE {
                out["candidates"][0]["times"] = json!([{"role":"event","quote":{"text":"昨天"},
                    "interp":{"kind":"relative","unit":"day","offset":-1},"anchor":"said_on"}]);
            }
            out
        }
        "w4_v5" => {
            let w4 = &input["w4_input"];
            let old = w4["records"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["format"] == "current" && r["stored_status"] == "active");
            json!({"decisions": w4["candidates"].as_array().unwrap().iter().map(|c| {
                let mut basis = vec![json!({"source":c["source"],"quote":c["candidate"]["sentence"]})];
                if let Some(old) = old { basis.extend(old["evidence"].as_array().unwrap().clone()); }
                json!({"candidate":c["index"],"status":"decided","action":if old.is_some(){"state_change"}else{"new"},
                    "targets":old.map(|r| vec![r["id"].clone()]).unwrap_or_default(),"basis":basis,
                    "effective":null,"relation_scope":null,"pending_reason":null,"resolves_relations":[]})
            }).collect::<Vec<_>>()})
        }
        "r4_v7" => {
            let e = &input["evidence"];
            let records: Vec<_> = e["records"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|r| r["format"] == "current" && r["stored_status"] == "active")
                .collect();
            let raw = e["raw"].as_array().unwrap().iter().find(|r| {
                let text = r["quote"]["text"].as_str().unwrap();
                text.contains("搬到") && text != HYPOTHESIS && r["enrichment"] != "needs_rejudge"
            });
            let view = if let Some(raw) = raw {
                let mut evidence = vec![
                    json!({"source":{"kind":"stored","source_id":raw["source_id"]},"quote":raw["quote"]}),
                ];
                for r in &records {
                    evidence.extend(r["evidence"].as_array().unwrap().clone());
                }
                fact(
                    raw["quote"]["text"].as_str().unwrap(),
                    evidence,
                    vec![],
                    records.iter().map(|r| r["id"].clone()).collect(),
                    json!([]),
                )
            } else if let Some(r) = records.first() {
                let c = &r["candidate"];
                fact(
                    c["sentence"]["text"].as_str().unwrap(),
                    r["evidence"].as_array().unwrap().clone(),
                    vec![r["id"].clone()],
                    vec![],
                    c["times"].clone(),
                )
            } else {
                return json!({"verdict":"insufficient","facts":[],"compute":null});
            };
            json!({"verdict":"supported","facts":[view],"compute":null})
        }
        _ => output(body),
    }
}
fn visible_client() -> (OpenAiClient, Sent) {
    let (mut c, sent) = client();
    let requests = Arc::clone(&sent);
    c.transport = Some(Arc::new(move |request| {
        let body: Json =
            serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap();
        requests.lock().unwrap().push(body.clone());
        Ok(envelope(visible_output(&body)))
    }));
    (c, sent)
}
fn verify_http_fixture(sent: &Sent) {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let requests = sent.lock().unwrap().clone();
    let expected = json!(requests.iter().map(visible_output).collect::<Vec<_>>());
    let mut process = Command::new("python3")
        .args(["tests/mcp_visibility.py", "--check-fixture"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    process
        .stdin
        .take()
        .unwrap()
        .write_all(json!(requests).to_string().as_bytes())
        .unwrap();
    let result = process.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Json>(&result.stdout).unwrap(),
        expected
    );
}
fn shared_call(s: &Mutex<ChatMemoryService>, name: &str, args: Json) -> Json {
    let out = ChatMemoryService::call_shared(s, name, args, || false, "visibility").unwrap();
    assert!(!out.is_error.unwrap_or(false), "{out:?}");
    out.structured_content.unwrap()
}
fn save_shared(s: &Mutex<ChatMemoryService>, turn: &str, text: &str) -> Json {
    shared_call(
        s,
        "memory_remember",
        json!({"session_id":"visibility","turn_id":turn,
        "text":text,"observed_at":AT,"capture_source":true}),
    )
}
fn read_shared(s: &Mutex<ChatMemoryService>, id: &str) -> Json {
    shared_call(s, "memory_recall", query(id))["recall"].clone()
}
fn seed(db: &Db, s: &mut ChatMemoryService) {
    let (c, _) = visible_client();
    s.inject_model_client(c);
    capture(s, db, "old", SHANGHAI);
    assert!(s.run_background_once().unwrap());
}
fn source_status(db: &Db, turn: &str) -> String {
    db.open()
        .read()
        .unwrap()
        .sources
        .values()
        .find(|s| s.turn_id == turn)
        .unwrap()
        .enrichment
        .clone()
}
fn settled(db: &Db, turn: &str, status: &str) {
    let start = Instant::now();
    loop {
        let state = db.open().read().unwrap();
        let source = state.sources.values().find(|s| s.turn_id == turn).unwrap();
        if source.enrichment == status {
            return;
        }
        assert!(
            matches!(source.enrichment.as_str(), "queued" | "running")
                && start.elapsed() < Duration::from_secs(8),
            "turn={turn} expected={status} enrichment={} error={:?} round={:?} diagnostic={}",
            source.enrichment,
            source.error,
            source.task.round,
            source
                .result
                .as_ref()
                .map(|r| &r["w4_input_meta"]["diagnostic"])
                .unwrap_or(&Json::Null)
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn af336_settled_reports_terminal_source_without_waiting() {
    for terminal in ["failed", "needs_rejudge", "budget_exhausted", "pending"] {
        let db = Db::new();
        let mut s = service(&db);
        let key = capture(&mut s, &db, "diagnostic", NEW);
        db.open()
            .transaction(|state| {
                let source = state.sources.get_mut(&key).unwrap();
                source.enrichment = terminal.into();
                source.error = Some("synthetic_error".into());
                Ok(())
            })
            .unwrap();
        let failure = std::panic::catch_unwind(|| settled(&db, "diagnostic", "committed"))
            .expect_err("unexpected terminal state must fail");
        let message = failure.downcast_ref::<String>().unwrap();
        assert!(message.contains("turn=diagnostic"));
        assert!(message.contains(&format!("enrichment={terminal}")));
        assert!(message.contains("synthetic_error"));
        assert!(message.contains("round="));
        assert!(message.contains("diagnostic="));
        settled(&db, "diagnostic", terminal);
    }
}

#[test]
fn visibility_a_inline_raw_then_committed_new_version() {
    let db = Db::new();
    let mut s = service(&db);
    seed(&db, &mut s);
    let (mut c, sent) = visible_client();
    let (entered, release) = hold(&mut c, "w1_v5");
    s.inject_model_client(c);
    let worker = s.start_background().unwrap().unwrap();
    let shared = Mutex::new(s);
    let saved = save_shared(&shared, "new", NANJING);
    assert!(
        matches!(
            saved["enrichment"]["status"].as_str(),
            Some("queued" | "running")
        ),
        "{saved}"
    );
    wait(&entered);
    let before = read_shared(&shared, "before");
    assert_eq!(before["status"], "answered", "{before}");
    assert_eq!(before["facts"][0]["statement"], NANJING);
    assert_eq!(before["facts"][0]["grade"], "uncommitted");
    assert!(
        before["raw"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["quote"]["text"] == NANJING)
    );
    release.send(()).unwrap();
    settled(&db, "new", "committed");
    worker.stop_and_join().unwrap();
    let after = read_shared(&shared, "after");
    assert_eq!(after["facts"][0]["statement"], NANJING, "{after}");
    assert_eq!(after["facts"][0]["grade"], "committed");
    assert_ne!(before["snapshot"], after["snapshot"]);
    verify_http_fixture(&sent);
}
#[test]
fn visibility_b_hypothesis_is_not_a_current_update() {
    let db = Db::new();
    let mut s = service(&db);
    seed(&db, &mut s);
    let (c, _) = visible_client();
    s.inject_model_client(c);
    let worker = s.start_background().unwrap().unwrap();
    let shared = Mutex::new(s);
    save_shared(&shared, "hypothesis", HYPOTHESIS);
    settled(&db, "hypothesis", "no_candidates_unreviewed");
    worker.stop_and_join().unwrap();
    let out = read_shared(&shared, "hypothesis");
    assert_eq!(out["facts"][0]["statement"], SHANGHAI, "{out}");
    assert_eq!(
        db.open()
            .read()
            .unwrap()
            .repository
            .list_by_owner(&OWNER.into())
            .unwrap()
            .len(),
        1
    );
}
#[test]
fn visibility_c_failed_enrichment_keeps_raw_evidence_and_status() {
    let db = Db::new();
    let mut s = service(&db);
    seed(&db, &mut s);
    let (c, _) = visible_client();
    s.inject_model_client(c);
    let worker = s.start_background().unwrap().unwrap();
    let shared = Mutex::new(s);
    let text = "我已搬到南京。整理失败测试。";
    save_shared(&shared, "failure", text);
    settled(&db, "failure", "failed");
    worker.stop_and_join().unwrap();
    let receipt = save_shared(&shared, "failure", text);
    assert_eq!(receipt["enrichment"]["status"], "failed");
    let out = read_shared(&shared, "failure");
    assert_eq!(out["facts"][0]["statement"], text, "{out}");
    assert!(
        out["raw"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["enrichment"] == "failed" && r["quote"]["text"] == text)
    );
}
#[test]
fn visibility_d_truncated_related_raw_blocks_old_current_value() {
    let db = Db::new();
    let mut s = service(&db);
    seed(&db, &mut s);
    let (mut c, _) = visible_client();
    let (entered, release) = hold(&mut c, "w1_v5");
    s.inject_model_client(c);
    let worker = s.start_background().unwrap().unwrap();
    let shared = Mutex::new(s);
    save_shared(&shared, "omitted", NANJING);
    wait(&entered);
    for n in 0..32 {
        save_shared(&shared, &format!("noise-{n}"), "我有一本画册。");
    }
    let out = read_shared(&shared, "truncated");
    assert_eq!(out["status"], "insufficient", "{out}");
    assert_eq!(out["completeness"]["retrieval_truncated"], true);
    assert_eq!(out["completeness"]["plan_complete"], false);
    assert_ne!(out["facts"][0]["current"], "yes");
    assert!(
        !out["degraded_reasons"]
            .as_array()
            .unwrap()
            .contains(&json!("budget_exhausted"))
    );
    release.send(()).unwrap();
    worker.stop_and_join().unwrap();
}
#[test]
fn visibility_e_newer_update_commits_first_without_older_overwrite() {
    let db = Db::new();
    let mut s = service(&db);
    seed(&db, &mut s);
    let (mut c, _) = visible_client();
    let (entered, release) = hold(&mut c, "w1_v5");
    s.inject_model_client(c);
    let worker = s.start_background().unwrap().unwrap();
    let shared = Mutex::new(s);
    save_shared(&shared, "older", NANJING);
    wait(&entered);
    let saved = save_shared(&shared, "newer", "我已搬到杭州。");
    let mut explicit = service(&db);
    let (c, _) = visible_client();
    explicit.inject_model_client(c);
    explicit
        .enrich_source(saved["source"]["source_id"].as_str().unwrap())
        .unwrap();
    assert_eq!(source_status(&db, "newer"), "committed");
    release.send(()).unwrap();
    worker.stop_and_join().unwrap();
    assert_eq!(source_status(&db, "older"), "needs_rejudge");
    let records = db
        .open()
        .read()
        .unwrap()
        .repository
        .list_by_owner(&OWNER.into())
        .unwrap();
    assert!(records.iter().any(|r| r.status == MemoryStatus::Active
        && r.evidence.iter().any(|e| e.quote == "我已搬到杭州。")));
    assert!(
        !records
            .iter()
            .any(|r| r.status == MemoryStatus::Active
                && r.evidence.iter().any(|e| e.quote == NANJING))
    );
    let out = read_shared(&shared, "ordered");
    assert_eq!(out["facts"][0]["statement"], "我已搬到杭州。", "{out}");
    assert!(
        out["raw"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["enrichment"] == "needs_rejudge")
    );
}
#[test]
fn visibility_f_cross_day_uses_received_day_for_relative_time() {
    let db = Db::new();
    let mut s = service(&db);
    let (c, sent) = visible_client();
    s.inject_model_client(c);
    let worker = s.start_background().unwrap().unwrap();
    let shared = Mutex::new(s);
    save_shared(&shared, "relative", RELATIVE);
    settled(&db, "relative", "committed");
    worker.stop_and_join().unwrap();
    let state = db.open().read().unwrap();
    let src = state
        .sources
        .values()
        .find(|s| s.turn_id == "relative")
        .unwrap();
    assert_eq!(src.observed_at, AT);
    let batch = crate::enrichment::stored_batch(src).unwrap().unwrap();
    let resolution = &batch.batch.w2.candidates[0].time_resolutions[0];
    let day = time::OffsetDateTime::from_unix_timestamp(resolution.start.unwrap() + 28_800)
        .unwrap()
        .date()
        .to_string();
    assert_eq!(day, "2026-10-01");
    assert_eq!(
        resolution.end_exclusive.unwrap() - resolution.start.unwrap(),
        86_400
    );
    let w1 = sent
        .lock()
        .unwrap()
        .iter()
        .find(|b| b["text"]["format"]["name"] == "w1_v5")
        .unwrap()
        .clone();
    assert!(w1["input"].as_str().unwrap().contains("2026-10-02"));
    let out = read_shared(&shared, "relative");
    assert_eq!(out["facts"][0]["grade"], "committed", "{out}");
    verify_http_fixture(&sent);
}
#[test]
fn visibility_g_deletion_invalidates_foreground_and_late_background() {
    let db = Db::new();
    let mut s = service(&db);
    seed(&db, &mut s);
    let (mut c, _) = visible_client();
    let (entered, release) = hold(&mut c, "w1_v5");
    let (r4_entered, r4_release) = hold(&mut c, "r4_v7");
    s.inject_model_client(c);
    let worker = s.start_background().unwrap().unwrap();
    let shared = Mutex::new(s);
    let saved = save_shared(&shared, "deleted", NANJING);
    wait(&entered);
    std::thread::scope(|scope| {
        let recall = scope.spawn(|| {
            ChatMemoryService::call_shared(
                &shared,
                "memory_recall",
                query("deleted"),
                || false,
                "foreground",
            )
        });
        wait(&r4_entered);
        shared_call(
            &shared,
            "memory_forget",
            json!({"source_id":saved["source"]["source_id"]}),
        );
        release.send(()).unwrap();
        worker.stop_and_join().unwrap();
        r4_release.send(()).unwrap();
        let out = recall.join().unwrap().unwrap();
        assert!(out.is_error.unwrap_or(false));
        assert!(
            !out.structured_content
                .unwrap()
                .to_string()
                .contains(NANJING)
        );
    });
    assert_eq!(source_status(&db, "deleted"), "deleted");
    assert!(
        !serde_json::to_string(&db.open().read().unwrap())
            .unwrap()
            .contains(NANJING)
    );
}
#[test]
fn visibility_inline_requires_capture_and_offline_prepare_only_queues_write() {
    let db = Db::new();
    let mut s = service(&db);
    assert!(
        s.call(
            "memory_remember",
            json!({"session_id":"capture","turn_id":"missing","text":NANJING}),
            || false
        )
        .unwrap_err()
        .to_string()
        .contains("capture_source_required")
    );
    let out=s.call("memory_prepare",json!({"offline":true,"user_turn":{"session_id":"capture","turn_id":"offline","text":NANJING,"capture_source":true,"observed_at":AT}}),||false).unwrap();
    assert_eq!(out["enrichment"]["status"], "queued");
    assert_eq!(out["recall"]["semantic_judgment"], "skipped");
    assert_eq!(db.calls(), 0);
}

#[test]
fn visibility_normal_cadence_snapshot_change_frequency() {
    // Independent elapsed-time schedules, no event barriers or forced completion ordering.
    // Synthetic timings measure race exposure, not production latency or model quality.
    for (label, background_ms, foreground_ms, pause_ms) in [
        ("after-150ms", 2, 5, 150),
        ("after-35ms", 2, 5, 35),
        ("immediate-fast-write", 8, 20, 0),
        ("immediate-slow-write", 25, 5, 0),
    ] {
        let db = Db::new();
        let mut s = service(&db);
        seed(&db, &mut s);
        let (mut c, _) = visible_client();
        let transport = c.transport.take().unwrap();
        let stamps = Arc::new(Mutex::new(Vec::new()));
        let initial_stamps = Arc::clone(&stamps);
        let path = db.0.clone();
        c.transport = Some(Arc::new(move |request| {
            let body: Json =
                serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap();
            if body["text"]["format"]["name"] == "r1_v2" {
                let state = SqliteMemoryStore::open_operation(&path, OWNER.into())
                    .unwrap()
                    .read()
                    .unwrap();
                let receipt = state
                    .receipts
                    .values()
                    .find(|r| {
                        r.request["form"] == "recall_metadata_v1" && r.result["state"] == "running"
                    })
                    .unwrap();
                initial_stamps
                    .lock()
                    .unwrap()
                    .push(receipt.result["stamp"].clone());
            }
            let bg = body["text"]["format"]["name"]
                .as_str()
                .unwrap()
                .starts_with('w');
            std::thread::sleep(Duration::from_millis(if bg {
                background_ms
            } else {
                foreground_ms
            }));
            transport(request)
        }));
        s.inject_model_client(c);
        let worker = s.start_background().unwrap().unwrap();
        let shared = Mutex::new(s);
        let mut changed = 0;
        let mut other = 0;
        let mut commit_overlap = 0;
        let mut status_only = 0;
        for n in 0..30 {
            let turn = format!("cadence-{n}");
            save_shared(&shared, &turn, if n % 2 == 0 { NANJING } else { SHANGHAI });
            std::thread::sleep(Duration::from_millis(pause_ms));
            let out = ChatMemoryService::call_shared(
                &shared,
                "memory_recall",
                query(&turn),
                || false,
                "foreground",
            )
            .unwrap();
            if out.is_error.unwrap_or(false) {
                let error = out.structured_content.unwrap().to_string();
                if error.contains("recall_snapshot_changed") {
                    changed += 1;
                } else {
                    other += 1;
                }
            }
            let initial = stamps.lock().unwrap().last().unwrap().clone();
            let latest = db.open().read().unwrap();
            if latest.revision > initial["revision"].as_u64().unwrap() {
                commit_overlap += 1;
            } else if crate::openai::recall_models::fingerprint(&json!(
                crate::recall::source_metadata(&latest).unwrap()
            )) != initial["source_metadata"]
            {
                status_only += 1;
            }
            settled(&db, &turn, "committed"); // next turn starts after the previous conversation completes.
        }
        worker.stop_and_join().unwrap();
        eprintln!(
            "SNAPSHOT_CADENCE {label}: changed={changed}/30 commit_overlap={commit_overlap} status_only={status_only} other={other} bg_stage_ms={background_ms} fg_stage_ms={foreground_ms} read_pause_ms={pause_ms} calls={}",
            db.calls()
        );
        assert_eq!(other, 0);
        assert_eq!(
            changed, 0,
            "ordinary background progress must not invalidate recall"
        );
        assert!(db.calls() <= 152, "unexpected implicit retry");
    }
}

#[test]
fn visibility_real_prepare_immediate_recall_success_frequency() {
    for (label, background_ms, foreground_ms) in
        [("prepare-fast-write", 8, 20), ("prepare-slow-write", 25, 5)]
    {
        let db = Db::new();
        let mut service = service(&db);
        seed(&db, &mut service);
        let (mut client, _) = visible_client();
        let transport = client.transport.take().unwrap();
        client.transport = Some(Arc::new(move |request| {
            let body: Json =
                serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap();
            let background = body["text"]["format"]["name"]
                .as_str()
                .unwrap()
                .starts_with('w');
            std::thread::sleep(Duration::from_millis(if background {
                background_ms
            } else {
                foreground_ms
            }));
            transport(request)
        }));
        service.inject_model_client(client);
        let worker = service.start_background().unwrap().unwrap();
        let shared = Mutex::new(service);
        let mut success = 0;
        let mut failed = 0;
        for n in 0..30 {
            let turn = format!("prepare-cadence-{n}");
            let text = if n % 2 == 0 {
                "我已搬到苏州。"
            } else {
                NANJING
            };
            let args = json!({"user_turn":{"text":text,"session_id":"visibility",
                "turn_id":turn,"observed_at":AT,"capture_source":true}});
            // Actual prepare performs T1 and immediately starts R1/R3/R4; no separate remember.
            let response = ChatMemoryService::call_shared(
                &shared,
                "memory_prepare",
                args.clone(),
                || false,
                "prepare-cadence",
            )
            .unwrap();
            let body = response.structured_content.as_ref().unwrap();
            if response.is_error != Some(true) && body["recall"]["version"] == "recall.v11" {
                success += 1;
                assert_eq!(body["source"]["status"], "stored");
                assert_eq!(body["recall"]["status"], "answered", "{body}");
                assert_eq!(body["recall"]["facts"][0]["statement"], text);
                let id = &body["source"]["source_id"];
                if let Some(raw) = body["recall"]["raw"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|r| &r["source_id"] == id)
                {
                    assert_eq!(body["enrichment"]["status"], raw["enrichment"]);
                }
                // Delivery/replay after the worker commits keep the same R3 observation.
                settled(&db, &turn, "committed");
                let operation = shared.lock().unwrap().operation();
                let checked = operation
                    .connect()
                    .unwrap()
                    .validate_prepare_response(&args, response.clone());
                assert_eq!(
                    checked.structured_content.as_ref().unwrap()["recall"],
                    body["recall"]
                );
                assert_eq!(
                    checked.structured_content.as_ref().unwrap()["enrichment"],
                    body["enrichment"]
                );
                let replay = shared_call(&shared, "memory_prepare", args);
                assert_eq!(replay["recall"], body["recall"]);
                assert_eq!(replay["enrichment"], body["enrichment"]);
            } else {
                failed += 1;
                eprintln!("PREPARE_FAILED {label}/{n}: {body}");
                settled(&db, &turn, "committed");
            }
        }
        worker.stop_and_join().unwrap();
        eprintln!(
            "PREPARE_CADENCE {label}: success={success}/30 failed={failed} bg_stage_ms={background_ms} fg_stage_ms={foreground_ms} calls={}",
            db.calls()
        );
        assert_eq!(failed, 0);
        assert_eq!(success, 30);
        assert!(db.calls() <= 152, "unexpected implicit retry");
    }
}

#[test]
fn query_embedding_releases_host_lock_and_offline_bootstrap_do_not_call_it() {
    let db = Db::new();
    let mut s = service(&db);
    seed(&db, &mut s);
    // Keep this lock test on the non-enumerating path after AF288.
    capture(&mut s, &db, "new", NANJING);
    assert!(s.run_background_once().unwrap());
    let (entered, entry) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let released = Mutex::new(released);
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let calls = count.clone();
    s.inject_embedding_query(Arc::new(move |text| {
        assert_eq!(text, "我住在哪？");
        calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        entered.send(()).unwrap();
        released
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(8))
            .unwrap();
        Err("injected_query_failure".into())
    }));
    let shared = Mutex::new(s);
    let held = || {
        let mut input = query("embedding:held");
        input["limit"] = json!(1);
        shared_call(&shared, "memory_recall", input)["recall"].clone()
    };
    std::thread::scope(|scope| {
        let read = scope.spawn(&held);
        wait(&entry);
        // This management call must complete before the query embedding is released.
        let boot = shared_call(
            &shared,
            "memory_recall",
            json!({"query":"全局回答要求","request_id":"embedding:boot","bootstrap":true,"at":AT}),
        );
        assert_eq!(boot["recall"]["retrieval"]["status"], "not_needed");
        let mut offline = query("embedding:offline");
        offline["offline"] = json!(true);
        let result = shared_call(&shared, "memory_recall", offline);
        assert_eq!(result["recall"]["retrieval"]["status"], "not_needed"); // R1 skipped: actual scope enumeration
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1);
        let listed = shared_call(&shared, "memory_list", json!({}));
        assert!(listed.get("error").is_none());
        release.send(()).unwrap();
        let out = read.join().unwrap();
        assert_eq!(out["retrieval"]["status"], "unavailable");
        assert_eq!(held(), out);
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1);
    });
}
