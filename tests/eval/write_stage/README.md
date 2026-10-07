
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
Responses/Chat 将 base_url 视为完整 API 前缀，分别追加 `/responses`、
`/chat/completions`：传根地址不会自动加 `/v1`，传 `/v1` 或 `/api/v3` 则保留该前缀。
这保留已验证的 DeepSeek `/api/v3/responses` 与 OpenAI 代理 `/responses`。
Anthropic 按 SDK 根地址约定追加 `/v1/messages`；也接受已带 `/v1` 的基址，不重复添加。
Qwen 使用 openai_chat、上述完整 Ollama 模型名、QWEN_BASE_URL、effort=omit、
thinking=true/false；还须提供实际 context_tokens、model_revision 和已核实的 context_proof
（含 tokenizer_revision、byte_level_bpe、template_token_bound），详见
[运行时配置](../../../docs/architecture/recall-runtime.md#显式多模型接入af119)。
缺证明或超限项单独报告发送前拒绝，不冒充模型评分失败。

只有明确设置 EVAL_RECORD=N 才允许真实发送；localhost 也受此限制。
EVAL_API_KEY/EVAL_BASE_URL 可显式覆盖配置引用，未设置时只读取该 provider 指定的变量，
不回落到另一 provider 的 key。普通分阶段入口请求超时为 360 秒；冒烟按下述档案执行。

每个新 campaign 的 EVAL_CAMPAIGN_DIR 保存固定授权上限与槽位身份，进程间加锁；
发送前 fsync started，逐调用持久化录制后再标记 responded。续跑累计所有 started/responded，
不退款、不重置额度；已发送但缺录制的槽位拒绝自动重发。需要再试时必须另行授权新槽位/campaign。
严格回放不发送、不要求 key。模型/endpoint/版本/schema/思考/强度/上限变化使录制过期，
不能靠目录标签复用。不得在历史 AC2 目录生成新产物；历史合并 59/4 保持单独核验。

### provider 通路冒烟（AF123、AF128）

`providers.json` 固定八档，按 DS Pro、DS Flash、GPT、Opus、27B 开/关思考、
9B 开/关思考的顺序串行执行。每档仅使用已有虚构样本
`w1_01_simple_residence` 发一次 W1；随后离线跑原 schema、decode、W2 和样本评分，
不调用 W3 或其他阶段、不提交记忆。报告分列结构化输出有效性、机械核验与语义评分，
用于核实通路，不作为模型质量排名。档案按生产阶段列出输出上限：W1 32768、
W3/W4 16384（含思考）；冒烟只使用 W1 上限。云端和本地关闭思考档超时 360 秒，
本地开启思考档超时 1800 秒。输出上限进入发送正文，超时进入请求绑定和指纹，
不作为字段发给模型。旧的 8192/360 秒录制如实过期，不修改旧指纹。

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
EVAL_SMOKE=1 EVAL_SMOKE_PROFILES=ds-flash,opus-5.5,qwen-27b-think,qwen-9b-think \
EVAL_DRY_RUN=1 \
cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
```

取得真实调用授权后，设置 `DEEPSEEK_BASE_URL`、`OPENAI_BASE_URL`、
`ANTHROPIC_BASE_URL`、`QWEN_BASE_URL`，以及对应的三个远程 `*_API_KEY`。
本地 Qwen 不读取密钥，也不回落到远程 key。录制命令（本任务未执行）：

```sh
EVAL_SMOKE=1 EVAL_SMOKE_PROFILES=ds-flash,opus-5.5,qwen-27b-think,qwen-9b-think \
EVAL_RECORD=4 \
cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
```

`EVAL_SMOKE_PROFILES` 是已知档案 ID 的逗号列表，拒绝空列表、未知 ID 和重复项；
按档案固定顺序执行，不受列表顺序影响。省略时选择全部八档。
`EVAL_RECORD` 必须为所选档数（上例 4）；不接受选择四档却授权 8 的组合。

新产物在 `target/eval-providers/smoke-af128/<按固定顺序以--连接的档案ID>/`：
`recordings/<profile>/`、共享 `campaign/authorization.json` 与逐次发送记录、`report.json`。
每个所选集合一次装配后冻结为总额度（上例 4），与旧八槽 campaign 独立；
逐槽选择 endpoint 和认证，不自动重试。相同集合换顺序仍命中同一目录与账本。
旧 `smoke-af122/` 下的八次录制、发送账本与报告保持只读，包括已通过的四档。
干跑读取旧录制并如实提示过期，不复制或改写，不将其当作新配置的有效回放。
发送前持久化 started，每次调用落盘后写 responded，失败也占用额度且保留结果。
续跑用相同命令与相同 endpoint：复用已有录制，只发送未发送槽位，不重新授权整批。
若某槽已 started 但缺录制，则拒绝自动重发，需单独裁决；不得删除额度文件重置计数。

严格离线回放保留相同 endpoint 环境变量，但不需要 key；报告只打印到 stdout，
不修改录制、额度或报告文件：

```sh
EVAL_SMOKE=1 EVAL_SMOKE_PROFILES=ds-flash,opus-5.5,qwen-27b-think,qwen-9b-think \
cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
```

未设置 `EVAL_RECORD` 时如实报告缺口；设置 `EVAL_RECORD=0` 要求全部录制有效。
身份或请求指纹不一致仍按原规则过期，不能靠更改档案标签复用。
固定入口拒绝 `EVAL_CASES`、`EVAL_REPEAT`、`EVAL_MODEL`、`EVAL_PROVIDER_CONFIG`、
`EVAL_API_KEY`/`EVAL_BASE_URL`、输出上限等自定义评测参数；需要其他比较时另立任务。

### Opus 无约束解码实验（AF132，仅评测例外）

Opus 的 W1 原生 schema 曾因 `compiled grammar is too large` 返回 400；这不证明
其他阶段必然失败，也不能根据未报告 usage 推断零计费。仅 `opus-5.5` 档案显式设置
`structured_mode=prompt_schema`，其余档案缺省 `native_schema`。生产客户端拒绝
prompt_schema，普通 `EVAL_PROVIDER_CONFIG` 也不接受该模式。

实验请求不发送 `output_config.format`，在原 system 提示词末尾附固定
`prompt-schema.v1` 指令和未经删改的原 schema，保留 medium、自适应思考、W1 输出上限
32768 与超时 360 秒。发送前对追加后的完整请求做上下文预检。该模式没有约束解码保证；
返回仍须通过原 schema、decode 与 W2，围栏、前后缀和非法 JSON 一律失败，不修补、
不自动重试。报告分列 `transport_valid`（完整响应通路）、`provider_output_valid`
（原 schema）、`mechanical_valid`（decode/W2）与 `semantic_score`，并明确
`structured_mode=prompt_schema`、`constrained_decoding=false`。

模式、固定附加指令及其版本、实际请求进入绑定和指纹；仅 Opus 的旧录制过期，
原生模式的请求与绑定不变。单独选择 Opus 使用新的独立目录
`target/eval-providers/smoke-af132/opus-5.5/`，总额度固定为 1；不读取密钥的干跑：

```sh
EVAL_SMOKE=1 EVAL_SMOKE_PROFILES=opus-5.5 EVAL_DRY_RUN=1 \
cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
```

AF130 已授权的确认调用由 Claude 执行，本次实现不执行：

```sh
EVAL_SMOKE=1 EVAL_SMOKE_PROFILES=opus-5.5 EVAL_RECORD=1 \
cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
```

续跑沿用同一命令、endpoint、目录与账本：失败也占用唯一槽位，已有失败录制只回放，
已 started 但缺录制时拒绝重发。不能删除旧账本重置额度。AF122/AF128 的 campaign
与产物保持原样；混合旧 cohort 的 Opus 请求身份已改变，不能沿用其授权重发。
当前入口将含 Opus 的混合 cohort 限为干跑或只读回放；真实录制须只选
`opus-5.5`、`EVAL_RECORD=1`。上述 AF128 四档录制命令描述历史版本，不用于本次续跑。

HTTP 非 2xx 错误正文最多读取 64 KiB 加一个溢出判定字节；录制、账本和运行时轨迹
只保留 HTTP 状态、白名单错误类型、受限的响应头请求 ID、固定安全失败码及数字用量。
语法超限记为 `schema_grammar_too_large`；未知、非 JSON、超大或不可读正文分别分类，
不保存自由文本，防止服务商回显原话或凭据。无 usage 记未知，不退款、不自动重试。
