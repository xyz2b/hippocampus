//! AF216: independent negative-fact trial; no prompt/schema/version changes.
use super::*;
pub(super) mod points;
pub(super) mod storage;

fn scenes() -> Vec<Value> {
    serde_json::from_str(include_str!("scenes.json")).unwrap()
}
fn labelled(sample: &Value) -> Value {
    let branches = expectation_branches(&sample["w1_expect"])
        .into_iter()
        .map(|e| {
            let mut s = sample.clone();
            s["w1_expect"] = e.clone();
            s["expect"] = e;
            let mut expanded = af206::labelled(&s)["expect"].clone();
            for (i, c) in items(&s["w1_expect"]["candidates"]).iter().enumerate() {
                if c["relation"] != "fact" {
                    expanded["candidates"][i]["value"] = c["value"].clone();
                }
            }
            expanded
        })
        .collect::<Vec<_>>();
    let primary = branches[0].clone();
    let mut result = sample.clone();
    result["expect"] = primary;
    if branches.len() > 1 {
        result["expect"]["alternatives"] = json!(branches);
    }
    result
}
pub(super) fn score_w1(sample: &Value, text: &str) -> Value {
    score_raw("w1", &labelled(sample), Some(text))
}
pub(super) fn score_w3(sample: &Value, text: &str) -> Value {
    let mut row = af198::score_w3(sample, text);
    if row["mechanical_valid"] == true {
        let output = W3Output::decode(text).unwrap();
        let batch: w2::Batch = serde_json::from_value(sample["af198_final_w2"].clone()).unwrap();
        let branches = expectation_branches(&labelled(sample)["expect"]);
        let source = sample["input"]["text"].as_str().unwrap();
        let mut known = 0;
        let mut refused = 0;
        for r in &output.reviews {
            let c = &batch
                .candidates
                .iter()
                .find(|c| c.index == r.candidate)
                .unwrap()
                .candidate;
            let matched = branches.iter().any(|b| {
                items(&b["candidates"])
                    .iter()
                    .any(|e| candidate_matches(&normalize_candidate(&json!(c), e), e, source))
            });
            known += usize::from(matched);
            refused += usize::from(matched && r.verdict != Verdict::Supported);
            let review = row["candidate_reviews"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|v| v["candidate"] == r.candidate)
                .unwrap();
            review["labelled_assertion_preserved"] = json!(matched);
            review["semantic_review"] = json!(if matched {
                "labelled"
            } else {
                "unmatched_actual_W1_requires_review"
            });
        }
        row["labelled_candidates"] = json!(known);
        row["labelled_review_failures"] = json!(refused);
        row["actual_commit"] = match af206::commit_probe(sample, &output, &batch) {
            Ok(probe) => probe,
            Err(e) => json!({"passed":false,"error":e}),
        };
        row["status"] = json!(if row["actual_commit"]["passed"] == true && refused == 0 {
            "核验通过（未匹配语义待复核）"
        } else {
            "失败"
        });
    }
    row["scoring_scope"] =
        json!("mechanics-verdicts-actual-commit; generic/dedicated alternatives; draft labels");
    row
}
pub(super) fn run() -> Result<(), String> {
    let mode = std::env::var("EVAL_AF216").map_err(|e| e.to_string())?;
    if !matches!(mode.as_str(), "chain" | "points") {
        return Err("EVAL_AF216 accepts chain or points".into());
    }
    for key in [
        "EVAL_AF206",
        "EVAL_AF198",
        "EVAL_AF193",
        "EVAL_AF196",
        "EVAL_COMPARE",
        "EVAL_SMOKE",
        "EVAL_M15",
        "EVAL_CASES",
        "EVAL_STAGES",
        "EVAL_REPEAT",
        "EVAL_MODEL",
        "EVAL_MODEL_LABEL",
        "EVAL_PROVIDER_CONFIG",
        "EVAL_REASONING_EFFORT",
        "EVAL_CAMPAIGN_DIR",
        "EVAL_BASE_URL",
        "EVAL_API_KEY",
        "EVAL_MAX_OUTPUT_TOKENS",
    ] {
        if std::env::var_os(key).is_some() {
            return Err(format!("AF216 frozen cohort rejects {key}"));
        }
    }
    if mode == "points" {
        return points::run();
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/eval-negative-af216/chain");
    let model =
        std::env::var("DEEPSEEK_PRO_MODEL").unwrap_or_else(|_| "ep-af216-unresolved-pro".into());
    let base = std::env::var("HUOSHAN_DEEPSEEK_BASE_URL")
        .unwrap_or_else(|_| "https://af216.invalid/v1".into());
    let trial = af198::plan_scenes(&root, &model, &base, "af216-pro-low", "AF216", scenes())?;
    let limit = limit()?;
    if std::env::var("EVAL_DRY_RUN").as_deref() == Ok("1") {
        println!(
            "{}\nAF216 独立 points 固定8槽（W4 4 + R4 4）；合计上限24。旧记录与R1计划注入，R1=0；不使用私有原话。",
            trial.dry(limit)?
        );
        return Ok(());
    }
    let live = live(limit, 16, &model, &base)?;
    let mut report = af198::evaluate(&trial, limit, &mut |c| send(&live, c))?;
    report["trial"] = json!("AF216-chain");
    report["summary"]["actual_commit_passed"] = json!(
        items(&report["results"])
            .iter()
            .filter(|r| r["actual_commit"]["passed"] == true)
            .count()
    );
    report["note"] = json!(
        "fictional draft labels; generic/dedicated alternatives; W4/R4 points independent, no additional R1 calls"
    );
    m15_write_json(&root.join("report.json"), &report)?;
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    Ok(())
}
fn limit() -> Result<Option<usize>, String> {
    std::env::var("EVAL_RECORD")
        .ok()
        .map(|n| n.parse().map_err(|_| "EVAL_RECORD integer".into()))
        .transpose()
}
type Live = Option<(reqwest::blocking::Client, String)>;
fn live(limit: Option<usize>, cap: usize, model: &str, base: &str) -> Result<Live, String> {
    if limit != Some(cap) {
        return Ok(None);
    }
    if std::env::var("HIPPOCAMPUS_MODEL_NETWORK").as_deref() != Ok("live")
        || model == "ep-af216-unresolved-pro"
        || base == "https://af216.invalid/v1"
    {
        return Err("AF216 live gate and real DEEPSEEK model/base required; nothing sent".into());
    }
    let key = std::env::var("HUOSHAN_DEEPSEEK_API_KEY")
        .map_err(|_| "missing HUOSHAN_DEEPSEEK_API_KEY")?;
    if key.trim().is_empty() {
        return Err("missing HUOSHAN_DEEPSEEK_API_KEY".into());
    }
    Ok(Some((
        reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()
            .map_err(|e| e.to_string())?,
        key,
    )))
}
fn send(live: &Live, c: &Case) -> Result<Value, String> {
    let (http, key) = live.as_ref().ok_or("offline AF216 cannot send")?;
    let response = http
        .execute(smoke_http_request(http, c, key)?)
        .map_err(|e| e.to_string())?;
    model_provider::read_http_response(response, ProviderProtocol::OpenaiResponses)
        .map(|(_, v)| v)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn af216_legal_dedicated_and_mixed_branches_do_not_hide_mechanical_failures() {
        for sample in scenes() {
            for branch in expectation_branches(&sample["w1_expect"]) {
                let mut input = sample["input"].clone();
                input["candidates"] = branch["candidates"].clone();
                let output = adapter::expand(&input).unwrap();
                let report = score_w1(&sample, &json!(output).to_string());
                assert_eq!(report["status"], "通过", "{}: {report}", sample["id"]);
                let mut bad = json!(output);
                bad["candidates"][0]["subject"] = json!({"entity_id":"not-in-catalog"});
                assert_eq!(score_w1(&sample, &bad.to_string())["status"], "失败");
            }
        }
    }

    #[test]
    fn af216_sixteen_upstream_slots_blind_inputs_strict_replay_and_negative_admission() {
        let temp = TempRecords::new();
        let trial = af198::plan_scenes(
            &temp.0,
            "ep-fixture",
            "https://af216.invalid/v1",
            "af216-pro-low",
            "AF216",
            scenes(),
        )
        .unwrap();
        assert!(trial.dry(Some(16)).unwrap().contains("新增调用上限 16"));
        assert!(trial.dry(Some(24)).is_err());
        assert!(!temp.0.exists());
        for c in &trial.w1 {
            let input: Value = serde_json::from_str(c.request["input"].as_str().unwrap()).unwrap();
            assert!(input.get("expect").is_none() && input.get("model").is_none());
            assert_ne!(input["source_snapshot"]["eval_sample"], c.sample["id"]);
            for prompt in [
                write_models::W1_PROMPT,
                write_models::W3_PROMPT,
                write_models::W4_PROMPT,
                hippocampus::recall_models::R4_PROMPT,
            ] {
                assert!(!prompt.contains(c.sample["input"]["text"].as_str().unwrap()));
            }
        }
        let report=af198::evaluate(&trial,Some(16),&mut |c|{
            Ok(fake_envelope(if c.stage=="w1" {json!(adapter::expand(&c.sample["input"]).unwrap())}else{
                let input:Value=serde_json::from_str(c.request["input"].as_str().unwrap()).unwrap();
                json!({"reviews":items(&input["candidates"]).iter().map(|c|json!({"candidate":c["index"],"verdict":"supported","problems":[],"episode_value":"not_applicable"})).collect::<Vec<_>>(),
                    "coverage":{"status":"complete","missing":[]},"request_reviews":[]})
            }))
        }).unwrap();
        assert_eq!(report["sent_calls"], 16);
        for r in items(&report["results"])
            .iter()
            .filter(|r| r["stage"] == "w3")
        {
            assert_eq!(r["actual_commit"]["passed"], true, "{r}");
            assert_eq!(
                r["actual_commit"]["record_count"],
                if r["id"] == "n01" { 2 } else { 1 }
            );
        }
        let replay = af198::evaluate(
            &af198::plan_scenes(
                &temp.0,
                "ep-fixture",
                "https://af216.invalid/v1",
                "af216-pro-low",
                "AF216",
                scenes(),
            )
            .unwrap(),
            Some(0),
            &mut |_| panic!("no send"),
        )
        .unwrap();
        assert_eq!(replay["sent_calls"], 0);
        assert!(
            af198::plan_scenes(
                &temp.0,
                "ep-changed",
                "https://af216.invalid/v1",
                "af216-pro-low",
                "AF216",
                scenes()
            )
            .is_err()
        );
    }
}
