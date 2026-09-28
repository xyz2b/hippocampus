//! Write-side orchestration: admission → related-memory lookup → reconciliation → Store。

use crate::{
    AdmissionCheck, AdmissionCheckStatus, AdmissionDecision, AdmissionPolicy, AdmissionReport,
    CandidateIntent, CandidateMemory, CandidateStaging, CandidateSubject, Cardinality, DatedValue,
    DefaultAdmissionPolicy, DefaultReconciliationPolicy, DefaultRetentionPolicy, EntityRepository,
    EntityResolution, Evidence, EvidenceId, Memory, MemoryId, MemoryRepository, MemoryStatus,
    PredicateRegistry, ReconciliationAction, ReconciliationDecision, ReconciliationPolicy,
    RetentionAction, RetentionPolicy, StagedCandidateId, StoreError, SupersedeReason,
    TemporalSource, Timestamp, same_semantic_slot,
};

#[derive(serde::Serialize, Clone, Debug, Eq, PartialEq)]
/// 一次 candidate 写入的可观察结果。
pub enum IngestOutcome {
    /// 成功执行一个 reconciliation action。
    Applied {
        /// 实际执行的 NEW / REINFORCE / REFINE / SUPERSEDE / RETRACT。
        action: ReconciliationAction,
        /// 新建或被强化的 memory；Retract 没有新 memory，因此为 `None`。
        memory_id: Option<MemoryId>,
        /// 被强化、替换或撤回的旧 memory。
        target: Option<MemoryId>,
    },
    /// Candidate 未通过 admission，附带稳定的拒绝原因。
    Rejected(&'static str),
    /// Candidate 被 reconciliation 明确忽略。
    Ignored,
    /// 相同 Evidence ID 或同一 source turn 已处理，未产生任何 Store 变更。
    Duplicate {
        /// 本次被判定为重复的 Evidence ID。
        evidence_id: EvidenceId,
        /// 已持有该证据的 Memory。
        memory_id: MemoryId,
    },
    /// 无法安全合并；可选 target 是已定位到的冲突记录。
    Conflict { target: Option<MemoryId> },
}

#[derive(serde::Serialize, Clone, Debug, Eq, PartialEq)]
/// 一次写入从 admission 到 reconciliation 的结构化决策轨迹。
pub struct IngestTrace {
    /// Admission policy 的最终决定。
    pub admission: AdmissionDecision,
    /// Admit 或 Reject 的直接原因。
    pub admission_reason: &'static str,
    /// 实际执行的准入检查；自定义策略未提供明细时为 None。
    pub admission_checks: Option<Vec<AdmissionCheck>>,
    /// 本次 owner 范围搜索看到的全部旧 Memory IDs。
    pub existing_memory_ids: Vec<MemoryId>,
    /// 与 Candidate semantic slot 匹配的 Active Memory IDs。
    pub matched_memory_ids: Vec<MemoryId>,
    /// Reconciliation 决定；admission reject 或提前去重时为空。
    pub reconciliation: Option<ReconciliationDecision>,
    /// 选择该 action 或跳过 reconciliation 的原因。
    pub reconciliation_reason: Option<&'static str>,
}

#[derive(serde::Serialize, Clone, Debug, Eq, PartialEq)]
/// 写入结果及其可测试、可输出的决策轨迹。
pub struct IngestReport {
    /// Store 可观察结果。
    pub outcome: IngestOutcome,
    /// 产生该结果的完整写入轨迹。
    pub trace: IngestTrace,
}

#[derive(serde::Serialize, Clone, Debug, Eq, PartialEq)]
/// 正式写入前 Entity resolution 边界的处理结果。
pub enum CandidateIngestOutcome {
    /// Candidate 已有唯一 subject，并完成原有写入流程。
    Ingested(IngestOutcome),
    /// Candidate 暂存在正式 Memory Store 之外，等待澄清。
    Pending {
        /// 后续澄清 turn 使用的稳定句柄。
        staged_id: StagedCandidateId,
        /// 最近一次 subject reference 未能唯一解析的原因。
        resolution: EntityResolution,
    },
}

#[derive(serde::Serialize, Clone, Debug, Eq, PartialEq)]
/// 候选处理结果及实际执行过的准入、合并轨迹。
pub struct CandidateIngestReport {
    pub outcome: CandidateIngestOutcome,
    /// 待澄清候选尚未执行正式准入、合并，因而没有这段轨迹。
    pub trace: Option<IngestTrace>,
}

/// Personal Memory 写入和留存流程的组合入口。
pub struct MemoryEngine<
    R,
    P,
    A = DefaultAdmissionPolicy,
    C = DefaultReconciliationPolicy,
    T = DefaultRetentionPolicy,
> {
    /// 正式 Memory Store。
    pub repository: R,
    /// 提供 predicate cardinality 的 schema registry。
    pub predicates: P,
    /// 正式入库前的候选过滤规则。
    pub admission: A,
    /// Candidate 与现有 memory 的合并规则。
    pub reconciliation: C,
    /// 自动过期与层级升级规则。
    pub retention: T,
    /// 在正式 Memory Store 外等待澄清的 unresolved / ambiguous candidates。
    pub staging: CandidateStaging,
}

impl<R, P> MemoryEngine<R, P> {
    pub fn new(repository: R, predicates: P) -> Self {
        Self {
            repository,
            predicates,
            admission: DefaultAdmissionPolicy,
            reconciliation: DefaultReconciliationPolicy,
            retention: DefaultRetentionPolicy,
            staging: CandidateStaging::default(),
        }
    }
}

fn reconciliation_reason(
    candidate: &CandidateMemory,
    decision: &ReconciliationDecision,
) -> &'static str {
    match decision.action {
        ReconciliationAction::New => "no active equal-scope memory matched",
        ReconciliationAction::Reinforce => {
            "same semantic slot, scope, and assertion matched independent evidence"
        }
        ReconciliationAction::Refine => "candidate explicitly refines the matched memory",
        ReconciliationAction::Supersede => "candidate explicitly supersedes the matched memory",
        ReconciliationAction::Retract => "candidate explicitly retracts the matched memory",
        ReconciliationAction::Ignore => match &candidate.intent {
            CandidateIntent::Ignore => "candidate explicitly requests no write",
            _ => "explicit update target was not found",
        },
        ReconciliationAction::Conflict => {
            if matches!(&candidate.intent, CandidateIntent::Conflict) {
                "extractor marked the candidate as unresolved conflict"
            } else if decision.target.is_some() {
                "same slot has a different assertion without explicit update intent"
            } else {
                "multiple old memories matched and no unique target was safe"
            }
        }
    }
}

impl<R, P, A, C, T> MemoryEngine<R, P, A, C, T>
where
    R: MemoryRepository,
    P: PredicateRegistry,
    A: AdmissionPolicy,
    C: ReconciliationPolicy,
    T: RetentionPolicy,
{
    /// 尝试解析 unresolved subject；不能唯一解析时写入 staging。
    pub fn ingest_or_stage<E: EntityRepository>(
        &mut self,
        candidate: CandidateMemory,
        entities: &E,
    ) -> Result<CandidateIngestOutcome, StoreError> {
        Ok(self
            .ingest_or_stage_with_trace(candidate, entities)?
            .outcome)
    }

    /// 只处理一次候选，并保留实际准入、合并原因；结果不保证外层事务已提交。
    pub fn ingest_or_stage_with_trace<E: EntityRepository>(
        &mut self,
        mut candidate: CandidateMemory,
        entities: &E,
    ) -> Result<CandidateIngestReport, StoreError> {
        if !candidate.has_complete_evidence() {
            let reason = "evidence is incomplete";
            return Ok(CandidateIngestReport {
                outcome: CandidateIngestOutcome::Ingested(IngestOutcome::Rejected(reason)),
                trace: Some(IngestTrace {
                    admission: AdmissionDecision::Reject(reason),
                    admission_reason: reason,
                    admission_checks: Some(vec![AdmissionCheck {
                        rule: "evidence_complete",
                        status: AdmissionCheckStatus::Failed,
                        reason,
                    }]),
                    existing_memory_ids: Vec::new(),
                    matched_memory_ids: Vec::new(),
                    reconciliation: None,
                    reconciliation_reason: None,
                }),
            });
        }
        if let CandidateSubject::Unresolved(reference) = &candidate.subject {
            match entities.resolve(&candidate.owner_id, reference) {
                EntityResolution::Resolved(entity_id) => {
                    let source_supported = candidate.source_support_valid();
                    candidate.subject = CandidateSubject::Resolved(entity_id);
                    if source_supported {
                        // Exact catalog resolution preserves the reviewed subject reference.
                        candidate.bind_source_support();
                    }
                }
                resolution => {
                    let staged_id = self.staging.insert(candidate, resolution.clone());
                    return Ok(CandidateIngestReport {
                        outcome: CandidateIngestOutcome::Pending {
                            staged_id,
                            resolution,
                        },
                        trace: None,
                    });
                }
            }
        }
        let report = self.ingest_with_trace(candidate)?;
        Ok(CandidateIngestReport {
            outcome: CandidateIngestOutcome::Ingested(report.outcome),
            trace: Some(report.trace),
        })
    }

    /// 应用后续 Entity 指代；只有唯一解析后才进入正式写入流程。
    pub fn clarify_staged<E: EntityRepository>(
        &mut self,
        staged_id: StagedCandidateId,
        entity_reference: &str,
        clarification: Evidence,
        entities: &E,
    ) -> Result<CandidateIngestOutcome, StoreError> {
        let entity_reference = entity_reference.trim();
        if entity_reference.is_empty() {
            return Err(StoreError(
                "entity clarification must not be empty".to_owned(),
            ));
        }
        if !clarification.is_complete() {
            return Err(StoreError(
                "clarification evidence is incomplete".to_owned(),
            ));
        }
        let mut staged = self.staging.get(staged_id).cloned().ok_or_else(|| {
            StoreError(format!("staged candidate {} does not exist", staged_id.0))
        })?;
        if !staged.candidate.source_support_valid() {
            return Ok(CandidateIngestOutcome::Ingested(IngestOutcome::Rejected(
                "staged candidate has no valid source support",
            )));
        }
        let text = clarification.quote.trim();
        let text = text
            .strip_suffix(['。', '.', '！', '!'])
            .unwrap_or(text)
            .trim();
        let selected = ["", "我说的是", "我指的是", "说的是", "指的是", "我是说"]
            .iter()
            .any(|prefix| text.strip_prefix(prefix) == Some(entity_reference))
            || matches!(&staged.candidate.subject, CandidateSubject::Unresolved(reference)
                if text == format!("{reference}指的是{entity_reference}"));
        if !selected {
            return Ok(CandidateIngestOutcome::Ingested(IngestOutcome::Rejected(
                "clarification does not explicitly select the requested entity",
            )));
        }
        if staged.candidate.evidence.iter().any(|evidence| {
            (evidence.id == clarification.id || evidence.same_turn(&clarification))
                && evidence.quote != clarification.quote
        }) {
            return Ok(CandidateIngestOutcome::Ingested(IngestOutcome::Rejected(
                "clarification evidence conflicts with an existing source turn",
            )));
        }
        if !staged
            .candidate
            .evidence
            .iter()
            .any(|evidence| evidence.id == clarification.id || evidence.same_turn(&clarification))
        {
            staged.candidate.evidence.push(clarification);
        }
        let resolution = entities.resolve(&staged.candidate.owner_id, entity_reference);

        match resolution {
            EntityResolution::Resolved(entity_id) => {
                staged.candidate.subject = CandidateSubject::Resolved(entity_id);
                staged.candidate.bind_source_support();
                // 只在正式 Store 操作成功后移除；存储错误时保留 pending record 以便重试。
                let outcome = self.ingest(staged.candidate)?;
                if !matches!(
                    outcome,
                    IngestOutcome::Rejected(_) | IngestOutcome::Conflict { .. }
                ) {
                    self.staging.remove(staged_id);
                }
                Ok(CandidateIngestOutcome::Ingested(outcome))
            }
            resolution => {
                staged.candidate.subject =
                    CandidateSubject::Unresolved(entity_reference.to_owned());
                staged.candidate.bind_source_support();
                self.staging
                    .update(staged_id, staged.candidate, resolution.clone());
                Ok(CandidateIngestOutcome::Pending {
                    staged_id,
                    resolution,
                })
            }
        }
    }

    pub fn ingest(&mut self, candidate: CandidateMemory) -> Result<IngestOutcome, StoreError> {
        Ok(self.ingest_with_trace(candidate)?.outcome)
    }

    /// 执行正式写入并返回每个决策阶段的结构化原因。
    pub fn ingest_with_trace(
        &mut self,
        candidate: CandidateMemory,
    ) -> Result<IngestReport, StoreError> {
        // Keep policy rejection reasons intact; every admitted candidate then crosses
        // the same source boundary before any formal repository access.
        let AdmissionReport {
            decision: mut admission,
            checks: mut admission_checks,
        } = self.admission.evaluate_with_trace(&candidate);
        if admission == AdmissionDecision::Admit {
            for (rule, passed, success, failure, status) in [
                (
                    "fact_value_representation",
                    candidate.has_complete_fact_value(),
                    "generic fact retains a complete USER sentence; dedicated predicates keep typed values",
                    "generic fact requires a complete USER sentence",
                    if candidate.predicate.0 == "fact" {
                        AdmissionCheckStatus::Passed
                    } else {
                        AdmissionCheckStatus::NotApplicable
                    },
                ),
                (
                    "source_support",
                    candidate.source_support_valid(),
                    "candidate matches source-reviewed snapshot",
                    "candidate has no valid source support",
                    AdmissionCheckStatus::Passed,
                ),
            ] {
                admission_checks
                    .get_or_insert_default()
                    .push(AdmissionCheck {
                        rule,
                        status: if passed {
                            status
                        } else {
                            AdmissionCheckStatus::Failed
                        },
                        reason: if passed { success } else { failure },
                    });
                if !passed {
                    admission = AdmissionDecision::Reject(failure);
                    break;
                }
            }
        }
        if let AdmissionDecision::Reject(reason) = admission {
            return Ok(IngestReport {
                outcome: IngestOutcome::Rejected(reason),
                trace: IngestTrace {
                    admission: AdmissionDecision::Reject(reason),
                    admission_reason: reason,
                    admission_checks,
                    existing_memory_ids: Vec::new(),
                    matched_memory_ids: Vec::new(),
                    reconciliation: None,
                    reconciliation_reason: None,
                },
            });
        }

        // V1 内存版做 owner 范围扫描；持久化实现可在 repository 内优化 related lookup。
        let existing = self.repository.list_by_owner(&candidate.owner_id)?;
        let cardinality = self
            .predicates
            .definition(&candidate.predicate)
            .map_or(Cardinality::Multiple, |definition| definition.cardinality);
        let mut trace = IngestTrace {
            admission,
            admission_reason: "admission policy admitted candidate",
            admission_checks,
            existing_memory_ids: existing.iter().map(|memory| memory.id).collect(),
            matched_memory_ids: existing
                .iter()
                .filter(|memory| {
                    memory.status == MemoryStatus::Active
                        && same_semantic_slot(memory, &candidate, cardinality)
                })
                .map(|memory| memory.id)
                .collect(),
            reconciliation: None,
            reconciliation_reason: None,
        };
        // Evidence ID 在 owner 范围内标识一次原子证据，重放不能进入 reconciliation。
        if let Some((memory_id, evidence_id)) = existing.iter().find_map(|memory| {
            candidate.evidence.iter().find_map(|candidate_evidence| {
                memory
                    .evidence
                    .iter()
                    .any(|evidence| evidence.id == candidate_evidence.id)
                    .then(|| (memory.id, candidate_evidence.id.clone()))
            })
        }) {
            trace.reconciliation_reason = Some("duplicate evidence id; reconciliation skipped");
            return Ok(IngestReport {
                outcome: IngestOutcome::Duplicate {
                    evidence_id,
                    memory_id,
                },
                trace,
            });
        }
        let decision = self
            .reconciliation
            .decide(&candidate, &existing, cardinality);
        trace.reconciliation = Some(decision.clone());
        trace.reconciliation_reason = Some(reconciliation_reason(&candidate, &decision));

        let outcome = match decision.action {
            ReconciliationAction::New => {
                let id = self.insert(candidate, None)?;
                IngestOutcome::Applied {
                    action: ReconciliationAction::New,
                    memory_id: Some(id),
                    target: None,
                }
            }
            ReconciliationAction::Reinforce => {
                let target = decision.target.expect("reinforce decision has a target");
                let mut memory = self.required(target)?;
                if let Some(duplicate) = candidate.evidence.iter().find(|candidate_evidence| {
                    memory
                        .evidence
                        .iter()
                        .any(|evidence| evidence.same_turn(candidate_evidence))
                }) {
                    trace.reconciliation_reason =
                        Some("same source turn already supports target; reinforcement skipped");
                    return Ok(IngestReport {
                        outcome: IngestOutcome::Duplicate {
                            evidence_id: duplicate.id.clone(),
                            memory_id: target,
                        },
                        trace,
                    });
                }
                memory.reinforcement_count += 1;
                memory.last_reinforced_at = Some(candidate.temporal.observed_at);
                memory.certainty = memory.certainty.max(candidate.certainty);
                // 留存升级由 policy 决定，后续模型候选的 tier 不绕过独立强化门槛。
                memory.explicit_memory_request |= candidate.explicit_memory_request;
                memory.evidence.extend(candidate.evidence.clone());
                // Reinforcement 只来自本次新的用户证据；retrieval_count 不参与升级。
                if let RetentionAction::Promote(tier) = self
                    .retention
                    .evaluate(&memory, candidate.temporal.observed_at)
                {
                    memory.retention_tier = tier;
                }
                self.repository.update(memory)?;
                IngestOutcome::Applied {
                    action: ReconciliationAction::Reinforce,
                    memory_id: Some(target),
                    target: Some(target),
                }
            }
            ReconciliationAction::Refine | ReconciliationAction::Supersede => {
                let target = decision.target.expect("replacement decision has a target");
                let reason = decision.reason.unwrap_or(SupersedeReason::Refinement);
                let mut candidate = candidate;
                let mut boundary = candidate.temporal.observed_at;
                if reason == SupersedeReason::StateChanged && candidate.temporal.reference.is_none()
                {
                    // 新旧状态必须共用切换边界。未给日期时仅推定从本次声明起成立；
                    // 明确补述的过去日期优先，未知事件日期则保留 reference，不伪造时间。
                    let start = candidate.temporal.valid_from.get_or_insert(DatedValue {
                        at: boundary,
                        source: TemporalSource::Supersession,
                    });
                    boundary = start.at;
                    let old = self.required(target)?;
                    if old
                        .temporal
                        .valid_from
                        .is_some_and(|from| from.at >= boundary)
                        || candidate
                            .temporal
                            .valid_to
                            .is_some_and(|to| boundary >= to.at)
                    {
                        // 日期与已有版本矛盾时，必须在任何 Store 变更前停下并留下原因。
                        trace.reconciliation = Some(ReconciliationDecision {
                            action: ReconciliationAction::Conflict,
                            target: Some(target),
                            reason: Some(reason),
                        });
                        trace.reconciliation_reason =
                            Some("state-change boundary contradicts version validity");
                        return Ok(IngestReport {
                            outcome: IngestOutcome::Conflict {
                                target: Some(target),
                            },
                            trace,
                        });
                    }
                }
                let new_id = self.repository.allocate_id()?;
                let new_memory = Memory::from_candidate(new_id, &candidate, Some(target));
                // 旧版本先结束，再写入带双向 lineage 的新版本；生产 Store 应事务化此步骤。
                self.end_memory(target, Some(new_id), reason, boundary, &[])?;
                self.repository.insert(new_memory)?;
                IngestOutcome::Applied {
                    action: decision.action,
                    memory_id: Some(new_id),
                    target: Some(target),
                }
            }
            ReconciliationAction::Retract => {
                let target = decision.target.expect("retract decision has a target");
                // Retract 没有替代值：结束旧记录，但不制造一个反向 Preference。
                self.end_memory(
                    target,
                    None,
                    SupersedeReason::UserRetracted,
                    candidate.temporal.observed_at,
                    &candidate.evidence,
                )?;
                IngestOutcome::Applied {
                    action: ReconciliationAction::Retract,
                    memory_id: None,
                    target: Some(target),
                }
            }
            ReconciliationAction::Ignore => IngestOutcome::Ignored,
            ReconciliationAction::Conflict => IngestOutcome::Conflict {
                target: decision.target,
            },
        };
        Ok(IngestReport { outcome, trace })
    }

    pub fn run_retention_for_owner(
        &mut self,
        owner_id: &crate::UserId,
        now: Timestamp,
    ) -> Result<usize, StoreError> {
        let memories = self.repository.list_by_owner(owner_id)?;
        let mut changed = 0;
        // Retention 只做状态迁移，不物理删除历史记录。
        for mut memory in memories {
            match self.retention.evaluate(&memory, now) {
                RetentionAction::Keep => continue,
                RetentionAction::Promote(tier) => memory.retention_tier = tier,
                RetentionAction::Expire => memory.status = MemoryStatus::Expired,
            }
            self.repository.update(memory)?;
            changed += 1;
        }
        Ok(changed)
    }

    fn insert(
        &mut self,
        candidate: CandidateMemory,
        supersedes: Option<MemoryId>,
    ) -> Result<MemoryId, StoreError> {
        let id = self.repository.allocate_id()?;
        self.repository
            .insert(Memory::from_candidate(id, &candidate, supersedes))?;
        Ok(id)
    }

    fn required(&self, id: MemoryId) -> Result<Memory, StoreError> {
        self.repository
            .get(id)?
            .ok_or_else(|| StoreError(format!("memory {} does not exist", id.0)))
    }

    fn end_memory(
        &mut self,
        id: MemoryId,
        superseded_by: Option<MemoryId>,
        reason: SupersedeReason,
        at: Timestamp,
        evidence: &[Evidence],
    ) -> Result<(), StoreError> {
        let mut old = self.required(id)?;
        old.evidence.extend_from_slice(evidence);
        old.status = MemoryStatus::Superseded;
        old.superseded_by = superseded_by;
        old.supersede_reason = Some(reason);
        old.temporal.valid_to = Some(DatedValue {
            at,
            source: TemporalSource::Supersession,
        });
        self.repository.update(old)
    }
}

#[cfg(test)]
mod source_support_tests {
    use super::*;
    use crate::{
        EntityId, InMemoryMemoryRepository, InMemoryPredicateRegistry, MemoryType, MemoryValue,
        Predicate, SessionId, TurnId, UserId,
    };

    fn candidate() -> CandidateMemory {
        CandidateMemory::new(
            UserId::from("owner"),
            EntityId::from("printer"),
            MemoryType::Fact,
            Predicate::from("fact"),
            MemoryValue::Text("松岚打印机的默认纸张规格是B5。".into()),
            Timestamp(1),
            Evidence::new(
                EvidenceId::from("original"),
                SessionId::from("session"),
                TurnId::from("original"),
                "松岚打印机的默认纸张规格是B5。",
            ),
        )
    }

    fn engine() -> MemoryEngine<InMemoryMemoryRepository, InMemoryPredicateRegistry> {
        MemoryEngine::new(
            InMemoryMemoryRepository::default(),
            InMemoryPredicateRegistry::default(),
        )
    }

    #[test]
    fn source_support_rejects_unverified_at_shared_boundary() {
        let mut engine = engine();
        let report = engine.ingest_with_trace(candidate()).unwrap();
        assert!(matches!(report.outcome, IngestOutcome::Rejected(_)));
        assert!(
            engine
                .repository
                .list_by_owner(&UserId::from("owner"))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn source_support_binds_every_candidate_field_and_rejects_json_approvals() {
        let mut approved = candidate();
        assert!(!approved.source_support_valid());
        approved.bind_source_support();
        assert!(approved.source_support_valid());
        let decoded: CandidateMemory =
            serde_json::from_value(serde_json::to_value(&approved).unwrap()).unwrap();
        assert!(!decoded.source_support_valid());
        assert!(matches!(
            engine().ingest(decoded).unwrap(),
            IngestOutcome::Rejected(_)
        ));

        let mut forged = serde_json::to_value(candidate()).unwrap();
        forged["source_support"] = forged.clone();
        let forged: CandidateMemory = serde_json::from_value(forged).unwrap();
        assert!(!forged.source_support_valid());
        assert!(matches!(
            engine().ingest(forged).unwrap(),
            IngestOutcome::Rejected(_)
        ));
        for change in [
            (|c: &mut CandidateMemory| c.owner_id = UserId::from("other"))
                as fn(&mut CandidateMemory),
            |c| c.evidence[0].quote = "另一条原文".into(),
            |c| c.evidence[0].turn_id = TurnId::from("other"),
            |c| c.subject = CandidateSubject::Resolved(EntityId::from("other")),
            |c| c.memory_type = MemoryType::Episode,
            |c| c.predicate = Predicate::from("other"),
            |c| c.value = MemoryValue::Text("A3".into()),
            |c| c.polarity = Some(crate::Polarity::Negative),
            |c| c.strength = Some(crate::Strength::Strong),
            |c| c.scope.domain = Some(crate::Domain::from("other")),
            |c| c.retention_tier = crate::RetentionTier::ShortTerm,
            |c| c.temporal.observed_at = Timestamp(2),
            |c| c.extraction_confidence = crate::Confidence::Low,
            |c| c.certainty = crate::Confidence::Low,
            |c| c.source = crate::CandidateSource::AssistantInference,
            |c| c.intent = CandidateIntent::Retract { target: None },
            |c| c.explicit_memory_request = true,
            |c| c.cross_turn_useful = false,
            |c| c.tags.push("other".into()),
        ] {
            let mut changed = approved.clone();
            change(&mut changed);
            assert!(!changed.source_support_valid());
            assert!(matches!(
                engine().ingest(changed).unwrap(),
                IngestOutcome::Rejected(_)
            ));
        }
        let mut legacy = serde_json::to_value(approved).unwrap();
        legacy.as_object_mut().unwrap().remove("source_support");
        assert!(
            !serde_json::from_value::<CandidateMemory>(legacy)
                .unwrap()
                .source_support_valid()
        );
    }

    #[test]
    fn source_support_custom_admission_cannot_bypass_shared_boundary() {
        struct AdmitEverything;
        impl AdmissionPolicy for AdmitEverything {
            fn evaluate(&self, _: &CandidateMemory) -> AdmissionDecision {
                AdmissionDecision::Admit
            }
        }
        let mut engine = MemoryEngine {
            repository: InMemoryMemoryRepository::default(),
            predicates: InMemoryPredicateRegistry::default(),
            admission: AdmitEverything,
            reconciliation: DefaultReconciliationPolicy,
            retention: DefaultRetentionPolicy,
            staging: CandidateStaging::default(),
        };
        assert!(matches!(
            engine.ingest(candidate()).unwrap(),
            IngestOutcome::Rejected(_)
        ));
        let mut bad = candidate();
        bad.value = MemoryValue::Boolean(false);
        bad.bind_source_support();
        assert!(matches!(
            engine.ingest(bad).unwrap(),
            IngestOutcome::Rejected(_)
        ));
        assert!(
            engine
                .repository
                .list_by_owner(&UserId::from("owner"))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn source_support_rejects_incomplete_generic_values_but_preserves_typed_properties() {
        for value in [
            MemoryValue::Boolean(false),
            MemoryValue::Integer(240),
            MemoryValue::Entity(EntityId::from("paper")),
            MemoryValue::Text("B5".into()),
            MemoryValue::Text("默认纸张规格是B5".into()),
            MemoryValue::Text("松岚打印机的默认纸张规格是B5。".into()),
        ] {
            let mut candidate = candidate();
            candidate.evidence[0].quote = "在手动模式下，松岚打印机的默认纸张规格是B5。".into();
            candidate.value = value;
            // A mistaken approval must not bypass the independent representation contract.
            candidate.bind_source_support();
            assert!(matches!(
                engine().ingest(candidate).unwrap(),
                IngestOutcome::Rejected(_)
            ));
        }

        let mut complete = candidate();
        complete.evidence[0].quote = "请记住。松岚打印机的默认纸张规格是B5。".into();
        complete.bind_source_support();
        assert!(matches!(
            engine().ingest(complete).unwrap(),
            IngestOutcome::Applied { .. }
        ));

        for value in [
            MemoryValue::Text("B5".into()),
            MemoryValue::Integer(240),
            MemoryValue::Entity(EntityId::from("paper")),
            MemoryValue::Boolean(false),
        ] {
            let mut typed = candidate();
            typed.predicate = Predicate::from("reviewed_property");
            typed.value = value;
            typed.bind_source_support();
            assert!(matches!(
                engine().ingest(typed).unwrap(),
                IngestOutcome::Applied { .. }
            ));
        }
    }

    #[test]
    fn source_support_complete_sentences_keep_decimal_prices_and_versions() {
        for text in [
            "松岚打印机的单张成本是3.5元。",
            "松岚打印机的固件版本是v1.2。",
        ] {
            let mut candidate = candidate();
            candidate.value = MemoryValue::Text(text.into());
            candidate.evidence[0].quote = format!("请记住。{text}");
            candidate.bind_source_support();
            assert!(matches!(
                engine().ingest(candidate).unwrap(),
                IngestOutcome::Applied { .. }
            ));
        }
    }

    #[test]
    fn source_support_clarification_preserves_unverified_or_ambiguous_pending() {
        use crate::{Entity, EntityType, InMemoryEntityRepository};
        let mut entities = InMemoryEntityRepository::default();
        entities.upsert(Entity {
            id: EntityId::from("printer"),
            owner_id: UserId::from("owner"),
            entity_type: EntityType::Device,
            canonical_name: "松岚打印机".into(),
            aliases: vec![],
        });
        for (proof_version, restored, quote, accepted) in [
            (Some(4), false, "松岚打印机。", true),
            (Some(4), false, "我说的是松岚打印机。", true),
            (Some(4), false, "我是说松岚打印机。", true),
            (Some(4), false, "那台打印机指的是松岚打印机。", true),
            (Some(4), false, "不是松岚打印机。", false),
            (Some(4), false, "松岚打印机？", false),
            (
                Some(4),
                false,
                "我说的是松岚打印机，现在规格改为A3。",
                false,
            ),
            (None, false, "我说的是松岚打印机。", false),
            (Some(2), false, "我说的是松岚打印机。", false),
            (Some(2), true, "我说的是松岚打印机。", false),
            (Some(3), false, "我说的是松岚打印机。", false),
            (Some(3), true, "我说的是松岚打印机。", false),
            (Some(4), true, "我说的是松岚打印机。", true),
        ] {
            let mut engine = engine();
            let mut pending = candidate();
            pending.subject = CandidateSubject::Unresolved("那台打印机".into());
            if let Some(version) = proof_version {
                pending.bind_source_support();
                let proof = pending.source_support.as_mut().unwrap();
                assert_eq!(proof["version"], 4);
                proof["version"] = serde_json::json!(version);
            }
            let CandidateIngestOutcome::Pending { staged_id, .. } =
                engine.ingest_or_stage(pending, &entities).unwrap()
            else {
                panic!("expected pending candidate");
            };
            if restored {
                let stored = serde_json::to_value(&engine.staging).unwrap();
                engine.staging = serde_json::from_value(stored.clone()).unwrap();
                engine.staging.restore_trusted_source_support(&stored);
                assert_eq!(
                    engine
                        .staging
                        .get(staged_id)
                        .unwrap()
                        .candidate
                        .source_support_valid(),
                    proof_version == Some(4)
                );
            }
            let original = engine.staging.get(staged_id).unwrap().clone();
            let outcome = engine
                .clarify_staged(
                    staged_id,
                    "松岚打印机",
                    Evidence::new(
                        EvidenceId::from("clarification"),
                        SessionId::from("session"),
                        TurnId::from("clarification"),
                        quote,
                    ),
                    &entities,
                )
                .unwrap();
            assert_eq!(
                matches!(
                    outcome,
                    CandidateIngestOutcome::Ingested(IngestOutcome::Applied { .. })
                ),
                accepted,
                "{proof_version:?}, restored={restored}: {quote}"
            );
            if accepted {
                assert!(engine.staging.is_empty());
            } else {
                assert!(matches!(
                    outcome,
                    CandidateIngestOutcome::Ingested(IngestOutcome::Rejected(_))
                ));
                assert_eq!(engine.staging.get(staged_id), Some(&original));
                assert!(
                    engine
                        .repository
                        .list_by_owner(&UserId::from("owner"))
                        .unwrap()
                        .is_empty()
                );
            }
        }
    }

    #[test]
    fn source_support_catalog_resolution_rebinds_only_approved_candidates() {
        use crate::{Entity, EntityType, InMemoryEntityRepository};
        let mut entities = InMemoryEntityRepository::default();
        entities.upsert(Entity {
            id: EntityId::from("printer"),
            owner_id: UserId::from("owner"),
            entity_type: EntityType::Device,
            canonical_name: "松岚打印机".into(),
            aliases: vec![],
        });
        for approved in [false, true] {
            let mut candidate = candidate();
            candidate.subject = CandidateSubject::Unresolved("松岚打印机".into());
            if approved {
                candidate.bind_source_support();
            }
            let outcome = engine().ingest_or_stage(candidate, &entities).unwrap();
            assert_eq!(
                matches!(
                    outcome,
                    CandidateIngestOutcome::Ingested(IngestOutcome::Applied { .. })
                ),
                approved
            );
        }
    }
}
