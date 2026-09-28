//! Opt-in live integration suite derived from `docs/Personal-Memory-V1-测试用例整理.md`.
//!
//! Unlike recorded tests, every `live_*` test calls the configured OpenAI Responses API.
//! Live tests are ignored by default because they consume API quota and model output is stochastic.
//! Run smoke cases with:
//! `cargo test --test openai_live_scenarios live_smoke -- --ignored --nocapture --test-threads=1`
//! Run the full document suite with:
//! `cargo test --test openai_live_scenarios -- --ignored --nocapture --test-threads=1`
//! 校准口径见测试文档 §7：按实际 Memory ID、状态和 trace 验收，不重写模型输出。
//! PMV1-14 唯一允许故障注入；文件末尾的 calibration_* 是离线断言自测，不计 live 结果。

mod common;

use std::error::Error;
use std::sync::{Mutex, MutexGuard};

use hippocampus::*;

type TestResult = Result<(), Box<dyn Error>>;
type TestEngine = MemoryEngine<InMemoryMemoryRepository, InMemoryPredicateRegistry>;

// ponytail: 整组串行共享 API 配额；只有确有吞吐需求时再按账号拆锁。
static LIVE_API_LOCK: Mutex<()> = Mutex::new(());
const NOW: i64 = 1_788_494_400;

fn live_guard() -> MutexGuard<'static, ()> {
    LIVE_API_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn owner() -> UserId {
    UserId::from("live-user")
}

fn user() -> EntityId {
    EntityId::from("entity-user")
}

fn entities() -> Vec<Entity> {
    vec![
        entity(user(), EntityType::Person, "用户", &["我", "自己"]),
        entity(
            EntityId::from("child-older"),
            EntityType::Person,
            "大孩子",
            &["大儿子", "大的", "他", "儿子"],
        ),
        entity(
            EntityId::from("child-younger"),
            EntityType::Person,
            "小孩子",
            &["小儿子", "小的", "他", "儿子"],
        ),
        entity(
            EntityId::from("macbook-2018"),
            EntityType::Device,
            "旧 MacBook",
            &["MacBook", "以前那台 MacBook", "旧电脑"],
        ),
        entity(
            EntityId::from("thinkpad"),
            EntityType::Device,
            "ThinkPad",
            &["新电脑"],
        ),
        entity(
            EntityId::from("vehicle-1"),
            EntityType::Device,
            "我的车",
            &["车", "汽车"],
        ),
    ]
}

fn entity(id: EntityId, entity_type: EntityType, name: &str, aliases: &[&str]) -> Entity {
    Entity {
        id,
        owner_id: owner(),
        entity_type,
        canonical_name: name.to_owned(),
        aliases: aliases.iter().map(|alias| (*alias).to_owned()).collect(),
    }
}

#[derive(Clone)]
struct WriteResult {
    candidate: CandidateMemory,
    report: IngestReport,
}

struct LiveHarness {
    client: OpenAiClient,
    definitions: Vec<PredicateDefinition>,
    known_entities: Vec<Entity>,
    task_id: Option<TaskId>,
    entity_repository: InMemoryEntityRepository,
    engine: TestEngine,
}

impl LiveHarness {
    fn new(predicates: &[(&str, Cardinality)]) -> Result<Self, OpenAiAdapterError> {
        let definitions = predicates
            .iter()
            .map(|(predicate, cardinality)| PredicateDefinition {
                predicate: Predicate::from(*predicate),
                cardinality: *cardinality,
            })
            .collect::<Vec<_>>();
        let mut registry = InMemoryPredicateRegistry::default();
        for definition in &definitions {
            registry.register(definition.predicate.clone(), definition.cardinality);
        }
        let known_entities = entities();
        let mut entity_repository = InMemoryEntityRepository::default();
        for entity in &known_entities {
            entity_repository.upsert(entity.clone());
        }
        Ok(Self {
            client: OpenAiClient::from_env()?,
            definitions,
            known_entities,
            task_id: None,
            entity_repository,
            engine: MemoryEngine::new(InMemoryMemoryRepository::default(), registry),
        })
    }

    fn context(&self, at: i64) -> Result<OpenAiAdapterContext, OpenAiAdapterError> {
        let context =
            OpenAiAdapterContext::new(owner(), self.known_entities.clone(), Timestamp(at))?
                .with_utc_offset_seconds(8 * 60 * 60)?
                .with_semantic_catalog(
                    self.definitions.clone(),
                    ["food", "travel", "programming"]
                        .into_iter()
                        .map(Domain::from)
                        .collect(),
                )?
                .with_predicate_value_kinds(
                    self.definitions
                        .iter()
                        .filter_map(|definition| {
                            use MemoryValueKind::*;
                            let kinds: &[MemoryValueKind] = match definition.predicate.0.as_str() {
                                "control" => return None,
                                "age" => &[Text, Integer],
                                "hotel_budget" => &[Integer],
                                "owns_device" => &[Entity],
                                "attitude_toward_kindergarten"
                                | "willingness_to_attend_kindergarten"
                                | "vehicle_broken" => &[Text, Boolean],
                                _ => &[Text],
                            };
                            Some((definition.predicate.clone(), kinds.to_vec()))
                        })
                        .collect(),
                )?
                .with_predicate_memory_types(
                    self.definitions
                        .iter()
                        .filter(|definition| definition.predicate.0 == "visited")
                        .map(|definition| (definition.predicate.clone(), vec![MemoryType::Episode]))
                        .collect(),
                )?;
        match &self.task_id {
            Some(task) => context.with_task_id(task.clone()),
            None => Ok(context),
        }
    }

    fn extract(
        &self,
        session: &str,
        turn: &str,
        at: i64,
        history: &[&str],
        current: &str,
    ) -> Result<Vec<CandidateMemory>, OpenAiAdapterError> {
        let candidates = OpenAiMemoryExtractor::new(
            self.client.clone(),
            self.context(at)?,
            SessionId::from(session),
            TurnId::from(turn),
        )
        .extract_turn(history, current)?;
        // Live failures must expose the actual model decision, not only the final assertion.
        eprintln!("[live][{session}/{turn}] candidates={candidates:#?}");
        Ok(candidates)
    }

    fn ingest(&mut self, candidates: Vec<CandidateMemory>) -> Result<Vec<WriteResult>, StoreError> {
        candidates
            .into_iter()
            .map(|candidate| {
                let report = self.engine.ingest_with_trace(candidate.clone())?;
                eprintln!(
                    "[live][ingest/{}] outcome={:?} trace={:#?}",
                    candidate.predicate.0, report.outcome, report.trace
                );
                Ok(WriteResult { candidate, report })
            })
            .collect()
    }

    fn build_query(&self, at: i64, query: &str) -> Result<RuntimeContext, OpenAiAdapterError> {
        let context = OpenAiRuntimeContextBuilder::new(self.client.clone(), self.context(at)?)
            .build(query)?;
        eprintln!("[live][query/{query}] context={context:#?}");
        Ok(context)
    }

    fn retrieve(&mut self, context: &RuntimeContext) -> Result<RetrievalResult, StoreError> {
        let result =
            DefaultMemoryRetriever::new(&mut self.engine.repository, &self.engine.predicates)
                .retrieve(context)?;
        eprintln!("[live][retrieval] result={result:#?}");
        let memories = self.memories()?;
        let mut traced = result
            .trace
            .iter()
            .map(|entry| entry.memory_id)
            .collect::<Vec<_>>();
        let mut stored = memories.iter().map(|memory| memory.id).collect::<Vec<_>>();
        traced.sort();
        stored.sort();
        assert_eq!(traced, stored, "每条 Store Memory 必须恰有一个检索决定");
        Ok(result)
    }

    fn memories(&self) -> Result<Vec<Memory>, StoreError> {
        self.engine.repository.list_by_owner(&owner())
    }

    fn memory(&self, id: MemoryId) -> Result<Memory, StoreError> {
        self.engine
            .repository
            .get(id)?
            .ok_or_else(|| StoreError(format!("missing memory {id:?}")))
    }
}

fn candidate_for<'a>(
    candidates: &'a [CandidateMemory],
    memory_type: MemoryType,
    predicate: &str,
) -> &'a CandidateMemory {
    candidate_with_types(candidates, &[memory_type], predicate)
}

// 仅在文档允许等价表达的场景使用多类型；不能让一条正确候选掩盖同谓词的额外错误候选。
fn candidate_with_types<'a>(
    candidates: &'a [CandidateMemory],
    allowed: &[MemoryType],
    predicate: &str,
) -> &'a CandidateMemory {
    let matches = candidates
        .iter()
        .filter(|candidate| candidate.predicate == Predicate::from(predicate))
        .collect::<Vec<_>>();
    assert_eq!(
        matches.len(),
        1,
        "expected one {predicate} candidate, got {candidates:#?}"
    );
    assert!(
        allowed.contains(&matches[0].memory_type),
        "expected {allowed:?}/{predicate}, got {candidates:#?}"
    );
    matches[0]
}

fn written_id(writes: &[WriteResult], predicate: &str, action: ReconciliationAction) -> MemoryId {
    assert_eq!(
        action_for(writes, predicate),
        action,
        "predicate={predicate}"
    );
    let write = writes
        .iter()
        .find(|write| write.candidate.predicate == Predicate::from(predicate))
        .unwrap();
    match write.report.outcome {
        IngestOutcome::Applied {
            memory_id: Some(id),
            ..
        } => id,
        ref other => panic!("需要真实入库记录，实际为 {other:?}"),
    }
}

fn assert_ids(memories: &[Memory], expected: &[MemoryId]) {
    let mut actual = memories.iter().map(|memory| memory.id).collect::<Vec<_>>();
    let mut expected = expected.to_vec();
    actual.sort();
    expected.sort();
    assert_eq!(
        actual, expected,
        "必须召回正确版本，不能仅检查非空或数量：{memories:#?}"
    );
}

fn assert_decision(result: &RetrievalResult, id: MemoryId, expected: RetrievalDecision) {
    let actual = result
        .trace
        .iter()
        .find(|entry| entry.memory_id == id)
        .map(|entry| entry.decision);
    assert_eq!(actual, Some(expected), "memory={id:?}, result={result:#?}");
}

fn descriptive_lane(result: &RetrievalResult, memory_type: MemoryType) -> &[Memory] {
    match memory_type {
        MemoryType::Fact => &result.facts,
        MemoryType::Episode => &result.episodes,
        other => panic!("行为/习惯不能变成 {other:?}"),
    }
}

// 所有权的 None 与 Positive 都表示肯定；Negative 是“不再拥有”的证据，不算当前设备。
fn assert_owned_devices(memories: &[Memory], expected: &[EntityId]) {
    let mut actual = Vec::new();
    for memory in memories
        .iter()
        .filter(|memory| memory.predicate == Predicate::from("owns_device"))
    {
        assert_eq!(memory.memory_type, MemoryType::Fact);
        assert_eq!(memory.subject_id, user());
        assert_eq!(memory.status, MemoryStatus::Active);
        let MemoryValue::Entity(entity) = &memory.value else {
            panic!("设备所有权必须保留 Entity 身份：{memory:#?}");
        };
        if memory.polarity != Some(Polarity::Negative) {
            actual.push(entity.clone());
        }
    }
    actual.sort();
    let mut expected = expected.to_vec();
    expected.sort();
    assert_eq!(actual, expected, "当前拥有的设备不正确：{memories:#?}");
}

fn action_for(writes: &[WriteResult], predicate: &str) -> ReconciliationAction {
    let writes = writes
        .iter()
        .filter(|write| write.candidate.predicate == Predicate::from(predicate))
        .collect::<Vec<_>>();
    assert_eq!(writes.len(), 1, "unexpected writes for {predicate}");
    match writes[0].report.outcome {
        IngestOutcome::Applied { action, .. } => action,
        ref outcome => panic!(
            "expected applied {predicate}; got {outcome:?}; trace={:#?}",
            writes[0].report.trace
        ),
    }
}

fn lane_has(memories: &[Memory], predicate: &str) -> bool {
    memories
        .iter()
        .any(|memory| memory.predicate == Predicate::from(predicate))
}

fn assert_trace_complete(result: &RetrievalResult, store_size: usize) {
    assert_eq!(result.trace.len(), store_size, "trace={:#?}", result.trace);
}

fn all_lanes_empty(result: &RetrievalResult) -> bool {
    result.mandatory.is_empty()
        && result.constraints.is_empty()
        && result.preferences.is_empty()
        && result.facts.is_empty()
        && result.episodes.is_empty()
        && result.conflicts.is_empty()
}

macro_rules! live_test {
    ($name:ident, $body:block) => {
        #[test]
        #[ignore = "requires DEEPSEEK_BASE_URL/DEEPSEEK_API_KEY and consumes live API quota"]
        fn $name() -> TestResult {
            let _guard = live_guard();
            $body
        }
    };
}

// PMV1-01：真实模型分别理解偏好变化和早餐硬约束，再通过正式 Store/Query 流程。
live_test!(live_smoke_pmv1_01_preference_change_and_constraint, {
    let mut harness = LiveHarness::new(&[
        ("likes", Cardinality::Multiple),
        ("exclude_drink", Cardinality::Multiple),
    ])?;
    let first = harness.extract("S1", "T1", NOW, &[], "我挺喜欢咖啡的。")?;
    assert_eq!(
        candidate_for(&first, MemoryType::Preference, "likes").polarity,
        Some(Polarity::Positive)
    );
    assert_eq!(
        action_for(&harness.ingest(first)?, "likes"),
        ReconciliationAction::New
    );
    let second = harness.extract(
        "S2",
        "T1",
        NOW + 1,
        &["我挺喜欢咖啡的。"],
        "我现在有点不喜欢咖啡了。",
    )?;
    assert_eq!(
        candidate_for(&second, MemoryType::Preference, "likes").polarity,
        Some(Polarity::Negative)
    );
    assert_eq!(
        action_for(&harness.ingest(second)?, "likes"),
        ReconciliationAction::Supersede
    );
    let third = harness.extract("S3", "T1", NOW + 2, &[], "以后早餐别给我推荐咖啡。")?;
    let constraint = candidate_for(&third, MemoryType::Constraint, "exclude_drink");
    assert_eq!(constraint.polarity, Some(Polarity::Negative));
    assert!(constraint.scope.domain.is_some());
    assert_eq!(
        action_for(&harness.ingest(third)?, "exclude_drink"),
        ReconciliationAction::New
    );
    let context = harness.build_query(NOW + 3, "早餐喝什么？")?;
    let result = harness.retrieve(&context)?;
    assert!(lane_has(&result.preferences, "likes"), "result={result:#?}");
    assert!(
        lane_has(&result.constraints, "exclude_drink"),
        "result={result:#?}"
    );
    assert_trace_complete(&result, harness.memories()?.len());
    Ok(())
});

// PMV1-02：家庭旅行一般规则与日本特例。
live_test!(live_full_pmv1_02_family_travel_exception, {
    let mut harness = LiveHarness::new(&[("homestay", Cardinality::Multiple)])?;
    let general = harness.extract("S1", "T1", NOW, &[], "以后家庭旅行不要给我推荐民宿。")?;
    let general = candidate_for(&general, MemoryType::Constraint, "homestay");
    assert!(general.scope.conditions.iter().any(|condition| matches!(
        condition,
        ScopeCondition::Companion(EntityOrRole::Role(
            CompanionRole::Children | CompanionRole::Family
        ))
    )));
    assert_eq!(general.polarity, Some(Polarity::Negative));
    let general_id = written_id(
        &harness.ingest(vec![general.clone()])?,
        "homestay",
        ReconciliationAction::New,
    );
    let exception = harness.extract(
        "S1",
        "T2",
        NOW + 1,
        &["以后家庭旅行不要给我推荐民宿。"],
        "不过带孩子去日本，温泉民宿可以考虑。",
    )?;
    let exception_candidate = candidate_for(&exception, MemoryType::Constraint, "homestay");
    assert_eq!(exception_candidate.polarity, Some(Polarity::Positive));
    assert!(exception_candidate.scope.conditions.iter().any(|condition| matches!(
        condition,
        ScopeCondition::Location(LocationScope::Country(country)) if country == "JP" || country == "日本"
    )));
    let exception_id = written_id(
        &harness.ingest(exception)?,
        "homestay",
        ReconciliationAction::New,
    );
    assert_eq!(harness.memory(general_id)?.status, MemoryStatus::Active);
    assert_eq!(harness.memory(exception_id)?.status, MemoryStatus::Active);
    let yunnan_context = harness.build_query(NOW + 2, "带孩子去云南住哪？")?;
    let yunnan = harness.retrieve(&yunnan_context)?;
    assert_ids(&yunnan.constraints, &[general_id]);
    let japan_context = harness.build_query(NOW + 3, "带孩子去日本住哪？")?;
    let japan = harness.retrieve(&japan_context)?;
    assert_ids(&japan.constraints, &[exception_id]);
    assert_decision(
        &japan,
        general_id,
        RetrievalDecision::Filtered(RetrievalFilterReason::OverriddenBy(exception_id)),
    );
    assert_trace_complete(&japan, harness.memories()?.len());
    Ok(())
});

// PMV1-03：自然语言“算了……就行”必须形成 ScopeRefined。
live_test!(live_full_pmv1_03_scope_refinement, {
    let mut harness = LiveHarness::new(&[("exclude_drink", Cardinality::Multiple)])?;
    let first = harness.extract("S1", "T1", NOW, &[], "以后都别给我推荐咖啡。")?;
    harness.ingest(first)?;
    let second = harness.extract(
        "S1",
        "T2",
        NOW + 1,
        &["以后都别给我推荐咖啡。"],
        "算了，早餐别推荐就行。",
    )?;
    let candidate = candidate_for(&second, MemoryType::Constraint, "exclude_drink");
    assert!(matches!(
        candidate.intent,
        CandidateIntent::Supersede {
            reason: SupersedeReason::ScopeRefined,
            ..
        }
    ));
    assert_eq!(
        action_for(&harness.ingest(second)?, "exclude_drink"),
        ReconciliationAction::Supersede
    );
    let dinner = harness.build_query(NOW + 2, "晚餐喝什么？")?;
    assert!(all_lanes_empty(&harness.retrieve(&dinner)?));
    let breakfast = harness.build_query(NOW + 3, "早餐喝什么？")?;
    assert!(lane_has(
        &harness.retrieve(&breakfast)?.constraints,
        "exclude_drink"
    ));
    Ok(())
});

// PMV1-04：真实模型识别住所状态变化及 current/historical 查询。
live_test!(live_full_pmv1_04_residence_history, {
    let mut harness = LiveHarness::new(&[("primary_residence", Cardinality::Single)])?;
    let first = harness.extract("S1", "T1", NOW, &[], "我现在住南京。")?;
    let old_id = written_id(
        &harness.ingest(first)?,
        "primary_residence",
        ReconciliationAction::New,
    );
    let second = harness.extract("S2", "T1", NOW + 10, &["我现在住南京。"], "我搬到阜阳了。")?;
    assert!(matches!(
        candidate_for(&second, MemoryType::Fact, "primary_residence").intent,
        CandidateIntent::Supersede {
            reason: SupersedeReason::StateChanged,
            ..
        }
    ));
    let new_id = written_id(
        &harness.ingest(second)?,
        "primary_residence",
        ReconciliationAction::Supersede,
    );
    assert_eq!(harness.memory(old_id)?.status, MemoryStatus::Superseded);
    assert_eq!(harness.memory(new_id)?.supersedes, Some(old_id));
    let current = harness.build_query(NOW + 20, "我现在住哪？")?;
    assert_ids(&harness.retrieve(&current)?.facts, &[new_id]);
    let history = harness.build_query(NOW + 20, "我搬到阜阳之前住哪？")?;
    assert_eq!(history.temporal_intent, TemporalIntent::Historical);
    assert_ids(&harness.retrieve(&history)?.facts, &[old_id]);
    Ok(())
});

// PMV1-05：Correction 不得让错误月份成为历史事实。
live_test!(live_full_pmv1_05_correction, {
    let mut harness = LiveHarness::new(&[("visited", Cardinality::Multiple)])?;
    let first = harness.extract("S1", "T1", NOW, &[], "我去年 7 月去了云南。")?;
    let old_id = written_id(
        &harness.ingest(first)?,
        "visited",
        ReconciliationAction::New,
    );
    let second = harness.extract(
        "S1",
        "T2",
        NOW + 1,
        &["我去年 7 月去了云南。"],
        "说错了，是 6 月。",
    )?;
    assert!(matches!(
        candidate_for(&second, MemoryType::Episode, "visited").intent,
        CandidateIntent::Supersede {
            reason: SupersedeReason::Correction,
            ..
        }
    ));
    let new_id = written_id(
        &harness.ingest(second)?,
        "visited",
        ReconciliationAction::Supersede,
    );
    assert_eq!(harness.memory(old_id)?.status, MemoryStatus::Superseded);
    assert_eq!(
        harness.memory(old_id)?.supersede_reason,
        Some(SupersedeReason::Correction)
    );
    let context = harness.build_query(NOW + 2, "我什么时候去云南的？")?;
    let result = harness.retrieve(&context)?;
    assert_ids(&result.episodes, &[new_id]);
    assert_eq!(result.episodes[0].supersedes, Some(old_id));
    assert_decision(
        &result,
        old_id,
        RetrievalDecision::Filtered(RetrievalFilterReason::Status),
    );
    Ok(())
});

// PMV1-06：当前所有权按 Entity + polarity 验收；售出可撤销，或形成否定的新状态。
live_test!(live_full_pmv1_06_device_lifecycle, {
    let mut harness = LiveHarness::new(&[
        ("owns_device", Cardinality::Multiple),
        ("device_issue", Cardinality::Multiple),
    ])?;
    let old_id = written_id(
        &harness.ingest(harness.extract("S1", "T1", NOW, &[], "我有一台 MacBook。")?)?,
        "owns_device",
        ReconciliationAction::New,
    );
    let kept_id = written_id(
        &harness.ingest(harness.extract("S2", "T1", NOW + 1, &[], "我又买了一台 ThinkPad。")?)?,
        "owns_device",
        ReconciliationAction::New,
    );
    let sold = harness.extract(
        "S3",
        "T1",
        NOW + 2,
        &["我有一台 MacBook。", "我又买了一台 ThinkPad。"],
        "旧 MacBook 已经卖掉了。",
    )?;
    assert!(matches!(
        candidate_for(&sold, MemoryType::Fact, "owns_device").intent,
        CandidateIntent::Retract { .. }
            | CandidateIntent::Supersede {
                reason: SupersedeReason::StateChanged,
                ..
            }
    ));
    let sold_writes = harness.ingest(sold)?;
    assert!(matches!(
        action_for(&sold_writes, "owns_device"),
        ReconciliationAction::Retract | ReconciliationAction::Supersede
    ));
    assert_eq!(harness.memory(old_id)?.status, MemoryStatus::Superseded);
    assert_eq!(harness.memory(kept_id)?.status, MemoryStatus::Active);
    let active = harness
        .memories()?
        .into_iter()
        .filter(|memory| memory.status == MemoryStatus::Active)
        .collect::<Vec<_>>();
    assert_owned_devices(&active, &[EntityId::from("thinkpad")]);
    let current = harness.build_query(NOW + 3, "我现在有哪些电脑？")?;
    let result = harness.retrieve(&current)?;
    assert_owned_devices(&result.facts, &[EntityId::from("thinkpad")]);
    assert_decision(
        &result,
        old_id,
        RetrievalDecision::Filtered(RetrievalFilterReason::Status),
    );
    Ok(())
});

// PMV1-07：首轮真实模型输出 unresolved；次轮真实模型结合历史解析“小的”。
live_test!(live_smoke_pmv1_07_ambiguous_pronoun_staging, {
    let mut harness = LiveHarness::new(&[("likes", Cardinality::Multiple)])?;
    let first = harness.extract("S1", "T1", NOW, &[], "他不喜欢游泳。")?;
    let unresolved = candidate_for(&first, MemoryType::Preference, "likes").clone();
    assert!(matches!(
        unresolved.subject,
        CandidateSubject::Unresolved(_)
    ));
    let staged_id = match harness
        .engine
        .ingest_or_stage(unresolved, &harness.entity_repository)?
    {
        CandidateIngestOutcome::Pending { staged_id, .. } => staged_id,
        other => panic!("expected pending candidate, got {other:?}"),
    };
    assert!(harness.memories()?.is_empty(), "未澄清的代词不能进入 Store");
    let clarification =
        harness.extract("S1", "T2", NOW + 1, &["他不喜欢游泳。"], "我是说小的。")?;
    let resolved = candidate_for(&clarification, MemoryType::Preference, "likes");
    let CandidateSubject::Resolved(entity_id) = &resolved.subject else {
        panic!("clarification did not resolve an entity: {clarification:#?}");
    };
    assert_eq!(*entity_id, EntityId::from("child-younger"));
    let entity = harness.entity_repository.get(entity_id).unwrap();
    let outcome = harness.engine.clarify_staged(
        staged_id,
        &entity.canonical_name,
        Evidence::new(
            EvidenceId::from("S1:T2:clarification"),
            SessionId::from("S1"),
            TurnId::from("T2"),
            "我是说小的。",
        ),
        &harness.entity_repository,
    )?;
    let id = match outcome {
        CandidateIngestOutcome::Ingested(IngestOutcome::Applied {
            action: ReconciliationAction::New,
            memory_id: Some(id),
            ..
        }) => id,
        other => panic!("expected resolved write, got {other:?}"),
    };
    assert_eq!(
        harness.memory(id)?.subject_id,
        EntityId::from("child-younger")
    );
    assert_eq!(harness.memory(id)?.evidence.len(), 2);
    let context = harness.build_query(NOW + 2, "小孩子喜欢游泳吗？")?;
    let result = harness.retrieve(&context)?;
    assert_ids(&result.preferences, &[id]);
    Ok(())
});

// PMV1-08：模糊 certainty 与模型理解 confidence 分离，修正不粗暴翻成 Positive。
live_test!(live_full_pmv1_08_uncertain_kindergarten_statement, {
    let mut harness = LiveHarness::new(&[
        ("attitude_toward_kindergarten", Cardinality::Single),
        ("willingness_to_attend_kindergarten", Cardinality::Single),
    ])?;
    let first = harness.extract("S1", "T1", NOW, &[], "我大儿子最近好像不太喜欢幼儿园。")?;
    let first_candidate = candidate_for(&first, MemoryType::Fact, "attitude_toward_kindergarten");
    assert_eq!(first_candidate.extraction_confidence, Confidence::High);
    assert_eq!(first_candidate.certainty, Confidence::Low);
    harness.ingest(first)?;
    let second = harness.extract(
        "S1",
        "T2",
        NOW + 1,
        &["我大儿子最近好像不太喜欢幼儿园。"],
        "我觉得可能只是最近不想去，不一定是不喜欢。",
    )?;
    assert!(
        second.iter().any(|candidate| matches!(
            candidate.intent,
            CandidateIntent::Retract { .. } | CandidateIntent::Supersede { .. }
        )),
        "candidates={second:#?}"
    );
    assert!(
        second.iter().any(|candidate| {
            candidate.predicate == Predicate::from("willingness_to_attend_kindergarten")
        }),
        "candidates={second:#?}"
    );
    harness.ingest(second)?;
    let context = harness.build_query(NOW + 2, "他最近对上幼儿园是什么态度？")?;
    assert!(!harness.retrieve(&context)?.facts.is_empty());
    Ok(())
});

// PMV1-09：三个独立真实 turn 应形成 Reinforce、Reinforce 并触发升级。
live_test!(live_full_pmv1_09_reinforcement_retention, {
    let mut harness = LiveHarness::new(&[("learning", Cardinality::Multiple)])?;
    let turns = [
        ("S1", "我最近开始学 Rust。"),
        ("S2", "最近还在学 Rust。"),
        ("S3", "现在很多项目我都开始用 Rust 写了。"),
    ];
    for (index, (session, text)) in turns.into_iter().enumerate() {
        let candidates = harness.extract(session, "T1", NOW + index as i64, &[], text)?;
        let candidate = candidate_for(&candidates, MemoryType::Fact, "learning");
        // 初次落库必须短期；后续候选的 tier 不替代对真实 Store 升级时机的验收。
        if index == 0 {
            assert_eq!(candidate.retention_tier, RetentionTier::ShortTerm);
        }
        let writes = harness.ingest(candidates)?;
        let expected = if index == 0 {
            ReconciliationAction::New
        } else {
            ReconciliationAction::Reinforce
        };
        assert_eq!(action_for(&writes, "learning"), expected);
        let memories = harness.memories()?;
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].reinforcement_count, index as u32);
        assert_eq!(
            memories[0].retention_tier,
            if index < 2 {
                RetentionTier::ShortTerm
            } else {
                RetentionTier::LongTerm
            }
        );
    }
    let memories = harness.memories()?;
    assert_eq!(memories.len(), 1);
    assert_eq!(memories[0].reinforcement_count, 2);
    assert_eq!(memories[0].retention_tier, RetentionTier::LongTerm);
    assert_eq!(memories[0].evidence.len(), 3);
    let context = harness.build_query(NOW + 3, "我最近在学什么编程语言？")?;
    assert_ids(&harness.retrieve(&context)?.facts, &[memories[0].id]);
    let retrieved = harness.memory(memories[0].id)?;
    assert_eq!(retrieved.retrieval_count, 1);
    assert_eq!(retrieved.reinforcement_count, 2, "检索不计独立强化");
    Ok(())
});

// PMV1-10：真实模型抽取 task budget；过期由确定性 Retention 执行。
live_test!(live_full_pmv1_10_task_budget_expiry, {
    let mut harness = LiveHarness::new(&[("hotel_budget", Cardinality::Single)])?;
    harness.task_id = Some(TaskId::from("trip-2026-09-04-a"));
    let candidates = harness.extract(
        "S1",
        "T1",
        NOW,
        &[],
        "这次去上海酒店预算控制在 800，行程明天结束。",
    )?;
    let candidate = candidate_for(&candidates, MemoryType::Constraint, "hotel_budget");
    assert_eq!(candidate.retention_tier, RetentionTier::ShortTerm);
    assert!(
        candidate.temporal.expires_at.is_some(),
        "candidate={candidate:#?}"
    );
    harness.ingest(candidates)?;
    harness
        .engine
        .run_retention_for_owner(&owner(), Timestamp(NOW + 172_800))?;
    assert!(
        harness
            .memories()?
            .iter()
            .any(|memory| memory.status == MemoryStatus::Expired)
    );
    Ok(())
});

// PMV1-11：真实模型的全局 Durable 中文约束必须完整入库并 mandatory 召回。
live_test!(live_smoke_pmv1_11_durable_language_rule, {
    let mut harness = LiveHarness::new(&[("response_language", Cardinality::Single)])?;
    let candidates = harness.extract(
        "S1",
        "T1",
        NOW,
        &[],
        "请记住：无论讨论什么主题，以后所有回答都必须使用中文。",
    )?;
    let candidate = candidate_for(&candidates, MemoryType::Constraint, "response_language");
    assert_eq!(candidate.retention_tier, RetentionTier::Durable);
    assert_eq!(candidate.scope, Scope::global());
    harness.ingest(candidates)?;
    let context = harness.build_query(NOW + 1, "Explain Rust Pin.")?;
    let result = harness.retrieve(&context)?;
    assert!(
        lane_has(&result.mandatory, "response_language"),
        "result={result:#?}"
    );
    Ok(())
});

// PMV1-12：Assistant 假设不得入库，用户确认后才保存。
live_test!(live_full_pmv1_12_assistant_inference_confirmation, {
    let mut harness = LiveHarness::new(&[("prefers_hotel", Cardinality::Multiple)])?;
    let inferred = harness.extract(
        "S0",
        "T1",
        NOW,
        &[],
        "Assistant：看起来你应该挺喜欢亚朵。用户没有回应。",
    )?;
    let inferred_writes = harness.ingest(inferred)?;
    assert!(
        inferred_writes.iter().all(|write| matches!(
            write.report.outcome,
            IngestOutcome::Rejected(_) | IngestOutcome::Ignored
        )),
        "writes={:#?}",
        inferred_writes
            .iter()
            .map(|write| &write.report)
            .collect::<Vec<_>>()
    );
    assert!(harness.memories()?.is_empty());
    let confirmed = harness.extract(
        "S1",
        "T1",
        NOW + 1,
        &["Assistant：看起来你应该挺喜欢亚朵。用户没有回应。"],
        "对，我确实一般优先住亚朵。",
    )?;
    assert_eq!(
        candidate_for(&confirmed, MemoryType::Preference, "prefers_hotel").source,
        CandidateSource::UserConfirmed
    );
    harness.ingest(confirmed)?;
    let context = harness.build_query(NOW + 2, "出差住什么酒店？")?;
    assert!(lane_has(
        &harness.retrieve(&context)?.preferences,
        "prefers_hotel"
    ));
    Ok(())
});

// PMV1-13：真实撤销语句不生成反向 Preference。
live_test!(live_full_pmv1_13_retract_constraint, {
    let mut harness = LiveHarness::new(&[("exclude_flight", Cardinality::Multiple)])?;
    let first = harness.extract("S1", "T1", NOW, &[], "不要红眼航班。")?;
    let id = written_id(
        &harness.ingest(first)?,
        "exclude_flight",
        ReconciliationAction::New,
    );
    let second = harness.extract(
        "S1",
        "T2",
        NOW + 1,
        &["不要红眼航班。"],
        "这个限制取消吧，以后红眼也可以。",
    )?;
    assert!(matches!(
        candidate_for(&second, MemoryType::Constraint, "exclude_flight").intent,
        CandidateIntent::Retract { .. }
    ));
    assert_eq!(second.len(), 1, "取消限制不能额外写入偏好");
    assert_eq!(
        action_for(&harness.ingest(second)?, "exclude_flight"),
        ReconciliationAction::Retract
    );
    assert_eq!(harness.memory(id)?.status, MemoryStatus::Superseded);
    assert_eq!(harness.memories()?.len(), 1);
    let context = harness.build_query(NOW + 2, "这次航班怎么选？")?;
    let result = harness.retrieve(&context)?;
    assert!(!lane_has(&result.constraints, "exclude_flight"));
    assert!(result.preferences.is_empty());
    assert_decision(
        &result,
        id,
        RetrievalDecision::Filtered(RetrievalFilterReason::Status),
    );
    Ok(())
});

// PMV1-14：按文档注入同槽位、同 Scope 的矛盾记录，必须真的运行冲突检索。
live_test!(live_full_pmv1_14_query_conflict_context, {
    let mut harness = LiveHarness::new(&[("exclude_drink", Cardinality::Multiple)])?;
    let extracted = harness.extract("S1", "T1", NOW, &[], "早餐不要推荐咖啡。")?;
    assert_eq!(
        candidate_for(&extracted, MemoryType::Constraint, "exclude_drink").polarity,
        Some(Polarity::Negative)
    );
    let original_id = written_id(
        &harness.ingest(extracted)?,
        "exclude_drink",
        ReconciliationAction::New,
    );
    // 唯一非自然写入夹具：复用真实模型的结构，不改 Scope/Temporal 来帮助检索通过。
    let mut conflicting = harness.memory(original_id)?;
    conflicting.id = harness.engine.repository.allocate_id()?;
    conflicting.polarity = Some(Polarity::Positive);
    let conflict_id = conflicting.id;
    harness.engine.repository.insert(conflicting)?;
    eprintln!("[live][fixture] injected conflict {original_id:?} <-> {conflict_id:?}");
    let context = harness.build_query(NOW + 1, "早餐可以喝咖啡吗？")?;
    assert_eq!(context.domain, Some(Domain::from("food")));
    assert!(
        context
            .predicates
            .contains(&Predicate::from("exclude_drink"))
    );
    assert!(!context.targets.is_empty(), "context={context:#?}");
    let result = harness.retrieve(&context)?;
    assert!(result.constraints.is_empty(), "冲突规则不得静默选一条");
    assert_eq!(result.conflicts.len(), 1, "result={result:#?}");
    assert_decision(
        &result,
        original_id,
        RetrievalDecision::Conflict {
            with: conflict_id,
            reason: RetrievalConflictReason::EqualSpecificity,
        },
    );
    assert_decision(
        &result,
        conflict_id,
        RetrievalDecision::Conflict {
            with: original_id,
            reason: RetrievalConflictReason::EqualSpecificity,
        },
    );
    Ok(())
});

// PMV1-15：上海只存在于 history，当前亚朵 Preference 不得携带上海 Scope。
live_test!(live_smoke_pmv1_15_context_does_not_pollute_scope, {
    let mut harness = LiveHarness::new(&[
        ("travel_plan", Cardinality::Multiple),
        ("prefers_hotel", Cardinality::Multiple),
    ])?;
    let plan = harness.extract("S1", "T1", NOW, &[], "我准备去上海旅游。")?;
    let writes = harness.ingest(plan)?;
    assert!(writes.iter().all(|write| matches!(
        write.report.outcome,
        IngestOutcome::Rejected(_) | IngestOutcome::Ignored
    )));
    let preference = harness.extract(
        "S1",
        "T2",
        NOW + 1,
        &["我准备去上海旅游。"],
        "我挺喜欢亚朵的。",
    )?;
    let candidate = candidate_for(&preference, MemoryType::Preference, "prefers_hotel");
    assert!(
        !candidate
            .scope
            .conditions
            .iter()
            .any(|condition| matches!(condition, ScopeCondition::Location(_))),
        "candidate={candidate:#?}"
    );
    harness.ingest(preference)?;
    let context = harness.build_query(NOW + 2, "去北京住哪里？")?;
    let result = harness.retrieve(&context)?;
    assert!(
        lane_has(&result.preferences, "prefers_hotel"),
        "result={result:#?}"
    );
    Ok(())
});

// PMV1-16：带孩子的租车偏好不能被真实 query builder 泛化到独自东京。
live_test!(live_full_pmv1_16_specific_scope, {
    let mut harness = LiveHarness::new(&[("preferred_transport", Cardinality::Single)])?;
    let candidates = harness.extract("S1", "T1", NOW, &[], "带孩子的时候我一般都租车。")?;
    let candidate = candidate_for(&candidates, MemoryType::Preference, "preferred_transport");
    assert!(candidate.scope.conditions.iter().any(|condition| matches!(
        condition,
        ScopeCondition::Companion(EntityOrRole::Role(CompanionRole::Children))
    )));
    let id = written_id(
        &harness.ingest(candidates)?,
        "preferred_transport",
        ReconciliationAction::New,
    );
    let matching = harness.build_query(NOW + 1, "带孩子去东京怎么出行？")?;
    assert_ids(&harness.retrieve(&matching)?.preferences, &[id]);
    let context = harness.build_query(NOW + 1, "我自己去东京怎么出行？")?;
    let result = harness.retrieve(&context)?;
    assert!(!lane_has(&result.preferences, "preferred_transport"));
    assert_decision(
        &result,
        id,
        RetrievalDecision::Filtered(RetrievalFilterReason::Scope),
    );
    Ok(())
});

// PMV1-17：父子偏好都必须真实入库，且 trace 证明父类被子类覆盖。
live_test!(live_full_pmv1_17_preference_parent_exception, {
    let mut harness = LiveHarness::new(&[("likes", Cardinality::Multiple)])?;
    let parent = harness.extract("S1", "T1", NOW, &[], "我不喜欢咖啡。")?;
    assert_eq!(
        candidate_for(&parent, MemoryType::Preference, "likes").polarity,
        Some(Polarity::Negative)
    );
    let parent_id = written_id(&harness.ingest(parent)?, "likes", ReconciliationAction::New);
    let child = harness.extract("S1", "T2", NOW + 1, &["我不喜欢咖啡。"], "不过拿铁还可以。")?;
    assert_eq!(
        candidate_for(&child, MemoryType::Preference, "likes").polarity,
        Some(Polarity::Positive)
    );
    let child_id = written_id(&harness.ingest(child)?, "likes", ReconciliationAction::New);
    assert_eq!(harness.memory(parent_id)?.status, MemoryStatus::Active);
    assert_eq!(harness.memory(child_id)?.status, MemoryStatus::Active);
    let context = harness.build_query(NOW + 2, "给我推荐一个拿铁类咖啡饮料。")?;
    let mut relations = InMemorySemanticRelationRegistry::default();
    relations.register_is_a(MemoryValue::from("拿铁"), MemoryValue::from("咖啡"));
    relations.register_is_a(MemoryValue::from("Latte"), MemoryValue::from("Coffee"));
    let result =
        DefaultMemoryRetriever::new(&mut harness.engine.repository, &harness.engine.predicates)
            .with_semantic_relations(&relations)
            .retrieve(&context)?;
    eprintln!("[live][retrieval] result={result:#?}");
    assert_ids(&result.preferences, &[child_id]);
    assert_trace_complete(&result, harness.memories()?.len());
    assert_decision(
        &result,
        parent_id,
        RetrievalDecision::Filtered(RetrievalFilterReason::OverriddenBy(child_id)),
    );
    Ok(())
});

// PMV1-18：真实抽取/查询后，父类 Constraint 必须通过 IS-A 命中 Latte。
live_test!(live_full_pmv1_18_constraint_is_a, {
    let mut harness = LiveHarness::new(&[
        ("likes", Cardinality::Multiple),
        ("exclude_drink", Cardinality::Multiple),
    ])?;
    harness.ingest(harness.extract("S1", "T1", NOW, &[], "我喜欢拿铁。")?)?;
    harness.ingest(harness.extract("S1", "T2", NOW + 1, &[], "早餐不要咖啡类饮料。")?)?;
    let context = harness.build_query(NOW + 2, "早餐饮料推荐拿铁怎么样？")?;
    let mut relations = InMemorySemanticRelationRegistry::default();
    relations.register_is_a(MemoryValue::from("拿铁"), MemoryValue::from("咖啡"));
    relations.register_is_a(MemoryValue::from("Latte"), MemoryValue::from("Coffee"));
    let result =
        DefaultMemoryRetriever::new(&mut harness.engine.repository, &harness.engine.predicates)
            .with_semantic_relations(&relations)
            .retrieve(&context)?;
    assert!(
        lane_has(&result.constraints, "exclude_drink"),
        "result={result:#?}"
    );
    assert!(lane_has(&result.preferences, "likes"), "result={result:#?}");
    Ok(())
});

// PMV1-19：真实 query builder 必须把同行孩子列入 entities/conditions。
live_test!(live_full_pmv1_19_related_child_entity, {
    let mut harness = LiveHarness::new(&[
        ("age", Cardinality::Single),
        ("homestay", Cardinality::Multiple),
    ])?;
    let age = harness.extract("S1", "T1", NOW, &[], "我小孩子快三岁了。")?;
    assert_eq!(
        candidate_for(&age, MemoryType::Fact, "age").subject,
        CandidateSubject::Resolved(EntityId::from("child-younger"))
    );
    let age_id = written_id(&harness.ingest(age)?, "age", ReconciliationAction::New);
    let lodging = harness.extract("S1", "T2", NOW + 1, &[], "带孩子旅行不要推荐民宿。")?;
    candidate_for(&lodging, MemoryType::Constraint, "homestay");
    let lodging_id = written_id(
        &harness.ingest(lodging)?,
        "homestay",
        ReconciliationAction::New,
    );
    let context = harness.build_query(NOW + 2, "带小孩子去日本有什么需要特别注意的吗？")?;
    assert!(
        context.entities.contains(&EntityId::from("child-younger")),
        "context={context:#?}"
    );
    assert!(context.entities.contains(&user()), "context={context:#?}");
    assert!(
        context.predicates.contains(&Predicate::from("age")),
        "context={context:#?}"
    );
    assert!(
        context.predicates.contains(&Predicate::from("homestay")),
        "context={context:#?}"
    );
    let result = harness.retrieve(&context)?;
    assert_ids(&result.facts, &[age_id]);
    assert_ids(&result.constraints, &[lodging_id]);
    Ok(())
});

// PMV1-20：真实 query understanding 不得把购买蓝衣服改写成颜色偏好。
live_test!(live_smoke_pmv1_20_insufficient_memory, {
    let mut harness = LiveHarness::new(&[
        ("purchased_item", Cardinality::Multiple),
        ("likes_color", Cardinality::Multiple),
    ])?;
    let candidates = harness.extract("S1", "T1", NOW, &[], "曾经买过蓝色衣服。")?;
    assert!(
        candidates
            .iter()
            .all(|candidate| { candidate.predicate != Predicate::from("likes_color") }),
        "candidates={candidates:#?}"
    );
    harness.ingest(candidates)?;
    let context = harness.build_query(NOW + 1, "我最喜欢什么颜色？")?;
    assert_eq!(context.predicates, vec![Predicate::from("likes_color")]);
    let result = harness.retrieve(&context)?;
    assert!(all_lanes_empty(&result), "result={result:#?}");
    // 当前实现只记录首个过滤原因，Temporal 在 Predicate 之前。
    // 此处已独立断言 query 只选 likes_color，不能强求不相关经历必须先经过时间过滤。
    for memory in harness.memories()? {
        assert_eq!(memory.predicate, Predicate::from("purchased_item"));
        let decision = result
            .trace
            .iter()
            .find(|entry| entry.memory_id == memory.id)
            .unwrap()
            .decision;
        assert!(
            matches!(
                decision,
                RetrievalDecision::Filtered(
                    RetrievalFilterReason::Predicate | RetrievalFilterReason::Temporal
                )
            ),
            "decision={decision:?}"
        );
    }
    Ok(())
});

// PMV1-B01：控制语和计算任务不产生可入库 Candidate。
live_test!(live_full_pmv1_b01_control_messages, {
    let mut harness = LiveHarness::new(&[("control", Cardinality::Multiple)])?;
    for (turn, text) in [("T1", "好的，继续。"), ("T2", "帮我算一下 23×18。")] {
        let candidates = harness.extract("S1", turn, NOW, &[], text)?;
        let writes = harness.ingest(candidates)?;
        assert!(
            writes.iter().all(|write| matches!(
                write.report.outcome,
                IngestOutcome::Rejected(_) | IngestOutcome::Ignored
            )),
            "writes={:#?}",
            writes.iter().map(|write| &write.report).collect::<Vec<_>>()
        );
    }
    assert!(harness.memories()?.is_empty());
    Ok(())
});

// PMV1-B02：低价值午餐 Episode 被 Admission 拒绝。
live_test!(live_full_pmv1_b02_low_value_episode, {
    let mut harness = LiveHarness::new(&[("ate", Cardinality::Multiple)])?;
    let candidates = harness.extract("S1", "T1", NOW, &[], "我今天中午吃了番茄炒蛋。")?;
    let writes = harness.ingest(candidates)?;
    assert!(
        writes.iter().all(|write| matches!(
            write.report.outcome,
            IngestOutcome::Rejected("episode has no cross-turn value") | IngestOutcome::Ignored
        )),
        "writes={:#?}",
        writes.iter().map(|write| &write.report).collect::<Vec<_>>()
    );
    assert!(harness.memories()?.is_empty());
    Ok(())
});

// PMV1-B03：每天喝咖啡必须抽成 habit Fact，不是 likes Preference。
live_test!(live_full_pmv1_b03_habit_not_preference, {
    let mut harness = LiveHarness::new(&[
        ("has_habit", Cardinality::Multiple),
        ("likes", Cardinality::Multiple),
    ])?;
    let candidates = harness.extract("S1", "T1", NOW, &[], "我每天早上都喝咖啡。")?;
    candidate_for(&candidates, MemoryType::Fact, "has_habit");
    assert!(
        candidates
            .iter()
            .all(|candidate| { candidate.predicate != Predicate::from("likes") }),
        "candidates={candidates:#?}"
    );
    harness.ingest(candidates)?;
    let context = harness.build_query(NOW + 1, "我每天早上有什么习惯？")?;
    assert!(lane_has(&harness.retrieve(&context)?.facts, "has_habit"));
    Ok(())
});

// PMV1-B04：当天能召回，次日 Retention 真正标记 Expired；只有 Date/valid_to 不算过期。
live_test!(live_full_pmv1_b04_today_breakfast_expiry, {
    let mut harness = LiveHarness::new(&[("exclude_drink", Cardinality::Multiple)])?;
    let candidates = harness.extract("S1", "T1", NOW, &[], "今天早餐别给我咖啡。")?;
    let candidate = candidate_for(&candidates, MemoryType::Constraint, "exclude_drink");
    assert_eq!(candidate.retention_tier, RetentionTier::ShortTerm);
    // NOW 是 +08 时区中午；自然日边界不是机械地加 24 小时。
    assert_eq!(candidate.temporal.expires_at, Some(Timestamp(NOW + 43_200)));
    let id = written_id(
        &harness.ingest(candidates)?,
        "exclude_drink",
        ReconciliationAction::New,
    );
    let today = harness.build_query(NOW + 1, "今天早餐可以喝咖啡吗？")?;
    assert_ids(&harness.retrieve(&today)?.constraints, &[id]);
    harness
        .engine
        .run_retention_for_owner(&owner(), Timestamp(NOW + 43_200))?;
    assert_eq!(harness.memory(id)?.status, MemoryStatus::Expired);
    let tomorrow = harness.build_query(NOW + 86_400, "今天早餐可以喝咖啡吗？")?;
    let result = harness.retrieve(&tomorrow)?;
    assert!(all_lanes_empty(&result));
    assert_decision(
        &result,
        id,
        RetrievalDecision::Filtered(RetrievalFilterReason::Status),
    );
    Ok(())
});

// PMV1-B05：复合旅行原话必须拆成两个 atomic candidates。
live_test!(live_full_pmv1_b05_atomic_travel_split, {
    let mut harness = LiveHarness::new(&[
        ("visited", Cardinality::Multiple),
        ("likes_place", Cardinality::Multiple),
    ])?;
    let candidates = harness.extract("S1", "T1", NOW, &[], "去年去了云南，大理我觉得很好。")?;
    candidate_for(&candidates, MemoryType::Episode, "visited");
    candidate_for(&candidates, MemoryType::Preference, "likes_place");
    assert_eq!(candidates.len(), 2, "candidates={candidates:#?}");
    let writes = harness.ingest(candidates)?;
    let visit_id = written_id(&writes, "visited", ReconciliationAction::New);
    let like_id = written_id(&writes, "likes_place", ReconciliationAction::New);
    let visit_query = harness.build_query(NOW + 1, "去年去过哪些地方？")?;
    assert_ids(&harness.retrieve(&visit_query)?.episodes, &[visit_id]);
    let like_query = harness.build_query(NOW + 2, "我觉得大理怎么样？")?;
    assert_ids(&harness.retrieve(&like_query)?.preferences, &[like_id]);
    Ok(())
});

// PMV1-B06：文档允许行为 Fact/Episode，但不能偷换成 Preference；后者需用户明确表达。
live_test!(live_full_pmv1_b06_behavior_and_preference, {
    let mut harness = LiveHarness::new(&[
        ("stayed_at_on_business_trip", Cardinality::Multiple),
        ("prefers_hotel", Cardinality::Multiple),
    ])?;
    let behavior = harness.extract("S1", "T1", NOW, &[], "我连续三次出差都住亚朵。")?;
    let behavior_type = candidate_with_types(
        &behavior,
        &[MemoryType::Fact, MemoryType::Episode],
        "stayed_at_on_business_trip",
    )
    .memory_type;
    assert_eq!(behavior.len(), 1, "描述行为不能额外推出偏好");
    let behavior_id = written_id(
        &harness.ingest(behavior)?,
        "stayed_at_on_business_trip",
        ReconciliationAction::New,
    );
    let preference = harness.extract(
        "S1",
        "T2",
        NOW + 1,
        &["我连续三次出差都住亚朵。"],
        "我每次出差基本都会优先选亚朵。",
    )?;
    candidate_for(&preference, MemoryType::Preference, "prefers_hotel");
    let preference_id = written_id(
        &harness.ingest(preference)?,
        "prefers_hotel",
        ReconciliationAction::New,
    );
    assert_eq!(harness.memory(behavior_id)?.status, MemoryStatus::Active);
    let behavior_query = harness.build_query(NOW + 2, "之前出差住过什么酒店？")?;
    assert_ids(
        descriptive_lane(&harness.retrieve(&behavior_query)?, behavior_type),
        &[behavior_id],
    );
    let preference_query = harness.build_query(NOW + 3, "这次出差我会优先选择什么酒店？")?;
    assert_ids(
        &harness.retrieve(&preference_query)?.preferences,
        &[preference_id],
    );
    Ok(())
});

// PMV1-B07：否认旧偏好必须抽成 Retract，而不是 Negative Preference。
live_test!(live_full_pmv1_b07_denial_retracts, {
    let mut harness = LiveHarness::new(&[("likes", Cardinality::Multiple)])?;
    let old = harness.extract("S1", "T1", NOW, &[], "我喜欢民宿。")?;
    let old_id = written_id(&harness.ingest(old)?, "likes", ReconciliationAction::New);
    let candidates = harness.extract(
        "S1",
        "T2",
        NOW + 1,
        &["我喜欢民宿。"],
        "我没说过我喜欢民宿。",
    )?;
    let candidate = candidate_for(&candidates, MemoryType::Preference, "likes");
    assert!(matches!(candidate.intent, CandidateIntent::Retract { .. }));
    assert_eq!(candidates.len(), 1, "不能额外创建反向偏好");
    assert_eq!(
        action_for(&harness.ingest(candidates)?, "likes"),
        ReconciliationAction::Retract
    );
    assert_eq!(harness.memory(old_id)?.status, MemoryStatus::Superseded);
    assert_eq!(
        harness.memory(old_id)?.supersede_reason,
        Some(SupersedeReason::UserRetracted)
    );
    assert_eq!(harness.memories()?.len(), 1, "撤销没有新的替代记录");
    let query = harness.build_query(NOW + 2, "我现在喜欢民宿吗？")?;
    let result = harness.retrieve(&query)?;
    assert!(all_lanes_empty(&result));
    assert_decision(
        &result,
        old_id,
        RetrievalDecision::Filtered(RetrievalFilterReason::Status),
    );
    Ok(())
});

// PMV1-B08：一句话拆成 primary_residence 与 weekend_stay 两个 predicate。
live_test!(live_full_pmv1_b08_two_residence_predicates, {
    let mut harness = LiveHarness::new(&[
        ("primary_residence", Cardinality::Single),
        ("weekend_stay", Cardinality::Multiple),
    ])?;
    let candidates = harness.extract("S1", "T1", NOW, &[], "我现在住阜阳，不过周末经常住南京。")?;
    candidate_for(&candidates, MemoryType::Fact, "primary_residence");
    candidate_for(&candidates, MemoryType::Fact, "weekend_stay");
    assert_eq!(candidates.len(), 2, "candidates={candidates:#?}");
    let writes = harness.ingest(candidates)?;
    let primary_id = written_id(&writes, "primary_residence", ReconciliationAction::New);
    let weekend_id = written_id(&writes, "weekend_stay", ReconciliationAction::New);
    assert_eq!(harness.memory(primary_id)?.status, MemoryStatus::Active);
    assert_eq!(harness.memory(weekend_id)?.status, MemoryStatus::Active);
    let primary = harness.build_query(NOW + 1, "我现在主要住在哪里？")?;
    assert_ids(&harness.retrieve(&primary)?.facts, &[primary_id]);
    let weekend = harness.build_query(NOW + 2, "我周末经常住在哪里？")?;
    assert_ids(&harness.retrieve(&weekend)?.facts, &[weekend_id]);
    Ok(())
});

// PMV1-B09：车辆故障的 cross-turn usefulness 决定 Admission。
live_test!(live_full_pmv1_b09_vehicle_breakdown, {
    let mut harness = LiveHarness::new(&[("vehicle_broken", Cardinality::Single)])?;
    let candidates = harness.extract(
        "S1",
        "T1",
        NOW,
        &[],
        "我的车今天坏了，接下来还要继续处理维修。",
    )?;
    let candidate = candidate_for(&candidates, MemoryType::Episode, "vehicle_broken");
    assert!(candidate.cross_turn_useful, "candidate={candidate:#?}");
    assert_eq!(
        action_for(&harness.ingest(candidates)?, "vehicle_broken"),
        ReconciliationAction::New
    );
    Ok(())
});

// PMV1-B10：一般预算与上海 task 预算必须作为两个 Scope 并存。
live_test!(live_full_pmv1_b10_task_specific_budget, {
    let mut harness = LiveHarness::new(&[("hotel_budget", Cardinality::Single)])?;
    let general_id = written_id(
        &harness.ingest(harness.extract(
            "S1",
            "T1",
            NOW,
            &[],
            "一般旅行酒店预算不超过 500。",
        )?)?,
        "hotel_budget",
        ReconciliationAction::New,
    );
    // 调用方提供本次旅行身份；模型仍决定原话的 Task Scope，不手工补改输出。
    let task = TaskId::from("trip-2026-09-04-a");
    harness.task_id = Some(task.clone());
    let specific_id = written_id(
        &harness.ingest(harness.extract(
            "S1",
            "T2",
            NOW + 1,
            &["一般旅行酒店预算不超过 500。"],
            "这次上海酒店预算不超过 800。",
        )?)?,
        "hotel_budget",
        ReconciliationAction::New,
    );
    let memories = harness.memories()?;
    assert_eq!(
        memories
            .iter()
            .filter(|memory| memory.predicate == Predicate::from("hotel_budget"))
            .count(),
        2,
        "memories={memories:#?}"
    );
    assert_eq!(harness.memory(general_id)?.status, MemoryStatus::Active);
    assert_eq!(harness.memory(specific_id)?.status, MemoryStatus::Active);
    assert!(
        harness
            .memory(specific_id)?
            .scope
            .conditions
            .iter()
            .any(|condition| condition == &ScopeCondition::Task(task.clone()))
    );
    let shanghai = harness.build_query(NOW + 2, "这次上海旅行酒店预算是多少？")?;
    let result = harness.retrieve(&shanghai)?;
    assert_ids(&result.constraints, &[specific_id]);
    assert_decision(
        &result,
        general_id,
        RetrievalDecision::Filtered(RetrievalFilterReason::OverriddenBy(specific_id)),
    );
    harness.task_id = Some(TaskId::from("trip-2026-09-04-b"));
    let other_shanghai = harness.build_query(NOW + 3, "下一次去上海旅行酒店预算是多少？")?;
    assert_ids(
        &harness.retrieve(&other_shanghai)?.constraints,
        &[general_id],
    );
    let beijing = harness.build_query(NOW + 4, "下一次去北京旅行酒店预算是多少？")?;
    assert_ids(&harness.retrieve(&beijing)?.constraints, &[general_id]);
    Ok(())
});

// PMV1-B11：保留为 live known-limitation test；当前预期暴露 Episode target 过滤缺口。
live_test!(live_full_pmv1_b11_open_ended_relevance, {
    let mut harness = LiveHarness::new(&[("visited", Cardinality::Multiple)])?;
    let id = written_id(
        &harness.ingest(harness.extract("S1", "T1", NOW, &[], "我去过云南。")?)?,
        "visited",
        ReconciliationAction::New,
    );
    // 配对正例保证数据可召回；不能把所有经历都过滤掉后宣称相关性判断正确。
    let positive = harness.build_query(NOW + 1, "我之前是不是去过云南？")?;
    assert_ids(&harness.retrieve(&positive)?.episodes, &[id]);
    let context = harness.build_query(NOW + 1, "我之前是不是去过泸沽湖？")?;
    let result = harness.retrieve(&context)?;
    assert!(
        all_lanes_empty(&result),
        "known V1 limitation: result={result:#?}"
    );
    Ok(())
});

// PMV1-B12：真实抽取后由 Retention 过期；current 与 historical 结果相反。
live_test!(live_full_pmv1_b12_expired_historical_memory, {
    let mut harness = LiveHarness::new(&[("evening_activity", Cardinality::Multiple)])?;
    let candidates = harness.extract(
        "S1",
        "T1",
        NOW,
        &[],
        "请临时记住：我以前经常晚上打游戏，这条信息明天后就不用保留。",
    )?;
    let candidate = candidate_with_types(
        &candidates,
        &[MemoryType::Fact, MemoryType::Episode],
        "evening_activity",
    );
    let memory_type = candidate.memory_type;
    assert_eq!(candidate.retention_tier, RetentionTier::ShortTerm);
    let id = written_id(
        &harness.ingest(candidates)?,
        "evening_activity",
        ReconciliationAction::New,
    );
    harness
        .engine
        .run_retention_for_owner(&owner(), Timestamp(NOW + 172_800))?;
    assert_eq!(harness.memory(id)?.status, MemoryStatus::Expired);
    let current = harness.build_query(NOW + 172_800, "晚上适合做什么？")?;
    let result = harness.retrieve(&current)?;
    assert!(all_lanes_empty(&result));
    assert_decision(
        &result,
        id,
        RetrievalDecision::Filtered(RetrievalFilterReason::Status),
    );
    let historical = harness.build_query(NOW + 172_800, "以前晚上经常做什么？")?;
    assert_eq!(historical.temporal_intent, TemporalIntent::Historical);
    assert_ids(
        descriptive_lane(&harness.retrieve(&historical)?, memory_type),
        &[id],
    );
    Ok(())
});

// 以下只测试“测试断言是否可靠”，使用人工夹具，不属于真实 API 场景。
fn calibration_candidate(memory_type: MemoryType) -> CandidateMemory {
    CandidateMemory::new(
        owner(),
        user(),
        memory_type,
        Predicate::from("owns_device"),
        MemoryValue::Entity(EntityId::from("thinkpad")),
        Timestamp(NOW),
        Evidence::new(
            EvidenceId::from("oracle:T1"),
            SessionId::from("oracle"),
            TurnId::from("T1"),
            "我有一台 ThinkPad。",
        ),
    )
}

#[test]
fn calibration_types_accept_only_documented_equivalence() {
    for memory_type in [MemoryType::Fact, MemoryType::Episode] {
        let candidates = [calibration_candidate(memory_type)];
        candidate_with_types(
            &candidates,
            &[MemoryType::Fact, MemoryType::Episode],
            "owns_device",
        );
    }
    let wrong = [calibration_candidate(MemoryType::Preference)];
    assert!(
        std::panic::catch_unwind(|| candidate_with_types(
            &wrong,
            &[MemoryType::Fact, MemoryType::Episode],
            "owns_device"
        ))
        .is_err()
    );
    let duplicate = [
        calibration_candidate(MemoryType::Fact),
        calibration_candidate(MemoryType::Episode),
    ];
    assert!(
        std::panic::catch_unwind(|| candidate_with_types(
            &duplicate,
            &[MemoryType::Fact, MemoryType::Episode],
            "owns_device"
        ))
        .is_err()
    );
    let episode = [calibration_candidate(MemoryType::Episode)];
    assert!(
        std::panic::catch_unwind(|| candidate_for(&episode, MemoryType::Fact, "owns_device"))
            .is_err()
    );
}

#[test]
fn calibration_ownership_checks_identity_and_polarity() -> TestResult {
    let mut engine = TestEngine::new(
        InMemoryMemoryRepository::default(),
        InMemoryPredicateRegistry::default(),
    );
    engine.ingest(common::grounded(calibration_candidate(MemoryType::Fact)))?;
    let kept = engine.repository.list_by_owner(&owner())?.remove(0);
    let mut sold = kept.clone();
    sold.id = MemoryId(2);
    sold.value = MemoryValue::Entity(EntityId::from("macbook-2018"));
    sold.polarity = Some(Polarity::Negative);
    let expected = [EntityId::from("thinkpad")];
    assert_owned_devices(&[kept.clone(), sold.clone()], &expected);
    let mut explicit_positive = kept.clone();
    explicit_positive.polarity = Some(Polarity::Positive);
    assert_owned_devices(&[explicit_positive, sold.clone()], &expected);
    sold.polarity = Some(Polarity::Positive);
    assert!(
        std::panic::catch_unwind(|| assert_owned_devices(&[kept.clone(), sold], &expected))
            .is_err()
    );
    assert!(std::panic::catch_unwind(|| assert_owned_devices(&[], &expected)).is_err());
    // 旧测试只数一条，错误设备也会通过；现在必须保留正确 Entity 身份。
    let mut wrong = kept;
    wrong.value = MemoryValue::Entity(EntityId::from("macbook-2018"));
    assert!(std::panic::catch_unwind(|| assert_owned_devices(&[wrong], &expected)).is_err());
    Ok(())
}

#[test]
fn calibration_retrieval_rejects_wrong_version_and_filter_reason() -> TestResult {
    let mut engine = TestEngine::new(
        InMemoryMemoryRepository::default(),
        InMemoryPredicateRegistry::default(),
    );
    engine.ingest(common::grounded(calibration_candidate(MemoryType::Fact)))?;
    let memories = engine.repository.list_by_owner(&owner())?;
    assert_ids(&memories, &[memories[0].id]);
    assert!(std::panic::catch_unwind(|| assert_ids(&memories, &[MemoryId(999)])).is_err());
    assert!(std::panic::catch_unwind(|| assert_ids(&[], &[memories[0].id])).is_err());
    let result = RetrievalResult {
        trace: vec![RetrievalTraceEntry {
            memory_id: memories[0].id,
            decision: RetrievalDecision::Filtered(RetrievalFilterReason::Temporal),
        }],
        ..Default::default()
    };
    assert!(
        std::panic::catch_unwind(|| assert_decision(
            &result,
            memories[0].id,
            RetrievalDecision::Filtered(RetrievalFilterReason::Scope)
        ))
        .is_err()
    );
    Ok(())
}
