//! ④-1：只经过生产 W2 接口，不调用模型、不写数据库。
use hippocampus::contract::{Interp, Precision, ResolutionStatus, Subject, TimeRole};
use hippocampus::w2::{self, Batch, Context, Error, Relation, Status};
use hippocampus::{EntityId, MemoryType, TaskId};
use serde_json::{Value as Json, json};

fn candidate(sentence: &str) -> Json {
    json!({"kind":"assertion", "speech":"user_statement", "subject":{"entity_id":"self"},
        "relation":"fact", "memory_type":"fact",
        "value":{"kind":"text", "text":sentence, "quote":{"text":sentence}},
        "sentence":{"text":sentence}, "polarity":"affirm", "strength":null,
        "uncertainty":null, "conditions":[], "times":[]})
}

fn request(kind: &str, quote: &str, indices: &[usize]) -> Json {
    json!({"kind":kind, "quote":{"text":quote},
        "target":{"object":"facts", "candidate_indices":indices, "quote":null}, "deadline":null})
}

fn time(quote: &str, interp: Json) -> Json {
    json!({"role":"event", "quote":{"text":quote}, "interp":interp, "anchor":"said_on"})
}

fn catalog() -> Vec<Relation> {
    vec![
        Relation {
            id: "fact".into(),
            memory_types: vec![
                MemoryType::Fact,
                MemoryType::Preference,
                MemoryType::Constraint,
                MemoryType::Episode,
            ],
        },
        Relation {
            id: "lives_in".into(),
            memory_types: vec![MemoryType::Fact],
        },
        Relation {
            id: "count".into(),
            memory_types: vec![MemoryType::Fact],
        },
        Relation {
            id: "likes".into(),
            memory_types: vec![MemoryType::Preference],
        },
    ]
}

fn run(text: &str, candidates: Vec<Json>, requests: Vec<Json>) -> Result<Batch, Error> {
    run_at(text, candidates, requests, "2026-04-15", 28_800, None)
}

fn run_at(
    text: &str,
    candidates: Vec<Json>,
    requests: Vec<Json>,
    said_on: &str,
    offset: i32,
    task: Option<&TaskId>,
) -> Result<Batch, Error> {
    let output =
        w2::decode(&json!({"candidates":candidates,"memory_requests":requests}).to_string())?;
    let entities = vec![
        EntityId::from("self"),
        EntityId::from("child"),
        EntityId::from("shanghai"),
    ];
    let relations = catalog();
    w2::validate(
        output,
        &Context {
            text,
            entities: &entities,
            relations: &relations,
            said_on: Some(said_on),
            utc_offset_seconds: offset,
            task_id: task,
        },
    )
}

fn rejected(text: &str, candidate: Json, reason: &str) {
    let batch = run(text, vec![candidate], vec![]).unwrap();
    assert_eq!(batch.candidates[0].status, Status::Rejected);
    assert_eq!(
        batch.candidates[0].reasons,
        vec![format!("candidate_rejected:{reason}")]
    );
}

// 要求 1：严格形状解码（正／反）。
#[test]
fn schema_accepts_nulls_and_empty_output() {
    let c = candidate("我住上海。");
    let decoded = w2::decode(&json!({"candidates":[c],"memory_requests":[]}).to_string()).unwrap();
    assert_eq!(decoded.candidates[0].strength, None);
    assert_eq!(run("你好。", vec![], vec![]).unwrap().candidates.len(), 0);
}

#[test]
fn schema_errors_fail_the_whole_output() {
    assert_eq!(w2::decode("not json"), Err(Error::InvalidOutput));
    for path in ["strength", "uncertainty", "value", "sentence"] {
        let mut c = candidate("我住上海。");
        c.as_object_mut().unwrap().remove(path);
        assert_eq!(
            run("我住上海。", vec![candidate("我住上海。"), c], vec![]),
            Err(Error::InvalidOutput),
            "{path}"
        );
    }
    for (field, value) in [
        ("task_id", json!("foreign")),
        ("memory_type", json!(" ")),
        ("speech", json!("assistant")),
        (
            "value",
            json!({"kind":"integer", "integer":9223372036854775808u64, "quote":{"text":"1"}}),
        ),
    ] {
        let mut c = candidate("我住上海。");
        c[field] = value;
        assert_eq!(
            run("我住上海。", vec![c], vec![]),
            Err(Error::InvalidOutput),
            "{field}"
        );
    }
    let output = w2::decode(
        &json!({"candidates":[candidate("我住上海。")],"memory_requests":[]}).to_string(),
    )
    .unwrap();
    let mut output = output;
    output.candidates[0]
        .conditions
        .push(hippocampus::contract::TextQualifier {
            quote: hippocampus::contract::Quote { text: "\n".into() },
            resolved: false,
        });
    assert_eq!(
        w2::validate(
            output,
            &Context {
                text: "我住上海。",
                entities: &[],
                relations: &[],
                said_on: Some("2026-04-15"),
                utc_offset_seconds: 0,
                task_id: None
            }
        ),
        Err(Error::InvalidOutput)
    );
}

// 要求 2：原文还原、连续片段、后缀修正与全量重验。
#[test]
fn quotes_restore_original_spaces_and_typography() {
    let source = "我说：‘can’t’。 画 速写 时，可能使用 炭笔。";
    let mut c = candidate("我说：'can't'。画速写时，可能使用炭笔。");
    c["conditions"] = json!([{"quote":{"text":"画速写时"},"resolved":false}]);
    c["uncertainty"] = json!({"quote":{"text":"可能"},"resolved":false});
    let result = run(source, vec![c], vec![]).unwrap();
    let c = &result.candidates[0];
    assert_eq!(c.status, Status::Ok);
    assert_eq!(c.candidate.sentence.text, source);
    assert_eq!(c.candidate.conditions[0].quote.text, "画 速写 时");
    assert!(!c.candidate.conditions[0].resolved);
    assert!(!c.candidate.uncertainty.as_ref().unwrap().resolved);
}

#[test]
fn quotes_cannot_use_history_substrings_or_disjoint_sentences() {
    rejected("I know it.", candidate("now"), "sentence_not_found");
    rejected(
        "我住上海。这里没有历史里的地址。",
        candidate("我住北京。"),
        "sentence_not_found",
    );
    let mut c = candidate("我住上海。");
    c["sentence"]["text"] = json!("我住上海。我喜欢茶。");
    rejected("我住上海。中间一句。我喜欢茶。", c, "sentence_not_found");
    let mut c = candidate("我住上海。");
    c["sentence"]["text"] = json!("我住上海。注释：我喜欢茶。");
    rejected("我住上海。中间一句。我喜欢茶。", c, "sentence_not_found");
}

#[test]
fn sentence_only_suffix_is_trimmed_and_every_field_is_rechecked() {
    let mut c = candidate("我住上海。");
    c["sentence"]["text"] = json!("我住上海。Translation: I live in Shanghai.");
    let result = run("我住上海。", vec![c.clone()], vec![]).unwrap();
    assert_eq!(result.candidates[0].status, Status::Ok);
    assert_eq!(result.candidates[0].candidate.sentence.text, "我住上海。");
    c["conditions"] = json!([{"quote":{"text":"Translation"},"resolved":true}]);
    rejected("我住上海。", c.clone(), "condition_not_found");
    c["conditions"] = json!([]);
    c["relation"] = json!("invented");
    rejected("我住上海。", c, "unknown_relation");
    let mut c = candidate("我住上海。译文。");
    // 同时伪造 Value 的依据时，不能随着 sentence 一起偷偷修 Value。
    rejected("我住上海。", c.clone(), "value_quote_not_found");
    c["value"] = candidate("我住上海。")["value"].clone();
    assert_eq!(
        run("我住上海。", vec![c], vec![]).unwrap().candidates[0].status,
        Status::Ok
    );
}

#[test]
fn every_nested_quote_requires_current_source_evidence() {
    for (field, edit, reason) in [
        (
            "subject",
            json!({"unresolved":{"text":"他"}}),
            "subject_not_found",
        ),
        (
            "value",
            json!({"kind":"text","text":"北京","quote":{"text":"北京"}}),
            "value_quote_not_found",
        ),
        (
            "uncertainty",
            json!({"quote":{"text":"也许"},"resolved":false}),
            "uncertainty_not_found",
        ),
        (
            "conditions",
            json!([{"quote":{"text":"出差时"},"resolved":false}]),
            "condition_not_found",
        ),
        (
            "times",
            json!([time("去年", json!({"kind":"unknown"}))]),
            "time_quote_not_found",
        ),
        (
            "times",
            json!([time(
                "上海",
                json!({"kind":"anchored","anchor_quote":{"text":"毕业"},"unit":"year","offset":2})
            )]),
            "anchor_quote_not_found",
        ),
    ] {
        let mut c = candidate("我住上海。");
        c[field] = edit;
        rejected("我住上海。", c, reason);
    }
}

// 要求 3：目录、主体与可信任务。
#[test]
fn catalog_subjects_and_runtime_task_are_preserved() {
    let task = TaskId::from("project-a");
    let batch = run_at(
        "我住上海。",
        vec![candidate("我住上海。")],
        vec![],
        "2026-04-15",
        28_800,
        Some(&task),
    )
    .unwrap();
    assert_eq!(batch.candidates[0].status, Status::Ok);
    assert_eq!(batch.task_id, Some(task));
    let mut c = candidate("阿明住上海。");
    c["subject"] = json!({"entity_id":"阿明"});
    let batch = run("阿明住上海。", vec![c], vec![]).unwrap();
    assert_eq!(batch.candidates[0].status, Status::Unresolved);
    assert_eq!(batch.candidates[0].reasons, ["unresolved_subject"]);
    assert!(
        matches!(&batch.candidates[0].candidate.subject, Subject::Unresolved(s) if s.unresolved.text == "阿明")
    );
}

#[test]
fn unresolved_subject_is_not_bound_to_the_only_catalog_entity() {
    let mut c = candidate("他住上海。");
    c["subject"] = json!({"unresolved":{"text":"他"}});
    let output = w2::decode(&json!({"candidates":[c],"memory_requests":[]}).to_string()).unwrap();
    let entities = [EntityId::from("self")];
    let relations = catalog();
    let batch = w2::validate(
        output,
        &Context {
            text: "他住上海。",
            entities: &entities,
            relations: &relations,
            said_on: Some("2026-04-15"),
            utc_offset_seconds: 0,
            task_id: None,
        },
    )
    .unwrap();
    assert_eq!(batch.candidates[0].status, Status::Unresolved);
    assert!(matches!(
        batch.candidates[0].candidate.subject,
        Subject::Unresolved(_)
    ));
}

#[test]
fn catalog_mismatch_rejects_instead_of_guessing() {
    for (field, value, reason) in [
        (
            "subject",
            json!({"entity_id":"missing-id"}),
            "subject_not_found",
        ),
        ("relation", json!("unlisted"), "unknown_relation"),
        ("memory_type", json!("unknown"), "unknown_memory_type"),
    ] {
        let mut c = candidate("我住上海。");
        c[field] = value;
        rejected("我住上海。", c, reason);
    }
    let mut c = candidate("我住上海。");
    c["relation"] = json!("lives_in");
    c["memory_type"] = json!("preference");
    rejected("我住上海。", c, "memory_type_not_allowed");
    assert_eq!(
        run_at(
            "我住上海。",
            vec![candidate("我住上海。")],
            vec![],
            "2026-04-15",
            0,
            Some(&TaskId::from(" "))
        ),
        Err(Error::InvalidContext("task_id"))
    );
}

// 要求 4：Value 四分支与未解析限定语。
#[test]
fn value_branches_are_checked_without_semantic_inference() {
    for (source, value) in [
        (
            "我住 上海。",
            json!({"kind":"text","text":"上海","quote":{"text":"上海"}}),
        ),
        (
            "我有12个。",
            json!({"kind":"integer","integer":12,"quote":{"text":"12"}}),
        ),
        (
            "我没有许可。",
            json!({"kind":"boolean","boolean":false,"quote":{"text":"没有许可"}}),
        ),
        (
            "我住上海。",
            json!({"kind":"entity","entity_id":"shanghai","quote":{"text":"上海"}}),
        ),
    ] {
        let mut c = candidate(source);
        c["relation"] = json!("count");
        c["value"] = value;
        assert_eq!(
            run(source, vec![c], vec![]).unwrap().candidates[0].status,
            Status::Ok
        );
    }
    let mut c = candidate("我很喜欢炭笔。");
    c["memory_type"] = json!("preference");
    c["strength"] = json!("unknown");
    c["polarity"] = json!("unknown");
    let batch = run("我很喜欢炭笔。", vec![c], vec![]).unwrap();
    assert_eq!(
        serde_json::to_value(&batch.candidates[0].candidate).unwrap()["strength"],
        "unknown"
    );
    assert_eq!(
        serde_json::to_value(&batch.candidates[0].candidate).unwrap()["polarity"],
        "unknown"
    );
}

#[test]
fn value_mismatch_is_rejected_without_defaults_or_rounding() {
    let source = "我有12个，住上海。";
    for (value, reason) in [
        (
            json!({"kind":"text","text":"Shanghai","quote":{"text":"上海"}}),
            "value_text_not_found",
        ),
        (
            json!({"kind":"integer","integer":13,"quote":{"text":"12"}}),
            "integer_quote_mismatch",
        ),
        (
            json!({"kind":"entity","entity_id":"unknown","quote":{"text":"上海"}}),
            "unknown_value_entity",
        ),
    ] {
        let mut c = candidate(source);
        c["relation"] = json!("count");
        c["value"] = value;
        rejected(source, c, reason);
    }
    let mut c = candidate("我有1.5个。");
    c["relation"] = json!("count");
    c["value"] = json!({"kind":"integer","integer":1,"quote":{"text":"1.5"}});
    rejected("我有1.5个。", c, "integer_quote_mismatch");
    let mut c = candidate("我住上海。");
    c["value"] = json!({"kind":"text","text":"上海","quote":{"text":"上海"}});
    rejected("我住上海。", c, "fact_sentence_mismatch");
    let mut c = candidate("我住上海。");
    c["strength"] = json!("strong");
    rejected("我住上海。", c, "strength_not_applicable");
}

#[test]
fn assertion_requires_value_but_retraction_may_have_none() {
    let mut c = candidate("我撤回之前的住址断言。");
    c["value"] = Json::Null;
    rejected(
        "我撤回之前的住址断言。",
        c.clone(),
        "assertion_without_value",
    );
    c["kind"] = json!("retraction");
    assert_eq!(
        run("我撤回之前的住址断言。", vec![c], vec![])
            .unwrap()
            .candidates[0]
            .status,
        Status::Ok
    );
}

// 要求 5：按接收日换算，精度／未知状态保留，非法与明确矛盾拒收。
#[test]
fn calendar_dates_ranges_and_local_timezone_resolve_once() {
    let source = "我在2024年2月29日去了展览，4月1日至4月14日（含首尾两日）住上海。";
    let mut c = candidate(source);
    c["times"] = json!([
        time("2024年2月29日", json!({"kind":"date","date":"2024-02-29"})),
        time(
            "4月1日至4月14日（含首尾两日）",
            json!({"kind":"range","start":"2026-04-01","end_inclusive":"2026-04-14"})
        ),
    ]);
    let batch = run(source, vec![c], vec![]).unwrap();
    assert_eq!(batch.candidates[0].status, Status::Ok);
    let resolutions = &batch.candidates[0].time_resolutions;
    assert_eq!(resolutions[0].precision, Precision::Day);
    assert_eq!(
        resolutions[0].end_exclusive.unwrap() - resolutions[0].start.unwrap(),
        86_400
    );
    assert_eq!(
        resolutions[1].end_exclusive.unwrap() - resolutions[1].start.unwrap(),
        14 * 86_400
    );
    assert_eq!(
        time::OffsetDateTime::from_unix_timestamp(resolutions[1].end_exclusive.unwrap() + 28_800)
            .unwrap()
            .date()
            .to_string(),
        "2026-04-15"
    );
}

#[test]
fn relative_and_month_times_use_said_on_not_execution_clock() {
    for (quote, interp, date, days, precision) in [
        (
            "昨天",
            json!({"kind":"relative","unit":"day","offset":-1}),
            "2024-02-29",
            1,
            Precision::Day,
        ),
        (
            "三周前",
            json!({"kind":"relative","unit":"week","offset":-3}),
            "2024-02-09",
            1,
            Precision::Day,
        ),
        (
            "上个月",
            json!({"kind":"relative","unit":"month","offset":-1}),
            "2024-02-01",
            29,
            Precision::Month,
        ),
        (
            "去年",
            json!({"kind":"relative","unit":"year","offset":-1}),
            "2023-01-01",
            365,
            Precision::Year,
        ),
        (
            "去年五月",
            json!({"kind":"month","year_offset":-1,"month":5}),
            "2023-05-01",
            31,
            Precision::Month,
        ),
    ] {
        let source = format!("我{quote}去了上海。");
        let mut c = candidate(&source);
        c["times"] = json!([time(quote, interp)]);
        let batch = run_at(&source, vec![c], vec![], "2024-03-01", 28_800, None).unwrap();
        assert_eq!(batch.candidates[0].status, Status::Ok);
        let r = &batch.candidates[0].time_resolutions[0];
        assert_eq!(r.precision, precision);
        assert_eq!(
            time::OffsetDateTime::from_unix_timestamp(r.start.unwrap() + 28_800)
                .unwrap()
                .date()
                .to_string(),
            date
        );
        assert_eq!(r.end_exclusive.unwrap() - r.start.unwrap(), days * 86_400);
    }
}

#[test]
fn unknown_anchored_and_recurring_times_keep_original_meaning() {
    let source = "我那阵子在上海，毕业后的第二年搬家，每周三画画。";
    let mut c = candidate(source);
    c["times"] = json!([
        time("那阵子", json!({"kind":"unknown"})),
        time(
            "毕业后的第二年",
            json!({"kind":"anchored","anchor_quote":{"text":"毕业"},"unit":"year","offset":2})
        ),
        time("每周三", json!({"kind":"recurring"})),
    ]);
    let batch = run(source, vec![c], vec![]).unwrap();
    assert_eq!(batch.candidates[0].status, Status::Ok);
    assert_eq!(batch.candidates[0].time_resolutions.len(), 3);
    for r in &batch.candidates[0].time_resolutions {
        assert_eq!(r.status, ResolutionStatus::Unknown);
        assert_eq!(
            (r.start, r.end_exclusive, r.precision),
            (None, None, Precision::Unknown)
        );
    }
    assert!(matches!(
        batch.candidates[0].candidate.times[1].interp,
        Interp::Anchored { .. }
    ));
}

#[test]
fn invalid_calendar_overflow_and_literal_contradictions_are_rejected() {
    for (quote, interp) in [
        ("2026年4月15日", json!({"kind":"date","date":"2026-04-16"})),
        ("4 月 15 日", json!({"kind":"date","date":"2026-04-16"})),
        ("2026-04-15", json!({"kind":"date","date":"2026-04-16"})),
        (
            "4月1日至4月14日",
            json!({"kind":"range","start":"2026-04-01","end_inclusive":"2026-04-15"}),
        ),
        (
            "4月1日至14日",
            json!({"kind":"range","start":"2026-04-01","end_inclusive":"2026-04-15"}),
        ),
        ("2026年4月", json!({"kind":"date","date":"2026-04-01"})),
        ("2026年2月", json!({"kind":"date","date":"2026-02-30"})),
        ("2026年2月30日", json!({"kind":"date","date":"2026-02-28"})),
        ("2026年300月1日", json!({"kind":"date","date":"2026-03-01"})),
        ("2026年", json!({"kind":"date","date":"2026-01-01"})),
        (
            "之前",
            json!({"kind":"range","start":"2026-04-15","end_inclusive":"2026-04-14"}),
        ),
        ("之前", json!({"kind":"month","year_offset":0,"month":13})),
        (
            "之前",
            json!({"kind":"relative","unit":"week","offset":i64::MAX}),
        ),
        (
            "之前",
            json!({"kind":"relative","unit":"day","offset":i64::MIN}),
        ),
        (
            "之前",
            json!({"kind":"relative","unit":"month","offset":i64::MAX}),
        ),
        (
            "之前",
            json!({"kind":"relative","unit":"year","offset":i64::MAX}),
        ),
        ("之前", json!({"kind":"date","date":"9999-12-31"})),
    ] {
        let source = format!("我{quote}去了上海。");
        let mut c = candidate(&source);
        c["times"] = json!([time(quote, interp)]);
        rejected(&source, c, "invalid_time");
    }
}

#[test]
fn retention_role_is_only_accepted_in_retention_deadline() {
    let source = "我住上海，这条只保留到明天。";
    let mut t = time("明天", json!({"kind":"relative","unit":"day","offset":1}));
    t["role"] = json!("retention");
    let mut c = candidate(source);
    c["times"] = json!([t.clone()]);
    rejected(source, c, "retention_in_candidate");
    let mut r = request("retention", "只保留到明天", &[0]);
    r["deadline"] = t;
    let batch = run(source, vec![candidate(source)], vec![r]).unwrap();
    assert_eq!(
        batch.memory_requests[0]
            .request
            .deadline
            .as_ref()
            .unwrap()
            .role,
        TimeRole::Retention
    );
    assert_eq!(
        batch.memory_requests[0]
            .deadline_resolution
            .as_ref()
            .unwrap()
            .precision,
        Precision::Day
    );
}

#[test]
fn malformed_runtime_clock_is_not_a_model_failure() {
    assert_eq!(
        run_at(
            "我住上海。",
            vec![candidate("我住上海。")],
            vec![],
            "2026-02-30",
            0,
            None
        ),
        Err(Error::InvalidContext("said_on"))
    );
    assert_eq!(
        run_at(
            "我住上海。",
            vec![candidate("我住上海。")],
            vec![],
            "2026-04-15",
            i32::MAX,
            None
        ),
        Err(Error::InvalidContext("timezone"))
    );
}

// 要求 6：控制请求结构与拒收后的映射，不改变原始对象范围。
#[test]
fn requests_remap_surviving_candidates_without_widening_targets() {
    let source = "记住A。B只保留到明天。";
    let mut bad = candidate("记住A。");
    bad["relation"] = json!("unlisted");
    let mut retention = request("retention", "只保留到明天", &[1]);
    let mut t = time("明天", json!({"kind":"relative","unit":"day","offset":1}));
    t["role"] = json!("retention");
    retention["deadline"] = t;
    let batch = run(
        source,
        vec![bad, candidate("B只保留到明天。")],
        vec![request("remember", "记住A", &[0]), retention],
    )
    .unwrap();
    assert_eq!(batch.index_map, vec![None, Some(0)]);
    assert_eq!(
        batch.memory_requests[0].request.target.candidate_indices,
        vec![0]
    );
    assert!(batch.memory_requests[0].final_candidate_indices.is_empty());
    assert_eq!(batch.memory_requests[0].rejected_candidate_indices, vec![0]);
    assert_eq!(
        batch.memory_requests[1].request.target.candidate_indices,
        vec![1]
    );
    assert_eq!(batch.memory_requests[1].final_candidate_indices, vec![0]);
}

#[test]
fn source_unknown_and_old_fact_targets_keep_their_words() {
    let source = "记住这条。忘掉那个旧地址。";
    let mut source_request = request("remember", "记住这条", &[]);
    source_request["target"]["object"] = json!("source");
    let mut unknown = request("forget", "忘掉那个旧地址", &[]);
    unknown["target"]["object"] = json!("unknown");
    unknown["target"]["quote"] = json!({"text":"那个旧地址"});
    let mut old = unknown.clone();
    old["target"]["object"] = json!("facts");
    let batch = run(source, vec![], vec![source_request, unknown, old]).unwrap();
    assert_eq!(batch.memory_requests.len(), 3);
    assert_eq!(
        batch.memory_requests[1]
            .request
            .target
            .quote
            .as_ref()
            .unwrap()
            .text,
        "那个旧地址"
    );
    assert!(
        batch
            .memory_requests
            .iter()
            .all(|r| r.final_candidate_indices.is_empty())
    );
}

#[test]
fn invalid_request_structure_or_quotes_fail_closed() {
    let source = "记住这条，只保留到明天。";
    let base = request("remember", "记住这条", &[0]);
    let mut cases = Vec::new();
    let mut r = base.clone();
    r["target"]["candidate_indices"] = json!([1]);
    cases.push(r);
    let mut r = base.clone();
    r["target"]["object"] = json!("unknown");
    r["target"]["quote"] = json!({"text":"这条"});
    cases.push(r);
    let mut r = base.clone();
    r["target"]["object"] = json!("unknown");
    r["target"]["candidate_indices"] = json!([]);
    cases.push(r);
    let mut r = base.clone();
    r["quote"]["text"] = json!("忘掉这条");
    cases.push(r);
    let mut r = base.clone();
    r["target"]["quote"] = json!({"text":"地址"});
    cases.push(r);
    let mut r = base.clone();
    r["kind"] = json!("retention");
    cases.push(r);
    let mut r = base.clone();
    r["kind"] = json!("retention");
    r["deadline"] = time("明天", json!({"kind":"relative","unit":"day","offset":1}));
    cases.push(r);
    let mut r = base;
    let mut t = time("明天", json!({"kind":"relative","unit":"day","offset":1}));
    t["role"] = json!("retention");
    r["deadline"] = t;
    cases.push(r);
    for r in cases {
        assert_eq!(
            run(source, vec![candidate(source)], vec![r.clone()]),
            Err(Error::InvalidOutput),
            "{r}"
        );
    }
}

// 要求 7：局部失败不影响独立候选，索引不丢。
#[test]
fn rejected_candidate_does_not_discard_independent_candidates() {
    let source = "我住上海。他画画。";
    let mut unresolved = candidate("他画画。");
    unresolved["subject"] = json!({"unresolved":{"text":"他"}});
    let batch = run(
        source,
        vec![candidate("我住上海。"), candidate("我住北京。"), unresolved],
        vec![],
    )
    .unwrap();
    assert_eq!(
        batch
            .candidates
            .iter()
            .map(|c| c.status)
            .collect::<Vec<_>>(),
        vec![Status::Ok, Status::Rejected, Status::Unresolved]
    );
    assert_eq!(batch.index_map, vec![Some(0), None, Some(1)]);
    assert_eq!(
        batch.candidates.iter().map(|c| c.index).collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
}

#[test]
fn all_rejected_candidates_are_still_reported() {
    let batch = run(
        "我住上海。",
        vec![candidate("我住北京。"), candidate("他画画。")],
        vec![],
    )
    .unwrap();
    assert_eq!(batch.candidates.len(), 2);
    assert!(
        batch
            .candidates
            .iter()
            .all(|c| c.status == Status::Rejected)
    );
    assert_eq!(batch.index_map, vec![None, None]);
}

#[test]
fn ambiguous_sentence_cuts_are_rejected_instead_of_shortening_facts() {
    let mut c = candidate("我住上海");
    c["sentence"]["text"] = json!("我住上海（译文）");
    assert_eq!(
        run("我住上海", vec![c.clone()], vec![]).unwrap().candidates[0].status,
        Status::Ok
    );
    rejected("我住上海，喜欢茶。", c, "sentence_not_found");
    let mut c = candidate("我有3.");
    c["sentence"]["text"] = json!("我有3.（注释）");
    rejected("我有3.14元。", c, "sentence_not_found");
}

#[test]
fn coarse_numeric_month_and_year_keep_their_precision() {
    for (quote, interp, precision) in [
        (
            "2026年4月",
            json!({"kind":"month","year_offset":0,"month":4}),
            Precision::Month,
        ),
        (
            "2026年",
            json!({"kind":"relative","unit":"year","offset":0}),
            Precision::Year,
        ),
    ] {
        let source = format!("我在{quote}住上海。");
        let mut c = candidate(&source);
        c["times"] = json!([time(quote, interp)]);
        let batch = run(&source, vec![c], vec![]).unwrap();
        assert_eq!(batch.candidates[0].status, Status::Ok);
        assert_eq!(batch.candidates[0].time_resolutions[0].precision, precision);
    }
}

#[test]
fn whole_sentence_time_quotes_cannot_invent_calendar_precision() {
    for (quote, interp) in [
        (
            "我于2022年8月参加展会。",
            json!({"kind":"date","date":"2022-08-12"}),
        ),
        (
            "我于2022年参加展会。",
            json!({"kind":"date","date":"2022-08-12"}),
        ),
        (
            "我于2022-08参加展会。",
            json!({"kind":"date","date":"2022-08-12"}),
        ),
        (
            "我八月参加展会。",
            json!({"kind":"date","date":"2026-08-12"}),
        ),
        (
            "我于2022年八月参加展会。",
            json!({"kind":"date","date":"2022-08-12"}),
        ),
        (
            "我8月参加展会。",
            json!({"kind":"date","date":"2026-08-12"}),
        ),
        (
            "我于2022年8月12日参加展会。",
            json!({"kind":"date","date":"2022-08-13"}),
        ),
        (
            "我于2022年2月30日参加展会。",
            json!({"kind":"date","date":"2022-02-28"}),
        ),
        (
            "展期2022年8月12日至14日。",
            json!({"kind":"date","date":"2022-08-12"}),
        ),
        (
            "我于2022年8月参加展会。",
            json!({"kind":"month","year_offset":-4,"month":9}),
        ),
        (
            "我于2022年参加展会。",
            json!({"kind":"relative","unit":"year","offset":-3}),
        ),
    ] {
        let t = serde_json::from_value(time(quote, interp)).unwrap();
        assert!(
            w2::resolve_time_interp(&t, "2026-04-15", 28_800).is_err(),
            "whole quote accepted invented precision: {quote}"
        );
    }
}

#[test]
fn whole_sentence_calendar_quotes_keep_valid_dates_ranges_and_coarse_precision() {
    for (quote, interp, precision) in [
        (
            "我于2022年8月12日参加展会。",
            json!({"kind":"date","date":"2022-08-12"}),
            Precision::Day,
        ),
        (
            "我于2022年八月12日参加展会。",
            json!({"kind":"date","date":"2022-08-12"}),
            Precision::Day,
        ),
        (
            "我于2022-08-12参加展会。",
            json!({"kind":"date","date":"2022-08-12"}),
            Precision::Day,
        ),
        (
            "我于2022年8月参加展会。",
            json!({"kind":"month","year_offset":-4,"month":8}),
            Precision::Month,
        ),
        (
            "我于2022-08参加展会。",
            json!({"kind":"month","year_offset":-4,"month":8}),
            Precision::Month,
        ),
        (
            "我于2022年参加展会。",
            json!({"kind":"relative","unit":"year","offset":-4}),
            Precision::Year,
        ),
        (
            "我八月参加展会。",
            json!({"kind":"month","year_offset":0,"month":8}),
            Precision::Month,
        ),
        (
            "我去年8月参加展会。",
            json!({"kind":"month","year_offset":-1,"month":8}),
            Precision::Month,
        ),
        (
            "展期2022年8月12日至14日。",
            json!({"kind":"range","start":"2022-08-12","end_inclusive":"2022-08-14"}),
            Precision::Day,
        ),
        (
            "展期2022-08-12到2022-08-14（含首尾两日）。",
            json!({"kind":"range","start":"2022-08-12","end_inclusive":"2022-08-14"}),
            Precision::Day,
        ),
        (
            "我于2022 年 8 月 12 日到北京。",
            json!({"kind":"date","date":"2022-08-12"}),
            Precision::Day,
        ),
        (
            "我2021年首次参展，2022年8月12日再次参展。",
            json!({"kind":"date","date":"2022-08-12"}),
            Precision::Day,
        ),
        (
            "我昨天参加展会。",
            json!({"kind":"relative","unit":"day","offset":-1}),
            Precision::Day,
        ),
    ] {
        let t = serde_json::from_value(time(quote, interp)).unwrap();
        assert_eq!(
            w2::resolve_time_interp(&t, "2026-04-15", 28_800)
                .unwrap()
                .precision,
            precision,
            "{quote}"
        );
    }
}

#[test]
fn requests_restore_quotes_and_keep_unknown_retention_deadlines() {
    let source = "记 住 这条，只保留一阵子。";
    let mut r = request("remember", "记住这条", &[0]);
    r["target"]["quote"] = json!({"text":"这条"});
    let mut retention = request("retention", "只保留一阵子", &[0]);
    let mut deadline = time("一阵子", json!({"kind":"unknown"}));
    deadline["role"] = json!("retention");
    retention["deadline"] = deadline;
    let batch = run(source, vec![candidate(source)], vec![r, retention]).unwrap();
    assert_eq!(batch.memory_requests[0].request.quote.text, "记 住 这条");
    assert_eq!(
        batch.memory_requests[1]
            .deadline_resolution
            .as_ref()
            .unwrap()
            .status,
        ResolutionStatus::Unknown
    );
    assert_eq!(
        batch.memory_requests[1]
            .request
            .deadline
            .as_ref()
            .unwrap()
            .quote
            .text,
        "一阵子"
    );
}

#[test]
fn spaced_integer_literals_keep_their_sign_and_value() {
    let source = "我记录的值是 - 12。";
    let mut c = candidate(source);
    c["relation"] = json!("count");
    c["value"] = json!({"kind":"integer","integer":-12,"quote":{"text":"-12"}});
    assert_eq!(
        run(source, vec![c.clone()], vec![]).unwrap().candidates[0].status,
        Status::Ok
    );
    c["value"]["integer"] = json!(12);
    rejected(source, c, "integer_quote_mismatch");
}
