# 新 inline 的快照变化评估（第4项⑤）

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

## 判断与建议

- `recall::Assembly::check_snapshot` 与在线 `RecallRuntime` 的 stamp 都包含来源整理状态；queued→running 在原文、业务 revision 未变时也可能使 R1 结果失效。事件屏障测试先等到 running 再召回，能证明前台不等待后台，却不能揭示接收后立即召回的这一窗口。
- 当前机制保证失效结果不泄露旧证据，安全门控保持；但在本实验的连续更新、立即召回节奏下，状态变化导致的失效明显影响可用性。不能仅凭最小并发测试通过就启用默认后台，也不建议宿主通过睡眠或普通同参重试规避。
- 建议另开方案讨论：将事实/原文/删除快照与仅整理进度的变化区分，允许后台进度推进而证据未变的请求完成，并明确回执的状态观察时刻；真正业务提交仍需快照一致性检查。若选择预算内显式重判，须重新明确请求ID与授权；不能暗中重复R1/R4。优先评审 R1 后按最新原文/事实状态重新组装证据而不重发 R1，R4 期间继续核对证据/业务/删除快照；R1 输入只包含问题、上下文与目录，但目录变化仍必须拦截。该策略需 Claude 审核，涉及冻结契约或新失败码时交用户决定。本段只评估、不实施、不改失败码。
- MCP+HTTP 七类端到端、真实模型质量/耗时和完整P95负载验收分别报告；本实验不能替代它们。

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

## 本轮检查

- `PMR_MODEL_NETWORK=offline cargo test --lib background_tests -- --nocapture`：23/0/0。
- 最终排除已确认29个端口阻断项的全套：workspace **797通过 / 0失败 / 33忽略**，命令参数 `/private/tmp/visibility-noport-command.json`，日志 `/private/tmp/visibility-noport-final.log`。未跳过其他测试；新增 a 测试允许 T1 后被扫描认领的 queued/running 合法竞态，继续验证前台先于后台模型释放完成。
- 包含端口项的 `PMR_MODEL_NETWORK=offline cargo test --all --no-fail-fast`：workspace 797通过 / 29失败 / 33忽略（根 crate 790通过、inspector 7通过）。29个失败均为端口受限，与第④段相比仅多出七个新 MCP 场景；无业务失败。原始日志 `/private/tmp/visibility-full.log`，清单 `/private/tmp/visibility-port-failures.txt`。
- `PMR_MODEL_NETWORK=offline cargo test --test mcp_visibility`：七项均在 loopback bind 时报 Operation not permitted，未进入业务验证。沙箱外复跑：**未执行**。
- Python 编译检查、a/f 实际请求与 Python mock 输出比对、git diff --check通过；真实模型调用0。
- 变更 Rust 文件/区域的 rustfmt检查通过。`cargo fmt --all -- --check`非零：HEAD已有 src/chat.rs、src/model.rs、src/openai.rs、src/policy.rs、src/temporal.rs、src/temporal_grounding.rs、tests/eval.rs、tests/w4_legacy_fixture.rs 共8个文件的格式差异，本段没有扩大修改范围。比较记录 `/private/tmp/visibility-format-scope.log`。
