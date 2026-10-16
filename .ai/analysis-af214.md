# Claude 分析（AF214，待 Codex 复核）：允许普通否定事实提交

## 现状
- 执行投影（src/contract.rs:1260 起）：preference/constraint 的否定 → Memory.polarity=Negative；retraction 与 plan 的否定 → polarity=None（plan 依赖完整 Candidate 保存极性，投影“非无损”）；普通 assertion + fact/episode + negate → Unprojectable::Polarity(Negate) 拒绝，候选 pending、不提交。
- 契约 2.5（contracts.md:273）明确“普通非方向型否定事实的投影限制不因计划放宽”；AF56 用户当时裁定维持现状、靠原话召回。2026-10-10 用户改为允许（AF206）。
- Memory.polarity 已是 Option<Polarity>（src/model.rs:446/550），无类型约束；fact_index 键含 polarity（fact_index.rs:68）；commit.rs:1097 身份检查比较 polarity；recall.rs:118/510 透传 polarity。完整 Candidate（含 polarity）随 W5 回执保存并由 RecordView 返回。

## 初步方案
1. 投影：普通 assertion 的 fact/episode + negate → Memory.polarity=Some(Negative)（不再拒绝）；affirm 仍为 None（保持旧记录不变）。不改 DB（字段已存在，JSON 快照兼容）。
2. 语义边界不在代码：否定事实与同槽肯定事实（如“我不住在上海” vs 已有 lives_in 上海）是更正、撤回还是并存，由 W4 判断；代码不自动结束或冲突。
3. 召回：RecordView/R4 已透传 polarity；需确认 R4 提示词和召回输出对“否定 + 普通事实”有明确呈现（如“用户不在公司上班”），宿主说明强调保留 polarity。
4. 版本：契约 2.5 文字修改；执行投影规则变化影响 W5/W4 提交结果与 recall 输出内容——是否需要升 W4/recall 版本由 Codex 评估（输入结构不变，可能只需契约文字+行为测试）。
5. 风险：W1 把“不是X”误判为否定事实的错误会直接进入记忆（之前被投影挡住）；由 W3 核验兜底。旧 pending 的否定候选不会自动重判。

## 验证
- 零调用：投影、身份检查、fact_index 键、RecordView/R4 输入中否定事实呈现、W4 与同槽肯定事实的输入组织。
- 实测（虚构）：“我不在公司上班，在家带娃”“我不住在蓝港了，住在橙湾”“我没去过青岚湖”等，串联 W1→W3→W5，以及定点 W4（与已有肯定事实）和 R4 召回各少量。

请 Codex 只读复核：投影改动的完整影响面（W4 同槽/冲突、去重、fact_index、grade、召回呈现、episode 否定的语义）、契约与版本、旧数据、最小实现与允许文件、实测设计与调用上限。
