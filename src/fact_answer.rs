//! Conservative single-fact certificates from a question and a committed USER quote.
//! The caller still checks source linkage, scope, revisions, conflicts and uniqueness.
use crate::{
    Entity, EntityId, EntityType, Memory, MemoryStatus, MemoryType, MemoryValue, Polarity,
    ScopeCondition,
};

#[derive(Clone, Debug)]
pub(crate) struct FactQuestion {
    pub(crate) subject_id: EntityId,
    pub(crate) relation: String,
    weekly: bool,
    pub(crate) activity: Option<String>,
    pub(crate) default_setting: Option<String>,
    usage: Option<UsageQuestion>,
    quantity: Option<QuantityQuestion>,
    monetary: bool,
}

#[derive(Clone, Debug)]
struct QuantityQuestion {
    role: String,
    unit: char,
    object: String,
}

const BORROW_VERBS: [&str; 8] = [
    "可以借阅",
    "能借阅",
    "可借阅",
    "可以借",
    "能借",
    "可借",
    "借阅",
    "借",
];

fn per_person(text: &str) -> Option<(&str, &str)> {
    if let Some(rest) = text.strip_prefix("每人") {
        return Some(("", rest));
    }
    let rest = ["每名", "每位", "每个", "一名", "一位"]
        .into_iter()
        .find_map(|prefix| text.strip_prefix(prefix))?;
    [
        "会员",
        "访客",
        "读者",
        "学生",
        "员工",
        "借阅者",
        "用户",
        "参与者",
        "租户",
        "住户",
        "乘客",
    ]
    .into_iter()
    .find_map(|role| rest.strip_prefix(role).map(|tail| (role, tail)))
}

fn quantity_object(text: &str, outstanding: bool) -> Option<(char, &str)> {
    let unit = text.chars().next()?;
    if !"本册台件把支套辆部个张".contains(unit) {
        return None;
    }
    let object = &text[unit.len_utf8()..];
    let object = if outstanding {
        object.strip_prefix("尚未归还的")?
    } else {
        object
    };
    if object.is_empty()
        || object.chars().count() > 20
        || !one_clause(object)
        || uncertain(object)
        || object.contains([
            '不', '未', '没', '和', '与', '或', '每', '最', '几', '多', '少', '的',
        ])
    {
        return None;
    }
    // 本/册 are interchangeable only for the explicitly named book object.
    let unit = if matches!(unit, '本' | '册') && object == "书" {
        '册'
    } else {
        unit
    };
    Some((unit, object))
}

fn quantity_question(text: &str) -> Option<QuantityQuestion> {
    let (role, rest) = per_person(text)?;
    let (rest, outstanding) = if let Some(rest) = rest.strip_prefix("同时最多") {
        (
            BORROW_VERBS
                .into_iter()
                .find_map(|verb| rest.strip_prefix(verb))?,
            false,
        )
    } else {
        (rest.strip_prefix("手上最多可以有")?, true)
    };
    let (unit, object) = quantity_object(rest.strip_prefix('几')?, outstanding)?;
    Some(QuantityQuestion {
        role: role.into(),
        unit,
        object: object.into(),
    })
}

fn quantity_relation(quantity: &QuantityQuestion) -> String {
    format!(
        "每{}同时最多借{}{}",
        if quantity.role.is_empty() {
            "人"
        } else {
            &quantity.role
        },
        quantity.unit,
        quantity.object
    )
}

fn fee_question(text: &str) -> Option<&'static str> {
    for (action, relation) in [
        ("报名", "报名费"),
        ("注册", "注册费"),
        ("买门票", "门票费"),
        ("门票", "门票费"),
        ("租金", "租金"),
    ] {
        if let Some(rest) = text.strip_prefix(action)
            && [
                "要交多少钱",
                "需要交多少钱",
                "需交多少钱",
                "要付多少钱",
                "需要付多少钱",
                "需要支付多少钱",
            ]
            .contains(&rest)
        {
            return Some(relation);
        }
    }
    None
}

fn money_amount(value: &str) -> bool {
    let Some(amount) = ["美元", "欧元", "英镑", "港元", "元"]
        .into_iter()
        .find_map(|unit| value.strip_suffix(unit))
    else {
        return false;
    };
    !amount.is_empty() && amount.chars().all(|ch| ch.is_ascii_digit())
}

#[derive(Clone, Debug)]
struct UsageQuestion {
    qualifier: String,
    category: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FactAnswer {
    pub(crate) value: String,
    pub(crate) relation: String,
    pub(crate) subject: String,
}

fn names(entity: &Entity) -> impl Iterator<Item = &str> {
    std::iter::once(entity.canonical_name.as_str()).chain(entity.aliases.iter().map(String::as_str))
}

fn terminal(text: &str) -> &str {
    text.trim()
        .strip_suffix(['。', '.', '？', '?', '！', '!'])
        .unwrap_or(text.trim())
        .trim()
}

fn one_clause(text: &str) -> bool {
    !text.is_empty()
        && !text.chars().any(char::is_control)
        && !text.contains([
            '。', '.', '？', '?', '！', '!', '，', ',', '；', ';', '：', ':', '“', '”', '‘', '’',
            '"', '\'', '「', '」', '《', '》', '（', '）', '(', ')',
        ])
}

fn uncertain(text: &str) -> bool {
    [
        "如果",
        "假如",
        "假设",
        "可能",
        "也许",
        "大概",
        "据说",
        "听说",
        "似乎",
        "计划",
        "打算",
        "考虑",
        "准备",
        "不是",
        "并非",
        "没有",
        "不再",
        "尚未",
        "并未",
        "取消",
        "撤回",
        "撤销",
        "曾经",
        "以前",
        "过去",
        "当时",
        "原本",
        "之前",
        "后来",
        "改为",
        "改成",
        "除了",
        "排除",
        "以外",
        "之外",
        "是否",
        "是不是",
        "有无",
        "有没有",
        "为什么",
        "为何",
        "如何",
        "分别",
        "各自",
        "相比",
        "一致",
        "比较",
        "或者",
        "还是",
        "声称",
        "写道",
        "这句话",
        "举例",
        "例如",
        "假话",
        "说法",
        "传言",
        "谣言",
        "并且",
        "而且",
    ]
    .iter()
    .any(|word| text.contains(word))
}

fn relation_valid(relation: &str) -> bool {
    (2..=60).contains(&relation.chars().count())
        && one_clause(relation)
        && !uncertain(relation)
        && !relation.contains(['不', '未', '没', '非', '和', '与', '及', '、', '或'])
        && ![
            "什么", "多少", "哪里", "哪儿", "哪个", "哪种", "哪位", "谁", "怎么",
        ]
        .iter()
        .any(|word| relation.contains(word))
}

fn unique_prefix<'a>(text: &'a str, entities: &'a [Entity]) -> Option<(&'a Entity, &'a str)> {
    let matches: Vec<_> = entities
        .iter()
        .flat_map(|entity| {
            names(entity)
                .filter(|name| !name.is_empty())
                .filter_map(move |name| {
                    text.strip_prefix(name)
                        .map(|rest| (entity, name.len(), rest))
                })
        })
        .collect();
    let longest = matches.iter().map(|(_, length, _)| *length).max()?;
    let mut matches = matches
        .into_iter()
        .filter(|(_, length, _)| *length == longest);
    let (entity, _, rest) = matches.next()?;
    matches
        .all(|(other, _, _)| other.id == entity.id)
        .then_some((entity, rest))
}

fn context_subject<'a>(context: &[String], entities: &'a [Entity]) -> Option<&'a Entity> {
    let [context] = context else { return None };
    if context.contains(['？', '?']) {
        return None;
    }
    let text = terminal(context);
    let text = [
        "我说的是",
        "我指的是",
        "我问的是",
        "说的是",
        "指的是",
        "关于",
    ]
    .iter()
    .find_map(|prefix| text.strip_prefix(prefix))
    .unwrap_or(text);
    let mut matches = entities
        .iter()
        .filter(|entity| names(entity).any(|name| name == text));
    let entity = matches.next()?;
    matches.next().is_none().then_some(entity)
}

fn clarified_remainder<'a>(text: &'a str, entity: &Entity) -> Option<&'a str> {
    for pronoun in ["他们", "她们", "它们", "他", "她", "它"] {
        if let Some(rest) = text.strip_prefix(pronoun) {
            let compatible = match pronoun {
                "他" | "她" | "她们" => entity.entity_type == EntityType::Person,
                "他们" => matches!(
                    entity.entity_type,
                    EntityType::Person | EntityType::Organization
                ),
                _ => entity.entity_type != EntityType::Person,
            };
            return compatible.then_some(rest);
        }
    }
    if text.starts_with(['这', '那']) {
        let mut chars = text.char_indices();
        chars.next()?;
        let (classifier_at, classifier) = chars.next()?;
        if !"个件盏台场次家所位名本辆部支套项座种".contains(classifier) {
            return None;
        }
        let tail = &text[classifier_at + classifier.len_utf8()..];
        // A literal noun suffix binds a named object without guessing its semantic category.
        return names(entity)
            .flat_map(|name| name.char_indices().map(move |(at, _)| &name[at..]))
            .filter(|noun| noun.chars().count() >= 2)
            .filter_map(|noun| tail.strip_prefix(noun).map(|rest| (noun.len(), rest)))
            .max_by_key(|(length, _)| *length)
            .map(|(_, rest)| rest);
    }
    Some(text)
}

// ponytail: finite surface grammar, not arbitrary semantic equivalence. New paraphrase
// families need source-backed relation checks here; a model span suggestion cannot approve them.
fn question_relation(text: &str) -> Option<(String, bool)> {
    let text = text.strip_prefix('的').unwrap_or(text);
    if !one_clause(text) || uncertain(text) {
        return None;
    }
    if let Some(relation) = fee_question(text) {
        return Some((relation.into(), false));
    }
    for prefix in ["最喜欢什么", "最爱什么"] {
        if let Some(category) = text.strip_prefix(prefix) {
            let relation = format!("最喜欢的{category}");
            return relation_valid(category).then_some((relation, false));
        }
    }
    for prefix in ["最喜欢的是哪种", "最爱的是哪种"] {
        if let Some(category) = text.strip_prefix(prefix) {
            let relation = format!("最喜欢的{category}");
            return relation_valid(category).then_some((relation, false));
        }
    }
    // Drinking preference keeps both the superlative and the beverage category.
    if matches!(text, "最爱喝的是哪种饮料" | "最喜欢喝的是哪种饮料") {
        return Some(("最喜欢的饮料".into(), false));
    }
    if matches!(
        text,
        "在哪儿举办"
            | "在哪里举办"
            | "在哪举办"
            | "在哪儿举行"
            | "在哪里举行"
            | "在哪个场地举办"
            | "在哪个场地举行"
            | "安排在哪个场地"
    ) {
        return Some(("举办地点".into(), false));
    }
    for prefix in ["用什么方式", "靠什么"] {
        if let Some(action) = text.strip_prefix(prefix) {
            return relation_valid(action).then(|| (format!("{action}方式"), false));
        }
    }
    let (time_text, weekly) = text
        .strip_prefix("每周")
        .map(|rest| (rest, true))
        .or_else(|| text.strip_prefix("通常在一周中的").map(|rest| (rest, true)))
        .unwrap_or((text, false));
    for prefix in ["什么时候", "什么时间"] {
        if let Some(event) = time_text.strip_prefix(prefix) {
            let event = event
                .strip_prefix("举办")
                .or_else(|| event.strip_prefix("举行"))
                .or_else(|| {
                    event
                        .strip_prefix('开')
                        .filter(|event| event.ends_with('会'))
                })
                .unwrap_or(event);
            return relation_valid(event).then(|| (format!("{event}时间"), weekly));
        }
    }
    if weekly {
        return None;
    }
    for suffix in [
        "是什么时候",
        "是什么时间",
        "是哪一天",
        "是哪一位",
        "是多少",
        "有多少",
        "为多少",
        "是什么",
        "是哪位",
        "是哪个",
        "是谁",
        "多少钱",
        "多少",
    ] {
        if let Some(relation) = text.strip_suffix(suffix) {
            return relation_valid(relation).then(|| (relation.to_owned(), false));
        }
    }
    None
}

// Only an explicit unchanged-setting condition licenses the default relation. It is
// retained on the question and must match a default assertion in the committed quote.
fn unchanged_setting(condition: &str) -> Option<&str> {
    let condition = condition
        .strip_suffix("的时候")
        .or_else(|| condition.strip_suffix('时'))?;
    let setting = ["不改", "不更改", "未修改"]
        .into_iter()
        .find_map(|prefix| condition.strip_prefix(prefix))
        .and_then(|rest| rest.strip_suffix("设置"))
        .or_else(|| condition.strip_prefix("保持")?.strip_suffix("设置不变"))?;
    relation_valid(setting).then_some(setting)
}

fn default_property<'a>(setting: &str, property: &'a str) -> Option<&'a str> {
    if setting == "纸张" && matches!(property, "纸张规格" | "纸张尺寸") {
        return Some("纸张规格");
    }
    (property == setting).then_some(property)
}

fn default_question_relation(setting: &str, text: &str) -> Option<String> {
    // Printing size refers to the paper dimension, not resolution, orientation or font.
    if setting == "纸张"
        && matches!(
            text,
            "会按哪种尺寸打印"
                | "按哪种尺寸打印"
                | "会按什么尺寸打印"
                | "按什么尺寸打印"
                | "会按哪种规格打印"
                | "按哪种规格打印"
        )
    {
        return Some("默认纸张规格".into());
    }
    let (relation, weekly) = question_relation(text)?;
    if weekly {
        return None;
    }
    let property = relation.strip_prefix("默认").unwrap_or(&relation);
    Some(format!("默认{}", default_property(setting, property)?))
}

// Keep the entire explicit activity phrase. No source can supply a missing query condition.
fn activity_parts(text: &str) -> Option<(Option<&str>, &str)> {
    for marker in ["的时候", "期间", "时"] {
        if let Some((activity, rest)) = text.split_once(marker) {
            // A property value (临时工/两小时) and compound time nouns are not activities.
            if activity.contains(['是', '为'])
                || marker == "时"
                    && (activity.ends_with('同')
                        || rest.starts_with(['候', '间', '长', '段', '期', '是', '为']))
            {
                continue;
            }
            return activity_valid(activity).then_some((Some(activity), rest));
        }
    }
    Some((None, text))
}

fn activity_valid(activity: &str) -> bool {
    relation_valid(activity)
        && !["仅", "只", "而", "但", "拿着", "未用"]
            .iter()
            .any(|part| activity.contains(part))
}

fn usage_prefix(text: &str) -> Option<(&str, &str)> {
    let (qualifier, text) = ["固定", "通常", "经常", "总是"]
        .into_iter()
        .find_map(|word| text.strip_prefix(word).map(|rest| (word, rest)))
        .unwrap_or(("", text));
    let rest = text
        .strip_prefix("使用")
        .or_else(|| text.strip_prefix('用'))?;
    Some((qualifier, rest))
}

fn usage_question(text: &str) -> Option<UsageQuestion> {
    let (qualifier, rest) = usage_prefix(text)?;
    if rest
        .strip_prefix("什么方式")
        .is_some_and(|tail| !tail.is_empty())
    {
        return None;
    }
    let category = ["的是什么", "的是哪种", "的是哪个", "哪种", "哪个", "什么"]
        .into_iter()
        .find_map(|marker| rest.strip_prefix(marker))?;
    if !category.is_empty()
        && (category.chars().count() > 20
            || !one_clause(category)
            || uncertain(category)
            || category.contains(['不', '未', '没', '非', '和', '与', '及', '或', '的', '时']))
    {
        return None;
    }
    Some(UsageQuestion {
        qualifier: qualifier.into(),
        category: category.into(),
    })
}

// A bounded activity-tool question: "校准拿的是哪种探头". Holding/borrowing alone
// does not imply use; no other form of 拿 is normalized to 使用.
fn activity_tool_question(text: &str) -> Option<(&str, UsageQuestion)> {
    let (activity, rest) = text.split_once("拿的是")?;
    let activity = activity.strip_suffix('时').unwrap_or(activity);
    if !activity_valid(activity) {
        return None;
    }
    let usage = usage_question(&format!("用{rest}"))?;
    Some((activity, usage))
}

fn activity_alias(left: &str, right: &str) -> bool {
    left == right
        || ["进行", "画", "写", "练"].iter().any(|verb| {
            right.chars().count() >= 2 && left.strip_prefix(verb) == Some(right)
                || left.chars().count() >= 2 && right.strip_prefix(verb) == Some(left)
        })
}

pub(crate) fn grounded_query_conditions(
    question: &FactQuestion,
    memories: &[Memory],
    entities: &[Entity],
) -> Vec<ScopeCondition> {
    let Some(activity) = &question.activity else {
        return Vec::new();
    };
    let mut conditions = vec![ScopeCondition::Activity(activity.clone())];
    // Only a bounded omitted-action spelling is bridged, after the full USER activity
    // and relation/value match. All location/time/task conditions stay in the hard filter.
    for memory in memories {
        if !memory
            .evidence
            .iter()
            .any(|e| verify(question, memory, &e.quote, entities).is_some())
        {
            continue;
        }
        for condition in &memory.scope.conditions {
            if let ScopeCondition::Activity(stored) = condition
                && activity_alias(activity, stored)
            {
                conditions.push(condition.clone());
            }
        }
    }
    conditions.sort();
    conditions.dedup();
    conditions
}

fn without_current_marker(text: &str) -> &str {
    ["当前", "现在", "目前"]
        .into_iter()
        .find_map(|prefix| text.strip_prefix(prefix))
        .unwrap_or(text)
}

// A default is not the current setting after an override. Match the same subject
// and setting axis; a different setting (e.g. paper orientation) cannot block size.
pub(crate) fn setting_statement(
    question: &FactQuestion,
    body: &str,
    entities: &[Entity],
    include_default: bool,
) -> bool {
    let Some(setting) = question.default_setting.as_deref() else {
        return false;
    };
    body.split(['。', '！', '!', '？', '?', '，', ',', '；', ';', '\n'])
        .any(|clause| {
            let clause = clause.trim();
            let clause = ["更正：", "更正:", "纠正：", "纠正:", "现在", "当前", "目前"]
                .into_iter()
                .find_map(|prefix| clause.strip_prefix(prefix))
                .unwrap_or(clause);
            let Some((subject, rest)) = unique_prefix(clause, entities) else {
                return false;
            };
            if subject.id != question.subject_id {
                return false;
            }
            let rest = rest.strip_prefix('的').unwrap_or(rest);
            let rest = without_current_marker(rest);
            let rest = if let Some(rest) = rest.strip_prefix("默认") {
                if !include_default {
                    return false;
                }
                rest
            } else {
                rest
            };
            let Some(rest) = rest.strip_prefix(setting) else {
                return false;
            };
            let rest = if setting == "纸张" {
                ["规格", "尺寸", "设置"]
                    .into_iter()
                    .find_map(|property| rest.strip_prefix(property))
                    .unwrap_or(rest)
            } else {
                rest.strip_prefix("设置").unwrap_or(rest)
            };
            let rest = without_current_marker(rest);
            [
                "是", "为", "改", "调整", "换", "设", "已", "不", "未", "没", "未知", "待", "可能",
            ]
            .into_iter()
            .any(|marker| rest.starts_with(marker))
        })
}

// Lifecycle and scope belong to the actual assertion, not every sentence in its
// supporting USER quote. A full fact value gives the unambiguous sentence binding.
pub(crate) fn setting_memory_statement(
    question: &FactQuestion,
    memory: &Memory,
    quote: &str,
    entities: &[Entity],
) -> bool {
    if let MemoryValue::Text(statement) = &memory.value
        && complete_assertion(quote, statement)
        && setting_statement(question, statement, entities, false)
    {
        return true;
    }
    if memory.predicate.0 == "fact" || !one_clause(terminal(quote)) {
        return false;
    }
    // A legacy scalar requires a same-axis predicate and a single bound statement;
    // a shared object string in a different sentence is not enough.
    let Some(subject) = entities
        .iter()
        .find(|entity| entity.id == memory.subject_id)
    else {
        return false;
    };
    setting_statement(
        question,
        &format!("{}的{}是", subject.canonical_name, memory.predicate.0),
        entities,
        false,
    ) && setting_statement(question, quote, entities, false)
}

// A newer same-subject use/change statement must settle before an old answer is released.
// Missing activity on a new change stays conservative; explicitly different activities do not.
pub(crate) fn pending_relation_matches(
    question: &FactQuestion,
    body: &str,
    entities: &[Entity],
) -> bool {
    if question.default_setting.is_some() {
        return setting_statement(question, body, entities, true);
    }
    if question.quantity.is_some() || question.monetary {
        // A pending same-subject revision must settle first. This only blocks an old
        // answer; matching words here never grant a new answer or a source citation.
        return crate::lexical::entity_hints(body, entities).contains_key(&question.subject_id)
            && if question.quantity.is_some() {
                body.contains('借') || body.contains("归还")
            } else {
                body.contains(&question.relation)
                    || question
                        .relation
                        .strip_suffix('费')
                        .is_some_and(|action| body.contains(action))
            };
    }
    if question.usage.is_none() && question.activity.is_none() {
        return false;
    }
    body.split(['。', '！', '!', '？', '?', '，', ',', '；', ';', '\n']).any(|clause| {
        if !crate::lexical::entity_hints(clause, entities).contains_key(&question.subject_id) {
            return false;
        }
        let clause = clause.trim();
        let clause = ["更正：", "更正:", "纠正：", "纠正:", "我更正：", "我更正:"]
            .iter().find_map(|prefix| clause.strip_prefix(prefix)).unwrap_or(clause);
        let clause = ["现在", "目前", "当前", "后来"]
            .iter().find_map(|prefix| clause.strip_prefix(prefix)).unwrap_or(clause);
        let Some((subject, rest)) = unique_prefix(clause, entities) else { return true; };
        if subject.id != question.subject_id { return true; }
        let rest = rest.strip_prefix('的').unwrap_or(rest);
        let rest = ["现在", "当前", "目前"]
            .iter().find_map(|prefix| rest.strip_prefix(prefix)).unwrap_or(rest);
        let activity = activity_parts(rest).and_then(|(activity, _)| activity);
        // No new answer is inferred here. An unfinished same-subject source pauses the
        // old answer unless it explicitly names a different complete activity.
        !matches!((question.activity.as_deref(), activity), (Some(asked), Some(actual)) if !activity_alias(asked, actual))
    })
}

pub(crate) fn parse_question(
    query: &str,
    context: &[String],
    entities: &[Entity],
) -> Option<FactQuestion> {
    let query = terminal(query);
    let query = query.strip_prefix("请问").unwrap_or(query);
    if query.chars().count() > 500 {
        return None;
    }
    let (condition, query) = query
        .split_once(['，', ','])
        .map(|(condition, query)| (Some(condition.trim()), query.trim()))
        .unwrap_or((None, query));
    let front_subject = condition.and_then(|condition| {
        let rest = condition
            .strip_prefix("参加")
            .or_else(|| condition.strip_prefix('在'))?;
        unique_prefix(rest, entities)
            .filter(|(_, remainder)| remainder.is_empty())
            .map(|(subject, _)| subject)
    });
    let default_setting = condition.and_then(unchanged_setting);
    let leading_activity = if let Some(condition) = condition {
        if !one_clause(condition)
            || front_subject.is_none()
                && entities.iter().any(|entity| {
                    names(entity).any(|name| !name.is_empty() && condition.contains(name))
                })
        {
            return None;
        }
        if default_setting.is_some() || front_subject.is_some() {
            None
        } else {
            let (activity, remainder) = activity_parts(condition)?;
            if !remainder.is_empty() {
                return None;
            }
            Some(activity?)
        }
    } else {
        None
    };
    if !one_clause(query) {
        return None;
    }
    let (subject, rest) = if let Some(subject) = front_subject {
        if !context.is_empty()
            && context_subject(context, entities).is_none_or(|other| other.id != subject.id)
        {
            return None;
        }
        (subject, query)
    } else if let Some((subject, rest)) = unique_prefix(query, entities) {
        if !context.is_empty()
            && context_subject(context, entities).is_none_or(|other| other.id != subject.id)
        {
            return None;
        }
        (subject, rest)
    } else {
        let subject = context_subject(context, entities)?;
        (subject, clarified_remainder(query, subject)?)
    };
    if entities
        .iter()
        .any(|entity| names(entity).any(|name| !name.is_empty() && rest.contains(name)))
    {
        return None;
    }
    let rest = rest.strip_prefix('的').unwrap_or(rest);
    let quantity = quantity_question(rest);
    if quantity.is_none() && uncertain(query) {
        return None;
    }
    let monetary = rest.ends_with("多少钱");
    // A fronted named place/participant is a subject binding only for these bounded
    // relations. Other fronted clauses keep their existing activity/scope checks.
    if front_subject.is_some() && quantity.is_none() && fee_question(rest).is_none() {
        return None;
    }
    if condition.is_some_and(|condition| condition.starts_with("参加")) && quantity.is_some() {
        return None;
    }
    let (activity, rest) = activity_parts(rest)?;
    // Reject two scope clauses rather than silently dropping either one.
    if condition.is_some() && activity.is_some()
        || quantity.is_some()
            && (activity.is_some() || leading_activity.is_some() || default_setting.is_some())
    {
        return None;
    }
    let mut activity = activity.or(leading_activity).map(str::to_owned);
    let usage = usage_question(rest).or_else(|| {
        if activity.is_some() {
            usage_question(&format!("用{}", rest.strip_prefix("拿的是")?))
        } else {
            let (phrase, usage) = activity_tool_question(rest)?;
            activity = Some(phrase.into());
            Some(usage)
        }
    });
    let (relation, weekly) = if let Some(quantity) = &quantity {
        (quantity_relation(quantity), false)
    } else if let Some(setting) = default_setting {
        if activity.is_some() || usage.is_some() {
            return None;
        }
        (default_question_relation(setting, rest)?, false)
    } else if let Some(usage) = &usage {
        (format!("{}使用", usage.qualifier), false)
    } else {
        question_relation(rest)?
    };
    Some(FactQuestion {
        subject_id: subject.id.clone(),
        relation,
        weekly,
        activity,
        default_setting: default_setting.map(str::to_owned),
        usage,
        quantity,
        monetary,
    })
}

pub(crate) fn verify(
    question: &FactQuestion,
    memory: &Memory,
    quote: &str,
    entities: &[Entity],
) -> Option<FactAnswer> {
    if quote.contains(['？', '?'])
        || memory.subject_id != question.subject_id
        || memory.status != MemoryStatus::Active
        || memory.memory_type == MemoryType::Constraint
        || memory.polarity == Some(Polarity::Negative)
        || !memory
            .evidence
            .iter()
            .any(|evidence| evidence.quote == quote)
    {
        return None;
    }
    let clause = terminal(quote);
    if (!one_clause(clause) && question.quantity.is_none())
        || uncertain(clause)
        || clause.ends_with(['吗', '呢', '么', '吧'])
        || clause.chars().count() > 1000
    {
        return None;
    }
    let (subject, rest) = unique_prefix(clause, entities)?;
    if subject.id != question.subject_id || subject.owner_id != memory.owner_id {
        return None;
    }
    let rest = rest.strip_prefix('的').unwrap_or(rest);
    if let Some(quantity) = &question.quantity {
        let assertion = rest
            .strip_prefix("规定，")
            .or_else(|| rest.strip_prefix("规定,"))
            .unwrap_or(rest);
        if !one_clause(assertion)
            || !assertion.starts_with('每')
            || memory.memory_type != MemoryType::Fact
        {
            return None;
        }
        let (role, tail) = per_person(assertion)?;
        if !quantity.role.is_empty() && !role.is_empty() && quantity.role != role {
            return None;
        }
        let tail = tail.strip_prefix("同时最多")?;
        let tail = BORROW_VERBS
            .into_iter()
            .find_map(|verb| tail.strip_prefix(verb))?;
        let digits = tail.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        let (unit, object) = quantity_object(&tail[digits..], false)?;
        if unit != quantity.unit || object != quantity.object {
            return None;
        }
        let value = &tail[..digits + tail[digits..].chars().next()?.len_utf8()];
        return certify_value(
            question,
            memory,
            clause,
            rest,
            &question.relation,
            value,
            subject,
            entities,
        );
    }
    let (activity, rest) = activity_parts(rest)?;
    if activity != question.activity.as_deref() {
        return None;
    }
    let rest = rest.strip_prefix('的').unwrap_or(rest);
    if let Some(usage) = &question.usage {
        if memory.memory_type != MemoryType::Fact
            || !["uses", "使用", "用"].contains(&memory.predicate.0.as_str())
        {
            return None;
        }
        let (qualifier, value) = usage_prefix(rest)?;
        let value = value.strip_prefix("的是").unwrap_or(value);
        // A whole-sentence fallback does not identify a verb's object: the remainder
        // might state a prerequisite, assessment or uncertainty about using it.
        let object_agrees = match &memory.value {
            MemoryValue::Text(stored) => terminal(stored) == value,
            MemoryValue::Entity(id) => entities
                .iter()
                .any(|entity| entity.id == *id && names(entity).any(|name| name == value)),
            _ => false,
        };
        if !object_agrees
            || (!usage.qualifier.is_empty() && qualifier != usage.qualifier)
            || (!usage.category.is_empty() && !value.ends_with(&usage.category))
        {
            return None;
        }
        return certify_value(
            question,
            memory,
            clause,
            rest,
            &question.relation,
            value,
            subject,
            entities,
        );
    }
    let mut answers = rest.char_indices().filter_map(|(at, mark)| {
        if !matches!(mark, '是' | '为') {
            return None;
        }
        let relation = rest[..at].trim();
        let value = rest[at + mark.len_utf8()..].trim();
        certify_value(
            question, memory, clause, rest, relation, value, subject, entities,
        )
    });
    let answer = answers.next()?;
    answers.next().is_none().then_some(answer)
}

#[allow(clippy::too_many_arguments)]
fn certify_value(
    question: &FactQuestion,
    memory: &Memory,
    clause: &str,
    rest: &str,
    relation: &str,
    value: &str,
    subject: &Entity,
    entities: &[Entity],
) -> Option<FactAnswer> {
    let relation_matches = if let Some(setting) = &question.default_setting {
        relation
            .strip_prefix("默认")
            .and_then(|property| default_property(setting, property))
            .is_some_and(|property| question.relation == format!("默认{property}"))
    } else {
        relation == question.relation
    };
    if !relation_matches
        || !relation_valid(relation)
        || value.is_empty()
        || value.chars().count() > 100
        || !one_clause(value)
        || question.monetary && !money_amount(value)
        || uncertain(value)
        || value.starts_with(['不', '未', '没', '非'])
        || value.contains(['和', '与', '及', '、', '或'])
        || [
            "什么",
            "多少",
            "哪里",
            "哪儿",
            "哪个",
            "哪种",
            "哪位",
            "谁",
            "未知",
            "不详",
            "待定",
            "未定",
            "不确定",
        ]
        .iter()
        .any(|word| value.contains(word))
        || question.weekly && !value.starts_with("每周")
    {
        return None;
    }
    let value_agrees = match &memory.value {
        MemoryValue::Text(stored) => {
            !stored.contains(['？', '?']) && [value, clause, rest].contains(&terminal(stored))
        }
        MemoryValue::Integer(stored) => value == stored.to_string(),
        MemoryValue::Entity(id) => entities
            .iter()
            .any(|entity| entity.id == *id && names(entity).any(|name| name == value)),
        MemoryValue::Boolean(_) => false,
    };
    value_agrees.then(|| FactAnswer {
        value: value.to_owned(),
        relation: relation.to_owned(),
        subject: subject.canonical_name.clone(),
    })
}

pub(crate) fn complete_assertion(source: &str, quote: &str) -> bool {
    let quote = quote.trim().trim_end_matches(['。', '.', '！', '!']).trim();
    source
        .split_inclusive(['。', '.', '！', '!', '？', '?', '\n', '\r'])
        .any(|sentence| {
            sentence
                .trim()
                .trim_end_matches(['。', '.', '！', '!'])
                .trim()
                == quote
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CandidateMemory, Evidence, EvidenceId, MemoryId, Predicate, SessionId, Timestamp, TurnId,
        UserId,
    };

    fn entity(name: &str, entity_type: EntityType) -> Entity {
        Entity {
            id: EntityId::from(name),
            owner_id: UserId::from("test"),
            entity_type,
            canonical_name: name.into(),
            aliases: Vec::new(),
        }
    }

    fn fact(subject: &Entity, quote: &str, value: &str) -> Memory {
        Memory::from_candidate(
            MemoryId(1),
            &CandidateMemory::new(
                subject.owner_id.clone(),
                subject.id.clone(),
                MemoryType::Fact,
                Predicate::from("fact"),
                MemoryValue::Text(value.into()),
                Timestamp(1),
                Evidence::new(
                    EvidenceId::from("e"),
                    SessionId::from("s"),
                    TurnId::from("t"),
                    quote,
                ),
            ),
            None,
        )
    }

    fn usage_fact(subject: &Entity, quote: &str, value: &str) -> Memory {
        let mut memory = fact(subject, quote, value);
        memory.predicate = Predicate::from("uses");
        memory
    }

    #[test]
    fn live_fee_and_quantity_gaps_require_complete_certificates() {
        let entities = vec![
            entity("海盐折纸工作坊", EntityType::Other("event".into())),
            entity("白鹭读书会", EntityType::Organization),
            entity("赤枫读书会", EntityType::Organization),
        ];
        let quotes = [
            "海盐折纸工作坊的报名费为36元。",
            "白鹭读书会规定，每名会员同时最多可借3册书。",
            "赤枫读书会规定，每名会员同时最多可借5册书。",
        ];
        let mut missing = Vec::new();
        for (id, query, context, subject, value) in [
            (
                "event_q5",
                "参加海盐折纸工作坊，报名要交多少钱？",
                vec![],
                0,
                "36元",
            ),
            (
                "organization_q3",
                "每人同时最多能借几本书？",
                vec!["我问的是白鹭读书会。".into()],
                1,
                "3册",
            ),
            (
                "organization_q4",
                "赤枫读书会每名会员同时最多能借几册书？",
                vec![],
                2,
                "5册",
            ),
            (
                "organization_v1",
                "赤枫读书会每名会员同时最多能借几册书？",
                vec![],
                2,
                "5册",
            ),
            (
                "organization_v2",
                "赤枫读书会每名会员同时最多能借几册书？",
                vec![],
                2,
                "5册",
            ),
            (
                "organization_q5",
                "在白鹭读书会，一名会员手上最多可以有几本尚未归还的书？",
                vec![],
                1,
                "3册",
            ),
        ] {
            let memory = fact(&entities[subject], quotes[subject], quotes[subject]);
            let answer = parse_question(query, &context, &entities)
                .and_then(|question| verify(&question, &memory, quotes[subject], &entities));
            if answer.as_ref().map(|answer| answer.value.as_str()) != Some(value) {
                missing.push(id);
            }
        }
        assert!(
            missing.is_empty(),
            "missing shared certificates: {missing:?}"
        );
    }

    #[test]
    fn fee_and_quantity_grammar_preserves_subject_units_and_limits() {
        let entities = vec![
            entity("银湾课程", EntityType::Other("event".into())),
            entity("杉溪书社", EntityType::Organization),
            entity("柏岸书社", EntityType::Organization),
        ];
        for (subject, query, quote, answer) in [
            (
                0,
                "参加银湾课程，报名需要付多少钱？",
                "银湾课程的报名费是72元。",
                "72元",
            ),
            (
                0,
                "银湾课程报名要交多少钱？",
                "银湾课程的报名费是72元。",
                "72元",
            ),
            (
                1,
                "在杉溪书社，每人同时最多可以借阅几本书？",
                "杉溪书社规定，每人同时最多可借阅12本书。",
                "12本",
            ),
        ] {
            let question = parse_question(query, &[], &entities).expect(query);
            assert_eq!(
                verify(
                    &question,
                    &fact(&entities[subject], quote, quote),
                    quote,
                    &entities
                )
                .unwrap()
                .value,
                answer
            );
        }
        for query in [
            "参加银湾课程以后，报名要交多少钱？",
            "如果参加银湾课程，报名要交多少钱？",
            "在杉溪书社和柏岸书社，每人同时最多能借几本书？",
            "在杉溪书社，每人每周最多能借几本书？",
            "杉溪书社每人同时最少能借几本书？",
            "杉溪书社每人同时能借几本书？",
            "杉溪书社同时最多能借几本书？",
            "杉溪书社每人最多能借几本书？",
            "杉溪书社所有会员同时最多能借几本书？",
            "杉溪书社每人同时最多能借几天书？",
            "在杉溪书社，一名会员手上最多可以有几本已经归还的书？",
        ] {
            assert!(parse_question(query, &[], &entities).is_none(), "{query}");
        }
        let question = parse_question("杉溪书社每人同时最多能借几本书？", &[], &entities).unwrap();
        for quote in [
            "柏岸书社规定，每名会员同时最多可借12册书。",
            "杉溪书社规定，每名会员每周最多可借12册书。",
            "杉溪书社规定，每名会员同时最少可借12册书。",
            "杉溪书社规定，每名会员同时可借12册书。",
            "杉溪书社规定，所有会员同时最多可借12册书。",
            "杉溪书社规定，同时最多可借12册书。",
            "杉溪书社规定，每名会员最多可借12册书。",
            "杉溪书社规定，每名会员同时最多可借12天。",
            "杉溪书社规定，每名会员同时最多可借12套书。",
            "杉溪书社规定，每名会员同时最多可借12册杂志。",
            "杉溪书社规定，每名会员同时最多可借12册书，但尚待确认。",
            "杉溪书社规定，每名会员同时最多可借12册书是错误的。",
            "杉溪书社规定，每名会员同时最多可借-12册书。",
            "杉溪书社规定，每名会员同时最多可借12.5册书。",
        ] {
            assert!(
                verify(
                    &question,
                    &fact(&entities[1], quote, quote),
                    quote,
                    &entities
                )
                .is_none(),
                "{quote}"
            );
        }
        let quote = "杉溪书社规定，每名会员同时最多可借12册书。";
        let other_unit =
            parse_question("杉溪书社每人同时最多能借几套书？", &[], &entities).unwrap();
        assert!(
            verify(
                &other_unit,
                &fact(&entities[1], quote, quote),
                quote,
                &entities
            )
            .is_none()
        );
        let question = parse_question("参加银湾课程，报名要交多少钱？", &[], &entities).unwrap();
        for quote in [
            "银湾课程的押金是72元。",
            "银湾课程的报名总费用是72元。",
            "银湾课程的报名费是72小时。",
            "银湾课程的报名费是免费。",
            "银湾课程的报名费可能是72元。",
        ] {
            assert!(
                verify(
                    &question,
                    &fact(&entities[0], quote, quote),
                    quote,
                    &entities
                )
                .is_none(),
                "{quote}"
            );
        }
    }

    #[test]
    fn quantity_roles_objects_and_pending_revisions_are_not_erased() {
        let entities = vec![
            entity("北湾器材馆", EntityType::Organization),
            entity("南岸器材馆", EntityType::Organization),
        ];
        let quote = "北湾器材馆规定，每名访客同时最多可借2台相机。";
        let memory = fact(&entities[0], quote, quote);
        let question = parse_question(
            "在北湾器材馆，每名访客同时最多可以借几台相机？",
            &[],
            &entities,
        )
        .unwrap();
        assert_eq!(
            verify(&question, &memory, quote, &entities).unwrap().value,
            "2台"
        );
        for query in [
            "北湾器材馆每名会员同时最多可以借几台相机？",
            "北湾器材馆每名学生同时最多可以借几台相机？",
            "北湾器材馆每名员工同时最多可以借几台相机？",
            "北湾器材馆每名访客同时最多可以借几件相机？",
            "北湾器材馆每名访客同时最多可以借几台电脑？",
            "南岸器材馆每名访客同时最多可以借几台相机？",
        ] {
            let question = parse_question(query, &[], &entities).unwrap();
            assert!(
                verify(&question, &memory, quote, &entities).is_none(),
                "{query}"
            );
        }
        for changed in [
            "北湾器材馆规定，每名访客同时最多可借7台相机。",
            "更正：北湾器材馆的借阅规则调整为最多7台。",
            "北湾器材馆暂时不许借相机。",
        ] {
            assert!(
                pending_relation_matches(&question, changed, &entities),
                "{changed}"
            );
        }
        assert!(!pending_relation_matches(
            &question,
            "南岸器材馆规定，每名访客同时最多可借7台相机。",
            &entities
        ));
        let question = parse_question("北湾器材馆租金需要付多少钱？", &[], &entities).unwrap();
        let quote = "北湾器材馆的租金是90元。";
        assert_eq!(
            verify(
                &question,
                &fact(&entities[0], quote, quote),
                quote,
                &entities
            )
            .unwrap()
            .value,
            "90元"
        );
        assert!(pending_relation_matches(
            &question,
            "北湾器材馆租金改为120元。",
            &entities
        ));
    }

    #[test]
    fn direct_and_clarified_facts_keep_subject_relation_and_value() {
        let cases = [
            (
                "顾衡",
                EntityType::Person,
                "顾衡最喜欢的饮料是无糖豆浆。",
                "无糖豆浆",
                "他最喜欢什么饮料？",
                "顾衡最爱喝的是哪种饮料？",
            ),
            (
                "琥珀台灯",
                EntityType::Device,
                "琥珀台灯的供电方式是太阳能板。",
                "太阳能板",
                "那盏台灯靠什么供电？",
                "琥珀台灯用什么方式供电？",
            ),
            (
                "晨星讲座",
                EntityType::Other("event".into()),
                "晨星讲座的举办地点是柏木厅。",
                "柏木厅",
                "那场讲座在哪儿举办？",
                "晨星讲座安排在哪个场地？",
            ),
            (
                "榆舟社团",
                EntityType::Organization,
                "榆舟社团的例会时间是每周三上午九点。",
                "每周三上午九点",
                "他们每周什么时候开例会？",
                "榆舟社团通常在一周中的什么时间开例会？",
            ),
            (
                "杉木仪器",
                EntityType::Device,
                "杉木仪器的校准编号是K72。",
                "K72",
                "它的校准编号是什么？",
                "杉木仪器的校准编号是什么？",
            ),
        ];
        for (name, kind, quote, value, clarified, direct) in cases {
            let entities = vec![entity(name, kind)];
            let memory = fact(&entities[0], quote, value);
            for (query, context) in [(clarified, vec![format!("{name}。")]), (direct, vec![])] {
                let question = parse_question(query, &context, &entities).expect(query);
                assert_eq!(
                    verify(&question, &memory, quote, &entities).unwrap().value,
                    value
                );
            }
            assert!(parse_question(clarified, &[], &entities).is_none());
        }
    }

    #[test]
    fn mismatched_uncertain_and_quoted_assertions_do_not_certify() {
        let entities = vec![
            entity("杉木仪器", EntityType::Device),
            entity("桦木仪器", EntityType::Device),
        ];
        let question = parse_question("杉木仪器的校准编号是什么？", &[], &entities).unwrap();
        for quote in [
            "杉木仪器的校准编号不是K72。",
            "杉木仪器的校准编号可能是K72。",
            "如果杉木仪器的校准编号是K72。",
            "“杉木仪器的校准编号是K72”。",
            "说明书写道杉木仪器的校准编号是K72。",
            "杉木仪器的校准编号是K72这句话是假的。",
            "杉木仪器的出厂编号是K72。",
            "桦木仪器的校准编号是K72。",
            "杉木仪器的校准编号是K72和K73。",
            "杉木仪器的校准编号为非K72。",
            "杉木仪器的校准编号是谁。",
            "杉木仪器的校准编号是K72？",
            "杉木仪器的校准编号是K72?",
            "杉木仪器的校准编号是K72吗。",
        ] {
            for stored in ["K72", quote] {
                let memory = fact(&entities[0], quote, stored);
                assert!(
                    verify(&question, &memory, quote, &entities).is_none(),
                    "{quote} / {stored}"
                );
            }
        }
        let quote = "杉木仪器的校准编号是K72。";
        for source in [
            "如果杉木仪器的校准编号是K72。",
            "杉木仪器的校准编号是K72，其实这只是一个假设。",
            "“杉木仪器的校准编号是K72。”",
            "杉木仪器的校准编号是K72？",
        ] {
            assert!(!complete_assertion(source, "杉木仪器的校准编号是K72"));
        }
        assert!(complete_assertion("记录。杉木仪器的校准编号是K72。", quote));
        for value in ["K73", "包含K72的描述", "校准编号可能是K72", "K72？"] {
            assert!(
                verify(
                    &question,
                    &fact(&entities[0], quote, value),
                    quote,
                    &entities
                )
                .is_none()
            );
        }
        assert!(
            verify(
                &question,
                &fact(&entities[1], quote, "K72"),
                quote,
                &entities
            )
            .is_none()
        );
        for value in [quote, "校准编号是K72", "K72"] {
            assert!(
                verify(
                    &question,
                    &fact(&entities[0], quote, value),
                    quote,
                    &entities
                )
                .is_some()
            );
        }
        for query in [
            "杉木仪器和桦木仪器的校准编号是什么？",
            "杉木仪器的校准编号和出厂编号是什么？",
            "杉木仪器的校准编号不是哪个？",
            "杉木仪器的校准编号是什么？桦木仪器呢？",
        ] {
            assert!(parse_question(query, &[], &entities).is_none(), "{query}");
        }
        for context in [
            "杉木仪器或桦木仪器。",
            "应该是杉木仪器。",
            "不是杉木仪器。",
            "杉木仪器？",
            "桦木仪器。",
        ] {
            assert!(
                parse_question("杉木仪器的校准编号是什么？", &[context.into()], &entities)
                    .is_none()
            );
        }
    }

    #[test]
    fn time_characters_in_existing_property_values_are_not_activity_markers() {
        let entities = vec![entity("琥珀队", EntityType::Organization)];
        for (query, quote, value) in [
            (
                "琥珀队的默认岗位是什么？",
                "琥珀队的默认岗位是临时工。",
                "临时工",
            ),
            (
                "琥珀队的轮班长度是多少？",
                "琥珀队的轮班长度是两小时。",
                "两小时",
            ),
            (
                "琥珀队的执行时长是多少？",
                "琥珀队的执行时长是三小时。",
                "三小时",
            ),
            (
                "琥珀队的工作时间是什么？",
                "琥珀队的工作时间是每周三上午。",
                "每周三上午",
            ),
        ] {
            let question = parse_question(query, &[], &entities).expect(query);
            assert!(question.activity.is_none());
            assert_eq!(
                verify(
                    &question,
                    &fact(&entities[0], quote, value),
                    quote,
                    &entities
                )
                .unwrap()
                .value,
                value
            );
        }
    }

    #[test]
    fn pending_activity_aliases_never_release_an_old_answer() {
        let entities = vec![entity("琥珀队", EntityType::Organization)];
        for (asked, pending) in [
            ("画速写", "速写"),
            ("写书法", "书法"),
            ("进行校准", "校准"),
            ("速写", "画速写"),
            ("书法", "写书法"),
            ("校准", "进行校准"),
        ] {
            let question =
                parse_question(&format!("琥珀队{asked}时使用什么？"), &[], &entities).unwrap();
            assert!(
                pending_relation_matches(
                    &question,
                    &format!("琥珀队{pending}时使用新工具。"),
                    &entities
                ),
                "{asked} / {pending}"
            );
            assert!(!pending_relation_matches(
                &question,
                "琥珀队游泳时使用新工具。",
                &entities
            ));
        }
    }

    #[test]
    fn activity_usage_keeps_conditions_and_qualifiers() {
        let entities = vec![entity("琥珀队", EntityType::Organization)];
        let quote = "琥珀队巡检时固定使用红外探头。";
        let mut memory = usage_fact(&entities[0], quote, "红外探头");
        memory.scope.conditions = vec![ScopeCondition::Activity("巡检".into())];
        for query in [
            "琥珀队巡检时固定用哪种探头？",
            "琥珀队巡检时使用什么？",
            "琥珀队巡检时用的是什么？",
            "琥珀队巡检拿的是哪种探头？",
        ] {
            let question = parse_question(query, &[], &entities).expect(query);
            assert_eq!(
                verify(&question, &memory, quote, &entities).unwrap().value,
                "红外探头"
            );
            assert_eq!(
                grounded_query_conditions(&question, &[memory.clone()], &entities),
                vec![ScopeCondition::Activity("巡检".into())]
            );
            assert!(pending_relation_matches(
                &question,
                "更正：琥珀队巡检时改用蓝光探头。",
                &entities
            ));
            assert!(!pending_relation_matches(
                &question,
                "琥珀队校准时改用蓝光探头。",
                &entities
            ));
            for quote in [
                "现在琥珀队巡检时使用蓝光探头。",
                "琥珀队巡检时已经改用蓝光探头。",
                "琥珀队巡检时不用红外探头了。",
            ] {
                assert!(
                    pending_relation_matches(&question, quote, &entities),
                    "{quote}"
                );
            }
        }
        for query in [
            "琥珀队使用什么？",
            "琥珀队校准时使用什么？",
            "琥珀队巡检时使用哪种笔？",
            "琥珀队不巡检时使用什么？",
            "琥珀队巡检时只是拿着哪种探头？",
        ] {
            assert!(
                parse_question(query, &[], &entities)
                    .is_none_or(|question| verify(&question, &memory, quote, &entities).is_none()),
                "{query}"
            );
        }
        let question = parse_question("琥珀队巡检时固定使用什么？", &[], &entities).unwrap();
        for quote in [
            "琥珀队巡检时使用红外探头。",
            "琥珀队巡检时喜欢红外探头。",
            "琥珀队在室内巡检时固定使用红外探头。",
            "琥珀队巡检时固定使用红外探头吗。",
        ] {
            assert!(
                verify(
                    &question,
                    &usage_fact(&entities[0], quote, "红外探头"),
                    quote,
                    &entities
                )
                .is_none(),
                "{quote}"
            );
        }
        let question = parse_question("琥珀队巡检时使用什么？", &[], &entities).unwrap();
        for quote in [
            "琥珀队巡检时使用红外探头是错误的。",
            "琥珀队巡检时使用红外探头需要断电。",
            "琥珀队巡检时使用红外探头尚待确认。",
            "琥珀队巡检时使用红外探头。",
        ] {
            assert!(
                verify(
                    &question,
                    &fact(&entities[0], quote, quote),
                    quote,
                    &entities
                )
                .is_none(),
                "{quote}"
            );
        }
        let quote = "琥珀队巡检时使用红外探头需要断电。";
        let mut warning = fact(&entities[0], quote, "红外探头需要断电");
        warning.predicate = Predicate::from("warning");
        assert!(verify(&question, &warning, quote, &entities).is_none());
        let entities = vec![entity("雾桥仪", EntityType::Device)];
        let question = parse_question("雾桥仪校准时的输出电压是多少？", &[], &entities).unwrap();
        let quote = "雾桥仪校准时的输出电压是12伏。";
        assert_eq!(
            verify(
                &question,
                &fact(&entities[0], quote, "12伏"),
                quote,
                &entities
            )
            .unwrap()
            .value,
            "12伏"
        );
        assert_eq!(question.activity.as_deref(), Some("校准"));
        assert!(pending_relation_matches(
            &question,
            "更正：雾桥仪校准时的输出电压是24伏。",
            &entities
        ));
        assert!(!pending_relation_matches(
            &question,
            "雾桥仪巡检时的输出电压是24伏。",
            &entities
        ));
        let entities = vec![entity("琥珀队", EntityType::Organization)];
        let quote = "琥珀队画图纸时固定使用炭笔。";
        let mut memory = usage_fact(&entities[0], quote, "炭笔");
        memory.scope.conditions = vec![
            ScopeCondition::Activity("图纸".into()),
            ScopeCondition::Activity("其他活动".into()),
        ];
        let question = parse_question("琥珀队画图纸时固定用哪种笔？", &[], &entities).unwrap();
        assert_eq!(
            grounded_query_conditions(&question, &[memory], &entities),
            vec![
                ScopeCondition::Activity("图纸".into()),
                ScopeCondition::Activity("画图纸".into())
            ]
        );
    }

    #[test]
    fn temporal_qualifiers_and_preference_strength_are_not_erased() {
        let entities = vec![entity("榆舟社团", EntityType::Organization)];
        let question = parse_question(
            "他们每周什么时候开例会？",
            &["榆舟社团。".into()],
            &entities,
        )
        .unwrap();
        let quote = "榆舟社团的例会时间是每月三日上午九点。";
        assert!(
            verify(
                &question,
                &fact(&entities[0], quote, "每月三日上午九点"),
                quote,
                &entities
            )
            .is_none()
        );
        let entities = vec![entity("顾衡", EntityType::Person)];
        let question = parse_question("顾衡最喜欢什么饮料？", &[], &entities).unwrap();
        let quote = "顾衡喜欢的饮料是无糖豆浆。";
        assert!(
            verify(
                &question,
                &fact(&entities[0], quote, "无糖豆浆"),
                quote,
                &entities
            )
            .is_none()
        );
    }
}
