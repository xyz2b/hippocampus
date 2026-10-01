//! W5 验收第一段（docs/architecture/w5-acceptance.md A1–A3）：真实 SQLite 文件、真实写事务，
//! 每项提交后关闭并重新打开数据库再核对。场景对应 W4 样本 01、05、09、10、12、13。

use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use hippocampus::*;

static TEST_ID: AtomicU64 = AtomicU64::new(0);
const OWNER: &str = "alice";

struct TestDb(PathBuf);
impl TestDb {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "pmr-w5-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            TEST_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        Self(directory.join("private/memory.sqlite3"))
    }
    fn open(&self) -> SqliteMemoryStore {
        SqliteMemoryStore::open(&self.0, UserId::from(OWNER)).unwrap()
    }
}
impl Drop for TestDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.0.parent().unwrap().parent().unwrap());
    }
}

fn cand(source: &str, predicate: &str, value: &str, quote: &str, observed: i64) -> CandidateMemory {
    CandidateMemory::new(
        UserId::from(OWNER),
        EntityId::from("self"),
        MemoryType::Fact,
        Predicate::from(predicate),
        MemoryValue::Text(value.into()),
        Timestamp(observed),
        Evidence::new(
            EvidenceId::from(source),
            SessionId::from("chat"),
            TurnId::from(source),
            quote,
        ),
    )
}

/// 直接写入已存在的旧记录，不经过旧确定性策略（避免 Single 谓词的自动替换干扰初始状态）。
fn seed(db: &TestDb, records: &[CandidateMemory]) -> Vec<MemoryId> {
    db.open()
        .transaction(|state| {
            let mut ids = Vec::new();
            for c in records {
                let id = state.repository.allocate_id()?;
                state.repository.insert(Memory {
                    id,
                    owner_id: c.owner_id.clone(),
                    evidence: c.evidence.clone(),
                    subject_id: c.subject_id().unwrap().clone(),
                    memory_type: c.memory_type,
                    predicate: c.predicate.clone(),
                    value: c.value.clone(),
                    polarity: c.polarity,
                    strength: c.strength,
                    scope: c.scope.clone(),
                    retention_tier: c.retention_tier,
                    status: MemoryStatus::Active,
                    temporal: c.temporal.clone(),
                    extraction_confidence: c.extraction_confidence,
                    certainty: c.certainty,
                    supersedes: None,
                    superseded_by: None,
                    supersede_reason: None,
                    explicit_memory_request: false,
                    reinforcement_count: 0,
                    retrieval_count: 0,
                    last_reinforced_at: None,
                    last_retrieved_at: None,
                    tags: vec![],
                })?;
                ids.push(id);
            }
            Ok(ids)
        })
        .unwrap()
}

fn decision(
    candidate: usize,
    action: W5Action,
    targets: &[MemoryId],
    basis: &[&str],
    effective: Option<DatedValue>,
) -> W5Decision {
    W5Decision {
        candidate,
        action,
        targets: targets.to_vec(),
        basis_sources: basis.iter().map(|s| s.to_string()).collect(),
        reason: None,
        effective,
    }
}

fn group(
    id: &str,
    candidates: Vec<(usize, CandidateMemory)>,
    decisions: Vec<W5Decision>,
) -> W5Group {
    W5Group {
        id: id.into(),
        candidates: candidates
            .into_iter()
            .map(|(index, candidate)| W5Candidate { index, candidate })
            .collect(),
        decisions,
    }
}

/// 重新打开数据库后的业务快照：全部记录、revision、回执数。
fn snapshot(db: &TestDb) -> (serde_json::Value, u64, usize) {
    let state = db.open().read().unwrap();
    (
        serde_json::to_value(&state.repository).unwrap(),
        state.revision,
        state.w5_receipts.len(),
    )
}

fn memory(db: &TestDb, id: MemoryId) -> Memory {
    db.open()
        .read()
        .unwrap()
        .repository
        .get(id)
        .unwrap()
        .unwrap()
}

fn count(db: &TestDb) -> usize {
    db.open()
        .read()
        .unwrap()
        .repository
        .list_by_owner(&UserId::from(OWNER))
        .unwrap()
        .len()
}

fn committed(outcome: W5Outcome) -> W5Receipt {
    match outcome {
        W5Outcome::Committed(receipt) => receipt,
        other => panic!("expected Committed, got {other:?}"),
    }
}

// w4_01: effective 已核验（上个月 → 2026-08，月精度），旧记录以该时间结束。
fn w4_01(db: &TestDb) -> (MemoryId, W5Group, DatedValue) {
    let old = seed(
        db,
        &[cand("s-101", "lives_in", "北京", "我住在北京。", 100)],
    )[0];
    let effective = DatedValue {
        at: Timestamp(1785513600),
        source: TemporalSource::Inferred,
    };
    let g = group(
        "s-205#0",
        vec![(
            0,
            cand(
                "s-205",
                "lives_in",
                "上海",
                "我上个月从北京搬到上海了。",
                200,
            ),
        )],
        vec![decision(
            0,
            W5Action::StateChange,
            &[old],
            &["s-101", "s-205"],
            Some(effective),
        )],
    );
    (old, g, effective)
}

// w4_12: 两个目标一起结束，只插入一个新版本；effective 未知。
fn w4_12(db: &TestDb) -> (Vec<MemoryId>, W5Group) {
    let olds = seed(
        db,
        &[
            cand("s-302", "lives_in", "北京", "我住在北京。", 100),
            cand("s-303", "lives_in", "北京朝阳区", "我家在北京朝阳区。", 110),
        ],
    );
    let g = group(
        "s-340#0",
        vec![(
            0,
            cand("s-340", "lives_in", "深圳", "我已经搬到深圳了。", 200),
        )],
        vec![decision(
            0,
            W5Action::StateChange,
            &olds,
            &["s-302", "s-303", "s-340"],
            None,
        )],
    );
    (olds, g)
}

// ---------------- A1 成功提交后重载

#[test]
fn a1_state_change_with_effective_survives_reload() {
    let db = TestDb::new();
    let (old, g, effective) = w4_01(&db);
    let (_, revision_before, _) = snapshot(&db);
    let receipt = committed(commit_group(&mut db.open(), &g).unwrap());
    let new = receipt.products[&0usize];

    let old_m = memory(&db, old);
    assert_eq!(old_m.status, MemoryStatus::Superseded);
    assert_eq!(old_m.supersede_reason, Some(SupersedeReason::StateChanged));
    assert_eq!(old_m.superseded_by, Some(new));
    assert_eq!(old_m.temporal.valid_to, Some(effective));
    let new_m = memory(&db, new);
    assert_eq!(new_m.status, MemoryStatus::Active);
    assert_eq!(new_m.supersedes, Some(old));
    assert_eq!(new_m.value, MemoryValue::Text("上海".into()));
    assert_eq!(new_m.temporal.valid_from, Some(effective));

    let state = db.open().read().unwrap();
    assert_eq!(state.revision, revision_before + 1);
    assert_eq!(state.w5_receipts["s-205#0"], receipt);
    assert_eq!(receipt.decisions[0].basis_sources, vec!["s-101", "s-205"]);
}

#[test]
fn a1_state_change_without_effective_keeps_end_unknown() {
    // w4_13：换工作但没给日期。旧记录结束时间保持未知，不回填接收时间（F1）。
    let db = TestDb::new();
    let old = seed(
        &db,
        &[cand(
            "s-304",
            "works_at",
            "字节跳动",
            "我在字节跳动工作。",
            100,
        )],
    )[0];
    let g = group(
        "s-350#0",
        vec![(
            0,
            cand("s-350", "works_at", "腾讯", "我换工作了，现在在腾讯。", 200),
        )],
        vec![decision(
            0,
            W5Action::StateChange,
            &[old],
            &["s-304", "s-350"],
            None,
        )],
    );
    let new = committed(commit_group(&mut db.open(), &g).unwrap()).products[&0usize];
    let old_m = memory(&db, old);
    assert_eq!(old_m.status, MemoryStatus::Superseded);
    assert_eq!(old_m.temporal.valid_to, None);
    assert_eq!(memory(&db, new).temporal.valid_from, None);
}

#[test]
fn a1_multi_target_ends_all_and_inserts_once() {
    let db = TestDb::new();
    let (olds, g) = w4_12(&db);
    let before = count(&db);
    let new = committed(commit_group(&mut db.open(), &g).unwrap()).products[&0usize];
    assert_eq!(count(&db), before + 1);
    for old in olds {
        let m = memory(&db, old);
        assert_eq!(m.status, MemoryStatus::Superseded);
        assert_eq!(m.superseded_by, Some(new));
        assert_eq!(m.temporal.valid_to, None);
    }
}

#[test]
fn a1_new_leaves_unrelated_record_untouched() {
    // w4_09
    let db = TestDb::new();
    let other = seed(
        &db,
        &[cand("s-301", "lives_in", "北京", "我住在北京。", 100)],
    )[0];
    let before = memory(&db, other);
    let g = group(
        "s-310#0",
        vec![(
            0,
            cand("s-310", "works_at", "字节跳动", "我在字节跳动工作。", 200),
        )],
        vec![decision(0, W5Action::New, &[], &["s-310"], None)],
    );
    let new = committed(commit_group(&mut db.open(), &g).unwrap()).products[&0usize];
    assert_eq!(memory(&db, other), before);
    assert_eq!(memory(&db, new).status, MemoryStatus::Active);
}

#[test]
fn a1_reinforce_adds_evidence_and_counts_once() {
    // w4_10
    let db = TestDb::new();
    let target = seed(
        &db,
        &[cand(
            "s-111",
            "fact",
            "我喜欢喝咖啡。",
            "我喜欢喝咖啡。",
            100,
        )],
    )[0];
    let g = group(
        "s-320#0",
        vec![(
            0,
            cand(
                "s-320",
                "fact",
                "我还是喜欢喝咖啡。",
                "我还是喜欢喝咖啡。",
                200,
            ),
        )],
        vec![decision(
            0,
            W5Action::Reinforce,
            &[target],
            &["s-111", "s-320"],
            None,
        )],
    );
    let before = count(&db);
    let receipt = committed(commit_group(&mut db.open(), &g).unwrap());
    assert!(receipt.products.is_empty());
    assert_eq!(count(&db), before);
    let m = memory(&db, target);
    assert_eq!(m.status, MemoryStatus::Active);
    assert_eq!(m.reinforcement_count, 1);
    assert_eq!(m.evidence.len(), 2);
}

#[test]
fn a1_retract_ends_with_unknown_end() {
    // w4_05
    let db = TestDb::new();
    let target = seed(
        &db,
        &[cand(
            "s-120",
            "fact",
            "我每周三晚上去游泳。",
            "我每周三晚上去游泳。",
            100,
        )],
    )[0];
    let mut retraction = cand(
        "s-250",
        "fact",
        "撤回",
        "我之前说每周三晚上去游泳，那句不对，收回。",
        200,
    );
    retraction.intent = CandidateIntent::Retract {
        target: Some(target),
    };
    let g = group(
        "s-250#0",
        vec![(0, retraction)],
        vec![decision(
            0,
            W5Action::Retract,
            &[target],
            &["s-120", "s-250"],
            None,
        )],
    );
    let before = count(&db);
    committed(commit_group(&mut db.open(), &g).unwrap());
    assert_eq!(count(&db), before);
    let m = memory(&db, target);
    assert_eq!(m.status, MemoryStatus::Superseded);
    assert_eq!(m.supersede_reason, Some(SupersedeReason::UserRetracted));
    assert_eq!(m.superseded_by, None);
    assert_eq!(m.temporal.valid_to, None);
    assert!(m.evidence.iter().any(|e| e.id == EvidenceId::from("s-250")));
}

// ---------------- A2 故障回滚

fn fail_at(step: W5Step) -> impl FnMut(W5Step) -> PersistenceResult<()> {
    move |s| {
        if s == step {
            Err("injected fault".into())
        } else {
            Ok(())
        }
    }
}

#[test]
fn a2_fault_rolls_back_everything() {
    for step in [W5Step::AfterDecision(0), W5Step::BeforeSave] {
        let db = TestDb::new();
        let (_, g) = w4_12(&db);
        let before = snapshot(&db);
        let result = commit_group_with_hook(&mut db.open(), &g, &mut fail_at(step));
        assert!(result.is_err(), "{step:?}");
        assert_eq!(snapshot(&db), before, "{step:?}");
    }
}

#[test]
fn a2_fault_between_two_decisions_rolls_back_both() {
    // 一组两个决定：先撤回一条，再替换另一条；在两者之间注入故障，两者都不生效。
    let db = TestDb::new();
    let ids = seed(
        &db,
        &[
            cand(
                "s-120",
                "fact",
                "我每周三晚上去游泳。",
                "我每周三晚上去游泳。",
                100,
            ),
            cand("s-304", "works_at", "字节跳动", "我在字节跳动工作。", 110),
        ],
    );
    let mut retraction = cand("s-400", "fact", "撤回", "游泳那句收回，我现在在腾讯。", 200);
    retraction.intent = CandidateIntent::Retract {
        target: Some(ids[0]),
    };
    let g = group(
        "s-400#0",
        vec![
            (0, retraction),
            (
                1,
                cand(
                    "s-400b",
                    "works_at",
                    "腾讯",
                    "游泳那句收回，我现在在腾讯。",
                    200,
                ),
            ),
        ],
        vec![
            decision(0, W5Action::Retract, &[ids[0]], &["s-120", "s-400"], None),
            decision(
                1,
                W5Action::StateChange,
                &[ids[1]],
                &["s-304", "s-400"],
                None,
            ),
        ],
    );
    let before = snapshot(&db);
    assert!(
        commit_group_with_hook(&mut db.open(), &g, &mut fail_at(W5Step::AfterDecision(0))).is_err()
    );
    assert_eq!(snapshot(&db), before);
    assert!(
        commit_group_with_hook(&mut db.open(), &g, &mut fail_at(W5Step::AfterDecision(1))).is_err()
    );
    assert_eq!(snapshot(&db), before);
}

#[test]
fn a2_invalid_group_changes_nothing() {
    let db = TestDb::new();
    let other = seed(
        &db,
        &[cand("s-301", "lives_in", "北京", "我住在北京。", 100)],
    )[0];
    let g = group(
        "s-310#0",
        vec![(
            0,
            cand("s-310", "works_at", "字节跳动", "我在字节跳动工作。", 200),
        )],
        vec![decision(0, W5Action::New, &[other], &["s-310"], None)],
    );
    let before = snapshot(&db);
    assert!(commit_group(&mut db.open(), &g).is_err());
    assert_eq!(snapshot(&db), before);
}

#[test]
fn a2_duplicate_candidate_index_is_rejected() {
    // 两个候选同为 0：执行时只会取到第一个，必须整组拒绝且零写入。
    let db = TestDb::new();
    seed(
        &db,
        &[cand("s-301", "lives_in", "北京", "我住在北京。", 100)],
    );
    let g = group(
        "s-310#0",
        vec![
            (
                0,
                cand("s-310", "works_at", "字节跳动", "我在字节跳动工作。", 200),
            ),
            (0, cand("s-311", "works_at", "腾讯", "我在腾讯工作。", 200)),
        ],
        vec![decision(0, W5Action::New, &[], &["s-310"], None)],
    );
    let before = snapshot(&db);
    assert!(commit_group(&mut db.open(), &g).is_err());
    assert_eq!(snapshot(&db), before);
}

// ---------------- A3 重复重试（幂等）

#[test]
fn a3_replaying_a_committed_group_changes_nothing() {
    let db = TestDb::new();
    let (_, g, _) = w4_01(&db);
    let receipt = committed(commit_group(&mut db.open(), &g).unwrap());
    let after_first = snapshot(&db);
    match commit_group(&mut db.open(), &g).unwrap() {
        W5Outcome::AlreadyApplied(r) => assert_eq!(r, receipt),
        other => panic!("expected AlreadyApplied, got {other:?}"),
    }
    assert_eq!(snapshot(&db), after_first);
}

#[test]
fn a3_replaying_reinforce_does_not_count_twice() {
    let db = TestDb::new();
    let target = seed(
        &db,
        &[cand(
            "s-111",
            "fact",
            "我喜欢喝咖啡。",
            "我喜欢喝咖啡。",
            100,
        )],
    )[0];
    let g = group(
        "s-320#0",
        vec![(
            0,
            cand(
                "s-320",
                "fact",
                "我还是喜欢喝咖啡。",
                "我还是喜欢喝咖啡。",
                200,
            ),
        )],
        vec![decision(
            0,
            W5Action::Reinforce,
            &[target],
            &["s-111", "s-320"],
            None,
        )],
    );
    commit_group(&mut db.open(), &g).unwrap();
    commit_group(&mut db.open(), &g).unwrap();
    // 同一来源放进另一个组 ID 也不再计数（证据只计一次）。
    let mut again = g.clone();
    again.id = "s-320#retry".into();
    commit_group(&mut db.open(), &again).unwrap();
    let m = memory(&db, target);
    assert_eq!(m.reinforcement_count, 1);
    assert_eq!(m.evidence.len(), 2);
}

#[test]
fn a3_retry_after_fault_applies_exactly_once() {
    let db = TestDb::new();
    let (olds, g) = w4_12(&db);
    let before = count(&db);
    assert!(commit_group_with_hook(&mut db.open(), &g, &mut fail_at(W5Step::BeforeSave)).is_err());
    let new = committed(commit_group(&mut db.open(), &g).unwrap()).products[&0usize];
    assert!(matches!(
        commit_group(&mut db.open(), &g).unwrap(),
        W5Outcome::AlreadyApplied(_)
    ));
    assert_eq!(count(&db), before + 1);
    for old in olds {
        assert_eq!(memory(&db, old).superseded_by, Some(new));
    }
}

#[test]
fn a3_same_group_id_with_different_content_is_rejected() {
    let db = TestDb::new();
    let (_, g, _) = w4_01(&db);
    commit_group(&mut db.open(), &g).unwrap();
    let after_first = snapshot(&db);
    let mut changed = g.clone();
    changed.decisions[0].basis_sources.push("s-999".into());
    assert!(commit_group(&mut db.open(), &changed).is_err());
    assert_eq!(snapshot(&db), after_first);
}

#[test]
fn a3_receipts_live_in_a_version_older_binaries_reject() {
    // 引入 W5 回执前的程序只接受版本 1..=5，重写快照时会丢掉 w5_receipts，
    // 随后重放已提交的组会再插入一次；数据库因此升到 6，让旧程序拒绝打开。
    let db = TestDb::new();
    let (_, g, _) = w4_01(&db);
    commit_group(&mut db.open(), &g).unwrap();
    let connection = rusqlite::Connection::open(&db.0).unwrap();
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 6);
    let raw: String = connection
        .query_row("SELECT state_json FROM memory_profiles", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(raw.contains("\"w5_receipts\":{\"s-205#0\""), "{raw}");
}
