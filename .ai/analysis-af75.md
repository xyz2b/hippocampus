# Claude 分析：第三次生产试用（4e5c3a8 部署后，2026-10-07）

数据（用户私有，只读，勿复制进仓库/勿大段引用）：~/.local/share/personal-memory-runtime/traces/trace-38144-*.jsonl；生产库 memory.sqlite3（owner local-user）。真实调用：W1 3、W3 3、W4 1、R1 5、R4 5（全部 HTTP 成功）。

## 已见效
1. “我十月份有什么安排？”recall：answered，R4 通过核验，给出行程/机票/停车/住宿（AF71 R4 时间引用规则生效）。
2. 首轮 W2：9 个候选中 7 个 ok（AF71 W1 整句规则生效），2 个 unresolved_subject（“我娃”），原因是 W1 在 01:36:52 保存时已开始，而实体 child-1 在 01:37:29 才登记。
3. 新诊断已落盘：R4 失败的 field/rule 可见。

## 问题
### P1 首轮来源最终 needs_rejudge，7 个 ok 候选未提交 — 原因未确认
- u1 enrichment=needs_rejudge；生产记忆 1–6 全部来自 u2/u3。时间线：u1 W1 01:36:52 起（176 s）、W3；u2 W1 94 s、W3；u3 W1 90 s、W3；最后唯一一次 W4（01:57 前后，5850 输出）。
- 推测：u2/u3 无旧目标直接 W5 先提交，u1 的 W4 晚于它们，触发顺序屏障/轮次过期而转 needs_rejudge；或 u1 的未解析候选导致整批等待后过期。需 Codex 确认具体规则与这是否为预期行为；若预期，则“首条消息因后续消息先提交而永远不提交”是产品缺陷（需要自动重判或调整顺序）。
- 已存 memory_source_status 可查看，但 CLI 不支持该工具；未能离线查看。

### P2 prepare 中 R4 被拒（3 次，均 rule=quote_or_source）
- 2 次：R1 对明显含个人陈述的消息给 current_evidence=query_only（u1、u2，以及 ChatGPT 用 u1 全文作 query 的 recall），R4 却以 kind=current 引用当前消息 → 契约禁止（query_only 不得有 current 引用）→ 整批丢弃。同一原话已作为 stored raw 在证据包中，R4 应引用 stored。
  - 两个模型错误叠加：R1 分类错（prepare 的当前消息是陈述，应为 assertion_present）；R4 违反“raw 用 stored source_id、query_only 无 current 引用”。
- 1 次：R4 引用路线时删去原文中的 Markdown“**”，导致 quote 不是连续原文片段。
- 修复方向（待定）：R1 提示词强化 current_evidence 判定（陈述性内容即使附带问题也为 assertion_present）；R4 提示词：逐字复制，包括 Markdown/符号；当前消息若已作为 stored raw 出现，引用 stored。是否允许代码在“current 文本与包内 stored raw 全文完全相同”时机械映射为 stored 引用？——涉及 query_only 语义（问题本身不是证据），倾向不做，请 Codex 评估。

### P3 宿主行为
- ChatGPT 在 u1 后又调用了一次 memory_recall，query 为 u1 全文（非问题）。产生额外 R1/R4 调用，且失败。指令可补“不要把陈述原文当作 recall 的 query”。

### P4 W1 耗时
- u1（长消息）W1 low 176 s、输出 12933 token；u2/u3 约 90 s。后台不阻塞对话，但首轮记忆约 3–4 分钟后才可用。暂不处理，记录。

### P5 其他
- u3 仍产出 visited + 两条同句 fact（Codex 评审 18 已认定类型/限定语不同，非结构重复），维持。
- 生日：u1 的 2025 候选因主体未解析未提交；u2 更正已提交为 child-1 的通用 fact（整句）。
