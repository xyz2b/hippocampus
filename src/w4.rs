//! W4 更新判定的代码侧（contracts 2.4 `w4.v2`、2.5）：模型输出的机械校验，以及把已校验的决定
//! 转换成 W5 依赖组。不调用模型，不判断语义：动作是否正确由 W4 负责，这里只保证输出合规、
//! 依据真实、目标与关系在输入范围内，再交给 W5 在事务里重查快照并执行。

use std::collections::{BTreeMap, BTreeSet};

use crate::contract::{
    Action, CandidateKind, ContractCandidate, DecisionStatus, EvidenceRef, PendingReason,
    QualifierOwner, RecordView2, RelationStatus as LinkStatus, SourceRef, TimeRole, Unprojectable,
    W4Candidate, W4Decision, W4Input, W4Output, locate_quote,
};
use crate::{
    EffectiveTime, MemoryId, MemoryRepository, PersistedMemoryState, PersistenceResult,
    RelationEvidence, SupersedeReason, W4PendingReason, W5Action, W5Candidate, W5Decision, W5Group,
    W5RelationSpec, w5_snapshot,
};

/// W4 输出不合规：整份输出不提交，原文保留（contracts 2.4）。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct W4Invalid(pub String);

impl std::fmt::Display for W4Invalid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "w4_invalid_output: {}", self.0)
    }
}

fn invalid<T>(message: impl Into<String>) -> Result<T, W4Invalid> {
    Err(W4Invalid(message.into()))
}

/// 结束目标的动作：必须逐目标有新旧双方依据。
fn terminal(action: Action) -> bool {
    matches!(
        action,
        Action::StateChange | Action::Correction | Action::Refine | Action::Retract
    )
}

fn stored(source: &SourceRef) -> Option<&str> {
    match source {
        SourceRef::Stored { source_id } => Some(source_id),
        SourceRef::Current {} => None,
    }
}

fn record_sources(record: &RecordView2) -> BTreeSet<&str> {
    let evidence = match record {
        RecordView2::Current(view) => &view.evidence,
        RecordView2::Legacy(view) => &view.evidence,
    };
    evidence.iter().filter_map(|e| stored(&e.source)).collect()
}

/// U3：在唯一的输出组装边界复制新候选依据；不推断动作，也不补旧方依据。
/// 调用方提供当前仍存在的原文。失败时不返回部分组装结果，validate 本身保持严格。
pub fn assemble_new_basis(
    input: &W4Input,
    output: &W4Output,
    text_of: &dyn Fn(&str) -> Option<String>,
) -> Result<W4Output, W4Invalid> {
    let mut assembled = output.clone();
    for decision in &mut assembled.decisions {
        let Some(candidate) = input
            .candidates
            .iter()
            .find(|c| c.index == decision.candidate)
        else {
            return invalid("决定引用了输入之外的候选");
        };
        let Some(id) = stored(&candidate.source) else {
            return invalid("新方依据只能引用已保存的来源");
        };
        let Some(text) = text_of(id) else {
            return invalid("新候选来源不存在或已删除");
        };
        let quote = &candidate.candidate.sentence;
        if quote.text.is_empty() || !text.contains(&quote.text) {
            return invalid("新候选原句不在当前来源原文中");
        }
        let equivalent = decision.basis.iter().any(|e| {
            e.source == candidate.source
                && locate_quote(&text, &e.quote.text).is_some_and(|range| text[range] == quote.text)
        });
        if !equivalent {
            decision.basis.push(EvidenceRef {
                source: candidate.source.clone(),
                quote: quote.clone(),
            });
        }
    }
    Ok(assembled)
}

/// 运行时边界：组装成功后仍执行原有严格校验。
pub fn prepare_output(
    input: &W4Input,
    output: &W4Output,
    text_of: &dyn Fn(&str) -> Option<String>,
) -> Result<W4Output, W4Invalid> {
    validate(input, &assemble_new_basis(input, output, text_of)?, text_of)
}

/// 机械校验 W4 输出，并把依据片段还原成原文写法（contracts 1.1）。`text_of` 返回已保存来源
/// 的原文；来源不存在或已删除时返回 None。任何一条不满足，整份输出无效。
pub fn validate(
    input: &W4Input,
    output: &W4Output,
    text_of: &dyn Fn(&str) -> Option<String>,
) -> Result<W4Output, W4Invalid> {
    let records: BTreeMap<MemoryId, &RecordView2> =
        input.records.iter().map(|r| (r.id(), r)).collect();
    // 输入里提供的来源：候选的原文、输入记录的依据、输入关系的依据。
    let mut provided: BTreeSet<&str> = BTreeSet::new();
    for candidate in &input.candidates {
        match stored(&candidate.source) {
            Some(id) => provided.insert(id),
            None => return invalid("W4 候选必须来自已保存的原文"),
        };
    }
    for record in &input.records {
        provided.extend(record_sources(record));
    }
    for relation in &input.relations {
        provided.extend(relation.basis.iter().filter_map(|e| stored(&e.source)));
    }

    let mut decided: Vec<usize> = output.decisions.iter().map(|d| d.candidate).collect();
    decided.sort_unstable();
    let mut expected: Vec<usize> = input.candidates.iter().map(|c| c.index).collect();
    expected.sort_unstable();
    if decided != expected {
        return invalid("每个候选必须恰好一个决定");
    }

    let mut normalized = output.clone();
    for decision in &mut normalized.decisions {
        let tag = format!("决定 {}", decision.candidate);
        let candidate = input
            .candidates
            .iter()
            .find(|c| c.index == decision.candidate)
            .expect("checked one-to-one");
        let own_source = stored(&candidate.source).expect("checked stored");
        let unique: BTreeSet<MemoryId> = decision.targets.iter().copied().collect();
        if unique.len() != decision.targets.len() {
            return invalid(format!("{tag}：targets 重复"));
        }
        if decision.targets.iter().any(|id| !records.contains_key(id)) {
            return invalid(format!("{tag}：targets 只能是输入记录"));
        }
        if candidate
            .committed_id
            .is_some_and(|id| unique.contains(&id))
        {
            return invalid(format!("{tag}：重判的断言不能以自身为目标"));
        }

        // 依据：只用输入里提供的已保存来源，片段逐字存在，还原为原文写法。
        for evidence in &mut decision.basis {
            let Some(id) = stored(&evidence.source).map(str::to_owned) else {
                return invalid(format!("{tag}：持久决定的依据只能引用已保存的来源"));
            };
            if !provided.contains(id.as_str()) {
                return invalid(format!("{tag}：依据引用了输入之外的来源 {id}"));
            }
            let Some(text) = text_of(&id) else {
                return invalid(format!("{tag}：来源 {id} 不存在或已删除"));
            };
            let Some(range) = locate_quote(&text, &evidence.quote.text) else {
                return invalid(format!("{tag}：依据片段不在来源 {id} 的原文中"));
            };
            evidence.quote.text = text[range].to_owned();
        }
        let cited: BTreeSet<&str> = decision
            .basis
            .iter()
            .filter_map(|e| stored(&e.source))
            .collect();

        match decision.status {
            DecisionStatus::Pending => {
                if decision.action.is_some() || decision.pending_reason.is_none() {
                    return invalid(format!(
                        "{tag}：pending 的 action 须为 null 且有 pending_reason"
                    ));
                }
                if !decision.resolves_relations.is_empty()
                    || decision.effective.is_some()
                    || decision.relation_scope.is_some()
                {
                    return invalid(format!("{tag}：pending 不解决关系、不带生效时间或冲突范围"));
                }
                continue;
            }
            DecisionStatus::Decided => {}
        }
        let Some(action) = decision.action else {
            return invalid(format!("{tag}：decided 须有 action"));
        };
        if decision.pending_reason.is_some() {
            return invalid(format!("{tag}：decided 的 pending_reason 须为 null"));
        }
        let count_ok = match action {
            Action::New | Action::Coexist => decision.targets.is_empty(),
            Action::Reinforce => decision.targets.len() == 1,
            _ => !decision.targets.is_empty(),
        };
        if !count_ok {
            return invalid(format!("{tag}：{action:?} 的目标数量不合规"));
        }
        // 候选类别与动作一致：撤回表达只能撤回，断言不能当撤回用。
        if (candidate.candidate.kind == CandidateKind::Retraction) != (action == Action::Retract) {
            return invalid(format!("{tag}：候选类别与动作不一致"));
        }
        if !cited.contains(own_source) {
            return invalid(format!("{tag}：依据须包含新候选的原文"));
        }
        // 有目标的动作逐目标检查旧方依据（不只是 conflict）。
        for target in &decision.targets {
            let record = records[target];
            if record_sources(record).is_disjoint(&cited) {
                return invalid(format!("{tag}：缺少目标 {} 的原文依据", target.0));
            }
            // 无法核对完整断言或限定语的旧记录不能被结束、强化或判为冲突，只能 pending。
            if matches!(record, RecordView2::Legacy(_)) {
                return invalid(format!(
                    "{tag}：目标 {} 是无法核对完整断言的旧记录，只能 pending",
                    target.0
                ));
            }
        }

        // 解决关系：只能是输入里的 open 关系，且只由结束或强化目标的动作提出。
        for id in &decision.resolves_relations {
            let open = input
                .relations
                .iter()
                .any(|r| &r.id == id && r.status == LinkStatus::Open);
            if !open {
                return invalid(format!("{tag}：只能解决输入中 open 的关系 {id}"));
            }
        }
        if !decision.resolves_relations.is_empty()
            && !(terminal(action) || action == Action::Reinforce)
        {
            return invalid(format!("{tag}：{action:?} 不能解决关系"));
        }

        // 生效时间：事件或有效期角色，且是该候选已核验时间之一。契约 2.4 不按动作限制它：
        // 结束目标的动作（state_change / correction / refine / retract）用它作结束时间，
        // 其余动作只把它记在决定里，不改任何记录。
        if let Some(effective) = &decision.effective {
            if effective.role == TimeRole::Retention {
                return invalid(format!("{tag}：生效时间不能用保存期限"));
            }
            if !candidate.candidate.times.contains(effective) {
                return invalid(format!("{tag}：生效时间须是该候选已核验的时间之一"));
            }
        }

        // 冲突范围：只在 conflict 时非空，引用新候选或目标完整记录的已核验限定语。
        if decision.relation_scope.is_some() != (action == Action::Conflict) {
            return invalid(format!("{tag}：relation_scope 只在 conflict 时非空"));
        }
        if let Some(scope) = &decision.relation_scope {
            let owner = |of: QualifierOwner| match of {
                QualifierOwner::Candidate => Some(&candidate.candidate),
                QualifierOwner::Record(id) => match records.get(&id) {
                    Some(RecordView2::Current(view)) if unique.contains(&id) => {
                        Some(&view.candidate)
                    }
                    _ => None,
                },
            };
            for reference in &scope.conditions {
                if owner(reference.of).is_none_or(|c| reference.index >= c.conditions.len()) {
                    return invalid(format!("{tag}：relation_scope 引用了不存在的条件"));
                }
            }
            for reference in &scope.times {
                match owner(reference.of).and_then(|c| c.times.get(reference.index)) {
                    Some(time) if time.role != TimeRole::Retention => {}
                    _ => return invalid(format!("{tag}：relation_scope 引用了无效的时间")),
                }
            }
        }
    }

    // 关系解决按整组验证：被解决关系的成员里，最多一个在本次决定之后仍然有效（保留方）；
    // 两个以上成员都还有效，冲突并没有解决。输入里已经结束的成员（之前被替代、撤回或过期）
    // 不算仍然有效：后续 W4 可以依据新原文显式解决这种历史关系。不在输入里的成员无法确认，
    // 按仍然有效计。同一关系不能被解决两次。
    let ended: BTreeSet<MemoryId> = normalized
        .decisions
        .iter()
        .filter(|d| d.action.is_some_and(terminal))
        .flat_map(|d| d.targets.iter().copied())
        .collect();
    let mut resolved: BTreeSet<&str> = BTreeSet::new();
    for decision in &normalized.decisions {
        for id in &decision.resolves_relations {
            if !resolved.insert(id) {
                return invalid(format!("关系 {id} 被解决了两次"));
            }
            let relation = input
                .relations
                .iter()
                .find(|r| &r.id == id)
                .expect("checked open relation");
            let kept = relation
                .members
                .iter()
                .filter(|member| {
                    !ended.contains(member) && records.get(member).is_none_or(|r| still_active(r))
                })
                .count();
            if kept > 1 {
                return invalid(format!(
                    "关系 {id} 仍有 {kept} 个成员在本次决定之后有效，不能算已解决"
                ));
            }
        }
    }
    Ok(normalized)
}

/// 输入记录当前是否仍然有效（未被替代、撤回或过期）。
fn still_active(record: &RecordView2) -> bool {
    match record {
        RecordView2::Current(view) => view.stored_status == crate::contract::StoredStatus::Active,
        RecordView2::Legacy(view) => view.data.status == crate::MemoryStatus::Active,
    }
}

/// 运行时为一次 W4 判定提供、模型不能给出的上下文。
pub struct W4Runtime<'a> {
    pub state: &'a PersistedMemoryState,
    pub owner: crate::UserId,
    /// 认领的原文在 `state.sources` 里的键。
    pub source_key: &'a str,
    /// 判定轮次：同一轮重试不变，重判换新轮次（进入组 ID）。
    pub round: u32,
    pub task_id: Option<crate::TaskId>,
    /// W3 已核验的上游结果，按候选索引给出；缺省视为都不成立。
    pub admissions: BTreeMap<usize, Admission>,
}

/// W3 对一个候选给出的、影响准入的可信结果（contracts 2.3）。
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Admission {
    /// Episode 的 episode_value 为 useful。
    pub episode_useful: bool,
    /// 有已核验、绑定到该候选的 remember 请求。
    pub remember_bound: bool,
}

/// 没有进入 W5 的候选及原因；候选与原文照常保留。
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq)]
pub enum Held {
    /// 现有 Memory 会误述的候选。
    Unprojectable(Unprojectable),
    /// Episode 没有跨轮价值依据：只保留原文。
    EpisodeNotAdmitted,
    /// 与另一个没有进入 W5 的候选处于同一依赖组：依赖动作不能只执行一部分（contracts 2.5）。
    DependsOnHeld { candidate: usize },
}

/// 转换结果：可交给 `commit_group` 的依赖组，以及没有进入 W5 的候选。
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub groups: Vec<W5Group>,
    pub held: BTreeMap<usize, Held>,
}

/// 把已校验的 W4 输出转换成 W5 依赖组。先 `validate`，再投影候选、执行 Episode 准入、
/// 映射动作、按依赖关系分组并记下快照。
pub fn plan(rt: &W4Runtime<'_>, input: &W4Input, output: &W4Output) -> PersistenceResult<Plan> {
    let state = rt.state;
    let source = state
        .sources
        .get(rt.source_key)
        .ok_or("W4 claimed source does not exist")?;
    let text_of = |id: &str| -> Option<String> {
        state
            .sources
            .values()
            .find(|s| s.id == id)
            .and_then(crate::lexical::body)
            .map(str::to_owned)
    };
    let output = validate(input, output, &text_of).map_err(|e| e.to_string())?;
    let owner = rt.owner.clone();

    // 依赖分组先按完整的 W4 决定做（不看候选能否进入 W5）：共享目标、共享要解决的关系、
    // 目标或 committed_id 是对方所解决关系的成员。
    let touched = |decision: &W4Decision| -> BTreeSet<MemoryId> {
        let mut ids: BTreeSet<MemoryId> = decision.targets.iter().copied().collect();
        ids.extend(
            input
                .candidates
                .iter()
                .find(|c| c.index == decision.candidate)
                .and_then(|c| c.committed_id),
        );
        for id in &decision.resolves_relations {
            if let Some(relation) = input.relations.iter().find(|r| &r.id == id) {
                ids.extend(relation.members.iter().copied());
            }
        }
        ids
    };
    let touches: Vec<BTreeSet<MemoryId>> = output.decisions.iter().map(touched).collect();
    let mut group_of: Vec<usize> = (0..output.decisions.len()).collect();
    loop {
        let mut changed = false;
        for a in 0..touches.len() {
            for b in a + 1..touches.len() {
                if group_of[a] != group_of[b] && !touches[a].is_disjoint(&touches[b]) {
                    let (from, into) = (group_of[b].max(group_of[a]), group_of[a].min(group_of[b]));
                    for g in &mut group_of {
                        if *g == from {
                            *g = into;
                        }
                    }
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }

    let mut held = BTreeMap::new();
    let mut planned: Vec<Option<(W5Candidate, W5Decision)>> = Vec::new();
    for decision in &output.decisions {
        let candidate = input
            .candidates
            .iter()
            .find(|c| c.index == decision.candidate)
            .expect("validated");
        if stored(&candidate.source) != Some(source.id.as_str()) {
            return Err(format!(
                "W4 candidate {} does not come from the claimed source",
                candidate.index
            )
            .into());
        }
        let admission = rt
            .admissions
            .get(&candidate.index)
            .copied()
            .unwrap_or_default();
        let contract = ContractCandidate {
            candidate: candidate.candidate.clone(),
            source: candidate.source.clone(),
            said_on: candidate.said_on.clone(),
            time_resolutions: candidate.time_resolutions.clone(),
        };
        let projection = match contract.project(&crate::contract::ProjectionContext {
            owner_id: owner.clone(),
            session_id: crate::SessionId::from(source.session_id.as_str()),
            turn_id: crate::TurnId::from(source.turn_id.as_str()),
            evidence_id: crate::EvidenceId(format!("w1:{}:{}", source.id, candidate.index)),
            observed_at: crate::Timestamp(source.observed_at),
            task_id: rt.task_id.clone(),
            explicit_memory_request: admission.remember_bound,
        }) {
            Ok(projection) => projection,
            Err(reason) => {
                held.insert(candidate.index, Held::Unprojectable(reason));
                planned.push(None);
                continue;
            }
        };
        // Episode 准入在 W4 判定之后：有用、有绑定的 remember，或 W4 判为对既有记录的
        // 更新 / 撤回，才进入 W5（contracts 2.3）。
        let updates_existing = decision
            .action
            .is_some_and(|action| terminal(action) || action == Action::Reinforce);
        if projection.candidate.memory_type == crate::MemoryType::Episode
            && !(admission.episode_useful || admission.remember_bound || updates_existing)
        {
            held.insert(candidate.index, Held::EpisodeNotAdmitted);
            planned.push(None);
            continue;
        }
        let w5_candidate = W5Candidate {
            index: candidate.index,
            candidate: projection.candidate,
            committed_id: candidate.committed_id,
            contract: Some(contract),
        };
        let w5_decision = to_w5(candidate, decision, input, source.id.as_str())?;
        planned.push(Some((w5_candidate, w5_decision)));
    }

    let mut groups = Vec::new();
    let mut labels: Vec<usize> = group_of.clone();
    labels.sort_unstable();
    labels.dedup();
    for label in labels {
        let positions: Vec<usize> = (0..group_of.len())
            .filter(|position| group_of[*position] == label)
            .collect();
        // 组里有候选没有进入 W5：整个依赖组都不提交（contracts 2.5 的原子性），独立的组照常。
        if let Some(blocker) = positions.iter().find(|p| planned[**p].is_none()) {
            let blocker = output.decisions[*blocker].candidate;
            for position in &positions {
                if planned[*position].is_some() {
                    held.insert(
                        output.decisions[*position].candidate,
                        Held::DependsOnHeld { candidate: blocker },
                    );
                }
            }
            continue;
        }
        let members: Vec<&(W5Candidate, W5Decision)> = positions
            .iter()
            .filter_map(|position| planned[*position].as_ref())
            .collect();
        let first = members
            .iter()
            .map(|(candidate, _)| candidate.index)
            .min()
            .expect("non-empty group");
        let mut targets: BTreeSet<MemoryId> = BTreeSet::new();
        let mut lineage: BTreeSet<MemoryId> = BTreeSet::new();
        let mut resolves: Vec<String> = Vec::new();
        for (candidate, decision) in &members {
            targets.extend(decision.targets.iter().copied());
            resolves.extend(decision.resolves_relations.iter().cloned());
            if let Some(committed) = candidate.committed_id {
                // 重判：输入里给出的前身也进入快照。W5 据此判断这次重判是否覆盖了
                // update_relation_unclear（w5-storage 第 3 节），提交前也核对它们没有变化。
                lineage.insert(committed);
                lineage.extend(
                    crate::commit::related_records(state, committed)
                        .into_iter()
                        .filter(|id| input.records.iter().any(|r| r.id() == *id)),
                );
            }
        }
        targets.extend(lineage.iter().copied());
        let targets: Vec<MemoryId> = targets.into_iter().collect();
        let (seen, mut seen_relations) = w5_snapshot(state, &targets, &resolves)?;
        // w5_snapshot 会补入目标所在的 open 关系；被任务隔离挡住的关系不算 W4 已见。
        // 目标的 open 关系编号集合仍保留，用于提交时检测关系集合变化。
        seen_relations.retain(|seen| input.relations.iter().any(|link| link.id == seen.id));
        // 前身所在的已解决 / 已失效关系也是判定依据的一部分：记下快照。
        for link in &input.relations {
            if link.members.iter().any(|member| lineage.contains(member))
                && !seen_relations.iter().any(|seen| seen.id == link.id)
            {
                let relation = state
                    .relations
                    .get(&link.id)
                    .ok_or_else(|| format!("W5 relation {} does not exist", link.id))?;
                seen_relations.push(crate::W5SeenRelation {
                    fingerprint: crate::w5_relation_fingerprint(relation)?,
                    id: link.id.clone(),
                });
            }
        }
        groups.push(W5Group {
            id: format!("{}#{}#{}", rt.source_key, rt.round, first),
            source_key: rt.source_key.to_owned(),
            candidates: members.iter().map(|(c, _)| c.clone()).collect(),
            decisions: members.iter().map(|(_, d)| d.clone()).collect(),
            seen,
            scope_complete: input.coverage.complete,
            seen_relations,
        });
    }
    Ok(Plan { groups, held })
}

/// 一个已校验的 W4 决定对应的 W5 决定（contracts 2.4 动作对应表）。
fn to_w5(
    candidate: &W4Candidate,
    decision: &W4Decision,
    input: &W4Input,
    own_source: &str,
) -> PersistenceResult<W5Decision> {
    let retraction = candidate.candidate.kind == CandidateKind::Retraction;
    let evidence = |e: &EvidenceRef| -> RelationEvidence {
        RelationEvidence {
            source_id: stored(&e.source).unwrap_or_default().to_owned(),
            quote: e.quote.text.clone(),
        }
    };
    let mut basis_sources: Vec<String> = decision
        .basis
        .iter()
        .filter_map(|e| stored(&e.source).map(str::to_owned))
        .collect();
    basis_sources.sort();
    basis_sources.dedup();
    let mut out = W5Decision {
        candidate: decision.candidate,
        action: W5Action::New,
        targets: decision.targets.clone(),
        basis_sources,
        reason: None,
        effective: None,
        resolves_relations: decision.resolves_relations.clone(),
        resolution: None,
        relation: None,
        pending_reason: None,
        basis: decision.basis.clone(),
        effective_time: None,
        relation_scope: None,
    };
    let Some(action) = decision.action else {
        // pending：断言追加为新记录，撤回零写入；不对目标执行动作。
        out.action = if retraction {
            W5Action::Retract
        } else {
            W5Action::New
        };
        out.pending_reason = Some(match decision.pending_reason {
            Some(PendingReason::TargetUnknown) => W4PendingReason::TargetUnknown,
            Some(PendingReason::ScopeIncomplete) => W4PendingReason::ScopeIncomplete,
            Some(PendingReason::UnclearRelation) | None => W4PendingReason::UnclearRelation,
        });
        return Ok(out);
    };
    out.action = match action {
        Action::New | Action::Coexist => W5Action::New,
        Action::Reinforce => W5Action::Reinforce,
        Action::StateChange | Action::Correction | Action::Refine => W5Action::StateChange,
        Action::Retract => W5Action::Retract,
        Action::Conflict => W5Action::Conflict,
    };
    out.reason = match action {
        Action::StateChange => Some(match candidate.candidate.memory_type.as_str() {
            "preference" => SupersedeReason::PreferenceChanged,
            "constraint" => SupersedeReason::ConstraintChanged,
            _ => SupersedeReason::StateChanged,
        }),
        Action::Correction => Some(SupersedeReason::Correction),
        // 契约允许 Refinement / ScopeRefined，但 W4 输出不区分；统一用 Refinement。
        Action::Refine => Some(SupersedeReason::Refinement),
        _ => None,
    };
    if let Some(effective) = &decision.effective {
        // 换算结果取候选已核验时间里的对应项，不由 W4 给。
        let index = candidate
            .candidate
            .times
            .iter()
            .position(|time| time == effective)
            .ok_or("W4 effective is not one of the candidate's times")?;
        let resolution = candidate
            .time_resolutions
            .get(index)
            .ok_or("W4 candidate has no resolution for its effective time")?
            .clone();
        let time = EffectiveTime {
            interp: effective.clone(),
            resolution,
        };
        out.effective = time.point();
        out.effective_time = Some(time);
    }
    if !decision.resolves_relations.is_empty() {
        // 解决依据来自本原文（W4 依据新原文判定已解决）。
        let own = decision
            .basis
            .iter()
            .find(|e| stored(&e.source) == Some(own_source))
            .ok_or("W4 resolution needs basis from the claimed source")?;
        out.resolution = Some(evidence(own));
    }
    if let Some(scope) = &decision.relation_scope {
        let owner = |of: QualifierOwner| match of {
            QualifierOwner::Candidate => Some(&candidate.candidate),
            QualifierOwner::Record(id) => input.records.iter().find_map(|r| match r {
                RecordView2::Current(view) if view.id == id => Some(&view.candidate),
                _ => None,
            }),
        };
        let conditions: Vec<_> = scope
            .conditions
            .iter()
            .filter_map(|r| owner(r.of).and_then(|c| c.conditions.get(r.index)))
            .collect();
        let times: Vec<_> = scope
            .times
            .iter()
            .filter_map(|r| owner(r.of).and_then(|c| c.times.get(r.index)))
            .collect();
        out.relation = Some(W5RelationSpec {
            conditions: serde_json::to_value(conditions)?,
            times: serde_json::to_value(times)?,
            basis: decision.basis.iter().map(evidence).collect(),
        });
        out.relation_scope = Some(scope.clone());
    }
    Ok(out)
}

// ---------------- 输入组装（contracts 2.4 “相关记录”、3.2 RecordView2）

/// 记录的依据里能链接到真实获准来源的引用（原文仍在、片段未被清洗）。
fn evidence_refs(state: &PersistedMemoryState, memory: &crate::Memory) -> Vec<EvidenceRef> {
    memory
        .evidence
        .iter()
        .filter_map(|evidence| {
            let source = state.sources.values().find(|s| {
                s.session_id == evidence.session_id.0 && s.turn_id == evidence.turn_id.0
            })?;
            let body = crate::lexical::body(source)?;
            body.contains(evidence.quote.as_str()).then(|| EvidenceRef {
                source: SourceRef::Stored {
                    source_id: source.id.clone(),
                },
                quote: crate::contract::Quote {
                    text: evidence.quote.clone(),
                },
            })
        })
        .collect()
}

/// 结束这条记录的已提交决定（state_change / retract，非待定）。
fn ending_decision(state: &PersistedMemoryState, id: MemoryId) -> Option<&W5Decision> {
    state
        .w5_receipts
        .values()
        .filter(|receipt| receipt.status == crate::W5Status::Committed)
        .flat_map(|receipt| receipt.decisions.iter())
        .find(|decision| {
            decision.pending_reason.is_none()
                && matches!(decision.action, W5Action::StateChange | W5Action::Retract)
                && decision.targets.contains(&id)
        })
}

/// 全部前身：产生这条记录的 state_change 决定的目标，逐层向上（排序、去重）。
fn history(state: &PersistedMemoryState, id: MemoryId) -> Vec<MemoryId> {
    let mut found: BTreeSet<MemoryId> = BTreeSet::new();
    let mut queue = vec![id];
    while let Some(current) = queue.pop() {
        let mut parents: Vec<MemoryId> = state
            .w5_receipts
            .values()
            .filter(|receipt| receipt.status == crate::W5Status::Committed)
            .flat_map(|receipt| {
                receipt.decisions.iter().filter(move |decision| {
                    decision.pending_reason.is_none()
                        && decision.action == W5Action::StateChange
                        && receipt.products.get(&decision.candidate) == Some(&current)
                })
            })
            .flat_map(|decision| decision.targets.iter().copied())
            .collect();
        if let Ok(Some(memory)) = state.repository.get(current) {
            parents.extend(memory.supersedes);
        }
        for parent in parents {
            if parent != id && found.insert(parent) {
                queue.push(parent);
            }
        }
    }
    found.into_iter().collect()
}

/// 一条记录的 W4 输入视图。有完整候选和更新状态的给完整记录视图，其余按旧记录给出并列出缺口；
/// 已删除的记录不进入任何分支。
pub fn record_view(state: &PersistedMemoryState, id: MemoryId) -> Option<RecordView2> {
    use crate::contract::{
        CurrentRecordView2, Ended, Grade, LegacyGap, LegacyRecordView, StoredStatus,
    };
    let memory = state.repository.get(id).ok().flatten()?;
    let stored_status = match memory.status {
        crate::MemoryStatus::Active => StoredStatus::Active,
        crate::MemoryStatus::Superseded => StoredStatus::Superseded,
        crate::MemoryStatus::Expired => StoredStatus::Expired,
        crate::MemoryStatus::Deleted => return None,
    };
    let evidence = evidence_refs(state, &memory);
    let update = state.record_updates.get(&id).cloned();
    if let (Some(contract), Some(update)) = (crate::w5_record_contract(state, id), update.clone()) {
        let (ended, end) = if memory.status != crate::MemoryStatus::Superseded {
            (Ended::NotEnded, None)
        } else {
            match ending_decision(state, id).and_then(|d| d.effective_time.as_ref()) {
                Some(time)
                    if time.resolution.status == crate::contract::ResolutionStatus::Resolved =>
                {
                    (Ended::Known, Some(time.resolution.clone()))
                }
                Some(_) => (Ended::EndUnknown, None),
                // 结束它的决定没有保存换算结果：按旧字段判断，不补造精度。
                None if memory.temporal.valid_to.is_some() => (Ended::Known, None),
                None => (Ended::EndUnknown, None),
            }
        };
        return Some(RecordView2::Current(CurrentRecordView2 {
            id,
            stored_status,
            termination_reason: memory.supersede_reason,
            ended,
            end,
            candidate: contract.candidate,
            evidence,
            grade: Grade::Committed,
            said_on: contract.said_on,
            time_resolutions: contract.time_resolutions,
            history: history(state, id),
            open_relations: crate::w5_open_relations_of(state, id),
            update,
        }));
    }
    let mut gaps = Vec::new();
    if evidence.is_empty() {
        gaps.push(LegacyGap::SourceMissing);
    }
    gaps.push(LegacyGap::SentenceMissing);
    gaps.push(LegacyGap::QualifiersUnmapped);
    if memory.temporal.valid_from.is_some()
        || memory.temporal.valid_to.is_some()
        || memory.temporal.reference.is_some()
    {
        gaps.push(LegacyGap::TimeUnmapped);
    }
    Some(RecordView2::Legacy(LegacyRecordView {
        id,
        data: memory,
        evidence,
        grade: Grade::Committed,
        update,
        gaps,
    }))
}

/// 关系的 W4 输入视图。
fn relation_link(
    relation: &crate::StoredRelation,
) -> PersistenceResult<crate::contract::RelationLink> {
    use crate::contract::{Quote, RelationKind, RelationLink};
    let evidence = |e: &RelationEvidence| EvidenceRef {
        source: SourceRef::Stored {
            source_id: e.source_id.clone(),
        },
        quote: Quote {
            text: e.quote.clone(),
        },
    };
    Ok(RelationLink {
        id: relation.id.clone(),
        kind: RelationKind::Conflict,
        members: relation.members.clone(),
        conditions: serde_json::from_value(relation.conditions.clone())?,
        times: serde_json::from_value(relation.times.clone())?,
        basis: relation.basis.iter().map(evidence).collect(),
        status: match relation.status {
            crate::RelationStatus::Open => LinkStatus::Open,
            crate::RelationStatus::Resolved => LinkStatus::Resolved,
            crate::RelationStatus::Invalidated => LinkStatus::Invalidated,
        },
        resolution: relation.resolution.as_ref().map(evidence),
    })
}

/// 待组装的一个候选：W3 已支持的完整候选，加运行时绑定的索引与（重判时的）已提交记录。
#[derive(Clone, Debug, PartialEq)]
pub struct Pending {
    pub index: usize,
    pub committed_id: Option<MemoryId>,
    pub contract: ContractCandidate,
}

/// 组装结果。没有相关记录和关系时不调用模型，直接用 `skipped`（断言新增；撤回待定）。
/// 必需记录超出预算时也不调用模型，整批用 `skipped` 待定（scope_incomplete）。
#[derive(Clone, Debug, PartialEq)]
pub struct Assembly {
    pub input: W4Input,
    pub call_w4: bool,
    pub skipped: W4Output,
    /// 按 fact 候选索引报告检索状态；不进入 W4 契约或持久存储。
    pub fact_retrieval: BTreeMap<usize, FactRetrieval>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FactRetrieval {
    FullText,
    /// 索引不可用时沿用旧词元规则，原因与成功但零命中区分。
    Fallback(String),
}

/// W4 输入预算（contracts 2.4“合计不超过输入预算”）。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct W4Budget {
    /// 每个通用 `fact` 候选最多取多少条检索命中。
    pub facts: usize,
    /// 输入记录总数上限。专用关系记录、关系成员与重判的前身不能被截掉：它们超出上限时
    /// 不调用 W4，整批待定（scope_incomplete）。
    pub records: usize,
}

/// 全文索引不可用时的降级：与候选共有至少两个词元，按共有数从高到低。
fn fallback_fact_hits(sentence: &str, records: &[&crate::Memory]) -> Vec<MemoryId> {
    let wanted: BTreeSet<String> = crate::lexical::tokens(sentence).into_iter().collect();
    let mut scored: Vec<(usize, MemoryId)> = records
        .iter()
        .filter_map(|memory| {
            let mut text = match &memory.value {
                crate::MemoryValue::Text(text) => text.clone(),
                _ => String::new(),
            };
            for evidence in &memory.evidence {
                text.push_str(&evidence.quote);
            }
            let have: BTreeSet<String> = crate::lexical::tokens(&text).into_iter().collect();
            let shared = wanted.intersection(&have).count();
            (shared >= 2).then_some((shared, memory.id))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, id)| id).collect()
}

/// 从一组记录出发要送入的全部记录与关系，展开到不再增加（结果与关系编号无关）。
struct Expansion {
    records: BTreeSet<MemoryId>,
    relations: BTreeSet<String>,
    /// 因含有本次任务范围之外的成员而整条不送入的关系里，被挡住的成员数。
    withheld: usize,
}

/// - 已选记录（以及重判的记录）所在的 open 关系：关系与全部成员。记录的前身所在的 open
///   关系同样送入（contracts 2.5：替代未解决成员后关系保持 open，只能由后续 W4 解决）。
/// - `lineage`（重判的记录及其前身）所在的关系不论状态都送入，W4 看到的是最新关系状态。
/// - 关系只要有一个成员不在本次任务范围内就整条不送入，也不带入它的其他成员：
///   不能截掉成员后仍把关系当作完整的。调用方据 `withheld` 把范围报告为不完整。
fn expand(
    state: &PersistedMemoryState,
    in_scope: &dyn Fn(MemoryId) -> bool,
    seeds: &BTreeSet<MemoryId>,
    committed: &BTreeSet<MemoryId>,
    lineage: &BTreeSet<MemoryId>,
) -> Expansion {
    let mut records = seeds.clone();
    let mut relations: BTreeSet<String> = BTreeSet::new();
    let mut blocked: BTreeMap<String, usize> = BTreeMap::new();
    let mut ancestors: BTreeMap<MemoryId, Vec<MemoryId>> = BTreeMap::new();
    loop {
        let mut changed = false;
        // 已选记录、重判记录，以及它们的全部前身：前身所在的 open 关系仍未解决。
        let mut reach: BTreeSet<MemoryId> = records.union(committed).copied().collect();
        for id in records.iter().chain(committed) {
            reach.extend(
                ancestors
                    .entry(*id)
                    .or_insert_with(|| history(state, *id))
                    .iter()
                    .copied(),
            );
        }
        for relation in state.relations.values() {
            if relations.contains(&relation.id) || blocked.contains_key(&relation.id) {
                continue;
            }
            let open_hit = relation.status == crate::RelationStatus::Open
                && relation.members.iter().any(|member| reach.contains(member));
            let lineage_hit = relation
                .members
                .iter()
                .any(|member| lineage.contains(member));
            if !(open_hit || lineage_hit) {
                continue;
            }
            let outside = relation
                .members
                .iter()
                .filter(|member| !in_scope(**member))
                .count();
            if outside > 0 {
                blocked.insert(relation.id.clone(), outside);
            } else {
                relations.insert(relation.id.clone());
                records.extend(relation.members.iter().copied());
            }
            changed = true;
        }
        if !changed {
            break;
        }
    }
    // 关系保留原成员 ID（删除后关系已失效），但删除、缺失或越界记录不送入，也不占预算。
    records.retain(|id| {
        in_scope(*id)
            && state
                .repository
                .get(*id)
                .ok()
                .flatten()
                .is_some_and(|memory| memory.status != crate::MemoryStatus::Deleted)
    });
    Expansion {
        records,
        relations,
        withheld: blocked.values().sum(),
    }
}

/// 在同一 SQLite 读事务中取得全文命中并组装 W4 输入（contracts 2.4）。
/// 返回状态快照供 plan 复用；模型调用后不能重新取状态补造旧判断的快照。
/// - 专用关系：同主体、同关系的全部有效记录，不受相似度截断。
/// - 通用 `fact`：检索命中的同主体记录，每个候选最多 `budget.facts` 条；范围恒不完整。
/// - 重判（committed_id）：产生该记录的决定替代或列出过的前身，及其所在关系的最新状态。
/// - 已选记录及其前身所在 open 关系的全部成员与关系本身，展开到不再增加。
/// - 以上除 fact 命中外都是必须送入的；合计超过 `budget.records` 时不调用 W4，整批待定。
///   fact 命中按全文检索顺序逐条加入，放不下的唯一记录计入 `coverage.omitted`。
///
/// 任务范围由可信上下文决定：带其他任务条件的记录不进入输入，含这种成员的关系整条不送入，
/// 并把范围报告为不完整。
pub fn assemble(
    store: &crate::SqliteMemoryStore,
    source_key: &str,
    task_id: Option<&crate::TaskId>,
    candidates: &[Pending],
    budget: W4Budget,
) -> PersistenceResult<(PersistedMemoryState, Assembly)> {
    use crate::contract::{Action, DecisionStatus, Subject, W4Candidate, W4Coverage, W4Decision};
    let tx = store.connection.unchecked_transaction()?;
    let snapshot = crate::SqliteMemoryStore::load(&tx, store.owner())?;
    let state = &snapshot;
    let source = state
        .sources
        .get(source_key)
        .ok_or("W4 claimed source does not exist")?;
    let all = state.repository.list_by_owner(store.owner())?;
    let in_task = |memory: &crate::Memory| {
        memory
            .scope
            .conditions
            .iter()
            .all(|condition| match condition {
                crate::ScopeCondition::Task(task) => task_id == Some(task),
                _ => true,
            })
    };
    // 已删除或不存在的成员不算越界（它们本来就不会出现在输入里）。
    let in_scope = |id: MemoryId| {
        all.iter()
            .find(|memory| memory.id == id)
            .is_none_or(|memory| memory.status == crate::MemoryStatus::Deleted || in_task(memory))
    };
    let committed: BTreeSet<MemoryId> = candidates.iter().filter_map(|c| c.committed_id).collect();
    let mut complete = true;
    let mut omitted = 0;

    // 必须送入的集合：专用关系的当前记录、重判记录的前身。
    let mut required: BTreeSet<MemoryId> = BTreeSet::new();
    let mut lineage: BTreeSet<MemoryId> = committed.clone();
    for id in &committed {
        for parent in crate::commit::related_records(state, *id) {
            if in_scope(parent) {
                required.insert(parent);
                lineage.insert(parent);
            } else {
                complete = false;
                omitted += 1;
            }
        }
    }
    let mut fact_candidates: Vec<Vec<MemoryId>> = Vec::new();
    let mut fact_retrieval = BTreeMap::new();
    for candidate in candidates {
        let Subject::Entity(subject) = &candidate.contract.candidate.subject else {
            continue;
        };
        let relation = candidate.contract.candidate.relation.as_str();
        let same_subject: Vec<&crate::Memory> = all
            .iter()
            .filter(|memory| {
                memory.status == crate::MemoryStatus::Active
                    && memory.subject_id.0 == subject.entity_id
                    && in_task(memory)
                    && !committed.contains(&memory.id)
            })
            .collect();
        if relation == "fact" {
            complete = false;
            let facts: Vec<&crate::Memory> = same_subject
                .into_iter()
                .filter(|memory| memory.predicate.0 == "fact")
                .collect();
            let sentence = &candidate.contract.candidate.sentence.text;
            let (hits, retrieval) = match store.search_w4_facts(&tx, state, sentence, &facts) {
                Ok(hits) => (hits, FactRetrieval::FullText),
                Err(error) => (
                    fallback_fact_hits(sentence, &facts),
                    FactRetrieval::Fallback(error.to_string()),
                ),
            };
            fact_candidates.push(hits);
            fact_retrieval.insert(candidate.index, retrieval);
        } else {
            required.extend(
                same_subject
                    .into_iter()
                    .filter(|memory| memory.predicate.0 == relation)
                    .map(|memory| memory.id),
            );
        }
    }
    let mut chosen = expand(state, &in_scope, &required, &committed, &lineage);
    let overflow = chosen.records.len() > budget.records;
    let mut omitted_facts = BTreeSet::new();
    if !overflow {
        // fact 命中按检索顺序逐条加入：连同它带入的关系成员放得下才加。
        let mut seeds = required.clone();
        for hits in &fact_candidates {
            let mut taken = 0;
            for hit in hits {
                if chosen.records.contains(hit) {
                    continue;
                }
                let mut trial = seeds.clone();
                trial.insert(*hit);
                let expansion = expand(state, &in_scope, &trial, &committed, &lineage);
                if taken < budget.facts && expansion.records.len() <= budget.records {
                    seeds = trial;
                    chosen = expansion;
                    taken += 1;
                } else {
                    omitted_facts.insert(*hit);
                }
            }
        }
    }
    // 同一记录可被多个候选命中，也可稍后随关系带入；只计最终未送入的唯一命中记录。
    omitted += omitted_facts.difference(&chosen.records).count();
    if chosen.withheld > 0 {
        complete = false;
        omitted += chosen.withheld;
    }
    let (records, relations) = if overflow {
        // 必须送入的集合超出预算：如实报告，不送入被截断的输入。
        complete = false;
        omitted += chosen.records.len();
        (Vec::new(), Vec::new())
    } else {
        (
            chosen
                .records
                .iter()
                .filter_map(|id| record_view(state, *id))
                .collect::<Vec<RecordView2>>(),
            chosen
                .relations
                .iter()
                .map(|id| relation_link(&state.relations[id]))
                .collect::<PersistenceResult<Vec<_>>>()?,
        )
    };
    let input = W4Input {
        candidates: candidates
            .iter()
            .map(|candidate| W4Candidate {
                index: candidate.index,
                committed_id: candidate.committed_id,
                candidate: candidate.contract.candidate.clone(),
                source: candidate.contract.source.clone(),
                said_on: candidate.contract.said_on.clone(),
                time_resolutions: candidate.contract.time_resolutions.clone(),
            })
            .collect(),
        records,
        relations,
        coverage: W4Coverage { complete, omitted },
    };
    let call_w4 = !input.records.is_empty() || !input.relations.is_empty();
    // 不调用 W4 时的决定：断言新增（范围不完整由 coverage 带出）；撤回没有可定位的目标。
    // 必须送入的集合超出预算时全部待定（scope_incomplete），不对任何旧记录执行动作。
    let skipped = W4Output {
        decisions: candidates
            .iter()
            .map(|candidate| {
                let retraction = candidate.contract.candidate.kind == CandidateKind::Retraction;
                W4Decision {
                    candidate: candidate.index,
                    status: if retraction || overflow {
                        DecisionStatus::Pending
                    } else {
                        DecisionStatus::Decided
                    },
                    action: (!retraction && !overflow).then_some(Action::New),
                    targets: Vec::new(),
                    resolves_relations: Vec::new(),
                    basis: vec![EvidenceRef {
                        source: SourceRef::Stored {
                            source_id: source.id.clone(),
                        },
                        quote: candidate.contract.candidate.sentence.clone(),
                    }],
                    effective: None,
                    relation_scope: None,
                    pending_reason: if overflow {
                        Some(PendingReason::ScopeIncomplete)
                    } else {
                        retraction.then_some(PendingReason::TargetUnknown)
                    },
                }
            })
            .collect(),
    };
    let assembly = Assembly {
        input,
        call_w4,
        skipped,
        fact_retrieval,
    };
    tx.commit()?;
    Ok((snapshot, assembly))
}
