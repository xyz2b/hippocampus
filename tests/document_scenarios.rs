//! 《Personal Memory V1 测试用例整理》的逐场景可执行验收。
//!
//! 每个 `pmv1_*` 测试对应文档中的一个编号；原始中文保存在 Evidence，查询意图由
//! RuntimeContext 明确表达。当前 V1 尚不支持的目标行为保留为 ignored specification。

mod common;

use hippocampus::*;

type TestEngine = MemoryEngine<InMemoryMemoryRepository, InMemoryPredicateRegistry>;

fn owner() -> UserId {
    UserId::from("user-1")
}

fn user() -> EntityId {
    EntityId::from("user-entity")
}

fn evidence(session: &str, turn: &str, ordinal: usize, quote: &str) -> Evidence {
    Evidence::new(
        EvidenceId(format!("{session}:{turn}:{ordinal}")),
        SessionId::from(session),
        TurnId::from(turn),
        quote,
    )
}

fn candidate(
    subject: EntityId,
    memory_type: MemoryType,
    predicate: &str,
    value: impl Into<MemoryValue>,
    at: i64,
    evidence: Evidence,
) -> CandidateMemory {
    CandidateMemory::new(
        owner(),
        subject,
        memory_type,
        Predicate::from(predicate),
        value.into(),
        Timestamp(at),
        evidence,
    )
}

fn engine(definitions: &[(&str, Cardinality)]) -> TestEngine {
    let mut registry = InMemoryPredicateRegistry::default();
    for (predicate, cardinality) in definitions {
        registry.register(Predicate::from(*predicate), *cardinality);
    }
    MemoryEngine::new(InMemoryMemoryRepository::default(), registry)
}

fn ingest(
    engine: &mut TestEngine,
    candidate: CandidateMemory,
    expected: ReconciliationAction,
) -> IngestReport {
    let report = engine
        .ingest_with_trace(common::grounded(candidate))
        .unwrap();
    assert_eq!(report.trace.admission, AdmissionDecision::Admit);
    assert!(!report.trace.admission_reason.is_empty());
    assert_eq!(
        report
            .trace
            .reconciliation
            .as_ref()
            .map(|decision| decision.action),
        Some(expected)
    );
    assert!(matches!(
        report.outcome,
        IngestOutcome::Applied { action, .. } if action == expected
    ));
    report
}

fn inserted_id(report: &IngestReport) -> MemoryId {
    match report.outcome {
        IngestOutcome::Applied {
            memory_id: Some(id),
            ..
        } => id,
        ref other => panic!("expected inserted memory, got {other:?}"),
    }
}

fn stored(engine: &TestEngine, id: MemoryId) -> Memory {
    engine.repository.get(id).unwrap().unwrap()
}

fn query(at: i64, domain: Option<&str>, predicates: &[&str]) -> RuntimeContext {
    let mut context = RuntimeContext::current(owner(), Timestamp(at));
    context.domain = domain.map(Domain::from);
    context.entities.push(user());
    context.predicates = predicates
        .iter()
        .map(|predicate| Predicate::from(*predicate))
        .collect();
    context
}

fn retrieve(engine: &mut TestEngine, context: &RuntimeContext) -> RetrievalResult {
    DefaultMemoryRetriever::new(&mut engine.repository, &engine.predicates)
        .retrieve(context)
        .unwrap()
}

fn decision(result: &RetrievalResult, id: MemoryId) -> RetrievalDecision {
    result
        .trace
        .iter()
        .find(|entry| entry.memory_id == id)
        .unwrap_or_else(|| panic!("missing retrieval trace for memory {}", id.0))
        .decision
}

fn assert_empty(result: &RetrievalResult) {
    assert!(result.mandatory.is_empty());
    assert!(result.constraints.is_empty());
    assert!(result.preferences.is_empty());
    assert!(result.facts.is_empty());
    assert!(result.episodes.is_empty());
    assert!(result.conflicts.is_empty());
}

fn food_scope(conditions: Vec<ScopeCondition>) -> Scope {
    Scope::new(Some(Domain::from("food")), conditions)
}

fn travel_scope(conditions: Vec<ScopeCondition>) -> Scope {
    Scope::new(Some(Domain::from("travel")), conditions)
}

/// Fault-injection helper for PMV1-14; normal writes must go through MemoryEngine.
fn active_memory(id: u64, candidate: CandidateMemory) -> Memory {
    let subject_id = candidate.subject_id().unwrap().clone();
    Memory {
        id: MemoryId(id),
        owner_id: candidate.owner_id,
        evidence: candidate.evidence,
        subject_id,
        memory_type: candidate.memory_type,
        predicate: candidate.predicate,
        value: candidate.value,
        polarity: candidate.polarity,
        strength: candidate.strength,
        scope: candidate.scope,
        retention_tier: candidate.retention_tier,
        status: MemoryStatus::Active,
        temporal: candidate.temporal,
        extraction_confidence: candidate.extraction_confidence,
        certainty: candidate.certainty,
        supersedes: None,
        superseded_by: None,
        supersede_reason: None,
        explicit_memory_request: candidate.explicit_memory_request,
        reinforcement_count: 0,
        retrieval_count: 0,
        last_reinforced_at: None,
        last_retrieved_at: None,
        tags: candidate.tags,
    }
}

// PMV1-01：偏好变化形成版本链，早餐 Constraint 保持独立，并分 lane 召回。
#[test]
fn pmv1_01_preference_change_and_breakfast_constraint() {
    let mut engine = engine(&[
        ("likes", Cardinality::Multiple),
        ("exclude_drink", Cardinality::Multiple),
    ]);
    let mut likes = candidate(
        user(),
        MemoryType::Preference,
        "likes",
        "coffee",
        1,
        evidence("S1", "T1", 0, "我挺喜欢咖啡的。"),
    );
    likes.polarity = Some(Polarity::Positive);
    likes.scope = food_scope(vec![]);
    let old_id = inserted_id(&ingest(&mut engine, likes, ReconciliationAction::New));

    let mut dislikes = candidate(
        user(),
        MemoryType::Preference,
        "likes",
        "coffee",
        2,
        evidence("S2", "T1", 0, "我现在有点不喜欢咖啡了。"),
    );
    dislikes.polarity = Some(Polarity::Negative);
    dislikes.scope = food_scope(vec![]);
    dislikes.intent = CandidateIntent::Supersede {
        target: Some(old_id),
        reason: SupersedeReason::PreferenceChanged,
    };
    let current_id = inserted_id(&ingest(
        &mut engine,
        dislikes,
        ReconciliationAction::Supersede,
    ));

    let mut constraint = candidate(
        user(),
        MemoryType::Constraint,
        "exclude_drink",
        "coffee",
        3,
        evidence("S3", "T1", 0, "以后早餐别给我推荐咖啡。"),
    );
    constraint.polarity = Some(Polarity::Negative);
    constraint.scope = food_scope(vec![ScopeCondition::Activity("breakfast".to_owned())]);
    let constraint_id = inserted_id(&ingest(&mut engine, constraint, ReconciliationAction::New));

    let old = stored(&engine, old_id);
    assert_eq!(old.status, MemoryStatus::Superseded);
    assert_eq!(old.superseded_by, Some(current_id));
    assert_eq!(
        old.supersede_reason,
        Some(SupersedeReason::PreferenceChanged)
    );
    assert_eq!(old.temporal.valid_to.unwrap().at, Timestamp(2));

    let mut context = query(4, Some("food"), &["likes", "exclude_drink"]);
    context.conditions = vec![ScopeCondition::Activity("breakfast".to_owned())];
    let result = retrieve(&mut engine, &context);
    assert_eq!(result.preferences[0].id, current_id);
    assert_eq!(result.constraints[0].id, constraint_id);
    assert_eq!(
        decision(&result, old_id),
        RetrievalDecision::Filtered(RetrievalFilterReason::Status)
    );
}

// PMV1-02：家庭旅行一般规则与日本亲子游例外按 Scope specificity 处理。
#[test]
fn pmv1_02_family_travel_rule_and_japan_exception() {
    let mut engine = engine(&[("homestay", Cardinality::Multiple)]);
    let children = ScopeCondition::Companion(EntityOrRole::Role(CompanionRole::Children));
    let mut general = candidate(
        user(),
        MemoryType::Constraint,
        "homestay",
        "homestay",
        1,
        evidence("S1", "T1", 0, "以后家庭旅行不要给我推荐民宿。"),
    );
    general.polarity = Some(Polarity::Negative);
    general.scope = travel_scope(vec![children.clone()]);
    let general_id = inserted_id(&ingest(&mut engine, general, ReconciliationAction::New));

    let mut japan = candidate(
        user(),
        MemoryType::Constraint,
        "homestay",
        "homestay",
        2,
        evidence("S1", "T2", 0, "不过带孩子去日本，温泉民宿可以考虑。"),
    );
    japan.polarity = Some(Polarity::Positive);
    japan.scope = travel_scope(vec![
        children.clone(),
        ScopeCondition::Location(LocationScope::Country("JP".to_owned())),
    ]);
    let japan_id = inserted_id(&ingest(&mut engine, japan, ReconciliationAction::New));

    let mut yunnan_query = query(3, Some("travel"), &["homestay"]);
    yunnan_query.conditions = vec![
        children.clone(),
        ScopeCondition::Location(LocationScope::City("Yunnan".to_owned())),
    ];
    let yunnan = retrieve(&mut engine, &yunnan_query);
    assert_eq!(yunnan.constraints[0].id, general_id);
    assert_eq!(
        decision(&yunnan, japan_id),
        RetrievalDecision::Filtered(RetrievalFilterReason::Scope)
    );

    let mut japan_query = query(4, Some("travel"), &["homestay"]);
    japan_query.conditions = vec![
        children,
        ScopeCondition::Location(LocationScope::Country("JP".to_owned())),
    ];
    let japan_result = retrieve(&mut engine, &japan_query);
    assert_eq!(japan_result.constraints[0].id, japan_id);
    assert_eq!(
        decision(&japan_result, general_id),
        RetrievalDecision::Filtered(RetrievalFilterReason::OverriddenBy(japan_id))
    );
}

// PMV1-03：“算了……就行”将 food 一般约束替换为早餐约束。
#[test]
fn pmv1_03_scope_refinement_replaces_general_constraint() {
    let mut engine = engine(&[("exclude_drink", Cardinality::Multiple)]);
    let mut general = candidate(
        user(),
        MemoryType::Constraint,
        "exclude_drink",
        "coffee",
        1,
        evidence("S1", "T1", 0, "以后都别给我推荐咖啡。"),
    );
    general.polarity = Some(Polarity::Negative);
    general.scope = food_scope(vec![]);
    let old_id = inserted_id(&ingest(&mut engine, general, ReconciliationAction::New));

    let mut breakfast = candidate(
        user(),
        MemoryType::Constraint,
        "exclude_drink",
        "coffee",
        2,
        evidence("S1", "T2", 0, "算了，早餐别推荐就行。"),
    );
    breakfast.polarity = Some(Polarity::Negative);
    breakfast.scope = food_scope(vec![ScopeCondition::Activity("breakfast".to_owned())]);
    breakfast.intent = CandidateIntent::Supersede {
        target: Some(old_id),
        reason: SupersedeReason::ScopeRefined,
    };
    let current_id = inserted_id(&ingest(
        &mut engine,
        breakfast,
        ReconciliationAction::Supersede,
    ));

    let mut dinner_query = query(3, Some("food"), &["exclude_drink"]);
    dinner_query.conditions = vec![ScopeCondition::Activity("dinner".to_owned())];
    assert_empty(&retrieve(&mut engine, &dinner_query));

    let mut breakfast_query = query(4, Some("food"), &["exclude_drink"]);
    breakfast_query.conditions = vec![ScopeCondition::Activity("breakfast".to_owned())];
    let result = retrieve(&mut engine, &breakfast_query);
    assert_eq!(result.constraints[0].id, current_id);
    assert_eq!(
        stored(&engine, old_id).supersede_reason,
        Some(SupersedeReason::ScopeRefined)
    );
}

// PMV1-04：Single residence 状态变化保留当前值和可查询的历史值。
#[test]
fn pmv1_04_residence_state_change_supports_history() {
    let mut engine = engine(&[("primary_residence", Cardinality::Single)]);
    let mut nanjing = candidate(
        user(),
        MemoryType::Fact,
        "primary_residence",
        "Nanjing",
        1,
        evidence("S1", "T1", 0, "我现在住南京。"),
    );
    nanjing.temporal.valid_from = Some(DatedValue {
        at: Timestamp(1),
        source: TemporalSource::Explicit,
    });
    let old_id = inserted_id(&ingest(&mut engine, nanjing, ReconciliationAction::New));

    let mut fuyang = candidate(
        user(),
        MemoryType::Fact,
        "primary_residence",
        "Fuyang",
        10,
        evidence("S2", "T1", 0, "我搬到阜阳了。"),
    );
    fuyang.temporal.valid_from = Some(DatedValue {
        at: Timestamp(10),
        source: TemporalSource::Explicit,
    });
    fuyang.intent = CandidateIntent::Supersede {
        target: Some(old_id),
        reason: SupersedeReason::StateChanged,
    };
    let current_id = inserted_id(&ingest(
        &mut engine,
        fuyang,
        ReconciliationAction::Supersede,
    ));

    let current = retrieve(&mut engine, &query(20, None, &["primary_residence"]));
    assert_eq!(current.facts[0].id, current_id);
    let mut historical_query = query(5, None, &["primary_residence"]);
    historical_query.temporal_intent = TemporalIntent::Historical;
    let historical = retrieve(&mut engine, &historical_query);
    assert_eq!(historical.facts[0].id, old_id);
    assert_eq!(
        decision(&historical, current_id),
        RetrievalDecision::Filtered(RetrievalFilterReason::Temporal)
    );
}

// PMV1-05：Correction 结束的错误事件不能在 historical 查询中复活。
#[test]
fn pmv1_05_correction_is_not_historical_truth() {
    let now = Timestamp(1_788_494_400);
    let mut engine = engine(&[("visited", Cardinality::Multiple)]);
    let mut july = candidate(
        user(),
        MemoryType::Episode,
        "visited",
        "Yunnan",
        now.0,
        evidence("S1", "T1", 0, "我去年 7 月去了云南。"),
    );
    july.temporal
        .apply_resolution(resolve_temporal_expression("去年七月", now, 8 * 60 * 60).unwrap());
    let wrong_id = inserted_id(&ingest(&mut engine, july, ReconciliationAction::New));

    let mut june = candidate(
        user(),
        MemoryType::Episode,
        "visited",
        "Yunnan",
        now.0 + 1,
        evidence("S1", "T2", 0, "说错了，是 6 月。"),
    );
    june.temporal
        .apply_resolution(resolve_temporal_expression("去年六月", now, 8 * 60 * 60).unwrap());
    june.intent = CandidateIntent::Supersede {
        target: Some(wrong_id),
        reason: SupersedeReason::Correction,
    };
    let corrected_id = inserted_id(&ingest(&mut engine, june, ReconciliationAction::Supersede));

    let mut historical = query(now.0 + 2, Some("travel"), &["visited"]);
    historical.domain = None;
    historical.temporal_intent = TemporalIntent::Historical;
    historical.temporal_filter =
        Some(resolve_temporal_expression("去年六月", now, 8 * 60 * 60).unwrap());
    let result = retrieve(&mut engine, &historical);
    assert_eq!(result.episodes[0].id, corrected_id);
    assert_eq!(
        decision(&result, wrong_id),
        RetrievalDecision::Filtered(RetrievalFilterReason::Status)
    );
}

// PMV1-06：Multiple 设备并存；售出目前以 Retract/UserRetracted 近似结束关系。
#[test]
fn pmv1_06_multiple_devices_and_current_sale_approximation() {
    let macbook = EntityId::from("macbook-2018");
    let thinkpad = EntityId::from("thinkpad");
    let mut entities = InMemoryEntityRepository::default();
    entities.upsert(Entity {
        id: macbook.clone(),
        owner_id: owner(),
        entity_type: EntityType::Device,
        canonical_name: "MacBook".to_owned(),
        aliases: vec!["旧 MacBook".to_owned(), "以前那台".to_owned()],
    });
    assert_eq!(
        entities.resolve(&owner(), "旧 MacBook"),
        EntityResolution::Resolved(macbook.clone())
    );

    let mut engine = engine(&[("owns_device", Cardinality::Multiple)]);
    let first = candidate(
        user(),
        MemoryType::Fact,
        "owns_device",
        MemoryValue::Entity(macbook.clone()),
        1,
        evidence("S1", "T1", 0, "我有一台 MacBook。"),
    );
    let mac_id = inserted_id(&ingest(&mut engine, first, ReconciliationAction::New));
    let second = candidate(
        user(),
        MemoryType::Fact,
        "owns_device",
        MemoryValue::Entity(thinkpad),
        2,
        evidence("S2", "T1", 0, "我又买了一台 ThinkPad。"),
    );
    let thinkpad_id = inserted_id(&ingest(&mut engine, second, ReconciliationAction::New));

    let mut sold = candidate(
        user(),
        MemoryType::Fact,
        "owns_device",
        MemoryValue::Entity(macbook.clone()),
        3,
        evidence("S3", "T1", 0, "旧 MacBook 已经卖掉了。"),
    );
    sold.intent = CandidateIntent::Retract {
        target: Some(mac_id),
    };
    ingest(&mut engine, sold, ReconciliationAction::Retract);
    assert_eq!(
        stored(&engine, mac_id).supersede_reason,
        Some(SupersedeReason::UserRetracted)
    );

    let current = retrieve(&mut engine, &query(4, None, &["owns_device"]));
    assert_eq!(current.facts.len(), 1);
    assert_eq!(current.facts[0].id, thinkpad_id);

    let mut old_device_query = query(4, None, &["device_issue"]);
    old_device_query.entities = vec![macbook];
    assert_empty(&retrieve(&mut engine, &old_device_query));
}

// PMV1-07：歧义 subject 先 staging，唯一澄清后携带两轮 Evidence 入库。
#[test]
fn pmv1_07_ambiguous_child_is_staged_then_clarified() {
    let older = EntityId::from("child-older");
    let younger = EntityId::from("child-younger");
    let mut entities = InMemoryEntityRepository::default();
    for (id, name, extra_alias) in [
        (older.clone(), "大孩子", "大的"),
        (younger.clone(), "小孩子", "小的"),
    ] {
        entities.upsert(Entity {
            id,
            owner_id: owner(),
            entity_type: EntityType::Person,
            canonical_name: name.to_owned(),
            aliases: vec!["他".to_owned(), "儿子".to_owned(), extra_alias.to_owned()],
        });
    }
    let mut engine = engine(&[("likes", Cardinality::Multiple)]);
    let mut unresolved = candidate(
        younger.clone(),
        MemoryType::Preference,
        "likes",
        "swimming",
        1,
        evidence("S1", "T1", 0, "他不喜欢游泳。"),
    );
    unresolved.subject = CandidateSubject::Unresolved("他".to_owned());
    unresolved.polarity = Some(Polarity::Negative);
    let pending_id = match engine
        .ingest_or_stage(common::grounded(unresolved), &entities)
        .unwrap()
    {
        CandidateIngestOutcome::Pending {
            staged_id,
            resolution: EntityResolution::Ambiguous(ids),
        } => {
            assert_eq!(ids.len(), 2);
            staged_id
        }
        other => panic!("expected ambiguous staging, got {other:?}"),
    };
    assert!(
        engine
            .repository
            .list_by_owner(&owner())
            .unwrap()
            .is_empty()
    );

    let outcome = engine
        .clarify_staged(
            pending_id,
            "小的",
            evidence("S1", "T2", 0, "我是说小的。"),
            &entities,
        )
        .unwrap();
    let id = match outcome {
        CandidateIngestOutcome::Ingested(IngestOutcome::Applied {
            action: ReconciliationAction::New,
            memory_id: Some(id),
            ..
        }) => id,
        other => panic!("expected clarified write, got {other:?}"),
    };
    let memory = stored(&engine, id);
    assert_eq!(memory.subject_id, younger);
    assert_eq!(memory.evidence.len(), 2);
    assert!(engine.staging.is_empty());
}

// PMV1-08：用户 certainty 低仍可保存；后续撤销过度概括并新增更精确 Fact。
#[test]
fn pmv1_08_certainty_is_separate_from_extraction_confidence() {
    let child = EntityId::from("child-older");
    let mut engine = engine(&[
        ("attitude_toward_kindergarten", Cardinality::Single),
        ("willingness_to_attend_kindergarten", Cardinality::Single),
    ]);
    let mut attitude = candidate(
        child.clone(),
        MemoryType::Fact,
        "attitude_toward_kindergarten",
        "dislikes",
        1,
        evidence("S1", "T1", 0, "我儿子最近好像不太喜欢幼儿园。"),
    );
    attitude.certainty = Confidence::Low;
    attitude.extraction_confidence = Confidence::High;
    attitude.retention_tier = RetentionTier::ShortTerm;
    let old_id = inserted_id(&ingest(&mut engine, attitude, ReconciliationAction::New));

    let mut retract = candidate(
        child.clone(),
        MemoryType::Fact,
        "attitude_toward_kindergarten",
        "dislikes",
        2,
        evidence("S1", "T2", 0, "我觉得可能只是最近不想去，不一定是不喜欢。"),
    );
    retract.intent = CandidateIntent::Retract {
        target: Some(old_id),
    };
    ingest(&mut engine, retract, ReconciliationAction::Retract);

    let mut willingness = candidate(
        child.clone(),
        MemoryType::Fact,
        "willingness_to_attend_kindergarten",
        "low",
        2,
        evidence("S1", "T2", 1, "我觉得可能只是最近不想去，不一定是不喜欢。"),
    );
    willingness.certainty = Confidence::Low;
    willingness.retention_tier = RetentionTier::ShortTerm;
    let new_id = inserted_id(&ingest(&mut engine, willingness, ReconciliationAction::New));

    let mut context = query(
        3,
        None,
        &[
            "attitude_toward_kindergarten",
            "willingness_to_attend_kindergarten",
        ],
    );
    context.entities = vec![child];
    let result = retrieve(&mut engine, &context);
    assert_eq!(result.facts[0].id, new_id);
    assert_eq!(
        decision(&result, old_id),
        RetrievalDecision::Filtered(RetrievalFilterReason::Status)
    );
}

// PMV1-09：两个独立 turn 的强化将 ShortTerm 提升为 LongTerm。
#[test]
fn pmv1_09_independent_reinforcement_promotes_retention() {
    let mut engine = engine(&[("learning", Cardinality::Multiple)]);
    let make = |session: &str, turn: &str, at: i64, quote: &str| {
        let mut memory = candidate(
            user(),
            MemoryType::Fact,
            "learning",
            "Rust",
            at,
            evidence(session, turn, 0, quote),
        );
        memory.retention_tier = RetentionTier::ShortTerm;
        memory.temporal.valid_from = Some(DatedValue {
            at: Timestamp(0),
            source: TemporalSource::Inferred,
        });
        memory.temporal.valid_to = Some(DatedValue {
            at: Timestamp(100),
            source: TemporalSource::Inferred,
        });
        memory
    };
    let id = inserted_id(&ingest(
        &mut engine,
        make("S1", "T1", 1, "我最近开始学 Rust。"),
        ReconciliationAction::New,
    ));
    let mut repeated = make("S2", "T1", 2, "最近还在学 Rust。");
    repeated.retention_tier = RetentionTier::LongTerm;
    ingest(&mut engine, repeated, ReconciliationAction::Reinforce);
    assert_eq!(stored(&engine, id).retention_tier, RetentionTier::ShortTerm);
    let mut established = make("S3", "T1", 3, "现在很多项目我都开始用 Rust 写了。");
    established.retention_tier = RetentionTier::Durable;
    ingest(&mut engine, established, ReconciliationAction::Reinforce);
    let memory = stored(&engine, id);
    assert_eq!(memory.retention_tier, RetentionTier::LongTerm);
    assert_eq!(memory.reinforcement_count, 2);
    assert_eq!(memory.evidence.len(), 3);
}

// PMV1-10：Task-scoped ShortTerm 预算在截止时间前按 Scope 过滤，之后自动 Expired。
#[test]
fn pmv1_10_task_scoped_budget_expires() {
    let mut engine = engine(&[("hotel_budget", Cardinality::Single)]);
    let mut budget = candidate(
        user(),
        MemoryType::Constraint,
        "hotel_budget",
        MemoryValue::Integer(800),
        1,
        evidence("S1", "T1", 0, "这次去上海酒店预算控制在 800。"),
    );
    budget.scope = travel_scope(vec![ScopeCondition::Task(TaskId::from("shanghai-trip"))]);
    budget.retention_tier = RetentionTier::ShortTerm;
    budget.temporal.expires_at = Some(Timestamp(10));
    let id = inserted_id(&ingest(&mut engine, budget, ReconciliationAction::New));

    let mut beijing = query(5, Some("travel"), &["hotel_budget"]);
    beijing.task_id = Some(TaskId::from("beijing-trip"));
    let before_expiry = retrieve(&mut engine, &beijing);
    assert_eq!(
        decision(&before_expiry, id),
        RetrievalDecision::Filtered(RetrievalFilterReason::Scope)
    );

    engine
        .run_retention_for_owner(&owner(), Timestamp(11))
        .unwrap();
    assert_eq!(stored(&engine, id).status, MemoryStatus::Expired);
    let after_expiry = retrieve(&mut engine, &beijing);
    assert_eq!(
        decision(&after_expiry, id),
        RetrievalDecision::Filtered(RetrievalFilterReason::Status)
    );
}

// PMV1-11：Global + Durable + Active 语言约束始终进入 mandatory lane。
#[test]
fn pmv1_11_global_durable_rule_is_mandatory() {
    let mut engine = engine(&[("response_language", Cardinality::Single)]);
    let mut language = candidate(
        user(),
        MemoryType::Constraint,
        "response_language",
        "Chinese",
        1,
        evidence("S1", "T1", 0, "以后都用中文和我说。"),
    );
    language.retention_tier = RetentionTier::Durable;
    language
        .temporal
        .apply_resolution(resolve_temporal_expression("以后", Timestamp(1), 0).unwrap());
    let id = inserted_id(&ingest(&mut engine, language, ReconciliationAction::New));

    let mut context = query(100, Some("programming"), &[]);
    context.entities = vec![EntityId::from("unrelated")];
    let result = retrieve(&mut engine, &context);
    assert_eq!(result.mandatory[0].id, id);
    assert_eq!(decision(&result, id), RetrievalDecision::Mandatory);
    let memory = stored(&engine, id);
    assert_eq!(memory.retrieval_count, 1);
    assert_eq!(memory.reinforcement_count, 0);
}

// PMV1-12：AssistantInference 被拒绝，用户确认后才形成正式 Preference。
#[test]
fn pmv1_12_assistant_inference_requires_user_confirmation() {
    let mut engine = engine(&[("prefers_hotel", Cardinality::Multiple)]);
    let mut inferred = candidate(
        user(),
        MemoryType::Preference,
        "prefers_hotel",
        "Atour",
        1,
        evidence("assistant", "T1", 0, "看起来你应该挺喜欢亚朵。"),
    );
    inferred.source = CandidateSource::AssistantInference;
    let rejected = engine
        .ingest_with_trace(common::grounded(inferred))
        .unwrap();
    assert_eq!(
        rejected.trace.admission,
        AdmissionDecision::Reject("source is not user evidence")
    );

    let mut confirmed = candidate(
        user(),
        MemoryType::Preference,
        "prefers_hotel",
        "Atour",
        2,
        evidence("S1", "T1", 0, "对，我确实一般优先住亚朵。"),
    );
    confirmed.source = CandidateSource::UserConfirmed;
    confirmed.polarity = Some(Polarity::Positive);
    confirmed.scope = travel_scope(vec![ScopeCondition::Activity("hotel".to_owned())]);
    let id = inserted_id(&ingest(&mut engine, confirmed, ReconciliationAction::New));
    assert_eq!(engine.repository.list_by_owner(&owner()).unwrap().len(), 1);

    let mut context = query(3, Some("travel"), &["prefers_hotel"]);
    context.conditions = vec![ScopeCondition::Activity("hotel".to_owned())];
    assert_eq!(retrieve(&mut engine, &context).preferences[0].id, id);
}

// PMV1-13：撤销航班约束只结束旧记录，不生成反向 Preference。
#[test]
fn pmv1_13_retract_constraint_does_not_create_preference() {
    let mut engine = engine(&[("exclude_flight", Cardinality::Multiple)]);
    let mut excluded = candidate(
        user(),
        MemoryType::Constraint,
        "exclude_flight",
        "red-eye",
        1,
        evidence("S1", "T1", 0, "不要红眼航班。"),
    );
    excluded.polarity = Some(Polarity::Negative);
    excluded.scope = travel_scope(vec![]);
    let id = inserted_id(&ingest(&mut engine, excluded, ReconciliationAction::New));

    let mut retract = candidate(
        user(),
        MemoryType::Constraint,
        "exclude_flight",
        "red-eye",
        2,
        evidence("S1", "T2", 0, "这个限制取消吧，以后红眼也可以。"),
    );
    retract.scope = travel_scope(vec![]);
    retract.intent = CandidateIntent::Retract { target: Some(id) };
    ingest(&mut engine, retract, ReconciliationAction::Retract);

    let result = retrieve(&mut engine, &query(3, Some("travel"), &["exclude_flight"]));
    assert_empty(&result);
    assert_eq!(
        decision(&result, id),
        RetrievalDecision::Filtered(RetrievalFilterReason::Status)
    );
    assert_eq!(engine.repository.list_by_owner(&owner()).unwrap().len(), 1);
}

// PMV1-14：损坏 Store 中同层级相反约束必须 query-time conflict，不能随机选边。
#[test]
fn pmv1_14_query_time_conflict_is_a_second_line_of_defense() {
    let mut engine = engine(&[("exclude_drink", Cardinality::Multiple)]);
    let make = |id: u64, polarity: Polarity, quote: &str| {
        let mut candidate = candidate(
            user(),
            MemoryType::Constraint,
            "exclude_drink",
            "coffee",
            id as i64,
            evidence("fault", &format!("T{id}"), 0, quote),
        );
        candidate.polarity = Some(polarity);
        candidate.scope = food_scope(vec![ScopeCondition::Activity("breakfast".to_owned())]);
        active_memory(id, candidate)
    };
    engine
        .repository
        .insert(make(1, Polarity::Negative, "早餐不要咖啡。"))
        .unwrap();
    engine
        .repository
        .insert(make(2, Polarity::Positive, "早餐可以咖啡。"))
        .unwrap();

    let mut context = query(3, Some("food"), &["exclude_drink"]);
    context.conditions = vec![ScopeCondition::Activity("breakfast".to_owned())];
    context.targets = vec![MemoryValue::from("coffee")];
    let result = retrieve(&mut engine, &context);
    assert!(result.constraints.is_empty());
    assert_eq!(result.conflicts.len(), 1);
    for id in [MemoryId(1), MemoryId(2)] {
        assert!(matches!(
            decision(&result, id),
            RetrievalDecision::Conflict {
                reason: RetrievalConflictReason::EqualSpecificity,
                ..
            }
        ));
    }
}

// PMV1-15：前一轮上海旅行计划不得污染后一轮酒店偏好的 Scope。
#[test]
fn pmv1_15_conversation_context_does_not_leak_into_scope() {
    let mut engine = engine(&[("prefers_hotel", Cardinality::Multiple)]);
    let mut plan = candidate(
        user(),
        MemoryType::Episode,
        "travel_plan",
        "Shanghai",
        1,
        evidence("S1", "T1", 0, "我准备去上海旅游。"),
    );
    plan.source = CandidateSource::GoalOrPlan;
    let rejected = engine.ingest_with_trace(common::grounded(plan)).unwrap();
    assert!(matches!(rejected.outcome, IngestOutcome::Rejected(_)));

    let mut preference = candidate(
        user(),
        MemoryType::Preference,
        "prefers_hotel",
        "Atour",
        2,
        evidence("S1", "T2", 0, "我挺喜欢亚朵的。"),
    );
    preference.polarity = Some(Polarity::Positive);
    preference.scope = travel_scope(vec![ScopeCondition::Activity("hotel".to_owned())]);
    let id = inserted_id(&ingest(&mut engine, preference, ReconciliationAction::New));
    assert!(
        !stored(&engine, id)
            .scope
            .conditions
            .contains(&ScopeCondition::Location(LocationScope::City(
                "Shanghai".to_owned()
            )))
    );

    let mut beijing = query(3, Some("travel"), &["prefers_hotel"]);
    beijing.conditions = vec![
        ScopeCondition::Activity("hotel".to_owned()),
        ScopeCondition::Location(LocationScope::City("Beijing".to_owned())),
    ];
    assert_eq!(retrieve(&mut engine, &beijing).preferences[0].id, id);
}

// PMV1-16：带孩子租车的条件化偏好不能用于独自出行。
#[test]
fn pmv1_16_specific_scope_is_not_generalized() {
    let mut engine = engine(&[("preferred_transport", Cardinality::Single)]);
    let mut preference = candidate(
        user(),
        MemoryType::Preference,
        "preferred_transport",
        "rental_car",
        1,
        evidence("S1", "T1", 0, "带孩子的时候我一般都租车。"),
    );
    preference.polarity = Some(Polarity::Positive);
    preference.scope = travel_scope(vec![ScopeCondition::Companion(EntityOrRole::Role(
        CompanionRole::Children,
    ))]);
    let id = inserted_id(&ingest(&mut engine, preference, ReconciliationAction::New));

    let mut alone = query(2, Some("travel"), &["preferred_transport"]);
    alone.conditions = vec![
        ScopeCondition::Companion(EntityOrRole::Role(CompanionRole::Alone)),
        ScopeCondition::Location(LocationScope::City("Tokyo".to_owned())),
    ];
    let result = retrieve(&mut engine, &alone);
    assert_empty(&result);
    assert_eq!(
        decision(&result, id),
        RetrievalDecision::Filtered(RetrievalFilterReason::Scope)
    );
}

// PMV1-17：显式目标命中子偏好；注册关系时另检查父类回退与例外覆盖。
#[test]
fn pmv1_17_specific_preference_should_override_parent_preference() {
    let mut engine = engine(&[("likes", Cardinality::Multiple)]);
    let mut coffee = candidate(
        user(),
        MemoryType::Preference,
        "likes",
        "Coffee",
        1,
        evidence("S1", "T1", 0, "我不喜欢咖啡。"),
    );
    coffee.polarity = Some(Polarity::Negative);
    coffee.scope = food_scope(vec![]);
    ingest(&mut engine, coffee, ReconciliationAction::New);
    let mut latte = candidate(
        user(),
        MemoryType::Preference,
        "likes",
        "Latte",
        2,
        evidence("S1", "T2", 0, "不过拿铁还可以。"),
    );
    latte.polarity = Some(Polarity::Positive);
    latte.strength = Some(Strength::Weak);
    latte.scope = food_scope(vec![]);
    let latte_id = inserted_id(&ingest(&mut engine, latte, ReconciliationAction::New));

    let mut context = query(3, Some("food"), &["likes"]);
    context.targets = vec![MemoryValue::from("Latte")];
    let result = retrieve(&mut engine, &context);
    assert_eq!(result.preferences.len(), 1);
    assert_eq!(result.preferences[0].id, latte_id);
}

// PMV1-18：Coffee Constraint 通过显式 IS-A 覆盖 Latte，并先于 Preference 生效。
#[test]
fn pmv1_18_parent_constraint_covers_child_target() {
    let mut engine = engine(&[
        ("likes", Cardinality::Multiple),
        ("exclude_drink", Cardinality::Multiple),
    ]);
    let mut preference = candidate(
        user(),
        MemoryType::Preference,
        "likes",
        "Latte",
        1,
        evidence("S1", "T1", 0, "我喜欢拿铁。"),
    );
    preference.polarity = Some(Polarity::Positive);
    preference.scope = food_scope(vec![]);
    let preference_id = inserted_id(&ingest(&mut engine, preference, ReconciliationAction::New));
    let mut constraint = candidate(
        user(),
        MemoryType::Constraint,
        "exclude_drink",
        "Coffee",
        2,
        evidence("S1", "T2", 0, "早餐不要咖啡类饮料。"),
    );
    constraint.polarity = Some(Polarity::Negative);
    constraint.scope = food_scope(vec![ScopeCondition::Activity("breakfast".to_owned())]);
    let constraint_id = inserted_id(&ingest(&mut engine, constraint, ReconciliationAction::New));
    let mut relations = InMemorySemanticRelationRegistry::default();
    relations.register_is_a(MemoryValue::from("Latte"), MemoryValue::from("Coffee"));

    let mut context = query(3, Some("food"), &["likes", "exclude_drink"]);
    context.conditions = vec![ScopeCondition::Activity("breakfast".to_owned())];
    context.targets = vec![MemoryValue::from("Latte")];
    let result = DefaultMemoryRetriever::new(&mut engine.repository, &engine.predicates)
        .with_semantic_relations(&relations)
        .retrieve(&context)
        .unwrap();
    assert_eq!(result.constraints[0].id, constraint_id);
    assert_eq!(result.preferences[0].id, preference_id);
}

// PMV1-19：builder 显式提供相关 child Entity 时，孩子 Fact 与同行约束都能召回。
#[test]
fn pmv1_19_context_related_entity_is_retrieved() {
    let child = EntityId::from("child-001");
    let mut engine = engine(&[
        ("age", Cardinality::Single),
        ("homestay", Cardinality::Multiple),
    ]);
    let age = candidate(
        child.clone(),
        MemoryType::Fact,
        "age",
        MemoryValue::Integer(3),
        1,
        evidence("S1", "T1", 0, "我孩子快三岁了。"),
    );
    let age_id = inserted_id(&ingest(&mut engine, age, ReconciliationAction::New));
    let mut homestay = candidate(
        user(),
        MemoryType::Constraint,
        "homestay",
        "homestay",
        2,
        evidence("S1", "T2", 0, "带孩子旅行不要推荐民宿。"),
    );
    homestay.polarity = Some(Polarity::Negative);
    homestay.scope = travel_scope(vec![ScopeCondition::Companion(EntityOrRole::Role(
        CompanionRole::Children,
    ))]);
    let constraint_id = inserted_id(&ingest(&mut engine, homestay, ReconciliationAction::New));

    let mut with_child = query(3, Some("travel"), &["age", "homestay"]);
    with_child.entities.push(child.clone());
    with_child.conditions = vec![
        ScopeCondition::Companion(EntityOrRole::Role(CompanionRole::Children)),
        ScopeCondition::Location(LocationScope::Country("JP".to_owned())),
    ];
    let result = retrieve(&mut engine, &with_child);
    assert_eq!(result.facts[0].id, age_id);
    assert_eq!(result.constraints[0].id, constraint_id);

    let without_child = query(4, Some("travel"), &["age"]);
    let filtered = retrieve(&mut engine, &without_child);
    assert_eq!(
        decision(&filtered, age_id),
        RetrievalDecision::Filtered(RetrievalFilterReason::Entity)
    );
}

// PMV1-20：无 likes_color 证据时返回空 lanes，不能从购买 Episode 推断颜色偏好。
#[test]
fn pmv1_20_insufficient_evidence_returns_empty_lanes() {
    let mut engine = engine(&[
        ("purchased_item", Cardinality::Multiple),
        ("likes_color", Cardinality::Multiple),
    ]);
    let episode = candidate(
        user(),
        MemoryType::Episode,
        "purchased_item",
        "blue_shirt",
        1,
        evidence("S1", "T1", 0, "曾经买过蓝色衣服。"),
    );
    let id = inserted_id(&ingest(&mut engine, episode, ReconciliationAction::New));
    let result = retrieve(&mut engine, &query(2, None, &["likes_color"]));
    assert_empty(&result);
    assert_eq!(
        decision(&result, id),
        RetrievalDecision::Filtered(RetrievalFilterReason::Predicate)
    );
}

// PMV1-B01：确认语和一次性任务指令不能成为正式记忆。
#[test]
fn pmv1_b01_control_and_task_messages_are_rejected() {
    let mut engine = engine(&[("control", Cardinality::Multiple)]);
    for (ordinal, (quote, source)) in [
        ("好的，继续。", CandidateSource::ControlMessage),
        ("帮我算一下 23×18。", CandidateSource::GoalOrPlan),
    ]
    .into_iter()
    .enumerate()
    {
        let mut candidate = candidate(
            user(),
            MemoryType::Fact,
            "control",
            quote,
            1,
            evidence("S1", "T1", ordinal, quote),
        );
        candidate.source = source;
        let report = engine
            .ingest_with_trace(common::grounded(candidate))
            .unwrap();
        assert_eq!(
            report.trace.admission,
            AdmissionDecision::Reject("source is not user evidence")
        );
    }
    assert!(
        engine
            .repository
            .list_by_owner(&owner())
            .unwrap()
            .is_empty()
    );
}

// PMV1-B02：没有跨 turn 价值且未明确要求记住的一次性 Episode 被拒绝。
#[test]
fn pmv1_b02_low_value_episode_is_rejected() {
    let mut engine = engine(&[("ate", Cardinality::Multiple)]);
    let mut meal = candidate(
        user(),
        MemoryType::Episode,
        "ate",
        "番茄炒蛋",
        1,
        evidence("S1", "T1", 0, "我今天中午吃了番茄炒蛋。"),
    );
    meal.cross_turn_useful = false;
    let report = engine.ingest_with_trace(common::grounded(meal)).unwrap();
    assert_eq!(
        report.trace.admission,
        AdmissionDecision::Reject("episode has no cross-turn value")
    );
}

// PMV1-B03：稳定习惯是 Fact，不自动变成 Preference。
#[test]
fn pmv1_b03_habit_fact_is_not_preference() {
    let mut engine = engine(&[
        ("has_habit", Cardinality::Multiple),
        ("likes", Cardinality::Multiple),
    ]);
    let mut habit = candidate(
        user(),
        MemoryType::Fact,
        "has_habit",
        "coffee_morning",
        1,
        evidence("S1", "T1", 0, "我每天早上都喝咖啡。"),
    );
    habit.scope = food_scope(vec![ScopeCondition::Time(TimeScope::TimeOfDay(
        TimeOfDay::Morning,
    ))]);
    let id = inserted_id(&ingest(&mut engine, habit, ReconciliationAction::New));
    let mut habit_query = query(2, Some("food"), &["has_habit"]);
    habit_query.conditions = vec![ScopeCondition::Time(TimeScope::TimeOfDay(
        TimeOfDay::Morning,
    ))];
    assert_eq!(retrieve(&mut engine, &habit_query).facts[0].id, id);
    let likes = retrieve(&mut engine, &query(2, Some("food"), &["likes"]));
    assert_empty(&likes);
    assert_eq!(
        decision(&likes, id),
        RetrievalDecision::Filtered(RetrievalFilterReason::Predicate)
    );
}

// 复现真实 B03：问题明确查询习惯，但并未限定 food/programming 等领域。
#[test]
fn broad_fact_query_keeps_scope_and_entity_guards() {
    let mut engine = engine(&[("has_habit", Cardinality::Multiple)]);
    let morning = ScopeCondition::Time(TimeScope::TimeOfDay(TimeOfDay::Morning));
    let mut habit = candidate(
        user(),
        MemoryType::Fact,
        "has_habit",
        "喝咖啡",
        100,
        evidence("B03", "T1", 0, "我每天早上都喝咖啡。"),
    );
    habit.scope = food_scope(vec![morning.clone()]);
    let id = inserted_id(&ingest(&mut engine, habit, ReconciliationAction::New));
    let mut context = query(101, None, &["has_habit"]);
    context.conditions.push(morning);
    let result = retrieve(&mut engine, &context);
    assert_eq!(result.facts.iter().map(|m| m.id).collect::<Vec<_>>(), [id]);
    assert_eq!(decision(&result, id), RetrievalDecision::Selected);

    for (memory_type, turn) in [(MemoryType::Preference, "P"), (MemoryType::Constraint, "C")] {
        let mut rule = candidate(
            user(),
            memory_type,
            "has_habit",
            "咖啡",
            100,
            evidence("B03", turn, 0, "晨间咖啡偏好或约束"),
        );
        rule.scope = food_scope(context.conditions.clone());
        let rule_id = inserted_id(&ingest(&mut engine, rule, ReconciliationAction::New));
        assert_eq!(
            decision(&retrieve(&mut engine, &context), rule_id),
            RetrievalDecision::Filtered(RetrievalFilterReason::Scope)
        );
    }

    for (domain, entities, conditions, predicates, expected) in [
        (
            Some(Domain::from("travel")),
            vec![user()],
            context.conditions.clone(),
            context.predicates.clone(),
            RetrievalFilterReason::Scope,
        ),
        (
            None,
            vec![user()],
            vec![],
            context.predicates.clone(),
            RetrievalFilterReason::Scope,
        ),
        (
            None,
            vec![EntityId::from("someone-else")],
            context.conditions.clone(),
            context.predicates.clone(),
            RetrievalFilterReason::Entity,
        ),
        (
            None,
            vec![user()],
            context.conditions.clone(),
            vec![Predicate::from("likes")],
            RetrievalFilterReason::Predicate,
        ),
        (
            None,
            vec![user()],
            context.conditions.clone(),
            vec![],
            RetrievalFilterReason::Scope,
        ),
    ] {
        let other = RuntimeContext {
            domain,
            entities,
            conditions,
            predicates,
            ..context.clone()
        };
        assert_eq!(
            decision(&retrieve(&mut engine, &other), id),
            RetrievalDecision::Filtered(expected)
        );
    }
}

#[test]
fn family_scope_contains_children_without_generalizing() {
    for with_country in [false, true] {
        let mut engine = engine(&[("homestay", Cardinality::Multiple)]);
        let family = ScopeCondition::Companion(EntityOrRole::Role(CompanionRole::Family));
        let children = ScopeCondition::Companion(EntityOrRole::Role(CompanionRole::Children));
        let japan = ScopeCondition::Location(LocationScope::Country("JP".to_owned()));
        let mut general = candidate(
            user(),
            MemoryType::Constraint,
            "homestay",
            "homestay",
            100,
            evidence("family", "T1", 0, "以后家庭旅行不要给我推荐民宿。"),
        );
        general.polarity = Some(Polarity::Negative);
        general.scope = travel_scope(vec![family]);
        let general_id = inserted_id(&ingest(
            &mut engine,
            general.clone(),
            ReconciliationAction::New,
        ));
        let mut exception = general;
        exception.scope = travel_scope(if with_country {
            vec![children.clone(), japan.clone()]
        } else {
            vec![children.clone()]
        });
        exception.polarity = Some(Polarity::Positive);
        exception.evidence = vec![evidence(
            "family",
            "T2",
            0,
            "不过带孩子去日本，民宿可以考虑。",
        )];
        let exception_id = inserted_id(&ingest(&mut engine, exception, ReconciliationAction::New));
        let mut context = query(101, Some("travel"), &["homestay"]);
        context.conditions = vec![children, japan];
        let result = retrieve(&mut engine, &context);
        assert_eq!(
            result.constraints.iter().map(|m| m.id).collect::<Vec<_>>(),
            [exception_id]
        );
        assert_eq!(
            decision(&result, general_id),
            RetrievalDecision::Filtered(RetrievalFilterReason::OverriddenBy(exception_id))
        );
        if with_country {
            context.conditions.pop();
            assert_eq!(
                retrieve(&mut engine, &context).constraints[0].id,
                general_id
            );
        }
        context.conditions = vec![ScopeCondition::Companion(EntityOrRole::Role(
            CompanionRole::Friends,
        ))];
        assert!(retrieve(&mut engine, &context).constraints.is_empty());
        context.conditions = vec![ScopeCondition::Companion(EntityOrRole::Role(
            CompanionRole::Family,
        ))];
        assert_eq!(
            retrieve(&mut engine, &context).constraints[0].id,
            general_id
        );
    }
}

// PMV1-B04：仅今天早餐生效的 Constraint 在次日边界后 Expired。
#[test]
fn pmv1_b04_today_breakfast_constraint_expires() {
    let today = "2026-09-04";
    let mut engine = engine(&[("exclude_drink", Cardinality::Multiple)]);
    let mut constraint = candidate(
        user(),
        MemoryType::Constraint,
        "exclude_drink",
        "coffee",
        1,
        evidence("S1", "T1", 0, "今天早餐别给我咖啡。"),
    );
    constraint.polarity = Some(Polarity::Negative);
    constraint.scope = food_scope(vec![
        ScopeCondition::Activity("breakfast".to_owned()),
        ScopeCondition::Time(TimeScope::Date(today.to_owned())),
    ]);
    constraint.retention_tier = RetentionTier::ShortTerm;
    constraint.temporal.expires_at = Some(Timestamp(10));
    let id = inserted_id(&ingest(&mut engine, constraint, ReconciliationAction::New));
    let mut current = query(5, Some("food"), &["exclude_drink"]);
    current.conditions = vec![
        ScopeCondition::Activity("breakfast".to_owned()),
        ScopeCondition::Time(TimeScope::Date(today.to_owned())),
    ];
    assert_eq!(retrieve(&mut engine, &current).constraints[0].id, id);
    engine
        .run_retention_for_owner(&owner(), Timestamp(10))
        .unwrap();
    let expired = retrieve(&mut engine, &current);
    assert_eq!(
        decision(&expired, id),
        RetrievalDecision::Filtered(RetrievalFilterReason::Status)
    );
}

// PMV1-B05：复合原话拆成 Episode 与地点 Preference，不发明 Experience 类型。
#[test]
fn pmv1_b05_compound_travel_statement_is_atomic() {
    let mut engine = engine(&[
        ("visited", Cardinality::Multiple),
        ("likes_place", Cardinality::Multiple),
    ]);
    let quote = "去年去了云南，大理我觉得很好。";
    let mut visited = candidate(
        user(),
        MemoryType::Episode,
        "visited",
        "Yunnan",
        1,
        evidence("S1", "T1", 0, quote),
    );
    visited.scope = travel_scope(vec![]);
    let visited_id = inserted_id(&ingest(&mut engine, visited, ReconciliationAction::New));
    let mut impression = candidate(
        user(),
        MemoryType::Preference,
        "likes_place",
        "Dali",
        1,
        evidence("S1", "T1", 1, quote),
    );
    impression.polarity = Some(Polarity::Positive);
    impression.scope = travel_scope(vec![]);
    ingest(&mut engine, impression, ReconciliationAction::New);
    let result = retrieve(&mut engine, &query(2, Some("travel"), &["visited"]));
    assert_eq!(result.episodes[0].id, visited_id);
    assert!(result.preferences.is_empty());
}

// PMV1-B06：行为事实与用户明确表达的低确定性 Preference 分开保存。
#[test]
fn pmv1_b06_behavior_fact_is_separate_from_preference() {
    let mut engine = engine(&[
        ("stayed_at_on_business_trip", Cardinality::Multiple),
        ("prefers_hotel", Cardinality::Multiple),
    ]);
    let scope = travel_scope(vec![ScopeCondition::Activity("business_trip".to_owned())]);
    let mut behavior = candidate(
        user(),
        MemoryType::Fact,
        "stayed_at_on_business_trip",
        "Atour",
        1,
        evidence("S1", "T1", 0, "我连续三次出差都住亚朵。"),
    );
    behavior.scope = scope.clone();
    let fact_id = inserted_id(&ingest(&mut engine, behavior, ReconciliationAction::New));
    let mut preference = candidate(
        user(),
        MemoryType::Preference,
        "prefers_hotel",
        "Atour",
        2,
        evidence("S1", "T2", 0, "我每次出差基本都会优先选亚朵。"),
    );
    preference.polarity = Some(Polarity::Positive);
    preference.certainty = Confidence::Low;
    preference.scope = scope.clone();
    let preference_id = inserted_id(&ingest(&mut engine, preference, ReconciliationAction::New));

    let mut context = query(
        3,
        Some("travel"),
        &["stayed_at_on_business_trip", "prefers_hotel"],
    );
    context.conditions = scope.conditions;
    let result = retrieve(&mut engine, &context);
    assert_eq!(result.facts[0].id, fact_id);
    assert_eq!(result.preferences[0].id, preference_id);
}

// PMV1-B07：否认旧说法是 Retract，不生成 dislikes。
#[test]
fn pmv1_b07_denial_retracts_without_opposite_preference() {
    let mut engine = engine(&[("likes", Cardinality::Multiple)]);
    let mut old = candidate(
        user(),
        MemoryType::Preference,
        "likes",
        "homestay",
        1,
        evidence("S0", "T1", 0, "我喜欢民宿。"),
    );
    old.polarity = Some(Polarity::Positive);
    let id = inserted_id(&ingest(&mut engine, old, ReconciliationAction::New));
    let mut denial = candidate(
        user(),
        MemoryType::Preference,
        "likes",
        "homestay",
        2,
        evidence("S1", "T1", 0, "我没说过我喜欢民宿。"),
    );
    denial.intent = CandidateIntent::Retract { target: Some(id) };
    ingest(&mut engine, denial, ReconciliationAction::Retract);
    let result = retrieve(&mut engine, &query(3, None, &["likes"]));
    assert_empty(&result);
    assert_eq!(engine.repository.list_by_owner(&owner()).unwrap().len(), 1);
}

// PMV1-B08：主居住地与周末常住地是不同 predicate，不互相 supersede。
#[test]
fn pmv1_b08_compound_residence_statement_uses_two_predicates() {
    let mut engine = engine(&[
        ("primary_residence", Cardinality::Single),
        ("weekend_stay", Cardinality::Multiple),
    ]);
    let quote = "我现在住阜阳，不过周末经常住南京。";
    let primary_id = inserted_id(&ingest(
        &mut engine,
        candidate(
            user(),
            MemoryType::Fact,
            "primary_residence",
            "Fuyang",
            1,
            evidence("S1", "T1", 0, quote),
        ),
        ReconciliationAction::New,
    ));
    let weekend_id = inserted_id(&ingest(
        &mut engine,
        candidate(
            user(),
            MemoryType::Fact,
            "weekend_stay",
            "Nanjing",
            1,
            evidence("S1", "T1", 1, quote),
        ),
        ReconciliationAction::New,
    ));
    assert_eq!(
        retrieve(&mut engine, &query(2, None, &["primary_residence"])).facts[0].id,
        primary_id
    );
    assert_eq!(
        retrieve(&mut engine, &query(2, None, &["weekend_stay"])).facts[0].id,
        weekend_id
    );
}

// PMV1-B09：车辆故障仅在具有跨 turn 价值时作为 ShortTerm Episode 入库。
#[test]
fn pmv1_b09_vehicle_breakdown_respects_cross_turn_value() {
    let vehicle = EntityId::from("vehicle-1");
    let make = |cross_turn_useful| {
        let mut episode = candidate(
            vehicle.clone(),
            MemoryType::Episode,
            "vehicle_broken",
            MemoryValue::Boolean(true),
            1,
            evidence(
                "S1",
                "T1",
                usize::from(cross_turn_useful),
                "我的车今天坏了。",
            ),
        );
        episode.retention_tier = RetentionTier::ShortTerm;
        episode.cross_turn_useful = cross_turn_useful;
        episode
    };

    let mut rejected_engine = engine(&[("vehicle_broken", Cardinality::Single)]);
    assert!(matches!(
        rejected_engine
            .ingest(common::grounded(make(false)))
            .unwrap(),
        IngestOutcome::Rejected("episode has no cross-turn value")
    ));
    let mut accepted_engine = engine(&[("vehicle_broken", Cardinality::Single)]);
    let id = inserted_id(&ingest(
        &mut accepted_engine,
        make(true),
        ReconciliationAction::New,
    ));
    let mut context = query(2, None, &["vehicle_broken"]);
    context.entities = vec![vehicle];
    assert_eq!(retrieve(&mut accepted_engine, &context).episodes[0].id, id);
}

// PMV1-B10：Task-specific 预算覆盖一般预算，但不会 supersede 它。
#[test]
fn pmv1_b10_task_budget_overrides_general_budget() {
    let mut engine = engine(&[("hotel_budget", Cardinality::Single)]);
    let mut general = candidate(
        user(),
        MemoryType::Constraint,
        "hotel_budget",
        MemoryValue::Integer(500),
        1,
        evidence("S1", "T1", 0, "一般旅行酒店预算不超过 500。"),
    );
    general.scope = travel_scope(vec![]);
    let general_id = inserted_id(&ingest(&mut engine, general, ReconciliationAction::New));
    let mut shanghai = candidate(
        user(),
        MemoryType::Constraint,
        "hotel_budget",
        MemoryValue::Integer(800),
        2,
        evidence("S1", "T2", 0, "这次上海酒店预算不超过 800。"),
    );
    shanghai.scope = travel_scope(vec![ScopeCondition::Task(TaskId::from("shanghai-trip"))]);
    let shanghai_id = inserted_id(&ingest(&mut engine, shanghai, ReconciliationAction::New));

    let mut shanghai_query = query(3, Some("travel"), &["hotel_budget"]);
    shanghai_query.task_id = Some(TaskId::from("shanghai-trip"));
    let result = retrieve(&mut engine, &shanghai_query);
    assert_eq!(result.constraints[0].id, shanghai_id);
    assert_eq!(
        decision(&result, general_id),
        RetrievalDecision::Filtered(RetrievalFilterReason::OverriddenBy(shanghai_id))
    );

    let mut beijing_query = query(4, Some("travel"), &["hotel_budget"]);
    beijing_query.task_id = Some(TaskId::from("beijing-trip"));
    let beijing = retrieve(&mut engine, &beijing_query);
    assert_eq!(beijing.constraints[0].id, general_id);
    assert_eq!(
        decision(&beijing, shanghai_id),
        RetrievalDecision::Filtered(RetrievalFilterReason::Scope)
    );
}

// PMV1-B11：明确询问的地点不能从另一个旅行经历推断；开放式评分仍不在本用例内。
#[test]
fn pmv1_b11_open_ended_travel_query_should_return_minimal_relevant_set() {
    let mut engine = engine(&[("visited", Cardinality::Multiple)]);
    ingest(
        &mut engine,
        candidate(
            user(),
            MemoryType::Episode,
            "visited",
            "Yunnan",
            1,
            evidence("S1", "T1", 0, "我去过云南。"),
        ),
        ReconciliationAction::New,
    );
    let mut context = query(2, None, &["visited"]);
    context.targets = vec![MemoryValue::from("Lugu Lake")];
    assert_empty(&retrieve(&mut engine, &context));
}

#[test]
fn explicit_targets_preserve_preference_and_episode_boundaries() {
    let mut engine = engine(&[
        ("likes", Cardinality::Multiple),
        ("exclude_drink", Cardinality::Multiple),
        ("age", Cardinality::Single),
        ("visited", Cardinality::Multiple),
    ]);
    let mut ids = Vec::new();
    for (index, (kind, predicate, value, polarity, conditions)) in [
        (
            MemoryType::Preference,
            "likes",
            "Coffee",
            Polarity::Negative,
            vec![],
        ),
        (
            MemoryType::Preference,
            "likes",
            "Latte",
            Polarity::Positive,
            vec![ScopeCondition::Activity("breakfast".into())],
        ),
        (
            MemoryType::Constraint,
            "exclude_drink",
            "Coffee",
            Polarity::Negative,
            vec![],
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let mut memory = candidate(
            user(),
            kind,
            predicate,
            value,
            index as i64 + 1,
            evidence("S1", "T1", index, value),
        );
        memory.polarity = Some(polarity);
        memory.scope = food_scope(conditions);
        ids.push(inserted_id(&ingest(
            &mut engine,
            memory,
            ReconciliationAction::New,
        )));
    }
    let age = inserted_id(&ingest(
        &mut engine,
        candidate(
            user(),
            MemoryType::Fact,
            "age",
            MemoryValue::Integer(42),
            1,
            evidence("S1", "T2", 0, "我 42 岁。"),
        ),
        ReconciliationAction::New,
    ));
    let visit = inserted_id(&ingest(
        &mut engine,
        candidate(
            user(),
            MemoryType::Episode,
            "visited",
            "Yunnan",
            1,
            evidence("S1", "T3", 0, "我去过云南。"),
        ),
        ReconciliationAction::New,
    ));
    let mut relations = InMemorySemanticRelationRegistry::default();
    relations.register_is_a(MemoryValue::from("Latte"), MemoryValue::from("Coffee"));
    relations.register_is_a(MemoryValue::from("Espresso"), MemoryValue::from("Coffee"));
    relations.register_is_a(MemoryValue::from("Lugu Lake"), MemoryValue::from("Yunnan"));
    let mut context = query(5, Some("food"), &["likes", "exclude_drink", "age"]);
    context.conditions = vec![ScopeCondition::Activity("breakfast".into())];
    context.targets = vec![MemoryValue::from("Latte")];
    let mut run = |context: &RuntimeContext| {
        DefaultMemoryRetriever::new(&mut engine.repository, &engine.predicates)
            .with_semantic_relations(&relations)
            .retrieve(context)
            .unwrap()
    };
    let result = run(&context);
    assert_eq!(
        result.preferences.iter().map(|m| m.id).collect::<Vec<_>>(),
        vec![ids[1]]
    );
    assert_eq!(
        decision(&result, ids[0]),
        RetrievalDecision::Filtered(RetrievalFilterReason::OverriddenBy(ids[1]))
    );
    assert_eq!(result.constraints[0].id, ids[2]);
    assert_eq!(result.facts[0].id, age, "候选饮料不能筛掉年龄事实");
    let mut unqualified = context.clone();
    unqualified.predicates.clear();
    assert_eq!(
        run(&unqualified).preferences.len(),
        2,
        "未限定谓词时保留原集合"
    );

    context.conditions = vec![ScopeCondition::Activity("dinner".into())];
    let result = run(&context);
    assert_eq!(
        result.preferences.iter().map(|m| m.id).collect::<Vec<_>>(),
        vec![ids[0]],
        "子偏好 Scope 不适用时，仍返回显式关系支持的父偏好"
    );
    assert_eq!(
        decision(&result, ids[1]),
        RetrievalDecision::Filtered(RetrievalFilterReason::Scope)
    );
    context.conditions = vec![ScopeCondition::Activity("breakfast".into())];
    context.targets.push(MemoryValue::from("Espresso"));
    assert_eq!(
        run(&context).preferences.len(),
        2,
        "拿铁例外不能删除浓缩咖啡对应的父规则"
    );
    context.targets.clear();
    assert_eq!(
        run(&context).preferences.len(),
        2,
        "开放式查询保留原偏好集合"
    );

    let mut history = query(5, None, &["visited"]);
    history.temporal_intent = TemporalIntent::Historical;
    history.targets = vec![MemoryValue::from("Yunnan")];
    assert_eq!(run(&history).episodes[0].id, visit);
    history.targets = vec![MemoryValue::from("Lugu Lake")];
    let result = run(&history);
    assert_empty(&result);
    assert_eq!(
        decision(&result, visit),
        RetrievalDecision::Filtered(RetrievalFilterReason::Target)
    );
    history.targets.clear();
    assert_eq!(run(&history).episodes[0].id, visit);
    assert_eq!(stored(&engine, ids[0]).status, MemoryStatus::Active);
    assert_eq!(stored(&engine, ids[1]).status, MemoryStatus::Active);
    // 已有受控类别别名即使未注册父子关系也可匹配；不是任意子串归一化。
    context.predicates = vec![Predicate::from("likes")];
    context.targets = vec![MemoryValue::from("拿铁类咖啡饮料")];
    assert_eq!(retrieve(&mut engine, &context).preferences[0].id, ids[1]);
    context.targets = vec![MemoryValue::from("咖啡因")];
    assert!(retrieve(&mut engine, &context).preferences.is_empty());
}

// PMV1-B12：Expired 记录不参与 Current，但仍可在 Historical 中读取。
#[test]
fn pmv1_b12_expired_memory_is_historical_only() {
    let mut engine = engine(&[("evening_activity", Cardinality::Multiple)]);
    let mut gaming = candidate(
        user(),
        MemoryType::Episode,
        "evening_activity",
        "gaming",
        1,
        evidence("S1", "T1", 0, "以前经常晚上打游戏。"),
    );
    gaming.retention_tier = RetentionTier::ShortTerm;
    gaming.temporal.expires_at = Some(Timestamp(10));
    let id = inserted_id(&ingest(&mut engine, gaming, ReconciliationAction::New));
    engine
        .run_retention_for_owner(&owner(), Timestamp(20))
        .unwrap();

    let current = retrieve(&mut engine, &query(20, None, &["evening_activity"]));
    assert_eq!(
        decision(&current, id),
        RetrievalDecision::Filtered(RetrievalFilterReason::Status)
    );
    let mut historical_query = query(20, None, &["evening_activity"]);
    historical_query.temporal_intent = TemporalIntent::Historical;
    let historical = retrieve(&mut engine, &historical_query);
    assert_eq!(historical.episodes[0].id, id);
}
