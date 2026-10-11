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
        "plan.filter.time":if value["plan"]["filter"]["time"].is_null(){Value::Null}else{normalize_time(&value["plan"]["filter"]["time"])},
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
        // 与生产构造保持一致的完整 r4.v7 包；单阶段评测不调用 R1 或检索模型。
        Ok((
            ModelStage::R4,
            "r4_v7",
            recall_models::R4_PROMPT,
            recall_models::r4_schema_for_plan(&f.plan),
            f.payload(),
        ))
    }
}

pub struct R4Input {
    pub request: RecallRequest,
    pub plan: r1::Plan,
    pub evidence: RecallEvidence,
    pub source_dates: BTreeMap<String, Option<String>>,
    pub source_texts: BTreeMap<String, String>,
    pub utc_offset_seconds: i32,
    pub source_context: BTreeMap<String, Value>,
}

fn required_option<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    <Option<T> as serde::Deserialize>::deserialize(d)
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FullSource {
    text: String,
    #[serde(deserialize_with = "required_option")]
    said_on: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FullInput {
    format: String,
    owner_id: UserId,
    request: RecallRequest,
    plan: R1Output,
    evidence: RecallEvidence,
    entities: Vec<Entity>,
    relations: Vec<String>,
    #[serde(deserialize_with = "required_option")]
    subject_ids: Option<Vec<EntityId>>,
    utc_offset_seconds: i32,
    sources: BTreeMap<String, FullSource>,
    #[serde(default)]
    source_context: BTreeMap<String, Value>,
}

impl R4Input {
    pub fn new(input: &Value) -> Result<Self, String> {
        if input.get("format").is_some() {
            return Self::full(input).map_err(|e| format!("sample_input_rejected: {e}"));
        }
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
            utc_offset_seconds: 28_800,
            source_context: BTreeMap::new(),
        })
    }

    pub fn payload(&self) -> Value {
        let mut payload = recall_models::r4_payload(
            &self.request,
            &self.plan,
            &self.evidence,
            self.utc_offset_seconds,
        );
        if !self.source_context.is_empty() {
            payload["source_context"] = json!(self.source_context);
        }
        payload
    }

    fn full(input: &Value) -> Result<Self, String> {
        let f: FullInput = serde_json::from_value(input.clone()).map_err(|e| e.to_string())?;
        if f.format != "recall.full@1" {
            return Err("unknown full input format".into());
        }
        let offset =
            time::UtcOffset::from_whole_seconds(f.utc_offset_seconds).map_err(|e| e.to_string())?;
        let said_on = time::OffsetDateTime::from_unix_timestamp(f.request.at)
            .map_err(|e| e.to_string())?
            .to_offset(offset)
            .date()
            .to_string();
        let ids: Vec<_> = f.entities.iter().map(|e| e.id.clone()).collect();
        if f.owner_id.0.trim().is_empty()
            || f.entities
                .iter()
                .any(|e| e.owner_id != f.owner_id || e.id.0.trim().is_empty())
            || ids.iter().collect::<std::collections::BTreeSet<_>>().len() != ids.len()
            || f.relations.iter().any(|r| r.trim().is_empty())
            || f.relations
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != f.relations.len()
            || f.subject_ids
                .as_ref()
                .is_some_and(|scope| scope.iter().any(|id| !ids.contains(id)))
        {
            return Err("owner/catalogue/scope mismatch".into());
        }
        let relations = relations(input);
        let task = f.request.task_id.as_deref().map(hippocampus::TaskId::from);
        let ctx = r1::Context {
            query: &f.request.query,
            context: &f.request.context,
            entities: &ids,
            relations: &relations,
            subject_ids: f.subject_ids.as_deref(),
            task_id: task.as_ref(),
            said_on: &said_on,
            utc_offset_seconds: f.utc_offset_seconds,
            supported_units: recall_models::SUPPORTED_UNITS,
        };
        let plan = r1::validate(
            r1::decode(&serde_json::to_string(&f.plan).unwrap()).map_err(|e| e.to_string())?,
            &ctx,
        )
        .map_err(|e| e.to_string())?;
        if plan.output.is_none() {
            return Err("R1 plan rejected".into());
        }
        let source_dates: BTreeMap<_, _> = f
            .sources
            .iter()
            .map(|(id, s)| (id.clone(), s.said_on.clone()))
            .collect();
        let source_texts: BTreeMap<_, _> = f
            .sources
            .iter()
            .map(|(id, s)| (id.clone(), s.text.clone()))
            .collect();
        for (id, source) in &f.sources {
            if id.trim().is_empty()
                || source.text.trim().is_empty()
                || source.said_on.as_ref().is_some_and(|d| day_at(d).is_err())
            {
                return Err("invalid source text/date".into());
            }
        }
        let check_ref = |e: &EvidenceRef| -> Result<(), String> {
            let text = match &e.source {
                SourceRef::Stored { source_id } => {
                    source_texts.get(source_id).ok_or("source missing")?
                }
                SourceRef::Current {} => return Err("stored evidence uses current".into()),
            };
            if !text.contains(&e.quote.text) || e.quote.text.trim().is_empty() {
                return Err("source quote mismatch".into());
            }
            Ok(())
        };
        let mut record_ids = std::collections::BTreeSet::new();
        for record in &f.evidence.records {
            if !record_ids.insert(record.id()) {
                return Err("duplicate record identity".into());
            }
            let refs = match record {
                RecordView::Current(r) => &r.evidence,
                RecordView::Legacy(r) => &r.evidence,
            };
            for e in refs {
                check_ref(e)?;
            }
            match record {
                RecordView::Current(r) => {
                    if r.candidate.kind != CandidateKind::Assertion || refs.is_empty() {
                        return Err("current record must have an assertion and source".into());
                    }
                    let source = refs
                        .iter()
                        .filter_map(|e| match &e.source {
                            SourceRef::Stored { source_id } => f.sources.get(source_id),
                            _ => None,
                        })
                        .find(|s| {
                            s.text.contains(&r.candidate.sentence.text) && s.said_on == r.said_on
                        })
                        .ok_or("sentence source/said_on mismatch")?;
                    let batch = w2::validate(
                        W1Output {
                            candidates: vec![r.candidate.clone()],
                            memory_requests: vec![],
                        },
                        &w2::Context {
                            text: &source.text,
                            entities: &ids,
                            relations: &relations,
                            said_on: source.said_on.as_deref(),
                            utc_offset_seconds: f.utc_offset_seconds,
                            task_id: task.as_ref(),
                        },
                    )
                    .map_err(|e| e.to_string())?;
                    let checked = &batch.candidates[0];
                    if checked.status != w2::Status::Ok
                        || checked.candidate != r.candidate
                        || checked.time_resolutions != r.time_resolutions
                    {
                        return Err("record candidate/time resolutions disagree with W2".into());
                    }
                    if let Subject::Entity(s) = &r.candidate.subject
                        && f.subject_ids
                            .as_ref()
                            .is_some_and(|scope| !scope.iter().any(|id| id.0 == s.entity_id))
                    {
                        return Err("record outside trusted subject scope".into());
                    }
                }
                RecordView::Legacy(r) => {
                    if r.data.id != r.id
                        || r.data.owner_id != f.owner_id
                        || r.data.status == hippocampus::MemoryStatus::Deleted
                        || !ids.contains(&r.data.subject_id)
                        || f.subject_ids
                            .as_ref()
                            .is_some_and(|scope| !scope.contains(&r.data.subject_id))
                    {
                        return Err("legacy record identity/owner/scope mismatch".into());
                    }
                }
            }
        }
        for raw in &f.evidence.raw {
            check_ref(&EvidenceRef {
                source: SourceRef::Stored {
                    source_id: raw.source_id.clone(),
                },
                quote: raw.quote.clone(),
            })?;
            if source_dates.get(&raw.source_id) != Some(&raw.said_on)
                || raw.enrichment == EnrichmentStatus::Deleted
                || raw.grade != RecallGrade::Uncommitted
            {
                return Err("raw source date/status/grade mismatch".into());
            }
        }
        if let Some(current) = &f.evidence.current_message {
            if current.text != f.request.query || current.said_on != said_on {
                return Err("current message/request clock mismatch".into());
            }
            if let Some(id) = &current.source_id
                && (source_texts.get(id) != Some(&current.text)
                    || source_dates.get(id) != Some(&Some(current.said_on.clone())))
            {
                return Err("current source mismatch".into());
            }
        }
        let mut relation_ids = std::collections::BTreeSet::new();
        for relation in &f.evidence.relations {
            if !relation_ids.insert(&relation.id) {
                return Err("duplicate relation identity".into());
            }
            for e in relation.basis.iter().chain(relation.resolution.iter()) {
                check_ref(e)?;
            }
        }
        if f.source_context
            .keys()
            .any(|id| !f.sources.contains_key(id))
        {
            return Err("source_context source missing".into());
        }
        let pending = f
            .evidence
            .records
            .iter()
            .filter(|r| match r {
                RecordView::Current(r) => r.update.status == hippocampus::UpdateStatus::Pending,
                RecordView::Legacy(r) => r
                    .update
                    .as_ref()
                    .is_some_and(|u| u.status == hippocampus::UpdateStatus::Pending),
            })
            .count();
        let gaps: usize = f
            .evidence
            .records
            .iter()
            .map(|r| match r {
                RecordView::Legacy(r) => r.gaps.len(),
                _ => 0,
            })
            .sum();
        let pending_raw = f
            .evidence
            .raw
            .iter()
            .filter(|r| {
                !matches!(
                    r.enrichment,
                    EnrichmentStatus::Committed | EnrichmentStatus::CompletedNoFacts
                )
            })
            .count();
        if f.evidence.completeness.pending_updates != pending
            || f.evidence.completeness.enrichment_gaps < gaps
            || f.evidence.completeness.pending_related_sources < pending_raw
            || (f.subject_ids.as_ref().is_some_and(Vec::is_empty) && !f.evidence.raw.is_empty())
        {
            return Err("completeness counters disagree with records".into());
        }
        Ok(Self {
            request: f.request,
            plan,
            evidence: f.evidence,
            source_dates,
            source_texts,
            utc_offset_seconds: f.utc_offset_seconds,
            source_context: f.source_context,
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
            utc_offset_seconds: f.utc_offset_seconds,
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
            if key == "must_not" {
                continue; // Semantic prohibitions belong to the independent human review.
            }
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
        Ok(serde_json::to_value(output).unwrap())
    } else {
        let f = R4Input::new(&sample["input"])?;
        let evaluated = r4::evaluate_detailed(
            r4::decode_detailed(text).map_err(diagnostic)?,
            &r4::Context {
                evidence: &f.evidence,
                plan: &f.plan,
                request: &f.request,
                utc_offset_seconds: f.utc_offset_seconds,
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
                    "time_quotes" => json!(
                        items(&a["times"])
                            .iter()
                            .map(|t| &t["quote"]["text"])
                            .collect::<Vec<_>>()
                    ),
                    "time_kinds" => json!(
                        items(&a["times"])
                            .iter()
                            .map(|t| &t["interp"]["kind"])
                            .collect::<Vec<_>>()
                    ),
                    "uncertainty" => quote_text(&a[key]),
                    "conditions" => {
                        json!(items(&a[key]).iter().map(quote_text).collect::<Vec<_>>())
                    }
                    _ => a[key].clone(),
                };
                compare(fields, format!("facts[{i}].{key}"), e, &a);
            }
        }
        for (i, forbidden) in items(&sample["expect"]["forbid_facts"]).iter().enumerate() {
            let found = items(&actual["facts"]).iter().any(|a| {
                matches(&a["record_ids"], &forbidden["record_ids"])
                    && forbidden["must"]
                        .as_object()
                        .unwrap()
                        .iter()
                        .all(|(key, value)| matches(&a[key], value))
            });
            check(
                fields,
                format!("forbid_facts[{i}]"),
                json!(false),
                json!(found),
                !found,
            );
        }
        if sample["expect"]["forbid_supersedes"] == true {
            let empty = items(&actual["facts"])
                .iter()
                .all(|a| items(&a["supersedes_record_ids"]).is_empty());
            check(
                fields,
                "forbid_supersedes",
                json!(true),
                json!(empty),
                empty,
            );
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
                    "expected_bound" | "exact_days" | "rounding" | "approximate" => {
                        let field = if key == "expected_bound" {
                            "bound"
                        } else {
                            key
                        };
                        let computed = evaluated
                            .computed
                            .as_ref()
                            .map(|c| serde_json::to_value(c).unwrap())
                            .unwrap_or(Value::Null);
                        compare(fields, format!("computed.{field}"), e, &computed[field]);
                    }
                    "operand_sources" => {
                        let actual_sources: Vec<_> = items(&a["operands"])
                            .iter()
                            .map(|operand| {
                                if operand == "@request.at" {
                                    json!("@request.at")
                                } else {
                                    let item =
                                        items(&a["items"]).iter().find(|i| i["id"] == *operand);
                                    item.map(|i| i["date_source"]["source_id"].clone())
                                        .unwrap_or(Value::Null)
                                }
                            })
                            .collect();
                        let valid = items(e).len() == actual_sources.len()
                            && items(e)
                                .iter()
                                .zip(&actual_sources)
                                .all(|(expected, actual)| expected == actual);
                        check(
                            fields,
                            "compute.operand_sources",
                            e.clone(),
                            json!(actual_sources),
                            valid,
                        );
                    }
                    "groups" => {
                        let valid = group_sources_match(&a["items"], e);
                        check(
                            fields,
                            "compute.groups",
                            e.clone(),
                            a["items"].clone(),
                            valid,
                        );
                    }
                    "coverage" | "unit" => compare(fields, format!("compute.{key}"), e, &a[key]),
                    _ => return Err(format!("unknown recall score field: {key}")),
                }
            }
        }
        Ok(actual)
    }
}

// Match the labelled equivalence relation, never a model's arbitrary group spelling.
fn group_sources_match(actual: &Value, expected: &Value) -> bool {
    let mut names = std::collections::BTreeMap::new();
    let mut covered = std::collections::BTreeSet::new();
    for item in items(actual) {
        let Some(name) = item["event_group"].as_str() else {
            return false;
        };
        let sources: Vec<_> = items(&item["evidence"])
            .iter()
            .filter_map(|e| e["source"]["source_id"].as_str())
            .collect();
        let matching: Vec<_> = items(expected)
            .iter()
            .enumerate()
            .filter(|(_, group)| {
                !sources.is_empty() && sources.iter().all(|s| items(group).contains(&json!(s)))
            })
            .collect();
        if matching.len() != 1 {
            return false;
        }
        let index = matching[0].0;
        if names.insert(index, name).is_some_and(|old| old != name) {
            return false;
        }
        covered.extend(sources);
    }
    names.len() == items(expected).len()
        && names
            .values()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            == names.len()
        && items(expected)
            .iter()
            .flat_map(items)
            .all(|s| s.as_str().is_some_and(|s| covered.contains(s)))
}

#[test]
fn af247_r1_periodic_branches_check_filter_time_without_cross_pairing() {
    let sample = samples("r1")
        .into_iter()
        .find(|s| s["id"] == "r1_11_condition_hint")
        .unwrap();
    let recurring = json!({"role":"event","quote":{"text":"周二"},
        "interp":{"kind":"recurring"},"anchor":"said_on"});
    let mut output = fake_r1(&sample);
    assert_eq!(
        super::score("r1", &sample, Some(&output.to_string()))["status"],
        "通过"
    );
    output["conditions"] = json!([]);
    assert_eq!(
        super::score("r1", &sample, Some(&output.to_string()))["status"],
        "失败"
    );
    output["time"] = recurring.clone();
    for filter in [Value::Null, recurring.clone()] {
        output["plan"]["filter"]["time"] = filter;
        assert_eq!(
            super::score("r1", &sample, Some(&output.to_string()))["status"],
            "通过"
        );
    }
    for bad in [
        json!({"kind":"unknown"}),
        json!({"kind":"anchored","anchor_quote":{"text":"周二"},"unit":"day","offset":-1}),
        json!({"kind":"date","date":"2026-09-29"}),
    ] {
        output["plan"]["filter"]["time"]["interp"] = bad;
        assert_eq!(
            super::score("r1", &sample, Some(&output.to_string()))["status"],
            "失败"
        );
        output["conditions"] = json!([{"quote":{"text":"周二"},"resolved":true}]);
        assert_eq!(
            super::score("r1", &sample, Some(&output.to_string()))["status"],
            "失败"
        );
    }
    output = fake_r1(&sample);
    output["conditions"] = json!([]);
    output["time"] = recurring.clone();
    output["time"]["interp"] = json!({"kind":"unknown"});
    assert_eq!(
        super::score("r1", &sample, Some(&output.to_string()))["status"],
        "失败"
    );
    output["time"] = recurring;
    output["plan"]["filter"]["time"] = output["time"].clone();
    output["plan"]["filter"]["time"]["quote"]["text"] = json!("not in the question");
    assert_eq!(
        super::score("r1", &sample, Some(&output.to_string()))["status"],
        "失败"
    );
}

#[test]
fn af247_r1_filters_are_normalized_and_explicit_null_is_scored() {
    let sample = samples("r1").remove(0);
    let mut output = fake_r1(&sample);
    output["plan"]["filter"]["time"] = json!({"role":"event","quote":{"text":"我"},
        "interp":{"kind":"anchored","anchor_quote":{"text":"我"},"unit":"day","offset":0},"anchor":"said_on"});
    let output: R1Output = serde_json::from_value(output).unwrap();
    let normalized = normalized_r1(&output);
    assert_eq!(normalized["plan.filter.time"]["quote"], "我");
    assert_eq!(
        normalized["plan.filter.time"]["interp"]["anchor_quote"],
        "我"
    );
    assert!(normalized["plan.filter.time"].get("anchor").is_none());
    let mut labelled = sample;
    labelled["expect"]["plan.filter.time"] = Value::Null;
    assert_eq!(
        super::score(
            "r1",
            &labelled,
            Some(&serde_json::to_string(&output).unwrap())
        )["status"],
        "失败"
    );
}

#[test]
fn af247_r1_assertion_alternatives_preserve_subject_evidence_and_question_intent() {
    for prefix in ["r1_02", "r1_17"] {
        let sample = samples("r1")
            .into_iter()
            .find(|s| s["id"].as_str().unwrap().starts_with(prefix))
            .unwrap();
        for need in ["context_only", "judge"] {
            let mut output = fake_r1(&sample);
            output["need"] = json!(need);
            if prefix == "r1_02" {
                output["plan"]["filter"]["relation"] = json!("visited");
            }
            assert_eq!(
                super::score("r1", &sample, Some(&output.to_string()))["status"],
                "通过"
            );
            output["current_evidence"] = json!("query_only");
            assert_eq!(
                super::score("r1", &sample, Some(&output.to_string()))["status"],
                "失败"
            );
            output["current_evidence"] = json!("assertion_present");
            output["subject"]["entity_id"] = json!("e-laowang");
            assert_eq!(
                super::score("r1", &sample, Some(&output.to_string()))["status"],
                "失败"
            );
        }
    }
    for prefix in ["r1_03", "r1_15"] {
        let sample = samples("r1")
            .into_iter()
            .find(|s| s["id"].as_str().unwrap().starts_with(prefix))
            .unwrap();
        let mut output = fake_r1(&sample);
        assert_eq!(
            super::score("r1", &sample, Some(&output.to_string()))["status"],
            "通过"
        );
        output["need"] = json!("context_only");
        assert_eq!(
            super::score("r1", &sample, Some(&output.to_string()))["status"],
            "失败"
        );
        output = fake_r1(&sample);
        output["current_evidence"] = json!("assertion_present");
        assert_eq!(
            super::score("r1", &sample, Some(&output.to_string()))["status"],
            "失败"
        );
        if prefix == "r1_03" {
            output = fake_r1(&sample);
            output["type"] = json!("single");
            output["plan"]["op"] = json!("lookup");
            output["plan"]["filter"]["relation"] = json!("allergic_to");
            assert_eq!(
                super::score("r1", &sample, Some(&output.to_string()))["status"],
                "通过"
            );
            for relation in [Value::Null, json!("fact")] {
                output["plan"]["filter"]["relation"] = relation;
                assert_eq!(
                    super::score("r1", &sample, Some(&output.to_string()))["status"],
                    "失败"
                );
            }
            output["plan"]["filter"]["relation"] = json!("allergic_to");
            for (kind, op) in [("single", "list"), ("aggregate", "lookup")] {
                output["type"] = json!(kind);
                output["plan"]["op"] = json!(op);
                assert_eq!(
                    super::score("r1", &sample, Some(&output.to_string()))["status"],
                    "失败"
                );
            }
        }
    }
}

#[test]
fn af247_r4_qualified_facts_old_reports_and_event_representatives_stay_bounded() {
    let sample = samples("r4")
        .into_iter()
        .find(|s| s["id"] == "r4_03_condition_applies_unknown")
        .unwrap();
    for verdict in ["supported", "insufficient"] {
        let mut output = fake_r4(&sample);
        output["verdict"] = json!(verdict);
        assert_eq!(
            super::score("r4", &sample, Some(&output.to_string()))["status"],
            "通过"
        );
        output["facts"][0]["applies"] = json!("yes");
        assert_eq!(
            super::score("r4", &sample, Some(&output.to_string()))["status"],
            "失败"
        );
        output["facts"][0]["applies"] = json!("unknown");
        output["facts"][0]["conditions"] = json!([]);
        assert_eq!(
            super::score("r4", &sample, Some(&output.to_string()))["status"],
            "失败"
        );
    }
    let sample = samples("r4")
        .into_iter()
        .find(|s| s["id"] == "r4_07_current_message_assertion")
        .unwrap();
    let mut output = fake_r4(&sample);
    let mut old = output["facts"][0].clone();
    old["statement"] = json!("用户曾自述住在杭州；不是当前住址。");
    old["record_ids"] = json!([7]);
    old["evidence"] =
        json!([{"source":{"kind":"stored","source_id":"s-7"},"quote":{"text":"我住在杭州。"}}]);
    old["current"] = json!("no");
    output["facts"].as_array_mut().unwrap().push(old);
    assert_eq!(
        super::score("r4", &sample, Some(&output.to_string()))["status"],
        "通过"
    );
    output["facts"][1]["current"] = json!("yes");
    assert_eq!(
        super::score("r4", &sample, Some(&output.to_string()))["status"],
        "失败"
    );
    output["facts"][1]["current"] = json!("no");
    output["facts"][0]["supersedes_record_ids"] = json!([7]);
    assert_eq!(
        super::score("r4", &sample, Some(&output.to_string()))["status"],
        "失败"
    );
    let sample = samples("r4")
        .into_iter()
        .find(|s| s["id"] == "r4_14_count_dedupe")
        .unwrap();
    for omitted in [0, 1] {
        let mut output = fake_r4(&sample);
        output["compute"]["items"]
            .as_array_mut()
            .unwrap()
            .remove(omitted);
        assert_eq!(
            super::score("r4", &sample, Some(&output.to_string()))["status"],
            "通过"
        );
        output["compute"]["coverage"] = json!("incomplete");
        assert_eq!(
            super::score("r4", &sample, Some(&output.to_string()))["status"],
            "失败"
        );
        output["compute"]["coverage"] = json!("complete");
        output["compute"]["items"][1]["event_group"] =
            output["compute"]["items"][0]["event_group"].clone();
        assert_eq!(
            super::score("r4", &sample, Some(&output.to_string()))["status"],
            "失败"
        );
        output = fake_r4(&sample);
        output["compute"]["items"]
            .as_array_mut()
            .unwrap()
            .remove(omitted);
        output["compute"]["items"][0]["evidence"][0]["quote"]["text"] = json!("not in the source");
        assert_eq!(
            super::score("r4", &sample, Some(&output.to_string()))["status"],
            "失败"
        );
    }
    let sample = samples("r4")
        .into_iter()
        .find(|s| s["id"] == "r4_21_plan_booking_not_execution")
        .unwrap();
    let mut output = fake_r4(&sample);
    assert_eq!(
        super::score("r4", &sample, Some(&output.to_string()))["status"],
        "通过"
    );
    output["verdict"] = json!("supported");
    assert_eq!(
        super::score("r4", &sample, Some(&output.to_string()))["status"],
        "失败"
    );
    output["verdict"] = json!("insufficient");
    output["facts"][2]["polarity"] = json!("affirm");
    assert_eq!(
        super::score("r4", &sample, Some(&output.to_string()))["status"],
        "失败"
    );
}

#[test]
fn af247_label_revisions_do_not_change_model_requests_or_fingerprints() {
    for stage in ["r1", "r4"] {
        for mut sample in samples(stage) {
            let request = stage_request(stage, &sample).unwrap();
            sample["expect"] = json!({"alternatives":[{"need":"judge"},{"verdict":"conflict"}]});
            sample["label_revision"] = json!({"decision":"AF247 test"});
            sample["note"] = json!("evaluation-only diagnostic");
            sample["focus"] = json!("evaluation-only focus");
            let changed = stage_request(stage, &sample).unwrap();
            assert_eq!(request, changed, "{}", sample["id"]);
            assert_eq!(fingerprint(&request), fingerprint(&changed));
        }
    }
}

#[cfg(test)]
pub(super) fn full_sample(sample: &Value) -> Value {
    let f = R4Input::new(&sample["input"]).unwrap();
    let mut result = sample.clone();
    result["input"] = json!({"format":"recall.full@1","owner_id":"eval",
        "request":f.request,"plan":f.plan.output,"evidence":f.evidence,
        "entities":[{"id":"self","owner_id":"eval","entity_type":"Person","canonical_name":"我","aliases":[]}],
        "relations":["fact"],"subject_ids":null,"utc_offset_seconds":28800,
        "sources":f.source_texts.iter().map(|(id,text)|(id.clone(),json!({"text":text,"said_on":f.source_dates[id]}))).collect::<BTreeMap<_,_>>()});
    result
}

#[test]
fn af224_full_form_preserves_all_old_payloads_and_uses_semantic_expectations() {
    for sample in samples("r4")
        .into_iter()
        .filter(|s| s["input"]["format"].is_null())
    {
        let mut full = full_sample(&sample);
        full["fixed_input"] = json!(true);
        assert_eq!(
            request("r4", &sample).unwrap().4,
            request("r4", &full).unwrap().4,
            "{}",
            sample["id"]
        );
        let output = fake_r4(&sample).to_string();
        let report = super::score("r4", &full, Some(&output));
        assert_ne!(report["scoring_scope"], "contract_only");
        assert_eq!(report["status"], "通过", "{}", sample["id"]);
        let mut wrong = full;
        wrong["expect"] = expectation_branches(&wrong["expect"]).remove(0);
        wrong["expect"]["verdict"] = json!("needs_clarification");
        assert_eq!(super::score("r4", &wrong, Some(&output))["status"], "失败");
    }
}

#[test]
fn af224_full_input_keeps_nullable_clocks_plan_qualifiers_subjects_and_spans() {
    let original = samples("r4").remove(0);
    let mut sample = full_sample(&original);
    sample["input"]["entities"].as_array_mut().unwrap().push(json!({"id":"child","owner_id":"eval","entity_type":"Person","canonical_name":"小杉","aliases":[]}));
    sample["input"]["subject_ids"] = json!(["child"]);
    sample["input"]["plan"]["subject"] = json!({"entity_id":"child"});
    let body = "如果天气晴朗，小杉可能去石港游玩，我不去。";
    sample["input"]["sources"]["s-1"] = json!({"text":body,"said_on":null});
    let r = &mut sample["input"]["evidence"]["records"][0];
    r["said_on"] = Value::Null;
    r["candidate"]["speech"] = json!("plan");
    r["candidate"]["subject"] = json!({"entity_id":"child"});
    r["candidate"]["sentence"] = json!({"text":body});
    r["candidate"]["value"] =
        json!({"kind":"text","text":"小杉可能去石港游玩","quote":{"text":"小杉可能去石港游玩"}});
    r["candidate"]["uncertainty"] = json!({"quote":{"text":"可能"},"resolved":true});
    r["candidate"]["conditions"] = json!([{"quote":{"text":"如果天气晴朗"},"resolved":true}]);
    r["evidence"][0]["quote"] = json!({"text":body});
    let input = R4Input::new(&sample["input"]).unwrap();
    assert_eq!(input.source_dates["s-1"], None);
    let record = &input.payload()["evidence"]["records"][0];
    assert_eq!(record["candidate"]["speech"], "plan");
    assert_eq!(record["candidate"]["subject"]["entity_id"], "child");
    assert_eq!(record["candidate"]["value"]["text"], "小杉可能去石港游玩");
    assert_eq!(record["candidate"]["uncertainty"]["quote"]["text"], "可能");
    for mutation in [
        "date",
        "absent_date",
        "quote",
        "duplicate",
        "scope",
        "qualifier",
        "counter",
        "value",
        "time_resolution",
    ] {
        let mut bad = sample.clone();
        match mutation {
            "date" => bad["input"]["sources"]["s-1"]["said_on"] = json!("2026-01-01"),
            "absent_date" => {
                bad["input"]["sources"]["s-1"]
                    .as_object_mut()
                    .unwrap()
                    .remove("said_on");
            }
            "quote" => {
                bad["input"]["evidence"]["records"][0]["evidence"][0]["quote"]["text"] =
                    json!("不存在")
            }
            "duplicate" => {
                let r = bad["input"]["evidence"]["records"][0].clone();
                bad["input"]["evidence"]["records"]
                    .as_array_mut()
                    .unwrap()
                    .push(r);
            }
            "scope" => bad["input"]["subject_ids"] = json!(["self"]),
            "qualifier" => {
                bad["input"]["evidence"]["records"][0]["candidate"]["uncertainty"]["quote"]["text"] =
                    json!("必然")
            }
            "counter" => bad["input"]["evidence"]["completeness"]["pending_updates"] = json!(1),
            "value" => {
                bad["input"]["evidence"]["records"][0]["candidate"]["value"]["text"] =
                    json!("去石港")
            }
            "time_resolution" => {
                bad["input"]["evidence"]["records"][0]["time_resolutions"] = json!([{"bogus":true}])
            }
            _ => unreachable!(),
        }
        assert!(R4Input::new(&bad["input"]).is_err(), "{mutation}");
    }
}

#[test]
fn af224_full_labels_are_outside_fingerprints_and_false_semantic_expectations_fail() {
    let old = samples("r4").remove(0);
    let full = full_sample(&old);
    let request = stage_request_with_output_tokens("r4", &full, 4096).unwrap();
    let mut labels = full.clone();
    labels["expect"]["facts"][0]["must"]["current"] = json!("no");
    labels["focus"] = json!("different annotation");
    assert_eq!(
        request,
        stage_request_with_output_tokens("r4", &labels, 4096).unwrap()
    );
    assert_eq!(
        super::score("r4", &labels, Some(&fake_r4(&old).to_string()))["status"],
        "失败"
    );
    let mut clock = full;
    clock["input"]["sources"]["s-1"]["said_on"] = Value::Null;
    clock["input"]["evidence"]["records"][0]["said_on"] = Value::Null;
    assert_ne!(
        fingerprint(&request),
        fingerprint(&stage_request_with_output_tokens("r4", &clock, 4096).unwrap())
    );
}

#[test]
fn af224_full_records_keep_negative_sibling_identity_and_raw_final_basis_checks() {
    let old = samples("r4")
        .into_iter()
        .find(|s| s["id"] == "r4_10_superseded_history")
        .unwrap();
    let mut old = old;
    old["expect"]["facts"]
        .as_array_mut()
        .unwrap()
        .push(json!({"record_ids":[11],"sources":["s-11"],"must":{"current":"yes"}}));
    let full = full_sample(&old);
    let mut output = fake_r4(&old);
    output["facts"][1]["supersedes_record_ids"] = json!([10]);
    let result = super::score("r4", &full, Some(&output.to_string()));
    assert_eq!(result["raw_result"]["status"], "失败");
    assert_eq!(result["final_result"]["status"], "通过");
    for mutation in ["missing", "quote", "outside"] {
        let mut bad = full.clone();
        let mut output = output.clone();
        match mutation {
            "missing" => {
                bad["input"]["sources"]
                    .as_object_mut()
                    .unwrap()
                    .remove("s-10");
            }
            "quote" => output["facts"][1]["evidence"][0]["quote"]["text"] = json!("不存在"),
            _ => output["facts"][1]["supersedes_record_ids"] = json!([999]),
        }
        assert_ne!(
            super::score("r4", &bad, Some(&output.to_string()))["status"],
            "通过",
            "{mutation}"
        );
    }
    let mut siblings = full_sample(&samples("r4").remove(0));
    let body = "我不使用铁杯，我使用陶杯。";
    siblings["input"]["sources"]["s-1"]["text"] = json!(body);
    let record = &mut siblings["input"]["evidence"]["records"][0];
    record["candidate"]["sentence"]["text"] = json!(body);
    record["evidence"][0]["quote"]["text"] = json!(body);
    record["candidate"]["value"] =
        json!({"kind":"text","text":"我不使用铁杯","quote":{"text":"我不使用铁杯"}});
    record["candidate"]["polarity"] = json!("negate");
    record["update"] = json!({"status":"pending","reasons":["update_scope_incomplete"]});
    let mut other = record.clone();
    other["id"] = json!(2);
    other["candidate"]["value"] =
        json!({"kind":"text","text":"我使用陶杯","quote":{"text":"我使用陶杯"}});
    other["candidate"]["polarity"] = json!("affirm");
    other["update"] = json!({"status":"judged","reasons":[]});
    siblings["input"]["evidence"]["records"]
        .as_array_mut()
        .unwrap()
        .push(other);
    siblings["input"]["evidence"]["completeness"]["pending_updates"] = json!(1);
    let f = R4Input::new(&siblings["input"]).unwrap();
    let records = f.payload()["evidence"]["records"].clone();
    assert_eq!(records[0]["candidate"]["polarity"], "negate");
    assert_eq!(records[1]["candidate"]["polarity"], "affirm");
    assert_ne!(records[0]["id"], records[1]["id"]);
    assert_ne!(records[0]["update"], records[1]["update"]);
}
