//! 仅展开样本 input；标签永不进入模型请求或核验上下文。
use super::*;
use hippocampus::{
    Entity, EntityId, EntityType, MemoryId, MemoryType, UserId, r1, r4, recall_models, w2,
};
use std::collections::BTreeMap;

fn day_at(day: &str) -> Result<i64, String> {
    let parts: Vec<_> = day.split('-').collect();
    if parts.len() != 3 {
        return Err("invalid sample day".into());
    }
    let date = time::Date::from_calendar_date(
        parts[0].parse().map_err(|_| "year")?,
        time::Month::try_from(parts[1].parse::<u8>().map_err(|_| "month")?)
            .map_err(|e| e.to_string())?,
        parts[2].parse().map_err(|_| "day")?,
    )
    .map_err(|e| e.to_string())?;
    Ok(date
        .midnight()
        .assume_offset(time::UtcOffset::from_whole_seconds(28_800).unwrap())
        .unix_timestamp())
}
fn stored_date(input: &Value) -> Result<Option<String>, String> {
    let date: Option<String> =
        serde_json::from_value(input.get("said_on").ok_or("said_on")?.clone())
            .map_err(|e| e.to_string())?;
    if let Some(day) = &date {
        day_at(day)?;
    }
    Ok(date)
}
fn entities(input: &Value) -> Vec<Entity> {
    items(&input["entities"])
        .iter()
        .map(|e| Entity {
            id: e["id"].as_str().unwrap().into(),
            owner_id: "eval".into(),
            entity_type: EntityType::Person,
            canonical_name: e["names"][0].as_str().unwrap().into(),
            aliases: items(&e["names"])
                .iter()
                .map(|s| s.as_str().unwrap().into())
                .collect(),
        })
        .collect()
}
fn relations(input: &Value) -> Vec<w2::Relation> {
    items(&input["relations"])
        .iter()
        .map(|r| w2::Relation {
            id: r.as_str().unwrap().into(),
            memory_types: vec![
                MemoryType::Fact,
                MemoryType::Preference,
                MemoryType::Constraint,
                MemoryType::Episode,
            ],
        })
        .collect()
}
fn r1_context<'a>(
    input: &'a Value,
    ids: &'a [EntityId],
    relations: &'a [w2::Relation],
    context: &'a [String],
    task: Option<&'a hippocampus::TaskId>,
) -> r1::Context<'a> {
    r1::Context {
        query: input["message"].as_str().unwrap(),
        context,
        entities: ids,
        relations,
        subject_ids: None,
        task_id: task,
        said_on: input["said_on"].as_str().unwrap(),
        utc_offset_seconds: 28_800,
        supported_units: recall_models::SUPPORTED_UNITS,
    }
}
fn quote_text(value: &Value) -> Value {
    value["quote"]["text"].clone()
}
fn normalized_r1(output: &R1Output) -> Value {
    let value = serde_json::to_value(output).unwrap();
    json!({"need":value["need"],"current_evidence":value["current_evidence"],"type":value["type"],
        "subject":if value["subject"].get("entity_id").is_some(){value["subject"]["entity_id"].clone()}
            else if value["subject"].get("unresolved").is_some(){json!({"unresolved":value["subject"]["unresolved"]["text"]})}
            else{value["subject"].clone()},
        "conditions":items(&value["conditions"]).iter().map(quote_text).collect::<Vec<_>>(),
        "time":if value["time"].is_null(){Value::Null}else{normalize_time(&value["time"])},
        "plan.op":value["plan"]["op"],"plan.filter.relation":value["plan"]["filter"]["relation"],"unit":value["plan"]["unit"]})
}

pub fn request(
    stage: &str,
    sample: &Value,
) -> Result<(ModelStage, &'static str, &'static str, Value, Value), String> {
    let input = &sample["input"];
    if stage == "r1" {
        let entities = entities(input);
        let ids = entities.iter().map(|e| e.id.clone()).collect::<Vec<_>>();
        let relations = relations(input);
        let context = items(&input["context"])
            .iter()
            .map(|s| s.as_str().unwrap().into())
            .collect::<Vec<_>>();
        let task = input["task_id"].as_str().map(hippocampus::TaskId::from);
        let ctx = r1_context(input, &ids, &relations, &context, task.as_ref());
        r1::not_understood(&ctx).map_err(|e| e.to_string())?;
        Ok((
            ModelStage::R1,
            "r1_v2",
            recall_models::R1_PROMPT,
            recall_models::r1_schema(),
            recall_models::r1_input(&ctx, &entities),
        ))
    } else {
        let f = R4Input::new(input)?;
        // 与生产构造保持一致的完整 r4.v6 包；单阶段评测不调用 R1 或检索模型。
        Ok((
            ModelStage::R4,
            "r4_v6",
            recall_models::R4_PROMPT,
            recall_models::r4_schema_for_plan(&f.plan),
            recall_models::r4_payload(&f.request, &f.plan, &f.evidence, 28_800),
        ))
    }
}

pub struct R4Input {
    pub request: RecallRequest,
    pub plan: r1::Plan,
    pub evidence: RecallEvidence,
    pub source_dates: BTreeMap<String, Option<String>>,
    pub source_texts: BTreeMap<String, String>,
}
impl R4Input {
    pub fn new(input: &Value) -> Result<Self, String> {
        let date = input["at"].as_str().ok_or("R4 sample at must be a date")?;
        let request = RecallRequest {
            request_id: "stage-eval".into(),
            query: input["query"].as_str().ok_or("query")?.into(),
            context: vec![],
            task_id: None,
            at: day_at(date)?,
        };
        let relations = [w2::Relation {
            id: "fact".into(),
            memory_types: vec![MemoryType::Fact],
        }];
        let ids = [EntityId::from("self")];
        let ctx = r1::Context {
            query: &request.query,
            context: &[],
            entities: &ids,
            relations: &relations,
            subject_ids: None,
            task_id: None,
            said_on: date,
            utc_offset_seconds: 28_800,
            supported_units: recall_models::SUPPORTED_UNITS,
        };
        let output = json!({"need":"judge","current_evidence":input["current_evidence"],"type":input["plan"]["type"],
            "subject":{"entity_id":"self"},"relation":input["plan"]["relation"],"conditions":[],"time":null,
            "plan":{"op":input["plan"]["op"],"filter":{"relation":"fact","time":null},"unit":null}});
        let plan = r1::validate(
            r1::decode(&output.to_string()).map_err(|e| e.to_string())?,
            &ctx,
        )
        .map_err(|e| e.to_string())?;
        if plan.output.is_none() {
            return Err("sample_input_rejected: R1 plan".into());
        }
        let mut source_dates = BTreeMap::new();
        let mut source_texts = BTreeMap::new();
        let mut records = vec![];
        for r in items(&input["records"]) {
            let id = MemoryId(r["id"].as_u64().ok_or("record id")?);
            let said_on = stored_date(r)?;
            let text = r["sentence"].as_str().unwrap_or("legacy");
            let evidence = r["source"]
                .as_str()
                .map(|id| {
                    source_dates.insert(id.into(), said_on.clone());
                    source_texts.insert(id.into(), text.into());
                    vec![EvidenceRef {
                        source: SourceRef::Stored {
                            source_id: id.into(),
                        },
                        quote: Quote { text: text.into() },
                    }]
                })
                .unwrap_or_default();
            let update = match r["update"].as_str() {
                Some("judged") => Some(hippocampus::UpdateState::from_reasons(vec![])),
                Some(s) if s.starts_with("pending:") => Some(hippocampus::UpdateState {
                    status: hippocampus::UpdateStatus::Pending,
                    reasons: vec![
                        serde_json::from_value(json!(s.trim_start_matches("pending:")))
                            .map_err(|e| e.to_string())?,
                    ],
                }),
                _ => None,
            };
            let termination_reason = r["termination"]
                .as_str()
                .map(|s| match s {
                    "state_changed" => Ok(hippocampus::SupersedeReason::StateChanged),
                    "correction" => Ok(hippocampus::SupersedeReason::Correction),
                    "user_retracted" | "retracted" => {
                        Ok(hippocampus::SupersedeReason::UserRetracted)
                    }
                    _ => Err("unknown sample termination"),
                })
                .transpose()?;
            let status: StoredStatus =
                serde_json::from_value(r["status"].clone()).map_err(|e| e.to_string())?;
            if r["legacy_gaps"].is_array() || update.is_none() {
                let data = hippocampus::Memory {
                    id,
                    owner_id: UserId::from("eval"),
                    evidence: vec![],
                    subject_id: "self".into(),
                    memory_type: MemoryType::Fact,
                    predicate: "fact".into(),
                    value: hippocampus::MemoryValue::Text(text.into()),
                    polarity: None,
                    strength: None,
                    scope: hippocampus::Scope {
                        domain: None,
                        conditions: vec![],
                    },
                    retention_tier: hippocampus::RetentionTier::LongTerm,
                    status: match status {
                        StoredStatus::Active => hippocampus::MemoryStatus::Active,
                        StoredStatus::Superseded => hippocampus::MemoryStatus::Superseded,
                        _ => hippocampus::MemoryStatus::Expired,
                    },
                    temporal: hippocampus::Temporal::observed_at(hippocampus::Timestamp(day_at(
                        said_on.as_deref().ok_or("legacy record observed date")?,
                    )?)),
                    extraction_confidence: hippocampus::Confidence::High,
                    certainty: hippocampus::Confidence::High,
                    supersedes: None,
                    superseded_by: None,
                    supersede_reason: termination_reason,
                    explicit_memory_request: false,
                    reinforcement_count: 0,
                    retrieval_count: 0,
                    last_reinforced_at: None,
                    last_retrieved_at: None,
                    tags: vec![],
                };
                records.push(RecordView::Legacy(LegacyRecordView {
                    id,
                    data,
                    evidence,
                    grade: Grade::Committed,
                    update,
                    gaps: serde_json::from_value(r["legacy_gaps"].clone())
                        .map_err(|e| e.to_string())?,
                }));
            } else {
                let candidate:Candidate=serde_json::from_value(json!({"kind":"assertion","speech":"user_statement","subject":{"entity_id":"self"},
                    "relation":"fact","memory_type":"fact","value":{"kind":"text","text":text,"quote":{"text":text}},"sentence":{"text":text},
                    "polarity":r.get("polarity").unwrap_or(&json!("affirm")),"strength":null,"uncertainty":null,
                    "conditions":items(&r["conditions"]).iter().map(|q|json!({"quote":{"text":q},"resolved":true})).collect::<Vec<_>>(),
                    "times":r["times"]})).map_err(|e|e.to_string())?;
                records.push(RecordView::Current(CurrentRecordView {
                    id,
                    stored_status: status,
                    termination_reason,
                    ended: serde_json::from_value(r["ended"].clone()).map_err(|e| e.to_string())?,
                    candidate,
                    evidence,
                    grade: Grade::Committed,
                    said_on,
                    time_resolutions: vec![],
                    history: vec![],
                    open_relations: items(&input["relations"])
                        .iter()
                        .filter(|l| items(&l["members"]).contains(&json!(id.0)))
                        .map(|l| l["id"].as_str().unwrap().into())
                        .collect(),
                    update: update.unwrap(),
                }));
            }
        }
        let raw: Vec<_> = items(&input["raw"])
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let source_id = r["source"].as_str().ok_or("raw source")?.to_owned();
                let said_on = stored_date(r)?;
                source_dates.insert(source_id.clone(), said_on.clone());
                source_texts.insert(
                    source_id.clone(),
                    r["text"].as_str().ok_or("raw text")?.into(),
                );
                Ok(RawEvidence {
                    source_id,
                    quote: Quote {
                        text: r["text"].as_str().ok_or("raw text")?.into(),
                    },
                    said_on,
                    sequence: (records.len() + i + 1) as u64,
                    grade: RecallGrade::Uncommitted,
                    enrichment: serde_json::from_value(r["enrichment"].clone())
                        .map_err(|e| e.to_string())?,
                    memory_requests: vec![],
                })
            })
            .collect::<Result<_, String>>()?;
        let relations = items(&input["relations"])
            .iter()
            .map(|r| {
                let members: Vec<MemoryId> =
                    serde_json::from_value(r["members"].clone()).map_err(|e| e.to_string())?;
                let basis = records
                    .iter()
                    .filter(|r| members.contains(&r.id()))
                    .flat_map(|r| match r {
                        RecordView::Current(r) => r.evidence.clone(),
                        RecordView::Legacy(r) => r.evidence.clone(),
                    })
                    .collect();
                Ok(RelationLink {
                    id: r["id"].as_str().ok_or("relation id")?.into(),
                    kind: hippocampus::contract::RelationKind::Conflict,
                    members,
                    conditions: vec![],
                    times: vec![],
                    basis,
                    status: serde_json::from_value(r["status"].clone())
                        .map_err(|e| e.to_string())?,
                    resolution: None,
                })
            })
            .collect::<Result<_, String>>()?;
        let pending_updates=records.iter().filter(|r|matches!(r,RecordView::Current(r) if r.update.status==hippocampus::UpdateStatus::Pending)).count();
        let gaps = records
            .iter()
            .map(|r| match r {
                RecordView::Legacy(r) => r.gaps.len(),
                _ => 0,
            })
            .sum();
        let completeness = RecallCompleteness {
            retrieval_truncated: input["completeness"]["retrieval_truncated"]
                .as_bool()
                .ok_or("truncated")?,
            plan_complete: input["completeness"]["plan_complete"]
                .as_bool()
                .ok_or("plan_complete")?,
            pending_related_sources: raw.len(),
            pending_updates,
            enrichment_gaps: gaps,
        };
        let evidence = RecallEvidence {
            current_message: Some(CurrentMessage {
                source_id: None,
                text: request.query.clone(),
                said_on: date.into(),
                sequence: None,
                enrichment: EnrichmentStatus::NotStored,
                memory_requests: vec![],
            }),
            records,
            raw,
            relations,
            completeness,
        };
        Ok(Self {
            request,
            plan,
            evidence,
            source_dates,
            source_texts,
        })
    }
}

pub fn assemble_r4(sample: &Value, text: &str) -> Result<R4Output, String> {
    let f = R4Input::new(&sample["input"])?;
    let output = r4::decode_detailed(text).map_err(diagnostic)?;
    Ok(r4::assemble_old_basis(
        &output,
        &r4::Context {
            evidence: &f.evidence,
            plan: &f.plan,
            request: &f.request,
            utc_offset_seconds: 28_800,
            source_dates: &f.source_dates,
        },
        &|id| f.source_texts.get(id).cloned(),
    ))
}

fn diagnostic(d: r4::ValidationDiagnostic) -> String {
    json!({"code":d.error.to_string(),"rule":d.error.0,"field":d.field,"cause":d.cause}).to_string()
}

pub fn score(
    stage: &str,
    sample: &Value,
    text: &str,
    fields: &mut Vec<Value>,
) -> Result<Value, String> {
    if stage == "r1" {
        let input = &sample["input"];
        let entities = entities(input);
        let ids = entities.iter().map(|e| e.id.clone()).collect::<Vec<_>>();
        let relations = relations(input);
        let context = items(&input["context"])
            .iter()
            .map(|s| s.as_str().unwrap().into())
            .collect::<Vec<_>>();
        let task = input["task_id"].as_str().map(hippocampus::TaskId::from);
        let ctx = r1_context(input, &ids, &relations, &context, task.as_ref());
        let plan = r1::validate(r1::decode(text).map_err(|e| e.to_string())?, &ctx)
            .map_err(|e| e.to_string())?;
        let output = plan.output.ok_or("r1.validate")?;
        let actual = normalized_r1(&output);
        for (key, expected) in sample["expect"].as_object().unwrap() {
            if key == "time" && expected.is_null() {
                continue; // Z8: no asserted time label; mechanical validation still ran.
            }
            if key == "conditions" {
                let passed = items(expected)
                    .iter()
                    .all(|e| items(&actual[key]).iter().any(|a| matches(a, e)));
                check(fields, key, expected.clone(), actual[key].clone(), passed);
            } else {
                compare(fields, key, expected, &actual[key]);
            }
        }
        Ok(Value::Null)
    } else {
        let f = R4Input::new(&sample["input"])?;
        let evaluated = r4::evaluate_detailed(
            r4::decode_detailed(text).map_err(diagnostic)?,
            &r4::Context {
                evidence: &f.evidence,
                plan: &f.plan,
                request: &f.request,
                utc_offset_seconds: 28_800,
                source_dates: &f.source_dates,
            },
        )
        .map_err(diagnostic)?;
        let actual = serde_json::to_value(&evaluated.output).unwrap();
        compare(
            fields,
            "verdict",
            &sample["expect"]["verdict"],
            &actual["verdict"],
        );
        for (i, e) in items(&sample["expect"]["facts"]).iter().enumerate() {
            let matching: Vec<_> = items(&actual["facts"])
                .iter()
                .filter(|a| {
                    matches(&a["record_ids"], &e["record_ids"])
                        && items(&e["sources"]).iter().all(|source| {
                            items(&a["evidence"]).iter().any(|ev| {
                                source
                                    == if ev["source"]["kind"] == "current" {
                                        &ev["source"]["kind"]
                                    } else {
                                        &ev["source"]["source_id"]
                                    }
                            })
                        })
                })
                .collect();
            check(
                fields,
                format!("facts[{i}].identity"),
                json!(1),
                json!(matching.len()),
                matching.len() == 1,
            );
            let a = matching.first().copied().unwrap_or(&Value::Null);
            for (key, e) in e["must"].as_object().unwrap() {
                let a = match key.as_str() {
                    "uncertainty" => quote_text(&a[key]),
                    "conditions" => {
                        json!(items(&a[key]).iter().map(quote_text).collect::<Vec<_>>())
                    }
                    _ => a[key].clone(),
                };
                compare(fields, format!("facts[{i}].{key}"), e, &a);
            }
        }
        let expected = &sample["expect"]["compute"];
        let a = &actual["compute"];
        if expected.is_null() {
            compare(fields, "compute", expected, a);
        } else if expected["insufficient_ok"] == true {
            check(
                fields,
                "compute.insufficient_ok",
                json!(true),
                json!(evaluated.computed.is_none()),
                evaluated.computed.is_none(),
            );
            if !a.is_null() {
                compare(fields, "compute.op", &expected["op"], &a["op"]);
            }
        } else {
            compare(fields, "compute.op", &expected["op"], &a["op"]);
            for (key, e) in expected.as_object().unwrap() {
                match key.as_str() {
                    "op" => (),
                    "items" => {
                        compare(fields, "compute.items", e, &json!(items(&a["items"]).len()))
                    }
                    "operands" if e.is_number() => {
                        compare(fields, "compute.operands", e, &json!(items(&a[key]).len()))
                    }
                    "operands" => {
                        let operands = items(&a[key]);
                        let valid = operands.len() == 2
                            && operands[0] == "@request.at"
                            && items(&a["items"]).iter().any(|i| i["id"] == operands[1]);
                        check(fields, "compute.operands", e.clone(), a[key].clone(), valid);
                    }
                    "distinct_groups" => {
                        let groups: std::collections::BTreeSet<_> = items(&a["items"])
                            .iter()
                            .filter_map(|i| i["event_group"].as_str())
                            .collect();
                        compare(fields, "compute.distinct_groups", e, &json!(groups.len()));
                    }
                    "quantities" => compare(
                        fields,
                        "compute.quantities",
                        e,
                        &json!(
                            items(&a["items"])
                                .iter()
                                .map(|i| &i["quantity"]["amount"])
                                .collect::<Vec<_>>()
                        ),
                    ),
                    "expected_value" => compare(
                        fields,
                        "computed.value",
                        e,
                        &evaluated
                            .computed
                            .as_ref()
                            .map(|c| json!(c.value))
                            .unwrap_or(Value::Null),
                    ),
                    "coverage" | "unit" => compare(fields, format!("compute.{key}"), e, &a[key]),
                    _ => return Err(format!("unknown recall score field: {key}")),
                }
            }
        }
        Ok(actual)
    }
}
