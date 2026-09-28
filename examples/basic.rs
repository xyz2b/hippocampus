// Offline domain demonstration: the reviewer below is an explicit test double.
#[path = "../tests/common/mod.rs"]
mod common;

use hippocampus::*;

fn main() -> Result<(), StoreError> {
    let owner = UserId::from("user-1");
    let user = EntityId::from("entity-user-1");
    let predicate = Predicate::from("primary_residence");

    let mut predicates = InMemoryPredicateRegistry::default();
    predicates.register(predicate.clone(), Cardinality::Single);
    let mut engine = MemoryEngine::new(InMemoryMemoryRepository::default(), predicates);

    let candidate = CandidateMemory::new(
        owner.clone(),
        user.clone(),
        MemoryType::Fact,
        predicate,
        MemoryValue::from("Fuyang"),
        Timestamp(1),
        Evidence::new(
            EvidenceId::from("example-session:turn-1:0"),
            SessionId::from("example-session"),
            TurnId::from("turn-1"),
            "我现在住阜阳。",
        ),
    );
    let write = engine.ingest_with_trace(common::grounded(candidate))?;
    println!(
        "outcome: {:?}\nwrite trace: {:#?}",
        write.outcome, write.trace
    );

    let mut context = RuntimeContext::current(owner, Timestamp(2));
    context.entities.push(user);
    let result = DefaultMemoryRetriever::new(&mut engine.repository, &engine.predicates)
        .retrieve(&context)?;
    println!(
        "facts: {:?}\nretrieval trace: {:#?}",
        result.facts, result.trace
    );
    Ok(())
}
