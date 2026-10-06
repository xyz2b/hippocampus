//! Explicit source controls; identity semantics remain W3's responsibility.
use super::*;
use crate::contract::{EntitySubject, EvidenceRef, SourceRef, Subject, W1Output};
use std::collections::BTreeSet;

#[derive(Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[schemars(crate = "rmcp::schemars")]
enum Mode {
    ConfirmSubject,
    Reextract,
}

#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(crate = "rmcp::schemars")]
pub(super) struct Reprocess {
    source_id: String,
    request_id: String,
    expected_state_token: String,
    mode: Mode,
    #[serde(default)]
    enqueue: bool,
    candidate_index: Option<usize>,
    entity_id: Option<String>,
    #[schemars(schema_with = "confirmation_schema")]
    confirmation: Option<EvidenceRef>,
}

fn confirmation_schema(_: &mut rmcp::schemars::SchemaGenerator) -> rmcp::schemars::Schema {
    rmcp::schemars::json_schema!({"anyOf":[{"type":"null"},{
        "type":"object","additionalProperties":false,"required":["source","quote"],
        "properties":{
            "source":{"type":"object","additionalProperties":false,"required":["kind","source_id"],
                "properties":{"kind":{"const":"stored"},"source_id":{"type":"string"}}},
            "quote":{"type":"object","additionalProperties":false,"required":["text"],
                "properties":{"text":{"type":"string"}}}
        }
    }]})
}

/// Includes no-product actions such as retractions. An accepted transaction is irreversible history.
fn committed_indices(state: &PersistedMemoryState, key: &str) -> BTreeSet<usize> {
    state
        .w5_receipts
        .iter()
        .filter(|(id, r)| id.starts_with(&format!("{key}#")) && r.status == W5Status::Committed)
        .flat_map(|(_, r)| {
            r.decisions
                .iter()
                .map(|d| d.candidate)
                .chain(r.products.keys().copied())
        })
        .collect()
}
fn applied(result: &Value) -> bool {
    result["outcomes"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(result.get("outcome"))
        .any(|row| row["Ingested"]["Applied"].is_object())
}
fn legacy_committed(
    state: &PersistedMemoryState,
    owner: &UserId,
    source: &SourceRecord,
) -> PersistenceResult<bool> {
    let staged: BTreeSet<_> = source
        .result
        .as_ref()
        .into_iter()
        .flat_map(|result| {
            result["outcomes"]
                .as_array()
                .into_iter()
                .flatten()
                .chain(result.get("outcome"))
        })
        .filter_map(|row| row["Pending"]["staged_id"].as_u64())
        .collect();
    Ok(source.result.as_ref().is_some_and(applied)
        || state.receipts.values().any(|receipt| {
            ((receipt.request["session_id"] == source.session_id
                && receipt.request["turn_id"] == source.turn_id)
                || receipt.request["staged_id"]
                    .as_u64()
                    .is_some_and(|id| staged.contains(&id)))
                && applied(&receipt.result)
        })
        || state.repository.list_by_owner(owner)?.iter().any(|memory| {
            memory.evidence.iter().any(|evidence| {
                evidence.session_id.0 == source.session_id && evidence.turn_id.0 == source.turn_id
            })
        }))
}

impl ChatMemoryService {
    pub(super) fn reprocess_source(&mut self, input: Reprocess) -> PersistenceResult<Value> {
        for (name, text) in [
            ("source_id", &input.source_id),
            ("request_id", &input.request_id),
            ("expected_state_token", &input.expected_state_token),
        ] {
            bounded_text(text, name, 512)?;
        }
        let owner = self.store.owner().clone();
        let config = self.config.clone();
        let args = serde_json::to_value(&input)?;
        let digest = crate::recall_models::fingerprint(&args);
        let receipt_key = json!([
            "source_reprocess",
            crate::recall_models::fingerprint(&json!(input.request_id))
        ])
        .to_string();
        // Replay is authorized by its original accepted command; never re-enqueue or spend again.
        let state = self.store.read()?;
        let (key, source) = state
            .sources
            .iter()
            .find(|(_, s)| s.id == input.source_id)
            .ok_or("source_not_found")?;
        if source.request.is_none() {
            return Err("source_deleted".into());
        }
        if let Some(r) = state.receipts.get(&receipt_key) {
            if r.request["digest"] != digest {
                return Err("request_id_conflict".into());
            }
            if r.result["error"] == "source_changed" {
                return Err("source_changed".into());
            }
            let mut out = r.result.clone();
            out["replayed"] = json!(true);
            return Ok(out);
        }
        if input.enqueue
            && (config.enrichment_mode == EnrichmentMode::Off
                || config.model_budget.is_none()
                || !self
                    .model_client(&receipt_key)
                    .is_ok_and(|c| c.network_allowed()))
        {
            return Err("operation_not_authorized".into());
        }
        let key = key.clone();
        let out = self.store.transaction(|state| {
            let source = state.sources.get(&key).ok_or("source_not_found")?;
            if source.request.is_none() { return Err("source_deleted".into()); }
            if let Some(r) = state.receipts.get(&receipt_key) {
                if r.request["digest"] != digest { return Err("request_id_conflict".into()); }
                if r.result["error"] == "source_changed" { return Err("source_changed".into()); }
                let mut out = r.result.clone(); out["replayed"] = json!(true); return Ok(out);
            }
            if source.task.attempt_id.is_some() && source.task.lease_until.is_some_and(|t| t >= unix_seconds().unwrap_or(0)) {
                return Err("source_busy".into());
            }
            if sources::status(state, &key, &owner, config.utc_offset_seconds)?["state_token"] != input.expected_state_token {
                return Err("source_changed".into());
            }
            let committed = committed_indices(state, &key);
            let legacy_applied = source.task.processing_version.as_deref() != Some(W4_PROCESSING_VERSION)
                && legacy_committed(state, &owner, source)?;
            let (seed, confirmation) = match input.mode {
                Mode::Reextract => {
                    if input.candidate_index.is_some() || input.entity_id.is_some() || input.confirmation.is_some() {
                        return Err("invalid_confirmation".into());
                    }
                    if !committed.is_empty() || legacy_applied {
                        return Err("rebuild_requires_uncommitted_source".into());
                    }
                    (None, None)
                }
                Mode::ConfirmSubject => {
                    let index = input.candidate_index.ok_or("invalid_confirmation")?;
                    if committed.contains(&index) || legacy_applied { return Err("invalid_confirmation".into()); }
                    let entity_id = EntityId::from(input.entity_id.as_deref().ok_or("invalid_confirmation")?);
                    if state.entities.get(&entity_id).is_none_or(|e| e.owner_id != owner) { return Err("invalid_confirmation".into()); }
                    let evidence = input.confirmation.clone().ok_or("invalid_confirmation")?;
                    let SourceRef::Stored {source_id} = &evidence.source else { return Err("invalid_confirmation".into()); };
                    let confirming = state.sources.values().find(|s| &s.id == source_id && s.request.is_some()).ok_or("invalid_confirmation")?;
                    if evidence.quote.text.trim().is_empty() || evidence.quote.text.chars().count() > 2000
                        || confirming.request.as_ref().unwrap()["capture_source"] != true
                        || crate::lexical::body(confirming).is_none_or(|text| !text.contains(&evidence.quote.text))
                        || (crate::lexical::task(confirming).is_some() && crate::lexical::task(confirming) != crate::lexical::task(source)) {
                        return Err("invalid_confirmation".into());
                    }
                    let saved = crate::enrichment::stored_batch(source)?.ok_or("invalid_confirmation")?;
                    let mut candidates: Vec<_> = saved.batch.w2.candidates.iter().map(|c| c.candidate.clone()).collect();
                    let candidate = candidates.get_mut(index).ok_or("invalid_confirmation")?;
                    let original_subject = source.management.confirmations.iter().find(|c| c.candidate_index == index).map_or_else(|| candidate.subject.clone(), |c| c.original_subject.clone());
                    candidate.subject = Subject::Entity(EntitySubject { entity_id:entity_id.0.clone() });
                    let confirmation = SubjectConfirmation {generation:source.task.generation.checked_add(1).ok_or("generation overflow")?,
                        candidate_index:index, candidate_identity:crate::recall_models::fingerprint(&json!(candidate)),
                        original_subject, entity_id, evidence, evidence_binding:json!({"request":confirming.request,"observed_at":confirming.observed_at})};
                    let entities: Vec<_> = state.entities.list_by_owner(&owner).iter().map(|e| e.id.clone()).collect();
                    let relations = crate::w2::relation_catalog(&config.predicates, &config.memory_types)?;
                    let batch = crate::w2::validate(W1Output { candidates, memory_requests:saved.batch.w2.memory_requests.iter().map(|r| r.request.clone()).collect() },
                        &crate::w2::Context {text:crate::lexical::body(source).ok_or("source_deleted")?, entities:&entities,
                            relations:&relations, said_on:&saved.batch.w2.said_on, utc_offset_seconds:saved.batch.w2.utc_offset_seconds,
                            task_id:saved.batch.w2.task_id.as_ref()})?;
                    (Some(batch), Some(confirmation))
                }
            };
            let source = state.sources.get_mut(&key).unwrap();
            source.management.audits.push(json!({"generation":source.task.generation,"round":source.task.round,
                "task":source.task,"selection":source.management.selection,
                "result":source.result,"confirmations":source.management.confirmations,
                "control":{"request_id":input.request_id,"mode":input.mode,"enqueue":input.enqueue}}));
            match confirmation {
                Some(c) => { source.management.confirmations.retain(|prior| prior.candidate_index != c.candidate_index); source.management.confirmations.push(c); }
                None => source.management.confirmations.clear(),
            }
            crate::commit::begin_source_generation(state, &key, &receipt_key, seed, input.candidate_index, input.enqueue)?;
            crate::recall_models::invalidate_deleted(state, &BTreeSet::from([input.source_id.as_str()]));
            let mut out = sources::status(state, &key, &owner, config.utc_offset_seconds)?;
            out["enqueued"] = json!(input.enqueue); out["replayed"] = json!(false);
            state.receipts.insert(receipt_key.clone(), crate::sqlite::TurnReceipt {request:json!({"digest":digest,"source_id":input.source_id}), result:out.clone()});
            Ok(out)
        })?;
        if out["replayed"] != true {
            self.recall_runtime.invalidate_owner(&self.store);
            if input.enqueue
                && let Some(wake) = &self.background_wake
            {
                let _ = wake.try_send(());
            }
        }
        Ok(out)
    }
}
