//! Isolated AF302 authorization: no changes to historic provider manifests.
use super::cost_campaign::*;
use super::*;
const ROOT: &str = "target/eval-providers/compare-af302";
const IDS: [&str; 4] = ["ds-pro", "gpt-6-luna", "gpt-6.1-sol", "claude-haiku-5-5"];
const CAP: u64 = 20_000_000_000; // nano-CNY; exact integer accounting
fn migrate(root: &Path) -> Result<Value, String> {
    let path = root.join("cost-ledger.json");
    let mut ledger = read(&path, Value::Null)?;
    let before = total(&ledger)?;
    let manifest = read(&root.join("manifest.json"), Value::Null)?;
    if manifest["version"] != "af302.v1" {
        return Err("AF308 requires existing AF302 manifest".into());
    }
    let mut changed = Vec::new();
    for profile in items(&manifest["profiles"]) {
        let id = profile["id"].as_str().ok_or("profile ID missing")?;
        let send_logs = logs(&root.join("campaigns").join(id))?;
        for frozen in items(&profile["slots"]) {
            let slot = &frozen["slot"];
            let key = hippocampus::recall_models::fingerprint(slot);
            if ledger[&key]["state"] != "settled" {
                continue;
            }
            let attempts: Vec<_> = send_logs.iter().filter(|l| l["slot"] == *slot).collect();
            if attempts.is_empty()
                || !attempts
                    .iter()
                    .all(|l| transport_log(l) && l["request"] == slot["request"])
            {
                continue;
            }
            let file = read(
                &root
                    .join("recordings")
                    .join(id)
                    .join(format!("repeat-{}", slot["repeat"]))
                    .join(format!(
                        "{}.json",
                        slot["id"].as_str().ok_or("sample ID missing")?
                    )),
                Value::Null,
            )?;
            let records: Vec<_> = items(&file["recorded"])
                .iter()
                .filter(|r| {
                    r["request"] == slot["request"]
                        && r["repeat"] == slot["repeat"]
                        && r["model"] == id
                })
                .collect();
            if records.len() != attempts.len() || !records.iter().all(|r| transport_record(r)) {
                return Err("AF308 send/recording mismatch; migration aborted".into());
            }
            if !ledger[&key]["usage"].is_null() {
                return Err("AF308 transport entry has usage".into());
            }
            ledger[&key]["state"] = json!("transport_failed");
            ledger[&key]["charged_nano_cny"] = json!(0);
            changed.push(key);
        }
    }
    if !changed.is_empty() {
        let backup = root.join("cost-ledger.before-af308.json");
        if backup.exists() {
            return Err("AF308 backup exists; refusing migration overwrite".into());
        }
        std::fs::copy(&path, &backup).map_err(|e| e.to_string())?;
        m15_write_json(&path, &ledger)?;
    }
    Ok(json!({"changed":changed.len(),"before_nano_cny":before,"after_nano_cny":total(&ledger)?}))
}
fn reserve(ledger: &mut Value, c: &Case) -> Result<bool, String> {
    reserve_with_cap(ledger, c, CAP)
}
fn plan(root: &Path, variable: impl Fn(&str) -> Option<String>) -> Result<Vec<Vec<Case>>, String> {
    let mut profiles = evaluation_profiles(include_str!("providers.json"))?;
    profiles
        .iter_mut()
        .find(|p| p.id == "ds-pro")
        .unwrap()
        .effort = ReasoningEffort::Low;
    compare_plan_for(root, &profiles, &IDS, variable)
}
fn manifest(groups: &[Vec<Case>]) -> Value {
    json!({"version":"af302.v1","slots":320,"budget_nano_cny":CAP,
        "input_estimate":"wire JSON UTF-8 bytes + 1024; no cache discount for reservation",
        "price_note":"AF303 frozen scenario rates, not proxy billing guarantee",
        "profiles":groups.iter().map(|g|json!({"id":g[0].model.label,"effort":g[0].model.effort,
            "rates_nano_cny_per_token":rates(&g[0].model.label),"timeout_seconds":360,
            "slots":g.iter().map(|c|json!({"slot":campaign_slot(c),"request":case_fingerprint(c),
                "input_token_bound":input_bound(c),"upper_nano_cny":upper(c),
                "labels":hippocampus::recall_models::fingerprint(&json!({"expect":c.sample["expect"],"label_revision":c.sample["label_revision"],"observation_only":c.sample["observation_only"]}))})).collect::<Vec<_>>() })).collect::<Vec<_>>()})
}
// Label amendments never change authorization, routing, or cost accounting.
fn refreeze(root: &Path, frozen: &Value, groups: &[Vec<Case>]) -> Result<Value, String> {
    refreeze_labels(root, frozen, groups, "AF311")
}
fn selected(c: &Case, repeat: usize, stage: Option<&str>) -> bool {
    c.repeat == repeat && stage.is_none_or(|stage| c.stage == stage)
}
fn selection(stage: Option<&str>, repeat: usize) -> Result<Option<&str>, String> {
    match (stage, repeat) {
        (Some("w1"), _) => Ok(Some("w1")),
        (None, 1) => Ok(None),
        _ => Err(
            "AF313 repeat 2 requires EVAL_AF302_STAGE=w1; other stages are not authorized".into(),
        ),
    }
}
fn combined_w1(root: &Path, groups: &[Vec<Case>]) -> Result<Value, String> {
    let mut profiles = Vec::new();
    for g in groups {
        let cases: Vec<_> = g.iter().filter(|c| c.stage == "w1").cloned().collect();
        // Reload strictly from disk after this run; never enable transport retries here.
        let mut cases = cases;
        for c in &mut cases {
            let options = c.provider.clone().map(|provider| ProviderOptions {
                provider,
                campaign: c.campaign.clone().unwrap(),
            });
            *c = case_from_request_with_provider(
                c.stage.clone(),
                c.sample.clone(),
                c.path.parent().unwrap(),
                &c.model,
                c.repeat,
                c.request.clone(),
                options.as_ref(),
                c.timeout_seconds,
            )?;
        }
        compare_mark_interrupted(&mut cases)?;
        let result = evaluate_cases(&cases, None, &mut |_| panic!("AF313 offline report sent"))?;
        let rows = items(&result["results"]);
        profiles.push(
            json!({"profile":g[0].model.label,"slots":40,"stages":summaries(rows),
            "results":rows,"cost":compare_cost(&g[0].model.label,rows)}),
        );
    }
    let report = json!({"label_revision":"AF311","sent_calls":0,"profiles":profiles});
    m15_write_json(&root.join("report-w1-combined-af311.json"), &report)?;
    Ok(report)
}
fn summaries(rows: &[Value]) -> Value {
    json!(["w1","w3"].map(|stage| {
        let selected:Vec<_>=rows.iter().filter(|r|r["stage"]==stage).cloned().collect();
        let mut ms:Vec<_>=selected.iter().filter_map(|r|r["elapsed_ms"].as_u64()).collect();
        ms.sort_unstable();
        let percentile=|p:usize|ms.get((ms.len()*p).div_ceil(100).saturating_sub(1)).copied();
        json!({"stage":stage,"automatic":stage_metrics(&selected),
            "observation":selected.iter().filter(|r|r["observation_only"]==true || r["status"]=="观察").collect::<Vec<_>>(),
            "latency_ms":{"count":ms.len(),"p50":percentile(50),"p90":percentile(90)},
            "usage": (["input_tokens","output_tokens"].map(|name|json!({"field":name,
                "known":selected.iter().filter(|r|r["usage"][name].is_u64()).count(),
                "sum":selected.iter().filter_map(|r|r["usage"][name].as_u64()).sum::<u64>()})))})
    }))
}
pub fn run() -> Result<(), String> {
    run_campaign(false)
}
pub(super) fn run_campaign(w4_only: bool) -> Result<(), String> {
    run_scoped_campaign(w4_only, false)
}
pub(super) fn run_scoped_campaign(w4_only: bool, scope_trial: bool) -> Result<(), String> {
    run_kind(if w4_only {
        Campaign::Af319
    } else if scope_trial {
        Campaign::Af342
    } else {
        Campaign::Af302
    })
}
#[derive(Clone, Copy)]
pub(super) enum Campaign {
    Af302,
    Af319,
    Af342,
    Af352,
}
pub(super) fn run_kind(kind: Campaign) -> Result<(), String> {
    let w4_only = matches!(kind, Campaign::Af319);
    let scope_trial = matches!(kind, Campaign::Af342);
    let effort_trial = matches!(kind, Campaign::Af352);
    let prefix = if effort_trial {
        "EVAL_AF352"
    } else if scope_trial {
        "EVAL_AF342"
    } else if w4_only {
        "EVAL_AF319"
    } else {
        "EVAL_AF302"
    };
    let cap = if effort_trial {
        af352::CAP
    } else if scope_trial {
        af342::CAP
    } else if w4_only {
        af319::CAP
    } else {
        CAP
    };
    let slots = if effort_trial {
        40
    } else if scope_trial {
        24
    } else if w4_only {
        150
    } else {
        320
    };
    let var = |k: &str| std::env::var(k).ok();
    for name in [
        "EVAL_COMPARE",
        "EVAL_MODEL",
        "EVAL_REASONING_EFFORT",
        "EVAL_PROVIDER_CONFIG",
        "EVAL_COMPARE_PROFILES",
        "EVAL_COMPARE_CASES",
        "EVAL_CAMPAIGN_DIR",
    ] {
        if var(name).is_some() {
            return Err(format!("{prefix} does not accept {name}"));
        }
    }
    let repeat = match var(&format!("{prefix}_REPEAT")).as_deref() {
        None | Some("1") => 1,
        Some("2") => 2,
        _ => return Err("repeat must be 1 or 2".into()),
    };
    if (scope_trial || effort_trial) && repeat != 1 {
        return Err(format!("{prefix} authorizes one repeat only"));
    }
    let stage_var = var(&format!("{prefix}_STAGE"));
    let stage = if w4_only {
        if !matches!(stage_var.as_deref(), None | Some("w4")) {
            return Err("AF319 only permits stage=w4".into());
        }
        Some("w4")
    } else {
        selection(stage_var.as_deref(), repeat)?
    };
    let profiles = if w4_only {
        af319::profile_selection(var("EVAL_AF319_PROFILES").as_deref(), repeat)?
    } else {
        Vec::new()
    };
    let selected = |c: &Case, repeat, stage| {
        selected(c, repeat, stage) && (!w4_only || profiles.iter().any(|p| p == &c.model.label))
    };
    let dry = var("EVAL_DRY_RUN").as_deref() == Some("1");
    let record = match var("EVAL_RECORD").as_deref() {
        None | Some("0") => false,
        Some(value) if value == slots.to_string() => true,
        _ => {
            return Err(format!(
                "{prefix} requires EVAL_RECORD={slots} (cumulative authorization)"
            ));
        }
    };
    let root = Path::new(if effort_trial {
        af352::ROOT
    } else if scope_trial {
        af342::ROOT
    } else if w4_only {
        af319::ROOT
    } else {
        ROOT
    });
    let _lock = w3_trial_lock(&root.join("global-lock"))?;
    if !w4_only
        && !scope_trial
        && !effort_trial
        && var("EVAL_AF308_MIGRATE").as_deref() == Some("1")
    {
        println!("{}", migrate(root)?);
        return Ok(());
    }
    let mut groups = if effort_trial {
        af352::plan(root, var)?
    } else if scope_trial {
        af342::plan(root, var)?
    } else if w4_only {
        af319::plan(root, var)?
    } else {
        plan(root, var)?
    };
    let frozen = if effort_trial {
        af352::manifest(&groups)
    } else if scope_trial {
        af342::manifest(&groups)
    } else if w4_only {
        af319::manifest(&groups)
    } else {
        manifest(&groups)
    };
    let path = root.join("manifest.json");
    if let Some(amendment) = var(&format!("{prefix}_REFREEZE")) {
        if scope_trial || (!w4_only && !effort_trial && amendment != "AF311") || !dry || record {
            return Err("label refreeze requires EVAL_DRY_RUN=1 and no EVAL_RECORD; AF302 only accepts AF311".into());
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&refreeze_labels(root, &frozen, &groups, &amendment)?)
                .unwrap()
        );
        return Ok(());
    }
    if read(&path, frozen.clone())? != frozen {
        return Err("campaign manifest changed; no sends permitted".into());
    }
    if !path.exists() {
        m15_write_json(&path, &frozen)?;
    }
    for g in &groups {
        compare_freeze(g)?;
    }
    let ledger_path = root.join("cost-ledger.json");
    if !ledger_path.exists()
        && groups
            .iter()
            .any(|g| count_sends([g[0].campaign.as_ref().unwrap().join("sends")]).unwrap_or(1) > 0)
    {
        return Err("campaign sends exist but cost ledger is missing".into());
    }
    let mut ledger = read(&ledger_path, json!({}))?;
    total(&ledger)?;
    enable_retries(&mut groups, &ledger)?;
    let mut summary = Vec::new();
    for g in &groups {
        let (i, o, _) = rates(&g[0].model.label);
        summary.push(json!({"profile":g[0].model.label,"slots":g.len(),"selected":g.iter().filter(|c|selected(c,repeat,stage)).count(),
            "pending":g.iter().filter(|c|selected(c,repeat,stage) && matches!(c.replay,Replay::Missing) && (ledger.get(key(c)).is_none() || ledger[key(c)]["state"]=="transport_failed")).count(),
            "rejected":g.iter().filter(|c|matches!(c.replay,Replay::Rejected(_))).count(),
            "worst_cny":g.iter().filter(|c|selected(c,repeat,stage)).map(upper).sum::<u64>() as f64/1e9,
            "scenario_cny":g.iter().filter(|c|selected(c,repeat,stage)).map(|c|input_bound(c)/4*i+3000*o).sum::<u64>() as f64/1e9,
            "fingerprint":hippocampus::recall_models::fingerprint(&if effort_trial {af352::manifest(&[g.clone()])} else if scope_trial {af342::manifest(&[g.clone()])} else if w4_only {af319::manifest(&[g.clone()])} else {manifest(&[g.clone()])})}));
    }
    let dry_report = json!({"campaign":if effort_trial {"AF352"} else if scope_trial {"AF342"} else if w4_only {"AF319"} else {"AF302"},"fixed_slots":slots,"selected_repeat":repeat,"selected_stage":stage,"selected_profiles":profiles,"selected_slots":groups.iter().flatten().filter(|c|selected(c,repeat,stage)).count(),
        "sent":0,"cap_cny":cap as f64/1e9,"charged_cny":total(&ledger)? as f64/1e9,"profiles":summary,
        "estimate_note":"scenario only: input bound / 4, output 3000 per call, no cache; not measured usage",
        "manifest_fingerprint":hippocampus::recall_models::fingerprint(&frozen)});
    let suffix = stage.map(|s| format!("-{s}")).unwrap_or_default();
    m15_write_json(
        &root.join(format!("plan-repeat-{repeat}{suffix}.json")),
        &dry_report,
    )?;
    println!("{}", serde_json::to_string_pretty(&dry_report).unwrap());
    if dry {
        return Ok(());
    }
    let selected_cases: Vec<_> = groups
        .iter()
        .flatten()
        .filter(|c| selected(c, repeat, stage))
        .cloned()
        .collect();
    let keys = if record {
        smoke_credentials(&selected_cases, var)?
    } else {
        Default::default()
    };
    let http = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .build()
        .map_err(|e| e.to_string())?;
    let mut reports = Vec::new();
    for g in &groups {
        let mut rows = Vec::new();
        for original in g.iter().filter(|c| selected(c, repeat, stage)) {
            let mut c = original.clone();
            if ledger.get(key(&c)).is_some()
                && ledger[key(&c)]["state"] != "transport_failed"
                && matches!(c.replay, Replay::Missing)
            {
                c.replay = Replay::Interrupted;
            }
            if matches!(c.replay, Replay::Stale | Replay::Rejected(_)) {
                return Err("campaign stale/rejected request; no replacement send".into());
            }
            let circuits = provider_circuits(g)?;
            let send =
                record && matches!(c.replay, Replay::Missing) && !circuit_blocks(&c, &circuits);
            if send {
                if !reserve_with_cap(&mut ledger, &c, cap)? {
                    rows.push(json!({"id":c.sample["id"],"stage":c.stage,"status":"费用熔断未发送","repeat":repeat}));
                    continue;
                }
                m15_write_json(&ledger_path, &ledger)?;
            }
            let mut transport_failed = false;
            let r = evaluate_cases(
                &[c.clone()],
                if send { Some(g.len()) } else { None },
                &mut |case| {
                    let request = smoke_http_request(&http, case, &keys[&case.model.label])?;
                    let response = http.execute(request).map_err(|e| {
                        transport_failed = true;
                        format!("{TRANSPORT}{e}")
                    })?;
                    model_provider::read_http_response(
                        response,
                        case.provider.as_ref().unwrap().protocol,
                    )
                    .map(|(_, e)| e)
                    .map_err(|e| e.to_string())
                },
            )?;
            if send {
                let usage = &r["results"][0]["usage"];
                ledger[key(&c)] = settlement(&c, usage, transport_failed);
                m15_write_json(&ledger_path, &ledger)?;
            }
            rows.extend(items(&r["results"]).iter().cloned());
        }
        reports.push(json!({"profile":g[0].model.label,"charged_cny":g.iter().filter_map(|c|ledger[key(c)]["charged_nano_cny"].as_u64()).sum::<u64>() as f64/1e9,"stages":if effort_trial {af352::summaries(&rows)} else if w4_only {af319::summaries(&rows)} else {summaries(&rows)},"results":rows,"cost":compare_cost(&g[0].model.label,&rows)}));
    }
    let label_revisions = groups
        .iter()
        .flatten()
        .filter_map(|c| c.sample["label_revision"].as_str())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(",");
    if effort_trial {
        af352::write_comparison(root, &groups, &reports)?;
    }
    m15_write_json(
        &root.join(format!("report-repeat-{repeat}{suffix}.json")),
        &json!({"selected_repeat":repeat,"selected_stage":stage,"label_revision":if w4_only || scope_trial || effort_trial {label_revisions.as_str()} else {"AF311"},"profiles":reports,"cost_ledger":ledger,"charged_cny":total(&ledger)? as f64/1e9}),
    )?;
    if !scope_trial && !effort_trial && stage == Some("w1") {
        combined_w1(root, &groups)?;
    }
    if w4_only {
        af319::combined(root, var)?;
    }
    Ok(())
}
#[test]
fn af302_budget_reservation_and_missing_usage_are_conservative() {
    let dir = std::env::temp_dir().join(format!("af302-{}", hippocampus::diagnostics::new_id()));
    let groups = plan(&dir, |k| {
        if k.ends_with("BASE_URL") {
            Some("https://example.test/v1".into())
        } else {
            None
        }
    })
    .unwrap();
    assert_eq!(groups.iter().map(Vec::len).sum::<usize>(), 320);
    assert_eq!(groups[0][0].model.effort, ReasoningEffort::Low);
    assert!(
        groups[1..]
            .iter()
            .all(|g| g[0].model.effort == ReasoningEffort::Medium)
    );
    let c = &groups[0][0];
    let mut ledger = json!({});
    assert!(reserve(&mut ledger, c).unwrap());
    assert!(reserve(&mut ledger, c).is_err());
    assert_eq!(actual(c, &Value::Null), None);
    let copy: Value = serde_json::from_str(&ledger.to_string()).unwrap();
    assert_eq!(total(&copy).unwrap(), upper(c));
    ledger["other"] = json!({"charged_nano_cny":CAP-total(&ledger).unwrap()});
    assert!(!reserve(&mut ledger, &groups[0][1]).unwrap());
    assert_eq!(total(&ledger).unwrap(), CAP);
    assert_eq!(
        actual(
            c,
            &json!({"input_tokens":100,"output_tokens":20,"input_tokens_details":{"cached_tokens":50}})
        ),
        Some(50 * 9000 + 50 * 300 + 20 * 27000)
    );
}

#[test]
fn af302_frozen_requests_replay_failed_calls_and_keep_repeat_two_separate() {
    let dir = std::env::temp_dir().join(format!("af302-{}", hippocampus::diagnostics::new_id()));
    let variable = |k: &str| {
        if k.ends_with("BASE_URL") {
            Some("https://example.test/v1".into())
        } else {
            None
        }
    };
    let groups = plan(&dir, variable).unwrap();
    for g in &groups {
        compare_freeze(g).unwrap();
    }
    assert_eq!(
        groups.iter().flatten().filter(|c| c.repeat == 1).count(),
        160
    );
    assert_eq!(
        groups.iter().flatten().filter(|c| c.repeat == 2).count(),
        160
    );
    let before = manifest(&groups);
    let c = &groups[0][0];
    let mut ledger = json!({});
    assert!(reserve(&mut ledger, c).unwrap());
    m15_write_json(&dir.join("cost.json"), &ledger).unwrap();
    let result = evaluate_cases(&[c.clone()], Some(80), &mut |_| {
        Ok(json!({"provider_http_error":{"http_status":400,"error_type":"invalid_request_error"}}))
    })
    .unwrap();
    assert_eq!(result["sent_calls"], 1);
    assert!(actual(c, &result["results"][0]["usage"]).is_none());
    assert_eq!(
        total(&read(&dir.join("cost.json"), Value::Null).unwrap()).unwrap(),
        upper(c)
    );
    let next = plan(&dir, variable).unwrap();
    assert_eq!(before, manifest(&next));
    assert!(matches!(next[0][0].replay, Replay::Failed(_)));
    let r = evaluate_cases(&[next[0][0].clone()], Some(80), &mut |_| {
        panic!("must not resend")
    })
    .unwrap();
    assert_eq!(r["sent_calls"], 0);
    let mut changed = next[0][1].clone();
    changed.request["max_output_tokens"] = json!(123);
    changed.wire_request["max_output_tokens"] = json!(123);
    assert!(campaign_check(&[changed], 80).is_err());
    let missing = &groups[0][1];
    campaign_start(missing).unwrap();
    let resumed = plan(&dir, variable).unwrap();
    assert!(matches!(resumed[0][1].replay, Replay::Interrupted));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn af308_transport_retry_appends_and_http_or_interruption_cannot_retry() {
    let dir = std::env::temp_dir().join(format!("af308-{}", hippocampus::diagnostics::new_id()));
    let variable = |k: &str| {
        k.ends_with("BASE_URL")
            .then(|| "https://example.test/v1".into())
    };
    let groups = plan(&dir, variable).unwrap();
    m15_write_json(&dir.join("manifest.json"), &manifest(&groups)).unwrap();
    for g in &groups {
        compare_freeze(g).unwrap();
    }
    let c = &groups[0][0];
    let mut ledger = json!({});
    assert!(reserve(&mut ledger, c).unwrap());
    m15_write_json(&dir.join("cost-ledger.json"), &ledger).unwrap();
    let failed = evaluate_cases(&[c.clone()], Some(80), &mut |_| {
        Err(format!("{TRANSPORT}synthetic disconnect"))
    })
    .unwrap();
    assert_eq!(failed["sent_calls"], 1);
    ledger[key(c)] = settlement(c, &Value::Null, true);
    assert_eq!(total(&ledger).unwrap(), 0);
    m15_write_json(&dir.join("cost-ledger.json"), &ledger).unwrap();
    let mut resumed = plan(&dir, variable).unwrap();
    enable_retries(&mut resumed, &ledger).unwrap();
    let retry = &resumed[0][0];
    assert!(matches!(retry.replay, Replay::Missing));
    assert!(reserve(&mut ledger, retry).unwrap());
    m15_write_json(&dir.join("cost-ledger.json"), &ledger).unwrap();
    let response = evaluate_cases(&[retry.clone()], Some(80), &mut |_| {
        Ok(json!({"provider_http_error":{"http_status":400,"error_type":"bad_request"}}))
    })
    .unwrap();
    assert_eq!(response["sent_calls"], 1);
    ledger[key(c)] = settlement(c, &Value::Null, false);
    assert_eq!(total(&ledger).unwrap(), upper(c));
    m15_write_json(&dir.join("cost-ledger.json"), &ledger).unwrap();
    let mut final_plan = plan(&dir, variable).unwrap();
    enable_retries(&mut final_plan, &ledger).unwrap();
    let http_case = &final_plan[0][0];
    assert_eq!(items(&http_case.file["recorded"]).len(), 2);
    assert_eq!(
        occupied_slots(c.campaign.as_ref().unwrap()).unwrap(),
        Some(1)
    );
    let r = evaluate_cases(&[http_case.clone()], Some(80), &mut |_| {
        panic!("HTTP response must not retry")
    })
    .unwrap();
    assert_eq!(r["sent_calls"], 0);
    // A crash after reservation/start still consumes the upper bound.
    let pending = &final_plan[0][1];
    assert!(reserve(&mut ledger, pending).unwrap());
    m15_write_json(&dir.join("cost-ledger.json"), &ledger).unwrap();
    campaign_start(pending).unwrap();
    let mut final_plan = plan(&dir, variable).unwrap();
    enable_retries(&mut final_plan, &ledger).unwrap();
    assert!(matches!(final_plan[0][1].replay, Replay::Interrupted));
    assert_eq!(ledger[key(pending)]["charged_nano_cny"], upper(pending));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn af308_migration_checks_recording_and_backs_up_exact_ledger() {
    let dir = std::env::temp_dir().join(format!(
        "af308-migrate-{}",
        hippocampus::diagnostics::new_id()
    ));
    let groups = plan(&dir, |k| {
        k.ends_with("BASE_URL")
            .then(|| "https://example.test/v1".into())
    })
    .unwrap();
    m15_write_json(&dir.join("manifest.json"), &manifest(&groups)).unwrap();
    for g in &groups {
        compare_freeze(g).unwrap();
    }
    let c = &groups[0][0];
    evaluate_cases(&[c.clone()], Some(80), &mut |_| {
        Err("error sending request (synthetic)".into())
    })
    .unwrap();
    let mut ledger = json!({});
    ledger[key(c)] = settlement(c, &Value::Null, false);
    m15_write_json(&dir.join("cost-ledger.json"), &ledger).unwrap();
    let original = std::fs::read(dir.join("cost-ledger.json")).unwrap();
    let result = migrate(&dir).unwrap();
    assert_eq!(result["changed"], 1);
    assert_eq!(result["after_nano_cny"], 0);
    assert_eq!(
        std::fs::read(dir.join("cost-ledger.before-af308.json")).unwrap(),
        original
    );
    assert_eq!(migrate(&dir).unwrap()["changed"], 0);
    assert!(!transport_log(
        &json!({"state":"responded","error":"error sending request","provider_http_error":{"http_status":500}})
    ));
    assert!(!transport_log(
        &json!({"state":"started","error":"error sending request"})
    ));
    assert!(!transport_log(
        &json!({"state":"responded","error":"error sending request","usage":{"input_tokens":1}})
    ));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn af313_refreeze_only_labels_keeps_backup_budget_and_request_identity() {
    let temp = TempRecords::new();
    let mut groups = plan(&temp.0, |k| {
        k.ends_with("BASE_URL")
            .then(|| "https://example.test/v1".into())
    })
    .unwrap();
    // This tests the historical AF311 amendment, independent of current sample labels.
    for c in groups.iter_mut().flatten() {
        c.sample["label_revision"] = json!("AF311");
    }
    let frozen = manifest(&groups);
    let mut old = frozen.clone();
    for p in old["profiles"].as_array_mut().unwrap() {
        for s in p["slots"].as_array_mut().unwrap() {
            if s["slot"]["id"].as_str().unwrap().starts_with("w1_05") {
                s["labels"] = json!("old AF310 label digest");
            }
        }
    }
    m15_write_json(&temp.0.join("manifest.json"), &old).unwrap();
    let original = std::fs::read(temp.0.join("manifest.json")).unwrap();
    let ledger = json!({"prior":{"charged_nano_cny":4_603_130_430u64,"state":"settled"}});
    m15_write_json(&temp.0.join("cost-ledger.json"), &ledger).unwrap();
    let ledger_bytes = std::fs::read(temp.0.join("cost-ledger.json")).unwrap();
    for pointer in [
        "/budget_nano_cny",
        "/slots",
        "/profiles/0/effort",
        "/profiles/0/rates_nano_cny_per_token/0",
        "/profiles/0/slots/0/request",
        "/profiles/0/slots/0/slot/repeat",
        "/profiles/0/slots/0/input_token_bound",
    ] {
        let mut changed = frozen.clone();
        *changed.pointer_mut(pointer).unwrap() = json!("changed");
        assert!(refreeze(&temp.0, &changed, &groups).is_err(), "{pointer}");
        assert_eq!(
            std::fs::read(temp.0.join("manifest.json")).unwrap(),
            original
        );
        assert!(!temp.0.join("manifest.before-af311.json").exists());
    }
    let report = refreeze(&temp.0, &frozen, &groups).unwrap();
    assert_eq!(items(&report["changes"]).len(), 8);
    assert!(
        items(&report["changes"])
            .iter()
            .all(|c| c["after_label_revision"] == "AF311")
    );
    assert_eq!(
        std::fs::read(temp.0.join("manifest.before-af311.json")).unwrap(),
        original
    );
    assert_eq!(
        std::fs::read(temp.0.join("cost-ledger.json")).unwrap(),
        ledger_bytes
    );
    assert_eq!(
        read(&temp.0.join("manifest.json"), Value::Null).unwrap(),
        frozen
    );
    assert!(items(&refreeze(&temp.0, &frozen, &groups).unwrap()["changes"]).is_empty());
    let mut near_cap = ledger;
    near_cap["other"] = json!({"charged_nano_cny":CAP-total(&near_cap).unwrap()});
    assert!(!reserve(&mut near_cap, &groups[0][0]).unwrap());
}

#[test]
fn af313_w1_second_repeat_has_80_slots_and_combined_report_rescores_both() {
    let temp = TempRecords::new();
    let groups = plan(&temp.0, |k| {
        k.ends_with("BASE_URL")
            .then(|| "https://example.test/v1".into())
    })
    .unwrap();
    assert!(selection(None, 2).is_err());
    assert!(selection(Some("w3"), 2).is_err());
    let stage = selection(Some("w1"), 2).unwrap();
    let chosen: Vec<_> = groups
        .iter()
        .flatten()
        .filter(|c| selected(c, 2, stage))
        .collect();
    assert_eq!(chosen.len(), 80);
    assert!(chosen.iter().all(|c| c.stage == "w1" && c.repeat == 2));
    let g = &groups[0];
    compare_freeze(g).unwrap();
    // A first-repeat recording is rescored using today's labels; old reports are not read.
    for repeat in [1, 2] {
        let c = g
            .iter()
            .find(|c| c.repeat == repeat && c.sample["id"].as_str().unwrap().starts_with("w1_05"))
            .unwrap();
        let output = fake_w1(&c.sample);
        let mut recorded_case = c.clone();
        if repeat == 1 {
            // Only the old label differs; requests and outputs remain strictly reusable.
            recorded_case.sample["expect"] =
                json!({"candidate_count":0,"candidates":[],"memory_requests":[]});
        }
        let recorded = evaluate_cases(&[recorded_case], Some(80), &mut |_| {
            Ok(fake_envelope(output.clone()))
        })
        .unwrap();
        assert_eq!(
            recorded["results"][0]["status"],
            if repeat == 1 { "失败" } else { "通过" }
        );
    }
    let report = combined_w1(&temp.0, &groups).unwrap();
    assert_eq!(report["sent_calls"], 0);
    for p in items(&report["profiles"]) {
        assert_eq!(items(&p["results"]).len(), 40);
        assert!(items(&p["results"]).iter().all(|r| r["stage"] == "w1"));
    }
    assert_eq!(
        items(&report["profiles"][0]["results"])
            .iter()
            .filter(|r| r["status"] == "通过")
            .count(),
        2
    );
    assert_eq!(logs(g[0].campaign.as_ref().unwrap()).unwrap().len(), 2);
    assert!(
        logs(g[0].campaign.as_ref().unwrap())
            .unwrap()
            .iter()
            .all(|l| l["slot"]["stage"] == "w1")
    );
}
