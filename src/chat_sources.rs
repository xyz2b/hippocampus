//! Source management reads reuse the same W2/W3/W5 receipts as prepare/recall.
use super::*;

fn candidate_summary(candidate: &Value) -> Value {
    json!({"index":candidate["index"], "status":candidate["status"], "reasons":candidate["reasons"]})
}

fn summary(state: &PersistedMemoryState, key: &str) -> PersistenceResult<Value> {
    let receipt = crate::enrichment::receipt_from_state(state, key)?;
    let candidates = receipt["enrichment"]["candidates"]
        .as_array()
        .into_iter()
        .flatten();
    let uncommitted: Vec<_> = candidates
        .clone()
        .filter(|c| c["status"] != "committed")
        .map(candidate_summary)
        .collect();
    let pending_updates: Vec<_> = candidates
        .filter(|c| c["status"] == "committed" && c["update"]["status"] == "pending")
        .map(candidate_summary)
        .collect();
    Ok(json!({
        "source_id":state.sources[key].id,
        "generation":state.sources[key].task.generation,
        "status":receipt["source"]["status"],
        "enrichment":{"status":receipt["enrichment"]["status"], "error":state.sources[key].error},
        "uncommitted_candidates":uncommitted,
        "pending_update_candidates":pending_updates
    }))
}

pub(super) fn status(
    state: &PersistedMemoryState,
    key: &str,
    owner: &UserId,
    utc_offset_seconds: i32,
) -> PersistenceResult<Value> {
    let source = &state.sources[key];
    let mut result = summary(state, key)?;
    result.as_object_mut().unwrap().remove("source_id");
    result.as_object_mut().unwrap().remove("status");
    result["source"] = source.status()["source"].clone();
    // Older snapshots default to ordinary provenance. Never infer history times from text.
    result["generation"] = json!(source.task.generation);
    result["round"] = json!(source.task.round);
    result["provenance"] = json!({
        "kind":source.status()["source"]["provenance"],
        "session_id":source.session_id, "turn_id":source.turn_id,
        "parent_source_id":null, "history_index":null
    });
    if let Some(provenance) = &source.management.provenance {
        result["provenance"] = provenance.clone();
    }
    let offset = time::UtcOffset::from_whole_seconds(utc_offset_seconds)?;
    let said_on = time::OffsetDateTime::from_unix_timestamp(source.observed_at)
        .ok()
        .map(|at| at.to_offset(offset).date().to_string());
    result["time_anchor"] = json!({
        "observed_at":source.observed_at, "said_on":said_on,
        "utc_offset_seconds":utc_offset_seconds
    });
    if let Some(anchor) = &source.management.time_anchor {
        result["time_anchor"] = anchor.clone();
    }
    result["subject_confirmations"] = json!(source.management.confirmations.iter().map(|c| json!({
        "generation":c.generation,"candidate_index":c.candidate_index,"candidate_identity":c.candidate_identity,
        "entity_id":c.entity_id,"source":c.evidence.source,
        "quote_hash":crate::recall_models::fingerprint(&json!(c.evidence.quote))
    })).collect::<Vec<_>>());
    result["audit_generations"] = json!(
        source
            .management
            .audits
            .iter()
            .map(|a| a["generation"].clone())
            .collect::<Vec<_>>()
    );
    // A committed group may precede the source summary. Include its derived receipt too;
    // unrelated source/global revision changes must not invalidate this token.
    let mut binding = serde_json::to_value(source)?;
    // The FTS projection stamps every source with the owner's global source_revision.
    // That derived index stamp is not a change to this source or its processing state.
    binding.as_object_mut().unwrap().remove("index");
    result["state_token"] = json!(crate::recall_models::fingerprint(&json!({
        "owner":owner, "source":binding, "utc_offset_seconds":utc_offset_seconds,
        "receipt":crate::enrichment::receipt_from_state(state, key)?
    })));
    result["model_calls"] = json!([]);
    result["history_entries"] = json!(source.request.as_ref().and_then(|r| r["history"].as_array())
        .into_iter().flatten().enumerate().filter_map(|(index, value)| value.as_str().map(|text|
            json!({"history_index":index,"content_fingerprint":crate::recall_models::fingerprint(&json!(text)),
                "bytes":text.len()}))).collect::<Vec<_>>());
    Ok(result)
}

pub(super) fn pending(state: &PersistedMemoryState) -> PersistenceResult<Vec<Value>> {
    let mut results = Vec::new();
    for (key, source) in &state.sources {
        if source.request.is_none() {
            continue;
        }
        let item = summary(state, key)?;
        if !matches!(
            source.enrichment.as_str(),
            "committed" | "completed_no_facts" | "completed_rejected"
        ) || !item["uncommitted_candidates"]
            .as_array()
            .unwrap()
            .is_empty()
            || !item["pending_update_candidates"]
                .as_array()
                .unwrap()
                .is_empty()
        {
            results.push(item);
        }
    }
    Ok(results)
}
