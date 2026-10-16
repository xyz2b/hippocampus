# Claude 方案（AF122，待 Codex 复核）：各 provider 真实可用性冒烟 + 评测 provider 档案

1. 评测侧 provider 档案（不改生产默认）：
   - ds-pro：openai_responses，DEEPSEEK_BASE_URL/KEY，ep-20261003130756-trhj6
   - ds-flash：openai_responses，同上，ep-20260923215039-vxgxg
   - gpt-6.1-sol：openai_responses，OPENAI_BASE_URL/KEY，effort medium
   - opus-5.5：anthropic_messages，ANTHROPIC_BASE_URL/KEY，claude-opus-5-5，effort medium
   - qwen-27b-think / qwen-27b-nothink：openai_chat，QWEN_BASE_URL/KEY，qwen3.8:27b-q4_K_M，context 65536
   - qwen-9b-think / qwen-9b-nothink：同上，qwen3.5:9b-q8_0，context 131072
   - Qwen 部署证明（Claude 本地实测）：tokenizer.ggml.model=gpt2（字节级 BPE，pre=qwen35）；模型摘要 27B 25b843619e944cd0…、9B a8ee95bc3592ebaa…（取完整 digest）；tokenizer_revision 绑定同一摘要；模板开销实测 1 条消息 13、2 条 19 token，json_schema 不注入提示词 → template_token_bound 取 256（>10 倍余量）。
2. 冒烟：每个档案只发 1 次 W1 请求（一份最短的虚构 W1 样本），共 8 次；独立 campaign 额度 8；记录成功/失败、延迟、usage、是否通过现有核验。目的只是确认真实通路，不比较质量。
3. 之后再设计第 ② 步写入侧对比（样本与调用量另报用户批准）。

请 Codex 复核：档案字段是否完整、Qwen 证明数值是否足以满足 check_context 的要求（摘要格式、tokenizer_revision 取值）、冒烟设计与额度；给出允许修改文件。

## 补充证据（Claude 本地实测，2026-10-07，Ollama 0.40.0）
- 通过 /v1 调用时 Ollama 使用服务端默认 num_ctx=131072：27B 原名会以 128K 加载（26 GB，11% CPU / 89% GPU）。已创建别名 `qwen3.8:27b-q4_K_M-ctx64k`（Modelfile：FROM qwen3.8:27b-q4_K_M + PARAMETER num_ctx 65536），实测 /v1 调用加载为 65536、20 GB、100% GPU。27B 档案必须用此别名。
- 完整 digest：27B 别名 d8e86c06dd75962a2119a900e4fa0acb476e551220106c94577525eaa70a7833（原模型 25b843619e944cd0ae6069f94ff4e5e26a16e109ccbc0a66a0f05979ed70098e）；9B a8ee95bc3592ebaa624014ca8b429d25e0ef5f45030dc35786da7ff7e9e1b60a。
- tokenizer.ggml.model=gpt2（字节级 BPE），pre=qwen35；两模型 template sha256 前缀 b507b9c2f6ca642b（相同）。
- 模板开销（prompt_tokens，内容各 1 字符）：1 条消息 13；system+user 19（think 关）、17（think 开）；json_schema 格式不改变 prompt_tokens（未注入正文）。实际请求为 system+user 两条，开销 ≤19，template_token_bound=256 留 >13 倍余量。
- 证明数据需与服务版本 0.40.0、template 摘要一起纳入 revision 绑定。
