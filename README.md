# hippocampus

A personal memory system for AI assistants that preserves your original words, follows changes over time, and recalls relevant context with traceable sources.

面向 AI 助手的个人记忆系统：保留你的原话，记录信息随时间的变化，在需要时找回相关上下文，并提供可追溯的来源。

## 特点

- **保留原话**：原文与整理出的记忆分别保存、分别报告状态；整理失败时仍能保留获准保存的原文。
- **记住关系与变化**：区分人物、物品、事件等主体，处理偏好、约束、事实、经历，以及更正、冲突和有效期限。
- **按上下文找回**：结合原文搜索与结构化记忆，按主体、时间和适用条件召回相关内容。
- **来源可追溯**：记忆关联原文证据；提供来源核验与答案校验，在缺少依据或主体不明确时保守处理。
- **本地保存与控制**：记忆存于本地，可查看、澄清、删除；通过 MCP 接入 AI 助手。
- **过程可检查**：独立的只读 Memory Inspector 展示原文、记忆版本、检索结果和模型调用记录。

## 当前状态

项目仍在开发中。本地原文保存、检索与双通道召回已完成阶段验证；可选模型整理、辅助查询和最终答案校验仍在 S5 验收中。

模型可能漏抽、错误理解时间，独立复核也可能漏判。缺少证据时拒答不等于效果验收通过；MCP 工具也无法强制第三方聊天客户端原样采用核准答案。

项目原名 Personal Memory Runtime。Rust 包与库现名为 `hippocampus`；为兼容已有客户端，命令仍为 `pmr-mcp`、`pmr-inspector`，环境变量仍使用 `PMR_*`，默认数据目录仍为 `~/.local/share/personal-memory-runtime/`。

## 构建与运行

需要支持 Rust 2024 edition 的工具链。测试还使用 Python 3 和 Node.js。

```sh
git clone git@github.com:xyz2b/hippocampus.git
cd hippocampus
cargo build --workspace --bins
cargo run --example basic
./target/debug/pmr-mcp --help
```

`basic` 使用内存示例，展示保存、裁决和召回过程，不调用模型。

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

先创建只保存原文、本地检索的配置：

```json
{
  "retrieval_mode": "local",
  "enrichment_mode": "off"
}
```

此模式不调用模型；保存时仍须明确授权原文留存并传入 `capture_source=true`。原文保存成功与结构化整理成功是两个独立结果。

完整配置可通过 `pmr-mcp --print-default-config` 查看。调用规则见[客户端接入约定](docs/memory-client-contract.md)与[自动记忆规则](docs/personal-memory-instructions.md)。

### 可选模型能力

支持 `legacy`／`local`／`assisted` 查询，以及 `inline`／`off`／`deferred` 整理。模型请求须配置服务端身份、端点、凭据和持久调用预算；当前适配使用兼容 Responses 的协议及 `DEEPSEEK_BASE_URL`、`DEEPSEEK_API_KEY`。

- `PMR_MODEL_NETWORK=offline`：默认仅允许本机回环测试端点。
- `PMR_MODEL_NETWORK=deny`：禁止全部模型 HTTP 请求。
- `PMR_MODEL_NETWORK=live`：允许显式配置的真实模型请求。

预算通过配置中的 `model_budget` 管理，跨重启累计；失败或换入口不会重置额度。没有自动网络重试。

开发版可显式开启 `allow_extraction_repair=true`：可定位的提取错误最多进行一次完整重提取和独立复核。它与漏抽纠正 `allow_coverage_repair` 共用一次机会，启用任一项要求 `max_operation_calls=4`；正常两次、提取错误纠正三次、复核后纠正最多四次。二次校验不通过时保留原文并报告整理未完成。

### Memory Inspector

```sh
./scripts/start-memory-inspector.sh
```

打开程序打印的本机授权链接。Inspector 只读已有数据，不执行记忆操作或模型调用；具体配置见 [Inspector 使用指南](inspector/README.md)。

## 开发与验证

```sh
cargo fmt --all -- --check
cargo build --workspace --bins
PMR_MODEL_NETWORK=offline PMR_INSPECTOR_BINARY="$PWD/target/debug/pmr-inspector" cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
```

普通测试使用合成数据和本机回环服务；付费模型测试默认标记为 ignored。不要在未配置有限预算时启用真实模型测试。

仓库包含核心源码、当前文档和必要的合成回归样本。历史运行日志、冻结二进制、原始会话记录和本机部署资料留在开发者本地；部分历史验收脚本需要这些档案，无法仅凭公开源码重放原批次。当前改名也不会重写旧证据的名称或哈希。

领域模型与设计约定见[架构文档](docs/architecture_v1.md)。
