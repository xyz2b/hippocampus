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

fn jobs(value: Option<&str>) -> usize {
    value.map_or(8, |value| {
        value
            .parse()
            .ok()
            .filter(|&n| n > 0)
            .expect("EVAL_JOBS must be a positive integer")
    })
}

fn estimated_seconds(case: &Value) -> f64 {
    let n = counts(case);
    (n[0] + n[2]) as f64 * (3782.19 / 82.0)
        + n[1] as f64 * (2189.09 / 61.0)
        + n[3] as f64 * 2.5
        + n[4] as f64 * 4.5
}

fn estimated_wall_seconds(cases: &[Value], jobs: usize) -> f64 {
    // Simulate the same FIFO case queue with these estimated case durations.
    let mut workers = vec![0.0f64; jobs.min(cases.len())];
    for case in cases {
        let next = (0..workers.len())
            .min_by(|&a, &b| workers[a].total_cmp(&workers[b]))
            .unwrap();
        workers[next] += estimated_seconds(case);
    }
    workers.into_iter().fold(0.0, f64::max)
}

fn plan(cases: &[Value], jobs: usize) -> u64 {
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
    let seconds: f64 = cases.iter().map(estimated_seconds).sum();
    let wall = estimated_wall_seconds(cases, jobs);
    println!(
        "耗时参考：W1/W4≈46.1s（Pro 混合82次/3782.19s），W3≈35.9s（Flash 混合61次/2189.09s，代理估计）；R1≈2.5s、R4≈4.5s（minimal p50）。"
    );
    println!(
        "上述情景串行约 {seconds:.0}s / {:.1}分钟 / {:.2}小时。",
        seconds / 60.0,
        seconds / 3600.0
    );
    println!(
        "EVAL_JOBS={jobs}（有效 {}）：FIFO 用例队列预计墙钟 {wall:.0}s / {:.1}分钟 / {:.2}小时；用例内严格串行。无独立 W3 时延，未计并发限流与排队，非可靠上界。",
        jobs.min(cases.len()),
        wall / 60.0,
        wall / 3600.0
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

fn normalized(request: &Value, mapping: &BTreeMap<String, String>) -> Result<Value, String> {
    if !request.is_object() {
        return Err("proxy request must be an object".into());
    }
    let mut value = request.clone();
    let mut input: Value = serde_json::from_str(
        value["input"]
            .as_str()
            .ok_or("proxy input must be JSON text")?,
    )
    .map_err(|e| format!("proxy input: {e}"))?;
    ids(&mut input, mapping);
    value["input"] = input;
    Ok(value)
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
    observations: Mutex<HashMap<String, Vec<Value>>>,
    run_id: String,
    send_log: Mutex<Value>,
    sent: AtomicUsize,
}

impl Recorder {
    fn new(dir: PathBuf, limit: Option<u64>, live: Option<(String, String)>) -> Self {
        let mut cfg = config("configuration", "", 1);
        cfg.model_budget = None;
        let run_id = diagnostics::new_id();
        Self {
            dir,
            configuration: serde_json::to_value(cfg).unwrap(),
            allowance: Mutex::new(limit),
            live,
            operations: Mutex::default(),
            documents: Mutex::default(),
            observations: Mutex::default(),
            send_log: Mutex::new(json!({"version":"eval.e2e.sends@1","run_id":run_id,
                "limit":limit,"attempts":[],"proxy_errors":[]})),
            run_id,
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
    fn calls(&self, case: &str) -> Vec<Value> {
        self.observations
            .lock()
            .unwrap()
            .get(case)
            .cloned()
            .unwrap_or_default()
    }
    fn send_log_path(&self) -> PathBuf {
        self.dir.join("runs").join(format!("{}.json", self.run_id))
    }
    // Persist BEFORE entering the physical send. An interrupted `started` entry is a
    // possibly billed attempt, not proof the remote endpoint received it.
    fn start_send(&self, mut attempt: Value) -> Result<usize, String> {
        let mut log = self.send_log.lock().map_err(|_| "send log lock poisoned")?;
        let mut next = log.clone();
        let attempts = next["attempts"].as_array_mut().ok_or("invalid send log")?;
        let id = attempts.len();
        attempt["id"] = json!(id);
        attempt["state"] = json!("started");
        attempts.push(attempt);
        try_write_json(&self.send_log_path(), &next)?;
        *log = next;
        self.sent.fetch_add(1, Ordering::Relaxed);
        Ok(id)
    }
    fn finish_send(&self, id: usize, recording: &Value) -> Result<(), String> {
        let mut log = self.send_log.lock().map_err(|_| "send log lock poisoned")?;
        let mut next = log.clone();
        let attempt = next["attempts"].get_mut(id).ok_or("send entry missing")?;
        attempt["state"] = json!("recorded");
        for key in [
            "http_status",
            "status",
            "usage",
            "elapsed_ms",
            "incomplete_details",
            "transport_error",
        ] {
            attempt[key] = recording[key].clone();
        }
        try_write_json(&self.send_log_path(), &next)?;
        *log = next;
        Ok(())
    }
    fn proxy_error(&self, case: Option<&str>, error: String) -> (u16, String) {
        eprintln!("EVAL_PROXY_ERROR: {error}");
        if let Ok(mut log) = self.send_log.lock() {
            if let Some(errors) = log["proxy_errors"].as_array_mut() {
                errors.push(json!({"case":case,"error":error}));
            }
            if let Err(e) = try_write_json(&self.send_log_path(), &log) {
                eprintln!("EVAL_PROXY_DIAGNOSTIC_WRITE_FAILED: {e}");
            }
        }
        (
            503,
            json!({"error":"eval_proxy_failed","detail":error}).to_string(),
        )
    }
    fn observe(
        &self,
        case: &str,
        operation: &str,
        request: &Value,
        recording: &Value,
        origin: &str,
        elapsed: Duration,
        reason: Option<&str>,
    ) {
        let schema = request
            .pointer("/text/format/name")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let stage = match schema {
            "w1_v1" => "W1",
            "w3_v1" => "W3",
            "w4_v2" => "W4",
            "r1_v1" => "R1",
            "r4_v3" => "R4",
            other => other,
        };
        let body: Value = recording["body"]
            .as_str()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        if let Ok(mut observations) = self.observations.lock() {
            observations.entry(case.into()).or_default().push(json!({
            "operation":operation,"stage":stage,"origin":origin,"sends":usize::from(origin=="live"),
            "elapsed_ms":elapsed.as_millis(),"recorded_elapsed_ms":recording["elapsed_ms"],
            "http_status":recording["http_status"],"response_status":body["status"],"usage":body["usage"],
            "incomplete_details":body["incomplete_details"],"transport_error":recording["transport_error"],
            "error":body["error"],"reason":reason}));
        }
    }
    /// Inject the *send* only in harness unit tests; production requests are never constructed here.
    fn respond(
        &self,
        case: &str,
        request: &Value,
        send: impl FnOnce() -> Result<(u16, String), String>,
    ) -> (u16, String) {
        match self.try_respond(case, request, send) {
            Ok(response) => response,
            Err(error) => {
                if let Ok(mut ops) = self.operations.lock()
                    && let Some(op) = ops.get_mut(case)
                {
                    op.missing = Some(format!("代理错误：{error}"));
                }
                self.proxy_error(Some(case), error)
            }
        }
    }
    fn try_respond(
        &self,
        case: &str,
        request: &Value,
        send: impl FnOnce() -> Result<(u16, String), String>,
    ) -> Result<(u16, String), String> {
        let handling = Instant::now();
        let (operation, position, mapping) = {
            let mut ops = self
                .operations
                .lock()
                .map_err(|_| "operation lock poisoned")?;
            let op = ops
                .get_mut(case)
                .ok_or("proxy case has no active operation")?;
            let result = (op.id.clone(), op.position, op.ids.clone());
            op.position += 1;
            result
        };
        let canonical = normalized(request, &mapping)?;
        let fp = recall_models::fingerprint(&canonical);
        let reverse = mapping
            .iter()
            .map(|(a, b)| (b.clone(), a.clone()))
            .collect();
        let saved = self
            .documents
            .lock()
            .map_err(|_| "recording lock poisoned")?
            .get(case)
            .ok_or("proxy case has no recording document")?["operations"][&operation][position]
            .clone();
        if saved["request"] == fp {
            let status = saved["http_status"]
                .as_u64()
                .and_then(|s| u16::try_from(s).ok())
                .ok_or("invalid recorded HTTP status")?;
            let body = saved["body"].as_str().ok_or("invalid recorded body")?;
            self.observe(
                case,
                &operation,
                request,
                &saved,
                "replay",
                handling.elapsed(),
                None,
            );
            return Ok((status, response_ids(body, &reverse)));
        }
        // One global reservation, made under the lock BEFORE invoking the send closure.
        // Replays never reserve. Even concurrent failures consume their reserved attempt.
        let (send_id, exhausted) = {
            let mut limit = self
                .allowance
                .lock()
                .map_err(|_| "allowance lock poisoned")?;
            match limit.as_mut() {
                Some(n) if *n > 0 => {
                    let id = self.start_send(json!({"case":case,"operation":operation,"position":position,
                        "stage":request.pointer("/text/format/name"),"request":fp,"endpoint":request["model"]}))?;
                    *n -= 1;
                    (Some(id), false)
                }
                Some(_) => (None, true),
                None => (None, false),
            }
        };
        let Some(send_id) = send_id else {
            let reason = if exhausted {
                "额度耗尽（EVAL_RECORD）"
            } else if saved.is_null() {
                "未录制"
            } else {
                "录制过期（请求指纹不符）"
            };
            self.operations
                .lock()
                .map_err(|_| "operation lock poisoned")?
                .get_mut(case)
                .ok_or("proxy case has no active operation")?
                .missing = Some(reason.into());
            self.observe(
                case,
                &operation,
                request,
                &Value::Null,
                "not_recorded",
                handling.elapsed(),
                Some(reason),
            );
            return Ok((503, json!({"error":reason}).to_string()));
        };
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
            "send_run":self.run_id,"send_index":send_id,
            "request":fp,"request_content":canonical,"http_status":status,"body":response_ids(&body,&mapping),
            "usage":envelope["usage"],"elapsed_ms":started.elapsed().as_millis(),
            "status":envelope["status"],"incomplete_details":envelope["incomplete_details"],"transport_error":failure});
        self.observe(
            case,
            &operation,
            request,
            &recording,
            "live",
            handling.elapsed(),
            None,
        );
        let mut docs = self
            .documents
            .lock()
            .map_err(|_| "recording lock poisoned")?;
        let mut doc = docs.get(case).ok_or("recording document missing")?.clone();
        let ops = doc["operations"]
            .as_object_mut()
            .ok_or("invalid recording operations")?;
        let list = ops
            .entry(operation)
            .or_insert(json!([]))
            .as_array_mut()
            .ok_or("invalid recorded operation")?;
        // A changed prefix invalidates subsequent stages; never reuse their old output by index.
        list.truncate(position);
        list.push(recording.clone());
        let target = self.dir.join(format!("{case}.json"));
        // A response is durable before returning it to production, not at case completion.
        try_write_json(&target, &doc)?;
        docs.insert(case.into(), doc);
        drop(docs);
        if let Err(e) = self.finish_send(send_id, &recording) {
            // The per-call recording is already durable; reports reconcile this window.
            self.proxy_error(Some(case), e);
        }
        Ok((status, body))
    }
}

fn read_proxy_request(reader: &mut impl BufRead) -> Result<(String, Value), String> {
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|e| format!("proxy request line: {e}"))?;
    let mut words = line.split_ascii_whitespace();
    if words.next() != Some("POST") {
        return Err("proxy expects POST".into());
    }
    let case = words
        .next()
        .and_then(|p| p.strip_prefix("/v1/case/"))
        .and_then(|p| p.strip_suffix("/responses"))
        .filter(|s| {
            !s.is_empty()
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        })
        .ok_or("invalid proxy case path")?
        .to_owned();
    let mut length = None;
    loop {
        line.clear();
        if reader
            .read_line(&mut line)
            .map_err(|e| format!("proxy headers: {e}"))?
            == 0
        {
            return Err("proxy headers ended early".into());
        }
        if line == "\r\n" {
            break;
        }
        if let Some((k, v)) = line.split_once(':')
            && k.eq_ignore_ascii_case("content-length")
        {
            length = Some(
                v.trim()
                    .parse::<usize>()
                    .map_err(|e| format!("proxy content length: {e}"))?,
            );
        }
    }
    let length = length
        .filter(|&n| n <= 16 * 1024 * 1024)
        .ok_or("missing or oversized proxy content length")?;
    let mut body = vec![0; length];
    reader
        .read_exact(&mut body)
        .map_err(|e| format!("proxy body: {e}"))?;
    let request = serde_json::from_slice(&body).map_err(|e| format!("proxy request JSON: {e}"))?;
    Ok((case, request))
}

fn proxy_exchange(
    stream: &mut (impl Read + Write),
    recorder: &Recorder,
    send: impl FnOnce(&Value) -> Result<(u16, String), String>,
) {
    let response = match read_proxy_request(&mut BufReader::new(&mut *stream)) {
        Ok((case, request)) => recorder.respond(&case, &request, || send(&request)),
        Err(e) => recorder.proxy_error(None, e),
    };
    if let Err(e) = proxy_reply(stream, response) {
        recorder.proxy_error(None, format!("proxy response write: {e}"));
    }
}

fn proxy_reply(stream: &mut impl Write, (status, body): (u16, String)) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status} Eval\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}

fn configure_connection(stream: &TcpStream) -> std::io::Result<()> {
    // macOS accepted sockets inherit the listener's O_NONBLOCK flag.
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    stream.set_write_timeout(Some(Duration::from_secs(30)))
}

/// Loopback transport, one worker per physical send. Recording never calls legacy forward()
/// (which retries). Join workers on shutdown so incomplete/late responses retain diagnostics.
fn server(
    recorder: Arc<Recorder>,
    fixture: Option<fn(&Value) -> Value>,
) -> Result<(String, Arc<AtomicBool>, std::thread::JoinHandle<()>), String> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| format!("proxy bind: {e}"))?;
    let address = listener
        .local_addr()
        .map_err(|e| e.to_string())?
        .to_string();
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let http = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(600))
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .build()
        .map_err(|e| format!("proxy HTTP client: {e}"))?;
    let stop = Arc::new(AtomicBool::new(false));
    let stopping = stop.clone();
    let handle = std::thread::spawn(move || {
        let mut workers = Vec::new();
        while !stopping.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let recorder = recorder.clone();
                    let http = http.clone();
                    workers.push(std::thread::spawn(move || {
                        if let Err(e) = configure_connection(&stream) {
                            let response = recorder.proxy_error(None, format!("proxy connection setup: {e}"));
                            if let Err(e) = proxy_reply(&mut stream, response) {
                                recorder.proxy_error(None, format!("proxy response write: {e}"));
                            }
                            return;
                        }
                        proxy_exchange(&mut stream, &recorder, |request| {
                            if let Some(fixture) = fixture {
                                return Ok((200,json!({"status":"completed","usage":{"input_tokens":10,"output_tokens":20},
                                    "output":[{"type":"message","content":[{"type":"output_text","text":fixture(&request).to_string()}]}]}).to_string()));
                            }
                            let (url,key) = recorder.live.as_ref().ok_or("live transport unavailable")?;
                            let response = http.post(url).bearer_auth(key).json(&request).send().map_err(|e| e.to_string())?;
                            let status = response.status().as_u16();
                            Ok((status,response.text().map_err(|e| e.to_string())?))
                        });
                    }));
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(e) => {
                    recorder.proxy_error(None, format!("eval listener: {e}"));
                    break;
                }
            }
        }
        for worker in workers {
            if worker.join().is_err() {
                recorder.proxy_error(None, "proxy worker panicked".into());
            }
        }
    });
    Ok((address, stop, handle))
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

fn try_write_json(path: &Path, value: &Value) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    let temporary = path.with_extension("json.tmp");
    let mut file = std::fs::File::create(&temporary).map_err(|e| e.to_string())?;
    serde_json::to_writer_pretty(&mut file, value).map_err(|e| e.to_string())?;
    file.write_all(b"\n").map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    std::fs::rename(&temporary, path).map_err(|e| format!("record {}: {e}", path.display()))?;
    std::fs::File::open(parent)
        .and_then(|dir| dir.sync_all())
        .map_err(|e| e.to_string())
}

fn write_json(path: &Path, value: &Value) {
    try_write_json(path, value).unwrap();
}

fn parallel_cases(
    cases: &[Value],
    jobs: usize,
    run: impl Fn(&Value) -> Value + Sync,
) -> Vec<Value> {
    let next = AtomicUsize::new(0);
    let results: Vec<Mutex<Option<Value>>> = cases.iter().map(|_| Mutex::new(None)).collect();
    std::thread::scope(|scope| {
        for _ in 0..jobs.min(cases.len()) {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(case) = cases.get(index) else { break };
                    *results[index].lock().unwrap() = Some(run(case));
                }
            });
        }
    });
    results
        .into_iter()
        .map(|result| result.into_inner().unwrap().unwrap())
        .collect()
}

pub(super) fn run() {
    let cases = cases();
    let jobs = jobs(std::env::var("EVAL_JOBS").ok().as_deref());
    let ceiling = plan(&cases, jobs);
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
    println!(
        "逐次发送日志 {}（发送前落盘，started 项可能在途或已计费）",
        recorder.send_log_path().display()
    );
    let output = std::env::var_os("EVAL_V3_OUTPUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| dir.join("run.json"));
    let case_dir = output.with_extension("cases");
    let started = Instant::now();
    // Leave a report entry point even if this process is killed before any case finishes.
    write_json(
        &output,
        &json!({"pipeline":"v3","label":LABEL,"run_id":recorder.run_id,
        "configuration":recorder.configuration,"cases":[],"real_calls":0,"jobs":jobs,
        "send_log":recorder.send_log_path()}),
    );
    let empty = limit.is_none()
        && cases.iter().all(|c| {
            !dir.join(format!("{}.json", c["id"].as_str().unwrap()))
                .exists()
        });
    let mut results = if empty {
        cases.iter().map(|c| json!({"case":c["id"],"steps":c["questions"].as_array().unwrap().iter().map(|q|
            json!({"step":"question","question":q["id"],"state":"not_recorded","reason":"未录制"})).collect::<Vec<_>>()})).collect::<Vec<_>>()
    } else {
        let (address, stop, handle) = server(recorder.clone(), None).unwrap();
        // Only this suite mutates provider environment, before constructing production clients.
        unsafe {
            std::env::set_var("PMR_MODEL_NETWORK", "offline");
            std::env::set_var("DEEPSEEK_API_KEY", "eval-replay");
        }
        let results = parallel_cases(&cases, jobs, |case| {
            let started = Instant::now();
            let mut result = run_case(case, &recorder, &address);
            result["elapsed_ms"] = json!(started.elapsed().as_millis());
            result["calls"] = json!(recorder.calls(case["id"].as_str().unwrap()));
            write_json(
                &case_dir.join(format!("{}.json", case["id"].as_str().unwrap())),
                &result,
            );
            result
        });
        stop.store(true, Ordering::SeqCst);
        handle.join().unwrap();
        results
    };
    // The server joins late responses before finalizing per-case diagnostics. This changes
    // neither the result returned at the production deadline nor its scoring state.
    for result in &mut results {
        let case = result["case"].as_str().unwrap().to_owned();
        result["calls"] = json!(recorder.calls(&case));
        write_json(&case_dir.join(format!("{case}.json")), result);
    }
    write_json(
        &output,
        &json!({"pipeline":"v3","label":LABEL,"run_id":recorder.run_id,
        "configuration":recorder.configuration,"cases":results,"real_calls":recorder.sent.load(Ordering::Relaxed),
        "send_log":recorder.send_log_path(),
        "jobs":jobs,"elapsed_ms":started.elapsed().as_millis()}),
    );
    println!(
        "真实调用 {}；运行输出 {}；空录制记为未录制，不计通过。",
        recorder.sent.load(Ordering::Relaxed),
        output.display()
    );
}

#[test]
fn v3_proxy_invalid_input_is_reported_without_panicking() {
    let dir = std::env::temp_dir().join(format!("eval-v3-invalid-{}", diagnostics::new_id()));
    let recorder = Recorder::new(dir.clone(), Some(1), None);
    recorder.begin("case", "write:m1", &BTreeMap::new());
    let request = json!({"model":PRO,"input":"invalid-json"});
    let response = std::panic::catch_unwind(|| {
        recorder.respond("case", &request, || panic!("invalid input must not send"))
    });
    assert!(
        response.is_ok(),
        "proxy must return a diagnostic, not panic"
    );
    assert_eq!(response.unwrap().0, 503);
    assert_eq!(recorder.sent.load(Ordering::Relaxed), 0);
    if dir.exists() {
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[test]
fn v3_proxy_read_and_write_errors_are_durable_without_panicking() {
    struct Io {
        input: std::io::Cursor<Vec<u8>>,
        output: Vec<u8>,
        read_error: bool,
        write_error: bool,
    }
    impl Read for Io {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            if self.read_error {
                return Err(std::io::ErrorKind::WouldBlock.into());
            }
            self.input.read(bytes)
        }
    }
    impl Write for Io {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.write_error {
                return Err(std::io::ErrorKind::BrokenPipe.into());
            }
            self.output.write(bytes)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let dir = std::env::temp_dir().join(format!("eval-v3-io-{}", diagnostics::new_id()));
    let recorder = Recorder::new(dir.clone(), Some(3), None);
    for (input, read_error, write_error) in [
        ("", true, false),
        (
            "POST /v1/case/case/responses HTTP/1.1\r\nContent-Length: nope\r\n\r\n",
            false,
            false,
        ),
        (
            "POST /v1/case/case/responses HTTP/1.1\r\nContent-Length: 3\r\n\r\n{",
            false,
            false,
        ),
        ("invalid\r\n", false, true),
    ] {
        let mut io = Io {
            input: std::io::Cursor::new(input.as_bytes().to_vec()),
            output: vec![],
            read_error,
            write_error,
        };
        proxy_exchange(&mut io, &recorder, |_| {
            panic!("invalid request must not send")
        });
        if !write_error {
            assert!(
                String::from_utf8(io.output)
                    .unwrap()
                    .starts_with("HTTP/1.1 503")
            );
        }
    }
    recorder.begin("case", "write:m1", &BTreeMap::new());
    let body = json!({"model":PRO,"input":"{}","text":{"format":{"name":"w1_v1"}}}).to_string();
    let wire = format!(
        "POST /v1/case/case/responses HTTP/1.1\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let mut io = Io {
        input: std::io::Cursor::new(wire.into_bytes()),
        output: vec![],
        read_error: false,
        write_error: true,
    };
    proxy_exchange(&mut io, &recorder, |_| {
        Ok((200, json!({"status":"completed"}).to_string()))
    });
    let saved: Value =
        serde_json::from_slice(&std::fs::read(dir.join("case.json")).unwrap()).unwrap();
    assert_eq!(saved["operations"]["write:m1"][0]["status"], "completed");
    let log: Value =
        serde_json::from_slice(&std::fs::read(recorder.send_log_path()).unwrap()).unwrap();
    assert_eq!(log["proxy_errors"].as_array().unwrap().len(), 6);
    assert_eq!(log["attempts"][0]["state"], "recorded");
    assert_eq!(recorder.sent.load(Ordering::Relaxed), 1);
    let replay = Recorder::new(dir.clone(), None, None);
    replay.begin("case", "write:m1", &BTreeMap::new());
    let request: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        replay
            .respond("case", &request, || panic!(
                "broken client must not lose recording"
            ))
            .0,
        200
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v3_interrupted_run_preserves_calls_and_replays_without_duplicate_sends() {
    let dir = std::env::temp_dir().join(format!("eval-v3-interrupt-{}", diagnostics::new_id()));
    let recorder = Recorder::new(dir.clone(), Some(2), None);
    let log_path = recorder.send_log_path();
    let request = json!({"model":PRO,"input":"{}","text":{"format":{"name":"w1_v1"}}});
    recorder.begin("case", "write:m1", &BTreeMap::new());
    let first = recorder.respond("case", &request, || {
        let log: Value = serde_json::from_slice(&std::fs::read(&log_path).unwrap()).unwrap();
        assert_eq!(log["attempts"][0]["state"], "started");
        Ok((200, json!({"status":"completed"}).to_string()))
    });
    // No case-result file has been written: the first physical response is already durable.
    assert!(dir.join("case.json").exists());
    assert!(!dir.join("run.cases").exists());
    let interrupted = std::panic::catch_unwind(|| {
        recorder.respond("case", &request, || {
            panic!("simulate process interruption at send")
        })
    });
    assert!(interrupted.is_err());
    let log: Value = serde_json::from_slice(&std::fs::read(&log_path).unwrap()).unwrap();
    assert_eq!(log["attempts"].as_array().unwrap().len(), 2);
    assert_eq!(log["attempts"][0]["state"], "recorded");
    assert_eq!(log["attempts"][1]["state"], "started");
    drop(recorder);
    let replay = Recorder::new(dir.clone(), Some(1), None);
    replay.begin("case", "write:m1", &BTreeMap::new());
    assert_eq!(
        replay.respond("case", &request, || panic!("durable call must not resend")),
        first
    );
    assert_eq!(replay.sent.load(Ordering::Relaxed), 0);
    assert_eq!(*replay.allowance.lock().unwrap(), Some(1));
    assert_eq!(
        replay
            .respond("case", &request, || Ok((200, "{}".into())))
            .0,
        200
    );
    assert_eq!(replay.sent.load(Ordering::Relaxed), 1);
    assert_eq!(*replay.allowance.lock().unwrap(), Some(0));
    // A fully saved prefix, reopened again, requires no physical send at all.
    let final_replay = Recorder::new(dir.clone(), None, None);
    final_replay.begin("case", "write:m1", &BTreeMap::new());
    for _ in 0..2 {
        assert_eq!(
            final_replay
                .respond("case", &request, || panic!("no duplicate send"))
                .0,
            200
        );
    }
    assert_eq!(final_replay.sent.load(Ordering::Relaxed), 0);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v3_failed_send_log_prevents_send_and_recording_error_keeps_started_count() {
    let dir = std::env::temp_dir().join(format!("eval-v3-disk-{}", diagnostics::new_id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("runs"), "not a directory").unwrap();
    let recorder = Recorder::new(dir.clone(), Some(1), None);
    recorder.begin("case", "write:m1", &BTreeMap::new());
    let request = json!({"model":PRO,"input":"{}"});
    assert_eq!(
        recorder
            .respond("case", &request, || panic!("no durable counter, no send"))
            .0,
        503
    );
    assert_eq!(recorder.sent.load(Ordering::Relaxed), 0);
    assert_eq!(*recorder.allowance.lock().unwrap(), Some(1));
    std::fs::remove_file(dir.join("runs")).unwrap();
    std::fs::create_dir(dir.join("case.json.tmp")).unwrap();
    recorder.begin("case", "write:m1", &BTreeMap::new());
    assert_eq!(
        recorder
            .respond("case", &request, || Ok((200, "{}".into())))
            .0,
        503
    );
    assert_eq!(recorder.sent.load(Ordering::Relaxed), 1);
    let log: Value =
        serde_json::from_slice(&std::fs::read(recorder.send_log_path()).unwrap()).unwrap();
    assert_eq!(log["attempts"][0]["state"], "started");
    assert_eq!(log["proxy_errors"].as_array().unwrap().len(), 2);
    assert!(recorder.documents.lock().unwrap()["case"]["operations"]["write:m1"].is_null());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v3_nonblocking_listener_reads_delayed_request_without_panicking() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let dir = std::env::temp_dir().join(format!("eval-v3-socket-{}", diagnostics::new_id()));
    let recorder = Recorder::new(dir.clone(), Some(1), None);
    recorder.begin("case", "write:m1", &BTreeMap::new());
    let (tx, rx) = std::sync::mpsc::channel();
    let client = std::thread::spawn(move || {
        let mut stream = TcpStream::connect(address).unwrap();
        tx.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        let body = json!({"input":"{}","model":PRO}).to_string();
        write!(
            stream,
            "POST /v1/case/case/responses HTTP/1.1\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    });
    rx.recv().unwrap();
    let (mut stream, _) = listener.accept().unwrap();
    // Force the inherited macOS flag on every platform, then exercise the actual fix.
    stream.set_nonblocking(true).unwrap();
    configure_connection(&stream).unwrap();
    proxy_exchange(&mut stream, &recorder, |_| Ok((200, "{}".into())));
    drop(stream);
    client.join().unwrap();
    assert_eq!(recorder.sent.load(Ordering::Relaxed), 1);
    std::fs::remove_dir_all(dir).unwrap();
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
        let serial: f64 = cases.iter().map(estimated_seconds).sum();
        assert!((estimated_wall_seconds(&cases, 1) - serial).abs() < 0.001);
        assert!((estimated_wall_seconds(&cases, 8) - 3264.0306217513).abs() < 0.001);
    }
    let cfg = config("test", "local", 9);
    assert_eq!(cfg.write_models.as_ref().unwrap().w1, PRO);
    assert_eq!(cfg.write_models.as_ref().unwrap().w3, FLASH);
    assert_eq!(cfg.recall_models.as_ref().unwrap().r4, PRO);
    assert_eq!(cfg.background_operation_limit, Some(3));
    assert_eq!(jobs(None), 8);
    assert_eq!(jobs(Some("1")), 1);
    assert!(std::panic::catch_unwind(|| jobs(Some("0"))).is_err());
}

#[test]
fn v3_parallel_cases_keep_recordings_and_reports_isolated_and_ordered() {
    let dir = std::env::temp_dir().join(format!("eval-v3-parallel-{}", diagnostics::new_id()));
    let recorder = Recorder::new(dir.clone(), Some(24 * 3), None);
    let cases: Vec<_> = (0..24).map(|i| json!({"id":format!("case-{i}")})).collect();
    let barrier = std::sync::Barrier::new(8);
    let results = parallel_cases(&cases, 8, |case| {
        let id = case["id"].as_str().unwrap();
        barrier.wait();
        let mut steps = Vec::new();
        for (index, stage) in ["w1_v1", "w3_v1", "r1_v1"].iter().enumerate() {
            recorder.begin(id, &format!("step:{index}"), &BTreeMap::new());
            let request = json!({"model":PRO,"input":json!({"case":id,"index":index}).to_string(),"text":{"format":{"name":stage}}});
            assert_eq!(
                recorder
                    .respond(id, &request, || Ok((
                        200,
                        json!({"status":"completed","case":id,"index":index}).to_string()
                    )))
                    .0,
                200
            );
            steps.push(index);
        }
        let result = json!({"case":id,"steps":steps,"calls":recorder.calls(id)});
        write_json(&dir.join("run.cases").join(format!("{id}.json")), &result);
        result
    });
    assert_eq!(recorder.sent.load(Ordering::Relaxed), 72);
    for (case, result) in cases.iter().zip(results.iter()) {
        let id = case["id"].as_str().unwrap();
        assert_eq!(result["case"], case["id"]);
        assert_eq!(result["steps"], json!([0, 1, 2]));
        let saved: Value =
            serde_json::from_slice(&std::fs::read(dir.join(format!("{id}.json"))).unwrap())
                .unwrap();
        for index in 0..3 {
            let row = &saved["operations"][format!("step:{index}")][0];
            assert_eq!(row["request_content"]["input"]["case"], id);
            let body: Value = serde_json::from_str(row["body"].as_str().unwrap()).unwrap();
            assert_eq!(body["case"], id);
            assert_eq!(body["index"], index);
        }
        let report: Value = serde_json::from_slice(
            &std::fs::read(dir.join("run.cases").join(format!("{id}.json"))).unwrap(),
        )
        .unwrap();
        assert_eq!(&report, result);
        assert_eq!(report["calls"].as_array().unwrap().len(), 3);
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v3_concurrent_reservations_never_exceed_global_allowance() {
    let dir = std::env::temp_dir().join(format!("eval-v3-limit-{}", diagnostics::new_id()));
    let recorder = Recorder::new(dir.clone(), Some(7), None);
    let cases: Vec<_> = (0..64).map(|i| json!({"id":format!("case-{i}")})).collect();
    let barrier = std::sync::Barrier::new(16);
    let sends = AtomicUsize::new(0);
    let results = parallel_cases(&cases, 16, |case| {
        let id = case["id"].as_str().unwrap();
        recorder.begin(id, "write:m1", &BTreeMap::new());
        barrier.wait();
        let request = json!({"model":PRO,"input":"{}","text":{"format":{"name":"w1_v1"}}});
        let (status, _) = recorder.respond(id, &request, || {
            sends.fetch_add(1, Ordering::SeqCst);
            Ok((200, json!({"status":"completed"}).to_string()))
        });
        json!({"case":id,"status":status,"reason":recorder.missing(id)})
    });
    assert_eq!(sends.load(Ordering::SeqCst), 7);
    assert_eq!(recorder.sent.load(Ordering::Relaxed), 7);
    assert_eq!(*recorder.allowance.lock().unwrap(), Some(0));
    assert_eq!(results.iter().filter(|r| r["status"] == 200).count(), 7);
    assert_eq!(
        results
            .iter()
            .filter(|r| r["reason"] == "额度耗尽（EVAL_RECORD）")
            .count(),
        57
    );
    assert_eq!(
        std::fs::read_dir(&dir)
            .unwrap()
            .filter(|p| p
                .as_ref()
                .unwrap()
                .path()
                .extension()
                .is_some_and(|s| s == "json"))
            .count(),
        7
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v3_source_ids_preserve_text_and_strict_request_binding() {
    let request = json!({"model":PRO,"input":json!({"source_snapshot":["random", "session", "m1"],
        "current_user_message":"random","sources":{"random":{"text":"random"}},
        "source":{"kind":"stored","source_id":"random"},"quote":{"text":"random"}}).to_string(),
        "instructions":"production","reasoning":{"effort":"medium"},"max_output_tokens":32768});
    let map = BTreeMap::from([("random".into(), "m1".into())]);
    let canonical = normalized(&request, &map).unwrap();
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
        normalized(&next, &BTreeMap::from([("next".into(), "m1".into())])).unwrap()
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
    let (address, stop, handle) = server(recorder.clone(), Some(fixture)).unwrap();
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
    let (address, stop, handle) = server(replay.clone(), None).unwrap();
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
