use super::*;

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("af236-{}", hippocampus::diagnostics::new_id())))
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn fixture_cases() -> Vec<Case> {
    cases(&config(), "https://fixture.invalid").unwrap()
}
fn output(case: &Case) -> Value {
    match case.id {
        "w1" => {
            json!({"memory_requests":[],"candidates":[inputs(&config())[2]["w4_input"]["candidates"][0]["candidate"].clone()]})
        }
        "w3" => {
            json!({"reviews":[{"candidate":0,"verdict":"supported","problems":[],"episode_value":"not_applicable"}],
            "coverage":{"status":"complete","missing":[]},"request_reviews":[]})
        }
        "w4" => {
            json!({"decisions":[{"candidate":0,"status":"decided","action":"new","targets":[],"resolves_relations":[],
            "basis":[{"source":{"kind":"stored","source_id":"s"},"quote":{"text":config().statement}}],
            "effective":null,"relation_scope":null,"pending_reason":null}]})
        }
        "r1" => json!(plan_output(false)),
        _ => json!({"verdict":"supported","facts":[{"statement":config().statement,
            "evidence":[{"source":if case.id=="r4-current" {json!({"kind":"current"})} else {json!({"kind":"stored","source_id":"s"})},
                "quote":{"text":config().statement}}],"record_ids":[],"polarity":"affirm","strength":null,
            "uncertainty":null,"conditions":[],"times":[],"current":"yes","applies":"yes","supersedes_record_ids":[]}],"compute":null}),
    }
}
fn envelope(case: &Case) -> Value {
    json!({"stop_reason":"end_turn","content":[{"type":"text","text":output(case).to_string()}],
        "usage":{"input_tokens":12,"output_tokens":20}})
}
fn same_acceptance(schema: &Value, values: &[Value]) {
    let before = model_provider::anthropic_schema(schema).unwrap();
    let wire = model_provider::anthropic_simplified_schema(schema).unwrap();
    let after = model_provider::expand_anthropic_simplified_schema(&wire).unwrap();
    for value in values {
        assert_eq!(
            model_provider::check_output(&before, value).is_ok(),
            model_provider::check_output(&after, value).is_ok(),
            "{value}"
        );
    }
}
fn flatten(node: &mut Value) {
    if let Some(props) = node.get_mut("properties").and_then(Value::as_object_mut) {
        for v in props.values_mut() {
            flatten(v);
        }
    }
    if let Some(v) = node.get_mut("items") {
        flatten(v);
    }
    if let Some(branches) = node.get_mut("anyOf").and_then(Value::as_array_mut) {
        let mut result = Vec::new();
        for mut b in std::mem::take(branches) {
            flatten(&mut b);
            if b.as_object().is_some_and(|o| o.len() == 1) && b["anyOf"].is_array() {
                result.extend(b["anyOf"].as_array().unwrap().iter().cloned());
            } else {
                result.push(b);
            }
        }
        *branches = result;
    }
}

#[test]
fn all_six_schemas_expand_to_the_same_language_and_preserve_patterns() {
    for case in fixture_cases() {
        let original = case.canonical["text"]["format"]["schema"].clone();
        let mut expected = model_provider::anthropic_schema(&original).unwrap();
        flatten(&mut expected);
        let wire = &case.wire["output_config"]["format"]["schema"];
        assert_eq!(
            model_provider::expand_anthropic_simplified_schema(wire).unwrap(),
            expected
        );
        assert_eq!(case.binding["schema_binding"]["original_schema"], original);
        assert_eq!(&case.binding["schema_binding"]["wire_schema"], wire);
        assert_eq!(
            case.binding["schema_binding"]["rewrite_version"],
            model_provider::ANTHROPIC_SIMPLIFIED_VERSION
        );
        assert!(verification(&case, &envelope(&case))["success"] == true);
        let mut extra = output(&case);
        extra["extra"] = json!(1);
        assert!(model_provider::check_output(&expected, &extra).is_err());
        assert_eq!(original, case.canonical["text"]["format"]["schema"]);
    }
    assert!(
        stats(&fixture_cases()[0].wire["output_config"]["format"]["schema"])["anyOf"]
            .as_u64()
            .unwrap()
            < stats(&write_models::w1_schema())["anyOf"].as_u64().unwrap()
    );
}

#[test]
fn union_branches_and_null_empty_false_zero_keep_their_meaning() {
    let schema = write_models::w1_schema();
    let basic = output(&fixture_cases()[0]);
    let branches = [
        json!({"kind":"date","date":"2026-10-10"}),
        json!({"kind":"range","start":"2026-10-10","end_inclusive":"2026-10-11"}),
        json!({"kind":"relative","unit":"day","offset":0}),
        json!({"kind":"month","month":10,"year_offset":0}),
        json!({"kind":"anchored","anchor_quote":{"text":"当天"},"unit":"day","offset":0}),
        json!({"kind":"recurring"}),
        json!({"kind":"unknown"}),
    ];
    let values = [
        Value::Null,
        json!({"kind":"text","text":"零","quote":{"text":"零"}}),
        json!({"kind":"integer","integer":0,"quote":{"text":"零"}}),
        json!({"kind":"boolean","boolean":false,"quote":{"text":"否"}}),
        json!({"kind":"entity","entity_id":"self","quote":{"text":"我"}}),
    ];
    let mut all = vec![basic.clone(), json!({"candidates":[],"memory_requests":[]})];
    for interp in branches {
        for value in &values {
            let mut v = basic.clone();
            v["candidates"][0]["value"] = value.clone();
            v["candidates"][0]["subject"] = json!({"unresolved":{"text":"他"}});
            v["candidates"][0]["times"] = json!([{"role":"validity","quote":{"text":"某时"},"anchor":"said_on","interp":interp}]);
            v["candidates"][0]["uncertainty"] = json!({"quote":{"text":"可能"},"resolved":false});
            assert!(model_provider::check_output(&schema, &v).is_ok());
            all.push(v);
        }
    }
    let mut invalid = basic.clone();
    invalid["candidates"][0]["sentence"]["text"] = json!(" \n\t");
    all.push(invalid);
    let mut invalid = basic.clone();
    invalid["candidates"][0]["value"] =
        json!({"kind":"boolean","boolean":false,"integer":0,"quote":{"text":"否"}});
    all.push(invalid);
    let mut invalid = basic.clone();
    invalid["candidates"][0]
        .as_object_mut()
        .unwrap()
        .remove("strength");
    all.push(invalid);
    same_acceptance(&schema, &all);
    let r4 = fixture_cases()
        .into_iter()
        .find(|c| c.id == "r4-stored")
        .unwrap();
    let mut bad = output(&r4);
    bad["facts"][0]["evidence"][0]["source"] = json!({"kind":"current"});
    assert!(model_provider::check_output(&r4.canonical["text"]["format"]["schema"], &bad).is_err());
    assert!(
        model_provider::check_output(
            &model_provider::expand_anthropic_simplified_schema(
                &r4.wire["output_config"]["format"]["schema"]
            )
            .unwrap(),
            &bad
        )
        .is_err()
    );
}

#[test]
fn constraints_are_not_flattened_away_and_references_fail_closed() {
    let schema = json!({"anyOf":[{"anyOf":[{"type":"string"},{"type":"null"}],"description":"keep"},{"type":"integer"}]});
    let wire = model_provider::anthropic_simplified_schema(&schema).unwrap();
    assert_eq!(
        model_provider::expand_anthropic_simplified_schema(&wire).unwrap(),
        schema
    );
    for bad in [
        json!({"$ref":"https://evil.example/schema"}),
        json!({"$ref":"#/definitions/missing"}),
        json!({"$ref":"#/definitions/a","type":"string","definitions":{"a":{"type":"string"}}}),
        json!({"$ref":"#/definitions/a","definitions":{"a":{"$ref":"#/definitions/a"}}}),
        json!({"type":"string","definitions":{"a":{"$ref":"#/definitions/b"},"b":{"$ref":"#/definitions/a"}}}),
        json!({"type":"string","definitions":{"unused":{"type":"string","minLength":1}}}),
    ] {
        assert!(
            model_provider::expand_anthropic_simplified_schema(&bad).is_err(),
            "{bad}"
        );
    }
}

#[test]
fn wire_bounds_may_pass_but_original_bounds_and_usage_still_stop_the_campaign() {
    let c = fixture_cases()[1].clone();
    let mut response = envelope(&c);
    let mut bad = output(&c);
    bad["reviews"][0]["candidate"] = json!(-1);
    response["content"][0]["text"] = json!(bad.to_string());
    let result = verification(&c, &response);
    assert_eq!(result["wire_schema"]["passed"], true);
    assert_eq!(result["original_schema"]["passed"], false);
    assert_eq!(result["success"], false);
    response = envelope(&c);
    response["usage"]["output_tokens"] = Value::Null;
    assert_eq!(verification(&c, &response)["success"], false);
    response = envelope(&c);
    response["stop_reason"] = json!("max_tokens");
    assert_eq!(verification(&c, &response)["success"], false);
    response["stop_reason"] = json!("refusal");
    assert_eq!(verification(&c, &response)["success"], false);
}

#[test]
fn production_rejects_rewrite_and_other_provider_bytes_and_bindings_are_unchanged() {
    let cfg = config();
    let c = fixture_cases()[0].clone();
    assert!(
        hippocampus::OpenAiClient::new_for_provider(
            "https://fixture.invalid",
            "fixture",
            cfg.provider.clone()
        )
        .is_err()
    );
    let mut native = cfg.provider.clone();
    native.structured_mode = StructuredMode::NativeSchema;
    assert!(native.structured_mode_binding().is_none());
    assert!(
        serde_json::to_value(&native)
            .unwrap()
            .get("structured_mode")
            .is_none()
    );
    let old = model_provider::provider_request(Some(&native), &c.canonical).unwrap();
    let expected = json!({"model":cfg.model,"system":c.canonical["instructions"],
        "messages":[{"role":"user","content":c.canonical["input"]}],"max_tokens":4096,
        "thinking":{"type":"adaptive"},"output_config":{"effort":"low","format":{"type":"json_schema",
        "schema":model_provider::anthropic_schema(&c.canonical["text"]["format"]["schema"]).unwrap()}}});
    assert_eq!(old.to_string(), expected.to_string());
    let mut gpt = cfg.provider.clone();
    gpt.structured_mode = StructuredMode::NativeSchema;
    gpt.protocol = ProviderProtocol::OpenaiResponses;
    gpt.thinking = None;
    let mut request = c.canonical.clone();
    request["model"] = json!("gpt-6-luna");
    let mut expected = request.clone();
    expected.as_object_mut().unwrap().remove("temperature");
    assert_eq!(
        model_provider::provider_request(Some(&gpt), &request)
            .unwrap()
            .to_string(),
        expected.to_string()
    );
    assert_eq!(
        model_provider::provider_request(None, &request).unwrap(),
        request
    );
    assert_eq!(model_provider::ADAPTER_VERSION, "structured-provider.v1");
    let binding = model_provider::anthropic_simplified_binding(&write_models::w1_schema()).unwrap();
    let mut changed = binding.clone();
    changed["rewrite_version"] = json!("future");
    assert_ne!(
        recall_models::fingerprint(&binding),
        recall_models::fingerprint(&changed)
    );
    changed = binding.clone();
    changed["original_schema"]["properties"]["memory_requests"]["items"]["properties"]["target"]
        ["properties"]["candidate_indices"]["items"]["minimum"] = json!(1);
    assert_ne!(
        recall_models::fingerprint(&binding),
        recall_models::fingerprint(&changed)
    );
}

#[test]
fn dry_is_read_only_and_warm_is_locked_until_six_successes() {
    let t = Temp::new();
    let cases = fixture_cases();
    let report = dry(&t.0, &cases, 1).unwrap();
    assert!(report.contains("待录制6"));
    assert!(!t.0.exists());
    let mut sent = 0;
    assert!(
        execute(&t.0, &cases, 2, &mut |_| {
            sent += 1;
            unreachable!()
        })
        .is_err()
    );
    assert_eq!(sent, 0);
    let cold = execute(&t.0, &cases, 1, &mut |c| {
        sent += 1;
        Ok(envelope(c))
    })
    .unwrap();
    assert_eq!(cold["sent_calls"], 6);
    assert!(cold_ready(&t.0, &cases).unwrap());
    let warm = execute(&t.0, &cases, 2, &mut |c| {
        sent += 1;
        Ok(envelope(c))
    })
    .unwrap();
    assert_eq!(warm["sent_calls"], 6);
    assert_eq!(warm["cumulative_sent"], 12);
    assert_eq!(sent, 12);
    let replay = execute(&t.0, &cases, 1, &mut |_| panic!("replay must not send")).unwrap();
    assert_eq!(replay["sent_calls"], 0);
    for c in &cases {
        assert_eq!(
            identity(c, 1)["request_fingerprint"],
            identity(c, 2)["request_fingerprint"]
        );
    }
    assert!(dry(&t.0, &cases, 2).unwrap().contains("待录制0"));
}

#[test]
fn failed_or_interrupted_slot_stops_without_resend_and_stale_bindings_fail() {
    let t = Temp::new();
    let cases = fixture_cases();
    let mut sent = 0;
    let report = execute(&t.0, &cases, 1, &mut |c| {
        sent += 1;
        let mut e = envelope(c);
        if sent == 2 {
            e["stop_reason"] = json!("max_tokens");
        }
        Ok(e)
    })
    .unwrap();
    assert_eq!(sent, 2);
    assert_eq!(report["cumulative_sent"], 2);
    assert_eq!(report["warm_unlocked"], false);
    assert!(
        execute(&t.0, &cases, 1, &mut |_| panic!(
            "failed slots never retried"
        ))
        .is_err()
    );
    let t = Temp::new();
    write(
        &t.0.join("campaign/authorization.json"),
        &authorization(&cases),
    )
    .unwrap();
    write(
        &t.0.join("campaign/sends/1-w1.json"),
        &json!({"state":"started","slot":identity(&cases[0],1)}),
    )
    .unwrap();
    assert_eq!(check_campaign(&t.0, &cases).unwrap(), 1);
    assert!(
        execute(&t.0, &cases, 1, &mut |_| panic!(
            "in-flight slot never retried"
        ))
        .is_err()
    );
    let mut changed = cases.clone();
    changed[0].binding["timeout_seconds"] = json!(361);
    assert!(check_campaign(&t.0, &changed).is_err());
    let t = Temp::new();
    execute(&t.0, &cases, 1, &mut |c| Ok(envelope(c))).unwrap();
    let mut record = read(&record_path(&t.0, &cases[0], 1)).unwrap().unwrap();
    record["binding"]["schema_binding"]["rewrite_version"] = json!("future");
    write(&record_path(&t.0, &cases[0], 1), &record).unwrap();
    assert!(check_campaign(&t.0, &cases).is_err());
}

#[test]
fn concurrent_executor_and_unknown_slots_cannot_spend_or_expand_allowance() {
    let t = Temp::new();
    let cases = fixture_cases();
    std::fs::create_dir_all(t.0.join("campaign")).unwrap();
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(t.0.join("campaign/lock"))
        .unwrap();
    lock.try_lock().unwrap();
    assert!(
        execute(&t.0, &cases, 1, &mut |_| panic!(
            "concurrent executor must not send"
        ))
        .is_err()
    );
    assert!(!t.0.join("campaign/sends").exists());
    drop(lock);
    write(
        &t.0.join("campaign/authorization.json"),
        &authorization(&cases),
    )
    .unwrap();
    write(
        &t.0.join("campaign/sends/unknown.json"),
        &json!({"state":"started"}),
    )
    .unwrap();
    assert!(
        execute(&t.0, &cases, 1, &mut |_| panic!(
            "unknown occupied slot must not be ignored"
        ))
        .is_err()
    );
    assert_eq!(
        read(&t.0.join("campaign/authorization.json"))
            .unwrap()
            .unwrap()["total_limit"],
        12
    );
}

#[test]
fn replay_rechecks_raw_response_instead_of_trusting_saved_success() {
    let t = Temp::new();
    let cases = fixture_cases();
    execute(&t.0, &cases, 1, &mut |c| Ok(envelope(c))).unwrap();
    let path = record_path(&t.0, &cases[0], 1);
    let mut record = read(&path).unwrap().unwrap();
    record["raw_response"]["content"][0]["text"] = json!("{}");
    assert_eq!(record["verification"]["success"], true);
    write(&path, &record).unwrap();
    assert!(
        execute(&t.0, &cases, 2, &mut |_| panic!(
            "tampered success must not unlock warm sends"
        ))
        .is_err()
    );
}

fn af238_cases() -> Vec<Case> {
    cases(&config_af238(), "https://fixture.invalid").unwrap()
}

#[test]
fn af238_only_removes_nonblank_patterns_and_binds_a_distinct_mode() {
    for (old, new) in fixture_cases().iter().zip(af238_cases()) {
        assert_eq!(old.canonical, new.canonical);
        let wire = &new.wire["output_config"]["format"]["schema"];
        assert_eq!(stats(wire)["patterns"], 0);
        assert_eq!(
            stats(wire)["refs"],
            stats(&old.wire["output_config"]["format"]["schema"])["refs"]
        );
        assert_eq!(
            new.binding["schema_binding"]["original_schema"],
            old.canonical["text"]["format"]["schema"]
        );
        assert_eq!(new.binding["schema_binding"]["wire_schema"], *wire);
        assert_eq!(
            new.binding["schema_binding"]["rewrite_version"],
            model_provider::ANTHROPIC_NOPATTERN_VERSION
        );
        assert_ne!(
            identity(old, 1)["request_fingerprint"],
            identity(&new, 1)["request_fingerprint"]
        );
        let cfg = config_af238();
        assert_eq!(
            cfg.provider.structured_mode_binding().unwrap(),
            json!({
            "mode":"native_simplified_nopattern","rewrite_version":model_provider::ANTHROPIC_NOPATTERN_VERSION})
        );
        assert!(
            hippocampus::OpenAiClient::new_for_provider(
                "https://fixture.invalid",
                "fixture",
                cfg.provider.clone()
            )
            .is_err()
        );
        let mut invalid = cfg.provider;
        invalid.protocol = ProviderProtocol::OpenaiResponses;
        assert!(invalid.validate().is_err());
        assert_eq!(
            verification(&new, &envelope(&new))["success"],
            true,
            "{}: {}",
            new.id,
            verification(&new, &envelope(&new))
        );
    }
    let schema = json!({"type":"object","properties":{
        "text":{"type":"string","pattern":r"^[\s\S]*\S[\s\S]*$","enum":["pattern"]},
        "code":{"type":"string","pattern":"^[A-Z]+$"}},"required":["text","code"],"additionalProperties":false});
    let wire = model_provider::anthropic_simplified_nopattern_schema(&schema).unwrap();
    assert!(wire["properties"]["text"].get("pattern").is_none());
    assert_eq!(
        wire["properties"]["text"]["enum"],
        schema["properties"]["text"]["enum"]
    );
    assert_eq!(wire["properties"]["code"]["pattern"], "^[A-Z]+$");
}

#[test]
fn af238_wire_allows_blanks_but_canonical_and_stage_reject_them() {
    for c in af238_cases() {
        let mut out = output(&c);
        match c.id {
            "w1" => out["candidates"][0]["sentence"]["text"] = json!(" \n\t"),
            "w3" => {
                out["reviews"][0]["verdict"] = json!("insufficient");
                out["reviews"][0]["problems"] =
                    json!([{"aspect":"subject","quote":{"text":" \n\t"}}]);
            }
            "w4" => out["decisions"][0]["basis"][0]["quote"]["text"] = json!(" \n\t"),
            "r1" => out["subject"]["entity_id"] = json!(" \n\t"),
            _ => out["facts"][0]["statement"] = json!(" \n\t"),
        }
        let mut e = envelope(&c);
        e["content"][0]["text"] = json!(out.to_string());
        let v = verification(&c, &e);
        assert_eq!(v["wire_schema"]["passed"], true, "{}: {v}", c.id);
        assert_eq!(v["original_schema"]["passed"], false, "{}: {v}", c.id);
        assert_eq!(v["stage_validation"]["passed"], false, "{}: {v}", c.id);
        assert_eq!(v["success"], false);
    }
}

#[test]
fn af238_schema_success_cannot_hide_stage_reference_or_identity_failures() {
    for c in af238_cases() {
        let mut out = output(&c);
        match c.id {
            "w1" => out["candidates"][0]["sentence"]["text"] = json!("不存在的引用"),
            "w3" => out["reviews"][0]["candidate"] = json!(99),
            "w4" => out["decisions"][0]["candidate"] = json!(99),
            "r1" => out["subject"]["entity_id"] = json!("unknown-id"),
            _ => out["facts"][0]["evidence"][0]["quote"]["text"] = json!("不存在的引用"),
        }
        let mut e = envelope(&c);
        e["content"][0]["text"] = json!(out.to_string());
        let v = verification(&c, &e);
        assert_eq!(v["wire_schema"]["passed"], true, "{}: {v}", c.id);
        assert_eq!(v["original_schema"]["passed"], true, "{}: {v}", c.id);
        assert_eq!(v["stage_validation"]["passed"], false, "{}: {v}", c.id);
        assert_eq!(v["success"], false);
    }
    let t = Temp::new();
    let cases = af238_cases();
    let mut sent = 0;
    let report = execute(&t.0, &cases, 1, &mut |c| {
        sent += 1;
        let mut e = envelope(c);
        let mut out = output(c);
        out["candidates"][0]["sentence"]["text"] = json!("不存在的引用");
        e["content"][0]["text"] = json!(out.to_string());
        Ok(e)
    })
    .unwrap();
    assert_eq!(sent, 1);
    assert_eq!(report["warm_unlocked"], false);
    assert!(execute(&t.0, &cases, 1, &mut |_| panic!("failure is terminal")).is_err());
}

#[test]
fn af238_freezes_six_cold_and_five_warm_slots_with_strict_replay() {
    let t = Temp::new();
    let cases = af238_cases();
    assert!(dry(&t.0, &cases, 1).unwrap().contains("总额度11=冷6+暖5"));
    assert!(!t.0.exists());
    assert!(
        execute(&t.0, &cases, 3, &mut |_| panic!(
            "round outside the frozen manifest"
        ))
        .is_err()
    );
    assert!(!t.0.exists());
    assert!(execute(&t.0, &cases, 2, &mut |_| panic!("warm is locked")).is_err());
    let cold = execute(&t.0, &cases, 1, &mut |c| Ok(envelope(c))).unwrap();
    assert_eq!(cold["sent_calls"], 6);
    let warm = execute(&t.0, &cases, 2, &mut |c| Ok(envelope(c))).unwrap();
    assert_eq!(warm["sent_calls"], 5);
    assert_eq!(warm["cumulative_sent"], 11);
    assert_eq!(warm["results"].as_array().unwrap().len(), 5);
    assert!(!record_path(&t.0, &cases[1], 2).exists());
    let auth = read(&t.0.join("campaign/authorization.json"))
        .unwrap()
        .unwrap();
    assert_eq!(auth["slots"].as_array().unwrap().len(), 11);
    assert_eq!(auth["total_limit"], 11);
    assert_eq!(
        execute(&t.0, &cases, 2, &mut |_| panic!("no resend")).unwrap()["sent_calls"],
        0
    );
    assert!(check_campaign(&t.0, &fixture_cases()).is_err());
    write(
        &t.0.join("campaign/sends/2-w3.json"),
        &json!({"state":"started","slot":identity(&cases[1],2)}),
    )
    .unwrap();
    assert!(check_campaign(&t.0, &cases).is_err());
}

#[test]
fn af238_leaves_other_provider_request_bytes_unchanged() {
    let c = af238_cases()[0].clone();
    let mut cfg = config_af238().provider;
    cfg.structured_mode = StructuredMode::NativeSchema;
    cfg.protocol = ProviderProtocol::OpenaiChat;
    cfg.model_revision = Some("fixture-local".into());
    cfg.context_tokens = Some(65536);
    cfg.context_proof = Some(model_provider::ContextProof {
        tokenizer_revision: "fixture-tokenizer".into(),
        template_token_bound: 4096,
        byte_level_bpe: true,
    });
    cfg.thinking = Some(false);
    let mut canonical = c.canonical;
    canonical["model"] = json!("qwen3.8:27b-q4_K_M-ctx64k");
    canonical.as_object_mut().unwrap().remove("reasoning");
    let expected = json!({"model":canonical["model"],"messages":[
        {"role":"system","content":canonical["instructions"]},{"role":"user","content":canonical["input"]}],
        "max_tokens":4096,"temperature":0,"stream":false,"reasoning_effort":"none",
        "response_format":{"type":"json_schema","json_schema":{"name":canonical["text"]["format"]["name"],
            "strict":true,"schema":canonical["text"]["format"]["schema"]}}});
    assert_eq!(
        model_provider::provider_request(Some(&cfg), &canonical)
            .unwrap()
            .to_string(),
        expected.to_string()
    );
    assert_eq!(model_provider::ADAPTER_VERSION, "structured-provider.v1");
}

fn af240_cases() -> Vec<Case> {
    cases(&config_af240(), "https://fixture.invalid").unwrap()
}

#[test]
fn af240_freezes_only_three_recall_slots_and_never_runs_a_second_round() {
    let t = Temp::new();
    let cases = af240_cases();
    assert_eq!(
        cases.iter().map(|c| c.id).collect::<Vec<_>>(),
        vec!["r1", "r4-stored", "r4-current"]
    );
    for (c, old) in cases.iter().zip(af238_cases().iter().skip(3)) {
        assert_eq!(c.canonical, old.canonical);
        assert_eq!(c.wire, old.wire);
        assert_eq!(c.binding, old.binding);
        assert_ne!(identity(c, 1), identity(old, 1));
    }
    let plan = authorization(&cases);
    assert_eq!(plan["version"], "af240.campaign@1");
    assert_eq!(plan["total_limit"], 3);
    assert_eq!(plan["cold_limit"], 3);
    assert_eq!(plan["warm_limit"], 0);
    assert_eq!(plan["slots"].as_array().unwrap().len(), 3);
    assert!(
        dry(&t.0, &cases, 1)
            .unwrap()
            .contains("计划3：r1 → r4-stored → r4-current")
    );
    assert!(!t.0.exists());
    assert!(dry(&t.0, &cases, 2).is_err());
    assert!(execute(&t.0, &cases, 2, &mut |_| panic!("no warm authorization")).is_err());
    assert!(!t.0.exists());
    let mut sent = Vec::new();
    let report = execute(&t.0, &cases, 1, &mut |c| {
        assert_eq!(
            read(&t.0.join("campaign/authorization.json"))
                .unwrap()
                .unwrap(),
            plan
        );
        assert_eq!(
            read(&t.0.join(format!("campaign/sends/1-{}.json", c.id)))
                .unwrap()
                .unwrap()["state"],
            "started"
        );
        sent.push(c.id);
        Ok(envelope(c))
    })
    .unwrap();
    assert_eq!(sent, vec!["r1", "r4-stored", "r4-current"]);
    assert_eq!(report["sent_calls"], 3);
    assert_eq!(report["cumulative_sent"], 3);
    assert_eq!(report["warm_unlocked"], false);
    assert_eq!(
        execute(&t.0, &cases, 1, &mut |_| panic!(
            "strict replay must not resend"
        ))
        .unwrap()["sent_calls"],
        0
    );
    let mut changed = cases.clone();
    changed[0].binding["timeout_seconds"] = json!(361);
    assert!(check_campaign(&t.0, &changed).is_err());
    assert!(check_campaign(&t.0, &af238_cases()).is_err());
    write(
        &t.0.join("campaign/sends/1-w1.json"),
        &json!({"state":"started"}),
    )
    .unwrap();
    assert!(check_campaign(&t.0, &cases).is_err());
}

#[test]
fn af240_stops_at_each_failure_and_never_retries_an_inflight_send() {
    let cases = af240_cases();
    for fail_at in 1..=3 {
        let t = Temp::new();
        let mut sent = Vec::new();
        let report = execute(&t.0, &cases, 1, &mut |c| {
            sent.push(c.id);
            if sent.len() == fail_at {
                Ok(json!({"http_status":400,"error":{"code":"schema_grammar_too_large"}}))
            } else {
                Ok(envelope(c))
            }
        })
        .unwrap();
        assert_eq!(
            sent,
            cases.iter().take(fail_at).map(|c| c.id).collect::<Vec<_>>()
        );
        assert_eq!(report["sent_calls"], fail_at);
        assert_eq!(
            report["results"][fail_at - 1]["verification"]["success"],
            false
        );
        assert!(execute(&t.0, &cases, 1, &mut |_| panic!("failure is terminal")).is_err());
        assert_eq!(check_campaign(&t.0, &cases).unwrap(), fail_at);
    }
    let t = Temp::new();
    write(
        &t.0.join("campaign/authorization.json"),
        &authorization(&cases),
    )
    .unwrap();
    write(
        &t.0.join("campaign/sends/1-r1.json"),
        &json!({"state":"started","slot":identity(&cases[0],1)}),
    )
    .unwrap();
    assert!(execute(&t.0, &cases, 1, &mut |_| panic!("inflight is never resent")).is_err());
    assert_eq!(check_campaign(&t.0, &cases).unwrap(), 1);
    assert_eq!(
        read(&t.0.join("campaign/authorization.json"))
            .unwrap()
            .unwrap()["total_limit"],
        3
    );
}
