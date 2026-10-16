//! Pure adaptation of a query_only snapshot. No store opening, migration or provider.
use hippocampus::{MemoryId, PersistedMemoryState};
use serde_json::{Value, json};

pub(super) fn project(state: &Value) -> Result<Value, &'static str> {
    let typed: PersistedMemoryState = serde_json::from_value(state.clone()).map_err(
        |_| "current_view_unavailable: 快照无法按当前持久结构解析；完整候选与更新状态未知",
    )?;
    let mut records = serde_json::Map::new();
    for key in state["repository"]["memories"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(k, _)| k)
    {
        let id = MemoryId(
            key.parse()
                .map_err(|_| "current_view_unavailable: 记录编号无效")?,
        );
        if let Some(view) = hippocampus::w4::record_view(&typed, id) {
            let relations: Vec<_> = typed
                .relations
                .values()
                .filter(|r| r.members.contains(&id))
                .collect();
            records.insert(key.clone(), json!({"record":view,"relations":relations}));
        }
    }
    let sources: serde_json::Map<String, Value> = typed
        .sources
        .iter()
        .map(|(key, source)| {
            let receipt = hippocampus::enrichment::receipt_from_state(&typed, key).unwrap_or_else(
                |_| json!({"status":"unknown","note":"候选回执不可核验，不推断提交状态"}),
            );
            (source.id.clone(), receipt)
        })
        .collect();
    Ok(json!({"records":records,"sources":sources}))
}

pub(super) fn workflow(source: &Value) -> Value {
    let result = &source["result"];
    json!({
        "status": if result.is_object() {"available"} else {"not_captured"},
        "note":"当前留存检查点；不是完整历史执行轨迹。缺失字段表示未记录，不表示通过。",
        "w2":result["w4_batch"]["batch"]["w2"],
        "w3":result["w4_batch"]["batch"]["review"]["output"],
        "w3_recorded_output":result["w4_model_stages"]["w3"]["output"],
        "admission":result["w4_input_meta"],
        "checkpoint": {"candidates":result["w4_checkpoint"]["candidates"],"plan":result["w4_checkpoint"]["plan"],"invalidated":result["w4_checkpoint"]["invalidated"]},
        "summary":result["w4_summary"],
        "result":result.get("outcomes"),
    })
}

pub(super) fn hidden() -> Value {
    json!({"status":"privacy_filtered","reason":"依赖已删除或不可确认"})
}

/// Check every dependency, not just the subset surviving record_view's projection.
pub(super) struct Visibility<'a> {
    state: &'a Value,
    owner: &'a str,
    typed: Option<PersistedMemoryState>,
}
impl<'a> Visibility<'a> {
    pub(super) fn new(state: &'a Value, owner: &'a str) -> Self {
        Self {
            state,
            owner,
            typed: serde_json::from_value(state.clone()).ok(),
        }
    }
    fn source(&self, sid: &str) -> Option<&Value> {
        let mut found = self.state["sources"]
            .as_object()?
            .values()
            .filter(|s| s["id"] == sid);
        let source = found.next()?;
        if found.next().is_some() || !source["request"]["text"].is_string() {
            return None;
        }
        // Sources belong to the owner-selected memory_profiles row.
        Some(source)
    }
    fn quote(&self, sid: &str, text: &str) -> bool {
        !text.is_empty()
            && self
                .source(sid)
                .and_then(|s| s["request"]["text"].as_str())
                .is_some_and(|body| body.contains(text))
    }
    pub(super) fn record(&self, id: &str) -> bool {
        self.record_inner(id, &mut std::collections::BTreeSet::new())
    }
    fn record_inner(&self, id: &str, seen: &mut std::collections::BTreeSet<String>) -> bool {
        if !seen.insert(id.into()) {
            return true;
        }
        let m = &self.state["repository"]["memories"][id];
        if m["owner_id"] != self.owner || m["status"] == "Deleted" {
            return false;
        }
        let Some(evidence) = m["evidence"].as_array().filter(|e| !e.is_empty()) else {
            return false;
        };
        for e in evidence {
            let Some(all) = self.state["sources"].as_object() else {
                return false;
            };
            let matches: Vec<_> = all
                .values()
                .filter(|s| s["session_id"] == e["session_id"] && s["turn_id"] == e["turn_id"])
                .collect();
            if matches.len() != 1
                || !matches[0]["id"]
                    .as_str()
                    .zip(e["quote"].as_str())
                    .is_some_and(|(sid, q)| self.quote(sid, q))
            {
                return false;
            }
        }
        for key in ["supersedes", "superseded_by"] {
            if !m[key].is_null()
                && !m[key]
                    .as_u64()
                    .is_some_and(|n| self.record_inner(&n.to_string(), seen))
            {
                return false;
            }
        }
        // Production history also includes correction/state-change parents from receipts.
        if let Some(typed) = &self.typed {
            if let Some(view) = id
                .parse()
                .ok()
                .and_then(|n| hippocampus::w4::record_view(&typed, MemoryId(n)))
            {
                let mut view = serde_json::to_value(view).unwrap();
                // Legacy raw evidence was checked above before projection could drop it.
                view.as_object_mut().unwrap().remove("data");
                let contract =
                    hippocampus::w5_record_contract(&typed, MemoryId(id.parse().unwrap()));
                let binding = contract
                    .as_ref()
                    .map(|c| serde_json::to_value(&c.source).unwrap());
                let sid = binding.as_ref().and_then(|v| v["source_id"].as_str());
                if !self.refs(&view, sid, seen) {
                    return false;
                }
            } else {
                return false;
            }
        } else {
            return false;
        }
        self.state["relations"]
            .as_object()
            .into_iter()
            .flat_map(|r| r.values())
            .filter(|r| {
                r["members"].as_array().is_some_and(|members| {
                    members
                        .iter()
                        .any(|n| n.as_u64().map(|n| n.to_string()).as_deref() == Some(id))
                })
            })
            .all(|r| self.refs(r, None, seen))
    }
    fn refs(
        &self,
        v: &Value,
        local: Option<&str>,
        seen: &mut std::collections::BTreeSet<String>,
    ) -> bool {
        match v {
            Value::Array(a) => a.iter().all(|v| self.refs(v, local, seen)),
            Value::Object(o) => {
                let explicit = o
                    .get("source_id")
                    .and_then(Value::as_str)
                    .or_else(|| v["source"]["source_id"].as_str());
                if let Some(sid) = explicit {
                    if self.source(sid).is_none() {
                        return false;
                    }
                }
                if v["source"]["kind"] == "current" && local.is_none() {
                    return false;
                }
                if let Some(text) = o.get("text") {
                    if !text
                        .as_str()
                        .zip(explicit.or(local))
                        .is_some_and(|(q, s)| self.quote(s, q))
                    {
                        return false;
                    }
                }
                if let Some(q) = o.get("quote") {
                    let q = q.as_str().or_else(|| q["text"].as_str());
                    if !q
                        .zip(explicit.or(local))
                        .is_some_and(|(q, s)| self.quote(s, q))
                    {
                        return false;
                    }
                }
                if let Some(sentence) = o.get("sentence") {
                    let q = sentence.as_str().or_else(|| sentence["text"].as_str());
                    if !q
                        .zip(explicit.or(local))
                        .is_some_and(|(q, s)| self.quote(s, q))
                    {
                        return false;
                    }
                }
                if let Some(ids) = o.get("basis_sources") {
                    if !ids.as_array().is_some_and(|a| {
                        a.iter()
                            .all(|id| id.as_str().is_some_and(|sid| self.source(sid).is_some()))
                    }) {
                        return false;
                    }
                }
                for key in ["resolves_relations", "open_relations"] {
                    if let Some(ids) = o.get(key) {
                        if !ids.as_array().is_some_and(|a| {
                            a.iter().all(|id| {
                                id.as_str().is_some_and(|rid| {
                                    let r = &self.state["relations"][rid];
                                    r.is_object() && self.refs(r, None, seen)
                                })
                            })
                        }) {
                            return false;
                        }
                    }
                }
                for key in ["memory_id", "committed_id"] {
                    if let Some(id) = o.get(key).filter(|x| !x.is_null()) {
                        if !id
                            .as_u64()
                            .is_some_and(|id| self.record_inner(&id.to_string(), seen))
                        {
                            return false;
                        }
                    }
                }
                for key in ["history", "members", "targets"] {
                    if let Some(ids) = o.get(key) {
                        if !ids.as_array().is_some_and(|a| {
                            a.iter().all(|id| {
                                id.as_u64()
                                    .is_some_and(|n| self.record_inner(&n.to_string(), seen))
                            })
                        }) {
                            return false;
                        }
                    }
                }
                o.iter()
                    .filter(|(k, _)| {
                        !["quote", "sentence", "history", "members", "targets"]
                            .contains(&k.as_str())
                    })
                    .all(|(_, v)| self.refs(v, explicit.or(local), seen))
            }
            _ => true,
        }
    }
    pub(super) fn block(&self, v: Value, local: Option<&str>) -> Value {
        fn local_refs(v: &Value, sid: &str) -> bool {
            match v {
                Value::Object(o) => o.iter().all(|(k, v)| {
                    if k == "source_id" {
                        v.as_str() == Some(sid)
                    } else {
                        local_refs(v, sid)
                    }
                }),
                Value::Array(a) => a.iter().all(|v| local_refs(v, sid)),
                _ => true,
            }
        }
        if local.is_none_or(|sid| local_refs(&v, sid))
            && self.refs(&v, local, &mut Default::default())
        {
            v
        } else {
            hidden()
        }
    }
    pub(super) fn workflow(&self, source: &Value) -> Value {
        let sid = source["id"].as_str().unwrap_or("");
        let mut out = workflow(source);
        // Raw unvalidated output and binding snapshots can contain unlinked history.
        out["w3_recorded_output"] = hidden();
        out["admission"] = hidden();
        out["result"] = Value::Null;
        out["w2"] = serde_json::from_value::<hippocampus::w2::Batch>(out["w2"].clone())
            .ok()
            .map(|b| self.block(serde_json::to_value(b).unwrap(), Some(sid)))
            .unwrap_or_else(hidden);
        out["w3"] = serde_json::from_value::<hippocampus::contract::W3Output>(out["w3"].clone())
            .ok()
            .map(|b| self.block(serde_json::to_value(b).unwrap(), Some(sid)))
            .unwrap_or_else(hidden);
        out["w1"] = serde_json::from_value::<hippocampus::contract::W1Output>(
            source["result"]["w4_model_stages"]["w1"]["output"].clone(),
        )
        .ok()
        .filter(|w| {
            w.candidates.iter().enumerate().all(|(i, c)| {
                out["w2"]["candidates"].as_array().is_some_and(|a| {
                    a.iter().any(|r| {
                        r["index"] == i
                            && r["status"] != "rejected"
                            && r["candidate"] == serde_json::to_value(c).unwrap()
                    })
                })
            })
        })
        .map(|w| self.block(serde_json::to_value(w).unwrap(), Some(sid)))
        .unwrap_or_else(hidden);
        let checkpoint = &source["result"]["w4_checkpoint"];
        let plan = &checkpoint["plan"];
        let groups: Vec<_> = plan["groups"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|g| {
                let decisions =
                    serde_json::from_value::<Vec<hippocampus::W5Decision>>(g["decisions"].clone());
                match decisions {
                    Ok(d) => {
                        let v = json!({"id":g["id"],"decisions":d});
                        if self.refs(&v, Some(sid), &mut Default::default()) {
                            v
                        } else {
                            hidden()
                        }
                    }
                    Err(_) => hidden(),
                }
            })
            .collect();
        out["checkpoint"] = json!({"candidates":self.block(checkpoint["candidates"].clone(),Some(sid)),"plan":{"groups":groups,"held":plan["held"]}});
        out["history"] = json!({"status":"privacy_filtered","reason":"request.history 未链接 source_id，依赖不可确认"});
        out
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use hippocampus::*;

    pub(crate) fn fixture() -> Value {
        let sample: Value = serde_json::from_str(include_str!(
            "../../tests/eval/w4/samples/w4_01_state_change.json"
        ))
        .unwrap();
        let contract: contract::ContractCandidate = serde_json::from_value(json!({
            "candidate":sample["input"]["candidates"][0]["candidate"],
            "source":sample["input"]["candidates"][0]["source"],
            "said_on":null,"time_resolutions":[]
        }))
        .unwrap();
        let c = contract
            .project(&contract::ProjectionContext {
                owner_id: UserId::from("owner"),
                session_id: SessionId::from("session"),
                turn_id: TurnId::from("turn"),
                evidence_id: EvidenceId::from("evidence"),
                observed_at: Timestamp(1),
                task_id: None,
                explicit_memory_request: false,
            })
            .unwrap()
            .candidate;
        let mut state = PersistedMemoryState::default();
        let m = Memory {
            id: MemoryId(1),
            owner_id: c.owner_id.clone(),
            evidence: c.evidence.clone(),
            subject_id: EntityId::from("self"),
            memory_type: c.memory_type,
            predicate: c.predicate.clone(),
            value: c.value.clone(),
            polarity: c.polarity,
            strength: c.strength,
            scope: c.scope.clone(),
            retention_tier: c.retention_tier,
            status: MemoryStatus::Active,
            temporal: c.temporal.clone(),
            extraction_confidence: c.extraction_confidence,
            certainty: c.certainty,
            supersedes: None,
            superseded_by: None,
            supersede_reason: None,
            explicit_memory_request: false,
            reinforcement_count: 0,
            retrieval_count: 0,
            last_reinforced_at: None,
            last_retrieved_at: None,
            tags: vec![],
        };
        state.repository.insert(m.clone()).unwrap();
        let mut legacy = m;
        legacy.id = MemoryId(2);
        state.repository.insert(legacy).unwrap();
        let mut value = serde_json::to_value(state).unwrap();
        value["record_updates"]["1"] =
            json!({"status":"pending","reasons":["update_scope_incomplete"]});
        value["w5_receipts"]["g"] = json!({"group_id":"g","fingerprint":json!({"candidates":[{"index":0,"contract":contract}]}).to_string(),"status":"Committed","legacy_fingerprint":false,"products":{"0":1},"decisions":[],"updates":{},"merged":{},"form":"v8"});
        let _: PersistedMemoryState = serde_json::from_value(value.clone()).unwrap();
        value
    }
    #[test]
    fn current_and_legacy_use_runtime_projection_without_changing_snapshot() {
        let value = fixture();
        let before = value.clone();
        let result = project(&value).unwrap();
        assert_eq!(value, before);
        assert_eq!(result["records"]["1"]["record"]["format"], "current");
        assert_eq!(
            result["records"]["1"]["record"]["update"]["status"],
            "pending"
        );
        assert_eq!(
            result["records"]["1"]["record"]["update"]["reasons"][0],
            "update_scope_incomplete"
        );
        assert_eq!(result["records"]["2"]["record"]["format"], "legacy");
        assert!(result["records"]["2"]["record"]["update"].is_null());
        let mut judged = value.clone();
        judged["record_updates"]["1"] = json!({"status":"judged","reasons":[]});
        assert_eq!(
            project(&judged).unwrap()["records"]["1"]["record"]["update"]["status"],
            "judged"
        );
        let mut deleted = value;
        deleted["repository"]["memories"]["1"]["status"] = json!("Deleted");
        assert!(project(&deleted).unwrap()["records"].get("1").is_none());
        assert!(project(&json!({})).is_err());
    }
    #[test]
    fn workflow_preserves_rejection_review_and_coverage_without_binding_snapshots() {
        let source = json!({"result":{"w4_batch":{"batch":{"w2":{"candidates":[{"status":"rejected","reasons":["quote_not_found"]}]},"review":{"output":{"reviews":[{"verdict":"insufficient","aspects":["subject"]}],"coverage":{"status":"gaps","missing":[{"text":"虚构遗漏"}]}}}}},"w4_checkpoint":{"source_binding":{"secret":"not exported"},"candidates":[],"plan":{"held":[0]}},"w4_input_meta":{"admissions":{"0":{"reason":"review_insufficient"}}}}});
        let v = workflow(&source);
        assert_eq!(v["w2"]["candidates"][0]["reasons"][0], "quote_not_found");
        assert_eq!(v["w3"]["reviews"][0]["verdict"], "insufficient");
        assert_eq!(v["w3"]["coverage"]["status"], "gaps");
        assert!(!v.to_string().contains("not exported"));
        assert_eq!(workflow(&json!({}))["status"], "not_captured");
    }
}

#[cfg(test)]
mod privacy_tests {
    use super::*;
    fn state() -> Value {
        let mut s = super::tests::fixture();
        let body = s["repository"]["memories"]["1"]["evidence"][0]["quote"].clone();
        s["sources"]["safe"] = json!({"id":"s-205","session_id":"session","turn_id":"turn","observed_at":1,"request":{"text":body,"history":["deleted-private-history"]},"enrichment":"pending","error":null,"result":null,"base_revision":0,"received_sequence":1});
        s["sources"]["gone"] = s["sources"]["safe"].clone();
        s["sources"]["gone"]["id"] = json!("gone");
        s["sources"]["gone"]["turn_id"] = json!("gone");
        s["sources"]["gone"]["request"] = Value::Null;
        s
    }
    #[test]
    fn af360_checks_raw_evidence_version_chain_owner_and_relations() {
        let s = state();
        assert!(Visibility::new(&s, "owner").record("1"));
        assert!(Visibility::new(&s, "owner").record("2"));
        assert!(!Visibility::new(&s, "other").record("1"));
        let mut broken = s.clone();
        broken["repository"]["memories"]["2"]["evidence"][0]["turn_id"] = json!("gone");
        assert!(!Visibility::new(&broken, "owner").record("2"));
        broken["repository"]["memories"]["1"]["supersedes"] = json!(2);
        assert!(!Visibility::new(&broken, "owner").record("1"));
        let mut related = s.clone();
        related["relations"]["r"] = json!({"id":"r","kind":"conflict","members":[1,99],"conditions":[],"times":[],"basis":[],"status":"open","resolution":null,"created_by":"g","closed_by":null});
        assert!(!Visibility::new(&related, "owner").record("1"));
        let mut deleted = s;
        deleted["repository"]["memories"]["1"]["status"] = json!("Deleted");
        assert!(!Visibility::new(&deleted, "owner").record("1"));
    }
    #[test]
    fn af360_local_w1_w2_w3_survive_unrelated_tombstone() {
        let mut s = state();
        let fp: Value =
            serde_json::from_str(s["w5_receipts"]["g"]["fingerprint"].as_str().unwrap()).unwrap();
        let candidate = fp["candidates"][0]["contract"]["candidate"].clone();
        let review = json!({"reviews":[{"candidate":0,"verdict":"insufficient","problems":[{"aspect":"subject","quote":{"text":s["sources"]["safe"]["request"]["text"]}}],"episode_value":"not_applicable"}],"coverage":{"status":"complete","missing":[]},"request_reviews":[]});
        s["sources"]["safe"]["result"] = json!({"w4_batch":{"batch":{"w2":{"candidates":[{"index":0,"status":"ok","candidate":candidate,"time_resolutions":[],"reasons":[]}],"memory_requests":[],"index_map":[0],"task_id":null,"said_on":null,"utc_offset_seconds":0},"review":{"output":review}}},"w4_model_stages":{"w1":{"output":{"candidates":[candidate],"memory_requests":[]}}}});
        let v = Visibility::new(&s, "owner").workflow(&s["sources"]["safe"]);
        assert_eq!(v["w1"]["candidates"].as_array().unwrap().len(), 1);
        assert_eq!(v["w2"]["candidates"][0]["status"], "ok");
        assert_eq!(v["w3"]["reviews"][0]["verdict"], "insufficient");
        s["sources"]["safe"]["result"]["w4_batch"]["batch"]["review"]["output"]["reviews"][0]["problems"]
            [0]["quote"]["text"] = json!("deleted-private-history");
        assert_eq!(
            Visibility::new(&s, "owner").workflow(&s["sources"]["safe"])["w3"],
            hidden()
        );
    }

    #[test]
    fn af360_w4_basis_and_unlinked_history_are_hidden_per_block() {
        let mut s = state();
        let quote = s["sources"]["safe"]["request"]["text"].clone();
        let decision = json!({"candidate":0,"action":"New","targets":[],"basis_sources":["s-205"],"basis":[{"source":{"kind":"stored","source_id":"s-205"},"quote":{"text":quote}}],"reason":null,"effective":null});
        s["sources"]["safe"]["result"] = json!({"w4_checkpoint":{"candidates":[],"plan":{"groups":[{"id":"g","decisions":[decision]}],"held":{}}}});
        let v = Visibility::new(&s, "owner").workflow(&s["sources"]["safe"]);
        assert_eq!(v["checkpoint"]["plan"]["groups"][0]["id"], "g");
        assert!(!v.to_string().contains("deleted-private-history"));
        assert_eq!(v["history"]["status"], "privacy_filtered");
        s["sources"]["safe"]["result"]["w4_checkpoint"]["plan"]["groups"][0]["decisions"][0]["basis"]
            [0]["source"]["source_id"] = json!("gone");
        let v = Visibility::new(&s, "owner").workflow(&s["sources"]["safe"]);
        assert_eq!(v["checkpoint"]["plan"]["groups"][0], hidden());
        let g = Visibility::new(&s, "owner");
        assert_eq!(
            g.block(
                json!({"coverage":{"missing":[{"text":"deleted-private-history"}]}}),
                Some("s-205")
            ),
            hidden()
        );
        assert_eq!(g.block(json!({"memory_id":99}), Some("s-205")), hidden());
    }
}
