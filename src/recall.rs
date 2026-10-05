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
    Ok(crate::openai::recall_models::fingerprint(
        &serde_json::json!([
            source.id,
            source.session_id,
            source.turn_id,
            source.observed_at,
            source.received_sequence,
            source.request,
            source.enrichment == "awaiting_confirmation",
            controls
        ]),
    ))
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

/// R4 的注入位置：消费本次组装，沿用原请求、R1 标记与读快照，不另做一次检索。
pub struct Assembly {
    pub request: RecallRequest,
    pub plan: r1::Plan,
    pub evidence: RecallEvidence,
    pub snapshot: RecallSnapshot,
    pub(crate) safety: SafetySnapshot,
    utc_offset_seconds: i32,
    source_dates: BTreeMap<String, String>,
    entity_names: BTreeMap<String, String>,
    history_until: BTreeMap<MemoryId, String>,
    // 仅保留未覆盖来源的可靠槽位；None 表示无法证明它与某条事实无关。
    uncovered_sources: Vec<Option<(String, String)>>,
}

impl Assembly {
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
            let evaluated =
                crate::diagnostics::checked("R4", "validate", r4::evaluate(final_output, &context))
                    .map_err(|_| JudgmentFailure::InvalidOutput)?;
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
                    None,
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
                let clarification_needed = self.plan.needs_clarification()
                    || output.verdict == R4Verdict::NeedsClarification;
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
                version: "recall.v3".into(),
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
                degraded_reasons: reasons,
                snapshot: self.snapshot,
            },
            audit,
        ))
    }

    fn fact(&self, f: FactView) -> (RecallFact, bool) {
        let related: Vec<_> = self
            .evidence
            .records
            .iter()
            .filter(|r| {
                f.record_ids.contains(&r.id())
                    || f.supersedes_record_ids.contains(&r.id())
                    || f.evidence.iter().any(|e| {
                        r4::record_evidence(r).iter().any(|re| {
                            e.source == re.source
                                && (locate_quote(&re.quote.text, &e.quote.text).is_some()
                                    || locate_quote(&e.quote.text, &re.quote.text).is_some())
                        })
                    })
            })
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
            && self.evidence.records.iter().any(|r| r4::supports_record(e, r)))
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
        // 无事实证据只阻止 R4；R1 已识别的主体歧义仍须按 Y1 请求澄清。
        let clarification = (reason == SkipReason::NoEvidence && self.plan.needs_clarification())
            .then(|| self.clarification(&[]));
        Ok(RecallOutput {
            version: "recall.v3".into(),
            request: self.request,
            status: if context_only {
                RecallStatus::ContextOnly
            } else if clarification.is_some() {
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
            degraded_reasons,
            snapshot: self.snapshot,
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

fn source_in_task(source: &SourceRecord, request: &Request) -> bool {
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

fn record_in_scope(state: &PersistedMemoryState, memory: &Memory, request: &Request) -> bool {
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

fn enrichment(source: &SourceRecord) -> Option<EnrichmentStatus> {
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

/// 全部数据与索引查询共用 tx；不调用旧召回，也不增加 retrieval_count 或写入来源。
pub fn assemble(
    store: &SqliteMemoryStore,
    request: &Request,
    plan: &r1::Plan,
    budget: Budget,
) -> PersistenceResult<Assembly> {
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
    let relation = plan
        .output
        .as_ref()
        .and_then(|o| o.plan.filter.relation.as_deref());
    let subject = plan.output.as_ref().and_then(|o| match &o.subject {
        R1Subject::Entity(s) => Some(s.entity_id.as_str()),
        _ => None,
    });
    let matches_plan = |m: &&Memory| {
        relation.is_none_or(|r| m.predicate.0 == r) && subject.is_none_or(|id| m.subject_id.0 == id)
    };
    let candidates: Vec<_> = eligible.values().copied().filter(matches_plan).collect();
    // FTS 只是候选提示；旧来源无索引时用同一快照的完整值/片段兜底，不据此认证语义。
    let hits = if enumerate {
        BTreeSet::new()
    } else {
        store
            .search_w4_facts(&tx, &state, &hint, &candidates)
            .unwrap_or_default()
            .into_iter()
            .collect()
    };
    let mut scores = BTreeMap::new();
    let mut selected = BTreeSet::new();
    for m in candidates {
        let text = std::iter::once(serde_json::to_string(&m.value)?)
            .chain(m.evidence.iter().map(|e| e.quote.clone()))
            .collect::<Vec<_>>()
            .join(" ");
        let score = phrase_score(&text, &phrases);
        let entity_score = entity_hints
            .values()
            .filter(|names| crate::lexical::matches_phrases(&text, names))
            .count();
        scores.insert(m.id, (entity_score, score));
        if enumerate || relation.is_some() || hits.contains(&m.id) || score > 0 {
            selected.insert(m.id);
        }
    }
    let mut complete = plan.output.is_some() && (enumerate || relation.is_some());
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
    let sequence = |id: &MemoryId| {
        sources_of(&state, eligible[id])
            .iter()
            .map(|s| s.received_sequence)
            .max()
            .unwrap_or(0)
    };
    let mut ordered: Vec<_> = selected.iter().copied().collect();
    ordered.sort_by_key(|id| {
        (
            std::cmp::Reverse(scores.get(id).copied().unwrap_or_default()),
            std::cmp::Reverse(sequence(id)),
            *id,
        )
    });
    let mut views: Vec<_> = ordered
        .into_iter()
        .filter_map(|id| projection(&state, id))
        .collect();
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
            said_on: local_date(source.observed_at, offset)?,
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
    let mut remaining = budget.stored_bytes;
    let mut truncated = false;
    if views.len() > budget.records {
        truncated = true;
        views.truncate(budget.records);
    }
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
    if raw.len() > budget.raw {
        truncated = true;
        raw.truncate(budget.raw);
    }
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
            // 仅单条已提交 Current 候选的整句覆盖整个来源时，才用其槽位排除无关事实。
            // 片段链接、legacy 或未整理原文都不足以排除来源中尚未理解的其他断言。
            let linked: Vec<_> = source_links
                .get(id)
                .into_iter()
                .flatten()
                .filter(|m| m.status != MemoryStatus::Deleted)
                .collect();
            let slot = if linked.len() == 1 {
                projection(&state, linked[0].id).and_then(|view| match &view {
                    RecordView::Current(v)
                        if v.candidate.sentence.text == body && v.evidence.iter().any(|e| {
                            matches!(&e.source, SourceRef::Stored {source_id} if source_id == id)
                                && locate_quote(body, &e.quote.text).is_some()
                        }) =>
                    {
                        record_slot(&view)
                    }
                    _ => None,
                })
            } else {
                None
            };
            Some(slot)
        })
        .collect();
    if relations.len() > budget.relations {
        truncated = true;
        relations.truncate(budget.relations);
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
        request: request.binding.clone(),
        plan: plan.clone(),
        uncovered_sources,
        safety: SafetySnapshot::capture(&state, request)?,
        utc_offset_seconds: request.utc_offset_seconds,
        source_dates: state
            .sources
            .values()
            .filter(|s| source_in_task(s, request))
            .map(|s| Ok((s.id.clone(), local_date(s.observed_at, offset)?)))
            .collect::<PersistenceResult<_>>()?,
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
    tx.commit()?;
    Ok(assembly)
}
