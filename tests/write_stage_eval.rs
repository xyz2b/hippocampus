//! 单阶段离线评分；样本标签仍为 draft，未录制与过期录制不冒充模型失败。
#[path = "eval/w3/adapter.rs"]
mod adapter;

#[path = "eval/recall_stage/adapter.rs"]
mod recall_adapter;

#[path = "eval/recall_stage/compare.rs"]
mod recall_compare;

#[path = "eval/recall_stage/review.rs"]
mod recall_review;

#[path = "eval/recall_stage/af264.rs"]
mod af264;

#[path = "eval/recall_stage/af268.rs"]
mod af268;

#[path = "eval/fixed_inputs/adapter.rs"]
mod fixed_adapter;

#[path = "eval/write_stage/af193/mod.rs"]
mod af193;

#[path = "eval/write_stage/af198/mod.rs"]
mod af198;
#[path = "eval/write_stage/af206/mod.rs"]
mod af206;
#[path = "eval/write_stage/af216/mod.rs"]
mod af216;

use hippocampus::model_provider::{self, ProviderConfig, ProviderProtocol, StructuredMode};
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
const W3_EFFORT_PROMPT: &str = include_str!("eval/write_stage/af193/w3-before.txt");
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
    let gaps = actual["coverage"]["status"] == "gaps";
    let missing = items(&actual["coverage"]["missing"]);
    check(
        fields,
        "coverage.validate",
        json!(true),
        actual["coverage"].clone(),
        (!gaps || !missing.is_empty())
            && missing.iter().all(|q| {
                q["text"].as_str().is_some_and(|q| {
                    !q.is_empty() && sample["input"]["text"].as_str().unwrap().contains(q)
                })
            }),
    );
    let scored = sample["expect"]["coverage"]["scored"] != false;
    if scored {
        compare(
            fields,
            "coverage.status",
            &sample["expect"]["coverage"]["status"],
            &actual["coverage"]["status"],
        );
    }
    for e in items(&sample["expect"]["coverage"]["missing_contains"])
        .iter()
        .filter(|_| scored || gaps)
    {
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

// Alternatives are full expectations, never per-field unions. This task has
// only one optional candidate; expand it into the same complete-branch check.
fn expectation_branches(expect: &Value) -> Vec<Value> {
    let bases = expect
        .get("alternatives")
        .map(items)
        .unwrap_or_else(|| std::slice::from_ref(expect));
    let mut branches = Vec::new();
    for base in bases {
        let optional = items(&base["optional_candidates"]);
        assert!(
            optional.len() <= 1,
            "only one optional candidate is supported"
        );
        let mut branch = base.clone();
        branch
            .as_object_mut()
            .unwrap()
            .remove("optional_candidates");
        branches.push(branch.clone());
        if let Some(candidate) = optional.first() {
            branch["candidates"]
                .as_array_mut()
                .unwrap()
                .push(candidate.clone());
            branch["candidate_count"] = json!(items(&branch["candidates"]).len());
            branches.push(branch);
        }
    }
    branches
}

fn score_raw(stage: &str, sample: &Value, text: Option<&str>) -> Value {
    if matches!(stage, "w1" | "w3" | "r1" | "r4")
        && text.is_some()
        && (sample["expect"].get("alternatives").is_some()
            || !items(&sample["expect"]["optional_candidates"]).is_empty())
    {
        let branches = expectation_branches(&sample["expect"]);
        if branches.len() > 1 || sample["expect"].get("alternatives").is_some() {
            let reports: Vec<_> = branches
                .iter()
                .map(|expect| {
                    let mut branch = sample.clone();
                    branch["expect"] = expect.clone();
                    if let Some(prohibitions) = expect.get("must_not") {
                        branch["must_not"] = prohibitions.clone();
                    }
                    score_raw(stage, &branch, text)
                })
                .collect();
            let passed = reports.iter().position(|r| r["status"] == "通过");
            let selected = passed.unwrap_or_else(|| {
                (0..reports.len())
                    .min_by_key(|i| {
                        items(&reports[*i]["fields"])
                            .iter()
                            .filter(|f| f["passed"] != true)
                            .count()
                    })
                    .unwrap()
            });
            let mut report = reports[selected].clone();
            report["expectation_branches"] = json!({"selected":selected,"passed":passed.is_some(),
                "statuses":reports.iter().map(|r|&r["status"]).collect::<Vec<_>>()});
            return report;
        }
    }
    let mut fields = Vec::new();
    let result = text.map(|text| {
        if sample["fixed_input"] == true && sample["input"]["format"] != "recall.full@1" {
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
    if let Some(revision) = sample.get("label_revision") {
        report["label_revision"] = revision.clone();
    }
    if stage == "w3" && text.is_some() {
        let raw: Value = text
            .and_then(|t| serde_json::from_str(t).ok())
            .unwrap_or(Value::Null);
        report["raw_output"] = raw.clone();
        let verdict_fields: Vec<_> = fields
            .iter()
            .filter(|f| !f["field"].as_str().unwrap().starts_with("coverage."))
            .collect();
        let valid = verdict_fields
            .iter()
            .filter(|f| f["category"] == "contract")
            .all(|f| f["passed"] == true);
        let coverage_fields: Vec<_> = fields
            .iter()
            .filter(|f| f["field"].as_str().unwrap().starts_with("coverage."))
            .collect();
        let coverage_valid = coverage_fields
            .iter()
            .any(|f| f["field"] == "coverage.validate" && f["passed"] == true);
        let scored = sample["expect"]["coverage"]["scored"] != false;
        report["verdict_result"] = json!({"valid":valid,"passed":valid.then(||verdict_fields.iter().all(|f|f["passed"]==true))});
        report["coverage_result"] = json!({"scored":scored,"status":raw["coverage"]["status"],
            "missing":raw["coverage"]["missing"],"valid":coverage_valid,
            "passed":scored.then(||coverage_valid && coverage_fields.iter().all(|f|f["passed"]==true)),
            "guard_passed":coverage_valid && coverage_fields.iter().filter(|f|f["field"]=="coverage.validate" || !scored).all(|f|f["passed"]==true)});
    }
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
    if stage == "r1" {
        report["raw_output_text"] = json!(text);
        report["final_output"] = result
            .as_ref()
            .and_then(|r| r.as_ref().ok())
            .cloned()
            .unwrap_or(Value::Null);
    }
    if stage == "r4" {
        report["validated_output"] = result
            .as_ref()
            .and_then(|r| r.as_ref().ok())
            .cloned()
            .unwrap_or(Value::Null);
    }
    if sample["fixed_input"] == true && sample["input"]["format"] != "recall.full@1" {
        report["scoring_scope"] = json!("contract_only");
        report["origin"] = sample["origin"].clone();
        report["manual_review"] = json!({"unlabelled_semantics":"pending"});
    }
    report
}

fn mark_observation(sample: &Value, report: &mut Value) {
    if sample["observation_only"] != true {
        return;
    }
    report["observation_only"] = json!(true);
    report["observation_reason"] = sample["observation_reason"].clone();
    if matches!(report["status"].as_str(), Some("通过" | "失败")) {
        report["observed_status"] = report["status"].clone();
        let mechanical_failure = items(&report["fields"]).iter().any(|f| {
            matches!(f["category"].as_str(), Some("contract" | "reference")) && f["passed"] != true
        });
        if !mechanical_failure {
            report["status"] = json!("观察");
        }
    }
}

fn score(stage: &str, sample: &Value, text: Option<&str>) -> Value {
    if sample["af216"] == true {
        if let Some(text) = text {
            if stage == "w1" {
                return af216::score_w1(sample, text);
            }
            if stage == "w3" {
                return af216::score_w3(sample, text);
            }
        }
    }

    if stage == "w1"
        && sample["af206"] == true
        && let Some(text) = text
    {
        return af206::score_w1(sample, text);
    }
    if stage == "w3"
        && sample["af206"] == true
        && sample["af198_final_w2"].is_object()
        && let Some(text) = text
    {
        return af206::score_w3(sample, text);
    }
    if stage == "w3"
        && sample["af198_final_w2"].is_object()
        && let Some(text) = text
    {
        return af198::score_w3(sample, text);
    }
    let mut raw = score_raw(stage, sample, text);
    mark_observation(sample, &mut raw);
    if !matches!(stage, "w4" | "r4") || text.is_none() {
        return raw;
    }
    let assembled = if stage == "r4" {
        (if sample["fixed_input"] == true && sample["input"]["format"] != "recall.full@1" {
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
    report["raw_result"] = raw;
    report["raw_output"] = serde_json::from_str(text.unwrap()).unwrap_or(json!(text.unwrap()));
    report["final_output"] = if stage == "r4" && !report["validated_output"].is_null() {
        report["validated_output"].clone()
    } else {
        assembled.unwrap_or(Value::Null)
    };
    if stage == "r4" {
        report["raw_output_text"] = json!(text.unwrap());
        if sample["af216_point"] == true {
            af216::points::check_r4(sample, &mut report);
        }
    }
    report["final_result"] = json!({"status":report["status"],"fields":report["fields"],"categories":report["categories"]});
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
    if sample["fixed_input"] == true && sample["input"]["format"] != "recall.full@1" {
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
            let mut payload = json!({"contract": if stage == "w1" { "w1.v4" } else { "w3.v4" }, "source_snapshot": {"eval_sample": sample["id"]},
                "current_user_message": input["text"], "user_history": input.get("history").cloned().unwrap_or(json!([])),
                "entities": items(&entities).iter().map(|e| json!({"id": e["id"], "owner_id": "eval",
                    "entity_type": "Person", "canonical_name": e["names"][0], "aliases": e["names"]})).collect::<Vec<_>>(),
                "relations": items(&relations).iter().map(|r| json!({"id": r,
                    "memory_types": ["fact", "preference", "constraint", "episode"]})).collect::<Vec<_>>(),
                "said_on": input["said_on"], "utc_offset_seconds": 28_800, "task_id": input.get("task_id").cloned().unwrap_or(Value::Null)});
            if stage == "w1" {
                (
                    ModelStage::W1,
                    "w1_v4",
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
                    "w3_v4",
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
                "w4_v5",
                write_models::W4_PROMPT,
                write_models::w4_schema(),
                json!({"contract": "w4.v5", "w4_input": input, "sources": sources}),
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
        Some("r1_v1" | "r1_v2" | "r4_v3" | "r4_v4" | "r4_v5" | "r4_v6" | "r4_v7")
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
    if std::env::var_os("EVAL_PROVIDER_CONFIG").is_some() {
        return std::env::var_os("EVAL_PROVIDER_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("target/eval-providers"));
    }
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
// Historical bindings retain DEEPSEEK_*; only evaluation credential/endpoint lookup changes.
fn evaluation_env<'a>(name: &'a str, model: Option<&str>) -> &'a str {
    if model.is_none_or(|m| m.starts_with("ep-")) {
        match name {
            "DEEPSEEK_BASE_URL" => return "HUOSHAN_DEEPSEEK_BASE_URL",
            "DEEPSEEK_API_KEY" => return "HUOSHAN_DEEPSEEK_API_KEY",
            _ => {}
        }
    }
    name
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
        let provider: Option<ProviderConfig> = std::env::var("EVAL_PROVIDER_CONFIG")
            .ok()
            .map(|s| {
                serde_json::from_str(&s).map_err(|e| format!("invalid EVAL_PROVIDER_CONFIG: {e}"))
            })
            .transpose()?;
        let selected_model = std::env::var("EVAL_MODEL").ok();
        let provider_base = provider.as_ref().and_then(|p| {
            std::env::var(evaluation_env(&p.base_url_env, selected_model.as_deref())).ok()
        });
        let mut model = Self {
            label: std::env::var("EVAL_MODEL_LABEL").unwrap_or_else(|_| MODEL.into()),
            model: selected_model,
            base: std::env::var("EVAL_BASE_URL")
                .ok()
                .or(provider_base)
                .or_else(|| {
                    if provider.is_none() {
                        std::env::var("HUOSHAN_DEEPSEEK_BASE_URL").ok()
                    } else {
                        None
                    }
                })
                .map(|s| s.trim_end_matches('/').to_owned()),
            effort: match std::env::var("EVAL_REASONING_EFFORT").ok() {
                None if provider
                    .as_ref()
                    .is_some_and(|p| p.protocol == ProviderProtocol::OpenaiChat) =>
                {
                    ReasoningEffort::Omit
                }
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
#[derive(Clone)]
struct ProviderOptions {
    provider: ProviderConfig,
    campaign: PathBuf,
}
fn provider_options_from_env() -> Result<Option<ProviderOptions>, String> {
    let Some(value) = std::env::var("EVAL_PROVIDER_CONFIG").ok() else {
        return Ok(None);
    };
    let provider: ProviderConfig =
        serde_json::from_str(&value).map_err(|e| format!("invalid EVAL_PROVIDER_CONFIG: {e}"))?;
    // AF132 authorizes only the fixed Opus profile, not arbitrary evaluation overrides.
    provider
        .require_native_schema()
        .map_err(|e| e.to_string())?;
    let campaign = std::env::var_os("EVAL_CAMPAIGN_DIR")
        .map(PathBuf::from)
        .ok_or("new provider requires EVAL_CAMPAIGN_DIR")?;
    Ok(Some(ProviderOptions { provider, campaign }))
}

const SMOKE_ROOT: &str = "target/eval-providers/smoke-af122";
const SMOKE_CURRENT_ROOT: &str = "target/eval-providers/smoke-af128";
const OPUS_PROMPT_SMOKE_ROOT: &str = "target/eval-providers/smoke-af132";
const SMOKE_CAP: usize = 8;
const SMOKE_SAMPLE: &str = "w1_01_simple_residence";
const SMOKE_IDS: [&str; SMOKE_CAP] = [
    "ds-pro",
    "ds-flash",
    "gpt-6.1-sol",
    "opus-5.5",
    "qwen-27b-think",
    "qwen-27b-nothink",
    "qwen-9b-think",
    "qwen-9b-nothink",
];

#[derive(Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SmokeProfile {
    id: String,
    model: String,
    provider: ProviderConfig,
    effort: ReasoningEffort,
    max_output_tokens: std::collections::BTreeMap<String, u32>,
    timeout_seconds: u64,
    // Routing metadata is deliberately outside the historic provider binding.
    #[serde(default)]
    endpoint_env: Option<String>,
}
fn evaluation_profiles(text: &str) -> Result<Vec<SmokeProfile>, String> {
    let profiles: Vec<SmokeProfile> = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let expected: Vec<_> = SMOKE_IDS
        .into_iter()
        .chain(AF156_IDS)
        .chain(AF163_IDS)
        .chain(AF166_QWEN_IDS)
        .collect();
    if profiles.iter().map(|p| p.id.as_str()).collect::<Vec<_>>() != expected {
        return Err("evaluation requires historic, AF156 and AF163 profiles in fixed order".into());
    }
    for profile in &profiles {
        profile.provider.validate().map_err(|e| e.to_string())?;
        if profile.provider.structured_mode == StructuredMode::PromptSchema
            && !matches!(
                (profile.id.as_str(), profile.model.as_str()),
                ("opus-5.5", "claude-opus-5-5")
                    | ("claude-sonnet-5-5", "claude-sonnet-5-5")
                    | ("claude-haiku-5-5", "claude-haiku-5-5")
            )
        {
            return Err(
                "prompt_schema is limited to the AF132/AF166 fixed evaluation profiles".into(),
            );
        }
        EvalModel {
            label: profile.id.clone(),
            model: Some(profile.model.clone()),
            base: None,
            effort: profile.effort,
        }
        .validate()?;
        let cloud_qwen = AF156_IDS.contains(&profile.id.as_str());
        let qwen38 = AF166_QWEN_IDS.contains(&profile.id.as_str());
        let local = profile.id.starts_with("qwen-");
        if json!(profile.max_output_tokens) != json!({"w1":32768,"w3":16384,"w4":16384})
            || profile.timeout_seconds
                != if local && profile.provider.thinking == Some(true) {
                    1800
                } else {
                    360
                }
            || profile.effort
                != if local {
                    ReasoningEffort::Omit
                } else if qwen38 || (cloud_qwen && profile.provider.thinking == Some(false)) {
                    ReasoningEffort::Omit
                } else {
                    ReasoningEffort::Medium
                }
        {
            return Err("AF122 output/timeout/effort is fixed".into());
        }
        if profile.endpoint_env.as_deref()
            != if local {
                Some("QWEN_LOCAL_BASE_URL")
            } else if matches!(profile.id.as_str(), "ds-pro" | "ds-flash") {
                Some("HUOSHAN_DEEPSEEK_BASE_URL")
            } else {
                None
            }
        {
            return Err("evaluation endpoint routing differs from the approved profile".into());
        }
        if cloud_qwen
            && (profile.model != model_provider::DASHSCOPE_MODEL
                || profile.provider.protocol != ProviderProtocol::OpenaiChat
                || profile.provider.base_url_env != "QWEN_BASE_URL"
                || profile.provider.api_key_env.as_deref() != Some("QWEN_API_KEY")
                || profile.provider.thinking != Some(profile.id.ends_with("-think"))
                || profile.provider.context_tokens != Some(1_000_000)
                || profile.provider.context_proof.is_some()
                || profile.provider.model_revision.as_deref()
                    != Some(model_provider::DASHSCOPE_CONTEXT_REVISION))
        {
            return Err(
                "AF156 snapshot/context/thinking binding differs from the approved profile".into(),
            );
        }
        if AF163_IDS.contains(&profile.id.as_str()) {
            let anthropic = profile.id.starts_with("claude-");
            let prefix = if anthropic { "ANTHROPIC" } else { "OPENAI" };
            if profile.model != profile.id
                || profile.provider.protocol
                    != if anthropic {
                        ProviderProtocol::AnthropicMessages
                    } else {
                        ProviderProtocol::OpenaiResponses
                    }
                || profile.provider.base_url_env != format!("{prefix}_BASE_URL")
                || profile.provider.api_key_env != Some(format!("{prefix}_API_KEY"))
                || profile.provider.thinking != anthropic.then_some(true)
                || profile.provider.structured_mode
                    != if anthropic {
                        StructuredMode::PromptSchema
                    } else {
                        StructuredMode::NativeSchema
                    }
                || profile.provider.context_tokens.is_some()
                || profile.provider.context_proof.is_some()
                || profile.provider.model_revision.is_some()
            {
                return Err(
                    "AF163 requires the approved AF166 structured-mode cloud profile".into(),
                );
            }
        }
        if qwen38
            && (profile.model != model_provider::DASHSCOPE_QWEN38_MODEL
                || profile.provider.protocol != ProviderProtocol::OpenaiChat
                || profile.provider.base_url_env != "QWEN_BASE_URL"
                || profile.provider.api_key_env.as_deref() != Some("QWEN_API_KEY")
                || profile.provider.thinking != Some(profile.id.ends_with("-think"))
                || profile.provider.structured_mode != StructuredMode::NativeSchema
                || profile.provider.context_tokens != Some(1_000_000)
                || profile.provider.context_proof.is_some()
                || profile.provider.model_revision.as_deref()
                    != Some(model_provider::DASHSCOPE_QWEN38_CONTEXT_REVISION))
        {
            return Err(
                "AF166 Qwen3.8 alias/context/thinking profile differs from approval".into(),
            );
        }
        profile
            .provider
            .check_model(&profile.model, profile.effort)
            .map_err(|e| e.to_string())?;
        if local {
            let revision = profile
                .provider
                .model_revision
                .as_deref()
                .ok_or("missing model digest")?;
            let digest = revision
                .strip_prefix("sha256:")
                .ok_or("expected sha256 digest")?;
            let proof = profile
                .provider
                .context_proof
                .as_ref()
                .ok_or("missing context proof")?;
            let capacity = if profile.id.starts_with("qwen-27b") {
                65536
            } else {
                131072
            };
            if digest.len() != 64
                || !digest.bytes().all(|b| b.is_ascii_hexdigit())
                || !proof.tokenizer_revision.contains(revision)
                || !proof
                    .tokenizer_revision
                    .contains("ollama=0.40.0;template_sha256_prefix=b507b9c2f6ca642b")
                || !proof.byte_level_bpe
                || proof.template_token_bound != 256
                || profile.provider.context_tokens != Some(capacity)
                || profile.provider.thinking != Some(profile.id.ends_with("-think"))
            {
                return Err(
                    "AF122 local deployment proof differs from the approved profile".into(),
                );
            }
        }
    }
    Ok(profiles)
}
fn smoke_profiles(text: &str) -> Result<Vec<SmokeProfile>, String> {
    Ok(evaluation_profiles(text)?
        .into_iter()
        .filter(|p| SMOKE_IDS.contains(&p.id.as_str()))
        .collect())
}

fn profile_base(
    profile: &SmokeProfile,
    recordings: &Path,
    variable: impl Fn(&str) -> Option<String>,
) -> Result<String, String> {
    let name = profile
        .endpoint_env
        .as_deref()
        .unwrap_or_else(|| evaluation_env(&profile.provider.base_url_env, Some(&profile.model)));
    if let Some(base) = variable(name) {
        return Ok(base);
    }
    // Recover only the old endpoint, then rebuild and check the full request fingerprint.
    // Never substitute the current cloud QWEN_BASE_URL for a historic local endpoint.
    fn endpoints(
        dir: &Path,
        p: &SmokeProfile,
        result: &mut std::collections::BTreeSet<String>,
    ) -> Result<(), String> {
        if !dir.exists() {
            return Ok(());
        }
        for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.is_dir() {
                endpoints(&path, p, result)?;
                continue;
            }
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let value: Value =
                serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            for record in items(&value["recorded"]) {
                let b = &record["provider_binding"];
                if b["provider"] == json!(p.provider)
                    && b["model"] == p.model
                    && b["effort"] == json!(p.effort)
                    && let Some(endpoint) = b["endpoint"].as_str()
                    && let Some(base) = record["base_url"].as_str().or_else(|| {
                        endpoint
                            .strip_suffix("/chat/completions")
                            .or_else(|| endpoint.strip_suffix("/responses"))
                            .or_else(|| endpoint.strip_suffix("/v1/messages"))
                    })
                {
                    if p.provider.endpoint(base).map_err(|e| e.to_string())? == endpoint {
                        result.insert(base.into());
                    }
                }
            }
        }
        Ok(())
    }
    let mut bases = std::collections::BTreeSet::new();
    endpoints(recordings, profile, &mut bases)?;
    if bases.len() > 1 {
        return Err(
            "historical endpoints are ambiguous; set the profile endpoint environment explicitly"
                .into(),
        );
    }
    Ok(bases.into_iter().next().unwrap_or_else(|| {
        if AF156_IDS.contains(&profile.id.as_str()) || AF166_QWEN_IDS.contains(&profile.id.as_str())
        {
            "https://dashscope.aliyuncs.com/compatible-mode".into()
        } else {
            let placeholder = if matches!(profile.id.as_str(), "ds-pro" | "ds-flash") {
                profile.provider.base_url_env.as_str()
            } else {
                name
            };
            format!("https://{}.invalid/v1", placeholder.to_ascii_lowercase())
        }
    }))
}
fn smoke_selection(value: Option<&str>) -> Result<Vec<String>, String> {
    let requested: Vec<_> = value.map_or_else(
        || SMOKE_IDS.to_vec(),
        |s| s.split(',').map(str::trim).collect(),
    );
    let distinct: std::collections::BTreeSet<_> = requested.iter().copied().collect();
    if requested.is_empty()
        || requested.len() != distinct.len()
        || requested.iter().any(|id| !SMOKE_IDS.contains(id))
    {
        return Err("EVAL_SMOKE_PROFILES requires distinct known profile IDs".into());
    }
    // Preserve the approved serial order regardless of the command's spelling order.
    Ok(SMOKE_IDS
        .iter()
        .filter(|id| distinct.contains(**id))
        .map(|id| (*id).to_owned())
        .collect())
}
fn smoke_root(base: &Path, selected: &[String]) -> PathBuf {
    let root = if selected == ["opus-5.5"] {
        OPUS_PROMPT_SMOKE_ROOT
    } else {
        SMOKE_CURRENT_ROOT
    };
    base.join(root).join(selected.join("--"))
}
fn smoke_options(
    variable: impl Fn(&str) -> Option<String>,
) -> Result<(bool, Option<usize>, Vec<String>), String> {
    if variable("EVAL_SMOKE").as_deref() != Some("1") {
        return Err("EVAL_SMOKE must be 1".into());
    }
    for key in [
        "EVAL_M15",
        "EVAL_HISTORY",
        "EVAL_FIXED_INPUTS",
        "EVAL_STAGES",
        "EVAL_CASES",
        "EVAL_MODEL",
        "EVAL_MODEL_LABEL",
        "EVAL_REASONING_EFFORT",
        "EVAL_PROVIDER_CONFIG",
        "EVAL_REPEAT",
        "EVAL_MAX_OUTPUT_TOKENS",
        "EVAL_PROVIDER_DIR",
        "EVAL_CAMPAIGN_DIR",
        "EVAL_BASE_URL",
        "EVAL_API_KEY",
        "EVAL_GATE",
    ] {
        if variable(key).is_some() {
            return Err(format!("AF122 fixed smoke does not accept {key}"));
        }
    }
    let selected = smoke_selection(variable("EVAL_SMOKE_PROFILES").as_deref())?;
    let dry = variable("EVAL_DRY_RUN").as_deref() == Some("1");
    let limit = match variable("EVAL_RECORD").as_deref() {
        None => None,
        Some("0") => Some(0),
        Some(s) if s.parse::<usize>().ok() == Some(selected.len()) => Some(selected.len()),
        _ => {
            return Err(format!(
                "EVAL_RECORD must be absent, 0 (cached only), or {} for this cohort",
                selected.len()
            ));
        }
    };
    if !dry
        && limit.is_some_and(|n| n > 0)
        && selected.iter().any(|id| id == "opus-5.5")
        && selected.len() != 1
    {
        return Err("AF132 prompt_schema recording requires only opus-5.5 and EVAL_RECORD=1; mixed Opus cohorts are read-only".into());
    }
    Ok((dry, limit, selected))
}
fn smoke_plan(
    root: &Path,
    profiles: &[SmokeProfile],
    variable: impl Fn(&str) -> Option<String>,
) -> Result<Vec<Case>, String> {
    let sample = samples("w1")
        .into_iter()
        .find(|s| s["id"] == SMOKE_SAMPLE)
        .ok_or("smoke sample missing")?;
    let mut cases = Vec::new();
    for p in profiles {
        // Placeholders permit key-free planning. Live preflight requires the actual environment URL.
        let base = profile_base(p, &root.join("recordings").join(&p.id), &variable)?;
        let model = EvalModel {
            label: p.id.clone(),
            model: Some(p.model.clone()),
            base: Some(base),
            effort: p.effort,
        };
        let options = ProviderOptions {
            provider: p.provider.clone(),
            campaign: root.join("campaign"),
        };
        model.validate()?;
        options
            .provider
            .endpoint(model.base.as_deref().unwrap())
            .map_err(|e| e.to_string())?;
        let mut request =
            stage_request_with_output_tokens("w1", &sample, p.max_output_tokens["w1"])?;
        request["model"] = json!(p.model);
        p.effort.apply(&mut request);
        let mut case = case_from_request_with_provider(
            "w1".into(),
            sample.clone(),
            &root.join("recordings").join(&p.id),
            &model,
            1,
            request,
            Some(&options),
            Some(p.timeout_seconds),
        )?;
        case.endpoint_env = p.endpoint_env.clone();
        cases.push(case);
    }
    if cases.is_empty()
        || cases
            .iter()
            .map(|c| campaign_slot(c).to_string())
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != profiles.len()
    {
        return Err("smoke requires distinct nonempty W1 slots".into());
    }
    Ok(cases)
}
fn smoke_dry_plan(cases: &[Case]) -> String {
    let mut lines = vec![
        "provider 冒烟干跑（不发送请求）".to_owned(),
        format!(
            "样本: {SMOKE_SAMPLE}；{} 档 × W1 × 1 次；W3/W4/R1/R4=0。",
            cases.len()
        ),
        "档案 | 模型 | effort | thinking | structured_mode | 输出上限 | 超时秒 | 预检".into(),
    ];
    for c in cases {
        let p = c.provider.as_ref().unwrap();
        let status = match &c.replay {
            Replay::Rejected(s) => format!("拒绝: {s}"),
            Replay::Output(_) | Replay::Failed(_) => "可回放".into(),
            Replay::Stale => "录制过期（需新录制）".into(),
            _ => "待录制".into(),
        };
        lines.push(format!(
            "{} | {} | {} | {} | {} | {} | {} | {status}",
            c.model.label,
            c.model.model.as_deref().unwrap(),
            serde_json::to_value(c.model.effort)
                .unwrap()
                .as_str()
                .unwrap(),
            p.thinking.map(|b| b.to_string()).unwrap_or("null".into()),
            serde_json::to_value(p.structured_mode)
                .unwrap()
                .as_str()
                .unwrap(),
            c.request["max_output_tokens"],
            c.timeout_seconds.unwrap()
        ));
    }
    let rejected = cases
        .iter()
        .filter(|c| matches!(c.replay, Replay::Rejected(_)))
        .count();
    let missing = cases
        .iter()
        .filter(|c| matches!(c.replay, Replay::Missing | Replay::Stale))
        .count();
    lines.push(format!(
        "固定槽位/累计授权上限: {}；待录制: {missing}；发送前拒绝: {rejected}。",
        cases.len()
    ));
    lines.push(format!(
        "独立 campaign: {}",
        campaign_root(cases).unwrap().unwrap().display()
    ));
    let tokens: u64 = cases
        .iter()
        .map(|c| c.request["max_output_tokens"].as_u64().unwrap())
        .sum();
    lines.push(format!(
        "输出预留上限: {tokens} token（含思考）；串行，无自动重试。"
    ));
    if cases
        .iter()
        .any(|c| c.provider.as_ref().unwrap().structured_mode == StructuredMode::PromptSchema)
    {
        lines.push("Opus prompt_schema：未使用约束解码；只接受原 schema/decode/W2 核验通过的输出，不修补。".into());
    }
    lines.push("本次真实调用: 0；不读取密钥，不创建录制/额度/报告文件。".into());
    lines.join("\n")
}
fn smoke_plan_with_history(
    root: &Path,
    history: &Path,
    profiles: &[SmokeProfile],
    variable: impl Fn(&str) -> Option<String> + Copy,
) -> Result<Vec<Case>, String> {
    let mut current = smoke_plan(root, profiles, variable)?;
    let previous = smoke_plan(history, profiles, variable)?;
    for (case, old) in current.iter_mut().zip(previous) {
        // Historical artifacts stay read-only; only advertise their strict binding mismatch.
        if matches!(case.replay, Replay::Missing) && matches!(old.replay, Replay::Stale) {
            case.replay = Replay::Stale;
        }
    }
    Ok(current)
}
fn smoke_credentials(
    cases: &[Case],
    variable: impl Fn(&str) -> Option<String>,
) -> Result<std::collections::BTreeMap<String, String>, String> {
    let mut keys = std::collections::BTreeMap::new();
    for c in cases
        .iter()
        .filter(|c| matches!(c.replay, Replay::Missing | Replay::Stale))
    {
        let p = c
            .provider
            .as_ref()
            .ok_or("smoke requires explicit provider")?;
        let name = c
            .endpoint_env
            .as_deref()
            .unwrap_or_else(|| evaluation_env(&p.base_url_env, c.model.model.as_deref()));
        let base = variable(name).ok_or_else(|| format!("missing {name}"))?;
        if Some(&base) != c.model.base.as_ref() {
            return Err("smoke endpoint changed after planning".into());
        }
        let key = if let Some(name) = &p.api_key_env {
            let name = evaluation_env(name, c.model.model.as_deref());
            variable(name).ok_or_else(|| format!("missing {name}"))?
        } else {
            String::new()
        };
        if key.trim().is_empty()
            && (p.api_key_env.is_some() || p.protocol != ProviderProtocol::OpenaiChat)
        {
            return Err(format!("missing API key for {}", c.model.label));
        }
        keys.insert(c.model.label.clone(), key.trim().to_owned());
    }
    Ok(keys)
}
fn smoke_http_request(
    http: &reqwest::blocking::Client,
    c: &Case,
    key: &str,
) -> Result<reqwest::blocking::Request, String> {
    let provider = c.provider.as_ref().ok_or("smoke requires provider")?;
    let url = provider
        .endpoint(c.model.base.as_deref().ok_or("missing endpoint")?)
        .map_err(|e| e.to_string())?;
    hippocampus::provider_http_request(http, &url, key, provider.protocol)
        .timeout(std::time::Duration::from_secs(
            c.timeout_seconds.ok_or("missing smoke timeout")?,
        ))
        .json(&c.wire_request)
        .build()
        .map_err(|e| e.to_string())
}
fn smoke_preflight(cases: &[Case], limit: Option<usize>) -> Result<usize, String> {
    if limit.is_some_and(|n| n > 0 && n != cases.len()) {
        return Err("smoke allowance must equal the selected slot count".into());
    }
    if limit.is_some()
        && cases
            .iter()
            .any(|c| matches!(c.replay, Replay::Rejected(_)))
    {
        return Err("AF122 entire batch refused: a profile failed preflight".into());
    }
    preflight(cases, limit)
}
fn smoke_report(mut report: Value, cases: &[Case]) -> Value {
    let rows: Vec<_> = items(&report["results"])
        .iter()
        .zip(cases)
        .map(|(r, c)| {
            let raw = if r["raw"].is_null() { &r["raw_output"] } else { &r["raw"] };
            let wire_valid = if r["status"] == "调用失败" {
                Some(false)
            } else if raw.is_null() {
                None
            } else {
                Some(model_provider::check_output(&c.request["text"]["format"]["schema"], raw).is_ok())
            };
            let checks: Vec<_> = items(&r["fields"])
                .iter()
                .filter(|f| {
                    f["field"] == "decode" || f["field"] == "w3.validate"
                        || f["field"].as_str().is_some_and(|s| s.starts_with("w2."))
                })
                .collect();
            let provider = c.provider.as_ref().unwrap();
            let transport_valid = if r["status"] == "调用失败" {
                Some(r["response"]["status"] == "completed")
            } else if raw.is_null() {
                None
            } else {
                Some(true)
            };
            json!({
                "profile": c.model.label, "model": c.model.model, "stage":c.stage,"id":c.sample["id"],"protocol": provider.protocol,
                "endpoint": provider.endpoint(c.model.base.as_deref().unwrap()).unwrap(),
                "effort": c.model.effort, "thinking": provider.thinking,
                "structured_mode":provider.structured_mode,
                "structured_output_note":if provider.structured_mode==StructuredMode::PromptSchema {"未使用约束解码"} else {"请求原生 schema"},
                "model_identity_note":if AF166_QWEN_IDS.contains(&c.model.label.as_str()) {"别名，可能漂移"} else {"固定档案"},
                "constrained_decoding":provider.structured_mode == StructuredMode::NativeSchema,
                "transport_valid":transport_valid,"provider_http_error":r["provider_http_error"],
                "max_output_tokens": c.request["max_output_tokens"], "timeout_seconds": c.timeout_seconds,
                "status": r["status"], "error": r["error"],
                "provider_output_valid": wire_valid,
                "mechanical_valid": (!checks.is_empty()).then(|| checks.iter().all(|f| f["passed"] == true)),
                "mechanical_checks": checks, "sample_score": r["status"],
                "semantic_score": r["categories"]["semantic"],
                "elapsed_ms": r["elapsed_ms"], "usage": r["usage"], "request": r["request"]
            })
        })
        .collect();
    report["model"] = json!("mixed-provider-smoke");
    report["purpose"] = json!("protocol availability only; not a quality comparison");
    report["smoke"] = json!(rows);
    report
}
fn provider_smoke_from_env() -> Result<(), String> {
    let variable = |name: &str| std::env::var(name).ok();
    let (dry, limit, selected) = smoke_options(variable)?;
    let base = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = smoke_root(base, &selected);
    let profiles: Vec<_> = smoke_profiles(include_str!("eval/write_stage/providers.json"))?
        .into_iter()
        .filter(|p| selected.contains(&p.id))
        .collect();
    let cases = smoke_plan_with_history(&root, &base.join(SMOKE_ROOT), &profiles, variable)?;
    if dry {
        println!("{}", smoke_dry_plan(&cases));
        return Ok(());
    }
    if limit.is_some_and(|n| n > 0) && selected.iter().any(|id| id.starts_with("qwen-")) {
        return Err("historic local Qwen profiles are read-only; recording is disabled".into());
    }
    // Validate the complete cohort before creating a client or spending any allowance.
    let sends = smoke_preflight(&cases, limit).map_err(|error| {
        println!("{}", smoke_dry_plan(&cases));
        error
    })?;
    let keys = if sends > 0 {
        smoke_credentials(&cases, variable)?
    } else {
        Default::default()
    };
    let http = if sends > 0 {
        Some(
            reqwest::blocking::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .build()
                .map_err(|e| e.to_string())?,
        )
    } else {
        None
    };
    let report = evaluate_cases(&cases, limit, &mut |c| {
        let http = http.as_ref().ok_or("offline smoke cannot send")?;
        let request = smoke_http_request(http, c, &keys[&c.model.label])?;
        let response = http.execute(request).map_err(|e| e.to_string())?;
        model_provider::read_http_response(response, c.provider.as_ref().unwrap().protocol)
            .map(|(_, envelope)| envelope)
            .map_err(|e| e.to_string())
    })?;
    let report = smoke_report(report, &cases);
    // Offline reports go to stdout only; historical recordings and allowance files remain untouched.
    if sends > 0 {
        m15_write_json(&root.join("report.json"), &report)?;
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
    );
    Ok(())
}

const COMPARE_ROOT: &str = "target/eval-providers/compare-af137";
const AF156_ROOT: &str = "target/eval-providers/compare-af156";
const AF163_ROOT: &str = "target/eval-providers/compare-af163";
const AF163_IDS: [&str; 3] = ["gpt-6-luna", "claude-sonnet-5-5", "claude-haiku-5-5"];
const AF166_PROMPT_IDS: [&str; 2] = ["claude-sonnet-5-5", "claude-haiku-5-5"];
const AF166_QWEN_IDS: [&str; 2] = ["qwen38-flash-think", "qwen38-flash-nothink"];
const AF163_COMPARE_IDS: [&str; 5] = [
    "gpt-6-luna",
    "claude-sonnet-5-5",
    "claude-haiku-5-5",
    "qwen38-flash-think",
    "qwen38-flash-nothink",
];
const AF166_DIAG_ROOT: &str = "target/eval-providers/compare-af163/diag-af166";
const AF163_SMOKE_SAMPLES: [&str; 2] = ["w1_01_simple_residence", "w3_01_supported_simple"];
const AF162_ROOT: &str = "target/eval-providers/compare-af156/diag-af162";
const AF156_IDS: [&str; 2] = ["qwen37-flash-think", "qwen37-flash-nothink"];
const COMPARE_IDS: [&str; 7] = [
    "ds-pro",
    "ds-flash",
    "gpt-6.1-sol",
    "opus-5.5",
    "qwen-27b-think",
    "qwen-27b-nothink",
    "qwen-9b-nothink",
];

struct CompareOptions {
    qwen38: bool,
    af163: bool,
    smoke: bool,
    af156: bool,
    diagnostic: bool,
    dry: bool,
    limit: Option<usize>,
    profiles: Vec<String>,
    cases: Vec<String>,
    gate: bool,
}
impl CompareOptions {
    fn quota(&self, id: &str) -> usize {
        if self.smoke {
            2
        } else if self.diagnostic {
            1
        } else {
            compare_quota(id)
        }
    }
}
fn compare_quota(id: &str) -> usize {
    if id == "qwen-27b-think" { 40 } else { 80 }
}
fn compare_sample_id(id: &str) -> String {
    let hash = hippocampus::recall_models::fingerprint(&json!(["af137-sample.v1", id]));
    format!("s-{}", &hash[7..23])
}
fn compare_request(stage: &str, sample: &Value, tokens: u32) -> Result<Value, String> {
    let mut request = stage_request_with_output_tokens(stage, sample, tokens)?;
    let mut input: Value =
        serde_json::from_str(request["input"].as_str().ok_or("missing AF137 input")?)
            .map_err(|e| e.to_string())?;
    // Only AF137 blinds this evaluator-owned identifier. History and actual W2 input stay intact.
    input["source_snapshot"]["eval_sample"] = json!(compare_sample_id(
        sample["id"].as_str().ok_or("missing AF137 sample ID")?
    ));
    request["input"] = json!(input.to_string());
    Ok(request)
}
fn compare_selection(value: Option<&str>, allowed: &[&str]) -> Result<Vec<String>, String> {
    let requested: Vec<_> = value.map_or_else(
        || allowed.to_vec(),
        |s| s.split(',').map(str::trim).collect(),
    );
    let distinct: std::collections::BTreeSet<_> = requested.iter().copied().collect();
    if requested.is_empty()
        || requested.len() != distinct.len()
        || requested.iter().any(|id| !allowed.contains(id))
    {
        return Err("AF137 selection requires distinct, nonempty, known IDs".into());
    }
    Ok(allowed
        .iter()
        .filter(|id| distinct.contains(**id))
        .map(|id| (*id).to_owned())
        .collect())
}
fn compare_samples() -> Result<Vec<(String, Value)>, String> {
    let mut result = Vec::new();
    for stage in ["w1", "w3"] {
        let samples = samples(stage);
        if samples.len() != 20 || samples.iter().any(|s| s["label_status"] != "reviewed") {
            return Err(format!("AF137 requires 20 reviewed {stage} samples"));
        }
        result.extend(samples.into_iter().map(|s| (stage.into(), s)));
    }
    let ids: std::collections::BTreeSet<_> = result.iter().map(|(_, s)| s["id"].as_str()).collect();
    if ids.len() != 40 || ids.iter().any(|id| id.is_none_or(str::is_empty)) {
        return Err("AF137 requires 40 distinct nonempty sample IDs".into());
    }
    if ids
        .iter()
        .map(|id| compare_sample_id(id.unwrap()))
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        != 40
    {
        return Err("AF137 opaque sample ID collision".into());
    }
    Ok(result)
}
fn compare_options(variable: impl Fn(&str) -> Option<String>) -> Result<CompareOptions, String> {
    let (af156, diagnostic, af163, smoke) = match variable("EVAL_COMPARE").as_deref() {
        Some("af137") => (false, false, false, false),
        Some("af156") => (true, false, false, false),
        Some("af162") => (true, true, false, false),
        Some("af163") => (false, false, true, false),
        Some("af163-smoke") => (false, false, true, true),
        _ => return Err("EVAL_COMPARE must be af137, af156, af162, af163 or af163-smoke".into()),
    };
    for key in [
        "EVAL_SMOKE",
        "EVAL_SMOKE_PROFILES",
        "EVAL_M15",
        "EVAL_HISTORY",
        "EVAL_FIXED_INPUTS",
        "EVAL_STAGES",
        "EVAL_CASES",
        "EVAL_MODEL",
        "EVAL_MODEL_LABEL",
        "EVAL_REASONING_EFFORT",
        "EVAL_PROVIDER_CONFIG",
        "EVAL_REPEAT",
        "EVAL_MAX_OUTPUT_TOKENS",
        "EVAL_PROVIDER_DIR",
        "EVAL_CAMPAIGN_DIR",
        "EVAL_BASE_URL",
        "EVAL_API_KEY",
    ] {
        if variable(key).is_some() && !(diagnostic && key == "EVAL_CASES") {
            return Err(format!("AF137 fixed comparison does not accept {key}"));
        }
    }
    let diagnostic_ids: Vec<_> = AF156_IDS.into_iter().chain(AF166_QWEN_IDS).collect();
    let profiles = compare_selection(
        variable("EVAL_COMPARE_PROFILES").as_deref(),
        if smoke {
            &AF166_PROMPT_IDS
        } else if af163 {
            &AF163_COMPARE_IDS
        } else if diagnostic && variable("EVAL_COMPARE_PROFILES").is_some() {
            &diagnostic_ids
        } else if af156 {
            &AF156_IDS
        } else {
            &COMPARE_IDS
        },
    )?;
    let qwen38 = profiles
        .iter()
        .any(|id| AF166_QWEN_IDS.contains(&id.as_str()));
    if diagnostic && qwen38 && profiles.iter().any(|id| AF156_IDS.contains(&id.as_str())) {
        return Err(
            "Qwen3.7 and Qwen3.8 diagnostics have separate authorizations; cannot mix".into(),
        );
    }
    let samples = compare_samples()?;
    let ids: Vec<_> = samples
        .iter()
        .map(|(_, s)| s["id"].as_str().unwrap())
        .collect();
    let cases = if smoke {
        if variable("EVAL_COMPARE_CASES").is_some() {
            return Err("AF163 smoke uses the fixed W1/W3 pair".into());
        }
        AF163_SMOKE_SAMPLES
            .iter()
            .map(|s| (*s).to_owned())
            .collect()
    } else if diagnostic {
        if variable("EVAL_COMPARE_CASES").is_some() {
            return Err("AF162 uses only EVAL_CASES".into());
        }
        let prefix = variable("EVAL_CASES").unwrap_or_else(|| "w1_01".into());
        let found: Vec<_> = ids
            .iter()
            .filter(|id| !prefix.is_empty() && id.starts_with(&prefix))
            .map(|id| (*id).to_owned())
            .collect();
        if found.len() != 1 {
            return Err("AF162 EVAL_CASES must identify exactly one sample".into());
        }
        found
    } else {
        compare_selection(variable("EVAL_COMPARE_CASES").as_deref(), &ids)?
    };
    if diagnostic && qwen38 && cases != [SMOKE_SAMPLE] {
        return Err("AF166 Qwen3.8 diagnostic is fixed to w1_01".into());
    }
    let quota: usize = profiles
        .iter()
        .map(|id| {
            if smoke {
                2
            } else if diagnostic {
                1
            } else {
                compare_quota(id)
            }
        })
        .sum();
    let limit = match variable("EVAL_RECORD").as_deref() {
        None => None,
        Some("0") => Some(0),
        Some(s) if s.parse::<usize>().ok() == Some(quota) => Some(quota),
        _ => {
            return Err(format!(
                "AF137 EVAL_RECORD must be absent, 0, or {quota}; subsets retain the full profile allowance"
            ));
        }
    };
    Ok(CompareOptions {
        qwen38,
        af163,
        smoke,
        af156,
        diagnostic,
        dry: variable("EVAL_DRY_RUN").as_deref() == Some("1"),
        limit,
        profiles,
        cases,
        gate: eval_gate(variable("EVAL_GATE").as_deref())?,
    })
}
fn compare_plan(
    root: &Path,
    profiles: &[SmokeProfile],
    variable: impl Fn(&str) -> Option<String>,
) -> Result<Vec<Vec<Case>>, String> {
    compare_plan_for(root, profiles, &COMPARE_IDS, variable)
}
fn compare_plan_for(
    root: &Path,
    profiles: &[SmokeProfile],
    ids: &[&str],
    variable: impl Fn(&str) -> Option<String>,
) -> Result<Vec<Vec<Case>>, String> {
    compare_plan_for_sample(root, profiles, ids, variable, None)
}
fn compare_plan_for_sample(
    root: &Path,
    profiles: &[SmokeProfile],
    ids: &[&str],
    variable: impl Fn(&str) -> Option<String>,
    diagnostic_case: Option<&str>,
) -> Result<Vec<Vec<Case>>, String> {
    compare_plan_with_smoke(root, profiles, ids, variable, diagnostic_case, false)
}
fn compare_plan_with_smoke(
    root: &Path,
    profiles: &[SmokeProfile],
    ids: &[&str],
    variable: impl Fn(&str) -> Option<String>,
    diagnostic_case: Option<&str>,
    smoke: bool,
) -> Result<Vec<Vec<Case>>, String> {
    let mut selected = compare_samples()?;
    if smoke {
        selected.retain(|(_, s)| AF163_SMOKE_SAMPLES.contains(&s["id"].as_str().unwrap()));
        if selected.len() != 2 {
            return Err("AF163 smoke pair missing".into());
        }
    }
    if let Some(id) = diagnostic_case {
        selected.retain(|(_, s)| s["id"] == id);
        if selected.len() != 1 {
            return Err("AF162 requires one known sample".into());
        }
    }
    let mut groups = Vec::new();
    for &id in ids {
        let p = profiles
            .iter()
            .find(|p| p.id == id)
            .ok_or("missing AF137 profile")?;
        let base = profile_base(p, &root.join("recordings").join(id), &variable)?;
        let model = EvalModel {
            label: p.id.clone(),
            model: Some(p.model.clone()),
            base: Some(base),
            effort: p.effort,
        };
        model.validate()?;
        p.provider
            .endpoint(model.base.as_deref().unwrap())
            .map_err(|e| e.to_string())?;
        p.provider
            .check_model(&p.model, p.effort)
            .map_err(|e| e.to_string())?;
        let options = ProviderOptions {
            provider: p.provider.clone(),
            campaign: root.join("campaigns").join(id),
        };
        let mut cases = Vec::new();
        for (stage, sample) in &selected {
            let mut request = compare_request(stage, sample, p.max_output_tokens[stage])?;
            request["model"] = json!(p.model);
            p.effort.apply(&mut request);
            for repeat in 1..=if diagnostic_case.is_some() || smoke {
                1
            } else {
                compare_quota(id) / 40
            } {
                let mut case = case_from_request_with_provider(
                    stage.clone(),
                    sample.clone(),
                    &root
                        .join("recordings")
                        .join(id)
                        .join(format!("repeat-{repeat}")),
                    &model,
                    repeat,
                    request.clone(),
                    Some(&options),
                    Some(p.timeout_seconds),
                )?;
                case.endpoint_env = p.endpoint_env.clone();
                cases.push(case);
            }
        }
        if cases
            .iter()
            .map(campaign_slot)
            .map(|s| s.to_string())
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != if smoke {
                2
            } else if diagnostic_case.is_some() {
                1
            } else {
                compare_quota(id)
            }
        {
            return Err("AF137 duplicate slots".into());
        }
        compare_mark_interrupted(&mut cases)?;
        groups.push(cases);
    }
    Ok(groups)
}
fn compare_mark_interrupted(cases: &mut [Case]) -> Result<(), String> {
    let root = campaign_root(cases)?.ok_or("AF137 campaign missing")?;
    let dir = root.join("sends");
    if !dir.exists() {
        return Ok(());
    }
    let mut sent = std::collections::BTreeSet::new();
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let log: Value = serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        if !matches!(log["state"].as_str(), Some("started" | "responded")) {
            return Err("invalid AF137 send log".into());
        }
        sent.insert(log["slot"].to_string());
    }
    for case in cases {
        // Only an exact sent identity with no recording is skippable. Stale
        // recordings and changed campaign identities still fail strict checks.
        if matches!(case.replay, Replay::Missing) && sent.contains(&campaign_slot(case).to_string())
        {
            case.replay = Replay::Interrupted;
        }
    }
    Ok(())
}
fn compare_subset(cases: &[Case], options: &CompareOptions) -> Vec<Case> {
    cases
        .iter()
        .filter(|c| options.cases.iter().any(|id| c.sample["id"] == *id))
        .cloned()
        .collect()
}
fn compare_preflight(groups: &[Vec<Case>], options: &CompareOptions) -> Result<usize, String> {
    let quota: usize = options.profiles.iter().map(|id| options.quota(id)).sum();
    if options.limit.is_some_and(|n| n != 0 && n != quota) {
        return Err(format!("AF137 allowance must equal {quota}"));
    }
    let mut sends = 0;
    for group in groups
        .iter()
        .filter(|g| options.profiles.contains(&g[0].model.label))
    {
        let id = &group[0].model.label;
        let limit = options
            .limit
            .map(|n| if n == 0 { 0 } else { options.quota(id) });
        if limit.is_some_and(|n| n > 0) {
            campaign_check(group, options.quota(id))?;
        }
        sends += preflight(&compare_subset(group, options), limit)?;
    }
    Ok(sends)
}
fn compare_freeze(group: &[Case]) -> Result<(), String> {
    let root = campaign_root(group)?.ok_or("AF137 campaign missing")?;
    let _lock = w3_trial_lock(root)?;
    let quota = group.len();
    campaign_check(group, quota)?;
    let path = root.join("authorization.json");
    if !path.exists() {
        m15_write_json(
            &path,
            &json!({"limit":quota,"requests":group.iter().map(campaign_slot).collect::<Vec<_>>()}),
        )?;
    }
    Ok(())
}
fn compare_resource(id: &str) -> &str {
    if id.starts_with("ds-") {
        "deepseek"
    } else if id.starts_with("qwen-") {
        "qwen"
    } else if AF156_IDS.contains(&id) || AF166_QWEN_IDS.contains(&id) {
        "dashscope"
    } else {
        id
    }
}
fn compare_dry_plan(groups: &[Vec<Case>], options: &CompareOptions) -> Result<String, String> {
    let mut lines = vec![
        format!("{} 写入侧多模型比较干跑（不发送请求）",if options.smoke {"AF163 冒烟"} else if options.af163 {"AF163"} else if options.af156 {"AF156"} else {"AF137"}),
        if options.af156 || options.af163 {
            "固定样本: W1 20 + W3 20（reviewed，当前 AF155 标注 / 请求 v3）；W4/R1/R4=0。"
        } else {
            "固定样本: W1 20 + W3 20（reviewed，标注 v1 / 请求 v3）；W4/R1/R4=0。"
        }.into(),
        "档案 | 完整槽位/累计上限 | 本次所选 | 待录制 | 可回放 | 过期 | 拒绝 | 中断在途 | 已占用 | 输出 token 上界 | 超时秒 | 熔断未发送".into(),
    ];
    let mut selected_slots = 0;
    let mut needed = 0;
    let mut rejected = 0;
    let mut interrupted = 0;
    let mut occupied = 0;
    let mut blocked = 0;
    let mut reserved = 0;
    let mut cloud_reserved = 0;
    for group in groups {
        let id = &group[0].model.label;
        let circuits = provider_circuits(group)?;
        let subset = if options.profiles.contains(id) {
            compare_subset(group, options)
        } else {
            vec![]
        };
        let missing = subset
            .iter()
            .filter(|c| {
                matches!(c.replay, Replay::Missing | Replay::Stale) && !circuit_blocks(c, &circuits)
            })
            .count();
        let stale = subset
            .iter()
            .filter(|c| matches!(c.replay, Replay::Stale))
            .count();
        let refused = subset
            .iter()
            .filter(|c| matches!(c.replay, Replay::Rejected(_)))
            .count();
        let cached = subset
            .iter()
            .filter(|c| matches!(c.replay, Replay::Output(_) | Replay::Failed(_)))
            .count();
        let inflight = subset
            .iter()
            .filter(|c| matches!(c.replay, Replay::Interrupted))
            .count();
        let circuit_open = subset
            .iter()
            .filter(|c| circuit_blocks(c, &circuits))
            .count();
        let used = count_sends([campaign_root(group)?.unwrap().join("sends")])?;
        let tokens: u64 = group
            .iter()
            .map(|c| c.request["max_output_tokens"].as_u64().unwrap())
            .sum();
        selected_slots += subset.len();
        needed += missing;
        rejected += refused;
        interrupted += inflight;
        blocked += circuit_open;
        if options.profiles.contains(id) {
            occupied += used;
        }
        reserved += tokens;
        if !id.starts_with("qwen-") {
            cloud_reserved += tokens;
        }
        lines.push(format!(
            "{id} | {} | {} | {missing} | {cached} | {stale} | {refused} | {inflight} | {used} | {tokens} | {} | {circuit_open}",
            group.len(),
            subset.len(),
            group[0].timeout_seconds.unwrap()
        ));
    }
    let fixed = if options.smoke {
        "4（独立于正式 400 槽；Luna 原 2 槽保留）"
    } else if options.af163 {
        "400（三档 240 + Qwen3.8 两档 160；本地 0）"
    } else if options.af156 {
        "160（云端 160 / 本地 0）"
    } else {
        "520（云端 320 / 本地 200）"
    };
    lines.push(format!("完整固定槽位: {fixed}；本次所选: {selected_slots}；待录制: {needed}；发送前拒绝: {rejected}。"));
    lines.push(format!("本次所选档案累计已占用: {occupied}；未录制（中断在途）: {interrupted}（跳过、不重发、仍占额度）。"));
    lines.push(format!("熔断未发送: {blocked}（同档连续 3 次同类 HTTP 错误或 incomplete(max_output_tokens)；后续槽不占额度，重启仍停止）。"));
    lines.push(format!("完整输出额度上界: {reserved} token（云端 {cloud_reserved} / 本地 {}，含思考）；不是预计用量，输入另计。", reserved - cloud_reserved));
    let root = if options.smoke {
        "target/eval-providers/compare-af163/smoke"
    } else if options.af163 {
        AF163_ROOT
    } else if options.af156 {
        AF156_ROOT
    } else {
        COMPARE_ROOT
    };
    let quota = if options.smoke {
        "2"
    } else if options.af163 || options.af156 {
        "80"
    } else {
        "80/40"
    };
    lines.push(format!(
        "每档独立 campaign: {root}/campaigns/<profile>；子集沿用完整 {quota} 槽额度。"
    ));
    lines.push("比较模式标识盲化: source_snapshot.eval_sample=s-<16 位 SHA-256>；报告保留真实样本 ID 对照。".into());
    if options.af156 {
        let selected: Vec<_> = groups
            .iter()
            .filter(|g| options.profiles.contains(&g[0].model.label))
            .flat_map(|g| compare_subset(g, options))
            .collect();
        let lengths: Vec<_> = selected
            .iter()
            .map(|c| serde_json::to_vec(&c.wire_request).unwrap().len())
            .collect();
        lines.push(format!("输入粗估: {} token（逐请求完整 wire UTF-8 字节/3 向上取整，非预检依据）；完整请求字节合计 {}，单次最大 {}。",
            lengths.iter().map(|n|n.div_ceil(3)).sum::<usize>(),lengths.iter().sum::<usize>(),lengths.iter().max().unwrap_or(&0)));
        lines.push(format!("输出使用 max_completion_tokens（思考+回答）；官方至多 10 token 误差，完整额度保守合计 {} token。",reserved+groups.iter().map(Vec::len).sum::<usize>() as u64*10));
        lines.push("40 样本不变：39 个计分、1 个观察（两档共 4 个观察槽）；当前 AF155 标注，W3 verdict/coverage 分列。".into());
        lines.push("DashScope 仅固定快照、官方 HTTPS、纯文本两条消息；完整 wire 字节预检官方输入/上下文上限，超限拒绝、不截断；云端真实分词超限亦报错。".into());
        lines.push("仅 AF156 两档可录制；本地历史档只读，与 reports-af155 七档严格对照，旧账本不复用、不扩额。".into());
        for group in groups {
            let budget = |stage| {
                group
                    .iter()
                    .find(|c| c.stage == stage)
                    .map(|c| c.wire_request["thinking_budget"].clone())
                    .unwrap_or(Value::Null)
            };
            lines.push(format!(
                "{} thinking_budget: W1={} / W3={}（null 表示不传）。",
                group[0].model.label,
                budget("w1"),
                budget("w3")
            ));
        }
    }
    if options.af163 {
        let bytes: usize = groups
            .iter()
            .filter(|g| options.profiles.contains(&g[0].model.label))
            .flat_map(|g| compare_subset(g, options))
            .map(|c| serde_json::to_vec(&c.wire_request).unwrap().len())
            .sum();
        lines.push(format!("输入粗估: {} token（完整 wire UTF-8 字节/3，非额度/上下文证明）；请求字节合计 {bytes}。",bytes.div_ceil(3)));
        let selected: Vec<_> = groups
            .iter()
            .filter(|g| options.profiles.contains(&g[0].model.label))
            .flat_map(|g| compare_subset(g, options))
            .collect();
        let output: u64 = selected
            .iter()
            .map(|c| c.request["max_output_tokens"].as_u64().unwrap())
            .sum();
        let rounding = selected
            .iter()
            .filter(|c| AF166_QWEN_IDS.contains(&c.model.label.as_str()))
            .count() as u64
            * 10;
        lines.push(format!("本次所选输出额度: {output} token；Qwen 实际输出至多每次另有 10 token 误差，保守合计 {}（不是预计用量）。",output+rounding));
        lines.push(
            "当前 AF155 标注：40 样本中 39 个计分、1 个观察；W3 verdict/coverage 分列。".into(),
        );
        lines.push("Luna native schema；Sonnet/Haiku medium + prompt_schema（未使用约束解码）；Qwen3.8 effort=omit，以 enable_thinking + thinking_budget 绑定开关。".into());
        lines.push(if options.smoke {"每档 W1 w1_01 + W3 w3_01，各一次；独立 2 槽，共 4；Luna 两条原生冒烟保留，不重开额度。"} else {"正式录制须先核验所选档 W1/W3 冒烟的严格绑定、协议/schema/机械核验与 usage；缺录制时发送前拒绝。"}.into());
        if !options.smoke {
            lines.push("Qwen3.8：别名，可能漂移；真实 W1 schema 诊断须通过完整 JSON/schema/W2/usage 且无完全相同候选/请求重复后，才允许本档正式发送。".into());
            for group in groups
                .iter()
                .filter(|g| AF166_QWEN_IDS.contains(&g[0].model.label.as_str()))
            {
                lines.push(format!("{} enable_thinking={}；thinking_budget: W1={} / W3={}；reasoning_effort 不传。",
                    group[0].model.label,group[0].wire_request["enable_thinking"],
                    group.iter().find(|c|c.stage=="w1").unwrap().wire_request["thinking_budget"],
                    group.iter().find(|c|c.stage=="w3").unwrap().wire_request["thinking_budget"]));
            }
        }
        lines.push(
            "Qwen3.7-flash：不可用，无约束解码；旧 Qwen3.7 AF156/AF162 剩余额度作废，仅保留离线历史。".into(),
        );
    } else if options.af156 {
        lines.push("两档串行，共用 DashScope 执行锁；同一档不可重复启动。".into());
    } else {
        lines.push(
            "各档串行；DeepSeek 两档共用执行锁，本地三档共用执行锁；不同 provider 可独立进程并行。"
                .into(),
        );
        lines.push("Opus prompt_schema：未使用约束解码，原 schema 完整核验、不修补；27B 思考仅一次，稳定性未验证。".into());
    }
    lines.push("本次真实调用: 0；不读取密钥，不创建录制/额度/报告文件。".into());
    Ok(lines.join("\n"))
}
fn diagnostic_dry_plan(groups: &[Vec<Case>], options: &CompareOptions) -> Result<String, String> {
    let mut lines = vec![
        if options.qwen38 {
            "AF166 Qwen3.8 单样本诊断干跑（沿用 AF162，不发送请求）"
        } else {
            "AF162 单样本诊断干跑（不发送请求）"
        }
        .into(),
        format!(
            "固定诊断样本: {}；独立目录: {}；正式槽位: 0。",
            options.cases[0],
            if options.qwen38 {
                AF166_DIAG_ROOT
            } else {
                AF162_ROOT
            }
        ),
        "档案 | 槽位上限 | 待录制 | 拒绝 | 已占用 | thinking_budget | 输出上限".into(),
    ];
    let mut needed = 0;
    let mut rejected = 0;
    for group in groups
        .iter()
        .filter(|g| options.profiles.contains(&g[0].model.label))
    {
        campaign_check(group, 1)?;
        let circuits = provider_circuits(group)?;
        let pending = group
            .iter()
            .filter(|c| {
                matches!(c.replay, Replay::Missing | Replay::Stale) && !circuit_blocks(c, &circuits)
            })
            .count();
        let refused = group
            .iter()
            .filter(|c| matches!(c.replay, Replay::Rejected(_)))
            .count();
        let used = count_sends([campaign_root(group)?.unwrap().join("sends")])?;
        if used + pending > 1 {
            return Err("AF162 profile cumulative allowance exceeds 1".into());
        }
        needed += pending;
        rejected += refused;
        lines.push(format!(
            "{} | 1 | {pending} | {refused} | {used} | {} | {}",
            group[0].model.label,
            group[0].wire_request["thinking_budget"],
            group[0].request["max_output_tokens"]
        ));
    }
    lines.push(format!("完整诊断额度: 2（think 1 + nothink 1）；本次所选: {}；待录制: {needed}；拒绝: {rejected}。",options.profiles.len()));
    if options.qwen38 {
        lines.push("qwen3.8-flash：别名，可能漂移；原生 json_schema，真实 W1 schema，盲化 ID；enable_thinking=true/false，reasoning_effort 不传。".into());
        lines.push("开思考 thinking_budget=16384；关思考不传预算；输出上限 32768（思考+回答），完整两槽至多 65556 token（含各 10 token 误差）；截断留有界诊断片段。".into());
        lines.push("诊断核验完整 JSON、原 schema/W2、usage 与完全相同重复候选/请求；通过后才能使用正式两档各 80 槽，本诊断不计正式额度。".into());
    }
    lines.push("发送前落盘、不重试、在途不重发、严格指纹；续跑不扩额。诊断片段不参与评分。".into());
    lines.push("本次真实调用: 0；不读取密钥、不创建目录/账本/录制。".into());
    Ok(lines.join("\n"))
}
fn compare_mechanical(row: &Value) -> Vec<String> {
    let reason = match row["status"].as_str() {
        Some("未录制") => Some("missing"),
        Some("未录制（中断在途）") => Some("interrupted_inflight"),
        Some("熔断未发送") => Some("provider_circuit_open"),
        Some("录制过期") => Some("stale"),
        Some("发送前拒绝") => Some("preflight_rejected"),
        Some("输入无效") => Some("invalid_input"),
        Some("调用失败") => {
            let error = row["error"].as_str().unwrap_or("").to_ascii_lowercase();
            Some(
                if error.contains("timeout") || error.contains("timed out") {
                    "timeout"
                } else if error.contains("json") || error.contains("schema") {
                    "decode/schema"
                } else {
                    "call_failed"
                },
            )
        }
        _ => None,
    };
    let mut reasons: Vec<_> = reason.into_iter().map(str::to_owned).collect();
    for f in items(&row["fields"])
        .iter()
        .filter(|f| f["passed"] == false)
    {
        let field = f["field"].as_str().unwrap_or("");
        if f["category"] == "contract" {
            reasons.push(field.to_owned());
        }
    }
    reasons
}
fn compare_tier(cells: &[Value]) -> &str {
    let stable = |c: &Value| c["total"] == 2 && c["passed"] == 2;
    if cells
        .iter()
        .filter(|c| c["profile"].as_str().unwrap().starts_with("qwen-"))
        .any(stable)
    {
        return "本地即可（本轮两次均通过）";
    }
    if cells.iter().any(|c| c["complete"] != true) {
        return "证据不足，暂不分层";
    }
    if cells.iter().any(|c| c["total"] == 1 && c["passed"] == 1) {
        return "本地单次通过，稳定性未验证";
    }
    if cells.iter().any(|c| c["unstable"] == true) {
        return "重复不一致，暂不分层";
    }
    if cells
        .iter()
        .filter(|c| {
            matches!(
                c["profile"].as_str(),
                Some("gpt-6-luna" | "claude-haiku-5-5")
            )
        })
        .any(stable)
    {
        return "低价云档即可（本轮 Luna/Haiku 两次均通过）";
    }
    if cells
        .iter()
        .filter(|c| c["profile"] == "claude-sonnet-5-5")
        .any(stable)
    {
        return "Sonnet 即可（本轮两次均通过）";
    }
    if cells
        .iter()
        .filter(|c| {
            AF156_IDS.contains(&c["profile"].as_str().unwrap())
                || AF166_QWEN_IDS.contains(&c["profile"].as_str().unwrap())
        })
        .any(stable)
    {
        return "阿里云 Qwen 即可（本轮两次均通过）";
    }
    if cells
        .iter()
        .filter(|c| c["profile"].as_str().unwrap().starts_with("ds-"))
        .any(stable)
    {
        return "需 DeepSeek（本轮）";
    }
    if cells
        .iter()
        .filter(|c| matches!(c["profile"].as_str(), Some("gpt-6.1-sol" | "opus-5.5")))
        .any(stable)
    {
        return "本轮仅 GPT 或 Opus 通过";
    }
    "全部失败（不自动归因）"
}
// AF163 prices are the user-approved scenario, not proxy billing quotes.
fn compare_price(id: &str) -> Option<(f64, f64, Option<f64>)> {
    Some(match id {
        "ds-pro" => (9.0, 27.0, Some(0.3)),
        "ds-flash" => (1.0, 4.0, Some(0.02)), // AF163 off-peak scenario.
        "gpt-6.1-sol" | "claude-sonnet-5-5" => (2.0 * 7.1, 10.0 * 7.1, None),
        "opus-5.5" => (4.0 * 7.1, 20.0 * 7.1, None),
        "qwen38-flash-think" | "qwen38-flash-nothink" => (0.8, 2.7, Some(0.1)),
        "gpt-6-luna" | "claude-haiku-5-5" => (0.10 * 7.1, 0.50 * 7.1, None),
        "qwen-27b-think" | "qwen-27b-nothink" | "qwen-9b-nothink" => (0.0, 0.0, None),
        _ => return None,
    })
}
fn compare_cost(id: &str, rows: &[Value]) -> Value {
    let price = compare_price(id);
    let mut subtotal = 0.0;
    let mut known = 0;
    let mut unknown = 0;
    let mut reasons = std::collections::BTreeSet::new();
    for r in rows {
        if !matches!(
            r["status"].as_str(),
            Some("通过" | "失败" | "调用失败" | "观察" | "未录制（中断在途）")
        ) {
            continue; // Missing/blocked/rejected slots have no known send, not fictitious fees.
        }
        let values = r["usage"]["input_tokens"]
            .as_u64()
            .zip(r["usage"]["output_tokens"].as_u64());
        let Some(((input, output), (input_rate, output_rate, cached_rate))) = values.zip(price)
        else {
            unknown += 1;
            reasons.insert("usage_or_price_unknown");
            continue;
        };
        if id == "claude-haiku-5-5" && input > 100_000 {
            unknown += 1;
            reasons.insert("haiku_price_only_approved_up_to_100k_input");
            continue;
        }
        let cached = r
            .pointer("/usage/input_tokens_details/cached_tokens")
            .and_then(Value::as_u64);
        if cached.is_some_and(|n| n > input) {
            unknown += 1;
            reasons.insert("invalid_cached_token_count");
            continue;
        }
        let cached = if cached_rate.is_some() {
            cached.unwrap_or(0)
        } else {
            0
        };
        subtotal += ((input - cached) as f64 * input_rate
            + cached as f64 * cached_rate.unwrap_or(input_rate)
            + output as f64 * output_rate)
            / 1_000_000.0;
        known += 1;
    }
    let passed = rows
        .iter()
        .filter(|r| r["observation_only"] != true && r["status"] == "通过")
        .count();
    let mut samples = std::collections::BTreeMap::<String, Vec<&Value>>::new();
    for r in rows.iter().filter(|r| r["observation_only"] != true) {
        samples
            .entry(format!("{}:{}", r["stage"], r["id"]))
            .or_default()
            .push(r);
    }
    let stable = samples
        .values()
        .filter(|rs| rs.len() == 2 && rs.iter().all(|r| r["status"] == "通过"))
        .count();
    json!({"currency":"CNY","price_revision":"AF163/AF165 / 2026-10-08","usd_cny":7.1,
        "basis":"官方价估算，代理实际计费可能不同；实际已录制 usage，失败/观察调用也计费用，推理 token 已含在 output 中",
        "input_cny_per_million":price.map(|p|p.0),"output_cny_per_million":price.map(|p|p.1),
        "cached_input_cny_per_million":price.and_then(|p|p.2),
        "assumptions":"DeepSeek Flash 按 AF163 非高峰价；已知 DS/Qwen3.8 缓存按缓存价，否则按普通输入价。其他档未计缓存创建/读取费率差异；本地仅 API 费为零，不含硬件/电费。Haiku 仅输入≤100k 的批准价。",
        "priced_attempts":known,"unknown_usage_or_price_attempts":unknown,"unknown_reasons":reasons,
        "known_subtotal_cny":subtotal,"estimated_total_cny":(unknown==0).then_some(subtotal),
        "passed_attempts":passed,"stable_passed_samples":stable,
        "cost_per_passed_attempt_cny":(unknown==0 && passed>0).then(||subtotal/passed as f64),
        "cost_per_stable_passed_sample_cny":(unknown==0 && stable>0).then(||subtotal/stable as f64),
        "denominator":"每通过样本成本按两次均通过的非观察样本数；单次通过成本另列。分子含本档所列阶段全部已发送调用费用；未录制/过期不是免费或通过。"})
}
fn af163_smoke_check(
    root: &Path,
    profiles: &[SmokeProfile],
    selected: &[String],
    variable: impl Fn(&str) -> Option<String>,
) -> Result<(), String> {
    let ids: Vec<_> = selected.iter().map(String::as_str).collect();
    let groups = compare_plan_with_smoke(root, profiles, &ids, variable, None, true)?;
    for group in groups {
        campaign_check(&group, 2)?;
        let report = evaluate_cases(&group, None, &mut |_| panic!("smoke check must not send"))?;
        let checked = smoke_report(report, &group);
        for row in items(&checked["smoke"]) {
            if row["transport_valid"] != true
                || row["provider_output_valid"] != true
                || row["mechanical_valid"] != true
                || row["usage"]["input_tokens"].as_u64().is_none()
                || row["usage"]["output_tokens"].as_u64().is_none()
            {
                return Err(format!(
                    "AF163 {} smoke {} is missing/stale or failed protocol/schema/mechanical/usage checks; no formal send permitted",
                    row["profile"], row["request"]
                ));
            }
        }
    }
    Ok(())
}
fn af166_diagnostic_rows(groups: &[Vec<Case>], reports: &[Value]) -> Vec<Value> {
    groups.iter().zip(reports).flat_map(|(group,report)| {
        let checked = smoke_report(report.clone(),group);
        items(&checked["smoke"]).iter().zip(items(&report["results"])).map(|(check,row)| {
            let raw = if row["raw"].is_null() {&row["raw_output"]} else {&row["raw"]};
            let duplicate = raw.is_object().then(||["candidates","memory_requests"].iter().any(|field| {
                let values = items(&raw[*field]);
                values.iter().enumerate().any(|(i,value)|values[..i].contains(value))
            }));
            let valid = check["transport_valid"]==true && check["provider_output_valid"]==true
                && check["mechanical_valid"]==true && duplicate==Some(false)
                && check["usage"]["input_tokens"].as_u64().is_some()
                && check["usage"]["output_tokens"].as_u64().is_some();
            let mut check = check.clone();
            check["exact_duplicate_candidates_or_requests"] = json!(duplicate);
            check["diagnostic_passed"] = json!(valid);
            check["boundary"] = json!("单个完整 JSON + 原 schema/W2 + usage + 完全相同重复项；不判语义，不证明所有输入的约束执行");
            check
        }).collect::<Vec<_>>()
    }).collect()
}
fn af166_diagnostic_check(
    root: &Path,
    profiles: &[SmokeProfile],
    selected: &[String],
    variable: impl Fn(&str) -> Option<String>,
) -> Result<(), String> {
    let ids: Vec<_> = selected.iter().map(String::as_str).collect();
    let groups = compare_plan_for_sample(root, profiles, &ids, variable, Some(SMOKE_SAMPLE))?;
    for group in &groups {
        campaign_check(group, 1)?;
    }
    let reports = groups
        .iter()
        .map(|group| evaluate_cases(group, None, &mut |_| panic!("diagnostic check sent")))
        .collect::<Result<Vec<_>, _>>()?;
    if af166_diagnostic_rows(&groups, &reports)
        .iter()
        .any(|r| r["diagnostic_passed"] != true)
    {
        return Err("AF166 Qwen3.8 diagnostic missing/stale/invalid/duplicated or usage unknown; no formal send permitted".into());
    }
    Ok(())
}
fn compare_report(groups: &[Vec<Case>], reports: &[Value], sent: usize) -> Value {
    let af163 = groups
        .iter()
        .any(|g| AF163_COMPARE_IDS.contains(&g[0].model.label.as_str()));
    let af156 = groups
        .iter()
        .any(|g| AF156_IDS.contains(&g[0].model.label.as_str()));
    let mut matrix = Vec::new();
    for (stage, sample) in compare_samples().unwrap() {
        let mut cells = Vec::new();
        for (group, report) in groups.iter().zip(reports) {
            let rows: Vec<_> = items(&report["results"])
                .iter()
                .filter(|r| r["stage"] == stage && r["id"] == sample["id"])
                .collect();
            let passed = rows.iter().filter(|r| r["status"] == "通过").count();
            let complete = rows
                .iter()
                .all(|r| matches!(r["status"].as_str(), Some("通过" | "失败" | "观察")));
            let observed = rows
                .iter()
                .filter(|r| {
                    matches!(
                        r["status"].as_str(),
                        Some("通过" | "失败" | "调用失败" | "观察")
                    )
                })
                .count();
            let distinct: std::collections::BTreeSet<_> =
                rows.iter().map(|r| r["status"].as_str()).collect();
            let mode = group[0].provider.as_ref().unwrap().structured_mode;
            let attempts: Vec<_> = rows.iter().map(|r| json!({
                "repeat":r["repeat"],"status":r["status"],"mechanical_failures":compare_mechanical(r),
                "contract_failures":r["categories"]["contract"]["failures"],
                "reference_failures":r["categories"]["reference"]["failures"],
                "semantic_failures":r["categories"]["semantic"]["failures"],
                "error":r["error"],"elapsed_ms":r["elapsed_ms"],"usage":r["usage"],
                "provider_circuit_error":r["provider_circuit_error"],
                "request":r["request"],
                "verdict_result":r["verdict_result"],"coverage_result":r["coverage_result"],
                "raw_output":r["raw_output"],"fields":r["fields"],"observed_status":r["observed_status"],
            })).collect();
            cells.push(json!({"profile":group[0].model.label,"passed":passed,"total":rows.len(),
                "reference_only":(af156 && !AF156_IDS.contains(&group[0].model.label.as_str())) || (af163 && !AF163_COMPARE_IDS.contains(&group[0].model.label.as_str())),
                "score":if sample["observation_only"]==true {"观察，不计分".to_owned()} else {format!("{passed}/{}",rows.len())},"complete":complete,
                "observed_attempts":observed,"unstable":observed==rows.len() && distinct.len()>1,
                "stability":if rows.len()==1 {"单次，稳定性未验证"} else {"仅两次观测"},
                "structured_mode":mode,"constrained_decoding":mode==StructuredMode::NativeSchema,
                "structured_output_note":if mode==StructuredMode::PromptSchema {"未使用约束解码"} else {"请求原生 schema"},
                "model_identity_note":if AF166_QWEN_IDS.contains(&group[0].model.label.as_str()) {"别名，可能漂移"} else {"固定档案"},"attempts":attempts}));
        }
        matrix.push(
            json!({"stage":stage,"id":sample["id"],"label_contract":sample["contract"],
            "eval_sample":compare_sample_id(sample["id"].as_str().unwrap()),
            "label_revision":sample["label_revision"],
            "observation_only":sample["observation_only"]==true,"observation_reason":sample["observation_reason"],
            "request_contract":format!("{stage}.v4"),"tier":if sample["observation_only"]==true {"观察（契约待定）"} else {compare_tier(&cells)},"profiles":cells}),
        );
    }
    let profile_reports: Vec<_> = groups.iter().zip(reports).map(|(group, report)| {
        let mut metrics = report["metrics"].clone();
        for metric in metrics.as_array_mut().unwrap() {
            let rows: Vec<_> = items(&report["results"]).iter().filter(|r| r["stage"] == metric["stage"]).collect();
            let usage: Value = ["input_tokens","output_tokens","reasoning_tokens"].into_iter().map(|key| {
                let path = if key == "reasoning_tokens" {"/usage/output_tokens_details/reasoning_tokens".to_owned()} else {format!("/usage/{key}")};
                let known: Vec<_> = rows.iter().filter_map(|r| r.pointer(&path).and_then(Value::as_u64)).collect();
                (key.to_owned(),json!({"known":known.len(),"unknown":rows.len()-known.len(),"known_total":(!known.is_empty()).then(||known.iter().sum::<u64>())}))
            }).collect();
            metric["usage"] = usage;
            metric["cost_estimate"] = compare_cost(&group[0].model.label, &rows.iter().map(|r|(*r).clone()).collect::<Vec<_>>());
            metric["failure_categories"] = json!({
                "mechanical":rows.iter().filter(|r| !compare_mechanical(r).is_empty()).count(),
                "reference":rows.iter().filter(|r| r["categories"]["reference"]["passed"]==false).count(),
                "semantic":rows.iter().filter(|r| r["categories"]["semantic"]["passed"]==false).count(),
            });
        }
        let provider = group[0].provider.as_ref().unwrap();
        let w3: Vec<_> = items(&report["results"]).iter().filter(|r|r["stage"]=="w3" && r["observation_only"]!=true).collect();
        let verdict = |pass|w3.iter().filter(|r|r["verdict_result"]["passed"]==pass).count();
        let w3_10: Vec<_> = w3.iter().filter(|r|r["id"]=="w3_10_value_not_supported").map(|r| {
            let reviews: Vec<_> = items(&r["raw_output"]["reviews"]).iter().filter(|r|r["candidate"]==0).collect();
            if reviews.len()==1 {reviews[0]["verdict"].clone()} else {Value::Null}
        }).collect();
        let verdict_count = |v|w3_10.iter().filter(|r|**r==v).count();
        let observations: Vec<_> = items(&report["results"]).iter().filter(|r|r["observation_only"]==true).map(|r|json!({
            "id":r["id"],"repeat":r["repeat"],"status":r["status"],"observed_status":r["observed_status"],
            "reason":r["observation_reason"],"raw_output":r["raw_output"],"fields":r["fields"],
            "mechanical_failures":compare_mechanical(r),
        })).collect();
        json!({"profile":group[0].model.label,"model":group[0].model.model,"protocol":provider.protocol,
            "reference_only":(af156 && !AF156_IDS.contains(&group[0].model.label.as_str())) || (af163 && !AF163_COMPARE_IDS.contains(&group[0].model.label.as_str())),
            "effort":group[0].model.effort,"thinking":provider.thinking,"structured_mode":provider.structured_mode,
            "structured_output_note":if provider.structured_mode==StructuredMode::PromptSchema {"未使用约束解码"} else {"请求原生 schema"},
            "model_identity_note":if AF166_QWEN_IDS.contains(&group[0].model.label.as_str()) {"别名，可能漂移"} else {"固定档案"},
            "timeout_seconds":group[0].timeout_seconds,"fixed_slots":group.len(),
            "interrupted_inflight":items(&report["results"]).iter().filter(|r|r["status"]=="未录制（中断在途）").count(),
            "circuit_not_sent":report["circuit_not_sent"],"provider_circuits":report["provider_circuits"],
            "w3_verdict":{"passed":verdict(true),"failed":verdict(false),"unavailable":w3.len()-verdict(true)-verdict(false)},
            "w3_10_verdict_distribution":{"contradicted":verdict_count("contradicted"),"insufficient":verdict_count("insufficient"),
                "supported":verdict_count("supported"),"unavailable":w3_10.len()-verdict_count("contradicted")-verdict_count("insufficient")-verdict_count("supported")},
            "observations":observations,
            "w3_coverage":{"complete":w3.iter().filter(|r|r["coverage_result"]["status"]=="complete").count(),
                "gaps":w3.iter().filter(|r|r["coverage_result"]["status"]=="gaps").count(),
                "scored_passed":w3.iter().filter(|r|r["coverage_result"]["passed"]==true).count(),
                "scored_failed":w3.iter().filter(|r|r["coverage_result"]["passed"]==false).count(),
                "unscored":w3.iter().filter(|r|r["coverage_result"]["scored"]==false).count(),
                "guard_failed":w3.iter().filter(|r|r["coverage_result"]["guard_passed"]==false).count()},
            "summary":report["summary"],"metrics":metrics,
            "stage_pass_rates":items(&report["summary"]).iter().filter(|r|r["stage"]=="w1" || r["stage"]=="w3").map(|r|json!({
                "stage":r["stage"],"passed":r["passed"],"scoring_slots":r["scoring_slots"],
                "pass_rate":r["scoring_slots"].as_u64().filter(|n|*n>0).map(|n|r["passed"].as_u64().unwrap_or(0) as f64/n as f64)})).collect::<Vec<_>>(),
            "cost_estimate":compare_cost(&group[0].model.label,items(&report["results"]))})
    }).collect();
    let unstable: Vec<_> = matrix
        .iter()
        .filter(|r| {
            r["observation_only"] != true
                && items(&r["profiles"]).iter().any(|c| c["unstable"] == true)
        })
        .map(|r| r["id"].clone())
        .collect();
    let interrupted: Vec<_> = reports
        .iter()
        .flat_map(|r| items(&r["results"]))
        .filter(|r| r["status"] == "未录制（中断在途）")
        .map(|r| {
            json!({"profile":r["model"],"stage":r["stage"],"id":r["id"],
            "repeat":r["repeat"],"request":r["request"],"status":r["status"]})
        })
        .collect();
    json!({"comparison":if af163 {"af163"} else if af156 {"af156"} else {"af137"},"purpose":"W1/W3 stage screening, not end-to-end reliability",
        "fixed_slots":groups.iter().map(Vec::len).sum::<usize>(),"sent_calls":sent,"profiles":profile_reports,"matrix":matrix,"unstable_samples":unstable,
        "scoring_samples":matrix.iter().filter(|r|r["observation_only"]!=true).count(),
        "observation_samples":matrix.iter().filter(|r|r["observation_only"]==true).count(),
        "interrupted":interrupted,
        "circuit_not_sent":reports.iter().flat_map(|r|items(&r["results"]))
            .filter(|r|r["status"]=="熔断未发送").count(),
        "results":reports.iter().flat_map(|r|items(&r["results"]).iter().cloned()).collect::<Vec<_>>()})
}
fn provider_comparison_from_env() -> Result<(), String> {
    let variable = |name: &str| std::env::var(name).ok();
    provider_comparison(variable)
}
fn af156_reference_check(report: &Value, baseline: &Value) -> Result<(), String> {
    if baseline["comparison"] != "af137"
        || baseline["fixed_slots"] != 520
        || baseline["sent_calls"] != 0
    {
        return Err("AF156 requires the zero-call AF155 520-slot reference report".into());
    }
    let identity = |report: &Value| {
        items(&report["results"])
            .iter()
            .filter(|r| COMPARE_IDS.contains(&r["model"].as_str().unwrap_or("")))
            .map(|r| {
                json!([
                    r["model"],
                    r["stage"],
                    r["id"],
                    r["repeat"],
                    r["request"],
                    r["status"],
                    r["observation_only"]
                ])
                .to_string()
            })
            .collect::<std::collections::BTreeSet<_>>()
    };
    let expected = identity(baseline);
    if expected.len() != 520 || identity(report) != expected {
        return Err(
            "AF156 historical requests/scores differ from reports-af155; no recording permitted"
                .into(),
        );
    }
    Ok(())
}
fn af156_reference(profiles: &[SmokeProfile]) -> Result<(Vec<Vec<Case>>, Vec<Value>), String> {
    let history = Path::new(env!("CARGO_MANIFEST_DIR")).join(COMPARE_ROOT);
    let groups = compare_plan(&history, profiles, |_| None)?;
    let reports = groups
        .iter()
        .map(|g| evaluate_cases(g, None, &mut |_| panic!("historical comparison sent")))
        .collect::<Result<Vec<_>, _>>()?;
    let baseline: Value = serde_json::from_slice(
        &std::fs::read(history.join("reports-af155/report.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    af156_reference_check(&compare_report(&groups, &reports, 0), &baseline)?;
    Ok((groups, reports))
}
fn provider_comparison(variable: impl Fn(&str) -> Option<String> + Copy) -> Result<(), String> {
    provider_comparison_at(Path::new(env!("CARGO_MANIFEST_DIR")), variable)
}
fn provider_comparison_at(
    base: &Path,
    variable: impl Fn(&str) -> Option<String> + Copy,
) -> Result<(), String> {
    let options = compare_options(variable)?;
    if options.af156 && !options.qwen38 && options.limit.is_some_and(|n| n > 0) && !options.dry {
        return Err("AF164: Qwen3.7-flash unavailable (no constrained decoding); remaining authorization revoked".into());
    }
    let root = base.join(if options.diagnostic && options.qwen38 {
        AF166_DIAG_ROOT
    } else if options.smoke {
        "target/eval-providers/compare-af163/smoke"
    } else if options.af163 {
        AF163_ROOT
    } else if options.diagnostic {
        AF162_ROOT
    } else if options.af156 {
        AF156_ROOT
    } else {
        COMPARE_ROOT
    });
    let profiles = evaluation_profiles(include_str!("eval/write_stage/providers.json"))?;
    let ids: &[&str] = if options.smoke {
        &AF166_PROMPT_IDS
    } else if options.af163 {
        &AF163_COMPARE_IDS
    } else if options.diagnostic && options.qwen38 {
        &AF166_QWEN_IDS
    } else if options.af156 {
        &AF156_IDS
    } else {
        &COMPARE_IDS
    };
    let diagnostic_case = options.diagnostic.then(|| options.cases[0].as_str());
    let groups = compare_plan_with_smoke(
        &root,
        &profiles,
        ids,
        variable,
        diagnostic_case,
        options.smoke,
    )?;
    if options.dry {
        println!(
            "{}",
            if options.diagnostic {
                diagnostic_dry_plan(&groups, &options)?
            } else {
                compare_dry_plan(&groups, &options)?
            }
        );
        return Ok(());
    }
    if options.limit.is_some_and(|n| n > 0)
        && options.profiles.iter().any(|id| id.starts_with("qwen-"))
    {
        return Err("historic local Qwen profiles are read-only; recording is disabled".into());
    }
    // Reject a stale historical reference before credentials, clients, freeze or sends.
    let reference = if (options.af156 && !options.diagnostic) || (options.af163 && !options.smoke) {
        Some(af156_reference(&profiles)?)
    } else {
        None
    };
    // Check every selected campaign before any client, credentials or physical sends.
    let sends = compare_preflight(&groups, &options)?;
    if sends > 0 && options.af163 && !options.smoke {
        let anthropic_and_luna: Vec<_> = options
            .profiles
            .iter()
            .filter(|id| AF163_IDS.contains(&id.as_str()))
            .cloned()
            .collect();
        if !anthropic_and_luna.is_empty() {
            af163_smoke_check(
                &root.join("smoke"),
                &profiles,
                &anthropic_and_luna,
                variable,
            )?;
        }
        let qwen: Vec<_> = options
            .profiles
            .iter()
            .filter(|id| AF166_QWEN_IDS.contains(&id.as_str()))
            .cloned()
            .collect();
        if !qwen.is_empty() {
            af166_diagnostic_check(&root.join("diag-af166"), &profiles, &qwen, variable)?;
        }
    }
    let selected_cases: Vec<_> = groups
        .iter()
        .filter(|g| options.profiles.contains(&g[0].model.label))
        .flat_map(|g| compare_subset(g, &options))
        .collect();
    let keys = if sends > 0 {
        smoke_credentials(&selected_cases, variable)?
    } else {
        Default::default()
    };
    if sends > 0 {
        for group in groups
            .iter()
            .filter(|g| options.profiles.contains(&g[0].model.label))
        {
            compare_freeze(group)?; // The full manifest is frozen even when only a few samples run.
        }
    }
    let http = if sends > 0 {
        Some(
            reqwest::blocking::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .build()
                .map_err(|e| e.to_string())?,
        )
    } else {
        None
    };
    let mut sent = 0;
    for group in groups
        .iter()
        .filter(|g| options.profiles.contains(&g[0].model.label))
    {
        let id = &group[0].model.label;
        let subset = compare_subset(group, &options);
        let limit = options
            .limit
            .map(|n| if n == 0 { 0 } else { options.quota(id) });
        let pending = preflight(&subset, limit)?;
        // Separate processes may run different cloud providers; DS and Ollama cannot overlap.
        let _resource = if pending > 0 {
            Some(w3_trial_lock(
                &root.join("locks").join(compare_resource(id)),
            )?)
        } else {
            None
        };
        let report = evaluate_cases(&subset, limit, &mut |c| {
            let request = smoke_http_request(
                http.as_ref().ok_or("offline comparison cannot send")?,
                c,
                &keys[&c.model.label],
            )?;
            let response = http.as_ref().unwrap().execute(request).map_err(|e| {
                if e.is_timeout() {
                    "provider_timeout".into()
                } else {
                    e.to_string()
                }
            })?;
            model_provider::read_http_response(response, c.provider.as_ref().unwrap().protocol)
                .map(|(_, envelope)| envelope)
                .map_err(|e| e.to_string())
        })?;
        sent += report["sent_calls"].as_u64().unwrap() as usize;
    }
    // Refresh after recording, also including unselected profiles' existing recordings.
    let mut groups = compare_plan_with_smoke(
        &root,
        &profiles,
        ids,
        variable,
        diagnostic_case,
        options.smoke,
    )?;
    let mut reports: Vec<_> = groups
        .iter()
        .map(|g| evaluate_cases(g, None, &mut |_| panic!("offline comparison sent")))
        .collect::<Result<_, _>>()?;
    let new_reports = reports.clone();
    if let Some((mut historic, mut historic_reports)) = reference {
        historic.append(&mut groups);
        historic_reports.append(&mut reports);
        groups = historic;
        reports = historic_reports;
    }
    let mut report = if options.smoke {
        let cases: Vec<_> = groups.iter().flat_map(|g| g.iter().cloned()).collect();
        let rows: Vec<_> = reports
            .iter()
            .flat_map(|r| items(&r["results"]).iter().cloned())
            .collect();
        smoke_report(
            json!({"comparison":"af163-smoke","authorized_slots":4,"formal_slots":0,
            "fixed_slots":4,"sent_calls":sent,"results":rows}),
            &cases,
        )
    } else if options.diagnostic {
        json!({"comparison":if options.qwen38 {"af166-qwen38-diagnostic"} else {"af162"},"diagnostic_only":true,"scoring_eligible":false,
            "authorized_slots":2,"formal_af156_slots":0,"fixed_slots":2,"sample":options.cases[0],"sent_calls":sent,
            "diagnostic_checks":if options.qwen38 {json!(af166_diagnostic_rows(&groups,&reports))} else {Value::Null},
            "profiles":groups.iter().map(|g|json!({"profile":g[0].model.label,"allowance":1})).collect::<Vec<_>>(),
            "results":reports.iter().flat_map(|r|items(&r["results"]).iter().cloned()).collect::<Vec<_>>()})
    } else {
        compare_report(&groups, &reports, sent)
    };
    if (options.af156 && !options.diagnostic) || (options.af163 && !options.smoke) {
        report["authorized_slots"] = json!(if options.af163 { 400 } else { 160 });
        report["historical_slots"] = json!(520);
        report["reference_report"] = json!("compare-af137/reports-af155/report.json");
        report["reference_profiles"] = json!(COMPARE_IDS);
        report["new_profile_report"] = compare_report(&groups[7..], &new_reports, sent);
    }
    if options.af163 {
        report["structured_mode_decision"] = json!({"decision":"AF166","profiles":AF166_PROMPT_IDS,
            "w1_evidence":"schema_grammar_too_large","w3_evidence":"HTTP 400 provider_error_unclassified; 原始正文未保存，无法离线确认是否 schema 问题",
            "mode":"prompt_schema","note":"未使用约束解码"});
        report["unavailable_profiles"] = json!([{"model":model_provider::DASHSCOPE_MODEL,
            "profiles":AF156_IDS,"status":"不可用：无约束解码","decision":"AF164",
            "remaining_authorization":"revoked","quality_comparison":false}]);
    }
    if sent > 0 {
        m15_write_json(
            &root
                .join("reports")
                .join(options.profiles.join("--"))
                .join("report.json"),
            &report,
        )?;
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
    );
    compare_check_report(&report, &options)?;
    Ok(())
}
fn compare_check_report(report: &Value, options: &CompareOptions) -> Result<(), String> {
    // Gate only the selected attempts, never unselected profiles or samples.
    if options.gate
        && items(&report["results"]).iter().any(|r| {
            options.profiles.iter().any(|id| r["model"] == *id)
                && options.cases.iter().any(|id| r["id"] == *id)
                && !(r["status"] == "通过"
                    || (r["observation_only"] == true && r["status"] == "观察"))
        })
    {
        return Err("AF137 selected model scores failed (EVAL_GATE=1)".into());
    }
    Ok(())
}

fn repeat_count(value: Option<&str>) -> Result<usize, String> {
    value
        .unwrap_or("1")
        .parse::<usize>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| "EVAL_REPEAT must be a positive integer".into())
}

#[derive(Clone)]
enum Replay {
    Missing,
    Interrupted,
    Stale,
    Output(String),
    Failed(Value),
    Rejected(String),
}

#[derive(Clone)]
struct Case {
    stage: String,
    sample: Value,
    request: Value,
    wire_request: Value,
    provider: Option<ProviderConfig>,
    campaign: Option<PathBuf>,
    path: PathBuf,
    file: Value,
    replay: Replay,
    record: Value,
    model: EvalModel,
    repeat: usize,
    w3_trial_root: Option<PathBuf>,
    timeout_seconds: Option<u64>,
    endpoint_env: Option<String>,
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
    plan_repeated_with_provider(selected, root, model, repeats, None)
}
fn plan_repeated_with_provider(
    selected: Vec<(String, Value)>,
    root: &Path,
    model: &EvalModel,
    repeats: usize,
    options: Option<&ProviderOptions>,
) -> Result<Vec<Case>, String> {
    if let Some(options) = options {
        options
            .provider
            .check_model(
                model
                    .model
                    .as_deref()
                    .ok_or("new provider requires EVAL_MODEL")?,
                model.effort,
            )
            .map_err(|e| e.to_string())?;
        options
            .provider
            .endpoint(
                model
                    .base
                    .as_deref()
                    .ok_or("new provider requires endpoint")?,
            )
            .map_err(|e| e.to_string())?;
    }
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
        if options.is_none() && case.0 == "r4" && case.1["fixed_input"] == true {
            // 首轮冻结 payload 中的计划已过 R1 核验；不重新调用 R1，也不读取 R4 答案。
            let models = hippocampus::recall_models::RecallModels {
                reasoning_effort: [("r4".into(), model.effort)].into(),
                ..Default::default()
            };
            selected_model.effort = models.r4_effort(Some(fixed_r4_op(&case.1)?));
        }
        for repeat in 1..=repeats {
            let mut dir = if let Some(options) = options {
                let provider = &options.provider;
                root.join(
                    serde_json::to_value(provider.protocol)
                        .unwrap()
                        .as_str()
                        .unwrap(),
                )
                .join(&selected_model.label)
                .join(match provider.thinking {
                    Some(true) => "thinking-on",
                    Some(false) => "thinking-off",
                    None => "thinking-default",
                })
            } else {
                root.join(&selected_model.label)
            };
            if selected_model.effort != ReasoningEffort::Medium {
                dir = dir.join(format!(
                    "reasoning-{}",
                    serde_json::to_value(selected_model.effort)
                        .unwrap()
                        .as_str()
                        .unwrap()
                ));
            }
            let (stage, sample) = case.clone();
            let mut request = stage_request(&stage, &sample)?;
            if let Some(id) = &selected_model.model {
                request["model"] = json!(id);
            }
            selected_model.effort.apply(&mut request);
            cases.push(case_from_request_with_provider(
                stage,
                sample,
                &dir.join(format!("repeat-{repeat}")),
                &selected_model,
                repeat,
                request,
                options,
                None,
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
    case_from_request_with_provider(stage, sample, root, model, repeat, request, None, None)
}
fn case_from_request_with_provider(
    stage: String,
    sample: Value,
    root: &Path,
    model: &EvalModel,
    repeat: usize,
    request: Value,
    options: Option<&ProviderOptions>,
    timeout_seconds: Option<u64>,
) -> Result<Case, String> {
    let provider = options.map(|o| &o.provider);
    let prepared = model_provider::provider_request(provider, &request);
    let rejection = prepared.as_ref().err().map(ToString::to_string);
    let wire_request = prepared.unwrap_or_else(|_| request.clone());
    let binding = eval_provider_binding(model, &wire_request, provider, timeout_seconds);
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
        r["request"]
            == eval_request_fingerprint(
                &sample,
                &request,
                model,
                &wire_request,
                provider,
                timeout_seconds,
            )
            && r["schema"] == request["text"]["format"]["name"]
            && r["max_output_tokens"] == request["max_output_tokens"]
            && r["base_url"] == json!(model.base)
            && r["repeat"] == repeat
            && r["endpoint"] == request["model"]
            && r["recording_version"] == if provider.is_some() { 3 } else { 2 }
            && (provider.is_none() || r["provider_binding"] == binding)
            && (sample["w3_prompt_arm"].is_null() || r["prompt_arm"] == sample["w3_prompt_arm"])
            && (sample["w3_effort_arm"].is_null()
                || (r["effort_arm"] == sample["w3_effort_arm"]
                    && r["trial"] == "af101-w3-effort"
                    && r["baseline_commit"] == "dc604f8"
                    && r["prompt_byte_fingerprint"] == W3_EFFORT_PROMPT_FINGERPRINT))
    });
    let replay = if let Some(reason) = rejection {
        Replay::Rejected(reason)
    } else if let Some(r) = record {
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
        wire_request,
        provider: provider.cloned(),
        campaign: options.map(|o| o.campaign.clone()),
        path,
        file,
        replay,
        record,
        model: model.clone(),
        repeat,
        w3_trial_root: None,
        timeout_seconds,
        endpoint_env: None,
    })
}

fn eval_provider_binding(
    model: &EvalModel,
    wire: &Value,
    provider: Option<&ProviderConfig>,
    timeout_seconds: Option<u64>,
) -> Value {
    provider.map_or(Value::Null, |provider| {
        let mut binding = json!({"adapter":model_provider::ADAPTER_VERSION,
        "provider":provider,"endpoint":provider.endpoint(model.base.as_deref().unwrap()).unwrap(),
        "model":model.model,"effort":model.effort,"request":wire});
        if let Some(seconds) = timeout_seconds {
            binding["timeout_seconds"] = json!(seconds);
        }
        if let Some(mode) = provider.structured_mode_binding() {
            binding["structured_mode"] = mode;
        }
        binding
    })
}
fn eval_request_fingerprint(
    sample: &Value,
    canonical: &Value,
    model: &EvalModel,
    wire: &Value,
    provider: Option<&ProviderConfig>,
    timeout_seconds: Option<u64>,
) -> Value {
    if provider.is_none() {
        recording_fingerprint(sample, canonical)
    } else {
        json!(hippocampus::recall_models::fingerprint(
            &eval_provider_binding(model, wire, provider, timeout_seconds)
        ))
    }
}
fn case_fingerprint(case: &Case) -> Value {
    eval_request_fingerprint(
        &case.sample,
        &case.request,
        &case.model,
        &case.wire_request,
        case.provider.as_ref(),
        case.timeout_seconds,
    )
}
fn campaign_slot(case: &Case) -> Value {
    json!({"request":case_fingerprint(case),"id":case.sample["id"],"stage":case.stage,"repeat":case.repeat})
}

fn campaign_root(cases: &[Case]) -> Result<Option<&Path>, String> {
    let root = cases.iter().find_map(|c| c.campaign.as_deref());
    if let Some(root) = root
        && cases.iter().any(|c| c.campaign.as_deref() != Some(root))
    {
        return Err("cannot mix campaigns".into());
    }
    Ok(root)
}
fn campaign_check(cases: &[Case], limit: usize) -> Result<(), String> {
    let Some(root) = campaign_root(cases)? else {
        return Ok(());
    };
    let path = root.join("authorization.json");
    if !path.exists() {
        return Ok(());
    }
    let authorization: Value =
        serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    if authorization["limit"] != limit {
        return Err("campaign allowance differs from persisted authorization".into());
    }
    for case in cases {
        if !items(&authorization["requests"]).contains(&campaign_slot(case)) {
            return Err("campaign request identity changed".into());
        }
    }
    let sends = root.join("sends");
    if sends.exists() {
        for entry in std::fs::read_dir(sends).map_err(|e| e.to_string())? {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let log: Value =
                serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            if cases.iter().any(|c| {
                matches!(c.replay, Replay::Missing | Replay::Stale)
                    && log["slot"] == campaign_slot(c)
            }) {
                return Err(
                    "campaign slot already sent but recording missing; no automatic resend".into(),
                );
            }
        }
    }
    Ok(())
}
fn campaign_start(case: &Case) -> Result<PathBuf, String> {
    let root = case.campaign.as_ref().ok_or("missing campaign")?;
    let path = root
        .join("sends")
        .join(format!("{}.json", hippocampus::diagnostics::new_id()));
    m15_write_json(
        &path,
        &json!({"state":"started","slot":campaign_slot(case),"request":case_fingerprint(case),"sample":case.sample["id"],"repeat":case.repeat}),
    )?;
    Ok(path)
}
fn normalized_envelope(provider: Option<&ProviderConfig>, envelope: &Value) -> Value {
    if envelope.get("provider_http_error").is_some() {
        return envelope.clone();
    }
    let Some(provider) = provider else {
        return envelope.clone();
    };
    let result = model_provider::provider_output(provider.protocol, envelope);
    let usage = model_provider::normalized_usage(provider.protocol, envelope);
    match result {
        Ok(text) => {
            json!({"status":"completed","usage":usage,"output":[{"content":[{"type":"output_text","text":text}]}]})
        }
        Err(hippocampus::OpenAiAdapterError::Incomplete(reason)) => {
            let mut result =
                json!({"status":"incomplete","usage":usage,"incomplete_details":{"reason":reason}});
            if let Some(diagnostic) =
                model_provider::incomplete_diagnostics(provider.protocol, envelope)
            {
                result["diagnostic_fragments"] = diagnostic;
            }
            result
        }
        Err(e) => {
            json!({"status":"failed","usage":usage,"error":e.to_string(),"incomplete_details":{"reason":e.to_string()}})
        }
    }
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct ProviderCircuit {
    error: Option<String>,
    consecutive: usize,
}
impl ProviderCircuit {
    fn observe(&mut self, envelope: &Value) {
        let detail = &envelope["provider_http_error"];
        let key = detail["http_status"]
            .as_u64()
            .filter(|s| (300..600).contains(s))
            .map(|status| json!([status, detail["error_type"], detail["code"]]).to_string())
            .or_else(|| {
                (envelope["status"] == "incomplete"
                    && envelope["incomplete_details"]["reason"] == "max_output_tokens")
                    .then(|| "incomplete:max_output_tokens".into())
            });
        self.consecutive = match &key {
            Some(_) if key == self.error => self.consecutive.saturating_add(1).min(3),
            Some(_) => 1,
            None => 0,
        };
        self.error = key;
    }
    fn open(&self) -> bool {
        self.consecutive >= 3 || self.error.as_deref() == Some(NATIVE_STOP)
    }
}
type ProviderCircuits = std::collections::BTreeMap<String, ProviderCircuit>;
const NATIVE_STOP: &str = "anthropic_native_failure; separate approval required";
fn provider_circuits(cases: &[Case]) -> Result<ProviderCircuits, String> {
    provider_circuits_with_native_stop(cases, false)
}
fn provider_circuits_with_native_stop(
    cases: &[Case],
    native_stop: bool,
) -> Result<ProviderCircuits, String> {
    let mut circuits = load_provider_circuits(cases)?;
    if native_stop {
        // Reconstruct the stop after a crash between recording and circuit persistence.
        for case in cases.iter().filter(|c| native_anthropic(c)) {
            let failed = match &case.replay {
                Replay::Failed(_) => true,
                Replay::Output(text) => {
                    native_output_failed(&score(&case.stage, &case.sample, Some(text)))
                }
                _ => false,
            };
            if failed {
                stop_native(&mut circuits, &case.model.label);
            }
        }
    }
    Ok(circuits)
}
fn native_anthropic(case: &Case) -> bool {
    case.provider.as_ref().is_some_and(|p| {
        p.protocol == ProviderProtocol::AnthropicMessages
            && p.structured_mode == StructuredMode::NativeSchema
    })
}
fn native_output_failed(row: &Value) -> bool {
    row["status"] == "调用失败"
        || items(&row["fields"])
            .iter()
            .any(|f| f["category"] == "contract" && f["passed"] == false)
}
fn stop_native(circuits: &mut ProviderCircuits, id: &str) {
    circuits.insert(
        id.into(),
        ProviderCircuit {
            error: Some(NATIVE_STOP.into()),
            consecutive: 1,
        },
    );
}
fn load_provider_circuits(cases: &[Case]) -> Result<ProviderCircuits, String> {
    let Some(root) = campaign_root(cases)? else {
        return Ok(Default::default());
    };
    let path = root.join("provider_circuit.json");
    if !path.exists() {
        return Ok(Default::default());
    }
    let state: Value = serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let authorization: Value = serde_json::from_slice(
        &std::fs::read(root.join("authorization.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    if state["authorization"] != hippocampus::recall_models::fingerprint(&authorization) {
        return Err("provider circuit authorization changed".into());
    }
    serde_json::from_value(state["profiles"].clone()).map_err(|e| e.to_string())
}
fn persist_provider_circuits(cases: &[Case], circuits: &ProviderCircuits) -> Result<(), String> {
    if let Some(root) = campaign_root(cases)? {
        let authorization: Value = serde_json::from_slice(
            &std::fs::read(root.join("authorization.json")).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        m15_write_json(
            &root.join("provider_circuit.json"),
            &json!({"authorization":hippocampus::recall_models::fingerprint(&authorization),"profiles":circuits}),
        )?;
    }
    Ok(())
}
fn circuit_blocks(case: &Case, circuits: &ProviderCircuits) -> bool {
    matches!(case.replay, Replay::Missing | Replay::Stale)
        && circuits
            .get(&case.model.label)
            .is_some_and(ProviderCircuit::open)
}

fn preflight(cases: &[Case], limit: Option<usize>) -> Result<usize, String> {
    preflight_with_native_stop(cases, limit, false)
}
fn preflight_with_native_stop(
    cases: &[Case],
    limit: Option<usize>,
    native_stop: bool,
) -> Result<usize, String> {
    let circuits = provider_circuits_with_native_stop(cases, native_stop)?;
    let needed = cases
        .iter()
        .filter(|c| {
            !circuit_blocks(c, &circuits)
                && !matches!(
                    c.replay,
                    Replay::Output(_)
                        | Replay::Failed(_)
                        | Replay::Rejected(_)
                        | Replay::Interrupted
                )
        })
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
    if let (Some(limit), Some(root)) = (limit.filter(|n| *n > 0), campaign_root(cases)?) {
        campaign_check(cases, limit)?;
        let previous = count_sends([root.join("sends")])?;
        if previous.saturating_add(needed) > limit {
            return Err(format!(
                "campaign cumulative {previous} + missing {needed} exceeds {limit}"
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
    let mut record = json!({"recording_version":if case.provider.is_some() {3} else {2},"model":case.model.label,"repeat":case.repeat,
        "reasoning_effort":case.model.effort,"elapsed_ms":elapsed_ms,
        "schema":case.request["text"]["format"]["name"],"endpoint":case.request["model"],
        "base_url":case.model.base,"max_output_tokens":case.request["max_output_tokens"],
        "request":case_fingerprint(case),"response":{
            "status":envelope["status"],"incomplete_details":envelope["incomplete_details"],"usage":envelope["usage"]}});
    if let Some(text) = text {
        record["output"] = serde_json::from_str(text).unwrap_or(json!(text));
        record["output_text"] = json!(text);
        record["usage"] = envelope["usage"].clone();
    }
    if let Some(error) = error {
        record["error"] = json!(error);
    }
    if let Some(error) = envelope.get("provider_http_error") {
        record["provider_http_error"] = error.clone();
    }
    if let Some(diagnostic) = envelope
        .get("diagnostic_fragments")
        .filter(|v| !v.is_null())
    {
        record["diagnostic_fragments"] = diagnostic.clone();
    }
    if case.provider.is_some() {
        record["provider_binding"] = eval_provider_binding(
            &case.model,
            &case.wire_request,
            case.provider.as_ref(),
            case.timeout_seconds,
        );
    }
    w3_trial_metadata(&mut record, case);
    recorded.push(record); // Includes failed attempts; reruns replay them instead of picking a success.
    file["recorded"] = json!(recorded);
    if case.sample["m15_review"] == true || case.w3_trial_root.is_some() || case.campaign.is_some()
    {
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
    if let Some(error) = record.get("provider_http_error").filter(|v| !v.is_null()) {
        report["provider_http_error"] = error.clone();
    }
    if let Some(diagnostic) = record.get("diagnostic_fragments").filter(|v| !v.is_null()) {
        report["diagnostic_fragments"] = diagnostic.clone();
    }
    report["categories"] = json!({"contract":{"passed":false,"failures":["call"]},
        "semantic":{"passed":null,"failures":[]},"reference":{"passed":null,"failures":[]}});
    report
}

fn evaluate(
    cases: &[Case],
    limit: Option<usize>,
    send: &mut dyn FnMut(&Value) -> Result<Value, String>,
) -> Result<Value, String> {
    evaluate_cases(cases, limit, &mut |case| send(&case.wire_request))
}

fn evaluate_cases(
    cases: &[Case],
    limit: Option<usize>,
    send: &mut dyn FnMut(&Case) -> Result<Value, String>,
) -> Result<Value, String> {
    evaluate_cases_with_native_stop(cases, limit, send, false)
}
fn evaluate_cases_with_native_stop(
    cases: &[Case],
    limit: Option<usize>,
    send: &mut dyn FnMut(&Case) -> Result<Value, String>,
    native_stop: bool,
) -> Result<Value, String> {
    let recall_ledger = if cases
        .iter()
        .any(|c| matches!(c.stage.as_str(), "r1" | "r4"))
    {
        recall_review::ledger()?
    } else {
        Value::Null
    };
    evaluate_cases_with_review_ledger(cases, limit, send, native_stop, &recall_ledger)
}
fn evaluate_cases_with_review_ledger(
    cases: &[Case],
    limit: Option<usize>,
    send: &mut dyn FnMut(&Case) -> Result<Value, String>,
    native_stop: bool,
    recall_ledger: &Value,
) -> Result<Value, String> {
    let mut planned = preflight_with_native_stop(cases, limit, native_stop)?;
    // The OS releases this lock on exit/crash; all arms share one fixed ledger.
    let _trial_lock = if planned > 0 {
        w3_trial_root(cases)?.map(w3_trial_lock).transpose()?
    } else {
        None
    };
    if _trial_lock.is_some() {
        planned = preflight_with_native_stop(cases, limit, native_stop)?;
    }
    let _campaign_lock = if planned > 0 {
        campaign_root(cases)?.map(w3_trial_lock).transpose()?
    } else {
        None
    };
    if _campaign_lock.is_some() {
        planned = preflight_with_native_stop(cases, limit, native_stop)?;
        let root = campaign_root(cases)?.unwrap();
        let path = root.join("authorization.json");
        if !path.exists() {
            m15_write_json(
                &path,
                &json!({"limit":limit,"requests":cases.iter().map(campaign_slot).collect::<Vec<_>>()}),
            )?;
        }
    }
    let mut circuits = provider_circuits_with_native_stop(cases, native_stop)?;
    let mut sent = 0;
    let mut results = Vec::new();
    for case in cases {
        let mut metrics = case.record.clone();
        let mut result = match &case.replay {
            Replay::Output(text) => score(&case.stage, &case.sample, Some(text)),
            Replay::Failed(record) => failed_score(case, record),
            Replay::Rejected(reason) => {
                json!({"id":case.sample["id"],"stage":case.stage,"status":"发送前拒绝","error":reason,"sent":false})
            }
            Replay::Interrupted => {
                let mut r = score(&case.stage, &case.sample, None);
                r["status"] = json!("未录制（中断在途）");
                r["sent"] = json!(false);
                r
            }
            Replay::Missing | Replay::Stale
                if circuit_blocks(case, &circuits)
                    && (limit.is_some() || matches!(case.replay, Replay::Missing)) =>
            {
                let mut r = score(&case.stage, &case.sample, None);
                r["status"] = json!("熔断未发送");
                r["sent"] = json!(false);
                r["provider_circuit_error"] = json!(circuits[&case.model.label].error);
                r
            }
            _ if limit.is_some() => {
                // A durable started entry precedes the physical send. A crash
                // without a response remains a possibly billed attempt.
                let attempt = if case.campaign.is_some() {
                    Some(campaign_start(case)?)
                } else if case.w3_trial_root.is_some() {
                    Some(w3_start_send(case)?)
                } else if case.sample["m15_review"] == true {
                    Some(m15_start_send(case)?)
                } else {
                    None
                };
                sent += 1;
                let started = std::time::Instant::now();
                let response = send(case);
                let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                let envelope = response
                    .as_ref()
                    .ok()
                    .map(|e| normalized_envelope(case.provider.as_ref(), e))
                    .unwrap_or(Value::Null);
                // Persist HTTP/truncation streaks before another send, including across restart.
                circuits
                    .entry(case.model.label.clone())
                    .or_default()
                    .observe(&envelope);
                if native_stop && native_anthropic(case) && envelope["status"] != "completed" {
                    stop_native(&mut circuits, &case.model.label);
                }
                persist_provider_circuits(cases, &circuits)?;
                let pending_log = if let Some(path) = attempt {
                    let mut entry = json!({"state":"responded","request":case_fingerprint(case),
                        "elapsed_ms":elapsed_ms,"usage":envelope["usage"],"status":envelope["status"],"error":response.as_ref().err()});
                    if let Some(error) = envelope.get("provider_http_error") {
                        entry["provider_http_error"] = error.clone();
                    }
                    if case.w3_trial_root.is_some() {
                        entry["sample"] = case.sample["id"].clone();
                        entry["repeat"] = json!(case.repeat);
                        entry["model"] = case.request["model"].clone();
                        entry["reasoning_effort"] = json!(case.model.effort);
                        w3_trial_metadata(&mut entry, case);
                    }
                    if case.campaign.is_some() {
                        entry["slot"] = campaign_slot(case);
                    }
                    Some((path, entry))
                } else {
                    None
                };
                metrics = json!({"elapsed_ms":elapsed_ms,"usage":envelope["usage"],
                    "provider_http_error":envelope["provider_http_error"]});
                let text = response.and_then(|e| {
                    if let Some(error) = e.get("provider_http_error") {
                        return Err(format!("provider HTTP error: {error}"));
                    }
                    if let Some(provider) = &case.provider {
                        let text = model_provider::provider_output(provider.protocol, &e)
                            .map_err(|e| e.to_string())?;
                        let parsed = serde_json::from_str(&text)
                            .map_err(|e| format!("invalid JSON: {e}"))?;
                        model_provider::check_output(
                            &case.request["text"]["format"]["schema"],
                            &parsed,
                        )
                        .map_err(|e| e.to_string())?;
                        Ok(text)
                    } else {
                        completed_text(&e)
                    }
                });
                let scored = match text {
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
                            &json!({"error":error,"provider_http_error":envelope["provider_http_error"],"diagnostic_fragments":envelope.get("diagnostic_fragments"),"response":{
                            "status":envelope["status"],"incomplete_details":envelope["incomplete_details"],"usage":envelope["usage"]}}),
                        )
                    }
                };
                if native_stop && native_anthropic(case) && native_output_failed(&scored) {
                    stop_native(&mut circuits, &case.model.label);
                    persist_provider_circuits(cases, &circuits)?;
                }
                if let Some((path, entry)) = pending_log {
                    m15_write_json(&path, &entry)?;
                }
                scored
            }
            Replay::Missing => score(&case.stage, &case.sample, None),
            Replay::Stale => {
                let mut r = score(&case.stage, &case.sample, None);
                r["status"] = json!("录制过期");
                r
            }
        };
        result["model"] = json!(case.model.label);
        mark_observation(&case.sample, &mut result);
        if let Some(revision) = case.sample.get("label_revision") {
            result["label_revision"] = revision.clone();
        }
        result["reasoning_effort"] = json!(case.model.effort);
        result["elapsed_ms"] = metrics["elapsed_ms"].clone();
        result["usage"] = metrics
            .get("usage")
            .filter(|u| !u.is_null())
            .cloned()
            .unwrap_or_else(|| metrics["response"]["usage"].clone());
        result["repeat"] = json!(case.repeat);
        result["max_output_tokens"] = case.request["max_output_tokens"].clone();
        result["request"] = case_fingerprint(case);
        if let Some(error) = metrics.get("provider_http_error").filter(|v| !v.is_null()) {
            result["provider_http_error"] = error.clone();
        }
        if let Some(provider) = &case.provider {
            result["structured_mode"] = json!(provider.structured_mode);
        }
        w3_trial_metadata(&mut result, case);
        recall_review::apply(case, &mut result, recall_ledger);
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
            "provider_circuits":circuits,"circuit_not_sent":results.iter().filter(|r|r["status"]=="熔断未发送").count(),
            "reasoning_effort":cases.first().filter(|first|cases.iter().all(|c|c.model.effort==first.model.effort)).map(|c|c.model.effort),
            "reasoning_efforts":cases.iter().map(|c|serde_json::to_value(c.model.effort).unwrap().as_str().unwrap().to_owned()).collect::<std::collections::BTreeSet<_>>(),
            "metrics":stage_metrics(&results),
            "preflight_rejections":{"context_exceeded":results.iter().filter(|r|r["error"]=="model_context_exceeded").count(),
                "context_unproven":results.iter().filter(|r|r["error"]=="model_context_unproven").count(),
                "total":results.iter().filter(|r|r["status"]=="发送前拒绝").count()},
            "raw_summary":stage_summaries(&raw_results,cases),"raw_splits":split_reports(&raw_results),
            "review_summary":recall_review::summary(&results),
            "summary":summary,"splits":split_reports(&results),"per_sample":per_sample(&results),"results":results}),
    )
}

fn stage_summaries(results: &[Value], cases: &[Case]) -> Vec<Value> {
    ["w1", "w3", "w4", "r1", "r4"].into_iter()
        .filter(|stage| !stage.starts_with('r') || cases.iter().any(|c|c.stage==*stage)).map(|stage| {
        let all: Vec<_> = results.iter().filter(|r| r["stage"] == stage).collect();
        let rows: Vec<_> = all.iter().copied().filter(|r|r["observation_only"]!=true).collect();
        let count = |status| rows.iter().filter(|r| r["status"] == status).count();
        let all_count = |status| all.iter().filter(|r| r["status"] == status).count();
        json!({"stage": stage, "passed": count("通过"), "total": all.len(),"scoring_slots":rows.len(),
            "observation_only":all.len()-rows.len(),
            "observed":all.iter().filter(|r|r["status"]=="观察").count(),
            "observation_failed":all.iter().filter(|r|r["observation_only"]==true && matches!(r["status"].as_str(),Some("失败"|"调用失败"))).count(),
            "evaluated": count("通过") + count("失败"), "failed": count("失败") + count("调用失败"),
            "missing": all_count("未录制"), "stale": all_count("录制过期"), "invalid_input": all_count("输入无效"),
            "circuit_not_sent":all_count("熔断未发送"),
            "draft": all.iter().filter(|r| r["label_status"] == "draft").count()})
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
    if std::env::var_os("EVAL_AF268").is_some() {
        af268::run().unwrap();
        return;
    }
    if std::env::var_os("EVAL_AF264").is_some() {
        af264::run().unwrap();
        return;
    }
    if std::env::var_os("EVAL_RECALL_COMPARE").is_some() {
        recall_compare::run_from_env().unwrap();
        return;
    }
    if std::env::var_os("EVAL_AF216").is_some() {
        af216::run().unwrap();
        return;
    }
    if std::env::var_os("EVAL_AF206").is_some() {
        af206::run().unwrap();
        return;
    }
    if std::env::var_os("EVAL_AF198").is_some() {
        af198::run().unwrap();
        return;
    }
    if std::env::var_os("EVAL_AF193").is_some() || std::env::var_os("EVAL_AF196").is_some() {
        af193::run().unwrap();
        return;
    }
    if std::env::var_os("EVAL_COMPARE").is_some() {
        provider_comparison_from_env().unwrap();
        return;
    }
    if std::env::var_os("EVAL_SMOKE").is_some() {
        provider_smoke_from_env().unwrap();
        return;
    }
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
        plan_repeated_with_provider(
            selected,
            &root,
            &model,
            repeats,
            provider_options_from_env().unwrap().as_ref(),
        )
        .unwrap()
    };
    let limit = std::env::var("EVAL_RECORD").ok().map(|s| {
        s.parse::<usize>()
            .expect("EVAL_RECORD must be a nonnegative integer")
    });
    let sends = preflight(&cases, limit).unwrap();
    let first = cases.first().expect("nonempty stage plan");
    let model = &first.model;
    let provider = first.provider.as_ref();
    // No client or credentials when there are no sends, including fully cached EVAL_RECORD=0.
    let live = (sends > 0).then(|| {
        let base = model
            .base
            .as_ref()
            .expect("EVAL_RECORD requires EVAL_BASE_URL or HUOSHAN_DEEPSEEK_BASE_URL");
        let key = std::env::var("EVAL_API_KEY")
            .or_else(|_| {
                if let Some(provider) = provider {
                    provider
                        .api_key_env
                        .as_ref()
                        .map_or(Ok(String::new()), |name| {
                            std::env::var(evaluation_env(name, model.model.as_deref()))
                        })
                } else {
                    std::env::var("HUOSHAN_DEEPSEEK_API_KEY")
                }
            })
            .expect("EVAL_RECORD requires EVAL_API_KEY or HUOSHAN_DEEPSEEK_API_KEY");
        assert!(
            !key.trim().is_empty()
                || provider.is_some_and(|p| p.protocol == ProviderProtocol::OpenaiChat),
            "evaluation API key must not be empty"
        );
        let url = if let Some(provider) = provider {
            provider.endpoint(base).unwrap()
        } else {
            format!("{}/responses", base.trim_end_matches('/'))
        };
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
        let protocol = provider.map_or(ProviderProtocol::OpenaiResponses, |p| p.protocol);
        let response = hippocampus::provider_http_request(http, url, key, protocol)
            .json(request)
            .timeout(if m15 || provider.is_some() {
                std::time::Duration::from_secs(360)
            } else {
                eval_request_timeout(request)
            })
            .send()
            .map_err(|e| e.to_string())?;
        model_provider::read_http_response(response, protocol)
            .map(|(_, envelope)| envelope)
            .map_err(|e| e.to_string())
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
        .filter(|c| {
            !matches!(
                c.replay,
                Replay::Output(_) | Replay::Failed(_) | Replay::Rejected(_)
            )
        })
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
        .or_else(|| std::env::var("HUOSHAN_DEEPSEEK_BASE_URL").ok())
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
            "w3_v4",
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
    println!("固定 Pro、minimal、w3.v4、said_on=2026-10-07、UTC+8、max_output_tokens=16384。");
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
        .or_else(|| std::env::var("HUOSHAN_DEEPSEEK_BASE_URL").ok())
        .map(|s| s.trim_end_matches('/').to_owned());
    w3_effort_plan(&w3_effort_root(), base)
}

fn w3_effort_plan(root: &Path, base: Option<String>) -> Result<Vec<Case>, String> {
    if w3_prompt_fingerprint(W3_EFFORT_PROMPT) != W3_EFFORT_PROMPT_FINGERPRINT {
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
        af193::replace_prompt(&mut frozen, ModelStage::W3, W3_EFFORT_PROMPT);
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
        "固定 Pro、w3.v4、said_on=2026-10-07、UTC+8、max_output_tokens=16384；两臂仅 reasoning.effort 不同。"
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
        w3_prompt_fingerprint(W3_EFFORT_PROMPT),
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
            assert_eq!(case.request["text"]["format"]["name"], "w3_v4");
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
                    .ends_with(W3_EFFORT_PROMPT)
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
        if request["text"]["format"]["name"] == "r4_v7"
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
        ("r4_v7", "low", "count", 240),
        ("r4_v7", "low", "sum", 240),
        ("r4_v7", "low", "elapsed", 240),
        ("r4_v7", "low", "compare", 240),
        ("r4_v7", "low", "lookup", 180),
        ("r4_v7", "low", "list", 180),
        ("r4_v7", "low", "invalid", 180),
        ("r4_v7", "minimal", "count", 180),
        ("r4_v7", "high", "count", 180),
        ("r1_v2", "low", "count", 180),
        ("w3_v4", "low", "count", 180),
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
    if items(&report["summary"]).iter().any(|s| {
        s["invalid_input"] != 0
            || (gate && (s["failed"] != 0 || s["observation_failed"].as_u64().unwrap_or(0) > 0))
    }) {
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
                "uncertainty",
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
    if std::env::var_os("EVAL_RECALL_COMPARE").is_some() {
        recall_compare::dry_run_from_env().unwrap();
        return;
    }
    let stages = stages_from_env();
    let prefix = std::env::var("EVAL_CASES").unwrap_or_default();
    let root = recording_root();
    let model = EvalModel::from_env().unwrap();
    let repeats = repeat_count(std::env::var("EVAL_REPEAT").ok().as_deref()).unwrap();
    let cases = plan_repeated_with_provider(
        selected(&stages, &prefix).unwrap(),
        &root,
        &model,
        repeats,
        provider_options_from_env().unwrap().as_ref(),
    )
    .unwrap();
    if cases.iter().any(|c| c.provider.is_some()) {
        println!(
            "provider: {:?}；发送前拒绝 {}（上下文超限 {}，无法证明 {}）。",
            cases[0].provider.as_ref().unwrap().protocol,
            cases
                .iter()
                .filter(|c| matches!(c.replay, Replay::Rejected(_)))
                .count(),
            cases
                .iter()
                .filter(|c| matches!(&c.replay,Replay::Rejected(s) if s=="model_context_exceeded"))
                .count(),
            cases
                .iter()
                .filter(|c| matches!(&c.replay,Replay::Rejected(s) if s=="model_context_unproven"))
                .count()
        );
        let limit = std::env::var("EVAL_RECORD")
            .ok()
            .map(|s| s.parse().expect("EVAL_RECORD integer"));
        preflight(&cases, limit).unwrap();
    }
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
            .filter(|c| !matches!(
                c.replay,
                Replay::Output(_) | Replay::Failed(_) | Replay::Rejected(_)
            ))
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
            if sample["observation_only"] == true {
                "观察"
            } else {
                "通过"
            },
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
    let mut sample = sample.clone();
    if sample["expect"].get("alternatives").is_some() {
        sample["expect"] = expectation_branches(&sample["expect"]).remove(0);
    }
    let scalar = |v: &Value| {
        if v.is_array() {
            v[0].clone()
        } else {
            v.clone()
        }
    };
    json!({"reviews": items(&sample["expect"]["reviews"]).iter().map(|r| json!({
        "candidate": r["candidate"], "verdict": scalar(&r["verdict"]), "episode_value": r["episode_value"],
        "problems": items(&r["aspects"]).iter().map(|a| json!({"aspect": a,
            "quote": {"text": sample["input"]["text"]}})).collect::<Vec<_>>()
    })).collect::<Vec<_>>(), "coverage": {"status": sample["expect"]["coverage"]["status"],
        "missing": if sample["expect"]["coverage"]["status"]=="gaps" {items(&sample["expect"]["coverage"]["missing_contains"]).iter().map(adapter::quote).collect::<Vec<_>>()} else {vec![]}},
        "request_reviews": items(&sample["expect"]["request_reviews"]).iter().map(|r|json!({"request":r["request"],"verdict":scalar(&r["verdict"])})).collect::<Vec<_>>()})
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
    let branch = expectation_branches(&sample["expect"]);
    let e = &branch[0];
    let subject = pick(&e["subject"]);
    let subject = if subject.is_string() {
        json!({"entity_id":subject})
    } else if subject.get("unresolved").is_some() {
        json!({"unresolved":{"text":subject["unresolved"]}})
    } else {
        subject
    };
    let selected_time = pick(&e["time"]);
    let time = if selected_time.is_null() {
        Value::Null
    } else {
        json!({"role":"event","quote":{"text":selected_time["quote"]},"interp":selected_time["interp"],"anchor":"said_on"})
    };
    json!({"need":pick(&e["need"]),"current_evidence":pick(&e["current_evidence"]),"type":pick(&e["type"]),"subject":subject,
        "relation":"stage query relation","conditions":items(&e["conditions"]).iter().map(|q|json!({"quote":{"text":pick(q)},"resolved":true})).collect::<Vec<_>>(),
        "time":time,"plan":{"op":pick(&e["plan.op"]),"filter":{"relation":pick(&e["plan.filter.relation"]),"time":null},"unit":if e.get("unit").is_some(){pick(&e["unit"])}else{Value::Null}}})
}
fn fake_r4(sample: &Value) -> Value {
    let mut sample = sample.clone();
    sample["expect"] = expectation_branches(&sample["expect"]).remove(0);
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
        if let Some(RecordView::Current(r)) = f
            .evidence
            .records
            .iter()
            .find(|r| e["record_ids"] == json!([r.id().0]))
        {
            let candidate = serde_json::to_value(&r.candidate).unwrap();
            fact["statement"] = candidate["value"]["text"].clone();
            fact["times"] = candidate["times"].clone();
        }
        for (key, v) in e["must"].as_object().unwrap() {
            if key == "time_quotes" || key == "time_kinds" {
                if items(&fact["times"]).is_empty() {
                    fact["times"] = json!(items(&e["must"]["time_quotes"]).iter().map(|q| json!({"role":"event","quote":{"text":q},"interp":{"kind":"unknown"},"anchor":"said_on"})).collect::<Vec<_>>());
                }
                continue;
            }
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
                "event_group":if let Some(group) = items(&expected["groups"]).iter().position(|g| evidence.iter().any(|ev| items(g).contains(&json!(match &ev.source {SourceRef::Stored{source_id}=>source_id.clone(),_=>String::new()})))) {format!("g{group}")}else if op=="count" && expected["distinct_groups"]==2 && i<2 {"same".to_owned()}else{format!("g{i}")}})
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
            "coverage":if expected.get("coverage").is_some(){if expected["coverage"].is_array(){expected["coverage"][0].clone()}else{expected["coverage"].clone()}}else{json!("complete")}});
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
    assert_eq!(selected.len(), 44);
    let cases = plan(selected.clone(), &temp.0).unwrap();
    assert_eq!(preflight(&cases, None).unwrap(), 0);
    assert!(preflight(&cases, Some(43)).is_err());
    let report = evaluate(&cases, Some(44), &mut |request| {
        let case = cases.iter().find(|c| c.request == *request).unwrap();
        Ok(fake_envelope(if case.stage == "r1" {
            fake_r1(&case.sample)
        } else {
            fake_r4(&case.sample)
        }))
    })
    .unwrap();
    for row in items(&report["splits"]) {
        assert_eq!(
            row["total"],
            if row["split"] == "dev" {
                14
            } else if row["stage"] == "r4" {
                10
            } else {
                6
            }
        );
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
    assert_eq!(changed.len(), 88);
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
    assert_eq!(preflight(&changed, Some(88)).unwrap(), 88);
    assert!(preflight(&changed, Some(87)).is_err());
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
        ("w1", "w1_v4", "w1.v4"),
        ("w3", "w3_v4", "w3.v4"),
        ("w4", "w4_v5", "w4.v5"),
        ("r1", "r1_v2", "r1.v2"),
        ("r4", "r4_v7", "r4.v7"),
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
                "w3" => "w3.v4",
                "r1" => "r1.v2",
                "r4" => "r4.v7",
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

fn provider_eval_fixture(
    temp: &TempRecords,
    protocol: ProviderProtocol,
) -> (EvalModel, ProviderOptions) {
    let mut model = EvalModel::default_model();
    model.label = "fixture-provider".into();
    model.base = Some("https://fixture.invalid/v1".into());
    model.model = Some(
        match protocol {
            ProviderProtocol::OpenaiResponses => "gpt-6.1-sol",
            ProviderProtocol::AnthropicMessages => "claude-opus-5-5",
            ProviderProtocol::OpenaiChat => "qwen3.8:27b-q4_K_M",
        }
        .into(),
    );
    if protocol == ProviderProtocol::OpenaiChat {
        model.effort = ReasoningEffort::Omit;
    }
    let options = ProviderOptions {
        campaign: temp.0.join("campaign"),
        provider: ProviderConfig {
            protocol,
            structured_mode: Default::default(),
            base_url_env: "FIXTURE_BASE".into(),
            api_key_env: None,
            model_revision: Some("fixture-model-digest".into()),
            thinking: if protocol == ProviderProtocol::OpenaiChat {
                Some(false)
            } else {
                None
            },
            context_tokens: if protocol == ProviderProtocol::OpenaiChat {
                Some(65536)
            } else {
                None
            },
            context_proof: Some(model_provider::ContextProof {
                tokenizer_revision: "fixture-tokenizer".into(),
                template_token_bound: 64,
                byte_level_bpe: true,
            }),
        },
    };
    (model, options)
}
#[test]
fn af119_stage_providers_record_and_strictly_replay_without_thinking_bodies() {
    for protocol in [
        ProviderProtocol::OpenaiResponses,
        ProviderProtocol::AnthropicMessages,
        ProviderProtocol::OpenaiChat,
    ] {
        let temp = TempRecords::new();
        let (model, options) = provider_eval_fixture(&temp, protocol);
        let chosen = selected("w1", "w1_01").unwrap();
        let cases = plan_repeated_with_provider(chosen.clone(), &temp.0, &model, 1, Some(&options))
            .unwrap();
        assert!(!options.campaign.exists());
        let output = "{\"candidates\":[],\"memory_requests\":[]}";
        let report=evaluate(&cases,Some(1),&mut |wire| {
            assert_eq!(wire["model"],model.model.as_deref().unwrap());
            Ok(match protocol {
                ProviderProtocol::OpenaiResponses=>json!({"status":"completed","usage":{"input_tokens":1,"output_tokens":2},"output":[{"content":[{"type":"output_text","text":output}]}]}),
                ProviderProtocol::AnthropicMessages=>json!({"stop_reason":"end_turn","usage":{"input_tokens":1,"cache_read_input_tokens":3,"output_tokens":2},"content":[{"type":"thinking","thinking":"PRIVATE_THOUGHT"},{"type":"text","text":output}]}),
                ProviderProtocol::OpenaiChat=>json!({"usage":{"prompt_tokens":1,"completion_tokens":2},"choices":[{"finish_reason":"stop","message":{"content":output,"reasoning":"PRIVATE_THOUGHT"}}]}),
            })
        }).unwrap();
        assert_eq!(report["sent_calls"], 1);
        let stored = std::fs::read_to_string(&cases[0].path).unwrap();
        assert!(!stored.contains("PRIVATE_THOUGHT"));
        let stored: Value = serde_json::from_str(&stored).unwrap();
        assert_eq!(stored["recorded"][0]["recording_version"], 3);
        assert_eq!(
            stored["recorded"][0]["provider_binding"]["provider"]["protocol"],
            serde_json::to_value(protocol).unwrap()
        );
        let replay =
            plan_repeated_with_provider(chosen.clone(), &temp.0, &model, 1, Some(&options))
                .unwrap();
        assert!(matches!(replay[0].replay, Replay::Output(_)));
        assert_eq!(
            evaluate(&replay, None, &mut |_| panic!(
                "offline replay must not send"
            ))
            .unwrap()["sent_calls"],
            0
        );
        let mut changed = options.clone();
        changed.provider.model_revision = Some("changed".into());
        let stale =
            plan_repeated_with_provider(chosen, &temp.0, &model, 1, Some(&changed)).unwrap();
        assert!(matches!(stale[0].replay, Replay::Stale));
        assert!(preflight(&stale, Some(1)).is_err());
    }
}
#[test]
fn af119_stage_context_rejections_never_send_or_create_campaign() {
    let temp = TempRecords::new();
    let (model, mut options) = provider_eval_fixture(&temp, ProviderProtocol::OpenaiChat);
    for (tokens, proof, code) in [
        (1, true, "model_context_exceeded"),
        (65536, false, "model_context_unproven"),
    ] {
        options.provider.context_tokens = Some(tokens);
        if !proof {
            options.provider.context_proof = None;
        }
        let cases = plan_repeated_with_provider(
            selected("w1", "w1_01").unwrap(),
            &temp.0,
            &model,
            1,
            Some(&options),
        )
        .unwrap();
        let report = evaluate(&cases, Some(0), &mut |_| {
            panic!("context rejection cannot call model")
        })
        .unwrap();
        assert_eq!(report["sent_calls"], 0);
        assert_eq!(report["results"][0]["error"], code);
        assert_eq!(report["preflight_rejections"]["total"], 1);
        assert!(!options.campaign.exists());
    }
}
#[test]
fn af119_campaign_retains_started_allowance_and_never_resends_missing_recordings() {
    let temp = TempRecords::new();
    let (model, options) = provider_eval_fixture(&temp, ProviderProtocol::OpenaiResponses);
    let chosen = selected("w1", "w1_01").unwrap();
    let cases =
        plan_repeated_with_provider(chosen.clone(), &temp.0, &model, 1, Some(&options)).unwrap();
    m15_write_json(
        &options.campaign.join("authorization.json"),
        &json!({"limit":2,"requests":cases.iter().map(campaign_slot).collect::<Vec<_>>()}),
    )
    .unwrap();
    campaign_start(&cases[0]).unwrap();
    assert_eq!(count_sends([options.campaign.join("sends")]).unwrap(), 1);
    assert!(
        preflight(&cases, Some(2))
            .unwrap_err()
            .contains("already sent")
    );
    assert!(
        evaluate(&cases, Some(2), &mut |_| panic!(
            "unknown attempt cannot be retried"
        ))
        .is_err()
    );
    assert!(
        preflight(&cases, Some(3))
            .unwrap_err()
            .contains("allowance")
    );
    // Even a valid recording lost after a completed response must not cause an implicit resend.
    let other = TempRecords::new();
    let (model, options) = provider_eval_fixture(&other, ProviderProtocol::OpenaiResponses);
    let cases =
        plan_repeated_with_provider(chosen.clone(), &other.0, &model, 1, Some(&options)).unwrap();
    evaluate(&cases,Some(2),&mut |_|Ok(json!({"status":"completed","output":[{"content":[{"type":"output_text","text":"{\"candidates\":[],\"memory_requests\":[]}"}]}]}))).unwrap();
    std::fs::remove_file(&cases[0].path).unwrap();
    let missing = plan_repeated_with_provider(chosen, &other.0, &model, 1, Some(&options)).unwrap();
    assert!(preflight(&missing, Some(2)).is_err());
}

#[test]
fn af123_profiles_are_fixed_and_dry_planning_never_reads_keys_or_writes() {
    let profiles = smoke_profiles(include_str!("eval/write_stage/providers.json")).unwrap();
    assert_eq!(profiles.len(), 8);
    assert_eq!(profiles[4].model, "qwen3.8:27b-q4_K_M-ctx64k");
    assert_eq!(profiles[5].model, profiles[4].model);
    for broken in [
        "[]".to_owned(),
        include_str!("eval/write_stage/providers.json").replace("32768", "8192"),
        include_str!("eval/write_stage/providers.json").replace("16384", "8192"),
        include_str!("eval/write_stage/providers.json").replace("0.40.0", "0.39.0"),
        include_str!("eval/write_stage/providers.json").replace("131072", "65536"),
    ] {
        assert!(smoke_profiles(&broken).is_err());
    }
    assert!(smoke_options(|name| (name == "EVAL_SMOKE").then(|| "1".into())).is_ok());
    for key in [
        "EVAL_SMOKE",
        "EVAL_RECORD",
        "EVAL_CASES",
        "EVAL_API_KEY",
        "EVAL_BASE_URL",
        "EVAL_REPEAT",
        "EVAL_MAX_OUTPUT_TOKENS",
        "EVAL_PROVIDER_CONFIG",
    ] {
        assert!(
            smoke_options(|name| if name == key {
                Some("9".into())
            } else {
                (name == "EVAL_SMOKE").then(|| "1".into())
            })
            .is_err(),
            "{key}"
        );
    }
    let root =
        std::env::temp_dir().join(format!("smoke-dry-{}", hippocampus::diagnostics::new_id()));
    let cases = smoke_plan(&root, &profiles, |name| {
        assert!(name.ends_with("_BASE_URL"), "planning cannot read {name}");
        None
    })
    .unwrap();
    let plan = smoke_dry_plan(&cases);
    assert!(plan.contains("固定槽位/累计授权上限: 8"));
    assert!(plan.contains("本次真实调用: 0"));
    assert!(
        cases
            .iter()
            .all(|c| c.stage == "w1" && c.repeat == 1 && c.request["max_output_tokens"] == 32768)
    );
    assert!(!root.exists());
    assert!(cases.iter().all(|c| matches!(c.replay, Replay::Missing)));
    assert_eq!(smoke_preflight(&cases, Some(8)).unwrap(), 8);
    // The approved alias must survive planning and wire conversion unchanged.
    for c in cases
        .iter()
        .filter(|c| c.model.label.starts_with("qwen-27b"))
    {
        assert_eq!(c.request["model"], "qwen3.8:27b-q4_K_M-ctx64k");
        assert_eq!(c.wire_request["model"], c.request["model"]);
    }
}

fn smoke_fixture_profiles() -> Vec<SmokeProfile> {
    smoke_profiles(include_str!("eval/write_stage/providers.json")).unwrap()
}
fn smoke_fixture_variable(name: &str) -> Option<String> {
    Some(
        match name {
            "HUOSHAN_DEEPSEEK_BASE_URL" => "https://ds.fixture.invalid/v1",
            "OPENAI_BASE_URL" => "https://gpt.fixture.invalid/v1",
            "ANTHROPIC_BASE_URL" => "https://opus.fixture.invalid",
            "QWEN_LOCAL_BASE_URL" => "http://127.0.0.1:11434/v1",
            "HUOSHAN_DEEPSEEK_API_KEY" => "fixture-ds-key",
            "OPENAI_API_KEY" => "fixture-gpt-key",
            "ANTHROPIC_API_KEY" => "fixture-opus-key",
            _ => panic!("unexpected credential lookup: {name}"),
        }
        .into(),
    )
}
fn smoke_fixture_response(c: &Case) -> Value {
    let text = "{\"candidates\":[],\"memory_requests\":[]}";
    match c.provider.as_ref().unwrap().protocol {
        ProviderProtocol::OpenaiResponses => {
            json!({"status":"completed","usage":{"input_tokens":1,"output_tokens":2},"output":[{"type":"message","content":[{"type":"output_text","text":text}]}]})
        }
        ProviderProtocol::AnthropicMessages => {
            json!({"stop_reason":"end_turn","usage":{"input_tokens":1,"cache_read_input_tokens":3,"output_tokens":2},"content":[{"type":"thinking","thinking":"PRIVATE_THOUGHT"},{"type":"text","text":text}]})
        }
        ProviderProtocol::OpenaiChat => {
            json!({"usage":{"prompt_tokens":1,"completion_tokens":2},"choices":[{"finish_reason":"stop","message":{"content":text,"reasoning":"PRIVATE_THOUGHT"}}]})
        }
    }
}
#[test]
fn af123_mixed_smoke_routes_each_slot_and_strictly_replays_one_shared_campaign() {
    let temp = TempRecords::new();
    let profiles = smoke_fixture_profiles();
    let cases = smoke_plan(&temp.0, &profiles, smoke_fixture_variable).unwrap();
    assert_eq!(smoke_preflight(&cases, Some(8)).unwrap(), 8);
    let keys = smoke_credentials(&cases, smoke_fixture_variable).unwrap();
    let http = reqwest::blocking::Client::builder()
        .no_proxy()
        .build()
        .unwrap();
    let mut sent = Vec::new();
    let report = evaluate_cases(&cases, Some(8), &mut |c| {
        let request = smoke_http_request(&http, c, &keys[&c.model.label]).unwrap();
        assert_eq!(
            request.timeout().copied(),
            Some(std::time::Duration::from_secs(c.timeout_seconds.unwrap()))
        );
        assert_eq!(
            request.headers()[reqwest::header::USER_AGENT],
            model_provider::MODEL_USER_AGENT
        );
        let body: Value =
            serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap();
        assert_eq!(body["model"], c.request["model"]);
        match c.provider.as_ref().unwrap().protocol {
            ProviderProtocol::AnthropicMessages => {
                assert_eq!(request.url().host_str(), Some("opus.fixture.invalid"));
                assert_eq!(request.url().path(), "/v1/messages");
                assert_eq!(request.headers()["x-api-key"], "fixture-opus-key");
                assert!(request.headers().get("authorization").is_none());
                assert_eq!(body["max_tokens"], 32768);
            }
            ProviderProtocol::OpenaiChat => {
                assert_eq!(
                    request.url().as_str(),
                    "http://127.0.0.1:11434/v1/chat/completions"
                );
                assert!(request.headers().get("authorization").is_none());
                assert_eq!(
                    body["reasoning_effort"],
                    if c.provider.as_ref().unwrap().thinking == Some(true) {
                        "medium"
                    } else {
                        "none"
                    }
                );
                assert_eq!(body["max_tokens"], 32768);
            }
            ProviderProtocol::OpenaiResponses => {
                let gpt = c.model.label == "gpt-6.1-sol";
                assert_eq!(
                    request.url().host_str(),
                    Some(if gpt {
                        "gpt.fixture.invalid"
                    } else {
                        "ds.fixture.invalid"
                    })
                );
                assert_eq!(
                    request.headers()["authorization"],
                    if gpt {
                        "Bearer fixture-gpt-key"
                    } else {
                        "Bearer fixture-ds-key"
                    }
                );
                assert_eq!(body["max_output_tokens"], 32768);
                assert!(body["instructions"].is_string());
            }
        }
        sent.push(c.model.label.clone());
        Ok(smoke_fixture_response(c))
    })
    .unwrap();
    assert_eq!(
        sent.iter().map(String::as_str).collect::<Vec<_>>(),
        SMOKE_IDS
    );
    assert_eq!(report["sent_calls"], 8);
    let report = smoke_report(report, &cases);
    for r in items(&report["smoke"]) {
        assert_eq!(r["provider_output_valid"], true);
        assert_eq!(r["mechanical_valid"], true);
        assert_eq!(r["sample_score"], "失败"); // Empty extraction is not a semantic pass.
    }
    let auth: Value =
        serde_json::from_slice(&std::fs::read(temp.0.join("campaign/authorization.json")).unwrap())
            .unwrap();
    assert_eq!(auth["limit"], 8);
    assert_eq!(items(&auth["requests"]).len(), 8);
    assert_eq!(count_sends([temp.0.join("campaign/sends")]).unwrap(), 8);
    for c in &cases {
        assert!(
            !std::fs::read_to_string(&c.path)
                .unwrap()
                .contains("PRIVATE_THOUGHT")
        );
    }
    let replay = smoke_plan(&temp.0, &profiles, smoke_fixture_variable).unwrap();
    assert!(
        smoke_credentials(&replay, |_| panic!("cached replay cannot read keys"))
            .unwrap()
            .is_empty()
    );
    let replay_report = evaluate_cases(&replay, Some(0), &mut |_| {
        panic!("offline localhost request")
    })
    .unwrap();
    assert_eq!(replay_report["results"], report["results"]);
    assert_eq!(replay_report["sent_calls"], 0);
    let mut changed = profiles;
    changed[0].provider.model_revision = Some("changed-provider".into());
    let stale = smoke_plan(&temp.0, &changed, smoke_fixture_variable).unwrap();
    assert!(matches!(stale[0].replay, Replay::Stale));
    assert!(
        smoke_preflight(&stale, Some(8))
            .unwrap_err()
            .contains("identity")
    );
}
#[test]
fn af123_context_rejection_and_crash_consume_no_extra_slots() {
    let temp = TempRecords::new();
    let profiles = smoke_fixture_profiles();
    let mut rejected = profiles.clone();
    rejected[6].provider.context_proof = None;
    rejected[7].provider.context_tokens = Some(1);
    let cases = smoke_plan(&temp.0, &rejected, smoke_fixture_variable).unwrap();
    assert!(matches!(&cases[6].replay,Replay::Rejected(s) if s=="model_context_unproven"));
    assert!(matches!(&cases[7].replay,Replay::Rejected(s) if s=="model_context_exceeded"));
    assert!(smoke_preflight(&cases, Some(8)).is_err());
    assert!(!temp.0.join("campaign").exists());
    let cases = smoke_plan(&temp.0, &profiles, smoke_fixture_variable).unwrap();
    let mut sent = 0;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        evaluate_cases(&cases, Some(8), &mut |c| {
            sent += 1;
            if sent == 3 {
                panic!("simulated crash after durable started");
            }
            Ok(smoke_fixture_response(c))
        })
        .unwrap();
    }));
    assert!(result.is_err());
    assert_eq!(count_sends([temp.0.join("campaign/sends")]).unwrap(), 3);
    let resumed = smoke_plan(&temp.0, &profiles, smoke_fixture_variable).unwrap();
    assert!(matches!(resumed[0].replay, Replay::Output(_)));
    assert!(
        smoke_preflight(&resumed, Some(8))
            .unwrap_err()
            .contains("already sent")
    );
    assert!(
        evaluate_cases(&resumed, Some(8), &mut |_| panic!(
            "must not resend unknown attempt"
        ))
        .is_err()
    );
    assert_eq!(
        evaluate_cases(&resumed, None, &mut |_| panic!("offline crash replay")).unwrap()["sent_calls"],
        0
    );
}

#[test]
fn af128_selected_smoke_has_its_own_allowance_and_preserves_expired_history() {
    let selected_text = "ds-flash,opus-5.5,qwen-27b-think,qwen-9b-think";
    let (dry, limit, selected) = smoke_options(|name| match name {
        "EVAL_SMOKE" | "EVAL_DRY_RUN" => Some("1".into()),
        "EVAL_SMOKE_PROFILES" => Some(selected_text.into()),
        "EVAL_RECORD" => Some("4".into()),
        _ => None,
    })
    .unwrap();
    assert!(dry);
    assert_eq!(limit, Some(4));
    assert_eq!(selected.join(","), selected_text);
    assert_eq!(
        smoke_selection(Some("qwen-9b-think,ds-flash,opus-5.5,qwen-27b-think")).unwrap(),
        selected
    );
    for bad in ["", "ds-flash,ds-flash", "unknown", "ds-flash,"] {
        assert!(smoke_selection(Some(bad)).is_err());
    }
    assert!(
        smoke_options(|name| match name {
            "EVAL_SMOKE" => Some("1".into()),
            "EVAL_SMOKE_PROFILES" => Some(selected_text.into()),
            "EVAL_RECORD" => Some("8".into()),
            _ => None,
        })
        .is_err()
    );

    let temp = TempRecords::new();
    let history = temp.0.join(SMOKE_ROOT);
    let profiles = smoke_fixture_profiles();
    let mut old_profiles = profiles.clone();
    for p in &mut old_profiles {
        p.max_output_tokens.insert("w1".into(), 8192);
        p.timeout_seconds = 360;
    }
    let old = smoke_plan(&history, &old_profiles, smoke_fixture_variable).unwrap();
    evaluate_cases(&old, Some(8), &mut |c| Ok(smoke_fixture_response(c))).unwrap();
    let mut old_paths: Vec<_> = old.iter().map(|c| c.path.clone()).collect();
    old_paths.push(history.join("campaign/authorization.json"));
    old_paths.extend(
        std::fs::read_dir(history.join("campaign/sends"))
            .unwrap()
            .map(|p| p.unwrap().path()),
    );
    let original: Vec<_> = old_paths
        .iter()
        .map(|p| std::fs::read(p).unwrap())
        .collect();
    let profiles: Vec<_> = profiles
        .into_iter()
        .filter(|p| selected.contains(&p.id))
        .collect();
    let root = smoke_root(&temp.0, &selected);
    let cases =
        smoke_plan_with_history(&root, &history, &profiles, smoke_fixture_variable).unwrap();
    assert!(cases.iter().all(|c| matches!(c.replay, Replay::Stale)));
    assert_eq!(
        cases
            .iter()
            .map(|c| c.timeout_seconds.unwrap())
            .collect::<Vec<_>>(),
        [360, 360, 1800, 1800]
    );
    assert_eq!(smoke_preflight(&cases, Some(4)).unwrap(), 4);
    assert!(smoke_preflight(&cases, Some(8)).is_err());
    assert_ne!(cases[0].campaign, old[0].campaign);
    let plan = smoke_dry_plan(&cases);
    assert!(plan.contains("4 档 × W1 × 1 次"));
    assert!(plan.contains("累计授权上限: 4；待录制: 4；发送前拒绝: 0"));
    assert!(plan.contains("输出预留上限: 131072 token"));
    assert!(!root.exists());
    let http = reqwest::blocking::Client::builder()
        .no_proxy()
        .build()
        .unwrap();
    let keys = smoke_credentials(&cases, smoke_fixture_variable).unwrap();
    let report = evaluate_cases(&cases, Some(4), &mut |c| {
        let request = smoke_http_request(&http, c, &keys[&c.model.label]).unwrap();
        assert_eq!(
            request.timeout().copied(),
            Some(std::time::Duration::from_secs(c.timeout_seconds.unwrap()))
        );
        assert_eq!(c.request["max_output_tokens"], 32768);
        Ok(smoke_fixture_response(c))
    })
    .unwrap();
    assert_eq!(report["sent_calls"], 4);
    let auth: Value =
        serde_json::from_slice(&std::fs::read(root.join("campaign/authorization.json")).unwrap())
            .unwrap();
    assert_eq!(auth["limit"], 4);
    assert_eq!(items(&auth["requests"]).len(), 4);
    assert_eq!(count_sends([root.join("campaign/sends")]).unwrap(), 4);
    let replay =
        smoke_plan_with_history(&root, &history, &profiles, smoke_fixture_variable).unwrap();
    assert!(replay.iter().all(|c| matches!(c.replay, Replay::Output(_))));
    assert_eq!(
        evaluate_cases(&replay, Some(0), &mut |_| panic!("must strictly replay")).unwrap()["sent_calls"],
        0
    );
    let mut changed_timeout = profiles;
    changed_timeout[2].timeout_seconds += 1;
    let changed = smoke_plan(&root, &changed_timeout, smoke_fixture_variable).unwrap();
    assert_eq!(changed[2].wire_request, replay[2].wire_request);
    assert_ne!(case_fingerprint(&changed[2]), case_fingerprint(&replay[2]));
    assert!(matches!(changed[2].replay, Replay::Stale));
    assert!(
        smoke_preflight(&changed, Some(4))
            .unwrap_err()
            .contains("identity")
    );
    for (path, bytes) in old_paths.iter().zip(original) {
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }
    assert_eq!(count_sends([history.join("campaign/sends")]).unwrap(), 8);
    assert_eq!(count_sends([root.join("campaign/sends")]).unwrap(), 4);
}

#[test]
fn af132_only_opus_changes_identity_and_gets_an_independent_single_slot() {
    assert!(
        smoke_options(|name| match name {
            "EVAL_SMOKE" => Some("1".into()),
            "EVAL_SMOKE_PROFILES" => Some("ds-flash,opus-5.5,qwen-27b-think,qwen-9b-think".into()),
            "EVAL_RECORD" => Some("4".into()),
            _ => None,
        })
        .is_err()
    );
    let temp = TempRecords::new();
    let profiles = smoke_fixture_profiles();
    let mut native_profiles = profiles.clone();
    native_profiles[3].provider.structured_mode = StructuredMode::NativeSchema;
    let history = temp.0.join("history");
    let native = smoke_plan(&history, &native_profiles, smoke_fixture_variable).unwrap();
    evaluate_cases(&native, Some(8), &mut |c| Ok(smoke_fixture_response(c))).unwrap();
    let native_bytes: Vec<_> = native
        .iter()
        .map(|c| std::fs::read(&c.path).unwrap())
        .collect();
    let changed = smoke_plan(&history, &profiles, smoke_fixture_variable).unwrap();
    for (i, (old, new)) in native.iter().zip(&changed).enumerate() {
        if i == 3 {
            assert_ne!(case_fingerprint(old), case_fingerprint(new));
            assert!(matches!(new.replay, Replay::Stale));
            let binding = eval_provider_binding(
                &new.model,
                &new.wire_request,
                new.provider.as_ref(),
                new.timeout_seconds,
            );
            assert_eq!(
                binding["structured_mode"]["instruction_version"],
                model_provider::PROMPT_SCHEMA_VERSION
            );
        } else {
            assert_eq!(case_fingerprint(old), case_fingerprint(new));
            assert_eq!(old.wire_request, new.wire_request);
            assert!(matches!(new.replay, Replay::Output(_)));
        }
    }
    let (_, limit, selected) = smoke_options(|name| match name {
        "EVAL_SMOKE" | "EVAL_RECORD" => Some("1".into()),
        "EVAL_SMOKE_PROFILES" => Some("opus-5.5".into()),
        _ => None,
    })
    .unwrap();
    assert_eq!(limit, Some(1));
    let root = smoke_root(&temp.0, &selected);
    assert!(root.starts_with(temp.0.join(OPUS_PROMPT_SMOKE_ROOT)));
    assert_ne!(root, temp.0.join(SMOKE_CURRENT_ROOT).join("opus-5.5"));
    let opus = &profiles[3..4];
    let cases = smoke_plan_with_history(&root, &history, opus, smoke_fixture_variable).unwrap();
    assert!(matches!(cases[0].replay, Replay::Stale));
    assert_eq!(smoke_preflight(&cases, Some(1)).unwrap(), 1);
    assert!(smoke_preflight(&cases, Some(2)).is_err());
    assert!(smoke_dry_plan(&cases).contains("未使用约束解码"));
    assert!(!root.exists());
    let report = evaluate_cases(&cases, Some(1), &mut |c| {
        assert!(c.wire_request["output_config"].get("format").is_none());
        let body = json!({"error":{"type":"invalid_request_error",
            "message":"The compiled grammar is too large. PRIVATE_TEXT secret-key"}});
        Ok(model_provider::safe_http_error(
            400,
            body.to_string().as_bytes(),
            Some("req_12345678"),
            ProviderProtocol::AnthropicMessages,
        ))
    })
    .unwrap();
    let report = smoke_report(report, &cases);
    assert_eq!(report["sent_calls"], 1);
    assert_eq!(report["smoke"][0]["structured_mode"], "prompt_schema");
    assert_eq!(report["smoke"][0]["constrained_decoding"], false);
    assert_eq!(report["smoke"][0]["transport_valid"], false);
    assert_eq!(
        report["smoke"][0]["provider_http_error"]["code"],
        "schema_grammar_too_large"
    );
    assert!(report["smoke"][0]["usage"].is_null());
    assert_eq!(count_sends([root.join("campaign/sends")]).unwrap(), 1);
    let authorization: Value =
        serde_json::from_slice(&std::fs::read(root.join("campaign/authorization.json")).unwrap())
            .unwrap();
    assert_eq!(authorization["limit"], 1);
    for path in std::iter::once(cases[0].path.clone()).chain(
        std::fs::read_dir(root.join("campaign/sends"))
            .unwrap()
            .map(|p| p.unwrap().path()),
    ) {
        let text = std::fs::read_to_string(path).unwrap();
        assert!(text.contains("schema_grammar_too_large"));
        assert!(!text.contains("PRIVATE_TEXT"));
        assert!(!text.contains("secret-key"));
    }
    let replay = smoke_plan_with_history(&root, &history, opus, smoke_fixture_variable).unwrap();
    assert!(matches!(replay[0].replay, Replay::Failed(_)));
    let replay_report = smoke_report(
        evaluate_cases(&replay, Some(1), &mut |_| {
            panic!("failed slot cannot resend")
        })
        .unwrap(),
        &replay,
    );
    assert_eq!(replay_report["sent_calls"], 0);
    assert_eq!(replay_report["results"], report["results"]);
    assert_eq!(replay_report["smoke"], report["smoke"]);
    for (c, bytes) in native.iter().zip(native_bytes) {
        assert_eq!(std::fs::read(&c.path).unwrap(), bytes);
    }
}

#[test]
fn af132_prompt_schema_outputs_are_checked_without_repair_and_failures_replay() {
    for (text, valid) in [
        ("{\"candidates\":[],\"memory_requests\":[]}", true),
        (
            "```json\n{\"candidates\":[],\"memory_requests\":[]}\n```",
            false,
        ),
        ("Here: {\"candidates\":[],\"memory_requests\":[]}", false),
        ("{\"candidates\":[],\"memory_requests\":[]} trailing", false),
        (
            "{\"candidates\":[],\"memory_requests\":[],\"extra\":true}",
            false,
        ),
        (
            "{\"candidates\":[{\"kind\":\"invalid\"}],\"memory_requests\":[]}",
            false,
        ),
    ] {
        let temp = TempRecords::new();
        let profiles = smoke_fixture_profiles();
        let cases = smoke_plan(&temp.0, &profiles[3..4], smoke_fixture_variable).unwrap();
        let report = evaluate_cases(&cases, Some(1), &mut |_| {
            Ok(json!({"stop_reason":"end_turn",
            "usage":{"input_tokens":1,"output_tokens":2},"content":[{"type":"text","text":text}]}))
        })
        .unwrap();
        let report = smoke_report(report, &cases);
        assert_eq!(report["smoke"][0]["transport_valid"], true);
        assert_eq!(report["smoke"][0]["provider_output_valid"], valid);
        if valid {
            assert_eq!(report["smoke"][0]["mechanical_valid"], true);
            assert_eq!(report["smoke"][0]["semantic_score"]["passed"], false);
        }
        let replay = smoke_plan(&temp.0, &profiles[3..4], smoke_fixture_variable).unwrap();
        let replay_report = evaluate_cases(&replay, Some(1), &mut |_| {
            panic!("completed or failed output cannot resend")
        })
        .unwrap();
        assert_eq!(replay_report["sent_calls"], 0);
        assert_eq!(replay_report["results"], report["results"]);
    }
}

#[test]
fn af132_non_json_http_errors_preserve_reported_usage_and_do_not_echo_text() {
    for (body, expected, usage) in [
        (
            "<html>PRIVATE_TEXT secret-key</html>".to_owned(),
            "provider_error_non_json",
            Value::Null,
        ),
        (
            json!({"error":{"type":"PRIVATE_TEXT","message":"PRIVATE_TEXT secret-key"},
            "usage":{"input_tokens":4,"output_tokens":0}})
            .to_string(),
            "provider_error_unclassified",
            json!({"input_tokens":4,"output_tokens":0}),
        ),
    ] {
        let temp = TempRecords::new();
        let profiles = smoke_fixture_profiles();
        let cases = smoke_plan(&temp.0, &profiles[3..4], smoke_fixture_variable).unwrap();
        let report = evaluate_cases(&cases, Some(1), &mut |_| {
            Ok(model_provider::safe_http_error(
                502,
                body.as_bytes(),
                None,
                ProviderProtocol::AnthropicMessages,
            ))
        })
        .unwrap();
        assert_eq!(
            report["results"][0]["provider_http_error"]["code"],
            expected
        );
        assert_eq!(report["results"][0]["usage"], usage);
        assert!(!report.to_string().contains("PRIVATE_TEXT"));
        assert!(
            !std::fs::read_to_string(&cases[0].path)
                .unwrap()
                .contains("secret-key")
        );
        let replay = smoke_plan(&temp.0, &profiles[3..4], smoke_fixture_variable).unwrap();
        assert_eq!(
            evaluate_cases(&replay, Some(1), &mut |_| panic!(
                "HTTP failure cannot resend"
            ))
            .unwrap()["results"],
            report["results"]
        );
    }
}

fn af156_fixture_variable(name: &str) -> Option<String> {
    match name {
        "QWEN_BASE_URL" => Some("https://dashscope.aliyuncs.com/compatible-mode".into()),
        "QWEN_API_KEY" => Some("fixture-cloud-key".into()),
        _ => smoke_fixture_variable(name),
    }
}
fn af156_fixture_options() -> CompareOptions {
    compare_options(|name| match name {
        "EVAL_COMPARE" => Some("af156".into()),
        "EVAL_RECORD" => Some("160".into()),
        _ => None,
    })
    .unwrap()
}
fn af160_http_error(status: u16, kind: &str, message: &str) -> Value {
    model_provider::safe_http_error(
        status,
        json!({"error":{"type":kind,"message":message}})
            .to_string()
            .as_bytes(),
        None,
        ProviderProtocol::OpenaiChat,
    )
}
fn af162_truncated_response() -> Value {
    json!({"choices":[{"finish_reason":"length","message":{
        "content":format!("HEAD{}TAIL","汉".repeat(5000)),"reasoning_content":format!("{}END","想".repeat(2000))}}],
        "usage":{"prompt_tokens":2353,"completion_tokens":19131,"completion_tokens_details":{"reasoning_tokens":2744}}})
}
fn af162_fixture_options() -> CompareOptions {
    compare_options(|key| match key {
        "EVAL_COMPARE" => Some("af162".into()),
        "EVAL_CASES" => Some("w1_01".into()),
        "EVAL_RECORD" => Some("2".into()),
        _ => None,
    })
    .unwrap()
}
#[test]
fn af162_incomplete_records_only_bounded_diagnostics_and_trips_after_three() {
    let temp = TempRecords::new();
    let profiles = evaluation_profiles(include_str!("eval/write_stage/providers.json")).unwrap();
    let groups = compare_plan_for(&temp.0, &profiles, &AF156_IDS, af156_fixture_variable).unwrap();
    compare_freeze(&groups[0]).unwrap();
    let mut calls = 0;
    let report = evaluate_cases(&groups[0], Some(80), &mut |_| {
        calls += 1;
        assert!(calls <= 3);
        Ok(af162_truncated_response())
    })
    .unwrap();
    assert_eq!(calls, 3);
    assert_eq!(report["sent_calls"], 3);
    assert_eq!(report["circuit_not_sent"], 77);
    assert_eq!(
        count_sends([groups[0][0].campaign.as_ref().unwrap().join("sends")]).unwrap(),
        3
    );
    for case in &groups[0][..3] {
        let record: Value = serde_json::from_slice(&std::fs::read(&case.path).unwrap()).unwrap();
        let row = &record["recorded"][0];
        assert_eq!(row["response"]["status"], "incomplete");
        assert_eq!(
            row["response"]["incomplete_details"]["reason"],
            "max_output_tokens"
        );
        assert_eq!(row["response"]["usage"]["output_tokens"], 19131);
        assert_eq!(
            row["response"]["usage"]["output_tokens_details"]["reasoning_tokens"],
            2744
        );
        assert!(row.get("output").is_none() && row.get("output_text").is_none());
        assert_eq!(row["diagnostic_fragments"]["diagnostic_only"], true);
        assert_eq!(row["diagnostic_fragments"]["scoring_eligible"], false);
        assert!(
            row["diagnostic_fragments"]["output_head"]
                .as_str()
                .unwrap()
                .len()
                <= 2048
        );
        assert!(
            row["diagnostic_fragments"]["output_tail"]
                .as_str()
                .unwrap()
                .len()
                <= 2048
        );
        assert!(
            row["diagnostic_fragments"]["reasoning_tail"]
                .as_str()
                .unwrap()
                .len()
                <= 1024
        );
    }
    assert!(
        items(&report["results"])[..3]
            .iter()
            .all(|r| r["status"] == "调用失败")
    );
    let resumed = compare_plan_for(&temp.0, &profiles, &AF156_IDS, af156_fixture_variable).unwrap();
    let replay = evaluate_cases(&resumed[0], Some(0), &mut |_| {
        panic!("truncated slot retried")
    })
    .unwrap();
    assert_eq!(replay["sent_calls"], 0);
    assert_eq!(replay["results"], report["results"]);
    let mut circuit = ProviderCircuit::default();
    let incomplete =
        json!({"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"}});
    circuit.observe(&incomplete);
    circuit.observe(&incomplete);
    circuit.observe(&json!({"status":"incomplete","incomplete_details":{"reason":"other"}}));
    assert_eq!(circuit.consecutive, 0);
    circuit.observe(&incomplete);
    assert_eq!(circuit.consecutive, 1);
    circuit.observe(&af160_http_error(
        400,
        "invalid_request_error",
        "different class",
    ));
    assert_eq!(circuit.consecutive, 1);
    // Even an apparently complete, valid answer must not be scored when the provider marks length.
    compare_freeze(&groups[1]).unwrap();
    let mut complete = compare_fixture_response(&groups[1][0]);
    complete["choices"][0]["finish_reason"] = json!("length");
    let short = evaluate_cases(&groups[1][..1], Some(80), &mut |_| Ok(complete.clone())).unwrap();
    assert_eq!(short["results"][0]["status"], "调用失败");
    assert_eq!(
        short["results"][0]["diagnostic_fragments"]["scoring_eligible"],
        false
    );
}
#[test]
fn af162_single_sample_has_independent_two_slot_authorization_and_strict_resume() {
    let temp = TempRecords::new();
    let profiles = evaluation_profiles(include_str!("eval/write_stage/providers.json")).unwrap();
    let formal = temp.0.join("formal");
    let full = compare_plan_for(&formal, &profiles, &AF156_IDS, af156_fixture_variable).unwrap();
    for group in &full {
        compare_freeze(group).unwrap();
    }
    let before: Vec<_> = full
        .iter()
        .map(|g| std::fs::read(g[0].campaign.as_ref().unwrap().join("authorization.json")).unwrap())
        .collect();
    let options = af162_fixture_options();
    let root = formal.join("diag-af162");
    let groups = compare_plan_for_sample(
        &root,
        &profiles,
        &AF156_IDS,
        af156_fixture_variable,
        Some(&options.cases[0]),
    )
    .unwrap();
    assert_eq!(groups.iter().map(Vec::len).sum::<usize>(), 2);
    assert_eq!(compare_preflight(&groups, &options).unwrap(), 2);
    assert!(
        diagnostic_dry_plan(&groups, &options)
            .unwrap()
            .contains("待录制: 2；拒绝: 0")
    );
    assert!(!root.exists());
    for (group, full_group) in groups.iter().zip(&full) {
        assert_eq!(group[0].wire_request, full_group[0].wire_request);
        assert_eq!(
            case_fingerprint(&group[0]),
            case_fingerprint(&full_group[0])
        );
        assert_ne!(group[0].campaign, full_group[0].campaign);
        compare_freeze(group).unwrap();
        let report =
            evaluate_cases(group, Some(1), &mut |_| Ok(af162_truncated_response())).unwrap();
        assert_eq!(report["sent_calls"], 1);
        assert_eq!(
            count_sends([group[0].campaign.as_ref().unwrap().join("sends")]).unwrap(),
            1
        );
        let auth: Value = serde_json::from_slice(
            &std::fs::read(
                group[0]
                    .campaign
                    .as_ref()
                    .unwrap()
                    .join("authorization.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(auth["limit"], 1);
        assert_eq!(items(&auth["requests"]).len(), 1);
    }
    let resumed = compare_plan_for_sample(
        &root,
        &profiles,
        &AF156_IDS,
        af156_fixture_variable,
        Some(&options.cases[0]),
    )
    .unwrap();
    assert_eq!(compare_preflight(&resumed, &options).unwrap(), 0);
    for group in &resumed {
        assert_eq!(
            evaluate_cases(group, Some(1), &mut |_| panic!("diagnostic resent")).unwrap()["sent_calls"],
            0
        );
    }
    let changed = compare_plan_for_sample(
        &root,
        &profiles,
        &AF156_IDS,
        af156_fixture_variable,
        Some("w1_02_uncertainty"),
    )
    .unwrap();
    assert!(compare_preflight(&changed, &options).is_err());
    for (group, bytes) in full.iter().zip(before) {
        assert_eq!(
            std::fs::read(
                group[0]
                    .campaign
                    .as_ref()
                    .unwrap()
                    .join("authorization.json")
            )
            .unwrap(),
            bytes
        );
        assert_eq!(
            count_sends([group[0].campaign.as_ref().unwrap().join("sends")]).unwrap(),
            0
        );
    }
    for bad in ["3", "80", "160"] {
        assert!(
            compare_options(|key| match key {
                "EVAL_COMPARE" => Some("af162".into()),
                "EVAL_RECORD" => Some(bad.into()),
                _ => None,
            })
            .is_err()
        );
    }
    for bad in ["w1_", "w1_01,w1_02", ""] {
        assert!(
            compare_options(|key| match key {
                "EVAL_COMPARE" => Some("af162".into()),
                "EVAL_CASES" => Some(bad.into()),
                _ => None,
            })
            .is_err()
        );
    }
    provider_comparison_at(&temp.0, |key| match key {
        "EVAL_COMPARE" => Some("af162".into()),
        "EVAL_DRY_RUN" => Some("1".into()),
        "EVAL_RECORD" => Some("2".into()),
        "EVAL_CASES" => Some("w1_01".into()),
        "QWEN_BASE_URL" => Some("https://dashscope.aliyuncs.com/compatible-mode".into()),
        "QWEN_API_KEY" => panic!("dry diagnostic read a key"),
        _ => None,
    })
    .unwrap();
}
#[test]
fn af160_three_same_errors_stop_unbilled_slots_and_survive_restart() {
    let temp = TempRecords::new();
    let profiles = evaluation_profiles(include_str!("eval/write_stage/providers.json")).unwrap();
    let groups = compare_plan_for(&temp.0, &profiles, &AF156_IDS, af156_fixture_variable).unwrap();
    compare_freeze(&groups[0]).unwrap();
    let mut calls = 0;
    // Two failures persisted in an earlier invocation; restarting must not reset the streak.
    evaluate_cases(&groups[0][..2], Some(80), &mut |_| {
        calls += 1;
        Ok(af160_http_error(
            400,
            "invalid_request_error",
            "PRIVATE_TEXT first failure",
        ))
    })
    .unwrap();
    let resumed = compare_plan_for(&temp.0, &profiles, &AF156_IDS, af156_fixture_variable).unwrap();
    let report = evaluate_cases(&resumed[0], Some(80), &mut |_| {
        calls += 1;
        assert!(calls <= 3, "circuit sent a fourth bad request");
        Ok(af160_http_error(
            400,
            "invalid_request_error",
            "different provider prose",
        ))
    })
    .unwrap();
    assert_eq!(calls, 3);
    assert_eq!(report["sent_calls"], 1);
    assert_eq!(report["circuit_not_sent"], 77);
    assert_eq!(items(&report["results"])[2]["status"], "调用失败");
    assert!(
        items(&report["results"])[3..]
            .iter()
            .all(|r| r["status"] == "熔断未发送" && r["sent"] == false)
    );
    let campaign = temp.0.join("campaigns").join(AF156_IDS[0]);
    assert_eq!(count_sends([campaign.join("sends")]).unwrap(), 3);
    assert_eq!(resumed[0].iter().filter(|c| c.path.exists()).count(), 3);
    assert!(!report.to_string().contains("PRIVATE_TEXT"));
    let final_groups =
        compare_plan_for(&temp.0, &profiles, &AF156_IDS, af156_fixture_variable).unwrap();
    assert_eq!(
        compare_preflight(&final_groups, &af156_fixture_options()).unwrap(),
        80
    );
    let dry = compare_dry_plan(&final_groups, &af156_fixture_options()).unwrap();
    assert!(dry.contains("熔断未发送: 77"));
    for limit in [None, Some(0), Some(80)] {
        let replay = evaluate_cases(&final_groups[0], limit, &mut |_| {
            panic!("open circuit resent")
        })
        .unwrap();
        assert_eq!(replay["sent_calls"], 0);
        assert_eq!(replay["results"], report["results"]);
        let comparison = compare_report(&final_groups[..1], &[replay], 0);
        assert_eq!(comparison["circuit_not_sent"], 77);
        assert_eq!(comparison["profiles"][0]["circuit_not_sent"], 77);
    }
    // A separate profile can send, retaining its original 80-slot authorization.
    compare_freeze(&final_groups[1]).unwrap();
    let other=evaluate_cases(&final_groups[1][..1],Some(80),&mut |_| {
        Ok(json!({"choices":[{"finish_reason":"stop","message":{"content":"{\"candidates\":[],\"memory_requests\":[]}"}}]}))
    }).unwrap();
    assert_eq!(other["sent_calls"], 1);
    assert_eq!(other["circuit_not_sent"], 0);
    assert_eq!(count_sends([campaign.join("sends")]).unwrap(), 3);
    let auth_path = campaign.join("authorization.json");
    let mut auth: Value = serde_json::from_slice(&std::fs::read(&auth_path).unwrap()).unwrap();
    auth["limit"] = json!(81);
    m15_write_json(&auth_path, &auth).unwrap();
    assert!(provider_circuits(&final_groups[0]).is_err());
}
#[test]
fn af160_error_class_and_success_reset_the_streak_not_semantic_scores() {
    let mut circuit = ProviderCircuit::default();
    let a = af160_http_error(400, "invalid_request_error", "ordinary error");
    let b = af160_http_error(400, "authentication_error", "ordinary error");
    let c = af160_http_error(401, "authentication_error", "ordinary error");
    let d = af160_http_error(
        401,
        "authentication_error",
        "The compiled grammar is too large",
    );
    for error in [&a, &b, &c, &d] {
        circuit.observe(error);
        assert_eq!(circuit.consecutive, 1);
        circuit.observe(error);
        assert_eq!(circuit.consecutive, 2);
        assert!(!circuit.open());
    }
    for non_provider in [
        json!({"status":"completed"}),
        json!({"error":"invalid JSON"}),
        Value::Null,
    ] {
        circuit.observe(&non_provider);
        assert_eq!(circuit.consecutive, 0);
        circuit.observe(&a);
        assert_eq!(circuit.consecutive, 1);
    }
    let temp = TempRecords::new();
    let profiles = evaluation_profiles(include_str!("eval/write_stage/providers.json")).unwrap();
    let groups = compare_plan_for(&temp.0, &profiles, &AF156_IDS, af156_fixture_variable).unwrap();
    compare_freeze(&groups[0]).unwrap();
    let mut calls = 0;
    let report=evaluate_cases(&groups[0][..8],Some(80),&mut |_| {
        calls+=1;
        match calls {
            1|2|4|5|7|8=>Ok(a.clone()),
            // Valid provider envelopes with bad JSON/schema/semantics do not count as HTTP errors.
            3=>Ok(json!({"choices":[{"finish_reason":"stop","message":{"content":"bad JSON"}}]})),
            _=>Ok(json!({"choices":[{"finish_reason":"stop","message":{"content":"{\"candidates\":[],\"memory_requests\":[]}"}}]})),
        }
    }).unwrap();
    assert_eq!(calls, 8);
    assert_eq!(report["circuit_not_sent"], 0);
    assert_eq!(
        provider_circuits(&groups[0]).unwrap()[AF156_IDS[0]].consecutive,
        2
    );
}
#[test]
fn af156_plan_is_160_snapshot_slots_and_requires_explicit_auth_without_keys_in_dry_run() {
    let temp = TempRecords::new();
    let profiles = evaluation_profiles(include_str!("eval/write_stage/providers.json")).unwrap();
    assert_eq!(profiles.len(), 15);
    let options = af156_fixture_options();
    let groups = compare_plan_for(&temp.0, &profiles, &AF156_IDS, |name| {
        assert_eq!(name, "QWEN_BASE_URL");
        None
    })
    .unwrap();
    assert_eq!(groups.iter().map(Vec::len).sum::<usize>(), 160);
    assert_eq!(
        groups
            .iter()
            .flatten()
            .map(|c| c.request["max_output_tokens"].as_u64().unwrap())
            .sum::<u64>(),
        3_932_160
    );
    assert_eq!(compare_preflight(&groups, &options).unwrap(), 160);
    let dry = compare_dry_plan(&groups, &options).unwrap();
    for text in [
        "完整固定槽位: 160",
        "待录制: 160",
        "发送前拒绝: 0",
        "3932160",
        "3933760",
        "当前 AF155",
        "本次真实调用: 0",
    ] {
        assert!(dry.contains(text), "{dry}");
    }
    assert!(!temp.0.exists());
    let http = reqwest::blocking::Client::builder()
        .no_proxy()
        .build()
        .unwrap();
    for group in &groups {
        let keys = smoke_credentials(group, af156_fixture_variable).unwrap();
        assert!(
            smoke_credentials(group, |name| (name == "QWEN_BASE_URL")
                .then(|| "https://dashscope.aliyuncs.com/compatible-mode".into()))
            .is_err()
        );
        for case in group {
            let body = &case.wire_request;
            assert_eq!(body["model"], model_provider::DASHSCOPE_MODEL);
            if case.provider.as_ref().unwrap().thinking == Some(true) {
                assert_eq!(
                    body["thinking_budget"],
                    case.request["max_output_tokens"].as_u64().unwrap() / 2
                );
                let mut old = case.clone();
                old.wire_request
                    .as_object_mut()
                    .unwrap()
                    .remove("thinking_budget");
                assert_ne!(case_fingerprint(&old), case_fingerprint(case));
            } else {
                assert!(body.get("thinking_budget").is_none());
            }
            assert_eq!(
                body["max_completion_tokens"],
                case.request["max_output_tokens"]
            );
            assert_eq!(
                body["reasoning_effort"],
                if case.provider.as_ref().unwrap().thinking == Some(true) {
                    "medium"
                } else {
                    "none"
                }
            );
            assert!(body.get("max_tokens").is_none());
            assert_eq!(case.timeout_seconds, Some(360));
            let input: Value =
                serde_json::from_str(case.request["input"].as_str().unwrap()).unwrap();
            assert_eq!(
                input["source_snapshot"]["eval_sample"],
                compare_sample_id(case.sample["id"].as_str().unwrap())
            );
            assert!(
                !body
                    .to_string()
                    .contains(case.sample["id"].as_str().unwrap())
            );
            let request = smoke_http_request(&http, case, &keys[&case.model.label]).unwrap();
            assert_eq!(
                request.headers()[reqwest::header::AUTHORIZATION],
                "Bearer fixture-cloud-key"
            );
            assert_eq!(
                request.headers()[reqwest::header::USER_AGENT],
                model_provider::MODEL_USER_AGENT
            );
            assert_eq!(
                request.url().as_str(),
                "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions"
            );
        }
    }
    for bad in ["159", "161", "520"] {
        assert!(
            compare_options(|name| match name {
                "EVAL_COMPARE" => Some("af156".into()),
                "EVAL_RECORD" => Some(bad.into()),
                _ => None,
            })
            .is_err()
        );
    }
    assert!(
        compare_options(|name| match name {
            "EVAL_COMPARE" => Some("af156".into()),
            "EVAL_COMPARE_PROFILES" => Some("qwen-27b-think".into()),
            _ => None,
        })
        .is_err()
    );
    assert!(!temp.0.exists());
}
#[test]
fn af156_durable_slots_skip_inflight_keep_80_per_profile_and_reject_changed_requests() {
    let temp = TempRecords::new();
    let profiles = evaluation_profiles(include_str!("eval/write_stage/providers.json")).unwrap();
    let groups = compare_plan_for(&temp.0, &profiles, &AF156_IDS, af156_fixture_variable).unwrap();
    for group in &groups {
        compare_freeze(group).unwrap();
    }
    campaign_start(&groups[0][0]).unwrap();
    let resumed = compare_plan_for(&temp.0, &profiles, &AF156_IDS, af156_fixture_variable).unwrap();
    assert!(matches!(resumed[0][0].replay, Replay::Interrupted));
    assert_eq!(
        compare_preflight(&resumed, &af156_fixture_options()).unwrap(),
        159
    );
    let mut sends = 0;
    for group in &resumed {
        let report = evaluate_cases(&group[..2], Some(80), &mut |case| {
            sends += 1;
            assert_ne!(campaign_slot(case), campaign_slot(&groups[0][0]));
            Ok(compare_fixture_response(case))
        })
        .unwrap();
        assert_eq!(
            report["sent_calls"],
            if group[0].model.label == AF156_IDS[0] {
                1
            } else {
                2
            }
        );
    }
    assert_eq!(sends, 3);
    for id in AF156_IDS {
        assert_eq!(
            count_sends([temp.0.join("campaigns").join(id).join("sends")]).unwrap(),
            2
        );
        let a: Value = serde_json::from_slice(
            &std::fs::read(temp.0.join("campaigns").join(id).join("authorization.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(a["limit"], 80);
        assert_eq!(items(&a["requests"]).len(), 80);
    }
    let ready = compare_plan_for(&temp.0, &profiles, &AF156_IDS, af156_fixture_variable).unwrap();
    for group in &ready {
        assert_eq!(
            evaluate_cases(&group[..2], Some(0), &mut |_| panic!(
                "strict replay resent"
            ))
            .unwrap()["sent_calls"],
            0
        );
    }
    let reports: Vec<_> = ready
        .iter()
        .map(|g| evaluate_cases(g, None, &mut |_| panic!("offline report sent")).unwrap())
        .collect();
    let report = compare_report(&ready, &reports, 0);
    assert_eq!(report["fixed_slots"], 160);
    assert_eq!(report["interrupted"][0]["profile"], AF156_IDS[0]);
    let mut changed = profiles.clone();
    changed[8].max_output_tokens.insert("w1".into(), 32767);
    let stale = compare_plan_for(&temp.0, &changed, &AF156_IDS, af156_fixture_variable).unwrap();
    assert!(matches!(stale[0][1].replay, Replay::Stale));
    assert!(compare_preflight(&stale, &af156_fixture_options()).is_err());
    assert_eq!(
        count_sends([temp.0.join("campaigns").join(AF156_IDS[0]).join("sends")]).unwrap(),
        2
    );
}
#[test]
fn af156_local_routing_is_separate_and_historic_replay_still_checks_full_fingerprints() {
    let temp = TempRecords::new();
    let profiles = smoke_fixture_profiles();
    let local = &profiles[4..5];
    let cases = smoke_plan(&temp.0, local, smoke_fixture_variable).unwrap();
    evaluate_cases(&cases, Some(1), &mut |case| {
        Ok(smoke_fixture_response(case))
    })
    .unwrap();
    let variable = |name: &str| {
        assert_eq!(
            name, "QWEN_LOCAL_BASE_URL",
            "must not read cloud QWEN_BASE_URL"
        );
        None
    };
    let old = smoke_plan(&temp.0, local, variable).unwrap();
    assert_eq!(
        old[0].model.base.as_deref(),
        Some("http://127.0.0.1:11434/v1")
    );
    assert_eq!(case_fingerprint(&old[0]), case_fingerprint(&cases[0]));
    assert!(matches!(old[0].replay, Replay::Output(_)));
    assert_eq!(
        evaluate_cases(&old, Some(0), &mut |_| panic!("historic replay sent")).unwrap()["sent_calls"],
        0
    );
    let empty = TempRecords::new();
    let missing = smoke_plan(&empty.0, local, variable).unwrap();
    assert!(
        smoke_credentials(&missing, |_| None)
            .unwrap_err()
            .contains("QWEN_LOCAL_BASE_URL")
    );
    let mut changed = local.to_vec();
    changed[0].max_output_tokens.insert("w1".into(), 32767);
    let stale = smoke_plan(&temp.0, &changed, variable).unwrap();
    assert!(matches!(stale[0].replay, Replay::Stale));
    let denied = provider_comparison(|name| match name {
        "EVAL_COMPARE" => Some("af137".into()),
        "EVAL_COMPARE_PROFILES" => Some("qwen-27b-think".into()),
        "EVAL_RECORD" => Some("40".into()),
        _ => None,
    })
    .unwrap_err();
    assert!(denied.contains("read-only"));
}
#[test]
fn af156_nine_profile_report_preserves_observation_and_strict_reference_scores() {
    let temp = TempRecords::new();
    let profiles = evaluation_profiles(include_str!("eval/write_stage/providers.json")).unwrap();
    let mut history =
        compare_plan(&temp.0.join("history"), &profiles, af156_fixture_variable).unwrap();
    let mut current = compare_plan_for(
        &temp.0.join("current"),
        &profiles,
        &AF156_IDS,
        af156_fixture_variable,
    )
    .unwrap();
    for case in history.iter_mut().chain(&mut current).flatten() {
        let output = if case.stage == "w3" {
            fake_w3(&case.sample)
        } else {
            json!({"candidates":[],"memory_requests":[]})
        };
        case.replay = Replay::Output(output.to_string());
    }
    let reports = |groups: &[Vec<Case>]| {
        groups
            .iter()
            .map(|g| evaluate_cases(g, None, &mut |_| panic!("fixture replay sent")).unwrap())
            .collect::<Vec<_>>()
    };
    let mut historic_reports = reports(&history);
    let baseline = compare_report(&history, &historic_reports, 0);
    historic_reports.extend(reports(&current));
    history.extend(current);
    let merged = compare_report(&history, &historic_reports, 0);
    assert_eq!(merged["comparison"], "af156");
    assert_eq!(merged["fixed_slots"], 680);
    assert_eq!(items(&merged["profiles"]).len(), 9);
    assert_eq!(merged["scoring_samples"], 39);
    assert_eq!(merged["observation_samples"], 1);
    af156_reference_check(&merged, &baseline).unwrap();
    let mut stale = merged.clone();
    stale["results"][0]["request"] = json!("changed");
    assert!(af156_reference_check(&stale, &baseline).is_err());
    let observation = items(&merged["matrix"])
        .iter()
        .find(|r| r["id"] == "w3_19_unresolved_condition_honest")
        .unwrap();
    assert_eq!(observation["tier"], "观察（契约待定）");
    assert!(
        items(&observation["profiles"])
            .iter()
            .all(|p| p["passed"] == 0)
    );
    for profile in &items(&merged["profiles"])[7..] {
        assert_eq!(items(&profile["observations"]).len(), 2);
        assert!(profile.get("w3_verdict").is_some() && profile.get("w3_coverage").is_some());
        assert_eq!(profile["summary"][1]["passed"], 38);
    }
    assert_eq!(
        compare_tier(&[
            json!({"profile":AF156_IDS[0],"total":2,"passed":2,"complete":true,"unstable":false})
        ]),
        "阿里云 Qwen 即可（本轮两次均通过）"
    );
    assert!(!temp.0.exists());
}
fn compare_fixture_options(
    profiles: Option<&str>,
    cases: Option<&str>,
    limit: Option<&str>,
) -> CompareOptions {
    compare_options(|key| match key {
        "EVAL_COMPARE" => Some("af137".into()),
        "EVAL_COMPARE_PROFILES" => profiles.map(str::to_owned),
        "EVAL_COMPARE_CASES" => cases.map(str::to_owned),
        "EVAL_RECORD" => limit.map(str::to_owned),
        _ => None,
    })
    .unwrap()
}
fn compare_fixture_response(c: &Case) -> Value {
    let output = if c.stage == "w3" {
        fake_w3(&c.sample)
    } else {
        json!({"candidates":[],"memory_requests":[]})
    };
    let text = output.to_string();
    match c.provider.as_ref().unwrap().protocol {
        ProviderProtocol::OpenaiResponses => fake_envelope(output),
        ProviderProtocol::AnthropicMessages => {
            json!({"stop_reason":"end_turn","usage":{"input_tokens":12,"output_tokens":7},"content":[{"type":"text","text":text}]})
        }
        ProviderProtocol::OpenaiChat => {
            json!({"usage":{"prompt_tokens":12,"completion_tokens":7},"choices":[{"finish_reason":"stop","message":{"content":text}}]})
        }
    }
}
#[test]
fn af137_plan_has_520_fixed_slots_and_key_free_dry_run() {
    let temp = TempRecords::new();
    let profiles = smoke_fixture_profiles();
    let groups = compare_plan(&temp.0, &profiles, |_| None).unwrap();
    let options = compare_fixture_options(None, None, None);
    assert_eq!(groups.iter().map(Vec::len).sum::<usize>(), 520);
    assert_eq!(
        groups
            .iter()
            .map(|g| g[0].model.label.as_str())
            .collect::<Vec<_>>(),
        COMPARE_IDS
    );
    let slots: std::collections::BTreeSet<_> = groups
        .iter()
        .flatten()
        .map(|c| campaign_slot(c).to_string())
        .collect();
    assert_eq!(slots.len(), 520);
    let dry = compare_dry_plan(&groups, &options).unwrap();
    for text in [
        "12779520",
        "7864320",
        "4915200",
        "待录制: 520",
        "发送前拒绝: 0",
        "本次真实调用: 0",
    ] {
        assert!(dry.contains(text), "{text}\n{dry}");
    }
    assert!(!temp.0.exists());
    assert_eq!(compare_preflight(&groups, &options).unwrap(), 0);
    for bad in ["519", "521", "-1"] {
        assert!(
            compare_options(|key| match key {
                "EVAL_COMPARE" => Some("af137".into()),
                "EVAL_RECORD" => Some(bad.into()),
                _ => None,
            })
            .is_err()
        );
    }
    for bad in ["", "qwen-9b-think", "ds-pro,ds-pro", "unknown"] {
        assert!(compare_selection(Some(bad), &COMPARE_IDS).is_err());
    }
    let cloud = compare_fixture_options(
        Some("opus-5.5,ds-pro,gpt-6.1-sol,ds-flash"),
        None,
        Some("320"),
    );
    assert_eq!(cloud.profiles, &COMPARE_IDS[..4]);
    let local = compare_fixture_options(
        Some("qwen-27b-think,qwen-27b-nothink,qwen-9b-nothink"),
        None,
        Some("200"),
    );
    assert_eq!(local.limit, Some(200));
    assert!(!temp.0.exists());
}
#[test]
fn af137_routes_both_stages_and_freezes_full_manifests_before_subset_resume() {
    let temp = TempRecords::new();
    let profiles = smoke_fixture_profiles();
    let groups = compare_plan(&temp.0, &profiles, smoke_fixture_variable).unwrap();
    let ids = format!(
        "{},{}",
        samples("w1")[0]["id"].as_str().unwrap(),
        samples("w3")[0]["id"].as_str().unwrap()
    );
    let options = compare_fixture_options(None, Some(&ids), Some("520"));
    assert_eq!(compare_preflight(&groups, &options).unwrap(), 26);
    let http = reqwest::blocking::Client::builder()
        .no_proxy()
        .build()
        .unwrap();
    let mut sent = 0;
    for group in &groups {
        compare_freeze(group).unwrap();
    }
    for group in &groups {
        let id = &group[0].model.label;
        let full_auth: Value = serde_json::from_slice(
            &std::fs::read(
                group[0]
                    .campaign
                    .as_ref()
                    .unwrap()
                    .join("authorization.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(full_auth["limit"], compare_quota(id));
        assert_eq!(items(&full_auth["requests"]).len(), compare_quota(id));
        let subset = compare_subset(group, &options);
        let keys = smoke_credentials(&subset, smoke_fixture_variable).unwrap();
        let report = evaluate_cases(&subset, Some(compare_quota(id)), &mut |c| {
            let request = smoke_http_request(&http, c, &keys[&c.model.label]).unwrap();
            assert_eq!(
                request.timeout().copied(),
                Some(std::time::Duration::from_secs(
                    if c.model.label == "qwen-27b-think" {
                        1800
                    } else {
                        360
                    }
                ))
            );
            let body: Value =
                serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap();
            let tokens = if c.stage == "w1" { 32768 } else { 16384 };
            let p = c.provider.as_ref().unwrap();
            assert_eq!(
                request.url().as_str(),
                p.endpoint(c.model.base.as_deref().unwrap()).unwrap()
            );
            assert_eq!(body["model"], c.model.model.as_deref().unwrap());
            assert_eq!(
                body[if p.protocol == ProviderProtocol::OpenaiResponses {
                    "max_output_tokens"
                } else {
                    "max_tokens"
                }],
                tokens
            );
            if p.protocol == ProviderProtocol::AnthropicMessages {
                assert_eq!(request.headers()["x-api-key"], "fixture-opus-key");
                assert!(
                    body.get("output_config")
                        .is_none_or(|v| v.get("format").is_none())
                );
                assert!(
                    body["system"]
                        .as_str()
                        .unwrap()
                        .contains("prompt-schema.v1")
                );
            } else if p.protocol == ProviderProtocol::OpenaiChat {
                assert!(request.headers().get("authorization").is_none());
            } else {
                assert_eq!(
                    request.headers()["authorization"],
                    if c.model.label.starts_with("ds-") {
                        "Bearer fixture-ds-key"
                    } else {
                        "Bearer fixture-gpt-key"
                    }
                );
            }
            assert_eq!(
                eval_provider_binding(
                    &c.model,
                    &c.wire_request,
                    c.provider.as_ref(),
                    c.timeout_seconds
                )["timeout_seconds"],
                c.timeout_seconds.unwrap()
            );
            sent += 1;
            // Failure recordings consume their slots and are replayed, never retried.
            if c.model.label == "ds-flash" && c.stage == "w1" {
                Err("fixture timed out".into())
            } else {
                Ok(compare_fixture_response(c))
            }
        })
        .unwrap();
        assert_eq!(report["sent_calls"], subset.len());
        assert_eq!(
            count_sends([group[0].campaign.as_ref().unwrap().join("sends")]).unwrap(),
            subset.len()
        );
    }
    assert_eq!(sent, 26);
    let resumed = compare_plan(&temp.0, &profiles, smoke_fixture_variable).unwrap();
    assert_eq!(compare_preflight(&resumed, &options).unwrap(), 0);
    for group in &resumed {
        let subset = compare_subset(group, &options);
        assert!(
            smoke_credentials(&subset, |_| panic!("cached subset read a key"))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            evaluate_cases(&subset, Some(0), &mut |_| panic!("cached subset sent")).unwrap()["sent_calls"],
            0
        );
    }
    let full = compare_fixture_options(None, None, Some("520"));
    assert_eq!(compare_preflight(&resumed, &full).unwrap(), 494);
    let reports: Vec<_> = resumed
        .iter()
        .map(|g| evaluate_cases(g, None, &mut |_| panic!("report sent")).unwrap())
        .collect();
    let report = compare_report(&resumed, &reports, sent);
    assert_eq!(items(&report["matrix"]).len(), 40);
    assert_eq!(items(&report["results"]).len(), 520);
    assert_eq!(report["matrix"][0]["label_contract"], "w1.v1");
    assert_eq!(report["matrix"][0]["request_contract"], "w1.v4");
    assert_eq!(
        report["matrix"][0]["profiles"][3]["constrained_decoding"],
        false
    );
    assert_eq!(
        report["profiles"][3]["metrics"][0]["usage"]["reasoning_tokens"]["known_total"],
        Value::Null
    );
    assert_eq!(report["matrix"][1]["tier"], "证据不足，暂不分层");
    assert_eq!(
        report["matrix"][0]["profiles"][1]["attempts"][0]["mechanical_failures"],
        json!(["timeout"])
    );
    let mut changed = profiles.clone();
    changed[0].timeout_seconds += 1;
    let stale = compare_plan(&temp.0, &changed, smoke_fixture_variable).unwrap();
    assert!(matches!(stale[0][0].replay, Replay::Stale));
    assert!(
        compare_preflight(&stale, &full)
            .unwrap_err()
            .contains("identity")
    );
    changed = profiles;
    changed[0].max_output_tokens.insert("w3".into(), 16000);
    let stale = compare_plan(&temp.0, &changed, smoke_fixture_variable).unwrap();
    assert!(matches!(stale[0][40].replay, Replay::Stale));
}
#[test]
fn af137_crash_skips_inflight_resumes_other_slots_without_refunding_quota() {
    let temp = TempRecords::new();
    let profiles = smoke_fixture_profiles();
    let groups = compare_plan(&temp.0, &profiles, smoke_fixture_variable).unwrap();
    compare_freeze(&groups[0]).unwrap();
    let subset = groups[0][..2].to_vec();
    let mut sent = 0;
    let crash = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        evaluate_cases(&subset, Some(80), &mut |c| {
            sent += 1;
            if sent == 2 {
                panic!("fixture crash after started")
            }
            Ok(compare_fixture_response(c))
        })
        .unwrap();
    }));
    assert!(crash.is_err());
    let resumed = compare_plan(&temp.0, &profiles, smoke_fixture_variable).unwrap();
    let options = compare_fixture_options(Some("ds-pro"), None, Some("80"));
    assert!(matches!(resumed[0][0].replay, Replay::Output(_)));
    assert!(matches!(resumed[0][1].replay, Replay::Interrupted));
    assert_eq!(compare_preflight(&resumed, &options).unwrap(), 78);
    // The historical campaign precheck is unchanged; only AF137 plans mark gaps.
    assert!(
        preflight(&subset, Some(80))
            .unwrap_err()
            .contains("already sent")
    );
    assert_eq!(
        count_sends([temp.0.join("campaigns/ds-pro/sends")]).unwrap(),
        2
    );
    let dry = compare_dry_plan(&resumed, &options).unwrap();
    assert!(dry.contains("ds-pro | 80 | 80 | 78 | 1 | 0 | 0 | 1 | 2 |"));
    let report = evaluate_cases(&resumed[0], Some(80), &mut |c| {
        assert_ne!(campaign_slot(c), campaign_slot(&subset[1]));
        sent += 1;
        Ok(compare_fixture_response(c))
    })
    .unwrap();
    assert_eq!(report["sent_calls"], 78);
    assert_eq!(sent, 80);
    assert_eq!(report["results"][1]["status"], "未录制（中断在途）");
    assert_eq!(report["results"][1]["sent"], false);
    assert_eq!(report["results"][1]["usage"], Value::Null);
    assert!(!subset[1].path.exists());
    assert_eq!(
        count_sends([temp.0.join("campaigns/ds-pro/sends")]).unwrap(),
        80
    );
    let final_groups = compare_plan(&temp.0, &profiles, smoke_fixture_variable).unwrap();
    assert_eq!(compare_preflight(&final_groups, &options).unwrap(), 0);
    let reports: Vec<_> = final_groups
        .iter()
        .map(|g| evaluate_cases(g, None, &mut |_| panic!("offline report sent")).unwrap())
        .collect();
    let final_report = compare_report(&final_groups, &reports, 78);
    assert_eq!(items(&final_report["interrupted"]).len(), 1);
    assert_eq!(final_report["interrupted"][0]["profile"], "ds-pro");
    assert_eq!(final_report["interrupted"][0]["id"], subset[1].sample["id"]);
    assert_eq!(final_report["interrupted"][0]["repeat"], 2);
    assert_eq!(final_report["profiles"][0]["interrupted_inflight"], 1);
    let cell = &final_report["matrix"][0]["profiles"][0];
    assert_eq!(cell["complete"], false);
    assert_eq!(cell["observed_attempts"], 1);
    assert_eq!(cell["unstable"], false);
    assert_eq!(
        cell["attempts"][1]["mechanical_failures"],
        json!(["interrupted_inflight"])
    );
    assert_eq!(final_report["matrix"][0]["tier"], "证据不足，暂不分层");
    for limit in [Some(80), Some(0)] {
        assert_eq!(
            evaluate_cases(&final_groups[0], limit, &mut |_| panic!("inflight resent")).unwrap()["sent_calls"],
            0
        );
    }
    let mut changed = profiles;
    changed[0].timeout_seconds += 1;
    let changed = compare_plan(&temp.0, &changed, smoke_fixture_variable).unwrap();
    assert!(matches!(changed[0][1].replay, Replay::Missing));
    assert!(
        compare_preflight(&changed, &options)
            .unwrap_err()
            .contains("identity")
    );
    // Every started/responded log consumes allowance, including duplicate logs.
    campaign_start(&subset[1]).unwrap();
    assert!(
        compare_preflight(&final_groups, &options)
            .unwrap_err()
            .contains("cumulative 81 + missing 0 exceeds 80")
    );
}

#[test]
fn af137_durable_recording_before_responded_log_is_replayed_not_skipped() {
    let temp = TempRecords::new();
    let profiles = smoke_fixture_profiles();
    let groups = compare_plan(&temp.0, &profiles, smoke_fixture_variable).unwrap();
    compare_freeze(&groups[0]).unwrap();
    let case = &groups[0][0];
    let path = campaign_start(case).unwrap();
    let response = compare_fixture_response(case);
    let text = model_provider::provider_output(case.provider.as_ref().unwrap().protocol, &response)
        .unwrap();
    save_recording(
        case,
        &normalized_envelope(case.provider.as_ref(), &response),
        Some(&text),
        None,
    )
    .unwrap();
    let log: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(log["state"], "started");
    let resumed = compare_plan(&temp.0, &profiles, smoke_fixture_variable).unwrap();
    assert!(matches!(resumed[0][0].replay, Replay::Output(_)));
    assert_eq!(
        compare_preflight(
            &resumed,
            &compare_fixture_options(Some("ds-pro"), None, Some("80"))
        )
        .unwrap(),
        79
    );
    let report =
        evaluate_cases(&resumed[0][..1], Some(80), &mut |_| panic!("cached resent")).unwrap();
    assert_eq!(report["sent_calls"], 0);
    assert_ne!(report["results"][0]["status"], "未录制（中断在途）");
    assert_eq!(
        count_sends([temp.0.join("campaigns/ds-pro/sends")]).unwrap(),
        1
    );
}

#[test]
fn af137_provider_locks_do_not_overlap() {
    let temp = TempRecords::new();
    let root = temp.0.join("locks");
    let ds = w3_trial_lock(&root.join(compare_resource("ds-pro"))).unwrap();
    assert!(w3_trial_lock(&root.join(compare_resource("ds-flash"))).is_err());
    let qwen = w3_trial_lock(&root.join(compare_resource("qwen-27b-think"))).unwrap();
    for id in ["qwen-27b-nothink", "qwen-9b-nothink"] {
        assert!(w3_trial_lock(&root.join(compare_resource(id))).is_err());
    }
    assert!(w3_trial_lock(&root.join(compare_resource("opus-5.5"))).is_ok());
    drop(ds);
    drop(qwen);
    assert!(w3_trial_lock(&root.join(compare_resource("ds-flash"))).is_ok());
}
#[test]
fn af137_tiers_distinguish_single_pass_instability_and_missing_data() {
    let mut cells: Vec<_> = COMPARE_IDS.iter().map(|id|json!({"profile":id,"total":compare_quota(id)/40,"passed":0,"complete":true,"unstable":false})).collect();
    assert_eq!(compare_tier(&cells), "全部失败（不自动归因）");
    cells[2]["passed"] = json!(2);
    assert_eq!(compare_tier(&cells), "本轮仅 GPT 或 Opus 通过");
    cells[0]["complete"] = json!(false);
    assert_eq!(compare_tier(&cells), "证据不足，暂不分层");
    cells[0]["complete"] = json!(true);
    cells[0]["passed"] = json!(2);
    assert_eq!(compare_tier(&cells), "需 DeepSeek（本轮）");
    cells[4]["passed"] = json!(1);
    assert_eq!(compare_tier(&cells), "本地单次通过，稳定性未验证");
    cells[4]["passed"] = json!(0);
    cells[6]["passed"] = json!(1);
    cells[6]["unstable"] = json!(true);
    assert_eq!(compare_tier(&cells), "重复不一致，暂不分层");
    cells[5]["passed"] = json!(2);
    assert_eq!(compare_tier(&cells), "本地即可（本轮两次均通过）");
}

#[test]
fn af137_report_marks_timeout_inconsistency_and_gates_only_selected_attempts() {
    let temp = TempRecords::new();
    let groups = compare_plan(&temp.0, &smoke_fixture_profiles(), |_| None).unwrap();
    let mut reports: Vec<_> = groups
        .iter()
        .map(|g| evaluate_cases(g, None, &mut |_| panic!("report sent")).unwrap())
        .collect();
    reports[0]["results"][0]["status"] = json!("通过");
    reports[0]["results"][1]["status"] = json!("调用失败");
    reports[0]["results"][1]["error"] = json!("operation timed out");
    let report = compare_report(&groups, &reports, 0);
    assert_eq!(report["matrix"][0]["profiles"][0]["unstable"], true);
    assert_eq!(report["matrix"][0]["profiles"][0]["observed_attempts"], 2);
    assert_eq!(report["matrix"][0]["profiles"][0]["complete"], false);
    assert_eq!(report["matrix"][0]["tier"], "证据不足，暂不分层");
    assert_eq!(report["unstable_samples"][0], groups[0][0].sample["id"]);
    let mut options = compare_fixture_options(
        Some("ds-pro"),
        Some(groups[0][0].sample["id"].as_str().unwrap()),
        None,
    );
    assert!(compare_check_report(&report, &options).is_ok());
    options.gate = true;
    assert!(compare_check_report(&report, &options).is_err());
    let mut report = report;
    report["results"][1]["status"] = json!("通过");
    // The other 518 missing/unselected attempts are not an explicit gate failure.
    assert!(compare_check_report(&report, &options).is_ok());
    assert!(!temp.0.exists());
}

#[test]
fn af137_wire_inputs_are_blind_and_reports_keep_the_reverse_lookup() {
    let temp = TempRecords::new();
    let samples = compare_samples().unwrap();
    let groups = compare_plan(&temp.0, &smoke_fixture_profiles(), |_| None).unwrap();
    let allowed = [
        "contract",
        "source_snapshot",
        "current_user_message",
        "user_history",
        "entities",
        "relations",
        "said_on",
        "utc_offset_seconds",
        "task_id",
        "candidates",
        "memory_requests",
    ];
    let annotations = [
        "label_status",
        "focus",
        "expect",
        "expected",
        "must_not",
        "note",
        "filename",
        "file_name",
        "path",
        "label",
    ];
    fn annotation_free(value: &Value, forbidden: &[&str]) {
        match value {
            Value::Object(fields) => {
                for (key, value) in fields {
                    assert!(!forbidden.contains(&key.as_str()), "annotation key {key}");
                    annotation_free(value, forbidden);
                }
            }
            Value::Array(values) => values.iter().for_each(|v| annotation_free(v, forbidden)),
            _ => {}
        }
    }
    let mut opaque = std::collections::BTreeSet::new();
    for c in groups.iter().flatten() {
        let input: Value = serde_json::from_str(c.request["input"].as_str().unwrap()).unwrap();
        assert!(
            input
                .as_object()
                .unwrap()
                .keys()
                .all(|k| allowed.contains(&k.as_str()))
        );
        annotation_free(&input, &annotations);
        let token = input["source_snapshot"]["eval_sample"].as_str().unwrap();
        assert_eq!(token, compare_sample_id(c.sample["id"].as_str().unwrap()));
        assert_eq!(token.len(), 18);
        assert!(token.starts_with("s-") && token[2..].bytes().all(|b| b.is_ascii_hexdigit()));
        opaque.insert(token.to_owned());
        // All three physical protocols, including the Opus schema suffix, must be blind.
        let wire = c.wire_request.to_string();
        for (_, sample) in &samples {
            assert!(!wire.contains(sample["id"].as_str().unwrap()));
            assert!(!wire.contains(sample["focus"].as_str().unwrap()));
        }
        for word in [
            "reviewed",
            "draft",
            "label_status",
            "must_not",
            "tests/eval/",
        ] {
            assert!(!wire.contains(word), "{word} in {}", c.model.label);
        }
    }
    assert_eq!(opaque.len(), 40);
    for (stage, sample) in &samples {
        let mut poisoned = sample.clone();
        for key in annotations {
            poisoned[key] = json!("AF137_ANNOTATION_CANARY");
            poisoned["input"][key] = json!("AF137_ANNOTATION_CANARY");
        }
        let request = compare_request(stage, &poisoned, 16384).unwrap();
        assert!(!request.to_string().contains("AF137_ANNOTATION_CANARY"));
    }
    let reports: Vec<_> = groups
        .iter()
        .map(|g| evaluate_cases(g, None, &mut |_| panic!("blind report sent")).unwrap())
        .collect();
    let report = compare_report(&groups, &reports, 0);
    for row in items(&report["matrix"]) {
        assert_eq!(
            row["eval_sample"],
            compare_sample_id(row["id"].as_str().unwrap())
        );
        assert_eq!(
            items(&report["matrix"])
                .iter()
                .filter(|r| r["eval_sample"] == row["eval_sample"])
                .count(),
            1
        );
    }
    assert!(!temp.0.exists());
}
#[test]
fn af155_w1_memory_type_alternatives_keep_subject_time_and_request_checks() {
    let sample = samples("w1")[11].clone();
    let mut output = fake_w1(&sample);
    for memory_type in ["fact", "episode"] {
        output["candidates"][0]["memory_type"] = json!(memory_type);
        assert_eq!(
            score("w1", &sample, Some(&output.to_string()))["status"],
            "通过"
        );
    }
    for subject in [
        json!({"entity_id":"unresolved"}),
        json!({"entity_id":"self"}),
    ] {
        let mut invalid = output.clone();
        invalid["candidates"][0]["subject"] = subject.clone();
        let report = score("w1", &sample, Some(&invalid.to_string()));
        assert_eq!(report["status"], "失败");
        if subject["entity_id"] == "unresolved" {
            assert!(
                items(&report["fields"])
                    .iter()
                    .any(|f| f["field"] == "w2.candidates[0]" && f["passed"] == false)
            );
        }
    }
    let mut invalid = output.clone();
    invalid["candidates"][0]["times"][0]["interp"] = json!({"kind":"date","iso":"2026-09-13"});
    assert_eq!(
        score("w1", &sample, Some(&invalid.to_string()))["status"],
        "失败"
    );
    invalid = output;
    invalid["memory_requests"] = json!([{"kind":"remember","quote":{"text":sample["input"]["text"]},
        "target":{"object":"facts","candidate_indices":[0],"quote":{"text":sample["input"]["text"]}},"deadline":null}]);
    assert_eq!(
        score("w1", &sample, Some(&invalid.to_string()))["status"],
        "失败"
    );
}

#[test]
fn af155_w3_complete_and_gaps_are_whole_branches_with_bank_basis() {
    let sample = samples("w3")[11].clone();
    let mut output = fake_w3(&sample);
    assert_eq!(
        score("w3", &sample, Some(&output.to_string()))["status"],
        "通过"
    );
    output["coverage"] = json!({"status":"gaps","missing":[{"text":"在一家银行做产品经理"}]});
    assert_eq!(
        score("w3", &sample, Some(&output.to_string()))["status"],
        "通过"
    );
    for missing in [
        json!([]),
        json!([{"text":"上海"}]),
        json!([{"text":"在一家银行工作"}]),
    ] {
        let mut invalid = output.clone();
        invalid["coverage"]["missing"] = missing;
        assert_eq!(
            score("w3", &sample, Some(&invalid.to_string()))["status"],
            "失败"
        );
    }
    output["reviews"][1]["verdict"] = json!("contradicted");
    output["reviews"][1]["problems"] =
        json!([{"aspect":"condition","quote":{"text":"在一家银行"}}]);
    assert_eq!(
        score("w3", &sample, Some(&output.to_string()))["status"],
        "失败"
    );
    let mut crossing = sample.clone();
    crossing["expect"]["alternatives"][0]["reviews"][1] = json!({"candidate":1,"verdict":"contradicted","aspects":["condition"],"episode_value":"not_applicable"});
    // Verdict matches the complete branch, coverage matches the supported/gaps branch: neither whole branch passes.
    assert_eq!(
        score("w3", &crossing, Some(&output.to_string()))["status"],
        "失败"
    );
}

#[test]
fn af155_w3_temporary_verdict_set_still_requires_value_and_validation() {
    let sample = samples("w3")[9].clone();
    let mut output = fake_w3(&sample);
    for verdict in ["contradicted", "insufficient"] {
        output["reviews"][0]["verdict"] = json!(verdict);
        assert_eq!(
            score("w3", &sample, Some(&output.to_string()))["status"],
            "通过"
        );
    }
    output["reviews"][0]["verdict"] = json!("supported");
    output["reviews"][0]["problems"] = json!([]);
    assert_eq!(
        score("w3", &sample, Some(&output.to_string()))["status"],
        "失败"
    );
    output = fake_w3(&sample);
    output["reviews"][0]["problems"][0]["aspect"] = json!("subject");
    assert_eq!(
        score("w3", &sample, Some(&output.to_string()))["status"],
        "失败"
    );
    output = fake_w3(&sample);
    output["reviews"][0]["problems"][0]["quote"]["text"] = json!("not in source");
    let report = score("w3", &sample, Some(&output.to_string()));
    assert_eq!(report["status"], "失败");
    assert_eq!(report["verdict_result"]["passed"], Value::Null);
}

#[test]
fn af155_observation_never_counts_as_pass_or_hides_mechanical_failure() {
    let sample = samples("w3")[18].clone();
    let output = fake_w3(&sample);
    let passing_label = score("w3", &sample, Some(&output.to_string()));
    assert_eq!(passing_label["status"], "观察");
    assert_eq!(passing_label["observed_status"], "通过");
    let mut different = output.clone();
    different["reviews"][0]["verdict"] = json!("contradicted");
    different["reviews"][0]["problems"] = json!([{"aspect":"strength","quote":{"text":"一般"}}]);
    let observed = score("w3", &sample, Some(&different.to_string()));
    assert_eq!(observed["status"], "观察");
    assert_eq!(observed["observed_status"], "失败");
    assert_eq!(observed["raw_output"], different);
    assert!(
        items(&observed["fields"])
            .iter()
            .any(|f| f["passed"] == false)
    );
    let mut summary = stage_summaries(&[passing_label, observed], &[]);
    let w3 = summary.iter().find(|s| s["stage"] == "w3").unwrap();
    for key in ["passed", "failed", "evaluated", "scoring_slots"] {
        assert_eq!(w3[key], 0);
    }
    assert_eq!(w3["observed"], 2);
    assert!(check_report(&json!({"summary":summary}), true).is_ok());
    for invalid in [
        "{".to_owned(),
        {
            let mut empty = output.clone();
            empty["coverage"] = json!({"status":"gaps","missing":[]});
            empty.to_string()
        },
        {
            different["reviews"][0]["problems"][0]["quote"]["text"] = json!("not in source");
            different.to_string()
        },
    ] {
        let bad = score("w3", &sample, Some(&invalid));
        assert_eq!(bad["status"], "失败", "{bad}");
        summary = stage_summaries(&[bad.clone()], &[]);
        assert_eq!(
            summary.iter().find(|s| s["stage"] == "w3").unwrap()["observation_failed"],
            1
        );
        assert!(check_report(&json!({"summary":summary}), true).is_err());
        let options = CompareOptions {
            qwen38: false,
            af163: false,
            smoke: false,
            af156: false,
            diagnostic: false,
            dry: false,
            profiles: vec!["ds-pro".into()],
            cases: vec![sample["id"].as_str().unwrap().into()],
            limit: None,
            gate: true,
        };
        let mut bad = bad;
        bad["model"] = json!("ds-pro");
        assert!(compare_check_report(&json!({"results":[bad]}), &options).is_err());
    }
    let mut invalid_input = score("w3", &sample, None);
    invalid_input["status"] = json!("输入无效");
    assert!(
        check_report(
            &json!({"summary":stage_summaries(&[invalid_input], &[])}),
            false
        )
        .is_err()
    );
    for status in ["未录制", "录制过期"] {
        let mut row = score("w3", &sample, None);
        row["status"] = json!(status);
        let summary = stage_summaries(&[row], &[]);
        assert_eq!(
            summary.iter().find(|s| s["stage"] == "w3").unwrap()[if status == "未录制" {
                "missing"
            } else {
                "stale"
            }],
            1
        );
    }
}

#[test]
fn af155_annotations_preserve_all_520_request_and_campaign_identities() {
    let temp = TempRecords::new();
    let groups = compare_plan(&temp.0, &smoke_fixture_profiles(), |_| None).unwrap();
    assert_eq!(compare_samples().unwrap().len(), 40);
    assert_eq!(groups.iter().map(Vec::len).sum::<usize>(), 520);
    for group in &groups {
        assert_eq!(group.len(), compare_quota(&group[0].model.label));
        for case in group {
            let mut changed = case.clone();
            changed.sample["expect"] = json!({"alternatives":[{},{}]});
            changed.sample["label_revision"] = json!("different revision");
            changed.sample["observation_only"] = json!(case.sample["observation_only"] != true);
            changed.sample["observation_reason"] = json!("annotation-only");
            assert_eq!(
                compare_request(
                    &case.stage,
                    &case.sample,
                    case.request["max_output_tokens"].as_u64().unwrap() as u32
                )
                .unwrap(),
                compare_request(
                    &changed.stage,
                    &changed.sample,
                    case.request["max_output_tokens"].as_u64().unwrap() as u32
                )
                .unwrap()
            );
            assert_eq!(case_fingerprint(case), case_fingerprint(&changed));
            assert_eq!(campaign_slot(case), campaign_slot(&changed));
        }
    }
    assert!(!temp.0.exists());
}

#[test]
fn af151_full_branches_do_not_mix_fields_or_hide_w3_validation() {
    let mut sample = samples("w3")[3].clone();
    let mut a = sample["expect"].clone();
    a["reviews"][0]["verdict"] = json!("contradicted");
    a["reviews"][0]["aspects"] = json!(["polarity"]);
    let b = sample["expect"].clone();
    let mut output = fake_w3(&sample);
    sample["expect"] = json!({"alternatives":[a,b]});
    assert_eq!(
        score("w3", &sample, Some(&output.to_string()))["status"],
        "通过"
    );
    output["reviews"][0]["problems"][0]["aspect"] = json!("polarity");
    assert_eq!(
        score("w3", &sample, Some(&output.to_string()))["status"],
        "失败"
    );
    output["reviews"][0]["problems"][0]["aspect"] = json!("relation");
    output["reviews"][0]["problems"][0]["quote"]["text"] = json!("not in source");
    let report = score("w3", &sample, Some(&output.to_string()));
    assert_eq!(report["status"], "失败");
    assert!(
        items(&report["fields"])
            .iter()
            .any(|f| f["field"] == "w3.validate" && f["passed"] == false)
    );
    output = fake_w3(&samples("w3")[3]);
    sample["expect"]["alternatives"] = json!([sample["expect"]["alternatives"][1]]);
    assert_eq!(
        score("w3", &sample, Some(&output.to_string()))["status"],
        "通过"
    );
}

#[test]
fn af151_optional_candidate_is_exact_and_cannot_hide_w2_rejection() {
    let sample = samples("w1")[13].clone();
    let mut output = serde_json::to_value(
        adapter::expand(&json!({
            "candidates":sample["expect"]["candidates"],"requests":[]
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        score("w1", &sample, Some(&output.to_string()))["status"],
        "通过"
    );
    let mut invalid = output.clone();
    invalid["candidates"][0]["value"]["quote"]["text"] = json!("not in source");
    let report = score("w1", &sample, Some(&invalid.to_string()));
    assert_eq!(report["status"], "失败");
    assert!(
        items(&report["fields"])
            .iter()
            .any(|f| f["field"] == "w2.candidates[0]" && f["passed"] == false)
    );
    let optional = adapter::candidate(&sample["expect"]["optional_candidates"][0]);
    output["candidates"]
        .as_array_mut()
        .unwrap()
        .push(optional.clone());
    assert_eq!(
        score("w1", &sample, Some(&output.to_string()))["status"],
        "通过"
    );
    output["candidates"][2]["value"]["text"] = json!("上海");
    output["candidates"][2]["value"]["quote"]["text"] = json!("上海");
    assert_eq!(
        score("w1", &sample, Some(&output.to_string()))["status"],
        "失败"
    );
    output["candidates"][2] = optional.clone();
    output["candidates"].as_array_mut().unwrap().push(optional);
    assert_eq!(
        score("w1", &sample, Some(&output.to_string()))["status"],
        "失败"
    );
}

#[test]
fn af151_unscored_coverage_still_requires_exact_nonempty_mother_basis() {
    let sample = samples("w3")[4].clone();
    let mut output = fake_w3(&sample);
    let report = score("w3", &sample, Some(&output.to_string()));
    assert_eq!(report["status"], "通过");
    assert_eq!(report["coverage_result"]["passed"], Value::Null);
    output["coverage"]["status"] = json!("gaps");
    let report = score("w3", &sample, Some(&output.to_string()));
    assert_eq!(report["status"], "失败");
    assert_eq!(report["verdict_result"]["passed"], true);
    output["coverage"]["missing"] = json!([{"text":"我"}]);
    assert_eq!(
        score("w3", &sample, Some(&output.to_string()))["status"],
        "失败"
    );
    output["coverage"]["missing"] = json!([{"text":"我妈对青霉素过敏"}]);
    assert_eq!(
        score("w3", &sample, Some(&output.to_string()))["status"],
        "通过"
    );
    output["coverage"]["missing"] = json!([{"text":"母亲过敏"}]);
    assert_eq!(
        score("w3", &sample, Some(&output.to_string()))["status"],
        "失败"
    );
    let report = score("w3", &sample, Some("{}"));
    assert_eq!(report["status"], "失败");
    assert_eq!(report["coverage_result"]["guard_passed"], false);
    assert_eq!(report["verdict_result"]["passed"], Value::Null);
}

#[test]
fn af151_time_and_request_verdict_sets_never_accept_supported() {
    let sample = samples("w3")[15].clone();
    let mut output = fake_w3(&sample);
    for verdict in ["contradicted", "insufficient"] {
        output["reviews"][1]["verdict"] = json!(verdict);
        output["request_reviews"][0]["verdict"] = json!(verdict);
        assert_eq!(
            score("w3", &sample, Some(&output.to_string()))["status"],
            "通过"
        );
    }
    output["request_reviews"][0]["verdict"] = json!("supported");
    assert_eq!(
        score("w3", &sample, Some(&output.to_string()))["status"],
        "失败"
    );
    output["request_reviews"][0]["verdict"] = json!("insufficient");
    output["reviews"][1]["verdict"] = json!("supported");
    output["reviews"][1]["problems"] = json!([]);
    assert_eq!(
        score("w3", &sample, Some(&output.to_string()))["status"],
        "失败"
    );
}

#[test]
fn af151_annotation_changes_do_not_change_requests_or_fingerprints() {
    let temp = TempRecords::new();
    let groups = compare_plan(&temp.0, &smoke_fixture_profiles(), |_| None).unwrap();
    for group in &groups {
        for stage in ["w1", "w3"] {
            let c = group.iter().find(|c| c.stage == stage).unwrap();
            let mut changed = c.clone();
            changed.sample["expect"] = json!({"alternatives":[{},{}]});
            changed.sample["label_revision"] =
                json!({"date":"different", "decisions":["annotation-only"]});
            changed.sample["focus"] = json!("annotation-only");
            assert_eq!(
                compare_request(stage, &c.sample, 16384).unwrap(),
                compare_request(stage, &changed.sample, 16384).unwrap()
            );
            assert_eq!(case_fingerprint(c), case_fingerprint(&changed));
        }
    }
    assert!(!temp.0.exists());
}

#[test]
fn af137_blinding_changes_only_comparison_requests_and_keeps_legacy_replay_valid() {
    let temp = TempRecords::new();
    for stage in ["w1", "w3"] {
        let sample = samples(stage).remove(0);
        let ordinary = stage_request_with_output_tokens(stage, &sample, 16384).unwrap();
        let mut blinded = compare_request(stage, &sample, 16384).unwrap();
        let mut input: Value = serde_json::from_str(blinded["input"].as_str().unwrap()).unwrap();
        assert_eq!(
            input["source_snapshot"]["eval_sample"],
            compare_sample_id(sample["id"].as_str().unwrap())
        );
        assert_ne!(fingerprint(&ordinary), fingerprint(&blinded));
        input["source_snapshot"]["eval_sample"] = sample["id"].clone();
        blinded["input"] = json!(input.to_string());
        assert_eq!(ordinary, blinded); // No original message, W2 field, catalogue or other request change.
        let model = EvalModel::default_model();
        let old = case_from_request(
            stage.into(),
            sample.clone(),
            &temp.0,
            &model,
            1,
            ordinary.clone(),
        )
        .unwrap();
        let response = if stage == "w1" {
            fake_w1(&sample)
        } else {
            fake_w3(&sample)
        };
        save_recording(
            &old,
            &fake_envelope(response.clone()),
            Some(&response.to_string()),
            None,
        )
        .unwrap();
        let fresh = stage_request_with_output_tokens(stage, &sample, 16384).unwrap();
        assert_eq!(fresh, ordinary);
        let replay = case_from_request(stage.into(), sample, &temp.0, &model, 1, fresh).unwrap();
        assert!(matches!(replay.replay, Replay::Output(_)));
        assert_eq!(case_fingerprint(&old), case_fingerprint(&replay));
        assert_eq!(
            evaluate_cases(&[replay], Some(0), &mut |_| panic!("legacy replay sent")).unwrap()["sent_calls"],
            0
        );
    }
    // Every historic entry using the generic constructor keeps its original, semantic ID.
    let mut historical =
        smoke_plan(&temp.0.join("smoke"), &smoke_fixture_profiles(), |_| None).unwrap();
    historical.extend(m15_w1_plan(&temp.0.join("m15")).unwrap());
    historical.extend(w3_comparison_plan(&temp.0.join("af96"), None).unwrap());
    historical.extend(w3_effort_plan(&temp.0.join("af101"), None).unwrap());
    for c in historical {
        let input: Value = serde_json::from_str(c.request["input"].as_str().unwrap()).unwrap();
        assert_eq!(input["source_snapshot"]["eval_sample"], c.sample["id"]);
    }
}

fn af163_fixture_options(smoke: bool, record: Option<&str>) -> CompareOptions {
    compare_options(|key| match key {
        "EVAL_COMPARE" => Some(if smoke { "af163-smoke" } else { "af163" }.into()),
        "EVAL_RECORD" => record.map(str::to_owned),
        "EVAL_COMPARE_PROFILES" => (!smoke).then(|| AF163_IDS.join(",")),
        _ => None,
    })
    .unwrap()
}
#[test]
fn af166_prompt_profiles_smoke_four_and_original_240_blinded_requests() {
    let temp = TempRecords::new();
    let profiles = evaluation_profiles(include_str!("eval/write_stage/providers.json")).unwrap();
    let http = reqwest::blocking::Client::builder()
        .no_proxy()
        .build()
        .unwrap();
    for smoke in [true, false] {
        let root = temp.0.join(if smoke { "smoke" } else { "formal" });
        let groups = compare_plan_with_smoke(
            &root,
            &profiles,
            if smoke { &AF166_PROMPT_IDS } else { &AF163_IDS },
            smoke_fixture_variable,
            None,
            smoke,
        )
        .unwrap();
        let options = af163_fixture_options(smoke, Some(if smoke { "4" } else { "240" }));
        assert_eq!(
            compare_preflight(&groups, &options).unwrap(),
            if smoke { 4 } else { 240 }
        );
        assert_eq!(
            groups.iter().map(Vec::len).sum::<usize>(),
            if smoke { 4 } else { 240 }
        );
        let dry = compare_dry_plan(&groups, &options).unwrap();
        assert!(
            dry.contains(if smoke {
                "完整固定槽位: 4"
            } else {
                "完整固定槽位: 400"
            }),
            "{dry}"
        );
        assert!(dry.contains("发送前拒绝: 0"));
        assert!(!root.exists());
        for group in &groups {
            assert_eq!(group.len(), if smoke { 2 } else { 80 });
            assert_eq!(
                group.iter().filter(|c| c.stage == "w1").count(),
                if smoke { 1 } else { 40 }
            );
            assert_eq!(
                group.iter().filter(|c| c.stage == "w3").count(),
                if smoke { 1 } else { 40 }
            );
            let keys = smoke_credentials(group, smoke_fixture_variable).unwrap();
            for case in group {
                assert!(matches!(case.replay, Replay::Missing));
                assert_eq!(case.timeout_seconds, Some(360));
                assert_eq!(case.model.effort, ReasoningEffort::Medium);
                assert_eq!(
                    case.provider.as_ref().unwrap().structured_mode,
                    if case.model.label == "gpt-6-luna" {
                        StructuredMode::NativeSchema
                    } else {
                        StructuredMode::PromptSchema
                    }
                );
                let wire = case.wire_request.to_string();
                assert!(!wire.contains(case.sample["id"].as_str().unwrap()));
                assert!(!wire.contains("\"expect\""));
                let request = smoke_http_request(&http, case, &keys[&case.model.label]).unwrap();
                assert_eq!(
                    request.headers()["user-agent"],
                    model_provider::MODEL_USER_AGENT
                );
                if case.provider.as_ref().unwrap().protocol == ProviderProtocol::AnthropicMessages {
                    assert_eq!(request.headers()["x-api-key"], "fixture-opus-key");
                } else {
                    assert!(
                        request.headers()["authorization"]
                            .to_str()
                            .unwrap()
                            .starts_with("Bearer ")
                    );
                }
                if case.model.label == "gpt-6-luna" {
                    assert!(
                        case.wire_request["instructions"]
                            .as_str()
                            .is_some_and(|s| !s.is_empty())
                    );
                    assert_eq!(case.wire_request["text"]["format"]["strict"], true);
                    assert!(case.wire_request.get("temperature").is_none());
                    assert!(request.url().path().ends_with("/v1/responses"));
                } else {
                    assert_eq!(case.wire_request["output_config"]["effort"], "medium");
                    assert!(case.wire_request["output_config"].get("format").is_none());
                    assert!(
                        case.wire_request["system"]
                            .as_str()
                            .unwrap()
                            .contains(model_provider::PROMPT_SCHEMA_INSTRUCTION)
                    );
                    assert!(
                        case.provider
                            .as_ref()
                            .unwrap()
                            .require_native_schema()
                            .is_err()
                    );
                    assert_eq!(case.wire_request["thinking"]["type"], "adaptive");
                    assert_eq!(request.url().path(), "/v1/messages");
                }
            }
        }
    }
    for (mode, wrong) in [("af163-smoke", "240"), ("af163", "6"), ("af163", "241")] {
        assert!(
            compare_options(|key| match key {
                "EVAL_COMPARE" => Some(mode.into()),
                "EVAL_RECORD" => Some(wrong.into()),
                _ => None,
            })
            .is_err()
        );
    }
    provider_comparison_at(&temp.0, |key| match key {
        "EVAL_COMPARE" => Some("af163-smoke".into()),
        "EVAL_DRY_RUN" => Some("1".into()),
        "EVAL_RECORD" => Some("4".into()),
        "OPENAI_BASE_URL" => Some("https://gpt.fixture.invalid/v1".into()),
        "ANTHROPIC_BASE_URL" => Some("https://opus.fixture.invalid".into()),
        "OPENAI_API_KEY" | "ANTHROPIC_API_KEY" => panic!("dry run read secret"),
        _ => None,
    })
    .unwrap();
}
#[test]
fn af163_smoke_is_strict_separate_from_formal_and_never_retries_schema_errors() {
    let temp = TempRecords::new();
    let root = temp.0.join("smoke");
    let profiles = evaluation_profiles(include_str!("eval/write_stage/providers.json")).unwrap();
    let selected: Vec<_> = AF163_IDS.iter().map(|id| (*id).to_owned()).collect();
    assert!(af163_smoke_check(&root, &profiles, &selected, smoke_fixture_variable).is_err());
    assert!(!temp.0.exists());
    let groups = compare_plan_with_smoke(
        &root,
        &profiles,
        &AF163_IDS,
        smoke_fixture_variable,
        None,
        true,
    )
    .unwrap();
    for group in &groups {
        compare_freeze(group).unwrap();
        let mut calls = 0;
        let report = evaluate_cases(group, Some(2), &mut |c| {
            calls += 1;
            Ok(compare_fixture_response(c))
        })
        .unwrap();
        assert_eq!(calls, 2);
        assert_eq!(report["sent_calls"], 2);
        assert_eq!(
            count_sends([campaign_root(group).unwrap().unwrap().join("sends")]).unwrap(),
            2
        );
    }
    af163_smoke_check(&root, &profiles, &selected, smoke_fixture_variable).unwrap();
    let formal = compare_plan_for(
        &temp.0.join("formal"),
        &profiles,
        &AF163_IDS,
        smoke_fixture_variable,
    )
    .unwrap();
    assert_eq!(
        compare_preflight(&formal, &af163_fixture_options(false, Some("240"))).unwrap(),
        240
    );
    assert!(!temp.0.join("formal").exists());
    let mut changed = profiles.clone();
    changed
        .iter_mut()
        .find(|p| p.id == "gpt-6-luna")
        .unwrap()
        .timeout_seconds += 1;
    assert!(af163_smoke_check(&root, &changed, &selected, smoke_fixture_variable).is_err());
    // Provider failures stay failed; no automatic retry or renewed allowance.
    let other = temp.0.join("failed");
    let groups = compare_plan_with_smoke(
        &other,
        &profiles,
        &["claude-sonnet-5-5"],
        smoke_fixture_variable,
        None,
        true,
    )
    .unwrap();
    compare_freeze(&groups[0]).unwrap();
    let mut calls = 0;
    evaluate_cases(&groups[0], Some(2), &mut |_| {
        calls += 1;
        Ok(af160_http_error(
            400,
            "invalid_request_error",
            "schema_grammar_too_large",
        ))
    })
    .unwrap();
    assert_eq!(calls, 2);
    let selected = vec!["claude-sonnet-5-5".to_owned()];
    assert!(af163_smoke_check(&other, &profiles, &selected, smoke_fixture_variable).is_err());
    let resumed = compare_plan_with_smoke(
        &other,
        &profiles,
        &["claude-sonnet-5-5"],
        smoke_fixture_variable,
        None,
        true,
    )
    .unwrap();
    assert_eq!(
        evaluate_cases(&resumed[0], Some(2), &mut |_| panic!("failure resent")).unwrap()["sent_calls"],
        0
    );
}
#[test]
fn af163_formal_keeps_80_slots_for_subsets_and_uses_existing_breaker() {
    let temp = TempRecords::new();
    let profiles = evaluation_profiles(include_str!("eval/write_stage/providers.json")).unwrap();
    let groups = compare_plan_for(&temp.0, &profiles, &AF163_IDS, smoke_fixture_variable).unwrap();
    let options = compare_options(|key| match key {
        "EVAL_COMPARE" => Some("af163".into()),
        "EVAL_COMPARE_PROFILES" => Some("gpt-6-luna".into()),
        "EVAL_COMPARE_CASES" => Some("w1_01_simple_residence".into()),
        "EVAL_RECORD" => Some("80".into()),
        _ => None,
    })
    .unwrap();
    assert_eq!(compare_preflight(&groups, &options).unwrap(), 2);
    compare_freeze(&groups[0]).unwrap();
    campaign_start(&groups[0][0]).unwrap();
    let resumed = compare_plan_for(&temp.0, &profiles, &AF163_IDS, smoke_fixture_variable).unwrap();
    assert!(matches!(resumed[0][0].replay, Replay::Interrupted));
    assert_eq!(compare_preflight(&resumed, &options).unwrap(), 1);
    let mut calls = 0;
    let report = evaluate_cases(&resumed[0],Some(80),&mut |_|{
        calls+=1;Ok(json!({"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"usage":{"input_tokens":1,"output_tokens":2},
            "output":[{"type":"message","content":[{"type":"output_text","text":"partial"}]}]}))
    }).unwrap();
    assert_eq!(calls, 3);
    assert_eq!(report["circuit_not_sent"], 76);
    assert_eq!(
        count_sends([temp.0.join("campaigns/gpt-6-luna/sends")]).unwrap(),
        4
    );
    let refreshed =
        compare_plan_for(&temp.0, &profiles, &AF163_IDS, smoke_fixture_variable).unwrap();
    assert_eq!(
        evaluate_cases(&refreshed[0], Some(80), &mut |_| panic!("circuit resent")).unwrap()["sent_calls"],
        0
    );
}
#[test]
fn af163_costs_use_actual_usage_unknowns_and_non_observation_denominators() {
    let a = json!({"stage":"w1","id":"a","status":"通过","usage":{"input_tokens":1000,"output_tokens":2000,"output_tokens_details":{"reasoning_tokens":1500}}});
    let mut b = a.clone();
    b["repeat"] = json!(2);
    let cost = compare_cost("gpt-6-luna", &[a.clone(), b.clone()]);
    assert_eq!(cost["stable_passed_samples"], 1);
    assert!((cost["estimated_total_cny"].as_f64().unwrap() - 0.01562).abs() < 1e-10);
    assert_eq!(
        cost["cost_per_stable_passed_sample_cny"],
        cost["estimated_total_cny"]
    );
    assert_eq!(cost["cost_per_passed_attempt_cny"], json!(0.00781));
    let mut observation = a.clone();
    observation["observation_only"] = json!(true);
    observation["status"] = json!("观察");
    let cost = compare_cost("gpt-6-luna", &[a.clone(), b, observation]);
    assert_eq!(cost["passed_attempts"], 2);
    assert_eq!(cost["priced_attempts"], 3);
    assert!(cost["estimated_total_cny"].as_f64().unwrap() > 0.02);
    let failed = json!({"stage":"w1","id":"b","status":"调用失败"});
    let cost = compare_cost("gpt-6-luna", &[a.clone(), failed]);
    assert_eq!(cost["unknown_usage_or_price_attempts"], 1);
    assert!(cost["estimated_total_cny"].is_null());
    assert!(cost["cost_per_passed_attempt_cny"].is_null());
    assert!(cost["known_subtotal_cny"].as_f64().unwrap() > 0.0);
    let mut ds = a.clone();
    ds["usage"]["input_tokens_details"] = json!({"cached_tokens":500});
    assert!(
        (compare_cost("ds-pro", &[ds.clone()])["estimated_total_cny"]
            .as_f64()
            .unwrap()
            - 0.05865)
            .abs()
            < 1e-10
    );
    ds["usage"]["input_tokens_details"]["cached_tokens"] = json!(1001);
    assert!(compare_cost("ds-pro", &[ds])["estimated_total_cny"].is_null());
    let mut haiku = a.clone();
    haiku["usage"]["input_tokens"] = json!(100001);
    assert!(compare_cost("claude-haiku-5-5", &[haiku])["estimated_total_cny"].is_null());
    let cost = compare_cost(
        "gpt-6-luna",
        &[json!({"status":"未录制"}), json!({"status":"熔断未发送"})],
    );
    assert_eq!(cost["priced_attempts"], 0);
    assert_eq!(cost["unknown_usage_or_price_attempts"], 0);
    assert!(cost["cost_per_stable_passed_sample_cny"].is_null());
}
#[test]
fn af163_ten_profile_report_preserves_historic_scores_and_columns() {
    let temp = TempRecords::new();
    let profiles = evaluation_profiles(include_str!("eval/write_stage/providers.json")).unwrap();
    let mut groups =
        compare_plan(&temp.0.join("history"), &profiles, smoke_fixture_variable).unwrap();
    groups.extend(
        compare_plan_for(
            &temp.0.join("new"),
            &profiles,
            &AF163_IDS,
            smoke_fixture_variable,
        )
        .unwrap(),
    );
    let reports: Vec<_> = groups
        .iter()
        .map(|g| evaluate_cases(g, None, &mut |_| panic!("report sent")).unwrap())
        .collect();
    let report = compare_report(&groups, &reports, 0);
    assert_eq!(report["comparison"], "af163");
    assert_eq!(report["fixed_slots"], 760);
    assert_eq!(report["profiles"].as_array().unwrap().len(), 10);
    assert_eq!(report["matrix"].as_array().unwrap().len(), 40);
    assert_eq!(report["scoring_samples"], 39);
    assert_eq!(report["observation_samples"], 1);
    for (i, profile) in items(&report["profiles"]).iter().enumerate() {
        assert_eq!(profile["reference_only"], i < 7);
        assert!(profile.get("w3_verdict").is_some());
        assert!(profile.get("w3_coverage").is_some());
        assert!(profile.get("observations").is_some());
        assert!(profile.get("cost_estimate").is_some());
        assert_eq!(profile["stage_pass_rates"].as_array().unwrap().len(), 2);
        assert!(
            items(&profile["metrics"])
                .iter()
                .all(|m| m.get("cost_estimate").is_some())
        );
    }
    let mut cells: Vec<_> = AF163_IDS
        .iter()
        .map(|id| json!({"profile":id,"total":2,"passed":0,"complete":true,"unstable":false}))
        .collect();
    cells[0]["passed"] = json!(2);
    assert_eq!(
        compare_tier(&cells),
        "低价云档即可（本轮 Luna/Haiku 两次均通过）"
    );
    cells[0]["passed"] = json!(0);
    cells[1]["passed"] = json!(2);
    assert_eq!(compare_tier(&cells), "Sonnet 即可（本轮两次均通过）");
}
#[test]
fn af164_qwen_revocation_rejects_before_secret_lookup_or_sends() {
    for mode in ["af156", "af162"] {
        let error = provider_comparison(|key| match key {
            "EVAL_COMPARE" => Some(mode.into()),
            "EVAL_RECORD" => Some(if mode == "af156" { "160" } else { "2" }.into()),
            "QWEN_API_KEY" | "QWEN_BASE_URL" => panic!("revoked campaign read credentials"),
            _ => None,
        })
        .unwrap_err();
        assert!(error.contains("AF164"));
        assert!(error.contains("revoked"));
    }
}

#[test]
fn af163_cloud_incomplete_excerpts_and_usage_survive_strict_offline_replay() {
    let temp = TempRecords::new();
    let profiles = evaluation_profiles(include_str!("eval/write_stage/providers.json")).unwrap();
    let groups = compare_plan_for(&temp.0, &profiles, &AF163_IDS, smoke_fixture_variable).unwrap();
    let text = "片段".repeat(3000);
    for group in &groups {
        compare_freeze(group).unwrap();
        let envelope = if group[0].provider.as_ref().unwrap().protocol
            == ProviderProtocol::OpenaiResponses
        {
            json!({"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"usage":{"input_tokens":12,"output_tokens":50},
                "output":[{"type":"message","content":[{"type":"output_text","text":text}]}]})
        } else {
            json!({"stop_reason":"max_tokens","usage":{"input_tokens":12,"output_tokens":50},
                "content":[{"type":"thinking","thinking":"PRIVATE_THINKING"},{"type":"text","text":text}]})
        };
        let report = evaluate_cases(&group[..1], Some(80), &mut |_| Ok(envelope.clone())).unwrap();
        let row = &report["results"][0];
        assert_eq!(row["status"], "调用失败");
        assert_eq!(row["diagnostic_fragments"]["scoring_eligible"], false);
        assert_eq!(
            row["response"]["incomplete_details"]["reason"],
            "max_output_tokens"
        );
        assert_eq!(row["usage"], json!({"input_tokens":12,"output_tokens":50}));
        let file: Value = serde_json::from_slice(&std::fs::read(&group[0].path).unwrap()).unwrap();
        let record = &file["recorded"][0];
        assert!(record.get("output_text").is_none());
        assert!(record.get("output").is_none());
        assert!(!file.to_string().contains("PRIVATE_THINKING"));
    }
    let resumed = compare_plan_for(&temp.0, &profiles, &AF163_IDS, smoke_fixture_variable).unwrap();
    for group in resumed {
        let report =
            evaluate_cases(&group[..1], Some(80), &mut |_| panic!("incomplete resent")).unwrap();
        assert_eq!(report["sent_calls"], 0);
        assert_eq!(
            report["results"][0]["diagnostic_fragments"]["diagnostic_only"],
            true
        );
    }
}

fn af166_fixture_variable(name: &str) -> Option<String> {
    match name {
        "QWEN_BASE_URL" => {
            Some("https://ws-fixture.cn-beijing.maas.aliyuncs.com/compatible-mode".into())
        }
        "QWEN_API_KEY" => Some("fixture-qwen-key".into()),
        _ => smoke_fixture_variable(name),
    }
}
#[test]
fn af166_modes_have_four_smoke_two_diagnostic_and_400_formal_slots() {
    let temp = TempRecords::new();
    let profiles = evaluation_profiles(include_str!("eval/write_stage/providers.json")).unwrap();
    for (mode, ids, record, smoke, diagnostic, count) in [
        (
            "af163-smoke",
            AF166_PROMPT_IDS.as_slice(),
            "4",
            true,
            false,
            4,
        ),
        ("af162", AF166_QWEN_IDS.as_slice(), "2", false, true, 2),
        (
            "af163",
            AF163_COMPARE_IDS.as_slice(),
            "400",
            false,
            false,
            400,
        ),
    ] {
        let options = compare_options(|key| match key {
            "EVAL_COMPARE" => Some(mode.into()),
            "EVAL_RECORD" => Some(record.into()),
            "EVAL_COMPARE_PROFILES" => diagnostic.then(|| AF166_QWEN_IDS.join(",")),
            _ => None,
        })
        .unwrap();
        assert_eq!(options.profiles.len(), ids.len());
        let root = temp.0.join(mode);
        let groups = compare_plan_with_smoke(
            &root,
            &profiles,
            ids,
            af166_fixture_variable,
            diagnostic.then_some(SMOKE_SAMPLE),
            smoke,
        )
        .unwrap();
        assert_eq!(groups.iter().map(Vec::len).sum::<usize>(), count);
        assert_eq!(compare_preflight(&groups, &options).unwrap(), count);
        assert!(!root.exists());
        for group in &groups {
            assert!(group.iter().all(|c| matches!(c.replay, Replay::Missing)));
            let case = &group[0];
            if AF166_PROMPT_IDS.contains(&case.model.label.as_str()) {
                assert_eq!(
                    case.provider.as_ref().unwrap().structured_mode,
                    StructuredMode::PromptSchema
                );
                assert!(
                    case.provider
                        .as_ref()
                        .unwrap()
                        .require_native_schema()
                        .is_err()
                );
                assert!(case.wire_request["output_config"].get("format").is_none());
                assert!(
                    case.wire_request["system"]
                        .as_str()
                        .unwrap()
                        .ends_with(&case.request["text"]["format"]["schema"].to_string())
                );
            } else if AF166_QWEN_IDS.contains(&case.model.label.as_str()) {
                assert_eq!(case.model.effort, ReasoningEffort::Omit);
                assert!(case.wire_request.get("reasoning_effort").is_none());
                assert_eq!(
                    case.wire_request["enable_thinking"],
                    case.provider.as_ref().unwrap().thinking.unwrap()
                );
                assert_eq!(
                    case.wire_request["response_format"]["json_schema"]["strict"],
                    true
                );
                assert_eq!(
                    case.wire_request["model"],
                    model_provider::DASHSCOPE_QWEN38_MODEL
                );
                assert!(
                    !case
                        .wire_request
                        .to_string()
                        .contains(case.sample["id"].as_str().unwrap())
                );
                if case.provider.as_ref().unwrap().thinking == Some(true) {
                    assert_eq!(
                        case.wire_request["thinking_budget"],
                        case.request["max_output_tokens"].as_u64().unwrap() / 2
                    );
                } else {
                    assert!(case.wire_request.get("thinking_budget").is_none());
                }
                let mut changed = case.clone();
                changed.wire_request["enable_thinking"] =
                    json!(!case.provider.as_ref().unwrap().thinking.unwrap());
                assert_ne!(case_fingerprint(case), case_fingerprint(&changed));
                let mut changed = case.clone();
                changed.wire_request["thinking_budget"] = json!(7);
                assert_ne!(case_fingerprint(case), case_fingerprint(&changed));
            }
        }
    }
    for (mode, profiles, record, cases) in [
        ("af163-smoke", None, "6", None),
        (
            "af162",
            Some("qwen38-flash-think,qwen37-flash-think"),
            "2",
            None,
        ),
        (
            "af162",
            Some("qwen38-flash-think,qwen38-flash-nothink"),
            "160",
            None,
        ),
        (
            "af162",
            Some("qwen38-flash-think,qwen38-flash-nothink"),
            "2",
            Some("w1_02"),
        ),
        ("af163", None, "401", None),
    ] {
        assert!(
            compare_options(|key| match key {
                "EVAL_COMPARE" => Some(mode.into()),
                "EVAL_COMPARE_PROFILES" => profiles.map(str::to_owned),
                "EVAL_RECORD" => Some(record.into()),
                "EVAL_CASES" => cases.map(str::to_owned),
                _ => None,
            })
            .is_err()
        );
    }
    for (ids, record) in [
        (AF163_IDS.as_slice(), "240"),
        (AF166_QWEN_IDS.as_slice(), "160"),
    ] {
        let options = compare_options(|key| match key {
            "EVAL_COMPARE" => Some("af163".into()),
            "EVAL_COMPARE_PROFILES" => Some(ids.join(",")),
            "EVAL_RECORD" => Some(record.into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(options.profiles.len(), ids.len());
    }
}
#[test]
fn af166_diagnostic_gates_strictly_and_does_not_consume_formal_allowance() {
    let temp = TempRecords::new();
    let profiles = evaluation_profiles(include_str!("eval/write_stage/providers.json")).unwrap();
    let selected: Vec<_> = AF166_QWEN_IDS.iter().map(|s| (*s).to_owned()).collect();
    let root = temp.0.join("diagnostic");
    assert!(af166_diagnostic_check(&root, &profiles, &selected, af166_fixture_variable).is_err());
    assert!(!root.exists());
    let groups = compare_plan_for_sample(
        &root,
        &profiles,
        &AF166_QWEN_IDS,
        af166_fixture_variable,
        Some(SMOKE_SAMPLE),
    )
    .unwrap();
    for group in &groups {
        compare_freeze(group).unwrap();
        let report =
            evaluate_cases(group, Some(1), &mut |c| Ok(compare_fixture_response(c))).unwrap();
        assert_eq!(report["sent_calls"], 1);
    }
    af166_diagnostic_check(&root, &profiles, &selected, af166_fixture_variable).unwrap();
    let formal = compare_plan_for(
        &temp.0.join("formal"),
        &profiles,
        &AF166_QWEN_IDS,
        af166_fixture_variable,
    )
    .unwrap();
    assert!(
        formal
            .iter()
            .flatten()
            .all(|c| matches!(c.replay, Replay::Missing))
    );
    for group in &formal {
        assert!(!group[0].campaign.as_ref().unwrap().exists());
    }
    let resumed = compare_plan_for_sample(
        &root,
        &profiles,
        &AF166_QWEN_IDS,
        af166_fixture_variable,
        Some(SMOKE_SAMPLE),
    )
    .unwrap();
    for group in &resumed {
        assert_eq!(
            evaluate_cases(group, Some(1), &mut |_| panic!("diagnostic resent")).unwrap()["sent_calls"],
            0
        );
    }
    let mut changed = profiles.clone();
    changed
        .iter_mut()
        .find(|p| p.id == AF166_QWEN_IDS[0])
        .unwrap()
        .timeout_seconds += 1;
    assert!(af166_diagnostic_check(&root, &changed, &selected, af166_fixture_variable).is_err());
    for (kind, output) in [
        ("duplicate", {
            let mut o = fake_w1(&groups[0][0].sample);
            let c = o["candidates"][0].clone();
            o["candidates"].as_array_mut().unwrap().push(c);
            o.to_string()
        }),
        (
            "repeated_json",
            "{\"candidates\":[],\"memory_requests\":[]}{\"candidates\":[],\"memory_requests\":[]}"
                .into(),
        ),
        (
            "extra_field",
            "{\"candidates\":[],\"memory_requests\":[],\"extra\":true}".into(),
        ),
        ("incomplete", "partial".into()),
    ] {
        let badroot = temp.0.join(kind);
        let bad = compare_plan_for_sample(
            &badroot,
            &profiles,
            &AF166_QWEN_IDS[..1],
            af166_fixture_variable,
            Some(SMOKE_SAMPLE),
        )
        .unwrap();
        compare_freeze(&bad[0]).unwrap();
        let report=evaluate_cases(&bad[0],Some(1),&mut |_|Ok(json!({"choices":[{"finish_reason":if kind=="incomplete" {"length"} else {"stop"},"message":{"content":output}}],"usage":{"prompt_tokens":2,"completion_tokens":3}}))).unwrap();
        assert_eq!(report["sent_calls"], 1);
        assert!(
            af166_diagnostic_check(&badroot, &profiles, &selected[..1], af166_fixture_variable)
                .is_err(),
            "{kind}"
        );
        if kind == "incomplete" {
            assert_eq!(
                report["results"][0]["diagnostic_fragments"]["diagnostic_only"],
                true
            );
        }
    }
}
#[test]
fn af166_prompt_mode_invalidates_native_recordings_and_campaigns() {
    let temp = TempRecords::new();
    let profiles = evaluation_profiles(include_str!("eval/write_stage/providers.json")).unwrap();
    let mut native = profiles.clone();
    for p in native
        .iter_mut()
        .filter(|p| AF166_PROMPT_IDS.contains(&p.id.as_str()))
    {
        p.provider.structured_mode = StructuredMode::NativeSchema;
    }
    let old = compare_plan_with_smoke(
        &temp.0,
        &native,
        &AF166_PROMPT_IDS,
        smoke_fixture_variable,
        None,
        true,
    )
    .unwrap();
    for group in &old {
        compare_freeze(group).unwrap();
        evaluate_cases(group, Some(2), &mut |c| Ok(compare_fixture_response(c))).unwrap();
    }
    let current = compare_plan_with_smoke(
        &temp.0,
        &profiles,
        &AF166_PROMPT_IDS,
        smoke_fixture_variable,
        None,
        true,
    )
    .unwrap();
    for (old, current) in old.iter().zip(&current) {
        assert!(current.iter().all(|c| matches!(c.replay, Replay::Stale)));
        assert_ne!(case_fingerprint(&old[0]), case_fingerprint(&current[0]));
        assert!(campaign_check(current, 2).is_err());
    }
    let mut invalid: Vec<SmokeProfile> = profiles.clone();
    invalid[0].provider.structured_mode = StructuredMode::PromptSchema;
    assert!(evaluation_profiles(&serde_json::to_string(&json!(invalid.iter().map(|p|json!({"id":p.id,"model":p.model,"provider":p.provider,"effort":p.effort,"max_output_tokens":p.max_output_tokens,"timeout_seconds":p.timeout_seconds,"endpoint_env":p.endpoint_env})).collect::<Vec<_>>())).unwrap()).is_err());
}
#[test]
fn af166_twelve_profile_report_includes_modes_alias_cache_cost_and_observation() {
    let temp = TempRecords::new();
    let profiles = evaluation_profiles(include_str!("eval/write_stage/providers.json")).unwrap();
    let mut groups =
        compare_plan(&temp.0.join("history"), &profiles, af166_fixture_variable).unwrap();
    groups.extend(
        compare_plan_for(
            &temp.0.join("new"),
            &profiles,
            &AF163_COMPARE_IDS,
            af166_fixture_variable,
        )
        .unwrap(),
    );
    let reports: Vec<_> = groups
        .iter()
        .map(|g| evaluate_cases(g, None, &mut |_| panic!("offline report sent")).unwrap())
        .collect();
    let report = compare_report(&groups, &reports, 0);
    assert_eq!(report["comparison"], "af163");
    assert_eq!(report["fixed_slots"], 920);
    assert_eq!(report["profiles"].as_array().unwrap().len(), 12);
    assert_eq!(report["scoring_samples"], 39);
    assert_eq!(report["observation_samples"], 1);
    for profile in items(&report["profiles"]) {
        let id = profile["profile"].as_str().unwrap();
        assert_eq!(profile["reference_only"], COMPARE_IDS.contains(&id));
        if AF166_PROMPT_IDS.contains(&id) {
            assert_eq!(profile["structured_output_note"], "未使用约束解码");
        }
        if AF166_QWEN_IDS.contains(&id) {
            assert_eq!(profile["model_identity_note"], "别名，可能漂移");
        }
        assert!(profile.get("w3_verdict").is_some());
        assert!(profile.get("w3_coverage").is_some());
        assert!(profile.get("observations").is_some());
    }
    let row = json!({"stage":"w1","id":"x","status":"通过","usage":{"input_tokens":1000,"input_tokens_details":{"cached_tokens":500},"output_tokens":1000,"output_tokens_details":{"reasoning_tokens":800}}});
    let cost = compare_cost(AF166_QWEN_IDS[0], &[row]);
    assert!((cost["estimated_total_cny"].as_f64().unwrap() - 0.00315).abs() < 1e-10);
}

#[test]
fn af166_formal_dispatch_rejects_before_credentials_when_prerequisites_missing() {
    for (selected, allowance, reason) in [
        (AF166_QWEN_IDS.join(","), "160", "AF166 Qwen3.8 diagnostic"),
        ("claude-sonnet-5-5".to_owned(), "80", "AF163"),
    ] {
        let temp = TempRecords::new();
        let error = provider_comparison_at(&temp.0, |key| match key {
            "EVAL_COMPARE" => Some("af163".into()),
            "EVAL_COMPARE_PROFILES" => Some(selected.clone()),
            "EVAL_RECORD" => Some(allowance.into()),
            "OPENAI_API_KEY" | "ANTHROPIC_API_KEY" | "QWEN_API_KEY" => {
                panic!("prerequisite rejection must precede credential lookup")
            }
            "QWEN_BASE_URL" => Some("https://dashscope.aliyuncs.com/compatible-mode".into()),
            "OPENAI_BASE_URL" => Some("https://gpt.fixture.invalid/v1".into()),
            "ANTHROPIC_BASE_URL" => Some("https://opus.fixture.invalid".into()),
            _ => None,
        })
        .unwrap_err();
        // Current prompts also invalidate the earlier AF155 prerequisite report. That
        // strict rejection must still occur before credentials or campaign creation.
        assert!(
            error.contains(reason)
                || error
                    == "AF156 historical requests/scores differ from reports-af155; no recording permitted",
            "{error}"
        );
        assert!(!temp.0.exists());
    }
}

#[test]
fn af227_recall_whole_branches_prohibitions_and_mechanical_failures_do_not_mix() {
    let r1 = samples("r1")
        .into_iter()
        .find(|s| s["id"] == "r1_12_statement_plus_question")
        .unwrap();
    for need in ["judge", "context_only"] {
        let mut good = fake_r1(&r1);
        good["need"] = json!(need);
        assert_eq!(score("r1", &r1, Some(&good.to_string()))["status"], "通过");
        good["current_evidence"] = json!("query_only");
        assert_eq!(score("r1", &r1, Some(&good.to_string()))["status"], "失败");
    }
    let mut sample = samples("r4").remove(0);
    let mut a = sample["expect"].clone();
    a["must_not"] = json!(["branch A semantic prohibition"]);
    let mut b = a.clone();
    b["verdict"] = json!("insufficient");
    b["facts"][0]["must"]["current"] = json!("unknown");
    b["must_not"] = json!(["branch B semantic prohibition"]);
    sample["expect"] = json!({"alternatives":[a,b]});
    let mut output = fake_r4(&sample);
    assert_eq!(
        score("r4", &sample, Some(&output.to_string()))["status"],
        "通过"
    );
    output["verdict"] = json!("insufficient");
    assert_eq!(
        score("r4", &sample, Some(&output.to_string()))["status"],
        "失败",
        "branch fields must not cross-pair"
    );
    output["facts"][0]["current"] = json!("unknown");
    let report = score("r4", &sample, Some(&output.to_string()));
    assert_eq!(report["status"], "通过");
    assert_eq!(report["must_not"], json!(["branch B semantic prohibition"]));
    output["facts"][0]["evidence"][0]["quote"]["text"] = json!("not in source");
    assert_eq!(
        score("r4", &sample, Some(&output.to_string()))["status"],
        "失败"
    );
    let residence = samples("r4")
        .into_iter()
        .find(|s| s["id"] == "r4_07_current_message_assertion")
        .unwrap();
    let mut current = fake_r4(&residence);
    current["facts"][0]["supersedes_record_ids"] = json!([7]);
    assert_eq!(
        score("r4", &residence, Some(&current.to_string()))["status"],
        "失败"
    );
    let request = stage_request("r4", &sample).unwrap();
    sample["expect"] = json!({"verdict":"conflict","facts":[],"compute":null});
    sample["label_revision"] = json!({"decision":"another label"});
    assert_eq!(request, stage_request("r4", &sample).unwrap());
}

#[test]
fn af227_r4_computations_check_group_identity_operands_precision_and_results() {
    for prefix in ["r4_14", "r4_15", "r4_16", "r4_18", "r4_19"] {
        let sample = samples("r4")
            .into_iter()
            .find(|s| s["id"].as_str().unwrap().starts_with(prefix))
            .unwrap();
        let mut output = fake_r4(&sample);
        assert_eq!(
            score("r4", &sample, Some(&output.to_string()))["status"],
            "通过",
            "{prefix}"
        );
        if prefix == "r4_16" {
            output["compute"]["unit"] = json!("week");
        } else if prefix == "r4_18" {
            output["compute"]["operands"]
                .as_array_mut()
                .unwrap()
                .reverse();
        } else {
            output["compute"]["items"][0]["event_group"] = json!("wrong-group");
            if prefix != "r4_14" {
                output["compute"]["items"][1]["event_group"] = json!("wrong-group");
            }
        }
        assert_eq!(
            score("r4", &sample, Some(&output.to_string()))["status"],
            "失败",
            "{prefix}"
        );
    }
    let sample = samples("r4")
        .into_iter()
        .find(|s| s["id"] == "r4_14_count_dedupe")
        .unwrap();
    let mut merged = fake_r4(&sample);
    let duplicate = merged["compute"]["items"].as_array_mut().unwrap().remove(1);
    merged["compute"]["items"][0]["evidence"]
        .as_array_mut()
        .unwrap()
        .extend(items(&duplicate["evidence"]).iter().cloned());
    assert_eq!(
        score("r4", &sample, Some(&merged.to_string()))["status"],
        "通过"
    );
    let sample = samples("r4")
        .into_iter()
        .find(|s| s["id"] == "r4_17_elapsed_month_precision")
        .unwrap();
    let mut output = fake_r4(&sample);
    output["facts"] = json!([]);
    assert_eq!(
        score("r4", &sample, Some(&output.to_string()))["status"],
        "失败",
        "supported must retain the coarse time fact"
    );
    output["verdict"] = json!("insufficient");
    assert_eq!(
        score("r4", &sample, Some(&output.to_string()))["status"],
        "通过"
    );
    output["compute"]["items"][0]["date"]["interp"] = json!({"kind":"date","date":"2026-07-01"});
    assert_eq!(
        score("r4", &sample, Some(&output.to_string()))["status"],
        "失败"
    );
}
