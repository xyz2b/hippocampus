//! r4.v3 的机械核验与计算。引用/结构失效拒绝整份输出；不能精确计算单独返回不足。
use std::collections::{BTreeMap, BTreeSet};

use crate::contract::*;
use crate::{MemoryId, UpdateStatus, r1};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidOutput(pub &'static str);
impl std::fmt::Display for InvalidOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("model_invalid_output")
    }
}
impl std::error::Error for InvalidOutput {}

pub struct Context<'a> {
    pub evidence: &'a RecallEvidence,
    pub plan: &'a r1::Plan,
    pub request: &'a RecallRequest,
    pub utc_offset_seconds: i32,
    /// 运行时从同一快照取得的来源接收日；不能由模型给出。
    pub source_dates: &'a BTreeMap<String, String>,
}

pub struct Evaluated {
    pub output: R4Output,
    pub computed: Option<Computed>,
    pub compute_insufficient: bool,
}

fn nonblank(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::String(s) => !s.trim().is_empty(),
        serde_json::Value::Array(a) => a.iter().all(nonblank),
        serde_json::Value::Object(o) => o.values().all(nonblank),
        _ => true,
    }
}

pub fn decode(text: &str) -> Result<R4Output, InvalidOutput> {
    let output = serde_json::from_str(text).map_err(|_| InvalidOutput("schema"))?;
    shape(&output)?;
    Ok(output)
}

fn shape(output: &R4Output) -> Result<(), InvalidOutput> {
    if !nonblank(&serde_json::to_value(output).map_err(|_| InvalidOutput("schema"))?) {
        return Err(InvalidOutput("blank"));
    }
    Ok(())
}

pub(crate) fn record_evidence(record: &RecordView) -> &[EvidenceRef] {
    match record {
        RecordView::Current(v) => &v.evidence,
        RecordView::Legacy(v) => &v.evidence,
    }
}

/// 短片段也须落在该记录的依据中；同 source_id 的另一条断言不能冒充这条记录的依据。
pub(crate) fn supports_record(evidence: &EvidenceRef, record: &RecordView) -> bool {
    record_evidence(record).iter().any(|e| {
        e.source == evidence.source && locate_quote(&e.quote.text, &evidence.quote.text).is_some()
    })
}

impl Context<'_> {
    fn fragments(&self, source: &SourceRef) -> Vec<&str> {
        match source {
            SourceRef::Current {} if self.plan.review_current_message() => self
                .evidence
                .current_message
                .as_ref()
                .map(|c| vec![c.text.as_str()])
                .unwrap_or_default(),
            SourceRef::Current {} => Vec::new(),
            SourceRef::Stored { source_id } => self
                .evidence
                .raw
                .iter()
                .filter(|r| r.source_id == *source_id)
                .map(|r| r.quote.text.as_str())
                .chain(
                    self.evidence
                        .records
                        .iter()
                        .flat_map(record_evidence)
                        .filter(|e| e.source == *source)
                        .map(|e| e.quote.text.as_str()),
                )
                .chain(
                    self.evidence
                        .relations
                        .iter()
                        .flat_map(|r| r.basis.iter().chain(r.resolution.iter()))
                        .filter(|e| e.source == *source)
                        .map(|e| e.quote.text.as_str()),
                )
                .collect(),
        }
    }

    fn restore(&self, quote: &mut Quote, source: &SourceRef) -> Result<(), InvalidOutput> {
        for text in self.fragments(source) {
            if let Some(range) = locate_quote(text, &quote.text) {
                quote.text = text[range].into();
                return Ok(());
            }
        }
        Err(InvalidOutput("quote_or_source"))
    }

    fn refs(&self, evidence: &mut [EvidenceRef]) -> Result<(), InvalidOutput> {
        if evidence.is_empty() {
            return Err(InvalidOutput("empty_evidence"));
        }
        for e in evidence {
            self.restore(&mut e.quote, &e.source)?;
        }
        Ok(())
    }

    fn qualifier(&self, quote: &mut Quote, evidence: &[EvidenceRef]) -> Result<(), InvalidOutput> {
        for e in evidence {
            if self.restore(quote, &e.source).is_ok() {
                return Ok(());
            }
        }
        Err(InvalidOutput("qualifier_source"))
    }

    fn time(
        &self,
        time: &mut TimeInterp,
        source: &SourceRef,
    ) -> Result<TimeResolution, InvalidOutput> {
        self.restore(&mut time.quote, source)?;
        if let Interp::Anchored { anchor_quote, .. } = &mut time.interp {
            self.restore(anchor_quote, source)?;
        }
        let said_on = match source {
            SourceRef::Current {} => {
                &self
                    .evidence
                    .current_message
                    .as_ref()
                    .ok_or(InvalidOutput("current_missing"))?
                    .said_on
            }
            SourceRef::Stored { source_id } => self
                .source_dates
                .get(source_id)
                .ok_or(InvalidOutput("source_date_missing"))?,
        };
        let resolution = crate::w2::resolve_time_interp(time, said_on, self.utc_offset_seconds)
            .map_err(|_| InvalidOutput("time"))?;
        // W2 检查完整数字年月；补核对无年份的数字/中文月名，不能补成某一日。
        if let Some(month) = bare_month(&time.quote.text) {
            if resolution.status == ResolutionStatus::Resolved {
                let offset = time::UtcOffset::from_whole_seconds(self.utc_offset_seconds)
                    .map_err(|_| InvalidOutput("timezone"))?;
                let start = time::OffsetDateTime::from_unix_timestamp(
                    resolution.start.ok_or(InvalidOutput("time"))?,
                )
                .map_err(|_| InvalidOutput("time"))?
                .to_offset(offset);
                if resolution.precision != Precision::Month || start.month() as u8 != month {
                    return Err(InvalidOutput("time_precision"));
                }
            }
        }
        Ok(resolution)
    }

    fn fact(&self, fact: &mut FactView) -> Result<(), InvalidOutput> {
        self.refs(&mut fact.evidence)?;
        let record = |id: &MemoryId| {
            self.evidence
                .records
                .iter()
                .find(|r| r.id() == *id)
                .ok_or(InvalidOutput("record_id"))
        };
        for id in fact.record_ids.iter().chain(&fact.supersedes_record_ids) {
            let r = record(id)?;
            if !fact.evidence.iter().any(|e| supports_record(e, r)) {
                return Err(InvalidOutput("record_basis"));
            }
        }
        if !fact.supersedes_record_ids.is_empty() {
            let old: Vec<_> = fact
                .supersedes_record_ids
                .iter()
                .map(record)
                .collect::<Result<_, _>>()?;
            if !fact
                .evidence
                .iter()
                .any(|e| !old.iter().any(|r| supports_record(e, r)))
            {
                return Err(InvalidOutput("new_basis"));
            }
        }
        if let Some(q) = &mut fact.uncertainty {
            self.qualifier(&mut q.quote, &fact.evidence)?;
        }
        for q in &mut fact.conditions {
            self.qualifier(&mut q.quote, &fact.evidence)?;
        }
        for t in &mut fact.times {
            let mut checked = None;
            for e in &fact.evidence {
                let mut copy = t.clone();
                if self.time(&mut copy, &e.source).is_ok() {
                    checked = Some(copy);
                    break;
                }
            }
            *t = checked.ok_or(InvalidOutput("fact_time"))?;
        }
        Ok(())
    }
}

fn bare_month(text: &str) -> Option<u8> {
    let text: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let month = text.strip_suffix('月')?;
    let month = month.rsplit_once('年').map_or(month, |(_, m)| m);
    month
        .parse::<u8>()
        .ok()
        .filter(|m| (1..=12).contains(m))
        .or_else(|| {
            [
                "一", "二", "三", "四", "五", "六", "七", "八", "九", "十", "十一", "十二",
            ]
            .iter()
            .position(|m| *m == month)
            .map(|i| i as u8 + 1)
        })
}

/// 先完整核验再计算；任何字段非法都不留下部分事实或部分数字。
pub fn evaluate(mut output: R4Output, ctx: &Context<'_>) -> Result<Evaluated, InvalidOutput> {
    shape(&output)?;
    for fact in &mut output.facts {
        ctx.fact(fact)?;
    }
    let Some(plan) = &mut output.compute else {
        return Ok(Evaluated {
            output,
            computed: None,
            compute_insufficient: false,
        });
    };
    let allowed = ctx
        .plan
        .output
        .as_ref()
        .ok_or(InvalidOutput("unapproved_compute"))?;
    let op = match plan.op {
        ComputeOp::Count => R1Op::Count,
        ComputeOp::Sum => R1Op::Sum,
        ComputeOp::Elapsed => R1Op::Elapsed,
        ComputeOp::Compare => R1Op::Compare,
    };
    if allowed.plan.op != op
        || allowed
            .plan
            .unit
            .as_ref()
            .is_some_and(|u| Some(u) != plan.unit.as_ref())
    {
        return Err(InvalidOutput("unapproved_compute"));
    }
    let mut ids = BTreeSet::new();
    let mut dates = BTreeMap::new();
    for item in &mut plan.items {
        if item.id.starts_with('@') || !ids.insert(item.id.clone()) {
            return Err(InvalidOutput("item_id"));
        }
        ctx.refs(&mut item.evidence)?;
        if let Some(q) = &item.quantity {
            decimal_text(&q.amount).ok_or(InvalidOutput("quantity"))?;
            if !item
                .evidence
                .iter()
                .any(|e| quantity_basis(q, &e.quote.text))
            {
                return Err(InvalidOutput("quantity_basis"));
            }
        }
        match (&mut item.date, &item.date_source) {
            (Some(date), Some(source)) if item.evidence.iter().any(|e| e.source == *source) => {
                dates.insert(item.id.clone(), ctx.time(date, source)?);
            }
            (None, None) => (),
            _ => return Err(InvalidOutput("date_source")),
        }
    }
    match plan.op {
        ComputeOp::Count | ComputeOp::Sum if !plan.operands.is_empty() => {
            return Err(InvalidOutput("operands"));
        }
        ComputeOp::Elapsed | ComputeOp::Compare => {
            if plan.operands.len() != 2 {
                return Err(InvalidOutput("operands"));
            }
            for (i, operand) in plan.operands.iter().enumerate() {
                if operand == "@request.at" {
                    if plan.op != ComputeOp::Elapsed || i != 0 {
                        return Err(InvalidOutput("request_operand"));
                    }
                } else if !ids.contains(operand) {
                    return Err(InvalidOutput("operand_id"));
                }
            }
        }
        _ => (),
    }
    let computed = calculate(plan, &dates, ctx);
    let compute_insufficient = computed.is_none();
    Ok(Evaluated {
        output,
        computed,
        compute_insufficient,
    })
}

/// 规范化十进制文本仅用于逐字数值核对；不从中文词语推导数字，不接受指数/NaN/Infinity。
fn decimal_text(text: &str) -> Option<String> {
    let negative = text.starts_with('-');
    let text = text.strip_prefix(['-', '+']).unwrap_or(text);
    let (whole, fraction) = text.split_once('.').map_or((text, ""), |(a, b)| (a, b));
    if (whole.is_empty() && fraction.is_empty())
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || !fraction.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let whole = whole.trim_start_matches('0');
    let whole = if whole.is_empty() { "0" } else { whole };
    let fraction = fraction.trim_end_matches('0');
    let sign = if negative && (whole != "0" || !fraction.is_empty()) {
        "-"
    } else {
        ""
    };
    Some(if fraction.is_empty() {
        format!("{sign}{whole}")
    } else {
        format!("{sign}{whole}.{fraction}")
    })
}

#[derive(Clone, Copy, Debug)]
struct Decimal {
    mantissa: i128,
    scale: u32,
}
impl Decimal {
    fn parse(text: &str) -> Option<Self> {
        let normalized = decimal_text(text)?;
        let scale = normalized.split_once('.').map_or(0, |(_, f)| f.len());
        if scale > 38 {
            return None;
        }
        Some(Self {
            mantissa: normalized.replace('.', "").parse().ok()?,
            scale: scale as u32,
        })
    }
    fn at_scale(self, scale: u32) -> Option<i128> {
        if self.mantissa == 0 {
            return Some(0);
        }
        self.mantissa
            .checked_mul(10_i128.checked_pow(scale.checked_sub(self.scale)?)?)
    }
    fn add(self, other: Self) -> Option<Self> {
        let scale = self.scale.max(other.scale);
        Some(Self {
            mantissa: self.at_scale(scale)?.checked_add(other.at_scale(scale)?)?,
            scale,
        })
    }
    fn compare(self, other: Self) -> Option<std::cmp::Ordering> {
        let scale = self.scale.max(other.scale);
        Some(self.at_scale(scale)?.cmp(&other.at_scale(scale)?))
    }
    fn shift(mut self, power: i32) -> Option<Self> {
        if power < 0 {
            self.scale = self.scale.checked_add(power.unsigned_abs())?;
        } else {
            let reduce = self.scale.min(power as u32);
            self.scale -= reduce;
            self.mantissa = self
                .mantissa
                .checked_mul(10_i128.checked_pow(power as u32 - reduce)?)?;
        }
        (self.scale <= 38).then_some(self)
    }
    fn text(self) -> String {
        let sign = if self.mantissa < 0 { "-" } else { "" };
        let mut digits = self.mantissa.unsigned_abs().to_string();
        if self.scale != 0 {
            if digits.len() <= self.scale as usize {
                digits = format!(
                    "{}{}",
                    "0".repeat(self.scale as usize + 1 - digits.len()),
                    digits
                );
            }
            digits.insert(digits.len() - self.scale as usize, '.');
        }
        decimal_text(&format!("{sign}{digits}")).unwrap()
    }
}

fn metric(unit: &str) -> Option<(&'static str, i32, &'static [&'static str])> {
    Some(match unit {
        "kg" | "千克" | "公斤" => ("mass", 0, &["kg", "千克", "公斤"]),
        "g" | "克" => ("mass", -3, &["g", "克"]),
        "mg" | "毫克" => ("mass", -6, &["mg", "毫克"]),
        "km" | "千米" | "公里" => ("length", 3, &["km", "千米", "公里"]),
        "m" | "米" => ("length", 0, &["m", "米"]),
        "cm" | "厘米" => ("length", -2, &["cm", "厘米"]),
        "mm" | "毫米" => ("length", -3, &["mm", "毫米"]),
        _ => return None,
    })
}

// ponytail: 只核准字面十进制与相邻单位（含固定 SI 别名）；中文数量解释另需获准规则。
fn quantity_basis(quantity: &Quantity, quote: &str) -> bool {
    let number = decimal_text(&quantity.amount);
    let mut start = None;
    for (end, ch) in quote
        .char_indices()
        .chain(std::iter::once((quote.len(), ' ')))
    {
        if ch.is_ascii_digit() || matches!(ch, '.' | '-' | '+') {
            start.get_or_insert(end);
        } else if let Some(begin) = start.take() {
            if decimal_text(&quote[begin..end]) == number
                && quantity
                    .unit
                    .as_deref()
                    .is_none_or(|unit| unit_neighbor(unit, quote, begin, end))
            {
                return true;
            }
        }
    }
    false
}

fn unit_neighbor(unit: &str, quote: &str, number_start: usize, number_end: usize) -> bool {
    let aliases = metric(unit)
        .map(|(_, _, aliases)| aliases)
        .unwrap_or(std::slice::from_ref(&unit));
    let before = quote[..number_start].trim_end();
    let after = quote[number_end..].trim_start();
    aliases.iter().any(|alias| {
        let positions = [
            before.ends_with(alias).then(|| before.len() - alias.len()),
            after.starts_with(alias).then(|| quote.len() - after.len()),
        ];
        positions.into_iter().flatten().any(|start| {
            let end = start + alias.len();
            // 克/米不能冒充千克/厘米，g/m 不能冒充 kg/cm。
            if [
                "千克", "公斤", "毫克", "千米", "公里", "厘米", "毫米", "kg", "mg", "km", "cm",
                "mm",
            ]
            .iter()
            .any(|long| {
                long.len() > alias.len()
                    && quote
                        .match_indices(long)
                        .any(|(s, _)| s <= start && end <= s + long.len())
            }) {
                return false;
            }
            if !alias.is_ascii() {
                return true;
            }
            !quote[..start]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphabetic())
                && !quote[end..]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic())
        })
    })
}

fn converted(q: &Quantity, unit: Option<&str>) -> Option<Decimal> {
    let value = Decimal::parse(&q.amount)?;
    if q.unit.as_deref() == unit {
        return Some(value);
    }
    let (from, f, _) = metric(q.unit.as_deref()?)?;
    let (to, t, _) = metric(unit?)?;
    (from == to).then_some(())?;
    value.shift(f - t)
}

fn complete(plan: &ComputePlan, ctx: &Context<'_>) -> bool {
    let c = &ctx.evidence.completeness;
    let covers = |id: &str| {
        plan.items
            .iter()
            .flat_map(|i| &i.evidence)
            .any(|e| matches!(&e.source, SourceRef::Stored {source_id} if source_id == id))
    };
    plan.coverage == ComputeCoverage::Complete
        && c.plan_complete
        && !c.retrieval_truncated
        && c.pending_updates == 0
        && c.enrichment_gaps == 0
        && c.pending_related_sources <= ctx.evidence.raw.len()
        && ctx.evidence.raw.iter().all(|r| covers(&r.source_id))
        && ctx.evidence.records.iter().all(|r| match r {
            RecordView::Current(v) => v.update.status == UpdateStatus::Judged,
            RecordView::Legacy(v) => v
                .update
                .as_ref()
                .is_some_and(|u| u.status == UpdateStatus::Judged),
        })
}

fn calculate(
    plan: &ComputePlan,
    dates: &BTreeMap<String, TimeResolution>,
    ctx: &Context<'_>,
) -> Option<Computed> {
    let mut result = Computed {
        op: plan.op,
        value: String::new(),
        unit: plan.unit.clone(),
        bound: ComputeBound::Exact,
        items: plan.items.clone(),
        operands: plan.operands.clone(),
        rounding: None,
        exact_days: None,
        approximate: None,
        request_time: None,
    };
    match plan.op {
        ComputeOp::Count => {
            let groups: BTreeSet<_> = plan
                .items
                .iter()
                .filter_map(|i| i.event_group.as_ref())
                .collect();
            let exact = complete(plan, ctx) && plan.items.iter().all(|i| i.event_group.is_some());
            if !exact && groups.is_empty() {
                return None;
            }
            result.value = groups.len().to_string();
            result.bound = if exact {
                ComputeBound::Exact
            } else {
                ComputeBound::AtLeast
            };
        }
        ComputeOp::Sum => {
            // 本版没有证明未计部分非负的 wire 字段；不把看见的非负数当作遗漏也非负。
            if !complete(plan, ctx) {
                return None;
            }
            let mut groups: BTreeMap<&str, Decimal> = BTreeMap::new();
            for item in &plan.items {
                let group = item.event_group.as_deref()?;
                let value = converted(item.quantity.as_ref()?, plan.unit.as_deref())?;
                if let Some(previous) = groups.insert(group, value) {
                    if previous.compare(value)? != std::cmp::Ordering::Equal {
                        return None;
                    }
                }
            }
            let mut sum = Decimal {
                mantissa: 0,
                scale: 0,
            };
            for value in groups.into_values() {
                sum = sum.add(value)?;
            }
            result.value = sum.text();
        }
        ComputeOp::Compare => {
            let left = plan.items.iter().find(|i| i.id == plan.operands[0])?;
            let right = plan.items.iter().find(|i| i.id == plan.operands[1])?;
            let order = if left.quantity.is_some() && right.quantity.is_some() {
                let unit = plan
                    .unit
                    .as_deref()
                    .or(left.quantity.as_ref()?.unit.as_deref());
                converted(left.quantity.as_ref()?, unit)?
                    .compare(converted(right.quantity.as_ref()?, unit)?)?
            } else {
                let a = dates.get(&left.id)?;
                let b = dates.get(&right.id)?;
                if a.status != ResolutionStatus::Resolved || b.status != ResolutionStatus::Resolved
                {
                    return None;
                }
                let (a0, a1, b0, b1) = (a.start?, a.end_exclusive?, b.start?, b.end_exclusive?);
                if a1 <= b0 {
                    std::cmp::Ordering::Less
                } else if b1 <= a0 {
                    std::cmp::Ordering::Greater
                } else if a0 == b0 && a1 == b1 && a.precision == Precision::Day && a1 - a0 == 86400
                {
                    std::cmp::Ordering::Equal
                } else {
                    return None;
                }
            };
            result.value = match order {
                std::cmp::Ordering::Less => "less",
                std::cmp::Ordering::Equal => "equal",
                std::cmp::Ordering::Greater => "greater",
            }
            .into();
        }
        ComputeOp::Elapsed => {
            let offset = time::UtcOffset::from_whole_seconds(ctx.utc_offset_seconds).ok()?;
            let date = |id: &str| {
                let r = dates.get(id)?;
                if r.status != ResolutionStatus::Resolved
                    || r.precision != Precision::Day
                    || r.end_exclusive?.checked_sub(r.start?)? != 86400
                {
                    return None;
                }
                Some(
                    time::OffsetDateTime::from_unix_timestamp(r.start?)
                        .ok()?
                        .to_offset(offset)
                        .date(),
                )
            };
            let end = if plan.operands[0] == "@request.at" {
                let local = time::OffsetDateTime::from_unix_timestamp(ctx.request.at)
                    .ok()?
                    .to_offset(offset)
                    .date();
                result.request_time = Some(RequestTime {
                    at: ctx.request.at,
                    local_date: local.to_string(),
                    tz_offset_seconds: ctx.utc_offset_seconds,
                    precision: Precision::Day,
                });
                local
            } else {
                date(&plan.operands[0])?
            };
            let start = date(&plan.operands[1])?;
            use crate::openai::ElapsedUnit;
            let (unit, rounding) = match plan.unit.as_deref()? {
                "day" => (ElapsedUnit::Day, ElapsedRounding::CalendarDays),
                "week" => (ElapsedUnit::Week, ElapsedRounding::NearestWeek),
                "month" => (ElapsedUnit::Month, ElapsedRounding::CompleteCalendarMonths),
                "year" => (ElapsedUnit::Year, ElapsedRounding::CompleteCalendarYears),
                _ => return None,
            };
            result.value = crate::openai::elapsed_count(start, end, unit)?.to_string();
            let days = (end - start).whole_days();
            result.rounding = Some(rounding);
            result.exact_days = Some(days);
            result.approximate = Some(matches!(unit, ElapsedUnit::Week) && days % 7 != 0);
        }
    }
    Some(result)
}
