# AF236：Anthropic 原生 schema 独立诊断

仅评测用 `native_simplified`，生产入口拒绝。使用六种当前完整 schema：
W1 v4、W3 v4、W4 v5、R1 v2、R4 v7 stored-only / current-enabled。
虚构正文与目录独立于历史评测样本；不读写 AF230 配置、录制、额度。

发送副本仍删除整数上下界，展平无其他约束的嵌套 anyOf，将重复对象
（Quote、TimeInterp、限定语等）提为无环本地 definitions/$ref。
保留 pattern、required、类型和分支字段，不改响应形状、不做反向映射。
录制绑定原 schema、发送 schema、改写版本、完整请求、端点及超时。
其他 provider 的请求、模式和全局 ADAPTER_VERSION 不变。

Haiku low，输出上限每请求 4096，总超时 360 秒：这是短输入的 schema
编译诊断配置，不是生产性能/语义质量评测。各轮输入和请求字节相同。
每条保存原始响应、规范化 usage、耗时，以及发送格式/原 schema 核验两栏。
核验仅覆盖现有 schema 子集，不认证语义支持或业务提交。

零调用干跑（不需要密钥、不创建产物；真实端点存在时只读取端点）：

```sh
EVAL_AF236=1 EVAL_DRY_RUN=1 cargo test --test anthropic_schema_eval af236_diagnostic -- --exact --nocapture
EVAL_AF236=1 EVAL_DRY_RUN=1 EVAL_AF236_ROUND=2 cargo test --test anthropic_schema_eval af236_diagnostic -- --exact --nocapture
```

真实运行由 Claude 复核后执行，不由本轮实现发起：

```sh
PMR_MODEL_NETWORK=live EVAL_AF236=1 EVAL_RECORD=6 cargo test --test anthropic_schema_eval af236_diagnostic -- --exact --nocapture
PMR_MODEL_NETWORK=live EVAL_AF236=1 EVAL_RECORD=6 EVAL_AF236_ROUND=2 cargo test --test anthropic_schema_eval af236_diagnostic -- --exact --nocapture
```

固定独立根目录 `target/eval-anthropic-schema-af236/`；campaign 冻结全部
12 槽（第一轮 6、第二轮 6）。`EVAL_RECORD=6` 是单轮授权，不追加总额度。
第二轮仅在第一轮六条全部完成、usage 正常且两栏核验通过后解锁。
这里“冷/暖”表示首轮/重复请求，不保证代理或服务端的真实缓存命中。

发送前持久记录占用槽位；失败、截断、拒绝或中断在途即停止整个 campaign，
不重试、不切模式、不重开额度。逐条响应立即落盘；重跑严格复用相同绑定。
配置/提示词/schema/端点/改写版本变化时报过期，不绕过指纹复用。
若需修复后重新诊断，应另行批准新 campaign，保留本次结果。

零调用测试：`cargo test --test anthropic_schema_eval`。
schema 字节为紧凑 UTF-8 JSON；anyOf/ref 数统计发送文件全部节点（含 definitions），
与完整展开节点数不同。定义复用减少字节，不保证服务端编译状态随之减少。

## AF238：独立去非空 pattern 诊断

AF236 第一轮 W1 返回 `schema_grammar_too_large` 后停发，原目录和额度不动。
新配置 `diagnostic-af238.json` 使用评测专用 `native_simplified_nopattern`：
在 AF236 发送副本上，仅去掉 `^[\s\S]*\S[\s\S]*$`，其他 pattern 保留。
不改响应形状，不做反向映射；生产仍拒绝此模式。
绑定区分模式、改写版本，并保存原 schema、发送 schema和阶段核验版本。
全局 adapter 版本与其他 provider 的请求字节不变。

独立目录 `target/eval-anthropic-schema-af238/`，冻结 11 槽：冷请求六种各一次；
六条全部通过后暖请求 W1、W4、R1、R4-stored、R4-current 各一次（省略较小的 W3）。
额度不会随轮次、续跑或筛选增加；失败、截断或中断在途即停，不重试。
暖请求只重复对应冷请求，模型可见输入与请求字节相同。

报告分列发送 schema、原 schema、阶段核验，三栏及 usage 均通过才成功。
原 schema 仍保留非空约束，空白字符串不能因发送副本放宽而被接受。
阶段核验复用 W1 decode→W2、W3 decode/validate、W4 prepare_output→validate、
R1 decode/validate_detailed、R4 decode/evaluate_detailed；R1 的降级诊断也算失败。
固定 R4 包没有记录，因而不需补旧方依据。仅核机械规则，不判语义标签或提交成功。
原始响应与核验结果保存，回放重新核验，不能只相信保存的成功标记。

零调用干跑（不需密钥，不建产物，`EVAL_RECORD` 同时示明后续每轮额度）：

```sh
EVAL_AF238=1 EVAL_DRY_RUN=1 EVAL_RECORD=6 cargo test --test anthropic_schema_eval af238_diagnostic -- --exact --nocapture
EVAL_AF238=1 EVAL_DRY_RUN=1 EVAL_RECORD=5 EVAL_AF238_ROUND=2 cargo test --test anthropic_schema_eval af238_diagnostic -- --exact --nocapture
```

Claude 复核后执行的录制命令（本实现阶段未执行）：

```sh
# 先加载 ANTHROPIC_BASE_URL、ANTHROPIC_API_KEY；不打印密钥。
PMR_MODEL_NETWORK=live EVAL_AF238=1 EVAL_RECORD=6 cargo test --test anthropic_schema_eval af238_diagnostic -- --exact --nocapture
# 仅在首轮六条全部成功后运行：
PMR_MODEL_NETWORK=live EVAL_AF238=1 EVAL_RECORD=5 EVAL_AF238_ROUND=2 cargo test --test anthropic_schema_eval af238_diagnostic -- --exact --nocapture
```

单次上限 4096 输出 token、360 秒；冷/暖输出上限 24576/20480，合计 45056。
去非空 pattern 仍可能无法解决服务端 schema 编译超限，需真实诊断确认；不自动回落。

## AF240：仅召回 schema 的独立诊断

AF238 冷轮 W1 编译超限后停发，AF236/AF238 原配置、账本与产物保留。
`diagnostic-af240.json` 沿用 Haiku low、`native_simplified_nopattern`、4096 输出上限、
360 秒超时及严格三栏核验。固定顺序 R1 → R4-stored → R4-current，各一次；
不发送 W1/W3/W4，没有暖轮。新目录 `target/eval-anthropic-schema-af240/`，
独立 campaign 冻结 3 槽（占用总批准 12 次中尚余 10 次的一部分），不能筛选扩额。
请求绑定保持相同请求的身份，AF240 槽位另绑定 campaign；不借用旧 campaign 的槽位。
发送前落盘；任一失败、截断或中断在途即停，不重试、不切模式。
成功槽位续跑严格复用，核验原响应；第二轮和未知槽位明确拒绝。

零调用干跑（不需密钥、不创建产物）：

```sh
EVAL_AF240=1 EVAL_DRY_RUN=1 EVAL_RECORD=3 cargo test --test anthropic_schema_eval af240_diagnostic -- --exact --nocapture
```

Claude 复核后录制（本实现阶段未执行；先加载 `ANTHROPIC_BASE_URL`、
`ANTHROPIC_API_KEY`，不打印密钥）：

```sh
PMR_MODEL_NETWORK=live EVAL_AF240=1 EVAL_RECORD=3 cargo test --test anthropic_schema_eval af240_diagnostic -- --exact --nocapture
```

上限 3 次、12288 输出 token，逐条原始响应、usage、核验结果立即落盘。
这是固定虚构短输入的 schema 诊断，不代表生产语义质量或大输入性能。
