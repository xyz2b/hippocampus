//! Isolated AF302 authorization: no changes to historic provider manifests.
use super::*;
const ROOT: &str = "target/eval-providers/compare-af302";
const IDS: [&str; 4] = ["ds-pro", "gpt-6-luna", "gpt-6.1-sol", "claude-haiku-5-5"];
const CAP: u64 = 20_000_000_000; // nano-CNY; exact integer accounting
fn rates(id: &str) -> (u64, u64, u64) {
    match id {
        "ds-pro" => (9000, 27000, 300),
        "gpt-6.1-sol" => (14200, 71000, 14200),
        _ => (710, 3550, 710),
    }
}
fn input_bound(c: &Case) -> u64 {
    // Conservative byte estimate, including schema and protocol framing allowance.
    serde_json::to_vec(&c.wire_request).unwrap().len() as u64 + 1024
}
fn upper(c: &Case) -> u64 {
    let (i, o, _) = rates(&c.model.label);
    input_bound(c) * i + c.request["max_output_tokens"].as_u64().unwrap() * o
}
fn actual(c: &Case, usage: &Value) -> Option<u64> {
    let input = usage["input_tokens"].as_u64()?;
    let output = usage["output_tokens"].as_u64()?;
    let cached = usage["input_tokens_details"]["cached_tokens"]
        .as_u64()
        .unwrap_or(0);
    if cached > input {
        return None;
    }
    let (i, o, cache) = rates(&c.model.label);
    Some((input - cached) * i + cached * cache + output * o)
}
fn key(c: &Case) -> String {
    hippocampus::recall_models::fingerprint(&campaign_slot(c))
}
fn read(path: &Path, default: Value) -> Result<Value, String> {
    match std::fs::read(path) {
        Ok(b) => serde_json::from_slice(&b).map_err(|e| e.to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(default),
        Err(e) => Err(e.to_string()),
    }
}
fn total(ledger: &Value) -> Result<u64, String> {
    ledger
        .as_object()
        .ok_or("invalid cost ledger")?
        .values()
        .try_fold(0u64, |n, e| {
            n.checked_add(e["charged_nano_cny"].as_u64().ok_or("invalid cost entry")?)
                .ok_or("cost overflow".into())
        })
}
const TRANSPORT: &str = "af308_transport_failed: ";
fn transport_error(error: &Value) -> bool {
    error
        .as_str()
        .is_some_and(|s| s.starts_with(TRANSPORT) || s.starts_with("error sending request"))
}
fn transport_log(log: &Value) -> bool {
    log["state"] == "responded"
        && transport_error(&log["error"])
        && log["usage"].is_null()
        && log["status"].is_null()
        && log["provider_http_error"].is_null()
}
fn transport_record(record: &Value) -> bool {
    transport_error(&record["error"])
        && record["usage"].is_null()
        && record["response"]["usage"].is_null()
        && record["response"]["status"].is_null()
        && record["provider_http_error"].is_null()
}
fn campaign_manifest(root: &Path) -> Result<Option<Value>, String> {
    let Some(base) = root.parent().and_then(Path::parent) else {
        return Ok(None);
    };
    let m = read(&base.join("manifest.json"), Value::Null)?;
    Ok((m["version"] == "af302.v1").then_some(m))
}
fn logs(root: &Path) -> Result<Vec<Value>, String> {
    let dir = root.join("sends");
    if !dir.exists() {
        return Ok(vec![]);
    }
    std::fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .filter_map(|e| match e {
            Ok(e) if e.path().extension().is_some_and(|s| s == "json") => {
                Some(read(&e.path(), Value::Null))
            }
            Ok(_) => None,
            Err(e) => Some(Err(e.to_string())),
        })
        .collect()
}
// Only AF302 opted-in retries may discount physical attempts from the slot quota.
pub(super) fn occupied_slots(root: &Path) -> Result<Option<usize>, String> {
    if campaign_manifest(root)?.is_none() {
        return Ok(None);
    }
    Ok(Some(
        logs(root)?
            .iter()
            .map(|l| l["slot"].to_string())
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
    ))
}
pub(super) fn retryable_send(case: &Case, log: &Value) -> Result<bool, String> {
    let Some(root) = &case.campaign else {
        return Ok(false);
    };
    let Some(m) = campaign_manifest(root)? else {
        return Ok(false);
    };
    let slot = campaign_slot(case);
    if !items(&m["profiles"])
        .iter()
        .any(|p| items(&p["slots"]).iter().any(|s| s["slot"] == slot))
    {
        return Ok(false);
    }
    let ledger = read(
        &root
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("cost-ledger.json"),
        Value::Null,
    )?;
    let entry = &ledger[key(case)];
    Ok(transport_log(log)
        && (entry["state"] == "transport_failed"
            || (entry["state"] == "reserved" && entry["retry_transport"] == true)))
}
fn settlement(c: &Case, usage: &Value, transport: bool) -> Value {
    let free = transport && usage.is_null();
    json!({"state":if free {"transport_failed"}else{"settled"},
        "charged_nano_cny":if free {0}else{actual(c,usage).unwrap_or(upper(c))},
        "upper_nano_cny":upper(c),"usage":usage})
}
fn enable_retries(groups: &mut [Vec<Case>], ledger: &Value) -> Result<(), String> {
    for c in groups.iter_mut().flatten() {
        if ledger[key(c)]["state"] != "transport_failed" {
            continue;
        }
        if !matches!(&c.replay,Replay::Failed(r) if transport_record(r)) {
            return Err("AF308 retry lacks matching transport recording".into());
        }
        let attempts: Vec<_> = logs(c.campaign.as_ref().unwrap())?
            .into_iter()
            .filter(|l| l["slot"] == campaign_slot(c))
            .collect();
        if attempts.is_empty() {
            return Err("AF308 retry missing send log".into());
        }
        for log in &attempts {
            if !retryable_send(c, log)? {
                return Err("AF308 retry has HTTP/inflight attempt".into());
            }
        }
        c.replay = Replay::Missing;
    }
    Ok(())
}
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
    let retry = ledger[key(c)]["state"] == "transport_failed";
    if ledger.get(key(c)).is_some() && !retry {
        return Err("cost slot already occupied".into());
    }
    if total(ledger)?.saturating_add(upper(c)) > CAP {
        return Ok(false);
    }
    ledger[key(c)] = json!({"state":"reserved","charged_nano_cny":upper(c),"upper_nano_cny":upper(c),"retry_transport":retry});
    Ok(true)
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
            return Err(format!("AF302 does not accept {name}"));
        }
    }
    let repeat = match var("EVAL_AF302_REPEAT").as_deref() {
        None | Some("1") => 1,
        Some("2") => 2,
        _ => return Err("repeat must be 1 or 2".into()),
    };
    let dry = var("EVAL_DRY_RUN").as_deref() == Some("1");
    let record = match var("EVAL_RECORD").as_deref() {
        None | Some("0") => false,
        Some("320") => true,
        _ => return Err("AF302 requires EVAL_RECORD=320 (cumulative authorization)".into()),
    };
    let root = Path::new(ROOT);
    let _lock = w3_trial_lock(&root.join("global-lock"))?;
    if var("EVAL_AF308_MIGRATE").as_deref() == Some("1") {
        println!("{}", migrate(root)?);
        return Ok(());
    }
    let mut groups = plan(root, var)?;
    let frozen = manifest(&groups);
    let path = root.join("manifest.json");
    if read(&path, frozen.clone())? != frozen {
        return Err("AF302 manifest changed; no sends permitted".into());
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
        return Err("AF302 sends exist but cost ledger is missing".into());
    }
    let mut ledger = read(&ledger_path, json!({}))?;
    total(&ledger)?;
    enable_retries(&mut groups, &ledger)?;
    let mut summary = Vec::new();
    for g in &groups {
        let (i, o, _) = rates(&g[0].model.label);
        summary.push(json!({"profile":g[0].model.label,"slots":g.len(),"selected":40,
            "pending":g.iter().filter(|c|c.repeat==repeat && matches!(c.replay,Replay::Missing) && (ledger.get(key(c)).is_none() || ledger[key(c)]["state"]=="transport_failed")).count(),
            "rejected":g.iter().filter(|c|matches!(c.replay,Replay::Rejected(_))).count(),
            "worst_cny":g.iter().map(upper).sum::<u64>() as f64/1e9,
            "scenario_cny":g.iter().map(|c|input_bound(c)/4*i+3000*o).sum::<u64>() as f64/1e9,
            "fingerprint":hippocampus::recall_models::fingerprint(&manifest(&[g.clone()]))}));
    }
    let dry_report = json!({"campaign":"AF302","fixed_slots":320,"selected_repeat":repeat,"selected_slots":160,
        "sent":0,"cap_cny":20,"charged_cny":total(&ledger)? as f64/1e9,"profiles":summary,
        "estimate_note":"scenario only: input bound / 4, output 3000 per call, no cache; not measured usage",
        "manifest_fingerprint":hippocampus::recall_models::fingerprint(&frozen)});
    m15_write_json(
        &root.join(format!("plan-repeat-{repeat}.json")),
        &dry_report,
    )?;
    println!("{}", serde_json::to_string_pretty(&dry_report).unwrap());
    if dry {
        return Ok(());
    }
    let selected: Vec<_> = groups
        .iter()
        .flatten()
        .filter(|c| c.repeat == repeat)
        .cloned()
        .collect();
    let keys = if record {
        smoke_credentials(&selected, var)?
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
        for original in g.iter().filter(|c| c.repeat == repeat) {
            let mut c = original.clone();
            if ledger.get(key(&c)).is_some()
                && ledger[key(&c)]["state"] != "transport_failed"
                && matches!(c.replay, Replay::Missing)
            {
                c.replay = Replay::Interrupted;
            }
            if matches!(c.replay, Replay::Stale | Replay::Rejected(_)) {
                return Err("AF302 stale/rejected request; no replacement send".into());
            }
            let circuits = provider_circuits(g)?;
            let send =
                record && matches!(c.replay, Replay::Missing) && !circuit_blocks(&c, &circuits);
            if send {
                if !reserve(&mut ledger, &c)? {
                    rows.push(json!({"id":c.sample["id"],"stage":c.stage,"status":"费用熔断未发送","repeat":repeat}));
                    continue;
                }
                m15_write_json(&ledger_path, &ledger)?;
            }
            let mut transport_failed = false;
            let r = evaluate_cases(
                &[c.clone()],
                if send { Some(80) } else { None },
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
        reports.push(json!({"profile":g[0].model.label,"charged_cny":g.iter().filter_map(|c|ledger[key(c)]["charged_nano_cny"].as_u64()).sum::<u64>() as f64/1e9,"stages":summaries(&rows),"results":rows,"cost":compare_cost(&g[0].model.label,&rows)}));
    }
    m15_write_json(
        &root.join(format!("report-repeat-{repeat}.json")),
        &json!({"profiles":reports,"cost_ledger":ledger,"charged_cny":total(&ledger)? as f64/1e9}),
    )?;
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
