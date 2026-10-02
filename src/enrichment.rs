//! 新路径的库级入口。调用方显式登记轮次；生产分发和模型提供者在第④段接入。
use std::collections::BTreeMap;

use crate::contract::{ContractCandidate, W4Candidate, W4Input, W4Output};
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

/// 提供者不能返回 committed_id、组编号或快照；它们属于运行时。
#[derive(Clone, Debug)]
pub struct SupportedCandidate {
    pub index: usize,
    pub contract: ContractCandidate,
    pub admission: w4::Admission,
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
}

#[derive(Clone, Debug)]
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
    mut upstream: impl FnMut(
        &ProviderContext,
        &SourceInput,
    ) -> PersistenceResult<Vec<SupportedCandidate>>,
    mut judge: impl FnMut(
        &ProviderContext,
        &W4Input,
        &BTreeMap<String, SourceInput>,
    ) -> PersistenceResult<Option<W4Output>>,
) -> PersistenceResult<serde_json::Value> {
    let claim = w4_claim_round(store, key)?;
    if let Err(error) = run_claimed(store, key, budget, &claim, &mut upstream, &mut judge) {
        let reason = error.to_string();
        // A late executor must not release or overwrite the successor's claim (or resurrect deletion).
        if reason.starts_with("claim_lost") || reason == "source_deleted" {
            return Err(error);
        }
        // Decoder/provider errors can contain quoted raw inputs. Persist codes, never their details.
        let code = [
            "model_timeout",
            "model_invalid_output",
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
        crate::commit::w4_pause_attempt(
            store,
            &claim,
            status,
            Some(code),
            serde_json::json!({"reason":code}),
        )?;
    }
    receipt(store, key)
}

fn run_claimed(
    store: &mut SqliteMemoryStore,
    key: &str,
    budget: RuntimeInputBudget,
    claim: &W4Claim,
    upstream: &mut impl FnMut(
        &ProviderContext,
        &SourceInput,
    ) -> PersistenceResult<Vec<SupportedCandidate>>,
    judge: &mut impl FnMut(
        &ProviderContext,
        &W4Input,
        &BTreeMap<String, SourceInput>,
    ) -> PersistenceResult<Option<W4Output>>,
) -> PersistenceResult<()> {
    let saved = w4_progress(store, key)?;
    if !saved.plan_ready {
        crate::commit::w4_check_order(store, claim)?;
        let state = store.read()?;
        let source = &state.sources[key];
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
        };
        let input = SourceInput::of(source)?;
        let upstream_bytes = input.raw_bytes();
        if over_budget(
            store,
            claim,
            "raw_bytes",
            upstream_bytes,
            budget.max_raw_bytes,
        )? {
            return Ok(());
        }
        let inputs = match w4_load_inputs(store, claim)? {
            Some(inputs) => inputs,
            None => {
                let supported = upstream(&ctx, &input)?;
                let admissions = supported.iter().map(|c| (c.index, c.admission)).collect();
                let candidates: Vec<W4Candidate> = supported
                    .into_iter()
                    .map(|c| W4Candidate {
                        index: c.index,
                        committed_id: saved
                            .candidates
                            .iter()
                            .find(|p| p.index == c.index)
                            .and_then(|p| p.committed_id),
                        candidate: c.contract.candidate,
                        source: c.contract.source,
                        said_on: c.contract.said_on,
                        time_resolutions: c.contract.time_resolutions,
                    })
                    .collect();
                w4_save_inputs(store, claim, &candidates, &admissions)?;
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
        if over_budget(
            store,
            claim,
            "candidates",
            candidates.len(),
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
            ("candidates".into(), candidates.len()),
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
            match judge(&ctx, &assembly.input, &texts)? {
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
    Ok(serde_json::json!({
        "source": {"status":if source.request.is_some() {"stored"} else {"deleted"},"source_id":source.id},
        "enrichment": {"status":source.enrichment,"candidates":crate::commit::w4_candidate_results(&state,key)?,"memory_requests":[],"retention_pending":false}
    }))
}
