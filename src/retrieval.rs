//! Local global-requirements projection. Ordinary questions use R1/R3/R4/R5.
use crate::{
    Cardinality, EntityId, Memory, MemoryId, MemoryStatus, MemoryType, PredicateRegistry,
    RetentionTier, Timestamp,
};

/// AF283's approved relation IDs; no text-based health or preference classification.
pub const STATEMENT_SNAPSHOT_RELATIONS: &[&str] = &[
    "response_language",
    "response_style",
    "code_example_language",
    "likes",
    "avoids",
    "job_title",
    "works_at",
    "lives_in",
    "allergic_to",
];

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct StatementSnapshotConfig {
    pub enabled: bool,
    pub max_records: usize,
    pub max_bytes: usize,
}
impl Default for StatementSnapshotConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_records: 8,
            max_bytes: 16_384,
        }
    }
}
impl StatementSnapshotConfig {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }

    pub fn validate(&self) -> crate::PersistenceResult<()> {
        if !(1..=32).contains(&self.max_records) || !(512..=65_536).contains(&self.max_bytes) {
            return Err(
                "statement_snapshot bounds: max_records=1..32, max_bytes=512..65536".into(),
            );
        }
        Ok(())
    }
}

pub(crate) fn statement_snapshot(
    state: &crate::PersistedMemoryState,
    request: &crate::recall::Request,
    config: &StatementSnapshotConfig,
    limit: usize,
    read_at: i64,
) -> crate::PersistenceResult<Option<crate::contract::StatementSnapshot>> {
    use crate::contract::{Grade, RecordView, SourceRef, StoredStatus, Subject};
    use crate::{MemoryRepository, RetentionPolicy, UpdateStatus};
    if !config.enabled || !request.bootstrap {
        return Ok(None);
    }
    config.validate()?;
    let authorized = |id: &str| {
        state.sources.values().find(|s| s.id == id).filter(|s| {
            crate::recall::source_in_task(s, request)
                && s.request
                    .as_ref()
                    .is_some_and(|r| r["capture_source"] == true)
                && s.enrichment != "awaiting_confirmation"
        })
    };
    let mut result = crate::contract::StatementSnapshot {
        records: vec![],
        snapshot: crate::contract::RecallSnapshot {
            read_revision: state.revision,
            source_revision: state.source_revision,
        },
        read_at,
        unfinished_sources: state
            .sources
            .values()
            .filter(|s| {
                crate::recall::source_in_task(s, request)
                    && !matches!(
                        crate::recall::enrichment(s),
                        Some(
                            crate::contract::EnrichmentStatus::Committed
                                | crate::contract::EnrichmentStatus::CompletedNoFacts
                        )
                    )
            })
            .count(),
        coverage_unknown: true,
        truncated: false,
        notice: "已存陈述，非本轮判断；不证明当前值、条件适用或完整覆盖。".into(),
    };
    let mut memories = state.repository.list_by_owner(&request.owner)?;
    memories.sort_by_key(|m| (std::cmp::Reverse(m.temporal.observed_at), m.id));
    for memory in memories {
        if memory.subject_id != EntityId::from("self")
            || memory.status != MemoryStatus::Active
            || matches!(
                crate::DefaultRetentionPolicy.evaluate(&memory, Timestamp(read_at)),
                crate::RetentionAction::Expire
            )
            || !crate::recall::record_in_scope(state, &memory, request)
            || !STATEMENT_SNAPSHOT_RELATIONS.contains(&memory.predicate.0.as_str())
            || (matches!(
                memory.predicate.0.as_str(),
                "response_language"
                    | "response_style"
                    | "code_example_language"
                    | "likes"
                    | "avoids"
            ) && !matches!(
                memory.memory_type,
                MemoryType::Preference | MemoryType::Constraint
            ))
        {
            continue;
        }
        let Some(RecordView::Current(view)) = crate::recall::projection(state, memory.id) else {
            continue;
        };
        if view.stored_status != StoredStatus::Active
            || view.grade != Grade::Committed
            || view.update.status != UpdateStatus::Judged
            || !view.open_relations.is_empty()
            || !matches!(&view.candidate.subject, Subject::Entity(s) if s.entity_id == "self")
        {
            continue;
        }
        let Some(contract) = crate::w5_record_contract(state, memory.id) else {
            continue;
        };
        let SourceRef::Stored { source_id } = &contract.source else {
            continue;
        };
        let Some(origin) = authorized(source_id) else {
            continue;
        };
        if crate::contract::locate_quote(
            crate::lexical::body(origin).unwrap(),
            &view.candidate.sentence.text,
        )
        .is_none()
            || view.evidence.is_empty()
            || !view.evidence.iter().all(|e| {
                let SourceRef::Stored { source_id } = &e.source else {
                    return false;
                };
                authorized(source_id).is_some_and(|s| {
                    crate::contract::locate_quote(crate::lexical::body(s).unwrap(), &e.quote.text)
                        .is_some()
                })
            })
        {
            continue;
        }
        if result.records.len() >= config.max_records.min(limit) {
            result.truncated = true;
            continue;
        }
        result.records.push(view);
        if serde_json::to_vec(&result)?.len() > config.max_bytes {
            result.records.pop();
            result.truncated = true;
        }
    }
    Ok(Some(result))
}

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
