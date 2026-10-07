# hippocampus

A personal memory system for AI assistants that preserves your original words, follows changes over time, and recalls relevant context with traceable sources.

面向 AI 助手的个人记忆系统：保留你的原话，记录信息随时间的变化，在需要时找回相关上下文，并提供可追溯的来源。

## 特点

- **保留原话**：原文与整理出的记忆分别保存、分别报告状态；整理失败时仍能保留获准保存的原文。
- **记住关系与变化**：区分人物、物品、事件等主体，处理偏好、约束、事实、经历，以及更正、冲突和有效期限。
- **按上下文找回**：结合原文搜索与结构化记忆，按主体、时间和适用条件召回相关内容。
- **来源可追溯**：记忆关联原文证据；提供来源核验与答案校验，在缺少依据或主体不明确时保守处理。
- **本地保存与控制**：记忆存于本地，可查看、删除；通过 MCP 接入 AI 助手。
- **过程可检查**：独立的只读 Memory Inspector 展示原文、记忆版本、检索结果和模型调用记录。

## 当前状态

项目仍在开发中。M1 历史全量与两轮定点结果按 common.v1 合并为 63 题 59 通过 / 4 失败 / 0 待复核（用户已确认）。默认写入 w4_runtime_v1、召回 recall_v3；旧入口不再兼容，旧数据读取与删除保留。完整负载 P95 验收尚未完成。

模型可能漏抽、错误理解时间，独立复核也可能漏判。缺少证据时拒答不等于效果验收通过；MCP 工具也无法强制第三方聊天客户端原样采用核准答案。

项目原名 Personal Memory Runtime。Rust 包与库现名为 `hippocampus`；为兼容已有客户端，命令仍为 `pmr-mcp`、`pmr-inspector`，环境变量仍使用 `PMR_*`，默认数据目录仍为 `~/.local/share/personal-memory-runtime/`。

## 构建与运行

需要支持 Rust 2024 edition 的工具链。测试还使用 Python 3 和 Node.js。

```sh
git clone git@github.com:xyz2b/hippocampus.git
cd hippocampus
cargo build --workspace --bins
./target/debug/pmr-mcp --help
```

旧写入与召回执行器、`basic` / `s5_assisted_eval` 示例已删除；当前接入使用 MCP 新路径。旧数据读取、管理和删除仍保留。

### MCP 接入

将客户端的 MCP 启动命令设为已构建的程序，并指定数据库和固定用户身份。例如：

```json
{
  "mcpServers": {
    "hippocampus": {
      "command": "/absolute/path/to/hippocampus/target/debug/pmr-mcp",
      "args": [
        "--db", "/absolute/path/to/memory.sqlite3",
        "--owner", "local-user",
        "--config", "/absolute/path/to/memory-config.json"
      ]
    }
  }
}
```

先创建只保存原文、不自动整理的配置：

```json
{
  "enrichment_mode": "off"
}
```

配合 `PMR_MODEL_NETWORK=deny` 使用此配置可确保全服务不调用模型；保存时仍须明确授权原文留存并传入 `capture_source=true`。原文保存成功与结构化整理成功是两个独立结果。

完整配置可通过 `pmr-mcp --print-default-config` 查看。调用规则见[客户端接入约定](docs/memory-client-contract.md)与[自动记忆规则](docs/personal-memory-instructions.md)。

新聊天 bootstrap 后继续处理同一条用户消息；获准个人背景、计划原话、补充或更正用 memory_prepare 独立保存，具体个人问题另用普通 memory_recall。bootstrap 空结果不代表没有个人记忆，在线请求不要自行设 offline=true。未登记家人等人物用 memory_entity_upsert 明确登记确认的称呼（不要求真名；有歧义问一次，不把属性/事件当人物）。

原文保存与整理状态分别核对；pending 表示原话已保存、整理待处理，可用 memory_source_status 查看来源状态及候选索引/原因（零模型调用）。memory_list 的 pending_sources 与旧 staged pending 分开分页，不提供事实值或原文。实体登记不自动重建旧候选；计划结构化存储与主体确认/重建/历史补录将在 M1.5 后续步骤实现。

### 可选模型能力

默认 W0 保存原文，W1/W3/W4 后台整理，R1/R4 前台召回。inline/deferred 在 T1 后排队，off 不自动整理。Recall 包括 bootstrap 须稳定 request_id，Prepare 缺省从 session/turn 派生。capture_source=true 必须有整条原文及必要 USER history 的保存授权；省略/false 被拒绝。

模型调用仍需 DEEPSEEK_BASE_URL、DEEPSEEK_API_KEY、有限持久 model_budget 和网络门控。缺省角色为 W1/W3 Pro low、W4 Pro medium、R1/R4 Pro minimal、计算 R4 low；设置 `DEEPSEEK_PRO_MODEL` 为自己的 Pro 接入点/模型 ID，或显式配置角色。缺少默认模型只在需要在线提供者时报错，offline/Bootstrap 可用。在线配置示例见 [background-recall.json](examples/background-recall.json)，其中预算须按用户授权填写，不能自动提高。

- PMR_MODEL_NETWORK=offline（默认）：只允许字面本机回环测试端点。
- PMR_MODEL_NETWORK=deny：禁止所有模型 HTTP。
- PMR_MODEL_NETWORK=live：允许已配置的真实端点，仍受持久预算限制。

前台最多 2 次、每条消息后台最多 3 次，共享总授权。历史策略字段作为授权数据保留；旧 repair 开关不驱动新路径，已有授权组不重置或自动扩大。旧组未授权新阶段额度时需显式新授权。`write_pipeline` / `recall_pipeline` 的 legacy 值以及 `retrieval_mode`、`local_recall_enabled`、`local_recall_budget`、`is_a` 字段（即使 false/null/空值）在打开数据库前明确拒绝，不自动改写配置；`answer_contract.v1` 不支持，`memory_clarify` 不再提交旧暂存；旧数据经 LegacyRecordView 读取，不自动模型重建。切换前按 [手工备份步骤](docs/architecture/m1-cutover.md) 操作。

### Memory Inspector

```sh
./scripts/start-memory-inspector.sh
```

打开程序打印的本机授权链接。Inspector 只读已有数据，不执行记忆操作或模型调用；具体配置见 [Inspector 使用指南](inspector/README.md)。

## 开发与验证

```sh
cargo build --workspace --all-targets
PMR_MODEL_NETWORK=offline PMR_INSPECTOR_BINARY="$PWD/target/debug/pmr-inspector" cargo test --workspace --all-targets
python3 -m unittest discover -s tests/eval/scoring -v
```

普通测试使用合成数据、本机回环服务和已有录制。默认 E2E 为严格回放，只有显式 `EVAL_RECORD` 才可发送，发起前仍须用户另行授权。只格式化本轮改动的 Rust 文件。

默认测试还会从原始 AC2、两轮定点结果及复核台账重算历史 59/4，并断言失败题集合：`unresolved_conflict/conflict_q2`（冲突说明外仍给出唯一尺寸）、`lme_7a87bd0c/q`、`lme_a2f3aa27/q`、`lme_59524333/q`（提取、计划或判断的已知局限）。这不代表当前请求全量回放通过：当前严格回放为 235 次复用、59 个缺口（58 过期、1 缺录制）、0 发送，63 题均未形成完整当前回放结果，不计通过。回放报告与诊断写入 `target/eval-replay/`；补录和费用须另行批准。命令及历史结果来源见[评测说明](tests/eval/README-v3.md)。

仓库包含核心源码、当前文档和必要的合成回归样本。历史运行日志、冻结二进制、原始会话记录和本机部署资料留在开发者本地；部分历史验收脚本需要这些档案，无法仅凭公开源码重放原批次。当前改名也不会重写旧证据的名称或哈希。

设计与冻结契约见[目标设计](docs/architecture/target-design.md)和[契约](docs/architecture/contracts.md)；旧 architecture_v1 为历史参考。
