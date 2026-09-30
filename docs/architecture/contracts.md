# 第一阶段契约（已冻结，待实现；r4、recall 为 v3，其余 v1）

本文是 [target-design.md](target-design.md) 第 5 节第 1 步“冻结契约和不变量”的产出：各模型环节的输入输出格式、代码核验规则、状态与失败码、预算，以及可测试的不变量。实现各环节时按本文生成对应的 Rust 类型和 strict JSON Schema；本文于 2026-09-30 冻结，各契约统一编号为 v1：`w1.v1`、`w3.v1`、`w4.v1`、`r1.v1`、`r4.v1` 及宿主输出 `recall.v1`。冻结前的多轮草稿版本号不再使用。冻结后任何改动须升受影响契约的版本号；这些均为目标契约，不表示当前代码已经实现。同日按此规则将 `r4`、`recall` 依次升为 v2（elapsed 的提问时刻端点改由代码提供）和 v3（elapsed 的取整口径与精确天数由代码给出），见第 8 节；其余契约仍为 v1。

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

- `role`：`event` 为事情发生的时间；`validity` 为事实成立的区间；`retention` 为用户说的保存期限（第一阶段只标记，不执行，见 2.1、3.5）。三者分别解释，不把事件时间当作保存期限。
- `anchor`：相对时间的起点，第一阶段固定为 `said_on`（原文接收日，用户本地时区）；后台任务也用接收日，不用执行日。
- `range` 的模型输出固定为包含首尾的 `start/end_inclusive`，不接受在 `end_inclusive` 中填写次日。代码按用户本地自然日只转换一次为左闭右开；“4 月 1 日至 14 日”的内部结束端点为 15 日零时。日历无效、倒置或溢出时不能修成另一日期。精度保留到原文给出的粒度，不从月份推造具体日。
- 无法换算（`unknown`、`anchored` 的锚点未知）时保留原文和 `unknown` 状态，**不丢弃**。
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
- 目录中找不到，或原文指代不明时，用 `unresolved` 保存原文指代，进入待澄清。不得因目录中只有一个候选就自动绑定。

### 1.5 值 `Value` 与数值 `Quantity`

| Value.kind | 必需字段 | 约束 |
| --- | --- | --- |
| `text` | `text: string`、`quote: Quote` | text 保存原文写法；通用 fact 为连续完整原句 |
| `integer` | `integer: i64`、`quote: Quote` | 数字含义由模型提出，代码检查表示、范围及原文依据，W3 核验数值归属 |
| `boolean` | `boolean: bool`、`quote: Quote` | 肯定或否定的哪个属性由 W3 核验，不仅检查原文有无“是/否” |
| `entity` | `entity_id: string`、`quote: Quote` | ID 在实体目录内；原文需支持该对象，不靠目录存在即认定 |

计算用 `Quantity = { "amount": string, "unit": string? }`。amount 是有限十进制文本（如 `"32"`、`"-10"`、`"1.5"`），代码核验合法表示、单位换算、精度和溢出后计算；无单位计数用 null。不把格式化、舍入或单位换算交给模型；不能可靠归一的量返回不足。Value 与 Quantity 都保留原文依据，不从 quoted text 中临时新增自然语言推断规则。

### 1.6 更新判定状态 `UpdateState`

`UpdateState = { "status": "judged | pending", "reasons": ["update_scope_incomplete | update_target_unknown | update_relation_unclear"] }`，由代码根据 W4 决定及实际查找范围生成。judged 的 reasons 为空；pending 至少一个原因，无重复。范围不完整必含 update_scope_incomplete，即使对明确目标的动作已执行，也不代表其余关系已判断。judged 表示更新判定完成，不表示真实冲突已经解决（后者由 RelationLink 表达）。

事实是否提交由候选的提交状态表达，更新关系是否待判定由 UpdateState 表达；允许候选 status=committed 且 update.status=pending。UpdateState 及来源、候选索引、已提交记忆 ID 一起保存在现有决定回执，并随有效整理记录持久保留，不新增存储框架。普通召回、整理完成或重放不能清掉 pending；只有显式重判覆盖了对应缺口后才更新该状态。

## 2. 写入侧

### 2.1 W1 提取 `w1.v1`

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
- `speech` 枚举为 `user_statement | question | hypothetical | reported | plan`，按候选断言分类，不能因整条消息是问句就丢掉其中的个人陈述。非 user_statement 不冒充用户本人的确认事实；转述保留实际来源和原文。`memory_type` 仍由 W1 给出，代码核对关系目录允许的类型，W3 核验分类。
- `relation`：目录内的关系 ID，或通用关系 `fact`。`fact` 时 `value.quote` 与 `sentence` 相同，为一段连续的完整用户原句。
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
| 限定语与 Value | 逐字存在的未解析限定语保留；按 Value 分支核验值、实体及数值表示，不从缺失字段猜默认值 |
| 时间结构与日历合法性 | 结构合法但无法换算：保留原文与 `unknown`；日期与原文明确矛盾（如原文写 4 月 15 日，模型给 4 月 16 日）：拒收该候选 |

截断后的 candidate 必须重新执行全部 W2 检查，并以最终完整候选进入 W3；不能复用截断前的核验结果。拒收候选不影响其他独立候选；这条规则仅指已经获准且完成 T1 的原文，不覆盖 W0 的保存拒绝或后续删除。

### 2.3 W3 语义核验 `w3.v1`

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

代码核验：reviews 与输入候选一一对应、无重复，request_reviews 与送审请求一一对应；所有 Quote 逐字存在；supported 时 problems 为空。核验结果绑定最终候选、请求集合及来源快照，输入变化后不得沿用。

- supported 候选在主体、极性及其他提交条件满足时进入 W4 或 W5；待澄清候选保持候选状态。contradicted、insufficient 不提交，原文照常可用。W3 同时核验 memory_type、偏好强弱、限定语和请求所指对象，不能把无关 remember 请求当作全体候选的明确保存要求。
- Episode 的跨轮价值由同一次 W3 给出 episode_value：只有 useful、已核验且绑定该候选的 remember 请求，或 W4 判定为对既有记录的更新/撤回时，沿用现有经历准入；low_value/unknown 且无上述依据时仅保留原文。不因 low_value 提前跳过必要的 W4，准入在更新判定后执行。非 Episode 的 episode_value 为 not_applicable，不把旧 cross_turn_useful 默认为 true 来绕过检查。
- `coverage.status = gaps` 时，已支持的候选照常提交，缺口写入整理状态（`coverage_gaps`），召回时可见。
- W1 输出为空时不调用 W3，整理状态为 no_candidates_unreviewed：不代表原文没有可记内容，请求也不因此算核验通过。2.1 的疑似控制请求暂停规则仍独立生效；第一阶段不为了执行保存期限额外调用模型。

### 2.4 W4 更新判定 `w4.v1`

**何时调用**：W3 支持且可判定的候选中，存在相关记录或未解决关系时调用。断言未检索到任何相关记录时不调用 W4，以追加新记录进入 W5；范围不完整时（通用 `fact` 按本次检索取候选，不作全量保证）令 update.status=pending、reasons 含 update_scope_incomplete，不宣称它是唯一当前值。retraction 找不到目标时由代码保留 pending/target_unknown，不调用 W4，也不作为有值断言提交；不能把“没检索到”当作“没有旧记录”。

**相关记录（代码取得）**：在合法主体及任务范围内取同关系当前记录、未解决关系的成员及必要历史，加上全文检索的相关记录，合计不超过输入预算。目录内的专用关系（如 `lives_in`）和已知未解决关系不能被相似度截掉；超出预算如实报告。通用关系 `fact` 下同一主体的记录可能有数百条，不要求全部送入：范围定义为“本次全文检索取得的集合”，并在 coverage 中如实报告未覆盖全部（`complete = false`）。第一阶段不用向量检索。

**输入**：

```json
{
  "candidates": [ { "index": 0, "committed_id": null, "candidate": "Candidate", "source": "SourceRef", "said_on": "2023-05-30", "time_resolutions": ["TimeResolution"] } ],
  "records": [ "RecordView" ],
  "relations": [ "RelationLink" ],
  "coverage": { "complete": true, "omitted": 0 }
}
```

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
      "pending_reason": null
    }
  ]
}
```

代码核验：每个候选恰好一个决定，targets 无重复且只能为输入记录；decided 的 action 非空、pending_reason 为 null。new/coexist 的 targets 为空；reinforce 只有一个目标；state_change/correction/refine/retract/conflict 至少一个目标。basis 引用输入来源中的真实片段；真实冲突须有新旧双方依据，不能仅凭检索不足产生。action 为上述枚举或 JSON null（不是字符串 "null"）；effective 为 TimeInterp 或 null，非空时必须绑定新候选来源且在已核验时间中，只允许事件或有效期角色，不能使用保存期限；合法换算结果由代码提供，不由 W4 改写。

resolves_relations 是 W4 明确判定本次动作已解决的输入关系 ID，不能由代码看到一次更新就自动推断。只能引用 open 关系，成员及适用范围必须完整覆盖，basis 包含明确解决依据；pending/new/coexist/conflict 时该数组为空。动作与关系解决一起提交；依据保存在关系 resolution 及完整决定回执中。

pending 的 action 为 null，pending_reason 为 `target_unknown | scope_incomplete | unclear_relation`，targets 可以为空或仅列已定位目标，不修改旧记录。

committed_id 由运行时绑定，首次提交前为 null；重判已提交且 update.pending 的断言时为原记忆 ID，候选不与自身做新旧比较。W4 的 pending 表示不执行针对旧目标的动作，不等于 W3 已支持的断言不能保存；W5 的断言追加与更新状态按下述规则分别处理。

coverage 不完整时区分两类结论：

- **依赖“没有其他相关记录”的结论不成立**：不能据此确认唯一当前值，也不能因 retract 找不到目标就忽略。这类更新决定为 pending/scope_incomplete，不改判 conflict。满足 W3 与准入条件的 assertion 可追加为记录：候选提交状态为 committed，update.status 为 pending；主体、极性等尚未解析的候选及 retraction 仍只保留候选和原文，不借此绕过提交条件。
- **针对已明确定位目标的动作可以执行**：state_change、correction、refine、retract 的目标都在输入中、经过逐条比对且有新旧双方依据时，代码按常规核验后执行，只修改这些目标。对未送入记录的关系仍以 update.pending 持久标注；新记录也不能被包装成全局唯一当前值。R3/R4 按 3.2、3.3 带出该状态，不能仅靠“后台尚未完成”兜底，因为事实提交后缺口仍可能存在。

范围与快照校验由代码执行，不因模型返回 decided 而跳过。

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

`RelationLink = { "id": string, "kind": "conflict", "members": [MemoryId], "conditions": [TextQualifier], "times": [TimeInterp], "basis": [EvidenceRef], "status": "open | resolved | invalidated", "resolution": EvidenceRef? }`。真实 conflict 保存已核验新断言及其与旧目标的关系，当前资格由关系是否解决共同约束；不把冲突当作事实已被否定，也不只给整条 Memory 加 Conflicted 状态。成员至少两个，均存在于同一授权范围。

优先在现有 owner 快照增加关系集合，不新增数据库或存储框架；旧快照缺此集合时迁移为 []，快照格式版本与旧程序拒绝读取的规则同步调整。后续明确的 W4 判定或用户确认才能解决关系；解决依据持久保存。成员/依据来源删除时关系失效，不能通过关系恢复被删除内容，也不连带删除另一端仍获准保存的记录。

### 2.5 W5 提交（代码）

- 在写锁内重读，逐项检查：来源未删除、认领有效、目标记录状态与 W4 输入时一致、快照版本未失效。任一不满足，该条决定进入 `needs_rejudge`，不复用旧决定；committed_id 非空时同时检查它仍对应同一来源、候选和有效版本。
- 旧记录被 state_change 结束而 effective 未知时，结束时间存为未知，不回填接收时间。召回的历史查询不能把“结束时间未知”当作一直有效：现有检索把没有结束时间的记录视为持续成立，须改为对已结束且结束时间未知的记录标注 `end_unknown`，与新记录按接收顺序一起交给 R4 判断，不在代码中推定哪条在某个时间点成立。
- 独立候选分别提交；同一决定中存在依赖的动作（如撤回旧值并写入纠正值）一起提交或一起失败。
- 同一原文拆出、彼此依赖的多个决定（例如一个决定结束冲突败方并解决关系，另一个决定细化胜方）是一组，在同一 T2 中重查全部目标与关系快照后原子提交：任一决定 needs_rejudge 则整组重判，提交失败则整组回滚，不能只完成其中一部分。
- state_change、correction、refine 结束的旧记录若是 open 关系的成员，而本次没有决定解决该关系，替代它的新记录在同一 T2 中继承该关系成员身份（关系成员由旧记录换为替代记录，依据不变），冲突在召回中保持可见；不能因成员进入历史而让另一方显得没有冲突。此条与上条分别适用：原子提交处理同一原文的相关决定，成员迁移处理不解决关系的普通替代。
- W4 决定与现有引擎动作是语义对应，不能直接把新候选再交给旧确定性策略重判。复用存储与幂等检查，适配已验证决定的执行；现有 Refine 会结束旧版本、未知有效时间会回退到观察时刻，这两处必须修正。effective 未知时保留未知，接收时间仅用于来源顺序，不伪造现实终止日期。
- 多目标 state_change/correction/refine/retract 在同一 T2 中重查全部目标，结束全部授权目标并最多插入一次新版本；任一目标失效则整组 needs_rejudge，不能循环单目标入口产生多个新版本或部分成功。完整 targets/basis 保存在原文任务的决定回执；现有单个 supersedes 仅作兼容字段，不能作为完整关系依据。committed_id 已存在时复用该断言记录，按重判结果更新关系和状态，不再插入同一断言或把自身当作一次强化。若重判为 reinforce 到另一既有记录，按来源幂等归并证据，在决定回执保存已提交 ID 到保留目标的替代映射并退出重复投影；证据只计一次，不同时返回两份等效记录。
- conflict 的新成员与 RelationLink 一起提交；解决既有关系时同一 T2 重查关系及成员快照并更新状态。pending 不强化、不关闭旧记录；合格 assertion 的追加、UpdateState 和回执同一 T2 提交，retraction 或未满足提交条件者仅暂存。reinforce 仍检查独立用户证据，同轮重放不增加计数。
- 事务失败回滚 T2，原文（T1）不回滚。

### 2.6 现有记忆的迁移

现有记忆的条件主要是结构化字段，未必保留本契约要求的完整原句、TextQualifier 和 TimeInterp，不能无损适配为本版完整记录。现有 capture_source=false 路径也可提交事实（src/chat.rs），不能假定每条旧记忆都有独立 SourceRecord。第一阶段先盘点完整原文、旧回执、片段及原始保存授权，再决定哪些来源可重建；不把片段拼成整条原文，不因迁移扩大保存授权。

- 对具备可核对完整原文、原始接收时间/顺序及保存授权的部分，按 target-design 1.4 用 w1.v1、w3.v1、w4.v1 重新整理，作为显式、有预算的重建任务；先产生并核验新结果，再切换对应的有效整理版本，不先清空可用记忆。
- 用户确认、实体登记和删除墓碑等控制记录保留并约束重建；已删除的原文不参与重建。
- 重建前及缺可用来源而无法重建的旧记录，通过 3.2 的 LegacyRecordView 持续召回：保留现有值、方向、强度、结构化条件、时间和依据，缺口单列，不补造完整 Candidate 或新 Quote。不能因新版本启用、预算耗尽或重建失败而丢掉这些记录；已删除或未获准的内容仍不可返回。
- 旧/新记录的替代映射保存在重建回执；只有已核验替代的旧记录退出有效召回，不双返回已被替代的旧记录，也不删除未覆盖的 legacy 记录。确认与删除控制引用沿用该映射，重建不能改变其含义。
- 迁移验收用相同输入比较重建前后结果；实际迁移范围按可用授权来源确定，不缩成评测集。可重建、无法重建、失败、尚未完成的数量和模型调用预算在执行前单独报出。

## 3. 召回侧

### 3.1 R1 问题理解 `r1.v1`

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
- `subject` 可为 `unresolved`（原文指代）或 `{ "ambiguous": ["entity_a", "entity_b"] }`；此时召回返回需澄清，不擅自选择。
- `relation`、`conditions` 是自然语言描述，用于检索和给 R4 参考，不做字面过滤。
- `plan.op` 只能是列出的六种；`filter` 只能收窄范围，不能扩大访问范围（主体、任务由代码按可信上下文限定）。
- filter.relation 为目录内 ID 或 null，filter.time 为 TimeInterp 或 null，unit 为代码支持的输出单位或 null；比较和间隔所需的操作数由 R4 明确引用，不能执行自由文本表达式。

代码核验：实体在目录内；任务不超出可信范围；`time` 为合法 `TimeInterp`；`op` 在枚举内。失败时按“未理解”处理：做宽泛检索，`need` 视为 `judge`。

### 3.2 R2、R3 检索与证据组织（代码）

R3 交给 R4 的证据包：

`RecordView = CurrentRecordView | LegacyRecordView`，用 format 判别分支，不给 legacy 补造必填 Candidate：

- `CurrentRecordView = { "format": "current", "id": MemoryId, "stored_status": "active | superseded | expired", "termination_reason": SupersedeReason?, "ended": "not_ended | known | end_unknown", "candidate": Candidate, "evidence": [EvidenceRef], "grade": "committed", "said_on": LocalDate, "time_resolutions": [TimeResolution], "history": [MemoryId], "open_relations": [string], "update": UpdateState }`。
- `LegacyRecordView = { "format": "legacy", "id": MemoryId, "data": LegacyMemory, "evidence": [EvidenceRef], "grade": "committed", "update": UpdateState?, "gaps": ["source_missing | sentence_missing | qualifiers_unmapped | time_unmapped"] }`。LegacyMemory 冻结第一阶段开始前 src/model.rs 的 Memory 序列化形状，沿用旧格式解码并保留旧字段，不随新核心类型变化而改释；先执行相同的归属、任务与删除过滤，不把旧 scope 转成无依据的 TextQualifier。旧版没有更新判定状态时 update=null，表示未知而非 judged。

沿用现有 SupersedeReason 区分现实变化、纠错和撤回；Deleted 不进入任一分支。完整记录的 candidate.kind 为 assertion；用户确认保留独立控制记录及依据，不改写成不存在的完整用户原话。legacy 的 evidence 仅包含能链接到真实获准来源的引用；只有旧数据中的片段而没有可链接原文时 evidence=[]，片段留在 data 内，不能伪造 source_id 或作为新契约已核验的原文依据。gaps 汇入 enrichment_gaps，原文存在则同时交给 R4 核对；无法核准完整断言及限定语的 legacy 目标在 W4 保持 pending，不能以缺字段等于无限定来更新。

stored_status 与旧 data.status 均不是本轮确认的 current。CurrentRecordView.update 从持久回执/整理版本读取；提交后的 pending 仍进入 R3/R4 及宿主 records，不因 source.enrichment=committed 消失。

`RawEvidence = { "source_id": string, "quote": Quote, "said_on": LocalDate, "sequence": integer, "grade": "uncommitted", "enrichment": string, "memory_requests": [MemoryRequest] }`。enrichment 使用 3.5 的状态枚举；保留接收顺序及尚未处理的限制。模型不能因为 grade 改变事实是否可用。

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

### 3.3 R4 证据判断 `r4.v3`

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

- `statement` 是模型对事实的概括，**不要求逐字**；可核验的是 `evidence`。
- `supersedes_record_ids` 只表示本轮判断中新证据取代了旧记录，**不写入存储**；持久的版本关系仍由后台 W4 决定。
- exact 只在查询范围完整枚举、相关来源已覆盖、成员身份及所需输入明确时成立；代码综合 R2/R3 完整性与 R4 判断，模型自报 coverage=complete 不能覆盖代码已知的截断或遗漏，grade 不决定 exact。否则按操作分别处理：count 只计已确认不同的事件组，可以给有依据的下限；sum 只有已知未计部分非负等条件得到支持时才能给下限；未知符号或重复对象不能给下限；elapsed/compare 缺少端点、必要量或足够精度时返回不足，不输出猜测的数值。
- count/sum 的非精确结果明确限定为已确认范围及 at_least；没有有效下限、负数求和截断（如已见 +10 而遗漏 -20）、无法去重等情况 computed 为 null，说明不足原因。不能把“部分和”统一标作下限。

### 3.4 召回输出（给宿主） `recall.v3`

```json
{
  "version": "recall.v3",
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
  "degraded_reasons": [],
  "snapshot": { "read_revision": 42, "source_revision": 57 }
}
```

- 宿主按 `facts`、`computed`、`history`、`conflicts` 组织回答；`evidence` 是可引用的原文片段。无 R4 判断时从 records/current_message/raw 取得上下文，并明确未经本轮语义判断，不能自行把 Active 记录当作确定当前结论。
- request 由代码复制原始 query/context/task_id/at 并绑定稳定 request_id；复用结果时核对同一请求及有效快照。来源 ID 不代替请求绑定，只读请求可仅通过 request_id 关联预算。
- RecallFact 为 R4 的 FactView 加代码派生的 grade（committed/uncommitted）和 update（UpdateState 或 null），不删除 polarity、strength、uncertainty、conditions、times 或 applies。update 有 pending 时优先合并并去重其原因；否则，相关引用中有未知状态或仅来自未整理原文时为 null，全部相关引用有明确 judged 状态时才为 judged。null 表示未知而非完成。原文直接支持的事实仍可呈现，但宿主必须保留 pending/未知标注，不擅自声称唯一当前值。Computed 可空；count/sum/elapsed 的 value 为有限十进制文本，compare 的 value 为 `less | equal | greater`，只有精确可比较时输出。bound 由代码按 3.3 产生，不由模型自报；items/operands 保留可追溯输入，不能仅列消息 ID。
- `grade` 只做标注，代码不据此改变 `status`。
- semantic_judgment = skipped/failed 时 facts=[]、computed=null，不新增本轮事实判断；已整理及已核验记录通过 records 原样返回，当前及相关待整理原文仍通过 current_message/raw 返回。stored_status=active 不能包装成 current=yes；有未覆盖新证据时不给无条件旧当前值。模型判断为 unknown 的当前资格或适用性不改为肯定默认值。
- history 区分曾经成立与被纠正/撤回的记录；纠错旧值仅作为审计记录保留，不作为历史事实输出。until 可空，日期未知不回填接收日；clarification 可空，非空为 `{ "question": string, "evidence": [EvidenceRef] }`。
- op 为 elapsed 时，computed 另带代码按 3.3 计算的 `rounding`、`exact_days`（整数天数 d）与 `approximate`；其他 op 这三个字段为 null。模型输出不含这些字段，也不能覆盖它们。
- computed 的任一操作数为 `@request.at` 时，computed 另带代码生成的 `request_time = { "at": Timestamp, "local_date": LocalDate, "tz_offset_seconds": integer, "precision": "day" }`：at 等于 request.at，local_date 为按 tz_offset_seconds 换算的本地日期；否则 request_time 为 null。它不属于 items，也没有 evidence。
- 旧 `answer_contract`（其版本字段为 1，与 recall.v3 无关）仅在明确迁移窗口兼容，详见第 7 节；客户端指定契约版本，一次请求只有一个权威输出。附带的兼容输出不能覆盖所选版本的判断。

### 3.5 `memory_prepare` 返回结构

```json
{
  "source": { "status": "stored | rejected | failed | deleted", "source_id": null },
  "enrichment": { "status": "queued | running | committed | completed_no_facts | no_candidates_unreviewed | coverage_gaps | pending | awaiting_confirmation | failed | needs_rejudge | budget_exhausted | not_requested | deleted", "candidates": [], "memory_requests": [], "retention_pending": false },
  "recall": { "…": "recall.v3" }
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
4. `speech ≠ user_statement` 的候选不会成为用户本人的事实。
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
| 现有记忆迁移 | 可重建部分从授权原文重新整理，其余保留 legacy | 按 2.6 盘点、核验和映射后切换；缺字段走兼容分支，不无损适配或丢弃无来源记录 |
| 旧 `answer_contract` | 仅在一次明确的客户端迁移窗口提供兼容输出 | 客户端选择旧 answer_contract 或 recall.v3，默认值随发布配置明确，不静默改变；单次请求只有一个权威版本 |

旧 answer_contract 适配从同一次召回及同一快照机械产生，不重跑 legacy/assisted 模型链路，不新增调用。无法安全表达为旧 safe_answer/safe_source_ids 的复杂、未知或未提交结果，旧版保守返回不足；新版保留事实及证据，附带旧版不能否决新版。memory_validate_answer 同样按所选契约版本核对完整原问题、上下文、任务与来源。

迁移开始前登记当前受支持客户端及迁移发布里程碑。退出条件为：这些客户端和宿主说明已切换、评测按共同事实/来源口径切换、memory_validate_answer 完成迁移，并在同批契约用例及零模型/降级路径验收通过；满足后在登记的里程碑移除旧 answer_contract 适配及旧配置。若仍有受支持客户端未迁移，先明确更新里程碑，不无限期默认双返回；退出后旧版本请求返回 contract_version_unsupported，不静默升级。

## 8. 版本变更记录

| 日期 | 契约 | 变更 | 原因 |
| --- | --- | --- | --- |
| 2026-09-30 | `r4.v1` → `r4.v2` | elapsed 的结束端点可用保留操作数 `@request.at`，由代码按可信 request.at 与用户时区提供；不再接受以 `current` 来源加“相对 0 天”伪装提问时刻 | 共同判分视图四题试跑发现 v1 无法合规表达“距今”端点（common-scoring-view.md 第 8 节） |
| 2026-09-30 | `recall.v1` → `recall.v2` | computed 增加 `request_time`；version 字段为 `recall.v2` | 随 r4.v2 同步 |
| 2026-09-30 | `r4.v2` → `r4.v3` | elapsed 的取整口径固定并由代码执行：天为自然日差，周为最近整数周（与旧实现一致），月、年为完整日历月、年；两端点须有日精度，不把月份补成日期 | 取整规则影响计算语义；此前契约未规定，评分器不能自行决定（四题试跑复核） |
| 2026-09-30 | `recall.v2` → `recall.v3` | elapsed 的 computed 增加代码生成的 `rounding`、`exact_days`、`approximate`；version 字段为 `recall.v3` | 宿主据此表述“约 N 周（d 天）”；随 r4.v3 同步 |

| 2026-09-30 | W5 提交规则（2.5，无版本号） | 同一原文的相关决定整组原子提交、整组重判或回滚；替代 open 关系成员且未解决关系时，替代记录继承成员身份 | W4 样本 w4_21：确认冲突一方时两个决定若部分提交，败方会显得没有冲突 |

w1.v1、w3.v1、w4.v1、r1.v1 未变。旧版本的 r4/recall 输出不作为新版本的合规输出；评测夹具与适配器已同步到 v3。
