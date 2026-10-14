//! Background dispatch with real SQLite, injected physical sends and event barriers; no ports.
use super::*;
use crate::chat::{ChatConfig, ChatMemoryService};
use crate::contract::*;
use crate::*;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

const OWNER: &str = "background-fixture";
const OLD: &str = "我住在海棠市。";
const NEW: &str = "我已搬到青岚市。";
const AT: i64 = 1_790_899_200;

struct Db(PathBuf);
impl Db {
    fn new() -> Self {
        Self(
            std::env::temp_dir()
                .join(format!("pmr-background-{}", diagnostics::new_id()))
                .join("memory.sqlite3"),
        )
    }
    fn open(&self) -> SqliteMemoryStore {
        SqliteMemoryStore::open_operation(&self.0, OWNER.into()).unwrap()
    }
    fn service(&self, max_calls: u64) -> ChatMemoryService {
        let config: ChatConfig = serde_json::from_value(json!({
            "write_pipeline":"w4_runtime_v1","recall_pipeline":"recall_v3",
        "write_models":{"w1":DEEPSEEK_MODEL,"w3":DEEPSEEK_MODEL,"w4":DEEPSEEK_MODEL,
            "reasoning_effort":{"w1":"low","w3":"medium","w4":"medium"}},
        "recall_models":{"r1":DEEPSEEK_MODEL,"r4":DEEPSEEK_MODEL,
            "reasoning_effort":{"r1":"medium","r4":"medium"}},
            "enrichment_mode":"deferred","background_operation_limit":3,
            "model_budget":{"group_id":"joint-fixture","max_calls":max_calls,"max_operation_calls":2}
        })).unwrap();
        ChatMemoryService::new(
            SqliteMemoryStore::open(&self.0, OWNER.into()).unwrap(),
            config,
        )
        .unwrap()
    }
    fn calls(&self) -> i64 {
        self.open()
            .connection
            .query_row("SELECT count(*) FROM model_calls", [], |r| r.get(0))
            .unwrap()
    }
    fn expire(&self, key: &str) {
        self.open()
            .transaction(|state| {
                state.sources.get_mut(key).unwrap().task.lease_until = Some(0);
                Ok(())
            })
            .unwrap();
    }
}
impl Drop for Db {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.0.parent().unwrap());
    }
}

fn capture(service: &mut ChatMemoryService, db: &Db, turn: &str, text: &str) -> String {
    let result = service
        .call(
            "memory_remember",
            json!({"session_id":"fixture","turn_id":turn,
        "text":text,"observed_at":AT,"capture_source":true}),
            || false,
        )
        .unwrap();
    assert_eq!(result["enrichment"]["status"], "queued");
    db.open()
        .read()
        .unwrap()
        .sources
        .into_iter()
        .find(|(_, s)| s.turn_id == turn)
        .unwrap()
        .0
}
fn w1(text: &str) -> Json {
    json!({"candidates":[{"kind":"assertion","speech":"user_statement","subject":{"entity_id":"self"},
        "relation":"lives_in","memory_type":"fact","value":{"kind":"text","text":text,"quote":{"text":text}},
        "sentence":{"text":text},"polarity":"affirm","strength":null,"uncertainty":null,"conditions":[],"times":[]}],
        "memory_requests":[]})
}
fn output(request: &Json) -> Json {
    let input: Json = serde_json::from_str(request["input"].as_str().unwrap()).unwrap();
    match request["text"]["format"]["name"].as_str().unwrap() {
        "w1_v4" => w1(input["current_user_message"].as_str().unwrap()),
        "w3_v5" => json!({"reviews":input["candidates"].as_array().unwrap().iter().map(|c|
            json!({"candidate":c["index"],"verdict":"supported","problems":[],"episode_value":"not_applicable"})).collect::<Vec<_>>(),
            "coverage":{"status":"complete","missing":[]},"request_reviews":[]}),
        "w4_v5" => {
            let records = input["w4_input"]["records"].as_array().unwrap();
            json!({"decisions": input["w4_input"]["candidates"].as_array().unwrap().iter().map(|c| {
                let mut basis = vec![json!({"source":c["source"],"quote":c["candidate"]["sentence"]})];
                let mut targets = vec![];
                if let Some(old) = records.first() {
                    targets.push(old["id"].clone());
                    basis.extend(old["evidence"].as_array().unwrap().clone());
                }
                json!({"candidate":c["index"],"status":"decided","action":if targets.is_empty(){"new"}else{"state_change"},
                    "targets":targets,"basis":basis,"effective":null,"relation_scope":null,"pending_reason":null,"resolves_relations":[]})
            }).collect::<Vec<_>>()})
        }
        "r1_v2" => {
            json!({"need":"judge","current_evidence":"query_only","type":"single","subject":{"entity_id":"self"},
            "relation":"居住地","conditions":[],"time":null,"plan":{"op":"lookup","filter":{"relation":"lives_in","time":null},"unit":null}})
        }
        "r4_v7" => json!({"verdict":"insufficient","facts":[],"compute":null}),
        stage => panic!("unexpected stage {stage}"),
    }
}
fn envelope(output: Json) -> (u16, String) {
    (
        200,
        json!({"status":"completed","usage":{"input_tokens":10,"output_tokens":10},
        "output":[{"type":"message","content":[{"type":"output_text","text":output.to_string()}]}]})
        .to_string(),
    )
}
type Sent = Arc<Mutex<Vec<Json>>>;
fn client() -> (OpenAiClient, Sent) {
    let mut client = OpenAiClient::new("http://127.0.0.1:1/v1", "fixture-key").unwrap();
    client.network_allowed = true;
    let sent = Arc::new(Mutex::new(vec![]));
    let requests = Arc::clone(&sent);
    client.transport = Some(Arc::new(move |request| {
        let body: Json =
            serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap();
        requests.lock().unwrap().push(body.clone());
        Ok(envelope(output(&body)))
    }));
    (client, sent)
}
fn hold(client: &mut OpenAiClient, stage: &'static str) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
    let transport = client.transport.take().unwrap();
    let (entered, entry) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let released = Mutex::new(released);
    client.transport = Some(Arc::new(move |request| {
        let body: Json =
            serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap();
        if body["text"]["format"]["name"] == stage {
            entered.send(()).unwrap();
            released
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(20))
                .unwrap();
        }
        transport(request)
    }));
    (entry, release)
}
fn wait(entry: &mpsc::Receiver<()>) {
    entry.recv_timeout(Duration::from_secs(8)).unwrap();
}
fn query(id: &str) -> Json {
    json!({"request_id":id,"query":"我住在哪？","at":AT})
}

#[test]
fn background_candidate_diagnostics_are_durable_and_optional() {
    for mode in ["enabled", "disabled", "failed"] {
        let db = Db::new();
        let trace = db.0.parent().unwrap().join("trace");
        std::fs::create_dir_all(&trace).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&trace, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        // Separate processes isolate the opt-in writer's OnceLock and environment.
        let child = format!(
            "{}::background_trace_child",
            module_path!().split_once("::").unwrap().1
        );
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", &child, "--nocapture"])
            .env("PMR_BACKGROUND_TRACE_TEST", mode)
            .env_remove("PMR_TRACE_DIR");
        match mode {
            "enabled" => {
                command.env("PMR_TRACE_DIR", &trace);
            }
            "failed" => {
                command.env("PMR_TRACE_DIR", trace.join("missing-directory"));
            }
            _ => (),
        }
        let result = command.output().unwrap();
        assert!(
            result.status.success(),
            "{mode}: {}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(String::from_utf8_lossy(&result.stdout).contains("1 passed"));
        let files: Vec<_> = std::fs::read_dir(&trace)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        if mode != "enabled" {
            assert!(files.is_empty());
            if mode == "failed" {
                assert!(String::from_utf8_lossy(&result.stderr).contains("evidence_incomplete"));
            }
            continue;
        }
        assert_eq!(files.len(), 1);
        let rows: Vec<Json> = std::fs::read_to_string(&files[0])
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let checks: Vec<_> = rows.iter().filter(|r| r["kind"] == "model_check").collect();
        let rejected = checks.iter().find(|r| r["data"]["stage"] == "W2").unwrap();
        assert_eq!(rejected["data"]["candidate_index"], 0);
        assert_eq!(
            rejected["data"]["reasons"],
            json!(["candidate_rejected:fact_sentence_mismatch"])
        );
        assert!(checks.iter().any(|r| r["data"]["stage"] == "W1"
            && r["data"]["phase"] == "validate"
            && r["data"]["status"] == "completed"));
        assert!(
            checks
                .iter()
                .any(|r| r["data"]["stage"] == "W3" && r["data"]["status"] == "completed")
        );
        for row in checks {
            assert!(!row.to_string().contains(OLD));
            assert!(row["data"].get("candidate").is_none());
            assert!(
                row["run_id"]
                    .as_str()
                    .is_some_and(|id| id.starts_with("w4:"))
            );
        }
    }
}

#[test]
fn background_trace_child() {
    if std::env::var_os("PMR_BACKGROUND_TRACE_TEST").is_none() {
        return;
    }
    let db = Db::new();
    let mut service = db.service(5);
    const OTHER_SENTENCE: &str = "我收藏木牌。";
    let key = capture(
        &mut service,
        &db,
        "diagnostics",
        &format!("{OLD}{OTHER_SENTENCE}"),
    );
    let (mut c, sent) = client();
    let original = c.transport.take().unwrap();
    c.transport = Some(Arc::new(move |request| {
        let body: Json =
            serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap();
        if body["text"]["format"]["name"] != "w1_v4" {
            return original(request);
        }
        let mut extraction = w1(OLD);
        extraction["candidates"][0]["relation"] = json!("fact");
        let valid = extraction["candidates"][0].clone();
        // The quote exists in the source but lies outside this candidate's full sentence.
        extraction["candidates"][0]["value"]["text"] = json!(OTHER_SENTENCE);
        extraction["candidates"][0]["value"]["quote"]["text"] = json!(OTHER_SENTENCE);
        extraction["candidates"].as_array_mut().unwrap().push(valid);
        Ok(envelope(extraction))
    }));
    service.inject_model_client(c);
    let worker = service.start_background().unwrap().unwrap();
    worker.wait_idle();
    worker.stop_and_join().unwrap();
    // Identical business assertions run with diagnostics enabled, disabled and failing.
    assert_eq!(db.calls(), 2);
    assert_eq!(sent.lock().unwrap().len(), 1); // W1 is intercepted above; W3 uses the original transport.
    let state = db.open().read().unwrap();
    let result = crate::enrichment::receipt(&db.open(), &key).unwrap();
    assert_eq!(state.sources[&key].enrichment, "pending");
    assert_eq!(result["enrichment"]["candidates"][0]["status"], "rejected");
    assert_eq!(result["enrichment"]["candidates"][1]["status"], "committed");
}

#[test]
fn remember_publication_preserves_committed_result_when_snapshot_is_unreadable() {
    for recall_pipeline in [crate::chat::RecallPipeline::RecallV3] {
        let db = Db::new();
        // T1 publication survives corrupt post-commit snapshots in the recall_v3 dispatch.
        let config = ChatConfig {
            recall_pipeline,
            model_budget: Some(
                serde_json::from_value(json!({"group_id":"global-context-fixture","max_calls":4}))
                    .unwrap(),
            ),
            ..Default::default()
        };
        let service = ChatMemoryService::new(
            SqliteMemoryStore::open(&db.0, OWNER.into()).unwrap(),
            config,
        )
        .unwrap();
        let shared = Mutex::new(service);
        let remember = |session: &str| {
            let arguments = json!({"text":"以后无论聊什么，回答都必须使用中文。",
                "session_id":session,"turn_id":"one","capture_source":true});
            let response = ChatMemoryService::call_shared(
                &shared,
                "memory_remember",
                arguments.clone(),
                || false,
                &diagnostics::new_id(),
            )
            .unwrap();
            (arguments, response)
        };
        let (_, first) = remember("direct-save");
        assert_ne!(first.is_error, Some(true));
        assert_eq!(
            first.structured_content.unwrap()["global_context"]["status"],
            "available"
        );
        let recall_arguments = json!({"recall_version":"recall.v11","request_id":"retained",
            "query":"全局回答要求","offline":true,"bootstrap":true});
        let recalled = ChatMemoryService::call_shared(
            &shared,
            "memory_recall",
            recall_arguments.clone(),
            || false,
            &diagnostics::new_id(),
        )
        .unwrap();
        assert_ne!(recalled.is_error, Some(true));
        db.open()
            .connection
            .execute_batch(
                "CREATE TRIGGER break_context AFTER UPDATE ON memory_profiles BEGIN
             UPDATE memory_profiles SET state_json='invalid-json' WHERE owner_id=NEW.owner_id; END",
            )
            .unwrap();
        let (arguments, committed) = remember("context-failure");
        assert_ne!(committed.is_error, Some(true));
        let body = committed.structured_content.as_ref().unwrap();
        assert_eq!(body["source"]["status"], "stored");
        assert_eq!(body["enrichment"]["status"], "queued");
        assert_eq!(body["global_context"]["status"], "unavailable");
        assert!(shared.lock().unwrap().operation().connect().is_err());
        let published = ChatMemoryService::publish_shared_response(
            &shared,
            "memory_remember",
            &arguments,
            committed.clone(),
            |value| value,
        )
        .unwrap();
        assert_eq!(published, committed);
        assert!(
            ChatMemoryService::publish_shared_response(
                &shared,
                "memory_recall",
                &recall_arguments,
                recalled,
                |_| panic!("unreadable storage must block recall publication"),
            )
            .is_err()
        );
        // Inspect SQL directly: an operation connection intentionally rejects the corrupt profile.
        let connection = rusqlite::Connection::open(&db.0).unwrap();
        let calls: i64 = connection
            .query_row("SELECT count(*) FROM model_calls", [], |r| r.get(0))
            .unwrap();
        assert_eq!(calls, 0);
        let state: String = connection
            .query_row(
                "SELECT state_json FROM memory_profiles WHERE owner_id=?1",
                [OWNER],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(state, "invalid-json");
    }
}

#[test]
fn background_config_keeps_default_inert_and_joint_allowance_explicit() {
    let db = Db::new();
    let mut legacy = ChatMemoryService::new(
        SqliteMemoryStore::open(&db.0, OWNER.into()).unwrap(),
        ChatConfig::default(),
    )
    .unwrap();
    assert!(legacy.start_background().unwrap().is_none());
    for value in [
        json!({"background_operation_limit":3}),
        json!({"background_operation_limit":4,
        "model_budget":{"group_id":"test","max_calls":5}}),
    ] {
        assert!(
            serde_json::from_value::<ChatConfig>(value)
                .unwrap()
                .validate()
                .is_err()
        );
    }
    let example: ChatConfig =
        serde_json::from_str(include_str!("../examples/background-recall.json")).unwrap();
    example.validate().unwrap();
    assert_eq!(example.background_operation_limit, Some(3));
    assert_eq!(example.model_budget.unwrap().max_operation_calls, 2);
}

#[test]
fn background_startup_recovers_queued_task_and_preserves_receiving_date() {
    let db = Db::new();
    let mut first = db.service(5);
    let key = capture(&mut first, &db, "queued", OLD);
    drop(first); // new operation/service has no in-memory notification for this task.
    let (mut c, sent) = client();
    let (entered, release) = hold(&mut c, "w3_v5");
    let mut service = db.service(5);
    service.inject_model_client(c);
    let worker = service.start_background().unwrap().unwrap();
    assert!(service.start_background().is_err());
    wait(&entered);
    assert_eq!(
        db.open().read().unwrap().sources[&key].enrichment,
        "running"
    );
    let input: Json =
        serde_json::from_str(sent.lock().unwrap()[0]["input"].as_str().unwrap()).unwrap();
    assert_eq!(input["said_on"], "2026-10-02");
    release.send(()).unwrap();
    // The model gate releases before join, and the source may finish while shutdown is requested.
    worker.stop_and_join().unwrap();
    assert_eq!(
        db.open().read().unwrap().sources[&key].enrichment,
        "committed"
    );
    assert_eq!(db.calls(), 2);
    assert!(!service.run_background_once().unwrap());
}

fn claimed_upstream(
    db: &Db,
    service: &mut ChatMemoryService,
    key: &str,
    c: OpenAiClient,
) -> W4Claim {
    w4_start_round(&mut db.open(), key, "saved-stages", false).unwrap();
    let claim = w4_claim_round(&mut db.open(), key).unwrap();
    let state = db.open().read().unwrap();
    let source = &state.sources[key];
    let config = ChatConfig::default();
    let context = OpenAiAdapterContext::new(
        OWNER.into(),
        state.entities.list_by_owner(&OWNER.into()),
        Timestamp(AT),
    )
    .unwrap()
    .with_utc_offset_seconds(config.utc_offset_seconds)
    .unwrap()
    .with_semantic_catalog(config.predicates, vec![])
    .unwrap()
    .with_predicate_value_kinds(config.value_kinds)
    .unwrap()
    .with_predicate_memory_types(config.memory_types)
    .unwrap();
    let provider = WriteModelProvider::new(
        c.with_budget(
            &db.0,
            OWNER,
            serde_json::from_value(json!({
        "group_id":"joint-fixture","max_calls":5,"max_operation_calls":2}))
            .unwrap(),
            "placeholder",
        )
        .unwrap()
        .with_background_operation_limit(3)
        .unwrap(),
        context,
    );
    let ctx = ProviderContext {
        owner: OWNER.into(),
        source_key: key.into(),
        source_id: source.id.clone(),
        source_binding: claim.source_binding().clone(),
        observed_at: AT,
        received_sequence: source.received_sequence,
        task_id: None,
        round: 0,
        operation_id: "saved-stages".into(),
        previous_w2: None,
    };
    let input = SourceInput::of(source).unwrap();
    let _ = provider.provide(&mut db.open(), &claim, &ctx, &input);
    service.inject_model_client(provider.client.clone());
    claim
}

#[test]
fn background_reuses_saved_model_stages_after_lease_transfer_without_sends() {
    let db = Db::new();
    let mut service = db.service(5);
    let key = capture(&mut service, &db, "stages", OLD);
    let (c, sent) = client();
    let old = claimed_upstream(&db, &mut service, &key, c);
    assert_eq!(sent.lock().unwrap().len(), 2);
    assert!(!service.run_background_once().unwrap());
    db.expire(&key);
    assert!(service.run_background_once().unwrap());
    assert_eq!(
        db.open().read().unwrap().sources[&key].enrichment,
        "committed"
    );
    assert_eq!(sent.lock().unwrap().len(), 2);
    assert_eq!(db.calls(), 2);
    assert!(
        w4_renew_claim(&mut db.open(), &old)
            .unwrap_err()
            .to_string()
            .starts_with("claim_lost")
    );
}

#[test]
fn background_reserved_missing_result_fails_once_without_resending() {
    let db = Db::new();
    let mut service = db.service(5);
    let key = capture(&mut service, &db, "missing", OLD);
    let (mut c, sent) = client();
    let transport = c.transport.take().unwrap();
    c.transport = Some(Arc::new(move |request| {
        let body: Json =
            serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap();
        if body["text"]["format"]["name"] == "w3_v5" {
            return Err(OpenAiAdapterError::InvalidResponse(
                "synthetic interrupted response".into(),
            ));
        }
        transport(request)
    }));
    claimed_upstream(&db, &mut service, &key, c);
    assert_eq!(db.calls(), 2);
    db.expire(&key);
    assert!(service.run_background_once().unwrap());
    let source = &db.open().read().unwrap().sources[&key];
    assert_eq!(source.enrichment, "failed");
    assert!(
        source
            .result
            .as_ref()
            .unwrap()
            .to_string()
            .contains("model_result_missing")
    );
    for _ in 0..3 {
        assert!(!service.run_background_once().unwrap());
    }
    assert_eq!(db.calls(), 2);
    assert_eq!(sent.lock().unwrap().len(), 1);
}

fn planned(db: &Db, service: &mut ChatMemoryService, turn: &str) -> (String, W4Claim, w4::Plan) {
    let text = "喜欢豆浆。喜欢茉莉茶。";
    let key = capture(service, db, turn, text);
    w4_start_round(&mut db.open(), &key, &format!("plan-{turn}"), false).unwrap();
    let claim = w4_claim_round(&mut db.open(), &key).unwrap();
    let source_id = db.open().read().unwrap().sources[&key].id.clone();
    let candidates: Vec<W4Candidate> = ["喜欢豆浆", "喜欢茉莉茶"]
        .iter()
        .enumerate()
        .map(|(index, text)| {
            let mut candidate: Candidate =
                serde_json::from_value(w1(text)["candidates"][0].clone()).unwrap();
            candidate.relation = "fact".into();
            W4Candidate {
                index,
                committed_id: None,
                candidate,
                source: SourceRef::Stored {
                    source_id: source_id.clone(),
                },
                said_on: Some("2026-10-02".into()),
                time_resolutions: vec![],
            }
        })
        .collect();
    let input = W4Input {
        candidates: candidates.clone(),
        records: vec![],
        relations: vec![],
        coverage: W4Coverage {
            complete: false,
            omitted: 0,
        },
    };
    let output = W4Output {
        decisions: candidates
            .iter()
            .map(|c| W4Decision {
                candidate: c.index,
                status: DecisionStatus::Decided,
                action: Some(Action::New),
                targets: vec![],
                resolves_relations: vec![],
                basis: vec![EvidenceRef {
                    source: c.source.clone(),
                    quote: c.candidate.sentence.clone(),
                }],
                effective: None,
                relation_scope: None,
                pending_reason: None,
            })
            .collect(),
    };
    let state = db.open().read().unwrap();
    let plan = w4::plan(
        &w4::W4Runtime {
            state: &state,
            owner: OWNER.into(),
            source_key: &key,
            round: 0,
            task_id: None,
            admissions: Default::default(),
        },
        &input,
        &output,
    )
    .unwrap();
    w4_save_plan(&mut db.open(), &claim, &candidates, &plan).unwrap();
    (key, claim, plan)
}

#[test]
fn background_reopen_partial_plan_has_zero_duplicate_writes_and_calls() {
    let db = Db::new();
    let mut service = db.service(5);
    let (key, old, plan) = planned(&db, &mut service, "partial");
    assert_eq!(plan.groups.len(), 2);
    let outcome = w4_commit_group(&mut db.open(), &old, &plan.groups[0]).unwrap();
    db.expire(&key);
    drop(service);
    let mut restarted = db.service(5); // no model credentials or injection needed for a saved plan.
    assert!(restarted.run_background_once().unwrap());
    let state = db.open().read().unwrap();
    assert_eq!(state.sources[&key].enrichment, "committed");
    assert_eq!(
        state.repository.list_by_owner(&OWNER.into()).unwrap().len(),
        2
    );
    assert_eq!(state.w5_receipts.len(), 2);
    assert_eq!(db.calls(), 0);
    assert_eq!(
        outcome,
        W5Outcome::Committed(state.w5_receipts[&plan.groups[0].id].clone())
    );
    assert!(w4_commit_group(&mut db.open(), &old, &plan.groups[1]).is_err());
    assert!(!restarted.run_background_once().unwrap());
}

#[test]
fn background_repairs_only_summary_after_all_groups_and_newer_source_committed() {
    let db = Db::new();
    let mut service = db.service(5);
    let (key, claim, plan) = planned(&db, &mut service, "summary");
    for group in &plan.groups {
        w4_commit_group(&mut db.open(), &claim, group).unwrap();
    }
    let (_, newer, next_plan) = planned(&db, &mut service, "newer");
    for group in &next_plan.groups {
        w4_commit_group(&mut db.open(), &newer, group).unwrap();
    }
    w4_finish_round(&mut db.open(), &newer).unwrap();
    db.expire(&key);
    let before = serde_json::to_value(db.open().read().unwrap().repository).unwrap();
    assert!(service.run_background_once().unwrap());
    let state = db.open().read().unwrap();
    assert_eq!(state.sources[&key].enrichment, "committed");
    assert_eq!(serde_json::to_value(state.repository).unwrap(), before);
    assert_eq!(db.calls(), 0);
}

#[test]
fn background_skips_terminal_and_off_and_legacy_tasks() {
    let db = Db::new();
    let mut service = db.service(5);
    let key = capture(&mut service, &db, "terminal", OLD);
    for status in [
        "failed",
        "budget_exhausted",
        "needs_rejudge",
        "pending",
        "coverage_gaps",
        "committed",
        "no_candidates_unreviewed",
    ] {
        db.open()
            .transaction(|state| {
                state.sources.get_mut(&key).unwrap().enrichment = status.into();
                Ok(())
            })
            .unwrap();
        assert!(!service.run_background_once().unwrap());
    }
    for (version, mode) in [
        (W4_PROCESSING_VERSION, "off"),
        ("s5_enrichment_v1", "deferred"),
    ] {
        db.open()
            .transaction(|state| {
                let source = state.sources.get_mut(&key).unwrap();
                source.enrichment = "queued".into();
                source.task.processing_version = Some(version.into());
                source.task.accepted_mode = Some(mode.into());
                Ok(())
            })
            .unwrap();
        assert!(!service.run_background_once().unwrap());
    }
}

#[test]
fn background_periodic_scan_reclaims_expired_lease_and_rejects_late_executor() {
    let db = Db::new();
    let mut service = db.service(5);
    let key = capture(&mut service, &db, "lease", OLD);
    w4_start_round(&mut db.open(), &key, "lease", false).unwrap();
    let old = w4_claim_round(&mut db.open(), &key).unwrap();
    let (mut c, _) = client();
    let (entered, release) = hold(&mut c, "w1_v4");
    service.inject_model_client(c);
    assert!(!service.run_background_once().unwrap());
    let worker = service.start_background().unwrap().unwrap();
    worker.wait_idle();
    db.expire(&key); // no notification: the periodic scan must discover this change.
    wait(&entered);
    let source = &db.open().read().unwrap().sources[&key];
    assert_ne!(
        source.task.attempt_id.as_deref(),
        Some(old.claim.attempt_id.as_str())
    );
    assert!(source.task.lease_until.unwrap() > AT);
    assert!(w4_fail_attempt(&mut db.open(), &old, "late").is_err());
    release.send(()).unwrap();
    worker.stop_and_join().unwrap();
    assert_eq!(
        db.open().read().unwrap().sources[&key].enrichment,
        "committed"
    );
}

#[test]
fn background_and_frontend_share_total_budget_without_extra_allowance() {
    let db = Db::new();
    let mut service = db.service(4);
    let (c, sent) = client();
    service.inject_model_client(c);
    capture(&mut service, &db, "first", OLD);
    assert!(service.run_background_once().unwrap());
    let result = service
        .call("memory_recall", query("joint-recall"), || false)
        .unwrap();
    assert_eq!(result["status"], "insufficient");
    let second = capture(&mut service, &db, "second", NEW);
    assert!(service.run_background_once().unwrap());
    assert_eq!(
        db.open().read().unwrap().sources[&second].enrichment,
        "budget_exhausted"
    );
    assert_eq!(db.calls(), 4);
    assert_eq!(sent.lock().unwrap().len(), 4);
    assert!(!service.run_background_once().unwrap());
    let policy: String = db
        .open()
        .connection
        .query_row("SELECT policy_json FROM model_budget_groups", [], |r| {
            r.get(0)
        })
        .unwrap();
    let policy: Json = serde_json::from_str(&policy).unwrap();
    assert_eq!(policy["max_background_operation_calls"], 3);
    assert_eq!(policy["limits"]["max_operation_calls"], 2);
    assert_eq!(policy["limits"]["max_calls"], 4);
}

#[test]
fn background_held_model_allows_online_foreground_recall_and_new_raw_before_release() {
    let db = Db::new();
    let mut service = db.service(10);
    let (c, _) = client();
    service.inject_model_client(c);
    capture(&mut service, &db, "old", OLD);
    service.run_background_once().unwrap();
    let (mut c, sent) = client();
    let (entered, release) = hold(&mut c, "w1_v4");
    service.inject_model_client(c);
    let worker = service.start_background().unwrap().unwrap();
    let shared = Arc::new(Mutex::new(service));
    let saved = ChatMemoryService::call_shared(
        &shared,
        "memory_remember",
        json!({"session_id":"fixture",
        "turn_id":"new","text":NEW,"observed_at":AT,"capture_source":true}),
        || false,
        "capture",
    )
    .unwrap();
    assert!(!saved.is_error.unwrap_or(false));
    wait(&entered);
    std::thread::scope(|scope| {
        let shared = Arc::clone(&shared);
        let (done, finished) = mpsc::channel();
        scope.spawn(move || {
            done.send(ChatMemoryService::call_shared(
                &shared,
                "memory_recall",
                query("held-background"),
                || false,
                "foreground",
            ))
            .unwrap();
        });
        let result = finished
            .recv_timeout(Duration::from_secs(5))
            .expect("foreground waited for background response")
            .unwrap();
        assert!(!result.is_error.unwrap_or(false));
        let recall = &result.structured_content.unwrap()["recall"];
        assert_eq!(recall["status"], "insufficient");
        assert!(
            recall["raw"]
                .as_array()
                .unwrap()
                .iter()
                .any(|raw| raw["quote"]["text"] == NEW && raw["enrichment"] == "running")
        );
        assert!(!recall["records"].as_array().unwrap().is_empty());
        assert_eq!(recall["facts"], json!([]));
        assert_eq!(sent.lock().unwrap().len(), 2); // R1/R4 completed while W1 is still held.
        release.send(()).unwrap();
    });
    worker.stop_and_join().unwrap();
    assert_eq!(
        db.open()
            .read()
            .unwrap()
            .sources
            .values()
            .find(|s| s.turn_id == "new")
            .unwrap()
            .enrichment,
        "committed"
    );
}

#[test]
fn background_commit_during_foreground_r4_preserves_snapshot_in_three_controlled_runs() {
    for repeat in 0..3 {
        let db = Db::new();
        let mut service = db.service(10);
        let (c, _) = client();
        service.inject_model_client(c);
        capture(&mut service, &db, "old", OLD);
        service.run_background_once().unwrap();
        let (mut c, _) = client();
        let (w1_entered, release_w1) = hold(&mut c, "w1_v4");
        let (r4_entered, release_r4) = hold(&mut c, "r4_v7");
        service.inject_model_client(c);
        capture(&mut service, &db, "new", NEW);
        let worker = service.start_background().unwrap().unwrap();
        let shared = Arc::new(Mutex::new(service));
        wait(&w1_entered);
        std::thread::scope(|scope| {
            let shared = Arc::clone(&shared);
            let foreground = scope.spawn(move || {
                ChatMemoryService::call_shared(
                    &shared,
                    "memory_recall",
                    query(&format!("snapshot-{repeat}")),
                    || false,
                    "foreground",
                )
            });
            wait(&r4_entered);
            release_w1.send(()).unwrap();
            worker.stop_and_join().unwrap(); // background has committed before releasing R4.
            assert_eq!(
                db.open()
                    .read()
                    .unwrap()
                    .sources
                    .values()
                    .find(|s| s.turn_id == "new")
                    .unwrap()
                    .enrichment,
                "committed"
            );
            release_r4.send(()).unwrap();
            let result = foreground.join().unwrap().unwrap();
            assert!(!result.is_error.unwrap_or(false));
            let recall = &result.structured_content.as_ref().unwrap()["recall"];
            assert_eq!(recall["version"], "recall.v11");
            assert!(
                recall["snapshot"]["read_revision"].as_u64().unwrap()
                    < db.open().read().unwrap().revision
            );
            assert!(
                recall["raw"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|raw| raw["enrichment"] == "running")
            );
        });
        assert_eq!(db.calls(), 7); // old 2 + foreground 2 + new 3, no automatic repeat.
    }
}

#[test]
fn background_does_not_relax_order_barrier_for_an_older_unsubmitted_source() {
    let db = Db::new();
    let mut service = db.service(5);
    let key = capture(&mut service, &db, "older", OLD);
    db.open()
        .transaction(|state| {
            state.last_applied_source_sequence = state.sources[&key].received_sequence + 1;
            Ok(())
        })
        .unwrap();
    let (c, sent) = client();
    service.inject_model_client(c);
    assert!(service.run_background_once().unwrap());
    assert_eq!(
        db.open().read().unwrap().sources[&key].enrichment,
        "needs_rejudge"
    );
    assert!(!service.run_background_once().unwrap());
    assert_eq!(db.calls(), 0);
    assert!(sent.lock().unwrap().is_empty());
}

#[test]
fn background_invalidated_checkpoint_fails_before_claim_without_retry_loop() {
    let db = Db::new();
    let mut service = db.service(5);
    let key = capture(&mut service, &db, "changed", OLD);
    w4_start_round(&mut db.open(), &key, "changed", false).unwrap();
    db.open()
        .transaction(|state| {
            state
                .sources
                .get_mut(&key)
                .unwrap()
                .request
                .as_mut()
                .unwrap()["text"] = json!(NEW);
            Ok(())
        })
        .unwrap();
    let (c, sent) = client();
    service.inject_model_client(c);
    assert!(service.run_background_once().unwrap());
    assert_eq!(
        db.open().read().unwrap().sources[&key].enrichment,
        "needs_rejudge"
    );
    for _ in 0..3 {
        assert!(!service.run_background_once().unwrap());
    }
    assert!(sent.lock().unwrap().is_empty());
}

#[test]
fn background_late_model_response_cannot_restore_a_deleted_source() {
    let db = Db::new();
    let mut service = db.service(5);
    let key = capture(&mut service, &db, "deleted", OLD);
    let id = db.open().read().unwrap().sources[&key].id.clone();
    let (mut c, _) = client();
    let (entered, release) = hold(&mut c, "w1_v4");
    service.inject_model_client(c);
    let worker = service.start_background().unwrap().unwrap();
    let shared = Mutex::new(service);
    wait(&entered);
    let forgotten = ChatMemoryService::call_shared(
        &shared,
        "memory_forget",
        json!({"source_id":id}),
        || false,
        "forget",
    )
    .unwrap();
    assert_eq!(forgotten.structured_content.unwrap()["removed"], true);
    release.send(()).unwrap();
    worker.stop_and_join().unwrap();
    let state = db.open().read().unwrap();
    assert!(state.sources[&key].request.is_none());
    assert_eq!(state.sources[&key].enrichment, "deleted");
    assert!(!serde_json::to_string(&state).unwrap().contains(OLD));
    assert!(
        state
            .repository
            .list_by_owner(&OWNER.into())
            .unwrap()
            .is_empty()
    );
    assert_eq!(db.calls(), 1);
}

#[path = "chat_visibility_tests.rs"]
mod visibility;
