//! 召回①：生产 R1 纯接口；无模型、数据库、样本与旧召回入口。
use hippocampus::contract::{
    Precision, R1CurrentEvidence, R1Need, R1Output, R1Subject, ResolutionStatus,
};
use hippocampus::r1::{self, Context, Error, Plan};
use hippocampus::w2::Relation;
use hippocampus::{EntityId, MemoryType, TaskId};
use serde_json::{Value as Json, json};

fn output() -> Json {
    json!({"need":"judge", "current_evidence":"assertion_present", "type":"single",
        "subject":{"entity_id":"self"}, "relation":"讨论的安排", "conditions":[],
        "time":null, "plan":{"op":"lookup", "filter":{"relation":null,"time":null}, "unit":null}})
}

fn time(quote: &str, interp: Json) -> Json {
    json!({"role":"event", "quote":{"text":quote}, "interp":interp,"anchor":"said_on"})
}

fn using_context<T>(query: &str, history: &[String], run: impl FnOnce(&Context<'_>) -> T) -> T {
    let entities = ["self", "child", "peer"].map(EntityId::from);
    let relations = [Relation {
        id: "fact".into(),
        memory_types: vec![MemoryType::Fact],
    }];
    let task = TaskId::from("trusted-task");
    run(&Context {
        query,
        context: history,
        entities: &entities,
        relations: &relations,
        subject_ids: None,
        task_id: Some(&task),
        said_on: "2026-04-15",
        utc_offset_seconds: 28_800,
        supported_units: &["day", "week", "month", "year"],
    })
}

fn parse(raw: Json, ctx: &Context<'_>) -> Plan {
    match r1::decode(&raw.to_string()) {
        Ok(output) => r1::validate(output, ctx),
        Err(Error::InvalidOutput) => r1::not_understood(ctx),
        Err(error) => panic!("unexpected decode error: {error}"),
    }
    .unwrap()
}

fn run(query: &str, history: &[String], raw: Json) -> Plan {
    using_context(query, history, |ctx| parse(raw, ctx))
}

fn degraded(plan: &Plan) {
    assert_eq!(plan.output, None);
    assert_eq!(plan.time_resolution, None);
    assert_eq!(plan.filter_time_resolution, None);
    assert_eq!(plan.need(), R1Need::Judge);
    assert_eq!(plan.current_evidence(), R1CurrentEvidence::Unknown);
    assert!(plan.review_current_message());
    assert_eq!(plan.degraded_reasons(), ["r1_not_understood"]);
    assert!(!plan.needs_clarification());
}

// 要求 1：严格类型、必填字段与非空字符串，已解码输入也不能绕过。
#[test]
fn schema_accepts_all_enums_and_required_nulls() {
    for (field, values) in [
        ("need", vec!["context_only", "judge"]),
        (
            "current_evidence",
            vec!["assertion_present", "query_only", "unknown"],
        ),
        (
            "type",
            vec!["single", "aggregate", "comparison", "temporal", "open"],
        ),
    ] {
        for value in values {
            let mut raw = output();
            raw[field] = json!(value);
            let plan = run("安排已经确定。", &[], raw.clone());
            assert_eq!(serde_json::to_value(plan.output.unwrap()).unwrap(), raw);
        }
    }
    for op in ["lookup", "list", "count", "sum", "compare", "elapsed"] {
        let mut raw = output();
        raw["plan"]["op"] = json!(op);
        assert!(run("安排已经确定。", &[], raw).output.is_some());
    }
}

#[test]
fn schema_rejects_unknown_enums_fields_and_missing_fields() {
    for path in ["/need", "/current_evidence", "/type", "/plan/op"] {
        let mut raw = output();
        *raw.pointer_mut(path).unwrap() = json!("unsupported");
        assert_eq!(r1::decode(&raw.to_string()), Err(Error::InvalidOutput));
        degraded(&run("原始陈述。", &[], raw));
    }
    for (parent, field) in [
        ("", "need"),
        ("", "current_evidence"),
        ("", "type"),
        ("", "subject"),
        ("", "relation"),
        ("", "conditions"),
        ("", "time"),
        ("", "plan"),
        ("/plan", "op"),
        ("/plan", "filter"),
        ("/plan", "unit"),
        ("/plan/filter", "relation"),
        ("/plan/filter", "time"),
    ] {
        let mut raw = output();
        raw.pointer_mut(parent)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert_eq!(r1::decode(&raw.to_string()), Err(Error::InvalidOutput));
    }
    for path in ["", "/plan", "/plan/filter", "/subject"] {
        let mut raw = output();
        raw.pointer_mut(path).unwrap()["unexpected"] = json!(true);
        assert_eq!(r1::decode(&raw.to_string()), Err(Error::InvalidOutput));
    }
    for value in [json!(null), json!(1), json!(true)] {
        let mut raw = output();
        raw["need"] = value;
        assert_eq!(r1::decode(&raw.to_string()), Err(Error::InvalidOutput));
    }
    let mut raw = output();
    raw["conditions"] = json!([{"quote":{"text":"原始"},"resolved":"true"}]);
    assert_eq!(r1::decode(&raw.to_string()), Err(Error::InvalidOutput));
    assert_eq!(r1::decode("not JSON"), Err(Error::InvalidOutput));
}

#[test]
fn decoded_blank_fields_degrade() {
    using_context("原始陈述。", &[], |ctx| {
        for path in ["/relation", "/subject/entity_id", "/plan/unit"] {
            let mut raw = output();
            *raw.pointer_mut(path).unwrap() = json!("\n\t ");
            let decoded: R1Output = serde_json::from_value(raw.clone()).unwrap();
            degraded(&r1::validate(decoded, ctx).unwrap());
            assert_eq!(r1::decode(&raw.to_string()), Err(Error::InvalidOutput));
        }
    });
}

// 要求 2：实体、原文指代与歧义主体；目录存在不证明指代含义。
#[test]
fn entity_subject_must_be_in_catalog() {
    for id in ["self", "child", "peer"] {
        let mut raw = output();
        raw["subject"] = json!({"entity_id":id});
        let plan = run("安排已经确定。", &[], raw.clone());
        assert!(!plan.needs_clarification());
        assert_eq!(serde_json::to_value(plan.output.unwrap()).unwrap(), raw);
    }
}

#[test]
fn unknown_entity_degrades_without_binding_unique_catalog() {
    using_context("他会参加。", &[], |ctx| {
        let ctx = Context {
            entities: &ctx.entities[..1],
            ..*ctx
        };
        let mut raw = output();
        raw["subject"] = json!({"entity_id":"invented-person"});
        degraded(&parse(raw, &ctx));
    });
}

#[test]
fn unresolved_subject_preserves_current_quote_and_requires_clarification() {
    let mut raw = output();
    raw["subject"] = json!({"unresolved":{"text":"他"}});
    let plan = run("他会参加。", &[], raw);
    assert!(plan.needs_clarification());
    assert_eq!(plan.degraded_reasons(), [] as [&str; 0]);
    assert!(
        matches!(plan.output.unwrap().subject, R1Subject::Unresolved(s) if s.unresolved.text == "他")
    );
}

#[test]
fn unresolved_subject_can_quote_context_without_selecting_entity() {
    let history = ["那位同事会参加。".into()];
    using_context("什么时候开始？", &history, |ctx| {
        let ctx = Context {
            entities: &ctx.entities[..1],
            ..*ctx
        };
        let mut raw = output();
        raw["subject"] = json!({"unresolved":{"text":"那位同事"}});
        let plan = parse(raw, &ctx);
        assert!(plan.needs_clarification());
        assert!(matches!(
            plan.output.unwrap().subject,
            R1Subject::Unresolved(_)
        ));
    });
}

#[test]
fn unresolved_subject_missing_or_noncontiguous_quote_degrades() {
    for quote in ["某个人", "他会参加", "now"] {
        let mut raw = output();
        raw["subject"] = json!({"unresolved":{"text":quote}});
        degraded(&run("他知道。I know.", &["会参加。".into()], raw));
    }
}

#[test]
fn ambiguous_subject_keeps_distinct_catalog_members() {
    let mut raw = output();
    raw["subject"] = json!({"ambiguous":["child","peer"]});
    let plan = run("他会参加。", &[], raw);
    assert!(plan.needs_clarification());
    assert!(matches!(plan.output.unwrap().subject,
        R1Subject::Ambiguous(s) if s.ambiguous == ["child", "peer"]));
}

#[test]
fn ambiguous_subject_rejects_single_duplicate_or_foreign_members() {
    for ids in [
        json!([]),
        json!(["child"]),
        json!(["child", "child"]),
        json!(["child", "foreign"]),
    ] {
        let mut raw = output();
        raw["subject"] = json!({"ambiguous":ids});
        degraded(&run("他会参加。", &[], raw));
    }
    let mut raw = output();
    raw["subject"] = json!({"entity_id":"self", "ambiguous":["child","peer"]});
    assert_eq!(r1::decode(&raw.to_string()), Err(Error::InvalidOutput));
}

// 要求 3：条件只保留为提示；定位每条消息，不拼接上下文。
#[test]
fn conditions_preserve_original_spelling_and_remain_hints() {
    let query = "I’ll join only\n outdoors.";
    let history = ["和家人一起时需要确认。".into()];
    let mut raw = output();
    raw["relation"] = json!("一个不在目录里的自然语言问题描述");
    raw["conditions"] = json!([
        {"quote":{"text":"I'll join only outdoors"},"resolved":false},
        {"quote":{"text":"和家人一起时"},"resolved":true}
    ]);
    let plan = run(query, &history, raw);
    let output = plan.output.unwrap();
    assert_eq!(output.conditions[0].quote.text, "I’ll join only\n outdoors");
    assert!(!output.conditions[0].resolved);
    assert_eq!(output.conditions[1].quote.text, "和家人一起时");
    assert_eq!(output.plan.filter.relation, None);
    assert_eq!(output.plan.filter.time, None);
}

#[test]
fn missing_or_joined_condition_quotes_degrade() {
    for quote in ["室内", "户外和家人", "户外时再确认", "now"] {
        let mut raw = output();
        raw["conditions"] = json!([{"quote":{"text":quote},"resolved":false}]);
        degraded(&run(
            "户外时需要先预约，再确认。I know.",
            &["和家人一起。".into()],
            raw,
        ));
    }
}

// 要求 4：两个时间字段独立核验；换算仅使用可信 said_on 与时区。
#[test]
fn times_resolve_current_and_filter_using_said_on_and_timezone() {
    let mut raw = output();
    raw["time"] = time("昨天", json!({"kind":"relative","unit":"day","offset":-1}));
    raw["plan"]["filter"]["time"] = time(
        "4月1日至4月14日",
        json!({"kind":"range","start":"2026-04-01","end_inclusive":"2026-04-14"}),
    );
    raw["plan"]["filter"]["time"]["role"] = json!("validity");
    let query = "昨天确认了4月1日至4月14日的安排。";
    let plan = run(query, &[], raw.clone());
    let stamp = |day| {
        time::Date::from_calendar_date(2026, time::Month::April, day)
            .unwrap()
            .midnight()
            .assume_offset(time::UtcOffset::from_whole_seconds(28_800).unwrap())
            .unix_timestamp()
    };
    let resolution = plan.time_resolution.unwrap();
    assert_eq!(resolution.status, ResolutionStatus::Resolved);
    assert_eq!(resolution.precision, Precision::Day);
    assert_eq!(resolution.start, Some(stamp(14)));
    assert_eq!(resolution.end_exclusive, Some(stamp(15)));
    let filter = plan.filter_time_resolution.unwrap();
    assert_eq!(filter.start, Some(stamp(1)));
    assert_eq!(filter.end_exclusive, Some(stamp(15)));
    assert_eq!(serde_json::to_value(plan.output.unwrap()).unwrap(), raw);
    using_context(query, &[], |ctx| {
        let ctx = Context {
            utc_offset_seconds: 0,
            ..*ctx
        };
        assert_eq!(
            parse(raw, &ctx).time_resolution.unwrap().start,
            Some(stamp(14) + 28_800)
        );
    });
}

#[test]
fn unknown_anchored_recurring_times_remain_unknown() {
    for (quote, interp) in [
        ("以前", json!({"kind":"unknown"})),
        (
            "毕业之后",
            json!({"kind":"anchored","anchor_quote":{"text":"毕业"},"unit":"year","offset":2}),
        ),
        ("每周一", json!({"kind":"recurring"})),
    ] {
        let mut raw = output();
        raw["time"] = time(quote, interp);
        raw["plan"]["filter"]["time"] = raw["time"].clone();
        let plan = run(quote, &[], raw.clone());
        for resolution in [plan.time_resolution, plan.filter_time_resolution] {
            let resolution = resolution.unwrap();
            assert_eq!(resolution.status, ResolutionStatus::Unknown);
            assert_eq!(resolution.precision, Precision::Unknown);
            assert_eq!(resolution.start, None);
            assert_eq!(resolution.end_exclusive, None);
        }
        assert_eq!(serde_json::to_value(plan.output.unwrap()).unwrap(), raw);
    }
}

#[test]
fn invalid_calendar_ranges_and_overflows_degrade_for_both_time_fields() {
    for interp in [
        json!({"kind":"date","date":"2026-02-30"}),
        json!({"kind":"range","start":"2026-04-15","end_inclusive":"2026-04-14"}),
        json!({"kind":"range","start":"2026-04-01","end_inclusive":"2026-04-31"}),
        json!({"kind":"relative","unit":"week","offset":i64::MAX}),
        json!({"kind":"month","month":13,"year_offset":0}),
        json!({"kind":"month","month":1,"year_offset":i64::MAX}),
    ] {
        for path in ["/time", "/plan/filter/time"] {
            let mut raw = output();
            *raw.pointer_mut(path).unwrap() = time("那天", interp.clone());
            degraded(&run("那天的安排。", &[], raw));
        }
    }
}

#[test]
fn times_require_real_quotes_and_anchors() {
    for value in [
        time("不在原文", json!({"kind":"unknown"})),
        time(
            "以前",
            json!({"kind":"anchored","anchor_quote":{"text":"不在原文"},"unit":"year","offset":1}),
        ),
    ] {
        for path in ["/time", "/plan/filter/time"] {
            let mut raw = output();
            *raw.pointer_mut(path).unwrap() = value.clone();
            degraded(&run("以前的安排。", &[], raw));
        }
    }
    let mut raw = output();
    raw["time"] = time("昨天", json!({"kind":"relative","unit":"day","offset":-1}));
    assert!(
        run("是哪天？", &["昨天确认。".into()], raw)
            .output
            .is_some()
    );
}

#[test]
fn literal_date_or_precision_mismatch_degrades() {
    for (quote, interp) in [
        ("2026年2月30日", json!({"kind":"date","date":"2026-02-28"})),
        ("2026年4月", json!({"kind":"date","date":"2026-04-15"})),
        ("2026年4月15日", json!({"kind":"date","date":"2026-04-16"})),
    ] {
        let mut raw = output();
        raw["time"] = time(quote, interp);
        degraded(&run(quote, &[], raw));
    }
}

#[test]
fn retention_role_rejected_in_both_time_fields() {
    for path in ["/time", "/plan/filter/time"] {
        let mut raw = output();
        let mut value = time("明天", json!({"kind":"relative","unit":"day","offset":1}));
        value["role"] = json!("retention");
        *raw.pointer_mut(path).unwrap() = value;
        degraded(&run("明天的安排。", &[], raw));
    }
}

// 要求 5：目录过滤、代码单位与不可放宽的可信主体/任务。
#[test]
fn catalog_relation_and_supported_unit_are_accepted() {
    for unit in [
        json!(null),
        json!("day"),
        json!("week"),
        json!("month"),
        json!("year"),
    ] {
        let mut raw = output();
        raw["plan"]["op"] = json!("elapsed");
        raw["plan"]["unit"] = unit;
        raw["plan"]["filter"]["relation"] = json!("fact");
        assert!(run("过去多久？", &[], raw).output.is_some());
    }
}

#[test]
fn unknown_filter_relation_or_unit_degrades() {
    for path in ["/plan/filter/relation", "/plan/unit"] {
        let mut raw = output();
        *raw.pointer_mut(path).unwrap() = json!("not-supported");
        degraded(&run("过去多久？", &[], raw));
    }
}

#[test]
fn trusted_subject_and_task_scope_is_preserved_for_narrowing() {
    using_context("安排已经确定。", &[], |ctx| {
        let ctx = Context {
            subject_ids: Some(&ctx.entities[..2]),
            ..*ctx
        };
        let mut raw = output();
        raw["plan"]["filter"]["relation"] = json!("fact");
        let plan = parse(raw, &ctx);
        assert!(plan.output.is_some());
        assert_eq!(plan.subject_ids, Some(ctx.entities[..2].to_vec()));
        assert_eq!(plan.task_id.as_ref(), ctx.task_id);
        let ctx = Context {
            task_id: None,
            ..ctx
        };
        let plan = parse(output(), &ctx);
        assert!(plan.output.is_some());
        assert_eq!(plan.task_id, None);
    });
}

#[test]
fn model_cannot_select_outside_trusted_subject_scope() {
    using_context("他会参加。", &[], |ctx| {
        let ctx = Context {
            subject_ids: Some(&ctx.entities[1..2]),
            ..*ctx
        };
        for subject in [
            json!({"entity_id":"self"}),
            json!({"ambiguous":["child","peer"]}),
        ] {
            let mut raw = output();
            raw["subject"] = subject;
            let plan = parse(raw, &ctx);
            degraded(&plan);
            assert_eq!(plan.subject_ids, Some(ctx.entities[1..2].to_vec()));
            assert_eq!(plan.task_id.as_ref(), ctx.task_id);
        }
    });
}

#[test]
fn empty_trusted_subject_scope_never_becomes_all_entities() {
    using_context("安排已经确定。", &[], |ctx| {
        let ctx = Context {
            subject_ids: Some(&[]),
            ..*ctx
        };
        let plan = parse(output(), &ctx);
        degraded(&plan);
        assert_eq!(plan.subject_ids, Some(vec![]));
    });
}

#[test]
fn schema_cannot_add_subject_task_or_owner_filters() {
    for parent in ["", "/plan/filter"] {
        for field in ["subject_ids", "task_id", "owner"] {
            let mut raw = output();
            raw.pointer_mut(parent).unwrap()[field] = json!("foreign");
            degraded(&run("安排已经确定。", &[], raw));
        }
    }
}

// 要求 6：任一失败整份降级，保留可能的陈述以及原始上下文。
#[test]
fn any_failure_clears_model_filters_and_preserves_possible_assertions() {
    let query = "我昨天确认了安排，需要再核对吗？";
    let history = ["之前的安排尚未确定。".into()];
    using_context(query, &history, |ctx| {
        let ctx = Context {
            subject_ids: Some(&ctx.entities[..1]),
            ..*ctx
        };
        let mut raw = output();
        raw["need"] = json!("context_only");
        raw["current_evidence"] = json!("query_only");
        raw["time"] = time("昨天", json!({"kind":"relative","unit":"day","offset":-1}));
        raw["plan"]["filter"]["relation"] = json!("fact");
        assert!(parse(raw.clone(), &ctx).output.is_some());
        raw["plan"]["filter"]["time"] = time("不存在的时间", json!({"kind":"unknown"}));
        let plan = parse(raw, &ctx);
        degraded(&plan);
        assert_eq!(plan.query, query);
        assert_eq!(plan.context, history);
        assert_eq!(plan.subject_ids, Some(ctx.entities[..1].to_vec()));
        assert_eq!(plan.task_id.as_ref(), ctx.task_id);
    });
}

#[test]
fn decode_failure_uses_same_broad_input() {
    using_context("我已确认安排。", &["先前待定。".into()], |ctx| {
        assert_eq!(r1::decode("{incomplete"), Err(Error::InvalidOutput));
        let plan = r1::not_understood(ctx).unwrap();
        degraded(&plan);
        assert_eq!(plan.query, ctx.query);
        assert_eq!(plan.context, ctx.context);
        assert_eq!(plan.task_id.as_ref(), ctx.task_id);
    });
}

// 要求 7：只标送审资格，不在代码中把查询文本认证为事实。
#[test]
fn query_only_does_not_mark_current_message_for_review() {
    let mut raw = output();
    raw["current_evidence"] = json!("query_only");
    let plan = run("过去多久？", &[], raw);
    assert_eq!(plan.current_evidence(), R1CurrentEvidence::QueryOnly);
    assert!(!plan.review_current_message());
    assert_eq!(plan.query, "过去多久？");
    assert!(plan.output.is_some());
}

#[test]
fn assertion_and_unknown_current_messages_remain_for_review() {
    for value in ["assertion_present", "unknown"] {
        let mut raw = output();
        raw["current_evidence"] = json!(value);
        assert!(run("我已确认安排，还需要核对吗？", &[], raw).review_current_message());
    }
}

#[test]
fn invalid_trusted_context_is_reported_without_guessing() {
    using_context("安排已经确定。", &[], |ctx| {
        let task = TaskId::from(" ");
        let blank_entities = [EntityId::from("")];
        let outside_subjects = [EntityId::from("foreign")];
        let blank_relations = [Relation {
            id: " ".into(),
            memory_types: vec![],
        }];
        for (ctx, reason) in [
            (
                Context {
                    said_on: "2026-02-30",
                    ..*ctx
                },
                "said_on",
            ),
            (
                Context {
                    utc_offset_seconds: i32::MAX,
                    ..*ctx
                },
                "timezone",
            ),
            (
                Context {
                    task_id: Some(&task),
                    ..*ctx
                },
                "task_id",
            ),
            (
                Context {
                    entities: &blank_entities,
                    ..*ctx
                },
                "entities",
            ),
            (
                Context {
                    subject_ids: Some(&outside_subjects),
                    ..*ctx
                },
                "entities",
            ),
            (
                Context {
                    relations: &blank_relations,
                    ..*ctx
                },
                "relations",
            ),
            (
                Context {
                    supported_units: &[" "],
                    ..*ctx
                },
                "units",
            ),
        ] {
            let decoded = r1::decode(&output().to_string()).unwrap();
            assert_eq!(
                r1::validate(decoded, &ctx),
                Err(Error::InvalidContext(reason))
            );
            assert_eq!(r1::not_understood(&ctx), Err(Error::InvalidContext(reason)));
        }
    });
}
