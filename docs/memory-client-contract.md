# personal_memory 客户端接入约定 v1

本约定规定客户端如何向现有 MCP 工具提交用户原话、读取安全答复、核对改写。工具名称和参数以连接时的 `tools/list` schema 为准；v1 不增加工具或输入字段；结果增加版本、问题/作用域回显与快照字段，保留既有字段位置。仅适用于本仓库 S5 开发版，正式 S4 尚未切换。此处的 `version: 1` 是**结果契约版本**，不是请求参数。服务端以固定 `owner` 运行，客户端不能在工具请求中自报或切换 owner。

## recall.v3 opt-in

默认仍使用下文 v1。服务器可设置 `recall_pipeline=recall_v3`，三个入口也可传 `recall_version=recall.v3` 或 `answer_contract.v1` 显式选择；每次只返回所选权威输出。新流水线的旧回执从同一次 R4/快照机械适配，复杂、未知或未提交事实保守返回不足，不重跑旧模型链路。新 MCP Recall 的冻结对象位于 `recall`，Prepare 为 `source/enrichment/recall` 三段，均另附独立 global_context，不含旧 answer_contract。宿主按新版 facts/computed/history/conflicts 及真实 evidence 组织答复，完整保留 polarity、限定语、grade/update 和 unknown/pending；skipped/failed 不授予本轮确定事实判断。

只读新版提交稳定 request_id；Prepare 可从 session/turn 自动派生。重试保持参数不变，省略 at 时服务端复用首次时钟。bootstrap 完全离线且只取全局规则。MCP 在线新版在独立操作连接上运行，模型等待不占服务全局锁；同 ID 同参并发共用一次执行，快照变化明确失败且不隐式重试。可用 offline=true、bootstrap 跳过召回模型，详见 [运行时接线](architecture/recall-runtime.md#⑤-配置cli-与客户端迁移)。离线 Prepare 需 off/deferred 写入模式。不要以静默旧版回落继续答复。

新版 memory_validate_answer 提交同一 request_id 与完整原问题/上下文/任务，仅校验同进程、同快照的保留结果，零模型调用。只机械批准简单事实的逐字 statement 和对应来源集合；任意改写、复杂限定或计算不据来源 ID 批准。回执为 version=recall.v3、request、snapshot、approved、approved_source_ids、reason；失败后没有旧 safe_answer，应按原权威新版结果保守组织答复，缺失/失效则说明无法核对。跨 CLI 进程无完整保留结果时也报错，不重调。

## 输入与调用顺序

1. 新聊天第一次调用 `memory_recall`，传 `{"query":"全局回答要求","bootstrap":true}`，离线读取全局要求。其 `not_applicable` 结果不核准任何具体记忆答案。之后，纯查询、明确不保存的消息及单独 `memory_remember` 成功后的检索使用普通 `memory_recall`；有获准保存的个人陈述时用 `memory_prepare`，不要为同一消息再重复 `memory_remember`。
2. `memory_prepare.user_turn.text` 是**当前整条 USER 原话**；`history` 只放必要的先前 USER 原话，按时间顺序。只有整条当前原话及所传 history 均获准原文留存时，首次调用才设 `capture_source=true`。不得把模型改写、引用、假设、秘密或用户要求不保存的内容当成已授权原文写入。保存结果中的 `source`、`enrichment` 和事实 `outcomes` 分开核对：原文已存不等于事实已提交。`memory_prepare` 的 `remember` 与 `recall` 也分别核对，不能由一方成功推断另一方成功。
3. 普通 `memory_recall.query` 和 `memory_validate_answer.query` 都传**完整、未改写的原问题**；`context` 只放绑定问题所必需的真实 USER 上下文，包括之后收到的澄清。保留否定、范围词和原标点；不得放助手文字、推测的历史，或把不同轮消息拼入 `query`。不能用较窄的关键词检索核准原问题的较宽结论。
4. `memory_prepare` 内部直接以当前 `user_turn.text` 作 `query`、`user_turn.history` 作 `context`，不会自动找回之前未答的原问题。若当前消息仅是“松石47。”这类澄清，应另用普通 `memory_recall`，把之前原问放在 `query`，本次 USER 原话放在 `context`。
5. 多客户端共享同一服务时，`session_id` 应包含客户端与聊天的稳定命名空间；同一聊天复用 session，每条新消息使用新的 `turn_id`。`memory_prepare`/`memory_remember` 的同次重试必须保持**所有参数完全相同**；`in_progress` 用原参数继续等待，不生成新 ID 或重存。`session_id`/`turn_id` 只属于写入工具，不给 `memory_recall` 添加这些不存在的输入。

普通 Recall 与 Validate 共用输入限制：`query` 非空、最多 24,000 UTF-8 字节；`context` 最多 8 条，每条非空、最多 2,000 个 Unicode 字符；可选 `task_id` 非空、最多 200 UTF-8 字节。整个工具参数 JSON 最多 65,536 字节。Validate 另要求非空且最多 4,000 UTF-8 字节的 `draft_answer`，`source_ids` 最多 20 个、每个非空且最多 200 UTF-8 字节。Recall 的 `limit` 默认为 20，可设 1～100；bootstrap 不带 `task_id`。不要为了过限而默默截断 USER 上下文，尤其不能删掉否定或澄清。Remember/Prepare 的 `history` 自身仍允许最多 100 条、每条最多 24,000 UTF-8 字节；超出召回限制时，保存可能成功而召回单独失败，应如实报告两项状态；能保留完整含义时选择必要 USER 消息重新查询，不能确定所指时询问用户，不重复保存已存消息。

## 结果契约与放行

先检查 MCP 的 `isError` 或传输错误：报错时只能说明无法核对，不能从旧结果补答案或来源。以下路径相对于 MCP `structuredContent`；库调用则直接以返回对象为起点。普通 `memory_recall` 的安全结果在 `answer_contract`；`memory_prepare` 召回成功时在 `recall.result.answer_contract`，召回运行失败时在 `recall.answer_contract`。若 prepare 的召回为 `not_requested` 或整项为 `in_progress`，没有可用于该问题的答复契约。`memory_validate_answer` 在根对象返回同一组字段，并保留 `approved`、`approved_source_ids`、`model_calls`。

| 两种结果共有字段 | 客户端含义 |
| --- | --- |
| `version: 1`、`query`、`context`、`task_id` | 核准所绑定的完整问题、真实 USER 上下文及可空任务范围。返回值必须与本次目标问题匹配。 |
| `status`、`safe_answer` | 保守答复状态及可原样发送的文本；启动/非问句的 `safe_answer=null` 不授权事实回答。 |
| `safe_source_ids`、`sources` | 允许展示的来源编号及其 `source_id`/`quote` 配对。只展示这两者相互对应的原文片段和编号，不从候选、历史或其他检索结果补引用，也不构造 URL。数组为空就不引用。 |
| `read_revision`、`source_revision` | 可空的读取快照版本；两者共同绑定本次证据。它们不是永久使用许可，已知存储变化后应重新检索或校验。 |

`memory_prepare` 的服务端缓存交付前检查已有的两种快照版本；任一版本已变化，就返回 `recall.status=not_requested`、`reason=state_changed_after_prepare` 并撤回旧答复；已完成的 `remember` 仍单独报告，不因召回撤回而改写保存结论。

当前状态全集如下；未知状态一律保守处理；只有 `source_linked_exact_identity` 和 `source_linked_fact` 允许按契约引用事实性来源。`question_required` 只来自 Validate 的合法非问题输入。

| `status` | 客户端动作 |
| --- | --- |
| `not_applicable` | 启动或非问题；`safe_answer=null`，不用它回答记忆事实。 |
| `clarification_required` | 原样追问 `safe_answer`，不猜主体或引来源。 |
| `comparison_evidence_incomplete` | 原样说明无法判断及缺少的关系，不作比较结论。 |
| `comparison_conclusion_not_verified` | 已有片段仍不足以核准比较结论；原样使用保守答复。 |
| `source_linked_exact_identity` | 仅核准 `safe_answer` 中的唯一身份值及列出的来源，不扩写成其他事实。 |
| `source_linked_fact` | 仅核准原文直接支持的单一属性值及列出的来源；逐字采用 `safe_answer`，不扩写。 |
| `answer_not_verified` | 虽找到相关来源，但答案未获核准；原样使用保守答复，不引用候选。 |
| `insufficient_evidence` | 证据不足；不编造、不把无答案说成事实上的否定。 |
| `unsupported_retrieval_mode` | 当前检索模式不能核准；不用记忆给出事实答案。 |
| `recall_failed` | 合法请求的召回运行失败；不用记忆给出事实答案。 |
| `question_required` | Validate 收到非完整问题；改传完整原问，不发送草稿。 |

`source_linked_fact` 在本地核对明确主体或单条确定的 USER 澄清、问题关系、已提交事实的值及精确原文链接；同一有效快照内有多个可用值时不核准，检索数量上限不能隐藏竞争答案。支持任意同名属性的直接问法，以及偏好、供电等方式、举办地点、活动时间这几类有限改写。主体和答案不按样本名称硬编码；原文疑问、假设、否定、多问及复杂推断不进入这条放行路径。原文中同时出现多个同类型实体、关系表达不在当前支持范围等情况仍可能保守拒答。该步骤不增加模型请求，也不代表任意自然语言都可核准。

对于原样采用 `safe_answer` 且仅展示 `sources` 的答复，无需再调用 Validate。改写、加句或扩大结论前，调用 `memory_validate_answer`，提交同一完整 `query`、`context`、`task_id`、完整 `draft_answer` 和拟引用的全部 `source_ids`。只有 `approved=true` 才发送该草稿，且只引用 `approved_source_ids` 对应的 `sources`；否则原样发送返回的 `safe_answer`，只引用 `safe_source_ids`。`approved=true` 也可能只核准一条安全追问或拒答，**不代表有事实答案**。最终文本若不送检，MCP 无法强制宿主不改写；当前核准范围对复杂推断仍保守。

非法入参由工具报错，不附结果契约；合法 Recall 的运行错误返回 `recall_failed` 契约。Validate 的合法早退仍返回 v1 公共字段，`sources=[]`、两种 revision 为 `null`，并有 `approved=false`、`approved_source_ids=[]`、`model_calls`。缺少 `version`、未知版本、问题或上下文不匹配，都不能自动授权；如需兼容旧服务，须单独验收旧返回的处理。`global_context` 只报告全局要求的读取状态，独立于保存、召回和最终答案；不能据“没有向 personal_memory 写入”声称聊天平台不保存对话。

**已知边界的处理决定（2026-09-26）：**用户明确将“第三方聊天模型的最终改写无法由 MCP 强制约束”记录为已知限制，暂缓处理。目前保留本约定及现有答案校验，不建设受控聊天入口或新增强制拦截机制；该边界不作为下一轮继续开发的任务，也不据此宣称宿主最终文本已得到强制保障。以后用户明确要求恢复此项时再安排。

## 三个调用例子

**N04 缺主体、随后澄清。** 首问：

```json
{"query":"先搁置的那种内存缓存服务叫什么？","context":[]}
```

期望 `clarification_required` 且无来源。用户随后只答“松石47。”时，继续查**原问**：

```json
{"query":"先搁置的那种内存缓存服务叫什么？","context":["松石47。"]}
```

若后端核准 E1，才可使用 `source_linked_exact_identity` 的 `safe_answer="Redis。"` 及其唯一 `sources` 编号和原文；若仍要求澄清或证据不足，就照该次契约处理。

**N05 比较。**

```json
{"query":"松石47的技术选择和部署限制是否一致？","context":[]}
```

E1 证明技术选择，E2 的当前单机状态不能证明部署限制；证据不足时应为 `comparison_evidence_incomplete`、来源为空、不得回答“一致”或“不一致”。拆成关键词或单项问题的结果也不能批准原比较结论。

**校验拟改写答案。**

```json
{"query":"松石47的技术选择和部署限制是否一致？","context":[],"draft_answer":"两者一致。","source_ids":[]}
```

此例应得到 `approved=false`，按返回的安全答复说明无法判断。若客户端拟引用来源，须传入拟引用的真实 `source_id`；客户端提供编号本身不构成核准。

## 本次开发验证（2026-09-26）

`cargo test --lib --test s5_answer_gate --test s2_persistence --test s4_local --test s5_assisted_local` 在 `PMR_MODEL_NETWORK=offline` 下通过 116 项检查。新增覆盖：跨工具问题/上下文/作用域一致、非法输入不产生契约、保存成功与召回参数失败分离、仅含来源的缓存答复在任一快照变化后撤回；已有 N04/N05/无答案与正例检查继续通过。首次沙箱运行有 6 项因本机回环监听权限失败，允许回环后通过；没有真实提供商请求或正式环境切换。

随后完成独立入口的 v1 接口验收（本地历史记录）：实际 stdio MCP 11 项检查、连接器 10 次调用分别通过；用户刷新后，8 个工具说明与参数定义已确认一致。后端模型 API 新增请求为 0，测试入口已停止，正式配置、启动包装器和数据库基线未变化。

本轮验证的是服务端约定与实际 MCP 接入行为，没有重新进行 ChatGPT 网页最终答复验收，也没有证明第三方宿主会遵守最终文字约束。

2026-09-27 增补：通用单事实开发验证（本地历史记录）已通过 120 项 Rust 回归及实际 MCP 的 8 次合成整理、20 问和 8 次草稿校验。新增状态 `source_linked_fact`；真实提供商请求 0，真实模型效果与新版网页行为尚未验收。

同日完成真实模型小批验收（本地历史记录）：同版本 8/8 入库、20/20 查询、8/8 草稿校验通过，实际 Ark 请求 29/44；包括错误草稿拒绝后的安全答案及来源复核。该结论仅覆盖固定开发样本，正式 S4 未切换，网页宿主与 S6 独立留出尚未验收。
