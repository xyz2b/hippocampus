//! `memory_forget` 的删除范围（docs/architecture/w5-storage.md 第 4 节）。
//!
//! 1. 起点：source_id / staged_id 为对应原文；memory_id 为目标记录。
//! 2. 谱系（只对 memory_id）：从原始目标分别求全部前身与全部继任，取并集；向上只向上、向下只向下，
//!    不从继任者折回它的其他前身。
//! 3. 整条消息：起点原文及谱系记录的原文整条删除，这些原文派生的记录按第 5 条处理。
//! 4. 不再迭代：因“同一原文”连带删除的记录，不再把它们的其他原文加入删除集合。
//! 5. 部分证据被删的记录，剩余证据里仍有能确认的独立支持来源才保留；能确认的支持 = 创建该记录的
//!    原文 + 回执中对它做 reinforce 的原文（旧整理回执与已提交的 W5 回执），查不清即删除。
//! 6. 保留时：被删原文的证据打墓碑，强化次数与最近强化时间按剩余的独立强化来源重算。
//! 7. 成员、依据或解决依据的原文被删除的关系置为 invalidated（含 resolved），关系与 W5 回执里出自
//!    被删原文的文字一并清洗。继任被删、前身仍在时，前身保持 superseded。

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use crate::*;

type Turn = (String, String);

const TOMBSTONE: &str = "[deleted source]";

/// 清洗时保留的元数据字段：它们是定位信息，不是原文。
const METADATA_KEYS: &[&str] = &[
    "id",
    "session_id",
    "turn_id",
    "source_id",
    "source_key",
    "owner_id",
    "subject_id",
    "entity_id",
    "group_id",
    "predicate",
    "kind",
    "status",
];

fn turn_of(evidence: &Evidence) -> Turn {
    (evidence.session_id.0.clone(), evidence.turn_id.0.clone())
}

/// 已提交的 W5 回执里，每个候选的原文轮次（只认证据全部来自同一轮的候选；多源候选角色不明）。
fn candidate_turns(fingerprint: &str) -> BTreeMap<usize, Turn> {
    let Ok(group) = serde_json::from_str::<Value>(fingerprint) else {
        return BTreeMap::new();
    };
    let mut turns = BTreeMap::new();
    for candidate in group["candidates"].as_array().into_iter().flatten() {
        let Some(index) = candidate["index"].as_u64() else {
            continue;
        };
        let set: BTreeSet<Turn> = candidate["candidate"]["evidence"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|e| {
                Some((
                    e["session_id"].as_str()?.to_owned(),
                    e["turn_id"].as_str()?.to_owned(),
                ))
            })
            .collect();
        if set.len() == 1 {
            turns.insert(index as usize, set.into_iter().next().unwrap());
        }
    }
    turns
}

/// 由回执确认的证据角色。
#[derive(Default)]
struct Roles {
    /// 创建或强化该记录的原文轮次。
    support: BTreeMap<MemoryId, BTreeSet<Turn>>,
    /// 其中强化它的原文轮次。
    reinforce: BTreeMap<MemoryId, BTreeSet<Turn>>,
    /// 其中创建它的原文轮次。同一原文对自己创建的记录再做强化不增加计数，重算时也不算。
    created: BTreeMap<MemoryId, BTreeSet<Turn>>,
}

impl Roles {
    fn build(state: &PersistedMemoryState) -> Self {
        let mut roles = Roles::default();
        let request_turn = |request: &Value| -> Option<Turn> {
            Some((
                request["session_id"].as_str()?.to_owned(),
                request["turn_id"].as_str()?.to_owned(),
            ))
        };
        // 暂存候选来自哪一轮：澄清后创建或强化的记录，支持来源是原陈述，不是澄清消息。
        let mut staged_origin: BTreeMap<u64, Turn> = BTreeMap::new();
        let outcomes = |result: &Value| -> Vec<Value> {
            result["outcomes"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .chain(result.get("outcome").cloned())
                .collect()
        };
        for receipt in state.receipts.values() {
            let Some(turn) = request_turn(&receipt.request) else {
                continue;
            };
            // 澄清回执（请求带 staged_id）自身的 Pending 不是原陈述：只认最初暂存的回执。
            if receipt.request.get("staged_id").is_some() {
                continue;
            }
            for outcome in outcomes(&receipt.result) {
                if let Some(id) = outcome["Pending"]["staged_id"].as_u64() {
                    staged_origin.entry(id).or_insert_with(|| turn.clone());
                }
            }
        }
        for receipt in state.receipts.values() {
            let origin = match receipt.request.get("staged_id").and_then(Value::as_u64) {
                Some(staged) => staged_origin.get(&staged).cloned(),
                None => request_turn(&receipt.request),
            };
            let Some(origin) = origin else {
                continue;
            };
            for outcome in outcomes(&receipt.result) {
                let applied = &outcome["Ingested"]["Applied"];
                let Ok(id) = serde_json::from_value::<MemoryId>(applied["memory_id"].clone())
                else {
                    continue;
                };
                match applied["action"].as_str() {
                    Some("New" | "Supersede" | "Refine") => {
                        roles.support.entry(id).or_default().insert(origin.clone());
                        roles.created.entry(id).or_default().insert(origin.clone());
                    }
                    Some("Reinforce") => {
                        roles.support.entry(id).or_default().insert(origin.clone());
                        roles
                            .reinforce
                            .entry(id)
                            .or_default()
                            .insert(origin.clone());
                    }
                    _ => {}
                }
            }
        }
        // W5：只认已提交的回执（needs_rejudge 回执里的决定没有执行）。
        for receipt in state.w5_receipts.values() {
            if receipt.status != W5Status::Committed {
                continue;
            }
            let turns = candidate_turns(&receipt.fingerprint);
            for (candidate, id) in &receipt.products {
                if let Some(turn) = turns.get(candidate) {
                    roles.support.entry(*id).or_default().insert(turn.clone());
                    roles.created.entry(*id).or_default().insert(turn.clone());
                }
            }
            for decision in &receipt.decisions {
                if decision.action == W5Action::Reinforce
                    && decision.pending_reason.is_none()
                    && let (Some(target), Some(turn)) =
                        (decision.targets.first(), turns.get(&decision.candidate))
                {
                    roles
                        .support
                        .entry(*target)
                        .or_default()
                        .insert(turn.clone());
                    roles
                        .reinforce
                        .entry(*target)
                        .or_default()
                        .insert(turn.clone());
                }
            }
        }
        roles
    }
}

/// 记录谱系：原始目标的全部前身与全部继任。只沿 state_change 类替代走，冲突目标不算前身。
fn lineage(
    state: &PersistedMemoryState,
    memories: &[Memory],
    target: MemoryId,
) -> BTreeSet<MemoryId> {
    let committed: Vec<&W5Receipt> = state
        .w5_receipts
        .values()
        .filter(|r| r.status == W5Status::Committed)
        .collect();
    let replaced_by = |receipt: &W5Receipt, decision: &W5Decision| -> Option<MemoryId> {
        (decision.action == W5Action::StateChange && decision.pending_reason.is_none())
            .then(|| receipt.products.get(&decision.candidate).copied())
            .flatten()
    };
    let predecessors = |x: MemoryId| -> Vec<MemoryId> {
        let mut out: Vec<MemoryId> = memories
            .iter()
            .filter(|m| m.superseded_by == Some(x))
            .map(|m| m.id)
            .collect();
        out.extend(
            memories
                .iter()
                .filter(|m| m.id == x)
                .filter_map(|m| m.supersedes),
        );
        for receipt in &committed {
            for decision in &receipt.decisions {
                if replaced_by(receipt, decision) == Some(x) {
                    out.extend(decision.targets.iter().copied());
                }
            }
        }
        out
    };
    let successors = |x: MemoryId| -> Vec<MemoryId> {
        let mut out: Vec<MemoryId> = memories
            .iter()
            .filter(|m| m.supersedes == Some(x))
            .map(|m| m.id)
            .collect();
        out.extend(
            memories
                .iter()
                .filter(|m| m.id == x)
                .filter_map(|m| m.superseded_by),
        );
        for receipt in &committed {
            for decision in &receipt.decisions {
                if decision.targets.contains(&x)
                    && let Some(product) = replaced_by(receipt, decision)
                {
                    out.push(product);
                }
            }
        }
        out
    };
    let walk = |step: &dyn Fn(MemoryId) -> Vec<MemoryId>| {
        let mut found = BTreeSet::new();
        let mut queue = vec![target];
        while let Some(x) = queue.pop() {
            for next in step(x) {
                if next != target && found.insert(next) {
                    queue.push(next);
                }
            }
        }
        found
    };
    let mut all = walk(&predecessors);
    all.extend(walk(&successors));
    all.insert(target);
    all
}

/// 被删文字：被删原文的正文，以及要逐处替换的片段（正文、证据片段、关系依据）。
struct Deleted<'a> {
    bodies: &'a [String],
    fragments: &'a [String],
}

/// 关系或决定是否与本次删除有关：引用了被删原文（任意层的 `source_id`），或成员、目标里有
/// 被删记录。只有这类关系文字才按短 Quote 规则整体清洗，无关回执保持不变。
fn touches(value: &Value, sources: &BTreeSet<&str>, ids: &BTreeSet<MemoryId>) -> bool {
    match value {
        Value::Array(items) => items.iter().any(|item| touches(item, sources, ids)),
        Value::Object(map) => map.iter().any(|(key, item)| match key.as_str() {
            "source_id" => item.as_str().is_some_and(|id| sources.contains(id)),
            "members" | "targets" => item.as_array().into_iter().flatten().any(|m| {
                serde_json::from_value::<MemoryId>(m.clone()).is_ok_and(|m| ids.contains(&m))
            }),
            _ => touches(item, sources, ids),
        }),
        _ => false,
    }
}

/// Quote 的文字字段：内容只能是某条原文的连续片段，短到不含任何已知片段时也要整体清洗。
const QUOTE_TEXT_KEYS: &[&str] = &["text", "quote"];

/// 把字符串里出现的被删文字换成墓碑；对象的元数据字段不动。Quote 文字字段若是被删原文的
/// 子片段（例如条件里的“冬天”），整体换成墓碑。返回是否改动。
fn scrub(value: &mut Value, deleted: &Deleted) -> bool {
    scrub_at(value, deleted, false)
}

fn scrub_at(value: &mut Value, deleted: &Deleted, quote_text: bool) -> bool {
    match value {
        Value::String(text) => {
            if quote_text
                && text.as_str() != TOMBSTONE
                && !text.trim().is_empty()
                && deleted
                    .bodies
                    .iter()
                    .any(|body| body.contains(text.as_str()))
            {
                *text = TOMBSTONE.to_owned();
                return true;
            }
            let mut changed = false;
            for fragment in deleted.fragments {
                if text.contains(fragment.as_str()) {
                    *text = text.replace(fragment.as_str(), TOMBSTONE);
                    changed = true;
                }
            }
            changed
        }
        Value::Array(items) => items
            .iter_mut()
            .fold(false, |acc, item| scrub_at(item, deleted, quote_text) | acc),
        Value::Object(map) => map.iter_mut().fold(false, |acc, (key, item)| {
            if METADATA_KEYS.contains(&key.as_str()) {
                acc
            } else {
                let quote_text = QUOTE_TEXT_KEYS.contains(&key.as_str());
                scrub_at(item, deleted, quote_text) | acc
            }
        }),
        _ => false,
    }
}

/// 清洗一份 W5 回执的组指纹：组的原文被删时整体换成墓碑；否则逐层解析（组 JSON，以及其中
/// 作为字符串保存的记录、关系快照），清洗文字后重新序列化。被删记录的快照整体换成墓碑。
fn scrub_fingerprint(
    fingerprint: &mut String,
    deleted_turns: &BTreeSet<Turn>,
    deleted_ids: &BTreeSet<MemoryId>,
    deleted_sources: &BTreeSet<&str>,
    deleted: &Deleted,
) -> PersistenceResult<()> {
    let Ok(mut group) = serde_json::from_str::<Value>(fingerprint) else {
        return Ok(());
    };
    let own_deleted = group["candidates"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|c| c["candidate"]["evidence"].as_array().into_iter().flatten())
        .any(|e| {
            let turn = (
                e["session_id"].as_str().unwrap_or("").to_owned(),
                e["turn_id"].as_str().unwrap_or("").to_owned(),
            );
            deleted_turns.contains(&turn)
        });
    if own_deleted {
        *fingerprint = TOMBSTONE.to_owned();
        return Ok(());
    }
    // 短 Quote 整体清洗只用于与本次删除有关的关系文字（关系快照、决定）；记录快照、候选与
    // 无关关系只按片段替换，免得别的原文里恰好相同的短语被误清。
    let plain = Deleted {
        bodies: &[],
        fragments: deleted.fragments,
    };
    let mut changed = false;
    for key in ["seen", "seen_relations"] {
        for entry in group[key].as_array_mut().into_iter().flatten() {
            let deleted_record = key == "seen"
                && serde_json::from_value::<MemoryId>(entry["id"].clone())
                    .is_ok_and(|id| deleted_ids.contains(&id));
            let Some(inner) = entry["fingerprint"].as_str() else {
                continue;
            };
            if deleted_record {
                entry["fingerprint"] = json!(TOMBSTONE);
                changed = true;
                continue;
            }
            let Ok(mut snapshot) = serde_json::from_str::<Value>(inner) else {
                continue;
            };
            let rule =
                if key == "seen_relations" && touches(&snapshot, deleted_sources, deleted_ids) {
                    deleted
                } else {
                    &plain
                };
            if scrub(&mut snapshot, rule) {
                entry["fingerprint"] = json!(serde_json::to_string(&snapshot)?);
                changed = true;
            }
        }
    }
    changed |= scrub(&mut group["candidates"], &plain);
    for decision in group["decisions"].as_array_mut().into_iter().flatten() {
        let rule = if touches(decision, deleted_sources, deleted_ids) {
            deleted
        } else {
            &plain
        };
        changed |= scrub(decision, rule);
    }
    if changed {
        *fingerprint = serde_json::to_string(&group)?;
    }
    Ok(())
}

pub(crate) fn forget(
    state: &mut PersistedMemoryState,
    owner: &UserId,
    memory_id: Option<MemoryId>,
    staged_id: Option<u64>,
    source_id: Option<&str>,
) -> PersistenceResult<Value> {
    let memories = state.repository.list_by_owner(owner)?;
    let mut turns: BTreeSet<Turn> = BTreeSet::new();
    let mut targeted: BTreeSet<MemoryId> = BTreeSet::new();
    // 不靠原文记录也要清掉的文字（暂存候选、无原文旧记录的证据片段）。
    let mut sourceless_quotes: Vec<String> = Vec::new();
    if let Some(source_id) = source_id {
        let source = state
            .sources
            .values()
            .find(|s| s.id == source_id)
            .ok_or("source ID not found for this owner")?;
        turns.insert((source.session_id.clone(), source.turn_id.clone()));
    }
    if let Some(id) = staged_id {
        if let Some(staged) = state.staging.remove(StagedCandidateId(id)) {
            for evidence in &staged.candidate.evidence {
                turns.insert(turn_of(evidence));
                sourceless_quotes.push(evidence.quote.clone());
            }
        } else {
            return Ok(json!({"staged_id":id,"removed":false}));
        }
    }
    if let Some(id) = memory_id {
        if !memories.iter().any(|memory| memory.id == id) {
            return Err("memory ID not found for this owner".into());
        }
        targeted = lineage(state, &memories, id);
    }
    // 整条消息：谱系记录的原文整条删除（只认有原文记录的轮次）。没有原文记录的旧轮次（契约
    // §2.6 允许）不扩散到同轮其他记录，但被删记录的证据片段要从回执等处清掉。
    let has_source = |turn: &Turn| {
        state
            .sources
            .values()
            .any(|s| s.session_id == turn.0 && s.turn_id == turn.1)
    };
    for memory in memories.iter().filter(|m| targeted.contains(&m.id)) {
        for evidence in &memory.evidence {
            let turn = turn_of(evidence);
            if has_source(&turn) {
                turns.insert(turn);
            } else {
                sourceless_quotes.push(evidence.quote.clone());
            }
        }
    }
    // 派生记录：不再迭代；部分证据被删时按回执确认的独立支持决定去留。
    let roles = Roles::build(state);
    let mut deleted: BTreeSet<MemoryId> = targeted.clone();
    let mut retained: BTreeSet<MemoryId> = BTreeSet::new();
    for memory in &memories {
        if targeted.contains(&memory.id)
            || !memory.evidence.iter().any(|e| turns.contains(&turn_of(e)))
        {
            continue;
        }
        let support = roles.support.get(&memory.id);
        let supported = memory.status != MemoryStatus::Deleted
            && memory.evidence.iter().any(|e| {
                let turn = turn_of(e);
                !turns.contains(&turn) && support.is_some_and(|s| s.contains(&turn))
            });
        if supported {
            retained.insert(memory.id);
        } else {
            deleted.insert(memory.id);
        }
    }
    // 被删文字：原文正文、被删轮次的证据片段、出自被删原文的关系依据。
    let mut deleted_sources = Vec::new();
    let mut fragments: Vec<String> = Vec::new();
    for source in state
        .sources
        .values_mut()
        .filter(|s| turns.contains(&(s.session_id.clone(), s.turn_id.clone())))
    {
        if let Some(text) = source.request.as_ref().and_then(|r| r["text"].as_str()) {
            fragments.push(text.to_owned());
        }
        source.request = None;
        // Retain only the accepted mode; deletion removes business results and source text.
        source.result = source
            .result
            .as_ref()
            .filter(|result| result["enrichment_mode"] == "off")
            .map(|_| json!({"enrichment_mode":"off"}));
        source.error = Some("source_deleted".into());
        source.enrichment = "deleted".into();
        deleted_sources.push(source.id.clone());
    }
    // 整段删除的文字：原文正文与无原文路径的片段。其他请求里的历史副本含有它们就整条去掉。
    let removed_bodies = fragments.clone();
    sourceless_quotes.retain(|q| !q.trim().is_empty() && q != TOMBSTONE);
    let removed_copies: Vec<String> = removed_bodies
        .iter()
        .chain(sourceless_quotes.iter())
        .cloned()
        .collect();
    fragments.extend(sourceless_quotes);
    for memory in &memories {
        for evidence in &memory.evidence {
            if turns.contains(&turn_of(evidence)) && evidence.quote != TOMBSTONE {
                fragments.push(evidence.quote.clone());
            }
        }
    }
    let deleted_source_ids: BTreeSet<&str> = deleted_sources.iter().map(String::as_str).collect();
    for relation in state.relations.values() {
        for evidence in relation.basis.iter().chain(relation.resolution.iter()) {
            if deleted_source_ids.contains(evidence.source_id.as_str()) {
                fragments.push(evidence.quote.clone());
            }
        }
    }
    fragments.retain(|f| !f.trim().is_empty() && f != TOMBSTONE);
    fragments.sort_by_key(|f| std::cmp::Reverse(f.len()));
    fragments.dedup();
    let scrubbed = Deleted {
        bodies: &removed_bodies,
        fragments: &fragments,
    };

    if !deleted_sources.is_empty() {
        state.receipts.retain(|_, receipt| {
            !turns.contains(&(
                receipt.request["session_id"].as_str().unwrap_or("").into(),
                receipt.request["turn_id"].as_str().unwrap_or("").into(),
            ))
        });
    }
    // Remove verbatim history copies too; affected old retry payloads now conflict rather than resurrect data.
    if !removed_copies.is_empty() {
        for request in state
            .sources
            .values_mut()
            .filter_map(|s| s.request.as_mut())
            .chain(state.receipts.values_mut().map(|r| &mut r.request))
        {
            if let Some(history) = request["history"].as_array_mut() {
                history.retain(|v| {
                    !v.as_str().is_some_and(|text| {
                        removed_copies.iter().any(|removed| text.contains(removed))
                    })
                });
            }
        }
    }
    // 留下的旧回执里不能再有被删文字（无原文旧记录的消息只存在这里）。重放会因请求不符而失败关闭。
    if !fragments.is_empty() {
        let plain = Deleted {
            bodies: &[],
            fragments: &fragments,
        };
        for receipt in state.receipts.values_mut() {
            scrub(&mut receipt.request, &plain);
            scrub(&mut receipt.result, &plain);
        }
    }
    let pending: Vec<_> = state
        .staging
        .list_by_owner(owner)
        .into_iter()
        .filter(|s| {
            s.candidate
                .evidence
                .iter()
                .any(|e| turns.contains(&turn_of(e)))
        })
        .map(|s| s.id)
        .collect();
    for id in pending {
        state.staging.remove(id);
    }
    // 各原文的得知时间：原文记录优先；无原文的旧轮次取旧回执请求里的显式 observed_at。
    let mut observed: BTreeMap<Turn, i64> = state
        .receipts
        .values()
        .filter_map(|r| {
            Some((
                (
                    r.request["session_id"].as_str()?.to_owned(),
                    r.request["turn_id"].as_str()?.to_owned(),
                ),
                r.request["observed_at"].as_i64()?,
            ))
        })
        .collect();
    observed.extend(
        state
            .sources
            .values()
            .map(|s| ((s.session_id.clone(), s.turn_id.clone()), s.observed_at)),
    );
    for mut memory in memories
        .into_iter()
        .filter(|m| deleted.contains(&m.id) || retained.contains(&m.id))
    {
        let forgotten = deleted.contains(&memory.id);
        for evidence in &mut memory.evidence {
            if forgotten || turns.contains(&turn_of(evidence)) {
                evidence.quote = TOMBSTONE.into();
            }
        }
        if forgotten {
            memory.status = MemoryStatus::Deleted;
            // 值、条件、时间表达、标签都可能是被删原文：只留 ID、类型、谓词、状态、谱系、
            // 得知时间与墓碑证据供审计。
            memory.value = MemoryValue::Text(TOMBSTONE.into());
            memory.scope = Scope::default();
            memory.temporal = Temporal::observed_at(memory.temporal.observed_at);
            memory.tags.clear();
            state.record_updates.remove(&memory.id);
        } else {
            // 按剩余的独立强化来源重算（按原文去重，不按回执数量）。
            let reinforcing: Vec<&Turn> = roles
                .reinforce
                .get(&memory.id)
                .into_iter()
                .flatten()
                .filter(|turn| !turns.contains(*turn))
                .filter(|turn| {
                    !roles
                        .created
                        .get(&memory.id)
                        .is_some_and(|created| created.contains(*turn))
                })
                .filter(|turn| memory.evidence.iter().any(|e| turn_of(e) == **turn))
                .collect();
            memory.reinforcement_count = reinforcing.len().try_into()?;
            memory.last_reinforced_at = reinforcing
                .iter()
                .filter_map(|turn| observed.get(*turn))
                .max()
                .map(|at| Timestamp(*at));
        }
        state.repository.update(memory)?;
    }
    // 关系：成员、依据或解决依据的原文被删即失效；出自被删原文的文字打墓碑。
    let mut invalidated = Vec::new();
    for relation in state.relations.values_mut() {
        let cites_deleted = relation
            .basis
            .iter()
            .chain(relation.resolution.iter())
            .any(|e| deleted_source_ids.contains(e.source_id.as_str()));
        let member_deleted = relation.members.iter().any(|m| deleted.contains(m));
        if !(cites_deleted || member_deleted) {
            continue;
        }
        if relation.status != RelationStatus::Invalidated {
            relation.status = RelationStatus::Invalidated;
            relation.closed_by = None;
            invalidated.push(relation.id.clone());
        }
        for evidence in relation
            .basis
            .iter_mut()
            .chain(relation.resolution.iter_mut())
        {
            if deleted_source_ids.contains(evidence.source_id.as_str()) {
                evidence.quote = TOMBSTONE.into();
            }
        }
        scrub(&mut relation.conditions, &scrubbed);
        scrub(&mut relation.times, &scrubbed);
    }
    // W5 回执：组指纹与决定里的关系文字。
    if !fragments.is_empty() || !deleted.is_empty() {
        for receipt in state.w5_receipts.values_mut() {
            scrub_fingerprint(
                &mut receipt.fingerprint,
                &turns,
                &deleted,
                &deleted_source_ids,
                &scrubbed,
            )?;
            for decision in &mut receipt.decisions {
                let mut cites_deleted = false;
                for evidence in decision
                    .relation
                    .iter_mut()
                    .flat_map(|spec| spec.basis.iter_mut())
                    .chain(decision.resolution.iter_mut())
                {
                    if deleted_source_ids.contains(evidence.source_id.as_str()) {
                        evidence.quote = TOMBSTONE.into();
                        cites_deleted = true;
                    }
                }
                if let Some(spec) = decision.relation.as_mut()
                    && cites_deleted
                {
                    scrub(&mut spec.conditions, &scrubbed);
                    scrub(&mut spec.times, &scrubbed);
                }
            }
        }
    }
    Ok(
        json!({"forgotten_ids":deleted,"retained_ids":retained,"invalidated_relation_ids":invalidated,
        "deleted_source_ids":deleted_sources,"staged_id":staged_id,
        "removed":true,"deletion":"soft","audit_evidence_retained":true,
        "note":"Source bodies and linked receipts removed from the current snapshot; tombstones block replay. Forgotten memories keep their IDs, status, lineage and tombstoned evidence for audit, not their content. Existing diagnostic files, backups and WAL are not physically erased."}),
    )
}
