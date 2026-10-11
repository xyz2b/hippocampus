//! AF255 is opt-in. Historical AC2 recordings and authorization remain untouched.
use super::*;
use hippocampus::model_provider::ProviderConfig;
use std::collections::BTreeSet;

const QUESTIONS: &[&str] = &[
    "device_default_paper/device_q1",
    "device_default_paper/device_q3",
    "device_default_paper/device_q5",
    "event_registration_fee/event_q5",
    "person_sketch_pen/person_q3",
    "organization_borrow_limit/organization_q6",
    "task_isolation/task_q1",
    "task_isolation/task_q2",
    "explicit_correction/correction_q1",
    "explicit_correction/correction_q2",
    "unresolved_conflict/conflict_q2",
    "absolute_date_ranges/time_q1",
    "absolute_date_ranges/time_q3",
    "lme_gpt4_6dc9b45b/q",
    "lme_gpt4_b5700ca9/q",
];
const VERSION: &str = "eval.e2e.af255@1";

fn subset() -> Vec<Value> {
    let mut found = BTreeSet::new();
    let mut selected = Vec::new();
    let mut paths: Vec<_> = std::fs::read_dir(Path::new(ROOT).join("cases"))
        .unwrap()
        .map(|p| p.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    paths.sort();
    for path in paths {
        let mut case: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let id = case["id"].as_str().unwrap().to_owned();
        case["questions"].as_array_mut().unwrap().retain(|q| {
            let key = format!("{id}/{}", q["id"].as_str().unwrap());
            QUESTIONS.contains(&key.as_str()) && {
                found.insert(key);
                true
            }
        });
        if case["questions"].as_array().unwrap().is_empty() {
            continue;
        }
        let end = case["questions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|q| q["after"].as_u64().unwrap())
            .max()
            .unwrap() as usize;
        case["messages"].as_array_mut().unwrap().truncate(end);
        selected.push(case);
    }
    assert_eq!(found, QUESTIONS.iter().map(|s| s.to_string()).collect());
    selected
}

fn profile(b: bool, pro: &str) -> Value {
    json!({"version":VERSION,"arm":if b {"B"} else {"A"},"hybrid":false,"id_namespace":"af255-opaque@1",
        "models":{"W1":pro,"W3":pro,"W4":pro,"R1":pro,"R4":if b {"gpt-6.1-sol"} else {pro}},
        "efforts":{"W1":"low","W3":"low","W4":"medium","R1":"minimal","R4":if b {"low"} else {"minimal"},"R4_compute":"low"},
        "output_tokens":{"W1":32768,"W3":16384,"W4":16384,"R1":4096,"R4":4096,"R4_compute":16384},
        "timeouts_ms":{"W1":360000,"W3":360000,"W4":360000,"R1":180000,"R4":180000,"R4_compute":240000}})
}

fn eval_config(b: bool, pro: &str, case: &str, address: &str, calls: u64) -> ChatConfig {
    let mut cfg = config(case, address, calls);
    let write = cfg.write_models.as_mut().unwrap();
    write.w1 = pro.into();
    write.w3 = pro.into();
    write.w4 = pro.into();
    write
        .reasoning_effort
        .insert("w1".into(), ReasoningEffort::Low);
    write
        .reasoning_effort
        .insert("w3".into(), ReasoningEffort::Low);
    let recall = cfg.recall_models.as_mut().unwrap();
    recall.r1 = pro.into();
    recall.r4 = if b { "gpt-6.1-sol" } else { pro }.into();
    if b {
        recall
            .reasoning_effort
            .insert("r4".into(), ReasoningEffort::Low);
    }
    // Explicit providers require live mode; both arms use the offline loopback
    // Responses adapter and the recording proxy routes each stage upstream.
    assert!(recall.providers.is_empty());
    assert!(!cfg.embedding.enabled);
    cfg
}

fn read(path: &Path) -> Result<Value, String> {
    serde_json::from_slice(&std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?)
        .map_err(|e| e.to_string())
}
fn slot(case: &str, operation: &str, stage: &str) -> String {
    format!("{case}/{operation}/{stage}")
}
fn input_identity(cases: &[Value]) -> Value {
    json!(cases.iter().map(|c|json!({"id":c["id"],"entities":c["entities"],
        "messages":c["messages"].as_array().unwrap().iter().map(|m|json!({"id":m["id"],"text":m["text"],"task_id":m["task_id"],"observed_at":m["observed_at"]})).collect::<Vec<_>>(),
        "questions":c["questions"].as_array().unwrap().iter().map(|q|json!({"id":q["id"],"after":q["after"],"query":q["query"],"context":q["context"],"at":q["at"],"task_id":q["task_id"]})).collect::<Vec<_>>()
    })).collect::<Vec<_>>())
}

fn slots(cases: &[Value]) -> Vec<String> {
    let mut slots = Vec::new();
    for c in cases {
        let id = c["id"].as_str().unwrap();
        for (i, m) in c["messages"].as_array().unwrap().iter().enumerate() {
            let op = format!("write:{}", m["id"].as_str().unwrap());
            for s in ["W1", "W3", "W4"] {
                if s != "W4" || i > 0 {
                    slots.push(slot(id, &op, s));
                }
            }
        }
        for q in c["questions"].as_array().unwrap() {
            for s in ["R1", "R4"] {
                slots.push(slot(
                    id,
                    &format!("recall:{}", q["id"].as_str().unwrap()),
                    s,
                ));
            }
        }
    }
    slots
}

// Parent files are loaded once, read-only; every byte is bound in the child campaign.
struct Parent {
    binding: Value,
    documents: HashMap<String, Value>,
    _lock: Option<std::fs::File>,
}
fn parent(dir: &Path, cases: &[Value]) -> Result<Parent, String> {
    let lock = std::fs::File::open(dir.join("campaign.lock")).map_err(|e| e.to_string())?;
    lock.try_lock_shared()
        .map_err(|_| "A campaign is still running".to_owned())?;
    let run = read(&dir.join("run.json"))?;
    if run["af255"]["arm"] != "A"
        || run["selected_questions"] != json!(QUESTIONS)
        || run["finished"] != true
    {
        return Err("B requires a finished AF255 A run with the exact frozen subset".into());
    }
    let mut documents = HashMap::<String, Value>::new();
    for c in cases {
        let id = c["id"].as_str().unwrap();
        documents.insert(id.into(), read(&dir.join(format!("{id}.json")))?);
    }
    let binding = json!({"run":recall_models::fingerprint(&run),"documents":documents.iter().map(|(id,d)|(id.clone(),recall_models::fingerprint(d))).collect::<BTreeMap<_,_>>()});
    Ok(Parent {
        binding,
        documents,
        _lock: Some(lock),
    })
}
fn b_slots(parent: &Parent) -> Vec<String> {
    let mut out = Vec::new();
    for (id, doc) in &parent.documents {
        for (op, rows) in doc["operations"].as_object().unwrap() {
            for row in rows.as_array().unwrap() {
                if stage_name(&row["request_content"]) == "R4" {
                    out.push(slot(id, op, "R4"));
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

pub(super) struct Arm {
    pub(super) b: bool,
    pro: String,
    profile: Value,
    parent: Option<Parent>,
    campaign_path: PathBuf,
    campaign: Mutex<Value>,
    // Hold the OS advisory lock across the whole run; process death releases it.
    _lock: std::fs::File,
    live: bool,
    fatal: AtomicBool,
    ds: Option<(String, String)>,
    r4: Option<(String, String)>,
}
impl Arm {
    pub(super) fn config(&self, case: &str, address: &str, calls: u64) -> ChatConfig {
        eval_config(self.b, &self.pro, case, address, calls)
    }
    pub(super) fn upstream(&self, request: &Value) -> Result<&(String, String), String> {
        if self.b && stage_name(request) == "R4" {
            self.r4.as_ref()
        } else {
            self.ds.as_ref()
        }
        .ok_or("AF255 upstream unavailable".into())
    }
    pub(super) fn invalidate(&self) {
        self.fatal.store(true, Ordering::SeqCst);
    }
    fn binding(&self, stage: &str) -> Value {
        let endpoint = if self.b && stage == "R4" {
            "OPENAI_BASE_URL"
        } else {
            "HUOSHAN_DEEPSEEK_BASE_URL"
        };
        // Stage-local profile, not an exclusion list over arbitrary future config fields.
        json!({"version":VERSION,"stage":stage,"model":self.profile["models"][stage],
            "effort":self.profile["efforts"][stage],"output":self.profile["output_tokens"][stage],
            "timeout":self.profile["timeouts_ms"][stage],"compute_effort":if stage=="R4" {self.profile["efforts"]["R4_compute"].clone()} else {Value::Null},
            "compute_output":if stage=="R4" {self.profile["output_tokens"]["R4_compute"].clone()} else {Value::Null},
            "protocol":"openai_responses","endpoint_env":endpoint,"endpoint_hash":self.profile["route_hashes"][endpoint],"hybrid":false})
    }
    pub(super) fn checkpoint(
        &self,
        recorder: &Recorder,
        case: &str,
        key: &str,
        db: &TempDb,
        source_ids: &BTreeMap<String, String>,
    ) -> Result<(), String> {
        let state = db.open().read().map_err(|e| e.to_string())?;
        let mut snapshot = json!({"records":state.repository.list_by_owner(&UserId::from(OWNER)).map_err(|e|e.to_string())?,
            "relations":state.relations,"updates":state.record_updates,"staging":state.staging,
            "sources":source_ids.iter().map(|(id,runtime)| {
                let s=state.sources.values().find(|s|&s.id==runtime).unwrap();
                (id.clone(),json!({"request":s.request,"enrichment":s.enrichment,"error":s.error,"index":s.index,
                    "round":s.task.round,"generation":s.task.generation,"processing_version":s.task.processing_version,
                    "batch":s.result.as_ref().map(|r|json!({"w2":r["w4_batch"]["batch"]["w2"],"review_output":r["w4_batch"]["batch"]["review"]["output"]}))}))
            }).collect::<BTreeMap<_,_>>()});
        ids(
            &mut snapshot,
            &source_ids
                .iter()
                .map(|(s, r)| (r.clone(), s.clone()))
                .collect(),
        );
        // W5 evidence IDs are exactly w1:<volatile source ID>:<candidate index>.
        // Only that structural identity is canonicalized; no USER text or quotes change.
        for record in snapshot["records"].as_array_mut().unwrap() {
            for evidence in record["evidence"].as_array_mut().unwrap() {
                if let Some(id) = evidence["id"].as_str() {
                    for (stable, runtime) in source_ids {
                        if let Some(index) = id.strip_prefix(&format!("w1:{runtime}:")) {
                            if index.parse::<usize>().is_ok() {
                                evidence["id"] = json!(format!("w1:{stable}:{index}"));
                                break;
                            }
                        }
                    }
                }
            }
        }
        self.check_snapshot(recorder, case, key, &snapshot)
    }
    fn check_snapshot(
        &self,
        recorder: &Recorder,
        case: &str,
        key: &str,
        snapshot: &Value,
    ) -> Result<(), String> {
        if let Some(parent) = &self.parent {
            if parent.documents[case]["checkpoints"][key] != *snapshot {
                return Err(format!(
                    "B write state/records/relations changed: {case}/{key}; refusing all further sends"
                ));
            }
        }
        let mut docs = recorder.documents.lock().unwrap();
        let doc = docs.get_mut(case).ok_or("checkpoint without document")?;
        if let Some(old) = doc["checkpoints"].get(key) {
            if old != snapshot {
                return Err("own checkpoint changed; new campaign required".into());
            }
        }
        doc["checkpoints"][key] = snapshot.clone();
        if self.live {
            try_write_json(&recorder.dir.join(format!("{case}.json")), doc)?;
        }
        Ok(())
    }
    fn export_question_bindings(&self, recorder: &Recorder, result: &mut Value) {
        let case = result["case"].as_str().unwrap().to_owned();
        let docs = recorder.documents.lock().unwrap();
        let local = &docs[&case];
        for step in result["steps"].as_array_mut().unwrap() {
            if step["step"] != "question" {
                continue;
            }
            let id = step["question"].as_str().unwrap();
            let op = format!("recall:{id}");
            let rows = if self.b {
                &self.parent.as_ref().unwrap().documents[&case]["operations"][&op]
            } else {
                &local["operations"][&op]
            };
            let requests=rows.as_array().into_iter().flatten().filter(|r|!r.is_null()).map(|r|{
                let stage=stage_name(&r["request_content"]);
                let row=if self.b && stage=="R4" {local["operations"][&op].as_array().into_iter().flatten().find(|x|stage_name(&x["request_content"])=="R4").unwrap_or(r)}else{r};
                (stage.to_owned(),json!({"request":row["request_content"],"request_fingerprint":row["request"],"stage_binding":row["stage_binding"],"response":row["body"]}))
            }).collect::<BTreeMap<_,_>>();
            step["question_binding"] = json!({"stages":requests,"write_snapshot":local["checkpoints"][format!("before:{id}")]});
        }
    }
    fn evidence_equal(
        &self,
        case: &str,
        op: &str,
        position: usize,
        canonical: &Value,
    ) -> Result<(), String> {
        if let Some(parent) = &self.parent {
            let old = &parent.documents[case]["operations"][op][position]["request_content"];
            if old["input"] != canonical["input"] {
                return Err(format!("B R4 input/evidence package changed: {case}/{op}"));
            }
        }
        Ok(())
    }
    fn reserve(&self, key: &str, fp: &str) -> Result<(), String> {
        let mut ledger = self.campaign.lock().unwrap();
        if !ledger["slots"].as_array().unwrap().contains(&json!(key)) {
            return Err(format!("unplanned stage slot: {key}"));
        }
        if ledger["attempts"].get(key).is_some() {
            return Err("未录制（中断在途）；已占额度，禁止重发".into());
        }
        if ledger["circuit"]["open"] == true {
            return Err("熔断未发送（不占额度）".into());
        }
        if ledger["attempts"].as_object().unwrap().len()
            >= ledger["ceiling"].as_u64().unwrap() as usize
        {
            return Err("frozen quota exhausted".into());
        }
        let mut next = ledger.clone();
        next["attempts"][key] = json!({"request":fp,"state":"started"});
        next["order"].as_array_mut().unwrap().push(json!(key));
        try_write_json(&self.campaign_path, &next)?;
        *ledger = next;
        Ok(())
    }
    fn finish(&self, key: &str, recording: &Value) -> Result<(), String> {
        let mut ledger = self.campaign.lock().unwrap();
        let mut next = ledger.clone();
        next["attempts"][key]["state"] = json!("recorded");
        next["attempts"][key]["recording"] = recording.clone();
        rebuild_circuit(&mut next);
        try_write_json(&self.campaign_path, &next)?;
        *ledger = next;
        Ok(())
    }
    fn reconcile(&self, key: &str, saved: &Value) -> Result<(), String> {
        let attempt = self.campaign.lock().unwrap()["attempts"][key].clone();
        if !attempt.is_null() && attempt["request"] != saved["request"] {
            return Err("saved response disagrees with occupied request fingerprint".into());
        }
        // Offline replay must not repair or otherwise mutate the frozen ledger.
        if self.live && attempt["state"] == "started" {
            self.finish(key, saved)?;
        }
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    pub(super) fn respond(
        &self,
        recorder: &Recorder,
        case: &str,
        op: &str,
        position: usize,
        request: &Value,
        canonical: &Value,
        mapping: &BTreeMap<String, String>,
        send: impl FnOnce() -> Result<(u16, String), String>,
    ) -> Result<(u16, String), String> {
        if self.fatal.load(Ordering::SeqCst) {
            return Err("AF255 strict comparison already refused; no further sends".into());
        }
        let stage = stage_name(request);
        if stage == "unknown" {
            return Err("unknown stage".into());
        }
        let key = slot(case, op, stage);
        let fp = recall_models::fingerprint(canonical);
        let binding = self.binding(stage);
        let inverse = mapping
            .iter()
            .map(|(a, b)| (b.clone(), a.clone()))
            .collect();
        let local = recorder.documents.lock().unwrap()[case]["operations"][op][position].clone();
        let saved = if self.b && stage != "R4" {
            self.parent.as_ref().ok_or("B requires parent")?.documents[case]["operations"][op]
                [position]
                .clone()
        } else {
            local
        };
        if stage == "R4" {
            self.evidence_equal(case, op, position, canonical)?;
        }
        if !saved.is_null() {
            if saved["request"] != fp
                || saved["request_content"] != *canonical
                || saved["stage_binding"] != binding
            {
                return Err(format!("strict stage binding changed: {key}; no resend"));
            }
            self.reconcile(&key, &saved)?;
            recorder.observe(case, op, request, &saved, "replay", Duration::ZERO, None);
            return Ok((
                saved["http_status"]
                    .as_u64()
                    .ok_or("recorded status missing")? as u16,
                response_ids(
                    saved["body"].as_str().ok_or("recorded body missing")?,
                    &inverse,
                ),
            ));
        }
        if self.b && stage != "R4" && saved.is_null() {
            return Err(format!(
                "B non-R4 parent recording missing: {key}; no fallback send"
            ));
        }
        let refuse = if self.b && stage != "R4" {
            Some("B non-R4 parent recording missing; no fallback send".to_owned())
        } else if !self.live {
            Some("未录制".into())
        } else {
            self.reserve(&key, &fp).err()
        };
        if let Some(reason) = refuse {
            recorder
                .operations
                .lock()
                .unwrap()
                .get_mut(case)
                .unwrap()
                .missing = Some(reason.clone());
            recorder.observe(
                case,
                op,
                request,
                &Value::Null,
                "not_recorded",
                Duration::ZERO,
                Some(&reason),
            );
            return Ok((503, json!({"error":reason}).to_string()));
        }
        let send_id=recorder.start_send(json!({"case":case,"operation":op,"position":position,"stage":request.pointer("/text/format/name"),"request":fp}))?;
        let started = Instant::now();
        let (status, body, error) = match send() {
            Ok((s, b)) => (s, b, None),
            Err(e) => (
                503,
                json!({"error":{"type":"transport_error"}}).to_string(),
                Some(e),
            ),
        };
        let envelope: Value = serde_json::from_str(&body).unwrap_or_default();
        let recording = json!({"schema":request.pointer("/text/format/name"),"endpoint":request["model"],"stage_binding":binding,
            "send_run":recorder.run_id,"send_index":send_id,"request":fp,"request_content":canonical,
            "http_status":status,"body":response_ids(&body,mapping),"usage":envelope["usage"],"elapsed_ms":started.elapsed().as_millis(),
            "status":envelope["status"],"incomplete_details":envelope["incomplete_details"],"transport_error":error});
        {
            let mut docs = recorder.documents.lock().unwrap();
            let doc = docs.get_mut(case).unwrap();
            if doc["operations"][op].is_null() {
                doc["operations"][op] = json!([]);
            }
            let rows = doc["operations"][op].as_array_mut().unwrap();
            rows.resize(position + 1, Value::Null);
            rows[position] = recording.clone();
            try_write_json(&recorder.dir.join(format!("{case}.json")), doc)?;
        }
        self.finish(&key, &recording)?;
        recorder.finish_send(send_id, &recording)?;
        recorder.observe(
            case,
            op,
            request,
            &recording,
            "live",
            started.elapsed(),
            None,
        );
        Ok((status, body))
    }
}

fn validate_campaign(ledger: &Value, identity: &Value) -> Result<(), String> {
    if ledger["identity"] != *identity
        || ledger["slots"] != identity["slots"]
        || ledger["ceiling"] != identity["ceiling"]
    {
        return Err("frozen campaign authorization changed".into());
    }
    let attempts = ledger["attempts"]
        .as_object()
        .ok_or("invalid campaign attempts")?;
    let slots: BTreeSet<_> = identity["slots"]
        .as_array()
        .ok_or("invalid frozen slots")?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let order: Vec<_> = ledger["order"]
        .as_array()
        .ok_or("invalid send order")?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    if attempts.len() > identity["ceiling"].as_u64().ok_or("invalid ceiling")? as usize
        || order.len() != attempts.len()
        || order.iter().copied().collect::<BTreeSet<_>>().len() != order.len()
        || attempts
            .keys()
            .any(|key| !slots.contains(key.as_str()) || !order.contains(&key.as_str()))
    {
        return Err("campaign occupancy exceeds or disagrees with frozen slots".into());
    }
    for attempt in attempts.values() {
        if !matches!(attempt["state"].as_str(), Some("started" | "recorded"))
            || attempt["request"].as_str().is_none()
        {
            return Err("invalid frozen send attempt".into());
        }
        if attempt["state"] == "recorded" && attempt["request"] != attempt["recording"]["request"] {
            return Err("recorded attempt fingerprint changed".into());
        }
    }
    Ok(())
}

fn failure_key(recording: &Value) -> Option<String> {
    let body: Value = recording["body"]
        .as_str()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    if recording["http_status"].as_u64().unwrap_or(0) >= 400 {
        Some(
            json!([
                recording["http_status"],
                body["error"]["type"],
                body["error"]["code"]
            ])
            .to_string(),
        )
    } else if body["status"] == "incomplete"
        && body["incomplete_details"]["reason"] == "max_output_tokens"
    {
        Some("incomplete:max_output_tokens".into())
    } else {
        None
    }
}
fn rebuild_circuit(ledger: &mut Value) {
    let mut attempts: Vec<_> = ledger["attempts"]
        .as_object()
        .unwrap()
        .values()
        .filter_map(|a| a.get("recording"))
        .collect();
    attempts.sort_by_key(|r| {
        (
            r["send_run"].as_str().unwrap_or(""),
            r["send_index"].as_u64().unwrap_or(0),
        )
    });
    // Persisted ordinal is the actual send order, including across resumptions.
    let order = ledger["order"].as_array().cloned().unwrap_or_default();
    if !order.is_empty() {
        attempts = order
            .iter()
            .filter_map(|key| ledger["attempts"][key.as_str().unwrap()].get("recording"))
            .collect();
    }
    let mut last = None;
    let mut n = 0;
    let mut open = false;
    for r in attempts {
        let key = failure_key(r);
        if key.is_some() && key == last {
            n += 1;
        } else {
            n = usize::from(key.is_some());
        }
        last = key;
        if n >= 3 {
            open = true;
        }
    }
    ledger["circuit"] = json!({"open":open,"consecutive":n,"key":last});
}

fn endpoint(env: &str) -> Result<String, String> {
    let base = std::env::var(env).map_err(|_| format!("missing {env}"))?;
    let provider: ProviderConfig =
        serde_json::from_value(json!({"protocol":"openai_responses","base_url_env":env})).unwrap();
    provider.endpoint(&base).map_err(|e| e.to_string())
}

pub(super) fn run(arm: &str) {
    assert!(matches!(arm, "A" | "B"), "EVAL_E2E_AB must be A or B");
    assert!(
        std::env::var_os("EVAL_CASES").is_none() && std::env::var_os("EVAL_QUESTIONS").is_none(),
        "AF255 subset is frozen"
    );
    let b = arm == "B";
    let cases = subset();
    let root = std::env::var_os("EVAL_AB_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/eval-e2e-af255"));
    assert!(
        root.starts_with(Path::new("target"))
            || root.starts_with(Path::new(env!("CARGO_MANIFEST_DIR")).join("target"))
            || root.starts_with(std::env::temp_dir()),
        "AF255 artifacts must be in target or temp"
    );
    let dir = root.join(arm);
    let parent = if b && root.join("A/run.json").exists() {
        Some(parent(&root.join("A"), &cases).unwrap())
    } else {
        None
    };
    let pro = std::env::var("DEEPSEEK_PRO_MODEL").ok().or_else(|| {
        parent
            .as_ref()
            .and_then(|_| read(&root.join("A/run.json")).ok())
            .and_then(|r| r["af255"]["models"]["W1"].as_str().map(str::to_owned))
    });
    let dry = std::env::var("EVAL_DRY_RUN").as_deref() == Ok("1");
    let pro = pro.unwrap_or_else(|| {
        assert!(dry, "DEEPSEEK_PRO_MODEL required");
        "$DEEPSEEK_PRO_MODEL".into()
    });
    let mut cfg = profile(b, &pro);
    cfg["route_hashes"] = json!({});
    for env in ["HUOSHAN_DEEPSEEK_BASE_URL", "OPENAI_BASE_URL"] {
        if env == "OPENAI_BASE_URL" && !b {
            continue;
        }
        let hash = std::env::var(env)
            .ok()
            .map(|_| recall_models::fingerprint(&json!(endpoint(env).unwrap())))
            .or_else(|| {
                parent
                    .as_ref()
                    .and_then(|_| read(&root.join("A/run.json")).ok())
                    .map(|r| r["af255"]["route_hashes"][env].as_str().map(str::to_owned))
                    .flatten()
            })
            .unwrap_or_else(|| {
                assert!(dry, "missing upstream base URL for binding");
                format!("${env}")
            });
        cfg["route_hashes"][env] = json!(hash);
    }
    let stage_slots = if b {
        parent.as_ref().map(b_slots).unwrap_or_else(|| {
            QUESTIONS
                .iter()
                .map(|q| {
                    let (c, q) = q.split_once('/').unwrap();
                    slot(c, &format!("recall:{q}"), "R4")
                })
                .collect()
        })
    } else {
        slots(&cases)
    };
    let ceiling = stage_slots.len() as u64;
    assert!(ceiling <= if b { 15 } else { 104 });
    let identity = json!({"version":VERSION,"profile":cfg,"questions":QUESTIONS,"cases":recall_models::fingerprint(&input_identity(&cases)),"slots":stage_slots,"ceiling":ceiling,
        "parent":parent.as_ref().map(|p|p.binding.clone())});
    let old = if dir.join("campaign.json").exists() {
        Some(read(&dir.join("campaign.json")).unwrap())
    } else {
        None
    };
    if let Some(old) = &old {
        validate_campaign(old, &identity).unwrap();
    }
    let used = old
        .as_ref()
        .map_or(0, |v| v["attempts"].as_object().unwrap().len());
    println!("AF255 {arm}：10 例 / 15 问；混合检索关闭，retrieval=disabled，嵌入 0。");
    let mut totals = [0; 5];
    for c in &cases {
        let n = counts(c);
        for i in 0..5 {
            totals[i] += n[i];
        }
    }
    println!(
        "阶段槽位 W1/W3/W4/R1/R4 = {}；总上限 {ceiling}；已占 {used}；剩余额度 {}。",
        if b {
            format!("0/0/0/0/{ceiling}")
        } else {
            totals
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join("/")
        },
        ceiling - used as u64
    );
    println!(
        "B 仅替换 R4；非 R4（含失败响应）严格只读复用 A；A 未完成时 B=15 为上限，不是已生成可发送计划。"
    );
    println!(
        "token 情景（非上界）：A 输入420554 / 输出90742；B 输入72245 / 输出3537；合计约 7–8 元（冻结参考价，长 LME 输入与未知输出会改变费用）。"
    );
    if dry {
        println!("干跑：发送 0；未读取密钥、未建 campaign、未绑定端口。A/B 总授权上限 119。");
        return;
    }
    assert!(
        !b || parent.is_some(),
        "B requires completed A before execution"
    );
    let limit = std::env::var("EVAL_RECORD")
        .ok()
        .map(|s| s.parse::<u64>().unwrap());
    if let Some(limit) = limit {
        assert_eq!(
            limit, ceiling,
            "EVAL_RECORD must equal this arm's exact frozen ceiling"
        );
    }
    let live = limit.is_some();
    std::fs::create_dir_all(&dir).unwrap();
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("campaign.lock"))
        .unwrap();
    lock.try_lock()
        .expect("another AF255 process owns campaign");
    let campaign = if dir.join("campaign.json").exists() {
        read(&dir.join("campaign.json")).unwrap()
    } else {
        json!({"identity":identity,"slots":stage_slots,"ceiling":ceiling,"attempts":{},"order":[],"circuit":{}})
    };
    validate_campaign(&campaign, &identity).unwrap();
    if live {
        write_json(&dir.join("campaign.json"), &campaign);
    }
    let ds = if live && !b {
        Some((
            endpoint("HUOSHAN_DEEPSEEK_BASE_URL").unwrap(),
            std::env::var("HUOSHAN_DEEPSEEK_API_KEY").expect("missing DS key"),
        ))
    } else {
        None
    };
    let r4 = if live && b {
        Some((
            endpoint("OPENAI_BASE_URL").unwrap(),
            std::env::var("OPENAI_API_KEY").expect("missing OpenAI key"),
        ))
    } else {
        None
    };
    let mut recorder = Recorder::new(dir.clone(), limit, None);
    recorder.log_dir = dir.join(if live { "runs" } else { "runs-replay" });
    let mut configuration = eval_config(b, &pro, "configuration", "", 1);
    configuration.model_budget = None;
    recorder.configuration = serde_json::to_value(configuration).unwrap();
    let arm_state = Arc::new(Arm {
        b,
        pro,
        profile: cfg.clone(),
        parent,
        campaign_path: dir.join("campaign.json"),
        campaign: Mutex::new(campaign),
        _lock: lock,
        live,
        fatal: AtomicBool::new(false),
        ds,
        r4,
    });
    recorder.ab = Some(arm_state.clone());
    let recorder = Arc::new(recorder);
    let (address, stop, handle) = server(recorder.clone(), None).unwrap();
    unsafe {
        std::env::set_var("PMR_MODEL_NETWORK", "offline");
        std::env::set_var("DEEPSEEK_API_KEY", "eval-replay");
    }
    let output = dir.join(if live { "run.json" } else { "run-replay.json" });
    let mut document = json!({"pipeline":"v3","label":format!("af255-{arm}"),"af255":cfg,"selected_questions":QUESTIONS,
        "execution_mode":if live {"live_or_mixed"}else{"replay"},"run_id":recorder.run_id,"send_log":recorder.send_log_path(),"cases":[],"finished":false,"jobs":1});
    write_json(&output, &document);
    let mut results = Vec::new();
    let started = Instant::now();
    // Serial case execution makes the 3-failure breaker exact, without unbounded in-flight sends.
    for c in &cases {
        let now = Instant::now();
        let mut result = run_case(c, &recorder, &address);
        result["elapsed_ms"] = json!(now.elapsed().as_millis());
        result["calls"] = json!(recorder.calls(c["id"].as_str().unwrap()));
        results.push(result);
        document["cases"] = json!(results);
        document["real_calls"] = json!(recorder.sent.load(Ordering::Relaxed));
        write_json(&output, &document);
    }
    stop.store(true, Ordering::SeqCst);
    handle.join().unwrap();
    for r in &mut results {
        r["calls"] = json!(recorder.calls(r["case"].as_str().unwrap()));
        arm_state.export_question_bindings(&recorder, r);
    }
    document["cases"] = json!(results);
    document["finished"] = json!(true);
    document["elapsed_ms"] = json!(started.elapsed().as_millis());
    document["real_calls"] = json!(recorder.sent.load(Ordering::Relaxed));
    document["campaign"] = arm_state.campaign.lock().unwrap().clone();
    write_json(&output, &document);
    println!(
        "真实发送 {}；输出 {}",
        recorder.sent.load(Ordering::Relaxed),
        output.display()
    );
}

impl AbHooks for Arm {
    fn config(&self, case: &str, address: &str, calls: u64) -> ChatConfig {
        Arm::config(self, case, address, calls)
    }
    fn upstream(&self, request: &Value) -> Result<&(String, String), String> {
        Arm::upstream(self, request)
    }
    fn invalidate(&self) {
        Arm::invalidate(self)
    }
    fn checkpoint(
        &self,
        recorder: &Recorder,
        case: &str,
        key: &str,
        db: &TempDb,
        ids: &BTreeMap<String, String>,
    ) -> Result<(), String> {
        Arm::checkpoint(self, recorder, case, key, db, ids)
    }
    fn respond(
        &self,
        recorder: &Recorder,
        case: &str,
        op: &str,
        position: usize,
        request: &Value,
        canonical: &Value,
        mapping: &BTreeMap<String, String>,
        send: &mut dyn FnMut() -> Result<(u16, String), String>,
    ) -> Result<(u16, String), String> {
        Arm::respond(
            self, recorder, case, op, position, request, canonical, mapping, send,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    pub(super) struct Scratch(pub(super) PathBuf);
    impl Scratch {
        pub(super) fn new() -> Self {
            let p = std::env::temp_dir().join(format!("af255-test-{}", diagnostics::new_id()));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        pub(super) fn recorder(
            &self,
            b: bool,
            parent: Option<Parent>,
            slots: Vec<String>,
            live: bool,
        ) -> Recorder {
            let mut rec = Recorder::new(self.0.clone(), live.then_some(slots.len() as u64), None);
            let file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(self.0.join("lock"))
                .unwrap();
            file.try_lock().unwrap();
            let ledger = if self.0.join("campaign.json").exists() {
                read(&self.0.join("campaign.json")).unwrap()
            } else {
                json!({"slots":slots,"ceiling":slots.len(),"attempts":{},"order":[],"circuit":{}})
            };
            rec.ab = Some(Arc::new(Arm {
                b,
                pro: "pro".into(),
                profile: profile(b, "pro"),
                parent,
                campaign_path: self.0.join("campaign.json"),
                campaign: Mutex::new(ledger),
                _lock: file,
                live,
                fatal: AtomicBool::new(false),
                ds: None,
                r4: None,
            }));
            rec
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
    pub(super) fn request(stage: &str) -> Value {
        json!({"model":"pro","input":"{\"evidence\":{\"records\":[1]}}","text":{"format":{"name":match stage{"W1"=>"w1_v4","W3"=>"w3_v4","W4"=>"w4_v5","R1"=>"r1_v2",_=>"r4_v7"}}}})
    }
    fn envelope() -> String {
        json!({"status":"completed","usage":{"input_tokens":10,"output_tokens":5},"output":[]})
            .to_string()
    }
    pub(super) fn saved_parent(rec: &Recorder) -> Parent {
        Parent {
            binding: json!({}),
            documents: rec.documents.lock().unwrap().clone(),
            _lock: None,
        }
    }
    #[test]
    fn af256_b_keeps_the_offline_loopback_adapter_for_r4() {
        let cfg = eval_config(true, "pro", "synthetic", "127.0.0.1:1", 7);
        assert_eq!(
            cfg.model_base_url.as_deref(),
            Some("http://127.0.0.1:1/v1/case/synthetic")
        );
        assert!(
            cfg.recall_models.as_ref().unwrap().providers.is_empty(),
            "explicit R4 providers require PMR_MODEL_NETWORK=live and would gate the offline recording proxy"
        );
        assert_eq!(cfg.recall_models.unwrap().r4, "gpt-6.1-sol");
    }
    #[test]
    fn af255_subset_and_role_profiles_are_frozen_and_embedding_off() {
        let cases = subset();
        assert_eq!(cases.len(), 10);
        assert_eq!(slots(&cases).len(), 104);
        let mut relabeled = cases.clone();
        relabeled[0]["questions"][0]["expect_common"] = json!({"status":"different label"});
        assert_eq!(input_identity(&cases), input_identity(&relabeled));
        let mut total = [0; 5];
        for c in &cases {
            let n = counts(c);
            for i in 0..5 {
                total[i] += n[i];
            }
        }
        assert_eq!(total, [28, 28, 18, 15, 15]);
        for b in [false, true] {
            let cfg = eval_config(b, "pro", "test", "localhost:1", 104);
            assert!(!cfg.embedding.enabled);
            let w = cfg.write_models.unwrap();
            assert_eq!(w.effort("w1"), ReasoningEffort::Low);
            assert_eq!(w.effort("w3"), ReasoningEffort::Low);
            assert_eq!(w.effort("w4"), ReasoningEffort::Medium);
            let r = cfg.recall_models.unwrap();
            assert_eq!(r.effort("r1"), ReasoningEffort::Minimal);
            assert_eq!(
                r.effort("r4"),
                if b {
                    ReasoningEffort::Low
                } else {
                    ReasoningEffort::Minimal
                }
            );
            assert_eq!(cfg.model_budget.unwrap().r4_compute_output_tokens, 16384);
            assert!(r.providers.is_empty());
        }
    }
    #[test]
    fn af255_b_replays_failed_non_r4_and_only_sends_r4_with_identical_evidence() {
        let a = Scratch::new();
        let rec = a.recorder(
            false,
            None,
            vec![slot("c", "recall:q", "R1"), slot("c", "recall:q", "R4")],
            true,
        );
        rec.begin("c", "recall:q", &BTreeMap::new());
        let failure = json!({"error":{"type":"invalid_request_error","code":"bad"}}).to_string();
        assert_eq!(
            rec.try_respond("c", &request("R1"), || Ok((400, failure.clone())))
                .unwrap(),
            (400, failure.clone())
        );
        rec.try_respond("c", &request("R4"), || Ok((200, envelope())))
            .unwrap();
        let parent = saved_parent(&rec);
        let b = Scratch::new();
        let child = b.recorder(true, Some(parent), vec![slot("c", "recall:q", "R4")], true);
        child.begin("c", "recall:q", &BTreeMap::new());
        assert_eq!(
            child
                .try_respond("c", &request("R1"), || panic!("non-R4 sent"))
                .unwrap(),
            (400, failure)
        );
        child
            .try_respond("c", &request("R4"), || Ok((200, envelope())))
            .unwrap();
        assert_eq!(child.sent.load(Ordering::Relaxed), 1);
        let changed = Scratch::new();
        let bad = changed.recorder(
            true,
            Some(saved_parent(&rec)),
            vec![slot("c", "recall:q", "R4")],
            true,
        );
        bad.begin("c", "recall:q", &BTreeMap::new());
        bad.try_respond("c", &request("R1"), || panic!()).unwrap();
        let mut wire = request("R4");
        wire["input"] = json!("{\"evidence\":{\"records\":[2]}}");
        assert!(
            bad.try_respond("c", &wire, || panic!("changed evidence sent"))
                .unwrap_err()
                .contains("evidence package")
        );
    }
    #[test]
    fn af255_non_r4_fingerprint_change_or_missing_never_sends() {
        let a = Scratch::new();
        let rec = a.recorder(false, None, vec![slot("c", "write:m", "W1")], true);
        rec.begin("c", "write:m", &BTreeMap::new());
        rec.try_respond("c", &request("W1"), || Ok((200, envelope())))
            .unwrap();
        for missing in [false, true] {
            let mut parent = saved_parent(&rec);
            if missing {
                parent.documents.get_mut("c").unwrap()["operations"]["write:m"] = json!([]);
            }
            let b = Scratch::new();
            let child = b.recorder(true, Some(parent), vec![], true);
            child.begin("c", "write:m", &BTreeMap::new());
            let mut wire = request("W1");
            if !missing {
                wire["instructions"] = json!("changed prompt");
            }
            assert!(
                child
                    .try_respond("c", &wire, || panic!("B fallback sent"))
                    .is_err()
            );
            assert_eq!(child.sent.load(Ordering::Relaxed), 0);
        }
    }
    #[test]
    fn af255_interrupted_slot_is_occupied_after_restart_and_other_slot_can_send() {
        let s = Scratch::new();
        let keys = vec![slot("c", "write:m1", "W1"), slot("c", "write:m2", "W1")];
        {
            let rec = s.recorder(false, None, keys.clone(), true);
            rec.ab.as_ref().unwrap().invalidate(); // No physical send needed to simulate crash before response.
            let mut ledger =
                json!({"slots":keys,"ceiling":2,"attempts":{},"order":[],"circuit":{}});
            ledger["attempts"][&keys[0]] = json!({"request":"fp","state":"started"});
            ledger["order"] = json!([keys[0]]);
            write_json(&s.0.join("campaign.json"), &ledger);
        }
        let rec = s.recorder(false, None, keys, true);
        rec.begin("c", "write:m1", &BTreeMap::new());
        let (status, body) = rec
            .try_respond("c", &request("W1"), || panic!("interrupted resent"))
            .unwrap();
        assert_eq!(status, 503);
        assert!(body.contains("中断在途"));
        rec.begin("c", "write:m2", &BTreeMap::new());
        rec.try_respond("c", &request("W1"), || Ok((200, envelope())))
            .unwrap();
        assert_eq!(
            read(&s.0.join("campaign.json")).unwrap()["attempts"]
                .as_object()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(rec.sent.load(Ordering::Relaxed), 1);
    }
    #[test]
    fn af255_response_saved_before_ledger_finish_replays_readonly_and_checks_occupied_identity() {
        let s = Scratch::new();
        let key = slot("c", "write:m", "W1");
        {
            let rec = s.recorder(false, None, vec![key.clone()], true);
            rec.begin("c", "write:m", &BTreeMap::new());
            rec.try_respond("c", &request("W1"), || Ok((200, envelope())))
                .unwrap();
        }
        let path = s.0.join("campaign.json");
        let mut ledger = read(&path).unwrap();
        ledger["attempts"][&key]["state"] = json!("started");
        ledger["attempts"][&key]
            .as_object_mut()
            .unwrap()
            .remove("recording");
        write_json(&path, &ledger);
        let before = std::fs::read(&path).unwrap();
        {
            let rec = s.recorder(false, None, vec![key.clone()], false);
            rec.begin("c", "write:m", &BTreeMap::new());
            assert_eq!(
                rec.try_respond("c", &request("W1"), || panic!("replay sent"))
                    .unwrap()
                    .0,
                200
            );
        }
        assert_eq!(std::fs::read(&path).unwrap(), before);
        ledger["attempts"][&key]["request"] = json!("wrong occupied fingerprint");
        write_json(&path, &ledger);
        let rec = s.recorder(false, None, vec![key], true);
        rec.begin("c", "write:m", &BTreeMap::new());
        assert!(
            rec.try_respond("c", &request("W1"), || panic!("mismatch resent"))
                .is_err()
        );
        assert_eq!(rec.sent.load(Ordering::Relaxed), 0);
    }
    #[test]
    fn af255_three_same_http_errors_or_truncations_break_across_restart_without_spending_remaining()
    {
        for incomplete in [false, true] {
            let s = Scratch::new();
            let keys: Vec<_> = (0..4)
                .map(|i| slot("c", &format!("write:m{i}"), "W1"))
                .collect();
            {
                let rec = s.recorder(false, None, keys.clone(), true);
                for i in 0..3 {
                    rec.begin("c", &format!("write:m{i}"), &BTreeMap::new());
                    rec.try_respond("c",&request("W1"),||Ok(if incomplete{(200,json!({"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"output":[{"content":[{"type":"output_text","text":"bounded diagnostic"}]}]}).to_string())}else{(400,json!({"error":{"type":"invalid_request_error","code":"bad"}}).to_string())})).unwrap();
                }
            }
            let rec = s.recorder(false, None, keys, true);
            rec.begin("c", "write:m3", &BTreeMap::new());
            let (_, body) = rec
                .try_respond("c", &request("W1"), || panic!("open circuit sent"))
                .unwrap();
            assert!(body.contains("熔断未发送"));
            assert_eq!(
                read(&s.0.join("campaign.json")).unwrap()["attempts"]
                    .as_object()
                    .unwrap()
                    .len(),
                3
            );
        }
    }
    #[test]
    fn af255_write_snapshot_changes_refuse_even_without_r4() {
        let a = Scratch::new();
        let rec = a.recorder(false, None, vec![], true);
        rec.begin("c", "write:m", &BTreeMap::new());
        let snap = json!({"records":[1],"relations":[],"sources":{"m":{"enrichment":"pending"}}});
        // Test the exact comparison separately from SQLite construction (covered by run_case).
        let arm = rec.ab.as_ref().unwrap();
        let _ = arm;
        let mut parent = saved_parent(&rec);
        parent.documents.get_mut("c").unwrap()["checkpoints"] = json!({"write:m":snap});
        let b = Scratch::new();
        let child = b.recorder(true, Some(parent), vec![], true);
        child.begin("c", "write:m", &BTreeMap::new());
        let p = child.ab.as_ref().unwrap();
        let _ = p;
        let db = TempDb(b.0.join("state.sqlite3"));
        assert!(
            child
                .ab
                .as_ref()
                .unwrap()
                .checkpoint(&child, "c", "write:m", &db, &BTreeMap::new())
                .is_err()
        );
    }
    #[test]
    fn af255_os_campaign_lock_blocks_second_process_and_releases_on_drop() {
        let s = Scratch::new();
        let a = std::fs::File::create(s.0.join("lock")).unwrap();
        a.try_lock().unwrap();
        let b = std::fs::OpenOptions::new()
            .write(true)
            .open(s.0.join("lock"))
            .unwrap();
        assert!(b.try_lock().is_err());
        drop(a);
        // A concurrent test may briefly fork with the descriptor before exec
        // closes it. Allow that bounded kernel handoff without weakening exclusion.
        let deadline = Instant::now() + Duration::from_secs(2);
        while b.try_lock().is_err() {
            assert!(
                Instant::now() < deadline,
                "campaign lock remained held after drop"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

#[test]
fn af255_role_forwarder_headers_are_shared_without_sending() {
    let scratch = tests::Scratch::new();
    let mut rec = scratch.recorder(true, None, vec![], false);
    // Constructing a request exercises headers without touching an endpoint.
    let client = reqwest::blocking::Client::new();
    let wire = hippocampus::provider_http_request(
        &client,
        "http://127.0.0.1:1/responses",
        "fixture",
        hippocampus::model_provider::ProviderProtocol::OpenaiResponses,
    )
    .json(&json!({"instructions":"fixture"}))
    .build()
    .unwrap();
    assert_eq!(
        wire.headers()[reqwest::header::USER_AGENT],
        hippocampus::model_provider::MODEL_USER_AGENT
    );
    assert_eq!(
        wire.headers()[reqwest::header::AUTHORIZATION],
        "Bearer fixture"
    );
    // Even with configured upstreams, dry/replay refuses a send before transport resolution.
    rec.begin("c", "recall:q", &BTreeMap::new());
    assert_eq!(
        rec.try_respond("c", &tests::request("R4"), || panic!("offline sent"))
            .unwrap()
            .0,
        503
    );
    rec.ab = None;
}

#[test]
fn af255_production_loopback_roundtrip_reuses_write_state_and_only_changes_r4() {
    fn fixture(request: &Value) -> Value {
        let input: Value = serde_json::from_str(request["input"].as_str().unwrap()).unwrap();
        assert!(input.get("model").is_none());
        match stage_name(request) {
            "W1" => {
                let text = &input["current_user_message"];
                json!({"candidates":[{"kind":"assertion","speech":"user_statement","subject":{"entity_id":"self"},"relation":"fact","memory_type":"fact","value":{"kind":"text","text":text,"quote":{"text":text}},"sentence":{"text":text},"polarity":"affirm","strength":null,"uncertainty":null,"conditions":[],"times":[]}],"memory_requests":[]})
            }
            "W3" => {
                json!({"reviews":input["candidates"].as_array().unwrap().iter().map(|c|json!({"candidate":c["index"],"verdict":"supported","problems":[],"episode_value":"not_applicable"})).collect::<Vec<_>>(),"coverage":{"status":"complete","missing":[]},"request_reviews":[]})
            }
            "W4" => {
                let c = &input["w4_input"]["candidates"][0];
                let r = &input["w4_input"]["records"][0];
                json!({"decisions":[{"candidate":c["index"],"status":"decided","action":"reinforce","targets":[r["id"]],"resolves_relations":[],"basis":[r["evidence"][0]],"effective":null,"relation_scope":null,"pending_reason":null}]})
            }
            "R1" => {
                json!({"need":"judge","current_evidence":"query_only","type":"single","subject":{"entity_id":"self"},"relation":"研究","conditions":[],"time":null,"plan":{"op":"lookup","filter":{"relation":"fact","time":null},"unit":null}})
            }
            "R4" => {
                let r = &input["evidence"]["records"][0];
                json!({"verdict":"supported","facts":[{"statement":r["candidate"]["sentence"]["text"],"evidence":r["evidence"],"record_ids":[r["id"]],"polarity":"affirm","strength":null,"uncertainty":null,"conditions":[],"times":[],"current":"yes","applies":"yes","supersedes_record_ids":[]}],"compute":null})
            }
            _ => panic!("unexpected stage"),
        }
    }
    let case = json!({"id":"synthetic","entities":[{"id":"self","canonical_name":"本人","aliases":[],"entity_type":"Person"}],"messages":[{"id":"m1","text":"我研究数据库索引。"},{"id":"m2","text":"我研究数据库索引。"}],"questions":[{"id":"q","after":2,"query":"我研究什么？","context":[]}]});
    let a = tests::Scratch::new();
    let ar = Arc::new(a.recorder(false, None, slots(&[case.clone()]), true));
    let (address, stop, handle) = server(ar.clone(), Some(fixture)).unwrap();
    unsafe {
        std::env::set_var("PMR_MODEL_NETWORK", "offline");
        std::env::set_var("DEEPSEEK_API_KEY", "eval-replay");
    }
    let first = run_case(&case, &ar, &address);
    stop.store(true, Ordering::SeqCst);
    handle.join().unwrap();
    assert_eq!(ar.sent.load(Ordering::Relaxed), 7);
    assert_eq!(first["steps"][2]["state"], "completed");
    let parent = tests::saved_parent(&ar);
    let before = std::fs::read(a.0.join("synthetic.json")).unwrap();
    let b = tests::Scratch::new();
    let br = Arc::new(b.recorder(
        true,
        Some(parent),
        vec![slot("synthetic", "recall:q", "R4")],
        true,
    ));
    let (address, stop, handle) = server(br.clone(), Some(fixture)).unwrap();
    let second = run_case(&case, &br, &address);
    stop.store(true, Ordering::SeqCst);
    handle.join().unwrap();
    assert_eq!(br.sent.load(Ordering::Relaxed), 1);
    assert_eq!(second["steps"][2]["state"], "completed");
    assert_eq!(std::fs::read(a.0.join("synthetic.json")).unwrap(), before);
    assert_eq!(
        br.calls("synthetic")
            .iter()
            .filter(|c| c["origin"] == "replay")
            .count(),
        6
    );
}

#[test]
fn af255_real_sqlite_w2_w3_commit_checkpoints_match_without_ports() {
    use hippocampus::contract::*;
    use hippocampus::enrichment::{self, RuntimeInputBudget, UpstreamBatch};
    fn populate(db: &TempDb, b: bool) -> BTreeMap<String, String> {
        let cfg = eval_config(b, "pro", "synthetic", "localhost:1", 7);
        let mut service = ChatMemoryService::new(db.open(), cfg).unwrap();
        service
            .call(
                "memory_entity_upsert",
                json!({"id":"self","canonical_name":"本人","aliases":[],"entity_type":"Person"}),
                || false,
            )
            .unwrap();
        let received=service.call("memory_remember",json!({"text":"我研究数据库索引。","session_id":"eval-synthetic","turn_id":"m1","capture_source":true,"observed_at":CLOCK}),||false).unwrap();
        let source = received["source"]["source_id"].as_str().unwrap().to_owned();
        let mut store = db.open();
        let state = store.read().unwrap();
        let key = state
            .sources
            .iter()
            .find(|(_, s)| s.id == source)
            .unwrap()
            .0
            .clone();
        w4_start_round(&mut store, &key, "fixture-round", false).unwrap();
        let receipt=enrichment::run_source(&mut store,&key,RuntimeInputBudget::default(),|ctx,input| {
            let candidate:Candidate=serde_json::from_value(json!({"kind":"assertion","speech":"user_statement","subject":{"entity_id":"self"},"relation":"fact","memory_type":"fact","value":{"kind":"text","text":input.text,"quote":{"text":input.text}},"sentence":{"text":input.text},"polarity":"affirm","strength":null,"uncertainty":null,"conditions":[],"times":[]})).unwrap();
            let batch=w2::Batch{candidates:vec![w2::CandidateResult{index:0,status:w2::Status::Ok,candidate,time_resolutions:vec![],reasons:vec![]}],memory_requests:vec![],index_map:vec![Some(0)],task_id:None,said_on:Some("2026-09-28".into()),utc_offset_seconds:28800};
            let output:W3Output=serde_json::from_value(json!({"reviews":[{"candidate":0,"verdict":"supported","problems":[],"episode_value":"not_applicable"}],"coverage":{"status":"complete","missing":[]},"request_reviews":[]})).unwrap();
            let review=output.validate(&batch,&ctx.source_binding,&input.text).unwrap();
            Ok(UpstreamBatch{w2:batch,review:Some(review)})
        },|_,_,_|panic!("first source cannot need W4 model")).unwrap();
        assert_eq!(receipt["enrichment"]["status"], "committed");
        BTreeMap::from([("m1".into(), source)])
    }
    let a = tests::Scratch::new();
    let adb = TempDb(a.0.join("memory.sqlite3"));
    let sources = populate(&adb, false);
    let rec = a.recorder(false, None, vec![], true);
    rec.begin("synthetic", "write:m1", &sources);
    rec.ab
        .as_ref()
        .unwrap()
        .checkpoint(&rec, "synthetic", "write:m1", &adb, &sources)
        .unwrap();
    let b = tests::Scratch::new();
    let bdb = TempDb(b.0.join("memory.sqlite3"));
    let sources_b = populate(&bdb, true);
    let child = b.recorder(true, Some(tests::saved_parent(&rec)), vec![], true);
    child.begin("synthetic", "write:m1", &sources_b);
    assert_ne!(sources, sources_b);
    child
        .ab
        .as_ref()
        .unwrap()
        .checkpoint(&child, "synthetic", "write:m1", &bdb, &sources_b)
        .unwrap();
    // Altering a real committed record must refuse the child, even if no R4 was called.
    bdb.open()
        .transaction(|state| {
            let mut r = state
                .repository
                .list_by_owner(&UserId::from(OWNER))?
                .remove(0);
            r.value = MemoryValue::Text("changed".into());
            state.repository.update(r)?;
            Ok(())
        })
        .unwrap();
    assert!(
        child
            .ab
            .as_ref()
            .unwrap()
            .checkpoint(&child, "synthetic", "write:m1", &bdb, &sources_b)
            .is_err()
    );
}

#[test]
fn af255_authorization_cannot_expand_slots_or_reset_occupied_count() {
    let identity = json!({"slots":["one"],"ceiling":1});
    let ledger = json!({"identity":identity,"slots":["one"],"ceiling":1,"attempts":{"one":{"request":"fp","state":"started"}},"order":["one"]});
    validate_campaign(&ledger, &identity).unwrap();
    for field in ["slots", "ceiling", "order", "attempts"] {
        let mut bad = ledger.clone();
        bad[field] = match field {
            "slots" => json!(["one", "two"]),
            "ceiling" => json!(2),
            "order" => json!([]),
            _ => json!({}),
        };
        assert!(validate_campaign(&bad, &identity).is_err());
    }
}

#[test]
fn af255_model_visible_identifiers_do_not_reveal_case_or_question_labels() {
    let id = opaque_id(&json!(["recall", "unresolved_conflict", "conflict_q2"]));
    assert!(!id.contains("conflict"));
    assert!(!id.contains("recall"));
    assert_eq!(
        id,
        opaque_id(&json!(["recall", "unresolved_conflict", "conflict_q2"]))
    );
    assert_ne!(
        id,
        opaque_id(&json!(["turn", "unresolved_conflict", "conflict_q2"]))
    );
}
