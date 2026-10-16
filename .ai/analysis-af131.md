# Claude 分析与方案草案（AF131）：Opus 5.5 返回 400

## 事实
- 重放同一请求读到错误正文：`The compiled grammar is too large, which would cause performance issues. Simplify your tool schemas or reduce the number of strict tools.`（两次 400，未生成，无计费 token）。
- W1 发送副本：output_config.format json_schema 约 9 KB，含 enum 32、pattern 25、anyOf 9。去掉全部 pattern、去掉 thinking/effort 后仍 400——Anthropic 的约束解码无法编译我们这个规模的 schema。W3/W4/R1/R4 schema 规模相近或更大，预计同样失败。
- 另一缺陷：我们的错误记录只存了“HTTP 400 Bad Request”，没存服务返回的错误正文，导致无法从录制定位原因。

## 方案草案
1. Anthropic 结构化输出降级为“提示词内给出 schema + 本地严格核验”：不发送 output_config.format（不做约束解码），在 system 末尾追加固定说明“只输出一个符合以下 JSON Schema 的 JSON 对象，不要其他文字”并附原 schema；返回后照常走现有 decode/核验（strict 语义由本地核验保证）。正文若带前后缀文字或代码围栏，一律判失败，不做修补。
   - 该模式进入请求绑定（adapter 能力字段，如 structured_mode=prompt_schema），报告中标注“Opus 未使用约束解码”，对比时作为口径差异说明。
   - 备选：tool_use + tool_choice auto + strict:false（无语法编译），但模型可能不调用工具；不如 prompt_schema 简单可控。
2. 错误正文落盘：HTTP 非 2xx 时把服务返回的错误 message（去除凭据、截断到固定长度、过滤请求 ID 外的可识别信息）写入录制与轨迹的白名单字段。
3. 验证：Opus 单独 1 次 W1（用户已授权直接跑）。

请 Codex 复核：降级方式是否可接受（与“不回退 JSON mode / 不修补 JSON”的原则如何平衡）、绑定与报告标注、错误正文的脱敏范围；给出允许修改文件。
