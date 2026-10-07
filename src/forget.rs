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

const W4_ENUM_KEYS: &[&str] = &[
    "verdict",
    "episode_value",
    "aspect",
    "speech",
    "polarity",
    "strength",
    "role",
    "unit",
    "anchor",
    "precision",
    "object",
    "action",
    "pending_reason",
    "of",
];

fn turn_of(evidence: &Evidence) -> Turn {
    (evidence.session_id.0.clone(), evidence.turn_id.0.clone())
}

/// 已提交的 W5 回执里，每个候选的原文轮次（只认证据全部来自同一轮的候选；多源候选角色不明）。
pub(crate) fn candidate_turns(fingerprint: &str) -> BTreeMap<usize, Turn> {
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

/// 已提交 W5 回执的组指纹里：每个候选的证据，以及组快照里每条记录提交前已有的证据。
fn group_evidence(
    fingerprint: &str,
) -> (
    BTreeMap<usize, Vec<Evidence>>,
    BTreeMap<MemoryId, Vec<Evidence>>,
) {
    let Ok(group) = serde_json::from_str::<Value>(fingerprint) else {
        return (BTreeMap::new(), BTreeMap::new());
    };
    let evidence = |value: &Value| -> Option<Vec<Evidence>> {
        serde_json::from_value(value["evidence"].clone()).ok()
    };
    let candidates = group["candidates"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| Some((c["index"].as_u64()? as usize, evidence(&c["candidate"])?)))
        .collect();
    let seen = group["seen"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let id = serde_json::from_value::<MemoryId>(entry["id"].clone()).ok()?;
            let record = serde_json::from_str::<Value>(entry["fingerprint"].as_str()?).ok()?;
            Some((id, evidence(&record)?))
        })
        .collect();
    (candidates, seen)
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
            let (candidate_evidence, seen) = group_evidence(&receipt.fingerprint);
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
                    // 提交时是否实际计了强化：组快照里的目标已带有该候选的证据（同 ID 或同一轮，
                    // 与执行时的判断相同；例如经归并转入）时，执行按来源幂等没有计数，重算也不能
                    // 补算。没有快照的旧回执照旧认定。
                    let counted = match (
                        seen.get(target),
                        candidate_evidence.get(&decision.candidate),
                    ) {
                        (Some(present), Some(evidence)) => evidence
                            .iter()
                            .any(|e| !crate::commit::evidence_present(present, e)),
                        _ => true,
                    };
                    if counted {
                        roles
                            .reinforce
                            .entry(*target)
                            .or_default()
                            .insert(turn.clone());
                    }
                }
            }
        }
        // 归并（已提交记录 → 保留目标）：被归并记录的确认支持来源一并转给保留目标，只转支持，
        // 不转强化。归并本身是回执里的一条 reinforce 决定，上面已按快照判断它是否计数。归并
        // 可能成链，反复转移直到不再变化。
        let merges: Vec<(MemoryId, MemoryId)> = state
            .w5_receipts
            .values()
            .filter(|r| r.status == W5Status::Committed)
            .flat_map(|r| r.merged.iter().map(|(from, into)| (*from, *into)))
            .collect();
        loop {
            let mut changed = false;
            for (from, into) in &merges {
                let support = roles.support.get(from).cloned().unwrap_or_default();
                let entry = roles.support.entry(*into).or_default();
                for turn in support {
                    changed |= entry.insert(turn);
                }
            }
            if !changed {
                break;
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
    scrub_at(value, deleted, false, &[])
}

fn scrub_at(value: &mut Value, deleted: &Deleted, quote_text: bool, enum_keys: &[&str]) -> bool {
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
        Value::Array(items) => items.iter_mut().fold(false, |acc, item| {
            scrub_at(item, deleted, quote_text, enum_keys) | acc
        }),
        Value::Object(map) => map.iter_mut().fold(false, |acc, (key, item)| {
            if METADATA_KEYS.contains(&key.as_str()) || enum_keys.contains(&key.as_str()) {
                acc
            } else {
                let quote_text = QUOTE_TEXT_KEYS.contains(&key.as_str());
                scrub_at(item, deleted, quote_text, enum_keys) | acc
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
    // Import provenance is a hard dependency: deleting a parent deletes all descendants.
    // Never follow ordinary factual evidence to unrelated independent sources.
    loop {
        let ids: BTreeSet<_> = state
            .sources
            .values()
            .filter(|s| turns.contains(&(s.session_id.clone(), s.turn_id.clone())))
            .map(|s| s.id.as_str())
            .collect();
        let descendants: Vec<_> = state
            .sources
            .values()
            .filter(|s| {
                s.management.provenance.as_ref().is_some_and(|p| {
                    p["kind"] == "history_import"
                        && p["parent_source_id"]
                            .as_str()
                            .is_some_and(|id| ids.contains(id))
                })
            })
            .map(|s| (s.session_id.clone(), s.turn_id.clone()))
            .collect();
        let before = turns.len();
        turns.extend(descendants);
        if turns.len() == before {
            break;
        }
    }
    // Identity confirmations are dependencies, not independent factual support. Deleting one
    // invalidates only the selected candidate's products; it never deletes its other raw source.
    let deleting_sources: BTreeSet<String> = state
        .sources
        .values()
        .filter(|s| turns.contains(&(s.session_id.clone(), s.turn_id.clone())))
        .map(|s| s.id.clone())
        .collect();
    let dependent_confirmations: BTreeMap<String, Vec<crate::sqlite::SubjectConfirmation>> = state.sources.iter()
        .filter_map(|(key, source)| {
            let affected: Vec<_> = source.management.confirmations.iter().filter(|c| matches!(
                &c.evidence.source, crate::contract::SourceRef::Stored { source_id } if deleting_sources.contains(source_id)
            )).cloned().collect();
            (!affected.is_empty() && !deleting_sources.contains(&source.id)).then(|| (key.clone(), affected))
        }).collect();
    for (key, confirmations) in &dependent_confirmations {
        for receipt in state
            .w5_receipts
            .iter()
            .filter(|(id, r)| id.starts_with(&format!("{key}#")) && r.status == W5Status::Committed)
            .map(|(_, r)| r)
        {
            for confirmation in confirmations {
                if let Some(id) = receipt.products.get(&confirmation.candidate_index) {
                    targeted.insert(*id);
                }
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
        // Retain only parent/hash/slot identity; request IDs can contain text, so drop authorization too.
        let import = source
            .management
            .provenance
            .take()
            .filter(|p| p["kind"] == "history_import")
            .map(|p| {
                json!({"kind":"history_import","parent_source_id":p["parent_source_id"],
                "history_index":p["history_index"],"content_fingerprint":p["content_fingerprint"],
                "before_source_id":p["before_source_id"]})
            });
        source.management = crate::sqlite::SourceManagement {
            provenance: import,
            ..Default::default()
        };
        // Retain only the accepted mode; deletion removes business results and source text.
        source.result = source
            .result
            .as_ref()
            .filter(|result| result["enrichment_mode"] == "off")
            .map(|_| json!({"enrichment_mode":"off"}));
        source.error = Some("source_deleted".into());
        source.enrichment = "deleted".into();
        source.task.attempt_id = None;
        source.task.lease_until = None;
        source.task.claim_revision = None;
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
    crate::recall_models::invalidate_deleted(state, &deleted_source_ids);
    let affected_source_ids: BTreeSet<_> = dependent_confirmations
        .keys()
        .map(|key| state.sources[key].id.clone())
        .collect();
    let affected_sources = affected_source_ids.iter().map(String::as_str).collect();
    crate::recall_models::invalidate_deleted(state, &affected_sources);
    for receipt in state.receipts.values_mut() {
        if receipt.request.get("digest").is_some()
            && receipt.request["source_id"]
                .as_str()
                .is_some_and(|id| deleted_source_ids.contains(id) || affected_sources.contains(id))
        {
            receipt.result = json!({"error":"source_changed"});
        }
    }
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
    // 保留的记录不能带着被删原文的文字：文本值出自被删原文（例如通用 fact 的值就是那句原话），
    // 而剩余证据里没有这段文字时，这条记录没有可保留的内容，一并删除（剩余原文照常可检索）。
    let unsupported_text: Vec<MemoryId> = memories
        .iter()
        .filter(|m| retained.contains(&m.id))
        .filter(|memory| {
            let MemoryValue::Text(value) = &memory.value else {
                return false;
            };
            let from_deleted = removed_bodies
                .iter()
                .any(|body| body.contains(value.as_str()))
                || fragments
                    .iter()
                    .any(|fragment| value.contains(fragment.as_str()));
            let still_said = memory
                .evidence
                .iter()
                .filter(|e| !turns.contains(&turn_of(e)))
                .any(|e| e.quote.contains(value.as_str()));
            from_deleted && !still_said
        })
        .map(|memory| memory.id)
        .collect();
    for id in unsupported_text {
        retained.remove(&id);
        deleted.insert(id);
    }

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
            // 组自己的原文被删：指纹已整体墓碑化，决定里取自该原文的生效时间原文一并清掉。
            let own_deleted = receipt.fingerprint == TOMBSTONE;
            for decision in &mut receipt.decisions {
                for evidence in &mut decision.basis {
                    if let crate::contract::SourceRef::Stored { source_id } = &evidence.source
                        && deleted_source_ids.contains(source_id.as_str())
                    {
                        evidence.quote.text = TOMBSTONE.into();
                    }
                }
                if own_deleted && let Some(time) = decision.effective_time.as_mut() {
                    time.interp.quote.text = TOMBSTONE.into();
                    if let crate::contract::Interp::Anchored { anchor_quote, .. } =
                        &mut time.interp.interp
                    {
                        anchor_quote.text = TOMBSTONE.into();
                    }
                }
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
    // Checkpoints are raw-content copies too, including JSON-encoded W5 snapshots.
    for (key, source) in &mut state.sources {
        source.management.audits.retain_mut(|audit| {
            !touches(audit, &deleted_source_ids, &deleted)
                && !scrub_at(audit, &scrubbed, false, W4_ENUM_KEYS)
        });
        let confirmation_affected = dependent_confirmations.get(key);
        if let Some(confirmations) = confirmation_affected {
            source.management.confirmations.retain(|c| {
                !confirmations
                    .iter()
                    .any(|lost| lost.candidate_index == c.candidate_index)
            });
            if let Some(batch) = source.result.as_mut().and_then(|r| r.get_mut("w4_batch")) {
                for confirmation in confirmations {
                    if let Some(candidate) =
                        batch["batch"]["w2"]["candidates"].get_mut(confirmation.candidate_index)
                    {
                        candidate["candidate"]["subject"] =
                            serde_json::to_value(&confirmation.original_subject)?;
                        candidate["status"] = json!("unresolved");
                        candidate["reasons"] = json!(["unresolved_subject"]);
                    }
                }
                batch["batch"]["review"] = Value::Null;
            }
        }
        let mut batch_affected = false;
        if let Some(result) = source.result.as_mut() {
            for key in ["w4_batch", "w4_controls", "w4_model_stages"] {
                if let Some(value) = result.get_mut(key) {
                    let w4_before = (key == "w4_model_stages").then(|| value["w4"].clone());
                    let touched = touches(value, &deleted_source_ids, &deleted);
                    // 新批次的固定枚举不是原文；不能把被删的英文 supported 当作 verdict 清掉。
                    // 自由文本（包括未通过 W2 的 relation/memory_type）仍照常清洗；旧路径用空列表。
                    let scrubbed_stage = scrub_at(value, &scrubbed, false, W4_ENUM_KEYS);
                    batch_affected |= touched || scrubbed_stage;
                    if let Some(before) = w4_before
                        && (touches(&before, &deleted_source_ids, &deleted)
                            || before != value["w4"])
                        && let Some(output) = value.get_mut("w4").and_then(|s| s.get_mut("output"))
                        && output.get("raw_output").is_some()
                    {
                        // 原始引用可含空白变体或无效 JSON，不能靠逐字替换清洗。
                        output["raw_output"] = Value::Null;
                        if output["valid"] == false {
                            output["final_output"] = Value::Null;
                        }
                        output["validation_error"] = Value::Null;
                    }
                }
            }
        }
        let Some(checkpoint) = source
            .result
            .as_mut()
            .and_then(|r| r.get_mut("w4_checkpoint"))
        else {
            continue;
        };
        let mut affected = confirmation_affected.is_some()
            || batch_affected
            || touches(checkpoint, &deleted_source_ids, &deleted);
        for group in checkpoint
            .get_mut("plan")
            .and_then(|p| p.get_mut("groups"))
            .and_then(Value::as_array_mut)
            .into_iter()
            .flatten()
        {
            let before = serde_json::to_string(group)?;
            let mut fingerprint = before.clone();
            scrub_fingerprint(
                &mut fingerprint,
                &turns,
                &deleted,
                &deleted_source_ids,
                &scrubbed,
            )?;
            if fingerprint != before {
                affected = true;
            }
        }
        affected |= scrub(checkpoint, &scrubbed);
        if affected {
            // No executable old plan survives privacy invalidation. Historical group IDs remain in receipts.
            checkpoint["plan"] = Value::Null;
            checkpoint["invalidated"] = json!("source_deleted");
            source.task.attempt_id = None;
            source.task.lease_until = None;
            source.task.claim_revision = None;
            source.enrichment = "needs_rejudge".into();
            source.error = Some("checkpoint_invalidated_by_deletion".into());
        }
    }
    Ok(
        json!({"forgotten_ids":deleted,"retained_ids":retained,"invalidated_relation_ids":invalidated,
        "deleted_source_ids":deleted_sources,"staged_id":staged_id,
        "removed":true,"deletion":"soft","audit_evidence_retained":true,
        "note":"Source bodies and linked receipts removed from the current snapshot; tombstones block replay. Forgotten memories keep their IDs, status, lineage and tombstoned evidence for audit, not their content. Existing diagnostic files, backups and WAL are not physically erased."}),
    )
}
