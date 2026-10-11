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
                    management: Default::default(),
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
                        ..Default::default()
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

// 旧回归只观察组装结果；主样本流程另直接复用组装返回的状态快照进行 plan。
fn assemble(
    db: &Db,
    rt: &W4Runtime<'_>,
    candidates: &[Pending],
    budget: W4Budget,
) -> PersistenceResult<Assembly> {
    hippocampus::w4::assemble(
        &db.open(),
        rt.source_key,
        rt.task_id.as_ref(),
        candidates,
        budget,
    )
    .map(|(_, assembly)| assembly)
}

fn fact_candidate(sid: &str, sentence: &str) -> W4Candidate {
    let mut candidate: W4Candidate =
        serde_json::from_value(sample("w4_10_reinforce")["input"]["candidates"][0].clone())
            .unwrap();
    candidate.source = SourceRef::Stored {
        source_id: sid.into(),
    };
    candidate.candidate.sentence.text = sentence.into();
    candidate.candidate.value = Some(hippocampus::contract::Value::Text {
        text: sentence.into(),
        quote: candidate.candidate.sentence.clone(),
    });
    candidate
}

fn seed_fact(db: &Db, sid: &str, sentence: &str, raw: &str) -> MemoryId {
    claim_source(db, sid, raw);
    let id = commit_new(db, sid, 0, None, vec![fact_candidate(sid, sentence)], false)[0];
    w5_finish_task(&mut db.open(), &claim(sid), &W5TaskEnd::Committed).unwrap();
    id
}

fn fact_pending(sid: &str, sentence: &str) -> Vec<Pending> {
    let c = fact_candidate(sid, sentence);
    vec![Pending {
        index: c.index,
        committed_id: c.committed_id,
        contract: ContractCandidate {
            candidate: c.candidate,
            source: c.source,
            said_on: c.said_on,
            time_resolutions: c.time_resolutions,
        },
    }]
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
    let (before, assembly) =
        hippocampus::w4::assemble(&db.open(), &own, task_of(id).as_ref(), &pending, BUDGET)
            .unwrap();
    let rt = runtime(&before, &own, 1, task_of(id));
    let (w4_input, output, called_w4) = if id == "w4_09_new" {
        // 用户同意的例外：不比对组装结果，直接用样本输入。
        assert!(!assembly.call_w4, "the unrelated record is not retrieved");
        (
            input.clone(),
            serde_json::from_value(fixture(id)).unwrap(),
            true,
        )
    } else {
        // Fixed incomplete-scope controls inject fallback coverage; actual enumeration
        // is checked separately by the AF276 complete controls.
        if input.coverage.complete {
            assert_eq!(
                serde_json::to_value(&assembly.input.coverage).unwrap(),
                sample["input"]["coverage"],
                "{id}: complete coverage must come from actual enumeration"
            );
        }
        let mut expected = sample["input"].clone();
        expected["coverage"] = serde_json::to_value(&assembly.input.coverage).unwrap();
        assert_eq!(
            sorted_records(serde_json::to_value(&assembly.input).unwrap()),
            sorted_records(expected),
            "{id}: assembled records and closure"
        );
        assert_eq!(assembly.call_w4, sample["call_w4"], "{id}: call_w4");
        if assembly.call_w4 {
            let output: W4Output = serde_json::from_value(fixture(id)).unwrap();
            (input.clone(), output, true)
        } else {
            (input.clone(), assembly.skipped.clone(), false)
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
                "times" => assert_eq!(
                    &lower(w5_record_contract(state, record).unwrap().candidate.times),
                    expected,
                    "{tag}"
                ),
                "polarity" => assert_eq!(&lower(stored.polarity), expected, "{tag}"),
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
            // 自然语言召回语义独立人工复核，不把执行状态当作语义通过。
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

// The same temporary SQLite runner checks each model output and the standard fixtures.
pub fn verify_after(sample: Json, output: Json) -> Json {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let run = run_with(sample, Some(output));
        check_after(&run);
    })) {
        Ok(()) => json!({"status":"通过","reopened":true}),
        Err(error) => json!({"status":"失败","error":error.downcast_ref::<String>()
            .cloned().or_else(||error.downcast_ref::<&str>().map(|s|s.to_string()))
            .unwrap_or_else(||"runtime assertion failed".into())}),
    }
}
