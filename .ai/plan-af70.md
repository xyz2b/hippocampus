# Claude 修复方案（草案，待 Codex 复核）— 依据 .ai/analysis-af69.md 与 Codex 评审 18

原则：不改冻结契约、不新增召回状态、不让代码判语义；提示词只写规则不放例句，不使用评测样本或用户私有原文。

## A. 首轮候选被 W2 拒收
采纳 Codex 意见：现行契约（1.5/2.1）要求多断言共用完整原句、通用 fact 的 value=sentence，W1 low 违反了这一点。本轮**不改契约、不做分句**。
1. W1 提示词：在“BEFORE RETURNING CHECK”中显式加入“relation=fact 的 value.text 与 value.quote 必须与 sentence 完全相同（含句末标点）；断言粒度由 relation/subject/speech/times/限定语区分，而非截短 value”。
2. W2 机械规范化（仅此一种）：relation=fact 且 value 恰为 sentence 去掉句末标点（。！？；.!?;…及其组合）时，规范为 sentence；其他任何不一致仍 fact_sentence_mismatch。规范化后照常进入 W3。
3. 已有用户私有首轮来源不自动补处理；是否用 memory_source_reprocess 重提取由用户另行授权。

## B. R4 时间依据引用错来源
1. R4 提示词：每个 times[] 的 quote 必须出自该事实 evidence 中引用的来源；若时间来自另一来源，须把那条来源的引用也加入该事实的 evidence，并由模型判断其适用性；不得引用未列入 evidence 的来源中的时间片段。
2. 代码不放宽：仍只在该事实引用的来源中核验时间；不按同一行程或来源邻近推断关联；不引入部分成功。
3. 诊断落盘：校验失败时把 ValidationDiagnostic 的 field、rule、cause 写入轨迹 model_check（不含原文），W2 拒收同理已有 reason。
4. 机械补漏：literal_time_matches 识别明确的“M.D”“M.D-M.D”数字月日格式，只核对月、日与解释是否一致；年份归属仍不由代码判断。

## C. 征询被当作计划
1. W1/W3 提示词：区分“用户实际的意向/安排”（含条件性、试探性意向，保留其条件与不确定性）与“征求意见、评估可行性的提议”（speech=question 或 hypothetical）；不能按问号或关键词机械判断。
2. W1 提示词：计划优先使用通用 fact 或语义合适的关系，避免使用表示已发生经历的关系（如 visited）。不在 W2 增加硬性禁令（属新增政策，暂不做）。

## D. 重复/拆分
采纳 Codex 纠正：记忆 5/6 不是结构重复（episode+时间 vs constraint+不确定性）。不做代码去重。W1 提示词：同一断言只输出一个候选；只有确实是不同主张时才拆分，类型选择须与原话一致。

## E. 延迟
不改行为。轨迹 model_end 的 accounting 补记推理 token（若 API 返回 output_tokens_details.reasoning_tokens），用于以后分析。

## 版本与范围
- 提示词改动：W1/W3 升 v3？R4 升 v6？——请 Codex 判断按冻结规则是否必须升版（我倾向：提示词语义收紧但输出结构不变，按既有惯例提示词变化通过指纹过期体现，不升契约号；若规则要求升版则升）。
- 允许修改：src/write_models.rs、src/recall_models.rs、src/w2.rs（规范化与 M.D 识别）、src/r4.rs（诊断）、轨迹记录处、对应测试。
- 验证：零调用注入测试（缺句号规范化/其他不一致仍拒；时间跨来源仍拒、补引用后通过；诊断字段落盘；M.D 格式正反例）；全套 0 失败；59/4 不变。真实重测另行申请（仍用虚构样本，不发送用户私有原文）。
