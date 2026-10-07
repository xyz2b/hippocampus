
### 显式 provider 与累计 campaign（AF119）

缺省录制格式、目录和历史指纹不变。新增协议使用 recording_version=3，
生产与评测共用请求转换、认证头、最终正文解包和 usage 规范化；五阶段的原 schema
核验与评分不变。新 provider 的默认目录为 `target/eval-providers`，也可指定
`EVAL_PROVIDER_DIR`；子目录按协议/模型标签/思考开关/强度/重复编号隔离。

先干跑（不需要 API key，未创建录制或额度文件）：

```sh
EVAL_PROVIDER_CONFIG='{"protocol":"anthropic_messages","base_url_env":"ANTHROPIC_BASE_URL","api_key_env":"ANTHROPIC_API_KEY"}' \
EVAL_MODEL=claude-opus-5-5 EVAL_MODEL_LABEL=opus-5-5 \
EVAL_REASONING_EFFORT=medium EVAL_CAMPAIGN_DIR=target/eval-providers/campaign-opus-medium \
EVAL_STAGES=w1,w3,w4 EVAL_DRY_RUN=1 \
cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
```

OpenAI 使用 openai_responses、gpt-6.1-sol、OPENAI_BASE_URL/OPENAI_API_KEY。
Qwen 使用 openai_chat、上述完整 Ollama 模型名、QWEN_BASE_URL、effort=omit、
thinking=true/false；还须提供实际 context_tokens、model_revision 和已核实的 context_proof
（含 tokenizer_revision、byte_level_bpe、template_token_bound），详见
[运行时配置](../../../docs/architecture/recall-runtime.md#显式多模型接入af119)。
缺证明或超限项单独报告发送前拒绝，不冒充模型评分失败。

只有明确设置 EVAL_RECORD=N 才允许真实发送；localhost 也受此限制。
EVAL_API_KEY/EVAL_BASE_URL 可显式覆盖配置引用，未设置时只读取该 provider 指定的变量，
不回落到另一 provider 的 key。普通新 provider 请求超时为 360 秒；本轮测试全部为注入请求。

每个新 campaign 的 EVAL_CAMPAIGN_DIR 保存固定授权上限与槽位身份，进程间加锁；
发送前 fsync started，逐调用持久化录制后再标记 responded。续跑累计所有 started/responded，
不退款、不重置额度；已发送但缺录制的槽位拒绝自动重发。需要再试时必须另行授权新槽位/campaign。
严格回放不发送、不要求 key。模型/endpoint/版本/schema/思考/强度/上限变化使录制过期，
不能靠目录标签复用。不得在历史 AC2 目录生成新产物；历史合并 59/4 保持单独核验。

### 八档通路冒烟（AF123）

`providers.json` 固定八档，按 DS Pro、DS Flash、GPT、Opus、27B 开/关思考、
9B 开/关思考的顺序串行执行。每档仅使用已有虚构样本
`w1_01_simple_residence` 发一次 W1；随后离线跑原 schema、decode、W2 和样本评分，
不调用 W3 或其他阶段、不提交记忆。报告分列结构化输出有效性、机械核验与语义评分，
用于核实通路，不作为模型质量排名。每槽输出上限 8192（含思考），超时 360 秒。

Qwen 证明来自 Claude 在 Ollama 0.40.0 的本地探测：tokenizer 为 gpt2 字节级 BPE
（pre=qwen35）；system+user 两条消息的模板开销为 19 token（关闭思考）或 17 token
（开启思考），json_schema 不增加 prompt_tokens，采用上界 256。27B 原名在 `/v1`
默认加载 128K；档案使用已验证以 64K 加载的别名
`qwen3.8:27b-q4_K_M-ctx64k`，context_tokens=65536；9B 为 131072。
档案绑定完整模型摘要；tokenizer_revision 同时绑定该摘要、Ollama 0.40.0 和已知模板
SHA256 前缀 `b507b9c2f6ca642b`。模板证据只有前缀，不声称已取得完整摘要。
部署或模板改变后须重新核实证明、更新绑定，不能沿用旧录制。

运行时能力表精确接纳该 27B `-ctx64k` 别名，支持思考开/关与 effort=omit；
未知别名仍拒绝。任一档案预检失败时，录制入口会在任何发送前拒绝整批。

干跑不要求密钥、不创建产物（即使设置了录制额度也不发送）：

```sh
EVAL_SMOKE=1 EVAL_DRY_RUN=1 \
cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
```

取得真实调用授权后，设置 `DEEPSEEK_BASE_URL`、`OPENAI_BASE_URL`、
`ANTHROPIC_BASE_URL`、`QWEN_BASE_URL`，以及对应的三个远程 `*_API_KEY`。
本地 Qwen 不读取密钥，也不回落到远程 key。录制命令（本任务未执行）：

```sh
EVAL_SMOKE=1 EVAL_RECORD=8 \
cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
```

全部产物在 `target/eval-providers/smoke-af122/`：`recordings/<profile>/`、
共享 `campaign/authorization.json` 与逐次发送记录、`report.json`。
八槽位一次装配后冻结为总额度 8；逐槽选择 endpoint 和认证，不自动重试。
发送前持久化 started，每次调用落盘后写 responded，失败也占用额度且保留结果。
续跑用相同命令与相同 endpoint：复用已有录制，只发送未发送槽位，不重新授权 8 次。
若某槽已 started 但缺录制，则拒绝自动重发，需单独裁决；不得删除额度文件重置计数。

严格离线回放保留相同 endpoint 环境变量，但不需要 key；报告只打印到 stdout，
不修改录制、额度或报告文件：

```sh
EVAL_SMOKE=1 \
cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
```

未设置 `EVAL_RECORD` 时如实报告缺口；设置 `EVAL_RECORD=0` 要求全部录制有效。
身份或请求指纹不一致仍按原规则过期，不能靠更改档案标签复用。
固定入口拒绝 `EVAL_CASES`、`EVAL_REPEAT`、`EVAL_MODEL`、`EVAL_PROVIDER_CONFIG`、
`EVAL_API_KEY`/`EVAL_BASE_URL`、输出上限等自定义评测参数；需要其他比较时另立任务。
