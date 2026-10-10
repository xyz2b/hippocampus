//! Human semantics are separate from automatic fields and cannot override invalid evidence.
use super::*;

pub fn ledger() -> Result<Value, String> {
    let text = if let Ok(path) = std::env::var("EVAL_RECALL_REVIEW_LEDGER") {
        std::fs::read_to_string(path).map_err(|e| e.to_string())?
    } else {
        include_str!("review_ledger.json").into()
    };
    parse(&text)
}
fn parse(text: &str) -> Result<Value, String> {
    let ledger: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    if ledger["version"] != "recall.review@1" || !ledger["entries"].is_array() {
        return Err("invalid recall review ledger".into());
    }
    let mut decisions = std::collections::BTreeMap::new();
    for e in items(&ledger["entries"]) {
        let b = &e["binding"];
        if [
            "id",
            "stage",
            "label_version",
            "request",
            "input_digest",
            "final_output_digest",
            "dedup_key",
        ]
        .iter()
        .any(|key| b[key].as_str().is_none_or(|s| s.trim().is_empty()))
            || b["repeat"].as_u64().is_none_or(|r| r == 0)
            || !matches!(e["decision"].as_str(), Some("accepted" | "rejected"))
            || ["reason", "reviewer"]
                .iter()
                .any(|key| e[key].as_str().is_none_or(|s| s.trim().is_empty()))
            || b["dedup_key"] != dedup(b)
        {
            return Err("invalid recall review entry binding/decision".into());
        }
        if decisions
            .insert(b["dedup_key"].to_string(), e["decision"].clone())
            .is_some_and(|old| old != e["decision"])
        {
            return Err("conflicting recall review decisions".into());
        }
    }
    Ok(ledger)
}
fn digest(value: &Value) -> Value {
    json!(hippocampus::recall_models::fingerprint(value))
}
fn dedup(binding: &Value) -> Value {
    digest(&json!([
        binding["id"],
        binding["stage"],
        binding["label_version"],
        binding["input_digest"],
        binding["final_output_digest"]
    ]))
}
fn binding(case: &Case, row: &Value) -> Option<Value> {
    let output = &row["final_output"];
    if !output.is_object() {
        return None;
    }
    let mut b = json!({"id":case.sample["id"],"stage":case.stage,
        "label_version":digest(&json!([case.sample["contract"],case.sample["label_revision"],case.sample["expect"],case.sample["must_not"],case.sample["note"],case.sample["focus"]])),
        "request":case_fingerprint(case),"repeat":case.repeat,
        "input_digest":digest(&json!([case.stage,case.request["instructions"],case.request["input"]])),
        "final_output_digest":digest(output)});
    b["dedup_key"] = dedup(&b);
    Some(b)
}
pub fn apply(case: &Case, row: &mut Value, ledger: &Value) {
    if !matches!(case.stage.as_str(), "r1" | "r4") {
        return;
    }
    let b = binding(case, row);
    let mechanical = row["status"] == "通过"
        || (row["status"] == "失败"
            && !items(&row["fields"]).is_empty()
            && items(&row["fields"])
                .iter()
                .filter(|f| matches!(f["category"].as_str(), Some("contract" | "reference")))
                .all(|f| f["passed"] == true));
    let entry = b.as_ref().and_then(|b| {
        items(&ledger["entries"])
            .iter()
            .find(|e| e["binding"]["dedup_key"] == b["dedup_key"])
    });
    let decision = entry.map(|e| &e["decision"]);
    // A human cannot override automatic label failures; correct the label and re-score first.
    let status = if !mechanical {
        "not_eligible"
    } else if decision == Some(&json!("rejected")) {
        "rejected"
    } else if row["status"] != "通过" {
        "automatic_failed"
    } else if decision == Some(&json!("accepted")) {
        "confirmed"
    } else {
        "pending"
    };
    row["automatic_status"] = row["status"].clone();
    row["manual_review"] = json!({"status":status,"binding":b,"decision":entry,
        "must_not":row["must_not"],"requirements":if case.stage=="r1" {json!(["intent","subject_binding","conditions_and_time","retrieval_hints","must_not"])} else {json!(["extra_facts","statement_meaning","qualifiers","subject_identity","event_groups_and_operands","must_not"])},
        "note":"Automatic field pass is not full semantic confirmation; raw/final remain in this row."});
}
pub fn summary(rows: &[Value]) -> Value {
    json!(["r1", "r4"].iter().map(|stage| {
        let rows: Vec<_> = rows.iter().filter(|r| r["stage"] == *stage).collect();
        let count = |status| rows.iter().filter(|r| r["manual_review"]["status"] == status).count();
        let mut queue = std::collections::BTreeMap::new();
        for r in &rows {
            if r["manual_review"]["status"] == "pending" {
                queue.entry(r["manual_review"]["binding"]["dedup_key"].to_string()).or_insert(&r["manual_review"]["binding"]);
            }
        }
        json!({"stage":stage,"automatic_field_passed":rows.iter().filter(|r| r["automatic_status"]=="通过").count(),
            "human_confirmed":count("confirmed"),"human_rejected":count("rejected"),"pending":count("pending"),
            "not_eligible":count("not_eligible"),"automatic_failed":count("automatic_failed"),
            "unique_pending_outputs":queue.len(),"review_queue":queue.into_values().collect::<Vec<_>>()})
    }).collect::<Vec<_>>())
}

#[test]
fn af227_review_is_bound_deduplicated_and_never_overrides_failures() {
    let temp = TempRecords::new();
    let sample = samples("r4").remove(0);
    let mut cases = plan_repeated(
        vec![("r4".into(), sample.clone())],
        &temp.0,
        &EvalModel::default_model(),
        2,
    )
    .unwrap();
    let output = fake_r4(&sample).to_string();
    let mut row = score("r4", &sample, Some(&output));
    row["request"] = case_fingerprint(&cases[0]);
    let b = binding(&cases[0], &row).unwrap();
    let ledger = json!({"version":"recall.review@1","entries":[{"binding":b,"decision":"accepted","reason":"checked statement and all extras","reviewer":"test"}]});
    assert!(parse(&ledger.to_string()).is_ok());
    for field in ["request", "repeat", "dedup_key", "label_version"] {
        let mut bad = ledger.clone();
        bad["entries"][0]["binding"][field] = Value::Null;
        assert!(parse(&bad.to_string()).is_err(), "{field}");
    }
    let mut conflicting = ledger.clone();
    let mut rejected = conflicting["entries"][0].clone();
    rejected["decision"] = json!("rejected");
    conflicting["entries"]
        .as_array_mut()
        .unwrap()
        .push(rejected);
    assert!(parse(&conflicting.to_string()).is_err());
    apply(&cases[1], &mut row, &ledger);
    assert_eq!(row["manual_review"]["status"], "confirmed");
    let mut changed = row.clone();
    changed["final_output"]["facts"][0]["statement"] = json!("different semantic output");
    apply(&cases[1], &mut changed, &ledger);
    assert_eq!(changed["manual_review"]["status"], "pending");
    let original_input = cases[1].request["input"].clone();
    changed = row.clone();
    cases[1].request["input"] = json!("different input");
    apply(&cases[1], &mut changed, &ledger);
    assert_eq!(changed["manual_review"]["status"], "pending");
    cases[1].request["input"] = original_input;
    cases[1].sample["expect"]["verdict"] = json!("insufficient");
    apply(&cases[1], &mut row, &ledger);
    assert_eq!(row["manual_review"]["status"], "pending");
    let mut bad = fake_r4(&sample);
    bad["facts"][0]["evidence"][0]["quote"]["text"] = json!("absent");
    row = score("r4", &sample, Some(&bad.to_string()));
    let bad_binding = binding(&cases[0], &row).unwrap();
    let override_ledger = json!({"version":"recall.review@1","entries":[{"binding":bad_binding,"decision":"accepted","reason":"cannot override invalid evidence","reviewer":"test"}]});
    apply(&cases[0], &mut row, &override_ledger);
    assert_eq!(row["manual_review"]["status"], "not_eligible");
    let mut wrong_polarity = fake_r4(&sample);
    wrong_polarity["facts"][0]["polarity"] = json!("negate");
    let mut failed = score("r4", &sample, Some(&wrong_polarity.to_string()));
    assert_eq!(failed["status"], "失败");
    let failed_binding = binding(&cases[0], &failed).unwrap();
    let override_ledger = json!({"version":"recall.review@1","entries":[{"binding":failed_binding,"decision":"accepted","reason":"cannot override automatic label failure","reviewer":"test"}]});
    apply(&cases[0], &mut failed, &override_ledger);
    assert_eq!(failed["manual_review"]["status"], "automatic_failed");
    let mut row1 = score("r4", &sample, Some(&output));
    let mut row2 = row1.clone();
    apply(&cases[0], &mut row1, &json!({"entries":[]}));
    cases[1].sample = sample;
    apply(&cases[1], &mut row2, &json!({"entries":[]}));
    assert_eq!(summary(&[row1, row2])[1]["unique_pending_outputs"], 1);
}
