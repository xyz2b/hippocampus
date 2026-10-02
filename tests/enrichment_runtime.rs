//! 第③段：真实 T1、SQLite 重开和注入提供者驱动的新路径闭环。
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
                .join(format!("enrichment-{}", diagnostics::new_id()))
                .join("memory.sqlite3"),
        )
    }
    fn open(&self) -> SqliteMemoryStore {
        SqliteMemoryStore::open(&self.0, UserId::from("alice")).unwrap()
    }
    fn capture(&self, turn: &str, text: &str) -> String {
        self.capture_history(turn, text, &[])
    }
    fn capture_history(&self, turn: &str, text: &str, history: &[&str]) -> String {
        self.capture_at(turn, text, history, None)
    }
    fn capture_at(
        &self,
        turn: &str,
        text: &str,
        history: &[&str],
        observed_at: Option<i64>,
    ) -> String {
        let mut service = ChatMemoryService::new(
            self.open(),
            ChatConfig {
                enrichment_mode: EnrichmentMode::Off,
                ..Default::default()
            },
        )
        .unwrap();
        let mut request = json!({"text":text,"history":history,"session_id":"chat","turn_id":turn,"capture_source":true});
        if let Some(at) = observed_at {
            request["observed_at"] = json!(at);
        }
        let result = service.call("memory_remember", request, || false).unwrap();
        assert_eq!(result["source"]["status"], "stored");
        let state = self.open().read().unwrap();
        let (key, source) = state
            .sources
            .iter()
            .find(|(_, s)| s.id == result["source"]["source_id"].as_str().unwrap())
            .unwrap();
        assert_eq!(source.task.processing_version, None);
        key.clone()
    }
    fn start(&self, turn: &str, text: &str) -> String {
        let key = self.capture(turn, text);
        w4_start_round(&mut self.open(), &key, turn, false).unwrap();
        key
    }
}
impl Drop for Db {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.0.parent().unwrap());
    }
}

fn supported(ctx: &ProviderContext, text: &str, index: usize) -> SupportedCandidate {
    let sample: Json =
        serde_json::from_str(include_str!("eval/w4/samples/w4_10_reinforce.json")).unwrap();
    let mut contract: ContractCandidate = serde_json::from_value(json!({
        "candidate": sample["input"]["candidates"][0]["candidate"],
        "source": {"kind":"stored", "source_id":ctx.source_id},
        "said_on":"2026-10-02", "time_resolutions":[]
    }))
    .unwrap();
    contract.candidate.sentence.text = text.into();
    contract.candidate.value = Some(hippocampus::contract::Value::Text {
        text: text.into(),
        quote: contract.candidate.sentence.clone(),
    });
    SupportedCandidate {
        index,
        contract,
        admission: w4::Admission::default(),
    }
}

#[test]
fn real_t1_runs_without_w4_and_reopens_committed() {
    let db = Db::new();
    let key = db.start("first", "我喜欢喝咖啡。");
    let result = run_source(
        &mut db.open(),
        &key,
        RuntimeInputBudget::default(),
        |ctx, input| {
            assert_eq!(ctx.round, 0);
            assert_eq!(ctx.operation_id, "first");
            assert_eq!(input.text, "我喜欢喝咖啡。");
            Ok(vec![supported(ctx, &input.text, 0)])
        },
        |_, _, _| panic!("没有相关记录，不应调用 W4"),
    )
    .unwrap();
    assert_eq!(result["enrichment"]["status"], "committed");
    assert_eq!(result["enrichment"]["candidates"][0]["status"], "committed");
    assert!(result["enrichment"]["candidates"][0]["memory_id"].is_number());
    assert!(result.get("w4_checkpoint").is_none());
    let state = db.open().read().unwrap();
    assert_eq!(state.sources[&key].enrichment, "committed");
    assert_eq!(
        state
            .repository
            .list_by_owner(&UserId::from("alice"))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn unavailable_w4_preserves_inputs_and_resumes_without_upstream() {
    let db = Db::new();
    let old = db.start("old", "我喜欢喝咖啡。");
    run_source(
        &mut db.open(),
        &old,
        RuntimeInputBudget::default(),
        |ctx, input| Ok(vec![supported(ctx, &input.text, 0)]),
        |_, _, _| panic!("no W4"),
    )
    .unwrap();
    let key = db.start("repeat", "我还是喜欢喝咖啡。");
    let result = run_source(
        &mut db.open(),
        &key,
        RuntimeInputBudget::default(),
        |ctx, input| Ok(vec![supported(ctx, &input.text, 0)]),
        |_, input, texts| {
            assert_eq!(input.records.len(), 1);
            assert_eq!(texts.len(), 2);
            Ok(None)
        },
    )
    .unwrap();
    assert_eq!(result["enrichment"]["status"], "pending");
    assert_eq!(
        result["enrichment"]["candidates"].as_array().unwrap().len(),
        1
    );
    let state = db.open().read().unwrap();
    assert_eq!(state.sources[&key].task.attempt_id, None);
    assert_eq!(
        state
            .repository
            .list_by_owner(&UserId::from("alice"))
            .unwrap()
            .len(),
        1
    );
    let result = run_source(
        &mut db.open(),
        &key,
        RuntimeInputBudget::default(),
        |_, _| panic!("saved inputs"),
        |ctx, input, _| {
            assert_eq!(ctx.operation_id, "repeat");
            Ok(Some(reinforce(input)))
        },
    )
    .unwrap();
    assert_eq!(result["enrichment"]["status"], "committed");
    assert_eq!(result["enrichment"]["candidates"][0]["memory_id"], 1);
    assert_eq!(
        db.open()
            .read()
            .unwrap()
            .repository
            .list_by_owner(&UserId::from("alice"))
            .unwrap()[0]
            .reinforcement_count,
        1
    );
}

fn reinforce(input: &W4Input) -> W4Output {
    let c = &input.candidates[0];
    let old: Json = serde_json::to_value(&input.records[0]).unwrap();
    W4Output {
        decisions: vec![W4Decision {
            candidate: c.index,
            status: DecisionStatus::Decided,
            action: Some(Action::Reinforce),
            targets: vec![input.records[0].id()],
            resolves_relations: vec![],
            basis: vec![
                EvidenceRef {
                    source: c.source.clone(),
                    quote: c.candidate.sentence.clone(),
                },
                serde_json::from_value(old["evidence"][0].clone()).unwrap(),
            ],
            effective: None,
            relation_scope: None,
            pending_reason: None,
        }],
    }
}

fn seed(db: &Db, turn: &str, text: &str, relation: &str) -> MemoryId {
    let key = db.start(turn, text);
    let result = run_source(
        &mut db.open(),
        &key,
        RuntimeInputBudget::default(),
        |ctx, input| {
            let mut c = supported(ctx, &input.text, 0);
            c.contract.candidate.relation = relation.into();
            Ok(vec![c])
        },
        |_, input, _| Ok(Some(new_output(input))),
    )
    .unwrap();
    assert_eq!(result["enrichment"]["status"], "committed");
    serde_json::from_value(result["enrichment"]["candidates"][0]["memory_id"].clone()).unwrap()
}

#[test]
fn invalid_w4_output_fails_with_zero_business_writes() {
    let db = Db::new();
    seed(&db, "old", "我喜欢喝咖啡。", "fact");
    let key = db.start("bad", "我还是喜欢喝咖啡。");
    let before = db.open().read().unwrap();
    let result = run_source(
        &mut db.open(),
        &key,
        RuntimeInputBudget::default(),
        |ctx, input| Ok(vec![supported(ctx, &input.text, 0)]),
        |_, input, _| {
            let mut out = reinforce(input);
            out.decisions[0].targets = vec![MemoryId(999)];
            Ok(Some(out))
        },
    )
    .unwrap();
    assert_eq!(result["enrichment"]["status"], "failed");
    let after = db.open().read().unwrap();
    assert_eq!(
        serde_json::to_value(before.repository).unwrap(),
        serde_json::to_value(after.repository).unwrap()
    );
    assert_eq!(before.w5_receipts, after.w5_receipts);
    assert_eq!(after.sources[&key].task.attempt_id, None);
    assert_eq!(w4_progress(&db.open(), &key).unwrap().candidates.len(), 1);
}

fn retraction(mut c: SupportedCandidate) -> SupportedCandidate {
    c.contract.candidate.kind = CandidateKind::Retraction;
    c.contract.candidate.value = None;
    c
}

#[test]
fn unknown_retraction_and_mixed_results_stay_pending() {
    for mixed in [false, true] {
        let db = Db::new();
        let key = db.start("retract", "撤回先前偏好。我喜欢喝咖啡。");
        let result = run_source(
            &mut db.open(),
            &key,
            RuntimeInputBudget::default(),
            |ctx, _| {
                let mut cs = vec![retraction(supported(ctx, "撤回先前偏好。", 0))];
                if mixed {
                    cs.push(supported(ctx, "我喜欢喝咖啡。", 1));
                }
                Ok(cs)
            },
            |_, _, _| panic!("no targets"),
        )
        .unwrap();
        assert_eq!(result["enrichment"]["status"], "pending");
        let pending = &result["enrichment"]["candidates"][0];
        assert_eq!(pending["status"], "pending");
        assert!(pending["memory_id"].is_null());
        assert_eq!(
            pending["update"]["reasons"],
            json!(["update_scope_incomplete", "update_target_unknown"])
        );
        if mixed {
            assert_eq!(result["enrichment"]["candidates"][1]["status"], "committed");
        }
        assert_eq!(
            db.open()
                .read()
                .unwrap()
                .repository
                .list_by_owner(&UserId::from("alice"))
                .unwrap()
                .len(),
            usize::from(mixed)
        );
    }
}

#[test]
fn executed_multi_target_retraction_has_no_arbitrary_memory_id() {
    let db = Db::new();
    let first = seed(&db, "one", "我喜欢咖啡。", "likes");
    let second = seed(&db, "two", "我喜欢咖啡和红茶。", "likes");
    let key = db.start("retract", "撤回我喜欢咖啡和红茶的说法。");
    let result = run_source(
        &mut db.open(),
        &key,
        RuntimeInputBudget::default(),
        |ctx, input| {
            let mut c = retraction(supported(ctx, &input.text, 0));
            c.contract.candidate.relation = "likes".into();
            Ok(vec![c])
        },
        |_, input, _| {
            assert_eq!(input.records.len(), 2);
            let c = &input.candidates[0];
            let mut basis = vec![EvidenceRef {
                source: c.source.clone(),
                quote: c.candidate.sentence.clone(),
            }];
            for r in &input.records {
                let v = serde_json::to_value(r).unwrap();
                basis.push(serde_json::from_value(v["evidence"][0].clone()).unwrap());
            }
            Ok(Some(W4Output {
                decisions: vec![W4Decision {
                    candidate: 0,
                    status: DecisionStatus::Decided,
                    action: Some(Action::Retract),
                    targets: vec![first, second],
                    resolves_relations: vec![],
                    basis,
                    effective: None,
                    relation_scope: None,
                    pending_reason: None,
                }],
            }))
        },
    )
    .unwrap();
    assert_eq!(result["enrichment"]["status"], "committed");
    assert_eq!(result["enrichment"]["candidates"][0]["status"], "committed");
    assert!(result["enrichment"]["candidates"][0]["memory_id"].is_null());
    let state = db.open().read().unwrap();
    for id in [first, second] {
        assert_eq!(
            state.repository.get(id).unwrap().unwrap().status,
            MemoryStatus::Superseded
        );
    }
    assert_eq!(
        state.w5_receipts[&format!("{key}#0#0")].decisions[0].targets,
        vec![first, second]
    );
}

#[test]
fn held_and_unresolved_candidates_are_visible_without_fake_completion() {
    let db = Db::new();
    let key = db.start("held", "我刚才喝了咖啡。他喜欢红茶。我喜欢绿茶。");
    let result = run_source(
        &mut db.open(),
        &key,
        RuntimeInputBudget::default(),
        |ctx, _| {
            let mut episode = supported(ctx, "我刚才喝了咖啡。", 0);
            episode.contract.candidate.memory_type = "episode".into();
            let mut unresolved = supported(ctx, "他喜欢红茶。", 1);
            unresolved.contract.candidate.subject = Subject::Unresolved(UnresolvedSubject {
                unresolved: Quote { text: "他".into() },
            });
            Ok(vec![episode, unresolved, supported(ctx, "我喜欢绿茶。", 2)])
        },
        |_, _, _| panic!("no W4"),
    )
    .unwrap();
    assert_eq!(result["enrichment"]["status"], "pending");
    assert_eq!(result["enrichment"]["candidates"][0]["status"], "pending");
    assert_eq!(
        result["enrichment"]["candidates"][1]["status"],
        "unresolved"
    );
    assert_eq!(result["enrichment"]["candidates"][2]["status"], "committed");
    assert_eq!(
        db.open()
            .read()
            .unwrap()
            .repository
            .list_by_owner(&UserId::from("alice"))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn empty_upstream_is_unreviewed_and_never_completed_no_facts() {
    let db = Db::new();
    let key = db.start("empty", "早上好。");
    let result = run_source(
        &mut db.open(),
        &key,
        RuntimeInputBudget::default(),
        |_, _| Ok(vec![]),
        |_, _, _| panic!("no W4"),
    )
    .unwrap();
    assert_eq!(result["enrichment"]["status"], "no_candidates_unreviewed");
    assert_eq!(
        db.open().read().unwrap().sources[&key].enrichment,
        "no_candidates_unreviewed"
    );
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

#[test]
fn reinforce_keeps_its_own_pending_update_not_the_targets_judged_state() {
    let db = Db::new();
    let id = seed(&db, "old", "我喜欢喝咖啡。", "fact");
    db.open()
        .transaction(|state| {
            state
                .record_updates
                .insert(id, UpdateState::from_reasons(vec![]));
            Ok(())
        })
        .unwrap();
    let key = db.start("repeat", "我还是喜欢喝咖啡。");
    let result = run_source(
        &mut db.open(),
        &key,
        RuntimeInputBudget::default(),
        |ctx, input| Ok(vec![supported(ctx, &input.text, 0)]),
        |_, input, _| Ok(Some(reinforce(input))),
    )
    .unwrap();
    assert_eq!(result["enrichment"]["status"], "committed");
    assert_eq!(
        result["enrichment"]["candidates"][0]["update"]["status"],
        "pending"
    );
    assert_eq!(
        result["enrichment"]["candidates"][0]["update"]["reasons"],
        json!(["update_scope_incomplete"])
    );
    assert_eq!(
        db.open().read().unwrap().record_updates[&id].status,
        UpdateStatus::Judged
    );
}

#[test]
fn raw_byte_budget_checks_equal_overflow_and_keeps_t1() {
    for limit in ["我喜欢咖啡。".len(), "我喜欢咖啡。".len() - 1] {
        let db = Db::new();
        let key = db.start("bytes", "我喜欢咖啡。");
        let mut calls = 0;
        let result = run_source(
            &mut db.open(),
            &key,
            RuntimeInputBudget {
                max_raw_bytes: limit,
                ..Default::default()
            },
            |ctx, input| {
                calls += 1;
                Ok(vec![supported(ctx, &input.text, 0)])
            },
            |_, _, _| panic!("no W4"),
        )
        .unwrap();
        let state = db.open().read().unwrap();
        assert!(state.sources[&key].request.is_some());
        if limit == "我喜欢咖啡。".len() {
            assert_eq!(result["enrichment"]["status"], "committed");
            assert_eq!(calls, 1);
        } else {
            assert_eq!(result["enrichment"]["status"], "budget_exhausted");
            assert_eq!(calls, 0);
            assert_eq!(
                state.sources[&key].result.as_ref().unwrap()["w4_input_meta"]["diagnostic"],
                json!({"item":"raw_bytes","actual":18,"limit":17})
            );
        }
    }
}

#[test]
fn candidate_budget_never_truncates_and_resumes_saved_inputs() {
    for limit in [1, 0] {
        let db = Db::new();
        let key = db.start("candidates", "我喜欢咖啡。");
        let result = run_source(
            &mut db.open(),
            &key,
            RuntimeInputBudget {
                max_candidates: limit,
                ..Default::default()
            },
            |ctx, input| Ok(vec![supported(ctx, &input.text, 0)]),
            |_, _, _| panic!("no W4"),
        )
        .unwrap();
        assert_eq!(
            result["enrichment"]["status"],
            if limit == 1 {
                "committed"
            } else {
                "budget_exhausted"
            }
        );
        let state = db.open().read().unwrap();
        assert!(state.sources[&key].request.is_some());
        assert_eq!(w4_progress(&db.open(), &key).unwrap().candidates.len(), 1);
        if limit == 0 {
            assert!(state.w5_receipts.is_empty());
            let result = run_source(
                &mut db.open(),
                &key,
                RuntimeInputBudget::default(),
                |_, _| panic!("saved inputs"),
                |_, _, _| panic!("no W4"),
            )
            .unwrap();
            assert_eq!(result["enrichment"]["status"], "committed");
        }
    }
}

fn conflict_seed(db: &Db) {
    seed(db, "old", "我喜欢咖啡。", "likes");
    let key = db.start("conflict", "我喜欢红茶。");
    let result = run_source(
        &mut db.open(),
        &key,
        RuntimeInputBudget::default(),
        |ctx, input| {
            let mut c = supported(ctx, &input.text, 0);
            c.contract.candidate.relation = "likes".into();
            Ok(vec![c])
        },
        |_, input, _| {
            let mut out = reinforce(input);
            out.decisions[0].action = Some(Action::Conflict);
            out.decisions[0].relation_scope = Some(RelationScope {
                conditions: vec![],
                times: vec![],
            });
            Ok(Some(out))
        },
    )
    .unwrap();
    assert_eq!(
        result["enrichment"]["status"],
        "committed",
        "{:?}",
        db.open().read().unwrap().sources[&key].error
    );
    assert_eq!(db.open().read().unwrap().relations.len(), 1);
}

#[test]
fn relation_budget_checks_equal_overflow_before_w4() {
    for limit in [1, 0] {
        let db = Db::new();
        conflict_seed(&db);
        let key = db.start("relations", "我喜欢绿茶。");
        let mut calls = 0;
        let result = run_source(
            &mut db.open(),
            &key,
            RuntimeInputBudget {
                max_relations: limit,
                ..Default::default()
            },
            |ctx, input| {
                let mut c = supported(ctx, &input.text, 0);
                c.contract.candidate.relation = "likes".into();
                Ok(vec![c])
            },
            |_, input, _| {
                calls += 1;
                assert_eq!(input.relations.len(), 1);
                Ok(Some(new_output(input)))
            },
        )
        .unwrap();
        assert_eq!(
            result["enrichment"]["status"],
            if limit == 1 {
                "committed"
            } else {
                "budget_exhausted"
            }
        );
        assert_eq!(calls, limit);
        let state = db.open().read().unwrap();
        assert!(state.sources[&key].request.is_some());
        if limit == 0 {
            assert_eq!(
                state.sources[&key].result.as_ref().unwrap()["w4_input_meta"]["diagnostic"],
                json!({"item":"relations","actual":1,"limit":0})
            );
            assert_eq!(
                state
                    .repository
                    .list_by_owner(&UserId::from("alice"))
                    .unwrap()
                    .len(),
                2
            );
        }
    }
}

#[test]
fn w4_raw_budget_counts_distinct_old_sources_and_history() {
    for overflow in [false, true] {
        let db = Db::new();
        let old = db.capture_history("old", "我喜欢咖啡。", &["之前谈过饮品。"]);
        w4_start_round(&mut db.open(), &old, "old", false).unwrap();
        run_source(
            &mut db.open(),
            &old,
            RuntimeInputBudget::default(),
            |ctx, input| {
                let mut c = supported(ctx, &input.text, 0);
                c.contract.candidate.relation = "likes".into();
                Ok(vec![c])
            },
            |_, _, _| panic!("no W4"),
        )
        .unwrap();
        let key = db.capture_history("bytes", "我喜欢紅茶。", &["以前的偏好。"]);
        w4_start_round(&mut db.open(), &key, "bytes", false).unwrap();
        let total = "我喜欢咖啡。".len()
            + "我喜欢紅茶。".len()
            + "以前的偏好。".len()
            + "之前谈过饮品。".len();
        let mut calls = 0;
        let result = run_source(
            &mut db.open(),
            &key,
            RuntimeInputBudget {
                max_raw_bytes: total - usize::from(overflow),
                ..Default::default()
            },
            |ctx, input| {
                let mut c = supported(ctx, &input.text, 0);
                c.contract.candidate.relation = "likes".into();
                Ok(vec![c])
            },
            |_, input, texts| {
                calls += 1;
                assert_eq!(texts.len(), 2);
                Ok(Some(new_output(input)))
            },
        )
        .unwrap();
        assert_eq!(calls, usize::from(!overflow));
        assert_eq!(
            result["enrichment"]["status"],
            if overflow {
                "budget_exhausted"
            } else {
                "committed"
            }
        );
    }
}

fn expire(db: &Db, key: &str) {
    db.open()
        .transaction(|s| {
            s.sources.get_mut(key).unwrap().task.lease_until = Some(0);
            Ok(())
        })
        .unwrap();
}

fn context(db: &Db, claim: &W4Claim) -> ProviderContext {
    let state = db.open().read().unwrap();
    let source = &state.sources[&claim.claim.source_key];
    ProviderContext {
        owner: UserId::from("alice"),
        source_key: claim.claim.source_key.clone(),
        source_id: source.id.clone(),
        source_binding: claim.source_binding().clone(),
        observed_at: source.observed_at,
        received_sequence: source.received_sequence,
        task_id: None,
        round: claim.round,
        operation_id: claim.operation_id.clone(),
    }
}

fn planned(
    db: &Db,
    key: &str,
    texts: &[&str],
    output: impl FnOnce(&W4Input) -> W4Output,
) -> (W4Claim, w4::Plan) {
    let claim = w4_claim_round(&mut db.open(), key).unwrap();
    let ctx = context(db, &claim);
    let candidates: Vec<_> = texts
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let c = supported(&ctx, t, i).contract;
            W4Candidate {
                index: i,
                committed_id: None,
                candidate: c.candidate,
                source: c.source,
                said_on: c.said_on,
                time_resolutions: c.time_resolutions,
            }
        })
        .collect();
    w4_save_inputs(&mut db.open(), &claim, &candidates, &Default::default()).unwrap();
    let pending: Vec<_> = candidates
        .iter()
        .map(|c| w4::Pending {
            index: c.index,
            committed_id: None,
            contract: ContractCandidate {
                candidate: c.candidate.clone(),
                source: c.source.clone(),
                said_on: c.said_on.clone(),
                time_resolutions: c.time_resolutions.clone(),
            },
        })
        .collect();
    let (state, a) = w4::assemble(
        &db.open(),
        key,
        None,
        &pending,
        RuntimeInputBudget::default().w4,
    )
    .unwrap();
    let plan = w4::plan(
        &w4::W4Runtime {
            state: &state,
            owner: UserId::from("alice"),
            source_key: key,
            round: claim.round,
            task_id: None,
            admissions: Default::default(),
        },
        &a.input,
        &output(&a.input),
    )
    .unwrap();
    w4_save_plan(&mut db.open(), &claim, &candidates, &plan).unwrap();
    (claim, plan)
}

#[test]
fn saved_plan_recovers_without_any_provider_or_input_budget() {
    let db = Db::new();
    let key = db.start("plan", "我喜欢咖啡。我喜欢红茶。");
    let (_, plan) = planned(&db, &key, &["我喜欢咖啡。", "我喜欢红茶。"], new_output);
    assert_eq!(plan.groups.len(), 2);
    expire(&db, &key);
    let result = run_source(
        &mut db.open(),
        &key,
        RuntimeInputBudget {
            max_raw_bytes: 0,
            max_candidates: 0,
            max_relations: 0,
            ..Default::default()
        },
        |_, _| panic!("saved plan"),
        |_, _, _| panic!("saved plan"),
    )
    .unwrap();
    assert_eq!(result["enrichment"]["status"], "committed");
    assert_eq!(
        db.open()
            .read()
            .unwrap()
            .repository
            .list_by_owner(&UserId::from("alice"))
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn first_group_crash_recovers_without_reinforcing_twice() {
    let db = Db::new();
    let id = seed(&db, "old", "我喜欢咖啡。", "fact");
    let key = db.start("partial", "我还是喜欢咖啡。我喜欢红茶。");
    let (claim, plan) = planned(
        &db,
        &key,
        &["我还是喜欢咖啡。", "我喜欢红茶。"],
        |input| {
            let mut out = new_output(input);
            out.decisions[0] = reinforce(input).decisions.remove(0);
            out
        },
    );
    assert_eq!(plan.groups.len(), 2);
    w4_commit_group(&mut db.open(), &claim, &plan.groups[0]).unwrap();
    expire(&db, &key);
    let result = run_source(
        &mut db.open(),
        &key,
        RuntimeInputBudget::default(),
        |_, _| panic!("saved plan"),
        |_, _, _| panic!("saved plan"),
    )
    .unwrap();
    assert_eq!(result["enrichment"]["status"], "committed");
    let state = db.open().read().unwrap();
    assert_eq!(
        state
            .repository
            .get(id)
            .unwrap()
            .unwrap()
            .reinforcement_count,
        1
    );
    assert_eq!(
        state
            .repository
            .list_by_owner(&UserId::from("alice"))
            .unwrap()
            .len(),
        2
    );
    assert_eq!(w4_progress(&db.open(), &key).unwrap().committed.len(), 2);
    assert!(
        w4_commit_group(&mut db.open(), &claim, &plan.groups[1])
            .unwrap_err()
            .to_string()
            .contains("claim_lost")
    );
}

#[test]
fn all_groups_crash_only_writes_summary_after_reopen() {
    let db = Db::new();
    let key = db.start("all", "我喜欢咖啡。我喜欢红茶。");
    let (claim, plan) = planned(&db, &key, &["我喜欢咖啡。", "我喜欢红茶。"], new_output);
    for group in &plan.groups {
        w4_commit_group(&mut db.open(), &claim, group).unwrap();
    }
    assert_eq!(
        db.open().read().unwrap().sources[&key].enrichment,
        "running"
    );
    let before = db.open().read().unwrap();
    expire(&db, &key);
    let result = run_source(
        &mut db.open(),
        &key,
        RuntimeInputBudget::default(),
        |_, _| panic!("saved plan"),
        |_, _, _| panic!("saved plan"),
    )
    .unwrap();
    assert_eq!(result["enrichment"]["status"], "committed");
    let after = db.open().read().unwrap();
    assert_eq!(before.w5_receipts, after.w5_receipts);
    assert_eq!(before.revision, after.revision);
    assert_eq!(
        serde_json::to_value(before.repository).unwrap(),
        serde_json::to_value(after.repository).unwrap()
    );
}

#[test]
fn saved_inputs_survive_crash_and_preserve_episode_admission() {
    for remember in [false, true] {
        let db = Db::new();
        seed(&db, "old", "我喜欢咖啡。", "likes");
        let key = db.start("episode", "我刚才喝了咖啡。");
        let crash = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_source(
                &mut db.open(),
                &key,
                RuntimeInputBudget::default(),
                |ctx, input| {
                    let mut c = supported(ctx, &input.text, 0);
                    c.contract.candidate.relation = "likes".into();
                    c.contract.candidate.memory_type = "episode".into();
                    c.admission = w4::Admission {
                        episode_useful: !remember,
                        remember_bound: remember,
                    };
                    Ok(vec![c])
                },
                |_, _, _| panic!("crash after saving inputs"),
            )
        }));
        assert!(crash.is_err());
        expire(&db, &key);
        let result = run_source(
            &mut db.open(),
            &key,
            RuntimeInputBudget::default(),
            |_, _| panic!("saved admissions"),
            |ctx, input, _| {
                assert_eq!(ctx.round, 0);
                assert_eq!(ctx.operation_id, "episode");
                Ok(Some(new_output(input)))
            },
        )
        .unwrap();
        assert_eq!(result["enrichment"]["status"], "committed");
        let id: MemoryId =
            serde_json::from_value(result["enrichment"]["candidates"][0]["memory_id"].clone())
                .unwrap();
        let memory = db
            .open()
            .read()
            .unwrap()
            .repository
            .get(id)
            .unwrap()
            .unwrap();
        assert_eq!(memory.memory_type, MemoryType::Episode);
        assert_eq!(memory.explicit_memory_request, remember);
    }
}

#[test]
fn same_round_inputs_and_admissions_cannot_be_replaced() {
    let db = Db::new();
    let key = db.start("inputs", "我喜欢咖啡。我喜欢红茶。");
    let claim = w4_claim_round(&mut db.open(), &key).unwrap();
    let ctx = context(&db, &claim);
    let c = supported(&ctx, "我喜欢咖啡。", 0).contract;
    let candidates = vec![W4Candidate {
        index: 0,
        committed_id: None,
        candidate: c.candidate,
        source: c.source,
        said_on: c.said_on,
        time_resolutions: c.time_resolutions,
    }];
    w4_save_inputs(&mut db.open(), &claim, &candidates, &Default::default()).unwrap();
    w4_save_inputs(&mut db.open(), &claim, &candidates, &Default::default()).unwrap();
    let mut changed = candidates.clone();
    changed[0].candidate.sentence.text = "我喜欢红茶。".into();
    assert!(
        w4_save_inputs(&mut db.open(), &claim, &changed, &Default::default())
            .unwrap_err()
            .to_string()
            .contains("immutable")
    );
    assert!(
        w4_save_inputs(
            &mut db.open(),
            &claim,
            &candidates,
            &std::collections::BTreeMap::from([(
                0,
                w4::Admission {
                    episode_useful: true,
                    remember_bound: false
                }
            )])
        )
        .unwrap_err()
        .to_string()
        .contains("immutable")
    );
    assert_eq!(
        w4_load_inputs(&db.open(), &claim)
            .unwrap()
            .unwrap()
            .candidates,
        candidates
    );
}

#[test]
fn late_upstream_and_w4_cannot_overwrite_a_successor_claim() {
    for in_w4 in [false, true] {
        let db = Db::new();
        if in_w4 {
            seed(&db, "old", "我喜欢咖啡。", "fact");
        }
        let key = db.start("late", "我还是喜欢咖啡。");
        let next = std::cell::RefCell::new(None::<W4Claim>);
        let takeover = || {
            expire(&db, &key);
            *next.borrow_mut() = Some(w4_claim_round(&mut db.open(), &key).unwrap());
        };
        let error = run_source(
            &mut db.open(),
            &key,
            RuntimeInputBudget::default(),
            |ctx, input| {
                if !in_w4 {
                    takeover();
                }
                Ok(vec![supported(ctx, &input.text, 0)])
            },
            |_, input, _| {
                takeover();
                Ok(Some(reinforce(input)))
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("claim_lost"));
        let state = db.open().read().unwrap();
        let successor = next.borrow();
        let successor = successor.as_ref().unwrap();
        assert_eq!(
            state.sources[&key].task.attempt_id.as_deref(),
            Some(successor.claim.attempt_id.as_str())
        );
        assert_eq!(state.sources[&key].enrichment, "running");
        assert!(!w4_progress(&db.open(), &key).unwrap().plan_ready);
        assert_eq!(
            state
                .repository
                .list_by_owner(&UserId::from("alice"))
                .unwrap()
                .len(),
            usize::from(in_w4)
        );
    }
}

fn forget_source(db: &Db, key: &str) {
    let source_id = db.open().read().unwrap().sources[key].id.clone();
    let mut service = ChatMemoryService::new(
        db.open(),
        ChatConfig {
            enrichment_mode: EnrichmentMode::Off,
            ..Default::default()
        },
    )
    .unwrap();
    service
        .call("memory_forget", json!({"source_id":source_id}), || false)
        .unwrap();
}

#[test]
fn deleting_saved_inputs_clears_metadata_and_rejects_late_w4() {
    let db = Db::new();
    seed(&db, "old", "我喜欢咖啡。", "fact");
    let key = db.start("delete", "我还是喜欢咖啡。");
    let error = run_source(
        &mut db.open(),
        &key,
        RuntimeInputBudget::default(),
        |ctx, input| Ok(vec![supported(ctx, &input.text, 0)]),
        |_, input, _| {
            forget_source(&db, &key);
            Ok(Some(reinforce(input)))
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("claim_lost"));
    let state = db.open().read().unwrap();
    let source = &state.sources[&key];
    assert_eq!(source.enrichment, "deleted");
    assert!(source.request.is_none());
    let result = source.result.clone().unwrap_or(Json::Null);
    assert!(result.get("w4_checkpoint").is_none());
    assert!(result.get("w4_input_meta").is_none());
    assert!(
        !serde_json::to_string(&state)
            .unwrap()
            .contains("我还是喜欢咖啡。")
    );
    assert_eq!(
        receipt(&db.open(), &key).unwrap()["enrichment"]["status"],
        "deleted"
    );
    assert!(
        run_source(
            &mut db.open(),
            &key,
            RuntimeInputBudget::default(),
            |_, _| panic!("deleted"),
            |_, _, _| panic!("deleted")
        )
        .is_err()
    );
}

#[test]
fn deleting_a_w4_evidence_source_does_not_apply_the_late_decision() {
    let db = Db::new();
    seed(&db, "old", "我喜欢咖啡。", "fact");
    let old = db
        .open()
        .read()
        .unwrap()
        .sources
        .iter()
        .find(|(_, s)| s.turn_id == "old")
        .unwrap()
        .0
        .clone();
    let key = db.start("late", "我还是喜欢咖啡。");
    let result = run_source(
        &mut db.open(),
        &key,
        RuntimeInputBudget::default(),
        |ctx, input| Ok(vec![supported(ctx, &input.text, 0)]),
        |_, input, _| {
            forget_source(&db, &old);
            Ok(Some(reinforce(input)))
        },
    )
    .unwrap();
    assert_eq!(result["enrichment"]["status"], "needs_rejudge");
    assert_eq!(result["enrichment"]["candidates"][0]["status"], "pending");
    assert!(result["enrichment"]["candidates"][0]["memory_id"].is_null());
    assert_eq!(
        db.open()
            .read()
            .unwrap()
            .repository
            .get(MemoryId(1))
            .unwrap()
            .unwrap()
            .status,
        MemoryStatus::Deleted
    );
}

#[test]
fn changed_target_snapshot_requires_rejudge_with_zero_new_business_writes() {
    let db = Db::new();
    let id = seed(&db, "old", "我喜欢咖啡。", "fact");
    let key = db.start("stale", "我还是喜欢咖啡。");
    let result = run_source(
        &mut db.open(),
        &key,
        RuntimeInputBudget::default(),
        |ctx, input| Ok(vec![supported(ctx, &input.text, 0)]),
        |_, input, _| {
            db.open()
                .transaction(|state| {
                    let mut memory = state.repository.get(id)?.unwrap();
                    memory.reinforcement_count = 7;
                    state.repository.update(memory)?;
                    Ok(())
                })
                .unwrap();
            Ok(Some(reinforce(input)))
        },
    )
    .unwrap();
    assert_eq!(result["enrichment"]["status"], "needs_rejudge");
    let state = db.open().read().unwrap();
    assert_eq!(
        state
            .repository
            .get(id)
            .unwrap()
            .unwrap()
            .reinforcement_count,
        7
    );
    assert_eq!(
        state
            .repository
            .list_by_owner(&UserId::from("alice"))
            .unwrap()
            .len(),
        1
    );
    assert_eq!(state.sources[&key].task.attempt_id, None);
}

#[test]
fn newer_and_sourceless_barriers_preserve_committed_candidates_only() {
    for sourceless in [false, true] {
        let db = Db::new();
        let key = db.start("partial", "我喜欢咖啡。我喜欢红茶。");
        let (claim, plan) = planned(&db, &key, &["我喜欢咖啡。", "我喜欢红茶。"], new_output);
        w4_commit_group(&mut db.open(), &claim, &plan.groups[0]).unwrap();
        if sourceless {
            db.open()
                .transaction(|state| {
                    let mut memory = state.repository.get(MemoryId(1))?.unwrap();
                    memory.reinforcement_count = 2;
                    state.repository.update(memory)?;
                    Ok(())
                })
                .unwrap();
        } else {
            seed(&db, "newer", "我拥有自行车。", "owns");
        }
        expire(&db, &key);
        let result = run_source(
            &mut db.open(),
            &key,
            RuntimeInputBudget::default(),
            |_, _| panic!("saved plan"),
            |_, _, _| panic!("saved plan"),
        )
        .unwrap();
        assert_eq!(result["enrichment"]["status"], "needs_rejudge");
        assert_eq!(result["enrichment"]["candidates"][0]["status"], "committed");
        assert_eq!(result["enrichment"]["candidates"][1]["status"], "pending");
        assert_eq!(
            result["enrichment"]["candidates"][1]["reasons"],
            json!(["needs_rejudge"])
        );
        assert_eq!(w4_progress(&db.open(), &key).unwrap().committed.len(), 1);
    }
}

#[test]
fn another_source_cannot_borrow_the_same_round_resume_permission() {
    let db = Db::new();
    let older = db.start("older", "我喜欢绿茶。");
    seed(&db, "newer", "我喜欢咖啡。", "fact");
    let result = run_source(
        &mut db.open(),
        &older,
        RuntimeInputBudget::default(),
        |_, _| panic!("barrier before upstream"),
        |_, _, _| panic!("barrier before W4"),
    )
    .unwrap();
    assert_eq!(result["enrichment"]["status"], "needs_rejudge");
    assert!(
        w4_progress(&db.open(), &older)
            .unwrap()
            .candidates
            .is_empty()
    );
}

#[test]
fn explicit_rejudge_resets_input_meta_and_inherits_product_binding() {
    let db = Db::new();
    let id = seed(&db, "first", "我喜欢咖啡。", "fact");
    let key = db
        .open()
        .read()
        .unwrap()
        .sources
        .iter()
        .find(|(_, s)| s.turn_id == "first")
        .unwrap()
        .0
        .clone();
    let before = db.open().read().unwrap().sources[&key].clone();
    assert_eq!(
        w4_start_round(&mut db.open(), &key, "review", true).unwrap(),
        1
    );
    assert!(
        db.open().read().unwrap().sources[&key]
            .result
            .as_ref()
            .unwrap()
            .get("w4_input_meta")
            .is_none()
    );
    let result = run_source(
        &mut db.open(),
        &key,
        RuntimeInputBudget::default(),
        |ctx, input| {
            assert_eq!(ctx.round, 1);
            assert_eq!(ctx.operation_id, "review");
            assert_eq!(ctx.observed_at, before.observed_at);
            assert_eq!(ctx.received_sequence, before.received_sequence);
            Ok(vec![supported(ctx, &input.text, 0)])
        },
        |_, input, _| {
            assert_eq!(input.candidates[0].committed_id, Some(id));
            Ok(Some(new_output(input)))
        },
    )
    .unwrap();
    assert_eq!(
        result["enrichment"]["candidates"][0]["memory_id"],
        serde_json::to_value(id).unwrap()
    );
    assert_eq!(result["enrichment"]["status"], "committed");
    let state = db.open().read().unwrap();
    assert_eq!(
        state
            .repository
            .list_by_owner(&UserId::from("alice"))
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        state
            .repository
            .get(id)
            .unwrap()
            .unwrap()
            .reinforcement_count,
        0
    );
    assert_eq!(state.sources[&key].task.round, Some(1));
}

#[test]
fn provider_timeout_and_invalid_output_leave_t1_and_retryable_inputs() {
    for stage in ["upstream", "w4"] {
        let db = Db::new();
        if stage == "w4" {
            seed(&db, "old", "我喜欢咖啡。", "fact");
        }
        let key = db.start("failed", "我还是喜欢咖啡。");
        let result = run_source(
            &mut db.open(),
            &key,
            RuntimeInputBudget::default(),
            |ctx, input| {
                if stage == "upstream" {
                    return Err("model_timeout".into());
                }
                Ok(vec![supported(ctx, &input.text, 0)])
            },
            |_, _, _| Err("model_invalid_output".into()),
        )
        .unwrap();
        assert_eq!(result["enrichment"]["status"], "failed");
        let state = db.open().read().unwrap();
        assert!(state.sources[&key].request.is_some());
        assert_eq!(state.sources[&key].task.attempt_id, None);
        assert_eq!(
            state.sources[&key].error.as_deref(),
            Some(if stage == "upstream" {
                "model_timeout"
            } else {
                "model_invalid_output"
            })
        );
        assert_eq!(
            w4_progress(&db.open(), &key).unwrap().candidates.len(),
            usize::from(stage == "w4")
        );
    }
}

#[test]
fn injected_state_change_fixture_updates_the_real_sqlite_chain() {
    let db = Db::new();
    let sample: Json =
        serde_json::from_str(include_str!("eval/w4/samples/w4_01_state_change.json")).unwrap();
    let old = db.capture_at("beijing", "我住在北京。", &[], Some(1_772_380_800));
    w4_start_round(&mut db.open(), &old, "beijing", false).unwrap();
    let prior = run_source(
        &mut db.open(),
        &old,
        RuntimeInputBudget::default(),
        |ctx, _| {
            Ok(vec![SupportedCandidate {
                index: 0,
                contract: ContractCandidate {
                    candidate: serde_json::from_value(
                        sample["input"]["records"][0]["candidate"].clone(),
                    )
                    .unwrap(),
                    source: SourceRef::Stored {
                        source_id: ctx.source_id.clone(),
                    },
                    said_on: "2026-03-02".into(),
                    time_resolutions: vec![],
                },
                admission: Default::default(),
            }])
        },
        |_, _, _| panic!("no W4"),
    )
    .unwrap();
    let old_id: MemoryId =
        serde_json::from_value(prior["enrichment"]["candidates"][0]["memory_id"].clone()).unwrap();
    let key = db.capture_at(
        "shanghai",
        "我上个月从北京搬到上海了。",
        &[],
        Some(1_788_998_400),
    );
    w4_start_round(&mut db.open(), &key, "shanghai", false).unwrap();
    let result = run_source(
        &mut db.open(),
        &key,
        RuntimeInputBudget::default(),
        |ctx, _| {
            let c: W4Candidate =
                serde_json::from_value(sample["input"]["candidates"][0].clone()).unwrap();
            Ok(vec![SupportedCandidate {
                index: 0,
                contract: ContractCandidate {
                    candidate: c.candidate,
                    source: SourceRef::Stored {
                        source_id: ctx.source_id.clone(),
                    },
                    said_on: c.said_on,
                    time_resolutions: c.time_resolutions,
                },
                admission: Default::default(),
            }])
        },
        |ctx, input, texts| {
            assert_eq!(texts.len(), 2);
            let mut fixture: W4Output = serde_json::from_str(include_str!(
                "eval/w4/fixtures/w4_01_state_change.output.json"
            ))
            .unwrap();
            let old_view: Json = serde_json::to_value(&input.records[0]).unwrap();
            fixture.decisions[0].targets = vec![old_id];
            fixture.decisions[0].basis[0].source =
                serde_json::from_value(old_view["evidence"][0]["source"].clone()).unwrap();
            fixture.decisions[0].basis[1].source = SourceRef::Stored {
                source_id: ctx.source_id.clone(),
            };
            Ok(Some(fixture))
        },
    )
    .unwrap();
    assert_eq!(result["enrichment"]["status"], "committed");
    let state = db.open().read().unwrap();
    let memory = state.repository.get(old_id).unwrap().unwrap();
    assert_eq!(memory.status, MemoryStatus::Superseded);
    assert_eq!(memory.temporal.valid_to, None); // 月精度保留在回执，不虚造时间点。
    let id: MemoryId =
        serde_json::from_value(result["enrichment"]["candidates"][0]["memory_id"].clone()).unwrap();
    let new = state.repository.get(id).unwrap().unwrap();
    assert_eq!(new.supersedes, Some(old_id));
    assert_eq!(new.temporal.observed_at, Timestamp(1_788_998_400));
    assert_eq!(
        state.w5_receipts[&format!("{key}#0#0")].decisions[0]
            .effective_time
            .as_ref()
            .unwrap()
            .resolution
            .precision,
        Precision::Month
    );
}

#[test]
fn mandatory_record_overflow_preserves_the_existing_scope_pending_rule() {
    let db = Db::new();
    seed(&db, "old", "我喜欢咖啡。", "likes");
    let key = db.start("records", "我喜欢红茶。");
    let result = run_source(
        &mut db.open(),
        &key,
        RuntimeInputBudget {
            w4: w4::W4Budget {
                facts: 64,
                records: 0,
            },
            ..Default::default()
        },
        |ctx, input| {
            let mut c = supported(ctx, &input.text, 0);
            c.contract.candidate.relation = "likes".into();
            Ok(vec![c])
        },
        |_, _, _| panic!("mandatory overflow skips W4"),
    )
    .unwrap();
    assert_eq!(result["enrichment"]["status"], "committed");
    assert_eq!(
        result["enrichment"]["candidates"][0]["update"]["reasons"],
        json!(["update_scope_incomplete"])
    );
    assert_eq!(
        db.open()
            .read()
            .unwrap()
            .repository
            .list_by_owner(&UserId::from("alice"))
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn foreign_deletion_invalidates_saved_plan_without_raw_input_meta_copies() {
    let db = Db::new();
    seed(&db, "old", "我喜欢咖啡。", "fact");
    let old = db
        .open()
        .read()
        .unwrap()
        .sources
        .iter()
        .find(|(_, s)| s.turn_id == "old")
        .unwrap()
        .0
        .clone();
    let key = db.start("plan", "我还是喜欢咖啡。");
    let (claim, plan) = planned(&db, &key, &["我还是喜欢咖啡。"], reinforce);
    forget_source(&db, &old);
    let state = db.open().read().unwrap();
    assert!(
        !serde_json::to_string(&state)
            .unwrap()
            .contains("我喜欢咖啡。")
    );
    let progress = w4_progress(&db.open(), &key).unwrap();
    assert!(!progress.plan_ready);
    assert!(progress.invalidated.is_some());
    assert!(progress.remaining.is_empty());
    assert_eq!(state.sources[&key].enrichment, "needs_rejudge");
    assert_eq!(
        state.sources[&key].result.as_ref().unwrap()["w4_input_meta"]["admissions"],
        json!({})
    );
    assert!(
        w4_commit_group(&mut db.open(), &claim, &plan.groups[0])
            .unwrap_err()
            .to_string()
            .contains("claim_lost")
    );
    assert!(
        run_source(
            &mut db.open(),
            &key,
            RuntimeInputBudget::default(),
            |_, _| panic!("invalidated"),
            |_, _, _| panic!("invalidated")
        )
        .unwrap_err()
        .to_string()
        .contains("needs_rejudge")
    );
}

#[test]
fn rejudge_after_deletion_reads_invalidated_relation_without_deleted_raw_text() {
    let db = Db::new();
    conflict_seed(&db);
    let state = db.open().read().unwrap();
    let old = state
        .sources
        .iter()
        .find(|(_, s)| s.turn_id == "old")
        .unwrap()
        .0
        .clone();
    let key = state
        .sources
        .iter()
        .find(|(_, s)| s.turn_id == "conflict")
        .unwrap()
        .0
        .clone();
    let old_id = state.sources[&old].id.clone();
    forget_source(&db, &old);
    w4_start_round(&mut db.open(), &key, "review_deleted", true).unwrap();
    let result = run_source(
        &mut db.open(),
        &key,
        RuntimeInputBudget::default(),
        |ctx, input| {
            let mut c = supported(ctx, &input.text, 0);
            c.contract.candidate.relation = "likes".into();
            Ok(vec![c])
        },
        |_, input, texts| {
            assert_eq!(input.relations.len(), 1);
            assert_eq!(
                input.relations[0].status,
                hippocampus::contract::RelationStatus::Invalidated
            );
            assert!(!texts.contains_key(&old_id));
            assert_eq!(texts.len(), 1);
            Ok(Some(new_output(input)))
        },
    )
    .unwrap();
    assert_eq!(result["enrichment"]["status"], "committed");
    assert_eq!(result["enrichment"]["candidates"][0]["memory_id"], 2);
    assert_eq!(
        db.open()
            .read()
            .unwrap()
            .repository
            .list_by_owner(&UserId::from("alice"))
            .unwrap()
            .iter()
            .filter(|m| m.status == MemoryStatus::Active)
            .count(),
        1
    );
}

#[test]
fn provider_error_details_do_not_create_raw_copies_outside_checkpoints() {
    let db = Db::new();
    seed(&db, "old", "我喜欢咖啡。", "fact");
    let old = db
        .open()
        .read()
        .unwrap()
        .sources
        .iter()
        .find(|(_, s)| s.turn_id == "old")
        .unwrap()
        .0
        .clone();
    let key = db.start("bad", "我还是喜欢咖啡。");
    let result = run_source(
        &mut db.open(),
        &key,
        RuntimeInputBudget::default(),
        |ctx, input| Ok(vec![supported(ctx, &input.text, 0)]),
        |_, _, _| Err("model_invalid_output: unknown variant 我喜欢咖啡。".into()),
    )
    .unwrap();
    assert_eq!(result["enrichment"]["status"], "failed");
    forget_source(&db, &old);
    let state = db.open().read().unwrap();
    assert!(
        !serde_json::to_string(&state)
            .unwrap()
            .contains("我喜欢咖啡。")
    );
    assert_eq!(
        state.sources[&key].error.as_deref(),
        Some("model_invalid_output")
    );
}
