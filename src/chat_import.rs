//! Copy one retained USER history entry; never derive text or utterance time.
use super::*;

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(crate = "rmcp::schemars")]
pub(super) struct ImportHistory {
    source_id: String,
    history_index: usize,
    expected_content_fingerprint: String,
    request_id: String,
    capture_source: bool,
}

impl ChatMemoryService {
    pub(super) fn import_history(&mut self, input: ImportHistory) -> PersistenceResult<Value> {
        for (name, text) in [
            ("source_id", &input.source_id),
            ("request_id", &input.request_id),
            (
                "expected_content_fingerprint",
                &input.expected_content_fingerprint,
            ),
        ] {
            bounded_text(text, name, 512)?;
        }
        if !input.capture_source || !self.config.source_capture_enabled {
            return Err("operation_not_authorized".into());
        }
        let owner = self.store.owner().clone();
        let offset = self.config.utc_offset_seconds;
        let digest = crate::recall_models::fingerprint(&serde_json::to_value(&input)?);
        let receipt_key = json!([
            "source_import_history",
            crate::recall_models::fingerprint(&json!(input.request_id))
        ])
        .to_string();
        let now = unix_seconds()?;
        let out = self.store.transaction(|state| {
            let parent = state.sources.values().find(|s| s.id == input.source_id)
                .ok_or("source_not_found")?;
            let request = parent.request.as_ref().ok_or("source_deleted")?;
            if let Some(receipt) = state.receipts.get(&receipt_key) {
                if receipt.request["digest"] != digest { return Err("request_id_conflict".into()); }
                let id = receipt.result["source"]["source_id"].as_str().ok_or("source_deleted")?;
                if state.sources.values().find(|s| s.id == id).is_none_or(|s| s.request.is_none()) {
                    return Err("source_deleted".into());
                }
                let mut out = receipt.result.clone(); out["replayed"] = json!(true);
                return Ok(out);
            }
            if request["capture_source"] != true { return Err("operation_not_authorized".into()); }
            // Content identity is independent of the mutable history slot, including tombstones.
            let prior = state.sources.iter().find(|(_, s)| s.management.provenance.as_ref().is_some_and(|p|
                p["kind"] == "history_import" && p["parent_source_id"] == input.source_id
                    && p["content_fingerprint"] == input.expected_content_fingerprint));
            if prior.is_some_and(|(_, s)| s.request.is_none()) { return Err("source_deleted".into()); }
            let text = request["history"].get(input.history_index).and_then(Value::as_str)
                .ok_or("history_entry_not_found")?;
            if crate::recall_models::fingerprint(&json!(text)) != input.expected_content_fingerprint {
                return Err("history_content_changed".into());
            }
            if text.trim().is_empty() { return Err("history_entry_not_found".into()); }
            let (key, replayed) = if let Some((key, _)) = prior {
                (key.clone(), true)
            } else {
                let identity = crate::recall_models::fingerprint(&json!([
                    input.source_id, input.expected_content_fingerprint]));
                let key = json!(["history_import", identity]).to_string();
                let session_id = parent.session_id.clone();
                let turn_id = format!("history-import:{identity}");
                let task_id = crate::lexical::task(parent).map(str::to_owned);
                let sequence = state.next_source_sequence()?;
                let source = SourceRecord {
                    id: format!("history:{identity}"), session_id:session_id.clone(), turn_id:turn_id.clone(),
                    observed_at:now, received_sequence:sequence, base_revision:state.revision,
                    request:Some(json!({"text":text,"history":[],"session_id":session_id,
                        "turn_id":turn_id,"capture_source":true,"task_id":task_id,
                        "source_origin":"history_import"})),
                    enrichment:"not_requested".into(), error:None,
                    result:Some(json!({"enrichment_mode":"off","outcomes":[]})), index:Value::Null,
                    task:EnrichmentTask {processing_version:Some(W4_PROCESSING_VERSION.into()),
                        accepted_mode:Some("off".into()), ..Default::default()},
                    management:SourceManagement {
                        provenance:Some(json!({"kind":"history_import","parent_source_id":input.source_id,
                            "history_index":input.history_index,"content_fingerprint":input.expected_content_fingerprint,
                            "before_source_id":input.source_id,
                            "authorization":{"capture_source":true,"request_id":input.request_id}})),
                        time_anchor:Some(json!({"observed_at":now,"said_on":null,"utc_offset_seconds":offset})),
                        ..Default::default()
                    },
                };
                state.sources.insert(key.clone(), source);
                (key, false)
            };
            let mut out = sources::status(state, &key, &owner, offset)?;
            out["replayed"] = json!(replayed); out["enqueued"] = json!(false);
            state.receipts.insert(receipt_key.clone(), crate::sqlite::TurnReceipt {
                request:json!({"digest":digest,"source_id":out["source"]["source_id"],
                    "parent_source_id":input.source_id}), result:out.clone()
            });
            Ok(out)
        })?;
        // Newly captured sources are visible in the next R3 snapshot; ordinary in-flight reads stay valid.
        Ok(out)
    }
}
