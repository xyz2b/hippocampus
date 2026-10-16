# Claude 方案草案（AF181，待 Codex 复核）：混合检索第③④步 持久索引 + R2 接线

依据：评审 36 分步建议、AF173–AF179 实验（hybrid-df 32/32，两模型无差别）。默认关闭（opt-in），开启后才改变 R3 内容。

## ③ 持久向量索引
1. DB 10→11：新表 source_vectors(owner, source_id, segment_index, start, end, content_hash, binding, dims, vector BLOB)。binding = 模型标签 + Ollama 权重摘要 + 维度 + 分段版本(lexical::segments) + 文档格式/指令版本 + 归一化。只索引获准来源正文分段（不做 memory 向量）。
2. 待嵌入集合不另建任务表：按“有正文、未删除、当前 binding 下缺少或 content_hash 不符”的来源即时计算（ponytail：每轮扫描，来源量大再加队列表）。
3. 独立后台嵌入工作线程（不挂在 W1–W4 整理队列上）：批量 16、事务外调用本机 Ollama；落盘事务内重核 owner、来源存在且未删除、正文哈希、binding 未变；不符则丢弃。失败不重试本批，下轮扫描再处理，退避；计数单独记账（嵌入调用/失败/耗时），不占生成式预算。
4. 删除与重建：来源删除/墓碑在同一写事务内删除其全部 binding 的向量（挂到 lexical::synchronize 同一位置或 forget 流程），迟到结果因来源不存在被拒；换 binding 时旧 binding 行不参与检索，新 binding 覆盖完成后清理旧行；未覆盖期间召回标注 vector_pending。
5. 配置（chat_config / runtime.json）：embedding.enabled（默认 false）、model（默认 qwen3-embedding:0.6b）、base_url（仅回环）、batch、query_timeout_ms。禁止云端与自动回落。

## ④ R2 接线（仅 enabled 时生效；关闭时请求构造与现状逐字节一致）
1. 不变：授权范围先行；本轮来源直入；未整理/失败/待重判 raw 全范围可见；聚合/list/count/sum 枚举；小范围穷举不调用嵌入；版本链/关系闭包展开；完整性只由原有穷举条件给出，向量永远不证明完整。
2. 关系提示：有效关系过滤时，该关系全部记录仍全部入选（保留现行为与完整性）；融合候选填充剩余预算——修复错误关系硬过滤（Q31）而不丢正确关系的完整枚举。
3. 非穷举路径：查询嵌入 1 次（问题 + 上下文，与现 hint 同源）；向量通道在可信 owner/任务/主体范围内（不经关系过滤），来源分段最佳相似度映射到其链接记录；词法通道使用 DF>20% 截断、零命中不投票；等权 RRF(60)，两路各 2×records 种子；融合后展开闭包，再按数量/字节预算截断，截断如实报告。
4. 降级：查询嵌入失败/超时 → 只用词法（DF 截断同样只在 enabled 时启用）并标 vector_unavailable；索引覆盖不全 → 标 vector_pending；均不当作“无记录”，不覆盖已核验 R4 结论。
5. 契约：recall v6→v7（新增降级原因与向量覆盖字段）；R1/R4/W4 不升。同参重放复用已有结果，不再调用嵌入。

## 验收
- retrieval_eval 走生产路径（非模拟）复现：enabled 时 32/32、0 越权/删除复活/任务泄露；disabled 时与 AF173 基线逐题一致。
- 现有 recall/R4/端到端回放在 disabled 下指纹不变；enabled 下下游录制严格检查指纹（变化则如实报告受影响用例）。
- 并发：后台嵌入不阻塞前台召回；删除与迟到嵌入的竞争测试；重启恢复。
- 延迟：查询嵌入暖 ~40 ms、首次加载 ~2 s，测前台 P95 增量；内存峰值实测。
- 生产部署与默认开启需用户另行授权。

请 Codex 复核：上述分步是否最小、DB/契约升版范围、关系提示的处理是否正确、DF 是否仅在 enabled 生效、工作线程与删除竞争、配置与降级、验收与允许文件；主动指出漏洞与更简单方案。
