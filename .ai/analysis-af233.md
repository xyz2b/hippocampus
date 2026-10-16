# Claude 分析（AF233，待 Codex 复核）：为 Anthropic 原生结构化输出简化 schema

## 背景
用户核对官方文档：Haiku 5.5 / Sonnet 5.5 / Opus 5.5 均支持 output_config.format=json_schema（约束解码）。我们的 W1/R1 等请求返回 400 schema_grammar_too_large（Opus/Sonnet/Haiku 均如此）。官方结构化输出文档（platform.claude.com/docs/en/build-with-claude/structured-outputs，2026-10-10 查阅）列出：
- 显式上限：可选参数合计 ≤24；联合类型参数（anyOf 或 type 数组如 ["string","null"]）≤16；另有编译语法大小内部上限、编译超时 180 s。
- 不支持：minimum/maximum/multipleOf、minLength/maxLength、minItems 以外的数组约束、递归 schema、复杂 enum；additionalProperties 必须 false。
- 建议：减少可选参数（每个大约使语法状态翻倍）、扁平化嵌套、拆分请求。
现状：src/model_provider.rs:498 anthropic_schema 只在发送副本中删除整数 minimum/maximum；其余（大量 nullable 联合、pattern “\\S” 等）照发。

## 问题
需要对每个阶段 schema（W1、W3、W4、R1、R4 两种）统计：可选参数数、联合类型参数数、pattern/minLength 等不支持约束、嵌套深度，判断超限点。

## 方案方向（待复核）
A. 发送副本机械等价改写（仅 Anthropic）：
   - nullable 字段 ["X","null"] / anyOf[X,null] → 在发送副本中改为必填非空的显式哨兵或拆为 {present:bool, value:X}？会改变模型可见结构，响应需反向映射回原 schema，再按原 schema 严格核验（与现 integer bounds 同理：发送副本放宽，回收后全量核验）。
   - 删除不支持约束（pattern、minLength），由回收后的原 schema 核验兜底。
   - 风险：反向映射是代码，不得做语义判断；须可逆、无歧义。
B. 按阶段拆分请求：不可行（成本、延迟、语义整体性）。
C. 只对超限阶段做 A，其余原样。
不改其他 provider 的请求与指纹；Anthropic 档案的结构化模式新增一种（native_simplified），与 prompt_schema 区分；生产使用需另批。

## 验证
零调用：每个 schema 改写前后统计、往返映射（原 → 发送副本 → 模拟响应 → 原）一致性、回收后严格核验、其他 provider 请求字节不变。
真实：Haiku 各阶段各 1 次极小冒烟（R1、R4 两种 schema，W1、W3 可选），确认不再 grammar_too_large；然后补跑本轮召回对比 Haiku 88 + 冒烟 3（另批）。

请 Codex 只读复核：统计各 schema 超限点、A 的可行性与最小改写集合（是否只靠删除不支持约束+少量结构改写就够）、可逆映射的边界、指纹与契约影响、允许文件、实测设计与调用量。
