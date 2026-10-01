//! W5 提交（contracts.md 2.5）：执行已核验的 W4 决定，不交给旧确定性策略重判。
//!
//! 第一段只覆盖 new、reinforce、state_change、retract，以及“提交→重载→故障回滚→
//! 重复重试”（docs/architecture/w5-acceptance.md A1–A3）。一次调用处理一个依赖组，
//! 对应一个 SQLite 写事务（T2）；回执按组 ID 保存，重放同一组不产生任何变更。
//! 尚未覆盖：快照过期与 needs_rejudge、任务认领、关系、重判复用与归并、UpdateState。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::*;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
pub enum W5Action {
    New,
    Reinforce,
    StateChange,
    Retract,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct W5Decision {
    /// 对应 `W5Group::candidates` 中的 `index`。
    pub candidate: usize,
    pub action: W5Action,
    /// 完整目标；state_change 可有多个，全部在同一事务内结束。
    pub targets: Vec<MemoryId>,
    /// 决定依据的来源 ID，原样保存在回执中。
    pub basis_sources: Vec<String>,
    /// state_change 的结束原因；缺省为 StateChanged。
    pub reason: Option<SupersedeReason>,
    /// 已核验的生效时间；`None` 表示结束时间未知（end_unknown），不回填接收时间。
    pub effective: Option<DatedValue>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct W5Candidate {
    pub index: usize,
    pub candidate: CandidateMemory,
}

/// 一个依赖组：同一原文里彼此依赖的决定，一次调用、一个事务。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct W5Group {
    /// 稳定的组 ID（例如 `<source_id>#<组序号>`），用作回执与幂等键。
    pub id: String,
    pub candidates: Vec<W5Candidate>,
    pub decisions: Vec<W5Decision>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct W5Receipt {
    pub group_id: String,
    /// 组内容的规范 JSON；同一组 ID 只接受完全相同的内容重放。
    pub fingerprint: String,
    /// 候选 → 本次产生的记录（new、state_change）。
    pub products: BTreeMap<usize, MemoryId>,
    /// 完整 targets 与 basis。
    pub decisions: Vec<W5Decision>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum W5Outcome {
    Committed(W5Receipt),
    /// 同一组已提交过：不做任何变更，返回原回执。
    AlreadyApplied(W5Receipt),
}

/// 测试用故障注入点。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum W5Step {
    /// 第 n 个决定执行之后（从 0 开始）。
    AfterDecision(usize),
    /// 全部决定执行后、写回执与保存之前。
    BeforeSave,
}

pub fn commit_group(
    store: &mut SqliteMemoryStore,
    group: &W5Group,
) -> PersistenceResult<W5Outcome> {
    commit_group_with_hook(store, group, &mut |_: W5Step| -> PersistenceResult<()> {
        Ok(())
    })
}

/// 与 `commit_group` 相同，但在各步骤调用 `hook`；hook 返回错误时整组回滚。
pub fn commit_group_with_hook(
    store: &mut SqliteMemoryStore,
    group: &W5Group,
    hook: &mut dyn FnMut(W5Step) -> PersistenceResult<()>,
) -> PersistenceResult<W5Outcome> {
    let fingerprint = serde_json::to_string(group)?;
    let owner = store.owner().clone();
    store.transaction(|state| {
        if let Some(receipt) = state.w5_receipts.get(&group.id) {
            if receipt.fingerprint == fingerprint {
                return Ok(W5Outcome::AlreadyApplied(receipt.clone()));
            }
            return Err(format!(
                "W5 group {} was already committed with different content",
                group.id
            )
            .into());
        }
        validate(group, &owner)?;
        let mut products = BTreeMap::new();
        for (step, decision) in group.decisions.iter().enumerate() {
            let candidate = &group
                .candidates
                .iter()
                .find(|c| c.index == decision.candidate)
                .ok_or("W5 decision refers to a missing candidate")?
                .candidate;
            if let Some(id) = apply(&mut state.repository, candidate, decision)? {
                products.insert(decision.candidate, id);
            }
            hook(W5Step::AfterDecision(step))?;
        }
        let receipt = W5Receipt {
            group_id: group.id.clone(),
            fingerprint,
            products,
            decisions: group.decisions.clone(),
        };
        hook(W5Step::BeforeSave)?;
        state.w5_receipts.insert(group.id.clone(), receipt.clone());
        Ok(W5Outcome::Committed(receipt))
    })
}

fn validate(group: &W5Group, owner: &UserId) -> PersistenceResult<()> {
    if group.id.trim().is_empty() {
        return Err("W5 group id must not be empty".into());
    }
    // 候选编号必须唯一，否则执行时只会取到第一个，其余被静默忽略。
    let mut indexes: Vec<usize> = group.candidates.iter().map(|c| c.index).collect();
    indexes.sort_unstable();
    indexes.dedup();
    if indexes.len() != group.candidates.len() {
        return Err("W5 candidate indexes must be unique".into());
    }
    for d in &group.decisions {
        if !group.candidates.iter().any(|c| c.index == d.candidate) {
            return Err(format!("W5 decision refers to missing candidate {}", d.candidate).into());
        }
    }
    for c in &group.candidates {
        if c.candidate.subject_id().is_none() {
            return Err(format!("W5 candidate {} has an unresolved subject", c.index).into());
        }
        if &c.candidate.owner_id != owner {
            return Err(format!("W5 candidate {} belongs to another owner", c.index).into());
        }
        if group
            .decisions
            .iter()
            .filter(|d| d.candidate == c.index)
            .count()
            != 1
        {
            return Err(format!("W5 candidate {} must have exactly one decision", c.index).into());
        }
    }
    for d in &group.decisions {
        let mut targets = d.targets.clone();
        targets.sort();
        targets.dedup();
        if targets.len() != d.targets.len() {
            return Err(format!("W5 decision {} has duplicate targets", d.candidate).into());
        }
        let ok = match d.action {
            W5Action::New => d.targets.is_empty(),
            W5Action::Reinforce => d.targets.len() == 1,
            W5Action::StateChange | W5Action::Retract => !d.targets.is_empty(),
        };
        if !ok {
            return Err(format!(
                "W5 decision {} has an invalid target count for {:?}",
                d.candidate, d.action
            )
            .into());
        }
        if d.effective.is_some() && d.action != W5Action::StateChange {
            return Err(format!(
                "W5 decision {}: only state_change carries an effective time",
                d.candidate
            )
            .into());
        }
    }
    Ok(())
}

fn active(repository: &InMemoryMemoryRepository, id: MemoryId) -> PersistenceResult<Memory> {
    let memory = repository
        .get(id)?
        .ok_or_else(|| format!("W5 target {} does not exist", id.0))?;
    if memory.status != MemoryStatus::Active {
        return Err(format!("W5 target {} is not active", id.0).into());
    }
    Ok(memory)
}

fn apply(
    repository: &mut InMemoryMemoryRepository,
    candidate: &CandidateMemory,
    decision: &W5Decision,
) -> PersistenceResult<Option<MemoryId>> {
    match decision.action {
        W5Action::New => {
            let id = repository.allocate_id()?;
            repository.insert(Memory::from_candidate(id, candidate, None))?;
            Ok(Some(id))
        }
        W5Action::Reinforce => {
            let mut target = active(repository, decision.targets[0])?;
            // 同一来源的证据只计一次：已在目标上的证据（同 ID 或同一轮）不再计数。
            let fresh: Vec<Evidence> = candidate
                .evidence
                .iter()
                .filter(|e| {
                    !target
                        .evidence
                        .iter()
                        .any(|x| x.id == e.id || x.same_turn(e))
                })
                .cloned()
                .collect();
            if !fresh.is_empty() {
                target.reinforcement_count += 1;
                target.last_reinforced_at = Some(candidate.temporal.observed_at);
                target.evidence.extend(fresh);
                repository.update(target)?;
            }
            Ok(None)
        }
        W5Action::StateChange => {
            let reason = decision.reason.unwrap_or(SupersedeReason::StateChanged);
            if !matches!(
                reason,
                SupersedeReason::StateChanged
                    | SupersedeReason::PreferenceChanged
                    | SupersedeReason::ConstraintChanged
            ) {
                return Err(format!("W5 state_change cannot end with {reason:?}").into());
            }
            // 先核对全部目标，任一不可用则整组失败，不产生部分结果。
            let current: &InMemoryMemoryRepository = repository;
            let olds = decision
                .targets
                .iter()
                .map(|id| active(current, *id))
                .collect::<PersistenceResult<Vec<_>>>()?;
            let id = repository.allocate_id()?;
            let mut new = Memory::from_candidate(id, candidate, decision.targets.first().copied());
            if let Some(effective) = decision.effective {
                new.temporal.valid_from = Some(effective);
            }
            for mut old in olds {
                old.status = MemoryStatus::Superseded;
                old.superseded_by = Some(id);
                old.supersede_reason = Some(reason);
                // effective 未知时结束时间保持未知，不回填接收时间（契约 2.5）。
                old.temporal.valid_to = decision.effective;
                repository.update(old)?;
            }
            repository.insert(new)?;
            Ok(Some(id))
        }
        W5Action::Retract => {
            for target in &decision.targets {
                let mut old = active(repository, *target)?;
                old.evidence.extend(candidate.evidence.iter().cloned());
                old.status = MemoryStatus::Superseded;
                old.superseded_by = None;
                old.supersede_reason = Some(SupersedeReason::UserRetracted);
                old.temporal.valid_to = None;
                repository.update(old)?;
            }
            Ok(None)
        }
    }
}
