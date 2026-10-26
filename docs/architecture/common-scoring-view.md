# Offline scoring / 离线评分（common.v1）

评分代码位于 `tests/eval/scoring/`。适配器隔离输出格式，标注不进入被测系统。
历史输出的重算不代表当前模型请求回放通过；以下说明评分语义与复核边界。

## 1. 目标与边界

- 同一道题的判分项、分母和规则对新旧版本完全一致；差异只来自各自的适配器。
- 适配器只读取：问题、上下文、`query_meta`、原始输出。不读 `expect` / `expect_common`。
- 不建自然语言解析规则。无法用结构字段或整串等值可靠判定的内容，一律进入人工复核。
- 报告必须写明：自动部分只覆盖结构项，语义正确性（主体、关系、值的表述）依赖人工复核。

## 2. 标注格式

原有 `expect` 保留不动。每道题新增两块。

### 2.1 `query_meta`（问题属性，不属于期望）

```json
"query_meta": {
  "time_focus": "current | past | at_time | unspecified",
  "operation": "lookup | list | count | sum | compare | elapsed"
}
```

- 只根据问题文本（及上下文）就能写出；抽查时核对这一点。
- `operation` 沿用 R1 `plan.op` 枚举。存在性问题归 lookup，由 polarity 表达。
- 时间与操作不互斥（例如"历史上换过几次"= past + count）。
- 用途：适配器解读旧答案的角度。`time_focus=current` 下明确、直接的旧答案按当前结论评分；答案本身写明"以前"、表达不确定或报告冲突时，不强设 `current=yes`。

### 2.2 `expect_common`

```json
"expect_common": {
  "label_status": "draft | reviewed",
  "scoring_version": "common.v1",
  "status": "answer | insufficient | clarify | conflict",
  "facts": [{
    "subject": "self",
    "relation_note": "去健身房的时间",
    "value_any": ["6:00 pm", "6 pm"],
    "polarity": "affirm",
    "current": "yes",
    "conditions": [{ "text": "画速写时", "bound_by_query": true }],
    "uncertainty": null,
    "sources": { "all_of": ["s2_t3"], "any_of": [] }
  }],
  "conflicts": [],
  "computed": null,
  "forbidden": [{ "kind": "value_as_current", "value_any": ["7:00 pm", "7 pm"] }],
  "not_measurable": []
}
```

字段约定：

- **省略 = 不检查；`null` / `[]` = 明确要求没有。** 只有出现该项本身就是错误时才写 `null`/`[]`，否则省略。例：原文有 "usually"，R4 可能输出频率限定，此时不能把相关限定写成 `[]`。
- `relation_note` 与 `annotator_note`（标注理由、原文出处、需抽查的疑点）只给人看，不参与自动判分，但作为标注内容进入复核缓存键。
- 限定语（conditions / uncertainty / 时间 / 强度）只在原文确有表达时标注；`bound_by_query` 标该条件是否已被问题限定。
- 来源用稳定引用（`s2_t3`），不用运行时 source_id。
- `forbidden` 列出的才自动判错（kind：`value_as_current` 旧值或他值作为当前结论；`wrong_value` 把干扰值作为本题答案；`wrong_subject_source` / `wrong_source` 答案以另一主体或无关来源为依据；`source_out_of_scope` 任何结论引用了其他任务的来源，属阻断项；`condition_broadened`；`value_in_negation`）。除 `source_out_of_scope` 全局检查外，禁止项只约束本题答案（与期望事实匹配的断言；无期望事实时为全部断言）；引用同一来源或提到该值的正确历史、补充上下文作为额外结论进复核，不自动判错。多数“不得引用某来源”由期望事实的来源检查覆盖，`wrong_source` 只用于答案不得依据的来源（如提问时尚不存在的来源）。未标注的额外结论不自动判错，进入复核。禁止项不以引用位置代替值：引用了旧来源只说明来源错误，不能据此认定"旧值被当作当前结论"。
- `conflicts`：冲突成员各自出现，均不得作为确定结论；旧值可作为冲突成员或历史出现。
- `computed`：核对依据来源、端点和精度（例：工作坊题须核对 2023-11-01 → 2024-02-01 的整日历月数，不能只匹配数字 3）。elapsed 的标注写明 `unit`、`value`、`rounding`、`exact_days`、`approximate`，与 r4.v3 的规则一致（天 `calendar_days`、周 `nearest_week`、月 `complete_calendar_months`）；评分器分三项判分：`computed.value`（op、数值、单位、bound，新旧同义）、`computed.basis`（取整口径、精确天数、近似标记，旧格式记 `missing / format_cannot_express`）、`computed.endpoints`（端点、依据，并按同一规则从端点重算）；旧版的人工复核只确认数值与语义。评分器尚未实现的检查不得标为“不可测”（该分类只用于标注不足），应先实现或进入复核。

## 3. 可自动判定的范围

只有以下三类自动判值：

1. 结构字段：输出与问题的绑定（`binding`：契约版本，问题、上下文、任务、提问时间逐项一致）、status、来源 / record_ids、polarity、current、conditions、conflicts 成员、`computed.value` 与端点。来源须可映射、未删除，引用片段须逐字存在于该来源原文；否则来源项自动判错。计算端点还核对时间角色、依据来源和时间原文。
2. 旧 `safe_answer` 只去掉首尾空白和**一个**句末句号（`。` `.` `．`）后，**整串**等于 `value_any` 中某项。问号、省略号、引号等有语义的符号保留（"钢笔？""钢笔..."都不等于"钢笔"）。只有绑定正确、引用全部有效的答案才据此推定肯定、当前（time_focus=current）和无不确定表达。
3. `forbidden` 中可由结构字段或整串等值确定的项；自由文本（recall.v3 的 `statement`、非整串的旧答案）作出的当前结论，其禁止项进入复核。

其余（包括 recall.v3 的 `statement`，以及 `"3 months ago（2023-11-01）。"` 这类不能整串等值的旧答案）进入人工复核。来源正确不能推出主体或关系正确：引用冯序宁的正确来源却断言"冯砚川用钢笔"，仍是错误，由复核判定。

## 4. 判分结果分类

每个判分项的结果为以下之一：

| 结果 | 含义 |
|---|---|
| 自动通过 | 结构检查通过；**不等于最终通过** |
| 自动判错 | 结构项或已标注禁止项命中 |
| 不可测 | 标签不足（`not_measurable` 或字段省略） |
| 待复核 | 适配器无法可靠判定 |
| 缺失 | 输出缺少必需内容；旧格式无法表达的项附标记 `format_cannot_express` |

另列"额外结论"（未被标注覆盖的断言），全部进复核：未匹配的事实、`history` 中的每一条断言、未标注的冲突以及冲突组中未标注的成员。引用无效来源的额外结论直接判错。

规则：
- 待复核、缺失都不能自动当作通过，也不从分母中剔除。
- **最终通过** = 结构检查通过 + 必要的语义项与额外结论复核通过。门槛只看最终通过。
- 报告同时给出"仅自动"与"含复核"两组数字；新旧版本评分项和分母一致。

## 5. 新旧比较与退步定义

- 按判分项逐项比较，各分项分别报告新旧转移（通过 / 判错 / 弃答 / 缺失 / 待复核 → 新版对应结果）。
- 任一侧某项仍待复核时，该项比较**未完成**，不计入"无退步"。存在未完成项，或阻断类禁止项仍待复核时，报告结论为"比较未完成，不能判定不退步"。
- 以下都算退步：
  - 旧版最终通过，新版未通过；
  - 旧版弃答（insufficient / clarify），新版给出错误结论（记为"新增语义错误"）。
- **验收阻断项在新版必须为零**，与旧版是否通过无关：来源越权、条件扩大、错误替换（旧值作为当前结论等）。旧版同样未通过不构成放行理由。
- 旧版缺失（含 `format_cannot_express`）的项，新版结果分别报告为"新增正确覆盖"或"新增错误"；字段出现本身不算进步，新版在这些项上填错同样计入新增错误。

## 6. 人工复核台账

复核结论存入固定台账，重跑时复用。缓存键必须随评分依据失效，由以下内容共同哈希：

- `scoring_version` 与适配器版本；
- 该题 `query_meta` 与 `expect_common` 的内容；
- 问题、上下文、提问时间、task_id；
- 被复核的输出内容：经适配后的相关项（含全部限定语、时间、强度、更新状态、证据等级）及其引用的原文片段；禁止项的复核还依赖它检查过的全部结论（事实、history、冲突成员、计算），这些全部入键；引用本轮消息时，本轮消息原文入键；
- 该题**全部**额外结论；
- 复核结论所依赖的语义状态（不必纳入整个证据包）：相关记录的存储状态与更新判定状态、涉及相关记录的冲突、来源有效性（是否删除 / 可链接）、相关反证（如旧版的 historical_or_conflicting、新版的 history / conflicts）、完整性信息（截断、待整理来源、待判定更新、enrichment 缺口）。

随机 / 运行时 ID（source_id、record_id）先映射为稳定引用再计算哈希。revision、耗时等不直接入键，但它们对应的语义状态必须通过上一条保留。例：引用片段未变、但新增了未解决冲突时，旧的"当前结论"复核不能复用。任一组成部分变化，旧结论即失效，须重新复核。
