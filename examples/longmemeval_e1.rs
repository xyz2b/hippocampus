//! Isolated USER-only E1 intake and paired local recall; never reads evaluation labels.
use std::{fs, path::Path};

use hippocampus::{chat::ChatConfig, *};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Intake {
    owner_id: String,
    session_id: String,
    turn_id: String,
    text: String,
    observed_at: i64,
    #[serde(default)]
    history: Vec<String>,
    #[serde(default)]
    utc_offset_seconds: i32,
    #[serde(default)]
    case_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Query {
    owner_id: String,
    question_id: String,
    query: String,
    at: i64,
    #[serde(default)]
    utc_offset_seconds: i32,
    #[serde(default)]
    case_id: Option<String>,
}

fn read_rows<T: serde::de::DeserializeOwned>(path: &Path) -> PersistenceResult<Vec<T>> {
    fs::read_to_string(path)?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(line, text)| {
            serde_json::from_str(text)
                .map_err(|error| format!("input line {}: {error}", line + 1).into())
        })
        .collect()
}

fn registry(config: &ChatConfig) -> InMemoryPredicateRegistry {
    let mut registry = InMemoryPredicateRegistry::default();
    for definition in &config.predicates {
        registry.register(definition.predicate.clone(), definition.cardinality);
    }
    registry
}

fn request(row: &Intake) -> PersistenceResult<Value> {
    for (name, value, max) in [
        ("owner_id", row.owner_id.as_str(), 200),
        ("session_id", row.session_id.as_str(), 200),
        ("turn_id", row.turn_id.as_str(), 200),
        ("text", row.text.as_str(), 24_000),
    ] {
        if value.trim().is_empty() || value.len() > max {
            return Err(format!(
                "{name} must contain 1..={max} UTF-8 bytes; input is not truncated"
            )
            .into());
        }
    }
    time::UtcOffset::from_whole_seconds(row.utc_offset_seconds)?;
    // Fixed experiment policy: omit oversized context, never rewrite the current USER statement.
    let history: Vec<_> = row
        .history
        .iter()
        .skip(row.history.len().saturating_sub(8))
        .filter(|text| !text.trim().is_empty() && text.chars().count() <= 2000)
        .collect();
    let request = json!({"text":row.text,"session_id":row.session_id,"turn_id":row.turn_id,
        "history":history,"capture_source":true,"task_id":null,
        "observed_at":row.observed_at,"utc_offset_seconds":row.utc_offset_seconds,
        "source_origin":"longmemeval_user_import"});
    if serde_json::to_vec(&request)?.len() > 65_536 {
        return Err("intake exceeds 64 KiB; input is not truncated".into());
    }
    Ok(request)
}

// Older failed turns may add facts, but cannot rewrite or revive an existing slot.
// ponytail: deliberately conservative about scope; ambiguous historical updates stay reviewable.
fn historical_addition_allowed(
    candidate: &CandidateMemory,
    memories: &[Memory],
    config: &ChatConfig,
) -> bool {
    let Some(subject) = candidate.subject_id() else {
        return false;
    };
    if candidate.intent != CandidateIntent::Assert {
        return false;
    }
    let cardinality = config
        .predicates
        .iter()
        .find(|d| d.predicate == candidate.predicate)
        .map_or(Cardinality::Multiple, |d| d.cardinality);
    !memories.iter().any(|memory| {
        memory.owner_id == candidate.owner_id
            && &memory.subject_id == subject
            && memory.memory_type == candidate.memory_type
            && memory.predicate == candidate.predicate
            && (cardinality == Cardinality::Single || memory.value == candidate.value)
    })
}

fn ingest(
    path: &Path,
    row: &Intake,
    request: Value,
    budget: Option<&ModelBudgetConfig>,
    blocked_http_status: Option<u16>,
    resume_attempt: Option<&str>,
) -> PersistenceResult<(Value, Option<u16>)> {
    let owner = UserId::from(row.owner_id.as_str());
    let config = ChatConfig::default();
    let mode = if budget.is_some() { "inline" } else { "off" };
    let key = serde_json::to_string(&("longmemeval_e1", &row.session_id, &row.turn_id))?;
    let mut store = SqliteMemoryStore::open(path, owner.clone())?;
    store.set_fact_index_enabled(true);
    if store
        .read()?
        .entities
        .get(&EntityId::from("self"))
        .is_none()
    {
        store.transaction(|state| {
            state.entities.upsert(Entity {
                id: EntityId::from("self"),
                owner_id: owner.clone(),
                entity_type: EntityType::Person,
                canonical_name: "用户本人".into(),
                aliases: ["我", "自己", "本人", "I", "me"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
            });
            Ok(())
        })?;
    }
    let mut created = false;
    let mut historical_resume = false;
    // T1 is committed before credentials, budget binding, extraction or review can fail.
    let source = store.transaction(|state| {
        if let Some(source) = state.sources.get(&key) {
            if source.request.as_ref() != Some(&request) {
                return Err(
                    "source was deleted or its accepted input differs; no overwrite permitted"
                        .into(),
                );
            }
            if source
                .result
                .as_ref()
                .and_then(|r| r["enrichment_mode"].as_str())
                != Some(mode)
            {
                return Err(
                    "source was accepted in a different mode; use a separate evaluation snapshot"
                        .into(),
                );
            }
            if let Some(attempt) = resume_attempt {
                let already_attempted = source
                    .result
                    .as_ref()
                    .and_then(|r| r["resume_attempts"].as_array())
                    .is_some_and(|attempts| attempts.iter().any(|a| a["id"] == attempt));
                if !already_attempted
                    && matches!(
                        source.enrichment.as_str(),
                        "failed" | "extraction_incomplete" | "review_incomplete"
                    )
                {
                    historical_resume = state.sources.values().any(|later| {
                        later.received_sequence > source.received_sequence
                            && (later.enrichment.starts_with("completed_")
                                || matches!(
                                    later.enrichment.as_str(),
                                    "committed" | "stale_requires_review"
                                ))
                    });
                    let source = state.sources.get_mut(&key).unwrap();
                    let previous = json!({"id":attempt,"previous_enrichment":source.enrichment,
                        "previous_error":source.error,"historical_guard":historical_resume});
                    let result = source.result.as_mut().unwrap();
                    if !result["resume_attempts"].is_array() {
                        result["resume_attempts"] = json!([]);
                    }
                    result["resume_attempts"]
                        .as_array_mut()
                        .unwrap()
                        .push(previous);
                    source.enrichment = "pending".into();
                    source.error = None;
                    source.base_revision = state.revision;
                    created = true;
                    return Ok(source.clone());
                }
            }
            return Ok(source.clone());
        }
        if resume_attempt.is_some() {
            return Err("resume requires an already accepted source".into());
        }
        if state
            .sources
            .values()
            .any(|s| s.observed_at > row.observed_at)
        {
            return Err("intake must be in stable chronological order for each owner".into());
        }
        let sequence = state
            .sources
            .values()
            .map(|s| s.received_sequence)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or("source sequence overflow")?;
        let source = SourceRecord {
            id: diagnostics::new_id(),
            session_id: row.session_id.clone(),
            turn_id: row.turn_id.clone(),
            observed_at: row.observed_at,
            received_sequence: sequence,
            request: Some(request.clone()),
            enrichment: if budget.is_some() {
                "pending"
            } else {
                "not_requested"
            }
            .into(),
            error: None,
            result: Some(json!({"enrichment_mode":mode,"outcomes":[]})),
            base_revision: state.revision,
            index: Value::Null,
            task: Default::default(),
        };
        state.sources.insert(key.clone(), source.clone());
        created = true;
        Ok(source)
    })?;
    let mut model_client = None;
    let mut processing_error = None;
    if created && let Some(budget) = budget {
        let operation_id = resume_attempt
            .map(|attempt| format!("{}:resume:{attempt}", source.id))
            .unwrap_or_else(|| source.id.clone());
        let _operation = diagnostics::Operation::enter(&operation_id);
        let processed = (|| -> PersistenceResult<()> {
            if blocked_http_status.is_some() {
                return Ok(());
            }
            let state = store.read()?;
            let context = OpenAiAdapterContext::new(
                owner.clone(),
                state.entities.list_by_owner(&owner),
                Timestamp(row.observed_at),
            )?
            .with_utc_offset_seconds(row.utc_offset_seconds)?
            .with_semantic_catalog(config.predicates.clone(), config.domains.clone())?
            .with_predicate_value_kinds(config.value_kinds.clone())?
            .with_predicate_memory_types(config.memory_types.clone())?;
            let client = OpenAiClient::new(
                std::env::var("DEEPSEEK_BASE_URL")
                    .map_err(|_| OpenAiAdapterError::MissingBaseUrl)?,
                std::env::var("DEEPSEEK_API_KEY").map_err(|_| OpenAiAdapterError::MissingApiKey)?,
            )?
            .with_budget(path, owner.0.clone(), budget.clone(), &operation_id)?;
            model_client = Some(client.clone());
            let history: Vec<_> = request["history"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            let candidates = OpenAiMemoryExtractor::new(
                client,
                context,
                SessionId(row.session_id.clone()),
                TurnId(row.turn_id.clone()),
            )
            .extract_turn(&history, &row.text)?;
            store.transaction(|state| {
                let current = state.sources.get(&key).ok_or("source missing")?;
                if current.request.as_ref() != Some(&request) || current.enrichment != "pending"
                    || state.revision != source.base_revision {
                    return Err("source or memory state changed during extraction; candidates not committed".into());
                }
                let mut stale_candidates = Vec::new();
                let outcomes = state.with_engine(registry(&config), |engine, entities| {
                    let mut outcomes = Vec::new();
                    for candidate in candidates {
                        if historical_resume && !historical_addition_allowed(&candidate,
                            &engine.repository.list_by_owner(&owner)?, &config) {
                            stale_candidates.push(candidate);
                        } else {
                            outcomes.push(engine.ingest_or_stage(candidate, entities)?);
                        }
                    }
                    Ok(outcomes)
                })?;
                let source = state.sources.get_mut(&key).unwrap();
                source.enrichment = if !stale_candidates.is_empty() { "stale_requires_review" }
                    else if outcomes.is_empty() { "completed_no_facts" }
                    else if outcomes.iter().any(|o| matches!(o, CandidateIngestOutcome::Ingested(IngestOutcome::Applied { .. }))) { "committed" }
                    else if outcomes.iter().any(|o| matches!(o, CandidateIngestOutcome::Pending { .. })) { "completed_pending" }
                    else { "completed_rejected" }.into();
                let result = source.result.as_mut().unwrap();
                result["outcomes"] = json!(outcomes);
                if !stale_candidates.is_empty() {
                    result["stale_candidates"] = json!(stale_candidates);
                    result["stale_reason"] = json!("historical_retry_cannot_update_existing_slot");
                }
                Ok(())
            })
        })();
        let failure = if let Some(status) = blocked_http_status {
            Some(("failed", format!("batch_blocked_after_http_{status}")))
        } else if let Err(error) = processed {
            let (status, reason) = match error.downcast_ref::<OpenAiAdapterError>() {
                Some(OpenAiAdapterError::Incomplete(reason))
                    if reason.starts_with("extraction:") =>
                {
                    ("extraction_incomplete", "extraction_incomplete")
                }
                Some(OpenAiAdapterError::Incomplete(_)) => {
                    ("review_incomplete", "review_incomplete")
                }
                Some(OpenAiAdapterError::Budget(_)) => ("failed", "model_budget_denied"),
                Some(OpenAiAdapterError::Api { status: 401, .. }) => ("failed", "model_http_401"),
                Some(OpenAiAdapterError::Api { status: 403, .. }) => ("failed", "model_http_403"),
                _ => ("failed", "enrichment_failed"),
            };
            Some((status, reason.to_owned()))
        } else {
            None
        };
        if let Some((status, reason)) = failure {
            processing_error = Some(reason.clone());
            store.transaction(|state| {
                if let Some(source) = state.sources.get_mut(&key)
                    && source.request.as_ref() == Some(&request)
                    && source.enrichment == "pending"
                {
                    source.enrichment = status.into();
                    source.error = Some(reason);
                }
                Ok(())
            })?;
        }
    }
    let state = store.read()?;
    let source = state.sources.get(&key).ok_or("source receipt missing")?;
    let mut receipt = source.status();
    receipt["source"]["provenance"] = json!("longmemeval_user_import");
    let belongs_to_attempt = resume_attempt.is_none()
        || source
            .result
            .as_ref()
            .and_then(|r| r["resume_attempts"].as_array())
            .and_then(|attempts| attempts.last())
            .is_some_and(|attempt| attempt["id"] == resume_attempt.unwrap());
    let fatal_http_status = match source.error.as_deref().filter(|_| belongs_to_attempt) {
        Some("model_http_401" | "batch_blocked_after_http_401") => Some(401),
        Some("model_http_403" | "batch_blocked_after_http_403") => Some(403),
        _ => None,
    };
    Ok((
        json!({"case_id":row.case_id,"owner_id":row.owner_id,"session_id":row.session_id,
        "turn_id":row.turn_id,"observed_at":row.observed_at,"utc_offset_seconds":row.utc_offset_seconds,
        "enrichment_mode":mode,"resume_attempt":resume_attempt,"replayed":!created,"receipt":receipt,"result":source.result,
        "error":processing_error.as_deref().or(source.error.as_deref()),
        "history":{"provided":row.history.len(),"used":request["history"].as_array().unwrap().len(),
            "omitted":row.history.len()-request["history"].as_array().unwrap().len(),
            "policy":"previous_8_USER_messages; omit_context_over_2000_characters; current_text_unchanged"},
        "model_calls":model_client.map(|c| c.model_budget_report()).unwrap_or_default()}),
        fatal_http_status,
    ))
}

fn pair(path: &Path, row: &Query) -> PersistenceResult<Value> {
    time::UtcOffset::from_whole_seconds(row.utc_offset_seconds)?;
    let config = ChatConfig::default();
    let owner = UserId::from(row.owner_id.as_str());
    let mut store = SqliteMemoryStore::open_read_only(path, owner.clone())?;
    let before = serde_json::to_value(store.read()?)?;
    let runtime = RuntimeContext::current(owner, Timestamp(row.at));
    let mut relations = InMemorySemanticRelationRegistry::default();
    for (child, parent) in &config.is_a {
        relations.register_is_a(child.clone(), parent.clone());
    }
    let mut results = Vec::new();
    for enabled in [false, true] {
        store.set_fact_index_enabled(enabled);
        let trace_run_id = diagnostics::new_id();
        let _operation = diagnostics::Operation::enter(&trace_run_id);
        let result = store.recall_local(
            &row.query,
            &[],
            &runtime,
            registry(&config),
            &relations,
            &config.local_recall_budget,
        )?;
        results.push(
            json!({"fact_index_enabled":enabled,"trace_run_id":trace_run_id,"result":result}),
        );
    }
    if before != serde_json::to_value(store.read()?)?
        || results[0]["result"]["read_revision"] != results[1]["result"]["read_revision"]
        || results[0]["result"]["source_revision"] != results[1]["result"]["source_revision"]
    {
        return Err("evaluation snapshot changed during the pair; discard this comparison".into());
    }
    Ok(
        json!({"case_id":row.case_id,"question_id":row.question_id,"owner_id":row.owner_id,
        "query":row.query,"at":row.at,"utc_offset_seconds":row.utc_offset_seconds,
        "source_provenance":"longmemeval_USER_import",
        "shared_snapshot_verified":true,"backend_generation_calls":0,"paired":results}),
    )
}

#[cfg(test)]
#[path = "../tests/common/mod.rs"]
mod common;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn historical_retry_only_adds_unoccupied_slots_across_all_statuses() {
        let config = ChatConfig::default();
        let owner = UserId::from("fixture");
        let mut candidate = CandidateMemory::new(
            owner.clone(),
            EntityId::from("self"),
            MemoryType::Fact,
            Predicate::from("lives_in"),
            MemoryValue::from("Beijing"),
            Timestamp(100),
            Evidence::new(
                EvidenceId::from("e1"),
                SessionId::from("s1"),
                TurnId::from("t1"),
                "I live in Beijing.",
            ),
        );
        let mut engine = MemoryEngine::new(InMemoryMemoryRepository::default(), registry(&config));
        engine.ingest(common::grounded(candidate.clone())).unwrap();
        let mut memories = engine.repository.list_by_owner(&owner).unwrap();
        assert_eq!(memories.len(), 1);
        candidate.value = MemoryValue::from("Shanghai");
        candidate.scope.domain = Some(Domain::from("travel"));
        for status in [
            MemoryStatus::Active,
            MemoryStatus::Superseded,
            MemoryStatus::Expired,
            MemoryStatus::Deleted,
        ] {
            memories[0].status = status;
            assert!(!historical_addition_allowed(&candidate, &memories, &config));
        }
        candidate.predicate = Predicate::from("fact");
        memories[0].predicate = candidate.predicate.clone();
        assert!(historical_addition_allowed(&candidate, &memories, &config));
        candidate.value = memories[0].value.clone();
        assert!(!historical_addition_allowed(&candidate, &memories, &config));
        assert!(historical_addition_allowed(&candidate, &[], &config));
        candidate.intent = CandidateIntent::Retract { target: None };
        assert!(!historical_addition_allowed(&candidate, &[], &config));
        candidate.intent = CandidateIntent::Assert;
        candidate.subject = CandidateSubject::Unresolved("unknown".into());
        assert!(!historical_addition_allowed(&candidate, &[], &config));
    }

    #[test]
    fn bounded_intake_replays_without_models_or_mode_changes() {
        let path = std::env::temp_dir().join(format!("lme-off-{}.sqlite3", diagnostics::new_id()));
        let mut row = Intake {
            owner_id: "c0".into(),
            session_id: "s0".into(),
            turn_id: "t0".into(),
            text: "合成测试青榆围巾".into(),
            observed_at: 100,
            utc_offset_seconds: 0,
            history: (0..9).map(|i| format!("previous {i}")).collect(),
            case_id: None,
        };
        row.history[3] = "长".repeat(2001);
        let input = request(&row).unwrap();
        assert_eq!(input["history"].as_array().unwrap().len(), 7);
        assert_eq!(input["text"], row.text);
        let (first, _) = ingest(&path, &row, input.clone(), None, None, None).unwrap();
        assert_eq!(first["receipt"]["enrichment"]["status"], "not_requested");
        assert_eq!(first["model_calls"], json!([]));
        let (replay, _) = ingest(&path, &row, input.clone(), None, None, None).unwrap();
        assert_eq!(replay["replayed"], true);
        assert_eq!(
            replay["receipt"]["source"]["source_id"],
            first["receipt"]["source"]["source_id"]
        );
        let budget = ModelBudgetConfig {
            group_id: "fixture".into(),
            max_calls: 2,
            ..Default::default()
        };
        assert!(
            ingest(&path, &row, input, Some(&budget), None, None)
                .unwrap_err()
                .to_string()
                .contains("different mode")
        );
        let result = pair(
            &path,
            &Query {
                owner_id: "c0".into(),
                question_id: "c0".into(),
                query: "青榆围巾".into(),
                at: 101,
                utc_offset_seconds: 0,
                case_id: None,
            },
        )
        .unwrap();
        assert_eq!(result["shared_snapshot_verified"], true);
        assert_eq!(result["backend_generation_calls"], 0);
        for variant in result["paired"].as_array().unwrap() {
            assert!(!variant["trace_run_id"].as_str().unwrap().is_empty());
        }
        assert_ne!(
            result["paired"][0]["trace_run_id"],
            result["paired"][1]["trace_run_id"]
        );
        assert_eq!(
            result["paired"][0]["result"]["source_evidence"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            result["paired"][1]["result"]["source_evidence"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        row.text = "原".repeat(8001);
        assert!(
            request(&row).is_err(),
            "MCP's 24000 UTF-8 byte limit must not truncate text"
        );
        fs::remove_file(path).unwrap();
    }
}

fn main() -> PersistenceResult<()> {
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_else(|| "--help".into());
    if matches!(command.as_str(), "--help" | "-h") {
        println!(
            "longmemeval_e1 ingest --db PATH --input INTAKE.jsonl [--inline --budget BUDGET.json [--resume-attempt ID]]\nlongmemeval_e1 pair --db PATH --input QUERIES.jsonl\nExplicit isolated paths only. Resume retries failed existing sources once per explicit attempt ID, skips completed sources and stops at 401/403 or exhausted budget. Inline uses normal extraction and finite review; live providers require PMR_MODEL_NETWORK=live. Offline permits literal loopback fixtures. No labels, overwrite or default database. Pair is read-only and never calls a model."
        );
        return Ok(());
    }
    let (mut db, mut input, mut budget_path, mut inline) = (None, None, None, false);
    let mut resume_attempt = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--db" => db = Some(args.next().ok_or("--db requires a path")?),
            "--input" => input = Some(args.next().ok_or("--input requires a path")?),
            "--budget" => budget_path = Some(args.next().ok_or("--budget requires a path")?),
            "--inline" => inline = true,
            "--resume-attempt" => {
                resume_attempt = Some(args.next().ok_or("--resume-attempt requires an ID")?)
            }
            _ => return Err(format!("unknown argument {arg}").into()),
        }
    }
    let db = db.ok_or("--db is required")?;
    let input = input.ok_or("--input is required")?;
    if let Some(attempt) = resume_attempt.as_deref()
        && (!inline
            || command != "ingest"
            || attempt.is_empty()
            || attempt.len() > 100
            || !attempt
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b)))
    {
        return Err("resume requires inline ingest and a 1..=100 character alphanumeric, '-' or '_' attempt ID".into());
    }
    match command.as_str() {
        "ingest" => {
            if inline != budget_path.is_some() {
                return Err("--inline and --budget must be provided together".into());
            }
            let budget: Option<ModelBudgetConfig> = budget_path
                .map(|p| -> PersistenceResult<_> {
                    let config: ModelBudgetConfig = serde_json::from_slice(&fs::read(p)?)?;
                    config.validate()?;
                    Ok(config)
                })
                .transpose()?;
            let rows: Vec<Intake> = read_rows(Path::new(&input))?;
            let requests: Vec<_> = rows.iter().map(request).collect::<PersistenceResult<_>>()?;
            let mut failures = 0;
            let mut blocked_http_status = None;
            for (row, request) in rows.iter().zip(requests) {
                let (report, fatal_http_status) = ingest(
                    Path::new(&db),
                    row,
                    request,
                    budget.as_ref(),
                    blocked_http_status,
                    resume_attempt.as_deref(),
                )?;
                blocked_http_status = blocked_http_status.or(fatal_http_status);
                failures += usize::from(
                    !report["error"].is_null()
                        || matches!(
                            report["receipt"]["enrichment"]["status"].as_str(),
                            Some("pending" | "stale_requires_review")
                        ),
                );
                println!("{report}");
                if resume_attempt.is_some()
                    && (fatal_http_status.is_some() || report["error"] == "model_budget_denied")
                {
                    break;
                }
            }
            if failures != 0 {
                return Err(format!(
                    "{failures} intake rows remain incomplete; sources retained, no automatic retry"
                )
                .into());
            }
        }
        "pair" if !inline && budget_path.is_none() => {
            for row in read_rows::<Query>(Path::new(&input))? {
                println!("{}", pair(Path::new(&db), &row)?);
            }
        }
        _ => return Err("use ingest or pair; pair does not accept model options".into()),
    }
    Ok(())
}
