//! W5 提交（contracts.md 2.5）：执行已核验的 W4 决定，不交给旧确定性策略重判。
//!
//! 一次调用处理一个依赖组，对应一个 SQLite 写事务（T2）；回执按组 ID 保存，重放同一组
//! 不产生任何变更。提交必须持有该原文任务的有效认领（attempt_id、running、租约未过期），
//! 否则 claim_lost 且零写入。W4 输入时记下每个目标的快照指纹；提交时目标已变、更新的原文
//! 已先应用、所在任务已被标为过期或依据来源已删除，该组记为 needs_rejudge，零业务写入，
//! 且同一组以后只返回 needs_rejudge，不能重试旧决定（docs/architecture/w5-acceptance.md
//! A1–A7）。候选的证据必须来自所认领的原文；租约按取得写锁后的时间判断。
//! 存储形状（docs/architecture/w5-storage.md，版本 8）已一次定好：关系、更新状态、归并映射、
//! 回执格式标记，以及组与决定上后续各段要用的字段。
//!
//! 更新状态（w5-storage.md 第 3 节）：每个决定的 UpdateState 由代码按检索范围与 W4 待定原因
//! 生成，随回执保存；产生的记录同时写入 `record_updates`。待定决定不对旧目标执行动作：
//! 断言作为新记录提交并标 pending，撤回零写入（候选与原文由上游保留）。
//! 尚未执行：冲突关系、重判复用与归并；携带这些字段的组目前一律拒绝，不静默忽略。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::*;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
pub enum W5Action {
    New,
    Reinforce,
    StateChange,
    Retract,
    /// 新断言与目标真实冲突：插入新记录并建立冲突关系（第 3 段实现）。
    Conflict,
}

/// 更新判定状态（contracts 1.6）；由代码生成，不由 W4 直接给出。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub enum UpdateStatus {
    Judged,
    Pending,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub enum UpdateReason {
    UpdateScopeIncomplete,
    UpdateTargetUnknown,
    UpdateRelationUnclear,
}

#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct UpdateState {
    pub status: UpdateStatus,
    pub reasons: Vec<UpdateReason>,
}

/// 一次显式重判覆盖了哪些缺口（w5-storage.md 第 3 节）。只有覆盖了的旧原因才清除。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RejudgeCoverage {
    /// 本次组的检索范围完整。
    pub scope_complete: bool,
    /// 本次对该记录给出了已定决定（不是待定）。
    pub decided: bool,
    /// 本次 W4 输入包含该记录的前身或相关旧成员及其最新关系状态。
    pub relation_inputs: bool,
}

impl UpdateState {
    /// 由原因生成：去重、排序；有原因即 pending，否则 judged。
    pub fn from_reasons(mut reasons: Vec<UpdateReason>) -> Self {
        reasons.sort();
        reasons.dedup();
        let status = if reasons.is_empty() {
            UpdateStatus::Judged
        } else {
            UpdateStatus::Pending
        };
        Self { status, reasons }
    }

    /// 显式重判后的状态：本次新缺口与旧缺口合并，旧缺口逐项判断是否被本次覆盖——
    /// 一项被覆盖不代表其他项也解决。普通召回、整理完成、重放都不调用它。
    pub fn after_rejudge(
        previous: &UpdateState,
        fresh: &UpdateState,
        coverage: RejudgeCoverage,
    ) -> Self {
        let kept = previous
            .reasons
            .iter()
            .copied()
            .filter(|reason| match reason {
                UpdateReason::UpdateScopeIncomplete => !coverage.scope_complete,
                UpdateReason::UpdateTargetUnknown => !coverage.decided,
                UpdateReason::UpdateRelationUnclear => {
                    !(coverage.decided && coverage.relation_inputs)
                }
            });
        Self::from_reasons(fresh.reasons.iter().copied().chain(kept).collect())
    }
}

/// 旧格式输出（memory_list、召回的记录标注）里附上记录的更新状态。只在有条目时附加：
/// 没有条目表示未知（与 null 同义），这样旧记录的输出保持原样。
pub(crate) fn attach_update(
    value: &mut serde_json::Value,
    updates: &BTreeMap<MemoryId, UpdateState>,
    id: MemoryId,
) {
    if let (Some(update), Some(object)) = (updates.get(&id), value.as_object_mut()) {
        object.insert("update".into(), serde_json::json!(update));
    }
}

/// 一个决定的更新状态：检索范围不完整必含 update_scope_incomplete（即使对明确目标的动作已
/// 执行）；W4 待定原因映射为对应的更新原因。
fn decision_update(group: &W5Group, decision: &W5Decision) -> UpdateState {
    let mut reasons = Vec::new();
    if !group.scope_complete {
        reasons.push(UpdateReason::UpdateScopeIncomplete);
    }
    match decision.pending_reason {
        Some(W4PendingReason::ScopeIncomplete) => reasons.push(UpdateReason::UpdateScopeIncomplete),
        Some(W4PendingReason::TargetUnknown) => reasons.push(UpdateReason::UpdateTargetUnknown),
        Some(W4PendingReason::UnclearRelation) => reasons.push(UpdateReason::UpdateRelationUnclear),
        None => {}
    }
    UpdateState::from_reasons(reasons)
}

/// W4 待定决定的原因（contracts 2.4 `pending_reason`）。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum W4PendingReason {
    TargetUnknown,
    ScopeIncomplete,
    UnclearRelation,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RelationKind {
    Conflict,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RelationStatus {
    Open,
    Resolved,
    Invalidated,
}

/// 关系依据：来源 ID 与原文片段（contracts EvidenceRef 的已保存来源形式）。
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RelationEvidence {
    pub source_id: String,
    pub quote: String,
}

/// 冲突关系（contracts 2.4 RelationLink），与 Memory 生命周期分开保存在 owner 快照中。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct StoredRelation {
    pub id: String,
    pub kind: RelationKind,
    /// 至少两个，不重复。
    pub members: Vec<MemoryId>,
    /// TextQualifier / TimeInterp 原样保存，W5 不解释。
    pub conditions: serde_json::Value,
    pub times: serde_json::Value,
    pub basis: Vec<RelationEvidence>,
    pub status: RelationStatus,
    /// resolved 时必填。
    pub resolution: Option<RelationEvidence>,
    /// 建立它的 W5 组。
    pub created_by: String,
    /// 解决或失效它的组；因删除失效时为 null。
    pub closed_by: Option<String>,
}

/// conflict 决定要建立的关系内容（成员 = 新产物 + 目标，由代码补齐）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct W5RelationSpec {
    pub conditions: serde_json::Value,
    pub times: serde_json::Value,
    pub basis: Vec<RelationEvidence>,
}

/// 回执的组指纹形态。迁移到版本 8 时写入，此后不靠“某字段缺失”推断来源版本。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptForm {
    /// 版本 8 之前写的回执：按去掉版本 8 新字段的组形态比对（再叠加 legacy_fingerprint）。
    Pre8,
    V8,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
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
    /// W4 明确判定本次动作解决的 open 关系（第 3 段）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resolves_relations: Vec<String>,
    /// 解决依据；resolves_relations 非空时必填（第 3 段）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<RelationEvidence>,
    /// conflict 决定要建立的关系（第 3 段）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relation: Option<W5RelationSpec>,
    /// W4 待定决定：不对旧目标执行动作（第 2 段）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_reason: Option<W4PendingReason>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct W5Candidate {
    pub index: usize,
    pub candidate: CandidateMemory,
    /// 重判已提交断言时为原记录 ID（第 4 段）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub committed_id: Option<MemoryId>,
}

/// W4 输入时某个目标的快照指纹（不含检索计数，读取不算变更）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct W5Seen {
    pub id: MemoryId,
    pub fingerprint: String,
    /// 目标所在 open 关系的编号集合；集合增减即快照过期（第 3 段核对）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relations: Vec<String>,
}

/// W4 输入时某个关系的快照指纹（要解决的关系、目标及既有冲突成员所在的 open 关系）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct W5SeenRelation {
    pub id: String,
    pub fingerprint: String,
}

/// 一个依赖组：同一原文里彼此依赖的决定，一次调用、一个事务。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct W5Group {
    /// 稳定的组 ID（例如 `<source_id>#<组序号>`），用作回执与幂等键。
    pub id: String,
    /// 本组所属原文任务在 `PersistedMemoryState::sources` 中的键。
    pub source_key: String,
    pub candidates: Vec<W5Candidate>,
    pub decisions: Vec<W5Decision>,
    /// W4 输入时全部目标的快照指纹；每个目标都必须有。
    pub seen: Vec<W5Seen>,
    /// 运行时代码为 W4 输入生成的 coverage.complete；W5 不重新判断检索范围。缺省为
    /// false（不完整），与通用 fact 检索范围永不完整一致。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub scope_complete: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub seen_relations: Vec<W5SeenRelation>,
}

/// 执行者对原文任务的认领；沿用 `EnrichmentTask` 的 attempt_id 与租约。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct W5Claim {
    pub source_key: String,
    pub attempt_id: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub enum W5Status {
    Committed,
    /// 快照已过期：本组零业务写入，须重新走 W4，不能重试旧决定。
    NeedsRejudge {
        reason: String,
    },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct W5Receipt {
    pub group_id: String,
    /// 组内容的规范 JSON；同一组 ID 只接受完全相同的内容重放。
    pub fingerprint: String,
    /// v6 receipts (W5 A1–A3) had no status; all of them were committed.
    #[serde(default = "committed_status")]
    pub status: W5Status,
    /// Receipts written before this flag existed (bf83fd9, baa1ff9, 708b2d2) may hold either
    /// the v6 group fingerprint (no `source_key`, no `seen`; bf83fd9) or the current one
    /// (baa1ff9, 708b2d2), and the flag's absence cannot tell them apart. Such receipts
    /// accept a replay in either form; new receipts write false and accept only the current
    /// form.
    #[serde(default = "legacy_fingerprint_default")]
    pub legacy_fingerprint: bool,
    /// 候选 → 本次产生的记录（new、state_change）。
    pub products: BTreeMap<usize, MemoryId>,
    /// 完整 targets 与 basis。
    pub decisions: Vec<W5Decision>,
    /// 候选 → 其产物的更新状态；版本 8 之前的回执为空，表示未知而非 judged。
    #[serde(default)]
    pub updates: BTreeMap<usize, UpdateState>,
    /// 重判归并：已提交 ID → 保留目标（第 4 段写入）。
    #[serde(default)]
    pub merged: BTreeMap<MemoryId, MemoryId>,
    /// 缺省只可能来自版本 8 之前写的回执；迁移到 8 时显式写出。
    #[serde(default = "pre8_form")]
    pub form: ReceiptForm,
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
                relations: Vec::new(),
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

fn pre8_form() -> ReceiptForm {
    ReceiptForm::Pre8
}

/// 版本 8 之前的组形态：去掉 scope_complete、seen_relations 与 W5Seen.relations
/// （都在为空 / false 时不序列化）。
fn pre8_fingerprint(group: &W5Group) -> PersistenceResult<String> {
    let mut group = group.clone();
    group.scope_complete = false;
    group.seen_relations.clear();
    for seen in &mut group.seen {
        seen.relations.clear();
    }
    Ok(serde_json::to_string(&group)?)
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
            let same = match receipt.form {
                ReceiptForm::V8 => receipt.fingerprint == fingerprint,
                ReceiptForm::Pre8 => {
                    receipt.fingerprint == pre8_fingerprint(group)?
                        || (receipt.legacy_fingerprint
                            && receipt.fingerprint == legacy_fingerprint(group)?)
                }
            };
            if !same {
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
                updates: BTreeMap::new(),
                merged: BTreeMap::new(),
                form: ReceiptForm::V8,
            };
            state.w5_receipts.insert(group.id.clone(), receipt.clone());
            return Ok(W5Outcome::NeedsRejudge(receipt));
        }
        let mut products = BTreeMap::new();
        let mut updates = BTreeMap::new();
        for (step, decision) in group.decisions.iter().enumerate() {
            let candidate = &group
                .candidates
                .iter()
                .find(|c| c.index == decision.candidate)
                .ok_or("W5 decision refers to a missing candidate")?
                .candidate;
            let update = decision_update(group, decision);
            if let Some(id) = apply(&mut state.repository, candidate, decision)? {
                products.insert(decision.candidate, id);
                state.record_updates.insert(id, update.clone());
            }
            updates.insert(decision.candidate, update);
            hook(W5Step::AfterDecision(step))?;
        }
        let receipt = W5Receipt {
            group_id: group.id.clone(),
            fingerprint,
            status: W5Status::Committed,
            legacy_fingerprint: false,
            products,
            decisions: group.decisions.clone(),
            updates,
            merged: BTreeMap::new(),
            form: ReceiptForm::V8,
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
    // 存储已为后续各段备好的字段，执行尚未实现：拒绝，而不是静默忽略。
    if !group.seen_relations.is_empty() {
        return Err("W5 seen_relations are not supported yet".into());
    }
    if group.seen.iter().any(|seen| !seen.relations.is_empty())
        || group.candidates.iter().any(|c| c.committed_id.is_some())
    {
        return Err("W5 relation snapshots / committed_id are not supported yet".into());
    }
    for d in &group.decisions {
        if d.action == W5Action::Conflict
            || !d.resolves_relations.is_empty()
            || d.resolution.is_some()
            || d.relation.is_some()
        {
            return Err(format!(
                "W5 decision {}: conflict and relation resolution are not supported yet",
                d.candidate
            )
            .into());
        }
    }
    for d in &group.decisions {
        let Some(c) = group.candidates.iter().find(|c| c.index == d.candidate) else {
            return Err(format!("W5 decision refers to missing candidate {}", d.candidate).into());
        };
        // 候选类别与动作必须一致：撤回候选没有可写入的值，只能走 retract（待定时零写入）；
        // retract 也只接受撤回候选，不能把一条断言当撤回去结束目标。
        let retraction = matches!(c.candidate.intent, CandidateIntent::Retract { .. });
        if retraction != (d.action == W5Action::Retract) {
            return Err(format!(
                "W5 decision {}: {:?} does not match {} candidate",
                d.candidate,
                d.action,
                if retraction {
                    "a retraction"
                } else {
                    "an assertion"
                }
            )
            .into());
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
        // 待定决定不对旧目标执行动作：断言作为新记录追加，撤回零写入；targets 可为空或只列
        // 已定位的目标（仍须有快照），不修改它们。
        if d.pending_reason.is_some() {
            if !matches!(d.action, W5Action::New | W5Action::Retract) {
                return Err(format!(
                    "W5 decision {}: a pending decision only appends an assertion or keeps a retraction",
                    d.candidate
                )
                .into());
            }
            if d.effective.is_some() || d.reason.is_some() {
                return Err(format!(
                    "W5 decision {}: a pending decision does not end a target",
                    d.candidate
                )
                .into());
            }
            for target in &d.targets {
                if !group.seen.iter().any(|seen| seen.id == *target) {
                    return Err(format!("W5 target {} has no W4 snapshot", target.0).into());
                }
            }
            continue;
        }
        let ok = match d.action {
            W5Action::New => d.targets.is_empty(),
            W5Action::Reinforce => d.targets.len() == 1,
            W5Action::StateChange | W5Action::Retract | W5Action::Conflict => !d.targets.is_empty(),
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
    if decision.pending_reason.is_some() {
        return match decision.action {
            // 合格断言追加为新记录，不与列出的目标做任何新旧处理。
            W5Action::New => {
                let id = repository.allocate_id()?;
                repository.insert(Memory::from_candidate(id, candidate, None))?;
                Ok(Some(id))
            }
            // 撤回没有可执行的目标：不写入（validate 已限定动作）。
            _ => Ok(None),
        };
    }
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
        W5Action::Conflict => Err("W5 conflict is not supported yet".into()),
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
