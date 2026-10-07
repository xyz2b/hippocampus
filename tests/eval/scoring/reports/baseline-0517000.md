# 0517000 基线：原始评分与共同评分（common.v1，全部 63 题）

只有旧版一侧：新实现尚未完成，没有新版输出。全部离线，未调用模型。

- **原始评分**：tests/eval.rs 的旧口径（答案子串 + 来源），即严格回放的召回 42 / 拒答 21。
- **共同评分**：common.v1。“仅自动”只算结构检查和整串等值；“最终”另计台账中 confirmed 的复核结论，proposed 不计。
- **覆盖缺口**：旧格式无法表达的判分项（`format_cannot_express`），记为缺失、不补齐。

## 汇总

| 口径 | 结果 |
|---|---|
| 原始：召回 | 42 / 42 |
| 原始：拒答 | 21 / 21 |
| 共同：判分项 仅自动 / 最终 / 分母 | 459 / 483 / 505 |
| 共同：题目 仅自动通过 / 最终通过 | 53 / 54（共 63） |
| 共同：有待复核项的题 | 0（0 项） |
| 共同：有判错项的题 | 1 |
| 共同：有覆盖缺口的题 | 8 |
| 阻断项 | 0 |

## 按题型

| 题型 | 题数 | 原始通过 | 共同仅自动通过 | 共同最终通过 |
|---|---|---|---|---|
| 作答（存在旧值或他人/他物的值） | 23 | 23 | 22 | 23 |
| 证据不足 | 12 | 12 | 11 | 11 |
| 作答（取值） | 12 | 12 | 12 | 12 |
| 需澄清 | 8 | 8 | 8 | 8 |
| 日期计算 | 6 | 6 | 0 | 0 |
| 冲突 | 2 | 2 | 0 | 0 |

## 覆盖缺口（旧格式无法表达）

按判分项字段计：`computed.basis` 6，`computed.endpoints` 6，`conflicts.members.sources` 4，`conflicts.members.value` 4。

- 日期计算题：旧答案是一段文字，给不出取整口径、精确天数和结构化端点。
- 冲突题：旧格式只报告“存在冲突”，给不出冲突双方。
- 其余不可测范围：LongMemEval 的 27 个用例没有逐条入库期望，入库、漏提和错误更新在这 27 个用例上不可测（phase1-baseline 第 5 节），本报告只覆盖问答。

## 原始评分与共同评分不一致的题

- `lme_5e1b23de/q`：原始通过，共同未通过。缺失：computed.basis、computed.endpoints
- `lme_71017276/q`：原始通过，共同未通过。缺失：computed.basis、computed.endpoints
- `lme_af082822/q`：原始通过，共同未通过。缺失：computed.basis、computed.endpoints
- `lme_bcbe585f/q`：原始通过，共同未通过。缺失：computed.basis、computed.endpoints
- `lme_gpt4_6dc9b45b/q`：原始通过，共同未通过。缺失：computed.basis、computed.endpoints
- `lme_gpt4_b5700ca9/q`：原始通过，共同未通过。缺失：computed.basis、computed.endpoints
- `lme_gpt4_e072b769/q`：原始通过，共同未通过。判错：status、computed、extra:safe_answer（原文只说 2023-04-16 “I've just downloaded Ibotta … I want to make sure I get the most out of it”，下载不等于开始使用，开始使用的日期证据不足。原始 LongMemEval 期望“3 weeks ago”保留在 expect 中，报告需说明两种口径的差异；20 天约 3 周的算术本身正确。 状态为 insufficient 时仍可返回可信的补充事实（如“4月16日的消息里提到刚下载 Ibotta，首次使用日期没有记录”），作为额外结论进复核；若提到 20 天，须说明是距该条消息的时间，不能说成距下载或首次使用的精确时间。computed 保持 null。）
- `unresolved_conflict/conflict_q2`：原始通过，共同未通过。缺失：conflicts[0].members[0].sources、conflicts[0].members[0].value、conflicts[0].members[1].sources、conflicts[0].members[1].value
- `unresolved_conflict/conflict_q3`：原始通过，共同未通过。缺失：conflicts[0].members[0].sources、conflicts[0].members[0].value、conflicts[0].members[1].sources、conflicts[0].members[1].value

## 逐题

| 题 | 题型 | 旧答案 | 原始 | 共同 仅自动/最终/分母 | 待复核 | 缺失 | 判错 |
|---|---|---|---|---|---|---|---|
| `lme_0862e8bf_abs/q` | 证据不足 | insufficient_evidence | 通过 | 4/4/4 |  |  |  |
| `lme_10e09553/q` | 作答（取值） | 7 largemouth bass。 | 通过 | 9/9/9 |  |  |  |
| `lme_15745da0_abs/q` | 证据不足 | insufficient_evidence | 通过 | 4/4/4 |  |  |  |
| `lme_19b5f2b3_abs/q` | 证据不足 | insufficient_evidence | 通过 | 4/4/4 |  |  |  |
| `lme_29f2956b_abs/q` | 证据不足 | insufficient_evidence | 通过 | 4/4/4 |  |  |  |
| `lme_577d4d32/q` | 作答（取值） | 7 pm。 | 通过 | 9/9/9 |  |  |  |
| `lme_5831f84d/q` | 作答（存在旧值或他人/他物的值） | 15。 | 通过 | 10/10/10 |  |  |  |
| `lme_59524333/q` | 作答（存在旧值或他人/他物的值） | 6:00 pm。 | 通过 | 10/10/10 |  |  |  |
| `lme_5e1b23de/q` | 日期计算 | 3 months ago（2023-11-01）。 | 通过 | 4/7/9 |  | 2 |  |
| `lme_603deb26/q` | 作答（存在旧值或他人/他物的值） | 10 times。 | 通过 | 10/10/10 |  |  |  |
| `lme_6f9b354f/q` | 作答（取值） | a lighter shade of gray。 | 通过 | 8/8/8 |  |  |  |
| `lme_71017276/q` | 日期计算 | 4 weeks ago（2023-03-04）。 | 通过 | 4/7/9 |  | 2 |  |
| `lme_76d63226/q` | 作答（取值） | 55-inch。 | 通过 | 9/9/9 |  |  |  |
| `lme_7a87bd0c/q` | 作答（存在旧值或他人/他物的值） | 4 weeks。 | 通过 | 10/10/10 |  |  |  |
| `lme_a2f3aa27/q` | 作答（存在旧值或他人/他物的值） | close to 1300。 | 通过 | 5/11/11 |  |  |  |
| `lme_af082822/q` | 日期计算 | 2 weeks ago（2022-11-17）。 | 通过 | 4/7/9 |  | 2 |  |
| `lme_af8d2e46/q` | 作答（取值） | 7 shirts。 | 通过 | 9/9/9 |  |  |  |
| `lme_b6019101/q` | 作答（存在旧值或他人/他物的值） | 5 MCU films。 | 通过 | 12/12/12 |  |  |  |
| `lme_bc8a6e93_abs/q` | 证据不足 | insufficient_evidence | 通过 | 4/4/4 |  |  |  |
| `lme_bcbe585f/q` | 日期计算 | 4 weeks ago（2023-04-01）。 | 通过 | 4/7/9 |  | 2 |  |
| `lme_c8c3f81d/q` | 作答（取值） | Nike。 | 通过 | 9/9/9 |  |  |  |
| `lme_ccb36322/q` | 作答（取值） | Spotify。 | 通过 | 9/9/9 |  |  |  |
| `lme_e47becba/q` | 作答（取值） | Business Administration。 | 通过 | 8/8/8 |  |  |  |
| `lme_f4f1d8a4_abs/q` | 证据不足 | insufficient_evidence | 通过 | 4/4/4 |  |  |  |
| `lme_gpt4_6dc9b45b/q` | 日期计算 | 4 months ago（2021-06-01）。 | 通过 | 4/7/9 |  | 2 |  |
| `lme_gpt4_b5700ca9/q` | 日期计算 | 4 days ago（2023-04-06）。 | 通过 | 4/7/9 |  | 2 |  |
| `lme_gpt4_e072b769/q` | 证据不足 | 3 weeks ago（2023-04-16）。 | 通过 | 1/1/3 |  |  | status、computed、extra:safe_answer |
| `absolute_date_ranges/time_q1` | 作答（取值） | 17:30。 | 通过 | 9/9/9 |  |  |  |
| `absolute_date_ranges/time_q2` | 作答（取值） | 19:00。 | 通过 | 9/9/9 |  |  |  |
| `absolute_date_ranges/time_q3` | 证据不足 | insufficient_evidence | 通过 | 4/4/4 |  |  |  |
| `device_default_paper/device_q1` | 需澄清 | clarification_required | 通过 | 3/3/3 |  |  |  |
| `device_default_paper/device_q2` | 需澄清 | clarification_required | 通过 | 3/3/3 |  |  |  |
| `device_default_paper/device_q3` | 作答（存在旧值或他人/他物的值） | A4。 | 通过 | 11/11/11 |  |  |  |
| `device_default_paper/device_q4` | 作答（存在旧值或他人/他物的值） | B5。 | 通过 | 11/11/11 |  |  |  |
| `device_default_paper/device_q5` | 作答（存在旧值或他人/他物的值） | A4。 | 通过 | 11/11/11 |  |  |  |
| `device_default_paper/device_q6` | 证据不足 | insufficient_evidence | 通过 | 4/4/4 |  |  |  |
| `event_registration_fee/event_q1` | 需澄清 | clarification_required | 通过 | 3/3/3 |  |  |  |
| `event_registration_fee/event_q2` | 需澄清 | clarification_required | 通过 | 3/3/3 |  |  |  |
| `event_registration_fee/event_q3` | 作答（存在旧值或他人/他物的值） | 36元。 | 通过 | 11/11/11 |  |  |  |
| `event_registration_fee/event_q4` | 作答（存在旧值或他人/他物的值） | 58元。 | 通过 | 11/11/11 |  |  |  |
| `event_registration_fee/event_q5` | 作答（存在旧值或他人/他物的值） | 36元。 | 通过 | 11/11/11 |  |  |  |
| `event_registration_fee/event_q6` | 证据不足 | insufficient_evidence | 通过 | 4/4/4 |  |  |  |
| `explicit_correction/correction_q1` | 作答（取值） | 64GB。 | 通过 | 10/10/10 |  |  |  |
| `explicit_correction/correction_q2` | 作答（存在旧值或他人/他物的值） | 128GB。 | 通过 | 10/10/10 |  |  |  |
| `explicit_correction/correction_q3` | 作答（存在旧值或他人/他物的值） | 128GB。 | 通过 | 10/10/10 |  |  |  |
| `organization_borrow_limit/organization_q1` | 需澄清 | clarification_required | 通过 | 3/3/3 |  |  |  |
| `organization_borrow_limit/organization_q2` | 需澄清 | clarification_required | 通过 | 3/3/3 |  |  |  |
| `organization_borrow_limit/organization_q3` | 作答（存在旧值或他人/他物的值） | 3册。 | 通过 | 11/11/11 |  |  |  |
| `organization_borrow_limit/organization_q4` | 作答（存在旧值或他人/他物的值） | 5册。 | 通过 | 11/11/11 |  |  |  |
| `organization_borrow_limit/organization_q5` | 作答（存在旧值或他人/他物的值） | 3册。 | 通过 | 11/11/11 |  |  |  |
| `organization_borrow_limit/organization_q6` | 证据不足 | insufficient_evidence | 通过 | 4/4/4 |  |  |  |
| `person_sketch_pen/person_q1` | 需澄清 | clarification_required | 通过 | 3/3/3 |  |  |  |
| `person_sketch_pen/person_q2` | 需澄清 | clarification_required | 通过 | 3/3/3 |  |  |  |
| `person_sketch_pen/person_q3` | 作答（存在旧值或他人/他物的值） | 炭笔。 | 通过 | 13/13/13 |  |  |  |
| `person_sketch_pen/person_q4` | 作答（存在旧值或他人/他物的值） | 钢笔。 | 通过 | 13/13/13 |  |  |  |
| `person_sketch_pen/person_q5` | 作答（存在旧值或他人/他物的值） | 炭笔。 | 通过 | 13/13/13 |  |  |  |
| `person_sketch_pen/person_q6` | 证据不足 | insufficient_evidence | 通过 | 4/4/4 |  |  |  |
| `task_isolation/task_q1` | 作答（存在旧值或他人/他物的值） | 西班牙语。 | 通过 | 11/11/11 |  |  |  |
| `task_isolation/task_q2` | 作答（存在旧值或他人/他物的值） | 意大利语。 | 通过 | 11/11/11 |  |  |  |
| `task_isolation/task_q3` | 作答（存在旧值或他人/他物的值） | 西班牙语。 | 通过 | 11/11/11 |  |  |  |
| `unresolved_conflict/conflict_q1` | 作答（取值） | 7英寸。 | 通过 | 10/10/10 |  |  |  |
| `unresolved_conflict/conflict_q2` | 冲突 | conflicting_records | 通过 | 5/5/9 |  | 4 |  |
| `unresolved_conflict/conflict_q3` | 冲突 | conflicting_records | 通过 | 5/5/9 |  | 4 |  |
