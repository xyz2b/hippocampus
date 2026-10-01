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
        resolves_relations: vec![],
        resolution: None,
        relation: None,
        pending_reason: None,
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
    let mut targets: Vec<MemoryId> = decisions.iter().flat_map(|d| d.targets.clone()).collect();
    targets.sort();
    targets.dedup();
    let resolves: Vec<String> = decisions
        .iter()
        .flat_map(|d| d.resolves_relations.clone())
        .collect();
    let (seen, seen_relations) =
        w5_snapshot(&db.open().read().unwrap(), &targets, &resolves).unwrap();
    W5Group {
        id: id.into(),
        source_key,
        candidates: candidates
            .into_iter()
            .map(|(index, candidate)| W5Candidate {
                index,
                candidate,
                committed_id: None,
            })
            .collect(),
        decisions,
        seen,
        scope_complete: false,
        seen_relations,
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
            (1, {
                // 同一原文拆出的第二个候选：同一轮，证据 ID 不同。
                let mut c = cand(
                    "s-400",
                    "works_at",
                    "腾讯",
                    "游泳那句收回，我现在在腾讯。",
                    200,
                );
                c.evidence[0].id = EvidenceId::from("s-400-b");
                c
            }),
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
    // 随后重放已提交的组会再插入一次；回执加入 status 后再升到 7，让 v6 程序也拒绝打开；
    // 加入关系与更新状态后升到 8，让 v7 程序也拒绝打开。
    let db = TestDb::new();
    let (_, g, _) = w4_01(&db);
    commit_group(&mut db.open(), &claim(&g), &g).unwrap();
    let connection = rusqlite::Connection::open(&db.0).unwrap();
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 8);
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

// ---------------- 第二段复审修正

/// 把已提交的回执改写成 bf83fd9（版本 6）真实保存的样子：没有 status、没有
/// legacy_fingerprint，指纹是当时的组序列化（没有 source_key、seen）。
/// 去掉版本 8 才有的字段（快照的关系、更新状态，回执的 updates、merged、form）。真实旧数据
/// 见 `tests/fixtures/w5_storage/`；这里只用于在新程序写的数据上构造个别旧形态。
fn strip_v8_fields(state: &mut serde_json::Value) {
    let state = state.as_object_mut().unwrap();
    for key in ["relations", "next_relation", "record_updates"] {
        state.remove(key);
    }
    for receipt in state["w5_receipts"].as_object_mut().unwrap().values_mut() {
        let receipt = receipt.as_object_mut().unwrap();
        for key in ["updates", "merged", "form"] {
            receipt.remove(key);
        }
    }
}

fn rewrite_as_v6_receipt(db: &TestDb, g: &W5Group) {
    #[derive(serde::Serialize)]
    struct V6Group<'a> {
        id: &'a str,
        candidates: &'a [W5Candidate],
        decisions: &'a [W5Decision],
    }
    let v6_fingerprint = serde_json::to_string(&V6Group {
        id: &g.id,
        candidates: &g.candidates,
        decisions: &g.decisions,
    })
    .unwrap();
    let connection = rusqlite::Connection::open(&db.0).unwrap();
    let raw: String = connection
        .query_row("SELECT state_json FROM memory_profiles", [], |row| {
            row.get(0)
        })
        .unwrap();
    let mut old: serde_json::Value = serde_json::from_str(&raw).unwrap();
    strip_v8_fields(&mut old);
    let receipt = old["w5_receipts"][g.id.as_str()].as_object_mut().unwrap();
    receipt.remove("status");
    receipt.remove("legacy_fingerprint");
    receipt.insert("fingerprint".into(), v6_fingerprint.into());
    connection
        .execute(
            "UPDATE memory_profiles SET state_json=?1",
            [old.to_string()],
        )
        .unwrap();
    connection.pragma_update(None, "user_version", 6).unwrap();
}

#[test]
fn a3_v6_receipts_without_status_still_open_and_replay() {
    let db = TestDb::new();
    let (_, g, _) = w4_01(&db);
    commit_group(&mut db.open(), &claim(&g), &g).unwrap();
    rewrite_as_v6_receipt(&db, &g);
    let before = snapshot(&db);

    let receipt = db.open().read().unwrap().w5_receipts["s-205#0"].clone();
    assert_eq!(receipt.status, W5Status::Committed);
    assert!(receipt.legacy_fingerprint);
    let connection = rusqlite::Connection::open(&db.0).unwrap();
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 8);
    let raw: String = connection
        .query_row("SELECT state_json FROM memory_profiles", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(
        raw.contains("\"status\":\"Committed\""),
        "迁移后显式写出 status"
    );
    assert!(raw.contains("\"legacy_fingerprint\":true"));
    // 用当前格式的组（带 source_key、seen）重放旧回执：识别为同一组，不再执行。
    assert!(matches!(
        commit_group(&mut db.open(), &claim(&g), &g).unwrap(),
        W5Outcome::AlreadyApplied(_)
    ));
    assert_eq!(snapshot(&db), before);
    // 内容不同的组仍被拒绝。
    let mut changed = g.clone();
    changed.decisions[0].basis_sources.push("s-999".into());
    assert!(commit_group(&mut db.open(), &claim(&changed), &changed).is_err());
    assert_eq!(snapshot(&db), before);
}

/// 另一个连接持有写锁 `hold`，期间在主线程执行 `f`。
fn while_write_locked<T>(db: &TestDb, hold: std::time::Duration, f: impl FnOnce() -> T) -> T {
    let path = db.0.clone();
    let (locked, wait) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let mut connection = rusqlite::Connection::open(path).unwrap();
        let tx = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        locked.send(()).unwrap();
        std::thread::sleep(hold);
        tx.rollback().unwrap();
    });
    wait.recv().unwrap();
    let result = f();
    holder.join().unwrap();
    result
}

#[test]
fn a6_lease_expiring_while_waiting_for_the_write_lock_is_rejected() {
    let db = TestDb::new();
    let (_, g, _) = w4_01(&db);
    edit_source(&db, "s-205", |s| s.task.lease_until = Some(now() + 1));
    let before = snapshot(&db);
    let mut store = db.open();
    let result = while_write_locked(&db, std::time::Duration::from_millis(2500), || {
        commit_group(&mut store, &claim(&g), &g)
    });
    let error = result.unwrap_err();
    assert!(error.to_string().contains("claim_lost"), "{error}");
    assert_eq!(snapshot(&db), before);

    // 收尾入口同样在写锁内取时间。
    edit_source(&db, "s-205", |s| s.task.lease_until = Some(now() + 1));
    let task = source(&db, "s-205").task;
    let mut store = db.open();
    let result = while_write_locked(&db, std::time::Duration::from_millis(2500), || {
        w5_finish_task(&mut store, &claim(&g), &W5TaskEnd::Committed)
    });
    assert!(result.unwrap_err().to_string().contains("claim_lost"));
    assert_eq!(source(&db, "s-205").enrichment, "running");
    assert_eq!(source(&db, "s-205").task, task);
}

#[test]
fn a6_candidate_from_another_source_is_rejected() {
    // 用原文 A（s-205）的有效认领，携带已删除原文 B（s-300）的候选，依据只填 A。
    let db = TestDb::new();
    ensure_claimed_source(&db, "s-300");
    edit_source(&db, "s-300", |s| {
        s.request = None;
        s.enrichment = "deleted".into();
        s.task = EnrichmentTask::default();
    });
    let g = group(
        &db,
        "s-205#0",
        vec![(0, cand("s-300", "works_at", "腾讯", "我在腾讯工作。", 200))],
        vec![decision(0, W5Action::New, &[], &["s-205"], None)],
    );
    let before = snapshot(&db);
    let error = commit_group(&mut db.open(), &claim(&g), &g).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("does not come from the claimed source"),
        "{error}"
    );
    assert_eq!(snapshot(&db), before);
}

/// 把回执改写成 `flag` 字段出现之前、baa1ff9/708b2d2 写下的样子：有 status、当前形态的
/// 指纹，但没有 legacy_fingerprint；数据库版本为 `version`。
fn rewrite_as_unflagged_receipt(db: &TestDb, group_id: &str, version: i64) {
    let connection = rusqlite::Connection::open(&db.0).unwrap();
    let raw: String = connection
        .query_row("SELECT state_json FROM memory_profiles", [], |row| {
            row.get(0)
        })
        .unwrap();
    let mut old: serde_json::Value = serde_json::from_str(&raw).unwrap();
    strip_v8_fields(&mut old);
    old["w5_receipts"][group_id]
        .as_object_mut()
        .unwrap()
        .remove("legacy_fingerprint");
    connection
        .execute(
            "UPDATE memory_profiles SET state_json=?1",
            [old.to_string()],
        )
        .unwrap();
    connection
        .pragma_update(None, "user_version", version)
        .unwrap();
}

#[test]
fn a3_receipts_from_intermediate_versions_still_replay() {
    // baa1ff9 写的是版本 6，708b2d2 写的是版本 7；两者都有 status、当前形态的指纹，
    // 但都没有 legacy_fingerprint。
    for version in [6, 7] {
        let db = TestDb::new();
        let (_, g, _) = w4_01(&db);
        commit_group(&mut db.open(), &claim(&g), &g).unwrap();
        rewrite_as_unflagged_receipt(&db, "s-205#0", version);
        let before = snapshot(&db);
        assert!(
            matches!(
                commit_group(&mut db.open(), &claim(&g), &g).unwrap(),
                W5Outcome::AlreadyApplied(_)
            ),
            "version {version}"
        );
        assert_eq!(snapshot(&db), before, "version {version}");
        let mut changed = g.clone();
        changed.decisions[0].basis_sources.push("s-999".into());
        assert!(
            commit_group(&mut db.open(), &claim(&changed), &changed).is_err(),
            "version {version}"
        );
        assert_eq!(snapshot(&db), before, "version {version}");
    }
}

#[test]
fn a3_needs_rejudge_receipt_from_an_intermediate_version_stays_needs_rejudge() {
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
    needs_rejudge(commit_group(&mut db.open(), &claim(&g), &g).unwrap());
    rewrite_as_unflagged_receipt(&db, "s-205#0", 7);
    let before = snapshot(&db);
    needs_rejudge(commit_group(&mut db.open(), &claim(&g), &g).unwrap());
    assert_eq!(snapshot(&db), before);
    assert_eq!(memory(&db, old).status, MemoryStatus::Active);
}

#[test]
fn a3_new_receipts_accept_only_the_current_fingerprint() {
    // 新回执写明 legacy_fingerprint=false：只与当前形态比对，旧形态相同也不算同一组。
    let db = TestDb::new();
    let (_, g, _) = w4_01(&db);
    let receipt = committed(commit_group(&mut db.open(), &claim(&g), &g).unwrap());
    assert!(!receipt.legacy_fingerprint);
    let mut other_snapshot = g.clone();
    other_snapshot.seen[0].fingerprint.push(' ');
    let before = snapshot(&db);
    assert!(commit_group(&mut db.open(), &claim(&other_snapshot), &other_snapshot).is_err());
    assert_eq!(snapshot(&db), before);
}

// ---------------- 存储第 1 段（docs/architecture/w5-storage.md）：版本 8、真实旧数据迁移与重放

/// 把 `tests/fixtures/w5_storage/<name>.sql` 还原成数据库文件。这些脚本由对应提交自己的
/// 程序写出的数据库导出（模式 + 行 + user_version），不是从新格式删字段近似出来的。
fn restore_fixture(name: &str) -> TestDb {
    let db = TestDb::new();
    std::fs::create_dir_all(db.0.parent().unwrap()).unwrap();
    let script = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/w5_storage")
            .join(format!("{name}.sql")),
    )
    .unwrap();
    rusqlite::Connection::open(&db.0)
        .unwrap()
        .execute_batch(&script)
        .unwrap();
    db
}

fn user_version(db: &TestDb) -> i64 {
    rusqlite::Connection::open(&db.0)
        .unwrap()
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap()
}

fn raw_state(db: &TestDb) -> serde_json::Value {
    let raw: String = rusqlite::Connection::open(&db.0)
        .unwrap()
        .query_row("SELECT state_json FROM memory_profiles", [], |row| {
            row.get(0)
        })
        .unwrap();
    serde_json::from_str(&raw).unwrap()
}

/// 旧数据里的认领早已过期：给原文任务一个新的有效认领（不是业务数据，不推进 revision）。
fn reclaim(db: &TestDb, key: &str) {
    ensure_claimed_source(db, key);
    edit_source(db, key, |s| {
        s.enrichment = "running".into();
        s.task.attempt_id = Some(ATTEMPT.into());
        s.task.lease_until = Some(now() + 900);
    });
}

/// 旧程序在同一场景下提交的组：在新程序的空库上按相同步骤构造（场景是确定的）。
fn w4_01_group() -> W5Group {
    w4_01(&TestDb::new()).1
}

fn rejudge_scenario(db: &TestDb) -> (MemoryId, W5Group, W5Group) {
    let old = seed(
        db,
        &[cand("s-101", "lives_in", "北京", "我住在北京。", 100)],
    )[0];
    let earlier = older_source_reinforces(db, old);
    let g = group(
        db,
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
    (old, earlier, g)
}

fn assert_pre8_receipts(db: &TestDb, label: &str) {
    let state = raw_state(db);
    let receipts = state["w5_receipts"].as_object().unwrap();
    assert!(!receipts.is_empty(), "{label}");
    for (id, receipt) in receipts {
        assert_eq!(receipt["form"], "pre8", "{label} {id}");
        assert_eq!(receipt["updates"], serde_json::json!({}), "{label} {id}");
        assert_eq!(receipt["merged"], serde_json::json!({}), "{label} {id}");
    }
    assert_eq!(state["relations"], serde_json::json!({}), "{label}");
    assert_eq!(state["record_updates"], serde_json::json!({}), "{label}");
    assert_eq!(state["next_relation"], 0, "{label}");
}

#[test]
fn s1_committed_receipts_of_every_released_version_migrate_and_replay() {
    let g = w4_01_group();
    for (name, version) in [
        ("v6-bf83fd9", 6),
        ("v6-baa1ff9", 6),
        ("v7-708b2d2", 7),
        ("v7-ca16b64", 7),
    ] {
        let db = restore_fixture(name);
        assert_eq!(user_version(&db), version, "{name}");
        // 夹具里的组确实是这个场景的组（决定原样保存在回执里）。
        let stored: Vec<W5Decision> =
            serde_json::from_value(raw_state(&db)["w5_receipts"]["s-205#0"]["decisions"].clone())
                .unwrap();
        assert_eq!(stored, g.decisions, "{name}");

        // 迁移：版本 8，回执显式写出 form=pre8，快照补齐新字段。
        drop(db.open());
        assert_eq!(user_version(&db), 8, "{name}");
        assert_pre8_receipts(&db, name);
        // 关闭 → 重开 → 再保存一次（任务字段写入也会改写快照）→ 重开：标记仍在。
        reclaim(&db, "s-205");
        assert_pre8_receipts(&db, name);
        let receipt = db.open().read().unwrap().w5_receipts["s-205#0"].clone();
        assert_eq!(receipt.form, ReceiptForm::Pre8, "{name}");
        assert_eq!(receipt.status, W5Status::Committed, "{name}");

        // 重放同一组：识别为已提交，零变更。
        let before = snapshot(&db);
        assert!(
            matches!(
                commit_group(&mut db.open(), &claim(&g), &g).unwrap(),
                W5Outcome::AlreadyApplied(_)
            ),
            "{name}"
        );
        assert_eq!(snapshot(&db), before, "{name}");
        // 内容不同仍被拒绝；带上版本 8 新字段（scope_complete、关系快照）的同一组按旧投影比对。
        let mut changed = g.clone();
        changed.decisions[0].basis_sources.push("s-999".into());
        assert!(
            commit_group(&mut db.open(), &claim(&changed), &changed).is_err(),
            "{name}"
        );
        let mut widened = g.clone();
        widened.scope_complete = true;
        widened.seen[0].relations.push("rel-0".into());
        widened.seen_relations.push(W5SeenRelation {
            id: "rel-0".into(),
            fingerprint: "{}".into(),
        });
        assert!(
            matches!(
                commit_group(&mut db.open(), &claim(&widened), &widened).unwrap(),
                W5Outcome::AlreadyApplied(_)
            ),
            "{name}"
        );
        assert_eq!(snapshot(&db), before, "{name}");
    }
}

#[test]
fn s1_needs_rejudge_receipts_of_released_versions_stay_needs_rejudge() {
    let (_, _, g) = rejudge_scenario(&TestDb::new());
    for name in [
        "v6-baa1ff9-rejudge",
        "v7-708b2d2-rejudge",
        "v7-ca16b64-rejudge",
    ] {
        let db = restore_fixture(name);
        drop(db.open());
        assert_eq!(user_version(&db), 8, "{name}");
        assert_pre8_receipts(&db, name);
        reclaim(&db, "s-205");
        let before = snapshot(&db);
        needs_rejudge(commit_group(&mut db.open(), &claim(&g), &g).unwrap());
        assert_eq!(snapshot(&db), before, "{name}");
        assert_eq!(
            memory(&db, MemoryId(1)).status,
            MemoryStatus::Active,
            "{name}"
        );
    }
}

#[test]
fn s1_v5_database_migrates_and_new_receipts_are_v8() {
    let db = restore_fixture("v5");
    assert_eq!(user_version(&db), 5);
    drop(db.open());
    assert_eq!(user_version(&db), 8);
    reclaim(&db, "s-205");
    let old = MemoryId(1);
    assert_eq!(memory(&db, old).status, MemoryStatus::Active);
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
    let receipt = committed(commit_group(&mut db.open(), &claim(&g), &g).unwrap());
    assert_eq!(receipt.form, ReceiptForm::V8);
    assert!(receipt.merged.is_empty());
    assert_eq!(receipt.updates.len(), 1);
    assert_eq!(raw_state(&db)["w5_receipts"]["s-205#0"]["form"], "v8");
    // v8 回执只认当前形态：带新字段的组是另一组内容。
    let mut widened = g.clone();
    widened.scope_complete = true;
    assert!(commit_group(&mut db.open(), &claim(&widened), &widened).is_err());
    let before = snapshot(&db);
    assert!(matches!(
        commit_group(&mut db.open(), &claim(&g), &g).unwrap(),
        W5Outcome::AlreadyApplied(_)
    ));
    assert_eq!(snapshot(&db), before);
}

#[test]
fn s1_relations_and_update_states_are_business_data() {
    let db = TestDb::new();
    ensure_claimed_source(&db, "s-205");
    let revision = db.open().read().unwrap().revision;
    db.open()
        .transaction(|state| {
            state.relations.insert(
                "rel-0".into(),
                StoredRelation {
                    id: "rel-0".into(),
                    kind: RelationKind::Conflict,
                    members: vec![MemoryId(1), MemoryId(2)],
                    conditions: serde_json::json!([]),
                    times: serde_json::json!([]),
                    basis: vec![],
                    status: RelationStatus::Open,
                    resolution: None,
                    created_by: "test".into(),
                    closed_by: None,
                },
            );
            state.next_relation = 1;
            Ok(())
        })
        .unwrap();
    let state = db.open().read().unwrap();
    assert_eq!(state.revision, revision + 1);
    // 无来源的业务变更推进顺序闸门：正在整理的较早原文须重判。
    assert_eq!(state.sources["s-205"].enrichment, "stale_requires_review");
    assert_eq!(state.relations["rel-0"].status, RelationStatus::Open);
    assert_eq!(state.next_relation, 1);

    let pending = UpdateState {
        status: UpdateStatus::Pending,
        reasons: vec![UpdateReason::UpdateScopeIncomplete],
    };
    db.open()
        .transaction(|state| {
            state.record_updates.insert(MemoryId(7), pending.clone());
            Ok(())
        })
        .unwrap();
    let state = db.open().read().unwrap();
    assert_eq!(state.revision, revision + 2);
    assert_eq!(state.record_updates[&MemoryId(7)], pending);
    // 按契约的字面形式保存。
    let raw = raw_state(&db);
    assert_eq!(
        raw["record_updates"]["7"],
        serde_json::json!({"status":"pending","reasons":["update_scope_incomplete"]})
    );
    assert_eq!(raw["relations"]["rel-0"]["status"], "open");
    assert_eq!(raw["relations"]["rel-0"]["kind"], "conflict");
}

#[test]
fn s1_groups_using_later_segment_fields_are_rejected_without_writes() {
    let db = TestDb::new();
    let (_, g, _) = w4_01(&db);
    let before = snapshot(&db);
    let mut conflict = g.clone();
    conflict.decisions[0].action = W5Action::Conflict;
    let mut resolving = g.clone();
    resolving.decisions[0]
        .resolves_relations
        .push("rel-0".into());
    let mut reused = g.clone();
    reused.candidates[0].committed_id = Some(MemoryId(1));
    let mut related = g.clone();
    related.seen[0].relations.push("rel-0".into());
    // committed_id 留到第 4 段；其余字段第 3 段已执行，但缺少配套内容时同样拒绝、零写入。
    for (label, group, expected) in [
        (
            "conflict without relation",
            conflict,
            "exactly a conflict carries",
        ),
        ("resolves without resolution", resolving, "go together"),
        ("committed_id", reused, "not supported yet"),
        (
            "seen relations without snapshot",
            related,
            "has no W4 snapshot",
        ),
    ] {
        let error = commit_group(&mut db.open(), &claim(&group), &group).unwrap_err();
        assert!(error.to_string().contains(expected), "{label}: {error}");
        assert_eq!(snapshot(&db), before, "{label}");
    }
}

#[test]
fn s1_unknown_fields_fail_closed_instead_of_being_dropped() {
    // 以后的程序若给 W5 存储加字段，当前程序不能读入后在改写快照时悄悄丢掉它；
    // 嵌套类型（回执状态、决定、更新状态、关系依据）同样适用。
    let db = TestDb::new();
    let (_, earlier, g) = rejudge_scenario(&db);
    committed(commit_group(&mut db.open(), &claim(&earlier), &earlier).unwrap());
    needs_rejudge(commit_group(&mut db.open(), &claim(&g), &g).unwrap());
    db.open()
        .transaction(|state| {
            state.relations.insert(
                "rel-0".into(),
                StoredRelation {
                    id: "rel-0".into(),
                    kind: RelationKind::Conflict,
                    members: vec![MemoryId(1), MemoryId(2)],
                    conditions: serde_json::json!([]),
                    times: serde_json::json!([]),
                    basis: vec![],
                    status: RelationStatus::Resolved,
                    resolution: Some(RelationEvidence {
                        source_id: "s-205".into(),
                        quote: "q".into(),
                    }),
                    created_by: "test".into(),
                    closed_by: Some("test".into()),
                },
            );
            state.record_updates.insert(
                MemoryId(1),
                UpdateState {
                    status: UpdateStatus::Judged,
                    reasons: vec![],
                },
            );
            Ok(())
        })
        .unwrap();
    let original = raw_state(&db);
    let paths: [&[&str]; 6] = [
        &["w5_receipts", "s-205#0"],
        &["w5_receipts", "s-205#0", "status", "NeedsRejudge"],
        &["w5_receipts", "s-050#0", "decisions", "0"],
        &["record_updates", "1"],
        &["relations", "rel-0"],
        &["relations", "rel-0", "resolution"],
    ];
    for path in paths {
        let mut state = original.clone();
        let mut node = &mut state;
        for key in path {
            node = match key.parse::<usize>() {
                Ok(i) if node.is_array() => &mut node[i],
                _ => &mut node[*key],
            };
        }
        assert!(node.is_object(), "{path:?}");
        node["future_field"] = serde_json::json!(1);
        rusqlite::Connection::open(&db.0)
            .unwrap()
            .execute(
                "UPDATE memory_profiles SET state_json=?1",
                [state.to_string()],
            )
            .unwrap();
        assert!(
            SqliteMemoryStore::open(&db.0, UserId::from(OWNER)).is_err(),
            "{path:?}"
        );
    }
    // 还原后能正常打开：失败确实来自新增字段。
    rusqlite::Connection::open(&db.0)
        .unwrap()
        .execute(
            "UPDATE memory_profiles SET state_json=?1",
            [original.to_string()],
        )
        .unwrap();
    drop(db.open());
}

// ---------------- 存储第 2 段：更新状态（w5-storage.md 第 3 节）

fn scope_incomplete() -> UpdateState {
    UpdateState {
        status: UpdateStatus::Pending,
        reasons: vec![UpdateReason::UpdateScopeIncomplete],
    }
}

fn judged() -> UpdateState {
    UpdateState {
        status: UpdateStatus::Judged,
        reasons: vec![],
    }
}

fn pending(reasons: &[UpdateReason]) -> UpdateState {
    UpdateState {
        status: UpdateStatus::Pending,
        reasons: reasons.to_vec(),
    }
}

fn retraction(source: &str, quote: &str) -> CandidateMemory {
    let mut c = cand(source, "lives_in", "北京", quote, 200);
    c.intent = CandidateIntent::Retract { target: None };
    c
}

fn record_update(db: &TestDb, id: MemoryId) -> Option<UpdateState> {
    db.open().read().unwrap().record_updates.get(&id).cloned()
}

#[test]
fn u1_incomplete_scope_marks_products_pending_even_when_the_target_action_runs() {
    let db = TestDb::new();
    let (old, g, _) = w4_01(&db);
    assert!(!g.scope_complete);
    let receipt = committed(commit_group(&mut db.open(), &claim(&g), &g).unwrap());
    // 对明确目标的动作照常执行。
    assert_eq!(memory(&db, old).status, MemoryStatus::Superseded);
    let product = receipt.products[&0];
    assert_eq!(receipt.updates[&0], scope_incomplete());
    // 重新打开后仍在，回执里的状态与记录上的一致。
    assert_eq!(record_update(&db, product), Some(scope_incomplete()));
    assert_eq!(
        db.open().read().unwrap().w5_receipts["s-205#0"].updates[&0],
        scope_incomplete()
    );
    // 旧目标没有状态条目（未知），不因被结束而得到 judged。
    assert_eq!(record_update(&db, old), None);
}

#[test]
fn u1_complete_scope_and_decided_is_judged() {
    let db = TestDb::new();
    let (_, mut g, _) = w4_01(&db);
    g.scope_complete = true;
    let receipt = committed(commit_group(&mut db.open(), &claim(&g), &g).unwrap());
    assert_eq!(receipt.updates[&0], judged());
    assert_eq!(record_update(&db, receipt.products[&0]), Some(judged()));
}

#[test]
fn u1_reinforce_records_the_judgment_but_leaves_the_target_state_alone() {
    let db = TestDb::new();
    let old = seed(
        &db,
        &[cand("s-101", "lives_in", "北京", "我住在北京。", 100)],
    )[0];
    let g = group(
        &db,
        "s-205#0",
        vec![(0, cand("s-205", "lives_in", "北京", "我还住北京。", 200))],
        vec![decision(0, W5Action::Reinforce, &[old], &["s-205"], None)],
    );
    let receipt = committed(commit_group(&mut db.open(), &claim(&g), &g).unwrap());
    assert!(receipt.products.is_empty());
    assert_eq!(receipt.updates[&0], scope_incomplete());
    assert_eq!(record_update(&db, old), None);
}

#[test]
fn u2_pending_assertion_commits_without_touching_located_targets() {
    for (scope_complete, reason, expected) in [
        (
            true,
            W4PendingReason::UnclearRelation,
            pending(&[UpdateReason::UpdateRelationUnclear]),
        ),
        (
            false,
            W4PendingReason::UnclearRelation,
            pending(&[
                UpdateReason::UpdateScopeIncomplete,
                UpdateReason::UpdateRelationUnclear,
            ]),
        ),
        (
            true,
            W4PendingReason::TargetUnknown,
            pending(&[UpdateReason::UpdateTargetUnknown]),
        ),
        // 范围不完整与 W4 的 scope_incomplete 只记一次。
        (false, W4PendingReason::ScopeIncomplete, scope_incomplete()),
    ] {
        let db = TestDb::new();
        let old = seed(
            &db,
            &[cand("s-101", "lives_in", "北京", "我住在北京。", 100)],
        )[0];
        let mut g = group(
            &db,
            "s-205#0",
            vec![(
                0,
                cand("s-205", "lives_in", "上海", "我在上海也有个住处。", 200),
            )],
            vec![decision(
                0,
                W5Action::New,
                &[old],
                &["s-101", "s-205"],
                None,
            )],
        );
        g.scope_complete = scope_complete;
        g.decisions[0].pending_reason = Some(reason);
        let target_before = w5_fingerprint(&memory(&db, old)).unwrap();
        let receipt = committed(commit_group(&mut db.open(), &claim(&g), &g).unwrap());
        let product = receipt.products[&0];
        assert_eq!(
            memory(&db, product).status,
            MemoryStatus::Active,
            "{reason:?}"
        );
        assert_eq!(memory(&db, product).supersedes, None, "{reason:?}");
        // 列出的目标一个字节都没变。
        assert_eq!(
            w5_fingerprint(&memory(&db, old)).unwrap(),
            target_before,
            "{reason:?}"
        );
        assert_eq!(receipt.updates[&0], expected, "{reason:?}");
        assert_eq!(record_update(&db, product), Some(expected), "{reason:?}");
    }
}

#[test]
fn u2_pending_retraction_writes_nothing_but_keeps_its_state_in_the_receipt() {
    for located in [false, true] {
        let db = TestDb::new();
        let old = seed(
            &db,
            &[cand("s-101", "lives_in", "北京", "我住在北京。", 100)],
        )[0];
        let targets: Vec<MemoryId> = if located { vec![old] } else { vec![] };
        let mut g = group(
            &db,
            "s-205#0",
            vec![(0, retraction("s-205", "之前说的住址不对。"))],
            vec![decision(0, W5Action::Retract, &targets, &["s-205"], None)],
        );
        g.scope_complete = true;
        g.decisions[0].pending_reason = Some(W4PendingReason::TargetUnknown);
        let before = snapshot(&db);
        let staged_before = db.open().read().unwrap().staging.len();
        let receipt = committed(commit_group(&mut db.open(), &claim(&g), &g).unwrap());
        assert!(receipt.products.is_empty(), "located={located}");
        assert_eq!(
            receipt.updates[&0],
            pending(&[UpdateReason::UpdateTargetUnknown]),
            "located={located}"
        );
        let after = snapshot(&db);
        // 记录与 revision 都不变，只多了一份回执；也不进入旧的暂存队列（那是主体待澄清用的）。
        assert_eq!(
            (&after.0, after.1),
            (&before.0, before.1),
            "located={located}"
        );
        assert_eq!(after.2, before.2 + 1, "located={located}");
        assert_eq!(db.open().read().unwrap().staging.len(), staged_before);
        assert_eq!(memory(&db, old).status, MemoryStatus::Active);
        assert!(db.open().read().unwrap().record_updates.is_empty());
    }
}

#[test]
fn u2_pending_decisions_cannot_end_targets() {
    let db = TestDb::new();
    let (_, g, effective) = w4_01(&db);
    let before = snapshot(&db);
    let mut state_change = g.clone();
    state_change.decisions[0].pending_reason = Some(W4PendingReason::UnclearRelation);
    let mut with_effective = g.clone();
    with_effective.decisions[0].action = W5Action::New;
    with_effective.decisions[0].pending_reason = Some(W4PendingReason::UnclearRelation);
    with_effective.decisions[0].effective = Some(effective);
    let mut with_reason = with_effective.clone();
    with_reason.decisions[0].effective = None;
    with_reason.decisions[0].reason = Some(SupersedeReason::StateChanged);
    for (label, bad) in [
        ("state_change", state_change),
        ("effective", with_effective),
        ("reason", with_reason),
    ] {
        assert!(
            commit_group(&mut db.open(), &claim(&bad), &bad).is_err(),
            "{label}"
        );
        assert_eq!(snapshot(&db), before, "{label}");
    }
}

#[test]
fn u3_replay_and_reads_do_not_change_update_states() {
    let db = TestDb::new();
    let (_, g, _) = w4_01(&db);
    let quote = "我上个月从北京搬到上海了。";
    edit_source(&db, "s-205", |s| {
        s.request = Some(serde_json::json!({"text":quote,"capture_source":true,
            "session_id":"chat","turn_id":"s-205"}));
    });
    let receipt = committed(commit_group(&mut db.open(), &claim(&g), &g).unwrap());
    let product = receipt.products[&0];
    let raw_before = raw_state(&db);
    // 重放：返回原回执，状态不变。
    assert!(matches!(
        commit_group(&mut db.open(), &claim(&g), &g).unwrap(),
        W5Outcome::AlreadyApplied(_)
    ));
    // 旧格式输出：有条目的记录带出 update，没有条目的（旧记录）不带，表示未知。
    let mut service = hippocampus::chat::ChatMemoryService::new(
        db.open(),
        hippocampus::chat::ChatConfig::default(),
    )
    .unwrap();
    let listed = service
        .call(
            "memory_list",
            serde_json::json!({"include_history":true}),
            || false,
        )
        .unwrap();
    let items = listed["memories"].as_array().unwrap();
    let item = |id: MemoryId| {
        items
            .iter()
            .find(|m| m["id"] == serde_json::json!(id))
            .unwrap()
    };
    assert_eq!(
        item(product)["update"],
        serde_json::json!({"status":"pending","reasons":["update_scope_incomplete"]})
    );
    assert!(item(MemoryId(1)).get("update").is_none());
    drop(service);
    let result = db.open().search_sources("上海", None, 20).unwrap();
    let links: Vec<_> = result["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|c| c["known_memories"].as_array().unwrap().clone())
        .collect();
    let link = links
        .iter()
        .find(|l| l["memory_id"] == serde_json::json!(product))
        .expect("the committed record is linked to its source");
    assert_eq!(link["update"]["status"], "pending");
    // 读取与重放之后，状态与回执原样不变（memory_list 会跑留存整理，只可能动记录本身）。
    let raw_after = raw_state(&db);
    assert_eq!(raw_after["record_updates"], raw_before["record_updates"]);
    assert_eq!(raw_after["w5_receipts"], raw_before["w5_receipts"]);
    assert_eq!(record_update(&db, product), Some(scope_incomplete()));
}

#[test]
fn u4_rejudge_clears_each_reason_only_when_that_gap_is_covered() {
    use UpdateReason::*;
    let all = UpdateState::from_reasons(vec![
        UpdateRelationUnclear,
        UpdateScopeIncomplete,
        UpdateTargetUnknown,
        UpdateScopeIncomplete,
    ]);
    assert_eq!(
        all,
        pending(&[
            UpdateScopeIncomplete,
            UpdateTargetUnknown,
            UpdateRelationUnclear
        ])
    );
    let cover = |scope_complete, decided, relation_inputs| RejudgeCoverage {
        scope_complete,
        decided,
        relation_inputs,
    };
    // 范围完整只清除范围缺口；仍是待定决定，目标与关系缺口都留着。
    assert_eq!(
        UpdateState::after_rejudge(&all, &judged(), cover(true, false, true)),
        pending(&[UpdateTargetUnknown, UpdateRelationUnclear])
    );
    // 已定决定清除目标缺口；没送入关系输入，关系缺口留着；范围仍不完整。
    assert_eq!(
        UpdateState::after_rejudge(&all, &scope_incomplete(), cover(false, true, false)),
        pending(&[UpdateScopeIncomplete, UpdateRelationUnclear])
    );
    // 送入了关系输入并给出已定决定，才清除关系缺口；不要求旧关系已关闭。
    assert_eq!(
        UpdateState::after_rejudge(&all, &scope_incomplete(), cover(false, true, true)),
        scope_incomplete()
    );
    // 全部覆盖才变为 judged。
    assert_eq!(
        UpdateState::after_rejudge(&all, &judged(), cover(true, true, true)),
        judged()
    );
    // 本次新出现的缺口照样记上。
    assert_eq!(
        UpdateState::after_rejudge(
            &judged(),
            &pending(&[UpdateTargetUnknown]),
            cover(true, false, false)
        ),
        pending(&[UpdateTargetUnknown])
    );
}

#[test]
fn u2_candidate_kind_must_match_the_action() {
    // 撤回候选不能被写成事实（待定或已定的 new 都不行）；断言也不能当撤回去结束目标。
    let db = TestDb::new();
    let old = seed(
        &db,
        &[cand("s-101", "lives_in", "北京", "我住在北京。", 100)],
    )[0];
    let retract_as_new = group(
        &db,
        "s-205#0",
        vec![(0, retraction("s-205", "之前说的住址不对。"))],
        vec![decision(0, W5Action::New, &[], &["s-205"], None)],
    );
    let mut retract_as_pending_new = retract_as_new.clone();
    retract_as_pending_new.decisions[0].pending_reason = Some(W4PendingReason::TargetUnknown);
    let mut retract_as_state_change = group(
        &db,
        "s-205#0",
        vec![(0, retraction("s-205", "之前说的住址不对。"))],
        vec![decision(
            0,
            W5Action::StateChange,
            &[old],
            &["s-101", "s-205"],
            None,
        )],
    );
    retract_as_state_change.scope_complete = true;
    let assertion_as_retract = group(
        &db,
        "s-205#0",
        vec![(0, cand("s-205", "lives_in", "上海", "我搬到上海了。", 200))],
        vec![decision(
            0,
            W5Action::Retract,
            &[old],
            &["s-101", "s-205"],
            None,
        )],
    );
    let before = snapshot(&db);
    for (label, g) in [
        ("retraction as new", retract_as_new),
        ("retraction as pending new", retract_as_pending_new),
        ("retraction as state_change", retract_as_state_change),
        ("assertion as retract", assertion_as_retract),
    ] {
        let error = commit_group(&mut db.open(), &claim(&g), &g).unwrap_err();
        assert!(
            error.to_string().contains("does not match"),
            "{label}: {error}"
        );
        assert_eq!(snapshot(&db), before, "{label}");
        assert!(
            db.open().read().unwrap().record_updates.is_empty(),
            "{label}"
        );
    }
    assert_eq!(memory(&db, old).status, MemoryStatus::Active);
}

// ---------------- 第 3 段：冲突关系（w5-storage.md 第 4 节；w5-acceptance.md D1、E1）

/// 已整理完成的旧原文（不是本次认领的任务），带原文正文，供关系依据逐字核对。
fn stored_source(db: &TestDb, key: &str, text: &str) {
    ensure_claimed_source(db, key);
    edit_source(db, key, |s| {
        s.request = Some(serde_json::json!({"text": text, "capture_source": true}));
        s.enrichment = "committed".into();
        s.task = EnrichmentTask::default();
    });
}

/// 本次认领的原文：设置正文。
fn claimed_source(db: &TestDb, key: &str, text: &str) {
    ensure_claimed_source(db, key);
    edit_source(db, key, |s| {
        s.request = Some(serde_json::json!({"text": text, "capture_source": true}))
    });
}

fn evidence(source: &str, quote: &str) -> RelationEvidence {
    RelationEvidence {
        source_id: source.into(),
        quote: quote.into(),
    }
}

fn retraction_of(source: &str, quote: &str) -> CandidateMemory {
    let mut c = cand(source, "fact", "", quote, 300);
    c.intent = CandidateIntent::Retract { target: None };
    c
}

fn relation(db: &TestDb, id: &str) -> StoredRelation {
    db.open().read().unwrap().relations[id].clone()
}

/// 直接放入一条 open 冲突关系（模拟此前一组 W5 建立的关系）。
fn seed_relation(db: &TestDb, members: &[MemoryId], basis: Vec<RelationEvidence>) -> String {
    db.open()
        .transaction(|state| {
            let id = format!("rel-{}", state.next_relation);
            state.next_relation += 1;
            state.relations.insert(
                id.clone(),
                StoredRelation {
                    id: id.clone(),
                    kind: RelationKind::Conflict,
                    members: members.to_vec(),
                    conditions: serde_json::json!([]),
                    times: serde_json::json!([]),
                    basis,
                    status: RelationStatus::Open,
                    resolution: None,
                    created_by: "seed".into(),
                    closed_by: None,
                },
            );
            Ok(id)
        })
        .unwrap()
}

const SEVEN: &str = "我已确认栖风读书机的屏幕尺寸是7英寸。";
const EIGHT_UNSURE: &str = "另一份资料写栖风读书机的屏幕是8英寸，我还没核实哪份对。";

/// w4_07：50（7 英寸）已存在；s-260 说 8 英寸、对不上、未核实。
fn w4_07(db: &TestDb) -> (MemoryId, W5Group) {
    let text = "我又查到栖风读书机的屏幕是8英寸，和之前记的7英寸对不上，还没核实哪个对。";
    stored_source(db, "s-130", SEVEN);
    let old = seed(db, &[cand("s-130", "fact", SEVEN, SEVEN, 100)])[0];
    claimed_source(db, "s-260", text);
    let mut d = decision(0, W5Action::Conflict, &[old], &["s-130", "s-260"], None);
    d.relation = Some(W5RelationSpec {
        conditions: serde_json::json!([]),
        times: serde_json::json!([]),
        basis: vec![evidence("s-130", SEVEN), evidence("s-260", text)],
    });
    let g = group(
        db,
        "s-260#0",
        vec![(0, cand("s-260", "fact", text, text, 260))],
        vec![d],
    );
    (old, g)
}

#[test]
fn r1_conflict_keeps_the_old_record_and_opens_a_relation() {
    let db = TestDb::new();
    let (old, g) = w4_07(&db);
    let before_old = w5_fingerprint(&memory(&db, old)).unwrap();
    let receipt = committed(commit_group(&mut db.open(), &claim(&g), &g).unwrap());
    let new = receipt.products[&0];
    // 旧版本不结束、不取代。
    assert_eq!(w5_fingerprint(&memory(&db, old)).unwrap(), before_old);
    assert_eq!(memory(&db, new).status, MemoryStatus::Active);
    assert_eq!(memory(&db, new).supersedes, None);
    let rel = relation(&db, "rel-0");
    assert_eq!(rel.members, vec![old, new]);
    assert_eq!(rel.status, RelationStatus::Open);
    assert_eq!(rel.created_by, "s-260#0");
    assert_eq!(rel.basis.len(), 2);
    assert_eq!(db.open().read().unwrap().next_relation, 1);
    // 通用 fact 范围不完整：新记录 pending/scope_incomplete；回执记下同样的状态。
    assert_eq!(receipt.updates[&0], scope_incomplete());
    assert_eq!(record_update(&db, new), Some(scope_incomplete()));
    // 旧格式输出带出两端所在的 open 关系。
    let mut service = hippocampus::chat::ChatMemoryService::new(
        db.open(),
        hippocampus::chat::ChatConfig::default(),
    )
    .unwrap();
    let listed = service
        .call("memory_list", serde_json::json!({}), || false)
        .unwrap();
    for id in [old, new] {
        let item = listed["memories"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == serde_json::json!(id))
            .unwrap();
        assert_eq!(
            item["open_relations"],
            serde_json::json!(["rel-0"]),
            "{id:?}"
        );
    }
    drop(service);
    // 重放：不再建关系。
    let before = snapshot(&db);
    assert!(matches!(
        commit_group(&mut db.open(), &claim(&g), &g).unwrap(),
        W5Outcome::AlreadyApplied(_)
    ));
    assert_eq!(snapshot(&db), before);
    assert_eq!(db.open().read().unwrap().relations.len(), 1);
}

#[test]
fn r1_conflict_basis_must_be_verbatim_and_cover_both_sides() {
    let db = TestDb::new();
    let (_, g) = w4_07(&db);
    let before = snapshot(&db);
    let spec = |g: &W5Group| g.decisions[0].relation.clone().unwrap();
    let mut only_new = g.clone();
    only_new.decisions[0].relation = Some(W5RelationSpec {
        basis: vec![spec(&g).basis[1].clone()],
        ..spec(&g)
    });
    let mut paraphrased = g.clone();
    paraphrased.decisions[0].relation = Some(W5RelationSpec {
        basis: vec![
            evidence("s-130", "屏幕尺寸是7寸"),
            spec(&g).basis[1].clone(),
        ],
        ..spec(&g)
    });
    let mut unknown_source = g.clone();
    unknown_source.decisions[0].relation = Some(W5RelationSpec {
        basis: vec![evidence("s-999", SEVEN), spec(&g).basis[1].clone()],
        ..spec(&g)
    });
    let mut no_spec = g.clone();
    no_spec.decisions[0].relation = None;
    let mut spec_on_new = g.clone();
    spec_on_new.decisions[0].action = W5Action::New;
    spec_on_new.decisions[0].targets.clear();
    for (label, bad, expected) in [
        (
            "only the new side",
            only_new,
            "both the new source and a target",
        ),
        ("paraphrased quote", paraphrased, "not verbatim"),
        ("unknown source", unknown_source, "unknown source"),
        (
            "conflict without relation",
            no_spec,
            "exactly a conflict carries",
        ),
        ("relation on new", spec_on_new, "exactly a conflict carries"),
    ] {
        let error = commit_group(&mut db.open(), &claim(&bad), &bad).unwrap_err();
        assert!(error.to_string().contains(expected), "{label}: {error}");
        assert_eq!(snapshot(&db), before, "{label}");
        assert!(db.open().read().unwrap().relations.is_empty(), "{label}");
    }
}

/// w4_15 / w4_21 的初始状态：50（7 英寸）、51（8 英寸未核实），rel-0 open。
fn conflict_pair(db: &TestDb) -> (MemoryId, MemoryId, String) {
    stored_source(db, "s-130", SEVEN);
    stored_source(db, "s-261", EIGHT_UNSURE);
    let ids = seed(
        db,
        &[
            cand("s-130", "fact", SEVEN, SEVEN, 100),
            cand("s-261", "fact", EIGHT_UNSURE, EIGHT_UNSURE, 200),
        ],
    );
    let rel = seed_relation(
        db,
        &ids,
        vec![evidence("s-130", SEVEN), evidence("s-261", EIGHT_UNSURE)],
    );
    (ids[0], ids[1], rel)
}

/// w4_15：用户确认 7 英寸是记错的——撤回 50 并解决关系，51 不动。
fn w4_15(db: &TestDb) -> (MemoryId, MemoryId, String, W5Group) {
    let (seven, eight, rel) = conflict_pair(db);
    let g = w4_15_group(db, seven, &rel);
    (seven, eight, rel, g)
}

fn w4_15_group(db: &TestDb, seven: MemoryId, rel: &str) -> W5Group {
    let text = "7英寸那个是我记错了。";
    claimed_source(db, "s-370", text);
    let mut d = decision(0, W5Action::Retract, &[seven], &["s-130", "s-370"], None);
    d.resolves_relations = vec![rel.to_owned()];
    d.resolution = Some(evidence("s-370", text));
    group(
        db,
        "s-370#0",
        vec![(0, retraction_of("s-370", text))],
        vec![d],
    )
}

#[test]
fn r2_resolving_retract_closes_the_relation_with_its_resolution() {
    let db = TestDb::new();
    let (seven, eight, rel, g) = w4_15(&db);
    assert_eq!(g.seen[0].relations, vec![rel.clone()]);
    assert_eq!(g.seen_relations.len(), 1);
    let before_eight = w5_fingerprint(&memory(&db, eight)).unwrap();
    committed(commit_group(&mut db.open(), &claim(&g), &g).unwrap());
    assert_eq!(memory(&db, seven).status, MemoryStatus::Superseded);
    assert_eq!(
        memory(&db, seven).supersede_reason,
        Some(SupersedeReason::UserRetracted)
    );
    assert_eq!(w5_fingerprint(&memory(&db, eight)).unwrap(), before_eight);
    let closed = relation(&db, &rel);
    assert_eq!(closed.status, RelationStatus::Resolved);
    assert_eq!(
        closed.resolution,
        Some(evidence("s-370", "7英寸那个是我记错了。"))
    );
    assert_eq!(closed.closed_by.as_deref(), Some("s-370#0"));
    assert_eq!(closed.members, vec![seven, eight]);
}

#[test]
fn r2_relation_changes_after_w4_input_need_rejudge() {
    // 关系在 W4 之后被解决或失效，或目标新加入一条关系：整组 needs_rejudge，零业务写入。
    for change in ["invalidated", "joined_new_relation"] {
        let db = TestDb::new();
        let (seven, eight, rel, g) = w4_15(&db);
        // 只验证关系快照这一项：变更后把无来源变更触发的顺序屏障复原，否则任务先被标为过期。
        let barrier = db.open().read().unwrap().source_less_barrier_sequence;
        match change {
            "invalidated" => {
                db.open()
                    .transaction(|state| {
                        let r = state.relations.get_mut(&rel).unwrap();
                        r.status = RelationStatus::Invalidated;
                        Ok(())
                    })
                    .unwrap();
            }
            _ => {
                seed_relation(&db, &[seven, eight], vec![evidence("s-130", SEVEN)]);
            }
        }
        db.open()
            .transaction(|state| {
                state.source_less_barrier_sequence = barrier;
                let task = state.sources.get_mut("s-370").unwrap();
                task.enrichment = "running".into();
                task.error = None;
                Ok(())
            })
            .unwrap();
        let before = snapshot(&db);
        let relations_before = db.open().read().unwrap().relations.clone();
        let reason = needs_rejudge(commit_group(&mut db.open(), &claim(&g), &g).unwrap());
        assert!(reason.contains("relation"), "{change}: {reason}");
        let after = snapshot(&db);
        assert_eq!((&after.0, after.1), (&before.0, before.1), "{change}");
        assert_eq!(
            db.open().read().unwrap().relations,
            relations_before,
            "{change}"
        );
        assert_eq!(memory(&db, seven).status, MemoryStatus::Active, "{change}");
    }
}

#[test]
fn r2_resolution_rules_are_checked_mechanically() {
    let db = TestDb::new();
    let (_, _, rel, g) = w4_15(&db);
    let before = snapshot(&db);
    let mut old_source = g.clone();
    old_source.decisions[0].resolution = Some(evidence("s-130", SEVEN));
    let mut no_resolution = g.clone();
    no_resolution.decisions[0].resolution = None;
    let mut twice = g.clone();
    twice.decisions[0].resolves_relations = vec![rel.clone(), rel.clone()];
    let mut unsnapshotted = g.clone();
    unsnapshotted.seen_relations.clear();
    for (label, bad, expected) in [
        (
            "resolution from an old source",
            old_source,
            "claimed source",
        ),
        ("no resolution", no_resolution, "go together"),
        ("resolved twice", twice, "resolved twice"),
        ("no relation snapshot", unsnapshotted, "has no W4 snapshot"),
    ] {
        let error = commit_group(&mut db.open(), &claim(&bad), &bad).unwrap_err();
        assert!(error.to_string().contains(expected), "{label}: {error}");
        assert_eq!(snapshot(&db), before, "{label}");
        assert_eq!(relation(&db, &rel).status, RelationStatus::Open, "{label}");
    }
}

/// w4_21：一句话拆成细化 51 与撤回 50（并解决关系），两决定同一组。
fn w4_21(db: &TestDb) -> (MemoryId, MemoryId, String, W5Group) {
    let (seven, eight, rel) = conflict_pair(db);
    let text = "我核实过了，栖风读书机的屏幕是8英寸，7英寸那份是记错的。";
    claimed_source(db, "s-371", text);
    let mut refine = decision(
        0,
        W5Action::StateChange,
        &[eight],
        &["s-261", "s-371"],
        None,
    );
    refine.reason = Some(SupersedeReason::Refinement);
    let mut retract = decision(1, W5Action::Retract, &[seven], &["s-130", "s-371"], None);
    retract.resolves_relations = vec![rel.clone()];
    retract.resolution = Some(evidence("s-371", text));
    let mut second = retraction_of("s-371", text);
    second.evidence[0].id = EvidenceId::from("s-371-b");
    let g = group(
        db,
        "s-371#0",
        vec![(0, cand("s-371", "fact", text, text, 371)), (1, second)],
        vec![refine, retract],
    );
    (seven, eight, rel, g)
}

#[test]
fn d1_refine_and_resolving_retract_commit_together() {
    let db = TestDb::new();
    let (seven, eight, rel, g) = w4_21(&db);
    let receipt = committed(commit_group(&mut db.open(), &claim(&g), &g).unwrap());
    let new = receipt.products[&0];
    assert_eq!(
        memory(&db, seven).supersede_reason,
        Some(SupersedeReason::UserRetracted)
    );
    assert_eq!(
        memory(&db, eight).supersede_reason,
        Some(SupersedeReason::Refinement)
    );
    assert_eq!(memory(&db, eight).superseded_by, Some(new));
    assert_eq!(memory(&db, new).supersedes, Some(eight));
    assert_eq!(relation(&db, &rel).status, RelationStatus::Resolved);
    // 关系在本组内解决：替代记录不标 relation_unclear，只有范围不完整。
    assert_eq!(record_update(&db, new), Some(scope_incomplete()));
}

#[test]
fn d1_fault_between_the_two_decisions_rolls_back_both_and_the_relation() {
    let db = TestDb::new();
    let (seven, eight, rel, g) = w4_21(&db);
    let before = snapshot(&db);
    let relations_before = db.open().read().unwrap().relations.clone();
    for step in [
        W5Step::AfterDecision(0),
        W5Step::AfterDecision(1),
        W5Step::BeforeSave,
    ] {
        assert!(
            commit_group_with_hook(&mut db.open(), &claim(&g), &g, &mut fail_at(step)).is_err()
        );
        assert_eq!(snapshot(&db), before, "{step:?}");
        assert_eq!(
            db.open().read().unwrap().relations,
            relations_before,
            "{step:?}"
        );
    }
    assert_eq!(memory(&db, seven).status, MemoryStatus::Active);
    assert_eq!(memory(&db, eight).status, MemoryStatus::Active);
    assert_eq!(relation(&db, &rel).status, RelationStatus::Open);
    // 回滚后重试恰好生效一次。
    committed(commit_group(&mut db.open(), &claim(&g), &g).unwrap());
    assert_eq!(relation(&db, &rel).status, RelationStatus::Resolved);
}

#[test]
fn d1_one_stale_target_sends_the_whole_group_to_rejudge() {
    let db = TestDb::new();
    let (seven, eight, rel, g) = w4_21(&db);
    let earlier = older_source_reinforces(&db, eight);
    committed(commit_group(&mut db.open(), &claim(&earlier), &earlier).unwrap());
    let before = snapshot(&db);
    needs_rejudge(commit_group(&mut db.open(), &claim(&g), &g).unwrap());
    let after = snapshot(&db);
    assert_eq!((&after.0, after.1), (&before.0, before.1));
    assert_eq!(memory(&db, seven).status, MemoryStatus::Active);
    assert_eq!(relation(&db, &rel).status, RelationStatus::Open);
}

#[test]
fn e1_replacing_unresolved_members_keeps_the_relation_and_marks_relation_unclear() {
    let db = TestDb::new();
    stored_source(&db, "s-501", "我住在北京。");
    stored_source(&db, "s-502", "我住在上海。");
    let ids = seed(
        &db,
        &[
            cand("s-501", "lives_in", "北京", "我住在北京。", 100),
            cand("s-502", "lives_in", "上海", "我住在上海。", 110),
        ],
    );
    let rel = seed_relation(
        &db,
        &ids,
        vec![
            evidence("s-501", "我住在北京。"),
            evidence("s-502", "我住在上海。"),
        ],
    );
    let before_relation = relation(&db, &rel);
    claimed_source(&db, "s-510", "我已经搬到深圳了。");
    let mut g = group(
        &db,
        "s-510#0",
        vec![(
            0,
            cand("s-510", "lives_in", "深圳", "我已经搬到深圳了。", 510),
        )],
        vec![decision(
            0,
            W5Action::StateChange,
            &ids,
            &["s-501", "s-502", "s-510"],
            None,
        )],
    );
    g.scope_complete = true;
    let receipt = committed(commit_group(&mut db.open(), &claim(&g), &g).unwrap());
    let new = receipt.products[&0];
    for id in &ids {
        assert_eq!(memory(&db, *id).status, MemoryStatus::Superseded);
        assert_eq!(memory(&db, *id).superseded_by, Some(new));
    }
    // 关系与成员、依据原样；不把成员换成新记录，不去重，不自动解决。
    assert_eq!(relation(&db, &rel), before_relation);
    // 范围完整：只有 relation_unclear。
    let unclear = UpdateState {
        status: UpdateStatus::Pending,
        reasons: vec![UpdateReason::UpdateRelationUnclear],
    };
    assert_eq!(receipt.updates[&0], unclear);
    assert_eq!(record_update(&db, new), Some(unclear));
}

#[test]
fn r2_resolving_rechecks_every_member_not_only_the_targets() {
    // 8 英寸那条不是撤回的目标，但它是要解决的关系的成员：W4 之后它被较早的原文强化，
    // 关系据以解决的前提已变，整组 needs_rejudge。
    let db = TestDb::new();
    let (seven, eight, rel) = conflict_pair(&db);
    let earlier = older_source_reinforces(&db, eight);
    let g = w4_15_group(&db, seven, &rel);
    assert!(g.seen.iter().any(|seen| seen.id == eight));
    committed(commit_group(&mut db.open(), &claim(&earlier), &earlier).unwrap());
    let before = snapshot(&db);
    let reason = needs_rejudge(commit_group(&mut db.open(), &claim(&g), &g).unwrap());
    assert!(reason.contains(&format!("target {}", eight.0)), "{reason}");
    let after = snapshot(&db);
    assert_eq!((&after.0, after.1), (&before.0, before.1));
    assert_eq!(relation(&db, &rel).status, RelationStatus::Open);
    // 少了某个成员的快照：直接拒绝。
    let db = TestDb::new();
    let (seven, eight, _rel, mut g) = w4_15(&db);
    g.seen.retain(|seen| seen.id != eight);
    let before = snapshot(&db);
    let error = commit_group(&mut db.open(), &claim(&g), &g).unwrap_err();
    assert!(error.to_string().contains("has no W4 snapshot"), "{error}");
    assert_eq!(snapshot(&db), before);
    assert_eq!(memory(&db, seven).status, MemoryStatus::Active);
}

// ---------------- 第 3 段后半：删除范围与关系失效（w5-storage.md 第 4 节）

fn forget(db: &TestDb, arguments: serde_json::Value) -> serde_json::Value {
    let mut service = hippocampus::chat::ChatMemoryService::new(
        db.open(),
        hippocampus::chat::ChatConfig::default(),
    )
    .unwrap();
    service.call("memory_forget", arguments, || false).unwrap()
}

fn status(db: &TestDb, id: MemoryId) -> MemoryStatus {
    memory(db, id).status
}

fn source_deleted(db: &TestDb, key: &str) -> bool {
    source(db, key).request.is_none()
}

/// 提交一组并收尾任务。
fn w5_commit(db: &TestDb, g: &W5Group) -> W5Receipt {
    let receipt = committed(commit_group(&mut db.open(), &claim(g), g).unwrap());
    w5_finish_task(&mut db.open(), &claim(g), &W5TaskEnd::Committed).unwrap();
    receipt
}

/// W5 新增一条记录（有回执，创建来源可确认）。
fn w5_new(db: &TestDb, key: &str, predicate: &str, value: &str, text: &str) -> MemoryId {
    claimed_source(db, key, text);
    let g = group(
        db,
        &format!("{key}#0"),
        vec![(0, cand(key, predicate, value, text, 300))],
        vec![decision(0, W5Action::New, &[], &[key], None)],
    );
    w5_commit(db, &g).products[&0]
}

/// 整份快照里不能再出现被删原文的文字（回执指纹、决定、关系、记录都算，软删除记录的值也
/// 打墓碑，只留 ID、状态、谱系与墓碑证据）。
fn assert_text_gone(db: &TestDb, text: &str) {
    let state = raw_state(db);
    let raw = serde_json::to_string(&state).unwrap();
    let escaped = serde_json::to_string(text).unwrap();
    let inner = &escaped[1..escaped.len() - 1];
    assert!(!raw.contains(inner), "{text} still in the snapshot");
    // 嵌套快照里的二次转义形式也不能有。
    let twice = serde_json::to_string(inner).unwrap();
    assert!(
        !raw.contains(&twice[1..twice.len() - 1]),
        "{text} still nested"
    );
}

/// w4_22：110（北京，s-501）与 111（上海，s-502）互相冲突；s-510 同时替代两者为 N。
fn w4_22_live(db: &TestDb) -> (MemoryId, MemoryId, MemoryId, String) {
    let beijing = w5_new(db, "s-501", "lives_in", "北京", "我住在北京。");
    claimed_source(db, "s-502", "我住在上海。");
    let mut d = decision(0, W5Action::Conflict, &[beijing], &["s-501", "s-502"], None);
    d.relation = Some(W5RelationSpec {
        conditions: serde_json::json!([{"quote": "我住在北京。"}]),
        times: serde_json::json!([]),
        basis: vec![
            evidence("s-501", "我住在北京。"),
            evidence("s-502", "我住在上海。"),
        ],
    });
    let g = group(
        db,
        "s-502#0",
        vec![(0, cand("s-502", "lives_in", "上海", "我住在上海。", 310))],
        vec![d],
    );
    let shanghai = w5_commit(db, &g).products[&0];
    claimed_source(db, "s-510", "我已经搬到深圳了。");
    let mut g = group(
        db,
        "s-510#0",
        vec![(
            0,
            cand("s-510", "lives_in", "深圳", "我已经搬到深圳了。", 320),
        )],
        vec![decision(
            0,
            W5Action::StateChange,
            &[beijing, shanghai],
            &["s-501", "s-502", "s-510"],
            None,
        )],
    );
    g.scope_complete = true;
    let new = w5_commit(db, &g).products[&0];
    (beijing, shanghai, new, "rel-0".into())
}

#[test]
fn f1_deleting_one_member_source_keeps_the_other_branch_and_invalidates_the_relation() {
    let db = TestDb::new();
    let (beijing, shanghai, new, rel) = w4_22_live(&db);
    let result = forget(&db, serde_json::json!({"source_id":"s-501"}));
    assert_eq!(result["forgotten_ids"], serde_json::json!([beijing]));
    assert_eq!(status(&db, beijing), MemoryStatus::Deleted);
    assert_eq!(status(&db, shanghai), MemoryStatus::Superseded);
    assert_eq!(status(&db, new), MemoryStatus::Active);
    assert!(source_deleted(&db, "s-501"));
    assert!(!source_deleted(&db, "s-502") && !source_deleted(&db, "s-510"));
    let invalid = relation(&db, &rel);
    assert_eq!(invalid.status, RelationStatus::Invalidated);
    assert_eq!(invalid.closed_by, None);
    assert_eq!(invalid.basis[0].quote, "[deleted source]");
    assert_eq!(invalid.basis[1].quote, "我住在上海。");
    assert_eq!(result["invalidated_relation_ids"], serde_json::json!([rel]));
    assert_text_gone(&db, "我住在北京。");
}

#[test]
fn f2_forgetting_by_record_follows_its_own_lineage_only() {
    // 删 N：两个前身都在谱系里。
    let db = TestDb::new();
    let (beijing, shanghai, new, _) = w4_22_live(&db);
    forget(&db, serde_json::json!({"memory_id": new.0}));
    for id in [beijing, shanghai, new] {
        assert_eq!(status(&db, id), MemoryStatus::Deleted, "{id:?}");
    }
    for key in ["s-501", "s-502", "s-510"] {
        assert!(source_deleted(&db, key), "{key}");
    }
    // 删 110：继任 N 在谱系里，但不经 N 折回到它的另一个前身 111。
    let db = TestDb::new();
    let (beijing, shanghai, new, rel) = w4_22_live(&db);
    forget(&db, serde_json::json!({"memory_id": beijing.0}));
    assert_eq!(status(&db, beijing), MemoryStatus::Deleted);
    assert_eq!(status(&db, new), MemoryStatus::Deleted);
    assert_eq!(status(&db, shanghai), MemoryStatus::Superseded);
    assert!(!source_deleted(&db, "s-502"));
    assert_eq!(relation(&db, &rel).status, RelationStatus::Invalidated);
}

#[test]
fn f3_forgetting_the_middle_of_a_chain_forgets_all_three_generations() {
    let db = TestDb::new();
    let a = w5_new(&db, "s-1", "lives_in", "北京", "我住在北京。");
    let mut ids = vec![a];
    for (key, value, text) in [
        ("s-2", "上海", "我搬到上海了。"),
        ("s-3", "深圳", "我又搬到深圳了。"),
    ] {
        claimed_source(&db, key, text);
        let mut g = group(
            &db,
            &format!("{key}#0"),
            vec![(0, cand(key, "lives_in", value, text, 300))],
            vec![decision(
                0,
                W5Action::StateChange,
                &[*ids.last().unwrap()],
                &[key],
                None,
            )],
        );
        g.scope_complete = true;
        ids.push(w5_commit(&db, &g).products[&0]);
    }
    forget(&db, serde_json::json!({"memory_id": ids[1].0}));
    for id in &ids {
        assert_eq!(status(&db, *id), MemoryStatus::Deleted, "{id:?}");
    }
}

#[test]
fn f4_deleting_the_resolving_source_invalidates_the_resolved_relation() {
    let db = TestDb::new();
    let seven = w5_new(&db, "s-130", "fact", SEVEN, SEVEN);
    claimed_source(&db, "s-261", EIGHT_UNSURE);
    let mut d = decision(0, W5Action::Conflict, &[seven], &["s-130", "s-261"], None);
    d.relation = Some(W5RelationSpec {
        conditions: serde_json::json!([]),
        times: serde_json::json!([]),
        basis: vec![evidence("s-130", SEVEN), evidence("s-261", EIGHT_UNSURE)],
    });
    let g = group(
        &db,
        "s-261#0",
        vec![(0, cand("s-261", "fact", EIGHT_UNSURE, EIGHT_UNSURE, 261))],
        vec![d],
    );
    let eight = w5_commit(&db, &g).products[&0];
    let text = "我核实过了，栖风读书机的屏幕是8英寸，7英寸那份是记错的。";
    claimed_source(&db, "s-371", text);
    let mut refine = decision(
        0,
        W5Action::StateChange,
        &[eight],
        &["s-261", "s-371"],
        None,
    );
    refine.reason = Some(SupersedeReason::Refinement);
    let mut retract = decision(1, W5Action::Retract, &[seven], &["s-130", "s-371"], None);
    retract.resolves_relations = vec!["rel-0".into()];
    retract.resolution = Some(evidence("s-371", text));
    let mut second = retraction_of("s-371", text);
    second.evidence[0].id = EvidenceId::from("s-371-b");
    let g = group(
        &db,
        "s-371#0",
        vec![(0, cand("s-371", "fact", text, text, 371)), (1, second)],
        vec![refine, retract],
    );
    let verified = w5_commit(&db, &g).products[&0];
    assert_eq!(relation(&db, "rel-0").status, RelationStatus::Resolved);

    forget(&db, serde_json::json!({"source_id":"s-371"}));
    let invalid = relation(&db, "rel-0");
    assert_eq!(invalid.status, RelationStatus::Invalidated);
    assert_eq!(invalid.resolution.unwrap().quote, "[deleted source]");
    // 解决依据被删，不能再算已解决，也不回到 open；前身保持 superseded，不自动恢复。
    assert_eq!(status(&db, verified), MemoryStatus::Deleted);
    assert_eq!(status(&db, seven), MemoryStatus::Superseded);
    assert_eq!(
        memory(&db, seven).supersede_reason,
        Some(SupersedeReason::UserRetracted)
    );
    assert_eq!(status(&db, eight), MemoryStatus::Superseded);
    assert_text_gone(&db, text);
}

#[test]
fn f5_only_control_evidence_left_means_the_record_goes() {
    // M 由 s-1 创建、被 s-2 撤回（撤回原文作为控制证据追加）；删 s-1 后只剩控制证据。
    let db = TestDb::new();
    let m = w5_new(&db, "s-1", "lives_in", "北京", "我住在北京。");
    let text = "之前说的住址不对。";
    claimed_source(&db, "s-2", text);
    let mut c = cand("s-2", "lives_in", "北京", text, 300);
    c.intent = CandidateIntent::Retract { target: None };
    let g = group(
        &db,
        "s-2#0",
        vec![(0, c)],
        vec![decision(0, W5Action::Retract, &[m], &["s-1", "s-2"], None)],
    );
    w5_commit(&db, &g);
    forget(&db, serde_json::json!({"source_id":"s-1"}));
    assert_eq!(status(&db, m), MemoryStatus::Deleted);
    assert!(!source_deleted(&db, "s-2"));
}

#[test]
fn f6_an_independently_reinforced_record_stays_with_recounted_reinforcement() {
    let db = TestDb::new();
    let m = w5_new(&db, "s-1", "lives_in", "北京", "我住在北京。");
    for (key, text, observed) in [
        ("s-2", "我还住在北京。", 2_000),
        ("s-3", "我仍然住北京。", 3_000),
    ] {
        claimed_source(&db, key, text);
        edit_source(&db, key, |s| s.observed_at = observed);
        // 同一原文两组都强化：只算一次。
        let groups: Vec<W5Group> = (0..2)
            .map(|n| {
                group(
                    &db,
                    &format!("{key}#{n}"),
                    vec![(0, cand(key, "lives_in", "北京", text, observed))],
                    vec![decision(0, W5Action::Reinforce, &[m], &[key], None)],
                )
            })
            .collect();
        committed(commit_group(&mut db.open(), &claim(&groups[0]), &groups[0]).unwrap());
        // 第二组的快照早于第一组的强化：needs_rejudge，不执行。
        needs_rejudge(commit_group(&mut db.open(), &claim(&groups[1]), &groups[1]).unwrap());
        w5_finish_task(&mut db.open(), &claim(&groups[0]), &W5TaskEnd::Committed).unwrap();
    }
    assert_eq!(memory(&db, m).reinforcement_count, 2);
    forget(&db, serde_json::json!({"source_id":"s-1"}));
    let kept = memory(&db, m);
    assert_eq!(kept.status, MemoryStatus::Active);
    assert_eq!(kept.reinforcement_count, 2);
    assert_eq!(kept.last_reinforced_at, Some(Timestamp(3_000)));
    assert!(kept.evidence.iter().any(|e| e.quote == "[deleted source]"));
    forget(&db, serde_json::json!({"source_id":"s-3"}));
    let kept = memory(&db, m);
    assert_eq!(kept.status, MemoryStatus::Active);
    assert_eq!(kept.reinforcement_count, 1);
    assert_eq!(kept.last_reinforced_at, Some(Timestamp(2_000)));
    assert_text_gone(&db, "我住在北京。");
    assert_text_gone(&db, "我仍然住北京。");
}

#[test]
fn f7_a_needs_rejudge_reinforce_receipt_is_not_support() {
    // s-2 的一组撤回了 M；另一组以旧快照强化 M，得到 needs_rejudge。删 s-1 后 M 只剩控制证据。
    let db = TestDb::new();
    let m = w5_new(&db, "s-1", "lives_in", "北京", "我住在北京。");
    let text = "之前说的住址不对，不过我还住北京。";
    claimed_source(&db, "s-2", text);
    let mut c = cand("s-2", "lives_in", "北京", text, 300);
    c.intent = CandidateIntent::Retract { target: None };
    let retract = group(
        &db,
        "s-2#0",
        vec![(0, c)],
        vec![decision(0, W5Action::Retract, &[m], &["s-1", "s-2"], None)],
    );
    let mut r = cand("s-2", "lives_in", "北京", text, 300);
    r.evidence[0].id = EvidenceId::from("s-2-b");
    let reinforce = group(
        &db,
        "s-2#1",
        vec![(0, r)],
        vec![decision(0, W5Action::Reinforce, &[m], &["s-2"], None)],
    );
    committed(commit_group(&mut db.open(), &claim(&retract), &retract).unwrap());
    needs_rejudge(commit_group(&mut db.open(), &claim(&reinforce), &reinforce).unwrap());
    forget(&db, serde_json::json!({"source_id":"s-1"}));
    assert_eq!(status(&db, m), MemoryStatus::Deleted);
}

/// 旧整理回执（memory_remember / memory_clarify 写的形状），直接写入快照。
fn inject_legacy_receipt(
    db: &TestDb,
    key: &str,
    request: serde_json::Value,
    result: serde_json::Value,
) {
    let mut state = raw_state(db);
    state["receipts"][key] = serde_json::json!({"request": request, "result": result});
    rusqlite::Connection::open(&db.0)
        .unwrap()
        .execute(
            "UPDATE memory_profiles SET state_json=?1",
            [state.to_string()],
        )
        .unwrap();
}

#[test]
fn f8_legacy_clarified_record_is_supported_by_its_original_statement() {
    // A 陈述（主体待澄清，暂存 7 号）→ B 澄清后创建 X → C 强化 X。删 C：X 由 A 支持，保留。
    // 没有 A 的暂存回执时，剩下的 A、B 证据角色不明，X 删除。
    for with_pending_receipt in [true, false] {
        let db = TestDb::new();
        stored_source(&db, "A", "孩子住在上海。");
        stored_source(&db, "B", "孩子指的是小明");
        stored_source(&db, "C", "小明还住在上海。");
        let mut c = cand("A", "lives_in", "上海", "孩子住在上海。", 100);
        c.evidence.push(Evidence::new(
            EvidenceId::from("B"),
            SessionId::from("chat"),
            TurnId::from("B"),
            "孩子指的是小明",
        ));
        c.evidence.push(Evidence::new(
            EvidenceId::from("C"),
            SessionId::from("chat"),
            TurnId::from("C"),
            "小明还住在上海。",
        ));
        let x = seed(&db, &[c])[0];
        let applied = |action: &str| serde_json::json!({"Ingested":{"Applied":{"action":action,"memory_id":x,"target":null}}});
        if with_pending_receipt {
            inject_legacy_receipt(
                &db,
                "a",
                serde_json::json!({"session_id":"chat","turn_id":"A","text":"孩子住在上海。"}),
                serde_json::json!({"outcomes":[{"Pending":{"staged_id":7,"resolution":"NotFound"}}]}),
            );
        }
        inject_legacy_receipt(
            &db,
            "b",
            serde_json::json!({"staged_id":7,"session_id":"chat","turn_id":"B","text":"孩子指的是小明","entity_reference":"小明"}),
            serde_json::json!({"outcome": applied("New")}),
        );
        inject_legacy_receipt(
            &db,
            "c",
            serde_json::json!({"session_id":"chat","turn_id":"C","text":"小明还住在上海。"}),
            serde_json::json!({"outcomes":[applied("Reinforce")]}),
        );
        forget(&db, serde_json::json!({"source_id":"C"}));
        let expected = if with_pending_receipt {
            MemoryStatus::Active
        } else {
            MemoryStatus::Deleted
        };
        assert_eq!(
            status(&db, x),
            expected,
            "pending receipt: {with_pending_receipt}"
        );
        assert!(!source_deleted(&db, "A") && !source_deleted(&db, "B"));
    }
}

#[test]
fn f9_short_quotes_of_a_deleted_source_are_scrubbed_from_relation_text() {
    // 条件、时间里的 Quote 只是原文的短片段（“冬天”），不含任何整句也要清掉。
    let db = TestDb::new();
    let body = "冬天我住在北京。";
    let beijing = w5_new(&db, "s-501", "lives_in", "北京", body);
    claimed_source(&db, "s-502", "我住在上海。");
    let mut d = decision(0, W5Action::Conflict, &[beijing], &["s-501", "s-502"], None);
    d.relation = Some(W5RelationSpec {
        conditions: serde_json::json!([{"quote": {"text": "冬天"}, "resolved": true}]),
        times: serde_json::json!([{"quote": {"text": "冬天"}, "interp": {"kind": "unknown"}}]),
        basis: vec![evidence("s-501", body), evidence("s-502", "我住在上海。")],
    });
    let g = group(
        &db,
        "s-502#0",
        vec![(0, cand("s-502", "lives_in", "上海", "我住在上海。", 310))],
        vec![d],
    );
    let shanghai = w5_commit(&db, &g).products[&0];
    forget(&db, serde_json::json!({"source_id":"s-501"}));
    assert_eq!(status(&db, beijing), MemoryStatus::Deleted);
    assert_eq!(status(&db, shanghai), MemoryStatus::Active);
    assert_eq!(relation(&db, "rel-0").status, RelationStatus::Invalidated);
    assert_text_gone(&db, body);
    assert_text_gone(&db, "冬天");
    // 另一方的文字不受影响。
    assert_eq!(memory(&db, shanghai).evidence[0].quote, "我住在上海。");
}

#[test]
fn f10_a_still_pending_clarification_is_not_the_original_statement() {
    // A 陈述暂存 7 号；B 澄清后仍待定；C 再澄清创建 M。B 的回执键排在 A 之前。
    // 删 A：M 只剩澄清证据，删除。
    let db = TestDb::new();
    stored_source(&db, "A", "孩子住在上海。");
    stored_source(&db, "B", "孩子是说家里的");
    stored_source(&db, "C", "孩子指的是小明");
    let mut c = cand("A", "lives_in", "上海", "孩子住在上海。", 100);
    for (turn, quote) in [("B", "孩子是说家里的"), ("C", "孩子指的是小明")] {
        c.evidence.push(Evidence::new(
            EvidenceId::from(turn),
            SessionId::from("chat"),
            TurnId::from(turn),
            quote,
        ));
    }
    let m = seed(&db, &[c])[0];
    inject_legacy_receipt(
        &db,
        "z-a",
        serde_json::json!({"session_id":"chat","turn_id":"A","text":"孩子住在上海。"}),
        serde_json::json!({"outcomes":[{"Pending":{"staged_id":7,"resolution":"NotFound"}}]}),
    );
    inject_legacy_receipt(
        &db,
        "b",
        serde_json::json!({"staged_id":7,"session_id":"chat","turn_id":"B","text":"孩子是说家里的","entity_reference":"家里的"}),
        serde_json::json!({"outcome":{"Pending":{"staged_id":7,"resolution":"Ambiguous"}}}),
    );
    inject_legacy_receipt(
        &db,
        "c",
        serde_json::json!({"staged_id":7,"session_id":"chat","turn_id":"C","text":"孩子指的是小明","entity_reference":"小明"}),
        serde_json::json!({"outcome":{"Ingested":{"Applied":{"action":"New","memory_id":m,"target":null}}}}),
    );
    let result = forget(&db, serde_json::json!({"source_id":"A"}));
    assert_eq!(status(&db, m), MemoryStatus::Deleted, "{result}");
    assert_eq!(result["retained_ids"], serde_json::json!([]));
    assert!(!source_deleted(&db, "B") && !source_deleted(&db, "C"));
}

#[test]
fn f11_forgotten_values_are_tombstoned() {
    // 通用 fact 的值就是原句：软删除后只留 ID、状态、谱系与墓碑证据。
    let db = TestDb::new();
    let m = w5_new(&db, "s-1", "fact", SEVEN, SEVEN);
    let result = forget(&db, serde_json::json!({"memory_id": m.0}));
    assert_eq!(result["audit_evidence_retained"], true);
    let gone = memory(&db, m);
    assert_eq!(gone.status, MemoryStatus::Deleted);
    assert_eq!(gone.value, MemoryValue::Text("[deleted source]".into()));
    assert!(gone.evidence.iter().all(|e| e.quote == "[deleted source]"));
    assert_text_gone(&db, SEVEN);
}

#[test]
fn f12_forgotten_records_keep_no_content_fields() {
    // 条件、时间表达、标签都可能是原文片段（复审 0007 复现）。
    let db = TestDb::new();
    let body = "在锦瑟画室参加私人写生时，我住在北京。";
    claimed_source(&db, "secret-source", body);
    let mut c = cand("secret-source", "lives_in", "北京", body, 300);
    c.scope
        .conditions
        .push(ScopeCondition::Activity("锦瑟画室参加私人写生".into()));
    c.temporal.reference = Some(TemporalReference::Unknown {
        expression: "参加私人写生时".into(),
        source: TemporalSource::Unknown,
    });
    c.tags.push("锦瑟画室".into());
    let g = group(
        &db,
        "secret-source#0",
        vec![(0, c)],
        vec![decision(0, W5Action::New, &[], &["secret-source"], None)],
    );
    let id = w5_commit(&db, &g).products[&0];
    forget(&db, serde_json::json!({"source_id":"secret-source"}));
    let gone = memory(&db, id);
    assert_eq!(gone.status, MemoryStatus::Deleted);
    assert_eq!(gone.scope, Scope::default());
    assert!(gone.tags.is_empty() && gone.temporal.reference.is_none());
    assert_text_gone(&db, "锦瑟画室");
    assert_text_gone(&db, "参加私人写生时");
}

#[test]
fn f13_a_source_less_legacy_record_is_scrubbed_without_spreading() {
    // capture_source=false 的旧记录（契约 §2.6）：按 memory_id 删除时证据与旧回执里的文字也要清掉；
    // 同轮的另一条记录没有原文可整条删除，不受牵连。
    let db = TestDb::new();
    let body = "星港账户的备用口令提示是锦瑟。";
    let ids = seed(
        &db,
        &[
            cand("legacy-turn", "fact", body, body, 100),
            cand("legacy-turn", "likes", "星港", "星港", 100),
        ],
    );
    inject_legacy_receipt(
        &db,
        "legacy-receipt",
        serde_json::json!({"session_id":"chat","turn_id":"legacy-turn","text":body,"capture_source":false}),
        serde_json::json!({"outcomes":[{"Ingested":{"Applied":{"action":"New","memory_id":ids[0],"target":null}}}]}),
    );
    assert!(db.open().read().unwrap().sources.is_empty());
    let result = forget(&db, serde_json::json!({"memory_id": ids[0].0}));
    assert_eq!(result["forgotten_ids"], serde_json::json!([ids[0]]));
    assert_eq!(status(&db, ids[0]), MemoryStatus::Deleted);
    assert_eq!(status(&db, ids[1]), MemoryStatus::Active);
    assert_text_gone(&db, body);
}

#[test]
fn f14_an_unrelated_short_phrase_leaves_other_receipts_alone() {
    // 被删原文里的“冬天”与另一组关系条件里的“冬天”无关：那组回执不变，重放仍是 AlreadyApplied。
    let db = TestDb::new();
    let unrelated = w5_new(
        &db,
        "unrelated",
        "fact",
        "冬天我出门滑雪。",
        "冬天我出门滑雪。",
    );
    let body_b = "冬天我住在北京。";
    let beijing = w5_new(&db, "b", "lives_in", "北京", body_b);
    let body_c = "冬天我住在上海。";
    claimed_source(&db, "c", body_c);
    let mut d = decision(0, W5Action::Conflict, &[beijing], &["b", "c"], None);
    d.relation = Some(W5RelationSpec {
        conditions: serde_json::json!([{"quote": {"text": "冬天"}, "resolved": true}]),
        times: serde_json::json!([]),
        basis: vec![evidence("b", body_b), evidence("c", body_c)],
    });
    let g = group(
        &db,
        "c#0",
        vec![(0, cand("c", "lives_in", "上海", body_c, 310))],
        vec![d],
    );
    let before = committed(commit_group(&mut db.open(), &claim(&g), &g).unwrap());
    let result = forget(&db, serde_json::json!({"source_id":"unrelated"}));
    assert_eq!(result["forgotten_ids"], serde_json::json!([unrelated]));
    assert_eq!(result["invalidated_relation_ids"], serde_json::json!([]));
    let open = relation(&db, "rel-0");
    assert_eq!(open.status, RelationStatus::Open);
    assert_eq!(open.conditions[0]["quote"]["text"], "冬天");
    let after = db.open().read().unwrap().w5_receipts["c#0"].clone();
    assert_eq!(before.fingerprint, after.fingerprint);
    assert!(matches!(
        commit_group(&mut db.open(), &claim(&g), &g).unwrap(),
        W5Outcome::AlreadyApplied(_)
    ));
    assert_text_gone(&db, "冬天我出门滑雪。");
}

#[test]
fn f15_a_reinforce_from_the_creating_source_is_not_recounted() {
    // A 创建 M，同一原文另一组再强化 M（证据重复，计数不变）；B 独立强化后删除 B：计数回到 0。
    let db = TestDb::new();
    let body = "我住在北京。";
    claimed_source(&db, "A", body);
    let g = group(
        &db,
        "A#0",
        vec![(0, cand("A", "lives_in", "北京", body, 300))],
        vec![decision(0, W5Action::New, &[], &["A"], None)],
    );
    let m = committed(commit_group(&mut db.open(), &claim(&g), &g).unwrap()).products[&0];
    let g = group(
        &db,
        "A#1",
        vec![(1, cand("A", "lives_in", "北京", body, 300))],
        vec![decision(1, W5Action::Reinforce, &[m], &["A"], None)],
    );
    w5_commit(&db, &g);
    assert_eq!(memory(&db, m).reinforcement_count, 0);
    let other = "我还住在北京。";
    claimed_source(&db, "B", other);
    let g = group(
        &db,
        "B#0",
        vec![(0, cand("B", "lives_in", "北京", other, 500))],
        vec![decision(0, W5Action::Reinforce, &[m], &["B"], None)],
    );
    w5_commit(&db, &g);
    assert_eq!(memory(&db, m).reinforcement_count, 1);
    forget(&db, serde_json::json!({"source_id":"B"}));
    let kept = memory(&db, m);
    assert_eq!(kept.status, MemoryStatus::Active);
    assert_eq!(kept.reinforcement_count, 0);
    assert_eq!(kept.last_reinforced_at, None);
}

#[test]
fn f16_history_copies_of_a_source_less_message_are_removed() {
    // A 没有原文记录；无关消息 B 的请求历史里带着 A 的原话。删 A 的记录后历史副本也要去掉。
    let db = TestDb::new();
    let body = "星港账户的备用口令提示是锦瑟。";
    let a = seed(&db, &[cand("legacy-a", "fact", body, body, 100)])[0];
    inject_legacy_receipt(
        &db,
        "legacy-a",
        serde_json::json!({"session_id":"chat","turn_id":"legacy-a","text":body,"capture_source":false}),
        serde_json::json!({"outcomes":[{"Ingested":{"Applied":{"action":"New","memory_id":a,"target":null}}}]}),
    );
    stored_source(&db, "B", "今天天气不错。");
    edit_source(&db, "B", |s| {
        s.request.as_mut().unwrap()["history"] = serde_json::json!([body, "别的话"]);
    });
    forget(&db, serde_json::json!({"memory_id": a.0}));
    assert_text_gone(&db, body);
    assert_eq!(
        source(&db, "B").request.unwrap()["history"],
        serde_json::json!(["别的话"])
    );
}

#[test]
fn f17_a_surviving_legacy_reinforcement_keeps_its_receipt_time() {
    // M 由有原文的 A 创建，被无原文的旧消息 B 强化（旧回执带显式 observed_at）。删 A 后 M 由 B
    // 支持而保留，最近强化时间仍是 B 的时间。
    let db = TestDb::new();
    stored_source(&db, "A", "回答都必须使用中文。");
    let mut c = cand("A", "fact", "中文", "回答都必须使用中文。", 100);
    c.evidence.push(Evidence::new(
        EvidenceId::from("B"),
        SessionId::from("chat"),
        TurnId::from("B"),
        "以后所有回答都必须使用中文。",
    ));
    let m = seed(&db, &[c])[0];
    let applied = |action: &str| serde_json::json!({"Ingested":{"Applied":{"action":action,"memory_id":m,"target":null}}});
    inject_legacy_receipt(
        &db,
        "a",
        serde_json::json!({"session_id":"chat","turn_id":"A","text":"回答都必须使用中文。"}),
        serde_json::json!({"outcomes":[applied("New")]}),
    );
    inject_legacy_receipt(
        &db,
        "b",
        serde_json::json!({"session_id":"chat","turn_id":"B","text":"以后所有回答都必须使用中文。",
            "capture_source":false,"observed_at":1_700_000_100}),
        serde_json::json!({"outcomes":[applied("Reinforce")]}),
    );
    forget(&db, serde_json::json!({"source_id":"A"}));
    let kept = memory(&db, m);
    assert_eq!(kept.status, MemoryStatus::Active);
    assert_eq!(kept.reinforcement_count, 1);
    assert_eq!(kept.last_reinforced_at, Some(Timestamp(1_700_000_100)));
}
