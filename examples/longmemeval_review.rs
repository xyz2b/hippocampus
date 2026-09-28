//! Explicit, atomic review of already recorded historical candidates; no model calls.
use std::{collections::BTreeSet, fs, path::Path, process::Command};

use hippocampus::{
    CandidateIngestOutcome, CandidateIntent, CandidateMemory, CandidateSubject, Cardinality,
    DefaultAdmissionPolicy, DefaultReconciliationPolicy, DefaultRetentionPolicy, Entity, EntityId,
    EntityRepository, EntityResolution, InMemoryPredicateRegistry, IngestOutcome, Memory,
    MemoryEngine, MemoryId, MemoryRepository, MemoryStatus, MemoryType, OpenAiAdapterContext,
    PersistedMemoryState, PersistenceResult, ReconciliationAction, ReconciliationDecision,
    ReconciliationPolicy, ScopeCondition, SessionId, SourceRecord, SqliteMemoryStore,
    SupersedeReason, TaskId, Timestamp, TurnId, UserId, chat::ChatConfig,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ReviewPlan {
    review_id: String,
    owner_id: UserId,
    entities: Vec<Entity>,
    sources: Vec<SourceReview>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SourceReview {
    source_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    recorded_review: Option<RecordedReview>,
    decisions: Vec<Decision>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RecordedReview {
    recording_path: String,
    recording_sha256: String,
    model_start_line: usize,
    model_end_line: usize,
    validation_line: usize,
    model_start: Value,
    model_end: Value,
    validation: Value,
    output_text: String,
    normalizations: Value,
    parsed_candidates: Vec<CandidateMemory>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Decision {
    Covered {
        candidate_index: usize,
        target: MemoryId,
    },
    Bind {
        candidate_index: usize,
        subject_id: EntityId,
        task_id: Option<String>,
        corrects: Option<MemoryId>,
    },
    ReviewedNew {
        candidate_index: usize,
        subject_id: EntityId,
        task_id: Option<String>,
        reason: String,
    },
    Reject {
        candidate_index: usize,
        reason: String,
    },
}

impl Decision {
    fn index(&self) -> usize {
        match self {
            Self::Covered {
                candidate_index, ..
            }
            | Self::Bind {
                candidate_index, ..
            }
            | Self::ReviewedNew {
                candidate_index, ..
            }
            | Self::Reject {
                candidate_index, ..
            } => *candidate_index,
        }
    }
}

fn source_key(state: &PersistedMemoryState, id: &str) -> PersistenceResult<String> {
    let matches: Vec<_> = state.sources.iter().filter(|(_, s)| s.id == id).collect();
    match matches.as_slice() {
        [(key, _)] => Ok((*key).clone()),
        _ => Err(format!("source {id} is missing or ambiguous").into()),
    }
}

fn replay(state: &PersistedMemoryState, plan: &ReviewPlan) -> PersistenceResult<Option<Value>> {
    let first = plan.sources.first().ok_or("empty review plan")?;
    let markers: Vec<_> = state
        .sources
        .values()
        .filter(|s| {
            s.result
                .as_ref()
                .and_then(|r| r.pointer("/historical_review/review_id"))
                == Some(&json!(plan.review_id))
        })
        .collect();
    let first_source = &state.sources[&source_key(state, &first.source_id)?];
    let marker = first_source
        .result
        .as_ref()
        .and_then(|r| r.get("historical_review"));
    if markers.is_empty() && marker.is_none() {
        return Ok(None);
    }
    let marker = marker.ok_or("review receipt missing from first source")?;
    if marker.get("plan") != Some(&serde_json::to_value(plan)?)
        || markers.len() != plan.sources.len()
    {
        return Err("review plan differs or its persisted receipt is incomplete".into());
    }
    let mut seen = BTreeSet::new();
    for source in &plan.sources {
        let record = &state.sources[&source_key(state, &source.source_id)?];
        let saved = record
            .result
            .as_ref()
            .and_then(|r| r.get("historical_review"))
            .ok_or("review marker missing")?;
        if !seen.insert(&source.source_id)
            || saved["review_id"] != plan.review_id
            || saved["decisions"] != serde_json::to_value(&source.decisions)?
            || !matches!(
                record.enrichment.as_str(),
                "committed" | "completed_no_facts"
            )
        {
            return Err("review markers do not match the complete plan".into());
        }
    }
    let mut result = marker
        .get("result")
        .filter(|r| r.is_object())
        .cloned()
        .ok_or("aggregate review result missing")?;
    result["replayed"] = json!(true);
    Ok(Some(result))
}

// This example's sole exception is an exact, prevalidated correction pair. Everything else
// keeps the normal policy, including admission, duplicate checks and version-chain updates.
struct ReviewedCorrection(Option<(CandidateMemory, Memory)>);

impl ReconciliationPolicy for ReviewedCorrection {
    fn decide(
        &self,
        candidate: &CandidateMemory,
        existing: &[Memory],
        cardinality: Cardinality,
    ) -> ReconciliationDecision {
        if let Some((approved, target)) = &self.0
            && candidate == approved
            && existing.iter().any(|m| m == target)
        {
            return ReconciliationDecision {
                action: ReconciliationAction::Supersede,
                target: Some(target.id),
                reason: Some(SupersedeReason::Correction),
            };
        }
        DefaultReconciliationPolicy.decide(candidate, existing, cardinality)
    }
}

fn registry() -> InMemoryPredicateRegistry {
    let mut registry = InMemoryPredicateRegistry::default();
    for definition in ChatConfig::default().predicates {
        registry.register(definition.predicate, definition.cardinality);
    }
    registry
}

fn validate_evidence(
    candidate: &CandidateMemory,
    source: &SourceRecord,
    owner: &UserId,
) -> PersistenceResult<()> {
    let request = source
        .request
        .as_ref()
        .ok_or("source raw request is deleted")?;
    let text = request
        .get("text")
        .and_then(Value::as_str)
        .ok_or("source text missing")?;
    // SourceRecord has no owner field: it belongs to the owner-scoped profile loaded by the store.
    if &candidate.owner_id != owner
        || request.get("owner_id").is_some_and(|v| v != &json!(owner))
        || candidate.evidence.is_empty()
        || candidate.evidence.iter().any(|e| {
            e.id.0.trim().is_empty()
                || e.session_id.0 != source.session_id
                || e.turn_id.0 != source.turn_id
                || e.quote.trim().is_empty()
                || !text.contains(&e.quote)
        })
    {
        return Err("candidate owner or original source evidence does not match".into());
    }
    Ok(())
}

// This offline example deliberately requires the original local trace, not an arbitrary import.
fn recorded_candidates(
    recorded: &RecordedReview,
    source: &SourceRecord,
    state: &PersistedMemoryState,
    owner: &UserId,
) -> PersistenceResult<Vec<CandidateMemory>> {
    let digest = Command::new("shasum")
        .args(["-a", "256", "--", &recorded.recording_path])
        .output()?;
    if !digest.status.success()
        || recorded.recording_sha256.len() != 64
        || !recorded
            .recording_sha256
            .bytes()
            .all(|b| b.is_ascii_hexdigit())
        || String::from_utf8(digest.stdout)?.split_whitespace().next()
            != Some(recorded.recording_sha256.as_str())
    {
        return Err("recorded review trace SHA-256 mismatch".into());
    }
    let recording = if Path::new(&recorded.recording_path)
        .extension()
        .is_some_and(|ext| ext == "gz")
    {
        let decoded = Command::new("gzip")
            .args(["-dc", "--", &recorded.recording_path])
            .output()?;
        if !decoded.status.success() {
            return Err("recorded trace gzip decoding failed".into());
        }
        String::from_utf8(decoded.stdout)?
    } else {
        fs::read_to_string(&recorded.recording_path)?
    };
    let lines: Vec<_> = recording.lines().collect();
    for (line, event) in [
        (recorded.model_start_line, &recorded.model_start),
        (recorded.model_end_line, &recorded.model_end),
        (recorded.validation_line, &recorded.validation),
    ] {
        if line == 0
            || lines
                .get(line - 1)
                .map(|text| serde_json::from_str::<Value>(text))
                .transpose()?
                .as_ref()
                != Some(event)
        {
            return Err("recorded review event does not match its original trace line".into());
        }
    }
    let (start, end, validation) = (
        &recorded.model_start,
        &recorded.model_end,
        &recorded.validation,
    );
    let start_data = &start["data"];
    let end_data = &end["data"];
    let response = &end_data["response"];
    let nonempty = |v: &Value| v.as_str().is_some_and(|s| !s.trim().is_empty());
    let original_result = source
        .result
        .as_ref()
        .ok_or("failed source result missing")?;
    let attempt = original_result["resume_attempts"]
        .as_array()
        .and_then(|a| a.last())
        .and_then(|a| a["id"].as_str())
        .ok_or("failed source has no recorded resume attempt")?;
    let operation = format!("{}:resume:{attempt}", source.id);
    if source.enrichment != "failed"
        || source.error.is_none()
        || recorded.model_start_line >= recorded.model_end_line
        || recorded.validation_line != recorded.model_end_line + 1
        || start["kind"] != "model_start"
        || end["kind"] != "model_end"
        || validation["kind"] != "validation"
        || start["run_id"] != operation
        || end["run_id"] != operation
        || validation["run_id"] != operation
        || start_data["stage"] != "personal_memory_candidates_verification_v1"
        || !nonempty(&start_data["call_id"])
        || start_data["call_id"] != end_data["call_id"]
        || end_data["accounting"]["call_id"] != start_data["call_id"]
        || end_data["accounting"]["operation_id"] != operation
        || end_data["accounting"]["stage"] != "extraction_review"
        || !nonempty(&start_data["request"]["model"])
        || !nonempty(&response["model"])
        || !nonempty(&response["id"])
        || end_data["http_status"] != 200
        || end_data["status"] != "completed"
        || response["status"] != "completed"
        || end_data.get("error") != Some(&Value::Null)
        || response.get("error") != Some(&Value::Null)
        || end_data["parsing_status"] != "valid_json"
        || validation["data"]["stage"] != "extraction_review"
        || validation["data"]["status"] != "failed"
    {
        return Err("recorded review is not the failed source's completed final review".into());
    }
    let input: Value = serde_json::from_str(
        start_data["request"]["input"]
            .as_str()
            .ok_or("recorded input missing")?,
    )?;
    let request = source
        .request
        .as_ref()
        .ok_or("source raw request deleted")?;
    if input["owner_id"] != json!(owner)
        || input["session_id"] != source.session_id
        || input["turn_id"] != source.turn_id
        || input["now"] != source.observed_at
        || input["current_user_turn"] != request["text"]
        || input["conversation_history"] != request["history"]
        || input["caller_task_id"] != request["task_id"]
    {
        return Err("recorded review input differs from the original source".into());
    }
    let mut output = String::new();
    for message in response["output"]
        .as_array()
        .ok_or("recorded output missing")?
    {
        for part in message["content"]
            .as_array()
            .ok_or("recorded message content missing")?
        {
            if message["type"] != "message" || part["type"] != "output_text" {
                return Err("recorded review contains non-text or refusal output".into());
            }
            output.push_str(
                part["text"]
                    .as_str()
                    .ok_or("recorded output text missing")?,
            );
        }
    }
    if output.is_empty() || output != recorded.output_text {
        return Err("recorded output text does not match the completed response".into());
    }
    let mut entities = Vec::new();
    for entity in input["known_entities"]
        .as_array()
        .ok_or("recorded entity catalog missing")?
    {
        let id = EntityId::from(entity["id"].as_str().ok_or("recorded entity id missing")?);
        entities.push(
            state
                .entities
                .get(&id)
                .ok_or("recorded entity missing from current profile")?
                .clone(),
        );
    }
    let config = ChatConfig::default();
    let mut context =
        OpenAiAdapterContext::new(owner.clone(), entities, Timestamp(source.observed_at))?
            .with_utc_offset_seconds(i32::try_from(
                request["utc_offset_seconds"]
                    .as_i64()
                    .ok_or("source UTC offset missing")?,
            )?)?
            .with_semantic_catalog(config.predicates, config.domains)?
            .with_predicate_value_kinds(config.value_kinds)?
            .with_predicate_memory_types(config.memory_types)?;
    if let Some(task) = request["task_id"].as_str() {
        context = context.with_task_id(TaskId::from(task))?;
    }
    // Reparse the unchanged response; the annotation list is preserved but never applied as a patch.
    let candidates = context.decode_extraction(
        &output,
        request["text"].as_str().ok_or("source text missing")?,
        &SessionId::from(source.session_id.as_str()),
        &TurnId::from(source.turn_id.as_str()),
    )?;
    if candidates != recorded.parsed_candidates {
        return Err(
            "supplied candidates differ from offline decoding of the recorded review".into(),
        );
    }
    Ok(candidates)
}

fn bind_task(candidate: &mut CandidateMemory, task_id: &Option<String>) -> PersistenceResult<()> {
    if let Some(task) = task_id {
        if task.trim().is_empty()
            || candidate
                .scope
                .conditions
                .iter()
                .any(|c| matches!(c, ScopeCondition::Task(existing) if existing.0 != *task))
        {
            return Err("task binding cannot replace an existing task".into());
        }
        let condition = ScopeCondition::Task(TaskId(task.clone()));
        if !candidate.scope.conditions.contains(&condition) {
            candidate.scope.conditions.push(condition);
        }
    }
    Ok(())
}

fn correction_target(
    before: &PersistedMemoryState,
    source: &SourceRecord,
    candidate: &CandidateMemory,
    target: MemoryId,
) -> PersistenceResult<Memory> {
    let old = before
        .repository
        .get(target)?
        .ok_or("correction target missing")?;
    let earlier_same_session = old.evidence.iter().any(|e| {
        before.sources.values().any(|s| {
            e.session_id.0 == source.session_id
                && s.session_id == e.session_id.0
                && s.turn_id == e.turn_id.0
                && s.request.is_some()
                && s.received_sequence < source.received_sequence
        })
    });
    if old.owner_id != candidate.owner_id
        || old.status != MemoryStatus::Active
        || old.memory_type != MemoryType::Fact
        || candidate.memory_type != MemoryType::Fact
        || old.predicate.0 != "fact"
        || candidate.predicate.0 != "fact"
        || candidate.subject_id() == Some(&old.subject_id)
        || old.value == candidate.value
        || !source
            .request
            .as_ref()
            .and_then(|r| r["text"].as_str())
            .is_some_and(|t| t.contains("I meant to say"))
        || !earlier_same_session
    {
        return Err("correction does not match the narrowly reviewed historical case".into());
    }
    Ok(old)
}

fn apply(
    store: &mut SqliteMemoryStore,
    plan: &ReviewPlan,
    before: &PersistedMemoryState,
) -> PersistenceResult<Value> {
    if store.owner() != &plan.owner_id || plan.review_id.trim().is_empty() {
        return Err("review owner or identity is invalid".into());
    }
    if let Some(result) = replay(&store.read()?, plan)? {
        return Ok(result);
    }
    let expected = serde_json::to_value(before)?;
    store.transaction(|state| {
        if serde_json::to_value(&*state)? != expected {
            return Err("full snapshot changed since the authorized review plan".into());
        }
        let planned: BTreeSet<_> = plan.sources.iter().map(|s| s.source_id.as_str()).collect();
        let stale: BTreeSet<_> = state.sources.values().filter(|s| s.enrichment == "stale_requires_review")
            .map(|s| s.id.as_str()).collect();
        let recorded = plan.sources.iter().any(|s| s.recorded_review.is_some());
        if planned.len() != plan.sources.len() || planned.is_empty()
            || (recorded && (!plan.entities.is_empty() || plan.sources.iter().any(|s| s.recorded_review.is_none())))
            || (!recorded && planned != stale)
        {
            return Err("plan must cover all stale sources, or explicitly select recorded failed sources without new entities".into());
        }
        let mut new_entities = BTreeSet::new();
        for entity in &plan.entities {
            if entity.owner_id != plan.owner_id || entity.id.0.trim().is_empty() || entity.id.0 == "self"
                || entity.canonical_name.trim().is_empty() || state.entities.get(&entity.id).is_some()
                || !new_entities.insert(entity.id.clone())
            {
                return Err("entity must be new, non-self and owned by this profile".into());
            }
            state.entities.upsert(entity.clone());
        }
        let mut engine = MemoryEngine {
            repository: std::mem::take(&mut state.repository), predicates: registry(),
            admission: DefaultAdmissionPolicy, reconciliation: ReviewedCorrection(None),
            retention: DefaultRetentionPolicy, staging: std::mem::take(&mut state.staging),
        };
        let mut reports = Vec::new();
        let (mut covered_count, mut applied_count, mut rejected_count) = (0, 0, 0);
        for source_plan in &plan.sources {
            let key = source_key(state, &source_plan.source_id)?;
            let original_source = state.sources[&key].clone();
            let original_result = original_source.result.as_ref().ok_or("source result missing")?;
            let candidates: Vec<CandidateMemory> = if let Some(recording) = &source_plan.recorded_review {
                recorded_candidates(recording, &original_source, state, &plan.owner_id)?
            } else {
                serde_json::from_value(original_result.get("stale_candidates").ok_or("stale candidates missing")?.clone())?
            };
            let original_candidates = if recorded { serde_json::to_value(&candidates)? }
                else { original_result["stale_candidates"].clone() };
            let indices: BTreeSet<_> = source_plan.decisions.iter().map(Decision::index).collect();
            if candidates.is_empty() || source_plan.decisions.len() != candidates.len()
                || indices != (0..candidates.len()).collect()
            {
                return Err("every candidate needs exactly one decision".into());
            }
            let mut outcomes = original_result.get("outcomes").and_then(Value::as_array).cloned()
                .ok_or("original outcomes must be an array")?;
            let mut decisions = Vec::new();
            for decision in &source_plan.decisions {
                let mut candidate = candidates[decision.index()].clone();
                validate_evidence(&candidate, &original_source, &plan.owner_id)?;
                if recorded != matches!(decision, Decision::ReviewedNew { .. } | Decision::Reject { .. }) {
                    return Err("recorded and stale review decisions cannot be interchanged".into());
                }
                match decision {
                    Decision::Covered { candidate_index, target } => {
                        let old = before.repository.get(*target)?.ok_or("covered target missing from original snapshot")?;
                        if old.status != MemoryStatus::Active || old.owner_id != candidate.owner_id
                            || candidate.subject_id() != Some(&old.subject_id) || old.memory_type != candidate.memory_type
                            || old.predicate != candidate.predicate || old.value != candidate.value || old.polarity != candidate.polarity
                            || engine.repository.get(*target)?.as_ref() != Some(&old)
                        {
                            return Err("covered target is not the same active assertion".into());
                        }
                        decisions.push(json!({"candidate_index":candidate_index,"action":"already_covered","target":target}));
                        covered_count += 1;
                    }
                    Decision::Bind { candidate_index, subject_id, task_id, corrects } => {
                        let CandidateSubject::Unresolved(reference) = &candidate.subject else {
                            return Err("bind requires the original unresolved reference".into());
                        };
                        let entity = state.entities.get(subject_id).ok_or("bound entity missing")?;
                        if !new_entities.contains(subject_id) || entity.owner_id != plan.owner_id
                            || !(entity.canonical_name == *reference || entity.aliases.contains(reference))
                            || state.entities.resolve(&plan.owner_id, reference) != EntityResolution::Resolved(subject_id.clone())
                        {
                            return Err("new entity does not uniquely resolve the exact original reference".into());
                        }
                        candidate.subject = CandidateSubject::Resolved(subject_id.clone());
                        bind_task(&mut candidate, task_id)?;
                        engine.reconciliation.0 = if let Some(target) = corrects {
                            let old = correction_target(before, &original_source, &candidate, *target)?;
                            if engine.repository.get(*target)?.as_ref() != Some(&old) {
                                return Err("correction target changed during review".into());
                            }
                            candidate.intent = CandidateIntent::Supersede { target: Some(*target), reason: SupersedeReason::Correction };
                            Some((candidate.clone(), old))
                        } else { None };
                        let outcome = engine.ingest_or_stage(candidate.clone(), &state.entities)?;
                        let expected_action = if corrects.is_some() { ReconciliationAction::Supersede } else { ReconciliationAction::New };
                        let memory_id = match &outcome {
                            CandidateIngestOutcome::Ingested(IngestOutcome::Applied { action, memory_id: Some(id), target })
                                if *action == expected_action && target == corrects => *id,
                            _ => return Err(format!("review decision was not applied: {outcome:?}").into()),
                        };
                        outcomes.push(serde_json::to_value(&outcome)?);
                        decisions.push(json!({"candidate_index":candidate_index,"action":"bind","resolved_candidate":candidate,"outcome":outcome,"memory_id":memory_id}));
                        applied_count += 1;
                    }
                    Decision::ReviewedNew { candidate_index, subject_id, task_id, reason } => {
                        if reason.trim().is_empty()
                            || state.entities.get(subject_id).is_none_or(|e| e.owner_id != plan.owner_id)
                            || candidate.intent != CandidateIntent::Assert
                        {
                            return Err("reviewed NEW requires a reason, existing owned entity and original Assert intent".into());
                        }
                        candidate.subject = CandidateSubject::Resolved(subject_id.clone());
                        bind_task(&mut candidate, task_id)?;
                        engine.reconciliation.0 = None;
                        let outcome = engine.ingest_or_stage(candidate.clone(), &state.entities)?;
                        let memory_id = match &outcome {
                            CandidateIngestOutcome::Ingested(IngestOutcome::Applied {
                                action: ReconciliationAction::New, memory_id: Some(id), target: None,
                            }) => *id,
                            _ => return Err(format!("recorded review may only create NEW memory: {outcome:?}").into()),
                        };
                        outcomes.push(serde_json::to_value(&outcome)?);
                        decisions.push(json!({"candidate_index":candidate_index,"action":"reviewed_new","reason":reason,
                            "resolved_candidate":candidate,"outcome":outcome,"memory_id":memory_id}));
                        applied_count += 1;
                    }
                    Decision::Reject { candidate_index, reason } => {
                        if reason.trim().is_empty() { return Err("rejection requires an explicit review reason".into()); }
                        decisions.push(json!({"candidate_index":candidate_index,"action":"rejected","reason":reason}));
                        rejected_count += 1;
                    }
                }
            }
            let source = state.sources.get_mut(&key).unwrap();
            source.enrichment = if outcomes.iter().any(|o| o.pointer("/Ingested/Applied").is_some()) { "committed" } else { "completed_no_facts" }.into();
            source.error = None;
            let result = source.result.as_mut().and_then(Value::as_object_mut).ok_or("source result must be an object")?;
            result.insert("outcomes".into(), json!(outcomes));
            result.insert("historical_review".into(), json!({"review_id":plan.review_id,"decisions":source_plan.decisions,
                "original_stale_candidates":original_candidates,"original_stale_reason":original_result.get("stale_reason"),"results":decisions}));
            if let Some(recording) = &source_plan.recorded_review {
                let marker = result.get_mut("historical_review").unwrap().as_object_mut().unwrap();
                marker.remove("original_stale_candidates");
                marker.remove("original_stale_reason");
                marker.insert("original_candidates".into(), original_candidates);
                marker.insert("original_failed_result".into(), original_result.clone());
                marker.insert("original_failed_error".into(), json!(original_source.error));
                marker.insert("recorded_review".into(), serde_json::to_value(recording)?);
            }
            result.remove("stale_candidates");
            result.remove("stale_reason");
            reports.push(json!({"source_id":source.id,"status":source.enrichment,"results":decisions}));
        }
        if recorded {
            for memory in before.repository.list_by_owner(&plan.owner_id)? {
                if engine.repository.get(memory.id)?.as_ref() != Some(&memory) {
                    return Err("recorded recovery changed an existing memory".into());
                }
            }
        }
        state.repository = engine.repository;
        state.staging = engine.staging;
        let mut report = json!({"review_id":plan.review_id,"replayed":false,"model_calls":0,
            "entities_added":new_entities,"covered_count":covered_count,"applied_count":applied_count,"sources":reports});
        if recorded { report["rejected_count"] = json!(rejected_count); }
        let key = source_key(state, &plan.sources[0].source_id)?;
        let marker = &mut state.sources.get_mut(&key).unwrap().result.as_mut().unwrap()["historical_review"];
        marker["plan"] = serde_json::to_value(plan)?;
        marker["result"] = report.clone();
        Ok(report)
    })
}

fn run() -> PersistenceResult<Value> {
    let mut args = std::env::args().skip(1);
    let (mut db, mut plan_path, mut before_path) = (None, None, None);
    while let Some(flag) = args.next() {
        let slot = match flag.as_str() {
            "--db" => &mut db,
            "--plan" => &mut plan_path,
            "--before-state" => &mut before_path,
            _ => return Err(format!("unknown argument: {flag}").into()),
        };
        if slot.is_some() {
            return Err(format!("duplicate argument: {flag}").into());
        }
        *slot = Some(
            args.next()
                .filter(|v| !v.starts_with("--"))
                .ok_or("argument value missing")?,
        );
    }
    let db = db.ok_or("--db is required")?;
    let plan: ReviewPlan =
        serde_json::from_str(&fs::read_to_string(plan_path.ok_or("--plan is required")?)?)?;
    let before: PersistedMemoryState = serde_json::from_str(&fs::read_to_string(
        before_path.ok_or("--before-state is required")?,
    )?)?;
    if !Path::new(&db).is_file() {
        return Err("--db must be an existing database".into());
    }
    let read_only = SqliteMemoryStore::open_read_only(&db, plan.owner_id.clone())?;
    if let Some(result) = replay(&read_only.read()?, &plan)? {
        return Ok(result);
    }
    drop(read_only);
    let mut store = SqliteMemoryStore::open(&db, plan.owner_id.clone())?;
    store.set_fact_index_enabled(true);
    apply(&mut store, &plan, &before)
}

fn main() -> PersistenceResult<()> {
    println!("{}", serde_json::to_string(&run()?)?);
    Ok(())
}

#[cfg(test)]
#[path = "../tests/common/mod.rs"]
mod common;

#[cfg(test)]
mod tests {
    use super::*;
    use hippocampus::{
        EntityType, Evidence, EvidenceId, MemoryValue, Polarity, Predicate, SessionId, Timestamp,
        TurnId, diagnostics,
    };

    #[test]
    fn recorded_failed_review_decodes_but_requires_independent_source_support() {
        struct TempDir(std::path::PathBuf);
        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let dir = TempDir(
            std::env::temp_dir().join(format!("recorded-review-{}", diagnostics::new_id())),
        );
        fs::create_dir(&dir.0).unwrap();
        let owner = UserId::from("fixture");
        let mut store =
            SqliteMemoryStore::open(dir.0.join("memory.sqlite3"), owner.clone()).unwrap();
        let entity = |id: &str| Entity {
            id: EntityId::from(id),
            owner_id: owner.clone(),
            entity_type: EntityType::Person,
            canonical_name: id.into(),
            aliases: vec![],
        };
        store
            .transaction(|state| {
                state.entities.upsert(entity("self"));
                state.entities.upsert(entity("character"));
                for (i, text) in ["new fact", "tools", "remove names", "unselected failure"]
                    .iter()
                    .enumerate()
                {
                    let turn = format!("turn-{i}");
                    state.sources.insert(
                        turn.clone(),
                        SourceRecord {
                            id: turn.clone(),
                            session_id: "session".into(),
                            turn_id: turn.clone(),
                            observed_at: 100,
                            received_sequence: i as u64 + 1,
                            request: Some(json!({"text":text,"history":[],"task_id":null,
                        "utc_offset_seconds":0,"capture_source":true})),
                            enrichment: "failed".into(),
                            error: Some("enrichment_failed".into()),
                            result: Some(json!({"outcomes":[],
                        "resume_attempts":[{"id":"recorded-attempt"}],"private_field":"preserve"})),
                            base_revision: 0,
                            index: Value::Null,
                            task: Default::default(),
                        },
                    );
                }
                state.with_engine(registry(), |engine, _| {
                    Ok(engine.ingest(common::grounded(CandidateMemory::new(
                        owner.clone(),
                        EntityId::from("self"),
                        MemoryType::Fact,
                        Predicate::from("fact"),
                        MemoryValue::from("tools"),
                        Timestamp(100),
                        Evidence::new(
                            EvidenceId::from("old-evidence"),
                            SessionId::from("old-session"),
                            TurnId::from("old-turn"),
                            "tools",
                        ),
                    )))?)
                })?;
                Ok(())
            })
            .unwrap();
        let before = store.read().unwrap();
        let before_json = serde_json::to_value(&before).unwrap();
        let context =
            OpenAiAdapterContext::new(owner.clone(), vec![entity("self")], Timestamp(100))
                .unwrap()
                .with_semantic_catalog(ChatConfig::default().predicates, vec![])
                .unwrap();
        let trace_path = dir.0.join("recording.jsonl");
        let mut events = Vec::new();
        let mut sources = Vec::new();
        for (i, text) in ["new fact", "tools", "remove names"].iter().enumerate() {
            let turn = format!("turn-{i}");
            let run_id = format!("{turn}:resume:recorded-attempt");
            let call_id = format!("call-{i}");
            let output = json!({"omission_reason":null,"candidates":[{"subject_id":"self","unresolved_subject":null,
                "memory_type":"fact","predicate":"fact","value":{"kind":"text","text":text},
                "polarity":null,"strength":null,"scope":{"domain":null,"conditions":{"companion_roles":[],
                "companion_entity_ids":[],"geography":null,"country":null,"city":null,"date":[],
                "time_of_day":null,"activities":[],"task_id":null}},"retention_tier":"long_term",
                "valid_from":null,"valid_to":null,"expires_at":null,"temporal_expression":null,
                "extraction_confidence":"high","certainty":"high","source":"user_statement",
                "intent":{"kind":"assert","target_memory_id":null,"reason":null},
                "explicit_memory_request":false,"cross_turn_useful":true,"tags":[]}]}).to_string();
            let start = json!({"kind":"model_start","run_id":run_id,"data":{"call_id":call_id,
                "stage":"personal_memory_candidates_verification_v1","request":{"model":"recorded-model",
                "input":json!({"owner_id":owner,"session_id":"session","turn_id":turn,"now":100,
                    "current_user_turn":text,"conversation_history":[],"caller_task_id":null,
                    "known_entities":[{"id":"self"}]}).to_string()}}});
            let end = json!({"kind":"model_end","run_id":run_id,"data":{"call_id":call_id,"http_status":200,
                "status":"completed","error":null,"parsing_status":"valid_json",
                "accounting":{"call_id":call_id,"operation_id":run_id,"stage":"extraction_review"},
                "response":{"id":format!("response-{i}"),"model":"actual-recorded-model","status":"completed",
                "error":null,"output":[{"type":"message","content":[{"type":"output_text","text":output}]}]}}});
            let validation = json!({"kind":"validation","run_id":run_id,
                "data":{"stage":"extraction_review","status":"failed","error":"RawScopeConditions"}});
            events.extend([start.clone(), end.clone(), validation.clone()]);
            sources.push(SourceReview {
                source_id: turn.clone(),
                recorded_review: Some(RecordedReview {
                    recording_path: trace_path.to_string_lossy().into(),
                    recording_sha256: String::new(),
                    model_start_line: i * 3 + 1,
                    model_end_line: i * 3 + 2,
                    validation_line: i * 3 + 3,
                    model_start: start,
                    model_end: end,
                    validation,
                    output_text: output.clone(),
                    normalizations: json!([]),
                    parsed_candidates: context
                        .decode_extraction(
                            &output,
                            text,
                            &SessionId::from("session"),
                            &TurnId::from(turn.as_str()),
                        )
                        .unwrap(),
                }),
                decisions: vec![if i == 2 {
                    Decision::Reject {
                        candidate_index: 0,
                        reason: "one-turn editing control".into(),
                    }
                } else {
                    Decision::ReviewedNew {
                        candidate_index: 0,
                        subject_id: EntityId::from(if i == 1 { "character" } else { "self" }),
                        task_id: (i == 1).then(|| "character-task".into()),
                        reason: "explicitly reviewed original context".into(),
                    }
                }],
            });
        }
        fs::write(
            &trace_path,
            events
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
        let digest = Command::new("shasum")
            .args(["-a", "256"])
            .arg(&trace_path)
            .output()
            .unwrap();
        assert!(digest.status.success());
        let digest = String::from_utf8(digest.stdout)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .to_owned();
        for source in &mut sources {
            source.recorded_review.as_mut().unwrap().recording_sha256 = digest.clone();
        }
        let plan = ReviewPlan {
            review_id: "recorded-review".into(),
            owner_id: owner.clone(),
            entities: vec![],
            sources,
        };

        for source_plan in &plan.sources {
            let recording = source_plan.recorded_review.as_ref().unwrap();
            let source = &before.sources[&source_plan.source_id];
            assert_eq!(
                recorded_candidates(recording, source, &before, &owner).unwrap(),
                recording.parsed_candidates
            );
        }

        let mut forged = plan.clone();
        forged.sources[0]
            .recorded_review
            .as_mut()
            .unwrap()
            .parsed_candidates[0]
            .value = MemoryValue::from("forged");
        assert!(apply(&mut store, &forged, &before).is_err());
        assert_eq!(
            serde_json::to_value(store.read().unwrap()).unwrap(),
            before_json
        );
        let mut wrong_recording = plan.clone();
        wrong_recording.sources[0]
            .recorded_review
            .as_mut()
            .unwrap()
            .model_end["data"]["status"] = json!("incomplete");
        assert!(apply(&mut store, &wrong_recording, &before).is_err());
        // Exercise the collision policy directly: apply() now stops at source review first.
        let existing = before.repository.list_by_owner(&owner).unwrap();
        for (index, action, target) in [
            (0, ReconciliationAction::New, None),
            (1, ReconciliationAction::Reinforce, Some(existing[0].id)),
        ] {
            let candidate = &plan.sources[index]
                .recorded_review
                .as_ref()
                .unwrap()
                .parsed_candidates[0];
            let decision =
                ReviewedCorrection(None).decide(candidate, &existing, Cardinality::Multiple);
            assert_eq!((decision.action, decision.target), (action, target));
        }
        assert_eq!(
            serde_json::to_value(store.read().unwrap()).unwrap(),
            before_json
        );
        let error = apply(&mut store, &plan, &before).unwrap_err().to_string();
        assert!(
            error.contains("candidate has no valid source support"),
            "{error}"
        );
        assert_eq!(
            serde_json::to_value(store.read().unwrap()).unwrap(),
            before_json
        );
        assert!(replay(&store.read().unwrap(), &plan).unwrap().is_none());
    }

    #[test]
    fn historical_binding_requires_new_source_support_and_coverage_replays() {
        struct TempDb(std::path::PathBuf);
        impl Drop for TempDb {
            fn drop(&mut self) {
                let _ = fs::remove_file(&self.0);
            }
        }
        let db = TempDb(std::env::temp_dir().join(format!(
            "historical-review-{}.sqlite3",
            diagnostics::new_id()
        )));
        let owner = UserId::from("fixture");
        let mut store = SqliteMemoryStore::open(&db.0, owner.clone()).unwrap();
        store.set_fact_index_enabled(true);
        let candidate = |turn: &str, value: &str, quote: &str| {
            CandidateMemory::new(
                owner.clone(),
                EntityId::from("self"),
                MemoryType::Fact,
                Predicate::from("fact"),
                MemoryValue::from(value),
                Timestamp(100),
                Evidence::new(
                    EvidenceId(format!("{turn}:{value}")),
                    SessionId::from("session"),
                    TurnId::from(turn),
                    quote,
                ),
            )
        };
        let covered = candidate("covered", "Blue.", "Blue. That remains correct.");
        let bound: Vec<_> = ["the project", "the character", "service"]
            .iter()
            .map(|reference| {
                let mut c = candidate(
                    "bind",
                    reference,
                    "The project, character and service are private.",
                );
                c.subject = CandidateSubject::Unresolved((*reference).into());
                c.memory_type = MemoryType::Constraint;
                c.predicate = Predicate::from("response_style");
                c.polarity = Some(Polarity::Positive);
                c
            })
            .collect();
        let mut corrected = candidate(
            "correct",
            "I meant to say silver, not red.",
            "I meant to say silver, not red.",
        );
        corrected.subject = CandidateSubject::Unresolved("the car".into());
        let (covered_id, corrected_id) = store.transaction(|state| {
            state.entities.upsert(Entity { id: EntityId::from("self"), owner_id: owner.clone(),
                entity_type: EntityType::Person, canonical_name: "self".into(), aliases: vec![] });
            for (sequence, turn, raw, stale) in [
                (1, "original", "Blue. Red.", vec![]),
                (2, "covered", "Blue. That remains correct.", vec![covered.clone()]),
                (3, "bind", "The project, character and service are private.", bound.clone()),
                (4, "correct", "I meant to say silver, not red.", vec![corrected.clone()]),
            ] {
                state.sources.insert(turn.into(), SourceRecord {
                    id: turn.into(), session_id: "session".into(), turn_id: turn.into(),
                    observed_at: 100, received_sequence: sequence, request: Some(json!({"text":raw,"capture_source":true})),
                    enrichment: if stale.is_empty() { "committed" } else { "stale_requires_review" }.into(),
                    error: None, result: Some(json!({"outcomes":[],"resume_attempts":[],
                        "stale_candidates":stale,"stale_reason":"historical_retry_cannot_update_existing_slot"})),
                    base_revision: 0, index: Value::Null, task: Default::default(),
                });
            }
            state.with_engine(registry(), |engine, _| {
                let mut ids = Vec::new();
                for value in ["Blue.", "Red."] {
                    match engine.ingest(common::grounded(candidate("original", value, "Blue. Red.")))? {
                        IngestOutcome::Applied { memory_id: Some(id), .. } => ids.push(id),
                        other => return Err(format!("fixture ingest: {other:?}").into()),
                    }
                }
                Ok((ids[0], ids[1]))
            })
        }).unwrap();
        let before = store.read().unwrap();
        let before_json = serde_json::to_value(&before).unwrap();
        let entity = |id: &str, alias: &str| Entity {
            id: EntityId::from(id),
            owner_id: owner.clone(),
            entity_type: EntityType::Other(id.into()),
            canonical_name: id.into(),
            aliases: vec![alias.into()],
        };
        let plan = ReviewPlan {
            review_id: "review-fixture".into(),
            owner_id: owner.clone(),
            entities: vec![
                entity("project", "the project"),
                entity("character", "the character"),
                entity("service", "expanded service name"),
                entity("car", "the car"),
            ],
            sources: vec![
                SourceReview {
                    source_id: "covered".into(),
                    recorded_review: None,
                    decisions: vec![Decision::Covered {
                        candidate_index: 0,
                        target: covered_id,
                    }],
                },
                SourceReview {
                    source_id: "bind".into(),
                    recorded_review: None,
                    decisions: ["project", "character", "service"]
                        .iter()
                        .enumerate()
                        .map(|(candidate_index, id)| Decision::Bind {
                            candidate_index,
                            subject_id: EntityId::from(*id),
                            task_id: Some(format!("{id}-task")),
                            corrects: None,
                        })
                        .collect(),
                },
                SourceReview {
                    source_id: "correct".into(),
                    recorded_review: None,
                    decisions: vec![Decision::Bind {
                        candidate_index: 0,
                        subject_id: EntityId::from("car"),
                        task_id: None,
                        corrects: Some(corrected_id),
                    }],
                },
            ],
        };
        let mut wrong_before: PersistedMemoryState =
            serde_json::from_value(before_json.clone()).unwrap();
        wrong_before.revision += 1;
        assert!(apply(&mut store, &plan, &wrong_before).is_err());
        assert_eq!(
            serde_json::to_value(store.read().unwrap()).unwrap(),
            before_json
        );
        // Check the target lookup itself; the earlier unverified Bind cannot mask this error.
        let mut bound_correction = corrected.clone();
        bound_correction.subject = CandidateSubject::Resolved(EntityId::from("car"));
        let source = &before.sources["correct"];
        assert_eq!(
            correction_target(&before, source, &bound_correction, corrected_id)
                .unwrap()
                .id,
            corrected_id
        );
        assert_eq!(
            correction_target(&before, source, &bound_correction, MemoryId(999))
                .unwrap_err()
                .to_string(),
            "correction target missing"
        );
        assert_eq!(
            serde_json::to_value(store.read().unwrap()).unwrap(),
            before_json
        );
        let error = apply(&mut store, &plan, &before).unwrap_err().to_string();
        assert!(
            error.contains("candidate has no valid source support"),
            "{error}"
        );
        assert_eq!(
            serde_json::to_value(store.read().unwrap()).unwrap(),
            before_json
        );
        assert!(replay(&store.read().unwrap(), &plan).unwrap().is_none());

        // A separate fixture branch with only a covered assertion remains replayable.
        store
            .transaction(|state| {
                for id in ["bind", "correct"] {
                    state.sources.get_mut(id).unwrap().enrichment = "failed".into();
                }
                Ok(())
            })
            .unwrap();
        let before_covered = store.read().unwrap();
        let before_covered_json = serde_json::to_value(&before_covered).unwrap();
        let mut covered_only = plan.clone();
        covered_only.entities.clear();
        covered_only.sources.truncate(1);
        let report = apply(&mut store, &covered_only, &before_covered).unwrap();
        assert_eq!(report["covered_count"], 1);
        assert_eq!(report["applied_count"], 0);
        assert_eq!(report["model_calls"], 0);
        let after = store.read().unwrap();
        assert_eq!(
            after.repository.list_by_owner(&owner).unwrap(),
            before.repository.list_by_owner(&owner).unwrap()
        );
        assert_eq!(after.sources["covered"].enrichment, "completed_no_facts");
        for turn in ["original", "bind", "correct"] {
            assert_eq!(
                serde_json::to_value(&after.sources[turn]).unwrap(),
                before_covered_json["sources"][turn]
            );
        }
        let after_json = serde_json::to_value(&after).unwrap();
        assert_eq!(
            apply(&mut store, &covered_only, &before_covered).unwrap()["replayed"],
            true
        );
        let mut changed = covered_only.clone();
        if let Decision::Covered { target, .. } = &mut changed.sources[0].decisions[0] {
            *target = corrected_id;
        }
        assert!(apply(&mut store, &changed, &before_covered).is_err());
        assert_eq!(
            serde_json::to_value(store.read().unwrap()).unwrap(),
            after_json
        );
        assert_eq!(before_json["receipts"], after_json["receipts"]);
    }
}
