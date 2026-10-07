//! Historical USER imports use temporary real SQLite and never HTTP.
use hippocampus::chat::{ChatConfig, ChatMemoryService, EnrichmentMode, memory_tools};
use hippocampus::contract::*;
use hippocampus::enrichment::{ProviderContext, RuntimeInputBudget, SourceInput, UpstreamBatch};
use hippocampus::*;
use serde_json::{Value as Json, json};
use std::path::PathBuf;

struct Db(PathBuf);
impl Db {
    fn new() -> Self {
        Self(
            std::env::temp_dir()
                .join(format!("history-import-{}", diagnostics::new_id()))
                .join("memory.sqlite3"),
        )
    }
    fn open(&self, owner: &str) -> SqliteMemoryStore {
        SqliteMemoryStore::open(&self.0, owner.into()).unwrap()
    }
    fn service(&self, owner: &str) -> ChatMemoryService {
        ChatMemoryService::new(
            self.open(owner),
            ChatConfig {
                enrichment_mode: EnrichmentMode::Off,
                ..Default::default()
            },
        )
        .unwrap()
    }
    fn call(&self, name: &str, input: Json) -> PersistenceResult<Json> {
        self.service("alice").call(name, input, || false)
    }
    fn capture(&self, turn: &str, text: &str, history: Vec<&str>) -> String {
        self.call(
            "memory_remember",
            json!({"text":text,"history":history,"session_id":"history-tests", "turn_id":turn,
            "capture_source":true,"observed_at":1791000000}),
        )
        .unwrap()["source"]["source_id"]
            .as_str()
            .unwrap()
            .into()
    }
    fn status(&self, id: &str) -> Json {
        self.call("memory_source_status", json!({"source_id":id}))
            .unwrap()
    }
    fn command(&self, parent: &str, index: usize, request: &str) -> Json {
        let metadata = self.status(parent);
        json!({"source_id":parent,"history_index":index,"expected_content_fingerprint":metadata["history_entries"][index]["content_fingerprint"],
            "request_id":request,"capture_source":true})
    }
    fn import(&self, input: Json) -> Json {
        self.call("memory_source_import_history", input).unwrap()
    }
    fn calls(&self) -> i64 {
        rusqlite::Connection::open(&self.0)
            .unwrap()
            .query_row("SELECT count(*) FROM model_calls", [], |r| r.get(0))
            .unwrap()
    }
}
impl Drop for Db {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.0.parent().unwrap());
    }
}

#[test]
fn full_authorized_user_entry_imports_once_without_enrichment_and_reopens() {
    let db = Db::new();
    let text = "  我可能周末练习编织。\n如果天气合适就去公园。  ";
    let parent = db.capture("parent", "我补充了地点。", vec![text, "另一条 USER 原话"]);
    let command = db.command(&parent, 0, "import-one");
    let out = db.import(command.clone());
    let id = out["source"]["source_id"].as_str().unwrap();
    assert_eq!(out["enrichment"]["status"], "not_requested");
    assert_eq!(out["enqueued"], false);
    assert_eq!(out["provenance"]["parent_source_id"], parent);
    assert_eq!(out["provenance"]["history_index"], 0);
    assert_eq!(out["provenance"]["before_source_id"], parent);
    assert_eq!(out["time_anchor"]["said_on"], Json::Null);
    assert!(out["time_anchor"]["observed_at"].as_i64().unwrap() > 1791000000);
    assert!(!out.to_string().contains(text.trim()));
    let state = db.open("alice").read().unwrap();
    let child = state.sources.values().find(|s| s.id == id).unwrap();
    assert_eq!(child.request.as_ref().unwrap()["text"], text);
    assert_eq!(child.request.as_ref().unwrap()["history"], json!([]));
    assert_eq!(child.task.accepted_mode.as_deref(), Some("off"));
    assert_eq!(child.task.round, None);
    assert_eq!(child.task.generation, 0);
    assert_eq!(child.index["status"], "ready");
    let before = db.open("alice").read().unwrap().revision;
    assert_eq!(db.import(command)["replayed"], true);
    let duplicate = db.import(db.command(&parent, 0, "other-request"));
    assert_eq!(duplicate["source"]["source_id"], id);
    assert_eq!(duplicate["replayed"], true);
    assert_eq!(db.open("alice").read().unwrap().sources.len(), 2);
    // Same-request replay is read-only; a second authorization may add its own metadata receipt.
    assert!(db.open("alice").read().unwrap().revision >= before);
    assert_eq!(db.calls(), 0);
}

#[test]
fn authorization_fingerprint_slot_owner_and_request_conflicts_reject_atomically() {
    let db = Db::new();
    let parent = db.capture("guard", "我有新的笔记。", vec!["我会看陶艺展。"]);
    let good = db.command(&parent, 0, "guard-one");
    let before = db.open("alice").read().unwrap().revision;
    for (field, value, error) in [
        ("capture_source", json!(false), "operation_not_authorized"),
        ("history_index", json!(90), "history_entry_not_found"),
        (
            "expected_content_fingerprint",
            json!("wrong"),
            "history_content_changed",
        ),
    ] {
        let mut bad = good.clone();
        bad[field] = value;
        assert_eq!(
            db.call("memory_source_import_history", bad)
                .unwrap_err()
                .to_string(),
            error
        );
        assert_eq!(db.open("alice").read().unwrap().revision, before);
    }
    assert_eq!(
        db.service("bob")
            .call("memory_source_import_history", good.clone(), || false)
            .unwrap_err()
            .to_string(),
        "source_not_found"
    );
    db.import(good.clone());
    let mut altered = good;
    altered["history_index"] = json!(1);
    assert_eq!(
        db.call("memory_source_import_history", altered)
            .unwrap_err()
            .to_string(),
        "request_id_conflict"
    );
    assert_eq!(db.calls(), 0);
}

#[test]
fn deleting_parent_propagates_imports_but_preserves_independent_sources() {
    let db = Db::new();
    let old = "我会在河边画画。";
    let parent = db.capture("parent-delete", "我记下了安排。", vec![old]);
    let independent = db.capture("independent", old, vec![]);
    let command = db.command(&parent, 0, "delete-import");
    let child = db.import(command.clone());
    let id = child["source"]["source_id"].as_str().unwrap();
    let out = db
        .call("memory_forget", json!({"source_id":parent}))
        .unwrap();
    assert!(
        out["deleted_source_ids"]
            .as_array()
            .unwrap()
            .contains(&json!(id))
    );
    let state = db.open("alice").read().unwrap();
    assert!(
        state
            .sources
            .values()
            .find(|s| s.id == id)
            .unwrap()
            .request
            .is_none()
    );
    assert_eq!(
        state
            .sources
            .values()
            .find(|s| s.id == independent)
            .unwrap()
            .request
            .as_ref()
            .unwrap()["text"],
        old
    );
    assert_eq!(
        db.call("memory_source_import_history", command)
            .unwrap_err()
            .to_string(),
        "source_deleted"
    );
    assert_eq!(db.calls(), 0);
}

#[test]
fn deleting_import_removes_history_copies_and_keeps_hash_tombstone_after_slot_shift() {
    let db = Db::new();
    let old = "我在湖边骑车。";
    let keep = "我参加手工课。";
    let parent = db.capture("slot-delete", "我留了两条记录。", vec![old, keep, old]);
    let command = db.command(&parent, 0, old);
    let out = db.import(command.clone());
    let child = out["source"]["source_id"].as_str().unwrap();
    db.call("memory_forget", json!({"source_id":child}))
        .unwrap();
    let mut other = db.open("alice");
    other
        .transaction(|state| {
            let p = state.sources.values_mut().find(|s| s.id == parent).unwrap();
            assert_eq!(p.request.as_ref().unwrap()["history"], json!([keep]));
            // A later client/copy tries the deleted content in a different slot.
            p.request.as_mut().unwrap()["history"] = json!([keep, old]);
            Ok(())
        })
        .unwrap();
    let mut shifted = command;
    shifted["history_index"] = json!(1);
    shifted["request_id"] = json!("slot-again");
    assert_eq!(
        db.call("memory_source_import_history", shifted)
            .unwrap_err()
            .to_string(),
        "source_deleted"
    );
    assert!(db.status(&parent)["source"]["status"] == "stored");
    let state = db.open("alice").read().unwrap();
    let tombstone = state.sources.values().find(|s| s.id == child).unwrap();
    assert!(
        tombstone
            .management
            .provenance
            .as_ref()
            .unwrap()
            .get("authorization")
            .is_none()
    );
    assert!(!serde_json::to_string(&tombstone).unwrap().contains(old));
    assert_eq!(db.calls(), 0);
}

#[test]
fn import_tool_schema_requires_explicit_authorization_and_has_no_text_or_clock_input() {
    let tool = memory_tools()
        .into_iter()
        .find(|t| t.name == "memory_source_import_history")
        .unwrap();
    let schema = serde_json::to_value(tool.input_schema).unwrap();
    assert_eq!(schema["additionalProperties"], false);
    assert!(
        schema["required"]
            .as_array()
            .unwrap()
            .contains(&json!("capture_source"))
    );
    assert!(schema["properties"].get("text").is_none());
    assert!(schema["properties"].get("said_on").is_none());
}

#[test]
fn unknown_original_time_never_resolves_relative_dates_but_absolute_dates_still_validate() {
    use hippocampus::contract::*;
    let make = |quote, interp| {
        serde_json::from_value::<TimeInterp>(
            json!({"role":"event","quote":{"text":quote},"anchor":"said_on","interp":interp}),
        )
        .unwrap()
    };
    for interp in [
        json!({"kind":"relative","unit":"day","offset":1}),
        json!({"kind":"month","year_offset":0,"month":9}),
    ] {
        let time = make("时间表达", interp);
        let resolution = w2::resolve_time_interp_nullable(&time, None, 28800).unwrap();
        assert_eq!(resolution.status, ResolutionStatus::Unknown);
        assert_eq!(resolution.start, None);
        assert_eq!(resolution.end_exclusive, None);
    }
    let invalid_month = make("月份", json!({"kind":"month","year_offset":0,"month":13}));
    assert!(w2::resolve_time_interp_nullable(&invalid_month, None, 28800).is_err());
    let date = make("2030-04-12", json!({"kind":"date","date":"2030-04-12"}));
    assert_eq!(
        w2::resolve_time_interp_nullable(&date, None, 28800)
            .unwrap()
            .status,
        ResolutionStatus::Resolved
    );
    let bad = make("2030-04-12", json!({"kind":"date","date":"2030-04-13"}));
    assert!(w2::resolve_time_interp_nullable(&bad, None, 28800).is_err());
}

fn time(quote: &str, interp: Json) -> TimeInterp {
    serde_json::from_value(json!({"role":"event","quote":{"text":quote},
        "anchor":"said_on","interp":interp}))
    .unwrap()
}

fn batch(ctx: &ProviderContext, input: &SourceInput, times: Vec<TimeInterp>) -> UpstreamBatch {
    let config = ChatConfig::default();
    let relations = w2::relation_catalog(&config.predicates, &config.memory_types).unwrap();
    let said_on = if input.provenance.is_some() {
        None
    } else {
        Some("2026-10-03")
    };
    let w2 = w2::validate(
        serde_json::from_value(json!({"memory_requests":[],"candidates":[{
            "kind":"assertion","speech":"plan","subject":{"entity_id":"self"},
            "relation":"fact","memory_type":"fact","sentence":{"text":input.text},
            "value":{"kind":"text","text":input.text,"quote":{"text":input.text}},
            "polarity":"affirm","strength":null,"uncertainty":null,"conditions":[],"times":times
        }]}))
        .unwrap(),
        &w2::Context {
            text: &input.text,
            entities: &[EntityId::from("self")],
            relations: &relations,
            said_on,
            utc_offset_seconds: 28800,
            task_id: ctx.task_id.as_ref(),
        },
    )
    .unwrap();
    assert_eq!(w2.candidates[0].status, w2::Status::Ok);
    let review = W3Output {
        reviews: vec![CandidateReview {
            candidate: 0,
            verdict: Verdict::Supported,
            problems: vec![],
            episode_value: EpisodeValue::NotApplicable,
        }],
        request_reviews: vec![],
        coverage: W3Coverage {
            status: CoverageStatus::Complete,
            missing: vec![],
        },
    }
    .validate(&w2, &ctx.source_binding, &input.text)
    .unwrap();
    UpstreamBatch {
        w2,
        review: Some(review),
    }
}

fn source_key(db: &Db, id: &str) -> String {
    db.open("alice")
        .read()
        .unwrap()
        .sources
        .iter()
        .find(|(_, s)| s.id == id)
        .unwrap()
        .0
        .clone()
}

fn check_nullable<T: serde::Serialize + serde::de::DeserializeOwned>(value: &T) {
    let mut encoded = serde_json::to_value(value).unwrap();
    assert_eq!(encoded["said_on"], Json::Null);
    assert!(serde_json::from_value::<T>(encoded.clone()).is_ok());
    encoded["said_on"] = json!("2026-10-03");
    let old = serde_json::from_value::<T>(encoded.clone()).unwrap();
    assert_eq!(serde_json::to_value(old).unwrap()["said_on"], "2026-10-03");
    encoded.as_object_mut().unwrap().remove("said_on");
    assert!(
        serde_json::from_value::<T>(encoded).is_err(),
        "missing said_on must not mean null"
    );
}

fn assembly(db: &Db, elapsed: bool) -> recall::Assembly {
    let request = recall::Request {
        binding: RecallRequest {
            request_id: format!("nullable-{elapsed}"),
            query: "河湾展览安排".into(),
            context: vec![],
            task_id: None,
            at: time::Date::from_calendar_date(2031, time::Month::June, 20)
                .unwrap()
                .midnight()
                .assume_offset(time::UtcOffset::from_whole_seconds(28800).unwrap())
                .unix_timestamp(),
        },
        owner: UserId::from("alice"),
        subject_ids: None,
        said_on: "2031-06-20".into(),
        utc_offset_seconds: 28800,
        bootstrap: false,
    };
    let config = ChatConfig::default();
    let relations = w2::relation_catalog(&config.predicates, &config.memory_types).unwrap();
    let context = r1::Context {
        query: &request.binding.query,
        context: &[],
        entities: &[EntityId::from("self")],
        relations: &relations,
        subject_ids: None,
        task_id: None,
        said_on: &request.said_on,
        utc_offset_seconds: 28800,
        supported_units: &["day"],
    };
    let output = json!({"need":"judge","current_evidence":"query_only","type":if elapsed {"temporal"} else {"single"},
        "subject":{"entity_id":"self"},"relation":"fact","conditions":[],"time":null,
        "plan":{"op":if elapsed {"elapsed"} else {"lookup"},"filter":{"relation":"fact","time":null},
            "unit":if elapsed {Some("day")} else {None}}});
    let plan = r1::validate(r1::decode(&output.to_string()).unwrap(), &context).unwrap();
    assert!(plan.output.is_some());
    recall::assemble(
        &db.open("alice"),
        &request,
        &plan,
        recall::Budget::default(),
    )
    .unwrap()
}

#[test]
fn nullable_raw_r3_r4_keeps_unknown_relative_time_and_resolves_absolute_time() {
    let db = Db::new();
    let old = "我明天去河湾展览，安排在2031-06-09。";
    let parent = db.capture("raw-parent", "我补充河湾展览安排。", vec![old]);
    let imported = db.import(db.command(&parent, 0, "raw-import"));
    let child = imported["source"]["source_id"].as_str().unwrap();
    for (date, expected) in [
        (
            time("明天", json!({"kind":"relative","unit":"day","offset":1})),
            RecallStatus::Insufficient,
        ),
        (
            time("2031-06-09", json!({"kind":"date","date":"2031-06-09"})),
            RecallStatus::Answered,
        ),
    ] {
        let assembled = assembly(&db, true);
        let raw = assembled
            .evidence
            .raw
            .iter()
            .find(|r| r.source_id == child)
            .unwrap();
        assert_eq!(raw.said_on, None);
        check_nullable(raw);
        let payload = recall_models::r4_input(&assembled, 28800);
        assert_eq!(payload["contract"], "r4.v6");
        assert_eq!(
            payload["source_context"][child]["time_anchor"]["said_on"],
            Json::Null
        );
        assert_eq!(
            payload["source_context"][child]["provenance"]["before_source_id"],
            parent
        );
        let output = json!({"verdict":"supported","facts":[],"compute":{
            "op":"elapsed","items":[{"id":"old-date","evidence":[{"source":{"kind":"stored","source_id":child},
                "quote":{"text":old}}],"quantity":null,"date":date,"date_source":{"kind":"stored","source_id":child},"event_group":null}],
            "operands":["@request.at","old-date"],"unit":"day","coverage":"complete"}});
        let result = assembled
            .finish_r4(&db.open("alice"), Ok(&output.to_string()))
            .unwrap();
        assert_eq!(result.status, expected, "{result:?}");
        assert_eq!(result.semantic_judgment, SemanticJudgment::Model);
        if expected == RecallStatus::Answered {
            assert_eq!(result.computed.unwrap().value, "11");
        } else {
            assert!(result.computed.is_none());
            assert!(
                result
                    .degraded_reasons
                    .iter()
                    .any(|r| r == "compute_insufficient")
            );
        }
    }
    assert_eq!(db.calls(), 0);
}

#[test]
fn imported_older_plan_keeps_nullable_w2_w4_record_and_does_not_override_new_assertion() {
    let db = Db::new();
    let old = "我明天去河湾展览，安排在2031-06-09。";
    let new = "我改为2031-06-10参加河湾展览。";
    let parent = db.capture("new-plan", new, vec![old]);
    let parent_key = source_key(&db, &parent);
    w4_start_round(&mut db.open("alice"), &parent_key, "new-plan-round", false).unwrap();
    let result = enrichment::run_source(
        &mut db.open("alice"),
        &parent_key,
        RuntimeInputBudget::default(),
        |ctx, input| {
            Ok(batch(
                ctx,
                input,
                vec![time(
                    "2031-06-10",
                    json!({"kind":"date","date":"2031-06-10"}),
                )],
            ))
        },
        |_, _, _| panic!("first source skips W4"),
    )
    .unwrap();
    assert_eq!(result["enrichment"]["status"], "committed");
    let new_id = MemoryId(
        result["enrichment"]["candidates"][0]["memory_id"]
            .as_u64()
            .unwrap(),
    );
    let imported = db.import(db.command(&parent, 0, "old-plan-import"));
    let child = imported["source"]["source_id"].as_str().unwrap();
    let child_key = source_key(&db, child);
    w4_start_round(&mut db.open("alice"), &child_key, "old-plan-round", false).unwrap();
    let mut judged = false;
    let result = enrichment::run_source(
        &mut db.open("alice"),
        &child_key,
        RuntimeInputBudget::default(),
        |ctx, input| {
            assert_eq!(
                input.provenance.as_ref().unwrap()["before_source_id"],
                parent
            );
            assert_eq!(input.time_anchor.as_ref().unwrap()["said_on"], Json::Null);
            let b = batch(
                ctx,
                input,
                vec![
                    time("明天", json!({"kind":"relative","unit":"day","offset":1})),
                    time("2031-06-09", json!({"kind":"date","date":"2031-06-09"})),
                ],
            );
            assert_eq!(b.w2.said_on, None);
            assert_eq!(
                b.w2.candidates[0].time_resolutions[0].status,
                ResolutionStatus::Unknown
            );
            assert_eq!(
                b.w2.candidates[0].time_resolutions[1].status,
                ResolutionStatus::Resolved
            );
            Ok(b)
        },
        |_, input, sources| {
            judged = true;
            assert_eq!(input.candidates[0].said_on, None);
            assert!(input.records.iter().any(|r| r.id() == new_id));
            assert_eq!(
                sources[child].provenance.as_ref().unwrap()["before_source_id"],
                parent
            );
            // The injected semantic provider recognizes this as an older independent plan.
            // Code must carry chronology, not infer a state_change from received_sequence.
            Ok(Some(
                serde_json::from_value(json!({"decisions":[{"candidate":0,"status":"decided",
                "action":"coexist","targets":[],"resolves_relations":[],"basis":[{
                    "source":{"kind":"stored","source_id":child},"quote":{"text":old}}],
                "effective":null,"relation_scope":null,"pending_reason":null}]}))
                .unwrap(),
            ))
        },
    )
    .unwrap();
    assert!(judged);
    assert_eq!(result["enrichment"]["status"], "committed", "{result}");
    let old_id = MemoryId(
        result["enrichment"]["candidates"][0]["memory_id"]
            .as_u64()
            .unwrap(),
    );
    let state = db.open("alice").read().unwrap();
    assert!(
        state.sources[&child_key].received_sequence > state.sources[&parent_key].received_sequence
    );
    let original = state.repository.get(new_id).unwrap().unwrap();
    assert_eq!(original.status, MemoryStatus::Active);
    assert_eq!(original.temporal.valid_to, None);
    let RecordView2::Current(old_view) = w4::record_view(&state, old_id).unwrap() else {
        panic!("current record required")
    };
    assert_eq!(old_view.said_on, None);
    assert_eq!(
        old_view.time_resolutions[0].status,
        ResolutionStatus::Unknown
    );
    check_nullable(&old_view);
    let contract = ContractCandidate {
        candidate: old_view.candidate.clone(),
        source: SourceRef::Stored {
            source_id: child.into(),
        },
        said_on: None,
        time_resolutions: old_view.time_resolutions.clone(),
    };
    check_nullable(&contract);
    check_nullable(&W4Candidate {
        index: 0,
        committed_id: None,
        candidate: contract.candidate.clone(),
        source: contract.source.clone(),
        said_on: None,
        time_resolutions: contract.time_resolutions.clone(),
    });
    let assembled = assembly(&db, false);
    let RecordView::Current(old_view) = assembled
        .evidence
        .records
        .iter()
        .find(|r| r.id() == old_id)
        .unwrap()
    else {
        panic!("current record required")
    };
    assert_eq!(old_view.said_on, None);
    check_nullable(old_view);
    assert!(assembled.evidence.records.iter().any(|r| r.id() == new_id));
    let output = json!({"verdict":"supported","compute":null,"facts":[{
        "statement":new,"evidence":[{"source":{"kind":"stored","source_id":parent},"quote":{"text":new}}],
        "record_ids":[new_id],"polarity":"affirm","strength":null,"uncertainty":null,"conditions":[],
        "times":[time("2031-06-10",json!({"kind":"date","date":"2031-06-10"}))],"current":"yes","applies":"yes","supersedes_record_ids":[]}]});
    let result = assembled
        .finish_r4(&db.open("alice"), Ok(&output.to_string()))
        .unwrap();
    assert_eq!(result.status, RecallStatus::Answered, "{result:?}");
    assert_eq!(result.facts[0].record_ids, vec![new_id]);
    assert_eq!(result.facts[0].statement, new);
    db.call("memory_forget", json!({"source_id":parent}))
        .unwrap();
    let deleted = db.open("alice").read().unwrap();
    assert!(deleted.sources[&child_key].request.is_none());
    for id in [old_id, new_id] {
        assert!(w4::record_view(&deleted, id).is_none());
    }
    let retained = serde_json::to_string(&deleted).unwrap();
    assert!(!retained.contains(old));
    assert!(!retained.contains(new));
    assert_eq!(db.calls(), 0);
}

#[test]
fn imported_w2_cannot_substitute_the_receiving_date_for_null() {
    let db = Db::new();
    let parent = db.capture(
        "substitution",
        "我补充河湾展览安排。",
        vec!["我去河湾展览。"],
    );
    let imported = db.import(db.command(&parent, 0, "substitution-import"));
    let key = source_key(&db, imported["source"]["source_id"].as_str().unwrap());
    w4_start_round(&mut db.open("alice"), &key, "substitution-round", false).unwrap();
    let result = enrichment::run_source(
        &mut db.open("alice"),
        &key,
        RuntimeInputBudget::default(),
        |ctx, input| {
            let mut b = batch(ctx, input, vec![]);
            b.w2.said_on = Some("2026-10-03".into());
            Ok(b)
        },
        |_, _, _| panic!("invalid anchor must fail before W4"),
    )
    .unwrap();
    assert_eq!(result["enrichment"]["status"], "failed");
    assert_eq!(
        db.open("alice").read().unwrap().sources[&key]
            .error
            .as_deref(),
        Some("w1_invalid_output")
    );
    assert!(
        db.open("alice")
            .read()
            .unwrap()
            .repository
            .list_by_owner(&UserId::from("alice"))
            .unwrap()
            .is_empty()
    );
    assert_eq!(db.calls(), 0);
}
