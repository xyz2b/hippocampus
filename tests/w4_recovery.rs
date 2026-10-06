//! 第②段：经真实 SQLite 认领、持久计划和 W5 回执恢复；不调用模型。
use hippocampus::contract::*;
use hippocampus::*;
use serde_json::json;
use std::path::PathBuf;

struct Db(PathBuf);
impl Db {
    fn new() -> Self {
        Self(
            std::env::temp_dir()
                .join(format!("w4-recovery-{}", diagnostics::new_id()))
                .join("memory.sqlite3"),
        )
    }
    fn open(&self) -> SqliteMemoryStore {
        SqliteMemoryStore::open(&self.0, UserId::from("alice")).unwrap()
    }
}
impl Drop for Db {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.0.parent().unwrap());
    }
}

#[test]
fn v8_migrates_to_v9_without_enrolling_legacy_tasks() {
    let db = Db::new();
    std::fs::create_dir_all(db.0.parent().unwrap()).unwrap();
    let connection = rusqlite::Connection::open(&db.0).unwrap();
    connection
        .execute_batch(include_str!("fixtures/w5_storage/v8-9933ece.sql"))
        .unwrap();
    let raw: String = connection
        .query_row(
            "SELECT state_json FROM memory_profiles WHERE owner_id='alice'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let old: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let before: SourceRecord = serde_json::from_value(old["sources"]["legacy"].clone()).unwrap();
    assert_eq!(
        connection
            .pragma_query_value::<i64, _>(None, "user_version", |r| r.get(0))
            .unwrap(),
        8
    );
    drop(db.open());
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, 9);
    assert!(
        !(1..=8).contains(&version),
        "the released v8 writer rejects this version"
    );
    let after = db.open().read().unwrap().sources["legacy"].clone();
    assert_eq!(
        serde_json::to_value(&before).unwrap(),
        serde_json::to_value(&after).unwrap()
    );
    assert_eq!(after.task, EnrichmentTask::default());
    assert_eq!(after.enrichment, "failed");
}

fn source(db: &Db, key: &str, text: &str) {
    db.open()
        .transaction(|state| {
            let sequence = state.next_source_sequence()?;
            state.sources.insert(
                key.into(),
                SourceRecord {
                    id: key.into(),
                    session_id: "chat".into(),
                    turn_id: key.into(),
                    observed_at: 1700000000,
                    received_sequence: sequence,
                    request: Some(json!({"text":text,"capture_source":true})),
                    enrichment: "queued".into(),
                    error: None,
                    result: None,
                    base_revision: state.revision,
                    index: serde_json::Value::Null,
                    task: EnrichmentTask::default(),
                },
            );
            Ok(())
        })
        .unwrap();
}

fn prepared(db: &Db, claim: &W4Claim) -> (Vec<W4Candidate>, w4::Plan) {
    let sample: serde_json::Value =
        serde_json::from_str(include_str!("eval/w4/samples/w4_10_reinforce.json")).unwrap();
    let mut candidates = Vec::new();
    for (index, text) in [(0, "喜欢咖啡"), (1, "喜欢红茶")] {
        let mut c: W4Candidate =
            serde_json::from_value(sample["input"]["candidates"][0].clone()).unwrap();
        c.index = index;
        c.committed_id = None;
        c.source = SourceRef::Stored {
            source_id: claim.claim.source_key.clone(),
        };
        c.candidate.sentence.text = text.into();
        c.candidate.value = Some(hippocampus::contract::Value::Text {
            text: text.into(),
            quote: c.candidate.sentence.clone(),
        });
        candidates.push(c);
    }
    let saved = w4_progress(&db.open(), &claim.claim.source_key).unwrap();
    if !saved.candidates.is_empty() {
        candidates = saved.candidates;
    }
    let output = W4Output {
        decisions: candidates
            .iter()
            .map(|c| W4Decision {
                candidate: c.index,
                status: DecisionStatus::Decided,
                action: Some(Action::New),
                targets: vec![],
                resolves_relations: vec![],
                basis: vec![EvidenceRef {
                    source: c.source.clone(),
                    quote: c.candidate.sentence.clone(),
                }],
                effective: None,
                relation_scope: None,
                pending_reason: None,
            })
            .collect(),
    };
    let input = W4Input {
        candidates: candidates.clone(),
        records: candidates
            .iter()
            .filter_map(|c| c.committed_id)
            .filter_map(|id| w4::record_view(&db.open().read().unwrap(), id))
            .collect(),
        relations: vec![],
        coverage: W4Coverage {
            complete: false,
            omitted: 0,
        },
    };
    let state = db.open().read().unwrap();
    let rt = w4::W4Runtime {
        state: &state,
        owner: UserId::from("alice"),
        source_key: &claim.claim.source_key,
        round: claim.round,
        task_id: None,
        admissions: Default::default(),
    };
    let plan = w4::plan(&rt, &input, &output).unwrap();
    (candidates, plan)
}

#[test]
fn partial_commit_recovers_the_same_round_once_after_reopen() {
    let db = Db::new();
    source(&db, "s", "喜欢咖啡。喜欢红茶。");
    assert_eq!(
        w4_start_round(&mut db.open(), "s", "initial", false).unwrap(),
        0
    );
    let first = w4_claim_round(&mut db.open(), "s").unwrap();
    let (candidates, plan) = prepared(&db, &first);
    w4_save_plan(&mut db.open(), &first, &candidates, &plan).unwrap();
    w4_commit_group(&mut db.open(), &first, &plan.groups[0]).unwrap();
    expire(&db, "s");
    let next = w4_claim_round(&mut db.open(), "s").unwrap();
    assert_ne!(first.claim.attempt_id, next.claim.attempt_id);
    assert_eq!(next.round, 0);
    assert_eq!(next.operation_id, "initial");
    let progress = w4_progress(&db.open(), "s").unwrap();
    assert_eq!(progress.committed.len(), 1);
    assert_eq!(progress.remaining.len(), 1);
    assert!(matches!(
        w4_commit_group(&mut db.open(), &next, &plan.groups[0]).unwrap(),
        W5Outcome::AlreadyApplied(_)
    ));
    w4_commit_group(&mut db.open(), &next, &progress.remaining[0]).unwrap();
    w4_finish_round(&mut db.open(), &next).unwrap();
    let state = db.open().read().unwrap();
    assert_eq!(
        state
            .repository
            .list_by_owner(&UserId::from("alice"))
            .unwrap()
            .len(),
        2
    );
    assert_eq!(state.sources["s"].enrichment, "committed");
    assert_eq!(w4_progress(&db.open(), "s").unwrap().committed.len(), 2);
}

fn planned(db: &Db, key: &str) -> (W4Claim, Vec<W4Candidate>, w4::Plan) {
    source(db, key, "喜欢咖啡。喜欢红茶。");
    w4_start_round(&mut db.open(), key, "initial", false).unwrap();
    let claim = w4_claim_round(&mut db.open(), key).unwrap();
    let (candidates, plan) = prepared(db, &claim);
    w4_save_plan(&mut db.open(), &claim, &candidates, &plan).unwrap();
    (claim, candidates, plan)
}

#[test]
fn explicit_rejudge_is_deduplicated_and_reuses_assertions() {
    let db = Db::new();
    let (first, _, plan) = planned(&db, "s");
    for group in &plan.groups {
        w4_commit_group(&mut db.open(), &first, group).unwrap();
    }
    w4_finish_round(&mut db.open(), &first).unwrap();
    let before = db.open().read().unwrap().sources["s"].clone();
    let products = w4_progress(&db.open(), "s").unwrap().products;
    assert!(w4_claim_round(&mut db.open(), "s").is_err());
    assert_eq!(
        w4_start_round(&mut db.open(), "s", "rejudge-1", true).unwrap(),
        1
    );
    assert_eq!(
        w4_start_round(&mut db.open(), "s", "rejudge-1", true).unwrap(),
        1
    );
    let second = w4_claim_round(&mut db.open(), "s").unwrap();
    let (candidates, plan) = prepared(&db, &second);
    for c in &candidates {
        assert_eq!(c.committed_id, products.get(&c.index).copied());
    }
    w4_save_plan(&mut db.open(), &second, &candidates, &plan).unwrap();
    for group in &plan.groups {
        assert!(matches!(
            w4_commit_group(&mut db.open(), &second, group).unwrap(),
            W5Outcome::Committed(_)
        ));
    }
    w4_finish_round(&mut db.open(), &second).unwrap();
    assert_eq!(
        w4_start_round(&mut db.open(), "s", "rejudge-2", true).unwrap(),
        2
    );
    assert_eq!(
        w4_start_round(&mut db.open(), "s", "rejudge-1", true).unwrap(),
        1
    );
    assert!(w4_start_round(&mut db.open(), "s", "rejudge-1", false).is_err());
    source(&db, "next", "下一条原文");
    let state = db.open().read().unwrap();
    assert_eq!(
        state.sources["s"].received_sequence,
        before.received_sequence
    );
    assert_eq!(state.sources["s"].observed_at, before.observed_at);
    assert!(state.sources["next"].received_sequence > state.sources["s"].order_sequence());
    let records = state
        .repository
        .list_by_owner(&UserId::from("alice"))
        .unwrap();
    assert_eq!(records.len(), 2);
    assert!(records.iter().all(|r| r.reinforcement_count == 0));
}

fn expire(db: &Db, key: &str) {
    // Simulate elapsed time in durable lease metadata; recovery still uses the real claim API.
    db.open()
        .transaction(|s| {
            s.sources.get_mut(key).unwrap().task.lease_until = Some(0);
            Ok(())
        })
        .unwrap();
}
fn snapshot(db: &Db) -> serde_json::Value {
    serde_json::to_value(db.open().read().unwrap()).unwrap()
}
fn assert_lost<T: std::fmt::Debug>(result: PersistenceResult<T>) {
    assert!(result.unwrap_err().to_string().contains("claim_lost"));
}

#[test]
fn missing_plan_recovers_without_inventing_no_facts_or_model_calls() {
    let db = Db::new();
    source(&db, "s", "喜欢咖啡。喜欢红茶。");
    w4_start_round(&mut db.open(), "s", "op", false).unwrap();
    let first = w4_claim_round(&mut db.open(), "s").unwrap();
    expire(&db, "s");
    let next = w4_claim_round(&mut db.open(), "s").unwrap();
    assert_eq!(next.round, first.round);
    assert_eq!(next.operation_id, first.operation_id);
    let result = w4_finish_round(&mut db.open(), &next).unwrap();
    assert!(!result.plan_ready);
    assert!(result.committed.is_empty());
    assert_eq!(db.open().read().unwrap().sources["s"].enrichment, "pending");
    assert_eq!(
        db.open().read().unwrap().sources["s"].error.as_deref(),
        Some("input_not_ready")
    );
    let resumed = w4_claim_round(&mut db.open(), "s").unwrap();
    assert_eq!(resumed.round, 0);
    let connection = rusqlite::Connection::open(&db.0).unwrap();
    let count: i64 = connection
        .query_row("SELECT COUNT(*) FROM model_calls", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
    w4_save_plan(
        &mut db.open(),
        &resumed,
        &[],
        &w4::Plan {
            groups: vec![],
            held: Default::default(),
        },
    )
    .unwrap();
    w4_finish_round(&mut db.open(), &resumed).unwrap();
    assert_eq!(db.open().read().unwrap().sources["s"].enrichment, "pending");
}

#[test]
fn all_groups_committed_before_summary_recover_after_newer_source() {
    let db = Db::new();
    let (first, _, plan) = planned(&db, "a");
    for group in &plan.groups {
        w4_commit_group(&mut db.open(), &first, group).unwrap();
    }
    let (newer, _, new_plan) = planned(&db, "b");
    for group in &new_plan.groups {
        w4_commit_group(&mut db.open(), &newer, group).unwrap();
    }
    w4_finish_round(&mut db.open(), &newer).unwrap();
    expire(&db, "a");
    let next = w4_claim_round(&mut db.open(), "a").unwrap();
    let before = db.open().read().unwrap();
    w4_finish_round(&mut db.open(), &next).unwrap();
    let after = db.open().read().unwrap();
    assert_eq!(
        serde_json::to_value(before.repository).unwrap(),
        serde_json::to_value(after.repository).unwrap()
    );
    assert_eq!(after.sources["a"].enrichment, "committed");
    assert_eq!(before.revision, after.revision);
}

#[test]
fn lease_transfer_rejects_every_late_writer_and_legacy_bypass() {
    let db = Db::new();
    let (old, candidates, plan) = planned(&db, "s");
    assert!(w4_claim_round(&mut db.open(), "s").is_err());
    assert!(w4_start_round(&mut db.open(), "s", "new", true).is_err());
    w4_renew_claim(&mut db.open(), &old).unwrap();
    expire(&db, "s");
    assert_lost(w4_commit_group(&mut db.open(), &old, &plan.groups[0]));
    let current = w4_claim_round(&mut db.open(), "s").unwrap();
    let before = snapshot(&db);
    assert_lost(w4_save_plan(&mut db.open(), &old, &candidates, &plan));
    assert_lost(w4_commit_group(&mut db.open(), &old, &plan.groups[0]));
    assert_lost(w4_renew_claim(&mut db.open(), &old));
    assert_lost(w4_finish_round(&mut db.open(), &old));
    assert_lost(w4_fail_attempt(&mut db.open(), &old, "late"));
    assert!(commit_group(&mut db.open(), &current.claim, &plan.groups[0]).is_err());
    assert!(w5_finish_task(&mut db.open(), &current.claim, &W5TaskEnd::Committed).is_err());
    assert_eq!(snapshot(&db), before);
}

#[test]
fn persisted_plan_is_immutable_and_bound_to_round_version_and_input() {
    let db = Db::new();
    let (claim, candidates, plan) = planned(&db, "s");
    w4_save_plan(&mut db.open(), &claim, &candidates, &plan).unwrap();
    let mut changed = plan.clone();
    changed.groups[0].id = "s#99#0".into();
    let before = snapshot(&db);
    assert!(w4_save_plan(&mut db.open(), &claim, &candidates, &changed).is_err());
    assert!(w4_commit_group(&mut db.open(), &claim, &changed.groups[0]).is_err());
    assert_eq!(snapshot(&db), before);
    for field in [
        "round",
        "processing_version",
        "operation_id",
        "order_sequence",
    ] {
        let mut changed_claim = claim.clone();
        db.open()
            .transaction(|s| {
                let task = &mut s.sources.get_mut("s").unwrap().task;
                match field {
                    "round" => task.round = Some(8),
                    "processing_version" => task.processing_version = Some("other".into()),
                    "operation_id" => task.operation_id = Some("other".into()),
                    _ => task.order_sequence = Some(99),
                };
                Ok(())
            })
            .unwrap();
        assert_lost(w4_renew_claim(&mut db.open(), &changed_claim));
        db.open()
            .transaction(|s| {
                let task = &mut s.sources.get_mut("s").unwrap().task;
                task.round = Some(claim.round);
                task.processing_version = Some(W4_PROCESSING_VERSION.into());
                task.operation_id = Some(claim.operation_id.clone());
                task.order_sequence = Some(claim.order_sequence);
                Ok(())
            })
            .unwrap();
        changed_claim.round = 99;
        assert_lost(w4_finish_round(&mut db.open(), &changed_claim));
    }
    db.open()
        .transaction(|s| {
            s.sources.get_mut("s").unwrap().request.as_mut().unwrap()["text"] = json!("改变输入");
            Ok(())
        })
        .unwrap();
    let before = snapshot(&db);
    assert!(
        w4_commit_group(&mut db.open(), &claim, &plan.groups[0])
            .unwrap_err()
            .to_string()
            .contains("needs_rejudge")
    );
    assert_eq!(snapshot(&db), before);
}

#[test]
fn newer_source_and_sourceless_barriers_block_only_remaining_work() {
    for (sourceless, submit_remaining) in
        [(false, false), (false, true), (true, false), (true, true)]
    {
        let db = Db::new();
        let (old, _, plan) = planned(&db, "a");
        w4_commit_group(&mut db.open(), &old, &plan.groups[0]).unwrap();
        if sourceless {
            db.open()
                .transaction(|s| {
                    let id = s.w5_receipts[&plan.groups[0].id].products[&0];
                    let mut m = s.repository.get(id)?.unwrap();
                    m.tags.push("changed".into());
                    s.repository.update(m)?;
                    Ok(())
                })
                .unwrap();
            assert!(db.open().read().unwrap().source_less_barrier_sequence >= old.order_sequence);
        } else {
            let (newer, _, new_plan) = planned(&db, "b");
            w4_commit_group(&mut db.open(), &newer, &new_plan.groups[0]).unwrap();
        }
        expire(&db, "a");
        let current = w4_claim_round(&mut db.open(), "a").unwrap();
        assert!(matches!(
            w4_commit_group(&mut db.open(), &current, &plan.groups[0]).unwrap(),
            W5Outcome::AlreadyApplied(_)
        ));
        if submit_remaining {
            assert!(matches!(
                w4_commit_group(&mut db.open(), &current, &plan.groups[1]).unwrap(),
                W5Outcome::NeedsRejudge(_)
            ));
        }
        let result = w4_finish_round(&mut db.open(), &current).unwrap();
        assert_eq!(result.committed.len(), 1);
        assert_eq!(result.needs_rejudge.len(), usize::from(submit_remaining));
        assert_eq!(result.remaining.len(), usize::from(!submit_remaining));
        assert_eq!(result.candidates.len(), 2);
        assert_eq!(
            db.open().read().unwrap().sources["a"].enrichment,
            "needs_rejudge"
        );
        assert!(w4_claim_round(&mut db.open(), "a").is_err());
    }
}

#[test]
fn equality_without_own_receipt_is_not_a_resume_permission() {
    let db = Db::new();
    let (claim, _, plan) = planned(&db, "s");
    db.open()
        .transaction(|s| {
            s.last_applied_source_sequence = claim.order_sequence;
            Ok(())
        })
        .unwrap();
    assert!(matches!(
        w4_commit_group(&mut db.open(), &claim, &plan.groups[0]).unwrap(),
        W5Outcome::NeedsRejudge(_)
    ));
    assert!(w4_progress(&db.open(), "s").unwrap().committed.is_empty());
}

fn forget_source(db: &Db, key: &str) {
    let mut service = chat::ChatMemoryService::new(db.open(), chat::ChatConfig::default()).unwrap();
    service
        .call("memory_forget", json!({"source_id":key}), || false)
        .unwrap();
}

#[test]
fn deletion_cleans_own_and_foreign_checkpoints_and_blocks_late_results() {
    let db = Db::new();
    let (a, _, a_plan) = planned(&db, "a");
    w4_commit_group(&mut db.open(), &a, &a_plan.groups[0]).unwrap();
    let id = w4_progress(&db.open(), "a").unwrap().products[&0];
    source(&db, "b", "喜欢咖啡。喜欢红茶。");
    db.open()
        .transaction(|s| {
            s.sources.get_mut("b").unwrap().request.as_mut().unwrap()["history"] =
                json!(["喜欢咖啡。喜欢红茶。"]);
            Ok(())
        })
        .unwrap();
    w4_start_round(&mut db.open(), "b", "initial", false).unwrap();
    let b = w4_claim_round(&mut db.open(), "b").unwrap();
    let (candidates, mut plan) = prepared(&db, &b);
    let memory = db
        .open()
        .read()
        .unwrap()
        .repository
        .get(id)
        .unwrap()
        .unwrap();
    plan.groups[1].seen.push(W5Seen {
        id,
        fingerprint: w5_fingerprint(&memory).unwrap(),
        relations: vec![],
    });
    plan.groups[1].decisions[0].basis.push(EvidenceRef {
        source: SourceRef::Stored {
            source_id: "a".into(),
        },
        quote: candidates[0].candidate.sentence.clone(),
    });
    w4_save_plan(&mut db.open(), &b, &candidates, &plan).unwrap();
    w4_commit_group(&mut db.open(), &b, &plan.groups[0]).unwrap();
    source(&db, "unrelated", "完全独立的消息");
    w4_start_round(&mut db.open(), "unrelated", "op", false).unwrap();
    let unrelated = db.open().read().unwrap().sources["unrelated"]
        .result
        .clone();
    forget_source(&db, "a");
    let state = db.open().read().unwrap();
    assert!(state.sources["a"].result.is_none());
    assert!(state.sources["a"].task.attempt_id.is_none());
    let checkpoint = &state.sources["b"].result.as_ref().unwrap()["w4_checkpoint"];
    assert!(checkpoint["plan"].is_null());
    assert_eq!(checkpoint["invalidated"], "source_deleted");
    assert!(!checkpoint.to_string().contains("喜欢咖啡"));
    assert!(!checkpoint.to_string().contains("喜欢红茶"));
    assert_eq!(state.sources["unrelated"].result, unrelated);
    assert_eq!(state.sources["b"].enrichment, "needs_rejudge");
    assert!(state.sources["b"].request.is_some());
    assert_eq!(
        state.sources["b"].request.as_ref().unwrap()["history"],
        json!([])
    );
    let progress = w4_progress(&db.open(), "b").unwrap();
    assert_eq!(progress.committed.len(), 1);
    assert!(progress.remaining.is_empty());
    let before = snapshot(&db);
    assert_lost(w4_commit_group(&mut db.open(), &a, &a_plan.groups[1]));
    assert_lost(w4_save_plan(&mut db.open(), &b, &candidates, &plan));
    assert_lost(w4_commit_group(&mut db.open(), &b, &plan.groups[1]));
    assert_lost(w4_finish_round(&mut db.open(), &b));
    assert_eq!(snapshot(&db), before);
    assert!(w4_claim_round(&mut db.open(), "b").is_err());
}

fn reinforcement(db: &Db, group: &mut W5Group, target: MemoryId) {
    group.decisions[0].action = W5Action::Reinforce;
    group.decisions[0].targets = vec![target];
    let memory = db
        .open()
        .read()
        .unwrap()
        .repository
        .get(target)
        .unwrap()
        .unwrap();
    group.seen = vec![W5Seen {
        id: target,
        fingerprint: w5_fingerprint(&memory).unwrap(),
        relations: vec![],
    }];
}

#[test]
fn one_changed_group_does_not_block_an_independent_group_or_repeat_reinforcement() {
    let db = Db::new();
    let (seed, _, seed_plan) = planned(&db, "seed");
    w4_commit_group(&mut db.open(), &seed, &seed_plan.groups[0]).unwrap();
    let target = w4_progress(&db.open(), "seed").unwrap().products[&0];
    source(&db, "actor", "喜欢咖啡。喜欢红茶。");
    w4_start_round(&mut db.open(), "actor", "initial", false).unwrap();
    let actor = w4_claim_round(&mut db.open(), "actor").unwrap();
    let (actor_candidates, mut actor_plan) = prepared(&db, &actor);
    reinforcement(&db, &mut actor_plan.groups[0], target);
    w4_save_plan(&mut db.open(), &actor, &actor_candidates, &actor_plan).unwrap();
    source(&db, "s", "喜欢咖啡。喜欢红茶。");
    w4_start_round(&mut db.open(), "s", "initial", false).unwrap();
    let claim = w4_claim_round(&mut db.open(), "s").unwrap();
    let (candidates, mut plan) = prepared(&db, &claim);
    plan.groups[0].seen = actor_plan.groups[0].seen.clone();
    w4_save_plan(&mut db.open(), &claim, &candidates, &plan).unwrap();
    w4_commit_group(&mut db.open(), &actor, &actor_plan.groups[0]).unwrap();
    assert!(matches!(
        w4_commit_group(&mut db.open(), &claim, &plan.groups[0]).unwrap(),
        W5Outcome::NeedsRejudge(_)
    ));
    assert!(matches!(
        w4_commit_group(&mut db.open(), &claim, &plan.groups[1]).unwrap(),
        W5Outcome::Committed(_)
    ));
    let result = w4_finish_round(&mut db.open(), &claim).unwrap();
    assert_eq!(result.committed.len(), 1);
    assert_eq!(result.needs_rejudge.len(), 1);
    assert_eq!(result.candidates.len(), 2);
    assert_eq!(
        db.open()
            .read()
            .unwrap()
            .repository
            .get(target)
            .unwrap()
            .unwrap()
            .reinforcement_count,
        1
    );
    w4_fail_attempt(&mut db.open(), &actor, "interrupt").unwrap();
    let resumed = w4_claim_round(&mut db.open(), "actor").unwrap();
    assert!(matches!(
        w4_commit_group(&mut db.open(), &resumed, &actor_plan.groups[0]).unwrap(),
        W5Outcome::AlreadyApplied(_)
    ));
    w4_fail_attempt(&mut db.open(), &resumed, "request rejudge").unwrap();
    w4_start_round(&mut db.open(), "actor", "rejudge", true).unwrap();
    let rejudge = w4_claim_round(&mut db.open(), "actor").unwrap();
    let (candidates, mut plan) = prepared(&db, &rejudge);
    reinforcement(&db, &mut plan.groups[0], target);
    w4_save_plan(&mut db.open(), &rejudge, &candidates, &plan).unwrap();
    w4_commit_group(&mut db.open(), &rejudge, &plan.groups[0]).unwrap();
    assert_eq!(
        db.open()
            .read()
            .unwrap()
            .repository
            .get(target)
            .unwrap()
            .unwrap()
            .reinforcement_count,
        1
    );
}

#[test]
fn rejudge_order_blocks_older_sources_and_sourceless_change_uses_task_order() {
    let db = Db::new();
    let (a, _, a_plan) = planned(&db, "a");
    for g in &a_plan.groups {
        w4_commit_group(&mut db.open(), &a, g).unwrap();
    }
    w4_finish_round(&mut db.open(), &a).unwrap();
    let (older, _, older_plan) = planned(&db, "b");
    w4_start_round(&mut db.open(), "a", "rejudge", true).unwrap();
    let rejudge = w4_claim_round(&mut db.open(), "a").unwrap();
    let (candidates, plan) = prepared(&db, &rejudge);
    w4_save_plan(&mut db.open(), &rejudge, &candidates, &plan).unwrap();
    w4_commit_group(&mut db.open(), &rejudge, &plan.groups[0]).unwrap();
    assert!(matches!(
        w4_commit_group(&mut db.open(), &older, &older_plan.groups[0]).unwrap(),
        W5Outcome::NeedsRejudge(_)
    ));
    assert_eq!(
        db.open().read().unwrap().last_applied_source_sequence,
        rejudge.order_sequence
    );
    db.open()
        .transaction(|s| {
            let id = candidates[0].committed_id.unwrap();
            let mut m = s.repository.get(id)?.unwrap();
            m.tags.push("changed".into());
            s.repository.update(m)?;
            Ok(())
        })
        .unwrap();
    let state = db.open().read().unwrap();
    assert_eq!(state.source_less_barrier_sequence, rejudge.order_sequence);
    assert!(state.source_less_barrier_sequence > state.sources["a"].received_sequence);
    assert!(matches!(
        w4_commit_group(&mut db.open(), &rejudge, &plan.groups[1]).unwrap(),
        W5Outcome::NeedsRejudge(_)
    ));
}

#[test]
fn real_v8_receipt_replays_unchanged_and_future_v10_is_rejected() {
    let db = Db::new();
    std::fs::create_dir_all(db.0.parent().unwrap()).unwrap();
    let connection = rusqlite::Connection::open(&db.0).unwrap();
    connection
        .execute_batch(include_str!("fixtures/w5_storage/v8-9933ece.sql"))
        .unwrap();
    let raw: String = connection
        .query_row("SELECT state_json FROM memory_profiles", [], |r| r.get(0))
        .unwrap();
    let old: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let group: W5Group = serde_json::from_str(
        old["w5_receipts"]["s-205#0"]["fingerprint"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    let claim = W5Claim {
        source_key: "s-205".into(),
        attempt_id: "replay".into(),
    };
    db.open()
        .transaction(|s| {
            let source = s.sources.get_mut("s-205").unwrap();
            source.enrichment = "running".into();
            source.task.attempt_id = Some("replay".into());
            source.task.lease_until = Some(i64::MAX);
            Ok(())
        })
        .unwrap();
    let before = snapshot(&db);
    assert_eq!(before["w5_receipts"], old["w5_receipts"]);
    assert!(matches!(
        commit_group(&mut db.open(), &claim, &group).unwrap(),
        W5Outcome::AlreadyApplied(_)
    ));
    assert_eq!(snapshot(&db), before);
    assert_eq!(
        serde_json::to_value(
            SqliteMemoryStore::open_read_only(&db.0, UserId::from("alice"))
                .unwrap()
                .read()
                .unwrap()
        )
        .unwrap(),
        before
    );
    connection.pragma_update(None, "user_version", 10).unwrap();
    assert!(SqliteMemoryStore::open(&db.0, UserId::from("alice")).is_err());
    assert!(SqliteMemoryStore::open_read_only(&db.0, UserId::from("alice")).is_err());
}

#[test]
fn old_explicit_entry_cannot_claim_the_new_processing_version() {
    let db = Db::new();
    source(&db, "s", "喜欢咖啡。喜欢红茶。");
    w4_start_round(&mut db.open(), "s", "op", false).unwrap();
    let budget =
        serde_json::from_value(json!({"group_id":"offline","max_calls":1,"max_operation_calls":1}))
            .unwrap();
    let config = chat::ChatConfig {
        write_pipeline: chat::WritePipeline::Legacy,
        model_budget: Some(budget),
        ..Default::default()
    };
    let mut service = chat::ChatMemoryService::new(db.open(), config).unwrap();
    let before = snapshot(&db);
    assert!(
        service
            .enrich_source("s")
            .unwrap_err()
            .to_string()
            .contains("unsupported_processing_version")
    );
    assert_eq!(snapshot(&db), before);
}

#[test]
fn rejudge_cannot_drop_binding_or_reinsert_an_inactive_product() {
    let db = Db::new();
    let (claim, _, plan) = planned(&db, "s");
    for g in &plan.groups {
        w4_commit_group(&mut db.open(), &claim, g).unwrap();
    }
    w4_finish_round(&mut db.open(), &claim).unwrap();
    w4_start_round(&mut db.open(), "s", "rejudge", true).unwrap();
    let next = w4_claim_round(&mut db.open(), "s").unwrap();
    let (mut candidates, mut plan) = prepared(&db, &next);
    let id = candidates[0].committed_id.unwrap();
    candidates[0].committed_id = None;
    plan.groups[0].candidates[0].committed_id = None;
    assert!(w4_save_plan(&mut db.open(), &next, &candidates, &plan).is_err());
    let (candidates, plan) = prepared(&db, &next);
    w4_save_plan(&mut db.open(), &next, &candidates, &plan).unwrap();
    db.open()
        .transaction(|s| {
            let mut m = s.repository.get(id)?.unwrap();
            m.status = MemoryStatus::Deleted;
            s.repository.update(m)?;
            Ok(())
        })
        .unwrap();
    assert!(matches!(
        w4_commit_group(&mut db.open(), &next, &plan.groups[0]).unwrap(),
        W5Outcome::NeedsRejudge(_)
    ));
    assert_eq!(
        db.open()
            .read()
            .unwrap()
            .repository
            .list_by_owner(&UserId::from("alice"))
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn plan_must_be_durable_and_complete_before_any_group_can_commit() {
    let db = Db::new();
    source(&db, "s", "喜欢咖啡。喜欢红茶。");
    w4_start_round(&mut db.open(), "s", "initial", false).unwrap();
    let claim = w4_claim_round(&mut db.open(), "s").unwrap();
    let (candidates, plan) = prepared(&db, &claim);
    let before = snapshot(&db);
    assert!(w4_commit_group(&mut db.open(), &claim, &plan.groups[0]).is_err());
    for case in 0..5 {
        let mut invalid = plan.clone();
        match case {
            0 => invalid.groups[0].id = "s#99#0".into(),
            1 => invalid.groups[0].source_key = "other".into(),
            2 => {
                invalid.groups.pop();
            }
            3 => invalid.groups.push(invalid.groups[0].clone()),
            _ => {
                invalid.groups[0].candidates[0]
                    .contract
                    .as_mut()
                    .unwrap()
                    .candidate
                    .sentence
                    .text = " changed".into()
            }
        }
        assert!(w4_save_plan(&mut db.open(), &claim, &candidates, &invalid).is_err());
        assert_eq!(snapshot(&db), before);
    }
    w4_save_plan(&mut db.open(), &claim, &candidates, &plan).unwrap();
    assert!(w4_finish_round(&mut db.open(), &claim).is_err());
    for group in &plan.groups {
        w4_commit_group(&mut db.open(), &claim, group).unwrap();
    }
    w4_finish_round(&mut db.open(), &claim).unwrap();
    let state = db.open().read().unwrap();
    assert_eq!(state.sources["s"].enrichment, "committed");
    assert!(
        state
            .record_updates
            .values()
            .all(|u| u.status == UpdateStatus::Pending)
    );
}
