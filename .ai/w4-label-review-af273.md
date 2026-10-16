# Claude 起草（AF273，待 Codex 复核与用户确认）：W4 23 个草稿样本评审

背景：样本按 w4.v2 起草（label_status=draft，从未经用户评审）；现行 W1/W3 v4（通用 fact value 为连续片段、时间限定语、否定事实可提交）、W4 v5。目标：确定标签可用于更新判定阶段的模型对比。

## 逐条意见（期望动作沿用，除非注明）
| 样本 | 期望 | Claude 意见 |
|---|---|---|
| 01 state_change（北京→上个月搬上海） | state_change [17]，effective 上个月 | 同意 |
| 02 correction（苏州→说错了杭州） | correction [21] | 同意 |
| 03 refine（每天早上跑步→五公里） | refine [30] | 同意；候选 value 按 v4 可改为片段“我每天早上跑步五公里”（可选，非必须） |
| 04 coexist（喜欢咖啡；周末更喜欢手冲） | coexist [] | 同意 |
| 05 retract（周三游泳，收回） | retract [40] | 同意 |
| 06 撤回目标不明，不调用 W4 | 无决定 | 同意（代码路径） |
| 07 conflict（7 英寸 vs 又查到 8 英寸未核实） | conflict [50] | 同意 |
| 08 无记录，不调用 W4 | 无决定 | 同意 |
| 09 new（住址 vs 工作） | new | 同意 |
| 10 reinforce（还是喜欢咖啡） | reinforce [31] | 同意 |
| 11 偏好强度 弱→强 | state_change [41] | 同意动作；**输入过时**：v4 下应为 likes/preference + strength（weak→strong），现为 fact 整句，建议改输入为专用关系（会改指纹，未录过，无损失） |
| 12 多目标 state_change | [62,63] | 同意 |
| 13 无生效时间 state_change | [64]，结束时间未知 | 同意 |
| 14 重判已提交（一杯→现在两杯） | state_change [55] | 同意 |
| 15 解决冲突（7 英寸记错） | retract [50] + 解决关系 | 同意 |
| 16 pending scope_incomplete（只用这一台） | pending | 同意 |
| 17 pending unclear_relation（学日语/学韩语） | pending [90] | 同意 |
| 18 legacy 缺来源 | pending unclear_relation | 同意 |
| 19 任务范围（改为葡萄牙语） | state_change [97] | 同意 |
| 20 撤回目标不明（北京/天津） | pending target_unknown | 同意 |
| 21 确认冲突一方并重述 | refine [51] + retract [50] | 同意动作；**输入过时**：两候选共享整句 value，v4 下应各取片段（“栖风读书机的屏幕是8英寸”与撤回“7英寸那份是记错的”），建议改输入 |
| 22 冲突双方被新版本替代 | state_change [110,111]，不解决关系 | 同意 |
| 23 重判为 reinforce | reinforce [55] | 同意 |

## 建议新增（覆盖新能力）
- n1 否定事实更新：已有“我在字节工作”（works_at 字节），新“我不在字节工作了”（works_at negate 字节）→ state_change（结束旧记录）；对照“我从来没在字节工作过”→ correction。
- n2 同句兄弟片段：一句含两个断言，一个与旧记录 state_change、另一个 new。

## 请 Codex 复核
按 w4.v5 与现行 W1/W3 v4 检查：每条期望与 after/recall 断言是否仍正确；输入是否需按 v4 更新（11、21 及其他整句 value）；新增样本设计；评分器支持；允许文件。
