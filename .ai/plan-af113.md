# Claude 方案（草案，待 Codex 复核）：当前版本评测基线 + 多模型对比的准备（AF113）

## 用户要求（2026-10-07）
- PR #4 已合并。批准重建评测基线：用“目前用过的效果最好的模型配置”跑全量 63 题，不考虑费用。
- 后续：更强模型对比（gpt-6.1-sol、Opus 5.5），本地部署对比（Qwen3.5-9B-Q8、Qwen3.8-27B-Q4）。

## 最佳配置的选择（依据已做过的对比）
| 角色 | 选择 | 依据 |
|---|---|---|
| W1 | DeepSeek-V4-Pro + medium | AE6 minimal 6/20 vs medium 27/40；AF54 low≈medium（质量相当），不计成本取 medium |
| W3 | Pro + low | AF103 low 6/8 vs minimal 2/8；Pro medium 未测过 |
| W4 | Pro + medium | 唯一测过的强度 |
| R1 | Pro + medium | Z11 medium 24/40 ≥ minimal 23/40 |
| R4 | Pro + medium（含计算类） | Z11/Z14 medium 19/40 ≥ minimal 18/40；计算类 low 为成本折中，不计成本取 medium |
（以上对比多在旧提示词版本上做，只能说明“历史上不差”；本基线本身就是新的事实。）

## 第 1 步（本轮）：评测配置可选 + 跑 DeepSeek 最佳基线
1. 评测配置参数化：tests/eval/v3_core.rs 目前把 AC2 配置与标签硬编码（LABEL、config()）。改为命名配置（profile）：每个 profile 给出标签、各角色 provider/模型/强度、R4 计算强度、超时；默认 profile 仍是 AC2（保证历史回放与 59/4 不变）；新增 profile `best-deepseek-pro`。录制目录按标签隔离；严格指纹、发送前落盘、额度预检、续跑沿用现有机制。
2. 超时：medium 的 R1/R4 可能比 minimal 慢很多（R4 medium p50≈84 s、p90≈132 s），评测侧超时需覆盖；不改生产默认。
3. 干跑给出精确调用上限与 token 粗估；我核对后直接开跑（用户已批准，不计费用）。
4. 评分：common.v1 绝对评分（recall v6）；新输出会产生大量待复核项，由我起草、用户确认后得出最终分数。

## 第 2 步（后续，单独评审与授权）：多 provider 对比
- 需要的能力：按角色选择 provider——OpenAI Responses（gpt-6.1-sol）、Anthropic Messages（Opus 5.5）、本地 OpenAI 兼容端点（llama.cpp/LM Studio 等运行 Qwen3.5-9B-Q8、Qwen3.8-27B-Q4）。现有客户端为 Responses 结构化输出；Anthropic 与本地端点的结构化输出/JSON schema 支持方式不同，需要适配层与能力探测。
- 需要用户提供：OpenAI 与 Anthropic 的 API 密钥（只放环境变量，不入库）、本地模型服务地址与模型名。
- 评测设计：先用阶段评测（W1/W3/R1/R4 固定输入集）比较各模型，再选组合跑端到端，避免每个模型都跑全量。
- 本轮只在第 1 步的 profile 设计里预留 provider 字段，不实现第 2 步。

请 Codex 复核：最佳配置取舍、profile 设计与默认兼容、超时与额度、对第 2 步的预留是否合理、遗漏的风险；给出第 1 步的最终范围。
