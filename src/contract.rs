//! 第一阶段契约类型（docs/architecture/contracts.md §1、§2.1、§2.3 w3.v4、§2.4 w4.v5、§3.1 r1.v2、§3.2）。
//!
//! JSON 字段名与契约逐字一致，拒绝未知字段，`null` 与缺省按契约区分；包含 W3 输出核验。
//! Quote 是否逐字存在、日历是否合法、目录与任务是否允许等机械校验属于 W2（§2.2），
//! 不在解码时做。

use serde::{Deserialize, Serialize};

use crate::{MemoryId, SupersedeReason, UpdateState};

/// 契约里“必须出现、可以为 null”的字段：缺省视为错误，不当作 null。
pub(crate) fn required<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
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

/// §3.1 R1 主体；不扩展写入侧 Subject 的分支。
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(untagged)]
pub enum R1Subject {
    Entity(EntitySubject),
    Unresolved(UnresolvedSubject),
    Ambiguous(AmbiguousSubject),
}

#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AmbiguousSubject {
    pub ambiguous: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum R1Need {
    ContextOnly,
    Judge,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum R1CurrentEvidence {
    AssertionPresent,
    QueryOnly,
    Unknown,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum R1Type {
    Single,
    Aggregate,
    Comparison,
    Temporal,
    Open,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum R1Op {
    Lookup,
    List,
    Count,
    Sum,
    Compare,
    Elapsed,
}

/// §3.1 唯一允许的过滤字段；主体与任务取运行时可信范围。
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct R1Filter {
    #[serde(deserialize_with = "required")]
    pub relation: Option<String>,
    #[serde(deserialize_with = "required")]
    pub time: Option<TimeInterp>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct R1Plan {
    pub op: R1Op,
    pub filter: R1Filter,
    #[serde(deserialize_with = "required")]
    pub unit: Option<String>,
}

/// §3.1 r1.v2 模型输出；自然语言 relation/conditions 仅为检索提示。
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct R1Output {
    pub need: R1Need,
    pub current_evidence: R1CurrentEvidence,
    #[serde(rename = "type")]
    pub query_type: R1Type,
    pub subject: R1Subject,
    pub relation: String,
    pub conditions: Vec<TextQualifier>,
    #[serde(deserialize_with = "required")]
    pub time: Option<TimeInterp>,
    pub plan: R1Plan,
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

/// §2.1 w1.v4 候选。`relation` 为目录内关系 ID 或通用关系 `fact`；`memory_type` 由代码对照
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

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Supported,
    Contradicted,
    Insufficient,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewAspect {
    Subject,
    Relation,
    MemoryType,
    Value,
    Polarity,
    Strength,
    Uncertainty,
    Condition,
    Time,
    Speech,
}

#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ReviewProblem {
    pub aspect: ReviewAspect,
    pub quote: Quote,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum EpisodeValue {
    Useful,
    LowValue,
    Unknown,
    NotApplicable,
}

#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CandidateReview {
    pub candidate: usize,
    pub verdict: Verdict,
    pub problems: Vec<ReviewProblem>,
    pub episode_value: EpisodeValue,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CoverageStatus {
    Complete,
    Gaps,
}

#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct W3Coverage {
    pub status: CoverageStatus,
    pub missing: Vec<Quote>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RequestReview {
    pub request: usize,
    pub verdict: Verdict,
}

/// §2.3 w3.v4；形状及逐项覆盖由代码核验，不改变旧 SourceReview。
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct W3Output {
    pub reviews: Vec<CandidateReview>,
    pub coverage: W3Coverage,
    pub request_reviews: Vec<RequestReview>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct W3InvalidOutput;
impl std::fmt::Display for W3InvalidOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("model_invalid_output")
    }
}
impl std::error::Error for W3InvalidOutput {}

/// 内部核验回执，绑定完整 W2 结果和运行时来源快照；不是模型 wire 类型。
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ReviewedW3 {
    binding: serde_json::Value,
    output: W3Output,
}

impl W3Output {
    pub fn decode(text: &str) -> Result<Self, W3InvalidOutput> {
        serde_json::from_str(text).map_err(|_| W3InvalidOutput)
    }

    pub fn validate(
        mut self,
        batch: &crate::w2::Batch,
        source_snapshot: &serde_json::Value,
        text: &str,
    ) -> Result<ReviewedW3, W3InvalidOutput> {
        use std::collections::BTreeSet;
        let expected: BTreeSet<_> = batch
            .candidates
            .iter()
            .filter(|c| c.status != crate::w2::Status::Rejected)
            .map(|c| c.index)
            .collect();
        let actual: BTreeSet<_> = self.reviews.iter().map(|r| r.candidate).collect();
        let requests: BTreeSet<_> = self.request_reviews.iter().map(|r| r.request).collect();
        if expected != actual
            || actual.len() != self.reviews.len()
            || requests != batch.memory_requests.iter().map(|r| r.index).collect()
            || requests.len() != self.request_reviews.len()
        {
            return Err(W3InvalidOutput);
        }
        let restore = |q: &mut Quote| {
            let range = locate_quote(text, &q.text).ok_or(W3InvalidOutput)?;
            q.text = text[range].into();
            Ok(())
        };
        for review in &mut self.reviews {
            if (review.verdict == Verdict::Supported && !review.problems.is_empty())
                || (batch
                    .candidates
                    .iter()
                    .find(|c| c.index == review.candidate)
                    .unwrap()
                    .candidate
                    .memory_type
                    != "episode"
                    && review.episode_value != EpisodeValue::NotApplicable)
            {
                return Err(W3InvalidOutput);
            }
            for problem in &mut review.problems {
                restore(&mut problem.quote)?;
            }
        }
        for missing in &mut self.coverage.missing {
            restore(missing)?;
        }
        Ok(ReviewedW3 {
            binding: serde_json::json!([batch, source_snapshot]),
            output: self,
        })
    }
}

impl ReviewedW3 {
    pub fn output(&self) -> &W3Output {
        &self.output
    }

    pub fn for_input(
        &self,
        batch: &crate::w2::Batch,
        source_snapshot: &serde_json::Value,
        text: &str,
    ) -> Result<&W3Output, W3InvalidOutput> {
        if self.binding != serde_json::json!([batch, source_snapshot]) {
            return Err(W3InvalidOutput);
        }
        // 重开后的内部 JSON 也重新核验，不能只相信持久标记。
        let checked = self.output.clone().validate(batch, source_snapshot, text)?;
        if checked != *self {
            return Err(W3InvalidOutput);
        }
        Ok(&self.output)
    }
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
    #[serde(deserialize_with = "required")]
    pub said_on: Option<String>,
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
    pub records: Vec<RecordView2>,
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
    pub relation_scope: Option<RelationScope>,
    #[serde(deserialize_with = "required")]
    pub pending_reason: Option<PendingReason>,
}

/// §2.4 被引用限定语的所属断言：本决定的新候选，或某个 target 记录。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QualifierOwner {
    Candidate,
    Record(MemoryId),
}

impl Serialize for QualifierOwner {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            QualifierOwner::Candidate => serializer.serialize_str("candidate"),
            QualifierOwner::Record(id) => id.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for QualifierOwner {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match serde_json::Value::deserialize(deserializer)? {
            serde_json::Value::String(text) if text == "candidate" => Ok(QualifierOwner::Candidate),
            value @ serde_json::Value::Number(_) => serde_json::from_value(value)
                .map(QualifierOwner::Record)
                .map_err(serde::de::Error::custom),
            other => Err(serde::de::Error::custom(format!(
                "qualifier owner must be \"candidate\" or a record id, got {other}"
            ))),
        }
    }
}

/// §2.4 对已核验限定语的引用：`conditions[index]` 或 `times[index]`。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct QualifierRef {
    pub of: QualifierOwner,
    pub index: usize,
}

/// §2.4 冲突的适用范围，由 W4 引用新候选或目标已核验的限定语给出；都为空表示没有限定范围。
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RelationScope {
    pub conditions: Vec<QualifierRef>,
    pub times: Vec<QualifierRef>,
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
    #[serde(deserialize_with = "required")]
    pub said_on: Option<String>,
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

/// §3.2 / §3.5 来源整理状态；不改变旧路径的字符串回执。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum EnrichmentStatus {
    Queued,
    Running,
    Committed,
    CompletedNoFacts,
    NoCandidatesUnreviewed,
    CoverageGaps,
    Pending,
    AwaitingConfirmation,
    Failed,
    NeedsRejudge,
    BudgetExhausted,
    NotRequested,
    Deleted,
    NotStored,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RecallGrade {
    Committed,
    Uncommitted,
}

#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CurrentMessage {
    #[serde(deserialize_with = "required")]
    pub source_id: Option<String>,
    pub text: String,
    pub said_on: String,
    #[serde(deserialize_with = "required")]
    pub sequence: Option<u64>,
    pub enrichment: EnrichmentStatus,
    pub memory_requests: Vec<MemoryRequest>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RawEvidence {
    pub source_id: String,
    pub quote: Quote,
    #[serde(deserialize_with = "required")]
    pub said_on: Option<String>,
    pub sequence: u64,
    pub grade: RecallGrade,
    pub enrichment: EnrichmentStatus,
    pub memory_requests: Vec<MemoryRequest>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RecallCompleteness {
    pub retrieval_truncated: bool,
    pub pending_related_sources: usize,
    pub pending_updates: usize,
    pub enrichment_gaps: usize,
    pub plan_complete: bool,
}

/// R3 与 R4 的共享输入；current_evidence 的送审标记由运行时另行绑定，不扩展 wire。
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RecallEvidence {
    #[serde(deserialize_with = "required")]
    pub current_message: Option<CurrentMessage>,
    pub records: Vec<RecordView>,
    pub raw: Vec<RawEvidence>,
    pub relations: Vec<RelationLink>,
    pub completeness: RecallCompleteness,
}

#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RecallRequest {
    pub request_id: String,
    pub query: String,
    pub context: Vec<String>,
    #[serde(deserialize_with = "required")]
    pub task_id: Option<String>,
    pub at: i64,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RecallSnapshot {
    pub read_revision: u64,
    pub source_revision: u64,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RecallStatus {
    Answered,
    ContextOnly,
    Insufficient,
    Conflict,
    NeedsClarification,
    Degraded,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SemanticJudgment {
    Model,
    Skipped,
    Failed,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Qualification {
    Yes,
    No,
    Unknown,
}

/// §3.3 r4.v7；不把写入侧 insufficient/contradicted 枚举混入召回判断。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum R4Verdict {
    Supported,
    Insufficient,
    Conflict,
    NeedsClarification,
}

#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FactView {
    pub statement: String,
    pub evidence: Vec<EvidenceRef>,
    pub record_ids: Vec<MemoryId>,
    pub polarity: Polarity,
    #[serde(deserialize_with = "required")]
    pub strength: Option<Strength>,
    #[serde(deserialize_with = "required")]
    pub uncertainty: Option<TextQualifier>,
    pub conditions: Vec<TextQualifier>,
    pub times: Vec<TimeInterp>,
    pub current: Qualification,
    pub applies: Qualification,
    pub supersedes_record_ids: Vec<MemoryId>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ComputeCoverage {
    Complete,
    Incomplete,
    Unknown,
}

#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ComputePlan {
    pub op: ComputeOp,
    pub items: Vec<ComputeItem>,
    pub operands: Vec<String>,
    #[serde(deserialize_with = "required")]
    pub unit: Option<String>,
    pub coverage: ComputeCoverage,
}

#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct R4Output {
    pub verdict: R4Verdict,
    pub facts: Vec<FactView>,
    #[serde(deserialize_with = "required")]
    pub compute: Option<ComputePlan>,
}

/// §3.4：仅定义冻结形状；R4 核验与 R5 派生留给召回③。
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RecallFact {
    pub statement: String,
    pub evidence: Vec<EvidenceRef>,
    pub record_ids: Vec<MemoryId>,
    pub polarity: Polarity,
    #[serde(deserialize_with = "required")]
    pub strength: Option<Strength>,
    #[serde(deserialize_with = "required")]
    pub uncertainty: Option<TextQualifier>,
    pub conditions: Vec<TextQualifier>,
    pub times: Vec<TimeInterp>,
    pub current: Qualification,
    pub applies: Qualification,
    pub supersedes_record_ids: Vec<MemoryId>,
    pub grade: RecallGrade,
    #[serde(deserialize_with = "required")]
    pub update: Option<UpdateState>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Quantity {
    pub amount: String,
    #[serde(deserialize_with = "required")]
    pub unit: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ComputeItem {
    pub id: String,
    pub evidence: Vec<EvidenceRef>,
    #[serde(deserialize_with = "required")]
    pub quantity: Option<Quantity>,
    #[serde(deserialize_with = "required")]
    pub date: Option<TimeInterp>,
    #[serde(deserialize_with = "required")]
    pub date_source: Option<SourceRef>,
    #[serde(deserialize_with = "required")]
    pub event_group: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ComputeOp {
    Count,
    Sum,
    Elapsed,
    Compare,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ComputeBound {
    Exact,
    AtLeast,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ElapsedRounding {
    CalendarDays,
    NearestWeek,
    CompleteCalendarMonths,
    CompleteCalendarYears,
}

#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RequestTime {
    pub at: i64,
    pub local_date: String,
    pub tz_offset_seconds: i32,
    pub precision: Precision,
}

#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Computed {
    pub op: ComputeOp,
    pub value: String,
    #[serde(deserialize_with = "required")]
    pub unit: Option<String>,
    pub bound: ComputeBound,
    pub items: Vec<ComputeItem>,
    pub operands: Vec<String>,
    #[serde(deserialize_with = "required")]
    pub rounding: Option<ElapsedRounding>,
    #[serde(deserialize_with = "required")]
    pub exact_days: Option<i64>,
    #[serde(deserialize_with = "required")]
    pub approximate: Option<bool>,
    #[serde(deserialize_with = "required")]
    pub request_time: Option<RequestTime>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RecallHistory {
    pub statement: String,
    #[serde(deserialize_with = "required")]
    pub until: Option<String>,
    pub evidence: Vec<EvidenceRef>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RecallClarification {
    pub question: String,
    pub evidence: Vec<EvidenceRef>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RecallOutput {
    pub version: String,
    pub request: RecallRequest,
    pub status: RecallStatus,
    #[serde(deserialize_with = "required")]
    pub current_message: Option<CurrentMessage>,
    pub records: Vec<RecordView>,
    pub raw: Vec<RawEvidence>,
    pub facts: Vec<RecallFact>,
    #[serde(deserialize_with = "required")]
    pub computed: Option<Computed>,
    pub history: Vec<RecallHistory>,
    pub conflicts: Vec<RelationLink>,
    #[serde(deserialize_with = "required")]
    pub clarification: Option<RecallClarification>,
    pub semantic_judgment: SemanticJudgment,
    pub completeness: RecallCompleteness,
    pub retrieval: RecallRetrieval,
    pub degraded_reasons: Vec<String>,
    pub snapshot: RecallSnapshot,
}

/// Host-only index availability; never establishes evidence or enters R4's completeness.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalStatus {
    #[default]
    Disabled,
    NotNeeded,
    Ready,
    Pending,
    Unavailable,
}
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct RecallRetrieval {
    pub status: RetrievalStatus,
    pub sources_total: usize,
    pub sources_indexed: usize,
    pub segments_total: usize,
    pub segments_indexed: usize,
}

/// §3.2 w4.v2 输入用的完整记录视图：CurrentRecordView 的全部字段，加结束时间 `end`
/// （`ended = known` 时为保留精度的换算结果，否则为 null）。
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CurrentRecordView2 {
    pub id: MemoryId,
    pub stored_status: StoredStatus,
    #[serde(deserialize_with = "required")]
    pub termination_reason: Option<SupersedeReason>,
    pub ended: Ended,
    #[serde(deserialize_with = "required")]
    pub end: Option<TimeResolution>,
    pub candidate: Candidate,
    pub evidence: Vec<EvidenceRef>,
    pub grade: Grade,
    #[serde(deserialize_with = "required")]
    pub said_on: Option<String>,
    pub time_resolutions: Vec<TimeResolution>,
    pub history: Vec<MemoryId>,
    pub open_relations: Vec<String>,
    pub update: UpdateState,
}

/// §3.2 `RecordView2 = CurrentRecordView2 | LegacyRecordView`。
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(tag = "format", rename_all = "snake_case")]
pub enum RecordView2 {
    Current(CurrentRecordView2),
    Legacy(LegacyRecordView),
}

impl RecordView2 {
    pub fn id(&self) -> MemoryId {
        match self {
            RecordView2::Current(view) => view.id,
            RecordView2::Legacy(view) => view.id,
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
    #[serde(deserialize_with = "required")]
    pub said_on: Option<String>,
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
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
pub enum Unprojectable {
    UnresolvedSubject,
    NotUserStatement(Speech),
    UnknownMemoryType(String),
    MissingValue,
    /// 普通非方向型否定断言，或任何类型的方向未知；计划的极性保留在完整候选中。
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
        c.speech == Speech::UserStatement
            && c.conditions.is_empty()
            && c.uncertainty.is_none()
            && self.validity().is_ok()
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
        // 计划仅以 fact 执行存储；speech、极性与限定语仍以完整候选为权威，
        // 不能把现有 Memory 投影当作已发生的事实。
        let plan = c.speech == Speech::Plan && c.memory_type == "fact";
        if c.speech != Speech::UserStatement && !plan {
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
            (Polarity::Negate, false, CandidateKind::Assertion) if plan => None,
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

/// §1.1 在原文里定位 Quote：只忽略空白差异与引号排版（`’`/`‘` 与 `'`、`“`/`”` 与 `"`、
/// 重复的 `''`），片段必须连续。返回原文中的字节范围，调用方据此还原原文写法；英文、数字
/// 两端不能落在词的中间（`know` 里的 `now` 不算；`can’t` 不能只取 `can`）。
pub fn locate_quote(source: &str, quote: &str) -> Option<std::ops::Range<usize>> {
    // 规范化后的字符，连同它在原文里的字节范围。
    fn normalized(text: &str) -> Vec<(char, usize, usize)> {
        let mut out: Vec<(char, usize, usize)> = Vec::new();
        for (start, ch) in text.char_indices() {
            if ch.is_whitespace() {
                continue;
            }
            let ch = match ch {
                '’' | '‘' => '\'',
                '“' | '”' => '"',
                other => other,
            };
            let end = start + ch_len(text, start);
            if ch == '\'' && out.last().is_some_and(|(last, _, _)| *last == '\'') {
                // 重复的 '' 视为一个；范围并入前一个。
                out.last_mut().expect("checked").2 = end;
                continue;
            }
            out.push((ch, start, end));
        }
        out
    }
    fn ch_len(text: &str, start: usize) -> usize {
        text[start..].chars().next().map_or(0, char::len_utf8)
    }
    let haystack = normalized(source);
    let needle: Vec<char> = normalized(quote).into_iter().map(|(ch, _, _)| ch).collect();
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    let word = |ch: char| ch.is_ascii_alphanumeric();
    (0..=haystack.len() - needle.len()).find_map(|at| {
        let window = &haystack[at..at + needle.len()];
        if !window
            .iter()
            .map(|(ch, _, _)| *ch)
            .eq(needle.iter().copied())
        {
            return None;
        }
        let (_, start, _) = window[0];
        let (_, _, end) = window[window.len() - 1];
        // 片段的两端不能落在词的中间。词内的撇号（can’t、don't，含重复的 ''）连着两侧
        // 字母，算在词里：越过紧邻边界的撇号后，两侧都是字母或数字，就是词的中间。
        let apostrophe = |ch: char| matches!(ch, '\'' | '’' | '‘');
        let inside_word = |at: usize| {
            let left = source[..at].chars().rev().find(|ch| !apostrophe(*ch));
            let right = source[at..].chars().find(|ch| !apostrophe(*ch));
            left.is_some_and(word) && right.is_some_and(word)
        };
        if inside_word(start) || inside_word(end) {
            return None;
        }
        Some(start..end)
    })
}
