//! W5 提交（contracts.md 2.5）：执行已核验的 W4 决定，不交给旧确定性策略重判。
//!
//! 一次调用处理一个依赖组，对应一个 SQLite 写事务（T2）；回执按组 ID 保存，重放同一组
//! 不产生任何变更。提交必须持有该原文任务的有效认领（attempt_id、running、租约未过期），
//! 否则 claim_lost 且零写入。W4 输入时记下每个目标的快照指纹；提交时目标已变、更新的原文
//! 已先应用、所在任务已被标为过期或依据来源已删除，该组记为 needs_rejudge，零业务写入，
//! 且同一组以后只返回 needs_rejudge，不能重试旧决定（docs/architecture/w5-acceptance.md
//! A1–A7）。候选的证据必须来自所认领的原文；租约按取得写锁后的时间判断。
//! 尚未覆盖：关系、重判复用与归并、UpdateState。

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

/// W4 输入时某个目标的快照指纹（不含检索计数，读取不算变更）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct W5Seen {
    pub id: MemoryId,
    pub fingerprint: String,
}

/// 一个依赖组：同一原文里彼此依赖的决定，一次调用、一个事务。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct W5Group {
    /// 稳定的组 ID（例如 `<source_id>#<组序号>`），用作回执与幂等键。
    pub id: String,
    /// 本组所属原文任务在 `PersistedMemoryState::sources` 中的键。
    pub source_key: String,
    pub candidates: Vec<W5Candidate>,
    pub decisions: Vec<W5Decision>,
    /// W4 输入时全部目标的快照指纹；每个目标都必须有。
    pub seen: Vec<W5Seen>,
}

/// 执行者对原文任务的认领；沿用 `EnrichmentTask` 的 attempt_id 与租约。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct W5Claim {
    pub source_key: String,
    pub attempt_id: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum W5Status {
    Committed,
    /// 快照已过期：本组零业务写入，须重新走 W4，不能重试旧决定。
    NeedsRejudge {
        reason: String,
    },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct W5Receipt {
    pub group_id: String,
    /// 组内容的规范 JSON；同一组 ID 只接受完全相同的内容重放。
    pub fingerprint: String,
    /// v6 receipts (W5 A1–A3) had no status; all of them were committed.
    #[serde(default = "committed_status")]
    pub status: W5Status,
    /// v6 receipts fingerprinted the group before `source_key` and `seen` existed, so a
    /// replay is compared in that older form. Absent in stored JSON means a v6 receipt.
    #[serde(default = "legacy_fingerprint_default")]
    pub legacy_fingerprint: bool,
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
    /// 快照已过期，零业务写入（重放同一组也返回它）。
    NeedsRejudge(W5Receipt),
}

/// 测试用故障注入点。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum W5Step {
    /// 第 n 个决定执行之后（从 0 开始）。
    AfterDecision(usize),
    /// 全部决定执行后、写回执与保存之前。
    BeforeSave,
}

/// 任务结束时写回原文任务状态并释放认领。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum W5TaskEnd {
    /// 全部组已提交。
    Committed,
    /// 普通故障：沿用现有 `failed`，可重新认领后重试。
    Failed(String),
    /// 快照过期：沿用现有 `stale_requires_review`，须重新走 W4，不能重试旧决定。
    NeedsRejudge(String),
}

/// 目标在 W4 输入时的指纹；不含检索计数，读取不算变更。
pub fn w5_fingerprint(memory: &Memory) -> PersistenceResult<String> {
    let mut memory = memory.clone();
    memory.retrieval_count = 0;
    memory.last_retrieved_at = None;
    Ok(serde_json::to_string(&memory)?)
}

/// 为 W4 输入中的目标记下快照指纹。
pub fn w5_seen(
    repository: &InMemoryMemoryRepository,
    ids: &[MemoryId],
) -> PersistenceResult<Vec<W5Seen>> {
    ids.iter()
        .map(|id| {
            let memory = repository
                .get(*id)?
                .ok_or_else(|| format!("W5 target {} does not exist", id.0))?;
            Ok(W5Seen {
                id: *id,
                fingerprint: w5_fingerprint(&memory)?,
            })
        })
        .collect()
}

fn committed_status() -> W5Status {
    W5Status::Committed
}

fn legacy_fingerprint_default() -> bool {
    true
}

/// The group as v6 (W5 A1–A3) serialized it: no `source_key`, no `seen`.
fn legacy_fingerprint(group: &W5Group) -> PersistenceResult<String> {
    #[derive(Serialize)]
    struct V6Group<'a> {
        id: &'a str,
        candidates: &'a [W5Candidate],
        decisions: &'a [W5Decision],
    }
    Ok(serde_json::to_string(&V6Group {
        id: &group.id,
        candidates: &group.candidates,
        decisions: &group.decisions,
    })?)
}

fn now() -> PersistenceResult<i64> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs()
        .try_into()?)
}

/// 认领仍属于本执行者：attempt_id 相同、任务 running、租约未过期、原文未删除。
fn owned<'a>(
    state: &'a PersistedMemoryState,
    claim: &W5Claim,
    now: i64,
) -> PersistenceResult<&'a SourceRecord> {
    let source = state
        .sources
        .get(&claim.source_key)
        .ok_or("claim_lost: source missing")?;
    if source.task.attempt_id.as_deref() != Some(claim.attempt_id.as_str())
        || source.request.is_none()
        || source.task.lease_until.is_none_or(|until| until < now)
    {
        return Err("claim_lost".into());
    }
    if !matches!(
        source.enrichment.as_str(),
        "running" | "stale_requires_review"
    ) {
        return Err("claim_lost".into());
    }
    Ok(source)
}

pub fn commit_group(
    store: &mut SqliteMemoryStore,
    claim: &W5Claim,
    group: &W5Group,
) -> PersistenceResult<W5Outcome> {
    commit_group_with_hook(
        store,
        claim,
        group,
        &mut |_: W5Step| -> PersistenceResult<()> { Ok(()) },
    )
}

/// 与 `commit_group` 相同，但在各步骤调用 `hook`；hook 返回错误时整组回滚。
pub fn commit_group_with_hook(
    store: &mut SqliteMemoryStore,
    claim: &W5Claim,
    group: &W5Group,
    hook: &mut dyn FnMut(W5Step) -> PersistenceResult<()>,
) -> PersistenceResult<W5Outcome> {
    let fingerprint = serde_json::to_string(group)?;
    let owner = store.owner().clone();
    store.transaction(|state| {
        // Read the clock only after the write lock is held: a lease may expire while waiting.
        let now = now()?;
        if claim.source_key != group.source_key {
            return Err("W5 claim and group belong to different sources".into());
        }
        let source = owned(state, claim, now)?;
        let (source_id, received_sequence, marked_stale) = (
            source.id.clone(),
            source.received_sequence,
            source.enrichment == "stale_requires_review",
        );
        let source_turn = (source.session_id.clone(), source.turn_id.clone());
        if let Some(receipt) = state.w5_receipts.get(&group.id) {
            let expected = if receipt.legacy_fingerprint {
                legacy_fingerprint(group)?
            } else {
                fingerprint.clone()
            };
            if receipt.fingerprint != expected {
                return Err(format!(
                    "W5 group {} was already committed with different content",
                    group.id
                )
                .into());
            }
            return Ok(match receipt.status {
                W5Status::Committed => W5Outcome::AlreadyApplied(receipt.clone()),
                W5Status::NeedsRejudge { .. } => W5Outcome::NeedsRejudge(receipt.clone()),
            });
        }
        validate(group, &owner)?;
        // Every candidate must come from the claimed source: a valid claim on one source cannot
        // carry candidates of another (possibly deleted) source.
        for c in &group.candidates {
            if c.candidate.evidence.is_empty()
                || c.candidate.evidence.iter().any(|e| {
                    (e.session_id.0.as_str(), e.turn_id.0.as_str())
                        != (source_turn.0.as_str(), source_turn.1.as_str())
                })
            {
                return Err(format!(
                    "W5 candidate {} does not come from the claimed source {}",
                    c.index, claim.source_key
                )
                .into());
            }
        }
        if let Some(reason) =
            stale_reason(state, group, &source_id, received_sequence, marked_stale)?
        {
            let receipt = W5Receipt {
                group_id: group.id.clone(),
                fingerprint,
                status: W5Status::NeedsRejudge { reason },
                legacy_fingerprint: false,
                products: BTreeMap::new(),
                decisions: group.decisions.clone(),
            };
            state.w5_receipts.insert(group.id.clone(), receipt.clone());
            return Ok(W5Outcome::NeedsRejudge(receipt));
        }
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
            status: W5Status::Committed,
            legacy_fingerprint: false,
            products,
            decisions: group.decisions.clone(),
        };
        hook(W5Step::BeforeSave)?;
        // The change belongs to this source: advance the applied watermark (a later group of
        // the same source keeps it and marks the change as attributed, not source-less).
        if received_sequence > state.last_applied_source_sequence {
            state.last_applied_source_sequence = received_sequence;
        } else {
            state.source_attributed_change = true;
        }
        state.w5_receipts.insert(group.id.clone(), receipt.clone());
        Ok(W5Outcome::Committed(receipt))
    })
}

/// 快照是否过期；返回原因。只读检查，不写入。
fn stale_reason(
    state: &PersistedMemoryState,
    group: &W5Group,
    source_id: &str,
    received_sequence: u64,
    marked_stale: bool,
) -> PersistenceResult<Option<String>> {
    if marked_stale {
        return Ok(Some("source task marked stale_requires_review".into()));
    }
    if received_sequence < state.last_applied_source_sequence {
        return Ok(Some("newer_source_applied".into()));
    }
    if received_sequence <= state.source_less_barrier_sequence {
        return Ok(Some("source_less_authority_changed".into()));
    }
    for d in &group.decisions {
        for basis in &d.basis_sources {
            if basis == source_id {
                continue;
            }
            if state
                .sources
                .values()
                .any(|s| &s.id == basis && s.request.is_none())
            {
                return Ok(Some(format!("basis source {basis} deleted")));
            }
        }
    }
    for seen in &group.seen {
        let current = match state.repository.get(seen.id)? {
            Some(memory) => w5_fingerprint(&memory)?,
            None => return Ok(Some(format!("target {} no longer exists", seen.id.0))),
        };
        if current != seen.fingerprint {
            return Ok(Some(format!("target {} changed since W4 input", seen.id.0)));
        }
    }
    Ok(None)
}

/// 结束原文任务：写回任务状态并释放认领；认领已不属于本执行者时零写入并报 claim_lost。
pub fn w5_finish_task(
    store: &mut SqliteMemoryStore,
    claim: &W5Claim,
    end: &W5TaskEnd,
) -> PersistenceResult<()> {
    store.transaction(|state| {
        let now = now()?;
        owned(state, claim, now)?;
        let source = state
            .sources
            .get_mut(&claim.source_key)
            .ok_or("claim_lost: source missing")?;
        let (status, error) = match end {
            W5TaskEnd::Committed => ("committed", None),
            W5TaskEnd::Failed(reason) => ("failed", Some(reason.clone())),
            W5TaskEnd::NeedsRejudge(reason) => (
                "stale_requires_review",
                Some(format!("w5_needs_rejudge: {reason}")),
            ),
        };
        source.enrichment = status.into();
        source.error = error;
        source.task.attempt_id = None;
        source.task.lease_until = None;
        source.task.claim_revision = None;
        Ok(())
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
        for target in &d.targets {
            if !group.seen.iter().any(|seen| seen.id == *target) {
                return Err(format!("W5 target {} has no W4 snapshot", target.0).into());
            }
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
