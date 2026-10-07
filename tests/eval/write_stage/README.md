
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

### 七档 W1/W3 对比（AF137–AF139）

`EVAL_COMPARE=af137` 是独立入口，复用八档 `providers.json` 中的七档：
DS Pro、DS Flash、GPT、Opus、27B 开/关思考、9B 关思考。9B 开思考的历史
冒烟档案与产物保留，本轮不运行。W1/W3 各 20 个 reviewed 样本；六档各重复
两次（80 槽），27B 开思考一次（40 槽），完整授权为 520 槽：云端 320、本地 200。
不运行 W4，不调用真实写入管线、不提交记忆。

固定请求使用当前 W1/W3 v3 提示词与 schema，报告同时保留样本标注版本 v1。
仅 AF137 将模型可见的 `source_snapshot.eval_sample` 替换为 `s-<16 位 SHA-256>`：
对固定域 `af137-sample.v1` 与样本 ID 求哈希，同一样本跨档案/重复保持一致，40 项
预检拒绝碰撞。通过报告 `matrix` 中的 `eval_sample` 与真实 `id` 配对反查，不将
报告映射、题型 ID、文件名、focus、expect、标签或注释发给模型。W2 的机械状态、
原因、候选索引、时间解析与原始语义输入保留，不能把它们误当评分标签删除。
盲化发生在协议转换和指纹计算前，未经盲化的 AF137 请求/录制按严格指纹过期，
已有发送账本不能因盲化而重置或自动重发。

历史普通阶段评测及冒烟输入中的语义样本 ID 存在题型提示泄漏，历史分数应据此
审慎解释；沿用该构造的 M1.5、AF96/AF101 也未在本轮追溯修改。AC2 固定输入、
M1.5、AF96/AF101、冒烟及其他历史入口均保持原请求字节与指纹，现有有效录制
不会因 AF137 盲化而过期。旧数据只回放，不追溯改写。

W3 输入由既有适配器展开并通过 W2；W1 主分数按 W2 最终候选计算。云端 medium、
本地 effort=omit 和档案中的思考开关保持固定；此配置不同于生产 W1/W3 默认 low。
输出上限 W1 32768、W3 16384；云端及本地关闭思考超时 360 秒，27B 开思考
1800 秒。输出上限、实际请求、模式、部署证明与超时进入绑定及严格指纹。
Opus 仅此固定评测档案使用 prompt_schema，报告标注未使用约束解码；仍执行原 schema
完整核验，不修补 JSON、不重试。

干跑无需 endpoint 或密钥，不创建任何评测产物，即使设置录制额度也不发送：

```sh
EVAL_COMPARE=af137 EVAL_DRY_RUN=1 \
cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
```

用户已按 AF139 批准 520 次，真实运行由 Claude 执行。沿用上文的四个 BASE_URL
和三个云端 KEY；不打印密钥。本地调用同样计入授权，不得额外预热或探测。建议分批：

```sh
# 云端四档，单进程串行。
EVAL_COMPARE=af137 EVAL_COMPARE_PROFILES=ds-pro,ds-flash,gpt-6.1-sol,opus-5.5 \
EVAL_RECORD=320 \
cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture

# 本地三档，单进程串行。
EVAL_COMPARE=af137 EVAL_COMPARE_PROFILES=qwen-27b-think,qwen-27b-nothink,qwen-9b-nothink \
EVAL_RECORD=200 \
cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
```

也可分成独立进程：DeepSeek 两档合计 `EVAL_RECORD=160`，GPT 或 Opus 单档各为
80，27B 思考单档 40，两档本地关闭思考各为 80。不同云端 provider 可并行；
DeepSeek 两档共用执行锁，三个本地档共用执行锁，避免同时占用同一服务。
锁被占用时拒绝当前执行，不等待、不发送；待另一个进程完成后用原命令续跑。

`EVAL_COMPARE_PROFILES` 接受已知七档 ID 的非空、无重复列表，按固定顺序执行。
可用 `EVAL_COMPARE_CASES` 选择样本完整 ID（不接受前缀、未知或重复项）先试跑：

```sh
EVAL_COMPARE=af137 EVAL_COMPARE_PROFILES=qwen-27b-think \
EVAL_COMPARE_CASES=w3_01_supported_simple EVAL_RECORD=40 \
cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
```

上例发送一个尚未录制槽位，但首次发送前冻结该档全部 40 槽身份及累计额度。
子集、续跑必须继续使用完整档案额度，不能将 `EVAL_RECORD` 改成剩余缺口或子集大小；
除缺省（只回放）、0（所选槽位必须有有效录制或已发送中断标记）外，只接受所选档案完整额度之和。
未知档案、参数覆盖、额度不符与已有授权身份变化均在发送前拒绝。
输入上下文超限项单独列为发送前拒绝，不发送、不冒充语义失败。

所有新产物仅在 `target/eval-providers/compare-af137/`：

- `recordings/<profile>/repeat-<n>/<sample-id>.json`：逐调用原结果与绑定。
- `campaigns/<profile>/authorization.json`、`sends/`：完整 80/40 槽清单及累计发送账本。
- `locks/<resource>/`：DeepSeek、本地 Qwen 或独立云端档的执行锁。
- `reports/<所选档案按固定顺序连接>/report.json`：有新发送时保存的报告。

发送前 fsync started；录制持久化后标 responded。失败也计费占槽，不退款、不重试。
续跑复用有效成功或失败录制；已发送却缺录制的槽位跳过、不重发，其余待录制槽位
继续执行。中断在途仍占原额度，预检按累计已占用加本次待发送数计算，不扩额。
干跑单列中断在途与累计已占用；报告 `interrupted` 清单、各档 `interrupted_inflight`
计数与逐次状态“未录制（中断在途）”保留这些缺口，机械原因是 `interrupted_inflight`。
这些槽位不计评分通过或模型失败，usage/延迟未知，不能据此作完整或稳定性结论。
仅相同请求身份且无录制可跳过；录制过期、授权身份变化仍严格拒绝。
禁止删除账本、换目录或修改指纹来重置额度。AF122/128/132、AC2 与历史 59/4 不变。

严格离线汇总（保留原 endpoint 环境变量，不需要密钥）只打印 stdout，不改产物：

```sh
EVAL_COMPARE=af137 \
cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
```

默认只报告，不因模型评分失败而退出失败；`EVAL_GATE=1` 才要求本次所选档案/样本
全部通过。其他参数/账本错误仍正常报错。`EVAL_RECORD=0` 则要求本次所选槽位
全部有有效录制或已发送中断标记；尚未发送的缺录制或过期槽位不发送并报错。

报告读入七档当前可用录制，包含逐样本矩阵、逐次契约/机械/引用/语义失败、重复
不一致、各档各阶段延迟和 usage；缺失的推理用量为未知。并行运行期间汇总可能尚不
完整，全部进程完成后再离线汇总。两次结果写作 2/2、1/2、0/2，不挑最好一次；
27B 思考的 1/1 明确标为“单次，稳定性未验证”。分层仅表示本轮观测：本地两次通过、
需 DeepSeek、本轮仅 GPT 或 Opus 通过、全部失败；缺数据/调用失败不能证明仅强档
具备能力，重复不一致或本地仅单次通过时暂不作排他分层。全部失败不自动归因为标注
或契约错误。W3 固定候选的通过不代表能补救对应模型 W1 的漏提或错误；未测 W4，
不能据此选择完整写入组合。已参与提示词迭代的 40 个样本也不是独立留出集。

完整输出额度上界 12779520 token（云端 7864320 / 本地 4915200，含思考），
不是预计实际用量，输入另计。27B 思考按已有 603 秒冒烟推算，40 次已约 6.7 小时，
尚未包含两档关闭思考；W3 耗时尚未测定，不能保证本地总计 8 小时。
