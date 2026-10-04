//! Opt-in end-to-end harness. Production owns all five model stages and their prompts.
use super::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

const LABEL: &str = "ac1-pro-medium-flash-medium-pro-minimal";
const PRO: &str = "ep-20261003130756-trhj6";
const FLASH: &str = "ep-20260923215039-vxgxg";
const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/eval");

fn config(case: &str, address: &str, calls: u64) -> ChatConfig {
    let mut config: ChatConfig = serde_json::from_value(json!({
        "write_pipeline":"w4_runtime_v1", "recall_pipeline":"recall_v3",
        "enrichment_mode":"deferred", "fact_index_enabled":true,
        "background_operation_limit":3,
        "write_models":{"w1":PRO,"w3":FLASH,"w4":PRO,
            "reasoning_effort":{"w1":"medium","w3":"medium","w4":"medium"}},
        "recall_models":{"r1":PRO,"r4":PRO,
            "reasoning_effort":{"r1":"minimal","r4":"minimal"}},
        "model_budget":{"group_id":format!("eval-v3-{case}"),
            "max_calls":calls.max(1),"max_operation_calls":2,
            "max_input_bytes":256000,"query_output_tokens":4096,"review_output_tokens":16384}
    }))
    .unwrap();
    config.model_base_url = Some(format!("http://{address}/v1/case/{case}"));
    config
}

fn cases() -> Vec<Value> {
    let only = std::env::var("EVAL_CASES").unwrap_or_default();
    let only = if only == "quick" {
        QUICK.join(",")
    } else {
        only
    };
    let mut paths: Vec<_> = std::fs::read_dir(Path::new(ROOT).join("cases"))
        .unwrap()
        .map(|p| p.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .filter(|p| {
            only.is_empty()
                || only.split(',').any(|s| {
                    p.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with(s.trim())
                })
        })
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "no eval cases");
    paths
        .iter()
        .map(|p| serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap())
        .collect()
}

fn counts(case: &Value) -> [u64; 5] {
    let m = case["messages"].as_array().unwrap().len() as u64;
    let q = case["questions"].as_array().unwrap().len() as u64;
    // Fresh databases contain only entities, never facts: the first message cannot call W4.
    [m, m, m.saturating_sub(1), q, q]
}

fn preflight(limit: Option<u64>, ceiling: u64) -> Result<(), String> {
    if let Some(limit) = limit
        && limit < ceiling
    {
        return Err(format!(
            "整批拒绝：EVAL_RECORD={limit} < 调用上限 {ceiling}（未读取凭据、未发送）"
        ));
    }
    Ok(())
}

fn plan(cases: &[Value]) -> u64 {
    println!(
        "配置 {LABEL}；write=w4_runtime_v1，recall=recall_v3；逐消息显式 enrich_source，无重试。"
    );
    println!(
        "预计列是容量规划情景：W1/W3 有效且后续写入/每题均需模型判定；不是模型质量预测。W4/R4 实际可为 0；W3 也可能跳过。"
    );
    println!("case | W1 | W3 | W4预计 | W4上限 | R1 | R4预计 | R4上限 | 总上限");
    let mut total = [0; 5];
    for case in cases {
        let n = counts(case);
        for i in 0..5 {
            total[i] += n[i];
        }
        println!(
            "{} | {} | {} | {} | {} | {} | {} | {} | {}",
            case["id"].as_str().unwrap(),
            n[0],
            n[1],
            n[2],
            n[2],
            n[3],
            n[4],
            n[4],
            n.iter().sum::<u64>()
        );
    }
    let ceiling = total.iter().sum();
    println!(
        "合计 | {} | {} | {} | {} | {} | {} | {} | {ceiling}",
        total[0], total[1], total[2], total[2], total[3], total[4], total[4]
    );
    // Historical recordings lack separate W1/W3/W4 elapsed times. These pooled averages
    // are explicitly proxies, not stage medians or a wall-clock upper bound.
    let seconds = (total[0] + total[2]) as f64 * (3782.19 / 82.0)
        + total[1] as f64 * (2189.09 / 61.0)
        + total[3] as f64 * 2.5
        + total[4] as f64 * 4.5;
    println!(
        "耗时参考：W1/W4≈46.1s（Pro 混合82次/3782.19s），W3≈35.9s（Flash 混合61次/2189.09s，代理估计）；R1≈2.5s、R4≈4.5s（minimal p50）。"
    );
    println!(
        "上述情景串行约 {seconds:.0}s / {:.1}分钟 / {:.2}小时；无独立 W3 时延，非可靠上界。新模式逐用例串行，EVAL_JOBS 不适用。",
        seconds / 60.0,
        seconds / 3600.0
    );
    ceiling
}

/// Only volatile *structural* source identifiers are canonicalized. Never rewrite quotes,
/// sentences, values, USER text/history, or arbitrary string leaves. IDs in database records
/// and relations are deterministic counters and remain fully fingerprinted.
fn ids(value: &mut Value, mapping: &BTreeMap<String, String>) {
    match value {
        Value::Array(a) => {
            for v in a {
                ids(v, mapping);
            }
        }
        Value::Object(o) => {
            if let Some(Value::String(id)) = o.get_mut("source_id")
                && let Some(stable) = mapping.get(id)
            {
                *id = stable.clone();
            }
            if let Some(Value::Array(snapshot)) = o.get_mut("source_snapshot")
                && let Some(Value::String(id)) = snapshot.first_mut()
                && let Some(stable) = mapping.get(id)
            {
                *id = stable.clone();
            }
            if let Some(Value::Object(sources)) = o.get_mut("sources") {
                *sources = std::mem::take(sources)
                    .into_iter()
                    .map(|(k, v)| (mapping.get(&k).cloned().unwrap_or(k), v))
                    .collect();
            }
            for v in o.values_mut() {
                ids(v, mapping);
            }
        }
        _ => {}
    }
}

fn normalized(request: &Value, mapping: &BTreeMap<String, String>) -> Value {
    let mut value = request.clone();
    let mut input: Value =
        serde_json::from_str(value["input"].as_str().expect("production JSON input")).unwrap();
    ids(&mut input, mapping);
    value["input"] = input;
    value
}

fn response_ids(body: &str, mapping: &BTreeMap<String, String>) -> String {
    // Preserve failed, non-JSON and partial model responses verbatim for diagnostics/replay.
    let Ok(mut envelope) = serde_json::from_str::<Value>(body) else {
        return body.into();
    };
    if let Some(output) = envelope.get_mut("output").and_then(Value::as_array_mut) {
        for message in output {
            if let Some(content) = message.get_mut("content").and_then(Value::as_array_mut) {
                for part in content {
                    if part["type"] == "output_text"
                        && let Some(text) = part["text"].as_str()
                        && let Ok(mut value) = serde_json::from_str::<Value>(text)
                    {
                        ids(&mut value, mapping);
                        part["text"] = json!(value.to_string());
                    }
                }
            }
        }
    }
    envelope.to_string()
}

#[derive(Default)]
struct Operation {
    id: String,
    position: usize,
    missing: Option<String>,
    // runtime -> stable message ID; registered after T1, before W1.
    ids: BTreeMap<String, String>,
}

struct Recorder {
    dir: PathBuf,
    configuration: Value,
    allowance: Mutex<Option<u64>>,
    live: Option<(String, String)>,
    operations: Mutex<HashMap<String, Operation>>,
    documents: Mutex<HashMap<String, Value>>,
    sent: AtomicUsize,
}

impl Recorder {
    fn new(dir: PathBuf, limit: Option<u64>, live: Option<(String, String)>) -> Self {
        let mut cfg = config("configuration", "", 1);
        cfg.model_budget = None;
        Self {
            dir,
            configuration: serde_json::to_value(cfg).unwrap(),
            allowance: Mutex::new(limit),
            live,
            operations: Mutex::default(),
            documents: Mutex::default(),
            sent: AtomicUsize::new(0),
        }
    }
    fn begin(&self, case: &str, operation: &str, source_ids: &BTreeMap<String, String>) {
        self.operations.lock().unwrap().insert(
            case.into(),
            Operation {
                id: operation.into(),
                ids: source_ids
                    .iter()
                    .map(|(stable, runtime)| (runtime.clone(), stable.clone()))
                    .collect(),
                ..Operation::default()
            },
        );
        let mut docs = self.documents.lock().unwrap();
        if !docs.contains_key(case) {
            let path = self.dir.join(format!("{case}.json"));
            let doc = if path.exists() {
                serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
            } else {
                json!({"version":"eval.e2e.v3@1","configuration":self.configuration,"operations":{}})
            };
            assert_eq!(
                doc["configuration"], self.configuration,
                "配置标签已绑定其他配置，必须使用单独目录"
            );
            docs.insert(case.into(), doc);
        }
    }
    fn missing(&self, case: &str) -> Option<String> {
        self.operations.lock().unwrap()[case].missing.clone()
    }
    /// Inject the *send* only in harness unit tests; production requests are never constructed here.
    fn respond(
        &self,
        case: &str,
        request: &Value,
        send: impl FnOnce() -> Result<(u16, String), String>,
    ) -> (u16, String) {
        let (operation, position, mapping) = {
            let mut ops = self.operations.lock().unwrap();
            let op = ops.get_mut(case).unwrap();
            let result = (op.id.clone(), op.position, op.ids.clone());
            op.position += 1;
            result
        };
        let canonical = normalized(request, &mapping);
        let fp = recall_models::fingerprint(&canonical);
        let reverse = mapping
            .iter()
            .map(|(a, b)| (b.clone(), a.clone()))
            .collect();
        let saved =
            self.documents.lock().unwrap()[case]["operations"][&operation][position].clone();
        if saved["request"] == fp {
            return (
                saved["http_status"].as_u64().unwrap() as u16,
                response_ids(saved["body"].as_str().unwrap(), &reverse),
            );
        }
        let allowed = {
            let mut limit = self.allowance.lock().unwrap();
            match limit.as_mut() {
                Some(n) if *n > 0 => {
                    *n -= 1;
                    true
                }
                _ => false,
            }
        };
        if !allowed {
            let reason = if saved.is_null() {
                "未录制"
            } else {
                "录制过期（请求指纹不符）"
            };
            self.operations
                .lock()
                .unwrap()
                .get_mut(case)
                .unwrap()
                .missing = Some(reason.into());
            return (503, json!({"error":reason}).to_string());
        }
        self.sent.fetch_add(1, Ordering::Relaxed);
        let started = Instant::now();
        let (status, body, failure) = match send() {
            Ok((s, b)) => (s, b, None),
            Err(e) => (
                503,
                json!({"error":"eval_transport_failed"}).to_string(),
                Some(e),
            ),
        };
        let envelope: Value = serde_json::from_str(&body).unwrap_or_default();
        let recording = json!({"schema":request.pointer("/text/format/name"),"endpoint":request["model"],
            "request":fp,"request_content":canonical,"http_status":status,"body":response_ids(&body,&mapping),
            "usage":envelope["usage"],"elapsed_ms":started.elapsed().as_millis(),
            "status":envelope["status"],"incomplete_details":envelope["incomplete_details"],"transport_error":failure});
        let mut docs = self.documents.lock().unwrap();
        let doc = docs.get_mut(case).unwrap();
        let ops = doc["operations"].as_object_mut().unwrap();
        let list = ops
            .entry(operation)
            .or_insert(json!([]))
            .as_array_mut()
            .unwrap();
        // A changed prefix invalidates subsequent stages; never reuse their old output by index.
        list.truncate(position);
        list.push(recording);
        std::fs::create_dir_all(&self.dir).unwrap();
        let target = self.dir.join(format!("{case}.json"));
        let temporary = target.with_extension("json.tmp");
        std::fs::write(
            &temporary,
            serde_json::to_string_pretty(doc).unwrap() + "\n",
        )
        .unwrap();
        std::fs::rename(temporary, target).unwrap();
        (status, body)
    }
}

/// Loopback transport, one worker per physical send. Recording never calls legacy forward()
/// (which retries). Join workers on shutdown so incomplete/late responses retain diagnostics.
fn server(
    recorder: Arc<Recorder>,
    fixture: Option<fn(&Value) -> Value>,
) -> (String, Arc<AtomicBool>, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    listener.set_nonblocking(true).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let stopping = stop.clone();
    let handle = std::thread::spawn(move || {
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(600))
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()
            .unwrap();
        let mut workers = Vec::new();
        while !stopping.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let recorder = recorder.clone();
                    let http = http.clone();
                    workers.push(std::thread::spawn(move || {
                        stream.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
                        let mut reader = BufReader::new(stream.try_clone().unwrap());
                        let mut line = String::new(); reader.read_line(&mut line).unwrap();
                        let case = line.split(' ').nth(1).unwrap().split('/').nth(3).unwrap().to_owned();
                        let mut length = 0;
                        loop {
                            line.clear(); reader.read_line(&mut line).unwrap();
                            if line == "\r\n" || line.is_empty() { break; }
                            if let Some((k,v)) = line.split_once(':') && k.eq_ignore_ascii_case("content-length") { length = v.trim().parse().unwrap(); }
                        }
                        let mut body = vec![0; length]; reader.read_exact(&mut body).unwrap();
                        let request: Value = serde_json::from_slice(&body).unwrap();
                        let (status,body) = recorder.respond(&case,&request, || {
                            if let Some(fixture) = fixture {
                                return Ok((200,json!({"status":"completed","usage":{"input_tokens":10,"output_tokens":20},
                                    "output":[{"type":"message","content":[{"type":"output_text","text":fixture(&request).to_string()}]}]}).to_string()));
                            }
                            let (url,key) = recorder.live.as_ref().expect("preflight authorized live transport");
                            let response = http.post(url).bearer_auth(key).json(&request).send().map_err(|e| e.to_string())?;
                            let status = response.status().as_u16();
                            Ok((status,response.text().map_err(|e| e.to_string())?))
                        });
                        let _ = write!(stream,"HTTP/1.1 {status} Eval\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());
                    }));
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(e) => panic!("eval listener: {e}"),
            }
        }
        for worker in workers {
            worker.join().unwrap();
        }
    });
    (address, stop, handle)
}

fn run_case(case: &Value, recorder: &Recorder, address: &str) -> Value {
    let case_id = case["id"].as_str().unwrap();
    let db = TempDb(std::env::temp_dir().join(format!(
        "eval-v3-{case_id}-{}.sqlite3",
        diagnostics::new_id()
    )));
    let mut service = ChatMemoryService::new(
        db.open(),
        config(case_id, address, counts(case).iter().sum()),
    )
    .unwrap();
    service.call("memory_recall",json!({"query":"全局回答要求","bootstrap":true,"request_id":format!("{case_id}-bootstrap"),"at":CLOCK}),|| false).unwrap();
    for entity in case["entities"].as_array().unwrap() {
        service
            .call("memory_entity_upsert", entity.clone(), || false)
            .unwrap();
    }
    let messages = case["messages"].as_array().unwrap();
    let mut source_ids = BTreeMap::<String, String>::new();
    let mut steps = Vec::new();
    let mut blocked = None;
    for (index, message) in messages.iter().enumerate() {
        let id = message["id"].as_str().unwrap();
        if blocked.is_none() {
            let history: Vec<_> = messages[..index]
                .iter()
                .filter(|m| m.get("task_id") == message.get("task_id"))
                .map(|m| m["text"].clone())
                .collect();
            let mut args = json!({"text":message["text"],"session_id":format!("eval-{case_id}"),"turn_id":id,
                "history":history,"capture_source":true,"observed_at":message["observed_at"].as_i64().unwrap_or(CLOCK+60*index as i64)});
            if let Some(task) = message.get("task_id") {
                args["task_id"] = task.clone();
            }
            let received = service.call("memory_remember", args, || false).unwrap();
            let source = received
                .pointer("/source/source_id")
                .and_then(Value::as_str)
                .expect("T1 source ID");
            source_ids.insert(id.into(), source.into());
            recorder.begin(case_id, &format!("write:{id}"), &source_ids);
            let result = service.enrich_source(source).map_err(|e| e.to_string());
            blocked = recorder.missing(case_id);
            steps.push(json!({"step":"message","message":id,"received":received,"result":result,"missing":blocked}));
        }
        for question in case["questions"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|q| q["after"].as_u64() == Some(index as u64 + 1))
        {
            let id = question["id"].as_str().unwrap();
            let at = question["at"]
                .as_i64()
                .unwrap_or(CLOCK + 60 * index as i64 + 30);
            let mut args = json!({"query":question["query"],"context":question["context"],"at":at,
                "request_id":format!("eval-{case_id}-{id}")});
            if let Some(task) = question.get("task_id") {
                args["task_id"] = task.clone();
            }
            if let Some(reason) = &blocked {
                steps.push(json!({"step":"question","question":id,"state":"not_recorded","reason":reason,"at":at}));
                continue;
            }
            recorder.begin(case_id, &format!("recall:{id}"), &source_ids);
            let result = service
                .call("memory_recall", args, || false)
                .unwrap_or_else(|e| panic!("{case_id}/{id}: {e}"));
            let missing = recorder.missing(case_id);
            let state = db.open().read().unwrap();
            let source_state: BTreeMap<_, _> = source_ids
                .iter()
                .map(|(stable, runtime)| {
                    let live = state
                        .sources
                        .values()
                        .any(|s| &s.id == runtime && s.request.is_some());
                    (stable, json!({"linkable":live,"deleted":!live}))
                })
                .collect();
            steps.push(json!({"step":"question","question":id,"at":at,
                "state":if missing.is_some() {"not_recorded"} else {"completed"},"reason":missing,
                "result":result,"source_ids":source_ids,"source_state":source_state}));
        }
    }
    json!({"case":case_id,"steps":steps})
}

pub(super) fn run() {
    let cases = cases();
    let ceiling = plan(&cases);
    let limit = std::env::var("EVAL_RECORD")
        .ok()
        .map(|s| s.parse::<u64>().expect("EVAL_RECORD must be an integer"));
    preflight(limit, ceiling).unwrap_or_else(|e| panic!("{e}"));
    if std::env::var("EVAL_DRY_RUN").as_deref() == Ok("1") {
        println!("干跑：真实调用 0；未读取凭据、未创建录制。");
        return;
    }
    let dir = std::env::var_os("EVAL_V3_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(ROOT).join("end_to_end").join(LABEL));
    // Dry/preflight precede credential reads and binding a socket, including empty replay.
    let live = limit.map(|_| {
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
    let recorder = Arc::new(Recorder::new(dir.clone(), limit, live));
    let empty = limit.is_none()
        && cases.iter().all(|c| {
            !dir.join(format!("{}.json", c["id"].as_str().unwrap()))
                .exists()
        });
    let results = if empty {
        cases.iter().map(|c| json!({"case":c["id"],"steps":c["questions"].as_array().unwrap().iter().map(|q|
            json!({"step":"question","question":q["id"],"state":"not_recorded","reason":"未录制"})).collect::<Vec<_>>()})).collect::<Vec<_>>()
    } else {
        let (address, stop, handle) = server(recorder.clone(), None);
        // Only this suite mutates provider environment, before constructing production clients.
        unsafe {
            std::env::set_var("PMR_MODEL_NETWORK", "offline");
            std::env::set_var("DEEPSEEK_API_KEY", "eval-replay");
        }
        let results = cases
            .iter()
            .map(|c| run_case(c, &recorder, &address))
            .collect();
        stop.store(true, Ordering::SeqCst);
        handle.join().unwrap();
        results
    };
    let output = std::env::var_os("EVAL_V3_OUTPUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| dir.join("run.json"));
    std::fs::create_dir_all(output.parent().unwrap()).unwrap();
    std::fs::write(&output,serde_json::to_string_pretty(&json!({"pipeline":"v3","label":LABEL,
        "configuration":recorder.configuration,"cases":results,"real_calls":recorder.sent.load(Ordering::Relaxed)})).unwrap()+"\n").unwrap();
    println!(
        "真实调用 {}；运行输出 {}；空录制记为未录制，不计通过。",
        recorder.sent.load(Ordering::Relaxed),
        output.display()
    );
}

#[test]
fn v3_full_plan_and_preflight_are_offline() {
    let cases = cases();
    // Do not depend on EVAL_CASES: this test reads the full immutable corpus explicitly.
    if std::env::var_os("EVAL_CASES").is_none() {
        assert_eq!(cases.len(), 35);
        let total = cases.iter().map(counts).fold([0; 5], |mut a, n| {
            for i in 0..5 {
                a[i] += n[i];
            }
            a
        });
        assert_eq!(total, [204, 204, 169, 63, 63]);
        assert!(preflight(Some(702), 703).is_err());
        assert!(preflight(Some(703), 703).is_ok());
        assert!(preflight(None, 703).is_ok());
    }
    let cfg = config("test", "local", 9);
    assert_eq!(cfg.write_models.as_ref().unwrap().w1, PRO);
    assert_eq!(cfg.write_models.as_ref().unwrap().w3, FLASH);
    assert_eq!(cfg.recall_models.as_ref().unwrap().r4, PRO);
    assert_eq!(cfg.background_operation_limit, Some(3));
}

#[test]
fn v3_source_ids_preserve_text_and_strict_request_binding() {
    let request = json!({"model":PRO,"input":json!({"source_snapshot":["random", "session", "m1"],
        "current_user_message":"random","sources":{"random":{"text":"random"}},
        "source":{"kind":"stored","source_id":"random"},"quote":{"text":"random"}}).to_string(),
        "instructions":"production","reasoning":{"effort":"medium"},"max_output_tokens":32768});
    let map = BTreeMap::from([("random".into(), "m1".into())]);
    let canonical = normalized(&request, &map);
    assert_eq!(canonical["input"]["source_snapshot"][0], "m1");
    assert_eq!(canonical["input"]["sources"]["m1"]["text"], "random");
    assert_eq!(canonical["input"]["current_user_message"], "random");
    assert_eq!(canonical["input"]["quote"]["text"], "random");
    let mut next = request.clone();
    let mut input = serde_json::from_str::<Value>(next["input"].as_str().unwrap()).unwrap();
    input["source_snapshot"][0] = json!("next");
    input["source"]["source_id"] = json!("next");
    input["sources"] = json!({"next":{"text":"random"}});
    next["input"] = json!(input.to_string());
    assert_eq!(
        canonical,
        normalized(&next, &BTreeMap::from([("next".into(), "m1".into())]))
    );
    for key in ["model", "instructions", "max_output_tokens", "reasoning"] {
        let mut changed = canonical.clone();
        changed[key] = json!("different");
        assert_ne!(
            recall_models::fingerprint(&canonical),
            recall_models::fingerprint(&changed)
        );
    }
    let body = json!({"status":"completed","output":[{"type":"reasoning","summary":[]},
        {"type":"message","content":[{"type":"output_text","text":json!({"evidence":[{
            "source":{"kind":"stored","source_id":"random"},"quote":{"text":"random"}}]}).to_string()}]}]}).to_string();
    let frozen = response_ids(&body, &map);
    let restored: Value = serde_json::from_str(&response_ids(
        &frozen,
        &BTreeMap::from([("m1".into(), "next".into())]),
    ))
    .unwrap();
    assert!(restored["output"][0].get("content").is_none());
    let output: Value = serde_json::from_str(
        restored["output"][1]["content"][0]["text"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(output["evidence"][0]["source"]["source_id"], "next");
    assert_eq!(output["evidence"][0]["quote"]["text"], "random");
}

#[test]
fn v3_failed_and_incomplete_responses_are_recorded_once_and_replayed() {
    let dir = std::env::temp_dir().join(format!("eval-v3-recorder-{}", diagnostics::new_id()));
    let recorder = Recorder::new(dir.clone(), Some(3), None);
    let request = json!({"model":PRO,"input":"{}","text":{"format":{"name":"w1_v1"}}});
    for (id,response) in [
        ("incomplete",Ok((200,json!({"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"usage":{"output_tokens":33}}).to_string()))),
        ("http",Ok((429,json!({"error":"limited"}).to_string()))),
        ("transport",Err("connection closed".into()))
    ] {
        recorder.begin("case",id,&BTreeMap::new());
        let first=recorder.respond("case",&request,|| response);
        recorder.begin("case",id,&BTreeMap::new());
        assert_eq!(first,recorder.respond("case",&request,|| panic!("must not retry")));
    }
    assert_eq!(recorder.sent.load(Ordering::Relaxed), 3);
    let document: Value =
        serde_json::from_slice(&std::fs::read(dir.join("case.json")).unwrap()).unwrap();
    assert_eq!(
        document["operations"]["incomplete"][0]["incomplete_details"]["reason"],
        "max_output_tokens"
    );
    assert_eq!(
        document["operations"]["incomplete"][0]["usage"]["output_tokens"],
        33
    );
    assert_eq!(
        document["operations"]["transport"][0]["transport_error"],
        "connection closed"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v3_empty_and_stale_replay_never_send() {
    let recorder = Recorder::new(PathBuf::new(), None, None);
    recorder.begin("case", "write:m1", &BTreeMap::new());
    let request = json!({"model":PRO,"input":"{}"});
    assert_eq!(
        recorder
            .respond("case", &request, || panic!("no real call"))
            .0,
        503
    );
    assert_eq!(recorder.missing("case").as_deref(), Some("未录制"));
    recorder.documents.lock().unwrap().get_mut("case").unwrap()["operations"]["write:m1"] =
        json!([{"request":"stale"}]);
    recorder.begin("case", "write:m1", &BTreeMap::new());
    assert_eq!(
        recorder
            .respond("case", &request, || panic!("no stale retry"))
            .0,
        503
    );
    assert!(recorder.missing("case").unwrap().contains("过期"));
}

#[test]
fn v3_production_http_roundtrip_uses_all_five_stages() {
    // Synthetic HTTP outputs exercise production; they are never corpus evaluation results.
    fn fixture(request: &Value) -> Value {
        let input: Value = serde_json::from_str(request["input"].as_str().unwrap()).unwrap();
        assert!(input.get("model").is_none());
        let name = request
            .pointer("/text/format/name")
            .unwrap()
            .as_str()
            .unwrap();
        assert_eq!(request["model"], if name == "w3_v1" { FLASH } else { PRO });
        assert_eq!(
            request["reasoning"]["effort"],
            if name.starts_with('r') {
                "minimal"
            } else {
                "medium"
            }
        );
        match name {
            "w1_v1" => {
                let text = &input["current_user_message"];
                json!({"candidates":[{
                "kind":"assertion","speech":"user_statement","subject":{"entity_id":"self"},
                "relation":"fact","memory_type":"fact","value":{"kind":"text","text":text,"quote":{"text":text}},
                "sentence":{"text":text},"polarity":"affirm","strength":null,"uncertainty":null,
                "conditions":[],"times":[]}],"memory_requests":[]})
            }
            "w3_v1" => json!({"reviews":input["candidates"].as_array().unwrap().iter().map(|c|
                json!({"candidate":c["index"],"verdict":"supported","problems":[],"episode_value":"not_applicable"})).collect::<Vec<_>>(),
                "coverage":{"status":"complete","missing":[]},"request_reviews":[]}),
            "w4_v2" => {
                let c = &input["w4_input"]["candidates"][0];
                let r = &input["w4_input"]["records"][0];
                json!({"decisions":[{"candidate":c["index"],"status":"decided","action":"reinforce","targets":[r["id"]],
                    "resolves_relations":[],"basis":[{"source":c["source"],"quote":c["candidate"]["sentence"]},r["evidence"][0]],
                    "effective":null,"relation_scope":null,"pending_reason":null}]})
            }
            "r1_v1" => {
                json!({"need":"judge","current_evidence":"query_only","type":"single","subject":{"entity_id":"self"},
                "relation":"研究","conditions":[],"time":null,"plan":{"op":"lookup","filter":{"relation":"fact","time":null},"unit":null}})
            }
            "r4_v3" => {
                let r = &input["evidence"]["records"][0];
                json!({"verdict":"supported","facts":[{
                "statement":r["candidate"]["sentence"]["text"],"evidence":r["evidence"],"record_ids":[r["id"]],
                "polarity":"affirm","strength":null,"uncertainty":null,"conditions":[],"times":[],
                "current":"yes","applies":"yes","supersedes_record_ids":[]}],"compute":null})
            }
            _ => panic!("unexpected stage {name}"),
        }
    }
    let case = json!({"id":"synthetic","entities":[{"id":"self","canonical_name":"本人","aliases":[],"entity_type":"Person"}],
        "messages":[{"id":"m1","text":"我正在研究数据库索引。"},{"id":"m2","text":"我正在研究数据库索引。"}],
        "questions":[{"id":"q","after":2,"query":"我正在研究什么？","context":[]}]});
    let dir = std::env::temp_dir().join(format!("eval-v3-roundtrip-{}", diagnostics::new_id()));
    let recorder = Arc::new(Recorder::new(dir.clone(), Some(7), None));
    let (address, stop, handle) = server(recorder.clone(), Some(fixture));
    unsafe {
        std::env::set_var("PMR_MODEL_NETWORK", "offline");
        std::env::set_var("DEEPSEEK_API_KEY", "eval-replay");
    }
    let first = run_case(&case, &recorder, &address);
    stop.store(true, Ordering::SeqCst);
    handle.join().unwrap();
    assert_eq!(recorder.sent.load(Ordering::Relaxed), 7);
    assert_eq!(first["steps"][2]["result"]["status"], "answered");
    let document: Value =
        serde_json::from_slice(&std::fs::read(dir.join("synthetic.json")).unwrap()).unwrap();
    let names: Vec<_> = document["operations"]
        .as_object()
        .unwrap()
        .values()
        .flat_map(|v| v.as_array().unwrap())
        .map(|r| r["schema"].as_str().unwrap())
        .collect();
    for stage in ["w1_v1", "w3_v1", "w4_v2", "r1_v1", "r4_v3"] {
        assert!(names.contains(&stage));
    }
    let replay = Arc::new(Recorder::new(dir.clone(), None, None));
    let (address, stop, handle) = server(replay.clone(), None);
    let second = run_case(&case, &replay, &address);
    stop.store(true, Ordering::SeqCst);
    handle.join().unwrap();
    assert_eq!(replay.sent.load(Ordering::Relaxed), 0);
    assert_eq!(second["steps"][2]["state"], "completed");
    let mut a = first["steps"][2]["result"].clone();
    let mut b = second["steps"][2]["result"].clone();
    let mapping = |run: &Value| {
        run["steps"][2]["source_ids"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(stable, runtime)| (runtime.as_str().unwrap().to_owned(), stable.clone()))
            .collect::<BTreeMap<_, _>>()
    };
    ids(&mut a, &mapping(&first));
    ids(&mut b, &mapping(&second));
    assert_eq!(a, b);
    std::fs::remove_dir_all(dir).unwrap();
}
