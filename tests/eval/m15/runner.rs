//! Opt-in fictional MVP; the default 63-case historical suite is unchanged.
use super::*;
const RESERVED: u64 = 24;

fn sample() -> Value {
    serde_json::from_str(include_str!("mvp.json")).unwrap()
}

fn sent_before(dir: &Path) -> Result<u64, String> {
    let logs = dir.join("runs");
    if !logs.exists() {
        return Ok(0);
    }
    let mut count = 0;
    for entry in std::fs::read_dir(logs).map_err(|e| e.to_string())? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let log: Value = serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        count += log["attempts"].as_array().ok_or("invalid send log")?.len() as u64;
    }
    Ok(count)
}

pub(super) fn run() {
    let case = sample();
    let dir = std::env::var_os("EVAL_M15_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("tests/eval/m15/recordings/mvp"));
    let spent = sent_before(&dir).unwrap();
    let remaining = RESERVED
        .checked_sub(spent)
        .expect("MVP campaign already exceeds approved 24 sends");
    let limit = std::env::var("EVAL_RECORD")
        .ok()
        .map(|s| s.parse().expect("EVAL_RECORD integer"));
    preflight(limit, RESERVED).unwrap();
    println!("本目录累计已发送/在途 {spent}；本轮最多新增 {remaining}；累计上限 24。");
    println!("M1.5 MVP：预登记 child-demo（0 调用）；4 prepare + 4 后台整理 + 新会话 2 recall。");
    println!("精确预留上限 24：W1=4、W3=4、W4=4、R1=6、R4=6；跳过阶段少发，不增加重试槽位。");
    assert!(
        std::process::Command::new("python3")
            .args(["tests/eval/m15/token_estimate.py", "mvp"])
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .status()
            .unwrap()
            .success()
    );
    let cfg = config("m15_mvp", "", RESERVED);
    let budget = cfg.model_budget.as_ref().unwrap();
    let output = 4 * 32768
        + 8 * 16384
        + 6 * budget.query_output_tokens as u64
        + 6 * budget.review_output_tokens as u64;
    println!(
        "按请求字节预算/3 粗估输入容量约 {}（24 × {} 字节）；输出配置上限合计 {}；动态证据未知，不把容量当实际 token 上界。",
        (24 * budget.max_input_bytes).div_ceil(3),
        budget.max_input_bytes,
        output
    );
    if std::env::var("EVAL_DRY_RUN").as_deref() == Ok("1") {
        println!("干跑：真实调用 0；未读取密钥、未创建录制。标注 draft，待 Claude/用户复核。");
        return;
    }
    let output = std::env::var_os("EVAL_M15_OUTPUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/eval-m15/mvp-run.json"));
    let live = limit.filter(|_| remaining > 0).map(|_| {
        (
            format!(
                "{}/responses",
                std::env::var("DEEPSEEK_BASE_URL")
                    .expect("live base URL")
                    .trim_end_matches('/')
            ),
            std::env::var("DEEPSEEK_API_KEY").expect("live API key"),
        )
    });
    let recorder = Arc::new(Recorder::new(dir.clone(), limit.map(|_| remaining), live));
    // Missing offline replay does not even need a listening socket.
    let mut result = if limit.is_none() && !dir.join("m15_mvp.json").exists() {
        json!({"case":"m15_mvp","steps":case["questions"].as_array().unwrap().iter().map(|q|
            json!({"step":"question","question":q["id"],"state":"not_recorded","reason":"未录制"})).collect::<Vec<_>>()})
    } else {
        let (address, stop, handle) = server(recorder.clone(), None).unwrap();
        unsafe {
            std::env::set_var("PMR_MODEL_NETWORK", "offline");
            std::env::set_var("DEEPSEEK_API_KEY", "eval-replay");
        }
        let result = run_on(&case, &recorder, &address);
        stop.store(true, Ordering::SeqCst);
        handle.join().unwrap();
        result
    };
    result["calls"] = json!(recorder.calls("m15_mvp"));
    write_json(
        &output,
        &json!({"pipeline":"m15","label_status":"draft","cases":[result],
            "reserved":RESERVED,"real_calls":recorder.sent.load(Ordering::Relaxed),"previous_sends":spent,
            "total_sends":spent+recorder.sent.load(Ordering::Relaxed) as u64,"send_log":recorder.send_log_path(),
        "configuration":recorder.configuration}),
    );
    let status = std::process::Command::new("python3")
        .args(["tests/eval/m15/report.py", output.to_str().unwrap()])
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .status()
        .unwrap();
    assert!(status.success(), "MVP report construction failed");
    println!(
        "真实调用 {}；输出 {}；未录制/待复核不计通过。",
        recorder.sent.load(Ordering::Relaxed),
        output.display()
    );
}

fn run_on(case: &Value, recorder: &Recorder, address: &str) -> Value {
    let id = case["id"].as_str().unwrap();
    let db =
        TempDb(std::env::temp_dir().join(format!("eval-m15-{}.sqlite3", diagnostics::new_id())));
    let cfg = config(id, address, RESERVED);
    let mut service = ChatMemoryService::new(db.open(), cfg.clone()).unwrap();
    recorder.bind_source_database(id, &db.0);
    let mut steps = Vec::new();
    let mut source_ids = BTreeMap::new();
    let bootstrap=service.call("memory_recall",json!({"bootstrap":true,"query":"全局回答要求","request_id":"m15-bootstrap-write","at":CLOCK}),||false).unwrap();
    steps.push(json!({"step":"bootstrap","result":bootstrap}));
    for entity in case["entities"].as_array().unwrap() {
        let result = service
            .call("memory_entity_upsert", entity.clone(), || false)
            .unwrap();
        steps.push(json!({"step":"entity","result":result}));
    }
    let messages = case["messages"].as_array().unwrap();
    let mut blocked = None;
    for (index, message) in messages.iter().enumerate() {
        if blocked.is_some() {
            break;
        }
        let mid = message["id"].as_str().unwrap();
        recorder.begin(id, &format!("prepare:{mid}"), &source_ids);
        let args = json!({"request_id":format!("m15-prepare-{mid}"),"user_turn":{
            "session_id":"m15-write-session","turn_id":mid,"text":message["text"],"capture_source":true,
            "observed_at":message["observed_at"],"history":messages[..index].iter().map(|m|m["text"].clone()).collect::<Vec<_>>()}});
        let (received, diagnostics) = diagnostics::capture_model_events(|| {
            service.call("memory_prepare", args, || false).unwrap()
        });
        blocked = recorder.missing(id);
        let source = received["source"]["source_id"]
            .as_str()
            .expect("prepare must save T1")
            .to_owned();
        assert_eq!(received["source"]["status"], "stored");
        source_ids.insert(mid.to_owned(), source.clone());
        steps.push(json!({"step":"prepare","message":mid,"operation":format!("prepare:{mid}"),"result":received,
            "model_diagnostics":diagnostics,"missing":blocked}));
        if blocked.is_some() {
            break;
        }
        recorder.begin(id, &format!("write:{mid}"), &source_ids);
        // Same queued-source processing as the worker, no worker races. One
        // source is queued at a time; no retry/rejudge/model repair is added.
        let (result, diagnostics) =
            diagnostics::capture_model_events(|| service.drain_enrichment(1).unwrap());
        blocked = recorder.missing(id);
        let state = db.open().read().unwrap();
        let stored = state.sources.values().find(|s| s.id == source).unwrap();
        steps.push(json!({"step":"message","message":mid,"operation":format!("write:{mid}"),"result":result,
            "source":stored,"model_diagnostics":diagnostics,"missing":blocked}));
    }
    drop(service);
    let mut service = ChatMemoryService::new(db.open(), cfg).unwrap();
    let bootstrap=service.call("memory_recall",json!({"bootstrap":true,"query":"全局回答要求","request_id":"m15-bootstrap-read","at":1790568300}),||false).unwrap();
    steps.push(json!({"step":"bootstrap","session":"new","result":bootstrap}));
    for question in case["questions"].as_array().unwrap() {
        let qid = question["id"].as_str().unwrap();
        if let Some(reason) = &blocked {
            steps.push(
                json!({"step":"question","question":qid,"state":"not_recorded","reason":reason}),
            );
            continue;
        }
        recorder.begin(id, &format!("recall:{qid}"), &source_ids);
        let (result, diagnostics) = diagnostics::capture_model_events(|| {
            service.call("memory_recall",json!({
            "query":question["query"],"context":[],"at":question["at"],"request_id":format!("m15-read-{qid}")}),||false).unwrap()
        });
        let missing = recorder.missing(id);
        let source_state: BTreeMap<_, _> = source_ids
            .keys()
            .map(|s| (s.clone(), json!({"linkable":true,"deleted":false})))
            .collect();
        steps.push(json!({"step":"question","question":qid,"at":question["at"],"state":if missing.is_some(){"not_recorded"}else{"completed"},
            "reason":missing,"result":result,"model_diagnostics":diagnostics,"source_ids":source_ids,"source_state":source_state}));
    }
    json!({"case":id,"steps":steps})
}

#[test]
fn m15_mvp_reserves_24_and_refuses_23() {
    let s = sample();
    assert_eq!(s["messages"].as_array().unwrap().len(), 4);
    assert_eq!(s["questions"].as_array().unwrap().len(), 2);
    assert!(
        s["questions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|q| q["context"] == json!([]))
    );
    assert!(preflight(Some(23), RESERVED).is_err());
    assert!(preflight(Some(24), RESERVED).is_ok());
}

#[test]
fn m15_prepare_t1_ids_are_registered_before_foreground_and_strictly_replayed() {
    let dir = std::env::temp_dir().join(format!("m15-ids-{}", diagnostics::new_id()));
    let mut previous = Value::Null;
    for replay in [false, true] {
        let db = TempDb(
            std::env::temp_dir().join(format!("m15-source-{}.sqlite3", diagnostics::new_id())),
        );
        let mut service = ChatMemoryService::new(db.open(), config("m15_ids", "", 24)).unwrap();
        service.call("memory_remember",json!({"session_id":"test","turn_id":"m1","text":"测试用虚构原句。","capture_source":true,"observed_at":CLOCK}),||false).unwrap();
        let source = db
            .open()
            .read()
            .unwrap()
            .sources
            .values()
            .next()
            .unwrap()
            .id
            .clone();
        let recorder = Recorder::new(dir.clone(), if replay { None } else { Some(1) }, None);
        recorder.bind_source_database("m15_ids", &db.0);
        recorder.begin("m15_ids", "prepare:m1", &BTreeMap::new());
        let request = json!({"model":PRO,"input":json!({"source":{"kind":"stored","source_id":source},"quote":{"text":"测试用虚构原句。"}}).to_string(),"text":{"format":{"name":"r1_v1"}}});
        let response = recorder.respond("m15_ids", &request, || {
            assert!(!replay, "exact request must reuse response");
            let log: Value =
                serde_json::from_slice(&std::fs::read(recorder.send_log_path()).unwrap()).unwrap();
            assert_eq!(log["attempts"][0]["state"], "started");
            Ok((200, json!({"status":"completed"}).to_string()))
        });
        assert_eq!(response.0, 200);
        let saved =
            recorder.documents.lock().unwrap()["m15_ids"]["operations"]["prepare:m1"][0].clone();
        assert_eq!(
            saved["request_content"]["input"]["source"]["source_id"],
            "m1"
        );
        if replay {
            assert_eq!(saved["request"], previous);
            assert_eq!(recorder.sent.load(Ordering::Relaxed), 0);
        }
        previous = saved["request"].clone();
        let mut changed = request.clone();
        changed["reasoning"] = json!({"effort":"low"});
        recorder.begin("m15_ids", "prepare:m1", &BTreeMap::new());
        if replay {
            assert_eq!(
                recorder
                    .respond("m15_ids", &changed, || panic!("stale must not send"))
                    .0,
                503
            );
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn m15_mvp_prepare_background_and_new_session_http_replay() {
    // Synthetic provider output tests routing/persistence, not sample quality.
    fn fixture(request: &Value) -> Value {
        let input: Value = serde_json::from_str(request["input"].as_str().unwrap()).unwrap();
        assert!(input.get("model").is_none());
        assert!(input.get("expect_common").is_none());
        match request["text"]["format"]["name"].as_str().unwrap() {
            "w1_v2" => {
                let text = &input["current_user_message"];
                json!({"candidates":[{"kind":"assertion","speech":"user_statement","subject":{"entity_id":"self"},
                    "relation":"fact","memory_type":"fact","value":{"kind":"text","text":text,"quote":{"text":text}},
                    "sentence":{"text":text},"polarity":"affirm","strength":null,"uncertainty":null,"conditions":[],"times":[]}],"memory_requests":[]})
            }
            "w3_v2" => json!({"reviews":input["candidates"].as_array().unwrap().iter().map(|c|
                json!({"candidate":c["index"],"verdict":"supported","problems":[],"episode_value":"not_applicable"})).collect::<Vec<_>>(),
                "coverage":{"status":"complete","missing":[]},"request_reviews":[]}),
            "w4_v4" => {
                let c = &input["w4_input"]["candidates"][0];
                let r = &input["w4_input"]["records"][0];
                json!({"decisions":[{"candidate":c["index"],"status":"decided","action":"reinforce","targets":[r["id"]],
                    "resolves_relations":[],"basis":[{"source":c["source"],"quote":c["candidate"]["sentence"]},r["evidence"][0]],
                    "effective":null,"relation_scope":null,"pending_reason":null}]})
            }
            "r1_v1" => {
                json!({"need":"judge","current_evidence":"assertion_present","type":"single","subject":{"entity_id":"self"},
                "relation":"fact","conditions":[],"time":null,"plan":{"op":"lookup","filter":{"relation":"fact","time":null},"unit":null}})
            }
            "r4_v4" => {
                let r = &input["evidence"]["records"][0];
                if !r.is_null() {
                    json!({"verdict":"supported","facts":[{"statement":r["candidate"]["sentence"]["text"],
                        "evidence":r["evidence"],"record_ids":[r["id"]],"polarity":"affirm","strength":null,"uncertainty":null,
                        "conditions":[],"times":[],"current":"yes","applies":"yes","supersedes_record_ids":[]}],"compute":null})
                } else {
                    json!({"verdict":"insufficient","facts":[],"compute":null})
                }
            }
            other => panic!("unexpected stage {other}"),
        }
    }
    let dir = std::env::temp_dir().join(format!("m15-mvp-http-{}", diagnostics::new_id()));
    let mut first = Value::Null;
    for replay in [false, true] {
        let recorder = Arc::new(Recorder::new(
            dir.clone(),
            if replay { None } else { Some(24) },
            None,
        ));
        let (address, stop, handle) = server(recorder.clone(), Some(fixture)).unwrap();
        unsafe {
            std::env::set_var("PMR_MODEL_NETWORK", "offline");
            std::env::set_var("DEEPSEEK_API_KEY", "eval-replay");
        }
        let result = run_on(&sample(), &recorder, &address);
        stop.store(true, Ordering::SeqCst);
        handle.join().unwrap();
        assert_eq!(
            result["steps"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|s| s["step"] == "prepare")
                .count(),
            4
        );
        let writes: Vec<_> = result["steps"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| s["step"] == "message")
            .collect();
        assert_eq!(writes.len(), 4);
        assert!(
            writes
                .iter()
                .all(|s| s["source"]["enrichment"] == "committed")
        );
        assert_eq!(
            result["steps"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|s| s["step"] == "question" && s["state"] == "completed")
                .count(),
            2
        );
        let calls = recorder.calls("m15_mvp");
        for stage in ["W1", "W3", "W4", "R1", "R4"] {
            assert!(calls.iter().any(|c| c["stage"] == stage), "{stage}");
        }
        if replay {
            assert_eq!(recorder.sent.load(Ordering::Relaxed), 0);
            let answers = |r: &Value| {
                r["steps"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|s| s["step"] == "question")
                    .map(|s| s["result"]["facts"].clone())
                    .collect::<Vec<_>>()
            };
            // Quotes/values unchanged; runtime source IDs legitimately differ.
            let texts = |xs: Vec<Value>| {
                xs.iter()
                    .map(|v| v[0]["statement"].clone())
                    .collect::<Vec<_>>()
            };
            assert_eq!(texts(answers(&result)), texts(answers(&first)));
        } else {
            assert!(recorder.sent.load(Ordering::Relaxed) <= 24);
            first = result;
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn m15_mvp_counts_unanswered_sends_across_restarts() {
    let dir = std::env::temp_dir().join(format!("m15-quota-{}", diagnostics::new_id()));
    assert_eq!(sent_before(&dir).unwrap(), 0);
    write_json(
        &dir.join("runs/one.json"),
        &json!({"attempts":[{"state":"started"},{"state":"recorded"}]}),
    );
    write_json(
        &dir.join("runs/two.json"),
        &json!({"attempts":[{"state":"started"}]}),
    );
    assert_eq!(sent_before(&dir).unwrap(), 3);
    std::fs::remove_dir_all(dir).unwrap();
}
