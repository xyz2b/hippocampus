# hippocampus

A personal memory system for AI assistants: retain authorized original words,
track changes over time, and recall context with traceable sources through MCP.

面向 AI 助手的个人记忆系统：保留获准保存的原话，记录变化，通过 MCP 找回带出处的上下文。
记忆数据库存储在本机；启用在线模型时，请求内容会发送给所配置的提供者。

## Quick start / 最小上手

Install a stable Rust toolchain supporting edition 2024 and a native C compiler
(for bundled SQLite and cryptography). Python 3 and Node.js are used by tests,
not by the installed service.

```sh
git clone https://github.com/xyz2b/hippocampus.git
cd hippocampus
cargo build --locked --workspace --bins
./target/debug/hippo-mcp --help
```

Create `memory-config.json` outside the repository (or in your private configuration
directory), with automatic enrichment disabled:

```json
{"enrichment_mode":"off"}
```

Configure your MCP client with absolute paths. This example denies all model HTTP
calls and stores only original text that you explicitly authorize it to retain.
程序不自动读取 `.env`；配置通过进程环境或 MCP 客户端的 `env` 传入。

```json
{
  "mcpServers": {
    "hippocampus": {
      "command": "/absolute/path/to/hippocampus/target/debug/hippo-mcp",
      "args": [
        "--db", "/absolute/path/to/memory.sqlite3",
        "--owner", "local-user",
        "--config", "/absolute/path/to/memory-config.json"
      ],
      "env": {"HIPPOCAMPUS_MODEL_NETWORK":"deny"}
    }
  }
}
```

After MCP initialization and tool discovery, an authorized synthetic example is
`memory_remember` with these arguments:

```json
{
  "text": "I have a blue umbrella.",
  "session_id": "demo-chat",
  "turn_id": "1",
  "capture_source": true
}
```

Only send this when the entire text is approved for storage. Check
`source.status=stored` separately from `enrichment.status=not_requested`.
This configuration does not perform model-based extraction or semantic recall.
For a local original-text lookup, call `memory_recall` with
`{"query":"umbrella","request_id":"demo-lookup-1","offline":true}`;
returned raw text is context, not a verified current fact.

原文保存与结构化整理是两个独立结果。每条新消息使用新的 `turn_id`，同一聊天复用
`session_id`；重放保持参数不变。日常客户端的 bootstrap、prepare、召回与删除规则见
[客户端接入约定](docs/memory-client-contract.md)。服务内嵌的
[客户端指令](docs/personal-memory-instructions.md)目前以中文提供。

## Online models / 可选在线能力

Online extraction and recall require your own endpoint, key and model ID,
`HIPPOCAMPUS_MODEL_NETWORK=live`, and an explicitly authorized finite persistent budget.
The [configuration guide](docs/configuration.md) describes roles, network gates,
logging and an example budget. Inspect defaults with:

```sh
./target/debug/hippo-mcp --print-default-config
```

默认后台整理最多使用 W1/W3/W4 三次，前台召回最多使用 R1/R4 两次，消耗同一总授权。
失败不自动扩大预算。仅设置单次 `offline=true` 不会停止已经获准运行的后台任务。
仓库录制中的 `ep-*` 是账号相关的历史模型标签，不能复用为你的接入配置。

## Inspector

The independent, read-only Inspector displays stored sources, records and available
traces without running MCP or calling a model.

```sh
./scripts/start-memory-inspector.sh
```

Open the local authorization link printed by the program. See the
[Inspector guide](inspector/README.md) for explicit database, owner and log paths,
privacy filtering and exports. Exported files may contain content; keep them private.

## Current status and limits / 当前状态与局限

The project is under active development. Original-text storage, background
enrichment, evidence-based recall, source controls and Inspector are implemented.
Model extraction and judgments can fail; missing results are not proof of absence.
Legacy data is readable but is not automatically reinterpreted. Complete-load
latency targets have not been validated. Historical benchmark scores do not prove
current replay completeness or production quality.

原话保存成功不等于整理成功；系统不能强制第三方聊天客户端准确表达核验结果。
当前只支持 `recall.v11`。升级前阅读[备份与升级](docs/architecture/m1-cutover.md)。
更轻量的本地模型架构仍属于探索方向，不是本版提供的替代流水线。

The Rust package is `hippocampus`. Commands are now `hippo-mcp` and
`hippo-inspector`, environment variables use `HIPPOCAMPUS_*`, and the default data
directory is `~/.local/share/hippocampus/`. Old command names and `PMR_*` variables
are not supported. Both binaries reject known old variables at startup without
using or displaying their values; help and default-config output remain available.

To migrate an existing installation:

1. Stop all related processes and back up the data directory, including SQLite WAL files.
2. Only if the destination directory does not exist, move the entire directory:
   `mv "$HOME/.local/share/personal-memory-runtime" "$HOME/.local/share/hippocampus"`.
   If the destination already exists, resolve it manually first; do not overwrite it.
3. Update binary paths in MCP clients and launch scripts, replace all `PMR_*`
   environment names with `HIPPOCAMPUS_*`, and update explicit database, config,
   budget database and trace paths (including paths in `runtime.json`).
4. Rebuild/restart the service and Inspector; reopen the Inspector login link.

Without explicit `--db`, either binary refuses to start when the old default
database exists and the new default database is missing, even if the new directory
already exists. No automatic migration occurs. These steps do not authorize or
perform changes to an existing deployment.

## Development / 开发验证

Use an environment without provider credentials or `EVAL_RECORD`. Some tests use
loopback HTTP with synthetic responses. Test packages separately: workspace-wide
feature unification enables Inspector's `serde_json/preserve_order` in root tests
and changes canonical receipt serialization.

```sh
cargo build --locked --workspace --bins
HIPPOCAMPUS_MODEL_NETWORK=offline HIPPOCAMPUS_INSPECTOR_BINARY="$PWD/target/debug/hippo-inspector" cargo test --locked -p hippocampus
HIPPOCAMPUS_MODEL_NETWORK=offline cargo test --locked -p hippo-inspector
python3 -B -m unittest discover -s tests/eval/scoring -v
```

Historical archives are not all distributed. The [evaluation guide](tests/eval/README-v3.md)
distinguishes strict replay, accepted historical scores and missing recordings.
Do not set `EVAL_RECORD=0` as an offline switch: leave it unset.
See the [documentation index](docs/README.md) and [security policy](SECURITY.md).

## License / 许可证

Project-owned code is available under either [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE), at your option. Copyright (c) 2026 xyz2b.
Third-party data in `tests/eval` and related derived material retain their own
licenses; see [third-party notices](THIRD_PARTY_NOTICES.md).
