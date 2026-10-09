//! AF198: actual W1 -> production W2 -> W3, with one attempt per dependent slot.
use super::*;

const CAP: usize = 24;
const LABEL: &str = "af198-pro-low";

struct Trial {
    root: PathBuf,
    w1: Vec<Case>,
    authorization: Value,
}

fn slot_root(root: &Path, repeat: usize, stage: &str, id: &str) -> PathBuf {
    root.join("campaign/slots")
        .join(format!("repeat-{repeat}-{stage}-{id}"))
}

fn case(
    root: &Path,
    stage: &str,
    sample: Value,
    model: &EvalModel,
    repeat: usize,
    request: Value,
) -> Result<Case, String> {
    let provider = serde_json::from_value(json!({"protocol":"openai_responses",
        "base_url_env":"DEEPSEEK_BASE_URL","api_key_env":"DEEPSEEK_API_KEY"}))
    .unwrap();
    let options = ProviderOptions {
        provider,
        campaign: slot_root(root, repeat, stage, sample["id"].as_str().unwrap()),
    };
    let mut cases = vec![case_from_request_with_provider(
        stage.into(),
        sample,
        &root.join(format!("recordings/repeat-{repeat}/{stage}")),
        model,
        repeat,
        request,
        Some(&options),
        Some(360),
    )?];
    compare_mark_interrupted(&mut cases)?;
    Ok(cases.remove(0))
}

fn w3_request(w1: &Case, batch: Option<&w2::Batch>) -> Value {
    let mut input: Value = serde_json::from_str(w1.request["input"].as_str().unwrap()).unwrap();
    input["contract"] = json!("w3.v3");
    input["candidates"] = json!(
        batch
            .map(|b| b
                .candidates
                .iter()
                .filter(|c| c.status != w2::Status::Rejected)
                .collect::<Vec<_>>())
            .unwrap_or_default()
    );
    input["memory_requests"] = json!(
        batch
            .map(|b| &b.memory_requests)
            .cloned()
            .unwrap_or_default()
    );
    let mut budget = ModelBudgetConfig::default();
    budget.review_output_tokens = 16384;
    let mut request = structured_request(
        ModelStage::W3,
        "w3_v3",
        write_models::W3_PROMPT,
        input,
        write_models::w3_schema(),
        &budget,
    );
    request["model"] = w1.request["model"].clone();
    w1.model.effort.apply(&mut request);
    request
}

fn plan(root: &Path, model: &str, base: &str) -> Result<Trial, String> {
    let config: hippocampus::chat::ChatConfig =
        serde_json::from_value(json!({"write_models":{}})).unwrap();
    let roles = config.write_models.as_ref().unwrap();
    // This cohort freezes the actual production defaults, rather than accepting effort overrides.
    if roles.effort("w1") != ReasoningEffort::Low || roles.effort("w3") != ReasoningEffort::Low {
        return Err("AF198 requires production W1/W3 low defaults".into());
    }
    let catalog = w2::relation_catalog(&config.predicates, &config.memory_types)
        .map_err(|e| e.to_string())?;
    let mut w1 = Vec::new();
    for scene in af193::scenes() {
        for repeat in 1..=2 {
            let mut sample = scene.clone();
            sample["expect"] = sample["w1_expect"].clone();
            let model = EvalModel {
                label: LABEL.into(),
                model: Some(model.into()),
                base: Some(base.trim_end_matches('/').into()),
                effort: roles.effort("w1"),
            };
            let mut request = stage_request_with_output_tokens("w1", &sample, 32768)?;
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
            w1.push(case(root, "w1", sample, &model, repeat, request)?);
        }
    }
    // W3's actual request cannot exist yet. Freeze its recipe and upstream identity now;
    // its one-slot ledger freezes the exact derived request before it can be sent.
    let slots: Vec<_> = w1.iter().flat_map(|c| [
        json!({"slot":campaign_slot(c)}),
        json!({"id":c.sample["id"],"stage":"w3","repeat":c.repeat,
            "upstream_request":case_fingerprint(c),"w2_context":serde_json::from_str::<Value>(c.request["input"].as_str().unwrap()).unwrap(),
            "recipe":"actual-w1-production-w2.v1","request_template":w3_request(c,None),
            "provider":c.provider,"timeout_seconds":c.timeout_seconds}),
    ]).collect();
    let trial = Trial {
        root: root.into(),
        w1,
        authorization: json!({"trial":"AF198","limit":CAP,"slots":slots}),
    };
    trial.check(None)?;
    Ok(trial)
}

fn read_json(path: &Path) -> Result<Value, String> {
    serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
        .map_err(|e| format!("{}: {e}", path.display()))
}

fn exact_file(path: &Path, expected: &Value) -> Result<(), String> {
    if path.exists() && read_json(path)? != *expected {
        return Err(format!(
            "AF198 binding changed: {}; use a separately authorized campaign",
            path.display()
        ));
    }
    Ok(())
}

// Validate using the catalog, source clock and identities actually visible to W1.
// The generic stage scoring adapter intentionally has looser sample relation types.
fn final_batch(c: &Case) -> Result<w2::Batch, String> {
    let Replay::Output(text) = &c.replay else {
        return Err("w1_unavailable".into());
    };
    let input: Value = serde_json::from_str(c.request["input"].as_str().unwrap()).unwrap();
    let entities: Vec<_> = items(&input["entities"])
        .iter()
        .map(|e| EntityId::from(e["id"].as_str().unwrap()))
        .collect();
    let relations: Vec<_> = items(&input["relations"])
        .iter()
        .map(|r| {
            Ok(w2::Relation {
                id: r["id"].as_str().unwrap().into(),
                memory_types: items(&r["memory_types"])
                    .iter()
                    .map(|t| match t.as_str() {
                        Some("fact") => Ok(MemoryType::Fact),
                        Some("preference") => Ok(MemoryType::Preference),
                        Some("constraint") => Ok(MemoryType::Constraint),
                        Some("episode") => Ok(MemoryType::Episode),
                        _ => Err("unknown production memory type".to_owned()),
                    })
                    .collect::<Result<_, _>>()?,
            })
        })
        .collect::<Result<_, String>>()?;
    w2::validate(
        w2::decode(text).map_err(|e| e.to_string())?,
        &w2::Context {
            text: input["current_user_message"].as_str().unwrap(),
            entities: &entities,
            relations: &relations,
            said_on: input["said_on"].as_str(),
            utc_offset_seconds: input["utc_offset_seconds"].as_i64().unwrap() as i32,
            task_id: None,
        },
    )
    .map_err(|e| e.to_string())
}

fn dependent_case(root: &Path, w1: &Case) -> Result<Option<Case>, String> {
    let Ok(batch) = final_batch(w1) else {
        return Ok(None);
    };
    if !batch
        .candidates
        .iter()
        .any(|c| c.status != w2::Status::Rejected)
    {
        return Ok(None);
    }
    let mut sample = w1.sample.clone();
    sample["af198_final_w2"] = json!(batch);
    let request = w3_request(w1, Some(&batch));
    sample["af198_source_snapshot"] = serde_json::from_str::<Value>(
        request["input"].as_str().unwrap(),
    )
    .unwrap()["source_snapshot"]
        .clone();
    let c = case(root, "w3", sample, &w1.model, w1.repeat, request)?;
    let Replay::Output(text) = &w1.replay else {
        unreachable!()
    };
    exact_file(
        &c.campaign.as_ref().unwrap().join("dependency.json"),
        &json!({"upstream_request":case_fingerprint(w1),
            "upstream_output":hippocampus::recall_models::fingerprint(&json!(text)),
            "final_w2":batch,"request":case_fingerprint(&c)}),
    )?;
    Ok(Some(c))
}

impl Trial {
    fn occupied(&self) -> Result<usize, String> {
        count_sends(self.w1.iter().flat_map(|c| {
            ["w1", "w3"].map(|stage| {
                slot_root(
                    &self.root,
                    c.repeat,
                    stage,
                    c.sample["id"].as_str().unwrap(),
                )
                .join("sends")
            })
        }))
    }

    fn check(&self, limit: Option<usize>) -> Result<(), String> {
        if self.w1.len() != 12 || limit.is_some_and(|l| l != 0 && l != CAP) {
            return Err("AF198 freezes 24 slots; EVAL_RECORD must be 24 or 0".into());
        }
        exact_file(
            &self.root.join("campaign/authorization.json"),
            &self.authorization,
        )?;
        if self.occupied()? > CAP {
            return Err("AF198 cumulative allowance exceeded".into());
        }
        for c in &self.w1 {
            check_case(c, limit)?;
            if let Some(w3) = dependent_case(&self.root, c)? {
                check_case(&w3, limit)?;
            }
        }
        Ok(())
    }

    fn dry(&self, limit: Option<usize>) -> Result<String, String> {
        self.check(limit)?;
        let mut w3_ready = 0;
        let mut w3_reused = 0;
        let mut w3_wait = 0;
        let mut blocked = 0;
        for c in &self.w1 {
            if let Some(w3) = dependent_case(&self.root, c)? {
                if matches!(w3.replay, Replay::Missing) {
                    w3_ready += 1;
                } else if matches!(w3.replay, Replay::Output(_) | Replay::Failed(_)) {
                    w3_reused += 1;
                } else {
                    blocked += 1;
                }
            } else if matches!(c.replay, Replay::Missing) {
                w3_wait += 1;
            } else {
                blocked += 1;
            }
        }
        let missing = self
            .w1
            .iter()
            .filter(|c| matches!(c.replay, Replay::Missing))
            .count();
        let reused = self
            .w1
            .iter()
            .filter(|c| matches!(c.replay, Replay::Output(_) | Replay::Failed(_)))
            .count();
        let bytes: usize = self
            .w1
            .iter()
            .map(|c| c.wire_request.to_string().len())
            .sum();
        Ok(format!(
            "AF198：6 个虚构场景 × 2 次重复 × W1→W2→W3 = 24 槽（W1 12 + W3 12）。\n\
            生产缺省：DeepSeek Pro；W1 low/32768，W3 low/16384；超时均 360 秒。\n\
            W1：待录制 {missing}，可复用 {reused}；W3：已确定待录制 {w3_ready}，可复用 {w3_reused}，待该次 W1→W2 输入 {w3_wait}，阻断/在途 {blocked}。\n\
            独立 campaign 固定额度 24；累计已发送/在途 {}；新增调用上限 {}（W3 无可核验候选则跳过，不用固定候选补位）。\n\
            先冻结完整槽位/构造规则，再将 W3 绑定至该次 W1 输出、完整 W2 与实际请求；发送前落盘，不重试、不重发在途槽。\n\
            W1 请求合计 {bytes} 字节；输入 token 粗估 UTF-8 字节上界 {bytes}；W3 输入依赖尚未产生，不伪报精确 token。\n\
            输出配置容量 589824 token（含推理，非预期用量）；W4/R1/R4=0。\n\
            本次真实调用 0；不读取密钥、不创建目录/账本/录制；模型/地址{}。",
            self.occupied()?,
            missing + w3_ready + w3_wait,
            if self.w1[0].model.model.as_deref() == Some("ep-af198-unresolved-pro")
                || self.w1[0].model.base.as_deref() == Some("https://af198.invalid/v1")
            {
                "未设置，使用干跑占位；真实录制拒绝占位"
            } else {
                "已加载（不输出值）"
            }
        ))
    }
}

fn check_case(c: &Case, limit: Option<usize>) -> Result<(), String> {
    match &c.replay {
        Replay::Rejected(reason) => return Err(format!("AF198 provider rejected: {reason}")),
        Replay::Stale => {
            return Err("AF198 recording stale; no fingerprint bypass or resend".into());
        }
        Replay::Missing if limit == Some(0) => {
            return Err("AF198 strict replay has missing recordings".into());
        }
        _ => {}
    }
    campaign_check(std::slice::from_ref(c), 1)
}

// No fixed supported labels for W3: validate mechanics and record the model's judgments.
pub(super) fn score_w3(sample: &Value, text: &str) -> Value {
    let parsed = W3Output::decode(text);
    let raw = serde_json::from_str::<Value>(text).unwrap_or(json!(text));
    let batch: w2::Batch = serde_json::from_value(sample["af198_final_w2"].clone()).unwrap();
    let reviewed = parsed.and_then(|o| {
        o.validate(
            &batch,
            &sample["af198_source_snapshot"],
            sample["input"]["text"].as_str().unwrap(),
        )
    });
    let mut rows = Vec::new();
    let mut time_refused = 0;
    if let Ok(checked) = &reviewed {
        for r in &checked.output().reviews {
            let c = &batch
                .candidates
                .iter()
                .find(|c| c.index == r.candidate)
                .unwrap()
                .candidate;
            let normalized = normalize_candidate(&json!(c), &json!({}));
            let expected = items(&sample["w1_expect"]["candidates"]).iter().find(|e| {
                e["relation"] == normalized["relation"]
                    && e["value"] == normalized["value"]
                    && e["subject"] == normalized["subject"]
            });
            let preserved = expected.map(|e| {
                !items(&e["times"]).is_empty()
                    && items(&e["times"]).iter().all(|t| {
                        c.times
                            .iter()
                            .any(|actual| Some(actual.quote.text.as_str()) == t["quote"].as_str())
                    })
            });
            let refused = r.verdict != Verdict::Supported
                && r.problems.iter().any(|p| p.aspect == ReviewAspect::Time);
            let retained_time_refusal = preserved == Some(true) && refused;
            time_refused += usize::from(retained_time_refusal);
            rows.push(json!({"candidate":r.candidate,"verdict":r.verdict,"problems":r.problems,
                "episode_value":r.episode_value,"actual_times":c.times,
                "label_time_quotes_preserved":preserved,"retained_time_refusal":retained_time_refusal}));
        }
    } else {
        // Keep even invalid judgments visible; never include them in the refusal statistic.
        rows = items(&raw["reviews"])
            .iter()
            .map(|r| {
                json!({
                    "candidate":r["candidate"],"verdict":r["verdict"],"problems":r["problems"],
                    "mechanically_unverified":true,"retained_time_refusal":false,
                })
            })
            .collect();
    }
    let valid = reviewed.is_ok();
    json!({"id":sample["id"],"stage":"w3","status":if valid{"核验通过（语义仅观察）"}else{"失败"},
        "scoring_scope":"mechanics-and-verdict-observation","mechanical_valid":reviewed.is_ok(),
        "error":reviewed.err().map(|e|e.to_string()),"raw_output":raw,"candidate_reviews":rows,
        "qualifiers_retained_time_refusals":time_refused,"fields":[{"field":"w3.validate","passed":valid}]})
}

fn evaluate(
    trial: &Trial,
    limit: Option<usize>,
    send: &mut dyn FnMut(&Case) -> Result<Value, String>,
) -> Result<Value, String> {
    trial.check(limit)?;
    let _lock = if limit == Some(CAP) {
        Some(w3_trial_lock(&trial.root.join("campaign"))?)
    } else {
        None
    };
    trial.check(limit)?;
    if limit == Some(CAP) {
        let path = trial.root.join("campaign/authorization.json");
        if !path.exists() {
            m15_write_json(&path, &trial.authorization)?;
        }
    }
    let mut results = Vec::new();
    let mut sent = 0;
    for planned in &trial.w1 {
        let report = evaluate_cases(
            std::slice::from_ref(planned),
            limit.map(|l| usize::from(l > 0)),
            send,
        )?;
        sent += report["sent_calls"].as_u64().unwrap();
        let mut row = report["results"][0].clone();
        // Reload the response saved by this attempt, never substitute the scene's ideal candidates.
        let actual = case(
            &trial.root,
            "w1",
            planned.sample.clone(),
            &planned.model,
            planned.repeat,
            planned.request.clone(),
        )?;
        let final_w2 = final_batch(&actual);
        row["production_w2"] = json!(final_w2.as_ref().ok());
        row["production_w2_error"] = json!(final_w2.as_ref().err());
        results.push(row);
        let Some(w3) = dependent_case(&trial.root, &actual)? else {
            results.push(json!({"id":actual.sample["id"],"repeat":actual.repeat,"stage":"w3",
                "status":"上游阻断（未发送）","sent":false,"reason":final_w2.err().unwrap_or_else(||"no_reviewable_candidates".into())}));
            continue;
        };
        if limit == Some(CAP) {
            let Replay::Output(text) = &actual.replay else {
                unreachable!()
            };
            let path = w3.campaign.as_ref().unwrap().join("dependency.json");
            if !path.exists() {
                m15_write_json(
                    &path,
                    &json!({"upstream_request":case_fingerprint(&actual),
                "upstream_output":hippocampus::recall_models::fingerprint(&json!(text)),
                "final_w2":w3.sample["af198_final_w2"],"request":case_fingerprint(&w3)}),
                )?;
            }
        }
        let report = evaluate_cases(&[w3], limit.map(|l| usize::from(l > 0)), send)?;
        sent += report["sent_calls"].as_u64().unwrap();
        results.push(report["results"][0].clone());
    }
    let time_refused: u64 = results
        .iter()
        .filter_map(|r| r["qualifiers_retained_time_refusals"].as_u64())
        .sum();
    let cases_refused = results
        .iter()
        .filter(|r| {
            r["qualifiers_retained_time_refusals"]
                .as_u64()
                .is_some_and(|n| n > 0)
        })
        .count();
    Ok(
        json!({"trial":"AF198","authorized_calls":CAP,"sent_calls":sent,"cumulative_sent_or_inflight":trial.occupied()?,
        "stage_only":false,"chain":"actual W1 -> production W2 -> W3","results":results,
        "summary":{"w1_passed":results.iter().filter(|r|r["stage"]=="w1" && r["status"]=="通过").count(),
            "w3_mechanically_valid":results.iter().filter(|r|r["stage"]=="w3" && r["mechanical_valid"]==true).count(),
            "qualifiers_retained_time_refusal_candidates":time_refused,"qualifiers_retained_time_refusal_cases":cases_refused},
        "count_definition":"Among mechanically valid W3 outputs: a non-supported review declares aspect=time, and all existing label time quotes for the same subject/relation/value remain in that actual W2 candidate's times. No semantic role inference by code; unmatched assertions are unknown, not counted.",
        "label_status":"draft","note":"W1 uses unchanged AF193 labels; W3 verdicts are observations, not fixed supported expectations. Two repeats do not establish statistical significance."}),
    )
}

pub(super) fn run() -> Result<(), String> {
    if std::env::var("EVAL_AF198").as_deref() != Ok("1") {
        return Err("EVAL_AF198 accepts 1".into());
    }
    for key in [
        "EVAL_AF193",
        "EVAL_AF196",
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
        "EVAL_CAMPAIGN_DIR",
    ] {
        if std::env::var_os(key).is_some() {
            return Err(format!("AF198 frozen cohort rejects {key}"));
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/eval-time-af198");
    let model =
        std::env::var("DEEPSEEK_PRO_MODEL").unwrap_or_else(|_| "ep-af198-unresolved-pro".into());
    let base =
        std::env::var("DEEPSEEK_BASE_URL").unwrap_or_else(|_| "https://af198.invalid/v1".into());
    let trial = plan(&root, &model, &base)?;
    let limit = std::env::var("EVAL_RECORD")
        .ok()
        .map(|s| {
            s.parse::<usize>()
                .map_err(|_| "EVAL_RECORD integer".to_owned())
        })
        .transpose()?;
    if std::env::var("EVAL_DRY_RUN").as_deref() == Ok("1") {
        println!("{}", trial.dry(limit)?);
        return Ok(());
    }
    trial.check(limit)?;
    let live = if limit == Some(CAP) {
        if std::env::var("PMR_MODEL_NETWORK").as_deref() != Ok("live")
            || model == "ep-af198-unresolved-pro"
            || base == "https://af198.invalid/v1"
        {
            return Err("AF198 recording requires live network gate and real DEEPSEEK_PRO_MODEL/BASE_URL; nothing sent".into());
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
    let report = evaluate(&trial, limit, &mut |c| {
        let (http, key) = live.as_ref().ok_or("offline AF198 cannot send")?;
        let response = http
            .execute(smoke_http_request(http, c, key)?)
            .map_err(|e| {
                if e.is_timeout() {
                    "provider_timeout".into()
                } else {
                    e.to_string()
                }
            })?;
        model_provider::read_http_response(response, ProviderProtocol::OpenaiResponses)
            .map(|(_, e)| e)
            .map_err(|e| e.to_string())
    })?;
    m15_write_json(&root.join("report.json"), &report)?;
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    if eval_gate(std::env::var("EVAL_GATE").ok().as_deref())?
        && items(&report["results"]).iter().any(|r| {
            r["stage"] == "w1" && r["status"] != "通过"
                || r["stage"] == "w3" && r["mechanical_valid"] != true
        })
    {
        return Err("AF198 score/mechanical gaps (EVAL_GATE=1)".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trial(temp: &TempRecords) -> Trial {
        plan(
            &temp.0,
            "ep-af198-synthetic-pro",
            "https://af198.invalid/v1",
        )
        .unwrap()
    }

    fn output(c: &Case) -> Value {
        if c.stage == "w1" {
            return json!(adapter::expand(&c.sample["input"]).unwrap());
        }
        let input: Value = serde_json::from_str(c.request["input"].as_str().unwrap()).unwrap();
        json!({"reviews":items(&input["candidates"]).iter().map(|c|json!({
            "candidate":c["index"],"verdict":"supported","problems":[],
            "episode_value":if c["candidate"]["memory_type"]=="episode"{"useful"}else{"not_applicable"}
        })).collect::<Vec<_>>(),"request_reviews":[],"coverage":{"status":"complete","missing":[]}})
    }

    fn ready(temp: &TempRecords, c: &Case, value: Value) -> Case {
        save_recording(
            c,
            &fake_envelope(value.clone()),
            Some(&value.to_string()),
            None,
        )
        .unwrap();
        case(
            &temp.0,
            "w1",
            c.sample.clone(),
            &c.model,
            c.repeat,
            c.request.clone(),
        )
        .unwrap()
    }

    #[test]
    fn af198_dry_freezes_twenty_four_dependent_slots_without_artifacts() {
        let temp = TempRecords::new();
        let t = trial(&temp);
        let dry = t.dry(Some(CAP)).unwrap();
        assert!(dry.contains("W1 12 + W3 12"));
        assert!(dry.contains("待该次 W1→W2 输入 12"));
        assert!(dry.contains("新增调用上限 24"));
        assert!(dry.contains("本次真实调用 0"));
        assert!(!temp.0.exists());
        for limit in [1, 12, 23, 25] {
            assert!(t.check(Some(limit)).is_err());
        }
        assert!(t.check(Some(0)).is_err());
        assert_eq!(items(&t.authorization["slots"]).len(), CAP);
        for c in &t.w1 {
            assert!(dependent_case(&temp.0, c).unwrap().is_none());
            assert!(
                c.request["instructions"]
                    .as_str()
                    .unwrap()
                    .contains("for when a state or assertion holds or held")
            );
            assert!(
                c.request["instructions"]
                    .as_str()
                    .unwrap()
                    .contains("its schedule is not the validity of the intention")
            );
            assert_eq!(c.request["max_output_tokens"], 32768);
            assert_eq!(w3_request(c, None)["max_output_tokens"], 16384);
            assert_eq!(c.model.effort, ReasoningEffort::Low);
        }
    }

    #[test]
    fn af198_actual_w1_w2_reaches_w3_and_reports_time_refusals() {
        let temp = TempRecords::new();
        let mut sequence = Vec::new();
        let report = evaluate(&trial(&temp), Some(CAP), &mut |c| {
            // The complete 24-slot recipe and this exact stage's request are durable before send.
            let auth = read_json(&temp.0.join("campaign/authorization.json")).unwrap();
            assert_eq!(auth["limit"], CAP);
            assert_eq!(items(&auth["slots"]).len(), CAP);
            let root = c.campaign.as_ref().unwrap();
            assert_eq!(
                read_json(&root.join("authorization.json")).unwrap()["limit"],
                1
            );
            assert_eq!(count_sends([root.join("sends")]).unwrap(), 1);
            sequence.push((c.sample["id"].clone(), c.repeat, c.stage.clone()));
            let mut value = output(c);
            if c.sample["id"] == "t02" {
                if c.stage == "w1" {
                    value["candidates"][0]["times"][0]["role"] = json!("event");
                } else {
                    let input: Value =
                        serde_json::from_str(c.request["input"].as_str().unwrap()).unwrap();
                    assert_eq!(
                        input["candidates"][0]["candidate"]["times"][0]["role"],
                        "event"
                    );
                    assert_eq!(
                        input["candidates"],
                        c.sample["af198_final_w2"]["candidates"]
                    );
                    assert!(input.get("expect").is_none());
                    assert!(input.get("w1_expect").is_none());
                    assert!(input.get("model").is_none());
                    assert_ne!(input["source_snapshot"]["eval_sample"], c.sample["id"]);
                    assert!(root.join("dependency.json").exists());
                    value["reviews"][0]["verdict"] = json!("contradicted");
                    value["reviews"][0]["problems"] =
                        json!([{"aspect":"time","quote":{"text":"之前"}}]);
                }
            }
            Ok(fake_envelope(value))
        })
        .unwrap();
        assert_eq!(sequence.len(), CAP);
        for pair in sequence.chunks(2) {
            assert_eq!(pair[0].0, pair[1].0);
            assert_eq!(pair[0].1, pair[1].1);
            assert_eq!(pair[0].2, "w1");
            assert_eq!(pair[1].2, "w3");
        }
        assert_eq!(report["sent_calls"], CAP);
        assert_eq!(report["cumulative_sent_or_inflight"], CAP);
        assert_eq!(report["summary"]["w1_passed"], 10);
        assert_eq!(report["summary"]["w3_mechanically_valid"], 12);
        assert_eq!(
            report["summary"]["qualifiers_retained_time_refusal_candidates"],
            2
        );
        assert_eq!(
            report["summary"]["qualifiers_retained_time_refusal_cases"],
            2
        );
        assert!(
            items(&report["results"])
                .iter()
                .filter(|r| r["stage"] == "w3")
                .all(|r| !items(&r["candidate_reviews"]).is_empty())
        );
        let replay = evaluate(&trial(&temp), Some(0), &mut |_| {
            panic!("strict replay sent")
        })
        .unwrap();
        assert_eq!(replay["sent_calls"], 0);
        assert_eq!(replay["results"], report["results"]);
    }

    #[test]
    fn af198_resume_does_not_resend_inflight_w3_or_expand_allowance() {
        let temp = TempRecords::new();
        let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            evaluate(&trial(&temp), Some(CAP), &mut |c| {
                if c.stage == "w3" {
                    panic!("inflight interruption");
                }
                Ok(fake_envelope(output(c)))
            })
            .unwrap();
        }));
        assert!(crashed.is_err());
        assert_eq!(trial(&temp).occupied().unwrap(), 2);
        let resumed = evaluate(&trial(&temp), Some(CAP), &mut |c| {
            assert!(!(c.sample["id"] == "t01" && c.repeat == 1));
            Ok(fake_envelope(output(c)))
        })
        .unwrap();
        assert_eq!(resumed["sent_calls"], 22);
        assert_eq!(resumed["cumulative_sent_or_inflight"], CAP);
        assert_eq!(
            items(&resumed["results"])
                .iter()
                .filter(|r| r["status"] == "未录制（中断在途）")
                .count(),
            1
        );
        let next = evaluate(&trial(&temp), Some(CAP), &mut |_| panic!("no retries")).unwrap();
        assert_eq!(next["sent_calls"], 0);
        assert_eq!(next["cumulative_sent_or_inflight"], CAP);
        assert!(trial(&temp).check(Some(25)).is_err());
    }

    #[test]
    fn af198_failed_or_rejected_w1_never_uses_fixed_candidates() {
        let temp = TempRecords::new();
        let report = evaluate(&trial(&temp), Some(CAP), &mut |c| {
            if c.sample["id"] == "t01" {
                assert_eq!(c.stage, "w1");
                return Err("injected provider failure".into());
            }
            let mut value = output(c);
            if c.sample["id"] == "t03" {
                assert_eq!(c.stage, "w1");
                value["candidates"][0]["subject"] = json!({"entity_id":"missing"});
            }
            Ok(fake_envelope(value))
        })
        .unwrap();
        assert_eq!(report["sent_calls"], 20);
        assert_eq!(
            items(&report["results"])
                .iter()
                .filter(|r| r["stage"] == "w3" && r["status"] == "上游阻断（未发送）")
                .count(),
            4
        );
        let replay = evaluate(&trial(&temp), Some(0), &mut |_| {
            panic!("failed W1 must not retry")
        })
        .unwrap();
        assert_eq!(replay["sent_calls"], 0);
        assert_eq!(replay["cumulative_sent_or_inflight"], 20);
    }

    #[test]
    fn af198_dependency_and_prompt_changes_fail_before_any_send() {
        let temp = TempRecords::new();
        evaluate(&trial(&temp), Some(CAP), &mut |c| {
            Ok(fake_envelope(output(c)))
        })
        .unwrap();
        assert!(plan(&temp.0, "ep-af198-changed-pro", "https://af198.invalid/v1").is_err());
        let t = trial(&temp);
        let c = &t.w1[0];
        let mut file = read_json(&c.path).unwrap();
        // Equivalent W2 data still cannot silently change the bound upstream response.
        let original = file["recorded"][0]["output_text"].as_str().unwrap();
        file["recorded"][0]["output_text"] = json!(format!(" {original}"));
        m15_write_json(&c.path, &file).unwrap();
        assert!(
            plan(
                &temp.0,
                "ep-af198-synthetic-pro",
                "https://af198.invalid/v1"
            )
            .is_err()
        );
    }

    #[test]
    fn af198_w3_mechanical_failure_is_visible_and_quotes_must_be_retained() {
        let temp = TempRecords::new();
        let t = trial(&temp);
        let w1 = ready(&temp, &t.w1[0], output(&t.w1[0]));
        assert!(final_batch(&w1).is_ok(), "{:?}", final_batch(&w1));
        let c = dependent_case(&temp.0, &w1).unwrap().unwrap();
        let mut value = output(&c);
        value["reviews"][0]["verdict"] = json!("insufficient");
        value["reviews"][0]["problems"] = json!([{"aspect":"time","quote":{"text":"现在"}}]);
        let scored = score_w3(&c.sample, &value.to_string());
        assert_eq!(scored["qualifiers_retained_time_refusals"], 1);
        let mut omitted = c.sample.clone();
        omitted["af198_final_w2"]["candidates"][0]["candidate"]["times"] = json!([]);
        assert_eq!(
            score_w3(&omitted, &value.to_string())["qualifiers_retained_time_refusals"],
            0
        );
        value["reviews"][0]["problems"][0]["quote"]["text"] = json!("unquoted fabricated words");
        let invalid = score_w3(&c.sample, &value.to_string());
        assert_eq!(invalid["mechanical_valid"], false);
        assert_eq!(invalid["status"], "失败");
        assert_eq!(invalid["raw_output"], value);
        assert_eq!(invalid["qualifiers_retained_time_refusals"], 0);
        assert_eq!(invalid["candidate_reviews"][0]["verdict"], "insufficient");
        assert_eq!(
            invalid["candidate_reviews"][0]["mechanically_unverified"],
            true
        );
    }

    #[test]
    fn af198_partial_w2_rejection_keeps_original_candidate_indices() {
        let temp = TempRecords::new();
        let t = trial(&temp);
        let c = t.w1.iter().find(|c| c.sample["id"] == "t02").unwrap();
        let mut value = output(c);
        value["candidates"][0]["subject"] = json!({"entity_id":"missing"});
        let actual = ready(&temp, c, value);
        let w3 = dependent_case(&temp.0, &actual).unwrap().unwrap();
        let input: Value = serde_json::from_str(w3.request["input"].as_str().unwrap()).unwrap();
        assert_eq!(items(&input["candidates"]).len(), 1);
        assert_eq!(input["candidates"][0]["index"], 1);
        assert_eq!(
            w3.sample["af198_final_w2"]["candidates"][0]["status"],
            "rejected"
        );
        let scored = score_w3(&w3.sample, &output(&w3).to_string());
        assert_eq!(scored["mechanical_valid"], true);
        assert_eq!(scored["candidate_reviews"][0]["candidate"], 1);
    }

    #[test]
    fn af198_role_rule_changes_w1_identity_but_not_w3_prompt_or_schema() {
        let temp = TempRecords::new();
        let t = trial(&temp);
        let current = &t.w1[0];
        let rule = "Choose the role by what the time qualifies, not by its past or future direction. Use validity\nfor when a state or assertion holds or held. Use event for when an activity, planned action,\narrangement or occurrence takes place; its schedule is not the validity of the intention.\n";
        let before = write_models::W1_PROMPT.replace(rule, "");
        assert_ne!(before, write_models::W1_PROMPT);
        let mut old_request = current.request.clone();
        af193::replace_prompt(&mut old_request, ModelStage::W1, &before);
        let old = case(
            &temp.0,
            "w1",
            current.sample.clone(),
            &current.model,
            current.repeat,
            old_request,
        )
        .unwrap();
        assert_ne!(case_fingerprint(&old), case_fingerprint(current));
        assert_eq!(
            old.request["text"]["format"]["schema"],
            current.request["text"]["format"]["schema"]
        );
        assert_eq!(w3_request(&old, None), w3_request(current, None));
        ready(&temp, &old, output(&old));
        let stale = case(
            &temp.0,
            "w1",
            current.sample.clone(),
            &current.model,
            current.repeat,
            current.request.clone(),
        )
        .unwrap();
        assert!(matches!(stale.replay, Replay::Stale));
        assert!(check_case(&stale, Some(CAP)).is_err());
    }
}
