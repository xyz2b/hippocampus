//! W4 comparison uses the AF302 runner and shared accounting, with independent authorization.
use super::cost_campaign::*;
use super::*;
pub(super) const ROOT: &str = "target/eval-providers/compare-af319";
pub(super) const CAP: u64 = 15_000_000_000;
const IDS: [&str; 3] = ["ds-pro", "gpt-6.1-sol", "gpt-6-luna"];

pub(super) fn plan(
    root: &Path,
    variable: impl Fn(&str) -> Option<String>,
) -> Result<Vec<Vec<Case>>, String> {
    let samples = samples("w4");
    if samples.len() != 28
        || samples
            .iter()
            .any(|s| s["label_status"] != "reviewed" || s["contract"] != "w4.v5")
    {
        return Err("AF319 requires the 28 reviewed W4 v5 samples".into());
    }
    let selected: Vec<_> = samples.iter().filter(|s| s["call_w4"] == true).collect();
    if selected.len() != 25 {
        return Err("AF319 requires 25 model-call samples (150 slots)".into());
    }
    let profiles = evaluation_profiles(include_str!("providers.json"))?;
    let mut groups = Vec::new();
    for id in IDS {
        let p = profiles
            .iter()
            .find(|p| p.id == id)
            .ok_or("missing AF319 profile")?;
        if p.effort != ReasoningEffort::Medium
            || p.max_output_tokens["w4"] != 16384
            || p.timeout_seconds != 360
        {
            return Err("AF319 freezes medium / 16384 / 360s".into());
        }
        let base = profile_base(p, &root.join("recordings").join(id), &variable)?;
        let model = EvalModel {
            label: id.into(),
            model: Some(p.model.clone()),
            base: Some(base),
            effort: p.effort,
        };
        model.validate()?;
        p.provider
            .endpoint(model.base.as_deref().unwrap())
            .map_err(|e| e.to_string())?;
        p.provider
            .check_model(&p.model, p.effort)
            .map_err(|e| e.to_string())?;
        let options = ProviderOptions {
            provider: p.provider.clone(),
            campaign: root.join("campaigns").join(id),
        };
        let mut cases = Vec::new();
        for sample in &selected {
            // W4 has neutral record/source IDs and no evaluator-owned sample identifier in its payload.
            let mut request = stage_request_with_output_tokens("w4", sample, 16384)?;
            request["model"] = json!(p.model);
            p.effort.apply(&mut request);
            for repeat in 1..=2 {
                let mut c = case_from_request_with_provider(
                    "w4".into(),
                    (*sample).clone(),
                    &root
                        .join("recordings")
                        .join(id)
                        .join(format!("repeat-{repeat}")),
                    &model,
                    repeat,
                    request.clone(),
                    Some(&options),
                    Some(360),
                )?;
                c.endpoint_env = p.endpoint_env.clone();
                cases.push(c);
            }
        }
        compare_mark_interrupted(&mut cases)?;
        groups.push(cases);
    }
    Ok(groups)
}
pub(super) fn manifest(groups: &[Vec<Case>]) -> Value {
    json!({"version":"af319.v1","slots":150,"budget_nano_cny":CAP,
        "input_estimate":"wire JSON UTF-8 bytes + 1024; no cache discount for reservation",
        "price_note":"AF302 frozen rates; proxy billing may differ",
        "profiles":groups.iter().map(|g|json!({"id":g[0].model.label,"effort":g[0].model.effort,
            "rates_nano_cny_per_token":rates(&g[0].model.label),"timeout_seconds":360,
            "slots":g.iter().map(|c|json!({"slot":campaign_slot(c),"request":case_fingerprint(c),
                "input_token_bound":input_bound(c),"upper_nano_cny":upper(c),
                "labels":hippocampus::recall_models::fingerprint(&json!({"expect":c.sample["expect"],
                    "label_revision":c.sample["label_revision"],"must_not":c.sample["must_not"]}))})).collect::<Vec<_>>() })).collect::<Vec<_>>()})
}
pub(super) fn summaries(rows: &[Value]) -> Value {
    let mut latency: Vec<_> = rows
        .iter()
        .filter_map(|r| r["elapsed_ms"].as_u64())
        .collect();
    latency.sort_unstable();
    let p = |percent: usize| {
        latency
            .get((latency.len() * percent).div_ceil(100).saturating_sub(1))
            .copied()
    };
    json!([{"stage":"w4","automatic_fields_passed":rows.iter().filter(|r|r["automatic_fields"]["status"]=="通过").count(),
        "runtime_passed":rows.iter().filter(|r|r["runtime_state_result"]["status"]=="通过").count(),
        "runtime_failed":rows.iter().filter(|r|r["runtime_state_result"]["status"]=="失败").count(),
        "final_automatic":stage_metrics(rows),
        "manual_pending":rows.iter().filter(|r|r["manual_review"]["status"]=="pending").count(),
        "latency_ms":{"count":latency.len(),"p50":p(50),"p90":p(90)},
        "usage":(["input_tokens","output_tokens"].map(|name|json!({"field":name,
            "known":rows.iter().filter(|r|r["usage"][name].is_u64()).count(),
            "sum":rows.iter().filter_map(|r|r["usage"][name].as_u64()).sum::<u64>()}))),
        "note":"after.recall / must_not require human review; no R4 or other model calls"}])
}
pub(super) fn combined(
    root: &Path,
    variable: impl Fn(&str) -> Option<String>,
) -> Result<(), String> {
    let mut profiles = Vec::new();
    for g in plan(root, variable)? {
        let r = evaluate_cases(&g, None, &mut |_| panic!("AF319 report attempted send"))?;
        let rows = items(&r["results"]);
        profiles.push(
            json!({"profile":g[0].model.label,"slots":50,"stages":summaries(rows),
            "cost":compare_cost(&g[0].model.label,rows),"results":rows}),
        );
    }
    m15_write_json(
        &root.join("report-combined-w4.json"),
        &json!({"sent_calls":0,"profiles":profiles}),
    )
}

#[test]
fn af319_plan_is_150_w4_slots_and_has_no_label_leak_or_persistent_manifest() {
    let temp = TempRecords::new();
    let groups = plan(&temp.0, |k| {
        k.ends_with("BASE_URL")
            .then(|| "https://example.test/v1".into())
    })
    .unwrap();
    assert_eq!(groups.iter().map(Vec::len).sum::<usize>(), 150);
    assert_eq!(
        groups.iter().flatten().filter(|c| c.repeat == 1).count(),
        75
    );
    let mut slots = std::collections::BTreeSet::new();
    for c in groups.iter().flatten() {
        assert_eq!(c.stage, "w4");
        assert_eq!(c.model.effort, ReasoningEffort::Medium);
        assert_eq!(c.request["max_output_tokens"], 16384);
        assert_eq!(c.timeout_seconds, Some(360));
        assert!(!c.sample["id"].as_str().unwrap().starts_with("w4_06"));
        assert!(!c.sample["id"].as_str().unwrap().starts_with("w4_08"));
        let payload = c.request["input"].as_str().unwrap();
        assert!(!payload.contains(c.sample["id"].as_str().unwrap()));
        for forbidden in ["eval_sample", "label_revision", "must_not", "expected"] {
            assert!(!payload.contains(forbidden));
        }
        assert!(slots.insert(json!([c.model.label, campaign_slot(c)]).to_string()));
    }
    for id in [
        "w4_14_complete_rejudge",
        "w4_n1a_negative_state_change",
        "w4_n1b_negative_correction",
        "w4_n2_sibling_fragments",
    ] {
        assert!(groups[0].iter().any(|c| c.sample["id"] == id));
    }
    assert_eq!(manifest(&groups)["budget_nano_cny"], CAP);
    assert!(!temp.0.exists());
    println!("AF319 synthetic dry plan: 3 profiles × 25 W4 × 2 = 150; repeat 1 = 75; sends = 0");
    for g in &groups {
        let (i, o, _) = rates(&g[0].model.label);
        println!(
            "{}: slots=50, medium, max_output=16384, worst={:.4} CNY, scenario={:.4} CNY, synthetic_fingerprint={}",
            g[0].model.label,
            g.iter().map(upper).sum::<u64>() as f64 / 1e9,
            g.iter()
                .map(|c| input_bound(c) / 4 * i + 3000 * o)
                .sum::<u64>() as f64
                / 1e9,
            hippocampus::recall_models::fingerprint(&manifest(&[g.clone()]))
        );
    }
}

#[test]
fn af319_transport_retry_keeps_15_yuan_budget_and_http_or_crash_reservations() {
    let temp = TempRecords::new();
    let variable = |k: &str| {
        k.ends_with("BASE_URL")
            .then(|| "https://example.test/v1".into())
    };
    let groups = plan(&temp.0, variable).unwrap();
    m15_write_json(&temp.0.join("manifest.json"), &manifest(&groups)).unwrap();
    for g in &groups {
        compare_freeze(g).unwrap();
    }
    let c = &groups[0][0];
    let mut ledger = json!({});
    assert!(reserve_with_cap(&mut ledger, c, CAP).unwrap());
    m15_write_json(&temp.0.join("cost-ledger.json"), &ledger).unwrap();
    evaluate_cases(&[c.clone()], Some(50), &mut |_| {
        Err(format!("{TRANSPORT}injected"))
    })
    .unwrap();
    ledger[key(c)] = settlement(c, &Value::Null, true);
    assert_eq!(total(&ledger).unwrap(), 0);
    m15_write_json(&temp.0.join("cost-ledger.json"), &ledger).unwrap();
    let mut resumed = plan(&temp.0, variable).unwrap();
    enable_retries(&mut resumed, &ledger).unwrap();
    let c = &resumed[0][0];
    assert!(reserve_with_cap(&mut ledger, c, CAP).unwrap());
    m15_write_json(&temp.0.join("cost-ledger.json"), &ledger).unwrap();
    evaluate_cases(&[c.clone()], Some(50), &mut |_| {
        Ok(json!({"provider_http_error":{"http_status":400,"error_type":"bad_request"}}))
    })
    .unwrap();
    ledger[key(c)] = settlement(c, &Value::Null, false);
    assert_eq!(total(&ledger).unwrap(), upper(c));
    m15_write_json(&temp.0.join("cost-ledger.json"), &ledger).unwrap();
    assert!(reserve_with_cap(&mut ledger, c, CAP).is_err());
    let mut after_response = plan(&temp.0, variable).unwrap();
    enable_retries(&mut after_response, &ledger).unwrap();
    assert!(!matches!(after_response[0][0].replay, Replay::Missing));
    assert_eq!(
        occupied_slots(c.campaign.as_ref().unwrap()).unwrap(),
        Some(1)
    );
    assert_eq!(
        items(&plan(&temp.0, variable).unwrap()[0][0].file["recorded"]).len(),
        2
    );
    let other = &resumed[0][1];
    assert!(reserve_with_cap(&mut ledger, other, CAP).unwrap());
    let persisted: Value = serde_json::from_str(&ledger.to_string()).unwrap();
    assert_eq!(persisted[key(other)]["charged_nano_cny"], upper(other));
    ledger["prior"] = json!({"charged_nano_cny":CAP-total(&ledger).unwrap()});
    assert!(!reserve_with_cap(&mut ledger, &resumed[0][2], CAP).unwrap());
    assert_eq!(total(&ledger).unwrap(), CAP);
    assert_eq!(
        actual(
            c,
            &json!({"input_tokens":100,"output_tokens":10,"input_tokens_details":{"cached_tokens":50}})
        ),
        Some(50 * 9000 + 50 * 300 + 10 * 27000)
    );
}

#[test]
fn af319_labels_refreeze_only_and_w4_reports_keep_raw_runtime_and_manual_separate() {
    let temp = TempRecords::new();
    let variable = |k: &str| {
        k.ends_with("BASE_URL")
            .then(|| "https://example.test/v1".into())
    };
    let mut groups = plan(&temp.0, variable).unwrap();
    let frozen = manifest(&groups);
    m15_write_json(&temp.0.join("manifest.json"), &frozen).unwrap();
    for c in groups.iter_mut().flatten() {
        c.sample["label_revision"] = json!("test-af319");
    }
    let revised = manifest(&groups);
    let mut bad = revised.clone();
    bad["budget_nano_cny"] = json!(CAP + 1);
    assert!(refreeze_labels(&temp.0, &bad, &groups, "test-af319").is_err());
    bad = revised.clone();
    bad["profiles"][0]["slots"][0]["request"] = json!("changed");
    assert!(refreeze_labels(&temp.0, &bad, &groups, "test-af319").is_err());
    assert_eq!(
        items(&refreeze_labels(&temp.0, &revised, &groups, "test-af319").unwrap()["changes"]).len(),
        150
    );
    assert_eq!(
        read(&temp.0.join("manifest.before-test-af319.json"), Value::Null).unwrap(),
        frozen
    );
    // Use a fixture through actual W4 assembly and SQLite runtime scoring, no provider.
    let c = &groups[0][0];
    compare_freeze(&groups[0]).unwrap();
    let fixture: Value = serde_json::from_slice(
        &std::fs::read(format!(
            "tests/eval/w4/fixtures/{}.output.json",
            c.sample["id"].as_str().unwrap()
        ))
        .unwrap(),
    )
    .unwrap();
    let report = evaluate_cases(&[c.clone()], Some(50), &mut |_| {
        Ok(fake_envelope(fixture.clone()))
    })
    .unwrap();
    let row = &report["results"][0];
    assert_eq!(row["automatic_fields"]["status"], "通过");
    assert_eq!(row["runtime_state_result"]["status"], "通过");
    assert_eq!(row["manual_review"]["status"], "pending");
    assert_eq!(row["recall_semantic_result"]["status"], "待人工复核");
    assert!(!row["raw_result"].is_null());
    combined(&temp.0, variable).unwrap();
    let merged = read(&temp.0.join("report-combined-w4.json"), Value::Null).unwrap();
    assert_eq!(merged["sent_calls"], 0);
    assert_eq!(items(&merged["profiles"][0]["results"]).len(), 50);
    assert_eq!(merged["profiles"][0]["stages"][0]["manual_pending"], 1);
}

pub(super) fn profile_selection(value: Option<&str>, repeat: usize) -> Result<Vec<String>, String> {
    let default = if repeat == 2 {
        "ds-pro,gpt-6.1-sol"
    } else {
        "ds-pro,gpt-6.1-sol,gpt-6-luna"
    };
    let ids: Vec<String> = value
        .unwrap_or(default)
        .split(',')
        .map(str::to_owned)
        .collect();
    let unique: std::collections::BTreeSet<_> = ids.iter().collect();
    if ids.len() != unique.len()
        || ids
            .iter()
            .any(|id| !IDS.contains(&id.as_str()) || (repeat == 2 && id == "gpt-6-luna"))
    {
        return Err(
            "AF322 profile selection invalid; repeat 2 permits only ds-pro,gpt-6.1-sol".into(),
        );
    }
    Ok(ids)
}

#[test]
fn af322_profile_subset_preserves_all_slots_and_exact_time_branches() {
    let temp = TempRecords::new();
    let groups = plan(&temp.0, |k| {
        k.ends_with("BASE_URL")
            .then(|| "https://example.test/v1".into())
    })
    .unwrap();
    let all = manifest(&groups);
    let selected = profile_selection(Some("ds-pro,gpt-6.1-sol"), 2).unwrap();
    assert_eq!(
        groups
            .iter()
            .flatten()
            .filter(|c| c.repeat == 2 && selected.contains(&c.model.label))
            .count(),
        50
    );
    assert_eq!(groups.iter().flatten().count(), 150);
    assert_eq!(manifest(&groups), all);
    for invalid in ["", "unknown", "ds-pro,ds-pro", "gpt-6-luna"] {
        assert!(profile_selection(Some(invalid), 2).is_err());
    }
    assert_eq!(profile_selection(None, 2).unwrap(), selected);
    for c in groups[0]
        .iter()
        .filter(|c| c.repeat == 1 && c.sample["label_revision"] == "AF322")
    {
        let fixture: Value = serde_json::from_slice(
            &std::fs::read(format!(
                "tests/eval/w4/fixtures/{}.output.json",
                c.sample["id"].as_str().unwrap()
            ))
            .unwrap(),
        )
        .unwrap();
        let mut output = fixture;
        output["decisions"][0]["effective"] =
            c.sample["expect"]["alternatives"][1]["decisions"][0]["effective"].clone();
        let report = score("w4", &c.sample, Some(&output.to_string()));
        assert_eq!(report["status"], "通过", "{}: {report}", c.sample["id"]);
        output["decisions"][0]["effective"]["quote"]["text"] = json!("fabricated");
        assert_eq!(
            score("w4", &c.sample, Some(&output.to_string()))["status"],
            "失败"
        );
        let mut original = c.sample.clone();
        original["expect"] = original["expect"]["alternatives"][0].clone();
        original["label_revision"] = json!("AF276");
        assert_eq!(
            stage_request_with_output_tokens("w4", &original, 16384).unwrap(),
            stage_request_with_output_tokens("w4", &c.sample, 16384).unwrap()
        );
    }
}

#[test]
fn af322_offline_first_repeat() {
    if std::env::var("EVAL_AF322_REPLAY").as_deref() != Ok("1") {
        return;
    }
    assert!(std::env::var_os("EVAL_RECORD").is_none(), "offline only");
    let root = Path::new(ROOT);
    let groups = plan(root, |_| None).unwrap();
    // Check every frozen request/config/rate/slot, without changing the real manifest.
    let temp = TempRecords::new();
    m15_write_json(
        &temp.0.join("manifest.json"),
        &read(&root.join("manifest.json"), Value::Null).unwrap(),
    )
    .unwrap();
    refreeze_labels(&temp.0, &manifest(&groups), &groups, "AF322").unwrap();
    let mut profiles = Vec::new();
    for group in &groups {
        let cases: Vec<_> = group.iter().filter(|c| c.repeat == 1).cloned().collect();
        assert_eq!(cases.len(), 25);
        assert!(
            cases.iter().all(|c| matches!(c.replay, Replay::Output(_))),
            "strict replay missing/stale"
        );
        let report =
            evaluate_cases(&cases, None, &mut |_| panic!("offline attempted send")).unwrap();
        let rows = items(&report["results"]);
        profiles.push(json!({"profile":group[0].model.label,"passed":rows.iter().filter(|r| r["status"]=="通过").count(),"stages":summaries(rows),"cost":compare_cost(&group[0].model.label,rows),"results":rows}));
    }
    m15_write_json(
        &root.join("reports-af322/report-repeat-1-w4.json"),
        &json!({"sent_calls":0,"label_revision":"AF322","profiles":profiles}),
    )
    .unwrap();
}
