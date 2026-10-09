//! ④-2：完整批次、W3 绑定、控制请求和真实 SQLite 关闭重开。
use hippocampus::chat::{ChatConfig, ChatMemoryService, EnrichmentMode};
use hippocampus::contract::*;
use hippocampus::enrichment::*;
use hippocampus::*;
use serde_json::{Value as Json, json};
use std::path::PathBuf;

struct Db(PathBuf);
impl Db {
    fn new() -> Self {
        Self(
            std::env::temp_dir()
                .join(format!("enrichment-batch-{}", diagnostics::new_id()))
                .join("memory.sqlite3"),
        )
    }
    fn open(&self) -> SqliteMemoryStore {
        SqliteMemoryStore::open(&self.0, UserId::from("alice")).unwrap()
    }
    fn start(&self, turn: &str, text: &str) -> String {
        let mut service = ChatMemoryService::new(
            self.open(),
            ChatConfig {
                enrichment_mode: EnrichmentMode::Off,
                ..Default::default()
            },
        )
        .unwrap();
        let out = service.call("memory_remember", json!({"text":text,"session_id":"batch","turn_id":turn,"capture_source":true,"observed_at":1790899200}), || false).unwrap();
        let state = self.open().read().unwrap();
        let key = state
            .sources
            .iter()
            .find(|(_, s)| s.id == out["source"]["source_id"].as_str().unwrap())
            .unwrap()
            .0
            .clone();
        w4_start_round(&mut self.open(), &key, turn, false).unwrap();
        key
    }
    fn memories(&self) -> Vec<Memory> {
        self.open()
            .read()
            .unwrap()
            .repository
            .list_by_owner(&UserId::from("alice"))
            .unwrap()
    }
    fn forget(&self, key: &str) {
        let id = self.open().read().unwrap().sources[key].id.clone();
        ChatMemoryService::new(self.open(), ChatConfig::default())
            .unwrap()
            .call("memory_forget", json!({"source_id":id}), || false)
            .unwrap();
    }
}
impl Drop for Db {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.0.parent().unwrap());
    }
}

fn candidate(text: &str) -> Candidate {
    serde_json::from_value(json!({"kind":"assertion","speech":"user_statement","subject":{"entity_id":"self"},"relation":"fact","memory_type":"fact",
        "value":{"kind":"text","text":text,"quote":{"text":text}},"sentence":{"text":text},"polarity":"affirm","strength":null,"uncertainty":null,"conditions":[],"times":[]})).unwrap()
}
fn request(
    kind: RequestKind,
    quote: &str,
    object: RequestObject,
    indices: Vec<usize>,
) -> MemoryRequest {
    MemoryRequest {
        kind,
        quote: Quote { text: quote.into() },
        target: RequestTarget {
            object,
            candidate_indices: indices,
            quote: (object == RequestObject::Unknown).then(|| Quote { text: quote.into() }),
        },
        deadline: (kind == RequestKind::Retention).then(|| TimeInterp {
            role: TimeRole::Retention,
            quote: Quote {
                text: "明天".into(),
            },
            interp: Interp::Relative {
                unit: TimeUnit::Day,
                offset: 1,
            },
            anchor: TimeAnchor::SaidOn,
        }),
    }
}
fn w2_batch(
    ctx: &ProviderContext,
    input: &SourceInput,
    candidates: Vec<Candidate>,
    memory_requests: Vec<MemoryRequest>,
) -> w2::Batch {
    let config = ChatConfig::default();
    let catalog = w2::relation_catalog(&config.predicates, &config.memory_types).unwrap();
    w2::validate(
        W1Output {
            candidates,
            memory_requests,
        },
        &w2::Context {
            text: &input.text,
            entities: &[EntityId::from("self")],
            relations: &catalog,
            said_on: Some("2026-10-02"),
            utc_offset_seconds: 28_800,
            task_id: ctx.task_id.as_ref(),
        },
    )
    .unwrap()
}
fn reviewed(
    ctx: &ProviderContext,
    input: &SourceInput,
    w2: w2::Batch,
    edit: impl FnOnce(&mut W3Output),
) -> UpstreamBatch {
    let mut output = W3Output {
        reviews: w2
            .candidates
            .iter()
            .filter(|c| c.status != w2::Status::Rejected)
            .map(|c| CandidateReview {
                candidate: c.index,
                verdict: Verdict::Supported,
                problems: vec![],
                episode_value: if c.candidate.memory_type == "episode" {
                    EpisodeValue::Unknown
                } else {
                    EpisodeValue::NotApplicable
                },
            })
            .collect(),
        request_reviews: w2
            .memory_requests
            .iter()
            .map(|r| RequestReview {
                request: r.index,
                verdict: Verdict::Supported,
            })
            .collect(),
        coverage: W3Coverage {
            status: CoverageStatus::Complete,
            missing: vec![],
        },
    };
    edit(&mut output);
    let review = (!w2.candidates.is_empty()).then(|| {
        output
            .validate(&w2, &ctx.source_binding, &input.text)
            .unwrap()
    });
    UpstreamBatch { w2, review }
}
fn new_output(input: &W4Input) -> W4Output {
    W4Output {
        decisions: input
            .candidates
            .iter()
            .map(|c| W4Decision {
                candidate: c.index,
                status: DecisionStatus::Decided,
                action: Some(Action::New),
                targets: vec![],
                resolves_relations: vec![],
                basis: vec![EvidenceRef {
                    source: c.source.clone(),
                    quote: c.candidate.sentence.clone(),
                }],
                effective: None,
                relation_scope: None,
                pending_reason: None,
            })
            .collect(),
    }
}
fn run(
    db: &Db,
    key: &str,
    upstream: impl FnMut(&ProviderContext, &SourceInput) -> PersistenceResult<UpstreamBatch>,
) -> Json {
    run_source(
        &mut db.open(),
        key,
        RuntimeInputBudget::default(),
        upstream,
        |_, input, _| Ok(Some(new_output(input))),
    )
    .unwrap()
}
fn seed(db: &Db, turn: &str, text: &str) -> String {
    let key = db.start(turn, text);
    run(db, &key, |ctx, input| {
        Ok(reviewed(
            ctx,
            input,
            w2_batch(ctx, input, vec![candidate(text)], vec![]),
            |_| {},
        ))
    });
    key
}

#[test]
fn full_batch_persists_all_w2_w3_results_and_runtime_bindings() {
    let db = Db::new();
    let key = db.start(
        "mixed",
        "我养猫。他喜欢茶。我住北京。我喜欢雨天。我用铅笔。",
    );
    let out = run(&db, &key, |ctx, input| {
        let mut bad = candidate("我养猫。");
        bad.relation = "invented".into();
        let mut unresolved = candidate("他喜欢茶。");
        unresolved.subject = Subject::Unresolved(UnresolvedSubject {
            unresolved: Quote { text: "他".into() },
        });
        let w2 = w2_batch(
            ctx,
            input,
            vec![
                bad,
                unresolved,
                candidate("我住北京。"),
                candidate("我喜欢雨天。"),
                candidate("我用铅笔。"),
            ],
            vec![],
        );
        Ok(reviewed(ctx, input, w2, |w3| {
            w3.reviews[1].verdict = Verdict::Contradicted;
            w3.reviews[1].problems.push(ReviewProblem {
                aspect: ReviewAspect::Value,
                quote: Quote {
                    text: "北京".into(),
                },
            });
            w3.reviews[2].verdict = Verdict::Insufficient;
        }))
    });
    assert_eq!(out["enrichment"]["status"], "pending");
    let cs = &out["enrichment"]["candidates"];
    assert_eq!(
        cs[0]["reasons"],
        json!(["candidate_rejected:unknown_relation"])
    );
    assert_eq!(cs[1]["status"], "unresolved");
    assert_eq!(cs[1]["reasons"], json!(["unresolved_subject"]));
    assert_eq!(cs[2]["status"], "rejected");
    assert_eq!(cs[2]["reasons"], json!(["review_contradicted"]));
    assert_eq!(cs[3]["status"], "pending");
    assert_eq!(cs[3]["reasons"], json!(["review_insufficient"]));
    assert_eq!(cs[4]["status"], "committed");
    for c in cs.as_array().unwrap().iter().take(4) {
        assert!(c["update"].is_null());
        assert!(c["memory_id"].is_null());
    }
    let state = db.open().read().unwrap();
    let result = state.sources[&key].result.as_ref().unwrap();
    assert_eq!(
        result["w4_batch"]["batch"]["w2"]["candidates"]
            .as_array()
            .unwrap()
            .len(),
        5
    );
    assert_eq!(
        result["w4_batch"]["batch"]["review"]["output"]["reviews"][1]["problems"][0]["quote"]["text"],
        "北京"
    );
    assert_eq!(state.w5_receipts.len(), 1);
    let receipt = &state.w5_receipts[&format!("{key}#0#4")];
    let group: Json = serde_json::from_str(&receipt.fingerprint).unwrap();
    assert_eq!(group["candidates"][0]["index"], 4);
    assert_eq!(group["candidates"][0]["committed_id"], Json::Null);
    assert_eq!(db.memories().len(), 1);
    assert_eq!(
        hippocampus::enrichment::receipt(&db.open(), &key).unwrap(),
        out
    );
}

#[test]
fn w3_schema_and_mechanical_checks_reject_invalid_reviews() {
    let db = Db::new();
    let key = db.start("w3", "我养猫。记住这句。");
    let out = run(&db, &key, |ctx, input| {
        let batch = reviewed(
            ctx,
            input,
            w2_batch(
                ctx,
                input,
                vec![candidate("我养猫。")],
                vec![request(
                    RequestKind::Remember,
                    "记住这句",
                    RequestObject::Facts,
                    vec![0],
                )],
            ),
            |_| {},
        );
        let output = batch.review.as_ref().unwrap().output().clone();
        let mut wire = serde_json::to_value(&output).unwrap();
        wire["extra"] = json!(true);
        assert!(W3Output::decode(&wire.to_string()).is_err());
        let mut wire = serde_json::to_value(&output).unwrap();
        wire["reviews"][0]
            .as_object_mut()
            .unwrap()
            .remove("episode_value");
        assert!(W3Output::decode(&wire.to_string()).is_err());
        for variant in 0..9 {
            let mut bad = output.clone();
            match variant {
                0 => bad.reviews.clear(),
                1 => bad.reviews.push(bad.reviews[0].clone()),
                2 => bad.reviews[0].candidate = 99,
                3 => bad.request_reviews.clear(),
                4 => bad.request_reviews.push(bad.request_reviews[0].clone()),
                5 => bad.request_reviews[0].request = 99,
                6 => bad.reviews[0].problems.push(ReviewProblem {
                    aspect: ReviewAspect::Value,
                    quote: Quote { text: "猫".into() },
                }),
                7 => {
                    bad.reviews[0].verdict = Verdict::Insufficient;
                    bad.reviews[0].problems.push(ReviewProblem {
                        aspect: ReviewAspect::Subject,
                        quote: Quote {
                            text: "不存在".into(),
                        },
                    });
                }
                _ => bad.coverage.missing.push(Quote {
                    text: "历史里有，但本条没有".into(),
                }),
            }
            assert!(
                bad.validate(&batch.w2, &ctx.source_binding, &input.text)
                    .is_err(),
                "variant {variant}"
            );
        }
        Ok(batch)
    });
    assert_eq!(out["enrichment"]["status"], "committed");
    assert_eq!(
        hippocampus::enrichment::receipt(&db.open(), &key).unwrap(),
        out
    );
}

#[test]
fn changed_candidate_request_or_source_cannot_reuse_w3_binding() {
    for change in ["candidate", "request", "source"] {
        let db = Db::new();
        let key = db.start(change, "我养猫。我养狗。记住猫。");
        let out = run(&db, &key, |ctx, input| {
            let mut batch = reviewed(
                ctx,
                input,
                w2_batch(
                    ctx,
                    input,
                    vec![candidate("我养猫。")],
                    vec![request(
                        RequestKind::Remember,
                        "记住猫",
                        RequestObject::Facts,
                        vec![0],
                    )],
                ),
                |_| {},
            );
            match change {
                "candidate" => batch.w2.candidates[0].candidate = candidate("我养狗。"),
                "request" => batch.w2.memory_requests[0].request.quote.text = "猫".into(),
                _ => {
                    let mut stored = serde_json::to_value(&batch).unwrap();
                    stored["review"]["binding"][1][0] = json!("other-source");
                    batch = serde_json::from_value(stored).unwrap();
                }
            }
            Ok(batch)
        });
        assert_eq!(out["enrichment"]["status"], "failed");
        let state = db.open().read().unwrap();
        assert_eq!(
            state.sources[&key].error.as_deref(),
            Some("model_invalid_output")
        );
        assert!(state.w5_receipts.is_empty());
        assert!(db.memories().is_empty());
        assert!(state.sources[&key].request.is_some());
    }
}

#[test]
fn coverage_gaps_survives_finish_reopen_and_rejudge() {
    let db = Db::new();
    let key = db.start("gaps", "我养猫。我也养鱼。");
    let out = run(&db, &key, |ctx, input| {
        Ok(reviewed(
            ctx,
            input,
            w2_batch(ctx, input, vec![candidate("我养猫。")], vec![]),
            |w3| {
                w3.coverage.status = CoverageStatus::Gaps;
                w3.coverage.missing.push(Quote {
                    text: "我也养鱼。".into(),
                });
            },
        ))
    });
    assert_eq!(out["enrichment"]["status"], "coverage_gaps");
    assert_eq!(out["enrichment"]["candidates"][0]["status"], "committed");
    let id = out["enrichment"]["candidates"][0]["memory_id"].clone();
    w4_start_round(&mut db.open(), &key, "rejudge", true).unwrap();
    let out = run(&db, &key, |_, _| panic!("重判复用已核验批次，不再提取"));
    assert_eq!(out["enrichment"]["status"], "coverage_gaps");
    assert_eq!(out["enrichment"]["candidates"][0]["memory_id"], id);
    let state = db.open().read().unwrap();
    assert_eq!(
        state.sources[&key].result.as_ref().unwrap()["w4_summary"]["status"],
        "coverage_gaps"
    );
    assert_eq!(
        state.sources[&key].result.as_ref().unwrap()["w4_batch"]["batch"]["review"]["output"]["coverage"]
            ["missing"][0]["text"],
        "我也养鱼。"
    );
    assert_eq!(db.memories().len(), 1);
}

#[test]
fn empty_w1_does_not_review_requests_or_claim_no_facts() {
    let db = Db::new();
    let key = db.start("empty", "早上好。记住这句。");
    let out = run(&db, &key, |ctx, input| {
        let w2 = w2_batch(
            ctx,
            input,
            vec![],
            vec![request(
                RequestKind::Remember,
                "记住这句",
                RequestObject::Source,
                vec![],
            )],
        );
        Ok(UpstreamBatch { w2, review: None })
    });
    assert_eq!(out["enrichment"]["status"], "no_candidates_unreviewed");
    assert_eq!(
        out["enrichment"]["memory_requests"][0]["status"],
        "unreviewed"
    );
    assert!(out["enrichment"]["memory_requests"][0]["verdict"].is_null());
    assert!(!out["enrichment"]["retention_pending"].as_bool().unwrap());
    assert!(db.memories().is_empty());
    assert_eq!(
        hippocampus::enrichment::receipt(&db.open(), &key).unwrap(),
        out
    );
}

#[test]
fn all_filtered_is_pending_and_never_completed_no_facts() {
    for rejected in [false, true] {
        let db = Db::new();
        let key = db.start("filtered", "我养猫。");
        let out = run(&db, &key, |ctx, input| {
            let mut c = candidate("我养猫。");
            if rejected {
                c.relation = "invented".into();
            }
            Ok(reviewed(
                ctx,
                input,
                w2_batch(ctx, input, vec![c], vec![]),
                |w3| {
                    for r in &mut w3.reviews {
                        r.verdict = Verdict::Insufficient;
                    }
                },
            ))
        });
        assert_eq!(out["enrichment"]["status"], "pending");
        assert!(db.memories().is_empty());
        assert_eq!(
            out["enrichment"]["candidates"][0]["status"],
            if rejected { "rejected" } else { "pending" }
        );
        assert_eq!(
            hippocampus::enrichment::receipt(&db.open(), &key).unwrap(),
            out
        );
    }
}

#[test]
fn supported_retention_reports_object_deadline_and_not_executed() {
    for object in [RequestObject::Facts, RequestObject::Source] {
        let db = Db::new();
        let key = db.start("retention", "我养猫。只保留到明天。");
        let out = run(&db, &key, |ctx, input| {
            Ok(reviewed(
                ctx,
                input,
                w2_batch(
                    ctx,
                    input,
                    vec![candidate("我养猫。")],
                    vec![request(
                        RequestKind::Retention,
                        "只保留到明天",
                        object,
                        if object == RequestObject::Facts {
                            vec![0]
                        } else {
                            vec![]
                        },
                    )],
                ),
                |_| {},
            ))
        });
        assert_eq!(out["enrichment"]["status"], "committed");
        assert_eq!(out["enrichment"]["retention_pending"], true);
        let r = &out["enrichment"]["memory_requests"][0];
        assert_eq!(r["status"], "retention_pending");
        assert_eq!(r["executed"], false);
        assert_eq!(r["deadline"]["quote"]["text"], "明天");
        assert!(r["resolved_target"]["source_id"].is_string());
        assert_eq!(db.memories()[0].temporal.expires_at, None);
        assert_eq!(
            hippocampus::enrichment::receipt(&db.open(), &key).unwrap(),
            out
        );
    }
}

#[test]
fn known_control_target_blocks_only_bound_candidate_without_deleting() {
    for kind in [
        RequestKind::DoNotStore,
        RequestKind::Forget,
        RequestKind::Retention,
    ] {
        let db = Db::new();
        let key = db.start("control", "我养猫。我养狗。猫这句不要保存到明天。");
        let out = run(&db, &key, |ctx, input| {
            Ok(reviewed(
                ctx,
                input,
                w2_batch(
                    ctx,
                    input,
                    vec![candidate("我养猫。"), candidate("我养狗。")],
                    vec![request(
                        kind,
                        "猫这句不要保存到明天",
                        RequestObject::Facts,
                        vec![0],
                    )],
                ),
                |w3| {
                    w3.request_reviews[0].verdict = Verdict::Insufficient;
                },
            ))
        });
        assert_eq!(out["enrichment"]["status"], "awaiting_confirmation");
        assert_eq!(
            out["enrichment"]["candidates"][0]["reasons"],
            json!(["awaiting_confirmation"])
        );
        assert_eq!(out["enrichment"]["candidates"][1]["status"], "committed");
        assert_eq!(db.memories().len(), 1);
        assert_eq!(db.memories()[0].value, MemoryValue::Text("我养狗。".into()));
        assert!(db.open().read().unwrap().sources[&key].request.is_some());
        assert_eq!(
            hippocampus::enrichment::receipt(&db.open(), &key).unwrap(),
            out
        );
    }
}

#[test]
fn unknown_old_or_source_control_target_pauses_whole_source() {
    for object in [
        RequestObject::Unknown,
        RequestObject::Facts,
        RequestObject::Source,
    ] {
        let db = Db::new();
        let key = db.start("unknown", "我养猫。我养狗。删掉之前那条。");
        let out = run(&db, &key, |ctx, input| {
            let mut r = request(RequestKind::Forget, "删掉之前那条", object, vec![]);
            if object == RequestObject::Facts {
                r.target.quote = Some(Quote {
                    text: "之前那条".into(),
                });
            }
            Ok(reviewed(
                ctx,
                input,
                w2_batch(
                    ctx,
                    input,
                    vec![candidate("我养猫。"), candidate("我养狗。")],
                    vec![r],
                ),
                |_| {},
            ))
        });
        assert_eq!(out["enrichment"]["status"], "awaiting_confirmation");
        assert!(db.memories().is_empty());
        for c in out["enrichment"]["candidates"].as_array().unwrap() {
            assert_eq!(c["reasons"], json!(["awaiting_confirmation"]));
        }
        assert_eq!(out["enrichment"]["memory_requests"][0]["executed"], false);
        assert_eq!(
            hippocampus::enrichment::receipt(&db.open(), &key).unwrap(),
            out
        );
    }
}

#[test]
fn empty_w1_control_requests_still_pause_and_unknown_retention_is_not_approved() {
    for empty in [false, true] {
        let db = Db::new();
        let key = db.start("limit", "我养猫。这个只保留到明天。");
        let out = run(&db, &key, |ctx, input| {
            let w2 = w2_batch(
                ctx,
                input,
                if empty {
                    vec![]
                } else {
                    vec![candidate("我养猫。")]
                },
                vec![request(
                    RequestKind::Retention,
                    "这个只保留到明天",
                    RequestObject::Unknown,
                    vec![],
                )],
            );
            Ok(reviewed(ctx, input, w2, |_| {}))
        });
        assert_eq!(out["enrichment"]["status"], "awaiting_confirmation");
        assert_eq!(out["enrichment"]["retention_pending"], false);
        assert!(out["enrichment"]["memory_requests"][0]["resolved_target"].is_null());
        assert!(db.memories().is_empty());
    }
}

#[test]
fn control_pause_is_sticky_across_rejudge_and_new_model_verdict() {
    let db = Db::new();
    let key = db.start("sticky", "我养猫。只保留到明天。");
    let out = run(&db, &key, |ctx, input| {
        Ok(reviewed(
            ctx,
            input,
            w2_batch(
                ctx,
                input,
                vec![candidate("我养猫。")],
                vec![request(
                    RequestKind::Retention,
                    "只保留到明天",
                    RequestObject::Facts,
                    vec![0],
                )],
            ),
            |w3| {
                w3.request_reviews[0].verdict = Verdict::Insufficient;
            },
        ))
    });
    assert_eq!(out["enrichment"]["status"], "awaiting_confirmation");
    // 改变必要历史，要求重核验 W3，但不允许重提取或由新核验解除控制标记。
    db.open()
        .transaction(|s| {
            s.sources.get_mut(&key).unwrap().request.as_mut().unwrap()["history"] =
                json!(["指代背景更新"]);
            Ok(())
        })
        .unwrap();
    w4_start_round(&mut db.open(), &key, "new-review", true).unwrap();
    let mut reviewed_again = false;
    let out = run(&db, &key, |ctx, input| {
        reviewed_again = true;
        let previous = ctx.previous_w2.clone().expect("复用最终候选与请求");
        Ok(reviewed(ctx, input, previous, |_| {}))
    });
    assert!(reviewed_again);
    assert_eq!(out["enrichment"]["status"], "awaiting_confirmation");
    assert_eq!(
        out["enrichment"]["memory_requests"][0]["status"],
        "awaiting_confirmation"
    );
    assert!(db.memories().is_empty());
    assert_eq!(
        hippocampus::enrichment::receipt(&db.open(), &key).unwrap(),
        out
    );
}

#[test]
fn remember_uses_final_mapping_and_episode_unknown_is_not_useful() {
    let db = Db::new();
    let key = db.start(
        "remember",
        "我养猫。我今天散步。我今天喝茶。我今天画画。记住散步。",
    );
    let out = run(&db, &key, |ctx, input| {
        let mut bad = candidate("我养猫。");
        bad.relation = "invented".into();
        let mut cs = vec![
            bad,
            candidate("我今天散步。"),
            candidate("我今天喝茶。"),
            candidate("我今天画画。"),
        ];
        for c in cs.iter_mut().skip(1) {
            c.memory_type = "episode".into();
        }
        let w2 = w2_batch(
            ctx,
            input,
            cs,
            vec![request(
                RequestKind::Remember,
                "记住散步",
                RequestObject::Facts,
                vec![1],
            )],
        );
        assert_eq!(w2.index_map, vec![None, Some(0), Some(1), Some(2)]);
        Ok(reviewed(ctx, input, w2, |w3| {
            w3.reviews[1].episode_value = EpisodeValue::LowValue;
            w3.reviews[2].episode_value = EpisodeValue::Useful;
        }))
    });
    assert_eq!(out["enrichment"]["candidates"][1]["status"], "committed");
    assert_eq!(out["enrichment"]["candidates"][2]["status"], "pending");
    assert_eq!(out["enrichment"]["candidates"][3]["status"], "committed");
    assert_eq!(
        out["enrichment"]["memory_requests"][0]["final_candidate_indices"],
        json!([0])
    );
    let memories = db.memories();
    assert_eq!(memories.len(), 2);
    assert!(
        memories
            .iter()
            .find(|m| m.value == MemoryValue::Text("我今天散步。".into()))
            .unwrap()
            .explicit_memory_request
    );
    assert!(
        !memories
            .iter()
            .find(|m| m.value == MemoryValue::Text("我今天画画。".into()))
            .unwrap()
            .explicit_memory_request
    );
    let state = db.open().read().unwrap();
    let admissions = &state.sources[&key].result.as_ref().unwrap()["w4_input_meta"]["admissions"];
    assert_eq!(
        admissions["1"],
        json!({"episode_useful":false,"remember_bound":true})
    );
    assert_eq!(
        admissions["3"],
        json!({"episode_useful":true,"remember_bound":false})
    );
    assert_eq!(
        hippocampus::enrichment::receipt(&db.open(), &key).unwrap(),
        out
    );
}

#[test]
fn rejected_remember_target_never_widens_to_other_candidates() {
    let db = Db::new();
    let key = db.start("lost", "我养猫。我今天散步。记住猫。");
    let out = run(&db, &key, |ctx, input| {
        let mut bad = candidate("我养猫。");
        bad.relation = "invented".into();
        let mut episode = candidate("我今天散步。");
        episode.memory_type = "episode".into();
        Ok(reviewed(
            ctx,
            input,
            w2_batch(
                ctx,
                input,
                vec![bad, episode],
                vec![request(
                    RequestKind::Remember,
                    "记住猫",
                    RequestObject::Facts,
                    vec![0],
                )],
            ),
            |_| {},
        ))
    });
    assert!(db.memories().is_empty());
    assert_eq!(
        out["enrichment"]["memory_requests"][0]["target"]["candidate_indices"],
        json!([0])
    );
    assert_eq!(
        out["enrichment"]["memory_requests"][0]["final_candidate_indices"],
        json!([])
    );
    assert_eq!(
        out["enrichment"]["memory_requests"][0]["rejected_candidate_indices"],
        json!([0])
    );
    assert_eq!(out["enrichment"]["status"], "pending");
}

#[test]
fn episode_update_is_admitted_after_w4_without_useful_default() {
    let db = Db::new();
    let old = db.start("old", "我去过北京。");
    run(&db, &old, |ctx, input| {
        let mut c = candidate("我去过北京。");
        c.memory_type = "episode".into();
        c.relation = "visited".into();
        Ok(reviewed(
            ctx,
            input,
            w2_batch(ctx, input, vec![c], vec![]),
            |w3| {
                w3.reviews[0].episode_value = EpisodeValue::Useful;
            },
        ))
    });
    let key = db.start("update", "我也去过北京。");
    let mut called = false;
    let out = run_source(
        &mut db.open(),
        &key,
        RuntimeInputBudget::default(),
        |ctx, input| {
            let mut c = candidate("我也去过北京。");
            c.memory_type = "episode".into();
            c.relation = "visited".into();
            Ok(reviewed(
                ctx,
                input,
                w2_batch(ctx, input, vec![c], vec![]),
                |_| {},
            ))
        },
        |_, input, _| {
            called = true;
            assert_eq!(input.records.len(), 1);
            let mut out = new_output(input);
            out.decisions[0].action = Some(Action::Reinforce);
            out.decisions[0].targets = vec![input.records[0].id()];
            let record = serde_json::to_value(&input.records[0]).unwrap();
            out.decisions[0]
                .basis
                .push(serde_json::from_value(record["evidence"][0].clone()).unwrap());
            Ok(Some(out))
        },
    )
    .unwrap();
    assert!(called);
    assert_eq!(out["enrichment"]["status"], "committed");
    assert_eq!(db.memories().len(), 1);
    assert_eq!(db.memories()[0].reinforcement_count, 1);
    assert_eq!(
        hippocampus::enrichment::receipt(&db.open(), &key).unwrap(),
        out
    );
}

#[test]
fn catalog_from_existing_registry_includes_fact_and_explicit_types() {
    let db = Db::new();
    let key = db.start("catalog", "我用炭笔。");
    let out = run(&db, &key, |ctx, input| {
        let mut config = ChatConfig::default();
        config.predicates.retain(|p| p.predicate.0 != "fact");
        let catalog = w2::relation_catalog(&config.predicates, &config.memory_types).unwrap();
        let fact = catalog.iter().find(|r| r.id == "fact").unwrap();
        assert!(fact.memory_types.contains(&MemoryType::Fact));
        let limited = catalog
            .iter()
            .find(|r| r.id == "code_example_language")
            .unwrap();
        assert_eq!(
            limited.memory_types,
            vec![MemoryType::Constraint, MemoryType::Preference]
        );
        let w2 = w2::validate(
            W1Output {
                candidates: vec![candidate("我用炭笔。")],
                memory_requests: vec![],
            },
            &w2::Context {
                text: &input.text,
                entities: &[EntityId::from("self")],
                relations: &catalog,
                said_on: Some("2026-10-02"),
                utc_offset_seconds: 28_800,
                task_id: None,
            },
        )
        .unwrap();
        assert_eq!(w2.candidates[0].status, w2::Status::Ok);
        Ok(reviewed(ctx, input, w2, |_| {}))
    });
    assert_eq!(out["enrichment"]["status"], "committed");
    assert_eq!(db.memories().len(), 1);
    assert_eq!(
        hippocampus::enrichment::receipt(&db.open(), &key).unwrap(),
        out
    );
}

#[test]
fn batch_budget_counts_rejected_candidates_and_saved_input_revalidates_w3() {
    let db = Db::new();
    let key = db.start("budget", "我养猫。我养狗。");
    let out = run_source(
        &mut db.open(),
        &key,
        RuntimeInputBudget {
            max_candidates: 1,
            ..Default::default()
        },
        |ctx, input| {
            let mut bad = candidate("我养猫。");
            bad.relation = "invented".into();
            Ok(reviewed(
                ctx,
                input,
                w2_batch(ctx, input, vec![bad, candidate("我养狗。")], vec![]),
                |_| {},
            ))
        },
        |_, _, _| panic!("超限不能截断后继续"),
    )
    .unwrap();
    assert_eq!(out["enrichment"]["status"], "budget_exhausted");
    assert_eq!(out["enrichment"]["candidates"].as_array().unwrap().len(), 2);
    db.open()
        .transaction(|s| {
            s.sources.get_mut(&key).unwrap().result.as_mut().unwrap()["w4_batch"]["batch"]["w2"]
                ["candidates"][1]["candidate"] =
                serde_json::to_value(candidate("我养猫。")).unwrap();
            Ok(())
        })
        .unwrap();
    let out = run(&db, &key, |_, _| panic!("已有批次，不重新提取"));
    assert_eq!(out["enrichment"]["status"], "failed");
    assert!(db.memories().is_empty());
    assert_eq!(
        db.open().read().unwrap().sources[&key].error.as_deref(),
        Some("model_invalid_output")
    );
}

#[test]
fn deleting_source_cleans_batch_controls_and_foreign_snapshot_copies() {
    let db = Db::new();
    let old = seed(&db, "old", "我养猫。");
    let key = db.start("copy", "我养狗。只保留到明天。");
    db.open()
        .transaction(|s| {
            s.sources.get_mut(&key).unwrap().request.as_mut().unwrap()["history"] =
                json!(["我养猫。"]);
            Ok(())
        })
        .unwrap();
    w4_start_round(&mut db.open(), &key, "changed-history", true).unwrap();
    run(&db, &key, |ctx, input| {
        Ok(reviewed(
            ctx,
            input,
            w2_batch(
                ctx,
                input,
                vec![candidate("我养狗。")],
                vec![request(
                    RequestKind::Retention,
                    "只保留到明天",
                    RequestObject::Facts,
                    vec![0],
                )],
            ),
            |_| {},
        ))
    });
    db.forget(&old);
    let state = db.open().read().unwrap();
    assert!(!serde_json::to_string(&state).unwrap().contains("我养猫。"));
    assert_eq!(state.sources[&key].enrichment, "needs_rejudge");
    assert!(state.sources[&key].result.as_ref().unwrap()["w4_checkpoint"]["plan"].is_null());
    db.forget(&key);
    let state = db.open().read().unwrap();
    let result = state.sources[&key].result.clone().unwrap_or(Json::Null);
    assert!(result.get("w4_batch").is_none());
    assert!(result.get("w4_controls").is_none());
    let out = hippocampus::enrichment::receipt(&db.open(), &key).unwrap();
    assert_eq!(out["enrichment"]["status"], "deleted");
    assert_eq!(out["enrichment"]["memory_requests"], json!([]));
    assert_eq!(out["enrichment"]["retention_pending"], false);
    assert!(
        !serde_json::to_string(&state)
            .unwrap()
            .contains("只保留到明天")
    );
}

#[test]
fn w3_input_changed_during_w4_or_before_w5_cannot_commit_old_plan() {
    for direct_commit in [false, true] {
        let db = Db::new();
        seed(&db, "old", "我养猫。");
        let key = db.start("changed", "我也养猫。");
        let out = run_source(
            &mut db.open(),
            &key,
            RuntimeInputBudget::default(),
            |ctx, input| {
                Ok(reviewed(
                    ctx,
                    input,
                    w2_batch(ctx, input, vec![candidate("我也养猫。")], vec![]),
                    |_| {},
                ))
            },
            |_, input, _| {
                if direct_commit {
                    return Ok(None);
                }
                db.open()
                        .transaction(|s| {
                            s.sources.get_mut(&key).unwrap().result.as_mut().unwrap()["w4_batch"]
                                ["batch"]["w2"]["said_on"] = json!("2026-10-03");
                            Ok(())
                        })
                        .unwrap();
                Ok(Some(new_output(input)))
            },
        )
        .unwrap();
        if direct_commit {
            assert_eq!(out["enrichment"]["status"], "pending");
            let claim = w4_claim_round(&mut db.open(), &key).unwrap();
            let inputs = w4_load_inputs(&db.open(), &claim).unwrap().unwrap();
            let pending: Vec<_> = inputs
                .candidates
                .iter()
                .map(|c| w4::Pending {
                    index: c.index,
                    committed_id: c.committed_id,
                    contract: ContractCandidate {
                        candidate: c.candidate.clone(),
                        source: c.source.clone(),
                        said_on: c.said_on.clone(),
                        time_resolutions: c.time_resolutions.clone(),
                    },
                })
                .collect();
            let (state, assembly) = w4::assemble(
                &db.open(),
                &key,
                None,
                &pending,
                RuntimeInputBudget::default().w4,
            )
            .unwrap();
            let plan = w4::plan(
                &w4::W4Runtime {
                    state: &state,
                    owner: UserId::from("alice"),
                    source_key: &key,
                    round: claim.round,
                    task_id: None,
                    admissions: inputs.admissions,
                },
                &assembly.input,
                &new_output(&assembly.input),
            )
            .unwrap();
            w4_save_plan(&mut db.open(), &claim, &inputs.candidates, &plan).unwrap();
            db.open()
                .transaction(|s| {
                    s.sources.get_mut(&key).unwrap().result.as_mut().unwrap()["w4_batch"]["batch"]
                        ["w2"]["said_on"] = json!("2026-10-03");
                    Ok(())
                })
                .unwrap();
            assert_eq!(
                w4_commit_group(&mut db.open(), &claim, &plan.groups[0])
                    .unwrap_err()
                    .to_string(),
                "model_invalid_output"
            );
        } else {
            assert_eq!(out["enrichment"]["status"], "failed");
        }
        assert_eq!(db.memories().len(), 1);
        assert_eq!(db.open().read().unwrap().w5_receipts.len(), 1);
    }
}

#[test]
fn source_snapshot_changed_during_upstream_rejects_late_batch() {
    let db = Db::new();
    let key = db.start("snapshot", "我养猫。");
    let error = run_source(
        &mut db.open(),
        &key,
        RuntimeInputBudget::default(),
        |ctx, input| {
            let batch = reviewed(
                ctx,
                input,
                w2_batch(ctx, input, vec![candidate("我养猫。")], vec![]),
                |_| {},
            );
            db.open()
                .transaction(|s| {
                    s.sources.get_mut(&key).unwrap().request.as_mut().unwrap()["history"] =
                        json!(["来源快照已变"]);
                    Ok(())
                })
                .unwrap();
            Ok(batch)
        },
        |_, _, _| panic!("迟到的核验不能进入 W4"),
    )
    .unwrap_err();
    assert!(error.to_string().starts_with("needs_rejudge"));
    assert!(db.memories().is_empty());
    let state = db.open().read().unwrap();
    assert!(state.w5_receipts.is_empty());
    assert!(
        state.sources[&key]
            .result
            .as_ref()
            .unwrap()
            .get("w4_batch")
            .is_none()
    );
}

#[test]
fn deletion_preserves_new_batch_enums_when_raw_text_matches_verdict() {
    let db = Db::new();
    let old = seed(&db, "enum-word", "supported");
    let key = seed(&db, "independent", "我养狗。");
    db.forget(&old);
    let out = hippocampus::enrichment::receipt(&db.open(), &key).unwrap();
    assert_eq!(out["enrichment"]["status"], "committed");
    assert_eq!(out["enrichment"]["candidates"][0]["status"], "committed");
    let state = db.open().read().unwrap();
    assert_eq!(
        state.sources[&key].result.as_ref().unwrap()["w4_batch"]["batch"]["review"]["output"]["reviews"]
            [0]["verdict"],
        "supported"
    );
    assert!(state.sources[&old].request.is_none());
}

#[test]
fn temporarily_unsupported_product_reuses_historical_binding_when_supported_again() {
    let db = Db::new();
    let key = db.start("products", "我养猫。我养狗。");
    let first = run(&db, &key, |ctx, input| {
        Ok(reviewed(
            ctx,
            input,
            w2_batch(
                ctx,
                input,
                vec![candidate("我养猫。"), candidate("我养狗。")],
                vec![],
            ),
            |_| {},
        ))
    });
    let ids: Vec<_> = first["enrichment"]["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["memory_id"].clone())
        .collect();
    for supported_again in [false, true] {
        db.open()
            .transaction(|s| {
                s.sources.get_mut(&key).unwrap().request.as_mut().unwrap()["history"] =
                    json!([format!("新核验背景 {supported_again}")]);
                Ok(())
            })
            .unwrap();
        w4_start_round(
            &mut db.open(),
            &key,
            if supported_again {
                "supported-again"
            } else {
                "paused"
            },
            true,
        )
        .unwrap();
        let out = run_source(
            &mut db.open(),
            &key,
            RuntimeInputBudget::default(),
            |ctx, input| {
                Ok(reviewed(
                    ctx,
                    input,
                    ctx.previous_w2.clone().unwrap(),
                    |w3| {
                        if !supported_again {
                            w3.reviews[0].verdict = Verdict::Insufficient;
                        }
                    },
                ))
            },
            |_, input, _| {
                for c in &input.candidates {
                    assert_eq!(serde_json::to_value(c.committed_id).unwrap(), ids[c.index]);
                }
                Ok(Some(new_output(input)))
            },
        )
        .unwrap();
        assert_eq!(
            out["enrichment"]["status"],
            if supported_again {
                "committed"
            } else {
                "pending"
            }
        );
        assert_eq!(
            out["enrichment"]["candidates"][0]["status"],
            if supported_again {
                "committed"
            } else {
                "pending"
            }
        );
        if supported_again {
            assert_eq!(out["enrichment"]["candidates"][0]["memory_id"], ids[0]);
        }
        assert_eq!(out["enrichment"]["candidates"][1]["memory_id"], ids[1]);
        assert_eq!(db.memories().len(), 2);
        assert_eq!(
            hippocampus::enrichment::receipt(&db.open(), &key).unwrap(),
            out
        );
    }
}

#[test]
fn new_control_review_can_pause_previously_committed_product_and_persist_batch() {
    let db = Db::new();
    let key = db.start("committed-control", "我养猫。我养狗。猫只保留到明天。");
    run(&db, &key, |ctx, input| {
        Ok(reviewed(
            ctx,
            input,
            w2_batch(
                ctx,
                input,
                vec![candidate("我养猫。"), candidate("我养狗。")],
                vec![request(
                    RequestKind::Retention,
                    "猫只保留到明天",
                    RequestObject::Facts,
                    vec![0],
                )],
            ),
            |_| {},
        ))
    });
    assert_eq!(db.memories().len(), 2);
    db.open()
        .transaction(|s| {
            s.sources.get_mut(&key).unwrap().request.as_mut().unwrap()["history"] =
                json!(["新的对象核验背景"]);
            Ok(())
        })
        .unwrap();
    w4_start_round(&mut db.open(), &key, "new-control-review", true).unwrap();
    let out = run(&db, &key, |ctx, input| {
        Ok(reviewed(
            ctx,
            input,
            ctx.previous_w2.clone().unwrap(),
            |w3| {
                w3.request_reviews[0].verdict = Verdict::Insufficient;
            },
        ))
    });
    assert_eq!(out["enrichment"]["status"], "awaiting_confirmation");
    assert_eq!(out["enrichment"]["candidates"][0]["status"], "pending");
    assert_eq!(out["enrichment"]["candidates"][1]["status"], "committed");
    assert_eq!(
        out["enrichment"]["memory_requests"][0]["status"],
        "awaiting_confirmation"
    );
    let state = db.open().read().unwrap();
    assert_eq!(
        state.sources[&key].result.as_ref().unwrap()["w4_batch"]["batch"]["review"]["output"]["request_reviews"]
            [0]["verdict"],
        "insufficient"
    );
    assert_eq!(db.memories().len(), 2);
    assert!(
        db.memories()
            .iter()
            .all(|m| m.status != MemoryStatus::Deleted)
    );
    assert_eq!(
        hippocampus::enrichment::receipt(&db.open(), &key).unwrap(),
        out
    );
}

// M1.5：只注入模型输出；真实 SQLite / W2 / W3 / W4 / W5，关闭重开读完整候选。
fn run_plan_action(
    db: &Db,
    key: &str,
    c: Candidate,
    action: Action,
    targets: Vec<MemoryId>,
) -> Json {
    run_source(
        &mut db.open(),
        key,
        RuntimeInputBudget::default(),
        |ctx, input| {
            Ok(reviewed(
                ctx,
                input,
                w2_batch(ctx, input, vec![c.clone()], vec![]),
                |_| {},
            ))
        },
        |_, input, _| {
            let mut output = new_output(input);
            let decision = &mut output.decisions[0];
            decision.action = Some(action);
            decision.targets = targets.clone();
            for id in &targets {
                let record = input.records.iter().find(|r| r.id() == *id).unwrap();
                let RecordView2::Current(record) = record else {
                    panic!("plan is complete")
                };
                decision.basis.extend(record.evidence.clone());
            }
            Ok(Some(output))
        },
    )
    .unwrap()
}
fn submitted(out: &Json) -> MemoryId {
    assert_eq!(
        out["enrichment"]["candidates"][0]["status"], "committed",
        "{out}"
    );
    MemoryId(
        out["enrichment"]["candidates"][0]["memory_id"]
            .as_u64()
            .unwrap(),
    )
}
fn as_plan(text: &str) -> Candidate {
    let mut c = candidate(text);
    c.speech = Speech::Plan;
    c
}

#[test]
fn plans_keep_modality_polarity_qualifiers_and_past_event_dates_on_reopen() {
    let db = Db::new();
    let text = "如果码头开放，我可能计划在2002年2月14日乘渡轮。";
    let mut c = as_plan(text);
    c.conditions = vec![TextQualifier {
        quote: Quote {
            text: "如果码头开放".into(),
        },
        resolved: true,
    }];
    c.uncertainty = Some(TextQualifier {
        quote: Quote {
            text: "可能".into(),
        },
        resolved: true,
    });
    c.times = vec![TimeInterp {
        role: TimeRole::Event,
        quote: Quote {
            text: "2002年2月14日".into(),
        },
        interp: Interp::Date {
            date: "2002-02-14".into(),
        },
        anchor: TimeAnchor::SaidOn,
    }];
    let key = db.start("plan", text);
    let id = submitted(&run_plan_action(&db, &key, c.clone(), Action::New, vec![]));
    let state = db.open().read().unwrap();
    let full = w5_record_contract(&state, id).unwrap();
    assert_eq!(full.candidate, c);
    assert!(!full.lossless());
    let RecordView2::Current(view) = w4::record_view(&state, id).unwrap() else {
        panic!()
    };
    assert_eq!(view.candidate, c);
    assert_eq!(view.stored_status, StoredStatus::Active);
    let m = db.memories().into_iter().find(|m| m.id == id).unwrap();
    assert_eq!(m.temporal.valid_to, None);
    assert_eq!(m.temporal.expires_at, None);

    let text = "我不计划参加夜间巡游。";
    let mut c = as_plan(text);
    c.polarity = hippocampus::contract::Polarity::Negate;
    let key = db.start("negative", text);
    let id = submitted(&run_plan_action(
        &db,
        &key,
        c.clone(),
        Action::Coexist,
        vec![],
    ));
    assert_eq!(
        w5_record_contract(&db.open().read().unwrap(), id)
            .unwrap()
            .candidate,
        c
    );
    c.polarity = hippocampus::contract::Polarity::Unknown;
    let key = db.start("unknown", text);
    let out = run_plan_action(&db, &key, c, Action::New, vec![]);
    assert_eq!(out["enrichment"]["candidates"][0]["status"], "pending");
    assert!(out["enrichment"]["candidates"][0]["memory_id"].is_null());
    assert_eq!(db.memories().len(), 2);
    let listed = ChatMemoryService::new(db.open(), ChatConfig::default())
        .unwrap()
        .call("memory_list", json!({}), || false)
        .unwrap();
    let negative = listed["memories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == id.0)
        .unwrap();
    assert_eq!(negative["qualifiers_unmapped"]["speech"], "plan");
    assert_eq!(negative["qualifiers_unmapped"]["polarity"], "negate");
}

#[test]
fn plan_changes_and_cancellation_end_only_model_targets_and_keep_booking() {
    let db = Db::new();
    let booking = seed(&db, "booking", "我已购买渡轮船票。");
    let booking_id = submitted(&hippocampus::enrichment::receipt(&db.open(), &booking).unwrap());
    let text = "我计划清晨乘渡轮。";
    let key = db.start("morning", text);
    let original = submitted(&run_plan_action(
        &db,
        &key,
        as_plan(text),
        Action::Coexist,
        vec![],
    ));
    let text = "我改为计划下午乘渡轮。";
    let key = db.start("afternoon", text);
    let changed = submitted(&run_plan_action(
        &db,
        &key,
        as_plan(text),
        Action::StateChange,
        vec![original],
    ));
    let text = "我取消乘渡轮的计划。";
    let key = db.start("cancel", text);
    let mut c = as_plan(text);
    c.polarity = hippocampus::contract::Polarity::Negate;
    let cancelled = submitted(&run_plan_action(
        &db,
        &key,
        c,
        Action::StateChange,
        vec![changed],
    ));
    let state = db.open().read().unwrap();
    for id in [original, changed] {
        let RecordView2::Current(view) = w4::record_view(&state, id).unwrap() else {
            panic!()
        };
        assert_eq!(view.termination_reason, Some(SupersedeReason::StateChanged));
        assert_eq!(view.ended, Ended::EndUnknown);
        assert_eq!(view.candidate.speech, Speech::Plan);
    }
    let RecordView2::Current(view) = w4::record_view(&state, booking_id).unwrap() else {
        panic!()
    };
    assert_eq!(view.stored_status, StoredStatus::Active);
    assert_eq!(view.candidate.speech, Speech::UserStatement);
    assert_eq!(
        w5_record_contract(&state, cancelled)
            .unwrap()
            .candidate
            .polarity,
        hippocampus::contract::Polarity::Negate
    );
    assert!(state.sources.values().all(|s| s.enrichment != "deleted"));
}

#[test]
fn plan_correction_and_withdrawal_preserve_the_distinct_model_decision() {
    for action in [Action::Correction, Action::Retract] {
        let db = Db::new();
        let text = "我计划参加晨间导览。";
        let key = db.start("original", text);
        let id = submitted(&run_plan_action(
            &db,
            &key,
            as_plan(text),
            Action::New,
            vec![],
        ));
        let text = "此前导览计划的陈述有误。";
        let key = db.start("revision", text);
        let mut c = as_plan(text);
        if action == Action::Retract {
            c.kind = CandidateKind::Retraction;
            c.value = None;
        }
        run_plan_action(&db, &key, c, action, vec![id]);
        let state = db.open().read().unwrap();
        let RecordView2::Current(view) = w4::record_view(&state, id).unwrap() else {
            panic!()
        };
        assert_eq!(
            view.termination_reason,
            Some(if action == Action::Correction {
                SupersedeReason::Correction
            } else {
                SupersedeReason::UserRetracted
            })
        );
        assert!(state.sources.values().all(|s| s.enrichment != "deleted"));
        assert_eq!(
            db.memories().len(),
            if action == Action::Correction { 2 } else { 1 }
        );
    }
}

#[test]
fn w3_reported_missing_plan_condition_cannot_be_supported_or_committed() {
    let db = Db::new();
    let text = "若渡口开放，我计划乘渡轮。";
    let key = db.start("condition", text);
    let out = run(&db, &key, |ctx, input| {
        let batch = w2_batch(ctx, input, vec![as_plan(text)], vec![]);
        let problem = ReviewProblem {
            aspect: ReviewAspect::Condition,
            quote: Quote {
                text: "若渡口开放".into(),
            },
        };
        let mut review = W3Output {
            reviews: vec![CandidateReview {
                candidate: 0,
                verdict: Verdict::Supported,
                problems: vec![problem],
                episode_value: EpisodeValue::NotApplicable,
            }],
            request_reviews: vec![],
            coverage: W3Coverage {
                status: CoverageStatus::Complete,
                missing: vec![],
            },
        };
        assert!(
            review
                .clone()
                .validate(&batch, &ctx.source_binding, &input.text)
                .is_err()
        );
        review.reviews[0].verdict = Verdict::Contradicted;
        let review = Some(
            review
                .validate(&batch, &ctx.source_binding, &input.text)
                .unwrap(),
        );
        Ok(UpstreamBatch { w2: batch, review })
    });
    assert!(out["enrichment"]["candidates"][0]["memory_id"].is_null());
    assert!(db.memories().is_empty());
}

#[test]
fn recall_preserves_plan_records_and_injected_plan_wording_without_inferring_execution() {
    use hippocampus::recall::{self, Budget, Request};
    let db = Db::new();
    let text = "我计划参加清晨导览。";
    let c = as_plan(text);
    let key = db.start("recall-plan", text);
    let id = submitted(&run_plan_action(&db, &key, c.clone(), Action::New, vec![]));
    let query = "已记录的导览计划是什么？";
    let req = Request {
        binding: RecallRequest {
            request_id: "plan-query".into(),
            query: query.into(),
            context: vec![],
            task_id: None,
            at: 1893456000,
        },
        owner: UserId::from("alice"),
        subject_ids: None,
        said_on: "2030-01-01".into(),
        utc_offset_seconds: 28800,
        bootstrap: false,
    };
    let relations = [w2::Relation {
        id: "fact".into(),
        memory_types: vec![MemoryType::Fact],
    }];
    let entities = [EntityId::from("self")];
    let plan = r1::validate(serde_json::from_value(json!({"need":"judge","current_evidence":"query_only","type":"single","subject":{"entity_id":"self"},"relation":"导览计划","conditions":[],"time":null,"plan":{"op":"lookup","filter":{"relation":"fact","time":null},"unit":null}})).unwrap(), &r1::Context { query, context: &[], entities: &entities, relations: &relations, subject_ids: None, task_id: None, said_on: &req.said_on, utc_offset_seconds: req.utc_offset_seconds, supported_units: &[] }).unwrap();
    let assembly = recall::assemble(&db.open(), &req, &plan, Budget::default()).unwrap();
    let stored_source = db.open().read().unwrap().sources[&key].id.clone();
    let model = json!({"verdict":"supported","facts":[{"statement":"用户计划参加清晨导览，执行情况未知。","evidence":[{"source":{"kind":"stored","source_id":stored_source},"quote":{"text":text}}],"record_ids":[id.0],"polarity":"affirm","strength":null,"uncertainty":null,"conditions":[],"times":[],"current":"yes","applies":"yes","supersedes_record_ids":[]}],"compute":null});
    let out = assembly
        .finish_r4(&db.open(), Ok(&model.to_string()))
        .unwrap();
    assert_eq!(out.version, "recall.v7");
    assert_eq!(out.status, RecallStatus::Answered);
    assert_eq!(
        out.facts[0].statement,
        "用户计划参加清晨导览，执行情况未知。"
    );
    let RecordView::Current(record) = &out.records[0] else {
        panic!()
    };
    assert_eq!(record.candidate, c);
    assert_eq!(record.stored_status, StoredStatus::Active);
    // 查询执行：模型提供不足判断，代码不根据 Active/日期自行改成已发生。
    let mut execution = req.clone();
    execution.binding.request_id = "plan-execution-query".into();
    execution.binding.query = "导览是否已参加？".into();
    let mut understood = plan.output.clone().unwrap();
    understood.relation = "导览执行情况".into();
    let execution_plan = r1::validate(
        understood,
        &r1::Context {
            query: &execution.binding.query,
            context: &[],
            entities: &entities,
            relations: &relations,
            subject_ids: None,
            task_id: None,
            said_on: &execution.said_on,
            utc_offset_seconds: execution.utc_offset_seconds,
            supported_units: &[],
        },
    )
    .unwrap();
    let out = recall::assemble(&db.open(), &execution, &execution_plan, Budget::default())
        .unwrap()
        .finish_r4(
            &db.open(),
            Ok(r#"{"verdict":"insufficient","facts":[],"compute":null}"#),
        )
        .unwrap();
    assert_eq!(out.status, RecallStatus::Insufficient);
    assert!(out.facts.is_empty());
}
