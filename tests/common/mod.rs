//! Explicitly approved fixtures for domain/storage tests, including recorded decoder replay.
//!
//! This helper uses trusted SQLite staging restoration after fixture edits. It does not run
//! production source review or validate temporal grounding/USER assertion coverage; arbitrary fixture time boundaries
//! test later domain behavior. It must not be used as model or intake-safety acceptance evidence.

use hippocampus::*;
use serde_json::{Value, json};

pub fn grounded(candidate: CandidateMemory) -> CandidateMemory {
    // Deliberately malformed evidence must reach the engine's original rejection checks.
    if candidate.evidence.is_empty()
        || candidate.evidence.iter().any(|evidence| {
            evidence.id.0.trim().is_empty()
                || evidence.session_id.0.trim().is_empty()
                || evidence.turn_id.0.trim().is_empty()
                || evidence.quote.trim().is_empty()
        })
        || candidate
            .evidence
            .iter()
            .enumerate()
            .any(|(index, evidence)| {
                candidate.evidence[..index]
                    .iter()
                    .any(|prior| prior.id == evidence.id)
            })
    {
        return candidate;
    }
    // These fixtures exercise later domain/storage rules. Restore an explicitly approved
    // v4 snapshot through the existing trusted SQLite seam; never grant JSON input approval.
    let directory =
        std::env::temp_dir().join(format!("pmr-approved-fixture-{}", diagnostics::new_id()));
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&directory).unwrap();
    let directory = FixtureDirectory(directory);
    let path = directory.0.join("memory.sqlite3");
    let store = SqliteMemoryStore::open(&path, candidate.owner_id.clone()).unwrap();
    let mut state = serde_json::to_value(store.read().unwrap()).unwrap();
    let mut snapshot = serde_json::to_value(&candidate).unwrap();
    snapshot.as_object_mut().unwrap().remove("source_support");
    let proof = json!({"version": 4, "candidate": snapshot});
    snapshot["source_support"] = proof;
    state["staging"] = json!({
        "next_id": 1,
        "pending": {"1": {"id": 1, "candidate": snapshot, "resolution": "NotFound"}}
    });
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute(
            "INSERT INTO memory_profiles(owner_id, state_json) VALUES (?1, ?2)",
            rusqlite::params![candidate.owner_id.0, state.to_string()],
        )
        .unwrap();
    let approved = store
        .read()
        .unwrap()
        .staging
        .get(StagedCandidateId(1))
        .unwrap()
        .candidate
        .clone();
    assert!(
        approved.source_support_valid(),
        "fixture proof version must match the runtime"
    );
    approved
}

struct FixtureDirectory(std::path::PathBuf);
impl Drop for FixtureDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Historical read fixtures only: callers specify assertion fields and update intent.
/// This is not evidence that a production write would be accepted.
#[derive(Debug)]
pub enum FixtureOutcome {
    Applied { memory_id: Option<MemoryId> },
}

pub trait LegacyFixture {
    fn seed_fixture(&mut self, candidate: CandidateMemory) -> Result<FixtureOutcome, StoreError>;
}
impl<P> LegacyFixture for MemoryEngine<InMemoryMemoryRepository, P> {
    fn seed_fixture(&mut self, candidate: CandidateMemory) -> Result<FixtureOutcome, StoreError> {
        let mut row = serde_json::to_value(&candidate).unwrap();
        row["subject_id"] = json!(
            candidate
                .subject_id()
                .expect("fixture needs a resolved subject")
        );
        let target = match candidate.intent {
            CandidateIntent::Supersede { target, .. } => {
                Some(target.expect("fixture update must name a target"))
            }
            CandidateIntent::Assert => None,
            _ => panic!("fixture must specify a row or explicit update"),
        };
        let id = self.repository.allocate_id()?;
        row["id"] = json!(id);
        row["status"] = json!("Active");
        row["supersedes"] = json!(target);
        row["superseded_by"] = Value::Null;
        row["supersede_reason"] = Value::Null;
        row["reinforcement_count"] = json!(0);
        row["retrieval_count"] = json!(0);
        row["last_reinforced_at"] = Value::Null;
        row["last_retrieved_at"] = Value::Null;
        let memory: Memory = serde_json::from_value(row).unwrap();
        if let Some(target) = target {
            let mut old = self.repository.get(target)?.expect("fixture target exists");
            old.status = MemoryStatus::Superseded;
            old.superseded_by = Some(id);
            old.supersede_reason = Some(match candidate.intent {
                CandidateIntent::Supersede { reason, .. } => reason,
                _ => unreachable!("fixture update is an explicit supersession"),
            });
            old.temporal.valid_to = Some(DatedValue {
                at: candidate.temporal.observed_at,
                source: TemporalSource::Supersession,
            });
            self.repository.update(old)?;
        }
        self.repository.insert(memory)?;
        Ok(FixtureOutcome::Applied {
            memory_id: Some(id),
        })
    }
}

/// Load a released staged snapshot directly, preserving the old proof for SQLite's trusted read.
pub fn stage_fixture(store: &SqliteMemoryStore, candidate: CandidateMemory) {
    let mut state = serde_json::to_value(store.read().unwrap()).unwrap();
    let id = state["staging"]["next_id"].as_u64().unwrap() + 1;
    state["staging"]["next_id"] = json!(id);
    state["staging"]["pending"][id.to_string()] =
        json!({"id":id,"candidate":grounded(candidate),"resolution":"NotFound"});
    rusqlite::Connection::open(store.path()).unwrap().execute(
        "INSERT INTO memory_profiles(owner_id, state_json) VALUES (?1,?2) ON CONFLICT(owner_id) DO UPDATE SET state_json=excluded.state_json",
        rusqlite::params![store.owner().0, state.to_string()]).unwrap();
}
