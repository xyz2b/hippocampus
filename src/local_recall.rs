//! Bounded local retrieval. Raw statements remain evidence, never executable instructions.
use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{Connection, OptionalExtension};
use serde_json::{Value, json};

use crate::*;

// ponytail: surface noun heads can miss paraphrases; source-linked role parsing is needed
// before these checks can certify arbitrary natural-language references.
fn referential_heads(query: &str) -> Vec<String> {
    let chars: Vec<_> = query.chars().collect();
    chars
        .windows(3)
        .enumerate()
        .filter_map(|(index, span)| {
            if !matches!(span[0], '这' | '那')
                || matches!(span[1], '他' | '她' | '它')
                || !span[2].is_alphabetic()
            {
                return None;
            }
            let mut head = span[2].to_string();
            if let Some(next) = chars.get(index + 3)
                && next.is_alphabetic()
                && !"是为在有会能要可该把被与和".contains(*next)
            {
                head.push(*next);
            }
            Some(head)
        })
        .collect()
}

fn context_binding_phrase<'a>(context: &'a str, head: &str) -> Option<&'a str> {
    ["继续说", "继续聊", "关于", "指的是", "说的是"]
        .iter()
        .find_map(|marker| context.rsplit_once(marker).map(|(_, tail)| tail))
        .map(|tail| {
            tail.trim()
                .trim_end_matches(['。', '！', '？', '.', '!', '?'])
        })
        .filter(|phrase| {
            phrase.contains(head)
                && phrase.chars().count() <= 40
                && (phrase.matches(head).count() == 1
                    || phrase.contains(['这', '那'])
                        && !phrase.contains(['和', '与', '、', '，', ',']))
        })
}

fn unbound_reference(
    query: &str,
    context: &str,
    query_hints: &BTreeMap<EntityId, Vec<String>>,
    context_hints: &BTreeMap<EntityId, Vec<String>>,
    entities: &[Entity],
) -> bool {
    let heads = referential_heads(query);
    let nominal_unbound = heads
        .iter()
        .any(|head| context_binding_phrase(context, head).is_none());
    let pronoun = query.replace("其他", "");
    let person_pronoun = pronoun.contains(['他', '她']);
    let thing_pronoun = pronoun.contains('它');
    let pronoun_unbound = (person_pronoun || thing_pronoun)
        && (context_hints.len() != 1
            || person_pronoun
                && !context_hints.keys().any(|id| {
                    entities
                        .iter()
                        .any(|entity| entity.id == *id && entity.entity_type == EntityType::Person)
                })
            || thing_pronoun
                && !context_hints.keys().any(|id| {
                    entities
                        .iter()
                        .any(|entity| entity.id == *id && entity.entity_type != EntityType::Person)
                }));
    let modifier_unbound = query.split_once('的').is_some_and(|(prefix, rest)| {
        let prefix = prefix.trim_start_matches("请问");
        let head = heads
            .first()
            .cloned()
            .unwrap_or_else(|| rest.chars().take(2).collect());
        ["先", "上次", "之前", "刚才", "当时", "曾经"]
            .iter()
            .any(|word| prefix.starts_with(word))
            && query_hints.is_empty()
            && context_binding_phrase(context, &head).is_none()
    });
    nominal_unbound || pronoun_unbound || modifier_unbound
}

/// A question about the asker: 我… in Chinese, or an English first-person word.
pub(crate) fn first_person(query: &str) -> bool {
    query.trim_start_matches("请问").starts_with('我')
        || query
            .split(|c: char| !c.is_ascii_alphabetic())
            .any(|word| ["i", "my", "me", "mine", "myself"].contains(&word.to_lowercase().as_str()))
}

fn literal_phrase(text: &str, phrase: &str) -> bool {
    let words = format!(" {} ", crate::lexical::tokens(text).join(" "));
    let phrase = crate::lexical::tokens(phrase).join(" ");
    !phrase.is_empty() && words.contains(&format!(" {phrase} "))
}

fn direct_property_assertion(text: &str, subject_names: &[&str], property: &str) -> bool {
    if text.contains(['？', '?']) {
        return false;
    }
    text.split(['。', '；', ';', '，', ',']).any(|clause| {
        let clause = strip_assertion_prefix(clause.trim());
        subject_names.iter().any(|subject| {
            let clause = clause.strip_prefix("项目").unwrap_or(clause);
            [
                format!("{subject}的{property}"),
                format!("{subject}{property}"),
            ]
            .iter()
            .any(|prefix| {
                clause.strip_prefix(prefix).is_some_and(|tail| {
                    tail.chars()
                        .next()
                        .is_some_and(|next| "是为有在：:".contains(next))
                        && ![
                            "多少",
                            "什么",
                            "谁",
                            "哪",
                            "是否",
                            "是不是",
                            "有无",
                            "有没有",
                            "在不在",
                            "还是",
                        ]
                        .iter()
                        .any(|word| tail.contains(word))
                        && !tail.ends_with('吗')
                })
            })
        })
    })
}

fn technical_choice_question(query: &str, subject_names: &[&str]) -> bool {
    let query = query
        .trim_start_matches("请问")
        .trim_end_matches(['？', '?']);
    let named = query.strip_prefix("项目").unwrap_or(query);
    let rest = subject_names
        .iter()
        .find_map(|name| named.strip_prefix(name))
        .or_else(|| {
            ["它", "他", "她"]
                .iter()
                .find_map(|name| query.strip_prefix(name))
        })
        .unwrap_or(query);
    let rest = rest.trim_start_matches('的');
    let rest = ["现在", "目前", "当前", "最新"]
        .iter()
        .find_map(|word| rest.strip_prefix(word))
        .unwrap_or(rest)
        .trim_start_matches('的');
    if matches!(answer_slot_stem(rest), Some("技术选择" | "数据库选择")) {
        return true;
    }
    if matches!(rest, "数据库选用什么" | "技术选用什么") {
        return true;
    }
    ["选用", "采用", "选择"]
        .iter()
        .filter_map(|verb| rest.strip_prefix(verb))
        .map(|tail| tail.strip_prefix('了').unwrap_or(tail))
        .any(|tail| {
            ["什么", "哪种", "哪个", "哪款"]
                .iter()
                .filter_map(|word| tail.strip_prefix(word))
                .any(|object| matches!(object, "" | "数据库" | "技术"))
        })
}

fn direct_technical_choice_assertion(quote: &str, subject_names: &[&str], memory: &Memory) -> bool {
    if !matches!(memory.predicate.0.as_str(), "技术选择" | "数据库选择") {
        return false;
    }
    quote.split(['。', '；', ';', '，', ',']).any(|clause| {
        ["选用", "采用", "选择", "决定使用", "决定用"]
            .iter()
            .any(|relation| {
                linked_comparison_assertion(
                    clause.trim(),
                    subject_names,
                    &memory.predicate.0,
                    relation,
                    memory,
                )
            })
    })
}

fn single_relation_family(query: &str, predicate: &str, evidence: &str) -> bool {
    let deferred = ["搁置", "暂缓", "推迟", "延后"]
        .iter()
        .any(|word| query.contains(word))
        && ["暂缓", "搁置", "推迟", "延后"]
            .iter()
            .any(|word| predicate.contains(word))
        && !["原因", "时间", "状态", "预算"]
            .iter()
            .any(|word| predicate.contains(word))
        && [
            "暂不引入",
            "暂时不引入",
            "先不引入",
            "暂缓引入",
            "推迟引入",
            "搁置",
        ]
        .contains(&evidence);
    deferred || query == predicate && evidence == predicate
}

pub(crate) fn single_relation_identity_parts(query: &str) -> Option<(&str, &str)> {
    let stem = query.trim_end_matches(['？', '?']);
    let (modifier, answer_role) = stem.split_once('的')?;
    let noun = answer_role
        .strip_suffix("叫什么")
        .or_else(|| answer_role.strip_suffix("是什么"))
        .or_else(|| answer_role.strip_prefix("是哪种"))
        .or_else(|| answer_role.strip_prefix("是哪个"))?;
    let negated_relation = ["搁置", "暂缓", "推迟", "延后"].iter().any(|relation| {
        modifier.match_indices(relation).any(|(at, _)| {
            let prefix = modifier[..at].trim_end();
            ["没", "未", "不", "不再"]
                .iter()
                .any(|negation| prefix.ends_with(negation))
        })
    });
    let revised_relation = ["取消", "撤回", "撤销", "改为", "改成", "后来"]
        .iter()
        .any(|word| modifier.contains(word));
    if modifier.is_empty()
        || noun.is_empty()
        || negated_relation
        || revised_relation
        || [
            "为什么",
            "什么时候",
            "多少",
            "几时",
            "何时",
            "怎么",
            "如何",
            "原因",
            "理由",
            "缘由",
            "目的",
            "时间",
            "日期",
            "时刻",
            "预算",
            "成本",
            "费用",
            "价格",
            "数量",
            "状态",
            "限制",
            "方式",
            "方法",
            "地点",
            "位置",
            "地址",
            "进度",
            "时长",
            "结果",
            "概率",
            "负责人",
            "不是",
            "并非",
            "除外",
            "以外",
            "不包括",
            "除了",
            "没有",
            "排除",
            "排除掉",
            "分别",
            "各自",
        ]
        .iter()
        .any(|word| query.contains(word))
        || ["和", "与", "及", "、"]
            .iter()
            .any(|word| answer_role.contains(word))
    {
        return None;
    }
    Some((modifier, answer_role))
}

fn single_relation_answer_role_matches(answer_role: &str, predicate: &str) -> bool {
    let allowed: &[&str] = if predicate.contains("技术") {
        &["技术", "缓存", "服务", "软件", "工具", "系统", "组件"]
    } else if predicate.contains("维护") {
        &["组件", "配件", "耗材", "部件", "设备"]
    } else if predicate.contains("事项") || predicate.contains("计划") {
        &["事项", "计划", "项目", "事情", "活动"]
    } else {
        return answer_role.contains(predicate);
    };
    allowed.iter().any(|noun| answer_role.contains(noun))
}

fn direct_single_relation_assertion(
    quote: &str,
    entities: &[Entity],
    subject: &EntityId,
    relation: &str,
    value: &str,
) -> bool {
    if relation.is_empty()
        || value.is_empty()
        || quote.contains(['？', '?'])
        || quote.matches(relation).count() != 1
        || quote.matches(value).count() != 1
        || [
            "如果", "假如", "可能", "也许", "计划", "考虑", "并非", "不是", "没有", "取消", "撤回",
            "不再", "无需", "不必", "并未", "并不", "未曾", "曾经", "以前", "过去", "当时", "原本",
        ]
        .iter()
        .any(|word| quote.contains(word))
    {
        return false;
    }
    let (Some(relation_start), Some(value_start)) = (quote.find(relation), quote.find(value))
    else {
        return false;
    };
    let relation_end = relation_start + relation.len();
    let value_end = value_start + value.len();
    let direct = if relation_end <= value_start {
        quote[relation_end..value_start].trim().is_empty()
    } else if value_end <= relation_start {
        quote[value_end..relation_start].trim().is_empty()
    } else {
        false
    };
    if !direct
        || quote[relation_start.min(value_start)..relation_end.max(value_end)]
            .contains(['，', ',', '；', ';', '。', '.', '！', '!'])
    {
        return false;
    }
    let phrase_start = relation_start.min(value_start);
    let phrase_end = relation_end.max(value_end);
    let sentence_start = quote[..phrase_start]
        .rfind(['。', '.', '！', '!', '？', '?'])
        .map_or(0, |at| at + quote[at..].chars().next().unwrap().len_utf8());
    let clause_start = quote[..phrase_start]
        .rfind(['，', ',', '；', ';', '。', '.', '！', '!', '？', '?'])
        .map_or(0, |at| at + quote[at..].chars().next().unwrap().len_utf8());
    let clause_end = quote[phrase_end..]
        .find(['，', ',', '；', ';', '。', '.', '！', '!', '？', '?'])
        .map_or(quote.len(), |offset| phrase_end + offset);
    if !quote[phrase_end..clause_end].trim().is_empty() {
        return false;
    }
    let last_subject = entities
        .iter()
        .flat_map(|entity| {
            std::iter::once(entity.canonical_name.as_str())
                .chain(entity.aliases.iter().map(String::as_str))
                .filter(move |name| !name.is_empty() && *name != "我")
                .flat_map(move |name| {
                    quote[..relation_start.max(value_start)]
                        .match_indices(name)
                        .filter(move |(at, _)| *at >= sentence_start)
                        .map(move |(at, _)| (at, at + name.len(), &entity.id))
                })
        })
        .max_by_key(|(at, _, _)| *at);
    let Some((at, end, id)) = last_subject else {
        return false;
    };
    if id != subject {
        return false;
    }
    // A comma may carry forward the subject only when the next clause starts
    // directly with the asserted value/relation. Any prefix could be a new,
    // unregistered subject and must not inherit the previous one.
    let prefix = quote[clause_start..phrase_start].trim();
    if at < clause_start {
        prefix.is_empty()
    } else {
        let between_subject_and_assertion = quote[end..phrase_start].trim();
        between_subject_and_assertion.is_empty() || between_subject_and_assertion == "的"
    }
}

// Both literal and model-proposed comparison roles need a direct, source-linked assertion.
// The committed atomic relation, its value, and the exact USER clause must all agree.
fn linked_comparison_assertion(
    quote: &str,
    subject_names: &[&str],
    property: &str,
    evidence_relation: &str,
    memory: &Memory,
) -> bool {
    let MemoryValue::Text(value) = &memory.value else {
        return false;
    };
    if memory.predicate.0 != property
        || value.is_empty()
        || evidence_relation.chars().count() > 200
        || !quote.contains(evidence_relation)
        || quote.contains(['？', '?'])
    {
        return false;
    }
    // A model may copy "relation + value"; only a known relation prefix may be checked.
    let known_prefix = |form: &str| {
        evidence_relation == form
            || evidence_relation
                .strip_prefix(form)
                .is_some_and(|rest| rest.contains(value.as_str()))
    };
    let evidence_relation = if known_prefix(property) {
        Some(property)
    } else if property.ends_with("选择") {
        ["选用", "采用", "选择", "决定使用", "决定用"]
            .into_iter()
            .filter(|form| known_prefix(form))
            .max_by_key(|form| form.len())
    } else if property.contains("限制") {
        [
            "只能",
            "只能在",
            "仅能",
            "仅能在",
            "必须",
            "必须在",
            "不得",
            "不得在",
            "至多",
            "不超过",
            "限于",
            "禁止",
            "禁止在",
        ]
        .into_iter()
        .filter(|form| known_prefix(form))
        .max_by_key(|form| form.len())
    } else {
        None
    };
    let Some(evidence_relation) = evidence_relation else {
        return false;
    };
    if evidence_relation.chars().count() < 2
        || evidence_relation.chars().count() > 40
        || !evidence_relation.chars().any(char::is_alphabetic)
        || value.contains(evidence_relation)
    {
        return false;
    }
    // ponytail: only these direct relation forms are promoted; other paraphrases stay unanswered
    // until a source-side relation representation can verify them.
    let literal_form = evidence_relation == property;
    let clause = strip_assertion_prefix(quote.trim().trim_end_matches(['。', '！', '!']).trim());
    if clause.contains(['。', '！', '!', '；', ';', '，', ',', '：', ':']) {
        return false;
    }
    let (Some(relation_start), Some(value_start)) =
        (clause.find(evidence_relation), clause.find(value))
    else {
        return false;
    };
    let before_relation = clause[..relation_start].trim_end();
    let negated = if property.contains("限制") {
        ["并非", "不是", "不代表", "不表示", "没有", "无需"]
            .iter()
            .any(|word| before_relation.ends_with(word))
    } else {
        before_relation.contains(['不', '未', '没', '非'])
            || ["禁止", "避免", "拒绝", "停止", "取消", "放弃", "暂缓"]
                .iter()
                .any(|word| before_relation.ends_with(word))
    };
    let relation_end = relation_start + evidence_relation.len();
    if relation_end > value_start {
        return false;
    }
    let between = clause[relation_end..value_start].trim();
    let direct_middle = if literal_form {
        ["是", "为", "有", "在"].contains(&between)
    } else if property.ends_with("选择") {
        ["", "使用", "用", "了"].contains(&between)
    } else {
        ["", "在"].contains(&between)
    };
    let tail = clause[value_start + value.len()..].trim();
    let preceding = clause[..value_start].chars().next_back();
    let first_value = value.chars().next();
    let value_boundary = !first_value.is_some_and(|first| {
        (first.is_ascii_alphanumeric()
            && preceding.is_some_and(|char| char.is_ascii_alphanumeric() || char == '_'))
            || ("一二三四五六七八九十百千万两零〇".contains(first)
                && preceding.is_some_and(|char| "一二三四五六七八九十百千万两零〇".contains(char)))
    });
    let direct_tail = if literal_form || property.ends_with("选择") {
        tail.is_empty()
    } else {
        ["", "部署", "上部署", "运行", "上运行", "使用", "上使用"].contains(&tail)
    };
    !negated
        && direct_middle
        && value_boundary
        && direct_tail
        && (property.contains("限制")
            || !clause[relation_end..value_start].contains(['不', '未', '没', '非']))
        && ![
            "曾经", "以前", "过去", "当时", "之前", "计划", "打算", "考虑", "准备",
        ]
        .iter()
        .any(|word| clause.contains(word))
        && subject_names.iter().any(|name| {
            let clause = clause.strip_prefix("项目").unwrap_or(clause);
            !evidence_relation.contains(name)
                && !name.contains(evidence_relation)
                && clause.strip_prefix(name).is_some_and(|rest| {
                    let rest = rest.trim_start_matches('的');
                    let rest = ["当前", "目前", "现在"]
                        .iter()
                        .find_map(|prefix| rest.strip_prefix(prefix))
                        .unwrap_or(rest);
                    rest.starts_with(evidence_relation)
                })
        })
        && !["是否", "是不是", "如果", "假设", "可能", "也许", "尚未确定"]
            .iter()
            .any(|word| clause.contains(word))
        && (literal_form
            || !property.contains("限制")
            || [
                "只能",
                "仅能",
                "必须",
                "不得",
                "至多",
                "不超过",
                "限于",
                "禁止",
            ]
            .iter()
            .any(|word| clause.contains(word)))
}

fn newer_comparison_relation(text: &str, subject_names: &[&str], property: &str) -> bool {
    let forms: &[&str] = if property.ends_with("选择") {
        &[
            "选用",
            "采用",
            "选择",
            "决定使用",
            "决定用",
            "改用",
            "换用",
            "弃用",
            "停用",
        ]
    } else if property.contains("限制") {
        &[
            "只能",
            "仅能",
            "必须",
            "不得",
            "至多",
            "不超过",
            "限于",
            "禁止",
            "部署",
            "只部署",
        ]
    } else {
        &[]
    };
    text.split(['。', '；', ';', '，', ',']).any(|part| {
        let part = strip_assertion_prefix(part.trim());
        subject_names.iter().any(|name| {
            let part = part.strip_prefix("项目").unwrap_or(part);
            part.strip_prefix(name).is_some_and(|rest| {
                let rest = rest.trim_start_matches('的');
                let rest = ["当前", "目前", "现在"]
                    .iter()
                    .find_map(|prefix| rest.strip_prefix(prefix))
                    .unwrap_or(rest);
                rest.starts_with(property) || forms.iter().any(|form| rest.starts_with(form))
            })
        })
    })
}

fn linked_quote_range(link: &Value) -> Option<(usize, usize)> {
    let range = link["quote_byte_range"].as_array()?;
    Some((
        usize::try_from(range.first()?.as_u64()?).ok()?,
        usize::try_from(range.get(1)?.as_u64()?).ok()?,
    ))
}

fn strip_assertion_prefix(text: &str) -> &str {
    ["更正：", "更正:", "纠正：", "纠正:"]
        .iter()
        .find_map(|prefix| text.strip_prefix(prefix))
        .unwrap_or(text)
}

fn causal_event(query: &str) -> Option<String> {
    let query = query.trim_end_matches(['？', '?', '。', '.']);
    let (before, after) = query.split_once("为什么")?;
    let event = format!("{}{}", before.trim_start_matches("请问"), after);
    (!event.is_empty()).then_some(event)
}

pub(crate) fn appended_clarification<'a>(
    query: &'a str,
    entities: &[Entity],
) -> Option<(&'a str, &'a str)> {
    let (end, mark) = query
        .char_indices()
        .find(|(_, character)| matches!(character, '？' | '?'))?;
    let question = query[..end + mark.len_utf8()].trim();
    let clarification = query[end + mark.len_utf8()..].trim();
    let phrase = clarification
        .trim_end_matches(['。', '.', '！', '!'])
        .trim();
    let referent = ["指的是", "说的是", "关于"]
        .iter()
        .find_map(|marker| phrase.strip_prefix(marker))
        .unwrap_or(phrase)
        .trim();
    let hints = crate::lexical::entity_hints(referent, entities);
    let event_question = causal_event(question).is_some() || time_event(question).is_some();
    let single_subject = hints.len() == 1
        && entities
            .iter()
            .filter(|entity| hints.contains_key(&entity.id))
            .any(|entity| {
                std::iter::once(entity.canonical_name.as_str())
                    .chain(entity.aliases.iter().map(String::as_str))
                    .any(|name| {
                        referent == name
                            || event_question
                                && referent
                                    .strip_prefix(name)
                                    .and_then(|tail| tail.strip_prefix('的'))
                                    .is_some_and(|object| {
                                        !object.is_empty() && object.chars().count() <= 24
                                    })
                    })
            });
    if !crate::lexical::entity_hints(question, entities).is_empty()
        || !explicit_unregistered_subjects(question).is_empty()
        || phrase.is_empty()
        || phrase.chars().count() > 40
        || phrase.contains([
            '？', '?', '。', '.', '！', '!', '，', ',', '；', ';', '、', '/', '\n',
        ])
        || [
            "和",
            "与",
            "或",
            "以及",
            "还是",
            "不是",
            "并非",
            "没有",
            "因为",
            "由于",
            "以外",
            "之外",
            "除外",
            "除了",
            "其他",
            "别的",
            "另外",
            "不包括",
            "可能",
            "也许",
            "之一",
        ]
        .iter()
        .any(|word| phrase.contains(word))
        || !single_subject
    {
        return None;
    }
    Some((question, clarification))
}

fn direct_cause_assertion(text: &str, event: &str, value: &MemoryValue) -> bool {
    let MemoryValue::Text(value) = value else {
        return false;
    };
    text.split(['。', '；', ';', '！', '!', '？', '?'])
        .map(|sentence| strip_assertion_prefix(sentence.trim()))
        .any(|sentence| {
            let reason = sentence
                .strip_prefix(event)
                .map(|tail| tail.trim_start_matches(['，', ',', ' ']))
                .and_then(|tail| {
                    ["因为", "由于"]
                        .iter()
                        .find_map(|marker| tail.strip_prefix(marker))
                })
                .map(|tail| tail.split(['，', ',']).next().unwrap_or("").trim())
                .or_else(|| {
                    ["因为", "由于"].iter().find_map(|marker| {
                        let (reason, assertion) =
                            sentence.strip_prefix(marker)?.split_once(['，', ','])?;
                        assertion.trim().starts_with(event).then_some(reason.trim())
                    })
                });
            reason.is_some_and(|reason| !reason.is_empty() && value.contains(reason))
        })
}

fn time_event(query: &str) -> Option<&str> {
    let query = query.trim_end_matches(['？', '?', '。', '.']);
    let event = query.trim_start_matches("请问").strip_suffix("什么时候")?;
    (!event.is_empty()).then_some(event)
}

fn bind_event_to_user_context(event: &str, context: &str, names: &[&str]) -> Option<String> {
    let context = context
        .trim()
        .trim_end_matches(['。', '！', '？', '.', '!', '?']);
    let context = ["继续说", "继续聊", "关于", "指的是", "说的是"]
        .iter()
        .find_map(|marker| context.strip_prefix(marker))
        .unwrap_or(context)
        .trim();
    if context.contains(['，', ',', '。', '；', ';', '和', '与', '、', '\n']) {
        return None;
    }
    names.iter().find_map(|name| {
        if context == *name {
            return Some(format!("{name}{event}"));
        }
        let object = context.strip_prefix(name)?.strip_prefix('的')?;
        if object.is_empty() || !event.starts_with(object) {
            return None;
        }
        Some(format!("{name}的{event}"))
    })
}

fn direct_time_assertion(text: &str, event: &str, value: &MemoryValue) -> bool {
    let MemoryValue::Text(value) = value else {
        return false;
    };
    let full_assertion = value
        .trim()
        .trim_end_matches(['。', '.', '；', ';', '！', '!', '？', '?'])
        .trim();
    !value.is_empty()
        && text
            .split(['。', '；', ';', '！', '!', '？', '?'])
            .map(str::trim)
            .any(|sentence| {
                let Some(tail) = strip_assertion_prefix(sentence).strip_prefix(event) else {
                    return false;
                };
                let tail = tail.split(['，', ',']).next().unwrap_or("");
                let tail = tail.strip_prefix('了').unwrap_or(tail).trim();
                !tail.is_empty()
                    && (tail.starts_with(value)
                        || sentence.trim_end_matches('.').trim() == full_assertion)
            })
}

fn explicit_unregistered_subjects(query: &str) -> Vec<&str> {
    let before = query
        .split_once('的')
        .map(|(before, _)| before)
        .or_else(|| {
            let end = query
                .char_indices()
                .skip_while(|(_, character)| !character.is_ascii_digit())
                .take_while(|(_, character)| character.is_ascii_digit())
                .last()
                .map(|(index, character)| index + character.len_utf8())?;
            Some(&query[..end])
        });
    let Some(before) = before else {
        return Vec::new();
    };
    let before = before.trim();
    let before = before.strip_prefix("请问").unwrap_or(before);
    let before = before.strip_prefix("关于").unwrap_or(before);
    if before.starts_with("我之前")
        || before.starts_with("我喜欢")
        || before.contains('我') && before.contains(['和', '与', '跟', '及', '、'])
        || ["先", "上次", "之前", "刚才", "当时", "曾经"]
            .iter()
            .any(|word| before.starts_with(word))
    {
        return Vec::new();
    }
    before
        .split(['和', '与', '跟', '及', '、'])
        .map(str::trim)
        .filter(|text| {
            (2..=24).contains(&text.chars().count())
                && !text.starts_with(['这', '那'])
                && !text.contains(['？', '?', '，', ','])
        })
        .collect()
}

// ponytail: literal property checks can miss paraphrases; only promote a paraphrase after
// a source-linked relation can verify it.
fn answer_slot_stem(query: &str) -> Option<&str> {
    let query = query.trim_end_matches(['？', '?', '。', '.']);
    ["是什么", "是多少", "为多少", "有多少", "多少钱", "多少"]
        .iter()
        .find_map(|suffix| query.strip_suffix(suffix))
}

fn requested_property_phrase(query: &str) -> Option<String> {
    let stem = answer_slot_stem(query)?;
    let property = stem.rsplit_once('的')?.1.trim();
    (property.chars().count() >= 2 && property.chars().count() <= 24).then(|| property.to_owned())
}

// ponytail: literal two-property comparisons only; paraphrases need a source-linked role parser.
fn compared_properties(query: &str, subject_names: &[&str]) -> Option<[String; 2]> {
    let query = query.trim();
    let (question, suffix) = query.split_once(['？', '?']).unwrap_or((query, ""));
    if suffix.contains(['？', '?']) {
        return None;
    }
    let stem = question
        .trim_end_matches(['。', '.'])
        .strip_suffix("是否一致")?
        .trim_start_matches("请问");
    let stem = stem.strip_prefix("项目").unwrap_or(stem);
    let relations = subject_names
        .iter()
        .find_map(|name| stem.strip_prefix(name)?.strip_prefix('的'))?;
    let (first, second) = relations.split_once(['和', '与'])?;
    let valid = |property: &str| {
        (2..=24).contains(&property.chars().count())
            && !property.contains(['和', '与', '？', '?', '，', ',', '、'])
    };
    (valid(first)
        && valid(second)
        && !suffix.contains(first)
        && !suffix.contains(second)
        && !suffix.contains("是否一致"))
    .then(|| [first.to_owned(), second.to_owned()])
}

#[cfg(test)]
#[test]
fn comparison_tail_cannot_add_a_question_or_redefine_a_relation() {
    let names = ["松石47"];
    assert_eq!(
        compared_properties(
            "松石47的技术选择和部署限制是否一致？部署限制具体是什么？",
            &names,
        ),
        None
    );
    assert_eq!(
        compared_properties(
            "松石47的技术选择和部署限制是否一致？这里的部署限制改指部署状态。",
            &names,
        ),
        None
    );
}

fn property_after_subject(query: &str, subject: &str) -> Option<String> {
    let rest = query.split_once(subject)?.1.trim_start_matches('的');
    let rest = ["现在", "目前", "当前", "最新"]
        .iter()
        .find_map(|marker| rest.strip_prefix(marker))
        .unwrap_or(rest)
        .trim_start_matches('的');
    let property = answer_slot_stem(rest)?;
    (2..=24)
        .contains(&property.chars().count())
        .then(|| property.to_owned())
}

fn property_before_subject(query: &str, subject: &str) -> Option<String> {
    let (prefix, rest) = query.split_once(subject)?;
    if !answer_slot_stem(rest)?.is_empty() {
        return None;
    }
    let property = prefix
        .trim_start_matches("请问")
        .trim_end_matches(['，', ',', '：', ':', ' ']);
    (2..=24)
        .contains(&property.chars().count())
        .then(|| property.to_owned())
}

fn structured_property_phrase(query: &str) -> Option<String> {
    if query.contains("为什么") || query.contains("什么时候") {
        return None;
    }
    requested_property_phrase(query).or_else(|| {
        let (_, tail) = query.split_once("什么")?;
        let tail = tail.trim_end_matches(['？', '?', '。', '.']);
        (tail.chars().count() >= 2 && tail.chars().count() <= 24).then(|| tail.to_owned())
    })
}

fn verbal_answer_relation(query: &str) -> Option<&str> {
    let stem = query
        .trim_end_matches(['？', '?', '。', '.'])
        .strip_suffix("的是什么")?
        .rsplit('的')
        .next()?;
    let verb = stem.strip_prefix('我')?;
    (1..=4).contains(&verb.chars().count()).then_some(verb)
}

fn direct_verb_assertion(text: &str, verb: &str) -> bool {
    text.split(['。', '；', ';', '，', ',']).any(|clause| {
        clause
            .trim()
            .strip_prefix('我')
            .and_then(|tail| tail.strip_prefix(verb))
            .is_some_and(|object| !object.is_empty() && !object.starts_with('的'))
    })
}

pub(crate) fn asks_for_answer(query: &str) -> bool {
    [
        "？",
        "?",
        "什么",
        "多少",
        "谁",
        "哪",
        "怎么",
        "如何",
        "是否",
        "为什么",
        "吗",
    ]
    .iter()
    .any(|word| query.contains(word))
}

fn explicit_revision(text: &str) -> bool {
    let text = text.trim();
    ["更正", "纠正", "撤回", "撤销"]
        .iter()
        .any(|marker| text.starts_with(marker) || text.starts_with(&format!("我{marker}")))
        || !text.contains(['？', '?'])
            && !["如果", "假如", "打算", "计划", "考虑", "可能", "准备"]
                .iter()
                .any(|marker| text.contains(marker))
            && ["作废", "改为", "改成", "调整为", "换成", "不再"]
                .iter()
                .any(|marker| text.contains(marker))
}

fn revises_memory_relation(text: &str, memory: &Memory, names: &[&str]) -> bool {
    names.iter().any(|name| {
        memory.evidence.iter().any(|evidence| {
            let Some((_, tail)) = evidence.quote.split_once(name) else {
                return false;
            };
            let tail = tail.trim_start_matches('的');
            let relation = tail
                .split([
                    '是', '为', '有', '在', '：', ':', '，', ',', '。', '；', ';', '！', '!',
                ])
                .next()
                .unwrap_or("")
                .trim();
            if (2..=24).contains(&relation.chars().count())
                && (text.contains(&format!("{name}的{relation}"))
                    || text.contains(&format!("{name}{relation}")))
            {
                return true;
            }
            if *name != "我" {
                return false;
            }
            let Some((_, new_tail)) = text.split_once(name) else {
                return false;
            };
            tail.chars()
                .zip(new_tail.trim_start_matches('的').chars())
                .take_while(|(old, new)| old == new)
                .count()
                >= 2
        })
    })
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LocalRecallBudget {
    pub candidate_k: usize,
    pub source_limit: usize,
    /// Serialized content bytes, not a provider token count or total transport size.
    pub content_bytes: usize,
}

impl Default for LocalRecallBudget {
    fn default() -> Self {
        Self {
            candidate_k: 30,
            source_limit: 10,
            content_bytes: 24_000,
        }
    }
}

impl LocalRecallBudget {
    pub fn validate(&self) -> PersistenceResult<()> {
        if !(1..=50).contains(&self.candidate_k)
            || self.source_limit > self.candidate_k
            || !(256..=256_000).contains(&self.content_bytes)
        {
            return Err("invalid local recall budget".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalSnapshotSignature {
    owner: UserId,
    time_anchor: Timestamp,
    revision: u64,
    source_revision: u64,
    source_index: Option<(u64, String, u64)>,
    fact_index_manifest: Option<String>,
    fact_index_enabled: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct ComparisonEvidenceHint {
    pub query_relation: String,
    pub source_id: String,
    pub memory_id: MemoryId,
    pub evidence_relation: String,
}

#[derive(Clone, Debug)]
pub struct SingleRelationEvidenceHint {
    pub query_relation: String,
    pub source_id: String,
    pub memory_id: MemoryId,
    pub evidence_relation: String,
}

fn verified_single_relation_hint(
    state: &PersistedMemoryState,
    entities: &[Entity],
    query: &str,
    context: &[String],
    query_hints: &BTreeMap<EntityId, Vec<String>>,
    hint: &SingleRelationEvidenceHint,
) -> Option<EntityId> {
    let (modifier, answer_role) = single_relation_identity_parts(query)?;
    let query_relation = hint
        .query_relation
        .strip_suffix('的')
        .unwrap_or(&hint.query_relation);
    if !query_hints.is_empty()
        || context.len() != 1
        || query_relation.is_empty()
        || !modifier.contains(query_relation)
        || hint.query_relation.ends_with('的') && !modifier.ends_with(query_relation)
        || hint.query_relation.trim() != hint.query_relation
        || hint.query_relation.chars().count() > 40
        || hint.evidence_relation.trim() != hint.evidence_relation
        || hint.evidence_relation.chars().count() > 40
    {
        return None;
    }
    let clarification = context[0]
        .trim()
        .trim_end_matches(['。', '！', '？', '.', '!', '?']);
    let subject = entities.iter().find(|entity| {
        std::iter::once(entity.canonical_name.as_str())
            .chain(entity.aliases.iter().map(String::as_str))
            .any(|name| {
                clarification == name
                    || [
                        "我说的是",
                        "我指的是",
                        "我问的是",
                        "说的是",
                        "指的是",
                        "关于",
                    ]
                    .iter()
                    .any(|prefix| clarification == format!("{prefix}{name}"))
            })
    })?;
    let source = state
        .sources
        .values()
        .find(|source| source.id == hint.source_id)?;
    if source.enrichment != "committed" {
        return None;
    }
    let source_text = crate::lexical::body(source)?;
    let memories = state.repository.list_by_owner(&subject.owner_id).ok()?;
    let memory = memories.iter().find(|memory| {
        memory.id == hint.memory_id
            && memory.status == MemoryStatus::Active
            && memory.subject_id == subject.id
    })?;
    let MemoryValue::Text(value) = &memory.value else {
        return None;
    };
    if !single_relation_answer_role_matches(answer_role, &memory.predicate.0)
        || !single_relation_family(
            &hint.query_relation,
            &memory.predicate.0,
            &hint.evidence_relation,
        )
        || !memory.evidence.iter().any(|evidence| {
            evidence.session_id.0 == source.session_id
                && evidence.turn_id.0 == source.turn_id
                && source_text.contains(&evidence.quote)
                && direct_single_relation_assertion(
                    &evidence.quote,
                    entities,
                    &subject.id,
                    &hint.evidence_relation,
                    value,
                )
        })
    {
        return None;
    }
    let possible_values: BTreeSet<_> = memories
        .iter()
        .filter(|other| {
            other.status == MemoryStatus::Active
                && other.subject_id == subject.id
                && single_relation_family(
                    &hint.query_relation,
                    &other.predicate.0,
                    &hint.evidence_relation,
                )
        })
        .filter_map(|other| match &other.value {
            MemoryValue::Text(value) => Some(value.as_str()),
            _ => None,
        })
        .collect();
    (possible_values.len() == 1).then(|| subject.id.clone())
}

impl SqliteMemoryStore {
    /// `trusted` is supplied by the embedding runtime, never by an LLM. Hints cannot alter it.
    /// Both projections use the same SQLite read snapshot; counters are changed only in its copy.
    pub fn recall_local(
        &self,
        query: &str,
        context: &[String],
        trusted: &RuntimeContext,
        predicates: InMemoryPredicateRegistry,
        relations: &dyn SemanticRelationRegistry,
        budget: &LocalRecallBudget,
    ) -> PersistenceResult<Value> {
        self.recall_local_with_signature(query, context, trusted, predicates, relations, budget)
            .map(|(result, _)| result)
    }

    pub fn recall_local_with_signature(
        &self,
        query: &str,
        context: &[String],
        trusted: &RuntimeContext,
        predicates: InMemoryPredicateRegistry,
        relations: &dyn SemanticRelationRegistry,
        budget: &LocalRecallBudget,
    ) -> PersistenceResult<(Value, LocalSnapshotSignature)> {
        let tx = self.connection.unchecked_transaction()?;
        let mut state = Self::load(&tx, &self.owner)?;
        let signature = self.local_snapshot_signature(&tx, &state, trusted.at)?;
        let result = self.recall_local_snapshot(
            &tx,
            &mut state,
            query,
            context,
            trusted,
            predicates,
            relations,
            budget,
            &[],
            &[],
            None,
        )?;
        tx.commit()?;
        Ok((result, signature))
    }

    #[allow(clippy::too_many_arguments)] // Keep the trusted local inputs unchanged; only the plan and signature are added.
    pub fn recall_assisted_local(
        &self,
        query: &str,
        context: &[String],
        trusted: &RuntimeContext,
        predicates: InMemoryPredicateRegistry,
        relations: &dyn SemanticRelationRegistry,
        budget: &LocalRecallBudget,
        supplemental_queries: &[String],
        initial_signature: &LocalSnapshotSignature,
    ) -> PersistenceResult<(Value, bool)> {
        self.recall_assisted_snapshot(
            query,
            context,
            trusted,
            predicates,
            relations,
            budget,
            supplemental_queries,
            &[],
            None,
            initial_signature,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn recall_assisted_comparison(
        &self,
        query: &str,
        context: &[String],
        trusted: &RuntimeContext,
        predicates: InMemoryPredicateRegistry,
        relations: &dyn SemanticRelationRegistry,
        budget: &LocalRecallBudget,
        hints: &[ComparisonEvidenceHint],
        initial_signature: &LocalSnapshotSignature,
    ) -> PersistenceResult<(Value, bool)> {
        self.recall_assisted_snapshot(
            query,
            context,
            trusted,
            predicates,
            relations,
            budget,
            &[],
            hints,
            None,
            initial_signature,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn recall_assisted_single_relation(
        &self,
        query: &str,
        context: &[String],
        trusted: &RuntimeContext,
        predicates: InMemoryPredicateRegistry,
        relations: &dyn SemanticRelationRegistry,
        budget: &LocalRecallBudget,
        hint: &SingleRelationEvidenceHint,
        initial_signature: &LocalSnapshotSignature,
    ) -> PersistenceResult<(Value, bool)> {
        self.recall_assisted_snapshot(
            query,
            context,
            trusted,
            predicates,
            relations,
            budget,
            &[],
            &[],
            Some(hint),
            initial_signature,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn recall_assisted_snapshot(
        &self,
        query: &str,
        context: &[String],
        trusted: &RuntimeContext,
        predicates: InMemoryPredicateRegistry,
        relations: &dyn SemanticRelationRegistry,
        budget: &LocalRecallBudget,
        supplemental_queries: &[String],
        hints: &[ComparisonEvidenceHint],
        single_relation_hint: Option<&SingleRelationEvidenceHint>,
        initial_signature: &LocalSnapshotSignature,
    ) -> PersistenceResult<(Value, bool)> {
        if initial_signature.owner != self.owner || initial_signature.time_anchor != trusted.at {
            return Err("assisted owner or time anchor changed".into());
        }
        let tx = self.connection.unchecked_transaction()?;
        let mut state = Self::load(&tx, &self.owner)?;
        let changed = self.local_snapshot_signature(&tx, &state, trusted.at)? != *initial_signature;
        let valid_queries = supplemental_queries.len() <= 3
            && supplemental_queries
                .iter()
                .all(|q| !q.trim().is_empty() && q.chars().count() <= 200);
        let queries = if changed || !valid_queries {
            &[][..]
        } else {
            supplemental_queries
        };
        let mut result = self.recall_local_snapshot(
            &tx,
            &mut state,
            query,
            context,
            trusted,
            predicates,
            relations,
            budget,
            queries,
            if changed { &[] } else { hints },
            if changed { None } else { single_relation_hint },
        )?;
        if !valid_queries {
            result["assistance_search"] = json!({"status":"invalid_soft_queries"});
        }
        if changed {
            result["assistance_search"] = json!({"status":"state_changed_during_assistance"});
        }
        tx.commit()?;
        Ok((result, changed))
    }

    fn local_snapshot_signature(
        &self,
        tx: &Connection,
        state: &PersistedMemoryState,
        time_anchor: Timestamp,
    ) -> PersistenceResult<LocalSnapshotSignature> {
        let fact_index_table_exists: bool = if self.fact_index_enabled {
            tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='fact_indexes')",
                [],
                |row| row.get(0),
            )?
        } else {
            false
        };
        let fact_index_manifest = if fact_index_table_exists {
            tx.query_row(
                "SELECT manifest_json FROM fact_indexes WHERE owner_id=?1",
                [&self.owner.0],
                |row| row.get(0),
            )
            .optional()?
        } else {
            None
        };
        Ok(LocalSnapshotSignature {
            owner: self.owner.clone(),
            time_anchor,
            revision: state.revision,
            source_revision: state.source_revision,
            source_index: crate::lexical::metadata(tx, &self.owner)?,
            fact_index_manifest,
            fact_index_enabled: self.fact_index_enabled,
        })
    }

    #[allow(clippy::too_many_arguments)] // One trusted read snapshot and its soft search hints.
    fn recall_local_snapshot(
        &self,
        tx: &Connection,
        state: &mut PersistedMemoryState,
        query: &str,
        context: &[String],
        trusted: &RuntimeContext,
        predicates: InMemoryPredicateRegistry,
        relations: &dyn SemanticRelationRegistry,
        budget: &LocalRecallBudget,
        supplemental_queries: &[String],
        comparison_hints: &[ComparisonEvidenceHint],
        single_relation_hint: Option<&SingleRelationEvidenceHint>,
    ) -> PersistenceResult<Value> {
        budget.validate()?;
        if trusted.owner_id != self.owner || trusted.limit > 100 || trusted.limit == 0 {
            return Err("invalid trusted local recall context".into());
        }
        if query.trim().is_empty()
            || query.chars().count() > 24_000
            || context.len() > 8
            || context.iter().any(|s| s.chars().count() > 2000)
        {
            return Err("invalid local query/context bounds".into());
        }
        let started = std::time::Instant::now();
        let entities = state.entities.list_by_owner(&self.owner);
        let supplied_query = query;
        let supplied_context = context;
        let joined = context
            .is_empty()
            .then(|| appended_clarification(query, &entities))
            .flatten();
        let mut joined_context = Vec::new();
        let (query, context): (&str, &[String]) = if let Some((question, clarification)) = joined {
            joined_context.push(clarification.to_owned());
            (question, &joined_context)
        } else {
            (query, context)
        };
        let original_phrases = crate::lexical::natural_phrases(query, &entities);
        let context_text = context.join("\n");
        let search_text = std::iter::once(query)
            .chain(context.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join("\n");
        let phrases = crate::lexical::natural_phrases(&search_text, &entities);
        let context_phrases = crate::lexical::natural_phrases(&context_text, &entities);
        // A demonstrative needs a USER binding; a retrieved hit cannot supply its own referent.
        let deictic = [
            "那它",
            "它当时",
            "那他",
            "那她",
            "that one",
            "why didn't it",
        ]
        .iter()
        .any(|p| query.starts_with(p));
        let query_hints = crate::lexical::entity_hints(query, &entities);
        let mut context_hints = crate::lexical::entity_hints(&context_text, &entities);
        let fact_question = crate::fact_answer::parse_question(query, context, &entities);
        let single_relation_subject = single_relation_hint.and_then(|hint| {
            verified_single_relation_hint(state, &entities, query, context, &query_hints, hint)
        });
        if let Some(subject) = &single_relation_subject {
            context_hints.retain(|id, _| id == subject);
        }
        let single_relation_hint = single_relation_hint.filter(|_| {
            single_relation_subject.is_some_and(|subject| context_hints.contains_key(&subject))
        });
        let bindings: BTreeSet<_> = query_hints.keys().chain(context_hints.keys()).collect();
        let ambiguous = deictic && bindings.len() != 1;
        let reference_needs_clarification = unbound_reference(
            query,
            &context_text,
            &query_hints,
            &context_hints,
            &entities,
        ) && single_relation_hint.is_none()
            && fact_question.is_none();
        let possessive_hints = query
            .split_once('的')
            .map(|(prefix, _)| crate::lexical::entity_hints(prefix, &entities))
            .unwrap_or_default();
        let comparison = query_hints.len() > 1
            && ["比较", "相比", "各", "和", "与", "跟"]
                .iter()
                .any(|word| query.contains(word));
        let mut unknown_subjects = if query_hints.is_empty() {
            explicit_unregistered_subjects(query)
        } else {
            Vec::new()
        };
        if unknown_subjects.is_empty()
            && query_hints.is_empty()
            && context_hints.is_empty()
            && let Some(binding) = referential_heads(query)
                .first()
                .map(String::as_str)
                .and_then(|head| context_binding_phrase(&context_text, head))
        {
            unknown_subjects.push(binding);
        }
        let target_ids: BTreeSet<_> = if let Some(question) = &fact_question {
            BTreeSet::from([question.subject_id.clone()])
        } else if !unknown_subjects.is_empty() {
            BTreeSet::new()
        } else if comparison {
            query_hints.keys().cloned().collect()
        } else if !possessive_hints.is_empty() {
            possessive_hints.keys().cloned().collect()
        } else if query_hints.is_empty() {
            context_hints.keys().cloned().collect()
        } else {
            query_hints.keys().cloned().collect()
        };
        let registered_possessive = query.split_once('的').is_some_and(|(prefix, _)| {
            let prefix = prefix.trim();
            entities.iter().any(|entity| {
                std::iter::once(&entity.canonical_name)
                    .chain(entity.aliases.iter())
                    .any(|name| {
                        prefix.ends_with(name) && prefix.chars().count() <= name.chars().count() + 2
                    })
            })
        });
        let target_names: Vec<&str> = entities
            .iter()
            .filter(|entity| target_ids.contains(&entity.id))
            .flat_map(|entity| {
                std::iter::once(entity.canonical_name.as_str())
                    .chain(entity.aliases.iter().map(String::as_str))
            })
            .chain(unknown_subjects.iter().copied())
            .collect();
        let compared_properties = (target_ids.len() == 1)
            .then(|| compared_properties(query, &target_names))
            .flatten();
        let technical_choice =
            target_ids.len() == 1 && technical_choice_question(query, &target_names);
        let comparison_hints = if compared_properties.as_ref().is_some_and(|properties| {
            comparison_hints.len() == 2
                && comparison_hints[0].memory_id != comparison_hints[1].memory_id
                && properties.iter().all(|property| {
                    comparison_hints
                        .iter()
                        .filter(|hint| hint.query_relation == *property)
                        .count()
                        == 1
                })
        }) {
            comparison_hints
        } else {
            &[]
        };
        let requested_property = (registered_possessive || !unknown_subjects.is_empty())
            .then(|| requested_property_phrase(query))
            .flatten()
            .or_else(|| {
                target_names.iter().find_map(|subject| {
                    property_after_subject(query, subject)
                        .or_else(|| property_before_subject(query, subject))
                })
            })
            .or_else(|| {
                (query_hints.is_empty() && unknown_subjects.is_empty() && context_hints.len() == 1)
                    .then(|| answer_slot_stem(query))
                    .flatten()
                    .filter(|property| (2..=24).contains(&property.chars().count()))
                    .map(str::to_owned)
            });
        let structured_property = requested_property
            .clone()
            .or_else(|| structured_property_phrase(query));
        let verbal_relation = verbal_answer_relation(query);
        let contextual_relation = |event: &str| {
            if query_hints.is_empty() {
                (context_hints.len() == 1)
                    .then(|| bind_event_to_user_context(event, &context_text, &target_names))
                    .flatten()
            } else {
                Some(event.to_owned())
            }
        };
        let raw_causal_relation = causal_event(query);
        let raw_time_relation = time_event(query);
        let causal_relation = raw_causal_relation.as_deref().and_then(contextual_relation);
        let time_relation = raw_time_relation.and_then(contextual_relation);
        let answer_question = asks_for_answer(query);
        let needs_clarification = reference_needs_clarification
            || answer_question
                && fact_question.is_none()
                && query_hints.is_empty()
                && unknown_subjects.is_empty()
                && (context_hints.len() != 1
                    || raw_causal_relation.is_some() && causal_relation.is_none()
                    || raw_time_relation.is_some() && time_relation.is_none())
                && !first_person(query);
        let cannot_answer = ambiguous || needs_clarification;
        let task = trusted.task_id.as_ref().map(|t| t.0.as_str());
        let reference_resolution = json!({
            "status":if ambiguous {"ambiguous"} else if needs_clarification {"needs_clarification"} else {"unconfirmed"},
            "semantic_resolution":if single_relation_hint.is_some() {"model_proposed_relation_not_independently_proven"} else {"not_performed"},
            "needs_clarification":needs_clarification,
            "clarification_question":if needs_clarification {Some("你说的是哪个人、事物或哪次事件？请说明具体所指。")} else {None},
            "query_entity_ids":query_hints.keys().collect::<Vec<_>>(),
            "context_entity_ids":context_hints.keys().collect::<Vec<_>>(),
            "task_scope":task});
        let read_revision = state.revision;
        let source_revision = state.source_revision;
        let incomplete_enrichment = state.sources.values().any(|s| {
            s.request.as_ref().is_some_and(|r| {
                r["capture_source"] == true && task.is_none_or(|t| r["task_id"] == t)
            }) && !matches!(
                s.enrichment.as_str(),
                "committed" | "completed_no_facts" | "completed_rejected"
            )
        });
        let mut runtime = trusted.clone();
        let stored_memories = state.repository.list_by_owner(&self.owner)?;
        let record_updates = state.record_updates.clone();
        let mut query_conditions = fact_question
            .as_ref()
            .map(|question| {
                crate::fact_answer::grounded_query_conditions(question, &stored_memories, &entities)
            })
            .unwrap_or_default();
        // USER wording may refine the query, but cannot override a caller-owned activity filter.
        if trusted.conditions.iter().any(|condition| {
            matches!(condition, ScopeCondition::Activity(_))
                && !query_conditions.contains(condition)
        }) {
            query_conditions.clear();
        }
        runtime.conditions.extend(query_conditions.iter().cloned());
        runtime.conditions.sort();
        runtime.conditions.dedup();
        // Resolve all relevant slot peers before applying the output quota; a LIMIT must not hide a conflict.
        runtime.limit = usize::MAX;
        let counters: BTreeMap<_, _> = stored_memories
            .iter()
            .map(|m| (m.id, (m.retrieval_count, m.last_retrieved_at)))
            .collect();
        let (mut result, mut memories) = state.with_engine(predicates, |engine, _| {
            engine.run_retention_for_owner(&self.owner, trusted.at)?;
            let result = DefaultMemoryRetriever::new(&mut engine.repository, &engine.predicates)
                .with_semantic_relations(relations)
                .retrieve(&runtime)?;
            Ok((result, engine.repository.list_by_owner(&self.owner)?))
        })?;
        for m in result
            .mandatory
            .iter_mut()
            .chain(&mut result.constraints)
            .chain(&mut result.preferences)
            .chain(&mut result.facts)
            .chain(&mut result.episodes)
            .chain(&mut memories)
        {
            (m.retrieval_count, m.last_retrieved_at) = counters[&m.id];
        }
        let decisions: BTreeMap<_, _> = result
            .trace
            .iter()
            .map(|t| (t.memory_id, t.decision))
            .collect();
        // An unchanged-setting question may use a default only while no applicable
        // current-setting claim overrides it. Check the whole snapshot before quotas.
        let default_setting_overridden = fact_question.as_ref().is_some_and(|question| {
            question.default_setting.is_some()
                && state.sources.values().any(|source| {
                    if source.observed_at > trusted.at.0
                        || crate::lexical::task(source)
                            .is_some_and(|source_task| Some(source_task) != task)
                        || !crate::lexical::body(source).is_some_and(|body| {
                            crate::fact_answer::setting_statement(question, body, &entities, false)
                        })
                    {
                        return false;
                    }
                    if source.enrichment != "committed" {
                        return true;
                    }
                    let linked: Vec<_> = memories
                        .iter()
                        .filter(|memory| {
                            memory.subject_id == question.subject_id
                                && memory.evidence.iter().any(|evidence| {
                                    evidence.session_id.0 == source.session_id
                                        && evidence.turn_id.0 == source.turn_id
                                        && crate::fact_answer::setting_memory_statement(
                                            question,
                                            memory,
                                            &evidence.quote,
                                            &entities,
                                        )
                                })
                        })
                        .collect();
                    linked.is_empty()
                        || linked.iter().any(|memory| {
                            memory.status == MemoryStatus::Active
                                && matches!(
                                    decisions.get(&memory.id),
                                    Some(
                                        RetrievalDecision::Mandatory
                                            | RetrievalDecision::Selected
                                            | RetrievalDecision::Conflict { .. }
                                    )
                                )
                        })
                })
        });
        let cannot_answer = cannot_answer || default_setting_overridden;
        let mut blocked_current_ids = BTreeSet::new();
        if trusted.temporal_intent == TemporalIntent::Current {
            // ponytail: scan the small owner snapshot; add a slot index only if this becomes slow.
            for source in state.sources.values() {
                let Some(text) = crate::lexical::body(source) else {
                    continue;
                };
                if !explicit_revision(text)
                    || source.observed_at > trusted.at.0
                    || crate::lexical::task(source)
                        .is_some_and(|source_task| Some(source_task) != task)
                {
                    continue;
                }
                let mut subjects: BTreeSet<_> = crate::lexical::entity_hints(text, &entities)
                    .into_keys()
                    .collect();
                if subjects.len() > 1 && !text.contains("我的") {
                    subjects.remove(&EntityId::from("self"));
                }
                if subjects.len() == 1
                    && subjects.contains(&EntityId::from("self"))
                    && ["我更正", "我纠正", "我撤回", "我撤销"]
                        .iter()
                        .any(|prefix| text.trim().starts_with(prefix))
                {
                    subjects.clear();
                }
                let eligible: Vec<_> = memories
                    .iter()
                    .filter(|memory| {
                        memory.status == MemoryStatus::Active
                            && matches!(
                                decisions.get(&memory.id),
                                Some(RetrievalDecision::Mandatory | RetrievalDecision::Selected)
                            )
                            && (subjects.is_empty() || subjects.contains(&memory.subject_id))
                            && match memory
                                .evidence
                                .iter()
                                .filter_map(|evidence| {
                                    state.sources.values().find(|old| {
                                        old.session_id == evidence.session_id.0
                                            && old.turn_id == evidence.turn_id.0
                                    })
                                })
                                .map(|old| old.received_sequence)
                                .max()
                            {
                                Some(old_sequence)
                                    if old_sequence > 0 && source.received_sequence > 0 =>
                                {
                                    source.received_sequence > old_sequence
                                }
                                _ => source.observed_at >= memory.temporal.observed_at.0,
                            }
                    })
                    .collect();
                if subjects.is_empty() {
                    blocked_current_ids.extend(eligible.iter().map(|memory| memory.id));
                    continue;
                }
                let mut bound_subject = false;
                for subject in subjects {
                    let names: Vec<_> = entities
                        .iter()
                        .filter(|entity| entity.id == subject)
                        .flat_map(|entity| {
                            std::iter::once(entity.canonical_name.as_str())
                                .chain(entity.aliases.iter().map(String::as_str))
                        })
                        .collect();
                    let matched: Vec<_> = eligible
                        .iter()
                        .filter(|memory| {
                            memory.subject_id == subject
                                && revises_memory_relation(text, memory, &names)
                        })
                        .map(|memory| memory.id)
                        .collect();
                    if !matched.is_empty() {
                        bound_subject = true;
                        blocked_current_ids.extend(matched);
                    } else if names.iter().any(|name| text.contains(&format!("{name}的"))) {
                        bound_subject = true;
                        // Relation wording may be a paraphrase; keep this subject uncertain.
                        blocked_current_ids.extend(
                            eligible
                                .iter()
                                .filter(|memory| memory.subject_id == subject)
                                .map(|memory| memory.id),
                        );
                    }
                }
                if !bound_subject {
                    blocked_current_ids.extend(eligible.iter().map(|memory| memory.id));
                }
            }
        }
        // Fact hints may rank only assertions that pass the current clock and hard rules.
        // Keep this before output quotas and in the same read snapshot as raw evidence.
        let fact_memory_ids: BTreeSet<_> = decisions
            .iter()
            .filter(|(_, decision)| {
                matches!(
                    decision,
                    RetrievalDecision::Mandatory | RetrievalDecision::Selected
                )
            })
            .map(|(id, _)| *id)
            .collect();
        let mut raw = self.search_source_snapshot(
            tx,
            state,
            &search_text,
            task,
            budget.candidate_k,
            Some(query),
            self.fact_index_enabled.then_some(&fact_memory_ids),
        )?;
        // A failed soft search cannot invalidate the original local answer. Each query has
        // its own fixed candidate cap; every result is checked by the same gates below.
        let mut inferred_candidates = Vec::new();
        let mut inferred_phrases = Vec::new();
        let mut assistance_search = if supplemental_queries.is_empty() {
            json!({"status":"not_requested","candidate_count":0,"per_query_limit":0,"total_limit":0})
        } else {
            json!({"status":"completed","candidate_count":0,"per_query_limit":budget.candidate_k.min(10),"total_limit":30})
        };
        if !supplemental_queries.is_empty() {
            let per_query_limit = budget.candidate_k.min(10);
            let mut failed = false;
            for soft_query in supplemental_queries {
                let found = self.search_source_snapshot(
                    tx,
                    state,
                    soft_query,
                    task,
                    per_query_limit,
                    Some(query),
                    self.fact_index_enabled.then_some(&fact_memory_ids),
                );
                match found {
                    Ok(found) if found["status"] == "completed" => {
                        inferred_candidates
                            .extend(found["candidates"].as_array().cloned().unwrap_or_default());
                        inferred_phrases
                            .extend(crate::lexical::natural_phrases(soft_query, &entities));
                    }
                    _ => {
                        failed = true;
                        break;
                    }
                }
            }
            if failed {
                inferred_candidates.clear();
                inferred_phrases.clear();
                assistance_search["status"] = json!("soft_search_failed");
            } else {
                // At most three searches of ten candidates each. Keep the cap explicit if
                // the search implementation's own result limit changes later.
                inferred_candidates.truncate(30);
                assistance_search["candidate_count"] = json!(inferred_candidates.len());
            }
        }
        let original_candidates = raw["candidates"].as_array().cloned().unwrap_or_default();
        let named_target = (bindings.len() == 1).then(|| *bindings.iter().next().unwrap());
        let another_named_entity = |text: &str| {
            let names = crate::lexical::entity_hints(text, &entities);
            named_target.is_some_and(|target| !names.is_empty() && !names.contains_key(target))
        };
        if named_target.is_some() {
            // ponytail: filtering after FTS top-k can crowd out evidence; move it into indexed search if measured.
            // Mixed-entity sources need explicit segment links if lexical names cannot establish a subject.
            let before = inferred_candidates.len();
            inferred_candidates.retain(|candidate| {
                let source_text = candidate["source_id"]
                    .as_str()
                    .and_then(|id| state.sources.get(id))
                    .and_then(crate::lexical::body);
                !another_named_entity(candidate["text"].as_str().unwrap_or(""))
                    && source_text.is_none_or(|text| !another_named_entity(text))
            });
            assistance_search["candidate_count"] = json!(inferred_candidates.len());
            assistance_search["filtered_other_named_entity"] =
                json!(before - inferred_candidates.len());
        }
        crate::diagnostics::record("assistance_candidates", || {
            let refs = |candidates: &[Value]| {
                candidates
                    .iter()
                    .map(|candidate| {
                        json!({"source_id":candidate["source_id"],"turn_id":candidate["turn_id"]})
                    })
                    .collect::<Vec<_>>()
            };
            json!({"stage":"pre_filter","original":refs(&original_candidates),
                "supplemental":refs(&inferred_candidates)})
        });
        let allowed = |m: &Memory| {
            m.status != MemoryStatus::Deleted
                && !blocked_current_ids.contains(&m.id)
                && task.is_none_or(|t| {
                    m.scope
                        .conditions
                        .iter()
                        .all(|c| !matches!(c, ScopeCondition::Task(id) if id.0 != t))
                })
        };
        let global = |m: &Memory| {
            m.subject_id == EntityId::from("self")
                && m.scope.domain.is_none()
                && m.scope.conditions.is_empty()
                && ((m.memory_type == MemoryType::Constraint
                    && m.retention_tier == RetentionTier::Durable)
                    || matches!(
                        m.predicate.0.as_str(),
                        "response_language" | "response_style"
                    ))
        };
        let subject_matches = |m: &Memory| {
            if let Some(question) = &fact_question {
                return m.subject_id == question.subject_id;
            }
            if verbal_relation.is_some() {
                return m.subject_id == EntityId::from("self");
            }
            if !unknown_subjects.is_empty() {
                return false;
            }
            target_ids.is_empty() || target_ids.contains(&m.subject_id)
        };
        let relation_matches = |m: &Memory| {
            if let Some(hint) = single_relation_hint {
                return m.id == hint.memory_id;
            }
            if let Some(question) = &fact_question
                && m.evidence.iter().any(|evidence| {
                    crate::fact_answer::verify(question, m, &evidence.quote, &entities).is_some()
                })
            {
                return true;
            }
            if let Some(properties) = &compared_properties {
                return properties.iter().any(|property| {
                    m.predicate.0 == *property
                        && (m.evidence.iter().any(|evidence| {
                            direct_property_assertion(&evidence.quote, &target_names, property)
                        }) || comparison_hints
                            .iter()
                            .any(|hint| hint.query_relation == *property && hint.memory_id == m.id))
                });
            }
            if technical_choice
                && m.evidence.iter().any(|evidence| {
                    direct_technical_choice_assertion(&evidence.quote, &target_names, m)
                })
            {
                return true;
            }
            if let Some(verb) = verbal_relation {
                return m.subject_id == EntityId::from("self")
                    && m.evidence
                        .iter()
                        .any(|evidence| direct_verb_assertion(&evidence.quote, verb));
            }
            if let Some(event) = causal_relation.as_deref() {
                return m
                    .evidence
                    .iter()
                    .any(|evidence| direct_cause_assertion(&evidence.quote, event, &m.value));
            }
            if let Some(event) = time_relation.as_deref() {
                return m
                    .evidence
                    .iter()
                    .any(|evidence| direct_time_assertion(&evidence.quote, event, &m.value));
            }
            if answer_question && structured_property.is_none() {
                return false;
            }
            structured_property.as_ref().is_none_or(|property| {
                if requested_property.is_some() {
                    m.evidence.iter().any(|evidence| {
                        direct_property_assertion(&evidence.quote, &target_names, property)
                    })
                } else {
                    literal_phrase(&m.predicate.0, property)
                        || m.evidence.iter().any(|evidence| {
                            direct_property_assertion(&evidence.quote, &target_names, property)
                        })
                }
            })
        };
        let oldest_committed_subject_source = (compared_properties.is_some()
            || fact_question.is_some())
        .then(|| {
            state
                .sources
                .values()
                .filter(|source| {
                    source.enrichment == "committed"
                        && task.is_none_or(|id| crate::lexical::task(source) == Some(id))
                        && crate::lexical::body(source).is_some_and(|body| {
                            target_names.iter().any(|name| literal_phrase(body, name))
                        })
                })
                .map(|source| source.received_sequence)
                .min()
        })
        .flatten();
        let pending_relation_matches = |body: &str| {
            if compared_properties.is_some() {
                target_names.iter().any(|name| literal_phrase(body, name))
            } else {
                fact_question.as_ref().is_some_and(|question| {
                    newer_comparison_relation(body, &target_names, &question.relation)
                        || crate::fact_answer::pending_relation_matches(question, body, &entities)
                })
            }
        };
        let pending_subject_source = oldest_committed_subject_source.is_some_and(|oldest| {
            state.sources.values().any(|pending| {
                pending.received_sequence > oldest
                    && pending.enrichment != "committed"
                    && task.is_none_or(|id| crate::lexical::task(pending) == Some(id))
                    && crate::lexical::body(pending).is_some_and(pending_relation_matches)
            })
        });
        let pending_revision_affects_query = pending_subject_source
            || memories.iter().any(|memory| {
                blocked_current_ids.contains(&memory.id)
                    && (global(memory) || subject_matches(memory) && relation_matches(memory))
                    && matches!(
                        decisions.get(&memory.id),
                        Some(RetrievalDecision::Mandatory | RetrievalDecision::Selected)
                    )
            });
        // Check all applicable peers before FTS and output quotas can hide another answer.
        let fact_values: BTreeSet<_> = fact_question
            .as_ref()
            .into_iter()
            .flat_map(|question| {
                memories.iter().filter_map(|memory| {
                    if !allowed(memory)
                        || !matches!(
                            decisions.get(&memory.id),
                            Some(RetrievalDecision::Mandatory | RetrievalDecision::Selected)
                        )
                    {
                        return None;
                    }
                    memory.evidence.iter().find_map(|evidence| {
                        crate::fact_answer::verify(question, memory, &evidence.quote, &entities)
                            .map(|answer| answer.value)
                    })
                })
            })
            .collect();
        let content = |m: &Memory| if global(m) { json!([m, m]) } else { json!(m) };
        let relevant = |m: &Memory| {
            (global(m) || !cannot_answer)
                && allowed(m)
                && (global(m)
                    || subject_matches(m)
                        && relation_matches(m)
                        && (m.memory_type == MemoryType::Constraint
                            || crate::lexical::matches_phrases(
                                &format!(
                                    "{:?} {}",
                                    m.value,
                                    m.evidence
                                        .iter()
                                        .map(|e| e.quote.as_str())
                                        .collect::<Vec<_>>()
                                        .join(" ")
                                ),
                                &phrases,
                            )))
        };
        let inferred_relevant = |m: &Memory| {
            let text = format!(
                "{:?} {}",
                m.value,
                m.evidence
                    .iter()
                    .map(|e| e.quote.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            !cannot_answer
                && allowed(m)
                && !global(m)
                && subject_matches(m)
                && relation_matches(m)
                && m.memory_type != MemoryType::Constraint
                && !named_target.is_some_and(|target| {
                    m.subject_id != *target && entities.iter().any(|e| e.id == m.subject_id)
                })
                && !another_named_entity(&text)
                && crate::lexical::matches_phrases(&text, &inferred_phrases)
        };
        let mut historical = Vec::new();
        let mut inferred_historical = Vec::new();
        for m in memories.iter().filter(|m| relevant(m)) {
            if let Some(RetrievalDecision::Conflict { .. }) = decisions.get(&m.id) {
                historical.push(json!({"kind":"conflict","memory":m,"decision":decisions[&m.id]}));
            }
        }
        for m in memories
            .iter()
            .filter(|m| !relevant(m) && inferred_relevant(m))
        {
            if let Some(RetrievalDecision::Conflict { .. }) = decisions.get(&m.id) {
                inferred_historical
                    .push(json!({"kind":"conflict","memory":m,"decision":decisions[&m.id]}));
            }
        }
        let mut exclusions = Vec::new();
        let mut remaining = budget.content_bytes;
        let mut used = 0;
        let mut coverage = true;
        let mut keep = |value: &Value, reference: Value, mandatory: bool| {
            let size = serde_json::to_vec(value).unwrap().len();
            if size <= remaining {
                remaining -= size;
                used += size;
                true
            } else {
                coverage = false;
                exclusions.push(json!({"reference":reference,"reason":"content_budget","required":mandatory,"bytes":size}));
                false
            }
        };
        result
            .mandatory
            .retain(|m| relevant(m) && keep(&content(m), json!({"memory_id":m.id}), true));
        historical.retain(|v| keep(v, json!({"memory_id":v["memory"]["id"]}), true));
        let mut count = 0;
        let mut structured_exclusions = Vec::new();
        let mut optional_structured = Vec::new();
        let mut inferred_structured = Vec::new();
        for lane in [
            &mut result.constraints,
            &mut result.preferences,
            &mut result.facts,
            &mut result.episodes,
        ] {
            lane.retain(|m| {
                if !relevant(m) {
                    if inferred_relevant(m) {
                        inferred_structured.push(m.clone());
                    } else {
                        structured_exclusions.push(
                            json!({"reference":{"memory_id":m.id},"reason":"relevance_or_ambiguity"}),
                        );
                    }
                    return false;
                }
                if m.memory_type != MemoryType::Constraint && !global(m) {
                    optional_structured.push(m.clone());
                    return false;
                }
                if count == trusted.limit {
                    structured_exclusions
                        .push(json!({"reference":{"memory_id":m.id},"reason":"structured_limit"}));
                    return false;
                }
                if !keep(&content(m), json!({"memory_id":m.id}), true) {
                    return false;
                }
                count += 1;
                true
            });
        }
        let mut sources = Vec::new();
        let mut selected_sources = 0;
        let mut seen = BTreeSet::new();
        let candidates = original_candidates;
        let mut source_decisions = Vec::new();
        let mut eligible_sources = Vec::new();
        let mut eligible_inferred_sources = Vec::new();
        let target_types: Vec<_> = entities
            .iter()
            .filter(|entity| target_ids.contains(&entity.id))
            .map(|entity| entity.entity_type.clone())
            .collect();
        for (mut candidate, is_inferred) in candidates
            .into_iter()
            .map(|candidate| (candidate, false))
            .chain(
                inferred_candidates
                    .into_iter()
                    .map(|candidate| (candidate, true)),
            )
        {
            // Diagnostic matches stay in lexical_raw; they must not consume evidence budget.
            candidate
                .as_object_mut()
                .unwrap()
                .remove("matched_query_phrases");
            candidate.as_object_mut().unwrap().remove("ranking_signals");
            let id = candidate["segment_id"].clone();
            let text = candidate["text"].as_str().unwrap_or("").to_owned();
            let mut channels = if is_inferred {
                vec!["llm_inferred"]
            } else if crate::lexical::matches_phrases(&text, &original_phrases) {
                vec!["lexical_original"]
            } else {
                Vec::new()
            };
            if !is_inferred && crate::lexical::matches_phrases(&text, &context_phrases) {
                channels.push("explicit_context");
            }
            if candidate["fact_hint_matches"]
                .as_array()
                .is_some_and(|hints| !hints.is_empty())
            {
                channels.push("committed_fact_hint");
            }
            candidate["channels"] = json!(channels);
            let links = candidate["known_memories"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let stale_link = links.iter().any(|link| {
                memories.iter().any(|memory| {
                    json!(memory.id) == link["memory_id"]
                        && blocked_current_ids.contains(&memory.id)
                })
            });
            let mut annotations = Vec::new();
            let mut has_applicable = false;
            for link in &links {
                let Some(m) = memories
                    .iter()
                    .find(|m| json!(m.id) == link["memory_id"] && allowed(m))
                else {
                    continue;
                };
                let decision = decisions.get(&m.id);
                has_applicable |= matches!(
                    decision,
                    Some(RetrievalDecision::Mandatory | RetrievalDecision::Selected)
                );
                let replacement = m
                    .superseded_by
                    .and_then(|id| memories.iter().find(|n| n.id == id && allowed(n)));
                let mut annotation = json!({"memory_id":m.id,"status":m.status,"supersede_reason":m.supersede_reason,
                    "association":link["association"],"quote_byte_range":link["quote_byte_range"],"decision":decision,
                    "replacement":replacement,"interpretation":"applies only to linked assertion; other text remains unverified"});
                crate::commit::attach_update(&mut annotation, &record_updates, m.id);
                annotations.push(annotation);
            }
            let restricted = !trusted.entities.is_empty()
                || !trusted.predicates.is_empty()
                || trusted.temporal_filter.is_some()
                || trusted.domain.is_some()
                || !runtime.conditions.is_empty();
            let source_text = candidate["source_id"]
                .as_str()
                .and_then(|source_id| state.sources.get(source_id))
                .and_then(crate::lexical::body)
                .unwrap_or(&text);
            if answer_question {
                if cannot_answer {
                    source_decisions.push(json!({"segment_id":id,"decision":"filtered",
                        "reason":if needs_clarification {"unbound_reference"} else if default_setting_overridden {"default_setting_overridden"} else {"ambiguous_reference"}}));
                    continue;
                }
                if candidate["mapping_uncertain"] != false && !links.is_empty() {
                    source_decisions.push(json!({"segment_id":id,"decision":"filtered","reason":"uncertain_memory_mapping"}));
                    continue;
                }
                if candidate["enrichment"] != "committed" {
                    source_decisions.push(json!({"segment_id":id,"decision":"filtered","reason":"source_not_committed"}));
                    continue;
                }
                let newer_uncommitted_subject_source = (compared_properties.is_some()
                    || fact_question.is_some())
                    && state.sources.values().any(|source| {
                        source.received_sequence
                            > candidate["received_sequence"].as_u64().unwrap_or(0)
                            && source.enrichment != "committed"
                            && task.is_none_or(|id| crate::lexical::task(source) == Some(id))
                            && crate::lexical::body(source).is_some_and(pending_relation_matches)
                    });
                if newer_uncommitted_subject_source {
                    source_decisions.push(json!({"segment_id":id,"decision":"filtered","reason":"newer_uncommitted_subject_source"}));
                    continue;
                }
                let mut linked = 0;
                for link in &links {
                    if link["association"] != "exact_quote" {
                        continue;
                    }
                    let Some(m) = memories.iter().find(|m| {
                        json!(m.id) == link["memory_id"]
                            && allowed(m)
                            && subject_matches(m)
                            && relation_matches(m)
                            && matches!(
                                decisions.get(&m.id),
                                Some(RetrievalDecision::Mandatory | RetrievalDecision::Selected)
                            )
                    }) else {
                        continue;
                    };
                    let Some((start, end)) = linked_quote_range(link) else {
                        continue;
                    };
                    if links.iter().any(|other| {
                        let blocked = memories.iter().any(|memory| {
                            json!(memory.id) == other["memory_id"]
                                && blocked_current_ids.contains(&memory.id)
                        });
                        blocked
                            && (other["association"] != "exact_quote"
                                || linked_quote_range(other).is_none_or(
                                    |(other_start, other_end)| {
                                        other_start < end && start < other_end
                                    },
                                ))
                    }) {
                        continue;
                    }
                    let Some(quote) = source_text.get(start..end) else {
                        continue;
                    };
                    if compared_properties.is_some()
                        && state.sources.values().any(|newer| {
                            newer.received_sequence
                                > candidate["received_sequence"].as_u64().unwrap_or(0)
                                && task.is_none_or(|id| crate::lexical::task(newer) == Some(id))
                                && crate::lexical::body(newer).is_some_and(|body| {
                                    newer_comparison_relation(body, &target_names, &m.predicate.0)
                                })
                        })
                    {
                        continue;
                    }
                    let literal_comparison_property =
                        compared_properties.as_ref().and_then(|properties| {
                            properties
                                .iter()
                                .find(|property| {
                                    m.predicate.0 == **property
                                        && linked_comparison_assertion(
                                            quote,
                                            &target_names,
                                            property,
                                            property,
                                            m,
                                        )
                                })
                                .map(String::as_str)
                        });
                    let proposed_comparison_property = comparison_hints.iter().find_map(|hint| {
                        (hint.memory_id == m.id
                            && candidate["source_id"] == hint.source_id
                            && linked_comparison_assertion(
                                quote,
                                &target_names,
                                &hint.query_relation,
                                &hint.evidence_relation,
                                m,
                            ))
                        .then_some(hint.query_relation.as_str())
                    });
                    let proposed_single_relation = single_relation_hint.is_some_and(|hint| {
                        hint.memory_id == m.id
                            && candidate["source_id"] == hint.source_id
                            && matches!(&m.value, MemoryValue::Text(value) if
                                single_relation_family(&hint.query_relation, &m.predicate.0, &hint.evidence_relation)
                                && direct_single_relation_assertion(quote, &entities, &m.subject_id, &hint.evidence_relation, value))
                    });
                    let verified_fact = fact_question
                        .as_ref()
                        .filter(|_| crate::fact_answer::complete_assertion(source_text, quote))
                        .and_then(|question| {
                            crate::fact_answer::verify(question, m, quote, &entities)
                        });
                    let verified_relation = structured_property.as_ref().is_some_and(|property| {
                        direct_property_assertion(quote, &target_names, property)
                    }) || literal_comparison_property.is_some()
                        || proposed_comparison_property.is_some()
                        || proposed_single_relation
                        || verified_fact.is_some()
                        || (technical_choice
                            && direct_technical_choice_assertion(quote, &target_names, m))
                        || verbal_relation.is_some_and(|verb| direct_verb_assertion(quote, verb))
                        || causal_relation
                            .as_deref()
                            .is_some_and(|event| direct_cause_assertion(quote, event, &m.value))
                        || time_relation
                            .as_deref()
                            .is_some_and(|event| direct_time_assertion(quote, event, &m.value));
                    if !verified_relation {
                        continue;
                    }
                    if target_ids.len() == 1
                        && crate::lexical::entity_hints(quote, &entities)
                            .keys()
                            .any(|id| {
                                !target_ids.contains(id)
                                    && entities.iter().any(|entity| {
                                        entity.id == *id
                                            && target_types.contains(&entity.entity_type)
                                    })
                            })
                    {
                        continue;
                    }
                    if !seen.insert((candidate["source_id"].to_string(), start.to_string())) {
                        continue;
                    }
                    let mut verified = candidate.clone();
                    verified["text"] = json!(quote);
                    verified["byte_start"] = json!(start);
                    verified["byte_end"] = json!(end);
                    verified["evidence_kind"] = json!("source_linked_quote");
                    verified["semantic_status"] = json!(if proposed_comparison_property.is_some()
                        || proposed_single_relation
                    {
                        "model_proposed_relation_checked_against_committed_fact"
                    } else {
                        "literal_relation_checked"
                    });
                    if proposed_single_relation
                        && let (Some(hint), MemoryValue::Text(value), Some(subject)) = (
                            single_relation_hint,
                            &m.value,
                            entities.iter().find(|entity| entity.id == m.subject_id),
                        )
                    {
                        verified["verified_identity"] = json!({"memory_id":m.id,
                            "subject":subject.canonical_name,"value":value,
                            "query_relation":hint.query_relation,"evidence_relation":hint.evidence_relation});
                    }
                    // 更新判定待定的记录不能被核准为快照中唯一的当前值（contracts 1.6）。
                    let update_pending = record_updates
                        .get(&m.id)
                        .is_some_and(|update| update.status == crate::UpdateStatus::Pending);
                    if let Some(answer) = verified_fact
                        && fact_values.len() == 1
                        && fact_values.contains(&answer.value)
                        && !pending_revision_affects_query
                        && !update_pending
                    {
                        verified["verified_fact"] = json!({"memory_id":m.id,
                            "subject":answer.subject,"relation":answer.relation,
                            "value":answer.value,"unique_in_snapshot":true});
                    }
                    if let Some(property) =
                        literal_comparison_property.or(proposed_comparison_property)
                    {
                        verified["comparison_role"] = json!(property);
                    }
                    let mut annotation = json!({"memory_id":m.id,"decision":decisions.get(&m.id),
                        "association":"exact_quote","quote_byte_range":[start,end]});
                    crate::commit::attach_update(&mut annotation, &record_updates, m.id);
                    verified["state_annotations"] = json!([annotation]);
                    verified["instruction_authority"] = json!("none");
                    verified.as_object_mut().unwrap().remove("known_memories");
                    if is_inferred {
                        eligible_inferred_sources.push((verified, false, id.clone()));
                    } else {
                        eligible_sources.push((verified, false, id.clone()));
                    }
                    linked += 1;
                }
                if linked == 0 {
                    source_decisions.push(json!({"segment_id":id,"decision":"filtered","reason":if stale_link {"uncommitted_newer_revision"} else {"no_verified_relation"}}));
                }
                continue;
            }
            let source_hints = crate::lexical::entity_hints(source_text, &entities);
            let segment_hints = crate::lexical::entity_hints(&text, &entities);
            let source_has_target = source_hints.keys().any(|id| target_ids.contains(id))
                || unknown_subjects
                    .iter()
                    .any(|name| literal_phrase(source_text, name));
            let segment_has_target = segment_hints.keys().any(|id| target_ids.contains(id))
                || unknown_subjects
                    .iter()
                    .any(|name| literal_phrase(&text, name));
            let other_peer_in_source = target_ids.len() == 1
                && source_hints.keys().any(|id| {
                    !target_ids.contains(id)
                        && entities.iter().any(|entity| {
                            entity.id == *id && target_types.contains(&entity.entity_type)
                        })
                });
            let subject_mismatch = (!target_ids.is_empty() || !unknown_subjects.is_empty())
                && (!source_has_target || !segment_has_target || other_peer_in_source);
            let property_mismatch = requested_property
                .as_ref()
                .is_some_and(|property| !direct_property_assertion(&text, &target_names, property));
            let reason = if needs_clarification {
                Some("unbound_reference")
            } else if ambiguous {
                Some("ambiguous_reference")
            } else if subject_mismatch {
                Some("subject_mismatch")
            } else if property_mismatch {
                Some("requested_property_absent")
            } else if restricted
                && (!has_applicable
                    || candidate["mapping_uncertain"] != false
                    || annotations
                        .iter()
                        .any(|a| !matches!(a["decision"].as_str(), Some("Selected" | "Mandatory")))
                    || !annotations.iter().any(|a| {
                        a["quote_byte_range"]
                            == json!([candidate["byte_start"], candidate["byte_end"]])
                    }))
            {
                Some("unverified_hard_conditions")
            } else if !seen.insert((
                candidate["source_id"].to_string(),
                candidate["byte_start"].to_string(),
            )) {
                Some("duplicate_segment")
            } else {
                None
            };
            if let Some(reason) = reason {
                source_decisions
                    .push(json!({"segment_id":id,"decision":"filtered","reason":reason}));
                continue;
            }
            let flagged = stale_link
                || annotations
                    .iter()
                    .any(|a| !matches!(a["decision"].as_str(), Some("Selected" | "Mandatory")));
            candidate["state_annotations"] = json!(annotations);
            candidate["semantic_status"] = json!(if flagged {
                "mixed_or_historical_do_not_treat_as_current"
            } else {
                "unverified_source_statement"
            });
            candidate["instruction_authority"] = json!("none");
            // Do not repeat the same metadata in known_memories and the policy annotations.
            candidate.as_object_mut().unwrap().remove("known_memories");
            let wrapped = if flagged {
                json!({"kind":"source_with_state_caveats","source":candidate})
            } else {
                candidate.clone()
            };
            if is_inferred {
                eligible_inferred_sources.push((wrapped, flagged, id));
            } else {
                eligible_sources.push((wrapped, flagged, id));
            }
        }
        // Mandatory rules and conflicts precede both evidence channels. Alternate whole items
        // so optional structured background cannot consume the raw channel's entire budget.
        let source_asserts = |source: &Value, property: &str| source["comparison_role"] == property;
        for (phase, (mut raw_queue, mut structured_queue)) in [
            (
                eligible_sources.into_iter(),
                optional_structured.into_iter(),
            ),
            (
                eligible_inferred_sources.into_iter(),
                inferred_structured.into_iter(),
            ),
        ]
        .into_iter()
        .enumerate()
        {
            if phase == 1 {
                // Original local evidence has already claimed its quota. A soft query may
                // expose a conflict, but it cannot displace the original historical lane.
                inferred_historical
                    .retain(|v| keep(v, json!({"memory_id":v["memory"]["id"]}), false));
                historical.append(&mut inferred_historical);
            }
            while raw_queue.len() != 0 || structured_queue.len() != 0 {
                for (wrapped, flagged, id) in raw_queue.by_ref() {
                    if compared_properties.as_ref().is_some_and(|properties| {
                        !properties.iter().any(|property| {
                            source_asserts(&wrapped, property)
                                && !sources
                                    .iter()
                                    .any(|source| source_asserts(source, property))
                        })
                    }) {
                        source_decisions.push(json!({"segment_id":id,"decision":"filtered","reason":"comparison_relation_already_covered"}));
                    } else if selected_sources >= budget.source_limit {
                        source_decisions.push(
                            json!({"segment_id":id,"decision":"filtered","reason":"source_limit"}),
                        );
                    } else if keep(&wrapped, json!({"segment_id":id}), false) {
                        selected_sources += 1;
                        source_decisions.push(json!({"segment_id":id,"decision":"selected","lane":if flagged {"historical_or_conflicting"}else{"source_evidence"}}));
                        if flagged {
                            historical.push(wrapped);
                        } else {
                            sources.push(wrapped);
                        }
                        break;
                    }
                }
                for memory in structured_queue.by_ref() {
                    if count == trusted.limit {
                        structured_exclusions.push(
                            json!({"reference":{"memory_id":memory.id},"reason":"structured_limit"}),
                        );
                    } else if keep(&content(&memory), json!({"memory_id":memory.id}), false) {
                        count += 1;
                        match memory.memory_type {
                            MemoryType::Preference => result.preferences.push(memory),
                            MemoryType::Fact => result.facts.push(memory),
                            MemoryType::Episode => result.episodes.push(memory),
                            MemoryType::Constraint => {
                                unreachable!("constraints were selected first")
                            }
                        }
                        break;
                    }
                }
            }
        }
        let missing_comparison_relations: Vec<_> = compared_properties
            .iter()
            .flatten()
            .filter(|property| {
                !sources
                    .iter()
                    .any(|source| source_asserts(source, property))
            })
            .cloned()
            .collect();
        let comparison_incomplete = !missing_comparison_relations.is_empty();
        if comparison_incomplete {
            for decision in &mut source_decisions {
                if decision["decision"] == "selected" {
                    decision["decision"] = json!("filtered");
                    decision["reason"] = json!("incomplete_comparison");
                    decision.as_object_mut().unwrap().remove("lane");
                }
            }
            sources.clear();
            historical.clear();
            for lane in [
                &mut result.mandatory,
                &mut result.constraints,
                &mut result.preferences,
                &mut result.facts,
                &mut result.episodes,
            ] {
                lane.retain(&global);
            }
            result.conflicts.clear();
        }
        if structured_exclusions
            .iter()
            .any(|e| e["reason"] == "structured_limit")
        {
            coverage = false;
        }
        exclusions.extend(structured_exclusions);
        let selected_ids: BTreeSet<_> = result
            .mandatory
            .iter()
            .chain(&result.constraints)
            .chain(&result.preferences)
            .chain(&result.facts)
            .chain(&result.episodes)
            .map(|m| m.id)
            .collect();
        for entry in &mut result.trace {
            if matches!(
                entry.decision,
                RetrievalDecision::Mandatory | RetrievalDecision::Selected
            ) && !selected_ids.contains(&entry.memory_id)
            {
                entry.decision = RetrievalDecision::Filtered(RetrievalFilterReason::LocalSelection);
            }
        }
        if source_decisions
            .iter()
            .any(|d| d["reason"] == "source_limit")
        {
            coverage = false;
        }
        let mut degraded = Vec::new();
        if incomplete_enrichment {
            degraded.push("structured_enrichment_incomplete");
        }
        if pending_revision_affects_query {
            degraded.push("uncommitted_newer_revision");
        }
        if raw["status"] != "completed" {
            degraded.push("lexical_coverage_incomplete");
        }
        if self.fact_index_enabled && raw["fact_index"]["status"] != "ready" {
            degraded.push("fact_index_unavailable");
        }
        if !coverage {
            degraded.push("context_budget_incomplete");
        }
        if ambiguous {
            degraded.push("ambiguous_reference");
        }
        if needs_clarification {
            degraded.push("clarification_required");
        }
        if comparison_incomplete {
            degraded.push("comparison_evidence_incomplete");
        }
        if raw["candidate_count"] == budget.candidate_k {
            degraded.push("candidate_quota_reached");
        }
        let mut provenance = json!([
            {"fields":["owner","task","at","runtime_filters"],"source":"trusted_runtime","strength":"hard","value":trusted},
            {"fields":["query","context"],"source":"explicit_user_validated","strength":"search_text_only","query":supplied_query,"context":supplied_context},
            {"fields":["aliases","bigrams"],"source":"deterministic_parse","strength":"soft","version":"natural-v2"},
            {"fields":["unbound_subject","unparsed_time","unparsed_scope"],"source":"unknown","strength":"none"}
        ]);
        if !query_conditions.is_empty() {
            provenance.as_array_mut().unwrap().push(json!({
                "fields":["activity"],"source":"explicit_user_activity_with_source_checked_aliases",
                "strength":"hard","query":query,
                "activity_span":fact_question.as_ref().and_then(|question| question.activity.as_deref()),
                "conditions":query_conditions
            }));
        }
        if let Some(setting) = fact_question
            .as_ref()
            .and_then(|question| question.default_setting.as_ref())
        {
            provenance.as_array_mut().unwrap().push(json!({
                "fields":["default_setting"],"source":"explicit_unchanged_setting_requires_default_assertion",
                "strength":"certificate_only","setting":setting,"query":query,"current_override":default_setting_overridden
            }));
        }
        if joined.is_some() {
            provenance.as_array_mut().unwrap().insert(
                2,
                json!({"fields":["effective_query","effective_context"],
                "source":"deterministic_parse","strength":"soft","query":query,"context":context}),
            );
        }
        result.conflicts.retain(|c| {
            memories.iter().any(|m| {
                (m.id == c.left || m.id == c.right) && (relevant(m) || inferred_relevant(m))
            })
        });
        crate::diagnostics::record(
            "retrieval",
            || json!({"runtime":runtime,"result":result,"memories":memories,"conditions_provenance":provenance}),
        );
        result.trace.clear();
        let mut global_result = result.clone();
        for lane in [
            &mut global_result.mandatory,
            &mut global_result.constraints,
            &mut global_result.preferences,
            &mut global_result.facts,
            &mut global_result.episodes,
        ] {
            lane.retain(&global);
        }
        global_result.conflicts.clear();
        raw["source_decisions"] = json!(source_decisions);
        crate::diagnostics::record("lexical_raw", || raw.clone());
        crate::diagnostics::record(
            "local_selection_exclusions",
            || json!({"excluded":exclusions}),
        );
        let excluded_count = exclusions.len();
        exclusions.truncate(50);
        let output = json!({"result":result,
            "global_context":{"status":"available","source":"local_read_snapshot","read_revision":read_revision,
                "coverage_complete":coverage,"result":global_result},
            "source_evidence":sources,"historical_or_conflicting":historical,
            "retrieval_mode":"local","recall_status":"completed","read_revision":read_revision,"source_revision":source_revision,
            "index_status":raw["status"],"degraded":!degraded.is_empty(),"degraded_reasons":degraded,
            "fact_index":raw["fact_index"],
            "comparison_evidence":compared_properties.as_ref().map(|relations| json!({
                "required_relations":relations,"missing_relations":missing_comparison_relations})),
            "evidence_status":if ambiguous {"ambiguous"} else if needs_clarification {"clarification_required"} else if default_setting_overridden {"default_setting_overridden"} else if pending_revision_affects_query {"uncommitted_newer_revision"} else {"sufficiency_not_verified"},
            "reference_resolution":reference_resolution,
            "conditions_provenance":provenance,"backend_generation_calls_in_recall":0,
            "budget":{"config":budget,"content_bytes":used,"unit":"serialized_utf8_bytes_not_tokens","coverage_complete":coverage,"excluded_count":excluded_count,"excluded":exclusions},
            "source_decisions":source_decisions,"duration_ms":started.elapsed().as_millis()});
        let mut output = output;
        output["assistance_search"] = assistance_search;
        crate::diagnostics::record("local_recall", || output.clone());
        Ok(output)
    }
}
