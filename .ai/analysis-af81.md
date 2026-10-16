# Claude 分析：第四次生产试用（26ef7d3 部署后，2026-10-07）

数据（用户私有，只读，勿复制进仓库/勿大段引用）：~/.local/share/personal-memory-runtime/traces/trace-49119-*.jsonl；生产库 memory.sqlite3。

## 已见效
1. 宿主先 memory_entity_upsert（child:wo-wa）再 memory_prepare；没有再用陈述重复召回。
2. 首轮 W2：9 个候选 8 个 ok、并已提交 8 条记忆（出生/年龄/计划/已订好/机票/开车/停车/路线）。AF71+AF78 修复生效。
3. “我十月份有什么安排？”answered，7 条事实（行程、机票、开车、停车、路线、住宿、少张掖 2 晚）。
4. R1 对 t1/t2/t3 均给 assertion_present。

## 问题
### Q1 t2 的 W1 输出被整条拒收（w1_invalid_output，decode 阶段）
- 两个候选的 subject 均为 {"unresolved":{"text":""}}（空字符串），decode 的非空检查拒收整份输出 → 来源 failed，生日更正（2023-08-25）与住宿安排都没有结构化。
- 叠加因素：该 prepare 的 user_turn.history 为空（宿主未带上一条消息），W1 无法从历史解析“出生日期”是谁的。
- 全有全无：一个字段非法，整份 W1 输出作废；无诊断字段（R4 有 field/rule，W1 decode 没有）。
- 方向：W1 提示词——unresolved.text 必须逐字复制原文中的指代短语，不得为空；无法给出指代短语时……（待定）。W1 decode 失败也记录 field/rule（白名单）。宿主说明：prepare 的 history 带上必要的上一条 USER 消息。是否改为“逐候选拒收而非整份作废”需评估（涉及冻结 2.1/2.2 的严格解码规则，可能需用户批准）。

### Q2 t1 prepare 的 R4 被拒（fact_time，cause=time）
- 事实“孩子出生于 2025-08-25”的时间 quote 为“2025-08-25生”，但 interp.date 填成了 2026-10-07（当天）→ 字面日期核对不符，正确拒收。模型错误。
- 同时 degraded_reasons 含 r1_not_understood（R1 输出 625 token），需确认含义。
- 方向：R4 提示词——interp 必须与 quote 中的明确日历一致；不确定时 unknown。

### Q3 主体误判
- “整个行程20晚21天”→ subject unresolved“整个行程”；“少了张掖2晚”→ unresolved“少了张掖2晚”（把事物/短语当主体）。属于用户本人计划，应为 self。W1 提示词已有“属性不是人物、事件回指不是别名”，但模型仍把非人物短语放进 unresolved。
- 方向：W1 提示词——计划/行程类陈述的主体是计划持有者；unresolved 只用于未能解析的人物指代，不放事物或整句。

### Q4 t3 的 W4（观察）
- 分析时 W4 刚开始（约 02:55:44 起，超时 360 s），属正常进行中，不是卡住。完成后再看 t3 提交结果。

### Q5 记忆冗余（观察）
- t1 同一句产生 4 条整句记忆（出生、年龄、计划、已订好），主体/类型不同；契约下合法。召回已能正确使用。暂不处理。
