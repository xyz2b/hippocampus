# Claude 分析：第五次生产试用（d92374d，2026-10-07）

数据（用户私有，只读，勿复制进仓库/勿大段引用）：~/.local/share/personal-memory-runtime/traces/trace-59963-*.jsonl；生产库 memory.sqlite3。

## 已见效
- 宿主：先登记 child-wa 再保存；第 2/3 条 prepare 带 history=1；未重复召回。
- W1/W2：三条消息所有候选 W2 ok（无空主体、无事物当人物）。
- turn-002 两条提交（生日更正 2023-08-25、住宿安排）；turn-003 提交 child-wa visited 泸沽湖。
- 十月安排 recall answered（行程、机票、停车、路线、住宿等）。turn-001 prepare 的 R4 也通过（6 条事实）。

## 问题
### R1 turn-001 全部被 W3 拒绝（新）
- W3 结论：[0][1][3][5] contradicted（aspect=time），[2][4][6][7] insufficient（[2] value，其余 time）。0 条提交，来源 pending。
- [0] 出生日 2025-08-25（interp 与原文一致）被判 time contradicted——推测 W3 把“2025-08-25生”与“目前三岁多一点”的内部矛盾归到时间上。原话确有矛盾（用户随后更正为 2023）。
- [1][3][5] “10.10-10.30”等无年份月日，W1 解释为 2026-10-10..2026-10-30（said_on=2026-10-07），W3 判 contradicted——推测 W3 不接受由 said_on 补全年份，或要求 anchored/unknown。
- [4][6][7] 无 times 的计划被判 time insufficient——W3 似乎要求计划带上时间，过严。
- 对比：上一轮（26ef7d3）同一首条消息 W3 全部 supported。W3 提示词本轮未改，W1 输出与输入变化导致判断变化，也可能是模型波动。
- 影响：首条消息（最重要）又没有结构化；召回靠 raw/R4 仍能答对。

### R2 turn-003 coverage_gaps
- “少了张掖2晚” plan 被 W3 判 relation insufficient；“不超过3000米应该还行”判 relation/uncertainty insufficient；coverage 报 gaps。

### R3 turn-002 prepare 的 R1 仍降级
- R1 validate：conditions[9].quote.text quote_or_source（引用与原文不符；新诊断已能定位）。R4 仍 answered（宽泛检索）。

## 需要 Codex 确认
1. W3 对“由 said_on 补全年份的月日解释”与“无时间的计划”的判定依据：是 W3 提示词/契约规定（应判 contradicted/insufficient），还是模型过严？契约 1.2/2.1/2.3 对无年份日期的 interp 规定是什么（W1 应给 date/range 还是 anchored/month 等）？
2. 出生日被判 time contradicted 是否合理（内部矛盾应由谁、以什么 aspect 表达）？
3. R1 conditions[9] 的具体不符原因（Markdown？截断？）。
4. 方向是改 W1（无年份日期的表达方式）还是改 W3（核验口径），避免两者对同一规则理解不一致。
