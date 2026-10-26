//! Four independently bounded W4/R4 slots. R4 uses the actual W4-applied SQLite snapshot.
use super::*;
use hippocampus::EntityRepository;
use hippocampus::contract::*;
use hippocampus::{
    EnrichmentTask, Entity, EntityType, MemoryId, SourceRecord, W5Claim, W5Outcome, W5TaskEnd,
    commit_group, r1, recall, w5_finish_task,
};
use serde_json::Value;
use std::collections::BTreeMap;

struct Db(PathBuf);
impl Db {
    fn new() -> Self {
        Self(
            std::env::temp_dir()
                .join(format!(
                    "af206-point-{}",
                    hippocampus::diagnostics::new_id()
                ))
                .join("memory.sqlite3"),
        )
    }
    fn open(&self) -> SqliteMemoryStore {
        SqliteMemoryStore::open(&self.0, UserId::from("eval")).unwrap()
    }
}
impl Drop for Db {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.0.parent().unwrap());
    }
}
const OLD: &str = "我的收纳盒是蓝色的，壁灯是白色的。";
const NEW: &str = "我改用银色收纳盒，壁灯仍用原来的白色。";
fn src(db: &Db, id: &str, text: &str, sequence: u64) {
    db.open()
        .transaction(|s| {
            s.entities.upsert(Entity {
                id: "self".into(),
                owner_id: "eval".into(),
                entity_type: EntityType::Person,
                canonical_name: "用户本人".into(),
                aliases: vec!["我".into()],
            });
            s.sources.insert(
                id.into(),
                SourceRecord {
                    management: Default::default(),
                    id: id.into(),
                    session_id: "af206-point".into(),
                    turn_id: id.into(),
                    observed_at: 1791561600 + sequence as i64,
                    received_sequence: sequence,
                    request: Some(json!({"text":text,"history":if sequence==2{vec![OLD]}else{vec![]},"capture_source":true})),
                    enrichment: "running".into(),
                    error: None,
                    result: None,
                    base_revision: s.revision,
                    index: Value::Null,
                    task: EnrichmentTask {
                        attempt_id: Some(format!("claim-{id}")),
                        lease_until: Some(4_000_000_000),
                        claim_revision: Some(s.revision),
                        ..Default::default()
                    },
                },
            );
            Ok(())
        })
        .unwrap();
}
fn candidate(id: &str, text: &str, fragment: &str, index: usize) -> W4Candidate {
    let scene = scenes().into_iter().find(|s| s["id"] == "f06").unwrap();
    let mut c: Candidate =
        serde_json::from_value(adapter::candidate(&scene["input"]["candidates"][index])).unwrap();
    c.sentence.text = text.into();
    c.value = Some(hippocampus::contract::Value::Text {
        text: fragment.into(),
        quote: Quote {
            text: fragment.into(),
        },
    });
    W4Candidate {
        index,
        committed_id: None,
        candidate: c,
        source: SourceRef::Stored {
            source_id: id.into(),
        },
        said_on: Some("2026-10-10".into()),
        time_resolutions: vec![],
    }
}
fn apply(db: &Db, key: &str, input: &W4Input, output: &W4Output) -> Result<Vec<MemoryId>, String> {
    if key == "new" {
        let mut store = db.open();
        store
            .transaction(|s| {
                let source = s.sources.get_mut(key).unwrap();
                source.task.attempt_id = None;
                source.task.lease_until = None;
                source.enrichment = "queued".into();
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        hippocampus::w4_start_round(&mut store, key, "point-write", false)
            .map_err(|e| e.to_string())?;
        let batch = w2::Batch {
            candidates: input
                .candidates
                .iter()
                .map(|c| w2::CandidateResult {
                    index: c.index,
                    candidate: c.candidate.clone(),
                    time_resolutions: c.time_resolutions.clone(),
                    status: w2::Status::Ok,
                    reasons: vec![],
                })
                .collect(),
            index_map: vec![Some(0), Some(1)],
            memory_requests: vec![],
            task_id: None,
            said_on: Some("2026-10-10".into()),
            utc_offset_seconds: 28800,
        };
        let review = W3Output {
            reviews: input
                .candidates
                .iter()
                .map(|c| CandidateReview {
                    candidate: c.index,
                    verdict: Verdict::Supported,
                    problems: vec![],
                    episode_value: EpisodeValue::NotApplicable,
                })
                .collect(),
            request_reviews: vec![],
            coverage: W3Coverage {
                status: CoverageStatus::Complete,
                missing: vec![],
            },
        };
        let receipt = hippocampus::enrichment::run_source(
            &mut store,
            key,
            RuntimeInputBudget::default(),
            |ctx, source| {
                Ok(UpstreamBatch {
                    w2: batch.clone(),
                    review: Some(review.clone().validate(
                        &batch,
                        &ctx.source_binding,
                        &source.text,
                    )?),
                })
            },
            |_, actual, _| {
                // Same stage identities: do not reassign a model's target/candidate indices.
                if actual != input {
                    return Err("point W4 input changed".into());
                }
                Ok(Some(output.clone()))
            },
        )
        .map_err(|e| e.to_string())?;
        if matches!(
            receipt["enrichment"]["status"].as_str(),
            Some("failed" | "needs_rejudge")
        ) {
            return Err(format!("point commit failed: {}", receipt["enrichment"]));
        }
        return Ok(items(&receipt["enrichment"]["candidates"])
            .iter()
            .filter_map(|c| c["memory_id"].as_u64().map(MemoryId))
            .collect());
    }
    let state = db.open().read().unwrap();
    let p = w4::plan(
        &w4::W4Runtime {
            state: &state,
            owner: "eval".into(),
            source_key: key,
            round: 0,
            task_id: None,
            admissions: BTreeMap::new(),
        },
        input,
        output,
    )
    .map_err(|e| e.to_string())?;
    let claim = W5Claim {
        source_key: key.into(),
        attempt_id: format!("claim-{key}"),
    };
    let mut ids = vec![];
    for g in &p.groups {
        match commit_group(&mut db.open(), &claim, g).map_err(|e| e.to_string())? {
            W5Outcome::Committed(receipt) => ids.extend(receipt.products.values().copied()),
            other => return Err(format!("{other:?}")),
        }
    }
    w5_finish_task(&mut db.open(), &claim, &W5TaskEnd::Committed).map_err(|e| e.to_string())?;
    Ok(ids)
}
fn prepared() -> (Db, W4Input) {
    let db = Db::new();
    src(&db, "old", OLD, 1);
    let old = W4Input {
        candidates: vec![
            candidate("old", OLD, "我的收纳盒是蓝色的", 0),
            candidate("old", OLD, "壁灯是白色的", 1),
        ],
        records: vec![],
        relations: vec![],
        coverage: W4Coverage {
            complete: true,
            omitted: 0,
        },
    };
    let output:W4Output=serde_json::from_value(json!({"decisions":(0..2).map(|i|json!({"candidate":i,"status":"decided","action":"new","targets":[],"resolves_relations":[],"basis":[{"source":{"kind":"stored","source_id":"old"},"quote":{"text":OLD}}],"effective":null,"relation_scope":null,"pending_reason":null})).collect::<Vec<_>>()})).unwrap();
    let ids = apply(&db, "old", &old, &output).unwrap();
    src(&db, "new", NEW, 2);
    let state = db.open().read().unwrap();
    let input = W4Input {
        candidates: vec![
            candidate("new", NEW, "我改用银色收纳盒", 0),
            candidate("new", NEW, "壁灯仍用原来的白色", 1),
        ],
        records: ids
            .iter()
            .map(|id| w4::record_view(&state, *id).unwrap())
            .collect(),
        relations: vec![],
        coverage: W4Coverage {
            complete: true,
            omitted: 0,
        },
    };
    (db, input)
}
fn w4_case(root: &Path, model: &str, base: &str, repeat: usize) -> Result<Case, String> {
    let (_, input) = prepared();
    let sample = json!({"id":"p01","af206_point":true,"stage":"w4","input":input,"sources":{"old":{"text":OLD,"history":[]},"new":{"text":NEW,"history":[OLD]}},
        "expect":{"decisions":[{"candidate":0,"status":"decided","action":"state_change","targets":[input.records[0].id()],"pending_reason":null,"basis_sources":["old","new"],"resolves_relations":[]},
            {"candidate":1,"status":"decided","action":"reinforce","targets":[input.records[1].id()],"pending_reason":null,"basis_sources":["old","new"],"resolves_relations":[]}]}});
    let mut request = stage_request_with_output_tokens("w4", &sample, 16384)?;
    request["model"] = json!(model);
    ReasoningEffort::Medium.apply(&mut request);
    af198::case(
        root,
        "w4",
        sample,
        &EvalModel {
            label: "af206-pro-points".into(),
            model: Some(model.into()),
            base: Some(base.into()),
            effort: ReasoningEffort::Medium,
        },
        repeat,
        request,
    )
}
fn r4_case(root: &Path, w4case: &Case) -> Result<Option<Case>, String> {
    let Replay::Output(text) = &w4case.replay else {
        return Ok(None);
    };
    let (db, input) = prepared();
    let proposed: W4Output = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let final_output = w4::prepare_output(&input, &proposed, &|id| match id {
        "old" => Some(OLD.into()),
        "new" => Some(NEW.into()),
        _ => None,
    })
    .map_err(|e| e.to_string())?;
    apply(&db, "new", &input, &final_output)?;
    let request = recall::Request {
        binding: RecallRequest {
            request_id: "point-read".into(),
            query: "我的收纳盒和壁灯现在各是什么颜色？".into(),
            context: vec![],
            task_id: None,
            at: 1791561700,
        },
        owner: "eval".into(),
        subject_ids: None,
        said_on: "2026-10-10".into(),
        utc_offset_seconds: 28800,
        bootstrap: false,
    };
    let ids = ["self".into()];
    let rel = [w2::Relation {
        id: "fact".into(),
        memory_types: vec![MemoryType::Fact],
    }];
    let ctx = r1::Context {
        query: &request.binding.query,
        context: &[],
        entities: &ids,
        relations: &rel,
        subject_ids: None,
        task_id: None,
        said_on: &request.said_on,
        utc_offset_seconds: 28800,
        supported_units: hippocampus::recall_models::SUPPORTED_UNITS,
    };
    let plan=r1::validate(r1::decode(&json!({"need":"judge","current_evidence":"query_only","type":"open","subject":{"entity_id":"self"},"relation":"颜色","conditions":[],"time":null,"plan":{"op":"list","filter":{"relation":"fact","time":null},"unit":null}}).to_string()).unwrap(),&ctx).unwrap();
    let assembly = recall::assemble(&db.open(), &request, &plan, recall::Budget::default())
        .map_err(|e| e.to_string())?;
    let sample = json!({"id":"p01","fixed_input":true,"af206_point":true,"input":{"request":request.binding,"plan":plan.output,"evidence":assembly.evidence,"utc_offset_seconds":28800},
        "source_texts":{"old":OLD,"new":NEW},"expect":{},"origin":"AF206 fictional actual W4 commit"});
    let mut req = fixed_adapter::request("r4", &sample, 4096)?;
    req["model"] = w4case.request["model"].clone();
    ReasoningEffort::Minimal.apply(&mut req);
    let model = EvalModel {
        effort: ReasoningEffort::Minimal,
        ..w4case.model.clone()
    };
    let case = af198::case(root, "r4", sample, &model, w4case.repeat, req)?;
    let binding = json!({"upstream_request":case_fingerprint(w4case),"upstream_output":hippocampus::recall_models::fingerprint(&json!(text)),"request":case_fingerprint(&case)});
    let path = case.campaign.as_ref().unwrap().join("dependency.json");
    if path.exists()
        && serde_json::from_slice::<Value>(&std::fs::read(&path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?
            != binding
    {
        return Err("AF206 points dependency changed".into());
    }
    Ok(Some(case))
}
fn occupied(root: &Path) -> Result<usize, String> {
    count_sends((1..=2).flat_map(|r| {
        ["w4", "r4"].map(move |s| root.join(format!("campaign/slots/repeat-{r}-{s}-p01/sends")))
    }))
}
fn manifest(cases: &[Case]) -> Value {
    json!({"trial":"AF206-points","limit":4,"slots":cases.iter().flat_map(|c|[json!({"slot":campaign_slot(c)}),json!({"stage":"r4","repeat":c.repeat,"upstream_request":case_fingerprint(c),"recipe":"actual-W4-prepare-validate-plan-W5-commit-R3.v1","prompt":hippocampus::recall_models::R4_PROMPT,"schema":hippocampus::recall_models::r4_schema(),"effort":"minimal","max_output_tokens":4096})]).collect::<Vec<_>>()})
}
pub(super) fn run() -> Result<(), String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/eval-fact-af206/points");
    let model =
        std::env::var("DEEPSEEK_PRO_MODEL").unwrap_or_else(|_| "ep-af206-unresolved-pro".into());
    let base = std::env::var("HUOSHAN_DEEPSEEK_BASE_URL")
        .unwrap_or_else(|_| "https://af206.invalid/v1".into());
    let cases = (1..=2)
        .map(|r| w4_case(&root, &model, &base, r))
        .collect::<Result<Vec<_>, _>>()?;
    let limit = std::env::var("EVAL_RECORD")
        .ok()
        .map(|v| v.parse::<usize>().map_err(|e| e.to_string()))
        .transpose()?;
    if limit.is_some_and(|n| !matches!(n, 0 | 4)) {
        return Err("AF206 points freezes 4 slots".into());
    }
    let auth = manifest(&cases);
    let authpath = root.join("campaign/authorization.json");
    if authpath.exists()
        && serde_json::from_slice::<Value>(&std::fs::read(&authpath).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?
            != auth
    {
        return Err("AF206 points binding changed".into());
    }
    if occupied(&root)? > 4 {
        return Err("AF206 points allowance exceeded".into());
    }
    for c in &cases {
        campaign_check(std::slice::from_ref(c), 1)?;
        if matches!(c.replay, Replay::Stale | Replay::Rejected(_)) {
            return Err("AF206 points stale/rejected".into());
        }
    }
    if std::env::var("EVAL_DRY_RUN").as_deref() == Ok("1") {
        let missing = cases
            .iter()
            .filter(|c| matches!(c.replay, Replay::Missing))
            .count();
        let mut ready = 0;
        let mut blocked = 0;
        for c in &cases {
            match r4_case(&root, c) {
                Ok(Some(r)) if matches!(r.replay, Replay::Missing) => ready += 1,
                Err(_) => blocked += 1,
                _ => (),
            }
        }
        println!(
            "AF206 points：一项更新、兄弟项保持；2 次重复 × W4→真实 W5 提交→R3→R4，独立上限 4。\nW4 待录制 {missing}；R4 已确定待录制 {ready}，待该次 W4 提交 {}，上游拒收 {blocked}。\nDeepSeek Pro；W4 medium/16384，R4 minimal/4096；超时 360 秒；R1 注入，不调用模型。\n累计已发送/在途 {}；新增上限 {}；输出容量 40960 token；正文请求输入仍随 W4 实际结果确定。\n严格指纹、依赖绑定、发送前落盘、不重试、在途不重发；本次真实调用 0。",
            missing,
            occupied(&root)?,
            missing * 2 + ready
        );
        return Ok(());
    }
    let live = if limit == Some(4) {
        if std::env::var("HIPPOCAMPUS_MODEL_NETWORK").as_deref() != Ok("live")
            || model == "ep-af206-unresolved-pro"
            || base == "https://af206.invalid/v1"
        {
            return Err("AF206 points requires real model/base and live gate".into());
        }
        let key = std::env::var("HUOSHAN_DEEPSEEK_API_KEY")
            .map_err(|_| "missing HUOSHAN_DEEPSEEK_API_KEY")?;
        Some((
            reqwest::blocking::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .build()
                .map_err(|e| e.to_string())?,
            key,
        ))
    } else {
        None
    };
    let _lock = if live.is_some() {
        Some(w3_trial_lock(&root.join("campaign"))?)
    } else {
        None
    };
    if live.is_some() && !authpath.exists() {
        m15_write_json(&authpath, &auth)?;
    }
    let mut send = |c: &Case| {
        let (http, key) = live.as_ref().ok_or("offline points cannot send")?;
        let response = http
            .execute(smoke_http_request(http, c, key)?)
            .map_err(|e| e.to_string())?;
        model_provider::read_http_response(response, ProviderProtocol::OpenaiResponses)
            .map(|(_, e)| e)
            .map_err(|e| e.to_string())
    };
    let mut results = vec![];
    let mut sent = 0;
    for c in cases {
        let report = evaluate_cases(
            std::slice::from_ref(&c),
            limit.map(|n| usize::from(n > 0)),
            &mut send,
        )?;
        sent += report["sent_calls"].as_u64().unwrap();
        results.push(report["results"][0].clone());
        let actual = w4_case(&root, &model, &base, c.repeat)?;
        match r4_case(&root,&actual){
            Ok(Some(r))=>{if live.is_some(){let Replay::Output(text)=&actual.replay else{unreachable!()};let dependency=json!({"upstream_request":case_fingerprint(&actual),"upstream_output":hippocampus::recall_models::fingerprint(&json!(text)),"request":case_fingerprint(&r)});let p=r.campaign.as_ref().unwrap().join("dependency.json");if !p.exists(){m15_write_json(&p,&dependency)?;}}
                let report=evaluate_cases(&[r],limit.map(|n|usize::from(n>0)),&mut send)?;sent+=report["sent_calls"].as_u64().unwrap();results.push(report["results"][0].clone());},
            other=>results.push(json!({"stage":"r4","repeat":c.repeat,"status":"上游阻断（未发送）","detail":other.err()})),
        }
    }
    let report = json!({"trial":"AF206-points","sent_calls":sent,"cumulative_sent_or_inflight":occupied(&root)?,"authorized_calls":4,"results":results});
    m15_write_json(&root.join("report.json"), &report)?;
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn af206_points_strict_deterministic_requests_and_sibling_commit() {
        let temp = TempRecords::new();
        let a = w4_case(&temp.0, "ep-fixture", "https://af206.invalid/v1", 1).unwrap();
        let b = w4_case(&temp.0, "ep-fixture", "https://af206.invalid/v1", 1).unwrap();
        assert_eq!(case_fingerprint(&a), case_fingerprint(&b));
        assert_eq!(
            a.sample["input"]["records"][0]["candidate"]["value"]["text"],
            "我的收纳盒是蓝色的"
        );
        assert_eq!(
            a.sample["input"]["records"][1]["candidate"]["value"]["text"],
            "壁灯是白色的"
        );
        assert!(!temp.0.exists());
        let mut output = json!({"decisions":a.sample["expect"]["decisions"]});
        for d in output["decisions"].as_array_mut().unwrap() {
            let obj = d.as_object_mut().unwrap();
            obj.remove("basis_sources");
            obj.insert("basis".into(),json!([{"source":{"kind":"stored","source_id":"old"},"quote":{"text":OLD}},{"source":{"kind":"stored","source_id":"new"},"quote":{"text":NEW}}]));
            obj.insert("effective".into(), Value::Null);
            obj.insert("relation_scope".into(), Value::Null);
        }
        save_recording(
            &a,
            &fake_envelope(output.clone()),
            Some(&output.to_string()),
            None,
        )
        .unwrap();
        let actual = w4_case(&temp.0, "ep-fixture", "https://af206.invalid/v1", 1).unwrap();
        let r = r4_case(&temp.0, &actual).unwrap().unwrap();
        assert_eq!(r.request["text"]["format"]["name"], "r4_v7");
        let input: Value = serde_json::from_str(r.request["input"].as_str().unwrap()).unwrap();
        assert_eq!(items(&input["evidence"]["records"]).len(), 3);
        let old_id = a.sample["input"]["records"][0]["id"].clone();
        let sibling = a.sample["input"]["records"][1]["id"].clone();
        assert!(
            items(&input["evidence"]["records"])
                .iter()
                .any(|v| v["id"] == old_id && v["stored_status"] == "superseded")
        );
        assert!(
            items(&input["evidence"]["records"])
                .iter()
                .any(|v| v["id"] == sibling && v["stored_status"] == "active")
        );
        assert_eq!(
            case_fingerprint(&r),
            case_fingerprint(&r4_case(&temp.0, &actual).unwrap().unwrap())
        );
    }
}
