# Z18：R4 旧方依据机械补入

## 实现与口径

生产 `Assembly::finish_r4`、提供者及离线评测共用 `r4::assemble_old_basis`。仅复制模型已选包内 supersedes 目标的已核准 stored 引用，并独立核对未删除来源正文中的逐字 Quote。目标不在包内、来源丢失/已删、原文不含 Quote 时不补；已有有效短片段/空白等价依据不重复。无效原引用、record_ids 依据缺失或缺新方依据时不补，不推断 ID、不补计算操作数。最后运行原完整核验，失败整份降级。

raw 原始文本与 final 组装结果共同保留在现有64项内存缓存中。成功 final 为核验恢复原文后的规范结果，核验失败保留组装结果，解码失败 final=null。快照失效不返回审计；重开无内存结果仍零重调。数据库回执只存原有元数据，不新增正文保存或冻结输出字段。

真实来源接收时，sources 的 map key 是会话/轮次键，SourceRecord.id 是独立来源 ID。新回查及同链路原有的 origin_allowed/valid_evidence 两处检查均按 id 查找；不能把 source_id 当 map key。新增真实 SQLite 回归将键改为会话/轮次键，验证旧记录仍进入授权包、补入成功且删除后拒绝；任务/主体/所有者过滤条件不变。

评测 raw 直接按严格规则评分，final 组装后按相同规则评分。raw_result/raw_summary/raw_splits 与 final_result/summary/splits 分别报告；原录制、完整指纹、提示词及标签不变。

## 40 项离线重算

使用 73ebb41 的 DeepSeek-V4-Pro + minimal / R4 v2，20个已复核样本各重复2次：

```sh
EVAL_RECORD=0 EVAL_REASONING_EFFORT=minimal EVAL_MODEL_LABEL=DeepSeek-V4-Pro EVAL_MODEL=ep-20261003130756-trhj6 EVAL_STAGES=r4 EVAL_REPEAT=2 EVAL_BASE_URL=https://ark.cn-beijing.volces.com/api/v3 cargo test --offline --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
```

| 口径 | 全量 | dev | holdout | 首个核验失败 |
| --- | --- | --- | --- | --- |
| raw | 18/40 | 12/28 | 6/12 | 13：record_basis 8、operands 5 |
| final | 26/40 | 20/28 | 6/12 | 5：operands 5 |

预检 planned_calls=0、sent_calls=0，全部40项指纹有效；过期0、缺失0。补入8条旧引用，对以下每个样本的两次响应各补一次，raw失败、final通过：

| 样本 | 模型明确选定的旧目标 | 补入来源 |
| --- | --- | --- |
| r4_04_new_raw_overrides_old | 4 | s-4 |
| r4_07_current_message_assertion | 7 | s-7 |
| r4_10_superseded_history | 10 | s-10 |
| r4_12_end_unknown | 14 | s-14 |

其余14次仍失败：

| 样本/重复 | 次数 | 保留的失败 |
| --- | --- | --- |
| r4_16 / 1、2；r4_17 / 2；r4_18 / 1、2 | 5 | 空操作数，operands 核验失败 |
| r4_03 / 1、2；r4_17 / 1；r4_19 / 1、2 | 5 | verdict 与标签不符 |
| r4_14 / 1、2 | 2 | verdict、count 条目数/事件组数 |
| r4_15 / 1、2 | 2 | verdict、sum 条目/数量/单位 |

原始报告与解析后的完整 raw/final 报告在 `/private/tmp/r4-old-basis-replay.log`、`/private/tmp/r4-old-basis-report.json`。沿用原实测延迟数据，离线补入不产生新延迟测量。补入只证明引用真实存在，不能证明替代目标或旧条件/时间在新断言上的语义正确；附加 facts、statement、唯一当前值及 must_not 仍需人工复核。r4_14/r4_17 既有评分边界未扩大解释。

## 验证

新增6个测试函数，覆盖可补、包外目标、来源缺失/删除、正文缺片段（含仅空白不同）、短片段/空白等价依据不重复、幂等、缺新方依据/其他 record_ids 错误不修补、补后条件/操作数仍拒绝、真实 SQLite 无正文写入、独立来源 ID、晚到删除、raw/final 成对缓存/重放与重开零重调，以及评测 raw/final 汇总。

现有 r4_04 的“只有新方依据必须失败”断言按 Z18 改为自动补旧后成功；“只有旧方依据”仍失败。其余原有业务断言不变。未升库、未改冻结契约/提示词/样本/录制，零真实调用、未 commit。原有 docs/architecture/README.md 工作区2行改动保留。

变更文件：`src/r4.rs`、`src/recall.rs`、`src/recall_models.rs`、`src/recall_models_tests.rs`、`tests/recall_r4.rs`、`tests/write_stage_eval.rs`、`tests/eval/recall_stage/adapter.rs`；同步 `docs/architecture/recall-runtime.md`、本目录 README 及本报告。

实际检查：

- 首轮相关测试：`cargo test --offline --test recall_r4 --test write_stage_eval`，67通过、0失败（补齐回归前）。补入相关过滤回归4通过、0失败；日志 `/private/tmp/r4-old-basis-targeted.log`、`/private/tmp/r4-old-basis-regression.log`。
- 独立来源 ID 回归首轮在夹具准备阶段因 `source tombstone must be retained` 失败；改为新库首次种入后单独复跑1通过、0失败。没有改存储保护规则。日志 `/private/tmp/r4-old-basis-source-identity.log`、`/private/tmp/r4-old-basis-source-identity-fixed.log`。
- 首轮 `cargo test --offline --no-fail-fast`：728通过、19失败、33忽略，cargo退出101；19次均为沙箱端口/子进程端口 `Operation not permitted`，与既有端口清单完全一致，不重复计算 s5 父测试中的子测试。该轮在来源 ID 修正和第6个回归加入前编译；日志 `/private/tmp/r4-old-basis-full.log`。
- 最终版本清除全部 EVAL 环境变量、设置 `PMR_MODEL_NETWORK=offline`，仅逐名跳过下列19项，重新执行离线全套：**729通过、0失败、33忽略**，cargo退出0。新增6个测试函数全部通过；日志 `/private/tmp/r4-old-basis-nonport.log`。端口项沙箱外复跑：**未执行**；真实模型测试：**未执行**。
- 修改的7个 Rust 文件 `rustfmt --check --edition 2024 --config skip_children=true` 通过，`git diff --check` 通过。全仓 `cargo fmt --all -- --check` 退出1，仍是8个已有格式差异文件：src/chat.rs、model.rs、openai.rs、policy.rs、temporal.rs、temporal_grounding.rs、tests/eval.rs、tests/w4_legacy_fixture.rs；逐个确认与 HEAD 字节相同，本轮未动。日志 `/private/tmp/r4-old-basis-format.log`。
- R1/R4 提示词、schema、纯请求构造与 HEAD 完全一致；契约、数据库结构、样本、录制、HEAD 均未变。离线重算 planned_calls=0、sent_calls=0。

端口项（19）：

```text
explicit_modes_off_capture_and_bounded_inline_through_mcp
forward_retries_rate_limits_and_dropped_connections
frozen_candidates_need_independent_source_review_through_mcp
mcp_global_context_fallback
mcp_prepare_saves_before_recall
mcp_stdio_persistence_roundtrip
openai::recall_models::tests::recall_real_http_mock_two_sends_replay_and_reopen_zero_sends
openai::tests::assistance_sends_once_and_accepts_only_bounded_queries
openai::tests::evaluation_text_uses_distinct_budgeted_stages_and_respects_deny
openai::tests::extraction_verification_is_bounded_and_preserves_legitimate_empty_results
openai::tests::offline_gate_blocks_provider_and_redirect_without_sending
openai::tests::physical_sends_never_retry_connection_or_http_failures
openai::tests::query_reviews_omitted_named_entities_once_without_forcing_subjects
pending_update_state_blocks_model_assisted_answers
resident_eval_suite
s1_diagnostic_evidence
s2_independent_source_capture
write_pipeline_http_enrich_source_roundtrip
write_pipeline_http_mixed_drain_roundtrip
```
