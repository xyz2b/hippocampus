//! 20 个录制 LLM 端到端案例：测试数据只保存原始中文与 strict JSON 输出。
//!
//! CandidateMemory / RuntimeContext 均通过生产 OpenAI decoder 生成，CI 不访问网络。
//! common::grounded 从可信 SQLite 恢复显式已批准夹具，仅测试 decoder 与后续领域行为。
//! 此回放不验证时间依据或真实独立来源审核，不能作为模型或入库安全验收证据。
//! 日期字段为按新版合同补齐的合成提议，非历史模型原始回包。

mod common;

use hippocampus::*;
use serde_json::{Value, json};

const OWNER: &str = "user-1";
const USER: &str = "user-entity";
const NOW: i64 = 1_788_494_400;
const OFFSET: i32 = 8 * 60 * 60;

struct RecordedCase {
    name: &'static str,
    predicates: Vec<(&'static str, Cardinality)>,
    relations: Vec<(&'static str, &'static str)>,
    turns: Vec<RecordedTurn>,
    retention_at: Option<i64>,
    expected_store: Vec<ExpectedMemory>,
    query: RecordedQuery,
}

struct RecordedTurn {
    session_id: &'static str,
    turn_id: &'static str,
    at: i64,
    input: &'static str,
    recorded_output: Value,
    /// Pure clarification turns resolve the single pending candidate with this reference.
    clarification: Option<&'static str>,
    expected: Vec<ExpectedWrite>,
}

struct RecordedQuery {
    at: i64,
    input: &'static str,
    recorded_output: Value,
    expected: ExpectedRetrieval,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExpectedWrite {
    Applied(ReconciliationAction),
    Rejected,
    Pending,
    Duplicate,
    Conflict,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ExpectedMemory {
    id: MemoryId,
    subject_id: EntityId,
    memory_type: MemoryType,
    predicate: Predicate,
    value: MemoryValue,
    retention_tier: RetentionTier,
    status: MemoryStatus,
    supersedes: Option<MemoryId>,
    superseded_by: Option<MemoryId>,
    supersede_reason: Option<SupersedeReason>,
    reinforcement_count: u32,
}

#[derive(Default)]
struct ExpectedRetrieval {
    mandatory: Vec<&'static str>,
    constraints: Vec<&'static str>,
    preferences: Vec<&'static str>,
    facts: Vec<&'static str>,
    episodes: Vec<&'static str>,
    conflicts: usize,
}

#[test]
fn recorded_llm_v1_acceptance_matrix_has_twenty_cases() {
    let cases = cases();
    assert_eq!(cases.len(), 20);
    for case in cases {
        run(case);
    }
}

fn run(case: RecordedCase) {
    let entities = entities();
    let mut entity_repository = InMemoryEntityRepository::default();
    for entity in &entities {
        entity_repository.upsert(entity.clone());
    }
    let mut predicates = InMemoryPredicateRegistry::default();
    for (predicate, cardinality) in &case.predicates {
        predicates.register(Predicate::from(*predicate), *cardinality);
    }
    let mut relations = InMemorySemanticRelationRegistry::default();
    for (child, parent) in &case.relations {
        relations.register_is_a(MemoryValue::from(*child), MemoryValue::from(*parent));
    }
    let mut engine = MemoryEngine::new(InMemoryMemoryRepository::default(), predicates);
    let mut pending = Vec::new();

    for turn in &case.turns {
        let context = adapter_context(entities.clone(), turn.at, &case.predicates);
        let candidates = context
            .decode_extraction(
                &turn.recorded_output.to_string(),
                turn.input,
                &SessionId::from(turn.session_id),
                &TurnId::from(turn.turn_id),
            )
            .unwrap_or_else(|error| panic!("{}: decode failed: {error}", case.name));

        let mut actual = Vec::new();
        if let Some(reference) = turn.clarification {
            assert!(
                candidates.is_empty(),
                "{}: clarification extracted memory",
                case.name
            );
            let staged_id = pending
                .pop()
                .expect("clarification requires one pending candidate");
            let evidence = Evidence::new(
                EvidenceId(format!(
                    "{}:{}:clarification",
                    turn.session_id, turn.turn_id
                )),
                SessionId::from(turn.session_id),
                TurnId::from(turn.turn_id),
                turn.input,
            );
            actual.push(normalize_candidate_outcome(
                engine
                    .clarify_staged(staged_id, reference, evidence, &entity_repository)
                    .unwrap(),
                &mut pending,
            ));
        } else {
            for candidate in candidates {
                if candidate.subject_id().is_some() {
                    let report = engine
                        .ingest_with_trace(common::grounded(candidate))
                        .unwrap();
                    assert!(!report.trace.admission_reason.is_empty());
                    actual.push(normalize_outcome(report.outcome));
                } else {
                    actual.push(normalize_candidate_outcome(
                        engine
                            .ingest_or_stage(common::grounded(candidate), &entity_repository)
                            .unwrap(),
                        &mut pending,
                    ));
                }
            }
        }
        assert_eq!(actual, turn.expected, "{}: write mismatch", case.name);
    }

    if let Some(at) = case.retention_at {
        engine
            .run_retention_for_owner(&UserId::from(OWNER), Timestamp(at))
            .unwrap();
    }
    let actual_store = engine
        .repository
        .list_by_owner(&UserId::from(OWNER))
        .unwrap();
    assert_store(&case, &actual_store);

    assert!(
        !case.query.input.trim().is_empty(),
        "{}: raw query must not be empty",
        case.name
    );
    let context = adapter_context(entities, case.query.at, &case.predicates)
        .decode_runtime_context(&case.query.recorded_output.to_string())
        .unwrap_or_else(|error| panic!("{}: query decode failed: {error}", case.name));
    let result = DefaultMemoryRetriever::new(&mut engine.repository, &engine.predicates)
        .with_semantic_relations(&relations)
        .retrieve(&context)
        .unwrap();
    assert_retrieval(&case, &result);
    assert_eq!(
        result.trace.len(),
        actual_store.len(),
        "{}: incomplete trace",
        case.name
    );
}

fn normalize_candidate_outcome(
    outcome: CandidateIngestOutcome,
    pending: &mut Vec<StagedCandidateId>,
) -> ExpectedWrite {
    match outcome {
        CandidateIngestOutcome::Ingested(outcome) => normalize_outcome(outcome),
        CandidateIngestOutcome::Pending { staged_id, .. } => {
            pending.push(staged_id);
            ExpectedWrite::Pending
        }
    }
}

fn normalize_outcome(outcome: IngestOutcome) -> ExpectedWrite {
    match outcome {
        IngestOutcome::Applied { action, .. } => ExpectedWrite::Applied(action),
        IngestOutcome::Rejected(_) | IngestOutcome::Ignored => ExpectedWrite::Rejected,
        IngestOutcome::Duplicate { .. } => ExpectedWrite::Duplicate,
        IngestOutcome::Conflict { .. } => ExpectedWrite::Conflict,
    }
}

fn assert_store(case: &RecordedCase, actual: &[Memory]) {
    assert_eq!(
        actual.len(),
        case.expected_store.len(),
        "{}: store size",
        case.name
    );
    for (actual, expected) in actual.iter().zip(&case.expected_store) {
        let snapshot = ExpectedMemory {
            id: actual.id,
            subject_id: actual.subject_id.clone(),
            memory_type: actual.memory_type,
            predicate: actual.predicate.clone(),
            value: actual.value.clone(),
            retention_tier: actual.retention_tier,
            status: actual.status,
            supersedes: actual.supersedes,
            superseded_by: actual.superseded_by,
            supersede_reason: actual.supersede_reason,
            reinforcement_count: actual.reinforcement_count,
        };
        assert_eq!(&snapshot, expected, "{}: store mismatch", case.name);
    }
}

fn assert_retrieval(case: &RecordedCase, result: &RetrievalResult) {
    let expected = &case.query.expected;
    assert_lane(
        case.name,
        "mandatory",
        &result.mandatory,
        &expected.mandatory,
    );
    assert_lane(
        case.name,
        "constraints",
        &result.constraints,
        &expected.constraints,
    );
    assert_lane(
        case.name,
        "preferences",
        &result.preferences,
        &expected.preferences,
    );
    assert_lane(case.name, "facts", &result.facts, &expected.facts);
    assert_lane(case.name, "episodes", &result.episodes, &expected.episodes);
    assert_eq!(
        result.conflicts.len(),
        expected.conflicts,
        "{}: conflicts",
        case.name
    );
}

fn assert_lane(name: &str, lane: &str, memories: &[Memory], expected: &[&str]) {
    let mut actual: Vec<_> = memories
        .iter()
        .map(|memory| match &memory.value {
            MemoryValue::Text(value) => value.as_str(),
            _ => panic!("{name}: {lane} contains non-text fixture"),
        })
        .collect();
    let mut expected = expected.to_vec();
    actual.sort_unstable();
    expected.sort_unstable();
    assert_eq!(actual, expected, "{name}: {lane} mismatch");
}

fn adapter_context(
    entities: Vec<Entity>,
    at: i64,
    predicates: &[(&str, Cardinality)],
) -> OpenAiAdapterContext {
    OpenAiAdapterContext::new(UserId::from(OWNER), entities, Timestamp(at))
        .unwrap()
        .with_utc_offset_seconds(OFFSET)
        .unwrap()
        .with_semantic_catalog(
            predicates
                .iter()
                .map(|(predicate, cardinality)| PredicateDefinition {
                    predicate: Predicate::from(*predicate),
                    cardinality: *cardinality,
                })
                .collect(),
            ["food", "travel", "programming"]
                .into_iter()
                .map(Domain::from)
                .collect(),
        )
        .unwrap()
}

fn entities() -> Vec<Entity> {
    vec![
        entity(USER, "用户", &["我"]),
        entity("child-older", "大儿子", &["他", "儿子", "大宝"]),
        entity("child-younger", "小儿子", &["他", "儿子", "小宝"]),
    ]
}

fn entity(id: &str, name: &str, aliases: &[&str]) -> Entity {
    Entity {
        id: EntityId::from(id),
        owner_id: UserId::from(OWNER),
        entity_type: EntityType::Person,
        canonical_name: name.to_owned(),
        aliases: aliases.iter().map(|alias| (*alias).to_owned()).collect(),
    }
}

fn recorded(candidates: Vec<Value>) -> Value {
    json!({ "candidates": candidates })
}

fn raw_candidate(memory_type: &str, predicate: &str, value: &str) -> Value {
    json!({
        "subject_id": USER,
        "unresolved_subject": null,
        "memory_type": memory_type,
        "predicate": predicate,
        "value": raw_value(value),
        "polarity": null,
        "strength": null,
        "scope": { "domain": null, "conditions": [] },
        "retention_tier": "long_term",
        "valid_from": null,
        "valid_to": null,
        "expires_at": null,
        "temporal_expression": null,
        "extraction_confidence": "high",
        "certainty": "high",
        "source": "user_statement",
        "intent": { "kind": "assert", "target_memory_id": null, "reason": null },
        "explicit_memory_request": false,
        "cross_turn_useful": true,
        "tags": []
    })
}

fn raw_value(value: &str) -> Value {
    json!({
        "kind": "text",
        "text": value,
        "integer": null,
        "boolean": null,
        "entity_id": null
    })
}

fn with_polarity(mut candidate: Value, polarity: &str) -> Value {
    candidate["polarity"] = json!(polarity);
    candidate
}

fn with_strength(mut candidate: Value, strength: &str) -> Value {
    candidate["strength"] = json!(strength);
    candidate
}

fn with_scope(mut candidate: Value, domain: Option<&str>, conditions: Vec<Value>) -> Value {
    candidate["scope"] = json!({ "domain": domain, "conditions": conditions });
    candidate
}

fn with_intent(
    mut candidate: Value,
    kind: &str,
    target: Option<u64>,
    reason: Option<&str>,
) -> Value {
    candidate["intent"] = json!({ "kind": kind, "target_memory_id": target, "reason": reason });
    candidate
}

fn with_retention(mut candidate: Value, tier: &str, expiry: Option<(&str, &str)>) -> Value {
    candidate["retention_tier"] = json!(tier);
    candidate["expiry_expression"] = json!(expiry.map(|(expression, _)| expression));
    candidate["expiry_date"] = json!(expiry.map(|(_, date)| date));
    if expiry.is_some() {
        candidate["explicit_memory_request"] = json!(true);
    }
    candidate
}

fn with_temporal(mut candidate: Value, expression: &str, dates: Option<(&str, &str)>) -> Value {
    candidate["temporal_expression"] = json!(expression);
    candidate["temporal_date"] = json!(dates.map(|(start, _)| start));
    candidate["temporal_end_date"] = json!(dates.map(|(_, end)| end));
    candidate
}

fn unresolved(mut candidate: Value, reference: &str) -> Value {
    candidate["subject_id"] = Value::Null;
    candidate["unresolved_subject"] = json!(reference);
    candidate
}

fn raw_query(predicates: &[&str]) -> Value {
    json!({
        "domain": null,
        "entity_ids": [USER],
        "task_id": null,
        "conditions": [],
        "predicates": predicates,
        "targets": [],
        "temporal_intent": "current",
        "temporal_expression": null
    })
}

fn query_scope(mut query: Value, domain: Option<&str>, conditions: Vec<Value>) -> Value {
    query["domain"] = json!(domain);
    query["conditions"] = json!(conditions);
    query
}

fn query_entities(mut query: Value, entities: &[&str]) -> Value {
    query["entity_ids"] = json!(entities);
    query
}

fn query_targets(mut query: Value, targets: &[&str]) -> Value {
    query["targets"] = Value::Array(targets.iter().map(|target| raw_value(target)).collect());
    query
}

fn query_temporal(mut query: Value, intent: &str, expression: Option<&str>) -> Value {
    query["temporal_intent"] = json!(intent);
    query["temporal_expression"] = json!(expression);
    query
}

fn stored(id: u64, memory_type: MemoryType, predicate: &str, value: &str) -> ExpectedMemory {
    ExpectedMemory {
        id: MemoryId(id),
        subject_id: EntityId::from(USER),
        memory_type,
        predicate: Predicate::from(predicate),
        value: MemoryValue::from(value),
        retention_tier: RetentionTier::LongTerm,
        status: MemoryStatus::Active,
        supersedes: None,
        superseded_by: None,
        supersede_reason: None,
        reinforcement_count: 0,
    }
}

fn for_subject(mut memory: ExpectedMemory, subject: &str) -> ExpectedMemory {
    memory.subject_id = EntityId::from(subject);
    memory
}

fn retained(mut memory: ExpectedMemory, tier: RetentionTier) -> ExpectedMemory {
    memory.retention_tier = tier;
    memory
}

fn reinforced(mut memory: ExpectedMemory, count: u32) -> ExpectedMemory {
    memory.reinforcement_count = count;
    memory
}

fn ended(
    mut memory: ExpectedMemory,
    superseded_by: Option<u64>,
    reason: SupersedeReason,
) -> ExpectedMemory {
    memory.status = MemoryStatus::Superseded;
    memory.superseded_by = superseded_by.map(MemoryId);
    memory.supersede_reason = Some(reason);
    memory
}

fn replacing(mut memory: ExpectedMemory, old: u64) -> ExpectedMemory {
    memory.supersedes = Some(MemoryId(old));
    memory
}

fn expired(mut memory: ExpectedMemory) -> ExpectedMemory {
    memory.status = MemoryStatus::Expired;
    memory
}

fn turn(
    session_id: &'static str,
    turn_id: &'static str,
    at: i64,
    input: &'static str,
    candidates: Vec<Value>,
    expected: Vec<ExpectedWrite>,
) -> RecordedTurn {
    RecordedTurn {
        session_id,
        turn_id,
        at,
        input,
        recorded_output: recorded(candidates),
        clarification: None,
        expected,
    }
}

fn clarification(
    session_id: &'static str,
    turn_id: &'static str,
    at: i64,
    input: &'static str,
    reference: &'static str,
    expected: ExpectedWrite,
) -> RecordedTurn {
    RecordedTurn {
        session_id,
        turn_id,
        at,
        input,
        recorded_output: recorded(Vec::new()),
        clarification: Some(reference),
        expected: vec![expected],
    }
}

fn query(
    at: i64,
    input: &'static str,
    recorded_output: Value,
    expected: ExpectedRetrieval,
) -> RecordedQuery {
    RecordedQuery {
        at,
        input,
        recorded_output,
        expected,
    }
}

fn cases() -> Vec<RecordedCase> {
    let activity = |value: &str| json!({ "kind": "activity", "activity": value });
    let companion_children = || {
        json!({
            "kind": "companion",
            "entity_id": null,
            "role": "children"
        })
    };
    let japan = || {
        json!({
            "kind": "location",
            "location_kind": "country",
            "location_value": "JP"
        })
    };

    vec![
        // 01: 基础 Fact NEW 与当前查询。
        RecordedCase {
            name: "01 new residence fact",
            predicates: vec![("primary_residence", Cardinality::Single)],
            relations: vec![],
            turns: vec![turn(
                "s01",
                "t1",
                1,
                "我现在住南京。",
                vec![raw_candidate("fact", "primary_residence", "Nanjing")],
                vec![ExpectedWrite::Applied(ReconciliationAction::New)],
            )],
            retention_at: None,
            expected_store: vec![stored(1, MemoryType::Fact, "primary_residence", "Nanjing")],
            query: query(
                2,
                "我现在住哪里？",
                raw_query(&["primary_residence"]),
                ExpectedRetrieval {
                    facts: vec!["Nanjing"],
                    ..ExpectedRetrieval::default()
                },
            ),
        },
        // 02: 跨 session 的相同陈述是独立强化。
        RecordedCase {
            name: "02 cross-session reinforcement",
            predicates: vec![("likes", Cardinality::Multiple)],
            relations: vec![],
            turns: vec![
                turn(
                    "s02-a",
                    "t1",
                    1,
                    "我喜欢咖啡。",
                    vec![with_polarity(
                        raw_candidate("preference", "likes", "coffee"),
                        "positive",
                    )],
                    vec![ExpectedWrite::Applied(ReconciliationAction::New)],
                ),
                turn(
                    "s02-b",
                    "t1",
                    2,
                    "对，我确实喜欢咖啡。",
                    vec![with_polarity(
                        raw_candidate("preference", "likes", "coffee"),
                        "positive",
                    )],
                    vec![ExpectedWrite::Applied(ReconciliationAction::Reinforce)],
                ),
            ],
            retention_at: None,
            expected_store: vec![reinforced(
                stored(1, MemoryType::Preference, "likes", "coffee"),
                1,
            )],
            query: query(
                3,
                "我喜欢喝什么？",
                raw_query(&["likes"]),
                ExpectedRetrieval {
                    preferences: vec!["coffee"],
                    ..ExpectedRetrieval::default()
                },
            ),
        },
        // 03: 搬家是 StateChanged supersession。
        RecordedCase {
            name: "03 residence state change",
            predicates: vec![("primary_residence", Cardinality::Single)],
            relations: vec![],
            turns: vec![
                turn(
                    "s03-a",
                    "t1",
                    1,
                    "我住南京。",
                    vec![raw_candidate("fact", "primary_residence", "Nanjing")],
                    vec![ExpectedWrite::Applied(ReconciliationAction::New)],
                ),
                turn(
                    "s03-b",
                    "t1",
                    2,
                    "我搬到阜阳了。",
                    vec![with_intent(
                        raw_candidate("fact", "primary_residence", "Fuyang"),
                        "supersede",
                        Some(1),
                        Some("state_changed"),
                    )],
                    vec![ExpectedWrite::Applied(ReconciliationAction::Supersede)],
                ),
            ],
            retention_at: None,
            expected_store: vec![
                ended(
                    stored(1, MemoryType::Fact, "primary_residence", "Nanjing"),
                    Some(2),
                    SupersedeReason::StateChanged,
                ),
                replacing(
                    stored(2, MemoryType::Fact, "primary_residence", "Fuyang"),
                    1,
                ),
            ],
            query: query(
                3,
                "我现在住哪里？",
                raw_query(&["primary_residence"]),
                ExpectedRetrieval {
                    facts: vec!["Fuyang"],
                    ..ExpectedRetrieval::default()
                },
            ),
        },
        // 04: 用户纠错保留 lineage，但错误值不进入当前结果。
        RecordedCase {
            name: "04 corrected episode",
            predicates: vec![("trip_month", Cardinality::Single)],
            relations: vec![],
            turns: vec![
                turn(
                    "s04",
                    "t1",
                    1,
                    "旅行是在七月。",
                    vec![raw_candidate("episode", "trip_month", "July")],
                    vec![ExpectedWrite::Applied(ReconciliationAction::New)],
                ),
                turn(
                    "s04",
                    "t2",
                    2,
                    "不对，是六月。",
                    vec![with_intent(
                        raw_candidate("episode", "trip_month", "June"),
                        "supersede",
                        Some(1),
                        Some("correction"),
                    )],
                    vec![ExpectedWrite::Applied(ReconciliationAction::Supersede)],
                ),
            ],
            retention_at: None,
            expected_store: vec![
                ended(
                    stored(1, MemoryType::Episode, "trip_month", "July"),
                    Some(2),
                    SupersedeReason::Correction,
                ),
                replacing(stored(2, MemoryType::Episode, "trip_month", "June"), 1),
            ],
            query: query(
                3,
                "旅行是几月？",
                raw_query(&["trip_month"]),
                ExpectedRetrieval {
                    episodes: vec!["June"],
                    ..ExpectedRetrieval::default()
                },
            ),
        },
        // 05: 全局禁咖啡缩窄为早餐约束。
        RecordedCase {
            name: "05 global constraint narrowed",
            predicates: vec![("exclude_drink", Cardinality::Multiple)],
            relations: vec![],
            turns: vec![
                turn(
                    "s05",
                    "t1",
                    1,
                    "以后都别推荐咖啡。",
                    vec![with_polarity(
                        with_scope(
                            raw_candidate("constraint", "exclude_drink", "coffee"),
                            Some("food"),
                            vec![],
                        ),
                        "negative",
                    )],
                    vec![ExpectedWrite::Applied(ReconciliationAction::New)],
                ),
                turn(
                    "s05",
                    "t2",
                    2,
                    "算了，早餐别推荐就行。",
                    vec![with_intent(
                        with_polarity(
                            with_scope(
                                raw_candidate("constraint", "exclude_drink", "coffee"),
                                Some("food"),
                                vec![activity("breakfast")],
                            ),
                            "negative",
                        ),
                        "supersede",
                        Some(1),
                        Some("scope_refined"),
                    )],
                    vec![ExpectedWrite::Applied(ReconciliationAction::Supersede)],
                ),
            ],
            retention_at: None,
            expected_store: vec![
                ended(
                    stored(1, MemoryType::Constraint, "exclude_drink", "coffee"),
                    Some(2),
                    SupersedeReason::ScopeRefined,
                ),
                replacing(
                    stored(2, MemoryType::Constraint, "exclude_drink", "coffee"),
                    1,
                ),
            ],
            query: query(
                3,
                "早餐喝什么？",
                query_scope(
                    raw_query(&["exclude_drink"]),
                    Some("food"),
                    vec![activity("breakfast")],
                ),
                ExpectedRetrieval {
                    constraints: vec!["coffee"],
                    ..ExpectedRetrieval::default()
                },
            ),
        },
        // 06: 日本亲子游的允许民宿特例覆盖一般禁用规则。
        RecordedCase {
            name: "06 family travel exception",
            predicates: vec![("homestay", Cardinality::Multiple)],
            relations: vec![],
            turns: vec![
                turn(
                    "s06",
                    "t1",
                    1,
                    "旅行时不要住民宿。",
                    vec![with_polarity(
                        with_scope(
                            raw_candidate("constraint", "homestay", "homestay"),
                            Some("travel"),
                            vec![],
                        ),
                        "negative",
                    )],
                    vec![ExpectedWrite::Applied(ReconciliationAction::New)],
                ),
                turn(
                    "s06",
                    "t2",
                    2,
                    "带孩子去日本时可以住民宿。",
                    vec![with_polarity(
                        with_scope(
                            raw_candidate("constraint", "homestay", "homestay"),
                            Some("travel"),
                            vec![companion_children(), japan()],
                        ),
                        "positive",
                    )],
                    vec![ExpectedWrite::Applied(ReconciliationAction::New)],
                ),
            ],
            retention_at: None,
            expected_store: vec![
                stored(1, MemoryType::Constraint, "homestay", "homestay"),
                stored(2, MemoryType::Constraint, "homestay", "homestay"),
            ],
            query: query(
                3,
                "带孩子去日本能住民宿吗？",
                query_scope(
                    raw_query(&["homestay"]),
                    Some("travel"),
                    vec![companion_children(), japan()],
                ),
                ExpectedRetrieval {
                    constraints: vec!["homestay"],
                    ..ExpectedRetrieval::default()
                },
            ),
        },
        // 07: 模糊代词先 staging，中文澄清后才正式入库。
        RecordedCase {
            name: "07 ambiguous pronoun clarification",
            predicates: vec![("likes", Cardinality::Multiple)],
            relations: vec![],
            turns: vec![
                turn(
                    "s07",
                    "t1",
                    1,
                    "他不喜欢游泳。",
                    vec![unresolved(
                        with_polarity(raw_candidate("preference", "likes", "swimming"), "negative"),
                        "他",
                    )],
                    vec![ExpectedWrite::Pending],
                ),
                clarification(
                    "s07",
                    "t2",
                    2,
                    "我是说小儿子。",
                    "小儿子",
                    ExpectedWrite::Applied(ReconciliationAction::New),
                ),
            ],
            retention_at: None,
            expected_store: vec![for_subject(
                stored(1, MemoryType::Preference, "likes", "swimming"),
                "child-younger",
            )],
            query: query(
                3,
                "小儿子喜欢游泳吗？",
                query_entities(raw_query(&["likes"]), &["child-younger"]),
                ExpectedRetrieval {
                    preferences: vec!["swimming"],
                    ..ExpectedRetrieval::default()
                },
            ),
        },
        // 08: +08 当地中午记下的 ShortTerm 预算在明天日末自动过期。
        RecordedCase {
            name: "08 short-term budget expiry",
            predicates: vec![("hotel_budget", Cardinality::Single)],
            relations: vec![],
            turns: vec![turn(
                "s08",
                "t1",
                NOW,
                "这次订酒店预算五千元，保留到明天。",
                vec![with_retention(
                    raw_candidate("constraint", "hotel_budget", "5000"),
                    "short_term",
                    Some(("明天", "2026-09-05")),
                )],
                vec![ExpectedWrite::Applied(ReconciliationAction::New)],
            )],
            retention_at: Some(NOW + 129_600),
            expected_store: vec![expired(retained(
                stored(1, MemoryType::Constraint, "hotel_budget", "5000"),
                RetentionTier::ShortTerm,
            ))],
            query: query(
                NOW + 129_600,
                "现在酒店预算是多少？",
                raw_query(&["hotel_budget"]),
                ExpectedRetrieval::default(),
            ),
        },
        // 09: Durable 全局中文要求进入 mandatory lane。
        RecordedCase {
            name: "09 durable Chinese requirement",
            predicates: vec![("response_language", Cardinality::Single)],
            relations: vec![],
            turns: vec![turn(
                "s09",
                "t1",
                1,
                "以后都用中文回答我。",
                vec![with_retention(
                    raw_candidate("constraint", "response_language", "Chinese"),
                    "durable",
                    None,
                )],
                vec![ExpectedWrite::Applied(ReconciliationAction::New)],
            )],
            retention_at: None,
            expected_store: vec![retained(
                stored(1, MemoryType::Constraint, "response_language", "Chinese"),
                RetentionTier::Durable,
            )],
            query: query(
                2,
                "解释一下所有权。",
                query_scope(raw_query(&[]), Some("programming"), vec![]),
                ExpectedRetrieval {
                    mandatory: vec!["Chinese"],
                    ..ExpectedRetrieval::default()
                },
            ),
        },
        // 10: 撤销约束只结束旧 Memory，不生成反向记录。
        RecordedCase {
            name: "10 retract flight constraint",
            predicates: vec![("exclude_flight", Cardinality::Multiple)],
            relations: vec![],
            turns: vec![
                turn(
                    "s10",
                    "t1",
                    1,
                    "不要给我推荐红眼航班。",
                    vec![with_polarity(
                        raw_candidate("constraint", "exclude_flight", "red-eye"),
                        "negative",
                    )],
                    vec![ExpectedWrite::Applied(ReconciliationAction::New)],
                ),
                turn(
                    "s10",
                    "t2",
                    2,
                    "红眼航班的限制取消。",
                    vec![with_intent(
                        with_polarity(
                            raw_candidate("constraint", "exclude_flight", "red-eye"),
                            "negative",
                        ),
                        "retract",
                        Some(1),
                        None,
                    )],
                    vec![ExpectedWrite::Applied(ReconciliationAction::Retract)],
                ),
            ],
            retention_at: None,
            expected_store: vec![ended(
                stored(1, MemoryType::Constraint, "exclude_flight", "red-eye"),
                None,
                SupersedeReason::UserRetracted,
            )],
            query: query(
                3,
                "能推荐红眼航班吗？",
                raw_query(&["exclude_flight"]),
                ExpectedRetrieval::default(),
            ),
        },
        // 11: 同 slot、同 specificity 的相反偏好在查询侧报告冲突。
        RecordedCase {
            name: "11 query-side preference conflict",
            predicates: vec![("likes", Cardinality::Multiple)],
            relations: vec![],
            turns: vec![
                turn(
                    "s11",
                    "t1",
                    1,
                    "带孩子时我喜欢喝咖啡。",
                    vec![with_polarity(
                        with_scope(
                            raw_candidate("preference", "likes", "coffee"),
                            Some("food"),
                            vec![companion_children()],
                        ),
                        "positive",
                    )],
                    vec![ExpectedWrite::Applied(ReconciliationAction::New)],
                ),
                turn(
                    "s11",
                    "t2",
                    2,
                    "在日本时我不喜欢咖啡。",
                    vec![with_polarity(
                        with_scope(
                            raw_candidate("preference", "likes", "coffee"),
                            Some("food"),
                            vec![japan()],
                        ),
                        "negative",
                    )],
                    vec![ExpectedWrite::Applied(ReconciliationAction::New)],
                ),
            ],
            retention_at: None,
            expected_store: vec![
                stored(1, MemoryType::Preference, "likes", "coffee"),
                stored(2, MemoryType::Preference, "likes", "coffee"),
            ],
            query: query(
                3,
                "带孩子在日本时我喜欢咖啡吗？",
                query_scope(
                    raw_query(&["likes"]),
                    Some("food"),
                    vec![companion_children(), japan()],
                ),
                ExpectedRetrieval {
                    conflicts: 1,
                    ..ExpectedRetrieval::default()
                },
            ),
        },
        // 12: Multiple cardinality 允许两台设备同时存在。
        RecordedCase {
            name: "12 multiple owned devices",
            predicates: vec![("owns_device", Cardinality::Multiple)],
            relations: vec![],
            turns: vec![turn(
                "s12",
                "t1",
                1,
                "我有一台 MacBook 和一台 ThinkPad。",
                vec![
                    raw_candidate("fact", "owns_device", "MacBook"),
                    raw_candidate("fact", "owns_device", "ThinkPad"),
                ],
                vec![
                    ExpectedWrite::Applied(ReconciliationAction::New),
                    ExpectedWrite::Applied(ReconciliationAction::New),
                ],
            )],
            retention_at: None,
            expected_store: vec![
                stored(1, MemoryType::Fact, "owns_device", "MacBook"),
                stored(2, MemoryType::Fact, "owns_device", "ThinkPad"),
            ],
            query: query(
                2,
                "我有哪些电脑？",
                raw_query(&["owns_device"]),
                ExpectedRetrieval {
                    facts: vec!["MacBook", "ThinkPad"],
                    ..ExpectedRetrieval::default()
                },
            ),
        },
        // 13: 明确的偏好强度增强产生 Refine 版本。
        RecordedCase {
            name: "13 preference strength refinement",
            predicates: vec![("likes", Cardinality::Multiple)],
            relations: vec![],
            turns: vec![
                turn(
                    "s13",
                    "t1",
                    1,
                    "我有点喜欢咖啡。",
                    vec![with_strength(
                        with_polarity(raw_candidate("preference", "likes", "coffee"), "positive"),
                        "weak",
                    )],
                    vec![ExpectedWrite::Applied(ReconciliationAction::New)],
                ),
                turn(
                    "s13",
                    "t2",
                    2,
                    "其实我非常喜欢咖啡。",
                    vec![with_intent(
                        with_strength(
                            with_polarity(
                                raw_candidate("preference", "likes", "coffee"),
                                "positive",
                            ),
                            "strong",
                        ),
                        "refine",
                        Some(1),
                        None,
                    )],
                    vec![ExpectedWrite::Applied(ReconciliationAction::Refine)],
                ),
            ],
            retention_at: None,
            expected_store: vec![
                ended(
                    stored(1, MemoryType::Preference, "likes", "coffee"),
                    Some(2),
                    SupersedeReason::Refinement,
                ),
                replacing(stored(2, MemoryType::Preference, "likes", "coffee"), 1),
            ],
            query: query(
                3,
                "我喜欢咖啡吗？",
                raw_query(&["likes"]),
                ExpectedRetrieval {
                    preferences: vec!["coffee"],
                    ..ExpectedRetrieval::default()
                },
            ),
        },
        // 14: 相同 session/turn 的消息重放必须幂等。
        RecordedCase {
            name: "14 duplicate evidence replay",
            predicates: vec![("likes", Cardinality::Multiple)],
            relations: vec![],
            turns: vec![
                turn(
                    "s14",
                    "t1",
                    1,
                    "我喜欢咖啡。",
                    vec![with_polarity(
                        raw_candidate("preference", "likes", "coffee"),
                        "positive",
                    )],
                    vec![ExpectedWrite::Applied(ReconciliationAction::New)],
                ),
                turn(
                    "s14",
                    "t1",
                    1,
                    "我喜欢咖啡。",
                    vec![with_polarity(
                        raw_candidate("preference", "likes", "coffee"),
                        "positive",
                    )],
                    vec![ExpectedWrite::Duplicate],
                ),
            ],
            retention_at: None,
            expected_store: vec![stored(1, MemoryType::Preference, "likes", "coffee")],
            query: query(
                2,
                "我喜欢什么？",
                raw_query(&["likes"]),
                ExpectedRetrieval {
                    preferences: vec!["coffee"],
                    ..ExpectedRetrieval::default()
                },
            ),
        },
        // 15: “最近”由本地 resolver 转成 30 天区间并支持历史查询。
        RecordedCase {
            name: "15 recent episode",
            predicates: vec![("read_book", Cardinality::Multiple)],
            relations: vec![],
            turns: vec![turn(
                "s15",
                "t1",
                NOW,
                "最近我读了《Rust 程序设计语言》。",
                vec![with_temporal(
                    raw_candidate("episode", "read_book", "Rust Book"),
                    "最近",
                    None,
                )],
                vec![ExpectedWrite::Applied(ReconciliationAction::New)],
            )],
            retention_at: None,
            expected_store: vec![stored(1, MemoryType::Episode, "read_book", "Rust Book")],
            query: query(
                NOW,
                "我最近读了什么？",
                query_temporal(raw_query(&["read_book"]), "historical", Some("最近")),
                ExpectedRetrieval {
                    episodes: vec!["Rust Book"],
                    ..ExpectedRetrieval::default()
                },
            ),
        },
        // 16: “下个月”按自然月边界处理未来约束。
        RecordedCase {
            name: "16 next-month constraint",
            predicates: vec![("hotel_budget", Cardinality::Single)],
            relations: vec![],
            turns: vec![turn(
                "s16",
                "t1",
                NOW,
                "下个月酒店预算是八百元。",
                vec![with_temporal(
                    raw_candidate("constraint", "hotel_budget", "800"),
                    "下个月",
                    Some(("2026-10-01", "2026-11-01")),
                )],
                vec![ExpectedWrite::Applied(ReconciliationAction::New)],
            )],
            retention_at: None,
            expected_store: vec![stored(1, MemoryType::Constraint, "hotel_budget", "800")],
            query: query(
                NOW,
                "下个月酒店预算是多少？",
                query_temporal(raw_query(&["hotel_budget"]), "current", Some("下个月")),
                ExpectedRetrieval {
                    constraints: vec!["800"],
                    ..ExpectedRetrieval::default()
                },
            ),
        },
        // 17: “去年六月”解析为历史自然月区间。
        RecordedCase {
            name: "17 last-June historical episode",
            predicates: vec![("visited", Cardinality::Multiple)],
            relations: vec![],
            turns: vec![turn(
                "s17",
                "t1",
                NOW,
                "去年六月我去过杭州。",
                vec![with_temporal(
                    raw_candidate("episode", "visited", "Hangzhou"),
                    "去年六月",
                    Some(("2025-06-01", "2025-07-01")),
                )],
                vec![ExpectedWrite::Applied(ReconciliationAction::New)],
            )],
            retention_at: None,
            expected_store: vec![stored(1, MemoryType::Episode, "visited", "Hangzhou")],
            query: query(
                NOW,
                "去年六月我去过哪里？",
                query_temporal(raw_query(&["visited"]), "historical", Some("去年六月")),
                ExpectedRetrieval {
                    episodes: vec!["Hangzhou"],
                    ..ExpectedRetrieval::default()
                },
            ),
        },
        // 18: 事件相对时间不伪造 timestamp，只匹配相同事件引用。
        RecordedCase {
            name: "18 after-trip event reference",
            predicates: vec![("learning_since", Cardinality::Multiple)],
            relations: vec![],
            turns: vec![turn(
                "s18",
                "t1",
                NOW,
                "旅行结束后我开始学 Rust。",
                vec![with_temporal(
                    raw_candidate("fact", "learning_since", "Rust"),
                    "旅行结束后",
                    None,
                )],
                vec![ExpectedWrite::Applied(ReconciliationAction::New)],
            )],
            retention_at: None,
            expected_store: vec![stored(1, MemoryType::Fact, "learning_since", "Rust")],
            query: query(
                NOW,
                "旅行结束后我开始学什么？",
                query_temporal(
                    raw_query(&["learning_since"]),
                    "current",
                    Some("旅行结束后"),
                ),
                ExpectedRetrieval {
                    facts: vec!["Rust"],
                    ..ExpectedRetrieval::default()
                },
            ),
        },
        // 19: Latte 通过显式 IS-A 边命中 Coffee Constraint。
        RecordedCase {
            name: "19 Latte inherits Coffee constraint",
            predicates: vec![("exclude_drink", Cardinality::Multiple)],
            relations: vec![("Latte", "Coffee")],
            turns: vec![turn(
                "s19",
                "t1",
                1,
                "不要给我推荐 Coffee。",
                vec![with_polarity(
                    raw_candidate("constraint", "exclude_drink", "Coffee"),
                    "negative",
                )],
                vec![ExpectedWrite::Applied(ReconciliationAction::New)],
            )],
            retention_at: None,
            expected_store: vec![stored(1, MemoryType::Constraint, "exclude_drink", "Coffee")],
            query: query(
                2,
                "可以给我推荐 Latte 吗？",
                query_targets(raw_query(&["exclude_drink"]), &["Latte"]),
                ExpectedRetrieval {
                    constraints: vec!["Coffee"],
                    ..ExpectedRetrieval::default()
                },
            ),
        },
        // 20: 单轮四种 MemoryType 经真实 decoder 后进入独立 retrieval lanes。
        RecordedCase {
            name: "20 four memory type lanes",
            predicates: vec![
                ("works_mode", Cardinality::Single),
                ("likes_environment", Cardinality::Multiple),
                ("exclude_food", Cardinality::Multiple),
                ("visited", Cardinality::Multiple),
            ],
            relations: vec![],
            turns: vec![turn(
                "s20",
                "t1",
                1,
                "我远程办公，喜欢安静环境，不吃麸质，去过北京。",
                vec![
                    raw_candidate("fact", "works_mode", "remote"),
                    with_polarity(
                        raw_candidate("preference", "likes_environment", "quiet"),
                        "positive",
                    ),
                    with_polarity(
                        raw_candidate("constraint", "exclude_food", "gluten"),
                        "negative",
                    ),
                    raw_candidate("episode", "visited", "Beijing"),
                ],
                vec![
                    ExpectedWrite::Applied(ReconciliationAction::New),
                    ExpectedWrite::Applied(ReconciliationAction::New),
                    ExpectedWrite::Applied(ReconciliationAction::New),
                    ExpectedWrite::Applied(ReconciliationAction::New),
                ],
            )],
            retention_at: None,
            expected_store: vec![
                stored(1, MemoryType::Fact, "works_mode", "remote"),
                stored(2, MemoryType::Preference, "likes_environment", "quiet"),
                stored(3, MemoryType::Constraint, "exclude_food", "gluten"),
                stored(4, MemoryType::Episode, "visited", "Beijing"),
            ],
            query: query(
                2,
                "完整回忆一下我的工作、环境、饮食和旅行。",
                raw_query(&["works_mode", "likes_environment", "exclude_food", "visited"]),
                ExpectedRetrieval {
                    constraints: vec!["gluten"],
                    preferences: vec!["quiet"],
                    facts: vec!["remote"],
                    episodes: vec!["Beijing"],
                    ..ExpectedRetrieval::default()
                },
            ),
        },
    ]
}
