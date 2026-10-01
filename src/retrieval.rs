//! Query-side pipeline：mandatory lane → filters → scope precedence → conflict detection。

use std::cmp::Reverse;
use std::collections::HashMap;

use crate::{
    Cardinality, CompanionRole, EntityOrRole, Memory, MemoryId, MemoryRepository, MemoryStatus,
    MemoryType, MemoryValue, PredicateRegistry, RetentionTier, RuntimeContext, ScopeCondition,
    ScopeRelation, SemanticRelationRegistry, StoreError, SupersedeReason, TemporalIntent,
    TemporalRange, TemporalReference, TemporalRelation, TemporalResolution, TemporalSource,
    Timestamp, memories_share_slot,
};

#[derive(serde::Serialize, Clone, Debug, Eq, PartialEq)]
/// Query-time 无法安全裁决的一对 memory。
pub struct RetrievalConflict {
    /// 第一条冲突 memory。
    pub left: MemoryId,
    /// 第二条冲突 memory。
    pub right: MemoryId,
}

#[derive(serde::Serialize, Clone, Copy, Debug, Eq, PartialEq)]
/// Memory 未进入结果的首个确定性过滤原因。
pub enum RetrievalFilterReason {
    /// 生命周期状态不符合 current / historical 语义。
    Status,
    /// 时间点、区间或事件引用不匹配。
    Temporal,
    /// Subject 与 query entities 不匹配。
    Entity,
    /// Predicate 不在 query 白名单中。
    Predicate,
    /// 目标相关类型的 value 不匹配；Episode 不从父区域推断访问子地点。
    Target,
    /// 结构化 Scope 不适用于当前 context。
    Scope,
    /// 被同 semantic slot 中更具体或更早选中的 Memory 覆盖。
    OverriddenBy(MemoryId),
    /// 已达到普通候选数量上限。
    Limit,
    /// Local relevance, ambiguity, or context budget; detailed reason is in local recall diagnostics.
    LocalSelection,
    /// 完整候选带有现有 Memory 表达不了的限定语，或限定语已无从得知（完整候选随删除清洗）：
    /// 不能当作无限定事实参与旧路径的选取与 scope 判断，只作为原文上下文（contract-types.md §3）。
    QualifiersUnmapped,
}

#[derive(serde::Serialize, Clone, Copy, Debug, Eq, PartialEq)]
/// Query-time conflict 的具体形成原因。
pub enum RetrievalConflictReason {
    /// 同优先级、同 slot 的断言不同，无法安全选边。
    EqualSpecificity,
    /// 该 slot 已经被前一对冲突封锁。
    SlotAlreadyConflicted,
}

#[derive(serde::Serialize, Clone, Copy, Debug, Eq, PartialEq)]
/// 单条 Memory 在 retrieval pipeline 中的最终决策。
pub enum RetrievalDecision {
    /// 全局 Durable Constraint 进入 mandatory lane。
    Mandatory,
    /// 通过所有过滤与优先级处理，进入类型对应的结果 lane。
    Selected,
    /// 在指定阶段被过滤。
    Filtered(RetrievalFilterReason),
    /// 与另一条 Memory 冲突，双方均不返回。
    Conflict {
        /// 发生冲突的另一条 Memory。
        with: MemoryId,
        /// 冲突形成原因。
        reason: RetrievalConflictReason,
    },
}

#[derive(serde::Serialize, Clone, Copy, Debug, Eq, PartialEq)]
/// 一条 Memory 的完整 retrieval 决策结果。
pub struct RetrievalTraceEntry {
    /// 被评估的 Memory。
    pub memory_id: MemoryId,
    /// 最终进入、过滤或冲突决定。
    pub decision: RetrievalDecision,
}

#[derive(serde::Serialize, Clone, Debug, Default, Eq, PartialEq)]
/// Constraint / Preference 分 lane 返回，调用方可先过滤再排序。
pub struct RetrievalResult {
    /// 每次 current query 都必须加载的全局 Durable Constraint。
    pub mandatory: Vec<Memory>,
    /// 先于 Preference 执行过滤的适用约束。
    pub constraints: Vec<Memory>,
    /// 对剩余候选进行排序的适用偏好。
    pub preferences: Vec<Memory>,
    /// 与当前 Entity、时间和 Scope 匹配的事实。
    pub facts: Vec<Memory>,
    /// 与当前 Entity、时间和 Scope 匹配的经历。
    pub episodes: Vec<Memory>,
    /// 同 semantic slot、同优先级且无法选边的冲突。
    pub conflicts: Vec<RetrievalConflict>,
    /// Owner 范围内每条 Memory 的最终检索决策，按 ID 排序。
    pub trace: Vec<RetrievalTraceEntry>,
}

pub trait MemoryRetriever {
    fn retrieve(&mut self, context: &RuntimeContext) -> Result<RetrievalResult, StoreError>;
}

/// 使用 repository 与 predicate schema 执行默认查询流程。
pub struct DefaultMemoryRetriever<'a, R, P> {
    /// 查询并更新 retrieval counters 的 Memory Store。
    pub repository: &'a mut R,
    /// 解析 semantic slot 所需的 predicate schema。
    pub predicates: &'a P,
    /// 可选的最小 IS-A registry；未提供时只做 target 精确匹配。
    pub semantic_relations: Option<&'a dyn SemanticRelationRegistry>,
    /// 限定语无法用现有 Memory 表达的记录及其处理方式（见 `UnmappedQualifiers`）。
    pub unmapped_qualifiers: Option<(&'a std::collections::BTreeSet<MemoryId>, UnmappedQualifiers)>,
}

/// 限定语无法用现有 Memory 表达（或已无从得知）的记录在旧路径里怎么处理。投影出的 Memory 没有
/// 这些限定语，不能当作无限定事实参与 scope 裁决（contract-types.md §3）。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnmappedQualifiers {
    /// 结果不带限定语标注的入口（全局要求、旧召回）：直接排除。
    Exclude,
    /// 结果会附限定语原文的入口（本地召回）：仍按状态、时间、主体、谓词等过滤，但不进入
    /// 全局要求，不参与同槽竞争与覆盖，只作为上下文附在所属分区末尾。
    ContextOnly,
}

impl<'a, R, P> DefaultMemoryRetriever<'a, R, P> {
    pub fn new(repository: &'a mut R, predicates: &'a P) -> Self {
        Self {
            repository,
            predicates,
            semantic_relations: None,
            unmapped_qualifiers: None,
        }
    }

    /// 指定限定语无法用现有 Memory 表达的记录及处理方式。
    pub fn with_unmapped_qualifiers(
        mut self,
        ids: &'a std::collections::BTreeSet<MemoryId>,
        mode: UnmappedQualifiers,
    ) -> Self {
        self.unmapped_qualifiers = Some((ids, mode));
        self
    }

    /// 为本次查询启用父子类别匹配。
    pub fn with_semantic_relations(mut self, relations: &'a dyn SemanticRelationRegistry) -> Self {
        self.semantic_relations = Some(relations);
        self
    }
}

impl<R, P> MemoryRetriever for DefaultMemoryRetriever<'_, R, P>
where
    R: MemoryRepository,
    P: PredicateRegistry,
{
    fn retrieve(&mut self, context: &RuntimeContext) -> Result<RetrievalResult, StoreError> {
        let all = self.repository.list_by_owner(&context.owner_id)?;
        // 事件时间只能从当前 owner 的真实版本链取得；不让模型编造搬家时间戳。
        let mut resolved_context = context.clone();
        if let Some(range) = state_change_boundary(&all, context) {
            resolved_context.temporal_filter = Some(TemporalResolution::Range(range));
        }
        let context = &resolved_context;
        let mut trace = Vec::with_capacity(all.len());
        let mut mandatory = Vec::new();
        let mut candidates = Vec::new();
        let mut context_only = Vec::new();
        let unmapped = |id: MemoryId, wanted: UnmappedQualifiers| {
            self.unmapped_qualifiers
                .is_some_and(|(ids, mode)| mode == wanted && ids.contains(&id))
        };

        // 顺序判断并记录首个过滤原因；每条 owner memory 最终恰有一条 trace。
        for memory in all {
            if unmapped(memory.id, UnmappedQualifiers::Exclude) {
                trace.push(RetrievalTraceEntry {
                    memory_id: memory.id,
                    decision: RetrievalDecision::Filtered(
                        RetrievalFilterReason::QualifiersUnmapped,
                    ),
                });
                continue;
            }
            let only_context = unmapped(memory.id, UnmappedQualifiers::ContextOnly);
            if !only_context && is_mandatory(&memory, context) {
                trace.push(RetrievalTraceEntry {
                    memory_id: memory.id,
                    decision: RetrievalDecision::Mandatory,
                });
                mandatory.push(memory);
                continue;
            }
            let filtered = if !status_matches(&memory, context) {
                Some(RetrievalFilterReason::Status)
            } else if !time_matches(&memory, context) {
                Some(RetrievalFilterReason::Temporal)
            } else if !context.entities.is_empty()
                && !context.entities.contains(&memory.subject_id)
                && !memory.scope.references_any_entity(&context.entities)
            {
                Some(RetrievalFilterReason::Entity)
            } else if !context.predicates.is_empty()
                && !context.predicates.contains(&memory.predicate)
            {
                Some(RetrievalFilterReason::Predicate)
            } else if !target_matches(&memory, context, self.semantic_relations) {
                Some(RetrievalFilterReason::Target)
            } else if !scope_matches(&memory, context) {
                Some(RetrievalFilterReason::Scope)
            } else {
                None
            };
            if let Some(reason) = filtered {
                trace.push(RetrievalTraceEntry {
                    memory_id: memory.id,
                    decision: RetrievalDecision::Filtered(reason),
                });
            } else if only_context {
                trace.push(RetrievalTraceEntry {
                    memory_id: memory.id,
                    decision: RetrievalDecision::Selected,
                });
                context_only.push(memory);
            } else {
                candidates.push(memory);
            }
        }

        // ponytail: 候选集内成对检查显式关系；规模增大时按 subject/predicate 分组索引。
        let mut preference_overrides = HashMap::new();
        if let Some(relations) = self
            .semantic_relations
            .filter(|_| !context.targets.is_empty())
        {
            for parent in candidates.iter().filter(|m| {
                m.memory_type == MemoryType::Preference && context.predicates.contains(&m.predicate)
            }) {
                if let Some(child) = candidates.iter().find(|child| {
                    child.value != parent.value
                        && memories_compete(parent, child, Cardinality::Single, context)
                        && matches!(
                            child.scope.relation(&parent.scope),
                            ScopeRelation::Equal | ScopeRelation::MoreSpecific
                        )
                        && relations.is_a(&child.value, &parent.value)
                        && !relations.is_a(&parent.value, &child.value)
                        && context
                            .targets
                            .iter()
                            .filter(|target| relations.is_a(target, &parent.value))
                            .all(|target| relations.is_a(target, &child.value))
                }) {
                    preference_overrides.insert(parent.id, child.id);
                }
            }
        }
        candidates.retain(|memory| {
            if let Some(&child) = preference_overrides.get(&memory.id) {
                trace.push(RetrievalTraceEntry {
                    memory_id: memory.id,
                    decision: RetrievalDecision::Filtered(RetrievalFilterReason::OverriddenBy(
                        child,
                    )),
                });
                false
            } else {
                true
            }
        });

        // 更具体的 scope 先进入 resolution；同层级先处理 Constraint。
        candidates.sort_by_key(|memory| {
            (
                Reverse(memory.scope.specificity()),
                // 相同条件数下先看 Children，但只在真实包含 Family 时覆盖；跨轴仍报告冲突。
                Reverse(memory.scope.conditions.contains(&ScopeCondition::Companion(
                    EntityOrRole::Role(CompanionRole::Children),
                ))),
                type_priority(memory.memory_type),
                Reverse(memory.temporal.observed_at),
                memory.id,
            )
        });

        let mut selected: Vec<Memory> = Vec::new();
        let mut blocked: Vec<Memory> = Vec::new();
        let mut conflicts = Vec::new();
        let mut limit_reached = context.limit == 0;
        for candidate in candidates {
            if limit_reached {
                trace.push(RetrievalTraceEntry {
                    memory_id: candidate.id,
                    decision: RetrievalDecision::Filtered(RetrievalFilterReason::Limit),
                });
                continue;
            }
            let cardinality = self
                .predicates
                .definition(&candidate.predicate)
                .map_or(Cardinality::Multiple, |definition| definition.cardinality);
            if let Some(existing) = blocked
                .iter()
                .find(|memory| memories_compete(memory, &candidate, cardinality, context))
            {
                conflicts.push(RetrievalConflict {
                    left: existing.id,
                    right: candidate.id,
                });
                trace.push(RetrievalTraceEntry {
                    memory_id: candidate.id,
                    decision: RetrievalDecision::Conflict {
                        with: existing.id,
                        reason: RetrievalConflictReason::SlotAlreadyConflicted,
                    },
                });
                continue;
            }
            if let Some(position) = selected
                .iter()
                .position(|memory| memories_compete(memory, &candidate, cardinality, context))
            {
                let existing = &selected[position];
                if existing.scope.specificity() == candidate.scope.specificity()
                    && existing.scope.relation(&candidate.scope) != ScopeRelation::MoreSpecific
                    && !same_assertion(existing, &candidate)
                {
                    // 同优先级冲突时移除已选项，保证不会随机向 LLM 暴露其中一边。
                    conflicts.push(RetrievalConflict {
                        left: existing.id,
                        right: candidate.id,
                    });
                    set_trace_decision(
                        &mut trace,
                        existing.id,
                        RetrievalDecision::Conflict {
                            with: candidate.id,
                            reason: RetrievalConflictReason::EqualSpecificity,
                        },
                    );
                    trace.push(RetrievalTraceEntry {
                        memory_id: candidate.id,
                        decision: RetrievalDecision::Conflict {
                            with: existing.id,
                            reason: RetrievalConflictReason::EqualSpecificity,
                        },
                    });
                    blocked.push(selected.remove(position));
                } else {
                    trace.push(RetrievalTraceEntry {
                        memory_id: candidate.id,
                        decision: RetrievalDecision::Filtered(RetrievalFilterReason::OverriddenBy(
                            existing.id,
                        )),
                    });
                }
                // 已按 specificity 排序：窄 scope 覆盖同 semantic slot 的宽 scope。
                continue;
            }
            trace.push(RetrievalTraceEntry {
                memory_id: candidate.id,
                decision: RetrievalDecision::Selected,
            });
            selected.push(candidate);
            limit_reached = selected.len() >= context.limit;
        }
        // 只作上下文的记录不参与竞争与覆盖，附在所属分区末尾。
        selected.extend(context_only);

        // Retrieval 信号独立记账，绝不能变成用户 reinforcement。
        for memory in mandatory.iter_mut().chain(selected.iter_mut()) {
            memory.retrieval_count += 1;
            memory.last_retrieved_at = Some(context.at);
            self.repository.update(memory.clone())?;
        }

        trace.sort_by_key(|entry| entry.memory_id);
        let mut result = RetrievalResult {
            mandatory,
            conflicts,
            trace,
            ..RetrievalResult::default()
        };
        for memory in selected {
            match memory.memory_type {
                MemoryType::Constraint => result.constraints.push(memory),
                MemoryType::Preference => result.preferences.push(memory),
                MemoryType::Fact => result.facts.push(memory),
                MemoryType::Episode => result.episodes.push(memory),
            }
        }
        Ok(result)
    }
}

fn scope_matches(memory: &Memory, context: &RuntimeContext) -> bool {
    // 指定事实槽位的跨领域查询（如晨间习惯）未限定 domain；其他 Scope 条件仍须满足。
    if memory.memory_type == MemoryType::Fact
        && context.domain.is_none()
        && !context.predicates.is_empty()
    {
        let mut context = context.clone();
        context.domain.clone_from(&memory.scope.domain);
        return memory.scope.applies_to(&context);
    }
    memory.scope.applies_to(context)
}

fn target_matches(
    memory: &Memory,
    context: &RuntimeContext,
    relations: Option<&dyn SemanticRelationRegistry>,
) -> bool {
    // Fact 属性保留原语义；未明确 predicate/target 时不做对象过滤。
    if memory.memory_type == MemoryType::Fact
        || context.targets.is_empty()
        || !context.predicates.contains(&memory.predicate)
    {
        return true;
    }
    context.targets.iter().any(|target| {
        crate::semantic::equivalent_values(target, &memory.value)
            || (memory.memory_type != MemoryType::Episode
                && relations.is_some_and(|relations| relations.is_a(target, &memory.value)))
    })
}

fn set_trace_decision(
    trace: &mut [RetrievalTraceEntry],
    memory_id: MemoryId,
    decision: RetrievalDecision,
) {
    trace
        .iter_mut()
        .find(|entry| entry.memory_id == memory_id)
        .expect("selected memory must already have a trace")
        .decision = decision;
}

fn is_mandatory(memory: &Memory, context: &RuntimeContext) -> bool {
    context.temporal_intent == TemporalIntent::Current
        && memory.memory_type == MemoryType::Constraint
        && memory.retention_tier == RetentionTier::Durable
        && memory.status == MemoryStatus::Active
        && memory.scope.domain.is_none()
        && memory.scope.conditions.is_empty()
        && time_matches(memory, context)
}

fn status_matches(memory: &Memory, context: &RuntimeContext) -> bool {
    match context.temporal_intent {
        TemporalIntent::Current => memory.status == MemoryStatus::Active,
        TemporalIntent::Historical => match memory.status {
            MemoryStatus::Deleted => false,
            MemoryStatus::Superseded => {
                // Correction 从未在现实中成立，不能作为“历史事实”返回。
                memory.supersede_reason != Some(SupersedeReason::Correction)
            }
            MemoryStatus::Active | MemoryStatus::Expired => true,
        },
    }
}

fn time_matches(memory: &Memory, context: &RuntimeContext) -> bool {
    match &context.temporal_filter {
        Some(TemporalResolution::Range(range)) => memory.temporal.overlaps(*range),
        Some(TemporalResolution::Reference(reference)) => {
            memory.temporal.reference.as_ref() == Some(reference)
        }
        None if context.temporal_intent == TemporalIntent::Historical => {
            // 无日期的历史问句不是“此刻仍然成立吗”；但未来状态和真正未知的事件不能混入。
            memory
                .temporal
                .valid_from
                .is_none_or(|from| from.at <= context.at)
                && match &memory.temporal.reference {
                    None => true,
                    Some(TemporalReference::Unknown { expression, .. }) => {
                        crate::temporal::is_generic_past(expression)
                    }
                    Some(TemporalReference::Event { .. }) => false,
                }
        }
        None => memory.temporal.valid_at(context.at),
    }
}

/// 历史版本在互不相交的现实有效期内都可能正确，不能因同槽位而覆盖/报冲突。
fn memories_compete(
    left: &Memory,
    right: &Memory,
    cardinality: crate::Cardinality,
    context: &RuntimeContext,
) -> bool {
    let disjoint = match (
        left.temporal.valid_to,
        right.temporal.valid_from,
        right.temporal.valid_to,
        left.temporal.valid_from,
    ) {
        (Some(end), Some(start), _, _) if end.at <= start.at => true,
        (_, _, Some(end), Some(start)) if end.at <= start.at => true,
        _ => false,
    };
    memories_share_slot(left, right, cardinality)
        && !(context.temporal_intent == TemporalIntent::Historical && disjoint)
}

/// V1 只解析明确的“搬到某地之前”，不把任意事件名与证据做模糊匹配。
fn state_change_boundary(memories: &[Memory], context: &RuntimeContext) -> Option<TemporalRange> {
    let Some(TemporalResolution::Reference(TemporalReference::Event {
        event,
        relation: TemporalRelation::BeforeStart,
        ..
    })) = &context.temporal_filter
    else {
        return None;
    };
    if context.entities.is_empty() {
        return None;
    }
    let event = event.strip_prefix('我').unwrap_or(event);
    let place = event.strip_prefix("搬到")?.trim();
    // ponytail: 内存 V1 扫描版本链；数据量显著增长后改用 repository 的版本索引。
    let mut anchors = memories.iter().filter_map(|current| {
        if current.predicate.0 != "primary_residence"
            || current.memory_type != MemoryType::Fact
            || current.status == MemoryStatus::Deleted
            || current.supersede_reason == Some(SupersedeReason::Correction)
            || current.temporal.observed_at > context.at
            || current.value != MemoryValue::Text(place.to_owned())
            || (!context.entities.is_empty() && !context.entities.contains(&current.subject_id))
            || (!context.predicates.is_empty() && !context.predicates.contains(&current.predicate))
        {
            return None;
        }
        let previous = memories
            .iter()
            .find(|previous| Some(previous.id) == current.supersedes)?;
        if previous.status != MemoryStatus::Superseded
            || previous.superseded_by != Some(current.id)
            || previous.supersede_reason != Some(SupersedeReason::StateChanged)
            || previous.subject_id != current.subject_id
            || previous.predicate != current.predicate
            || previous.memory_type != current.memory_type
        {
            return None;
        }
        let boundary = previous.temporal.valid_to?.at;
        // 双向版本链与实际有效期都要一致；不能把未知事件的 observed_at 当成发生时间。
        (current.temporal.reference.is_none()
            && current.temporal.valid_from?.at == boundary
            && boundary <= context.at
            && boundary.0 > i64::MIN)
            .then_some(boundary)
    });
    let boundary = anchors.next()?;
    // 重复搬到同一城市或多人搬家均有歧义，不能随意挑一条时间。
    if anchors.next().is_some() {
        return None;
    }
    Some(TemporalRange {
        start: Timestamp(i64::MIN),
        end: boundary,
        source: TemporalSource::Supersession,
    })
}

fn same_assertion(left: &Memory, right: &Memory) -> bool {
    left.value == right.value && left.polarity == right.polarity && left.strength == right.strength
}

fn type_priority(memory_type: MemoryType) -> u8 {
    match memory_type {
        MemoryType::Constraint => 0,
        MemoryType::Preference => 1,
        MemoryType::Fact => 2,
        MemoryType::Episode => 3,
    }
}
