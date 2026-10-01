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
//! 冲突关系（w5-storage.md 第 4 节）：conflict 决定插入新记录并与全部目标组成 open 关系，旧版本
//! 不结束；resolves_relations 在同一事务内把关系置为 resolved 并保存解决依据；替代 open 关系成员
//! 而本组未解决该关系时，关系与成员不变，替代记录标 update_relation_unclear。关系快照（目标所在
//! open 关系编号集合与涉及关系的指纹）在写锁内重查，变化即整组 needs_rejudge。依据片段须逐字
//! 出现在获准保存的原文中。
//! 重判已提交断言（committed_id，第 4 段）：复用原记录，不再插入、不当作对自身的强化；状态按
//! 第 3 节逐项清除。重判为强化另一记录时按来源幂等并入证据，已提交记录退出投影（status=Deleted），
//! 回执 `merged` 记下已提交 ID → 保留目标。删除范围与关系失效见 `src/forget.rs`。

use std::collections::{BTreeMap, BTreeSet};

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

/// 读取侧看到的 W5 状态：每条记录的更新状态，以及它所在的 open 关系。从同一快照取出，
/// 供旧格式输出附加字段、答复门判断“能否作为唯一当前值”。
#[derive(Clone, Debug, Default)]
pub(crate) struct W5View {
    updates: BTreeMap<MemoryId, UpdateState>,
    open: BTreeMap<MemoryId, Vec<String>>,
    /// 完整候选里有现有 Memory 表达不了的限定语的记录，附上这些限定语原文。
    qualified: BTreeMap<MemoryId, serde_json::Value>,
}

impl W5View {
    pub(crate) fn of(state: &PersistedMemoryState) -> Self {
        let mut open: BTreeMap<MemoryId, Vec<String>> = BTreeMap::new();
        for relation in state
            .relations
            .values()
            .filter(|relation| relation.status == RelationStatus::Open)
        {
            for member in &relation.members {
                open.entry(*member).or_default().push(relation.id.clone());
            }
        }
        let mut qualified = BTreeMap::new();
        let committed: Vec<&W5Receipt> = state
            .w5_receipts
            .values()
            .filter(|receipt| receipt.status == W5Status::Committed)
            .collect();
        let mut with_contract = BTreeSet::new();
        for receipt in &committed {
            for (id, contract) in receipt_contracts(receipt) {
                with_contract.insert(id);
                if qualified.contains_key(&id) {
                    continue;
                }
                if !contract.lossless() {
                    let c = &contract.candidate;
                    qualified.insert(
                        id,
                        serde_json::json!({
                            "conditions": c.conditions,
                            "uncertainty": c.uncertainty,
                            "times": c.times,
                        }),
                    );
                }
            }
        }
        // W5 产生的记录找不到完整候选（创建回执已随删除清洗墓碑化，或没有完整形式）：限定语
        // 无从得知，不能默认为无限定，也不能恢复被删除的候选（复审 0016）。
        for receipt in &committed {
            for id in receipt.products.values() {
                if !with_contract.contains(id) {
                    qualified
                        .entry(*id)
                        .or_insert_with(|| serde_json::json!({"status": "unknown"}));
                }
            }
        }
        Self {
            updates: state.record_updates.clone(),
            open,
            qualified,
        }
    }

    /// 旧格式输出（memory_list、召回的记录标注）里附上记录的更新状态与所在 open 关系。只在有
    /// 条目时附加：没有条目表示未知（与 null 同义），这样旧记录的输出保持原样。
    pub(crate) fn attach(&self, value: &mut serde_json::Value, id: MemoryId) {
        let Some(object) = value.as_object_mut() else {
            return;
        };
        if let Some(update) = self.updates.get(&id) {
            object.insert("update".into(), serde_json::json!(update));
        }
        if let Some(relations) = self.open.get(&id) {
            object.insert("open_relations".into(), serde_json::json!(relations));
        }
        if let Some(qualifiers) = self.qualified.get(&id) {
            object.insert("qualifiers_unmapped".into(), qualifiers.clone());
        }
    }

    /// 限定语无法用现有 Memory 表达（或已无从得知）的记录：旧路径不能把它们当作无限定事实选取。
    pub(crate) fn unmapped_qualifiers(&self) -> BTreeSet<MemoryId> {
        self.qualified.keys().copied().collect()
    }

    /// 更新判定待定、处在未解决的冲突里，或带有现有 Memory 表达不了的限定语：不能被核准为
    /// 唯一的当前值，也不能当作无限定事实答复（contracts 1.3、1.6、2.4）。
    pub(crate) fn unsettled(&self, id: MemoryId) -> bool {
        self.updates
            .get(&id)
            .is_some_and(|update| update.status == UpdateStatus::Pending)
            || self.open.contains_key(&id)
            || self.qualified.contains_key(&id)
    }
}

/// 记录的完整候选：取产生它的已提交回执里对应候选的 `contract`（重判复用时各回执里的完整候选
/// 必须相同）。删除清洗会把创建回执整体墓碑化，此时没有完整候选，读取侧回退为旧记录视图。
pub fn w5_record_contract(
    state: &PersistedMemoryState,
    id: MemoryId,
) -> Option<crate::contract::ContractCandidate> {
    state
        .w5_receipts
        .values()
        .filter(|receipt| receipt.status == W5Status::Committed)
        .find_map(|receipt| {
            receipt_contracts(receipt)
                .into_iter()
                .find(|(product, _)| *product == id)
                .map(|(_, contract)| contract)
        })
}

/// 一份回执里各产物的完整候选（指纹已墓碑化或候选没有完整形式时没有）。
fn receipt_contracts(receipt: &W5Receipt) -> Vec<(MemoryId, crate::contract::ContractCandidate)> {
    let Ok(group) = serde_json::from_str::<serde_json::Value>(&receipt.fingerprint) else {
        return Vec::new();
    };
    let entries = group["candidates"].as_array().cloned().unwrap_or_default();
    receipt
        .products
        .iter()
        .filter_map(|(index, id)| {
            let entry = entries
                .iter()
                .find(|c| c["index"].as_u64() == Some(*index as u64))?;
            let contract = serde_json::from_value(entry.get("contract")?.clone()).ok()?;
            Some((*id, contract))
        })
        .collect()
}

/// 记录所在的 open 关系编号（排序、去重）。
pub fn w5_open_relations_of(state: &PersistedMemoryState, id: MemoryId) -> Vec<String> {
    let mut ids: Vec<String> = state
        .relations
        .values()
        .filter(|relation| {
            relation.status == RelationStatus::Open && relation.members.contains(&id)
        })
        .map(|relation| relation.id.clone())
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// 关系的快照指纹。
pub fn w5_relation_fingerprint(relation: &StoredRelation) -> PersistenceResult<String> {
    Ok(serde_json::to_string(relation)?)
}

/// 为 W4 输入记下快照：每个目标的记录指纹及其所在 open 关系编号；涉及关系（要解决的关系与
/// 目标所在的 open 关系）各自的指纹。
pub fn w5_snapshot(
    state: &PersistedMemoryState,
    targets: &[MemoryId],
    resolves: &[String],
) -> PersistenceResult<(Vec<W5Seen>, Vec<W5SeenRelation>)> {
    // 要解决的关系须重查全部成员的快照（contracts 2.5），不只是本组的目标。
    let mut ids: Vec<MemoryId> = targets.to_vec();
    for id in resolves {
        let relation = state
            .relations
            .get(id)
            .ok_or_else(|| format!("W5 relation {id} does not exist"))?;
        ids.extend(relation.members.iter().copied());
    }
    ids.sort();
    ids.dedup();
    let mut seen = w5_seen(&state.repository, &ids)?;
    let mut involved: Vec<String> = resolves.to_vec();
    for entry in &mut seen {
        entry.relations = w5_open_relations_of(state, entry.id);
        involved.extend(entry.relations.iter().cloned());
    }
    involved.sort();
    involved.dedup();
    let relations = involved
        .into_iter()
        .map(|id| {
            let relation = state
                .relations
                .get(&id)
                .ok_or_else(|| format!("W5 relation {id} does not exist"))?;
            Ok(W5SeenRelation {
                fingerprint: w5_relation_fingerprint(relation)?,
                id,
            })
        })
        .collect::<PersistenceResult<Vec<_>>>()?;
    Ok((seen, relations))
}

/// 一个决定的更新状态：检索范围不完整必含 update_scope_incomplete（即使对明确目标的动作已
/// 执行）；W4 待定原因映射为对应的更新原因。
/// `replaces_unresolved`：本决定结束的目标是某个 open 关系的成员，而本组没有解决该关系
/// （contracts 2.5）——替代记录与旧成员之间的关系待后续 W4 判定，标 update_relation_unclear。
fn decision_update(
    group: &W5Group,
    decision: &W5Decision,
    replaces_unresolved: bool,
) -> UpdateState {
    let mut reasons = Vec::new();
    if replaces_unresolved {
        reasons.push(UpdateReason::UpdateRelationUnclear);
    }
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
    /// 完整的 w1.v1 候选（A 段）。为空时不序列化，旧组的指纹不变。记录的完整候选取产生它的
    /// 那次提交，之后不可变：重判必须带完全相同的完整候选。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract: Option<crate::contract::ContractCandidate>,
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
        // 快照失效，或 committed_id 不再对应同一来源、候选和有效版本（contracts 2.5）：
        // 整组 needs_rejudge，零业务写入。
        let reason = match stale_reason(state, group, &source_id, received_sequence, marked_stale)?
        {
            Some(reason) => Some(reason),
            None => committed_mismatch(state, group, &source_turn)?,
        };
        if let Some(reason) = reason {
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
        check_relation_evidence(state, group, &source_id)?;
        // 本组解决的关系；替代 open 关系成员而本组未解决该关系时，产物标 relation_unclear。
        // 依据组开始前的状态判断（快照已核对一致）。
        let resolved_here: Vec<&String> = group
            .decisions
            .iter()
            .flat_map(|d| d.resolves_relations.iter())
            .collect();
        let mut replaces_unresolved = BTreeMap::new();
        for decision in &group.decisions {
            let unresolved = decision.pending_reason.is_none()
                && decision.action == W5Action::StateChange
                && decision.targets.iter().any(|target| {
                    w5_open_relations_of(state, *target)
                        .iter()
                        .any(|relation| !resolved_here.contains(&relation))
                });
            replaces_unresolved.insert(decision.candidate, unresolved);
        }
        let mut products = BTreeMap::new();
        let mut updates = BTreeMap::new();
        let mut merged = BTreeMap::new();
        for (step, decision) in group.decisions.iter().enumerate() {
            let entry = group
                .candidates
                .iter()
                .find(|c| c.index == decision.candidate)
                .ok_or("W5 decision refers to a missing candidate")?;
            let fresh = decision_update(group, decision, replaces_unresolved[&decision.candidate]);
            // 重判复用时按 w5-storage.md 第 3 节逐项清除：本次覆盖了的旧缺口才去掉。
            let coverage = entry.committed_id.map(|committed| RejudgeCoverage {
                scope_complete: group.scope_complete,
                decided: decision.pending_reason.is_none(),
                relation_inputs: related_records(state, committed)
                    .iter()
                    .all(|id| group.seen.iter().any(|seen| seen.id == *id)),
            });
            match apply(
                state,
                &group.id,
                &entry.candidate,
                decision,
                entry.committed_id,
            )? {
                Applied::Product(id) => {
                    let update = match (coverage, state.record_updates.get(&id)) {
                        (Some(coverage), Some(previous)) => {
                            UpdateState::after_rejudge(previous, &fresh, coverage)
                        }
                        _ => fresh,
                    };
                    products.insert(decision.candidate, id);
                    state.record_updates.insert(id, update.clone());
                    updates.insert(decision.candidate, update);
                }
                Applied::Merged { from, into } => {
                    state.record_updates.remove(&from);
                    merged.insert(from, into);
                    updates.insert(decision.candidate, fresh);
                }
                Applied::Nothing => {
                    updates.insert(decision.candidate, fresh);
                }
            }
            hook(W5Step::AfterDecision(step))?;
        }
        // 归并的记录退出投影：按整个依赖组判断（contracts 2.5），组内全部决定执行后它不能再处在
        // open 关系里，与决定顺序无关；不能留下指向已退出记录的未解决冲突，也不能机械地把成员
        // 换成保留目标。
        for from in merged.keys() {
            if let Some(relation) = w5_open_relations_of(state, *from).first() {
                return Err(format!(
                    "W5 group {}: merging record {} leaves relation {relation} unresolved",
                    group.id, from.0
                )
                .into());
            }
        }
        let receipt = W5Receipt {
            group_id: group.id.clone(),
            fingerprint,
            status: W5Status::Committed,
            legacy_fingerprint: false,
            products,
            decisions: group.decisions.clone(),
            updates,
            merged,
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
        let relation_sources = d
            .relation
            .iter()
            .flat_map(|spec| spec.basis.iter())
            .chain(d.resolution.iter())
            .map(|evidence| &evidence.source_id);
        for basis in d.basis_sources.iter().chain(relation_sources) {
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
        // 目标加入或离开了 open 关系（新建、解决、失效），逐条比对已见关系发现不了。
        if w5_open_relations_of(state, seen.id) != seen.relations {
            return Ok(Some(format!(
                "open relations of target {} changed since W4 input",
                seen.id.0
            )));
        }
    }
    for seen in &group.seen_relations {
        let current = match state.relations.get(&seen.id) {
            Some(relation) => w5_relation_fingerprint(relation)?,
            None => return Ok(Some(format!("relation {} no longer exists", seen.id))),
        };
        if current != seen.fingerprint {
            return Ok(Some(format!("relation {} changed since W4 input", seen.id)));
        }
    }
    Ok(None)
}

/// 与已提交断言相关的旧记录：产生它的那次决定替代或列出过的目标。重判覆盖
/// update_relation_unclear 的前提是这些记录都在本次 W4 输入里（及其最新关系状态）。
fn related_records(state: &PersistedMemoryState, committed: MemoryId) -> Vec<MemoryId> {
    let mut ids: Vec<MemoryId> = state
        .w5_receipts
        .values()
        .filter(|receipt| receipt.status == W5Status::Committed)
        .flat_map(|receipt| {
            receipt
                .decisions
                .iter()
                .filter(|d| receipt.products.get(&d.candidate) == Some(&committed))
                .flat_map(|d| d.targets.iter().copied())
                .collect::<Vec<_>>()
        })
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// committed_id 仍对应同一来源、候选和有效版本（contracts 2.5）：记录存在且有效、是本原文某个
/// 已提交 W5 组里候选的产物、候选的证据（完整证据，不只是编号）都已在它上面、由候选得来且之后
/// 不会被 W5 改动的字段与候选一致（有效期、证据、强化与替代链会被后续动作改动，不比较）。
/// 是否仍是 W4 输入时的版本由 seen 指纹核对。不符时返回原因，整组 needs_rejudge。
fn committed_mismatch(
    state: &PersistedMemoryState,
    group: &W5Group,
    source_turn: &(String, String),
) -> PersistenceResult<Option<String>> {
    for entry in &group.candidates {
        let Some(committed) = entry.committed_id else {
            continue;
        };
        let Some(record) = state
            .repository
            .get(committed)?
            .filter(|m| m.status == MemoryStatus::Active)
        else {
            return Ok(Some(format!(
                "candidate {}: committed_id {} is no longer active",
                entry.index, committed.0
            )));
        };
        let produced_here = state.w5_receipts.values().any(|receipt| {
            receipt.status == W5Status::Committed && {
                let turns = crate::forget::candidate_turns(&receipt.fingerprint);
                receipt.products.iter().any(|(candidate, id)| {
                    *id == committed && turns.get(candidate) == Some(source_turn)
                })
            }
        });
        let candidate = &entry.candidate;
        let same_evidence = candidate.evidence.iter().all(|e| {
            (e.session_id.0.as_str(), e.turn_id.0.as_str())
                == (source_turn.0.as_str(), source_turn.1.as_str())
                && record.evidence.contains(e)
        });
        let expected = Memory::from_candidate(committed, candidate, None);
        let same_assertion = (
            &record.owner_id,
            &record.subject_id,
            record.memory_type,
            &record.predicate,
            &record.value,
            record.polarity,
            record.strength,
            &record.scope,
        ) == (
            &expected.owner_id,
            &expected.subject_id,
            expected.memory_type,
            &expected.predicate,
            &expected.value,
            expected.polarity,
            expected.strength,
            &expected.scope,
        ) && (
            record.certainty,
            record.extraction_confidence,
            record.explicit_memory_request,
            &record.tags,
            record.temporal.observed_at,
            &record.temporal.reference,
            record.temporal.expires_at,
            record.retention_tier,
        ) == (
            expected.certainty,
            expected.extraction_confidence,
            expected.explicit_memory_request,
            &expected.tags,
            expected.temporal.observed_at,
            &expected.temporal.reference,
            expected.temporal.expires_at,
            expected.retention_tier,
        );
        let same_contract = entry.contract == w5_record_contract(state, committed);
        if !(produced_here && same_evidence && same_assertion && same_contract) {
            return Ok(Some(format!(
                "candidate {}: committed_id {} does not belong to this source and candidate",
                entry.index, committed.0
            )));
        }
    }
    Ok(None)
}

/// 关系依据与解决依据的机械核对：来源存在且未删除，片段逐字出现在原文中。冲突的依据须同时
/// 引用本原文与至少一个目标的来源；解决依据须来自本原文（W4 依据新原文判定已解决）。
fn check_relation_evidence(
    state: &PersistedMemoryState,
    group: &W5Group,
    source_id: &str,
) -> PersistenceResult<()> {
    let verbatim = |evidence: &RelationEvidence| -> PersistenceResult<&SourceRecord> {
        let source = state
            .sources
            .values()
            .find(|source| source.id == evidence.source_id)
            .ok_or_else(|| {
                format!(
                    "W5 relation evidence cites unknown source {}",
                    evidence.source_id
                )
            })?;
        let text = crate::lexical::body(source).unwrap_or("");
        if evidence.quote.trim().is_empty() || !text.contains(&evidence.quote) {
            return Err(format!(
                "W5 relation evidence quote is not verbatim in source {}",
                evidence.source_id
            )
            .into());
        }
        Ok(source)
    };
    for d in &group.decisions {
        if let Some(spec) = &d.relation {
            let mut own = false;
            let mut old_side = false;
            for evidence in &spec.basis {
                let source = verbatim(evidence)?;
                own |= source.id == source_id;
                old_side |= d.targets.iter().any(|target| {
                    state
                        .repository
                        .get(*target)
                        .ok()
                        .flatten()
                        .is_some_and(|memory| {
                            memory.evidence.iter().any(|e| {
                                e.session_id.0 == source.session_id && e.turn_id.0 == source.turn_id
                            })
                        })
                });
            }
            if !own || !old_side {
                return Err(format!(
                    "W5 decision {}: a conflict needs basis from both the new source and a target's source",
                    d.candidate
                )
                .into());
            }
        }
        for id in &d.resolves_relations {
            let relation = state
                .relations
                .get(id)
                .ok_or_else(|| format!("W5 relation {id} does not exist"))?;
            if let Some(member) = relation
                .members
                .iter()
                .find(|member| !group.seen.iter().any(|seen| seen.id == **member))
            {
                return Err(format!(
                    "W5 decision {}: member {} of relation {id} has no W4 snapshot",
                    d.candidate, member.0
                )
                .into());
            }
        }
        if let Some(resolution) = &d.resolution {
            let source = verbatim(resolution)?;
            if source.id != source_id {
                return Err(format!(
                    "W5 decision {}: the resolution must come from the claimed source",
                    d.candidate
                )
                .into());
            }
        }
    }
    Ok(())
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
    // 重判已提交断言：每个 committed_id 只出现一次，有快照，不与自身比较、不并入自身。
    let mut committed_ids: Vec<MemoryId> = Vec::new();
    for c in &group.candidates {
        let Some(committed) = c.committed_id else {
            continue;
        };
        if committed_ids.contains(&committed) {
            return Err(format!("W5 committed_id {} is used twice", committed.0).into());
        }
        committed_ids.push(committed);
        if !group.seen.iter().any(|seen| seen.id == committed) {
            return Err(format!("W5 committed_id {} has no W4 snapshot", committed.0).into());
        }
        for d in group.decisions.iter().filter(|d| d.candidate == c.index) {
            if d.action == W5Action::Retract {
                return Err(format!(
                    "W5 decision {}: a committed assertion cannot be rejudged as a retraction",
                    d.candidate
                )
                .into());
            }
            if d.targets.contains(&committed) {
                return Err(format!(
                    "W5 decision {}: a committed assertion cannot target or merge into itself",
                    d.candidate
                )
                .into());
            }
        }
    }
    // 关系快照：每个涉及的关系只记一次；目标所在的 open 关系与要解决的关系都必须有快照。
    let mut seen_relation_ids: Vec<&String> = group.seen_relations.iter().map(|s| &s.id).collect();
    seen_relation_ids.sort();
    seen_relation_ids.dedup();
    if seen_relation_ids.len() != group.seen_relations.len() {
        return Err("W5 relation snapshots must be unique".into());
    }
    for seen in &group.seen {
        let mut sorted = seen.relations.clone();
        sorted.sort();
        sorted.dedup();
        if sorted != seen.relations {
            return Err(format!(
                "W5 target {} lists its relations unsorted or twice",
                seen.id.0
            )
            .into());
        }
        if let Some(missing) = seen
            .relations
            .iter()
            .find(|id| !seen_relation_ids.contains(id))
        {
            return Err(format!("W5 relation {missing} has no W4 snapshot").into());
        }
    }
    let mut resolved: Vec<&String> = Vec::new();
    for d in &group.decisions {
        if (d.action == W5Action::Conflict) != d.relation.is_some() {
            return Err(format!(
                "W5 decision {}: exactly a conflict carries the relation to create",
                d.candidate
            )
            .into());
        }
        if d.relation
            .as_ref()
            .is_some_and(|spec| spec.basis.is_empty())
        {
            return Err(format!("W5 decision {}: a conflict needs basis", d.candidate).into());
        }
        if d.resolves_relations.is_empty() != d.resolution.is_none() {
            return Err(format!(
                "W5 decision {}: resolved relations and a resolution go together",
                d.candidate
            )
            .into());
        }
        if !d.resolves_relations.is_empty()
            && (d.pending_reason.is_some()
                || !matches!(
                    d.action,
                    W5Action::StateChange | W5Action::Retract | W5Action::Reinforce
                ))
        {
            // contracts 2.4：pending/new/coexist/conflict 时 resolves_relations 为空。
            return Err(format!(
                "W5 decision {}: {:?} cannot resolve a relation",
                d.candidate, d.action
            )
            .into());
        }
        for id in &d.resolves_relations {
            if !seen_relation_ids.contains(&id) {
                return Err(format!("W5 relation {id} has no W4 snapshot").into());
            }
            if resolved.contains(&id) {
                return Err(format!("W5 relation {id} is resolved twice").into());
            }
            resolved.push(id);
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
            if !matches!(d.action, W5Action::New | W5Action::Retract) || d.relation.is_some() {
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
        if (d.effective.is_some() || d.reason.is_some()) && d.action != W5Action::StateChange {
            return Err(format!(
                "W5 decision {}: only state_change carries an effective time or an end reason",
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

/// 执行一个决定，再解决它判定已解决的关系（同一事务；任一步失败整组回滚）。
/// 决定执行后的产物。
enum Applied {
    /// 没有产物（强化、撤回、待定撤回）。
    Nothing,
    /// 新插入或复用（重判时的 committed_id）的记录。
    Product(MemoryId),
    /// 重判为强化另一记录：已提交记录归并进保留目标，退出投影。
    Merged { from: MemoryId, into: MemoryId },
}

fn apply(
    state: &mut PersistedMemoryState,
    group_id: &str,
    candidate: &CandidateMemory,
    decision: &W5Decision,
    reused: Option<MemoryId>,
) -> PersistenceResult<Applied> {
    let applied = match reused {
        None => match apply_action(&mut state.repository, candidate, decision)? {
            Some(id) => Applied::Product(id),
            None => Applied::Nothing,
        },
        Some(committed) => apply_reused(state, committed, candidate, decision)?,
    };
    let product = match applied {
        Applied::Product(id) => Some(id),
        _ => None,
    };
    if let (Some(spec), Some(id)) = (&decision.relation, product) {
        // 冲突：新记录与全部目标组成一个 open 关系；旧版本不结束。
        let mut members = decision.targets.clone();
        members.push(id);
        let relation_id = format!("rel-{}", state.next_relation);
        state.next_relation = state
            .next_relation
            .checked_add(1)
            .ok_or("W5 relation id overflow")?;
        if state.relations.contains_key(&relation_id) {
            return Err(format!("W5 relation {relation_id} already exists").into());
        }
        state.relations.insert(
            relation_id.clone(),
            StoredRelation {
                id: relation_id,
                kind: RelationKind::Conflict,
                members,
                conditions: spec.conditions.clone(),
                times: spec.times.clone(),
                basis: spec.basis.clone(),
                status: RelationStatus::Open,
                resolution: None,
                created_by: group_id.to_owned(),
                closed_by: None,
            },
        );
    }
    for id in &decision.resolves_relations {
        let relation = state
            .relations
            .get_mut(id)
            .ok_or_else(|| format!("W5 relation {id} does not exist"))?;
        if relation.status != RelationStatus::Open {
            return Err(format!("W5 relation {id} is not open").into());
        }
        relation.status = RelationStatus::Resolved;
        relation.resolution = decision.resolution.clone();
        relation.closed_by = Some(group_id.to_owned());
    }
    Ok(applied)
}

/// 证据是否已在记录上：同 ID 或同一轮即视为已有（来源幂等）。执行强化与删除时的重算共用此判断。
pub(crate) fn evidence_present(existing: &[Evidence], e: &Evidence) -> bool {
    existing.iter().any(|x| x.id == e.id || x.same_turn(e))
}

/// 按来源幂等并入证据：目标上已有的证据（同 ID 或同一轮）不再并入。强化与证据转移分开：只有
/// 本次执行的原文（`counted`，即候选的证据）此前没有贡献过证据时才计一次强化；归并顺带转入的
/// 其他来源证据不计数（contracts 2.5 按来源幂等）。返回目标是否改动。
fn absorb(
    target: &mut Memory,
    evidence: &[Evidence],
    counted: &[Evidence],
    observed_at: Timestamp,
) -> bool {
    let reinforces = counted
        .iter()
        .any(|e| !evidence_present(&target.evidence, e));
    let fresh: Vec<Evidence> = evidence
        .iter()
        .filter(|e| !evidence_present(&target.evidence, e))
        .cloned()
        .collect();
    if fresh.is_empty() {
        return false;
    }
    if reinforces {
        target.reinforcement_count += 1;
        target.last_reinforced_at = Some(observed_at);
    }
    target.evidence.extend(fresh);
    true
}

/// state_change：以对应原因结束全部目标，由 `successor` 接替。W4 的 state_change / correction /
/// refine 都走这里（contracts 2.4 动作对应表）；撤回走 retract。先核对全部目标，任一不可用则整组
/// 失败，不产生部分结果。effective 未知时结束时间保持未知，不回填接收时间（契约 2.5）。
fn end_targets(
    repository: &mut InMemoryMemoryRepository,
    decision: &W5Decision,
    successor: MemoryId,
) -> PersistenceResult<()> {
    let reason = decision.reason.unwrap_or(SupersedeReason::StateChanged);
    if !matches!(
        reason,
        SupersedeReason::StateChanged
            | SupersedeReason::PreferenceChanged
            | SupersedeReason::ConstraintChanged
            | SupersedeReason::Correction
            | SupersedeReason::Refinement
            | SupersedeReason::ScopeRefined
    ) {
        return Err(format!("W5 state_change cannot end with {reason:?}").into());
    }
    let current: &InMemoryMemoryRepository = repository;
    let olds = decision
        .targets
        .iter()
        .map(|id| active(current, *id))
        .collect::<PersistenceResult<Vec<_>>>()?;
    for mut old in olds {
        old.status = MemoryStatus::Superseded;
        old.superseded_by = Some(successor);
        old.supersede_reason = Some(reason);
        old.temporal.valid_to = decision.effective;
        repository.update(old)?;
    }
    Ok(())
}

/// 重判已提交断言（committed_id）：复用原记录，不再插入，也不把它当作对自身的强化。
fn apply_reused(
    state: &mut PersistedMemoryState,
    committed: MemoryId,
    candidate: &CandidateMemory,
    decision: &W5Decision,
) -> PersistenceResult<Applied> {
    let repository = &mut state.repository;
    let mut record = active(repository, committed)?;
    if decision.pending_reason.is_some() {
        return Ok(Applied::Product(committed));
    }
    match decision.action {
        W5Action::New => Ok(Applied::Product(committed)),
        W5Action::Conflict => {
            for target in &decision.targets {
                active(repository, *target)?;
            }
            Ok(Applied::Product(committed))
        }
        W5Action::StateChange => {
            end_targets(repository, decision, committed)?;
            if record.supersedes.is_none() {
                record.supersedes = decision.targets.first().copied();
            }
            if let Some(effective) = decision.effective {
                record.temporal.valid_from = Some(effective);
            }
            repository.update(record)?;
            Ok(Applied::Product(committed))
        }
        W5Action::Reinforce => {
            let into = decision.targets[0];
            let mut target = active(repository, into)?;
            if absorb(
                &mut target,
                &record.evidence,
                &candidate.evidence,
                record.temporal.observed_at,
            ) {
                repository.update(target)?;
            }
            // 退出投影：不同时返回两份等效记录；归并映射保存在回执。
            record.status = MemoryStatus::Deleted;
            repository.update(record)?;
            Ok(Applied::Merged {
                from: committed,
                into,
            })
        }
        W5Action::Retract => {
            Err("W5 a committed assertion cannot be rejudged as a retraction".into())
        }
    }
}

fn apply_action(
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
            if absorb(
                &mut target,
                &candidate.evidence,
                &candidate.evidence,
                candidate.temporal.observed_at,
            ) {
                repository.update(target)?;
            }
            Ok(None)
        }
        W5Action::StateChange => {
            let id = repository.allocate_id()?;
            end_targets(repository, decision, id)?;
            let mut new = Memory::from_candidate(id, candidate, decision.targets.first().copied());
            if let Some(effective) = decision.effective {
                new.temporal.valid_from = Some(effective);
            }
            repository.insert(new)?;
            Ok(Some(id))
        }
        W5Action::Conflict => {
            // 目标保持不变（不结束、不取代），只核对它们仍然有效。
            for target in &decision.targets {
                active(repository, *target)?;
            }
            let id = repository.allocate_id()?;
            repository.insert(Memory::from_candidate(id, candidate, None))?;
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
