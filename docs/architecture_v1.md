# Personal Memory Architecture V1

> 历史 V1 设计，旧执行器已退出。当前默认新路径与旧数据只读兼容见 [架构索引](architecture/README.md)，本文件不作为当前运行指南。

## 边界

系统管理属于某个用户的 Personal Memory，但被描述的 subject 可以是用户、孩子、
设备或其他 entity。Goal、plan 和 project state 不属于 Personal Memory。

V1 不引入 Knowledge Graph 或完整 ontology，只维护 Constraint target 实际需要的 IS-A
父类边，例如 `Latte → Coffee`、`MacBook → Laptop`。

## 当前端到端流程与职责

当前实现由大模型理解自然语言，输出结构化记忆或查询条件，再由 Rust 代码校验、合并、
筛选和持久化。保存与查询共用实体、谓词、Scope 和时间等字段体系，但分别回答
“用户陈述了什么”和“本次需要查什么”。数据库负责保存和加载用户状态，具体记忆匹配在内存中执行。

```text
保存：用户当前原话 + 必要历史 + 实体/谓词目录与时间上下文
        -> capture_source=true 时先独立提交获准原文与 S3 全文索引
        -> LLM 提取 CandidateMemory（strict JSON Schema）
        -> 本地解码校验；命中特定条件时最多一次 LLM 复核
        -> 实体解析 -> 待澄清，或 Admission -> 与已有记忆 Reconciliation
        -> SQLite 事务保存用户状态

召回：用户问题 + 实体/谓词目录与时间上下文
        -> LLM 生成 RuntimeContext -> 本地解码校验
        -> 从 SQLite 加载用户状态，处理到期记忆
        -> Rust 遍历记忆，执行字段过滤、范围覆盖和冲突检测
        -> 按规则排序并限制普通结果数量，分类返回并保存召回计数
```

日常入口 `memory_prepare` 先执行保存，再用当前原话执行召回，分别返回 `remember` 与
`recall` 的状态。召回失败不会撤销已完成的保存，召回成功也不代表保存成功；保存阶段
返回成功仍须核对候选结果，Pending、Rejected、Conflict 不等于正式记忆写入成功。
`memory_recall` 不把查询作为证据写入；`bootstrap=true` 是例外路径，通过本地固定条件
加载全局要求，不调用查询理解模型。

### 原话提取与证据

提取器接收完整的当前用户原话，以及调用方提供的必要历史用户消息。历史仅用于理解指代、
补充和纠正，只允许从当前消息提取新陈述，不重新保存未变化的历史信息。已有记忆全文
不会自动放入提取请求；旧记忆匹配在本地 Reconciliation 阶段执行。

模型将独立陈述拆为候选，选择主体、Fact/Preference/Constraint/Episode 类型、谓词、值、
方向、Scope、时间表达、留存层级、置信度和更新意图等字段。问题中的个人背景可以成为
候选，但计划不能冒充已发生事实，助手推断不能冒充用户陈述。具体准入与复核规则见后文。

运行时注入 owner、observed_at 和证据元数据。每条原子候选的 Evidence ID 为
`session_id:turn_id:候选序号`，原文引用保存当前消息全文，没有事实对应的字符区间定位；
一条消息拆为多个候选时，证据中可能重复包含同一段原文。消息回执和证据幂等共同防止
重试造成重复写入或重复强化。`memory_prepare` 遇到空提取或全部候选被拒绝时不保存该消息回执。

### 持久化与索引现状

SQLite 的主要业务状态仍按用户保存在快照表：

```sql
CREATE TABLE memory_profiles (owner_id TEXT PRIMARY KEY, state_json TEXT NOT NULL);
```

S3 新增 `source_indexes`、`source_segments` 与 `source_fts` 作为可重建投影；
`state_json` 包含记忆及版本、证据、实体、待澄清记录、消息处理回执、独立原文及源修订。读取时反序列化为
内存对象；写入事务先加锁、读取最新状态，再执行修改并提交完整快照，避免并发覆盖。
模型网络调用在写入事务之外完成。召回计数和到期状态的变动也会提交快照。

| 查找维度 | 当前实现 |
| --- | --- |
| 用户状态 | SQLite 的 `owner_id` 主键 |
| Memory ID | 内存 `BTreeMap<MemoryId, Memory>` |
| 主体、谓词、类型、对象、Scope、时间 | 遍历该用户记忆后按字段匹配，没有对应的数据库字段索引 |
| 类别与同义名称 | 有限别名表及显式注册的 IS-A 关系 |
| 原文全文检索 | S3 已实现 FTS5 内部只读搜索，限明确获准捕获的原文；尚未接入正式 MCP 召回 |
| BM25、向量检索 | 未实现；S3 使用授权范围内的确定性接收顺序排序 |
| 标签索引 | 未实现；`tags` 被保存，但当前召回不按标签筛选 |

结构化字段是匹配依据，不代表已经建立检索索引。查询模型输出的是 `RuntimeContext`，
不生成 SQL，也不直接扫描原文挑选答案。普通结果按 Scope 具体程度、类型、记录时间等
规则处理；当前有效的全局 Durable Constraint 走独立必返通道，不占普通结果数量限制。

### 模型依赖与可靠性边界

- **模型负责语义理解**：判断哪些内容值得提取、属于哪个主体和谓词、适用场景与更新意图，
  以及查询应选择哪些条件。提示词和 Schema 约束输出，但不能保证语义正确或信息完整。
- **代码负责执行规则**：字段和目录校验、时间换算、实体解析、准入、幂等、版本更新、
  留存、匹配、范围覆盖、冲突检测和事务持久化。代码依据模型提供的字段执行，
  无法普遍识别形式合法但语义错误的输出。
- **遗漏会传递到结果**：模型漏提事实，本地规则通常无法补回；模型选错查询主体、谓词或
  Scope，可能漏召回或召回不相关记录。条件触发的一次提取复核只能缓解部分问题。
  S4 已提供可选本地结构化＋原文路径；默认旧路径仍依赖模型规划，没有向量或开放式语义相关度评分。
- **规模边界**：完整 JSON 快照的读写与记忆遍历随用户数据量增长，部分候选关系比较为
  成对检查。当前结构适用于较小记忆库；规模增大时需要评估按记录存储和字段索引。

实现入口：[工具编排](../src/chat.rs)、[模型提取与查询理解](../src/openai.rs)、
[写入引擎](../src/engine.rs)、[规则策略](../src/policy.rs)、[召回](../src/retrieval.rs)、
[SQLite 持久化](../src/sqlite.rs)、[内存仓库](../src/store.rs)。

S3 的中文处理、内部 CLI、schema v3 迁移、删除／重建和验收边界见执行报告（本地历史记录）。

## 模型

- 每条 atomic memory 只有一个 `MemoryType`：Fact、Preference、Constraint、Episode。
- Preference 用于排序，Constraint 用于过滤，二者不合并成一个分数。
- `Entity.owner_id` 表示记忆库归属；`Memory.subject_id` 表示被描述对象。
- `PredicateDefinition.cardinality` 定义 semantic slot 是 Single 还是 Multiple。
- `SemanticRelationRegistry` 对 canonical `MemoryValue` 提供带环路保护的传递 IS-A 查询。
- `Scope` 是适用条件集合，不是自由文本场景名；context 不自动成为 scope。
- `RetentionTier` 表示 ShortTerm、LongTerm、Durable；Durable 仍可被更新或撤回。
- `MemoryStatus` 只描述已入库记录：Active、Superseded、Expired、Deleted。
- `Temporal` 分开保存 observed time、real-world validity 与 retention expiry；未知值留空。
- 可落到日历的相对时间保存为左闭右开 `TemporalRange`；事件相对或未知表达保存在
  `TemporalReference`，不会伪造绝对时间戳。
- `Evidence` 保存稳定 Evidence ID、session、turn 和原文；Candidate 与正式 Memory 都保留证据链。
- `extraction_confidence` 与用户表达的 `certainty` 是两个字段。
- retrieval count 与 reinforcement count 分开，避免系统自我强化。

## 写入

```text
Conversation -> Atomic Extraction -> CandidateMemory
             -> Entity Resolution -> Pending Staging / Clarification -> Admission
             -> Related Memory Search -> Reconciliation -> Store
```

默认 admission 接受已解析、来自用户、可跨 turn 使用的信息。低 extraction confidence、
assistant-only inference、hypothetical、控制消息、goal/plan 和无价值的一次性 episode
不会进入正式 store。

`ingest_or_stage` 对 unresolved subject 先调用 `EntityRepository`：唯一命中时继续正式
写入；NotFound 或 Ambiguous 时生成 `StagedCandidateId` 并保留完整 Candidate。后续
`clarify_staged` 仍只在唯一命中时进入 Admission；再次歧义会更新 pending record，
不会任选 Entity。每次澄清的原文证据会追加到 Candidate；成功处理后 staged ID 被消费，
不能重复入库。

`Evidence.id` 标识一次 atomic extraction，重试必须保持稳定；`session_id + turn_id` 标识
来源消息。Engine 先按 Evidence ID 幂等，再阻止同一 source turn 被误计为 Reinforce。
同 session 的新 turn、或另一个 session 的新 turn 才是独立强化证据。OpenAI adapter 在
本地注入这些字段和原文，不接受模型生成的证据元数据。

Reconciliation action：

- `New`: semantic slot 中没有同一信息。
- `Reinforce`: 同 scope、同 assertion 的新独立用户证据。
- `Refine`: 明确的同方向精化，旧版本保留 supersession link。
- `Supersede`: 明确的状态变化、纠错、偏好/约束变化或 scope 修正。
- `Retract`: 结束旧 memory，不制造反向偏好。
- `Ignore`: 明确无须写入或目标不存在。
- `Conflict`: 不能安全选择；不随机覆盖。

`ingest_with_trace` 返回 `IngestReport`。其中 `IngestTrace` 记录 admission 决定及原因、
owner 范围内看到的旧 Memory、active semantic-slot matches、最终 reconciliation decision
及选择该 action 的原因；证据重放也会明确记录 reconciliation 被跳过。

仅凭值不同无法区分 state change 与 correction，因此上游 extractor 必须通过
`CandidateIntent` 提供更新意图与 reason。

## 时间

时间表达式由 LLM 原样提取，最终换算由本地确定性 resolver 完成，并使用调用方提供的
UTC offset 计算日历边界。V1 约定“最近”为过去 30 天；“下个月”和“去年六月”解析为
完整自然月区间，来源为 Inferred；ISO 日期来源为 Explicit。“旅行结束后”保留事件结束
关系，来源为 Explicit；无法理解的原文以 Unknown 保存，二者都不会误当成当前时间。

## 留存

- 有 `expires_at` 的 Active ShortTerm / LongTerm 到期后变为 Expired，不物理删除。
- ShortTerm 收到两个新 source turn 的独立强化或明确长期记忆请求后升级为 LongTerm。
  后续候选声明的更高 tier 不直接升级已有 Memory，统一由 RetentionPolicy 判定。
- Durable 不自动 expire，但允许 supersede、retract 和 correction。

## 查询

```text
RuntimeContext
  -> Mandatory global durable constraints + candidate retrieval
  -> status / temporal / entity / predicate / target IS-A / scope filters
  -> specificity override -> conflict detection -> minimum sufficient set
```

Current retrieval 只使用 Active memory。Historical retrieval 可读取有效时间内的
StateChanged superseded record 与 expired archive，但永不把 Correction 当作历史事实。
没有 temporal filter 时，Current 按 `RuntimeContext.at` 做点查询；Historical 查询截至该
时间已成立的历史，不要求记录此刻仍然有效。具体日期/区间仍严格过滤，任意 Unknown
不会变成通配符；只有“以前/之前/曾经”等过去泛指可用于无范围历史召回。
如需历史中的具体时间点，调用方应显式提供包含该时间点的左闭右开区间。
互不相交有效期内的历史版本可并存返回，不按“同一时刻冲突”处理。
事件相对查询默认只匹配相同 `TemporalReference`；“搬到某地之前”例外使用当前 owner
与明确 Entity 的真实 StateChanged 版本链解析。重复搬到同地、缺少 Entity、没有可靠
链路或错误版本被 Correction 时不猜测锚点。StateChanged 新旧版本共用有效期切换边界：
有 valid_from 时优先使用（允许补述过去日期）；无日期、无 unresolved reference 时以
observed_at 为推定边界，来源标记 Supersession，而不是 Explicit。与旧版本开始时间矛盾
的切换返回 Conflict 且不改 Store。未知事件时间不推定为声明时刻，也不能作为搬家锚点。

“每天早上”等周期表达映射为 TimeOfDay Scope，不当作未知日期。今天/明天/昨天/后天、
去年等日历范围由现有时区解析器计算。Current 查询单独的“这次/本次”是当前情境而非
未知日期；具名任务仍由 task_id 表达，附带的具体日期不能丢弃。
写入侧仅在已有 Task Scope 时将单独的“这次/本次”按任务情境处理；缺少任务身份时
保留未解析时间，不把临时规则扩大为普通长期规则。
LLM 的 `expiry_expression` 独立表达留存截止日期，
与 `expires_at` 互斥；本地计算有限自然日边界。仅某日适用的 ShortTerm Constraint 默认
在该日结束淘汰，Episode 的事件日期不自动产生 TTL；未知事件结束时间不能生成截止时间。
当 query 同时给出 predicate 与 `RuntimeContext.targets` 时，普通 Constraint/Preference 只保留 value
与 target 相同、或 target IS-A value 的记录。因此禁止 Coffee 的规则可覆盖 Latte，禁止
Laptop 的规则可覆盖 MacBook；关系不会反向推理。未指定 target/predicate 时保守返回原有
目标相关类型，避免误删语言等非类别约束。Fact 属性继续按原规则检索；Episode 对明确
对象仅作身份匹配，不从访问父区域推断去过子地点。开放式目标为空时保留原集合。
关系层对 V1 小词表内的明确类别别名统一比较，例如“咖啡类饮料/咖啡/Coffee”与
“拿铁/拿铁咖啡/Latte”。只影响启用语义关系的匹配，不改写 Store 原始值，不做子串
匹配、不把“咖啡因”并入“咖啡”，也不将产品 Entity 转换成类别文本。目标身份比较共用
该有限别名表，无需先注册父类边；父子类别包含仍必须显式注册。
对于已通过状态、时间、实体、谓词和 Scope 过滤的 Preference，显式 IS-A 支持同主体、
同谓词的子目标例外覆盖；子 Scope 至少同样具体，并覆盖父偏好关联的全部查询目标。
局部子例外不删除多目标查询中其他目标的父偏好。覆盖只影响检索，Store 中两条记录均保留
Active，并以 OverriddenBy 记录原因；未指定 predicate/target 不执行该覆盖。
同 semantic slot 下更具体的适用规则覆盖更一般规则；同优先级且无法裁决的矛盾只报告
conflict，不返回任一方。

未限定 domain、但明确指定 predicate 的 Fact 查询可以跨领域读取事实，例如查询晨间
习惯时同时读取 food/programming 领域的习惯。仍执行原有 status、temporal、entity、
predicate 和全部 Scope 条件过滤；该规则不扩展到 Preference、Constraint 或未指定
predicate 的问题。

带孩子是家庭同行的更具体条件；Family 不反向推出 Children，也不包含 Friends。
相同条件数下 Children 可以覆盖 Family，但 Children 与 Country 等不同轴的相反规则
仍按原有冲突规则处理。重复声明 Children 和 Family 不提高 specificity。

OpenAI 两个入口的模型端 Scope 使用具名字段：companion_roles、companion_entity_ids、
geography、country、city、date、time_of_day、activities、task_id；geography 只能是
domestic、international 或 null。解码后仍转换为同一领域条件集合，并兼容既有 tagged
condition 数组的录制数据；不会把错误地点文本自动改成活动。具名字段减少结构分支歧义，
不保证模型内容正确。Task 身份由调用方通过 `OpenAiAdapterContext::with_task_id` 提供，
相关写入和查询共用同一 ID，不同旅行即使同城也使用不同 ID。模型只能选择该 ID 或 null，
一般写入规则不因调用方存在任务上下文而自动增加 Task Scope；查询中的 ID 则由运行时保留，
不因模型漏选而丢失，调用方应为无关查询清除该字段。有明确调用方 Task 的 Current 查询中，
单独“下一次/下次”指任务实例而非未知日期；没有 Task 时仍保留未解析时间，具体日期和
事件边界不清除。未提供 ID 时 schema 只允许 null；
提取 Scope、查询顶层 task_id 和查询 Scope 均在本地拒绝缺少调用方依据或不匹配的 ID，
包括既有 tagged condition 数组的录制数据。任务创建、识别和生命周期仍由调用方管理，
不按城市或活动生成身份。模型端 targets 仅表示明确评估的备选物品或类别；开放式推荐为空数组，
疑问词和旅行目的地不充当候选目标。物品/类别约束的 value 表示目标，polarity 表示方向，
上下文例外延续原规则的目标身份。这些是模型输入约定，未通过启发式改写输出兜底。
value 的模型 schema 使用按 kind 区分的四个分支，只有对应 text/integer/boolean/entity_id
字段非 null，其余字段必须为 null；本地仍再次校验内容和已知 Entity 身份。

他人近期态度的模糊观察按描述性 Fact 处理，保留 Low certainty 与独立的 extraction
confidence；用户表达不确定不等于 hypothetical 或 assistant inference，不因此丢弃原陈述。
明确稳定喜好仍是 Preference。撤销宽泛态度、改述具体意愿时分别撤销旧槽并
新增描述性槽。临时任务规则明确给出日历结束时间时，提取日期片段到 expiry_expression，
交由既有本地 resolver 计算截止；未知事件结束时间仍不猜测。这些属于提取输入约定，
不在 decoder 中根据原文重写模型类型或补造截止字段。
Country 在线生成从 249 个 ISO 3166-1 代码中选择，或为 null；两个适配器共同去除代码周围空白、
统一大小写并校验代码成员，拒绝 JX 等仅符合格式的无效代码，兼容已有日本名称别名为 JP。
其他旧名称保留，不提供完整地名别名库，也不根据国家名推断 City。目录快照及来源保存在验证记录中。
年龄、近似年龄与属性数值不充当 temporal_expression；另行明确的日期仍保留。查询明确
提及且唯一匹配已知 Entity 的主体必须进入 entity_ids，历史事件边界也不省略该主体。
活动条件保留所有明确提及的情境，例如出差住宿同时含 business_trip 和 hotel，不因已有
Task 或另一个活动而漏掉条件。
查询 schema 的字段说明直接声明相关主体与谓词应完整选择：宽泛态度包含具体意愿槽，
带已知孩子出行包含用户规则和孩子事实对应的主体。Entity ID 也使用既有目录 enum。
提取约定要求关系或事件保留具名对象，例如访问的地点和拥有的设备，
不能仅存 true 或把对象只放进 tags；真正的布尔属性仍可使用 Boolean。

两个在线适配器共用 Responses API 请求配置；当前源码指定推理接入点
`ep-20260920181641-8728q`、reasoning effort=`medium`，输出上限为 4096 token。
请求地址和凭据通过 `DEEPSEEK_BASE_URL`、`DEEPSEEK_API_KEY` 提供。
调用方可通过 `with_predicate_value_kinds` 为谓词声明允许的 `MemoryValueKind`：例如对象名称用
Text、预算用 Integer、设备身份用 Entity。目录全部有类型声明时，提取 value schema 仅允许
这些类型的并集；decoder 再按具体谓词检查子集，错误类型不能进入 Store。未声明的谓词兼容原行为，
原有 PredicateDefinition/cardinality 接口保留。示例和联网用例在请求前提供类型契约，不改写返回值。
`with_predicate_memory_types` 同样可声明谓词允许的 Fact/Preference/Constraint/Episode 类别，
schema 使用目录类型并集，decoder 再逐谓词校验；未配置的谓词保留全部类别。例如 visited
表示旅行经历时声明 Episode，避免被误分类为当前属性；分类错误进入既有一次复核，不直接改写类别。

提取为空、用户新述 Episode 被标为低跨轮价值且没有显式记忆请求、领域解码失败、
包含非泛指过去的 Unknown 时间、文本值未出现在原文/历史中，
或包含明确 Supersede/Refine 时，
提取器最多再做一次语义复核。更新复核区分取消与替换、纠错、范围缩窄和新增例外。
单独合法 Retract 不因其更新意图额外复核，以免正确撤回被再次生成改写；类型和时间检查仍适用。
文本缺少原文对应只是核查触发器，不是拒绝条件；复核允许忠实改写，但不得把具体对象扩大为类别。
价值核查区分自述旅行经历与低价值日常事件，不在本地强制设置 cross_turn_useful=true；
真正低价值的 Episode 仍由原 AdmissionPolicy 拒绝，用户显式要求记忆的事件不因价值标志额外复核。
复核使用原 turn/history、完整初稿和校验反馈，并使用变化的复核标识；不先写入初稿，不循环到非空。
复核仍不合法则报错；合法空结果必须给出 omission_reason（无个人断言、来源不合格、低价值经历、
目录不支持）。该理由仍是模型判定，不能证明任意自然语言输入的完整性。公开 decoder 兼容既有
没有 omission_reason 的录制 JSON。真正未知的时间可以继续保留，不通过放宽检索时间过滤来兜底。
HTTP/API 错误、拒绝和未完成响应不触发语义复核。

`PMR_OPENAI_TRACE=1` 可选记录响应 ID、模型、状态、推理配置、用量和耗时；
另设 `PMR_OPENAI_TRACE_CONTENT=1` 才记录原始结构化模型输出，可能包含用户内容，仅供需要内容的诊断。
这些日志不包含凭据或请求 URL。首次与复核结果有独立 schema 名，测试可以逐次核对。

HTTP 发送失败保留去掉 URL 后的底层错误链。客户端使用 reqwest 原生重试策略，在
is_connect() 为真，或 HTTPS 尚未取得响应头、is_request() 为真且不是超时时，
同一服务 host 内最多额外尝试两次，保留默认重试预算与 180 秒总请求超时。
这里仅执行 store=false 的无状态生成；重试可能重复 API 计算与计费，但不会重复写入本地记忆。
HTTP 错误和已取得响应头后的截断响应体不重放；不忽略 TLS EOF、不接受半份响应，证书验证照常执行。
持续网络故障仍会返回错误。本机 TLS 故障注入命令见稳定性验证报告。

每次 retrieval 都在 `RetrievalResult.trace` 中为 owner 范围内每条 Memory 输出一个最终
决定：Mandatory、Selected、Filtered 或 Conflict。Filtered 保留首个失败阶段（status、
temporal、entity、predicate、target、scope、override、limit）；Conflict 同时记录对方
Memory ID 和冲突原因。RealCase 失败信息会附带对应写入或检索轨迹。

## V1 已知限制

- 只支持显式注册的 IS-A；PART-OF、属性继承和自动 ontology discovery 不属于 V1。
- location 层级只做保守匹配，不猜测 City / Country / Domestic 的包含关系。
- extractor 和 runtime-context understanding 是接口，由 LLM adapter 实现。
- 当前 SQLite 持久化复用内存 repository，以用户 JSON 快照事务保证 replacement 原子性；
  尚未实现按记忆记录存储及字段索引，规模限制见“持久化与索引现状”。

## 本轮确定性边界补充

对已存 Episode 的明确纠错、细化或撤销不要求新的 cross_turn_useful；证据来源、主体与置信度检查
仍先执行，再由 Reconciliation 唯一匹配同槽位 Active 旧记录。缺失、错误和歧义目标不能变成 New；
新增低价值 Episode 仍拒绝。

“最近开始 / 最近刚开始 / recently started”复用 V1 的 30 天推定起点，并保持结束开放，
表示正在持续的状态而非已经结束的最近观察窗口；这是 Inferred 的粗略边界，不是精确开始日期。
普通“最近”的有限回看、明确日历日期、真正未知时间及事件边界仍按原规则处理。

泛指过去的识别允许“以前经常”等纯频率后缀，频率不提供日历边界。只有去掉一个频率词后仍为已知过去词的表达才成立；含其他日期或事件描述的 Unknown 仍不作为无范围历史查询的通配符。该判断由提取复核、查询解码及历史检索共用。

## S4 本地查询补充

`ChatConfig.local_recall_enabled=true` 时，普通 recall 在一个 SQLite 读快照中复用现有规则并检索原问题全文；默认 false 保持旧行为。新增 source_evidence 与 historical_or_conflicting，原文不能升级为当前事实或指令。返回 global_context 共用本次快照；local 不提交 retention 或检索计数更新。预算、有限自然语言支持及实际验证见 S4 报告（本地历史记录）。S5 的完整模式与整理开关尚未实现。
