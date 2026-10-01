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

const ATTEMPT: &str = "attempt-1";

/// 写入（若尚无）一条原文任务，并模拟现有认领：running、attempt_id、租约未过期。
fn ensure_claimed_source(db: &TestDb, key: &str) {
    db.open()
        .transaction(|state| {
            if state.sources.contains_key(key) {
                return Ok(());
            }
            let sequence = state.sources.len() as u64 + 1;
            let task = EnrichmentTask {
                processing_version: Some("w5-acceptance".into()),
                attempt_count: 1,
                attempt_id: Some(ATTEMPT.into()),
                lease_until: Some(now() + 900),
                claim_revision: Some(state.revision),
                accepted_mode: Some("inline".into()),
            };
            state.sources.insert(
                key.into(),
                SourceRecord {
                    id: key.into(),
                    session_id: "chat".into(),
                    turn_id: key.into(),
                    observed_at: 200,
                    received_sequence: sequence,
                    request: Some(serde_json::json!({"text": key})),
                    enrichment: "running".into(),
                    error: None,
                    result: None,
                    base_revision: state.revision,
                    index: serde_json::Value::Null,
                    task,
                },
            );
            Ok(())
        })
        .unwrap()
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// 构造依赖组：所属原文为组 ID 中 `#` 之前的部分；在此刻（W4 输入时）记下全部目标的快照。
fn group(
    db: &TestDb,
    id: &str,
    candidates: Vec<(usize, CandidateMemory)>,
    decisions: Vec<W5Decision>,
) -> W5Group {
    let source_key = id.split('#').next().unwrap().to_string();
    ensure_claimed_source(db, &source_key);
    let targets: Vec<MemoryId> = decisions.iter().flat_map(|d| d.targets.clone()).collect();
    let seen = w5_seen(&db.open().read().unwrap().repository, &targets).unwrap();
    W5Group {
        id: id.into(),
        source_key,
        candidates: candidates
            .into_iter()
            .map(|(index, candidate)| W5Candidate { index, candidate })
            .collect(),
        decisions,
        seen,
    }
}

fn claim(g: &W5Group) -> W5Claim {
    W5Claim {
        source_key: g.source_key.clone(),
        attempt_id: ATTEMPT.into(),
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
        db,
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
        db,
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
    let receipt = committed(commit_group(&mut db.open(), &claim(&g), &g).unwrap());
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
        &db,
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
    let new = committed(commit_group(&mut db.open(), &claim(&g), &g).unwrap()).products[&0usize];
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
    let new = committed(commit_group(&mut db.open(), &claim(&g), &g).unwrap()).products[&0usize];
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
        &db,
        "s-310#0",
        vec![(
            0,
            cand("s-310", "works_at", "字节跳动", "我在字节跳动工作。", 200),
        )],
        vec![decision(0, W5Action::New, &[], &["s-310"], None)],
    );
    let new = committed(commit_group(&mut db.open(), &claim(&g), &g).unwrap()).products[&0usize];
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
        &db,
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
    let receipt = committed(commit_group(&mut db.open(), &claim(&g), &g).unwrap());
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
        &db,
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
    committed(commit_group(&mut db.open(), &claim(&g), &g).unwrap());
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
        let result = commit_group_with_hook(&mut db.open(), &claim(&g), &g, &mut fail_at(step));
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
        &db,
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
        commit_group_with_hook(
            &mut db.open(),
            &claim(&g),
            &g,
            &mut fail_at(W5Step::AfterDecision(0))
        )
        .is_err()
    );
    assert_eq!(snapshot(&db), before);
    assert!(
        commit_group_with_hook(
            &mut db.open(),
            &claim(&g),
            &g,
            &mut fail_at(W5Step::AfterDecision(1))
        )
        .is_err()
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
        &db,
        "s-310#0",
        vec![(
            0,
            cand("s-310", "works_at", "字节跳动", "我在字节跳动工作。", 200),
        )],
        vec![decision(0, W5Action::New, &[other], &["s-310"], None)],
    );
    let before = snapshot(&db);
    assert!(commit_group(&mut db.open(), &claim(&g), &g).is_err());
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
        &db,
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
    assert!(commit_group(&mut db.open(), &claim(&g), &g).is_err());
    assert_eq!(snapshot(&db), before);
}

// ---------------- A3 重复重试（幂等）

#[test]
fn a3_replaying_a_committed_group_changes_nothing() {
    let db = TestDb::new();
    let (_, g, _) = w4_01(&db);
    let receipt = committed(commit_group(&mut db.open(), &claim(&g), &g).unwrap());
    let after_first = snapshot(&db);
    match commit_group(&mut db.open(), &claim(&g), &g).unwrap() {
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
        &db,
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
    commit_group(&mut db.open(), &claim(&g), &g).unwrap();
    commit_group(&mut db.open(), &claim(&g), &g).unwrap();
    // 同一来源放进另一个组 ID 也不再计数（证据只计一次）。
    let mut again = g.clone();
    again.id = "s-320#retry".into();
    commit_group(&mut db.open(), &claim(&again), &again).unwrap();
    let m = memory(&db, target);
    assert_eq!(m.reinforcement_count, 1);
    assert_eq!(m.evidence.len(), 2);
}

#[test]
fn a3_retry_after_fault_applies_exactly_once() {
    let db = TestDb::new();
    let (olds, g) = w4_12(&db);
    let before = count(&db);
    assert!(
        commit_group_with_hook(
            &mut db.open(),
            &claim(&g),
            &g,
            &mut fail_at(W5Step::BeforeSave)
        )
        .is_err()
    );
    let new = committed(commit_group(&mut db.open(), &claim(&g), &g).unwrap()).products[&0usize];
    assert!(matches!(
        commit_group(&mut db.open(), &claim(&g), &g).unwrap(),
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
    commit_group(&mut db.open(), &claim(&g), &g).unwrap();
    let after_first = snapshot(&db);
    let mut changed = g.clone();
    changed.decisions[0].basis_sources.push("s-999".into());
    assert!(commit_group(&mut db.open(), &claim(&changed), &changed).is_err());
    assert_eq!(snapshot(&db), after_first);
}

#[test]
fn a3_receipts_live_in_a_version_older_binaries_reject() {
    // 引入 W5 回执前的程序只接受版本 1..=5，重写快照时会丢掉 w5_receipts，
    // 随后重放已提交的组会再插入一次；数据库因此升到 6，让旧程序拒绝打开。
    let db = TestDb::new();
    let (_, g, _) = w4_01(&db);
    commit_group(&mut db.open(), &claim(&g), &g).unwrap();
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

// ---------------- 第二段：A4–A7（快照过期、来源删除、任务认领），以及 D2/D3（同一原文分组）

fn source(db: &TestDb, key: &str) -> SourceRecord {
    db.open().read().unwrap().sources[key].clone()
}

/// 只改原文任务字段（非业务数据，不推进 revision）。
fn edit_source(db: &TestDb, key: &str, f: impl FnOnce(&mut SourceRecord)) {
    db.open()
        .transaction(|state| {
            f(state.sources.get_mut(key).unwrap());
            Ok(())
        })
        .unwrap()
}

fn needs_rejudge(outcome: W5Outcome) -> String {
    match outcome {
        W5Outcome::NeedsRejudge(W5Receipt {
            status: W5Status::NeedsRejudge { reason },
            products,
            ..
        }) => {
            assert!(products.is_empty());
            reason
        }
        other => panic!("expected NeedsRejudge, got {other:?}"),
    }
}

/// 一条较早接收的原文（先建任务、序号更小）先提交，强化目标 `target`：
/// 这是不触发全局顺序闸门、只能靠目标快照发现的并发变更。
fn older_source_reinforces(db: &TestDb, target: MemoryId) -> W5Group {
    group(
        db,
        "s-050#0",
        vec![(0, cand("s-050", "lives_in", "北京", "我还住在北京。", 150))],
        vec![decision(
            0,
            W5Action::Reinforce,
            &[target],
            &["s-050"],
            None,
        )],
    )
}

#[test]
fn a4_target_changed_by_an_older_source_needs_rejudge() {
    let db = TestDb::new();
    let old = seed(
        &db,
        &[cand("s-101", "lives_in", "北京", "我住在北京。", 100)],
    )[0];
    let earlier = older_source_reinforces(&db, old);
    let g = group(
        &db,
        "s-205#0",
        vec![(0, cand("s-205", "lives_in", "上海", "我搬到上海了。", 200))],
        vec![decision(
            0,
            W5Action::StateChange,
            &[old],
            &["s-101", "s-205"],
            None,
        )],
    );
    committed(commit_group(&mut db.open(), &claim(&earlier), &earlier).unwrap());
    let before = snapshot(&db);

    let reason = needs_rejudge(commit_group(&mut db.open(), &claim(&g), &g).unwrap());
    assert!(reason.contains("changed since W4 input"), "{reason}");
    let after = snapshot(&db);
    assert_eq!((&after.0, after.1), (&before.0, before.1), "零业务写入");
    assert_eq!(memory(&db, old).status, MemoryStatus::Active);

    // 不能重试旧决定：同一组再提交只返回 needs_rejudge。
    needs_rejudge(commit_group(&mut db.open(), &claim(&g), &g).unwrap());
    assert_eq!(snapshot(&db), after);
}

#[test]
fn a4_one_stale_target_blocks_the_whole_multi_target_group() {
    let db = TestDb::new();
    let olds = seed(
        &db,
        &[
            cand("s-302", "lives_in", "北京", "我住在北京。", 100),
            cand("s-303", "lives_in", "北京朝阳区", "我家在北京朝阳区。", 110),
        ],
    );
    let earlier = older_source_reinforces(&db, olds[1]);
    let g = group(
        &db,
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
    committed(commit_group(&mut db.open(), &claim(&earlier), &earlier).unwrap());
    let records = count(&db);
    needs_rejudge(commit_group(&mut db.open(), &claim(&g), &g).unwrap());
    assert_eq!(count(&db), records);
    for old in olds {
        assert_eq!(memory(&db, old).status, MemoryStatus::Active);
    }
}

#[test]
fn a4_source_less_change_marks_the_task_stale() {
    // 不经任何原文的直接改动推进全局屏障，现有事务把运行中的任务标为 stale_requires_review；
    // W5 随后得到 needs_rejudge，零业务写入。
    let db = TestDb::new();
    let (old, g, _) = w4_01(&db);
    db.open()
        .transaction(|state| {
            let mut m = state.repository.get(old)?.unwrap();
            m.tags.push("edited".into());
            state.repository.update(m)?;
            Ok(())
        })
        .unwrap();
    assert_eq!(source(&db, "s-205").enrichment, "stale_requires_review");
    let before = snapshot(&db);
    needs_rejudge(commit_group(&mut db.open(), &claim(&g), &g).unwrap());
    let after = snapshot(&db);
    assert_eq!((&after.0, after.1), (&before.0, before.1));
}

#[test]
fn a5_deleted_basis_source_needs_rejudge() {
    let db = TestDb::new();
    let (_, g, _) = w4_01(&db);
    // s-101（旧记录的依据）作为已删除的原文存在。
    ensure_claimed_source(&db, "s-101");
    edit_source(&db, "s-101", |s| {
        s.request = None;
        s.enrichment = "deleted".into();
        s.task = EnrichmentTask::default();
    });
    let before = snapshot(&db);
    let reason = needs_rejudge(commit_group(&mut db.open(), &claim(&g), &g).unwrap());
    assert!(reason.contains("s-101"), "{reason}");
    let after = snapshot(&db);
    assert_eq!((&after.0, after.1), (&before.0, before.1));
}

#[test]
fn a5_deleted_own_source_loses_the_claim() {
    let db = TestDb::new();
    let (_, g, _) = w4_01(&db);
    edit_source(&db, "s-205", |s| {
        s.request = None;
        s.enrichment = "deleted".into();
    });
    let before = snapshot(&db);
    let error = commit_group(&mut db.open(), &claim(&g), &g).unwrap_err();
    assert!(error.to_string().contains("claim_lost"), "{error}");
    assert_eq!(snapshot(&db), before);
}

#[test]
fn a6_late_commit_after_reclaim_is_rejected_and_keeps_the_new_claim() {
    let db = TestDb::new();
    let (old, g, _) = w4_01(&db);
    // 甲（attempt-1）的租约过期，乙重新认领。
    edit_source(&db, "s-205", |s| {
        s.task.attempt_id = Some("attempt-2".into());
        s.task.attempt_count = 2;
        s.task.lease_until = Some(now() + 900);
    });
    let task_before = source(&db, "s-205").task;
    let before = snapshot(&db);
    let error = commit_group(&mut db.open(), &claim(&g), &g).unwrap_err();
    assert!(error.to_string().contains("claim_lost"), "{error}");
    assert_eq!(snapshot(&db), before, "甲零写入");
    assert_eq!(source(&db, "s-205").task, task_before, "不改写乙的认领");
    assert!(w5_finish_task(&mut db.open(), &claim(&g), &W5TaskEnd::Committed).is_err());
    assert_eq!(source(&db, "s-205").enrichment, "running");

    // 乙正常提交。
    let second = W5Claim {
        source_key: "s-205".into(),
        attempt_id: "attempt-2".into(),
    };
    committed(commit_group(&mut db.open(), &second, &g).unwrap());
    assert_eq!(memory(&db, old).status, MemoryStatus::Superseded);
}

#[test]
fn a6_expired_lease_is_rejected_even_without_reclaim() {
    let db = TestDb::new();
    let (_, g, _) = w4_01(&db);
    edit_source(&db, "s-205", |s| s.task.lease_until = Some(now() - 1));
    let before = snapshot(&db);
    let error = commit_group(&mut db.open(), &claim(&g), &g).unwrap_err();
    assert!(error.to_string().contains("claim_lost"), "{error}");
    assert_eq!(snapshot(&db), before);
}

#[test]
fn a7_ordinary_failure_marks_failed_and_releases_the_claim() {
    let db = TestDb::new();
    let (olds, g) = w4_12(&db);
    assert!(
        commit_group_with_hook(
            &mut db.open(),
            &claim(&g),
            &g,
            &mut fail_at(W5Step::BeforeSave)
        )
        .is_err()
    );
    w5_finish_task(
        &mut db.open(),
        &claim(&g),
        &W5TaskEnd::Failed("injected fault".into()),
    )
    .unwrap();
    let s = source(&db, "s-340");
    assert_eq!(s.enrichment, "failed");
    assert_eq!(s.error.as_deref(), Some("injected fault"));
    assert_eq!((s.task.attempt_id, s.task.lease_until), (None, None));
    // 认领已释放：旧执行者不能再提交。
    assert!(commit_group(&mut db.open(), &claim(&g), &g).is_err());
    // 重新认领后重试，恰好生效一次。
    edit_source(&db, "s-340", |s| {
        s.enrichment = "running".into();
        s.task.attempt_id = Some("attempt-2".into());
        s.task.lease_until = Some(now() + 900);
    });
    let retry = W5Claim {
        source_key: "s-340".into(),
        attempt_id: "attempt-2".into(),
    };
    let records = count(&db);
    let new = committed(commit_group(&mut db.open(), &retry, &g).unwrap()).products[&0usize];
    assert_eq!(count(&db), records + 1);
    for old in olds {
        assert_eq!(memory(&db, old).superseded_by, Some(new));
    }
}

#[test]
fn a7_needs_rejudge_ends_the_task_and_the_old_decision_stays_unusable() {
    let db = TestDb::new();
    let old = seed(
        &db,
        &[cand("s-101", "lives_in", "北京", "我住在北京。", 100)],
    )[0];
    let earlier = older_source_reinforces(&db, old);
    let g = group(
        &db,
        "s-205#0",
        vec![(0, cand("s-205", "lives_in", "上海", "我搬到上海了。", 200))],
        vec![decision(
            0,
            W5Action::StateChange,
            &[old],
            &["s-101", "s-205"],
            None,
        )],
    );
    committed(commit_group(&mut db.open(), &claim(&earlier), &earlier).unwrap());
    let reason = needs_rejudge(commit_group(&mut db.open(), &claim(&g), &g).unwrap());
    w5_finish_task(&mut db.open(), &claim(&g), &W5TaskEnd::NeedsRejudge(reason)).unwrap();
    let s = source(&db, "s-205");
    assert_eq!(s.enrichment, "stale_requires_review");
    assert!(s.error.unwrap().starts_with("w5_needs_rejudge"));
    assert_eq!(s.task.attempt_id, None);
    // 即使重新认领，同一组旧决定也只返回 needs_rejudge，不会执行。
    edit_source(&db, "s-205", |s| {
        s.enrichment = "running".into();
        s.task.attempt_id = Some("attempt-2".into());
        s.task.lease_until = Some(now() + 900);
    });
    let again = W5Claim {
        source_key: "s-205".into(),
        attempt_id: "attempt-2".into(),
    };
    let before = snapshot(&db);
    needs_rejudge(commit_group(&mut db.open(), &again, &g).unwrap());
    assert_eq!(snapshot(&db), before);
    assert_eq!(memory(&db, old).status, MemoryStatus::Active);
}

/// D2/D3：同一句“我已经搬到深圳了，现在在腾讯工作。”拆成两组，共用一个认领。
fn same_source_two_groups(db: &TestDb) -> (Vec<MemoryId>, W5Group, W5Group) {
    let text = "我已经搬到深圳了，现在在腾讯工作。";
    let olds = seed(
        db,
        &[
            cand("s-501", "lives_in", "北京", "我住在北京。", 100),
            cand("s-502", "lives_in", "上海", "我住在上海。", 110),
        ],
    );
    let g0 = group(
        db,
        "s-510#0",
        vec![(0, cand("s-510", "lives_in", "深圳", text, 200))],
        vec![decision(
            0,
            W5Action::StateChange,
            &olds,
            &["s-501", "s-502", "s-510"],
            None,
        )],
    );
    let g1 = group(
        db,
        "s-510#1",
        vec![(1, cand("s-510", "works_at", "腾讯", text, 200))],
        vec![decision(1, W5Action::New, &[], &["s-510"], None)],
    );
    (olds, g0, g1)
}

#[test]
fn d3_two_groups_of_one_source_commit_in_turn_under_one_claim() {
    let db = TestDb::new();
    let (olds, g0, g1) = same_source_two_groups(&db);
    let records = count(&db);
    committed(commit_group(&mut db.open(), &claim(&g0), &g0).unwrap());
    // 第一组推进了 revision；第二组仍在同一认领下正确提交，任务未被自己标为过期。
    assert_eq!(source(&db, "s-510").enrichment, "running");
    committed(commit_group(&mut db.open(), &claim(&g1), &g1).unwrap());
    assert_eq!(count(&db), records + 2);
    assert_eq!(source(&db, "s-510").enrichment, "running");
    for old in olds {
        assert_eq!(memory(&db, old).status, MemoryStatus::Superseded);
    }
    w5_finish_task(&mut db.open(), &claim(&g0), &W5TaskEnd::Committed).unwrap();
    assert_eq!(source(&db, "s-510").enrichment, "committed");
}

#[test]
fn d2_one_group_needs_rejudge_while_the_other_commits_once() {
    let db = TestDb::new();
    // 较早的原文先建任务，之后它会改动 g0 的一个目标。
    let olds_pre = seed(
        &db,
        &[
            cand("s-501", "lives_in", "北京", "我住在北京。", 100),
            cand("s-502", "lives_in", "上海", "我住在上海。", 110),
        ],
    );
    let earlier = older_source_reinforces(&db, olds_pre[0]);
    let text = "我已经搬到深圳了，现在在腾讯工作。";
    let g0 = group(
        &db,
        "s-510#0",
        vec![(0, cand("s-510", "lives_in", "深圳", text, 200))],
        vec![decision(
            0,
            W5Action::StateChange,
            &olds_pre,
            &["s-501", "s-502", "s-510"],
            None,
        )],
    );
    let g1 = group(
        &db,
        "s-510#1",
        vec![(1, cand("s-510", "works_at", "腾讯", text, 200))],
        vec![decision(1, W5Action::New, &[], &["s-510"], None)],
    );
    committed(commit_group(&mut db.open(), &claim(&earlier), &earlier).unwrap());
    let records = count(&db);
    needs_rejudge(commit_group(&mut db.open(), &claim(&g0), &g0).unwrap());
    committed(commit_group(&mut db.open(), &claim(&g1), &g1).unwrap());
    assert!(matches!(
        commit_group(&mut db.open(), &claim(&g1), &g1).unwrap(),
        W5Outcome::AlreadyApplied(_)
    ));
    assert_eq!(count(&db), records + 1, "D2：只新增候选 1 的一条");
    for old in olds_pre {
        assert_eq!(memory(&db, old).status, MemoryStatus::Active);
    }
}
