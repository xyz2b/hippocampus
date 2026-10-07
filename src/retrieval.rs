//! Local global-requirements projection. Ordinary questions use R1/R3/R4/R5.
use crate::{
    Cardinality, EntityId, Memory, MemoryId, MemoryStatus, MemoryType, PredicateRegistry,
    RetentionTier, Timestamp,
};

#[derive(serde::Serialize, Clone, Debug, Default, Eq, PartialEq)]
pub struct GlobalContext {
    pub mandatory: Vec<Memory>,
    pub constraints: Vec<Memory>,
    pub preferences: Vec<Memory>,
    pub facts: Vec<Memory>,
    pub episodes: Vec<Memory>,
    pub conflicts: Vec<GlobalRequirementConflict>,
    pub trace: Vec<serde_json::Value>,
}
#[derive(serde::Serialize, Clone, Debug, Eq, PartialEq)]
pub struct GlobalRequirementConflict {
    pub left: MemoryId,
    pub right: MemoryId,
}

pub(crate) fn global_requirements(
    memories: Vec<Memory>,
    registry: &impl PredicateRegistry,
    unmapped: &std::collections::BTreeSet<MemoryId>,
    now: Timestamp,
) -> GlobalContext {
    let mut result = GlobalContext::default();
    let mut candidates = Vec::new();
    for memory in memories {
        if memory.subject_id != EntityId::from("self")
            || memory.scope.domain.is_some()
            || !memory.scope.conditions.is_empty()
            || unmapped.contains(&memory.id)
            || memory.status != MemoryStatus::Active
            || !memory.temporal.valid_at(now)
        {
            continue;
        }
        if memory.memory_type == MemoryType::Constraint
            && memory.retention_tier == RetentionTier::Durable
        {
            result.mandatory.push(memory);
        } else if matches!(
            memory.predicate.0.as_str(),
            "response_language" | "response_style"
        ) {
            candidates.push(memory);
        }
    }
    candidates.sort_by_key(|m| {
        (
            match m.memory_type {
                MemoryType::Constraint => 0,
                MemoryType::Preference => 1,
                MemoryType::Fact => 2,
                MemoryType::Episode => 3,
            },
            std::cmp::Reverse(m.temporal.observed_at),
            m.id,
        )
    });
    let mut selected: Vec<Memory> = Vec::new();
    let mut blocked: Vec<Memory> = Vec::new();
    for candidate in candidates {
        // Only exact stored slot identity/equality is checked. No scope, alias or IS-A inference.
        let same_slot = |m: &&Memory| {
            m.subject_id == candidate.subject_id
                && m.memory_type == candidate.memory_type
                && m.predicate == candidate.predicate
                && (registry
                    .definition(&candidate.predicate)
                    .is_some_and(|p| p.cardinality == Cardinality::Single)
                    || m.value == candidate.value)
        };
        if let Some(m) = blocked.iter().find(same_slot) {
            result.conflicts.push(GlobalRequirementConflict {
                left: m.id,
                right: candidate.id,
            });
        } else if let Some(i) = selected.iter().position(|m| same_slot(&m)) {
            let existing = &selected[i];
            if existing.value != candidate.value
                || existing.polarity != candidate.polarity
                || existing.strength != candidate.strength
            {
                result.conflicts.push(GlobalRequirementConflict {
                    left: existing.id,
                    right: candidate.id,
                });
                blocked.push(selected.remove(i));
            }
        } else {
            selected.push(candidate);
        }
    }
    for memory in selected {
        match memory.memory_type {
            MemoryType::Constraint => result.constraints.push(memory),
            MemoryType::Preference => result.preferences.push(memory),
            MemoryType::Fact => result.facts.push(memory),
            MemoryType::Episode => result.episodes.push(memory),
        }
    }
    result
}
