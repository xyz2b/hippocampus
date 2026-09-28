mod common;

use hippocampus::chat::{ChatConfig, ChatMemoryService, RetrievalMode};
use hippocampus::*;
use serde_json::{Value, json};

fn fact(id: &str, name: &str, activity: &str, value: &str, quote: &str) -> CandidateMemory {
    let mut candidate = CandidateMemory::new(
        UserId::from("alice"),
        EntityId::from(name),
        MemoryType::Fact,
        Predicate::from("uses"),
        MemoryValue::Text(value.into()),
        Timestamp(100),
        Evidence::new(
            EvidenceId::from(id),
            SessionId::from("session"),
            TurnId::from(id),
            quote,
        ),
    );
    candidate.scope.conditions = vec![ScopeCondition::Activity(activity.into())];
    candidate
}

fn source(id: &str, quote: &str, sequence: usize) -> SourceRecord {
    serde_json::from_value(json!({
        "id":id,"session_id":"session","turn_id":id,"observed_at":100,
        "received_sequence":sequence,"request":{"text":quote,"capture_source":true},
        "enrichment":"committed","error":null,"result":null,"base_revision":0
    }))
    .unwrap()
}

struct Fixture {
    path: std::path::PathBuf,
    service: ChatMemoryService,
}

impl Fixture {
    fn new() -> Self {
        Self::from_facts(vec![
            (
                EntityType::Person,
                fact(
                    "sketch-a",
                    "冯砚川",
                    "速写",
                    "炭笔",
                    "冯砚川画速写时固定使用炭笔。",
                ),
            ),
            (
                EntityType::Person,
                fact(
                    "sketch-b",
                    "冯序宁",
                    "画速写",
                    "钢笔",
                    "冯序宁画速写时固定使用钢笔。",
                ),
            ),
            (
                EntityType::Person,
                fact(
                    "calligraphy-a",
                    "冯砚川",
                    "写书法",
                    "毛笔",
                    "冯砚川写书法时固定使用毛笔。",
                ),
            ),
            (
                EntityType::Device,
                fact(
                    "calibration",
                    "晴川仪器",
                    "校准",
                    "标准电阻",
                    "晴川仪器校准时使用标准电阻。",
                ),
            ),
            (
                EntityType::Organization,
                fact(
                    "inspection",
                    "青苔小队",
                    "巡检",
                    "测温枪",
                    "青苔小队巡检时使用测温枪。",
                ),
            ),
            (
                EntityType::Person,
                fact(
                    "exercise",
                    "林映禾",
                    "晨练",
                    "跳绳",
                    "林映禾晨练时使用跳绳。",
                ),
            ),
        ])
    }

    fn from_facts(facts: Vec<(EntityType, CandidateMemory)>) -> Self {
        let path =
            std::env::temp_dir().join(format!("query-scope-{}.sqlite3", diagnostics::new_id()));
        let mut store = SqliteMemoryStore::open(&path, UserId::from("alice")).unwrap();
        store
            .transaction(|state| {
                for (index, (kind, candidate)) in facts.iter().enumerate() {
                    let id = candidate.subject_id().unwrap().clone();
                    state.entities.upsert(Entity {
                        canonical_name: id.0.clone(),
                        id,
                        owner_id: candidate.owner_id.clone(),
                        entity_type: kind.clone(),
                        aliases: vec![],
                    });
                    let evidence = &candidate.evidence[0];
                    let mut record = source(&evidence.turn_id.0, &evidence.quote, index + 1);
                    if let Some(ScopeCondition::Task(task)) = candidate
                        .scope
                        .conditions
                        .iter()
                        .find(|c| matches!(c, ScopeCondition::Task(_)))
                    {
                        record.request.as_mut().unwrap()["task_id"] = json!(task.0);
                    }
                    state.sources.insert(evidence.turn_id.0.clone(), record);
                }
                state.with_engine(InMemoryPredicateRegistry::default(), |engine, _| {
                    for (_, candidate) in facts {
                        assert!(matches!(
                            engine.ingest(common::grounded(candidate))?,
                            IngestOutcome::Applied { .. }
                        ));
                    }
                    Ok(())
                })
            })
            .unwrap();
        let service = ChatMemoryService::new(
            store,
            ChatConfig {
                retrieval_mode: Some(RetrievalMode::Local),
                ..ChatConfig::default()
            },
        )
        .unwrap();
        Self { path, service }
    }

    fn call(&mut self, tool: &str, arguments: Value) -> Value {
        let value = self.service.call(tool, arguments, || false).unwrap();
        assert_eq!(value["model_calls"], json!([]), "{value}");
        value
    }

    fn recall(&mut self, query: &str, context: &[&str]) -> Value {
        self.call("memory_recall", json!({"query":query,"context":context}))
    }

    fn validate(&mut self, query: &str, context: &[&str], value: &str, source_id: &str) -> Value {
        self.call("memory_validate_answer", json!({
            "query":query,"context":context,"draft_answer":format!("{value}。"),"source_ids":[source_id]
        }))
    }

    fn assert_no_approval(&mut self, query: &str, source_id: &str, value: &str) {
        let recalled = self.recall(query, &[]);
        assert_eq!(
            recalled["answer_contract"]["safe_source_ids"],
            json!([]),
            "{query}: {recalled}"
        );
        let validated = self.validate(query, &[], value, source_id);
        assert_eq!(validated["approved"], false, "{query}: {validated}");
        assert_eq!(
            validated["approved_source_ids"],
            json!([]),
            "{query}: {validated}"
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[test]
fn explicit_activity_and_relation_reach_recall_and_final_answer_gate() {
    let mut fixture = Fixture::new();
    let mut failures = Vec::new();
    for (query, context, value, id) in [
        ("冯砚川画速写时固定用哪种笔？", vec![], "炭笔", "sketch-a"),
        (
            "画速写时固定用哪种笔？",
            vec!["我问的是冯砚川。"],
            "炭笔",
            "sketch-a",
        ),
        ("冯序宁画速写时固定用哪种笔？", vec![], "钢笔", "sketch-b"),
        (
            "冯砚川写书法时固定用哪种笔？",
            vec![],
            "毛笔",
            "calligraphy-a",
        ),
        ("冯砚川画速写拿的是哪种笔？", vec![], "炭笔", "sketch-a"),
        (
            "晴川仪器校准时使用什么？",
            vec![],
            "标准电阻",
            "calibration",
        ),
        (
            "校准时使用什么？",
            vec!["晴川仪器。"],
            "标准电阻",
            "calibration",
        ),
        ("青苔小队巡检时用什么？", vec![], "测温枪", "inspection"),
        (
            "巡检时使用哪种枪？",
            vec!["青苔小队。"],
            "测温枪",
            "inspection",
        ),
        ("林映禾晨练时使用什么？", vec![], "跳绳", "exercise"),
    ] {
        let recalled = fixture.recall(query, &context);
        let contract = &recalled["answer_contract"];
        let validated = fixture.validate(query, &context, value, id);
        if contract["safe_answer"] != json!(format!("{value}。"))
            || contract["safe_source_ids"] != json!([id])
            || validated["approved"] != true
            || validated["approved_source_ids"] != json!([id])
        {
            failures.push(format!(
                "{query} {context:?}: recall={contract}; validation={validated}"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn omitted_wrong_negated_and_hypothetical_activity_never_release_conditional_sources() {
    let mut fixture = Fixture::new();
    for query in [
        "冯砚川固定用哪种笔？",
        "冯砚川画水彩时固定用哪种笔？",
        "冯砚川不画速写时固定用哪种笔？",
        "如果冯砚川画速写时固定用哪种笔？",
        "冯砚川可能画速写时固定用哪种笔？",
    ] {
        let recalled = fixture.recall(query, &[]);
        assert_eq!(
            recalled["source_evidence"],
            json!([]),
            "{query}: {recalled}"
        );
        fixture.assert_no_approval(query, "sketch-a", "炭笔");
    }
    for query in [
        "冯砚川画速写时喜欢哪种笔？",
        "冯砚川画速写时固定用哪种纸？",
        "冯砚川画速写时拿着哪种笔？",
        "冯砚川画速写时持有哪种笔？",
        "冯砚川画速写时借来哪种笔？",
    ] {
        fixture.assert_no_approval(query, "sketch-a", "炭笔");
    }
    let wrong_activity = fixture.validate("冯砚川写书法时固定用哪种笔？", &[], "炭笔", "sketch-a");
    assert_eq!(wrong_activity["approved"], false, "{wrong_activity}");
    assert_eq!(wrong_activity["approved_source_ids"], json!([]));
}

#[test]
fn activity_does_not_guess_a_missing_subject() {
    let mut fixture = Fixture::new();
    let recalled = fixture.recall("画速写时固定用哪种笔？", &[]);
    assert_eq!(
        recalled["answer_contract"]["status"], "clarification_required",
        "{recalled}"
    );
    assert_eq!(recalled["answer_contract"]["safe_source_ids"], json!([]));
}

#[test]
fn activity_match_does_not_bypass_other_and_conditions_task_or_expiry() {
    let query = "林映禾晨练时使用什么？";
    for (condition, quote) in [
        (
            ScopeCondition::Location(LocationScope::City("北京".into())),
            "林映禾在北京晨练时使用跳绳。",
        ),
        (
            ScopeCondition::Time(TimeScope::Date("2026-09-26".into())),
            "林映禾在2026-09-26晨练时使用跳绳。",
        ),
        (
            ScopeCondition::Companion(EntityOrRole::Role(CompanionRole::Friends)),
            "林映禾与朋友晨练时使用跳绳。",
        ),
        (
            ScopeCondition::Task(TaskId::from("task-a")),
            "林映禾晨练时使用跳绳。",
        ),
    ] {
        let mut candidate = fact("guarded", "林映禾", "晨练", "跳绳", quote);
        let task_scoped = matches!(condition, ScopeCondition::Task(_));
        candidate.scope.conditions.push(condition);
        let mut fixture = Fixture::from_facts(vec![(EntityType::Person, candidate)]);
        fixture.assert_no_approval(query, "guarded", "跳绳");
        let wrong_task = fixture.call(
            "memory_validate_answer",
            json!({
                "query":query,"task_id":"task-b","draft_answer":"跳绳。","source_ids":["guarded"]
            }),
        );
        assert_eq!(wrong_task["approved"], false, "{wrong_task}");
        assert_eq!(wrong_task["approved_source_ids"], json!([]));
        if task_scoped {
            let correct_task = fixture.call("memory_validate_answer", json!({
                "query":query,"task_id":"task-a","draft_answer":"跳绳。","source_ids":["guarded"]
            }));
            assert_eq!(correct_task["approved"], true, "{correct_task}");
            assert_eq!(correct_task["approved_source_ids"], json!(["guarded"]));
        }
    }
    let mut expired = fact(
        "expired",
        "林映禾",
        "晨练",
        "跳绳",
        "林映禾晨练时使用跳绳。",
    );
    expired.temporal.expires_at = Some(Timestamp(150));
    expired.retention_tier = RetentionTier::ShortTerm;
    Fixture::from_facts(vec![(EntityType::Person, expired)])
        .assert_no_approval(query, "expired", "跳绳");
}

#[test]
fn generic_complete_assertion_keeps_activity_and_answer_value() {
    let quote = "晴川仪器校准时的输出电压是12伏。";
    let mut candidate = fact("generic", "晴川仪器", "校准", quote, quote);
    candidate.predicate = Predicate::from("fact");
    let mut fixture = Fixture::from_facts(vec![(EntityType::Device, candidate)]);
    let query = "晴川仪器校准时的输出电压是什么？";
    let recalled = fixture.recall(query, &[]);
    assert_eq!(
        recalled["answer_contract"]["safe_answer"], "12伏。",
        "{recalled}"
    );
    assert_eq!(
        recalled["answer_contract"]["safe_source_ids"],
        json!(["generic"])
    );
    let approved = fixture.validate(query, &[], "12伏", "generic");
    assert_eq!(approved["approved"], true, "{approved}");
    assert_eq!(approved["approved_source_ids"], json!(["generic"]));
}

#[test]
fn whole_usage_sentence_is_not_an_independently_verified_object_value() {
    for quote in [
        "晴川仪器校准时使用标准电阻。",
        "晴川仪器校准时使用标准电阻需要断电。",
        "晴川仪器校准时使用标准电阻是错误的。",
        "晴川仪器校准时使用标准电阻尚待确认。",
    ] {
        let mut candidate = fact("generic", "晴川仪器", "校准", quote, quote);
        candidate.predicate = Predicate::from("fact");
        let mut fixture = Fixture::from_facts(vec![(EntityType::Device, candidate)]);
        let query = "晴川仪器校准时使用什么？";
        let recalled = fixture.recall(query, &[]);
        assert_eq!(
            recalled["source_evidence"],
            json!([]),
            "{quote}: {recalled}"
        );
        fixture.assert_no_approval(query, "generic", "标准电阻");
    }
}

#[test]
fn explicit_question_cannot_override_trusted_activity_filter() {
    let fixture = Fixture::new();
    let store = SqliteMemoryStore::open(&fixture.path, UserId::from("alice")).unwrap();
    let mut runtime = RuntimeContext::current(UserId::from("alice"), Timestamp(200));
    runtime
        .conditions
        .push(ScopeCondition::Activity("写书法".into()));
    let recalled = store
        .recall_local(
            "冯砚川画速写时固定用哪种笔？",
            &[],
            &runtime,
            InMemoryPredicateRegistry::default(),
            &InMemorySemanticRelationRegistry::default(),
            &LocalRecallBudget::default(),
        )
        .unwrap();
    assert_eq!(recalled["source_evidence"], json!([]), "{recalled}");
    assert_eq!(recalled["backend_generation_calls_in_recall"], 0);
}

#[test]
fn source_relation_and_unasked_activity_prefix_cannot_be_discarded() {
    for (predicate, quote, query) in [
        ("likes", "林映禾晨练时喜欢跳绳。", "林映禾晨练时使用什么？"),
        ("holds", "林映禾晨练时拿着跳绳。", "林映禾晨练时使用什么？"),
        ("uses", "林映禾晨练时使用跳绳。", "林映禾使用什么？"),
        (
            "uses",
            "林映禾下雨天晨练时使用跳绳。",
            "林映禾晨练时使用什么？",
        ),
    ] {
        let mut candidate = fact("restricted", "林映禾", "晨练", "跳绳", quote);
        candidate.predicate = Predicate::from(predicate);
        let mut fixture = Fixture::from_facts(vec![(EntityType::Person, candidate)]);
        fixture.assert_no_approval(query, "restricted", "跳绳");
    }
}

#[test]
fn fresh_uncommitted_correction_invalidates_previously_supported_answer() {
    for (quote, same_activity) in [
        ("林映禾晨练时已经不再使用跳绳，改为使用哑铃。", true),
        ("现在林映禾晨练时用哑铃。", true),
        ("林映禾晨练时不用跳绳了。", true),
        ("林映禾晨练时已经改用哑铃。", true),
        ("林映禾游泳时用浮板。", false),
    ] {
        let mut fixture = Fixture::new();
        SqliteMemoryStore::open(&fixture.path, UserId::from("alice"))
            .unwrap()
            .transaction(|state| {
                let mut correction = source("correction", quote, 99);
                correction.enrichment = "pending".into();
                state.sources.insert("correction".into(), correction);
                Ok(())
            })
            .unwrap();
        let query = "林映禾晨练时使用什么？";
        if same_activity {
            fixture.assert_no_approval(query, "exercise", "跳绳");
        } else {
            let approved = fixture.validate(query, &[], "跳绳", "exercise");
            assert_eq!(approved["approved"], true, "{quote}: {approved}");
            assert_eq!(approved["approved_source_ids"], json!(["exercise"]));
        }
    }
}

fn complete_fact(id: &str, name: &str, quote: &str) -> CandidateMemory {
    let mut candidate = fact(id, name, "", quote, quote);
    candidate.predicate = Predicate::from("fact");
    candidate.scope.conditions.clear();
    candidate
}

#[test]
fn default_setting_paraphrase_reaches_recall_and_final_answer_gate() {
    let mut fixture = Fixture::from_facts(vec![
        (
            EntityType::Device,
            complete_fact("paper", "雾岚打印机", "雾岚打印机的默认纸张规格是A4。"),
        ),
        (
            EntityType::Device,
            complete_fact(
                "other-paper",
                "杉木打印机",
                "杉木打印机的默认纸张尺寸是B5。",
            ),
        ),
        (
            EntityType::Device,
            complete_fact("voltage", "晴川仪器", "晴川仪器的默认输出电压是12伏。"),
        ),
        (
            EntityType::Organization,
            complete_fact("payment", "青苔小队", "青苔小队的默认收款方式是银行转账。"),
        ),
    ]);
    for (query, context, answer, id) in [
        (
            "不改纸张设置时，雾岚打印机会按哪种尺寸打印？",
            vec![],
            "A4",
            "paper",
        ),
        (
            "保持纸张设置不变时，杉木打印机会按哪种尺寸打印？",
            vec![],
            "B5",
            "other-paper",
        ),
        (
            "未修改纸张设置时，它会按哪种尺寸打印？",
            vec!["雾岚打印机。"],
            "A4",
            "paper",
        ),
        (
            "不更改纸张设置的时候，杉木打印机的纸张尺寸是什么？",
            vec![],
            "B5",
            "other-paper",
        ),
        (
            "不改输出电压设置时，晴川仪器的输出电压是多少？",
            vec![],
            "12伏",
            "voltage",
        ),
        (
            "不改收款方式设置时，青苔小队的收款方式是什么？",
            vec![],
            "银行转账",
            "payment",
        ),
    ] {
        let recalled = fixture.recall(query, &context);
        assert_eq!(
            recalled["answer_contract"]["safe_answer"],
            format!("{answer}。"),
            "{query}: {recalled}"
        );
        assert_eq!(
            recalled["answer_contract"]["safe_source_ids"],
            json!([id]),
            "{query}"
        );
        let validated = fixture.validate(query, &context, answer, id);
        assert_eq!(validated["approved"], true, "{query}: {validated}");
        assert_eq!(validated["approved_source_ids"], json!([id]));
    }
    let query = "不改纸张设置时，会按哪种尺寸打印？";
    let recalled = fixture.recall(query, &[]);
    assert_eq!(
        recalled["answer_contract"]["status"], "clarification_required",
        "{recalled}"
    );
    assert_eq!(recalled["answer_contract"]["safe_source_ids"], json!([]));
    let validated = fixture.validate(query, &["杉木打印机。"], "B5", "other-paper");
    assert_eq!(validated["approved"], true, "{validated}");
}

#[test]
fn default_setting_certificate_keeps_condition_relation_scope_and_freshness() {
    let quote = "雾岚打印机的默认纸张规格是A4。";
    let candidate = complete_fact("paper", "雾岚打印机", quote);
    let mut fixture = Fixture::from_facts(vec![(EntityType::Device, candidate.clone())]);
    for query in [
        "改过纸张设置时，雾岚打印机会按哪种尺寸打印？",
        "修改纸张设置后，雾岚打印机会按哪种尺寸打印？",
        "不知道纸张设置是否改过时，雾岚打印机会按哪种尺寸打印？",
        "如果不改纸张设置时，雾岚打印机会按哪种尺寸打印？",
        "不改方向设置时，雾岚打印机会按哪种尺寸打印？",
        "不改纸张和方向设置时，雾岚打印机会按哪种尺寸打印？",
        "不改纸张设置时，雾岚打印机会按哪种分辨率打印？",
        "不改纸张设置时，雾岚打印机扫描时会按哪种尺寸打印？",
        "不改纸张设置时，今天雾岚打印机会按哪种尺寸打印？",
        "不改纸张设置时，雾岚打印机会按哪种尺寸打印，杉木打印机呢？",
        "画速写时，雾岚打印机的默认纸张规格是什么？",
    ] {
        fixture.assert_no_approval(query, "paper", "A4");
    }
    let query = "不改纸张设置时，雾岚打印机会按哪种尺寸打印？";
    for unsupported in [
        "雾岚打印机的纸张规格是A4。",
        "雾岚打印机的默认扫描规格是A4。",
        "雾岚打印机的默认纸张规格不是A4。",
        "雾岚打印机的默认纸张规格可能是A4。",
    ] {
        Fixture::from_facts(vec![(
            EntityType::Device,
            complete_fact("paper", "雾岚打印机", unsupported),
        )])
        .assert_no_approval(query, "paper", "A4");
    }
    for condition in [
        ScopeCondition::Activity("扫描".into()),
        ScopeCondition::Location(LocationScope::City("北京".into())),
        ScopeCondition::Time(TimeScope::Date("2026-09-26".into())),
        ScopeCondition::Task(TaskId::from("print-task")),
    ] {
        let mut scoped = candidate.clone();
        scoped.scope.conditions.push(condition);
        Fixture::from_facts(vec![(EntityType::Device, scoped)])
            .assert_no_approval(query, "paper", "A4");
    }
    SqliteMemoryStore::open(&fixture.path, UserId::from("alice"))
        .unwrap()
        .transaction(|state| {
            let mut correction = source("correction", "雾岚打印机的纸张设置已经改为B5。", 99);
            correction.enrichment = "pending".into();
            state.sources.insert("correction".into(), correction);
            Ok(())
        })
        .unwrap();
    fixture.assert_no_approval(query, "paper", "A4");
    let ambiguous = complete_fact("conflict", "雾岚打印机", "雾岚打印机的默认纸张尺寸是B5。");
    Fixture::from_facts(vec![
        (EntityType::Device, candidate),
        (EntityType::Device, ambiguous),
    ])
    .assert_no_approval(query, "paper", "A4");
}

#[test]
fn fronted_activity_is_preserved_through_clarification_and_answer_gate() {
    let mut fixture = Fixture::new();
    for (query, context, value, id) in [
        ("画速写时，冯砚川固定用哪种笔？", vec![], "炭笔", "sketch-a"),
        ("巡检期间，青苔小队用什么？", vec![], "测温枪", "inspection"),
        (
            "校准的时候，使用什么？",
            vec!["晴川仪器。"],
            "标准电阻",
            "calibration",
        ),
    ] {
        let recalled = fixture.recall(query, &context);
        assert_eq!(
            recalled["answer_contract"]["safe_answer"],
            format!("{value}。"),
            "{recalled}"
        );
        assert_eq!(recalled["answer_contract"]["safe_source_ids"], json!([id]));
        let validated = fixture.validate(query, &context, value, id);
        assert_eq!(validated["approved"], true, "{validated}");
        assert_eq!(validated["approved_source_ids"], json!([id]));
    }
    for query in [
        "不画速写时，冯砚川固定用哪种笔？",
        "画水彩时，冯砚川固定用哪种笔？",
        "如果画速写时，冯砚川固定用哪种笔？",
        "画速写时，冯砚川写书法时固定用哪种笔？",
        "画速写时，冯砚川可能固定用哪种笔？",
    ] {
        fixture.assert_no_approval(query, "sketch-a", "炭笔");
    }
}

#[test]
fn committed_current_setting_cannot_be_replaced_by_its_default() {
    for changed in [
        "雾岚打印机的当前纸张尺寸是B5。",
        "雾岚打印机的纸张尺寸当前是B5。",
        "雾岚打印机的纸张规格现在是B5。",
        "雾岚打印机的纸张尺寸目前是B5。",
        "雾岚打印机的纸张设置已经改为B5。",
        "雾岚打印机的纸张已改成B5。",
        "雾岚打印机的纸张设置为B5。",
        "雾岚打印机的当前纸张规格不清楚。",
        "雾岚打印机的纸张规格是B5。",
        "更正：雾岚打印机的默认纸张规格已经作废。",
        "我撤回雾岚打印机之前的设置记录。",
    ] {
        let mut fixture = Fixture::from_facts(vec![
            (
                EntityType::Device,
                complete_fact("default", "雾岚打印机", "雾岚打印机的默认纸张规格是A4。"),
            ),
            (
                EntityType::Device,
                complete_fact("current", "雾岚打印机", changed),
            ),
        ]);
        for query in [
            "保持纸张设置不变时，雾岚打印机会按哪种尺寸打印？",
            "不改纸张设置时，雾岚打印机会按哪种尺寸打印？",
            "不改纸张设置时，雾岚打印机的纸张规格是什么？",
        ] {
            let recalled = fixture.recall(query, &[]);
            assert_eq!(
                recalled["source_evidence"],
                json!([]),
                "{changed}: {recalled}"
            );
            fixture.assert_no_approval(query, "default", "A4");
        }
    }
}

#[test]
fn irrelevant_or_inactive_settings_do_not_block_a_default() {
    let query = "不改纸张设置时，雾岚打印机会按哪种尺寸打印？";
    for unrelated in [
        complete_fact("other", "杉木打印机", "杉木打印机的当前纸张尺寸是B5。"),
        complete_fact("other", "雾岚打印机", "雾岚打印机的当前纸张方向是横向。"),
        complete_fact("other", "雾岚打印机", "雾岚打印机的当前输出电压是12伏。"),
    ] {
        let mut fixture = Fixture::from_facts(vec![
            (
                EntityType::Device,
                complete_fact("default", "雾岚打印机", "雾岚打印机的默认纸张规格是A4。"),
            ),
            (EntityType::Device, unrelated),
        ]);
        let approved = fixture.validate(query, &[], "A4", "default");
        assert_eq!(approved["approved"], true, "{approved}");
    }
    for condition in [
        ScopeCondition::Activity("扫描".into()),
        ScopeCondition::Location(LocationScope::City("北京".into())),
        ScopeCondition::Time(TimeScope::Date("2026-09-26".into())),
        ScopeCondition::Task(TaskId::from("another-task")),
    ] {
        let mut current = complete_fact("current", "雾岚打印机", "雾岚打印机的当前纸张尺寸是B5。");
        current.scope.conditions.push(condition);
        let mut fixture = Fixture::from_facts(vec![
            (
                EntityType::Device,
                complete_fact("default", "雾岚打印机", "雾岚打印机的默认纸张规格是A4。"),
            ),
            (EntityType::Device, current),
        ]);
        let approved = fixture.validate(query, &[], "A4", "default");
        assert_eq!(approved["approved"], true, "{approved}");
    }
    for status in [MemoryStatus::Expired, MemoryStatus::Superseded] {
        let mut fixture = Fixture::from_facts(vec![
            (
                EntityType::Device,
                complete_fact("default", "雾岚打印机", "雾岚打印机的默认纸张规格是A4。"),
            ),
            (
                EntityType::Device,
                complete_fact("current", "雾岚打印机", "雾岚打印机的当前纸张尺寸是B5。"),
            ),
        ]);
        SqliteMemoryStore::open(&fixture.path, UserId::from("alice"))
            .unwrap()
            .transaction(|state| {
                let mut memory = state
                    .repository
                    .list_by_owner(&UserId::from("alice"))?
                    .into_iter()
                    .find(|m| m.evidence.iter().any(|e| e.turn_id.0 == "current"))
                    .unwrap();
                memory.status = status;
                state.repository.update(memory)?;
                Ok(())
            })
            .unwrap();
        let approved = fixture.validate(query, &[], "A4", "default");
        assert_eq!(approved["approved"], true, "{approved}");
    }
}

#[test]
fn review_mixed_source_does_not_revive_expired_override() {
    let current = "雾岚打印机的当前纸张尺寸是B5。";
    let other = "雾岚打印机的设备编号是P72。";
    let quote = format!("{current}{other}");
    let mut overriding = complete_fact("mixed", "雾岚打印机", &quote);
    overriding.value = MemoryValue::Text(current.into());
    let mut identity = complete_fact("mixed", "雾岚打印机", &quote);
    identity.evidence[0].id = EvidenceId::from("mixed:identity");
    identity.value = MemoryValue::Text(other.into());
    let mut fixture = Fixture::from_facts(vec![
        (
            EntityType::Device,
            complete_fact("default", "雾岚打印机", "雾岚打印机的默认纸张规格是A4。"),
        ),
        (EntityType::Device, overriding),
        (EntityType::Device, identity),
    ]);
    SqliteMemoryStore::open(&fixture.path, UserId::from("alice"))
        .unwrap()
        .transaction(|state| {
            let mut m = state
                .repository
                .list_by_owner(&UserId::from("alice"))?
                .into_iter()
                .find(|m| m.value == MemoryValue::Text(current.into()))
                .unwrap();
            m.status = MemoryStatus::Expired;
            state.repository.update(m)?;
            Ok(())
        })
        .unwrap();
    let q = "保持纸张设置不变时，雾岚打印机会按哪种尺寸打印？";
    let result = fixture.recall(q, &[]);
    let checked = fixture.validate(q, &[], "A4", "default");
    println!(
        "mixed => status={} contract={} approved={}",
        result["evidence_status"], result["answer_contract"], checked["approved"]
    );
    assert_eq!(
        checked["approved"], true,
        "expired override must not inherit the active identity memory's status"
    );
}
