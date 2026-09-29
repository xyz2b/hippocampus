//! 相对时间表达式的确定性解析；所有日历边界都按调用方时区计算。

use std::fmt;

use time::{Date, Duration, Month, OffsetDateTime, PrimitiveDateTime, Time, UtcOffset};

use crate::{
    TemporalRange, TemporalReference, TemporalRelation, TemporalResolution, TemporalSource,
    TimeOfDay, Timestamp,
};

/// “最近”在 V1 中采用的固定回看窗口；有业务配置需求时再外置。
const RECENT_DAYS: i64 = 30;

#[derive(Clone, Debug, Eq, PartialEq)]
/// 时间锚点或时区无效时返回的配置错误；未知自然语言不是错误。
pub struct TemporalResolveError(pub String);

impl fmt::Display for TemporalResolveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::error::Error for TemporalResolveError {}

/// 将 V1 支持的时间原文换算为绝对区间或安全的未解析引用。
pub fn resolve_temporal_expression(
    expression: &str,
    now: Timestamp,
    utc_offset_seconds: i32,
) -> Result<TemporalResolution, TemporalResolveError> {
    let expression = expression.trim();
    let offset = UtcOffset::from_whole_seconds(utc_offset_seconds)
        .map_err(|error| TemporalResolveError(format!("invalid UTC offset: {error}")))?;
    let local_now = OffsetDateTime::from_unix_timestamp(now.0)
        .map_err(|error| TemporalResolveError(format!("invalid time anchor: {error}")))?
        .to_offset(offset);

    if let Some(days) = relative_day_offset(expression) {
        let date = local_now
            .date()
            .checked_add(Duration::days(days))
            .ok_or_else(|| TemporalResolveError("relative day overflow".to_owned()))?;
        return day_range(date, TemporalSource::Inferred, offset);
    }

    match expression {
        "以后" | "今后" | "从今以后" | "现在" | "目前" | "当前" | "from now on" | "now" =>
        {
            return Ok(range(now.0, i64::MAX, TemporalSource::Inferred));
        }
        "最近" | "recently" | "最近开始" | "最近刚开始" | "recently started" => {
            let start = now
                .0
                .checked_sub(Duration::days(RECENT_DAYS).whole_seconds())
                .ok_or_else(|| TemporalResolveError("recent range underflow".to_owned()))?;
            // A recent start uses the same inferred lookback, with no asserted end of the state.
            let end = if matches!(expression, "最近" | "recently") {
                now.0
                    .checked_add(1)
                    .ok_or_else(|| TemporalResolveError("recent range overflow".to_owned()))?
            } else {
                i64::MAX
            };
            return Ok(range(start, end, TemporalSource::Inferred));
        }
        "下个月" | "next month" => {
            let (year, month) = next_month(local_now.year(), local_now.month() as u8)?;
            return month_range(year, month, TemporalSource::Inferred, offset);
        }
        "去年" | "last year" => {
            let year = local_now
                .year()
                .checked_sub(1)
                .ok_or_else(|| TemporalResolveError("last year overflow".to_owned()))?;
            let start = Date::from_calendar_date(year, Month::January, 1)
                .map_err(|error| TemporalResolveError(error.to_string()))?;
            let end = Date::from_calendar_date(local_now.year(), Month::January, 1)
                .map_err(|error| TemporalResolveError(error.to_string()))?;
            return Ok(range(
                local_midnight(start, offset).0,
                local_midnight(end, offset).0,
                TemporalSource::Inferred,
            ));
        }
        _ => {}
    }

    if let Some(month) = last_year_month(expression) {
        let year = local_now
            .year()
            .checked_sub(1)
            .ok_or_else(|| TemporalResolveError("last-year range underflow".to_owned()))?;
        return month_range(year, month, TemporalSource::Inferred, offset);
    }

    if let Some(event) = expression.strip_suffix("结束后") {
        let event = event
            .trim()
            .strip_prefix('在')
            .unwrap_or(event.trim())
            .trim();
        if !event.is_empty() {
            return Ok(TemporalResolution::Reference(TemporalReference::Event {
                event: event.to_owned(),
                relation: TemporalRelation::AfterEnd,
                source: TemporalSource::Explicit,
            }));
        }
    }

    if let Some(event) = expression.strip_suffix("之前")
        && !event.trim().is_empty()
    {
        return Ok(TemporalResolution::Reference(TemporalReference::Event {
            event: event.trim().to_owned(),
            relation: TemporalRelation::BeforeStart,
            source: TemporalSource::Explicit,
        }));
    }

    if let Some(date) = parse_iso_date(expression) {
        return day_range(date, TemporalSource::Explicit, offset);
    }

    if let [span] = absolute_spans(expression)[..]
        && span.text == expression
    {
        return Ok(range(
            local_midnight(span.start, offset).0,
            local_midnight(span.end, offset).0,
            TemporalSource::Explicit,
        ));
    }

    Ok(TemporalResolution::Reference(TemporalReference::Unknown {
        expression: expression.to_owned(),
        source: TemporalSource::Unknown,
    }))
}

/// Verify a model's calendar date; do not fill a missing relative-date proposal.
pub(crate) fn normalized_date(
    expression: &str,
    proposed: Option<&str>,
    now: Timestamp,
    offset: i32,
) -> Result<Option<TemporalResolution>, TemporalResolveError> {
    let expression = expression.trim();
    let relative = relative_day_offset(expression).is_some();
    let absolute = parse_iso_date(expression).is_some();
    if !relative && !absolute {
        return no_calendar_proposal(proposed.is_some());
    }
    let Some(proposed) = proposed else {
        return if relative {
            Err(TemporalResolveError(
                "relative day requires a model YYYY-MM-DD date".into(),
            ))
        } else {
            Ok(None)
        };
    };
    let actual = proposed_day(proposed, now, offset)?;
    let expected = resolve_temporal_expression(expression, now, offset)?;
    match (actual, expected) {
        (TemporalResolution::Range(mut actual), TemporalResolution::Range(expected))
            if actual.start == expected.start && actual.end == expected.end =>
        {
            actual.source = expected.source;
            Ok(Some(TemporalResolution::Range(actual)))
        }
        _ => Err(TemporalResolveError(
            "model date does not match the USER expression and local anchor".into(),
        )),
    }
}

/// Day proposals have no end date; month/year proposals retain their full interval.
pub(crate) fn normalized_calendar(
    expression: &str,
    proposed_start: Option<&str>,
    proposed_end_exclusive: Option<&str>,
    now: Timestamp,
    offset: i32,
) -> Result<Option<TemporalResolution>, TemporalResolveError> {
    let expression = expression.trim();
    if relative_day_offset(expression).is_some() || parse_iso_date(expression).is_some() {
        if proposed_end_exclusive.is_some() {
            return Err(TemporalResolveError(
                "single-day expression must not provide an end date".into(),
            ));
        }
        return normalized_date(expression, proposed_start, now, offset);
    }
    if let [span] = absolute_spans(expression)[..]
        && span.text == expression
    {
        // The USER wrote the calendar dates; a model proposal may only restate them.
        if proposed_start.is_some_and(|start| parse_iso_date(start) != Some(span.start))
            || proposed_end_exclusive.is_some_and(|end| parse_iso_date(end) != Some(span.end))
        {
            return Err(TemporalResolveError(
                "model dates do not match the USER calendar dates".into(),
            ));
        }
        return resolve_temporal_expression(expression, now, offset).map(Some);
    }
    if !calendar_interval(expression) {
        return no_calendar_proposal(proposed_start.is_some() || proposed_end_exclusive.is_some());
    }
    let (Some(start), Some(end)) = (proposed_start, proposed_end_exclusive) else {
        return Err(TemporalResolveError(
            "calendar interval requires model start and exclusive end dates".into(),
        ));
    };
    let TemporalResolution::Range(start) = proposed_day(start, now, offset)? else {
        unreachable!()
    };
    let TemporalResolution::Range(end) = proposed_day(end, now, offset)? else {
        unreachable!()
    };
    let TemporalResolution::Range(expected) = resolve_temporal_expression(expression, now, offset)?
    else {
        unreachable!()
    };
    if start.start != expected.start || end.start != expected.end {
        return Err(TemporalResolveError(
            "model dates do not match the USER calendar interval".into(),
        ));
    }
    Ok(Some(TemporalResolution::Range(TemporalRange {
        start: start.start,
        end: end.start,
        source: expected.source,
    })))
}

/// Retention's model date is inclusive, including the last day of a month/year.
pub(crate) fn normalized_expiry_date(
    expression: &str,
    proposed: Option<&str>,
    now: Timestamp,
    offset: i32,
) -> Result<Option<TemporalResolution>, TemporalResolveError> {
    let expression = expression.trim();
    if !calendar_interval(expression) {
        return normalized_date(expression, proposed, now, offset);
    }
    let Some(proposed) = proposed else {
        return Err(TemporalResolveError(
            "calendar expiry requires a model inclusive YYYY-MM-DD date".into(),
        ));
    };
    let TemporalResolution::Range(mut actual) = proposed_day(proposed, now, offset)? else {
        unreachable!()
    };
    let TemporalResolution::Range(expected) = resolve_temporal_expression(expression, now, offset)?
    else {
        unreachable!()
    };
    if actual.end != expected.end {
        return Err(TemporalResolveError(
            "model expiry date does not match the USER calendar interval".into(),
        ));
    }
    actual.source = expected.source;
    Ok(Some(TemporalResolution::Range(actual)))
}

fn no_calendar_proposal(present: bool) -> Result<Option<TemporalResolution>, TemporalResolveError> {
    if present {
        Err(TemporalResolveError(
            "unsupported expression must not provide a model calendar date".into(),
        ))
    } else {
        Ok(None)
    }
}

fn proposed_day(
    expression: &str,
    now: Timestamp,
    offset: i32,
) -> Result<TemporalResolution, TemporalResolveError> {
    if parse_iso_date(expression).is_none() {
        return Err(TemporalResolveError(
            "model calendar date must be a valid YYYY-MM-DD".into(),
        ));
    }
    resolve_temporal_expression(expression, now, offset)
}

fn calendar_interval(expression: &str) -> bool {
    matches!(expression, "下个月" | "next month" | "去年" | "last year")
        || last_year_month(expression).is_some_and(|month| (1..=12).contains(&month))
}

// ponytail: finite whole-expression day grammar; modifiers and ranges stay unresolved.
pub(crate) fn relative_day_offset(expression: &str) -> Option<i64> {
    let expression = expression.trim();
    let named = match expression {
        "今天" | "今日" | "today" => Some(0),
        "昨天" | "yesterday" => Some(-1),
        "前天" | "day before yesterday" | "the day before yesterday" => Some(-2),
        "大前天" => Some(-3),
        "明天" | "tomorrow" => Some(1),
        "后天" | "day after tomorrow" | "the day after tomorrow" => Some(2),
        "大后天" => Some(3),
        _ => None,
    };
    if named.is_some() {
        return named;
    }
    for (suffix, sign) in [
        ("天前", -1),
        ("天后", 1),
        (" days ago", -1),
        (" day ago", -1),
        (" days later", 1),
        (" day later", 1),
    ] {
        if let Some(number) = expression.strip_suffix(suffix) {
            return day_count(number).map(|days| i64::from(days) * sign);
        }
    }
    expression
        .strip_prefix("in ")
        .and_then(|s| s.strip_suffix(" days").or_else(|| s.strip_suffix(" day")))
        .and_then(day_count)
        .map(i64::from)
}

fn day_count(number: &str) -> Option<u32> {
    let number = number.trim();
    if !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()) {
        return number.parse().ok();
    }
    let unit = |s| match s {
        "零" | "zero" => Some(0),
        "一" | "one" => Some(1),
        "二" | "两" | "two" => Some(2),
        "三" | "three" => Some(3),
        "四" | "four" => Some(4),
        "五" | "five" => Some(5),
        "六" | "six" => Some(6),
        "七" | "seven" => Some(7),
        "八" | "eight" => Some(8),
        "九" | "nine" => Some(9),
        "十" | "ten" => Some(10),
        _ => None,
    };
    if let Some(value) = unit(number) {
        return Some(value);
    }
    let (tens, ones) = number.split_once('十')?;
    let tens = if tens.is_empty() {
        1
    } else {
        unit(tens).filter(|n| (1..=9).contains(n))?
    };
    let ones = if ones.is_empty() {
        0
    } else {
        unit(ones).filter(|n| (1..=9).contains(n))?
    };
    Some(tens * 10 + ones)
}

fn day_range(
    date: Date,
    source: TemporalSource,
    offset: UtcOffset,
) -> Result<TemporalResolution, TemporalResolveError> {
    let end = date
        .next_day()
        .ok_or_else(|| TemporalResolveError("date cannot advance one day".into()))?;
    Ok(range(
        local_midnight(date, offset).0,
        local_midnight(end, offset).0,
        source,
    ))
}

/// 无具体边界的过去描述不是任意 Unknown 的通配符；仅用于无范围历史查询。
pub(crate) fn is_generic_past(expression: &str) -> bool {
    let expression = expression.trim();
    // Frequency alone adds no date boundary; keep any remaining event/date wording intact.
    let expression = ["经常", "常常", "常"]
        .into_iter()
        .find_map(|suffix| expression.strip_suffix(suffix))
        .unwrap_or(expression)
        .trim_end();
    matches!(
        expression,
        "以前" | "之前" | "过去" | "曾经" | "曾" | "从前" | "previously" | "in the past"
    )
}

/// 反复发生的日内活动属于 Scope，而不是一段无法解析的现实有效期。
pub(crate) fn recurring_time(expression: &str) -> Option<TimeOfDay> {
    match expression.trim() {
        "每天早上" | "每天早晨" | "每天上午" | "每天早上都" | "every morning" => {
            Some(TimeOfDay::Morning)
        }
        "每天下午" | "every afternoon" => Some(TimeOfDay::Afternoon),
        "每天晚上" | "每晚" | "every evening" => Some(TimeOfDay::Evening),
        _ => None,
    }
}

fn range(start: i64, end: i64, source: TemporalSource) -> TemporalResolution {
    TemporalResolution::Range(TemporalRange {
        start: Timestamp(start),
        end: Timestamp(end),
        source,
    })
}

fn month_range(
    year: i32,
    month: u8,
    source: TemporalSource,
    offset: UtcOffset,
) -> Result<TemporalResolution, TemporalResolveError> {
    let month = Month::try_from(month)
        .map_err(|error| TemporalResolveError(format!("invalid month: {error}")))?;
    let start = Date::from_calendar_date(year, month, 1)
        .map_err(|error| TemporalResolveError(format!("invalid month range: {error}")))?;
    let (end_year, end_month) = next_month(year, month as u8)?;
    let end_month = Month::try_from(end_month)
        .map_err(|error| TemporalResolveError(format!("invalid month: {error}")))?;
    let end = Date::from_calendar_date(end_year, end_month, 1)
        .map_err(|error| TemporalResolveError(format!("invalid month range: {error}")))?;
    Ok(TemporalResolution::Range(TemporalRange {
        start: local_midnight(start, offset),
        end: local_midnight(end, offset),
        source,
    }))
}

fn next_month(year: i32, month: u8) -> Result<(i32, u8), TemporalResolveError> {
    if month == 12 {
        let year = year
            .checked_add(1)
            .ok_or_else(|| TemporalResolveError("next-month range overflow".to_owned()))?;
        Ok((year, 1))
    } else {
        Ok((year, month + 1))
    }
}

fn local_midnight(date: Date, offset: UtcOffset) -> Timestamp {
    Timestamp(
        PrimitiveDateTime::new(date, Time::MIDNIGHT)
            .assume_offset(offset)
            .unix_timestamp(),
    )
}

fn last_year_month(expression: &str) -> Option<u8> {
    let month = expression.strip_prefix("去年")?.strip_suffix('月')?.trim();
    month.parse().ok().or_else(|| {
        Some(match month {
            "一" => 1,
            "二" => 2,
            "三" => 3,
            "四" => 4,
            "五" => 5,
            "六" => 6,
            "七" => 7,
            "八" => 8,
            "九" => 9,
            "十" => 10,
            "十一" => 11,
            "十二" => 12,
            _ => return None,
        })
    })
}

/// A literal calendar date or inclusive date range in USER text, e.g. 2027年4月1日至4月14日.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct AbsoluteSpan<'a> {
    pub(crate) text: &'a str,
    pub(crate) start: Date,
    /// Exclusive: the day after the last named day.
    pub(crate) end: Date,
}

fn leading_number(text: &str, max_digits: usize) -> Option<(u32, &str)> {
    let digits = text.bytes().take_while(u8::is_ascii_digit).count();
    (1..=max_digits)
        .contains(&digits)
        .then(|| Some((text[..digits].parse().ok()?, &text[digits..])))
        .flatten()
}

/// Parses `[YYYY年]M月D日|号` at the start of text; the year falls back to `year`.
fn cjk_date(text: &str, year: Option<i32>) -> Option<(Date, &str)> {
    let (year, rest) = match leading_number(text, 4) {
        Some((year, rest)) if rest.starts_with('年') && text.len() - rest.len() == 4 => {
            (year as i32, &rest['年'.len_utf8()..])
        }
        _ => (year?, text),
    };
    let (month, rest) = leading_number(rest, 2)?;
    let rest = rest.strip_prefix('月')?;
    let (day, rest) = leading_number(rest, 2)?;
    let rest = rest.strip_prefix(['日', '号'])?;
    let date = Date::from_calendar_date(year, Month::try_from(month as u8).ok()?, day as u8).ok()?;
    Some((date, rest))
}

// ponytail: full-year CJK dates and "A至|到B" ranges only; relative or weekday ranges stay
// unresolved until a real case needs them.
/// Every literal absolute date or date range in text, left to right.
pub(crate) fn absolute_spans(text: &str) -> Vec<AbsoluteSpan<'_>> {
    let mut spans = Vec::new();
    let mut at = 0;
    while at < text.len() {
        let tail = &text[at..];
        let starts_year = text[..at].chars().next_back().is_none_or(|c| !c.is_ascii_digit());
        if let Some((start, rest)) = starts_year.then(|| cjk_date(tail, None)).flatten() {
            let (last, rest) = ["至", "到", "-", "~", "～", "—", "－"]
                .iter()
                .find_map(|joint| rest.strip_prefix(joint))
                .and_then(|rest| cjk_date(rest, Some(start.year())))
                .filter(|(last, _)| *last >= start)
                .unwrap_or((start, rest));
            let end = at + tail.len() - rest.len();
            if let Some(next) = last.next_day() {
                spans.push(AbsoluteSpan { text: &text[at..end], start, end: next });
            }
            at = end;
        } else {
            at += tail.chars().next().map_or(1, char::len_utf8);
        }
    }
    spans
}

// ponytail: lexical sanity check only; it never resolves a time, it just refuses filler.
/// Whether an expression contains any time word or digit at all.
pub(crate) fn mentions_time(expression: &str) -> bool {
    let lower = expression.to_lowercase();
    let chars: Vec<char> = lower.chars().collect();
    // A bare count ("7") is not a time; digits need a date/clock separator (7/10, 17:30,
    // 2026-09-28) or must form a plausible year.
    let numeric_time = chars.windows(3).any(|w| {
        w[0].is_ascii_digit() && matches!(w[1], '/' | ':' | '-' | '.') && w[2].is_ascii_digit()
    }) || lower
        .split(|c: char| !c.is_ascii_digit())
        .any(|n| n.len() == 4 && n.parse::<u32>().is_ok_and(|y| (1900..=2100).contains(&y)));
    numeric_time
        || chars.iter().any(|c| {
            "年月日号天周星礼拜时点分秒今明昨前后早晚午夜晨季旬初底末刚曾将现目当近久往过去来以此期间际次".contains(*c)
        })
        || [
        "now", "day", "week", "month", "year", "ago", "later", "recent", "since", "until",
        "before", "after", "morning", "evening", "night", "tonight", "weekend", "hour",
        "minute", "today", "tomorrow", "yesterday", "monday", "tuesday", "wednesday",
        "thursday", "friday", "saturday", "sunday", "spring", "summer", "autumn", "fall",
        "winter", "next", "last", "past", "future", "currently", "then",
    ]
    .iter()
    .any(|word| lower.split(|c: char| !c.is_ascii_alphabetic()).any(|w| w.starts_with(word)))
}

pub(crate) fn is_valid_iso_date(value: &str) -> bool {
    parse_iso_date(value).is_some()
}

fn parse_iso_date(value: &str) -> Option<Date> {
    let mut parts = value.split('-');
    let (year, month, day) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some()
        || year.len() != 4
        || month.len() != 2
        || day.len() != 2
        || ![year, month, day]
            .iter()
            .all(|part| part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return None;
    }
    let year = year.parse().ok()?;
    let month = Month::try_from(month.parse::<u8>().ok()?).ok()?;
    let day = day.parse().ok()?;
    Date::from_calendar_date(year, month, day).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_dates_and_ranges_are_read_from_user_text() {
        let text = "从2027年4月15日至2027年4月30日（含首尾两日），每天19:00闭馆。另有2028年1月3号和12月9日。";
        let spans = absolute_spans(text);
        let dates: Vec<_> = spans
            .iter()
            .map(|s| (s.text, s.start.to_string(), s.end.to_string()))
            .collect();
        assert_eq!(
            dates,
            [
                ("2027年4月15日至2027年4月30日", "2027-04-15".into(), "2027-05-01".into()),
                ("2028年1月3号", "2028-01-03".into(), "2028-01-04".into()),
            ]
        );
        assert_eq!(absolute_spans("2027年4月1日至4月14日")[0].end.to_string(), "2027-04-15");
        assert!(absolute_spans("2027年2月30日 12027年1月1日 2027年5月1日至4月1日")
            .iter()
            .all(|s| s.text == "2027年5月1日"));
        let now = Timestamp(1790514274);
        let range = |text| normalized_calendar(text, Some("2027-04-15"), Some("2027-05-01"), now, 28800);
        assert!(matches!(range("2027年4月15日至2027年4月30日"), Ok(Some(_))));
        assert!(range("2027年4月15日至2027年4月29日").is_err());
    }

    #[test]
    fn filler_is_not_a_time_expression() {
        for filler in ["从", "青", ".", "n", "]", "墨", "7", "12"] {
            assert!(!mentions_time(filler), "{filler}");
        }
        for expression in [
            "明天", "2027年4月1日", "上周末", "这次", "3 days ago", "next month", "7/10", "17:30",
            "2026-09-28", "in 1990",
        ] {
            assert!(mentions_time(expression), "{expression}");
        }
    }

    #[test]
    fn relative_days_cover_named_and_counted_days_without_truncation() {
        let now = Timestamp(1790514274); // 2026-09-27, UTC+8.
        for (expression, date) in [
            ("前天", "2026-09-25"),
            ("大前天", "2026-09-24"),
            ("大后天", "2026-09-30"),
            ("the day before yesterday", "2026-09-25"),
            ("day after tomorrow", "2026-09-29"),
            ("3天前", "2026-09-24"),
            ("三天后", "2026-09-30"),
            ("3 days ago", "2026-09-24"),
            ("in 3 days", "2026-09-30"),
        ] {
            let TemporalResolution::Range(actual) =
                resolve_temporal_expression(expression, now, 28800).unwrap()
            else {
                panic!("relative day remained unresolved: {expression}");
            };
            let TemporalResolution::Range(expected) =
                resolve_temporal_expression(date, now, 28800).unwrap()
            else {
                panic!();
            };
            assert_eq!((actual.start, actual.end), (expected.start, expected.end));
            assert_eq!(actual.source, TemporalSource::Inferred);
        }
    }

    #[test]
    fn normalized_day_requires_a_correct_model_date_and_uses_the_local_anchor() {
        let now = local_midnight(parse_iso_date("2024-02-28").unwrap(), UtcOffset::UTC);
        for (expression, proposed, offset) in [
            ("明天", "2024-02-29", 28800),
            ("today", "2024-02-27", -28800),
            ("前天", "2024-02-26", 28800),
            ("大后天", "2024-03-02", 28800),
            ("十一天后", "2024-03-10", 28800),
            ("three days ago", "2024-02-25", 28800),
        ] {
            assert!(normalized_date(expression, None, now, offset).is_err());
            let TemporalResolution::Range(actual) =
                normalized_date(expression, Some(proposed), now, offset)
                    .unwrap()
                    .unwrap()
            else {
                panic!()
            };
            assert_eq!(actual.source, TemporalSource::Inferred);
            assert_eq!(actual.end.0 - actual.start.0, 86400);
            assert_eq!(
                OffsetDateTime::from_unix_timestamp(actual.start.0)
                    .unwrap()
                    .to_offset(UtcOffset::from_whole_seconds(offset).unwrap())
                    .date()
                    .to_string(),
                proposed
            );
        }
        for proposed in [
            "2024-02-28",
            "2024-02-30",
            "2024-2-29",
            " 2024-02-29",
            "2024-02-29T00:00:00",
            "",
        ] {
            assert!(
                normalized_date("明天", Some(proposed), now, 28800).is_err(),
                "{proposed}"
            );
        }
        assert!(normalized_date("明天", Some("2024-02-29"), now, 86400).is_err());
        assert!(normalized_date("4294967295天后", Some("2024-02-29"), now, 28800).is_err());
        assert!(
            normalized_date("2024-02-29", None, now, 28800)
                .unwrap()
                .is_none()
        );
        let TemporalResolution::Range(absolute) =
            normalized_date("2024-02-29", Some("2024-02-29"), now, 28800)
                .unwrap()
                .unwrap()
        else {
            panic!()
        };
        assert_eq!(absolute.source, TemporalSource::Explicit);
        assert!(normalized_date("2024-02-29", Some("2024-03-01"), now, 28800).is_err());
    }

    #[test]
    fn normalized_calendar_preserves_month_year_and_inclusive_expiry_roles() {
        let now = local_midnight(parse_iso_date("2025-12-31").unwrap(), UtcOffset::UTC);
        for (expression, start, end, last_day) in [
            ("下个月", "2026-01-01", "2026-02-01", "2026-01-31"),
            ("last year", "2024-01-01", "2025-01-01", "2024-12-31"),
            ("去年二月", "2024-02-01", "2024-03-01", "2024-02-29"),
        ] {
            for (a, b) in [
                (None, None),
                (Some(start), None),
                (None, Some(end)),
                (Some(start), Some(last_day)),
            ] {
                assert!(normalized_calendar(expression, a, b, now, 28800).is_err());
            }
            let actual = normalized_calendar(expression, Some(start), Some(end), now, 28800)
                .unwrap()
                .unwrap();
            assert_eq!(
                actual,
                resolve_temporal_expression(expression, now, 28800).unwrap()
            );
            assert!(normalized_expiry_date(expression, None, now, 28800).is_err());
            assert!(normalized_expiry_date(expression, Some(start), now, 28800).is_err());
            let TemporalResolution::Range(expiry) =
                normalized_expiry_date(expression, Some(last_day), now, 28800)
                    .unwrap()
                    .unwrap()
            else {
                panic!()
            };
            let TemporalResolution::Range(validity) = actual else {
                panic!()
            };
            assert_eq!(expiry.end, validity.end);
            assert_eq!(expiry.end.0 - expiry.start.0, 86400);
        }
        assert!(
            normalized_calendar("明天", Some("2026-01-01"), Some("2026-01-02"), now, 28800)
                .is_err()
        );
        assert!(
            normalized_calendar("明天", Some("2026-01-01"), None, now, 28800)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn normalized_dates_do_not_truncate_modifiers_or_invent_dates_for_references() {
        let now = Timestamp(1790514274);
        for expression in [
            "明天早上",
            "三天后左右",
            "3天内",
            "从明天起",
            "明天到后天",
            "大大前天",
            "这个月",
            "现在",
            "最近",
            "旅行结束后",
            "等合适的时候",
        ] {
            assert!(
                normalized_calendar(expression, None, None, now, 28800)
                    .unwrap()
                    .is_none(),
                "{expression}"
            );
            assert!(
                normalized_calendar(expression, Some("2026-09-28"), None, now, 28800).is_err(),
                "{expression}"
            );
            assert!(
                normalized_expiry_date(expression, Some("2026-09-28"), now, 28800).is_err(),
                "{expression}"
            );
        }
    }
}
