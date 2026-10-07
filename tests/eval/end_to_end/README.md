# 端到端历史归档与当前回放

这些文件是获批真实运行的历史证据，不随当前代码改写。默认严格回放只复用完整请求指纹相同的录制；当前235复用/59缺口/0发送不能计为63题当前通过，补录须另行授权。

已接受的 common.v1 63题59通过/4失败/0待复核按以下顺序重算：

1. `ac2-pro-medium-pro-minimal-pro-minimal-r4-compute-low/run.json`：AC2 全量运行。
2. `ac2-af-targeted/run.json` + `target-plan.json`：只替换计划中16题。
3. `af14-targeted/run.json` + `target-plan.json`：再替换 conflict_q2、conflict_q3、event_q5。
4. `../scoring/review_ledger.json`：只有当前标注/输出/语义状态对应的 confirmed 项有效。

AF14 的两个文件从本地 `.ai/log/af14-targeted/` 原样迁入；不迁入阶段录制、发送日志和分用例报告，评分不依赖本地目录。两轮计划均绑定 AC2 原 run.json 的 SHA-256 `37af009b784bb3ee55931b8dc831f50c88e11eeca86d2c82923304d79a84553c`。run.json 内原发送日志路径是历史元数据，不要求本地路径存在，也不表示本轮发生发送。

`tests/eval/scoring/test_m1_history.py` 调用现有合并、适配与评分器自动重算，并断言失败集合：unresolved_conflict/conflict_q2、lme_7a87bd0c/q、lme_a2f3aa27/q、lme_59524333/q。历史合并不是全量重测，不代替当前请求严格回放。

默认回放输出与代理日志写入 `target/eval-replay/`，显式输出使用临时目录，不能把回放产物放回历史录制目录。命令见 [评测指南](../README-v3.md)。0517000 旧基线及原结论另在 [baseline/README.md](../baseline/README.md) 保留，仅作参考。
