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
独立原文回执直接从 SQLite 读取，即使关闭诊断采集也能显示保存及整理状态；本地原文索引按记录的实际状态展示。Trace 按 AF375 删除纪元逐事件过滤；无法核验当前数据库时隐藏正文。未链接旧回执与普通日志仍保守隐藏；静态导出文件不会自动撤销。

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

`--dump` 与 `/api/live` 共用当前 DB 过滤；独立 `--export-trace` 也必须用 `--db/--owner` 核验当前删除状态，纪元前事件只导出白名单元数据；数据库不可读则拒绝导出，owner 不符的事件正文隐藏。已另存的离线文件不可能追溯撤销。


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

运行时新事件使用 trace_version=2，分离 meta / content_deps / body；Inspector 继续兼容 v1。③a 的 content_deps 一律 deps_complete=false，不声称正文依赖已证明，也不改变删除墓碑/导出拒绝策略。③b 的依赖收集见下节；③c 当前采用 AF373/AF375 删除纪元政策，见下节。

后台 claim 携带诊断专用上下文（内部 trace ID、source_id、generation、round、attempt_id），模型客户端显式克隆传递；请求/阶段绑定/预算不包含该上下文。旧诊断入口用进程内不透明 ID 关联，最多保留 4096 个映射，超限清空后旧关联可能分段，不能按时间猜测恢复。宿主 session/turn/request/operation 标识只可能在受保护正文，不进入安全 meta。


### AF371：依赖绑定与有界摘要

依赖仅服务诊断，不进入模型请求、阶段绑定、预算或持久业务状态。`content_deps` 包含 `deps_complete`、固定类别 `reasons`、`frozen_input_fingerprint`、`sources` 与 `memories`。来源条目绑定 owner、source_id、generation 和冻结来源内容/管理信息摘要；记录条目绑定 owner、memory_id 和 Memory + 完整记录投影视图摘要。使用现有 canonical JSON SHA-256 指纹。仅有 ID 不足以证明依赖，来源缺失、正文变更、旧记录投影不可证明、目录、查询、未绑定 history 等都保守标为不完整。依赖最多各 128 个来源/记录，超限不标完整；R3 摘要合计最多 128 个 ID，截断也标不完整。原模型输出、核验事件及 final 沿用输入上下文，不根据模型输出自行缩小依赖。

正文写入点分类（所有内容仍在受保护 `body`，本期不放宽 Inspector 删除墓碑门控）：

| 写入点 | 依赖与完整性 |
|---|---|
| W1/W3 `model_start/end/check`、W2 候选诊断与 `w2_summary` | 当前来源内容/代际与实际输入摘要；实体目录使其不完整，history 无 source 绑定时追加 `unbound_history`。W2 摘要只含计数和代码产生的拒收原因。 |
| W4 `model_start/end/check` | 按冻结 sources 与实际记录视图核对当前快照，绑定候选、basis、记录、history 链和关系成员的来源/版本；不匹配、缺失、legacy、未绑定 history/provenance 或超限均不完整。 |
| R1 `model_start/end/check` | 输入摘要；查询/context/实体目录没有来源证明，`query_context_and_entity_directory`。 |
| `retrieval(phase=selection)`、`r3_evidence` | 同一 R3 SQLite 读快照收集依赖；R2 记录真实枚举/词法/融合分支及可信范围/主体范围/提及/返回计数；R3 仅计数、限额 ID、完整性。current_message 无绑定、历史 provenance、超限等标不完整。 |
| R4 `model_start/end/check`、`r5_final` | 继承 R3 依赖，加实际输入摘要；查询/context 使其不完整。R5 仅最终状态、事实/计算/records/raw 数量。 |
| `w5_commit` | 仅 SQLite commit 成功后写，重放不新增事件；内部 source/generation/round/attempt，组 ID 的哈希、决定类型、产物 ID 与准确 committed revision。按同一事务状态绑定来源、目标和产物版本，截断/缺失不完整。不是 W4 计划。 |
| `transaction_committed/rollback` | 默认 revision、计数、变化标记（回滚有错误）；不完整。完整快照只允许独立调试开关。 |
| `validation(initial_snapshot)` | 仅完整快照调试开启时写；`snapshot_or_unbound_state`，永不声称完整。 |
| `run_start/run_end/rpc_response` | 混合宿主参数/响应含查询、上下文、标识及 global_context，无法从输出 ID 反推完整输入依赖；`unbound_host_payload`。不按结束时的重读快照冒充原读版本。 |
| `global_context` | 目录/全局投影未逐项证明，`entity_directory_or_unbound_state`。 |
| `source_receipt` | 含宿主标识与嵌套回执，`unbound_receipt`。 |
| `lexical_raw` | 旧词法原文结果未携带冻结版本证明，`unbound_retrieval`。 |
| 原有 `retrieval` 查询诊断 / `fact_index_refresh` | 有界诊断或错误，`unbound_diagnostic`；不推定正文安全。 |
| `model_transport_error` / preflight、`w4_order_blocked` | 沿用调用/claim 上下文；未证明的 claim 仍不完整。 |
| `PMR_OPENAI_TRACE_CONTENT` stderr 输出 | 独立的旧调试旁路，不是 trace v2 事件，也没有依赖证明；不能将其当作可按事件安全显示的正文。本期不改变该开关。 |
| 未列出的未来事件 / 旧入口 | 默认 `not_proven`，不得默认放行。 |

`PMR_TRACE_FULL_SNAPSHOTS=1` 与 `PMR_TRACE_DIR` 同时启用时才写完整 initial/before/after；默认关闭。完整快照可能包含全库及未链接 history，始终不完整，增加同步序列化、磁盘体积与敏感数据留存。默认关闭后，旧 trace 调试工具不能再依靠事务快照重建全库，应使用 Inspector 当前 DB 视图；已有 v1/v2 文件读取不变。

体积对照仅用合成夹具：100 条约 1KB 正文的来源，稳态完整 before+after JSON 314,982 B，默认 transaction 事件约 635 B（减少约 99.8%）。模型请求/响应仍保留，依赖条目会增加每次模型事件体积，不能把事务缩减率当作整份 trace 缩减率。没有读取生产 trace 做估算。


### AF375 / Inspector ③c：删除纪元与日志清理

当前政策替代③a/③b阶段的“有墓碑就整体隐藏 Trace”。成功提交来源删除后，运行时在 `PMR_TRACE_DIR` 写入 `deletion-epoch.json`（内部纪元 ID、写入时间、DB revision、各 owner 墓碑摘要、清理结果），关闭本进程旧文件并删除目录内全部 `trace-*.jsonl`。`trace.lock` 串行化跨进程写入/轮换；其他进程下一次写入检查纪元并换文件。纪元使用原子替换和同步落盘；失败通过固定 stderr 诊断及 `diagnostics::incomplete()` 报告，不改变 forget 的成功结果。物理清理部分失败会在纪元文件记录 `cleanup_failures`；残留旧文件仍不可展示正文。

Inspector 在解析正文和聚合之前统一过滤（live、dump、export-trace、页面下载与搜索使用同一已过滤 bundle）：
- 起点与写入均在纪元后的 v2 事件可展示完整正文，允许 `deps_complete=false`。写入时刻必须可确定，且写入纪元 ID 匹配；显式来源/记录依赖已删除、缺失或 owner 不符仍隐藏。混合 owner 文件保守隐藏全部事件正文。
- 纪元前、未知写入时刻、v1/未知版本只显示固定元数据（模型/阶段 token、用量、耗时），不保留宿主 session/turn/operation、自由错误或正文。旧 v2 没有写入时钟字段也按未知处理。
- 有历史墓碑但无纪元，纪元损坏/尚未完成，或墓碑摘要不一致时，所有现存 Trace 正文隐藏。这也覆盖删除已提交但纪元写入失败/进程崩溃的窗口。读取聚合后再次核对删除状态，变化时拒绝本次结果并提示刷新。
- W2/W5/R3/R5 使用明确摘要字段显示；R2 复用 retrieval 路径/计数诊断。未采集的内容不补造。普通 stderr 日志仍无事件纪元，存在墓碑时继续隐藏。

**已接受的边界**：宿主在新的 history/query/context 中重发已删除文字，可能出现在纪元后日志；这不是原来源恢复。AF376 按同一 call/attempt/run 的最早起点判定：跨纪元返回的在途输出隐藏；缺起点的孤立输出、核验或摘要也隐藏。运行时通过同一 TraceContext 携带内部起点时间，旧文件清理后仍可判定；未携带起点的事件仅能依赖同文件中关联的 model_start/run_start。删除会清空此前所有调用日志（包含无关来源/其他 owner 的日志）。备份、已导出或浏览器已取得的文件/内容不会自动撤销；文件 unlink 也不是介质安全擦除。需部署新运行时与 Inspector；旧进程不会执行新轮换协议。未设置 `PMR_TRACE_DIR` 时无法定位并清理历史目录，Inspector 以当前 DB 墓碑保守隐藏；重新配置目录并不自动追认历史文件。

不改变模型请求、指纹、预算或提交语义；不升 W/R、recall、DB 版本。完整事务快照调试开关默认仍关闭，执行删除的事务不记录删除前后全文快照。


### AF378：已有历史墓碑的首次部署

若旧版已删除来源却没有 `deletion-epoch.json`，先停止记忆服务，再显式执行一次：

```sh
PMR_TRACE_DIR=/path/to/private-traces pmr-mcp --init-trace-epoch --db /path/to/memory.sqlite3 --owner OWNER
```

诊断目录须已存在且权限为 0700；必须明确给出数据库和 owner，不读取运行时配置、不启动 worker、不调用模型。命令只读打开现有 DB（不迁移、不创建 owner、不写业务表），在 `trace.lock` 下读取当前 revision/墓碑摘要，清理所有 `trace-*.jsonl` 后原子发布与运行时相同的删除纪元。返回 JSON 的 `cleanup_status=partial` / `cleanup_failures` 表示仍有旧文件未清理，其正文仍被隐藏；需人工排查目录权限。数据库不可读、owner 不存在或目录权限不当时拒绝初始化。

已有有效纪元且该 owner 墓碑摘要一致时返回 `already_initialized`，不清理新日志、不改纪元。成功后重启服务与 Inspector：新起点的 v2 事件可显示，旧/缺失起点、显式已删依赖仍隐藏。此命令会清空该目录内所有 owner 的旧调用日志；备份和已导出文件不受影响。不要用此命令把运行中的旧调用追认为新调用。


### 默认事务摘要展示（AF381）

v2 `transaction_committed` 的 `committed_revision` 与 `revision_observability=persisted` 是提交证据；默认 `before/after=null` 不表示失败。页面显示读取/提交修订号、业务/来源变化标志与提交后总计数；完整快照未采集时不推断逐条 diff。逐条产物 ID 与决定类型来自独立 `w5_commit` 摘要，两组列表不假定逐项对应。W2、R3、R5 与新版 retrieval 按各自明确字段展示；检索 unavailable 或最终 insufficient 不等同于事件未完成。

零调用验证：`cargo test -p pmr-inspector --no-fail-fast`；`node inspector/tests/af381-summary-ui.js`（合成数据、离线页面函数测试）。


### 消息流水线（AF383）

运行详情默认展示 W0–W5、R1–R5 固定泳道，原始 run 详情仍可展开。只按内部 source_id、parent_trace_id 关联，整理轮次与 attempt 分列；W5 即使使用另一个 trace_id，也可通过来源/父级回到同一消息。召回子 attempt 从发起 run 显式接收 TraceContext，prepare 已保存来源时携带 source_id。后台来源关联为有界、仅内存诊断映射，W5 沿用同 attempt 的父级；进程重启或关联未采集时 parent 可空，页面仅尝试按明确来源 ID 关联，孤立运行单独保留，不按时间或证据中提及的来源猜归属。

W3 零候选/全拒收记录 w3_skipped；W4 即使未发模型请求也记录 w4_plan（组数、暂存数、实际模型调用标记、固定原因和决定计数）。计划与 W5 成功提交分开。复用计划不重新推断决定计数；缺失模型响应、摘要或阶段均如实标示，不补造历史。所有关联发生在原有服务端隐私过滤之后，删除纪元与依赖检查不放宽。

验证：`node inspector/tests/af383-pipeline-ui.js`；`cargo test --lib af383`；`cargo test -p pmr-inspector --no-fail-fast`。均使用合成数据或注入传输，不发送真实模型请求。
