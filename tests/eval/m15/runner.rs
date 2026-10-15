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

// Keep the AC2 harness profile unchanged; this approved repair has its own campaign.
fn mvp_config(case: &str, address: &str) -> ChatConfig {
    let mut cfg = config(case, address, RESERVED);
    cfg.write_models
        .as_mut()
        .unwrap()
        .reasoning_effort
        .insert("w1".into(), ReasoningEffort::Low);
    cfg
}

fn r1_requests(case: &Value) -> BTreeMap<String, Value> {
    let db =
        TempDb(std::env::temp_dir().join(format!("m15-plan-{}.sqlite3", diagnostics::new_id())));
    let cfg = mvp_config("m15_mvp", "");
    let mut service = ChatMemoryService::new(db.open(), cfg.clone()).unwrap();
    service
        .call(
            "memory_recall",
            json!({"bootstrap":true,"offline":true,"query":"全局回答要求",
        "request_id":"m15-plan-bootstrap","at":CLOCK}),
            || false,
        )
        .unwrap();
    for entity in case["entities"].as_array().unwrap() {
        service
            .call("memory_entity_upsert", entity.clone(), || false)
            .unwrap();
    }
    let entities = db
        .open()
        .read()
        .unwrap()
        .entities
        .list_by_owner(&UserId::from(OWNER));
    let entity_ids = entities.iter().map(|e| e.id.clone()).collect::<Vec<_>>();
    let relations = w2::relation_catalog(&cfg.predicates, &cfg.memory_types).unwrap();
    let mut requests = BTreeMap::new();
    let mut turns = Vec::new();
    let mut history = Vec::new();
    for message in case["messages"].as_array().unwrap() {
        turns.push((
            format!("prepare:{}", message["id"].as_str().unwrap()),
            message["text"].as_str().unwrap().to_owned(),
            history.clone(),
            message["observed_at"].as_i64().unwrap(),
        ));
        history.push(message["text"].as_str().unwrap().to_owned());
    }
    for question in case["questions"].as_array().unwrap() {
        turns.push((
            format!("recall:{}", question["id"].as_str().unwrap()),
            question["query"].as_str().unwrap().to_owned(),
            serde_json::from_value(question["context"].clone()).unwrap(),
            question["at"].as_i64().unwrap(),
        ));
    }
    for (op, query, context, at) in turns {
        let said_on = time::OffsetDateTime::from_unix_timestamp(at)
            .unwrap()
            .to_offset(time::UtcOffset::from_whole_seconds(cfg.utc_offset_seconds).unwrap())
            .date()
            .to_string();
        let ctx = r1::Context {
            query: &query,
            context: &context,
            entities: &entity_ids,
            relations: &relations,
            subject_ids: None,
            task_id: None,
            said_on: &said_on,
            utc_offset_seconds: cfg.utc_offset_seconds,
            supported_units: recall_models::SUPPORTED_UNITS,
        };
        let mut wire = hippocampus::structured_request(
            ModelStage::R1,
            "r1_v2",
            recall_models::R1_PROMPT,
            recall_models::r1_input(&ctx, &entities),
            recall_models::r1_schema(),
            cfg.model_budget.as_ref().unwrap(),
        );
        wire["model"] = json!(cfg.recall_models.as_ref().unwrap().r1);
        cfg.recall_models
            .as_ref()
            .unwrap()
            .effort("r1")
            .apply(&mut wire);
        requests.insert(op, normalized(&wire, &BTreeMap::new()).unwrap());
    }
    requests
}

fn repair_plan(case: &Value, archive: &Value, configuration: &Value) -> (Value, Value, u64, usize) {
    let current_r1 = r1_requests(case);
    let mut prefixes = serde_json::Map::new();
    let mut slots = serde_json::Map::new();
    let mut reused = 0;
    for (op, request) in current_r1 {
        let fp = recall_models::fingerprint(&request);
        let old = &archive["operations"][&op][0];
        let same = old["request"] == fp && old["request_content"] == request;
        prefixes.insert(op.clone(), if same { json!([fp]) } else { json!([]) });
        slots.insert(
            op,
            if same {
                json!(["R4"])
            } else {
                json!(["R1", "R4"])
            },
        );
        reused += usize::from(same);
    }
    for (i, message) in case["messages"].as_array().unwrap().iter().enumerate() {
        let op = format!("write:{}", message["id"].as_str().unwrap());
        prefixes.insert(op.clone(), json!([]));
        // The first source has an empty record/relation snapshot; production skips W4.
        slots.insert(
            op,
            if i == 0 {
                json!(["W1", "W3"])
            } else {
                json!(["W1", "W3", "W4"])
            },
        );
    }
    let ceiling = slots
        .values()
        .map(|s| s.as_array().unwrap().len() as u64)
        .sum();
    let mut reserved = BTreeMap::from([("W1", 0), ("W3", 0), ("W4", 0), ("R1", 0), ("R4", 0)]);
    for s in slots.values().flat_map(|s| s.as_array().unwrap()) {
        *reserved.get_mut(s.as_str().unwrap()).unwrap() += 1;
    }
    let plan = json!({"version":"eval.e2e.targeted@1","call_ceiling":ceiling,"reserved":reserved,
        "cases":{"m15_mvp":{"prefixes":prefixes,"slots":slots}}});
    let document = json!({"version":"eval.e2e.v3@1","configuration":configuration,
        "reuse_configuration":archive["configuration"],"operations":archive.get("operations").cloned().unwrap_or(json!({}))});
    (document, plan, ceiling, reused)
}

fn campaign_recorder(
    dir: PathBuf,
    limit: Option<u64>,
    live: Option<(String, String)>,
    document: Value,
    plan: Value,
) -> Recorder {
    let mut recorder = Recorder::new(dir, limit, live);
    recorder.configuration = document["configuration"].clone();
    recorder.seed_mvp(document, plan);
    recorder
}

pub(super) fn run() {
    let case = sample();
    let dir = std::env::var_os("EVAL_M15_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/eval-m15/mvp-af56"));
    let archive_path = std::env::var_os("EVAL_M15_REUSE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("tests/eval/m15/recordings/mvp"))
        .join("m15_mvp.json");
    assert_ne!(
        dir.join("m15_mvp.json"),
        archive_path,
        "repair must not overwrite its archive"
    );
    if dir.join("m15_mvp.json").exists() && archive_path.exists() {
        assert_ne!(
            std::fs::canonicalize(dir.join("m15_mvp.json")).unwrap(),
            std::fs::canonicalize(&archive_path).unwrap(),
            "repair must not overwrite its archive"
        );
    }
    let mut cfg = mvp_config("configuration", "");
    cfg.model_budget = None;
    let configuration = serde_json::to_value(cfg).unwrap();
    let archive: Value = if dir.join("m15_mvp.json").exists() {
        let doc: Value =
            serde_json::from_slice(&std::fs::read(dir.join("m15_mvp.json")).unwrap()).unwrap();
        assert_eq!(
            doc["configuration"], configuration,
            "repair campaign configuration changed"
        );
        doc
    } else if archive_path.exists() {
        serde_json::from_slice(&std::fs::read(&archive_path).unwrap()).unwrap()
    } else {
        json!({})
    };
    let (document, plan, ceiling, reused) = repair_plan(&case, &archive, &configuration);
    let spent = sent_before(&dir).unwrap();
    let remaining = RESERVED
        .checked_sub(spent)
        .expect("MVP campaign already exceeds approved 24 sends");
    let limit = std::env::var("EVAL_RECORD")
        .ok()
        .map(|s| s.parse().expect("EVAL_RECORD integer"));
    preflight(limit, ceiling).unwrap();
    println!("本目录累计已发送/在途 {spent}；本轮最多新增 {remaining}；累计上限 24。");
    println!("M1.5 MVP：预登记 child-demo（0 调用）；4 prepare + 4 后台整理 + 新会话 2 recall。");
    println!(
        "严格匹配可复用 R1={reused}/6；新发送槽位上限 {ceiling}：{}；W4 首条跳过，后续按模型结果可能少发，不增加重试。",
        plan["reserved"]
    );
    println!(
        "历史 W1 low/提示词、W3/W4 提示词、R4 v5/提示词变化均按完整指纹过期；不改旧请求或指纹。"
    );
    assert!(
        std::process::Command::new("python3")
            .args(["tests/eval/m15/token_estimate.py", "mvp"])
            .arg(plan["reserved"].to_string())
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .status()
            .unwrap()
            .success()
    );
    let cfg = mvp_config("m15_mvp", "");
    let budget = cfg.model_budget.as_ref().unwrap();
    let output = 4 * 32768
        + 7 * 16384
        + plan["reserved"]["R1"].as_u64().unwrap() * budget.query_output_tokens as u64
        + 6 * budget.review_output_tokens as u64;
    println!(
        "新发送输入容量约 {}（{ceiling} × {} 字节/3）；输出配置上限合计 {}；动态证据未知，不把容量当实际 token 上界。",
        (ceiling * budget.max_input_bytes as u64).div_ceil(3),
        budget.max_input_bytes,
        output
    );
    if std::env::var("EVAL_DRY_RUN").as_deref() == Ok("1") {
        println!(
            "干跑：真实调用 0；未读取密钥、未创建录制。标注 {}；未来实际发送次数由语义输出决定。",
            case["label_status"]
        );
        return;
    }
    let output = std::env::var_os("EVAL_M15_OUTPUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/eval-m15/mvp-run.json"));
    let live = limit.filter(|_| remaining > 0).map(|_| {
        (
            format!(
                "{}/responses",
                std::env::var("HUOSHAN_DEEPSEEK_BASE_URL")
                    .expect("live base URL")
                    .trim_end_matches('/')
            ),
            std::env::var("HUOSHAN_DEEPSEEK_API_KEY").expect("live API key"),
        )
    });
    let recorder = Arc::new(campaign_recorder(
        dir.clone(),
        limit.map(|n| remaining.min(n)),
        live,
        document,
        plan,
    ));
    // Missing offline replay does not even need a listening socket.
    let mut result = if limit.is_none()
        && !archive_path.exists()
        && !dir.join("m15_mvp.json").exists()
    {
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
        &json!({"pipeline":"m15","label_status":case["label_status"],"cases":[result],
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
    let cfg = mvp_config(id, address);
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
fn m15_mvp_dimensions_keep_the_24_send_authorization_cap() {
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
    assert_eq!(RESERVED, 24);
}

#[test]
fn m15_repair_plan_reuses_only_exact_r1_requests_and_reserves_changed_stages() {
    let case = sample();
    let requests = r1_requests(&case);
    let operations: serde_json::Map<String, Value> = requests
        .iter()
        .map(|(op, request)| {
            (
                op.clone(),
                json!([{"request":recall_models::fingerprint(request),
            "request_content":request,"http_status":200,"body":"{}"}]),
            )
        })
        .collect();
    let archive = json!({"configuration":{"old":true},"operations":operations});
    let configuration = json!({"repair":true});
    let (document, plan, ceiling, reused) = repair_plan(&case, &archive, &configuration);
    assert_eq!((ceiling, reused), (17, 6));
    assert!(preflight(Some(16), ceiling).is_err());
    assert!(preflight(Some(17), ceiling).is_ok());
    assert_eq!(
        plan["reserved"],
        json!({"W1":4,"W3":4,"W4":3,"R1":0,"R4":6})
    );
    assert_eq!(document["operations"], archive["operations"]);
    assert_eq!(document["reuse_configuration"], archive["configuration"]);
    let dir = std::env::temp_dir().join(format!("m15-repair-plan-{}", diagnostics::new_id()));
    let recorder = campaign_recorder(dir.clone(), None, None, document, plan);
    let (op, request) = requests.first_key_value().unwrap();
    let mut wire = request.clone();
    wire["input"] = json!(request["input"].to_string());
    recorder.begin("m15_mvp", op, &BTreeMap::new());
    assert_eq!(
        recorder
            .respond("m15_mvp", &wire, || panic!("exact replay must not send"))
            .0,
        200
    );
    let mut changed = request.clone();
    changed["reasoning"] = json!({"effort":"low"});
    wire["reasoning"] = changed["reasoning"].clone();
    recorder.begin("m15_mvp", op, &BTreeMap::new());
    assert_eq!(
        recorder
            .respond("m15_mvp", &wire, || panic!(
                "changed retained prefix must not send"
            ))
            .0,
        503
    );
    assert_eq!(recorder.sent.load(Ordering::Relaxed), 0);
    let mut stale = archive.clone();
    stale["operations"][op][0]["request_content"] = changed;
    // A matching stored hash alone cannot authorize reuse of altered content.
    assert_eq!(repair_plan(&case, &stale, &configuration).2, 18);
    assert_eq!(repair_plan(&case, &json!({}), &configuration).2, 23);
    if dir.exists() {
        std::fs::remove_dir_all(dir).unwrap();
    }
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
        let request = json!({"model":PRO,"input":json!({"source":{"kind":"stored","source_id":source},"quote":{"text":"测试用虚构原句。"}}).to_string(),"text":{"format":{"name":"r1_v2"}}});
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
            "w1_v4" => {
                let text = &input["current_user_message"];
                json!({"candidates":[{"kind":"assertion","speech":"user_statement","subject":{"entity_id":"self"},
                    "relation":"fact","memory_type":"fact","value":{"kind":"text","text":text,"quote":{"text":text}},
                    "sentence":{"text":text},"polarity":"affirm","strength":null,"uncertainty":null,"conditions":[],"times":[]}],"memory_requests":[]})
            }
            "w3_v5" => json!({"reviews":input["candidates"].as_array().unwrap().iter().map(|c|
                json!({"candidate":c["index"],"verdict":"supported","problems":[],"episode_value":"not_applicable"})).collect::<Vec<_>>(),
                "coverage":{"status":"complete","missing":[]},"request_reviews":[]}),
            "w4_v5" => {
                let c = &input["w4_input"]["candidates"][0];
                let r = &input["w4_input"]["records"][0];
                json!({"decisions":[{"candidate":c["index"],"status":"decided","action":"reinforce","targets":[r["id"]],
                    "resolves_relations":[],"basis":[{"source":c["source"],"quote":c["candidate"]["sentence"]},r["evidence"][0]],
                    "effective":null,"relation_scope":null,"pending_reason":null}]})
            }
            "r1_v2" => {
                json!({"need":"judge","current_evidence":"assertion_present","type":"single","subject":{"entity_id":"self"},
                "relation":"fact","conditions":[],"time":null,"plan":{"op":"lookup","filter":{"relation":"fact","time":null},"unit":null}})
            }
            "r4_v7" => {
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
        for step in &writes {
            if step["source"]["enrichment"] != "committed" {
                eprintln!(
                    "M15_WRITE_NOT_COMMITTED {}",
                    json!({"message":step["message"],
                    "enrichment":step["source"]["enrichment"],"error":step["source"]["error"],
                    "round":step["source"]["task"]["round"],"model_diagnostics":step["model_diagnostics"],
                    "diagnostic":step["source"]["result"]["w4_input_meta"]["diagnostic"]})
                );
            }
        }
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
