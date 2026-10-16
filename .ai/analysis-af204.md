# Claude 分析（AF204，待 Codex 复核）：一句多断言时通用 fact 无法区分

## 现象（生产 2026-10-10，只读）
“我现在不在公司上班，在家里带娃”：W1 两候选均 relation=fact、value=sentence=整句，一 polarity=negate、一 affirm，times 均含“现在”(validity unknown，时间修复生效)。W3：候选 0 contradicted[polarity “在家里带娃”]，候选 1 insufficient[condition “在家里”]。来源 pending，零提交。

## 根因
契约 1.5/2.1 与 W1 提示词：多断言共用完整原句；relation=fact 时 value.quote/value.text == sentence（整句）。该规则来自 AF66/AF70：模型自造子串/截断导致 W2 fact_sentence_mismatch 大量拒收，于是统一整句。副作用：同一句里的多个通用断言在 value/sentence 上完全相同，只能靠 polarity 等字段区分；W3 拿到两个“值相同、极性相反”的候选，无法判断各自代表哪一部分，于是按整句语义判极性矛盾或缺条件。日常对话里“不X，而是Y”“不吃辣但喜欢酸”这类句子很常见。
（专用关系不受影响：value 本就是对象值。）

## 方案方向
A. 契约修改：通用 fact 的 value 改为“表达该断言的最短连续原文片段”（sentence 仍为完整原句；value.quote 必须是 sentence 的连续子串，机械校验）；限定语仍须进各自字段；W3 按 value 片段 + 字段判断该候选，片段外的同句其他内容不作为本候选的反证。需 W1/W3（及 W2 校验）契约升版、提示词改动、录制失效，需用户批准。风险：模型把限定语截在片段外（由字段规则与 W3 覆盖核验兜底）；AF66 的子串拒收根源是“value 不是 sentence 子串/不连续”，A 只接受连续子串，仍可机械拦截。
B. 保持 value=整句，新增候选字段 claim_quote（该断言对应的连续片段），W3 以 claim_quote 定位。比 A 多一个字段，但不改变 value 语义与下游（R4/召回对 fact value 整句的假设）。
C. 只改 W3 提示词（同句多候选按极性/字段推断各自部分）：不可靠，不推荐。
倾向：A 或 B 由 Codex 评估下游影响后定——关键是 R4/召回/W4 是否依赖“fact value == 整句”（如证据引用、去重、同槽判断）。

## 附带
“在家里带娃”中“在家里”是活动地点，属于断言内容而非条件；A/B 后应由片段自然包含。
已 pending 的 4 条来源补处理仍另立任务（全文重提取禁止混合来源）。

请 Codex 只读复核：根因、A/B/C 取舍、契约与版本范围（w1/w3/w2/r4/recall/DB）、下游对整句 value 的依赖、录制影响、最小实现与允许文件、实测设计（虚构场景）。
