//! AF193/AF196: fixed fictional stage probes, using the runtime's Pro/low defaults.
use super::*;

const CAP: usize = 12;
const BEFORE_W1: &str = include_str!("w1-before.txt");
const BEFORE_W3: &str = include_str!("w3-before.txt");
const AF196_RULE: &str = r#"A present-time qualifier expressing an ongoing state or an assertion still holding at utterance
time does not bound that state to the speaking day. Never encode it as a single-day
relative/anchored interval or any interval with an end. If its continuing scope cannot be
represented reliably, use interp.kind=unknown and keep its original Quote. This does not
override separately expressed explicit calendar dates.
"#;

pub(super) fn scenes() -> Vec<Value> {
    serde_json::from_str(include_str!("samples.json")).unwrap()
}

pub(super) fn replace_prompt(request: &mut Value, stage: ModelStage, prompt: &str) {
    request["instructions"] = structured_request(
        stage,
        request["text"]["format"]["name"].as_str().unwrap(),
        prompt,
        serde_json::from_str(request["input"].as_str().unwrap()).unwrap(),
        request["text"]["format"]["schema"].clone(),
        &ModelBudgetConfig::default(),
    )["instructions"]
        .clone();
}

fn plan(root: &Path, model: &str, base: &str) -> Result<Vec<Case>, String> {
    plan_for(root, model, base, false)
}

fn plan_for(root: &Path, model: &str, base: &str, w1_only: bool) -> Result<Vec<Case>, String> {
    let config: hippocampus::chat::ChatConfig =
        serde_json::from_value(json!({"write_models":{}})).unwrap();
    let roles = config.write_models.as_ref().unwrap();
    let catalog = w2::relation_catalog(&config.predicates, &config.memory_types)
        .map_err(|e| e.to_string())?;
    let provider: ProviderConfig = serde_json::from_value(json!({
        "protocol":"openai_responses", "base_url_env":"DEEPSEEK_BASE_URL",
        "api_key_env":"DEEPSEEK_API_KEY"
    }))
    .unwrap();
    let options = ProviderOptions {
        provider,
        campaign: root.join("campaign"),
    };
    let mut cases = Vec::new();
    for scene in scenes() {
        for &stage in if w1_only {
            &["w1"][..]
        } else {
            &["w1", "w3"][..]
        } {
            let mut sample = scene.clone();
            sample["expect"] = scene[format!("{stage}_expect")].clone();
            let model = EvalModel {
                label: if w1_only {
                    "af196-pro-low"
                } else {
                    "af193-pro-low"
                }
                .into(),
                model: Some(model.into()),
                base: Some(base.trim_end_matches('/').into()),
                effort: roles.effort(stage),
            };
            let mut request = stage_request_with_output_tokens(
                stage,
                &sample,
                if stage == "w1" { 32768 } else { 16384 },
            )?;
            let mut input: Value =
                serde_json::from_str(request["input"].as_str().unwrap()).unwrap();
            input["source_snapshot"]["eval_sample"] = json!(format!(
                "s-{}",
                hippocampus::recall_models::fingerprint(&json!(["af193-source.v1", scene["id"]]))
                    .trim_start_matches("sha256:")
            ));
            input["relations"] = json!(catalog.iter().map(|r|json!({"id":r.id,
                "memory_types":r.memory_types.iter().map(|t|format!("{t:?}").to_lowercase()).collect::<Vec<_>>()
            })).collect::<Vec<_>>());
            request["input"] = json!(input.to_string());
            request["model"] = json!(model.model);
            model.effort.apply(&mut request);
            cases.push(case_from_request_with_provider(
                stage.into(),
                sample,
                &root.join("recordings").join(stage),
                &model,
                1,
                request,
                Some(&options),
                Some(360),
            )?);
        }
    }
    compare_mark_interrupted(&mut cases)?;
    Ok(cases)
}

fn cohort_cap(cases: &[Case]) -> usize {
    if cases
        .first()
        .is_some_and(|c| c.model.label == "af196-pro-low")
    {
        6
    } else {
        CAP
    }
}

fn check(cases: &[Case], limit: Option<usize>) -> Result<usize, String> {
    let cap = cohort_cap(cases);
    if cases.len() != cap
        || limit.is_some_and(|n| n != 0 && n != cap)
        || (cap == 6
            && cases
                .iter()
                .any(|c| c.stage != "w1" || c.model.label != "af196-pro-low"))
    {
        return Err(format!(
            "time trial fixes {cap} slots; EVAL_RECORD must be {cap} or 0"
        ));
    }
    for c in cases {
        if let Replay::Rejected(reason) = &c.replay {
            return Err(format!(
                "time trial {} {} rejected: {reason}; nothing was sent",
                c.stage, c.sample["id"]
            ));
        }
    }
    preflight(cases, limit)
}

fn dry(cases: &[Case], limit: Option<usize>) -> Result<String, String> {
    let cap = cohort_cap(cases);
    check(cases, limit.or(Some(cap)))?;
    let count = |stage: &str, pred: fn(&Replay) -> bool| {
        cases
            .iter()
            .filter(|c| c.stage == stage && pred(&c.replay))
            .count()
    };
    let bytes: usize = cases.iter().map(|c| c.wire_request.to_string().len()).sum();
    let cohort = if cap == 6 {
        "AF196：第二轮，6 个虚构场景 × W1 各 1 次 = 6 槽；W3/W4/R1/R4=0。"
    } else {
        "AF193：6 个虚构场景 × W1/W3 各 1 次 = 12 槽；W4/R1/R4=0。"
    };
    let roles = if cap == 6 {
        "生产缺省：DeepSeek Pro；W1 low/32768；超时 360 秒。"
    } else {
        "生产缺省：DeepSeek Pro；W1 low/32768，W3 low/16384；超时均 360 秒。"
    };
    let boundary = if cap == 6 {
        "仅录制 W1；不调用 W3，不读写 AF193 录制或账本。"
    } else {
        "W3 输入为固定完整候选，经 W2；这是分阶段诊断，不是 W1 输出到 W3 的端到端验证。"
    };
    let output_tokens = if cap == 6 { 196608 } else { 294912 };
    Ok(format!(
        "{cohort}\n\
        {roles}\n\
        W1：待录制 {}，可复用 {}，过期 {}；W3：待录制 {}，可复用 {}，过期 {}。\n\
        独立 campaign 固定额度 {cap}；累计已发送/在途 {}，新缺口 {}；不重试、不重发在途槽。\n\
        模型/地址{}；真实录制拒绝干跑占位。\n\
        {boundary}\n\
        请求正文合计 {bytes} 字节；输入 token 粗估按 UTF-8 字节上界 {bytes}，非计费预测。\n\
        输出配置容量 {output_tokens} token（含推理，非预期用量）。\n\
        本次真实调用 0；不读取密钥、不创建目录/账本/录制；标签 draft，待复核。",
        count("w1", |r| matches!(r, Replay::Missing)),
        count("w1", |r| matches!(r, Replay::Output(_) | Replay::Failed(_))),
        count("w1", |r| matches!(r, Replay::Stale)),
        count("w3", |r| matches!(r, Replay::Missing)),
        count("w3", |r| matches!(r, Replay::Output(_) | Replay::Failed(_))),
        count("w3", |r| matches!(r, Replay::Stale)),
        count_sends([cases[0].campaign.as_ref().unwrap().join("sends")])?,
        cases
            .iter()
            .filter(|c| matches!(c.replay, Replay::Missing | Replay::Stale))
            .count(),
        if cases[0].model.model.as_deref() == Some("ep-af193-unresolved-pro")
            || cases[0].model.base.as_deref() == Some("https://af193.invalid/v1")
        {
            "未设置，使用干跑占位"
        } else {
            "已从环境加载（不输出值）"
        }
    ))
}

pub(super) fn run() -> Result<(), String> {
    let second = std::env::var_os("EVAL_AF196").is_some();
    if second && std::env::var_os("EVAL_AF193").is_some() {
        return Err("AF193 and AF196 cannot share a campaign".into());
    }
    let trial = if second { "AF196" } else { "AF193" };
    let mode = std::env::var(if second { "EVAL_AF196" } else { "EVAL_AF193" }).unwrap();
    if mode == "impact" && !second {
        println!("{}", serde_json::to_string_pretty(&impact()?).unwrap());
        return Ok(());
    }
    if mode != "1" {
        return Err(format!(
            "{trial} accepts 1{}",
            if second { "" } else { " or impact" }
        ));
    }
    for key in [
        "EVAL_COMPARE",
        "EVAL_SMOKE",
        "EVAL_M15",
        "EVAL_CASES",
        "EVAL_STAGES",
        "EVAL_MODEL",
        "EVAL_MODEL_LABEL",
        "EVAL_REASONING_EFFORT",
        "EVAL_REPEAT",
        "EVAL_MAX_OUTPUT_TOKENS",
        "EVAL_PROVIDER_CONFIG",
        "EVAL_BASE_URL",
        "EVAL_API_KEY",
    ] {
        if std::env::var_os(key).is_some() {
            return Err(format!("{trial} fixed cohort rejects {key}"));
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join(if second {
        "target/eval-time-af196"
    } else {
        "target/eval-time-af193"
    });
    let model =
        std::env::var("DEEPSEEK_PRO_MODEL").unwrap_or_else(|_| "ep-af193-unresolved-pro".into());
    let base =
        std::env::var("DEEPSEEK_BASE_URL").unwrap_or_else(|_| "https://af193.invalid/v1".into());
    let cases = plan_for(&root, &model, &base, second)?;
    let limit = std::env::var("EVAL_RECORD")
        .ok()
        .map(|s| {
            s.parse::<usize>()
                .map_err(|_| "EVAL_RECORD integer".to_owned())
        })
        .transpose()?;
    if std::env::var("EVAL_DRY_RUN").as_deref() == Ok("1") {
        println!("{}", dry(&cases, limit)?);
        return Ok(());
    }
    let sends = check(&cases, limit)?;
    let live = if sends > 0 {
        if std::env::var("PMR_MODEL_NETWORK").as_deref() != Ok("live") {
            return Err(format!(
                "{trial} real recording requires PMR_MODEL_NETWORK=live; nothing sent"
            ));
        }
        if model == "ep-af193-unresolved-pro" || base == "https://af193.invalid/v1" {
            return Err("recording requires DEEPSEEK_PRO_MODEL and DEEPSEEK_BASE_URL".into());
        }
        let key = std::env::var("DEEPSEEK_API_KEY").map_err(|_| "missing DEEPSEEK_API_KEY")?;
        if key.trim().is_empty() {
            return Err("missing DEEPSEEK_API_KEY".into());
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
    let mut report = evaluate_cases(&cases, limit, &mut |c| {
        let (http, key) = live.as_ref().ok_or("offline time trial cannot send")?;
        let request = smoke_http_request(http, c, key)?;
        let response = http.execute(request).map_err(|e| {
            if e.is_timeout() {
                "provider_timeout".into()
            } else {
                e.to_string()
            }
        })?;
        model_provider::read_http_response(response, ProviderProtocol::OpenaiResponses)
            .map(|(_, envelope)| envelope)
            .map_err(|e| e.to_string())
    })?;
    report["trial"] = json!(trial);
    report["authorized_calls"] = json!(cohort_cap(&cases));
    report["stage_only"] = json!(true);
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    m15_write_json(&root.join("report.json"), &report)?;
    if eval_gate(std::env::var("EVAL_GATE").ok().as_deref())?
        && items(&report["results"])
            .iter()
            .any(|r| r["status"] != "通过")
    {
        return Err(format!("{trial} model scores failed (EVAL_GATE=1)"));
    }
    Ok(())
}

// Inventory recordings read-only. Full stored requests identify the previous prompt exactly;
// older hash-only artifacts are reported separately, never declared valid by guessing.
fn impact() -> Result<Value, String> {
    fn files(root: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
        if !root.exists() {
            return Ok(());
        }
        for e in std::fs::read_dir(root).map_err(|e| e.to_string())? {
            let p = e.map_err(|e| e.to_string())?.path();
            if p.is_dir() {
                files(&p, out)?;
            } else if p.extension().is_some_and(|s| s == "json") {
                out.push(p);
            }
        }
        Ok(())
    }
    let mut summaries = Vec::new();
    for root in [
        "tests/eval/write_stage/recordings",
        "tests/eval/m15/recordings",
        "tests/eval/end_to_end",
        "tests/eval/fixed_inputs/recordings",
        "target/eval-providers/compare-af137/recordings",
        "target/eval-providers/compare-af156",
        "target/eval-providers/compare-af163",
        "target/eval-m15/w3-af96",
        "target/eval-m15/w3-af101",
    ] {
        let mut paths = Vec::new();
        files(Path::new(root), &mut paths)?;
        let mut stages = std::collections::BTreeMap::<String, Value>::new();
        for path in paths {
            let data: Value =
                serde_json::from_slice(&std::fs::read(&path).map_err(|e| e.to_string())?)
                    .map_err(|e| format!("{}: {e}", path.display()))?;
            let rows: Vec<&Value> = if data["recorded"].is_array() {
                items(&data["recorded"]).iter().collect()
            } else if data["operations"].is_object() {
                data["operations"]
                    .as_object()
                    .unwrap()
                    .values()
                    .flat_map(|v| items(v).iter())
                    .collect()
            } else {
                continue;
            };
            for record in rows {
                let name = record["schema"].as_str().unwrap_or("");
                let stage = if name.starts_with("w1_") {
                    "w1"
                } else if name.starts_with("w3_") {
                    "w3"
                } else {
                    continue;
                };
                let stats=stages.entry(stage.into()).or_insert_with(||json!({"total":0,"previous_prompt_match":0,"other_or_hash_only":0,"frozen_history":0}));
                stats["total"] = json!(stats["total"].as_u64().unwrap() + 1);
                let frozen = root.ends_with("w3-af101") || record["prompt_arm"] == "old";
                let request = if record["request_content"].is_object() {
                    &record["request_content"]
                } else {
                    &record["provider_binding"]["request"]
                };
                let before = if stage == "w1" { BEFORE_W1 } else { BEFORE_W3 };
                let encoded = serde_json::to_string(before).unwrap();
                let contains = serde_json::to_string(request)
                    .unwrap()
                    .contains(&encoded[1..encoded.len() - 1]);
                let key = if frozen {
                    "frozen_history"
                } else if contains {
                    "previous_prompt_match"
                } else {
                    "other_or_hash_only"
                };
                stats[key] = json!(stats[key].as_u64().unwrap() + 1);
            }
        }
        summaries.push(json!({"root":root,"stages":stages}));
    }
    Ok(json!({"sent_calls":0,"groups":summaries,
        "meaning":"previous_prompt_match: recorded request contains the exact AF193-before prompt; it differs from the new prompt and must be stale. other_or_hash_only: earlier prompt or incomplete request identity; not certified reusable. Frozen historical arms keep their original requests.",
        "downstream":"W4/R4 are stale only when their actual inputs change; no recordings or ledgers were modified."}))
}

#[test]
fn fixed_plan_budget_inputs_and_strict_replay() {
    let temp = TempRecords::new();
    let cases = plan(
        &temp.0,
        "ep-af193-synthetic-pro",
        "https://af193.invalid/v1",
    )
    .unwrap();
    assert_eq!(cases.len(), 12);
    assert!(check(&cases, Some(11)).is_err());
    assert!(check(&cases, Some(13)).is_err());
    assert_eq!(check(&cases, Some(12)).unwrap(), 12);
    assert!(dry(&cases, Some(CAP)).unwrap().contains("本次真实调用 0"));
    assert!(!temp.0.exists());
    for c in &cases {
        assert_eq!(c.model.effort, ReasoningEffort::Low);
        assert_eq!(c.timeout_seconds, Some(360));
        assert!(
            !c.request["instructions"]
                .as_str()
                .unwrap()
                .contains(c.sample["input"]["text"].as_str().unwrap())
        );
        let input: Value = serde_json::from_str(c.request["input"].as_str().unwrap()).unwrap();
        assert!(input.get("expect").is_none());
        assert!(input.get("model").is_none());
        assert_ne!(input["source_snapshot"]["eval_sample"], c.sample["id"]);
        let output = if c.stage == "w1" {
            serde_json::to_value(adapter::expand(&c.sample["input"]).unwrap()).unwrap()
        } else {
            json!({"reviews":items(&c.sample["input"]["candidates"]).iter().enumerate().map(|(i,x)|json!({"candidate":i,"verdict":"supported","problems":[],"episode_value":if x["memory_type"]=="episode"{"useful"}else{"not_applicable"}})).collect::<Vec<_>>(),"coverage":{"status":"complete","missing":[]},"request_reviews":[]})
        };
        assert_eq!(
            score(&c.stage, &c.sample, Some(&output.to_string()))["status"],
            "通过"
        );
        save_recording(
            c,
            &json!({"status":"completed","usage":{}}),
            Some(&output.to_string()),
            None,
        )
        .unwrap();
    }
    let replay = plan(
        &temp.0,
        "ep-af193-synthetic-pro",
        "https://af193.invalid/v1",
    )
    .unwrap();
    assert_eq!(check(&replay, Some(0)).unwrap(), 0);
    let report = evaluate_cases(&replay, Some(0), &mut |_| panic!("replay must not send")).unwrap();
    assert_eq!(report["sent_calls"], 0);
    let c = &replay[0];
    let mut old = c.request.clone();
    replace_prompt(&mut old, ModelStage::W1, BEFORE_W1);
    let stale = case_from_request_with_provider(
        c.stage.clone(),
        c.sample.clone(),
        c.path.parent().unwrap(),
        &c.model,
        1,
        old,
        Some(&ProviderOptions {
            provider: c.provider.clone().unwrap(),
            campaign: c.campaign.clone().unwrap(),
        }),
        Some(360),
    )
    .unwrap();
    assert!(matches!(stale.replay, Replay::Stale));
    assert!(preflight(&[stale], Some(0)).is_err());
}

#[test]
fn resume_skips_inflight_and_keeps_twelve_slot_cap() {
    let temp = TempRecords::new();
    let cases = plan(
        &temp.0,
        "ep-af193-synthetic-pro",
        "https://af193.invalid/v1",
    )
    .unwrap();
    m15_write_json(
        &temp.0.join("campaign/authorization.json"),
        &json!({
            "limit":CAP, "requests":cases.iter().map(campaign_slot).collect::<Vec<_>>()
        }),
    )
    .unwrap();
    campaign_start(&cases[0]).unwrap(); // Crash after durable started, before response.
    let resumed = plan(
        &temp.0,
        "ep-af193-synthetic-pro",
        "https://af193.invalid/v1",
    )
    .unwrap();
    assert!(matches!(resumed[0].replay, Replay::Interrupted));
    assert_eq!(check(&resumed, Some(CAP)).unwrap(), CAP - 1);
    let report = evaluate_cases(&resumed, Some(CAP), &mut |c| {
        assert_ne!(campaign_slot(c), campaign_slot(&cases[0]));
        Ok(compare_fixture_response(c)) // Fake transport only; scoring isn't this test's purpose.
    })
    .unwrap();
    assert_eq!(report["sent_calls"], CAP - 1);
    assert_eq!(report["results"][0]["status"], "未录制（中断在途）");
    assert_eq!(count_sends([temp.0.join("campaign/sends")]).unwrap(), CAP);
    let replay = plan(
        &temp.0,
        "ep-af193-synthetic-pro",
        "https://af193.invalid/v1",
    )
    .unwrap();
    assert_eq!(check(&replay, Some(0)).unwrap(), 0);
    assert_eq!(check(&replay, Some(CAP)).unwrap(), 0);
    let changed = plan(&temp.0, "ep-af193-other-pro", "https://af193.invalid/v1").unwrap();
    assert!(check(&changed, Some(CAP)).is_err());
}

#[test]
fn af196_six_w1_slots_freeze_before_send_and_replay_without_sends() {
    let temp = TempRecords::new();
    let root = temp.0.join("af196");
    let cases = plan_for(
        &root,
        "ep-af193-synthetic-pro",
        "https://af193.invalid/v1",
        true,
    )
    .unwrap();
    assert_eq!(cases.len(), 6);
    for limit in [0, 5, 7, 12] {
        assert!(check(&cases, Some(limit)).is_err());
    }
    assert_eq!(check(&cases, Some(6)).unwrap(), 6);
    assert!(dry(&cases, Some(6)).unwrap().contains("W3/W4/R1/R4=0"));
    assert!(!root.exists());
    let mut sent = 0;
    let report = evaluate_cases(&cases, Some(6), &mut |c| {
        assert_eq!(c.stage, "w1");
        assert_eq!(c.model.label, "af196-pro-low");
        assert_eq!(c.model.effort, ReasoningEffort::Low);
        assert_eq!(c.timeout_seconds, Some(360));
        assert_eq!(c.wire_request["max_output_tokens"], 32768);
        assert!(
            c.wire_request["instructions"]
                .as_str()
                .unwrap()
                .contains(AF196_RULE)
        );
        let auth: Value = serde_json::from_slice(
            &std::fs::read(root.join("campaign/authorization.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(auth["limit"], 6);
        assert_eq!(items(&auth["requests"]).len(), 6);
        sent += 1;
        assert_eq!(count_sends([root.join("campaign/sends")]).unwrap(), sent);
        let started = std::fs::read_dir(root.join("campaign/sends"))
            .unwrap()
            .filter_map(|p| {
                let p = p.unwrap().path();
                (p.extension().is_some_and(|e| e == "json"))
                    .then(|| serde_json::from_slice::<Value>(&std::fs::read(p).unwrap()).unwrap())
            })
            .any(|log| log["slot"] == campaign_slot(c) && log["state"] == "started");
        assert!(started, "physical send must follow durable started");
        Ok(fake_envelope(
            serde_json::to_value(adapter::expand(&c.sample["input"]).unwrap()).unwrap(),
        ))
    })
    .unwrap();
    assert_eq!(report["sent_calls"], 6);
    assert!(
        items(&report["results"])
            .iter()
            .all(|r| r["status"] == "通过")
    );
    let replay = plan_for(
        &root,
        "ep-af193-synthetic-pro",
        "https://af193.invalid/v1",
        true,
    )
    .unwrap();
    assert_eq!(check(&replay, Some(0)).unwrap(), 0);
    assert_eq!(
        evaluate_cases(&replay, Some(0), &mut |_| panic!("replay cannot send")).unwrap()["sent_calls"],
        0
    );
    assert!(!root.join("recordings/w3").exists());
    let changed = plan_for(
        &root,
        "ep-af196-different-pro",
        "https://af193.invalid/v1",
        true,
    )
    .unwrap();
    assert!(check(&changed, Some(6)).is_err());
}

#[test]
fn af196_old_w1_is_stale_w3_is_unchanged_and_round_one_stays_intact() {
    assert!(write_models::W1_PROMPT.contains(AF196_RULE));
    let temp = TempRecords::new();
    let first_root = temp.0.join("af193");
    let cases = plan(
        &first_root,
        "ep-af193-synthetic-pro",
        "https://af193.invalid/v1",
    )
    .unwrap();
    for c in &cases {
        let mut old_request = c.request.clone();
        if c.stage == "w1" {
            old_request["instructions"] = json!(
                c.request["instructions"]
                    .as_str()
                    .unwrap()
                    .replace(AF196_RULE, "")
            );
        }
        let old = case_from_request_with_provider(
            c.stage.clone(),
            c.sample.clone(),
            c.path.parent().unwrap(),
            &c.model,
            c.repeat,
            old_request,
            Some(&ProviderOptions {
                provider: c.provider.clone().unwrap(),
                campaign: c.campaign.clone().unwrap(),
            }),
            c.timeout_seconds,
        )
        .unwrap();
        let envelope = compare_fixture_response(&old);
        save_recording(
            &old,
            &envelope,
            Some(&completed_text(&envelope).unwrap()),
            None,
        )
        .unwrap();
    }
    let before = cases
        .iter()
        .map(|c| std::fs::read(&c.path).unwrap())
        .collect::<Vec<_>>();
    let current = plan(
        &first_root,
        "ep-af193-synthetic-pro",
        "https://af193.invalid/v1",
    )
    .unwrap();
    assert_eq!(
        current
            .iter()
            .filter(|c| c.stage == "w1" && matches!(c.replay, Replay::Stale))
            .count(),
        6
    );
    assert_eq!(
        current
            .iter()
            .filter(|c| c.stage == "w3" && matches!(c.replay, Replay::Output(_)))
            .count(),
        6
    );
    let second_root = temp.0.join("af196");
    let second = plan_for(
        &second_root,
        "ep-af193-synthetic-pro",
        "https://af193.invalid/v1",
        true,
    )
    .unwrap();
    assert_eq!(check(&second, Some(6)).unwrap(), 6);
    assert!(!second_root.exists());
    for (c, bytes) in current.iter().zip(before) {
        assert_eq!(std::fs::read(&c.path).unwrap(), bytes);
    }
    let input = |c: &Case| c.request["input"].clone();
    for (a, b) in current.iter().filter(|c| c.stage == "w1").zip(&second) {
        assert_eq!(input(a), input(b));
        assert_ne!(a.campaign, b.campaign);
        assert_ne!(a.model.label, b.model.label);
    }
}
