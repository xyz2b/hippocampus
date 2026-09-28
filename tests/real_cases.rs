//! 数据驱动的端到端案例：原始中文输入 → recorded extraction/context → engine → Store/query 断言。
//!
//! Recorded adapter 固定模型边界，保证 CI 可复现；接入真实 LLM 后可复用同一个 `run_case`。

mod common;

use std::cell::RefCell;
use std::collections::{HashSet, VecDeque};
use std::convert::Infallible;
use std::fmt::Debug;

use hippocampus::*;

#[derive(Debug)]
// 一个完整案例同时描述写入阶段、最终 Store snapshot 与未来查询。
struct RealCase {
    /// 断言失败时显示的业务场景名称。
    name: &'static str,
    /// 本案例使用的 predicate schema。
    predicates: Vec<(&'static str, Cardinality)>,
    /// 按发生顺序排列的多轮会话。
    sessions: Vec<CaseSession>,
    /// 所有写入完成、任何查询发生前的 Store 期望状态。
    expected_store: Vec<MemorySnapshot>,
    /// 在最终 Store 上执行的未来查询及期望结果。
    queries: Vec<CaseQuery>,
}

#[derive(Debug)]
/// 一次独立 conversation session。
struct CaseSession {
    /// 用于检查案例没有错误合并 session 的稳定标识。
    id: &'static str,
    /// Session 内按时间排序的用户输入。
    turns: Vec<CaseTurn>,
}

#[derive(Debug)]
/// 一条用户输入及其完整 write-side 期望。
struct CaseTurn {
    /// 本 turn 的 observed time。
    at: Timestamp,
    /// 交给 `MemoryExtractor` 的原始中文输入。
    input: &'static str,
    /// Extractor 应输出的 atomic candidates。
    expected_candidates: Vec<CandidateMemory>,
    /// 每个 candidate 对应的 admission/reconciliation 结果。
    expected_outcomes: Vec<ExpectedOutcome>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ExpectedOutcome {
    Applied(ReconciliationAction),
    Rejected,
    Ignored,
    Duplicate,
    Conflict,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// 写入完成后用于精确比较关键 Store 字段的快照。
struct MemorySnapshot {
    /// Memory 版本 ID。
    id: MemoryId,
    /// Atomic memory 类型。
    memory_type: MemoryType,
    /// Semantic slot 的 predicate。
    predicate: Predicate,
    /// 当前断言值。
    value: MemoryValue,
    /// Preference / Constraint 方向。
    polarity: Option<Polarity>,
    /// 适用范围。
    scope: Scope,
    /// 当前留存层级。
    retention_tier: RetentionTier,
    /// 当前生命周期状态。
    status: MemoryStatus,
    /// 新版本指向的旧版本。
    supersedes: Option<MemoryId>,
    /// 旧版本指向的替代版本。
    superseded_by: Option<MemoryId>,
    /// 旧版本被终止的原因。
    supersede_reason: Option<SupersedeReason>,
}

impl From<&Memory> for MemorySnapshot {
    fn from(memory: &Memory) -> Self {
        Self {
            id: memory.id,
            memory_type: memory.memory_type,
            predicate: memory.predicate.clone(),
            value: memory.value.clone(),
            polarity: memory.polarity,
            scope: memory.scope.clone(),
            retention_tier: memory.retention_tier,
            status: memory.status,
            supersedes: memory.supersedes,
            superseded_by: memory.superseded_by,
            supersede_reason: memory.supersede_reason,
        }
    }
}

#[derive(Debug)]
/// 一次 future query 的 query-understanding 与 retrieval 期望。
struct CaseQuery {
    /// 交给 `RuntimeContextBuilder` 的原始中文查询。
    input: &'static str,
    /// Query understanding 应产生的结构化上下文。
    expected_context: RuntimeContext,
    /// 各 retrieval lane 应返回的 Memory IDs。
    expected: RetrievalSnapshot,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
/// RetrievalResult 的稳定、易比较表示。
struct RetrievalSnapshot {
    /// Mandatory lane 中的 IDs。
    mandatory: Vec<MemoryId>,
    /// Constraint lane 中的 IDs。
    constraints: Vec<MemoryId>,
    /// Preference lane 中的 IDs。
    preferences: Vec<MemoryId>,
    /// Fact lane 中的 IDs。
    facts: Vec<MemoryId>,
    /// Episode lane 中的 IDs。
    episodes: Vec<MemoryId>,
    /// 无法安全裁决的 ID pairs。
    conflicts: Vec<(MemoryId, MemoryId)>,
}

impl From<&RetrievalResult> for RetrievalSnapshot {
    fn from(result: &RetrievalResult) -> Self {
        Self {
            mandatory: ids(&result.mandatory),
            constraints: ids(&result.constraints),
            preferences: ids(&result.preferences),
            facts: ids(&result.facts),
            episodes: ids(&result.episodes),
            conflicts: result
                .conflicts
                .iter()
                .map(|conflict| (conflict.left, conflict.right))
                .collect(),
        }
    }
}

fn ids(memories: &[Memory]) -> Vec<MemoryId> {
    memories.iter().map(|memory| memory.id).collect()
}

/// 按案例顺序重放已录制的 extractor 输出，使领域测试不依赖在线 LLM。
struct RecordedExtractor {
    /// 尚未消费的原始输入及对应 candidate fixtures。
    turns: RefCell<VecDeque<(&'static str, Vec<CandidateMemory>)>>,
}

impl RecordedExtractor {
    fn from_case(case: &RealCase) -> Self {
        Self {
            turns: RefCell::new(
                case.sessions
                    .iter()
                    .flat_map(|session| &session.turns)
                    .map(|turn| (turn.input, turn.expected_candidates.clone()))
                    .collect(),
            ),
        }
    }

    fn assert_exhausted(&self) {
        assert!(self.turns.borrow().is_empty());
    }
}

impl MemoryExtractor for RecordedExtractor {
    type Error = Infallible;

    fn extract(&self, conversation: &str) -> Result<Vec<CandidateMemory>, Self::Error> {
        let (expected_input, candidates) = self
            .turns
            .borrow_mut()
            .pop_front()
            .expect("unexpected extractor call");
        assert_eq!(conversation, expected_input);
        Ok(candidates)
    }
}

/// 按查询顺序重放已录制的 RuntimeContext。
struct RecordedContextBuilder {
    /// 尚未消费的原始查询及对应 context fixtures。
    queries: RefCell<VecDeque<(&'static str, RuntimeContext)>>,
}

impl RecordedContextBuilder {
    fn from_case(case: &RealCase) -> Self {
        Self {
            queries: RefCell::new(
                case.queries
                    .iter()
                    .map(|query| (query.input, query.expected_context.clone()))
                    .collect(),
            ),
        }
    }

    fn assert_exhausted(&self) {
        assert!(self.queries.borrow().is_empty());
    }
}

impl RuntimeContextBuilder for RecordedContextBuilder {
    type Error = Infallible;

    fn build(&self, query: &str) -> Result<RuntimeContext, Self::Error> {
        let (expected_input, context) = self
            .queries
            .borrow_mut()
            .pop_front()
            .expect("unexpected context-builder call");
        assert_eq!(query, expected_input);
        Ok(context)
    }
}

fn run_case<E, B>(case: &RealCase, extractor: &E, context_builder: &B)
where
    E: MemoryExtractor,
    E::Error: Debug,
    B: RuntimeContextBuilder,
    B::Error: Debug,
{
    let mut predicates = InMemoryPredicateRegistry::default();
    for (predicate, cardinality) in &case.predicates {
        predicates.register(Predicate::from(*predicate), *cardinality);
    }
    let mut engine = MemoryEngine::new(InMemoryMemoryRepository::default(), predicates);
    let mut session_ids = HashSet::new();
    let mut previous_time = None;

    // 先按 session/turn 顺序重放对话并核对每次抽取与 reconciliation。
    for session in &case.sessions {
        assert!(!session.id.is_empty(), "{}: empty session id", case.name);
        assert!(
            session_ids.insert(session.id),
            "{}: duplicate session id {}",
            case.name,
            session.id
        );
        for (turn_index, turn) in session.turns.iter().enumerate() {
            if let Some(previous) = previous_time {
                assert!(turn.at >= previous, "{}: time moved backwards", case.name);
            }
            previous_time = Some(turn.at);

            let actual_candidates = extractor.extract(turn.input).unwrap();
            assert_eq!(
                actual_candidates, turn.expected_candidates,
                "{}: candidate mismatch for {:?}",
                case.name, turn.input
            );
            assert_eq!(
                actual_candidates.len(),
                turn.expected_outcomes.len(),
                "{}: candidate/outcome count mismatch",
                case.name
            );
            let expected_turn_id = TurnId(format!("turn-{}", turn_index + 1));
            for candidate in &actual_candidates {
                assert_eq!(candidate.evidence.len(), 1);
                assert_eq!(
                    candidate.evidence[0].session_id,
                    SessionId::from(session.id)
                );
                assert_eq!(candidate.evidence[0].turn_id, expected_turn_id);
                assert_eq!(candidate.evidence[0].quote, turn.input);
            }

            for (candidate, expected) in actual_candidates.into_iter().zip(&turn.expected_outcomes)
            {
                assert_eq!(candidate.temporal.observed_at, turn.at);
                let report = engine
                    .ingest_with_trace(common::grounded(candidate))
                    .unwrap();
                assert!(!report.trace.admission_reason.is_empty());
                let actual = normalize_outcome(report.outcome.clone());
                assert_eq!(
                    actual, *expected,
                    "{}: ingest outcome mismatch; trace: {:#?}",
                    case.name, report.trace
                );
            }
        }
    }

    // 写入阶段结束后先断言 Store，避免 retrieval_count 影响基线状态。
    let mut actual_store: Vec<_> = engine
        .repository
        .list_by_owner(&owner())
        .unwrap()
        .iter()
        .map(MemorySnapshot::from)
        .collect();
    actual_store.sort_by_key(|memory| memory.id);
    assert_eq!(
        actual_store, case.expected_store,
        "{}: store mismatch",
        case.name
    );

    // 再重放未来查询，核对 query understanding 和最终各 retrieval lane。
    for query in &case.queries {
        let context = context_builder.build(query.input).unwrap();
        assert_eq!(context, query.expected_context);
        let result = DefaultMemoryRetriever::new(&mut engine.repository, &engine.predicates)
            .retrieve(&context)
            .unwrap();
        assert_eq!(
            RetrievalSnapshot::from(&result),
            query.expected,
            "{}: retrieval mismatch for {:?}; trace: {:#?}",
            case.name,
            query.input,
            result.trace
        );
    }
}

fn normalize_outcome(outcome: IngestOutcome) -> ExpectedOutcome {
    match outcome {
        IngestOutcome::Applied { action, .. } => ExpectedOutcome::Applied(action),
        IngestOutcome::Rejected(_) => ExpectedOutcome::Rejected,
        IngestOutcome::Ignored => ExpectedOutcome::Ignored,
        IngestOutcome::Duplicate { .. } => ExpectedOutcome::Duplicate,
        IngestOutcome::Conflict { .. } => ExpectedOutcome::Conflict,
    }
}

fn owner() -> UserId {
    UserId::from("user-1")
}

fn user() -> EntityId {
    EntityId::from("user-entity")
}

fn candidate(
    subject: EntityId,
    memory_type: MemoryType,
    predicate: &str,
    value: &str,
    at: i64,
    evidence: Evidence,
) -> CandidateMemory {
    CandidateMemory::new(
        owner(),
        subject,
        memory_type,
        Predicate::from(predicate),
        MemoryValue::from(value),
        Timestamp(at),
        evidence,
    )
}

fn evidence(session_id: &str, turn_id: &str, quote: &str) -> Evidence {
    Evidence::new(
        EvidenceId(format!("{session_id}:{turn_id}:0")),
        SessionId::from(session_id),
        TurnId::from(turn_id),
        quote,
    )
}

fn context(at: i64) -> RuntimeContext {
    let mut context = RuntimeContext::current(owner(), Timestamp(at));
    context.entities.push(user());
    context
}

fn snapshot(
    id: u64,
    memory_type: MemoryType,
    predicate: &str,
    value: &str,
    polarity: Option<Polarity>,
    scope: Scope,
    status: MemoryStatus,
) -> MemorySnapshot {
    MemorySnapshot {
        id: MemoryId(id),
        memory_type,
        predicate: Predicate::from(predicate),
        value: MemoryValue::from(value),
        polarity,
        scope,
        retention_tier: RetentionTier::LongTerm,
        status,
        supersedes: None,
        superseded_by: None,
        supersede_reason: None,
    }
}

fn run_recorded(case: RealCase) {
    let extractor = RecordedExtractor::from_case(&case);
    let context_builder = RecordedContextBuilder::from_case(&case);
    run_case(&case, &extractor, &context_builder);
    extractor.assert_exhausted();
    context_builder.assert_exhausted();
}

// 场景：跨三个 session 的咖啡偏好变化，以及独立的早餐硬约束。
#[test]
fn real_case_preference_change_and_breakfast_constraint() {
    let food = Scope::new(Some(Domain::from("food")), vec![]);
    let breakfast = Scope::new(
        Some(Domain::from("food")),
        vec![ScopeCondition::Activity("breakfast".to_owned())],
    );

    let mut likes = candidate(
        user(),
        MemoryType::Preference,
        "likes",
        "coffee",
        1,
        evidence("session-1", "turn-1", "我挺喜欢咖啡的。"),
    );
    likes.polarity = Some(Polarity::Positive);
    likes.scope = food.clone();

    let mut dislikes = candidate(
        user(),
        MemoryType::Preference,
        "likes",
        "coffee",
        20,
        evidence("session-2", "turn-1", "我现在有点不喜欢咖啡了。"),
    );
    dislikes.polarity = Some(Polarity::Negative);
    dislikes.scope = food.clone();
    dislikes.intent = CandidateIntent::Supersede {
        target: Some(MemoryId(1)),
        reason: SupersedeReason::PreferenceChanged,
    };

    let mut excludes = candidate(
        user(),
        MemoryType::Constraint,
        "exclude_drink",
        "coffee",
        30,
        evidence("session-3", "turn-1", "以后早餐别给我推荐咖啡。"),
    );
    excludes.polarity = Some(Polarity::Negative);
    excludes.scope = breakfast.clone();

    let mut breakfast_query = context(40);
    breakfast_query.domain = Some(Domain::from("food"));
    breakfast_query.conditions = breakfast.conditions.clone();

    let mut old = snapshot(
        1,
        MemoryType::Preference,
        "likes",
        "coffee",
        Some(Polarity::Positive),
        food.clone(),
        MemoryStatus::Superseded,
    );
    old.superseded_by = Some(MemoryId(2));
    old.supersede_reason = Some(SupersedeReason::PreferenceChanged);
    let mut current = snapshot(
        2,
        MemoryType::Preference,
        "likes",
        "coffee",
        Some(Polarity::Negative),
        food,
        MemoryStatus::Active,
    );
    current.supersedes = Some(MemoryId(1));

    run_recorded(RealCase {
        name: "preference change plus scoped constraint",
        predicates: vec![
            ("likes", Cardinality::Multiple),
            ("exclude_drink", Cardinality::Multiple),
        ],
        sessions: vec![
            CaseSession {
                id: "session-1",
                turns: vec![CaseTurn {
                    at: Timestamp(1),
                    input: "我挺喜欢咖啡的。",
                    expected_candidates: vec![likes],
                    expected_outcomes: vec![ExpectedOutcome::Applied(ReconciliationAction::New)],
                }],
            },
            CaseSession {
                id: "session-2",
                turns: vec![CaseTurn {
                    at: Timestamp(20),
                    input: "我现在有点不喜欢咖啡了。",
                    expected_candidates: vec![dislikes],
                    expected_outcomes: vec![ExpectedOutcome::Applied(
                        ReconciliationAction::Supersede,
                    )],
                }],
            },
            CaseSession {
                id: "session-3",
                turns: vec![CaseTurn {
                    at: Timestamp(30),
                    input: "以后早餐别给我推荐咖啡。",
                    expected_candidates: vec![excludes],
                    expected_outcomes: vec![ExpectedOutcome::Applied(ReconciliationAction::New)],
                }],
            },
        ],
        expected_store: vec![
            old,
            current,
            snapshot(
                3,
                MemoryType::Constraint,
                "exclude_drink",
                "coffee",
                Some(Polarity::Negative),
                breakfast,
                MemoryStatus::Active,
            ),
        ],
        queries: vec![CaseQuery {
            input: "早餐喝什么？",
            expected_context: breakfast_query,
            expected: RetrievalSnapshot {
                constraints: vec![MemoryId(3)],
                preferences: vec![MemoryId(2)],
                ..RetrievalSnapshot::default()
            },
        }],
    });
}

// 场景：“算了……就行”把全局禁咖啡修正为仅早餐生效。
#[test]
fn real_case_global_constraint_is_narrowed_by_follow_up() {
    let global_food = Scope::new(Some(Domain::from("food")), vec![]);
    let breakfast = Scope::new(
        Some(Domain::from("food")),
        vec![ScopeCondition::Activity("breakfast".to_owned())],
    );
    let mut global = candidate(
        user(),
        MemoryType::Constraint,
        "exclude_drink",
        "coffee",
        1,
        evidence("session-1", "turn-1", "以后都别给我推荐咖啡。"),
    );
    global.polarity = Some(Polarity::Negative);
    global.scope = global_food.clone();

    let mut narrowed = candidate(
        user(),
        MemoryType::Constraint,
        "exclude_drink",
        "coffee",
        2,
        evidence("session-1", "turn-2", "算了，早餐别推荐就行。"),
    );
    narrowed.polarity = Some(Polarity::Negative);
    narrowed.scope = breakfast.clone();
    narrowed.intent = CandidateIntent::Supersede {
        target: Some(MemoryId(1)),
        reason: SupersedeReason::ScopeRefined,
    };

    let mut old = snapshot(
        1,
        MemoryType::Constraint,
        "exclude_drink",
        "coffee",
        Some(Polarity::Negative),
        global_food,
        MemoryStatus::Superseded,
    );
    old.superseded_by = Some(MemoryId(2));
    old.supersede_reason = Some(SupersedeReason::ScopeRefined);
    let mut current = snapshot(
        2,
        MemoryType::Constraint,
        "exclude_drink",
        "coffee",
        Some(Polarity::Negative),
        breakfast.clone(),
        MemoryStatus::Active,
    );
    current.supersedes = Some(MemoryId(1));

    let mut evening_query = context(3);
    evening_query.domain = Some(Domain::from("food"));
    evening_query.conditions = vec![ScopeCondition::Activity("dinner".to_owned())];
    let mut breakfast_query = context(4);
    breakfast_query.domain = Some(Domain::from("food"));
    breakfast_query.conditions = breakfast.conditions.clone();

    run_recorded(RealCase {
        name: "pragmatic follow-up narrows global constraint",
        predicates: vec![("exclude_drink", Cardinality::Multiple)],
        sessions: vec![CaseSession {
            id: "session-1",
            turns: vec![
                CaseTurn {
                    at: Timestamp(1),
                    input: "以后都别给我推荐咖啡。",
                    expected_candidates: vec![global],
                    expected_outcomes: vec![ExpectedOutcome::Applied(ReconciliationAction::New)],
                },
                CaseTurn {
                    at: Timestamp(2),
                    input: "算了，早餐别推荐就行。",
                    expected_candidates: vec![narrowed],
                    expected_outcomes: vec![ExpectedOutcome::Applied(
                        ReconciliationAction::Supersede,
                    )],
                },
            ],
        }],
        expected_store: vec![old, current],
        queries: vec![
            CaseQuery {
                input: "晚餐喝什么？",
                expected_context: evening_query,
                expected: RetrievalSnapshot::default(),
            },
            CaseQuery {
                input: "早餐喝什么？",
                expected_context: breakfast_query,
                expected: RetrievalSnapshot {
                    constraints: vec![MemoryId(2)],
                    ..RetrievalSnapshot::default()
                },
            },
        ],
    });
}

// 场景：南京搬到阜阳；当前查询返回阜阳，搬家前的历史查询返回南京。
#[test]
fn real_case_state_change_supports_current_and_historical_queries() {
    let mut nanjing = candidate(
        user(),
        MemoryType::Fact,
        "primary_residence",
        "Nanjing",
        1,
        evidence("session-nanjing", "turn-1", "我现在住南京。"),
    );
    nanjing.temporal.valid_from = Some(DatedValue {
        at: Timestamp(1),
        source: TemporalSource::Explicit,
    });
    let mut fuyang = candidate(
        user(),
        MemoryType::Fact,
        "primary_residence",
        "Fuyang",
        10,
        evidence("session-fuyang", "turn-1", "我搬到阜阳了。"),
    );
    fuyang.temporal.valid_from = Some(DatedValue {
        at: Timestamp(10),
        source: TemporalSource::Explicit,
    });
    fuyang.intent = CandidateIntent::Supersede {
        target: Some(MemoryId(1)),
        reason: SupersedeReason::StateChanged,
    };

    let mut old = snapshot(
        1,
        MemoryType::Fact,
        "primary_residence",
        "Nanjing",
        None,
        Scope::global(),
        MemoryStatus::Superseded,
    );
    old.superseded_by = Some(MemoryId(2));
    old.supersede_reason = Some(SupersedeReason::StateChanged);
    let mut current = snapshot(
        2,
        MemoryType::Fact,
        "primary_residence",
        "Fuyang",
        None,
        Scope::global(),
        MemoryStatus::Active,
    );
    current.supersedes = Some(MemoryId(1));

    let current_query = context(20);
    let mut historical_query = context(5);
    historical_query.temporal_intent = TemporalIntent::Historical;

    run_recorded(RealCase {
        name: "moving house preserves historical truth",
        predicates: vec![("primary_residence", Cardinality::Single)],
        sessions: vec![
            CaseSession {
                id: "session-nanjing",
                turns: vec![CaseTurn {
                    at: Timestamp(1),
                    input: "我现在住南京。",
                    expected_candidates: vec![nanjing],
                    expected_outcomes: vec![ExpectedOutcome::Applied(ReconciliationAction::New)],
                }],
            },
            CaseSession {
                id: "session-fuyang",
                turns: vec![CaseTurn {
                    at: Timestamp(10),
                    input: "我搬到阜阳了。",
                    expected_candidates: vec![fuyang],
                    expected_outcomes: vec![ExpectedOutcome::Applied(
                        ReconciliationAction::Supersede,
                    )],
                }],
            },
        ],
        expected_store: vec![old, current],
        queries: vec![
            CaseQuery {
                input: "我现在住哪？",
                expected_context: current_query,
                expected: RetrievalSnapshot {
                    facts: vec![MemoryId(2)],
                    ..RetrievalSnapshot::default()
                },
            },
            CaseQuery {
                input: "我搬家前住哪？",
                expected_context: historical_query,
                expected: RetrievalSnapshot {
                    facts: vec![MemoryId(1)],
                    ..RetrievalSnapshot::default()
                },
            },
        ],
    });
}

// 场景：“他”有歧义时不入库，后续明确“小宝”后才保存偏好。
#[test]
fn real_case_ambiguous_subject_waits_for_clarification() {
    let child = EntityId::from("child-younger");
    let mut unresolved = candidate(
        child.clone(),
        MemoryType::Preference,
        "likes",
        "swimming",
        1,
        evidence("family-chat", "turn-1", "他不喜欢游泳。"),
    );
    unresolved.subject = CandidateSubject::Unresolved("他".to_owned());
    unresolved.polarity = Some(Polarity::Negative);

    let mut resolved = candidate(
        child.clone(),
        MemoryType::Preference,
        "likes",
        "swimming",
        2,
        evidence("family-chat", "turn-2", "我是说小宝。"),
    );
    resolved.polarity = Some(Polarity::Negative);

    let mut query = RuntimeContext::current(owner(), Timestamp(3));
    query.entities.push(child);

    run_recorded(RealCase {
        name: "ambiguous pronoun is clarified before admission",
        predicates: vec![("likes", Cardinality::Multiple)],
        sessions: vec![CaseSession {
            id: "family-chat",
            turns: vec![
                CaseTurn {
                    at: Timestamp(1),
                    input: "他不喜欢游泳。",
                    expected_candidates: vec![unresolved],
                    expected_outcomes: vec![ExpectedOutcome::Rejected],
                },
                CaseTurn {
                    at: Timestamp(2),
                    input: "我是说小宝。",
                    expected_candidates: vec![resolved],
                    expected_outcomes: vec![ExpectedOutcome::Applied(ReconciliationAction::New)],
                },
            ],
        }],
        expected_store: vec![snapshot(
            1,
            MemoryType::Preference,
            "likes",
            "swimming",
            Some(Polarity::Negative),
            Scope::global(),
            MemoryStatus::Active,
        )],
        queries: vec![CaseQuery {
            input: "小宝喜欢游泳吗？",
            expected_context: query,
            expected: RetrievalSnapshot {
                preferences: vec![MemoryId(1)],
                ..RetrievalSnapshot::default()
            },
        }],
    });
}
