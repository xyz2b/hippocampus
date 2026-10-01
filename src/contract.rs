//! 第一阶段契约类型（docs/architecture/contracts.md §1、§2.1、§2.4、§3.2）。
//!
//! 这里只定义形状：JSON 字段名与契约逐字一致，拒绝未知字段，`null` 与缺省按契约区分。
//! Quote 是否逐字存在、日历是否合法、目录与任务是否允许等机械校验属于 W2（§2.2），
//! 不在解码时做。

use serde::{Deserialize, Serialize};

use crate::{MemoryId, SupersedeReason, UpdateState};

/// 契约里“必须出现、可以为 null”的字段：缺省视为错误，不当作 null。
fn required<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

/// §1.1 原文片段。只核验逐字存在，不核验语义。
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Quote {
    pub text: String,
}

/// §1.1 来源引用：已保存的来源，或本次请求的当前消息。
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SourceRef {
    Stored { source_id: String },
    Current {},
}

/// §1.1 跨来源依据。
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EvidenceRef {
    pub source: SourceRef,
    pub quote: Quote,
}

/// §1.2 时间的角色：事件时间、事实成立区间、用户说的保存期限。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TimeRole {
    Event,
    Validity,
    Retention,
}

/// §1.2 相对时间的单位。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TimeUnit {
    Day,
    Week,
    Month,
    Year,
}

/// §1.2 时间含义。日期保持模型给出的 `YYYY-MM-DD` 文本，日历合法性由 W2 检查。
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Interp {
    Date {
        date: String,
    },
    Range {
        start: String,
        end_inclusive: String,
    },
    Relative {
        unit: TimeUnit,
        offset: i64,
    },
    Month {
        year_offset: i64,
        month: u8,
    },
    Anchored {
        anchor_quote: Quote,
        unit: TimeUnit,
        offset: i64,
    },
    Recurring {},
    Unknown {},
}

/// §1.2 相对时间的起点；第一阶段固定为原文接收日。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TimeAnchor {
    SaidOn,
}

/// §1.2 时间解释：模型给出原文与含义，代码负责日历、时区与算术。
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TimeInterp {
    pub role: TimeRole,
    pub quote: Quote,
    pub interp: Interp,
    pub anchor: TimeAnchor,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionStatus {
    Resolved,
    Unknown,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Precision {
    Day,
    Week,
    Month,
    Year,
    Unknown,
}

/// §1.2 代码给出的换算结果（左闭右开，Unix 秒）；未知时端点为 null。
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TimeResolution {
    pub status: ResolutionStatus,
    #[serde(deserialize_with = "required")]
    pub start: Option<i64>,
    #[serde(deserialize_with = "required")]
    pub end_exclusive: Option<i64>,
    pub precision: Precision,
}

/// §1.3 文字限定语；`resolved` 是模型对是否理解该表达的声明。
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TextQualifier {
    pub quote: Quote,
    pub resolved: bool,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Polarity {
    Affirm,
    Negate,
    Unknown,
}

/// §1.3 偏好强度；候选上 `null` 表示没有强弱表达，`unknown` 表示有表达但不能归类。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Strength {
    Weak,
    Moderate,
    Strong,
    Unknown,
}

/// §1.4 主体：目录内实体，或保留原文指代待澄清。
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(untagged)]
pub enum Subject {
    Entity(EntitySubject),
    Unresolved(UnresolvedSubject),
}

#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EntitySubject {
    pub entity_id: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct UnresolvedSubject {
    pub unresolved: Quote,
}

/// §1.5 值；每个分支都保留原文依据。
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Value {
    Text { text: String, quote: Quote },
    Integer { integer: i64, quote: Quote },
    Boolean { boolean: bool, quote: Quote },
    Entity { entity_id: String, quote: Quote },
}

/// §2.1 候选断言的言语类别。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Speech {
    UserStatement,
    Question,
    Hypothetical,
    Reported,
    Plan,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CandidateKind {
    Assertion,
    Retraction,
}

/// §2.1 w1.v1 候选。`relation` 为目录内关系 ID 或通用关系 `fact`；`memory_type` 由代码对照
/// 关系目录核对（W2），这里保留模型给出的文本。
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    pub kind: CandidateKind,
    pub speech: Speech,
    pub subject: Subject,
    pub relation: String,
    pub memory_type: String,
    #[serde(deserialize_with = "required")]
    pub value: Option<Value>,
    pub sentence: Quote,
    pub polarity: Polarity,
    #[serde(deserialize_with = "required")]
    pub strength: Option<Strength>,
    #[serde(deserialize_with = "required")]
    pub uncertainty: Option<TextQualifier>,
    pub conditions: Vec<TextQualifier>,
    pub times: Vec<TimeInterp>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RequestKind {
    Remember,
    Retention,
    DoNotStore,
    Forget,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RequestObject {
    Source,
    Facts,
    Unknown,
}

#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RequestTarget {
    pub object: RequestObject,
    pub candidate_indices: Vec<usize>,
    #[serde(deserialize_with = "required")]
    pub quote: Option<Quote>,
}

/// §2.1 整条消息一次判断出的记忆请求。
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MemoryRequest {
    pub kind: RequestKind,
    pub quote: Quote,
    pub target: RequestTarget,
    #[serde(deserialize_with = "required")]
    pub deadline: Option<TimeInterp>,
}

/// §2.1 W1 输出。
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct W1Output {
    pub memory_requests: Vec<MemoryRequest>,
    pub candidates: Vec<Candidate>,
}

/// §2.4 W4 输入里的一个候选。
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct W4Candidate {
    pub index: usize,
    #[serde(deserialize_with = "required")]
    pub committed_id: Option<MemoryId>,
    pub candidate: Candidate,
    pub source: SourceRef,
    pub said_on: String,
    pub time_resolutions: Vec<TimeResolution>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct W4Coverage {
    pub complete: bool,
    pub omitted: usize,
}

/// §2.4 W4 输入。
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct W4Input {
    pub candidates: Vec<W4Candidate>,
    pub records: Vec<RecordView>,
    pub relations: Vec<RelationLink>,
    pub coverage: W4Coverage,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum DecisionStatus {
    Decided,
    Pending,
}

/// §2.4 W4 动作。pending 时动作为 JSON null（`Option::None`），不是字符串 "null"。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    New,
    Reinforce,
    StateChange,
    Correction,
    Refine,
    Retract,
    Coexist,
    Conflict,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PendingReason {
    TargetUnknown,
    ScopeIncomplete,
    UnclearRelation,
}

/// §2.4 W4 的一个决定。
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct W4Decision {
    pub candidate: usize,
    pub status: DecisionStatus,
    #[serde(deserialize_with = "required")]
    pub action: Option<Action>,
    pub targets: Vec<MemoryId>,
    pub resolves_relations: Vec<String>,
    pub basis: Vec<EvidenceRef>,
    #[serde(deserialize_with = "required")]
    pub effective: Option<TimeInterp>,
    #[serde(deserialize_with = "required")]
    pub pending_reason: Option<PendingReason>,
}

/// §2.4 W4 输出。
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct W4Output {
    pub decisions: Vec<W4Decision>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RelationKind {
    Conflict,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RelationStatus {
    Open,
    Resolved,
    Invalidated,
}

/// §2.4 冲突关系。
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RelationLink {
    pub id: String,
    pub kind: RelationKind,
    pub members: Vec<MemoryId>,
    pub conditions: Vec<TextQualifier>,
    pub times: Vec<TimeInterp>,
    pub basis: Vec<EvidenceRef>,
    pub status: RelationStatus,
    #[serde(deserialize_with = "required")]
    pub resolution: Option<EvidenceRef>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum StoredStatus {
    Active,
    Superseded,
    Expired,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Ended {
    NotEnded,
    Known,
    EndUnknown,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Grade {
    Committed,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum LegacyGap {
    SourceMissing,
    SentenceMissing,
    QualifiersUnmapped,
    TimeUnmapped,
}

/// §3.2 完整记录视图。
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CurrentRecordView {
    pub id: MemoryId,
    pub stored_status: StoredStatus,
    #[serde(deserialize_with = "required")]
    pub termination_reason: Option<SupersedeReason>,
    pub ended: Ended,
    pub candidate: Candidate,
    pub evidence: Vec<EvidenceRef>,
    pub grade: Grade,
    pub said_on: String,
    pub time_resolutions: Vec<TimeResolution>,
    pub history: Vec<MemoryId>,
    pub open_relations: Vec<String>,
    pub update: UpdateState,
}

/// §3.2 旧记录视图。`data` 是第一阶段开始前 Memory 的序列化形状，原样保留，不补造候选。
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LegacyRecordView {
    pub id: MemoryId,
    pub data: crate::Memory,
    pub evidence: Vec<EvidenceRef>,
    pub grade: Grade,
    #[serde(deserialize_with = "required")]
    pub update: Option<UpdateState>,
    pub gaps: Vec<LegacyGap>,
}

/// §3.2 `RecordView = CurrentRecordView | LegacyRecordView`，按 `format` 判别。
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(tag = "format", rename_all = "snake_case")]
pub enum RecordView {
    Current(CurrentRecordView),
    Legacy(LegacyRecordView),
}

impl RecordView {
    pub fn id(&self) -> MemoryId {
        match self {
            RecordView::Current(view) => view.id,
            RecordView::Legacy(view) => view.id,
        }
    }
}

/// W5 组里随候选保存的完整新候选（方案 a：放在回执里，不新增状态字段）。一条记录的完整候选
/// 不可变：取产生它的那次提交；重判只能复用完全相同的完整候选，改变候选须显式重建。
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ContractCandidate {
    pub candidate: Candidate,
    pub source: SourceRef,
    pub said_on: String,
    pub time_resolutions: Vec<TimeResolution>,
}

/// 投影时由运行时提供、模型不能给出的上下文。
#[derive(Clone, Debug)]
pub struct ProjectionContext {
    pub owner_id: crate::UserId,
    pub session_id: crate::SessionId,
    pub turn_id: crate::TurnId,
    pub evidence_id: crate::EvidenceId,
    pub observed_at: crate::Timestamp,
    /// 可信任务 ID；有则作为任务范围写入，由代码维护，不来自模型。
    pub task_id: Option<crate::TaskId>,
    /// W3 已核验、绑定到这条候选的 remember 请求（contracts 2.1、2.3）。
    pub explicit_memory_request: bool,
}

/// 为什么不能投影为确定的事实记录。候选与原文照常保留，只是不提交为现有 Memory。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Unprojectable {
    UnresolvedSubject,
    NotUserStatement(Speech),
    UnknownMemoryType(String),
    MissingValue,
    /// 非偏好、约束的否定断言，或任何类型的方向未知：现有 Memory 无法表达。
    Polarity(Polarity),
    UnknownStrength,
}

/// 投影结果。`lossless = false` 表示完整候选里有现有 Memory 表达不了的限定语（条件、不确定
/// 表达、除单个已换算有效期以外的时间）：这类记录在旧路径里只能作为原文上下文，不能作为
/// 无限定事实答复或参与 scope 关系判断（复审 0015）。完整限定语保存在 `ContractCandidate`。
#[derive(Clone, Debug, PartialEq)]
pub struct Projection {
    pub candidate: crate::CandidateMemory,
    pub lossless: bool,
}

impl ContractCandidate {
    /// 现有 Memory 能否无损表达这条候选的限定语。
    pub fn lossless(&self) -> bool {
        let c = &self.candidate;
        c.conditions.is_empty() && c.uncertainty.is_none() && self.validity().is_ok()
    }

    /// 至多一个已换算的有效期：可以写成 valid_from / valid_to。其他时间（事件、周期、
    /// 保存期限、无法换算或多个有效期）只能保留在完整候选里。
    fn validity(&self) -> Result<Option<&TimeResolution>, ()> {
        match (
            self.candidate.times.as_slice(),
            self.time_resolutions.as_slice(),
        ) {
            ([], _) => Ok(None),
            ([time], [resolution])
                if time.role == TimeRole::Validity
                    && resolution.status == ResolutionStatus::Resolved =>
            {
                Ok(Some(resolution))
            }
            _ => Err(()),
        }
    }

    /// 投影为 W5 的执行形式。撤回只用于定位与校验，执行时不插入（W5 只接受撤回动作）。
    pub fn project(&self, ctx: &ProjectionContext) -> Result<Projection, Unprojectable> {
        use crate::{
            CandidateIntent, CandidateMemory, CandidateSource, CandidateSubject, Confidence,
            DatedValue, Evidence, MemoryType, MemoryValue, Predicate, RetentionTier, Scope,
            ScopeCondition, Temporal, TemporalSource, Timestamp,
        };
        let c = &self.candidate;
        let subject = match &c.subject {
            Subject::Entity(entity) => crate::EntityId::from(entity.entity_id.as_str()),
            Subject::Unresolved(_) => return Err(Unprojectable::UnresolvedSubject),
        };
        if c.speech != Speech::UserStatement {
            return Err(Unprojectable::NotUserStatement(c.speech));
        }
        let memory_type = match c.memory_type.as_str() {
            "fact" => MemoryType::Fact,
            "preference" => MemoryType::Preference,
            "constraint" => MemoryType::Constraint,
            "episode" => MemoryType::Episode,
            other => return Err(Unprojectable::UnknownMemoryType(other.to_owned())),
        };
        let directional = matches!(memory_type, MemoryType::Preference | MemoryType::Constraint);
        let polarity = match (c.polarity, directional, c.kind) {
            (Polarity::Unknown, _, _) => return Err(Unprojectable::Polarity(Polarity::Unknown)),
            (Polarity::Affirm, true, _) => Some(crate::Polarity::Positive),
            (Polarity::Negate, true, _) => Some(crate::Polarity::Negative),
            (Polarity::Affirm, false, _) => None,
            // 撤回本身就是否定，不是否定事实。
            (Polarity::Negate, false, CandidateKind::Retraction) => None,
            (Polarity::Negate, false, CandidateKind::Assertion) => {
                return Err(Unprojectable::Polarity(Polarity::Negate));
            }
        };
        let strength = match c.strength {
            None => None,
            Some(Strength::Weak) => Some(crate::Strength::Weak),
            Some(Strength::Moderate) => Some(crate::Strength::Moderate),
            Some(Strength::Strong) => Some(crate::Strength::Strong),
            Some(Strength::Unknown) => return Err(Unprojectable::UnknownStrength),
        };
        let value = match (&c.value, c.kind) {
            (Some(Value::Text { text, .. }), _) => MemoryValue::Text(text.clone()),
            (Some(Value::Integer { integer, .. }), _) => MemoryValue::Integer(*integer),
            (Some(Value::Boolean { boolean, .. }), _) => MemoryValue::Boolean(*boolean),
            (Some(Value::Entity { entity_id, .. }), _) => {
                MemoryValue::Entity(crate::EntityId::from(entity_id.as_str()))
            }
            // 撤回可以没有新值：占位为原句，只用于形状，执行时不会插入。
            (None, CandidateKind::Retraction) => MemoryValue::Text(c.sentence.text.clone()),
            (None, CandidateKind::Assertion) => return Err(Unprojectable::MissingValue),
        };
        let mut temporal = Temporal::observed_at(ctx.observed_at);
        if let Ok(Some(resolution)) = self.validity() {
            temporal.valid_from = resolution.start.map(|at| DatedValue {
                at: Timestamp(at),
                source: TemporalSource::Explicit,
            });
            temporal.valid_to = resolution.end_exclusive.map(|at| DatedValue {
                at: Timestamp(at),
                source: TemporalSource::Explicit,
            });
        }
        let scope = Scope {
            domain: None,
            conditions: ctx
                .task_id
                .iter()
                .map(|task| ScopeCondition::Task(task.clone()))
                .collect(),
        };
        let candidate = CandidateMemory {
            source_support: None,
            owner_id: ctx.owner_id.clone(),
            evidence: vec![Evidence::new(
                ctx.evidence_id.clone(),
                ctx.session_id.clone(),
                ctx.turn_id.clone(),
                c.sentence.text.clone(),
            )],
            subject: CandidateSubject::Resolved(subject),
            memory_type,
            predicate: Predicate::from(c.relation.as_str()),
            value,
            polarity,
            strength,
            scope,
            retention_tier: RetentionTier::LongTerm,
            temporal,
            extraction_confidence: Confidence::High,
            certainty: if c.uncertainty.is_some() {
                Confidence::Low
            } else {
                Confidence::High
            },
            source: CandidateSource::UserStatement,
            intent: match c.kind {
                CandidateKind::Assertion => CandidateIntent::Assert,
                CandidateKind::Retraction => CandidateIntent::Retract { target: None },
            },
            explicit_memory_request: ctx.explicit_memory_request,
            // W5 不跑准入：Episode 的跨轮价值由 W3 / 绑定的 remember 请求 / W4 更新结果在组装
            // W5 组之前决定，这里的值不参与任何判断。
            cross_turn_useful: true,
            tags: Vec::new(),
        };
        Ok(Projection {
            candidate,
            lossless: self.lossless(),
        })
    }
}
