//! AF342 scope trial: fixed candidates, not a W1→W3 pipeline; draft semantics.
use super::cost_campaign::*;
use super::*;
pub(super) const ROOT: &str = "target/eval-providers/af342";
pub(super) const CAP: u64 = 10_000_000_000;
fn samples_for(stage: &str) -> Vec<Value> {
    serde_json::from_str(if stage == "w1" {
        include_str!("af342/w1.json")
    } else {
        include_str!("af342/w3.json")
    })
    .unwrap()
}
pub(super) fn plan(
    root: &Path,
    variable: impl Fn(&str) -> Option<String>,
) -> Result<Vec<Vec<Case>>, String> {
    let profiles = evaluation_profiles(include_str!("providers.json"))?;
    let mut p = profiles.into_iter().find(|p| p.id == "ds-pro").unwrap();
    p.model = variable("DEEPSEEK_PRO_MODEL")
        .filter(|m| !m.trim().is_empty())
        .ok_or("AF342 requires DEEPSEEK_PRO_MODEL from the production role configuration")?;
    p.effort = ReasoningEffort::Low;
    let base = variable("HUOSHAN_DEEPSEEK_BASE_URL")
        .filter(|s| !s.trim().is_empty())
        .ok_or("AF342 requires HUOSHAN_DEEPSEEK_BASE_URL; no placeholder manifest")?;
    let model = EvalModel {
        label: p.id.clone(),
        model: Some(p.model.clone()),
        base: Some(base),
        effort: p.effort,
    };
    model.validate()?;
    let options = ProviderOptions {
        provider: p.provider.clone(),
        campaign: root.join("campaigns/ds-pro"),
    };
    let mut cases = Vec::new();
    for stage in ["w1", "w3"] {
        let samples = samples_for(stage);
        if samples.len() != 12 {
            return Err("AF342 requires 12 samples per stage".into());
        }
        for sample in samples {
            let mut request =
                compare_request(stage, &sample, if stage == "w1" { 32768 } else { 16384 })?;
            request["model"] = json!(p.model);
            p.effort.apply(&mut request);
            let mut case = case_from_request_with_provider(
                stage.into(),
                sample,
                &root.join("recordings/ds-pro/repeat-1"),
                &model,
                1,
                request,
                Some(&options),
                Some(360),
            )?;
            case.endpoint_env = p.endpoint_env.clone();
            cases.push(case);
        }
    }
    compare_mark_interrupted(&mut cases)?;
    Ok(vec![cases])
}
pub(super) fn manifest(groups: &[Vec<Case>]) -> Value {
    json!({"version":"af342.v1","slots":24,"budget_nano_cny":CAP,"label_revision":"AF342-draft",
        "input_estimate":"wire JSON UTF-8 bytes + 1024; reservation without cache discount",
        "profiles":groups.iter().map(|g|json!({"id":g[0].model.label,"effort":g[0].model.effort,"timeout_seconds":360,
            "rates_nano_cny_per_token":rates(&g[0].model.label),
            "slots":g.iter().map(|c|json!({"slot":campaign_slot(c),"request":case_fingerprint(c),"input_token_bound":input_bound(c),"upper_nano_cny":upper(c),
                "labels":hippocampus::recall_models::fingerprint(&json!({"expect":c.sample["expect"],"label_revision":c.sample["label_revision"],"label_status":c.sample["label_status"]}))})).collect::<Vec<_>>() })).collect::<Vec<_>>()})
}
#[test]
fn af342_plan_is_24_blind_current_requests_and_budget_is_bounded() {
    let dir = std::env::temp_dir().join(format!("af342-{}", hippocampus::diagnostics::new_id()));
    let groups = plan(&dir, |k| match k {
        "DEEPSEEK_PRO_MODEL" => Some("ep-fictional".into()),
        "HUOSHAN_DEEPSEEK_BASE_URL" => Some("https://example.test/v1".into()),
        _ => None,
    })
    .unwrap();
    assert_eq!(groups[0].len(), 24);
    assert!(!dir.exists(), "plan must not create manifest");
    let mut ledger = json!({});
    for c in &groups[0] {
        assert_eq!(c.model.effort, ReasoningEffort::Low);
        assert_eq!(c.sample["label_status"], "draft");
        let payload: Value = serde_json::from_str(c.request["input"].as_str().unwrap()).unwrap();
        assert_eq!(
            payload["contract"],
            if c.stage == "w1" { "w1.v5" } else { "w3.v6" }
        );
        assert!(
            !c.request
                .to_string()
                .contains(c.sample["id"].as_str().unwrap())
        );
        assert!(
            !c.request
                .to_string()
                .contains(c.sample["focus"].as_str().unwrap())
        );
        assert!(!write_models::W1_PROMPT.contains(c.sample["input"]["text"].as_str().unwrap()));
        assert!(!write_models::W3_PROMPT.contains(c.sample["input"]["text"].as_str().unwrap()));
        let mut old = c.request.clone();
        old["text"]["format"]["name"] = json!(if c.stage == "w1" { "w1_v4" } else { "w3_v5" });
        assert_ne!(fingerprint(&old), fingerprint(&c.request));
        let _ = reserve_with_cap(&mut ledger, c, CAP).unwrap();
        assert!(total(&ledger).unwrap() <= CAP);
    }
    println!(
        "AF342 injected plan: W1=12 W3=12 total=24 sent=0 cap=10; scenario/upper {:.3}/{:.3} CNY (synthetic endpoint)",
        groups[0]
            .iter()
            .map(|c| input_bound(c) / 4 * 9000 + 3000 * 27000)
            .sum::<u64>() as f64
            / 1e9,
        groups[0].iter().map(upper).sum::<u64>() as f64 / 1e9
    );
    assert!(plan(&dir, |_| None).is_err());
}

#[test]
fn af342_fixed_candidates_and_scope_coverage_labels_are_executable() {
    for sample in samples_for("w3") {
        let reviews:Vec<_>=items(&sample["expect"]["reviews"]).iter().map(|r|json!({"candidate":r["candidate"],"verdict":r["verdict"],"problems":[],"episode_value":if r["episode_value"].is_array() {r["episode_value"][0].clone()} else {r["episode_value"].clone()}})).collect();
        let missing: Vec<_> = items(&sample["expect"]["coverage"]["missing_contains"])
            .iter()
            .map(|q| json!({"text":q}))
            .collect();
        let out = json!({"reviews":reviews,"request_reviews":[],"coverage":{"status":sample["expect"]["coverage"]["status"],"missing":missing}});
        let report = score("w3", &sample, Some(&out.to_string()));
        assert_eq!(report["status"], "通过", "{report:#}");
        if sample["expect"]["coverage"]["status"] == "gaps" {
            let mut bad = out;
            bad["coverage"]["missing"] = json!([]);
            assert_ne!(
                score("w3", &sample, Some(&bad.to_string()))["status"],
                "通过"
            );
        }
    }
    for (w1, w3) in samples_for("w1").into_iter().zip(samples_for("w3")) {
        let mut input = w3["input"].clone();
        if w1["id"] == "af342_w1_11" {
            let mut c = w1["expect"]["candidates"][0].clone();
            c["value"] = json!("我负责白鹭项目的排版设计");
            input["candidates"] = json!([c]);
        }
        let out = adapter::expand(&input).unwrap();
        let report = score("w1", &w1, Some(&serde_json::to_string(&out).unwrap()));
        assert_eq!(report["status"], "通过", "{report:#}");
    }
}

#[test]
fn af342_transport_resume_keeps_frozen_slots_and_old_requests_are_stale() {
    let temp = TempRecords::new();
    let variable = |k: &str| match k {
        "DEEPSEEK_PRO_MODEL" => Some("ep-fictional".into()),
        "HUOSHAN_DEEPSEEK_BASE_URL" => Some("https://example.test/v1".into()),
        _ => None,
    };
    let groups = plan(&temp.0, variable).unwrap();
    m15_write_json(&temp.0.join("manifest.json"), &manifest(&groups)).unwrap();
    compare_freeze(&groups[0]).unwrap();
    let c = &groups[0][0];
    let mut ledger = json!({});
    assert!(reserve_with_cap(&mut ledger, c, CAP).unwrap());
    m15_write_json(&temp.0.join("cost-ledger.json"), &ledger).unwrap();
    let report = evaluate_cases(&[c.clone()], Some(24), &mut |_| {
        Err(format!("{TRANSPORT}fixture disconnect"))
    })
    .unwrap();
    assert_eq!(report["sent_calls"], 1);
    ledger[key(c)] = settlement(c, &Value::Null, true);
    assert_eq!(total(&ledger).unwrap(), 0);
    m15_write_json(&temp.0.join("cost-ledger.json"), &ledger).unwrap();
    let mut resumed = plan(&temp.0, variable).unwrap();
    enable_retries(&mut resumed, &ledger).unwrap();
    assert!(matches!(resumed[0][0].replay, Replay::Missing));
    assert_eq!(manifest(&resumed), manifest(&groups));
    assert!(reserve_with_cap(&mut ledger, &resumed[0][0], CAP).unwrap());
    m15_write_json(&temp.0.join("cost-ledger.json"), &ledger).unwrap();
    let result = evaluate_cases(&[resumed[0][0].clone()], Some(24), &mut |_| {
        Ok(json!({"provider_http_error":{"http_status":400,"error_type":"fixture"}}))
    })
    .unwrap();
    assert_eq!(result["sent_calls"], 1);
    ledger[key(c)] = settlement(c, &Value::Null, false);
    m15_write_json(&temp.0.join("cost-ledger.json"), &ledger).unwrap();
    assert_eq!(total(&ledger).unwrap(), upper(c));
    let final_plan = plan(&temp.0, variable).unwrap();
    assert_eq!(items(&final_plan[0][0].file["recorded"]).len(), 2);
    assert_eq!(
        evaluate_cases(&[final_plan[0][0].clone()], Some(24), &mut |_| panic!(
            "response must not be resent"
        ))
        .unwrap()["sent_calls"],
        0
    );
    let mut old_request = c.request.clone();
    old_request["text"]["format"]["name"] = json!("w1_v4");
    let options = ProviderOptions {
        provider: c.provider.clone().unwrap(),
        campaign: c.campaign.clone().unwrap(),
    };
    let old = case_from_request_with_provider(
        c.stage.clone(),
        c.sample.clone(),
        c.path.parent().unwrap(),
        &c.model,
        c.repeat,
        old_request,
        Some(&options),
        c.timeout_seconds,
    )
    .unwrap();
    assert!(matches!(old.replay, Replay::Stale));
    assert!(campaign_check(&[old], 24).is_err());
}

#[test]
fn af348_labels_preserve_requests_and_mechanical_checks() {
    for stage in ["w1", "w3"] {
        for sample in samples_for(stage)
            .into_iter()
            .filter(|s| s["label_revision"] == "AF348")
        {
            let mut old = sample.clone();
            old["label_revision"] = json!("AF342-draft");
            match sample["id"].as_str().unwrap() {
                "af342_w1_10" => {
                    old["expect"]["candidates"][0]["value"]["required"] =
                        json!(["已决定在青梧项目中采用离线验签"])
                }
                "af342_w3_09" => old["expect"]["reviews"][0]["episode_value"] = json!("useful"),
                _ => {
                    old["expect"]["coverage"]
                        .as_object_mut()
                        .unwrap()
                        .remove("ignore_terminal_punctuation");
                }
            }
            assert_eq!(
                compare_request(stage, &old, 16384).unwrap(),
                compare_request(stage, &sample, 16384).unwrap()
            );
        }
    }
    assert!(coverage_contains("甲。乙", "甲。乙。", true));
    assert!(coverage_contains("甲。乙!", "甲。乙。", true));
    for bad in ["甲乙", "甲，乙", "甲。", "甲。乙 "] {
        // Whitespace is not stripped; an internal punctuation change is not accepted.
        if bad.ends_with(' ') {
            assert!(!coverage_contains("甲。乙", bad, true));
        } else {
            assert!(!coverage_contains(bad, "甲。乙。", true));
        }
    }
    assert!(!coverage_contains("甲。乙", "甲。乙。", false));
    assert!(!coverage_contains("甲", "。", true));
    let s = samples_for("w3")
        .into_iter()
        .find(|s| s["id"] == "af342_w3_11")
        .unwrap();
    for (quote, pass) in [
        ("我负责白鹭项目的排版设计", true),
        ("我负责白鹭项目的排版设计!", false),
        ("我负责白鹭项目设计", false),
    ] {
        let output = json!({"reviews":[],"request_reviews":[],"coverage":{"status":"gaps","missing":[{"text":quote}]}});
        assert_eq!(
            score("w3", &s, Some(&output.to_string()))["status"] == "通过",
            pass
        );
    }
    let s = samples_for("w3")
        .into_iter()
        .find(|s| s["id"] == "af342_w3_09")
        .unwrap();
    for (value, pass) in [("useful", true), ("low_value", true), ("unknown", false)] {
        let output = json!({"reviews":[{"candidate":0,"verdict":"supported","problems":[],"episode_value":value}],"request_reviews":[],"coverage":{"status":"complete","missing":[]}});
        assert_eq!(
            score("w3", &s, Some(&output.to_string()))["status"] == "通过",
            pass
        );
    }
    let s = samples_for("w1")
        .into_iter()
        .find(|s| s["id"] == "af342_w1_10")
        .unwrap();
    let mut input = samples_for("w3")
        .into_iter()
        .find(|s| s["id"] == "af342_w3_10")
        .unwrap()["input"]
        .clone();
    input["candidates"][0]["value"] = json!("采用离线验签");
    let out = adapter::expand(&input).unwrap();
    assert_eq!(
        score("w1", &s, Some(&serde_json::to_string(&out).unwrap()))["status"],
        "通过"
    );
    input["candidates"][0]["sentence"] = json!("采用离线验签");
    assert_ne!(
        score(
            "w1",
            &s,
            Some(&serde_json::to_string(&adapter::expand(&input).unwrap()).unwrap())
        )["status"],
        "通过"
    );
}

#[test]
#[ignore = "Explicit AF348 offline re-score of authorized evaluation recordings only"]
fn af348_replay() {
    let root = Path::new(ROOT);
    let path = root.join("recordings/ds-pro/repeat-1/af342_w1_01.json");
    let file = read(&path, Value::Null).unwrap();
    let r = items(&file["recorded"]).last().unwrap();
    let groups = plan(root, |k| match k {
        "DEEPSEEK_PRO_MODEL" => r["provider_binding"]["model"].as_str().map(str::to_owned),
        "HUOSHAN_DEEPSEEK_BASE_URL" => r["base_url"].as_str().map(str::to_owned),
        _ => None,
    })
    .unwrap();
    assert_eq!(groups[0].len(), 24);
    assert!(
        groups[0]
            .iter()
            .all(|c| matches!(c.replay, Replay::Output(_))),
        "strict replay gap; no send permitted"
    );
    let report = evaluate_cases(&groups[0], None, &mut |_| {
        panic!("offline replay must not send")
    })
    .unwrap();
    assert_eq!(report["sent_calls"], 0);
    let before = read(&root.join("report-repeat-1.json"), Value::Null).unwrap();
    let rows = items(&report["results"]);
    let summary = json!({"label_revision":"AF348","sent_calls":0,"strict_reused":24,
        "stages":(["w1","w3"].map(|stage|json!({"stage":stage,"passed":rows.iter().filter(|r|r["stage"]==stage && r["status"]=="通过").count(),"total":12}))),
        "before_stages":before["profiles"][0]["stages"],"report":report});
    m15_write_json(&root.join("reports-af348/report-repeat-1.json"), &summary).unwrap();
    println!("AF348 strict reuse=24 sends=0 stages={}", summary["stages"]);
}
