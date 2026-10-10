//! Eight fixed-input point slots; old records/R1 are injected, actual W4 commits feed R4.
use super::*;
use hippocampus::contract::Polarity as CPolarity;
use hippocampus::{r1, recall};
use storage::*;

fn prepared(id: &str) -> (Db, W4Input) {
    let scene = scenes().into_iter().find(|s| s["id"] == id).unwrap();
    let old = scene["input"]["history"][0].as_str().unwrap();
    let c = &scene["input"]["candidates"][0];
    let db = Db::new();
    seed(
        &db,
        "old",
        old,
        "works_at",
        c["value"].as_str().unwrap(),
        CPolarity::Affirm,
    );
    let text = scene["input"]["text"].as_str().unwrap();
    db.source("new", text, vec![old.into()]);
    let input = db.input(
        "new",
        vec![candidate(
            "new",
            text,
            "works_at",
            c["value"].as_str().unwrap(),
            CPolarity::Negate,
            0,
        )],
    );
    (db, input)
}
fn w4_case(root: &Path, model: &str, base: &str, id: &str, repeat: usize) -> Result<Case, String> {
    let (db, input) = prepared(id);
    let target = input.records[0].id();
    let action = if id == "n03" {
        "state_change"
    } else {
        "correction"
    };
    let sources = db
        .state()
        .sources
        .values()
        .map(|s| (s.id.clone(), s.request.clone().unwrap()))
        .collect::<serde_json::Map<_, _>>();
    let sample = json!({"id":id,"af216_point":true,"label_status":"draft","input":input,"sources":sources,
        "expect":{"decisions":[{"candidate":0,"status":"decided","action":action,"targets":[target],
            "pending_reason":null,"basis_sources":["old","new"],"resolves_relations":[]}]}});
    let mut request = stage_request_with_output_tokens("w4", &sample, 16384)?;
    request["model"] = json!(model);
    ReasoningEffort::Medium.apply(&mut request);
    af198::case(
        root,
        "w4",
        sample,
        &EvalModel {
            label: "af216-pro-points".into(),
            model: Some(model.into()),
            base: Some(base.into()),
            effort: ReasoningEffort::Medium,
        },
        repeat,
        request,
    )
}
fn r4_case(root: &Path, c: &Case) -> Result<Option<Case>, String> {
    let Replay::Output(text) = &c.replay else {
        return Ok(None);
    };
    let id = c.sample["id"].as_str().unwrap();
    let (db, input) = prepared(id);
    let state = db.state();
    let output: W4Output = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let final_output = w4::prepare_output(&input, &output, &|key| {
        state
            .sources
            .get(key)
            .and_then(|s| s.request.as_ref())
            .and_then(|r| r["text"].as_str())
            .map(str::to_owned)
    })
    .map_err(|e| e.to_string())?;
    let products = db.apply("new", &input, &final_output)?;
    let new = products.get(&0).ok_or("W4 did not commit a product")?;
    let place =
        scenes().into_iter().find(|s| s["id"] == id).unwrap()["input"]["candidates"][0]["value"]
            .as_str()
            .unwrap()
            .to_owned();
    let query = format!("我还在{place}任职吗？");
    let request = recall::Request {
        binding: RecallRequest {
            request_id: "af216-point-read".into(),
            query: query.clone(),
            context: vec![],
            task_id: None,
            at: AT + 100,
        },
        owner: OWNER.into(),
        subject_ids: None,
        said_on: DAY.into(),
        utc_offset_seconds: 28800,
        bootstrap: false,
    };
    let entities = [hippocampus::EntityId::from("self")];
    let relations = [w2::Relation {
        id: "works_at".into(),
        memory_types: vec![hippocampus::MemoryType::Fact],
    }];
    let ctx = r1::Context {
        query: &query,
        context: &[],
        entities: &entities,
        relations: &relations,
        subject_ids: None,
        task_id: None,
        said_on: DAY,
        utc_offset_seconds: 28800,
        supported_units: hippocampus::recall_models::SUPPORTED_UNITS,
    };
    let plan = r1::validate(
        r1::decode(
            &json!({"need":"judge","current_evidence":"query_only","type":"single",
        "subject":{"entity_id":"self"},"relation":"任职","conditions":[],"time":null,
        "plan":{"op":"lookup","filter":{"relation":"works_at","time":null},"unit":null}})
            .to_string(),
        )
        .map_err(|e| e.to_string())?,
        &ctx,
    )
    .map_err(|e| e.to_string())?;
    let assembly = recall::assemble(&db.open(), &request, &plan, recall::Budget::default())
        .map_err(|e| e.to_string())?;
    let source_texts = state
        .sources
        .values()
        .map(|s| (s.id.clone(), s.request.as_ref().unwrap()["text"].clone()))
        .collect::<serde_json::Map<_, _>>();
    let sample = json!({"id":id,"fixed_input":true,"af216_point":true,"label_status":"draft",
        "input":{"request":request.binding,"plan":plan.output,"evidence":assembly.evidence,"utc_offset_seconds":28800},
        "source_texts":source_texts,"expect":{},"negative_record_id":new,"old_record_id":input.records[0].id(),
        "origin":"AF216 fictional actual W4 commit; semantic negative current answer requires manual text review"});
    let mut req = fixed_adapter::request("r4", &sample, 4096)?;
    req["model"] = c.request["model"].clone();
    ReasoningEffort::Minimal.apply(&mut req);
    let model = EvalModel {
        effort: ReasoningEffort::Minimal,
        ..c.model.clone()
    };
    let case = af198::case(root, "r4", sample, &model, c.repeat, req)?;
    exact(
        &case.campaign.as_ref().unwrap().join("dependency.json"),
        &dependency(c, &case, text),
    )?;
    Ok(Some(case))
}
fn dependency(c: &Case, r: &Case, text: &str) -> Value {
    json!({"upstream_request":case_fingerprint(c),"upstream_output":hippocampus::recall_models::fingerprint(&json!(text)),"request":case_fingerprint(r)})
}
pub(crate) fn check_r4(sample: &Value, row: &mut Value) {
    let supported =
        serde_json::from_value::<R4Output>(row["validated_output"].clone()).is_ok_and(|o| {
            o.verdict == R4Verdict::Supported
                && o.facts.iter().any(|f| {
                    json!(f.record_ids)
                        .as_array()
                        .unwrap()
                        .contains(&sample["negative_record_id"])
                        && f.polarity == CPolarity::Negate
                        && f.current == Qualification::Yes
                })
                && o.facts.iter().all(|f| {
                    !json!(f.record_ids)
                        .as_array()
                        .unwrap()
                        .contains(&sample["old_record_id"])
                        || f.current != Qualification::Yes
                })
        });
    let mut fields = items(&row["fields"]).to_vec();
    check(
        &mut fields,
        "negative_current_record",
        json!(true),
        json!(supported),
        supported,
    );
    row["categories"] = categories(&fields);
    row["fields"] = json!(fields);
    if !supported && row["status"] != "输入无效" {
        row["status"] = json!("失败");
    }
    row["semantic_note"] = json!(
        "typed negative/current and explicit record identity checked; statement semantics still manual review"
    );
}
fn exact(path: &Path, value: &Value) -> Result<(), String> {
    if path.exists()
        && serde_json::from_slice::<Value>(&std::fs::read(path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?
            != *value
    {
        return Err("AF216 points binding changed; separate authorization required".into());
    }
    Ok(())
}
fn occupied(root: &Path) -> Result<usize, String> {
    count_sends(["n03", "n04"].into_iter().flat_map(|id| {
        (1..=2).flat_map(move |r| {
            ["w4", "r4"]
                .map(move |s| root.join(format!("campaign/slots/repeat-{r}-{s}-{id}/sends")))
        })
    }))
}
fn manifest(cases: &[Case]) -> Value {
    json!({"trial":"AF216-points","limit":8,"slots":cases.iter().flat_map(|c|[
        json!({"slot":campaign_slot(c)}),json!({"id":c.sample["id"],"stage":"r4","repeat":c.repeat,
            "upstream_request":case_fingerprint(c),"recipe":"actual-W4-prepare-validate-plan-W5-commit-R3.v1",
            "prompt":hippocampus::recall_models::R4_PROMPT,"schema":hippocampus::recall_models::r4_schema(),
            "effort":"minimal","max_output_tokens":4096,"timeout_seconds":360})]).collect::<Vec<_>>()})
}
pub(super) fn run() -> Result<(), String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/eval-negative-af216/points");
    let model =
        std::env::var("DEEPSEEK_PRO_MODEL").unwrap_or_else(|_| "ep-af216-unresolved-pro".into());
    let base =
        std::env::var("DEEPSEEK_BASE_URL").unwrap_or_else(|_| "https://af216.invalid/v1".into());
    let cases = ["n03", "n04"]
        .into_iter()
        .flat_map(|id| (1..=2).map(move |r| (id, r)))
        .map(|(id, r)| w4_case(&root, &model, &base, id, r))
        .collect::<Result<Vec<_>, _>>()?;
    let limit = limit()?;
    if limit.is_some_and(|n| n != 0 && n != 8) {
        return Err("AF216 points freezes 8 slots".into());
    }
    let auth = manifest(&cases);
    let authpath = root.join("campaign/authorization.json");
    exact(&authpath, &auth)?;
    if occupied(&root)? > 8 {
        return Err("AF216 points allowance exceeded".into());
    }
    let mut ready = 0;
    let mut wait = 0;
    let mut blocked = 0;
    let mut reused = 0;
    for c in &cases {
        campaign_check(std::slice::from_ref(c), 1)?;
        if matches!(c.replay, Replay::Stale | Replay::Rejected(_)) {
            return Err("AF216 points stale/rejected; no resend".into());
        }
        match r4_case(&root, c)? {
            Some(r) => {
                campaign_check(std::slice::from_ref(&r), 1)?;
                match r.replay {
                    Replay::Missing => ready += 1,
                    Replay::Output(_) | Replay::Failed(_) => reused += 1,
                    Replay::Stale | Replay::Rejected(_) => {
                        return Err("AF216 R4 stale/rejected".into());
                    }
                    _ => blocked += 1,
                }
            }
            None if matches!(c.replay, Replay::Missing) => wait += 1,
            None => blocked += 1,
        }
    }
    let missing = cases
        .iter()
        .filter(|c| matches!(c.replay, Replay::Missing))
        .count();
    if std::env::var("EVAL_DRY_RUN").as_deref() == Ok("1") {
        let bytes: usize = cases.iter().map(|c| c.wire_request.to_string().len()).sum();
        println!(
            "AF216-points：2个更新场景 ×2次 ×W4→SQLite→R3→R4 =8槽（W4 4 + R4 4）；R1=0。\nW4 medium/16384；R4 minimal/4096；DeepSeek Pro；均360秒。\nW4待录制 {missing}；R4已确定待录制 {ready}，可复用 {reused}，待W4输入 {wait}，上游阻断/在途 {blocked}。\n固定额度8；累计已发送/在途 {}；新增调用上限 {}。\nW4请求合计 {bytes} 字节（输入token保守字节上界）；R4待W4提交，不伪报精确token；输出配置容量81920 token（含推理，非预期用量）。\n本次真实调用0；不读取密钥、不创建账本/录制；chain 16 + points 8 =总上限24。",
            occupied(&root)?,
            missing + ready + wait
        );
        return Ok(());
    }
    if limit == Some(0) && missing + ready + wait > 0 {
        return Err("AF216 strict replay missing recordings".into());
    }
    let live = live(limit, 8, &model, &base)?;
    let _lock = if live.is_some() {
        Some(w3_trial_lock(&root.join("campaign"))?)
    } else {
        None
    };
    exact(&authpath, &auth)?;
    if live.is_some() && !authpath.exists() {
        m15_write_json(&authpath, &auth)?;
    }
    let mut results = vec![];
    let mut sent = 0;
    for c in cases {
        let report = evaluate_cases(
            std::slice::from_ref(&c),
            limit.map(|n| usize::from(n > 0)),
            &mut |c| send(&live, c),
        )?;
        sent += report["sent_calls"].as_u64().unwrap();
        results.push(report["results"][0].clone());
        let actual = w4_case(
            &root,
            &model,
            &base,
            c.sample["id"].as_str().unwrap(),
            c.repeat,
        )?;
        match r4_case(&root,&actual) {
            Ok(Some(r))=>{
                if live.is_some(){let Replay::Output(text)=&actual.replay else{unreachable!()};let p=r.campaign.as_ref().unwrap().join("dependency.json");if !p.exists(){m15_write_json(&p,&dependency(&actual,&r,text))?;}}
                let report=evaluate_cases(&[r],limit.map(|n|usize::from(n>0)),&mut |c|send(&live,c))?;
                sent+=report["sent_calls"].as_u64().unwrap();results.push(report["results"][0].clone());
            },other=>results.push(json!({"id":c.sample["id"],"stage":"r4","repeat":c.repeat,"status":"上游阻断（未发送）","detail":other.err()}))
        }
    }
    // Review 42 identified this semantic issue in the frozen recording, not by a code rule.
    let manual_review_items: Vec<_> = results.iter()
        .filter(|r| r["stage"] == "r4" && r["id"] == "n04" && r["repeat"] == 1)
        .map(|_| json!({"id":"n04","stage":"r4","repeat":1,"field":"facts[1].statement",
            "status":"待复核","finding":"纠错旧值被概括为真实的曾经任职；旧断言只能作为审计说明，不证明真实历史。",
            "origin":"AF219／评审42人工诊断；补入和机械核验不判定 statement 语义。"}))
        .collect();
    let report = json!({"trial":"AF216-points","sent_calls":sent,"cumulative_sent_or_inflight":occupied(&root)?,"authorized_calls":8,"results":results,
        "manual_review_items":manual_review_items,
        "note":"fixed verified candidates independently isolate W4/R4; chain uses real W1/W3 outputs; text semantics manual review"});
    let report_name = if limit == Some(0) {
        "report-af219.json"
    } else {
        "report.json"
    };
    m15_write_json(&root.join(report_name), &report)?;
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn af216_points_strict_requests_actual_negative_commit_and_derived_r4() {
        let temp = TempRecords::new();
        for id in ["n03", "n04"] {
            for repeat in 1..=2 {
                let c = w4_case(
                    &temp.0,
                    "ep-fixture",
                    "https://af216.invalid/v1",
                    id,
                    repeat,
                )
                .unwrap();
                assert_eq!(
                    case_fingerprint(&c),
                    case_fingerprint(
                        &w4_case(
                            &temp.0,
                            "ep-fixture",
                            "https://af216.invalid/v1",
                            id,
                            repeat
                        )
                        .unwrap()
                    )
                );
                assert_eq!(c.request["reasoning"]["effort"], "medium");
                let (_, input) = prepared(id);
                let output = W4Output {
                    decisions: vec![decision(
                        &input,
                        0,
                        if id == "n03" {
                            Action::StateChange
                        } else {
                            Action::Correction
                        },
                        vec![input.records[0].id()],
                    )],
                };
                save_recording(
                    &c,
                    &fake_envelope(json!(output)),
                    Some(&json!(output).to_string()),
                    None,
                )
                .unwrap();
                let actual = w4_case(
                    &temp.0,
                    "ep-fixture",
                    "https://af216.invalid/v1",
                    id,
                    repeat,
                )
                .unwrap();
                let r = r4_case(&temp.0, &actual).unwrap().unwrap();
                assert_eq!(r.request["reasoning"]["effort"], "minimal");
                let input: Value =
                    serde_json::from_str(r.request["input"].as_str().unwrap()).unwrap();
                assert!(
                    items(&input["evidence"]["records"])
                        .iter()
                        .any(
                            |v| v["candidate"]["polarity"] == "negate" && v["grade"] == "committed"
                        )
                );
                assert!(!input.to_string().contains("negative_record_id"));
                let new = r.sample["negative_record_id"].clone();
                let text = input["evidence"]["records"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|v| v["id"] == new)
                    .unwrap()["candidate"]["sentence"]["text"]
                    .clone();
                let output = json!({"verdict":"supported","compute":null,"facts":[{"statement":text,"evidence":[{"source":{"kind":"stored","source_id":"new"},"quote":{"text":text}}],"record_ids":[new],"polarity":"negate","strength":null,"uncertainty":null,"conditions":[],"times":[],"current":"yes","applies":"yes","supersedes_record_ids":[]}]});
                assert_eq!(
                    score("r4", &r.sample, Some(&output.to_string()))["status"],
                    "通过"
                );
                let mut supersedes = output.clone();
                supersedes["facts"][0]["supersedes_record_ids"] =
                    json!([r.sample["old_record_id"]]);
                let row = score("r4", &r.sample, Some(&supersedes.to_string()));
                assert_eq!(row["raw_result"]["status"], "失败");
                assert_eq!(
                    row["raw_result"]["fields"][0]["actual"]["rule"],
                    "record_basis"
                );
                assert_eq!(
                    row["raw_result"]["fields"][0]["actual"]["field"],
                    "facts[0].supersedes_record_ids[0]"
                );
                assert_eq!(row["status"], "通过");
                assert_eq!(row["final_result"]["status"], "通过");
                assert_eq!(items(&row["raw_output"]["facts"][0]["evidence"]).len(), 1);
                assert_eq!(items(&row["final_output"]["facts"][0]["evidence"]).len(), 2);
                assert_eq!(row["fields"], row["final_result"]["fields"]);
                assert_eq!(row["categories"], row["final_result"]["categories"]);
                assert!(
                    items(&row["final_result"]["fields"])
                        .iter()
                        .any(|f| f["field"] == "negative_current_record" && f["passed"] == true)
                );

                for old_body in [None, Some(json!("正文没有已核准的旧引用。"))] {
                    let mut sample = r.sample.clone();
                    if let Some(body) = old_body {
                        sample["source_texts"]["old"] = body;
                    } else {
                        sample["source_texts"]
                            .as_object_mut()
                            .unwrap()
                            .remove("old");
                    }
                    let rejected = score("r4", &sample, Some(&supersedes.to_string()));
                    assert_eq!(rejected["raw_result"]["status"], "失败");
                    assert_eq!(rejected["final_result"]["status"], "失败");
                    assert_eq!(
                        rejected["final_result"]["fields"][0]["actual"]["rule"],
                        "record_basis"
                    );
                    assert_eq!(
                        items(&rejected["final_output"]["facts"][0]["evidence"]).len(),
                        1
                    );
                }
                let mut outside = supersedes.clone();
                outside["facts"][0]["supersedes_record_ids"] = json!([90000]);
                let rejected = score("r4", &r.sample, Some(&outside.to_string()));
                assert_eq!(rejected["final_result"]["status"], "失败");
                assert_eq!(
                    rejected["final_result"]["fields"][0]["actual"]["rule"],
                    "record_id"
                );
                let mut wrong = supersedes;
                wrong["facts"][0]["polarity"] = json!("affirm");
                let rejected = score("r4", &r.sample, Some(&wrong.to_string()));
                assert_eq!(rejected["status"], "失败");
                assert_eq!(rejected["final_result"]["status"], "失败");
            }
        }
        assert_eq!(
            manifest(&[
                w4_case(&temp.0, "ep-fixture", "https://af216.invalid/v1", "n03", 1).unwrap()
            ])["limit"],
            8
        );
        assert_eq!(occupied(&temp.0).unwrap(), 0);
    }
}
