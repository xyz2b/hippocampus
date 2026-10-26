//! AF216: projection through W4/W5/SQLite and R4, with injected semantic decisions only.
#[path = "eval/write_stage/af216/storage.rs"]
mod storage;
use hippocampus::contract::Polarity as CPolarity;
use hippocampus::contract::*;
use hippocampus::*;
use serde_json::json;
use storage::*;

fn memory(db: &Db, id: MemoryId) -> Memory {
    db.state().repository.get(id).unwrap().unwrap()
}
fn recall(db: &Db, query: &str) -> recall::Assembly {
    let req = recall::Request {
        binding: RecallRequest {
            request_id: "af216-read".into(),
            query: query.into(),
            context: vec![],
            task_id: None,
            at: AT + 100,
        },
        owner: OWNER.into(),
        subject_ids: None,
        said_on: DAY.into(),
        utc_offset_seconds: 28800,
        bootstrap: false,
    };
    let entities = [EntityId::from("self")];
    let relations = [w2::Relation {
        id: "fact".into(),
        memory_types: vec![MemoryType::Fact],
    }];
    let ctx = r1::Context {
        query,
        context: &[],
        entities: &entities,
        relations: &relations,
        subject_ids: None,
        task_id: None,
        said_on: DAY,
        utc_offset_seconds: 28800,
        supported_units: recall_models::SUPPORTED_UNITS,
    };
    let plan = r1::validate(
        r1::decode(
            &json!({"need":"judge","current_evidence":"query_only","type":"open",
        "subject":{"entity_id":"self"},"relation":"使用物品","conditions":[],"time":null,
        "plan":{"op":"list","filter":{"relation":null,"time":null},"unit":null}})
            .to_string(),
        )
        .unwrap(),
        &ctx,
    )
    .unwrap();
    recall::assemble(&db.open(), &req, &plan, recall::Budget::default()).unwrap()
}
fn fact(
    key: &str,
    sentence: &str,
    statement: &str,
    ids: Vec<MemoryId>,
    polarity: &str,
) -> serde_json::Value {
    json!({"statement":statement,"evidence":[{"source":{"kind":"stored","source_id":key},"quote":{"text":sentence}}],
        "record_ids":ids,"polarity":polarity,"strength":null,"uncertainty":null,"conditions":[],"times":[],
        "current":"unknown","applies":"yes","supersedes_record_ids":[]})
}

#[test]
fn af216_same_sentence_siblings_keep_polarity_identity_grade_and_pending_separate() {
    let db = Db::new();
    let text = "我不使用石杯，我使用竹碗。";
    db.source("siblings", text, vec![]);
    let input = db.input(
        "siblings",
        vec![
            candidate(
                "siblings",
                text,
                "fact",
                "我不使用石杯",
                CPolarity::Negate,
                0,
            ),
            candidate("siblings", text, "fact", "我使用竹碗", CPolarity::Affirm, 1),
        ],
    );
    let ids = db.apply("siblings", &input, &additions(&input)).unwrap();
    assert_ne!(ids[&0], ids[&1]);
    assert_eq!(
        memory(&db, ids[&0]).polarity,
        Some(hippocampus::Polarity::Negative)
    );
    assert_eq!(memory(&db, ids[&1]).polarity, None);
    let mut indexed = db.open();
    indexed.set_fact_index_enabled(true);
    indexed.rebuild_fact_index().unwrap();
    let hints: String = rusqlite::Connection::open(&db.0)
        .unwrap()
        .query_row(
            "SELECT hints_json FROM fact_indexes WHERE owner_id='eval'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let hints: serde_json::Value = serde_json::from_str(&hints).unwrap();
    assert!(hints.as_array().unwrap().iter().any(|h| {
        serde_json::from_str::<serde_json::Value>(h["text"].as_str().unwrap()).unwrap()["polarity"]
            == "Negative"
    }));
    assert!(
        matches!(w4::record_view(&db.state(),ids[&0]),Some(RecordView2::Current(v)) if v.candidate.polarity==CPolarity::Negate)
    );
    let pending = UpdateState {
        status: UpdateStatus::Pending,
        reasons: vec![UpdateReason::UpdateScopeIncomplete],
    };
    db.open()
        .transaction(|s| {
            s.record_updates.insert(ids[&0], pending.clone());
            Ok(())
        })
        .unwrap();
    let result=recall(&db,"我使用和不使用哪些物品？").finish_r4(&db.open(),Ok(&json!({"verdict":"supported","compute":null,
        "facts":[fact("siblings",text,"我不使用石杯",vec![ids[&0]],"negate"),
            fact("siblings",text,"我使用竹碗",vec![ids[&1]],"affirm"),fact("siblings",text,"我不使用石杯",vec![],"negate")]}).to_string())).unwrap();
    assert_eq!(result.status, RecallStatus::Answered);
    assert_eq!(result.facts[0].polarity, CPolarity::Negate);
    assert_eq!(result.facts[0].grade, RecallGrade::Committed);
    assert_eq!(result.facts[0].update, Some(pending));
    assert_eq!(result.facts[1].grade, RecallGrade::Committed);
    assert_eq!(
        result.facts[1].update.as_ref().unwrap().status,
        UpdateStatus::Judged
    );
    assert_eq!(result.facts[2].grade, RecallGrade::Uncommitted);
    assert_eq!(result.facts[2].update, None);
}

#[test]
fn af216_negative_fact_commits_when_scope_incomplete_and_does_not_create_conflict() {
    let db = Db::new();
    db.source("negative", "我不使用石杯。", vec![]);
    let mut input = db.input(
        "negative",
        vec![candidate(
            "negative",
            "我不使用石杯。",
            "fact",
            "我不使用石杯",
            CPolarity::Negate,
            0,
        )],
    );
    input.coverage.complete = false;
    let output = W4Output {
        decisions: vec![W4Decision {
            candidate: 0,
            status: DecisionStatus::Pending,
            action: None,
            targets: vec![],
            resolves_relations: vec![],
            basis: vec![],
            effective: None,
            relation_scope: None,
            pending_reason: Some(PendingReason::ScopeIncomplete),
        }],
    };
    let ids = db.apply("negative", &input, &output).unwrap();
    assert_eq!(
        memory(&db, ids[&0]).polarity,
        Some(hippocampus::Polarity::Negative)
    );
    assert_eq!(
        db.state().record_updates[&ids[&0]],
        UpdateState {
            status: UpdateStatus::Pending,
            reasons: vec![UpdateReason::UpdateScopeIncomplete]
        }
    );
    assert!(db.state().relations.is_empty());
}

#[test]
fn af216_negative_fact_update_actions_keep_model_decisions_and_legacy_data() {
    for action in [
        Action::New,
        Action::StateChange,
        Action::Correction,
        Action::Conflict,
    ] {
        let db = Db::new();
        let old = seed(
            &db,
            "old",
            "我住在松丘。",
            "lives_in",
            "松丘",
            CPolarity::Affirm,
        );
        // An existing legacy record remains readable, with no invented Candidate.
        let legacy = seed(
            &db,
            "legacy",
            "我收藏木牌。",
            "fact",
            "我收藏木牌。",
            CPolarity::Affirm,
        );
        db.open()
            .transaction(|s| {
                s.w5_receipts
                    .retain(|_, r| !r.products.values().any(|id| *id == legacy));
                Ok(())
            })
            .unwrap();
        assert!(
            matches!(w4::record_view(&db.state(),legacy),Some(RecordView2::Legacy(v)) if v.data.polarity.is_none())
        );
        db.source("new", "我不住在松丘。", vec!["我住在松丘。".into()]);
        let input = db.input(
            "new",
            vec![candidate(
                "new",
                "我不住在松丘。",
                "lives_in",
                "松丘",
                CPolarity::Negate,
                0,
            )],
        );
        assert!(
            input.records.iter().any(|r| r.id() == old),
            "opposite polarity must remain visible to W4"
        );
        let ids = db
            .apply(
                "new",
                &input,
                &W4Output {
                    decisions: vec![decision(
                        &input,
                        0,
                        action,
                        if action == Action::New {
                            vec![]
                        } else {
                            vec![old]
                        },
                    )],
                },
            )
            .unwrap();
        let new = ids[&0];
        assert_eq!(
            memory(&db, new).polarity,
            Some(hippocampus::Polarity::Negative)
        );
        assert_eq!(memory(&db, legacy).status, MemoryStatus::Active);
        if action == Action::New {
            assert_eq!(memory(&db, old).status, MemoryStatus::Active);
            assert!(db.state().relations.is_empty());
        } else if action == Action::Conflict {
            assert_eq!(memory(&db, old).status, MemoryStatus::Active);
            assert!(
                db.state()
                    .relations
                    .values()
                    .any(|r| r.status == hippocampus::RelationStatus::Open
                        && r.members.contains(&old)
                        && r.members.contains(&new))
            );
        } else {
            assert_eq!(memory(&db, old).status, MemoryStatus::Superseded);
            assert_eq!(memory(&db, old).superseded_by, Some(new));
            assert_eq!(
                memory(&db, old).supersede_reason,
                Some(if action == Action::Correction {
                    SupersedeReason::Correction
                } else {
                    SupersedeReason::StateChanged
                })
            );
        }
    }
}

#[test]
fn af216_reinforce_replay_is_idempotent_and_tampered_polarity_cannot_reuse_record() {
    let db = Db::new();
    let text = "我不住在松丘。";
    let old = seed(&db, "old", text, "lives_in", "松丘", CPolarity::Negate);
    db.source("new", text, vec![]);
    let input = db.input(
        "new",
        vec![candidate(
            "new",
            text,
            "lives_in",
            "松丘",
            CPolarity::Negate,
            0,
        )],
    );
    let output = W4Output {
        decisions: vec![decision(&input, 0, Action::Reinforce, vec![old])],
    };
    let plan = db.plan("new", &input, &output, 0).unwrap();
    let first = commit_group(&mut db.open(), &claim("new"), &plan.groups[0]).unwrap();
    assert!(matches!(first, W5Outcome::Committed(_)));
    let before = memory(&db, old);
    let second = commit_group(&mut db.open(), &claim("new"), &plan.groups[0]).unwrap();
    assert!(matches!(second, W5Outcome::AlreadyApplied(_)));
    assert_eq!(memory(&db, old), before);
    assert_eq!(before.reinforcement_count, 1);
    assert_eq!(before.polarity, Some(hippocampus::Polarity::Negative));

    let mut same = db.input(
        "old",
        vec![candidate(
            "old",
            text,
            "lives_in",
            "松丘",
            CPolarity::Negate,
            0,
        )],
    );
    same.candidates[0].committed_id = Some(old);
    // Reclaim the creating source for explicit rejudge, preserving its original identity.
    db.open()
        .transaction(|s| {
            let r = s.revision;
            let source = s.sources.get_mut("old").unwrap();
            source.enrichment = "running".into();
            source.task.attempt_id = Some("claim-old".into());
            source.task.lease_until = Some(4_000_000_000);
            source.task.claim_revision = Some(r);
            Ok(())
        })
        .unwrap();
    let business = serde_json::to_value(&db.state().repository).unwrap();
    for round in 1..=2 {
        let mut group = db
            .plan("old", &same, &additions(&same), round)
            .unwrap()
            .groups
            .remove(0);
        if round == 1 {
            group.candidates[0].candidate.polarity = None;
        } else {
            group.candidates[0]
                .contract
                .as_mut()
                .unwrap()
                .candidate
                .polarity = CPolarity::Affirm;
        }
        assert!(matches!(
            commit_group(&mut db.open(), &claim("old"), &group).unwrap(),
            W5Outcome::NeedsRejudge(_)
        ));
        assert_eq!(
            serde_json::to_value(&db.state().repository).unwrap(),
            business
        );
    }
}

#[test]
fn af216_forgetting_negative_source_scrubs_content_and_reopen_cannot_restore_it() {
    let db = Db::new();
    let text = "我不使用琉璃杖。";
    let id = seed(
        &db,
        "private-negative",
        text,
        "fact",
        text,
        CPolarity::Negate,
    );
    let mut service = chat::ChatMemoryService::new(db.open(), chat::ChatConfig::default()).unwrap();
    service
        .call(
            "memory_forget",
            json!({"source_id":"private-negative"}),
            || false,
        )
        .unwrap();
    assert_eq!(memory(&db, id).status, MemoryStatus::Deleted);
    assert!(w4::record_view(&db.state(), id).is_none());
    let raw = serde_json::to_string(&db.state()).unwrap();
    assert!(!raw.contains("琉璃杖"));
    let assembly = recall(&db, "我使用哪些物品？");
    assert!(assembly.evidence.records.is_empty());
    assert!(assembly.evidence.raw.is_empty());
}
