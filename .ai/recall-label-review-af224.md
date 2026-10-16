# AF224 召回标注复核清单（2026-10-10）

只读复核当前40项，未修改样本、生成器或标签；以下是供 Claude/用户裁定的建议，不是新评分规则。没有使用生产原话或发起调用。

## 版本与评分边界

- R1 v1→v2（AF88/AF90）：无年份日期以表达所属来源的可信时间及事件方向解释；明确年份/明确上下文优先。上下文无独立时间不能借当前时钟，不明方向/未知锚点保持 unknown。字段形状不变，不能仅因版本旧就重标全部样本。
- R4 v3→v4（AF39/AF41）：计划与实现/预订分开；修改、取消、纠错保留原义与限定语。历史補录 said_on 可未知，接收日不是原陈述日。
- R4 v4→v5（AF56）：R1 主体歧义仅初判，R4 根据授权证据终审；开放列举无需强制选人，真实歧义仍澄清。
- R4 v5→v6（AF88/AF90）：无年份日期依可信原话时间及事件方向解释，非机械补年。
- R4 v6→v7（AF206）：通用 fact value 定位单条断言的连续片段；完整 sentence 保留。兄弟记录不因引用交叠共享 grade/update，单候选不证明来源全部整理。
- AF216 否定事实可提交是投影政策变化，R4 支持 negate 的字段本来存在，不另升版。recall.v8 是宿主包络；混合检索独立 retrieval 不进入固定 R4 包，不在本阶段测检索。
- 沿用 Z8：time=null 标签表示不计 time 语义，非要求模型必须 null；conditions 是必需提示的子集，合法额外提示不扣分。保留完整机械核验。
- R4 自动评分只匹配必需 facts；额外事实及 statement 含义、must_not 均须人工复核。不能将自动通过率称为完整语义通过率。多种合法输出需要整支期望，不能字段串配。
- 当前 R4 简写均把完整记录表示为 self/user_statement/fact/整句 value，无 uncertainty/strength/history；plan.filter.relation 固定 fact，time/unit 缺省空。保留此历史解释，不按标签自动修输入。完整形式通过明确输入表达上述字段，输入修订会改变请求指纹；只修标签不改变指纹。

## R1：逐样本建议

| 样本 | 可能受影响字段 | 理由与建议 | 输入表达限制 |
|---|---|---|---|
| r1_01_lookup_residence | filter.relation/type | 保留。lives_in/null 都合法，查询不是事实；无年份新规则不直接影响。 | 无本题所需限制 |
| r1_02_plain_statement | need/current_evidence/time | 保留 context_only/assertion_present；“今天”可合法给 time，现 null 标签不计时间，不能误扣。 | 无 |
| r1_03_list_allergies | type/op/filter | 保留 aggregate/list；限定语检索提示不要求排除额外合法内容。 | 无 |
| r1_04_count_countries | op/filter | 保留 count；R1 不输出总数，事件去重交 R4。 | 不验证实际检索枚举/去重 |
| r1_05_sum_with_time | time/filter.time | 保留 relative month=-1，来源为当前问题时钟。“上个月”不是无年份方向推断；等价表示的新增备选须整支核对。 | filter.time 未评分，需人工看是否漏掉检索范围 |
| r1_06_elapsed_since_move | op/unit/time | 保留 elapsed；time=null 不表示不能解析合法时间。缺单位不强制指定日。 | 无 |
| r1_07_compare_order | type/op | 保留 comparison/compare；R1 不产生操作数或答案。 | 无 |
| r1_08_unresolved_pronoun | subject/time | 保留 unresolved“他”；不能凭唯一/最近目录选人。“最近”若输出 time 仍须合法引用。 | 当前 subject_ids=None，不能测宿主可信主体限制 |
| r1_09_ambiguous_name | subject/time | 保留两 ID 歧义，次序不评分；R1 歧义不能用作 R4 必澄清标签。 | 不直接测 R4 终审 |
| r1_10_context_binds_name | subject/time | 保留显式点名绑定。上下文“上个月”无自己的可信日期，不得借本轮日期给历史事件补时间；time=null 目前不计该风险。 | context 只有文本，无独立时间锚点；若补时间标签需另审 |
| r1_11_condition_hint | conditions | 保留必须含“周二”；可同时在 time 描述 recurring，不因额外提示失败。 | 不评分 time/null，不代表已验证 recurring |
| r1_12_statement_plus_question | need/current_evidence/op | assertion_present 保留；问题问周边推荐，是否确需 judge、还是只需 context_only，应用户裁定后采用完整分支，不能仅据混合句要求 judge。 | 不测推荐答案，只测规划 |
| r1_13_hypothetical_question | need/current_evidence/conditions | 保留 query_only，假设不成为陈述；need 的现有备选可保留，新增条件提示仍接受合法引用。 | 无 |
| r1_14_past_month_job | time/role/filter.time | 保留去年五月 month offset=-1；不是无年份日期。角色/筛选时间尚未完整评分，不凭字段有值宣称历史范围正确。 | 现规范化 time 不保证所有语义细节已评分 |
| r1_15_tool_with_condition | conditions/time | 保留活动提示与 uses/null；“现在”可保留未知持续范围，不应强制当天或永久。time=null 不计分，建议加人工核对/专门时间样本。 | 当前不核时间语义 |
| r1_16_count_cats | op/filter | 保留 count；不同猫的身份交 R4，不按提及条数计。 | 无实际对象集合 |
| r1_17_assistant_preference | need/current_evidence/time | 保留 context_only/assertion_present；“以后”不是保存期限，合法时间可留，null 标签不计分。 | 无 |
| r1_18_did_i_say | current_evidence/op | 保留 query_only/lookup；“我说过…”在问句中不是新的过敏事实。 | 无 |
| r1_19_injection_other_users | subject/filter/可信范围 | 保留允许的初判，不靠宽松 subject 备选豁免非法 ID。当前只有自己目录，不能证明 owner/任务隔离，应补可信范围零调用回归。 | subject_ids 固定 None、task=null，无跨 owner 数据 |
| r1_20_elapsed_days | op/unit | 保留 elapsed/day 或 null；端点 @request.at 是 R4/代码规则，非 R1 输出。 | 无 |

## R4：逐样本建议

“输入修改”仅指建议 Claude 起草新完整输入后复核，本轮未执行。

| 样本 | 可能受影响字段 | 理由与建议 | 输入表达限制 |
|---|---|---|---|
| r4_01_single_supported | current/polarity/statement | 保留单记录 supported/affirm；current 是本轮判断，不由 Active 自动证明。 | 整句旧值合法，但只覆盖 self/fact |
| r4_02_uncertainty_kept | Candidate.uncertainty/facts.uncertainty/statement | **修改输入，保留不确定性期望**：当前 Candidate.uncertainty=null，原文却含“可能”。不能据此给模型“字段保留”成绩；完整输入显式给限定语，另测从 raw 理解。 | 简写不能表达 uncertainty/strength |
| r4_03_condition_applies_unknown | conditions/applies/value | 保留条件与 unknown/no，不改为无条件用笔。完整输入宜清楚限定语归属，value 不必整句。 | conditions 可表达；片段 value/专用关系 value 不能表达 |
| r4_04_new_raw_overrides_old | facts/current/supersedes/时间 | 保留新原文可支持、旧值不能确定当前；相对时间以 s-40 日期解释。若合法输出只保留新事实、把旧方列为替代依据，应考虑完整分支，不强制两条 facts。 | raw 可表达日期/状态；sequence 是适配器合成，不能证明真实事件时序 |
| r4_05_hypothetical_raw_not_update | modality/current/statement | 保留假设不能改住址；计划支持不把这条假设自动变成计划/事实。附加假设说明可以有，不能说已搬家。 | raw 保留全文，足够本题 |
| r4_06_query_only_not_evidence | verdict/facts/evidence | 保留 insufficient，无可核准事实；facts=[] 标签只是无必需行，不等于自动断言输出为空，must_not 要人工核对。 | 可表达；生产 no_evidence 跳过路径另测 |
| r4_07_current_message_assertion | verdict/current/替代关系 | **复核完整分支**：新陈述未显式解释变化还是纠错；supported/conflict 与 yes/unknown 不宜随意串配。需分别说明哪种输出成立，或明确注入有依据的变化场景。 | R1 被固定 self，不能检验真实歧义终审 |
| r4_08_pending_update_not_unique | verdict/facts/current/update | 保留 pending 不能确认唯一值；若接受 insufficient，应允许与之自洽的 facts 分支，不强制相同必需事实。R5 pending 不能被模型清除。 | 可表达 update，但模型输出没有 grade/update |
| r4_09_legacy_gap | verdict/evidence/legacy.data | 保留来源缺失不能新增 supported；当前 data 为“legacy”占位，宜补明确旧片段以真实测来源缺失，但不能伪造来源。 | Legacy 类型有；具体真实旧值不是简写现输入 |
| r4_10_superseded_history | facts/current/历史措辞 | 保留 state_changed 旧值可历史呈现；查询只问旧住址，强制返回新当前事实可能过严，建议必需旧值、新值可选或完整分支。不能以 known 虚构具体结束日期。 | R4 的 RecordView 本来无 end，不能要求模型给出旧值精确结束日 |
| r4_11_corrected_not_history | statement/额外 facts/verdict | **保留纠错非真实历史底线**，已有题不必重复新增。新记录的当前雇主并不证明“以前”；考虑“说明更正+历史未知”的合法分支。额外错误历史事实须人工复核，否则自动通过会漏 AF219。 | 简写能给 termination；无 history 链，不能要求不存在的链字段 |
| r4_12_end_unknown | verdict/applies/时间 | 保留不推断一直有效；针对去年三月缺区间支撑，applies=yes 备选需重新论证。insufficient 无事实应有独立分支，不与 supported 必需行串配。 | 注入 plan.time=null，未体现已解析的查询日期范围 |
| r4_13_open_conflict | verdict/facts/statement | conflict 保留；只验 verdict/facts=[] 会漏模型选唯一值或无出处冲突说明，需人工看双方。 | 可给关系成员/basis；专用范围限定语简写不能表达 |
| r4_14_count_dedupe | event_group/items/coverage/value | 保留同事件去重；应人工核每个组归属，3项/2组不足以保证归组正确。未来补基于可信范围的时间筛选与最终数值。 | 记录被表示为 fact 而非真实事件类型；plan.filter.time=null |
| r4_15_sum_quantities | quantity/unit/coverage/对象身份 | 保留120+80的原文依据；建议明确 event_group 与确切200的期望，不能靠数量集合忽略重复对象/负值/漏项。 | plan 没有“上个月”的解析筛选；限定时间可从原文判断但不是完整生产输入 |
| r4_16_elapsed_request_at | date/date_source/operands/rounding | 保留 @request.at，起点是事件日而非 said_on。已有明确年份不受 v6 补年变更直接影响；需评分日数/取整才算完整验证。 | 当前 Candidate.times=[]，靠完整原句仍可合法判断 |
| r4_17_elapsed_month_precision | verdict/compute/precision | 保留月精度不能算天；supported(有粗时间事实)+计算不足或 insufficient 的可接受分支需用户裁定，不能填某一天。新增无年份方向案例不复用这里作为全部证明。 | 无显式 Candidate 时间；仅当前 said_on 与原文月份 |
| r4_18_compare_dates | operands/date_source/precision/value | less 保留。年/月精度顺序可明确，不要求补日；操作数身份顺序须对应日语/韩语，不能只比 operands 数量。 | 整句 fact 可以提供日期，但真实类型/时间字段为空 |
| r4_19_truncated_count_lower_bound | verdict/compute/coverage/下限 | 保留不能 exact；建议完整分支明确“已有事件给下限”与“身份不足无法计算”。仅测 coverage 不足以证明下限算对；不得把原文消息数直接当国家数。 | completeness 可表达，derived bound 属代码，不让模型输出 |
| r4_20_retracted | verdict/statement/额外 facts | 保留无当前任职证据；可有“用户撤回过”的有出处说明，不等于真任职历史。facts=[] 不禁止合法审计说明，人工核验别把旧值当真实历史。 | 可表达 termination；无历史链 |

## 必须补的输入与验证

1. 使用显式完整 Candidate 的计划、预订/实现区别、计划取消/修改；肯定/否定分别定位同句片段，保留各自限定语；多主体开放列举及真实歧义；未知 said_on 与跨年无年份日期。现有简写不能靠改 label 覆盖这些能力。
2. 同句 grade/update 已有 `tests/recall_r4.rs::pending_requires_explicit_record_identity_and_unknown_stays_unknown`：两个同句片段记录，一个 pending 一个 judged；无 ID 原话事实为 uncommitted/update=null。属于 R5，零调用，不再新增重复测试。
3. 本轮完整输入机械检查来源、日期、W2 候选/时间解析、重复 ID/目录、可信主体范围、缺口计数；不从语义期望回填输入，不在代码判断计划是否发生或人物是否同一。
4. 本轮干跑按尚未修订的20+20计算，正式560槽、独立冒烟21槽。标注复核/输入冻结完成后重新计算、生成新 campaign，再报真实调用额度；不将当前计划视为获准发送。
5. 历史简写与全部样本标签原样保留，完整形式接同一语义评分及 raw→Z18→final 核验。人工复核结果接入比较报告的工作留后续 B 段，本轮只显示计划，不生成质量结论。
