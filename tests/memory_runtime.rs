//! 核心领域规则的确定性测试；每个场景直接构造结构化输入以定位具体规则。

mod common;

use hippocampus::*;

fn owner() -> UserId {
    UserId::from("user-1")
}

fn user() -> EntityId {
    EntityId::from("entity-user-1")
}

fn evidence(id: &str, session_id: &str, turn_id: &str, quote: &str) -> Evidence {
    Evidence::new(
        EvidenceId::from(id),
        SessionId::from(session_id),
        TurnId::from(turn_id),
        quote,
    )
}

fn candidate(
    memory_type: MemoryType,
    predicate: &str,
    value: impl Into<MemoryValue>,
    at: i64,
) -> CandidateMemory {
    let value = value.into();
    CandidateMemory::new(
        owner(),
        user(),
        memory_type,
        Predicate::from(predicate),
        value.clone(),
        Timestamp(at),
        evidence(
            &format!("fixture:{at}:{memory_type:?}:{predicate}:{value:?}"),
            "test-session",
            &format!("turn-{at}"),
            &format!("fixture: {predicate} = {value:?}"),
        ),
    )
}

fn engine(
    definitions: &[(&str, Cardinality)],
) -> MemoryEngine<InMemoryMemoryRepository, InMemoryPredicateRegistry> {
    let mut registry = InMemoryPredicateRegistry::default();
    for (predicate, cardinality) in definitions {
        registry.register(Predicate::from(*predicate), *cardinality);
    }
    MemoryEngine::new(InMemoryMemoryRepository::default(), registry)
}

fn applied_id(outcome: IngestOutcome) -> MemoryId {
    match outcome {
        IngestOutcome::Applied {
            memory_id: Some(id),
            ..
        } => id,
        other => panic!("expected an inserted memory, got {other:?}"),
    }
}

fn source_checks() -> Vec<AdmissionCheck> {
    vec![
        AdmissionCheck {
            rule: "fact_value_representation",
            status: AdmissionCheckStatus::NotApplicable,
            reason: "generic fact retains a complete USER sentence; dedicated predicates keep typed values",
        },
        AdmissionCheck {
            rule: "source_support",
            status: AdmissionCheckStatus::Passed,
            reason: "candidate matches source-reviewed snapshot",
        },
    ]
}

#[derive(Default)]
struct CountingAdmission(std::cell::Cell<usize>);

impl AdmissionPolicy for CountingAdmission {
    fn evaluate(&self, candidate: &CandidateMemory) -> AdmissionDecision {
        self.0.set(self.0.get() + 1);
        DefaultAdmissionPolicy.evaluate(candidate)
    }
}

fn counted_engine()
-> MemoryEngine<InMemoryMemoryRepository, InMemoryPredicateRegistry, CountingAdmission> {
    let base = engine(&[("primary_residence", Cardinality::Single)]);
    MemoryEngine {
        repository: base.repository,
        predicates: base.predicates,
        admission: CountingAdmission::default(),
        reconciliation: base.reconciliation,
        retention: base.retention,
        staging: base.staging,
    }
}

#[test]
fn candidate_report_preserves_outcomes_and_executes_each_decision_once() {
    let mut legacy = engine(&[("primary_residence", Cardinality::Single)]);
    let mut reported = counted_engine();
    let mut entities = InMemoryEntityRepository::default();
    entities.upsert(Entity {
        id: user(),
        owner_id: owner(),
        entity_type: EntityType::Person,
        canonical_name: "本人".into(),
        aliases: vec![],
    });
    let mut first = candidate(MemoryType::Fact, "primary_residence", "Nanjing", 1);
    first.subject = CandidateSubject::Unresolved("本人".into());
    let second = candidate(MemoryType::Fact, "primary_residence", "Nanjing", 2);
    let mut regenerated = second.clone();
    regenerated.evidence[0].id = EvidenceId::from("regenerated-same-turn");
    let conflict = candidate(MemoryType::Fact, "primary_residence", "Hangzhou", 3);
    let mut ignored = candidate(MemoryType::Fact, "primary_residence", "Nanjing", 4);
    ignored.intent = CandidateIntent::Ignore;
    let mut moved = candidate(MemoryType::Fact, "primary_residence", "Fuyang", 5);
    moved.intent = CandidateIntent::Supersede {
        target: Some(MemoryId(1)),
        reason: SupersedeReason::StateChanged,
    };
    let mut refined = candidate(MemoryType::Fact, "primary_residence", "Fuyang district", 6);
    refined.intent = CandidateIntent::Refine {
        target: Some(MemoryId(2)),
    };
    let mut retracted = candidate(MemoryType::Fact, "primary_residence", "Fuyang district", 7);
    retracted.intent = CandidateIntent::Retract {
        target: Some(MemoryId(3)),
    };
    let cases = [
        (
            first,
            Some(ReconciliationAction::New),
            "no active equal-scope memory matched",
        ),
        (
            second.clone(),
            Some(ReconciliationAction::Reinforce),
            "same semantic slot, scope, and assertion matched independent evidence",
        ),
        (
            second,
            None,
            "duplicate evidence id; reconciliation skipped",
        ),
        (
            regenerated,
            Some(ReconciliationAction::Reinforce),
            "same source turn already supports target; reinforcement skipped",
        ),
        (
            conflict,
            Some(ReconciliationAction::Conflict),
            "same slot has a different assertion without explicit update intent",
        ),
        (
            ignored,
            Some(ReconciliationAction::Ignore),
            "candidate explicitly requests no write",
        ),
        (
            moved,
            Some(ReconciliationAction::Supersede),
            "candidate explicitly supersedes the matched memory",
        ),
        (
            refined,
            Some(ReconciliationAction::Refine),
            "candidate explicitly refines the matched memory",
        ),
        (
            retracted,
            Some(ReconciliationAction::Retract),
            "candidate explicitly retracts the matched memory",
        ),
    ];
    for (index, (input, action, reason)) in cases.into_iter().enumerate() {
        let expected = legacy
            .ingest_or_stage(common::grounded(input.clone()), &entities)
            .unwrap();
        let report = reported
            .ingest_or_stage_with_trace(common::grounded(input), &entities)
            .unwrap();
        assert_eq!(report.outcome, expected);
        assert_eq!(
            serde_json::to_value(&report.outcome).unwrap(),
            serde_json::to_value(expected).unwrap()
        );
        let trace = report.trace.unwrap();
        assert_eq!(trace.admission, AdmissionDecision::Admit);
        assert_eq!(
            trace.admission_checks,
            Some(source_checks()),
            "shared source checks also run after a custom admission policy"
        );
        assert_eq!(trace.reconciliation.map(|decision| decision.action), action);
        assert_eq!(trace.reconciliation_reason, Some(reason));
        assert_eq!(
            reported.admission.0.get(),
            index + 1,
            "logging must not execute admission twice"
        );
        assert_eq!(
            serde_json::to_value(&reported.repository).unwrap(),
            serde_json::to_value(&legacy.repository).unwrap()
        );
    }
    let memories = reported.repository.list_by_owner(&owner()).unwrap();
    assert_eq!(memories.len(), 3);
    assert_eq!(memories[0].reinforcement_count, 1);
    assert_eq!(memories[0].evidence.len(), 2);
    assert!(
        memories
            .iter()
            .all(|memory| memory.status == MemoryStatus::Superseded)
    );
    assert!(reported.staging.is_empty());
}

#[test]
fn candidate_report_distinguishes_early_rejection_pending_and_policy_rejection() {
    let mut engine = counted_engine();
    let mut entities = InMemoryEntityRepository::default();
    let mut unresolved = candidate(MemoryType::Fact, "primary_residence", "Nanjing", 1);
    unresolved.subject = CandidateSubject::Unresolved("家人".into());
    let mut incomplete = unresolved.clone();
    incomplete.evidence.clear();
    let rejected = engine
        .ingest_or_stage_with_trace(common::grounded(incomplete), &entities)
        .unwrap();
    assert_eq!(
        rejected.outcome,
        CandidateIngestOutcome::Ingested(IngestOutcome::Rejected("evidence is incomplete"))
    );
    let trace = rejected.trace.unwrap();
    assert_eq!(
        trace.admission,
        AdmissionDecision::Reject("evidence is incomplete")
    );
    assert_eq!(trace.admission_reason, "evidence is incomplete");
    assert_eq!(
        trace.admission_checks,
        Some(vec![AdmissionCheck {
            rule: "evidence_complete",
            status: AdmissionCheckStatus::Failed,
            reason: "evidence is incomplete",
        }])
    );
    assert!(trace.existing_memory_ids.is_empty() && trace.matched_memory_ids.is_empty());
    assert!(trace.reconciliation.is_none() && trace.reconciliation_reason.is_none());
    assert_eq!(engine.admission.0.get(), 0);
    assert!(engine.staging.is_empty());
    let pending = engine
        .ingest_or_stage_with_trace(common::grounded(unresolved.clone()), &entities)
        .unwrap();
    assert!(matches!(
        pending.outcome,
        CandidateIngestOutcome::Pending {
            resolution: EntityResolution::NotFound,
            ..
        }
    ));
    assert!(pending.trace.is_none());
    for id in ["family-a", "family-b"] {
        entities.upsert(Entity {
            id: EntityId::from(id),
            owner_id: owner(),
            entity_type: EntityType::Person,
            canonical_name: id.into(),
            aliases: vec!["家人".into()],
        });
    }
    let ambiguous = engine
        .ingest_or_stage_with_trace(common::grounded(unresolved), &entities)
        .unwrap();
    assert!(
        matches!(ambiguous.outcome, CandidateIngestOutcome::Pending { resolution: EntityResolution::Ambiguous(ids), .. } if ids.len() == 2)
    );
    assert!(ambiguous.trace.is_none());
    assert_eq!(
        engine.admission.0.get(),
        0,
        "pending candidates have no admission trace"
    );
    assert_eq!(engine.staging.len(), 2);
    let mut low = candidate(MemoryType::Fact, "primary_residence", "Nanjing", 2);
    low.extraction_confidence = Confidence::Low;
    let rejected = engine
        .ingest_or_stage_with_trace(common::grounded(low), &entities)
        .unwrap();
    assert_eq!(
        rejected.outcome,
        CandidateIngestOutcome::Ingested(IngestOutcome::Rejected("extraction confidence is low"))
    );
    let trace = rejected.trace.unwrap();
    assert_eq!(trace.admission_reason, "extraction confidence is low");
    assert!(trace.admission_checks.is_none());
    assert_eq!(engine.admission.0.get(), 1);
    assert!(
        engine
            .repository
            .list_by_owner(&owner())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn candidate_report_preserves_store_errors_without_fabricating_results() {
    struct UnavailableRepository;
    impl MemoryRepository for UnavailableRepository {
        fn allocate_id(&mut self) -> Result<MemoryId, StoreError> {
            unreachable!("failed lookup must stop ingestion")
        }
        fn insert(&mut self, _: Memory) -> Result<(), StoreError> {
            unreachable!("failed lookup must stop ingestion")
        }
        fn update(&mut self, _: Memory) -> Result<(), StoreError> {
            unreachable!("failed lookup must stop ingestion")
        }
        fn get(&self, _: MemoryId) -> Result<Option<Memory>, StoreError> {
            unreachable!("failed lookup must stop ingestion")
        }
        fn list_by_owner(&self, _: &UserId) -> Result<Vec<Memory>, StoreError> {
            Err(StoreError("fixture store unavailable".into()))
        }
    }
    let mut engine = MemoryEngine::new(UnavailableRepository, InMemoryPredicateRegistry::default());
    let entities = InMemoryEntityRepository::default();
    let input = candidate(MemoryType::Fact, "primary_residence", "Nanjing", 1);
    let error = engine
        .ingest_or_stage_with_trace(common::grounded(input.clone()), &entities)
        .unwrap_err();
    assert_eq!(error, StoreError("fixture store unavailable".into()));
    assert_eq!(
        engine
            .ingest_or_stage(common::grounded(input), &entities)
            .unwrap_err(),
        error
    );
    assert!(engine.staging.is_empty());
}

#[test]
fn default_admission_trace_records_actual_non_episode_checks() {
    let mut input = candidate(MemoryType::Fact, "primary_residence", "Nanjing", 1);
    input.extraction_confidence = Confidence::Medium;
    input.certainty = Confidence::Low;
    input.source = CandidateSource::UserConfirmed;
    input.cross_turn_useful = false;
    let report = DefaultAdmissionPolicy.evaluate_with_trace(&input);
    assert_eq!(report.decision, AdmissionDecision::Admit);
    assert_eq!(DefaultAdmissionPolicy.evaluate(&input), report.decision);
    assert_eq!(
        serde_json::to_value(&report.checks).unwrap(),
        serde_json::json!([
            {"rule":"evidence_complete","status":"Passed","reason":"evidence is complete"},
            {"rule":"subject_resolved","status":"Passed","reason":"subject is resolved"},
            {"rule":"extraction_confidence","status":"Passed","reason":"extraction confidence is not low"},
            {"rule":"user_evidence","status":"Passed","reason":"source is user evidence"},
            {"rule":"episode_value","status":"NotApplicable","reason":"candidate is not an episode"}
        ])
    );
    let trace = engine(&[("primary_residence", Cardinality::Single)])
        .ingest_or_stage_with_trace(
            common::grounded(input),
            &InMemoryEntityRepository::default(),
        )
        .unwrap()
        .trace
        .unwrap();
    let mut expected_checks = report.checks.unwrap();
    expected_checks.extend(source_checks());
    assert_eq!(trace.admission_checks, Some(expected_checks));
}

#[test]
fn default_admission_trace_records_episode_eligibility_without_claiming_a_match() {
    for (useful, explicit, intent, reason) in [
        (
            true,
            false,
            CandidateIntent::Assert,
            "episode has cross-turn value",
        ),
        (
            false,
            true,
            CandidateIntent::Assert,
            "user explicitly requested memory",
        ),
        (
            false,
            false,
            CandidateIntent::Supersede {
                target: None,
                reason: SupersedeReason::Correction,
            },
            "candidate explicitly updates existing memory",
        ),
        (
            false,
            false,
            CandidateIntent::Refine { target: None },
            "candidate explicitly updates existing memory",
        ),
        (
            false,
            false,
            CandidateIntent::Retract { target: None },
            "candidate explicitly updates existing memory",
        ),
    ] {
        let mut input = candidate(MemoryType::Episode, "visited", "Hangzhou", 1);
        input.cross_turn_useful = useful;
        input.explicit_memory_request = explicit;
        input.intent = intent;
        let report = DefaultAdmissionPolicy.evaluate_with_trace(&input);
        assert_eq!(report.decision, AdmissionDecision::Admit);
        assert_eq!(DefaultAdmissionPolicy.evaluate(&input), report.decision);
        let checks = report.checks.unwrap();
        assert_eq!(checks.len(), 5);
        assert!(
            checks
                .iter()
                .all(|check| check.status == AdmissionCheckStatus::Passed)
        );
        assert_eq!(checks[4].rule, "episode_value");
        assert_eq!(checks[4].reason, reason);
        if matches!(input.intent, CandidateIntent::Retract { .. }) {
            let result = engine(&[("visited", Cardinality::Multiple)])
                .ingest_or_stage_with_trace(
                    common::grounded(input),
                    &InMemoryEntityRepository::default(),
                )
                .unwrap();
            assert_eq!(
                result.outcome,
                CandidateIngestOutcome::Ingested(IngestOutcome::Ignored)
            );
            let trace = result.trace.unwrap();
            let mut expected_checks = checks;
            expected_checks.extend(source_checks());
            assert_eq!(trace.admission_checks, Some(expected_checks));
            assert_eq!(
                trace.reconciliation_reason,
                Some("explicit update target was not found")
            );
        }
    }
}

#[test]
fn default_admission_trace_stops_at_the_first_failed_condition() {
    let mut input = candidate(MemoryType::Episode, "visited", "Hangzhou", 1);
    let complete_evidence = input.evidence.clone();
    input.evidence.clear();
    input.subject = CandidateSubject::Unresolved("someone".into());
    input.extraction_confidence = Confidence::Low;
    input.source = CandidateSource::AssistantInference;
    input.cross_turn_useful = false;
    for (index, (rule, reason)) in [
        ("evidence_complete", "evidence is incomplete"),
        ("subject_resolved", "subject is unresolved"),
        ("extraction_confidence", "extraction confidence is low"),
        ("user_evidence", "source is not user evidence"),
        ("episode_value", "episode has no cross-turn value"),
    ]
    .into_iter()
    .enumerate()
    {
        let report = DefaultAdmissionPolicy.evaluate_with_trace(&input);
        assert_eq!(report.decision, AdmissionDecision::Reject(reason));
        assert_eq!(DefaultAdmissionPolicy.evaluate(&input), report.decision);
        let checks = report.checks.unwrap();
        assert_eq!(
            checks.len(),
            index + 1,
            "later checks must not be reported as executed"
        );
        assert!(
            checks[..index]
                .iter()
                .all(|check| check.status == AdmissionCheckStatus::Passed)
        );
        assert_eq!(
            checks[index],
            AdmissionCheck {
                rule,
                status: AdmissionCheckStatus::Failed,
                reason
            }
        );
        let trace = engine(&[])
            .ingest_with_trace(common::grounded(input.clone()))
            .unwrap()
            .trace;
        assert_eq!(trace.admission_checks, Some(checks));
        assert!(trace.existing_memory_ids.is_empty() && trace.reconciliation.is_none());
        match index {
            0 => input.evidence = complete_evidence.clone(),
            1 => input.subject = CandidateSubject::Resolved(user()),
            2 => input.extraction_confidence = Confidence::High,
            3 => input.source = CandidateSource::UserStatement,
            _ => (),
        }
    }
}

// 场景：Travel+Children 比通用 Travel 更具体，且不能误用于 Friends。
#[test]
fn scope_is_structured_and_runtime_matched() {
    let general = Scope::new(Some(Domain::from("travel")), vec![]);
    let children = Scope::new(
        Some(Domain::from("travel")),
        vec![ScopeCondition::Companion(EntityOrRole::Role(
            CompanionRole::Children,
        ))],
    );
    let friends = Scope::new(
        Some(Domain::from("travel")),
        vec![ScopeCondition::Companion(EntityOrRole::Role(
            CompanionRole::Friends,
        ))],
    );

    assert_eq!(children.relation(&general), ScopeRelation::MoreSpecific);
    assert_eq!(general.relation(&children), ScopeRelation::MoreGeneral);
    assert_eq!(children.relation(&friends), ScopeRelation::Disjoint);

    let mut context = RuntimeContext::current(owner(), Timestamp(1));
    context.domain = Some(Domain::from("travel"));
    context.conditions = children.conditions.clone();
    assert!(children.applies_to(&context));
    assert!(!friends.applies_to(&context));
}

// 场景：相对时间按用户时区解析；模糊、日历、事件相对和未知表达保持不同来源语义。
#[test]
fn relative_time_resolution_covers_real_chinese_expressions() {
    let now = Timestamp(1_788_494_400); // 2026-09-04 12:00:00 +08:00
    let offset = 8 * 60 * 60;

    assert_eq!(
        resolve_temporal_expression("从今以后", now, offset).unwrap(),
        TemporalResolution::Range(TemporalRange {
            start: now,
            end: Timestamp(i64::MAX),
            source: TemporalSource::Inferred,
        })
    );
    assert_eq!(
        resolve_temporal_expression("现在", now, offset).unwrap(),
        TemporalResolution::Range(TemporalRange {
            start: now,
            end: Timestamp(i64::MAX),
            source: TemporalSource::Inferred,
        })
    );
    assert_eq!(
        resolve_temporal_expression("最近", now, offset).unwrap(),
        TemporalResolution::Range(TemporalRange {
            start: Timestamp(1_785_902_400),
            end: Timestamp(1_788_494_401),
            source: TemporalSource::Inferred,
        })
    );
    assert_eq!(
        resolve_temporal_expression("下个月", now, offset).unwrap(),
        TemporalResolution::Range(TemporalRange {
            start: Timestamp(1_790_784_000),
            end: Timestamp(1_793_462_400),
            source: TemporalSource::Inferred,
        })
    );
    assert_eq!(
        resolve_temporal_expression("去年六月", now, offset).unwrap(),
        TemporalResolution::Range(TemporalRange {
            start: Timestamp(1_748_707_200),
            end: Timestamp(1_751_299_200),
            source: TemporalSource::Inferred,
        })
    );
    assert_eq!(
        resolve_temporal_expression("旅行结束后", now, offset).unwrap(),
        TemporalResolution::Reference(TemporalReference::Event {
            event: "旅行".to_owned(),
            relation: TemporalRelation::AfterEnd,
            source: TemporalSource::Explicit,
        })
    );
    assert_eq!(
        resolve_temporal_expression("等合适的时候", now, offset).unwrap(),
        TemporalResolution::Reference(TemporalReference::Unknown {
            expression: "等合适的时候".to_owned(),
            source: TemporalSource::Unknown,
        })
    );
}

// 回归：相对日历支持跨年与闰日；来源保持 Inferred，而非伪装成用户提供的时间戳。
#[test]
fn relative_calendar_days_cover_year_and_leap_boundaries() {
    let offset = 8 * 3600;
    for (day, tomorrow) in [("2026-12-31", "2027-01-01"), ("2024-02-28", "2024-02-29")] {
        let TemporalResolution::Range(today) =
            resolve_temporal_expression(day, Timestamp(0), offset).unwrap()
        else {
            unreachable!()
        };
        let now = Timestamp(today.start.0 + 12 * 3600);
        let TemporalResolution::Range(actual) =
            resolve_temporal_expression("明天", now, offset).unwrap()
        else {
            unreachable!()
        };
        let TemporalResolution::Range(expected) =
            resolve_temporal_expression(tomorrow, now, offset).unwrap()
        else {
            unreachable!()
        };
        assert_eq!((actual.start, actual.end), (expected.start, expected.end));
        assert_eq!(actual.source, TemporalSource::Inferred);
    }
}

// 回归：未指定日期的历史查询能读到已结束的经历，但不能把未知将来事件或未来状态混入。
#[test]
fn broad_history_keeps_past_without_weakening_explicit_time_filters() {
    let mut engine = engine(&[
        ("visited", Cardinality::Multiple),
        ("evening_activity", Cardinality::Multiple),
    ]);
    let mut dated = candidate(MemoryType::Episode, "visited", "云南", 30);
    dated.temporal.valid_from = Some(DatedValue {
        at: Timestamp(10),
        source: TemporalSource::Explicit,
    });
    dated.temporal.valid_to = Some(DatedValue {
        at: Timestamp(20),
        source: TemporalSource::Explicit,
    });
    let dated_id = applied_id(engine.ingest(common::grounded(dated)).unwrap());
    let mut past = candidate(MemoryType::Episode, "visited", "江苏", 31);
    past.temporal.reference = Some(TemporalReference::Unknown {
        expression: "以前".into(),
        source: TemporalSource::Unknown,
    });
    let past_id = applied_id(engine.ingest(common::grounded(past)).unwrap());
    let mut habit = candidate(MemoryType::Fact, "evening_activity", "打游戏", 34);
    let TemporalResolution::Reference(reference) =
        resolve_temporal_expression("以前经常", Timestamp(34), 0).unwrap()
    else {
        panic!("a frequency modifier must not invent a dated range");
    };
    habit.temporal.reference = Some(reference);
    habit.retention_tier = RetentionTier::ShortTerm;
    habit.temporal.expires_at = Some(Timestamp(50));
    let habit_id = applied_id(engine.ingest(common::grounded(habit)).unwrap());
    engine
        .run_retention_for_owner(&owner(), Timestamp(1000))
        .unwrap();
    assert_eq!(
        engine.repository.get(habit_id).unwrap().unwrap().status,
        MemoryStatus::Expired
    );
    let mut unknown = candidate(MemoryType::Episode, "visited", "未知行程", 32);
    unknown.temporal.reference = Some(TemporalReference::Unknown {
        expression: "等合适的时候".into(),
        source: TemporalSource::Unknown,
    });
    engine.ingest(common::grounded(unknown)).unwrap();
    for expression in ["以前某个周五", "以前经常去的那天", "以前搬家之前"] {
        let mut unknown = candidate(MemoryType::Episode, "visited", expression, 35);
        let TemporalResolution::Reference(reference) =
            resolve_temporal_expression(expression, Timestamp(35), 0).unwrap()
        else {
            panic!("unknown dates and event boundaries must stay unresolved");
        };
        unknown.temporal.reference = Some(reference);
        engine.ingest(common::grounded(unknown)).unwrap();
    }
    let mut future = candidate(MemoryType::Episode, "visited", "未来行程", 33);
    future.temporal.valid_from = Some(DatedValue {
        at: Timestamp(2000),
        source: TemporalSource::Explicit,
    });
    engine.ingest(common::grounded(future)).unwrap();
    let mut context = RuntimeContext::current(owner(), Timestamp(1000));
    assert!(
        DefaultMemoryRetriever::new(&mut engine.repository, &engine.predicates)
            .retrieve(&context)
            .unwrap()
            .facts
            .is_empty()
    );
    assert!(
        DefaultMemoryRetriever::new(&mut engine.repository, &engine.predicates)
            .retrieve(&context)
            .unwrap()
            .episodes
            .is_empty()
    );
    context.temporal_intent = TemporalIntent::Historical;
    let result = DefaultMemoryRetriever::new(&mut engine.repository, &engine.predicates)
        .retrieve(&context)
        .unwrap();
    let mut ids = result
        .episodes
        .iter()
        .map(|memory| memory.id)
        .collect::<Vec<_>>();
    ids.sort();
    assert_eq!(ids, vec![dated_id, past_id]);
    assert_eq!(
        result
            .facts
            .iter()
            .map(|memory| memory.id)
            .collect::<Vec<_>>(),
        vec![habit_id]
    );
    context.temporal_filter = Some(TemporalResolution::Range(TemporalRange {
        start: Timestamp(10),
        end: Timestamp(11),
        source: TemporalSource::Explicit,
    }));
    let result = DefaultMemoryRetriever::new(&mut engine.repository, &engine.predicates)
        .retrieve(&context)
        .unwrap();
    assert!(result.facts.is_empty());
    assert_eq!(
        result
            .episodes
            .iter()
            .map(|memory| memory.id)
            .collect::<Vec<_>>(),
        vec![dated_id]
    );
}

// 回归：搬家前的锚点来自唯一、真实的 StateChanged 版本链；重复搬到同地必须保留歧义。
#[test]
fn before_move_uses_version_chain_and_refuses_ambiguous_anchors() {
    let mut engine = engine(&[("primary_residence", Cardinality::Single)]);
    let mut ids = Vec::new();
    for (at, city) in [(1, "南京"), (10, "阜阳"), (20, "杭州")] {
        let mut memory = candidate(MemoryType::Fact, "primary_residence", city, at);
        // 首条“现在住南京”有时间；后续“搬到…”不手工补时间，验证真实入库衔接。
        if at == 1 {
            memory.temporal.valid_from = Some(DatedValue {
                at: Timestamp(at),
                source: TemporalSource::Explicit,
            });
        }
        if at > 1 {
            memory.intent = CandidateIntent::Supersede {
                target: None,
                reason: SupersedeReason::StateChanged,
            };
        }
        ids.push(applied_id(engine.ingest(common::grounded(memory)).unwrap()));
    }
    assert_eq!(
        engine
            .repository
            .get(ids[1])
            .unwrap()
            .unwrap()
            .temporal
            .valid_from,
        Some(DatedValue {
            at: Timestamp(10),
            source: TemporalSource::Supersession
        })
    );
    let mut context = RuntimeContext::current(owner(), Timestamp(30));
    context.entities.push(user());
    context
        .predicates
        .push(Predicate::from("primary_residence"));
    context.temporal_intent = TemporalIntent::Historical;
    // 不同有效期的地址是多个历史版本，不能被误报为同一时间的冲突。
    let broad = DefaultMemoryRetriever::new(&mut engine.repository, &engine.predicates)
        .retrieve(&context)
        .unwrap();
    assert_eq!(broad.facts.len(), 3);
    assert!(broad.conflicts.is_empty());
    context.temporal_filter =
        Some(resolve_temporal_expression("搬到阜阳之前", context.at, 0).unwrap());
    let before = DefaultMemoryRetriever::new(&mut engine.repository, &engine.predicates)
        .retrieve(&context)
        .unwrap();
    assert_eq!(
        before
            .facts
            .iter()
            .map(|memory| memory.id)
            .collect::<Vec<_>>(),
        vec![ids[0]]
    );
    let mut again = candidate(MemoryType::Fact, "primary_residence", "阜阳", 40);
    again.intent = CandidateIntent::Supersede {
        target: None,
        reason: SupersedeReason::StateChanged,
    };
    engine.ingest(common::grounded(again)).unwrap();
    context.at = Timestamp(50);
    assert!(
        DefaultMemoryRetriever::new(&mut engine.repository, &engine.predicates)
            .retrieve(&context)
            .unwrap()
            .facts
            .is_empty()
    );
}

// 回归：补述搬家日期用真实有效期切换；矛盾日期不写库，未知事件不伪造发生时间。
#[test]
fn state_change_respects_backdated_and_unresolved_boundaries() {
    let mut engine = engine(&[("primary_residence", Cardinality::Single)]);
    let mut first = candidate(MemoryType::Fact, "primary_residence", "南京", 1);
    first.temporal.valid_from = Some(DatedValue {
        at: Timestamp(1),
        source: TemporalSource::Explicit,
    });
    let old_id = applied_id(engine.ingest(common::grounded(first)).unwrap());
    let mut moved = candidate(MemoryType::Fact, "primary_residence", "阜阳", 30);
    moved.temporal.valid_from = Some(DatedValue {
        at: Timestamp(10),
        source: TemporalSource::Explicit,
    });
    moved.intent = CandidateIntent::Supersede {
        target: None,
        reason: SupersedeReason::StateChanged,
    };
    let moved_id = applied_id(engine.ingest(common::grounded(moved.clone())).unwrap());
    assert_eq!(
        engine
            .repository
            .get(old_id)
            .unwrap()
            .unwrap()
            .temporal
            .valid_to
            .unwrap()
            .at,
        Timestamp(10)
    );
    let current = engine.repository.get(moved_id).unwrap().unwrap();
    assert_eq!(current.temporal.observed_at, Timestamp(30));
    assert_eq!(
        current.temporal.valid_from.unwrap().source,
        TemporalSource::Explicit
    );

    let mut invalid = candidate(MemoryType::Fact, "primary_residence", "杭州", 35);
    invalid.intent = moved.intent.clone();
    invalid.temporal.valid_from = Some(DatedValue {
        at: Timestamp(5),
        source: TemporalSource::Explicit,
    });
    let before = engine.repository.list_by_owner(&owner()).unwrap();
    let rejected = engine.ingest_with_trace(common::grounded(invalid)).unwrap();
    assert_eq!(
        rejected.outcome,
        IngestOutcome::Conflict {
            target: Some(moved_id)
        }
    );
    assert_eq!(
        rejected.trace.reconciliation.unwrap().action,
        ReconciliationAction::Conflict
    );
    assert_eq!(engine.repository.list_by_owner(&owner()).unwrap(), before);

    let mut unknown = candidate(MemoryType::Fact, "primary_residence", "杭州", 40);
    unknown.intent = moved.intent;
    unknown.temporal.reference = Some(TemporalReference::Unknown {
        expression: "某次旅行以后".to_owned(),
        source: TemporalSource::Unknown,
    });
    let unknown_id = applied_id(engine.ingest(common::grounded(unknown)).unwrap());
    assert!(
        engine
            .repository
            .get(unknown_id)
            .unwrap()
            .unwrap()
            .temporal
            .valid_from
            .is_none()
    );
    let mut context = RuntimeContext::current(owner(), Timestamp(50));
    context.entities.push(user());
    context.temporal_intent = TemporalIntent::Historical;
    context.temporal_filter =
        Some(resolve_temporal_expression("搬到杭州之前", context.at, 0).unwrap());
    assert!(
        DefaultMemoryRetriever::new(&mut engine.repository, &engine.predicates)
            .retrieve(&context)
            .unwrap()
            .facts
            .is_empty()
    );
}

// 回归：别名和 IS-A 在同一层比较，不重写记忆原文，也不把咖啡因/产品 Entity 当作咖啡。
#[test]
fn semantic_aliases_bridge_constraint_targets_without_substring_matching() {
    let mut relations = InMemorySemanticRelationRegistry::default();
    relations.register_is_a(MemoryValue::from("Latte"), MemoryValue::from("Coffee"));
    assert!(relations.is_a(
        &MemoryValue::from("拿铁咖啡"),
        &MemoryValue::from("咖啡类饮料")
    ));
    assert!(!relations.is_a(&MemoryValue::from("咖啡因"), &MemoryValue::from("Coffee")));
    assert!(!relations.is_a(
        &MemoryValue::Entity(EntityId::from("coffee")),
        &MemoryValue::from("Coffee")
    ));
    let mut engine = engine(&[("exclude_drink", Cardinality::Multiple)]);
    let mut rule = candidate(MemoryType::Constraint, "exclude_drink", "咖啡类饮料", 1);
    rule.polarity = Some(Polarity::Negative);
    let id = applied_id(engine.ingest(common::grounded(rule)).unwrap());
    let mut context = RuntimeContext::current(owner(), Timestamp(2));
    context.predicates.push(Predicate::from("exclude_drink"));
    context.targets.push(MemoryValue::from("拿铁咖啡"));
    let result = DefaultMemoryRetriever::new(&mut engine.repository, &engine.predicates)
        .with_semantic_relations(&relations)
        .retrieve(&context)
        .unwrap();
    assert_eq!(result.constraints.len(), 1);
    assert_eq!(result.constraints[0].id, id);
    assert_eq!(result.constraints[0].value, MemoryValue::from("咖啡类饮料"));
}

// 场景：两个孩子共享“孩子”alias 时必须返回歧义，明确“小宝”才能解析。
#[test]
fn entity_alias_resolution_refuses_ambiguity() {
    let mut entities = InMemoryEntityRepository::default();
    for (id, name) in [("older", "大宝"), ("younger", "小宝")] {
        entities.upsert(Entity {
            id: EntityId::from(id),
            owner_id: owner(),
            entity_type: EntityType::Person,
            canonical_name: name.to_owned(),
            aliases: vec!["孩子".to_owned()],
        });
    }

    assert_eq!(
        entities.resolve(&owner(), "小宝"),
        EntityResolution::Resolved(EntityId::from("younger"))
    );
    assert!(matches!(
        entities.resolve(&owner(), "孩子"),
        EntityResolution::Ambiguous(ids) if ids.len() == 2
    ));
}

// 场景：“他不喜欢游泳”先因两个儿子都匹配而暂存；“我是说小儿子”澄清后才入库。
#[test]
fn candidate_staging_waits_for_unique_entity_clarification() {
    let mut entities = InMemoryEntityRepository::default();
    for (id, name, unique_alias) in [
        ("child-older", "大儿子", "大宝"),
        ("child-younger", "小儿子", "小宝"),
    ] {
        entities.upsert(Entity {
            id: EntityId::from(id),
            owner_id: owner(),
            entity_type: EntityType::Person,
            canonical_name: name.to_owned(),
            aliases: vec!["他".to_owned(), "儿子".to_owned(), unique_alias.to_owned()],
        });
    }

    let mut engine = engine(&[("likes", Cardinality::Multiple)]);
    let mut dislikes_swimming = candidate(MemoryType::Preference, "likes", "swimming", 1);
    dislikes_swimming.subject = CandidateSubject::Unresolved("他".to_owned());
    dislikes_swimming.polarity = Some(Polarity::Negative);
    dislikes_swimming.evidence = vec![evidence(
        "family-chat:turn-1:0",
        "family-chat",
        "turn-1",
        "他不喜欢游泳。",
    )];

    let pending = engine
        .ingest_or_stage(common::grounded(dislikes_swimming), &entities)
        .unwrap();
    let staged_id = match pending {
        CandidateIngestOutcome::Pending {
            staged_id,
            resolution: EntityResolution::Ambiguous(ids),
        } => {
            assert_eq!(ids.len(), 2);
            staged_id
        }
        other => panic!("expected ambiguous staging, got {other:?}"),
    };
    assert_eq!(engine.staging.len(), 1);
    assert!(
        engine
            .repository
            .list_by_owner(&owner())
            .unwrap()
            .is_empty()
    );

    // 仍然模糊的澄清只更新 pending record，不能任选一个 Entity。
    assert!(matches!(
        engine
            .clarify_staged(
                staged_id,
                "儿子",
                evidence(
                    "family-chat:turn-2:0",
                    "family-chat",
                    "turn-2",
                    "我是说儿子。",
                ),
                &entities,
            )
            .unwrap(),
        CandidateIngestOutcome::Pending {
            resolution: EntityResolution::Ambiguous(_),
            ..
        }
    ));
    assert!(
        engine
            .repository
            .list_by_owner(&owner())
            .unwrap()
            .is_empty()
    );

    let clarified = engine
        .clarify_staged(
            staged_id,
            "小儿子",
            evidence(
                "family-chat:turn-3:0",
                "family-chat",
                "turn-3",
                "我是说小儿子。",
            ),
            &entities,
        )
        .unwrap();
    assert!(matches!(
        clarified,
        CandidateIngestOutcome::Ingested(IngestOutcome::Applied {
            action: ReconciliationAction::New,
            ..
        })
    ));
    assert!(engine.staging.is_empty());

    let memories = engine.repository.list_by_owner(&owner()).unwrap();
    assert_eq!(memories.len(), 1);
    assert_eq!(memories[0].subject_id, EntityId::from("child-younger"));
    assert_eq!(memories[0].value, MemoryValue::from("swimming"));
    assert_eq!(memories[0].polarity, Some(Polarity::Negative));
    assert_eq!(memories[0].evidence.len(), 3);

    // 已消费的 staged id 不能重复入库。
    assert!(
        engine
            .clarify_staged(
                staged_id,
                "小儿子",
                evidence(
                    "family-chat:turn-4:0",
                    "family-chat",
                    "turn-4",
                    "我是说小儿子。",
                ),
                &entities,
            )
            .is_err()
    );
    assert_eq!(engine.repository.list_by_owner(&owner()).unwrap().len(), 1);
}

// 场景：未解析 subject 和 Assistant 推断被拒绝；用户的低 certainty 仍可保存。
#[test]
fn admission_requires_resolved_user_evidence() {
    let policy = DefaultAdmissionPolicy;
    let mut unresolved = candidate(MemoryType::Preference, "likes", "swimming", 1);
    unresolved.subject = CandidateSubject::Unresolved("他".to_owned());
    assert!(matches!(
        policy.evaluate(&unresolved),
        AdmissionDecision::Reject("subject is unresolved")
    ));

    let mut inference = candidate(MemoryType::Preference, "likes", "atour", 1);
    inference.source = CandidateSource::AssistantInference;
    assert!(matches!(
        policy.evaluate(&inference),
        AdmissionDecision::Reject("source is not user evidence")
    ));

    let mut uncertain = candidate(MemoryType::Fact, "attends", "kindergarten", 1);
    uncertain.certainty = Confidence::Low;
    assert_eq!(policy.evaluate(&uncertain), AdmissionDecision::Admit);

    let mut incomplete = candidate(MemoryType::Fact, "attends", "kindergarten", 2);
    incomplete.evidence[0].quote.clear();
    assert!(matches!(
        policy.evaluate(&incomplete),
        AdmissionDecision::Reject("evidence is incomplete")
    ));
}

// 场景：首次居住地是 NEW，重复确认是 REINFORCE，无更新意图的改值是 CONFLICT。
#[test]
fn new_reinforce_and_ambiguous_change_are_distinct() {
    let mut engine = engine(&[("primary_residence", Cardinality::Single)]);
    let first = applied_id(
        engine
            .ingest(common::grounded(candidate(
                MemoryType::Fact,
                "primary_residence",
                "Nanjing",
                1,
            )))
            .unwrap(),
    );
    let repeated = engine
        .ingest(common::grounded(candidate(
            MemoryType::Fact,
            "primary_residence",
            "Nanjing",
            2,
        )))
        .unwrap();
    assert!(matches!(
        repeated,
        IngestOutcome::Applied {
            action: ReconciliationAction::Reinforce,
            memory_id: Some(id),
            ..
        } if id == first
    ));

    let changed = engine
        .ingest(common::grounded(candidate(
            MemoryType::Fact,
            "primary_residence",
            "Fuyang",
            3,
        )))
        .unwrap();
    assert_eq!(
        changed,
        IngestOutcome::Conflict {
            target: Some(first)
        }
    );
}

// 场景：写入 trace 必须解释 admission、旧 Memory 匹配以及 Supersede / Conflict 的原因。
#[test]
fn write_decision_trace_explains_reject_supersede_and_conflict() {
    let mut engine = engine(&[("primary_residence", Cardinality::Single)]);

    let mut inference = candidate(MemoryType::Fact, "primary_residence", "Nanjing", 1);
    inference.source = CandidateSource::AssistantInference;
    let rejected = engine
        .ingest_with_trace(common::grounded(inference))
        .unwrap();
    assert_eq!(
        rejected.trace.admission,
        AdmissionDecision::Reject("source is not user evidence")
    );
    assert!(rejected.trace.reconciliation.is_none());

    let old_id = applied_id(
        engine
            .ingest(common::grounded(candidate(
                MemoryType::Fact,
                "primary_residence",
                "Nanjing",
                2,
            )))
            .unwrap(),
    );
    let mut moved = candidate(MemoryType::Fact, "primary_residence", "Fuyang", 3);
    moved.intent = CandidateIntent::Supersede {
        target: Some(old_id),
        reason: SupersedeReason::StateChanged,
    };
    let superseded = engine.ingest_with_trace(common::grounded(moved)).unwrap();
    assert_eq!(superseded.trace.matched_memory_ids, vec![old_id]);
    assert_eq!(
        superseded.trace.reconciliation,
        Some(ReconciliationDecision {
            action: ReconciliationAction::Supersede,
            target: Some(old_id),
            reason: Some(SupersedeReason::StateChanged),
        })
    );

    let current_id = match superseded.outcome {
        IngestOutcome::Applied {
            memory_id: Some(id),
            ..
        } => id,
        other => panic!("expected supersede, got {other:?}"),
    };
    let conflict = engine
        .ingest_with_trace(common::grounded(candidate(
            MemoryType::Fact,
            "primary_residence",
            "Hangzhou",
            4,
        )))
        .unwrap();
    assert_eq!(conflict.trace.matched_memory_ids, vec![current_id]);
    assert!(matches!(
        conflict.trace.reconciliation,
        Some(ReconciliationDecision {
            action: ReconciliationAction::Conflict,
            target: Some(id),
            ..
        }) if id == current_id
    ));
    assert_eq!(
        conflict.trace.reconciliation_reason,
        Some("same slot has a different assertion without explicit update intent")
    );
}

// 场景：同一消息重放、重生成 evidence ID 都不算强化；新 turn 和跨 session 才算独立证据。
#[test]
fn evidence_deduplicates_replays_and_counts_only_independent_reinforcement() {
    let mut engine = engine(&[("likes", Cardinality::Multiple)]);
    let mut first = candidate(MemoryType::Preference, "likes", "coffee", 1);
    first.evidence = vec![evidence(
        "chat-a:turn-1:0",
        "chat-a",
        "turn-1",
        "我喜欢咖啡。",
    )];
    let memory_id = applied_id(engine.ingest(common::grounded(first.clone())).unwrap());

    assert!(matches!(
        engine.ingest(common::grounded(first.clone())).unwrap(),
        IngestOutcome::Duplicate {
            evidence_id,
            memory_id: id,
        } if evidence_id == EvidenceId::from("chat-a:turn-1:0") && id == memory_id
    ));

    let mut regenerated_id = first.clone();
    regenerated_id.evidence[0].id = EvidenceId::from("retry-generated-a-different-id");
    assert!(matches!(
        engine.ingest(common::grounded(regenerated_id)).unwrap(),
        IngestOutcome::Duplicate { memory_id: id, .. } if id == memory_id
    ));

    let mut next_turn = first.clone();
    next_turn.temporal.observed_at = Timestamp(2);
    next_turn.evidence = vec![evidence(
        "chat-a:turn-2:0",
        "chat-a",
        "turn-2",
        "对，我确实喜欢咖啡。",
    )];
    assert!(matches!(
        engine.ingest(common::grounded(next_turn)).unwrap(),
        IngestOutcome::Applied {
            action: ReconciliationAction::Reinforce,
            ..
        }
    ));

    let mut another_session = first;
    another_session.temporal.observed_at = Timestamp(3);
    another_session.evidence = vec![evidence(
        "chat-b:turn-1:0",
        "chat-b",
        "turn-1",
        "换个话题前再说一次，我喜欢咖啡。",
    )];
    assert!(matches!(
        engine.ingest(common::grounded(another_session)).unwrap(),
        IngestOutcome::Applied {
            action: ReconciliationAction::Reinforce,
            ..
        }
    ));

    let memory = engine.repository.get(memory_id).unwrap().unwrap();
    assert_eq!(memory.reinforcement_count, 2);
    assert_eq!(memory.evidence.len(), 3);
    assert_eq!(memory.evidence[0].quote, "我喜欢咖啡。");
}

// 场景：明确搬家会 SUPERSEDE 旧居住地，并建立双向版本链和 valid_to。
#[test]
fn explicit_single_value_change_supersedes_and_links_versions() {
    let mut engine = engine(&[("primary_residence", Cardinality::Single)]);
    let old_id = applied_id(
        engine
            .ingest(common::grounded(candidate(
                MemoryType::Fact,
                "primary_residence",
                "Nanjing",
                1,
            )))
            .unwrap(),
    );
    let mut moved = candidate(MemoryType::Fact, "primary_residence", "Fuyang", 10);
    moved.intent = CandidateIntent::Supersede {
        target: Some(old_id),
        reason: SupersedeReason::StateChanged,
    };
    moved.temporal.valid_from = Some(DatedValue {
        at: Timestamp(10),
        source: TemporalSource::Explicit,
    });
    let new_id = applied_id(engine.ingest(common::grounded(moved)).unwrap());

    let old = engine.repository.get(old_id).unwrap().unwrap();
    let new = engine.repository.get(new_id).unwrap().unwrap();
    assert_eq!(old.status, MemoryStatus::Superseded);
    assert_eq!(old.superseded_by, Some(new_id));
    assert_eq!(old.supersede_reason, Some(SupersedeReason::StateChanged));
    assert_eq!(old.temporal.valid_to.unwrap().at, Timestamp(10));
    assert_eq!(new.supersedes, Some(old_id));
}

// 场景：owns_device 是 Multiple，MacBook 与 ThinkPad 应同时保持 Active。
#[test]
fn multiple_cardinality_keeps_independent_values() {
    let mut engine = engine(&[("owns_device", Cardinality::Multiple)]);
    let mac = applied_id(
        engine
            .ingest(common::grounded(candidate(
                MemoryType::Fact,
                "owns_device",
                "MacBook",
                1,
            )))
            .unwrap(),
    );
    let thinkpad = applied_id(
        engine
            .ingest(common::grounded(candidate(
                MemoryType::Fact,
                "owns_device",
                "ThinkPad",
                2,
            )))
            .unwrap(),
    );

    assert_ne!(mac, thinkpad);
    assert_eq!(engine.repository.list_by_owner(&owner()).unwrap().len(), 2);
}

// 场景：偏好增强产生新版本；撤销红眼航班限制只终止旧记录，不生成反向偏好。
#[test]
fn refine_replaces_while_retract_only_ends_the_old_memory() {
    let mut engine = engine(&[
        ("likes", Cardinality::Multiple),
        ("exclude_flight", Cardinality::Multiple),
    ]);
    let mut weak = candidate(MemoryType::Preference, "likes", "coffee", 1);
    weak.polarity = Some(Polarity::Positive);
    weak.strength = Some(Strength::Weak);
    let weak_id = applied_id(engine.ingest(common::grounded(weak)).unwrap());

    let mut strong = candidate(MemoryType::Preference, "likes", "coffee", 2);
    strong.polarity = Some(Polarity::Positive);
    strong.strength = Some(Strength::Strong);
    strong.intent = CandidateIntent::Refine {
        target: Some(weak_id),
    };
    let strong_id = applied_id(engine.ingest(common::grounded(strong)).unwrap());
    assert_eq!(
        engine
            .repository
            .get(weak_id)
            .unwrap()
            .unwrap()
            .supersede_reason,
        Some(SupersedeReason::Refinement)
    );
    assert_eq!(
        engine.repository.get(strong_id).unwrap().unwrap().status,
        MemoryStatus::Active
    );

    let red_eye = applied_id(
        engine
            .ingest(common::grounded(candidate(
                MemoryType::Constraint,
                "exclude_flight",
                "red-eye",
                3,
            )))
            .unwrap(),
    );
    let mut retract = candidate(MemoryType::Constraint, "exclude_flight", "red-eye", 4);
    retract.intent = CandidateIntent::Retract {
        target: Some(red_eye),
    };
    assert!(matches!(
        engine.ingest(common::grounded(retract)).unwrap(),
        IngestOutcome::Applied {
            action: ReconciliationAction::Retract,
            memory_id: None,
            ..
        }
    ));
    assert_eq!(
        engine
            .repository
            .get(red_eye)
            .unwrap()
            .unwrap()
            .supersede_reason,
        Some(SupersedeReason::UserRetracted)
    );
}

// 同一单值字段在两个项目共存时，更正和撤销只能操作目标项目。
#[test]
fn explicit_updates_respect_project_identity_without_a_requested_target() {
    let mut engine = engine(&[("code_example_language", Cardinality::Single)]);
    let scoped = |value: &str, task: &str, at| {
        let mut c = candidate(MemoryType::Constraint, "code_example_language", value, at);
        c.scope = Scope::new(None, vec![ScopeCondition::Task(TaskId::from(task))]);
        c
    };
    let a = applied_id(
        engine
            .ingest(common::grounded(scoped("Rust", "project-a", 1)))
            .unwrap(),
    );
    let b = applied_id(
        engine
            .ingest(common::grounded(scoped("Python", "project-b", 2)))
            .unwrap(),
    );
    let mut update = scoped("Go", "project-a", 3);
    update.intent = CandidateIntent::Supersede {
        target: None,
        reason: SupersedeReason::ConstraintChanged,
    };
    let changed = applied_id(engine.ingest(common::grounded(update.clone())).unwrap());
    assert_eq!(
        engine.repository.get(a).unwrap().unwrap().status,
        MemoryStatus::Superseded
    );
    assert_eq!(
        engine.repository.get(b).unwrap().unwrap().status,
        MemoryStatus::Active
    );
    update.scope = Scope::global();
    update.evidence = vec![evidence("global:4", "global", "4", "全局改成 Go")];
    assert_eq!(
        engine.ingest(common::grounded(update)).unwrap(),
        IngestOutcome::Ignored
    );
    let mut retract = scoped("Go", "project-a", 5);
    retract.intent = CandidateIntent::Retract { target: None };
    engine.ingest(common::grounded(retract)).unwrap();
    assert_eq!(
        engine.repository.get(changed).unwrap().unwrap().status,
        MemoryStatus::Superseded
    );
    assert_eq!(
        engine.repository.get(b).unwrap().unwrap().status,
        MemoryStatus::Active
    );
}

// 场景：ShortTerm 会到期或经两次独立强化升级，Durable 不自动过期。
#[test]
fn retention_expires_short_term_promotes_reinforced_and_keeps_durable() {
    let mut engine = engine(&[("learning", Cardinality::Multiple)]);

    let mut expires = candidate(MemoryType::Fact, "learning", "temporary", 1);
    expires.retention_tier = RetentionTier::ShortTerm;
    expires.temporal.expires_at = Some(Timestamp(10));
    let expired_id = applied_id(engine.ingest(common::grounded(expires)).unwrap());

    let mut durable = candidate(MemoryType::Fact, "learning", "durable", 1);
    durable.retention_tier = RetentionTier::Durable;
    durable.temporal.expires_at = Some(Timestamp(10));
    let durable_id = applied_id(engine.ingest(common::grounded(durable)).unwrap());

    let mut reinforced = candidate(MemoryType::Fact, "learning", "Rust", 1);
    reinforced.retention_tier = RetentionTier::ShortTerm;
    let promoted_id = applied_id(engine.ingest(common::grounded(reinforced.clone())).unwrap());
    reinforced.temporal.observed_at = Timestamp(2);
    reinforced.evidence = vec![evidence(
        "learning:turn-2:0",
        "learning-session",
        "turn-2",
        "我还在学 Rust。",
    )];
    engine.ingest(common::grounded(reinforced.clone())).unwrap();
    reinforced.temporal.observed_at = Timestamp(3);
    reinforced.evidence = vec![evidence(
        "learning:turn-3:0",
        "learning-session",
        "turn-3",
        "我持续在学 Rust。",
    )];
    engine.ingest(common::grounded(reinforced)).unwrap();

    engine
        .run_retention_for_owner(&owner(), Timestamp(20))
        .unwrap();
    assert_eq!(
        engine.repository.get(expired_id).unwrap().unwrap().status,
        MemoryStatus::Expired
    );
    assert_eq!(
        engine.repository.get(durable_id).unwrap().unwrap().status,
        MemoryStatus::Active
    );
    assert_eq!(
        engine
            .repository
            .get(promoted_id)
            .unwrap()
            .unwrap()
            .retention_tier,
        RetentionTier::LongTerm
    );
}

// 场景：日本亲子游的特例覆盖一般民宿规则，云南亲子游仍使用一般规则。
#[test]
fn retrieval_uses_scope_and_specificity_override() {
    let mut engine = engine(&[("homestay", Cardinality::Multiple)]);
    let mut general = candidate(MemoryType::Constraint, "homestay", "homestay", 1);
    general.polarity = Some(Polarity::Negative);
    general.scope = Scope::new(Some(Domain::from("travel")), vec![]);
    engine.ingest(common::grounded(general)).unwrap();

    let jp_children = vec![
        ScopeCondition::Companion(EntityOrRole::Role(CompanionRole::Children)),
        ScopeCondition::Location(LocationScope::Country("JP".to_owned())),
    ];
    let mut exception = candidate(MemoryType::Constraint, "homestay", "homestay", 2);
    exception.polarity = Some(Polarity::Positive);
    exception.scope = Scope::new(Some(Domain::from("travel")), jp_children.clone());
    engine.ingest(common::grounded(exception)).unwrap();

    let mut japan = RuntimeContext::current(owner(), Timestamp(3));
    japan.domain = Some(Domain::from("travel"));
    japan.entities.push(user());
    japan.conditions = jp_children;
    let result = DefaultMemoryRetriever::new(&mut engine.repository, &engine.predicates)
        .retrieve(&japan)
        .unwrap();
    assert_eq!(result.constraints.len(), 1);
    assert_eq!(result.constraints[0].polarity, Some(Polarity::Positive));

    let mut yunnan = RuntimeContext::current(owner(), Timestamp(3));
    yunnan.domain = Some(Domain::from("travel"));
    yunnan.entities.push(user());
    yunnan.conditions = vec![ScopeCondition::Companion(EntityOrRole::Role(
        CompanionRole::Children,
    ))];
    let result = DefaultMemoryRetriever::new(&mut engine.repository, &engine.predicates)
        .retrieve(&yunnan)
        .unwrap();
    assert_eq!(result.constraints.len(), 1);
    assert_eq!(result.constraints[0].polarity, Some(Polarity::Negative));
}

// 场景：Latte 属于 Coffee、MacBook 属于 Laptop；父类别 Constraint 应覆盖子类别但不覆盖 Tea。
#[test]
fn constraint_targets_follow_minimal_is_a_relations() {
    let mut relations = InMemorySemanticRelationRegistry::default();
    relations.register_is_a(MemoryValue::from("Latte"), MemoryValue::from("Coffee"));
    relations.register_is_a(MemoryValue::from("Coffee"), MemoryValue::from("Beverage"));
    relations.register_is_a(MemoryValue::from("MacBook"), MemoryValue::from("Laptop"));
    assert!(relations.is_a(&MemoryValue::from("latte"), &MemoryValue::from("Beverage")));
    assert!(!relations.is_a(&MemoryValue::from("Laptop"), &MemoryValue::from("MacBook")));

    let mut engine = engine(&[("exclude_category", Cardinality::Multiple)]);
    let mut add_constraint = |value: &str, at| {
        let mut constraint = candidate(MemoryType::Constraint, "exclude_category", value, at);
        constraint.polarity = Some(Polarity::Negative);
        applied_id(engine.ingest(common::grounded(constraint)).unwrap())
    };
    let coffee_id = add_constraint("Coffee", 1);
    let laptop_id = add_constraint("Laptop", 2);
    let tea_id = add_constraint("Tea", 3);

    let mut context = RuntimeContext::current(owner(), Timestamp(4));
    context.entities.push(user());
    context.predicates.push(Predicate::from("exclude_category"));
    context.targets = vec![MemoryValue::from("Latte"), MemoryValue::from("MacBook")];
    let result = DefaultMemoryRetriever::new(&mut engine.repository, &engine.predicates)
        .with_semantic_relations(&relations)
        .retrieve(&context)
        .unwrap();
    let mut ids: Vec<_> = result.constraints.iter().map(|memory| memory.id).collect();
    ids.sort();
    assert_eq!(ids, vec![coffee_id, laptop_id]);
    let decision = |id| {
        result
            .trace
            .iter()
            .find(|entry| entry.memory_id == id)
            .unwrap()
            .decision
    };
    assert_eq!(decision(coffee_id), RetrievalDecision::Selected);
    assert_eq!(decision(laptop_id), RetrievalDecision::Selected);
    assert_eq!(
        decision(tea_id),
        RetrievalDecision::Filtered(RetrievalFilterReason::Target)
    );
}

// 场景：全局 Durable 语言约束始终进入 mandatory lane，检索不增加 reinforcement。
#[test]
fn global_durable_constraint_is_mandatory_and_retrieval_is_not_reinforcement() {
    let mut engine = engine(&[("response_language", Cardinality::Single)]);
    let mut language = candidate(MemoryType::Constraint, "response_language", "Chinese", 1);
    language.retention_tier = RetentionTier::Durable;
    let id = applied_id(engine.ingest(common::grounded(language)).unwrap());

    let mut query = RuntimeContext::current(owner(), Timestamp(100));
    query.domain = Some(Domain::from("programming"));
    query.entities.push(EntityId::from("unrelated"));
    let result = DefaultMemoryRetriever::new(&mut engine.repository, &engine.predicates)
        .retrieve(&query)
        .unwrap();
    assert_eq!(result.mandatory.len(), 1);
    let stored = engine.repository.get(id).unwrap().unwrap();
    assert_eq!(stored.retrieval_count, 1);
    assert_eq!(stored.reinforcement_count, 0);
}

// 场景：搬家前的真实旧地址可历史查询；被纠错的错误月份永不作为历史事实返回。
#[test]
fn historical_retrieval_keeps_state_change_but_hides_correction() {
    let mut engine = engine(&[
        ("primary_residence", Cardinality::Single),
        ("trip_month", Cardinality::Single),
    ]);
    let mut nanjing = candidate(MemoryType::Fact, "primary_residence", "Nanjing", 1);
    nanjing.temporal.valid_from = Some(DatedValue {
        at: Timestamp(1),
        source: TemporalSource::Explicit,
    });
    let nanjing_id = applied_id(engine.ingest(common::grounded(nanjing)).unwrap());
    let mut fuyang = candidate(MemoryType::Fact, "primary_residence", "Fuyang", 10);
    fuyang.temporal.valid_from = Some(DatedValue {
        at: Timestamp(10),
        source: TemporalSource::Explicit,
    });
    fuyang.intent = CandidateIntent::Supersede {
        target: Some(nanjing_id),
        reason: SupersedeReason::StateChanged,
    };
    engine.ingest(common::grounded(fuyang)).unwrap();

    let july_id = applied_id(
        engine
            .ingest(common::grounded(candidate(
                MemoryType::Episode,
                "trip_month",
                "July",
                2,
            )))
            .unwrap(),
    );
    let mut june = candidate(MemoryType::Episode, "trip_month", "June", 3);
    june.intent = CandidateIntent::Supersede {
        target: Some(july_id),
        reason: SupersedeReason::Correction,
    };
    engine.ingest(common::grounded(june)).unwrap();

    let mut historical = RuntimeContext::current(owner(), Timestamp(5));
    historical.entities.push(user());
    historical.temporal_intent = TemporalIntent::Historical;
    let result = DefaultMemoryRetriever::new(&mut engine.repository, &engine.predicates)
        .retrieve(&historical)
        .unwrap();
    assert!(result.facts.iter().any(|memory| memory.id == nanjing_id));
    assert!(!result.episodes.iter().any(|memory| memory.id == july_id));
}

// 场景：当前查询返回现版本；“去年六月”的历史区间返回旧版本；事件相对查询只匹配同一引用。
#[test]
fn retrieval_applies_point_range_and_event_relative_time() {
    let now = Timestamp(1_788_494_400);
    let offset = 8 * 60 * 60;
    let last_june = resolve_temporal_expression("去年六月", now, offset).unwrap();
    let after_trip = resolve_temporal_expression("旅行结束后", now, offset).unwrap();
    let TemporalResolution::Range(last_june_range) = last_june else {
        unreachable!()
    };

    let mut engine = engine(&[
        ("primary_residence", Cardinality::Single),
        ("learning_since", Cardinality::Multiple),
    ]);
    let mut nanjing = candidate(
        MemoryType::Fact,
        "primary_residence",
        "Nanjing",
        last_june_range.start.0,
    );
    nanjing.temporal.valid_from = Some(DatedValue {
        at: last_june_range.start,
        source: TemporalSource::Inferred,
    });
    let nanjing_id = applied_id(engine.ingest(common::grounded(nanjing)).unwrap());

    let mut fuyang = candidate(MemoryType::Fact, "primary_residence", "Fuyang", now.0);
    fuyang.temporal.valid_from = Some(DatedValue {
        at: now,
        source: TemporalSource::Explicit,
    });
    fuyang.intent = CandidateIntent::Supersede {
        target: Some(nanjing_id),
        reason: SupersedeReason::StateChanged,
    };
    let fuyang_id = applied_id(engine.ingest(common::grounded(fuyang)).unwrap());

    let mut learning = candidate(MemoryType::Fact, "learning_since", "Rust", now.0 + 1);
    learning.temporal.apply_resolution(after_trip.clone());
    let learning_id = applied_id(engine.ingest(common::grounded(learning)).unwrap());

    let mut current = RuntimeContext::current(owner(), now);
    current.entities.push(user());
    current
        .predicates
        .push(Predicate::from("primary_residence"));
    let current_result = DefaultMemoryRetriever::new(&mut engine.repository, &engine.predicates)
        .retrieve(&current)
        .unwrap();
    assert_eq!(current_result.facts[0].id, fuyang_id);

    let mut historical = current.clone();
    historical.temporal_intent = TemporalIntent::Historical;
    historical.temporal_filter = Some(TemporalResolution::Range(last_june_range));
    let historical_result = DefaultMemoryRetriever::new(&mut engine.repository, &engine.predicates)
        .retrieve(&historical)
        .unwrap();
    assert_eq!(historical_result.facts[0].id, nanjing_id);

    let mut event_relative = RuntimeContext::current(owner(), now);
    event_relative.entities.push(user());
    event_relative
        .predicates
        .push(Predicate::from("learning_since"));
    event_relative.temporal_filter = Some(after_trip);
    let event_result = DefaultMemoryRetriever::new(&mut engine.repository, &engine.predicates)
        .retrieve(&event_relative)
        .unwrap();
    assert_eq!(event_result.facts[0].id, learning_id);
}

// 场景：四种 MemoryType 必须进入独立结果 lane，Constraint 与 Preference 不混分。
#[test]
fn retrieval_keeps_memory_types_separate() {
    let mut engine = engine(&[
        ("age", Cardinality::Single),
        ("likes", Cardinality::Multiple),
        ("exclude", Cardinality::Multiple),
        ("visited", Cardinality::Multiple),
    ]);
    for (memory_type, predicate, value) in [
        (MemoryType::Fact, "age", "3"),
        (MemoryType::Preference, "likes", "swimming"),
        (MemoryType::Constraint, "exclude", "homestay"),
        (MemoryType::Episode, "visited", "Yunnan"),
    ] {
        engine
            .ingest(common::grounded(candidate(
                memory_type,
                predicate,
                value,
                1,
            )))
            .unwrap();
    }

    let mut context = RuntimeContext::current(owner(), Timestamp(2));
    context.entities.push(user());
    let result = DefaultMemoryRetriever::new(&mut engine.repository, &engine.predicates)
        .retrieve(&context)
        .unwrap();
    assert_eq!(result.facts.len(), 1);
    assert_eq!(result.preferences.len(), 1);
    assert_eq!(result.constraints.len(), 1);
    assert_eq!(result.episodes.len(), 1);
}

// 场景：同 semantic slot、同优先级的相反偏好只报告冲突，不随机返回任一方。
#[test]
fn query_time_conflict_is_reported_instead_of_randomly_resolved() {
    let mut engine = engine(&[("likes", Cardinality::Multiple)]);
    let mut by_companion = candidate(MemoryType::Preference, "likes", "coffee", 1);
    by_companion.polarity = Some(Polarity::Positive);
    by_companion.scope = Scope::new(
        Some(Domain::from("food")),
        vec![ScopeCondition::Companion(EntityOrRole::Role(
            CompanionRole::Children,
        ))],
    );
    let companion_id = applied_id(engine.ingest(common::grounded(by_companion)).unwrap());

    let mut by_location = candidate(MemoryType::Preference, "likes", "coffee", 2);
    by_location.polarity = Some(Polarity::Negative);
    by_location.scope = Scope::new(
        Some(Domain::from("food")),
        vec![ScopeCondition::Location(LocationScope::Country(
            "JP".to_owned(),
        ))],
    );
    let location_id = applied_id(engine.ingest(common::grounded(by_location)).unwrap());

    let mut context = RuntimeContext::current(owner(), Timestamp(3));
    context.domain = Some(Domain::from("food"));
    context.entities.push(user());
    context.conditions = vec![
        ScopeCondition::Companion(EntityOrRole::Role(CompanionRole::Children)),
        ScopeCondition::Location(LocationScope::Country("JP".to_owned())),
    ];
    let result = DefaultMemoryRetriever::new(&mut engine.repository, &engine.predicates)
        .retrieve(&context)
        .unwrap();
    assert_eq!(result.conflicts.len(), 1);
    assert!(result.preferences.is_empty());
    assert!(result.trace.iter().any(|entry| {
        entry.memory_id == companion_id
            && entry.decision
                == RetrievalDecision::Conflict {
                    with: location_id,
                    reason: RetrievalConflictReason::EqualSpecificity,
                }
    }));
    assert!(result.trace.iter().any(|entry| {
        entry.memory_id == location_id
            && entry.decision
                == RetrievalDecision::Conflict {
                    with: companion_id,
                    reason: RetrievalConflictReason::EqualSpecificity,
                }
    }));
}

// Replays the PMV1-05 admission failure observed in 24-medium-full.log.
#[test]
fn episode_updates_do_not_require_new_cross_turn_value() {
    let mut first = candidate(MemoryType::Episode, "visited", "云南", 1);
    first.temporal.valid_from = Some(DatedValue {
        at: Timestamp(100),
        source: TemporalSource::Explicit,
    });
    let mut update = candidate(MemoryType::Episode, "visited", "云南", 2);
    update.temporal.valid_from = Some(DatedValue {
        at: Timestamp(90),
        source: TemporalSource::Explicit,
    });
    update.cross_turn_useful = false;
    let correction = CandidateIntent::Supersede {
        target: None,
        reason: SupersedeReason::Correction,
    };

    for (intent, action) in [
        (correction.clone(), ReconciliationAction::Supersede),
        (
            CandidateIntent::Refine { target: None },
            ReconciliationAction::Refine,
        ),
        (
            CandidateIntent::Retract { target: None },
            ReconciliationAction::Retract,
        ),
    ] {
        let mut engine = engine(&[("visited", Cardinality::Multiple)]);
        let old = applied_id(engine.ingest(common::grounded(first.clone())).unwrap());
        update.intent = intent;
        let outcome = engine.ingest(common::grounded(update.clone())).unwrap();
        assert!(
            matches!(outcome, IngestOutcome::Applied { action: actual, target: Some(id), .. }
            if actual == action && id == old),
            "{outcome:?}"
        );
        assert_eq!(
            engine.repository.get(old).unwrap().unwrap().status,
            MemoryStatus::Superseded
        );
        let memories = engine.repository.list_by_owner(&owner()).unwrap();
        if action == ReconciliationAction::Retract {
            assert_eq!(memories.len(), 1);
        } else {
            assert_eq!(memories.len(), 2);
            let new = memories
                .iter()
                .find(|m| m.status == MemoryStatus::Active)
                .unwrap();
            assert_eq!(new.supersedes, Some(old));
            assert_eq!(new.temporal.valid_from, update.temporal.valid_from);
        }
    }

    let mut engine = engine(&[("visited", Cardinality::Multiple)]);
    update.intent = CandidateIntent::Assert;
    assert_eq!(
        engine.ingest(common::grounded(update.clone())).unwrap(),
        IngestOutcome::Rejected("episode has no cross-turn value")
    );
    update.intent = correction.clone();
    assert_eq!(
        engine.ingest(common::grounded(update.clone())).unwrap(),
        IngestOutcome::Ignored
    );
    assert!(
        engine
            .repository
            .list_by_owner(&owner())
            .unwrap()
            .is_empty()
    );
    let old = applied_id(engine.ingest(common::grounded(first.clone())).unwrap());
    let mut wrong = update.clone();
    wrong.value = MemoryValue::from("南京");
    assert_eq!(
        engine.ingest(common::grounded(wrong)).unwrap(),
        IngestOutcome::Ignored
    );
    let mut wrong = update.clone();
    wrong.owner_id = UserId::from("other-owner");
    assert_eq!(
        engine.ingest(common::grounded(wrong)).unwrap(),
        IngestOutcome::Ignored
    );
    let mut wrong = update.clone();
    wrong.intent = CandidateIntent::Supersede {
        target: Some(MemoryId(999)),
        reason: SupersedeReason::Correction,
    };
    assert_eq!(
        engine.ingest(common::grounded(wrong)).unwrap(),
        IngestOutcome::Ignored
    );
    let mut inference = update.clone();
    inference.source = CandidateSource::AssistantInference;
    assert_eq!(
        engine.ingest(common::grounded(inference)).unwrap(),
        IngestOutcome::Rejected("source is not user evidence")
    );
    let mut uncertain = update.clone();
    uncertain.extraction_confidence = Confidence::Low;
    assert_eq!(
        engine.ingest(common::grounded(uncertain)).unwrap(),
        IngestOutcome::Rejected("extraction confidence is low")
    );
    let mut unresolved = update.clone();
    unresolved.subject = CandidateSubject::Unresolved("他".into());
    assert_eq!(
        engine.ingest(common::grounded(unresolved)).unwrap(),
        IngestOutcome::Rejected("subject is unresolved")
    );
    assert_eq!(
        engine.repository.get(old).unwrap().unwrap().status,
        MemoryStatus::Active
    );
    assert_eq!(engine.repository.list_by_owner(&owner()).unwrap().len(), 1);
    let duplicate_id = engine.repository.allocate_id().unwrap();
    let mut duplicate = engine.repository.get(old).unwrap().unwrap();
    duplicate.id = duplicate_id;
    engine.repository.insert(duplicate).unwrap();
    assert_eq!(
        engine.ingest(common::grounded(update)).unwrap(),
        IngestOutcome::Conflict { target: None }
    );
    assert!(
        engine
            .repository
            .list_by_owner(&owner())
            .unwrap()
            .iter()
            .all(|m| m.status == MemoryStatus::Active)
    );
}

// Replays the PMV1-09 unknown "最近开始" temporal reference through the real resolver and engine.
#[test]
fn recent_start_remains_retrievable_after_reinforcement() {
    let now = Timestamp(1_788_494_400);
    for expression in ["最近开始", "最近刚开始", "recently started"] {
        let mut engine = engine(&[("learning", Cardinality::Multiple)]);
        let mut first = candidate(MemoryType::Fact, "learning", "Rust", now.0);
        first.retention_tier = RetentionTier::ShortTerm;
        first
            .temporal
            .apply_resolution(resolve_temporal_expression(expression, now, 8 * 3600).unwrap());
        let id = applied_id(engine.ingest(common::grounded(first)).unwrap());
        for at in [now.0 + 1, now.0 + 2] {
            let ongoing = candidate(MemoryType::Fact, "learning", "Rust", at);
            assert!(matches!(
                engine.ingest(common::grounded(ongoing)).unwrap(),
                IngestOutcome::Applied {
                    action: ReconciliationAction::Reinforce,
                    ..
                }
            ));
        }
        let mut query = RuntimeContext::current(owner(), Timestamp(now.0 + 3));
        query.entities.push(user());
        query.predicates.push(Predicate::from("learning"));
        query.temporal_filter =
            Some(resolve_temporal_expression("最近", query.at, 8 * 3600).unwrap());
        let result = DefaultMemoryRetriever::new(&mut engine.repository, &engine.predicates)
            .retrieve(&query)
            .unwrap();
        assert_eq!(
            result.facts.iter().map(|m| m.id).collect::<Vec<_>>(),
            vec![id],
            "{expression}: {result:?}"
        );
        let memory = engine.repository.get(id).unwrap().unwrap();
        assert_eq!(memory.reinforcement_count, 2);
        assert_eq!(memory.retention_tier, RetentionTier::LongTerm);
        assert!(
            memory.temporal.valid_at(Timestamp(now.0 + 86_400)),
            "ongoing state must not end at extraction time"
        );
    }
    assert!(matches!(
        resolve_temporal_expression("等合适的时候开始", now, 8 * 3600).unwrap(),
        TemporalResolution::Reference(TemporalReference::Unknown { .. })
    ));
    assert!(matches!(
        resolve_temporal_expression("旅行结束后", now, 8 * 3600).unwrap(),
        TemporalResolution::Reference(TemporalReference::Event { .. })
    ));
}
