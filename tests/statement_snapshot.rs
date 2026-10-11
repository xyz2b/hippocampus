//! AF283: real SQLite/W5 fixtures, bootstrap snapshots and publication barriers; zero model calls.
#![allow(dead_code)] // Shared W4 fixture helpers also cover unrelated offline scenarios.
include!("eval/w4/runtime.rs");

fn snapshot_service(db: &Db, enabled: bool) -> hippocampus::chat::ChatMemoryService {
    hippocampus::chat::ChatMemoryService::new(
        db.open(),
        hippocampus::chat::ChatConfig {
            statement_snapshot: StatementSnapshotConfig {
                enabled,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap()
}
fn boot(id: &str) -> Json {
    json!({"query":"全局回答要求","bootstrap":true,"request_id":id,"at":1_790_000_000})
}
fn snapshot_candidate(sid: &str, text: &str) -> W4Candidate {
    let mut c = fact_candidate(sid, text);
    c.candidate.memory_type = "fact".into();
    c
}
fn statement(db: &Db, sid: &str, text: &str, relation: &str) -> MemoryId {
    let mut c = snapshot_candidate(sid, text);
    c.candidate.relation = relation.into();
    statement_candidate(db, sid, text, c, None)
}
fn statement_candidate(
    db: &Db,
    sid: &str,
    text: &str,
    c: W4Candidate,
    task: Option<TaskId>,
) -> MemoryId {
    claim_source(db, sid, text);
    if let Some(task) = &task {
        db.open()
            .transaction(|s| {
                s.sources.get_mut(sid).unwrap().request.as_mut().unwrap()["task_id"] =
                    json!(task.0);
                Ok(())
            })
            .unwrap();
    }
    let id = commit_new(db, sid, 0, task, vec![c], true)[0];
    w5_finish_task(&mut db.open(), &claim(sid), &W5TaskEnd::Committed).unwrap();
    id
}
fn snapshot_ids(out: &Json) -> Vec<u64> {
    out["statement_snapshot"]["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_u64().unwrap())
        .collect()
}
fn assert_no_calls(db: &Db) {
    let n: i64 = rusqlite::Connection::open(&db.0)
        .unwrap()
        .query_row("SELECT count(*) FROM model_calls", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0);
}

#[test]
fn af283_default_output_and_global_context_stay_unchanged_except_version() {
    let db = Db::new();
    statement(&db, "home", "我住在星港。", "lives_in");
    let mut disabled = snapshot_service(&db, false);
    let out = disabled
        .call("memory_recall", boot("disabled"), || false)
        .unwrap();
    assert_eq!(out["version"], "recall.v9");
    assert!(out.get("statement_snapshot").is_none());
    assert_eq!(out["records"], json!([]));
    let mut enabled = snapshot_service(&db, true);
    let response = enabled.call_with_global_context("memory_recall", boot("enabled"), || false);
    let body = response.structured_content.unwrap();
    assert_eq!(
        body["recall"]["statement_snapshot"]["records"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(body["global_context"]["result"]["facts"], json!([]));
    let ordinary = enabled
        .call_with_global_context(
            "memory_recall",
            json!({
                "query":"住址原话是什么？","request_id":"ordinary","offline":true
            }),
            || false,
        )
        .structured_content
        .unwrap();
    assert!(ordinary["recall"].get("statement_snapshot").is_none());
    let prepared = enabled.call("memory_prepare", json!({"offline":true,"user_turn":{
        "session_id":"snapshot-test","turn_id":"prepare","text":"我喜欢手工。","capture_source":true
    }}), ||false).unwrap();
    assert!(prepared["recall"].get("statement_snapshot").is_none());
    assert_no_calls(&db);
}

#[test]
fn af283_mechanical_selection_excludes_pending_legacy_unknown_unauthorized_and_task_records() {
    let db = Db::new();
    let good = statement(&db, "good", "我住在星港。", "lives_in");
    let pending = statement(&db, "pending", "我住在月港。", "lives_in");
    let legacy = statement(&db, "legacy", "我在石屋工作。", "works_at");
    let unknown = statement(&db, "unknown", "我是讲解员。", "job_title");
    let conflict = statement(&db, "conflict", "我住在争议港。", "lives_in");
    let expired = statement(&db, "expired", "我住在旧港。", "lives_in");
    statement(&db, "unapproved", "我住在隐港。", "lives_in");
    statement(&db, "outside", "我有一支笔。", "owns");
    let mut task_candidate = snapshot_candidate("task", "我住在任务港。");
    task_candidate.candidate.relation = "lives_in".into();
    statement_candidate(
        &db,
        "task",
        "我住在任务港。",
        task_candidate,
        Some("task-private".into()),
    );
    let other = statement(&db, "other", "小石住在南港。", "lives_in");
    db.open()
        .transaction(|s| {
            s.record_updates.get_mut(&pending).unwrap().status = UpdateStatus::Pending;
            s.record_updates.remove(&unknown);
            s.w5_receipts
                .retain(|_, r| !r.products.values().any(|id| *id == legacy));
            s.sources
                .get_mut("unapproved")
                .unwrap()
                .request
                .as_mut()
                .unwrap()["capture_source"] = json!(false);
            let mut m = s.repository.get(other)?.unwrap();
            m.subject_id = EntityId::from("someone-else");
            s.repository.update(m)?;
            let mut m = s.repository.get(expired)?.unwrap();
            m.retention_tier = RetentionTier::ShortTerm;
            m.temporal.expires_at = Some(Timestamp(1));
            s.repository.update(m)?;
            s.relations.insert(
                "open".into(),
                StoredRelation {
                    id: "open".into(),
                    kind: hippocampus::RelationKind::Conflict,
                    members: vec![conflict, pending],
                    conditions: json!([]),
                    times: json!([]),
                    basis: vec![],
                    status: hippocampus::RelationStatus::Open,
                    resolution: None,
                    created_by: "fixture".into(),
                    closed_by: None,
                },
            );
            Ok(())
        })
        .unwrap();
    let mut service = snapshot_service(&db, true);
    let out = service
        .call("memory_recall", boot("mechanical"), || false)
        .unwrap();
    assert_eq!(snapshot_ids(&out), [good.0]);
    assert_eq!(out["statement_snapshot"]["coverage_unknown"], true);
    assert_eq!(out["facts"], json!([]));
    assert_eq!(out["semantic_judgment"], "skipped");
    assert_no_calls(&db);
}

#[test]
fn af283_preferences_and_nullable_statement_time_are_kept_without_inventing_health_relations() {
    let db = Db::new();
    let mut preference = snapshot_candidate("preference", "我喜欢手工。");
    preference.candidate.relation = "likes".into();
    preference.candidate.memory_type = "preference".into();
    preference.said_on = None;
    let preference = statement_candidate(&db, "preference", "我喜欢手工。", preference, None);
    let allergy = statement(&db, "allergy", "我对花粉过敏。", "allergic_to");
    statement(
        &db,
        "not-preference",
        "这是一句回答要求。",
        "response_style",
    );
    statement(&db, "health", "我有某种健康情况。", "health");
    let out = snapshot_service(&db, true)
        .call("memory_recall", boot("types"), || false)
        .unwrap();
    let ids = snapshot_ids(&out);
    assert_eq!(ids.len(), 2);
    assert!(ids.contains(&allergy.0) && ids.contains(&preference.0));
    let rows = out["statement_snapshot"]["records"].as_array().unwrap();
    assert!(rows.iter().find(|r| r["id"] == preference.0).unwrap()["said_on"].is_null());
    assert_no_calls(&db);
}

#[test]
fn af283_plans_negation_unknown_times_and_coexisting_values_remain_full_statements() {
    let db = Db::new();
    let raw = "周末如果晴天，我可能打算住在星港。";
    let mut plan = snapshot_candidate("plan", raw);
    plan.candidate.relation = "lives_in".into();
    plan.candidate.speech = Speech::Plan;
    plan.candidate.conditions = vec![TextQualifier {
        quote: Quote {
            text: "如果晴天".into(),
        },
        resolved: false,
    }];
    plan.candidate.uncertainty = Some(TextQualifier {
        quote: Quote {
            text: "可能".into(),
        },
        resolved: true,
    });
    plan.candidate.times =
        vec![serde_json::from_value(json!({
        "role":"event","quote":{"text":"周末"},"anchor":"said_on","interp":{"kind":"unknown"}
    })).unwrap()];
    plan.time_resolutions = vec![TimeResolution {
        status: ResolutionStatus::Unknown,
        start: None,
        end_exclusive: None,
        precision: Precision::Unknown,
    }];
    let expected = plan.candidate.clone();
    let planned = statement_candidate(&db, "plan", raw, plan, None);
    let mut negative = snapshot_candidate("negative", "我不在木屋工作。");
    negative.candidate.relation = "works_at".into();
    negative.candidate.polarity = hippocampus::contract::Polarity::Negate;
    let negative = statement_candidate(&db, "negative", "我不在木屋工作。", negative, None);
    let first = statement(&db, "first", "我住在东港。", "lives_in");
    let second = statement(&db, "second", "我住在西港。", "lives_in");
    let mut service = snapshot_service(&db, true);
    let out = service
        .call("memory_recall", boot("qualifiers"), || false)
        .unwrap();
    let ids = snapshot_ids(&out);
    for id in [planned, negative, first, second] {
        assert!(ids.contains(&id.0));
    }
    let rows = out["statement_snapshot"]["records"].as_array().unwrap();
    let stored = rows.iter().find(|r| r["id"] == planned.0).unwrap();
    assert_eq!(stored["candidate"], serde_json::to_value(expected).unwrap());
    assert_eq!(stored["said_on"], "2026-09-24");
    assert_eq!(
        rows.iter().find(|r| r["id"] == negative.0).unwrap()["candidate"]["polarity"],
        "negate"
    );
    assert_eq!(out["facts"], json!([]));
    assert!(
        rows.iter()
            .all(|r| r.get("current").is_none() && r.get("applies").is_none())
    );
    assert_no_calls(&db);
}

#[test]
fn af283_unfinished_count_is_scope_wide_not_query_related_or_complete() {
    let db = Db::new();
    statement(&db, "good", "我住在星港。", "lives_in");
    for (id, status) in [
        ("queued", "queued"),
        ("failed", "failed"),
        ("gaps", "coverage_gaps"),
        ("empty", "no_candidates_unreviewed"),
        ("unknown", "future-status"),
        ("done", "completed_no_facts"),
    ] {
        claim_source(&db, id, "此来源无需词法命中。");
        db.open()
            .transaction(|s| {
                s.sources.get_mut(id).unwrap().enrichment = status.into();
                Ok(())
            })
            .unwrap();
    }
    claim_source(&db, "private-task", "任务私有原话。");
    db.open()
        .transaction(|s| {
            s.sources
                .get_mut("private-task")
                .unwrap()
                .request
                .as_mut()
                .unwrap()["task_id"] = json!("private");
            Ok(())
        })
        .unwrap();
    let out = snapshot_service(&db, true)
        .call("memory_recall", boot("unfinished"), || false)
        .unwrap();
    assert_eq!(out["statement_snapshot"]["unfinished_sources"], 5);
    assert_eq!(out["statement_snapshot"]["coverage_unknown"], true);
    assert_eq!(out["completeness"]["pending_related_sources"], 0);
    assert!(
        !serde_json::to_string(&out)
            .unwrap()
            .contains("任务私有原话")
    );
    assert_no_calls(&db);
}

#[test]
fn af283_snapshot_caps_keep_whole_records_and_report_truncation() {
    let db = Db::new();
    statement(&db, "one", "我住在东港。", "lives_in");
    statement(&db, "two", "我住在西港。", "lives_in");
    for (id, records, bytes, want) in [("count", 1, 16384, 1), ("bytes", 8, 512, 0)] {
        let mut config = hippocampus::chat::ChatConfig::default();
        config.statement_snapshot = StatementSnapshotConfig {
            enabled: true,
            max_records: records,
            max_bytes: bytes,
        };
        let mut service = hippocampus::chat::ChatMemoryService::new(db.open(), config).unwrap();
        let out = service.call("memory_recall", boot(id), || false).unwrap();
        assert_eq!(snapshot_ids(&out).len(), want);
        assert_eq!(out["statement_snapshot"]["truncated"], true);
        assert!(
            serde_json::to_vec(&out["statement_snapshot"])
                .unwrap()
                .len()
                <= bytes
        );
    }
    assert_no_calls(&db);
}

#[test]
fn af283_replay_refresh_reopen_and_opt_in_policy_have_distinct_bindings() {
    let db = Db::new();
    let first = statement(&db, "one", "我住在东港。", "lives_in");
    let mut service = snapshot_service(&db, true);
    let before = service
        .call("memory_recall", boot("replay"), || false)
        .unwrap();
    statement(&db, "two", "我住在西港。", "lives_in");
    assert_eq!(
        service
            .call("memory_recall", boot("replay"), || false)
            .unwrap(),
        before
    );
    let fresh = service
        .call("memory_recall", boot("fresh"), || false)
        .unwrap();
    assert_eq!(snapshot_ids(&fresh).len(), 2);
    assert_eq!(snapshot_ids(&before), [first.0]);
    assert!(
        fresh["statement_snapshot"]["snapshot"]["read_revision"]
            .as_u64()
            .unwrap()
            > before["statement_snapshot"]["snapshot"]["read_revision"]
                .as_u64()
                .unwrap()
    );
    assert!(
        snapshot_service(&db, false)
            .call("memory_recall", boot("replay"), || false)
            .unwrap_err()
            .to_string()
            .contains("recall_request_mismatch")
    );
    assert!(
        snapshot_service(&db, true)
            .call("memory_recall", boot("replay"), || false)
            .unwrap_err()
            .to_string()
            .contains("recall_result_not_retained")
    );
    assert_eq!(
        snapshot_ids(
            &snapshot_service(&db, true)
                .call("memory_recall", boot("reopened"), || false)
                .unwrap()
        )
        .len(),
        2
    );
    assert_no_calls(&db);
}

#[test]
fn af283_deletion_before_bootstrap_replay_and_publication_never_returns_the_old_statement() {
    let db = Db::new();
    statement(&db, "private", "我住在唯一的隐港。", "lives_in");
    let mut service = snapshot_service(&db, true);
    let response = service.call_with_global_context("memory_recall", boot("delete"), || false);
    assert!(
        serde_json::to_string(&response)
            .unwrap()
            .contains("唯一的隐港")
    );
    service
        .call("memory_forget", json!({"source_id":"private"}), || false)
        .unwrap();
    let published = service.publish_response(response, |r| r).unwrap();
    assert!(published.is_error.unwrap_or(false));
    assert!(
        !serde_json::to_string(&published)
            .unwrap()
            .contains("唯一的隐港")
    );
    assert!(
        service
            .call("memory_recall", boot("delete"), || false)
            .is_err()
    );
    let fresh = service
        .call("memory_recall", boot("after-delete"), || false)
        .unwrap();
    assert!(snapshot_ids(&fresh).is_empty());
    assert!(
        !serde_json::to_string(&fresh)
            .unwrap()
            .contains("唯一的隐港")
    );
    assert_no_calls(&db);
}

#[test]
fn af283_v8_is_rejected_by_bootstrap_prepare_and_validate_without_side_effects() {
    let db = Db::new();
    let mut service = snapshot_service(&db, true);
    for (name, mut input) in [
        ("memory_recall", boot("unsupported")),
        (
            "memory_prepare",
            json!({"user_turn":{"text":"我住在星港。","session_id":"s","turn_id":"t","capture_source":true}}),
        ),
        (
            "memory_validate_answer",
            json!({"query":"住哪？","request_id":"unsupported","draft_answer":"星港"}),
        ),
    ] {
        input["recall_version"] = json!("recall.v8");
        assert!(
            service
                .call(name, input, || false)
                .unwrap_err()
                .to_string()
                .contains("contract_version_unsupported")
        );
    }
    assert!(db.state().sources.is_empty());
    assert_no_calls(&db);
}
