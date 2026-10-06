//! 召回②：真实 SQLite、生产组装/跳过出口与重开；不使用模型或评测样本。
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use hippocampus::RelationStatus;
use hippocampus::contract::RelationStatus as LinkStatus;
use hippocampus::contract::*;
use hippocampus::recall::{self, Budget, Request, SkipReason};
use hippocampus::*;
use serde_json::{Value as Json, json};

const OWNER: &str = "recall-owner";
const SAID_ON: &str = "2026-04-15";
const AT: i64 = 1_776_249_600;

#[test]
fn complete_empty_retrieval_is_insufficient_without_fabricating_a_judgment() {
    let db = Db::new();
    let req = request("我用哪种墨盒？", None, None);
    let understood = plan(&req, Some(understanding()));
    let assembly = recall::assemble(&db.open(), &req, &understood, Budget::default()).unwrap();
    assert!(assembly.evidence.completeness.plan_complete);
    assert!(!assembly.has_judgment_evidence());
    let result = assembly
        .finish_skipped(&db.open(), SkipReason::NoEvidence)
        .unwrap();
    assert_eq!(result.status, RecallStatus::Insufficient);
    assert_eq!(result.semantic_judgment, SemanticJudgment::Skipped);
    assert_eq!(
        result.degraded_reasons,
        ["semantic_judgment_skipped:no_evidence"]
    );
    assert!(result.facts.is_empty() && result.computed.is_none());
}

#[test]
fn empty_truncated_retrieval_stays_degraded() {
    let db = Db::new();
    source(&db, "unprocessed", "尚待整理的原文。", None, "queued");
    let req = request("墨盒？", None, None);
    let understood = plan(&req, Some(understanding()));
    let assembly = recall::assemble(
        &db.open(),
        &req,
        &understood,
        Budget {
            raw: 0,
            ..Budget::default()
        },
    )
    .unwrap();
    assert!(!assembly.has_judgment_evidence());
    assert!(assembly.evidence.completeness.retrieval_truncated);
    let result = assembly
        .finish_skipped(&db.open(), SkipReason::NoEvidence)
        .unwrap();
    assert_eq!(result.status, RecallStatus::Degraded);
    assert_eq!(result.semantic_judgment, SemanticJudgment::Skipped);
}

struct Db(PathBuf);
impl Db {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!("recall-r3-{}", diagnostics::new_id()));
        std::fs::create_dir_all(&directory).unwrap();
        let db = Self(directory.join("memory.sqlite3"));
        db.open()
            .transaction(|state| {
                for (id, name) in [("self", "本人"), ("child", "孩子")] {
                    state.entities.upsert(Entity {
                        id: EntityId::from(id),
                        owner_id: UserId::from(OWNER),
                        entity_type: EntityType::Person,
                        canonical_name: name.into(),
                        aliases: vec![],
                    });
                }
                Ok(())
            })
            .unwrap();
        db
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
        let _ = std::fs::remove_dir_all(self.0.parent().unwrap());
    }
}

fn source(db: &Db, id: &str, text: &str, task: Option<&str>, status: &str) {
    db.open()
        .transaction(|state| {
            let sequence = state
                .sources
                .values()
                .map(|s| s.received_sequence)
                .max()
                .unwrap_or(0)
                + 1;
            state.sources.insert(
                id.into(),
                SourceRecord {
                    id: id.into(),
                    session_id: "recall-test".into(),
                    turn_id: id.into(),
                    observed_at: AT + sequence as i64,
                    received_sequence: sequence,
                    request: Some(json!({"text":text,"task_id":task,"capture_source":true})),
                    enrichment: status.into(),
                    error: None,
                    result: None,
                    base_revision: state.revision,
                    index: Json::Null,
                    task: EnrichmentTask {
                        attempt_id: Some(format!("claim-{id}")),
                        lease_until: Some(
                            SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .unwrap()
                                .as_secs() as i64
                                + 600,
                        ),
                        claim_revision: Some(state.revision),
                        ..Default::default()
                    },
                },
            );
            Ok(())
        })
        .unwrap();
}

fn stored(id: &str, text: &str) -> EvidenceRef {
    EvidenceRef {
        source: SourceRef::Stored {
            source_id: id.into(),
        },
        quote: Quote { text: text.into() },
    }
}

fn candidate(id: &str, text: &str, subject: &str) -> W4Candidate {
    W4Candidate {
        index: 0,
        committed_id: None,
        source: SourceRef::Stored {
            source_id: id.into(),
        },
        said_on: SAID_ON.into(),
        time_resolutions: vec![],
        candidate: Candidate {
            kind: CandidateKind::Assertion,
            speech: Speech::UserStatement,
            subject: Subject::Entity(EntitySubject {
                entity_id: subject.into(),
            }),
            relation: "fact".into(),
            memory_type: "fact".into(),
            value: Some(hippocampus::contract::Value::Text {
                text: text.into(),
                quote: Quote { text: text.into() },
            }),
            sentence: Quote { text: text.into() },
            polarity: hippocampus::contract::Polarity::Affirm,
            strength: None,
            uncertainty: None,
            conditions: vec![],
            times: vec![],
        },
    }
}

fn commit(db: &Db, id: &str, task: Option<&str>, input: W4Input, decision: W4Decision) -> MemoryId {
    let state = db.state();
    let plan = w4::plan(
        &w4::W4Runtime {
            state: &state,
            owner: UserId::from(OWNER),
            source_key: id,
            round: 0,
            task_id: task.map(TaskId::from),
            admissions: BTreeMap::new(),
        },
        &input,
        &W4Output {
            decisions: vec![decision],
        },
    )
    .unwrap();
    assert!(plan.held.is_empty(), "{:?}", plan.held);
    let claim = W5Claim {
        source_key: id.into(),
        attempt_id: format!("claim-{id}"),
    };
    let mut products = Vec::new();
    for group in &plan.groups {
        match commit_group(&mut db.open(), &claim, group).unwrap() {
            W5Outcome::Committed(receipt) => products.extend(receipt.products.values().copied()),
            other => panic!("{other:?}"),
        }
    }
    w5_finish_task(&mut db.open(), &claim, &W5TaskEnd::Committed).unwrap();
    products[0]
}

fn seed(
    db: &Db,
    id: &str,
    text: &str,
    subject: &str,
    task: Option<&str>,
    complete: bool,
) -> MemoryId {
    source(db, id, text, task, "running");
    let input = W4Input {
        candidates: vec![candidate(id, text, subject)],
        records: vec![],
        relations: vec![],
        coverage: W4Coverage {
            complete,
            omitted: 0,
        },
    };
    commit(
        db,
        id,
        task,
        input,
        W4Decision {
            candidate: 0,
            status: DecisionStatus::Decided,
            action: Some(Action::New),
            targets: vec![],
            resolves_relations: vec![],
            basis: vec![stored(id, text)],
            effective: None,
            relation_scope: None,
            pending_reason: None,
        },
    )
}

fn request(query: &str, task: Option<&str>, subjects: Option<Vec<EntityId>>) -> Request {
    Request {
        binding: RecallRequest {
            request_id: "request-1".into(),
            query: query.into(),
            context: vec![],
            task_id: task.map(str::to_owned),
            at: AT,
        },
        owner: UserId::from(OWNER),
        subject_ids: subjects,
        said_on: SAID_ON.into(),
        utc_offset_seconds: 28_800,
        bootstrap: false,
    }
}

fn plan(request: &Request, output: Option<Json>) -> r1::Plan {
    let entities = [EntityId::from("self"), EntityId::from("child")];
    let relations = [w2::Relation {
        id: "fact".into(),
        memory_types: vec![MemoryType::Fact],
    }];
    let task = request.binding.task_id.as_deref().map(TaskId::from);
    let context = r1::Context {
        query: &request.binding.query,
        context: &request.binding.context,
        entities: &entities,
        relations: &relations,
        subject_ids: request.subject_ids.as_deref(),
        task_id: task.as_ref(),
        said_on: &request.said_on,
        utc_offset_seconds: request.utc_offset_seconds,
        supported_units: &["day", "week", "month", "year"],
    };
    match output {
        Some(output) => r1::validate(r1::decode(&output.to_string()).unwrap(), &context).unwrap(),
        None => r1::not_understood(&context).unwrap(),
    }
}

fn understanding() -> Json {
    json!({"need":"judge","current_evidence":"query_only","type":"aggregate",
        "subject":{"entity_id":"self"},"relation":"外出记录","conditions":[],"time":null,
        "plan":{"op":"count","filter":{"relation":"fact","time":null},"unit":null}})
}

fn run(db: &Db, request: &Request, output: Option<Json>, budget: Budget) -> RecallOutput {
    // 每次均已关闭建库连接并重开，只调用生产离线入口。
    recall::run_offline(&db.open(), request, &plan(request, output), budget).unwrap()
}

fn ids(output: &RecallOutput) -> BTreeSet<MemoryId> {
    output.records.iter().map(RecordView::id).collect()
}

fn relation(db: &Db, id: &str, members: &[MemoryId], status: RelationStatus) {
    let proof_id = format!("{id}-resolution");
    if status == RelationStatus::Resolved {
        source(
            db,
            &proof_id,
            "这组关系已经解决。",
            None,
            "completed_no_facts",
        );
    }
    db.open()
        .transaction(|state| {
            state.relations.insert(
                id.into(),
                StoredRelation {
                    id: id.into(),
                    kind: hippocampus::RelationKind::Conflict,
                    members: members.into(),
                    conditions: json!([]),
                    times: json!([]),
                    basis: vec![],
                    status,
                    resolution: (status == RelationStatus::Resolved).then(|| RelationEvidence {
                        source_id: proof_id.clone(),
                        quote: "这组关系已经解决。".into(),
                    }),
                    created_by: "test-group".into(),
                    closed_by: None,
                },
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn reopened_offline_loop_binds_request_snapshot_and_never_writes() {
    let db = Db::new();
    let id = seed(&db, "first", "常去图书馆。", "self", None, true);
    let before = serde_json::to_value(db.state()).unwrap();
    let mut req = request("统计外出。", None, None);
    req.binding.context = vec!["以前说过图书馆。".into()];
    let result = run(&db, &req, Some(understanding()), Budget::default());
    assert_eq!(ids(&result), [id].into());
    assert_eq!(result.request, req.binding);
    assert_eq!(
        result.snapshot.read_revision,
        before["revision"].as_u64().unwrap()
    );
    assert_eq!(
        result.snapshot.source_revision,
        before["source_revision"].as_u64().unwrap()
    );
    assert_eq!(result.status, RecallStatus::Degraded);
    assert_eq!(result.semantic_judgment, SemanticJudgment::Skipped);
    assert_eq!(
        result.degraded_reasons,
        ["semantic_judgment_skipped:offline"]
    );
    assert!(result.facts.is_empty() && result.computed.is_none() && result.history.is_empty());
    assert_eq!(serde_json::to_value(db.state()).unwrap(), before);
    let wire = serde_json::to_value(&result).unwrap();
    assert_eq!(wire["version"], "recall.v4");
    assert!(wire["records"][0].get("end").is_none());
    assert!(wire["records"][0].get("current").is_none());
    assert_eq!(
        serde_json::from_value::<RecallOutput>(wire).unwrap(),
        result
    );
}

#[test]
fn trusted_task_subject_and_owner_scope_cannot_be_expanded() {
    let db = Db::new();
    let global = seed(&db, "global", "全局记录。", "self", None, true);
    let mine = seed(&db, "mine", "本任务记录。", "self", Some("mine"), true);
    seed(&db, "other", "另一任务记录。", "self", Some("other"), true);
    seed(&db, "child", "孩子的记录。", "child", Some("mine"), true);
    source(&db, "other-raw", "本任务记录。", Some("other"), "queued");
    let req = request("记录", Some("mine"), Some(vec![EntityId::from("self")]));
    let result = run(&db, &req, None, Budget::default());
    assert_eq!(ids(&result), [global, mine].into());
    assert!(!serde_json::to_string(&result).unwrap().contains("另一任务"));
    assert!(
        !serde_json::to_string(&result)
            .unwrap()
            .contains("孩子的记录")
    );
    assert!(result.raw.is_empty());
    let globals = run(&db, &request("记录", None, None), None, Budget::default());
    assert_eq!(ids(&globals), [global].into());
    let none = run(
        &db,
        &request("记录", Some("mine"), Some(vec![])),
        None,
        Budget::default(),
    );
    assert!(none.records.is_empty() && none.raw.is_empty());
    let mut wrong = request("记录", None, None);
    wrong.owner = UserId::from("other-owner");
    assert!(recall::assemble(&db.open(), &wrong, &plan(&wrong, None), Budget::default()).is_err());
}

#[test]
fn current_message_is_pinned_and_query_only_is_not_judgment_evidence() {
    let db = Db::new();
    let req = request("完全无匹配的问题？", None, None);
    let understood = plan(&req, Some(understanding()));
    let assembly = recall::assemble(
        &db.open(),
        &req,
        &understood,
        Budget {
            records: 0,
            raw: 0,
            relations: 0,
            stored_bytes: 0,
        },
    )
    .unwrap();
    assert!(!assembly.review_current_message() && !assembly.has_judgment_evidence());
    let result = assembly
        .finish_skipped(&db.open(), SkipReason::NoEvidence)
        .unwrap();
    let current = result.current_message.unwrap();
    assert_eq!(current.text, req.binding.query);
    assert_eq!(current.said_on, SAID_ON);
    assert_eq!(current.source_id, None);
    assert_eq!(current.sequence, None);
    assert_eq!(current.enrichment, EnrichmentStatus::NotStored);
    assert!(current.memory_requests.is_empty());
    assert_eq!(
        result.degraded_reasons,
        ["semantic_judgment_skipped:no_evidence"]
    );
    let unknown = recall::assemble(&db.open(), &req, &plan(&req, None), Budget::default()).unwrap();
    assert!(unknown.review_current_message() && unknown.has_judgment_evidence());
    assert!(db.state().sources.is_empty());
}

#[test]
fn context_only_bootstrap_and_budget_skip_keep_the_evidence_package() {
    let db = Db::new();
    let id = seed(&db, "first", "走路去图书馆。", "self", None, true);
    let mut output = understanding();
    output["need"] = json!("context_only");
    output["current_evidence"] = json!("assertion_present");
    let req = request("走路去图书馆。", None, None);
    let result = run(&db, &req, Some(output), Budget::default());
    assert_eq!(result.status, RecallStatus::ContextOnly);
    assert_eq!(
        result.degraded_reasons,
        ["semantic_judgment_skipped:context_only"]
    );
    assert_eq!(ids(&result), [id].into());
    let assembly = recall::assemble(
        &db.open(),
        &req,
        &plan(&req, Some(understanding())),
        Budget::default(),
    )
    .unwrap();
    let evidence = assembly.evidence.clone();
    let skipped = assembly
        .finish_skipped(&db.open(), SkipReason::Budget)
        .unwrap();
    assert_eq!(skipped.records, evidence.records);
    assert_eq!(skipped.raw, evidence.raw);
    assert_eq!(skipped.current_message, evidence.current_message);
    assert_eq!(
        skipped.degraded_reasons,
        ["semantic_judgment_skipped:budget"]
    );
    let mut bootstrap = request("", None, None);
    bootstrap.bootstrap = true;
    assert!(
        run(&db, &bootstrap, None, Budget::default())
            .current_message
            .is_none()
    );
}

#[test]
fn raw_preserves_sequence_status_requests_and_local_received_date() {
    let db = Db::new();
    for (id, status) in [
        ("z-first", "queued"),
        ("a-second", "failed"),
        ("m-third", "stale_requires_review"),
    ] {
        source(&db, id, "保留这一段记录。", None, status);
    }
    db.open()
        .transaction(|state| {
            state.sources.get_mut("a-second").unwrap().result = Some(json!({"w4_controls":[{
                "kind":"remember","quote":{"text":"保留这一段记录"},
                "target":{"object":"source","candidate_indices":[],"quote":null},"deadline":null,
                "status":"unreviewed"
            }]}));
            Ok(())
        })
        .unwrap();
    let result = run(&db, &request("记录", None, None), None, Budget::default());
    assert_eq!(
        result
            .raw
            .iter()
            .map(|r| r.source_id.as_str())
            .collect::<Vec<_>>(),
        ["z-first", "a-second", "m-third"]
    );
    assert_eq!(
        result.raw.iter().map(|r| r.enrichment).collect::<Vec<_>>(),
        [
            EnrichmentStatus::Queued,
            EnrichmentStatus::Failed,
            EnrichmentStatus::NeedsRejudge
        ]
    );
    assert_eq!(
        result.raw[1].memory_requests[0].quote.text,
        "保留这一段记录"
    );
    assert_eq!(result.raw[1].memory_requests[0].kind, RequestKind::Remember);
    assert!(
        result
            .raw
            .iter()
            .all(|r| r.grade == RecallGrade::Uncommitted && r.said_on == SAID_ON)
    );
    assert_eq!(result.completeness.pending_related_sources, 3);
}

#[test]
fn legacy_gaps_and_unknown_update_survive_reopen_with_available_source() {
    let db = Db::new();
    let legacy = seed(&db, "legacy", "周末有时散步。", "self", None, true);
    let missing = seed(&db, "missing", "仅有旧片段。", "self", None, true);
    db.open()
        .transaction(|state| {
            state.record_updates.remove(&legacy);
            state.record_updates.remove(&missing);
            state.w5_receipts.clear();
            let mut memory = state.repository.get(missing)?.unwrap();
            // 模拟无独立原文链接的旧记忆；不删除已登记来源的墓碑。
            memory.evidence[0].session_id = SessionId::from("unlinked-legacy-session");
            memory.temporal.valid_from = Some(DatedValue {
                at: Timestamp(AT),
                source: TemporalSource::Explicit,
            });
            state.repository.update(memory)?;
            Ok(())
        })
        .unwrap();
    let result = run(&db, &request("旧记忆", None, None), None, Budget::default());
    let views: Vec<_> = result
        .records
        .iter()
        .map(|v| match v {
            RecordView::Legacy(v) => v,
            _ => panic!("not legacy"),
        })
        .collect();
    assert!(views.iter().all(|v| v.update.is_none()));
    let available = views.iter().find(|v| v.id == legacy).unwrap();
    assert_eq!(
        available.gaps,
        [LegacyGap::SentenceMissing, LegacyGap::QualifiersUnmapped]
    );
    assert_eq!(available.evidence[0], stored("legacy", "周末有时散步。"));
    let sourceless = views.iter().find(|v| v.id == missing).unwrap();
    assert!(sourceless.evidence.is_empty());
    assert!(sourceless.gaps.contains(&LegacyGap::SourceMissing));
    assert!(sourceless.gaps.contains(&LegacyGap::TimeUnmapped));
    assert_eq!(result.completeness.enrichment_gaps, 6);
    assert_eq!(result.raw[0].source_id, "legacy");
    assert_eq!(result.raw[0].enrichment, EnrichmentStatus::Committed);
    assert_eq!(result.completeness.pending_related_sources, 0);
    assert!(!sourceless.data.evidence.is_empty());
}

#[test]
fn unknown_end_is_not_replaced_by_received_time_or_active_current_yes() {
    let db = Db::new();
    let old = seed(&db, "old", "以前去南馆。", "self", None, true);
    source(&db, "new", "现在去北馆。", None, "running");
    let state = db.state();
    let old_view = w4::record_view(&state, old).unwrap();
    let input = W4Input {
        candidates: vec![candidate("new", "现在去北馆。", "self")],
        records: vec![old_view],
        relations: vec![],
        coverage: W4Coverage {
            complete: true,
            omitted: 0,
        },
    };
    let new = commit(
        &db,
        "new",
        None,
        input,
        W4Decision {
            candidate: 0,
            status: DecisionStatus::Decided,
            action: Some(Action::StateChange),
            targets: vec![old],
            resolves_relations: vec![],
            basis: vec![stored("new", "现在去北馆。"), stored("old", "以前去南馆。")],
            effective: None,
            relation_scope: None,
            pending_reason: None,
        },
    );
    let mut output = understanding();
    output["type"] = json!("temporal");
    output["plan"]["op"] = json!("lookup");
    let result = run(
        &db,
        &request("现在去北馆", None, None),
        Some(output),
        Budget::default(),
    );
    assert_eq!(ids(&result), [old, new].into());
    let old_view = result.records.iter().find(|v| v.id() == old).unwrap();
    match old_view {
        RecordView::Current(v) => {
            assert_eq!(v.stored_status, StoredStatus::Superseded);
            assert_eq!(v.ended, Ended::EndUnknown);
        }
        _ => panic!("missing complete contract"),
    }
    assert_eq!(
        db.state()
            .repository
            .get(old)
            .unwrap()
            .unwrap()
            .temporal
            .valid_to,
        None
    );
    assert!(result.facts.is_empty());
    assert!(serde_json::to_value(old_view).unwrap().get("end").is_none());
}

#[test]
fn all_relation_states_expand_members_and_do_not_cross_trusted_scope() {
    let db = Db::new();
    let a = seed(&db, "a", "alpha selected", "self", Some("mine"), true);
    let b = seed(&db, "b", "beta unrelated", "self", Some("mine"), true);
    let outside = seed(&db, "outside", "hidden phrase", "self", Some("other"), true);
    for (id, status) in [
        ("open", RelationStatus::Open),
        ("resolved", RelationStatus::Resolved),
        ("invalidated", RelationStatus::Invalidated),
    ] {
        relation(&db, id, &[a, b], status);
    }
    relation(&db, "cross-task", &[a, outside], RelationStatus::Open);
    let mut output = understanding();
    output["type"] = json!("single");
    output["relation"] = json!("alpha");
    output["plan"]["op"] = json!("lookup");
    output["plan"]["filter"]["relation"] = Json::Null;
    let req = request("alpha", Some("mine"), None);
    let assembly = recall::assemble(
        &db.open(),
        &req,
        &plan(&req, Some(output)),
        Budget::default(),
    )
    .unwrap();
    assert_eq!(
        assembly
            .evidence
            .records
            .iter()
            .map(RecordView::id)
            .collect::<BTreeSet<_>>(),
        [a, b].into()
    );
    assert_eq!(
        assembly
            .evidence
            .relations
            .iter()
            .map(|r| r.status)
            .collect::<Vec<_>>(),
        [
            LinkStatus::Invalidated,
            LinkStatus::Open,
            LinkStatus::Resolved
        ]
    );
    assert!(!assembly.evidence.completeness.plan_complete);
    assert!(
        assembly
            .evidence
            .relations
            .iter()
            .find(|r| r.status == LinkStatus::Resolved)
            .unwrap()
            .resolution
            .is_some()
    );
    let result = assembly
        .finish_skipped(&db.open(), SkipReason::Offline)
        .unwrap();
    assert_eq!(result.conflicts.len(), 3);
    assert!(
        !serde_json::to_string(&result)
            .unwrap()
            .contains("hidden phrase")
    );
}

#[test]
fn aggregate_enumerates_before_budget_and_counts_clipped_pending_and_gaps() {
    let db = Db::new();
    let first = seed(&db, "first", "周一游泳。", "self", None, false);
    let second = seed(&db, "second", "周五骑车。", "self", None, false);
    let legacy = seed(&db, "legacy", "周日散步。", "self", None, true);
    db.open()
        .transaction(|state| {
            state.record_updates.remove(&legacy);
            Ok(())
        })
        .unwrap();
    source(&db, "raw-1", "尚未整理的外出。", None, "queued");
    source(&db, "raw-2", "失败的外出。", None, "failed");
    let req = request("完全没有字面命中的聚合问题", None, None);
    let full = run(&db, &req, Some(understanding()), Budget::default());
    assert_eq!(ids(&full), [first, second, legacy].into());
    assert!(full.completeness.plan_complete);
    assert_eq!(full.completeness.pending_updates, 2);
    let clipped = run(
        &db,
        &req,
        Some(understanding()),
        Budget {
            records: 1,
            raw: 0,
            ..Budget::default()
        },
    );
    assert_eq!(clipped.records.len(), 1);
    assert!(clipped.raw.is_empty());
    assert_eq!(clipped.completeness.pending_updates, 2);
    assert_eq!(clipped.completeness.pending_related_sources, 2);
    assert_eq!(
        clipped.completeness.enrichment_gaps,
        full.completeness.enrichment_gaps
    );
    assert!(clipped.completeness.retrieval_truncated && !clipped.completeness.plan_complete);
    let record = full.records.iter().find(|v| v.id() == first).unwrap();
    assert!(matches!(record, RecordView::Current(v) if v.update.status == UpdateStatus::Pending));
}

#[test]
fn natural_conditions_are_hints_and_not_understood_does_broad_retrieval() {
    let db = Db::new();
    let a = seed(&db, "a", "周末可能散步。", "self", None, true);
    let b = seed(&db, "b", "工作日骑车。", "self", None, true);
    let req = request("风和日丽时的外出", None, None);
    let mut understood = understanding();
    understood["conditions"] = json!([{"quote":{"text":"风和日丽时"},"resolved":true}]);
    let with_hints = run(&db, &req, Some(understood), Budget::default());
    assert_eq!(ids(&with_hints), [a, b].into());
    let fallback = run(
        &db,
        &request("完全陌生的词", None, None),
        None,
        Budget::default(),
    );
    assert_eq!(ids(&fallback), [a, b].into());
    assert_eq!(
        fallback.degraded_reasons,
        ["r1_not_understood", "semantic_judgment_skipped:offline"]
    );
    assert!(!fallback.completeness.plan_complete);
}

#[test]
fn byte_budget_is_utf8_and_json_safe_and_relation_budget_is_honest() {
    let db = Db::new();
    source(&db, "raw", &"汉字\n\"\u{0000}".repeat(100), None, "queued");
    let req = request("宽检索", None, None);
    let bounded = run(
        &db,
        &req,
        None,
        Budget {
            stored_bytes: 230,
            ..Budget::default()
        },
    );
    assert_eq!(bounded.raw.len(), 1);
    assert!(serde_json::to_vec(&bounded.raw[0]).unwrap().len() <= 230);
    assert!(
        db.state().sources["raw"].request.as_ref().unwrap()["text"]
            .as_str()
            .unwrap()
            .starts_with(&bounded.raw[0].quote.text)
    );
    assert!(bounded.completeness.retrieval_truncated && !bounded.completeness.plan_complete);
    assert_eq!(bounded.completeness.pending_related_sources, 1);
    assert_eq!(bounded.current_message.unwrap().text, req.binding.query);
    let a = seed(&db, "a", "一条记录。", "self", None, true);
    let b = seed(&db, "b", "另一条记录。", "self", None, true);
    relation(&db, "rel", &[a, b], RelationStatus::Open);
    let clipped = run(
        &db,
        &req,
        None,
        Budget {
            relations: 0,
            ..Budget::default()
        },
    );
    assert!(clipped.conflicts.is_empty());
    assert!(clipped.completeness.retrieval_truncated && !clipped.completeness.plan_complete);
}

fn forget(db: &Db, id: &str) {
    let mut service =
        hippocampus::chat::ChatMemoryService::new(db.open(), Default::default()).unwrap();
    service
        .call("memory_forget", json!({"source_id":id}), || false)
        .unwrap();
}

#[test]
fn deletion_before_recall_and_after_assembly_never_returns_deleted_source() {
    let db = Db::new();
    let a = seed(&db, "secret-a", "敏感甲原文。", "self", None, true);
    let b = seed(&db, "keep-b", "仍然获准乙原文。", "self", None, true);
    relation(&db, "rel", &[a, b], RelationStatus::Open);
    source(&db, "secret-raw", "敏感待整理原文。", None, "queued");
    let req = request("宽检索", None, None);
    let assembly =
        recall::assemble(&db.open(), &req, &plan(&req, None), Budget::default()).unwrap();
    forget(&db, "secret-a");
    assert_eq!(
        assembly
            .finish_skipped(&db.open(), SkipReason::Offline)
            .unwrap_err()
            .to_string(),
        "recall_snapshot_changed"
    );
    forget(&db, "secret-raw");
    let result = run(&db, &req, None, Budget::default());
    assert_eq!(ids(&result), [b].into());
    assert!(result.raw.is_empty());
    assert_eq!(result.conflicts[0].status, LinkStatus::Invalidated);
    let wire = serde_json::to_string(&result).unwrap();
    assert!(!wire.contains("敏感甲") && !wire.contains("敏感待整理"));
}

#[test]
fn changed_enrichment_progress_returns_the_original_read_snapshot() {
    let db = Db::new();
    source(&db, "raw", "未完成的记录。", None, "queued");
    let req = request("记录", None, None);
    let assembly =
        recall::assemble(&db.open(), &req, &plan(&req, None), Budget::default()).unwrap();
    let snapshot = assembly.snapshot;
    db.open()
        .transaction(|state| {
            state.sources.get_mut("raw").unwrap().enrichment = "failed".into();
            Ok(())
        })
        .unwrap();
    let state = db.state();
    assert_eq!(state.revision, snapshot.read_revision);
    assert_eq!(state.source_revision, snapshot.source_revision);
    let out = assembly
        .finish_skipped(&db.open(), SkipReason::Offline)
        .unwrap();
    assert_eq!(out.snapshot, snapshot);
    assert_eq!(out.raw[0].enrichment, EnrichmentStatus::Queued);
    assert_eq!(
        run(&db, &req, None, Budget::default()).raw[0].enrichment,
        EnrichmentStatus::Failed
    );
}

#[test]
fn r1_request_mismatch_is_rejected_and_unknown_source_status_is_a_gap() {
    let db = Db::new();
    source(
        &db,
        "old-status",
        "旧来源记录。",
        None,
        "completed_rejected",
    );
    let mut req = request("记录", None, None);
    let planned = plan(&req, None);
    req.binding.context.push("另一个请求的上下文".into());
    assert!(recall::assemble(&db.open(), &req, &planned, Budget::default()).is_err());
    let result = run(&db, &req, None, Budget::default());
    assert!(result.raw.is_empty());
    assert_eq!(result.completeness.enrichment_gaps, 1);
    assert!(!result.completeness.plan_complete);
}

#[test]
fn disabled_index_uses_phrase_ranking_before_recency_without_writes() {
    let db = Db::new();
    let strong = seed(&db, "older", "周末喜欢喝咖啡。", "self", None, true);
    seed(&db, "newer", "周末散步。", "self", None, true);
    let req = request("周末喜欢喝咖啡", None, None);
    let mut understood = understanding();
    understood["type"] = json!("single");
    understood["relation"] = json!("喝咖啡");
    understood["plan"]["op"] = json!("lookup");
    understood["plan"]["filter"]["relation"] = Json::Null;
    let mut store = db.open();
    store.set_source_search_enabled(false);
    let before = serde_json::to_value(store.read().unwrap()).unwrap();
    let output = recall::run_offline(
        &store,
        &req,
        &plan(&req, Some(understood)),
        Budget {
            records: 1,
            ..Budget::default()
        },
    )
    .unwrap();
    assert_eq!(ids(&output), [strong].into());
    assert!(output.completeness.retrieval_truncated && !output.completeness.plan_complete);
    assert_eq!(serde_json::to_value(db.state()).unwrap(), before);
}

#[test]
fn subject_limited_unclassified_raw_is_not_assigned_by_guessing() {
    let db = Db::new();
    source(&db, "unknown", "孩子和本人都有新安排。", None, "queued");
    let req = request("安排", None, Some(vec![EntityId::from("self")]));
    let output = run(&db, &req, Some(understanding()), Budget::default());
    assert!(output.raw.is_empty());
    assert!(!output.completeness.plan_complete);
    assert_eq!(output.completeness.pending_related_sources, 0);
    assert!(!output.completeness.retrieval_truncated);
    let broad = run(&db, &request("安排", None, None), None, Budget::default());
    assert_eq!(broad.raw.len(), 1);
    assert_eq!(broad.completeness.pending_related_sources, 1);
}

fn lookup_understanding() -> Json {
    let mut output = understanding();
    output["type"] = json!("single");
    output["relation"] = json!("居住地点");
    output["plan"]["op"] = json!("lookup");
    output
}

#[test]
fn lookup_raw_includes_all_unfinished_states_and_budgets_newest_first() {
    let db = Db::new();
    let states = [
        "queued",
        "running",
        "failed",
        "stale_requires_review",
        "awaiting_confirmation",
        "completed_pending",
        "coverage_gaps",
        "no_candidates_unreviewed",
        "budget_exhausted",
        "not_requested",
    ];
    for (index, status) in states.iter().enumerate() {
        source(
            &db,
            &format!("raw-{index:02}"),
            "炉灶已停用。",
            None,
            status,
        );
    }
    let req = request("我住在哪？", None, None);
    let before = serde_json::to_value(db.state()).unwrap();
    let full = run(&db, &req, Some(lookup_understanding()), Budget::default());
    assert_eq!(full.raw.len(), states.len());
    assert_eq!(full.completeness.pending_related_sources, states.len());
    assert!(full.completeness.plan_complete && !full.completeness.retrieval_truncated);
    assert!(
        full.raw
            .windows(2)
            .all(|rows| rows[0].sequence < rows[1].sequence)
    );
    let newest = &full.raw[full.raw.len() - 2..];
    let bytes = newest
        .iter()
        .map(|r| serde_json::to_vec(r).unwrap().len())
        .sum();
    for budget in [
        Budget {
            raw: 2,
            ..Budget::default()
        },
        Budget {
            stored_bytes: bytes,
            ..Budget::default()
        },
    ] {
        let clipped = run(&db, &req, Some(lookup_understanding()), budget);
        assert_eq!(clipped.raw, newest);
        assert_eq!(clipped.completeness.pending_related_sources, states.len());
        assert!(clipped.completeness.retrieval_truncated && !clipped.completeness.plan_complete);
    }
    let empty = run(
        &db,
        &req,
        Some(lookup_understanding()),
        Budget {
            raw: 0,
            ..Budget::default()
        },
    );
    assert!(empty.raw.is_empty());
    assert_eq!(empty.completeness.pending_related_sources, states.len());
    assert!(empty.completeness.retrieval_truncated && !empty.completeness.plan_complete);
    assert_eq!(serde_json::to_value(db.state()).unwrap(), before);
}

#[test]
fn lookup_unfinished_raw_keeps_task_owner_and_subject_boundaries() {
    let db = Db::new();
    source(&db, "global", "炉灶已停用。", None, "queued");
    source(&db, "mine", "颜色换了。", Some("mine"), "failed");
    source(
        &db,
        "other-task",
        "别的任务私密内容。",
        Some("other"),
        "awaiting_confirmation",
    );
    let own = seed(
        &db,
        "classified-self",
        "物品位置换了。",
        "self",
        Some("mine"),
        true,
    );
    let child = seed(
        &db,
        "classified-child",
        "尺寸变了。",
        "child",
        Some("mine"),
        true,
    );
    let mixed = seed(&db, "mixed", "安排变了。", "self", Some("mine"), true);
    db.open()
        .transaction(|state| {
            for sid in ["classified-self", "classified-child", "mixed"] {
                state.sources.get_mut(sid).unwrap().enrichment = "needs_rejudge".into();
            }
            let mut second = state.repository.get(mixed)?.unwrap();
            second.id = state.repository.allocate_id()?;
            second.subject_id = EntityId::from("child");
            state.repository.insert(second)?;
            Ok(())
        })
        .unwrap();
    let mut foreign = db.state().sources["global"].clone();
    foreign.id = "foreign-owner".into();
    foreign.turn_id = foreign.id.clone();
    foreign.request = Some(json!({"text":"其他归属的私密内容。","capture_source":true}));
    SqliteMemoryStore::open(&db.0, UserId::from("other-owner"))
        .unwrap()
        .transaction(|state| {
            state.sources.insert(foreign.id.clone(), foreign.clone());
            Ok(())
        })
        .unwrap();
    let req = request("我住在哪？", Some("mine"), None);
    let broad = run(&db, &req, Some(lookup_understanding()), Budget::default());
    let visible: BTreeSet<_> = broad.raw.iter().map(|r| r.source_id.as_str()).collect();
    assert_eq!(
        visible,
        [
            "global",
            "mine",
            "classified-self",
            "classified-child",
            "mixed"
        ]
        .into()
    );
    // R1 的主体提示不删除可信范围内、未被其记录种子选中的来源。
    assert!(ids(&broad).contains(&own) && !ids(&broad).contains(&child));
    assert_eq!(broad.completeness.pending_related_sources, 5);
    let narrow = run(
        &db,
        &request(
            "我住在哪？",
            Some("mine"),
            Some(vec![EntityId::from("self")]),
        ),
        Some(lookup_understanding()),
        Budget::default(),
    );
    assert_eq!(
        narrow
            .raw
            .iter()
            .map(|r| r.source_id.as_str())
            .collect::<Vec<_>>(),
        ["classified-self"]
    );
    assert_eq!(narrow.completeness.pending_related_sources, 1);
    assert!(!narrow.completeness.plan_complete && !narrow.completeness.retrieval_truncated);
    let empty = run(
        &db,
        &request("我住在哪？", Some("mine"), Some(vec![])),
        None,
        Budget::default(),
    );
    assert!(empty.raw.is_empty());
}

#[test]
fn lookup_completed_sources_keep_legacy_relevance_and_do_not_become_pending_raw() {
    let db = Db::new();
    seed(&db, "current", "我住在杭州。", "self", None, true);
    let related = seed(&db, "legacy-self", "周末看书。", "self", None, true);
    let unrelated = seed(&db, "legacy-child", "每天画画。", "child", None, true);
    source(&db, "no-facts", "我住在哪？", None, "completed_no_facts");
    source(&db, "unlinked-completed", "我住在哪？", None, "committed");
    db.open()
        .transaction(|state| {
            state.record_updates.remove(&related);
            state.record_updates.remove(&unrelated);
            Ok(())
        })
        .unwrap();
    let result = run(
        &db,
        &request("我住在哪？", None, None),
        Some(lookup_understanding()),
        Budget::default(),
    );
    assert_eq!(
        result
            .raw
            .iter()
            .map(|r| r.source_id.as_str())
            .collect::<Vec<_>>(),
        ["legacy-self"]
    );
    assert_eq!(result.raw[0].enrichment, EnrichmentStatus::Committed);
    assert_eq!(result.completeness.pending_related_sources, 0);
    assert!(ids(&result).contains(&related) && !ids(&result).contains(&unrelated));
}

#[test]
fn recorded_r1_relation_filters_cannot_hide_stronger_or_only_authorized_evidence() {
    fn output(row: &Json) -> Json {
        let body: Json = serde_json::from_str(row["body"].as_str().unwrap()).unwrap();
        let text = body["output"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|m| m["content"].as_array().into_iter().flatten())
            .filter(|c| c["type"] == "output_text")
            .map(|c| c["text"].as_str().unwrap())
            .collect::<String>();
        serde_json::from_str(&text).unwrap()
    }
    let root = "tests/eval/end_to_end/ac2-pro-medium-pro-minimal-pro-minimal-r4-compute-low";
    for (file, cid, qid, mid, relation_id) in [
        ("lme_5831f84d.json", "lme_5831f84d", "q", "s2_t1", "fact"),
        (
            "s6_task_isolation.json",
            "task_isolation",
            "task_q2",
            "task_s2",
            "response_language",
        ),
    ] {
        let case: Json =
            serde_json::from_slice(&std::fs::read(format!("tests/eval/cases/{file}")).unwrap())
                .unwrap();
        let trace: Json =
            serde_json::from_slice(&std::fs::read(format!("{root}/{cid}.json")).unwrap()).unwrap();
        let question = case["questions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|q| q["id"] == qid)
            .unwrap();
        let message = case["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == mid)
            .unwrap();
        let understanding = output(&trace["operations"][format!("recall:{qid}")][0]);
        let proposed = output(&trace["operations"][format!("write:{mid}")][0]);
        let c: Candidate = serde_json::from_value(
            proposed["candidates"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["relation"] == relation_id)
                .unwrap()
                .clone(),
        )
        .unwrap();
        let Subject::Entity(subject) = &c.subject else {
            panic!("recorded entity")
        };
        let subject = subject.entity_id.clone();
        let db = Db::new();
        db.open()
            .transaction(|s| {
                s.entities.upsert(Entity {
                    id: subject.as_str().into(),
                    owner_id: OWNER.into(),
                    entity_type: EntityType::Person,
                    canonical_name: subject.clone(),
                    aliases: vec![],
                });
                Ok(())
            })
            .unwrap();
        let task = question["task_id"].as_str();
        source(&db, mid, message["text"].as_str().unwrap(), task, "running");
        let mut wanted = candidate(mid, message["text"].as_str().unwrap(), &subject);
        wanted.candidate = c;
        let input = W4Input {
            candidates: vec![wanted],
            records: vec![],
            relations: vec![],
            coverage: W4Coverage {
                complete: true,
                omitted: 0,
            },
        };
        let target = commit(
            &db,
            mid,
            task,
            input,
            W4Decision {
                candidate: 0,
                status: DecisionStatus::Decided,
                action: Some(Action::New),
                targets: vec![],
                resolves_relations: vec![],
                basis: vec![stored(mid, message["text"].as_str().unwrap())],
                effective: None,
                relation_scope: None,
                pending_reason: None,
            },
        );
        if cid == "lme_5831f84d" {
            let distractor = seed(
                &db,
                "unrelated",
                "I use Kaggle and Reddit.",
                &subject,
                None,
                true,
            );
            db.open()
                .transaction(|s| {
                    let mut m = s.repository.get(distractor)?.unwrap();
                    m.predicate = "uses".into();
                    s.repository.update(m)?;
                    Ok(())
                })
                .unwrap();
        }
        let outside = seed(
            &db,
            "outside",
            "Other task's response language.",
            &subject,
            Some("other-task"),
            true,
        );
        let req = request(
            question["query"].as_str().unwrap(),
            task,
            Some(vec![subject.as_str().into()]),
        );
        let entities = [EntityId::from(subject.as_str())];
        let relations: Vec<_> = ["fact", "uses", "response_language"]
            .into_iter()
            .map(|id| w2::Relation {
                id: id.into(),
                memory_types: vec![MemoryType::Fact, MemoryType::Constraint],
            })
            .collect();
        let task_id = task.map(TaskId::from);
        let ctx = r1::Context {
            query: &req.binding.query,
            context: &req.binding.context,
            entities: &entities,
            relations: &relations,
            subject_ids: req.subject_ids.as_deref(),
            task_id: task_id.as_ref(),
            said_on: &req.said_on,
            utc_offset_seconds: req.utc_offset_seconds,
            supported_units: &["day", "week", "month", "year"],
        };
        let understood =
            r1::validate(r1::decode(&understanding.to_string()).unwrap(), &ctx).unwrap();
        assert!(understood.output.is_some());
        assert_ne!(
            understood
                .output
                .as_ref()
                .unwrap()
                .plan
                .filter
                .relation
                .as_deref(),
            Some(relation_id),
            "the captured R1 filter excludes the correct record"
        );
        let assembly = recall::assemble(&db.open(), &req, &understood, Budget::default()).unwrap();
        let ids: BTreeSet<_> = assembly
            .evidence
            .records
            .iter()
            .map(RecordView::id)
            .collect();
        assert!(
            ids.contains(&target),
            "{cid}: captured filter must not hide the answer evidence"
        );
        assert!(
            !ids.contains(&outside),
            "fallback must retain task authorization"
        );
        assert!(assembly.evidence.completeness.plan_complete);
        let tight = recall::assemble(
            &db.open(),
            &req,
            &understood,
            Budget {
                records: 0,
                ..Budget::default()
            },
        )
        .unwrap();
        assert!(!tight.evidence.completeness.plan_complete);
    }
}
