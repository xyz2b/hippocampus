//! Read-side lifecycle rules and shared slot identity for legacy retrieval.
use crate::{Cardinality, Memory, MemoryStatus, RetentionTier, Timestamp};

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
