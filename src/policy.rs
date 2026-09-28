//! 可替换的 admission、reconciliation 与 retention 规则。

use crate::{
    CandidateIntent, CandidateMemory, CandidateSource, Cardinality, Confidence, Memory, MemoryId,
    MemoryStatus, MemoryType, RetentionTier, ScopeRelation, SupersedeReason, Timestamp,
};

#[derive(serde::Serialize, Clone, Debug, Eq, PartialEq)]
pub enum AdmissionDecision {
    Admit,
    Reject(&'static str),
}

#[derive(serde::Serialize, Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdmissionCheckStatus {
    Passed,
    Failed,
    NotApplicable,
}

#[derive(serde::Serialize, Clone, Debug, Eq, PartialEq)]
pub struct AdmissionCheck {
    pub rule: &'static str,
    pub status: AdmissionCheckStatus,
    pub reason: &'static str,
}

#[derive(serde::Serialize, Clone, Debug, Eq, PartialEq)]
pub struct AdmissionReport {
    pub decision: AdmissionDecision,
    /// 仅包含实际执行的检查；None 表示策略没有提供检查明细。
    pub checks: Option<Vec<AdmissionCheck>>,
}

pub trait AdmissionPolicy {
    /// 决定 candidate 是否有资格进入正式 Personal Memory Store。
    fn evaluate(&self, candidate: &CandidateMemory) -> AdmissionDecision;

    /// 保留旧策略实现的语义，不重跑检查或推测自定义策略的判定依据。
    fn evaluate_with_trace(&self, candidate: &CandidateMemory) -> AdmissionReport {
        AdmissionReport {
            decision: self.evaluate(candidate),
            checks: None,
        }
    }
}

#[derive(serde::Serialize, Clone, Copy, Debug, Default)]
pub struct DefaultAdmissionPolicy;

impl AdmissionPolicy for DefaultAdmissionPolicy {
    fn evaluate(&self, candidate: &CandidateMemory) -> AdmissionDecision {
        self.evaluate_with_trace(candidate).decision
    }

    fn evaluate_with_trace(&self, candidate: &CandidateMemory) -> AdmissionReport {
        let mut checks = Vec::new();
        let result = (|| {
            admission_check(
                &mut checks,
                "evidence_complete",
                candidate.has_complete_evidence(),
                "evidence is complete",
                "evidence is incomplete",
            )?;
            // Entity 歧义不是“低质量正式 memory”，而是仍待澄清的 staging candidate。
            admission_check(
                &mut checks,
                "subject_resolved",
                candidate.subject_id().is_some(),
                "subject is resolved",
                "subject is unresolved",
            )?;
            // certainty 可以低；这里拒绝的是系统无法可靠理解用户原话。
            admission_check(
                &mut checks,
                "extraction_confidence",
                candidate.extraction_confidence != Confidence::Low,
                "extraction confidence is not low",
                "extraction confidence is low",
            )?;
            admission_check(
                &mut checks,
                "user_evidence",
                matches!(
                    candidate.source,
                    CandidateSource::UserStatement | CandidateSource::UserConfirmed
                ),
                "source is user evidence",
                "source is not user evidence",
            )?;
            let (status, reason) = if candidate.memory_type != MemoryType::Episode {
                (
                    AdmissionCheckStatus::NotApplicable,
                    "candidate is not an episode",
                )
            } else if candidate.cross_turn_useful {
                (AdmissionCheckStatus::Passed, "episode has cross-turn value")
            } else if candidate.explicit_memory_request {
                (
                    AdmissionCheckStatus::Passed,
                    "user explicitly requested memory",
                )
            } else if matches!(
                candidate.intent,
                CandidateIntent::Supersede { .. }
                    | CandidateIntent::Refine { .. }
                    | CandidateIntent::Retract { .. }
            ) {
                // 更新旧证据无需新增跨轮价值；合并仍要求唯一的有效目标。
                (
                    AdmissionCheckStatus::Passed,
                    "candidate explicitly updates existing memory",
                )
            } else {
                (
                    AdmissionCheckStatus::Failed,
                    "episode has no cross-turn value",
                )
            };
            checks.push(AdmissionCheck {
                rule: "episode_value",
                status,
                reason,
            });
            if status == AdmissionCheckStatus::Failed {
                Err(reason)
            } else {
                Ok(())
            }
        })();
        AdmissionReport {
            decision: match result {
                Ok(()) => AdmissionDecision::Admit,
                Err(reason) => AdmissionDecision::Reject(reason),
            },
            checks: Some(checks),
        }
    }
}

fn admission_check(
    checks: &mut Vec<AdmissionCheck>,
    rule: &'static str,
    passed: bool,
    success: &'static str,
    failure: &'static str,
) -> Result<(), &'static str> {
    checks.push(AdmissionCheck {
        rule,
        status: if passed {
            AdmissionCheckStatus::Passed
        } else {
            AdmissionCheckStatus::Failed
        },
        reason: if passed { success } else { failure },
    });
    if passed { Ok(()) } else { Err(failure) }
}

#[derive(serde::Serialize, Clone, Copy, Debug, Eq, PartialEq)]
/// Write side 对 candidate 与已有 memory 的合并动作。
pub enum ReconciliationAction {
    New,
    Reinforce,
    Refine,
    Supersede,
    Retract,
    Ignore,
    Conflict,
}

#[derive(serde::Serialize, Clone, Debug, Eq, PartialEq)]
/// Reconciliation policy 的结构化决定，由 engine 负责执行。
pub struct ReconciliationDecision {
    /// 应执行的合并动作。
    pub action: ReconciliationAction,
    /// 动作指向的已有 memory；NEW / Ignore 通常为空。
    pub target: Option<MemoryId>,
    /// Supersede / Refine / Retract 的版本终止原因。
    pub reason: Option<SupersedeReason>,
}

impl ReconciliationDecision {
    fn new(action: ReconciliationAction) -> Self {
        Self {
            action,
            target: None,
            reason: None,
        }
    }

    fn targeting(
        action: ReconciliationAction,
        target: MemoryId,
        reason: Option<SupersedeReason>,
    ) -> Self {
        Self {
            action,
            target: Some(target),
            reason,
        }
    }
}

pub trait ReconciliationPolicy {
    fn decide(
        &self,
        candidate: &CandidateMemory,
        existing: &[Memory],
        cardinality: Cardinality,
    ) -> ReconciliationDecision;
}

#[derive(serde::Serialize, Clone, Copy, Debug, Default)]
pub struct DefaultReconciliationPolicy;

impl ReconciliationPolicy for DefaultReconciliationPolicy {
    fn decide(
        &self,
        candidate: &CandidateMemory,
        existing: &[Memory],
        cardinality: Cardinality,
    ) -> ReconciliationDecision {
        // Pragmatic update signals 由 extractor 映射成 intent；policy 只执行结构化决定。
        match &candidate.intent {
            CandidateIntent::Ignore => {
                return ReconciliationDecision::new(ReconciliationAction::Ignore);
            }
            CandidateIntent::Conflict => {
                return ReconciliationDecision::new(ReconciliationAction::Conflict);
            }
            CandidateIntent::Refine { target } => {
                return explicit_update(
                    candidate,
                    existing,
                    cardinality,
                    *target,
                    ReconciliationAction::Refine,
                    SupersedeReason::Refinement,
                );
            }
            CandidateIntent::Supersede { target, reason } => {
                return explicit_update(
                    candidate,
                    existing,
                    cardinality,
                    *target,
                    ReconciliationAction::Supersede,
                    *reason,
                );
            }
            CandidateIntent::Retract { target } => {
                return explicit_update(
                    candidate,
                    existing,
                    cardinality,
                    *target,
                    ReconciliationAction::Retract,
                    SupersedeReason::UserRetracted,
                );
            }
            CandidateIntent::Assert => {}
        }

        // 没有明确更新意图时，只在相同 semantic slot + 相同 scope 内自动合并。
        let same_scope: Vec<_> = existing
            .iter()
            .filter(|memory| {
                memory.status == MemoryStatus::Active
                    && same_semantic_slot(memory, candidate, cardinality)
                    && memory.scope.relation(&candidate.scope) == ScopeRelation::Equal
            })
            .collect();

        if same_scope.len() > 1 {
            return ReconciliationDecision::new(ReconciliationAction::Conflict);
        }
        let Some(memory) = same_scope.first() else {
            return ReconciliationDecision::new(ReconciliationAction::New);
        };

        if memory.same_assertion(candidate) {
            ReconciliationDecision::targeting(ReconciliationAction::Reinforce, memory.id, None)
        } else {
            // A changed value is not enough to infer correction vs real-world change.
            ReconciliationDecision::targeting(ReconciliationAction::Conflict, memory.id, None)
        }
    }
}

fn explicit_update(
    candidate: &CandidateMemory,
    existing: &[Memory],
    cardinality: Cardinality,
    requested_target: Option<MemoryId>,
    action: ReconciliationAction,
    reason: SupersedeReason,
) -> ReconciliationDecision {
    // target 缺失时允许按 semantic slot 唯一定位；多于一个候选必须报告冲突。
    let matches: Vec<_> = existing
        .iter()
        .filter(|memory| {
            // An explicit target names the memory being replaced, so its old value need
            // not equal the new one even in a multi-value slot.
            let slot = if requested_target.is_some() { Cardinality::Single } else { cardinality };
            memory.status == MemoryStatus::Active
                && same_semantic_slot(memory, candidate, slot)
                && requested_target.is_none_or(|target| memory.id == target)
                // Untargeted updates may refine ordinary conditions, but cannot cross task identities.
                && (requested_target.is_some()
                    || memory.scope.conditions.iter()
                        .filter(|c| matches!(c, crate::ScopeCondition::Task(_)))
                        .eq(candidate.scope.conditions.iter()
                            .filter(|c| matches!(c, crate::ScopeCondition::Task(_)))))
        })
        .collect();

    match matches.as_slice() {
        [memory] => ReconciliationDecision::targeting(action, memory.id, Some(reason)),
        [] => ReconciliationDecision::new(ReconciliationAction::Ignore),
        _ => ReconciliationDecision::new(ReconciliationAction::Conflict),
    }
}

pub(crate) fn same_semantic_slot(
    memory: &Memory,
    candidate: &CandidateMemory,
    cardinality: Cardinality,
) -> bool {
    // Single slot 不含 value；Multiple slot 包含 value，允许多个设备/地点同时存在。
    memory.owner_id == candidate.owner_id
        && candidate.subject_id() == Some(&memory.subject_id)
        && memory.memory_type == candidate.memory_type
        && memory.predicate == candidate.predicate
        && (cardinality == Cardinality::Single || memory.value == candidate.value)
}

pub(crate) fn memories_share_slot(left: &Memory, right: &Memory, cardinality: Cardinality) -> bool {
    left.owner_id == right.owner_id
        && left.subject_id == right.subject_id
        && left.memory_type == right.memory_type
        && left.predicate == right.predicate
        && (cardinality == Cardinality::Single || left.value == right.value)
}

#[derive(serde::Serialize, Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetentionAction {
    Keep,
    Promote(RetentionTier),
    Expire,
}

pub trait RetentionPolicy {
    fn evaluate(&self, memory: &Memory, now: Timestamp) -> RetentionAction;
}

#[derive(serde::Serialize, Clone, Copy, Debug, Default)]
pub struct DefaultRetentionPolicy;

impl RetentionPolicy for DefaultRetentionPolicy {
    fn evaluate(&self, memory: &Memory, now: Timestamp) -> RetentionAction {
        // Durable 只免于自动淘汰，不免于 write-side supersede/retract。
        if memory.status != MemoryStatus::Active || memory.retention_tier == RetentionTier::Durable
        {
            return RetentionAction::Keep;
        }
        if memory
            .temporal
            .expires_at
            .is_some_and(|expires| expires <= now)
        {
            return RetentionAction::Expire;
        }
        if memory.retention_tier == RetentionTier::ShortTerm
            && (memory.explicit_memory_request || memory.reinforcement_count >= 2)
        {
            return RetentionAction::Promote(RetentionTier::LongTerm);
        }
        RetentionAction::Keep
    }
}
