//! W4 v5 离线验收：输入组装 → 输出校验 → 转换为 W5 组 → 真实提交与重开。
//!
//! 不调用模型。每个样本按其输入在真实 SQLite 里建库，运行组装并与样本输入比对，再把
//! `tests/eval/w4/fixtures/` 里的模型输出夹具（由期望决定生成，样本本身不改）交给校验与转换，
//! 提交后重新打开数据库，对 `expect.after` 做可执行断言。
//!
//! 范围说明：
//! - 自然语言 `after.recall` 不自动评分；AF276 另验收生产 R3/R5 的身份与极性传播。
//! - 样本的 `must_not` 是自然语言；能落到持久状态的由下面的字段断言覆盖，个别在
//!   `extra_checks` 里单独断言。
//! - 样本按 AF276 已复核；注入输出只能证明执行规则，不能证明模型语义质量。
//! - w4_09 不比对组装结果（用户同意的例外）：样本为了测试“检索到无关记录时判为 new”，
//!   送入了一条按取数规则取不到的其他关系记录。该样本直接用样本输入验证校验、转换与提交。

include!("eval/w4/runtime.rs");

#[test]
fn equally_relevant_fact_hits_follow_recency_before_the_record_budget() {
    let db = Db::new();
    let older = seed_fact(&db, "older", "coffee tea", "coffee tea");
    let newer = seed_fact(&db, "newer", "coffee tea", "coffee tea");
    claim_source(&db, "query", "coffee tea");
    let input = W4Input {
        candidates: vec![fact_candidate("query", "coffee tea")],
        records: vec![],
        relations: vec![],
        coverage: W4Coverage {
            complete: false,
            omitted: 0,
        },
    };
    let state = db.state();
    let assembly = assemble(
        &db,
        &runtime(&state, "query", 1, None),
        &pending_of(&input),
        W4Budget {
            facts: 1,
            records: 64,
        },
    )
    .unwrap();
    assert_eq!(ids(&assembly.input).0, [newer.0]);
    assert!(!ids(&assembly.input).0.contains(&older.0));
    assert_eq!(assembly.input.coverage.omitted, 1);
    assert_eq!(assembly.fact_retrieval[&0], FactRetrieval::FullText);
}

#[test]
fn older_fact_with_more_matching_phrases_wins_the_record_budget() {
    let db = Db::new();
    let older = seed_fact(&db, "older", "周末喜欢喝咖啡", "周末喜欢喝咖啡");
    let newer = seed_fact(&db, "newer", "周末散步", "周末散步");
    claim_source(&db, "query", "周末喜欢喝咖啡");
    let (_, assembly) = hippocampus::w4::assemble(
        &db.open(),
        "query",
        None,
        &fact_pending("query", "周末喜欢喝咖啡"),
        W4Budget {
            facts: 1,
            records: 64,
        },
    )
    .unwrap();
    assert_eq!(ids(&assembly.input).0, [older.0]);
    assert!(!ids(&assembly.input).0.contains(&newer.0));
    assert_eq!(assembly.input.coverage.omitted, 1);
    assert_eq!(assembly.fact_retrieval[&0], FactRetrieval::FullText);
}

#[test]
fn registered_entity_alias_matches_rank_before_generic_phrase_matches() {
    let db = Db::new();
    let older = seed_fact(&db, "older", "小石项目", "小石项目");
    let newer = seed_fact(&db, "newer", "coffee tea", "coffee tea");
    db.open()
        .transaction(|state| {
            state.entities.upsert(Entity {
                id: EntityId::from("project"),
                owner_id: UserId::from(OWNER),
                entity_type: EntityType::Other("project".into()),
                canonical_name: "松石47".into(),
                aliases: vec!["小石项目".into()],
            });
            Ok(())
        })
        .unwrap();
    claim_source(&db, "query", "松石47 coffee tea");
    let (_, assembly) = hippocampus::w4::assemble(
        &db.open(),
        "query",
        None,
        &fact_pending("query", "松石47 coffee tea"),
        W4Budget {
            facts: 1,
            records: 64,
        },
    )
    .unwrap();
    assert_eq!(ids(&assembly.input).0, [older.0]);
    assert!(!ids(&assembly.input).0.contains(&newer.0));
    assert_eq!(assembly.input.coverage.omitted, 1);
    assert_eq!(assembly.fact_retrieval[&0], FactRetrieval::FullText);
}

#[test]
fn overlapping_source_hits_consume_one_record_slot() {
    let db = Db::new();
    seed_fact(&db, "older", "coffee tea", "coffee tea");
    // 片段长度 768、重叠 128；此证据同时完整落在两个片段里。
    let raw = format!(
        "{}coffee tea{}",
        "padding ".repeat(90),
        " filler".repeat(90)
    );
    let newer = seed_fact(&db, "newer", "coffee tea", &raw);
    claim_source(&db, "query", "coffee tea");
    let candidate = fact_candidate("query", "coffee tea");
    let pending = vec![Pending {
        index: 0,
        committed_id: None,
        contract: ContractCandidate {
            candidate: candidate.candidate,
            source: candidate.source,
            said_on: candidate.said_on,
            time_resolutions: candidate.time_resolutions,
        },
    }];
    let (_, assembly) = hippocampus::w4::assemble(
        &db.open(),
        "query",
        None,
        &pending,
        W4Budget {
            facts: 1,
            records: 1,
        },
    )
    .unwrap();
    assert_eq!(ids(&assembly.input).0, [newer.0]);
    assert_eq!(assembly.input.coverage.omitted, 1);
    assert_eq!(assembly.fact_retrieval[&0], FactRetrieval::FullText);
}

#[test]
fn fact_filters_apply_before_full_text_hits_are_truncated() {
    let db = Db::new();
    let global = seed_fact(&db, "global", "coffee tea", "coffee tea");
    let mine = seed_fact(&db, "mine", "coffee tea", "coffee tea");
    let outside = seed_fact(&db, "outside", "coffee tea", "coffee tea");
    let another_subject = seed_fact(&db, "subject", "coffee tea", "coffee tea");
    let another_relation = seed_fact(&db, "relation", "coffee tea", "coffee tea");
    let deleted = seed_fact(&db, "deleted", "coffee tea", "coffee tea");
    db.open()
        .transaction(|state| {
            for (id, task) in [(mine, "mine"), (outside, "other")] {
                let mut memory = state.repository.get(id)?.unwrap();
                memory
                    .scope
                    .conditions
                    .push(ScopeCondition::Task(TaskId::from(task)));
                state.repository.update(memory)?;
            }
            let mut memory = state.repository.get(another_subject)?.unwrap();
            memory.subject_id = EntityId::from("someone-else");
            state.repository.update(memory)?;
            let mut memory = state.repository.get(another_relation)?.unwrap();
            memory.predicate = Predicate::from("likes");
            state.repository.update(memory)?;
            let mut memory = state.repository.get(deleted)?.unwrap();
            memory.status = MemoryStatus::Deleted;
            state.repository.update(memory)?;
            Ok(())
        })
        .unwrap();
    claim_source(&db, "query", "coffee tea");
    let pending = fact_pending("query", "coffee tea");
    let (_, assembly) = hippocampus::w4::assemble(
        &db.open(),
        "query",
        Some(&TaskId::from("mine")),
        &pending,
        W4Budget {
            facts: 1,
            records: 1,
        },
    )
    .unwrap();
    assert_eq!(ids(&assembly.input).0, [mine.0]);
    assert_eq!(
        assembly.input.coverage.omitted, 1,
        "only the global fact was truncated"
    );
    assert_eq!(assembly.fact_retrieval[&0], FactRetrieval::FullText);
    let (_, unscoped) =
        hippocampus::w4::assemble(&db.open(), "query", None, &pending, BUDGET).unwrap();
    assert_eq!(ids(&unscoped.input).0, [global.0]);
    assert_eq!(unscoped.input.coverage.omitted, 0);
}

#[test]
fn a_completed_full_text_search_can_have_no_record_hits_in_an_over_budget_scope() {
    let db = Db::new();
    seed_fact(&db, "older", "coffee tea", "coffee tea");
    claim_source(&db, "query", "mocha");
    let (_, assembly) = hippocampus::w4::assemble(
        &db.open(),
        "query",
        None,
        &fact_pending("query", "mocha"),
        W4Budget {
            facts: 0,
            records: 64,
        },
    )
    .unwrap();
    assert_eq!(assembly.fact_retrieval[&0], FactRetrieval::FullText);
    assert!(assembly.input.records.is_empty());
    assert_eq!(
        assembly.input.coverage,
        W4Coverage {
            complete: false,
            omitted: 1
        }
    );
    assert!(!assembly.call_w4);
}

#[test]
fn relation_members_and_repeated_fact_hits_are_counted_once() {
    let db = Db::new();
    let a = seed_fact(&db, "older", "coffee tea", "coffee tea");
    let b = seed_fact(&db, "newer", "coffee tea", "coffee tea");
    open_relation(&db, "rel-1", &[a, b]);
    claim_source(&db, "query", "coffee tea");
    let mut pending = fact_pending("query", "coffee tea");
    let (_, assembly) = hippocampus::w4::assemble(
        &db.open(),
        "query",
        None,
        &pending,
        W4Budget {
            facts: 1,
            records: 2,
        },
    )
    .unwrap();
    assert_eq!(ids(&assembly.input), (vec![a.0, b.0], vec!["rel-1".into()]));
    assert_eq!(
        assembly.input.coverage.omitted, 0,
        "both members are actually in the input"
    );
    let mut second = pending[0].clone();
    second.index = 1;
    pending.push(second);
    let (_, zero) = hippocampus::w4::assemble(
        &db.open(),
        "query",
        None,
        &pending,
        W4Budget {
            facts: 8,
            records: 0,
        },
    )
    .unwrap();
    assert_eq!(
        zero.input.coverage.omitted, 2,
        "two omitted records, not four candidate hits"
    );
}

#[test]
fn unavailable_full_text_search_reports_its_fallback_without_repairing_the_index() {
    for reason in [
        "source_search_disabled",
        "source_index_missing",
        "source_index_version_mismatch",
    ] {
        let db = Db::new();
        let existing = seed_fact(&db, "older", "coffee tea", "coffee tea");
        seed_fact(&db, "unmatched", "a blue scarf", "a blue scarf");
        claim_source(&db, "query", "coffee tea");
        let mut store = db.open();
        match reason {
            "source_search_disabled" => store.set_source_search_enabled(false),
            "source_index_missing" => {
                rusqlite::Connection::open(&db.0)
                    .unwrap()
                    .execute("DELETE FROM source_indexes WHERE owner_id=?1", [OWNER])
                    .unwrap();
            }
            _ => {
                rusqlite::Connection::open(&db.0)
                    .unwrap()
                    .execute(
                        "UPDATE source_indexes SET processing_version='stale' WHERE owner_id=?1",
                        [OWNER],
                    )
                    .unwrap();
            }
        }
        let before = serde_json::to_value(store.read().unwrap()).unwrap();
        let pending = fact_pending("query", "coffee tea");
        let (_, assembly) = hippocampus::w4::assemble(
            &store,
            "query",
            None,
            &pending,
            W4Budget {
                facts: 1,
                records: 64,
            },
        )
        .unwrap();
        assert_eq!(
            assembly.fact_retrieval[&0],
            FactRetrieval::Fallback(reason.into())
        );
        assert_eq!(ids(&assembly.input).0, [existing.0]);
        assert!(!assembly.input.coverage.complete);
        // 零预算仍按唯一记录计 omitted，降级不绕过预算。
        let (_, zero) = hippocampus::w4::assemble(
            &store,
            "query",
            None,
            &pending,
            W4Budget {
                facts: 1,
                records: 0,
            },
        )
        .unwrap();
        assert_eq!(zero.input.coverage.omitted, 2);
        assert!(zero.input.records.is_empty());
        assert_eq!(zero.fact_retrieval, assembly.fact_retrieval);
        assert_eq!(serde_json::to_value(store.read().unwrap()).unwrap(), before);
    }
}

#[test]
fn every_sample_assembles_validates_commits_and_matches_its_expected_state() {
    let ids: Vec<String> = samples()
        .iter()
        .map(|s| s["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(ids.len(), 28);
    for id in ids {
        let run = run(&id);
        check_after(&run);
        extra_checks(&run);
    }
}

/// 个别 `must_not` 与样本说明里、字段断言没有覆盖的持久状态。
fn extra_checks(run: &Run) {
    let id = run.sample["id"].as_str().unwrap();
    let state = &run.after;
    match id {
        "w4_01_state_change" => {
            // 结束时间取 effective：2026 年 8 月、月精度；不回填接收日，也不补成具体日。
            let Some(RecordView2::Current(view)) = record_view(state, MemoryId(17)) else {
                panic!("{id}: view of 17");
            };
            let end = view.end.expect("known end");
            assert_eq!(end.precision, Precision::Month);
            assert_eq!(
                (end.start, end.end_exclusive),
                (Some(1785513600), Some(1788192000))
            );
            assert_eq!(memory(state, MemoryId(17)).temporal.valid_to, None);
            assert_eq!(
                memory(state, MemoryId(17)).supersede_reason,
                Some(SupersedeReason::StateChanged)
            );
        }
        "w4_04_coexist_partial" => {
            // 不得结束 31。
            assert_eq!(memory(state, MemoryId(31)).status, MemoryStatus::Active);
        }
        "w4_07_conflict" => {
            // 不得结束或取代 50；新记录保留不确定表达。
            assert_eq!(memory(state, MemoryId(50)).status, MemoryStatus::Active);
            let new = run.product(0).unwrap();
            assert!(
                w5_record_contract(state, new)
                    .unwrap()
                    .candidate
                    .uncertainty
                    .is_some()
            );
        }
        "w4_13_state_change_date_unknown" => {
            // 结束时间未知：不回填接收时间。
            let Some(RecordView2::Current(view)) = record_view(state, MemoryId(64)) else {
                panic!("{id}: view of 64");
            };
            assert_eq!((view.ended, view.end), (Ended::EndUnknown, None));
        }
        "w4_21_confirm_conflict_side" => {
            // 同一原文的两个决定在同一组、同一事务里提交。
            assert_eq!(run.receipts.len(), 1);
            assert_eq!(run.receipts[0].decisions.len(), 2);
        }
        "w4_22_replace_conflict_members" => {
            // 不由代码自动解决 rel-2，成员不机械换成替代记录。
            let relation = &state.relations["rel-2"];
            assert_eq!(relation.status, hippocampus::RelationStatus::Open);
            assert_eq!(relation.members, vec![MemoryId(110), MemoryId(111)]);
        }
        _ => {}
    }
    let _ = &run.db;
}

// ---------------- 变体：现有样本没有覆盖的结束时间、冲突范围、分组与准入

/// w4_01 的变体：把“上个月”换成给定的时间表达与换算结果。
fn w4_01_with_time(interp: Json, resolution: Json) -> Run {
    let mut sample = sample("w4_01_state_change");
    let time = json!({"role": "event", "quote": {"text": "上个月"}, "interp": interp, "anchor": "said_on"});
    sample["input"]["candidates"][0]["candidate"]["times"] = json!([time]);
    sample["input"]["candidates"][0]["time_resolutions"] = json!([resolution]);
    let mut output = fixture("w4_01_state_change");
    output["decisions"][0]["effective"] = time;
    run_with(sample, Some(output))
}

#[test]
fn the_end_time_keeps_its_precision_and_only_a_single_day_becomes_a_point() {
    let day = 86_400;
    let start = 1_785_513_600;
    let view_of_17 = |run: &Run| match record_view(&run.after, MemoryId(17)) {
        Some(RecordView2::Current(view)) => view,
        other => panic!("{other:?}"),
    };
    // 恰好一天：同时写旧 Memory 的结束时间点。
    let run = w4_01_with_time(
        json!({"kind": "relative", "unit": "day", "offset": -1}),
        json!({"status": "resolved", "start": start, "end_exclusive": start + day, "precision": "day"}),
    );
    let view = view_of_17(&run);
    assert_eq!(view.ended, Ended::Known);
    assert_eq!(view.end.unwrap().precision, Precision::Day);
    assert_eq!(
        memory(&run.after, MemoryId(17))
            .temporal
            .valid_to
            .map(|v| v.at),
        Some(Timestamp(start))
    );
    // 多日区间：结束时间已知，但不折成一个点。
    let run = w4_01_with_time(
        json!({"kind": "range", "start": "2026-08-01", "end_inclusive": "2026-08-14"}),
        json!({"status": "resolved", "start": start, "end_exclusive": start + 14 * day, "precision": "day"}),
    );
    let view = view_of_17(&run);
    assert_eq!(view.ended, Ended::Known);
    assert_eq!(
        view.end.as_ref().unwrap().end_exclusive,
        Some(start + 14 * day)
    );
    assert_eq!(memory(&run.after, MemoryId(17)).temporal.valid_to, None);
    // 无法换算：结束时间未知，不补造。
    let run = w4_01_with_time(
        json!({"kind": "unknown"}),
        json!({"status": "unknown", "start": null, "end_exclusive": null, "precision": "unknown"}),
    );
    let view = view_of_17(&run);
    assert_eq!((view.ended, view.end), (Ended::EndUnknown, None));
    assert_eq!(memory(&run.after, MemoryId(17)).temporal.valid_to, None);
}

#[test]
fn a_month_precision_end_is_not_an_open_ended_fact_on_the_legacy_read_paths() {
    // w4_01：北京结束于 2026 年 8 月（月精度），旧 Memory 没有 valid_to。旧读取路径不能把它
    // 当作没有上界的历史事实：管理视图带出结束时间，答复门把它当作限定语无法表达的记录。
    let run = run("w4_01_state_change");
    let mut service =
        hippocampus::chat::ChatMemoryService::new(run.db.open(), Default::default()).unwrap();
    let listed = service
        .call("memory_list", json!({"include_history": true}), || false)
        .unwrap();
    let item = listed["memories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == 17)
        .unwrap_or_else(|| panic!("{listed}"));
    assert_eq!(
        item["qualifiers_unmapped"]["end"]["precision"], "month",
        "{item}"
    );
}

#[test]
fn a_conflict_scope_copies_the_referenced_qualifiers_into_the_relation() {
    // w4_07 的变体：新候选带条件“之前记的”，W4 引用它作为冲突的适用范围。
    let mut sample = sample("w4_07_conflict");
    let condition = json!({"quote": {"text": "之前记的"}, "resolved": true});
    sample["input"]["candidates"][0]["candidate"]["conditions"] = json!([condition]);
    let mut output = fixture("w4_07_conflict");
    output["decisions"][0]["relation_scope"] =
        json!({"conditions": [{"of": "candidate", "index": 0}], "times": []});
    let run = run_with(sample, Some(output));
    let relation = run.after.relations.values().next().unwrap();
    assert_eq!(relation.conditions, json!([condition]));
    assert_eq!(relation.times, json!([]));
    // 引用关系随决定回执保存，重开后仍在。
    let scope = run.receipts[0].decisions[0].relation_scope.clone().unwrap();
    assert_eq!(scope.conditions[0].of, QualifierOwner::Candidate);
    let stored = &run.after.w5_receipts[&run.receipts[0].group_id];
    assert_eq!(stored.decisions[0].relation_scope, Some(scope));
    assert_eq!(stored.decisions[0].basis.len(), 2);
}

/// w4_08（没有相关记录）的原文与候选，用来构造多候选的转换输入。
fn two_unrelated_candidates(second_type: &str) -> (Db, String, W4Input, W4Output) {
    let sample = sample("w4_08_scope_incomplete_no_records");
    let db = Db::new();
    let own = seed(&db, &sample);
    let mut input: W4Input = serde_json::from_value(sample["input"].clone()).unwrap();
    let mut second = input.candidates[0].clone();
    second.index = 1;
    second.candidate.memory_type = second_type.into();
    input.candidates.push(second);
    let output = W4Output {
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
    };
    (db, own, input, output)
}

#[test]
fn independent_decisions_form_separate_groups_and_the_round_is_part_of_the_group_id() {
    let (db, own, input, output) = two_unrelated_candidates("fact");
    let state = db.state();
    let first = plan(&runtime(&state, &own, 1, None), &input, &output).unwrap();
    let ids: Vec<&str> = first.groups.iter().map(|g| g.id.as_str()).collect();
    assert_eq!(ids, [format!("{own}#1#0"), format!("{own}#1#1")]);
    // 同一轮重试得到相同的组；重判换轮次，组 ID 不同，不会撞上旧回执。
    assert_eq!(
        plan(&runtime(&state, &own, 1, None), &input, &output).unwrap(),
        first
    );
    let again = plan(&runtime(&state, &own, 2, None), &input, &output).unwrap();
    assert_eq!(again.groups[0].id, format!("{own}#2#0"));
}

#[test]
fn an_episode_enters_w5_only_with_a_reason_to_keep_it() {
    let (db, own, input, output) = two_unrelated_candidates("episode");
    let state = db.state();
    // 没有跨轮价值依据：只保留原文，不进入 W5；同批的普通事实不受影响。
    let plain = plan(&runtime(&state, &own, 1, None), &input, &output).unwrap();
    assert_eq!(plain.held, BTreeMap::from([(1, Held::EpisodeNotAdmitted)]));
    assert_eq!(plain.groups.len(), 1);
    for admission in [
        Admission {
            episode_useful: true,
            remember_bound: false,
        },
        Admission {
            episode_useful: false,
            remember_bound: true,
        },
    ] {
        let mut rt = runtime(&state, &own, 1, None);
        rt.admissions.insert(1, admission);
        let admitted = plan(&rt, &input, &output).unwrap();
        assert!(admitted.held.is_empty());
        assert_eq!(admitted.groups.len(), 2);
        assert_eq!(
            admitted.groups[1].candidates[0]
                .candidate
                .explicit_memory_request,
            admission.remember_bound
        );
    }
}

// ---------------- 反例：不合规的 W4 输出整份拒绝

fn rejected(id: &str, edit: impl FnOnce(&mut Json, &mut Json)) -> String {
    let mut sample = sample(id);
    let mut output = fixture(id);
    edit(&mut sample, &mut output);
    let input: W4Input = serde_json::from_value(sample["input"].clone()).unwrap();
    let Ok(output) = serde_json::from_value::<W4Output>(output) else {
        return "decode".into();
    };
    let sources = sample["sources"].clone();
    let text_of = |sid: &str| sources[sid]["text"].as_str().map(str::to_owned);
    match validate(&input, &output, &text_of) {
        Ok(_) => panic!("{id}: the broken output was accepted"),
        Err(error) => error.to_string(),
    }
}

#[test]
fn w4_rejects_two_terminal_decisions_for_the_same_target() {
    for first in [
        Action::StateChange,
        Action::Correction,
        Action::Refine,
        Action::Retract,
    ] {
        for second in [
            Action::StateChange,
            Action::Correction,
            Action::Refine,
            Action::Retract,
        ] {
            let s = sample("w4_01_state_change");
            let mut input: W4Input = serde_json::from_value(s["input"].clone()).unwrap();
            let mut output: W4Output =
                serde_json::from_value(fixture("w4_01_state_change")).unwrap();
            let mut candidate = input.candidates[0].clone();
            candidate.index = 1000;
            input.candidates[0].candidate.kind = if first == Action::Retract {
                CandidateKind::Retraction
            } else {
                CandidateKind::Assertion
            };
            candidate.candidate.kind = if second == Action::Retract {
                CandidateKind::Retraction
            } else {
                CandidateKind::Assertion
            };
            input.candidates.push(candidate);
            output.decisions[0].action = Some(first);
            let mut decision = output.decisions[0].clone();
            decision.candidate = 1000;
            decision.action = Some(second);
            output.decisions.push(decision);
            let text_of = |id: &str| s["sources"][id]["text"].as_str().map(str::to_owned);
            let error = validate(&input, &output, &text_of).unwrap_err();
            assert!(
                error.0.contains("重复结束目标"),
                "{first:?}/{second:?}: {error}"
            );
            let pending = output.decisions.last_mut().unwrap();
            pending.status = DecisionStatus::Pending;
            pending.action = None;
            pending.pending_reason = Some(PendingReason::UnclearRelation);
            pending.effective = None;
            pending.relation_scope = None;
            pending.resolves_relations.clear();
            validate(&input, &output, &text_of).unwrap();
        }
    }
}

#[test]
fn invalid_outputs_are_rejected_as_a_whole() {
    let cases: Vec<(&str, &str, Box<dyn FnOnce(&mut Json, &mut Json)>, &str)> = vec![
        (
            "重复决定",
            "w4_01_state_change",
            Box::new(|_, o| {
                let d = o["decisions"][0].clone();
                o["decisions"].as_array_mut().unwrap().push(d);
            }),
            "恰好一个决定",
        ),
        (
            "遗漏决定",
            "w4_21_confirm_conflict_side",
            Box::new(|_, o| {
                o["decisions"].as_array_mut().unwrap().pop();
            }),
            "恰好一个决定",
        ),
        (
            "输入外的目标",
            "w4_01_state_change",
            Box::new(|_, o| {
                o["decisions"][0]["targets"] = json!([99]);
            }),
            "只能是输入记录",
        ),
        (
            "重复目标",
            "w4_01_state_change",
            Box::new(|_, o| {
                o["decisions"][0]["targets"] = json!([17, 17]);
            }),
            "重复",
        ),
        (
            "伪造片段",
            "w4_01_state_change",
            Box::new(|_, o| {
                o["decisions"][0]["basis"][0]["quote"]["text"] = json!("我住在广州。");
            }),
            "不在来源",
        ),
        (
            "输入外的来源",
            "w4_01_state_change",
            Box::new(|_, o| {
                o["decisions"][0]["basis"][0]["source"]["source_id"] = json!("s-999");
            }),
            "输入之外的来源",
        ),
        (
            "current 来源",
            "w4_01_state_change",
            Box::new(|_, o| {
                o["decisions"][0]["basis"][0]["source"] = json!({"kind": "current"});
            }),
            "已保存的来源",
        ),
        (
            "只有新来源的依据就结束旧记录",
            "w4_01_state_change",
            Box::new(|_, o| {
                o["decisions"][0]["basis"].as_array_mut().unwrap().remove(0);
            }),
            "缺少目标 17 的原文依据",
        ),
        (
            "没有新候选原文的依据",
            "w4_01_state_change",
            Box::new(|_, o| {
                o["decisions"][0]["basis"].as_array_mut().unwrap().remove(1);
            }),
            "须包含新候选的原文",
        ),
        (
            "生效时间用保存期限",
            "w4_01_state_change",
            Box::new(|s, o| {
                s["input"]["candidates"][0]["candidate"]["times"][0]["role"] = json!("retention");
                o["decisions"][0]["effective"]["role"] = json!("retention");
            }),
            "保存期限",
        ),
        (
            "生效时间不属于该候选",
            "w4_01_state_change",
            Box::new(|_, o| {
                o["decisions"][0]["effective"]["interp"]["offset"] = json!(-12);
            }),
            "已核验的时间之一",
        ),
        (
            "pending 带动作",
            "w4_16_pending_scope_incomplete",
            Box::new(|_, o| {
                o["decisions"][0]["action"] = json!("new");
            }),
            "pending 的 action 须为 null",
        ),
        (
            "字符串 null",
            "w4_16_pending_scope_incomplete",
            Box::new(|_, o| {
                o["decisions"][0]["action"] = json!("null");
            }),
            "decode",
        ),
        (
            "断言当撤回用",
            "w4_01_state_change",
            Box::new(|_, o| {
                o["decisions"][0]["action"] = json!("retract");
                o["decisions"][0]["effective"] = json!(null);
            }),
            "候选类别与动作不一致",
        ),
        (
            "结束无法核对的旧记录",
            "w4_18_legacy_gaps",
            Box::new(|_, o| {
                o["decisions"][0] = json!({"candidate": 0, "status": "decided", "action": "state_change",
                "targets": [95], "resolves_relations": [],
                "basis": o["decisions"][0]["basis"].clone(), "effective": null,
                "relation_scope": null, "pending_reason": null});
            }),
            "缺少目标 95 的原文依据",
        ),
        (
            "关闭关系却不结束任何一方",
            "w4_15_resolve_conflict",
            Box::new(|s, o| {
                // 把撤回 50 改成强化 51 并关闭 rel-1：两方都还有效。
                s["input"]["candidates"][0]["candidate"]["kind"] = json!("assertion");
                s["input"]["candidates"][0]["candidate"]["value"] =
                    s["input"]["records"][1]["candidate"]["value"].clone();
                o["decisions"][0]["action"] = json!("reinforce");
                o["decisions"][0]["targets"] = json!([51]);
                let own = o["decisions"][0]["basis"][1].clone();
                o["decisions"][0]["basis"] = json!([
                {"source": {"kind": "stored", "source_id": "s-261"},
                 "quote": s["input"]["records"][1]["evidence"][0]["quote"].clone()},
                own]);
            }),
            "不能算已解决",
        ),
        (
            "解决不在输入里的关系",
            "w4_15_resolve_conflict",
            Box::new(|_, o| {
                o["decisions"][0]["resolves_relations"] = json!(["rel-9"]);
            }),
            "只能解决输入中 open 的关系",
        ),
        (
            "new 解决关系",
            "w4_22_replace_conflict_members",
            Box::new(|_, o| {
                o["decisions"][0]["action"] = json!("new");
                o["decisions"][0]["targets"] = json!([]);
                o["decisions"][0]["resolves_relations"] = json!(["rel-2"]);
            }),
            "不能解决关系",
        ),
        (
            "非 conflict 带范围",
            "w4_01_state_change",
            Box::new(|_, o| {
                o["decisions"][0]["relation_scope"] = json!({"conditions": [], "times": []});
            }),
            "只在 conflict 时非空",
        ),
        (
            "conflict 缺范围",
            "w4_07_conflict",
            Box::new(|_, o| {
                o["decisions"][0]["relation_scope"] = json!(null);
            }),
            "只在 conflict 时非空",
        ),
        (
            "范围引用不存在的条件",
            "w4_07_conflict",
            Box::new(|_, o| {
                o["decisions"][0]["relation_scope"] =
                    json!({"conditions": [{"of": "candidate", "index": 0}], "times": []});
            }),
            "不存在的条件",
        ),
        (
            "范围引用非目标记录",
            "w4_07_conflict",
            Box::new(|_, o| {
                o["decisions"][0]["relation_scope"] =
                    json!({"conditions": [{"of": 51, "index": 0}], "times": []});
            }),
            "不存在的条件",
        ),
        (
            "重判以自身为目标",
            "w4_14_rejudge_committed",
            Box::new(|s, o| {
                let mut own = s["input"]["records"][0].clone();
                own["id"] = json!(70);
                s["input"]["records"].as_array_mut().unwrap().push(own);
                o["decisions"][0]["targets"] = json!([70]);
            }),
            "不能以自身为目标",
        ),
    ];
    for (label, id, edit, expected) in cases {
        let error = rejected(id, edit);
        assert!(error.contains(expected), "{label}: {error}");
    }
}

#[test]
fn accepted_quotes_are_restored_to_the_source_wording() {
    // 空白与引号排版的差异可以接受；进入 W5 的是原文写法。
    let sample = sample("w4_01_state_change");
    let mut output = fixture("w4_01_state_change");
    output["decisions"][0]["basis"][1]["quote"]["text"] = json!("我上个月 从北京搬到 上海了。");
    let input: W4Input = serde_json::from_value(sample["input"].clone()).unwrap();
    let output: W4Output = serde_json::from_value(output).unwrap();
    let sources = sample["sources"].clone();
    let text_of = |sid: &str| sources[sid]["text"].as_str().map(str::to_owned);
    let normalized = validate(&input, &output, &text_of).unwrap();
    assert_eq!(
        normalized.decisions[0].basis[1].quote.text,
        "我上个月从北京搬到上海了。"
    );
}

#[test]
fn fixtures_restate_the_expected_decisions_and_nothing_else() {
    // 夹具只是把期望决定里的 basis_sources 换成具体片段：其余字段逐一相同，来源集合相同。
    for sample in samples() {
        if sample["call_w4"] != true {
            continue;
        }
        let id = sample["id"].as_str().unwrap();
        let output = fixture(id);
        let expected = sample["expect"]["decisions"].as_array().unwrap();
        let actual = output["decisions"].as_array().unwrap();
        assert_eq!(expected.len(), actual.len(), "{id}");
        for (expected, actual) in expected.iter().zip(actual) {
            let mut expected = expected.clone();
            let mut actual = actual.clone();
            let listed = expected
                .as_object_mut()
                .unwrap()
                .remove("basis_sources")
                .unwrap();
            let basis = actual.as_object_mut().unwrap().remove("basis").unwrap();
            assert_eq!(expected, actual, "{id}");
            let cited: Vec<&str> = basis
                .as_array()
                .unwrap()
                .iter()
                .map(|e| e["source"]["source_id"].as_str().unwrap())
                .collect();
            assert_eq!(json!(cited), listed, "{id}");
        }
    }
}

#[test]
fn forgetting_a_source_scrubs_the_decision_basis_and_effective_time() {
    // w4_01 提交后删除新原文 s-205：决定回执里它的依据片段、生效时间原文都清掉；
    // 旧原文 s-101 的依据仍在。再删 s-101，它的片段也清掉。
    let run = run("w4_01_state_change");
    let forget = |source: &str| {
        let mut service =
            hippocampus::chat::ChatMemoryService::new(run.db.open(), Default::default()).unwrap();
        service
            .call("memory_forget", json!({"source_id": source}), || false)
            .unwrap()
    };
    let snapshot = || serde_json::to_string(&run.db.state().w5_receipts).unwrap();
    assert!(snapshot().contains("上个月") && snapshot().contains("我住在北京。"));
    forget("s-205");
    let after = snapshot();
    assert!(
        !after.contains("上个月") && !after.contains("搬到上海"),
        "{after}"
    );
    assert!(after.contains("我住在北京。"));
    forget("s-101");
    assert!(!snapshot().contains("我住在北京。"));
}

#[test]
fn w5_rejects_inconsistent_basis_and_effective_fields() {
    // 这些字段由转换生成；W5 仍独立核对，手工构造的不一致组整组拒绝。
    let sample = sample("w4_01_state_change");
    let db = Db::new();
    let own = seed(&db, &sample);
    let input: W4Input = serde_json::from_value(sample["input"].clone()).unwrap();
    let output: W4Output = serde_json::from_value(fixture("w4_01_state_change")).unwrap();
    let state = db.state();
    let good = plan(&runtime(&state, &own, 1, None), &input, &output)
        .unwrap()
        .groups[0]
        .clone();
    let before = serde_json::to_string(&db.state().repository).unwrap();
    let mut cases: Vec<(&str, W5Group, &str)> = Vec::new();
    let mut g = good.clone();
    g.decisions[0].basis_sources = vec!["s-205".into()];
    cases.push(("basis sources differ", g, "different sources"));
    let mut g = good.clone();
    g.decisions[0].effective = Some(DatedValue {
        at: Timestamp(1785513600),
        source: TemporalSource::Explicit,
    });
    cases.push(("month precision written as a point", g, "disagree"));
    let mut g = good.clone();
    g.decisions[0].basis[0].quote.text = "我住在广州。".into();
    cases.push(("basis quote not in the source", g, "not verbatim"));
    let mut g = good.clone();
    g.decisions[0].relation_scope = Some(RelationScope {
        conditions: vec![],
        times: vec![],
    });
    cases.push(("scope on a state change", g, "relation scope"));
    for (label, group, expected) in cases {
        let error = commit_group(&mut db.open(), &claim(&own), &group).unwrap_err();
        assert!(error.to_string().contains(expected), "{label}: {error}");
        assert_eq!(
            serde_json::to_string(&db.state().repository).unwrap(),
            before,
            "{label}"
        );
    }
}

// ---------------- 复审 0026 的回归

fn pending_of(input: &W4Input) -> Vec<Pending> {
    input
        .candidates
        .iter()
        .map(|c| Pending {
            index: c.index,
            committed_id: c.committed_id,
            contract: ContractCandidate {
                candidate: c.candidate.clone(),
                source: c.source.clone(),
                said_on: c.said_on.clone(),
                time_resolutions: c.time_resolutions.clone(),
            },
        })
        .collect()
}

/// 直接写入一条照着 `like` 复制的记录（不同主体，检索取不到），可带任务条件。
fn clone_record(db: &Db, like: u64, subject: &str, task: Option<&str>) -> MemoryId {
    let mut made = None;
    db.open()
        .transaction(|state| {
            let mut memory = state.repository.get(MemoryId(like))?.unwrap();
            memory.id = state.repository.allocate_id()?;
            memory.subject_id.0 = subject.into();
            if let Some(task) = task {
                memory
                    .scope
                    .conditions
                    .push(ScopeCondition::Task(TaskId::from(task)));
            }
            made = Some(memory.id);
            state.repository.insert(memory)?;
            Ok(())
        })
        .unwrap();
    made.unwrap()
}

fn open_relation(db: &Db, id: &str, members: &[MemoryId]) {
    db.open()
        .transaction(|state| {
            state.relations.insert(
                id.into(),
                StoredRelation {
                    id: id.into(),
                    kind: hippocampus::RelationKind::Conflict,
                    members: members.to_vec(),
                    conditions: json!([]),
                    times: json!([]),
                    basis: vec![],
                    status: hippocampus::RelationStatus::Open,
                    resolution: None,
                    created_by: "seed".into(),
                    closed_by: None,
                },
            );
            Ok(())
        })
        .unwrap();
}

fn ids(input: &W4Input) -> (Vec<u64>, Vec<String>) {
    let mut records: Vec<u64> = input.records.iter().map(|r| r.id().0).collect();
    records.sort_unstable();
    let mut relations: Vec<String> = input.relations.iter().map(|r| r.id.clone()).collect();
    relations.sort();
    (records, relations)
}

/// 重新认领一条已整理完的原文（重判）。
fn reclaim(db: &Db, sid: &str) {
    db.open()
        .transaction(|state| {
            state.source_less_barrier_sequence = 0;
            let revision = state.revision;
            let task = state.sources.get_mut(sid).unwrap();
            task.enrichment = "running".into();
            task.error = None;
            task.task.attempt_id = Some(ATTEMPT.into());
            task.task.lease_until = Some(now() + 900);
            task.task.claim_revision = Some(revision);
            Ok(())
        })
        .unwrap();
}

fn rejudged_new(input: &W4Input) -> W4Output {
    W4Output {
        decisions: input
            .candidates
            .iter()
            .map(|candidate| W4Decision {
                candidate: candidate.index,
                status: DecisionStatus::Decided,
                action: Some(Action::New),
                targets: vec![],
                resolves_relations: vec![],
                basis: vec![EvidenceRef {
                    source: candidate.source.clone(),
                    quote: candidate.candidate.sentence.clone(),
                }],
                effective: None,
                relation_scope: None,
                pending_reason: None,
            })
            .collect(),
    }
}

#[test]
fn a_rejudge_cannot_clear_a_relation_withheld_by_task_scope() {
    for status in [
        hippocampus::RelationStatus::Open,
        hippocampus::RelationStatus::Resolved,
        hippocampus::RelationStatus::Invalidated,
    ] {
        let sample = sample("w4_01_state_change");
        let db = Db::new();
        let own = seed(&db, &sample);
        let other = clone_record(&db, 17, "someone-else", Some("other-task"));
        open_relation(&db, "rel-7", &[MemoryId(17), other]);
        reclaim(&db, &own);
        let input: W4Input = serde_json::from_value(sample["input"].clone()).unwrap();
        let state = db.state();
        let rt = runtime(&state, &own, 1, None);
        let assembly = assemble(&db, &rt, &pending_of(&input), BUDGET).unwrap();
        let output = serde_json::from_value(fixture("w4_01_state_change")).unwrap();
        let first = plan(&rt, &assembly.input, &output).unwrap();
        let product = match commit_group(&mut db.open(), &claim(&own), &first.groups[0]).unwrap() {
            W5Outcome::Committed(receipt) => receipt.products[&0],
            other => panic!("{other:?}"),
        };
        assert!(
            db.state().record_updates[&product]
                .reasons
                .contains(&UpdateReason::UpdateRelationUnclear)
        );
        db.open()
            .transaction(|state| {
                let relation = state.relations.get_mut("rel-7").unwrap();
                relation.status = status;
                if status == hippocampus::RelationStatus::Resolved {
                    relation.resolution = Some(RelationEvidence {
                        source_id: own.clone(),
                        quote: input.candidates[0].candidate.sentence.text.clone(),
                    });
                }
                Ok(())
            })
            .unwrap();
        reclaim(&db, &own);
        let mut input = input;
        input.candidates[0].committed_id = Some(product);
        let state = db.state();
        let rt = runtime(&state, &own, 2, None);
        let assembly = assemble(&db, &rt, &pending_of(&input), BUDGET).unwrap();
        assert_eq!(ids(&assembly.input), (vec![17], vec![]));
        assert!(!assembly.input.coverage.complete);
        let plan = plan(&rt, &assembly.input, &rejudged_new(&assembly.input)).unwrap();
        match commit_group(&mut db.open(), &claim(&own), &plan.groups[0]).unwrap() {
            W5Outcome::Committed(_) => {}
            other => panic!("{other:?}"),
        }
        assert_eq!(
            db.state().record_updates[&product],
            UpdateState::from_reasons(vec![
                UpdateReason::UpdateScopeIncomplete,
                UpdateReason::UpdateRelationUnclear,
            ]),
            "withheld relation: {status:?}"
        );
        assert!(plan.groups[0].seen_relations.is_empty());
        assert_eq!(memory(&db.state(), other).status, MemoryStatus::Active);
    }
}

fn replacement_with_a_forgotten_predecessor() -> Run {
    let run = run("w4_22_replace_conflict_members");
    let mut service =
        hippocampus::chat::ChatMemoryService::new(run.db.open(), Default::default()).unwrap();
    service
        .call("memory_forget", json!({"source_id": "s-501"}), || false)
        .unwrap();
    reclaim(&run.db, "s-510");
    run
}

#[test]
fn a_rejudge_after_forgetting_a_predecessor_can_clear_the_relation_gap() {
    let run = replacement_with_a_forgotten_predecessor();
    let product = run.product(0).unwrap();
    assert_eq!(
        memory(&run.db.state(), MemoryId(110)).status,
        MemoryStatus::Deleted
    );
    assert!(
        run.db.state().record_updates[&product]
            .reasons
            .contains(&UpdateReason::UpdateRelationUnclear),
        "forgetting alone must not clear the gap"
    );
    let mut input: W4Input = serde_json::from_value(run.sample["input"].clone()).unwrap();
    input.candidates[0].committed_id = Some(product);
    let state = run.db.state();
    let rt = runtime(&state, "s-510", 2, None);
    let assembly = assemble(&run.db, &rt, &pending_of(&input), BUDGET).unwrap();
    assert!(assembly.call_w4);
    assert_eq!(ids(&assembly.input), (vec![111], vec!["rel-2".into()]));
    assert_eq!(
        assembly.input.relations[0].status,
        hippocampus::contract::RelationStatus::Invalidated
    );
    let plan = plan(&rt, &assembly.input, &rejudged_new(&assembly.input)).unwrap();
    assert_eq!(
        plan.groups[0]
            .seen
            .iter()
            .map(|s| s.id.0)
            .collect::<Vec<_>>(),
        [111, 112]
    );
    let receipt = match commit_group(&mut run.db.open(), &claim("s-510"), &plan.groups[0]).unwrap()
    {
        W5Outcome::Committed(receipt) => receipt,
        other => panic!("{other:?}"),
    };
    assert_eq!(receipt.products[&0], product);
    assert_eq!(
        run.db.state().record_updates[&product],
        UpdateState::from_reasons(vec![])
    );
    assert_eq!(
        memory(&run.db.state(), MemoryId(110)).status,
        MemoryStatus::Deleted
    );
}

#[test]
fn a_deleted_relation_member_does_not_consume_the_input_budget() {
    let run = replacement_with_a_forgotten_predecessor();
    let mut input: W4Input = serde_json::from_value(run.sample["input"].clone()).unwrap();
    input.candidates[0].committed_id = run.product(0);
    let state = run.db.state();
    let rt = runtime(&state, "s-510", 2, None);
    let assembly = assemble(
        &run.db,
        &rt,
        &pending_of(&input),
        W4Budget {
            facts: 8,
            records: 1,
        },
    )
    .unwrap();
    assert!(
        assembly.call_w4,
        "only the surviving predecessor consumes the budget"
    );
    assert_eq!(ids(&assembly.input), (vec![111], vec!["rel-2".into()]));
    assert_eq!(
        assembly.input.coverage,
        W4Coverage {
            complete: true,
            omitted: 0
        }
    );
    let plan = plan(&rt, &assembly.input, &rejudged_new(&assembly.input)).unwrap();
    match commit_group(&mut run.db.open(), &claim("s-510"), &plan.groups[0]).unwrap() {
        W5Outcome::Committed(receipt) => {
            assert_eq!(receipt.updates[&0], UpdateState::from_reasons(vec![]));
        }
        other => panic!("{other:?}"),
    }
    // 真正放不下唯一的可送入记录时，仍整批待定；删除成员不计入 omitted。
    let zero = assemble(
        &run.db,
        &rt,
        &pending_of(&input),
        W4Budget {
            facts: 8,
            records: 0,
        },
    )
    .unwrap();
    assert!(!zero.call_w4);
    assert_eq!(
        zero.input.coverage,
        W4Coverage {
            complete: false,
            omitted: 1
        }
    );
    assert_eq!(
        zero.skipped.decisions[0].pending_reason,
        Some(PendingReason::ScopeIncomplete)
    );
}

#[test]
fn a_held_candidate_holds_its_whole_dependency_group() {
    // w4_21 变体：候选 0（修订 51）极性未知，仍不可投影；候选 1 撤回 50 并解决
    // rel-1。两者经 rel-1 相互依赖，不能只执行后一半。
    let mut sample = sample("w4_21_confirm_conflict_side");
    sample["input"]["candidates"][0]["candidate"]["polarity"] = json!("unknown");
    let db = Db::new();
    let own = seed(&db, &sample);
    let input: W4Input = serde_json::from_value(sample["input"].clone()).unwrap();
    let output: W4Output = serde_json::from_value(fixture("w4_21_confirm_conflict_side")).unwrap();
    let state = db.state();
    let plan = plan(&runtime(&state, &own, 1, None), &input, &output).unwrap();
    assert!(plan.groups.is_empty(), "{:?}", plan.groups);
    assert_eq!(
        plan.held,
        BTreeMap::from([
            (
                0,
                Held::Unprojectable(Unprojectable::Polarity(
                    hippocampus::contract::Polarity::Unknown
                ))
            ),
            (1, Held::DependsOnHeld { candidate: 0 }),
        ])
    );
    // 独立的组不受影响：同批再加一个无关的新候选，照常成组。
    let mut input = input;
    let mut output = output;
    let mut extra = input.candidates[0].clone();
    extra.index = 2;
    extra.candidate.polarity = hippocampus::contract::Polarity::Affirm;
    input.candidates.push(extra.clone());
    output.decisions.push(W4Decision {
        candidate: 2,
        status: DecisionStatus::Decided,
        action: Some(Action::New),
        targets: vec![],
        resolves_relations: vec![],
        basis: vec![EvidenceRef {
            source: extra.source.clone(),
            quote: extra.candidate.sentence.clone(),
        }],
        effective: None,
        relation_scope: None,
        pending_reason: None,
    });
    let plan = hippocampus::w4::plan(&runtime(&state, &own, 1, None), &input, &output).unwrap();
    assert_eq!(plan.groups.len(), 1);
    assert_eq!(plan.groups[0].candidates[0].index, 2);
    assert_eq!(plan.held.len(), 2);
}

#[test]
fn a_relation_reaching_into_another_task_is_withheld_and_reported() {
    // w4_19：记录 97 在本任务内可见，98 属于另一个任务。两者之间有 open 关系时，本任务的
    // 输入不能带入 98，也不能送入一条截掉成员的关系；范围如实报告为不完整。
    let sample = sample("w4_19_task_scope");
    let db = Db::new();
    let own = seed(&db, &sample);
    open_relation(&db, "rel-7", &[MemoryId(97), MemoryId(98)]);
    let input: W4Input = serde_json::from_value(sample["input"].clone()).unwrap();
    let state = db.state();
    let rt = runtime(&state, &own, 1, task_of("w4_19_task_scope"));
    let assembly = assemble(&db, &rt, &pending_of(&input), BUDGET).unwrap();
    assert_eq!(ids(&assembly.input), (vec![97], vec![]));
    assert_eq!(
        assembly.input.coverage,
        W4Coverage {
            complete: false,
            omitted: 1
        }
    );
    // 在 98 所属的任务里，同一条关系完整可见。
    let rt = runtime(&state, &own, 1, Some(TaskId::from("s6-plateau-demo")));
    let other = assemble(&db, &rt, &pending_of(&input), BUDGET).unwrap();
    assert!(other.input.records.iter().all(|r| r.id().0 != 97));
}

#[test]
fn relation_expansion_runs_to_a_fixpoint_whatever_the_relation_ids() {
    // 初选只命中 17；17—A 与 A—B 两条关系无论编号先后，A、B 和两条关系都进入输入。
    for (near, far) in [("rel-3", "rel-1"), ("rel-3", "rel-9")] {
        let sample = sample("w4_01_state_change");
        let db = Db::new();
        let own = seed(&db, &sample);
        let a = clone_record(&db, 17, "someone-else", None);
        let b = clone_record(&db, 17, "someone-else", None);
        open_relation(&db, near, &[MemoryId(17), a]);
        open_relation(&db, far, &[a, b]);
        let input: W4Input = serde_json::from_value(sample["input"].clone()).unwrap();
        let state = db.state();
        let assembly = assemble(
            &db,
            &runtime(&state, &own, 1, None),
            &pending_of(&input),
            BUDGET,
        )
        .unwrap();
        let mut relations = vec![near.to_owned(), far.to_owned()];
        relations.sort();
        assert_eq!(ids(&assembly.input), (vec![17, a.0, b.0], relations));
    }
}

#[test]
fn required_records_over_budget_make_the_whole_batch_pending() {
    // w4_12：两条专用关系记录都必须送入。预算放不下时不送入被截断的输入、不调用 W4，
    // 候选待定（scope_incomplete），旧记录不动。
    let sample = sample("w4_12_multi_target");
    let db = Db::new();
    let own = seed(&db, &sample);
    let input: W4Input = serde_json::from_value(sample["input"].clone()).unwrap();
    let state = db.state();
    let rt = runtime(&state, &own, 1, None);
    let untruncated = assemble(
        &db,
        &rt,
        &pending_of(&input),
        W4Budget {
            facts: 0,
            records: 64,
        },
    )
    .unwrap();
    assert_eq!(ids(&untruncated.input).0, [62, 63]);
    assert_eq!(untruncated.input.coverage.omitted, 0);
    let tight = W4Budget {
        facts: 8,
        records: 1,
    };
    let assembly = assemble(&db, &rt, &pending_of(&input), tight).unwrap();
    assert!(!assembly.call_w4);
    assert_eq!(ids(&assembly.input), (vec![], vec![]));
    assert_eq!(
        assembly.input.coverage,
        W4Coverage {
            complete: false,
            omitted: 2
        }
    );
    let decision = &assembly.skipped.decisions[0];
    assert_eq!(decision.status, DecisionStatus::Pending);
    assert_eq!(
        decision.pending_reason,
        Some(PendingReason::ScopeIncomplete)
    );
    let plan = plan(&rt, &assembly.input, &assembly.skipped).unwrap();
    let receipt = match commit_group(&mut db.open(), &claim(&own), &plan.groups[0]).unwrap() {
        W5Outcome::Committed(receipt) => receipt,
        other => panic!("{other:?}"),
    };
    let after = db.state();
    assert_eq!(
        receipt.updates[&0].reasons,
        vec![UpdateReason::UpdateScopeIncomplete]
    );
    for id in [62, 63] {
        assert_eq!(memory(&after, MemoryId(id)).status, MemoryStatus::Active);
    }
    // 通用 fact 的检索命中不是必须送入的：放不下时计入 omitted，候选照常新增。
    let sample = self::sample("w4_07_conflict");
    let db = Db::new();
    let own = seed(&db, &sample);
    let input: W4Input = serde_json::from_value(sample["input"].clone()).unwrap();
    let state = db.state();
    let none = W4Budget {
        facts: 8,
        records: 0,
    };
    let assembly = assemble(
        &db,
        &runtime(&state, &own, 1, None),
        &pending_of(&input),
        none,
    )
    .unwrap();
    assert!(!assembly.call_w4);
    assert_eq!(
        assembly.input.coverage,
        W4Coverage {
            complete: false,
            omitted: 1
        }
    );
    assert_eq!(assembly.skipped.decisions[0].action, Some(Action::New));
}

#[test]
fn a_rejudge_sees_its_predecessors_and_a_later_source_can_resolve_their_relation() {
    // w4_22 提交后：产物 112 替代了 110、111，rel-2 仍 open，112 的更新状态含
    // update_relation_unclear。
    let run = run("w4_22_replace_conflict_members");
    let product = run.product(0).unwrap();
    assert_eq!(product, MemoryId(112));
    assert!(
        run.after.record_updates[&product]
            .reasons
            .contains(&UpdateReason::UpdateRelationUnclear)
    );
    let db = &run.db;
    let mut input: W4Input = serde_json::from_value(run.sample["input"].clone()).unwrap();
    let own = "s-510";

    let quote = |source: &str, text: &str| EvidenceRef {
        source: SourceRef::Stored {
            source_id: source.into(),
        },
        quote: Quote { text: text.into() },
    };

    // 重判 112：输入包含前身 110、111 和 rel-2 的最新状态。给出已定决定后
    // update_relation_unclear 被清除（不要求 rel-2 被关闭）。
    reclaim(db, own);
    let mut rejudge = input.clone();
    rejudge.candidates[0].committed_id = Some(product);
    {
        let state = db.state();
        let rt = runtime(&state, own, 2, None);
        let assembly = assemble(&db, &rt, &pending_of(&rejudge), BUDGET).unwrap();
        assert!(assembly.call_w4);
        assert_eq!(
            ids(&assembly.input),
            (vec![110, 111], vec!["rel-2".to_owned()])
        );
        let output = W4Output {
            decisions: vec![W4Decision {
                candidate: 0,
                status: DecisionStatus::Decided,
                action: Some(Action::New),
                targets: vec![],
                resolves_relations: vec![],
                basis: vec![quote("s-510", "我已经搬到深圳了。")],
                effective: None,
                relation_scope: None,
                pending_reason: None,
            }],
        };
        let plan = plan(&rt, &assembly.input, &output).unwrap();
        let seen: Vec<u64> = plan.groups[0].seen.iter().map(|s| s.id.0).collect();
        assert_eq!(seen, [110, 111, 112]);
        assert!(
            plan.groups[0]
                .seen_relations
                .iter()
                .any(|relation| relation.id == "rel-2")
        );
        match commit_group(&mut db.open(), &claim(own), &plan.groups[0]).unwrap() {
            W5Outcome::Committed(_) => {}
            other => panic!("{other:?}"),
        }
        w5_finish_task(&mut db.open(), &claim(own), &W5TaskEnd::Committed).unwrap();
    }
    let after = db.state();
    assert_eq!(after.record_updates[&product].status, UpdateStatus::Judged);
    assert_eq!(
        after.relations["rel-2"].status,
        hippocampus::RelationStatus::Open
    );

    // 后续原文：重申住在深圳。输入带出 112、它的前身和前身所在的 open 关系；110、111
    // 已经结束，W4 可以强化 112 并显式解决 rel-2。
    claim_source(db, "s-520", "我确实住在深圳。");
    reclaim(db, "s-520");
    input.candidates[0].candidate.sentence.text = "我确实住在深圳。".into();
    input.candidates[0].source = SourceRef::Stored {
        source_id: "s-520".into(),
    };
    {
        let state = db.state();
        let rt = runtime(&state, "s-520", 1, None);
        let assembly = assemble(&db, &rt, &pending_of(&input), BUDGET).unwrap();
        assert_eq!(
            ids(&assembly.input),
            (vec![110, 111, 112], vec!["rel-2".to_owned()])
        );
        let output = W4Output {
            decisions: vec![W4Decision {
                candidate: 0,
                status: DecisionStatus::Decided,
                action: Some(Action::Reinforce),
                targets: vec![product],
                resolves_relations: vec!["rel-2".into()],
                basis: vec![
                    quote("s-510", "我已经搬到深圳了。"),
                    quote("s-520", "我确实住在深圳。"),
                ],
                effective: None,
                relation_scope: None,
                pending_reason: None,
            }],
        };
        let plan = plan(&rt, &assembly.input, &output).unwrap();
        match commit_group(&mut db.open(), &claim("s-520"), &plan.groups[0]).unwrap() {
            W5Outcome::Committed(_) => {}
            other => panic!("{other:?}"),
        }
        w5_finish_task(&mut db.open(), &claim("s-520"), &W5TaskEnd::Committed).unwrap();
    }
    let after = db.state();
    assert_eq!(
        after.relations["rel-2"].status,
        hippocampus::RelationStatus::Resolved
    );
    for id in [110, 111] {
        assert_eq!(
            memory(&after, MemoryId(id)).status,
            MemoryStatus::Superseded
        );
    }
    // 之后再重判 112 时，输入给出的是 rel-2 的最新状态（已解决），不是漏掉它。
    let rt = runtime(&after, own, 3, None);
    let assembly = assemble(&db, &rt, &pending_of(&rejudge), BUDGET).unwrap();
    assert_eq!(
        ids(&assembly.input),
        (vec![110, 111], vec!["rel-2".to_owned()])
    );
    assert_eq!(
        assembly.input.relations[0].status,
        hippocampus::contract::RelationStatus::Resolved
    );
}

fn with_time(id: &str, interp: Json, resolution: Json) -> Run {
    let mut sample = sample(id);
    let time = json!({"role": "event", "quote": {"text": "上个月"}, "interp": interp, "anchor": "said_on"});
    sample["input"]["candidates"][0]["candidate"]["times"] = json!([time]);
    sample["input"]["candidates"][0]["time_resolutions"] = json!([resolution]);
    let mut output = fixture(id);
    output["decisions"][0]["effective"] = time;
    run_with(sample, Some(output))
}

#[test]
fn a_retraction_may_carry_an_end_time_and_other_actions_only_record_it() {
    let day = 86_400;
    let start = 1_785_513_600;
    // 撤回带生效时间：是目标的结束时间（契约 2.4 不按动作限制 effective）。
    let run = with_time(
        "w4_05_retract",
        json!({"kind": "relative", "unit": "day", "offset": -1}),
        json!({"status": "resolved", "start": start, "end_exclusive": start + day, "precision": "day"}),
    );
    let old = memory(&run.after, MemoryId(40));
    assert_eq!(old.status, MemoryStatus::Superseded);
    assert_eq!(old.supersede_reason, Some(SupersedeReason::UserRetracted));
    assert_eq!(old.temporal.valid_to.map(|v| v.at), Some(Timestamp(start)));
    match record_view(&run.after, MemoryId(40)) {
        Some(RecordView2::Current(view)) => {
            assert_eq!(view.ended, Ended::Known);
            assert_eq!(view.end.unwrap().precision, Precision::Day);
        }
        other => panic!("{other:?}"),
    }
    // 强化不结束任何记录：生效时间只记在决定里，目标没有结束时间。
    let run = with_time(
        "w4_10_reinforce",
        json!({"kind": "relative", "unit": "month", "offset": -1}),
        json!({"status": "resolved", "start": start, "end_exclusive": start + 31 * day, "precision": "month"}),
    );
    let stored = &run.after.w5_receipts[&run.receipts[0].group_id];
    assert!(stored.decisions[0].effective_time.is_some());
    let target = memory(&run.after, MemoryId(31));
    assert_eq!(target.status, MemoryStatus::Active);
    assert_eq!(target.temporal.valid_to, None);
    match record_view(&run.after, MemoryId(31)) {
        Some(RecordView2::Current(view)) => {
            assert_eq!((view.ended, view.end), (Ended::NotEnded, None))
        }
        other => panic!("{other:?}"),
    }
    let mut service =
        hippocampus::chat::ChatMemoryService::new(run.db.open(), Default::default()).unwrap();
    let listed = service
        .call("memory_list", json!({"include_history": true}), || false)
        .unwrap();
    let item = listed["memories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == 31)
        .unwrap();
    assert!(item["qualifiers_unmapped"]["end"].is_null(), "{item}");
}

#[test]
fn authorized_fact_scope_is_complete_only_after_actual_enumeration() {
    let db = Db::new();
    let first = seed_fact(&db, "first", "coffee tea", "coffee tea");
    let second = seed_fact(&db, "second", "a red umbrella", "a red umbrella");
    let outside = clone_record(&db, first.0, "self", Some("other-task"));
    claim_source(&db, "query", "coffee tea");
    let pending = fact_pending("query", "coffee tea");
    let before = serde_json::to_value(db.state()).unwrap();
    let (_, full) = hippocampus::w4::assemble(&db.open(), "query", None, &pending, BUDGET).unwrap();
    assert_eq!(ids(&full.input).0, [first.0, second.0]);
    assert!(!ids(&full.input).0.contains(&outside.0));
    assert_eq!(
        full.input.coverage,
        W4Coverage {
            complete: true,
            omitted: 0
        }
    );
    assert_eq!(full.fact_retrieval[&0], FactRetrieval::Enumerated);
    assert_eq!(
        serde_json::to_value(db.state()).unwrap(),
        before,
        "existing pending is not rewritten"
    );
    assert_eq!(
        db.state().record_updates[&first].status,
        UpdateStatus::Pending
    );
    let (_, tight) = hippocampus::w4::assemble(
        &db.open(),
        "query",
        None,
        &pending,
        W4Budget {
            facts: 1,
            records: 64,
        },
    )
    .unwrap();
    assert_eq!(ids(&tight.input).0, [first.0]);
    assert_eq!(
        tight.input.coverage,
        W4Coverage {
            complete: false,
            omitted: 1
        },
        "one FTS hit without truncation does not cover the other authorized fact"
    );
    let (state, full) =
        hippocampus::w4::assemble(&db.open(), "query", None, &pending, BUDGET).unwrap();
    let p = plan(
        &runtime(&state, "query", 1, None),
        &full.input,
        &rejudged_new(&full.input),
    )
    .unwrap();
    let receipt = match commit_group(&mut db.open(), &claim("query"), &p.groups[0]).unwrap() {
        W5Outcome::Committed(r) => r,
        other => panic!("{other:?}"),
    };
    assert_eq!(receipt.updates[&0].status, UpdateStatus::Judged);
    assert_eq!(
        db.state().record_updates[&first].status,
        UpdateStatus::Pending,
        "committing a new complete decision does not rejudge an old pending record"
    );
}

#[test]
fn enumerated_fact_scope_includes_history_and_transitive_relations() {
    let previous = run("w4_14_rejudge_committed");
    let db = &previous.db;
    claim_source(db, "next", "an unrelated new fact");
    let (_, full) = hippocampus::w4::assemble(
        &db.open(),
        "next",
        None,
        &fact_pending("next", "an unrelated new fact"),
        BUDGET,
    )
    .unwrap();
    assert!(full.input.coverage.complete);
    assert_eq!(
        ids(&full.input).0,
        [55, 70],
        "active fact and its superseded predecessor"
    );
    // An open relation reached through another relation is included in full.
    let first = seed_fact(&db, "closure-a", "orange juice", "orange juice");
    let second = seed_fact(&db, "closure-b", "purple scarf", "purple scarf");
    let third = seed_fact(&db, "closure-c", "blue coat", "blue coat");
    open_relation(&db, "closure-1", &[first, second]);
    open_relation(&db, "closure-2", &[second, third]);
    claim_source(&db, "closure-query", "orange juice");
    let (_, full) = hippocampus::w4::assemble(
        &db.open(),
        "closure-query",
        None,
        &fact_pending("closure-query", "orange juice"),
        BUDGET,
    )
    .unwrap();
    assert!(full.input.coverage.complete);
    assert!(full.input.relations.iter().any(|r| r.id == "closure-2"));
    let (_, tight) = hippocampus::w4::assemble(
        &db.open(),
        "closure-query",
        None,
        &fact_pending("closure-query", "orange juice"),
        W4Budget {
            facts: 8,
            records: 1,
        },
    )
    .unwrap();
    assert!(!tight.input.coverage.complete);
}

fn af276_recall(run: &Run) -> hippocampus::recall::Assembly {
    af276_recall_with_budget(run, Default::default())
}

fn af276_recall_with_budget(
    run: &Run,
    budget: hippocampus::recall::Budget,
) -> hippocampus::recall::Assembly {
    let query = "请列出已保存的内容";
    let request = hippocampus::recall::Request {
        binding: RecallRequest {
            request_id: "af276-recall".into(),
            query: query.into(),
            context: vec![],
            task_id: None,
            at: 1_791_648_000,
        },
        owner: OWNER.into(),
        subject_ids: Some(vec!["self".into()]),
        said_on: "2026-10-11".into(),
        utc_offset_seconds: 28_800,
        bootstrap: false,
    };
    let plan = hippocampus::r1::Plan {
        query: query.into(),
        context: vec![],
        subject_ids: request.subject_ids.clone(),
        task_id: None,
        output: Some(
            serde_json::from_value(
                json!({"need":"judge","current_evidence":"query_only","type":"aggregate",
            "subject":{"entity_id":"self"},"relation":"","conditions":[],"time":null,
            "plan":{"op":"list","filter":{"relation":null,"time":null},"unit":null}}),
            )
            .unwrap(),
        ),
        time_resolution: None,
        filter_time_resolution: None,
    };
    hippocampus::recall::assemble(&run.db.open(), &request, &plan, budget).unwrap()
}

#[test]
fn af276_complete_controls_clear_only_the_covered_scope_gap() {
    for id in ["w4_08_complete_no_records", "w4_14_complete_rejudge"] {
        let run = run(id);
        check_after(&run);
        assert_eq!(run.update_of(0).unwrap().status, UpdateStatus::Judged);
        let recalled = af276_recall(&run);
        let current = run.product(0).unwrap();
        let row = recalled
            .evidence
            .records
            .iter()
            .find(|r| r.id() == current)
            .unwrap();
        let RecordView::Current(row) = row else {
            panic!("current assertion missing")
        };
        assert_eq!(row.update.status, UpdateStatus::Judged);
    }
}

#[test]
fn af276_pending_target_branches_leave_existing_records_judged() {
    for id in ["w4_17_pending_unclear_relation", "w4_18_legacy_gaps"] {
        let s = sample(id);
        for expect in s["expect"]["alternatives"].as_array().unwrap() {
            let mut branch = s.clone();
            branch["expect"] = expect.clone();
            let mut output = fixture(id);
            output["decisions"][0]["targets"] = expect["decisions"][0]["targets"].clone();
            let run = run_with(branch, Some(output));
            check_after(&run);
            if id == "w4_17_pending_unclear_relation" {
                let assembly = af276_recall(&run);
                let RecordView::Current(old) = assembly
                    .evidence
                    .records
                    .iter()
                    .find(|r| r.id() == MemoryId(90))
                    .unwrap()
                else {
                    panic!("old record missing")
                };
                assert_eq!(old.update.status, UpdateStatus::Judged);
                let RecordView::Current(new) = assembly
                    .evidence
                    .records
                    .iter()
                    .find(|r| Some(r.id()) == run.product(0))
                    .unwrap()
                else {
                    panic!("pending record missing")
                };
                assert_eq!(new.update.status, UpdateStatus::Pending);
            }
        }
    }
}

#[test]
fn af276_negative_changes_and_corrections_keep_polarity_and_history_reason() {
    for (id, reason) in [
        (
            "w4_n1a_negative_state_change",
            SupersedeReason::StateChanged,
        ),
        ("w4_n1b_negative_correction", SupersedeReason::Correction),
    ] {
        let run = run(id);
        check_after(&run);
        let current = run.product(0).unwrap();
        assert_eq!(
            memory(&run.after, current).polarity,
            Some(hippocampus::Polarity::Negative)
        );
        let old = run.sample["input"]["records"][0]["id"].as_u64().unwrap();
        assert_eq!(
            memory(&run.after, MemoryId(old)).supersede_reason,
            Some(reason)
        );
        let recalled = af276_recall(&run);
        let row = recalled
            .evidence
            .records
            .iter()
            .find(|r| r.id() == current)
            .unwrap();
        let RecordView::Current(row) = row else {
            panic!("negative fact missing")
        };
        assert_eq!(
            row.candidate.polarity,
            hippocampus::contract::Polarity::Negate
        );
        assert_eq!(row.history, vec![MemoryId(old)]);
        // A mechanical R3 record retains the reason; R4 must not turn a corrected claim into history.
        assert_eq!(
            w5_record_contract(&run.after, current).unwrap().candidate,
            row.candidate
        );
    }
}

#[test]
fn af276_sibling_records_never_borrow_grade_or_update_and_replay_is_idempotent() {
    let run = run("w4_n2_sibling_fragments");
    check_after(&run);
    let a = run.product(0).unwrap();
    let b = run.product(1).unwrap();
    assert_ne!(a, b);
    let ca = w5_record_contract(&run.after, a).unwrap();
    let cb = w5_record_contract(&run.after, b).unwrap();
    assert_eq!(ca.candidate.sentence, cb.candidate.sentence);
    assert_ne!(ca.candidate.value, cb.candidate.value);
    run.db
        .open()
        .transaction(|state| {
            state.record_updates.insert(
                a,
                UpdateState::from_reasons(vec![UpdateReason::UpdateScopeIncomplete]),
            );
            Ok(())
        })
        .unwrap();
    let recalled = af276_recall(&run);
    let arow = recalled
        .evidence
        .records
        .iter()
        .find(|r| r.id() == a)
        .unwrap();
    let brow = recalled
        .evidence
        .records
        .iter()
        .find(|r| r.id() == b)
        .unwrap();
    let (RecordView::Current(arow), RecordView::Current(brow)) = (arow, brow) else {
        panic!("siblings missing")
    };
    assert_eq!(arow.update.status, UpdateStatus::Pending);
    assert_eq!(brow.update.status, UpdateStatus::Judged);
    assert_eq!(arow.grade, Grade::Committed);
    assert_eq!(brow.grade, Grade::Committed);
    assert_eq!(arow.candidate.value, ca.candidate.value);
    assert_eq!(brow.candidate.value, cb.candidate.value);
    let fact = |contract: &ContractCandidate, ids: Vec<MemoryId>| FactView {
        statement: match contract.candidate.value.as_ref().unwrap() {
            Value::Text { text, .. } => text.clone(),
            _ => panic!("expected fact fragment"),
        },
        evidence: vec![EvidenceRef {
            source: contract.source.clone(),
            quote: contract.candidate.sentence.clone(),
        }],
        record_ids: ids,
        polarity: contract.candidate.polarity,
        strength: None,
        uncertainty: None,
        conditions: vec![],
        times: vec![],
        current: Qualification::Unknown,
        applies: Qualification::Yes,
        supersedes_record_ids: vec![],
    };
    let output = R4Output {
        verdict: R4Verdict::Supported,
        facts: vec![fact(&ca, vec![a]), fact(&cb, vec![b])],
        compute: None,
    };
    let result = recalled
        .finish_r4(&run.db.open(), Ok(&lower(output).to_string()))
        .unwrap();
    assert_eq!(result.facts.len(), 2);
    assert_eq!(result.facts[0].grade, RecallGrade::Committed);
    assert_eq!(
        result.facts[0].update.as_ref().unwrap().status,
        UpdateStatus::Pending
    );
    assert_eq!(result.facts[1].grade, RecallGrade::Committed);
    assert_eq!(
        result.facts[1].update.as_ref().unwrap().status,
        UpdateStatus::Judged
    );
    let output = R4Output {
        verdict: R4Verdict::Supported,
        facts: vec![fact(&cb, vec![])],
        compute: None,
    };
    let result = af276_recall(&run)
        .finish_r4(&run.db.open(), Ok(&lower(output).to_string()))
        .unwrap();
    assert_eq!(result.facts.len(), 1);
    assert_eq!(result.facts[0].grade, RecallGrade::Uncommitted);
    assert_eq!(
        result.facts[0].update, None,
        "shared sentence cannot provide record identity"
    );
    run.db
        .open()
        .transaction(|state| {
            state.sources.get_mut("s-611").unwrap().enrichment = "pending".into();
            Ok(())
        })
        .unwrap();
    let bounded = af276_recall_with_budget(
        &run,
        hippocampus::recall::Budget {
            records: 1,
            ..Default::default()
        },
    );
    assert!(bounded.evidence.completeness.retrieval_truncated);
    assert!(!bounded.evidence.completeness.plan_complete);
    assert_eq!(
        bounded
            .evidence
            .raw
            .iter()
            .find(|r| r.source_id == "s-611")
            .unwrap()
            .enrichment,
        EnrichmentStatus::Pending
    );
    let db = Db::new();
    let own = seed(&db, &run.sample);
    let before = db.state();
    let input: W4Input = serde_json::from_value(run.sample["input"].clone()).unwrap();
    let output: W4Output = serde_json::from_value(fixture("w4_n2_sibling_fragments")).unwrap();
    let rt = runtime(&before, &own, 1, None);
    let original = plan(&rt, &input, &output).unwrap();
    for group in &original.groups {
        let first = commit_group(&mut db.open(), &claim(&own), group).unwrap();
        let W5Outcome::Committed(first) = first else {
            panic!("first commit failed")
        };
        let committed = serde_json::to_value(db.state()).unwrap();
        let replay = commit_group(&mut db.open(), &claim(&own), group).unwrap();
        let W5Outcome::AlreadyApplied(replay) = replay else {
            panic!("replay changed outcome")
        };
        assert_eq!(first, replay);
        assert_eq!(serde_json::to_value(db.state()).unwrap(), committed);
    }
}
