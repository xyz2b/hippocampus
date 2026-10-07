# R4 minimal 失败核对与第 2 版提示词

本轮零真实调用。只读核对 `.ai/log/recall-pro-minimal.log` 与 Pro `reasoning-minimal/repeat-{1,2}` 的40条 R4 录制：原始 output、请求指纹与报告40/40一致。复用现有样本适配器和 `r4::decode/evaluate` 在临时分析程序中核对详细失败码，没有修改样本、录制、评分器或核验器。

16次 decode/validate 失败均已成功解码为冻结结构；首个失败码为 `record_basis` 8次、`quote_or_source` 2次、`operands` 6次。不是缺字段、Quote 对象格式错误或非连续引用。另10次评分失败中，2次是事实身份/引用匹配失败，其余8次是语义评分失败。原结果仍为14/40，不重新标注或调整评分。

## 逐条核验失败

| 样本 | 重复 | 首个核验失败码 | 具体原因与次级问题 |
| --- | --- | --- | --- |
| r4_04_new_raw_overrides_old | 1 | record_basis | 新事实声明替代旧记录，但该事实 evidence 只有新方；旧方出现在另一事实，不能满足同条依据要求。 |
| r4_04_new_raw_overrides_old | 2 | record_basis | 同上；新事实的相对时间不改变旧方依据缺失。 |
| r4_09_legacy_gap | 1 | quote_or_source | 把记录 ID 当作 source_id，引用无真实来源的 legacy 占位内容。 |
| r4_09_legacy_gap | 2 | quote_or_source | 同上；还把 source_missing 元数据当成不确定性的原文依据。 |
| r4_10_superseded_history | 1 | record_basis | 替代事实只引用新来源，缺被替代旧记录的同条依据。 |
| r4_10_superseded_history | 2 | record_basis | 同上。 |
| r4_11_corrected_not_history | 1 | record_basis | 替代事实缺同条旧依据；还将已纠正的旧断言表述为真实历史。 |
| r4_11_corrected_not_history | 2 | record_basis | 同上。 |
| r4_12_end_unknown | 1 | record_basis | 替代事实缺同条旧依据；未知结束日期下，旧事实 applies=yes 不能证明查询时段持续有效。 |
| r4_12_end_unknown | 2 | record_basis | 同样缺同条旧依据；旧事实已保守标 applies=no，此处不额外判它为错误。 |
| r4_16_elapsed_request_at | 1 | operands | elapsed 的 items、operands 都为空；已知端点只写进 facts，缺计算条目与 @request.at。 |
| r4_16_elapsed_request_at | 2 | operands | 同上。 |
| r4_17_elapsed_month_precision | 1 | operands | elapsed 的 items、operands 都为空；月精度允许计算不足，但不能返回非法操作数。 |
| r4_17_elapsed_month_precision | 2 | operands | 同上；还漏保留 facts.times 的月时间。 |
| r4_18_compare_dates | 1 | operands | compare 的 items、operands 都为空；另外将月/年精度编造成具体日期。 |
| r4_18_compare_dates | 2 | operands | 同上。 |

## 其余评分失败（逐重复列出）

| 样本 | 重复 | 失败字段 | 判断 |
| --- | --- | --- | --- |
| r4_03_condition_applies_unknown | 1 | verdict、applies | 保留了条件，但把“条件已理解”当成“查询场景满足条件”；应保留条件适用性未知，不能给无条件答案。标签允许 supported 表示条件事实有依据。 |
| r4_03_condition_applies_unknown | 2 | verdict、applies | 同上。 |
| r4_07_current_message_assertion | 1 | facts.identity、current | 当前新断言未返回，只有旧事实 current=no；supported 不足以掩盖漏掉当前依据。属于真实遗漏，不是 JSON 解码失败。 |
| r4_07_current_message_assertion | 2 | facts.identity、current | 同上。 |
| r4_14_count_dedupe | 1 | verdict、distinct_groups、items | facts 有相关事件，compute.items 却为空，无已确认组可计数；这是遗漏计算输入。 |
| r4_14_count_dedupe | 2 | verdict、distinct_groups、items | 同上。 |
| r4_15_sum_quantities | 1 | verdict、items、quantities、unit | facts 有原文数量，但计算条目、数量、单位全部遗漏。 |
| r4_15_sum_quantities | 2 | verdict、items、quantities、unit | 同上。 |
| r4_19_truncated_count_lower_bound | 1 | verdict | 截断禁止精确总数，不禁止有证据的下限；实际条目为空，也无法由代码计算下限。只把 verdict 改 supported 不足以修复。 |
| r4_19_truncated_count_lower_bound | 2 | verdict | 同上。 |

## 提示词修改对照

| 第2版规则 | 对应失败 |
| --- | --- |
| 每个 record/supersedes ID 必须在同一 FactView 内有对应旧依据；替代事实还需新依据，其他事实的证据不算。 | 04、10、11、12 共8次核验失败。 |
| Quote/EvidenceRef/SourceRef 明确形状；source_id 不能用记录 ID；legacy 占位内容与缺口元数据不可作为来源，没有依据则省略事实。 | 09 共2次；其他形状规则用于预防，并未声称本轮出现此类错误。 |
| facts 与 compute.items 分开填写；列明 ComputeItem 全部6个必填字段、quantity 形状、date/date_source 配对；有真实输入时不以空 items 表示不确定。 | 14、15、16、17、18、19 的计算输入遗漏。 |
| count/sum 操作数为空；elapsed/compare 恰好2个；当前结束点直接引用 @request.at、不创建时钟条目；不足也须给合法结构或 compute=null。 | 16、17、18 共6次 operands 失败。 |
| Quote 选择时间表达本身；月/年保留原精度；facts.times 与计算 date 均不得补日；精度不足仍保留已知端点。 | 17 精度不足；18 编造日期。 |
| 条件 resolved 与 applies 分开；unknown 情境不自动适用；已纠正断言不能表述成真实历史；当前/raw 新断言不能遗漏。 | 03、07、11、12 的适用性或事实遗漏。 |
| 支持条件事实不等于无条件答案；保留可确认的计数组、区分下限与精确总数；缺口不由自报 complete 覆盖。 | 03、14、15、19 的 verdict/coverage；不强行让粗精度 elapsed 判 supported。 |
| 简短输出前自检清单覆盖字段、真实引用、同条新旧依据、计算输入、操作数及时间精度。 | 覆盖以上结构失误，未加入样本文字、ID、标签。 |

## 尚未解决的边界

- 第2版的质量、输出长度和延迟尚未真实测量；旧 minimal R4 录制按完整请求指纹过期，不能冒充第2版结果。下一轮需40次 R4 调用，另行授权后执行。
- r4_18 的伪造日期首次失败实际是 operands；整句时间 Quote 没有触发当前核验器对纯月/年表达的字面精度检查（`src/w2.rs:600`、`src/r4.rs:225`）。提示词已要求选时间表达并保留精度，核验器按本轮范围未改，机械检查缺口仍存在。
- r4_17 标签 verdict=supported 与“粗精度 elapsed 返回不足”的提示词/计算规则存在口径张力；本轮不改标签、评分或契约，不为提高分数强制 supported。
- r4_14 的 compute.items 评分要求3条，合并重复事件的证据形成2条也可能合约有效；当前 minimal 输出0条仍是真实遗漏。本轮不修改评分。
- 推荐配置未改变代码缺省值、预算或 CLI/MCP 行为。单阶段耗时不能证明宿主全流程 P95，冷启动与生产预算仍需后续验收。

## 本轮实际验收

- 相关测试：提供者19/0（排除1个端口用例），评测脚本34/0；既有泄漏扫描通过。新增推荐配置解析/缺省值测试，既有录制兼容测试保留 R1 完整指纹核对并增加 Pro minimal；R4 旧录制不能继续断言有效。
- `cargo test --offline --no-fail-fast`：719通过、19失败、33忽略。19项均与下列既有端口清单吻合，每个失败目标日志均有 `Operation not permitted`。
- 排除下列19项复跑离线全套：719通过、0失败、33忽略。沙箱外端口复测：**未执行**。
- Pro minimal 两阶段离线回放：实际调用0；R1 的40条录制有效、评分23/40，R4 的40条全部过期。R1 独立 `EVAL_RECORD=0` 严格预检及回放也通过，实际调用0。
- 修改的3个 Rust 文件 `rustfmt --edition 2024 --config skip_children=true --check` 通过；`git diff --check` 通过。全仓 `cargo fmt --all -- --check` 未通过：chat.rs、model.rs、openai.rs、policy.rs、temporal.rs、temporal_grounding.rs、tests/eval.rs、tests/w4_legacy_fixture.rs 共8个未改文件存在既有格式差异，未扩大范围格式化。
- 新提示词真实重录、质量与延迟测量：**未执行**（本任务零真实调用）。没有 git commit，HEAD仍为2488029。

干跑使用 `EVAL_REASONING_EFFORT=minimal EVAL_MODEL_LABEL=DeepSeek-V4-Pro EVAL_MODEL=ep-20261003130756-trhj6 EVAL_STAGES=r1,r4 EVAL_REPEAT=2`；以下为计划输出原文：

```text
分阶段评测干跑计划（不发送请求）
模型: DeepSeek-V4-Pro，推理强度: minimal
r1: 40 次潜在真实调用，需录制 0
  dev: 28 次潜在真实调用
  holdout: 12 次潜在真实调用
r4: 40 次潜在真实调用，需录制 40
  dev: 28 次潜在真实调用
  holdout: 12 次潜在真实调用
合计: 80 次潜在真实调用
本次需录制: 40
本次真实调用: 0
```

端口项单列（本次全部在沙箱内尝试，外部复测未执行）：

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
