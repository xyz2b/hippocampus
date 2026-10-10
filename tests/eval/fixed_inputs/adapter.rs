//! 冻结首轮失败输入；核验复用生产 W3/R4，不调用上游、不增加语义标签。
use super::*;
use hippocampus::{r1, r4, recall_models};
use std::collections::{BTreeMap, BTreeSet};

pub fn selected(stages: &str, prefixes: &str) -> Result<Vec<(String, Value)>, String> {
    selected_set(
        stages,
        prefixes,
        std::env::var("EVAL_FIXED_INPUTS").as_deref() == Ok("af11"),
    )
}

pub fn selected_set(
    stages: &str,
    prefixes: &str,
    af11: bool,
) -> Result<Vec<(String, Value)>, String> {
    let stages: Vec<_> = stages.split(',').map(str::trim).collect();
    let allowed = if af11 { ["r1", "r4"] } else { ["w3", "r4"] };
    if stages.iter().any(|s| !allowed.contains(s)) {
        return Err(format!(
            "fixed input EVAL_STAGES only accepts {}",
            allowed.join(",")
        ));
    }
    let prefixes: Vec<_> = prefixes.split(',').map(str::trim).collect();
    if prefixes.len() > 1 && prefixes.contains(&"") {
        return Err("EVAL_CASES must not contain an empty prefix".into());
    }
    let data: Value = serde_json::from_str(if af11 {
        include_str!("af11.json")
    } else {
        include_str!("inputs.json")
    })
    .map_err(|e| e.to_string())?;
    if data["version"] != "eval.fixed_inputs@1" {
        return Err("invalid fixed input version".into());
    }
    Ok(items(&data["samples"])
        .iter()
        .filter(|s| stages.contains(&s["stage"].as_str().unwrap()))
        .filter(|s| {
            prefixes
                .iter()
                .any(|p| s["id"].as_str().unwrap().starts_with(p))
        })
        .map(|s| (s["stage"].as_str().unwrap().into(), s.clone()))
        .collect())
}

fn batch(input: &Value) -> Result<w2::Batch, String> {
    let candidates: Vec<w2::CandidateResult> =
        serde_json::from_value(input["candidates"].clone()).map_err(|e| e.to_string())?;
    let mut index_map = vec![None; candidates.iter().map(|c| c.index + 1).max().unwrap_or(0)];
    for (i, candidate) in candidates.iter().enumerate() {
        if candidate.status == w2::Status::Rejected
            || index_map[candidate.index].replace(i).is_some()
        {
            return Err("sample_input_rejected: frozen W3 candidates".into());
        }
    }
    Ok(w2::Batch {
        candidates,
        memory_requests: serde_json::from_value(input["memory_requests"].clone())
            .map_err(|e| e.to_string())?,
        index_map,
        task_id: serde_json::from_value(input["task_id"].clone()).map_err(|e| e.to_string())?,
        said_on: serde_json::from_value(input.get("said_on").ok_or("said_on")?.clone())
            .map_err(|e| e.to_string())?,
        utc_offset_seconds: serde_json::from_value(input["utc_offset_seconds"].clone())
            .map_err(|e| e.to_string())?,
    })
}

fn r4_input(sample: &Value) -> Result<recall_adapter::R4Input, String> {
    let input = &sample["input"];
    let request: RecallRequest =
        serde_json::from_value(input["request"].clone()).map_err(|e| e.to_string())?;
    let evidence: RecallEvidence =
        serde_json::from_value(input["evidence"].clone()).map_err(|e| e.to_string())?;
    let mut source_dates = BTreeMap::new();
    for raw in &evidence.raw {
        source_dates.insert(raw.source_id.clone(), raw.said_on.clone());
    }
    for record in &evidence.records {
        if let RecordView::Current(record) = record {
            for evidence in &record.evidence {
                if let SourceRef::Stored { source_id } = &evidence.source {
                    source_dates.insert(source_id.clone(), record.said_on.clone());
                }
            }
        }
    }
    Ok(recall_adapter::R4Input {
        plan: r1::Plan {
            query: request.query.clone(),
            context: request.context.clone(),
            subject_ids: None,
            task_id: request.task_id.as_deref().map(Into::into),
            output: serde_json::from_value(input["plan"].clone()).map_err(|e| e.to_string())?,
            time_resolution: None,
            filter_time_resolution: None,
        },
        request,
        evidence,
        source_dates,
        source_texts: serde_json::from_value(sample["source_texts"].clone())
            .map_err(|e| e.to_string())?,
        utc_offset_seconds: 28_800,
        source_context: BTreeMap::new(),
    })
}

fn r1_plan(sample: &Value, text: &str) -> Result<r1::Plan, String> {
    let input = &sample["input"];
    let entities: Vec<hippocampus::Entity> =
        serde_json::from_value(input["entities"].clone()).map_err(|e| e.to_string())?;
    let ids: Vec<_> = entities.into_iter().map(|e| e.id).collect();
    let relations: Vec<w2::Relation> =
        serde_json::from_value::<Vec<String>>(input["relations"].clone())
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|id| w2::Relation {
                id,
                memory_types: vec![
                    hippocampus::MemoryType::Fact,
                    hippocampus::MemoryType::Preference,
                    hippocampus::MemoryType::Constraint,
                    hippocampus::MemoryType::Episode,
                ],
            })
            .collect();
    let context: Vec<String> =
        serde_json::from_value(input["context"].clone()).map_err(|e| e.to_string())?;
    let subject_ids: Option<Vec<hippocampus::EntityId>> =
        serde_json::from_value(input["subject_ids"].clone()).map_err(|e| e.to_string())?;
    let task: Option<hippocampus::TaskId> =
        serde_json::from_value(input["task_id"].clone()).map_err(|e| e.to_string())?;
    let units: Vec<String> =
        serde_json::from_value(input["supported_units"].clone()).map_err(|e| e.to_string())?;
    let units: Vec<_> = units.iter().map(String::as_str).collect();
    let ctx = r1::Context {
        query: input["message"].as_str().ok_or("message")?,
        context: &context,
        entities: &ids,
        relations: &relations,
        subject_ids: subject_ids.as_deref(),
        task_id: task.as_ref(),
        said_on: input["said_on"].as_str().ok_or("said_on")?,
        utc_offset_seconds: serde_json::from_value(input["utc_offset_seconds"].clone())
            .map_err(|e| e.to_string())?,
        supported_units: &units,
    };
    if text.is_empty() {
        r1::not_understood(&ctx)
    } else {
        r1::validate(r1::decode(text).map_err(|e| e.to_string())?, &ctx)
    }
    .map_err(|e| e.to_string())
}

pub fn request(stage: &str, sample: &Value, tokens: u32) -> Result<Value, String> {
    let (stage, name, prompt, schema) = match stage {
        "r1" => {
            r1_plan(sample, "")?;
            (
                ModelStage::R1,
                "r1_v2",
                recall_models::R1_PROMPT,
                recall_models::r1_schema(),
            )
        }
        "w3" => {
            batch(&sample["input"])?;
            (
                ModelStage::W3,
                "w3_v4",
                write_models::W3_PROMPT,
                write_models::w3_schema(),
            )
        }
        "r4" => {
            let input = r4_input(sample)?;
            (
                ModelStage::R4,
                "r4_v7",
                recall_models::R4_PROMPT,
                recall_models::r4_schema_for_plan(&input.plan),
            )
        }
        _ => return Err("invalid fixed input stage".into()),
    };
    // Preserve the frozen production fields; only the current protocol binding changes.
    let mut input = sample["input"].clone();
    input["contract"] = json!(match stage {
        ModelStage::W3 => "w3.v4",
        ModelStage::R1 => "r1.v2",
        ModelStage::R4 => "r4.v7",
        _ => unreachable!(),
    });
    let mut request = structured_request(
        stage,
        name,
        prompt,
        input,
        schema,
        &ModelBudgetConfig::default(),
    );
    request["max_output_tokens"] = json!(tokens);
    Ok(request)
}

pub fn assemble_r4(sample: &Value, text: &str) -> Result<R4Output, String> {
    let f = r4_input(sample)?;
    let output = r4::decode_detailed(text).map_err(diagnostic)?;
    Ok(r4::assemble_old_basis(
        &output,
        &context(&f, sample),
        &|id| f.source_texts.get(id).cloned(),
    ))
}

fn context<'a>(f: &'a recall_adapter::R4Input, sample: &Value) -> r4::Context<'a> {
    r4::Context {
        evidence: &f.evidence,
        plan: &f.plan,
        request: &f.request,
        utc_offset_seconds: sample["input"]["utc_offset_seconds"].as_i64().unwrap() as i32,
        source_dates: &f.source_dates,
    }
}

fn diagnostic(d: r4::ValidationDiagnostic) -> String {
    json!({"code":d.error.to_string(),"rule":d.error.0,"field":d.field,"cause":d.cause}).to_string()
}

pub fn score(
    stage: &str,
    sample: &Value,
    text: &str,
    fields: &mut Vec<Value>,
) -> Result<Value, String> {
    let actual = match stage {
        "r1" => {
            let output = r1_plan(sample, text)?.output.ok_or("r1_not_understood")?;
            let actual = serde_json::to_value(output).unwrap();
            if let Some(expected) = sample["expect"]["plan.op"].as_str() {
                compare(fields, "plan.op", &json!(expected), &actual["plan"]["op"]);
            }
            actual
        }
        "w3" => {
            let input = &sample["input"];
            let batch = batch(input)?;
            let output = W3Output::decode(text).map_err(|e| e.to_string())?;
            output
                .clone()
                .validate(
                    &batch,
                    &input["source_snapshot"],
                    input["current_user_message"]
                        .as_str()
                        .ok_or("current message")?,
                )
                .map_err(|e| e.to_string())?;
            serde_json::to_value(output).unwrap()
        }
        "r4" => {
            let f = r4_input(sample)?;
            let evaluated = r4::evaluate_detailed(
                r4::decode_detailed(text).map_err(diagnostic)?,
                &context(&f, sample),
            )
            .map_err(diagnostic)?;
            serde_json::to_value(evaluated.output).unwrap()
        }
        _ => return Err("invalid fixed input stage".into()),
    };
    check(fields, "decode/validate", json!(true), json!(true), true);
    Ok(actual)
}

#[test]
fn af11_exact_inputs_bound_sources_and_seven_call_cap() {
    let selected = selected_set("r1,r4", "", true).unwrap();
    assert_eq!(selected.iter().filter(|(s, _)| s == "r1").count(), 2);
    assert_eq!(selected.iter().filter(|(s, _)| s == "r4").count(), 5);
    for (stage, sample) in &selected {
        let origin = &sample["origin"];
        let original: Value = serde_json::from_slice(
            &std::fs::read(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/eval/end_to_end")
                    .join(origin["label"].as_str().unwrap())
                    .join(format!("{}.json", origin["case"].as_str().unwrap())),
            )
            .unwrap(),
        )
        .unwrap();
        let row = &original["operations"][origin["operation"].as_str().unwrap()]
            [origin["row"].as_u64().unwrap() as usize];
        assert_eq!(
            origin["request"],
            json!(recall_models::fingerprint(&row["request_content"]))
        );
        if sample.get("evidence_origin").is_none() {
            assert_eq!(sample["input"], row["request_content"]["input"]);
        } else {
            let mut input = sample["input"].clone();
            input["evidence"] = row["request_content"]["input"]["evidence"].clone();
            assert_eq!(input, row["request_content"]["input"]);
            assert_eq!(
                sample["input"]["evidence"]["records"][0]["update"]["status"],
                "judged"
            );
            assert_eq!(
                sample["input"]["evidence"]["completeness"]["pending_updates"],
                0
            );
        }
        let request = request(stage, sample, 16384).unwrap();
        let mut expected = sample["input"].clone();
        expected["contract"] = json!(match stage.as_str() {
            "w3" => "w3.v4",
            "r1" => "r1.v2",
            "r4" => "r4.v7",
            _ => unreachable!(),
        });
        assert_eq!(
            serde_json::from_str::<Value>(request["input"].as_str().unwrap()).unwrap(),
            expected
        );
        assert!(sample["input"].get("model").is_none());
        assert!(sample["input"].get("origin").is_none());
        assert_ne!(recording_fingerprint(sample, &request), origin["request"]);
    }
    let temp = TempRecords::new();
    let model = EvalModel {
        label: "af11-test".into(),
        model: Some("ep-20261003130756-trhj6".into()),
        base: None,
        effort: ReasoningEffort::Minimal,
    };
    let cases = plan_repeated(selected, &temp.0, &model, 1).unwrap();
    assert_eq!(preflight(&cases, None).unwrap(), 0);
    assert!(preflight(&cases, Some(6)).is_err());
    assert_eq!(preflight(&cases, Some(7)).unwrap(), 7);
    assert_eq!(
        cases
            .iter()
            .filter(|c| c.model.effort == ReasoningEffort::Low)
            .count(),
        2
    );
    let report = evaluate(&cases, None, &mut |_| panic!("AF11 offline sent")).unwrap();
    assert_eq!(report["sent_calls"], 0);
}

#[test]
fn af11_q3_wrong_current_reference_remains_rejected() {
    let sample = selected_set("r4", "af11_r4_unresolved", true)
        .unwrap()
        .remove(0)
        .1;
    let origin = &sample["origin"];
    let doc: Value = serde_json::from_slice(
        &std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/eval/end_to_end")
                .join(origin["label"].as_str().unwrap())
                .join("unresolved_conflict.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let row = &doc["operations"][origin["operation"].as_str().unwrap()]
        [origin["row"].as_u64().unwrap() as usize];
    let envelope: Value = serde_json::from_str(row["body"].as_str().unwrap()).unwrap();
    let text = completed_text(&envelope).unwrap();
    let error: Value =
        serde_json::from_str(&score("r4", &sample, &text, &mut vec![]).unwrap_err()).unwrap();
    assert_eq!(error["rule"], "quote_or_source");
    assert_eq!(error["field"], "facts[0].evidence[0]");
}

#[test]
fn fixed_inputs_are_exactly_the_33_original_failed_requests() {
    let selected = selected("w3,r4", "").unwrap();
    assert_eq!(selected.iter().filter(|(s, _)| s == "w3").count(), 23);
    assert_eq!(selected.iter().filter(|(s, _)| s == "r4").count(), 10);
    let mut seen = BTreeSet::new();
    for (stage, sample) in selected {
        assert!(seen.insert(sample["id"].as_str().unwrap().to_owned()));
        let origin = &sample["origin"];
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/eval/end_to_end")
            .join(origin["label"].as_str().unwrap());
        let file: Value = serde_json::from_slice(
            &std::fs::read(dir.join(format!("{}.json", origin["case"].as_str().unwrap()))).unwrap(),
        )
        .unwrap();
        let row = &file["operations"][origin["operation"].as_str().unwrap()]
            [origin["row"].as_u64().unwrap() as usize];
        assert_eq!(row["request"], origin["request"]);
        assert_eq!(
            json!(recall_models::fingerprint(&row["request_content"])),
            origin["request"]
        );
        assert_eq!(sample["input"], row["request_content"]["input"]);
        assert_eq!(sample["original_usage"], row["usage"]);
        let request = request(&stage, &sample, 16384).unwrap();
        let mut expected = sample["input"].clone();
        expected["contract"] = json!(match stage.as_str() {
            "w3" => "w3.v4",
            "r1" => "r1.v2",
            "r4" => "r4.v7",
            _ => unreachable!(),
        });
        assert_eq!(
            serde_json::from_str::<Value>(request["input"].as_str().unwrap()).unwrap(),
            expected
        );
        assert!(sample["input"].get("model").is_none());
        assert!(sample["input"].get("origin").is_none());
        // 冻结旧输入，不能将原失败响应的指纹冒充新提示词请求。
        assert_ne!(recording_fingerprint(&sample, &request), origin["request"]);
    }
    assert_eq!(self::selected("w3", "").unwrap().len(), 23);
    assert_eq!(self::selected("r4", "").unwrap().len(), 10);
    assert!(self::selected("w1", "").is_err());
}

#[test]
fn fixed_input_record_basis_diagnostic_keeps_raw_and_stale_replay_does_not_send() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/eval/fixed_inputs/recordings");
    let model = EvalModel {
        label: "ac2-fixed-pro-minimal".into(),
        model: Some("ep-20261003130756-trhj6".into()),
        base: Some("https://ark.cn-beijing.volces.com/api/v3".into()),
        effort: ReasoningEffort::Minimal,
    };
    let cases = plan_repeated(selected("r4", "").unwrap(), &root, &model, 1).unwrap();
    let report = evaluate(&cases, None, &mut |_| panic!("offline replay sent")).unwrap();
    assert_eq!(report["sent_calls"], 0);
    assert!(cases.iter().all(|c| matches!(c.replay, Replay::Stale)));
    for id in ["r4_lme_603deb26_q", "r4_lme_gpt4_6dc9b45b_q"] {
        let case = cases.iter().find(|c| c.sample["id"] == id).unwrap();
        // Historical output is only a pure validator/assembly regression fixture.
        // It is not registered under a new fingerprint or reused as a model answer.
        let text = items(&case.file["recorded"]).last().unwrap()["output_text"]
            .as_str()
            .unwrap();
        let row = super::score("r4", &case.sample, Some(text));
        assert_eq!(row["raw_result"]["status"], "失败");
        assert_eq!(
            row["raw_result"]["fields"][0]["actual"]["rule"],
            "record_basis"
        );
        assert_eq!(
            row["raw_result"]["fields"][0]["actual"]["field"],
            "facts[0].record_ids[0]"
        );
        assert_eq!(row["final_result"]["status"], "通过");
        assert_eq!(
            row["final_output"]["facts"][0]["record_ids"],
            row["raw_output"]["facts"][0]["record_ids"]
        );
        assert_eq!(row["final_output"]["compute"], row["raw_output"]["compute"]);
        assert_eq!(
            row["final_output"]["facts"][0]["evidence"]
                .as_array()
                .unwrap()
                .len(),
            row["raw_output"]["facts"][0]["evidence"]
                .as_array()
                .unwrap()
                .len()
                + 1
        );
    }
}

#[test]
fn fixed_input_replay_binds_every_field_and_records_each_call() {
    let selected = selected("w3,r4", "").unwrap();
    let dir = TempRecords::new();
    let model = EvalModel {
        label: "fixed-test".into(),
        model: Some("ep-20261003130756-trhj6".into()),
        base: None,
        effort: ReasoningEffort::Minimal,
    };
    let cases = plan_repeated(selected.clone(), &dir.0, &model, 1).unwrap();
    assert_eq!(preflight(&cases, None).unwrap(), 0);
    assert!(preflight(&cases, Some(32)).is_err());
    let mut sends = 0;
    let report = evaluate(&cases, Some(33), &mut |request| {
        sends += 1;
        assert_eq!(request["model"], model.model.as_ref().unwrap().as_str());
        let input: Value = serde_json::from_str(request["input"].as_str().unwrap()).unwrap();
        let effort = if input["plan"].is_object()
            && !["lookup", "list"].contains(&input["plan"]["plan"]["op"].as_str().unwrap())
        {
            "low"
        } else {
            "minimal"
        };
        assert_eq!(request["reasoning"]["effort"], effort);
        Ok(
            json!({"status":"incomplete","incomplete_details":{"reason":"length"},
            "usage":{"input_tokens":12,"output_tokens":16384}}),
        )
    })
    .unwrap();
    assert_eq!(sends, 33);
    assert_eq!(report["sent_calls"], 33);
    assert!(report["reasoning_effort"].is_null());
    assert_eq!(report["reasoning_efforts"], json!(["low", "minimal"]));
    assert_eq!(
        cases
            .iter()
            .filter(|c| c.model.effort == ReasoningEffort::Low)
            .count(),
        8
    );
    assert!(
        report["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["status"] == "调用失败")
    );
    let cached = plan_repeated(selected, &dir.0, &model, 1).unwrap();
    assert_eq!(preflight(&cached, Some(0)).unwrap(), 0);
    let report = evaluate(&cached, Some(0), &mut |_| panic!("replay sent")).unwrap();
    assert_eq!(report["sent_calls"], 0);
    let case = &cached[0];
    let mut changed = case.request.clone();
    let original = recording_fingerprint(&case.sample, &changed);
    changed["instructions"] = json!("different prompt");
    assert_ne!(recording_fingerprint(&case.sample, &changed), original);
    let mut changed = case.request.clone();
    let mut input: Value = serde_json::from_str(changed["input"].as_str().unwrap()).unwrap();
    input["source_snapshot"][3] = json!(0);
    changed["input"] = json!(input.to_string());
    assert_ne!(recording_fingerprint(&case.sample, &changed), original);
    for (key, value) in [
        ("model", json!("another-model")),
        ("reasoning", json!({"effort":"medium"})),
        ("max_output_tokens", json!(4096)),
    ] {
        let mut changed = case.request.clone();
        changed[key] = value;
        assert_ne!(recording_fingerprint(&case.sample, &changed), original);
    }
}

#[test]
fn fixed_input_m15_prompts_expire_w3_and_r4_without_sending() {
    // AE14 已补齐 low 录制；此回归仍独立模拟“只有 minimal”时的严格复用。
    // M1.5 同时修改 W3/R4 提示词；旧 W3 也必须过期，不能沿用原先的 23 项复用。
    let root = TempRecords::new();
    let minimal = Path::new("ac2-fixed-pro-minimal/reasoning-minimal/repeat-1");
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/eval/fixed_inputs/recordings")
        .join(minimal);
    let destination = root.0.join(minimal);
    std::fs::create_dir_all(&destination).unwrap();
    for entry in std::fs::read_dir(source).unwrap().map(Result::unwrap) {
        std::fs::copy(entry.path(), destination.join(entry.file_name())).unwrap();
    }
    let model = EvalModel {
        label: "ac2-fixed-pro-minimal".into(),
        model: Some("ep-20261003130756-trhj6".into()),
        base: Some("https://ark.cn-beijing.volces.com/api/v3".into()),
        effort: ReasoningEffort::Minimal,
    };
    let cases = plan_repeated(selected("w3,r4", "").unwrap(), &root.0, &model, 1).unwrap();
    assert_eq!(
        cases
            .iter()
            .filter(|c| matches!(c.replay, Replay::Output(_) | Replay::Failed(_)))
            .count(),
        0
    );
    assert_eq!(
        cases
            .iter()
            .filter(|c| matches!(c.replay, Replay::Missing))
            .count(),
        8
    );
    assert_eq!(
        cases
            .iter()
            .filter(|c| matches!(c.replay, Replay::Stale))
            .count(),
        25
    );
    assert!(
        cases
            .iter()
            .all(|c| matches!(c.replay, Replay::Missing | Replay::Stale))
    );
    for case in &cases {
        if case.model.effort == ReasoningEffort::Low {
            assert_eq!(case.stage, "r4");
            assert!(case.path.to_string_lossy().contains("reasoning-low"));
            let mut old = case.request.clone();
            ReasoningEffort::Minimal.apply(&mut old);
            assert_ne!(
                recording_fingerprint(&case.sample, &case.request),
                recording_fingerprint(&case.sample, &old)
            );
        }
    }
    assert!(preflight(&cases, Some(32)).is_err());
    assert_eq!(preflight(&cases, Some(33)).unwrap(), 33);
    let report = evaluate(&cases, None, &mut |_| panic!("offline sender invoked")).unwrap();
    assert_eq!(report["sent_calls"], 0);
}

#[test]
fn fixed_input_original_responses_reproduce_the_failure_modes_without_sending() {
    fn source_ids(value: &mut Value, ids: &BTreeMap<String, String>) {
        match value {
            Value::Object(o) => {
                if let Some(Value::String(id)) = o.get_mut("source_id") {
                    if let Some(stable) = ids.get(id) {
                        *id = stable.clone();
                    }
                }
                o.values_mut().for_each(|v| source_ids(v, ids));
            }
            Value::Array(a) => a.iter_mut().for_each(|v| source_ids(v, ids)),
            _ => (),
        }
    }
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/eval/end_to_end/ac1-pro-medium-flash-medium-pro-minimal");
    let run: Value = serde_json::from_slice(&std::fs::read(dir.join("run.json")).unwrap()).unwrap();
    let mut incomplete = 0;
    let mut invalid_w3 = 0;
    let mut invalid_r4 = 0;
    for (stage, sample) in selected("w3,r4", "").unwrap() {
        let origin = &sample["origin"];
        let doc: Value = serde_json::from_slice(
            &std::fs::read(dir.join(format!("{}.json", origin["case"].as_str().unwrap()))).unwrap(),
        )
        .unwrap();
        let row = &doc["operations"][origin["operation"].as_str().unwrap()]
            [origin["row"].as_u64().unwrap() as usize];
        let envelope: Value = serde_json::from_str(row["body"].as_str().unwrap()).unwrap();
        if envelope["status"] != "completed" {
            assert_eq!(stage, "w3");
            incomplete += 1;
            continue;
        }
        // 仅诊断原失败：不注册为新指纹录制，不作新提示词质量比较。
        let case = items(&run["cases"])
            .iter()
            .find(|c| c["case"] == origin["case"])
            .unwrap();
        let ids: BTreeMap<_, _> = items(&case["steps"])
            .iter()
            .filter(|s| s["step"] == "message")
            .map(|s| {
                (
                    s["received"]["source"]["source_id"]
                        .as_str()
                        .unwrap()
                        .to_owned(),
                    s["message"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        let text = completed_text(&envelope).unwrap();
        let mut output: Value = serde_json::from_str(&text).unwrap_or(json!(text));
        source_ids(&mut output, &ids);
        let report = super::score(&stage, &sample, Some(&output.to_string()));
        assert_eq!(report["status"], "失败", "{}: {report}", sample["id"]);
        assert_eq!(report["scoring_scope"], "contract_only");
        if stage == "w3" {
            invalid_w3 += 1;
        } else {
            invalid_r4 += 1;
        }
    }
    assert_eq!((incomplete, invalid_w3, invalid_r4), (14, 9, 10));
}
