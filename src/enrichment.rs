//! 新路径的库级入口。调用方显式登记轮次；W1/W3/W4 显式接入，生产分发仍待后续段。
use std::collections::{BTreeMap, BTreeSet};

use crate::contract::*;
use crate::*;

#[derive(Clone, Copy, Debug)]
pub struct RuntimeInputBudget {
    pub w4: w4::W4Budget,
    pub max_candidates: usize,
    pub max_relations: usize,
    pub max_raw_bytes: usize,
}
impl Default for RuntimeInputBudget {
    fn default() -> Self {
        Self {
            w4: w4::W4Budget {
                facts: 64,
                records: 256,
            },
            max_candidates: 64,
            max_relations: 64,
            max_raw_bytes: 262_144,
        }
    }
}

/// 完整上游批次：拒收项仍在 W2 中；W1 为空时不产生 W3。
/// 提供者不能返回 committed_id、组编号、计划或提交快照。
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct UpstreamBatch {
    pub w2: w2::Batch,
    pub review: Option<ReviewedW3>,
}

#[derive(Clone, Debug)]
pub struct ProviderContext {
    pub owner: UserId,
    pub source_key: String,
    pub source_id: String,
    pub source_binding: serde_json::Value,
    pub observed_at: i64,
    pub received_sequence: u64,
    pub task_id: Option<TaskId>,
    pub round: u32,
    pub operation_id: String,
    /// 来源快照变化而需重核验时，复用这些最终候选与请求，不能重跑 W1 后按索引套产物。
    pub previous_w2: Option<w2::Batch>,
}

#[derive(serde::Serialize, Clone, Debug)]
pub struct SourceInput {
    pub text: String,
    pub history: Vec<String>,
}

impl SourceInput {
    fn of(source: &SourceRecord) -> PersistenceResult<Self> {
        let request = source.request.as_ref().ok_or("source_deleted")?;
        Ok(Self {
            text: crate::lexical::body(source).ok_or("source_deleted")?.into(),
            history: request
                .get("history")
                .map(|v| serde_json::from_value(v.clone()))
                .transpose()?
                .unwrap_or_default(),
        })
    }

    fn raw_bytes(&self) -> usize {
        self.text.len() + self.history.iter().map(String::len).sum::<usize>()
    }
}

/// 返回 contracts 3.5 的 source/enrichment 片段，不改变现有 MCP 输出。
pub fn run_source(
    store: &mut SqliteMemoryStore,
    key: &str,
    budget: RuntimeInputBudget,
    mut upstream: impl FnMut(&ProviderContext, &SourceInput) -> PersistenceResult<UpstreamBatch>,
    mut judge: impl FnMut(
        &ProviderContext,
        &W4Input,
        &BTreeMap<String, SourceInput>,
    ) -> PersistenceResult<Option<W4Output>>,
) -> PersistenceResult<serde_json::Value> {
    run_with_upstream(
        store,
        key,
        budget,
        false,
        |_, _, ctx, input| upstream(ctx, input),
        |_, _, ctx, input, texts| judge(ctx, input, texts),
    )
}

/// 显式模型入口；不更改后台分发、MCP 或旧处理版本。W4 继续使用注入点。
pub fn run_source_with_models(
    store: &mut SqliteMemoryStore,
    key: &str,
    budget: RuntimeInputBudget,
    provider: &crate::write_models::WriteModelProvider,
    mut judge: impl FnMut(
        &ProviderContext,
        &W4Input,
        &BTreeMap<String, SourceInput>,
    ) -> PersistenceResult<Option<W4Output>>,
) -> PersistenceResult<serde_json::Value> {
    run_with_upstream(
        store,
        key,
        budget,
        true,
        |store, claim, ctx, input| provider.provide(store, claim, ctx, input),
        |_, _, ctx, input, texts| judge(ctx, input, texts),
    )
}

/// 完整 W1/W3/W4 模型入口；沿用同一恢复、validate、计划与 W5 提交流程。
pub fn run_source_with_all_models(
    store: &mut SqliteMemoryStore,
    key: &str,
    budget: RuntimeInputBudget,
    provider: &crate::write_models::WriteModelProvider,
) -> PersistenceResult<serde_json::Value> {
    run_with_upstream(
        store,
        key,
        budget,
        true,
        |store, claim, ctx, input| provider.provide(store, claim, ctx, input),
        |store, claim, ctx, input, texts| provider.judge(store, claim, ctx, input, texts).map(Some),
    )
}

fn run_with_upstream(
    store: &mut SqliteMemoryStore,
    key: &str,
    budget: RuntimeInputBudget,
    verify_upstream: bool,
    mut upstream: impl FnMut(
        &mut SqliteMemoryStore,
        &W4Claim,
        &ProviderContext,
        &SourceInput,
    ) -> PersistenceResult<UpstreamBatch>,
    mut judge: impl FnMut(
        &mut SqliteMemoryStore,
        &W4Claim,
        &ProviderContext,
        &W4Input,
        &BTreeMap<String, SourceInput>,
    ) -> PersistenceResult<Option<W4Output>>,
) -> PersistenceResult<serde_json::Value> {
    let claim = w4_claim_round(store, key)?;
    if let Err(error) = run_claimed(
        store,
        key,
        budget,
        &claim,
        verify_upstream,
        &mut upstream,
        &mut judge,
    ) {
        let reason = error.to_string();
        // A late executor must not release or overwrite the successor's claim (or resurrect deletion).
        if reason.starts_with("claim_lost") || reason == "source_deleted" {
            return Err(error);
        }
        // Decoder/provider errors can contain quoted raw inputs. Persist codes, never their details.
        let code = [
            "model_timeout",
            "model_invalid_output",
            "w1_invalid_output",
            "w4_invalid_output",
            "needs_rejudge",
            "budget_exhausted",
        ]
        .into_iter()
        .find(|code| reason == *code || reason.starts_with(&format!("{code}:")))
        .unwrap_or("attempt_failed");
        let status = match code {
            "needs_rejudge" => "needs_rejudge",
            "budget_exhausted" => "budget_exhausted",
            _ => "failed",
        };
        let mut diagnostic = serde_json::json!({"reason":code});
        if reason == "model_invalid_output: model_result_missing" {
            diagnostic["detail_code"] = serde_json::json!("model_result_missing");
        }
        crate::commit::w4_pause_attempt(store, &claim, status, Some(code), diagnostic)?;
    }
    receipt(store, key)
}

fn run_claimed(
    store: &mut SqliteMemoryStore,
    key: &str,
    budget: RuntimeInputBudget,
    claim: &W4Claim,
    verify_upstream: bool,
    upstream: &mut impl FnMut(
        &mut SqliteMemoryStore,
        &W4Claim,
        &ProviderContext,
        &SourceInput,
    ) -> PersistenceResult<UpstreamBatch>,
    judge: &mut impl FnMut(
        &mut SqliteMemoryStore,
        &W4Claim,
        &ProviderContext,
        &W4Input,
        &BTreeMap<String, SourceInput>,
    ) -> PersistenceResult<Option<W4Output>>,
) -> PersistenceResult<()> {
    let saved = w4_progress(store, key)?;
    let stored = store.read()?;
    let source = &stored.sources[key];
    if let Some(prior) = stored_batch(source)?
        && (saved.plan_ready || w4_load_inputs(store, claim)?.is_some())
    {
        if prior.source_binding != *claim.source_binding() {
            return Err("needs_rejudge: reviewed source snapshot changed".into());
        }
        let input = SourceInput::of(source)?;
        check_batch(
            &prior.batch,
            claim.source_binding(),
            &input.text,
            crate::lexical::task(source).map(TaskId::from).as_ref(),
        )?;
    }
    let previous = stored_batch(source)?;
    let ctx = ProviderContext {
        owner: store.owner().clone(),
        source_key: key.into(),
        source_id: source.id.clone(),
        source_binding: claim.source_binding().clone(),
        observed_at: source.observed_at,
        received_sequence: source.received_sequence,
        task_id: crate::lexical::task(source).map(TaskId::from),
        round: claim.round,
        operation_id: claim.operation_id.clone(),
        previous_w2: previous.as_ref().map(|p| p.batch.w2.clone()),
    };
    let input = SourceInput::of(source)?;
    let upstream_bytes = input.raw_bytes();
    if !saved.plan_ready {
        crate::commit::w4_check_order(store, claim)?;
    }
    if !saved.plan_ready {
        if over_budget(
            store,
            claim,
            "raw_bytes",
            upstream_bytes,
            budget.max_raw_bytes,
        )? {
            return Ok(());
        }
    }
    // Model stage bindings must also be checked when a full batch/plan already exists.
    let model_batch = if verify_upstream {
        Some(upstream(store, claim, &ctx, &input)?)
    } else {
        None
    };
    if !saved.plan_ready {
        let inputs = match w4_load_inputs(store, claim)? {
            Some(inputs) => inputs,
            None => {
                let batch = match (model_batch, previous) {
                    (Some(batch), _) => batch,
                    (None, Some(prior)) if prior.source_binding == ctx.source_binding => {
                        prior.batch
                    }
                    _ => upstream(store, claim, &ctx, &input)?,
                };
                check_batch(
                    &batch,
                    &ctx.source_binding,
                    &input.text,
                    ctx.task_id.as_ref(),
                )?;
                if ctx
                    .previous_w2
                    .as_ref()
                    .is_some_and(|prior| prior != &batch.w2)
                {
                    return Err(
                        "needs_rejudge: final W1 candidates or requests cannot be replaced".into(),
                    );
                }
                let requests = merged_requests(source, request_results(&batch, &ctx.source_id)?);
                let (blocked, whole_source) = request_blocks(&requests);
                let reviews = batch
                    .review
                    .as_ref()
                    .map(|r| r.for_input(&batch.w2, &ctx.source_binding, &input.text))
                    .transpose()?;
                let mut admissions = BTreeMap::new();
                let candidates: Vec<W4Candidate> = batch
                    .w2
                    .candidates
                    .iter()
                    .filter(|c| {
                        c.status == w2::Status::Ok && !whole_source && !blocked.contains(&c.index)
                    })
                    .filter(|c| {
                        reviews.is_some_and(|r| {
                            r.reviews
                                .iter()
                                .any(|r| r.candidate == c.index && r.verdict == Verdict::Supported)
                        })
                    })
                    .map(|c| {
                        let review = reviews
                            .unwrap()
                            .reviews
                            .iter()
                            .find(|r| r.candidate == c.index)
                            .unwrap();
                        admissions.insert(
                            c.index,
                            w4::Admission {
                                episode_useful: review.episode_value == EpisodeValue::Useful,
                                remember_bound: remember_bound(&batch, c.index),
                            },
                        );
                        W4Candidate {
                            index: c.index,
                            committed_id: saved
                                .candidates
                                .iter()
                                .find(|p| p.index == c.index)
                                .and_then(|p| p.committed_id),
                            candidate: c.candidate.clone(),
                            source: SourceRef::Stored {
                                source_id: ctx.source_id.clone(),
                            },
                            said_on: batch.w2.said_on.clone(),
                            time_resolutions: c.time_resolutions.clone(),
                        }
                    })
                    .collect();
                crate::commit::w4_save_batch(
                    store,
                    claim,
                    &candidates,
                    &admissions,
                    &batch,
                    &requests,
                )?;
                W4Inputs {
                    candidates,
                    admissions,
                }
            }
        };
        let W4Inputs {
            candidates,
            admissions,
        } = inputs;
        let candidate_count = stored_batch(&store.read()?.sources[key])?
            .map_or(candidates.len(), |b| b.batch.w2.candidates.len());
        if over_budget(
            store,
            claim,
            "candidates",
            candidate_count,
            budget.max_candidates,
        )? {
            return Ok(());
        }
        let pending: Vec<_> = candidates
            .iter()
            .map(|c| w4::Pending {
                index: c.index,
                committed_id: c.committed_id,
                contract: ContractCandidate {
                    candidate: c.candidate.clone(),
                    source: c.source.clone(),
                    said_on: c.said_on.clone(),
                    time_resolutions: c.time_resolutions.clone(),
                },
            })
            .collect();
        let (state, assembly) =
            w4::assemble(store, key, ctx.task_id.as_ref(), &pending, budget.w4)?;
        let texts = source_texts(&state, &assembly.input, &ctx.source_id)?;
        let raw_bytes = texts.values().map(SourceInput::raw_bytes).sum::<usize>();
        let counts = BTreeMap::from([
            ("raw_bytes".into(), raw_bytes),
            ("candidates".into(), candidate_count),
            ("relations".into(), assembly.input.relations.len()),
            ("records".into(), assembly.input.records.len()),
        ]);
        // Diagnostic-only retrieval status; no new frozen failure code and no raw-content copies.
        let retrieval: BTreeMap<_, _> = assembly
            .fact_retrieval
            .iter()
            .map(|(i, r)| {
                (
                    *i,
                    match r {
                        w4::FactRetrieval::FullText => "full_text",
                        w4::FactRetrieval::Fallback(_) => "fallback",
                    },
                )
            })
            .collect();
        crate::commit::w4_note_inputs(
            store,
            claim,
            counts,
            Some(serde_json::json!({"fact_retrieval":retrieval})),
        )?;
        if over_budget(
            store,
            claim,
            "relations",
            assembly.input.relations.len(),
            budget.max_relations,
        )? || over_budget(store, claim, "raw_bytes", raw_bytes, budget.max_raw_bytes)?
        {
            return Ok(());
        }
        let output = if assembly.call_w4 {
            match judge(store, claim, &ctx, &assembly.input, &texts)? {
                Some(output) => output,
                None => {
                    crate::commit::w4_pause_attempt(
                        store,
                        claim,
                        "pending",
                        None,
                        serde_json::json!({"reason":"capability_unavailable"}),
                    )?;
                    return Ok(());
                }
            }
        } else {
            assembly.skipped
        };
        let plan = w4::plan(
            &w4::W4Runtime {
                state: &state,
                owner: ctx.owner,
                source_key: key,
                round: claim.round,
                task_id: ctx.task_id,
                admissions,
            },
            &assembly.input,
            &output,
        )?;
        w4_save_plan(store, claim, &candidates, &plan)?;
    }
    for group in w4_progress(store, key)?.remaining {
        w4_commit_group(store, claim, &group)?;
    }
    w4_finish_round(store, claim)?;
    Ok(())
}

fn over_budget(
    store: &mut SqliteMemoryStore,
    claim: &W4Claim,
    item: &str,
    actual: usize,
    limit: usize,
) -> PersistenceResult<bool> {
    if actual <= limit {
        return Ok(false);
    }
    crate::commit::w4_pause_attempt(
        store,
        claim,
        "budget_exhausted",
        Some("budget_exhausted"),
        serde_json::json!({"item":item,"actual":actual,"limit":limit}),
    )?;
    Ok(true)
}

fn source_texts(
    state: &PersistedMemoryState,
    input: &W4Input,
    current: &str,
) -> PersistenceResult<BTreeMap<String, SourceInput>> {
    // Walk the frozen input's SourceRef values, including record history and relation evidence.
    fn sources(value: &serde_json::Value, ids: &mut std::collections::BTreeSet<String>) {
        match value {
            serde_json::Value::Object(map) => {
                if map.get("kind").and_then(|v| v.as_str()) == Some("stored")
                    && let Some(id) = map.get("source_id").and_then(|v| v.as_str())
                {
                    ids.insert(id.into());
                }
                for v in map.values() {
                    sources(v, ids);
                }
            }
            serde_json::Value::Array(values) => {
                for v in values {
                    sources(v, ids);
                }
            }
            _ => {}
        }
    }
    let mut ids = std::collections::BTreeSet::from([current.to_owned()]);
    sources(&serde_json::to_value(input)?, &mut ids);
    ids.into_iter()
        .filter_map(|id| {
            // Invalidated relations retain deleted source IDs and tombstones as historical context.
            // Only live raw inputs are provided; validate/W5 still reject deleted decision evidence.
            let source = state
                .sources
                .values()
                .find(|s| s.id == id)
                .filter(|s| crate::lexical::body(s).is_some())?;
            Some(SourceInput::of(source).map(|input| (id, input)))
        })
        .collect()
}

pub fn receipt(store: &SqliteMemoryStore, key: &str) -> PersistenceResult<serde_json::Value> {
    let state = store.read()?;
    let source = state.sources.get(key).ok_or("source missing")?;
    let requests = visible_requests(source)?;
    let retention_pending = requests.iter().any(|r| r["status"] == "retention_pending");
    Ok(serde_json::json!({
        "source": {"status":if source.request.is_some() {"stored"} else {"deleted"},"source_id":source.id},
        "enrichment": {"status":source.enrichment,"candidates":crate::commit::w4_candidate_results(&state,key)?,"memory_requests":requests,"retention_pending":retention_pending}
    }))
}

/// 既有 result 的内部扩展；不改变数据库表、版本、W5 指纹或冻结输出。
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StoredBatch {
    pub source_binding: serde_json::Value,
    pub batch: UpstreamBatch,
}

pub(crate) fn stored_batch(source: &SourceRecord) -> PersistenceResult<Option<StoredBatch>> {
    source
        .result
        .as_ref()
        .and_then(|r| r.get("w4_batch"))
        .map(|b| serde_json::from_value(b.clone()).map_err(Into::into))
        .transpose()
}

fn check_batch(
    batch: &UpstreamBatch,
    snapshot: &serde_json::Value,
    text: &str,
    task: Option<&TaskId>,
) -> PersistenceResult<()> {
    let w2 = &batch.w2;
    let mut next = 0;
    let expected: Vec<_> = w2
        .candidates
        .iter()
        .enumerate()
        .map(|(i, c)| {
            if c.index != i {
                return Err("w1_invalid_output");
            }
            Ok((c.status != w2::Status::Rejected).then(|| {
                let i = next;
                next += 1;
                i
            }))
        })
        .collect::<Result<_, _>>()?;
    if expected != w2.index_map
        || w2.task_id.as_ref() != task
        || crate::temporal::parse_iso_date(&w2.said_on).is_none()
        || time::UtcOffset::from_whole_seconds(w2.utc_offset_seconds).is_err()
    {
        return Err("w1_invalid_output".into());
    }
    for (i, request) in w2.memory_requests.iter().enumerate() {
        let original = &request.request.target.candidate_indices;
        if request.index != i
            || original.iter().any(|i| *i >= expected.len())
            || request.final_candidate_indices
                != original
                    .iter()
                    .filter_map(|i| expected[*i])
                    .collect::<Vec<_>>()
            || request.rejected_candidate_indices
                != original
                    .iter()
                    .copied()
                    .filter(|i| expected[*i].is_none())
                    .collect::<Vec<_>>()
        {
            return Err("w1_invalid_output".into());
        }
    }
    if let Some(review) = &batch.review {
        if w2.candidates.is_empty() {
            return Err("model_invalid_output".into());
        }
        review.for_input(w2, snapshot, text)?;
    } else if w2
        .candidates
        .iter()
        .any(|c| c.status != w2::Status::Rejected)
    {
        return Err("model_invalid_output".into());
    }
    Ok(())
}

fn remember_bound(batch: &UpstreamBatch, candidate: usize) -> bool {
    let Some(final_index) = batch.w2.index_map[candidate] else {
        return false;
    };
    batch.w2.memory_requests.iter().any(|r| {
        r.request.kind == RequestKind::Remember
            && r.final_candidate_indices.contains(&final_index)
            && batch.review.as_ref().is_some_and(|review| {
                review
                    .output()
                    .request_reviews
                    .iter()
                    .any(|v| v.request == r.index && v.verdict == Verdict::Supported)
            })
    })
}

/// 计划保存和逐组提交也检查同一批次，避免提供者调用期间的变化绕过入口检查。
pub(crate) fn check_saved_inputs(
    source: &SourceRecord,
    snapshot: &serde_json::Value,
    candidates: &[W4Candidate],
    admissions: &BTreeMap<usize, w4::Admission>,
) -> PersistenceResult<()> {
    let Some(saved) = stored_batch(source)? else {
        return Ok(());
    };
    if saved.source_binding != *snapshot {
        return Err("needs_rejudge: reviewed source snapshot changed".into());
    }
    let input = SourceInput::of(source)?;
    let batch = saved.batch;
    check_batch(
        &batch,
        snapshot,
        &input.text,
        crate::lexical::task(source).map(TaskId::from).as_ref(),
    )?;
    let (blocked, whole) = request_blocks(&visible_requests(source)?);
    let reviews = batch.review.as_ref().map(|r| r.output());
    let eligible: BTreeMap<_, _> = batch
        .w2
        .candidates
        .iter()
        .filter(|c| c.status == w2::Status::Ok && !whole && !blocked.contains(&c.index))
        .filter_map(|c| {
            reviews
                .and_then(|r| {
                    r.reviews
                        .iter()
                        .find(|r| r.candidate == c.index && r.verdict == Verdict::Supported)
                })
                .map(|r| (c.index, (c, r)))
        })
        .collect();
    if eligible.len() != candidates.len() || admissions.len() != candidates.len() {
        return Err("needs_rejudge: reviewed admission changed".into());
    }
    for c in candidates {
        let (original, review) = eligible
            .get(&c.index)
            .ok_or("needs_rejudge: reviewed admission changed")?;
        if c.candidate != original.candidate
            || c.time_resolutions != original.time_resolutions
            || c.said_on != batch.w2.said_on
            || c.source
                != (SourceRef::Stored {
                    source_id: source.id.clone(),
                })
            || admissions.get(&c.index)
                != Some(&w4::Admission {
                    episode_useful: review.episode_value == EpisodeValue::Useful,
                    remember_bound: remember_bound(&batch, c.index),
                })
        {
            return Err("needs_rejudge: reviewed candidate or admission changed".into());
        }
    }
    Ok(())
}

fn request_results(
    batch: &UpstreamBatch,
    source_id: &str,
) -> PersistenceResult<Vec<serde_json::Value>> {
    batch
        .w2
        .memory_requests
        .iter()
        .map(|r| {
            // 使用 K3 映射反查稳定的原始索引，不能将丢失的候选目标扩大为旧事实或整源。
            let indices: Vec<_> = r
                .final_candidate_indices
                .iter()
                .filter_map(|final_index| {
                    batch
                        .w2
                        .index_map
                        .iter()
                        .position(|i| *i == Some(*final_index))
                })
                .collect();
            let target = match r.request.target.object {
                RequestObject::Source => serde_json::json!({"source_id":source_id}),
                RequestObject::Facts if !indices.is_empty() => {
                    serde_json::json!({"source_id":source_id,"candidate_indices":indices})
                }
                _ => serde_json::Value::Null,
            };
            let verdict = batch.review.as_ref().and_then(|review| {
                review
                    .output()
                    .request_reviews
                    .iter()
                    .find(|v| v.request == r.index)
                    .map(|v| v.verdict)
            });
            let status = match r.request.kind {
                RequestKind::DoNotStore | RequestKind::Forget => "awaiting_confirmation",
                RequestKind::Retention
                    if verdict == Some(Verdict::Supported) && !target.is_null() =>
                {
                    "retention_pending"
                }
                RequestKind::Retention => "awaiting_confirmation",
                RequestKind::Remember => match verdict {
                    Some(Verdict::Supported) => "supported",
                    Some(Verdict::Contradicted) => "contradicted",
                    Some(Verdict::Insufficient) => "insufficient",
                    None => "unreviewed",
                },
            };
            let mut result = serde_json::to_value(&r.request)?;
            result["index"] = serde_json::json!(r.index);
            result["verdict"] = serde_json::to_value(verdict)?;
            result["status"] = serde_json::json!(status);
            result["resolved_target"] = target;
            result["final_candidate_indices"] = serde_json::json!(r.final_candidate_indices);
            result["rejected_candidate_indices"] = serde_json::json!(r.rejected_candidate_indices);
            result["deadline_resolution"] = serde_json::to_value(&r.deadline_resolution)?;
            result["executed"] = serde_json::json!(false);
            Ok(result)
        })
        .collect()
}

fn same_request(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    ["kind", "quote", "target", "deadline"]
        .iter()
        .all(|k| a[*k] == b[*k])
}

fn merged_requests(
    source: &SourceRecord,
    mut requests: Vec<serde_json::Value>,
) -> Vec<serde_json::Value> {
    for control in source
        .result
        .as_ref()
        .and_then(|r| r["w4_controls"].as_array())
        .into_iter()
        .flatten()
    {
        if let Some(current) = requests.iter_mut().find(|r| same_request(r, control)) {
            // 疑似限制不能由后续模型的 supported 判断自动解除。
            if control["status"] == "awaiting_confirmation"
                || current["status"] != "awaiting_confirmation"
            {
                *current = control.clone();
            }
        } else {
            requests.push(control.clone());
        }
    }
    requests
}

pub(crate) fn visible_requests(source: &SourceRecord) -> PersistenceResult<Vec<serde_json::Value>> {
    if source.request.is_none() {
        return Ok(Vec::new());
    }
    let requests = stored_batch(source)?
        .map(|b| request_results(&b.batch, &source.id))
        .transpose()?
        .unwrap_or_default();
    Ok(merged_requests(source, requests))
}

fn request_blocks(requests: &[serde_json::Value]) -> (BTreeSet<usize>, bool) {
    let mut blocked = BTreeSet::new();
    let mut whole = false;
    for r in requests
        .iter()
        .filter(|r| r["status"] == "awaiting_confirmation")
    {
        let target = &r["resolved_target"];
        if target.is_null() || target.get("candidate_indices").is_none() {
            whole = true;
        }
        for i in target["candidate_indices"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|i| i.as_u64())
        {
            blocked.insert(i as usize);
        }
    }
    (blocked, whole)
}

pub(crate) fn merge_candidate_results(
    source: &SourceRecord,
    w4: Vec<serde_json::Value>,
) -> PersistenceResult<Vec<serde_json::Value>> {
    let Some(saved) = stored_batch(source)? else {
        return Ok(w4);
    };
    let batch = saved.batch;
    let (blocked, whole) = request_blocks(&visible_requests(source)?);
    let mut results = BTreeMap::new();
    for c in &batch.w2.candidates {
        let review = batch
            .review
            .as_ref()
            .and_then(|r| r.output().reviews.iter().find(|r| r.candidate == c.index));
        let mut reasons = c.reasons.clone();
        if let Some(review) = review {
            match review.verdict {
                Verdict::Contradicted => reasons.push("review_contradicted".into()),
                Verdict::Insufficient => reasons.push("review_insufficient".into()),
                Verdict::Supported => (),
            }
        }
        let status = match c.status {
            w2::Status::Rejected => "rejected",
            w2::Status::Unresolved => "unresolved",
            w2::Status::Ok if review.is_some_and(|r| r.verdict == Verdict::Contradicted) => {
                "rejected"
            }
            w2::Status::Ok => "pending",
        };
        if c.status == w2::Status::Ok && (whole || blocked.contains(&c.index)) {
            reasons.push("awaiting_confirmation".into());
        }
        results.insert(c.index, serde_json::json!({"index":c.index,"status":status,"memory_id":null,"update":null,"reasons":reasons}));
    }
    for result in w4 {
        let index = result["index"]
            .as_u64()
            .ok_or("invalid W4 candidate result")? as usize;
        results.insert(index, result);
    }
    Ok(results.into_values().collect())
}

pub(crate) fn summary_status(source: &SourceRecord, default: &str) -> PersistenceResult<String> {
    if default == "needs_rejudge" {
        return Ok(default.into());
    }
    if visible_requests(source)?
        .iter()
        .any(|r| r["status"] == "awaiting_confirmation")
    {
        return Ok("awaiting_confirmation".into());
    }
    if let Some(saved) = stored_batch(source)? {
        if saved.batch.w2.candidates.is_empty() {
            return Ok("no_candidates_unreviewed".into());
        }
        if saved
            .batch
            .review
            .as_ref()
            .is_some_and(|r| r.output().coverage.status == CoverageStatus::Gaps)
        {
            return Ok("coverage_gaps".into());
        }
        if default == "no_candidates_unreviewed" {
            return Ok("pending".into());
        }
    }
    Ok(default.into())
}
