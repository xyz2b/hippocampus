//! 单阶段离线评分；样本标签仍为 draft，未录制与过期录制不冒充模型失败。
#[path = "eval/w3/adapter.rs"]
mod adapter;

#[path = "eval/recall_stage/adapter.rs"]
mod recall_adapter;

#[path = "eval/fixed_inputs/adapter.rs"]
mod fixed_adapter;

use hippocampus::{
    EntityId, MemoryType, ModelBudgetConfig, ModelStage, ReasoningEffort, contract::*,
    structured_request, w2, w4, write_models,
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

// Keep this readable label in sync with tests/eval.rs and DEEPSEEK_MODEL.
const MODEL: &str = "DeepSeek-V4.1-Flash";
const W3_OLD_PROMPT: &str = include_str!("eval/m15/w3-old-91a068a.txt");
const W3_OLD_FINGERPRINT: &str = "fnv1a64:e20f41189ca2159c";
const W3_EFFORT_PROMPT_FINGERPRINT: &str = "fnv1a64:a20cc11e24e4d855";
const W3_TRIAL_CAP: usize = 16;

fn items(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or(&[])
}

// 标量字段的数组是允许集合；结构数组（条件、时间、索引）逐项匹配，顺序不评分。
fn matches(actual: &Value, expected: &Value) -> bool {
    quote_matches(actual, expected, "", "")
}

// U4 仅对引用选择开放：连续原文中的末尾句号/分句逗号，以及条件开头的“我”。
// 不折叠内部标点，不缩短目标指代，不改时间、限定语或专用关系的值。
fn quote_matches(actual: &Value, expected: &Value, field: &str, source: &str) -> bool {
    match (actual, expected) {
        (Value::Array(a), Value::Array(e)) => {
            let mut used = vec![false; a.len()];
            a.len() == e.len()
                && e.iter().all(|e| {
                    if let Some(i) = a
                        .iter()
                        .enumerate()
                        .position(|(i, a)| !used[i] && quote_matches(a, e, field, source))
                    {
                        used[i] = true;
                        true
                    } else {
                        false
                    }
                })
        }
        (_, Value::Array(e)) => e.iter().any(|e| quote_matches(actual, e, field, source)),
        (Value::Object(a), Value::Object(e)) => e.iter().all(|(k, e)| {
            a.get(k).is_some_and(|a| {
                quote_matches(
                    a,
                    e,
                    &if field.is_empty() {
                        k.clone()
                    } else {
                        format!("{field}.{k}")
                    },
                    source,
                )
            })
        }),
        (Value::String(a), Value::String(e))
            if matches!(
                field,
                "sentence"
                    | "conditions"
                    | "conditions.quote"
                    | "quote"
                    | "target_quote"
                    | "fact_value"
            ) =>
        {
            if a == e {
                return true;
            }
            if a.is_empty() || e.is_empty() || !source.contains(a) || !source.contains(e) {
                return false;
            }
            let trim = |s: &str| {
                s.strip_suffix(['。', '.', '，', ','])
                    .unwrap_or(s)
                    .to_owned()
            };
            if a == &trim(e) || e == &trim(a) {
                return true;
            }
            matches!(field, "conditions" | "conditions.quote")
                && (a.strip_prefix('我') == Some(e.as_str())
                    || e.strip_prefix('我') == Some(a.as_str()))
        }
        _ => actual == expected,
    }
}

fn candidate_matches(actual: &Value, expected: &Value, source: &str) -> bool {
    expected.as_object().unwrap().iter().all(|(key, e)| {
        let field = if key == "value" && actual["relation"] == "fact" {
            "fact_value"
        } else {
            key
        };
        let context = if key == "conditions" {
            actual["sentence"].as_str().unwrap_or("")
        } else {
            source
        };
        quote_matches(&actual[key], e, field, context)
    })
}

fn category(field: &str) -> &'static str {
    if field == "decode"
        || field.contains("validate")
        || field.starts_with("w2.")
        || field.ends_with(".identity")
        || field == "assembly"
    {
        "contract"
    } else if field.ends_with(".sentence")
        || field.ends_with(".quote")
        || field.ends_with(".target_quote")
        || field.ends_with(".basis_sources")
    {
        "reference"
    } else {
        "semantic"
    }
}

fn categories(fields: &[Value]) -> Value {
    let mut result = json!({});
    for category in ["contract", "semantic", "reference"] {
        let rows: Vec<_> = fields
            .iter()
            .filter(|f| f["category"] == category)
            .collect();
        result[category] = json!({"passed":(!rows.is_empty()).then(||rows.iter().all(|f| f["passed"] == true)),
            "failures":rows.iter().filter(|f| f["passed"] != true).map(|f| &f["field"]).collect::<Vec<_>>()});
    }
    result
}

fn check(
    fields: &mut Vec<Value>,
    field: impl Into<String>,
    expected: Value,
    actual: Value,
    passed: bool,
) {
    let field = field.into();
    fields.push(json!({"category":category(&field), "field":field,
        "expected":expected,"actual":actual,"passed":passed}));
}

fn compare(fields: &mut Vec<Value>, field: impl Into<String>, expected: &Value, actual: &Value) {
    check(
        fields,
        field,
        expected.clone(),
        actual.clone(),
        matches(actual, expected),
    );
}

fn normalize_time(value: &Value) -> Value {
    let mut value = value.clone();
    value["quote"] = value["quote"]["text"].clone();
    value.as_object_mut().unwrap().remove("anchor");
    if let Some(anchor) = value["interp"].get_mut("anchor_quote") {
        *anchor = anchor["text"].clone();
    }
    value
}

fn normalize_candidate(value: &Value, expected: &Value) -> Value {
    let mut value = value.clone();
    value["subject"] = if value["subject"].get("entity_id").is_some() {
        value["subject"]["entity_id"].clone()
    } else {
        json!({"unresolved": value["subject"]["unresolved"]["text"]})
    };
    value["sentence"] = value["sentence"]["text"].clone();
    value["value"] = match value["value"]["kind"].as_str() {
        Some("text") => value["value"]["text"].clone(),
        Some("integer") => value["value"]["integer"].clone(),
        Some("boolean") => value["value"]["boolean"].clone(),
        Some("entity") => value["value"]["entity_id"].clone(),
        _ => Value::Null,
    };
    value["uncertainty"] = value["uncertainty"]["quote"]["text"].clone();
    value["conditions"] = Value::Array(
        items(&value["conditions"])
            .iter()
            .map(|q| {
                if items(&expected["conditions"]).iter().any(Value::is_object) {
                    json!({"quote": q["quote"]["text"], "resolved": q["resolved"]})
                } else {
                    q["quote"]["text"].clone()
                }
            })
            .collect(),
    );
    value["times"] = Value::Array(items(&value["times"]).iter().map(normalize_time).collect());
    value
}

fn batch(output: W1Output, input: &Value) -> Result<w2::Batch, w2::Error> {
    let entities: Vec<_> = if input["entities"].is_array() {
        items(&input["entities"])
            .iter()
            .map(|e| EntityId::from(e["id"].as_str().unwrap()))
            .collect()
    } else {
        items(&input["candidates"])
            .iter()
            .filter_map(|c| c["subject"].as_str())
            .map(EntityId::from)
            .collect()
    };
    let names: Vec<String> = if input["relations"].is_array() {
        items(&input["relations"])
            .iter()
            .map(|r| r.as_str().unwrap().to_owned())
            .collect()
    } else {
        items(&input["candidates"])
            .iter()
            .map(|c| c["relation"].as_str().unwrap().to_owned())
            .collect()
    };
    let relations: Vec<_> = names
        .into_iter()
        .map(|id| w2::Relation {
            id,
            memory_types: vec![
                MemoryType::Fact,
                MemoryType::Preference,
                MemoryType::Constraint,
                MemoryType::Episode,
            ],
        })
        .collect();
    w2::validate(
        output,
        &w2::Context {
            text: input["text"].as_str().unwrap(),
            entities: &entities,
            relations: &relations,
            said_on: input["said_on"].as_str(),
            utc_offset_seconds: 28_800,
            task_id: None,
        },
    )
}

fn score_w1(sample: &Value, text: &str, fields: &mut Vec<Value>) -> Result<Value, String> {
    let output = w2::decode(text).map_err(|e| e.to_string())?;
    check(fields, "decode", json!(true), json!(true), true);
    let raw = serde_json::to_value(&output).unwrap();
    let checked = batch(output, &sample["input"]);
    check(
        fields,
        "w2.validate",
        json!(true),
        checked
            .as_ref()
            .err()
            .map(|e| json!(e.to_string()))
            .unwrap_or(json!(true)),
        checked.is_ok(),
    );
    for c in checked
        .as_ref()
        .map(|b| b.candidates.as_slice())
        .unwrap_or(&[])
    {
        check(
            fields,
            format!("w2.candidates[{}]", c.index),
            json!(["ok", "unresolved"]),
            json!({"status": c.status, "reasons": c.reasons}),
            c.status != w2::Status::Rejected,
        );
    }
    let final_output = checked
        .as_ref()
        .map(|b| {
            json!({
                "candidates": b.candidates.iter().map(|c| &c.candidate).collect::<Vec<_>>(),
                "memory_requests": b.memory_requests.iter().map(|r| &r.request).collect::<Vec<_>>()
            })
        })
        .unwrap_or_else(|_| raw.clone());
    let expect = &sample["expect"];
    if sample["m15_review"] == true {
        return Ok(final_output);
    }
    compare(
        fields,
        "candidate_count",
        &expect["candidate_count"],
        &json!(items(&final_output["candidates"]).len()),
    );
    let mut index_map = Vec::new();
    let mut used = std::collections::BTreeSet::new();
    for (i, e) in items(&expect["candidates"]).iter().enumerate() {
        let normalized: Vec<_> = items(&final_output["candidates"])
            .iter()
            .map(|c| normalize_candidate(c, e))
            .collect();
        let matching: Vec<_> = normalized
            .iter()
            .enumerate()
            .filter(|(_, a)| {
                candidate_matches(a, e, sample["input"]["text"].as_str().unwrap())
                    && (sample["id"] != "w1_15_negation"
                        || match a["relation"].as_str() {
                            Some("avoids") => a["polarity"] == "affirm",
                            Some("likes") => a["polarity"] == "negate",
                            _ => false,
                        })
            })
            .map(|(i, _)| i)
            .collect();
        let unique = matching.len() == 1 && used.insert(matching[0]);
        check(
            fields,
            format!("candidates[{i}].match"),
            json!("恰好一个且不复用"),
            json!(matching),
            unique,
        );
        // 完整匹配仍决定整项通过；诊断按唯一身份配对，避免 value 错误连带污染其他字段。
        let identities: Vec<_> = normalized
            .iter()
            .enumerate()
            .filter(|(_, a)| {
                ["subject", "relation", "memory_type"].iter().all(|key| {
                    e.get(*key)
                        .is_none_or(|expected| matches(&a[*key], expected))
                })
            })
            .map(|(i, _)| i)
            .collect();
        let pair = if unique {
            Some(matching[0])
        } else if normalized.len() == 1 {
            Some(0)
        } else if identities.len() == 1 {
            Some(identities[0])
        } else {
            None
        };
        index_map.push(pair);
        let actual = pair.map(|i| &normalized[i]);
        for (key, expected) in e.as_object().unwrap() {
            if let Some(actual) = actual {
                let quote_field = if key == "value" && actual["relation"] == "fact" {
                    "fact_value"
                } else {
                    key
                };
                check(
                    fields,
                    format!("candidates[{i}].{key}"),
                    expected.clone(),
                    actual[key].clone(),
                    quote_matches(
                        &actual[key],
                        expected,
                        quote_field,
                        if key == "conditions" {
                            actual["sentence"].as_str().unwrap_or("")
                        } else {
                            sample["input"]["text"].as_str().unwrap()
                        },
                    ),
                );
            } else {
                check(
                    fields,
                    format!("candidates[{i}].{key}"),
                    expected.clone(),
                    json!(normalized.iter().map(|a| &a[key]).collect::<Vec<_>>()),
                    normalized.iter().any(|a| {
                        quote_matches(
                            &a[key],
                            expected,
                            key,
                            sample["input"]["text"].as_str().unwrap(),
                        )
                    }),
                );
            }
        }
        if sample["id"] == "w1_15_negation" {
            check(
                fields,
                format!("candidates[{i}].relation+polarity"),
                json!(["avoids+affirm", "likes+negate"]),
                actual
                    .map(|a| json!([a["relation"], a["polarity"]]))
                    .unwrap_or(Value::Null),
                actual.is_some_and(|a| match a["relation"].as_str() {
                    Some("avoids") => a["polarity"] == "affirm",
                    Some("likes") => a["polarity"] == "negate",
                    _ => false,
                }),
            );
        }
    }
    compare(
        fields,
        "memory_requests.count",
        &json!(items(&expect["memory_requests"]).len()),
        &json!(items(&final_output["memory_requests"]).len()),
    );
    for (i, e) in items(&expect["memory_requests"]).iter().enumerate() {
        let mut expected = e.clone();
        if let Some(indices) = expected.get_mut("candidate_indices") {
            *indices = Value::Array(
                items(indices)
                    .iter()
                    .map(|index| {
                        index_map
                            .get(index.as_u64().unwrap() as usize)
                            .copied()
                            .flatten()
                            .map(|i| json!(i))
                            .unwrap_or(Value::Null)
                    })
                    .collect(),
            );
        }
        let actual: Vec<_> = items(&final_output["memory_requests"]).iter().map(|r| json!({
            "kind": r["kind"], "quote": r["quote"]["text"], "target_object": r["target"]["object"],
            "candidate_indices": r["target"]["candidate_indices"], "target_quote": r["target"]["quote"]["text"],
            "deadline": if r["deadline"].is_null() { Value::Null } else { normalize_time(&r["deadline"]) }
        })).collect();
        let matching: Vec<_> = actual
            .iter()
            .enumerate()
            .filter(|(_, a)| {
                quote_matches(a, &expected, "", sample["input"]["text"].as_str().unwrap())
            })
            .map(|(i, _)| i)
            .collect();
        check(
            fields,
            format!("memory_requests[{i}].match"),
            json!(1),
            json!(matching.len()),
            matching.len() == 1,
        );
        let row = matching
            .first()
            .map(|&i| &actual[i])
            .or_else(|| (actual.len() == 1).then(|| &actual[0]));
        for (key, e) in expected.as_object().unwrap() {
            if let Some(a) = row {
                check(
                    fields,
                    format!("memory_requests[{i}].{key}"),
                    e.clone(),
                    a[key].clone(),
                    quote_matches(&a[key], e, key, sample["input"]["text"].as_str().unwrap()),
                );
            } else {
                check(
                    fields,
                    format!("memory_requests[{i}].{key}"),
                    e.clone(),
                    json!(actual.iter().map(|a| &a[key]).collect::<Vec<_>>()),
                    actual.iter().any(|a| {
                        quote_matches(&a[key], e, key, sample["input"]["text"].as_str().unwrap())
                    }),
                );
            }
        }
    }
    Ok(serde_json::to_value(checked.ok()).unwrap())
}

fn score_w3(sample: &Value, text: &str, fields: &mut Vec<Value>) -> Result<(), String> {
    let output = W3Output::decode(text).map_err(|e| e.to_string())?;
    check(fields, "decode", json!(true), json!(true), true);
    let batch = batch(
        adapter::expand(&sample["input"]).map_err(|e| e.to_string())?,
        &sample["input"],
    )
    .map_err(|e| e.to_string())?;
    let rejected: Vec<_> = batch
        .candidates
        .iter()
        .filter(|c| c.status == w2::Status::Rejected)
        .collect();
    if !rejected.is_empty() {
        return Err(format!(
            "sample_input_rejected:{}",
            serde_json::to_value(rejected).unwrap()
        ));
    }
    let reviewed = output.clone().validate(
        &batch,
        &json!({"eval_sample": sample["id"]}),
        sample["input"]["text"].as_str().unwrap(),
    );
    check(
        fields,
        "w3.validate",
        json!(true),
        reviewed
            .as_ref()
            .err()
            .map(|e| json!(e.to_string()))
            .unwrap_or(json!(true)),
        reviewed.is_ok(),
    );
    let actual =
        serde_json::to_value(reviewed.as_ref().map(|r| r.output()).unwrap_or(&output)).unwrap();
    for e in items(&sample["expect"]["reviews"]) {
        let matching: Vec<_> = items(&actual["reviews"])
            .iter()
            .filter(|a| a["candidate"] == e["candidate"])
            .collect();
        check(
            fields,
            format!("reviews[{}].identity", e["candidate"]),
            json!(1),
            json!(matching.len()),
            matching.len() == 1,
        );
        let a = matching.first().copied().unwrap_or(&Value::Null);
        for key in ["verdict", "episode_value"] {
            compare(
                fields,
                format!("reviews[{}].{key}", e["candidate"]),
                &e[key],
                &a[key],
            );
        }
        let aspects: Vec<_> = items(&a["problems"])
            .iter()
            .map(|p| p["aspect"].clone())
            .collect();
        check(
            fields,
            format!("reviews[{}].aspects", e["candidate"]),
            e["aspects"].clone(),
            json!(aspects),
            !a.is_null() && items(&e["aspects"]).iter().all(|e| aspects.contains(e)),
        );
    }
    compare(
        fields,
        "coverage.status",
        &sample["expect"]["coverage"]["status"],
        &actual["coverage"]["status"],
    );
    for e in items(&sample["expect"]["coverage"]["missing_contains"]) {
        check(
            fields,
            "coverage.missing_contains",
            e.clone(),
            actual["coverage"]["missing"].clone(),
            items(&actual["coverage"]["missing"])
                .iter()
                .any(|a| a["text"].as_str().unwrap().contains(e.as_str().unwrap())),
        );
    }
    for e in items(&sample["expect"]["request_reviews"]) {
        let matching: Vec<_> = items(&actual["request_reviews"])
            .iter()
            .filter(|a| a["request"] == e["request"])
            .collect();
        check(
            fields,
            format!("request_reviews[{}].identity", e["request"]),
            json!(1),
            json!(matching.len()),
            matching.len() == 1,
        );
        let a = matching.first().copied().unwrap_or(&Value::Null);
        compare(
            fields,
            format!("request_reviews[{}].verdict", e["request"]),
            &e["verdict"],
            &a["verdict"],
        );
    }
    Ok(())
}

fn score_w4(sample: &Value, text: &str, fields: &mut Vec<Value>) -> Result<(), String> {
    let input: W4Input =
        serde_json::from_value(sample["input"].clone()).map_err(|e| e.to_string())?;
    let output: W4Output = serde_json::from_str(text).map_err(|e| e.to_string())?;
    check(fields, "decode", json!(true), json!(true), true);
    let checked = w4::validate(&input, &output, &|id| {
        sample["sources"][id]["text"].as_str().map(str::to_owned)
    });
    check(
        fields,
        "w4.validate",
        json!(true),
        checked
            .as_ref()
            .err()
            .map(|e| json!(e.to_string()))
            .unwrap_or(json!(true)),
        checked.is_ok(),
    );
    let actual = serde_json::to_value(checked.as_ref().unwrap_or(&output)).unwrap();
    for e in items(&sample["expect"]["decisions"]) {
        let matching: Vec<_> = items(&actual["decisions"])
            .iter()
            .filter(|a| a["candidate"] == e["candidate"])
            .collect();
        check(
            fields,
            format!("decisions[{}].identity", e["candidate"]),
            json!(1),
            json!(matching.len()),
            matching.len() == 1,
        );
        let a = matching.first().copied().unwrap_or(&Value::Null);
        for key in ["status", "action", "targets", "pending_reason"] {
            check(
                fields,
                format!("decisions[{}].{key}", e["candidate"]),
                e[key].clone(),
                a[key].clone(),
                !a.is_null() && matches(&a[key], &e[key]),
            );
        }
        let sources: Vec<_> = items(&a["basis"])
            .iter()
            .map(|b| b["source"]["source_id"].clone())
            .collect();
        let required: Vec<_> = items(&e["basis_sources"])
            .iter()
            .filter(|id| {
                // coexist 无 targets：不把比较可解释性偏好变成冻结契约的旧方要求。
                e["action"] != "coexist"
                    || !items(&e["targets"]).is_empty()
                    || input.candidates.iter().any(|c| {
                        c.index == e["candidate"].as_u64().unwrap() as usize
                            && serde_json::to_value(&c.source).unwrap()["source_id"] == **id
                    })
            })
            .cloned()
            .collect();
        check(
            fields,
            format!("decisions[{}].basis_sources", e["candidate"]),
            json!(required),
            json!(sources),
            required.iter().all(|id| sources.contains(id)),
        );
    }
    // 契约约束归属是否合法；评分只要求整组恰好解决预期关系，重复仍然失败。
    let resolved = |decisions: &Value| {
        let mut ids: Vec<_> = items(decisions)
            .iter()
            .flat_map(|d| items(&d["resolves_relations"]))
            .map(|id| id.as_str().unwrap().to_owned())
            .collect();
        ids.sort();
        json!(ids)
    };
    compare(
        fields,
        "resolves_relations",
        &resolved(&sample["expect"]["decisions"]),
        &resolved(&actual["decisions"]),
    );
    Ok(())
}

fn score_raw(stage: &str, sample: &Value, text: Option<&str>) -> Value {
    let mut fields = Vec::new();
    let result = text.map(|text| {
        if sample["fixed_input"] == true {
            fixed_adapter::score(stage, sample, text, &mut fields)
        } else {
            match stage {
                "w1" => score_w1(sample, text, &mut fields),
                "w3" => score_w3(sample, text, &mut fields).map(|_| Value::Null),
                "w4" => score_w4(sample, text, &mut fields).map(|_| Value::Null),
                "r1" | "r4" => recall_adapter::score(stage, sample, text, &mut fields),
                _ => unreachable!(),
            }
        }
    });
    if let Some(Err(error)) = &result {
        check(
            &mut fields,
            "decode/validate",
            json!(true),
            if stage == "r4" {
                serde_json::from_str(error).unwrap_or_else(|_| json!(error))
            } else {
                json!(error)
            },
            false,
        );
    }
    let status = if result.as_ref().is_some_and(|r| {
        r.as_ref()
            .err()
            .is_some_and(|e| e.starts_with("sample_input_rejected:"))
    }) {
        "输入无效"
    } else if result.is_none() {
        "未录制"
    } else if fields.iter().all(|f| f["passed"] == true) {
        "通过"
    } else {
        "失败"
    };
    let mut report = json!({"stage": stage, "id": sample["id"], "label_status": sample["label_status"], "split":sample["split"], "status": status,
        "categories": categories(&fields), "fields": fields, "must_not": sample.get("must_not").or_else(|| sample["expect"].get("must_not")), "note": sample["note"]});
    if stage == "w1" {
        report["raw"] = text
            .map(|t| serde_json::from_str(t).unwrap_or(json!(t)))
            .unwrap_or(Value::Null);
        if sample["m15_review"] == true {
            report["mechanical_status"] = report["status"].clone();
            if report["status"] == "通过" {
                report["status"] = json!("待复核");
            }
            report["manual_review"] = json!({"status":"pending",
                "requirements":sample["review_requirements"],
                "fields":["coverage","subject","speech","sentence","value","polarity","conditions","uncertainty","times"],
                "note":"按 W2 最终候选核对覆盖与限定语；不以字段齐全代替语义正确"});
        }
        report["w2_final"] = result
            .as_ref()
            .and_then(|r| r.as_ref().ok())
            .cloned()
            .unwrap_or(Value::Null);
    }
    if matches!(stage, "r1" | "r4") {
        report["raw_output"] = text
            .map(|t| serde_json::from_str(t).unwrap_or(json!(t)))
            .unwrap_or(Value::Null);
        report["manual_review"] = json!({"must_not":"pending","unlabelled_semantics":"pending"});
    }
    if sample["w3_effort_arm"].is_string() {
        let raw: Value = text
            .map(|t| serde_json::from_str(t).unwrap_or(json!(t)))
            .unwrap_or(Value::Null);
        report["candidate_reviews"] = json!(items(&raw["reviews"]).iter().map(|r| json!({
            "candidate":r["candidate"], "verdict":r["verdict"],
            "aspects":items(&r["problems"]).iter().map(|p|p["aspect"].clone()).collect::<Vec<_>>(),
            "problems":r["problems"], "episode_value":r["episode_value"]
        })).collect::<Vec<_>>());
        report["raw_output"] = raw;
    }
    if stage == "r4" {
        report["validated_output"] = result
            .as_ref()
            .and_then(|r| r.as_ref().ok())
            .cloned()
            .unwrap_or(Value::Null);
    }
    if sample["fixed_input"] == true {
        report["scoring_scope"] = json!("contract_only");
        report["origin"] = sample["origin"].clone();
        report["manual_review"] = json!({"unlabelled_semantics":"pending"});
    }
    report
}

fn score(stage: &str, sample: &Value, text: Option<&str>) -> Value {
    let raw = score_raw(stage, sample, text);
    if !matches!(stage, "w4" | "r4") || text.is_none() {
        return raw;
    }
    let assembled = if stage == "r4" {
        (if sample["fixed_input"] == true {
            fixed_adapter::assemble_r4(sample, text.unwrap())
        } else {
            recall_adapter::assemble_r4(sample, text.unwrap())
        })
        .and_then(|output| serde_json::to_value(output).map_err(|e| e.to_string()))
    } else {
        serde_json::from_str::<W4Output>(text.unwrap())
            .map_err(|e| e.to_string())
            .and_then(|output| {
                let input: W4Input =
                    serde_json::from_value(sample["input"].clone()).map_err(|e| e.to_string())?;
                w4::assemble_new_basis(&input, &output, &|id| {
                    sample["sources"][id]["text"].as_str().map(str::to_owned)
                })
                .map_err(|e| e.to_string())
            })
            .and_then(|output| serde_json::to_value(output).map_err(|e| e.to_string()))
    };
    let mut report = match &assembled {
        Ok(output) => score_raw(stage, sample, Some(&serde_json::to_string(output).unwrap())),
        Err(error) => {
            let mut report = raw.clone();
            let fields = report["fields"].as_array_mut().unwrap();
            check(fields, "assembly", json!(true), json!(error), false);
            report["categories"] = categories(fields);
            if report["status"] != "输入无效" {
                report["status"] = json!("失败");
            }
            report
        }
    };
    report["final_result"] = json!({"status":report["status"],"fields":report["fields"],"categories":report["categories"]});
    report["raw_result"] = raw;
    report["raw_output"] = serde_json::from_str(text.unwrap()).unwrap_or(json!(text.unwrap()));
    report["final_output"] = if stage == "r4" && !report["validated_output"].is_null() {
        report["validated_output"].clone()
    } else {
        assembled.unwrap_or(Value::Null)
    };
    if stage == "r4" {
        report["raw_output_text"] = json!(text.unwrap());
    }
    report
}

fn samples(stage: &str) -> Vec<Value> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("tests/eval/{stage}/samples"));
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|p| p.unwrap().path())
        .collect();
    paths.sort();
    paths
        .into_iter()
        .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
        .map(|p| serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap())
        .collect()
}

fn selected(stages: &str, prefixes: &str) -> Result<Vec<(String, Value)>, String> {
    if fixed_inputs_enabled() {
        return fixed_adapter::selected(stages, prefixes);
    }
    let stages: Vec<_> = stages.split(',').map(str::trim).collect();
    if stages
        .iter()
        .any(|s| !["w1", "w3", "w4", "r1", "r4"].contains(s))
    {
        return Err("EVAL_STAGES only accepts w1,w3,w4,r1,r4".into());
    }
    let prefixes: Vec<_> = prefixes.split(',').map(str::trim).collect();
    if prefixes.len() > 1 && prefixes.iter().any(|p| p.is_empty()) {
        return Err("EVAL_CASES must not contain an empty prefix".into());
    }
    let mut result = Vec::new();
    for stage in ["w1", "w3", "w4", "r1", "r4"]
        .into_iter()
        .filter(|s| stages.contains(s))
    {
        result.extend(
            samples(stage)
                .into_iter()
                .filter(|s| {
                    (stage != "w4" || s["call_w4"] == true)
                        && prefixes
                            .iter()
                            .any(|p| s["id"].as_str().unwrap().starts_with(p))
                })
                .map(|s| (stage.to_owned(), s)),
        );
    }
    Ok(result)
}

fn stage_request(stage: &str, sample: &Value) -> Result<Value, String> {
    let configured = std::env::var("EVAL_MAX_OUTPUT_TOKENS").ok();
    let tokens = eval_output_tokens(configured.as_deref().or(Some(if stage == "w1" {
        "32768"
    } else {
        "16384"
    })))?;
    stage_request_with_output_tokens(stage, sample, tokens)
}

fn eval_output_tokens(value: Option<&str>) -> Result<u32, String> {
    value
        .unwrap_or("16384")
        .parse::<u32>()
        .ok()
        .filter(|tokens| *tokens > 0)
        .ok_or_else(|| "EVAL_MAX_OUTPUT_TOKENS must be a positive u32 integer".into())
}

fn stage_request_with_output_tokens(
    stage: &str,
    sample: &Value,
    tokens: u32,
) -> Result<Value, String> {
    if sample["fixed_input"] == true {
        return fixed_adapter::request(stage, sample, tokens);
    }
    let (stage, name, prompt, schema, input) = match stage {
        "w1" | "w3" => {
            let input = &sample["input"];
            // The samples have no clock or runtime task; said_on is fixed, timezone matches the production default (UTC+8).
            let entities = if input["entities"].is_array() {
                input["entities"].clone()
            } else {
                json!(
                    items(&input["candidates"])
                        .iter()
                        .filter_map(|c| c["subject"].as_str())
                        .collect::<std::collections::BTreeSet<_>>()
                        .into_iter()
                        .map(|id| json!({"id": id, "names": [id]}))
                        .collect::<Vec<_>>()
                )
            };
            let relations = if input["relations"].is_array() {
                input["relations"].clone()
            } else {
                json!(
                    items(&input["candidates"])
                        .iter()
                        .map(|c| c["relation"].as_str().unwrap())
                        .collect::<std::collections::BTreeSet<_>>()
                )
            };
            let mut payload = json!({"contract": if stage == "w1" { "w1.v3" } else { "w3.v3" }, "source_snapshot": {"eval_sample": sample["id"]},
                "current_user_message": input["text"], "user_history": input.get("history").cloned().unwrap_or(json!([])),
                "entities": items(&entities).iter().map(|e| json!({"id": e["id"], "owner_id": "eval",
                    "entity_type": "Person", "canonical_name": e["names"][0], "aliases": e["names"]})).collect::<Vec<_>>(),
                "relations": items(&relations).iter().map(|r| json!({"id": r,
                    "memory_types": ["fact", "preference", "constraint", "episode"]})).collect::<Vec<_>>(),
                "said_on": input["said_on"], "utc_offset_seconds": 28_800, "task_id": input.get("task_id").cloned().unwrap_or(Value::Null)});
            if stage == "w1" {
                (
                    ModelStage::W1,
                    "w1_v3",
                    write_models::W1_PROMPT,
                    write_models::w1_schema(),
                    payload,
                )
            } else {
                let batch = batch(adapter::expand(input).map_err(|e| e.to_string())?, input)
                    .map_err(|e| e.to_string())?;
                if batch
                    .candidates
                    .iter()
                    .any(|c| c.status == w2::Status::Rejected)
                {
                    return Err(format!("{}: W3 sample input rejected by W2", sample["id"]));
                }
                payload["candidates"] = serde_json::to_value(&batch.candidates).unwrap();
                payload["memory_requests"] = serde_json::to_value(&batch.memory_requests).unwrap();
                (
                    ModelStage::W3,
                    "w3_v3",
                    write_models::W3_PROMPT,
                    write_models::w3_schema(),
                    payload,
                )
            }
        }
        "w4" => {
            let input: W4Input =
                serde_json::from_value(sample["input"].clone()).map_err(|e| e.to_string())?;
            let sources: std::collections::BTreeMap<_, _> = sample["sources"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(id, s)| {
                    (
                        id,
                        hippocampus::enrichment::SourceInput {
                            provenance: None,
                            time_anchor: None,
                            text: s["text"].as_str().unwrap().into(),
                            history: vec![],
                        },
                    )
                })
                .collect();
            (
                ModelStage::W4,
                "w4_v4",
                write_models::W4_PROMPT,
                write_models::w4_schema(),
                json!({"contract": "w4.v4", "w4_input": input, "sources": sources}),
            )
        }
        "r1" | "r4" => recall_adapter::request(stage, sample)?,
        _ => return Err("unknown stage".into()),
    };
    let mut request = structured_request(
        stage,
        name,
        prompt,
        input,
        schema,
        &ModelBudgetConfig::default(),
    );
    // 缺省与生产一致；显式覆盖及实际上限另存入录制绑定。
    request["max_output_tokens"] = json!(tokens);
    Ok(request)
}

// Same canonical FNV-1a convention as tests/eval.rs. Missing fingerprints are stale here.
fn fingerprint(request: &Value) -> Value {
    fn strip(value: &mut Value) {
        match value {
            Value::Object(map) => {
                for key in ["now", "trusted_at", "review_binding", "observed_at"] {
                    map.remove(key);
                }
                map.values_mut().for_each(strip);
            }
            Value::Array(items) => items.iter_mut().for_each(strip),
            _ => {}
        }
    }
    let mut request = request.clone();
    request.as_object_mut().unwrap().remove("max_output_tokens");
    if let Some(input) = request["input"].as_str()
        && let Ok(input) = serde_json::from_str::<Value>(input)
    {
        request["input"] = input;
    }
    if matches!(
        request["text"]["format"]["name"].as_str(),
        Some("r1_v1" | "r1_v2" | "r4_v3" | "r4_v4" | "r4_v5" | "r4_v6")
    ) {
        // 新召回不删除时钟字段；request.at/said_on/时区/模型/提示词/完整包都须参与绑定。
        return json!(hippocampus::recall_models::fingerprint(&request));
    }
    strip(&mut request);
    let hash = request
        .to_string()
        .bytes()
        .fold(0xcbf29ce484222325u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        });
    json!(format!("fnv1a64:{hash:016x}"))
}

fn recording_fingerprint(sample: &Value, request: &Value) -> Value {
    if sample["fixed_input"] != true
        && sample["m15_review"] != true
        && sample["w3_prompt_arm"].is_null()
        && sample["w3_effort_arm"].is_null()
    {
        return fingerprint(request);
    }
    let mut request = request.clone();
    request["input"] = serde_json::from_str(request["input"].as_str().unwrap()).unwrap();
    // 固定集及 M1.5 保留完整生产输入/输出上限，不沿用旧样本的字段剔除。
    json!(hippocampus::recall_models::fingerprint(&request))
}

fn fixed_inputs_enabled() -> bool {
    match std::env::var("EVAL_FIXED_INPUTS").as_deref() {
        Err(_) | Ok("0") => false,
        Ok("1" | "af11") => true,
        _ => panic!("EVAL_FIXED_INPUTS only accepts 0, 1 or af11"),
    }
}

fn stages_from_env() -> String {
    std::env::var("EVAL_STAGES").unwrap_or_else(|_| {
        if fixed_inputs_enabled() {
            if std::env::var("EVAL_FIXED_INPUTS").as_deref() == Ok("af11") {
                "r1,r4"
            } else {
                "w3,r4"
            }
        } else {
            "w1,w3,w4"
        }
        .into()
    })
}

fn recording_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(if fixed_inputs_enabled() {
        "tests/eval/fixed_inputs/recordings"
    } else {
        "tests/eval/write_stage/recordings"
    })
}

#[derive(Clone)]
struct EvalModel {
    label: String,
    model: Option<String>,
    base: Option<String>,
    effort: ReasoningEffort,
}
impl EvalModel {
    fn default_model() -> Self {
        Self {
            label: MODEL.into(),
            model: None,
            base: None,
            effort: ReasoningEffort::Medium,
        }
    }
    fn from_env() -> Result<Self, String> {
        let mut model = Self {
            label: std::env::var("EVAL_MODEL_LABEL").unwrap_or_else(|_| MODEL.into()),
            model: std::env::var("EVAL_MODEL").ok(),
            base: std::env::var("EVAL_BASE_URL")
                .ok()
                .or_else(|| std::env::var("DEEPSEEK_BASE_URL").ok())
                .map(|s| s.trim_end_matches('/').to_owned()),
            effort: match std::env::var("EVAL_REASONING_EFFORT").ok() {
                None => ReasoningEffort::Medium,
                Some(s) => serde_json::from_value(json!(s))
                    .map_err(|_| "invalid EVAL_REASONING_EFFORT".to_owned())?,
            },
        };
        if fixed_inputs_enabled() {
            if std::env::var_os("EVAL_MODEL_LABEL").is_none() {
                model.label = if std::env::var("EVAL_FIXED_INPUTS").as_deref() == Ok("af11") {
                    "af11-pro-minimal"
                } else {
                    "ac2-fixed-pro-minimal"
                }
                .into();
            }
            if std::env::var_os("EVAL_MODEL").is_none() {
                model.model = Some("ep-20261003130756-trhj6".into());
            }
            if std::env::var_os("EVAL_REASONING_EFFORT").is_none() {
                model.effort = ReasoningEffort::Minimal;
            }
        }
        model.validate()?;
        Ok(model)
    }
    fn validate(&self) -> Result<(), String> {
        if self.label.is_empty()
            || matches!(self.label.as_str(), "." | "..")
            || !self
                .label
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.'))
        {
            return Err("EVAL_MODEL_LABEL must be a nonempty safe directory name".into());
        }
        if self.model.as_ref().is_some_and(|m| m.trim().is_empty()) {
            return Err("EVAL_MODEL must not be empty".into());
        }
        if let Some(base) = &self.base {
            let url = reqwest::Url::parse(base).map_err(|_| "EVAL_BASE_URL must be a valid URL")?;
            if !matches!(url.scheme(), "http" | "https") {
                return Err("EVAL_BASE_URL must use http or https".into());
            }
        }
        Ok(())
    }
}
fn repeat_count(value: Option<&str>) -> Result<usize, String> {
    value
        .unwrap_or("1")
        .parse::<usize>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| "EVAL_REPEAT must be a positive integer".into())
}

enum Replay {
    Missing,
    Stale,
    Output(String),
    Failed(Value),
}

struct Case {
    stage: String,
    sample: Value,
    request: Value,
    path: PathBuf,
    file: Value,
    replay: Replay,
    record: Value,
    model: EvalModel,
    repeat: usize,
    w3_trial_root: Option<PathBuf>,
}

fn plan(selected: Vec<(String, Value)>, root: &Path) -> Result<Vec<Case>, String> {
    plan_once(selected, root, &EvalModel::default_model(), 1)
}

fn fixed_r4_op(sample: &Value) -> Result<R1Op, String> {
    let output: R1Output = serde_json::from_value(sample["input"]["plan"].clone())
        .map_err(|e| format!("fixed R1 plan: {e}"))?;
    Ok(output.plan.op)
}

fn plan_repeated(
    selected: Vec<(String, Value)>,
    root: &Path,
    model: &EvalModel,
    repeats: usize,
) -> Result<Vec<Case>, String> {
    model.validate()?;
    if repeats == 0 {
        return Err("EVAL_REPEAT must be positive".into());
    }
    selected
        .len()
        .checked_mul(repeats)
        .ok_or("evaluation plan is too large")?;
    // Interleave repetitions by sample, retaining every attempt independently.
    let mut cases = Vec::new();
    for case in selected {
        let mut selected_model = model.clone();
        if case.0 == "r4" && case.1["fixed_input"] == true {
            // 首轮冻结 payload 中的计划已过 R1 核验；不重新调用 R1，也不读取 R4 答案。
            let models = hippocampus::recall_models::RecallModels {
                reasoning_effort: [("r4".into(), model.effort)].into(),
                ..Default::default()
            };
            selected_model.effort = models.r4_effort(Some(fixed_r4_op(&case.1)?));
        }
        for repeat in 1..=repeats {
            let mut dir = root.join(&selected_model.label);
            if selected_model.effort != ReasoningEffort::Medium {
                dir = dir.join(format!(
                    "reasoning-{}",
                    serde_json::to_value(selected_model.effort)
                        .unwrap()
                        .as_str()
                        .unwrap()
                ));
            }
            cases.extend(plan_once(
                vec![case.clone()],
                &dir.join(format!("repeat-{repeat}")),
                &selected_model,
                repeat,
            )?);
        }
    }
    Ok(cases)
}

fn plan_once(
    selected: Vec<(String, Value)>,
    root: &Path,
    model: &EvalModel,
    repeat: usize,
) -> Result<Vec<Case>, String> {
    selected
        .into_iter()
        .map(|(stage, sample)| {
            let mut request = stage_request(&stage, &sample)?;
            if let Some(id) = &model.model {
                request["model"] = json!(id);
            }
            model.effort.apply(&mut request);
            case_from_request(stage, sample, root, model, repeat, request)
        })
        .collect()
}

fn case_from_request(
    stage: String,
    sample: Value,
    root: &Path,
    model: &EvalModel,
    repeat: usize,
    request: Value,
) -> Result<Case, String> {
    let path = root.join(format!("{}.json", sample["id"].as_str().unwrap()));
    let file: Value = match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            json!({"id":sample["id"],"recorded":[]})
        }
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    if !file.is_object() || !file["recorded"].is_array() {
        return Err(format!("{}: invalid recording file", path.display()));
    }
    let current: Vec<_> = items(&file["recorded"])
        .iter()
        .filter(|r| r["model"] == model.label)
        .collect();
    let record = current.iter().rev().find(|r| {
        r["request"] == recording_fingerprint(&sample, &request)
            && r["schema"] == request["text"]["format"]["name"]
            && r["max_output_tokens"] == request["max_output_tokens"]
            && r["base_url"] == json!(model.base)
            && r["repeat"] == repeat
            && r["endpoint"] == request["model"]
            && r["recording_version"] == 2
            && (sample["w3_prompt_arm"].is_null() || r["prompt_arm"] == sample["w3_prompt_arm"])
            && (sample["w3_effort_arm"].is_null()
                || (r["effort_arm"] == sample["w3_effort_arm"]
                    && r["trial"] == "af101-w3-effort"
                    && r["baseline_commit"] == "dc604f8"
                    && r["prompt_byte_fingerprint"] == W3_EFFORT_PROMPT_FINGERPRINT))
    });
    let replay = if let Some(r) = record {
        if r.get("error").is_some() {
            Replay::Failed((*r).clone())
        } else if let Some(text) = r["output_text"].as_str() {
            Replay::Output(text.into())
        } else {
            return Err(format!(
                "{}: completed recording has no output_text",
                path.display()
            ));
        }
    } else if current.is_empty() {
        Replay::Missing
    } else {
        Replay::Stale
    };
    let record = record.map(|r| (*r).clone()).unwrap_or(Value::Null);
    Ok(Case {
        stage,
        sample,
        request,
        path,
        file,
        replay,
        record,
        model: model.clone(),
        repeat,
        w3_trial_root: None,
    })
}

fn preflight(cases: &[Case], limit: Option<usize>) -> Result<usize, String> {
    let needed = cases
        .iter()
        .filter(|c| !matches!(c.replay, Replay::Output(_) | Replay::Failed(_)))
        .count();
    let sends = if limit.is_some() { needed } else { 0 };
    println!(
        "录制预检: 所选 {}，需录制 {needed}，本次将发送 {sends} 次真实调用，上限 {}",
        cases.len(),
        limit
            .map(|l| l.to_string())
            .unwrap_or_else(|| "未启用录制".into())
    );
    use std::io::Write;
    std::io::stdout().flush().map_err(|e| e.to_string())?;
    if limit.is_some_and(|limit| needed > limit) {
        return Err(format!(
            "planned calls {needed} exceed EVAL_RECORD limit {}",
            limit.unwrap()
        ));
    }
    if limit.is_some()
        && let Some(root) = w3_trial_root(cases)?
    {
        let previous = w3_sent_before(root)?;
        if previous.saturating_add(needed) > W3_TRIAL_CAP {
            return Err(format!(
                "W3 对照整批拒绝：累计已发送/在途 {previous} + 缺口 {needed} > 批准上限 {W3_TRIAL_CAP}"
            ));
        }
    }
    if limit.is_some() && cases.iter().any(|c| c.sample["m15_review"] == true) {
        let previous = m15_sent_before(cases)?;
        if previous + needed > 24 {
            return Err(format!(
                "M1.5 W1 整批拒绝：累计已发送/在途 {previous} + 缺口 {needed} > 批准上限 24"
            ));
        }
    }
    Ok(sends)
}

fn completed_text(envelope: &Value) -> Result<String, String> {
    if envelope["status"] != "completed" {
        return Err("response is not completed; not recorded".into());
    }
    let parts: Vec<_> = items(&envelope["output"])
        .iter()
        .flat_map(|i| items(&i["content"]))
        .collect();
    if parts.iter().any(|p| p["type"] == "refusal") {
        return Err("response refused; not recorded".into());
    }
    let text: String = parts
        .iter()
        .filter(|p| p["type"] == "output_text")
        .filter_map(|p| p["text"].as_str())
        .collect();
    if text.is_empty() {
        Err("missing output_text; not recorded".into())
    } else {
        Ok(text)
    }
}

fn save_recording(
    case: &Case,
    envelope: &Value,
    text: Option<&str>,
    error: Option<&str>,
) -> Result<(), String> {
    save_recording_timed(case, envelope, text, error, None)
}

fn save_recording_timed(
    case: &Case,
    envelope: &Value,
    text: Option<&str>,
    error: Option<&str>,
    elapsed_ms: Option<u64>,
) -> Result<(), String> {
    let mut file = case.file.clone();
    let mut recorded = items(&file["recorded"]).to_vec();
    let mut record = json!({"recording_version":2,"model":case.model.label,"repeat":case.repeat,
        "reasoning_effort":case.model.effort,"elapsed_ms":elapsed_ms,
        "schema":case.request["text"]["format"]["name"],"endpoint":case.request["model"],
        "base_url":case.model.base,"max_output_tokens":case.request["max_output_tokens"],
        "request":recording_fingerprint(&case.sample, &case.request),"response":{
            "status":envelope["status"],"incomplete_details":envelope["incomplete_details"],"usage":envelope["usage"]}});
    if let Some(text) = text {
        record["output"] = serde_json::from_str(text).unwrap_or(json!(text));
        record["output_text"] = json!(text);
        record["usage"] = envelope["usage"].clone();
    }
    if let Some(error) = error {
        record["error"] = json!(error);
    }
    w3_trial_metadata(&mut record, case);
    recorded.push(record); // Includes failed attempts; reruns replay them instead of picking a success.
    file["recorded"] = json!(recorded);
    if case.sample["m15_review"] == true || case.w3_trial_root.is_some() {
        return m15_write_json(&case.path, &file);
    }
    std::fs::create_dir_all(case.path.parent().unwrap()).map_err(|e| e.to_string())?;
    let temp = case
        .path
        .with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(
        &temp,
        format!("{}\n", serde_json::to_string_pretty(&file).unwrap()),
    )
    .map_err(|e| e.to_string())?;
    std::fs::rename(temp, &case.path).map_err(|e| e.to_string())
}

fn failed_score(case: &Case, record: &Value) -> Value {
    let mut report = score(&case.stage, &case.sample, None);
    report["status"] = json!("调用失败");
    report["error"] = record["error"].clone();
    report["response"] = record["response"].clone();
    report["categories"] = json!({"contract":{"passed":false,"failures":["call"]},
        "semantic":{"passed":null,"failures":[]},"reference":{"passed":null,"failures":[]}});
    report
}

fn evaluate(
    cases: &[Case],
    limit: Option<usize>,
    send: &mut dyn FnMut(&Value) -> Result<Value, String>,
) -> Result<Value, String> {
    let mut planned = preflight(cases, limit)?;
    // The OS releases this lock on exit/crash; all arms share one fixed ledger.
    let _trial_lock = if planned > 0 {
        w3_trial_root(cases)?.map(w3_trial_lock).transpose()?
    } else {
        None
    };
    if _trial_lock.is_some() {
        planned = preflight(cases, limit)?;
    }
    let mut sent = 0;
    let mut results = Vec::new();
    for case in cases {
        let mut metrics = case.record.clone();
        let mut result = match &case.replay {
            Replay::Output(text) => score(&case.stage, &case.sample, Some(text)),
            Replay::Failed(record) => failed_score(case, record),
            _ if limit.is_some() => {
                // A durable started entry precedes the physical send. A crash
                // without a response remains a possibly billed attempt.
                let attempt = if case.w3_trial_root.is_some() {
                    Some(w3_start_send(case)?)
                } else if case.sample["m15_review"] == true {
                    Some(m15_start_send(case)?)
                } else {
                    None
                };
                sent += 1;
                let started = std::time::Instant::now();
                let response = send(&case.request);
                let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                let envelope = response.as_ref().ok().cloned().unwrap_or(Value::Null);
                if let Some(path) = attempt {
                    let mut entry = json!({"state":"responded","request":recording_fingerprint(&case.sample,&case.request),
                        "elapsed_ms":elapsed_ms,"usage":envelope["usage"],"status":envelope["status"],"error":response.as_ref().err()});
                    if case.w3_trial_root.is_some() {
                        entry["sample"] = case.sample["id"].clone();
                        entry["repeat"] = json!(case.repeat);
                        entry["model"] = case.request["model"].clone();
                        entry["reasoning_effort"] = json!(case.model.effort);
                        w3_trial_metadata(&mut entry, case);
                    }
                    m15_write_json(&path, &entry)?;
                }
                metrics = json!({"elapsed_ms":elapsed_ms,"usage":envelope["usage"]});
                let text = response.and_then(|e| completed_text(&e));
                match text {
                    Ok(text) => {
                        save_recording_timed(case, &envelope, Some(&text), None, Some(elapsed_ms))?;
                        score(&case.stage, &case.sample, Some(&text))
                    }
                    Err(error) => {
                        save_recording_timed(
                            case,
                            &envelope,
                            None,
                            Some(&error),
                            Some(elapsed_ms),
                        )?;
                        failed_score(
                            case,
                            &json!({"error":error,"response":{
                            "status":envelope["status"],"incomplete_details":envelope["incomplete_details"],"usage":envelope["usage"]}}),
                        )
                    }
                }
            }
            Replay::Missing => score(&case.stage, &case.sample, None),
            Replay::Stale => {
                let mut r = score(&case.stage, &case.sample, None);
                r["status"] = json!("录制过期");
                r
            }
        };
        result["model"] = json!(case.model.label);
        result["reasoning_effort"] = json!(case.model.effort);
        result["elapsed_ms"] = metrics["elapsed_ms"].clone();
        result["usage"] = metrics
            .get("usage")
            .filter(|u| !u.is_null())
            .cloned()
            .unwrap_or_else(|| metrics["response"]["usage"].clone());
        result["repeat"] = json!(case.repeat);
        result["max_output_tokens"] = case.request["max_output_tokens"].clone();
        result["request"] = recording_fingerprint(&case.sample, &case.request);
        w3_trial_metadata(&mut result, case);
        results.push(result);
    }
    let summary = stage_summaries(&results, cases);
    let raw_results: Vec<_> = results
        .iter()
        .map(|r| {
            let mut row = r.clone();
            if let Some(raw) = r.get("raw_result") {
                for key in ["status", "fields", "categories"] {
                    row[key] = raw[key].clone();
                }
            }
            row
        })
        .collect();
    Ok(
        json!({"model": cases.first().map(|c|&c.model.label), "planned_calls": planned, "sent_calls": sent,
            "reasoning_effort":cases.first().filter(|first|cases.iter().all(|c|c.model.effort==first.model.effort)).map(|c|c.model.effort),
            "reasoning_efforts":cases.iter().map(|c|serde_json::to_value(c.model.effort).unwrap().as_str().unwrap().to_owned()).collect::<std::collections::BTreeSet<_>>(),
            "metrics":stage_metrics(&results),
            "raw_summary":stage_summaries(&raw_results,cases),"raw_splits":split_reports(&raw_results),
            "summary":summary,"splits":split_reports(&results),"per_sample":per_sample(&results),"results":results}),
    )
}

fn stage_summaries(results: &[Value], cases: &[Case]) -> Vec<Value> {
    ["w1", "w3", "w4", "r1", "r4"].into_iter()
        .filter(|stage| !stage.starts_with('r') || cases.iter().any(|c|c.stage==*stage)).map(|stage| {
        let rows: Vec<_> = results.iter().filter(|r| r["stage"] == stage).collect();
        let count = |status| rows.iter().filter(|r| r["status"] == status).count();
        json!({"stage": stage, "passed": count("通过"), "total": rows.len(),
            "evaluated": count("通过") + count("失败"), "failed": count("失败") + count("调用失败"),
            "missing": count("未录制"), "stale": count("录制过期"), "invalid_input": count("输入无效"),
            "draft": rows.iter().filter(|r| r["label_status"] == "draft").count()})
    }).collect()
}

fn stage_metrics(results: &[Value]) -> Vec<Value> {
    ["w1", "w3", "w4", "r1", "r4"].into_iter()
        .filter(|s|results.iter().any(|r|r["stage"]==*s)).map(|stage| {
            let rows:Vec<_>=results.iter().filter(|r|r["stage"]==stage).collect();
            let mut elapsed:Vec<_>=rows.iter().filter_map(|r|r["elapsed_ms"].as_u64()).collect();
            elapsed.sort_unstable();
            // Nearest-rank percentiles; historical recordings without timers remain null.
            let percentile=|p:usize| (!elapsed.is_empty()).then(||elapsed[(elapsed.len()*p).div_ceil(100)-1]);
            let mean=|path:&str| {
                let tokens:Vec<_>=rows.iter().filter_map(|r|r.pointer(path).and_then(Value::as_u64)).collect();
                json!({"count":tokens.len(),"mean":(!tokens.is_empty()).then(||tokens.iter().map(|n|*n as f64).sum::<f64>()/tokens.len() as f64)})
            };
            json!({"stage":stage,"elapsed_ms":{"count":elapsed.len(),"p50":percentile(50),"p90":percentile(90),"max":elapsed.last()},
                "output_tokens":mean("/usage/output_tokens"),"reasoning_tokens":mean("/usage/output_tokens_details/reasoning_tokens")})
        }).collect()
}

fn split_reports(results: &[Value]) -> Vec<Value> {
    ["r1","r4"].into_iter().flat_map(|stage|["dev","holdout"].into_iter().map(move |split|(stage,split)))
        .filter(|(stage,_)|results.iter().any(|r|r["stage"]==*stage)).map(|(stage,split)|{
            let rows:Vec<_>=results.iter().filter(|r|r["stage"]==stage && r["split"]==split).collect();
            json!({"stage":stage,"split":split,"total":rows.len(),"passed":rows.iter().filter(|r|r["status"]=="通过").count(),
                "failed":rows.iter().filter(|r|matches!(r["status"].as_str(),Some("失败"|"调用失败"))).count(),
                "missing":rows.iter().filter(|r|r["status"]=="未录制").count(),"stale":rows.iter().filter(|r|r["status"]=="录制过期").count()})
        }).collect()
}

fn per_sample(results: &[Value]) -> Vec<Value> {
    let mut groups = std::collections::BTreeMap::<(String, String, String), Vec<&Value>>::new();
    for row in results {
        groups
            .entry((
                row["model"].as_str().unwrap().into(),
                row["stage"].as_str().unwrap().into(),
                row["id"].as_str().unwrap().into(),
            ))
            .or_default()
            .push(row);
    }
    groups.into_iter().map(|((model,stage,id),rows)| {
        let errors:Value = ["contract","semantic","reference"].into_iter().map(|category| {
            (category.to_owned(),json!(rows.iter().filter(|r|r["categories"][category]["passed"]==false).count()))
        }).collect();
        json!({"model":model,"stage":stage,"id":id,"passed":rows.iter().filter(|r|r["status"]=="通过").count(),
            "total":rows.len(),"error_categories":errors,
            "repeats":rows.iter().map(|r|json!({"repeat":r["repeat"],"status":r["status"]})).collect::<Vec<_>>()})
    }).collect()
}

// Historical rescoring is deliberately separate from request-bound repeat comparisons.
fn historical_reports() -> Result<Value, String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !std::process::Command::new("git")
        .args(["rev-parse", "--verify", "54ef102^{commit}"])
        .current_dir(root)
        .output()
        .map_err(|e| e.to_string())?
        .status
        .success()
    {
        return Err("historical baseline 54ef102 is unavailable".into());
    }
    let mut reports = Vec::new();
    for version in [1, 2] {
        let mut results = Vec::new();
        for (stage, sample) in selected("w1,w3,w4", "")? {
            let path = format!(
                "tests/eval/write_stage/recordings/{}.json",
                sample["id"].as_str().unwrap()
            );
            let text = if version == 1 {
                let output = std::process::Command::new("git")
                    .args(["show", &format!("54ef102:{path}")])
                    .current_dir(root)
                    .output()
                    .map_err(|e| e.to_string())?;
                if output.status.success() {
                    Some(String::from_utf8(output.stdout).map_err(|e| e.to_string())?)
                } else {
                    None
                }
            } else {
                Some(std::fs::read_to_string(root.join(path)).map_err(|e| e.to_string())?)
            };
            let file: Value = text
                .map(|t| serde_json::from_str(&t))
                .transpose()
                .map_err(|e| e.to_string())?
                .unwrap_or(Value::Null);
            let record = items(&file["recorded"])
                .iter()
                .rev()
                .find(|r| r["model"] == MODEL);
            let text = record.and_then(|r| {
                r["output_text"]
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| r.get("output").map(Value::to_string))
            });
            results.push(score(&stage, &sample, text.as_deref()));
        }
        let summary:Vec<_> = ["w1","w3","w4"].into_iter().map(|stage| {
            let rows:Vec<_> = results.iter().filter(|r|r["stage"]==stage).collect();
            json!({"stage":stage,"total":rows.len(),"passed":rows.iter().filter(|r|r["status"]=="通过").count(),
                "raw_passed":rows.iter().filter(|r|r.get("raw_result").unwrap_or(r)["status"]=="通过").count(),
                "missing":rows.iter().filter(|r|r["status"]=="未录制").count()})
        }).collect();
        reports.push(json!({"prompt_version":version,"summary":summary,"results":results}));
    }
    Ok(json!({"historical":true,"comparison_eligible":false,"sent_calls":0,"versions":reports}))
}

#[test]
fn write_stage_eval_suite() {
    let m15 = std::env::var("EVAL_M15").as_deref() == Ok("w1");
    let w3_trial = std::env::var("EVAL_M15").as_deref() == Ok("w3");
    let w3_effort = std::env::var("EVAL_M15").as_deref() == Ok("w3-effort");
    if std::env::var("EVAL_DRY_RUN").as_deref() == Ok("1") {
        if w3_effort {
            w3_effort_dry_run();
        } else if w3_trial {
            w3_comparison_dry_run();
        } else if m15 {
            m15_dry_run();
        } else {
            dry_run_plan();
        }
        return;
    }
    if w3_effort {
        w3_trial_options("AF101").unwrap();
    } else if w3_trial {
        w3_comparison_options().unwrap();
    }
    if std::env::var("EVAL_HISTORY").as_deref() == Ok("1") {
        println!(
            "{}",
            serde_json::to_string_pretty(&historical_reports().unwrap()).unwrap()
        );
        return;
    }
    let gate = eval_gate(std::env::var("EVAL_GATE").ok().as_deref()).unwrap();
    let cases = if w3_effort {
        w3_effort_from_env().unwrap()
    } else if w3_trial {
        w3_comparison_from_env().unwrap()
    } else if m15 {
        m15_w1_plan(&m15_root()).unwrap()
    } else {
        let selected = selected(
            &stages_from_env(),
            &std::env::var("EVAL_CASES").unwrap_or_default(),
        )
        .unwrap();
        let root = recording_root();
        let model = EvalModel::from_env().unwrap();
        let repeats = repeat_count(std::env::var("EVAL_REPEAT").ok().as_deref()).unwrap();
        plan_repeated(selected, &root, &model, repeats).unwrap()
    };
    let limit = std::env::var("EVAL_RECORD").ok().map(|s| {
        s.parse::<usize>()
            .expect("EVAL_RECORD must be a nonnegative integer")
    });
    let sends = preflight(&cases, limit).unwrap();
    let model = &cases.first().expect("nonempty stage plan").model;
    // No client or credentials when there are no sends, including fully cached EVAL_RECORD=0.
    let live = (sends > 0).then(|| {
        let base = model
            .base
            .as_ref()
            .expect("EVAL_RECORD requires EVAL_BASE_URL or DEEPSEEK_BASE_URL");
        let key = std::env::var("EVAL_API_KEY")
            .or_else(|_| std::env::var("DEEPSEEK_API_KEY"))
            .expect("EVAL_RECORD requires EVAL_API_KEY or DEEPSEEK_API_KEY");
        assert!(
            !key.trim().is_empty(),
            "evaluation API key must not be empty"
        );
        let url = format!("{}/responses", base.trim_end_matches('/'));
        let parsed = reqwest::Url::parse(&url).expect("DEEPSEEK_BASE_URL must be a valid URL");
        assert!(
            matches!(parsed.scheme(), "http" | "https"),
            "DEEPSEEK_BASE_URL must use http or https"
        );
        let http = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .timeout(std::time::Duration::from_secs(180))
            .build()
            .unwrap();
        (http, url, key)
    });
    let mut report = evaluate(&cases, limit, &mut |request| {
        let (http, url, key) = live.as_ref().expect("offline replay must never send");
        let response = http
            .post(url)
            .bearer_auth(key)
            .json(request)
            .timeout(if m15 {
                std::time::Duration::from_secs(360)
            } else {
                eval_request_timeout(request)
            })
            .send()
            .map_err(|e| e.to_string())?;
        if !response.status().is_success() {
            return Err(format!("HTTP {}", response.status()));
        }
        response.json().map_err(|e| e.to_string())
    })
    .unwrap();
    if w3_effort {
        report["effort_comparison"] = w3_effort_report(&report);
        let path = w3_effort_root().join("report.json");
        m15_write_json(&path, &report).unwrap();
        println!("AF101 W3 强度对照报告：{}", path.display());
    } else if w3_trial {
        report["prompt_comparison"] = w3_comparison_report(&report);
        let path = w3_comparison_root().join("report.json");
        m15_write_json(&path, &report).unwrap();
        println!("AF96 W3 对照报告：{}", path.display());
    }
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    if m15 {
        let path = std::env::var_os("EVAL_M15_OUTPUT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("target/eval-m15/w1-report.json"));
        m15_write_json(&path, &report).unwrap();
        println!(
            "M1.5 W1 字段对照（raw / w2_final / review / latency / usage）：{}",
            path.display()
        );
    }
    check_report(&report, gate).expect("stage evaluation failures; see report above");
}

fn m15_root() -> PathBuf {
    std::env::var_os("EVAL_M15_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("tests/eval/m15/recordings/w1"))
}

fn m15_w1_plan(root: &Path) -> Result<Vec<Case>, String> {
    let samples: Vec<Value> =
        serde_json::from_str(include_str!("eval/m15/w1.json")).map_err(|e| e.to_string())?;
    if samples.len() != 6 {
        return Err("M1.5 requires six W1 scenarios".into());
    }
    let mut model = EvalModel::from_env()?;
    model.label = "m15-pro".into();
    model.model = Some("ep-20261003130756-trhj6".into());
    let mut cases = Vec::new();
    for effort in [ReasoningEffort::Low, ReasoningEffort::Medium] {
        model.effort = effort;
        model.label = format!(
            "m15-pro-{}",
            serde_json::to_value(effort).unwrap().as_str().unwrap()
        );
        cases.extend(plan_repeated(
            samples.iter().cloned().map(|s| ("w1".into(), s)).collect(),
            root,
            &model,
            2,
        )?);
    }
    // Pair identical input/repetition across efforts to reduce order effects.
    cases.sort_by_key(|c| {
        (
            c.sample["id"].as_str().unwrap().to_owned(),
            c.repeat,
            c.model.label.clone(),
        )
    });
    Ok(cases)
}

fn m15_dry_run() {
    let cases = m15_w1_plan(&m15_root()).unwrap();
    let limit = std::env::var("EVAL_RECORD")
        .ok()
        .map(|s| s.parse().expect("EVAL_RECORD integer"));
    preflight(&cases, limit).unwrap();
    let needed = cases
        .iter()
        .filter(|c| !matches!(c.replay, Replay::Output(_) | Replay::Failed(_)))
        .count();
    let bytes: usize = cases
        .iter()
        .map(|c| serde_json::to_vec(&c.request).unwrap().len())
        .sum();
    let output: u64 = cases
        .iter()
        .map(|c| c.request["max_output_tokens"].as_u64().unwrap())
        .sum();
    println!("M1.5 W1 对照：6 场景 × low/medium × 2 次 = 24 个 W1 槽位；W3/W4/R1/R4=0。");
    println!(
        "严格指纹可复用 {}；需新增 {}；精确预留上限 24。",
        24 - needed,
        needed
    );
    assert!(
        std::process::Command::new("python3")
            .args(["tests/eval/m15/token_estimate.py", "w1"])
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .status()
            .unwrap()
            .success()
    );
    println!(
        "token 粗估：完整请求 UTF-8 字节/3 向上取整，输入约 {}；输出按配置上限合计 {}（不是预期用量）。",
        bytes.div_ceil(3),
        output
    );
    println!("干跑：真实调用 0；未读取密钥、未创建录制。标注 draft，语义待人工复核。");
}

fn m15_write_json(path: &Path, value: &Value) -> Result<(), String> {
    let parent = path.parent().unwrap();
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    let mut file = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
    use std::io::Write;
    file.write_all(serde_json::to_string_pretty(value).unwrap().as_bytes())
        .map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    std::fs::rename(tmp, path).map_err(|e| e.to_string())?;
    std::fs::File::open(parent)
        .and_then(|d| d.sync_all())
        .map_err(|e| e.to_string())
}

fn m15_start_send(case: &Case) -> Result<PathBuf, String> {
    let path = case
        .path
        .parent()
        .unwrap()
        .join("sends")
        .join(format!("{}.json", hippocampus::diagnostics::new_id()));
    m15_write_json(
        &path,
        &json!({"state":"started","sample":case.sample["id"],"repeat":case.repeat,
        "effort":case.model.effort,"model":case.request["model"],"request":recording_fingerprint(&case.sample,&case.request)}),
    )?;
    Ok(path)
}

fn m15_sent_before(cases: &[Case]) -> Result<usize, String> {
    let dirs: std::collections::BTreeSet<_> = cases
        .iter()
        .map(|c| c.path.parent().unwrap().join("sends"))
        .collect();
    count_sends(dirs)
}

fn count_sends(dirs: impl IntoIterator<Item = PathBuf>) -> Result<usize, String> {
    let mut count = 0;
    for dir in dirs {
        if !dir.exists() {
            continue;
        }
        for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.extension().and_then(|s| s.to_str()) == Some("json") {
                let value: Value =
                    serde_json::from_slice(&std::fs::read(&path).map_err(|e| e.to_string())?)
                        .map_err(|e| e.to_string())?;
                if !matches!(value["state"].as_str(), Some("started" | "responded")) {
                    return Err("invalid M1.5 send log".into());
                }
                count += 1;
            }
        }
    }
    Ok(count)
}

fn w3_prompt_fingerprint(prompt: &str) -> String {
    let hash = prompt.bytes().fold(0xcbf29ce484222325u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    });
    format!("fnv1a64:{hash:016x}")
}

fn w3_arm_prompt(sample: &Value) -> &'static str {
    match sample["w3_prompt_arm"].as_str() {
        Some("old") => W3_OLD_PROMPT,
        Some("new") => write_models::W3_PROMPT,
        _ => unreachable!("fixed AF96 prompt arm"),
    }
}

fn w3_trial_metadata(row: &mut Value, case: &Case) {
    if case.w3_trial_root.is_none() {
        return;
    }
    if case.sample["w3_effort_arm"].is_string() {
        row["trial"] = json!("af101-w3-effort");
        row["effort_arm"] = case.sample["w3_effort_arm"].clone();
        row["baseline_commit"] = json!("dc604f8");
        row["prompt_byte_fingerprint"] = json!(W3_EFFORT_PROMPT_FINGERPRINT);
    } else {
        row["prompt_arm"] = case.sample["w3_prompt_arm"].clone();
        row["baseline_commit"] = json!("91a068a");
        row["prompt_byte_fingerprint"] = json!(w3_prompt_fingerprint(w3_arm_prompt(&case.sample)));
    }
}

fn w3_comparison_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("target/eval-m15/w3-af96")
}

fn w3_comparison_options() -> Result<(), String> {
    w3_trial_options("AF96")
}

fn w3_trial_options(trial: &str) -> Result<(), String> {
    // This authorization covers one fixed cohort; filtering/config changes need a new approval.
    for key in [
        "EVAL_M15_DIR",
        "EVAL_M15_OUTPUT",
        "EVAL_CASES",
        "EVAL_STAGES",
        "EVAL_FIXED_INPUTS",
        "EVAL_HISTORY",
        "EVAL_MODEL",
        "EVAL_MODEL_LABEL",
        "EVAL_REASONING_EFFORT",
        "EVAL_REPEAT",
        "EVAL_MAX_OUTPUT_TOKENS",
    ] {
        if std::env::var_os(key).is_some() {
            return Err(format!("{trial} W3 配置固定，不接受 {key}"));
        }
    }
    Ok(())
}

fn w3_comparison_from_env() -> Result<Vec<Case>, String> {
    w3_comparison_options()?;
    let base = std::env::var("EVAL_BASE_URL")
        .ok()
        .or_else(|| std::env::var("DEEPSEEK_BASE_URL").ok())
        .map(|s| s.trim_end_matches('/').to_owned());
    w3_comparison_plan(&w3_comparison_root(), base)
}

fn w3_comparison_plan(root: &Path, base: Option<String>) -> Result<Vec<Case>, String> {
    if w3_prompt_fingerprint(W3_OLD_PROMPT) != W3_OLD_FINGERPRINT {
        return Err("AF96 旧提示词快照不是固定的 91a068a 字节".into());
    }
    let samples: Vec<Value> =
        serde_json::from_str(include_str!("eval/m15/w3.json")).map_err(|e| e.to_string())?;
    if samples.len() != 4 || samples.iter().any(|s| s["fictional"] != true) {
        return Err("AF96 requires four fictional W3 scenarios".into());
    }
    let model = EvalModel {
        label: "af96-pro-minimal".into(),
        model: Some("ep-20261003130756-trhj6".into()),
        base,
        effort: ReasoningEffort::Minimal,
    };
    model.validate()?;
    let mut cases = Vec::new();
    for sample in samples {
        // One deterministic W2 assembly per scenario, cloned byte-for-byte into both arms/repeats.
        let mut frozen = stage_request_with_output_tokens("w3", &sample, 16384)?;
        frozen["model"] = json!(model.model);
        model.effort.apply(&mut frozen);
        let mut old = frozen.clone();
        old["instructions"] = structured_request(
            ModelStage::W3,
            "w3_v3",
            W3_OLD_PROMPT,
            serde_json::from_str(frozen["input"].as_str().unwrap()).unwrap(),
            frozen["text"]["format"]["schema"].clone(),
            &ModelBudgetConfig::default(),
        )["instructions"]
            .clone();
        for repeat in 1..=2 {
            for arm in if repeat == 1 {
                ["old", "new"]
            } else {
                ["new", "old"]
            } {
                let mut sample = sample.clone();
                sample["w3_prompt_arm"] = json!(arm);
                let mut case = case_from_request(
                    "w3".into(),
                    sample,
                    &root.join(arm).join(format!("repeat-{repeat}")),
                    &model,
                    repeat,
                    if arm == "old" {
                        old.clone()
                    } else {
                        frozen.clone()
                    },
                )?;
                case.w3_trial_root = Some(root.to_owned());
                cases.push(case);
            }
        }
    }
    Ok(cases)
}

fn w3_trial_root(cases: &[Case]) -> Result<Option<&Path>, String> {
    let root = cases.iter().find_map(|c| c.w3_trial_root.as_deref());
    if let Some(root) = root
        && cases
            .iter()
            .any(|c| c.w3_trial_root.as_deref() != Some(root))
    {
        return Err("W3 trial cannot mix roots or unrelated cases".into());
    }
    Ok(root)
}

fn w3_sent_before(root: &Path) -> Result<usize, String> {
    count_sends([root.join("sends")])
}

fn w3_trial_lock(root: &Path) -> Result<std::fs::File, String> {
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join("trial.lock"))
        .map_err(|e| e.to_string())?;
    file.try_lock()
        .map_err(|e| format!("W3 试验已被另一执行者锁定：{e}"))?;
    Ok(file)
}

fn w3_start_send(case: &Case) -> Result<PathBuf, String> {
    let root = case.w3_trial_root.as_deref().unwrap();
    if w3_sent_before(root)? >= W3_TRIAL_CAP {
        return Err("W3 cumulative send allowance exhausted".into());
    }
    let path = root
        .join("sends")
        .join(format!("{}.json", hippocampus::diagnostics::new_id()));
    let mut entry = json!({"state":"started","sample":case.sample["id"],
        "repeat":case.repeat,
        "model":case.request["model"],"reasoning_effort":case.model.effort,
        "request":recording_fingerprint(&case.sample,&case.request)});
    w3_trial_metadata(&mut entry, case);
    m15_write_json(&path, &entry)?;
    Ok(path)
}

fn w3_comparison_report(report: &Value) -> Value {
    let results = items(&report["results"]);
    let arms = ["old", "new"].map(|arm| {
        let rows:Vec<_> = results.iter().filter(|r|r["prompt_arm"]==arm).cloned().collect();
        json!({"arm":arm,"total":rows.len(),"passed":rows.iter().filter(|r|r["status"]=="通过").count(),
            "failed":rows.iter().filter(|r|matches!(r["status"].as_str(),Some("失败"|"调用失败"))).count(),
            "missing":rows.iter().filter(|r|r["status"]=="未录制").count(),
            "stale":rows.iter().filter(|r|r["status"]=="录制过期").count(),
            "metrics":stage_metrics(&rows),"per_sample":per_sample(&rows),
            "usage_totals":{
                "available":rows.iter().filter(|r|r["usage"].is_object()).count(),
                "input_tokens":rows.iter().filter_map(|r|r["usage"]["input_tokens"].as_u64()).sum::<u64>(),
                "output_tokens_including_reasoning":rows.iter().filter_map(|r|r["usage"]["output_tokens"].as_u64()).sum::<u64>(),
                "reasoning_tokens":rows.iter().filter_map(|r|r["usage"]["output_tokens_details"]["reasoning_tokens"].as_u64()).sum::<u64>()}})
    });
    json!({"baseline_commit":"91a068a","authorized_calls":W3_TRIAL_CAP,
        "note":"每场景两次仅作定点诊断，不证明统计显著；draft 标签须复核。",
        "arms":arms})
}

fn w3_comparison_dry_run() {
    let cases = w3_comparison_from_env().unwrap();
    let limit = std::env::var("EVAL_RECORD")
        .ok()
        .map(|s| s.parse().expect("EVAL_RECORD integer"));
    let sends = preflight(&cases, limit).unwrap();
    let needed = cases
        .iter()
        .filter(|c| matches!(c.replay, Replay::Missing | Replay::Stale))
        .count();
    println!("AF96 W3 对照：旧/新提示词 × 4 虚构场景 × 2 次 = 16 个独立 W3 槽位；W1/W4/R1/R4=0。");
    println!("固定 Pro、minimal、w3.v3、said_on=2026-10-07、UTC+8、max_output_tokens=16384。");
    println!(
        "旧提示词：91a068a，字节指纹 {W3_OLD_FINGERPRINT}；新提示词：{}。",
        w3_prompt_fingerprint(write_models::W3_PROMPT)
    );
    println!(
        "严格可复用 {}；缺口 {needed}；累计已发送/在途 {}；批准上限 16。",
        cases.len() - needed,
        w3_sent_before(&w3_comparison_root()).unwrap()
    );
    println!("两臂输入逐字一致，仅提示词不同；每场景第1次 old→new，第2次 new→old。");
    println!(
        "录制/额度根目录：target/eval-m15/w3-af96；报告：target/eval-m15/w3-af96/report.json。"
    );
    println!("录制模式预检计划 {sends}；干跑实际发送 0，未读取密钥、未创建录制。");
}

fn w3_effort_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("target/eval-m15/w3-af101")
}

fn w3_effort_from_env() -> Result<Vec<Case>, String> {
    w3_trial_options("AF101")?;
    let base = std::env::var("EVAL_BASE_URL")
        .ok()
        .or_else(|| std::env::var("DEEPSEEK_BASE_URL").ok())
        .map(|s| s.trim_end_matches('/').to_owned());
    w3_effort_plan(&w3_effort_root(), base)
}

fn w3_effort_plan(root: &Path, base: Option<String>) -> Result<Vec<Case>, String> {
    if w3_prompt_fingerprint(write_models::W3_PROMPT) != W3_EFFORT_PROMPT_FINGERPRINT {
        return Err("AF101 W3 提示词不再是固定的 dc604f8 字节，禁止变更本组试验".into());
    }
    let samples: Vec<Value> =
        serde_json::from_str(include_str!("eval/m15/w3-effort.json")).map_err(|e| e.to_string())?;
    if samples.len() != 4 || samples.iter().any(|s| s["fictional"] != true) {
        return Err("AF101 requires four fictional W3 scenarios".into());
    }
    let mut model = EvalModel {
        label: "af101-pro-minimal".into(),
        model: Some("ep-20261003130756-trhj6".into()),
        base,
        effort: ReasoningEffort::Minimal,
    };
    model.validate()?;
    let mut cases = Vec::new();
    for sample in samples {
        // Assemble W2 once. Only the top-level reasoning effort differs between arms.
        let mut frozen = stage_request_with_output_tokens("w3", &sample, 16384)?;
        frozen["model"] = json!(model.model);
        for repeat in 1..=2 {
            let arms = if repeat == 1 {
                [ReasoningEffort::Minimal, ReasoningEffort::Low]
            } else {
                [ReasoningEffort::Low, ReasoningEffort::Minimal]
            };
            for effort in arms {
                let arm = serde_json::to_value(effort).unwrap();
                let arm = arm.as_str().unwrap();
                model.effort = effort;
                model.label = format!("af101-pro-{arm}");
                let mut request = frozen.clone();
                effort.apply(&mut request);
                let mut sample = sample.clone();
                sample["w3_effort_arm"] = json!(arm);
                let mut case = case_from_request(
                    "w3".into(),
                    sample,
                    &root.join(arm).join(format!("repeat-{repeat}")),
                    &model,
                    repeat,
                    request,
                )?;
                case.w3_trial_root = Some(root.to_owned());
                cases.push(case);
            }
        }
    }
    Ok(cases)
}

fn w3_effort_report(report: &Value) -> Value {
    let results = items(&report["results"]);
    let arms = ["minimal", "low"].map(|arm| {
        let rows: Vec<_> = results.iter().filter(|r| r["effort_arm"] == arm).cloned().collect();
        json!({"arm":arm,"total":rows.len(),
            "passed":rows.iter().filter(|r|r["status"]=="通过").count(),
            "failed":rows.iter().filter(|r|matches!(r["status"].as_str(),Some("失败"|"调用失败"))).count(),
            "missing":rows.iter().filter(|r|r["status"]=="未录制").count(),
            "stale":rows.iter().filter(|r|r["status"]=="录制过期").count(),
            "draft":rows.iter().filter(|r|r["label_status"]=="draft").count(),
            "metrics":stage_metrics(&rows),"per_sample":per_sample(&rows),
            "usage_totals":{
                "available":rows.iter().filter(|r|r["usage"].is_object()).count(),
                "input_tokens":rows.iter().filter_map(|r|r["usage"]["input_tokens"].as_u64()).sum::<u64>(),
                "output_tokens_including_reasoning":rows.iter().filter_map(|r|r["usage"]["output_tokens"].as_u64()).sum::<u64>(),
                "reasoning_tokens":rows.iter().filter_map(|r|r["usage"]["output_tokens_details"]["reasoning_tokens"].as_u64()).sum::<u64>()}})
    });
    json!({"trial":"af101-w3-effort","baseline_commit":"dc604f8",
        "prompt_byte_fingerprint":W3_EFFORT_PROMPT_FINGERPRINT,"authorized_calls":W3_TRIAL_CAP,
        "note":"新样本标签 draft，由 Claude 确认；每场景两次仅供定点诊断，不证明统计显著。",
        "arms":arms})
}

fn w3_effort_dry_run() {
    let cases = w3_effort_from_env().unwrap();
    let limit = std::env::var("EVAL_RECORD")
        .ok()
        .map(|s| s.parse().expect("EVAL_RECORD integer"));
    preflight(&cases, limit).unwrap();
    let needed = cases
        .iter()
        .filter(|c| matches!(c.replay, Replay::Missing | Replay::Stale))
        .count();
    println!(
        "AF101 W3 强度对照：同一提示词 × 4 虚构场景 × minimal/low × 2 次 = 16 个独立 W3 槽位；W1/W4/R1/R4=0。"
    );
    println!(
        "固定 Pro、w3.v3、said_on=2026-10-07、UTC+8、max_output_tokens=16384；两臂仅 reasoning.effort 不同。"
    );
    println!("固定提示词：dc604f8，字节指纹 {W3_EFFORT_PROMPT_FINGERPRINT}。");
    println!(
        "严格可复用 {}；缺口 {needed}；本组累计已发送/在途 {}；追加批准上限 16（AF96 账本保留）。",
        cases.len() - needed,
        w3_sent_before(&w3_effort_root()).unwrap()
    );
    println!("配对顺序：每场景第1次 minimal→low，第2次 low→minimal。");
    println!("输出配置容量合计 262144 token（含推理，非预期用量）；low 实际用量未知。");
    println!("干跑：真实调用 0；未读取密钥、未创建录制。新标签 draft，须 Claude 确认。");
}

#[test]
fn af101_w3_effort_plan_is_frozen_and_rejects_fifteen_before_output() {
    let temp = TempRecords::new();
    let cases = w3_effort_plan(&temp.0, None).unwrap();
    assert_eq!(cases.len(), 16);
    assert!(preflight(&cases, Some(15)).is_err());
    assert_eq!(preflight(&cases, Some(16)).unwrap(), 16);
    assert_eq!(preflight(&cases, None).unwrap(), 0);
    assert!(!temp.0.exists());
    assert_ne!(w3_effort_root(), w3_comparison_root());
    assert_eq!(
        w3_prompt_fingerprint(write_models::W3_PROMPT),
        W3_EFFORT_PROMPT_FINGERPRINT
    );
    assert_eq!(
        cases
            .iter()
            .map(|c| &c.path)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        16
    );
    for pair in cases.chunks_exact(4) {
        assert_eq!(
            pair.iter()
                .map(|c| c.sample["w3_effort_arm"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["minimal", "low", "low", "minimal"]
        );
        for case in pair {
            assert_eq!(case.stage, "w3");
            assert_eq!(
                case.request["reasoning"]["effort"],
                case.sample["w3_effort_arm"]
            );
            assert_eq!(case.request["model"], "ep-20261003130756-trhj6");
            assert_eq!(case.request["max_output_tokens"], 16384);
            assert_eq!(case.request["text"]["format"]["name"], "w3_v3");
            assert!(matches!(
                case.sample["label_status"].as_str(),
                Some("draft" | "reviewed")
            ));
            let mut a = case.request.clone();
            let mut b = pair[0].request.clone();
            a.as_object_mut().unwrap().remove("reasoning");
            b.as_object_mut().unwrap().remove("reasoning");
            assert_eq!(a, b);
            let input: Value =
                serde_json::from_str(case.request["input"].as_str().unwrap()).unwrap();
            for key in [
                "model",
                "focus",
                "expect",
                "repeat",
                "w3_effort_arm",
                "w3_prompt_arm",
                "fictional",
                "label_status",
            ] {
                assert!(input.get(key).is_none(), "{key}");
            }
            assert_eq!(input["user_history"], case.sample["input"]["history"]);
            assert_eq!(input["said_on"], "2026-10-07");
            assert_eq!(input["utc_offset_seconds"], 28_800);
            assert!(
                items(&input["candidates"])
                    .iter()
                    .all(|c| c["status"] == "ok")
            );
            assert!(
                case.request["instructions"]
                    .as_str()
                    .unwrap()
                    .ends_with(write_models::W3_PROMPT)
            );
        }
    }
}

#[test]
fn af101_w3_effort_full_fingerprint_and_metadata_never_borrow_responses() {
    let temp = TempRecords::new();
    let cases = w3_effort_plan(&temp.0, None).unwrap();
    let case = &cases[0];
    let original = recording_fingerprint(&case.sample, &case.request);
    assert_ne!(
        original,
        recording_fingerprint(&cases[1].sample, &cases[1].request)
    );
    for path in [
        "/instructions",
        "/model",
        "/text/format/schema",
        "/input",
        "/reasoning/effort",
        "/max_output_tokens",
    ] {
        let mut changed = case.request.clone();
        *changed.pointer_mut(path).unwrap() = if path == "/input" {
            json!(json!({"changed":true}).to_string())
        } else {
            json!("changed")
        };
        assert_ne!(
            recording_fingerprint(&case.sample, &changed),
            original,
            "{path}"
        );
    }
    let mut changed = case.request.clone();
    let mut input: Value = serde_json::from_str(changed["input"].as_str().unwrap()).unwrap();
    input["trusted_at"] = json!("full clock binding");
    changed["input"] = json!(input.to_string());
    assert_ne!(recording_fingerprint(&case.sample, &changed), original);
    let output = fake_w3(&case.sample);
    save_recording(
        case,
        &fake_envelope(output.clone()),
        Some(&output.to_string()),
        None,
    )
    .unwrap();
    let ready = w3_effort_plan(&temp.0, None).unwrap();
    assert!(matches!(ready[0].replay, Replay::Output(_)));
    for i in [1, 3] {
        // Even copying an otherwise valid response into a different slot must not borrow it.
        m15_write_json(&ready[i].path, &ready[0].file).unwrap();
        assert!(!matches!(
            w3_effort_plan(&temp.0, None).unwrap()[i].replay,
            Replay::Output(_)
        ));
    }
    for (key, wrong) in [
        ("effort_arm", json!("low")),
        ("repeat", json!(2)),
        ("trial", json!("af96")),
        ("baseline_commit", json!("91a068a")),
        ("prompt_byte_fingerprint", json!(W3_OLD_FINGERPRINT)),
    ] {
        let mut file = ready[0].file.clone();
        file["recorded"][0][key] = wrong;
        m15_write_json(&case.path, &file).unwrap();
        assert!(
            matches!(
                w3_effort_plan(&temp.0, None).unwrap()[0].replay,
                Replay::Stale
            ),
            "{key}"
        );
    }
    m15_write_json(&case.path, &ready[0].file).unwrap();
    assert!(matches!(
        w3_effort_plan(&temp.0, Some("https://example.invalid".into())).unwrap()[0].replay,
        Replay::Stale
    ));
    let old = w3_comparison_plan(&temp.0.join("af96"), None).unwrap();
    let output = fake_w3(&old[0].sample);
    save_recording(
        &old[0],
        &fake_envelope(output.clone()),
        Some(&output.to_string()),
        None,
    )
    .unwrap();
    let old = w3_comparison_plan(&temp.0.join("af96"), None).unwrap();
    m15_write_json(&case.path, &old[0].file).unwrap();
    assert!(!matches!(
        w3_effort_plan(&temp.0, None).unwrap()[0].replay,
        Replay::Output(_)
    ));
}

#[test]
fn af101_w3_effort_failure_budget_and_candidate_report_replay_without_retry() {
    let temp = TempRecords::new();
    let cases = w3_effort_plan(&temp.0, None).unwrap();
    let mut outputs = cases.iter().enumerate().map(|(i,c)| {
        if i == 0 { Err("injected failure".into()) }
        else if i == 1 { Ok(json!({"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},
            "usage":{"input_tokens":20,"output_tokens":16384,"output_tokens_details":{"reasoning_tokens":16000}}})) }
        else {
            let mut envelope = fake_envelope(fake_w3(&c.sample));
            envelope["usage"] = json!({"input_tokens":2000,"output_tokens":150,
                "output_tokens_details":{"reasoning_tokens":40}});
            Ok(envelope)
        }
    });
    let report = evaluate(&cases, Some(16), &mut |_| outputs.next().unwrap()).unwrap();
    assert_eq!(report["sent_calls"], 16);
    assert_eq!(w3_sent_before(&temp.0).unwrap(), 16);
    assert_eq!(report["results"][0]["status"], "调用失败");
    assert_eq!(
        report["results"][1]["response"]["incomplete_details"]["reason"],
        "max_output_tokens"
    );
    assert_eq!(
        report["results"][8]["candidate_reviews"][0]["verdict"],
        "insufficient"
    );
    assert_eq!(
        report["results"][8]["candidate_reviews"][0]["aspects"],
        json!(["subject"])
    );
    assert_eq!(
        report["results"][8]["candidate_reviews"][1]["verdict"],
        "supported"
    );
    assert_eq!(
        report["results"][12]["candidate_reviews"][0]["aspects"],
        json!(["subject"])
    );
    let comparison = w3_effort_report(&report);
    assert_eq!(comparison["arms"][0]["failed"], 1);
    assert_eq!(comparison["arms"][1]["failed"], 1);
    assert_eq!(
        comparison["arms"][1]["usage_totals"]["output_tokens_including_reasoning"],
        17434
    );
    assert_eq!(
        comparison["arms"][1]["usage_totals"]["reasoning_tokens"],
        16280
    );
    for entry in std::fs::read_dir(temp.0.join("sends")).unwrap() {
        let log: Value =
            serde_json::from_slice(&std::fs::read(entry.unwrap().path()).unwrap()).unwrap();
        assert_eq!(log["state"], "responded");
        assert_eq!(log["trial"], "af101-w3-effort");
        assert!(matches!(
            log["effort_arm"].as_str(),
            Some("minimal" | "low")
        ));
        assert!(log["prompt_arm"].is_null());
    }
    let ready = w3_effort_plan(&temp.0, None).unwrap();
    let replay = evaluate(&ready, Some(0), &mut |_| {
        panic!("must not retry any response")
    })
    .unwrap();
    assert_eq!(replay["sent_calls"], 0);
    assert_eq!(replay["results"], report["results"]);
    std::fs::remove_file(&ready[1].path).unwrap();
    let gap = w3_effort_plan(&temp.0, None).unwrap();
    assert!(preflight(&gap, Some(16)).is_err());
    assert!(preflight(&gap[1..2], Some(16)).is_err());
}

#[test]
fn af101_w3_effort_resume_crash_lock_and_af96_ledger_are_independent() {
    let temp = TempRecords::new();
    let old_root = temp.0.join("af96");
    let new_root = temp.0.join("af101");
    let old = w3_comparison_plan(&old_root, None).unwrap();
    for case in &old {
        w3_start_send(case).unwrap();
    }
    assert_eq!(w3_sent_before(&old_root).unwrap(), 16);
    assert!(preflight(&old, Some(16)).is_err());
    let cases = w3_effort_plan(&new_root, None).unwrap();
    assert_eq!(preflight(&cases, Some(16)).unwrap(), 16);
    let lock = w3_trial_lock(&new_root).unwrap();
    assert!(evaluate(&cases, Some(16), &mut |_| panic!("concurrent send")).is_err());
    drop(lock);
    let mut outputs = cases[..2]
        .iter()
        .map(|c| Ok(fake_envelope(fake_w3(&c.sample))));
    evaluate(&cases[..2], Some(2), &mut |_| outputs.next().unwrap()).unwrap();
    let resumed = w3_effort_plan(&new_root, None).unwrap();
    assert_eq!(preflight(&resumed, Some(14)).unwrap(), 14);
    let path = w3_start_send(&resumed[2]).unwrap();
    assert_eq!(w3_sent_before(&new_root).unwrap(), 3);
    let log: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(log["state"], "started");
    assert_eq!(log["effort_arm"], "low");
    assert!(preflight(&resumed, Some(16)).is_err());
    let replay = evaluate(&resumed, None, &mut |_| panic!("offline crash replay")).unwrap();
    assert_eq!(replay["sent_calls"], 0);
    assert_eq!(replay["results"][2]["status"], "未录制");
    assert_eq!(w3_sent_before(&old_root).unwrap(), 16);
    let mut mixed = old;
    mixed.extend(cases);
    assert!(preflight(&mixed, Some(32)).is_err());
}

#[test]
fn af97_w3_plan_has_sixteen_frozen_slots_and_rejects_fifteen() {
    let temp = TempRecords::new();
    let cases = w3_comparison_plan(&temp.0, None).unwrap();
    assert_eq!(cases.len(), 16);
    assert!(preflight(&cases, Some(15)).is_err());
    assert_eq!(preflight(&cases, Some(16)).unwrap(), 16);
    assert_eq!(preflight(&cases, None).unwrap(), 0);
    assert!(!temp.0.exists()); // Planning/preflight require neither credentials nor writable output.
    assert_eq!(
        cases
            .iter()
            .map(|c| &c.path)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        16
    );
    for pair in cases.chunks_exact(4) {
        assert_eq!(
            pair.iter()
                .map(|c| c.sample["w3_prompt_arm"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["old", "new", "new", "old"]
        );
        for case in pair {
            assert_eq!(case.stage, "w3");
            assert_eq!(case.request["reasoning"]["effort"], "minimal");
            assert_eq!(case.request["model"], "ep-20261003130756-trhj6");
            assert_eq!(case.request["max_output_tokens"], 16384);
            assert_eq!(case.request["input"], pair[0].request["input"]);
            let input: Value =
                serde_json::from_str(case.request["input"].as_str().unwrap()).unwrap();
            for key in [
                "model",
                "focus",
                "expect",
                "repeat",
                "w3_prompt_arm",
                "fictional",
                "label_status",
            ] {
                assert!(input.get(key).is_none(), "{key}");
            }
            assert_eq!(input["user_history"], case.sample["input"]["history"]);
            assert!(
                items(&input["candidates"])
                    .iter()
                    .all(|c| c["status"] == "ok")
            );
            assert!(
                case.request["instructions"]
                    .as_str()
                    .unwrap()
                    .ends_with(w3_arm_prompt(&case.sample))
            );
        }
        let mut a = pair[0].request.clone();
        let mut b = pair[1].request.clone();
        assert_ne!(a["instructions"], b["instructions"]);
        a.as_object_mut().unwrap().remove("instructions");
        b.as_object_mut().unwrap().remove("instructions");
        assert_eq!(a, b);
    }
    assert_eq!(w3_prompt_fingerprint(W3_OLD_PROMPT), W3_OLD_FINGERPRINT);
}

#[test]
fn af97_w3_full_fingerprint_and_arm_repeat_are_strict() {
    let temp = TempRecords::new();
    let cases = w3_comparison_plan(&temp.0, None).unwrap();
    let case = &cases[0];
    let original = recording_fingerprint(&case.sample, &case.request);
    for path in [
        "/instructions",
        "/model",
        "/text/format/schema",
        "/input",
        "/reasoning/effort",
        "/max_output_tokens",
    ] {
        let mut changed = case.request.clone();
        *changed.pointer_mut(path).unwrap() = if path == "/input" {
            json!(json!({"changed":true}).to_string())
        } else {
            json!("changed")
        };
        assert_ne!(
            recording_fingerprint(&case.sample, &changed),
            original,
            "{path}"
        );
    }
    let mut changed = case.request.clone();
    let mut input: Value = serde_json::from_str(changed["input"].as_str().unwrap()).unwrap();
    input["trusted_at"] = json!("clock remains bound");
    changed["input"] = json!(input.to_string());
    assert_ne!(recording_fingerprint(&case.sample, &changed), original);
    let output = fake_w3(&case.sample);
    save_recording(
        case,
        &fake_envelope(output.clone()),
        Some(&output.to_string()),
        None,
    )
    .unwrap();
    let ready = w3_comparison_plan(&temp.0, None).unwrap();
    assert!(matches!(ready[0].replay, Replay::Output(_)));
    assert!(matches!(ready[1].replay, Replay::Missing)); // New arm cannot borrow old response.
    assert!(matches!(ready[3].replay, Replay::Missing)); // Repeat 2 cannot borrow repeat 1.
    let other_base = w3_comparison_plan(&temp.0, Some("https://example.invalid".into())).unwrap();
    assert!(matches!(other_base[0].replay, Replay::Stale));
    let mut wrong = ready[0].file.clone();
    wrong["recorded"][0]["repeat"] = json!(2);
    m15_write_json(&case.path, &wrong).unwrap();
    assert!(matches!(
        w3_comparison_plan(&temp.0, None).unwrap()[0].replay,
        Replay::Stale
    ));
    wrong["recorded"][0]["repeat"] = json!(1);
    wrong["recorded"][0]["prompt_arm"] = json!("new");
    m15_write_json(&case.path, &wrong).unwrap();
    assert!(matches!(
        w3_comparison_plan(&temp.0, None).unwrap()[0].replay,
        Replay::Stale
    ));
}

#[test]
fn af97_w3_failed_attempts_replay_and_consume_the_same_sixteen_budget() {
    let temp = TempRecords::new();
    let cases = w3_comparison_plan(&temp.0, None).unwrap();
    let mut outputs = cases.iter().enumerate().map(|(i, c)| {
        if i == 0 {
            Err("injected failure".into())
        } else {
            Ok(fake_envelope(fake_w3(&c.sample)))
        }
    });
    let report = evaluate(&cases, Some(16), &mut |_| outputs.next().unwrap()).unwrap();
    assert_eq!(report["sent_calls"], 16);
    assert_eq!(w3_sent_before(&temp.0).unwrap(), 16);
    for entry in std::fs::read_dir(temp.0.join("sends")).unwrap() {
        let log: Value =
            serde_json::from_slice(&std::fs::read(entry.unwrap().path()).unwrap()).unwrap();
        assert_eq!(log["state"], "responded");
        assert!(matches!(log["prompt_arm"].as_str(), Some("old" | "new")));
        assert!(matches!(log["repeat"].as_u64(), Some(1 | 2)));
        assert!(log["sample"].as_str().unwrap().starts_with("af96_w3_"));
    }
    assert_eq!(report["results"][0]["status"], "调用失败");
    assert_eq!(w3_comparison_report(&report)["arms"][0]["failed"], 1);
    let ready = w3_comparison_plan(&temp.0, None).unwrap();
    let replay = evaluate(&ready, Some(0), &mut |_| {
        panic!("must not retry failed slot")
    })
    .unwrap();
    assert_eq!(replay["sent_calls"], 0);
    assert_eq!(replay["results"], report["results"]);
    // Removing a response does not restore its already consumed authorization.
    std::fs::remove_file(&ready[1].path).unwrap();
    let gap = w3_comparison_plan(&temp.0, None).unwrap();
    assert!(preflight(&gap, Some(16)).is_err());
    assert!(preflight(&gap[1..2], Some(16)).is_err()); // Arm/subset uses the whole root ledger.
}

#[test]
fn af97_w3_interruption_successful_resume_and_concurrent_lock() {
    let temp = TempRecords::new();
    let cases = w3_comparison_plan(&temp.0, None).unwrap();
    let lock = w3_trial_lock(&temp.0).unwrap();
    assert!(evaluate(&cases, Some(16), &mut |_| panic!("concurrent send")).is_err());
    drop(lock);
    let report = evaluate(&cases[..2], Some(2), &mut |_| {
        Ok(fake_envelope(fake_w3(&cases[0].sample)))
    })
    .unwrap();
    assert_eq!(report["sent_calls"], 2);
    let resumed = w3_comparison_plan(&temp.0, None).unwrap();
    assert_eq!(preflight(&resumed, Some(14)).unwrap(), 14);
    let path = w3_start_send(&resumed[2]).unwrap(); // Simulated crash after durable started.
    assert_eq!(w3_sent_before(&temp.0).unwrap(), 3);
    assert_eq!(
        serde_json::from_slice::<Value>(&std::fs::read(path).unwrap()).unwrap()["state"],
        "started"
    );
    assert!(preflight(&resumed, Some(16)).is_err());
    let replay = evaluate(&resumed, None, &mut |_| {
        panic!("offline interruption replay")
    })
    .unwrap();
    assert_eq!(replay["sent_calls"], 0);
    assert_eq!(replay["results"][2]["status"], "未录制");
}

#[test]
fn m15_w1_plan_is_24_and_rejects_23_before_send() {
    let cases = m15_w1_plan(
        &std::env::temp_dir().join(format!("m15-plan-{}", hippocampus::diagnostics::new_id())),
    )
    .unwrap();
    assert_eq!(cases.len(), 24);
    assert!(cases.iter().all(|c| c.stage == "w1"));
    assert!(preflight(&cases, Some(23)).is_err());
    assert_eq!(preflight(&cases, Some(24)).unwrap(), 24);
    for c in cases {
        let input: Value = serde_json::from_str(c.request["input"].as_str().unwrap()).unwrap();
        assert!(input.get("model").is_none());
        assert!(input.get("review_requirements").is_none());
    }
}

fn eval_request_timeout(request: &Value) -> std::time::Duration {
    let input: Value = request["input"]
        .as_str()
        .and_then(|text| serde_json::from_str(text).ok())
        .unwrap_or(Value::Null);
    let op = serde_json::from_value(input["plan"]["plan"]["op"].clone()).ok();
    let compute =
        hippocampus::recall_models::RecallModels::default().r4_effort(op) == ReasoningEffort::Low;
    std::time::Duration::from_secs(
        if request["text"]["format"]["name"] == "r4_v6"
            && request["reasoning"]["effort"] == "low"
            && compute
        {
            240
        } else {
            180
        },
    )
}

#[test]
fn evaluation_timeout_is_240_only_for_r4_compute_low() {
    for (stage, effort, op, seconds) in [
        ("r4_v6", "low", "count", 240),
        ("r4_v6", "low", "sum", 240),
        ("r4_v6", "low", "elapsed", 240),
        ("r4_v6", "low", "compare", 240),
        ("r4_v6", "low", "lookup", 180),
        ("r4_v6", "low", "list", 180),
        ("r4_v6", "low", "invalid", 180),
        ("r4_v6", "minimal", "count", 180),
        ("r4_v6", "high", "count", 180),
        ("r1_v2", "low", "count", 180),
        ("w3_v3", "low", "count", 180),
    ] {
        let request = json!({"text":{"format":{"name":stage}},"reasoning":{"effort":effort},
            "input":json!({"plan":{"plan":{"op":op}}}).to_string()});
        assert_eq!(
            eval_request_timeout(&request),
            std::time::Duration::from_secs(seconds)
        );
    }
}

fn eval_gate(value: Option<&str>) -> Result<bool, String> {
    match value {
        None | Some("0") => Ok(false),
        Some("1") => Ok(true),
        _ => Err("EVAL_GATE only accepts 0 or 1".into()),
    }
}

fn check_report(report: &Value, gate: bool) -> Result<(), String> {
    if items(&report["summary"])
        .iter()
        .any(|s| s["invalid_input"] != 0 || (gate && s["failed"] != 0))
    {
        Err("invalid sample input or gated model failure".into())
    } else {
        Ok(())
    }
}

struct TempRecords(PathBuf);
impl TempRecords {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        Self(std::env::temp_dir().join(format!(
            "hippocampus-stage-eval-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        )))
    }
}
impl Drop for TempRecords {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn first_cases(root: &Path) -> Vec<Case> {
    plan(
        ["w1", "w3", "w4"]
            .into_iter()
            .map(|stage| (stage.into(), samples(stage).remove(0)))
            .collect(),
        root,
    )
    .unwrap()
}

fn fake_envelope(output: Value) -> Value {
    json!({"status": "completed", "output": [{"type": "message", "content": [{"type": "output_text", "text": output.to_string()}]}],
        "usage": {"input_tokens": 12, "output_tokens": 7}})
}

fn fake_w1(sample: &Value) -> Value {
    let candidates: Vec<_> = items(&sample["expect"]["candidates"])
        .iter()
        .map(|e| {
            let mut c = json!({"subject": "self", "polarity": "affirm", "strength": null,
            "uncertainty": null, "conditions": [], "times": []});
            for (key, value) in e.as_object().unwrap() {
                c[key] = value.clone();
            }
            for key in [
                "subject",
                "relation",
                "memory_type",
                "value",
                "sentence",
                "polarity",
            ] {
                if c[key].is_array() {
                    c[key] = c[key][0].clone();
                }
            }
            for time in c["times"].as_array_mut().unwrap() {
                if time["role"].is_array() {
                    time["role"] = time["role"][0].clone();
                }
            }
            adapter::candidate(&c)
        })
        .collect();
    let requests: Vec<_> = items(&sample["expect"]["memory_requests"]).iter().map(|r| json!({
        "kind": r["kind"], "quote": adapter::quote(&r["quote"]), "target": {
            "object": if r["target_object"].is_array() { &r["target_object"][0] } else { &r["target_object"] },
            "candidate_indices": r["candidate_indices"],
            "quote": adapter::quote(r.get("target_quote").unwrap_or(&r["quote"]))
        }, "deadline": adapter::time(&r["deadline"])
    })).collect();
    json!({"candidates": candidates, "memory_requests": requests})
}

#[test]
fn corrected_w1_labels_match_injected_outputs_and_draft_is_visible() {
    for sample in samples("w1") {
        let report = score("w1", &sample, Some(&fake_w1(&sample).to_string()));
        assert_eq!(report["status"], "通过", "{}: {report}", sample["id"]);
    }
    let mut sample = samples("w1").remove(0);
    let request = stage_request("w1", &sample).unwrap();
    sample["label_status"] = json!("draft");
    let report = score("w1", &sample, Some(&fake_w1(&sample).to_string()));
    assert_eq!(report["label_status"], "draft");
    assert_eq!(
        fingerprint(&stage_request("w1", &sample).unwrap()),
        fingerprint(&request),
        "label approval is not model input"
    );
}

#[test]
fn acceptable_time_roles_and_request_objects_keep_other_semantics_strict() {
    let sample = samples("w1")
        .into_iter()
        .find(|s| s["id"] == "w1_17_unknown_time")
        .unwrap();
    for role in ["validity", "event", "retention"] {
        let mut output = fake_w1(&sample);
        output["candidates"][0]["times"][0]["role"] = json!(role);
        let report = score("w1", &sample, Some(&output.to_string()));
        assert_eq!(
            report["status"],
            if role == "retention" {
                "失败"
            } else {
                "通过"
            },
            "{report}"
        );
    }
    let mut wrong = fake_w1(&sample);
    wrong["candidates"][0]["times"][0]["interp"] =
        json!({"kind":"relative","unit":"year","offset":-1});
    assert_eq!(
        score("w1", &sample, Some(&wrong.to_string()))["status"],
        "失败"
    );
    for id in ["w1_10_remember_binds_one", "w1_11_retention_deadline"] {
        let sample = samples("w1").into_iter().find(|s| s["id"] == id).unwrap();
        for object in ["facts", "source", "unknown"] {
            let mut output = fake_w1(&sample);
            output["memory_requests"][0]["target"]["object"] = json!(object);
            let report = score("w1", &sample, Some(&output.to_string()));
            assert_eq!(
                report["status"],
                if object == "unknown" {
                    "失败"
                } else {
                    "通过"
                },
                "{id}: {report}"
            );
        }
    }
}

#[test]
fn offline_replay_and_limit_refusal_never_invoke_sender() {
    let temp = TempRecords::new();
    let cases = first_cases(&temp.0);
    let report = evaluate(&cases, None, &mut |_| panic!("offline sender invoked")).unwrap();
    assert_eq!(report["sent_calls"], 0);
    assert!(
        items(&report["results"])
            .iter()
            .all(|r| r["status"] == "未录制")
    );
    let error = evaluate(&cases, Some(2), &mut |_| {
        panic!("sender invoked before full preflight")
    })
    .unwrap_err();
    assert!(error.contains("planned calls 3 exceed"));
    assert!(!temp.0.exists(), "preflight must not mutate recordings");
}

#[test]
fn injected_recordings_roundtrip_preserve_other_models_and_replay_without_sends() {
    let temp = TempRecords::new();
    let mut cases = first_cases(&temp.0);
    for case in &mut cases {
        case.file["recorded"] = json!([{"model": "Other-model", "output": "keep me"}]);
    }
    let mut outputs = cases
        .iter()
        .map(|case| match case.stage.as_str() {
            "w1" => fake_w1(&case.sample),
            "w3" => fake_w3(&case.sample),
            _ => serde_json::from_str(
                &std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
                    "tests/eval/w4/fixtures/{}.output.json",
                    case.sample["id"].as_str().unwrap()
                )))
                .unwrap(),
            )
            .unwrap(),
        })
        .map(fake_envelope);
    let recorded = evaluate(&cases, Some(3), &mut |_| Ok(outputs.next().unwrap())).unwrap();
    assert_eq!(recorded["sent_calls"], 3); // Injected attempts, not network calls.
    assert!(items(&recorded["summary"]).iter().all(|s| s["passed"] == 1));
    let replayed = evaluate(&first_cases(&temp.0), None, &mut |_| {
        panic!("replay sender invoked")
    })
    .unwrap();
    assert_eq!(recorded["results"], replayed["results"]);
    assert_eq!(replayed["sent_calls"], 0);
    assert_eq!(preflight(&first_cases(&temp.0), Some(0)).unwrap(), 0);
    for case in first_cases(&temp.0) {
        assert_eq!(items(&case.file["recorded"]).len(), 2);
        assert_eq!(case.file["recorded"][0]["model"], "Other-model");
        assert_eq!(case.file["recorded"][1]["usage"]["input_tokens"], 12);
    }
}

#[test]
fn fingerprints_pin_prompt_schema_input_model_and_reject_stale_outputs() {
    let temp = TempRecords::new();
    let mut cases = first_cases(&temp.0);
    for case in &mut cases {
        let original = fingerprint(&case.request);
        for path in ["/instructions", "/input", "/model", "/text/format/schema"] {
            let mut changed = case.request.clone();
            *changed.pointer_mut(path).unwrap() = json!("changed");
            assert_ne!(fingerprint(&changed), original, "{path}");
        }
        let mut allowance = case.request.clone();
        allowance["max_output_tokens"] = json!(10000);
        assert_eq!(fingerprint(&allowance), original);
        for request in [Value::Null, json!("old-fingerprint")] {
            std::fs::create_dir_all(&temp.0).unwrap();
            std::fs::write(&case.path, json!({"recorded": [{"model": MODEL,
                "schema": case.request["text"]["format"]["name"], "request": request, "output": {"never": "grade this"}}]}).to_string()).unwrap();
            let stale = plan(vec![(case.stage.clone(), case.sample.clone())], &temp.0).unwrap();
            let report = evaluate(&stale, None, &mut |_| {
                panic!("stale offline sender invoked")
            })
            .unwrap();
            assert_eq!(report["results"][0]["status"], "录制过期");
            assert_eq!(
                report["summary"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|s| s["stale"].as_u64().unwrap())
                    .sum::<u64>(),
                1
            );
        }
        std::fs::write(&case.path, json!({"recorded": null}).to_string()).unwrap();
        assert!(plan(vec![(case.stage.clone(), case.sample.clone())], &temp.0).is_err());
    }
}

#[test]
fn eval_output_limits_preserve_recorded_fingerprints() {
    assert_eq!(eval_output_tokens(None).unwrap(), 16384);
    assert_eq!(eval_output_tokens(Some("32768")).unwrap(), 32768);
    for invalid in ["", "0", "-1", "abc", "4294967296"] {
        assert!(eval_output_tokens(Some(invalid)).is_err(), "{invalid}");
    }
    let temp = TempRecords::new();
    for case in first_cases(&temp.0) {
        let original = fingerprint(&case.request);
        for tokens in [4096, 16384, 32768] {
            let request =
                stage_request_with_output_tokens(&case.stage, &case.sample, tokens).unwrap();
            assert_eq!(request["max_output_tokens"], tokens);
            assert_eq!(fingerprint(&request), original);
        }
        std::fs::create_dir_all(&temp.0).unwrap();
        let output = match case.stage.as_str() {
            "w1" => fake_w1(&case.sample),
            "w3" => fake_w3(&case.sample),
            _ => serde_json::from_str(
                &std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
                    "tests/eval/w4/fixtures/{}.output.json",
                    case.sample["id"].as_str().unwrap()
                )))
                .unwrap(),
            )
            .unwrap(),
        };
        save_recording(
            &case,
            &fake_envelope(output.clone()),
            Some(&output.to_string()),
            None,
        )
        .unwrap();
        let replay = plan(vec![(case.stage, case.sample)], &temp.0).unwrap();
        assert!(matches!(replay[0].replay, Replay::Output(_)));
        let report = evaluate(&replay, None, &mut |_| panic!("offline sender invoked")).unwrap();
        assert_eq!(report["sent_calls"], 0);
    }
}

#[test]
fn model_scores_are_report_only_unless_explicitly_gated() {
    assert!(!eval_gate(None).unwrap());
    assert!(!eval_gate(Some("0")).unwrap());
    assert!(eval_gate(Some("1")).unwrap());
    for value in ["", "true", "2"] {
        assert!(eval_gate(Some(value)).is_err());
    }
    let failed = json!({"summary": [{"failed": 1, "invalid_input": 0}]});
    assert!(check_report(&failed, false).is_ok());
    assert!(check_report(&failed, true).is_err());
    let invalid = json!({"summary": [{"failed": 0, "invalid_input": 1}]});
    assert!(check_report(&invalid, false).is_err());
    let stale = json!({"summary": [{"failed": 0, "invalid_input": 0, "stale": 61}]});
    assert!(check_report(&stale, true).is_ok());
}

#[test]
fn w1_scores_final_w2_candidates_and_independent_fields() {
    let sample = samples("w1").remove(0);
    let mut output = fake_w1(&sample);
    let sentence = output["candidates"][0]["sentence"]["text"]
        .as_str()
        .unwrap();
    output["candidates"][0]["sentence"]["text"] = json!(format!("{sentence}\\\"}}garbage"));
    let repaired = score("w1", &sample, Some(&output.to_string()));
    assert_eq!(repaired["status"], "通过", "{repaired}");
    assert_ne!(
        repaired["raw"]["candidates"][0]["sentence"],
        repaired["w2_final"]["candidates"][0]["candidate"]["sentence"]
    );
    output = fake_w1(&sample);
    output["candidates"][0]["value"]["text"] = json!(sample["input"]["text"]);
    output["candidates"][0]["value"]["quote"] = adapter::quote(&sample["input"]["text"]);
    let wrong = score("w1", &sample, Some(&output.to_string()));
    assert_eq!(wrong["status"], "失败");
    for key in ["subject", "relation", "memory_type", "polarity", "sentence"] {
        let field = format!("candidates[0].{key}");
        assert_eq!(
            items(&wrong["fields"])
                .iter()
                .find(|f| f["field"] == field)
                .unwrap()["passed"],
            true,
            "{wrong}"
        );
    }
    let examples = samples("w1");
    let negation = examples
        .iter()
        .find(|s| s["id"].as_str().unwrap().starts_with("w1_15"))
        .unwrap();
    let mut output = fake_w1(negation);
    output["candidates"][0]["value"]["text"] = negation["input"]["text"].clone();
    output["candidates"][0]["value"]["quote"] = adapter::quote(&negation["input"]["text"]);
    let report = score("w1", negation, Some(&output.to_string()));
    assert_eq!(report["status"], "失败");
    assert_eq!(
        items(&report["fields"])
            .iter()
            .find(|f| f["field"] == "candidates[0].relation+polarity")
            .unwrap()["passed"],
        true
    );
    let request = examples
        .iter()
        .find(|s| s["id"].as_str().unwrap().starts_with("w1_10"))
        .unwrap();
    let mut output = fake_w1(request);
    output["candidates"][0]["polarity"] = json!("unknown");
    let report = score("w1", request, Some(&output.to_string()));
    assert_eq!(report["status"], "失败");
    assert_eq!(
        items(&report["fields"])
            .iter()
            .find(|f| f["field"] == "memory_requests[0].candidate_indices")
            .unwrap()["passed"],
        true
    );
}

#[test]
fn w4_relation_resolution_is_scored_across_the_whole_group() {
    let sample = samples("w4")
        .into_iter()
        .find(|s| s["id"].as_str().unwrap().starts_with("w4_21"))
        .unwrap();
    let mut output: Value = serde_json::from_str(
        &std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
            "tests/eval/w4/fixtures/{}.output.json",
            sample["id"].as_str().unwrap()
        )))
        .unwrap(),
    )
    .unwrap();
    output["decisions"][0]["resolves_relations"] =
        output["decisions"][1]["resolves_relations"].clone();
    output["decisions"][1]["resolves_relations"] = json!([]);
    assert_eq!(
        score("w4", &sample, Some(&output.to_string()))["status"],
        "通过"
    );
    output["decisions"][1]["resolves_relations"] =
        output["decisions"][0]["resolves_relations"].clone();
    assert_eq!(
        score("w4", &sample, Some(&output.to_string()))["status"],
        "失败"
    );
    output["decisions"][0]["resolves_relations"] = json!([]);
    output["decisions"][1]["resolves_relations"] = json!([]);
    assert_eq!(
        score("w4", &sample, Some(&output.to_string()))["status"],
        "失败"
    );
}

#[test]
fn failed_or_incomplete_attempts_are_bounded_recorded_and_never_retried() {
    let temp = TempRecords::new();
    let cases = first_cases(&temp.0);
    let mut attempts = 0;
    let report = evaluate(&cases, Some(3), &mut |_| {
        attempts += 1;
        match attempts {
            1 => Err("injected HTTP 429".into()),
            2 => Ok(json!({"status": "incomplete", "output": [],
                "incomplete_details": {"reason": "max_output_tokens"},
                "usage": {"input_tokens": 12, "output_tokens": 16384,
                    "output_tokens_details": {"reasoning_tokens": 16384}}})),
            _ => Ok(json!({"status": "completed", "output": [{"content": [{"type": "refusal"}]}]})),
        }
    })
    .unwrap();
    assert_eq!(attempts, 3);
    assert_eq!(report["sent_calls"], 3);
    assert!(
        items(&report["results"])
            .iter()
            .all(|r| r["status"] == "调用失败")
    );
    assert_eq!(
        report["results"][1]["response"],
        json!({"status": "incomplete",
        "incomplete_details": {"reason": "max_output_tokens"},
        "usage": {"input_tokens": 12, "output_tokens": 16384,
            "output_tokens_details": {"reasoning_tokens": 16384}}})
    );
    assert!(report["results"][0]["response"]["status"].is_null());
    let replay = evaluate(&first_cases(&temp.0), Some(0), &mut |_| {
        panic!("failed attempt retried")
    })
    .unwrap();
    assert_eq!(replay["results"], report["results"]);
    assert_eq!(replay["sent_calls"], 0);
    let missing_temp = TempRecords::new();
    let missing_cases = first_cases(&missing_temp.0);
    let missing_details = evaluate(&missing_cases[..1], Some(1), &mut |_| {
        Ok(json!({"status": "incomplete", "output": []}))
    })
    .unwrap();
    assert_eq!(missing_details["sent_calls"], 1);
    assert_eq!(
        missing_details["results"][0]["response"]["status"],
        "incomplete"
    );
    assert!(missing_details["results"][0]["response"]["incomplete_details"].is_null());
    assert!(missing_details["results"][0]["response"]["usage"].is_null());
    let invalid_temp = TempRecords::new();
    let cases = first_cases(&invalid_temp.0);
    let invalid = json!({"status": "completed", "output": [{"content": [{"type": "output_text", "text": "{"}]}]});
    let recorded = evaluate(&cases, Some(3), &mut |_| Ok(invalid.clone())).unwrap();
    assert!(
        items(&recorded["results"])
            .iter()
            .all(|r| r["status"] == "失败")
    );
    let replayed = evaluate(&first_cases(&invalid_temp.0), None, &mut |_| {
        panic!("invalid-output replay must not retry")
    })
    .unwrap();
    assert_eq!(recorded["results"], replayed["results"]);
    assert_eq!(replayed["sent_calls"], 0);
}

#[test]
fn json_string_response_does_not_turn_into_a_valid_object_on_replay() {
    let temp = TempRecords::new();
    let cases = first_cases(&temp.0);
    let quoted = serde_json::to_string(&fake_w1(&cases[0].sample).to_string()).unwrap();
    let envelope = json!({"status": "completed", "output": [{"content": [{"type": "output_text", "text": quoted}]}]});
    let recorded = evaluate(&cases[..1], Some(1), &mut |_| Ok(envelope.clone())).unwrap();
    assert_eq!(recorded["results"][0]["status"], "失败");
    let ready = first_cases(&temp.0);
    let replayed = evaluate(&ready[..1], None, &mut |_| panic!("must not resend")).unwrap();
    assert_eq!(recorded["results"], replayed["results"]);
}

#[test]
fn stage_and_prefix_filters_skip_non_w4_code_paths() {
    assert_eq!(selected("w1,w3,w4", "").unwrap().len(), 61);
    assert_eq!(selected("w4", "").unwrap().len(), 21);
    assert!(selected("w4", "w4_06,w4_08").unwrap().is_empty());
    let subset = selected("w3", "w3_01,w3_02").unwrap();
    assert_eq!(subset.len(), 2);
    assert!(subset.iter().all(|(stage, _)| stage == "w3"));
    assert!(selected("w2", "").is_err());
    assert!(selected("w1", "w1_01,").is_err());
}

#[test]
fn dry_run_plan() {
    let stages = stages_from_env();
    let prefix = std::env::var("EVAL_CASES").unwrap_or_default();
    let root = recording_root();
    let model = EvalModel::from_env().unwrap();
    let repeats = repeat_count(std::env::var("EVAL_REPEAT").ok().as_deref()).unwrap();
    let cases = plan_repeated(selected(&stages, &prefix).unwrap(), &root, &model, repeats).unwrap();
    let mut total = 0;
    println!("分阶段评测干跑计划（不发送请求）");
    println!(
        "模型: {}，常规推理强度: {}",
        model.label,
        serde_json::to_value(model.effort)
            .unwrap()
            .as_str()
            .unwrap()
    );
    for stage in ["w1", "w3", "w4", "r1", "r4"] {
        if !stages.split(',').any(|s| s.trim() == stage) {
            continue;
        }
        let count = cases.iter().filter(|c| c.stage == stage).count();
        let needed = cases
            .iter()
            .filter(|c| c.stage == stage && matches!(c.replay, Replay::Missing | Replay::Stale))
            .count();
        total += count;
        println!("{stage}: {count} 次潜在真实调用，需录制 {needed}");
        if stage.starts_with('r') {
            for split in ["dev", "holdout"] {
                let n = cases
                    .iter()
                    .filter(|c| c.stage == stage && c.sample["split"] == split)
                    .count();
                println!("  {split}: {n} 次潜在真实调用");
            }
        }
    }
    println!("合计: {total} 次潜在真实调用");
    if fixed_inputs_enabled() {
        let classifier = hippocampus::recall_models::RecallModels {
            reasoning_effort: [("r4".into(), ReasoningEffort::Minimal)].into(),
            ..Default::default()
        };
        let compute = cases
            .iter()
            .filter(|c| {
                c.stage == "r4"
                    && classifier.r4_effort(Some(fixed_r4_op(&c.sample).unwrap()))
                        == ReasoningEffort::Low
            })
            .count();
        let other = cases.iter().filter(|c| c.stage == "r4").count() - compute;
        println!(
            "R4 分流：计算类 {compute} 次 low，非计算类 {other} 次常规强度；依据冻结输入的已核验 R1 plan.op。"
        );
        if std::env::var("EVAL_FIXED_INPUTS").as_deref() == Ok("af11") {
            let usage: Vec<_> = cases
                .iter()
                .filter_map(|c| {
                    Some((
                        c.sample["original_usage"]["input_tokens"].as_u64()?,
                        c.sample["original_usage"]["output_tokens"].as_u64()?,
                    ))
                })
                .collect();
            println!(
                "AF11 原请求历史usage：已知{}/{}，输入{}、输出{} token；提示词/event证据已改变，仅作参考，不是新调用预测或费用上限。",
                usage.len(),
                cases.len(),
                usage.iter().map(|u| u.0).sum::<u64>(),
                usage.iter().map(|u| u.1).sum::<u64>()
            );
        } else {
            println!(
                "R4 输出估算：计算类 {:.0} token（low 40次均值2895.5），其余沿用同输入 minimal 实测；单次延迟参考 low p50≈41.8s、minimal≈4.5s。",
                compute as f64 * 2895.5
            );
            println!("完整 token/延迟估算：python3 tests/eval/fixed_inputs/prepare.py（零调用）。");
        }
    }
    println!(
        "本次需录制: {}",
        cases
            .iter()
            .filter(|c| !matches!(c.replay, Replay::Output(_) | Replay::Failed(_)))
            .count()
    );
    println!("本次真实调用: 0");
    if std::env::var("EVAL_FIXED_INPUTS").as_deref() == Ok("af11") {
        for case in &cases {
            println!(
                "{} {}: {}，max_output_tokens={}，请求指纹={}",
                case.stage,
                case.sample["id"].as_str().unwrap(),
                serde_json::to_value(case.model.effort)
                    .unwrap()
                    .as_str()
                    .unwrap(),
                case.request["max_output_tokens"],
                recording_fingerprint(&case.sample, &case.request)
                    .as_str()
                    .unwrap()
            );
        }
        println!(
            "AF11：固定输入独立阶段试验，不作为新端到端通过；以下仅模拟容量预检，不发送请求。"
        );
        let limit = std::env::var("EVAL_RECORD").ok().map(|s| {
            s.parse::<usize>()
                .expect("EVAL_RECORD must be a nonnegative integer")
        });
        preflight(&cases, limit).unwrap();
    }
}

#[test]
fn injected_w1_candidate_fields_and_request_fields_fail_individually() {
    let shorthand = json!({"kind": "assertion", "speech": "user_statement", "subject": "self", "relation": "likes",
        "memory_type": "preference", "value": "爬山", "sentence": "我可能在晚上喜欢爬山。", "polarity": "affirm",
        "strength": "weak", "uncertainty": "可能", "conditions": ["晚上"], "times": []});
    let request = json!({"kind": "retention", "quote": "保存到明天", "target": {"object": "facts", "candidate_indices": [0],
        "quote": "爬山"}, "deadline": {"role": "retention", "quote": "明天", "interp": {"kind": "relative", "unit": "day", "offset": 1}}});
    let output = serde_json::to_value(
        adapter::expand(&json!({"candidates": [shorthand], "requests": [request]})).unwrap(),
    )
    .unwrap();
    let sample = json!({"id": "synthetic_w1", "label_status": "draft", "input": {
        "text": "我可能在晚上喜欢爬山。保存到明天，也提到游泳。", "said_on": "2026-09-20",
        "entities": [{"id": "self"}, {"id": "mom"}], "relations": ["likes", "uses"]},
        "expect": {"candidate_count": 1, "candidates": [shorthand], "memory_requests": [{"kind": "retention", "quote": "保存到明天",
            "target_object": "facts", "candidate_indices": [0], "target_quote": "爬山", "deadline": request["deadline"]}]}});
    assert_eq!(
        score("w1", &sample, Some(&output.to_string()))["status"],
        "通过"
    );
    for (field, wrong) in [
        ("kind", json!("retraction")),
        ("speech", json!("reported")),
        ("subject", json!({"entity_id": "mom"})),
        ("relation", json!("uses")),
        ("memory_type", json!("fact")),
        (
            "value",
            json!({"kind": "text", "text": "游泳", "quote": {"text": "游泳"}}),
        ),
        ("sentence", json!({"text": "也提到游泳。"})),
        ("polarity", json!("negate")),
        ("strength", json!("strong")),
        ("uncertainty", Value::Null),
        ("conditions", json!([])),
        (
            "times",
            json!([{"role": "event", "quote": {"text": "明天"}, "interp": {"kind": "relative", "unit": "day", "offset": 1}, "anchor": "said_on"}]),
        ),
    ] {
        let mut wrong_output = output.clone();
        wrong_output["candidates"][0][field] = wrong;
        let report = score("w1", &sample, Some(&wrong_output.to_string()));
        assert_eq!(report["status"], "失败", "{field}: {report}");
        assert!(
            items(&report["fields"])
                .iter()
                .any(|f| f["field"] == format!("candidates[0].{field}") && f["passed"] == false)
        );
    }
    for (path, wrong) in [
        ("/memory_requests/0/kind", json!("remember")),
        ("/memory_requests/0/quote/text", json!("保存")),
        ("/memory_requests/0/target/object", json!("source")),
        ("/memory_requests/0/target/candidate_indices", json!([])),
        ("/memory_requests/0/target/quote/text", json!("游泳")),
        ("/memory_requests/0/deadline/interp/offset", json!(2)),
    ] {
        let mut wrong_output = output.clone();
        *wrong_output.pointer_mut(path).unwrap() = wrong;
        assert_eq!(
            score("w1", &sample, Some(&wrong_output.to_string()))["status"],
            "失败",
            "{path}"
        );
    }
    let mut duplicate = output.clone();
    duplicate["candidates"]
        .as_array_mut()
        .unwrap()
        .push(output["candidates"][0].clone());
    assert_eq!(
        score("w1", &sample, Some(&duplicate.to_string()))["status"],
        "失败"
    );
}

#[test]
fn all_w3_sample_inputs_pass_w2() {
    for sample in samples("w3") {
        let output = adapter::expand(&sample["input"]).unwrap();
        let batch = batch(output, &sample["input"]).unwrap();
        assert!(
            batch
                .candidates
                .iter()
                .all(|c| c.status != w2::Status::Rejected),
            "{}: {:?}",
            sample["id"],
            batch
        );
        assert_eq!(
            batch.memory_requests.len(),
            items(&sample["input"]["requests"]).len()
        );
        assert_eq!(
            score("w3", &sample, Some(&fake_w3(&sample).to_string()))["status"],
            "通过",
            "{}",
            sample["id"]
        );
    }
}

#[test]
fn negation_uses_labelled_relation_polarity_pairs_only() {
    let sample = samples("w1")
        .into_iter()
        .find(|s| s["id"] == "w1_15_negation")
        .unwrap();
    for (relation, polarity, expected) in [
        ("avoids", "affirm", "通过"),
        ("likes", "negate", "通过"),
        ("avoids", "negate", "失败"),
        ("likes", "affirm", "失败"),
    ] {
        let mut candidate = sample["expect"]["candidates"][0].clone();
        candidate["relation"] = json!(relation);
        candidate["polarity"] = json!(polarity);
        candidate["memory_type"] = json!("preference");
        let output = json!({"candidates": [adapter::candidate(&candidate)], "memory_requests": []});
        assert_eq!(
            score("w1", &sample, Some(&output.to_string()))["status"],
            expected,
            "{relation}+{polarity}"
        );
    }
}

#[test]
fn injected_three_stages_pass_decode_failure_and_missing() {
    for stage in ["w1", "w3", "w4"] {
        let sample = samples(stage).remove(0);
        let response = match stage {
            "w1" => fake_w1(&sample).to_string(),
            "w3" => json!({"reviews": [{"candidate": 0, "verdict": "supported", "problems": [], "episode_value": "not_applicable"}],
                "coverage": {"status": "complete", "missing": []}, "request_reviews": []}).to_string(),
            _ => std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("tests/eval/w4/fixtures/{}.output.json", sample["id"].as_str().unwrap()))).unwrap(),
        };
        assert_eq!(score(stage, &sample, Some(&response))["status"], "通过");
        assert_eq!(score(stage, &sample, Some("{"))["status"], "失败");
        assert_eq!(score(stage, &sample, None)["status"], "未录制");
    }
}

fn fake_w3(sample: &Value) -> Value {
    json!({"reviews": items(&sample["expect"]["reviews"]).iter().map(|r| json!({
        "candidate": r["candidate"], "verdict": r["verdict"], "episode_value": r["episode_value"],
        "problems": items(&r["aspects"]).iter().map(|a| json!({"aspect": a,
            "quote": {"text": sample["input"]["text"]}})).collect::<Vec<_>>()
    })).collect::<Vec<_>>(), "coverage": {"status": sample["expect"]["coverage"]["status"],
        "missing": items(&sample["expect"]["coverage"]["missing_contains"]).iter().map(adapter::quote).collect::<Vec<_>>()},
        "request_reviews": sample["expect"]["request_reviews"]})
}

#[test]
fn injected_w3_fields_fail_individually() {
    let samples = samples("w3");
    for (prefix, path, wrong) in [
        ("w3_01", "/reviews/0/verdict", json!("insufficient")),
        ("w3_02", "/reviews/0/problems", json!([])),
        ("w3_13", "/reviews/0/episode_value", json!("low_value")),
        ("w3_11", "/coverage/status", json!("complete")),
        ("w3_11", "/coverage/missing", json!([])),
        ("w3_15", "/request_reviews/0/verdict", json!("contradicted")),
        ("w3_12", "/reviews", json!([])),
    ] {
        let sample = samples
            .iter()
            .find(|s| s["id"].as_str().unwrap().starts_with(prefix))
            .unwrap();
        let mut output = fake_w3(sample);
        assert_eq!(
            score("w3", sample, Some(&output.to_string()))["status"],
            "通过"
        );
        *output.pointer_mut(path).unwrap() = wrong;
        assert_eq!(
            score("w3", sample, Some(&output.to_string()))["status"],
            "失败",
            "{prefix} {path}"
        );
    }
    let sample = samples
        .iter()
        .find(|s| s["id"].as_str().unwrap().starts_with("w3_02"))
        .unwrap();
    let mut output = fake_w3(sample);
    output["reviews"][0]["problems"]
        .as_array_mut()
        .unwrap()
        .push(json!({"aspect": "value", "quote": {"text": "花生"}}));
    assert_eq!(
        score("w3", sample, Some(&output.to_string()))["status"],
        "通过",
        "额外 aspect 不扣分"
    );
}

#[test]
fn injected_w4_fields_fail_and_all_fixtures_validate() {
    let samples = samples("w4");
    for sample in samples.iter().filter(|s| s["call_w4"] == true) {
        let text = std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(
            format!(
                "tests/eval/w4/fixtures/{}.output.json",
                sample["id"].as_str().unwrap()
            ),
        ))
        .unwrap();
        assert_eq!(
            score("w4", sample, Some(&text))["status"],
            "通过",
            "{}",
            sample["id"]
        );
    }
    for (prefix, key, wrong) in [
        ("w4_01", "status", json!("pending")),
        ("w4_01", "action", json!("coexist")),
        ("w4_01", "targets", json!([])),
        ("w4_15", "resolves_relations", json!([])),
        ("w4_20", "pending_reason", json!("scope_incomplete")),
        ("w4_01", "basis", json!([])),
    ] {
        let sample = samples
            .iter()
            .find(|s| s["id"].as_str().unwrap().starts_with(prefix))
            .unwrap();
        let text = std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(
            format!(
                "tests/eval/w4/fixtures/{}.output.json",
                sample["id"].as_str().unwrap()
            ),
        ))
        .unwrap();
        let mut output: Value = serde_json::from_str(&text).unwrap();
        output["decisions"][0][key] = wrong;
        assert_eq!(
            score("w4", sample, Some(&output.to_string()))["status"],
            "失败",
            "{prefix} {key}"
        );
    }
}

#[test]
fn calibrated_quotes_accept_only_u4_equivalences() {
    for (field, a, e, source, pass) in [
        (
            "conditions",
            "我练琴时",
            "练琴时",
            "我练琴时用节拍器。",
            true,
        ),
        (
            "sentence",
            "我使用节拍器",
            "我使用节拍器，",
            "我使用节拍器，然后录音。",
            true,
        ),
        (
            "quote",
            "请记住这件事。",
            "请记住这件事",
            "请记住这件事。",
            true,
        ),
        (
            "sentence",
            "我吃饭然后散步。",
            "我吃饭，然后散步。",
            "我吃饭，然后散步。",
            false,
        ),
        (
            "target_quote",
            "住址",
            "我住址那条",
            "请忘掉我住址那条。",
            false,
        ),
        ("conditions", "练琴", "练琴时", "我练琴时用节拍器。", false),
        (
            "conditions",
            "他练琴时",
            "练琴时",
            "他练琴时用节拍器。",
            false,
        ),
        (
            "sentence",
            "我不使用节拍器。",
            "我使用节拍器。",
            "我不使用节拍器。",
            false,
        ),
        (
            "quote",
            "记住这件事。。",
            "记住这件事",
            "记住这件事。。",
            false,
        ),
    ] {
        assert_eq!(
            quote_matches(&json!(a), &json!(e), field, source),
            pass,
            "{field}: {a} / {e}"
        );
    }
    let sample = samples("w1")
        .into_iter()
        .find(|s| s["id"].as_str().unwrap().starts_with("w1_03"))
        .unwrap();
    let mut output = fake_w1(&sample);
    output["candidates"][0]["conditions"][0]["quote"]["text"] = json!("我画速写的时候");
    assert_eq!(
        score("w1", &sample, Some(&output.to_string()))["status"],
        "通过"
    );
    output["candidates"][0]["conditions"] = json!([]);
    let bad = score("w1", &sample, Some(&output.to_string()));
    assert_eq!(bad["status"], "失败");
    assert_eq!(bad["categories"]["semantic"]["passed"], false);
    let expected = json!({"sentence":"女儿练琴时戴耳机。","conditions":["练琴时"]});
    let actual = json!({"sentence":"女儿练琴时戴耳机。","conditions":["我练琴时"]});
    assert!(
        !candidate_matches(&actual, &expected, "我练琴时用节拍器。女儿练琴时戴耳机。"),
        "the optional subject must belong to this assertion, not another sentence"
    );
    let request = samples("w1")
        .into_iter()
        .find(|s| s["id"].as_str().unwrap().starts_with("w1_09"))
        .unwrap();
    let mut output = fake_w1(&request);
    output["memory_requests"][0]["quote"] = adapter::quote(&request["input"]["text"]);
    assert_eq!(
        score("w1", &request, Some(&output.to_string()))["status"],
        "通过"
    );
}

#[test]
fn w4_assembly_reports_raw_and_final_without_relaxing_validation() {
    let sample = samples("w4").remove(0);
    let input: W4Input = serde_json::from_value(sample["input"].clone()).unwrap();
    let mut output: W4Output = serde_json::from_str(
        &std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
            "tests/eval/w4/fixtures/{}.output.json",
            sample["id"].as_str().unwrap()
        )))
        .unwrap(),
    )
    .unwrap();
    output.decisions[0]
        .basis
        .retain(|e| e.source != input.candidates[0].source);
    let text_of = |id: &str| sample["sources"][id]["text"].as_str().map(str::to_owned);
    assert!(w4::validate(&input, &output, &text_of).is_err());
    let final_output = w4::prepare_output(&input, &output, &text_of).unwrap();
    assert_eq!(
        w4::prepare_output(&input, &final_output, &text_of).unwrap(),
        final_output
    );
    let report = score(
        "w4",
        &sample,
        Some(&serde_json::to_string(&output).unwrap()),
    );
    assert_eq!(report["raw_result"]["status"], "失败");
    assert_eq!(report["final_result"]["status"], "通过");
    assert!(
        w4::prepare_output(&input, &output, &|_| None).is_err(),
        "deleted source cannot be copied"
    );
    let mut bad_input = input.clone();
    bad_input.candidates[0].candidate.sentence.text = "not present in source".into();
    assert!(w4::prepare_output(&bad_input, &output, &text_of).is_err());
    let mut outsider = output.clone();
    outsider.decisions[0].candidate = usize::MAX;
    assert!(w4::prepare_output(&input, &outsider, &text_of).is_err());
    output.decisions[0].basis.clear();
    assert!(
        w4::prepare_output(&input, &output, &text_of).is_err(),
        "missing old basis still invalid"
    );
    let pending_sample = samples("w4")
        .into_iter()
        .find(|s| s["id"].as_str().unwrap().starts_with("w4_20"))
        .unwrap();
    let mut pending: Value = serde_json::from_str(
        &std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/eval/w4/fixtures/w4_20_pending_target_unknown.output.json"),
        )
        .unwrap(),
    )
    .unwrap();
    pending["decisions"][0]["basis"] = json!([]);
    let report = score("w4", &pending_sample, Some(&pending.to_string()));
    assert_eq!(report["final_result"]["status"], "通过");
    assert!(!items(&report["final_output"]["decisions"][0]["basis"]).is_empty());
}

#[test]
fn coexist_without_targets_does_not_require_old_basis() {
    let sample = samples("w4")
        .into_iter()
        .find(|s| s["id"].as_str().unwrap().starts_with("w4_04"))
        .unwrap();
    let mut output: Value = serde_json::from_str(
        &std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/eval/w4/fixtures/w4_04_coexist_partial.output.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let own = &sample["input"]["candidates"][0]["source"]["source_id"];
    output["decisions"][0]["basis"]
        .as_array_mut()
        .unwrap()
        .retain(|b| &b["source"]["source_id"] == own);
    assert_eq!(
        score("w4", &sample, Some(&output.to_string()))["raw_result"]["status"],
        "通过"
    );
}

#[test]
fn repeats_models_and_caps_are_independent_and_all_attempts_count() {
    assert_eq!(repeat_count(None).unwrap(), 1);
    for bad in ["0", "-1", "", "x"] {
        assert!(repeat_count(Some(bad)).is_err());
    }
    let temp = TempRecords::new();
    let mut model = EvalModel::default_model();
    model.label = "comparison-A".into();
    model.model = Some("model-a".into());
    let chosen = selected("w1", "w1_01").unwrap();
    let cases = plan_repeated(chosen.clone(), &temp.0, &model, 2).unwrap();
    assert_ne!(cases[0].path, cases[1].path);
    let mut calls = 0;
    let report = evaluate(&cases, Some(2), &mut |_| {
        let mut output = fake_w1(&cases[0].sample);
        calls += 1;
        if calls == 2 {
            output["candidates"][0]["polarity"] = json!("unknown");
        }
        Ok(fake_envelope(output))
    })
    .unwrap();
    assert_eq!(report["per_sample"][0]["passed"], 1);
    assert_eq!(report["per_sample"][0]["total"], 2);
    assert_eq!(report["per_sample"][0]["error_categories"]["semantic"], 1);
    let ready = plan_repeated(chosen.clone(), &temp.0, &model, 2).unwrap();
    assert_eq!(
        evaluate(&ready, Some(0), &mut |_| panic!("repeat resent")).unwrap()["results"],
        report["results"]
    );
    let mut other = model.clone();
    other.label = "comparison-B".into();
    assert_eq!(
        preflight(
            &plan_repeated(chosen.clone(), &temp.0, &other, 2).unwrap(),
            Some(2)
        )
        .unwrap(),
        2
    );
    other = model.clone();
    other.model = Some("different-model".into());
    assert!(
        plan_repeated(chosen.clone(), &temp.0, &other, 2)
            .unwrap()
            .iter()
            .all(|c| matches!(c.replay, Replay::Stale))
    );
    other = model.clone();
    other.base = Some("https://example.invalid".into());
    assert!(
        plan_repeated(chosen.clone(), &temp.0, &other, 2)
            .unwrap()
            .iter()
            .all(|c| matches!(c.replay, Replay::Stale))
    );
    let cap_temp = TempRecords::new();
    let mut cases = plan_repeated(chosen.clone(), &cap_temp.0, &model, 1).unwrap();
    cases[0].request["max_output_tokens"] = json!(4096);
    let output = fake_w1(&cases[0].sample);
    save_recording(
        &cases[0],
        &fake_envelope(output.clone()),
        Some(&output.to_string()),
        None,
    )
    .unwrap();
    assert!(matches!(
        plan_repeated(chosen, &cap_temp.0, &model, 1).unwrap()[0].replay,
        Replay::Stale
    ));
    assert_eq!(
        plan_repeated(selected("w1,w4", "").unwrap(), &temp.0, &model, 2)
            .unwrap()
            .len(),
        82
    );
    for label in ["../escape", "/tmp/x", ".", ""] {
        let mut bad = model.clone();
        bad.label = label.into();
        assert!(bad.validate().is_err());
    }
    for stage in ["w1", "w3", "w4"] {
        assert_eq!(
            stage_request(stage, &samples(stage).remove(0)).unwrap()["max_output_tokens"],
            if stage == "w1" { 32768 } else { 16384 }
        );
    }
}

#[test]
fn single_eval_model_overrides_only_api_routing_for_all_selected_roles() {
    let temp = TempRecords::new();
    let mut model = EvalModel::default_model();
    model.model = Some("evaluation-model".into());
    let cases = plan_repeated(
        selected("w1,w3,w4", "w1_01,w3_01,w4_01").unwrap(),
        &temp.0,
        &model,
        1,
    )
    .unwrap();
    assert_eq!(cases.len(), 3);
    for case in cases {
        let mut request = case.request;
        assert_eq!(
            request.as_object_mut().unwrap().remove("model"),
            Some(json!("evaluation-model"))
        );
        assert!(
            !request.to_string().contains("evaluation-model"),
            "{}",
            case.stage
        );
        assert!(
            !request.to_string().contains(hippocampus::DEEPSEEK_MODEL),
            "{}",
            case.stage
        );
        let payload: Value = serde_json::from_str(request["input"].as_str().unwrap()).unwrap();
        assert!(payload.get("model").is_none(), "{}", case.stage);
    }
}

#[test]
fn adding_w4_input_model_field_expires_recordings_even_with_the_same_prompt_and_endpoint() {
    let temp = TempRecords::new();
    let mut model = EvalModel::default_model();
    model.model = Some("evaluation-model".into());
    let chosen = selected("w4", "w4_01").unwrap();
    let mut case = plan_repeated(chosen.clone(), &temp.0, &model, 1)
        .unwrap()
        .remove(0);
    let current = case.request.clone();
    let mut polluted_payload: Value =
        serde_json::from_str(current["input"].as_str().unwrap()).unwrap();
    assert!(polluted_payload.get("model").is_none());
    polluted_payload["model"] = current["model"].clone();
    case.request["input"] = json!(polluted_payload.to_string());
    assert_eq!(case.request["instructions"], current["instructions"]);
    assert_eq!(case.request["model"], current["model"]);
    assert_ne!(case.request, current, "the JSON actually sent is different");
    assert_ne!(fingerprint(&case.request), fingerprint(&current));
    let text = json!({"decisions":[]}).to_string();
    save_recording(
        &case,
        &fake_envelope(json!({"decisions":[]})),
        Some(&text),
        None,
    )
    .unwrap();
    let replay = plan_repeated(chosen, &temp.0, &model, 1).unwrap();
    assert!(matches!(replay[0].replay, Replay::Stale));
    let report = evaluate(&replay, None, &mut |_| {
        panic!("stale recording caused a send")
    })
    .unwrap();
    assert_eq!(report["sent_calls"], 0);
    assert_eq!(report["results"][0]["status"], "录制过期");
}

fn fake_r1(sample: &Value) -> Value {
    fn pick(value: &Value) -> Value {
        if value.is_array() {
            value[0].clone()
        } else {
            value.clone()
        }
    }
    let e = &sample["expect"];
    let subject = pick(&e["subject"]);
    let subject = if subject.is_string() {
        json!({"entity_id":subject})
    } else if subject.get("unresolved").is_some() {
        json!({"unresolved":{"text":subject["unresolved"]}})
    } else {
        subject
    };
    let time = if e["time"].is_null() {
        Value::Null
    } else {
        json!({"role":"event","quote":{"text":e["time"]["quote"]},"interp":e["time"]["interp"],"anchor":"said_on"})
    };
    json!({"need":pick(&e["need"]),"current_evidence":pick(&e["current_evidence"]),"type":pick(&e["type"]),"subject":subject,
        "relation":"stage query relation","conditions":items(&e["conditions"]).iter().map(|q|json!({"quote":{"text":pick(q)},"resolved":true})).collect::<Vec<_>>(),
        "time":time,"plan":{"op":pick(&e["plan.op"]),"filter":{"relation":pick(&e["plan.filter.relation"]),"time":null},"unit":if e.get("unit").is_some(){pick(&e["unit"])}else{Value::Null}}})
}
fn fake_r4(sample: &Value) -> Value {
    let f = recall_adapter::R4Input::new(&sample["input"]).unwrap();
    let mut facts = vec![];
    for e in items(&sample["expect"]["facts"]) {
        let basis:Vec<_>=items(&e["sources"]).iter().map(|id|{
            if id=="current" {json!({"source":{"kind":"current"},"quote":{"text":f.request.query}})}else{
                let source=id.as_str().unwrap();
                let text=f.evidence.raw.iter().find(|r|r.source_id==source).map(|r|r.quote.text.clone()).or_else(||f.evidence.records.iter().flat_map(|r| match r {
                    RecordView::Current(r)=>r.evidence.clone(),RecordView::Legacy(r)=>r.evidence.clone() }).find_map(|e|matches!(&e.source,SourceRef::Stored {source_id} if source_id==source).then_some(e.quote.text))).unwrap();
                json!({"source":{"kind":"stored","source_id":source},"quote":{"text":text}})
            }
        }).collect();
        let mut fact = json!({"statement":basis[0]["quote"]["text"],"evidence":basis,"record_ids":e["record_ids"],"polarity":"affirm","strength":null,
            "uncertainty":null,"conditions":[],"times":[],"current":"yes","applies":"yes","supersedes_record_ids":[]});
        for (key, v) in e["must"].as_object().unwrap() {
            let v = if v.is_array() && key != "conditions" {
                &v[0]
            } else {
                v
            };
            fact[key] = match key.as_str() {
                "uncertainty" => json!({"quote":{"text":v},"resolved":true}),
                "conditions" => json!(
                    items(v)
                        .iter()
                        .map(|v| json!({"quote":{"text":v},"resolved":true}))
                        .collect::<Vec<_>>()
                ),
                _ => v.clone(),
            };
        }
        facts.push(fact);
    }
    let mut compute = Value::Null;
    let expected = &sample["expect"]["compute"];
    if !expected.is_null() {
        let op = expected["op"].as_str().unwrap();
        let mut inputs:Vec<_>=f.evidence.records.iter().enumerate().map(|(i,r)|{
            let evidence=match r {RecordView::Current(r)=>r.evidence.clone(),RecordView::Legacy(r)=>r.evidence.clone()};
            json!({"id":r.id().0.to_string(),"evidence":evidence,"quantity":null,"date":null,"date_source":null,
                "event_group":if op=="count" && expected["distinct_groups"]==2 && i<2 {"same".to_owned()}else{format!("g{i}")}})
        }).collect();
        if op == "sum" {
            for (i, item) in inputs.iter_mut().enumerate() {
                item["quantity"] =
                    json!({"amount":expected["quantities"][i],"unit":expected["unit"]});
            }
        }
        if op == "elapsed" || op == "compare" {
            let dates = if op == "compare" {
                vec![
                    (
                        "2024年3月",
                        json!({"kind":"month","year_offset":0,"month":3}),
                    ),
                    (
                        "2025年",
                        json!({"kind":"relative","unit":"year","offset":0}),
                    ),
                ]
            } else if expected["insufficient_ok"] == true {
                vec![("七月", json!({"kind":"month","year_offset":0,"month":7}))]
            } else {
                vec![("7月1日", json!({"kind":"date","date":"2026-07-01"}))]
            };
            for (item, (quote, interp)) in inputs.iter_mut().zip(dates) {
                item["date"] = json!({"role":"event","quote":{"text":quote},"interp":interp,"anchor":"said_on"});
                item["date_source"] = item["evidence"][0]["source"].clone();
            }
        }
        let operands = match op {
            "elapsed" => json!(["@request.at", inputs[0]["id"]]),
            "compare" => json!([inputs[0]["id"], inputs[1]["id"]]),
            _ => json!([]),
        };
        compute = json!({"op":op,"items":inputs,"operands":operands,"unit":if op=="elapsed"{json!("day")}else{expected["unit"].clone()},
            "coverage":if expected.get("coverage").is_some(){expected["coverage"][0].clone()}else{json!("complete")}});
    }
    json!({"verdict":if sample["expect"]["verdict"].is_array(){sample["expect"]["verdict"][0].clone()}else{sample["expect"]["verdict"].clone()},"facts":facts,"compute":compute})
}
#[test]
fn recall_stage_all_inputs_expand_to_frozen_packages_without_labels_or_model_ids() {
    for (stage, sample) in selected("r1,r4", "").unwrap() {
        let request = stage_request(&stage, &sample).unwrap();
        let input: Value = serde_json::from_str(request["input"].as_str().unwrap()).unwrap();
        assert!(input.get("model").is_none());
        assert!(input.get("expect").is_none());
        assert!(input.get("split").is_none());
        assert!(!input.to_string().contains(sample["id"].as_str().unwrap()));
        if stage == "r4" {
            let evidence: RecallEvidence =
                serde_json::from_value(input["evidence"].clone()).unwrap();
            for record in evidence.records {
                if let RecordView::Current(r) = record {
                    assert!(
                        !serde_json::to_value(r)
                            .unwrap()
                            .as_object()
                            .unwrap()
                            .contains_key("end")
                    );
                }
            }
        }
    }
}
#[test]
fn recall_stage_calibrated_injections_validate_and_score_all_fields() {
    for (stage, sample) in selected("r1,r4", "").unwrap() {
        let output = if stage == "r1" {
            fake_r1(&sample)
        } else {
            fake_r4(&sample)
        };
        let report = score(&stage, &sample, Some(&output.to_string()));
        assert_eq!(report["status"], "通过", "{}: {report}", sample["id"]);
        assert_eq!(report["split"], sample["split"]);
    }
}
#[test]
fn recall_stage_wrong_fields_and_missing_sources_fail_without_lenient_semantics() {
    let sample = &samples("r1")[0];
    let good = fake_r1(sample);
    for field in [
        "need",
        "current_evidence",
        "type",
        "subject",
        "conditions",
        "time",
        "plan.op",
        "plan.filter.relation",
    ] {
        let mut bad = good.clone();
        match field {
            "subject" => bad[field] = json!({"entity_id":"absent"}),
            "conditions" => bad[field] = json!([{"quote":{"text":"absent"},"resolved":true}]),
            "time" => {
                bad[field] = json!({"role":"event","quote":{"text":"absent"},"anchor":"said_on","interp":{"kind":"unknown"}})
            }
            "plan.op" => bad["plan"]["op"] = json!("count"),
            "plan.filter.relation" => bad["plan"]["filter"]["relation"] = json!("absent"),
            "need" => bad[field] = json!("context_only"),
            "current_evidence" => bad[field] = json!("assertion_present"),
            _ => bad[field] = json!("aggregate"),
        }
        assert_eq!(
            score("r1", sample, Some(&bad.to_string()))["status"],
            "失败",
            "{field}"
        );
    }
    let sample = &samples("r4")[0];
    let good = fake_r4(sample);
    for (field, value) in [
        ("polarity", json!("negate")),
        ("record_ids", json!([999])),
        ("current", json!("no")),
    ] {
        let mut bad = good.clone();
        bad["facts"][0][field] = value;
        assert_eq!(
            score("r4", sample, Some(&bad.to_string()))["status"],
            "失败"
        );
    }
    let mut bad = good;
    bad["facts"][0]["evidence"][0]["source"]["source_id"] = json!("absent");
    assert_eq!(
        score("r4", sample, Some(&bad.to_string()))["status"],
        "失败"
    );
}

#[test]
fn r4_reports_validation_rules_and_fields_without_changing_production_errors() {
    use hippocampus::r4;
    let sample = &samples("r4")[0];
    let good = fake_r4(sample);
    let f = recall_adapter::R4Input::new(&sample["input"]).unwrap();
    let ctx = r4::Context {
        evidence: &f.evidence,
        plan: &f.plan,
        request: &f.request,
        utc_offset_seconds: 28_800,
        source_dates: &f.source_dates,
    };
    for (pointer, value, rule, field, cause) in [
        (
            "/facts/0/record_ids",
            json!([999]),
            "record_id",
            "facts[0].record_ids[0]",
            None,
        ),
        (
            "/facts/0/evidence",
            json!([]),
            "empty_evidence",
            "facts[0].evidence",
            None,
        ),
        (
            "/facts/0/evidence/0/quote/text",
            json!("not in any source"),
            "quote_or_source",
            "facts[0].evidence[0]",
            None,
        ),
        (
            "/facts/0/uncertainty",
            json!({"quote":{"text":"not in any source"},"resolved":false}),
            "qualifier_source",
            "facts[0].uncertainty.quote",
            None,
        ),
        (
            "/facts/0/times",
            json!([{"role":"event","quote":{"text":"not in any source"},"interp":{"kind":"unknown"},"anchor":"said_on"}]),
            "fact_time",
            "facts[0].times[0]",
            Some("quote_or_source"),
        ),
    ] {
        let mut bad = good.clone();
        *bad.pointer_mut(pointer).unwrap() = value;
        let raw = bad.to_string();
        let report = score_raw("r4", sample, Some(&raw));
        let diagnostic = &report["fields"][0]["actual"];
        assert_eq!(
            diagnostic,
            &json!({"code":"model_invalid_output","rule":rule,"field":field,"cause":cause})
        );
        let decoded = r4::decode(&raw).unwrap();
        let detailed = r4::evaluate_detailed(decoded.clone(), &ctx).err().unwrap();
        assert_eq!(r4::evaluate(decoded, &ctx).err(), Some(detailed.error));
        assert_eq!(detailed.error.to_string(), "model_invalid_output");
    }
    let schema = score_raw("r4", sample, Some("{}"));
    assert_eq!(
        schema["fields"][0]["actual"],
        json!({"code":"model_invalid_output","rule":"schema","field":"$","cause":null})
    );
    let sample = samples("r4")
        .into_iter()
        .find(|s| s["id"].as_str().unwrap().starts_with("r4_16_"))
        .unwrap();
    let mut bad = fake_r4(&sample);
    bad["compute"]["operands"] = json!([]);
    let report = score_raw("r4", &sample, Some(&bad.to_string()));
    assert_eq!(
        report["fields"][0]["actual"],
        json!({"code":"model_invalid_output","rule":"operands","field":"compute.operands","cause":null})
    );
    let sample = samples("r4")
        .into_iter()
        .find(|s| s["id"].as_str().unwrap().starts_with("r4_04_"))
        .unwrap();
    let mut bad = fake_r4(&sample);
    // 另一条事实携带旧方依据，不满足替代事实自身的新旧依据要求。
    bad["facts"][0]["supersedes_record_ids"] = json!([4]);
    let report = score_raw("r4", &sample, Some(&bad.to_string()));
    assert_eq!(
        report["fields"][0]["actual"],
        json!({"code":"model_invalid_output","rule":"record_basis","field":"facts[0].supersedes_record_ids[0]","cause":null})
    );
}

#[test]
fn r4_assembly_reports_strict_raw_and_final_scores_and_replays_identically() {
    let sample = samples("r4")
        .into_iter()
        .find(|s| s["id"].as_str().unwrap().starts_with("r4_04_"))
        .unwrap();
    let mut missing = fake_r4(&sample);
    missing["facts"][0]["supersedes_record_ids"] = json!([4]);
    let raw = format!(" \n{}\n ", missing);
    let row = score("r4", &sample, Some(&raw));
    assert_eq!(row["raw_result"]["status"], "失败");
    assert_eq!(
        row["raw_result"]["fields"][0]["actual"]["rule"],
        "record_basis"
    );
    assert_eq!(row["final_result"]["status"], "通过");
    assert_eq!(row["raw_output_text"], raw);
    assert_eq!(
        row["raw_output"]["facts"][0]["evidence"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        row["final_output"]["facts"][0]["evidence"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    for (pointer, value, count, rule) in [
        (
            "/facts/0/evidence/0/quote/text",
            json!("不在原文的引用"),
            1,
            "quote_or_source",
        ),
        (
            "/facts/0/conditions",
            json!([{"quote":{"text":"不在原文的条件"},"resolved":true}]),
            2,
            "qualifier_source",
        ),
    ] {
        let mut bad = missing.clone();
        *bad.pointer_mut(pointer).unwrap() = value;
        let row = score("r4", &sample, Some(&bad.to_string()));
        assert_eq!(row["final_result"]["status"], "失败");
        assert_eq!(row["final_result"]["fields"][0]["actual"]["rule"], rule);
        assert_eq!(
            row["final_output"]["facts"][0]["evidence"]
                .as_array()
                .unwrap()
                .len(),
            count
        );
    }
    let dir = TempRecords::new();
    let cases = plan(vec![("r4".into(), sample.clone())], &dir.0).unwrap();
    let first = evaluate(&cases, Some(1), &mut |_| Ok(fake_envelope(missing.clone()))).unwrap();
    assert_eq!(
        first["raw_summary"].as_array().unwrap().last().unwrap()["passed"],
        0
    );
    assert_eq!(
        first["summary"].as_array().unwrap().last().unwrap()["passed"],
        1
    );
    let replay = plan(vec![("r4".into(), sample)], &dir.0).unwrap();
    let replay = evaluate(&replay, None, &mut |_| {
        panic!("offline replay must not send")
    })
    .unwrap();
    assert_eq!(replay["sent_calls"], 0);
    for key in ["raw_summary", "summary", "raw_splits", "splits", "results"] {
        assert_eq!(first[key], replay[key], "{key}");
    }
}

#[test]
fn recall_stage_recording_replay_splits_and_clock_fingerprints_are_complete() {
    let temp = TempRecords::new();
    let selected = selected("r1,r4", "").unwrap();
    assert_eq!(selected.len(), 40);
    let cases = plan(selected.clone(), &temp.0).unwrap();
    assert_eq!(preflight(&cases, None).unwrap(), 0);
    assert!(preflight(&cases, Some(39)).is_err());
    let report = evaluate(&cases, Some(40), &mut |request| {
        let case = cases.iter().find(|c| c.request == *request).unwrap();
        Ok(fake_envelope(if case.stage == "r1" {
            fake_r1(&case.sample)
        } else {
            fake_r4(&case.sample)
        }))
    })
    .unwrap();
    for row in items(&report["splits"]) {
        assert_eq!(row["total"], if row["split"] == "dev" { 14 } else { 6 });
        assert_eq!(row["passed"], row["total"]);
    }
    let replay = evaluate(&plan(selected, &temp.0).unwrap(), None, &mut |_| {
        panic!("recall replay sent")
    })
    .unwrap();
    assert_eq!(replay["results"], report["results"]);
    assert_eq!(replay["sent_calls"], 0);
    let mut request = cases
        .iter()
        .find(|c| c.stage == "r4")
        .unwrap()
        .request
        .clone();
    let original = fingerprint(&request);
    let mut input: Value = serde_json::from_str(request["input"].as_str().unwrap()).unwrap();
    input["request"]["at"] = json!(0);
    request["input"] = json!(input.to_string());
    assert_ne!(original, fingerprint(&request));
}

#[test]
fn reasoning_effort_changes_all_stage_requests_fingerprints_and_recording_paths() {
    let temp = TempRecords::new();
    for (stage, sample) in selected("w1,w3,w4,r1,r4", "w1_01,w3_01,w4_01,r1_01,r4_01").unwrap() {
        let original = stage_request(&stage, &sample).unwrap();
        let mut default = original.clone();
        ReasoningEffort::default().apply(&mut default);
        assert_eq!(default.to_string(), original.to_string());
        for effort in [
            ReasoningEffort::Low,
            ReasoningEffort::High,
            ReasoningEffort::Minimal,
            ReasoningEffort::Omit,
        ] {
            let mut request = original.clone();
            effort.apply(&mut request);
            assert_ne!(fingerprint(&request), fingerprint(&original));
            assert_eq!(request["input"], original["input"]);
            assert_eq!(request["instructions"], original["instructions"]);
            if effort == ReasoningEffort::Omit {
                assert!(request.get("reasoning").is_none());
            } else {
                assert_eq!(request["reasoning"]["effort"], json!(effort));
            }
        }
    }
    let chosen = selected("r1,r4", "").unwrap();
    let medium = EvalModel::default_model();
    let low = EvalModel {
        effort: ReasoningEffort::Low,
        ..medium.clone()
    };
    let original = plan_repeated(chosen.clone(), &temp.0, &medium, 2).unwrap();
    let changed = plan_repeated(chosen, &temp.0, &low, 2).unwrap();
    assert_eq!(changed.len(), 80);
    assert_eq!(
        changed
            .iter()
            .filter(|c| c.sample["split"] == "dev")
            .count(),
        56
    );
    assert!(!original[0].path.to_string_lossy().contains("reasoning-"));
    assert!(changed[0].path.to_string_lossy().contains("reasoning-low"));
    assert_ne!(original[0].path, changed[0].path);
    assert_eq!(preflight(&changed, Some(80)).unwrap(), 80);
    assert!(preflight(&changed, Some(79)).is_err());
}

#[test]
fn existing_r1_recordings_become_stale_after_prompt_changes_without_sends() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/eval/write_stage/recordings");
    for (label, endpoint, effort) in [
        (MODEL, hippocampus::DEEPSEEK_MODEL, ReasoningEffort::Medium),
        (
            "DeepSeek-V4-Pro",
            "ep-20261003130756-trhj6",
            ReasoningEffort::Medium,
        ),
        (
            "DeepSeek-V4-Pro",
            "ep-20261003130756-trhj6",
            ReasoningEffort::Minimal,
        ),
    ] {
        let model = EvalModel {
            label: label.into(),
            model: Some(endpoint.into()),
            base: Some("https://ark.cn-beijing.volces.com/api/v3".into()),
            effort,
        };
        // B 改了 R1 的真实请求；旧录制不能当新提示词结果，仍逐条核对完整指纹。
        let cases = plan_repeated(selected("r1", "").unwrap(), &root, &model, 2).unwrap();
        assert_eq!(cases.len(), 40);
        assert!(preflight(&cases, Some(0)).is_err());
        assert_eq!(preflight(&cases, None).unwrap(), 0);
        assert!(cases.iter().all(|c| matches!(c.replay, Replay::Stale)));
        let report = evaluate(&cases, None, &mut |_| panic!("offline sender invoked")).unwrap();
        assert_eq!(report["sent_calls"], 0);
        assert_eq!(report["summary"][3]["stale"], 40);
        if effort == ReasoningEffort::Medium {
            for metrics in items(&report["metrics"]) {
                assert_eq!(metrics["elapsed_ms"]["count"], 0);
                assert!(metrics["elapsed_ms"]["p50"].is_null());
            }
        }
    }
}

#[test]
fn measured_success_and_failure_are_recorded_and_replayed_with_usage() {
    let temp = TempRecords::new();
    let chosen = selected("r1,r4", "r1_01,r4_01").unwrap();
    let cases = plan(chosen.clone(), &temp.0).unwrap();
    let mut calls = 0;
    let report = evaluate(&cases, Some(2), &mut |_| {
        calls += 1;
        if calls == 1 {
            Ok(fake_envelope(fake_r1(&cases[0].sample)))
        } else {
            Err("HTTP 503".into())
        }
    })
    .unwrap();
    let replay_cases = plan(chosen, &temp.0).unwrap();
    for case in &replay_cases {
        assert!(case.record["elapsed_ms"].is_u64());
        assert_eq!(case.record["reasoning_effort"], "medium");
    }
    let replay = evaluate(&replay_cases, None, &mut |_| {
        panic!("replayed failure sent again")
    })
    .unwrap();
    assert_eq!(report["results"], replay["results"]);
    assert_eq!(report["metrics"], replay["metrics"]);
    assert_eq!(replay["sent_calls"], 0);
    assert!(
        items(&report["metrics"])
            .iter()
            .all(|r| r["elapsed_ms"]["count"] == 1)
    );
    assert_eq!(report["results"][0]["usage"]["output_tokens"], 7);
    assert!(report["results"][1]["usage"].is_null());
    let rows:Vec<_>=[10,20,30,40,1000].into_iter().map(|elapsed|json!({"stage":"r1","status":"调用失败",
        "elapsed_ms":elapsed,"usage":{"output_tokens":20,"output_tokens_details":{"reasoning_tokens":15}}})).collect();
    let metric = &stage_metrics(&rows)[0];
    assert_eq!(
        metric["elapsed_ms"],
        json!({"count":5,"p50":30,"p90":1000,"max":1000})
    );
    assert_eq!(metric["output_tokens"], json!({"count":5,"mean":20.0}));
    assert_eq!(metric["reasoning_tokens"], json!({"count":5,"mean":15.0}));
}

#[test]
fn z8_r1_condition_subset_and_unlabelled_time_keep_required_semantics() {
    let sample = json!({"id":"synthetic-r1","split":"dev","label_status":"reviewed",
        "input":{"message":"去年我在家练习素描时戴耳机。","context":[],"said_on":"2026-10-04",
            "entities":[{"id":"self","names":["我"]}],"relations":["fact"]},
        "expect":{"need":"judge","current_evidence":"assertion_present","type":"single","subject":"self",
            "conditions":[["在家","我在家"]],"time":null,"plan.op":"lookup","plan.filter.relation":"fact"}});
    let mut output = fake_r1(&sample);
    output["conditions"]
        .as_array_mut()
        .unwrap()
        .push(json!({"quote":{"text":"练习素描时"},"resolved":true}));
    output["time"] = json!({"role":"event","quote":{"text":"去年"},"interp":{"kind":"relative","unit":"year","offset":-1},"anchor":"said_on"});
    let report = score("r1", &sample, Some(&output.to_string()));
    assert_eq!(report["status"], "通过", "{report}");
    assert!(
        !items(&report["fields"])
            .iter()
            .any(|f| f["field"] == "time")
    );
    let mut bad = output.clone();
    bad["conditions"].as_array_mut().unwrap().remove(0);
    assert_eq!(
        score("r1", &sample, Some(&bad.to_string()))["status"],
        "失败"
    );
    let mut labelled = sample.clone();
    // Set inclusion does not require duplicating a Quote for overlapping label alternatives.
    labelled["expect"]["conditions"] = json!(["在家", ["在家", "我在家"]]);
    assert_eq!(
        score("r1", &labelled, Some(&output.to_string()))["status"],
        "通过"
    );
    labelled["expect"]["time"] =
        json!({"quote":"去年","interp":{"kind":"relative","unit":"year","offset":0}});
    assert_eq!(
        score("r1", &labelled, Some(&output.to_string()))["status"],
        "失败"
    );
    output["time"]["quote"]["text"] = json!("不存在的时间");
    assert_eq!(
        score("r1", &sample, Some(&output.to_string()))["status"],
        "失败"
    );
}

#[test]
fn m15_stage_executors_bind_current_schema_and_input_contracts() {
    for (stage, schema, contract) in [
        ("w1", "w1_v3", "w1.v3"),
        ("w3", "w3_v3", "w3.v3"),
        ("w4", "w4_v4", "w4.v4"),
        ("r1", "r1_v2", "r1.v2"),
        ("r4", "r4_v6", "r4.v6"),
    ] {
        let request =
            stage_request_with_output_tokens(stage, &samples(stage).remove(0), 16384).unwrap();
        let input: Value = serde_json::from_str(request["input"].as_str().unwrap()).unwrap();
        assert_eq!(request["text"]["format"]["name"], schema);
        assert_eq!(input["contract"], contract);
        assert!(input.get("model").is_none());
        let mut previous = request.clone();
        let mut old_input = input;
        old_input.as_object_mut().unwrap().remove("contract");
        previous["input"] = json!(old_input.to_string());
        assert_ne!(fingerprint(&request), fingerprint(&previous));
    }
    for (stages, af11) in [("w3,r4", false), ("r1,r4", true)] {
        for (stage, sample) in fixed_adapter::selected_set(stages, "", af11).unwrap() {
            let request = stage_request_with_output_tokens(&stage, &sample, 16384).unwrap();
            let mut input: Value =
                serde_json::from_str(request["input"].as_str().unwrap()).unwrap();
            let contract = match stage.as_str() {
                "w3" => "w3.v3",
                "r1" => "r1.v2",
                "r4" => "r4.v6",
                _ => unreachable!(),
            };
            assert_eq!(input["contract"], contract);
            assert!(input.get("model").is_none());
            input.as_object_mut().unwrap().remove("contract");
            let mut frozen = sample["input"].clone();
            frozen.as_object_mut().unwrap().remove("contract");
            assert_eq!(input, frozen, "fixed input fields must remain verbatim");
        }
    }
}

#[test]
fn m15_w1_durable_send_and_w2_review_survive_resume_without_network() {
    let root = std::env::temp_dir().join(format!(
        "m15-w1-resume-{}",
        hippocampus::diagnostics::new_id()
    ));
    let mut cases = m15_w1_plan(&root).unwrap();
    cases.truncate(2);
    let sends = std::cell::Cell::new(0);
    let report=evaluate(&cases,Some(2),&mut |_request|{
        let paths:Vec<_>=std::fs::read_dir(cases[0].path.parent().unwrap().join("sends")).unwrap().map(|p|p.unwrap().path()).collect();
        if sends.get()==0 {
            let started:Value=serde_json::from_slice(&std::fs::read(&paths[0]).unwrap()).unwrap();
            assert_eq!(started["state"],"started");
        }
        sends.set(sends.get()+1);
        Ok(json!({"status":"completed","output":[{"content":[{"type":"output_text","text":"{\"candidates\":[],\"memory_requests\":[]}"}]}],"usage":{"input_tokens":10,"output_tokens":20}}))
    }).unwrap();
    assert_eq!(report["sent_calls"], 2);
    for row in items(&report["results"]) {
        assert_eq!(row["status"], "待复核");
        assert_eq!(row["mechanical_status"], "通过");
        assert_eq!(row["manual_review"]["status"], "pending");
        assert_eq!(row["w2_final"]["candidates"], json!([]));
        assert_eq!(row["usage"]["output_tokens"], 20);
    }
    let mut reopened = m15_w1_plan(&root).unwrap();
    reopened.truncate(2);
    let replay = evaluate(&reopened, None, &mut |_| {
        panic!("resume must not call a model")
    })
    .unwrap();
    assert_eq!(replay["sent_calls"], 0);
    assert_eq!(replay["results"][0]["raw"], report["results"][0]["raw"]);
    let path = reopened[0].path.clone();
    let mut altered: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    altered["recorded"][0]["request"] = json!("wrong-fingerprint");
    m15_write_json(&path, &altered).unwrap();
    let mut stale = m15_w1_plan(&root).unwrap();
    stale.truncate(1);
    assert!(matches!(stale[0].replay, Replay::Stale));
    assert_eq!(
        evaluate(&stale, None, &mut |_| panic!("stale must not send")).unwrap()["results"][0]["status"],
        "录制过期"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn m15_w1_inflight_send_consumes_campaign_quota_and_blocks_unfunded_resume() {
    let root = std::env::temp_dir().join(format!(
        "m15-inflight-{}",
        hippocampus::diagnostics::new_id()
    ));
    let cases = m15_w1_plan(&root).unwrap();
    let path = m15_start_send(&cases[0]).unwrap();
    assert!(path.exists());
    assert_eq!(m15_sent_before(&cases).unwrap(), 1);
    assert!(
        preflight(&cases, Some(24))
            .unwrap_err()
            .contains("累计已发送/在途")
    );
    let mut sent = 0;
    assert!(
        evaluate(&cases, Some(24), &mut |_| {
            sent += 1;
            Ok(Value::Null)
        })
        .is_err()
    );
    assert_eq!(sent, 0);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn m15_w1_fingerprint_covers_complete_request_without_stripping_fields() {
    let root = std::env::temp_dir().join(format!(
        "m15-fingerprint-{}",
        hippocampus::diagnostics::new_id()
    ));
    let cases = m15_w1_plan(&root).unwrap();
    let c = &cases[0];
    let original = recording_fingerprint(&c.sample, &c.request);
    let mut changed = c.request.clone();
    changed["max_output_tokens"] = json!(1);
    assert_ne!(original, recording_fingerprint(&c.sample, &changed));
    let mut changed = c.request.clone();
    changed["reasoning"] = json!({"effort":"medium"});
    assert_ne!(original, recording_fingerprint(&c.sample, &changed));
    let mut changed = c.request.clone();
    let mut input: Value = serde_json::from_str(changed["input"].as_str().unwrap()).unwrap();
    input["observed_at"] = json!(1);
    changed["input"] = json!(input.to_string());
    assert_ne!(original, recording_fingerprint(&c.sample, &changed));
}
