//! Opt-in live smoke test for both OpenAI adapters.
//!
//! Run after setting `DEEPSEEK_BASE_URL` and `DEEPSEEK_API_KEY` in the environment.

use std::time::{SystemTime, UNIX_EPOCH};

use hippocampus::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let owner = UserId::from("user-1");
    let response_language = Predicate::from("response_language");
    let predicate_definitions = vec![PredicateDefinition {
        predicate: response_language.clone(),
        cardinality: Cardinality::Single,
    }];
    let now = Timestamp(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64);
    let context = OpenAiAdapterContext::new(
        owner.clone(),
        vec![Entity {
            id: EntityId::from("entity-user"),
            owner_id: owner,
            entity_type: EntityType::Person,
            canonical_name: "小焦".to_owned(),
            aliases: vec!["我".to_owned(), "自己".to_owned()],
        }],
        now,
    )?
    .with_utc_offset_seconds(8 * 60 * 60)?
    .with_semantic_catalog(
        predicate_definitions.clone(),
        vec![Domain::from("food_recommendation")],
    )?
    .with_predicate_value_kinds(vec![(
        response_language.clone(),
        vec![MemoryValueKind::Text],
    )])?;
    let client = OpenAiClient::from_env()?;

    // 真实写入入口：自然语言对话 → CandidateMemory → admission/reconciliation → Store。
    let candidates = OpenAiMemoryExtractor::new(
        client.clone(),
        context.clone(),
        SessionId::from("openai-smoke-session"),
        TurnId::from("turn-1"),
    )
    .extract("请记住：无论讨论什么主题，以后所有回答都必须使用中文。")?;
    assert!(
        candidates.iter().any(|candidate| {
            candidate.memory_type == MemoryType::Constraint
                && candidate.predicate == response_language
                && candidate.retention_tier == RetentionTier::Durable
                && candidate.scope == Scope::global()
        }),
        "the real case should extract the global durable language constraint"
    );
    assert!(
        candidates
            .iter()
            .all(|candidate| candidate.owner_id == UserId::from("user-1")
                && candidate.temporal.observed_at == now),
        "runtime-owned fields must not come from the model"
    );
    println!("candidates: {candidates:#?}");

    let mut predicates = InMemoryPredicateRegistry::default();
    for definition in predicate_definitions {
        predicates.register(definition.predicate, definition.cardinality);
    }
    let mut engine = MemoryEngine::new(InMemoryMemoryRepository::default(), predicates);
    for candidate in candidates {
        let report = engine.ingest_with_trace(candidate)?;
        assert!(matches!(
            report.outcome,
            IngestOutcome::Applied {
                action: ReconciliationAction::New,
                ..
            }
        ));
    }

    // 真实查询入口：自然语言问题 → RuntimeContext → Retrieval。
    let runtime = OpenAiRuntimeContextBuilder::new(client, context)
        .build("根据你记住的要求，你以后必须用什么语言回答我？")?;
    assert_eq!(runtime.owner_id, UserId::from("user-1"));
    assert_eq!(runtime.at, now);
    assert_eq!(runtime.predicates, vec![response_language.clone()]);
    println!("runtime context: {runtime:#?}");

    let result = DefaultMemoryRetriever::new(&mut engine.repository, &engine.predicates)
        .retrieve(&runtime)?;
    assert!(
        result
            .mandatory
            .iter()
            .any(|memory| memory.predicate == response_language),
        "the live query should retrieve the stored language constraint"
    );
    println!("retrieval: {result:#?}");
    Ok(())
}
