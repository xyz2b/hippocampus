# 新 inline 的快照策略与节奏评估（第4项⑤、AB2）

AB2 已将召回有效性改为“输入绑定完整且仍获准可见”，普通后台提交/整理进度不再要求全局版本相等。以下先保留修正前的测量，再给出修正后同节奏与真正 Prepare 的数据；历史数据不代表当前策略。

## 方法与边界

`src/chat_visibility_tests.rs::visibility_normal_cadence_snapshot_change_frequency` 使用真实 SQLite、MCP 的 `call_shared` 分发、单后台 worker、前台2/后台3的联合预算。每轮先接收一条住所更新，再按固定节奏召回；下一轮在上一来源已完成后开始。所有模型响应均为注入的合成结果，零真实调用、无端口；没有事件屏障、没有指定前后台完成先后，也不隐式重试失败的召回。

每组30轮，轮流在两个住所之间更新。后台 W1/W3/W4 和前台 R1/R4 的单阶段响应分别注入固定毫秒延迟；这些只是模型等待时间，SQLite、请求绑定和代码开销另外发生，不能当作整条路径耗时，也不代表生产模型延迟或P95。由于该夹具持续保留阶段审计，后续轮次的 SQLite JSON 工作量会增长。

统计 `recall_snapshot_changed` 与其他失败。R1 物理请求开始时读取本次持久回执的初始 stamp，失败返回后立即读取最新状态：revision 已增的归为“伴随业务提交”，revision/source_revision 均未变但 source_metadata 改变的归为“仅整理状态变化”。前者是提交与请求区间重叠的观测，不能把每次重叠都认定为唯一失败原因；后者能证明业务提交不是触发失效的必要条件。本实验没有其他来源写入、删除或目录更新。

## 数据

全套结束后的独立串行无屏障测量（仅运行该频率测试，2026-10-04，本地 macOS / Darwin arm64 沙箱）如下：

| 保存后停顿 | 后台/前台单阶段等待 | 快照失效 | 伴随业务提交 | 仅整理状态变化 | 其他失败 |
| --- | --- | --- | --- | --- | --- |
| 150 ms | 2 / 5 ms | 5/30 | 5 | 0 | 0 |
| 35 ms | 2 / 5 ms | 21/30 | 1 | 20 | 0 |
| 立即召回 | 8 / 20 ms | 30/30 | 0 | 30 | 0 |
| 立即召回 | 25 / 5 ms | 30/30 | 0 | 30 | 0 |

原始输出：`/private/tmp/visibility-cadence-serial.log`。这一轮总120次召回，86次快照失效，其中6次伴随提交、80次仅状态变化；其余34次工具调用成功（不等同全部 answered），其他工具失败为0。联合预算实际预留527次合成物理调用（各组151/132/122/122），均未超过各自授权；R1 后快照失效的请求没有继续发 R4，更没有自动重试。串行测量耗时148.87秒。

实际执行命令（全套构建的测试二进制）：

```sh
PMR_MODEL_NETWORK=offline target/debug/deps/hippocampus-d97c788aeb9417fd --exact openai::write_models::background_tests::visibility::visibility_normal_cadence_snapshot_change_frequency --nocapture --test-threads=1
```

可移植重跑命令：

```sh
PMR_MODEL_NETWORK=offline cargo test --lib visibility_normal_cadence_snapshot_change_frequency -- --nocapture --test-threads=1
```

相关测试首轮（同时运行23项后台回归）各组为6/30、21/30、30/30、30/30，合计87/120；伴随提交11、仅状态变化76、其他失败0，原始日志 `/private/tmp/visibility-background-checked.log`。运行调度会改变窗口暴露，不能将这些比例外推为生产概率。

## 修正前的问题与 AB2 决定

- `recall::Assembly::check_snapshot` 与在线 `RecallRuntime` 的 stamp 都包含来源整理状态；queued→running 在原文、业务 revision 未变时也可能使 R1 结果失效。事件屏障测试先等到 running 再召回，能证明前台不等待后台，却不能揭示接收后立即召回的这一窗口。
- 旧机制在连续更新、立即召回节奏下出现普通状态失效，不能靠宿主睡眠或同参重试规避。AB2 采用读快照语义：R1 后只装配一次 R3；整理、提交、关系/update 推进和随后新增来源均允许完成，输出仍是该次 R3 版本。新 request_id 才读新版本。没有重复 R1/R4，也没有新增失败码或改冻结契约。
- 来源/记忆删除（含无来源 legacy）、正文/历史/接收时间、归属/主体/任务、控制限制及执行身份仍严格核对。最后核验与入缓存同处短事务；重放、Validate、MCP 发布/交付共用检查。删除不能插入检查到发布之间。已发布内容不能撤回，后续交付必须丢弃失效副本。Prepare 顶层整理回执与 recall 共用 R3 观察点；queued 表示原文已收到、读取时整理待执行，而非未接收。
- MCP+HTTP 七类端到端、真实模型质量/耗时和完整P95负载验收分别报告；本实验不能替代它们。

## AB2 修正后的测量

2026-10-05，本地 macOS 沙箱，无事件屏障的独立串行测量，真实 SQLite、注入模型响应、零真实调用。仍为每组30次的原120次召回。现在对所有请求统计 R1 开始到返回之间的变化，而非只统计失败请求；因此“伴随提交/仅状态变化”的计数不能直接与旧表比较。它们说明这些变化已穿过成功路径，不是失败原因。

| 保存后停顿 | 后台/前台单阶段等待 | 快照失效 | 伴随业务提交 | 仅整理状态变化 | 其他失败 | 合成调用 |
| --- | --- | --- | --- | --- | --- | --- |
| 150 ms | 2 / 5 ms | 0/30 | 15 | 0 | 0 | 152 |
| 35 ms | 2 / 5 ms | 0/30 | 5 | 15 | 0 | 152 |
| 立即召回 | 8 / 20 ms | 0/30 | 0 | 30 | 0 | 152 |
| 立即召回 | 25 / 5 ms | 0/30 | 0 | 30 | 0 | 152 |

120次召回全部成功；95次观察到版本/进度推进，另25次没有观察到变化。总608次合成调用（含每组2次初始化调用），高于旧失败策略的527次，因为成功请求会完成 R4；**预算和阶段上限没有增加**：每个前台请求最多2次、每个后台来源最多3次，无自动重试。

新增 `visibility_real_prepare_immediate_recall_success_frequency` 直接调用实际 `memory_prepare`：本轮原话在同一工具内保存并立即召回，而非先 Remember 再 Recall。检查 source=stored、recall=answered、事实精确引用本轮原话、顶层与 raw 的整理状态一致；后台完成后延迟交付、同参重放仍保持原 recall/整理回执，重放零调用。

| Prepare 节奏 | 后台/前台单阶段等待 | 成功 | 失败 | 合成调用 |
| --- | --- | --- | --- | --- |
| 立即召回，较快后台 | 8 / 20 ms | 30/30 | 0 | 152 |
| 立即召回，较慢后台 | 25 / 5 ms | 30/30 | 0 | 152 |

原始输出 `/private/tmp/snapshot-cadence.log`；上述测量所在串行测试运行11项、11通过，耗时319.88秒。重跑命令：

```sh
PMR_MODEL_NETWORK=offline cargo test --lib visibility -- --nocapture --test-threads=1
```

这些合成等待仅验证并发语义及预算，不是生产延迟或 P95 数据。

## 授权调整的旧断言

- `tests/recall_offline.rs::changed_enrichment_progress_returns_the_original_read_snapshot`：queued→failed 不再拒绝；原组装仍为 queued，新组装为 failed。
- `src/recall_models_tests.rs::replay_preserves_observed_enrichment_when_only_progress_changes`：failed→needs_rejudge 后，同参重放/保留结果沿用原快照；新 ID 观察 needs_rejudge。
- 同文件 `mcp_online_delete_invalidates_but_new_sources_preserve_both_model_stages_without_retry`：仅普通新增来源分支改为执行者、等待者、重放成功；各阶段仍最多一次。删除分支原失败码、正文清除、空缓存和发送次数断言原样保留。
- `src/chat_background_tests.rs::background_commit_during_foreground_r4_preserves_snapshot_in_three_controlled_runs`：三轮后台提交后前台成功，版本和 raw 的 running 状态仍是旧 R3；原7次调用断言不变。
- `src/chat_visibility_tests.rs::visibility_normal_cadence_snapshot_change_frequency`：进度/提交重叠改为统计全部请求，另明确断言120次快照失效为0；原调用上限不变。

新增回归覆盖输入/控制/执行身份改变、R1/R4/完成后的无来源 legacy 删除、R3 新来源的后续删除、最后检查到入缓存窗口、Prepare 发布到延迟交付窗口，以及普通版本推进后的实际 MCP Validate。原删除测试没有放宽。

## AB2 最终检查

- `PMR_MODEL_NETWORK=offline cargo test --all --no-fail-fast` 加29个已确认端口项的 `--skip`：workspace **807通过 / 0失败 / 33忽略**（根 crate 800通过、inspector 7通过），退出码0。命令参数 `/private/tmp/snapshot-noport-command.json`，日志 `/private/tmp/snapshot-noport-final-2.log`；两组频率测试在全套中再次通过。没有跳过其他测试。
- 未过滤首轮：804通过 / 30失败 / 33忽略。29项均为 loopback bind 的 Operation not permitted；另一项为构建时尚未调整的“后台提交必须失败”旧断言，已按 AB2 修改并通过最终全套。首轮日志 `/private/tmp/snapshot-full.log`，端口清单 `/private/tmp/snapshot-port-failures.txt`。29项端口业务断言及沙箱外复跑：**未执行**。
- 最终全套前发现旧 S4 诊断字段因无条件刷新辅助 global_context 而改变；已将刷新限定到新召回响应，旧测试和断言原样保留。`cargo test --test mcp_stdio s4_local_mcp_and_frozen_development_cases -- --nocapture` 复跑1/0，最终全套也通过；日志 `/private/tmp/snapshot-s4-final.log`。
- 变更 Rust 文件/区域的 rustfmt 与 `git diff --check` 通过。`cargo fmt --all -- --check` 仍只报告 HEAD 已有的8个文件差异（与第⑤段同一组）；没有扩大格式修改范围。记录 `/private/tmp/snapshot-fmt-all.log`、`/private/tmp/snapshot-format/scope-result.log`。
- 零真实模型调用，不提交 git；不升库、不改 contracts.md、不放宽 W5。当前安全绑定保守覆盖可信任务内既有来源/可见记录；这里只验证合成并发和预算，未验证生产 P95。

## 七类场景对照

| 场景 | 无端口 MCP 分发 / SQLite 测试 | MCP stdio + HTTP 测试 |
| --- | --- | --- |
| a 新原文与已提交版本 | `visibility_a_inline_raw_then_committed_new_version` | `mcp_visibility_a_inline_raw_then_committed` |
| b 假设不更新 | `visibility_b_hypothesis_is_not_a_current_update` | `mcp_visibility_b_hypothesis` |
| c 失败原文及状态 | `visibility_c_failed_enrichment_keeps_raw_evidence_and_status` | `mcp_visibility_c_background_failure` |
| d 截断门控 | `visibility_d_truncated_related_raw_blocks_old_current_value` | `mcp_visibility_d_truncated_new_raw` |
| e 乱序更新屏障 | `visibility_e_newer_update_commits_first_without_older_overwrite` | `mcp_visibility_e_out_of_order_updates` |
| f 跨日接收日 | `visibility_f_cross_day_uses_received_day_for_relative_time` | `mcp_visibility_f_cross_day_relative_time` |
| g 前后台删除 | `visibility_g_deletion_invalidates_foreground_and_late_background` | `mcp_visibility_g_delete_during_both_models` |

本机无端口七项已通过，a/f 还使用实际物理请求核对 Python HTTP mock 的输出与注入输出相同。MCP+HTTP 七项尝试运行时均因 `PermissionError: Operation not permitted` 不能绑定 loopback 端口，尚未执行业务断言；须沙箱外复跑，不计为端到端通过。

## 第⑤段历史检查

- `PMR_MODEL_NETWORK=offline cargo test --lib background_tests -- --nocapture`：23/0/0。
- 最终排除已确认29个端口阻断项的全套：workspace **797通过 / 0失败 / 33忽略**，命令参数 `/private/tmp/visibility-noport-command.json`，日志 `/private/tmp/visibility-noport-final.log`。未跳过其他测试；新增 a 测试允许 T1 后被扫描认领的 queued/running 合法竞态，继续验证前台先于后台模型释放完成。
- 包含端口项的 `PMR_MODEL_NETWORK=offline cargo test --all --no-fail-fast`：workspace 797通过 / 29失败 / 33忽略（根 crate 790通过、inspector 7通过）。29个失败均为端口受限，与第④段相比仅多出七个新 MCP 场景；无业务失败。原始日志 `/private/tmp/visibility-full.log`，清单 `/private/tmp/visibility-port-failures.txt`。
- `PMR_MODEL_NETWORK=offline cargo test --test mcp_visibility`：七项均在 loopback bind 时报 Operation not permitted，未进入业务验证。沙箱外复跑：**未执行**。
- Python 编译检查、a/f 实际请求与 Python mock 输出比对、git diff --check通过；真实模型调用0。
- 变更 Rust 文件/区域的 rustfmt检查通过。`cargo fmt --all -- --check`非零：HEAD已有 src/chat.rs、src/model.rs、src/openai.rs、src/policy.rs、src/temporal.rs、src/temporal_grounding.rs、tests/eval.rs、tests/w4_legacy_fixture.rs 共8个文件的格式差异，本段没有扩大修改范围。比较记录 `/private/tmp/visibility-format-scope.log`。
