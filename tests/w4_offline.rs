//! B 段离线验收：W4 代码侧（输入组装 → 输出校验 → 转换为 W5 组 → 真实提交）跑 23 组样本。
//!
//! 不调用模型。每个样本按其输入在真实 SQLite 里建库，运行组装并与样本输入比对，再把
//! `tests/eval/w4/fixtures/` 里的模型输出夹具（由期望决定生成，样本本身不改）交给校验与转换，
//! 提交后重新打开数据库，对 `expect.after` 做可执行断言。
//!
//! 范围说明：
//! - `after.recall`（历史资格、条件适用、唯一当前结论）属于 recall.v3，不在这里验收。
//! - 样本的 `must_not` 是自然语言；能落到持久状态的由下面的字段断言覆盖，个别在
//!   `extra_checks` 里单独断言。
//! - 样本迁移到 w4.v2 后标签为 draft，等用户复核；复核前本测试的结论只说明代码按当前标签
//!   行为一致，不作为验收通过的依据。
//! - w4_09 不比对组装结果（用户同意的例外）：样本为了测试“检索到无关记录时判为 new”，
//!   送入了一条按取数规则取不到的其他关系记录。该样本直接用样本输入验证校验、转换与提交。

use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use hippocampus::contract::*;
use hippocampus::w4::*;
use hippocampus::*;
use serde_json::{Value as Json, json};

static TEST_ID: AtomicU64 = AtomicU64::new(0);
const OWNER: &str = "user-1";
const ATTEMPT: &str = "attempt-1";
const BUDGET: W4Budget = W4Budget {
    facts: 8,
    records: 64,
};

struct Db(PathBuf);

impl Db {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "pmr-w4-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            TEST_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(directory.join("private")).unwrap();
        Self(directory.join("private/memory.sqlite3"))
    }

    fn open(&self) -> SqliteMemoryStore {
        SqliteMemoryStore::open(&self.0, UserId::from(OWNER)).unwrap()
    }

    fn state(&self) -> PersistedMemoryState {
        self.open().read().unwrap()
    }
}

impl Drop for Db {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.0.parent().unwrap().parent().unwrap());
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn samples() -> Vec<Json> {
    let mut out: Vec<Json> = std::fs::read_dir("tests/eval/w4/samples")
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .map(|path| serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap())
        .collect();
    out.sort_by_key(|sample: &Json| sample["id"].as_str().unwrap().to_owned());
    out
}

fn sample(id: &str) -> Json {
    samples()
        .into_iter()
        .find(|sample| sample["id"] == id)
        .unwrap_or_else(|| panic!("no sample {id}"))
}

fn fixture(id: &str) -> Json {
    let path = format!("tests/eval/w4/fixtures/{id}.output.json");
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}")))
        .unwrap()
}

/// 可信任务：只有 w4_19 绑定任务（样本 note）。
fn task_of(id: &str) -> Option<TaskId> {
    (id == "w4_19_task_scope").then(|| TaskId::from("s6-tide-exhibit"))
}

/// 写入一条已认领、正在整理的原文。接收顺序按调用顺序递增。
fn claim_source(db: &Db, sid: &str, text: &str) {
    db.open()
        .transaction(|state| {
            let sequence = state
                .sources
                .values()
                .map(|s| s.received_sequence)
                .max()
                .unwrap_or(0)
                .max(state.last_applied_source_sequence)
                + 1;
            state.sources.insert(
                sid.into(),
                SourceRecord {
                    id: sid.into(),
                    session_id: "chat".into(),
                    turn_id: sid.into(),
                    observed_at: 1_700_000_000 + sequence as i64,
                    received_sequence: sequence,
                    request: Some(json!({"text": text, "capture_source": true})),
                    enrichment: "running".into(),
                    error: None,
                    result: None,
                    base_revision: state.revision,
                    index: Json::Null,
                    task: EnrichmentTask {
                        processing_version: Some("w4-offline".into()),
                        attempt_count: 1,
                        attempt_id: Some(ATTEMPT.into()),
                        lease_until: Some(now() + 900),
                        claim_revision: Some(state.revision),
                        accepted_mode: Some("inline".into()),
                    },
                },
            );
            Ok(())
        })
        .unwrap();
}

fn claim(sid: &str) -> W5Claim {
    W5Claim {
        source_key: sid.into(),
        attempt_id: ATTEMPT.into(),
    }
}

/// 把下一个分配的记录 ID 调到 `id`（样本用固定编号）。
fn next_id_is(db: &Db, id: u64) {
    db.open()
        .transaction(|state| {
            loop {
                let current = serde_json::to_value(&state.repository)?["next_id"]
                    .as_u64()
                    .unwrap();
                assert!(current < id, "records must be seeded in ascending id order");
                if current + 1 == id {
                    return Ok(());
                }
                state.repository.allocate_id()?;
            }
        })
        .unwrap();
}

fn runtime<'a>(
    state: &'a PersistedMemoryState,
    sid: &'a str,
    round: u32,
    task_id: Option<TaskId>,
) -> W4Runtime<'a> {
    W4Runtime {
        state,
        owner: UserId::from(OWNER),
        source_key: sid,
        round,
        task_id,
        admissions: BTreeMap::new(),
    }
}

/// 经真实流程提交若干候选为新记录（不调用 W4 的路径），返回各候选的产物。
fn commit_new(
    db: &Db,
    sid: &str,
    round: u32,
    task_id: Option<TaskId>,
    candidates: Vec<W4Candidate>,
    complete: bool,
) -> Vec<MemoryId> {
    let input = W4Input {
        candidates: candidates.clone(),
        records: vec![],
        relations: vec![],
        coverage: W4Coverage {
            complete,
            omitted: 0,
        },
    };
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
    let state = db.state();
    let plan = plan(&runtime(&state, sid, round, task_id), &input, &output).unwrap();
    assert!(plan.held.is_empty(), "{:?}", plan.held);
    let mut products = Vec::new();
    for group in &plan.groups {
        match commit_group(&mut db.open(), &claim(sid), group).unwrap() {
            W5Outcome::Committed(receipt) => products.extend(receipt.products.values().copied()),
            other => panic!("seed commit of {sid}: {other:?}"),
        }
    }
    products
}

/// 按样本输入建库：原文、完整记录（经真实提交，带完整候选回执）、旧记录（原样写入）、关系。
/// 返回候选所在原文的 ID。
fn seed(db: &Db, sample: &Json) -> String {
    let id = sample["id"].as_str().unwrap();
    let input: W4Input = serde_json::from_value(sample["input"].clone()).unwrap();
    let text = |sid: &str| sample["sources"][sid]["text"].as_str().unwrap().to_owned();
    let own = match &input.candidates[0].source {
        SourceRef::Stored { source_id } => source_id.clone(),
        SourceRef::Current {} => panic!("{id}: candidate from current"),
    };
    let mut records: Vec<&RecordView2> = input.records.iter().collect();
    records.sort_by_key(|r| r.id());
    for record in records {
        match record {
            RecordView2::Current(view) => {
                assert_eq!(view.stored_status, StoredStatus::Active, "{id}");
                let SourceRef::Stored { source_id } = &view.evidence[0].source else {
                    panic!("{id}: record evidence");
                };
                next_id_is(db, view.id.0);
                claim_source(db, source_id, &text(source_id));
                let products = commit_new(
                    db,
                    source_id,
                    0,
                    task_of(id),
                    vec![W4Candidate {
                        index: 0,
                        committed_id: None,
                        candidate: view.candidate.clone(),
                        source: view.evidence[0].source.clone(),
                        said_on: view.said_on.clone(),
                        time_resolutions: view.time_resolutions.clone(),
                    }],
                    view.update.status == UpdateStatus::Judged,
                );
                assert_eq!(products, vec![view.id], "{id}");
                w5_finish_task(&mut db.open(), &claim(source_id), &W5TaskEnd::Committed).unwrap();
                db.open()
                    .transaction(|state| {
                        state.record_updates.insert(view.id, view.update.clone());
                        Ok(())
                    })
                    .unwrap();
            }
            RecordView2::Legacy(view) => {
                next_id_is(db, view.id.0);
                db.open()
                    .transaction(|state| {
                        assert_eq!(state.repository.allocate_id()?, view.id);
                        state.repository.insert(view.data.clone())?;
                        Ok(())
                    })
                    .unwrap();
            }
        }
    }
    if !input.relations.is_empty() {
        db.open()
            .transaction(|state| {
                for link in &input.relations {
                    let evidence = |e: &EvidenceRef| RelationEvidence {
                        source_id: match &e.source {
                            SourceRef::Stored { source_id } => source_id.clone(),
                            SourceRef::Current {} => unreachable!(),
                        },
                        quote: e.quote.text.clone(),
                    };
                    state.relations.insert(
                        link.id.clone(),
                        StoredRelation {
                            id: link.id.clone(),
                            kind: hippocampus::RelationKind::Conflict,
                            members: link.members.clone(),
                            conditions: serde_json::to_value(&link.conditions)?,
                            times: serde_json::to_value(&link.times)?,
                            basis: link.basis.iter().map(evidence).collect(),
                            status: match link.status {
                                hippocampus::contract::RelationStatus::Open => {
                                    hippocampus::RelationStatus::Open
                                }
                                hippocampus::contract::RelationStatus::Resolved => {
                                    hippocampus::RelationStatus::Resolved
                                }
                                hippocampus::contract::RelationStatus::Invalidated => {
                                    hippocampus::RelationStatus::Invalidated
                                }
                            },
                            resolution: link.resolution.as_ref().map(evidence),
                            created_by: "seed".into(),
                            closed_by: None,
                        },
                    );
                    let number: u64 = link.id.trim_start_matches("rel-").parse().unwrap();
                    state.next_relation = state.next_relation.max(number + 1);
                }
                Ok(())
            })
            .unwrap();
    }
    // w4_19：另一个任务里的同主体记录，必须不进入输入、不被改动。
    if id == "w4_19_task_scope" {
        next_id_is(db, 98);
        claim_source(
            db,
            "s-other-task",
            "在高原路演任务中，折页导航终端的开机显示语言设为意大利语。",
        );
        let mut other = input.candidates[0].clone();
        other.candidate.sentence.text =
            "在高原路演任务中，折页导航终端的开机显示语言设为意大利语。".into();
        other.candidate.value = Some(hippocampus::contract::Value::Text {
            text: other.candidate.sentence.text.clone(),
            quote: other.candidate.sentence.clone(),
        });
        other.source = SourceRef::Stored {
            source_id: "s-other-task".into(),
        };
        let products = commit_new(
            db,
            "s-other-task",
            0,
            Some(TaskId::from("s6-plateau-demo")),
            vec![other],
            false,
        );
        assert_eq!(products, vec![MemoryId(98)]);
        w5_finish_task(
            &mut db.open(),
            &claim("s-other-task"),
            &W5TaskEnd::Committed,
        )
        .unwrap();
    }
    // 候选所在原文：最后认领，接收顺序最大。重判的候选先按首次提交写入（范围不完整）。
    if let Some(committed) = input.candidates.iter().find_map(|c| c.committed_id) {
        next_id_is(db, committed.0);
    }
    claim_source(db, &own, &text(&own));
    for candidate in &input.candidates {
        if let Some(committed) = candidate.committed_id {
            let mut first = candidate.clone();
            first.committed_id = None;
            let products = commit_new(db, &own, 0, task_of(id), vec![first], false);
            assert_eq!(products, vec![committed], "{id}");
        }
    }
    // 建库用的无来源事务会触发顺序屏障：这里只验证 W4 代码侧，把候选原文恢复为可整理。
    db.open()
        .transaction(|state| {
            state.source_less_barrier_sequence = 0;
            let task = state.sources.get_mut(&own).unwrap();
            task.enrichment = "running".into();
            task.error = None;
            Ok(())
        })
        .unwrap();
    own
}

fn sorted_records(mut input: Json) -> Json {
    if let Some(records) = input["records"].as_array_mut() {
        records.sort_by_key(|r| r["id"].as_u64());
    }
    input
}

struct Run {
    db: Db,
    sample: Json,
    before: PersistedMemoryState,
    after: PersistedMemoryState,
    receipts: Vec<W5Receipt>,
    called_w4: bool,
    held: BTreeMap<usize, Held>,
}

impl Run {
    fn product(&self, candidate: usize) -> Option<MemoryId> {
        self.receipts
            .iter()
            .find_map(|receipt| receipt.products.get(&candidate).copied())
    }
    fn update_of(&self, candidate: usize) -> Option<UpdateState> {
        self.receipts
            .iter()
            .find_map(|receipt| receipt.updates.get(&candidate).cloned())
    }
}

/// 建库 → 组装 → （夹具输出或跳过路径）→ 校验与转换 → 提交 → 重开。
fn run(id: &str) -> Run {
    run_with(sample(id), None)
}

/// 同上，但样本内容和模型输出可以是在已有样本上改出的变体。
fn run_with(sample: Json, output: Option<Json>) -> Run {
    let id = sample["id"].as_str().unwrap().to_owned();
    let id = id.as_str();
    let fixture = |name: &str| output.clone().unwrap_or_else(|| fixture(name));
    let db = Db::new();
    let own = seed(&db, &sample);
    let input: W4Input = serde_json::from_value(sample["input"].clone()).unwrap();
    let before = db.state();
    let rt = runtime(&before, &own, 1, task_of(id));
    let pending: Vec<Pending> = input
        .candidates
        .iter()
        .map(|c| Pending {
            index: c.index,
            committed_id: c.committed_id,
            contract: ContractCandidate {
                candidate: c.candidate.clone(),
                source: c.source.clone(),
                said_on: c.said_on.clone(),
                time_resolutions: c.time_resolutions.clone(),
            },
        })
        .collect();
    let assembly = assemble(&rt, &pending, BUDGET).unwrap();
    let (w4_input, output, called_w4) = if id == "w4_09_new" {
        // 用户同意的例外：不比对组装结果，直接用样本输入。
        assert!(!assembly.call_w4, "the unrelated record is not retrieved");
        (
            input.clone(),
            serde_json::from_value(fixture(id)).unwrap(),
            true,
        )
    } else {
        assert_eq!(
            sorted_records(serde_json::to_value(&assembly.input).unwrap()),
            sorted_records(sample["input"].clone()),
            "{id}: assembled input"
        );
        assert_eq!(assembly.call_w4, sample["call_w4"], "{id}: call_w4");
        if assembly.call_w4 {
            let output: W4Output = serde_json::from_value(fixture(id)).unwrap();
            (assembly.input.clone(), output, true)
        } else {
            (assembly.input.clone(), assembly.skipped.clone(), false)
        }
    };
    let plan = plan(&rt, &w4_input, &output).unwrap_or_else(|e| panic!("{id}: {e}"));
    let mut receipts = Vec::new();
    for group in &plan.groups {
        match commit_group(&mut db.open(), &claim(&own), group)
            .unwrap_or_else(|e| panic!("{id}: {e}"))
        {
            W5Outcome::Committed(receipt) => receipts.push(receipt),
            other => panic!("{id}: {other:?}"),
        }
    }
    w5_finish_task(&mut db.open(), &claim(&own), &W5TaskEnd::Committed).unwrap();
    // 重新打开数据库再核对。
    let after = db.state();
    Run {
        db,
        sample,
        before,
        after,
        receipts,
        called_w4,
        held: plan.held,
    }
}

fn memory(state: &PersistedMemoryState, id: MemoryId) -> Memory {
    state
        .repository
        .get(id)
        .unwrap()
        .unwrap_or_else(|| panic!("no record {}", id.0))
}

fn lower(value: impl serde::Serialize) -> Json {
    serde_json::to_value(value).unwrap()
}

/// 更新状态的原因是集合（contracts 1.6：无重复，不规定顺序）：按排序后的原因比较。
fn update_json(mut value: Json) -> Json {
    if let Some(reasons) = value.get_mut("reasons").and_then(Json::as_array_mut) {
        reasons.sort_by_key(|reason| reason.as_str().unwrap_or_default().to_owned());
    }
    value
}

/// `expect.after` 里能落到持久状态的断言。
fn check_after(run: &Run) {
    let id = run.sample["id"].as_str().unwrap();
    let after = &run.sample["expect"]["after"];
    let state = &run.after;
    let resolve = |key: &str, spec: &Json| -> Option<MemoryId> {
        match key.parse::<u64>() {
            Ok(number) => Some(MemoryId(number)),
            Err(_) => run.product(spec["from_candidate"].as_u64().unwrap() as usize),
        }
    };
    for (key, spec) in after["records"].as_object().into_iter().flatten() {
        let tag = format!("{id} record {key}");
        let record = resolve(key, spec).unwrap_or_else(|| panic!("{tag}: no product"));
        if let Some(from) = spec.get("from_candidate")
            && key.parse::<u64>().is_ok()
            && spec.get("merged_into").is_none()
        {
            assert_eq!(
                run.product(from.as_u64().unwrap() as usize),
                Some(record),
                "{tag}: the committed record is reused"
            );
        }
        let stored = memory(state, record);
        let view = record_view(state, record);
        for (field, expected) in spec.as_object().unwrap() {
            match field.as_str() {
                "from_candidate" | "note" => {}
                "stored_status" => assert_eq!(
                    lower(stored.status).as_str().unwrap().to_lowercase(),
                    expected.as_str().unwrap(),
                    "{tag}"
                ),
                "status" => assert_eq!(&lower(stored.status), expected, "{tag}"),
                "termination_reason" => {
                    assert_eq!(&lower(stored.supersede_reason), expected, "{tag}")
                }
                "ended" => match view.as_ref().unwrap_or_else(|| panic!("{tag}: no view")) {
                    RecordView2::Current(view) => assert_eq!(&lower(view.ended), expected, "{tag}"),
                    RecordView2::Legacy(_) => panic!("{tag}: ended on a legacy view"),
                },
                "history" => match view.as_ref().unwrap() {
                    RecordView2::Current(view) => {
                        assert_eq!(&lower(&view.history), expected, "{tag}")
                    }
                    RecordView2::Legacy(_) => panic!("{tag}: history on a legacy view"),
                },
                "update" => assert_eq!(
                    update_json(lower(state.record_updates.get(&record))),
                    update_json(expected.clone()),
                    "{tag}"
                ),
                "open_relations" => assert_eq!(
                    w5_open_relations_of(state, record).len() as u64,
                    expected.as_u64().unwrap(),
                    "{tag}"
                ),
                "reinforcement_count" => {
                    assert_eq!(expected, "+1", "{tag}");
                    assert_eq!(
                        stored.reinforcement_count,
                        memory(&run.before, record).reinforcement_count + 1,
                        "{tag}"
                    );
                }
                "evidence_added" => {
                    let old: BTreeSet<String> = memory(&run.before, record)
                        .evidence
                        .iter()
                        .map(|e| e.turn_id.0.clone())
                        .collect();
                    let added: Vec<String> = stored
                        .evidence
                        .iter()
                        .map(|e| e.turn_id.0.clone())
                        .filter(|turn| !old.contains(turn))
                        .collect();
                    assert_eq!(&json!(added), expected, "{tag}");
                }
                "conditions" => {
                    let contract = w5_record_contract(state, record).unwrap();
                    let texts: Vec<&str> = contract
                        .candidate
                        .conditions
                        .iter()
                        .map(|c| c.quote.text.as_str())
                        .collect();
                    assert_eq!(&json!(texts), expected, "{tag}");
                }
                "strength" => assert_eq!(
                    &lower(
                        w5_record_contract(state, record)
                            .unwrap()
                            .candidate
                            .strength
                    ),
                    expected,
                    "{tag}"
                ),
                "uncertainty" => assert_eq!(
                    &lower(
                        w5_record_contract(state, record)
                            .unwrap()
                            .candidate
                            .uncertainty
                    ),
                    expected,
                    "{tag}"
                ),
                "valid_to" => assert_eq!(&lower(stored.temporal.valid_to), expected, "{tag}"),
                "format" => {
                    assert_eq!(expected, "legacy", "{tag}");
                    assert!(matches!(view, Some(RecordView2::Legacy(_))), "{tag}");
                }
                "merged_into" => {
                    let into = MemoryId(expected.as_u64().unwrap());
                    assert!(
                        run.receipts
                            .iter()
                            .any(|r| r.merged.get(&record) == Some(&into)),
                        "{tag}"
                    );
                }
                "projected" => {
                    assert_eq!(expected, false, "{tag}");
                    assert_eq!(stored.status, MemoryStatus::Deleted, "{tag}");
                    assert!(record_view(state, record).is_none(), "{tag}");
                }
                other => panic!("{tag}: unchecked field {other}"),
            }
        }
    }
    let count = |state: &PersistedMemoryState| {
        state
            .repository
            .list_by_owner(&UserId::from(OWNER))
            .unwrap()
            .len()
    };
    for (field, expected) in after.as_object().unwrap() {
        match field.as_str() {
            "records" => {}
            // recall.v3 的范围，不在这里验收。
            "recall" => {}
            "new_records" => assert_eq!(
                (count(state) - count(&run.before)) as u64,
                expected.as_u64().unwrap(),
                "{id}: new records"
            ),
            "candidate_update" => assert_eq!(
                update_json(lower(run.update_of(0))),
                update_json(expected.clone()),
                "{id}: candidate update"
            ),
            "candidate_status" => {
                assert_eq!(expected, "pending", "{id}");
                assert_eq!(
                    run.product(0),
                    None,
                    "{id}: a pending retraction writes nothing"
                );
            }
            "relations" => {
                for spec in expected.as_array().unwrap() {
                    let relation = match spec.get("id") {
                        Some(relation) => state.relations[relation.as_str().unwrap()].clone(),
                        None => {
                            let members: Vec<MemoryId> = spec["members"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .map(|m| match m.as_u64() {
                                    Some(number) => MemoryId(number),
                                    None => run.product(0).unwrap(),
                                })
                                .collect();
                            state
                                .relations
                                .values()
                                .find(|r| r.members == members)
                                .unwrap_or_else(|| panic!("{id}: no relation over {members:?}"))
                                .clone()
                        }
                    };
                    assert_eq!(
                        lower(relation.status),
                        spec["status"],
                        "{id}: {}",
                        relation.id
                    );
                    if let Some(source) = spec.get("resolution_source") {
                        assert_eq!(
                            &json!(relation.resolution.as_ref().unwrap().source_id),
                            source,
                            "{id}"
                        );
                    }
                    for survivor in spec["survivors"].as_array().into_iter().flatten() {
                        let survivor = match survivor.as_u64() {
                            Some(number) => MemoryId(number),
                            None => run.product(0).unwrap(),
                        };
                        assert_eq!(
                            memory(state, survivor).status,
                            MemoryStatus::Active,
                            "{id}: survivor {survivor:?}"
                        );
                    }
                    // 非保留成员都已由本次决定结束。
                    if relation.status == hippocampus::RelationStatus::Resolved {
                        let active = relation
                            .members
                            .iter()
                            .filter(|m| memory(state, **m).status == MemoryStatus::Active)
                            .count();
                        assert!(active <= 1, "{id}: {active} members still current");
                    }
                }
                if expected.as_array().unwrap().is_empty() {
                    assert!(state.relations.is_empty(), "{id}: no relation is created");
                }
            }
            "receipt" => {
                let map: BTreeMap<String, u64> = run
                    .receipts
                    .iter()
                    .flat_map(|r| r.merged.iter())
                    .map(|(from, into)| (from.0.to_string(), into.0))
                    .collect();
                assert_eq!(&json!(map), &expected["replacement_map"], "{id}");
            }
            "untouched_other_task" => {
                assert_eq!(
                    lower(memory(state, MemoryId(98))),
                    lower(memory(&run.before, MemoryId(98))),
                    "{id}: the other task's record is untouched"
                );
            }
            other => panic!("{id}: unchecked expectation {other}"),
        }
    }
    if let Some(path) = run.sample["expect"].get("code_path") {
        assert_eq!(path["w4_called"], run.called_w4, "{id}");
        match path["memory_id"].as_str() {
            Some("new") => assert!(run.product(0).is_some(), "{id}"),
            _ => assert_eq!(run.product(0), None, "{id}"),
        }
        let mut expected: Vec<String> =
            serde_json::from_value(path["update"]["reasons"].clone()).unwrap();
        expected.sort();
        let update = run.update_of(0).unwrap();
        let mut reasons: Vec<String> = serde_json::from_value(lower(&update.reasons)).unwrap();
        reasons.sort();
        assert_eq!(lower(update.status), path["update"]["status"], "{id}");
        assert_eq!(reasons, expected, "{id}");
    }
    assert!(run.held.is_empty(), "{id}: {:?}", run.held);
}

#[test]
fn every_sample_assembles_validates_commits_and_matches_its_expected_state() {
    let ids: Vec<String> = samples()
        .iter()
        .map(|s| s["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(ids.len(), 23);
    for id in ids {
        let run = run(&id);
        check_after(&run);
        extra_checks(&run);
    }
}

/// 个别 `must_not` 与样本说明里、字段断言没有覆盖的持久状态。
fn extra_checks(run: &Run) {
    let id = run.sample["id"].as_str().unwrap();
    let state = &run.after;
    match id {
        "w4_01_state_change" => {
            // 结束时间取 effective：2026 年 8 月、月精度；不回填接收日，也不补成具体日。
            let Some(RecordView2::Current(view)) = record_view(state, MemoryId(17)) else {
                panic!("{id}: view of 17");
            };
            let end = view.end.expect("known end");
            assert_eq!(end.precision, Precision::Month);
            assert_eq!(
                (end.start, end.end_exclusive),
                (Some(1785513600), Some(1788192000))
            );
            assert_eq!(memory(state, MemoryId(17)).temporal.valid_to, None);
            assert_eq!(
                memory(state, MemoryId(17)).supersede_reason,
                Some(SupersedeReason::StateChanged)
            );
        }
        "w4_04_coexist_partial" => {
            // 不得结束 31。
            assert_eq!(memory(state, MemoryId(31)).status, MemoryStatus::Active);
        }
        "w4_07_conflict" => {
            // 不得结束或取代 50；新记录保留不确定表达。
            assert_eq!(memory(state, MemoryId(50)).status, MemoryStatus::Active);
            let new = run.product(0).unwrap();
            assert!(
                w5_record_contract(state, new)
                    .unwrap()
                    .candidate
                    .uncertainty
                    .is_some()
            );
        }
        "w4_13_state_change_date_unknown" => {
            // 结束时间未知：不回填接收时间。
            let Some(RecordView2::Current(view)) = record_view(state, MemoryId(64)) else {
                panic!("{id}: view of 64");
            };
            assert_eq!((view.ended, view.end), (Ended::EndUnknown, None));
        }
        "w4_21_confirm_conflict_side" => {
            // 同一原文的两个决定在同一组、同一事务里提交。
            assert_eq!(run.receipts.len(), 1);
            assert_eq!(run.receipts[0].decisions.len(), 2);
        }
        "w4_22_replace_conflict_members" => {
            // 不由代码自动解决 rel-2，成员不机械换成替代记录。
            let relation = &state.relations["rel-2"];
            assert_eq!(relation.status, hippocampus::RelationStatus::Open);
            assert_eq!(relation.members, vec![MemoryId(110), MemoryId(111)]);
        }
        _ => {}
    }
    let _ = &run.db;
}

// ---------------- 变体：现有样本没有覆盖的结束时间、冲突范围、分组与准入

/// w4_01 的变体：把“上个月”换成给定的时间表达与换算结果。
fn w4_01_with_time(interp: Json, resolution: Json) -> Run {
    let mut sample = sample("w4_01_state_change");
    let time = json!({"role": "event", "quote": {"text": "上个月"}, "interp": interp, "anchor": "said_on"});
    sample["input"]["candidates"][0]["candidate"]["times"] = json!([time]);
    sample["input"]["candidates"][0]["time_resolutions"] = json!([resolution]);
    let mut output = fixture("w4_01_state_change");
    output["decisions"][0]["effective"] = time;
    run_with(sample, Some(output))
}

#[test]
fn the_end_time_keeps_its_precision_and_only_a_single_day_becomes_a_point() {
    let day = 86_400;
    let start = 1_785_513_600;
    let view_of_17 = |run: &Run| match record_view(&run.after, MemoryId(17)) {
        Some(RecordView2::Current(view)) => view,
        other => panic!("{other:?}"),
    };
    // 恰好一天：同时写旧 Memory 的结束时间点。
    let run = w4_01_with_time(
        json!({"kind": "relative", "unit": "day", "offset": -1}),
        json!({"status": "resolved", "start": start, "end_exclusive": start + day, "precision": "day"}),
    );
    let view = view_of_17(&run);
    assert_eq!(view.ended, Ended::Known);
    assert_eq!(view.end.unwrap().precision, Precision::Day);
    assert_eq!(
        memory(&run.after, MemoryId(17))
            .temporal
            .valid_to
            .map(|v| v.at),
        Some(Timestamp(start))
    );
    // 多日区间：结束时间已知，但不折成一个点。
    let run = w4_01_with_time(
        json!({"kind": "range", "start": "2026-08-01", "end_inclusive": "2026-08-14"}),
        json!({"status": "resolved", "start": start, "end_exclusive": start + 14 * day, "precision": "day"}),
    );
    let view = view_of_17(&run);
    assert_eq!(view.ended, Ended::Known);
    assert_eq!(
        view.end.as_ref().unwrap().end_exclusive,
        Some(start + 14 * day)
    );
    assert_eq!(memory(&run.after, MemoryId(17)).temporal.valid_to, None);
    // 无法换算：结束时间未知，不补造。
    let run = w4_01_with_time(
        json!({"kind": "unknown"}),
        json!({"status": "unknown", "start": null, "end_exclusive": null, "precision": "unknown"}),
    );
    let view = view_of_17(&run);
    assert_eq!((view.ended, view.end), (Ended::EndUnknown, None));
    assert_eq!(memory(&run.after, MemoryId(17)).temporal.valid_to, None);
}

#[test]
fn a_month_precision_end_is_not_an_open_ended_fact_on_the_legacy_read_paths() {
    // w4_01：北京结束于 2026 年 8 月（月精度），旧 Memory 没有 valid_to。旧读取路径不能把它
    // 当作没有上界的历史事实：管理视图带出结束时间，答复门把它当作限定语无法表达的记录。
    let run = run("w4_01_state_change");
    let mut service =
        hippocampus::chat::ChatMemoryService::new(run.db.open(), Default::default()).unwrap();
    let listed = service
        .call("memory_list", json!({"include_history": true}), || false)
        .unwrap();
    let item = listed["memories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == 17)
        .unwrap_or_else(|| panic!("{listed}"));
    assert_eq!(
        item["qualifiers_unmapped"]["end"]["precision"], "month",
        "{item}"
    );
}

#[test]
fn a_conflict_scope_copies_the_referenced_qualifiers_into_the_relation() {
    // w4_07 的变体：新候选带条件“之前记的”，W4 引用它作为冲突的适用范围。
    let mut sample = sample("w4_07_conflict");
    let condition = json!({"quote": {"text": "之前记的"}, "resolved": true});
    sample["input"]["candidates"][0]["candidate"]["conditions"] = json!([condition]);
    let mut output = fixture("w4_07_conflict");
    output["decisions"][0]["relation_scope"] =
        json!({"conditions": [{"of": "candidate", "index": 0}], "times": []});
    let run = run_with(sample, Some(output));
    let relation = run.after.relations.values().next().unwrap();
    assert_eq!(relation.conditions, json!([condition]));
    assert_eq!(relation.times, json!([]));
    // 引用关系随决定回执保存，重开后仍在。
    let scope = run.receipts[0].decisions[0].relation_scope.clone().unwrap();
    assert_eq!(scope.conditions[0].of, QualifierOwner::Candidate);
    let stored = &run.after.w5_receipts[&run.receipts[0].group_id];
    assert_eq!(stored.decisions[0].relation_scope, Some(scope));
    assert_eq!(stored.decisions[0].basis.len(), 2);
}

/// w4_08（没有相关记录）的原文与候选，用来构造多候选的转换输入。
fn two_unrelated_candidates(second_type: &str) -> (Db, String, W4Input, W4Output) {
    let sample = sample("w4_08_scope_incomplete_no_records");
    let db = Db::new();
    let own = seed(&db, &sample);
    let mut input: W4Input = serde_json::from_value(sample["input"].clone()).unwrap();
    let mut second = input.candidates[0].clone();
    second.index = 1;
    second.candidate.memory_type = second_type.into();
    input.candidates.push(second);
    let output = W4Output {
        decisions: input
            .candidates
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
    (db, own, input, output)
}

#[test]
fn independent_decisions_form_separate_groups_and_the_round_is_part_of_the_group_id() {
    let (db, own, input, output) = two_unrelated_candidates("fact");
    let state = db.state();
    let first = plan(&runtime(&state, &own, 1, None), &input, &output).unwrap();
    let ids: Vec<&str> = first.groups.iter().map(|g| g.id.as_str()).collect();
    assert_eq!(ids, [format!("{own}#1#0"), format!("{own}#1#1")]);
    // 同一轮重试得到相同的组；重判换轮次，组 ID 不同，不会撞上旧回执。
    assert_eq!(
        plan(&runtime(&state, &own, 1, None), &input, &output).unwrap(),
        first
    );
    let again = plan(&runtime(&state, &own, 2, None), &input, &output).unwrap();
    assert_eq!(again.groups[0].id, format!("{own}#2#0"));
}

#[test]
fn an_episode_enters_w5_only_with_a_reason_to_keep_it() {
    let (db, own, input, output) = two_unrelated_candidates("episode");
    let state = db.state();
    // 没有跨轮价值依据：只保留原文，不进入 W5；同批的普通事实不受影响。
    let plain = plan(&runtime(&state, &own, 1, None), &input, &output).unwrap();
    assert_eq!(plain.held, BTreeMap::from([(1, Held::EpisodeNotAdmitted)]));
    assert_eq!(plain.groups.len(), 1);
    for admission in [
        Admission {
            episode_useful: true,
            remember_bound: false,
        },
        Admission {
            episode_useful: false,
            remember_bound: true,
        },
    ] {
        let mut rt = runtime(&state, &own, 1, None);
        rt.admissions.insert(1, admission);
        let admitted = plan(&rt, &input, &output).unwrap();
        assert!(admitted.held.is_empty());
        assert_eq!(admitted.groups.len(), 2);
        assert_eq!(
            admitted.groups[1].candidates[0]
                .candidate
                .explicit_memory_request,
            admission.remember_bound
        );
    }
}

// ---------------- 反例：不合规的 W4 输出整份拒绝

fn rejected(id: &str, edit: impl FnOnce(&mut Json, &mut Json)) -> String {
    let mut sample = sample(id);
    let mut output = fixture(id);
    edit(&mut sample, &mut output);
    let input: W4Input = serde_json::from_value(sample["input"].clone()).unwrap();
    let Ok(output) = serde_json::from_value::<W4Output>(output) else {
        return "decode".into();
    };
    let sources = sample["sources"].clone();
    let text_of = |sid: &str| sources[sid]["text"].as_str().map(str::to_owned);
    match validate(&input, &output, &text_of) {
        Ok(_) => panic!("{id}: the broken output was accepted"),
        Err(error) => error.to_string(),
    }
}

#[test]
fn invalid_outputs_are_rejected_as_a_whole() {
    let cases: Vec<(&str, &str, Box<dyn FnOnce(&mut Json, &mut Json)>, &str)> = vec![
        (
            "重复决定",
            "w4_01_state_change",
            Box::new(|_, o| {
                let d = o["decisions"][0].clone();
                o["decisions"].as_array_mut().unwrap().push(d);
            }),
            "恰好一个决定",
        ),
        (
            "遗漏决定",
            "w4_21_confirm_conflict_side",
            Box::new(|_, o| {
                o["decisions"].as_array_mut().unwrap().pop();
            }),
            "恰好一个决定",
        ),
        (
            "输入外的目标",
            "w4_01_state_change",
            Box::new(|_, o| {
                o["decisions"][0]["targets"] = json!([99]);
            }),
            "只能是输入记录",
        ),
        (
            "重复目标",
            "w4_01_state_change",
            Box::new(|_, o| {
                o["decisions"][0]["targets"] = json!([17, 17]);
            }),
            "重复",
        ),
        (
            "伪造片段",
            "w4_01_state_change",
            Box::new(|_, o| {
                o["decisions"][0]["basis"][0]["quote"]["text"] = json!("我住在广州。");
            }),
            "不在来源",
        ),
        (
            "输入外的来源",
            "w4_01_state_change",
            Box::new(|_, o| {
                o["decisions"][0]["basis"][0]["source"]["source_id"] = json!("s-999");
            }),
            "输入之外的来源",
        ),
        (
            "current 来源",
            "w4_01_state_change",
            Box::new(|_, o| {
                o["decisions"][0]["basis"][0]["source"] = json!({"kind": "current"});
            }),
            "已保存的来源",
        ),
        (
            "只有新来源的依据就结束旧记录",
            "w4_01_state_change",
            Box::new(|_, o| {
                o["decisions"][0]["basis"].as_array_mut().unwrap().remove(0);
            }),
            "缺少目标 17 的原文依据",
        ),
        (
            "没有新候选原文的依据",
            "w4_01_state_change",
            Box::new(|_, o| {
                o["decisions"][0]["basis"].as_array_mut().unwrap().remove(1);
            }),
            "须包含新候选的原文",
        ),
        (
            "生效时间用保存期限",
            "w4_01_state_change",
            Box::new(|s, o| {
                s["input"]["candidates"][0]["candidate"]["times"][0]["role"] = json!("retention");
                o["decisions"][0]["effective"]["role"] = json!("retention");
            }),
            "保存期限",
        ),
        (
            "生效时间不属于该候选",
            "w4_01_state_change",
            Box::new(|_, o| {
                o["decisions"][0]["effective"]["interp"]["offset"] = json!(-12);
            }),
            "已核验的时间之一",
        ),
        (
            "pending 带动作",
            "w4_16_pending_scope_incomplete",
            Box::new(|_, o| {
                o["decisions"][0]["action"] = json!("new");
            }),
            "pending 的 action 须为 null",
        ),
        (
            "字符串 null",
            "w4_16_pending_scope_incomplete",
            Box::new(|_, o| {
                o["decisions"][0]["action"] = json!("null");
            }),
            "decode",
        ),
        (
            "断言当撤回用",
            "w4_01_state_change",
            Box::new(|_, o| {
                o["decisions"][0]["action"] = json!("retract");
                o["decisions"][0]["effective"] = json!(null);
            }),
            "候选类别与动作不一致",
        ),
        (
            "结束无法核对的旧记录",
            "w4_18_legacy_gaps",
            Box::new(|_, o| {
                o["decisions"][0] = json!({"candidate": 0, "status": "decided", "action": "state_change",
                "targets": [95], "resolves_relations": [],
                "basis": o["decisions"][0]["basis"].clone(), "effective": null,
                "relation_scope": null, "pending_reason": null});
            }),
            "缺少目标 95 的原文依据",
        ),
        (
            "关闭关系却不结束任何一方",
            "w4_15_resolve_conflict",
            Box::new(|s, o| {
                // 把撤回 50 改成强化 51 并关闭 rel-1：两方都还有效。
                s["input"]["candidates"][0]["candidate"]["kind"] = json!("assertion");
                s["input"]["candidates"][0]["candidate"]["value"] =
                    s["input"]["records"][1]["candidate"]["value"].clone();
                o["decisions"][0]["action"] = json!("reinforce");
                o["decisions"][0]["targets"] = json!([51]);
                let own = o["decisions"][0]["basis"][1].clone();
                o["decisions"][0]["basis"] = json!([
                {"source": {"kind": "stored", "source_id": "s-261"},
                 "quote": s["input"]["records"][1]["evidence"][0]["quote"].clone()},
                own]);
            }),
            "不能算已解决",
        ),
        (
            "解决不在输入里的关系",
            "w4_15_resolve_conflict",
            Box::new(|_, o| {
                o["decisions"][0]["resolves_relations"] = json!(["rel-9"]);
            }),
            "只能解决输入中 open 的关系",
        ),
        (
            "new 解决关系",
            "w4_22_replace_conflict_members",
            Box::new(|_, o| {
                o["decisions"][0]["action"] = json!("new");
                o["decisions"][0]["targets"] = json!([]);
                o["decisions"][0]["resolves_relations"] = json!(["rel-2"]);
            }),
            "不能解决关系",
        ),
        (
            "非 conflict 带范围",
            "w4_01_state_change",
            Box::new(|_, o| {
                o["decisions"][0]["relation_scope"] = json!({"conditions": [], "times": []});
            }),
            "只在 conflict 时非空",
        ),
        (
            "conflict 缺范围",
            "w4_07_conflict",
            Box::new(|_, o| {
                o["decisions"][0]["relation_scope"] = json!(null);
            }),
            "只在 conflict 时非空",
        ),
        (
            "范围引用不存在的条件",
            "w4_07_conflict",
            Box::new(|_, o| {
                o["decisions"][0]["relation_scope"] =
                    json!({"conditions": [{"of": "candidate", "index": 0}], "times": []});
            }),
            "不存在的条件",
        ),
        (
            "范围引用非目标记录",
            "w4_07_conflict",
            Box::new(|_, o| {
                o["decisions"][0]["relation_scope"] =
                    json!({"conditions": [{"of": 51, "index": 0}], "times": []});
            }),
            "不存在的条件",
        ),
        (
            "重判以自身为目标",
            "w4_14_rejudge_committed",
            Box::new(|s, o| {
                let mut own = s["input"]["records"][0].clone();
                own["id"] = json!(70);
                s["input"]["records"].as_array_mut().unwrap().push(own);
                o["decisions"][0]["targets"] = json!([70]);
            }),
            "不能以自身为目标",
        ),
    ];
    for (label, id, edit, expected) in cases {
        let error = rejected(id, edit);
        assert!(error.contains(expected), "{label}: {error}");
    }
}

#[test]
fn accepted_quotes_are_restored_to_the_source_wording() {
    // 空白与引号排版的差异可以接受；进入 W5 的是原文写法。
    let sample = sample("w4_01_state_change");
    let mut output = fixture("w4_01_state_change");
    output["decisions"][0]["basis"][1]["quote"]["text"] = json!("我上个月 从北京搬到 上海了。");
    let input: W4Input = serde_json::from_value(sample["input"].clone()).unwrap();
    let output: W4Output = serde_json::from_value(output).unwrap();
    let sources = sample["sources"].clone();
    let text_of = |sid: &str| sources[sid]["text"].as_str().map(str::to_owned);
    let normalized = validate(&input, &output, &text_of).unwrap();
    assert_eq!(
        normalized.decisions[0].basis[1].quote.text,
        "我上个月从北京搬到上海了。"
    );
}

#[test]
fn fixtures_restate_the_expected_decisions_and_nothing_else() {
    // 夹具只是把期望决定里的 basis_sources 换成具体片段：其余字段逐一相同，来源集合相同。
    for sample in samples() {
        if sample["call_w4"] != true {
            continue;
        }
        let id = sample["id"].as_str().unwrap();
        let output = fixture(id);
        let expected = sample["expect"]["decisions"].as_array().unwrap();
        let actual = output["decisions"].as_array().unwrap();
        assert_eq!(expected.len(), actual.len(), "{id}");
        for (expected, actual) in expected.iter().zip(actual) {
            let mut expected = expected.clone();
            let mut actual = actual.clone();
            let listed = expected
                .as_object_mut()
                .unwrap()
                .remove("basis_sources")
                .unwrap();
            let basis = actual.as_object_mut().unwrap().remove("basis").unwrap();
            assert_eq!(expected, actual, "{id}");
            let cited: Vec<&str> = basis
                .as_array()
                .unwrap()
                .iter()
                .map(|e| e["source"]["source_id"].as_str().unwrap())
                .collect();
            assert_eq!(json!(cited), listed, "{id}");
        }
    }
}

#[test]
fn forgetting_a_source_scrubs_the_decision_basis_and_effective_time() {
    // w4_01 提交后删除新原文 s-205：决定回执里它的依据片段、生效时间原文都清掉；
    // 旧原文 s-101 的依据仍在。再删 s-101，它的片段也清掉。
    let run = run("w4_01_state_change");
    let forget = |source: &str| {
        let mut service =
            hippocampus::chat::ChatMemoryService::new(run.db.open(), Default::default()).unwrap();
        service
            .call("memory_forget", json!({"source_id": source}), || false)
            .unwrap()
    };
    let snapshot = || serde_json::to_string(&run.db.state().w5_receipts).unwrap();
    assert!(snapshot().contains("上个月") && snapshot().contains("我住在北京。"));
    forget("s-205");
    let after = snapshot();
    assert!(
        !after.contains("上个月") && !after.contains("搬到上海"),
        "{after}"
    );
    assert!(after.contains("我住在北京。"));
    forget("s-101");
    assert!(!snapshot().contains("我住在北京。"));
}

#[test]
fn w5_rejects_inconsistent_basis_and_effective_fields() {
    // 这些字段由转换生成；W5 仍独立核对，手工构造的不一致组整组拒绝。
    let sample = sample("w4_01_state_change");
    let db = Db::new();
    let own = seed(&db, &sample);
    let input: W4Input = serde_json::from_value(sample["input"].clone()).unwrap();
    let output: W4Output = serde_json::from_value(fixture("w4_01_state_change")).unwrap();
    let state = db.state();
    let good = plan(&runtime(&state, &own, 1, None), &input, &output)
        .unwrap()
        .groups[0]
        .clone();
    let before = serde_json::to_string(&db.state().repository).unwrap();
    let mut cases: Vec<(&str, W5Group, &str)> = Vec::new();
    let mut g = good.clone();
    g.decisions[0].basis_sources = vec!["s-205".into()];
    cases.push(("basis sources differ", g, "different sources"));
    let mut g = good.clone();
    g.decisions[0].effective = Some(DatedValue {
        at: Timestamp(1785513600),
        source: TemporalSource::Explicit,
    });
    cases.push(("month precision written as a point", g, "disagree"));
    let mut g = good.clone();
    g.decisions[0].basis[0].quote.text = "我住在广州。".into();
    cases.push(("basis quote not in the source", g, "not verbatim"));
    let mut g = good.clone();
    g.decisions[0].relation_scope = Some(RelationScope {
        conditions: vec![],
        times: vec![],
    });
    cases.push(("scope on a state change", g, "relation scope"));
    for (label, group, expected) in cases {
        let error = commit_group(&mut db.open(), &claim(&own), &group).unwrap_err();
        assert!(error.to_string().contains(expected), "{label}: {error}");
        assert_eq!(
            serde_json::to_string(&db.state().repository).unwrap(),
            before,
            "{label}"
        );
    }
}

// ---------------- 复审 0026 的回归

fn pending_of(input: &W4Input) -> Vec<Pending> {
    input
        .candidates
        .iter()
        .map(|c| Pending {
            index: c.index,
            committed_id: c.committed_id,
            contract: ContractCandidate {
                candidate: c.candidate.clone(),
                source: c.source.clone(),
                said_on: c.said_on.clone(),
                time_resolutions: c.time_resolutions.clone(),
            },
        })
        .collect()
}

/// 直接写入一条照着 `like` 复制的记录（不同主体，检索取不到），可带任务条件。
fn clone_record(db: &Db, like: u64, subject: &str, task: Option<&str>) -> MemoryId {
    let mut made = None;
    db.open()
        .transaction(|state| {
            let mut memory = state.repository.get(MemoryId(like))?.unwrap();
            memory.id = state.repository.allocate_id()?;
            memory.subject_id.0 = subject.into();
            if let Some(task) = task {
                memory
                    .scope
                    .conditions
                    .push(ScopeCondition::Task(TaskId::from(task)));
            }
            made = Some(memory.id);
            state.repository.insert(memory)?;
            Ok(())
        })
        .unwrap();
    made.unwrap()
}

fn open_relation(db: &Db, id: &str, members: &[MemoryId]) {
    db.open()
        .transaction(|state| {
            state.relations.insert(
                id.into(),
                StoredRelation {
                    id: id.into(),
                    kind: hippocampus::RelationKind::Conflict,
                    members: members.to_vec(),
                    conditions: json!([]),
                    times: json!([]),
                    basis: vec![],
                    status: hippocampus::RelationStatus::Open,
                    resolution: None,
                    created_by: "seed".into(),
                    closed_by: None,
                },
            );
            Ok(())
        })
        .unwrap();
}

fn ids(input: &W4Input) -> (Vec<u64>, Vec<String>) {
    let mut records: Vec<u64> = input.records.iter().map(|r| r.id().0).collect();
    records.sort_unstable();
    let mut relations: Vec<String> = input.relations.iter().map(|r| r.id.clone()).collect();
    relations.sort();
    (records, relations)
}

/// 重新认领一条已整理完的原文（重判）。
fn reclaim(db: &Db, sid: &str) {
    db.open()
        .transaction(|state| {
            state.source_less_barrier_sequence = 0;
            let revision = state.revision;
            let task = state.sources.get_mut(sid).unwrap();
            task.enrichment = "running".into();
            task.error = None;
            task.task.attempt_id = Some(ATTEMPT.into());
            task.task.lease_until = Some(now() + 900);
            task.task.claim_revision = Some(revision);
            Ok(())
        })
        .unwrap();
}

fn rejudged_new(input: &W4Input) -> W4Output {
    W4Output {
        decisions: input
            .candidates
            .iter()
            .map(|candidate| W4Decision {
                candidate: candidate.index,
                status: DecisionStatus::Decided,
                action: Some(Action::New),
                targets: vec![],
                resolves_relations: vec![],
                basis: vec![EvidenceRef {
                    source: candidate.source.clone(),
                    quote: candidate.candidate.sentence.clone(),
                }],
                effective: None,
                relation_scope: None,
                pending_reason: None,
            })
            .collect(),
    }
}

#[test]
fn a_rejudge_cannot_clear_a_relation_withheld_by_task_scope() {
    for status in [
        hippocampus::RelationStatus::Open,
        hippocampus::RelationStatus::Resolved,
        hippocampus::RelationStatus::Invalidated,
    ] {
        let sample = sample("w4_01_state_change");
        let db = Db::new();
        let own = seed(&db, &sample);
        let other = clone_record(&db, 17, "someone-else", Some("other-task"));
        open_relation(&db, "rel-7", &[MemoryId(17), other]);
        reclaim(&db, &own);
        let input: W4Input = serde_json::from_value(sample["input"].clone()).unwrap();
        let state = db.state();
        let rt = runtime(&state, &own, 1, None);
        let assembly = assemble(&rt, &pending_of(&input), BUDGET).unwrap();
        let output = serde_json::from_value(fixture("w4_01_state_change")).unwrap();
        let first = plan(&rt, &assembly.input, &output).unwrap();
        let product = match commit_group(&mut db.open(), &claim(&own), &first.groups[0]).unwrap() {
            W5Outcome::Committed(receipt) => receipt.products[&0],
            other => panic!("{other:?}"),
        };
        assert!(
            db.state().record_updates[&product]
                .reasons
                .contains(&UpdateReason::UpdateRelationUnclear)
        );
        db.open()
            .transaction(|state| {
                let relation = state.relations.get_mut("rel-7").unwrap();
                relation.status = status;
                if status == hippocampus::RelationStatus::Resolved {
                    relation.resolution = Some(RelationEvidence {
                        source_id: own.clone(),
                        quote: input.candidates[0].candidate.sentence.text.clone(),
                    });
                }
                Ok(())
            })
            .unwrap();
        reclaim(&db, &own);
        let mut input = input;
        input.candidates[0].committed_id = Some(product);
        let state = db.state();
        let rt = runtime(&state, &own, 2, None);
        let assembly = assemble(&rt, &pending_of(&input), BUDGET).unwrap();
        assert_eq!(ids(&assembly.input), (vec![17], vec![]));
        assert!(!assembly.input.coverage.complete);
        let plan = plan(&rt, &assembly.input, &rejudged_new(&assembly.input)).unwrap();
        match commit_group(&mut db.open(), &claim(&own), &plan.groups[0]).unwrap() {
            W5Outcome::Committed(_) => {}
            other => panic!("{other:?}"),
        }
        assert_eq!(
            db.state().record_updates[&product],
            UpdateState::from_reasons(vec![
                UpdateReason::UpdateScopeIncomplete,
                UpdateReason::UpdateRelationUnclear,
            ]),
            "withheld relation: {status:?}"
        );
        assert!(plan.groups[0].seen_relations.is_empty());
        assert_eq!(memory(&db.state(), other).status, MemoryStatus::Active);
    }
}

fn replacement_with_a_forgotten_predecessor() -> Run {
    let run = run("w4_22_replace_conflict_members");
    let mut service =
        hippocampus::chat::ChatMemoryService::new(run.db.open(), Default::default()).unwrap();
    service
        .call("memory_forget", json!({"source_id": "s-501"}), || false)
        .unwrap();
    reclaim(&run.db, "s-510");
    run
}

#[test]
fn a_rejudge_after_forgetting_a_predecessor_can_clear_the_relation_gap() {
    let run = replacement_with_a_forgotten_predecessor();
    let product = run.product(0).unwrap();
    assert_eq!(
        memory(&run.db.state(), MemoryId(110)).status,
        MemoryStatus::Deleted
    );
    assert!(
        run.db.state().record_updates[&product]
            .reasons
            .contains(&UpdateReason::UpdateRelationUnclear),
        "forgetting alone must not clear the gap"
    );
    let mut input: W4Input = serde_json::from_value(run.sample["input"].clone()).unwrap();
    input.candidates[0].committed_id = Some(product);
    let state = run.db.state();
    let rt = runtime(&state, "s-510", 2, None);
    let assembly = assemble(&rt, &pending_of(&input), BUDGET).unwrap();
    assert!(assembly.call_w4);
    assert_eq!(ids(&assembly.input), (vec![111], vec!["rel-2".into()]));
    assert_eq!(
        assembly.input.relations[0].status,
        hippocampus::contract::RelationStatus::Invalidated
    );
    let plan = plan(&rt, &assembly.input, &rejudged_new(&assembly.input)).unwrap();
    assert_eq!(
        plan.groups[0]
            .seen
            .iter()
            .map(|s| s.id.0)
            .collect::<Vec<_>>(),
        [111, 112]
    );
    let receipt = match commit_group(&mut run.db.open(), &claim("s-510"), &plan.groups[0]).unwrap()
    {
        W5Outcome::Committed(receipt) => receipt,
        other => panic!("{other:?}"),
    };
    assert_eq!(receipt.products[&0], product);
    assert_eq!(
        run.db.state().record_updates[&product],
        UpdateState::from_reasons(vec![])
    );
    assert_eq!(
        memory(&run.db.state(), MemoryId(110)).status,
        MemoryStatus::Deleted
    );
}

#[test]
fn a_deleted_relation_member_does_not_consume_the_input_budget() {
    let run = replacement_with_a_forgotten_predecessor();
    let mut input: W4Input = serde_json::from_value(run.sample["input"].clone()).unwrap();
    input.candidates[0].committed_id = run.product(0);
    let state = run.db.state();
    let rt = runtime(&state, "s-510", 2, None);
    let assembly = assemble(
        &rt,
        &pending_of(&input),
        W4Budget {
            facts: 8,
            records: 1,
        },
    )
    .unwrap();
    assert!(
        assembly.call_w4,
        "only the surviving predecessor consumes the budget"
    );
    assert_eq!(ids(&assembly.input), (vec![111], vec!["rel-2".into()]));
    assert_eq!(
        assembly.input.coverage,
        W4Coverage {
            complete: true,
            omitted: 0
        }
    );
    let plan = plan(&rt, &assembly.input, &rejudged_new(&assembly.input)).unwrap();
    match commit_group(&mut run.db.open(), &claim("s-510"), &plan.groups[0]).unwrap() {
        W5Outcome::Committed(receipt) => {
            assert_eq!(receipt.updates[&0], UpdateState::from_reasons(vec![]));
        }
        other => panic!("{other:?}"),
    }
    // 真正放不下唯一的可送入记录时，仍整批待定；删除成员不计入 omitted。
    let zero = assemble(
        &rt,
        &pending_of(&input),
        W4Budget {
            facts: 8,
            records: 0,
        },
    )
    .unwrap();
    assert!(!zero.call_w4);
    assert_eq!(
        zero.input.coverage,
        W4Coverage {
            complete: false,
            omitted: 1
        }
    );
    assert_eq!(
        zero.skipped.decisions[0].pending_reason,
        Some(PendingReason::ScopeIncomplete)
    );
}

#[test]
fn a_held_candidate_holds_its_whole_dependency_group() {
    // w4_21 变体：候选 0（修订 51）是现有 Memory 表达不了的否定事实；候选 1 撤回 50 并解决
    // rel-1。两者经 rel-1 相互依赖，不能只执行后一半。
    let mut sample = sample("w4_21_confirm_conflict_side");
    sample["input"]["candidates"][0]["candidate"]["polarity"] = json!("negate");
    let db = Db::new();
    let own = seed(&db, &sample);
    let input: W4Input = serde_json::from_value(sample["input"].clone()).unwrap();
    let output: W4Output = serde_json::from_value(fixture("w4_21_confirm_conflict_side")).unwrap();
    let state = db.state();
    let plan = plan(&runtime(&state, &own, 1, None), &input, &output).unwrap();
    assert!(plan.groups.is_empty(), "{:?}", plan.groups);
    assert_eq!(
        plan.held,
        BTreeMap::from([
            (
                0,
                Held::Unprojectable(Unprojectable::Polarity(
                    hippocampus::contract::Polarity::Negate
                ))
            ),
            (1, Held::DependsOnHeld { candidate: 0 }),
        ])
    );
    // 独立的组不受影响：同批再加一个无关的新候选，照常成组。
    let mut input = input;
    let mut output = output;
    let mut extra = input.candidates[0].clone();
    extra.index = 2;
    extra.candidate.polarity = hippocampus::contract::Polarity::Affirm;
    input.candidates.push(extra.clone());
    output.decisions.push(W4Decision {
        candidate: 2,
        status: DecisionStatus::Decided,
        action: Some(Action::New),
        targets: vec![],
        resolves_relations: vec![],
        basis: vec![EvidenceRef {
            source: extra.source.clone(),
            quote: extra.candidate.sentence.clone(),
        }],
        effective: None,
        relation_scope: None,
        pending_reason: None,
    });
    let plan = hippocampus::w4::plan(&runtime(&state, &own, 1, None), &input, &output).unwrap();
    assert_eq!(plan.groups.len(), 1);
    assert_eq!(plan.groups[0].candidates[0].index, 2);
    assert_eq!(plan.held.len(), 2);
}

#[test]
fn a_relation_reaching_into_another_task_is_withheld_and_reported() {
    // w4_19：记录 97 在本任务内可见，98 属于另一个任务。两者之间有 open 关系时，本任务的
    // 输入不能带入 98，也不能送入一条截掉成员的关系；范围如实报告为不完整。
    let sample = sample("w4_19_task_scope");
    let db = Db::new();
    let own = seed(&db, &sample);
    open_relation(&db, "rel-7", &[MemoryId(97), MemoryId(98)]);
    let input: W4Input = serde_json::from_value(sample["input"].clone()).unwrap();
    let state = db.state();
    let rt = runtime(&state, &own, 1, task_of("w4_19_task_scope"));
    let assembly = assemble(&rt, &pending_of(&input), BUDGET).unwrap();
    assert_eq!(ids(&assembly.input), (vec![97], vec![]));
    assert_eq!(
        assembly.input.coverage,
        W4Coverage {
            complete: false,
            omitted: 1
        }
    );
    // 在 98 所属的任务里，同一条关系完整可见。
    let rt = runtime(&state, &own, 1, Some(TaskId::from("s6-plateau-demo")));
    let other = assemble(&rt, &pending_of(&input), BUDGET).unwrap();
    assert!(other.input.records.iter().all(|r| r.id().0 != 97));
}

#[test]
fn relation_expansion_runs_to_a_fixpoint_whatever_the_relation_ids() {
    // 初选只命中 17；17—A 与 A—B 两条关系无论编号先后，A、B 和两条关系都进入输入。
    for (near, far) in [("rel-3", "rel-1"), ("rel-3", "rel-9")] {
        let sample = sample("w4_01_state_change");
        let db = Db::new();
        let own = seed(&db, &sample);
        let a = clone_record(&db, 17, "someone-else", None);
        let b = clone_record(&db, 17, "someone-else", None);
        open_relation(&db, near, &[MemoryId(17), a]);
        open_relation(&db, far, &[a, b]);
        let input: W4Input = serde_json::from_value(sample["input"].clone()).unwrap();
        let state = db.state();
        let assembly =
            assemble(&runtime(&state, &own, 1, None), &pending_of(&input), BUDGET).unwrap();
        let mut relations = vec![near.to_owned(), far.to_owned()];
        relations.sort();
        assert_eq!(ids(&assembly.input), (vec![17, a.0, b.0], relations));
    }
}

#[test]
fn required_records_over_budget_make_the_whole_batch_pending() {
    // w4_12：两条专用关系记录都必须送入。预算放不下时不送入被截断的输入、不调用 W4，
    // 候选待定（scope_incomplete），旧记录不动。
    let sample = sample("w4_12_multi_target");
    let db = Db::new();
    let own = seed(&db, &sample);
    let input: W4Input = serde_json::from_value(sample["input"].clone()).unwrap();
    let state = db.state();
    let rt = runtime(&state, &own, 1, None);
    let tight = W4Budget {
        facts: 8,
        records: 1,
    };
    let assembly = assemble(&rt, &pending_of(&input), tight).unwrap();
    assert!(!assembly.call_w4);
    assert_eq!(ids(&assembly.input), (vec![], vec![]));
    assert_eq!(
        assembly.input.coverage,
        W4Coverage {
            complete: false,
            omitted: 2
        }
    );
    let decision = &assembly.skipped.decisions[0];
    assert_eq!(decision.status, DecisionStatus::Pending);
    assert_eq!(
        decision.pending_reason,
        Some(PendingReason::ScopeIncomplete)
    );
    let plan = plan(&rt, &assembly.input, &assembly.skipped).unwrap();
    let receipt = match commit_group(&mut db.open(), &claim(&own), &plan.groups[0]).unwrap() {
        W5Outcome::Committed(receipt) => receipt,
        other => panic!("{other:?}"),
    };
    let after = db.state();
    assert_eq!(
        receipt.updates[&0].reasons,
        vec![UpdateReason::UpdateScopeIncomplete]
    );
    for id in [62, 63] {
        assert_eq!(memory(&after, MemoryId(id)).status, MemoryStatus::Active);
    }
    // 通用 fact 的检索命中不是必须送入的：放不下时计入 omitted，候选照常新增。
    let sample = self::sample("w4_07_conflict");
    let db = Db::new();
    let own = seed(&db, &sample);
    let input: W4Input = serde_json::from_value(sample["input"].clone()).unwrap();
    let state = db.state();
    let none = W4Budget {
        facts: 8,
        records: 0,
    };
    let assembly = assemble(&runtime(&state, &own, 1, None), &pending_of(&input), none).unwrap();
    assert!(!assembly.call_w4);
    assert_eq!(
        assembly.input.coverage,
        W4Coverage {
            complete: false,
            omitted: 1
        }
    );
    assert_eq!(assembly.skipped.decisions[0].action, Some(Action::New));
}

#[test]
fn a_rejudge_sees_its_predecessors_and_a_later_source_can_resolve_their_relation() {
    // w4_22 提交后：产物 112 替代了 110、111，rel-2 仍 open，112 的更新状态含
    // update_relation_unclear。
    let run = run("w4_22_replace_conflict_members");
    let product = run.product(0).unwrap();
    assert_eq!(product, MemoryId(112));
    assert!(
        run.after.record_updates[&product]
            .reasons
            .contains(&UpdateReason::UpdateRelationUnclear)
    );
    let db = &run.db;
    let mut input: W4Input = serde_json::from_value(run.sample["input"].clone()).unwrap();
    let own = "s-510";

    let quote = |source: &str, text: &str| EvidenceRef {
        source: SourceRef::Stored {
            source_id: source.into(),
        },
        quote: Quote { text: text.into() },
    };

    // 重判 112：输入包含前身 110、111 和 rel-2 的最新状态。给出已定决定后
    // update_relation_unclear 被清除（不要求 rel-2 被关闭）。
    reclaim(db, own);
    let mut rejudge = input.clone();
    rejudge.candidates[0].committed_id = Some(product);
    {
        let state = db.state();
        let rt = runtime(&state, own, 2, None);
        let assembly = assemble(&rt, &pending_of(&rejudge), BUDGET).unwrap();
        assert!(assembly.call_w4);
        assert_eq!(
            ids(&assembly.input),
            (vec![110, 111], vec!["rel-2".to_owned()])
        );
        let output = W4Output {
            decisions: vec![W4Decision {
                candidate: 0,
                status: DecisionStatus::Decided,
                action: Some(Action::New),
                targets: vec![],
                resolves_relations: vec![],
                basis: vec![quote("s-510", "我已经搬到深圳了。")],
                effective: None,
                relation_scope: None,
                pending_reason: None,
            }],
        };
        let plan = plan(&rt, &assembly.input, &output).unwrap();
        let seen: Vec<u64> = plan.groups[0].seen.iter().map(|s| s.id.0).collect();
        assert_eq!(seen, [110, 111, 112]);
        assert!(
            plan.groups[0]
                .seen_relations
                .iter()
                .any(|relation| relation.id == "rel-2")
        );
        match commit_group(&mut db.open(), &claim(own), &plan.groups[0]).unwrap() {
            W5Outcome::Committed(_) => {}
            other => panic!("{other:?}"),
        }
        w5_finish_task(&mut db.open(), &claim(own), &W5TaskEnd::Committed).unwrap();
    }
    let after = db.state();
    assert_eq!(after.record_updates[&product].status, UpdateStatus::Judged);
    assert_eq!(
        after.relations["rel-2"].status,
        hippocampus::RelationStatus::Open
    );

    // 后续原文：重申住在深圳。输入带出 112、它的前身和前身所在的 open 关系；110、111
    // 已经结束，W4 可以强化 112 并显式解决 rel-2。
    claim_source(db, "s-520", "我确实住在深圳。");
    reclaim(db, "s-520");
    input.candidates[0].candidate.sentence.text = "我确实住在深圳。".into();
    input.candidates[0].source = SourceRef::Stored {
        source_id: "s-520".into(),
    };
    {
        let state = db.state();
        let rt = runtime(&state, "s-520", 1, None);
        let assembly = assemble(&rt, &pending_of(&input), BUDGET).unwrap();
        assert_eq!(
            ids(&assembly.input),
            (vec![110, 111, 112], vec!["rel-2".to_owned()])
        );
        let output = W4Output {
            decisions: vec![W4Decision {
                candidate: 0,
                status: DecisionStatus::Decided,
                action: Some(Action::Reinforce),
                targets: vec![product],
                resolves_relations: vec!["rel-2".into()],
                basis: vec![
                    quote("s-510", "我已经搬到深圳了。"),
                    quote("s-520", "我确实住在深圳。"),
                ],
                effective: None,
                relation_scope: None,
                pending_reason: None,
            }],
        };
        let plan = plan(&rt, &assembly.input, &output).unwrap();
        match commit_group(&mut db.open(), &claim("s-520"), &plan.groups[0]).unwrap() {
            W5Outcome::Committed(_) => {}
            other => panic!("{other:?}"),
        }
        w5_finish_task(&mut db.open(), &claim("s-520"), &W5TaskEnd::Committed).unwrap();
    }
    let after = db.state();
    assert_eq!(
        after.relations["rel-2"].status,
        hippocampus::RelationStatus::Resolved
    );
    for id in [110, 111] {
        assert_eq!(
            memory(&after, MemoryId(id)).status,
            MemoryStatus::Superseded
        );
    }
    // 之后再重判 112 时，输入给出的是 rel-2 的最新状态（已解决），不是漏掉它。
    let rt = runtime(&after, own, 3, None);
    let assembly = assemble(&rt, &pending_of(&rejudge), BUDGET).unwrap();
    assert_eq!(
        ids(&assembly.input),
        (vec![110, 111], vec!["rel-2".to_owned()])
    );
    assert_eq!(
        assembly.input.relations[0].status,
        hippocampus::contract::RelationStatus::Resolved
    );
}

fn with_time(id: &str, interp: Json, resolution: Json) -> Run {
    let mut sample = sample(id);
    let time = json!({"role": "event", "quote": {"text": "上个月"}, "interp": interp, "anchor": "said_on"});
    sample["input"]["candidates"][0]["candidate"]["times"] = json!([time]);
    sample["input"]["candidates"][0]["time_resolutions"] = json!([resolution]);
    let mut output = fixture(id);
    output["decisions"][0]["effective"] = time;
    run_with(sample, Some(output))
}

#[test]
fn a_retraction_may_carry_an_end_time_and_other_actions_only_record_it() {
    let day = 86_400;
    let start = 1_785_513_600;
    // 撤回带生效时间：是目标的结束时间（契约 2.4 不按动作限制 effective）。
    let run = with_time(
        "w4_05_retract",
        json!({"kind": "relative", "unit": "day", "offset": -1}),
        json!({"status": "resolved", "start": start, "end_exclusive": start + day, "precision": "day"}),
    );
    let old = memory(&run.after, MemoryId(40));
    assert_eq!(old.status, MemoryStatus::Superseded);
    assert_eq!(old.supersede_reason, Some(SupersedeReason::UserRetracted));
    assert_eq!(old.temporal.valid_to.map(|v| v.at), Some(Timestamp(start)));
    match record_view(&run.after, MemoryId(40)) {
        Some(RecordView2::Current(view)) => {
            assert_eq!(view.ended, Ended::Known);
            assert_eq!(view.end.unwrap().precision, Precision::Day);
        }
        other => panic!("{other:?}"),
    }
    // 强化不结束任何记录：生效时间只记在决定里，目标没有结束时间。
    let run = with_time(
        "w4_10_reinforce",
        json!({"kind": "relative", "unit": "month", "offset": -1}),
        json!({"status": "resolved", "start": start, "end_exclusive": start + 31 * day, "precision": "month"}),
    );
    let stored = &run.after.w5_receipts[&run.receipts[0].group_id];
    assert!(stored.decisions[0].effective_time.is_some());
    let target = memory(&run.after, MemoryId(31));
    assert_eq!(target.status, MemoryStatus::Active);
    assert_eq!(target.temporal.valid_to, None);
    match record_view(&run.after, MemoryId(31)) {
        Some(RecordView2::Current(view)) => {
            assert_eq!((view.ended, view.end), (Ended::NotEnded, None))
        }
        other => panic!("{other:?}"),
    }
    let mut service =
        hippocampus::chat::ChatMemoryService::new(run.db.open(), Default::default()).unwrap();
    let listed = service
        .call("memory_list", json!({"include_history": true}), || false)
        .unwrap();
    let item = listed["memories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == 31)
        .unwrap();
    assert!(item["qualifiers_unmapped"]["end"].is_null(), "{item}");
}
