# 契约类型与完整候选的保存（A 段）

依据 contracts.md §1、§2.1、§2.4、§3.2，设计经 GPT 复审 0015 确定。代码在 `src/contract.rs`。

## 1. 类型

- 类型与契约一一对应：Quote、SourceRef、EvidenceRef、TimeInterp（`interp` 按 `kind` 分支）、TimeResolution、TextQualifier、Subject、Value、w1.v1 Candidate、MemoryRequest、W1Output、W4Input、W4Decision、W4Output、RelationLink、RecordView（Current | Legacy，按 `format` 区分）。
- JSON 字段名与契约逐字一致，并且拒绝未知字段。
- 契约里“必须出现、可以为 null”的字段，缺省时报错，不当作 null。W4 的 `action` 用 JSON null 表示 pending；字符串 `"null"` 会被拒。
- 无字段的分支（`current`、`recurring`、`unknown`）也拒绝多余字段。
- 解码只检查形状。Quote 是否逐字出现、日历是否合法、目录与任务是否允许，属于 W2 的机械校验。
- 已复核的 23 组 W4 样本输入全部按这些类型解码，并原样回写（`tests/contract_types.rs`）。

## 2. 完整候选保存在哪里（方案 a）

- 完整候选放在 W5 组的 `W5Candidate.contract` 里：`ContractCandidate = { candidate, source, said_on, time_resolutions }`。为空时不序列化，所以旧组的指纹和重放规则不变，也不需要升存储版本。
- 一条记录的完整候选不可变，取产生它的那次已提交回执（`w5_record_contract`）。
  - 重判只更新关系和 UpdateState。
  - 重判必须带完全相同的完整候选，否则整组 needs_rejudge。
  - 要改变完整候选，只能走显式重建与替代映射。
- 回退：删除清洗会把创建回执整体墓碑化。此时记录可能因其他独立证据存活，但已经没有完整候选。召回侧回退为 LegacyRecordView，附上真实的 evidence 和 gaps，不能恢复被删除的候选。recall.v3 实现时落实这一点。

## 3. 投影到现有 Memory（W5 的执行形式）

在召回切到 recall.v3 之前，旧召回路径仍然直接读 Memory。现有 Memory 表达不了的内容按下面两类处理。

**拒绝投影。** 下列情况不提交为现有记录，候选和原文照常保留：

- 主体未解析；
- 不是用户本人的陈述；
- 非偏好、非约束的否定断言；
- 任何类型的方向未知；
- 偏好强度未知；
- 断言缺值。

撤回可以没有新值：它投影为撤回意图，执行时不会插入。

**可以投影，但标为有损。** 下列限定语现有 Memory 无法无损表达：

- 条件；
- 不确定表达；
- 除“单个已换算的有效期”以外的任何时间，包括事件、周期、保存期限、无法换算的时间和多个有效期。

处理方式：

- 不用占位条件冒充：同一 Activity 文本不同就会被判为互斥，不能用它代替。
- 带这类限定语或完整候选未知的记录，不进入 bootstrap/global_context 的无限定全局要求；这些本地结果不执行条件语义推导。
- 普通 recall.v3 保留完整候选限定语；旧数据使用 LegacyRecordView.data 与 gaps 原样表达，不按旧条件、别名或 IS-A 规则筛成确定结论。R4 依据可核准原文判断，R5 按完整性门控。
- memory_list 与来源检索仍附 qualifiers_unmapped（限定语原文或 unknown）；旧 answer_contract 答复门、本地/assisted 执行器已删除。

**单个已换算的有效期**写成 valid_from / valid_to（左闭右开，来源为明示）。保存期限只做标记：不写入有效期，也不写入到期时间。

**由代码给定的字段**：

- 可信任务 ID 写入任务范围。
- `explicit_memory_request` 来自 W3 已核验、绑定到该候选的 remember 请求。
- 其余已移除的字段（tags、extraction_confidence、cross_turn_useful 等）给固定值，不参与判断。
- W5 不跑准入。Episode 的跨轮价值必须在组装 W5 组之前，由 W3、绑定的 remember 请求或 W4 的更新结果决定。

## 4. 尚未做的

- 写入侧接线：W1 / W3 / W4 的模型调用，以及组装 W5 组。
- 无歧义的运行时证据 ID。
- recall.v3 读取 CurrentRecordView。
- W1 / W4 输出的 JSON Schema：接模型调用时，按 `openai.rs` 的严格对象写法手写。
