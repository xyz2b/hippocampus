//! Exact legacy scope storage helpers; no condition entailment or query interpretation.
use crate::{Domain, EntityId, EntityOrRole, Scope, ScopeCondition};
impl Scope {
    pub fn new(domain: Option<Domain>, conditions: Vec<ScopeCondition>) -> Self {
        Self { domain, conditions }
    }
    pub fn global() -> Self {
        Self::default()
    }
    pub fn references_any_entity(&self, entities: &[EntityId]) -> bool {
        self.conditions.iter().any(|condition| {
            matches!(condition,
            ScopeCondition::Companion(EntityOrRole::Entity(id)) if entities.contains(id))
        })
    }
}
