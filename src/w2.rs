//! W2 只做机械核验；语义、保存授权和准入仍由宿主／W3 负责。
use std::fmt;

use time::{Date, Month, UtcOffset};

use crate::contract::*;
use crate::{EntityId, MemoryType, TaskId, TemporalResolveError};

/// 关系目录的类型约束。通用 fact 也须有明确的类型约束，不能猜允许的类型。
#[derive(Clone, Debug)]
pub struct Relation {
    pub id: String,
    pub memory_types: Vec<MemoryType>,
}

pub struct Context<'a> {
    pub text: &'a str,
    pub entities: &'a [EntityId],
    pub relations: &'a [Relation],
    /// 原文接收日（用户本地日期），不是后台执行日。
    pub said_on: &'a str,
    pub utc_offset_seconds: i32,
    /// 仅从运行时复制；w1.v1 没有可由模型填写的任务字段。
    pub task_id: Option<&'a TaskId>,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Ok,
    Unresolved,
    Rejected,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CandidateResult {
    /// 最终 W1 中的原始索引，拒收不改变其他候选的身份。
    pub index: usize,
    pub status: Status,
    pub candidate: Candidate,
    pub time_resolutions: Vec<TimeResolution>,
    pub reasons: Vec<String>,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RequestResult {
    pub index: usize,
    /// 保留原始目标索引；不能把全被拒收的 facts 请求改释成针对旧事实的请求。
    pub request: MemoryRequest,
    /// W2 保留候选数组中的位置；与原始索引的对应关系由 Batch::index_map 给出。
    pub final_candidate_indices: Vec<usize>,
    pub rejected_candidate_indices: Vec<usize>,
    pub deadline_resolution: Option<TimeResolution>,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Batch {
    /// 包含拒收项，保留逐候选结果；送审时过滤 Rejected，并保留 index。
    pub candidates: Vec<CandidateResult>,
    pub memory_requests: Vec<RequestResult>,
    pub index_map: Vec<Option<usize>>,
    pub task_id: Option<TaskId>,
    pub said_on: String,
    pub utc_offset_seconds: i32,
}

/// 复用现有谓词注册表及类型约束；旧目录缺省表示允许四种类型，通用 fact 始终登记。
pub fn relation_catalog(
    predicates: &[crate::PredicateDefinition],
    memory_types: &[(crate::Predicate, Vec<MemoryType>)],
) -> Result<Vec<Relation>, Error> {
    use std::collections::BTreeMap;
    let all = vec![
        MemoryType::Fact,
        MemoryType::Preference,
        MemoryType::Constraint,
        MemoryType::Episode,
    ];
    let mut catalog = BTreeMap::new();
    for predicate in predicates {
        if predicate.predicate.0.trim().is_empty()
            || catalog
                .insert(predicate.predicate.0.clone(), all.clone())
                .is_some()
        {
            return Err(Error::InvalidContext("relation_catalog"));
        }
    }
    catalog.entry("fact".into()).or_insert(all);
    let mut seen = std::collections::BTreeSet::new();
    for (predicate, types) in memory_types {
        if types.is_empty() || !seen.insert(predicate.0.clone()) {
            return Err(Error::InvalidContext("relation_catalog"));
        }
        *catalog
            .get_mut(&predicate.0)
            .ok_or(Error::InvalidContext("relation_catalog"))? = types.clone();
    }
    Ok(catalog
        .into_iter()
        .map(|(id, memory_types)| Relation { id, memory_types })
        .collect())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    InvalidOutput,
    InvalidContext(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidOutput => f.write_str("w1_invalid_output"),
            Self::InvalidContext(reason) => write!(f, "invalid_w2_context:{reason}"),
        }
    }
}
impl std::error::Error for Error {}

/// strict 形状解码；错误不携带模型原文。已解码输入也会在 validate 中检查非空字符串。
pub fn decode(text: &str) -> Result<W1Output, Error> {
    let output = serde_json::from_str(text).map_err(|_| Error::InvalidOutput)?;
    shape(&output)?;
    Ok(output)
}

fn shape(output: &W1Output) -> Result<(), Error> {
    fn nonblank(value: &serde_json::Value) -> bool {
        match value {
            serde_json::Value::String(text) => !text.trim().is_empty(),
            serde_json::Value::Array(values) => values.iter().all(nonblank),
            serde_json::Value::Object(fields) => fields.values().all(nonblank),
            _ => true,
        }
    }
    if nonblank(&serde_json::to_value(output).map_err(|_| Error::InvalidOutput)?) {
        Ok(())
    } else {
        Err(Error::InvalidOutput)
    }
}

pub fn validate(mut output: W1Output, ctx: &Context<'_>) -> Result<Batch, Error> {
    shape(&output)?;
    if crate::temporal::parse_iso_date(ctx.said_on).is_none() {
        return Err(Error::InvalidContext("said_on"));
    }
    if time::UtcOffset::from_whole_seconds(ctx.utc_offset_seconds).is_err() {
        return Err(Error::InvalidContext("timezone"));
    }
    if ctx.task_id.is_some_and(|id| id.0.trim().is_empty()) {
        return Err(Error::InvalidContext("task_id"));
    }
    // 请求失效不能被静默忽略，尤其是可能限制保存的请求：整批失败关闭。
    let deadlines = output
        .memory_requests
        .iter_mut()
        .map(|request| check_request(request, output.candidates.len(), ctx))
        .collect::<Result<Vec<_>, _>>()?;
    let candidates: Vec<_> = output
        .candidates
        .into_iter()
        .enumerate()
        .map(|(index, mut candidate)| {
            // 只修正 sentence 的非原文后缀，之后不复用任何旧检查结果。
            if locate_quote(ctx.text, &candidate.sentence.text).is_none()
                && let Some(sentence) = trim_sentence(&candidate.sentence.text, ctx.text)
            {
                candidate.sentence.text = sentence;
            }
            let mut time_resolutions = Vec::new();
            let checked = check_candidate(&mut candidate, ctx, &mut time_resolutions);
            let (status, reasons) = match checked {
                Ok(Status::Unresolved) => (Status::Unresolved, vec!["unresolved_subject".into()]),
                Ok(status) => (status, Vec::new()),
                Err(reason) => (
                    Status::Rejected,
                    vec![format!("candidate_rejected:{reason}")],
                ),
            };
            CandidateResult {
                index,
                status,
                candidate,
                time_resolutions,
                reasons,
            }
        })
        .collect();
    let mut next = 0;
    let index_map: Vec<_> = candidates
        .iter()
        .map(|c| {
            (c.status != Status::Rejected).then(|| {
                let index = next;
                next += 1;
                index
            })
        })
        .collect();
    let memory_requests = output
        .memory_requests
        .into_iter()
        .zip(deadlines)
        .enumerate()
        .map(|(index, (request, deadline_resolution))| RequestResult {
            final_candidate_indices: request
                .target
                .candidate_indices
                .iter()
                .filter_map(|i| index_map[*i])
                .collect(),
            rejected_candidate_indices: request
                .target
                .candidate_indices
                .iter()
                .copied()
                .filter(|i| index_map[*i].is_none())
                .collect(),
            index,
            request,
            deadline_resolution,
        })
        .collect();
    Ok(Batch {
        candidates,
        memory_requests,
        index_map,
        task_id: ctx.task_id.cloned(),
        said_on: ctx.said_on.into(),
        utc_offset_seconds: ctx.utc_offset_seconds,
    })
}

fn restore(quote: &mut Quote, source: &str) -> Result<(), &'static str> {
    let range = locate_quote(source, &quote.text).ok_or("quote_not_found")?;
    quote.text = source[range].into();
    Ok(())
}

fn trim_sentence(sentence: &str, source: &str) -> Option<String> {
    for (end, _) in sentence.char_indices().rev().filter(|(end, _)| *end != 0) {
        let Some(range) = locate_quote(source, &sentence[..end]) else {
            continue;
        };
        let prefix = &source[range.clone()];
        // ponytail: only a punctuation-ended span or the whole remaining source proves a
        // mechanically safe cut; ambiguous sentence boundaries stay rejected for W3/re-extraction.
        if range.end != source.trim_end().len()
            && !prefix.trim_end().ends_with(['。', '！', '？', '!', '?'])
        {
            continue;
        }
        let suffix = sentence[end..].trim();
        // 原文句子被搬到不连续的位置也不能截掉；夹着注释搬运原句同样拒收。
        if locate_quote(source, suffix).is_some()
            || source
                .split_inclusive(['。', '！', '？', '.', '!', '?', '\n'])
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .any(|s| locate_quote(suffix, s).is_some())
        {
            return None;
        }
        return Some(prefix.into());
    }
    None
}

fn memory_type(name: &str) -> Option<MemoryType> {
    Some(match name {
        "fact" => MemoryType::Fact,
        "preference" => MemoryType::Preference,
        "constraint" => MemoryType::Constraint,
        "episode" => MemoryType::Episode,
        _ => return None,
    })
}

fn check_candidate(
    c: &mut Candidate,
    ctx: &Context<'_>,
    resolutions: &mut Vec<TimeResolution>,
) -> Result<Status, &'static str> {
    restore(&mut c.sentence, ctx.text).map_err(|_| "sentence_not_found")?;
    let status = match &mut c.subject {
        Subject::Entity(subject) if ctx.entities.iter().any(|e| e.0 == subject.entity_id) => {
            Status::Ok
        }
        Subject::Entity(subject) => {
            let mut reference = Quote {
                text: subject.entity_id.clone(),
            };
            restore(&mut reference, ctx.text).map_err(|_| "subject_not_found")?;
            c.subject = Subject::Unresolved(UnresolvedSubject {
                unresolved: reference,
            });
            Status::Unresolved
        }
        Subject::Unresolved(subject) => {
            restore(&mut subject.unresolved, ctx.text).map_err(|_| "subject_not_found")?;
            Status::Unresolved
        }
    };
    let relation = ctx.relations.iter().find(|r| r.id == c.relation);
    if relation.is_none() && c.relation != "fact" {
        return Err("unknown_relation");
    }
    let kind = memory_type(&c.memory_type).ok_or("unknown_memory_type")?;
    if relation.is_none_or(|r| !r.memory_types.contains(&kind)) {
        return Err("memory_type_not_allowed");
    }
    if kind != MemoryType::Preference && c.strength.is_some() {
        return Err("strength_not_applicable");
    }
    if c.kind == CandidateKind::Assertion && c.value.is_none() {
        return Err("assertion_without_value");
    }
    if let Some(value) = &mut c.value {
        let quote = match value {
            Value::Text { quote, .. }
            | Value::Integer { quote, .. }
            | Value::Boolean { quote, .. }
            | Value::Entity { quote, .. } => quote,
        };
        restore(quote, ctx.text).map_err(|_| "value_quote_not_found")?;
        match value {
            Value::Text { text, quote } => {
                let range = locate_quote(&quote.text, text).ok_or("value_text_not_found")?;
                *text = quote.text[range].into();
            }
            Value::Integer { integer, quote } => {
                let text: String = quote
                    .text
                    .chars()
                    .filter(|ch| !ch.is_whitespace())
                    .collect();
                // 仅比较明确的十进制表示，不新增中文数词或数值归属推断。
                if text.bytes().any(|b| b.is_ascii_digit())
                    && text
                        .bytes()
                        .all(|b| b.is_ascii_digit() || b"+-.".contains(&b))
                    && text.parse::<i64>() != Ok(*integer)
                {
                    return Err("integer_quote_mismatch");
                }
            }
            Value::Entity { entity_id, .. } => {
                if !ctx.entities.iter().any(|e| e.0 == *entity_id) {
                    return Err("unknown_value_entity");
                }
            }
            Value::Boolean { .. } => (), // 属性与真假含义由 W3 核验。
        }
        if c.relation == "fact"
            && !matches!(value, Value::Text { text, quote }
                if text == &c.sentence.text && quote == &c.sentence)
        {
            return Err("fact_sentence_mismatch");
        }
    }
    if let Some(qualifier) = &mut c.uncertainty {
        restore(&mut qualifier.quote, ctx.text).map_err(|_| "uncertainty_not_found")?;
    }
    for qualifier in &mut c.conditions {
        restore(&mut qualifier.quote, ctx.text).map_err(|_| "condition_not_found")?;
    }
    for time in &mut c.times {
        if time.role == TimeRole::Retention {
            return Err("retention_in_candidate");
        }
        check_time(time, ctx).map(|r| resolutions.push(r))?;
    }
    Ok(status)
}

fn check_time(time: &mut TimeInterp, ctx: &Context<'_>) -> Result<TimeResolution, &'static str> {
    restore(&mut time.quote, ctx.text).map_err(|_| "time_quote_not_found")?;
    if let Interp::Anchored { anchor_quote, .. } = &mut time.interp {
        restore(anchor_quote, ctx.text).map_err(|_| "anchor_quote_not_found")?;
    }
    resolve_time_interp(time, ctx.said_on, ctx.utc_offset_seconds).map_err(|_| "invalid_time")
}

fn check_request(
    request: &mut MemoryRequest,
    candidate_count: usize,
    ctx: &Context<'_>,
) -> Result<Option<TimeResolution>, Error> {
    restore(&mut request.quote, ctx.text).map_err(|_| Error::InvalidOutput)?;
    let target = &mut request.target;
    if target
        .candidate_indices
        .iter()
        .any(|i| *i >= candidate_count)
        || (target.object == RequestObject::Unknown && !target.candidate_indices.is_empty())
        || (target.object != RequestObject::Source
            && target.candidate_indices.is_empty()
            && target.quote.is_none())
    {
        return Err(Error::InvalidOutput);
    }
    if let Some(quote) = &mut target.quote {
        restore(quote, ctx.text).map_err(|_| Error::InvalidOutput)?;
    }
    match (&request.kind, &mut request.deadline) {
        (RequestKind::Retention, Some(time)) if time.role == TimeRole::Retention => {
            check_time(time, ctx)
                .map(Some)
                .map_err(|_| Error::InvalidOutput)
        }
        (RequestKind::Retention, _) | (_, Some(_)) => Err(Error::InvalidOutput),
        (_, None) => Ok(None),
    }
}

/// 新契约的纯换算：只计算模型已经给出的 TimeInterp，不调用旧自然语言解析策略。
/// 未知锚点和周期表达保留 unknown；非法日期、倒置、溢出及明确数字日期矛盾返回错误。
pub fn resolve_time_interp(
    time: &crate::contract::TimeInterp,
    said_on: &str,
    utc_offset_seconds: i32,
) -> Result<crate::contract::TimeResolution, TemporalResolveError> {
    use crate::contract::{Interp, Precision, ResolutionStatus, TimeResolution, TimeUnit};
    let invalid = || TemporalResolveError("invalid TimeInterp calendar or literal date".into());
    let said_on = crate::temporal::parse_iso_date(said_on).ok_or_else(invalid)?;
    let offset = UtcOffset::from_whole_seconds(utc_offset_seconds).map_err(|_| invalid())?;
    let year = |offset: i64| {
        i64::from(said_on.year())
            .checked_add(offset)
            .and_then(|year| i32::try_from(year).ok())
            .ok_or_else(invalid)
    };
    let day = |date: Date| Ok((date, date.next_day().ok_or_else(invalid)?, Precision::Day));
    let month = |year, month: u8| {
        let start =
            Date::from_calendar_date(year, Month::try_from(month).map_err(|_| invalid())?, 1)
                .map_err(|_| invalid())?;
        let (year, month) = if month == 12 {
            (year.checked_add(1).ok_or_else(invalid)?, 1)
        } else {
            (year, month + 1)
        };
        let end = Date::from_calendar_date(year, Month::try_from(month).map_err(|_| invalid())?, 1)
            .map_err(|_| invalid())?;
        Ok((start, end, Precision::Month))
    };
    let (start, end, precision) = match &time.interp {
        Interp::Date { date } => day(crate::temporal::parse_iso_date(date).ok_or_else(invalid)?)?,
        Interp::Range {
            start,
            end_inclusive,
        } => (
            crate::temporal::parse_iso_date(start).ok_or_else(invalid)?,
            crate::temporal::parse_iso_date(end_inclusive)
                .and_then(Date::next_day)
                .ok_or_else(invalid)?,
            Precision::Day,
        ),
        Interp::Relative { unit, offset } => match unit {
            TimeUnit::Day | TimeUnit::Week => {
                let days = offset
                    .checked_mul(if *unit == TimeUnit::Week { 7 } else { 1 })
                    .ok_or_else(invalid)?;
                let julian = i64::from(said_on.to_julian_day())
                    .checked_add(days)
                    .and_then(|day| i32::try_from(day).ok())
                    .ok_or_else(invalid)?;
                day(Date::from_julian_day(julian).map_err(|_| invalid())?)?
            }
            TimeUnit::Month => {
                let months = (i64::from(said_on.year()) * 12 + i64::from(said_on.month() as u8)
                    - 1)
                .checked_add(*offset)
                .ok_or_else(invalid)?;
                month(
                    i32::try_from(months.div_euclid(12)).map_err(|_| invalid())?,
                    (months.rem_euclid(12) + 1) as u8,
                )?
            }
            TimeUnit::Year => {
                let year = year(*offset)?;
                (
                    Date::from_calendar_date(year, Month::January, 1).map_err(|_| invalid())?,
                    Date::from_calendar_date(
                        year.checked_add(1).ok_or_else(invalid)?,
                        Month::January,
                        1,
                    )
                    .map_err(|_| invalid())?,
                    Precision::Year,
                )
            }
        },
        Interp::Month {
            year_offset,
            month: m,
        } => month(year(*year_offset)?, *m)?,
        Interp::Anchored { .. } | Interp::Recurring {} | Interp::Unknown {} => {
            return Ok(TimeResolution {
                status: ResolutionStatus::Unknown,
                start: None,
                end_exclusive: None,
                precision: Precision::Unknown,
            });
        }
    };
    if end <= start || !literal_time_matches(&time.quote.text, start, end, precision) {
        return Err(invalid());
    }
    // UTC 零时再减时区，避免极端合法 Date 在 assume_offset 内发生边界溢出。
    let timestamp = |date| {
        time::PrimitiveDateTime::new(date, time::Time::MIDNIGHT)
            .assume_utc()
            .unix_timestamp()
            .checked_sub(i64::from(offset.whole_seconds()))
            .ok_or_else(invalid)
    };
    Ok(TimeResolution {
        status: ResolutionStatus::Resolved,
        start: Some(timestamp(start)?),
        end_exclusive: Some(timestamp(end)?),
        precision,
    })
}

/// 核对 Quote 中明确日历片段；不解释相对时间，也不推断多个日期的语义归属。
fn literal_time_matches(quote: &str, start: Date, end: Date, precision: Precision) -> bool {
    let text: String = quote.chars().filter(|ch| !ch.is_whitespace()).collect();
    if let Some(matches) = literal_calendar_matches(&text, start, end, precision) {
        return matches;
    }
    // 取连续日历片段而非所有子串，防止把完整日期降为月份或把区间降为端点。
    let joint = |ch: char| matches!(ch, '至' | '到' | '~' | '～' | '—' | '－' | '-');
    let calendar = |ch: char| {
        ch.is_ascii_digit()
            || matches!(
                ch,
                '年' | '月'
                    | '日'
                    | '号'
                    | '一'
                    | '二'
                    | '三'
                    | '四'
                    | '五'
                    | '六'
                    | '七'
                    | '八'
                    | '九'
                    | '十'
            )
            || joint(ch)
    };
    let mut recognized = false;
    for segment in text.split(|ch| !calendar(ch)) {
        let segment = segment
            .trim_matches(joint)
            .trim_start_matches(['年', '月', '日', '号']);
        if let Some(matches) = literal_calendar_matches(segment, start, end, precision) {
            if matches {
                return true;
            }
            recognized = true;
        }
    }
    // 未识别的自然语言仍由模型解释；有明确日期时，至少须有一个完整片段吻合。
    !recognized
}

/// None 表示不是支持的明确日历表达；Some(false) 包括非法日期及精度矛盾。
fn literal_calendar_matches(
    text: &str,
    start: Date,
    end: Date,
    precision: Precision,
) -> Option<bool> {
    fn month_number(text: &str) -> Option<u8> {
        text.parse().ok().or_else(|| {
            [
                "一", "二", "三", "四", "五", "六", "七", "八", "九", "十", "十一", "十二",
            ]
            .iter()
            .position(|month| *month == text)
            .map(|i| i as u8 + 1)
        })
    }
    // 保留非法数字日期的形状，不能把“2月30日”悄悄接受成模型改写的“2月28日”。
    fn numeric_date(text: &str, year: i32) -> Option<(Option<Date>, &str)> {
        if let Some(prefix) = text.get(..10)
            && prefix.as_bytes()[4] == b'-'
            && prefix.as_bytes()[7] == b'-'
            && prefix
                .bytes()
                .enumerate()
                .all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit())
        {
            return Some((crate::temporal::parse_iso_date(prefix), &text[10..]));
        }
        let (head, rest) = text.split_once('月')?;
        let (year, month) = match head.split_once('年') {
            Some((year, month)) if year.len() == 4 => (year.parse().ok()?, month),
            Some(_) => return None,
            None => (year, head),
        };
        if month.is_empty()
            || !(month.bytes().all(|b| b.is_ascii_digit()) || month_number(month).is_some())
        {
            return None;
        }
        let at = rest.find(['日', '号'])?;
        let day = &rest[..at];
        if day.is_empty() || !day.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let month = month_number(month).and_then(|m| Month::try_from(m).ok());
        let day = day.parse::<u8>().ok();
        let rest = &rest[at + rest[at..].chars().next()?.len_utf8()..];
        Some((
            month
                .zip(day)
                .and_then(|(m, d)| Date::from_calendar_date(year, m, d).ok()),
            rest,
        ))
    }
    if let Some((first, rest)) = numeric_date(text, start.year()) {
        if rest.is_empty() {
            return Some(first == Some(start) && first.and_then(Date::next_day) == Some(end));
        }
        if let Some(tail) = ["至", "到", "~", "～", "—", "－", "-"]
            .iter()
            .find_map(|joint| rest.strip_prefix(joint))
        {
            let last =
                numeric_date(tail, end.previous_day().unwrap_or(start).year()).or_else(|| {
                    let first = first?;
                    let at = tail.find(['日', '号'])?;
                    let day = tail[..at].parse::<u8>().ok()?;
                    let rest = &tail[at + tail[at..].chars().next()?.len_utf8()..];
                    Some((
                        Date::from_calendar_date(first.year(), first.month(), day as u8).ok(),
                        rest,
                    ))
                });
            if let Some((last, rest)) = last
                && matches!(rest, "" | "（含首尾两日）" | "(含首尾两日)")
            {
                return Some(first == Some(start) && last.and_then(Date::next_day) == Some(end));
            }
        }
    }
    // 明确只有年月的原话不能被换成某个具体日。
    let numeric_month = text
        .strip_suffix('月')
        .and_then(|text| text.split_once('年'))
        .filter(|(year, _)| year.len() == 4 && year.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|(year, month)| Some((year.parse::<i32>().ok()?, month_number(month)?)))
        .or_else(|| {
            (text.len() == 7 && text.as_bytes()[4] == b'-')
                .then(|| Some((text[..4].parse().ok()?, text[5..].parse().ok()?)))
                .flatten()
        })
        .or_else(|| Some((start.year(), month_number(text.strip_suffix('月')?)?)));
    if let Some((year, month)) = numeric_month {
        return Some(
            precision == Precision::Month
                && start.year() == year
                && start.month() as u8 == month
                && start.day() == 1
                && end
                    .previous_day()
                    .is_some_and(|last| last.year() == year && last.month() as u8 == month)
                && end.day() == 1,
        );
    }
    if let Some(year) = text
        .strip_suffix('年')
        .filter(|year| year.len() == 4)
        .and_then(|year| year.parse::<i32>().ok())
    {
        return Some(
            precision == Precision::Year
                && start.year() == year
                && start.month() == Month::January
                && start.day() == 1
                && year.checked_add(1) == Some(end.year())
                && end.month() == Month::January
                && end.day() == 1,
        );
    }
    None
}
