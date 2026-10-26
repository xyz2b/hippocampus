//! AF206: independent, fictional fragment-value cohort; actual W1 -> W2 -> W3 -> SQLite.
use super::*;
mod points;
use hippocampus::chat::{ChatConfig, ChatMemoryService};
use hippocampus::enrichment::{RuntimeInputBudget, UpstreamBatch};
use hippocampus::{MemoryRepository, SqliteMemoryStore, UserId, w4_start_round};

fn scenes() -> Vec<Value> {
    serde_json::from_str(include_str!("scenes.json")).unwrap()
}

// Independent assertions have explicitly labelled locating spans, not a mandatory shortest value.
// Longer continuous spans are accepted when they contain the labelled content and no sibling claim.
pub(super) fn labelled(sample: &Value) -> Value {
    let mut adjusted = sample.clone();
    let sentence = sample["input"]["text"].as_str().unwrap();
    let candidates = items(&sample["w1_expect"]["candidates"]);
    let boundaries: Vec<_> = sentence
        .char_indices()
        .map(|(i, _)| i)
        .chain([sentence.len()])
        .collect();
    for (index, e) in candidates.iter().enumerate() {
        let anchor = e["value"].as_str().unwrap();
        let mut accepted = Vec::new();
        for (i, start) in boundaries.iter().enumerate() {
            for end in &boundaries[i + 1..] {
                let span = &sentence[*start..*end];
                if span.contains(anchor)
                    && !candidates.iter().enumerate().any(|(j, other)| {
                        j != index && span.contains(other["value"].as_str().unwrap())
                    })
                {
                    accepted.push(json!(span));
                }
            }
        }
        adjusted["expect"]["candidates"][index]["value"] = json!(accepted);
    }
    adjusted
}

pub(super) fn score_w1(sample: &Value, text: &str) -> Value {
    score_raw("w1", &labelled(sample), Some(text))
}

pub(super) fn commit_probe(
    sample: &Value,
    output: &W3Output,
    batch: &w2::Batch,
) -> Result<Value, String> {
    let dir = std::env::temp_dir().join(format!(
        "af206-commit-{}",
        hippocampus::diagnostics::new_id()
    ));
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(dir.clone());
    let mut store = SqliteMemoryStore::open(dir.join("memory.sqlite3"), UserId::from("eval"))
        .map_err(|e| e.to_string())?;
    let config: ChatConfig = serde_json::from_value(json!({"enrichment_mode":"off"})).unwrap();
    let mut service = ChatMemoryService::new(store, config).map_err(|e| e.to_string())?;
    for entity in items(&sample["input"]["entities"]) {
        if entity["id"] != "self" {
            service.call("memory_entity_upsert",json!({"id":entity["id"],"canonical_name":entity["names"][0],"entity_type":"Person","aliases":[]}),||false).map_err(|e|e.to_string())?;
        }
    }
    let saved = service.call("memory_remember", json!({"session_id":"af206","turn_id":"current",
        "text":sample["input"]["text"],"history":sample["input"]["history"],"capture_source":true,"observed_at":1791561600i64}),||false).map_err(|e|e.to_string())?;
    store = SqliteMemoryStore::open(dir.join("memory.sqlite3"), UserId::from("eval"))
        .map_err(|e| e.to_string())?;
    let state = store.read().map_err(|e| e.to_string())?;
    let key = state
        .sources
        .iter()
        .find(|(_, s)| s.id == saved["source"]["source_id"])
        .unwrap()
        .0
        .clone();
    w4_start_round(&mut store, &key, "af206-commit", false).map_err(|e| e.to_string())?;
    let receipt = hippocampus::enrichment::run_source(
        &mut store,
        &key,
        RuntimeInputBudget::default(),
        |ctx, input| {
            Ok(UpstreamBatch {
                w2: batch.clone(),
                review: Some(
                    output
                        .clone()
                        .validate(batch, &ctx.source_binding, &input.text)?,
                ),
            })
        },
        |_, _, _| Ok(None),
    )
    .map_err(|e| e.to_string())?;
    let records = store
        .read()
        .map_err(|e| e.to_string())?
        .repository
        .list_by_owner(&UserId::from("eval"))
        .map_err(|e| e.to_string())?;
    let expected_committable = batch
        .candidates
        .iter()
        .filter(|c| {
            c.status == w2::Status::Ok
                && output
                    .reviews
                    .iter()
                    .any(|r| r.candidate == c.index && r.verdict == Verdict::Supported)
        })
        .count();
    // Actual identities, full evidence and execution values must survive reopen. No semantic repair.
    let identity_valid = records
        .iter()
        .all(|m| hippocampus::w4::record_view(&store.read().unwrap(), m.id).is_some());
    Ok(
        json!({"receipt":receipt,"record_count":records.len(),"expected_supported_projectable":expected_committable,
        "committed_identity_valid":identity_valid,"passed":identity_valid && records.len()==expected_committable,
        "note":"fresh-source admission probe; history is reference context, not imported committed records; AF216 ordinary negative user_statement fact projects to Negative"}),
    )
}

pub(super) fn score_w3(sample: &Value, text: &str) -> Value {
    let mut row = af198::score_w3(sample, text);
    if let Ok(output) = W3Output::decode(text) {
        if row["mechanical_valid"] == true {
            let batch: w2::Batch =
                serde_json::from_value(sample["af198_final_w2"].clone()).unwrap();
            let labelled = labelled(sample);
            let mut failures = 0;
            let mut known = 0;
            let labels = items(&labelled["expect"]["candidates"]);
            let text = sample["input"]["text"].as_str().unwrap();
            for review in &output.reviews {
                let candidate = &batch
                    .candidates
                    .iter()
                    .find(|c| c.index == review.candidate)
                    .unwrap()
                    .candidate;
                let matched = labels
                    .iter()
                    .filter(|e| {
                        candidate_matches(&normalize_candidate(&json!(candidate), e), e, text)
                    })
                    .count()
                    == 1;
                known += usize::from(matched);
                failures += usize::from(matched && review.verdict != Verdict::Supported);
                if let Some(r) = row["candidate_reviews"]
                    .as_array_mut()
                    .unwrap()
                    .iter_mut()
                    .find(|r| r["candidate"] == review.candidate)
                {
                    r["labelled_assertion_preserved"] = json!(matched);
                    r["expected_verdict"] = if matched {
                        json!("supported")
                    } else {
                        Value::Null
                    };
                    r["semantic_review"] = json!(if matched {
                        "labelled"
                    } else {
                        "unmatched_actual_W1_requires_review"
                    });
                }
            }
            row["labelled_candidates"] = json!(known);
            row["labelled_review_failures"] = json!(failures);
            match commit_probe(sample, &output, &batch) {
                Ok(probe) => row["actual_commit"] = probe,
                Err(e) => row["actual_commit"] = json!({"passed":false,"error":e}),
            }
            row["status"] = json!(if row["actual_commit"]["passed"] == true
                && row["labelled_review_failures"] == 0
            {
                "核验通过（未匹配语义待复核）"
            } else {
                "失败"
            });
        }
    }
    row["scoring_scope"] = json!("mechanics-verdicts-and-actual-commit; semantic labels draft");
    row
}

pub(super) fn run() -> Result<(), String> {
    if !matches!(
        std::env::var("EVAL_AF206").as_deref(),
        Ok("chain" | "points" | "recount")
    ) {
        return Err("EVAL_AF206 accepts chain, points or recount".into());
    }
    for key in [
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
            return Err(format!("AF206 frozen cohort rejects {key}"));
        }
    }
    if std::env::var("EVAL_AF206").as_deref() == Ok("points") {
        return points::run();
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/eval-fact-af206/chain");
    let model =
        std::env::var("DEEPSEEK_PRO_MODEL").unwrap_or_else(|_| "ep-af206-unresolved-pro".into());
    let base = std::env::var("HUOSHAN_DEEPSEEK_BASE_URL")
        .unwrap_or_else(|_| "https://af206.invalid/v1".into());
    let trial = af198::plan_scenes(&root, &model, &base, "af206-pro-low", "AF206", scenes())?;
    let limit = std::env::var("EVAL_RECORD")
        .ok()
        .map(|v| {
            v.parse::<usize>()
                .map_err(|_| "EVAL_RECORD integer".to_owned())
        })
        .transpose()?;
    if std::env::var("EVAL_AF206").as_deref() == Ok("recount") && limit != Some(0) {
        return Err("AF206 recount requires EVAL_RECORD=0; nothing sent".into());
    }
    if std::env::var("EVAL_DRY_RUN").as_deref() == Ok("1") {
        println!(
            "{}\nAF206 另有独立 points campaign：W4 2 + R4 2，最多 4 次。串联检查断言片段与限定语，W3 后执行真实 SQLite 提交/重开；普通否定 fact 按 AF216 提交资格检查。",
            trial
                .dry(limit)?
                .replace("af198", "af206")
                .replace("W4/R1/R4=0", "本 chain 内 W4/R1/R4=0")
        );
        return Ok(());
    }
    trial.check(limit)?;
    let live = if limit == Some(24) {
        if std::env::var("PMR_MODEL_NETWORK").as_deref() != Ok("live")
            || model == "ep-af206-unresolved-pro"
            || base == "https://af206.invalid/v1"
        {
            return Err(
                "AF206 recording requires live gate and real DEEPSEEK model/base; nothing sent"
                    .into(),
            );
        }
        let key = std::env::var("HUOSHAN_DEEPSEEK_API_KEY")
            .map_err(|_| "missing HUOSHAN_DEEPSEEK_API_KEY")?;
        if key.trim().is_empty() {
            return Err("missing HUOSHAN_DEEPSEEK_API_KEY".into());
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
    let mut report = af198::evaluate(&trial, limit, &mut |c| {
        let (http, key) = live.as_ref().ok_or("offline AF206 cannot send")?;
        let response = http
            .execute(smoke_http_request(http, c, key)?)
            .map_err(|e| e.to_string())?;
        model_provider::read_http_response(response, ProviderProtocol::OpenaiResponses)
            .map(|(_, e)| e)
            .map_err(|e| e.to_string())
    })?;
    report["trial"] = json!("AF206");
    report["chain"] =
        json!("actual W1 -> production W2 -> W3 -> actual fresh-source SQLite admission");
    report["summary"]["actual_commit_passed"] = json!(
        items(&report["results"])
            .iter()
            .filter(|r| r["actual_commit"]["passed"] == true)
            .count()
    );
    report["summary"]["labelled_review_failures"] = json!(
        items(&report["results"])
            .iter()
            .filter_map(|r| r["labelled_review_failures"].as_u64())
            .sum::<u64>()
    );
    report["summary"]["labelled_candidates"] = json!(
        items(&report["results"])
            .iter()
            .filter_map(|r| r["labelled_candidates"].as_u64())
            .sum::<u64>()
    );
    report["note"] = json!(
        "fictional draft labels; history not automatically imported; W3 semantic verdicts retained; points independently tests updates"
    );
    let report_root = if std::env::var("EVAL_AF206").as_deref() == Ok("recount") {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("target/eval-negative-af216/af206-recount")
    } else {
        root.clone()
    };
    m15_write_json(&report_root.join("report.json"), &report)?;
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn af206_predecessor_recordings_are_strictly_stale_for_all_changed_stages() {
        let temp = TempRecords::new();
        let model = EvalModel {
            label: "af206-version-test".into(),
            model: Some("ep-fixture".into()),
            base: Some("https://af206.invalid/v1".into()),
            effort: ReasoningEffort::Low,
        };
        for (stage, old_version, old_schema) in [
            ("w1", "w1.v3", "w1_v3"),
            ("w3", "w3.v3", "w3_v3"),
            ("w4", "w4.v4", "w4_v4"),
            ("r4", "r4.v6", "r4_v6"),
        ] {
            let sample = samples(stage).remove(0);
            let current = stage_request_with_output_tokens(stage, &sample, 16384).unwrap();
            let mut previous = current.clone();
            previous["text"]["format"]["name"] = json!(old_schema);
            let mut input: Value =
                serde_json::from_str(previous["input"].as_str().unwrap()).unwrap();
            input["contract"] = json!(old_version);
            previous["input"] = json!(input.to_string());
            let old = af198::case(&temp.0, stage, sample.clone(), &model, 1, previous).unwrap();
            save_recording(&old, &fake_envelope(json!({})), Some("{}"), None).unwrap();
            let new = af198::case(&temp.0, stage, sample, &model, 1, current).unwrap();
            assert_ne!(case_fingerprint(&old), case_fingerprint(&new));
            assert!(matches!(new.replay, Replay::Stale), "{stage}");
        }
    }

    #[test]
    fn af206_chain_dry_and_injected_sqlite_admission() {
        let temp = TempRecords::new();
        let trial = af198::plan_scenes(
            &temp.0,
            "ep-af206-fixture",
            "https://af206.invalid/v1",
            "af206-pro-low",
            "AF206",
            scenes(),
        )
        .unwrap();
        assert!(trial.dry(Some(24)).unwrap().contains("新增调用上限 24"));
        assert!(!temp.0.exists());
        let report=af198::evaluate(&trial,Some(24),&mut |c| {
            let output=if c.stage=="w1" {json!(adapter::expand(&c.sample["input"]).unwrap())}else {
                let input:Value=serde_json::from_str(c.request["input"].as_str().unwrap()).unwrap();
                assert!(input.get("expect").is_none());assert!(input.get("model").is_none());
                assert!(input["source_snapshot"]["eval_sample"]!=c.sample["id"]);
                json!({"reviews":items(&input["candidates"]).iter().map(|c|json!({"candidate":c["index"],"verdict":"supported","problems":[],"episode_value":"not_applicable"})).collect::<Vec<_>>(),"coverage":{"status":"complete","missing":[]},"request_reviews":[]})
            };
            Ok(fake_envelope(output))
        }).unwrap();
        assert_eq!(report["sent_calls"], 24);
        for row in items(&report["results"])
            .iter()
            .filter(|r| r["stage"] == "w3")
        {
            assert_eq!(row["actual_commit"]["passed"], true, "{row}");
            if row["id"] == "f01" {
                assert_eq!(row["actual_commit"]["record_count"], 2);
            }
        }
        let replay = af198::evaluate(
            &af198::plan_scenes(
                &temp.0,
                "ep-af206-fixture",
                "https://af206.invalid/v1",
                "af206-pro-low",
                "AF206",
                scenes(),
            )
            .unwrap(),
            Some(0),
            &mut |_| panic!("must not send"),
        )
        .unwrap();
        assert_eq!(replay["sent_calls"], 0);
        for scene in scenes() {
            let text = scene["input"]["text"].as_str().unwrap();
            for prompt in [
                write_models::W1_PROMPT,
                write_models::W3_PROMPT,
                write_models::W4_PROMPT,
                hippocampus::recall_models::R4_PROMPT,
            ] {
                assert!(!prompt.contains(text));
            }
        }
    }
}
