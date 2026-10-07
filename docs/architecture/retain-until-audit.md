# 附录：`retain_until` 误填统计

> 历史文档：下文保留原版本的实现描述、命令及结论，不是当前执行入口。旧写入及 legacy/local/assisted 召回已退出；当前路径、配置、备份与评测见 [架构索引](README.md)。0517000 仅作参考，不是上线门槛。

本附录为 [target-design.md](target-design.md) 1.3 节“历史观测”提供可复核的依据。

## 统计方法

- **数据**：提交 `da88878`（test(eval): re-record after the time-phrase and validation fixes）中 `tests/eval/cases/*.json` 的全部提取录制（`personal_memory_candidates_v1`），35 个用例、204 条用户消息。模型 DeepSeek-V4.1-Flash（接入点 `ep-20260923215039-vxgxg`），推理强度 medium，当时尚未设置 `temperature`。
- **口径**：只对可解析为 JSON 的提取输出逐个统计候选，计数 `memory_request.kind == "retain_until"` 的数量。204 条提取录制中有 202 条可解析，另两条（`lme_af082822 / s1_t2`、`lme_bcbe585f / s1_t1`）输出不是有效 JSON，单列为格式失败，不计入候选分母，也不视为成功或空候选。237 个候选来自这 202 条可解析输出；59/237 是候选层面的误填比例，不是用户消息的错误率。
- **复核方式**：对提交 `da88878` 的录制按上述口径计数，结果为 **59 / 237**。
- **“不是真实留存请求”的依据**：逐条对照所在消息原文（见下表）。另在全部 204 条用户消息中搜索“保留到、保存到、记到、只记、别记、不要记、忘掉、忘记、删掉、keep this until、remember this until、forget this、don't save / remember / keep、delete this”等保留或遗忘表达，没有命中。
- **第二次统计 59/231**：来自改写提示词后的一次录制。该录制因提示词改动被撤销，未进入仓库，只能作为历史记录，无法从仓库复核。

## 分类

| 类别 | 数量 | 说明 |
| --- | --- | --- |
| A 原文中没有该短语 | 39 | 模型写入的短语不在该消息中，包括照抄提示词示例的“保留到明天”，以及 `null`、`not_requested`、`current_date` 等字段值 |
| B 事件或状态的时间 | 18 | 短语确实出现在原文里，但描述事情何时发生或持续多久（today、last week、in April、recently、7/22 等），不是保留要求；包含此类时间的整句话也归入本类 |
| C 原文片段，但不是时间 | 2 | 短语出现在原文里，但是与时间无关的整句话或普通名词（如“钢笔”） |

分类由脚本初分（短语是否出现在原文中、长度），再逐条人工确认。三类合计 59 条，均不是真实的留存请求。

## 逐条明细

| # | 用例 / 消息 | 模型填写的期限短语 | 类别 | 原文上下文 |
| --- | --- | --- | --- | --- |
| 1 | lme_0862e8bf_abs / s1_t2 | `0.95` | A |  |
| 2 | lme_0862e8bf_abs / s1_t3 | `January 25th` | B | l of her blankets and beds on January 25th because they were getting sti |
| 3 | lme_0862e8bf_abs / s1_t3 | `今天` | A |  |
| 4 | lme_10e09553 / s2_t2 | `7/22` | B | when I was there with Alex on 7/22 - we caught 9 largemouth bass |
| 5 | lme_10e09553 / s2_t5 | `before our last fishing trip to Lake Michigan` | B |  and tactics, just like I did before our last fishing trip to Lake Michigan. Remember that trip when we c |
| 6 | lme_15745da0_abs / s1_t2 | `today` | A |  |
| 7 | lme_19b5f2b3_abs / s1_t2 | `tomorrow` | A |  |
| 8 | lme_19b5f2b3_abs / s1_t2 | `tomorrow` | A |  |
| 9 | lme_29f2956b_abs / s1_t1 | `daily` | B | cticing guitar for 30 minutes daily, and it's been helping me pro |
| 10 | lme_29f2956b_abs / s1_t2 | `2026-06-10` | A |  |
| 11 | lme_29f2956b_abs / s1_t2 | `2026-06-10` | A |  |
| 12 | lme_577d4d32 / s1_t3 | `not_requested` | A |  |
| 13 | lme_5831f84d / s1_t2 | `I've been listening to the podcast` | A |  |
| 14 | lme_5831f84d / s1_t2 | `I've been inspired by the stories of entrepreneurs` | A |  |
| 15 | lme_5831f84d / s2_t1 | `in the past few weeks` | B | atched 15 Crash Course videos in the past few weeks. |
| 16 | lme_5831f84d / s2_t5 | `null` | A |  |
| 17 | lme_5831f84d / s2_t5 | `null` | A |  |
| 18 | lme_5831f84d / s2_t5 | `null` | A |  |
| 19 | lme_59524333 / s1_t3 | `tomorrow` | A |  |
| 20 | lme_59524333 / s1_t3 | `tomorrow` | A |  |
| 21 | lme_59524333 / s1_t3 | `tomorrow` | A |  |
| 22 | lme_59524333 / s2_t3 | `I think Tuesday afternoon at 2:00 pm could work well.` | B | I think Tuesday afternoon at 2:00 pm could work well. I need to make sure I'm done  |
| 23 | lme_5e1b23de / s1_t2 | `保留到明天` | A |  |
| 24 | lme_603deb26 / s1_t3 | `明天` | A |  |
| 25 | lme_603deb26 / s1_t5 | `I'm also curious about using different types of citrus in my cocktails.` | C | I'm also curious about using different types of citrus in my cocktails. I've been experimenting with  |
| 26 | lme_603deb26 / s1_t5 | `I've been experimenting with grapefruit and lemon.` | A |  |
| 27 | lme_603deb26 / s2_t1 | `today` | A |  |
| 28 | lme_6f9b354f / s1_t1 | `recently` | B | ve in low-light conditions. I recently added a vase with fresh green |
| 29 | lme_7a87bd0c / s1_t4 | `2023-06-11` | A |  |
| 30 | lme_7a87bd0c / s2_t1 | `this weekend` | B | nning to clean out the garage this weekend, but I need some advice on ho |
| 31 | lme_7a87bd0c / s2_t1 | `this weekend` | B | nning to clean out the garage this weekend, but I need some advice on ho |
| 32 | lme_7a87bd0c / s2_t5 | `现在` | A |  |
| 33 | lme_7a87bd0c / s2_t5 | `现在` | A |  |
| 34 | lme_7a87bd0c / s2_t5 | `现在` | A |  |
| 35 | lme_a2f3aa27 / s1_t2 | `now` | B | I've got 1250 followers on Instagram now, so it'd be great to get some insights |
| 36 | lme_a2f3aa27 / s1_t2 | `now` | B | I've got 1250 followers on Instagram now, so it'd be great to get some insights |
| 37 | lme_af082822 / s1_t1 | `the user cannot be remembered` | A |  |
| 38 | lme_bc8a6e93_abs / s1_t4 | `Lately` | B | with different types of flour lately, and I've been wondering if t |
| 39 | lme_bc8a6e93_abs / s1_t5 | `now` | A |  |
| 40 | lme_bc8a6e93_abs / s1_t5 | `now` | A |  |
| 41 | lme_bc8a6e93_abs / s1_t5 | `now` | A |  |
| 42 | lme_bcbe585f / s1_t3 | `no memory request` | A |  |
| 43 | lme_bcbe585f / s1_t5 | `明天` | A |  |
| 44 | lme_c8c3f81d / s1_t5 | `in April` | B | rent shoes at the outlet mall in April, and they were on sale for 20 |
| 45 | lme_ccb36322 / s1_t1 | `just got back` | B | me concert recommendations. I just got back from seeing The 1975 live and |
| 46 | lme_f4f1d8a4_abs / s1_t2 | `recently` | B | lairs. Speaking of caramel, I recently made a caramel apple tart for |
| 47 | lme_f4f1d8a4_abs / s1_t4 | `明天` | A |  |
| 48 | lme_gpt4_b5700ca9 / s1_t1 | `last week` | B | t at an Easter Egg Hunt event last week and I'm interested in finding |
| 49 | lme_gpt4_b5700ca9 / s1_t4 | `today` | B | rvice at the Episcopal Church today, it was a beautiful and movin |
| 50 | lme_gpt4_e072b769 / s1_t2 | `current_date` | A |  |
| 51 | absolute_date_ranges / time_s2 | `从2027年4月15日至2027年4月30日（含首尾两日）` | B | 岸芷艺术馆的另一段已确定安排是：从2027年4月15日至2027年4月30日（含首尾两日），每天19:00闭馆。 |
| 52 | device_default_paper / device_s2 | `null` | A |  |
| 53 | event_registration_fee / event_s2 | `保留到明天` | A |  |
| 54 | person_sketch_pen / person_s2 | `钢笔` | C | 冯序宁画速写时固定使用钢笔。 |
| 55 | task_isolation / task_s2 | `today` | A |  |
| 56 | task_isolation / task_s2 | `today` | A |  |
| 57 | task_isolation / task_s2 | `today` | A |  |
| 58 | task_isolation / task_s2 | `today` | A |  |
| 59 | task_isolation / task_s2 | `today` | A |  |
