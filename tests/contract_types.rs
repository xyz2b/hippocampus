//! 契约类型的形状检查：已复核的 W4 样本输入按契约类型解码并原样回写；常见的形状错误在
//! 解码层被拒（逐字、日历等机械校验属于 W2，不在这里）。

use hippocampus::contract::*;
use serde_json::{Value as Json, json};

fn samples() -> Vec<(String, Json)> {
    let mut out: Vec<(String, Json)> = std::fs::read_dir("tests/eval/w4/samples")
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .map(|path| {
            let text = std::fs::read_to_string(&path).unwrap();
            (
                path.display().to_string(),
                serde_json::from_str(&text).unwrap(),
            )
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[test]
fn reviewed_w4_inputs_decode_and_round_trip() {
    let samples = samples();
    assert_eq!(samples.len(), 23);
    let mut formats = std::collections::BTreeSet::new();
    for (name, sample) in samples {
        let input: W4Input = serde_json::from_value(sample["input"].clone())
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(
            serde_json::to_value(&input).unwrap(),
            sample["input"],
            "{name} round trip"
        );
        for record in &input.records {
            formats.insert(match record {
                RecordView2::Current(_) => "current",
                RecordView2::Legacy(_) => "legacy",
            });
        }
    }
    assert_eq!(formats.len(), 2, "both record formats are exercised");
}

fn candidate() -> Json {
    json!({
        "kind": "assertion",
        "speech": "user_statement",
        "subject": {"entity_id": "self"},
        "relation": "fact",
        "memory_type": "fact",
        "value": {"kind": "text", "text": "我画速写时固定使用炭笔。", "quote": {"text": "我画速写时固定使用炭笔。"}},
        "sentence": {"text": "我画速写时固定使用炭笔。"},
        "polarity": "affirm",
        "strength": null,
        "uncertainty": null,
        "conditions": [{"quote": {"text": "画速写时"}, "resolved": true}],
        "times": [{"role": "event", "quote": {"text": "去年五月"},
                   "interp": {"kind": "month", "year_offset": -1, "month": 5}, "anchor": "said_on"}]
    })
}

fn decision() -> Json {
    json!({"candidate": 0, "status": "pending", "action": null, "targets": [],
           "resolves_relations": [], "basis": [], "effective": null,
           "relation_scope": null, "pending_reason": "target_unknown"})
}

#[test]
fn the_contract_example_candidate_decodes() {
    let decoded: Candidate = serde_json::from_value(candidate()).unwrap();
    assert_eq!(serde_json::to_value(&decoded).unwrap(), candidate());
    let unresolved = json!({"unresolved": {"text": "他"}});
    assert_eq!(
        serde_json::from_value::<Subject>(unresolved).unwrap(),
        Subject::Unresolved(UnresolvedSubject {
            unresolved: Quote { text: "他".into() }
        })
    );
    let decision: W4Decision = serde_json::from_value(decision()).unwrap();
    assert_eq!(decision.action, None);
    let mut scoped = self::decision();
    scoped["relation_scope"] =
        json!({"conditions": [{"of": "candidate", "index": 0}], "times": [{"of": 17, "index": 1}]});
    let decoded: W4Decision = serde_json::from_value(scoped.clone()).unwrap();
    let scope = decoded.relation_scope.clone().unwrap();
    assert_eq!(scope.conditions[0].of, QualifierOwner::Candidate);
    assert_eq!(
        scope.times[0].of,
        QualifierOwner::Record(hippocampus::MemoryId(17))
    );
    assert_eq!(serde_json::to_value(&decoded).unwrap(), scoped);
}

#[test]
fn shape_errors_are_rejected_at_decoding() {
    let mut cases: Vec<(&str, Json, fn(Json) -> bool)> = Vec::new();
    fn as_candidate(v: Json) -> bool {
        serde_json::from_value::<Candidate>(v).is_err()
    }
    fn as_decision(v: Json) -> bool {
        serde_json::from_value::<W4Decision>(v).is_err()
    }
    let mut c = candidate();
    c["tags"] = json!([]);
    cases.push(("removed field tags", c, as_candidate));
    let mut c = candidate();
    c.as_object_mut().unwrap().remove("uncertainty");
    cases.push(("missing nullable field", c, as_candidate));
    let mut c = candidate();
    c["polarity"] = json!("positive");
    cases.push(("old polarity", c, as_candidate));
    let mut c = candidate();
    c["times"][0]["interp"] = json!({"kind": "month", "year_offset": -1});
    cases.push(("interp missing field", c, as_candidate));
    let mut c = candidate();
    c["times"][0]["interp"]["day"] = json!(3);
    cases.push(("interp extra field", c, as_candidate));
    let mut c = candidate();
    c["conditions"][0]["quote"]["resolved"] = json!(true);
    cases.push(("resolved inside Quote", c, as_candidate));
    let mut c = candidate();
    c["subject"] = json!({"entity_id": "self", "unresolved": {"text": "他"}});
    cases.push(("subject with both forms", c, as_candidate));
    let mut d = decision();
    d["relation_scope"] = json!({"conditions": [{"of": "target", "index": 0}], "times": []});
    cases.push((
        "scope owner that is neither candidate nor id",
        d,
        as_decision,
    ));
    let mut d = decision();
    d.as_object_mut().unwrap().remove("relation_scope");
    cases.push(("relation_scope missing", d, as_decision));
    let mut d = decision();
    d["action"] = json!("null");
    cases.push(("action string null", d, as_decision));
    let mut d = decision();
    d.as_object_mut().unwrap().remove("action");
    cases.push(("action missing", d, as_decision));
    let mut d = decision();
    d["basis"] = json!([{"source": {"kind": "stored"}, "quote": {"text": "x"}}]);
    cases.push(("stored source without id", d, as_decision));
    let mut d = decision();
    d["basis"] =
        json!([{"source": {"kind": "current", "source_id": "s-1"}, "quote": {"text": "x"}}]);
    cases.push(("current source with id", d, as_decision));
    fn as_request(v: Json) -> bool {
        serde_json::from_value::<MemoryRequest>(v).is_err()
    }
    cases.push((
        "request target without quote",
        json!({"kind": "remember", "quote": {"text": "记住"},
               "target": {"object": "facts", "candidate_indices": [0]}, "deadline": null}),
        as_request,
    ));
    let mut c = candidate();
    c["times"][0]["interp"] = json!({"kind": "unknown", "month": 5});
    cases.push(("unknown interp with fields", c, as_candidate));
    for (label, value, rejected) in cases {
        assert!(rejected(value), "{label} must be rejected");
    }
}

fn contract(candidate: Json) -> ContractCandidate {
    serde_json::from_value(json!({
        "candidate": candidate,
        "source": {"kind": "stored", "source_id": "s-1"},
        "said_on": "2026-09-28",
        "time_resolutions": []
    }))
    .unwrap()
}

fn context() -> ProjectionContext {
    ProjectionContext {
        owner_id: hippocampus::UserId::from("alice"),
        session_id: hippocampus::SessionId::from("chat"),
        turn_id: hippocampus::TurnId::from("s-1"),
        evidence_id: hippocampus::EvidenceId::from("s-1"),
        observed_at: hippocampus::Timestamp(100),
        task_id: None,
        explicit_memory_request: false,
    }
}

fn plain() -> Json {
    let mut c = candidate();
    c["conditions"] = json!([]);
    c["times"] = json!([]);
    c
}

#[test]
fn projection_keeps_what_memory_can_express_and_flags_the_rest() {
    let projected = contract(plain()).project(&context()).unwrap();
    assert!(projected.lossless);
    let memory = projected.candidate;
    assert_eq!(
        memory.value,
        hippocampus::MemoryValue::Text("我画速写时固定使用炭笔。".into())
    );
    assert_eq!(memory.polarity, None);
    assert_eq!(memory.evidence[0].quote, "我画速写时固定使用炭笔。");
    assert!(memory.scope.conditions.is_empty());

    // 条件、不确定表达、事件时间：仍可投影，但不是无损的。
    for (label, edit) in [
        (
            "condition",
            json!({"conditions": [{"quote": {"text": "画速写时"}, "resolved": true}]}),
        ),
        (
            "uncertainty",
            json!({"uncertainty": {"quote": {"text": "可能"}, "resolved": true}}),
        ),
        (
            "event time",
            json!({"times": [{"role": "event", "quote": {"text": "去年五月"},
            "interp": {"kind": "month", "year_offset": -1, "month": 5}, "anchor": "said_on"}]}),
        ),
    ] {
        let mut c = plain();
        for (key, value) in edit.as_object().unwrap() {
            c[key] = value.clone();
        }
        let projected = contract(c).project(&context()).unwrap();
        assert!(!projected.lossless, "{label}");
    }

    // 单个已换算的有效期写入 valid_from / valid_to。
    let mut c = plain();
    c["times"] = json!([{"role": "validity", "quote": {"text": "4月1日至4月14日"},
        "interp": {"kind": "range", "start": "2026-04-01", "end_inclusive": "2026-04-14"},
        "anchor": "said_on"}]);
    let mut with_validity = contract(c);
    with_validity.time_resolutions = vec![TimeResolution {
        status: ResolutionStatus::Resolved,
        start: Some(1_775_000_000),
        end_exclusive: Some(1_776_200_000),
        precision: Precision::Day,
    }];
    let projected = with_validity.project(&context()).unwrap();
    assert!(projected.lossless);
    assert_eq!(
        projected.candidate.temporal.valid_to.map(|v| v.at),
        Some(hippocampus::Timestamp(1_776_200_000))
    );
    // 保存期限不写入有效期或到期时间。
    with_validity.candidate.times[0].role = TimeRole::Retention;
    let projected = with_validity.project(&context()).unwrap();
    assert!(!projected.lossless);
    assert_eq!(projected.candidate.temporal.valid_from, None);
    assert_eq!(projected.candidate.temporal.expires_at, None);
}

#[test]
fn projection_refuses_what_memory_would_misstate() {
    let cases: Vec<(&str, Json, Unprojectable)> = vec![
        (
            "unresolved subject",
            json!({"subject": {"unresolved": {"text": "他"}}}),
            Unprojectable::UnresolvedSubject,
        ),
        (
            "reported speech",
            json!({"speech": "reported"}),
            Unprojectable::NotUserStatement(Speech::Reported),
        ),
        (
            "negated fact",
            json!({"polarity": "negate"}),
            Unprojectable::Polarity(Polarity::Negate),
        ),
        (
            "unknown polarity",
            json!({"polarity": "unknown", "memory_type": "preference"}),
            Unprojectable::Polarity(Polarity::Unknown),
        ),
        (
            "unknown strength",
            json!({"memory_type": "preference", "strength": "unknown"}),
            Unprojectable::UnknownStrength,
        ),
        (
            "assertion without value",
            json!({"value": null}),
            Unprojectable::MissingValue,
        ),
    ];
    for (label, edit, expected) in cases {
        let mut c = plain();
        for (key, value) in edit.as_object().unwrap() {
            c[key] = value.clone();
        }
        assert_eq!(
            contract(c).project(&context()).unwrap_err(),
            expected,
            "{label}"
        );
    }
    // 偏好的否定是方向，不是否定事实；撤回不需要新值。
    let mut c = plain();
    c["memory_type"] = json!("preference");
    c["polarity"] = json!("negate");
    let projected = contract(c).project(&context()).unwrap();
    assert_eq!(
        projected.candidate.polarity,
        Some(hippocampus::Polarity::Negative)
    );
    let mut c = plain();
    c["kind"] = json!("retraction");
    c["value"] = json!(null);
    c["polarity"] = json!("negate");
    let projected = contract(c).project(&context()).unwrap();
    assert!(matches!(
        projected.candidate.intent,
        hippocampus::CandidateIntent::Retract { target: None }
    ));
}

#[test]
fn quotes_are_located_as_continuous_source_text() {
    let source = "I don’t  know.  Call me now, 我说 “画速写时 固定用炭笔”。";
    let found = |quote: &str| locate_quote(source, quote).map(|range| &source[range]);
    // 空白与引号排版的差异不算不同；返回的是原文写法。
    assert_eq!(found("I don't know."), Some("I don’t  know."));
    assert_eq!(
        found("\"画速写时固定用炭笔\""),
        Some("“画速写时 固定用炭笔”")
    );
    assert_eq!(found("don''t"), Some("don’t"));
    // 英文不能落在词的中间：know 里没有独立的 now，后面那个才是。
    assert_eq!(found("now"), Some("now"));
    assert_eq!(
        locate_quote(source, "now").unwrap().start,
        source.find("now,").unwrap()
    );
    assert_eq!(locate_quote("I know", "now"), None);
    // 缩合词不能截掉否定后缀；独立的引号照常匹配。
    assert_eq!(locate_quote("I can’t swim.", "I can"), None);
    assert_eq!(locate_quote("I can't swim.", "can"), None);
    assert_eq!(locate_quote("I can’t swim.", "t swim"), None);
    assert_eq!(locate_quote("I can''t swim.", "I can"), None);
    assert_eq!(locate_quote("I can’t swim.", "I can'"), None);
    assert_eq!(locate_quote("I can’t swim.", "'t swim."), None);
    let possessive = "The dogs' bones.";
    assert_eq!(
        locate_quote(possessive, "dogs").map(|range| &possessive[range]),
        Some("dogs")
    );
    let quoted = "He said ‘now’ twice.";
    assert_eq!(
        locate_quote(quoted, "now").map(|range| &quoted[range]),
        Some("now")
    );
    // 不相邻的片段不能拼成一个；标点与大小写的差异不放过。
    assert_eq!(found("know. now"), None);
    assert_eq!(found("画速写时，固定用炭笔"), None);
    assert_eq!(found("i don't know"), None);
    assert_eq!(found("  "), None);
}
