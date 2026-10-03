//! R1 纯机械核验：模型不能扩大可信检索范围，失败不丢失本轮原文。
use crate::contract::*;
use crate::{EntityId, TaskId, w2};

pub struct Context<'a> {
    pub query: &'a str,
    pub context: &'a [String],
    /// 运行时提供的、已限定归属的目录。
    pub entities: &'a [EntityId],
    pub relations: &'a [w2::Relation],
    /// None 表示目录内不另限主体；Some([]) 表示没有获准主体，不是全目录。
    pub subject_ids: Option<&'a [EntityId]>,
    pub task_id: Option<&'a TaskId>,
    pub said_on: &'a str,
    pub utc_offset_seconds: i32,
    /// 仅由代码提供，不能由 R1 输出登记新单位。
    pub supported_units: &'a [&'a str],
}

/// ② 段的检索输入：output=None 时没有模型过滤条件，仍保留全部可信范围与原问题。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Plan {
    pub query: String,
    pub context: Vec<String>,
    pub subject_ids: Option<Vec<EntityId>>,
    pub task_id: Option<TaskId>,
    pub output: Option<R1Output>,
    pub time_resolution: Option<TimeResolution>,
    pub filter_time_resolution: Option<TimeResolution>,
}

impl Plan {
    pub fn need(&self) -> R1Need {
        self.output.as_ref().map_or(R1Need::Judge, |o| o.need)
    }

    pub fn current_evidence(&self) -> R1CurrentEvidence {
        self.output
            .as_ref()
            .map_or(R1CurrentEvidence::Unknown, |o| o.current_evidence)
    }

    /// 仅标记送审证据资格；调用 R4 另受 need/预算约束，不认证本轮文本已经是事实。
    pub fn review_current_message(&self) -> bool {
        self.current_evidence() != R1CurrentEvidence::QueryOnly
    }

    pub fn needs_clarification(&self) -> bool {
        self.output
            .as_ref()
            .is_some_and(|o| !matches!(o.subject, R1Subject::Entity(_)))
    }

    pub fn degraded_reasons(&self) -> &'static [&'static str] {
        if self.output.is_none() {
            &["r1_not_understood"]
        } else {
            &[]
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    InvalidOutput,
    InvalidContext(&'static str),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidOutput => f.write_str("r1_not_understood"),
            Self::InvalidContext(reason) => write!(f, "invalid_r1_context:{reason}"),
        }
    }
}
impl std::error::Error for Error {}

pub fn decode(text: &str) -> Result<R1Output, Error> {
    let output = serde_json::from_str(text).map_err(|_| Error::InvalidOutput)?;
    shape(&output)?;
    Ok(output)
}

fn shape(output: &R1Output) -> Result<(), Error> {
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

/// 解码失败或 R1 未调用时也走同一个降级入口；可信上下文错误单独返回，不猜授权范围。
pub fn not_understood(ctx: &Context<'_>) -> Result<Plan, Error> {
    if crate::temporal::parse_iso_date(ctx.said_on).is_none() {
        return Err(Error::InvalidContext("said_on"));
    }
    if time::UtcOffset::from_whole_seconds(ctx.utc_offset_seconds).is_err() {
        return Err(Error::InvalidContext("timezone"));
    }
    if ctx.task_id.is_some_and(|id| id.0.trim().is_empty()) {
        return Err(Error::InvalidContext("task_id"));
    }
    if ctx.entities.iter().any(|id| id.0.trim().is_empty())
        || ctx
            .subject_ids
            .is_some_and(|ids| ids.iter().any(|id| !ctx.entities.contains(id)))
    {
        return Err(Error::InvalidContext("entities"));
    }
    if ctx.relations.iter().any(|r| r.id.trim().is_empty()) {
        return Err(Error::InvalidContext("relations"));
    }
    if ctx.supported_units.iter().any(|u| u.trim().is_empty()) {
        return Err(Error::InvalidContext("units"));
    }
    Ok(Plan {
        query: ctx.query.into(),
        context: ctx.context.to_vec(),
        subject_ids: ctx.subject_ids.map(<[EntityId]>::to_vec),
        task_id: ctx.task_id.cloned(),
        output: None,
        time_resolution: None,
        filter_time_resolution: None,
    })
}

/// 任一模型字段失效，整份理解降级；不保留部分模型过滤条件。
pub fn validate(mut output: R1Output, ctx: &Context<'_>) -> Result<Plan, Error> {
    let mut plan = not_understood(ctx)?;
    let checked = (|| {
        shape(&output).ok()?;
        let allowed = |id: &str| {
            let id = EntityId::from(id);
            ctx.entities.contains(&id) && ctx.subject_ids.is_none_or(|ids| ids.contains(&id))
        };
        match &mut output.subject {
            R1Subject::Entity(subject) if allowed(&subject.entity_id) => {}
            R1Subject::Unresolved(subject) => restore(&mut subject.unresolved, ctx)?,
            R1Subject::Ambiguous(subject)
                if subject.ambiguous.len() >= 2
                    && subject.ambiguous.iter().all(|id| allowed(id))
                    && subject
                        .ambiguous
                        .iter()
                        .collect::<std::collections::BTreeSet<_>>()
                        .len()
                        == subject.ambiguous.len() => {}
            _ => return None,
        }
        for condition in &mut output.conditions {
            restore(&mut condition.quote, ctx)?;
        }
        if output
            .plan
            .filter
            .relation
            .as_ref()
            .is_some_and(|id| !ctx.relations.iter().any(|r| &r.id == id))
            || output
                .plan
                .unit
                .as_ref()
                .is_some_and(|unit| !ctx.supported_units.contains(&unit.as_str()))
        {
            return None;
        }
        let time = match output.time.as_mut() {
            Some(time) => Some(check_time(time, ctx)?),
            None => None,
        };
        let filter_time = match output.plan.filter.time.as_mut() {
            Some(time) => Some(check_time(time, ctx)?),
            None => None,
        };
        Some((time, filter_time))
    })();
    if let Some((time, filter_time)) = checked {
        plan.output = Some(output);
        plan.time_resolution = time;
        plan.filter_time_resolution = filter_time;
    }
    Ok(plan)
}

fn restore(quote: &mut Quote, ctx: &Context<'_>) -> Option<()> {
    for text in std::iter::once(ctx.query).chain(ctx.context.iter().map(String::as_str)) {
        if let Some(range) = locate_quote(text, &quote.text) {
            quote.text = text[range].into();
            return Some(());
        }
    }
    None
}

fn check_time(time: &mut TimeInterp, ctx: &Context<'_>) -> Option<TimeResolution> {
    if time.role == TimeRole::Retention {
        return None;
    }
    restore(&mut time.quote, ctx)?;
    if let Interp::Anchored { anchor_quote, .. } = &mut time.interp {
        restore(anchor_quote, ctx)?;
    }
    w2::resolve_time_interp(time, ctx.said_on, ctx.utc_offset_seconds).ok()
}
