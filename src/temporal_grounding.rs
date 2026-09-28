//! Conservative local grounding of model-supplied temporal metadata.
//! Unrecognized syntax is rejected rather than promoted to a calendar boundary.

use crate::{
    CandidateMemory, CandidateSubject, DatedValue, Entity, MemoryType, MemoryValue, RetentionTier,
    ScopeCondition, TemporalReference, TemporalResolution, TimeScope,
};

const UNSUPPORTED: &str = "temporal metadata lacks locally grounded USER support";

pub(crate) fn validate(
    candidate: &CandidateMemory,
    current_user_turn: &str,
    known_entities: &[Entity],
    utc_offset_seconds: i32,
) -> Result<(), &'static str> {
    let t = &candidate.temporal;
    let dates: Vec<_> = candidate
        .scope
        .conditions
        .iter()
        .filter_map(|c| match c {
            ScopeCondition::Time(TimeScope::Date(date)) => Some(date.as_str()),
            _ => None,
        })
        .collect();
    if t.valid_from.is_none()
        && t.valid_to.is_none()
        && t.reference.is_none()
        && t.expires_at.is_none()
        && dates.is_empty()
    {
        return Ok(());
    }
    let sentence = assertion(candidate, current_user_turn, known_entities).ok_or(UNSUPPORTED)?;
    let text = unquoted(sentence).ok_or(UNSUPPORTED)?;
    if [
        "不是",
        "并非",
        "不在",
        "没有说",
        "未说",
        "不从",
        "未从",
        "并未",
        "假设",
        "假如",
        "如果",
        "据说",
        "有人说",
        "他说",
        "她说",
        "我说",
        "听说",
        "引用",
        "提到",
        "且",
        "但是",
        "而是",
    ]
    .iter()
    .any(|word| text.contains(word))
    {
        return Err(UNSUPPORTED);
    }
    let aliases = subject_names(candidate, known_entities);
    // ponytail: only explicit frames and this finite vocabulary are grounded locally.
    // Reject complex clauses; extend evidenced syntax only when a real case needs it.
    let mut phrases: Vec<String> = [
        "从今以后",
        "最近刚开始",
        "最近开始",
        "以后",
        "今后",
        "现在",
        "目前",
        "当前",
        "最近",
        "下个月",
        "今天",
        "今日",
        "昨天",
        "明天",
        "后天",
        "去年",
        "以前",
        "过去",
        "曾经",
        "不久前",
        "前阵子",
        "近日",
        "这次",
        "本次",
        "等合适的时候",
        "from now on",
        "recently started",
        "recently",
        "next month",
        "last year",
        "today",
        "yesterday",
        "tomorrow",
        "now",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    for (start, _) in text.char_indices() {
        // Reuse the complete day grammar; longest spans win below (大前天 before 前天).
        // The finite bound covers named phrases and u32 day counts, not arbitrary NLP.
        for (relative_end, character) in text[start..].char_indices().take(32) {
            let end = start + relative_end + character.len_utf8();
            if crate::temporal::relative_day_offset(&text[start..end]).is_some() {
                phrases.push(text[start..end].to_owned());
            }
        }
        if let Some(date) = text.get(start..start + 10)
            && crate::temporal::is_valid_iso_date(date)
        {
            phrases.push(date.to_owned());
        }
        if text[start..].starts_with("去年") {
            let tail = &text[start..];
            if let Some(end) = tail.find('月')
                && tail[..end].chars().count() <= 6
            {
                phrases.push(tail[..end + '月'.len_utf8()].to_owned());
            }
        }
    }
    if let Some(TemporalReference::Event {
        event, relation, ..
    }) = &t.reference
    {
        phrases.push(format!(
            "{event}{}",
            match relation {
                crate::TemporalRelation::AfterEnd => "结束后",
                crate::TemporalRelation::BeforeStart => "之前",
            }
        ));
    }
    let mut spans = Vec::new();
    for phrase in &phrases {
        for (start, _) in text.match_indices(phrase) {
            let end = start + phrase.len();
            if aliases.iter().any(|name| {
                text.match_indices(name)
                    .any(|(at, _)| start < at + name.len() && at < end)
            }) {
                continue;
            }
            if phrase.is_ascii()
                && (text[..start]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_ascii_alphanumeric())
                    || text[end..]
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_alphanumeric()))
            {
                continue;
            }
            spans.push((start, end));
        }
    }
    spans.sort_by_key(|(start, end)| (*start, std::cmp::Reverse(end - start)));
    spans.dedup();
    let mut consumed = 0;
    let (mut from, mut to, mut reference, mut expiry) = (None, None, None, None);
    let mut validity = false;
    let mut date_ranges = Vec::new();
    let mut frames = Vec::new();
    for (start, end) in spans {
        if start < consumed {
            continue;
        }
        consumed = end;
        let before = text[..start].trim_end();
        let after = text[end..].trim_start();
        let anchored_prefix = |prefix: &str| {
            prefix.is_empty()
                || aliases
                    .iter()
                    .any(|name| prefix == *name || prefix == format!("{name}的"))
        };
        let direct_subject = |body: &str| {
            aliases.iter().any(|name| {
                body.trim_start_matches(|c: char| {
                    c.is_whitespace() || matches!(c, '，' | ',' | '：' | ':')
                })
                .starts_with(name)
            })
        };
        let clause = before.rsplit(['，', ',']).next().unwrap_or(before).trim();
        let role = if ["保留到", "保存到", "记到", "保留至", "保存至"].contains(&clause)
            && after.is_empty()
            || clause.is_empty() && ["后不用记了", "后删除", "后忘记"].contains(&after)
        {
            3
        } else if ["从", "自"]
            .iter()
            .any(|s| before.strip_suffix(s).is_some_and(anchored_prefix))
            && ["起", "开始", "生效"].iter().any(|s| after.starts_with(s))
        {
            1
        } else if before.strip_suffix("截至").is_some_and(anchored_prefix)
            || before.strip_suffix('到').is_some_and(anchored_prefix) && after.starts_with("为止")
        {
            2
        } else if before.is_empty() && direct_subject(after)
            || !before.is_empty() && anchored_prefix(before)
        {
            0
        } else {
            return Err(UNSUPPORTED);
        };
        if role == 0
            && crate::temporal::relative_day_offset(&text[start..end]).is_some()
            && !direct_subject(after)
            && [
                "上午", "下午", "晚上", "早上", "中午", "凌晨", "清晨", "傍晚", "夜里", "左右",
                "前", "后", "内", "起",
            ]
            .iter()
            .any(|word| after.starts_with(word))
        {
            return Err(UNSUPPORTED);
        }
        // A comma may introduce a time frame or retention instruction, not another property.
        if role != 3 && before.contains(['，', ',']) {
            return Err(UNSUPPORTED);
        }
        if role == 1
            && matches!(before, "从" | "自")
            && !["起", "开始", "生效"]
                .iter()
                .filter_map(|word| after.strip_prefix(word))
                .any(direct_subject)
            || role == 2
                && matches!(before, "截至" | "到")
                && !direct_subject(after.strip_prefix("为止").unwrap_or(after))
        {
            return Err(UNSUPPORTED);
        }
        frames.push(match role {
            1 => (
                before.len() - '从'.len_utf8(),
                text.len() - after.len()
                    + ["起", "开始", "生效"]
                        .iter()
                        .find(|word| after.starts_with(**word))
                        .unwrap()
                        .len(),
            ),
            2 => (
                before.len()
                    - if before.ends_with("截至") {
                        "截至".len()
                    } else {
                        '到'.len_utf8()
                    },
                text.len() - after.len()
                    + if after.starts_with("为止") {
                        "为止".len()
                    } else {
                        0
                    },
            ),
            3 => (before.rfind(['，', ',']).unwrap_or(0), text.len()),
            _ => (start, end),
        });
        let resolution = crate::temporal::resolve_temporal_expression(
            &text[start..end],
            t.observed_at,
            utc_offset_seconds,
        )
        .map_err(|_| UNSUPPORTED)?;
        match resolution {
            TemporalResolution::Range(r) => {
                if role == 3 {
                    if expiry.is_some() || r.end.0 == i64::MAX || r.end <= t.observed_at {
                        return Err(UNSUPPORTED);
                    }
                    expiry = Some(r.end);
                } else {
                    if role == 0 {
                        if validity {
                            return Err(UNSUPPORTED);
                        }
                        validity = true;
                        from = Some(DatedValue {
                            at: r.start,
                            source: r.source,
                        });
                        to = Some(DatedValue {
                            at: r.end,
                            source: r.source,
                        });
                        date_ranges.push(r);
                    } else if role == 1 && from.is_none() {
                        validity = true;
                        from = Some(DatedValue {
                            at: r.start,
                            source: r.source,
                        });
                    } else if role == 2 && to.is_none() {
                        validity = true;
                        to = Some(DatedValue {
                            at: r.end,
                            source: r.source,
                        });
                    } else {
                        return Err(UNSUPPORTED);
                    }
                }
            }
            TemporalResolution::Reference(r) if role == 0 && !validity => {
                validity = true;
                reference = Some(r);
            }
            _ => return Err(UNSUPPORTED),
        }
    }
    if candidate.predicate.0 != "fact"
        && !direct_scalar_assertion(candidate, &text, &aliases, &frames)
    {
        return Err(UNSUPPORTED);
    }
    if (t.valid_from.is_some() || t.valid_to.is_some() || t.reference.is_some())
        && (!validity || t.valid_from != from || t.valid_to != to || t.reference != reference)
    {
        return Err(UNSUPPORTED);
    }
    let offset =
        time::UtcOffset::from_whole_seconds(utc_offset_seconds).map_err(|_| UNSUPPORTED)?;
    for date in &dates {
        let matched = date_ranges
            .iter()
            .find(|r| {
                r.end.0.checked_sub(r.start.0) == Some(86400)
                    && time::OffsetDateTime::from_unix_timestamp(r.start.0)
                        .ok()
                        .is_some_and(|at| at.to_offset(offset).date().to_string() == *date)
            })
            .ok_or(UNSUPPORTED)?;
        if expiry.is_none()
            && candidate.memory_type == MemoryType::Constraint
            && candidate.retention_tier == RetentionTier::ShortTerm
        {
            expiry = Some(matched.end);
        }
    }
    if t.expires_at.is_some() && t.expires_at != expiry {
        return Err(UNSUPPORTED);
    }
    Ok(())
}

fn direct_scalar_assertion(
    candidate: &CandidateMemory,
    text: &str,
    names: &[&str],
    frames: &[(usize, usize)],
) -> bool {
    let mut body = text.to_owned();
    for &(start, end) in frames.iter().rev() {
        body.replace_range(start..end, "");
    }
    let trim = |s: &str| {
        s.trim_matches(|c: char| c.is_whitespace() || matches!(c, '，' | ',' | '：' | ':'))
            .to_owned()
    };
    let body = trim(&body);
    let value = match &candidate.value {
        MemoryValue::Text(value) => value.clone(),
        MemoryValue::Integer(value) => value.to_string(),
        _ => return false,
    };
    // No translated values or open-ended relation paraphrases are authorized here.
    let relations = if candidate.predicate.0 == "lives_in" {
        vec!["常住", "常住在", "住在", "居住在", "居住于"]
    } else {
        vec![candidate.predicate.0.as_str()]
    };
    names.iter().any(|name| {
        let Some(tail) = body.strip_prefix(name) else {
            return false;
        };
        let tail = tail.trim().strip_prefix('的').unwrap_or(tail.trim()).trim();
        relations.iter().any(|relation| {
            let Some(tail) = tail.strip_prefix(relation) else {
                return false;
            };
            let tail = tail.trim();
            let tail = ["是", "为", "=", "：", ":"]
                .iter()
                .find_map(|word| tail.strip_prefix(word))
                .unwrap_or(tail);
            tail.trim() == value.trim()
        })
    })
}

fn subject_names<'a>(candidate: &'a CandidateMemory, entities: &'a [Entity]) -> Vec<&'a str> {
    match &candidate.subject {
        CandidateSubject::Resolved(id) => entities
            .iter()
            .find(|e| &e.id == id)
            .map(|e| {
                std::iter::once(e.canonical_name.as_str())
                    .chain(e.aliases.iter().map(String::as_str))
                    .filter(|n| !n.is_empty())
                    .collect()
            })
            .unwrap_or_default(),
        CandidateSubject::Unresolved(name) => vec![name],
    }
}

fn assertion<'a>(
    candidate: &'a CandidateMemory,
    current: &'a str,
    entities: &[Entity],
) -> Option<&'a str> {
    let names = subject_names(candidate, entities);
    let value = match &candidate.value {
        MemoryValue::Text(v) => v.clone(),
        MemoryValue::Integer(v) => v.to_string(),
        _ => return None,
    };
    fn trim(s: &str) -> &str {
        s.trim()
            .trim_end_matches(['。', '.', '！', '!', '？', '?', ';', '；'])
    }
    let mut start = 0;
    let mut sentences = Vec::new();
    for (at, c) in current.char_indices() {
        let end = at + c.len_utf8();
        if matches!(c, '。' | '！' | '!' | '？' | '?' | ';' | '；' | '\n' | '\r')
            || c == '.'
                && current[end..]
                    .chars()
                    .next()
                    .is_none_or(char::is_whitespace)
        {
            sentences.push(trim(&current[start..end]));
            start = end;
        }
    }
    sentences.push(trim(&current[start..]));
    let matching: Vec<_> = sentences
        .into_iter()
        .filter(|s| {
            if candidate.predicate.0 == "fact" {
                *s == trim(&value)
            } else {
                names.iter().any(|n| s.contains(*n)) && s.contains(&value)
            }
        })
        .collect();
    if matching.len() != 1 {
        return None;
    }
    let sentence = matching[0];
    // Bound the conservative syntax to one subject and one assertion.
    if entities
        .iter()
        .filter(|e| candidate.subject_id() != Some(&e.id))
        .any(|e| {
            std::iter::once(&e.canonical_name)
                .chain(&e.aliases)
                .any(|n| !n.is_empty() && sentence.contains(n))
        })
    {
        return None;
    }
    if let Some((head, tail)) = sentence.split_once(['，', ',']) {
        let temporal_prefix = crate::temporal::relative_day_offset(head).is_some()
            || crate::temporal::is_valid_iso_date(head.trim())
            || sentence.starts_with(['从', '自'])
            || [
                "今天", "明天", "昨天", "最近", "现在", "目前", "当前", "截至", "到",
            ]
            .iter()
            .any(|p| sentence.starts_with(p));
        let retention_tail = ["保留到", "保存到", "记到", "保留至", "保存至"]
            .iter()
            .any(|p| tail.trim().starts_with(p));
        if tail.contains(['，', ','])
            || !temporal_prefix && !retention_tail
            || !retention_tail && names.iter().any(|name| head.contains(name))
        {
            return None;
        }
    }
    Some(sentence)
}

fn unquoted(text: &str) -> Option<String> {
    let mut close = None;
    let mut result = String::new();
    for c in text.chars() {
        if let Some(end) = close {
            if c == end {
                close = None;
            }
            result.extend(std::iter::repeat_n(' ', c.len_utf8()));
        } else if let Some(end) = match c {
            '“' => Some('”'),
            '‘' => Some('’'),
            '「' => Some('」'),
            '『' => Some('』'),
            '《' => Some('》'),
            '"' => Some('"'),
            '\'' => Some('\''),
            _ => None,
        } {
            close = Some(end);
            result.extend(std::iter::repeat_n(' ', c.len_utf8()));
        } else {
            result.push(c);
        }
    }
    close.is_none().then_some(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        EntityId, EntityType, Evidence, EvidenceId, Predicate, SessionId, Timestamp, TurnId, UserId,
    };

    fn fixture(source: &str, value: &str) -> (CandidateMemory, Vec<Entity>) {
        let owner = UserId::from("test-owner");
        let entity = Entity {
            id: EntityId::from("event-haiyan"),
            owner_id: owner.clone(),
            entity_type: EntityType::Other("event".into()),
            canonical_name: "海盐工作坊".into(),
            aliases: vec![],
        };
        let candidate = CandidateMemory::new(
            owner,
            entity.id.clone(),
            MemoryType::Fact,
            Predicate::from("fact"),
            MemoryValue::Text(value.into()),
            Timestamp(1790514274),
            Evidence::new(
                EvidenceId::from("e1"),
                SessionId::from("s1"),
                TurnId::from("t1"),
                source,
            ),
        );
        (candidate, vec![entity])
    }

    #[test]
    fn temporal_grounding_binds_the_same_assertion_and_exact_local_range() {
        for (source, value, expression, expected) in [
            (
                "现在海盐工作坊的报名费为36元。",
                "现在海盐工作坊的报名费为36元。",
                "现在",
                true,
            ),
            (
                "明天海盐工作坊的报名费为36元。",
                "明天海盐工作坊的报名费为36元。",
                "明天",
                true,
            ),
            (
                "2026-10-02海盐工作坊的报名费为36元。",
                "2026-10-02海盐工作坊的报名费为36元。",
                "2026-10-02",
                true,
            ),
            (
                "海盐工作坊的报名费为36元。",
                "海盐工作坊的报名费为36元。",
                "现在",
                false,
            ),
            (
                "海盐工作坊的报名费为36元。明天银杏工作坊开课。",
                "海盐工作坊的报名费为36元。",
                "明天",
                false,
            ),
            (
                "海盐工作坊的报名费为36元，购买于2026-10-02。",
                "海盐工作坊的报名费为36元，购买于2026-10-02。",
                "2026-10-02",
                false,
            ),
            (
                "海盐工作坊的报名费并非从明天开始为36元。",
                "海盐工作坊的报名费并非从明天开始为36元。",
                "明天",
                false,
            ),
            (
                "“明天”是海盐工作坊的海报标题，报名费为36元。",
                "“明天”是海盐工作坊的海报标题，报名费为36元。",
                "明天",
                false,
            ),
        ] {
            let (mut candidate, entities) = fixture(source, value);
            candidate.temporal.apply_resolution(
                crate::temporal::resolve_temporal_expression(
                    expression,
                    candidate.temporal.observed_at,
                    28800,
                )
                .unwrap(),
            );
            assert_eq!(
                validate(&candidate, source, &entities, 28800).is_ok(),
                expected,
                "{source}"
            );
            if expected {
                candidate.temporal.valid_from.as_mut().unwrap().at.0 -= 1;
                assert!(validate(&candidate, source, &entities, 28800).is_err());
            }
        }
    }

    #[test]
    fn temporal_grounding_binds_complete_relative_day_phrases() {
        for phrase in [
            "前天",
            "大前天",
            "大后天",
            "三天前",
            "13天后",
            "the day before yesterday",
            "in 3 days",
        ] {
            for source in [
                format!("{phrase}海盐工作坊的报名费为36元。"),
                format!("{phrase}，海盐工作坊的报名费为36元。"),
            ] {
                let (mut candidate, entities) = fixture(&source, &source);
                candidate.temporal.apply_resolution(
                    crate::temporal::resolve_temporal_expression(
                        phrase,
                        candidate.temporal.observed_at,
                        28800,
                    )
                    .unwrap(),
                );
                assert!(
                    validate(&candidate, &source, &entities, 28800).is_ok(),
                    "{source}"
                );
            }
        }
        for (source, value, name) in [
            (
                "前天纸厂的负责人是小林。",
                "前天纸厂的负责人是小林。",
                "前天纸厂",
            ),
            (
                "前天海盐工作坊开课，报名费为36元。",
                "前天海盐工作坊开课，报名费为36元。",
                "海盐工作坊",
            ),
            (
                "海盐工作坊的报名费为36元。前天银杏工作坊开课。",
                "海盐工作坊的报名费为36元。",
                "海盐工作坊",
            ),
            (
                "前天晚上海盐工作坊的报名费为36元。",
                "前天晚上海盐工作坊的报名费为36元。",
                "海盐工作坊",
            ),
            (
                "海盐工作坊前天晚上的报名费为36元。",
                "海盐工作坊前天晚上的报名费为36元。",
                "海盐工作坊",
            ),
            (
                "海盐工作坊前天左右的报名费为36元。",
                "海盐工作坊前天左右的报名费为36元。",
                "海盐工作坊",
            ),
            (
                "大前天海盐工作坊的报名费为36元。",
                "大前天海盐工作坊的报名费为36元。",
                "海盐工作坊",
            ),
        ] {
            let (mut candidate, mut entities) = fixture(source, value);
            entities[0].canonical_name = name.into();
            candidate.temporal.apply_resolution(
                crate::temporal::resolve_temporal_expression(
                    "前天",
                    candidate.temporal.observed_at,
                    28800,
                )
                .unwrap(),
            );
            assert!(
                validate(&candidate, source, &entities, 28800).is_err(),
                "{source}"
            );
        }
    }

    #[test]
    fn temporal_grounding_keeps_retention_separate_from_validity() {
        let source = "海盐工作坊的报名费为36元，保留到明天。";
        let (mut candidate, entities) = fixture(source, source);
        let TemporalResolution::Range(range) = crate::temporal::resolve_temporal_expression(
            "明天",
            candidate.temporal.observed_at,
            28800,
        )
        .unwrap() else {
            panic!()
        };
        candidate.temporal.expires_at = Some(range.end);
        assert!(validate(&candidate, source, &entities, 28800).is_ok());
        candidate.temporal.valid_from = Some(DatedValue {
            at: range.start,
            source: range.source,
        });
        assert!(validate(&candidate, source, &entities, 28800).is_err());
        for source in [
            "海盐工作坊报名费36元海报保留到明天。",
            "海盐工作坊报名费36元，海报保留到明天。",
        ] {
            let (mut candidate, entities) = fixture(source, source);
            candidate.temporal.expires_at = Some(range.end);
            assert!(
                validate(&candidate, source, &entities, 28800).is_err(),
                "{source}"
            );
        }
    }

    #[test]
    fn temporal_grounding_preserves_event_and_unknown_references() {
        for phrase in ["旅行结束后", "以前", "等合适的时候"] {
            let source = format!("{phrase}海盐工作坊的报名费为36元。");
            let (mut candidate, entities) = fixture(&source, &source);
            candidate.temporal.apply_resolution(
                crate::temporal::resolve_temporal_expression(
                    phrase,
                    candidate.temporal.observed_at,
                    28800,
                )
                .unwrap(),
            );
            assert!(validate(&candidate, &source, &entities, 28800).is_ok());
        }
    }

    #[test]
    fn temporal_grounding_does_not_borrow_names_or_other_property_boundaries() {
        for (source, name, partial) in [
            ("现在纸厂的负责人是小林。", "现在纸厂", 0),
            ("2026-10-02纸厂的负责人是小林。", "2026-10-02纸厂", 0),
            ("海盐工作坊报名费36元开课从明天开始。", "海盐工作坊", 1),
            ("海盐工作坊报名费36元报名截至明天。", "海盐工作坊", 2),
            ("明天要讨论海盐工作坊的报名费为36元。", "海盐工作坊", 0),
        ] {
            let (mut candidate, mut entities) = fixture(source, source);
            entities[0].canonical_name = name.into();
            let expression = if name.starts_with("现在") {
                "现在"
            } else if name.starts_with("2026") {
                "2026-10-02"
            } else {
                "明天"
            };
            candidate.temporal.apply_resolution(
                crate::temporal::resolve_temporal_expression(
                    expression,
                    candidate.temporal.observed_at,
                    28800,
                )
                .unwrap(),
            );
            if partial == 1 {
                candidate.temporal.valid_to = None;
            }
            if partial == 2 {
                candidate.temporal.valid_from = None;
            }
            assert!(
                validate(&candidate, source, &entities, 28800).is_err(),
                "{source}"
            );
        }
    }

    #[test]
    fn temporal_grounding_typed_slots_reject_both_directions_of_other_assertions() {
        for (source, subject, value, partial, expected) in [
            (
                "明天海盐工作坊开课，报名费为36元。",
                "海盐工作坊",
                "36",
                0,
                false,
            ),
            (
                "海盐工作坊报名费为36元，明天开课。",
                "海盐工作坊",
                "36",
                0,
                false,
            ),
            (
                "明天，海盐工作坊的报名费为36。",
                "海盐工作坊",
                "36",
                0,
                true,
            ),
            (
                "从明天起，海盐工作坊的报名费为36。",
                "海盐工作坊",
                "36",
                1,
                true,
            ),
            (
                "截至明天，海盐工作坊的报名费为36。",
                "海盐工作坊",
                "36",
                2,
                true,
            ),
            ("明天小李去北京开会，常住上海。", "小李", "上海", 0, false),
            ("小李常住上海，明天去北京开会。", "小李", "上海", 0, false),
            (
                "从明天开始讨论小李常住上海这件事。",
                "小李",
                "上海",
                1,
                false,
            ),
            ("截至明天讨论小李常住上海这件事。", "小李", "上海", 2, false),
            ("从明天开始，小李常住上海。", "小李", "上海", 1, true),
            (
                "小李从明天开始讨论常住上海这件事。",
                "小李",
                "上海",
                1,
                false,
            ),
            ("小李明天讨论常住上海这件事。", "小李", "上海", 0, false),
            ("小李截至明天讨论常住上海这件事。", "小李", "上海", 2, false),
            ("小李从明天开始常住上海。", "小李", "上海", 1, true),
            ("小李明天常住上海。", "小李", "上海", 0, true),
            ("小李截至明天常住上海。", "小李", "上海", 2, true),
        ] {
            for integer in [false, true] {
                if integer && value != "36" {
                    continue;
                }
                let (mut candidate, mut entities) = fixture(source, value);
                entities[0].canonical_name = subject.into();
                candidate.predicate = Predicate::from(if value == "36" {
                    "报名费"
                } else {
                    "lives_in"
                });
                if integer {
                    candidate.value = MemoryValue::Integer(36);
                }
                candidate.temporal.apply_resolution(
                    crate::temporal::resolve_temporal_expression(
                        "明天",
                        candidate.temporal.observed_at,
                        28800,
                    )
                    .unwrap(),
                );
                if partial == 1 {
                    candidate.temporal.valid_to = None;
                }
                if partial == 2 {
                    candidate.temporal.valid_from = None;
                }
                assert_eq!(
                    validate(&candidate, source, &entities, 28800).is_ok(),
                    expected,
                    "{source}"
                );
            }
        }
    }
}
