# hippocampus Memory Inspector

Inspector 是同一 Cargo workspace 内的独立 Rust package / 可执行程序 `pmr-inspector`。
它只读已有 SQLite、日志和 Trace，不启动 MCP、不调用模型、不执行记忆操作。
记忆服务与 Inspector 分别启动，关闭 Inspector 不会停止记忆服务。

## 启动与配置

从项目根目录启动：

```sh
./scripts/start-memory-inspector.sh --trace-dir "$HOME/.local/share/personal-memory-runtime/traces"
```

脚本每次执行增量 release 构建，再用 `exec` 启动服务；前端改动会重新嵌入。
首次构建需要 Rust 工具链，编译完成后的二进制可单独运行，无需 Python、Node 或源码目录：

```sh
cargo build -p pmr-inspector --release
./target/release/pmr-inspector --help
./target/release/pmr-inspector --port 8765 --owner local-user \
  --db "$HOME/.local/share/personal-memory-runtime/memory.sqlite3" \
  --trace-dir "$HOME/.local/share/personal-memory-runtime/traces"
```

| 参数 | 默认值与用途 |
| --- | --- |
| `--port PORT` | `8765`；仅监听 `127.0.0.1` |
| `--db PATH` | `~/.local/share/personal-memory-runtime/memory.sqlite3`；只读已有数据库 |
| `--owner ID` | `local-user`；选取该 owner 的持久快照与 Trace |
| `--log PATH` | 可重复；未指定时读取 `~/Library/Application Support/tunnel-client/logs/` 下的 `personal-memory.log` 与 `launchd.stderr.log` |
| `--trace-dir PATH` | 默认不启用；读取目录中的 `trace-*.jsonl`，应与记忆运行时的 `PMR_TRACE_DIR` 一致 |
| `--dump` | 将当前 SQLite、日志及所选 Trace 整理为 JSON，写入标准输出，不启动 HTTP |
| `--export-trace PATH` | 将一个运行时 Trace JSONL 转换为 TraceBundle JSON，写入标准输出，不启动 HTTP |

打开终端输出的授权链接；授权后地址回到 `/`。每次启动生成新凭据，重启后重新打开新链接。
访问凭据仅用于本机页面，不是模型 API Key。按 `Ctrl-C` 停止；同一端口已有服务时先停止旧服务，或指定其他端口。

页面每 2 秒获取最新记录；暂停刷新、切换历史记录或导入 JSON 的行为与原页面相同。
独立原文回执直接从 SQLite 读取，即使关闭诊断采集也能显示保存及整理状态；本地原文索引按记录的实际状态展示。存在 source 删除墓碑或无法核验当前数据库时，会保守隐藏旧 Trace、保存回执及日志正文，并明确显示原因；静态导出文件不会自动撤销。

数据库以只读模式访问并启用 `query_only`，保留 WAL 可见性；日志只读取末尾 64 KiB / 100 行。
未采集、未完成、不可读等状态直接展示，不能用当前快照补造历史执行过程。

离线查看当前数据或转换运行时 Trace 使用同一个 Rust 程序：

```sh
./target/release/pmr-inspector --dump --trace-dir "$HOME/.local/share/personal-memory-runtime/traces"
./target/release/pmr-inspector --export-trace /absolute/path/trace-ID.jsonl
```

`--export-trace` 接受运行时 JSONL；早期历史捕获的 `index.json` 不是该格式。接收调用与显式 CLI 整理是两条运行记录，Inspector 按同一个 `source_id` 关联；CLI 结果不是 MCP 响应，状态和尝试次数取自各自记录时刻。Trace 目录须预先创建并设为私有（Unix 权限 `0700`）。
已经导出的历史失败记录仍随程序内置，可以直接在页面查看。

## 目录与开发

- `inspector/src/main.rs`：独立 Rust HTTP 服务入口。
- `inspector/web/index.html`：页面的唯一源码，沿用原 HTML/CSS/JS，无外部前端依赖。
- `inspector/web/recorded-4096-failure.json`：真实历史失败记录的唯一静态包，内容未改写。
- `docs/memory-inspector-mvp/trace-bundle.schema.json`：导出数据格式。
- `tests/inspector_support.py`：测试共享驱动，直接启动 Rust 程序、调用其接口并检查结果，不包含旧 Python 后端实现。

Rust 将页面和历史包在编译时嵌入二进制。修改 HTML 后必须重新构建并重启，浏览器刷新后生效。
使用上述启动脚本即可完成构建与启动；直接执行旧二进制不会自动读取刚修改的 HTML。

## 回归检查与历史验收

仓库随源码保留必要合成样本；完整历史验收档案保存在开发者本地。
当前回归直接测试 Rust 实现；Python 仅用于隔离测试数据及驱动 Rust CLI/HTTP，
不再提供另一份 Inspector 服务或数据转换逻辑：

```sh
cargo build -p pmr-inspector --release
cargo build --bin pmr-mcp
cargo test -p pmr-inspector
node tests/s1_inspector.js
python3 tests/memory_inspector_live.py
python3 tests/s0_offline.py
cargo test --test mcp_stdio
cargo test --test mcp_background --test mcp_visibility
```

测试默认使用 `target/release/pmr-inspector`，可用 `PMR_INSPECTOR_BINARY` 指定其他已构建的二进制。
S0 只验证历史 Inspector 包；MCP 测试使用 debug `pmr-mcp` 和新默认路径，本机合成桩不调用真实模型。旧 s1_trace/mcp_prepare/S5 单次提取运行器已删除；接收、后台、召回与删除保护由新 MCP 测试覆盖。

历史报告、原型资料和 `.ai-coding-flow/` 早期验收资料中的 Python 命令描述当时的实现，
不再是可执行的当前入口；保留这些记录不表示旧实现仍在项目中。

## AF359：当前 DB 视图

来源详情把 **W0 原文接收** 与 **结构化整理** 分开：显示状态、error、轮次、operation_id，以及当前留存的 W2 核验、W3 verdict/aspects/coverage、准入原因、候选最终回执和计划。缺字段是未记录，不能解释为通过；这些检查点不是完整执行历史。

记录详情通过根库 `w4::record_view` 纯投影区分 CurrentRecordView 与 LegacyRecordView，展示完整候选（值、极性、限定语、时间）、grade、update 与原因、历史记录 ID、来源依据和相关关系。这里只展示已存陈述，不生成“当前事实”判断；Legacy 缺口与未知 update 保持未知。

读取仍使用 SQLite `READ_ONLY` + `PRAGMA query_only=ON` + 一致性读事务；不调用运行时 store 打开、迁移、维护、模型或嵌入。根库依赖只用于内存快照反序列化及纯投影/回执函数。

AF360 当前 DB 隐私按项过滤：未删除来源的自身原文可见；`request.history` 未链接 source_id，始终不导出并显示原因。W1/W2/W3 只展示可核对为当前消息精确片段的块，未核验原始输出、绑定快照不直接导出。记录先检查原始 evidence（不能依赖投影丢弃缺失引用后的子集），再检查前后版本、生产 history、关系成员/依据及 owner；依赖不存在、已删除或不可确认时隐藏该记录/块并标注原因。W4 只导出核对依赖后的决策摘要，不导出 seen/绑定快照。无关墓碑不再隐藏安全记录与原文。旧 trace、日志与无法关联的旧回执仍沿用保守隐藏；数据库不可读时不展示历史正文。

来源页与记录页显示隐私过滤/读取错误提示。Trace 超过 16 MiB 显示 `trace_size_limit`；新版 retrieval 的覆盖/错误诊断与旧 result/runtime 均可读取。未采集与无法确认归属会明确提示，不解释为无调用；仍不做费用估算或大文件增量解析。

零调用验证：`cargo test -p pmr-inspector --no-fail-fast`（仅生成临时虚构 SQLite/trace 夹具）。

### AF361：删除回执最小化

已删除来源不再进入普通 runs/sources 模板，仅通过 `deleted_sources` 输出 `status=deleted`、内部 `source_id`、`received_at`。不输出 session/turn/message、operation、工具参数、整理内容或宿主文本。时间按用户默认取舍保留，用于核对接收时间；它仍可能暴露活动时点。来源/记录页单独列出这份最小回执，页面下载使用同一服务端已过滤 bundle。

`--dump` 与 `/api/live` 共用当前 DB 过滤；独立 `--export-trace` 也必须用 `--db/--owner` 核验当前删除状态，存在墓碑、数据库不可读或 owner 不匹配时拒绝导出历史正文。已另存的离线文件不可能追溯撤销。


## AF363：阶段调用与来源时间线

来源卡片按显式 source_id（含请求 source_snapshot 绑定）或当前 DB operation_id 关联多轮整理和相关召回，不按时间相邻猜测。轮次只展示 DB 明确提供的值。无 run_start、缺 model_start / model_end 或 run_end 的事件保留并标为不完整；未知事件独立列出，不丢弃其他事件。无 owner 的后台文件只有在全部运行的显式来源 ID 均属于当前可见来源，或在没有来源 ID 时可绑定当前来源 operation_id 时才加载；否则显示 trace_owner_unconfirmed。独立导出的 owner 校验仍保持严格。

调用展示实际请求的 schema 名称、模型、推理强度、用量（输入/缓存/输出/推理）、耗时、local_outcome、错误与传输错误类别。原始响应/JSON raw、model_check 的 decode/validate、明确记录的 final、事务提交是不同证据。当前运行时通常只记录核验结论，不记录 final 正文，此时显示“未采集”，不能把 raw 视作核验通过或提交。无 call_id 的核验只关联唯一同阶段调用；无法确认则保留在事件列表。

召回详情展示 R1 raw 计划、R2 新旧检索诊断、实际 R4 请求中的 R3 证据包、R4 raw 判决/facts，以及最终服务响应（含 statement_snapshot）。来源时间线中的召回关联表示引用过该来源，不表示对其完成整理。

隐私策略不变：存在删除墓碑时整体隐藏历史 Trace；--export-trace 仍拒绝导出。16 MiB 上限保持。全部测试使用合成夹具，无模型调用、无生产数据。


### AF364：事件级兼容与隔离

当前 recall 的 facts 是判决事实，不是旧 Memory；仅明确旧分栏结果进入旧 Memory 适配。快照 receipts 的 JSON 字符串键作为不透明键保留，不按点号或协议版本拆解；其他类型回执不冒充 W5 outcomes。model_end 缺少 response/accounting/parsed_result 等字段时记录 missing_fields；有 accounting 用量但无 response 用量时注明 recorded_accounting，不补造缓存/推理 token。

JSONL 语法/信封错误记在 meta.parse_errors（无法解析 ID 时用行号）；字段错误记在 run.parse_errors，包含 event_id、kind、字段路径及固定错误类别，不包含失败正文，动态来源/回执键以 * 表示。坏事件的派生变更回滚并标为不完整，其前后正常事件仍加载；live 与导出使用同一解析器。混合 owner、删除墓碑等文件级隐私拒绝仍保持。前端显示隔离提示，缺失字段不解释为零调用或业务成功。

合成夹具 tests/fixtures/af364-event-shapes.jsonl 依照获准 shape 清单的 10 种 kind 手工填入虚构内容，包含当前嵌套 recall、opaque receipt keys、accounting 和 response usage；不包含真实记录内容，也不依赖生产文件。路径清单是多事件字段并集，不能证明任何字段每条事件均存在；因此另测缺失/null/错误类型和坏行。


### AF368：trace v2 协议准备

运行时新事件使用 trace_version=2，分离 meta / content_deps / body；Inspector 继续兼容 v1。③a 的 content_deps 一律 deps_complete=false，不声称正文依赖已证明，也不改变删除墓碑/导出拒绝策略。③b 收集依赖、③c 按事件过滤尚未实施。

后台 claim 携带诊断专用上下文（内部 trace ID、source_id、generation、round、attempt_id），模型客户端显式克隆传递；请求/阶段绑定/预算不包含该上下文。旧诊断入口用进程内不透明 ID 关联，最多保留 4096 个映射，超限清空后旧关联可能分段，不能按时间猜测恢复。宿主 session/turn/request/operation 标识只可能在受保护正文，不进入安全 meta。
