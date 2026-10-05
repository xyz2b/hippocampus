//! 召回②：真实 SQLite、生产组装/跳过出口与重开；不使用模型或评测样本。
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use hippocampus::RelationStatus;
use hippocampus::contract::*;
use hippocampus::recall::{self, Budget, JudgmentFailure, Request};
use hippocampus::*;
use serde_json::{Value as Json, json};

const OWNER: &str = "recall-owner";
const SAID_ON: &str = "2026-04-15";
const AT: i64 = 1_776_249_600;

struct Db(PathBuf);
impl Db {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!("recall-r4-{}", diagnostics::new_id()));
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

fn seed_on(
    db: &Db,
    id: &str,
    text: &str,
    subject: &str,
    task: Option<&str>,
    complete: bool,
    said_on: &str,
) -> MemoryId {
    source(db, id, text, task, "running");
    db.open()
        .transaction(|state| {
            state.sources.get_mut(id).unwrap().observed_at = at(said_on);
            Ok(())
        })
        .unwrap();
    let mut c = candidate(id, text, subject);
    c.said_on = said_on.into();
    let input = W4Input {
        candidates: vec![c],
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

fn at(day: &str) -> i64 {
    let parts: Vec<_> = day.split('-').collect();
    time::Date::from_calendar_date(
        parts[0].parse().unwrap(),
        time::Month::try_from(parts[1].parse::<u8>().unwrap()).unwrap(),
        parts[2].parse().unwrap(),
    )
    .unwrap()
    .midnight()
    .assume_offset(time::UtcOffset::from_whole_seconds(28_800).unwrap())
    .unix_timestamp()
}

// 只在测试端展开已复核样本 input：真实来源、W5 已提交记录和 SQLite 关系。
// 不读 expect/must_not；R4 是人工独立构造，经生产 assemble/finish_r4 运行。
struct Fixture {
    db: Db,
    input: Json,
    ids: BTreeMap<u64, MemoryId>,
}
impl Fixture {
    fn sample(prefix: &str) -> Self {
        let path = std::fs::read_dir("tests/eval/r4/samples")
            .unwrap()
            .map(Result::unwrap)
            .find(|e| e.file_name().to_str().unwrap().starts_with(prefix))
            .unwrap()
            .path();
        let sample: Json = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        Self::new(sample["input"].clone())
    }
    fn new(input: Json) -> Self {
        let db = Db::new();
        let mut ids = BTreeMap::new();
        for r in input["records"].as_array().unwrap() {
            let sid = r["source"].as_str().unwrap_or("legacy-seed");
            let text = r["sentence"].as_str().unwrap_or("旧条目");
            let id = seed_on(
                &db,
                sid,
                text,
                "self",
                None,
                r["update"] == "judged",
                r["said_on"].as_str().unwrap(),
            );
            ids.insert(r["id"].as_u64().unwrap(), id);
            db.open()
                .transaction(|state| {
                    let mut m = state.repository.get(id)?.unwrap();
                    m.status = match r["status"].as_str().unwrap() {
                        "superseded" => MemoryStatus::Superseded,
                        "expired" => MemoryStatus::Expired,
                        _ => MemoryStatus::Active,
                    };
                    m.supersede_reason = match r["termination"].as_str() {
                        Some("state_changed") => Some(SupersedeReason::StateChanged),
                        Some("correction") => Some(SupersedeReason::Correction),
                        Some("user_retracted" | "retracted") => {
                            Some(SupersedeReason::UserRetracted)
                        }
                        Some("preference_changed") => Some(SupersedeReason::PreferenceChanged),
                        Some("constraint_changed") => Some(SupersedeReason::ConstraintChanged),
                        _ => None,
                    };
                    if let Some(end) = r["end"].as_str() {
                        m.temporal.valid_to = Some(DatedValue {
                            at: Timestamp(at(end)),
                            source: TemporalSource::Explicit,
                        });
                    }
                    if r["source"].is_null() {
                        m.evidence.clear();
                        state.w5_receipts.clear();
                        for s in state.sources.values_mut() {
                            s.request = None;
                            s.result = None;
                            s.enrichment = "deleted".into();
                            s.error = Some("source_deleted".into());
                        }
                        state.record_updates.remove(&id);
                    }
                    state.repository.update(m)?;
                    Ok(())
                })
                .unwrap();
        }
        for raw in input["raw"].as_array().unwrap() {
            let sid = raw["source"].as_str().unwrap();
            source(
                &db,
                sid,
                raw["text"].as_str().unwrap(),
                None,
                raw["enrichment"].as_str().unwrap(),
            );
            db.open()
                .transaction(|state| {
                    state.sources.get_mut(sid).unwrap().observed_at =
                        at(raw["said_on"].as_str().unwrap());
                    Ok(())
                })
                .unwrap();
        }
        for link in input["relations"].as_array().unwrap() {
            let members: Vec<_> = link["members"]
                .as_array()
                .unwrap()
                .iter()
                .map(|id| ids[&id.as_u64().unwrap()])
                .collect();
            db.open()
                .transaction(|state| {
                    let mut basis = Vec::new();
                    for id in &members {
                        let m = state.repository.get(*id)?.unwrap();
                        for e in m.evidence {
                            basis.push(RelationEvidence {
                                source_id: e.turn_id.0,
                                quote: e.quote,
                            });
                        }
                    }
                    let id = link["id"].as_str().unwrap().to_owned();
                    state.relations.insert(
                        id.clone(),
                        StoredRelation {
                            id,
                            kind: hippocampus::RelationKind::Conflict,
                            members: members.clone(),
                            conditions: json!([]),
                            times: json!([]),
                            basis,
                            status: RelationStatus::Open,
                            resolution: None,
                            created_by: "fixture".into(),
                            closed_by: None,
                        },
                    );
                    Ok(())
                })
                .unwrap();
        }
        Self { db, input, ids }
    }
    fn request(&self) -> Request {
        Request {
            binding: RecallRequest {
                request_id: "r4-request".into(),
                query: self.input["query"].as_str().unwrap().into(),
                context: vec![],
                task_id: None,
                at: at(self.input["at"].as_str().unwrap()),
            },
            owner: UserId::from(OWNER),
            subject_ids: None,
            said_on: self.input["at"].as_str().unwrap().into(),
            utc_offset_seconds: 28_800,
            bootstrap: false,
        }
    }
    fn plan_json(&self) -> Json {
        json!({"need":"judge","current_evidence":self.input["current_evidence"],"type":self.input["plan"]["type"],
            "subject":{"entity_id":"self"},"relation":self.input["plan"]["relation"],"conditions":[],"time":null,
            "plan":{"op":self.input["plan"]["op"],"filter":{"relation":"fact","time":null},"unit":null}})
    }
    fn assembly_with(&self, req: &Request, output: Json, budget: Budget) -> recall::Assembly {
        let entities = [EntityId::from("self"), EntityId::from("child")];
        let relations = [w2::Relation {
            id: "fact".into(),
            memory_types: vec![MemoryType::Fact],
        }];
        let context = r1::Context {
            query: &req.binding.query,
            context: &req.binding.context,
            entities: &entities,
            relations: &relations,
            subject_ids: None,
            task_id: None,
            said_on: &req.said_on,
            utc_offset_seconds: req.utc_offset_seconds,
            supported_units: &["day", "week", "month", "year", "kg", "g", "m", "cm", "元"],
        };
        let plan = r1::validate(r1::decode(&output.to_string()).unwrap(), &context).unwrap();
        assert!(plan.output.is_some());
        recall::assemble(&self.db.open(), req, &plan, budget).unwrap()
    }
    fn assembly(&self) -> recall::Assembly {
        self.assembly_with(&self.request(), self.plan_json(), Budget::default())
    }
    fn finish(&self, output: Json) -> RecallOutput {
        self.assembly()
            .finish_r4(&self.db.open(), Ok(&output.to_string()))
            .unwrap()
    }
    fn fact(&self, id: u64, current: &str) -> Json {
        let r = self.input["records"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == id)
            .unwrap();
        fact(
            r["sentence"].as_str().unwrap(),
            vec![stored(
                r["source"].as_str().unwrap(),
                r["sentence"].as_str().unwrap(),
            )],
            vec![self.ids[&id]],
            current,
        )
    }
    fn item(&self, id: u64, group: Option<&str>) -> Json {
        let f = self.fact(id, "yes");
        json!({"id":id.to_string(),"evidence":f["evidence"],"quantity":null,"date":null,"date_source":null,"event_group":group})
    }
}
fn fact(statement: &str, evidence: Vec<EvidenceRef>, ids: Vec<MemoryId>, current: &str) -> Json {
    json!({"statement":statement,"evidence":evidence,"record_ids":ids,"polarity":"affirm","strength":null,
        "uncertainty":null,"conditions":[],"times":[],"current":current,"applies":"yes","supersedes_record_ids":[]})
}
fn output(facts: Vec<Json>, compute: Option<Json>) -> Json {
    json!({"verdict":"supported","facts":facts,"compute":compute})
}
fn compute(
    op: &str,
    items: Vec<Json>,
    operands: Vec<&str>,
    unit: Option<&str>,
    coverage: &str,
) -> Json {
    json!({"op":op,"items":items,"operands":operands,"unit":unit,"coverage":coverage})
}
fn invalid(f: &Fixture, output: Json) {
    let result = f.finish(output);
    assert_eq!(result.status, RecallStatus::Degraded);
    assert_eq!(result.semantic_judgment, SemanticJudgment::Failed);
    assert!(result.facts.is_empty() && result.computed.is_none() && result.history.is_empty());
    assert!(
        result
            .degraded_reasons
            .iter()
            .any(|r| r == "model_invalid_output")
    );
}
fn insufficient(result: RecallOutput) {
    assert_eq!(result.status, RecallStatus::Insufficient);
    assert_eq!(result.semantic_judgment, SemanticJudgment::Model);
    assert!(result.computed.is_none());
    assert!(
        result
            .degraded_reasons
            .iter()
            .any(|r| r == "compute_insufficient")
    );
}

fn missing_old_basis(f: &Fixture) -> R4Output {
    let raw = &f.input["raw"][0];
    let mut view = fact(
        raw["text"].as_str().unwrap(),
        vec![stored(
            raw["source"].as_str().unwrap(),
            raw["text"].as_str().unwrap(),
        )],
        vec![],
        "yes",
    );
    view["supersedes_record_ids"] = json!([f.ids[&4]]);
    r4::decode(&output(vec![view], None).to_string()).unwrap()
}

#[test]
fn old_basis_assembly_checks_real_sources_and_preserves_missing_new_basis_errors() {
    let f = Fixture::sample("r4_04_");
    let assembly = f.assembly();
    let dates = BTreeMap::new();
    let ctx = r4::Context {
        evidence: &assembly.evidence,
        plan: &assembly.plan,
        request: &assembly.request,
        utc_offset_seconds: 28_800,
        source_dates: &dates,
    };
    let state = f.db.state();
    let text_of = |id: &str| {
        state
            .sources
            .get(id)
            .filter(|s| s.enrichment != "deleted")
            .and_then(|s| s.request.as_ref()?["text"].as_str().map(str::to_owned))
    };
    let raw = missing_old_basis(&f);
    let assembled = r4::assemble_old_basis(&raw, &ctx, &text_of);
    assert_eq!(raw.facts[0].evidence.len(), 1);
    assert_eq!(assembled.facts[0].evidence.len(), 2);
    assert!(r4::evaluate(assembled.clone(), &ctx).is_ok());
    assert_eq!(
        r4::assemble_old_basis(&assembled, &ctx, &text_of),
        assembled
    );

    // 同来源的已核准短片段/空白变体已足够，无须再复制整句。
    let mut equivalent = raw.clone();
    equivalent.facts[0]
        .evidence
        .push(stored("s-4", "住在上 海"));
    assert_eq!(
        r4::assemble_old_basis(&equivalent, &ctx, &text_of),
        equivalent
    );
    assert!(r4::evaluate(equivalent, &ctx).is_ok());

    // 目标不在本包、旧来源已删/丢失、Quote 不在真实正文：均不能以记录 Quote 自证。
    let mut outside = raw.clone();
    outside.facts[0].supersedes_record_ids = vec![MemoryId(90000)];
    assert_eq!(r4::assemble_old_basis(&outside, &ctx, &text_of), outside);
    assert_eq!(r4::evaluate(outside, &ctx).err().unwrap().0, "record_id");
    for old_body in [
        None,
        Some("另一段不含旧引用的正文".to_owned()),
        Some("我住在上 海。".to_owned()),
    ] {
        let assembled = r4::assemble_old_basis(&raw, &ctx, &|id| {
            if id == "s-4" {
                old_body.clone()
            } else {
                text_of(id)
            }
        });
        assert_eq!(assembled, raw);
        assert_eq!(
            r4::evaluate(assembled, &ctx).err().unwrap().0,
            "record_basis"
        );
    }
    let mut only_old = raw.clone();
    only_old.facts[0].evidence = vec![stored("s-4", "我住在上海。")];
    assert_eq!(r4::assemble_old_basis(&only_old, &ctx, &text_of), only_old);
    assert_eq!(r4::evaluate(only_old, &ctx).err().unwrap().0, "new_basis");
    let mut bad_record = raw.clone();
    bad_record.facts[0].record_ids = vec![f.ids[&4]];
    assert_eq!(
        r4::assemble_old_basis(&bad_record, &ctx, &text_of),
        bad_record
    );
    assert_eq!(
        r4::evaluate(bad_record, &ctx).err().unwrap().0,
        "record_basis"
    );
}

#[test]
fn record_basis_assembly_requires_package_and_live_exact_source_and_preserves_other_errors() {
    let f = Fixture::new(json!({
        "query":"请核对我住在成都的记录。", "at":SAID_ON, "current_evidence":"query_only",
        "plan":{"type":"single","relation":"居住城市","op":"lookup"},
        "records":[{"id":11,"source":"s-record","sentence":"我住在成都。","said_on":SAID_ON,
            "status":"active","update":"judged","termination":null,"end":null}],
        "raw":[{"source":"s-repeat","text":"我现在住在成都。","said_on":SAID_ON,"enrichment":"failed"}],
        "relations":[]
    }));
    let assembly = f.assembly();
    let dates = BTreeMap::new();
    let ctx = r4::Context {
        evidence: &assembly.evidence,
        plan: &assembly.plan,
        request: &assembly.request,
        utc_offset_seconds: 28_800,
        source_dates: &dates,
    };
    let state = f.db.state();
    let text_of = |id: &str| {
        state
            .sources
            .get(id)
            .filter(|s| s.enrichment != "deleted")
            .and_then(|s| s.request.as_ref()?["text"].as_str().map(str::to_owned))
    };
    let mut view = f.fact(11, "yes");
    view["evidence"] = json!([stored("s-repeat", "我现在住在成都。")]);
    let raw_text = output(vec![view], None).to_string();
    let raw = r4::decode(&raw_text).unwrap();
    assert_eq!(
        r4::evaluate(raw.clone(), &ctx).err().unwrap().0,
        "record_basis"
    );
    let final_output = r4::assemble_old_basis(&raw, &ctx, &text_of);
    assert_eq!(final_output.facts[0].evidence.len(), 2);
    assert!(r4::evaluate(final_output.clone(), &ctx).is_ok());
    assert_eq!(
        r4::assemble_old_basis(&final_output, &ctx, &text_of),
        final_output
    );
    let (result, audit) = f
        .assembly()
        .finish_r4_with_audit(&f.db.open(), Ok(&raw_text))
        .unwrap();
    assert_eq!(result.semantic_judgment, SemanticJudgment::Model);
    let audit = audit.unwrap();
    assert_eq!(audit.raw_output, raw_text);
    assert_eq!(audit.final_output.unwrap().facts[0].evidence.len(), 2);
    assert_eq!(
        serde_json::to_value(f.db.state()).unwrap(),
        serde_json::to_value(&state).unwrap()
    );

    let mut equivalent = raw.clone();
    equivalent.facts[0]
        .evidence
        .push(stored("s-record", "住在成 都"));
    assert_eq!(
        r4::assemble_old_basis(&equivalent, &ctx, &text_of),
        equivalent
    );
    let mut outside = raw.clone();
    outside.facts[0].record_ids = vec![MemoryId(90000)];
    assert_eq!(r4::assemble_old_basis(&outside, &ctx, &text_of), outside);
    assert_eq!(r4::evaluate(outside, &ctx).err().unwrap().0, "record_id");
    for body in [
        None,
        Some("另一段正文".into()),
        Some("我住在成 都。".into()),
    ] {
        let assembled = r4::assemble_old_basis(&raw, &ctx, &|id| {
            if id == "s-record" {
                body.clone()
            } else {
                text_of(id)
            }
        });
        assert_eq!(assembled, raw);
        assert_eq!(
            r4::evaluate(assembled, &ctx).err().unwrap().0,
            "record_basis"
        );
    }
    for (field, value, count, rule) in [
        ("evidence", json!([]), 0, "empty_evidence"),
        (
            "evidence",
            json!([stored("s-repeat", "原文没有这个片段")]),
            1,
            "quote_or_source",
        ),
        (
            "conditions",
            json!([{"quote":{"text":"原文没有这个条件"},"resolved":true}]),
            2,
            "qualifier_source",
        ),
    ] {
        let mut bad = serde_json::to_value(&raw).unwrap();
        bad["facts"][0][field] = value;
        let assembled =
            r4::assemble_old_basis(&r4::decode(&bad.to_string()).unwrap(), &ctx, &text_of);
        assert_eq!(assembled.facts[0].evidence.len(), count);
        assert_eq!(r4::evaluate(assembled, &ctx).err().unwrap().0, rule);
    }
    let mut missing_operands = raw.clone();
    missing_operands.compute = Some(
        serde_json::from_value(compute("elapsed", vec![], vec![], Some("day"), "complete"))
            .unwrap(),
    );
    let mut temporal = f.plan_json();
    temporal["type"] = json!("temporal");
    temporal["plan"]["op"] = json!("elapsed");
    let (result, audit) = f
        .assembly_with(&f.request(), temporal, Budget::default())
        .finish_r4_with_audit(
            &f.db.open(),
            Ok(&serde_json::to_string(&missing_operands).unwrap()),
        )
        .unwrap();
    assert_eq!(result.status, RecallStatus::Degraded);
    assert!(
        audit
            .unwrap()
            .final_output
            .unwrap()
            .compute
            .unwrap()
            .operands
            .is_empty()
    );

    f.db.open()
        .transaction(|s| {
            s.sources.get_mut("s-record").unwrap().enrichment = "deleted".into();
            Ok(())
        })
        .unwrap();
    let after_delete = f.db.state();
    assert_eq!(
        r4::assemble_old_basis(&raw, &ctx, &|id| after_delete
            .sources
            .get(id)
            .filter(|s| s.enrichment != "deleted")
            .and_then(|s| s.request.as_ref()?["text"].as_str().map(str::to_owned))),
        raw
    );
    assert!(
        assembly
            .finish_r4_with_audit(&f.db.open(), Ok(&raw_text))
            .is_err()
    );
}

#[test]
fn old_basis_audit_uses_sqlite_snapshot_and_never_persists_readonly_outputs() {
    let f = Fixture::sample("r4_04_");
    let raw = format!(
        " \n{}\n ",
        serde_json::to_string(&missing_old_basis(&f)).unwrap()
    );
    let before = serde_json::to_value(f.db.state()).unwrap();
    let (result, audit) = f
        .assembly()
        .finish_r4_with_audit(&f.db.open(), Ok(&raw))
        .unwrap();
    assert_eq!(result.status, RecallStatus::Answered);
    let audit = audit.unwrap();
    assert_eq!(audit.raw_output, raw);
    assert_eq!(
        audit.final_output.unwrap().facts[0].evidence,
        result.facts[0].evidence
    );
    assert_eq!(
        r4::decode(&audit.raw_output).unwrap().facts[0]
            .evidence
            .len(),
        1
    );
    assert_eq!(serde_json::to_value(f.db.state()).unwrap(), before);

    let assembly = f.assembly();
    f.db.open()
        .transaction(|s| {
            s.sources.get_mut("s-4").unwrap().enrichment = "deleted".into();
            Ok(())
        })
        .unwrap();
    assert!(
        assembly
            .finish_r4_with_audit(&f.db.open(), Ok(&raw))
            .unwrap_err()
            .to_string()
            .contains("snapshot_changed")
    );
    // 即使删除标记保留正文，新快照也不能补旧依据。
    let (result, audit) = f
        .assembly()
        .finish_r4_with_audit(&f.db.open(), Ok(&raw))
        .unwrap();
    assert_eq!(result.status, RecallStatus::Degraded);
    assert!(result.facts.is_empty());
    assert_eq!(
        audit.unwrap().final_output.unwrap().facts[0].evidence.len(),
        1
    );
}

#[test]
fn old_basis_uses_source_identity_when_sqlite_map_keys_are_session_turn_keys() {
    let mut f = Fixture::sample("r4_04_");
    // 与真实接收一致：sources 的 key 是会话/轮次键，Stored 引用的是独立来源 ID。
    // 首次种入新库；已存在来源的键和身份不能被更改。
    let mut seed = f.db.state();
    seed.sources = seed
        .sources
        .into_values()
        .map(|source| {
            (
                json!([source.session_id, source.turn_id]).to_string(),
                source,
            )
        })
        .collect();
    let db = Db::new();
    db.open()
        .transaction(|s| {
            *s = seed;
            Ok(())
        })
        .unwrap();
    f.db = db;
    let before = serde_json::to_value(f.db.state()).unwrap();
    let assembly = f.assembly();
    assert!(
        assembly
            .evidence
            .records
            .iter()
            .any(|r| r.id() == f.ids[&4])
    );
    let raw = serde_json::to_string(&missing_old_basis(&f)).unwrap();
    let (result, audit) = assembly
        .finish_r4_with_audit(&f.db.open(), Ok(&raw))
        .unwrap();
    assert_eq!(result.status, RecallStatus::Answered);
    let final_output = audit.unwrap().final_output.unwrap();
    assert_eq!(final_output.facts[0].evidence.len(), 2);
    assert_eq!(
        final_output.facts[0].evidence[1].source,
        stored("s-4", "").source
    );
    assert_eq!(serde_json::to_value(f.db.state()).unwrap(), before);
    f.db.open()
        .transaction(|s| {
            s.sources
                .values_mut()
                .find(|source| source.id == "s-4")
                .unwrap()
                .enrichment = "deleted".into();
            Ok(())
        })
        .unwrap();
    assert_eq!(
        f.finish(serde_json::from_str(&raw).unwrap()).status,
        RecallStatus::Degraded
    );
}

#[test]
fn old_basis_assembly_does_not_repair_invalid_quotes_qualifiers_or_operands() {
    let f = Fixture::sample("r4_04_");
    for (field, value, expected_evidence) in [
        ("quote", json!({"text":"正文里没有这个片段"}), 1),
        (
            "conditions",
            json!([{"quote":{"text":"不存在的条件"},"resolved":true}]),
            2,
        ),
        (
            "compute",
            compute("elapsed", vec![], vec![], Some("day"), "complete"),
            2,
        ),
    ] {
        let mut raw = serde_json::to_value(missing_old_basis(&f)).unwrap();
        match field {
            "quote" => raw["facts"][0]["evidence"][0]["quote"] = value,
            "conditions" => raw["facts"][0]["conditions"] = value,
            _ => raw["compute"] = value,
        }
        let mut plan = f.plan_json();
        if field == "compute" {
            plan["type"] = json!("temporal");
            plan["plan"]["op"] = json!("elapsed");
        }
        let assembly = f.assembly_with(&f.request(), plan, Budget::default());
        let (result, audit) = assembly
            .finish_r4_with_audit(&f.db.open(), Ok(&raw.to_string()))
            .unwrap();
        assert_eq!(result.status, RecallStatus::Degraded, "{field}");
        assert!(result.facts.is_empty() && result.computed.is_none());
        let final_output = audit.unwrap().final_output.unwrap();
        assert_eq!(final_output.facts[0].evidence.len(), expected_evidence);
        if field == "compute" {
            assert!(final_output.compute.unwrap().operands.is_empty());
        }
    }
}

#[test]
fn sqlite_reopen_supported_qualifiers_and_snapshot_without_writes() {
    let f = Fixture::sample("r4_02_");
    let mut view = f.fact(2, "unknown");
    view["polarity"] = json!("unknown");
    view["uncertainty"] = json!({"resolved":false, "quote":{"text":"可能"}});
    let before = serde_json::to_value(f.db.state()).unwrap();
    let result = f.finish(output(vec![view.clone()], None));
    assert_eq!(result.status, RecallStatus::Answered);
    assert_eq!(result.semantic_judgment, SemanticJudgment::Model);
    assert_eq!(result.facts[0].grade, RecallGrade::Committed);
    assert_eq!(
        result.facts[0].update.as_ref().unwrap().status,
        UpdateStatus::Judged
    );
    let mut final_view = serde_json::to_value(&result.facts[0]).unwrap();
    final_view.as_object_mut().unwrap().remove("grade");
    final_view.as_object_mut().unwrap().remove("update");
    assert_eq!(final_view, view);
    assert_eq!(result.request, f.request().binding);
    assert_eq!(result.snapshot.read_revision, f.db.state().revision);
    assert_eq!(serde_json::to_value(f.db.state()).unwrap(), before);
    assert_eq!(
        serde_json::from_value::<RecallOutput>(serde_json::to_value(&result).unwrap()).unwrap(),
        result
    );

    let f = Fixture::sample("r4_03_");
    let mut view = f.fact(3, "unknown");
    view["conditions"] = json!([{"resolved":true, "quote":{"text":"画速写的时候"}}]);
    view["applies"] = json!("unknown");
    let result = f.finish(output(vec![view.clone()], None));
    assert_eq!(
        serde_json::to_value(&result.facts[0]).unwrap()["conditions"],
        view["conditions"]
    );
    assert_eq!(result.facts[0].applies, Qualification::Unknown);
}

#[test]
fn r4_invalid_reference_or_schema_rejects_entire_output() {
    let f = Fixture::sample("r4_02_");
    let correct = f.fact(2, "yes");
    for field in [
        "source",
        "quote",
        "record",
        "supersedes",
        "uncertainty",
        "condition",
        "time",
    ] {
        let mut bad = correct.clone();
        match field {
            "source" => bad["evidence"][0]["source"]["source_id"] = json!("outside"),
            "quote" => bad["evidence"][0]["quote"]["text"] = json!("原文没有这一句"),
            "record" => bad["record_ids"] = json!([90000]),
            "supersedes" => bad["supersedes_record_ids"] = json!([90000]),
            "uncertainty" => bad["uncertainty"] = json!({"resolved":true,"quote":{"text":"确定"}}),
            "condition" => bad["conditions"] = json!([{"resolved":true,"quote":{"text":"运动后"}}]),
            "time" => {
                bad["times"] = json!([{"role":"event","quote":{"text":"昨天"},"interp":{"kind":"relative","unit":"day","offset":-1},"anchor":"said_on"}])
            }
            _ => unreachable!(),
        }
        invalid(&f, output(vec![correct.clone(), bad], None));
    }
    for malformed in [
        json!({"verdict":"supported","facts":[]}),
        json!({"verdict":"contradicted","facts":[],"compute":null}),
        json!({"verdict":"supported","facts":[],"compute":null,"extra":true}),
    ] {
        invalid(&f, malformed);
    }
    let mut missing = correct.clone();
    missing.as_object_mut().unwrap().remove("uncertainty");
    invalid(&f, output(vec![missing], None));
    let mut blank = correct;
    blank["statement"] = json!("  ");
    invalid(&f, output(vec![blank], None));
}

#[test]
fn r4_04_raw_update_has_uncommitted_grade_and_dual_basis_supersession() {
    let f = Fixture::sample("r4_04_");
    let mut understood = f.plan_json();
    // 此样本只给 R4 的证据包骨架；用获准的广泛枚举计划构造同等 SQLite 包。
    understood["type"] = json!("aggregate");
    let mut new = fact(
        "现在住南京",
        vec![stored("s-40", "我上周已经搬到南京了。")],
        vec![],
        "yes",
    );
    new["evidence"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::to_value(stored("s-4", "我住在上海。")).unwrap());
    new["supersedes_record_ids"] = json!([f.ids[&4]]);
    let old = f.fact(4, "no");
    let finish = |out: Json| {
        f.assembly_with(&f.request(), understood.clone(), Budget::default())
            .finish_r4(&f.db.open(), Ok(&out.to_string()))
            .unwrap()
    };
    let result = finish(output(vec![new.clone(), old], None));
    assert_eq!(result.status, RecallStatus::Answered);
    assert_eq!(result.facts[0].current, Qualification::Yes);
    assert_eq!(result.facts[0].grade, RecallGrade::Uncommitted);
    assert!(result.facts[0].update.is_none());
    assert_eq!(result.facts[1].current, Qualification::No);
    assert!(result.history.is_empty() && result.conflicts.is_empty());
    let mut only_new = new.clone();
    only_new["evidence"].as_array_mut().unwrap().remove(1);
    let repaired = finish(output(vec![only_new], None));
    assert_eq!(repaired.status, RecallStatus::Answered);
    assert_eq!(repaired.facts[0].evidence.len(), 2);
    let mut only_old = new;
    only_old["evidence"].as_array_mut().unwrap().remove(0);
    let result = finish(output(vec![only_old], None));
    assert_eq!(result.status, RecallStatus::Degraded);
    assert!(result.facts.is_empty());
}

#[test]
fn query_only_legacy_missing_source_and_unlinked_record_cannot_supply_facts() {
    let f = Fixture::sample("r4_06_");
    let fake = fact(
        f.input["query"].as_str().unwrap(),
        vec![EvidenceRef {
            source: SourceRef::Current {},
            quote: Quote {
                text: f.input["query"].as_str().unwrap().into(),
            },
        }],
        vec![],
        "yes",
    );
    invalid(&f, output(vec![fake], None));
    let good = f.finish(json!({"verdict":"insufficient","facts":[],"compute":null}));
    assert_eq!(good.status, RecallStatus::Insufficient);
    let f = Fixture::sample("r4_09_");
    assert!(matches!(
        &f.assembly().evidence.records[0],
        RecordView::Legacy(_)
    ));
    invalid(
        &f,
        output(
            vec![fact(
                "旧条目",
                vec![stored("legacy-seed", "旧条目")],
                vec![f.ids[&9]],
                "yes",
            )],
            None,
        ),
    );
    let f = Fixture::sample("r4_14_");
    let mut wrong = f.fact(18, "yes");
    wrong["record_ids"] = json!([f.ids[&20]]);
    invalid(&f, output(vec![wrong], None));
}

#[test]
fn current_assertion_is_read_only_uncommitted_and_failed_judgment_has_no_facts() {
    let f = Fixture::sample("r4_07_");
    let text = f.input["query"].as_str().unwrap();
    let view = fact(
        text,
        vec![EvidenceRef {
            source: SourceRef::Current {},
            quote: Quote { text: text.into() },
        }],
        vec![],
        "yes",
    );
    let before = serde_json::to_value(f.db.state()).unwrap();
    let result = f.finish(output(vec![view], None));
    assert_eq!(result.facts[0].grade, RecallGrade::Uncommitted);
    assert!(result.facts[0].update.is_none());
    assert_eq!(serde_json::to_value(f.db.state()).unwrap(), before);
    let result = f
        .assembly()
        .finish_r4(&f.db.open(), Err(JudgmentFailure::Timeout))
        .unwrap();
    assert_eq!(result.status, RecallStatus::Degraded);
    assert_eq!(result.semantic_judgment, SemanticJudgment::Failed);
    assert_eq!(result.degraded_reasons, ["model_timeout"]);
    assert!(result.facts.is_empty() && result.computed.is_none());
    assert!(!result.records.is_empty());
}

#[test]
fn pending_is_merged_without_record_id_bypass_and_unknown_stays_unknown() {
    let f = Fixture::sample("r4_08_");
    let id = *f.ids.values().next().unwrap();
    f.db.open()
        .transaction(|state| {
            state.record_updates.get_mut(&id).unwrap().reasons = vec![
                UpdateReason::UpdateScopeIncomplete,
                UpdateReason::UpdateTargetUnknown,
                UpdateReason::UpdateScopeIncomplete,
            ];
            Ok(())
        })
        .unwrap();
    let sample_id = *f.ids.keys().next().unwrap();
    let mut view = f.fact(sample_id, "unknown");
    view["record_ids"] = json!([]);
    let result = f.finish(output(vec![view.clone()], None));
    assert_eq!(result.status, RecallStatus::Answered);
    assert_eq!(
        result.facts[0].update,
        Some(UpdateState {
            status: UpdateStatus::Pending,
            reasons: vec![
                UpdateReason::UpdateScopeIncomplete,
                UpdateReason::UpdateTargetUnknown
            ]
        })
    );
    assert_eq!(result.completeness.pending_updates, 1);
    f.db.open()
        .transaction(|state| {
            state.record_updates.remove(&id);
            Ok(())
        })
        .unwrap();
    let result = f.finish(output(vec![view], None));
    assert!(result.facts[0].update.is_none());
}

#[test]
fn history_uses_real_end_and_excludes_correction_retraction() {
    let mut input = Fixture::sample("r4_10_").input;
    input["records"][0]["end"] = json!("2026-01-30");
    let f = Fixture::new(input);
    let result = f.finish(output(vec![f.fact(10, "no"), f.fact(11, "yes")], None));
    assert_eq!(result.history.len(), 1);
    assert_eq!(result.history[0].until.as_deref(), Some("2026-01-30"));
    let f = Fixture::sample("r4_12_");
    let id = *f.ids.keys().next().unwrap();
    let result = f.finish(output(vec![f.fact(id, "no")], None));
    assert_eq!(result.history.len(), 1);
    assert!(result.history[0].until.is_none());
    for prefix in ["r4_11_", "r4_20_"] {
        let f = Fixture::sample(prefix);
        let id = *f.ids.keys().next().unwrap();
        let result = f.finish(output(vec![f.fact(id, "no")], None));
        assert!(result.history.is_empty(), "{prefix}");
    }
}

#[test]
fn status_priority_open_conflicts_and_ephemeral_resolution_do_not_write() {
    let f = Fixture::sample("r4_13_");
    let before = serde_json::to_value(f.db.state()).unwrap();
    let result = f.finish(json!({"verdict":"insufficient","facts":[],"compute":null}));
    assert_eq!(result.status, RecallStatus::Conflict);
    assert_eq!(result.conflicts.len(), 1);
    assert_eq!(result.records.len(), 2);
    let mut final_fact = f.fact(17, "yes");
    final_fact["supersedes_record_ids"] = json!([f.ids[&16]]);
    final_fact["evidence"]
        .as_array_mut()
        .unwrap()
        .push(f.fact(16, "no")["evidence"][0].clone());
    let result = f.finish(output(vec![final_fact], None));
    assert_eq!(result.status, RecallStatus::Answered);
    assert!(result.conflicts.is_empty());
    assert_eq!(serde_json::to_value(f.db.state()).unwrap(), before);
    let mut plan = f.plan_json();
    plan["subject"] = json!({"ambiguous":["self","child"]});
    let result = f
        .assembly_with(&f.request(), plan, Budget::default())
        .finish_r4(
            &f.db.open(),
            Ok("{\"verdict\":\"conflict\",\"facts\":[],\"compute\":null}"),
        )
        .unwrap();
    assert_eq!(result.status, RecallStatus::NeedsClarification);
    let clarification = result.clarification.unwrap();
    assert!(clarification.question.contains("本人") && clarification.question.contains("孩子"));
    assert_eq!(clarification.evidence[0].quote.text, f.input["query"]);
}

#[test]
fn r4_conflict_without_link_and_clarification_context_only() {
    let f = Fixture::sample("r4_01_");
    let result =
        f.finish(json!({"verdict":"conflict","facts":[f.fact(1,"unknown")],"compute":null}));
    assert_eq!(result.status, RecallStatus::Conflict);
    assert_eq!(result.facts.len(), 1);
    assert!(result.conflicts.is_empty());
    let mut plan = f.plan_json();
    plan["subject"] = json!({"unresolved":{"text":"我"}});
    let result = f
        .assembly_with(&f.request(), plan, Budget::default())
        .finish_r4(&f.db.open(), Ok(&output(vec![], None).to_string()))
        .unwrap();
    assert_eq!(result.status, RecallStatus::NeedsClarification);
    assert!(result.clarification.unwrap().question.contains("我"));
    let result = f.finish(json!({"verdict":"needs_clarification","facts":[],"compute":null}));
    assert_eq!(result.status, RecallStatus::NeedsClarification);
    assert!(result.clarification.is_some());
    let mut plan = f.plan_json();
    plan["need"] = json!("context_only");
    let result = f
        .assembly_with(&f.request(), plan, Budget::default())
        .finish_r4(&f.db.open(), Ok("invalid JSON"))
        .unwrap();
    assert_eq!(result.status, RecallStatus::ContextOnly);
    assert_eq!(result.semantic_judgment, SemanticJudgment::Skipped);
    assert!(result.facts.is_empty());
}

#[test]
fn count_deduplicates_events_and_unknown_identity_only_produces_lower_bound() {
    let f = Fixture::sample("r4_14_");
    let items = vec![
        f.item(18, Some("march")),
        f.item(19, Some("march")),
        f.item(20, Some("august")),
    ];
    let result = f.finish(output(
        vec![],
        Some(compute("count", items.clone(), vec![], None, "complete")),
    ));
    let c = result.computed.unwrap();
    assert_eq!(c.value, "2");
    assert_eq!(c.bound, ComputeBound::Exact);
    assert_eq!(c.items.len(), 3);
    let mut items = items;
    items[2]["event_group"] = Json::Null;
    let result = f.finish(output(
        vec![],
        Some(compute("count", items.clone(), vec![], None, "complete")),
    ));
    let c = result.computed.unwrap();
    assert_eq!(c.value, "1");
    assert_eq!(c.bound, ComputeBound::AtLeast);
    for item in &mut items {
        item["event_group"] = Json::Null;
    }
    insufficient(f.finish(output(
        vec![],
        Some(compute("count", items, vec![], None, "complete")),
    )));
}

#[test]
fn truncation_and_uncovered_sources_override_model_complete_for_count_and_sum() {
    let f = Fixture::sample("r4_19_");
    let assembly = f.assembly_with(
        &f.request(),
        f.plan_json(),
        Budget {
            records: 1,
            ..Budget::default()
        },
    );
    assert!(assembly.evidence.completeness.retrieval_truncated);
    let id = assembly.evidence.records[0].id();
    let original = *f.ids.iter().find(|(_, v)| **v == id).unwrap().0;
    let result = assembly
        .finish_r4(
            &f.db.open(),
            Ok(&output(
                vec![],
                Some(compute(
                    "count",
                    vec![f.item(original, Some("country"))],
                    vec![],
                    None,
                    "complete",
                )),
            )
            .to_string()),
        )
        .unwrap();
    assert_eq!(result.computed.unwrap().bound, ComputeBound::AtLeast);
    let f = Fixture::sample("r4_15_");
    source(
        &f.db,
        "pending-other",
        "九月有一笔咖啡豆支出，还没算。",
        None,
        "queued",
    );
    let out = sum_output(&f, "complete");
    insufficient(f.finish(out));
}

fn sum_output(f: &Fixture, coverage: &str) -> Json {
    let mut a = f.item(21, Some("first"));
    let mut b = f.item(22, Some("second"));
    a["quantity"] = json!({"amount":"120","unit":"元"});
    b["quantity"] = json!({"amount":"80","unit":"元"});
    output(
        vec![],
        Some(compute("sum", vec![a, b], vec![], Some("元"), coverage)),
    )
}
#[test]
fn sum_reuses_items_and_cannot_treat_partial_positive_sum_as_lower_bound() {
    let f = Fixture::sample("r4_15_");
    let result = f.finish(sum_output(&f, "complete"));
    let c = result.computed.unwrap();
    assert_eq!(c.value, "200");
    assert_eq!(c.bound, ComputeBound::Exact);
    assert!(c.rounding.is_none() && c.request_time.is_none());
    for coverage in ["incomplete", "unknown"] {
        insufficient(f.finish(sum_output(&f, coverage)));
    }
    let mut out = sum_output(&f, "complete");
    out["compute"]["items"][1]["event_group"] = json!("first");
    insufficient(f.finish(out));
}

fn dated_item(f: &Fixture, id: u64, quote: &str, interp: Json) -> Json {
    let mut item = f.item(id, None);
    item["date"] =
        json!({"role":"event","quote":{"text":quote},"interp":interp,"anchor":"said_on"});
    item["date_source"] = item["evidence"][0]["source"].clone();
    item
}
#[test]
fn elapsed_uses_request_at_timezone_and_all_frozen_rounding_rules() {
    let f = Fixture::sample("r4_16_");
    let item = dated_item(
        &f,
        23,
        "2026年7月1日",
        json!({"kind":"date","date":"2026-07-01"}),
    );
    for (unit, value, rounding, approximate) in [
        ("day", "94", ElapsedRounding::CalendarDays, false),
        ("week", "13", ElapsedRounding::NearestWeek, true),
        ("month", "3", ElapsedRounding::CompleteCalendarMonths, false),
        ("year", "0", ElapsedRounding::CompleteCalendarYears, false),
    ] {
        let mut req = f.request();
        req.said_on = "2020-01-01".into();
        // request.at 对应 10-03；不能被 current said_on 或运行机器时钟替换。
        let result = f
            .assembly_with(&req, f.plan_json(), Budget::default())
            .finish_r4(
                &f.db.open(),
                Ok(&output(
                    vec![],
                    Some(compute(
                        "elapsed",
                        vec![item.clone()],
                        vec!["@request.at", "23"],
                        Some(unit),
                        "complete",
                    )),
                )
                .to_string()),
            )
            .unwrap();
        let c = result.computed.unwrap();
        assert_eq!(c.value, value);
        assert_eq!(c.rounding, Some(rounding));
        assert_eq!(c.exact_days, Some(94));
        assert_eq!(c.approximate, Some(approximate));
        let clock = c.request_time.unwrap();
        assert_eq!(clock.at, req.binding.at);
        assert_eq!(clock.local_date, "2026-10-03");
        assert_eq!(clock.tz_offset_seconds, 28800);
    }
    let mut req = f.request();
    req.binding.at = at("2026-06-30");
    let result = f
        .assembly_with(&req, f.plan_json(), Budget::default())
        .finish_r4(
            &f.db.open(),
            Ok(&output(
                vec![],
                Some(compute(
                    "elapsed",
                    vec![item],
                    vec!["@request.at", "23"],
                    Some("day"),
                    "complete",
                )),
            )
            .to_string()),
        )
        .unwrap();
    insufficient(result);
}

#[test]
fn elapsed_month_precision_is_insufficient_and_false_precision_is_invalid() {
    let f = Fixture::sample("r4_17_");
    let item = dated_item(
        &f,
        24,
        "七月",
        json!({"kind":"month","year_offset":0,"month":7}),
    );
    insufficient(f.finish(output(
        vec![],
        Some(compute(
            "elapsed",
            vec![item.clone()],
            vec!["@request.at", "24"],
            Some("day"),
            "complete",
        )),
    )));
    let mut bad = item;
    bad["date"]["interp"] = json!({"kind":"date","date":"2026-07-01"});
    invalid(
        &f,
        output(
            vec![],
            Some(compute(
                "elapsed",
                vec![bad],
                vec!["@request.at", "24"],
                Some("day"),
                "complete",
            )),
        ),
    );
}

#[test]
fn whole_sentence_month_precision_is_checked_in_facts_and_compute_items() {
    let f = Fixture::sample("r4_17_");
    let quote = f.input["records"][0]["sentence"].as_str().unwrap();
    let mut item = dated_item(
        &f,
        24,
        quote,
        json!({"kind":"month","year_offset":0,"month":7}),
    );
    insufficient(f.finish(output(
        vec![],
        Some(compute(
            "elapsed",
            vec![item.clone()],
            vec!["@request.at", "24"],
            Some("day"),
            "complete",
        )),
    )));
    let mut view = f.fact(24, "yes");
    view["times"] = json!([item["date"].clone()]);
    assert_eq!(
        f.finish(output(vec![view], None)).status,
        RecallStatus::Answered
    );
    item["date"]["interp"] = json!({"kind":"date","date":"2026-07-12"});
    invalid(
        &f,
        output(
            vec![],
            Some(compute(
                "elapsed",
                vec![item.clone()],
                vec!["@request.at", "24"],
                Some("day"),
                "complete",
            )),
        ),
    );
    let mut view = f.fact(24, "yes");
    view["times"] = json!([item["date"]]);
    invalid(&f, output(vec![view], None));
}

#[test]
fn x3_compare_disjoint_date_intervals_and_overlapping_precision() {
    let f = Fixture::sample("r4_18_");
    let a = dated_item(
        &f,
        25,
        "2024年3月",
        json!({"kind":"month","year_offset":0,"month":3}),
    );
    let b = dated_item(
        &f,
        26,
        "2025年",
        json!({"kind":"relative","unit":"year","offset":0}),
    );
    for (operands, value) in [(vec!["25", "26"], "less"), (vec!["26", "25"], "greater")] {
        let c = f
            .finish(output(
                vec![],
                Some(compute(
                    "compare",
                    vec![a.clone(), b.clone()],
                    operands,
                    None,
                    "complete",
                )),
            ))
            .computed
            .unwrap();
        assert_eq!(c.value, value);
        assert!(c.request_time.is_none());
    }
    let mut input = f.input.clone();
    input["records"][1]["sentence"] = json!("我2024年开始学韩语。");
    input["records"][1]["said_on"] = json!("2024-02-01");
    let f = Fixture::new(input);
    let a = dated_item(
        &f,
        25,
        "2024年3月",
        json!({"kind":"month","year_offset":0,"month":3}),
    );
    let b = dated_item(
        &f,
        26,
        "2024年",
        json!({"kind":"relative","unit":"year","offset":0}),
    );
    insufficient(f.finish(output(
        vec![],
        Some(compute(
            "compare",
            vec![a, b],
            vec!["25", "26"],
            None,
            "complete",
        )),
    )));
}

#[test]
fn compute_item_ids_operands_op_quantity_and_date_source_are_checked() {
    let f = Fixture::sample("r4_15_");
    let good = sum_output(&f, "complete");
    for kind in [
        "id_duplicate",
        "id_reserved",
        "operand_extra",
        "op",
        "quantity",
        "quantity_missing_basis",
        "unit",
        "date_source",
        "request",
        "extra_value",
    ] {
        let mut bad = good.clone();
        match kind {
            "id_duplicate" => bad["compute"]["items"][1]["id"] = json!("21"),
            "id_reserved" => bad["compute"]["items"][0]["id"] = json!("@fake"),
            "operand_extra" => bad["compute"]["operands"] = json!(["21"]),
            "op" => bad["compute"]["op"] = json!("count"),
            "quantity" => bad["compute"]["items"][0]["quantity"]["amount"] = json!("NaN"),
            "quantity_missing_basis" => {
                bad["compute"]["items"][0]["quantity"]["amount"] = json!("121")
            }
            "unit" => bad["compute"]["items"][0]["quantity"]["unit"] = json!("kg"),
            "date_source" => {
                bad["compute"]["items"][0]["date_source"] =
                    json!({"kind":"stored","source_id":"s-21"})
            }
            "request" => {
                bad["compute"]["items"][0]["evidence"] =
                    json!([{"source":{"kind":"current"},"quote":{"text":f.input["query"]}}])
            }
            "extra_value" => bad["compute"]["value"] = json!("200"),
            _ => unreachable!(),
        }
        invalid(&f, bad);
    }
    let f = Fixture::sample("r4_16_");
    let item = dated_item(
        &f,
        23,
        "2026年7月1日",
        json!({"kind":"date","date":"2026-07-01"}),
    );
    for operands in [
        vec!["unknown", "23"],
        vec!["23", "@request.at"],
        vec!["23"],
        vec!["@request.at", "@request.at"],
    ] {
        invalid(
            &f,
            output(
                vec![],
                Some(compute(
                    "elapsed",
                    vec![item.clone()],
                    operands,
                    Some("day"),
                    "complete",
                )),
            ),
        );
    }
    for date in ["2026-07-02", "2026-02-30"] {
        let mut bad = item.clone();
        bad["date"]["interp"]["date"] = json!(date);
        invalid(
            &f,
            output(
                vec![],
                Some(compute(
                    "elapsed",
                    vec![bad],
                    vec!["@request.at", "23"],
                    Some("day"),
                    "complete",
                )),
            ),
        );
    }
    let mut bad = item;
    bad["date_source"] = json!({"kind":"stored","source_id":"s-15"});
    invalid(
        &f,
        output(
            vec![],
            Some(compute(
                "elapsed",
                vec![bad],
                vec!["@request.at", "23"],
                Some("day"),
                "complete",
            )),
        ),
    );
}

#[test]
fn snapshot_change_after_model_response_rejects_old_content() {
    let f = Fixture::sample("r4_01_");
    let assembly = f.assembly();
    let out = output(vec![f.fact(1, "yes")], None).to_string();
    f.db.open()
        .transaction(|state| {
            state.sources.get_mut("s-1").unwrap().enrichment = "deleted".into();
            Ok(())
        })
        .unwrap();
    let error = assembly
        .finish_r4(&f.db.open(), Ok(&out))
        .unwrap_err()
        .to_string();
    assert_eq!(error, "recall_snapshot_changed");
}

fn numeric_fixture(op: &str, text_a: &str, text_b: Option<&str>) -> Fixture {
    let mut records = vec![
        json!({"id":1,"sentence":text_a,"said_on":"2026-07-20","source":"a","status":"active","ended":"not_ended","termination":null,"update":"judged"}),
    ];
    if let Some(text) = text_b {
        records.push(json!({"id":2,"sentence":text,"said_on":"2026-07-20","source":"b","status":"active","ended":"not_ended","termination":null,"update":"judged"}));
    }
    Fixture::new(
        json!({"query":"统计这些记录","at":"2026-10-03","current_evidence":"query_only",
        "plan":{"type":"aggregate","op":op,"relation":"fact"},"records":records,"raw":[],"relations":[]}),
    )
}
fn quantity_item(f: &Fixture, id: u64, amount: &str, unit: Option<&str>, group: &str) -> Json {
    let mut item = f.item(id, Some(group));
    item["quantity"] = json!({"amount":amount,"unit":unit});
    item
}
#[test]
fn signed_decimal_sum_normalizes_units_deduplicates_and_checks_overflow() {
    let f = numeric_fixture("sum", "重量增加1.20kg", Some("重量变化-50g"));
    let items = vec![
        quantity_item(&f, 1, "1.20", Some("kg"), "a"),
        quantity_item(&f, 2, "-50", Some("g"), "b"),
    ];
    let result = f.finish(output(
        vec![],
        Some(compute("sum", items, vec![], Some("kg"), "complete")),
    ));
    assert_eq!(result.computed.unwrap().value, "1.15");
    let f = numeric_fixture("sum", "重量1kg", Some("重量1000g"));
    let items = vec![
        quantity_item(&f, 1, "1", Some("kg"), "same"),
        quantity_item(&f, 2, "1000", Some("g"), "same"),
    ];
    let result = f.finish(output(
        vec![],
        Some(compute(
            "sum",
            items.clone(),
            vec![],
            Some("kg"),
            "complete",
        )),
    ));
    assert_eq!(result.computed.unwrap().value, "1");
    let mut unknown = items.clone();
    unknown[0]["event_group"] = Json::Null;
    insufficient(f.finish(output(
        vec![],
        Some(compute("sum", unknown, vec![], Some("kg"), "complete")),
    )));
    let mut missing = items;
    missing[0]["quantity"] = Json::Null;
    insufficient(f.finish(output(
        vec![],
        Some(compute("sum", missing, vec![], Some("kg"), "complete")),
    )));
    let f = numeric_fixture(
        "sum",
        "金额170141183460469231731687303715884105727元",
        Some("金额1元"),
    );
    let items = vec![
        quantity_item(
            &f,
            1,
            "170141183460469231731687303715884105727",
            Some("元"),
            "a",
        ),
        quantity_item(&f, 2, "1", Some("元"), "b"),
    ];
    insufficient(f.finish(output(
        vec![],
        Some(compute("sum", items, vec![], Some("元"), "complete")),
    )));
}

#[test]
fn numeric_compare_units_incompatible_dimensions_and_unit_substrings() {
    let f = numeric_fixture("compare", "长度2米", Some("长度150厘米"));
    let items = vec![
        quantity_item(&f, 1, "2", Some("m"), "a"),
        quantity_item(&f, 2, "150", Some("cm"), "b"),
    ];
    let c = f
        .finish(output(
            vec![],
            Some(compute(
                "compare",
                items.clone(),
                vec!["1", "2"],
                Some("m"),
                "complete",
            )),
        ))
        .computed
        .unwrap();
    assert_eq!(c.value, "greater");
    let mut false_unit = items;
    false_unit[1]["quantity"]["unit"] = json!("m");
    invalid(
        &f,
        output(
            vec![],
            Some(compute(
                "compare",
                false_unit,
                vec!["1", "2"],
                Some("m"),
                "complete",
            )),
        ),
    );
    let f = numeric_fixture("compare", "重量2kg", Some("价格2元"));
    let items = vec![
        quantity_item(&f, 1, "2", Some("kg"), "a"),
        quantity_item(&f, 2, "2", Some("元"), "b"),
    ];
    insufficient(f.finish(output(
        vec![],
        Some(compute("compare", items, vec!["1", "2"], None, "complete")),
    )));
    let f = numeric_fixture("compare", "重量1kg", Some("重量1000g"));
    let items = vec![
        quantity_item(&f, 1, "1", Some("kg"), "a"),
        quantity_item(&f, 2, "1000", Some("g"), "b"),
    ];
    assert_eq!(
        f.finish(output(
            vec![],
            Some(compute("compare", items, vec!["1", "2"], None, "complete"))
        ))
        .computed
        .unwrap()
        .value,
        "equal"
    );
}

#[test]
fn one_message_can_supply_multiple_items_without_source_id_deduplication() {
    let f = numeric_fixture("sum", "买纸花了1.05元，买笔花了2.10元", None);
    let mut a = quantity_item(&f, 1, "1.05", Some("元"), "paper");
    a["evidence"] = json!([stored("a", "买纸花了1.05元")]);
    let mut b = quantity_item(&f, 1, "2.10", Some("元"), "pen");
    b["id"] = json!("second");
    b["evidence"] = json!([stored("a", "买笔花了2.10元")]);
    let c = f
        .finish(output(
            vec![],
            Some(compute(
                "sum",
                vec![a.clone(), b.clone()],
                vec![],
                Some("元"),
                "complete",
            )),
        ))
        .computed
        .unwrap();
    assert_eq!(c.value, "3.15");
    assert_eq!(c.items.len(), 2);
    let mut plan = f.plan_json();
    plan["plan"]["op"] = json!("count");
    let result = f
        .assembly_with(&f.request(), plan, Budget::default())
        .finish_r4(
            &f.db.open(),
            Ok(&output(
                vec![],
                Some(compute("count", vec![a, b], vec![], None, "complete")),
            )
            .to_string()),
        )
        .unwrap();
    assert_eq!(result.computed.unwrap().value, "2");
}

#[test]
fn source_clock_anchored_dates_unknown_dates_and_range_precision() {
    let f = numeric_fixture("elapsed", "我前天开始练琴。", None);
    let item = dated_item(
        &f,
        1,
        "前天",
        json!({"kind":"relative","unit":"day","offset":-2}),
    );
    let c = f
        .finish(output(
            vec![],
            Some(compute(
                "elapsed",
                vec![item],
                vec!["@request.at", "1"],
                Some("day"),
                "complete",
            )),
        ))
        .computed
        .unwrap();
    assert_eq!(c.value, "77");
    let f = numeric_fixture("elapsed", "我2026年7月1日开始跑步，三天后开始练琴。", None);
    let item = dated_item(
        &f,
        1,
        "三天后",
        json!({"kind":"anchored","anchor_quote":{"text":"2026年7月1日"},"unit":"day","offset":3}),
    );
    insufficient(f.finish(output(
        vec![],
        Some(compute(
            "elapsed",
            vec![item.clone()],
            vec!["@request.at", "1"],
            Some("day"),
            "complete",
        )),
    )));
    let mut bad = item;
    bad["date"]["interp"]["anchor_quote"]["text"] = json!("2026年1月1日");
    invalid(
        &f,
        output(
            vec![],
            Some(compute(
                "elapsed",
                vec![bad],
                vec!["@request.at", "1"],
                Some("day"),
                "complete",
            )),
        ),
    );
    let f = numeric_fixture("elapsed", "2026年7月1日至7月3日我搬家。", None);
    let item = dated_item(
        &f,
        1,
        "2026年7月1日至7月3日",
        json!({"kind":"range","start":"2026-07-01","end_inclusive":"2026-07-03"}),
    );
    insufficient(f.finish(output(
        vec![],
        Some(compute(
            "elapsed",
            vec![item],
            vec!["@request.at", "1"],
            Some("day"),
            "complete",
        )),
    )));
    let f = numeric_fixture(
        "compare",
        "某天我开始练琴。",
        Some("我2026年7月1日开始跑步。"),
    );
    let a = dated_item(&f, 1, "某天", json!({"kind":"unknown"}));
    let b = dated_item(
        &f,
        2,
        "2026年7月1日",
        json!({"kind":"date","date":"2026-07-01"}),
    );
    insufficient(f.finish(output(
        vec![],
        Some(compute(
            "compare",
            vec![a, b],
            vec!["1", "2"],
            None,
            "complete",
        )),
    )));
}

#[test]
fn declared_r1_unit_is_enforced_and_pending_or_gap_prevents_exact_count() {
    let f = Fixture::sample("r4_14_");
    let items = vec![f.item(18, Some("march"))];
    f.db.open()
        .transaction(|state| {
            state.record_updates.insert(
                f.ids[&20],
                UpdateState {
                    status: UpdateStatus::Pending,
                    reasons: vec![UpdateReason::UpdateScopeIncomplete],
                },
            );
            Ok(())
        })
        .unwrap();
    let result = f.finish(output(
        vec![],
        Some(compute("count", items.clone(), vec![], None, "complete")),
    ));
    assert_eq!(result.computed.unwrap().bound, ComputeBound::AtLeast);
    f.db.open()
        .transaction(|state| {
            state.record_updates.remove(&f.ids[&20]);
            Ok(())
        })
        .unwrap();
    let result = f.finish(output(
        vec![],
        Some(compute("count", items, vec![], None, "complete")),
    ));
    assert_eq!(result.computed.unwrap().bound, ComputeBound::AtLeast);
    let f = Fixture::sample("r4_16_");
    let mut plan = f.plan_json();
    plan["plan"]["unit"] = json!("week");
    let item = dated_item(
        &f,
        23,
        "2026年7月1日",
        json!({"kind":"date","date":"2026-07-01"}),
    );
    let result = f
        .assembly_with(&f.request(), plan, Budget::default())
        .finish_r4(
            &f.db.open(),
            Ok(&output(
                vec![],
                Some(compute(
                    "elapsed",
                    vec![item],
                    vec!["@request.at", "23"],
                    Some("day"),
                    "complete",
                )),
            )
            .to_string()),
        )
        .unwrap();
    assert_eq!(result.status, RecallStatus::Degraded);
}

#[test]
fn amounts_require_their_own_unit_not_another_number_or_source() {
    let f = numeric_fixture("sum", "7月1日买花花了2元。", None);
    invalid(
        &f,
        output(
            vec![],
            Some(compute(
                "sum",
                vec![quantity_item(&f, 1, "1", Some("元"), "a")],
                vec![],
                Some("元"),
                "complete",
            )),
        ),
    );
    let f = numeric_fixture("sum", "花了.5元。", Some("花了+1.00元。"));
    let c = f
        .finish(output(
            vec![],
            Some(compute(
                "sum",
                vec![
                    quantity_item(&f, 1, "0.50", Some("元"), "a"),
                    quantity_item(&f, 2, "+1.0", Some("元"), "b"),
                ],
                vec![],
                Some("元"),
                "complete",
            )),
        ))
        .computed
        .unwrap();
    assert_eq!(c.value, "1.5");
}

#[test]
fn legacy_linked_source_is_judged_from_original_with_unknown_update_and_raw_grade() {
    let f = Fixture::sample("r4_01_");
    f.db.open()
        .transaction(|state| {
            state.record_updates.remove(&f.ids[&1]);
            Ok(())
        })
        .unwrap();
    let assembly = f.assembly();
    assert_eq!(assembly.evidence.raw.len(), 1);
    let result = assembly
        .finish_r4(
            &f.db.open(),
            Ok(&output(vec![f.fact(1, "unknown")], None).to_string()),
        )
        .unwrap();
    assert_eq!(result.status, RecallStatus::Answered);
    assert_eq!(result.facts[0].grade, RecallGrade::Uncommitted);
    assert!(result.facts[0].update.is_none());
}

#[test]
fn qualifier_source_and_merged_pending_reasons_are_not_inferred_from_ids() {
    let f = numeric_fixture(
        "lookup",
        "我可能不太喜欢软铅笔，但只在画素描时，明天再试。",
        Some("我可能会继续用钢笔。"),
    );
    f.db.open()
        .transaction(|state| {
            state.record_updates.insert(
                f.ids[&1],
                UpdateState {
                    status: UpdateStatus::Pending,
                    reasons: vec![UpdateReason::UpdateScopeIncomplete],
                },
            );
            state.record_updates.insert(
                f.ids[&2],
                UpdateState {
                    status: UpdateStatus::Pending,
                    reasons: vec![
                        UpdateReason::UpdateScopeIncomplete,
                        UpdateReason::UpdateTargetUnknown,
                    ],
                },
            );
            Ok(())
        })
        .unwrap();
    let mut view = f.fact(1, "unknown");
    view["evidence"]
        .as_array_mut()
        .unwrap()
        .push(f.fact(2, "unknown")["evidence"][0].clone());
    view["record_ids"] = json!([f.ids[&1], f.ids[&2]]);
    view["polarity"] = json!("negate");
    view["strength"] = json!("weak");
    view["uncertainty"] = json!({"quote":{"text":"可能"},"resolved":false});
    view["conditions"] = json!([{"quote":{"text":"只在画素描时"},"resolved":true}]);
    view["times"] = json!([{"role":"event","quote":{"text":"明天"},"interp":{"kind":"relative","unit":"day","offset":1},"anchor":"said_on"}]);
    let result = f.finish(output(vec![view.clone()], None));
    assert_eq!(result.status, RecallStatus::Answered);
    assert_eq!(
        result.facts[0].polarity,
        hippocampus::contract::Polarity::Negate
    );
    assert_eq!(
        result.facts[0].strength,
        Some(hippocampus::contract::Strength::Weak)
    );
    assert_eq!(
        serde_json::to_value(&result.facts[0]).unwrap()["times"],
        view["times"]
    );
    let update = result.facts[0].update.as_ref().unwrap();
    assert_eq!(update.status, UpdateStatus::Pending);
    assert_eq!(update.reasons.len(), 2);
    assert!(update.reasons.contains(&UpdateReason::UpdateTargetUnknown));
    let mut foreign = f.fact(2, "unknown");
    foreign["conditions"] = view["conditions"].clone();
    invalid(&f, output(vec![foreign], None));
}

#[test]
fn r1_unresolved_history_does_not_forge_current_quote_and_fallback_cannot_authorize_compute() {
    let f = Fixture::sample("r4_14_");
    let mut req = f.request();
    req.binding.context = vec!["那位同事说过。".into()];
    let mut understood = f.plan_json();
    understood["subject"] = json!({"unresolved":{"text":"那位同事"}});
    let result = f
        .assembly_with(&req, understood, Budget::default())
        .finish_r4(&f.db.open(), Ok(&output(vec![], None).to_string()))
        .unwrap();
    let clarification = result.clarification.unwrap();
    assert!(clarification.question.contains("那位同事"));
    assert!(clarification.evidence.is_empty());
    let mut assembly = f.assembly();
    assembly.plan.output = None;
    let result = assembly
        .finish_r4(
            &f.db.open(),
            Ok(&output(
                vec![],
                Some(compute(
                    "count",
                    vec![f.item(18, Some("march"))],
                    vec![],
                    None,
                    "complete",
                )),
            )
            .to_string()),
        )
        .unwrap();
    assert_eq!(result.status, RecallStatus::Degraded);
    assert!(
        result
            .degraded_reasons
            .iter()
            .any(|r| r == "r1_not_understood")
    );
}

#[test]
fn changed_source_clock_advances_source_revision_and_rejects_package() {
    let f = Fixture::sample("r4_16_");
    let assembly = f.assembly();
    let before = f.db.state();
    f.db.open()
        .transaction(|state| {
            state.sources.get_mut("s-23").unwrap().observed_at += 86400;
            Ok(())
        })
        .unwrap();
    let after = f.db.state();
    assert_eq!(before.revision, after.revision);
    assert_ne!(before.source_revision, after.source_revision);
    let error = assembly
        .finish_r4(&f.db.open(), Ok(&output(vec![], None).to_string()))
        .unwrap_err()
        .to_string();
    assert_eq!(error, "recall_snapshot_changed");
}

#[test]
fn competing_current_fact_prevents_ephemeral_conflict_resolution() {
    let f = Fixture::sample("r4_13_");
    let mut final_fact = f.fact(17, "yes");
    final_fact["supersedes_record_ids"] = json!([f.ids[&16]]);
    final_fact["evidence"]
        .as_array_mut()
        .unwrap()
        .push(f.fact(16, "no")["evidence"][0].clone());
    let result = f.finish(output(vec![final_fact, f.fact(16, "yes")], None));
    assert_eq!(result.status, RecallStatus::Conflict);
    assert_eq!(result.conflicts.len(), 1);
}

#[test]
fn same_precise_day_can_be_equal_and_two_elapsed_items_have_no_request_clock() {
    let f = numeric_fixture(
        "compare",
        "我2026年7月1日开始练琴。",
        Some("我2026年7月1日开始跑步。"),
    );
    let a = dated_item(
        &f,
        1,
        "2026年7月1日",
        json!({"kind":"date","date":"2026-07-01"}),
    );
    let b = dated_item(
        &f,
        2,
        "2026年7月1日",
        json!({"kind":"date","date":"2026-07-01"}),
    );
    let c = f
        .finish(output(
            vec![],
            Some(compute(
                "compare",
                vec![a.clone(), b.clone()],
                vec!["1", "2"],
                None,
                "complete",
            )),
        ))
        .computed
        .unwrap();
    assert_eq!(c.value, "equal");
    let mut plan = f.plan_json();
    plan["plan"]["op"] = json!("elapsed");
    let c = f
        .assembly_with(&f.request(), plan, Budget::default())
        .finish_r4(
            &f.db.open(),
            Ok(&output(
                vec![],
                Some(compute(
                    "elapsed",
                    vec![a, b],
                    vec!["1", "2"],
                    Some("day"),
                    "complete",
                )),
            )
            .to_string()),
        )
        .unwrap()
        .computed
        .unwrap();
    assert_eq!(c.value, "0");
    assert!(c.request_time.is_none());
    assert_eq!(c.exact_days, Some(0));
}

#[test]
fn history_never_uses_old_received_boundary_but_keeps_explicit_legacy_end() {
    let mut input = Fixture::sample("r4_10_").input;
    input["records"][0]["end"] = json!("2026-01-30");
    let f = Fixture::new(input);
    let id = f.ids[&10];
    f.db.open()
        .transaction(|state| {
            let mut m = state.repository.get(id)?.unwrap();
            m.temporal.valid_to.as_mut().unwrap().source = TemporalSource::Supersession;
            state.repository.update(m)?;
            Ok(())
        })
        .unwrap();
    let result = f.finish(output(vec![f.fact(10, "no")], None));
    assert!(result.history[0].until.is_none());
    f.db.open()
        .transaction(|state| {
            let mut m = state.repository.get(id)?.unwrap();
            m.temporal.valid_to.as_mut().unwrap().source = TemporalSource::Explicit;
            state.repository.update(m)?;
            state.record_updates.remove(&id);
            Ok(())
        })
        .unwrap();
    let result = f.finish(output(vec![f.fact(10, "no")], None));
    assert_eq!(result.history[0].until.as_deref(), Some("2026-01-30"));
    assert!(matches!(
        result.records.iter().find(|r| r.id() == id).unwrap(),
        RecordView::Legacy(_)
    ));
}

#[test]
fn r4_04_lookup_retrieves_lexically_unmatched_update_and_answers_from_new_raw() {
    let f = Fixture::sample("r4_04_");
    let understood = f.plan_json();
    assert_eq!(understood["type"], "single");
    assert_eq!(understood["plan"]["op"], "lookup");
    let before = serde_json::to_value(f.db.state()).unwrap();
    let assembly = f.assembly();
    assert_eq!(assembly.evidence.raw.len(), 1);
    assert_eq!(assembly.evidence.raw[0].source_id, "s-40");
    assert_eq!(
        assembly.evidence.raw[0].quote.text,
        "我上周已经搬到南京了。"
    );
    assert_eq!(assembly.evidence.completeness.pending_related_sources, 1);
    let new = fact(
        "现在住南京",
        vec![stored("s-40", "我上周已经搬到南京了。")],
        vec![],
        "yes",
    );
    let result = assembly
        .finish_r4(
            &f.db.open(),
            Ok(&output(vec![new, f.fact(4, "no")], None).to_string()),
        )
        .unwrap();
    assert_eq!(result.status, RecallStatus::Answered);
    assert_eq!(result.facts[0].current, Qualification::Yes);
    assert_eq!(result.facts[0].grade, RecallGrade::Uncommitted);
    assert!(result.facts[0].update.is_none());
    assert_eq!(result.facts[1].current, Qualification::No);
    assert!(result.conflicts.is_empty());
    assert_eq!(serde_json::to_value(f.db.state()).unwrap(), before);
}

fn visibility_fixture() -> Fixture {
    Fixture::new(json!({
        "query":"我目前住在哪里？","at":SAID_ON,"current_evidence":"query_only",
        "plan":{"type":"single","relation":"居住地","op":"lookup"},
        "records":[{"id":1,"source":"home","sentence":"我住上海。",
            "said_on":SAID_ON,"status":"active","update":"judged"}],
        "raw":[],"relations":[]
    }))
}

#[test]
fn visibility_gate_withholds_old_current_when_new_raw_is_count_omitted() {
    let f = visibility_fixture();
    source(&f.db, "move", "我已搬到南京。", None, "queued");
    let before = serde_json::to_value(f.db.state()).unwrap();
    let assembly = f.assembly_with(
        &f.request(),
        f.plan_json(),
        Budget {
            raw: 0,
            ..Budget::default()
        },
    );
    assert!(assembly.evidence.raw.is_empty());
    let out = assembly
        .finish_r4(
            &f.db.open(),
            Ok(&output(vec![f.fact(1, "yes")], None).to_string()),
        )
        .unwrap();
    assert_eq!(out.facts[0].current, Qualification::Unknown);
    assert_eq!(out.status, RecallStatus::Insufficient);
    assert!(out.completeness.retrieval_truncated);
    assert!(!out.completeness.plan_complete);
    assert_eq!(out.completeness.pending_related_sources, 1);
    assert!(out.degraded_reasons.is_empty());
    assert_eq!(out.semantic_judgment, SemanticJudgment::Model);
    assert_eq!(serde_json::to_value(f.db.state()).unwrap(), before);
}

#[test]
fn visibility_gate_detects_partial_raw_even_when_pending_count_equals_raw_count() {
    let f = visibility_fixture();
    source(&f.db, "move", "我已搬到南京。", None, "failed");
    let full = f.assembly();
    let records_bytes: usize = full
        .evidence
        .records
        .iter()
        .map(|r| serde_json::to_vec(r).unwrap().len())
        .sum();
    let mut raw = full.evidence.raw[0].clone();
    raw.quote.text = "我已".into();
    let budget = Budget {
        stored_bytes: records_bytes + serde_json::to_vec(&raw).unwrap().len(),
        ..Budget::default()
    };
    let assembly = f.assembly_with(&f.request(), f.plan_json(), budget);
    assert_eq!(assembly.evidence.raw[0].quote.text, "我已");
    assert_eq!(
        assembly.evidence.raw.len(),
        assembly.evidence.completeness.pending_related_sources
    );
    let out = assembly
        .finish_r4(
            &f.db.open(),
            Ok(&output(vec![f.fact(1, "yes")], None).to_string()),
        )
        .unwrap();
    assert_eq!(out.facts[0].current, Qualification::Unknown);
    assert_eq!(out.status, RecallStatus::Insufficient);
    assert!(out.completeness.retrieval_truncated);
    assert!(!out.completeness.plan_complete);
    assert!(out.degraded_reasons.is_empty());
}

#[test]
fn visibility_gate_allows_complete_new_raw_and_keeps_no_current_history() {
    let f = visibility_fixture();
    source(&f.db, "move", "我已搬到南京。", None, "failed");
    let out = f.finish(output(
        vec![
            fact(
                "我现在住南京。",
                vec![stored("move", "我已搬到南京。")],
                vec![],
                "yes",
            ),
            f.fact(1, "no"),
        ],
        None,
    ));
    assert_eq!(out.status, RecallStatus::Answered);
    assert_eq!(out.facts[0].current, Qualification::Yes);
    assert_eq!(out.facts[0].grade, RecallGrade::Uncommitted);
    assert_eq!(out.facts[1].current, Qualification::No);
    assert!(!out.completeness.retrieval_truncated);
    assert!(out.degraded_reasons.is_empty());
}

#[test]
fn visibility_gate_excludes_whole_source_with_known_other_subject() {
    let f = visibility_fixture();
    seed_on(
        &f.db,
        "child",
        "孩子喜欢绘画。",
        "child",
        None,
        true,
        SAID_ON,
    );
    f.db.open()
        .transaction(|s| {
            s.sources.get_mut("child").unwrap().enrichment = "needs_rejudge".into();
            Ok(())
        })
        .unwrap();
    let assembly = f.assembly_with(
        &f.request(),
        f.plan_json(),
        Budget {
            raw: 0,
            ..Budget::default()
        },
    );
    assert_eq!(assembly.evidence.completeness.pending_related_sources, 1);
    assert!(
        assembly
            .evidence
            .records
            .iter()
            .all(|r| r.id() == f.ids[&1])
    );
    let out = assembly
        .finish_r4(
            &f.db.open(),
            Ok(&output(vec![f.fact(1, "yes")], None).to_string()),
        )
        .unwrap();
    assert!(out.completeness.retrieval_truncated);
    assert_eq!(out.status, RecallStatus::Answered);
    assert_eq!(out.facts[0].current, Qualification::Yes);
    assert!(out.degraded_reasons.is_empty());
}

#[test]
fn visibility_gate_does_not_use_partial_other_subject_link_to_exclude_source() {
    let f = visibility_fixture();
    seed_on(
        &f.db,
        "mixed",
        "孩子喜欢绘画。",
        "child",
        None,
        true,
        SAID_ON,
    );
    f.db.open()
        .transaction(|s| {
            let source = s.sources.get_mut("mixed").unwrap();
            source.enrichment = "needs_rejudge".into();
            source.request.as_mut().unwrap()["text"] = json!("孩子喜欢绘画。我已搬到南京。");
            Ok(())
        })
        .unwrap();
    let assembly = f.assembly_with(
        &f.request(),
        f.plan_json(),
        Budget {
            raw: 0,
            ..Budget::default()
        },
    );
    let out = assembly
        .finish_r4(
            &f.db.open(),
            Ok(&output(vec![f.fact(1, "yes")], None).to_string()),
        )
        .unwrap();
    assert_eq!(out.facts[0].current, Qualification::Unknown);
    assert_eq!(out.status, RecallStatus::Insufficient);
}

#[test]
fn visibility_gate_accepts_omitted_raw_already_fully_visible_in_record_evidence() {
    let f = visibility_fixture();
    f.db.open()
        .transaction(|s| {
            s.sources.get_mut("home").unwrap().enrichment = "queued".into();
            Ok(())
        })
        .unwrap();
    let assembly = f.assembly_with(
        &f.request(),
        f.plan_json(),
        Budget {
            raw: 0,
            ..Budget::default()
        },
    );
    let out = assembly
        .finish_r4(
            &f.db.open(),
            Ok(&output(vec![f.fact(1, "yes")], None).to_string()),
        )
        .unwrap();
    assert!(out.completeness.retrieval_truncated);
    assert_eq!(out.completeness.pending_related_sources, 1);
    assert_eq!(out.facts[0].current, Qualification::Yes);
    assert_eq!(out.status, RecallStatus::Answered);
}

#[test]
fn visibility_gate_does_not_downgrade_for_record_truncation_alone() {
    let f = Fixture::new(json!({
        "query":"我住上海，核对住所。","at":SAID_ON,"current_evidence":"query_only",
        "plan":{"type":"single","relation":"居住地","op":"lookup"},
        "records":[
            {"id":2,"source":"hobby","sentence":"我喜欢红色。","said_on":SAID_ON,"status":"active","update":"judged"},
            {"id":1,"source":"home","sentence":"我住上海。","said_on":SAID_ON,"status":"active","update":"judged"}
        ],"raw":[],"relations":[]
    }));
    let assembly = f.assembly_with(
        &f.request(),
        f.plan_json(),
        Budget {
            records: 1,
            ..Budget::default()
        },
    );
    assert_eq!(assembly.evidence.records[0].id(), f.ids[&1]);
    let out = assembly
        .finish_r4(
            &f.db.open(),
            Ok(&output(vec![f.fact(1, "yes")], None).to_string()),
        )
        .unwrap();
    assert!(out.completeness.retrieval_truncated);
    assert_eq!(out.completeness.pending_related_sources, 0);
    assert_eq!(out.facts[0].current, Qualification::Yes);
    assert_eq!(out.status, RecallStatus::Answered);
}
