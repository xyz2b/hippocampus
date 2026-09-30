# 共同判分视图 common.v1 四题试跑报告

自动部分只覆盖结构项；语义正确性（主体、关系、值的表述）依赖人工复核。“最终通过”只计自动通过项和台账中 **confirmed** 的复核通过项；proposed（草拟）不计。

新版一侧为手写 recall.v2 夹具（按 contracts.md 3.4 表达的正确输出），不是新实现的运行结果。lme_5e1b23de 的计算按已冻结的 r4.v2 / recall.v2 使用保留操作数 `@request.at`：提问时刻由代码按可信 request.at 与用户时区提供（computed.request_time）。

## 汇总

| 版本 | 判分项（分母） | 仅自动通过 | 含复核通过 | 题目仅自动通过 | 题目最终通过 | 待复核 | 阻断项（已定 / 待复核） |
|---|---|---|---|---|---|---|---|
| 旧基线 0517000 | 40 | 32 | 35 | 2/4 | 2/4 | 0 | 0 / 0 |
| recall.v2 夹具 | 40 | 31 | 40 | 0/4 | 4/4 | 0 | 0 / 0 |

**新旧比较结论：无退步、新版阻断项为零（新版为手写正确夹具，只说明判分流程成立，不代表新实现的效果）**（退步 0 项，未完成比较 0 项，新版阻断项 0 个、待复核的阻断类禁止项 0 个）。

## lme_59524333/q：What time do I usually go to the gym?

query_meta：`{"operation": "lookup", "time_focus": "current"}`

- 旧版：status `source_linked_model_answer` → answer；答案 `6:00 pm。`
- 新版：status `answered` → answer；facts 1 条，conflicts 0 个，computed 无

| 判分项 | 旧版 | 新版 | 转移 |
|---|---|---|---|
| `binding` | 自动通过 | 自动通过 | pass → pass |
| `status` | 自动通过 | 自动通过 | pass → pass |
| `facts[0].value` | 自动通过 | 待复核 → 通过（台账 confirmed: pass） | pass → pass |
| `facts[0].sources` | 自动通过 | 自动通过 | pass → pass |
| `facts[0].polarity` | 自动通过 | 自动通过 | pass → pass |
| `facts[0].current` | 自动通过 | 自动通过 | pass → pass |
| `facts[0].semantic` | 自动通过 | 待复核 → 通过（台账 confirmed: pass） | pass → pass |
| `conflicts` | 自动通过 | 自动通过 | pass → pass |
| `computed` | 自动通过 | 自动通过 | pass → pass |
| `forbidden[0]` | 自动通过 | 待复核 → 通过（台账 confirmed: pass） | pass → pass |
| 额外结论 new `extra:history[0]` | | | 待复核 → 通过（台账 confirmed: pass） |

旧版：分母 10，仅自动 10，含复核 10；新版：分母 10，仅自动 7，含复核 10。退步 0 项；未完成比较 0 项；新版阻断项 0 个，待复核的阻断类禁止项 0 个。

## lme_5e1b23de/q：How many months ago did I attend the photography workshop?

query_meta：`{"operation": "elapsed", "time_focus": "past"}`

- 旧版：status `source_linked_computed_elapsed` → answer；答案 `3 months ago（2023-11-01）。`
- 新版：status `answered` → answer；facts 1 条，conflicts 0 个，computed 有

| 判分项 | 旧版 | 新版 | 转移 |
|---|---|---|---|
| `binding` | 自动通过 | 自动通过 | pass → pass |
| `status` | 自动通过 | 自动通过 | pass → pass |
| `facts[0].sources` | 自动通过 | 自动通过 | pass → pass |
| `facts[0].polarity` | 待复核 → 通过（台账 confirmed: pass） | 自动通过 | pass → pass |
| `facts[0].semantic` | 待复核 → 通过（台账 confirmed: pass） | 待复核 → 通过（台账 confirmed: pass） | pass → pass |
| `conflicts` | 自动通过 | 自动通过 | pass → pass |
| `computed.value` | 待复核 → 通过（台账 confirmed: pass） | 自动通过 | pass → pass |
| `computed.endpoints` | 缺失 `format_cannot_express` | 自动通过 | missing(format_cannot_express) → pass；新增正确覆盖 |

旧版：分母 8，仅自动 4，含复核 7；新版：分母 8，仅自动 7，含复核 8。退步 0 项；未完成比较 0 项；新版阻断项 0 个，待复核的阻断类禁止项 0 个。

## unresolved_conflict/conflict_q2：栖风读书机的屏幕到底是几英寸？

query_meta：`{"operation": "lookup", "time_focus": "current"}`

- 旧版：status `conflicting_records` → conflict；答案 `（无断言）`
- 新版：status `conflict` → conflict；facts 0 条，conflicts 1 个，computed 无

| 判分项 | 旧版 | 新版 | 转移 |
|---|---|---|---|
| `binding` | 自动通过 | 自动通过 | pass → pass |
| `status` | 自动通过 | 自动通过 | pass → pass |
| `conflicts[0].members[0].sources` | 缺失 `format_cannot_express` | 自动通过 | missing(format_cannot_express) → pass；新增正确覆盖 |
| `conflicts[0].members[0].value` | 缺失 `format_cannot_express` | 待复核 → 通过（台账 confirmed: pass） | missing(format_cannot_express) → pass；新增正确覆盖 |
| `conflicts[0].members[1].sources` | 缺失 `format_cannot_express` | 自动通过 | missing(format_cannot_express) → pass；新增正确覆盖 |
| `conflicts[0].members[1].value` | 缺失 `format_cannot_express` | 待复核 → 通过（台账 confirmed: pass） | missing(format_cannot_express) → pass；新增正确覆盖 |
| `computed` | 自动通过 | 自动通过 | pass → pass |
| `forbidden[0]` | 自动通过 | 自动通过 | pass → pass |
| `forbidden[1]` | 自动通过 | 自动通过 | pass → pass |

旧版：分母 9，仅自动 5，含复核 5；新版：分母 9，仅自动 7，含复核 9。退步 0 项；未完成比较 0 项；新版阻断项 0 个，待复核的阻断类禁止项 0 个。

## person_sketch_pen/person_q4：冯序宁画速写时固定用哪种笔？

query_meta：`{"operation": "lookup", "time_focus": "current"}`

- 旧版：status `source_linked_fact` → answer；答案 `钢笔。`
- 新版：status `answered` → answer；facts 1 条，conflicts 0 个，computed 无

| 判分项 | 旧版 | 新版 | 转移 |
|---|---|---|---|
| `binding` | 自动通过 | 自动通过 | pass → pass |
| `status` | 自动通过 | 自动通过 | pass → pass |
| `facts[0].value` | 自动通过 | 待复核 → 通过（台账 confirmed: pass） | pass → pass |
| `facts[0].sources` | 自动通过 | 自动通过 | pass → pass |
| `facts[0].polarity` | 自动通过 | 自动通过 | pass → pass |
| `facts[0].current` | 自动通过 | 自动通过 | pass → pass |
| `facts[0].conditions` | 自动通过 | 自动通过 | pass → pass |
| `facts[0].uncertainty` | 自动通过 | 自动通过 | pass → pass |
| `facts[0].semantic` | 自动通过 | 待复核 → 通过（台账 confirmed: pass） | pass → pass |
| `conflicts` | 自动通过 | 自动通过 | pass → pass |
| `computed` | 自动通过 | 自动通过 | pass → pass |
| `forbidden[0]` | 自动通过 | 自动通过 | pass → pass |
| `forbidden[1]` | 自动通过 | 待复核 → 通过（台账 confirmed: pass） | pass → pass |

旧版：分母 13，仅自动 13，含复核 13；新版：分母 13，仅自动 10，含复核 13。退步 0 项；未完成比较 0 项；新版阻断项 0 个，待复核的阻断类禁止项 0 个。

## 反例

| 反例 | 版本 | 题 | 探测 | 目标项结果 | 题目最终通过 | 阻断项 |
|---|---|---|---|---|---|---|
| ce_new_after_abstain | new | person_sketch_pen/person_q4 | 新版在旧版弃答处给错（炭笔、引用 person_s1） | `facts[0].sources` 自动判错；`forbidden[0]` 自动判错；`forbidden[1]` 待复核 | 否 | —；待复核：错误替换 |
| ce_new_condition | new | person_sketch_pen/person_q4~unbound | 条件扩大（丢掉未被问题限定的“画速写时”） | `facts[0].conditions` 自动判错；`forbidden[0]` 自动判错 | 否 | 条件扩大 |
| ce_new_condition_control | new | person_sketch_pen/person_q4 | 对照：条件已被问题限定时省略不算扩大 | `facts[0].conditions` 自动通过 | 否 | —；待复核：错误替换 |
| ce_new_conflict_answered | new | unresolved_conflict/conflict_q2 | 冲突题给出确定值 8 英寸（current=yes） | `status` 自动判错；`forbidden[1]` 待复核；`conflicts[0].members[0].sources` 缺失 | 否 | —；待复核：错误替换 |
| ce_new_current | new | lme_59524333/q | 错误当前结论（旧值 7 pm 作为 current=yes） | `facts[0].sources` 自动判错；`forbidden[0]` 待复核；`facts[0].semantic` 待复核 | 否 | —；待复核：错误替换 |
| ce_new_duplicate_item_id | new | lme_5e1b23de/q | 计算条目前插入同名 workshop 的错误日期条目 | `computed.value` 自动判错；`computed.endpoints` 自动判错 | 否 | — |
| ce_new_elapsed_bad_quote | new | lme_5e1b23de/q | 计算起点的时间原文不在来源中 | `computed.endpoints` 自动判错 | 否 | — |
| ce_new_elapsed_endpoint | new | lme_5e1b23de/q | 计算值为 3 但起点错成 2023-11-04（与端点不一致） | `computed.value` 自动通过；`computed.endpoints` 自动判错 | 否 | — |
| ce_new_elapsed_no_evidence | new | lme_5e1b23de/q | 计算起点删除原文依据 | `computed.endpoints` 自动判错 | 否 | — |
| ce_new_elapsed_source | new | lme_5e1b23de/q | 计算值与日期都对，但起点来源错成 s1_t1 | `computed.endpoints` 自动判错 | 否 | — |
| ce_new_elapsed_validity | new | lme_5e1b23de/q | 计算起点把事件时间改成有效期角色 | `computed.endpoints` 自动判错 | 否 | — |
| ce_new_extra_forbidden | new | person_sketch_pen/person_q4 | 正确事实旁附带错误事实（引用 person_s1） | `forbidden[0]` 自动判错；`extra:facts[1]` 待复核 | 否 | —；待复核：错误替换 |
| ce_new_extra_member | new | unresolved_conflict/conflict_q2 | 冲突组追加错误的 99 英寸成员 | `conflicts[0].members[0].value` 自动通过；`extra:conflict[0].member:rec[conflict_s1]99` 待复核 | 否 | — |
| ce_new_extra_unlabeled | new | lme_59524333/q | 正确事实旁附带错误事实（结构上无禁止项命中） | `extra:facts[1]` 待复核 | 否 | —；待复核：错误替换 |
| ce_new_fake_quote | new | person_sketch_pen/person_q4 | recall.v2 事实引用片段不在原文中 | `facts[0].sources` 自动判错 | 否 | —；待复核：错误替换 |
| ce_new_fake_request_quote | new | lme_5e1b23de/q | 把提问时刻伪装成“相对 0 天”的事件原话（v1 夹具旧写法） | `computed.endpoints` 自动判错 | 否 | — |
| ce_new_forged_request_item | new | lme_5e1b23de/q | 伪造 ID 为 @request.at 的计算条目 | `computed.value` 自动判错；`computed.endpoints` 自动判错 | 否 | — |
| ce_new_history_extra | new | unresolved_conflict/conflict_q2 | 成员可自动判值，但 history 带错误断言“以前是 99 英寸” | `conflicts[0].members[0].value` 自动通过；`conflicts[0].members[1].value` 自动通过；`extra:history[0]` 待复核 | 否 | — |
| ce_new_member_deleted | new | unresolved_conflict/conflict_q2 | 冲突成员值合法，但 conflict_s2 来源已删除 | `conflicts[0].members[1].sources` 自动判错 | 否 | — |
| ce_new_negation | new | person_sketch_pen/person_q4 | 正确值出现在否定句中（polarity=negate） | `facts[0].polarity` 自动判错 | 否 | —；待复核：错误替换 |
| ce_new_request_operand_position | new | lme_5e1b23de/q | @request.at 放在起点位置 | `computed.endpoints` 自动判错 | 否 | — |
| ce_new_request_time_bad | new | lme_5e1b23de/q | 提问时刻的本地日期与可信 request.at 不符 | `computed.endpoints` 自动判错 | 否 | — |
| ce_new_source | new | person_sketch_pen/person_q4 | 错误来源（引用冯砚川的 person_s1） | `facts[0].sources` 自动判错；`forbidden[0]` 自动判错；`forbidden[1]` 待复核 | 否 | —；待复核：错误替换 |
| ce_new_source_newvalue | new | lme_59524333/q | statement 写新值 6 pm 但引用旧来源（来源错误，不应推断为替换） | `facts[0].sources` 自动判错；`forbidden[0]` 待复核 | 否 | —；待复核：错误替换 |
| ce_new_subject | new | person_sketch_pen/person_q4 | 错误主体（来源正确但断言冯砚川用钢笔） | `facts[0].semantic` 待复核 | 否 | —；待复核：错误替换 |
| ce_new_wrong_binding | new | lme_59524333/q | recall.v2 绑定到另一个问题 | `binding` 自动判错 | 否 | — |
| ce_new_wrong_version | new | lme_59524333/q | recall.v2 版本不符（给出 recall.v1） | `binding` 自动判错 | 否 | — |
| ce_old_abstain | old | person_sketch_pen/person_q4 | 旧版弃答（insufficient_evidence） | `status` 自动判错；`facts[0].semantic` 缺失 | 否 | — |
| ce_old_condition | old | person_sketch_pen/person_q4~unbound | 条件扩大（问题未限定画速写时，旧版只答钢笔） | `facts[0].conditions` 缺失；`forbidden[0]` 待复核 | 否 | —；待复核：条件扩大 |
| ce_old_conflict_answered | old | unresolved_conflict/conflict_q2 | 冲突题给出确定值 | `status` 自动判错；`forbidden[1]` 自动判错 | 否 | 错误替换 |
| ce_old_current | old | lme_59524333/q | 错误当前结论（旧值 7 pm） | `facts[0].sources` 自动判错；`forbidden[0]` 自动判错 | 否 | 错误替换 |
| ce_old_ellipsis | old | person_sketch_pen/person_q4 | 旧答案带省略号（“钢笔...”不能整串推定） | `facts[0].value` 待复核；`facts[0].current` 待复核 | 否 | —；待复核：错误替换 |
| ce_old_extra | old | person_sketch_pen/person_q4 | 正确事实旁附带错误事实 | `facts[0].value` 待复核；`facts[0].semantic` 待复核；`forbidden[1]` 待复核 | 否 | —；待复核：错误替换 |
| ce_old_fake_quote | old | person_sketch_pen/person_q4 | 旧答案引用片段不在原文中 | `facts[0].sources` 自动判错；`facts[0].current` 待复核；`facts[0].semantic` 待复核 | 否 | —；待复核：错误替换 |
| ce_old_negation | old | person_sketch_pen/person_q4 | 正确值出现在否定句中 | `facts[0].value` 待复核；`facts[0].polarity` 待复核 | 否 | —；待复核：错误替换 |
| ce_old_question_mark | old | person_sketch_pen/person_q4 | 旧答案带问号（“钢笔？”不能整串推定） | `facts[0].value` 待复核；`facts[0].polarity` 待复核；`facts[0].current` 待复核；`facts[0].uncertainty` 待复核 | 否 | —；待复核：错误替换 |
| ce_old_source | old | person_sketch_pen/person_q4 | 错误来源（值对，引用 person_s1） | `facts[0].sources` 自动判错；`forbidden[0]` 自动判错 | 否 | — |
| ce_old_subject | old | person_sketch_pen/person_q4 | 错误主体（答成冯砚川的炭笔） | `facts[0].value` 自动判错；`facts[0].sources` 自动判错；`forbidden[0]` 自动判错；`forbidden[1]` 自动判错 | 否 | 错误替换 |
| ce_old_unmapped_source | old | person_sketch_pen/person_q4 | 旧答案引用无法映射的来源 | `facts[0].sources` 自动判错；`facts[0].current` 待复核 | 否 | —；待复核：错误替换 |
| ce_old_wrong_binding | old | person_sketch_pen/person_q4 | 旧答案绑定到另一个问题 | `binding` 自动判错；`facts[0].semantic` 待复核 | 否 | —；待复核：错误替换 |
| ce_old_wrong_version | old | person_sketch_pen/person_q4 | 旧答案版本不是 v1 | `binding` 自动判错；`facts[0].polarity` 待复核 | 否 | —；待复核：错误替换 |

## 新旧成对反例（旧版弃答、新版给错）

- ce_old_abstain → ce_new_after_abstain：`facts[0].sources` 退步：新增语义错误（旧版弃答，新版给错）；`forbidden[0]` 退步：新增语义错误（旧版弃答，新版给错）
