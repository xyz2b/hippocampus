//! Diagnostic: reproduce an actual default-catalog query against an isolated test snapshot.
use hippocampus::{chat::ChatConfig, *};
use std::time::{SystemTime, UNIX_EPOCH};

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut args = std::env::args().skip(1);
    let db = args.next().ok_or("provide isolated database")?;
    let query = args.next().ok_or("provide query")?;
    let remember = args.next().is_some_and(|mode| mode == "remember");
    let task = args.next();
    let expected = args.next().unwrap_or_else(|| "祁门红茶".into());
    let withdrawn = args.next().unwrap_or_else(|| "茉莉花茶".into());
    let owner = UserId::from("auto-test-user");
    let state = SqliteMemoryStore::open(db, owner.clone())?.read()?;
    let config = ChatConfig::default();
    let mut context = OpenAiAdapterContext::new(
        owner.clone(),
        state.entities.list_by_owner(&owner),
        Timestamp(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64),
    )?
    .with_utc_offset_seconds(config.utc_offset_seconds)?
    .with_semantic_catalog(config.predicates.clone(), config.domains)?
    .with_predicate_value_kinds(config.value_kinds)?
    .with_predicate_memory_types(config.memory_types)?;
    if let Some(task) = task {
        context = context.with_task_id(TaskId(task))?;
    }
    let mut registry = InMemoryPredicateRegistry::default();
    for predicate in config.predicates {
        registry.register(predicate.predicate, predicate.cardinality);
    }
    if remember {
        let candidates = OpenAiMemoryExtractor::new(
            OpenAiClient::from_env()?,
            context,
            SessionId::from("update-probe"),
            TurnId::from("change"),
        )
        .extract_turn(&[], &query)?;
        let mut engine = MemoryEngine::new(state.repository, registry);
        for candidate in candidates {
            println!("candidate={candidate:#?}");
            println!("report={:#?}", engine.ingest_with_trace(candidate)?);
        }
        let memories = engine.repository.list_by_owner(&owner)?;
        assert!(
            memories.iter().any(|m| m.status == MemoryStatus::Active
                && m.value == MemoryValue::Text(expected.clone())),
            "new expected value missing"
        );
        assert!(
            !memories.iter().any(|m| m.status == MemoryStatus::Active
                && m.value == MemoryValue::Text(withdrawn.clone())),
            "withdrawn value remained active"
        );
        return Ok(());
    }
    let runtime =
        OpenAiRuntimeContextBuilder::new(OpenAiClient::from_env()?, context).build(&query)?;
    println!("runtime={runtime:#?}");
    let mut repository = state.repository;
    let result = DefaultMemoryRetriever::new(&mut repository, &registry).retrieve(&runtime)?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    assert!(
        !result.preferences.is_empty() || !result.constraints.is_empty(),
        "the stored preference or scoped requirement was not recalled"
    );
    Ok(())
}
