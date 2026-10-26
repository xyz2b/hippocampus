//! AF272: withdrawn AF268 trial is read-only history; AF250 replay uses current R1 requests.
use super::*;
use std::collections::BTreeMap;

const ROOT: &str = "target/eval-r1-af268";
const BASELINE: &str = "target/eval-providers/recall-af228";
const REPORT: &str =
    "reports-af250/ds-pro--ds-flash--gpt-6.1-sol--gpt-6-luna--qwen38-flash-nothink/report.json";
const OUT: &str = "target/eval-r1-af272";

fn fictional() -> Vec<Value> {
    [
        ("g01", "我的纸艺素材盒里有什么？", "aggregate", "list", "self"),
        ("g02", "青禾组的成员是谁？", "aggregate", "list", "e-group"),
        ("g03", "我的留声机默认播放速度是多少？", "single", "lookup", "self"),
        ("g04", "我说过我拥有“微光”台灯吗？", "single", "lookup", "self"),
    ].into_iter().map(|(id, query, kind, op, subject)| {
        json!({"id":id,"contract":"r1.v2","label_status":"draft","label_revision":"AF270-draft@1",
            "split":"generalization",
            "input":{"message":query,"context":[],"said_on":"2026-10-11","task_id":null,
                "entities":[{"id":"self","names":["我"]},
                    {"id":"e-group","names":["青禾组"],"entity_type":"Organization"}],
                "relations":["fact","owns"]},
            "expect":{"need":"judge","current_evidence":"query_only","type":kind,"subject":subject,
                "conditions":[],"time":null,"plan.op":op,"plan.filter.relation":if matches!(id,"g01"|"g04"){
                    json!(["fact","owns",null])}else{json!(["fact",null])}},
            "note":"虚构泛化题；自动字段通过仍需人工复核，不要求凭记忆之外的知识作答。"})
    }).collect()
}

fn readonly_limit(value: Option<&str>) -> Result<(), String> {
    match value {
        None | Some("0") => Ok(()),
        _ => Err("AF272: prompt withdrawn; AF268/AF250 are offline only, EVAL_RECORD must be absent or 0".into()),
    }
}

fn read_json(path: &Path) -> Result<Value, String> {
    serde_json::from_slice(&std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?)
        .map_err(|e| format!("{}: {e}", path.display()))
}

fn check_history(row: &Value, record: &Value) -> Result<(), String> {
    let binding = &record["provider_binding"];
    let request = &binding["request"];
    let schema = request
        .pointer("/text/format/name")
        .or_else(|| request.pointer("/response_format/json_schema/name"));
    let cap = request
        .get("max_output_tokens")
        .or_else(|| request.get("max_completion_tokens"));
    if !binding.is_object()
        || row["request"] != json!(hippocampus::recall_models::fingerprint(binding))
        || record["request"] != row["request"]
        || record["output_text"] != row["raw_output_text"]
        || record["model"] != row["model"]
        || record["repeat"] != row["repeat"]
        || schema != Some(&record["schema"])
        || cap != Some(&record["max_output_tokens"])
    {
        return Err(format!(
            "historical recording/binding/output mismatch: {} repeat {}",
            row["id"], row["repeat"]
        ));
    }
    Ok(())
}

fn records(
    root: &Path,
    report: &Value,
    historical: bool,
) -> Result<Vec<(Value, Value, PathBuf)>, String> {
    let mut found = Vec::new();
    let mut slots = std::collections::BTreeSet::new();
    for row in items(&report["results"])
        .iter()
        .filter(|r| r["stage"] == "r1")
    {
        let id = row["id"].as_str().ok_or("sample ID")?;
        let repeat = row["repeat"].as_u64().ok_or("repeat")?;
        let model = row["model"].as_str().ok_or("profile")?;
        if !slots.insert((id, repeat, model)) {
            return Err("duplicate historical R1 slot".into());
        }
        let dir = if historical {
            root.join(format!("recordings/repeat-{repeat}"))
        } else {
            root.join(format!("recordings/{model}/repeat-{repeat}"))
        };
        let file = read_json(&dir.join(format!("{id}.json")))?;
        let matching: Vec<_> = items(&file["recorded"])
            .iter()
            .filter(|r| {
                r["request"] == row["request"] && r["model"] == model && r["repeat"] == repeat
            })
            .collect();
        if matching.len() != 1 {
            return Err("historical recording missing or ambiguous".into());
        }
        check_history(row, matching[0])?;
        found.push((row.clone(), matching[0].clone(), dir));
    }
    if found.len() != if historical { 48 } else { 200 } {
        return Err("historical R1 cohort size changed".into());
    }
    Ok(found)
}

fn current_case(row: &Value, record: &Value, dir: &Path, sample: Value) -> Result<Case, String> {
    let binding = &record["provider_binding"];
    let model = EvalModel {
        label: row["model"].as_str().ok_or("profile")?.into(),
        model: Some(binding["model"].as_str().ok_or("model")?.into()),
        base: Some(record["base_url"].as_str().ok_or("base URL")?.into()),
        effort: serde_json::from_value(binding["effort"].clone()).map_err(|e| e.to_string())?,
    };
    model.validate()?;
    let mut request = stage_request_with_output_tokens(
        "r1",
        &sample,
        u32::try_from(record["max_output_tokens"].as_u64().ok_or("output cap")?)
            .map_err(|_| "output cap exceeds u32")?,
    )?;
    request["model"] = json!(model.model);
    model.effort.apply(&mut request);
    let options = ProviderOptions {
        provider: serde_json::from_value(binding["provider"].clone()).map_err(|e| e.to_string())?,
        campaign: dir.join("unused-offline-campaign"),
    };
    let mut case = case_from_request_with_provider(
        "r1".into(),
        sample,
        dir,
        &model,
        row["repeat"].as_u64().ok_or("repeat")? as usize,
        request,
        Some(&options),
        binding["timeout_seconds"].as_u64(),
    )?;
    case.campaign = None;
    Ok(case)
}

pub(super) fn run() -> Result<(), String> {
    readonly_limit(std::env::var("EVAL_RECORD").ok().as_deref())?;
    match std::env::var("EVAL_AF268").as_deref() {
        Ok("trial" | "history") => {
            let root = Path::new(ROOT);
            let report = read_json(&root.join("report.json"))?;
            records(root, &report, true)?;
            println!("AF268：提示词已撤回，结果仅作历史；核对原录制48条；当前提示词不参与；发送0；不改写历史产物。\n{}",
                serde_json::to_string_pretty(&report["cohorts"]).unwrap());
            Ok(())
        }
        Ok("af250") => {
            let root = Path::new(BASELINE);
            let prior = read_json(&root.join(REPORT))?;
            let samples: BTreeMap<_, _> = samples("r1").into_iter()
                .map(|s| (s["id"].as_str().unwrap().to_owned(), s)).collect();
            let mut cases = Vec::new();
            let mut previous = Vec::new();
            for (row, record, dir) in records(root, &prior, false)? {
                let sample = samples.get(row["id"].as_str().ok_or("ID")?).ok_or("unknown R1 sample")?;
                cases.push(current_case(&row, &record, &dir, sample.clone())?);
                previous.push(row);
            }
            let mut report = evaluate_cases_with_review_ledger(&cases, None,
                &mut |_| panic!("AF272 offline replay cannot send"), false,
                &json!({"version":"recall.review@1","entries":[]}))?;
            let rows = items(&report["results"]);
            let models: std::collections::BTreeSet<_> = cases.iter().map(|c| &c.model.label).collect();
            let summary: Vec<_> = models.iter().map(|model| {
                let old: Vec<_> = previous.iter().filter(|r| r["model"] == **model).collect();
                let new: Vec<_> = rows.iter().filter(|r| r["model"] == **model).collect();
                json!({"profile":model,"total":new.len(),
                    "old_passed":old.iter().filter(|r| r["automatic_status"] == "通过").count(),
                    "new_passed":new.iter().filter(|r| r["automatic_status"] == "通过").count(),
                    "r1_03":old.iter().zip(&new).filter(|(r,_)|r["id"]=="r1_03_list_allergies")
                        .map(|(a,b)|json!({"repeat":b["repeat"],"old":a["automatic_status"],"new":b["automatic_status"],
                            "output":b["final_output"],"request":b["request"]})).collect::<Vec<_>>()})
            }).collect();
            report["comparison"] = json!("AF272 R1-only strict AF250 replay");
            report["label_revision"] = json!("AF272");
            report["old_new"] = json!(summary);
            report["note"] = json!("自动字段分数；不冒用旧人工确认；当前请求完整指纹严格核对，缺失/过期不计通过。AF250/AF268录制与原报告只读。");
            let out = Path::new(OUT);
            m15_write_json(&out.join("report.json"), &report)?;
            let mut md = String::from("# AF272 R1 离线重评\n\nAF250 200条原录制，0发送；自动字段通过不等于人工确认。\n\n|档案|旧通过|新通过|总数|\n|---|---:|---:|---:|\n");
            for s in &summary {
                md.push_str(&format!("|{}|{}|{}|{}|\n", s["profile"].as_str().unwrap(), s["old_passed"], s["new_passed"], s["total"]));
            }
            std::fs::write(out.join("report.md"), md).map_err(|e| e.to_string())?;
            println!("AF272 AF250 R1严格回放；发送 {}；报告 {OUT}/report.json\n{}",
                report["sent_calls"], serde_json::to_string_pretty(&summary).unwrap());
            Ok(())
        }
        _ => Err("EVAL_AF268 requires history/trial (read-only history) or af250 (strict current replay)".into()),
    }
}

#[test]
fn af272_history_has_no_current_prompt_dependency_and_rejects_tampering() {
    let binding = json!({"request":{"instructions":"withdrawn historical prompt",
        "text":{"format":{"name":"r1_v2"}},"max_output_tokens":4096}});
    let hash = hippocampus::recall_models::fingerprint(&binding);
    let record = json!({"provider_binding":binding,"request":hash,"model":"ds-pro","repeat":1,
        "output_text":"historical output","schema":"r1_v2","max_output_tokens":4096});
    let row = json!({"id":"g01","request":hash,"model":"ds-pro","repeat":1,"raw_output_text":"historical output"});
    assert!(check_history(&row, &record).is_ok());
    for path in [
        "/provider_binding/request/instructions",
        "/request",
        "/output_text",
        "/repeat",
    ] {
        let mut changed = record.clone();
        *changed.pointer_mut(path).unwrap() = json!("tampered");
        assert!(check_history(&row, &changed).is_err(), "{path}");
    }
    assert!(readonly_limit(None).is_ok());
    assert!(readonly_limit(Some("0")).is_ok());
    assert!(readonly_limit(Some("48")).is_err());
    assert!(readonly_limit(Some("1")).is_err());
    let mut chat = record.clone();
    chat["provider_binding"]["request"] = json!({
        "messages":[], "response_format":{"json_schema":{"name":"r1_v2"}},
        "max_completion_tokens":4096});
    let hash = json!(hippocampus::recall_models::fingerprint(
        &chat["provider_binding"]
    ));
    chat["request"] = hash.clone();
    let mut chat_row = row.clone();
    chat_row["request"] = hash;
    assert!(check_history(&chat_row, &chat).is_ok());
    chat["max_output_tokens"] = json!(16384);
    assert!(check_history(&chat_row, &chat).is_err());
}

#[test]
fn af272_af250_replay_is_strict_and_label_changes_do_not_change_requests() {
    let temp = TempRecords::new();
    let sample = fictional().remove(0);
    let row = json!({"model":"ds-pro","repeat":1});
    let record = json!({"base_url":"https://historical.invalid/v1","max_output_tokens":4096,
        "provider_binding":{"model":"ep-test","effort":"minimal","timeout_seconds":180,
            "provider":{"protocol":"openai_responses","base_url_env":"HUOSHAN_DEEPSEEK_BASE_URL","api_key_env":"HUOSHAN_DEEPSEEK_API_KEY"}}});
    let case = current_case(&row, &record, &temp.0, sample.clone()).unwrap();
    save_recording(
        &case,
        &fake_envelope(fake_r1(&sample)),
        Some(&fake_r1(&sample).to_string()),
        None,
    )
    .unwrap();
    let mut relabeled = sample.clone();
    relabeled["expect"] = json!({"alternatives":[sample["expect"].clone(),{"type":"single"}]});
    let replay = current_case(&row, &record, &temp.0, relabeled).unwrap();
    assert_eq!(case_fingerprint(&case), case_fingerprint(&replay));
    assert!(matches!(replay.replay, Replay::Output(_)));
    let mut altered = case.clone();
    altered.request["instructions"] = json!("withdrawn prompt");
    altered.wire_request =
        model_provider::provider_request(altered.provider.as_ref(), &altered.request).unwrap();
    assert_ne!(case_fingerprint(&altered), case_fingerprint(&case));
    std::fs::remove_file(&case.path).unwrap();
    save_recording(
        &altered,
        &fake_envelope(fake_r1(&sample)),
        Some(&fake_r1(&sample).to_string()),
        None,
    )
    .unwrap();
    let stale = current_case(&row, &record, &temp.0, sample).unwrap();
    assert!(matches!(stale.replay, Replay::Stale));
    let report = evaluate_cases_with_review_ledger(
        &[stale],
        None,
        &mut |_| panic!("offline cannot send"),
        false,
        &Value::Null,
    )
    .unwrap();
    assert_eq!(report["sent_calls"], 0);
    assert_ne!(report["results"][0]["automatic_status"], "通过");
}
