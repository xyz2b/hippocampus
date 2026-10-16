# Claude 分析（AF191，待 Codex 复核）：① “现在/目前/之前”时间限定语 ② 重复人物合并

## ① 时间限定语导致常见陈述无法提交
证据（生产 2026-10-07/09，只读）：3 条来源 pending，均因 W3 对部分候选判 insufficient/contradicted，且无后续重判路径。
- “我之前住在深圳，现在住在阜阳，2025-11-01回到阜阳的……”：W1 给 lives_in 深圳（times=[]）、lives_in 阜阳（times=[]）；W3：深圳 contradicted[time “之前”]，阜阳 insufficient[time “现在”]。
- 10-07 行程消息：多条候选 insufficient[time “目前”/“10.10”]。
根因：W1 与 W3 提示词对同一类限定语规则不一致：
- W1（src/write_models.rs:89）：“Add times only for calendar time actually expressed… applicability or situational language alone creates no time”，所以“现在/目前/之前”不进 times。
- W3（:172）：“Preserve every applicable expressed qualifier, including unresolved ones”“A qualifier present only in sentence but omitted from its applicable field cannot be supported”，所以判 insufficient。
- target-design 1.x 已记录：“现在/目前/以后…现在不再写成起始时间，但限定词本身也随之消失，事实变成‘一直如此’。应保留为未解析的时间原文。”——即目标规则是保留为未解析时间。
另一层问题：insufficient → 来源 pending，但无澄清/重判触发，相当于静默丢弃；且“之前住在深圳”应作为历史状态（已结束）而非当前，被判 contradicted 后旧居住信息也丢失。

初步方案方向（待复核）：
A. 契约/提示词对齐（w1/w3 小版本）：W1 把“现在/目前/当前/之前/以前/以后/最近”等相对观察时刻的状态限定语写入 times（role=validity，kind=unknown 或新增 anchored-to-said_on 的开放区间？保留原文 quote，understood=false），W3 以字段中保留原文即视为保留（不要求能解析）；“之前X”表示已结束状态：candidate 带 validity “之前”，W4 用于标记旧值（ended）。
B. 只改 W3：限定语为“现在/目前”这类仅表达“当前”的词，若候选本身就是当前状态断言，不要求进 times（与 W1 现规则一致）；“之前”仍要求。更省（只改 W3），但“之前”问题仍在 W1。
C. 代码侧不做语义；不新增句式规则。
倾向 A（与 target-design 目标一致），需评估：契约版本、录制失效范围（W1/W3 请求改变→相关样本需重录，需用户授权）、W4 对 ended 的处理是否已有（termination_reason/ended 字段存在）、样本标签（w1_01 注释已提“现在不评分”的已知冲突）。
另：insufficient 来源的后续路径（澄清或重判）是否需要另立任务。

## ② 重复人物合并（用户要求：合并，但可能有多个小孩，须用户确认是哪个）
现状：child_wawa（娃，别名 孩子/小孩）与 ChatGPT 新建的 family_child_1（小孩，别名 孩子）别名重叠，导致“小孩”主体歧义（10-09 候选 3 主体 unresolved“小孩”）。family_child_1 未被已提交记录绑定（快照中仅 4 处引用，疑为实体本身与批次输入）。
工具现状：只有 memory_entity_upsert，无合并；memory_clarify 存在（用途待核）。
方案方向：
- 代码机械检查：memory_entity_upsert 新建/更新时，若别名或规范名与同 owner 其他实体冲突，返回 alias_conflict（列出冲突实体），不静默创建；需宿主携带用户确认（confirm 指定目标实体或确认为不同的人并去掉冲突别名）。这是机械事实（字符串冲突），不是语义判断。
- 新工具 memory_entity_merge(from, into, user_confirmation)：仅在用户明确确认“是同一个人”后执行；把 from 的别名并入 into，记录（含历史、关系、待整理批次中的主体绑定）改绑或标记需重判，保留确认记录，from 置为已合并墓碑（可审计、不可再绑定）；有多个同类实体时宿主必须让用户指定目标。
- 宿主说明：遇到人物别名冲突/“孩子”等泛称且存在多个候选时，先问用户是哪一个，不自行新建。
- 本次数据：合并 family_child_1 → child_wawa 需用户确认二者为同一个孩子（执行属生产数据修改，需授权）。

请 Codex 只读复核：根因是否成立、A/B 取舍、契约与录制影响、insufficient 后续路径；实体冲突检查与合并工具的最小设计、绑定改写的边界（已提交记录、W4 批次、原文 provenance）、契约/DB 是否升版、宿主说明；最小范围与允许文件。
