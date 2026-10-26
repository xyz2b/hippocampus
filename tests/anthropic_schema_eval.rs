//! Independent schema campaigns. Default tests and dry runs never access a provider.
use hippocampus::{
    Entity, EntityId, ModelBudgetConfig, ModelStage, ReasoningEffort,
    contract::{R1Output, RecallEvidence, RecallRequest, W1Output, W3Output, W4Input, W4Output},
    model_provider::{self, ProviderConfig, ProviderProtocol, StructuredMode},
    r1, r4, recall_models, structured_request, w2, w4, write_models,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const IDS: [&str; 6] = ["w1", "w3", "w4", "r1", "r4-stored", "r4-current"];
const DEFAULT_ROOT: &str = "target/eval-anthropic-schema-af236";
const AF238_ROOT: &str = "target/eval-anthropic-schema-af238";
const AF240_ROOT: &str = "target/eval-anthropic-schema-af240";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    version: String,
    model: String,
    provider: ProviderConfig,
    effort: ReasoningEffort,
    max_output_tokens: u32,
    timeout_seconds: u64,
    statement: String,
    query: String,
    said_on: String,
}
fn config() -> Config {
    serde_json::from_str(include_str!("eval/anthropic_schema/diagnostic.json")).unwrap()
}
fn config_af238() -> Config {
    serde_json::from_str(include_str!("eval/anthropic_schema/diagnostic-af238.json")).unwrap()
}
fn config_af240() -> Config {
    serde_json::from_str(include_str!("eval/anthropic_schema/diagnostic-af240.json")).unwrap()
}
fn plan_output(current: bool) -> R1Output {
    serde_json::from_value(json!({"need":"judge","current_evidence":if current {
        "assertion_present" } else { "query_only" },"type":"single",
        "subject":{"entity_id":"self"},"relation":"住址","conditions":[],"time":null,
        "plan":{"op":"lookup","filter":{"relation":null,"time":null},"unit":null}}))
    .unwrap()
}
fn plan(cfg: &Config, current: bool) -> r1::Plan {
    r1::Plan {
        query: if current {
            cfg.statement.clone()
        } else {
            cfg.query.clone()
        },
        context: vec![],
        subject_ids: None,
        task_id: None,
        output: Some(plan_output(current)),
        time_resolution: None,
        filter_time_resolution: None,
    }
}
fn schemas() -> Vec<Value> {
    vec![
        write_models::w1_schema(),
        write_models::w3_schema(),
        write_models::w4_schema(),
        recall_models::r1_schema(),
        recall_models::r4_schema_for_plan(&plan(&config(), false)),
        recall_models::r4_schema_for_plan(&plan(&config(), true)),
    ]
}
fn inputs(cfg: &Config) -> Vec<Value> {
    let entity: Entity = serde_json::from_value(json!({"id":"self","owner_id":"diagnostic",
        "entity_type":"Person","canonical_name":"用户本人","aliases":[]}))
    .unwrap();
    let relations = [w2::Relation {
        id: "fact".into(),
        memory_types: vec![hippocampus::MemoryType::Fact],
    }];
    let ids = [EntityId::from("self")];
    let candidate = json!({"kind":"assertion","speech":"user_statement",
        "subject":{"entity_id":"self"},"relation":"fact","memory_type":"fact",
        "value":{"kind":"text","text":cfg.statement,"quote":{"text":cfg.statement}},
        "sentence":{"text":cfg.statement},"polarity":"affirm","strength":null,
        "uncertainty":null,"conditions":[],"times":[]});
    let output: W1Output =
        serde_json::from_value(json!({"candidates":[candidate],"memory_requests":[]})).unwrap();
    let batch = w2::validate(
        output,
        &w2::Context {
            text: &cfg.statement,
            entities: &ids,
            relations: &relations,
            said_on: Some(&cfg.said_on),
            utc_offset_seconds: 28800,
            task_id: None,
        },
    )
    .unwrap();
    assert_eq!(batch.candidates[0].status, w2::Status::Ok);
    let base = json!({"contract":"w1.v5","source_snapshot":{"source_id":"s"},
        "current_user_message":cfg.statement,"user_history":[],"entities":[entity],
        "relations":[{"id":"fact","memory_types":["fact"]}],"said_on":cfg.said_on,
        "utc_offset_seconds":28800,"task_id":null});
    let mut w3 = base.clone();
    w3["contract"] = json!("w3.v6");
    w3["candidates"] = json!(batch.candidates);
    w3["memory_requests"] = json!(batch.memory_requests);
    let w4: W4Input = serde_json::from_value(json!({"candidates":[{"index":0,"committed_id":null,
        "candidate":candidate,"source":{"kind":"stored","source_id":"s"},
        "said_on":cfg.said_on,"time_resolutions":[]}],"records":[],"relations":[],
        "coverage":{"complete":true,"omitted":0}}))
    .unwrap();
    let ctx = r1::Context {
        query: &cfg.query,
        context: &[],
        entities: &ids,
        relations: &relations,
        subject_ids: None,
        task_id: None,
        said_on: &cfg.said_on,
        utc_offset_seconds: 28800,
        supported_units: recall_models::SUPPORTED_UNITS,
    };
    r1::validate(plan_output(false), &ctx).unwrap();
    let r4 = |current| {
        let evidence: RecallEvidence = serde_json::from_value(json!({
            "current_message":if current {json!({"source_id":null,"text":cfg.statement,
                "said_on":cfg.said_on,"sequence":null,"enrichment":"not_stored","memory_requests":[]})} else {Value::Null},
            "records":[],"raw":if current {json!([])} else {json!([{"source_id":"s",
                "quote":{"text":cfg.statement},"said_on":cfg.said_on,"sequence":1,
                "grade":"uncommitted","enrichment":"pending","memory_requests":[]}])},
            "relations":[],"completeness":{"retrieval_truncated":false,"pending_related_sources":if current {0} else {1},
                "pending_updates":0,"enrichment_gaps":if current {0} else {1},"plan_complete":true}})).unwrap();
        let p = plan(cfg, current);
        let request = RecallRequest {
            request_id: "diagnostic".into(),
            query: p.query.clone(),
            context: vec![],
            task_id: None,
            at: 1791561600,
        };
        recall_models::r4_payload(&request, &p, &evidence, 28800)
    };
    vec![
        base,
        w3,
        json!({"contract":"w4.v5","w4_input":w4,
        "sources":{"s":{"text":cfg.statement,"history":[],"provenance":null,"time_anchor":null}}}),
        recall_models::r1_input(&ctx, &[entity]),
        r4(false),
        r4(true),
    ]
}

#[derive(Clone)]
struct Case {
    campaign: &'static str,
    id: &'static str,
    canonical: Value,
    wire: Value,
    binding: Value,
}
fn cases(cfg: &Config, base: &str) -> Result<Vec<Case>, String> {
    let (expected_mode, campaign) = match cfg.version.as_str() {
        "af236.schema-diagnostic@1" => (StructuredMode::NativeSimplified, "af236"),
        "af238.schema-diagnostic@1" => (StructuredMode::NativeSimplifiedNopattern, "af238"),
        "af240.schema-diagnostic@1" => (StructuredMode::NativeSimplifiedNopattern, "af240"),
        _ => return Err("unknown diagnostic configuration".into()),
    };
    if cfg.model != "claude-haiku-5-5"
        || cfg.provider.protocol != ProviderProtocol::AnthropicMessages
        || cfg.provider.structured_mode != expected_mode
        || cfg.timeout_seconds != 360
        || cfg.max_output_tokens != 4096
        || cfg.effort != ReasoningEffort::Low
    {
        return Err("fixed diagnostic configuration changed".into());
    }
    let stages = [
        ModelStage::W1,
        ModelStage::W3,
        ModelStage::W4,
        ModelStage::R1,
        ModelStage::R4,
        ModelStage::R4,
    ];
    let names = ["w1_v5", "w3_v6", "w4_v5", "r1_v2", "r4_v7", "r4_v7"];
    let prompts = [
        write_models::W1_PROMPT,
        write_models::W3_PROMPT,
        write_models::W4_PROMPT,
        recall_models::R1_PROMPT,
        recall_models::R4_PROMPT,
        recall_models::R4_PROMPT,
    ];
    let mut result = Vec::new();
    for (i, (schema, input)) in schemas().into_iter().zip(inputs(cfg)).enumerate() {
        if campaign == "af240" && i < 3 {
            continue;
        }
        let mut canonical = structured_request(
            stages[i],
            names[i],
            prompts[i],
            input,
            schema.clone(),
            &ModelBudgetConfig::default(),
        );
        canonical["model"] = json!(cfg.model);
        canonical["max_output_tokens"] = json!(cfg.max_output_tokens);
        cfg.effort.apply(&mut canonical);
        let wire = model_provider::provider_request(Some(&cfg.provider), &canonical)
            .map_err(|e| e.to_string())?;
        let schema_binding = if expected_mode == StructuredMode::NativeSimplifiedNopattern {
            model_provider::anthropic_simplified_nopattern_binding(&schema)
        } else {
            model_provider::anthropic_simplified_binding(&schema)
        }
        .map_err(|e| e.to_string())?;
        let mut binding = json!({"adapter":model_provider::ADAPTER_VERSION,"provider":cfg.provider,
            "endpoint":cfg.provider.endpoint(base).map_err(|e|e.to_string())?,
            "timeout_seconds":cfg.timeout_seconds,"canonical_request":canonical,"wire_request":wire,
            "schema_binding":schema_binding});
        if expected_mode == StructuredMode::NativeSimplifiedNopattern {
            binding["stage_validation_version"] = json!("af238.stage-validation@1");
        }
        result.push(Case {
            campaign,
            id: IDS[i],
            canonical,
            wire,
            binding,
        });
    }
    Ok(result)
}
fn is_af238(cases: &[Case]) -> bool {
    campaign_id(cases) == "af238"
}
fn campaign_id(cases: &[Case]) -> &'static str {
    cases[0].campaign
}
fn rounds(cases: &[Case]) -> usize {
    if campaign_id(cases) == "af240" { 1 } else { 2 }
}
fn round_cases(cases: &[Case], round: usize) -> impl Iterator<Item = &Case> {
    // AF238 has five warm slots; retain the larger schemas and both R4 variants.
    cases
        .iter()
        .filter(move |c| round <= rounds(cases) && !(is_af238(cases) && round == 2 && c.id == "w3"))
}
fn total_limit(cases: &[Case]) -> usize {
    cases.len() + round_cases(cases, 2).count()
}
fn identity(case: &Case, round: usize) -> Value {
    let mut slot = json!({"id":case.id,"round":round,"request_fingerprint":recall_models::fingerprint(&case.binding)});
    if case.campaign == "af240" {
        slot["campaign"] = json!(case.campaign);
    }
    slot
}
fn authorization(cases: &[Case]) -> Value {
    json!({"version":format!("{}.campaign@1",campaign_id(cases)),"total_limit":total_limit(cases),
        "cold_limit":cases.len(),"warm_limit":round_cases(cases,2).count(),
        "warm_requires_all_cold_success":rounds(cases)==2,"slots":(1..=rounds(cases)).flat_map(|round|
            round_cases(cases,round).map(move|c|identity(c,round))).collect::<Vec<_>>()})
}
fn record_path(root: &Path, case: &Case, round: usize) -> PathBuf {
    root.join("recordings")
        .join(format!("round-{round}"))
        .join(format!("{}.json", case.id))
}
fn read(path: &Path) -> Result<Option<Value>, String> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| e.to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}
fn write(path: &Path, value: &Value) -> Result<(), String> {
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    let temp = path.with_extension("tmp");
    use std::io::Write;
    let mut file = std::fs::File::create(&temp).map_err(|e| e.to_string())?;
    file.write_all(format!("{}\n", serde_json::to_string_pretty(value).unwrap()).as_bytes())
        .map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    std::fs::rename(temp, path).map_err(|e| e.to_string())?;
    std::fs::File::open(path.parent().unwrap())
        .and_then(|dir| dir.sync_all())
        .map_err(|e| e.to_string())
}
fn validate_record(case: &Case, round: usize, record: &Value) -> Result<(), String> {
    if record["slot"] != identity(case, round) || record["binding"] != case.binding {
        return Err("schema diagnostic recording stale; no resend permitted".into());
    }
    let recomputed = verification(case, &record["raw_response"]);
    for field in [
        "wire_schema",
        "original_schema",
        "stage_validation",
        "usage_valid",
        "success",
        "usage",
        "output",
    ] {
        if record["verification"][field] != recomputed[field] {
            return Err("schema diagnostic verification disagrees with raw response".into());
        }
    }
    Ok(())
}
fn usage_valid(usage: &Value) -> bool {
    usage["input_tokens"].as_u64().is_some_and(|n| n > 0)
        && usage["output_tokens"].as_u64().is_some_and(|n| n > 0)
}
fn w2_batch(input: &Value, output: W1Output) -> Result<w2::Batch, String> {
    let entities: Vec<Entity> =
        serde_json::from_value(input["entities"].clone()).map_err(|e| e.to_string())?;
    let ids: Vec<_> = entities.into_iter().map(|e| e.id).collect();
    let relations: Vec<_> = input["relations"]
        .as_array()
        .ok_or("missing relations")?
        .iter()
        .map(|r| {
            Ok(w2::Relation {
                id: r["id"].as_str().ok_or("missing relation id")?.into(),
                memory_types: r["memory_types"]
                    .as_array()
                    .ok_or("missing memory types")?
                    .iter()
                    .map(|t| match t.as_str() {
                        Some("fact") => Ok(hippocampus::MemoryType::Fact),
                        Some("preference") => Ok(hippocampus::MemoryType::Preference),
                        Some("constraint") => Ok(hippocampus::MemoryType::Constraint),
                        Some("episode") => Ok(hippocampus::MemoryType::Episode),
                        _ => Err("unknown memory type".to_string()),
                    })
                    .collect::<Result<_, String>>()?,
            })
        })
        .collect::<Result<_, String>>()?;
    w2::validate(
        output,
        &w2::Context {
            text: input["current_user_message"]
                .as_str()
                .ok_or("missing source text")?,
            entities: &ids,
            relations: &relations,
            said_on: input["said_on"].as_str(),
            utc_offset_seconds: 28800,
            task_id: None,
        },
    )
    .map_err(|e| e.to_string())
}
/// Use the existing stage boundaries; no label or semantic score participates in the gate.
fn stage_validation(case: &Case, output: &Value) -> Result<(), String> {
    let input: Value =
        serde_json::from_str(case.canonical["input"].as_str().ok_or("missing input")?)
            .map_err(|e| e.to_string())?;
    let text = output.to_string();
    match case.id {
        "w1" => {
            let decoded =
                w2::decode_detailed(&text).map_err(|d| format!("{}: {}", d.field, d.rule))?;
            let batch = w2_batch(&input, decoded)?;
            if let Some(c) = batch
                .candidates
                .iter()
                .find(|c| c.status == w2::Status::Rejected)
            {
                return Err(format!("candidates[{}]: {}", c.index, c.reasons.join(",")));
            }
        }
        "w3" => {
            let candidates: Vec<w2::CandidateResult> =
                serde_json::from_value(input["candidates"].clone()).map_err(|e| e.to_string())?;
            let batch = w2::Batch {
                index_map: candidates
                    .iter()
                    .enumerate()
                    .map(|(i, c)| (c.status != w2::Status::Rejected).then_some(i))
                    .collect(),
                candidates,
                memory_requests: serde_json::from_value(input["memory_requests"].clone())
                    .map_err(|e| e.to_string())?,
                task_id: None,
                said_on: input["said_on"].as_str().map(String::from),
                utc_offset_seconds: 28800,
            };
            W3Output::decode(&text)
                .map_err(|e| e.to_string())?
                .validate(
                    &batch,
                    &input["source_snapshot"],
                    input["current_user_message"]
                        .as_str()
                        .ok_or("missing source text")?,
                )
                .map_err(|e| e.to_string())?;
        }
        "w4" => {
            let request: W4Input =
                serde_json::from_value(input["w4_input"].clone()).map_err(|e| e.to_string())?;
            let decoded: W4Output = serde_json::from_str(&text).map_err(|e| e.to_string())?;
            w4::prepare_output(&request, &decoded, &|id| {
                input["sources"][id]["text"].as_str().map(String::from)
            })
            .map_err(|e| e.to_string())?;
        }
        "r1" => {
            let entities: Vec<Entity> =
                serde_json::from_value(input["entities"].clone()).map_err(|e| e.to_string())?;
            let ids: Vec<_> = entities.into_iter().map(|e| e.id).collect();
            let relations: Vec<_> = input["relations"]
                .as_array()
                .ok_or("missing relations")?
                .iter()
                .map(|r| {
                    Ok(w2::Relation {
                        id: r.as_str().ok_or("missing relation")?.into(),
                        memory_types: vec![hippocampus::MemoryType::Fact],
                    })
                })
                .collect::<Result<_, String>>()?;
            let (_, diagnostic) = r1::validate_detailed(
                r1::decode(&text).map_err(|e| e.to_string())?,
                &r1::Context {
                    query: input["message"].as_str().ok_or("missing query")?,
                    context: &[],
                    entities: &ids,
                    relations: &relations,
                    subject_ids: None,
                    task_id: None,
                    said_on: input["said_on"].as_str().ok_or("missing said_on")?,
                    utc_offset_seconds: 28800,
                    supported_units: recall_models::SUPPORTED_UNITS,
                },
            )
            .map_err(|e| e.to_string())?;
            if let Some(d) = diagnostic {
                return Err(format!("{}: {}", d.field, d.rule));
            }
        }
        "r4-stored" | "r4-current" => {
            let evidence: RecallEvidence =
                serde_json::from_value(input["evidence"].clone()).map_err(|e| e.to_string())?;
            let request: RecallRequest =
                serde_json::from_value(input["request"].clone()).map_err(|e| e.to_string())?;
            let plan = r1::Plan {
                query: request.query.clone(),
                context: request.context.clone(),
                subject_ids: None,
                task_id: request.task_id.as_deref().map(hippocampus::TaskId::from),
                output: serde_json::from_value(input["plan"].clone()).map_err(|e| e.to_string())?,
                time_resolution: None,
                filter_time_resolution: None,
            };
            let dates = evidence
                .raw
                .iter()
                .map(|s| (s.source_id.clone(), s.said_on.clone()))
                .collect();
            r4::evaluate_detailed(
                r4::decode(&text).map_err(|e| e.0.to_string())?,
                &r4::Context {
                    evidence: &evidence,
                    plan: &plan,
                    request: &request,
                    utc_offset_seconds: 28800,
                    source_dates: &dates,
                },
            )
            .map_err(|d| format!("{}: {}", d.field, d.error.0))?;
        }
        _ => return Err("unknown stage".into()),
    }
    Ok(())
}
fn verification(case: &Case, envelope: &Value) -> Value {
    let mut result = json!({"wire_schema":null,"original_schema":null,
        "usage_valid":false,"success":false});
    result["usage"] =
        model_provider::normalized_usage(ProviderProtocol::AnthropicMessages, envelope)
            .unwrap_or(Value::Null);
    result["usage_valid"] = json!(usage_valid(&result["usage"]));
    let parsed = model_provider::provider_output(ProviderProtocol::AnthropicMessages, envelope)
        .map_err(|e| e.to_string())
        .and_then(|text| serde_json::from_str::<Value>(&text).map_err(|e| e.to_string()));
    match parsed {
        Ok(output) => {
            let expanded = model_provider::expand_anthropic_simplified_schema(
                &case.wire["output_config"]["format"]["schema"],
            );
            let wire = expanded.and_then(|s| model_provider::check_output(&s, &output));
            let original =
                model_provider::check_output(&case.canonical["text"]["format"]["schema"], &output);
            result["wire_schema"] =
                json!({"passed":wire.is_ok(),"error":wire.err().map(|e|e.to_string())});
            result["original_schema"] =
                json!({"passed":original.is_ok(),"error":original.err().map(|e|e.to_string())});
            result["success"] = json!(
                result["wire_schema"]["passed"] == true
                    && result["original_schema"]["passed"] == true
                    && result["usage_valid"] == true
            );
            if case.binding["stage_validation_version"].is_string() {
                let checked = stage_validation(case, &output);
                result["stage_validation"] =
                    json!({"passed":checked.is_ok(),"error":checked.err()});
                result["success"] = json!(
                    result["success"] == true && result["stage_validation"]["passed"] == true
                );
            }
            result["output"] = output;
        }
        Err(error) => result["error"] = json!(error),
    }
    result
}

fn check_campaign(root: &Path, cases: &[Case]) -> Result<usize, String> {
    if let Some(stored) = read(&root.join("campaign/authorization.json"))? {
        if stored != authorization(cases) {
            return Err("schema diagnostic frozen authorization changed".into());
        }
    }
    let sends = root.join("campaign/sends");
    if sends.exists() {
        for entry in std::fs::read_dir(&sends).map_err(|e| e.to_string())? {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.extension().is_some_and(|e| e == "json") {
                let name = path.file_name().unwrap().to_string_lossy();
                if !(1..=rounds(cases)).any(|round| {
                    round_cases(cases, round).any(|c| name == format!("{round}-{}.json", c.id))
                }) {
                    return Err(
                        "schema diagnostic unknown send slot; allowance cannot be reset".into(),
                    );
                }
            }
        }
    }
    let mut used = 0;
    for round in 1..=rounds(cases) {
        for case in round_cases(cases, round) {
            let slot = identity(case, round);
            let log = read(
                &root
                    .join("campaign/sends")
                    .join(format!("{round}-{}.json", case.id)),
            )?;
            let record = read(&record_path(root, case, round))?;
            if let Some(log) = log {
                if log["slot"] != slot
                    || !matches!(log["state"].as_str(), Some("started" | "responded"))
                {
                    return Err("schema diagnostic send identity changed".into());
                }
                used += 1;
                if let Some(record) = record {
                    validate_record(case, round, &record)?;
                }
                // A missing response after a durable send is a terminal interruption, never retried.
            } else if record.is_some() {
                return Err("schema diagnostic recording has no durable send".into());
            }
        }
    }
    if used > total_limit(cases) {
        return Err("schema diagnostic cumulative allowance exceeded".into());
    }
    Ok(used)
}
fn cold_ready(root: &Path, cases: &[Case]) -> Result<bool, String> {
    for case in cases {
        let Some(record) = read(&record_path(root, case, 1))? else {
            return Ok(false);
        };
        validate_record(case, 1, &record)?;
        if record["verification"]["success"] != true {
            return Ok(false);
        }
    }
    Ok(true)
}
fn dry(root: &Path, cases: &[Case], round: usize) -> Result<String, String> {
    if !(1..=rounds(cases)).contains(&round) {
        return Err("round outside frozen diagnostic plan".into());
    }
    let used = check_campaign(root, cases)?;
    let ready = cold_ready(root, cases)?;
    let warm = round_cases(cases, 2).count();
    let mut lines = if campaign_id(cases) == "af240" {
        vec![
            format!("AF240：独立 Haiku native_simplified_nopattern；总额度3；已占用{used}。"),
            "计划3：r1 → r4-stored → r4-current，各1次；无暖轮；发送前拒绝0。".into(),
        ]
    } else {
        vec![
            format!(
                "{}：独立 Haiku {}；总额度{}=冷6+暖{warm}；已占用{used}。",
                campaign_id(cases).to_uppercase(),
                cases[0].binding["schema_binding"]["mode"].as_str().unwrap(),
                total_limit(cases)
            ),
            format!("第一轮计划6；第二轮计划{warm}（条件解锁）；发送前拒绝0。"),
        ]
    };
    for case in cases {
        let before = &case.canonical["text"]["format"]["schema"];
        let after = &case.wire["output_config"]["format"]["schema"];
        lines.push(format!(
            "{}：{}",
            case.id,
            json!({"original":stats(before),"wire":stats(after)})
        ));
    }
    let pending = round_cases(cases, round)
        .filter(|c| {
            !root
                .join("campaign/sends")
                .join(format!("{round}-{}.json", c.id))
                .exists()
        })
        .count();
    let stopped = (1..=rounds(cases)).any(|r| {
        round_cases(cases, r).any(|c| {
            root.join("campaign/sends")
                .join(format!("{r}-{}.json", c.id))
                .exists()
                && read(&record_path(root, c, r))
                    .ok()
                    .flatten()
                    .is_none_or(|record| record["verification"]["success"] != true)
        })
    });
    lines.push(format!(
        "轮次{round}：待录制{pending}；{}。",
        if stopped {
            "已停发（失败或中断在途）"
        } else if round == 2 && !ready {
            "未解锁（须冷请求6/6成功）"
        } else {
            "额度可用"
        }
    ));
    lines.push(
        if rounds(cases) == 1 {
            "失败或中断即停；不重试、不换模式；成功槽严格复用，不扩大额度。"
        } else {
            "失败或中断即停；不重试、不换模式；暖请求与冷请求正文完全相同。"
        }
        .into(),
    );
    lines.push(format!(
        "本轮输出token上限{}；campaign：{}/campaign；本次实际发送0。",
        round_cases(cases, round).count() * 4096,
        root.display()
    ));
    Ok(lines.join("\n"))
}
fn stats(schema: &Value) -> Value {
    fn count(v: &Value, key: &str) -> usize {
        match v {
            Value::Object(o) => {
                usize::from(o.contains_key(key)) + o.values().map(|v| count(v, key)).sum::<usize>()
            }
            Value::Array(a) => a.iter().map(|v| count(v, key)).sum(),
            _ => 0,
        }
    }
    json!({"bytes":schema.to_string().len(),"anyOf":count(schema,"anyOf"),"refs":count(schema,"$ref"),"patterns":count(schema,"pattern")})
}

fn execute(
    root: &Path,
    cases: &[Case],
    round: usize,
    send: &mut dyn FnMut(&Case) -> Result<Value, String>,
) -> Result<Value, String> {
    if !(1..=rounds(cases)).contains(&round) {
        return Err("round outside frozen diagnostic plan".into());
    }
    std::fs::create_dir_all(root.join("campaign")).map_err(|e| e.to_string())?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join("campaign/lock"))
        .map_err(|e| e.to_string())?;
    lock.try_lock()
        .map_err(|_| "schema diagnostic campaign is already running")?;
    check_campaign(root, cases)?;
    if round == 2 && !cold_ready(root, cases)? {
        return Err("schema diagnostic warm round locked: cold 6/6 success required".into());
    }
    if !root.join("campaign/authorization.json").exists() {
        write(
            &root.join("campaign/authorization.json"),
            &authorization(cases),
        )?;
    }
    // Never continue a different round after any failed/in-flight physical send.
    for prior in 1..=rounds(cases) {
        for c in round_cases(cases, prior) {
            if root
                .join("campaign/sends")
                .join(format!("{prior}-{}.json", c.id))
                .exists()
                && read(&record_path(root, c, prior))?
                    .is_none_or(|r| r["verification"]["success"] != true)
            {
                return Err("schema diagnostic stopped after failed or interrupted send".into());
            }
        }
    }
    let mut sent = 0;
    let mut rows = Vec::new();
    for case in round_cases(cases, round) {
        if let Some(record) = read(&record_path(root, case, round))? {
            rows.push(record);
            continue;
        }
        if check_campaign(root, cases)? >= total_limit(cases) {
            return Err("schema diagnostic allowance exhausted".into());
        }
        let log = root
            .join("campaign/sends")
            .join(format!("{round}-{}.json", case.id));
        write(
            &log,
            &json!({"state":"started","slot":identity(case,round)}),
        )?;
        sent += 1;
        let started = std::time::Instant::now();
        let response = send(case);
        let elapsed_ms = started.elapsed().as_millis();
        let envelope = response.as_ref().cloned().unwrap_or(Value::Null);
        let mut verified = verification(case, &envelope);
        if let Err(error) = response {
            verified["error"] = json!(error);
        }
        let record = json!({"slot":identity(case,round),"binding":case.binding,
            "raw_response":envelope,"verification":verified,"elapsed_ms":elapsed_ms});
        write(&record_path(root, case, round), &record)?;
        write(
            &log,
            &json!({"state":"responded","slot":identity(case,round),"usage":record["verification"]["usage"]}),
        )?;
        let success = record["verification"]["success"] == true;
        rows.push(record);
        if !success {
            break;
        }
    }
    let report = json!({"version":format!("{}.diagnostic-report@1",campaign_id(cases)),"round":round,"sent_calls":sent,
        "cumulative_sent":check_campaign(root,cases)?,"results":rows,"warm_unlocked":rounds(cases)==2 && cold_ready(root,cases)?});
    write(&root.join(format!("report-round-{round}.json")), &report)?;
    Ok(report)
}

#[test]
fn af236_diagnostic() {
    if std::env::var("EVAL_AF236").as_deref() != Ok("1") {
        return;
    }
    run(config(), "EVAL_AF236", DEFAULT_ROOT);
}

#[test]
fn af238_diagnostic() {
    if std::env::var("EVAL_AF238").as_deref() != Ok("1") {
        return;
    }
    run(config_af238(), "EVAL_AF238", AF238_ROOT);
}

#[test]
fn af240_diagnostic() {
    if std::env::var("EVAL_AF240").as_deref() != Ok("1") {
        return;
    }
    run(config_af240(), "EVAL_AF240", AF240_ROOT);
}

fn run(cfg: Config, prefix: &str, default_root: &str) {
    let dry_run = std::env::var("EVAL_DRY_RUN").as_deref() == Ok("1");
    let round: usize = std::env::var(format!("{prefix}_ROUND"))
        .unwrap_or("1".into())
        .parse()
        .unwrap();
    assert!((1..=2).contains(&round), "round must be 1 or 2");
    let root =
        PathBuf::from(std::env::var(format!("{prefix}_ROOT")).unwrap_or(default_root.into()));
    // A caller cannot redirect this diagnostic into an existing comparison campaign.
    assert!(
        root == Path::new(default_root),
        "diagnostic command fixes its independent root"
    );
    let base =
        std::env::var(&cfg.provider.base_url_env).unwrap_or("https://diagnostic.invalid".into());
    let cases = cases(&cfg, &base).unwrap();
    assert!(
        (1..=rounds(&cases)).contains(&round),
        "round outside frozen diagnostic plan"
    );
    if dry_run {
        println!("{}", dry(&root, &cases, round).unwrap());
        return;
    }
    assert_eq!(
        std::env::var("EVAL_RECORD").ok(),
        Some(round_cases(&cases, round).count().to_string()),
        "each round requires its explicit allowance"
    );
    assert_eq!(
        std::env::var("HIPPOCAMPUS_MODEL_NETWORK").as_deref(),
        Ok("live"),
        "real calls require live authorization"
    );
    assert_ne!(
        base, "https://diagnostic.invalid",
        "provider base URL required"
    );
    let key =
        std::env::var(cfg.provider.api_key_env.as_ref().unwrap()).expect("provider key required");
    assert!(!key.trim().is_empty(), "provider key required");
    let endpoint = cfg.provider.endpoint(&base).unwrap();
    let http = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(cfg.timeout_seconds))
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .build()
        .unwrap();
    let report = execute(&root, &cases, round, &mut |case| {
        let response = hippocampus::provider_http_request(
            &http,
            &endpoint,
            &key,
            ProviderProtocol::AnthropicMessages,
        )
        .json(&case.wire)
        .send()
        .map_err(|_| "provider transport failed".to_string())?;
        model_provider::read_http_response(response, ProviderProtocol::AnthropicMessages)
            .map(|(_, envelope)| envelope)
            .map_err(|_| "provider response read failed".into())
    })
    .unwrap();
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
}

#[cfg(test)]
#[path = "eval/anthropic_schema/tests.rs"]
mod tests;
