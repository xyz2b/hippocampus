//! Fictional SQLite fixture shared by AF216 diagnostics and mechanical regressions.
use hippocampus::contract::Polarity as CandidatePolarity;
use hippocampus::contract::*;
use hippocampus::{EntityRepository, *};
use serde_json::{Value as Json, json};
use std::{collections::BTreeMap, path::PathBuf};

pub const OWNER: &str = "eval";
pub const DAY: &str = "2026-10-10";
pub const AT: i64 = 1791561600;
pub struct Db(pub PathBuf);
impl Db {
    pub fn new() -> Self {
        let db = Self(
            std::env::temp_dir()
                .join(format!("af216-{}", diagnostics::new_id()))
                .join("memory.sqlite3"),
        );
        db.open()
            .transaction(|s| {
                s.entities.upsert(Entity {
                    id: "self".into(),
                    owner_id: OWNER.into(),
                    entity_type: EntityType::Person,
                    canonical_name: "用户本人".into(),
                    aliases: vec!["我".into()],
                });
                Ok(())
            })
            .unwrap();
        db
    }
    pub fn open(&self) -> SqliteMemoryStore {
        SqliteMemoryStore::open(&self.0, OWNER.into()).unwrap()
    }
    pub fn state(&self) -> PersistedMemoryState {
        self.open().read().unwrap()
    }
    pub fn source(&self, key: &str, text: &str, history: Vec<String>) {
        self.open()
            .transaction(|s| {
                let sequence = s
                    .sources
                    .values()
                    .map(|s| s.received_sequence)
                    .max()
                    .unwrap_or(0)
                    .max(s.last_applied_source_sequence)
                    .max(s.source_less_barrier_sequence)
                    + 1;
                s.sources.insert(
                    key.into(),
                    SourceRecord {
                        management: Default::default(),
                        id: key.into(),
                        session_id: "af216".into(),
                        turn_id: key.into(),
                        observed_at: AT + sequence as i64,
                        received_sequence: sequence,
                        request: Some(json!({"text":text,"history":history,"capture_source":true})),
                        enrichment: "running".into(),
                        error: None,
                        result: None,
                        base_revision: s.revision,
                        index: Json::Null,
                        task: EnrichmentTask {
                            attempt_id: Some(format!("claim-{key}")),
                            lease_until: Some(4_000_000_000),
                            claim_revision: Some(s.revision),
                            ..Default::default()
                        },
                    },
                );
                Ok(())
            })
            .unwrap();
    }
    pub fn input(&self, key: &str, candidates: Vec<W4Candidate>) -> W4Input {
        let pending = candidates
            .into_iter()
            .map(|c| w4::Pending {
                index: c.index,
                committed_id: c.committed_id,
                contract: ContractCandidate {
                    candidate: c.candidate,
                    source: c.source,
                    said_on: c.said_on,
                    time_resolutions: c.time_resolutions,
                },
            })
            .collect::<Vec<_>>();
        w4::assemble(
            &self.open(),
            key,
            None,
            &pending,
            w4::W4Budget {
                facts: 64,
                records: 64,
            },
        )
        .unwrap()
        .1
        .input
    }
    pub fn plan(
        &self,
        key: &str,
        input: &W4Input,
        output: &W4Output,
        round: u32,
    ) -> Result<w4::Plan, String> {
        w4::plan(
            &w4::W4Runtime {
                state: &self.state(),
                owner: OWNER.into(),
                source_key: key,
                round,
                task_id: None,
                admissions: BTreeMap::new(),
            },
            input,
            output,
        )
        .map_err(|e| e.to_string())
    }
    pub fn apply(
        &self,
        key: &str,
        input: &W4Input,
        output: &W4Output,
    ) -> Result<BTreeMap<usize, MemoryId>, String> {
        let plan = self.plan(key, input, output, 0)?;
        if !plan.held.is_empty() {
            return Err(format!("held: {:?}", plan.held));
        }
        let mut ids = BTreeMap::new();
        for group in &plan.groups {
            match commit_group(&mut self.open(), &claim(key), group).map_err(|e| e.to_string())? {
                W5Outcome::Committed(r) => ids.extend(r.products),
                other => return Err(format!("{other:?}")),
            }
        }
        w5_finish_task(&mut self.open(), &claim(key), &W5TaskEnd::Committed)
            .map_err(|e| e.to_string())?;
        Ok(ids)
    }
}
impl Drop for Db {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.0.parent().unwrap());
    }
}
pub fn claim(key: &str) -> W5Claim {
    W5Claim {
        source_key: key.into(),
        attempt_id: format!("claim-{key}"),
    }
}
pub fn candidate(
    key: &str,
    sentence: &str,
    relation: &str,
    value: &str,
    polarity: CandidatePolarity,
    index: usize,
) -> W4Candidate {
    W4Candidate {
        index,
        committed_id: None,
        source: SourceRef::Stored {
            source_id: key.into(),
        },
        said_on: Some(DAY.into()),
        time_resolutions: vec![],
        candidate: Candidate {
            kind: CandidateKind::Assertion,
            speech: Speech::UserStatement,
            subject: Subject::Entity(EntitySubject {
                entity_id: "self".into(),
            }),
            relation: relation.into(),
            memory_type: "fact".into(),
            value: Some(Value::Text {
                text: value.into(),
                quote: Quote { text: value.into() },
            }),
            sentence: Quote {
                text: sentence.into(),
            },
            polarity,
            strength: None,
            uncertainty: None,
            conditions: vec![],
            times: vec![],
        },
    }
}
pub fn decision(
    input: &W4Input,
    index: usize,
    action: Action,
    targets: Vec<MemoryId>,
) -> W4Decision {
    let c = input.candidates.iter().find(|c| c.index == index).unwrap();
    let mut basis = vec![EvidenceRef {
        source: c.source.clone(),
        quote: c.candidate.sentence.clone(),
    }];
    for target in &targets {
        let record = input.records.iter().find(|r| r.id() == *target).unwrap();
        basis.extend(
            match record {
                RecordView2::Current(v) => &v.evidence,
                RecordView2::Legacy(v) => &v.evidence,
            }
            .iter()
            .cloned(),
        );
    }
    W4Decision {
        candidate: index,
        status: DecisionStatus::Decided,
        action: Some(action),
        targets,
        resolves_relations: vec![],
        basis,
        effective: None,
        relation_scope: if action == Action::Conflict {
            Some(RelationScope {
                conditions: vec![],
                times: vec![],
            })
        } else {
            None
        },
        pending_reason: None,
    }
}
pub fn additions(input: &W4Input) -> W4Output {
    W4Output {
        decisions: input
            .candidates
            .iter()
            .map(|c| decision(input, c.index, Action::New, vec![]))
            .collect(),
    }
}
pub fn seed(
    db: &Db,
    key: &str,
    text: &str,
    relation: &str,
    value: &str,
    polarity: CandidatePolarity,
) -> MemoryId {
    db.source(key, text, vec![]);
    let input = db.input(
        key,
        vec![candidate(key, text, relation, value, polarity, 0)],
    );
    db.apply(key, &input, &additions(&input)).unwrap()[&0]
}
