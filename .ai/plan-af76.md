# Claude 修复方案（草案，待 Codex 复核）— 依据 .ai/analysis-af75.md 与 Codex 评审 20

## P1 实体登记使处理中的来源失效（最重要）
采纳评审 20：登记实体推进 source_less_barrier_sequence，使处理中的 u1 被标 stale 并最终 needs_rejudge；这是保守规则的预期行为，但对“纯新增实体”过宽。
1. 宿主短期（零风险）：工具描述与宿主说明改为——消息提到未登记的人物时，**先** memory_entity_upsert 登记，**再** memory_prepare 保存；不要在保存后补登记再重复召回。
2. 规则长期：区分“纯新增实体”（新 id、不修改已有实体、别名与已有实体无冲突、不改变任何已有绑定）与修改已有实体/别名/删除。纯新增实体不推进 source-less 屏障；其余维持现状。不能自动把 unresolved 绑定给新实体。
   - 请 Codex 判断：这是否改变冻结契约中的顺序屏障条款（需用户批准与升版），还是属于实现细化。
3. 诊断：保存计划被屏障拒绝时，把内部原因（如 source_less_barrier）写入轨迹；最终外部状态不变。
4. CLI 增加只读 memory_source_status 分发（零模型调用）。

## P2 R4 引用被拒
1. R4 提示词：强化优先级——引用必须逐字复制原文连续片段，包括 Markdown、符号与空白；current_evidence=query_only 时不得使用 current，引用证据包中的 stored 来源。
2. R1 提示词：当前消息含用户本人陈述（即使同时提问）时 current_evidence=assertion_present；纯提问才 query_only。不在代码中按 prepare/recall 入口强判。
3. 机械资格约束：已核验 R1 计划为 query_only 时，R4 请求 schema 排除 current 来源分支（代码不改语义，只收窄可选项）。请 Codex 评估是否属契约/协议变化。
4. 不做：全文相等自动重绑来源；忽略 Markdown 的容错。

## P3 宿主重复召回
宿主说明：memory_prepare 已同时完成保存与召回；在没有新问题时，不要再用同一段陈述调用 memory_recall。核实/更正类查询仍可调用。

## P4/P5
不处理（W1 输出主要为推理 token；同句不同类型不去重）。

## 范围与验证
- 文件：src/sqlite.rs / src/commit.rs（屏障细化，视 Codex 判断）、src/chat*.rs（工具描述、CLI 分发、诊断）、src/recall_models.rs（R1/R4 提示词与 schema 收窄）、宿主/客户端说明（含被忽略的 chatgpt-custom-instructions.md 同步）、测试。
- 零调用测试：纯新增实体不使处理中来源失效、修改/别名冲突/删除仍推进屏障；query_only 下 schema 不含 current；诊断落盘；CLI status。
- 真实效果需真实对话验证；用户会在部署后自行触发。
