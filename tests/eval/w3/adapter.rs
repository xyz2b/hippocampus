//! 样本简写 → w1.v1；只展开形状，不纠正样本中的语义或机械错误。
use hippocampus::contract::W1Output;
use serde_json::{Value, json};

pub fn quote(value: &Value) -> Value {
    if value.is_string() {
        json!({"text": value})
    } else {
        value.clone()
    }
}

fn qualifier(value: &Value) -> Value {
    if value.is_null() {
        Value::Null
    } else if value.is_string() {
        json!({"quote": quote(value), "resolved": true})
    } else {
        json!({"quote": quote(&value["quote"]), "resolved": value["resolved"]})
    }
}

pub fn time(value: &Value) -> Value {
    if value.is_null() {
        return Value::Null;
    }
    let mut value = value.clone();
    value["quote"] = quote(&value["quote"]);
    value["anchor"] = json!("said_on");
    if let Some(anchor) = value["interp"].get_mut("anchor_quote") {
        *anchor = quote(anchor);
    }
    value
}

pub fn candidate(value: &Value) -> Value {
    let mut value = value.clone();
    value["subject"] = if value["subject"].is_string() {
        json!({"entity_id": value["subject"]})
    } else {
        json!({"unresolved": quote(&value["subject"]["unresolved"])})
    };
    value["sentence"] = quote(&value["sentence"]);
    if value["value"].is_string() {
        value["value"] = json!({"kind": "text", "text": value["value"],
            "quote": quote(&value["value"])});
    }
    value["uncertainty"] = qualifier(&value["uncertainty"]);
    value["conditions"] = Value::Array(
        value["conditions"]
            .as_array()
            .unwrap()
            .iter()
            .map(qualifier)
            .collect(),
    );
    value["times"] = Value::Array(
        value["times"]
            .as_array()
            .unwrap()
            .iter()
            .map(time)
            .collect(),
    );
    value
}

pub fn expand(input: &Value) -> Result<W1Output, serde_json::Error> {
    let requests: Vec<_> = input["requests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            let mut r = r.clone();
            r["quote"] = quote(&r["quote"]);
            r["target"]["quote"] = r["target"].get("quote").map(quote).unwrap_or(Value::Null);
            r["deadline"] = time(&r["deadline"]);
            r
        })
        .collect();
    serde_json::from_value(
        json!({"candidates": input["candidates"].as_array().unwrap().iter()
        .map(candidate).collect::<Vec<_>>(), "memory_requests": requests}),
    )
}
