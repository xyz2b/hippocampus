//! 旧 CandidateMemory 暂存快照；仅保留反序列化、读取和删除。

use std::collections::BTreeMap;

use crate::{CandidateMemory, EntityResolution, UserId};

#[derive(
    serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd,
)]
/// 一条 pending candidate 在当前进程内的标识。
pub struct StagedCandidateId(
    /// 在单个 `CandidateStaging` 实例内单调递增的编号。
    pub u64,
);

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq)]
/// subject 尚不能唯一解析时保留的 Candidate。
pub struct StagedCandidate {
    /// 后续澄清 turn 用来定位原 Candidate 的标识。
    pub id: StagedCandidateId,
    /// 完整 atomic candidate；澄清时只更新 unresolved subject。
    pub candidate: CandidateMemory,
    /// 最近一次 NotFound 或 Ambiguous 解析结果。
    pub resolution: EntityResolution,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default)]
/// 旧版 pending queue；不再支持新增、重判或提交。
pub struct CandidateStaging {
    /// 下一个进程内 staged candidate 编号。
    next_id: u64,
    /// 按 ID 排序的 pending candidates，保证检查和测试确定性。
    pending: BTreeMap<StagedCandidateId, StagedCandidate>,
}

impl CandidateStaging {
    /// SQLite alone restores persisted approval; ordinary candidate JSON never grants it.
    pub(crate) fn restore_trusted_source_support(&mut self, stored: &serde_json::Value) {
        for (id, staged) in &mut self.pending {
            staged.candidate.source_support = stored["pending"][id.0.to_string()]["candidate"]
                .get("source_support")
                .cloned();
            if !staged.candidate.source_support_valid() {
                staged.candidate.source_support = None;
            }
        }
    }

    /// 读取一条 pending candidate，不转移所有权。
    pub fn get(&self, id: StagedCandidateId) -> Option<&StagedCandidate> {
        self.pending.get(&id)
    }

    /// 按 ID 顺序列出属于指定用户的 pending candidates。
    pub fn list_by_owner(&self, owner_id: &UserId) -> Vec<&StagedCandidate> {
        self.pending
            .values()
            .filter(|staged| &staged.candidate.owner_id == owner_id)
            .collect()
    }

    /// 仍在等待澄清的 Candidate 数量。
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// 是否没有 Candidate 等待澄清。
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    pub(crate) fn remove(&mut self, id: StagedCandidateId) -> Option<StagedCandidate> {
        self.pending.remove(&id)
    }
}
