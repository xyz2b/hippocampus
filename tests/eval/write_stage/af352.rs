//! AF352: paired effort screen, current prompts, no implicit model/endpoint defaults.
use super::cost_campaign::*;
use super::*;
pub(super) const ROOT: &str = "target/eval-providers/af352";
pub(super) const CAP: u64 = 10_000_000_000;
fn selected_samples() -> Vec<(String, Value)> {
    let scope_w1: Vec<Value> = serde_json::from_str(include_str!("af342/w1.json")).unwrap();
    let scope_w3: Vec<Value> = serde_json::from_str(include_str!("af342/w3.json")).unwrap();
    let mut out: Vec<_> = scope_w1
        .into_iter()
        .filter(|s| {
            [
                "af342_w1_01",
                "af342_w1_02",
                "af342_w1_03",
                "af342_w1_04",
                "af342_w1_06",
                "af342_w1_08",
                "af342_w1_10",
                "af342_w1_11",
            ]
            .contains(&s["id"].as_str().unwrap())
        })
        .map(|s| ("w1".into(), s))
        .collect();
    out.extend(
        samples("w1")
            .into_iter()
            .filter(|s| {
                [
                    "w1_07_plan",
                    "w1_12_unresolved_subject",
                    "w1_13_history_disambiguates",
                    "w1_18_pure_question",
                ]
                .contains(&s["id"].as_str().unwrap())
            })
            .map(|s| ("w1".into(), s)),
    );
    let subject: Vec<Value> = serde_json::from_str(include_str!("af352/w3-subject.json")).unwrap();
    out.extend(subject.into_iter().map(|s| ("w3".into(), s)));
    out.extend(
        samples("w3")
            .into_iter()
            .filter(|s| {
                ["w3_02_dropped_uncertainty", "w3_03_dropped_condition"]
                    .contains(&s["id"].as_str().unwrap())
            })
            .map(|s| ("w3".into(), s)),
    );
    out.extend(
        scope_w3
            .into_iter()
            .filter(|s| ["af342_w3_11", "af342_w3_12"].contains(&s["id"].as_str().unwrap()))
            .map(|s| ("w3".into(), s)),
    );
    out
}
pub(super) fn plan(
    root: &Path,
    variable: impl Fn(&str) -> Option<String>,
) -> Result<Vec<Vec<Case>>, String> {
    let mut p = evaluation_profiles(include_str!("providers.json"))?
        .into_iter()
        .find(|p| p.id == "ds-pro")
        .unwrap();
    p.model = variable("DEEPSEEK_PRO_MODEL")
        .filter(|v| !v.trim().is_empty())
        .ok_or("AF352 requires production DEEPSEEK_PRO_MODEL")?;
    if !p.model.starts_with("ep-") {
        return Err("AF352 requires a Huoshan production endpoint ID (ep-...)".into());
    }
    let base = variable("HUOSHAN_DEEPSEEK_BASE_URL")
        .filter(|v| !v.trim().is_empty())
        .ok_or("AF352 requires HUOSHAN_DEEPSEEK_BASE_URL; no placeholder manifest")?;
    let selected = selected_samples();
    if selected.iter().filter(|(s, _)| s == "w1").count() != 12
        || selected.iter().filter(|(s, _)| s == "w3").count() != 8
    {
        return Err("AF352 requires 12 W1 + 8 W3".into());
    }
    let mut groups = Vec::new();
    for (label, effort) in [
        ("ds-pro-minimal", ReasoningEffort::Minimal),
        ("ds-pro-low", ReasoningEffort::Low),
    ] {
        let model = EvalModel {
            label: label.into(),
            model: Some(p.model.clone()),
            base: Some(base.clone()),
            effort,
        };
        model.validate()?;
        let options = ProviderOptions {
            provider: p.provider.clone(),
            campaign: root.join("campaigns").join(label),
        };
        let mut group = Vec::new();
        for (stage, sample) in &selected {
            let mut request =
                compare_request(stage, sample, if stage == "w1" { 32768 } else { 16384 })?;
            request["model"] = json!(p.model);
            effort.apply(&mut request);
            let mut case = case_from_request_with_provider(
                stage.clone(),
                sample.clone(),
                &root.join("recordings").join(label).join("repeat-1"),
                &model,
                1,
                request,
                Some(&options),
                Some(360),
            )?;
            case.endpoint_env = Some("HUOSHAN_DEEPSEEK_BASE_URL".into());
            group.push(case);
        }
        compare_mark_interrupted(&mut group)?;
        groups.push(group);
    }
    Ok(groups)
}
pub(super) fn manifest(groups: &[Vec<Case>]) -> Value {
    json!({"version":"af352.v1","slots":40,"budget_nano_cny":CAP,
        "input_estimate":"wire JSON UTF-8 bytes + 1024; no cache discount in reservation",
        "profiles":groups.iter().map(|g|json!({"id":g[0].model.label,"effort":g[0].model.effort,"timeout_seconds":360,"rates_nano_cny_per_token":rates(&g[0].model.label),
            "slots":g.iter().map(|c|json!({"slot":campaign_slot(c),"request":case_fingerprint(c),"input_token_bound":input_bound(c),"upper_nano_cny":upper(c),
                "labels":hippocampus::recall_models::fingerprint(&json!({"expect":c.sample["expect"],"label_revision":c.sample["label_revision"],"label_status":c.sample["label_status"],"must_not":c.sample["must_not"]}))})).collect::<Vec<_>>() })).collect::<Vec<_>>()})
}
pub(super) fn summaries(rows: &[Value]) -> Value {
    json!(["w1","w3"].map(|stage|{
        let selected:Vec<_>=rows.iter().filter(|r|r["stage"]==stage).cloned().collect();
        let mut latency:Vec<_>=selected.iter().filter_map(|r|r["elapsed_ms"].as_u64()).collect();latency.sort_unstable();
        json!({"stage":stage,"total":selected.len(),"passed":selected.iter().filter(|r|r["status"]=="通过").count(),"automatic":stage_metrics(&selected),
            "draft":selected.iter().filter(|r|r["label_status"]=="draft").count(),"cost":compare_cost("ds-pro",&selected),
            "p50_ms":latency.get(latency.len().div_ceil(2).saturating_sub(1)),
            "usage":(["input_tokens","output_tokens","reasoning_tokens"].map(|key|{
                let path=if key=="reasoning_tokens" {"/usage/output_tokens_details/reasoning_tokens".into()}else{format!("/usage/{key}")};
                let values:Vec<_>=selected.iter().filter_map(|r|r.pointer(&path).and_then(Value::as_u64)).collect();
                json!({"field":key,"known":values.len(),"unknown":selected.len()-values.len(),"sum":if values.is_empty(){Value::Null}else{json!(values.iter().sum::<u64>())}})
            }))})
    }))
}
pub(super) fn write_comparison(
    root: &Path,
    groups: &[Vec<Case>],
    reports: &[Value],
) -> Result<(), String> {
    let rows:Vec<_>=groups[0].iter().map(|c|json!({"stage":c.stage,"id":c.sample["id"],"label_revision":c.sample["label_revision"],"label_status":c.sample["label_status"],"focus":c.sample["focus"],
        "arms":reports.iter().map(|report|{
            let row=items(&report["results"]).iter().find(|r|r["stage"]==c.stage && r["id"]==c.sample["id"]);
            json!({"profile":report["profile"],"result":row})
        }).collect::<Vec<_>>() })).collect();
    m15_write_json(
        &root.join("comparison.json"),
        &json!({"campaign":"AF352","input_revision_note":"AF355: af352_w3_01–04 inputs revised after the historical AF352 screening conclusion; 8 old requests across two arms are stale, not re-recorded. Historical conclusions remain tied to the original inputs.","note":"One paired observation per sample, draft labels pending review; fixed W3 inputs, not a chained runtime test","profiles":reports,"comparison":rows}),
    )
}

#[test]
fn af352_paired_plan_is_blind_40_slots_current_and_requires_real_configuration() {
    let temp = TempRecords::new();
    let groups = plan(&temp.0, fixture_variable).unwrap();
    assert_eq!(
        groups.iter().map(Vec::len).collect::<Vec<_>>(),
        vec![20, 20]
    );
    assert!(!temp.0.join("manifest.json").exists());
    for (a, b) in groups[0].iter().zip(&groups[1]) {
        assert_eq!(a.sample, b.sample);
        assert_ne!(case_fingerprint(a), case_fingerprint(b));
        let mut request = a.request.clone();
        request["reasoning"] = b.request["reasoning"].clone();
        assert_eq!(request, b.request);
        assert!(!a
            .request
            .to_string()
            .contains(a.sample["id"].as_str().unwrap()));
        assert!(!a
            .request
            .to_string()
            .contains(a.sample["focus"].as_str().unwrap()));
        let payload: Value = serde_json::from_str(a.request["input"].as_str().unwrap()).unwrap();
        assert_eq!(
            payload["contract"],
            if a.stage == "w1" { "w1.v5" } else { "w3.v6" }
        );
        assert_eq!(rates(&a.model.label), (9000, 27000, 300));
        assert_eq!(rates(&b.model.label), (9000, 27000, 300));
        assert_eq!(
            a.request["max_output_tokens"],
            if a.stage == "w1" { 32768 } else { 16384 }
        );
        assert!(!write_models::W1_PROMPT.contains(a.sample["input"]["text"].as_str().unwrap()));
        assert!(!write_models::W3_PROMPT.contains(a.sample["input"]["text"].as_str().unwrap()));
    }
    assert!(plan(&temp.0, |_| None).is_err());
    println!(
        "AF352 injected plan: minimal W1=12 W3=8; low W1=12 W3=8; total=40 sends=0 cap=10 CNY"
    );
    for g in &groups {
        println!(
            "{} scenario={:.3} upper={:.3} CNY (fixture endpoint only)",
            g[0].model.label,
            g.iter()
                .map(|c| input_bound(c) / 4 * 9000 + 3000 * 27000)
                .sum::<u64>() as f64
                / 1e9,
            g.iter().map(upper).sum::<u64>() as f64 / 1e9
        );
    }
}
fn fixture_variable(k: &str) -> Option<String> {
    match k {
        "DEEPSEEK_PRO_MODEL" => Some("ep-fictional".into()),
        "HUOSHAN_DEEPSEEK_BASE_URL" => Some("https://example.test/v1".into()),
        _ => None,
    }
}
#[test]
fn af352_refreeze_only_labels_and_transport_reservation_are_shared() {
    let temp = TempRecords::new();
    let groups = plan(&temp.0, fixture_variable).unwrap();
    let frozen = manifest(&groups);
    m15_write_json(&temp.0.join("manifest.json"), &frozen).unwrap();
    for g in &groups {
        compare_freeze(g).unwrap();
    }
    let mut ledger = json!({});
    let c = &groups[0][0];
    assert!(reserve_with_cap(&mut ledger, c, CAP).unwrap());
    m15_write_json(&temp.0.join("cost-ledger.json"), &ledger).unwrap();
    evaluate_cases(&[c.clone()], Some(20), &mut |_| {
        Err(format!("{TRANSPORT}synthetic"))
    })
    .unwrap();
    ledger[key(c)] = settlement(c, &Value::Null, true);
    assert_eq!(total(&ledger).unwrap(), 0);
    m15_write_json(&temp.0.join("cost-ledger.json"), &ledger).unwrap();
    let mut resumed = plan(&temp.0, fixture_variable).unwrap();
    enable_retries(&mut resumed, &ledger).unwrap();
    assert!(matches!(resumed[0][0].replay, Replay::Missing));
    assert_eq!(
        settlement(c, &Value::Null, false)["charged_nano_cny"],
        upper(c)
    );
    let mut changed = groups.clone();
    for g in &mut changed {
        g[0].sample["label_revision"] = json!("AF352-review");
    }
    let amended = manifest(&changed);
    assert!(refreeze_labels(&temp.0, &amended, &changed, "AF352-review").is_ok());
    let mut bad = amended.clone();
    bad["budget_nano_cny"] = json!(CAP + 1);
    assert!(refreeze_labels(&temp.0, &bad, &changed, "AF352-review").is_err());
    bad = amended.clone();
    bad["profiles"][0]["slots"][0]["request"] = json!("changed");
    assert!(refreeze_labels(&temp.0, &bad, &changed, "AF352-review").is_err());
    for c in groups.iter().flatten() {
        let _ = reserve_with_cap(&mut ledger, c, CAP).unwrap();
        assert!(total(&ledger).unwrap() <= CAP);
    }
}
#[test]
fn af352_subject_cases_and_report_keep_semantic_safety_and_usage() {
    let samples = selected_samples();
    for (_, s) in samples.iter().filter(|(stage, _)| stage == "w3") {
        let batch = batch(adapter::expand(&s["input"]).unwrap(), &s["input"]).unwrap();
        assert!(batch
            .candidates
            .iter()
            .all(|c| c.status != w2::Status::Rejected));
    }
    for id in ["af352_w3_03", "af352_w3_04"] {
        let (_, s) = samples.iter().find(|(_, s)| s["id"] == id).unwrap();
        assert_eq!(s["label_status"], "draft");
        assert_eq!(s["expect"]["reviews"][0]["verdict"], "insufficient");
        let reviews:Vec<_>=items(&s["expect"]["reviews"]).iter().map(|r|json!({"candidate":r["candidate"],"verdict":"supported","problems":[],"episode_value":"not_applicable"})).collect();
        let out = json!({"reviews":reviews,"request_reviews":[],"coverage":{"status":"complete","missing":[]}});
        assert_ne!(score("w3", s, Some(&out.to_string()))["status"], "通过");
    }
    let rows = vec![
        json!({"stage":"w1","status":"通过","elapsed_ms":40,"usage":{"input_tokens":100,"output_tokens":200,"output_tokens_details":{"reasoning_tokens":150}}}),
        json!({"stage":"w1","status":"调用失败"}),
    ];
    let summary = summaries(&rows);
    assert_eq!(summary[0]["passed"], 1);
    assert_eq!(summary[0]["p50_ms"], 40);
    assert_eq!(summary[0]["usage"][2]["sum"], 150);
    assert_eq!(summary[0]["usage"][2]["unknown"], 1);
    assert_eq!(compare_price("ds-pro-minimal"), compare_price("ds-pro"));
}

#[test]
fn af352_comparison_keeps_both_arms_each_question_and_refreeze_rejects_price_changes() {
    let temp = TempRecords::new();
    let groups = plan(&temp.0, fixture_variable).unwrap();
    let reports:Vec<_> = groups.iter().map(|g| {
        let r = evaluate_cases(g,None,&mut |_|panic!("offline report sent")).unwrap();
        json!({"profile":g[0].model.label,"results":r["results"],"stages":summaries(items(&r["results"]))})
    }).collect();
    write_comparison(&temp.0, &groups, &reports).unwrap();
    let report = read(&temp.0.join("comparison.json"), Value::Null).unwrap();
    assert_eq!(items(&report["comparison"]).len(), 20);
    assert!(items(&report["comparison"]).iter().all(|r| {
        items(&r["arms"]).len() == 2
            && items(&r["arms"])
                .iter()
                .all(|a| a["result"]["status"] == "未录制")
    }));
    let original = manifest(&groups);
    m15_write_json(&temp.0.join("manifest.json"), &original).unwrap();
    for key in ["rates_nano_cny_per_token", "effort", "slots"] {
        let mut changed = original.clone();
        changed["profiles"][0][key] = json!("changed");
        assert!(refreeze_labels(&temp.0, &changed, &groups, "AF352-review").is_err());
    }
}

#[test]
fn af355_input_revision_invalidates_exactly_eight_recordings_without_refreeze() {
    let temp = TempRecords::new();
    let current = plan(&temp.0, fixture_variable).unwrap();
    let mut old = current.clone();
    for c in old.iter_mut().flatten() {
        if !c.sample["id"].as_str().unwrap().starts_with("af352_w3_") {
            continue;
        }
        let expect = c.sample["expect"].clone();
        assert_eq!(c.sample["label_revision"], "AF355");
        assert!(!c.sample["input"].to_string().contains("刚才"));
        let mut sample = c.sample.clone();
        sample["label_revision"] = json!("AF352-draft");
        for pointer in [
            "/input/text",
            "/input/candidates/0/sentence",
            "/input/candidates/0/value",
        ] {
            let field = sample.pointer_mut(pointer).unwrap();
            *field = json!(format!("刚才{}", field.as_str().unwrap()));
        }
        assert_eq!(sample["expect"], expect);
        assert_eq!(
            sample["input"]["candidates"][1],
            c.sample["input"]["candidates"][1]
        );
        let mut request = compare_request("w3", &sample, 16384).unwrap();
        request["model"] = c.request["model"].clone();
        c.model.effort.apply(&mut request);
        let options = ProviderOptions {
            provider: c.provider.clone().unwrap(),
            campaign: c.campaign.clone().unwrap(),
        };
        *c = case_from_request_with_provider(
            c.stage.clone(),
            sample,
            c.path.parent().unwrap(),
            &c.model,
            1,
            request,
            Some(&options),
            Some(360),
        )
        .unwrap();
    }
    assert_eq!(
        old.iter()
            .flatten()
            .zip(current.iter().flatten())
            .filter(|(a, b)| case_fingerprint(a) != case_fingerprint(b))
            .count(),
        8
    );
    let frozen = manifest(&old);
    m15_write_json(&temp.0.join("manifest.json"), &frozen).unwrap();
    for g in &old {
        compare_freeze(g).unwrap();
        evaluate_cases(g, Some(20), &mut |c| Ok(compare_fixture_response(c))).unwrap();
    }
    let resumed = plan(&temp.0, fixture_variable).unwrap();
    assert_eq!(
        resumed
            .iter()
            .flatten()
            .filter(|c| matches!(c.replay, Replay::Stale))
            .count(),
        8
    );
    assert!(refreeze_labels(&temp.0, &manifest(&resumed), &resumed, "AF355").is_err());
    assert_eq!(
        read(&temp.0.join("manifest.json"), Value::Null).unwrap(),
        frozen
    );
    let reports: Vec<_> = resumed
        .iter()
        .map(|g| {
            let r =
                evaluate_cases(g, None, &mut |_| panic!("offline replay must not send")).unwrap();
            assert_eq!(
                items(&r["results"])
                    .iter()
                    .filter(|r| r["status"] == "录制过期")
                    .count(),
                4
            );
            json!({"profile":g[0].model.label,"results":r["results"]})
        })
        .collect();
    write_comparison(&temp.0, &resumed, &reports).unwrap();
    let report = read(&temp.0.join("comparison.json"), Value::Null).unwrap();
    assert!(report["input_revision_note"]
        .as_str()
        .unwrap()
        .contains("AF355"));
}
