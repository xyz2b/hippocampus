//! Opt-in end-to-end harness. Production owns all five model stages and their prompts.
use super::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

#[path = "v3_core.rs"]
mod core;
use core::run_case;
use core::*;

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

fn r4_low_reference() -> (f64, f64, usize, usize) {
    let rows: Vec<Value> = std::fs::read_dir(
        Path::new(ROOT)
            .join("fixed_inputs/recordings/ac2-fixed-pro-minimal/reasoning-low/repeat-1"),
    )
    .unwrap()
    .map(|p| {
        let file: Value =
            serde_json::from_slice(&std::fs::read(p.unwrap().path()).unwrap()).unwrap();
        file["recorded"].as_array().unwrap().last().unwrap().clone()
    })
    .collect();
    let mut elapsed: Vec<_> = rows
        .iter()
        .map(|r| r["elapsed_ms"].as_u64().unwrap())
        .collect();
    elapsed.sort_unstable();
    let n = elapsed.len();
    let seconds = (elapsed[(n - 1) / 2] + elapsed[n / 2]) as f64 / 2000.0;
    let outputs: Vec<_> = rows
        .iter()
        .filter_map(|r| r["usage"]["output_tokens"].as_u64())
        .collect();
    (
        outputs.iter().sum::<u64>() as f64 / outputs.len() as f64,
        seconds,
        n,
        outputs.len(),
    )
}

fn estimated_seconds(case: &Value) -> f64 {
    let n = counts(case);
    (n[0] + n[2]) as f64 * (3782.19 / 82.0)
        + n[1] as f64 * (88.148 / 23.0)
        + n[3] as f64 * 2.5
        + r4_counts(case)[0] as f64 * r4_low_reference().1
        + r4_counts(case)[1] as f64 * 4.5
}

fn r4_counts(case: &Value) -> [u64; 2] {
    let models = config("plan", "offline.invalid", 2).recall_models.unwrap();
    let mut counts = [0; 2];
    for question in case["questions"].as_array().unwrap() {
        let op = serde_json::from_value(question["query_meta"]["operation"].clone())
            .expect("dry-run needs a valid annotated R1 op");
        let compute = models.r4_effort(Some(op)) == hippocampus::ReasoningEffort::Low;
        counts[if compute { 0 } else { 1 }] += 1;
    }
    counts
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
        "模型均 Pro；推理强度 W1/W4=medium，W3/R1=minimal，R4=计算类 low / 其余 minimal；默认路径不变。"
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
    let r4 = cases
        .iter()
        .map(r4_counts)
        .fold([0; 2], |a, n| [a[0] + n[0], a[1] + n[1]]);
    println!(
        "R4 分流预计：计算类 {} 次 low，非计算类 {} 次 minimal；依据 query_meta.operation 标注。实际只由已核验 R1 plan.op 决定，R1 失败走 minimal，最多仍 {} 次 R4。",
        r4[0], r4[1], total[4]
    );
    let (low_output, low_seconds, low_calls, known_usage) = r4_low_reference();
    println!(
        "R4 输出估算：low {:.0} + minimal {:.0} = {:.0} token；low 按 AE14 {low_calls}次中 {known_usage}次已知 usage 均值{low_output:.2}，未知 usage 不按零；minimal 用首轮均值278.10。不是 token 上界。",
        r4[0] as f64 * low_output,
        r4[1] as f64 * (16964.0 / 61.0),
        r4[0] as f64 * low_output + r4[1] as f64 * (16964.0 / 61.0)
    );
    println!("完整分阶段 token 估算：python3 tests/eval/fixed_inputs/prepare.py（零调用）。");
    // Historical recordings lack separate W1/W3/W4 elapsed times. These pooled averages
    // are explicitly proxies, not stage medians or a wall-clock upper bound.
    let seconds: f64 = cases.iter().map(estimated_seconds).sum();
    let wall = estimated_wall_seconds(cases, jobs);
    println!(
        "耗时参考：W1/W4≈46.1s（Pro 混合82次/3782.19s），W3≈3.8s（AE9 Pro minimal 固定输入23次/88.148s）；R1≈2.5s，R4 low≈{low_seconds:.1}s（AE14 {low_calls}次中位数，含超时）/ minimal≈4.5s。计算类 low 本地超时240s，无重试。W1/W4混合均值与W3失败固定输入均为代理，以下仅是容量情景。"
    );
    println!(
        "上述情景串行约 {seconds:.0}s / {:.1}分钟 / {:.2}小时。",
        seconds / 60.0,
        seconds / 3600.0
    );
    println!(
        "EVAL_JOBS={jobs}（有效 {}）：FIFO 用例队列预计墙钟 {wall:.0}s / {:.1}分钟 / {:.2}小时；用例内严格串行。未计并发限流与排队、输入/输出差异，非可靠上界。",
        jobs.min(cases.len()),
        wall / 60.0,
        wall / 3600.0
    );
    ceiling
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
        .unwrap_or_else(|| {
            dir.join(if limit.is_none() {
                "run-replay.json"
            } else {
                "run.json"
            })
        });
    if limit.is_none() && output.exists() {
        let previous: Value = serde_json::from_slice(&std::fs::read(&output).unwrap()).unwrap();
        assert!(
            previous["real_calls"].as_u64().unwrap_or(0) == 0,
            "回放不得覆盖 live 输出"
        );
    }
    let case_dir = output.with_extension("cases");
    let started = Instant::now();
    // Leave a report entry point even if this process is killed before any case finishes.
    write_json(
        &output,
        &json!({"pipeline":"v3","label":LABEL,"execution_mode":if limit.is_none() {"replay"} else {"live_or_mixed"},"run_id":recorder.run_id,
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
        &json!({"pipeline":"v3","label":LABEL,"execution_mode":if limit.is_none() {"replay"} else {"live_or_mixed"},"run_id":recorder.run_id,
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
        assert!((estimated_wall_seconds(&cases, 8) - 2501.403443266172).abs() < 0.001);
        let split = cases
            .iter()
            .map(r4_counts)
            .fold([0; 2], |a, n| [a[0] + n[0], a[1] + n[1]]);
        assert_eq!(split, [7, 56]);
        let (output, seconds, calls, known) = r4_low_reference();
        assert_eq!((calls, known), (8, 7));
        assert!((output - 5164.714285714285).abs() < 0.001);
        assert!((seconds - 84.3965).abs() < 0.001);
    }
    let cfg = config("test", "local", 9);
    assert_eq!(cfg.write_models.as_ref().unwrap().w1, PRO);
    assert_eq!(cfg.write_models.as_ref().unwrap().w3, PRO);
    assert_eq!(
        cfg.write_models.as_ref().unwrap().effort("w3"),
        hippocampus::ReasoningEffort::Minimal
    );
    assert_eq!(cfg.recall_models.as_ref().unwrap().r4, PRO);
    assert_eq!(
        cfg.recall_models
            .as_ref()
            .unwrap()
            .r4_compute_reasoning_effort,
        hippocampus::ReasoningEffort::Low
    );
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
        assert_eq!(request["model"], PRO);
        assert_eq!(
            request["reasoning"]["effort"],
            if name.starts_with('r') || name == "w3_v1" {
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
            "w4_v3" => {
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
    for stage in ["w1_v1", "w3_v1", "w4_v3", "r1_v1", "r4_v3"] {
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
