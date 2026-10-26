//! Mechanical entity controls; the host must obtain the USER's identity confirmation.
use super::*;
use crate::contract::{EvidenceRef, SourceRef};

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(crate = "rmcp::schemars")]
pub(super) struct Merge {
    from: String,
    into: String,
    request_id: String,
    expected_from_state_token: String,
    expected_into_state_token: String,
    #[schemars(schema_with = "confirmation_schema")]
    user_confirmation: EvidenceRef,
}

fn confirmation_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({"type":"object","additionalProperties":false,
    "required":["source","quote"],"properties":{
        "source":{"type":"object","additionalProperties":false,"required":["kind","source_id"],
            "properties":{"kind":{"const":"stored"},"source_id":{"type":"string"}}},
        "quote":{"type":"object","additionalProperties":false,"required":["text"],
            "properties":{"text":{"type":"string"}}}
    }})
}

pub(super) fn visible(entity: &Entity) -> Value {
    let mut value = json!(entity);
    value["state_token"] = json!(crate::recall_models::fingerprint(&json!(entity)));
    value
}

pub(super) fn conflict(entities: &[Entity]) -> Value {
    json!({"error":"alias_conflict","conflicts":entities.iter().map(|e|json!({
        "id":e.id,"canonical_name":e.canonical_name,"aliases":e.aliases
    })).collect::<Vec<_>>()})
}

pub(super) fn retired(state: &PersistedMemoryState, id: &EntityId) -> bool {
    // Audit metadata also survives deletion of the confirmation text; IDs cannot be resurrected.
    state
        .receipts
        .values()
        .any(|r| r.request["kind"] == "entity_merge" && r.request["entity_id"] == id.0)
}

fn references(
    state: &PersistedMemoryState,
    owner: &UserId,
    id: &EntityId,
) -> PersistenceResult<Vec<Value>> {
    fn scan(value: &Value, path: &str, id: &str, out: &mut Vec<Value>) {
        match value {
            Value::Object(object) => {
                for (key, value) in object {
                    // Input catalogs and raw model bindings are audit snapshots, not identity assignments.
                    if matches!(
                        key.as_str(),
                        "binding"
                            | "source_binding"
                            | "evidence_binding"
                            | "provider_binding"
                            | "entities"
                            | "known_entities"
                    ) {
                        continue;
                    }
                    let direct = matches!(key.as_str(), "subject_id" | "entity_id")
                        && value.as_str() == Some(id);
                    let typed =
                        matches!(key.as_str(), "subject" | "original_subject" | "resolution")
                            && (value["Resolved"].as_str() == Some(id)
                                || items_contains(&value["Ambiguous"], id));
                    let entity_value = key == "value" && value["Entity"].as_str() == Some(id);
                    let next = format!("{path}/{key}");
                    if direct || typed || entity_value {
                        out.push(json!({"path":next,"entity_id":id}));
                    }
                    scan(value, &next, id, out);
                }
            }
            Value::Array(array) => {
                for (index, value) in array.iter().enumerate() {
                    scan(value, &format!("{path}/{index}"), id, out);
                }
            }
            _ => {}
        }
    }
    fn items_contains(value: &Value, id: &str) -> bool {
        value
            .as_array()
            .is_some_and(|items| items.iter().any(|v| v.as_str() == Some(id)))
    }
    let mut refs = Vec::new();
    for memory in state.repository.list_by_owner(owner)? {
        scan(
            &json!(memory),
            &format!("records/{}", memory.id.0),
            &id.0,
            &mut refs,
        );
    }
    for staged in state.staging.list_by_owner(owner) {
        scan(
            &json!(staged),
            &format!("staging/{}", staged.id.0),
            &id.0,
            &mut refs,
        );
    }
    for source in state.sources.values() {
        scan(
            &json!(source.management.confirmations),
            &format!("sources/{}/confirmations", source.id),
            &id.0,
            &mut refs,
        );
        if let Some(result) = &source.result {
            scan(
                result,
                &format!("sources/{}/result", source.id),
                &id.0,
                &mut refs,
            );
        }
        for (index, audit) in source
            .management
            .audits
            .iter()
            .enumerate()
            .filter(|(_, a)| a["kind"] != "entity_merge")
        {
            scan(
                audit,
                &format!("sources/{}/audits/{index}", source.id),
                &id.0,
                &mut refs,
            );
        }
    }
    for (key, receipt) in &state.w5_receipts {
        match serde_json::from_str::<Value>(&receipt.fingerprint) {
            Ok(value) => scan(&value, &format!("w5_receipts/{key}"), &id.0, &mut refs),
            Err(_) => refs.push(
                json!({"path":format!("w5_receipts/{key}"),"reason":"unreadable_identity_binding"}),
            ),
        }
    }
    for (key, receipt) in &state.receipts {
        if key.starts_with("[\"recall.") || receipt.request["kind"] == "entity_merge" {
            continue;
        }
        scan(
            &receipt.request,
            &format!("receipts/{key}/request"),
            &id.0,
            &mut refs,
        );
        scan(
            &receipt.result,
            &format!("receipts/{key}/result"),
            &id.0,
            &mut refs,
        );
    }
    Ok(refs)
}

fn confirmation_key(
    state: &PersistedMemoryState,
    evidence: &EvidenceRef,
) -> PersistenceResult<String> {
    let SourceRef::Stored { source_id } = &evidence.source else {
        return Err("invalid_user_confirmation".into());
    };
    let (key, source) = state
        .sources
        .iter()
        .find(|(_, s)| &s.id == source_id)
        .ok_or("invalid_user_confirmation")?;
    if evidence.quote.text.trim().is_empty()
        || evidence.quote.text.chars().count() > 2000
        || source
            .request
            .as_ref()
            .is_none_or(|r| r["capture_source"] != true)
        || crate::lexical::body(source).is_none_or(|text| !text.contains(&evidence.quote.text))
    {
        return Err("invalid_user_confirmation".into());
    }
    Ok(key.clone())
}

impl ChatMemoryService {
    pub(super) fn merge_entity(&mut self, input: Merge) -> PersistenceResult<Value> {
        for (name, value, max) in [
            ("from", &input.from, 200),
            ("into", &input.into, 200),
            ("request_id", &input.request_id, 512),
            (
                "expected_from_state_token",
                &input.expected_from_state_token,
                512,
            ),
            (
                "expected_into_state_token",
                &input.expected_into_state_token,
                512,
            ),
        ] {
            bounded_text(value, name, max)?;
        }
        if input.from == "self" || input.into == "self" {
            return Err("self_protected".into());
        }
        if input.from == input.into {
            return Err("entity_merge_same_id".into());
        }
        let owner = self.store.owner().clone();
        let from_id = EntityId::from(input.from.as_str());
        let into_id = EntityId::from(input.into.as_str());
        let digest = crate::recall_models::fingerprint(&json!(input));
        let key = json!([
            "entity_merge",
            crate::recall_models::fingerprint(&json!(input.request_id))
        ])
        .to_string();
        let out=self.store.transaction(|state| {
            if let Some(receipt)=state.receipts.get(&key) {
                if receipt.request["digest"]!=digest {return Err("request_id_conflict".into());}
                confirmation_key(state,&input.user_confirmation)?;
                if receipt.result["error"].is_string() {return Err("invalid_user_confirmation".into());}
                let mut out=receipt.result.clone(); out["replayed"]=json!(true); return Ok(out);
            }
            let from=state.entities.get(&from_id).filter(|e|e.owner_id==owner).cloned().ok_or("entity_not_found")?;
            let into=state.entities.get(&into_id).filter(|e|e.owner_id==owner).cloned().ok_or("entity_not_found")?;
            if from.entity_type!=EntityType::Person || into.entity_type!=EntityType::Person {return Err("entity_merge_requires_person".into());}
            if visible(&from)["state_token"]!=input.expected_from_state_token || visible(&into)["state_token"]!=input.expected_into_state_token {
                return Err("entity_changed".into());
            }
            let confirming=confirmation_key(state,&input.user_confirmation)?;
            let refs=references(state,&owner,&from_id)?;
            if !refs.is_empty() {return Ok(json!({"error":"entity_has_references","references":refs}));}
            let mut merged=into.clone();
            let mut names=BTreeSet::from([crate::store::entity_name_key(&merged.canonical_name)]);
            merged.aliases=into.aliases.iter().chain(std::iter::once(&from.canonical_name)).chain(&from.aliases)
                .filter(|name|names.insert(crate::store::entity_name_key(name))).map(|name|name.trim().to_owned()).collect();
            if merged.aliases.len()>100 {return Err("aliases exceeds 100 entries".into());}
            let conflicts=state.entities.alias_conflicts(&merged,Some(&from_id));
            if !conflicts.is_empty() {return Ok(conflict(&conflicts));}
            state.entities.remove(&from_id);
            state.entities.upsert(merged.clone());
            state.sources.get_mut(&confirming).unwrap().management.audits.push(json!({
                "kind":"entity_merge","request_hash":crate::recall_models::fingerprint(&json!(input.request_id)),
                "from_before":from,"into_before":into,"into_after":merged,
                "expected_from_state_token":input.expected_from_state_token,"expected_into_state_token":input.expected_into_state_token,
                "user_confirmation":input.user_confirmation,
            }));
            let out=json!({"merged":true,"from":from_id,"into":into_id,"entity":visible(&merged),"replayed":false,
                "audit":{"source_id":state.sources[&confirming].id,"quote_hash":crate::recall_models::fingerprint(&json!(input.user_confirmation.quote))}});
            state.receipts.insert(key.clone(),crate::sqlite::TurnReceipt {
                request:json!({"kind":"entity_merge","digest":digest,"source_id":state.sources[&confirming].id,
                    "entity_id":from_id,"subject_id":into_id}), result:out.clone(),
            });
            Ok(out)
        })?;
        if out["merged"] == true && out["replayed"] != true {
            self.recall_runtime.invalidate_owner(&self.store);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    const CONFIRM: &str = "北北与小北是同一个孩子。";
    struct Db(PathBuf);
    impl Db {
        fn new() -> Self {
            Self(
                std::env::temp_dir()
                    .join(format!("entity-controls-{}", crate::diagnostics::new_id()))
                    .join("memory.sqlite3"),
            )
        }
        fn open(&self) -> SqliteMemoryStore {
            SqliteMemoryStore::open(&self.0, "alice".into()).unwrap()
        }
        fn service(&self) -> ChatMemoryService {
            ChatMemoryService::new(
                self.open(),
                ChatConfig {
                    enrichment_mode: EnrichmentMode::Off,
                    ..Default::default()
                },
            )
            .unwrap()
        }
        fn snapshot(&self) -> Value {
            json!(self.open().read().unwrap())
        }
        fn call(&self, name: &str, args: Value) -> PersistenceResult<Value> {
            self.service().call(name, args, || false)
        }
        fn capture(&self) -> String {
            self.call("memory_remember",json!({"text":CONFIRM,"session_id":"entities","turn_id":"confirmation","capture_source":true})).unwrap()
                ["source"]["source_id"].as_str().unwrap().into()
        }
        fn setup(&self) -> Value {
            for (id, name) in [("from", "北北"), ("into", "小北")] {
                assert!(
                    self.call("memory_entity_upsert", upsert(id, name, &[]))
                        .unwrap()
                        .get("error")
                        .is_none()
                );
            }
            command(self, &self.capture(), "merge-1")
        }
    }
    impl Drop for Db {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(self.0.parent().unwrap());
        }
    }
    fn upsert(id: &str, name: &str, aliases: &[&str]) -> Value {
        json!({"id":id,"canonical_name":name,"aliases":aliases,"entity_type":"Person"})
    }
    fn command(db: &Db, source_id: &str, request: &str) -> Value {
        let listed = db.call("memory_list", json!({})).unwrap();
        let token = |id: &str| {
            listed["entities"]
                .as_array()
                .unwrap()
                .iter()
                .find(|e| e["id"] == id)
                .unwrap()["state_token"]
                .clone()
        };
        json!({"from":"from","into":"into","request_id":request,
            "expected_from_state_token":token("from"),"expected_into_state_token":token("into"),
            "user_confirmation":{"source":{"kind":"stored","source_id":source_id},"quote":{"text":CONFIRM}}})
    }
    fn candidate(subject: &str, value: MemoryValue) -> CandidateMemory {
        CandidateMemory::new(
            "alice".into(),
            subject.into(),
            MemoryType::Fact,
            "fact".into(),
            value,
            Timestamp(100),
            Evidence::new(
                "fixture".into(),
                "fixture".into(),
                "turn".into(),
                "虚构的引用检查。",
            ),
        )
    }

    #[test]
    fn alias_precheck_uses_parser_for_names_case_whitespace_and_owner() {
        let db = Db::new();
        db.call(
            "memory_entity_upsert",
            upsert("child-a", "  BéBé  ", &[" 孩子 ", " CHILD "]),
        )
        .unwrap();
        let before = db.snapshot();
        for args in [
            upsert("child-b", "bÉbé", &[]),
            upsert("child-b", "第二个孩子", &[" child "]),
            upsert("child-b", "孩子", &[]),
        ] {
            let collision = db.call("memory_entity_upsert", args).unwrap();
            assert_eq!(collision["error"], "alias_conflict");
            assert_eq!(collision["conflicts"][0]["id"], "child-a");
            assert_eq!(collision["conflicts"][0]["canonical_name"], "BéBé");
            assert_eq!(db.snapshot(), before);
        }
        db.call(
            "memory_entity_upsert",
            upsert("child-b", "第二个孩子", &["小乙"]),
        )
        .unwrap();
        let before = db.snapshot();
        assert_eq!(
            db.call(
                "memory_entity_upsert",
                upsert("child-b", "更新孩子", &["孩子"])
            )
            .unwrap()["error"],
            "alias_conflict"
        );
        assert_eq!(db.snapshot(), before);
        let entities = db.open().read().unwrap().entities;
        for name in ["BÉBÉ", "  Bébé ", " child ", "孩子"] {
            assert_eq!(
                entities.resolve(&"alice".into(), name),
                EntityResolution::Resolved("child-a".into())
            );
        }
        // Dirty legacy directory entries use exactly the same normalization, not a second matcher.
        db.open()
            .transaction(|s| {
                s.entities.upsert(Entity {
                    id: "legacy".into(),
                    owner_id: "alice".into(),
                    entity_type: EntityType::Person,
                    canonical_name: "  老名字  ".into(),
                    aliases: vec!["  MIXED  ".into()],
                });
                Ok(())
            })
            .unwrap();
        assert_eq!(
            db.open()
                .read()
                .unwrap()
                .entities
                .resolve(&"alice".into(), " mixed "),
            EntityResolution::Resolved("legacy".into())
        );
        assert_eq!(
            db.call(
                "memory_entity_upsert",
                upsert("another", "另一人", &["mixed"])
            )
            .unwrap()["error"],
            "alias_conflict"
        );
        let mut bob = ChatMemoryService::new(
            SqliteMemoryStore::open(&db.0, "bob".into()).unwrap(),
            Default::default(),
        )
        .unwrap();
        assert!(
            bob.call(
                "memory_entity_upsert",
                upsert("child-b", "孩子", &["child"]),
                || false
            )
            .unwrap()
            .get("error")
            .is_none()
        );
    }

    #[test]
    fn merge_requires_saved_user_confirmation_and_protects_self_and_foreign() {
        let db = Db::new();
        let args = db.setup();
        let before = db.snapshot();
        for (field, value) in [
            ("from", json!("self")),
            ("into", json!("self")),
            ("into", json!("from")),
        ] {
            let mut bad = args.clone();
            bad[field] = value;
            assert!(db.call("memory_entity_merge", bad).is_err());
        }
        for evidence in [
            json!({"source":{"kind":"current"},"quote":{"text":CONFIRM}}),
            json!({"source":{"kind":"stored","source_id":"unknown"},"quote":{"text":CONFIRM}}),
            json!({"source":args["user_confirmation"]["source"],"quote":{"text":"宿主编造的确认。"}}),
        ] {
            let mut bad = args.clone();
            bad["user_confirmation"] = evidence;
            assert!(
                db.call("memory_entity_merge", bad)
                    .unwrap_err()
                    .to_string()
                    .contains("invalid_user_confirmation")
            );
        }
        assert_eq!(db.snapshot(), before);
        db.open()
            .transaction(|s| {
                s.entities.upsert(Entity {
                    id: "foreign".into(),
                    owner_id: "bob".into(),
                    entity_type: EntityType::Person,
                    canonical_name: "别人的人物".into(),
                    aliases: vec![],
                });
                Ok(())
            })
            .unwrap();
        let before = db.snapshot();
        for field in ["from", "into"] {
            let mut bad = args.clone();
            bad[field] = json!("foreign");
            assert!(
                db.call("memory_entity_merge", bad)
                    .unwrap_err()
                    .to_string()
                    .contains("entity_not_found")
            );
        }
        assert!(
            db.call("memory_entity_upsert", upsert("foreign", "越权覆盖", &[]))
                .unwrap_err()
                .to_string()
                .contains("entity_not_found")
        );
        assert_eq!(db.snapshot(), before);
        let mut self_bad = upsert("self", "用户", &[]);
        self_bad["entity_type"] = json!("Place");
        assert!(db.call("memory_entity_upsert", self_bad).is_err());
        // A cross-owner source ID cannot authorize this owner's action.
        let mut bob = ChatMemoryService::new(
            SqliteMemoryStore::open(&db.0, "bob".into()).unwrap(),
            ChatConfig {
                enrichment_mode: EnrichmentMode::Off,
                ..Default::default()
            },
        )
        .unwrap();
        let foreign=bob.call("memory_remember",json!({"text":CONFIRM,"session_id":"bob","turn_id":"confirmation","capture_source":true}),||false).unwrap();
        let mut bad = args;
        bad["user_confirmation"]["source"]["source_id"] = foreign["source"]["source_id"].clone();
        assert!(
            db.call("memory_entity_merge", bad)
                .unwrap_err()
                .to_string()
                .contains("invalid_user_confirmation")
        );
    }

    #[test]
    fn merge_rechecks_both_tokens_and_third_entity_collisions_atomically() {
        for id in ["from", "into"] {
            let db = Db::new();
            let args = db.setup();
            db.call("memory_entity_upsert", upsert(id, "变更后的名字", &[]))
                .unwrap();
            let before = db.snapshot();
            assert!(
                db.call("memory_entity_merge", args)
                    .unwrap_err()
                    .to_string()
                    .contains("entity_changed")
            );
            assert_eq!(db.snapshot(), before);
        }
        let db = Db::new();
        let args = db.setup();
        // A pre-existing ambiguous legacy directory must not be silently folded into a third person.
        db.open()
            .transaction(|s| {
                s.entities.upsert(Entity {
                    id: "third".into(),
                    owner_id: "alice".into(),
                    entity_type: EntityType::Person,
                    canonical_name: "第三个人".into(),
                    aliases: vec!["北北".into()],
                });
                Ok(())
            })
            .unwrap();
        let before = db.snapshot();
        let out = db.call("memory_entity_merge", args).unwrap();
        assert_eq!(out["error"], "alias_conflict");
        assert_eq!(out["conflicts"][0]["id"], "third");
        assert_eq!(db.snapshot(), before);
    }

    #[test]
    fn merge_rejects_all_authoritative_reference_locations_without_rewriting_audits() {
        for location in [
            "record_subject",
            "record_value",
            "staging",
            "confirmation",
            "w4_batch",
            "w4_checkpoint",
            "w1_output",
            "w5",
            "archived_batch",
        ] {
            let db = Db::new();
            let args = db.setup();
            db.open().transaction(|s| {
                let c=candidate(if location=="record_subject"{"from"}else{"self"},if location=="record_value"{MemoryValue::Entity("from".into())}else{MemoryValue::Text("普通值".into())});
                let source=s.sources.values_mut().next().unwrap();
                match location {
                    "record_subject"|"record_value"=>s.repository.insert(Memory::from_candidate(MemoryId(1),&c,None))?,
                    "staging"=>{let mut c=c.clone();c.subject=CandidateSubject::Resolved("from".into());
                        s.staging=serde_json::from_value(json!({"next_id":1,"pending":{"1":{"id":1,"candidate":c,"resolution":{"Resolved":"from"}}}}))?;},
                    "confirmation"=>source.management.confirmations.push(SubjectConfirmation {generation:0,candidate_index:0,candidate_identity:"fixture".into(),
                        original_subject:crate::contract::Subject::Unresolved(crate::contract::UnresolvedSubject {unresolved:crate::contract::Quote {text:"孩子".into()}}),
                        entity_id:"from".into(),evidence:serde_json::from_value(args["user_confirmation"].clone())?,evidence_binding:json!({})}),
                    "w4_batch"=>source.result=Some(json!({"w4_batch":{"batch":{"w2":{"candidates":[{"candidate":{"subject":{"entity_id":"from"}}}]}}}})),
                    "w4_checkpoint"=>source.result=Some(json!({"w4_checkpoint":{"candidates":[{"candidate":{"value":{"kind":"entity","entity_id":"from","quote":{"text":"人"}}}}]}})),
                    "w1_output"=>source.result=Some(json!({"w4_model_stages":{"w1":{"binding":{"input":{"entities":[{"id":"from"}]}},"output":{"candidates":[{"subject":{"entity_id":"from"}}]}}}})),
                    "archived_batch"=>source.management.audits.push(json!({"result":{"w4_batch":{"subject":{"entity_id":"from"}}}})),
                    "w5"=>{s.w5_receipts.insert("fixture".into(),serde_json::from_value(json!({"group_id":"fixture","fingerprint":json!({"seen":[{"subject_id":"from"}]}).to_string(),
                        "status":"Committed","legacy_fingerprint":false,"products":{},"decisions":[],"updates":{},"merged":{},"form":"v8"}))?);},
                    _=>unreachable!(),
                }
                Ok(())
            }).unwrap();
            let before = db.snapshot();
            let result = db.call("memory_entity_merge", args).unwrap();
            assert_eq!(result["error"], "entity_has_references", "{location}");
            assert!(
                !result["references"].as_array().unwrap().is_empty(),
                "{location}"
            );
            assert_eq!(db.snapshot(), before, "{location}");
        }
    }

    #[test]
    fn merge_is_idempotent_deduplicated_reopened_and_retired_ids_stay_invalid() {
        let db = Db::new();
        let mut args = db.setup();
        db.open().transaction(|s| {let mut e=s.entities.get(&"from".into()).unwrap().clone();e.aliases=vec![" Child ".into(),"孩子".into()];s.entities.upsert(e);
            let mut e=s.entities.get(&"into".into()).unwrap().clone();e.aliases=vec!["child".into()," 孩子 ".into()];s.entities.upsert(e);
            // Catalog and raw strings alone are not semantic ID assignments.
            s.sources.values_mut().next().unwrap().result=Some(json!({"w4_model_stages":{"w1":{"binding":{"input":{"entities":[{"entity_id":"from"}]}}}},"text":"from","value":{"Text":"from"}}));Ok(())}).unwrap();
        args = command(
            &db,
            args["user_confirmation"]["source"]["source_id"]
                .as_str()
                .unwrap(),
            "merge-1",
        );
        let before = db.open().read().unwrap();
        let audit_inputs = before.sources.values().next().unwrap().result.clone();
        let result = db.call("memory_entity_merge", args.clone()).unwrap();
        assert_eq!(result["merged"], true);
        assert_eq!(result["replayed"], false);
        assert_eq!(
            result["entity"]["aliases"],
            json!(["child", "孩子", "北北"])
        );
        let after = db.open().read().unwrap();
        assert_eq!(after.revision, before.revision + 1);
        assert!(after.entities.get(&"from".into()).is_none());
        assert_eq!(
            after.entities.resolve(&"alice".into(), "北北"),
            EntityResolution::Resolved("into".into())
        );
        assert_eq!(after.sources.values().next().unwrap().result, audit_inputs);
        assert_eq!(
            after
                .sources
                .values()
                .next()
                .unwrap()
                .management
                .audits
                .len(),
            1
        );
        let snapshot = db.snapshot();
        let replay = db.call("memory_entity_merge", args.clone()).unwrap();
        assert_eq!(replay["replayed"], true);
        assert_eq!(db.snapshot(), snapshot);
        let mut changed = args.clone();
        changed["into"] = json!("different");
        assert!(
            db.call("memory_entity_merge", changed)
                .unwrap_err()
                .to_string()
                .contains("request_id_conflict")
        );
        assert!(
            db.call("memory_entity_upsert", upsert("from", "旧ID", &[]))
                .unwrap_err()
                .to_string()
                .contains("entity_id_retired")
        );
        // W2 cannot refer to a retired ID, without rewriting or redirecting it to another person.
        let mut output:crate::contract::W1Output=serde_json::from_value(json!({"memory_requests":[],"candidates":[{
            "kind":"assertion","speech":"user_statement","subject":{"entity_id":"from"},"relation":"fact","memory_type":"fact",
            "value":{"kind":"text","text":"我喜欢蓝色。","quote":{"text":"我喜欢蓝色。"}},"sentence":{"text":"我喜欢蓝色。"},
            "polarity":"affirm","strength":null,"uncertainty":null,"conditions":[],"times":[]}]})).unwrap();
        let mut entity_value = json!(&output.candidates[0]);
        entity_value["subject"] = json!({"entity_id":"self"});
        entity_value["value"] =
            json!({"kind":"entity","entity_id":"from","quote":{"text":"我喜欢蓝色。"}});
        output
            .candidates
            .push(serde_json::from_value(entity_value).unwrap());
        let ids = after
            .entities
            .list_by_owner(&"alice".into())
            .into_iter()
            .map(|e| e.id)
            .collect::<Vec<_>>();
        let catalog = crate::w2::relation_catalog(&ChatConfig::default().predicates, &[]).unwrap();
        let batch = crate::w2::validate(
            output,
            &crate::w2::Context {
                text: "我喜欢蓝色。",
                entities: &ids,
                relations: &catalog,
                said_on: None,
                utc_offset_seconds: 28800,
                task_id: None,
            },
        )
        .unwrap();
        assert_eq!(batch.candidates[0].status, crate::w2::Status::Rejected);
        assert_eq!(batch.candidates[1].status, crate::w2::Status::Rejected);
        assert_eq!(
            rusqlite::Connection::open(&db.0)
                .unwrap()
                .pragma_query_value::<i64, _>(None, "user_version", |r| r.get(0))
                .unwrap(),
            11
        );
        db.call(
            "memory_forget",
            json!({"source_id":args["user_confirmation"]["source"]["source_id"]}),
        )
        .unwrap();
        assert!(!db.snapshot().to_string().contains(CONFIRM));
        assert!(db.call("memory_entity_merge", args).is_err());
        assert!(
            db.call("memory_entity_upsert", upsert("from", "旧ID", &[]))
                .unwrap_err()
                .to_string()
                .contains("entity_id_retired")
        );
    }

    #[test]
    fn independent_connections_cannot_double_merge_or_create_conflicting_names() {
        let db = Db::new();
        let args = db.setup();
        let barrier = Arc::new(Barrier::new(3));
        let handles = (0..2)
            .map(|i| {
                let path = db.0.clone();
                let barrier = barrier.clone();
                let mut args = args.clone();
                args["request_id"] = json!(format!("race-{i}"));
                std::thread::spawn(move || {
                    let mut service = ChatMemoryService::new(
                        SqliteMemoryStore::open_operation(path, "alice".into()).unwrap(),
                        ChatConfig {
                            enrichment_mode: EnrichmentMode::Off,
                            ..Default::default()
                        },
                    )
                    .unwrap();
                    barrier.wait();
                    service.call("memory_entity_merge", args, || false)
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        let outcomes = handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            outcomes
                .iter()
                .filter(|o| o.as_ref().is_ok_and(|v| v["merged"] == true))
                .count(),
            1
        );
        assert_eq!(
            db.open()
                .read()
                .unwrap()
                .sources
                .values()
                .next()
                .unwrap()
                .management
                .audits
                .len(),
            1
        );
        let barrier = Arc::new(Barrier::new(3));
        let handles = (0..2)
            .map(|i| {
                let path = db.0.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let mut service = ChatMemoryService::new(
                        SqliteMemoryStore::open_operation(path, "alice".into()).unwrap(),
                        Default::default(),
                    )
                    .unwrap();
                    barrier.wait();
                    service
                        .call(
                            "memory_entity_upsert",
                            upsert(
                                &format!("race-child-{i}"),
                                &format!("不同孩子{i}"),
                                &["共同泛称"],
                            ),
                            || false,
                        )
                        .unwrap()
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        let outcomes = handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            outcomes
                .iter()
                .filter(|v| v["error"] == "alias_conflict")
                .count(),
            1
        );
        let conn = rusqlite::Connection::open(&db.0).unwrap();
        assert_eq!(
            conn.query_row::<i64, _, _>("SELECT count(*) FROM model_calls", [], |r| r.get(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn merge_directory_removal_keeps_order_barrier_and_mcp_refusals_are_errors() {
        let db = Db::new();
        let args = db.setup();
        db.open()
            .transaction(|s| {
                let source = s.sources.values_mut().next().unwrap();
                source.received_sequence = 7;
                source.enrichment = "queued".into();
                source.task.processing_version = Some(W4_PROCESSING_VERSION.into());
                Ok(())
            })
            .unwrap();
        let mut store = db.open();
        let key = store.read().unwrap().sources.keys().next().unwrap().clone();
        crate::commit::w4_start_round(&mut store, &key, "before-merge", false).unwrap();
        let claim = crate::commit::w4_claim_round(&mut store, &key).unwrap();
        crate::commit::w4_check_order(&store, &claim).unwrap();
        db.call("memory_entity_merge", args).unwrap();
        assert!(
            crate::commit::w4_check_order(&store, &claim)
                .unwrap_err()
                .to_string()
                .contains("needs_rejudge")
        );
        let state = db.open().read().unwrap();
        assert_eq!(state.source_less_barrier_sequence, 7);
        assert_eq!(
            state.sources.values().next().unwrap().enrichment,
            "stale_requires_review"
        );
        let conflict = db.service().call_with_global_context(
            "memory_entity_upsert",
            upsert("new", "北北", &[]),
            || false,
        );
        assert_eq!(conflict.is_error, Some(true));
        assert_eq!(
            conflict.structured_content.unwrap()["error"],
            "alias_conflict"
        );
        let tool = memory_tools()
            .into_iter()
            .find(|t| t.name == "memory_entity_merge")
            .unwrap();
        assert!(tool.annotations.unwrap().destructive_hint.unwrap());
        assert_eq!(
            tool.input_schema["properties"]["user_confirmation"]["properties"]["source"]["properties"]
                ["kind"]["const"],
            "stored"
        );
    }

    #[test]
    fn merge_invalidates_retained_recall_across_connections_without_model_calls() {
        for same_service in [true, false] {
            let db = Db::new();
            let args = db.setup();
            let mut service = db.service();
            service.call("memory_recall",json!({"query":"人物目录中的获准原话","offline":true,"request_id":"before-merge"}),||false).unwrap();
            assert!(
                service
                    .recall_runtime
                    .retained(&service.store, "before-merge")
                    .is_ok()
            );
            if same_service {
                service.call("memory_entity_merge", args, || false).unwrap();
            } else {
                db.call("memory_entity_merge", args).unwrap();
            }
            assert!(
                service
                    .recall_runtime
                    .retained(&service.store, "before-merge")
                    .is_err()
            );
            assert_eq!(
                rusqlite::Connection::open(&db.0)
                    .unwrap()
                    .query_row::<i64, _, _>("SELECT count(*) FROM model_calls", [], |r| r.get(0))
                    .unwrap(),
                0
            );
        }
    }

    #[test]
    fn deleted_or_unauthorized_confirmation_cannot_authorize_merge() {
        for deleted in [true, false] {
            let db = Db::new();
            let args = db.setup();
            if deleted {
                db.call(
                    "memory_forget",
                    json!({"source_id":args["user_confirmation"]["source"]["source_id"]}),
                )
                .unwrap();
            } else {
                db.open()
                    .transaction(|s| {
                        s.sources
                            .values_mut()
                            .next()
                            .unwrap()
                            .request
                            .as_mut()
                            .unwrap()["capture_source"] = json!(false);
                        Ok(())
                    })
                    .unwrap();
            }
            let before = db.snapshot();
            assert!(
                db.call("memory_entity_merge", args)
                    .unwrap_err()
                    .to_string()
                    .contains("invalid_user_confirmation")
            );
            assert_eq!(db.snapshot(), before);
        }
    }
}
