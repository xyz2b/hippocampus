# R4 收尾：日期核验、v2 失败定位与旧方依据方案

## 离线回放口径

使用 73ebb41 的 Pro + minimal / R4 v2 原始录制，20 个样本各重复 2 次；不改样本、提示词、录制、请求绑定或评分语义。`EVAL_RECORD=0` 的全量预检确认需录制 0、发送 0；全部完整指纹继续校验，不绕过过期。

```sh
EVAL_RECORD=0 EVAL_REASONING_EFFORT=minimal EVAL_MODEL_LABEL=DeepSeek-V4-Pro EVAL_MODEL=ep-20261003130756-trhj6 EVAL_STAGES=r4 EVAL_REPEAT=2 EVAL_BASE_URL=https://ark.cn-beijing.volces.com/api/v3 cargo test --offline --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
```

修复前后均为 **18/40**，dev **12/28**、holdout **6/12**，核验失败 **13**；零过期、零缺失、零发送。解析后的 40 项完整报告逐项完全一致。原录制实测 p50 4542ms / p90 6302ms，离线回放不产生新的延迟测量。

## 13 次首个核验失败

报告保留冻结 `code=model_invalid_output`，增加具体 `rule`、`field`、可空 `cause`。下表逐次列出，不把它视为对整份输出所有错误的穷举。

| 样本 | 重复 | rule | field | 具体原因 |
| --- | --- | --- | --- | --- |
| r4_04_new_raw_overrides_old | 1 | record_basis | facts[1].supersedes_record_ids[0] | 替代 4，只引 s-40；s-4 只在另一条事实中 |
| r4_04_new_raw_overrides_old | 2 | record_basis | facts[1].supersedes_record_ids[0] | 同上 |
| r4_07_current_message_assertion | 1 | record_basis | facts[1].supersedes_record_ids[0] | 替代 7，只引 current；s-7 只在另一条事实中 |
| r4_07_current_message_assertion | 2 | record_basis | facts[1].supersedes_record_ids[0] | 同上 |
| r4_10_superseded_history | 1 | record_basis | facts[1].supersedes_record_ids[0] | 替代 10，只引 s-11；s-10 只在另一条事实中 |
| r4_10_superseded_history | 2 | record_basis | facts[1].supersedes_record_ids[0] | 同上 |
| r4_12_end_unknown | 1 | record_basis | facts[1].supersedes_record_ids[0] | 替代 14，只引 s-15；s-14 只在另一条事实中 |
| r4_12_end_unknown | 2 | record_basis | facts[1].supersedes_record_ids[0] | 同上 |
| r4_16_elapsed_request_at | 1 | operands | compute.operands | elapsed 的 items、operands 均为空，非固定两个端点 |
| r4_16_elapsed_request_at | 2 | operands | compute.operands | 同上 |
| r4_17_elapsed_month_precision | 2 | operands | compute.operands | elapsed 的 items、operands 均为空；粗精度不足不能豁免结构规则 |
| r4_18_compare_dates | 1 | operands | compute.operands | compare 的 items、operands 均为空，非固定两个端点 |
| r4_18_compare_dates | 2 | operands | compute.operands | 同上 |

分布：`record_basis` **8**，`operands` **5**。这 13 次不是 JSON/schema 解码错误；其余 9 次是标签字段评分失败，未改变评分口径。r4_14/r4_17 的已知评分边界仍待产品裁定。

## 日期核验修复

根因：W2 的字面日期检查只识别整段日期，整句前后有文字时落入“未识别自然语言”的允许分支；R4 的纯月名补查也不能覆盖整句。两个新增回归在旧代码上确实失败：整句月份被编成具体日仍获准，R4 最终返回 Answered。

在共享 W2 入口提取连续日历片段，复用原有日期/月/年/区间检查，保留完整区间，不枚举子串或猜测事件归属。R4 的事实 times、计算 date，以及 W2/R1 均经同一规则；删除 R4 重复的纯月名检查。回归覆盖整句月/年与日精度矛盾、错误日/月/年、非法日期、日期区间不能降为端点，以及合法整句日期/月/年、中文月名、空白、完整区间和多个日期。

边界：仅核验支持格式的明确日历表达；相对时间、未识别的自然语言和多日期的语义归属仍需模型判断。Unknown/Anchored/Recurring 保留原有 unknown，不机械制造日期。

## 机械补旧方依据：结论与方案（本轮未实现）

**可在冻结契约 3.3 内实现，无需升库或变更对外 JSON**：模型已经选定的 `supersedes_record_ids` 与包内记录的获准证据之间的引用关系可以机械确定。把该证据补到同条 FactView 后，再按现有“必须有新旧依据”规则严格核验最终输出，并未删除或放松规则。仅因另一条事实已经引用旧记录而直接放行，仍不满足同条依据要求，不推荐。

前提全部成立才补入：

1. 只处理模型明确列出的替代 ID；该记录在本次可信、授权证据包内。不能从库外补目标、推断 ID 或新增替代关系。
2. 旧记录具有可核准的 EvidenceRef；来源属于同一快照/权限范围且未删除，原 Quote 在本包相应来源片段内真实存在。legacy 的占位来源或 gaps 不能被修补成真实依据。
3. 原模型结构、既有 EvidenceRef 与 `record_ids` 依据先通过核验，只补明确替代 ID 的旧方引用缺失。原模型须保有区别于全部旧记录依据的真实新方依据；无效原依据仍报错，不能删掉或改写来“修复”，不能顺带修复其他记录 ID 的依据缺失。旧依据不能冒充新方依据。
4. 按包内记录顺序确定性补入、去重；最终再次运行完整 R4 核验（包括同条 record_basis/new_basis、限定语、时间和计算）。保留 pending/gaps、范围不完整与冲突规则，补引用不能清除这些状态或证明唯一当前值。返回前继续执行现有来源删除、快照与输出预算检查；截断不得静默丢弃必须的依据。

建议后续接在 `Assembly::finish_r4` 的 decode 与 evaluate 之间，用纯函数接收本包和模型输出；生产与离线评测共用入口，不调用模型、不扩大检索范围、不写持久事实或版本关系。评测保留原模型输出与组装后结果，分别报告 raw/final 得分和补入计数；只读生产回执不因此新增原文持久化。

**语义风险仍在**：模型可能选错替代目标、把条件/不同实体/不同时间的并存误当替代。补引用只证明旧证据真实存在，不能证明“应当替代”；这会消除一项结构告警，可能遮住上述语义错误。例如现有限定语/时间核验可在同条事实的任一依据来源找 Quote，自动加入旧依据后，误贴在新断言上的旧条件或旧时间也可能找到片段而机械通过。因此必须保留原始核验诊断、raw/final 差异与语义样本审核，不能把补入成功算作语义已核准，不能宣称这 8 次会全部通过。若让代码自行选择目标、补写断言或把补证据当作替代语义已核准，就超出本方案，应另行评审。

## 验证结果

- 旧代码红测：新增 W2 精度回归与真实 SQLite 的 R4 精度回归各失败一次；后者错误地返回 Answered 而非 Degraded。日志 `/private/tmp/recall-closeout-red.log`。
- 相关测试：`cargo test --offline --test w2_offline --test recall_r4 --test write_stage_eval`，96 通过、0 失败。新增4个测试函数，现有业务/回执断言未改。
- 日期修复前后各回放40项，均18/40，全部请求指纹有效；日志 `/private/tmp/recall-closeout-before.log`、`/private/tmp/recall-closeout-after.log`。零真实调用，无录制变更。
- 完整执行 `cargo test --offline --no-fail-fast`：**723 通过 / 19 失败 / 33 忽略**，退出101。失败全部为下列沙箱端口项，日志中对应 Operation not permitted；其中 s5 的父测试因端口子进程失败而失败，不重复计数。日志 `/private/tmp/recall-closeout-full.log`。
- 去除全部 EVAL 环境变量、设置 `PMR_MODEL_NETWORK=offline`，仅用19个明确用例名逐个 `--skip` 再跑非端口全套：**723 通过 / 0 失败 / 33 忽略**，退出0。日志 `/private/tmp/recall-closeout-nonport.log`。端口项沙箱外复跑：**未执行**。
- 本轮6个 Rust 文件的 `rustfmt --check --edition 2024 --config skip_children=true` 通过；`git diff --check` 通过。全仓 `cargo fmt --all -- --check` 退出1：src/chat.rs、model.rs、openai.rs、policy.rs、temporal.rs、temporal_grounding.rs、tests/eval.rs、tests/w4_legacy_fixture.rs 存在8个已有格式差异，逐个确认内容与 HEAD 一致，本轮未修改。日志 `/private/tmp/recall-closeout-format-final.log`。因此全仓格式验收仍有基线限制。
- 未升库、未改冻结契约/提示词/样本、未实现自动补旧依据、未 commit；原有 docs/architecture/README.md 的2行工作区改动保留。

端口项清单（19）：

```text
openai::recall_models::tests::recall_real_http_mock_two_sends_replay_and_reopen_zero_sends
openai::tests::assistance_sends_once_and_accepts_only_bounded_queries
openai::tests::evaluation_text_uses_distinct_budgeted_stages_and_respects_deny
openai::tests::extraction_verification_is_bounded_and_preserves_legitimate_empty_results
openai::tests::offline_gate_blocks_provider_and_redirect_without_sending
openai::tests::physical_sends_never_retry_connection_or_http_failures
openai::tests::query_reviews_omitted_named_entities_once_without_forcing_subjects
forward_retries_rate_limits_and_dropped_connections
resident_eval_suite
frozen_candidates_need_independent_source_review_through_mcp
mcp_global_context_fallback
mcp_prepare_saves_before_recall
mcp_stdio_persistence_roundtrip
s1_diagnostic_evidence
s2_independent_source_capture
pending_update_state_blocks_model_assisted_answers
explicit_modes_off_capture_and_bounded_inline_through_mcp
write_pipeline_http_enrich_source_roundtrip
write_pipeline_http_mixed_drain_roundtrip
```
