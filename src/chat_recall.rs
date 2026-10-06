//! recall.v3 宿主分发；旧契约分支保持原样，不读取评测夹具。
use super::*;
use crate::contract::{
    Polarity, Qualification, RecallOutput, RecallRequest, RecallStatus, SemanticJudgment, SourceRef,
};
use crate::openai::recall_models::RecallModelProvider;

impl ChatMemoryService {
    pub(super) fn uses_recall_runtime(
        &self,
        name: &str,
        arguments: &Value,
    ) -> PersistenceResult<bool> {
        if !matches!(
            name,
            "memory_recall" | "memory_prepare" | "memory_validate_answer"
        ) {
            return Ok(false);
        }
        if let Some(version) = arguments.get("recall_version").filter(|v| !v.is_null()) {
            if serde_json::from_value::<RecallVersion>(version.clone())
                .map_err(|_| "contract_version_unsupported")?
                == RecallVersion::Legacy
            {
                return Err("contract_version_unsupported".into());
            }
        }
        let selected = match name {
            "memory_recall" => serde_json::from_value::<Recall>(arguments.clone())?.recall_version,
            "memory_prepare" => {
                serde_json::from_value::<Prepare>(arguments.clone())?.recall_version
            }
            _ => {
                serde_json::from_value::<VersionedValidateAnswer>(arguments.clone())?.recall_version
            }
        };
        Ok(match selected {
            Some(RecallVersion::V3) => true,
            Some(RecallVersion::Legacy) => self.config.recall_pipeline == RecallPipeline::RecallV3,
            None => self.config.recall_pipeline == RecallPipeline::RecallV3,
        })
    }

    fn recall_provider(
        &self,
        operation: &str,
        offline: bool,
    ) -> PersistenceResult<Option<RecallModelProvider>> {
        if offline {
            return Ok(None);
        }
        #[cfg(test)]
        if let Some(provider) = &self.recall_provider {
            return Ok(Some(provider.clone()));
        }
        // 缺凭据与关闭网络均使用已有完全离线边界；缺预算由运行时明确降级。
        let client = match self.model_client(operation) {
            Ok(c) => c,
            Err(e)
                if e.downcast_ref::<OpenAiAdapterError>().is_some_and(|e| {
                    matches!(
                        e,
                        OpenAiAdapterError::MissingBaseUrl | OpenAiAdapterError::MissingApiKey
                    )
                }) =>
            {
                return Ok(None);
            }
            Err(e) => return Err(e),
        };
        if !client.network_allowed() {
            return Ok(None);
        }
        let context = self.config.context(
            self.store.owner().clone(),
            self.store
                .read()?
                .entities
                .list_by_owner(self.store.owner()),
            None,
            default_limit(),
        )?;
        Ok(Some(
            RecallModelProvider::new(client, context)
                .with_models(self.config.resolved_recall_models()?)?,
        ))
    }

    fn recall_id(id: Option<String>) -> PersistenceResult<String> {
        let id = id.ok_or("recall.v3 requires request_id")?;
        bounded_text(&id, "request_id", 512)?;
        Ok(id)
    }

    fn recall_at(&self, id: &str, at: Option<i64>) -> PersistenceResult<i64> {
        past_time(at, "at")?;
        let key = json!([
            "recall.v3",
            crate::openai::recall_models::fingerprint(&json!(id))
        ])
        .to_string();
        let first = self
            .store
            .read()?
            .receipts
            .get(&key)
            .and_then(|r| r.request["at"].as_i64());
        Ok(at.or(first).unwrap_or(unix_seconds()?))
    }

    fn recall_v3(
        &mut self,
        input: Recall,
        operation: &str,
        cancelled: &impl Fn() -> bool,
    ) -> PersistenceResult<Value> {
        validate_query_input(&input.query, &input.context, input.task_id.as_deref())?;
        check_limit(input.limit)?;
        if input.bootstrap && input.task_id.is_some() {
            return Err("bootstrap loads global requirements; omit task_id".into());
        }
        let id = Self::recall_id(input.request_id)?;
        let implicit_at = input.at.is_none();
        let at = self.recall_at(&id, input.at)?;
        let local = time::OffsetDateTime::from_unix_timestamp(at)?
            .to_offset(time::UtcOffset::from_whole_seconds(
                self.config.utc_offset_seconds,
            )?)
            .date();
        let legacy_output = input.recall_version == Some(RecallVersion::Legacy);
        let request = crate::recall::Request {
            binding: RecallRequest {
                request_id: id,
                query: input.query,
                context: input.context,
                task_id: input.task_id,
                at,
            },
            owner: self.store.owner().clone(),
            subject_ids: None,
            said_on: local.to_string(),
            utc_offset_seconds: self.config.utc_offset_seconds,
            bootstrap: input.bootstrap,
        };
        if input.bootstrap {
            // 启动召回只读取既有全局规则；不把普通个人事实或未整理正文送到宿主。
            let before = self.store.read()?;
            let global = self.global_context_from_state(&before)?;
            let memories = global
                .mandatory
                .iter()
                .chain(&global.constraints)
                .chain(&global.preferences)
                .chain(&global.facts)
                .chain(&global.episodes)
                .collect::<Vec<_>>();
            let records = memories
                .iter()
                .take(input.limit)
                .filter_map(|m| crate::recall::projection(&before, m.id))
                .collect::<Vec<_>>();
            let output = RecallOutput {
                version: "recall.v3".into(),
                request: request.binding.clone(),
                status: RecallStatus::ContextOnly,
                current_message: None,
                records,
                raw: vec![],
                facts: vec![],
                computed: None,
                history: vec![],
                conflicts: vec![],
                clarification: None,
                semantic_judgment: SemanticJudgment::Skipped,
                completeness: crate::contract::RecallCompleteness {
                    retrieval_truncated: memories.len() > input.limit,
                    pending_related_sources: 0,
                    pending_updates: 0,
                    enrichment_gaps: 0,
                    plan_complete: true,
                },
                degraded_reasons: vec!["semantic_judgment_skipped:context_only".into()],
                snapshot: crate::contract::RecallSnapshot {
                    read_revision: before.revision,
                    source_revision: before.source_revision,
                },
            };
            let output = self.recall_runtime.run_bootstrap(
                &mut self.store,
                &request,
                crate::recall::Budget {
                    records: input.limit,
                    ..Default::default()
                },
                output,
                crate::recall::SafetySnapshot::capture(&before, &request)?,
            )?;
            return format_recall_output(output, legacy_output);
        }
        let provider = self.recall_provider(operation, input.offline)?;
        let result = self.recall_runtime.run_cancellable(
            &mut self.store,
            &request,
            crate::recall::Budget {
                records: input.limit,
                ..Default::default()
            },
            provider.as_ref(),
            implicit_at,
            cancelled,
        )?;
        format_recall_output(result, legacy_output)
    }

    pub(super) fn call_recall_v3(
        &mut self,
        name: &str,
        arguments: Value,
        cancelled: impl Fn() -> bool,
        operation: &str,
    ) -> PersistenceResult<Value> {
        match name {
            "memory_recall" => {
                self.recall_v3(serde_json::from_value(arguments)?, operation, &cancelled)
            }
            "memory_prepare" => {
                let input: Prepare = serde_json::from_value(arguments)?;
                check_limit(input.limit)?;
                input.user_turn.validate()?;
                let key = receipt_key(
                    "memory_remember",
                    &input.user_turn.session_id,
                    &input.user_turn.turn_id,
                )?;
                let id = Self::recall_id(Some(input.request_id.unwrap_or_else(|| {
                    format!(
                        "prepare:{}",
                        crate::openai::recall_models::fingerprint(&json!(key))
                    )
                })))?;
                if input.offline
                    && self.config.enrichment_mode == EnrichmentMode::Inline
                    && self.config.write_pipeline == WritePipeline::Legacy
                {
                    return Err("offline prepare requires enrichment_mode off/deferred".into());
                }
                let query = input.user_turn.text.clone();
                let context = input.user_turn.history.clone();
                let task_id = input.user_turn.task_id.clone();
                let capture = input.user_turn.capture_source;
                let at = input.user_turn.observed_at;
                let source_request = serde_json::to_value(&input.user_turn)?;
                let saved = self.remember(input.user_turn, &cancelled, false, operation);
                let mut receipt = self.prepare_fragments(&key, &source_request, &saved)?;
                let rejected = matches!(
                    receipt["source"]["status"].as_str(),
                    Some("rejected" | "failed" | "deleted")
                );
                let recall_id = id.clone();
                let recall = if rejected || (capture && receipt["source"]["status"] != "stored") {
                    json!({"status":"not_requested","reason":"source_not_accepted"})
                } else {
                    check_cancelled(&cancelled)?;
                    match self.recall_v3(
                        Recall {
                            query,
                            context,
                            task_id,
                            at,
                            limit: input.limit,
                            bootstrap: false,
                            offline: input.offline,
                            recall_version: input.recall_version,
                            request_id: Some(id),
                        },
                        operation,
                        &cancelled,
                    ) {
                        Ok(out) => {
                            if capture {
                                receipt = self.recall_runtime.prepared_receipt(
                                    &self.store,
                                    &recall_id,
                                    &key,
                                )?;
                            }
                            out
                        }
                        Err(e) => json!({"status":"failed","error":e.to_string()}),
                    }
                };
                if input.recall_version == Some(RecallVersion::Legacy) {
                    let recalled = if recall.get("answer_contract").is_some() {
                        json!({"status":"completed","result":recall})
                    } else {
                        recall
                    };
                    return Ok(
                        json!({"remember":{"status":if saved.is_ok() {"completed"} else {"failed"},"result":receipt},"recall":recalled}),
                    );
                }
                Ok(
                    json!({"source":receipt["source"],"enrichment":receipt["enrichment"],"recall":recall}),
                )
            }
            "memory_validate_answer" => {
                let versioned: VersionedValidateAnswer = serde_json::from_value(arguments)?;
                let input = versioned.answer;
                validate_query_input(&input.query, &input.context, input.task_id.as_deref())?;
                bounded_text(&input.draft_answer, "draft_answer", 4_000)?;
                if input.source_ids.len() > 20 {
                    return Err("too many source IDs".into());
                }
                for id in &input.source_ids {
                    bounded_text(id, "source ID", 200)?;
                }
                let id = Self::recall_id(versioned.request_id.clone())?;
                let output = self.recall_runtime.retained(&self.store, &id)?;
                if output.request.query != input.query
                    || output.request.context != input.context
                    || output.request.task_id != input.task_id
                    || versioned.at.is_some_and(|at| at != output.request.at)
                {
                    return Err("recall_request_mismatch".into());
                }
                if versioned.recall_version == Some(RecallVersion::Legacy) {
                    let mut contract = legacy_contract(&output);
                    let approved = contract["safe_answer"].as_str() == Some(&input.draft_answer)
                        && json!(input.source_ids) == contract["safe_source_ids"];
                    contract["approved"] = json!(approved);
                    contract["approved_source_ids"] = if approved {
                        contract["safe_source_ids"].clone()
                    } else {
                        json!([])
                    };
                    return Ok(contract);
                }
                Ok(validate_retained_answer(&input, &output))
            }
            _ => unreachable!(),
        }
    }

    fn prepare_fragments(
        &self,
        key: &str,
        expected: &Value,
        saved: &PersistenceResult<Value>,
    ) -> PersistenceResult<Value> {
        let state = self.store.read()?;
        if let Some(source) = state.sources.get(key) {
            if source.request.as_ref().is_some_and(|r| r != expected) {
                return Ok(json!({"source":{"status":"rejected","source_id":null},
                    "enrichment":{"status":"not_requested","candidates":[],"memory_requests":[],"retention_pending":false}}));
            }
            if source.task.processing_version.as_deref() == Some(W4_PROCESSING_VERSION) {
                return crate::enrichment::receipt(&self.store, key);
            }
            let status = match source.enrichment.as_str() {
                "stale_requires_review" => "needs_rejudge",
                "completed_pending" => "pending",
                "completed_rejected" | "review_incomplete" => "failed",
                "not_requested" | "queued" | "running" | "committed" | "completed_no_facts"
                | "pending" | "failed" => &source.enrichment,
                _ => "failed",
            };
            let original = source.result.as_ref().unwrap_or(&Value::Null);
            let candidates = legacy_candidates(original);
            return Ok(
                json!({"source":{"status":if source.request.is_some() {"stored"} else {"deleted"},"source_id":source.id},
                "enrichment":{"status":if source.request.is_some() {status} else {"deleted"},"candidates":candidates,
                    "memory_requests":[],"retention_pending":false}}),
            );
        }
        let value = match saved {
            Ok(v) => v.clone(),
            Err(e) => error_body(e.as_ref()),
        };
        let candidates = legacy_candidates(&value);
        let source_status = value["source"]["status"]
            .as_str()
            .filter(|s| matches!(*s, "stored" | "rejected" | "failed" | "deleted"))
            .unwrap_or(if saved.is_ok() { "rejected" } else { "failed" });
        let enrichment_status = if saved.is_err() {
            "not_requested"
        } else if candidates.is_empty() {
            "completed_no_facts"
        } else if candidates.iter().any(|c| c["status"] == "committed") {
            "committed"
        } else {
            "pending"
        };
        Ok(json!({"source":{"status":source_status,"source_id":null},
            "enrichment":{"status":enrichment_status,"candidates":candidates,"memory_requests":[],"retention_pending":false}}))
    }

    pub(super) fn validate_v3_prepare_response(
        &self,
        response: &CallToolResult,
        state: &PersistedMemoryState,
    ) -> Option<CallToolResult> {
        let body = response.structured_content.as_ref()?;
        let compatibility = body["remember"]["result"].get("source").is_some()
            && body["recall"]["result"].get("request_id").is_some();
        if (body.get("source").is_none() || body.get("recall").is_none()) && !compatibility {
            return None;
        }
        let source = if compatibility {
            &body["remember"]["result"]["source"]
        } else {
            &body["source"]
        };
        let source_id = source["source_id"].as_str();
        let live = source_id.is_none()
            || state.sources.values().any(|src| {
                Some(src.id.as_str()) == source_id
                    && src.request.is_some()
                    && src.enrichment != "deleted"
            });
        let recall = if compatibility {
            &body["recall"]["result"]
        } else {
            &body["recall"]
        };
        let id = if compatibility {
            recall["request_id"].as_str()
        } else {
            recall["request"]["request_id"].as_str()
        };
        let valid = id.is_none_or(|id| {
            self.recall_runtime
                .retained_in_state(&self.store, state, id)
                .is_ok()
        });
        let mut body = body.clone();
        if body.get("global_context").is_some() {
            body["global_context"] = match self.global_context_from_state(state) {
                Ok(context) => {
                    json!({"status":"available","source":"local_snapshot","result":context})
                }
                Err(_) => json!({"status":"unavailable"}),
            };
        }
        if live && valid {
            let mut checked = CallToolResult::structured(body);
            checked.is_error = response.is_error;
            return Some(checked);
        }
        if !live {
            let receipt = if compatibility {
                &mut body["remember"]["result"]
            } else {
                &mut body
            };
            receipt["source"]["status"] = json!("deleted");
            receipt["enrichment"] = json!({"status":"deleted","candidates":[],"memory_requests":[],"retention_pending":false});
        }
        body["recall"] = json!({"status":"not_requested","reason":"state_changed_after_prepare"});
        Some(CallToolResult::structured(body))
    }

    /// MCP 最终发布只对新召回执行安全重读；普通工具保留已完成的业务回执和辅助读取降级。
    pub fn publish_shared_response<T>(
        shared: &std::sync::Mutex<Self>,
        name: &str,
        arguments: &Value,
        response: CallToolResult,
        publish: impl FnOnce(CallToolResult) -> T,
    ) -> PersistenceResult<T> {
        let operation = {
            let service = shared.lock().map_err(|_| "memory service lock poisoned")?;
            service
                .uses_recall_runtime(name, arguments)
                .unwrap_or(false)
                .then(|| service.operation())
        };
        match operation {
            Some(operation) => operation.connect()?.publish_response(response, publish),
            None => Ok(publish(response)),
        }
    }

    /// 只在新召回最终发布的短边界持有 SQLite 写锁；不覆盖模型等待。
    pub fn publish_response<T>(
        &self,
        response: CallToolResult,
        publish: impl FnOnce(CallToolResult) -> T,
    ) -> PersistenceResult<T> {
        self.store.read_for_publication(|state| {
            if let Some(checked) = self.validate_v3_prepare_response(&response, state) {
                return Ok(publish(checked));
            }
            let mut response = response;
            let body = response.structured_content.as_ref();
            let new_recall = self.config.recall_pipeline == RecallPipeline::RecallV3
                || body.is_some_and(|b| {
                    b["recall"]["request"]["request_id"].is_string()
                        || b["request"]["request_id"].is_string()
                        || b["request_id"].is_string()
                        || b["error"].as_str().is_some_and(|e| e.starts_with("recall_"))
                });
            if let Some(body) = response.structured_content.as_mut()
                && new_recall
                && body.get("global_context").is_some()
            {
                body["global_context"] = match self.global_context_from_state(state) {
                    Ok(context) => {
                        json!({"status":"available","source":"local_snapshot","result":context})
                    }
                    Err(_) => json!({"status":"unavailable"}),
                };
                let is_error = response.is_error;
                response = CallToolResult::structured(body.clone());
                response.is_error = is_error;
            }
            let body = response.structured_content.as_ref();
            let id = body.and_then(|b| {
                b["recall"]["request"]["request_id"]
                    .as_str()
                    .or_else(|| b["request"]["request_id"].as_str())
                    .or_else(|| b["request_id"].as_str())
            });
            if id.is_some_and(|id| {
                self.recall_runtime
                    .retained_in_state(&self.store, state, id)
                    .is_err()
            }) {
                return Ok(publish(CallToolResult::structured_error(
                    json!({"error":"recall_snapshot_changed",
                    "global_context":match self.global_context_from_state(state) {
                        Ok(context) => json!({"status":"available","source":"local_snapshot","result":context}),
                        Err(_) => json!({"status":"unavailable"}),
                    }}),
                )));
            }
            Ok(publish(response))
        })
    }

    #[cfg(test)]
    pub(crate) fn inject_recall_provider(&mut self, provider: RecallModelProvider) {
        self.recall_provider = Some(provider);
    }

    #[cfg(test)]
    pub(crate) fn shared_recall_runtime(&self) -> Arc<crate::openai::recall_models::RecallRuntime> {
        Arc::clone(&self.recall_runtime)
    }
}

fn legacy_candidates(result: &Value) -> Vec<Value> {
    result["outcomes"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(i, out)| {
            let applied = &out["Ingested"]["Applied"];
            let duplicate = &out["Ingested"]["Duplicate"];
            let (status, memory_id, reasons) = if applied.is_object() {
                ("committed", applied["memory_id"].clone(), vec![])
            } else if duplicate.is_object() {
                ("committed", duplicate["memory_id"].clone(), vec![])
            } else if out["Ingested"].get("Rejected").is_some() {
                (
                    "rejected",
                    Value::Null,
                    vec![out["Ingested"]["Rejected"].clone()],
                )
            } else if out.get("Pending").is_some() {
                ("unresolved", Value::Null, vec![])
            } else {
                ("pending", Value::Null, vec![])
            };
            json!({"index":i,"status":status,"memory_id":memory_id,"update":null,"reasons":reasons})
        })
        .collect()
}

/// ponytail: 仅核准简单事实的完全相同陈述；复杂限定/计算需宿主按 recall.v3 组织回答。
fn validate_retained_answer(input: &ValidateAnswer, out: &RecallOutput) -> Value {
    let fact = out.facts.first();
    // R5 对已知相关原文遗漏降 current=unknown、status=insufficient；两者都不能批准。
    // completeness 是包级计数，不能因无关裁剪或完整可见的 pending 原文一律拒绝。
    let eligible = out.status == RecallStatus::Answered
        && out.semantic_judgment == SemanticJudgment::Model
        && out.facts.len() == 1
        && out.computed.is_none()
        && out.history.is_empty()
        && out.conflicts.is_empty()
        && out.clarification.is_none()
        && fact.is_some_and(|f| {
            f.current == Qualification::Yes
                && f.applies == Qualification::Yes
                && f.polarity == Polarity::Affirm
                && f.strength.is_none()
                && f.uncertainty.is_none()
                && f.conditions.is_empty()
                && f.times.is_empty()
                && f.update
                    .as_ref()
                    .is_none_or(|u| u.status == UpdateStatus::Judged)
        });
    let sources: BTreeSet<_> = fact
        .into_iter()
        .flat_map(|f| &f.evidence)
        .filter_map(|e| match &e.source {
            SourceRef::Stored { source_id } => Some(source_id.clone()),
            SourceRef::Current {} => None,
        })
        .collect();
    let proposed: BTreeSet<_> = input.source_ids.iter().cloned().collect();
    let approved = eligible
        && input.source_ids.len() == proposed.len()
        && proposed == sources
        && fact.is_some_and(|f| f.statement == input.draft_answer);
    json!({"version":"recall.v3","request":out.request,"snapshot":out.snapshot,
        "approved":approved,"approved_source_ids":if approved {sources} else {BTreeSet::new()},
        "reason":if approved {"retained_exact_statement"} else {"draft_requires_semantic_review"}})
}

// 冻结§7：同一次 R4/快照机械生成旧回执；没有重跑旧检索或额外模型调用。
fn format_recall_output(output: RecallOutput, legacy: bool) -> PersistenceResult<Value> {
    if legacy {
        Ok(
            json!({"answer_contract":legacy_contract(&output),"request_id":output.request.request_id,
            "read_revision":output.snapshot.read_revision,"source_revision":output.snapshot.source_revision}),
        )
    } else {
        Ok(serde_json::to_value(output)?)
    }
}

fn legacy_contract(out: &RecallOutput) -> Value {
    let fact = out.facts.first();
    let mut answer = String::new();
    let mut ids = BTreeSet::new();
    let mut sources = Vec::new();
    let safe = !out.completeness.retrieval_truncated
        && out.completeness.pending_related_sources == 0
        && out.completeness.pending_updates == 0
        && out.completeness.enrichment_gaps == 0
        && out.completeness.plan_complete
        && fact.is_some_and(|f| {
            let input = ValidateAnswer {
                query: out.request.query.clone(),
                context: out.request.context.clone(),
                task_id: out.request.task_id.clone(),
                draft_answer: f.statement.clone(),
                source_ids: f
                    .evidence
                    .iter()
                    .filter_map(|e| match &e.source {
                        SourceRef::Stored { source_id } => Some(source_id.clone()),
                        _ => None,
                    })
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect(),
            };
            f.grade == crate::contract::RecallGrade::Committed
                && f.update
                    .as_ref()
                    .is_some_and(|u| u.status == UpdateStatus::Judged)
                && validate_retained_answer(&input, out)["approved"] == true
        });
    let status = if out.current_message.is_none()
        || !crate::local_recall::asks_for_answer(&out.request.query)
    {
        "not_applicable"
    } else if safe {
        let f = fact.unwrap();
        answer = f.statement.clone();
        for e in &f.evidence {
            if let SourceRef::Stored { source_id } = &e.source {
                ids.insert(source_id.clone());
                sources
                    .push(json!({"source_id":source_id,"quote":e.quote.text,"grade":"committed"}));
            }
        }
        "source_linked_fact"
    } else {
        answer = "已有证据无法在旧回执中安全表达，因此不能据此核准事实答复。".into();
        "insufficient_evidence"
    };
    json!({"version":1,"query":out.request.query,"context":out.request.context,"task_id":out.request.task_id,
        "status":status,"safe_answer":if status == "not_applicable" {Value::Null} else {json!(answer)},
        "safe_source_ids":ids,"sources":sources,"read_revision":out.snapshot.read_revision,
        "source_revision":out.snapshot.source_revision})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn new_validator_checks_r5_gate_instead_of_blanket_truncation() {
        let mut out: RecallOutput = serde_json::from_value(json!({
            "version":"recall.v3","request":{"request_id":"gate","query":"住所？","context":[],"task_id":null,"at":1791000000},
            "status":"answered","current_message":null,"records":[],"raw":[],
            "facts":[{"statement":"我住上海。","evidence":[{"source":{"kind":"stored","source_id":"home"},"quote":{"text":"我住上海。"}}],
                "record_ids":[1],"polarity":"affirm","strength":null,"uncertainty":null,"conditions":[],"times":[],"current":"yes","applies":"yes",
                "supersedes_record_ids":[],"grade":"committed","update":{"status":"judged","reasons":[]}}],
            "computed":null,"history":[],"conflicts":[],"clarification":null,"semantic_judgment":"model",
            "completeness":{"retrieval_truncated":true,"pending_related_sources":1,"pending_updates":0,"enrichment_gaps":0,"plan_complete":false},
            "degraded_reasons":[],"snapshot":{"read_revision":1,"source_revision":1}
        })).unwrap();
        let input: ValidateAnswer = serde_json::from_value(json!({
            "query":"住所？","draft_answer":"我住上海。","source_ids":["home"]
        }))
        .unwrap();
        assert_eq!(validate_retained_answer(&input, &out)["approved"], true);
        out.facts[0].current = Qualification::Unknown;
        assert_eq!(validate_retained_answer(&input, &out)["approved"], false);
        out.facts[0].current = Qualification::Yes;
        out.status = RecallStatus::Insufficient;
        assert_eq!(validate_retained_answer(&input, &out)["approved"], false);
    }

    #[test]
    fn legacy_adapter_quotes_committed_fact_but_withholds_incomplete_and_uncommitted() {
        // 已经经过 R4/R5 的冻结结果；这里只检验无模型适配，不替代上游核验。
        let mut out: RecallOutput = serde_json::from_value(json!({
            "version":"recall.v3","request":{"request_id":"adapt","query":"我的灯是什么颜色？","context":[],"task_id":null,"at":1791000000},
            "status":"answered","current_message":{"source_id":null,"text":"我的灯是什么颜色？","said_on":"2026-10-03","sequence":null,"enrichment":"not_stored","memory_requests":[]},
            "records":[],"raw":[],"facts":[{"statement":"我的灯是紫色。","evidence":[{"source":{"kind":"stored","source_id":"s-purple"},"quote":{"text":"我的灯是紫色。"}}],
                "record_ids":[1],"polarity":"affirm","strength":null,"uncertainty":null,"conditions":[],"times":[],"current":"yes","applies":"yes","supersedes_record_ids":[],"grade":"committed","update":{"status":"judged","reasons":[]}}],
            "computed":null,"history":[],"conflicts":[],"clarification":null,"semantic_judgment":"model",
            "completeness":{"retrieval_truncated":false,"pending_related_sources":0,"pending_updates":0,"enrichment_gaps":0,"plan_complete":true},
            "degraded_reasons":[],"snapshot":{"read_revision":1,"source_revision":1}
        })).unwrap();
        let old = legacy_contract(&out);
        assert_eq!(old["status"], "source_linked_fact");
        assert_eq!(
            old["sources"],
            json!([{"source_id":"s-purple","quote":"我的灯是紫色。","grade":"committed"}])
        );
        out.completeness.retrieval_truncated = true;
        assert_eq!(legacy_contract(&out)["status"], "insufficient_evidence");
        out.completeness.retrieval_truncated = false;
        out.facts[0].grade = crate::contract::RecallGrade::Uncommitted;
        assert_eq!(legacy_contract(&out)["safe_source_ids"], json!([]));
    }
}
