//! Legacy read-side retention wrapper; writing is exclusively W1/W3/W4/W5.
use crate::{
    CandidateStaging, DefaultRetentionPolicy, MemoryRepository, MemoryStatus, RetentionAction,
    RetentionPolicy, StoreError, Timestamp,
};

pub struct MemoryEngine<R, P, T = DefaultRetentionPolicy> {
    pub repository: R,
    pub predicates: P,
    pub retention: T,
    pub staging: CandidateStaging,
}

impl<R, P> MemoryEngine<R, P> {
    pub fn new(repository: R, predicates: P) -> Self {
        Self {
            repository,
            predicates,
            retention: DefaultRetentionPolicy,
            staging: CandidateStaging::default(),
        }
    }
}

impl<R: MemoryRepository, P, T: RetentionPolicy> MemoryEngine<R, P, T> {
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
}
