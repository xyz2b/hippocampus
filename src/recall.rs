//! R2/R3 只读组装与 R4 注入、R5 派生；不调用模型或改变旧召回入口。
use std::collections::{BTreeMap, BTreeSet};

use crate::contract::*;
use crate::{
    EntityId, EntityRepository, Memory, MemoryId, MemoryRepository, MemoryStatus,
    PersistedMemoryState, PersistenceResult, ScopeCondition, SourceRecord, SqliteMemoryStore,
    UpdateState, UpdateStatus, UserId, r1, r4,
};

/// 由调用方提供的可信输入；不从 R1 模型输出取得归属、任务或时钟。
#[derive(Clone)]
pub struct Request {
    pub binding: RecallRequest,
    pub owner: UserId,
    pub subject_ids: Option<Vec<EntityId>>,
    pub said_on: String,
    pub utc_offset_seconds: i32,
    /// bootstrap 无本轮消息，其余只读请求固定带入 query。
    pub bootstrap: bool,
}

/// 证据输出预算，不涉及模型调用预算。记录和关系整项裁剪；raw 可按 UTF-8 边界截断。
#[derive(Clone, Copy, Debug)]
pub struct Budget {
    pub records: usize,
    pub raw: usize,
    pub relations: usize,
    pub stored_bytes: usize,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            records: 64,
            raw: 32,
            relations: 64,
            stored_bytes: 262_144,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SkipReason {
    Offline,
    Budget,
    NoEvidence,
    ContextOnly,
}

impl SkipReason {
    fn code(self) -> &'static str {
        match self {
            Self::Offline => "offline",
            Self::Budget => "budget",
            Self::NoEvidence => "no_evidence",
            Self::ContextOnly => "context_only",
        }
    }
}

/// ④ 的提供者在此报告失败；错误文本不进入对外输出。
#[derive(Clone, Copy, Debug)]
pub enum JudgmentFailure {
    Timeout,
    InvalidOutput,
}
impl JudgmentFailure {
    fn code(self) -> &'static str {
        match self {
            Self::Timeout => "model_timeout",
            Self::InvalidOutput => "model_invalid_output",
        }
    }
}

/// 进度/普通提交可推进；只有输入完整性、删除和访问范围改变才使读快照失效。
/// ponytail: 保守绑定可信任务内的既有来源/记录；规模变大时再缩到实际依赖集合。
#[derive(Clone)]
pub(crate) struct SafetySnapshot {
    request: Request,
    sources: BTreeMap<String, String>,
    records: BTreeMap<MemoryId, (String, Vec<crate::Evidence>)>,
    entities: BTreeMap<EntityId, String>,
    pub(crate) prepared: BTreeMap<String, serde_json::Value>,
}

fn source_binding(source: &SourceRecord) -> PersistenceResult<String> {
    // remember 的核验回执是整理进度，不是访问/保存限制；保留限制请求全部控制字段。
    let controls: Vec<_> = crate::enrichment::visible_requests(source)?
        .into_iter()
        .filter(|r| r["kind"] != "remember")
        .collect();
    let mut binding = serde_json::json!([
        source.id,
        source.session_id,
        source.turn_id,
        source.observed_at,
        source.received_sequence,
        source.request,
        source.enrichment == "awaiting_confirmation",
        controls
    ]);
    if source.is_history_import() {
        binding.as_array_mut().unwrap().push(serde_json::json!({
            "provenance":source.management.provenance,"time_anchor":source.management.time_anchor}));
    }
    Ok(crate::openai::recall_models::fingerprint(&binding))
}

fn record_binding(memory: &Memory) -> String {
    crate::openai::recall_models::fingerprint(&serde_json::json!([
        memory.owner_id,
        memory.subject_id,
        memory.memory_type,
        memory.predicate,
        memory.value,
        memory.polarity,
        memory.strength,
        memory.scope,
        memory.temporal.observed_at
    ]))
}

impl SafetySnapshot {
    pub(crate) fn capture(
        state: &PersistedMemoryState,
        request: &Request,
    ) -> PersistenceResult<Self> {
        let mut sources = BTreeMap::new();
        let mut prepared = BTreeMap::new();
        for (key, source) in &state.sources {
            if source_in_task(source, request) {
                sources.insert(source.id.clone(), source_binding(source)?);
                prepared.insert(
                    key.clone(),
                    crate::enrichment::receipt_from_state(state, key)?,
                );
            }
        }
        let records = state
            .repository
            .list_by_owner(&request.owner)?
            .into_iter()
            .filter(|m| record_in_scope(state, m, request))
            .map(|m| (m.id, (record_binding(&m), m.evidence)))
            .collect();
        let entities = state
            .entities
            .list_by_owner(&request.owner)
            .into_iter()
            .map(|e| {
                (
                    e.id.clone(),
                    crate::openai::recall_models::fingerprint(&serde_json::json!(e)),
                )
            })
            .collect();
        Ok(Self {
            request: request.clone(),
            sources,
            records,
            entities,
            prepared,
        })
    }

    pub(crate) fn source_ids(&self) -> Vec<&str> {
        self.sources.keys().map(String::as_str).collect()
    }

    pub(crate) fn check(
        &self,
        owner: &UserId,
        state: &PersistedMemoryState,
    ) -> PersistenceResult<()> {
        if owner != &self.request.owner {
            return Err("recall_owner_mismatch".into());
        }
        let valid_sources = self.sources.iter().all(|(id, binding)| {
            state
                .sources
                .values()
                .find(|s| &s.id == id)
                .is_some_and(|s| {
                    source_in_task(s, &self.request)
                        && source_binding(s).is_ok_and(|b| &b == binding)
                })
        });
        let valid_records = self.records.iter().all(|(id, (binding, evidence))| {
            state.repository.get(*id).ok().flatten().is_some_and(|m| {
                record_in_scope(state, &m, &self.request)
                    && record_binding(&m) == *binding
                    && evidence.iter().all(|e| m.evidence.contains(e))
            })
        });
        let valid_entities = self.entities.iter().all(|(id, binding)| {
            state.entities.get(id).is_some_and(|e| {
                e.owner_id == *owner
                    && crate::openai::recall_models::fingerprint(&serde_json::json!(e)) == *binding
            })
        });
        if !valid_sources || !valid_records || !valid_entities {
            return Err("recall_snapshot_changed".into());
        }
        Ok(())
    }
}

/// 离线检索诊断；独立于冻结证据结构，普通 assemble 不收集。
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct RetrievalDiagnostic {
    pub stages: BTreeMap<String, Vec<MemoryId>>,
    pub raw_stages: BTreeMap<String, Vec<String>>,
    pub relation_stages: BTreeMap<String, Vec<String>>,
    pub requested_relation: Option<String>,
    pub effective_relation: Option<String>,
    pub relation_revoked: bool,
    pub enumeration: Option<String>,
    pub fts_called: bool,
    pub fts_error: bool,
    pub ranking: Vec<RetrievalRank>,
    pub lexical_df: Option<crate::LexicalDf>,
    pub lexical_ranked: Vec<MemoryId>,
    pub vector_ranked: Vec<MemoryId>,
    pub fused_ranked: Vec<MemoryId>,
    pub selection: BTreeMap<MemoryId, Vec<String>>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct RetrievalRank {
    pub id: MemoryId,
    pub entity_hits: usize,
    pub phrase_hits: usize,
    pub received_sequence: u64,
}

fn diagnostic_stage(
    diagnostic: &mut Option<RetrievalDiagnostic>,
    name: &str,
    ids: impl Iterator<Item = MemoryId>,
) {
    if let Some(d) = diagnostic {
        d.stages.insert(name.into(), ids.collect());
    }
}

fn diagnostic_raw(diagnostic: &mut Option<RetrievalDiagnostic>, name: &str, raw: &[RawEvidence]) {
    if let Some(d) = diagnostic {
        d.raw_stages.insert(
            name.into(),
            raw.iter().map(|r| r.source_id.clone()).collect(),
        );
    }
}

/// R4 的注入位置：消费本次组装，沿用原请求、R1 标记与读快照，不另做一次检索。
pub struct Assembly {
    retrieval_diagnostic: Option<RetrievalDiagnostic>,
    pub request: RecallRequest,
    pub plan: r1::Plan,
    pub evidence: RecallEvidence,
    pub snapshot: RecallSnapshot,
    pub retrieval: RecallRetrieval,
    pub(crate) safety: SafetySnapshot,
    utc_offset_seconds: i32,
    source_dates: BTreeMap<String, Option<String>>,
    pub source_context: BTreeMap<String, serde_json::Value>,
    entity_names: BTreeMap<String, String>,
    history_until: BTreeMap<MemoryId, String>,
    // 仅保留未覆盖来源的可靠槽位；None 表示无法证明它与某条事实无关。
    uncovered_sources: Vec<Option<(String, String)>>,
}

impl Assembly {
    pub fn retrieval_diagnostic(&self) -> Option<&RetrievalDiagnostic> {
        self.retrieval_diagnostic.as_ref()
    }

    /// 人工离线输出与后续模型提供者共用此入口；生产不读取评测夹具。
    pub fn finish_r4(
        self,
        store: &SqliteMemoryStore,
        response: Result<&str, JudgmentFailure>,
    ) -> PersistenceResult<RecallOutput> {
        self.finish_r4_with_audit(store, response)
            .map(|(output, _)| output)
    }

    /// 与模型运行时共用补入/核验边界；审计只在内存保留。
    pub fn finish_r4_with_audit(
        self,
        store: &SqliteMemoryStore,
        response: Result<&str, JudgmentFailure>,
    ) -> PersistenceResult<(RecallOutput, Option<r4::OutputAudit>)> {
        if self.plan.need() == R1Need::ContextOnly {
            return self
                .finish_skipped(store, SkipReason::ContextOnly)
                .map(|out| (out, None));
        }
        let source_state = store.read()?;
        let context = r4::Context {
            evidence: &self.evidence,
            plan: &self.plan,
            request: &self.request,
            utc_offset_seconds: self.utc_offset_seconds,
            source_dates: &self.source_dates,
        };
        let mut audit = response.ok().map(|text| r4::OutputAudit {
            raw_output: text.into(),
            final_output: None,
        });
        let result = response.and_then(|text| {
            let raw = crate::diagnostics::checked("R4", "decode", r4::decode(text))
                .map_err(|_| JudgmentFailure::InvalidOutput)?;
            let final_output = r4::assemble_old_basis(&raw, &context, &|id| {
                source_state
                    .sources
                    .values()
                    .find(|source| source.id == id)
                    .filter(|source| source.enrichment != "deleted")
                    .and_then(crate::lexical::body)
                    .map(str::to_owned)
            });
            audit.as_mut().unwrap().final_output = Some(final_output.clone());
            let evaluated = r4::evaluate_detailed(final_output, &context);
            crate::diagnostics::record("model_check", || {
                let mut event = serde_json::json!({"stage":"R4","phase":"validate",
                    "status":if evaluated.is_ok() {"completed"} else {"failed"},
                    "error":evaluated.as_ref().err().map(|d| d.error.to_string())});
                if let Err(diagnostic) = &evaluated {
                    event["field"] = serde_json::json!(diagnostic.field);
                    event["rule"] = serde_json::json!(diagnostic.error.0);
                    event["cause"] = serde_json::json!(diagnostic.cause);
                }
                event
            });
            let evaluated = evaluated.map_err(|_| JudgmentFailure::InvalidOutput)?;
            audit.as_mut().unwrap().final_output = Some(evaluated.output.clone());
            Ok(evaluated)
        });
        let mut reasons: Vec<String> = self
            .plan
            .degraded_reasons()
            .iter()
            .map(|s| (*s).into())
            .collect();
        let (status, facts, computed, history, conflicts, clarification, semantic) = match result {
            Err(error) => {
                reasons.push(error.code().into());
                (
                    RecallStatus::Degraded,
                    vec![],
                    None,
                    vec![],
                    self.evidence.relations.clone(),
                    self.plan
                        .needs_clarification()
                        .then(|| self.clarification(&[])),
                    SemanticJudgment::Failed,
                )
            }
            Ok(evaluated) => {
                let output = evaluated.output;
                let mut visibility_incomplete = false;
                let facts: Vec<_> = output
                    .facts
                    .into_iter()
                    .map(|f| {
                        let (fact, incomplete) = self.fact(f);
                        visibility_incomplete |= incomplete;
                        fact
                    })
                    .collect();
                let conflicts: Vec<_> = self
                    .evidence
                    .relations
                    .iter()
                    .filter(|link| {
                        link.status == crate::contract::RelationStatus::Open
                            && !resolved_here(link, &facts)
                    })
                    .cloned()
                    .collect();
                // R1 is preliminary; a fully validated R4 decides semantic clarification.
                // Skipped/failed judgment still uses R1 in the conservative fallback below.
                let clarification_needed = output.verdict == R4Verdict::NeedsClarification;
                let insufficient = evaluated.compute_insufficient
                    || visibility_incomplete
                    || output.verdict == R4Verdict::Insufficient
                    || (facts.is_empty() && evaluated.computed.is_none());
                if evaluated.compute_insufficient {
                    reasons.push("compute_insufficient".into());
                }
                let status = if clarification_needed {
                    RecallStatus::NeedsClarification
                } else if output.verdict == R4Verdict::Conflict || !conflicts.is_empty() {
                    RecallStatus::Conflict
                } else if insufficient {
                    RecallStatus::Insufficient
                } else {
                    RecallStatus::Answered
                };
                let history = facts.iter().filter_map(|f| self.history(f)).collect();
                let clarification = clarification_needed.then(|| self.clarification(&facts));
                (
                    status,
                    facts,
                    evaluated.computed,
                    history,
                    conflicts,
                    clarification,
                    SemanticJudgment::Model,
                )
            }
        };
        // 包含计算/判断所用来源日期与终止元数据的快照也必须仍有效。
        self.check_snapshot(store)?;
        Ok((
            RecallOutput {
                version: "recall.v11".into(),
                request: self.request,
                status,
                current_message: self.evidence.current_message,
                records: self.evidence.records,
                raw: self.evidence.raw,
                facts,
                computed,
                history,
                conflicts,
                clarification,
                semantic_judgment: semantic,
                completeness: self.evidence.completeness,
                retrieval: self.retrieval,
                degraded_reasons: reasons,
                snapshot: self.snapshot,
                statement_snapshot: None,
            },
            audit,
        ))
    }

    fn fact(&self, f: FactView) -> (RecallFact, bool) {
        let related: Vec<_> = self
            .evidence
            .records
            .iter()
            .filter(|r| f.record_ids.contains(&r.id()) || f.supersedes_record_ids.contains(&r.id()))
            .collect();
        // 无法定位的遗漏可能改变当前结论，不能用词法不命中证明无关。
        // 仅有截断标记、或完整可见的 queued/failed 原文，不触发此门控。
        let slots: Option<BTreeSet<_>> = (!related.is_empty())
            .then(|| related.iter().map(|r| record_slot(r)).collect())
            .flatten();
        let visibility_incomplete = f.current == Qualification::Yes
            && self
                .uncovered_sources
                .iter()
                .any(|source_slots| match (source_slots, &slots) {
                    (Some(source), Some(fact)) => fact.contains(source),
                    _ => true,
                });
        let grade = if f.evidence.iter().all(|e|
            !self.evidence.raw.iter().any(|raw| matches!(&e.source, SourceRef::Stored {source_id} if source_id == &raw.source_id))
            && !f.record_ids.is_empty()
            && related.iter().any(|r| f.record_ids.contains(&r.id()) && r4::supports_record(e, r)))
            { RecallGrade::Committed } else { RecallGrade::Uncommitted };
        let mut pending = Vec::new();
        let mut has_pending = false;
        let mut unknown = related.is_empty();
        for r in &related {
            let update = match r {
                RecordView::Current(v) => Some(&v.update),
                RecordView::Legacy(v) => v.update.as_ref(),
            };
            match update {
                Some(u) if u.status == UpdateStatus::Pending => {
                    has_pending = true;
                    for reason in &u.reasons {
                        if !pending.contains(reason) {
                            pending.push(*reason);
                        }
                    }
                }
                None => unknown = true,
                _ => (),
            }
        }
        unknown |= f
            .evidence
            .iter()
            .any(|e| !related.iter().any(|r| r4::supports_record(e, r)));
        let update = if has_pending {
            Some(UpdateState {
                status: UpdateStatus::Pending,
                reasons: pending,
            })
        } else if unknown {
            None
        } else {
            Some(UpdateState {
                status: UpdateStatus::Judged,
                reasons: vec![],
            })
        };
        (
            RecallFact {
                statement: f.statement,
                evidence: f.evidence,
                record_ids: f.record_ids,
                polarity: f.polarity,
                strength: f.strength,
                uncertainty: f.uncertainty,
                conditions: f.conditions,
                times: f.times,
                current: if visibility_incomplete {
                    Qualification::Unknown
                } else {
                    f.current
                },
                applies: f.applies,
                supersedes_record_ids: f.supersedes_record_ids,
                grade,
                update,
            },
            visibility_incomplete,
        )
    }

    fn history(&self, fact: &RecallFact) -> Option<RecallHistory> {
        if fact.current != Qualification::No {
            return None;
        }
        let ended: Vec<_> = fact
            .record_ids
            .iter()
            .filter_map(|id| self.evidence.records.iter().find(|r| r.id() == *id))
            .filter(|r| match r {
                RecordView::Current(v) => {
                    v.stored_status == StoredStatus::Superseded
                        && matches!(
                            v.termination_reason,
                            Some(
                                crate::SupersedeReason::StateChanged
                                    | crate::SupersedeReason::PreferenceChanged
                                    | crate::SupersedeReason::ConstraintChanged
                            )
                        )
                }
                RecordView::Legacy(v) => {
                    v.data.status == MemoryStatus::Superseded
                        && matches!(
                            v.data.supersede_reason,
                            Some(
                                crate::SupersedeReason::StateChanged
                                    | crate::SupersedeReason::PreferenceChanged
                                    | crate::SupersedeReason::ConstraintChanged
                            )
                        )
                }
            })
            .collect();
        // 同一概括混入纠错/撤回等旧值时，不把它包装为曾经成立的历史事实。
        if ended.is_empty() || ended.len() != fact.record_ids.len() {
            return None;
        }
        let dates: Vec<_> = ended
            .iter()
            .map(|r| self.history_until.get(&r.id()))
            .collect();
        let until = dates
            .first()
            .and_then(|d| *d)
            .filter(|first| dates.iter().all(|d| *d == Some(*first)))
            .cloned();
        Some(RecallHistory {
            statement: fact.statement.clone(),
            until,
            evidence: fact.evidence.clone(),
        })
    }

    fn clarification(&self, facts: &[RecallFact]) -> RecallClarification {
        let mut evidence = Vec::new();
        let question = match self.plan.output.as_ref().map(|o| &o.subject) {
            Some(R1Subject::Ambiguous(s)) => {
                let names: Vec<_> = s
                    .ambiguous
                    .iter()
                    .map(|id| {
                        self.entity_names
                            .get(id)
                            .map_or(id.as_str(), String::as_str)
                    })
                    .collect();
                format!("你指的是{}中的哪一位？", names.join("、"))
            }
            Some(R1Subject::Unresolved(s)) => format!("“{}”指的是谁？", s.unresolved.text),
            _ => {
                for fact in facts {
                    for e in &fact.evidence {
                        if !evidence.contains(e) {
                            evidence.push(e.clone());
                        }
                    }
                }
                "请明确要查询的对象或条件。".into()
            }
        };
        if matches!(
            self.plan.output.as_ref().map(|o| &o.subject),
            Some(R1Subject::Ambiguous(_) | R1Subject::Unresolved(_))
        ) {
            // 澄清依据引用触发歧义的原话，query_only 仍不能成为 FactView/ComputeItem 证据。
            if let Some(current) = &self.evidence.current_message {
                let text = match self.plan.output.as_ref().map(|o| &o.subject) {
                    Some(R1Subject::Unresolved(s))
                        if locate_quote(&current.text, &s.unresolved.text).is_some() =>
                    {
                        s.unresolved.text.clone()
                    }
                    Some(R1Subject::Unresolved(_)) => {
                        return RecallClarification { question, evidence };
                    }
                    _ => current.text.clone(),
                };
                evidence.push(EvidenceRef {
                    source: SourceRef::Current {},
                    quote: Quote { text },
                });
            }
        }
        RecallClarification { question, evidence }
    }

    pub fn review_current_message(&self) -> bool {
        self.evidence.current_message.is_some() && self.plan.review_current_message()
    }

    pub fn has_judgment_evidence(&self) -> bool {
        self.review_current_message()
            || !self.evidence.records.is_empty()
            || !self.evidence.raw.is_empty()
    }

    /// 返回前重查内容仍获准可见；进度或普通提交不要求与最新版本相等。
    pub fn check_snapshot(&self, store: &SqliteMemoryStore) -> PersistenceResult<()> {
        self.safety.check(store.owner(), &store.read()?)
    }

    pub fn finish_skipped(
        self,
        store: &SqliteMemoryStore,
        reason: SkipReason,
    ) -> PersistenceResult<RecallOutput> {
        self.check_snapshot(store)?;
        let context_only = self.plan.need() == R1Need::ContextOnly;
        let reason = if context_only {
            SkipReason::ContextOnly
        } else {
            reason
        };
        let mut degraded_reasons: Vec<String> = self
            .plan
            .degraded_reasons()
            .iter()
            .map(|reason| (*reason).into())
            .collect();
        degraded_reasons.push(format!("semantic_judgment_skipped:{}", reason.code()));
        // R4 未核验时保留初步澄清；离线/预算等降级状态不被提示覆盖。
        let clarification =
            (!context_only && self.plan.needs_clarification()).then(|| self.clarification(&[]));
        Ok(RecallOutput {
            version: "recall.v11".into(),
            request: self.request,
            status: if context_only {
                RecallStatus::ContextOnly
            } else if reason == SkipReason::NoEvidence && clarification.is_some() {
                RecallStatus::NeedsClarification
            } else if reason == SkipReason::NoEvidence
                && self.evidence.completeness.plan_complete
                && !self.evidence.completeness.retrieval_truncated
            {
                RecallStatus::Insufficient
            } else {
                RecallStatus::Degraded
            },
            current_message: self.evidence.current_message,
            records: self.evidence.records,
            raw: self.evidence.raw,
            facts: Vec::new(),
            computed: None,
            history: Vec::new(),
            // 原有关系的审计视图，不新增本轮冲突判断。
            conflicts: self.evidence.relations,
            clarification,
            semantic_judgment: SemanticJudgment::Skipped,
            completeness: self.evidence.completeness,
            retrieval: self.retrieval,
            degraded_reasons,
            snapshot: self.snapshot,
            statement_snapshot: None,
        })
    }
}

// 仅本轮消解：同一个有新旧依据的当前事实取代全部冲突成员（或保留唯一成员）。
// RelationLink 的持久状态原样呈现，不写 resolution 或变更存储。
fn resolved_here(link: &RelationLink, facts: &[RecallFact]) -> bool {
    facts.iter().any(|f| {
        link.members.len() >= 2
            && f.current == Qualification::Yes
            && !f.supersedes_record_ids.is_empty()
            && link
                .members
                .iter()
                .filter(|id| !f.supersedes_record_ids.contains(id))
                .count()
                <= 1
            && link
                .members
                .iter()
                .all(|id| f.supersedes_record_ids.contains(id) || f.record_ids.contains(id))
            && facts.iter().all(|other| {
                other == f
                    || other.current == Qualification::No
                    || other
                        .record_ids
                        .iter()
                        .all(|id| !f.supersedes_record_ids.contains(id))
            })
    })
}

pub fn run_offline(
    store: &SqliteMemoryStore,
    request: &Request,
    plan: &r1::Plan,
    budget: Budget,
) -> PersistenceResult<RecallOutput> {
    let assembly = assemble(store, request, plan, budget)?;
    let reason = if assembly.has_judgment_evidence() {
        SkipReason::Offline
    } else {
        SkipReason::NoEvidence
    };
    assembly.finish_skipped(store, reason)
}

fn record_slot(record: &RecordView) -> Option<(String, String)> {
    match record {
        RecordView::Current(v) => match &v.candidate.subject {
            Subject::Entity(subject) => {
                Some((subject.entity_id.clone(), v.candidate.relation.clone()))
            }
            Subject::Unresolved(_) => None,
        },
        RecordView::Legacy(v) => Some((v.data.subject_id.0.clone(), v.data.predicate.0.clone())),
    }
}

pub(crate) fn source_in_task(source: &SourceRecord, request: &Request) -> bool {
    source.enrichment != "deleted"
        && crate::lexical::body(source).is_some()
        && crate::lexical::task(source)
            .is_none_or(|task| Some(task) == request.binding.task_id.as_deref())
}

fn sources_of<'a>(state: &'a PersistedMemoryState, memory: &Memory) -> Vec<&'a SourceRecord> {
    state
        .sources
        .values()
        .filter(|s| {
            memory
                .evidence
                .iter()
                .any(|e| e.session_id.0 == s.session_id && e.turn_id.0 == s.turn_id)
        })
        .collect()
}

pub(crate) fn record_in_scope(
    state: &PersistedMemoryState,
    memory: &Memory,
    request: &Request,
) -> bool {
    let sources = sources_of(state, memory);
    let origin_allowed =
        crate::w5_record_contract(state, memory.id).is_none_or(|c| match c.source {
            SourceRef::Stored { source_id } => state
                .sources
                .values()
                .find(|source| source.id == source_id)
                .is_some_and(|s| source_in_task(s, request)),
            SourceRef::Current {} => false,
        });
    memory.owner_id == request.owner
        && memory.status != MemoryStatus::Deleted
        && request
            .subject_ids
            .as_ref()
            .is_none_or(|ids| ids.contains(&memory.subject_id))
        && memory.scope.conditions.iter().all(|c| match c {
            ScopeCondition::Task(task) => Some(task.0.as_str()) == request.binding.task_id.as_deref(),
            _ => true,
        })
        // 无独立来源的旧事实仍允许 legacy；已有来源不可跨任务借用原文。
        && (sources.is_empty() || sources.iter().any(|s| source_in_task(s, request)))
        && origin_allowed
}

pub(crate) fn projection(state: &PersistedMemoryState, id: MemoryId) -> Option<RecordView> {
    match crate::w4::record_view(state, id)? {
        RecordView2::Legacy(view) => Some(RecordView::Legacy(view)),
        RecordView2::Current(view) => Some(RecordView::Current(CurrentRecordView {
            id: view.id,
            stored_status: view.stored_status,
            termination_reason: view.termination_reason,
            ended: view.ended,
            candidate: view.candidate,
            evidence: view.evidence,
            grade: view.grade,
            said_on: view.said_on,
            time_resolutions: view.time_resolutions,
            history: view.history,
            open_relations: view.open_relations,
            update: view.update,
        })),
    }
}

pub(crate) fn enrichment(source: &SourceRecord) -> Option<EnrichmentStatus> {
    use EnrichmentStatus::*;
    match source.enrichment.as_str() {
        "stale_requires_review" => Some(NeedsRejudge),
        "completed_pending" => Some(Pending),
        text => match serde_json::from_value::<EnrichmentStatus>(serde_json::json!(text)) {
            Ok(status) if !matches!(status, Deleted | NotStored) => Some(status),
            // 冻结枚举没有唯一对应项：不自行把拒收/未知状态解释成 failed 或成功。
            _ => None,
        },
    }
}

fn requests(source: &SourceRecord) -> PersistenceResult<Vec<MemoryRequest>> {
    crate::enrichment::visible_requests(source)?
        .into_iter()
        .map(|request| {
            // 3.2 的 MemoryRequest 不带 3.5 回执的派生字段；控制记录仍优先于重提取。
            Ok(serde_json::from_value(serde_json::json!({
                "kind":request["kind"], "quote":request["quote"],
                "target":request["target"], "deadline":request["deadline"]
            }))?)
        })
        .collect()
}

// 来源状态/控制回执的改变不一定增加 source_revision（它主要绑定正文索引）。
// 仅用于进度诊断/节奏测量；安全有效性由 SafetySnapshot 检查，不拿进度作失效条件。
pub(crate) fn source_metadata(
    state: &PersistedMemoryState,
) -> PersistenceResult<Vec<(String, String, Vec<MemoryRequest>)>> {
    state
        .sources
        .values()
        .filter(|s| crate::lexical::body(s).is_some())
        .map(|s| Ok((s.id.clone(), s.enrichment.clone(), requests(s)?)))
        .collect()
}

fn phrase_score(text: &str, phrases: &[String]) -> usize {
    let words = format!(" {} ", crate::lexical::tokens(text).join(" "));
    phrases
        .iter()
        .filter(|p| words.contains(&format!(" {p} ")))
        .count()
}

fn local_date(at: i64, offset: time::UtcOffset) -> PersistenceResult<String> {
    Ok(time::OffsetDateTime::from_unix_timestamp(at)?
        .to_offset(offset)
        .date()
        .to_string())
}

fn clipped_prefix(text: &str, bytes: usize) -> &str {
    let mut end = text.len().min(bytes);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn fit_raw(raw: &mut RawEvidence, bytes: usize) -> PersistenceResult<bool> {
    if serde_json::to_vec(raw)?.len() <= bytes {
        return Ok(true);
    }
    let text = std::mem::take(&mut raw.quote.text);
    // 空串的元数据也放不下时整项省略；按序列化后的长度计转义字符，而非原文字节。
    if serde_json::to_vec(raw)?.len() >= bytes {
        return Ok(false);
    }
    let (mut low, mut high) = (0, text.len());
    while low < high {
        let mid = low + (high - low).div_ceil(2);
        raw.quote.text = clipped_prefix(&text, mid).into();
        if serde_json::to_vec(raw)?.len() <= bytes {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    raw.quote.text = clipped_prefix(&text, low).into();
    Ok(!raw.quote.text.is_empty() || text.is_empty())
}

fn source_date(
    source: &SourceRecord,
    offset: time::UtcOffset,
) -> PersistenceResult<Option<String>> {
    if source.is_history_import() {
        source.said_on(offset.whole_seconds())
    } else {
        Ok(Some(local_date(source.observed_at, offset)?))
    }
}

/// 全部数据与索引查询共用 tx；不调用旧召回，也不增加 retrieval_count 或写入来源。
pub fn assemble(
    store: &SqliteMemoryStore,
    request: &Request,
    plan: &r1::Plan,
    budget: Budget,
) -> PersistenceResult<Assembly> {
    assemble_inner(store, request, plan, budget, false, None)
}

/// 基线专用观察入口；不改变排序、过滤、裁剪或证据输出。
pub fn assemble_diagnosed(
    store: &SqliteMemoryStore,
    request: &Request,
    plan: &r1::Plan,
    budget: Budget,
) -> PersistenceResult<Assembly> {
    assemble_inner(store, request, plan, budget, true, None)
}

/// Injection seam for offline replay; the production callback never reads recordings.
pub struct Retrieval<'a> {
    pub config: &'a crate::embedding::Config,
    pub query: Option<&'a dyn Fn(&str) -> crate::embedding::Result<crate::embedding::QueryVector>>,
}
pub fn assemble_with_retrieval(
    store: &SqliteMemoryStore,
    request: &Request,
    plan: &r1::Plan,
    budget: Budget,
    retrieval: &Retrieval<'_>,
    diagnose: bool,
) -> PersistenceResult<Assembly> {
    retrieval.config.validate()?;
    assemble_inner(store, request, plan, budget, diagnose, Some(retrieval))
}

fn rrf(lexical: &[MemoryId], vector: &[(MemoryId, f64)], seeds: usize) -> BTreeMap<MemoryId, f64> {
    let mut scores = BTreeMap::new();
    for ids in [lexical.to_vec(), vector.iter().map(|r| r.0).collect()] {
        for (rank, id) in ids.into_iter().take(seeds).enumerate() {
            *scores.entry(id).or_default() += 1.0 / (60.0 + rank as f64 + 1.0);
        }
    }
    scores
}

fn assemble_inner(
    store: &SqliteMemoryStore,
    request: &Request,
    plan: &r1::Plan,
    budget: Budget,
    diagnose: bool,
    retrieval: Option<&Retrieval<'_>>,
) -> PersistenceResult<Assembly> {
    let mut diagnostic = diagnose.then(RetrievalDiagnostic::default);
    if store.owner() != &request.owner
        || request.binding.request_id.trim().is_empty()
        || crate::temporal::parse_iso_date(&request.said_on).is_none()
        || request
            .binding
            .task_id
            .as_ref()
            .is_some_and(|t| t.trim().is_empty())
        || request.binding.query != plan.query
        || request.binding.context != plan.context
        || request.binding.task_id.as_deref() != plan.task_id.as_ref().map(|t| t.0.as_str())
        || request.subject_ids != plan.subject_ids
    {
        return Err("invalid_recall_context".into());
    }
    let offset = time::UtcOffset::from_whole_seconds(request.utc_offset_seconds)?;
    let tx = store.connection.unchecked_transaction()?;
    let state = SqliteMemoryStore::load(&tx, store.owner())?;
    let all = state.repository.list_by_owner(store.owner())?;
    let mut source_links: BTreeMap<&str, Vec<&Memory>> = BTreeMap::new();
    for memory in &all {
        for source in sources_of(&state, memory) {
            source_links.entry(&source.id).or_default().push(memory);
        }
    }
    let eligible: BTreeMap<_, _> = all
        .iter()
        .filter(|m| record_in_scope(&state, m, request))
        .map(|m| (m.id, m))
        .collect();
    diagnostic_stage(&mut diagnostic, "owner", all.iter().map(|m| m.id));
    diagnostic_stage(&mut diagnostic, "trusted_scope", eligible.keys().copied());
    let entities = state.entities.list_by_owner(store.owner());
    let mut hint = std::iter::once(request.binding.query.as_str())
        .chain(request.binding.context.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ");
    if let Some(output) = &plan.output {
        hint.push(' ');
        hint.push_str(&output.relation);
        for condition in &output.conditions {
            hint.push(' ');
            hint.push_str(&condition.quote.text);
        }
    }
    let phrases = crate::lexical::natural_phrases(&hint, &entities);
    let entity_hints = crate::lexical::entity_hints(&hint, &entities);
    let enumerate = plan.output.as_ref().is_none_or(|o| {
        o.query_type == R1Type::Aggregate
            || matches!(o.plan.op, R1Op::Count | R1Op::Sum | R1Op::List)
    });
    let enabled = retrieval.is_some_and(|r| r.config.enabled);
    let mut relation = plan
        .output
        .as_ref()
        .and_then(|o| o.plan.filter.relation.as_deref());
    let subject = plan.output.as_ref().and_then(|o| match &o.subject {
        R1Subject::Entity(s) => Some(s.entity_id.as_str()),
        _ => None,
    });
    let mentioned_entity = subject
        .filter(|id| *id != "self")
        .and_then(|id| entities.iter().find(|e| e.id.0 == id));
    let mentions: BTreeSet<_> = eligible
        .values()
        .filter(|m| {
            mentioned_entity.is_some_and(|e| {
                m.subject_id != e.id
                    && (match &m.value {
                        crate::MemoryValue::Entity(id) => *id == e.id,
                        crate::MemoryValue::Text(text) => crate::lexical::mentions_entity(text, e),
                        _ => false,
                    } || m
                        .evidence
                        .iter()
                        .any(|v| crate::lexical::mentions_entity(&v.quote, e)))
            })
        })
        .map(|m| m.id)
        .collect();
    diagnostic_stage(
        &mut diagnostic,
        "r1_subject_mentions",
        mentions.iter().copied(),
    );
    let scoped: Vec<_> = eligible
        .values()
        .copied()
        .filter(|m| subject.is_none_or(|id| m.subject_id.0 == id) || mentions.contains(&m.id))
        .collect();
    diagnostic_stage(&mut diagnostic, "r1_subject", scoped.iter().map(|m| m.id));
    if let Some(d) = &mut diagnostic {
        d.requested_relation = relation.map(str::to_owned);
    }
    let relevance = |m: &Memory| {
        let text = m
            .evidence
            .iter()
            .map(|e| e.quote.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        phrase_score(&text, &phrases)
    };
    // Enumerate the whole subject scope, never just the model-selected relation.
    let small_scope = enabled && scoped.len() <= budget.records;
    if small_scope {
        relation = None;
    }
    if let Some(r) = relation {
        let matching: Vec<_> = scoped.iter().filter(|m| m.predicate.0 == r).collect();
        let best = matching.iter().map(|m| relevance(m)).max().unwrap_or(0);
        // 关系是模型的检索提示，不是授权边界。空结果或更强的范围内原文命中说明过滤不可靠。
        if matching.is_empty()
            || scoped
                .iter()
                .any(|m| m.predicate.0 != r && relevance(m) > best)
        {
            relation = None;
        }
    }
    let mut candidates: Vec<_> = scoped
        .iter()
        .copied()
        .filter(|m| relation.is_none_or(|r| m.predicate.0 == r))
        .collect();
    diagnostic_stage(
        &mut diagnostic,
        "relation_filter",
        candidates.iter().map(|m| m.id),
    );
    if let Some(d) = &mut diagnostic {
        d.effective_relation = relation.map(str::to_owned);
        d.relation_revoked = d.requested_relation.is_some() && relation.is_none();
        d.enumeration = if plan.output.is_none() {
            Some("r1_not_understood".into())
        } else if enumerate {
            Some("aggregate_or_list".into())
        } else if relation.is_none() && candidates.len() <= budget.records {
            Some("scope_within_record_budget".into())
        } else {
            None
        };
    }
    // 小范围实际穷举；大范围仍按词法取候选，不能据 FTS 未截断宣称完整。
    let enumerate = enumerate || (relation.is_none() && candidates.len() <= budget.records);
    let hybrid = enabled && !enumerate && !request.bootstrap;
    if hybrid {
        // Relation is a hint, never a reserved seat or lexical vote in the non-enumeration path.
        relation = None;
        candidates = scoped.clone();
        diagnostic_stage(
            &mut diagnostic,
            "relation_filter",
            candidates.iter().map(|m| m.id),
        );
        if let Some(d) = &mut diagnostic {
            d.effective_relation = None;
            d.relation_revoked = d.requested_relation.is_some();
        }
    }
    let df_sources: Vec<_> = if hybrid {
        state
            .sources
            .values()
            .filter(|s| {
                source_in_task(s, request)
                    && !source_links
                        .get(s.id.as_str())
                        .into_iter()
                        .flatten()
                        .any(|m| m.status != MemoryStatus::Deleted && !eligible.contains_key(&m.id))
                    && (request.subject_ids.is_none()
                        || source_links.get(s.id.as_str()).is_some_and(|links| {
                            links.iter().any(|m| eligible.contains_key(&m.id))
                        }))
            })
            .collect()
    } else {
        Vec::new()
    };
    let df = hybrid.then(|| {
        crate::LexicalDf::new(
            &hint,
            &entities,
            df_sources.iter().map(|s| crate::lexical::body(s).unwrap()),
        )
    });
    let vector_sources: Vec<_> = df_sources
        .iter()
        .copied()
        .filter(|s| {
            source_links.get(s.id.as_str()).is_none_or(|links| {
                !links.iter().any(|m| m.status != MemoryStatus::Deleted)
                    || links
                        .iter()
                        .any(|m| candidates.iter().any(|c| c.id == m.id))
            })
        })
        .collect();
    let cache_started = std::time::Instant::now();
    let cache = if hybrid {
        Some(crate::embedding_index::read_snapshot(
            &tx,
            store.owner(),
            &vector_sources,
            retrieval.unwrap().config.model,
        ))
    } else {
        None
    };
    let cache_elapsed_ms = cache_started.elapsed().as_millis();
    // FTS 只是候选提示；旧来源无索引时用同一快照的完整值/片段兜底，不据此认证语义。
    let hits = if enumerate || hybrid {
        BTreeSet::new()
    } else {
        if let Some(d) = &mut diagnostic {
            d.fts_called = true;
        }
        let result = store.search_w4_facts(&tx, &state, &hint, &candidates);
        if let Some(d) = &mut diagnostic {
            d.fts_error = result.is_err();
        }
        result.unwrap_or_default().into_iter().collect()
    };
    diagnostic_stage(&mut diagnostic, "fts_hits", hits.iter().copied());
    // All source bodies, cached vectors, FTS hits and record mappings share this R3 snapshot.
    tx.commit()?;
    let mut metadata = RecallRetrieval {
        status: if enabled {
            RetrievalStatus::NotNeeded
        } else {
            RetrievalStatus::Disabled
        },
        ..Default::default()
    };
    let mut vector_rank = Vec::new();
    if hybrid {
        metadata.sources_total = vector_sources.len();
        metadata.segments_total = vector_sources
            .iter()
            .map(|s| crate::embedding::segments(crate::lexical::body(s).unwrap()).len())
            .sum();
        let query_started = std::time::Instant::now();
        let query = retrieval
            .unwrap()
            .query
            .ok_or_else(|| "embedding_network_disabled".to_owned())
            .and_then(|query| query(&request.binding.query))
            .and_then(|q| {
                q.check(retrieval.unwrap().config.model)?;
                Ok(q)
            });
        let query_elapsed_ms = query_started.elapsed().as_millis();
        metadata.status = RetrievalStatus::Unavailable;
        if let Some(Ok(cache)) = &cache {
            metadata.segments_indexed = cache.vectors.values().map(Vec::len).sum();
            metadata.sources_indexed = cache
                .totals
                .iter()
                .filter(|(id, total)| cache.vectors.get(*id).is_some_and(|v| v.len() == **total))
                .count();
            if let Ok(query) = &query {
                if cache.binding.as_ref() == Some(&query.binding) {
                    let mut record_scores: BTreeMap<MemoryId, f64> = BTreeMap::new();
                    for (id, vectors) in &cache.vectors {
                        let source = vector_sources.iter().find(|s| s.id == *id).unwrap();
                        let body = crate::lexical::body(source).unwrap();
                        for v in vectors {
                            let score = v
                                .vector
                                .iter()
                                .zip(&query.vector)
                                .map(|(a, b)| f64::from(*a) * f64::from(*b))
                                .sum::<f64>();
                            for memory in source_links
                                .get(id.as_str())
                                .into_iter()
                                .flatten()
                                .filter(|m| candidates.iter().any(|c| c.id == m.id))
                            {
                                let overlaps = memory
                                    .evidence
                                    .iter()
                                    .filter(|e| {
                                        e.session_id.0 == source.session_id
                                            && e.turn_id.0 == source.turn_id
                                    })
                                    .filter_map(|e| locate_quote(body, &e.quote))
                                    .any(|q| v.start < q.end && q.start < v.end);
                                if overlaps {
                                    record_scores
                                        .entry(memory.id)
                                        .and_modify(|n| *n = n.max(score))
                                        .or_insert(score);
                                }
                            }
                        }
                    }
                    vector_rank = record_scores.into_iter().collect();
                    vector_rank.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
                } else {
                    metadata.sources_indexed = 0;
                    metadata.segments_indexed = 0;
                }
                metadata.status = if cache.binding.as_ref() == Some(&query.binding)
                    && metadata.segments_indexed == metadata.segments_total
                {
                    RetrievalStatus::Ready
                } else {
                    RetrievalStatus::Pending
                };
            }
        }
        crate::diagnostics::record("retrieval", || {
            serde_json::json!({
                "status": metadata.status,
                "query_error": query.as_ref().err().map(|e| retrieval_error(e)),
                "index_state": match &cache {
                    Some(Err(_)) => "index_read",
                    Some(Ok(c)) if c.binding.is_none() => "missing",
                    Some(Ok(c)) if query.as_ref().is_ok_and(|q| c.binding.as_ref() != Some(&q.binding)) => "binding_mismatch",
                    _ if metadata.segments_indexed < metadata.segments_total => "partial",
                    _ => "ready",
                },
                "query_elapsed_ms": query_elapsed_ms,
                "cache_elapsed_ms": cache_elapsed_ms,
                "sources_total": metadata.sources_total,
                "sources_indexed": metadata.sources_indexed,
                "segments_total": metadata.segments_total,
                "segments_indexed": metadata.segments_indexed,
            })
        });
    }
    let mut scores = BTreeMap::new();
    let mut selected = BTreeSet::new();
    for m in candidates {
        let text = std::iter::once(serde_json::to_string(&m.value)?)
            .chain(m.evidence.iter().map(|e| e.quote.clone()))
            .collect::<Vec<_>>()
            .join(" ");
        let score = df
            .as_ref()
            .map_or_else(|| phrase_score(&text, &phrases), |df| df.score(&text).1);
        let entity_score = if let Some(df) = &df {
            df.score(&text).0
        } else {
            entity_hints
                .values()
                .filter(|names| crate::lexical::matches_phrases(&text, names))
                .count()
        };
        scores.insert(m.id, (entity_score, score));
        let source_hit = df.as_ref().is_some_and(|df| {
            sources_of(&state, m).iter().any(|s| {
                if !df_sources.iter().any(|d| d.id == s.id) {
                    return false;
                }
                let body = crate::lexical::body(s).unwrap();
                crate::embedding::segments(body)
                    .into_iter()
                    .any(|(start, end)| {
                        df.matches_segment(body, start, end)
                            && m.evidence
                                .iter()
                                .filter(|e| {
                                    e.session_id.0 == s.session_id && e.turn_id.0 == s.turn_id
                                })
                                .filter_map(|e| locate_quote(body, &e.quote))
                                .any(|q| start < q.end && q.start < end)
                    })
            })
        });
        if enumerate
            || relation.is_some()
            || hits.contains(&m.id)
            || score > 0
            || (hybrid && entity_score > 0)
            || source_hit
        {
            selected.insert(m.id);
            if let Some(d) = &mut diagnostic {
                let mechanisms = [
                    (enumerate, "enumeration"),
                    (relation.is_some(), "relation"),
                    (hits.contains(&m.id), "fts"),
                    (source_hit, "source_literal"),
                    (score > 0, "literal"),
                ];
                d.selection.insert(
                    m.id,
                    mechanisms
                        .into_iter()
                        .filter(|(yes, _)| *yes)
                        .map(|(_, name)| name.into())
                        .collect(),
                );
            }
        }
    }
    let sequence = |id: &MemoryId| {
        sources_of(&state, eligible[id])
            .iter()
            .map(|s| s.received_sequence)
            .max()
            .unwrap_or(0)
    };
    let mut fused = BTreeMap::new();
    let mut seed_truncated = false;
    if hybrid {
        let mut lexical: Vec<_> = selected.iter().copied().collect();
        lexical.sort_by_key(|id| {
            (
                std::cmp::Reverse(scores[id]),
                std::cmp::Reverse(sequence(id)),
                *id,
            )
        });
        let limit = budget.records.saturating_mul(2);
        seed_truncated = lexical.len() > limit || vector_rank.len() > limit;
        fused = rrf(&lexical, &vector_rank, limit);
        selected = fused.keys().copied().collect();
        if let Some(d) = &mut diagnostic {
            d.lexical_df = df.clone();
            d.lexical_ranked = lexical;
            d.vector_ranked = vector_rank.iter().map(|r| r.0).collect();
            for (id, _) in vector_rank.iter().take(limit) {
                d.selection.entry(*id).or_default().push("vector".into());
            }
        }
    }
    diagnostic_stage(&mut diagnostic, "seeds", selected.iter().copied());
    let mut complete = plan.output.is_some() && (enumerate || (!enabled && relation.is_some()));
    complete &= plan
        .filter_time_resolution
        .as_ref()
        .is_none_or(|t| t.status == ResolutionStatus::Resolved);
    // 版本链与关系成员一起带出；只在可信范围内展开，决不靠关系扩大访问范围。
    loop {
        let before = selected.len();
        let parents: Vec<_> = selected
            .iter()
            .flat_map(|id| match projection(&state, *id) {
                Some(RecordView::Current(view)) => view.history,
                Some(RecordView::Legacy(view)) => view.data.supersedes.into_iter().collect(),
                None => Vec::new(),
            })
            .collect();
        if parents.iter().any(|id| {
            all.iter()
                .any(|m| m.id == *id && m.status != MemoryStatus::Deleted)
                && !eligible.contains_key(id)
        }) {
            complete = false;
        }
        if let Some(d) = &mut diagnostic {
            for id in parents.iter().filter(|id| eligible.contains_key(id)) {
                let reasons = d.selection.entry(*id).or_default();
                if !reasons.iter().any(|r| r == "version_chain") {
                    reasons.push("version_chain".into());
                }
            }
        }
        selected.extend(parents.into_iter().filter(|id| eligible.contains_key(id)));
        let links: Vec<_> = state
            .relations
            .values()
            .filter(|r| r.members.iter().any(|id| selected.contains(id)))
            .collect();
        for link in links {
            let outside = link.members.iter().any(|id| {
                all.iter()
                    .any(|m| m.id == *id && m.status != MemoryStatus::Deleted)
                    && !eligible.contains_key(id)
            });
            if outside {
                complete = false;
            } else {
                if let Some(d) = &mut diagnostic {
                    for id in link.members.iter().filter(|id| eligible.contains_key(id)) {
                        let reasons = d.selection.entry(*id).or_default();
                        if !reasons.iter().any(|r| r == "relation_member") {
                            reasons.push("relation_member".into());
                        }
                    }
                }
                selected.extend(
                    link.members
                        .iter()
                        .filter(|id| eligible.contains_key(id))
                        .copied(),
                );
            }
        }
        if selected.len() == before {
            break;
        }
    }
    diagnostic_stage(&mut diagnostic, "closure", selected.iter().copied());
    let sequence = |id: &MemoryId| {
        sources_of(&state, eligible[id])
            .iter()
            .map(|s| s.received_sequence)
            .max()
            .unwrap_or(0)
    };
    let mut ordered: Vec<_> = selected.iter().copied().collect();
    if hybrid {
        ordered.sort_by(|a, b| {
            fused
                .get(b)
                .copied()
                .unwrap_or_default()
                .total_cmp(&fused.get(a).copied().unwrap_or_default())
                .then_with(|| a.cmp(b))
        });
        if let Some(d) = &mut diagnostic {
            d.fused_ranked = ordered.clone();
        }
    } else {
        ordered.sort_by_key(|id| {
            (
                std::cmp::Reverse(scores.get(id).copied().unwrap_or_default()),
                std::cmp::Reverse(sequence(id)),
                *id,
            )
        });
    }
    diagnostic_stage(&mut diagnostic, "ranked", ordered.iter().copied());
    if let Some(d) = &mut diagnostic {
        d.ranking = ordered
            .iter()
            .map(|id| {
                let (entity_hits, phrase_hits) = scores.get(id).copied().unwrap_or_default();
                RetrievalRank {
                    id: *id,
                    entity_hits,
                    phrase_hits,
                    received_sequence: sequence(id),
                }
            })
            .collect();
    }
    let mut views: Vec<_> = ordered
        .into_iter()
        .filter_map(|id| projection(&state, id))
        .collect();
    diagnostic_stage(
        &mut diagnostic,
        "projected",
        views.iter().map(RecordView::id),
    );
    let valid_evidence = |e: &EvidenceRef| match &e.source {
        SourceRef::Stored { source_id } => state
            .sources
            .values()
            .find(|source| source.id == *source_id)
            .is_some_and(|s| {
                source_in_task(s, request)
                    && crate::contract::locate_quote(
                        crate::lexical::body(s).unwrap(),
                        &e.quote.text,
                    )
                    .is_some()
            }),
        SourceRef::Current {} => false,
    };
    for view in &mut views {
        match view {
            RecordView::Current(v) => {
                v.evidence.retain(&valid_evidence);
                v.history.retain(|id| eligible.contains_key(id));
            }
            RecordView::Legacy(v) => {
                v.evidence.retain(&valid_evidence);
                v.data.evidence.retain(|e| {
                    state
                        .sources
                        .values()
                        .find(|s| e.session_id.0 == s.session_id && e.turn_id.0 == s.turn_id)
                        .is_none_or(|s| source_in_task(s, request))
                });
                if v.evidence.is_empty() && !v.gaps.contains(&LegacyGap::SourceMissing) {
                    v.gaps.push(LegacyGap::SourceMissing);
                }
            }
        }
    }
    let pending_updates = views
        .iter()
        .filter(|v| match v {
            RecordView::Current(v) => v.update.status == UpdateStatus::Pending,
            RecordView::Legacy(v) => v
                .update
                .as_ref()
                .is_some_and(|u| u.status == UpdateStatus::Pending),
        })
        .count();
    let mut enrichment_gaps = views
        .iter()
        .map(|v| match v {
            RecordView::Legacy(v) => v.gaps.len(),
            _ => 0,
        })
        .sum();
    let legacy_sources: BTreeSet<_> = views
        .iter()
        .filter_map(|v| match v {
            RecordView::Legacy(v) => Some(&v.evidence),
            _ => None,
        })
        .flatten()
        .filter_map(|e| match &e.source {
            SourceRef::Stored { source_id } => Some(source_id.as_str()),
            _ => None,
        })
        .collect();
    let mut raw = Vec::new();
    let mut pending_related_sources = 0;
    let mut pending_sources = Vec::new();
    for source in state
        .sources
        .values()
        .filter(|s| source_in_task(s, request))
    {
        let status = enrichment(source);
        let pending = !matches!(
            status,
            Some(EnrichmentStatus::Committed | EnrichmentStatus::CompletedNoFacts)
        );
        // 未完成来源全送审；完成来源仍仅保留已选中的 legacy 链接。
        if !pending && !legacy_sources.contains(source.id.as_str()) {
            continue;
        }
        let body = crate::lexical::body(source).unwrap();
        let linked = source_links
            .get(source.id.as_str())
            .map(Vec::as_slice)
            .unwrap_or_default();
        if request.subject_ids.as_ref().is_some_and(Vec::is_empty) {
            continue;
        }
        if !linked.is_empty() && !linked.iter().any(|m| eligible.contains_key(&m.id)) {
            continue;
        }
        if linked
            .iter()
            .any(|m| m.status != MemoryStatus::Deleted && !eligible.contains_key(&m.id))
        {
            // 一条原文涉及范围内外多个主体/任务时，不能把整条正文借入窄范围。
            complete = false;
            continue;
        }
        // 未整理原文没有可靠主体字段；显式限制主体时不凭词面猜归属。
        if request.subject_ids.is_some() && linked.is_empty() {
            complete = false;
            continue;
        }
        let Some(status) = status else {
            enrichment_gaps += 1;
            complete = false;
            continue;
        };
        pending_related_sources += usize::from(pending);
        if pending {
            pending_sources.push(source);
        }
        raw.push(RawEvidence {
            source_id: source.id.clone(),
            quote: Quote { text: body.into() },
            said_on: source_date(source, offset)?,
            sequence: source.received_sequence,
            grade: RecallGrade::Uncommitted,
            enrichment: status,
            memory_requests: requests(source)?,
        });
    }
    // 先把最新来源放进数量/字节预算，选中后再按接收顺序交给宿主。
    raw.sort_by_key(|r| (std::cmp::Reverse(r.sequence), r.source_id.clone()));
    let mut relations = Vec::new();
    for relation in state
        .relations
        .values()
        .filter(|r| r.members.iter().any(|id| selected.contains(id)))
    {
        if relation.members.iter().any(|id| {
            all.iter()
                .any(|m| m.id == *id && m.status != MemoryStatus::Deleted)
                && !eligible.contains_key(id)
        }) {
            continue;
        }
        if relation
            .basis
            .iter()
            .chain(relation.resolution.iter())
            .any(|e| {
                state
                    .sources
                    .get(&e.source_id)
                    .is_some_and(|s| s.request.is_some() && !source_in_task(s, request))
            })
        {
            complete = false;
            continue;
        }
        let mut link = crate::w4::relation_link(relation)?;
        link.basis.retain(&valid_evidence);
        link.resolution = link.resolution.filter(&valid_evidence);
        relations.push(link);
    }
    diagnostic_raw(&mut diagnostic, "eligible", &raw);
    let mut remaining = budget.stored_bytes;
    let mut truncated = seed_truncated;
    if views.len() > budget.records {
        truncated = true;
        views.truncate(budget.records);
    }
    diagnostic_stage(
        &mut diagnostic,
        "record_count_budget",
        views.iter().map(RecordView::id),
    );
    views.retain(|v| {
        let size = serde_json::to_vec(v).expect("record serialization").len();
        let keep = size <= remaining;
        if keep {
            remaining -= size;
        } else {
            truncated = true;
        }
        keep
    });
    diagnostic_stage(
        &mut diagnostic,
        "record_byte_budget",
        views.iter().map(RecordView::id),
    );
    if raw.len() > budget.raw {
        truncated = true;
        raw.truncate(budget.raw);
    }
    diagnostic_raw(&mut diagnostic, "count_budget", &raw);
    let mut bounded_raw = Vec::new();
    for mut item in raw {
        let original_size = serde_json::to_vec(&item)?.len();
        if fit_raw(&mut item, remaining)? {
            let size = serde_json::to_vec(&item)?.len();
            truncated |= size != original_size;
            remaining -= size;
            bounded_raw.push(item);
        } else {
            truncated = true;
        }
    }
    bounded_raw.sort_by_key(|r| (r.sequence, r.source_id.clone()));
    let raw = bounded_raw;
    diagnostic_raw(&mut diagnostic, "byte_budget", &raw);
    let uncovered_sources = pending_sources
        .into_iter()
        .filter_map(|source| {
            let id = source.id.as_str();
            let body = crate::lexical::body(source).unwrap();
            // raw 被裁掉但完整正文已在记录依据中时，没有遗漏新证据。
            let covered = body.is_empty()
                || raw
                    .iter()
                    .any(|r| r.source_id == id && r.quote.text == body)
                || views.iter().any(|r| {
                    r4::record_evidence(r).iter().any(|e| {
                        matches!(&e.source, SourceRef::Stored {source_id} if source_id == id)
                            && locate_quote(&e.quote.text, body).is_some()
                    })
                });
            if covered {
                return None;
            }
            // 整句引用只证明原文可见，不证明其中所有断言已整理；已提交单条候选
            // 不能替该来源的其他未提交候选证明主体/关系槽位。遗漏来源仍保守门控。
            Some(None)
        })
        .collect();
    if let Some(d) = &mut diagnostic {
        d.relation_stages.insert(
            "eligible".into(),
            relations.iter().map(|r| r.id.clone()).collect(),
        );
    }
    if relations.len() > budget.relations {
        truncated = true;
        relations.truncate(budget.relations);
    }
    if let Some(d) = &mut diagnostic {
        d.relation_stages.insert(
            "count_budget".into(),
            relations.iter().map(|r| r.id.clone()).collect(),
        );
    }
    relations.retain(|r| {
        let size = serde_json::to_vec(r).expect("relation serialization").len();
        let keep = size <= remaining;
        if keep {
            remaining -= size;
        } else {
            truncated = true;
        }
        keep
    });
    if let Some(d) = &mut diagnostic {
        d.relation_stages.insert(
            "byte_budget".into(),
            relations.iter().map(|r| r.id.clone()).collect(),
        );
    }
    complete &= !truncated;
    let current_message = (!request.bootstrap).then(|| CurrentMessage {
        source_id: None,
        text: request.binding.query.clone(),
        said_on: request.said_on.clone(),
        sequence: None,
        enrichment: EnrichmentStatus::NotStored,
        memory_requests: Vec::new(),
    });
    let assembly = Assembly {
        retrieval_diagnostic: diagnostic,
        retrieval: metadata,
        request: request.binding.clone(),
        plan: plan.clone(),
        uncovered_sources,
        safety: SafetySnapshot::capture(&state, request)?,
        utc_offset_seconds: request.utc_offset_seconds,
        source_dates: state
            .sources
            .values()
            .filter(|s| source_in_task(s, request))
            .map(|s| Ok((s.id.clone(), source_date(s, offset)?)))
            .collect::<PersistenceResult<_>>()?,
        source_context: state.sources.values()
            .filter(|s| source_in_task(s, request) && s.is_history_import())
            .filter(|s| raw.iter().any(|r| r.source_id == s.id) || views.iter().any(|r| crate::r4::record_evidence(r).iter().any(|e| matches!(&e.source, crate::contract::SourceRef::Stored {source_id} if source_id == &s.id))))
            .map(|s| (s.id.clone(), serde_json::json!({"provenance":s.management.provenance,
                "time_anchor":s.management.time_anchor}))).collect(),
        entity_names: state
            .entities
            .list_by_owner(store.owner())
            .into_iter()
            .map(|e| (e.id.0, e.canonical_name))
            .collect(),
        history_until: views
            .iter()
            .filter_map(|v| {
                let explicit_old_end = || {
                    let to = state.repository.get(v.id()).ok()??.temporal.valid_to?;
                    // 旧 engine 的 Supersession 可能只用声明接收时刻，不能当作已知结束日。
                    (to.source == crate::TemporalSource::Explicit).then_some(to.at.0)
                };
                let at = match crate::w4::record_view(&state, v.id())? {
                    RecordView2::Current(full) if full.ended == Ended::Known => {
                        if let Some(end) = full.end {
                            if end.status != ResolutionStatus::Resolved
                                || end.precision != Precision::Day
                                || end.end_exclusive?.checked_sub(end.start?)? != 86400
                            {
                                return None;
                            }
                            end.start?
                        } else {
                            explicit_old_end()?
                        }
                    }
                    RecordView2::Legacy(_) => explicit_old_end()?,
                    _ => return None,
                };
                Some((v.id(), local_date(at, offset).ok()?))
            })
            .collect(),
        snapshot: RecallSnapshot {
            read_revision: state.revision,
            source_revision: state.source_revision,
        },
        evidence: RecallEvidence {
            current_message,
            records: views,
            raw,
            relations,
            completeness: RecallCompleteness {
                retrieval_truncated: truncated,
                pending_related_sources,
                pending_updates,
                enrichment_gaps,
                plan_complete: complete,
            },
        },
    };
    Ok(assembly)
}

// Never put provider error strings (which may contain URLs or content) into trace.
fn retrieval_error(error: &str) -> &'static str {
    match error {
        "embedding_deadline_exhausted" => "timeout",
        "embedding_transport" => "transport",
        "embedding_cancelled" => "cancelled",
        "embedding_network_disabled" | "embedding_network_policy_invalid" => "network_disabled",
        _ => "query_invalid",
    }
}

#[cfg(test)]
mod af288_tests {
    #[test]
    fn errors_are_fixed_categories_not_provider_text() {
        for (input, output) in [
            ("embedding_transport", "transport"),
            ("embedding_deadline_exhausted", "timeout"),
            (
                "http://secret:password@host/private original text",
                "query_invalid",
            ),
        ] {
            assert_eq!(super::retrieval_error(input), output);
        }
    }
}
