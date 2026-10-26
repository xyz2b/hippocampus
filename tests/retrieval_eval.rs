//! AF173：固定 R1、真实 SQLite 的 R2/R3 基线，不构造任何模型提供者。
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use hippocampus::contract::*;
use hippocampus::recall::{self, Budget, Request, RetrievalDiagnostic};
use hippocampus::*;
use serde::Deserialize;
use serde_json::{Value as Json, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Corpus {
    schema: String,
    revision: String,
    said_on: String,
    at: i64,
    utc_offset_seconds: i32,
    entities: Vec<Entity>,
    sources: Vec<OwnedSource>,
    records: Vec<FrozenRecord>,
    relations: Vec<StoredRelation>,
    delete_sources: Vec<String>,
    fill_rule: Json,
    insertion_orders: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnedSource {
    owner: String,
    record: SourceRecord,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FrozenRecord {
    label: String,
    memory: Memory,
    contract: ContractCandidate,
    receipt: W5Receipt,
    record_update: UpdateState,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Queries {
    schema: String,
    revision: String,
    r1_contract: String,
    budgets: Vec<usize>,
    queries: Vec<Query>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Query {
    id: String,
    query: String,
    category: String,
    control: bool,
    owner: String,
    subject_ids: Option<Vec<EntityId>>,
    task_id: Option<TaskId>,
    r1_output: Json,
    r1_failure: Option<String>,
    required_records: Vec<String>,
    relevant_records: Vec<String>,
    forbidden_records: Vec<String>,
    required_raw: Vec<String>,
    forbidden_sources: Vec<String>,
    expected_path: Json,
    stored_bytes: usize,
    overlay_sources: Vec<SourceRecord>,
}

fn data() -> (Corpus, Queries) {
    let corpus: Corpus = serde_json::from_str(include_str!("eval/retrieval/corpus.json")).unwrap();
    let queries: Queries =
        serde_json::from_str(include_str!("eval/retrieval/queries.json")).unwrap();
    assert_eq!(corpus.schema, "retrieval-corpus.v1");
    assert_eq!(queries.schema, "retrieval-queries.v1");
    assert_eq!(corpus.revision, queries.revision);
    assert_eq!(queries.r1_contract, "r1.v2");
    assert_eq!(queries.budgets, [64, 8]);
    (corpus, queries)
}

fn catalogue(c: &Corpus) -> Vec<w2::Relation> {
    let mut relations: BTreeMap<String, Vec<MemoryType>> = BTreeMap::new();
    for r in &c.records {
        let types = relations.entry(r.memory.predicate.0.clone()).or_default();
        if !types.contains(&r.memory.memory_type) {
            types.push(r.memory.memory_type);
        }
    }
    relations
        .into_iter()
        .map(|(id, memory_types)| w2::Relation { id, memory_types })
        .collect()
}

struct Db(PathBuf);
impl Drop for Db {
    fn drop(&mut self) {
        std::fs::remove_dir_all(self.0.parent().unwrap()).unwrap();
    }
}
impl Db {
    fn open(&self, owner: &str) -> SqliteMemoryStore {
        SqliteMemoryStore::open(&self.0, UserId::from(owner)).unwrap()
    }
    fn operation(&self, owner: &str) -> SqliteMemoryStore {
        SqliteMemoryStore::open_operation(&self.0, UserId::from(owner)).unwrap()
    }
    fn seed(c: &Corpus, order: &str) -> Self {
        let directory = std::env::temp_dir().join(format!("af173-{}", diagnostics::new_id()));
        std::fs::create_dir_all(&directory).unwrap();
        let db = Self(directory.join("baseline.sqlite3"));
        for owner in ["A", "B"] {
            let mut sources: Vec<_> = c.sources.iter().filter(|s| s.owner == owner).collect();
            let mut records: Vec<_> = c
                .records
                .iter()
                .filter(|r| r.memory.owner_id.0 == owner)
                .collect();
            if order == "reverse" {
                sources.reverse();
                records.reverse();
            }
            db.open(owner)
                .transaction(|state| {
                    for e in c.entities.iter().filter(|e| e.owner_id.0 == owner) {
                        state.entities.upsert(e.clone());
                    }
                    // Physical insertion order never changes IDs, statement clocks or sequence.
                    for _ in 0..c.records.len() {
                        state.repository.allocate_id()?;
                    }
                    for r in &records {
                        state.repository.insert(r.memory.clone())?;
                        state
                            .w5_receipts
                            .insert(r.receipt.group_id.clone(), r.receipt.clone());
                        state
                            .record_updates
                            .insert(r.memory.id, r.record_update.clone());
                    }
                    for relation in &c.relations {
                        if relation
                            .members
                            .iter()
                            .all(|id| records.iter().any(|r| r.memory.id == *id))
                        {
                            state
                                .relations
                                .insert(relation.id.clone(), relation.clone());
                        }
                    }
                    Ok(())
                })
                .unwrap();
            // Separate inserts exercise SQLite/FTS row order rather than only BTreeMap order.
            for source in sources {
                db.open(owner)
                    .transaction(|state| {
                        state
                            .sources
                            .insert(source.record.id.clone(), source.record.clone());
                        Ok(())
                    })
                    .unwrap();
            }
        }
        db.verify(c, false);
        let mut service =
            hippocampus::chat::ChatMemoryService::new(db.open("A"), Default::default()).unwrap();
        for id in &c.delete_sources {
            service
                .call("memory_forget", json!({"source_id": id}), || false)
                .unwrap();
        }
        drop(service);
        db.verify(c, true);
        db
    }
    fn verify(&self, c: &Corpus, deleted: bool) {
        let catalogue = catalogue(c);
        let sql = rusqlite::Connection::open(&self.0).unwrap();
        // FTS5's external-content integrity check also checks the inverted index.
        sql.execute(
            "INSERT INTO source_fts(source_fts, rank) VALUES('integrity-check', 1)",
            [],
        )
        .unwrap();
        for owner in ["A", "B"] {
            let state = self.open(owner).read().unwrap();
            let entities: Vec<_> = c
                .entities
                .iter()
                .filter(|e| e.owner_id.0 == owner)
                .map(|e| e.id.clone())
                .collect();
            for r in c.records.iter().filter(|r| r.memory.owner_id.0 == owner) {
                let SourceRef::Stored { source_id } = &r.contract.source else {
                    panic!("fixture source");
                };
                let s = c
                    .sources
                    .iter()
                    .find(|s| &s.record.id == source_id && s.owner == owner)
                    .unwrap();
                let body = s.record.request.as_ref().unwrap()["text"].as_str().unwrap();
                let task = s.record.request.as_ref().unwrap()["task_id"]
                    .as_str()
                    .map(TaskId::from);
                let checked = w2::validate(
                    W1Output {
                        memory_requests: vec![],
                        candidates: vec![r.contract.candidate.clone()],
                    },
                    &w2::Context {
                        text: body,
                        entities: &entities,
                        relations: &catalogue,
                        said_on: r.contract.said_on.as_deref(),
                        utc_offset_seconds: c.utc_offset_seconds,
                        task_id: task.as_ref(),
                    },
                )
                .unwrap();
                assert_eq!(
                    checked.candidates[0].status,
                    w2::Status::Ok,
                    "{} {:?}",
                    r.label,
                    checked.candidates[0].reasons
                );
                assert_eq!(
                    checked.candidates[0].candidate, r.contract.candidate,
                    "{} W2 normalized fixture",
                    r.label
                );
                assert_eq!(
                    checked.candidates[0].time_resolutions, r.contract.time_resolutions,
                    "{} time",
                    r.label
                );
                let group: W5Group = serde_json::from_str(&r.receipt.fingerprint).unwrap();
                assert_eq!(
                    serde_json::to_string(&serde_json::to_value(&group).unwrap()).unwrap(),
                    r.receipt.fingerprint,
                    "{} canonical receipt",
                    r.label
                );
                let projected = r
                    .contract
                    .project(&ProjectionContext {
                        owner_id: UserId::from(owner),
                        session_id: SessionId::from("af173"),
                        turn_id: TurnId::from(r.label.as_str()),
                        evidence_id: EvidenceId::from(format!("ev-{}", r.label).as_str()),
                        observed_at: Timestamp(s.record.observed_at),
                        task_id: task,
                        explicit_memory_request: false,
                    })
                    .unwrap();
                assert_eq!(
                    group.candidates[0].candidate, projected.candidate,
                    "{} projection",
                    r.label
                );
                assert_eq!(group.candidates[0].contract.as_ref(), Some(&r.contract));
                assert_eq!(r.receipt.status, W5Status::Committed);
                assert_eq!(r.receipt.products[&0], r.memory.id);
                assert_eq!(r.receipt.updates[&0], r.record_update);
                assert_eq!(r.record_update.status, UpdateStatus::Judged);
                assert_eq!(r.memory.value, projected.candidate.value);
                assert_eq!(r.memory.evidence, projected.candidate.evidence);
                assert_eq!(
                    r.memory.subject_id,
                    *projected.candidate.subject_id().unwrap()
                );
                if deleted && c.delete_sources.contains(source_id) {
                    assert!(w4::record_view(&state, r.memory.id).is_none());
                    assert!(state.sources[source_id].request.is_none());
                    assert_eq!(sql.query_row("SELECT COUNT(*) FROM source_segments WHERE owner_id=?1 AND source_id=?2", [owner, source_id], |row| row.get::<_, i64>(0)).unwrap(), 0);
                    continue;
                }
                let RecordView2::Current(view) = w4::record_view(&state, r.memory.id).unwrap()
                else {
                    panic!("{} became legacy", r.label);
                };
                assert_eq!(view.candidate, r.contract.candidate);
                assert_eq!(view.grade, Grade::Committed);
                assert_eq!(view.time_resolutions, r.contract.time_resolutions);
                assert_eq!(view.update, r.record_update);
                assert!(!view.evidence.is_empty());
                for e in &view.evidence {
                    assert!(locate_quote(body, &e.quote.text).is_some());
                }
                assert_eq!(state.sources[source_id].enrichment, "committed");
                assert_eq!(state.sources[source_id].index["status"], "ready");
                let (indexed, version): (String, i64) = sql.query_row("SELECT text, source_version FROM source_segments WHERE owner_id=?1 AND source_id=?2", [owner, source_id], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
                assert_eq!(indexed, body);
                assert_eq!(state.sources[source_id].index["source_version"], version);
                if let Some(old) = r.memory.supersedes {
                    let prior = state.repository.get(old).unwrap().unwrap();
                    assert_eq!(prior.superseded_by, Some(r.memory.id));
                    assert_eq!(prior.supersede_reason, Some(SupersedeReason::StateChanged));
                    assert!(view.history.contains(&old));
                    assert_eq!(prior.status, MemoryStatus::Superseded);
                }
            }
            let expected_sources = c
                .sources
                .iter()
                .filter(|s| {
                    s.owner == owner && !(deleted && c.delete_sources.contains(&s.record.id))
                })
                .count();
            assert_eq!(
                sql.query_row(
                    "SELECT COUNT(*) FROM source_segments WHERE owner_id=?1",
                    [owner],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
                expected_sources as i64
            );
        }
        let state = self.open("A").read().unwrap();
        for relation in &c.relations {
            assert_eq!(&state.relations[&relation.id], relation);
            for basis in &relation.basis {
                assert!(
                    locate_quote(
                        state.sources[&basis.source_id].request.as_ref().unwrap()["text"]
                            .as_str()
                            .unwrap(),
                        &basis.quote
                    )
                    .is_some()
                );
            }
            for id in &relation.members {
                let RecordView2::Current(view) = w4::record_view(&state, *id).unwrap() else {
                    panic!("relation member");
                };
                assert!(view.open_relations.contains(&relation.id));
            }
        }
        if c.revision == "AF293" {
            assert_eq!(c.records.len(), 144);
            assert_eq!(c.sources.len(), 144);
            return; // Independent fixture; the F0001–F0100 rule belongs only to AF173.
        }
        for number in 1..=100 {
            let label = format!("F{number:04}");
            let r = c.records.iter().find(|r| r.label == label).unwrap();
            assert_eq!(
                r.contract.candidate.sentence.text,
                format!("我的陶片标记为 F{number:04}。")
            );
            assert_eq!(r.memory.subject_id.0, "self");
            assert_eq!(r.memory.predicate.0, "fact");
        }
        assert_eq!(c.fill_rule["first"], "F0001");
        assert_eq!(c.fill_rule["last"], "F0100");
    }
}

fn request_and_plan(c: &Corpus, q: &Query) -> (Request, r1::Plan) {
    let request = Request {
        owner: UserId::from(q.owner.as_str()),
        subject_ids: q.subject_ids.clone(),
        said_on: c.said_on.clone(),
        utc_offset_seconds: c.utc_offset_seconds,
        bootstrap: false,
        binding: RecallRequest {
            request_id: format!("af173-{}", q.id),
            query: q.query.clone(),
            context: vec![],
            task_id: q.task_id.as_ref().map(|t| t.0.clone()),
            at: c.at,
        },
    };
    let entities: Vec<_> = c
        .entities
        .iter()
        .filter(|e| e.owner_id == request.owner)
        .map(|e| e.id.clone())
        .collect();
    let relations = catalogue(c);
    let ctx = r1::Context {
        query: &q.query,
        context: &request.binding.context,
        entities: &entities,
        relations: &relations,
        subject_ids: request.subject_ids.as_deref(),
        task_id: q.task_id.as_ref(),
        said_on: &c.said_on,
        utc_offset_seconds: c.utc_offset_seconds,
        supported_units: &[],
    };
    let decoded = r1::decode(&serde_json::to_string(&q.r1_output).unwrap());
    let plan = match q.r1_failure.as_deref() {
        None => {
            let (plan, diagnostic) = r1::validate_detailed(decoded.unwrap(), &ctx).unwrap();
            assert!(diagnostic.is_none(), "{} R1 {:?}", q.id, diagnostic);
            assert!(plan.output.is_some());
            plan
        }
        Some("decode") => {
            assert!(decoded.is_err());
            r1::not_understood(&ctx).unwrap()
        }
        Some("not_called") => {
            assert!(q.r1_output.is_null());
            r1::not_understood(&ctx).unwrap()
        }
        Some("validate") => {
            let (plan, diagnostic) = r1::validate_detailed(decoded.unwrap(), &ctx).unwrap();
            let diagnostic = diagnostic.unwrap();
            assert_eq!(diagnostic.field, "plan.filter.relation");
            assert_eq!(diagnostic.rule, "relation_catalogue");
            assert!(plan.output.is_none());
            plan
        }
        _ => panic!("unexpected R1 failure mode"),
    };
    (request, plan)
}

fn omitted_at(d: &RetrievalDiagnostic, id: MemoryId) -> &'static str {
    for (stage, reason) in [
        ("owner", "owner"),
        ("trusted_scope", "trusted_scope"),
        ("r1_subject", "r1_subject"),
        ("relation_filter", "relation_filter"),
        ("seeds", "lexical_no_hit"),
        ("projected", "projection"),
        ("record_count_budget", "record_count_budget"),
        ("record_byte_budget", "record_byte_budget"),
    ] {
        if !d.stages[stage].contains(&id)
            && !(matches!(stage, "r1_subject" | "relation_filter" | "seeds")
                && d.stages["closure"].contains(&id))
        {
            return reason;
        }
    }
    "returned"
}

fn evaluate(c: &Corpus, q: &Query, db: &Db, order: &str, records: usize) -> Json {
    let (request, plan) = request_and_plan(c, q);
    let store = db.open(&q.owner);
    let budget = Budget {
        records,
        stored_bytes: q.stored_bytes,
        ..Budget::default()
    };
    let start = Instant::now();
    let assembly = recall::assemble_diagnosed(&store, &request, &plan, budget).unwrap();
    let elapsed_us = start.elapsed().as_micros();
    evaluate_assembly(
        c, q, &store, order, records, &request, &plan, budget, &assembly, elapsed_us, true,
    )
}

fn evaluate_assembly(
    c: &Corpus,
    q: &Query,
    store: &SqliteMemoryStore,
    order: &str,
    records: usize,
    request: &Request,
    plan: &r1::Plan,
    budget: Budget,
    assembly: &recall::Assembly,
    elapsed_us: u128,
    baseline: bool,
) -> Json {
    let evidence = &assembly.evidence;
    let diagnostic = assembly.retrieval_diagnostic().unwrap();
    assert!(!diagnostic.fts_error, "{} FTS error", q.id);
    for key in ["fts_called", "enumeration", "relation_revoked"] {
        if baseline && !q.expected_path[key].is_null() {
            assert_eq!(
                serde_json::to_value(diagnostic).unwrap()[key],
                q.expected_path[key],
                "{} path {key}",
                q.id
            );
        }
    }
    let labels: BTreeMap<_, _> = c
        .records
        .iter()
        .map(|r| (r.memory.id, r.label.as_str()))
        .collect();
    let returned: Vec<_> = evidence
        .records
        .iter()
        .map(|r| labels[&r.id()].to_owned())
        .collect();
    let required: BTreeSet<_> = q.required_records.iter().collect();
    let relevant: BTreeSet<_> = q.relevant_records.iter().collect();
    assert!(required.is_subset(&relevant));
    assert_eq!(required.len(), q.required_records.len());
    for label in q
        .required_records
        .iter()
        .chain(q.relevant_records.iter())
        .chain(q.forbidden_records.iter())
    {
        assert!(
            c.records.iter().any(|r| &r.label == label),
            "{} unknown label {label}",
            q.id
        );
    }
    let mut source_quotes: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut add_quote = |e: &EvidenceRef| {
        let SourceRef::Stored { source_id } = &e.source else {
            panic!("non-stored R3 evidence");
        };
        source_quotes
            .entry(source_id.clone())
            .or_default()
            .push(e.quote.text.clone());
    };
    for r in &evidence.records {
        let RecordView::Current(v) = r else {
            panic!("{} legacy fixture result", q.id);
        };
        assert_eq!(v.grade, Grade::Committed);
        for e in &v.evidence {
            add_quote(e);
        }
    }
    for relation in &evidence.relations {
        for e in &relation.basis {
            add_quote(e);
        }
        if let Some(e) = &relation.resolution {
            add_quote(e);
        }
    }
    for raw in &evidence.raw {
        source_quotes
            .entry(raw.source_id.clone())
            .or_default()
            .push(raw.quote.text.clone());
    }
    let state = store.read().unwrap();
    for (source, quotes) in &source_quotes {
        assert!(
            !q.forbidden_sources.contains(source),
            "{} forbidden source {source}",
            q.id
        );
        let s = &state.sources[source];
        let body = s.request.as_ref().unwrap()["text"].as_str().unwrap();
        assert_ne!(s.enrichment, "deleted");
        let task = s.request.as_ref().unwrap()["task_id"].as_str();
        assert!(task.is_none() || task == request.binding.task_id.as_deref());
        for quote in quotes {
            assert!(locate_quote(body, quote).is_some());
        }
    }
    for r in &evidence.records {
        let memory = state.repository.get(r.id()).unwrap().unwrap();
        assert_eq!(memory.owner_id, request.owner);
        assert_ne!(memory.status, MemoryStatus::Deleted);
        if let Some(scope) = &request.subject_ids {
            assert!(scope.contains(&memory.subject_id));
        }
    }
    for view in &evidence.records {
        if let RecordView::Current(v) = view {
            for id in &v.history {
                assert!(!q.forbidden_records.iter().any(|label| labels[id] == label));
            }
        }
    }
    for relation in &evidence.relations {
        for id in &relation.members {
            assert!(!q.forbidden_records.iter().any(|label| labels[id] == label));
        }
    }
    for forbidden in &q.forbidden_records {
        assert!(
            !returned.contains(forbidden),
            "{} forbidden record {forbidden}",
            q.id
        );
    }
    for source in &q.required_raw {
        assert!(
            evidence.raw.iter().any(|r| &r.source_id == source),
            "{} raw not carried",
            q.id
        );
    }
    assert!(evidence.records.len() <= records);
    let used_bytes: usize = evidence
        .records
        .iter()
        .map(|r| serde_json::to_vec(r).unwrap().len())
        .sum::<usize>()
        + evidence
            .raw
            .iter()
            .map(|r| serde_json::to_vec(r).unwrap().len())
            .sum::<usize>()
        + evidence
            .relations
            .iter()
            .map(|r| serde_json::to_vec(r).unwrap().len())
            .sum::<usize>();
    assert!(used_bytes <= budget.stored_bytes);
    if q.id == "C02" {
        assert!(
            evidence.records.is_empty() && evidence.raw.is_empty() && evidence.relations.is_empty()
        );
    }
    if q.id == "C06" {
        assert!(evidence.records.is_empty() && evidence.raw.is_empty());
        assert!(evidence.completeness.retrieval_truncated);
        assert!(!evidence.completeness.plan_complete);
        assert!(!diagnostic.stages["record_count_budget"].is_empty());
    }
    // Observation cannot change the frozen payload, host output, or persisted state.
    if baseline
        && order == "canonical"
        && records == 64
        && matches!(q.id.as_str(), "Q01" | "Q02" | "C03" | "C06")
    {
        let ordinary = recall::assemble(&store, &request, &plan, budget).unwrap();
        assert!(ordinary.retrieval_diagnostic().is_none());
        assert_eq!(ordinary.evidence, assembly.evidence);
        assert_eq!(
            recall_models::r4_input(&ordinary, c.utc_offset_seconds),
            recall_models::r4_input(&assembly, c.utc_offset_seconds)
        );
    }
    let missing: Vec<_> = required
        .iter()
        .filter(|id| !returned.contains(id))
        .map(|id| {
            let record = c.records.iter().find(|r| &r.label == *id).unwrap();
            json!({"id":id,"stage":omitted_at(diagnostic,record.memory.id)})
        })
        .collect();
    let hits = required.len() - missing.len();
    let recall = (!required.is_empty()).then(|| hits as f64 / required.len() as f64);
    let mrr = (!required.is_empty()).then(|| {
        returned
            .iter()
            .position(|id| required.contains(id))
            .map_or(0.0, |index| 1.0 / (index + 1) as f64)
    });
    let precision = (!returned.is_empty()).then(|| {
        returned.iter().filter(|id| relevant.contains(id)).count() as f64 / returned.len() as f64
    });
    let required_sources: BTreeSet<_> = c
        .records
        .iter()
        .filter(|r| required.contains(&r.label))
        .map(|r| match &r.contract.source {
            SourceRef::Stored { source_id } => source_id.clone(),
            _ => unreachable!(),
        })
        .chain(q.required_raw.iter().cloned())
        .collect();
    let full_sources: Vec<_> = required_sources
        .iter()
        .filter(|id| {
            let body = state.sources[*id].request.as_ref().unwrap()["text"]
                .as_str()
                .unwrap();
            source_quotes.get(*id).is_some_and(|quotes| {
                quotes
                    .iter()
                    .any(|quote| locate_quote(quote, body).is_some())
            })
        })
        .cloned()
        .collect();
    let coverage = (!required_sources.is_empty())
        .then(|| full_sources.len() as f64 / required_sources.len() as f64);
    let stages: BTreeMap<_, _> = diagnostic
        .stages
        .iter()
        .map(|(stage, ids)| (stage, ids.iter().map(|id| labels[id]).collect::<Vec<_>>()))
        .collect();
    let ranking: Vec<_> = diagnostic.ranking.iter().map(|r| json!({"id":labels[&r.id],"entity_hits":r.entity_hits,"phrase_hits":r.phrase_hits,"received_sequence":r.received_sequence})).collect();
    let selection: BTreeMap<_, _> = diagnostic
        .selection
        .iter()
        .map(|(id, reasons)| (labels[id], reasons))
        .collect();
    let result = json!({"id":q.id,"query":q.query,"category":q.category,"control":q.control,"r1_failure":q.r1_failure,"r1_not_understood":plan.output.is_none(),"insertion_order":order,"records_budget":records,"stored_bytes_budget":q.stored_bytes,
        "required":q.required_records,"relevant":q.relevant_records,"returned":returned,"returned_count":returned.len(),"required_all": missing.is_empty(),
        "recall":recall,"mrr":mrr,"precision":precision,"missing":missing,"r3_original_coverage":coverage,"required_sources":required_sources,"covered_sources":full_sources,
        "raw_sources":evidence.raw.iter().map(|r| &r.source_id).collect::<Vec<_>>(),"relations":evidence.relations.iter().map(|r| &r.id).collect::<Vec<_>>(),
        "completeness":evidence.completeness,"diagnostic":{"stages":stages,"raw_stages":diagnostic.raw_stages,"relation_stages":diagnostic.relation_stages,"requested_relation":diagnostic.requested_relation,"effective_relation":diagnostic.effective_relation,
            "relation_revoked":diagnostic.relation_revoked,"enumeration":diagnostic.enumeration,"fts_called":diagnostic.fts_called,"ranking":ranking,"selection":selection},"elapsed_us":elapsed_us});
    result
}

fn summary(rows: &[Json]) -> Vec<Json> {
    let mut groups: BTreeMap<(u64, String), Vec<&Json>> = BTreeMap::new();
    for row in rows
        .iter()
        .filter(|r| !r["control"].as_bool().unwrap() && r["insertion_order"] == "canonical")
    {
        groups
            .entry((
                row["records_budget"].as_u64().unwrap(),
                row["category"].as_str().unwrap().into(),
            ))
            .or_default()
            .push(row);
    }
    groups.into_iter().map(|((budget, category), group)| {
        let applicable: Vec<_> = group.iter().filter(|r| !r["recall"].is_null()).collect();
        let average = |field: &str| {
            let values: Vec<_> = group.iter().filter_map(|r| r[field].as_f64()).collect();
            (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
        };
        json!({"records_budget":budget,"category":category,"queries":group.len(),"applicable":applicable.len(),"required_all":applicable.iter().filter(|r| r["required_all"]==true).count(),
            "recall_macro":average("recall"),"mrr_macro":average("mrr"),"precision_macro":average("precision"),"misses":group.iter().filter(|r| !r["missing"].as_array().unwrap().is_empty()).map(|r| json!({"id":r["id"],"missing":r["missing"]})).collect::<Vec<_>>()})
    }).collect()
}
fn markdown(report: &Json) -> String {
    let mut out = String::from(
        "# AF173 检索基线\n\n固定 R1，不评估 R1/R4 语义；发送 0 次。质量分不作测试门槛。耗时仅为本机 SQLite R2/R3 组装。\n\n## 分类（canonical；空 required 不计 recall/MRR）\n\n| records | 类别 | 全覆盖/适用题 | recall | MRR | precision | 漏召回 |\n|---|---|---|---|---|---|---|\n",
    );
    for s in report["summary"].as_array().unwrap() {
        out.push_str(&format!(
            "| {} | {} | {}/{} | {} | {} | {} | {} |\n",
            s["records_budget"],
            s["category"].as_str().unwrap(),
            s["required_all"],
            s["applicable"],
            s["recall_macro"],
            s["mrr_macro"],
            s["precision_macro"],
            s["misses"]
        ));
    }
    out.push_str("\n## 逐题与顺序对照\n\n| 顺序 | 题 | 类别 | records | 必需全覆盖 | recall/MRR/precision | 返回 | R3原文覆盖 | 完整性 | 路径与遗漏 | μs |\n|---|---|---|---|---|---|---|---|---|---|---|\n");
    for r in report["rows"].as_array().unwrap() {
        out.push_str(&format!("| {} | {} | {} | {} | {} | {}/{}/{} | {} | {} | {} | enum={} FTS={} relation={} revoked={} missing={} | {} |\n",
            r["insertion_order"].as_str().unwrap(),r["id"].as_str().unwrap(),r["category"].as_str().unwrap(),r["records_budget"],r["required_all"],r["recall"],r["mrr"],r["precision"],r["returned_count"],r["r3_original_coverage"],r["completeness"],r["diagnostic"]["enumeration"],r["diagnostic"]["fts_called"],r["diagnostic"]["effective_relation"],r["diagnostic"]["relation_revoked"],r["missing"],r["elapsed_us"]));
    }
    out.push_str("\n排序、阶段集合、每条记录的选择机制、数量/字节裁剪详见 report.json。安全违规 0；两种物理插入顺序结果一致。\n");
    out
}

#[test]
fn af173_retrieval_baseline() {
    let ((rows, c), events) = diagnostics::capture_model_events(|| {
        let (c, queries) = data();
        let ids: BTreeSet<_> = queries.queries.iter().map(|q| &q.id).collect();
        assert_eq!(ids.len(), queries.queries.len());
        assert_eq!(queries.queries.len(), 40);
        let record_ids: BTreeSet<_> = c.records.iter().map(|r| r.memory.id).collect();
        assert_eq!(record_ids.len(), c.records.len());
        let mut rows = Vec::new();
        for order in &c.insertion_orders {
            let db = Db::seed(&c, order);
            for q in &queries.queries {
                if !q.overlay_sources.is_empty() {
                    db.open(&q.owner)
                        .transaction(|state| {
                            for source in &q.overlay_sources {
                                state.sources.insert(source.id.clone(), source.clone());
                            }
                            Ok(())
                        })
                        .unwrap();
                }
                for records in &queries.budgets {
                    rows.push(evaluate(&c, q, &db, order, *records));
                }
                if !q.overlay_sources.is_empty() {
                    let mut service = hippocampus::chat::ChatMemoryService::new(
                        db.open(&q.owner),
                        Default::default(),
                    )
                    .unwrap();
                    for source in &q.overlay_sources {
                        service
                            .call("memory_forget", json!({"source_id":source.id}), || false)
                            .unwrap();
                    }
                }
                println!("AF173 {order} {} done", q.id);
            }
        }
        (rows, c)
    });
    assert!(events.is_empty(), "baseline invoked models: {events:?}");
    let mut canonical = BTreeMap::new();
    for row in &rows {
        let mut normalized = row.clone();
        normalized.as_object_mut().unwrap().remove("elapsed_us");
        normalized
            .as_object_mut()
            .unwrap()
            .remove("insertion_order");
        let key = (
            row["id"].as_str().unwrap().to_owned(),
            row["records_budget"].as_u64().unwrap(),
        );
        if row["insertion_order"] == "canonical" {
            canonical.insert(key, normalized);
        } else {
            assert_eq!(
                &normalized, &canonical[&key],
                "insertion order changed {}",
                row["id"]
            );
        }
    }
    let report = json!({"schema":"retrieval-report.v1","revision":c.revision,"model_calls":0,"safety_violations":0,"insertion_order_equal":true,
        "corpus_fingerprint":recall_models::fingerprint(&serde_json::from_str::<Json>(include_str!("eval/retrieval/corpus.json")).unwrap()),
        "queries_fingerprint":recall_models::fingerprint(&serde_json::from_str::<Json>(include_str!("eval/retrieval/queries.json")).unwrap()),
        "corpus_records_before_delete":c.records.len(),"queries":40,"assemblies":rows.len(),"summary":summary(&rows),"rows":rows});
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/eval-retrieval/af173");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("report.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
    std::fs::write(directory.join("report.md"), markdown(&report)).unwrap();
    println!(
        "AF173: {} queries × 2 budgets × 2 insertion orders = {} assemblies; models=0 safety=0; {}",
        report["queries"],
        report["assemblies"],
        directory.display()
    );
}

// AF175 stays an offline simulator; no production recall switch is introduced.
mod af175 {
    use super::*;
    use hippocampus::embedding::{self, Batch, Binding, Model, Ollama};
    use serde::Serialize;
    use std::io::Write;

    const MODES: [&str; 5] = [
        "current",
        "lexical-df",
        "vector-only",
        "hybrid",
        "hybrid-df",
    ];

    #[derive(Clone, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Item {
        key: String,
        text: String,
        source_id: Option<String>,
        owner: String,
        task: Option<String>,
        start: usize,
        end: usize,
    }
    #[derive(Clone, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Job {
        key: String,
        items: Vec<Item>,
        kind: String,
    }

    fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("target/eval-retrieval/af175")
    }
    fn fp(value: &Json) -> String {
        recall_models::fingerprint(value)
    }
    fn file_id(model: Model) -> &'static str {
        match model {
            Model::Qwen => "qwen3-embedding-0.6b",
            Model::Bge => "bge-m3",
        }
    }
    fn document_items(c: &Corpus, queries: &Queries) -> Vec<Item> {
        let mut sources: BTreeMap<String, (String, SourceRecord)> = c
            .sources
            .iter()
            .filter(|s| s.owner == "A" && !c.delete_sources.contains(&s.record.id))
            .map(|s| (s.record.id.clone(), (s.owner.clone(), s.record.clone())))
            .collect();
        for q in &queries.queries {
            for source in &q.overlay_sources {
                sources.insert(source.id.clone(), (q.owner.clone(), source.clone()));
            }
        }
        sources
            .into_values()
            .flat_map(|(owner, source)| {
                assert_eq!(source.enrichment == "deleted", false);
                let request = source.request.as_ref().unwrap();
                assert_eq!(request["capture_source"], true);
                let text = request["text"].as_str().unwrap();
                embedding::segments(text)
                    .into_iter()
                    .map(|(start, end)| Item {
                        key: format!("doc:{}:{start}:{end}", source.id),
                        text: text[start..end].into(),
                        source_id: Some(source.id.clone()),
                        owner: owner.clone(),
                        task: request["task_id"].as_str().map(str::to_owned),
                        start,
                        end,
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }
    fn jobs(c: &Corpus, queries: &Queries, model: Model) -> Vec<Job> {
        let mut seen = BTreeSet::new();
        let items: Vec<_> = queries
            .queries
            .iter()
            .filter(|q| seen.insert(q.query.clone()))
            .map(|q| Item {
                key: format!("query:{}", fp(&json!(q.query))),
                text: model.query(&q.query),
                source_id: None,
                owner: q.owner.clone(),
                task: None,
                start: 0,
                end: 0,
            })
            .collect();
        let probe = items[0].clone();
        let mut jobs = vec![
            Job {
                key: "query-first".into(),
                items: vec![probe.clone()],
                kind: "query_first".into(),
            },
            Job {
                key: "query-warm".into(),
                items: vec![probe],
                kind: "query_warm".into(),
            },
        ];
        for (index, chunk) in document_items(c, queries)
            .chunks(embedding::DEFAULT_BATCH)
            .enumerate()
        {
            jobs.push(Job {
                key: format!("docs-{index:04}"),
                items: chunk.to_vec(),
                kind: "documents".into(),
            });
        }
        for (index, chunk) in items[1..].chunks(embedding::DEFAULT_BATCH).enumerate() {
            jobs.push(Job {
                key: format!("queries-{index:04}"),
                items: chunk.to_vec(),
                kind: "queries".into(),
            });
        }
        jobs
    }
    fn inputs(job: &Job) -> Vec<String> {
        job.items.iter().map(|i| i.text.clone()).collect()
    }
    fn job_fp(binding: &Binding, job: &Job) -> String {
        fp(&json!({"embedding_fingerprint":binding.fingerprint(&inputs(job)).unwrap(),"job":job}))
    }
    fn plan(c: &Corpus, queries: &Queries) -> Json {
        let models:Vec<_>=[Model::Qwen,Model::Bge].into_iter().map(|model| {
            let jobs=jobs(c,queries,model);
            json!({"model":model.tag(),"digest":"unresolved; no Ollama contact in dry-run","dimensions":embedding::DIMENSIONS,
                "document_segments":document_items(c,queries).len(),"unique_queries":queries.queries.iter().map(|q|&q.query).collect::<BTreeSet<_>>().len(),
                "document_requests":jobs.iter().filter(|j|j.kind=="documents").count(),"query_requests":jobs.iter().filter(|j|j.kind=="queries").count(),
                "first_warm_probe_requests":2,"embedding_requests":jobs.len(),"metadata_http_requests":2+2*jobs.len(),
                "formatted_input_bytes":jobs.iter().flat_map(|j|&j.items).map(|i|i.text.len()).sum::<usize>(),"batch_max":embedding::DEFAULT_BATCH,"quota":jobs.len()})
        }).collect();
        json!({"campaign":"AF175","mode":"dry-run","sent":0,"models":models,"total_embedding_requests":models.iter().map(|m|m["embedding_requests"].as_u64().unwrap()).sum::<u64>(),
            "total_metadata_http_requests":models.iter().map(|m|m["metadata_http_requests"].as_u64().unwrap()).sum::<u64>(),
            "cold_probe":"first query request, not guaranteed unloaded; use load_duration; next request is warm probe","ollama_peak_memory":null})
    }
    fn atomic_write(path: &Path, value: &Json) -> Result<(), String> {
        let temp = path.with_extension(format!("tmp-{}", diagnostics::new_id()));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|e| e.to_string())?;
        file.write_all(&serde_json::to_vec_pretty(value).unwrap())
            .map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        std::fs::rename(&temp, path).map_err(|e| e.to_string())?;
        std::fs::File::open(path.parent().unwrap())
            .and_then(|file| file.sync_all())
            .map_err(|e| e.to_string())
    }
    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Campaign {
        binding: Binding,
        fingerprint: String,
        quota: usize,
        sent: BTreeMap<String, String>,
    }
    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Recording {
        fingerprint: String,
        result: Result<Batch, String>,
        result_hash: String,
    }
    impl Recording {
        fn checked(&self, fingerprint: &str, count: usize) -> Result<Option<Batch>, String> {
            if self.fingerprint != fingerprint
                || self.result_hash != fp(&serde_json::to_value(&self.result).unwrap())
            {
                return Err("embedding_recording_stale_or_corrupt".into());
            }
            match &self.result {
                Ok(batch) => {
                    if batch.vectors.len() != count
                        || !batch.elapsed_ms.is_finite()
                        || batch.elapsed_ms < 0.0
                    {
                        return Err("embedding_recording_batch".into());
                    }
                    for vector in &batch.vectors {
                        embedding::check_normalized(vector, embedding::DIMENSIONS)?;
                    }
                    Ok(Some(batch.clone()))
                }
                Err(_) => Ok(None),
            }
        }
    }
    struct Lock(PathBuf);
    impl Drop for Lock {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    fn lock(path: &Path) -> Result<Lock, String> {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|_| "AF175 locked; stale lock requires manual inspection")?;
        Ok(Lock(path.into()))
    }
    fn cached_or_send(
        directory: &Path,
        campaign: &mut Campaign,
        job: &Job,
        send: Option<&Ollama>,
        statuses: &mut Vec<Json>,
    ) -> Result<Option<Batch>, String> {
        let expected = job_fp(&campaign.binding, job);
        let path = directory.join(format!("{}.json", job.key));
        if path.exists() {
            if campaign.sent.get(&job.key) != Some(&expected) {
                return Err("embedding_recording_without_sent_slot".into());
            }
            let recording: Recording =
                serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            let result = recording.checked(&expected, job.items.len())?;
            statuses.push(json!({"job":job.key,"status":if result.is_some(){"reused"}else{"recorded_failure"},"error":recording.result.as_ref().err()}));
            return Ok(result);
        }
        if campaign.sent.contains_key(&job.key) {
            statuses
                .push(json!({"job":job.key,"status":"unrecorded_inflight","charged_slot":true}));
            return Ok(None);
        }
        let Some(client) = send else {
            statuses.push(json!({"job":job.key,"status":"missing","charged_slot":false}));
            return Ok(None);
        };
        if campaign.sent.len() >= campaign.quota {
            return Err("embedding_quota_exhausted".into());
        }
        campaign.sent.insert(job.key.clone(), expected.clone());
        atomic_write(
            &directory.join("campaign.json"),
            &serde_json::to_value(&*campaign).unwrap(),
        )?;
        let result = client.embed(&campaign.binding, &inputs(job));
        let recording = Recording {
            fingerprint: expected.clone(),
            result_hash: fp(&serde_json::to_value(&result).unwrap()),
            result,
        };
        atomic_write(&path, &serde_json::to_value(&recording).unwrap())?;
        let checked = recording.checked(&expected, job.items.len())?;
        statuses.push(json!({"job":job.key,"status":if checked.is_some(){"recorded"}else{"recorded_failure"},"error":recording.result.as_ref().err()}));
        Ok(checked)
    }
    fn model_vectors(
        c: &Corpus,
        queries: &Queries,
        model: Model,
        record: bool,
        base: &str,
        directory: &Path,
    ) -> Result<(Binding, BTreeMap<String, Vec<f32>>, Vec<Json>, Vec<Json>), String> {
        if !record && !directory.join("campaign.json").exists() {
            return Err("embedding_campaign_missing".into());
        }
        if record {
            std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
        }
        let _lock = lock(&directory.join("campaign.lock"))?;
        let client = if record {
            Some(Ollama::new(base)?)
        } else {
            None
        };
        let path = directory.join("campaign.json");
        let binding = if let Some(client) = &client {
            client.resolve(model)?
        } else {
            let campaign: Campaign =
                serde_json::from_slice(&std::fs::read(&path).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            campaign.binding
        };
        let expected_binding = Binding::from_tags(
            model,
            &json!({"models":[{"name":model.tag(),"digest":binding.digest}]}),
        )?;
        if binding != expected_binding {
            return Err("embedding_binding_stale_or_invalid".into());
        }
        let jobs = jobs(c, queries, model);
        let fingerprint = fp(&json!({"binding":binding,"jobs":jobs}));
        let mut campaign = if path.exists() {
            let existing: Campaign =
                serde_json::from_slice(&std::fs::read(&path).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            if existing.binding != binding
                || existing.fingerprint != fingerprint
                || existing.quota != jobs.len()
                || existing.sent.len() > existing.quota
                || existing.sent.iter().any(|(key, f)| {
                    !jobs
                        .iter()
                        .any(|j| &j.key == key && job_fp(&binding, j) == *f)
                })
            {
                return Err("embedding_campaign_stale_or_invalid; do not reset quota".into());
            }
            existing
        } else {
            let value = Campaign {
                binding: binding.clone(),
                fingerprint,
                quota: jobs.len(),
                sent: BTreeMap::new(),
            };
            atomic_write(&path, &serde_json::to_value(&value).unwrap())?;
            value
        };
        // Validate every existing recording before any new send.
        for job in &jobs {
            let path = directory.join(format!("{}.json", job.key));
            if path.exists() {
                if campaign.sent.get(&job.key) != Some(&job_fp(&binding, job)) {
                    return Err("embedding_recording_without_sent_slot".into());
                }
                let recording: Recording =
                    serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
                        .map_err(|e| e.to_string())?;
                recording.checked(&job_fp(&binding, job), job.items.len())?;
            }
        }
        let mut vectors = BTreeMap::new();
        let mut statuses = Vec::new();
        let mut usage = Vec::new();
        let mut stopped = false;
        for job in &jobs {
            let batch = cached_or_send(
                &directory,
                &mut campaign,
                job,
                if stopped { None } else { client.as_ref() },
                &mut statuses,
            )?;
            if let Some(batch) = batch {
                usage.push(json!({"job":job.key,"kind":job.kind,"input_tokens":batch.input_tokens,"elapsed_ms":batch.elapsed_ms,
                    "total_duration_ns":batch.total_duration_ns,"load_duration_ns":batch.load_duration_ns}));
                if job.kind != "query_warm" {
                    for (item, vector) in job.items.iter().zip(batch.vectors) {
                        vectors.insert(item.key.clone(), vector);
                    }
                }
            } else if statuses.last().unwrap()["status"] == "recorded_failure" {
                stopped = true;
            }
        }
        Ok((binding, vectors, statuses, usage))
    }
    fn view(state: &PersistedMemoryState, id: MemoryId) -> Option<RecordView> {
        let RecordView2::Current(v) = w4::record_view(state, id)? else {
            panic!("AF175 legacy fixture")
        };
        Some(RecordView::Current(CurrentRecordView {
            id: v.id,
            stored_status: v.stored_status,
            termination_reason: v.termination_reason,
            ended: v.ended,
            candidate: v.candidate,
            evidence: v.evidence,
            grade: v.grade,
            said_on: v.said_on,
            time_resolutions: v.time_resolutions,
            history: v.history,
            open_relations: v.open_relations,
            update: v.update,
        }))
    }
    fn allowed(c: &Corpus, q: &Query) -> BTreeSet<MemoryId> {
        let (_, plan) = request_and_plan(c, q);
        let subject = plan.output.and_then(|o| match o.subject {
            R1Subject::Entity(s) => Some(s.entity_id),
            _ => None,
        });
        scoped_records(c, q, subject.as_deref())
    }
    fn scoped_records(c: &Corpus, q: &Query, r1_subject: Option<&str>) -> BTreeSet<MemoryId> {
        c.records
            .iter()
            .filter(|r| r.memory.owner_id.0 == q.owner && r.memory.status != MemoryStatus::Deleted)
            .filter(|r| {
                r.memory
                    .scope
                    .conditions
                    .iter()
                    .all(|condition| match condition {
                        ScopeCondition::Task(task) => q.task_id.as_ref() == Some(task),
                        _ => true,
                    })
            })
            .filter(|r| {
                q.subject_ids
                    .as_ref()
                    .is_none_or(|ids| ids.contains(&r.memory.subject_id))
            })
            .filter(|r| r1_subject.is_none_or(|id| r.memory.subject_id.0 == id))
            .filter(|r| {
                let SourceRef::Stored { source_id } = &r.contract.source else {
                    return false;
                };
                !c.delete_sources.contains(source_id)
                    && c.sources.iter().any(|s| {
                        s.owner == q.owner
                            && &s.record.id == source_id
                            && s.record.enrichment != "deleted"
                            && s.record.request.as_ref().unwrap()["task_id"]
                                .as_str()
                                .is_none_or(|task| {
                                    Some(task) == q.task_id.as_ref().map(|t| t.0.as_str())
                                })
                    })
            })
            .map(|r| r.memory.id)
            .collect()
    }
    fn vector_ranking(
        c: &Corpus,
        q: &Query,
        docs: &[Item],
        vectors: &BTreeMap<String, Vec<f32>>,
    ) -> Result<Vec<(MemoryId, f64)>, String> {
        let key = format!("query:{}", fp(&json!(q.query)));
        let query = vectors.get(&key).ok_or("embedding_query_missing")?;
        let eligible = allowed(c, q);
        let mut scores: BTreeMap<MemoryId, f64> = BTreeMap::new();
        let overlay: BTreeSet<_> = q.overlay_sources.iter().map(|s| s.id.as_str()).collect();
        for item in docs.iter().filter(|d| {
            d.owner == q.owner
                && d.task
                    .as_deref()
                    .is_none_or(|t| Some(t) == q.task_id.as_ref().map(|t| t.0.as_str()))
        }) {
            let source = item.source_id.as_ref().unwrap();
            if !c.sources.iter().any(|s| &s.record.id == source)
                || c.delete_sources.contains(source)
            {
                if !overlay.contains(source.as_str()) {
                    continue;
                }
            }
            let matching:Vec<_>=c.records.iter().filter(|r|eligible.contains(&r.memory.id)&&matches!(&r.contract.source,SourceRef::Stored{source_id}if source_id==source)).collect();
            if matching.is_empty() {
                continue;
            }
            let vector = vectors.get(&item.key).ok_or("embedding_document_missing")?;
            let cosine = query
                .iter()
                .zip(vector)
                .map(|(a, b)| f64::from(*a) * f64::from(*b))
                .sum::<f64>();
            for r in matching {
                let body = c
                    .sources
                    .iter()
                    .find(|s| &s.record.id == source)
                    .unwrap()
                    .record
                    .request
                    .as_ref()
                    .unwrap()["text"]
                    .as_str()
                    .unwrap();
                let quote = locate_quote(body, &r.contract.candidate.sentence.text)
                    .ok_or("embedding_record_quote")?;
                if item.start < quote.end && quote.start < item.end {
                    scores
                        .entry(r.memory.id)
                        .and_modify(|score| *score = score.max(cosine))
                        .or_insert(cosine);
                }
            }
        }
        let mut ranked: Vec<_> = scores.into_iter().collect();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        assert!(ranked.iter().all(|(id, _)| eligible.contains(id)));
        Ok(ranked)
    }
    fn lexical_ranking(baseline: &Json, labels: &BTreeMap<&str, MemoryId>) -> Vec<MemoryId> {
        let diagnostic = &baseline["diagnostic"];
        let hits: BTreeSet<_> = diagnostic["ranking"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|rank| {
                let id = rank["id"].as_str().unwrap();
                rank["entity_hits"].as_u64().unwrap() > 0
                    || rank["phrase_hits"].as_u64().unwrap() > 0
                    || diagnostic["selection"][id]
                        .as_array()
                        .is_some_and(|reasons| reasons.iter().any(|reason| reason == "fts"))
            })
            .map(|rank| rank["id"].as_str().unwrap())
            .collect();
        // Relation/closure membership alone does not cast a lexical RRF vote.
        diagnostic["stages"]["ranked"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|id| hits.contains(id.as_str().unwrap()))
            .map(|id| labels[id.as_str().unwrap()])
            .collect()
    }
    fn query_hint(c: &Corpus, q: &Query) -> String {
        let (request, plan) = request_and_plan(c, q);
        let mut hint = std::iter::once(request.binding.query.as_str())
            .chain(request.binding.context.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ");
        if let Some(output) = &plan.output {
            hint.push(' ');
            hint.push_str(&output.relation);
            for condition in &output.conditions {
                hint.push(' ');
                hint.push_str(&condition.quote.text);
            }
        }
        hint
    }
    fn df_documents<'a>(c: &'a Corpus, q: &'a Query) -> BTreeMap<&'a str, String> {
        // R1 subject/relation are retrieval hints, not the host's trusted scope.
        let eligible = scoped_records(c, q, None);
        let sources: BTreeSet<_> = c
            .records
            .iter()
            .filter(|r| eligible.contains(&r.memory.id))
            .map(|r| match &r.contract.source {
                SourceRef::Stored { source_id } => source_id.as_str(),
                _ => unreachable!(),
            })
            .collect();
        c.sources
            .iter()
            .filter(|s| s.owner == q.owner && sources.contains(s.record.id.as_str()))
            .map(|s| {
                (
                    s.record.id.as_str(),
                    s.record.request.as_ref().unwrap()["text"]
                        .as_str()
                        .unwrap()
                        .to_owned(),
                )
            })
            .collect()
    }
    fn record_text(r: &FrozenRecord) -> String {
        std::iter::once(serde_json::to_string(&r.memory.value).unwrap())
            .chain(r.memory.evidence.iter().map(|e| e.quote.clone()))
            .collect::<Vec<_>>()
            .join(" ")
    }
    fn lexical_df_ranking(
        c: &Corpus,
        q: &Query,
        baseline: &Json,
    ) -> (Vec<MemoryId>, Vec<MemoryId>, Json) {
        let documents = df_documents(c, q);
        let entities: Vec<_> = c
            .entities
            .iter()
            .filter(|e| e.owner_id.0 == q.owner)
            .cloned()
            .collect();
        let df = LexicalDf::new(
            &query_hint(c, q),
            &entities,
            documents.values().map(String::as_str),
        );
        let candidates: BTreeSet<_> = baseline["diagnostic"]["stages"]["relation_filter"]
            .as_array()
            .unwrap()
            .iter()
            .map(|id| id.as_str().unwrap())
            .collect();
        let mut scored = Vec::new();
        for r in c
            .records
            .iter()
            .filter(|r| candidates.contains(r.label.as_str()))
        {
            let (entity, phrase) = df.score(&record_text(r));
            let SourceRef::Stored { source_id } = &r.contract.source else {
                unreachable!()
            };
            let fts = baseline["diagnostic"]["selection"][&r.label]
                .as_array()
                .is_some_and(|reasons| reasons.iter().any(|reason| reason == "fts"))
                && documents
                    .get(source_id.as_str())
                    .is_some_and(|text| df.matches(text));
            if entity > 0
                || phrase > 0
                || fts
                || baseline["diagnostic"]["effective_relation"].is_string()
            {
                let sequence = c
                    .sources
                    .iter()
                    .find(|s| s.owner == q.owner && s.record.id == *source_id)
                    .unwrap()
                    .record
                    .received_sequence;
                scored.push((r.memory.id, r.label.as_str(), entity, phrase, sequence, fts));
            }
        }
        scored.sort_by(|a, b| {
            (b.2, b.3, b.4)
                .cmp(&(a.2, a.3, a.4))
                .then_with(|| a.0.cmp(&b.0))
        });
        let diagnostic = json!({"df_percent":LEXICAL_DF_PERCENT,"df_documents":documents.len(),
            "df_scope":"live unique origin sources in owner/task/host-trusted-subject scope, before R1 subject/relation hints",
            "df_source_ids":documents.keys().collect::<Vec<_>>(),
            "terms":df.terms,
            "ranking":scored.iter().map(|r|json!({"id":r.1,"entity_hits":r.2,"phrase_hits":r.3,"received_sequence":r.4,"fts_surviving_term":r.5,"lexical_vote":r.2>0||r.3>0||r.5})).collect::<Vec<_>>()});
        let lexical = scored
            .iter()
            .filter(|r| r.2 > 0 || r.3 > 0 || r.5)
            .map(|r| r.0)
            .collect();
        (
            lexical,
            scored.into_iter().map(|r| r.0).collect(),
            diagnostic,
        )
    }
    fn rrf(lexical: &[MemoryId], vector: &[(MemoryId, f64)], limit: usize) -> Vec<(MemoryId, f64)> {
        let mut scores: BTreeMap<MemoryId, f64> = BTreeMap::new();
        for ids in [
            lexical.iter().copied().take(limit).collect::<Vec<_>>(),
            vector.iter().take(limit).map(|v| v.0).collect(),
        ] {
            let mut seen = BTreeSet::new();
            let mut rank = 0;
            for id in ids {
                if seen.insert(id) {
                    rank += 1;
                    *scores.entry(id).or_default() += 1.0 / (60.0 + rank as f64);
                }
            }
        }
        let mut ranked: Vec<_> = scores.into_iter().collect();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        ranked
    }
    fn simulate(
        c: &Corpus,
        q: &Query,
        baseline: &Json,
        ranked: &[(MemoryId, f64)],
        views: &BTreeMap<MemoryId, RecordView>,
        mode: &str,
        model: Model,
    ) -> Json {
        let budget = baseline["records_budget"].as_u64().unwrap() as usize;
        let labels: BTreeMap<_, _> = c
            .records
            .iter()
            .map(|r| (r.memory.id, r.label.as_str()))
            .collect();
        let eligible = allowed(c, q);
        let mut selected: BTreeSet<_> = ranked.iter().map(|r| r.0).collect();
        assert!(selected.is_subset(&eligible));
        loop {
            let before = selected.len();
            let parents: Vec<_> = selected
                .iter()
                .flat_map(|id| match &views[id] {
                    RecordView::Current(v) => v.history.clone(),
                    _ => unreachable!(),
                })
                .collect();
            selected.extend(parents.into_iter().filter(|id| eligible.contains(id)));
            for relation in &c.relations {
                if relation.members.iter().any(|id| selected.contains(id))
                    && relation.members.iter().all(|id| eligible.contains(id))
                {
                    selected.extend(relation.members.iter().copied());
                }
            }
            if selected.len() == before {
                break;
            }
        }
        let scores: BTreeMap<_, _> = ranked.iter().copied().collect();
        let mut ordered: Vec<_> = selected.iter().copied().collect();
        ordered.sort_by(|a, b| {
            scores
                .get(b)
                .copied()
                .unwrap_or(f64::NEG_INFINITY)
                .total_cmp(&scores.get(a).copied().unwrap_or(f64::NEG_INFINITY))
                .then_with(|| a.cmp(b))
        });
        let mut truncated = ordered.len() > budget;
        ordered.truncate(budget);
        let mut remaining = q.stored_bytes;
        ordered.retain(|id| {
            let bytes = serde_json::to_vec(&views[id]).unwrap().len();
            if bytes <= remaining {
                remaining -= bytes;
                true
            } else {
                truncated = true;
                false
            }
        });
        let returned: Vec<_> = ordered.iter().map(|id| labels[id].to_owned()).collect();
        let mut original_sources: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for id in &ordered {
            let RecordView::Current(v) = &views[id] else {
                unreachable!()
            };
            for e in &v.evidence {
                let SourceRef::Stored { source_id } = &e.source else {
                    unreachable!()
                };
                original_sources
                    .entry(source_id.clone())
                    .or_default()
                    .push(e.quote.text.clone());
            }
        }
        let mut raw_sources = Vec::new();
        for source in &q.overlay_sources {
            if q.subject_ids.is_some() {
                continue;
            }
            let body = source.request.as_ref().unwrap()["text"].as_str().unwrap();
            let raw = RawEvidence {
                source_id: source.id.clone(),
                quote: Quote { text: body.into() },
                said_on: Some(c.said_on.clone()),
                sequence: source.received_sequence,
                grade: RecallGrade::Uncommitted,
                enrichment: EnrichmentStatus::Queued,
                memory_requests: vec![],
            };
            let bytes = serde_json::to_vec(&raw).unwrap().len();
            if bytes <= remaining {
                remaining -= bytes;
                raw_sources.push(source.id.clone());
                original_sources
                    .entry(source.id.clone())
                    .or_default()
                    .push(body.into());
            } else {
                truncated = true;
            }
        }
        let mut relations = Vec::new();
        for relation in &c.relations {
            if !relation.members.iter().any(|id| selected.contains(id))
                || !relation.members.iter().all(|id| eligible.contains(id))
            {
                continue;
            }
            let evidence = |e: &RelationEvidence| EvidenceRef {
                source: SourceRef::Stored {
                    source_id: e.source_id.clone(),
                },
                quote: Quote {
                    text: e.quote.clone(),
                },
            };
            let link = RelationLink {
                id: relation.id.clone(),
                kind: hippocampus::contract::RelationKind::Conflict,
                members: relation.members.clone(),
                conditions: serde_json::from_value(relation.conditions.clone()).unwrap(),
                times: serde_json::from_value(relation.times.clone()).unwrap(),
                basis: relation.basis.iter().map(evidence).collect(),
                status: match relation.status {
                    hippocampus::RelationStatus::Open => {
                        hippocampus::contract::RelationStatus::Open
                    }
                    hippocampus::RelationStatus::Resolved => {
                        hippocampus::contract::RelationStatus::Resolved
                    }
                    hippocampus::RelationStatus::Invalidated => {
                        hippocampus::contract::RelationStatus::Invalidated
                    }
                },
                resolution: relation.resolution.as_ref().map(evidence),
            };
            let bytes = serde_json::to_vec(&link).unwrap().len();
            if bytes <= remaining {
                remaining -= bytes;
                relations.push(relation.id.clone());
                for e in &relation.basis {
                    original_sources
                        .entry(e.source_id.clone())
                        .or_default()
                        .push(e.quote.clone());
                }
            } else {
                truncated = true;
            }
        }
        for forbidden in &q.forbidden_records {
            assert!(!returned.contains(forbidden));
        }
        for forbidden in &q.forbidden_sources {
            assert!(!original_sources.contains_key(forbidden));
        }
        for source in &q.required_raw {
            assert!(raw_sources.contains(source));
        }
        assert!(remaining <= q.stored_bytes && returned.len() <= budget);
        let required: BTreeSet<_> = q.required_records.iter().collect();
        let relevant: BTreeSet<_> = q.relevant_records.iter().collect();
        let missing: Vec<_> = required
            .iter()
            .filter(|id| !returned.contains(id))
            .map(|id| json!({"id":id,"stage": if !eligible.contains(&c.records.iter().find(|r| &r.label == *id).unwrap().memory.id) {"trusted_or_r1_scope"} else if !selected.contains(&c.records.iter().find(|r| &r.label == *id).unwrap().memory.id) {if mode=="lexical-df" {"lexical_df_seed"} else {"vector_or_rrf_seed"}} else {"count_or_byte_budget"}}))
            .collect();
        let required_sources: BTreeSet<_> = c
            .records
            .iter()
            .filter(|r| required.contains(&r.label))
            .map(|r| match &r.contract.source {
                SourceRef::Stored { source_id } => source_id.clone(),
                _ => unreachable!(),
            })
            .chain(q.required_raw.iter().cloned())
            .collect();
        let covered_sources: Vec<_> = required_sources
            .iter()
            .filter(|id| {
                let source = c
                    .sources
                    .iter()
                    .find(|s| &s.record.id == *id)
                    .map(|s| &s.record)
                    .or_else(|| q.overlay_sources.iter().find(|s| &s.id == *id))
                    .unwrap();
                let body = source.request.as_ref().unwrap()["text"].as_str().unwrap();
                original_sources.get(*id).is_some_and(|quotes| {
                    quotes
                        .iter()
                        .any(|quote| locate_quote(quote, body).is_some())
                })
            })
            .cloned()
            .collect();
        json!({"id":q.id,"query":q.query,"category":q.category,"control":q.control,"insertion_order":"canonical","model":model.tag(),"mode":mode,"records_budget":budget,
            "required_all":missing.is_empty(),"recall":(!required.is_empty()).then(||(required.len()-missing.len())as f64/required.len()as f64),
            "mrr":(!required.is_empty()).then(||returned.iter().position(|id|required.contains(id)).map_or(0.0,|i|1.0/(i+1)as f64)),
            "precision":(!returned.is_empty()).then(||returned.iter().filter(|id|relevant.contains(id)).count()as f64/returned.len()as f64),
            "returned_count":returned.len(),"returned":returned,"missing":missing,"raw_sources":raw_sources,"relations":relations,
            "r3_original_coverage":(!required_sources.is_empty()).then(||covered_sources.len()as f64/required_sources.len()as f64),"required_sources":required_sources,"covered_sources":covered_sources,"stored_bytes_used":q.stored_bytes-remaining,"stored_bytes_budget":q.stored_bytes,
            "completeness":{"retrieval_truncated":truncated,"plan_complete":false,"pending_related_sources":baseline["completeness"]["pending_related_sources"],"pending_updates":baseline["completeness"]["pending_updates"],"enrichment_gaps":baseline["completeness"]["enrichment_gaps"]},
            "simulator":true,"diagnostic":{"stages":{"trusted_r1_scope":eligible.iter().map(|id|labels[id]).collect::<Vec<_>>(),"seeds":ranked.iter().map(|(id,_)|labels[id]).collect::<Vec<_>>(),"closure":selected.iter().map(|id|labels[id]).collect::<Vec<_>>(),"returned":returned},"fts_called":false},"seed_ranking":ranked.iter().map(|(id,score)|json!({"id":labels[id],"score":score})).collect::<Vec<_>>()})
    }
    fn experiment(c: &Corpus, queries: &Queries, record: bool, base: &str) -> Result<Json, String> {
        let baseline_path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("target/eval-retrieval/af173/report.json");
        let baseline: Json = serde_json::from_slice(
            &std::fs::read(baseline_path)
                .map_err(|_| "AF173 report missing; run baseline first")?,
        )
        .map_err(|e| e.to_string())?;
        for (field, text) in [
            (
                "corpus_fingerprint",
                include_str!("eval/retrieval/corpus.json"),
            ),
            (
                "queries_fingerprint",
                include_str!("eval/retrieval/queries.json"),
            ),
        ] {
            if baseline[field] != fp(&serde_json::from_str::<Json>(text).unwrap()) {
                return Err("AF173 report input binding missing/stale; regenerate baseline without model calls".into());
            }
        }
        let db = Db::seed(c, "canonical");
        let state = db.open("A").read().unwrap();
        let views: BTreeMap<_, _> = c
            .records
            .iter()
            .filter(|r| r.memory.owner_id.0 == "A")
            .filter_map(|r| view(&state, r.memory.id).map(|v| (r.memory.id, v)))
            .collect();
        let docs = document_items(c, queries);
        let labels: BTreeMap<_, _> = c
            .records
            .iter()
            .map(|r| (r.label.as_str(), r.memory.id))
            .collect();
        let mut reports = Vec::new();
        for model in [Model::Qwen, Model::Bge] {
            let loaded = model_vectors(
                c,
                queries,
                model,
                record,
                base,
                &root().join("recordings").join(file_id(model)),
            );
            let mut rows = Vec::new();
            let (binding, vectors, statuses, usage) = match loaded {
                Ok(v) => v,
                Err(error) => {
                    reports.push(
                        json!({"model":model.tag(),"status":"unavailable","error":error,"rows":[]}),
                    );
                    continue;
                }
            };
            let complete = jobs(c, queries, model)
                .iter()
                .filter(|j| j.kind != "query_warm")
                .all(|j| j.items.iter().all(|i| vectors.contains_key(&i.key)));
            for q in &queries.queries {
                let vector_start = Instant::now();
                let vector = if complete {
                    vector_ranking(c, q, &docs, &vectors)?
                } else {
                    vec![]
                };
                let vector_us = vector_start.elapsed().as_micros();
                let warm_start = Instant::now();
                if complete {
                    assert_eq!(vector, vector_ranking(c, q, &docs, &vectors)?);
                }
                let warm_us = warm_start.elapsed().as_micros();
                for budget in &queries.budgets {
                    let original = baseline["rows"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|r| {
                            r["id"] == q.id
                                && r["records_budget"] == *budget
                                && r["insertion_order"] == "canonical"
                        })
                        .ok_or("AF173 row missing")?;
                    let expected: BTreeSet<_> = original["diagnostic"]["stages"]["r1_subject"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|id| labels[id.as_str().unwrap()])
                        .collect();
                    assert_eq!(expected, allowed(c, q));
                    let mut current = original.clone();
                    current["mode"] = json!("current");
                    current["model"] = json!(model.tag());
                    rows.push(current);
                    let (lexical_df, current_df, df_diagnostic) =
                        lexical_df_ranking(c, q, original);
                    for mode in ["lexical-df", "vector-only", "hybrid", "hybrid-df"] {
                        if mode != "lexical-df" && !complete {
                            rows.push(json!({"id":q.id,"category":q.category,"control":q.control,"model":model.tag(),"mode":mode,"records_budget":budget,"status":"not_evaluated_vector_missing"}));
                            continue;
                        }
                        let mut result = if !original["diagnostic"]["enumeration"].is_null() {
                            let mut row = original.clone();
                            row[if mode == "lexical-df" {
                                "lexical_df_bypass"
                            } else {
                                "vector_bypass"
                            }] = original["diagnostic"]["enumeration"].clone();
                            row
                        } else {
                            let lexical = if mode == "hybrid-df" || mode == "lexical-df" {
                                lexical_df.clone()
                            } else {
                                lexical_ranking(original, &labels)
                            };
                            let ranked = match mode {
                                "hybrid" | "hybrid-df" => rrf(&lexical, &vector, 2 * budget),
                                "lexical-df" => current_df
                                    .iter()
                                    .enumerate()
                                    .map(|(i, id)| (*id, (current_df.len() - i) as f64))
                                    .collect(),
                                _ => vector.iter().take(2 * budget).copied().collect(),
                            };
                            let mut simulated =
                                simulate(c, q, original, &ranked, &views, mode, model);
                            if mode != "vector-only" {
                                let names: BTreeMap<_, _> =
                                    labels.iter().map(|(name, id)| (*id, *name)).collect();
                                simulated["diagnostic"]["lexical_replayed"] = json!(true);
                                simulated["diagnostic"]["lexical_fts_called_in_baseline"] =
                                    original["diagnostic"]["fts_called"].clone();
                                simulated["diagnostic"]["lexical_ranked_before_filter"] =
                                    original["diagnostic"]["stages"]["ranked"].clone();
                                simulated["diagnostic"]["lexical_ranked_after_filter"] =
                                    json!(lexical.iter().map(|id| names[id]).collect::<Vec<_>>());
                            }
                            simulated
                        };
                        result["mode"] = json!(mode);
                        result["model"] = json!(model.tag());
                        result["simulator"] = json!(true);
                        if mode == "lexical-df" || mode == "hybrid-df" {
                            result["diagnostic"]["lexical_df"] = df_diagnostic.clone();
                        }
                        if mode != "lexical-df" {
                            result["vector_scan_first_us"] = json!(vector_us);
                            result["vector_scan_warm_us"] = json!(warm_us);
                        }
                        rows.push(result);
                    }
                }
            }
            let summaries: Vec<_> = MODES
                .into_iter()
                .map(|mode| {
                    let eligible: Vec<_> = rows
                        .iter()
                        .filter(|r| r["mode"] == mode && r.get("recall").is_some())
                        .cloned()
                        .collect();
                    json!({"mode":mode,"categories":summary(&eligible)})
                })
                .collect();
            let totals: Vec<_> = MODES.into_iter().flat_map(|mode| {
                queries.budgets.iter().map(|budget| {
                    let group: Vec<_> = rows.iter().filter(|r|r["mode"]==mode && r["records_budget"]==*budget && r["control"]==false && r.get("recall").is_some()).collect();
                    let applicable: Vec<_> = group.iter().filter(|r|r["recall"].is_number()).collect();
                    let average = |field: &str| {
                        let values: Vec<_> = group.iter().filter_map(|r|r[field].as_f64()).collect();
                        (!values.is_empty()).then(||values.iter().sum::<f64>()/values.len() as f64)
                    };
                    json!({"mode":mode,"records_budget":budget,"queries":group.len(),"applicable":applicable.len(),"required_all":applicable.iter().filter(|r|r["required_all"]==true).count(),"mrr_macro":average("mrr"),"precision_macro":average("precision"),"precision_applicable":group.iter().filter(|r|r["precision"].is_number()).count()})
                }).collect::<Vec<_>>()
            }).collect();
            reports.push(json!({"model":model.tag(),"binding":binding,"status":if complete{"complete"}else{"vector_missing"},"quota":jobs(c,queries,model).len(),"charged_slots":statuses.iter().filter(|s|s["status"]!="missing").count(),"recording_complete":statuses.iter().all(|s|s["status"]=="reused"||s["status"]=="recorded"),"slots":statuses,"usage":usage,"summary":summaries,"totals":totals,"rows":rows,
                "vector_payload_bytes":vectors.values().map(|v|v.len()*std::mem::size_of::<f32>()).sum::<usize>(),"ollama_peak_memory":null,
                "query_first_warm_probe":usage.iter().filter(|u|u["kind"]=="query_first"||u["kind"]=="query_warm").collect::<Vec<_>>()}));
        }
        Ok(
            json!({"campaign":"AF175","simulation_only":true,"rrf_k":60,"seed_factor":2,"lexical_df_percent":LEXICAL_DF_PERCENT,"threshold_tuned_on_acceptance_set":false,"safety_violations":0,"models":reports}),
        )
    }
    #[test]
    fn af175_local_embedding_experiment() {
        let (c, queries) = data();
        let mode = std::env::var("RETRIEVAL_EMBED_MODE").unwrap_or_else(|_| "dry-run".into());
        let base = std::env::var("RETRIEVAL_EMBED_BASE_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:11434".into());
        embedding::endpoint(&base).unwrap();
        match mode.as_str() {
            "dry-run" => println!(
                "{}",
                serde_json::to_string_pretty(&plan(&c, &queries)).unwrap()
            ),
            "replay" | "replay-af177" | "replay-af178" | "record" => {
                if mode == "record" {
                    assert_eq!(
                        std::env::var("RETRIEVAL_EMBED_LIMIT").ok().as_deref(),
                        Some("28"),
                        "record requires explicit full campaign quota RETRIEVAL_EMBED_LIMIT=28"
                    );
                }
                let mut report = experiment(&c, &queries, mode == "record", &base).unwrap();
                report["evaluation_revision"] = json!("AF178");
                if mode != "record" {
                    report["new_embedding_requests"] = json!(0);
                }
                if mode.starts_with("replay") {
                    assert!(
                        report["models"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .all(|model| model["status"] == "complete"
                                && model["recording_complete"] == true
                                && model["slots"].as_array().unwrap().len() == 14
                                && model["slots"]
                                    .as_array()
                                    .unwrap()
                                    .iter()
                                    .all(|slot| slot["status"] == "reused")),
                        "requires strict full recording replay; details: {}",
                        json!(report["models"].as_array().unwrap().iter().map(|model|json!({"model":model["model"],"status":model["status"],"error":model["error"]})).collect::<Vec<_>>())
                    );
                }
                let dir = root().join(match mode.as_str() {
                    "record" => "reports-record",
                    "replay-af177" => "reports-af177",
                    "replay-af178" => "reports-af178",
                    _ => "reports-af178",
                });
                std::fs::create_dir_all(&dir).unwrap();
                atomic_write(&dir.join("report.json"), &report).unwrap();
                let mut text = String::from(
                    "# 本地嵌入离线模拟（AF178）\n\n不是生产 R2，不认证语义/完整性。RRF(60)，两路各2×records种子。DF 严格超过20%不计分；未用验收集调参。独立来源去重，范围为 owner/任务/宿主可信主体，在 R1 主体/关系提示之前；词元为现有中文相邻字对、字母数字词与登记实体别名。零新嵌入请求（回放模式）。冷探针仅是首次请求，不保证模型未驻留；峰值内存未测。\n\n",
                );
                for model in report["models"].as_array().unwrap() {
                    text.push_str(&format!(
                        "## {}\n\nstatus={}\n\n",
                        model["model"], model["status"]
                    ));
                    text.push_str(&format!("首次/暖请求（实际录制）：{}\n\n向量载荷字节：{}；Ollama 峰值内存：未测。\n\n", model["query_first_warm_probe"], model["vector_payload_bytes"]));
                    text.push_str("全覆盖/MRR 按32题非空必需集计算；precision 按34题非控制题中的非空值宏平均（空结果不算1分，coverage/MRR仍记失分）。hybrid 为 AF177 旧规则；DF 模式重计实体/短语命中后按命中数及接收顺序排序。lexical-df 保持现状的关系种子；hybrid-df 的零命中记录不投词法票。小范围/聚合/失败枚举保持现状。\n\n| 模式 | records | 全覆盖 | MRR | precision | precision 分母 |\n|---|---|---|---|---|---|\n");
                    for t in model["totals"].as_array().into_iter().flatten() {
                        text.push_str(&format!(
                            "| {} | {} | {}/{} | {} | {} | {} |\n",
                            t["mode"],
                            t["records_budget"],
                            t["required_all"],
                            t["applicable"],
                            t["mrr_macro"],
                            t["precision_macro"],
                            t["precision_applicable"]
                        ));
                    }
                    text.push('\n');
                    for s in model["summary"].as_array().into_iter().flatten() {
                        text.push_str(&format!("{}: {}\n\n", s["mode"], s["categories"]));
                    }
                    text.push_str("| 题 | 模式 | records | 全覆盖 | recall | MRR | precision | 返回 | 原文覆盖 |\n|---|---|---|---|---|---|---|---|---|\n");
                    for r in model["rows"].as_array().into_iter().flatten() {
                        text.push_str(&format!(
                            "| {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
                            r["id"],
                            r["mode"],
                            r["records_budget"],
                            r["required_all"],
                            r["recall"],
                            r["mrr"],
                            r["precision"],
                            r["returned_count"],
                            r["r3_original_coverage"]
                        ));
                    }
                }
                std::fs::write(dir.join("report.md"), text).unwrap();
                println!("AF175 {}: {}", mode, dir.display());
            }
            _ => panic!("unknown RETRIEVAL_EMBED_MODE"),
        }
    }
    #[test]
    fn af175_plan_scope_quota_rrf_and_strict_recordings_without_network() {
        let (c, queries) = data();
        let report = plan(&c, &queries);
        assert_eq!(report["total_embedding_requests"], 28);
        assert_eq!(report["sent"], 0);
        let docs = document_items(&c, &queries);
        assert_eq!(docs.len(), 137);
        assert!(docs.iter().all(|d| d.owner == "A"
            && d.source_id.as_deref() != Some("s-T28")
            && d.source_id.as_deref() != Some("s-T30")));
        for q in &queries.queries {
            let scope = allowed(&c, q);
            assert!(!scope.contains(&MemoryId(28)) && !scope.contains(&MemoryId(30)));
            if q.task_id.is_none() {
                assert!(!scope.contains(&MemoryId(29)));
            }
            if q.id == "C02" {
                assert!(scope.is_empty());
            }
        }
        let ranked = rrf(
            &[MemoryId(1), MemoryId(2)],
            &[(MemoryId(2), 0.1), (MemoryId(3), 0.0)],
            2,
        );
        assert_eq!(ranked[0].0, MemoryId(2));
        assert_eq!(ranked.len(), 3);
        let binding = Binding::from_tags(
            Model::Qwen,
            &json!({"models":[{"name":Model::Qwen.tag(),"digest":"a".repeat(64)}]}),
        )
        .unwrap();
        let job = jobs(&c, &queries, Model::Qwen).remove(0);
        let mut vector = vec![0.0; embedding::DIMENSIONS];
        vector[0] = 1.0;
        let result: Result<Batch, String> = Ok(Batch {
            vectors: vec![vector],
            input_tokens: Some(3),
            total_duration_ns: None,
            load_duration_ns: None,
            elapsed_ms: 1.0,
        });
        let recording = Recording {
            fingerprint: job_fp(&binding, &job),
            result_hash: fp(&serde_json::to_value(&result).unwrap()),
            result,
        };
        assert!(
            recording
                .checked(&job_fp(&binding, &job), 1)
                .unwrap()
                .is_some()
        );
        let mut changed = binding.clone();
        changed.digest = "b".repeat(64);
        assert!(recording.checked(&job_fp(&changed, &job), 1).is_err());
        let directory = std::env::temp_dir().join(format!("af175-test-{}", diagnostics::new_id()));
        std::fs::create_dir_all(&directory).unwrap();
        let mut campaign = Campaign {
            binding: binding.clone(),
            fingerprint: "test".into(),
            quota: 1,
            sent: BTreeMap::from([(job.key.clone(), job_fp(&binding, &job))]),
        };
        let mut statuses = Vec::new();
        assert!(
            cached_or_send(&directory, &mut campaign, &job, None, &mut statuses)
                .unwrap()
                .is_none()
        );
        assert_eq!(statuses[0]["status"], "unrecorded_inflight");
        assert_eq!(campaign.sent.len(), 1);
        std::fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn af175_fake_replay_filters_deleted_owner_task_subject_and_applies_budgets() {
        let (c, queries) = data();
        let directory =
            std::env::temp_dir().join(format!("af175-replay-{}", diagnostics::new_id()));
        std::fs::create_dir_all(&directory).unwrap();
        let binding = Binding::from_tags(
            Model::Qwen,
            &json!({"models":[{"name":Model::Qwen.tag(),"digest":"a".repeat(64)}]}),
        )
        .unwrap();
        let jobs = jobs(&c, &queries, Model::Qwen);
        let mut campaign = Campaign {
            binding: binding.clone(),
            fingerprint: fp(&json!({"binding":binding,"jobs":jobs})),
            quota: jobs.len(),
            sent: BTreeMap::new(),
        };
        for job in &jobs {
            let vectors = job
                .items
                .iter()
                .map(|item| {
                    let mut vector = vec![0.0; embedding::DIMENSIONS];
                    vector
                        [usize::from(item.source_id.as_deref().is_some_and(|id| id != "s-T01"))] =
                        1.0;
                    vector
                })
                .collect();
            let result: Result<Batch, String> = Ok(Batch {
                vectors,
                input_tokens: Some(1),
                total_duration_ns: Some(2),
                load_duration_ns: Some(0),
                elapsed_ms: 1.0,
            });
            let fingerprint = job_fp(&binding, job);
            let recording = Recording {
                fingerprint: fingerprint.clone(),
                result_hash: fp(&serde_json::to_value(&result).unwrap()),
                result,
            };
            campaign.sent.insert(job.key.clone(), fingerprint);
            atomic_write(
                &directory.join(format!("{}.json", job.key)),
                &serde_json::to_value(recording).unwrap(),
            )
            .unwrap();
        }
        atomic_write(
            &directory.join("campaign.json"),
            &serde_json::to_value(&campaign).unwrap(),
        )
        .unwrap();
        let (_, mut vectors, statuses, usage) = model_vectors(
            &c,
            &queries,
            Model::Qwen,
            false,
            "http://127.0.0.1:1",
            &directory,
        )
        .unwrap();
        assert_eq!(statuses.len(), 14);
        assert!(statuses.iter().all(|s| s["status"] == "reused"));
        assert_eq!(usage.len(), 14);
        let mut docs = document_items(&c, &queries);
        // Even an accidentally retained deleted/foreign segment cannot reach candidates.
        for id in ["s-T28", "s-T30"] {
            let source = c.sources.iter().find(|s| s.record.id == id).unwrap();
            let text = source.record.request.as_ref().unwrap()["text"]
                .as_str()
                .unwrap();
            docs.push(Item {
                key: format!("forbidden-{id}"),
                text: text.into(),
                source_id: Some(id.into()),
                owner: source.owner.clone(),
                task: None,
                start: 0,
                end: text.len(),
            });
        }
        let mut duplicate = docs
            .iter()
            .find(|d| d.source_id.as_deref() == Some("s-T01"))
            .unwrap()
            .clone();
        duplicate.key.push_str(":overlap");
        let mut weaker = vec![0.0; embedding::DIMENSIONS];
        weaker[1] = 1.0;
        vectors.insert(duplicate.key.clone(), weaker);
        docs.push(duplicate);
        for q in &queries.queries {
            let ranking = vector_ranking(&c, q, &docs, &vectors).unwrap();
            assert!(ranking.iter().all(|r| allowed(&c, q).contains(&r.0)));
            assert_eq!(
                ranking.len(),
                ranking.iter().map(|r| r.0).collect::<BTreeSet<_>>().len()
            );
            assert!(
                !ranking
                    .iter()
                    .any(|r| [MemoryId(28), MemoryId(30)].contains(&r.0))
            );
            if q.task_id.is_none() {
                assert!(!ranking.iter().any(|r| r.0 == MemoryId(29)));
            }
            if q.id == "C02" {
                assert!(ranking.is_empty());
            }
        }
        let q = queries.queries.iter().find(|q| q.id == "Q31").unwrap();
        let ranked = vector_ranking(&c, q, &docs, &vectors).unwrap();
        assert_eq!(ranked[0], (MemoryId(1), 1.0)); // best segment, wrong relation hint is not a permission gate
        let db = Db::seed(&c, "canonical");
        let state = db.open("A").read().unwrap();
        let views: BTreeMap<_, _> = c
            .records
            .iter()
            .filter(|r| r.memory.owner_id.0 == "A")
            .filter_map(|r| view(&state, r.memory.id).map(|v| (r.memory.id, v)))
            .collect();
        let baseline = json!({"records_budget":8,"completeness":{}});
        let vector = simulate(
            &c,
            q,
            &baseline,
            &ranked[..16],
            &views,
            "vector-only",
            Model::Qwen,
        );
        assert_eq!(vector["required_all"], true);
        assert!(vector["returned_count"].as_u64().unwrap() <= 8);
        assert!(vector["stored_bytes_used"].as_u64().unwrap() <= q.stored_bytes as u64);
        let lexical = vec![MemoryId(137)];
        let fused = rrf(&lexical, &ranked, 16);
        let hybrid = simulate(&c, q, &baseline, &fused, &views, "hybrid", Model::Qwen);
        assert_eq!(hybrid["required_all"], true);
        let raw = queries.queries.iter().find(|q| q.id == "C01").unwrap();
        let ranking = vector_ranking(&c, raw, &docs, &vectors).unwrap();
        assert_eq!(
            simulate(
                &c,
                raw,
                &baseline,
                &ranking[..16],
                &views,
                "vector-only",
                Model::Qwen
            )["raw_sources"],
            json!(["s-raw-C01"])
        );
        let cropped = queries.queries.iter().find(|q| q.id == "C06").unwrap();
        let ranking = vector_ranking(&c, cropped, &docs, &vectors).unwrap();
        let result = simulate(
            &c,
            cropped,
            &baseline,
            &ranking[..16],
            &views,
            "vector-only",
            Model::Qwen,
        );
        assert_eq!(result["returned_count"], 0);
        assert_eq!(result["stored_bytes_used"], 0);
        assert_eq!(result["completeness"]["retrieval_truncated"], true);
        assert_eq!(result["raw_sources"], json!([]));
        // Full campaign reuse needs no client; missing/stale weights never trigger a resend.
        assert!(
            model_vectors(
                &c,
                &queries,
                Model::Bge,
                false,
                "http://127.0.0.1:1",
                &directory
            )
            .is_err()
        );
        campaign.sent.remove(&jobs[0].key);
        atomic_write(
            &directory.join("campaign.json"),
            &serde_json::to_value(&campaign).unwrap(),
        )
        .unwrap();
        assert!(
            model_vectors(
                &c,
                &queries,
                Model::Qwen,
                false,
                "http://127.0.0.1:1",
                &directory
            )
            .is_err()
        );
        std::fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn af177_no_lexical_hit_hybrid_matches_vector_at_both_budgets() {
        let labels = BTreeMap::from([
            ("T", MemoryId(1)),
            ("F1", MemoryId(2)),
            ("F2", MemoryId(3)),
            ("F3", MemoryId(4)),
        ]);
        let baseline = json!({"diagnostic":{
            "stages":{"ranked":["F3","F2","F1"]},
            "ranking":[{"id":"F3","entity_hits":0,"phrase_hits":0},{"id":"F2","entity_hits":0,"phrase_hits":0},{"id":"F1","entity_hits":0,"phrase_hits":0}],
            "selection":{"F3":["relation"],"F2":["version_chain"],"F1":["relation_member"]}
        }});
        let vector = vec![
            (MemoryId(1), 1.0),
            (MemoryId(2), 0.8),
            (MemoryId(3), 0.7),
            (MemoryId(4), 0.6),
        ];
        for records in [8, 64] {
            let limit = 2 * records;
            let hybrid = rrf(&lexical_ranking(&baseline, &labels), &vector, limit);
            assert_eq!(
                hybrid.iter().map(|r| r.0).collect::<Vec<_>>(),
                vector.iter().take(limit).map(|r| r.0).collect::<Vec<_>>()
            );
        }
    }
    #[test]
    fn af177_lexical_preserves_fts_phrase_entity_hits_and_their_order() {
        let labels = BTreeMap::from([
            ("phrase", MemoryId(1)),
            ("entity", MemoryId(2)),
            ("fts", MemoryId(3)),
            ("relation_only", MemoryId(4)),
            ("history_only", MemoryId(5)),
        ]);
        let baseline = json!({"diagnostic":{
            "stages":{"ranked":["phrase","relation_only","entity","history_only","fts"]},
            "ranking":[{"id":"phrase","entity_hits":0,"phrase_hits":1},{"id":"relation_only","entity_hits":0,"phrase_hits":0},{"id":"entity","entity_hits":1,"phrase_hits":0},{"id":"history_only","entity_hits":0,"phrase_hits":0},{"id":"fts","entity_hits":0,"phrase_hits":0}],
            "selection":{"phrase":["literal"],"relation_only":["relation"],"entity":[],"history_only":["version_chain"],"fts":["fts"]}
        }});
        let lexical = lexical_ranking(&baseline, &labels);
        assert_eq!(lexical, vec![MemoryId(1), MemoryId(2), MemoryId(3)]);
        let vector = vec![(MemoryId(5), 0.99), (MemoryId(1), 0.9)];
        assert_eq!(rrf(&lexical, &vector, 128)[0].0, MemoryId(1));
    }
    #[test]
    fn af178_df_cutoff_is_strict_source_dedup_and_keeps_rare_literal_terms() {
        let docs = [
            "我的 Rust 铜鹤 铜鹤",
            "我的 铜鹤",
            "我的 Java",
            "Python",
            "Ruby",
            "Swift",
            "Go",
            "Zig",
            "Lua",
            "Perl",
        ];
        let mut entity = data().0.entities.remove(0);
        entity.canonical_name = "铜鹤".into();
        entity.aliases.clear();
        let df = LexicalDf::new("我的 RUST 铜鹤", &[entity], docs);
        let frequencies: Vec<_> = df.terms.iter().map(|t| (t.df, t.kept)).collect();
        assert!(frequencies.contains(&(3, false)));
        assert!(frequencies.contains(&(2, true))); // exactly 20%, not greater
        assert!(frequencies.contains(&(1, true)));
        assert!(!LexicalDf::kept(0, 0));
        assert_eq!(df.score(docs[0]), (1, 2));
        assert_eq!(df.score(docs[2]), (0, 0));
        assert_eq!(df.score("Rustacean"), (0, 0));
    }
    #[test]
    fn af178_df_scope_dedup_zero_vote_and_exact_term_ranking() {
        let (mut c, queries) = data();
        for q in &queries.queries {
            let docs = df_documents(&c, q);
            assert!(!docs.contains_key("s-T28") && !docs.contains_key("s-T30"));
            if q.task_id.is_none() {
                assert!(!docs.contains_key("s-T29"));
            }
            let eligible = scoped_records(&c, q, None);
            assert_eq!(
                docs.len(),
                c.records
                    .iter()
                    .filter(|r| eligible.contains(&r.memory.id))
                    .map(|r| match &r.contract.source {
                        SourceRef::Stored { source_id } => source_id,
                        _ => unreachable!(),
                    })
                    .collect::<BTreeSet<_>>()
                    .len()
            );
            if q.id == "C02" {
                assert!(docs.is_empty());
            }
        }
        let q = queries.queries.iter().find(|q| q.id == "Q03").unwrap();
        let other_subject = queries.queries.iter().find(|q| q.id == "Q12").unwrap();
        assert_eq!(df_documents(&c, q), df_documents(&c, other_subject));
        let before = json!(df_documents(&c, q));
        let corpus: Json =
            serde_json::from_str(include_str!("eval/retrieval/corpus.json")).unwrap();
        let mut duplicate: FrozenRecord =
            serde_json::from_value(corpus["records"][0].clone()).unwrap();
        duplicate.memory.id = MemoryId(1000);
        c.records.push(duplicate);
        assert_eq!(json!(df_documents(&c, q)), before);
        c.records.pop();
        let labels: BTreeMap<_, _> = c
            .records
            .iter()
            .map(|r| (r.label.as_str(), r.memory.id))
            .collect();
        let baseline_for = |q: &Query| json!({"diagnostic":{"stages":{"relation_filter":allowed(&c, q).iter().map(|id| c.records.iter().find(|r|r.memory.id==*id).unwrap().label.clone()).collect::<Vec<_>>()},"selection":{}}});
        let (lexical, current, _) = lexical_df_ranking(&c, q, &baseline_for(q));
        assert!(lexical.is_empty());
        assert!(current.is_empty());
        let vector = vec![(MemoryId(3), 0.9), (MemoryId(137), 0.8), (MemoryId(6), 0.7)];
        for budget in [8, 64] {
            assert_eq!(
                rrf(&lexical, &vector, 2 * budget)
                    .iter()
                    .map(|r| r.0)
                    .collect::<Vec<_>>(),
                vector.iter().map(|r| r.0).collect::<Vec<_>>()
            );
        }
        let exact = queries.queries.iter().find(|q| q.id == "Q19").unwrap();
        let (lexical, _, _) = lexical_df_ranking(&c, exact, &baseline_for(exact));
        assert_eq!(lexical[0], labels["T25"]);
        let mut relation_only = baseline_for(q);
        relation_only["diagnostic"]["effective_relation"] = json!("fact");
        let (lexical, current, _) = lexical_df_ranking(&c, q, &relation_only);
        assert!(lexical.is_empty()); // no RRF vote from relation membership alone
        assert!(!current.is_empty()); // current policy still admits relation seeds
    }
    #[test]
    fn af178_real_index_tokenizer_parity_and_strict_float_roundtrip() {
        let (c, _) = data();
        let db = Db::seed(&c, "canonical");
        let sql = rusqlite::Connection::open(&db.0).unwrap();
        let mut statement = sql
            .prepare("SELECT text,index_text FROM source_segments")
            .unwrap();
        for row in statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .unwrap()
        {
            let (text, indexed) = row.unwrap();
            assert_eq!(LexicalDf::index_text(&text), indexed);
        }
        let mut vector = vec![0.0; embedding::DIMENSIONS];
        vector[0] = 1.0;
        let result: Result<Batch, String> = Ok(Batch {
            vectors: vec![vector],
            input_tokens: Some(1),
            total_duration_ns: None,
            load_duration_ns: None,
            elapsed_ms: 397.60083299999997,
        });
        let recording = Recording {
            fingerprint: "strict-input".into(),
            result_hash: fp(&serde_json::to_value(&result).unwrap()),
            result,
        };
        let mut restored: Recording =
            serde_json::from_slice(&serde_json::to_vec(&recording).unwrap()).unwrap();
        assert_eq!(
            restored.result.as_ref().unwrap().elapsed_ms.to_bits(),
            recording.result.as_ref().unwrap().elapsed_ms.to_bits()
        );
        assert!(restored.checked("strict-input", 1).unwrap().is_some());
        assert!(restored.checked("changed-input", 1).is_err());
        let batch = restored.result.as_mut().unwrap();
        batch.elapsed_ms = f64::from_bits(batch.elapsed_ms.to_bits() + 1);
        assert!(restored.checked("strict-input", 1).is_err());
    }
    fn install_vectors(
        db: &Db,
        binding: &Binding,
        docs: &[Item],
        vectors: &BTreeMap<String, Vec<f32>>,
    ) {
        let mut store = db.operation("A");
        let state = store.read().unwrap();
        let token = store
            .set_embedding_binding(embedding_index::Binding::new(binding.clone()).unwrap())
            .unwrap();
        let sources: BTreeSet<_> = docs.iter().filter_map(|d| d.source_id.as_deref()).collect();
        for source in sources {
            if !state.sources.get(source).is_some_and(|s| {
                s.enrichment != "deleted"
                    && s.request
                        .as_ref()
                        .is_some_and(|r| r["capture_source"] == true && r["text"].is_string())
            }) {
                continue;
            }
            let input = store.embedding_source_input(&token, source).unwrap();
            let values: Vec<_> = input
                .segments
                .iter()
                .map(|s| vectors[&format!("doc:{source}:{}:{}", s.start, s.end)].clone())
                .collect();
            store.write_embedding_vectors(&input, &values).unwrap();
        }
    }
    fn totals(rows: &[Json], budget: usize) -> Json {
        let group: Vec<_> = rows
            .iter()
            .filter(|r| r["records_budget"] == budget && r["control"] == false)
            .collect();
        let applicable: Vec<_> = group.iter().filter(|r| r["recall"].is_number()).collect();
        let avg = |field: &str| {
            let v: Vec<_> = group.iter().filter_map(|r| r[field].as_f64()).collect();
            (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
        };
        json!({"records_budget":budget,"required_all":applicable.iter().filter(|r|r["required_all"]==true).count(),
            "applicable":applicable.len(),"mrr":avg("mrr"),"precision":avg("precision")})
    }
    #[test]
    #[ignore = "requires the strictly bound AF175 local vector recordings; sends nothing"]
    fn af185_production_strict_vector_replay() {
        let (c, queries) = data();
        let docs = document_items(&c, &queries);
        let historical: Json = serde_json::from_slice(
            &std::fs::read(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("target/eval-retrieval/af173/report.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            historical["corpus_fingerprint"],
            fp(&serde_json::from_str::<Json>(include_str!("eval/retrieval/corpus.json")).unwrap())
        );
        assert_eq!(
            historical["queries_fingerprint"],
            fp(&serde_json::from_str::<Json>(include_str!("eval/retrieval/queries.json")).unwrap())
        );
        let mut models = Vec::new();
        for model in [Model::Qwen, Model::Bge] {
            let (binding, vectors, slots, _) = model_vectors(
                &c,
                &queries,
                model,
                false,
                "http://127.0.0.1:1",
                &root().join("recordings").join(file_id(model)),
            )
            .unwrap();
            assert!(slots.iter().all(|s| s["status"] == "reused"));
            println!("AF185 {} strict recordings loaded", model.tag());
            let db = Db::seed(&c, "canonical");
            println!("AF185 {} database seeded", model.tag());
            install_vectors(&db, &binding, &docs, &vectors);
            println!("AF185 {} cache installed", model.tag());
            let mut rows = Vec::new();
            let cfg = embedding::Config {
                enabled: true,
                model,
                ..Default::default()
            };
            for q in &queries.queries {
                for source in &q.overlay_sources {
                    db.operation(&q.owner)
                        .transaction(|state| {
                            state.sources.insert(source.id.clone(), source.clone());
                            Ok(())
                        })
                        .unwrap();
                }
                if !q.overlay_sources.is_empty() {
                    install_vectors(&db, &binding, &docs, &vectors);
                }
                for records in &queries.budgets {
                    let store = db.operation(&q.owner);
                    let (request, plan) = request_and_plan(&c, q);
                    let budget = Budget {
                        records: *records,
                        stored_bytes: q.stored_bytes,
                        ..Default::default()
                    };
                    let calls = std::cell::Cell::new(0);
                    let callback = |text: &str| {
                        assert_eq!(text, q.query);
                        calls.set(calls.get() + 1);
                        Ok(embedding::QueryVector {
                            binding: binding.clone(),
                            vector: vectors[&format!("query:{}", fp(&json!(text)))].clone(),
                        })
                    };
                    let start = Instant::now();
                    let assembly = recall::assemble_with_retrieval(
                        &store,
                        &request,
                        &plan,
                        budget,
                        &recall::Retrieval {
                            config: &cfg,
                            query: Some(&callback),
                        },
                        true,
                    )
                    .unwrap();
                    assert_eq!(
                        calls.get(),
                        usize::from(assembly.retrieval.status != RetrievalStatus::NotNeeded)
                    );
                    assert!(matches!(
                        assembly.retrieval.status,
                        RetrievalStatus::Ready | RetrievalStatus::NotNeeded
                    ));
                    let mut row = evaluate_assembly(
                        &c,
                        q,
                        &store,
                        "canonical",
                        *records,
                        &request,
                        &plan,
                        budget,
                        &assembly,
                        start.elapsed().as_micros(),
                        false,
                    );
                    row["retrieval"] = json!(assembly.retrieval);
                    row["diagnostic"]["lexical_df"] =
                        json!(assembly.retrieval_diagnostic().unwrap().lexical_df);
                    let labels: BTreeMap<_, _> = c
                        .records
                        .iter()
                        .map(|r| (r.memory.id, r.label.as_str()))
                        .collect();
                    let d = assembly.retrieval_diagnostic().unwrap();
                    row["diagnostic"]["channels"] = json!({"lexical":d.lexical_ranked.iter().map(|id|labels[id]).collect::<Vec<_>>(),
                        "vector":d.vector_ranked.iter().map(|id|labels[id]).collect::<Vec<_>>(),"fused":d.fused_ranked.iter().map(|id|labels[id]).collect::<Vec<_>>()});
                    row["query_embedding_calls"] = json!(calls.get());
                    rows.push(row);
                    println!(
                        "AF185 {} {} r={} covered={}",
                        model.tag(),
                        q.id,
                        records,
                        rows.last().unwrap()["required_all"]
                    );
                    // Opt-out preserves every byte sent to R4, not merely IDs or counts.
                    if model == Model::Qwen {
                        let classic = recall::assemble(&store, &request, &plan, budget).unwrap();
                        let disabled = recall::assemble_with_retrieval(
                            &store,
                            &request,
                            &plan,
                            budget,
                            &recall::Retrieval {
                                config: &embedding::Config::default(),
                                query: Some(&|_| panic!("disabled queried")),
                            },
                            true,
                        )
                        .unwrap();
                        assert_eq!(
                            serde_json::to_vec(&classic.evidence).unwrap(),
                            serde_json::to_vec(&disabled.evidence).unwrap()
                        );
                        assert_eq!(
                            recall_models::r4_input(&classic, c.utc_offset_seconds),
                            recall_models::r4_input(&disabled, c.utc_offset_seconds)
                        );
                        let mut actual = evaluate_assembly(
                            &c,
                            q,
                            &store,
                            "canonical",
                            *records,
                            &request,
                            &plan,
                            budget,
                            &disabled,
                            0,
                            true,
                        );
                        let mut expected = historical["rows"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .find(|r| {
                                r["id"] == q.id
                                    && r["records_budget"] == *records
                                    && r["insertion_order"] == "canonical"
                            })
                            .unwrap()
                            .clone();
                        actual["diagnostic"]["stages"]
                            .as_object_mut()
                            .unwrap()
                            .remove("r1_subject_mentions");
                        actual.as_object_mut().unwrap().remove("elapsed_us");
                        expected.as_object_mut().unwrap().remove("elapsed_us");
                        assert_eq!(
                            actual, expected,
                            "disabled differs from frozen AF173: {}/{}",
                            q.id, records
                        );
                    }
                }
                for source in &q.overlay_sources {
                    hippocampus::chat::ChatMemoryService::new(
                        db.operation(&q.owner),
                        Default::default(),
                    )
                    .unwrap()
                    .call("memory_forget", json!({"source_id":source.id}), || false)
                    .unwrap();
                }
            }
            models.push(json!({"model":model.tag(),"strict_reused_slots":slots.len(),"totals":queries.budgets.iter().map(|b|totals(&rows,*b)).collect::<Vec<_>>(),"summary":summary(&rows),"rows":rows}));
        }
        let report = json!({"revision":"AF296","production_path":true,"new_embedding_requests":0,"safety_violations":0,"disabled_evidence_byte_equal":true,"models":models});
        let directory =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("target/eval-retrieval/af296/replay");
        std::fs::create_dir_all(&directory).unwrap();
        atomic_write(&directory.join("report.json"), &report).unwrap();
        let mut md = String::from(
            "# AF288 生产混合检索严格回放\n\n新嵌入请求 0；两模型均严格复用 AF175 14 槽。disabled R4 输入逐字节相同；安全违规 0。\n\n| 模型 | records | 必需全覆盖 | MRR | precision |\n|---|---|---|---|---|\n",
        );
        for model in report["models"].as_array().unwrap() {
            for row in model["totals"].as_array().unwrap() {
                md.push_str(&format!(
                    "| {} | {} | {}/{} | {} | {} |\n",
                    model["model"],
                    row["records_budget"],
                    row["required_all"],
                    row["applicable"],
                    row["mrr"],
                    row["precision"]
                ));
            }
        }
        std::fs::write(directory.join("report.md"), md).unwrap();
        println!(
            "{}",
            json!(
                report["models"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|m| json!({"model":m["model"],"totals":m["totals"]}))
                    .collect::<Vec<_>>()
            )
        );
    }
    fn fake_binding() -> Binding {
        Binding::from_tags(
            Model::Qwen,
            &json!({"models":[{"name":Model::Qwen.tag(),"digest":"a".repeat(64)}]}),
        )
        .unwrap()
    }
    fn unit_vector(index: usize) -> Vec<f32> {
        let mut v = vec![0.0; embedding::DIMENSIONS];
        v[index] = 1.0;
        v
    }
    #[test]
    fn af185_cache_partial_corrupt_reopen_and_query_failure() {
        let (c, queries) = data();
        let db = Db::seed(&c, "canonical");
        let q = queries.queries.iter().find(|q| q.id == "Q03").unwrap();
        let (request, plan) = request_and_plan(&c, q);
        let binding = fake_binding();
        let mut store = db.open("A");
        let token = store
            .set_embedding_binding(embedding_index::Binding::new(binding.clone()).unwrap())
            .unwrap();
        let input = store.embedding_source_input(&token, "s-T03").unwrap();
        store
            .write_embedding_vectors(&input, &vec![unit_vector(0); input.segments.len()])
            .unwrap();
        drop(store);
        let store = db.open("A");
        let cfg = embedding::Config {
            enabled: true,
            ..Default::default()
        };
        let query = |_: &str| {
            Ok(embedding::QueryVector {
                binding: binding.clone(),
                vector: unit_vector(0),
            })
        };
        let assemble =
            |callback: Option<&dyn Fn(&str) -> embedding::Result<embedding::QueryVector>>| {
                recall::assemble_with_retrieval(
                    &store,
                    &request,
                    &plan,
                    Budget {
                        records: 8,
                        ..Default::default()
                    },
                    &recall::Retrieval {
                        config: &cfg,
                        query: callback,
                    },
                    true,
                )
                .unwrap()
            };
        let partial = assemble(Some(&query));
        assert_eq!(partial.retrieval.status, RetrievalStatus::Pending);
        assert_eq!(partial.retrieval.sources_indexed, 1);
        assert!(
            partial
                .evidence
                .records
                .iter()
                .any(|r| r.id() == MemoryId(3))
        );
        assert!(!partial.evidence.completeness.plan_complete);
        assert!(
            recall_models::r4_input(&partial, c.utc_offset_seconds)
                .get("retrieval")
                .is_none()
        );
        let wrong = queries.queries.iter().find(|q| q.id == "Q31").unwrap();
        let (wrong_request, wrong_plan) = request_and_plan(&c, wrong);
        let mut writer = db.open("A");
        let origin = writer.embedding_source_input(&token, "s-T01").unwrap();
        writer
            .write_embedding_vectors(&origin, &vec![unit_vector(0); origin.segments.len()])
            .unwrap();
        writer
            .transaction(|state| {
                let mut link = state.relations.values().next().unwrap().clone();
                link.id = "wrong-hint-over-budget".into();
                link.members = std::iter::once(MemoryId(1))
                    .chain(
                        c.records
                            .iter()
                            .filter(|r| r.label.starts_with('F'))
                            .map(|r| r.memory.id),
                    )
                    .collect();
                state.relations.insert(link.id.clone(), link);
                Ok(())
            })
            .unwrap();
        let wrong_result = recall::assemble_with_retrieval(
            &store,
            &wrong_request,
            &wrong_plan,
            Budget {
                records: 8,
                ..Default::default()
            },
            &recall::Retrieval {
                config: &cfg,
                query: Some(&query),
            },
            true,
        )
        .unwrap();
        assert_eq!(wrong_result.evidence.records[0].id(), MemoryId(1));
        assert!(wrong_result.evidence.records.len() <= 8);
        assert!(wrong_result.evidence.completeness.retrieval_truncated);
        assert!(!wrong_result.evidence.completeness.plan_complete);
        assert!(
            wrong_result
                .retrieval_diagnostic()
                .unwrap()
                .lexical_ranked
                .iter()
                .all(|id| !c
                    .records
                    .iter()
                    .any(|r| r.label.starts_with('F') && r.memory.id == *id))
        );
        assert!(wrong_result.retrieval_diagnostic().unwrap().stages["closure"].len() > 8);
        let failed = assemble(Some(&|_| Err("fake_query_failure".into())));
        assert_eq!(failed.retrieval.status, RetrievalStatus::Unavailable);
        assert!(
            failed
                .retrieval_diagnostic()
                .unwrap()
                .vector_ranked
                .is_empty()
        );
        let zero = recall::assemble_with_retrieval(
            &store,
            &request,
            &plan,
            Budget {
                records: 0,
                ..Default::default()
            },
            &recall::Retrieval {
                config: &cfg,
                query: Some(&query),
            },
            true,
        )
        .unwrap();
        assert!(zero.evidence.records.is_empty());
        assert!(zero.evidence.completeness.retrieval_truncated);
        let blocked = assemble(None);
        assert_eq!(blocked.retrieval.status, RetrievalStatus::Unavailable);
        rusqlite::Connection::open(&db.0)
            .unwrap()
            .execute(
                "UPDATE source_vectors SET vector=X'00000000' WHERE source_id='s-T03'",
                [],
            )
            .unwrap();
        let corrupt = assemble(Some(&query));
        assert_eq!(corrupt.retrieval.status, RetrievalStatus::Pending);
        assert_eq!(corrupt.retrieval.sources_indexed, 1);
        assert!(
            !corrupt
                .evidence
                .records
                .iter()
                .any(|r| r.id() == MemoryId(3))
        );
    }
    #[test]
    fn af185_query_runs_outside_transaction_and_deletion_is_not_publishable() {
        let (c, queries) = data();
        let db = Db::seed(&c, "canonical");
        let q = queries.queries.iter().find(|q| q.id == "Q03").unwrap();
        let (request, plan) = request_and_plan(&c, q);
        let binding = fake_binding();
        let docs = document_items(&c, &queries);
        let vectors = docs
            .iter()
            .map(|i| {
                (
                    i.key.clone(),
                    unit_vector(usize::from(i.source_id.as_deref() != Some("s-T03"))),
                )
            })
            .collect();
        install_vectors(&db, &binding, &docs, &vectors);
        let cfg = embedding::Config {
            enabled: true,
            ..Default::default()
        };
        let store = db.operation("A");
        let original = store.read().unwrap().sources["s-T03"]
            .request
            .as_ref()
            .unwrap()["text"]
            .as_str()
            .unwrap()
            .to_owned();
        let changed = recall::assemble_with_retrieval(
            &store,
            &request,
            &plan,
            Budget {
                records: 8,
                ..Default::default()
            },
            &recall::Retrieval {
                config: &cfg,
                query: Some(&|_| {
                    db.operation("A")
                        .transaction(|s| {
                            s.sources
                                .get_mut("s-T03")
                                .unwrap()
                                .request
                                .as_mut()
                                .unwrap()["text"] = json!(format!("{original}附加文本。"));
                            Ok(())
                        })
                        .unwrap();
                    Ok(embedding::QueryVector {
                        binding: binding.clone(),
                        vector: unit_vector(0),
                    })
                }),
            },
            true,
        )
        .unwrap();
        assert!(changed.check_snapshot(&store).is_err());
        let token = store.embedding_binding().unwrap().unwrap();
        assert!(
            store
                .read_embedding_vectors(&token, "s-T03")
                .unwrap()
                .is_empty()
        );
        db.operation("A")
            .transaction(|s| {
                s.sources
                    .get_mut("s-T03")
                    .unwrap()
                    .request
                    .as_mut()
                    .unwrap()["text"] = json!(original);
                Ok(())
            })
            .unwrap();
        install_vectors(&db, &binding, &docs, &vectors);
        let query = |_: &str| {
            // A different process can delete while the read-side embedding is running.
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "af175::af185_delete_child", "--nocapture"])
                .env("AF185_DELETE_CHILD_DB", &db.0)
                .env("PMR_MODEL_NETWORK", "deny")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            Ok(embedding::QueryVector {
                binding: binding.clone(),
                vector: unit_vector(0),
            })
        };
        let assembly = recall::assemble_with_retrieval(
            &store,
            &request,
            &plan,
            Budget {
                records: 8,
                ..Default::default()
            },
            &recall::Retrieval {
                config: &cfg,
                query: Some(&query),
            },
            true,
        )
        .unwrap();
        assert!(
            assembly
                .evidence
                .records
                .iter()
                .any(|r| r.id() == MemoryId(3))
        );
        assert!(assembly.check_snapshot(&store).is_err());
        assert!(
            assembly
                .finish_skipped(&store, recall::SkipReason::Offline)
                .is_err()
        );
        let later = recall::assemble_with_retrieval(
            &store,
            &request,
            &plan,
            Budget {
                records: 8,
                ..Default::default()
            },
            &recall::Retrieval {
                config: &cfg,
                query: Some(&|_| {
                    Ok(embedding::QueryVector {
                        binding: binding.clone(),
                        vector: unit_vector(0),
                    })
                }),
            },
            true,
        )
        .unwrap();
        assert!(!later.evidence.records.iter().any(|r| r.id() == MemoryId(3)));
    }
    #[test]
    fn af185_delete_child() {
        let Ok(path) = std::env::var("AF185_DELETE_CHILD_DB") else {
            return;
        };
        let path = Path::new(&path);
        assert!(
            path.parent()
                .unwrap()
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("af173-")
        );
        let store = SqliteMemoryStore::open_operation(path, UserId::from("A")).unwrap();
        hippocampus::chat::ChatMemoryService::new(store, Default::default())
            .unwrap()
            .call("memory_forget", json!({"source_id":"s-T03"}), || false)
            .unwrap();
    }
}

/// AF293 isolates entity diagnostics from the frozen corpus and embedding campaigns.
#[test]
fn af293_entity_diagnostics_without_model_calls() {
    fn expand(value: &mut Json, fields: &Json) {
        match value {
            Json::Object(map) => {
                for (key, value) in map {
                    if key == "fingerprint" {
                        let mut group: Json =
                            serde_json::from_str(value.as_str().unwrap()).unwrap();
                        expand(&mut group, fields);
                        *value = json!(serde_json::to_string(&group).unwrap());
                    } else {
                        expand(value, fields);
                    }
                }
            }
            Json::Array(values) => values.iter_mut().for_each(|v| expand(v, fields)),
            Json::String(text) => {
                for key in ["label", "owner", "subject", "relation", "text"] {
                    *text = text.replace(&format!("@{key}"), fields[key].as_str().unwrap());
                }
            }
            _ => {}
        }
    }
    let fixture: Json =
        serde_json::from_str(include_str!("eval/retrieval/entity-corpus-af293.json")).unwrap();
    let queries: Queries =
        serde_json::from_str(include_str!("eval/retrieval/entity-queries-af293.json")).unwrap();
    assert_eq!(fixture["schema"], "entity-diagnostic-corpus.v1");
    assert_eq!(queries.revision, "AF293");
    assert_eq!(queries.r1_contract, "r1.v2");
    assert_eq!(queries.budgets, [64, 8]);
    let mut rows = Vec::new();
    for variant in ["low", "high"] {
        let mut c: Corpus = serde_json::from_value(fixture["base"].clone()).unwrap();
        let mut specs = fixture["records"].as_array().unwrap().clone();
        for rule in fixture["fillers"].as_array().unwrap() {
            for i in 1..=rule["count"].as_u64().unwrap() {
                specs.push(
                    json!({"label":format!("{}{:03}", rule["prefix"].as_str().unwrap(), i),
                    "owner":rule["owner"],"subject":rule["subject"],"relation":rule["relation"],
                    "text":rule[variant].as_str().unwrap().replace("{i}", &i.to_string())}),
                );
            }
        }
        for (i, spec) in specs.iter().enumerate() {
            let mut r = fixture["record_template"].clone();
            let mut source = fixture["source_template"].clone();
            expand(&mut r, spec);
            expand(&mut source, spec);
            r["memory"]["id"] = json!(i + 1);
            r["receipt"]["products"]["0"] = json!(i + 1);
            source["record"]["received_sequence"] = json!(i + 1);
            c.records.push(serde_json::from_value(r).unwrap());
            c.sources.push(serde_json::from_value(source).unwrap());
        }
        let db = Db::seed(&c, "canonical");
        for q in &queries.queries {
            let (request, plan) = request_and_plan(&c, q);
            assert_eq!(plan.output.as_ref().unwrap().plan.op, R1Op::Lookup);
            let store = db.operation(&q.owner);
            let state = store.read().unwrap();
            // The fixture has one complete, single-subject record per source: this reproduces
            // production's trusted-scope source set without duplicating its DF/ranking algorithm.
            let docs: BTreeMap<_, _> = c
                .records
                .iter()
                .filter(|r| {
                    r.memory.owner_id == request.owner
                        && request
                            .subject_ids
                            .as_ref()
                            .is_none_or(|ids| ids.contains(&r.memory.subject_id))
                })
                .map(|r| {
                    let SourceRef::Stored { source_id } = &r.contract.source else {
                        unreachable!()
                    };
                    (
                        source_id.clone(),
                        state.sources[source_id].request.as_ref().unwrap()["text"]
                            .as_str()
                            .unwrap()
                            .to_owned(),
                    )
                })
                .collect();
            let entities = state.entities.list_by_owner(&request.owner);
            let hint = format!("{} {}", q.query, plan.output.as_ref().unwrap().relation);
            let df = LexicalDf::new(&hint, &entities, docs.values().map(String::as_str));
            let daughter = format!(" {} ", LexicalDf::index_text("女儿"));
            let daughter_df = docs
                .values()
                .filter(|text| format!(" {} ", LexicalDf::index_text(text)).contains(&daughter))
                .count();
            if q.subject_ids.is_none() {
                assert_eq!(LexicalDf::kept(daughter_df, docs.len()), variant == "low");
            }
            for records in &queries.budgets {
                for enabled in [false, true] {
                    let calls = std::cell::Cell::new(0);
                    let fail_query = |_: &str| -> Result<embedding::QueryVector, String> {
                        calls.set(calls.get() + 1);
                        Err("embedding_connect".into())
                    };
                    let cfg = embedding::Config {
                        enabled,
                        ..Default::default()
                    };
                    let budget = Budget {
                        records: *records,
                        stored_bytes: q.stored_bytes,
                        ..Default::default()
                    };
                    let started = Instant::now();
                    let assembly = recall::assemble_with_retrieval(
                        &store,
                        &request,
                        &plan,
                        budget,
                        &recall::Retrieval {
                            config: &cfg,
                            query: Some(&fail_query),
                        },
                        true,
                    )
                    .unwrap();
                    let diagnostic = assembly.retrieval_diagnostic().unwrap();
                    assert!(
                        diagnostic.stages["r1_subject"].len() > 64,
                        "{} {variant}",
                        q.id
                    );
                    assert!(
                        diagnostic.enumeration.is_none(),
                        "{} unexpectedly enumerated",
                        q.id
                    );
                    assert_eq!(calls.get(), usize::from(enabled));
                    assert_eq!(
                        assembly.retrieval.status,
                        if enabled {
                            RetrievalStatus::Unavailable
                        } else {
                            RetrievalStatus::Disabled
                        }
                    );
                    assert!(
                        assembly.evidence.raw.is_empty(),
                        "completed sources must not gain raw fallback"
                    );
                    let mut row = evaluate_assembly(
                        &c,
                        q,
                        &store,
                        "canonical",
                        *records,
                        &request,
                        &plan,
                        budget,
                        &assembly,
                        started.elapsed().as_micros(),
                        false,
                    );
                    let targets: Vec<_> = q.required_records.iter().map(|label| {
                        let r = c.records.iter().find(|r| &r.label == label).unwrap();
                        json!({"label":label,"trusted_scope":diagnostic.stages["trusted_scope"].contains(&r.memory.id),
                            "r1_subject":diagnostic.stages["r1_subject"].contains(&r.memory.id),
                            "omitted_at":omitted_at(diagnostic,r.memory.id)})
                    }).collect();
                    if q.id == "D5-bound" {
                        assert_eq!(targets[0]["trusted_scope"], true);
                        assert_eq!(targets[0]["r1_subject"], true);
                        assert_eq!(row["required_all"], true);
                    }
                    if q.id == "D5-wide" {
                        assert_eq!(targets[0]["r1_subject"], true);
                    }
                    if q.id == "SAFE" {
                        assert!(diagnostic.stages["trusted_scope"].iter().all(|id| {
                            c.records
                                .iter()
                                .find(|r| r.memory.id == *id)
                                .unwrap()
                                .memory
                                .subject_id
                                .0
                                == "e-daughter"
                        }));
                    }
                    row["variant"] = json!(variant);
                    row["arm"] = json!(if enabled {
                        "lexical-df-fallback"
                    } else {
                        "embedding-disabled"
                    });
                    row["df"] = json!(df);
                    row["df_applied"] = json!(enabled);
                    row["targets"] = json!(targets);
                    row["retrieval"] = json!(assembly.retrieval);
                    row["injected_failure_calls"] = json!(calls.get());
                    row["other_child_returned"] = json!(
                        row["returned"]
                            .as_array()
                            .unwrap()
                            .contains(&json!("OTHER"))
                    );
                    rows.push(row);
                }
            }
        }
    }
    assert_eq!(rows.len(), queries.queries.len() * 8);
    let out = Path::new("target/eval-retrieval/af296/entity");
    std::fs::create_dir_all(out).unwrap();
    let report = json!({"revision":"AF296","model_calls":0,"embedding_calls":0,"vectors_added":0,
        "corpus_fingerprint":recall_models::fingerprint(&fixture),
        "queries_fingerprint":recall_models::fingerprint(&serde_json::from_str::<Json>(include_str!("eval/retrieval/entity-queries-af293.json")).unwrap()),
        "note":"Injected R1 and complete records; not a W1/R1 accuracy or vector-recall experiment. DF is diagnostic only in disabled arm. Retrieval misses do not fail the test.","rows":rows});
    std::fs::write(
        out.join("report.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
    let mut md = String::from(
        "# AF293 实体检索诊断\n\n真实模型/嵌入调用 0；只注入失败，无向量。命中不是回答正确率；未登记昵称没有指代真值。\n\n|题|DF|预算|臂|目标返回|遗漏阶段|R3原文覆盖|其他孩子返回|\n|---|---|---:|---|---|---|---|---|\n",
    );
    for row in report["rows"].as_array().unwrap() {
        md.push_str(&format!(
            "|{}|{}|{}|{}|{}|{}|{}|{}|\n",
            row["id"].as_str().unwrap(),
            row["variant"].as_str().unwrap(),
            row["records_budget"],
            row["arm"].as_str().unwrap(),
            row["required_all"],
            row["targets"][0]["omitted_at"].as_str().unwrap(),
            row["r3_original_coverage"],
            row["other_child_returned"]
        ));
    }
    std::fs::write(out.join("report.md"), md).unwrap();
    println!(
        "AF296: {} rows, 0 model calls, 0 embedding calls; {}",
        report["rows"].as_array().unwrap().len(),
        out.display()
    );
}
