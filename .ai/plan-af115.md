# 多模型接入：探测结果 + Claude 方案草案（AF115，待 Codex 复核）

## 探测结果（2026-10-07，脚本在 job 临时目录，极小请求）
凭证：~/.zshrc 中 DEEPSEEK_* / OPENAI_* / ANTHROPIC_* / QWEN_* 的 API_KEY 与 BASE_URL（只读环境变量，不入库、不打印）。

| 服务 | 结果 |
|---|---|
| OpenAI 代理（OPENAI_BASE_URL，token.aihezu.dev） | 无 User-Agent 时 Cloudflare 403（code 1010）；带 UA 后 /v1/models 可列出 gpt-6.1-sol 等；**/v1/responses + text.format json_schema strict 正常**，4.6 s。默认 reasoning effort=medium。**注意：不传 instructions 时代理会注入约 4k token 的 “You are Codex…” 系统提示；传入 instructions 后使用我们的（input 61 token）**。/v1/chat/completions 也通，但同样被注入系统提示。 |
| Anthropic 代理（ANTHROPIC_BASE_URL） | 带 UA；/v1/models 含 claude-opus-5-5；**forced tool_choice 不支持**（400）；**output_config.format=json_schema 正常**，3.7–4.2 s；output_format(beta) 已废弃；output_config.effort（low/medium）可用；thinking adaptive 可用；tool_choice=auto 也能出结构，但不保证。 |
| 本地 Qwen（QWEN_BASE_URL=127.0.0.1:11434/v1，Ollama） | 模型 qwen3.5:9b-q8_0、qwen3.8:27b-q4_K_M；chat/completions + response_format json_schema：27B 正常（18.9 s）；**9B 返回空 content**（推测思考占满 max_tokens=300，未诊断）。 |
| DeepSeek（ark） | 现有生产通路（Responses），Pro 与 V4.1-Flash 均可用。 |

## 方案草案
1. Provider 抽象：在现有 OpenAiClient（Responses）之外，按角色可配置 provider：
   - `openai_responses`：现有通路（DeepSeek ark、OpenAI 代理）。必须总是传 instructions（系统提示），请求带固定 User-Agent。
   - `anthropic_messages`：/v1/messages，system=提示词，output_config.format=json_schema（strict 等价）、output_config.effort 映射推理强度；max_tokens 映射输出上限；usage 映射（含 thinking_tokens）。
   - `openai_chat`：本地 Ollama 等 OpenAI 兼容 chat/completions，response_format json_schema；思考模型的处理（关闭思考或提高输出上限、读取 reasoning 字段）需诊断后定。
2. 配置：write_models/recall_models 每个角色增加 provider（缺省 openai_responses，保持现状与指纹兼容）；base_url/key 从对应环境变量名读取（配置里写变量名，不写值）；模型 ID 仍不进入模型可见内容。
3. 统一的结构化输出、错误分类（HTTP/超时/解码/截断）、usage 记账、严格指纹（provider 与 endpoint 进入绑定）、网络门控与预算沿用现有机制；不自动重试。
4. 推理强度映射表：minimal/low/medium/high → 各 provider 的对应参数；不支持时明确报错，不静默降级。
5. 评测：阶段评测（write_stage_eval）支持按 provider 选择模型标签；录制目录按 provider/模型/强度隔离。
6. 本轮只做接入与零调用测试；真实可用性再用每个 provider 1–2 次极小请求验证（已在探测中完成一次）。

请 Codex 复核：抽象位置与最小改动、Anthropic/Chat 的结构化输出与 strict 语义差异（schema 限制、额外字段）、思考模型（Qwen 9B、Opus 思考）的处理、指纹与录制兼容、预算记账、遗漏风险；给出最终范围与允许修改文件。

## 补充（用户 2026-10-07）
- 本地模型名：`qwen3.5:9b-q8_0`、`qwen3.8:27b-q4_K_M`（Ollama），当前 context length 设为 64k。
- 影响：现有预算 max_input_bytes=256000（中文约可达 8 万–17 万 token 上限），远超 64k 上下文；且思考 token 与输出也占上下文。需要按 provider/模型配置上下文上限，请求前机械检查“输入 + 输出上限”不超过上下文窗口，超出则明确失败（不截断、不静默丢证据）。Ollama 对超长输入可能静默截断，必须在发送前拦住。
