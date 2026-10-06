//! Personal Memory V1 的核心数据模型。
//!
//! 这里只描述领域事实，不包含存储、LLM 或检索实现。最重要的边界是
//! `owner_id`（记忆属于谁）与 `subject_id`（记忆描述谁）彼此独立。

use std::fmt;

macro_rules! string_id {
    ($name:ident) => {
        #[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(
            /// 对业务透明的字符串标识；具体格式由边界层决定。
            pub String,
        );

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

string_id!(UserId);
string_id!(EntityId);
string_id!(TaskId);
string_id!(Predicate);
string_id!(Domain);
string_id!(EvidenceId);
string_id!(SessionId);
string_id!(TurnId);

#[derive(
    serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd,
)]
pub struct MemoryId(
    /// 当前 repository 内单调递增的版本标识。
    pub u64,
);

#[derive(
    serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd,
)]
pub struct Timestamp(
    /// 由调用方统一约定单位的时间值；测试中使用逻辑时间。
    pub i64,
);

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq)]
/// 支撑一条 Candidate 的可追溯原始证据。
pub struct Evidence {
    /// 原子证据的稳定 ID；重放同一证据时必须保持不变。
    pub id: EvidenceId,
    /// 证据所属 conversation session。
    pub session_id: SessionId,
    /// 证据在 session 内的 turn；与 session_id 共同标识消息来源。
    pub turn_id: TurnId,
    /// 支撑 Candidate 的原文引用；V1 保存文本，不只保存摘要。
    pub quote: String,
}

impl Evidence {
    pub fn new(
        id: EvidenceId,
        session_id: SessionId,
        turn_id: TurnId,
        quote: impl Into<String>,
    ) -> Self {
        Self {
            id,
            session_id,
            turn_id,
            quote: quote.into(),
        }
    }

    pub(crate) fn same_turn(&self, other: &Self) -> bool {
        self.session_id == other.session_id && self.turn_id == other.turn_id
    }
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
/// 一条 atomic memory 的唯一类型；Preference 排序，Constraint 过滤。
pub enum MemoryType {
    /// Entity 的状态、属性、关系或稳定行为模式。
    Fact,
    /// 影响未来选择排序的软倾向。
    Preference,
    /// 影响未来候选过滤的硬限制。
    Constraint,
    /// 已经发生过的事件或经历。
    Episode,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq)]
pub enum EntityType {
    Person,
    Place,
    Organization,
    Device,
    Product,
    Other(String),
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq)]
/// 可被多条 memory 引用的现实对象。
pub struct Entity {
    /// Entity 自身的稳定标识。
    pub id: EntityId,
    /// Entity 所属的用户记忆库；不表示 memory 的 subject 一定是该用户。
    pub owner_id: UserId,
    /// 用于约束解析和展示的 Entity 类别。
    pub entity_type: EntityType,
    /// 面向用户和调试输出的规范名称。
    pub canonical_name: String,
    /// 用于解析“孩子”“旧 Mac”等自然语言指代。
    pub aliases: Vec<String>,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq)]
pub enum EntityResolution {
    Resolved(EntityId),
    Ambiguous(Vec<EntityId>),
    NotFound,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
/// Predicate 的基数决定 semantic slot 是否包含 value。
pub enum Cardinality {
    /// 同 subject、type、predicate、scope 只能有一个当前值，如主要居住地。
    Single,
    /// 不同 value 可同时存在，如拥有多台设备。
    Multiple,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq)]
/// Predicate schema；cardinality 在 schema 层定义，避免每条 memory 重复存储。
pub struct PredicateDefinition {
    /// 被定义的语义谓词。
    pub predicate: Predicate,
    /// 该谓词允许一个当前值还是多个并存值。
    pub cardinality: Cardinality,
}

#[derive(
    serde::Serialize, serde::Deserialize, Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd,
)]
pub enum EntityOrRole {
    Entity(EntityId),
    Role(CompanionRole),
}

#[derive(
    serde::Serialize, serde::Deserialize, Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd,
)]
pub enum CompanionRole {
    Alone,
    Friends,
    Partner,
    Children,
    Family,
}

#[derive(
    serde::Serialize, serde::Deserialize, Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd,
)]
pub enum LocationScope {
    Domestic,
    International,
    Country(String),
    City(String),
}

#[derive(
    serde::Serialize, serde::Deserialize, Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd,
)]
pub enum TimeOfDay {
    Morning,
    Afternoon,
    Evening,
    Night,
}

#[derive(
    serde::Serialize, serde::Deserialize, Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd,
)]
pub enum TimeScope {
    Date(String),
    TimeOfDay(TimeOfDay),
}

#[derive(
    serde::Serialize, serde::Deserialize, Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd,
)]
/// Scope 是受控的适用条件集合，而不是自由文本场景标签。
pub enum ScopeCondition {
    Companion(EntityOrRole),
    Location(LocationScope),
    Time(TimeScope),
    Activity(String),
    Task(TaskId),
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, Eq, PartialEq)]
/// Memory 成立或应被应用时必须满足的条件。
pub struct Scope {
    /// 可选领域；`None` 表示不受领域限制。
    pub domain: Option<Domain>,
    /// 所有条件都满足时 scope 才适用。
    pub conditions: Vec<ScopeCondition>,
}

#[derive(
    serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd,
)]
/// 系统计划以什么级别保留 memory，与 `MemoryType` 无关。
pub enum RetentionTier {
    /// 通常有过期时间，可经独立用户证据升级。
    ShortTerm,
    /// 长期保留，但仍可按明确的过期规则淘汰。
    LongTerm,
    /// 不自动过期；仍允许 supersede、retract 和 correction。
    Durable,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
/// 已进入正式 Store 的 memory 生命周期状态。
pub enum MemoryStatus {
    Active,
    /// 已被新版本替换或被用户撤回，记录仍保留。
    Superseded,
    /// 不再用于当前查询，但可进入历史检索。
    Expired,
    Deleted,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
pub enum TemporalSource {
    Explicit,
    Inferred,
    Supersession,
    Unknown,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
/// 带来源的现实时间点。
pub struct DatedValue {
    /// 时间值。
    pub at: Timestamp,
    /// 时间来自用户明示、系统推断还是 supersession。
    pub source: TemporalSource,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
/// 相对于某个事件边界的位置。
pub enum TemporalRelation {
    /// 在已知状态变化开始之前；只有能唯一关联到版本链时才换算边界。
    BeforeStart,
    /// 在事件结束之后。
    AfterEnd,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq)]
/// 尚不能安全换算成时间戳的时间引用。
pub enum TemporalReference {
    /// 用户明确给出事件关系，但事件结束时间仍未知。
    Event {
        /// 作为时间锚点的事件原文，例如“旅行”。
        event: String,
        /// 相对于事件哪个边界、位于什么方向。
        relation: TemporalRelation,
        /// 关系是用户明示还是系统推断。
        source: TemporalSource,
    },
    /// 无法可靠理解的原始时间表达式；不得猜测为当前时间。
    Unknown {
        /// 未解析的原文。
        expression: String,
        /// 固定为 `TemporalSource::Unknown`。
        source: TemporalSource,
    },
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq)]
/// 时间有效性与留存到期分开建模，避免把“现实失效”混同为“系统淘汰”。
pub struct Temporal {
    /// 系统何时获得这条信息，始终存在。
    pub observed_at: Timestamp,
    /// 信息描述的现实状态何时开始成立；未知时为 `None`。
    pub valid_from: Option<DatedValue>,
    /// 信息描述的现实状态何时结束，按左闭右开区间处理。
    pub valid_to: Option<DatedValue>,
    /// Store 的自动留存截止时间，不改变现实有效期。
    pub expires_at: Option<Timestamp>,
    /// 无法转换为绝对时间戳的事件相对或未知时间引用。
    pub reference: Option<TemporalReference>,
}

impl Temporal {
    pub fn observed_at(observed_at: Timestamp) -> Self {
        Self {
            observed_at,
            valid_from: None,
            valid_to: None,
            expires_at: None,
            reference: None,
        }
    }

    pub fn valid_at(&self, at: Timestamp) -> bool {
        self.reference.is_none()
            && self.valid_from.is_none_or(|from| from.at <= at)
            && self.valid_to.is_none_or(|to| at < to.at)
    }
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, Hash, PartialEq)]
pub enum MemoryValue {
    Text(String),
    Integer(i64),
    Boolean(bool),
    Entity(EntityId),
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
/// Value types permitted by a caller's semantic catalog.
pub enum MemoryValueKind {
    Text,
    Integer,
    Boolean,
    Entity,
}

impl MemoryValue {
    pub fn kind(&self) -> MemoryValueKind {
        match self {
            Self::Text(_) => MemoryValueKind::Text,
            Self::Integer(_) => MemoryValueKind::Integer,
            Self::Boolean(_) => MemoryValueKind::Boolean,
            Self::Entity(_) => MemoryValueKind::Entity,
        }
    }
}

impl From<&str> for MemoryValue {
    fn from(value: &str) -> Self {
        Self::Text(value.to_owned())
    }
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
pub enum Polarity {
    Positive,
    Negative,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
pub enum Strength {
    Weak,
    Moderate,
    Strong,
}

#[derive(
    serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd,
)]
pub enum Confidence {
    Low,
    Medium,
    High,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
/// 保留具体 supersession 原因，历史查询才能区分状态变化与原始错误。
pub enum SupersedeReason {
    StateChanged,
    Correction,
    PreferenceChanged,
    ConstraintChanged,
    Refinement,
    ScopeRefined,
    UserRetracted,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq)]
pub enum CandidateSubject {
    Resolved(EntityId),
    Unresolved(String),
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
pub enum CandidateSource {
    UserStatement,
    UserConfirmed,
    AssistantInference,
    Hypothetical,
    ControlMessage,
    GoalOrPlan,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq)]
/// Extractor 对本次信息应如何更新旧 memory 的判断。
pub enum CandidateIntent {
    /// 普通断言，由 deterministic policy 判断 NEW / REINFORCE / CONFLICT。
    Assert,
    Refine {
        target: Option<MemoryId>,
    },
    Supersede {
        target: Option<MemoryId>,
        reason: SupersedeReason,
    },
    Retract {
        target: Option<MemoryId>,
    },
    Ignore,
    Conflict,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq)]
/// 正式入库前的 staging record；未解析或不可信内容停留在这一层。
pub struct CandidateMemory {
    /// Runtime-owned snapshot of the complete candidate approved by source review.
    /// Legacy and newly constructed candidates have no approval by default.
    #[serde(default, skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub(crate) source_support: Option<serde_json::Value>,
    /// 目标用户记忆库。
    pub owner_id: UserId,
    /// 原始陈述及后续澄清形成的证据链；至少包含一条完整证据。
    pub evidence: Vec<Evidence>,
    /// 被描述对象；`Unresolved` 永远不能进入正式 Store。
    pub subject: CandidateSubject,
    /// Atomic memory 类型。
    pub memory_type: MemoryType,
    /// 由 registry 定义 cardinality 的语义谓词。
    pub predicate: Predicate,
    /// 断言的对象或值。
    pub value: MemoryValue,
    /// Preference / Constraint 的方向；其他类型通常为 `None`。
    pub polarity: Option<Polarity>,
    /// Preference 的强度；没有明确强度时为 `None`。
    pub strength: Option<Strength>,
    /// 明确限制适用范围的条件，不自动复制 conversation context。
    pub scope: Scope,
    /// 建议的初始留存层级，仍会由 retention policy 后续调整。
    pub retention_tier: RetentionTier,
    /// 得知时间、现实有效期及可选留存到期时间。
    pub temporal: Temporal,
    /// 系统是否正确理解原话；Low 默认不入库。
    pub extraction_confidence: Confidence,
    /// 用户自己对陈述有多确定，与 extraction confidence 独立。
    pub certainty: Confidence,
    /// 只有用户陈述或用户确认可以成为正式证据。
    pub source: CandidateSource,
    /// 自然语言中的更新意图，例如纠错、范围缩窄或撤回。
    pub intent: CandidateIntent,
    /// 用户是否明确要求系统记住，影响 admission 与 retention。
    pub explicit_memory_request: bool,
    /// 一次性 episode 为 false 时默认拒绝。
    pub cross_turn_useful: bool,
    /// 用于召回或调试的多值标签，不参与 MemoryType 判定。
    pub tags: Vec<String>,
}

impl CandidateMemory {
    pub fn new(
        owner_id: UserId,
        subject_id: EntityId,
        memory_type: MemoryType,
        predicate: Predicate,
        value: MemoryValue,
        observed_at: Timestamp,
        evidence: Evidence,
    ) -> Self {
        Self {
            source_support: None,
            owner_id,
            evidence: vec![evidence],
            subject: CandidateSubject::Resolved(subject_id),
            memory_type,
            predicate,
            value,
            polarity: None,
            strength: None,
            scope: Scope::default(),
            retention_tier: RetentionTier::LongTerm,
            temporal: Temporal::observed_at(observed_at),
            extraction_confidence: Confidence::High,
            certainty: Confidence::High,
            source: CandidateSource::UserStatement,
            intent: CandidateIntent::Assert,
            explicit_memory_request: false,
            cross_turn_useful: true,
            tags: Vec::new(),
        }
    }

    pub fn subject_id(&self) -> Option<&EntityId> {
        match &self.subject {
            CandidateSubject::Resolved(id) => Some(id),
            CandidateSubject::Unresolved(_) => None,
        }
    }

    fn source_support_snapshot(&self) -> serde_json::Value {
        let mut candidate = self.clone();
        candidate.source_support = None;
        // Older approvals predate model calendar normalization and explicit retention semantics.
        serde_json::json!({"version": 4, "candidate": candidate})
    }

    /// Approval becomes invalid whenever the reviewed candidate or its USER evidence changes.
    pub fn source_support_valid(&self) -> bool {
        self.source_support
            .as_ref()
            .is_some_and(|approved| *approved == self.source_support_snapshot())
    }

    /// Only the runtime's source verifier or checked entity rebinding may issue approval.
    #[cfg(test)]
    pub(crate) fn bind_source_support(&mut self) {
        self.source_support = Some(self.source_support_snapshot());
    }
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq)]
/// 已通过 admission、进入正式 Store 的版本化 memory。
pub struct Memory {
    /// 当前版本在 repository 内的唯一标识。
    pub id: MemoryId,
    /// 记忆库所有者。
    pub owner_id: UserId,
    /// 支撑当前版本的独立证据；用于追溯、去重和 reinforcement 计数。
    pub evidence: Vec<Evidence>,
    /// 该条记忆实际描述的 Entity，可以与 owner 不同。
    pub subject_id: EntityId,
    /// Atomic memory 的唯一类型。
    pub memory_type: MemoryType,
    /// 该断言所属的语义谓词。
    pub predicate: Predicate,
    /// 断言的对象或值。
    pub value: MemoryValue,
    /// Preference / Constraint 的正负方向。
    pub polarity: Option<Polarity>,
    /// Preference 的软强度。
    pub strength: Option<Strength>,
    /// Memory 成立或适用时必须满足的结构化条件。
    pub scope: Scope,
    /// 当前留存层级。
    pub retention_tier: RetentionTier,
    /// 正式 Store 中的生命周期状态。
    pub status: MemoryStatus,
    /// 得知时间、现实有效期及自动到期时间。
    pub temporal: Temporal,
    /// Extractor 对理解原话的置信度。
    pub extraction_confidence: Confidence,
    /// 用户表达本身的确定程度。
    pub certainty: Confidence,
    /// 新记录指向它替换的旧版本。
    pub supersedes: Option<MemoryId>,
    /// 旧记录反向指向当前替代版本；撤回时没有替代版本。
    pub superseded_by: Option<MemoryId>,
    /// 存在旧版本上的终止原因，供历史查询使用。
    pub supersede_reason: Option<SupersedeReason>,
    /// 原始候选是否来自用户的明确记忆请求。
    pub explicit_memory_request: bool,
    /// 新的独立用户证据次数；检索不会增加它。
    pub reinforcement_count: u32,
    /// 系统读取次数；不得用于留存升级。
    pub retrieval_count: u32,
    /// 最近一次收到独立用户强化证据的时间。
    pub last_reinforced_at: Option<Timestamp>,
    /// 最近一次被查询侧实际选中的时间。
    pub last_retrieved_at: Option<Timestamp>,
    /// 用于召回或调试的标签集合。
    pub tags: Vec<String>,
}

impl Memory {
    pub(crate) fn from_candidate(
        id: MemoryId,
        candidate: &CandidateMemory,
        supersedes: Option<MemoryId>,
    ) -> Self {
        Self {
            id,
            owner_id: candidate.owner_id.clone(),
            evidence: candidate.evidence.clone(),
            subject_id: candidate
                .subject_id()
                .expect("admission guarantees a resolved subject")
                .clone(),
            memory_type: candidate.memory_type,
            predicate: candidate.predicate.clone(),
            value: candidate.value.clone(),
            polarity: candidate.polarity,
            strength: candidate.strength,
            scope: candidate.scope.clone(),
            retention_tier: candidate.retention_tier,
            status: MemoryStatus::Active,
            temporal: candidate.temporal.clone(),
            extraction_confidence: candidate.extraction_confidence,
            certainty: candidate.certainty,
            supersedes,
            superseded_by: None,
            supersede_reason: None,
            explicit_memory_request: candidate.explicit_memory_request,
            reinforcement_count: 0,
            retrieval_count: 0,
            last_reinforced_at: None,
            last_retrieved_at: None,
            tags: candidate.tags.clone(),
        }
    }
}
