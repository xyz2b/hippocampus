# 第一阶段契约（冻结；w1 v5，w3 v6，w4 v5，r4 v7，recall v11，r1 v2）

> M1 收尾状态（2026-10-06）：默认新路径，旧执行器已删除、旧数据读取保留；历史合并 59/4，当前严格回放仍有 59 个缺口。实现、备份、局限与补录授权见 [架构索引](README.md)。M1.5 按 AF41 批准增加计划记忆并升版；主体确认/重建、历史补录与 nullable said_on 分别在第③④步实现。

本文是 [target-design.md](target-design.md) 第 5 节第 1 步“冻结契约和不变量”的产出：各模型环节的输入输出格式、代码核验规则、状态与失败码、预算，以及可测试的不变量。实现各环节时按本文生成对应的 Rust 类型和 strict JSON Schema；本文于 2026-09-30 冻结，各契约统一编号为 v1：`w1.v1`、`w3.v1`、`w4.v1`、`r1.v1`、`r4.v1` 及宿主输出 `recall.v1`。冻结前的多轮草稿版本号不再使用。冻结后任何改动须升受影响契约的版本号；冻结时这些为目标契约；当前实现与验收边界见运行时文档，不能将未验收性能目标视为已完成。同日按此规则将 `r4`、`recall` 依次升为 v2（elapsed 的提问时刻端点改由代码提供）和 v3（elapsed 的取整口径与精确天数由代码给出），见第 8 节。2026-10-02 `w4` 升为 v2（冲突范围由 W4 给出、输入记录带结束时间），见 2.4、3.2 与第 8 节；2026-10-05 `w4` 升为 v3（授权范围内实际穷举可报告完整，AF1），该次 W1/W3/R1 仍为 v1。2026-10-06 按 AF41 将 W1/W3 升为 v2、W4/R4/recall 升为 v4，R1 保持 v1；计划语义与后续来源控制条款见下文。2026-10-07 按 AF90 将 W1/W3 升为 v3、R1 升为 v2、R4/recall 升为 v6，批准无年份日期与 W3 核验口径；W4 v4、DB 10 不变。

形式约定：

- 以 JSON 形状和字段表描述。`?` 表示可为 `null`；`[]` 表示数组；`a | b` 表示枚举。形状中的 `Candidate`、`RecordView` 等类型名引用本文定义，不是模型实际返回的字符串；生成 schema 时使用类型引用。所有字段均须显式出现，可空字段用 `null`，每种联合类型只允许该分支的字段。
- 所有模型输出都用 strict JSON Schema；字符串字段在 schema 中使用锚定的非空 pattern（`^[\s\S]*\S[\s\S]*$`，服务商会整串匹配 pattern，且忽略单独的 `minLength`）。
- 每个契约带版本号，例如 `w1.v1`。版本号进入录制指纹；版本变化即视为模型请求变化。
- `owner`、可信任务、原文顺序、请求 ID、快照及处理版本由运行时绑定，模型不能生成或扩大授权。模型输入外的记录 ID 和来源不能被引用。
- `MemoryId` 沿用现有 u64 编号；`LocalDate` 为合法的 YYYY-MM-DD；生产边界的 `Timestamp` 为 UTC Unix 秒（i64），接收日按用户时区取得。`SupersedeReason` 沿用现有 `StateChanged | Correction | PreferenceChanged | ConstraintChanged | Refinement | ScopeRefined | UserRetracted`，不混同于记录生命周期。

## 1. 共用类型

### 1.1 原文片段 `Quote`

```json
{ "text": "只在户外写生时这样" }
```

- `text` 必须逐字出现在指定原文中（当前消息，或允许时的历史消息）。比对时忽略空白差异和引号排版（`’` 与 `'`、重复的 `''`）；存储的永远是原文中的写法。
- 比对成功后必须定位原文中的连续片段并还原其写法，不能把不相邻的句子拼成一个 Quote。英文词匹配不能把 `know` 中的字符当作独立的 `now`。
- 代码只核验“逐字存在”，不核验语义是否正确（语义由 W3、R4 判断）。

跨来源引用用 `EvidenceRef = { "source": SourceRef, "quote": Quote }`。`SourceRef` 为 `{ "kind": "stored", "source_id": "s-102" }` 或 `{ "kind": "current" }`：后者指本次请求的当前消息，允许未持久保存，不伪造来源 ID；前者只能引用输入中提供且未删除的来源。W1 的 Quote 默认绑定当前原文，W4/R4 跨来源依据必须使用 EvidenceRef。

W4 的持久决定、关系及解决依据只使用 stored 来源；只读 current 可参与 R4 本轮判断，不因被引用而获得保存授权。旧记录缺少本版所需的原文或限定语时显式标为整理缺口，不补造 Quote 或默认无限定。

### 1.2 时间解释 `TimeInterp`

模型给出原文与含义，代码负责日历合法性、时区和算术。

无明确年份且不能由明确上下文定年的月日或月份，由模型以该表达所属来源的可信 said_on 判断：明确指向未来计划、安排的事件取其后最近一次，明确指向过去经历的事件取此前最近一次。预订按事件方向判断，不按预订动作是否完成判断。方向不明、需要无依据的跨年推定或 said_on 未知时，保留原话并令 interp 为 unknown。保持原文精度与时间角色，不推造具体日。W1/W3、R1/R4 分别解释和核验；代码不补年、不判方向，W4 不重新定年。

```json
{
  "role": "event | validity | retention",
  "quote": { "text": "去年五月" },
  "interp": { "kind": "month", "year_offset": -1, "month": 5 },
  "anchor": "said_on"
}
```

`interp.kind` 的取值：

| kind | 字段 | 例子 |
| --- | --- | --- |
| `date` | `date`（YYYY-MM-DD） | “2027年4月15日” |
| `range` | `start`、`end_inclusive`（均为 YYYY-MM-DD） | “4月1日至4月14日（含首尾两日）” |
| `relative` | `unit`（`day / week / month / year`）、`offset`（整数） | “昨天” = day −1；“三周前” = week −3 |
| `month` | `year_offset`、`month` | “去年五月” |
| `anchored` | `anchor_quote`（Quote）、`unit`、`offset` | “毕业后的第二年” |
| `recurring` | `quote` 的原文即为规律（“每天早上”），不给日期 | “每周三” |
| `unknown` | 无 | “那阵子”“以前” |

- `role`：`event` 为事情发生或计划执行的时间；`validity` 仅为明确表达的事实/计划适用区间，不能仅凭日期推断；`retention` 为用户说的保存期限（第一阶段只标记，不执行，见 2.1、3.5）。三者分别解释，不把事件时间当作保存期限。
- `anchor`：相对时间的起点固定为可信 `said_on`（普通来源为原文接收日，用户本地时区）；后台不用执行日。历史补录采用原始陈述时间，未知时 said_on=null、相对时间保持 unknown，不用补录日、父来源或请求时刻代填（第④步实现）；绝对日期仍可独立核验。
- `range` 的模型输出固定为包含首尾的 `start/end_inclusive`，不接受在 `end_inclusive` 中填写次日。代码按用户本地自然日只转换一次为左闭右开；“4 月 1 日至 14 日”的内部结束端点为 15 日零时。日历无效、倒置或溢出时不能修成另一日期。精度保留到原文给出的粒度，不从月份推造具体日。
- 无法换算（`unknown`、`anchored` 的锚点未知）时保留原文和 `unknown` 状态，**不丢弃**。计划日期经过不证明执行，不自动删除或取消计划；明确 validity 到期只影响适用性，历史计划仍保留。有效时间未知不回填为接收时间；retention 的保存期限语义不变。
- 换算结果由代码另附 `TimeResolution = { "status": "resolved | unknown", "start": Timestamp?, "end_exclusive": Timestamp?, "precision": "day | week | month | year | unknown" }`；未知时端点为 `null`。不覆盖原始 TimeInterp；周期条件在第一阶段保留原文，不冒充一个连续日期区间。

### 1.3 限定语

| 字段 | 形式 | 说明 |
| --- | --- | --- |
| `polarity` | `affirm \| negate \| unknown` | 无法确定方向时为 unknown，保留完整原句，不猜成肯定 |
| `uncertainty` | `TextQualifier?` | “可能”“我觉得”“大概”等；`null` 表示原文没有不确定表达 |
| `conditions` | `[TextQualifier]` | 适用条件原文，如“画速写时”“和孩子一起时”；`[]` 表示原文没有条件 |
| `times` | `[TimeInterp]` | 与该断言相关的时间 |
| `strength` | `weak \| moderate \| strong \| unknown` 或 `null` | 仅偏好使用；null 为没有强弱表达，unknown 为有表达但不能归类；强弱原话保留在完整原句中 |

`TextQualifier = { "quote": Quote, "resolved": boolean }`，`resolved` 属于限定语，不是 Quote 的属性：模型声明是否理解该条件或不确定表达，W3 核验其含义，代码不据此宣称语义已证明。“原文没有”与“没能解析”必须区分：有表达但无法理解时保留 Quote 并令 `resolved = false`，时间则保留 `interp.kind = unknown`；不能省略成 `null` 或 `[]`。这些限定语从候选到记录、R4 和宿主输出持续保留。

主体或极性未解析的候选不成为确定当前记录；偏好强度 unknown 也不能转换成 None 后按“同一断言”强化。现有存储无法表达的限定语先保留于候选及原文通道，不能提交为无限定事实。

### 1.4 主体 `Subject`

```json
{ "entity_id": "self" }
```

或：

```json
{ "unresolved": { "text": "他" } }
```

- `entity_id` 必须在当前实体目录中；`self` 表示用户本人。
- 目录中找不到，或原文指代不明时，用 `unresolved` 保存原文指代，进入待澄清。不得因目录中只有一个候选就自动绑定。亲属称谓指向相关人物而非 self；省略主体必须有明确当前原文或必要 USER 历史指代依据。属性不是人物，事件回指不是实体别名，不自动建立受限实体。
- 主体确认是独立控制记录（第③步实现）：绑定来源、代际、候选身份、已登记实体及获准 USER 依据。代码核对身份与片段，W3 核验绑定含义；确认不改写原文/事实值，也不凭确认免除核验。

### 1.5 值 `Value` 与数值 `Quantity`

| Value.kind | 必需字段 | 约束 |
| --- | --- | --- |
| `text` | `text: string`、`quote: Quote` | text 保存原文写法；通用 fact 为足以定位该单条断言的连续原文片段（不要求最短） |
| `integer` | `integer: i64`、`quote: Quote` | 数字含义由模型提出，代码检查表示、范围及原文依据，W3 核验数值归属 |
| `boolean` | `boolean: bool`、`quote: Quote` | 肯定或否定的哪个属性由 W3 核验，不仅检查原文有无“是/否” |
| `entity` | `entity_id: string`、`quote: Quote` | ID 在实体目录内；原文需支持该对象，不靠目录存在即认定 |

通用 `relation=fact` 的值必须为 Text，`value.text == value.quote.text`，且该引用位于完整 `sentence` 内。代码只核对类型、连续引用与包含关系；片段是否足以定位单条断言、否定及限定语是否准确由 W3 核验。专用关系的值规则不变；旧整句 fact 值仍是合法连续片段，读取时不改写。

计算用 `Quantity = { "amount": string, "unit": string? }`。amount 是有限十进制文本（如 `"32"`、`"-10"`、`"1.5"`），代码核验合法表示、单位换算、精度和溢出后计算；无单位计数用 null。不把格式化、舍入或单位换算交给模型；不能可靠归一的量返回不足。Value 与 Quantity 都保留原文依据，不从 quoted text 中临时新增自然语言推断规则。

### 1.6 更新判定状态 `UpdateState`

`UpdateState = { "status": "judged | pending", "reasons": ["update_scope_incomplete | update_target_unknown | update_relation_unclear"] }`，由代码根据 W4 决定及实际查找范围生成。judged 的 reasons 为空；pending 至少一个原因，无重复。范围不完整必含 update_scope_incomplete，即使对明确目标的动作已执行，也不代表其余关系已判断。judged 表示更新判定完成，不表示真实冲突已经解决（后者由 RelationLink 表达）。

事实是否提交由候选的提交状态表达，更新关系是否待判定由 UpdateState 表达；允许候选 status=committed 且 update.status=pending。UpdateState 及来源、候选索引、已提交记忆 ID 一起保存在现有决定回执，并随有效整理记录持久保留，不新增存储框架。普通召回、整理完成或重放不能清掉 pending；只有显式重判覆盖了对应缺口后才更新该状态。

## 2. 写入侧

### 2.1 W1 提取 `w1.v5`

提取范围：用户本人及其相关人、物、活动、真实偏好、习惯、选择、计划、经历，以及本人职责、职业和工作状态；不要求长期不变。外部主题或方案的评价、反驳、因果推演、备选建议不提取，“认为/不同意”本身不构成个人偏好。项目决定/规则仅限明确已采用、决定采用或正在执行且可定位项目范围；“可以/建议/是否应该”等备选不算采用。混合消息逐断言判断，保留范围内断言及限定语。主体仍遵循现有绑定规则，不虚构项目实体。整条消息的控制请求不受此提取范围豁免。

**输入**：当前消息原文、必要的用户历史（只用于理解指代，不作为本条的证据）、实体目录、关系目录、`said_on`、可信任务 ID。

**输出**：

```json
{
  "memory_requests": [],
  "candidates": [
    {
      "kind": "assertion",
      "speech": "user_statement",
      "subject": { "entity_id": "self" },
      "relation": "fact",
      "memory_type": "fact",
      "value": { "kind": "text", "text": "我画速写时固定使用炭笔。", "quote": { "text": "我画速写时固定使用炭笔。" } },
      "sentence": { "text": "我画速写时固定使用炭笔。" },
      "polarity": "affirm",
      "strength": null,
      "uncertainty": null,
      "conditions": [ { "quote": { "text": "画速写时" }, "resolved": true } ],
      "times": []
    }
  ]
}
```

- `memory_requests` 是**整条消息一次判断出的请求集合**，无请求时为 `[]`，不再每个候选各填一次。每项 `MemoryRequest = { "kind": "remember | retention | do_not_store | forget", "quote": Quote, "target": { "object": "source | facts | unknown", "candidate_indices": [integer], "quote": Quote? }, "deadline": TimeInterp? }`。retention 的 deadline 必须为 retention 角色；其他种类 deadline 为 null。候选索引必须有效；目标未知时索引为空并保留指代原话，不能扩展到整条消息的所有事实。例如“记住 A，B 只保留到明天”产生两项，分别绑定 A、B。
- source 指该请求所属原文；candidate_indices 仅索引该来源的最终 W1 候选，重排或拒收后保留映射。facts 的空索引不表示全部事实：涉及旧记录的请求只保留目标原话，交给已有控制流程定位；W1 不生成旧记录 ID。请求类型、对象、期限及引用由 W2 检查结构和原文，W3 检查含义。
- 保存授权仍在 W0 前由宿主临时规则检查；W1 请求集合不是新增授权。后台发现疑似漏传的 retention、do_not_store 或 forget 时，标记请求及对象、停止相关整理，进入 `awaiting_confirmation`；对象未知时暂停整条消息整理。报告宿主确认或执行已有控制流程，模型判断不能触发自动删除。已获准保存的留存要求使用 retention_pending 标注，不能声称期限已执行。
- `speech` 枚举为 `user_statement | question | hypothetical | reported | plan`，按候选断言分类，不能因整条消息是问句就丢掉其中的个人陈述。user_statement 表示用户确认的断言；plan 表示用户确有的意图/安排，使用现有 fact 类型，保留 speech、方向、条件、不确定性与时间，不证明已执行。question、hypothetical、reported 不成为用户确认的事实/计划；转述保留实际来源和原文。`memory_type` 仍由 W1 给出，代码核对关系目录允许的类型，W3 核验分类。
- 已完成的预订/购买即使涉及未来事件，也属于 user_statement；将预订事实与未来执行计划分别提取，不从计划推断已经预订，也不从预订推断执行。
- `relation`：目录内的关系 ID，或通用关系 `fact`。`fact` 的 assertion 必须使用 Text：`value.text == value.quote.text`，为足以定位该单条断言的连续原文片段（不要求最短），且 `value.quote` 位于完整 `sentence` 内。同句多断言分别取各自片段；活动地点等断言内容留在片段中，不因此成为条件。
- `sentence`：断言所在的完整原句（可跨连续多句），供 W3、W4、R4 引用。
- 候选类型 `Candidate` 即上述字段对象；`kind` 为 assertion 或 retraction。retraction 表示“撤回之前关于住址的断言，尚无新值”这类事实撤回，value 可为 null；assertion 必须给出 Value。W1 不选择旧记录编号，由 W4 定位。要求删除保存内容的“忘掉我的地址”或“删掉吧”属于 forget 控制请求，宿主在明确授权、目标可定位时调用现有 `memory_forget`，不以 W4 Retract 替代原文、索引和任务删除；含义不明时澄清。
- 已移除的字段：`tags`、`certainty`、`extraction_confidence`、`cross_turn_useful`、`retention_tier`、逐候选的 `memory_request`、`intent`、`target_memory_id`、9 个细分的 scope 字段。retention_tier 由代码策略产生；偏好 strength 第一阶段保留，旧记录已有强度原样保留，不把新字段缺失当作 None 与旧 Strong 比较。

### 2.2 W2 机械校验（代码）

逐候选执行，结果为 `ok | unresolved | rejected`：

| 检查 | 失败时 |
| --- | --- |
| schema 解码 | 整条输出无效：该消息整理失败，原文保留；记 `w1_invalid_output` |
| 所有 Quote 逐字存在 | `sentence` 或 `value` 不存在：拒收该候选。只在 `sentence` 之后追加了非原文内容（翻译、注释、杂字）时，截去追加部分；追加的也是用户原句时不截断，拒收 |
| 实体、关系、类型、任务合法 | 实体不在目录：有原文指代则保留为 unresolved，无法机械定位指代则拒收并保留原文；关系不在目录或类型不被目录允许：拒收；任务不符：拒收 |
| 限定语与 Value | 逐字存在的未解析限定语保留；按 Value 分支核验值、实体及数值表示；通用 fact 只核 Text 相等与片段在 sentence 内，不核断言含义；不从缺失字段猜默认值 |
| 时间结构与日历合法性 | 结构合法但无法换算：保留原文与 `unknown`；日期与原文明确矛盾（如原文写 4 月 15 日，模型给 4 月 16 日）：拒收该候选 |

截断后的 candidate 必须重新执行全部 W2 检查，并以最终完整候选进入 W3；不能复用截断前的核验结果。拒收候选不影响其他独立候选；这条规则仅指已经获准且完成 T1 的原文，不覆盖 W0 的保存拒绝或后续删除。

### 2.3 W3 语义核验 `w3.v6`

coverage 仅统计 2.1 提取范围内的遗漏；仅遗漏讨论论证时为 complete。complete 不代表原文所有命题均已抽取。仍逐一核验送审候选是否获原文支持，不把范围外但获支持的候选改判 contradicted/insufficient，不新增准入字段。

**输入**：W2 通过的候选（含 `unresolved`）、整条消息的请求集合、当前原文、必要历史、实体与关系目录、每个候选的时间换算结果；保留原始候选和请求索引。

**输出**：

```json
{
  "reviews": [
    {
      "candidate": 0,
      "verdict": "supported | contradicted | insufficient",
      "problems": [ { "aspect": "subject | relation | memory_type | value | polarity | strength | uncertainty | condition | time | speech", "quote": { "text": "……" } } ],
      "episode_value": "useful | low_value | unknown | not_applicable"
    }
  ],
  "coverage": { "status": "complete | gaps", "missing": [ { "text": "……" } ] },
  "request_reviews": [ { "request": 0, "verdict": "supported | contradicted | insufficient" } ]
}
```

代码核验：coverage.status=gaps 时 missing 必须非空，每项仍须为原文中可核对的精确连续引用；complete 允许 missing 为空。reviews 与输入候选一一对应、无重复，request_reviews 与送审请求一一对应；所有 Quote 逐字存在；supported 时 problems 为空。核验结果绑定最终候选、请求集合及来源快照，输入变化后不得沿用。

- supported 的 user_statement 或 plan 候选在主体、极性、类型、限定语及保存授权等提交条件满足时进入 W4 或 W5；计划的 supported 只核验用户确有此计划，不核验未来事件已经发生。原文的条件、不确定性、时间、方向或强度被丢失/改变时不得 supported；完整句子里保留限定语不能替代相应字段。待澄清候选保持候选状态。原文明示反驳候选或候选改变明确限定语而造成冲突时判 contradicted；相关陈述内部矛盾且无法确定哪项错误时判 insufficient，不单方面择真。通用 fact 的 value 定位该候选断言，同句独立无关断言不影响本候选；否定范围、共享限定语、回指与同句纠正仍按完整原文核验。无关候选独立核验。两者不提交，原文照常可用。W3 同时核验 memory_type、偏好强弱、限定语和请求所指对象，不能把无关 remember 请求当作全体候选的明确保存要求。
- Episode 的跨轮价值由同一次 W3 给出 episode_value：只有 useful、已核验且绑定该候选的 remember 请求，或 W4 判定为对既有记录的更新/撤回时，沿用现有经历准入；low_value/unknown 且无上述依据时仅保留原文。不因 low_value 提前跳过必要的 W4，准入在更新判定后执行。非 Episode 的 episode_value 为 not_applicable，不把旧 cross_turn_useful 默认为 true 来绕过检查。
- `coverage.status = gaps` 时，已支持的候选照常提交，缺口写入整理状态（`coverage_gaps`），召回时可见。
- W1 输出为空时不调用 W3，整理状态为 no_candidates_unreviewed：不代表原文没有可记内容，请求也不因此算核验通过。2.1 的疑似控制请求暂停规则仍独立生效；第一阶段不为了执行保存期限额外调用模型。

### 2.4 W4 更新判定 `w4.v5`

**何时调用**：W3 支持且可判定的候选中，存在相关记录或未解决关系时调用。断言未检索到任何相关记录时不调用 W4，以追加新记录进入 W5；范围不完整时（通用 `fact` 未在授权范围内实际穷举，或穷举/闭包超预算、覆盖未知）令 update.status=pending、reasons 含 update_scope_incomplete，不宣称它是唯一当前值。retraction 找不到目标时由代码保留 pending/target_unknown，不调用 W4，也不作为有值断言提交；不能把“没检索到”当作“没有旧记录”。

**相关记录（代码取得）**：在合法主体及任务范围内取同关系当前记录、未解决关系的成员及必要历史，加上全文检索的相关记录，合计不超过输入预算。目录内的专用关系（如 `lives_in`）和已知未解决关系不能被相似度截掉；超出预算如实报告。通用关系 `fact` 先在授权主体、任务和同一读快照内实际穷举当前记录，包含必要历史与关系闭包；全部送入且未超输入预算、没有未知覆盖时才可令 `coverage.complete = true`。放不下时回落到全文检索候选并保持 `complete = false`；FTS 命中未截断不等于范围完整。越界关系整条扣留，超预算或覆盖未知仍保留 pending。完整仅指本次授权范围，不证明现实世界没有其他事实。已有 update.pending 须经显式重判才能改变，不批量改为 judged。第一阶段不用向量检索。

**输入**：

```json
{
  "candidates": [ { "index": 0, "committed_id": null, "candidate": "Candidate", "source": "SourceRef", "said_on": "2023-05-30", "time_resolutions": ["TimeResolution"] } ],
  "records": [ "RecordView2" ],
  "relations": [ "RelationLink" ],
  "coverage": { "complete": true, "omitted": 0 }
}
```

`RecordView2` 见 3.2：在 RecordView 的完整记录分支上增加结束时间 `end`。通用 fact 的片段 value 定位候选断言，完整 sentence 保留上下文与依据；旧整句 value 记录继续读取，不改写。W4 检索仍使用完整 sentence，不能因共用原句判为同一断言。W4 输入候选与共享 stored 来源的 said_on 对历史补录可为 null（第④步实现），普通当前消息日期保持已知。

**输出**：

```json
{
  "decisions": [
    {
      "candidate": 0,
      "status": "decided | pending",
      "action": "new | reinforce | state_change | correction | refine | retract | coexist | conflict",
      "targets": [17],
      "resolves_relations": [],
      "basis": [ "EvidenceRef" ],
      "effective": null,
      "relation_scope": null,
      "pending_reason": null
    }
  ]
}
```

`relation_scope` 只在 conflict 时非空：`{ "conditions": [QualifierRef], "times": [QualifierRef] }`，表示这对冲突在什么条件、时间下成立，由 W4 判断；两个数组都为空表示 W4 判断冲突没有限定范围。`QualifierRef = { "of": "candidate" | MemoryId, "index": integer }`：引用本决定的新候选（`"candidate"`）或某个 target 记录（其 ID）已核验的限定语，`conditions` 引用其 `conditions[index]`，`times` 引用其 `times[index]`。W4 不为冲突范围新写片段：限定语原文、所属来源与换算结果都取自被引用的断言，相对时间按那条断言的 `said_on` 换算。代码核验：被引用者是本决定的候选或 targets 之一、完整记录（legacy 记录没有可引用的限定语）、索引有效、时间不是 retention 角色；引用关系随决定回执保存。一个 conflict 决定的全部 targets 共用同一个范围；新断言与不同目标的冲突范围不同时，本版不表达，决定为 pending/unclear_relation。其他动作及 pending 时 `relation_scope` 为 null。

代码核验：每个候选恰好一个决定，targets 无重复且只能为输入记录；decided 的 action 非空、pending_reason 为 null。new/coexist 的 targets 为空；reinforce 只有一个目标；state_change/correction/refine/retract/conflict 至少一个目标。basis 引用输入来源中的真实片段；真实冲突须有新旧双方依据，不能仅凭检索不足产生。action 为上述枚举或 JSON null（不是字符串 "null"）；effective 为 TimeInterp 或 null，非空时必须绑定新候选来源且在已核验时间中，只允许事件或有效期角色，不能使用保存期限；合法换算结果由代码提供，不由 W4 改写。

resolves_relations 是 W4 明确判定本次动作已解决的输入关系 ID，不能由代码看到一次更新就自动推断。只能引用 open 关系，成员及适用范围必须完整覆盖，basis 包含明确解决依据；pending/new/coexist/conflict 时该数组为空。动作与关系解决一起提交；依据保存在关系 resolution 及完整决定回执中。

pending 的 action 为 null，pending_reason 为 `target_unknown | scope_incomplete | unclear_relation`，targets 可以为空或仅列已定位目标，不修改旧记录。

committed_id 由运行时绑定，首次提交前为 null；重判已提交且 update.pending 的断言时为原记忆 ID，候选不与自身做新旧比较。W4 的 pending 表示不执行针对旧目标的动作，不等于 W3 已支持的断言不能保存；W5 的断言追加与更新状态按下述规则分别处理。

coverage 不完整时区分两类结论：

- **依赖“没有其他相关记录”的结论不成立**：不能据此确认唯一当前值，也不能因 retract 找不到目标就忽略。这类更新决定为 pending/scope_incomplete，不改判 conflict。满足 W3 与准入条件的 assertion 可追加为记录：候选提交状态为 committed，update.status 为 pending；主体、极性等尚未解析的候选及 retraction 仍只保留候选和原文，不借此绕过提交条件。
- **针对已明确定位目标的动作可以执行**：state_change、correction、refine、retract 的目标都在输入中、经过逐条比对且有新旧双方依据时，代码按常规核验后执行，只修改这些目标。对未送入记录的关系仍以 update.pending 持久标注；新记录也不能被包装成全局唯一当前值。R3/R4 按 3.2、3.3 带出该状态，不能仅靠“后台尚未完成”兜底，因为事实提交后缺口仍可能存在。

范围与快照校验由代码执行，不因模型返回 decided 而跳过。

计划更新由 W4 按原文语义判定：真实意图的改变/取消通常为 state_change，旧计划曾作为意图成立；先前断言错误为 correction，撤回断言为 retract。取消不等于删除，不自动连带取消预订；不在代码中根据日期经过、否定词或接收顺序猜这些关系。

**动作与现有引擎的对应**：

| W4 动作 | 引擎动作 | 旧记录 | 召回中的表现 |
| --- | --- | --- | --- |
| `new` | New | 不变 | 新增一条断言；范围不足时 update.pending，不确认唯一当前值 |
| `reinforce` | Reinforce | 强化计数 +1、追加证据 | 当前值不变 |
| `state_change` | Supersede（`StateChanged`；偏好 / 约束类用 `PreferenceChanged` / `ConstraintChanged`） | Superseded，`valid_to` 为 `effective` 或未知 | 新值为当前，旧值为历史 |
| `correction` | Supersede（`Correction`） | Superseded，标为被纠正 | 旧值不作为历史事实出现 |
| `refine` | Refine（`Refinement` / `ScopeRefined`），须适配现有执行语义 | 仅在确认修订目标整个断言时结束旧版本 | 局部补充用 coexist 保留较一般的旧事实；不能默认结束旧全局记录 |
| `retract` | Retract（`UserRetracted`） | 结束 | 不再作为当前值 |
| `coexist` | New | 不变 | 两条各自带条件或时间并列 |
| `conflict` | **新增：保存冲突成员及 RelationLink** | 旧版本不变 | 两条都返回，关系为 open，不给唯一当前值 |
| `pending`（更新决定状态） | 不执行旧目标动作；合格 assertion 可追加并带 UpdateState，其余暂存 | 不变 | 事实提交状态与关系待判定分别呈现，不伪造真实冲突 |

`RelationLink = { "id": string, "kind": "conflict", "members": [MemoryId], "conditions": [TextQualifier], "times": [TimeInterp], "basis": [EvidenceRef], "status": "open | resolved | invalidated", "resolution": EvidenceRef? }`。conditions、times 是建立关系时 W4 经 `relation_scope` 引用的限定语副本，表示关系的适用范围，不是任一侧限定语的机械复制。真实 conflict 保存已核验新断言及其与旧目标的关系，当前资格由关系是否解决共同约束；不把冲突当作事实已被否定，也不只给整条 Memory 加 Conflicted 状态。成员至少两个，均存在于同一授权范围。

优先在现有 owner 快照增加关系集合，不新增数据库或存储框架；旧快照缺此集合时迁移为 []，快照格式版本与旧程序拒绝读取的规则同步调整。后续明确的 W4 判定或用户确认才能解决关系；解决依据持久保存。成员/依据来源删除时关系失效，不能通过关系恢复被删除内容，也不连带删除另一端仍获准保存的记录。

### 2.5 W5 提交（代码）

- 已核验的肯定或否定计划断言满足主体/类型等条件时可提交，polarity=unknown 仍暂存。完整 Candidate 是权威，speech=plan、极性及全部限定语随决定回执保存并供 RecordView 返回。现有 Memory 是执行投影，不表达 speech 时必须标为非无损，不能将投影冒充已发生的事实。已核验的 user_statement、memory_type=fact 的否定断言按既有准入条件提交，执行极性为 Negative；unknown 仍暂存。完整 Candidate 为权威，否定本身不决定更新、冲突或撤回。否定 episode 的投影限制不变。

- 在写锁内重读，逐项检查：来源未删除、认领有效、目标记录状态与 W4 输入时一致、快照版本未失效。任一不满足，该条决定进入 `needs_rejudge`，不复用旧决定；committed_id 非空时同时检查它仍对应同一来源、候选和有效版本。 纯新增实体不使已有判定失效：旧实体全部逐字段不变，新增 ID 不存在，新增名称及别名按现有目录解析规则不与旧实体冲突，且同事务记录、暂存、关系及更新状态不变时，只豁免无来源屏障推进，业务 revision 仍递增。既有屏障不降低，已失效任务不恢复；较新来源、删除、授权、目标快照及租约检查不放宽，不自动绑定 unresolved。重启后目录与阶段绑定不一致仍须重判，不改写旧绑定。
- 旧记录被 state_change 结束而 effective 未知时，结束时间存为未知，不回填接收时间。召回的历史查询不能把“结束时间未知”当作一直有效：现有检索把没有结束时间的记录视为持续成立，须改为对已结束且结束时间未知的记录标注 `end_unknown`，与新记录按接收顺序一起交给 R4 判断，不在代码中推定哪条在某个时间点成立。
- 独立候选分别提交；同一决定中存在依赖的动作（如撤回旧值并写入纠正值）一起提交或一起失败。
- 同一原文拆出、彼此依赖的多个决定（例如一个决定结束冲突败方并解决关系，另一个决定细化胜方）是一组，在同一 T2 中重查全部目标与关系快照后原子提交：任一决定 needs_rejudge 则整组重判，提交失败则整组回滚，不能只完成其中一部分。
- state_change、correction、refine 结束的旧记录若是 open 关系的成员，而本次没有决定解决该关系：关系保持 open，成员仍指向原记录，依据不变，不把成员 ID 机械换成替代记录——多个成员被同一新版本替代时会收敛成同一条，旧依据也不能证明新版本仍在冲突。替代记录的 update 为 pending，原因含 update_relation_unclear；召回时经 history 带出前身所在的 open 关系，冲突在历史与当前查询中保持可见。新版本是否仍与其他成员冲突、旧关系是否已被解决，只能由后续 W4 依据新原文判定：新冲突另建关系，解决旧关系用 resolves_relations。此条与上条分别适用：原子提交处理同一原文的相关决定，本条处理不解决关系的普通替代。
- W4 决定与现有引擎动作是语义对应，不能直接把新候选再交给旧确定性策略重判。复用存储与幂等检查，适配已验证决定的执行；现有 Refine 会结束旧版本、未知有效时间会回退到观察时刻，这两处必须修正。effective 未知时保留未知，接收时间仅用于来源顺序，不伪造现实终止日期。
- 多目标 state_change/correction/refine/retract 在同一 T2 中重查全部目标，结束全部授权目标并最多插入一次新版本；任一目标失效则整组 needs_rejudge，不能循环单目标入口产生多个新版本或部分成功。完整 targets/basis 保存在原文任务的决定回执；现有单个 supersedes 仅作兼容字段，不能作为完整关系依据。committed_id 已存在时复用该断言记录，按重判结果更新关系和状态，不再插入同一断言或把自身当作一次强化。若重判为 reinforce 到另一既有记录，按来源幂等归并证据，在决定回执保存已提交 ID 到保留目标的替代映射并退出重复投影；证据只计一次，不同时返回两份等效记录。
- conflict 的新成员与 RelationLink 一起提交；解决既有关系时同一 T2 重查关系及成员快照并更新状态。pending 不强化、不关闭旧记录；合格 assertion 的追加、UpdateState 和回执同一 T2 提交，retraction 或未满足提交条件者仅暂存。reinforce 仍检查独立用户证据，同轮重放不增加计数。
- 事务失败回滚 T2，原文（T1）不回滚。

### 2.6 现有记忆的迁移

现有记忆的条件主要是结构化字段，未必保留本契约要求的完整原句、TextQualifier 和 TimeInterp，不能无损适配为本版完整记录。历史 capture_source=false 路径曾可提交事实，不能假定每条旧记忆都有独立 SourceRecord。默认路径切换不以全部旧数据重建为前提；旧数据持续通过 LegacyRecordView 读取。语义重建另行授权、有预算和替代映射，不自动转换旧任务。重建前先盘点完整原文、旧回执、片段及原始保存授权，再决定哪些来源可重建；不把片段拼成整条原文，不因迁移扩大保存授权。

- 对具备可核对完整原文、原始接收时间/顺序及保存授权的部分，按 target-design 1.4 用当前 w1.v5、w3.v6、w4.v5 重新整理，作为显式、有预算的重建任务；先产生并核验新结果，再切换对应的有效整理版本，不先清空可用记忆。
- 用户确认、实体登记和删除墓碑等控制记录保留并约束重建；已删除的原文不参与重建。
- 重建前及缺可用来源而无法重建的旧记录，通过 3.2 的 LegacyRecordView 持续召回：保留现有值、方向、强度、结构化条件、时间和依据，缺口单列，不补造完整 Candidate 或新 Quote。不能因新版本启用、预算耗尽或重建失败而丢掉这些记录；已删除或未获准的内容仍不可返回。
- 旧/新记录的替代映射保存在重建回执；只有已核验替代的旧记录退出有效召回，不双返回已被替代的旧记录，也不删除未覆盖的 legacy 记录。确认与删除控制引用沿用该映射，重建不能改变其含义。
- 迁移验收用相同输入比较重建前后结果；实际迁移范围按可用授权来源确定，不缩成评测集。可重建、无法重建、失败、尚未完成的数量和模型调用预算在执行前单独报出。

### 2.7 来源确认、重建与历史补录

- 全文重提取只接受没有任何已提交动作的来源，新代际重新提取并核验，旧代际审计保留；不得把旧核验与新候选数组按索引拼接。混合来源仅逐项确认未提交候选，不覆盖已提交产品。普通重判复用原候选，不等于全文重提取。
- 历史补录只复制明确获准的独立完整 USER history 条目，不拼接、不改写、不导入助手文本；保存父来源/全文哈希/槽位出处与独立授权。原陈述时间与新接收时间分开，未知 said_on=null；模型取得原始时间锚点及早于父来源的顺序关系，新 received_sequence 不等于最新陈述。
- 删除传播覆盖依赖的补录、确认、派生记录、索引、缓存及原始审计副本，不扩散到独立获准且无依赖的来源；墓碑阻止重放与晚到执行恢复原文。主体确认只提供身份依据，不给旧候选事实值新增证据。
- 确认、重建、补录均绑定稳定请求 ID、参数、来源/代际与控制审计；同参重放幂等且不新增模型调用，异参不能复用，同一历史条目不能借槽位变化绕过墓碑。

**写入管理接口**（不属于 recall.degraded_reasons）：

- `memory_source_reprocess` 输入 `{source_id, request_id, expected_state_token, mode:"confirm_subject"|"reextract", enqueue:false}`。确认另传 `candidate_index`、已登记 `entity_id` 和 `confirmation:EvidenceRef`，引用必须是当前 owner 已获准完整保存的 USER 来源、可逐字核对的连续片段；任务来源仅在同可信任务内使用，全局确认可用于该任务。代码只核引用、归属、目录及提交资格，W3 核身份语义。只改选中候选主体，保留完整数组原索引；只允许选中项进入本轮 W4/W5。
- `enqueue=false` 仅存控制与新代际，不调用模型、不自动入队；`true` 须有限 model_budget、网络门控以及 inline/deferred 后台授权。同参回执重放不重新入队；回执给接受时状态，当前进度另查 `memory_source_status`。
- 返回同 status 的来源、整理、generation/round、state_token、出处、时间锚和候选索引/原因，附 `enqueued`、`replayed`、`model_calls:[]`；不返回事实值、原文、确认片段或旧代际原始输出。确认摘要包含代际、候选索引/身份哈希、实体、来源引用、片段哈希；审计公开旧代际号。
- `memory_source_import_history` 输入 `{source_id, history_index, expected_content_fingerprint, request_id, capture_source:true}`，不接受自带正文或日期；`memory_source_status.history_entries` 只给槽位、字节数与全文指纹，不公开原文。父来源及 history 必须已获准完整 USER 留存，再由本次显式授权逐条复制，任务范围继承父来源。返回同 status 的片段，附 `enqueued:false`、`replayed`、`model_calls:[]`；只做 T1+索引，不自动整理，另用 reprocess。
- 补录身份按父来源 ID＋完整条目指纹绑定，原槽位仅作出处；不同 request_id 同条目仍只产生一个来源，同 ID 异参 `request_id_conflict`。保存独立授权与父来源/指纹/槽位、`before_source_id`；新接收序号只用于事务屏障，不能冒充最新陈述。原陈述时间未知时 `time_anchor.said_on=null`，接收时间另存 observed_at，不从父消息或请求时刻推算。
- 补录的 nullable `said_on` 原样进入 W2/W4、提交记录及 R3/R4；字段必须出现，旧已知日期字符串仍可读取。未知锚点的 relative/month 换算为 unknown，绝对 date/range 仍按原文核验；代码不按接收序号决定语义更新。R4 沿用 v4。
- 补录错误：无/异 owner 父来源 `source_not_found`；父/条目墓碑 `source_deleted`；未获准留存 `operation_not_authorized`；槽位不存在/非完整文本 `history_entry_not_found`；指纹不符 `history_content_changed`。原子拒绝，零模型调用。删除父来源递归删除依附补录，删除子来源清除历史副本、产品、确认、索引、缓存及审计；子来源仅保留不含原文的父/指纹/槽位墓碑，槽位移动不能绕过删除；无关独立来源保留。
- 活跃租约 `source_busy`；状态令牌不符 `source_changed`；未知/异 owner `source_not_found`；墓碑 `source_deleted`；无效引用/实体/候选（含已提交项）`invalid_confirmation`；全文重提取涉及任何已提交动作（含无产品动作）`rebuild_requires_uncommitted_source`；异参重放 `request_id_conflict`；无网络/有限预算/后台授权 `operation_not_authorized`。操作原子拒绝；不隐式重试。
- 代际与 round 同时递增，取得新的排序序号、操作预算身份和来源绑定；原接收序号/时间不变，不放宽其他来源屏障。旧执行者及旧模型结果不能提交。DB 10 沿用 SourceRecord 控制/审计及 EnrichmentTask.generation；各 owner 旧字段缺省普通来源/代际 0，旧回执不改写、不自动入队，v9 写程序拒绝 v10，只读支持 3–11。删除传播同时清除确认依赖与代际原始副本；确认不是独立事实支持。

## 3. 召回侧

### 3.1 R1 问题理解 `r1.v2`

**输入**：本轮消息（问题或陈述）、实际用户上下文、实体与关系目录、`said_on`、可信任务 ID。

**输出**：

```json
{
  "need": "context_only | judge",
  "current_evidence": "assertion_present | query_only | unknown",
  "type": "single | aggregate | comparison | temporal | open",
  "subject": { "entity_id": "self" },
  "relation": "去健身房的时间",
  "conditions": [ { "quote": { "text": "周二" }, "resolved": true } ],
  "time": null,
  "plan": { "op": "lookup | list | count | sum | compare | elapsed", "filter": { "relation": "fact", "time": null }, "unit": null }
}
```

- `need = context_only`：本轮只需带出相关上下文（如普通陈述句），没有要判断的问题。`judge`：需要判断事实是否被支持、新旧关系、条件是否适用，或需要计算。
- current_evidence 区分本轮包含陈述、只有查询文本及未知；不能因为 current_message 总存在就把查询问题算成事实证据。失败时为 unknown，不把其中可能的陈述删除。
- `subject` 可为 `unresolved`（原文指代）或 `{ "ambiguous": ["entity_a", "entity_b"] }`；这是 R1 的初步主体判断，不擅自选择。R4 在完整授权证据包内终审原问题是否仍需澄清；开放列举不强制选择单一人物。未调用或未通过 R4 核验时不宣称歧义已解决，保留原有跳过/失败状态与 R1 澄清提示。
- `relation`、`conditions` 是自然语言描述，用于检索和给 R4 参考，不做字面过滤。
- `plan.op` 只能是列出的六种；`filter` 只能收窄范围，不能扩大访问范围（主体、任务由代码按可信上下文限定）。
- filter.relation 为目录内 ID 或 null，filter.time 为 TimeInterp 或 null，unit 为代码支持的输出单位或 null；比较和间隔所需的操作数由 R4 明确引用，不能执行自由文本表达式。

代码核验：实体在目录内；任务不超出可信范围；`time` 为合法 `TimeInterp`；`op` 在枚举内。失败时按“未理解”处理：做宽泛检索，`need` 视为 `judge`。

### 3.2 R2、R3 检索与证据组织（代码）

R3 交给 R4 的证据包：

`RecordView = CurrentRecordView | LegacyRecordView`，用 format 判别分支，不给 legacy 补造必填 Candidate：

- `CurrentRecordView = { "format": "current", "id": MemoryId, "stored_status": "active | superseded | expired", "termination_reason": SupersedeReason?, "ended": "not_ended | known | end_unknown", "candidate": Candidate, "evidence": [EvidenceRef], "grade": "committed", "said_on": LocalDate?, "time_resolutions": [TimeResolution], "history": [MemoryId], "open_relations": [string], "update": UpdateState }`。
- `LegacyRecordView = { "format": "legacy", "id": MemoryId, "data": LegacyMemory, "evidence": [EvidenceRef], "grade": "committed", "update": UpdateState?, "gaps": ["source_missing | sentence_missing | qualifiers_unmapped | time_unmapped"] }`。LegacyMemory 冻结第一阶段开始前 src/model.rs 的 Memory 序列化形状，沿用旧格式解码并保留旧字段，不随新核心类型变化而改释；先执行相同的归属、任务与删除过滤，不把旧 scope 转成无依据的 TextQualifier。旧版没有更新判定状态时 update=null，表示未知而非 judged。

`RecordView2 = CurrentRecordView2 | LegacyRecordView`（当前 w4.v5 的输入使用）：`CurrentRecordView2` 在 CurrentRecordView 的全部字段之外增加 `"end": TimeResolution?`。`ended = known` 时 `end` 为结束时间的换算结果，保留原精度（月精度不补成具体日，多日区间用 start / end_exclusive 表达，不折成一个时间点）；`ended = not_ended` 或 `end_unknown` 时为 null。结束时间来自结束该记录的 W4 决定的 `effective`，其换算结果取候选已核验时间中的对应项，不由 W4 给出；换算结果为 unknown 时 `ended = end_unknown`。结束它的决定没有保存换算结果（本版之前的回执）时，`ended` 按旧字段判断、`end` 为 null，不补造精度。r4.v7、recall.v11 仍使用不含 `end` 的 RecordView；召回侧实现该字段时改用 RecordView2 并升版。

沿用现有 SupersedeReason 区分现实变化、纠错和撤回；Deleted 不进入任一分支。完整记录的 candidate.kind 为 assertion；用户确认保留独立控制记录及依据，不改写成不存在的完整用户原话。legacy 的 evidence 仅包含能链接到真实获准来源的引用；只有旧数据中的片段而没有可链接原文时 evidence=[]，片段留在 data 内，不能伪造 source_id 或作为新契约已核验的原文依据。gaps 汇入 enrichment_gaps，原文存在则同时交给 R4 核对；无法核准完整断言及限定语的 legacy 目标在 W4 保持 pending，不能以缺字段等于无限定来更新。

records 的完整 candidate.speech=plan 标识计划，保持肯定/否定、条件、不确定性和时间；不从 stored_status=active 推断事件已发生。stored_status 与旧 data.status 均不是本轮确认的 current。CurrentRecordView.update 从持久回执/整理版本读取；提交后的 pending 仍进入 R3/R4 及宿主 records，不因 source.enrichment=committed 消失。

`RawEvidence = { "source_id": string, "quote": Quote, "said_on": LocalDate?, "sequence": integer, "grade": "uncommitted", "enrichment": string, "memory_requests": [MemoryRequest] }`。enrichment 使用 3.5 的状态枚举；保留接收顺序及尚未处理的限制。模型不能因为 grade 改变事实是否可用。共享 stored 来源的 nullable said_on 仅用于原始时间未知的补录，代码类型/schema 在第④步实现；不能用新接收日替代。

```json
{
  "current_message": { "source_id": null, "text": "……", "said_on": "2023-05-30", "sequence": null, "enrichment": "not_stored", "memory_requests": [] },
  "records": [ "RecordView" ],
  "raw": [ "RawEvidence" ],
  "relations": [ "RelationLink" ],
  "completeness": { "retrieval_truncated": false, "pending_related_sources": 1, "pending_updates": 0, "enrichment_gaps": 0, "plan_complete": true }
}
```

- 本轮消息固定放在 `current_message`，不依赖检索命中（target-design 2.6）。
- CurrentMessage 即上例类型，source_id、sequence 可空，enrichment 使用 3.5 枚举，未持久保存时为额外值 not_stored。只读召回或未保存的当前上下文使用 null，不为取得证据 ID 而保存问题；bootstrap 无本轮证据时 current_message 可空。
- 相关的未整理、失败、待重新判定的原文放入 `raw`，保留接收顺序和状态。
- `aggregate` 计划下，R2 在计划范围内枚举全部匹配记录（可分页），并在 `completeness.plan_complete` 如实报告是否截断。
- 该证据包在 R4 跳过/失败时也返回宿主；pending_related_sources 统计 raw 中相关未完成来源，不能用计数代替原文。来源删除、快照和输出预算检查在返回前再次执行；正文被预算截断时必须标为不完整。
- pending_updates 统计检索到的相关 update.pending 记录，即使它们已提交；预算裁剪的已知相关记录仍计入，并标为不完整。计数不替代逐记录状态，不能用 0 宣称未检索到的范围没有缺口。legacy 的缺字段/未知更新状态通过 gaps 与 enrichment_gaps 呈现。

R1 的单实体主体提示用于候选收窄，不认证记录主体。非 self 实体可同时纳入主体匹配、结构化实体值精确引用及登记名字字面提及的获准记录；名字至少两字符，按生产分词的完整连续名字匹配，仅检查记录文本 value 与已有 evidence quote，不扫描整条来源正文。提及仅作为候选线索，不加分、不保送，不改主体、身份或 grade/update；实体归属与相关性由 R4 判断。宿主可信 owner/主体/任务范围不变，后续关系提示、排名、枚举、闭包及预算规则照常执行。self、ambiguous/unresolved 主体保持原行为。该候选规则不依赖 embedding 开关。

仅 `embedding.enabled=true` 启用下列混合检索；关闭时不启用 DF/向量融合，但同样应用上述主体候选规则。宿主包络为 recall.v11。

- 开启混合检索时，在关系过滤前，可信范围经 R1 主体收窄后的全部记录数不超过 records 预算即尝试穷举；不做字节预判，闭包及字节裁剪仍撤销完整标记。关闭时保持原小范围规则。保持 aggregate/list/count/sum 计划枚举，不调用查询嵌入。其他非穷举路径统一融合，R1 的关系过滤不留保留席位、关系成员不投词法票；R1 主体提示只能收窄宿主可信范围。
- 词法通道使用生产中文相邻字对、字母数字词与实体别名。DF 按独立、现存且获准正文来源计数，在 owner/任务/宿主可信主体范围内、先于 R1 主体及关系提示统计；documents>0 时，df<=1 的词元保留；其余严格超过 20% 的词元不计分。零命中记录不投票，按实体/短语命中数、接收顺序、ID 排序。
- 向量通道只检索可信 owner/任务/主体范围，忽略关系过滤；来源分段的最佳相似度通过原文引用重叠映射到链接记录。等权 RRF(60)，两路各取 2×records 种子，融合后展开历史和关系闭包，再执行数量与字节预算；闭包成员没有额外席位。截断如实报告，plan_complete 只来自实际枚举，索引 ready 不认证完整性或语义。
- 查询嵌入至多一次，使用原问题文本，不附 context；正文、向量与记录映射取同一 R3 快照，事务及服务锁释放后执行本机请求，总时限含 digest 等元数据请求。失败退回 lexical-df；部分/陈旧/损坏索引仅用可核准的向量。bootstrap/offline/deny 不发送嵌入或元数据，同参重放不重调。删除及授权检查仍在发布前执行。

### 3.3 R4 证据判断 `r4.v7`

通用 fact 的片段 value 定位该记录断言；核验仍保留完整原句依据与全部适用限定语。旧整句 value 原样读取。记录身份来自明确的 record_ids/supersedes_record_ids，不能因兄弟记录的原句依据交叠借用提交等级或更新状态；一个候选的完整 sentence 不证明来源中所有断言已整理。

**何时调用**：need = judge 且有可判断证据时，在网络/预算允许的普通在线请求调用一次；records/raw 可作证据，current_message 在 current_evidence 为 assertion_present/unknown 时送审。仅有 query_only 的问题文本不算 no_evidence 检查中的事实证据。bootstrap、离线、预算不足或 context_only 时跳过并标明原因；R1 失败按 judge/unknown 降级，不擅自默认单事实。

**输出**：

```json
{
  "verdict": "supported | insufficient | conflict | needs_clarification",
  "facts": [
    {
      "statement": "通常 6 点去健身房",
      "evidence": [ { "source": { "kind": "stored", "source_id": "s-102" }, "quote": { "text": "which is usually at 6:00 pm" } } ],
      "record_ids": [21],
      "polarity": "affirm | negate | unknown",
      "strength": null,
      "uncertainty": null,
      "conditions": [],
      "times": [],
      "current": "yes | no | unknown",
      "applies": "yes | no | unknown",
      "supersedes_record_ids": [17]
    }
  ],
  "compute": null
}
```

FactView 即上述 facts 条目，保留所有限定语；applies 为 yes/no/unknown，current 为 yes/no/unknown，不能以记录 Active 推导本轮 current=yes。代码检查 EvidenceRef、record_ids/supersedes_record_ids 在输入内、限定语 Quote 存在于对应证据来源，并把 R4 限定语完整传到宿主；statement 中也须保留否定、不确定性等含义。supersedes_record_ids 非空时必须有新旧依据，不产生持久版本关系。

R4 对是否需要主体澄清作最终语义判断，不机械继承 R1 的 unresolved/ambiguous。原问题要求开放列举且证据足以支持相应列举时可判 supported；原问题所指人物仍有真实歧义时必须 needs_clarification，即使已找到部分 facts。代码只消费通过完整机械核验的 verdict，不按事实数量、目录唯一性或检索命中猜主体；owner、可信主体/任务范围、删除保护和完整性门控保持不变。

R4 必须读取逐记录 update 与 legacy.gaps。可以判断原文支持的新断言或对明确目标的变化，但不能把更新范围不足包装成唯一当前值；legacy 片段与旧结构可用于理解上下文，缺可核准来源/限定语时不能据此新增 supported 事实。R5 将引用记录的待判定原因合并带到宿主 facts，不因 R4 返回 supported 而清除持久标注。

计算 compute 可空，非空为 `ComputePlan = { "op": "count | sum | elapsed | compare", "items": [ComputeItem], "operands": [string], "unit": string?, "coverage": "complete | incomplete | unknown" }`；op 必须与 R1 获准计划相符。`ComputeItem = { "id": string, "evidence": [EvidenceRef], "quantity": Quantity?, "date": TimeInterp?, "date_source": SourceRef?, "event_group": string? }`。条目 ID 唯一，同一消息可以包含多个条目；operands 引用条目 ID，elapsed/compare 固定两个且顺序表示结束减起点／左与右，count/sum 则为空。sum/数值 compare 需要 quantity，elapsed/日期 compare 需要 date，比较双方须为同种可比量；未知时不填猜测值。date 非空时 date_source 必须指向条目依据中的一个来源，代码按该来源接收日解释相对时间；无 date 时为 null，不能改用本轮或后台执行日。

**提问时刻端点（r4.v2 起）**：“距今多久”的结束端点不是用户原话，R4 不为它编造 ComputeItem、Quote 或 TimeInterp。operands 可使用保留标识 `@request.at` 指代本请求的可信提问时刻，只能作为 elapsed 的第一个操作数（结束端点）；ComputeItem 的 id 不得以 `@` 开头。代码以运行时绑定的 request.at（UTC 秒）按用户时区换算为本地自然日，精度为日；模型不提供该值，也不能用本轮消息的 said_on、后台执行日或 `current` 来源加“相对 0 天”代替。起点精度粗于日时按上文规则返回不足。

**elapsed 的取整口径（r4.v3）**：数值、取整口径、精确天数和是否近似全部由代码从两个端点计算，模型只提供端点依据，不填写这些字段。两个端点都必须有日精度（起点为已换算到具体日期的 TimeInterp，终点为 `@request.at` 或同样到日的条目）；任一端点只有月、年或更粗的精度时返回不足（compute_insufficient），不把月份或年份补成具体日期。设起点、终点为用户本地自然日 a、b，天数 d = b − a；d < 0 时返回不足。

| unit | value | rounding | approximate |
| --- | --- | --- | --- |
| `day` | d | `calendar_days` | false |
| `week` | ⌊(d + 3) / 7⌋，即最近整数周；d 为整数，不存在恰好半周 | `nearest_week` | d 不是 7 的倍数时为 true |
| `month` | 完整日历月数：年差 × 12 + 月差，b 的日小于 a 的日时再减 1 | `complete_calendar_months` | false |
| `year` | 完整日历月数 ÷ 12 向下取整 | `complete_calendar_years` | false |

周的规则与第一阶段前的旧实现一致（如 20 天为 3 周）。approximate 为 true 时宿主须表述为“约 N 周（d 天）”，不能说成恰好 N 周；月、年表述为已满的整月、整年数。

count 的 event_group 由 R4 判断，同组表示同一事件，不同非空组表示已确认不同事件，null 表示身份未知。代码按组去重，不能按消息 source_id 去重或把所有 null 当作不同事件；标签及片段存在仅是机械检查，事件同一性仍须模型质量评测。sum 对同一计量对象的重复提及也去重；同组量或单位矛盾时不能选择其中一条后宣称精确。

数值须有原文依据，日期须有合法 TimeInterp/换算结果；代码核验单位、精度、允许操作、条目/操作数引用及溢出后计算。模型不给最终计算数字或比较结论。elapsed 的两个端点明确区分事件时间和有效期，精度不足时不能从月份生成精确天数。

- `statement` 是模型对事实/计划的概括，**不要求逐字**；可核验的是 `evidence`。计划必须显式表达为计划、原计划或已取消的计划，不能概括成已发生；current=yes 只指最新确认的计划版本，不证明执行。计划查询可 answered，执行问题缺独立证据时 insufficient。预订事实独立核验，不从计划取消推断预订取消；state_change 旧计划可作为历史意图，correction/retract 旧断言不当作真实历史。
- `supersedes_record_ids` 只表示本轮判断中新证据取代了旧记录，**不写入存储**；持久的版本关系仍由后台 W4 决定。
- exact 只在查询范围完整枚举、相关来源已覆盖、成员身份及所需输入明确时成立；代码综合 R2/R3 完整性与 R4 判断，模型自报 coverage=complete 不能覆盖代码已知的截断或遗漏，grade 不决定 exact。否则按操作分别处理：count 只计已确认不同的事件组，可以给有依据的下限；sum 只有已知未计部分非负等条件得到支持时才能给下限；未知符号或重复对象不能给下限；elapsed/compare 缺少端点、必要量或足够精度时返回不足，不输出猜测的数值。
- count/sum 的非精确结果明确限定为已确认范围及 at_least；没有有效下限、负数求和截断（如已见 +10 而遗漏 -20）、无法去重等情况 computed 为 null，说明不足原因。不能把“部分和”统一标作下限。

### 3.4 召回输出（给宿主） `recall.v11`

```json
{
  "version": "recall.v11",
  "request": { "request_id": "op-17", "query": "……", "context": [], "task_id": null, "at": 1790740800 },
  "status": "answered | context_only | insufficient | conflict | needs_clarification | degraded",
  "current_message": "CurrentMessage?",
  "records": [ "RecordView" ],
  "raw": [ "RawEvidence" ],
  "facts": [ "RecallFact" ],
  "computed": { "op": "count", "value": "3", "unit": null, "bound": "exact | at_least", "items": ["ComputeItem"], "operands": [], "rounding": null, "exact_days": null, "approximate": null, "request_time": null },
  "history": [ { "statement": "……", "until": "2023-05-30", "evidence": [ ] } ],
  "conflicts": [ "RelationLink" ],
  "clarification": null,
  "semantic_judgment": "model | skipped | failed",
  "completeness": { "retrieval_truncated": false, "pending_related_sources": 0, "pending_updates": 0, "enrichment_gaps": 0, "plan_complete": true },
  "retrieval": { "status": "disabled | not_needed | ready | pending | unavailable", "sources_total": 0, "sources_indexed": 0, "segments_total": 0, "segments_indexed": 0 },
  "degraded_reasons": [],
  "snapshot": { "read_revision": 42, "source_revision": 57 }
}
```

- 仅 bootstrap 且 `statement_snapshot.enabled=true` 时，增加可选 `statement_snapshot={records:[CurrentRecordView],snapshot:{read_revision,source_revision},read_at,unfinished_sources,coverage_unknown:true,truncated,notice}`；关闭或普通 Recall/Prepare 时省略该字段。read_at 为实际读快照的 Unix 秒，不代替来源 said_on 或问题 at。records 保留完整原句、Candidate（含 polarity、speech、限定语、times）、grade/update、来源证据与 nullable said_on，不生成 facts/current/applies；notice 明示“已存陈述，非本轮判断”。该字段不进入 R1/R4 输入、不追加到 global_context。
- 快照只选可信 owner/主体/任务范围内 subject=self、Active、committed、update=judged、无开放关系、有获准且未删除原话及可定位依据的 CurrentRecordView；legacy、状态未知、pending 不入选。关系白名单为 response_language、response_style、code_example_language、likes、avoids（这五项限 preference/constraint）、job_title、works_at、lives_in、allergic_to。不按关键词扩大健康范围，不按相同槽位推导唯一当前值。计划、否定、多值及限定语原样保留；Active 不证明现在仍成立。
- 快照默认关闭；默认最多 8 条、完整字段 JSON 16384 字节，可配置范围分别为 1–32、512–65536，条数还受本次 limit 限制。按 observed_at 降序、ID 升序择取整条记录，不能装入的记录丢弃并标 truncated，不截断原句/限定语。unfinished_sources 计同一读快照可信范围内全部未完成来源（含失败、pending、覆盖缺口、未知状态），不声称与某问题相关，也不受白名单/条数限制；已 committed 或 completed_no_facts 不计。coverage_unknown 永远 true，不因空值或零未完成来源改为完整。
- 快照仅允许“按记录你曾说……”的带出处陈述。问当前状态、条件适用、完整清单或本轮出现新相关陈述时必须普通 recall；不得用快照绕过 R4 判断。使用新 bootstrap request_id 刷新，同参重放仍返回原读快照；删除、授权/任务限制及正文变化按现有安全规则阻止缓存、重放和发布。快照配置进入 bootstrap 请求绑定，不偷偷持久化快照正文。
- retrieval 是宿主独立元数据，不进入 R4 的 RecallCompleteness、模型请求或业务证据。disabled 表示关闭；not_needed 表示本次实际枚举或 bootstrap；ready 表示查询向量及相同 binding 的索引覆盖完整；pending 表示查询可用但索引覆盖不全；unavailable 表示查询或索引读取失败，本次使用 lexical-df。覆盖数计同一 R3 快照内、当前可信范围内链接到候选记录或尚无记录链接的正文来源/分段，sources_indexed 仅计分段全部核准的来源；分母 0 不代表事实范围完整。原文是否完整进入 raw 仍由 3.2 的 completeness 表达，与这组向量索引覆盖数分别报告。
- 不新增计划 status 或字段：records.candidate.speech 保留 plan，R4 的 statement 明示计划性质，宿主在 facts/history 中继续保留计划措辞、限定语及日期，不改说成已实现。
- 宿主按 `facts`、`computed`、`history`、`conflicts` 组织回答；`evidence` 是可引用的原文片段。无 R4 判断时从 records/current_message/raw 取得上下文，并明确未经本轮语义判断，不能自行把 Active 记录当作确定当前结论。
- request 由代码复制原始 query/context/task_id/at 并绑定稳定 request_id；复用结果时核对同一请求及有效快照。来源 ID 不代替请求绑定，只读请求可仅通过 request_id 关联预算。
- RecallFact 为 R4 的 FactView 加代码派生的 grade（committed/uncommitted）和 update（UpdateState 或 null），不删除 polarity、strength、uncertainty、conditions、times 或 applies。update 有 pending 时优先合并并去重其原因；否则，相关引用中有未知状态或仅来自未整理原文时为 null，全部相关引用有明确 judged 状态时才为 judged。null 表示未知而非完成。原文直接支持的事实仍可呈现，但宿主必须保留 pending/未知标注，不擅自声称唯一当前值。Computed 可空；count/sum/elapsed 的 value 为有限十进制文本，compare 的 value 为 `less | equal | greater`，只有精确可比较时输出。bound 由代码按 3.3 产生，不由模型自报；items/operands 保留可追溯输入，不能仅列消息 ID。
- `grade` 只做标注，代码不据此改变 `status`。
- R4 已通过完整核验时，主体澄清由其 verdict 决定；R1 的初步歧义不单独覆盖该 verdict。其他 conflict/insufficient、可见性和计算门控继续适用；没有 facts 不自动 answered，有 facts 也不自动免除真实歧义。
- semantic_judgment = skipped/failed 时 facts=[]、computed=null，不新增本轮事实判断；已整理及已核验记录通过 records 原样返回，当前及相关待整理原文仍通过 current_message/raw 返回。stored_status=active 不能包装成 current=yes；有未覆盖新证据时不给无条件旧当前值。模型判断为 unknown 的当前资格或适用性不改为肯定默认值。
- history 区分曾经成立与被纠正/撤回的记录；纠错旧值仅作为审计记录保留，不作为历史事实输出。until 可空，日期未知不回填接收日；clarification 可空，非空为 `{ "question": string, "evidence": [EvidenceRef] }`。
- op 为 elapsed 时，computed 另带代码按 3.3 计算的 `rounding`、`exact_days`（整数天数 d）与 `approximate`；其他 op 这三个字段为 null。模型输出不含这些字段，也不能覆盖它们。
- computed 的任一操作数为 `@request.at` 时，computed 另带代码生成的 `request_time = { "at": Timestamp, "local_date": LocalDate, "tz_offset_seconds": integer, "precision": "day" }`：at 等于 request.at，local_date 为按 tz_offset_seconds 换算的本地日期；否则 request_time 为 null。它不属于 items，也没有 evidence。
- 当前目标默认且唯一受支持的召回输出为 recall.v11。旧路径未完整投入生产，不设置 answer_contract 兼容窗口；旧版本请求返回 contract_version_unsupported，不静默升级。旧数据读取仍遵循 2.6/3.2，详见第 7 节。

### 3.5 `memory_prepare` 返回结构

其 recall 段沿用 3.3/3.4 的终审规则：R1 为初步判断，只有完整核验通过的 R4 可判定是否仍需主体澄清。跳过/失败不新增确定事实，也不自动绑定主体；source/enrichment 的保存与整理状态不因召回判定改变。

```json
{
  "source": { "status": "stored | rejected | failed | deleted", "source_id": null },
  "enrichment": { "status": "queued | running | committed | completed_no_facts | no_candidates_unreviewed | coverage_gaps | pending | awaiting_confirmation | failed | needs_rejudge | budget_exhausted | not_requested | deleted", "candidates": [], "memory_requests": [], "retention_pending": false },
  "recall": { "…": "recall.v11" }
}
```

enrichment 描述后台状态；candidates 各项为 `{ "index": integer, "status": "ok | unresolved | rejected | pending | committed", "memory_id": MemoryId?, "update": UpdateState?, "reasons": [string] }`。已提交断言为 status=committed、memory_id 非空，可以同时 update.pending；未提交候选或找不到目标的 retraction 为 status=pending、memory_id=null。未进入更新判定时 update=null，不暗示 judged。enrichment.status=committed 只说明事实提交，不说明所有更新关系已确定；未提交的待判定候选用 pending，混合结果逐项呈现。模型发现的限制是疑似请求，宿主确认结果作为控制记录独立保留，不由后续提取改写。

例如，已提交但范围未覆盖的断言，与找不到目标而尚未执行的撤回，分别返回：

```json
[
  { "index": 0, "status": "committed", "memory_id": 21, "update": { "status": "pending", "reasons": ["update_scope_incomplete"] }, "reasons": [] },
  { "index": 1, "status": "pending", "memory_id": null, "update": { "status": "pending", "reasons": ["update_target_unknown"] }, "reasons": [] }
]
```

已获准保存的 retention 请求令 retention_pending=true，并返回对象、期限原话及尚未执行状态；不能只给一个布尔值掩盖对象未知。疑似漏传限制按 2.1 进入 awaiting_confirmation 并停止相关整理，不能继续 W5。确认后仅按新的明确授权恢复或通过现有 memory_forget 删除；是否删除及是否恢复都不由该标记自动决定。宿主无需为本轮回答等待后台完成。

source_id 在未保存时为 null，不伪造存储成功。宿主按临时规则跳过保存时根本不会调用 memory_prepare，服务端不会产生“被宿主跳过”的状态；这一情况只写在宿主说明中，由宿主改用只读召回。source rejected/failed 后需要召回时使用独立只读调用；只读当前上下文不触发持久写入。来源删除的迟到结果失效。

## 4. 状态与失败码

| 代码 | 含义 | 出现在 |
| --- | --- | --- |
| `w1_invalid_output` | 提取输出不是有效 JSON 或不符合 schema | enrichment |
| `candidate_rejected:<原因>` | 单个候选被 W2 拒收（片段不存在、关系不在目录、日期矛盾、截断后仍非原文） | enrichment.candidates |
| `unresolved_subject` | 主体待澄清 | enrichment.candidates |
| `review_contradicted` / `review_insufficient` | W3 不支持 | enrichment.candidates |
| `coverage_gaps` | W3 认为有漏提 | enrichment |
| `update_scope_incomplete` | 查找范围不足；合格事实可提交、明确目标动作可执行，其他关系保持 update.pending，不确认唯一当前值或伪造冲突 | candidates.update / records.update / facts.update；recall.completeness.pending_updates |
| `update_target_unknown` / `update_relation_unclear` | 目标或新旧关系未确定，标注 update.pending；合格断言可已提交，无目标撤回仍暂存 | candidates.update / records.update / facts.update |
| `awaiting_confirmation` | 疑似漏传保存限制/删除请求，相关整理暂停 | enrichment |
| `needs_rejudge` | W5 发现 W4 决定已过期 | enrichment |
| `budget_exhausted` | 预算用尽 | enrichment / recall.degraded_reasons |
| `model_timeout` / `model_invalid_output` | 调用超时或输出无效（记入预算，不自动重试） | 同上 |
| `semantic_judgment_skipped:<原因>` | R4 未调用：`context_only`、`no_evidence`、`offline`、`budget` | recall |
| `r1_not_understood` | R1 失败，按宽泛检索处理 | recall.degraded_reasons |
| `retrieval_truncated` | 检索或枚举被预算截断 | recall.completeness |
| `retention_pending` | 用户提出保存期限，尚未执行 | enrichment |
| `compute_insufficient` | 不能去重、归一单位、确定端点或建立有效界限 | recall.degraded_reasons |
| `contract_version_unsupported` | 请求的输出版本不再受支持，不静默换版本 | recall |

## 5. 预算

| 阶段 | 调用 | 上限 | 说明 |
| --- | --- | --- | --- |
| 前台（每个召回请求） | R1、R4 | 2 | bootstrap 与完全离线为 0；同参重放复用结果，不新增调用 |
| 后台（每条消息） | W1、W3、W4 | 3 | W1 为空时不调用 W3；未检索到相关记录且无未解决关系时跳过 W4，按 2.4 追加断言或暂存无目标撤回；范围不足仍标注；暂停不额外调用模型 |
| 向量 | — | 另计 | 第一阶段不启用 |

- 前后台分别记账，通过 source_id（可空）及 request_id/operation_id 关联，共享用户授权的总预算；没有持久来源的只读召回仍计入预算。
- 超时、无效输出都记入预算；失败后的重做和重新判定需要显式预算，不自动重试。
- 现有 `model_budget` 的阶段名需增加 `w1 / w3 / w4 / r1 / r4`，`max_operation_calls` 按前后台分开配置。

## 6. 不变量（每条对应一项契约检查）

1. 只有获准保存的消息进入 T1；完成的 T1 不因后续失败回滚，删除后不会被重建、晚到任务或重放恢复。
2. 候选中的每个 Quote 都逐字存在于其来源。
3. 否定、偏好强度、不确定性、条件、时间不因解析失败消失；按所属类型保留未知状态，不与原文没有该表达混同。
4. question、hypothetical、reported 不成为用户确认的事实；plan 仅成为带完整限定语的计划记忆，不冒充已发生事件。
5. 主体未解析时不会被自动绑定到目录中的实体。
6. W4 只修改输入中给出的、经过逐条比对且有双方依据的明确目标；范围不完整时可提交合格断言或执行明确目标动作，update.pending 持久保留并传至 R4/宿主，不确认唯一当前值或伪造冲突；重判已提交断言不重复插入或强化。
7. 被 `correction` 取代的旧值不会作为历史事实出现在召回中。
8. W5 提交前重查快照；过期决定不会覆盖新状态。
9. 普通召回的本轮消息总在宿主证据中，不依赖检索命中或 R4 成功；只读消息不为了取得 source_id 而持久保存。
10. 有相关的未整理新原文时，召回不会给出无条件的旧当前值。
11. R4 跳过或失败时不新增本轮 facts/current 判断；records、当前原文及相关 raw 仍可见。
12. 计算由代码执行且输入可追溯；只有确有数学依据才输出下限，未知符号的部分和、未确认独立的事件或缺端点的间隔不伪称精确/下限。
13. 模型调用都在写事务之外，且不持有阻塞前台的服务全局锁。
14. 前台每个召回请求最多 2 次生成式调用，后台每条消息最多 3 次；重放不新增调用。
15. 保存期限与对象持续可见，不被当作已执行；疑似限制先暂停并确认，明确删除走 memory_forget，不以事实撤回替代。
16. 未解决关系绑定双方及依据；来源删除使关联关系失效，解决依据可审计，不自动扩散删除。
17. 多目标依赖动作一起提交或失败，重放不重复新增记录/关系；未知有效时间不回填为接收时间，已结束但结束时间未知的记录在历史查询中不被当作一直有效。
18. 返回结果绑定原始问题、上下文、任务和快照；兼容输出来自同一结果，不追加模型调用或产生第二份权威判断。
19. legacy 记录不补造本版 Candidate、Quote 或来源 ID；缺可重建来源、失败或预算不足时仍保留获准旧记录，切换只替代已核验映射部分，不双返回同一替代记录。

## 7. 设计决策与迁移退出条件

| 事项 | 决定 | 实施约束 |
| --- | --- | --- |
| `strength`（偏好强弱） | 第一阶段保留偏好强度及原句 | 当前 same_assertion 依赖此字段；旧记录原样迁移。以后取消枚举须先调整比较/强化规则、保留原话并单独验收 |
| 未解决关系的存储 | 沿用 owner 快照，新增 RelationLink 集合 | 保存成员、范围、依据和解决状态；与 Memory 生命周期分开，按 2.4 迁移旧快照及处理删除 |
| `memory_type` | 由 W1 给出，W3 核验，代码检查目录 | 保留排序与准入；Episode 的跨轮价值按 2.3 判断，不盲目设置旧字段默认值 |
| 现有记忆迁移 | 默认切换与语义重建分开；旧数据持续经 LegacyRecordView 读取 | 重建另行按 2.6 授权、盘点、核验和映射；不自动转换旧任务，不无损适配或丢弃无来源记录 |
| 旧 `answer_contract` | 不设置兼容窗口；仅支持 recall.v11 | 客户端、宿主说明和 Validate 一并切换；旧版本请求返回 contract_version_unsupported，不静默升级 |

旧路径未完整投入生产，不再维护旧写入、召回行为及输出。执行器配置名仍为 write_pipeline=w4_runtime_v1、recall_pipeline=recall_v3（不是模型/输出契约号）；memory_validate_answer 只核对同进程保留的 recall.v11 请求及有效证据快照，不新增模型调用。旧数据、控制记录、删除墓碑和记账仍保留。

退出条件为：当前受支持客户端与宿主说明、共同事实/来源评分和 memory_validate_answer 已切换，同批契约用例、零模型/降级、旧库读取、删除及并发检查通过。满足后移除旧 answer_contract 适配及被替代执行路径；不以全部旧数据语义重建为前提。旧版本请求明确返回 contract_version_unsupported，不静默升级。M1 以用户确认的 common.v1 绝对结果为主，0517000 基线仅作参考；已知质量局限留账，不放宽安全阻断项。

## 8. 版本变更记录

AF331（2026-10-14）：W3 v4 → v5，仅收紧本地 gaps 非空核验；同步契约标识、阶段绑定和请求指纹，不增加动态数组长度 schema 或自动重试。W1/W4/R1/R4/recall/DB 不升版，历史录制不改写，旧检查点不能冒充 v5 核验结果。

| 日期 | 契约 | 变更 | 原因 |
| --- | --- | --- | --- |
| 2026-10-11 | recall.v10 → v11 | 非 self 的 R1 主体候选增加结构化实体引用及登记名字提及；两种 embedding 状态均生效，v10 明确拒绝 | AF295/AF296 用户批准；W/R 模型、DB、向量绑定不变，提及不认定语义 |
| 2026-10-11 | recall.v9 → v10 | 混合检索小范围按完整主体范围记录数枚举；DF 保留单来源词元；v9 请求明确拒绝 | AF288 用户批准；W/R 模型、DB、向量绑定不变；失败原因仅进入固定类别 trace |
| 2026-10-11 | recall.v8 → v9 | opt-in bootstrap 已存陈述快照，完整记录、读版本/时间、可信范围未完成来源数与覆盖未知；v8 请求明确拒绝 | AF283 用户批准；W/R 模型契约、提示词/schema、DB v11 不变，默认关闭且不附加到 global_context，历史数据/录制不改写 |
| 2026-10-10 | W5 提交规则（2.5，无独立版本号） | 已核验的 user_statement+fact 否定断言投影为 Negative 并按既有准入条件提交；肯定仍为 None，否定 episode、unknown、plan、retraction 不变 | AF216 用户批准；不改提示词/schema、阶段/recall 版本或 DB v11，不自动重整理旧 pending，旧数据和身份检查保持不变 |
| 2026-10-10 | W1/W3 v3 → v4；W4 v4 → v5；R4 v6 → v7；recall v7 → v8 | 通用 fact value 改为连续断言定位片段，保留完整 sentence；兄弟记录不借用提交等级/更新状态，单候选整句不证明全来源已整理 | AF206 用户批准；代码仅核类型、引用、包含和身份，语义由模型判断；旧整句数据不改写，R1 v2、DB v11 不变，旧录制严格过期 |
| 2026-10-09 | recall.v6 → v7；DB 10 → 11 | opt-in 混合检索及独立 retrieval 状态/覆盖数；来源分段缓存、失效与本机后台维护 | AF183/AF185 用户批准；R1 v2、R4 v6、W 契约不变，关闭检索时模型指纹/业务证据不变，旧 recall 请求明确拒绝 |

| 2026-10-07 | `w1.v2/w3.v2` → v3；`r1.v1` → v2；`r4.v5/recall.v5` → v6 | 无年份日期按所属来源可信时间与事件方向解释，明确年份/上下文优先；内部矛盾且无法择真时 W3 判 insufficient，无关候选独立核验 | AF88、AF90 用户批准；代码不补年、不判方向，W4 v4、DB 10 不变；旧数据可读、旧录制按原指纹保留，不自动重新定年 |
| 2026-10-07 | W5 提交规则（2.5，无独立版本号） | 无冲突、无其他业务变化的纯新增实体只豁免无来源屏障推进，revision 仍递增；其他失效与阶段绑定检查不变 | AF78 用户批准；不升 DB、模型或 recall 协议，不自动重判或绑定 unresolved |
| 2026-10-06 | `r4.v4/recall.v4` → v5；R1 v1 不变 | R1 主体歧义为初步判断，R4 按授权证据终审；开放列举不强制选人，真实歧义仍澄清；W1 角色缺省 low | AF56 批准方向，本轮措辞待用户确认；DB 仍 v10，旧请求不静默升级，旧录制按原指纹保留 |
| 2026-10-06 | `w1.v1/w3.v1` → v2；`w4.v3/r4.v3/recall.v3` → v4；R1 v1 不变 | 支持计划提交、区分预订/实现、计划修改/取消/纠错与限定语呈现；批准主体确认/代际重建、历史补录出处、未知 said_on（③④实现） | AF39、AF41 用户批准；契约/schema/输入绑定与请求指纹同步更新，旧录制不冒充新版；本步不升 DB，DB 10 留第③步 |
| 2026-10-06 | 默认路径与迁移退出条件 | 默认切换为 w4_runtime_v1 / recall_v3；旧 answer_contract 请求不支持；旧数据读取与显式预算重建分开，不设置旧输出兼容窗口 | AD1–AD3、AF21 用户批准；DB v9、现行模型契约与 recall.v3 字段结构不变 |
| 2026-09-30 | `r4.v1` → `r4.v2` | elapsed 的结束端点可用保留操作数 `@request.at`，由代码按可信 request.at 与用户时区提供；不再接受以 `current` 来源加“相对 0 天”伪装提问时刻 | 共同判分视图四题试跑发现 v1 无法合规表达“距今”端点（common-scoring-view.md 第 8 节） |
| 2026-09-30 | `recall.v1` → `recall.v2` | computed 增加 `request_time`；version 字段为 `recall.v2` | 随 r4.v2 同步 |
| 2026-09-30 | `r4.v2` → `r4.v3` | elapsed 的取整口径固定并由代码执行：天为自然日差，周为最近整数周（与旧实现一致），月、年为完整日历月、年；两端点须有日精度，不把月份补成日期 | 取整规则影响计算语义；此前契约未规定，评分器不能自行决定（四题试跑复核） |
| 2026-09-30 | `recall.v2` → `recall.v3` | elapsed 的 computed 增加代码生成的 `rounding`、`exact_days`、`approximate`；version 字段为 `recall.v3` | 宿主据此表述“约 N 周（d 天）”；随 r4.v3 同步 |
| 2026-09-30 | W5 提交规则（2.5，无版本号） | 同一原文的相关决定整组原子提交、整组重判或回滚；替代 open 关系成员且未解决关系时，关系与成员保持不变，替代记录标 update_relation_unclear 并经 history 带出该关系（不机械迁移成员） | W4 样本 w4_21：两个决定部分提交时败方会显得没有冲突；w4_22：两条冲突记录被同一新版本替代时，换 ID 会收敛成重复成员 |
| 2026-10-02 | `w4.v1` → `w4.v2` | conflict 决定增加 `relation_scope`（引用新候选或目标已核验的限定语，作为关系适用范围）；输入记录改用 `RecordView2`（完整记录增加结束时间 `end`，保留精度） | 用户裁定（B 段设计评审 0021、0022）：冲突范围是语义判断，应由 W4 给出，不由代码复制单侧限定语；W4 样本 w4_01 的“至 2026-08，月精度”在原 RecordView 中无法表达 |
| 2026-10-05 | `w4.v2` → `w4.v3` | 通用 fact 在授权主体、任务及读快照内实际穷举（含必要历史和关系闭包）、未超预算且覆盖已知时可 complete；FTS 未截断不算完整，已有 pending 仅经显式重判改变 | AF1 用户批准：小范围也永久 pending 阻止确认当前值；版本进入请求与录制指纹，旧录制不得冒充新判定 |

当前为 w1.v5、w3.v6、w4.v5、r1.v2、r4.v7、recall.v11；模型输出字段形状不变，r4/recall 继续使用不含 end 的 RecordView。AF206 改变共享 Candidate 的通用 fact value 语义并同步升版；历史整句记录原样可读，DB v11 不变。W1/W3/W4/R4 的提示词与请求版本变化使旧阶段录制按严格指纹过期，R1 v2 不变；不能为复用旧录制绕过指纹。混合检索开关仍只控制检索行为，AF206 不改变检索排名或向量绑定。更早契约录制按原指纹保留，旧数据可读，不自动重整理或重新定年。历史 59/4 仅作历史证据，不补齐新版回放缺口。

AF342：W1 v4→v5、W3 v5→v6，收窄提取及 coverage 范围；gaps 非空机械规则保持，W4/R1/R4/recall/DB 不升版。旧阶段检查点与录制严格按版本/提示词身份失效，已存数据不改写、不自动删除或重整理。
