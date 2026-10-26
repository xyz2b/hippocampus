//! Independent eight-slot R4 trial and read-only historical AF255 revalidation.
use super::*;
use hippocampus::{r1, r4};
use std::collections::BTreeMap;

const ROOT: &str = "target/eval-r4-af264";
const CAP: usize = 8;

fn scenes() -> Vec<Value> {
    let texts = [
        (
            "我正在阅读《灯影》，书中我最喜欢的章节名是\"竹声\"。",
            "我最喜欢哪一章？",
            vec![],
        ),
        (
            "我的台灯默认亮度为三级。",
            "不调整亮度时，我的台灯亮度是多少？",
            vec![],
        ),
        (
            "只在画水彩时，我使用\"湖青\"颜料。",
            "画水彩时我使用什么颜料？",
            vec!["只在画水彩时"],
        ),
        ("我的阅读灯名为\"叶光\"。", "我的阅读灯现在叫什么？", vec![]),
    ];
    texts.into_iter().enumerate().map(|(i, (text, query, conditions))| {
        let record = |id, source, text, conditions: Vec<&str>| json!({"id":id,"source":source,
            "sentence":text,"said_on":"2026-02-14","status":"active","termination":null,
            "ended":"not_ended","update":"judged","conditions":conditions,"times":[]});
        let mut records = vec![record(1, "s-1", text, conditions.clone())];
        if i == 3 {
            records.push(record(2, "s-2", "我把阅读灯改名为\"苔色\"，原来的名字不再使用。", vec![]));
        }
        let target = if i == 3 { 2 } else { 1 };
        let mut sample = json!({"id":format!("g{:02}",i+1),"label_revision":"AF264-draft@1",
            "input":{"at":"2026-02-14","query":query,"current_evidence":"query_only",
            "plan":{"type":"single","op":"lookup","relation":"fact"},"records":records,"raw":[],"relations":[],
            "completeness":{"retrieval_truncated":false,"plan_complete":true}},
            "expect":{"verdict":"supported","facts":[{"record_ids":[target],"must":{"current":"yes",
                "conditions":conditions,"supersedes_record_ids":if i==3 {vec![1]}else{vec![]}}}],"compute":null}});
        sample = recall_adapter::full_sample(&sample);
        if i == 1 {
            sample["input"]["plan"]["conditions"] = json!([{"quote":{"text":"不调整亮度时"},"resolved":true}]);
        }
        sample
    }).collect()
}

fn plan(root: &Path, model: &str, base: &str) -> Result<Vec<Case>, String> {
    let provider = serde_json::from_value(json!({"protocol":"openai_responses",
        "base_url_env":"HUOSHAN_DEEPSEEK_BASE_URL","api_key_env":"HUOSHAN_DEEPSEEK_API_KEY"}))
    .unwrap();
    let options = ProviderOptions {
        provider,
        campaign: root.join("campaign"),
    };
    let model = EvalModel {
        label: "af264-pro-minimal".into(),
        model: Some(model.into()),
        base: Some(base.trim_end_matches('/').into()),
        effort: ReasoningEffort::Minimal,
    };
    let mut cases = Vec::new();
    for sample in scenes() {
        for repeat in 1..=2 {
            let mut request = stage_request_with_output_tokens("r4", &sample, 4096)?;
            request["model"] = json!(model.model);
            model.effort.apply(&mut request);
            cases.push(case_from_request_with_provider(
                "r4".into(),
                sample.clone(),
                &root.join(format!("recordings/repeat-{repeat}")),
                &model,
                repeat,
                request,
                Some(&options),
                Some(180),
            )?);
        }
    }
    compare_mark_interrupted(&mut cases)?;
    Ok(cases)
}

fn historical_input(input: &Value, sources: &Value) -> Result<recall_adapter::R4Input, String> {
    if input["contract"] != "r4.v7" {
        return Err("archive contract mismatch".into());
    }
    let request: RecallRequest =
        serde_json::from_value(input["request"].clone()).map_err(|e| e.to_string())?;
    let evidence: RecallEvidence =
        serde_json::from_value(input["evidence"].clone()).map_err(|e| e.to_string())?;
    let mut source_dates = BTreeMap::new();
    // Use the actual pre-query source snapshot, never a record Quote to prove itself.
    let source_texts = sources
        .as_object()
        .ok_or("missing source snapshot")?
        .iter()
        .filter(|(_, s)| s["enrichment"] != "deleted")
        .filter_map(|(id, s)| {
            s["request"]["text"]
                .as_str()
                .map(|text| (id.clone(), text.to_owned()))
        })
        .collect::<BTreeMap<_, _>>();
    for raw in &evidence.raw {
        source_dates.insert(raw.source_id.clone(), raw.said_on.clone());
    }
    for record in &evidence.records {
        let refs = match record {
            RecordView::Current(r) => &r.evidence,
            RecordView::Legacy(r) => &r.evidence,
        };
        for e in refs {
            if let SourceRef::Stored { source_id } = &e.source {
                if let RecordView::Current(r) = record {
                    source_dates
                        .entry(source_id.clone())
                        .or_insert(r.said_on.clone());
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
        source_texts,
        utc_offset_seconds: input["utc_offset_seconds"].as_i64().ok_or("offset")? as i32,
        source_context: BTreeMap::new(),
    })
}

fn revalidate(input: &Value, sources: &Value, text: &str) -> Result<Value, String> {
    let f = historical_input(input, sources)?;
    let ctx = r4::Context {
        evidence: &f.evidence,
        plan: &f.plan,
        request: &f.request,
        utc_offset_seconds: f.utc_offset_seconds,
        source_dates: &f.source_dates,
    };
    let raw = r4::decode_detailed(text).map_err(|e| format!("{}:{}", e.error.0, e.field))?;
    let final_output = r4::assemble_old_basis(&raw, &ctx, &|id| f.source_texts.get(id).cloned());
    let check = |output| match r4::evaluate_detailed(output, &ctx) {
        Ok(result) => json!({"passed":true,"output":result.output,"computed":result.computed}),
        Err(e) => json!({"passed":false,"rule":e.error.0,"field":e.field,"cause":e.cause}),
    };
    Ok(
        json!({"raw_output_text":text,"raw_output":raw,"final_output":final_output,
        "raw_result":check(raw.clone()),"final_result":check(final_output.clone())}),
    )
}

fn replay() -> Result<(), String> {
    let root = Path::new("target/eval-e2e-af255/A");
    let mut paths: Vec<_> = std::fs::read_dir(root)
        .map_err(|e| e.to_string())?
        .map(|e| e.map(|e| e.path()))
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    paths.sort();
    let mut rows = Vec::new();
    for path in paths
        .into_iter()
        .filter(|p| p.extension().is_some_and(|s| s == "json"))
    {
        let doc: Value = serde_json::from_slice(&std::fs::read(&path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        let Some(ops) = doc["operations"].as_object() else {
            continue;
        };
        for (op, calls) in ops {
            for call in items(calls).iter().filter(|c| c["schema"] == "r4_v7") {
                if call["request"]
                    != json!(hippocampus::recall_models::fingerprint(
                        &call["request_content"]
                    ))
                {
                    return Err("archived request fingerprint mismatch".into());
                }
                let envelope: Value = serde_json::from_str(call["body"].as_str().ok_or("body")?)
                    .map_err(|e| e.to_string())?;
                let text = completed_text(&envelope)?;
                let question = op.strip_prefix("recall:").ok_or("not a recall operation")?;
                let mut row = revalidate(
                    &call["request_content"]["input"],
                    &doc["checkpoints"][format!("before:{question}")]["sources"],
                    &text,
                )?;
                row["case"] = json!(path.file_stem().unwrap().to_str().unwrap());
                row["operation"] = json!(op);
                row["archived_request_fingerprint"] = call["request"].clone();
                row["stale_for_current_prompt"] = json!(
                    !call["request_content"]["instructions"]
                        .as_str()
                        .unwrap_or("")
                        .contains(hippocampus::recall_models::R4_PROMPT)
                );
                rows.push(row);
            }
        }
    }
    let summary = json!({"total":rows.len(),"raw_passed":rows.iter().filter(|r|r["raw_result"]["passed"]==true).count(),
        "final_passed":rows.iter().filter(|r|r["final_result"]["passed"]==true).count(),"sent_calls":0});
    let report = json!({"trial":"AF264-historical-AF255","summary":summary,"results":rows,
        "note":"Original requests and fingerprints checked read-only. Historical output revalidation only; current prompt recordings are stale. No common.v1 rescoring or human acceptance."});
    let output = Path::new(ROOT).join("replay-af255.json");
    m15_write_json(&output, &report)?;
    println!(
        "AF255 历史原请求严格核对；机械核验 raw/final：{summary}；{}",
        output.display()
    );
    Ok(())
}

pub(super) fn run() -> Result<(), String> {
    let mode = std::env::var("EVAL_AF264").map_err(|e| e.to_string())?;
    for key in [
        "EVAL_RECALL_COMPARE",
        "EVAL_COMPARE",
        "EVAL_SMOKE",
        "EVAL_M15",
        "EVAL_AF216",
        "EVAL_CASES",
        "EVAL_STAGES",
        "EVAL_REPEAT",
        "EVAL_MODEL",
        "EVAL_REASONING_EFFORT",
        "EVAL_PROVIDER_CONFIG",
        "EVAL_BASE_URL",
        "EVAL_API_KEY",
        "EVAL_AF206",
        "EVAL_AF198",
        "EVAL_AF193",
        "EVAL_AF196",
        "EVAL_MAX_OUTPUT_TOKENS",
        "EVAL_CAMPAIGN_DIR",
    ] {
        if std::env::var_os(key).is_some() {
            return Err(format!("AF264 fixed cohort rejects {key}"));
        }
    }
    if mode == "replay" {
        if std::env::var_os("EVAL_RECORD").is_some() {
            return Err("historical replay forbids recording".into());
        }
        return replay();
    }
    if mode != "trial" {
        return Err("EVAL_AF264 requires trial or replay".into());
    }
    let limit = std::env::var("EVAL_RECORD")
        .ok()
        .map(|s| s.parse::<usize>().map_err(|_| "EVAL_RECORD integer"))
        .transpose()?;
    if limit.is_some_and(|n| n != CAP) {
        return Err("AF264 frozen allowance is exactly 8".into());
    }
    let model =
        std::env::var("DEEPSEEK_PRO_MODEL").unwrap_or_else(|_| "ep-af264-unresolved-pro".into());
    let base = std::env::var("HUOSHAN_DEEPSEEK_BASE_URL")
        .unwrap_or_else(|_| "https://af264.invalid/v1".into());
    let cases = plan(Path::new(ROOT), &model, &base)?;
    if std::env::var("EVAL_DRY_RUN").as_deref() == Ok("1") {
        let pending = preflight(&cases, Some(CAP))?;
        println!(
            "AF264 干跑：4 虚构固定输入 × 2；R4={CAP}，W/R1/嵌入=0；待录制 {pending}；拒绝 {}；输出 token 上界 {}；minimal，4096，180s；不发送、不建账本。",
            cases
                .iter()
                .filter(|c| matches!(c.replay, Replay::Rejected(_)))
                .count(),
            CAP * 4096
        );
        return Ok(());
    }
    let live = if limit.is_some() {
        if std::env::var("PMR_MODEL_NETWORK").as_deref() != Ok("live")
            || model == "ep-af264-unresolved-pro"
            || base == "https://af264.invalid/v1"
        {
            return Err("AF264 live gate/model/base missing; no send".into());
        }
        let key = std::env::var("HUOSHAN_DEEPSEEK_API_KEY")
            .map_err(|_| "missing HUOSHAN_DEEPSEEK_API_KEY")?;
        if key.trim().is_empty() {
            return Err("empty key".into());
        }
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
    let mut report = evaluate_cases_with_review_ledger(
        &cases,
        limit,
        &mut |c| {
            let (http, key) = live.as_ref().ok_or("offline cannot send")?;
            let response = http
                .execute(smoke_http_request(http, c, key)?)
                .map_err(|e| e.to_string())?;
            model_provider::read_http_response(response, ProviderProtocol::OpenaiResponses)
                .map(|(_, v)| v)
                .map_err(|e| e.to_string())
        },
        false,
        &json!({"version":"recall.review@1","entries":[]}),
    )?;
    report["trial"] = json!("AF264");
    report["note"] = json!(
        "draft fictional labels; all semantic passes need manual review; raw/final separated"
    );
    m15_write_json(&Path::new(ROOT).join("report.json"), &report)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&report["summary"]).unwrap()
    );
    Ok(())
}

#[test]
fn af264_trial_freezes_eight_blind_slots_and_strict_resume() {
    let temp = TempRecords::new();
    let cases = plan(&temp.0, "ep-test", "https://af264.invalid/v1").unwrap();
    assert_eq!(cases.len(), CAP);
    assert!(!temp.0.exists());
    assert!(preflight(&cases, Some(7)).is_err());
    for c in &cases {
        assert_eq!(c.stage, "r4");
        assert_eq!(c.request["max_output_tokens"], 4096);
        assert_eq!(c.request["reasoning"]["effort"], "minimal");
        assert!(!matches!(c.replay, Replay::Rejected(_)));
        let input: Value = serde_json::from_str(c.request["input"].as_str().unwrap()).unwrap();
        assert!(input.get("expect").is_none());
        assert!(input.get("id").is_none());
    }
    let mut prior_prompt = cases[0].clone();
    prior_prompt.request["instructions"] = json!(prior_prompt.request["instructions"]
        .as_str().unwrap().replace("Copy original quotation characters verbatim; escape them only as required by JSON syntax.\n", ""));
    prior_prompt.wire_request =
        model_provider::provider_request(prior_prompt.provider.as_ref(), &prior_prompt.request)
            .unwrap();
    assert_ne!(case_fingerprint(&prior_prompt), case_fingerprint(&cases[0]));
    let report=evaluate_cases_with_review_ledger(&cases,Some(CAP),&mut |_|Ok(json!({"status":"completed","usage":{"input_tokens":2,"output_tokens":2},"output":[{"content":[{"type":"output_text","text":"{}"}]}]})),false,&Value::Null).unwrap();
    assert_eq!(report["sent_calls"], CAP);
    let resumed = plan(&temp.0, "ep-test", "https://af264.invalid/v1").unwrap();
    assert_eq!(preflight(&resumed, Some(CAP)).unwrap(), 0);
    let changed = plan(&temp.0, "ep-other", "https://af264.invalid/v1").unwrap();
    assert!(preflight(&changed, Some(CAP)).is_err());
}

#[test]
fn af264_interrupted_slot_is_not_resent_and_remaining_slots_keep_the_allowance() {
    let temp = TempRecords::new();
    let cases = plan(&temp.0, "ep-test", "https://af264.invalid/v1").unwrap();
    m15_write_json(
        &cases[0]
            .campaign
            .as_ref()
            .unwrap()
            .join("authorization.json"),
        &json!({"limit":CAP,"requests":cases.iter().map(campaign_slot).collect::<Vec<_>>()}),
    )
    .unwrap();
    campaign_start(&cases[0]).unwrap();
    let resumed = plan(&temp.0, "ep-test", "https://af264.invalid/v1").unwrap();
    assert!(matches!(resumed[0].replay, Replay::Interrupted));
    let report=evaluate_cases_with_review_ledger(&resumed,Some(CAP),&mut |c|{
        assert_ne!(campaign_slot(c),campaign_slot(&cases[0]));
        Ok(json!({"status":"completed","usage":{"input_tokens":2,"output_tokens":2},"output":[{"content":[{"type":"output_text","text":"{}"}]}]}))
    },false,&Value::Null).unwrap();
    assert_eq!(report["sent_calls"], CAP - 1);
    assert_eq!(report["results"][0]["status"], "未录制（中断在途）");
    assert_eq!(
        count_sends([cases[0].campaign.as_ref().unwrap().join("sends")]).unwrap(),
        CAP
    );
}
