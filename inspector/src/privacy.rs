//! AF375: filter before parsing/aggregation. Text equality never substitutes dependencies.
use serde_json::{Value, json};
use std::path::Path;

/// Union call/attempt/run identities before filtering, so a later event cannot
/// replace an earlier origin. Carried origins survive deletion of the start file.
pub(super) struct Origins(std::collections::BTreeMap<String, Option<i64>>);
impl Origins {
    fn keys(row: &Value) -> Vec<String> {
        ["call_id", "attempt_id", "trace_id"]
            .iter()
            .filter_map(|key| {
                row["meta"][key]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(|id| format!("{key}:{id}"))
            })
            .collect()
    }
    pub fn collect(rows: &[Value]) -> Self {
        use std::collections::{BTreeMap, BTreeSet};
        let mut edges = BTreeMap::<String, BTreeSet<String>>::new();
        let mut times = BTreeMap::<String, i64>::new();
        for row in rows.iter().filter(|r| r["trace_version"] == "2") {
            let keys = Self::keys(row);
            let carried = row["meta"]["origin_at_unix_ns"].as_i64().filter(|n| *n > 0);
            let start = matches!(row["kind"].as_str(), Some("model_start" | "run_start"))
                .then(|| row["meta"]["written_at_unix_ns"].as_i64())
                .flatten()
                .filter(|n| *n > 0);
            let origin = carried.into_iter().chain(start).min();
            for key in &keys {
                edges
                    .entry(key.clone())
                    .or_default()
                    .extend(keys.iter().filter(|k| *k != key).cloned());
                if let Some(t) = origin {
                    times
                        .entry(key.clone())
                        .and_modify(|old| *old = (*old).min(t))
                        .or_insert(t);
                }
            }
        }
        let mut out = BTreeMap::new();
        for key in edges.keys() {
            if out.contains_key(key) {
                continue;
            }
            let mut pending = vec![key.clone()];
            let mut component = BTreeSet::new();
            let mut origin = None;
            while let Some(next) = pending.pop() {
                if !component.insert(next.clone()) {
                    continue;
                }
                if let Some(t) = times.get(&next) {
                    origin = Some(origin.map_or(*t, |old: i64| old.min(*t)));
                }
                pending.extend(
                    edges[&next]
                        .iter()
                        .filter(|k| !component.contains(*k))
                        .cloned(),
                );
            }
            for key in component {
                out.insert(key, origin);
            }
        }
        Self(out)
    }
    pub fn get(&self, row: &Value) -> Option<i64> {
        Self::keys(row)
            .iter()
            .filter_map(|k| self.0.get(k).copied().flatten())
            .min()
    }
}

pub(super) struct Policy<'a> {
    state: &'a Value,
    owner: &'a str,
    epoch: Option<Value>,
    blocked: bool,
}
impl<'a> Policy<'a> {
    pub fn new(state: &'a Value, owner: &'a str, dir: &Path) -> Self {
        let deleted: Vec<String> = state["sources"]
            .as_object()
            .into_iter()
            .flat_map(|m| m.values())
            .filter(|s| s["request"].is_null() || s["enrichment"] == "deleted")
            .filter_map(|s| s["id"].as_str().map(str::to_owned))
            .collect();
        let read = std::fs::read(dir.join("deletion-epoch.json"));
        let corrupt = read
            .as_ref()
            .is_err_and(|e| e.kind() != std::io::ErrorKind::NotFound);
        let present = read.is_ok();
        let epoch = read
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
        let valid = epoch.as_ref().is_some_and(|v| {
            v["version"] == 1 && v["epoch_id"].is_string() && v["written_at_unix_ns"].is_i64()
        });
        let covered = deleted.is_empty()
            || epoch.as_ref().is_some_and(|v| {
                v["owners"][owner] == hippocampus::diagnostics::deletion_signature(deleted)
            });
        Self {
            state,
            owner,
            blocked: corrupt
                || (present && !valid)
                || !covered
                || dir.join("deletion-pending").exists(),
            epoch,
        }
    }
    fn source_valid(&self, id: &str) -> bool {
        self.state["sources"].as_object().is_some_and(|m| {
            m.values()
                .any(|s| s["id"] == id && !s["request"].is_null() && s["enrichment"] != "deleted")
        })
    }
    pub fn summary(&self) -> Value {
        let mut out = json!({"status":if self.blocked {"unconfirmed"} else if self.epoch.is_some() {"confirmed"} else {"none"}});
        if let Some(epoch) = &self.epoch {
            for key in ["revision", "written_at_unix_ns", "cleanup_failures"] {
                if epoch[key].is_u64() {
                    out[key] = epoch[key].clone();
                }
            }
            if matches!(
                epoch["cleanup_status"].as_str(),
                Some("complete" | "partial")
            ) {
                out["cleanup_status"] = epoch["cleanup_status"].clone();
            }
        }
        out
    }
    pub fn foreign_owner(&self, row: &Value) -> bool {
        let body = if row["trace_version"] == "2" {
            &row["body"]
        } else {
            &row["data"]
        };
        body["owner"].as_str().is_some_and(|o| o != self.owner)
            || ["sources", "memories"].iter().any(|kind| {
                row["content_deps"][kind].as_object().is_some_and(|m| {
                    m.values()
                        .any(|d| d["owner"].as_str().is_some_and(|o| o != self.owner))
                })
            })
    }
    #[cfg(test)]
    pub fn reason(&self, row: &Value) -> Option<&'static str> {
        self.reason_with_origin(row, row["meta"]["origin_at_unix_ns"].as_i64())
    }
    pub fn reason_with_origin(&self, row: &Value, origin: Option<i64>) -> Option<&'static str> {
        if row["trace_version"] != "2" {
            return Some("legacy_or_unknown_version");
        }
        if self.blocked {
            return Some("deletion_epoch_unconfirmed");
        }
        let Some(written) = row["meta"]["written_at_unix_ns"].as_i64() else {
            return Some("write_time_unknown");
        };
        let Some(origin) = origin.filter(|n| *n > 0 && *n <= written) else {
            return Some("origin_missing_or_invalid");
        };
        if let Some(epoch) = &self.epoch {
            if origin <= epoch["written_at_unix_ns"].as_i64().unwrap() {
                return Some("origin_before_deletion_epoch");
            }
            if written <= epoch["written_at_unix_ns"].as_i64().unwrap()
                || row["meta"]["deletion_epoch"] != epoch["epoch_id"]
            {
                return Some("origin_before_deletion_epoch");
            }
        }
        if row["body"]["owner"]
            .as_str()
            .is_some_and(|v| v != self.owner)
        {
            return Some("owner_mismatch");
        }
        let deps = &row["content_deps"];
        if deps["reasons"].as_array().is_some_and(|r| {
            r.iter().any(|v| {
                matches!(
                    v.as_str(),
                    Some("source_unavailable" | "record_unavailable")
                )
            })
        }) {
            return Some("dependency_unavailable");
        }
        let (Some(sources), Some(memories)) =
            (deps["sources"].as_object(), deps["memories"].as_object())
        else {
            return Some("dependency_shape_unknown");
        };
        if let Some(id) = row["meta"]["source_id"].as_str() {
            if !self.source_valid(id) {
                return Some("source_deleted_or_missing");
            }
        }
        for (id, dep) in sources {
            if dep["owner"] != self.owner || dep["source_id"] != *id || !self.source_valid(id) {
                return Some("source_deleted_or_owner_mismatch");
            }
        }
        for (id, dep) in memories {
            let record = &self.state["repository"]["memories"][id];
            if dep["owner"] != self.owner
                || dep["memory_id"].as_u64().map(|n| n.to_string()).as_deref() != Some(id.as_str())
                || record["owner_id"] != self.owner
                || record["status"] == "Deleted"
                || !record.is_object()
            {
                return Some("record_deleted_or_owner_mismatch");
            }
        }
        None
    }
}

/// No host IDs, free-form errors, arbitrary maps or content dependencies survive here.
pub(super) fn metadata(row: &Value, reason: &str) -> Value {
    let kind = row["kind"].as_str().unwrap_or("");
    let known = [
        "model_start",
        "model_end",
        "model_check",
        "model_transport_error",
        "run_start",
        "run_end",
        "rpc_response",
        "validation",
        "transaction_committed",
        "transaction_rollback",
        "source_receipt",
        "global_context",
        "retrieval",
        "w2_summary",
        "w3_skipped",
        "w4_plan",
        "w5_commit",
        "r3_evidence",
        "r5_final",
    ];
    let mut out = json!({"kind":if known.contains(&kind){kind}else{"unknown"},"status":"privacy_filtered","reason":reason});
    let data = if row["trace_version"] == "2" {
        &row["body"]
    } else {
        &row["data"]
    };
    for key in ["duration_ms", "elapsed_ms", "timeout_ms"] {
        if let Some(n) = row["meta"][key].as_u64().or_else(|| data[key].as_u64()) {
            out[key] = json!(n);
        }
    }
    for key in [
        "input_tokens",
        "output_tokens",
        "cached_input_tokens",
        "reasoning_tokens",
    ] {
        if let Some(n) = data["accounting"]["usage"][key]
            .as_u64()
            .or_else(|| data["accounting"][key].as_u64())
            .or_else(|| data["usage"][key].as_u64())
        {
            out["usage"][key] = json!(n);
        }
    }
    // These values are configuration/protocol tokens, never arbitrary diagnostic text.
    for key in ["stage", "model", "local_outcome"] {
        let value = row["meta"].get(key).or_else(|| data.get(key)).or_else(|| {
            if key == "model" {
                data["request"]
                    .get("model")
                    .or_else(|| data["response"].get("model"))
            } else {
                None
            }
        });
        if let Some(s) = value.and_then(Value::as_str).filter(|s| {
            !s.is_empty()
                && s.len() <= 96
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-.:/".contains(&b))
        }) {
            out[key] = json!(s);
        }
    }
    out
}

/// Summary events have a fixed shape even when the epoch allows their bodies.
pub(super) fn summary(kind: &str, data: &Value) -> Option<Value> {
    let fields: &[&str] = match kind {
        "retrieval" if !data["result"].is_object() && !data["runtime"].is_object() => &[
            "phase",
            "status",
            "retrieval",
            "path",
            "plan_complete",
            "trusted_records",
            "subject_records",
            "subject_mentions",
            "records",
            "raw",
            "truncated",
            "query_error",
            "index_state",
            "query_elapsed_ms",
            "cache_elapsed_ms",
            "sources_total",
            "sources_indexed",
            "segments_total",
            "segments_indexed",
        ],
        "w3_skipped" => &["reason", "candidates"],
        "w4_plan" => &[
            "groups",
            "held",
            "not_admitted",
            "model_required",
            "model_called",
            "reason",
            "decisions",
        ],
        "w2_summary" => &["total", "admitted", "unresolved", "rejected", "reasons"],
        "w5_commit" => &[
            "group_fingerprint",
            "decision_types",
            "product_ids",
            "ids_truncated",
            "revision",
        ],
        "r3_evidence" => &[
            "records",
            "raw",
            "record_ids",
            "source_ids",
            "ids_truncated",
            "id_limit",
            "completeness",
        ],
        "r5_final" => &["status", "facts", "computed", "records", "raw"],
        _ => return None,
    };
    Some(Value::Object(
        fields
            .iter()
            .filter_map(|k| data.get(*k).map(|v| ((*k).to_owned(), v.clone())))
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn state() -> Value {
        json!({"sources":{"a":{"id":"live","request":{"text":"fictional"}},"b":{"id":"gone","request":null}},"repository":{"memories":{"1":{"owner_id":"owner","status":"Active"},"2":{"owner_id":"owner","status":"Deleted"},"3":{"owner_id":"other","status":"Active"}}}})
    }
    fn row() -> Value {
        json!({"trace_version":"2","kind":"model_check","event_id":"internal","at":"2026-10-16T00:00:01Z","meta":{"trace_id":"trace-internal","written_at_unix_ns":101,"origin_at_unix_ns":101,"deletion_epoch":"epoch"},"content_deps":{"sources":{},"memories":{},"deps_complete":false,"reasons":["unbound_history"]},"body":{"stage":"w3_v6","phase":"validate","status":"completed","output":{"text":"surviving-body"}}})
    }
    fn epoch(dir: &Path) {
        std::fs::write(dir.join("deletion-epoch.json"),json!({"version":1,"epoch_id":"epoch","written_at_unix_ns":100,"revision":12,"owners":{"owner":hippocampus::diagnostics::deletion_signature(vec!["gone".into()])}}).to_string()).unwrap();
    }
    #[test]
    fn af378_runtime_initialized_epoch_unblocks_only_new_events() {
        let dir = std::env::temp_dir().join(format!(
            "af378-inspector-{}",
            hippocampus::diagnostics::new_id()
        ));
        std::fs::create_dir(&dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut state = serde_json::to_value(hippocampus::PersistedMemoryState::default()).unwrap();
        state["sources"]["gone"] = json!({"id":"gone","session_id":"fixture-session","turn_id":"fixture-turn","observed_at":1,"request":null,"enrichment":"deleted","error":null,"result":null,"base_revision":0,"index":{},"task":{}});
        let db = dir.join("fixture.sqlite");
        let connection = rusqlite::Connection::open(&db).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE memory_profiles(owner_id TEXT PRIMARY KEY,state_json TEXT NOT NULL)",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO memory_profiles VALUES('owner',?)",
                [state.to_string()],
            )
            .unwrap();
        drop(connection);
        assert!(Policy::new(&state, "owner", &dir).reason(&row()).is_some());
        hippocampus::diagnostics::init_trace_epoch(&db, "owner", &dir).unwrap();
        let epoch: Value =
            serde_json::from_slice(&std::fs::read(dir.join("deletion-epoch.json")).unwrap())
                .unwrap();
        let at = epoch["written_at_unix_ns"].as_i64().unwrap();
        let mut event = row();
        event["meta"]["written_at_unix_ns"] = json!(at + 2);
        event["meta"]["origin_at_unix_ns"] = json!(at + 1);
        event["meta"]["deletion_epoch"] = epoch["epoch_id"].clone();
        let policy = Policy::new(&state, "owner", &dir);
        assert_eq!(policy.reason(&event), None);
        let path = dir.join("trace-new.jsonl");
        std::fs::write(&path, format!("{event}\n")).unwrap();
        assert!(
            crate::trace::export_filtered(&path, &policy)
                .unwrap()
                .to_string()
                .contains("surviving-body")
        );
        event["content_deps"]["sources"]["gone"] = json!({"source_id":"gone","owner":"owner"});
        assert!(policy.reason(&event).is_some());
        event["content_deps"]["sources"] = json!({});
        event["meta"]["origin_at_unix_ns"] = json!(at - 1);
        assert_eq!(policy.reason(&event), Some("origin_before_deletion_epoch"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn af376_call_and_run_origins_gate_all_outputs() {
        let dir =
            std::env::temp_dir().join(format!("af376-{}", hippocampus::diagnostics::new_id()));
        std::fs::create_dir(&dir).unwrap();
        epoch(&dir);
        let state = state();
        let policy = Policy::new(&state, "owner", &dir);
        for kind in [
            "model_end",
            "model_check",
            "w2_summary",
            "w5_commit",
            "r3_evidence",
            "r5_final",
            "run_end",
        ] {
            let mut end = row();
            end["kind"] = json!(kind);
            end["meta"]["written_at_unix_ns"] = json!(120);
            end["meta"]["origin_at_unix_ns"] = Value::Null;
            let origins = Origins::collect(&[end.clone()]);
            assert_eq!(
                policy.reason_with_origin(&end, origins.get(&end)),
                Some("origin_missing_or_invalid"),
                "{kind}"
            );
            // Start file was deleted: carried origin remains sufficient to hide.
            end["meta"]["origin_at_unix_ns"] = json!(90);
            let origins = Origins::collect(&[end.clone()]);
            assert_eq!(
                policy.reason_with_origin(&end, origins.get(&end)),
                Some("origin_before_deletion_epoch"),
                "{kind}"
            );
            end["meta"]["origin_at_unix_ns"] = json!(110);
            let origins = Origins::collect(&[end.clone()]);
            assert_eq!(
                policy.reason_with_origin(&end, origins.get(&end)),
                None,
                "{kind}"
            );
        }
        for start_kind in ["model_start", "run_start"] {
            let mut start = row();
            start["kind"] = json!(start_kind);
            start["meta"]["origin_at_unix_ns"] = Value::Null;
            start["meta"]["written_at_unix_ns"] = json!(110);
            let mut end = start.clone();
            end["kind"] = json!("run_end");
            end["meta"]["written_at_unix_ns"] = json!(120);
            let origins = Origins::collect(&[end.clone(), start.clone()]);
            assert_eq!(policy.reason_with_origin(&end, origins.get(&end)), None);
            start["meta"]["written_at_unix_ns"] = json!(90);
            let origins = Origins::collect(&[end.clone(), start]);
            assert_eq!(
                policy.reason_with_origin(&end, origins.get(&end)),
                Some("origin_before_deletion_epoch")
            );
        }
        // A retry/new trace sharing an attempt cannot replace its earliest origin.
        let mut first = row();
        first["meta"]["attempt_id"] = json!("attempt");
        first["meta"]["origin_at_unix_ns"] = json!(90);
        let mut retry = row();
        retry["meta"]["trace_id"] = json!("retry");
        retry["meta"]["attempt_id"] = json!("attempt");
        let origins = Origins::collect(&[first, retry.clone()]);
        assert_eq!(
            policy.reason_with_origin(&retry, origins.get(&retry)),
            Some("origin_before_deletion_epoch")
        );
        // Exercise the shared export boundary, not only the policy predicate.
        let mut early = row();
        early["meta"]["origin_at_unix_ns"] = json!(90);
        early["body"]["output"] = json!("inflight-secret");
        let mut orphan = row();
        orphan["meta"]["trace_id"] = json!("orphan");
        orphan["meta"]["origin_at_unix_ns"] = Value::Null;
        orphan["body"]["output"] = json!("orphan-secret");
        let mut fresh = row();
        fresh["meta"]["trace_id"] = json!("fresh");
        let path = dir.join("trace-boundary.jsonl");
        std::fs::write(&path, format!("{early}\n{orphan}\n{fresh}\n")).unwrap();
        let bundle = crate::trace::export_filtered(&path, &policy).unwrap();
        let exported = bundle.to_string();
        assert!(!exported.contains("inflight-secret"));
        assert!(!exported.contains("orphan-secret"));
        assert!(exported.contains("surviving-body"));
        assert_eq!(
            bundle["meta"]["filtered_events"].as_array().unwrap().len(),
            2
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn af375_epoch_time_dependencies_and_failed_epoch_are_fail_closed() {
        let dir = std::env::temp_dir().join(format!(
            "af375-policy-{}",
            hippocampus::diagnostics::new_id()
        ));
        std::fs::create_dir(&dir).unwrap();
        let state = state();
        assert_eq!(
            Policy::new(&state, "owner", &dir).reason(&row()),
            Some("deletion_epoch_unconfirmed")
        );
        epoch(&dir);
        let policy = Policy::new(&state, "owner", &dir);
        assert_eq!(
            policy.reason(&row()),
            None,
            "incomplete history allowed after epoch by AF373"
        );
        let mut old = row();
        old["meta"]["written_at_unix_ns"] = json!(99);
        old["meta"]["origin_at_unix_ns"] = json!(99);
        assert_eq!(policy.reason(&old), Some("origin_before_deletion_epoch"));
        old["meta"]
            .as_object_mut()
            .unwrap()
            .remove("written_at_unix_ns");
        assert_eq!(policy.reason(&old), Some("write_time_unknown"));
        for version in ["1", "3"] {
            old["trace_version"] = json!(version);
            assert_eq!(policy.reason(&old), Some("legacy_or_unknown_version"));
        }
        for (id, owner) in [("gone", "owner"), ("live", "other"), ("missing", "owner")] {
            let mut r = row();
            r["content_deps"]["sources"][id] = json!({"source_id":id,"owner":owner});
            assert!(policy.reason(&r).is_some());
        }
        for id in ["2", "3", "4"] {
            let mut r = row();
            r["content_deps"]["memories"][id] = json!({"memory_id":id,"owner":"owner"});
            assert!(policy.reason(&r).is_some());
        }
        let mut changed = state.clone();
        changed["sources"]["a"]["request"] = Value::Null;
        assert_eq!(
            Policy::new(&changed, "owner", &dir).reason(&row()),
            Some("deletion_epoch_unconfirmed")
        );
        std::fs::write(dir.join("deletion-pending"), b"").unwrap();
        assert!(Policy::new(&state, "owner", &dir).reason(&row()).is_some());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn af375_filter_precedes_parser_aggregation_and_preserves_summaries() {
        let dir = std::env::temp_dir().join(format!(
            "af375-filter-{}",
            hippocampus::diagnostics::new_id()
        ));
        std::fs::create_dir(&dir).unwrap();
        epoch(&dir);
        let state = state();
        let policy = Policy::new(&state, "owner", &dir);
        let path = dir.join("trace-test.jsonl");
        let mut before = row();
        before["meta"]["written_at_unix_ns"] = json!(99);
        before["meta"]["origin_at_unix_ns"] = json!(99);
        before["meta"]["trace_id"] = json!("old-trace");
        before["body"] =
            json!({"host":"deleted-secret","session_id":"deleted-topic","error":"deleted-secret"});
        let mut summary = row();
        summary["event_id"] = json!("summary");
        summary["kind"] = json!("w2_summary");
        summary["body"] =
            json!({"total":3,"admitted":1,"rejected":2,"not_a_summary":"unexpected-text"});
        std::fs::write(&path, format!("{before}\n{}\n{summary}\n", row())).unwrap();
        let bundle = crate::trace::export_filtered(&path, &policy).unwrap();
        let text = bundle.to_string();
        assert!(
            !text.contains("deleted-secret")
                && !text.contains("deleted-topic")
                && !text.contains("unexpected-text")
        );
        assert!(text.contains("surviving-body"));
        assert_eq!(
            bundle["meta"]["filtered_events"].as_array().unwrap().len(),
            1
        );
        assert!(
            bundle["runs"][0]["spans"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s["name"] == "w2_summary" && s["output"]["total"] == 3)
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
