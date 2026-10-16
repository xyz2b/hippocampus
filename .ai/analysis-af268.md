# Claude 分析与方案（AF268，待 Codex 复核）：R1 清单类问题应 list

## 证据
r1_03「我对什么过敏？」标签 aggregate/list；AF230 五档 10 次全部给 single/lookup（含 GPT-6.1 Sol）。Codex 评审 46 判为提示词缺口：现提示词只写“aggregate 时 plan 描述授权范围的枚举”，没说何时该判 aggregate/list。lookup 走排名候选可能漏项；过敏属安全相关信息，漏项代价高。

## 方案
在 R1 提示词 plan.op 段落补一条判别规则（泛化表述，不含样本原词）：
“If the request asks which members belong to a set that can contain several items for the subject (for example several allergies, pets, visited places or owned items), use type=aggregate and op=list even without words such as all or every; use lookup only for a property that normally has one current value or when the request asks about one named item.”
不改契约、schema、R4；R1 提示词进入请求指纹，R1 旧录制过期。

## 验证（真实调用另批）
- 回归：DS-V4-Pro（生产 R1 minimal）跑 R1 20 样本 × 2 = 40 次，确认 r1_03 变 list，其余（尤其 r1_01 住址 lookup、r1_16 count、r1_04 count、r1_18 did_i_say lookup）不退化。
- 泛化：4 个虚构问题 × 2 = 8 次（多值集合 2 个、单值属性 2 个）。
- 共 48 次，约 0.5 元；结果人工复核。

请 Codex 只读复核：措辞是否准确、会否把单值属性误判为 list、与 count/sum/open 的边界、是否有更好措辞；实测设计与允许文件。
