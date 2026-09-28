//! Optional derived keys. Evidence and the authoritative raw-token projection stay unchanged.
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::*;

pub(crate) const VERSION: &str = "committed-fact-key-v2";
const MAX_HINT_BYTES: usize = 4096;
const MAX_SEGMENT_HINTS: usize = 16;
const MAX_ASSOCIATIONS: usize = 4096;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct FactHint {
    pub source_id: String,
    pub source_version: u64,
    pub byte_start: usize,
    pub byte_end: usize,
    pub memory_id: MemoryId,
    pub evidence_id: EvidenceId,
    pub quote_byte_range: [usize; 2],
    pub text: String,
    pub tokens: String,
}

pub(crate) fn create_schema(db: &Connection) -> PersistenceResult<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS fact_indexes (
        owner_id TEXT PRIMARY KEY, manifest_json TEXT NOT NULL, hints_json TEXT NOT NULL)",
    )?;
    Ok(())
}

fn binding(
    db: &Connection,
    owner: &UserId,
    state: &PersistedMemoryState,
) -> PersistenceResult<Value> {
    let Some((generation, version, source_revision)) = crate::lexical::metadata(db, owner)? else {
        return Err("raw_index_missing".into());
    };
    if version != crate::lexical::VERSION || source_revision != state.source_revision {
        return Err("raw_index_version_mismatch".into());
    }
    Ok(
        json!({"owner":owner,"renderer_version":VERSION,"revision":state.revision,
        "source_revision":source_revision,"generation":generation,"raw_processing_version":version,
        "sources":state.sources.values().map(|s|json!([s.id,s.enrichment,s.index])).collect::<Vec<_>>()}),
    )
}

fn render(
    state: &PersistedMemoryState,
    owner: &UserId,
    memory: &Memory,
) -> PersistenceResult<String> {
    let entity = |id: &EntityId| state.entities.get(id).filter(|e| &e.owner_id == owner);
    let value_entity = match &memory.value {
        MemoryValue::Entity(id) => entity(id),
        _ => None,
    };
    // Keep entire semantic items, including negatives and conditions; oversized items are skipped.
    let mut value = json!({"subject_id":memory.subject_id,
        "subject":entity(&memory.subject_id),"type":memory.memory_type,"predicate":memory.predicate,
        "value":memory.value,"value_entity":value_entity,"polarity":memory.polarity,
        "strength":memory.strength,"scope":memory.scope,"status":memory.status,
        "temporal":memory.temporal,"supersede_reason":memory.supersede_reason});
    // Workspace feature unification can enable serde_json/preserve_order.
    value.sort_all_objects();
    Ok(serde_json::to_string(&value)?)
}

fn project(
    state: &PersistedMemoryState,
    owner: &UserId,
) -> PersistenceResult<(Vec<FactHint>, Value)> {
    let mut hints = Vec::new();
    let mut counts = BTreeMap::<(&str, usize), usize>::new();
    let mut skipped = BTreeMap::<&str, usize>::new();
    let memories = state.repository.list_by_owner(owner)?;
    let mut projected_memories = BTreeSet::new();
    let mut quote_associations = 0usize;
    for memory in &memories {
        if memory.status == MemoryStatus::Deleted {
            continue;
        }
        let text = render(state, owner, memory)?;
        if text.len() > MAX_HINT_BYTES {
            *skipped.entry("oversized_memory").or_default() += 1;
            continue;
        }
        let words = crate::lexical::tokens(&text).join(" ");
        for evidence in &memory.evidence {
            let sources: Vec<_> = state
                .sources
                .values()
                .filter(|s| {
                    s.session_id == evidence.session_id.0
                        && s.turn_id == evidence.turn_id.0
                        && crate::lexical::body(s).is_some()
                })
                .collect();
            if sources.len() != 1 {
                *skipped.entry("missing_or_ambiguous_source").or_default() += 1;
                continue;
            }
            let source = sources[0];
            let raw = crate::lexical::body(source).unwrap();
            let positions: Vec<_> = raw.match_indices(&evidence.quote).take(2).collect();
            if evidence.quote.trim().is_empty() || positions.len() != 1 {
                *skipped.entry("missing_or_ambiguous_quote").or_default() += 1;
                continue;
            }
            let start = positions[0].0;
            let end = start + evidence.quote.len();
            let segments: Vec<_> = crate::lexical::segments(raw)
                .into_iter()
                .filter(|&(a, b)| a <= start && end <= b)
                .collect();
            if segments.is_empty() {
                *skipped.entry("quote_crosses_segment").or_default() += 1;
                continue;
            }
            quote_associations += 1;
            for (a, b) in segments {
                let count = counts.entry((&source.id, a)).or_default();
                if *count >= MAX_SEGMENT_HINTS {
                    *skipped.entry("segment_hint_limit").or_default() += 1;
                    continue;
                }
                if hints.len() >= MAX_ASSOCIATIONS {
                    return Err("fact_projection_exceeds_4096_associations".into());
                }
                let source_version = source.index["source_version"]
                    .as_u64()
                    .ok_or("source_version_missing")?;
                hints.push(FactHint {
                    source_id: source.id.clone(),
                    source_version,
                    byte_start: a,
                    byte_end: b,
                    memory_id: memory.id,
                    evidence_id: evidence.id.clone(),
                    quote_byte_range: [start, end],
                    text: text.clone(),
                    tokens: words.clone(),
                });
                projected_memories.insert(memory.id);
                *count += 1;
            }
        }
    }
    let coverage = json!({"repository_memory_count":memories.len(),
        "projected_memory_count":projected_memories.len(),"exact_quote_evidence_count":quote_associations,
        "segment_association_count":hints.len(),"skipped":skipped,
        "hint_bytes":hints.iter().map(|h|h.text.len()).sum::<usize>(),
        "token_bytes":hints.iter().map(|h|h.tokens.len()).sum::<usize>(),
        "limits":{"hint_bytes":MAX_HINT_BYTES,"hints_per_segment":MAX_SEGMENT_HINTS,"associations":MAX_ASSOCIATIONS}});
    Ok((hints, coverage))
}

pub(crate) fn read(
    db: &Connection,
    owner: &UserId,
    state: &PersistedMemoryState,
    eligible: &BTreeSet<MemoryId>,
    task: Option<&str>,
) -> PersistenceResult<(Vec<FactHint>, Value)> {
    let record: Option<(String, String)> = db
        .query_row(
            "SELECT manifest_json,hints_json FROM fact_indexes WHERE owner_id=?1",
            [&owner.0],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (manifest, encoded) = record.ok_or("fact_index_not_built")?;
    let manifest: Value = serde_json::from_str(&manifest)?;
    if manifest["binding"] != binding(db, owner, state)? {
        return Err("fact_index_version_mismatch".into());
    }
    let hints: Vec<FactHint> = serde_json::from_str(&encoded)?;
    // ponytail: owner-wide validation (<=4096 associations); add per-memory rows/hashes only if measured latency warrants it.
    // Reload authoritative facts in this SAME read transaction: retention may have changed the caller's working copy.
    let persisted = SqliteMemoryStore::load(db, owner)?;
    let (expected, coverage) = project(&persisted, owner)?;
    if expected != hints || manifest["coverage"] != coverage {
        return Err("fact_index_projection_mismatch".into());
    }
    let mut allowed = Vec::new();
    for hint in hints {
        if !eligible.contains(&hint.memory_id) {
            continue;
        }
        let source = state
            .sources
            .values()
            .find(|s| s.id == hint.source_id)
            .ok_or("fact_source_missing")?;
        if task.is_some_and(|t| crate::lexical::task(source) != Some(t)) {
            continue;
        }
        let memory = state
            .repository
            .get(hint.memory_id)?
            .ok_or("fact_memory_missing")?;
        if memory.owner_id != *owner
            || memory.status == MemoryStatus::Deleted
            || render(state, owner, &memory)? != hint.text
        {
            return Err("fact_memory_changed_in_read_snapshot".into());
        }
        allowed.push(hint);
    }
    let report = json!({"status":"ready","renderer_version":VERSION,"revision":state.revision,
        "source_revision":state.source_revision,"generation":manifest["binding"]["generation"],
        "coverage":coverage,"eligible_segment_association_count":allowed.len(),
        "index_bytes":manifest["index_bytes"],"rebuild_ms":manifest["rebuild_ms"]});
    Ok((allowed, report))
}

impl SqliteMemoryStore {
    /// Independent experiment switch; enabling it never repairs or writes a projection.
    pub fn set_fact_index_enabled(&mut self, enabled: bool) {
        self.fact_index_enabled = enabled;
    }

    /// Explicit maintenance. Business commits survive refresh failures; reads never call this method.
    pub fn rebuild_fact_index(&mut self) -> PersistenceResult<Value> {
        let started = Instant::now();
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state = Self::load(&tx, &self.owner)?;
        let binding = binding(&tx, &self.owner, &state)?;
        let (hints, coverage) = project(&state, &self.owner)?;
        let encoded = serde_json::to_string(&hints)?;
        let manifest = json!({"binding":binding,"coverage":coverage,"index_bytes":encoded.len(),
            "rebuild_ms":started.elapsed().as_millis()});
        tx.execute("INSERT INTO fact_indexes(owner_id,manifest_json,hints_json) VALUES(?1,?2,?3)
            ON CONFLICT(owner_id) DO UPDATE SET manifest_json=excluded.manifest_json,hints_json=excluded.hints_json",
            params![self.owner.0,serde_json::to_string(&manifest)?,encoded])?;
        tx.commit()?;
        Ok(
            json!({"status":"ready","renderer_version":VERSION,"revision":state.revision,
            "source_revision":state.source_revision,"generation":binding["generation"],
            "coverage":coverage,"index_bytes":manifest["index_bytes"],"rebuild_ms":manifest["rebuild_ms"]}),
        )
    }

    pub(crate) fn refresh_fact_index_after_commit(&mut self) {
        if self.fact_index_enabled {
            let result = self.rebuild_fact_index();
            crate::diagnostics::record("fact_index_refresh", || match result {
                Ok(report) => report,
                Err(error) => json!({"status":"unavailable","reason":error.to_string()}),
            });
        }
    }
}
