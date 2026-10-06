//! Read-side lifecycle rules.
use crate::{Memory, MemoryStatus, RetentionTier, Timestamp};

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
