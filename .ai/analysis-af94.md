# Claude 分析：生日更正被 W3 判 subject insufficient（第六次生产试用，91a068a）

数据（用户私有，只读）：~/.local/share/personal-memory-runtime/traces/trace-70987-*.jsonl；生产库。

## 现象
- turn-002“出生日期写错了，是2023-08-25。…”：W1 绑定 subject=child（实体 child，aliases 娃/孩子），times=2023-08-25 date；W2 ok；W3（w3.v3，minimal）verdict=insufficient，problems=[subject]。来源 pending（住宿候选已提交）。
- W3 输入确有 user_history（上一条“我娃2025-08-25生，目前三岁多一点…”），entities 含 child。
- 结果：结构化记忆里孩子生日仍为 2025-08-25（记忆 1，整句），召回靠 raw 原话给出 2023-08-25。

## W3 提示词现状（src/write_models.rs W3_PROMPT）
- 已有：“Use necessary USER history to resolve an explicitly established subject, property or event antecedent, including an omitted subject in a correction. The current message supplies the new assertion or replacement value...”
- 同时有：“History only resolves references; evidence must be in current_user_message.”“Ambiguous history remains insufficient; catalogue uniqueness, recency or elimination cannot resolve a referent.”“Check kinship and omitted subjects against explicit references...”

## 假设
H1 模型波动（W3 minimal）：规则已存在，模型仍保守。上一轮（d92374d）同类更正 W3 supported、提交成功；更早一轮（26ef7d3 前）也曾 supported。
H2 历史里同时存在“2025-08-25生”与“三岁多一点”的内部矛盾，W3 可能把“更正对象是否明确”与矛盾混在一起，归到 subject。
H3 规则措辞冲突：“evidence must be in current_user_message” 与 “history resolves omitted subject” 并列，minimal 下优先采信前者。

## 可选修复方向（待 Codex 复核后定）
A. 提示词：把“更正句中省略的主体/属性由历史中明确先行语确定，确定后 subject 不因未在当前句重复而 insufficient”放到最显著位置，并说明历史内部矛盾不影响先行语是否明确。
B. W3 推理强度 minimal → low（成本小：W3 输出约 100–800 token）；需评估是否只对“主体由历史解析”的候选，还是全量（后者简单）。修改角色缺省属配置变化，非契约。
C. 不改：记为模型波动，依赖用户重述或 reprocess（confirm_subject）补救。

## 请 Codex 确认
1. 能否从 W3 原始输出/输入进一步判断是 H1/H2/H3 哪种（不发真实调用）。
2. A/B 是否越过“代码只核机械事实”或需改契约/升版（A 改提示词语义是否需升 W3 版本）。
3. 推荐方案与最小验证（真实验证需用户批准调用额度；可用虚构样本）。
