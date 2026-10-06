//! Mechanical ISO calendar parsing shared by W2/R1/R4.
use std::fmt;
use time::{Date, Month};
#[derive(Clone, Debug, Eq, PartialEq)]
/// 字面日期、日历锚点或时区无效时返回的机械核验错误。
pub struct TemporalResolveError(pub String);

impl fmt::Display for TemporalResolveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::error::Error for TemporalResolveError {}

pub(crate) fn parse_iso_date(value: &str) -> Option<Date> {
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
