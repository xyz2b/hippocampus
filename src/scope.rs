//! 结构化 Scope 的规范化、关系判断与 RuntimeContext 匹配。

use crate::{
    CompanionRole, Domain, EntityId, EntityOrRole, LocationScope, RuntimeContext, Scope,
    ScopeCondition, ScopeRelation, TimeScope,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Axis {
    Companion,
    Location,
    Date,
    TimeOfDay,
    Activity,
    Task,
}

impl ScopeCondition {
    fn entails(&self, other: &Self) -> bool {
        self == other
            || matches!(
                (self, other),
                (
                    Self::Companion(EntityOrRole::Role(CompanionRole::Children)),
                    Self::Companion(EntityOrRole::Role(CompanionRole::Family))
                )
            )
    }

    fn axis(&self) -> Axis {
        match self {
            Self::Companion(_) => Axis::Companion,
            Self::Location(_) => Axis::Location,
            Self::Time(TimeScope::Date(_)) => Axis::Date,
            Self::Time(TimeScope::TimeOfDay(_)) => Axis::TimeOfDay,
            Self::Activity(_) => Axis::Activity,
            Self::Task(_) => Axis::Task,
        }
    }

    fn definitely_conflicts_with(&self, other: &Self) -> bool {
        if self.axis() != other.axis() || self.entails(other) || other.entails(self) {
            return false;
        }

        match (self, other) {
            (
                Self::Location(LocationScope::Domestic),
                Self::Location(LocationScope::International),
            )
            | (
                Self::Location(LocationScope::International),
                Self::Location(LocationScope::Domestic),
            ) => true,
            (
                Self::Location(LocationScope::Country(a)),
                Self::Location(LocationScope::Country(b)),
            ) => a != b,
            (Self::Location(LocationScope::City(a)), Self::Location(LocationScope::City(b))) => {
                a != b
            }
            (Self::Location(_), Self::Location(_)) => false,
            _ => true,
        }
    }

    fn relation_is_unknown(&self, other: &Self) -> bool {
        self.axis() == other.axis()
            && self != other
            && matches!((self, other), (Self::Location(_), Self::Location(_)))
            && !self.definitely_conflicts_with(other)
    }
}

impl Scope {
    pub fn new(domain: Option<Domain>, mut conditions: Vec<ScopeCondition>) -> Self {
        if conditions.contains(&ScopeCondition::Companion(EntityOrRole::Role(
            CompanionRole::Children,
        ))) {
            // Children 已隐含 Family，重复描述不能增加条件数或改变优先级。
            conditions.retain(|c| {
                c != &ScopeCondition::Companion(EntityOrRole::Role(CompanionRole::Family))
            });
        }
        // 规范化后 scope equality 与 set-inclusion 判断可以保持确定性。
        conditions.sort();
        conditions.dedup();
        Self { domain, conditions }
    }

    pub fn global() -> Self {
        Self::default()
    }

    pub fn specificity(&self) -> usize {
        usize::from(self.domain.is_some()) + self.conditions.len()
    }

    pub fn relation(&self, other: &Self) -> ScopeRelation {
        if matches!((&self.domain, &other.domain), (Some(a), Some(b)) if a != b) {
            return ScopeRelation::Disjoint;
        }

        let mut unknown = false;
        for left in &self.conditions {
            for right in &other.conditions {
                if left.definitely_conflicts_with(right) {
                    return ScopeRelation::Disjoint;
                }
                // City/Country/Domestic 的层级没有 ontology 时保持 Unknown，不猜包含关系。
                unknown |= left.relation_is_unknown(right);
            }
        }

        let self_contains_other = domain_contains(&self.domain, &other.domain)
            && other
                .conditions
                .iter()
                .all(|c| self.conditions.iter().any(|s| s.entails(c)));
        let other_contains_self = domain_contains(&other.domain, &self.domain)
            && self
                .conditions
                .iter()
                .all(|c| other.conditions.iter().any(|s| s.entails(c)));

        match (self_contains_other, other_contains_self, unknown) {
            (true, true, _) => ScopeRelation::Equal,
            (true, false, false) => ScopeRelation::MoreSpecific,
            (false, true, false) => ScopeRelation::MoreGeneral,
            (_, _, true) => ScopeRelation::Unknown,
            _ => ScopeRelation::Overlap,
        }
    }

    pub fn applies_to(&self, context: &RuntimeContext) -> bool {
        if let Some(domain) = &self.domain
            && context.domain.as_ref() != Some(domain)
        {
            return false;
        }

        // Scope 是 AND 条件集合；conversation 中出现但未进入 context 的条件不自动匹配。
        self.conditions.iter().all(|condition| match condition {
            ScopeCondition::Task(task) => {
                context.task_id.as_ref() == Some(task) || context.conditions.contains(condition)
            }
            _ => context
                .conditions
                .iter()
                .any(|actual| actual.entails(condition)),
        })
    }

    pub fn references_any_entity(&self, entities: &[EntityId]) -> bool {
        self.conditions.iter().any(|condition| {
            matches!(condition, ScopeCondition::Companion(EntityOrRole::Entity(id)) if entities.contains(id))
        })
    }
}

fn domain_contains(more_specific: &Option<Domain>, more_general: &Option<Domain>) -> bool {
    more_general.is_none() || more_specific == more_general
}
