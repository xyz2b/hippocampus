# Claude 人工复核决定（AF249）：AF247 重评 review_queue 228 条（去重键前缀）

reviewer=Claude（产品负责人），2026-10-11。规则：自动字段已通过，人工只看额外事实、statement、限定语、人物、分组、must_not、R1 检索提示是否会误导。未列出的 202 条全部 accepted（理由：自动字段正确且未发现额外问题）。

## R1 rejected（21）
| key 前缀 | 样本/档 | 理由 |
|---|---|---|
| eedf9cb76573 | r1_01 qwen r2 | conditions 引用整句问题，不是限定语，会污染检索 |
| 2bec936bdac7 | r1_02 qwen r1 | conditions 为整句陈述 |
| 7d79e73874f3 | r1_05 ds-flash r1 | conditions 为整句问题 |
| 8c157d50363d | r1_06 qwen r1 | 整句 conditions；把“多久了”当时间 |
| 90e8de8274bc | r1_06 qwen r2 | 把“多久了”解释为 -1 年，伪造时间 |
| 7153d1c1e024 | r1_07 ds-flash r1 | relation 提示为无意义“ok” |
| 3a9a8476133e | r1_07 ds-flash r2 | relation 提示为无意义“.” |
| e2c73d3520f0 | r1_09 ds-pro r2 | “最近”压缩为当天（offset 0 day） |
| 5fc5abb61bde | r1_09 qwen r1 | “最近”伪造为 -1 月 |
| ea0a44829e67 | r1_09 ds-flash r2 | relation 提示为“undefined” |
| c3ec3d104a67 | r1_10 ds-flash r1 | filter.time 把“最近”伪造为锚定 -1 月 |
| 1e46f3c621a7 | r1_10 ds-flash r2 | “最近”解释为 +1 月（方向错误），op 改 list |
| d6b1b1348a5d | r1_10 qwen r1 | 整句 conditions；“最近”伪造 -1 月 |
| dd7b7988f676 | r1_10 qwen r2 | “最近”伪造 -1 月 |
| 8e080eedfa5c | r1_11 qwen r1 | 整句 conditions |
| eed2f4a4fc3e | r1_12 ds-flash r1 | 整句问题作 conditions；relation“null” |
| b9777806d673 | r1_13 ds-flash r1 | relation 提示为无意义“.timeline” |
| 4ca931e5c16e | r1_15 ds-flash r1 | “现在”压缩为当天（anchored offset 0 day） |
| 40412a89316f | r1_16 luna r1 | 把“养过”当作时间写入 filter.time |
| 78ceeede00fb | r1_17 ds-flash r2 | “以后”压缩为 2026-10-03 单日 |
| 88fd89e78f84 | r1_20 ds-flash r2 | relation 提示为无意义“type” |

## R4 rejected（5）
| key 前缀 | 样本/档 | 理由 |
|---|---|---|
| 42685cf7827a | r4_08 ds-pro r2 | 更新待定的记录给 current=yes，等于声称唯一当前雇主（违反 must_not） |
| bd74d1384e08 | r4_12 ds-pro r1 | 旧住址 applies=no，无证据地断言目标月份不适用（违反“声称已确认目标月份住址”） |
| 6e47fc9b91ab | r4_13 ds-pro r1 | 未解决冲突的两条都给 current=no，等于断言两者都不是当前值 |
| db851179ba5d | r4_13 qwen r2 | verdict=conflict 但未返回冲突双方事实（要求两条都返回） |
| c21a54a2e6ed | r4_24 qwen r2 | 把日期“3月15日”误填进 uncertainty |

## 备注
- 英文 statement（qwen/luna/ds-flash 偶发）不扣分，但影响宿主呈现，记为观察。
- r4_11 中“曾表示在字节工作，但该说法后来被更正”不算把字节当作曾经雇主，accepted。
