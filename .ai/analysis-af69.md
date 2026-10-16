# Claude 分析：第二次生产试用问题（2026-10-07，AF66/AF67）

数据：生产轨迹 ~/.local/share/personal-memory-runtime/traces/trace-20034-*.jsonl；生产库 memory.sqlite3（owner local-user，均为用户私有，只读分析，勿复制进仓库）。失败 R4 的输入/输出已私有提取到 $CLAUDE_JOB_DIR/tmp/r4_input.json、r4_output.json。

## A. 首轮 8 个候选全部被 W2 拒收（fact_sentence_mismatch）— 已确认
- 规则：src/w2.rs:363-368，relation=fact 时要求 value 为 Text 且 text 与 quote 都等于 sentence。
- 数据：turn1 的 W1（low）输出中，8 个候选的 value 都是 sentence 的子串（如 sentence“从阜阳开车去合肥机场。”value“从阜阳开车去合肥机场”，只差句号；或“2025-08-25”“20晚21天”）。其余轮次 W1 给的是整句，因此通过。
- 另一面：候选 0–2 共用同一个 sentence（出生、计划、已订好），若只把 value 改成 sentence，三条会变成同值重复记忆。
- 判断：契约对通用 fact 的定义（value=sentence）本身没问题；问题是 W1 把“断言粒度”放在 value 上而不是 sentence 上。
- 初步修复方向：
  1. W1 提示词：通用 fact 的 sentence 取表达该单一断言的最小连续分句，value 与之完全相同；不同断言不得共用同一 sentence。
  2. W2 机械处理：value 是 sentence 的连续子串且仅差首尾标点时，规范为 sentence（纯机械，不改语义）；其他不一致仍拒收。是否允许“value 是 sentence 的任意子串时把 sentence 收窄为 value”需评估（会丢失上下文限定语，倾向不做）。
  3. W2 机械去重：同一来源内 subject/relation/value/speech/polarity/限定语完全相同的候选只保留一个（见 D）。

## B. “我十月份有什么安排？”R4 被核验整体丢弃 — 原因待确认
- 现象：R4（r4.v5）输出 6+ 条事实，内容正确（10.10–10.30 行程、往返机票、停车场、路线、20 晚 21 天），validate 阶段 model_invalid_output，召回 degraded、facts=0；宿主靠 raw 原文自行回答（AF67）。
- 生产只记总码，具体失败字段未记录（evaluate_detailed 有诊断但未落盘）。
- 我读代码推测的候选原因（未证实）：
  1. facts[*].times：quote“10.10-10.30”“10.10”“10.30”为无年份的“月.日”写法，R4 解释为 kind=date/range 的 2026 年日期（anchor=said_on）。需确认 literal_time_matches（w2.rs:560）对“.”分隔的月日写法是否判为不匹配，或年份推断是否被拒。
  2. quote_or_source：某条事实引用跨越了原文的“**”标记或不连续片段。
  3. record_basis / new_basis：引用已提交记录 2 的 record_ids 时证据不支持。
- 产品层问题（确定）：核验是全有全无——单条事实或单个时间字段不合格，整批事实丢弃（r4.rs 注释“任何字段非法都不留下部分事实”，契约 3.3）。导致正确内容全部浪费、R4 调用白花钱。
- 初步修复方向：先确认具体规则；若是“月.日无年份”，按契约时间规则决定：允许 R4 用 said_on 年份解释无年份月日（与 W2 同口径）或要求此类时间用 unknown/anchored 而非 date；另需评估“逐条剔除不合格事实、保留合格事实并标记 degraded_partial”是否值得改冻结契约 3.3（需用户批准）。
- 生产诊断：把 ValidationDiagnostic 的 field/cause（不含原文）写入轨迹 model_check，便于以后定位。

## C. 征询/提问被当成计划提交 — 已确认
- “那我不在兰州住…是不是更好…你觉得可行吗？”被 W1 标 speech=plan（两条），W3 supported，已提交为记忆 7、8：relation=visited、value=兰州/西宁。
- 问题 1：这是征询意见，不是已决定的安排；应为 question/hypothetical（契约 2.1：question、hypothetical 不成为事实/计划）。属模型误判，提示词需强调“征求意见/询问可行性的提议不是计划，除非用户明确决定”。
- 问题 2：plan 使用 visited（经历类关系）。召回侧 records 保留 candidate.speech=plan，R4 会按计划呈现；但旧 Memory 投影/memory_list 读起来像“已去过”。修复方向：W1 提示词要求计划用通用 fact；W2 机械规则：speech=plan 时不允许经历类关系（visited 等）——是否属于“代码判语义”需 Codex 评估（我倾向属于目录约束，可机械执行）。

## D. 重复记忆 — 已确认
- turn3“然后我娃7月中旬去过一趟泸沽湖…”产出 3 个候选（同 sentence、同 subject child），提交为记忆 4（visited 泸沽湖）、5、6（两条完全相同的 fact 整句）。
- 修复方向：同 A.3 的 W2 机械去重；W1 提示词：同一断言不得重复输出。

## E. 首轮 prepare 慢（145 s）
- R4（minimal）对首轮长消息输出 8414 token；R1 262。status=conflict（出生 2025 与“三岁多”矛盾，判断合理）。
- 暂不处理；记录观察：长消息下 R4 输出偏长，可考虑在提示词中要求每条事实陈述简洁。

## F. 其他观察
- 宿主行为已达预期：首轮单独保存、主动登记 child 实体、问题时调用 recall。
- 生日更正本轮已提交（记忆 1，subject=child），但 value 为整句“出生日期写错了，是2023-08-25。”（符合通用 fact 规则）；首轮 2025 的原记录未提交，因此不存在被取代的旧记录。
