//! Two frozen S4 development questions through the S5 assisted seam.
//! This is an isolated evaluation runner, not a service-mode switch.
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use hippocampus::*;
use serde::Deserialize;
use serde_json::{Value, json};

const AT: i64 = 1_790_294_400; // 2026-09-25T00:00:00Z; after the synthetic intake.
const INPUTS_SHA: &str = "c11db785e10a0e2f35f29447510df393869bb199c9a6eae139bb881d89aae368";
const LABELS_SHA: &str = "56e8b7811bb3413413bbb32f49fee59cead4d4cc88de583f58dc6875128b9bfc";
const CASES: [&str; 2] = ["N04", "N04-ambiguous"];

#[derive(Deserialize)]
struct Source {
    id: String,
    text: String,
}

#[derive(Deserialize)]
struct Case {
    id: String,
    owner: String,
    query: String,
    context: Vec<String>,
    sources: Vec<Source>,
}

#[derive(Deserialize)]
struct Label {
    id: String,
    answerability: String,
    required_sources: Vec<String>,
    forbidden_sources: Vec<String>,
    required_points: BTreeMap<String, Vec<String>>,
}

struct Started {
    case: Case,
    store: SqliteMemoryStore,
    trusted: RuntimeContext,
    local: Value,
    signature: LocalSnapshotSignature,
    source_map: BTreeMap<String, String>,
}

struct Args {
    run_dir: PathBuf,
    budget: PathBuf,
    ledger: PathBuf,
    send: bool,
    case_filter: Option<String>,
    synthetic_all: bool,
    real_all: bool,
}

fn error(message: impl Into<String>) -> Box<dyn std::error::Error + Send + Sync> {
    message.into().into()
}

fn checked_file(path: &Path, expected: &str) -> PersistenceResult<Vec<u8>> {
    let output = Command::new("shasum")
        .args(["-a", "256"])
        .arg(path)
        .output()?;
    if !output.status.success()
        || String::from_utf8(output.stdout)?.split_whitespace().next() != Some(expected)
    {
        return Err(error(format!(
            "frozen fixture hash mismatch: {}",
            path.display()
        )));
    }
    Ok(fs::read(path)?)
}

fn arguments() -> PersistenceResult<Args> {
    let mut run_dir = None;
    let mut budget = None;
    let mut ledger = None;
    let mut send = None;
    let mut case_filter = None;
    let mut synthetic_all = false;
    let mut real_all = false;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--run-dir" if run_dir.is_none() => {
                run_dir = Some(PathBuf::from(args.next().ok_or("missing --run-dir value")?));
            }
            "--budget" if budget.is_none() => {
                budget = Some(PathBuf::from(args.next().ok_or("missing --budget value")?));
            }
            "--ledger" if ledger.is_none() => {
                ledger = Some(PathBuf::from(args.next().ok_or("missing --ledger value")?));
            }
            "--case" if case_filter.is_none() => {
                case_filter = Some(args.next().ok_or("missing --case value")?);
            }
            "--synthetic-all" if !synthetic_all => synthetic_all = true,
            "--real-all" if !real_all => real_all = true,
            "--dry-run" if send.is_none() => send = Some(false),
            "--send" if send.is_none() => send = Some(true),
            _ => return Err(error(format!("unknown or repeated argument: {arg}"))),
        }
    }
    match (run_dir, budget, ledger, send) {
        (Some(run_dir), Some(budget), Some(ledger), Some(send)) => Ok(Args {
            run_dir,
            budget,
            ledger,
            send,
            case_filter,
            synthetic_all,
            real_all,
        }),
        _ => Err(error(
            "usage: s5_assisted_eval --run-dir NEW_DIR --budget JSON --ledger NEW_PATH --dry-run|--send [--case N04|N04-ambiguous | --synthetic-all | --real-all]",
        )),
    }
}

fn registry(config: &chat::ChatConfig) -> InMemoryPredicateRegistry {
    let mut result = InMemoryPredicateRegistry::default();
    for definition in &config.predicates {
        result.register(definition.predicate.clone(), definition.cardinality);
    }
    result
}

fn relations(config: &chat::ChatConfig) -> InMemorySemanticRelationRegistry {
    let mut result = InMemorySemanticRelationRegistry::default();
    for (child, parent) in &config.is_a {
        result.register_is_a(child.clone(), parent.clone());
    }
    result
}

fn start(
    case: Case,
    run_dir: &Path,
    config: &chat::ChatConfig,
    all_cases: bool,
) -> PersistenceResult<Started> {
    let _operation = diagnostics::Operation::enter(&format!("s5-assisted-eval-{}", case.id));
    if case.owner != "s4-synthetic"
        || case.query.trim().is_empty()
        || (!all_cases && !case.context.is_empty())
    {
        return Err(error("S4 case owner, question or context changed"));
    }
    let db_path = run_dir.join(format!("{}.sqlite3", case.id));
    let owner = UserId::from(case.owner.as_str());
    let mut store = SqliteMemoryStore::open(db_path, owner.clone())?;
    store.set_fact_index_enabled(false);
    let mut source_map = BTreeMap::new();
    store.transaction(|state| {
        for name in ["松石47", "赤铜82"] {
            state.entities.upsert(Entity {
                id: EntityId::from(name),
                owner_id: owner.clone(),
                entity_type: EntityType::Other("Other".into()),
                canonical_name: name.into(),
                aliases: Vec::new(),
            });
        }
        for (index, source) in case.sources.iter().enumerate() {
            let source_id = format!("{}:{}", case.id, source.id);
            if source_map
                .insert(source_id.clone(), source.id.clone())
                .is_some()
            {
                return Err(error("duplicate S4 source id"));
            }
            state.sources.insert(
                source_id.clone(),
                SourceRecord {
                    id: source_id,
                    session_id: case.id.clone(),
                    turn_id: source.id.clone(),
                    observed_at: AT - 1,
                    received_sequence: (index + 1) as u64,
                    request: Some(json!({"text":source.text,"capture_source":true,
                        "source_origin":"s4_synthetic_user"})),
                    enrichment: "not_requested".into(),
                    error: None,
                    result: None,
                    base_revision: 0,
                    index: Value::Null,
                    task: Default::default(),
                },
            );
        }
        Ok(())
    })?;
    let trusted = RuntimeContext::current(owner, Timestamp(AT));
    let (local, signature) = store.recall_local_with_signature(
        &case.query,
        &case.context,
        &trusted,
        registry(config),
        &relations(config),
        &config.local_recall_budget,
    )?;
    Ok(Started {
        case,
        store,
        trusted,
        local,
        signature,
        source_map,
    })
}

fn snippets(local: &Value) -> Vec<String> {
    local["source_evidence"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(
            local["historical_or_conflicting"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|item| item.get("source")),
        )
        .filter_map(|item| item["text"].as_str())
        .take(3)
        .map(|text| text.chars().take(1000).collect())
        .collect()
}

fn failure(error: &OpenAiAdapterError) -> &'static str {
    match error {
        OpenAiAdapterError::MissingBaseUrl | OpenAiAdapterError::MissingApiKey => {
            "model_client_unavailable"
        }
        OpenAiAdapterError::NetworkDisabled => "model_network_disabled",
        OpenAiAdapterError::Budget(_) => "model_budget_denied",
        OpenAiAdapterError::Incomplete(_) => "planner_incomplete",
        OpenAiAdapterError::Refusal(_) => "model_refused",
        OpenAiAdapterError::Http(_) | OpenAiAdapterError::Api { .. } => "model_request_failed",
        OpenAiAdapterError::InvalidContext(_)
        | OpenAiAdapterError::InvalidResponse(_)
        | OpenAiAdapterError::InvalidStructuredOutput(_)
        | OpenAiAdapterError::SourceCoverageIncomplete => "invalid_plan",
    }
}

fn finish(
    started: Started,
    config: &chat::ChatConfig,
    budget: &ModelBudgetConfig,
    ledger: &Path,
    send: bool,
) -> PersistenceResult<Value> {
    let Started {
        case,
        store,
        trusted,
        local,
        signature,
        source_map,
    } = started;
    let _operation = diagnostics::Operation::enter(&format!("s5-assisted-eval-{}", case.id));
    let candidate_snippets = snippets(&local);
    let planner_input = json!({"question":case.query,"user_context":case.context,
        "trusted_at":trusted.at.0,"trusted_task_id":trusted.task_id,
        "candidate_snippets":candidate_snippets});
    if local["reference_resolution"]["needs_clarification"] == true {
        let mut assisted = local.clone();
        assisted["retrieval_mode"] = json!("assisted");
        assisted["assistance"] =
            json!({"status":"not_requested","reason":"clarification_required"});
        assisted["model_calls"] = json!([]);
        return Ok(
            json!({"id":case.id,"query":case.query,"context":case.context,
            "owner":case.owner,"trusted_at":AT,"fact_index_enabled":false,
            "source_map":source_map,"planner_input":planner_input,"local":local,
            "assisted":assisted,"planner_status":"not_requested_clarification",
            "model_calls":[]}),
        );
    }
    let (queries, planner_status, model_calls) = if send {
        let client = env::var("DEEPSEEK_BASE_URL")
            .map_err(|_| OpenAiAdapterError::MissingBaseUrl)
            .and_then(|base| {
                env::var("DEEPSEEK_API_KEY")
                    .map_err(|_| OpenAiAdapterError::MissingApiKey)
                    .and_then(|key| OpenAiClient::new(base, key))
            })
            .and_then(|client| {
                client.with_budget(
                    ledger,
                    case.owner.clone(),
                    budget.clone(),
                    format!("s5-assisted-eval-{}", case.id),
                )
            });
        match client {
            Ok(client) => {
                let plan = client.supplemental_queries(
                    &case.query,
                    &case.context,
                    &trusted,
                    &candidate_snippets,
                );
                let calls = client.model_budget_report();
                match plan {
                    Ok(queries) => (queries, "completed", calls),
                    Err(error) => (Vec::new(), failure(&error), calls),
                }
            }
            Err(error) => (Vec::new(), failure(&error), Vec::new()),
        }
    } else {
        (Vec::new(), "not_sent_dry_run", Vec::new())
    };
    let final_read = store.recall_assisted_local(
        &case.query,
        &case.context,
        &trusted,
        registry(config),
        &relations(config),
        &config.local_recall_budget,
        &queries,
        &signature,
    );
    let (mut assisted, changed) = match final_read {
        Ok(result) => result,
        Err(error) => {
            return Ok(
                json!({"id":case.id,"query":case.query,"context":case.context,
                "owner":case.owner,"trusted_at":AT,"fact_index_enabled":false,
                "source_map":source_map,"planner_input":planner_input,"local":local,
                "assisted":{"retrieval_mode":"assisted","recall_status":"failed",
                    "assistance":{"status":"final_local_read_failed","planner_status":planner_status},
                    "error":error.to_string(),"model_calls":model_calls},
                "planner_status":planner_status,"model_calls":model_calls}),
            );
        }
    };
    assisted["retrieval_mode"] = json!("assisted");
    assisted["assistance"] = if changed {
        json!({"status":"state_changed_during_assistance","planner_status":planner_status,
            "discarded_queries":queries.len()})
    } else if !send {
        json!({"status":"not_sent_dry_run"})
    } else if planner_status != "completed" {
        json!({"status":"fallback_local","reason":planner_status})
    } else if matches!(
        assisted["assistance_search"]["status"].as_str(),
        Some("soft_search_failed" | "invalid_soft_queries")
    ) {
        json!({"status":"fallback_local","reason":assisted["assistance_search"]["status"]})
    } else if queries.is_empty() {
        json!({"status":"completed_empty","queries":[]})
    } else {
        json!({"status":"completed","queries":queries,"source":"llm_inferred","strength":"soft"})
    };
    assisted["model_calls"] = json!(model_calls);
    Ok(
        json!({"id":case.id,"query":case.query,"context":case.context,"owner":case.owner,
        "trusted_at":AT,"fact_index_enabled":false,"source_map":source_map,
        "planner_input":planner_input,"local":local,"assisted":assisted,"planner_status":planner_status,
        "model_calls":model_calls}),
    )
}

fn selected(
    result: &Value,
    source_map: &BTreeMap<String, String>,
) -> PersistenceResult<BTreeMap<String, String>> {
    let mut found = BTreeMap::new();
    for source in result["source_evidence"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(
            result["historical_or_conflicting"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|item| item.get("source")),
        )
    {
        let source_id = source["source_id"]
            .as_str()
            .ok_or("returned source has no source_id")?;
        let label_id = source_map
            .get(source_id)
            .ok_or("returned source outside case fixture")?;
        let text = source["text"]
            .as_str()
            .ok_or("returned source has no text")?;
        found.insert(label_id.clone(), text.into());
    }
    Ok(found)
}

fn score(
    result: &Value,
    label: &Label,
    source_map: &BTreeMap<String, String>,
) -> PersistenceResult<Value> {
    if result["recall_status"] != "completed" {
        return Ok(json!({"answerability":label.answerability,"scorable":false,
            "failure_reason":result["assistance"]["status"],"required_evidence_complete":null,
            "ambiguity_safe":null,"ambiguity_no_sources":null,
            "unresolved_without_evidence":false,"runtime_ambiguous_status":false,
            "clarification_handoff_proxy":false,"semantic_resolution":null,
            "host_asked_project":null}));
    }
    let found = selected(result, source_map)?;
    let required: BTreeSet<_> = label.required_sources.iter().collect();
    let actual: BTreeSet<_> = found.keys().collect();
    let forbidden: Vec<_> = label
        .forbidden_sources
        .iter()
        .filter(|id| found.contains_key(*id))
        .collect();
    let point_coverage: BTreeMap<_, _> = label
        .required_points
        .iter()
        .map(|(id, points)| {
            (
                id.clone(),
                points
                    .iter()
                    .filter(|point| {
                        found
                            .get(id)
                            .is_some_and(|text| text.contains(point.as_str()))
                    })
                    .count(),
            )
        })
        .collect();
    let complete = required.is_subset(&actual)
        && label.required_sources.iter().all(|id| {
            point_coverage.get(id) == Some(&label.required_points.get(id).map_or(0, Vec::len))
        });
    let lanes = [
        "mandatory",
        "constraints",
        "preferences",
        "facts",
        "episodes",
        "conflicts",
    ];
    let no_facts = ["result", "global_context"].iter().all(|scope| {
        let snapshot = if *scope == "result" {
            &result["result"]
        } else {
            &result["global_context"]["result"]
        };
        lanes
            .iter()
            .all(|lane| snapshot[*lane].as_array().is_some_and(Vec::is_empty))
    });
    let no_answer_evidence = result["source_evidence"]
        .as_array()
        .is_some_and(Vec::is_empty)
        && result["historical_or_conflicting"]
            .as_array()
            .is_some_and(Vec::is_empty)
        && no_facts;
    let unresolved_without_evidence = no_answer_evidence
        && result["evidence_status"] == "sufficiency_not_verified"
        && result["reference_resolution"]["status"] == "unconfirmed";
    let runtime_ambiguous_status = result["evidence_status"] == "ambiguous"
        && result["reference_resolution"]["status"] == "ambiguous";
    let clarification = result["reference_resolution"]["needs_clarification"] == true;
    Ok(
        json!({"answerability":label.answerability,"scorable":true,"final_sources":actual,
        "required_sources":required,"required_point_coverage":point_coverage,
        "required_evidence_complete":if label.answerability=="answerable" {Some(complete)} else {None},
        "available_required_evidence_complete":if label.answerability=="answerable" {Some(complete)} else {None},
        "forbidden_found":forbidden,
        "evidence_status":result["evidence_status"],
        "unresolved_without_evidence":unresolved_without_evidence,
        "runtime_ambiguous_status":runtime_ambiguous_status,
        "clarification_handoff_proxy":no_answer_evidence
            && (clarification || unresolved_without_evidence || runtime_ambiguous_status),
        "semantic_resolution":result["reference_resolution"]["semantic_resolution"],
        "host_asked_project":null,
        "ambiguity_safe":if label.answerability=="ambiguous" {
            Some(result["evidence_status"]=="ambiguous" || clarification)
        } else {None},
        "ambiguity_no_sources":if label.answerability=="ambiguous" {
            Some((result["evidence_status"]=="ambiguous" || clarification) && found.is_empty())
        } else {None}}),
    )
}

fn ledger_rows(path: &Path, group: &str) -> PersistenceResult<Value> {
    if !path.exists() {
        return Ok(json!([]));
    }
    let db =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut query = db.prepare("SELECT operation_id,stage,outcome,usage_status,input_bytes,output_token_limit,input_tokens,output_tokens,request_fingerprint FROM model_calls WHERE group_id=?1 ORDER BY created_at,operation_id")?;
    let rows = query.query_map([group], |row| {
        Ok(json!({"operation_id":row.get::<_,String>(0)?,"stage":row.get::<_,String>(1)?,
            "outcome":row.get::<_,String>(2)?,"usage_status":row.get::<_,String>(3)?,
            "input_bytes":row.get::<_,i64>(4)?,"output_token_limit":row.get::<_,i64>(5)?,
            "input_tokens":row.get::<_,Option<i64>>(6)?,"output_tokens":row.get::<_,Option<i64>>(7)?,
            "request_fingerprint":row.get::<_,String>(8)?}))
    })?.collect::<Result<Vec<_>,_>>()?;
    Ok(json!(rows))
}

fn write_new(path: &Path, value: &Value) -> PersistenceResult<()> {
    use std::io::Write;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    writeln!(file)?;
    Ok(())
}

fn private_dir(path: &Path) -> PersistenceResult<()> {
    fs::create_dir(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn trace_summary(dir: &Path) -> PersistenceResult<Value> {
    let mut files = Vec::new();
    let mut per_run: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("jsonl") {
            continue;
        }
        files.push(path.file_name().unwrap().to_string_lossy().to_string());
        for line in fs::read_to_string(&path)?.lines() {
            let event: Value = serde_json::from_str(line)?;
            let run = event["run_id"]
                .as_str()
                .ok_or("trace event has no run_id")?;
            let kind = event["kind"].as_str().ok_or("trace event has no kind")?;
            *per_run
                .entry(run.into())
                .or_default()
                .entry(kind.into())
                .or_default() += 1;
        }
    }
    files.sort();
    let count = |kind: &str| {
        per_run
            .values()
            .map(|kinds| kinds.get(kind).copied().unwrap_or(0))
            .sum::<usize>()
    };
    Ok(json!({"directory":dir,"files":files,"per_run":per_run,
        "model_start":count("model_start"),"model_end":count("model_end"),
        "local_recall":count("local_recall"),"incomplete":diagnostics::incomplete()}))
}

fn main() -> PersistenceResult<()> {
    let Args {
        run_dir,
        budget: budget_file,
        ledger,
        send,
        case_filter,
        synthetic_all,
        real_all,
    } = arguments()?;
    let budget: ModelBudgetConfig = serde_json::from_slice(&fs::read(&budget_file)?)?;
    budget.validate()?;
    if (synthetic_all && real_all) || ((synthetic_all || real_all) && case_filter.is_some()) {
        return Err(error(
            "--synthetic-all and --real-all are exclusive and disallow --case",
        ));
    }
    if budget.max_operation_calls != 1 {
        return Err(error("evaluation requires max_operation_calls=1"));
    }
    if (synthetic_all || real_all) && budget.max_calls != 11 {
        return Err(error("11-case evaluation requires max_calls=11"));
    }
    if real_all
        && (budget.max_input_bytes != 65536
            || budget.query_output_tokens != 32768
            || !budget.group_id.starts_with("s5-07-real-11-")
            || ledger.symlink_metadata().is_ok())
    {
        return Err(error(
            "--real-all requires a new s5-07-real-11-* budget group, new ledger, max_input_bytes=65536, and query_output_tokens=32768",
        ));
    }
    let network_mode = env::var("PMR_MODEL_NETWORK").unwrap_or_default();
    if send && network_mode != if synthetic_all { "offline" } else { "live" } {
        return Err(error(
            "--send requires live network mode; --synthetic-all requires offline",
        ));
    }
    if real_all && send {
        let base = env::var("DEEPSEEK_BASE_URL")?;
        if base.trim_end_matches('/') != "https://ark.cn-beijing.volces.com/api/v3"
            || !env::var("DEEPSEEK_API_KEY").is_ok_and(|key| !key.trim().is_empty())
        {
            return Err(error(
                "--real-all --send requires the exact HTTPS Ark base URL and a model key",
            ));
        }
    }
    if synthetic_all {
        if !send {
            return Err(error("--synthetic-all requires --send"));
        }
        let url = reqwest::Url::parse(&env::var("DEEPSEEK_BASE_URL")?)?;
        if url.scheme() != "http"
            || url.host_str() != Some("127.0.0.1")
            || url.port().is_none()
            || url.path() != "/v1"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(error(
                "--synthetic-all requires a 127.0.0.1 loopback Responses URL",
            ));
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/refactor/s4");
    let inputs: Vec<Case> =
        serde_json::from_slice(&checked_file(&root.join("inputs.json"), INPUTS_SHA)?)?;
    // Verify scoring data before any send; parse it only after both retrievals.
    checked_file(&root.join("labels.json"), LABELS_SHA)?;
    let requested: Vec<String> = match case_filter.as_deref() {
        None if synthetic_all || real_all => inputs.iter().map(|case| case.id.clone()).collect(),
        Some(id) if CASES.contains(&id) => vec![id.into()],
        Some(_) => return Err(error("unknown evaluation case")),
        None => CASES.iter().map(|id| (*id).into()).collect(),
    };
    if (synthetic_all || real_all) && requested.len() != 11 {
        return Err(error("S4 fixture must contain exactly 11 cases"));
    }
    let mut cases = Vec::new();
    for id in &requested {
        cases.push(
            inputs
                .iter()
                .position(|case| case.id == id.as_str())
                .ok_or("missing S4 case")?,
        );
    }
    private_dir(&run_dir)?;
    let trace_dir = run_dir.join("trace");
    private_dir(&trace_dir)?;
    // SAFETY: this standalone runner sets the trace path once, before any worker or model client.
    unsafe { env::set_var("PMR_TRACE_DIR", &trace_dir) };
    let config = chat::ChatConfig::default();
    let mut started = Vec::new();
    let mut inputs: Vec<Option<Case>> = inputs.into_iter().map(Some).collect();
    for index in cases {
        started.push(start(
            inputs[index].take().unwrap(),
            &run_dir,
            &config,
            synthetic_all || real_all,
        )?);
    }
    // A changed local baseline invalidates the planned "missing evidence" and ambiguity probes.
    if !synthetic_all
        && !real_all
        && started.iter().any(|case| {
            !case.local["source_evidence"]
                .as_array()
                .is_some_and(Vec::is_empty)
                || !case.local["historical_or_conflicting"]
                    .as_array()
                    .is_some_and(Vec::is_empty)
        })
    {
        return Err(error(
            "S4 local baseline changed; review dry-run before sending",
        ));
    }
    let mut results = Vec::new();
    for case in started {
        let result = finish(case, &config, &budget, &ledger, send)?;
        write_new(
            &run_dir.join(format!("{}.json", result["id"].as_str().unwrap())),
            &result,
        )?;
        results.push(result);
    }
    // Labels are read only after the selected Planner attempts and never enter their prompts.
    let labels: Vec<Label> =
        serde_json::from_slice(&checked_file(&root.join("labels.json"), LABELS_SHA)?)?;
    let mut scored = Vec::new();
    for result in &results {
        let id = result["id"].as_str().unwrap();
        let label = labels
            .iter()
            .find(|label| label.id == id)
            .ok_or("missing S4 label")?;
        let source_map: BTreeMap<String, String> =
            serde_json::from_value(result["source_map"].clone())?;
        scored.push(
            json!({"id":id,"local":score(&result["local"],label,&source_map)?,
            "assisted":score(&result["assisted"],label,&source_map)?}),
        );
    }
    let report = json!({"mode":if synthetic_all {"synthetic_all"} else if real_all {"real_all"} else if send {"send"} else {"dry_run"},
        "scope":if synthetic_all {"S4 11-case synthetic local/assisted engineering pair; no real-model effect claim"}
            else if real_all && send {"S4 11-case real Planner local/assisted development pair; retrieval only, not final QA"}
            else if real_all {"S4 11-case real Planner dry-run; no model request or effect claim"}
            else {"S4 N04 and N04-ambiguous development-only retrieval; not final QA"},
        "planned_cases":requested.len(),
        "completed_final_reads":results.iter().filter(|case| case["assisted"]["recall_status"]=="completed").count(),
        "trusted_at":AT,"fact_index_enabled":false,"local_budget":config.local_recall_budget,
        "model":DEEPSEEK_MODEL,"reasoning_effort":"medium","model_budget":budget,
        "ledger_path":ledger,"ledger_rows":ledger_rows(&ledger,&budget.group_id)?,
        "trace":trace_summary(&trace_dir)?,
        "inputs_sha256":INPUTS_SHA,"labels_sha256":LABELS_SHA,"cases":scored,
        "raw_case_files":requested.iter().map(|id| format!("{id}.json")).collect::<Vec<_>>(),
        "send_requested":send});
    write_new(&run_dir.join("report.json"), &report)?;
    println!("{}", serde_json::to_string(&report)?);
    Ok(())
}
