//! 单阶段离线评分；样本标签仍为 draft，未录制与过期录制不冒充模型失败。
#[path = "eval/w3/adapter.rs"]
mod adapter;

use hippocampus::{
    EntityId, MemoryType, ModelBudgetConfig, ModelStage, contract::*, structured_request, w2, w4,
    write_models,
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

// Keep this readable label in sync with tests/eval.rs and DEEPSEEK_MODEL.
const MODEL: &str = "DeepSeek-V4.1-Flash";

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
            said_on: input["said_on"].as_str().unwrap(),
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
    let result = text.map(|text| match stage {
        "w1" => score_w1(sample, text, &mut fields),
        "w3" => score_w3(sample, text, &mut fields).map(|_| Value::Null),
        "w4" => score_w4(sample, text, &mut fields).map(|_| Value::Null),
        _ => unreachable!(),
    });
    if let Some(Err(error)) = &result {
        check(
            &mut fields,
            "decode/validate",
            json!(true),
            json!(error),
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
    let mut report = json!({"stage": stage, "id": sample["id"], "label_status": sample["label_status"], "status": status,
        "categories": categories(&fields), "fields": fields, "must_not": sample.get("must_not").or_else(|| sample["expect"].get("must_not")), "note": sample["note"]});
    if stage == "w1" {
        report["raw"] = text
            .map(|t| serde_json::from_str(t).unwrap_or(json!(t)))
            .unwrap_or(Value::Null);
        report["w2_final"] = result.and_then(Result::ok).unwrap_or(Value::Null);
    }
    report
}

fn score(stage: &str, sample: &Value, text: Option<&str>) -> Value {
    let raw = score_raw(stage, sample, text);
    if stage != "w4" || text.is_none() {
        return raw;
    }
    let assembled = serde_json::from_str::<W4Output>(text.unwrap())
        .map_err(|e| e.to_string())
        .and_then(|output| {
            let input: W4Input =
                serde_json::from_value(sample["input"].clone()).map_err(|e| e.to_string())?;
            w4::assemble_new_basis(&input, &output, &|id| {
                sample["sources"][id]["text"].as_str().map(str::to_owned)
            })
            .map_err(|e| e.to_string())
        });
    let mut report = match &assembled {
        Ok(output) => score_raw(stage, sample, Some(&serde_json::to_string(output).unwrap())),
        Err(error) => {
            let mut report = raw.clone();
            let fields = report["fields"].as_array_mut().unwrap();
            check(fields, "assembly", json!(true), json!(error), false);
            report["categories"] = categories(fields);
            report["status"] = json!("失败");
            report
        }
    };
    report["final_result"] = json!({"status":report["status"],"fields":report["fields"],"categories":report["categories"]});
    report["raw_result"] = raw;
    report["raw_output"] = serde_json::from_str(text.unwrap()).unwrap_or(json!(text.unwrap()));
    report["final_output"] = assembled
        .ok()
        .map(|o| serde_json::to_value(o).unwrap())
        .unwrap_or(Value::Null);
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
    let stages: Vec<_> = stages.split(',').map(str::trim).collect();
    if stages.iter().any(|s| !["w1", "w3", "w4"].contains(s)) {
        return Err("EVAL_STAGES only accepts w1,w3,w4".into());
    }
    let prefixes: Vec<_> = prefixes.split(',').map(str::trim).collect();
    if prefixes.len() > 1 && prefixes.iter().any(|p| p.is_empty()) {
        return Err("EVAL_CASES must not contain an empty prefix".into());
    }
    let mut result = Vec::new();
    for stage in ["w1", "w3", "w4"]
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
            let mut payload = json!({"source_snapshot": {"eval_sample": sample["id"]},
                "current_user_message": input["text"], "user_history": input.get("history").cloned().unwrap_or(json!([])),
                "entities": items(&entities).iter().map(|e| json!({"id": e["id"], "owner_id": "eval",
                    "entity_type": "Person", "canonical_name": e["names"][0], "aliases": e["names"]})).collect::<Vec<_>>(),
                "relations": items(&relations).iter().map(|r| json!({"id": r,
                    "memory_types": ["fact", "preference", "constraint", "episode"]})).collect::<Vec<_>>(),
                "said_on": input["said_on"], "utc_offset_seconds": 28_800, "task_id": input.get("task_id").cloned().unwrap_or(Value::Null)});
            if stage == "w1" {
                (
                    ModelStage::W1,
                    "w1_v1",
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
                    "w3_v1",
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
                            text: s["text"].as_str().unwrap().into(),
                            history: vec![],
                        },
                    )
                })
                .collect();
            (
                ModelStage::W4,
                "w4_v2",
                write_models::W4_PROMPT,
                write_models::w4_schema(),
                json!({"contract": "w4.v2", "model": hippocampus::DEEPSEEK_MODEL, "w4_input": input, "sources": sources}),
            )
        }
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
    strip(&mut request);
    let hash = request
        .to_string()
        .bytes()
        .fold(0xcbf29ce484222325u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        });
    json!(format!("fnv1a64:{hash:016x}"))
}

#[derive(Clone)]
struct EvalModel {
    label: String,
    model: Option<String>,
    base: Option<String>,
}
impl EvalModel {
    fn default_model() -> Self {
        Self {
            label: MODEL.into(),
            model: None,
            base: None,
        }
    }
    fn from_env() -> Result<Self, String> {
        let model = Self {
            label: std::env::var("EVAL_MODEL_LABEL").unwrap_or_else(|_| MODEL.into()),
            model: std::env::var("EVAL_MODEL").ok(),
            base: std::env::var("EVAL_BASE_URL")
                .ok()
                .or_else(|| std::env::var("DEEPSEEK_BASE_URL").ok())
                .map(|s| s.trim_end_matches('/').to_owned()),
        };
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
    model: EvalModel,
    repeat: usize,
}

fn plan(selected: Vec<(String, Value)>, root: &Path) -> Result<Vec<Case>, String> {
    plan_once(selected, root, &EvalModel::default_model(), 1)
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
        for repeat in 1..=repeats {
            cases.extend(plan_once(
                vec![case.clone()],
                &root.join(&model.label).join(format!("repeat-{repeat}")),
                model,
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
                if stage == "w4" {
                    let mut payload: Value =
                        serde_json::from_str(request["input"].as_str().unwrap()).unwrap();
                    payload["model"] = json!(id);
                    request["input"] = json!(payload.to_string());
                }
            }
            let path = root.join(format!("{}.json", sample["id"].as_str().unwrap()));
            let file: Value = match std::fs::read_to_string(&path) {
                Ok(text) => {
                    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?
                }
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
            let replay = if let Some(r) = current.iter().rev().find(|r| {
                r["request"] == fingerprint(&request)
                    && r["schema"] == request["text"]["format"]["name"]
                    && r["max_output_tokens"] == request["max_output_tokens"]
                    && r["base_url"] == json!(model.base)
                    && r["repeat"] == repeat
                    && r["endpoint"] == request["model"]
                    && r["recording_version"] == 2
            }) {
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
            Ok(Case {
                stage,
                sample,
                request,
                path,
                file,
                replay,
                model: model.clone(),
                repeat,
            })
        })
        .collect()
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
    let mut file = case.file.clone();
    let mut recorded = items(&file["recorded"]).to_vec();
    let mut record = json!({"recording_version":2,"model":case.model.label,"repeat":case.repeat,
        "schema":case.request["text"]["format"]["name"],"endpoint":case.request["model"],
        "base_url":case.model.base,"max_output_tokens":case.request["max_output_tokens"],
        "request":fingerprint(&case.request),"response":{
            "status":envelope["status"],"incomplete_details":envelope["incomplete_details"],"usage":envelope["usage"]}});
    if let Some(text) = text {
        record["output"] = serde_json::from_str(text).unwrap_or(json!(text));
        record["output_text"] = json!(text);
        record["usage"] = envelope["usage"].clone();
    }
    if let Some(error) = error {
        record["error"] = json!(error);
    }
    recorded.push(record); // Includes failed attempts; reruns replay them instead of picking a success.
    file["recorded"] = json!(recorded);
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
    let planned = preflight(cases, limit)?;
    let mut sent = 0;
    let mut results = Vec::new();
    for case in cases {
        let mut result = match &case.replay {
            Replay::Output(text) => score(&case.stage, &case.sample, Some(text)),
            Replay::Failed(record) => failed_score(case, record),
            _ if limit.is_some() => {
                sent += 1;
                let response = send(&case.request);
                let envelope = response.as_ref().ok().cloned().unwrap_or(Value::Null);
                let text = response.and_then(|e| completed_text(&e));
                match text {
                    Ok(text) => {
                        save_recording(case, &envelope, Some(&text), None)?;
                        score(&case.stage, &case.sample, Some(&text))
                    }
                    Err(error) => {
                        save_recording(case, &envelope, None, Some(&error))?;
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
        result["repeat"] = json!(case.repeat);
        result["max_output_tokens"] = case.request["max_output_tokens"].clone();
        result["request"] = fingerprint(&case.request);
        results.push(result);
    }
    let summary: Vec<_> = ["w1", "w3", "w4"].into_iter().map(|stage| {
        let rows: Vec<_> = results.iter().filter(|r| r["stage"] == stage).collect();
        let count = |status| rows.iter().filter(|r| r["status"] == status).count();
        json!({"stage": stage, "passed": count("通过"), "total": rows.len(),
            "evaluated": count("通过") + count("失败"), "failed": count("失败") + count("调用失败"),
            "missing": count("未录制"), "stale": count("录制过期"), "invalid_input": count("输入无效"),
            "draft": rows.iter().filter(|r| r["label_status"] == "draft").count()})
    }).collect();
    Ok(
        json!({"model": cases.first().map(|c|&c.model.label), "planned_calls": planned, "sent_calls": sent,
            "summary":summary,"per_sample":per_sample(&results),"results":results}),
    )
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
    if std::env::var("EVAL_HISTORY").as_deref() == Ok("1") {
        println!(
            "{}",
            serde_json::to_string_pretty(&historical_reports().unwrap()).unwrap()
        );
        return;
    }
    let gate = eval_gate(std::env::var("EVAL_GATE").ok().as_deref()).unwrap();
    let selected = selected(
        &std::env::var("EVAL_STAGES").unwrap_or_else(|_| "w1,w3,w4".into()),
        &std::env::var("EVAL_CASES").unwrap_or_default(),
    )
    .unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/eval/write_stage/recordings");
    let model = EvalModel::from_env().unwrap();
    let repeats = repeat_count(std::env::var("EVAL_REPEAT").ok().as_deref()).unwrap();
    let cases = plan_repeated(selected, &root, &model, repeats).unwrap();
    let limit = std::env::var("EVAL_RECORD").ok().map(|s| {
        s.parse::<usize>()
            .expect("EVAL_RECORD must be a nonnegative integer")
    });
    let sends = preflight(&cases, limit).unwrap();
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
    let report = evaluate(&cases, limit, &mut |request| {
        let (http, url, key) = live.as_ref().expect("offline replay must never send");
        let response = http
            .post(url)
            .bearer_auth(key)
            .json(request)
            .send()
            .map_err(|e| e.to_string())?;
        if !response.status().is_success() {
            return Err(format!("HTTP {}", response.status()));
        }
        response.json().map_err(|e| e.to_string())
    })
    .unwrap();
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    check_report(&report, gate).expect("stage evaluation failures; see report above");
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
    let stages = std::env::var("EVAL_STAGES").unwrap_or_else(|_| "w1,w3,w4".into());
    let prefix = std::env::var("EVAL_CASES").unwrap_or_default();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/eval/write_stage/recordings");
    let cases = plan(selected(&stages, &prefix).unwrap(), &root).unwrap();
    let mut total = 0;
    println!("写入侧分阶段评测干跑计划（不发送请求）");
    for stage in ["w1", "w3", "w4"] {
        if !stages.split(',').any(|s| s.trim() == stage) {
            continue;
        }
        let count = cases.iter().filter(|c| c.stage == stage).count();
        total += count;
        println!("{stage}: {count} 次潜在真实调用");
    }
    println!("合计: {total} 次潜在真实调用");
    println!(
        "本次需录制: {}",
        cases
            .iter()
            .filter(|c| !matches!(c.replay, Replay::Output(_) | Replay::Failed(_)))
            .count()
    );
    println!("本次真实调用: 0");
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
fn single_eval_model_overrides_all_selected_roles_and_w4_binding_input() {
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
        assert_eq!(case.request["model"], "evaluation-model");
        if case.stage == "w4" {
            let payload: Value =
                serde_json::from_str(case.request["input"].as_str().unwrap()).unwrap();
            assert_eq!(payload["model"], "evaluation-model");
        }
    }
}
