
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

取得真实调用授权后，设置 `HUOSHAN_DEEPSEEK_BASE_URL`、`OPENAI_BASE_URL`、
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

### AF150 / AF151 离线重评（2026-10-08）

本轮只修订期望及评分器，不改变输入、ID、提示词、schema、档案、录制、账本或请求
指纹。修订样本和报告行带 `label_revision`（AF150/AF151、2026-10-08）；未修订样本
仍使用原标注。完整分支、verdict 允许集合与可选候选的规则见
[样本说明](../w1/README.md#af150--af151-标注修订2026-10-08)。

保留真实录制时使用的四个 BASE_URL，取消 EVAL_RECORD、EVAL_GATE 和过滤参数，
无需任何 API key。只读取 `compare-af137/recordings`，报告输出到独立目录：

```sh
mkdir -p target/eval-providers/compare-af137/reports-af151
env -u EVAL_RECORD -u EVAL_GATE -u EVAL_COMPARE_PROFILES -u EVAL_COMPARE_CASES \
EVAL_COMPARE=af137 \
cargo test --offline --test write_stage_eval write_stage_eval_suite -- --exact --nocapture \
> target/eval-providers/compare-af137/reports-af151/replay.log 2>&1
```

日志内含 JSON 报告及预检结果；本次交付另提取 `report.json`，新旧对照保存为
`comparison.json` 与 `summary.md`。旧 `reports/` 不覆盖。应核对 sent_calls=0、520 槽、
零过期；原有 1 项中断在途及 3 项调用失败仍保留，不算通过。endpoint 未配置或改变
时录制会按严格绑定过期，不能为重评修改绑定或指纹。

W3 `verdict_result` 分列候选/请求 verdict、必需 aspects 与 episode_value；机械核验
失败记为 unavailable，不用备选掩盖。`coverage_result` 保留模型的 complete/gaps 与
missing，另列 scored、passed、valid 和 guard_passed。coverage 不计分的样本 passed
为 null；空 gaps、无出处引用及 05 缺母亲断言仍使整题失败。每档汇总分别提供
`w3_verdict` 和 `w3_coverage`，观察到的 coverage 状态不等于质量通过。

报告中的严格分层与重复一致性规则不变。新旧四类“预筛”对照若使用任一次通过
（包括 27B 思考单次），必须单独标注口径，不能将它当作两次均通过或稳定性结论。
被拒收候选是否算覆盖仍是提示词待澄清项，本轮不修改提示词或冻结契约。

### AF154 / AF155 离线重评（2026-10-08）

四个样本修订及观察题的规则见
[样本说明](../w1/README.md#af154--af155-标注修订2026-10-08)。本轮只改期望与报告口径，
不改模型输入、ID、提示词、schema、provider、请求指纹、录制或账本；仍为 40 样本、
520 固定槽。主语义评分与分层排除 W3 19，共 39 个计分样本、507 个计分槽；缺录制
或调用失败仍单列，不能算通过。观察题的 13 槽保留完整原始输出、失败字段与机械失败。

有效观察输出状态为“观察”，`observed_status` 保留按原标签评分的结果；主 summary 的
passed/evaluated/failed 不含观察题，另列 observation_only/observed/observation_failed。
机械失败的观察行仍为“失败”，显式 `EVAL_GATE=1` 不会豁免；输入无效照常报错。
各档 `observations` 保留判断与失败字段；`w3_10_verdict_distribution` 分列
contradicted/insufficient/supported/unavailable，不能把临时双收理解为运行时等价。
主 W3 verdict/coverage 汇总排除观察题，其完整 coverage 仍可在观察行中查阅。

保留录制时的四个 BASE_URL，清除录制/过滤等其他 EVAL 参数，无需密钥。以
`EVAL_RECORD=0` 进行严格离线回放，缺口或过期会报错，不创建客户端、不发送：

```sh
mkdir -p target/eval-providers/compare-af137/reports-af155
env -u EVAL_GATE -u EVAL_COMPARE_PROFILES -u EVAL_COMPARE_CASES -u EVAL_DRY_RUN \
EVAL_COMPARE=af137 EVAL_RECORD=0 \
cargo test --offline --test write_stage_eval write_stage_eval_suite -- --exact --nocapture \
> target/eval-providers/compare-af137/reports-af155/replay.log 2>&1
```

日志提取为该目录的 `report.json`，与 `reports-af151/report.json` 对照保存为
`comparison.json`、`summary.md`。应核对 sent_calls=0、520 槽指纹/账本身份不变、
零过期，原有 1 项中断在途与 3 项调用失败仍保留。`reports/` 与 `reports-af151/`
不得覆盖。预筛与严格分层继续分开；观察题不进入任何能力分层。

### AF156 阿里云 Qwen 写入侧对比

`EVAL_COMPARE=af156` 固定两档 `qwen37-flash-think` / `qwen37-flash-nothink`，
使用快照 `qwen3.7-flash-2026-07-15`，分别发送 reasoning_effort=medium/none。
两档均为 openai_chat + native json_schema strict；返回仍执行原 schema、W2/W3
机械核验，不修补、不重试、不跟随重定向。此处只增加评测档案，生产配置不变。

基址为 `QWEN_BASE_URL`（阿里云 HTTPS，含 DashScope 与 ws-*.maas 工作空间域名，
主机以 .aliyuncs.com 结尾；结尾 /compatible-mode 或
/compatible-mode/v1 均可），认证为 `QWEN_API_KEY`，发送 Bearer 与固定 User-Agent。
服务地址拼成 /compatible-mode/v1/chat/completions，不改变已有 Responses、本地 Chat
或 Anthropic 的 URL。其他主机、HTTP、本地地址、错误路径、空密钥在发送前拒绝。
环境变量必须在当前评测进程中生效；改 ~/.zshrc 不会更新已启动进程的环境。

W1/W3 的完整输出额度为 32768/16384，包含思考与回答，云端超时 360 秒。
开思考显式传 `thinking_budget=max_completion_tokens/2`（W1 16384、W3 8192），
为最终回答保留另一半额度；关思考不传。该字段进入 wire 与严格请求指纹，
旧 think 请求/录制不会当作新请求复用。不能依赖服务默认 thinking_budget=32768，
否则总输出上限小于或等于思考额度时会 HTTP 400。
采用 `max_completion_tokens`；DashScope 的 `max_tokens` 只限制回答，不含思考，
因此不能用于本次总输出额度。官方允许最多 10 token 误差，干跑另列保守预留。
usage 中 completion_tokens 映射为总输出，reasoning_tokens 单列，缺失时保留未知。
超时或截断占原槽额度，落盘后不会自动重试；没有长样本证据，暂不扩大 360 秒超时。
依据：[Chat API](https://help.aliyun.com/zh/model-studio/qwen-api-via-openai-chat-completions)。

上下文政策固定为 `dashscope-text-context.v1`，进入 model_revision 与严格请求绑定。
官方快照上下文 1,000,000、思考输入上限 983,616、输出上限 131,072；两档统一使用
较小输入限制。仅接受纯文本 system/user 两条消息，对完整 wire（含提示词、输入、
schema）的 UTF-8 字节数预检输入/上下文限制，并预留输出及 10 token 误差；超限、
容量配置不符或非文本请求拒绝，不截断。字节预检不是未公开的云端 tokenizer/template
证明，不填充 Ollama 的 ContextProof；真实分词容量由官方云端超限报错规则保证。
只支持官方直连接口，不能把该保证用于任意兼容代理。
依据：[快照容量](https://help.aliyun.com/zh/model-studio/qwen3-7-flash)、
[输入超限错误](https://help.aliyun.com/zh/model-studio/error-code)。

沿用 AF137 的 40 样本与相同盲化标识，使用当前 AF155 标注：39 个计分、1 个观察。
两档各重复两次、各 80 槽，合计 **160 次**（其中 4 个观察槽仍调用，但不计主评分
或分层，机械失败不豁免）。新增产物只在 `target/eval-providers/compare-af156/`，
结构仍为 recordings/<profile>/repeat-<n>、campaigns/<profile>、locks、reports。
独立目录避免更改 AF137 冻结身份或复用旧额度；发送前逐次 fsync，录制逐次落盘，
中断在途跳过但继续占额度。两档共用 DashScope 执行锁，推荐单进程串行。

所有阶段录制/比较模式按档案统计连续同类 provider HTTP 错误或
`incomplete(max_output_tokens)`：
相同 http_status + 已脱敏 error_type/code 达到 3 次，即停止该档的后续槽位，
报告标为“熔断未发送”，不创建这些槽位的发送账本或录制，也不占额度。
成功或其他错误类别会重置连续计数；模型语义/核验失败
不触发该熔断。状态在 `campaigns/<profile>/provider_circuit.json` 持久化，
绑定冻结 authorization，续跑不会自动解除。其他档可继续运行；解除需另行审批，
不能删除状态文件后自动重发。无 campaign 的通用录制在本次进程中同样熔断。

AF160 用户授权重建 think 档：78 条 HTTP 400 失败录制及完整旧 campaign
已移动到 `archive-af160/recordings/qwen37-flash-think/` 与
`archive-af160/campaigns/qwen37-flash-think/`，保留原始字节与授权。
旧账本另有 1 条 started、未录制的在途记录，共占用 79 槽，也随 campaign 归档。
新 think 请求含 thinking_budget，独立重新获得 80 槽；nothink 原 80 槽授权和
0 已占用保持不变，历史 AF137 520 槽与 AF155 报告不变。本次修复不发送真实调用。

干跑无需密钥、不建目录、不发送（未设置 BASE_URL 时只用官方北京地址作规划占位）：

```sh
EVAL_COMPARE=af156 EVAL_DRY_RUN=1 \
cargo test --offline --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
```

**真实录制需完成 diff 与干跑复核后另行授权发送**。批准后，两档一起运行仍只能用
完整额度 160；选一档用 80，子集仍保留该档完整 80 槽清单与累计额度，不能按缺口扩额：

```sh
EVAL_COMPARE=af156 EVAL_RECORD=160 \
cargo test --offline --test write_stage_eval write_stage_eval_suite -- --exact --nocapture

# 严格零发送回放；缺录制/过期则报错，已发送的中断槽只标缺口。
EVAL_COMPARE=af156 EVAL_RECORD=0 \
cargo test --offline --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
```

报告并列九档：历史 AF155 七档只读，标 reference_only；新两档单列
new_profile_report（160 槽）。合并 fixed_slots=680 仅表示九档观测行，
authorized_slots=160、historical_slots=520，新增发送只能来自 AF156。
发送前重新构造历史请求并严格核对全部 520 槽指纹、状态与 reports-af155，
不符则整批拒绝。沿用观察题、verdict/coverage 分列、逐次失败字段及稳定性规则；
分层增加“阿里云 Qwen 即可（本轮两次均通过）”。历史本地层只说明既有观测能力，
不表示继续使用本地模型；历史单次或重复不一致仍不能作排他能力结论。

本地四档保留原 provider.base_url_env=QWEN_BASE_URL 作为**历史绑定身份**；新增
档案级 endpoint_env=QWEN_LOCAL_BASE_URL，仅用于路由，位于指纹之外。它们不再
允许真实录制。未设置 LOCAL 时，离线从历史录制读取原 endpoint，然后重新构造并
核验完整指纹，绝不改读云端 QWEN_BASE_URL；旧输入、提示词、schema 等发生变化
仍报过期，不覆盖指纹。无历史地址且未设 LOCAL 时，发送凭据检查明确报缺 LOCAL。
旧冒烟、AF137 录制及账本保持不变；历史回放无需云端或本地密钥。

### AF162 截断诊断

openai_chat 返回 `finish_reason=length` 时，录制保留规范化的
`status=incomplete`、`incomplete_details.reason=max_output_tokens` 与 usage，
另存 `diagnostic_fragments`：正文首/尾各最多 2048 UTF-8 字节，
reasoning_content 尾最多 1024 UTF-8 字节（如有）。边界按完整字符截取，
记录总字节数与 diagnostic_only=true、scoring_eligible=false；不保存完整正文或
完整思考，不把片段放入 output/output_text，不修补 JSON、不参与评分。
此处理对 DashScope 与其他 openai_chat 相同，正常完成的回答保持原流程。

同档连续 3 次 max_output_tokens 截断也触发持久化熔断。HTTP 错误与截断分别
计数，交替出现不算同类；其他 incomplete 原因不套用此规则。诊断不能保证确认
“严格 schema + 思考导致重复”，片段仅供人工定位，不能自动做语义归因。

`EVAL_COMPARE=af162` 是独立诊断入口，目录固定
`target/eval-providers/compare-af156/diag-af162/`。`EVAL_CASES` 的前缀必须唯一选中
一个样本（默认 w1_01）；可选择 AF156 两档，各仅 repeat=1、累计额度 1，
完整上限 **2 次**（think 1 + nothink 1）。请求沿用正式提示词、schema、盲化 ID、
输出与思考额度，不通过改参数掩盖问题。正式 AF156 账本与额度不读取/不占用。
每次发送前落盘、不重试、在途不重发，改样本/请求后不能复用原诊断授权。
诊断报告标 diagnostic_only，不进入正式比较分数。

仅干跑（不读取密钥、不创建产物、不发送）：

```sh
EVAL_COMPARE=af162 EVAL_CASES=w1_01 \
EVAL_COMPARE_PROFILES=qwen37-flash-think,qwen37-flash-nothink \
EVAL_RECORD=2 EVAL_DRY_RUN=1 \
cargo test --offline --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
```

Claude 复核后才可发送：使用相同命令，去掉 EVAL_DRY_RUN=1。
只选一档时 EVAL_RECORD=1；严格零发送回放使用 EVAL_RECORD=0。
不能传 EVAL_REPEAT、覆盖输出额度或改变 campaign 目录扩额。

AF162 用户授权归档 3 条无正文截断录制及完整 think campaign，移至
`archive-af162/recordings/qwen37-flash-think/`、
`archive-af162/campaigns/qwen37-flash-think/`。账本还含 1 条 started 在途记录，
也完整保留。正式 think 档恢复 80 槽；nothink 的 0 已占用授权、AF160 归档与
历史 520 槽/AF155 报告不变。本次准备不发送真实调用。

### AF163 / AF166 云端比较、冒烟与真实 schema 诊断

当前正式比较五档：`gpt-6-luna`（Responses，OPENAI_*），
`claude-sonnet-5-5`、`claude-haiku-5-5`（Messages，ANTHROPIC_*），
`qwen38-flash-think`、`qwen38-flash-nothink`（Chat，QWEN_*）。
前三档 effort=medium；Qwen3.8 用 effort=omit，其真实思考开关和预算直接进入请求与绑定。
W1 输出上限 32768、W3 输出上限 16384，超时 360 秒；Responses 带 instructions，
所有请求带 User-Agent。生产配置、提示词、契约与样本标注不变。

AF163 初轮六次冒烟：Luna 原生 schema 的 W1/W3 完成；Sonnet/Haiku W1 均为
schema_grammar_too_large，W3 均为 HTTP 400 provider_error_unclassified。
安全录制没有保留原始 HTTP 错误正文，**无法离线确认 W3 是否同为 schema 问题**，
不补写或猜测错误原因，不为诊断另发请求。用户 AF166 批准把 AF132 的评测专用
prompt_schema 例外扩展到 Sonnet/Haiku：schema 原样放入 system，取消约束格式字段，
返回完整 JSON 仍严格核验原 schema/decode/W2/W3；不修补、不放宽、不自动重试。
报告明确标“未使用约束解码”。例外仅固定三档 Opus/Sonnet/Haiku 评测档案，
任意 EVAL_PROVIDER_CONFIG 和生产入口仍要求 native schema。

旧 Sonnet/Haiku 冒烟录制、完整 campaign 和原合并报告原样移动到
`compare-af163/smoke/archive-af166/`，原计数与失败证据保留。Luna 两条录制和完整
账本不动。更换模式使旧请求指纹过期，不能挪用失败录制或改指纹；新两档各独立
冻结 2 槽（w1_01 + w3_01 各一次），只新增 **4 次**冒烟额度。

```sh
# 仅干跑新两档冒烟，不读密钥、不建产物、不发送
EVAL_COMPARE=af163-smoke EVAL_RECORD=4 EVAL_DRY_RUN=1 \
cargo test --offline --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
```

Claude 复核并授权发送后，去掉 EVAL_DRY_RUN=1；先选一档时用
EVAL_COMPARE_PROFILES=claude-sonnet-5-5（或 haiku ID）、EVAL_RECORD=2。
新入口只选 Sonnet/Haiku，不重开 Luna。结果仍落在 `compare-af163/smoke/`。
正式发送前核验所选前三档各自 W1/W3 冒烟的严格绑定、协议/schema/机械核验和 usage；
语义评分单列，不将合法输出的语义失分当作协议不可用。

#### Qwen3.8 诊断与门控

模型仅使用别名 `qwen3.8-flash`，无快照，报告标“别名，可能漂移”；context revision
只标识适配规则和上限依据，不代表固定模型权重。开思考传 enable_thinking=true +
thinking_budget=输出上限/2（W1 16384、W3 8192），**不传 reasoning_effort**；
关思考传 enable_thinking=false，不传 thinking_budget 或 reasoning_effort，避免参数冲突。
上限使用 max_completion_tokens（思考+回答），完整 wire 字节预检官方百万上下文、
983616 输入保守上限、131072 输出上限；超限拒绝，不截断。
依据：[模型上限](https://help.aliyun.com/zh/model-studio/qwen3-8-flash)、
[Chat 参数互斥规则](https://help.aliyun.com/zh/model-studio/qwen-api-via-openai-chat-completions)。
极小 schema 的探测不能证明真实 W1 schema 有效，必须先执行以下独立诊断。

沿用 AF162 单样本机制，但显式选择新两档，目录独立为
`compare-af163/diag-af166/`。固定真实 w1_01 的完整提示词、schema、输入和盲化 ID，
两档各 repeat=1、各 1 槽、共 **2 次**；不得改选 W3 或与旧 Qwen3.7 档混用。

```sh
EVAL_COMPARE=af162 EVAL_COMPARE_PROFILES=qwen38-flash-think,qwen38-flash-nothink \
EVAL_CASES=w1_01 EVAL_RECORD=2 EVAL_DRY_RUN=1 \
cargo test --offline --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
```

Claude 复核并授权诊断后才去掉 EVAL_DRY_RUN=1。诊断不消耗正式 160 槽。
报告 diagnostic_checks 逐档给出协议、原 schema/W2、usage、完全相同重复项及
诊断结论。诊断要求一个完整合法 JSON、无完全相同候选/请求重复；多对象、夹杂文字、
重复字段、截断或未知 usage 不通过。代码只核机械事实，不推断语义重复；一次合法回答
也不证明所有输入都执行约束解码。截断正文首/尾各最多 2048 UTF-8 字节、
reasoning_content 尾最多 1024（如有），只供诊断，不评分。
正式发送前只核验所选 Qwen 档对应的严格绑定诊断；缺录制、过期或不通过时，在读密钥、
冻结正式额度和发送前拒绝。普通模式输出不能冒充通过本门控。

AF164 已判定 Qwen3.7-flash 不可用：无约束解码，AF156/旧 AF162 剩余额度作废。
旧记录仍只读保留；Qwen3.7 不计入十二档主比较与分层，另列不可用说明。
前文旧 Qwen3.7 录制命令为历史说明，真实发送已拒绝。Qwen3.8 不是其录制的替代身份。

#### 正式比较与费用

```sh
# 原三档 240 槽
EVAL_COMPARE=af163 EVAL_COMPARE_PROFILES=gpt-6-luna,claude-sonnet-5-5,claude-haiku-5-5 \
EVAL_RECORD=240 EVAL_DRY_RUN=1 \
cargo test --offline --test write_stage_eval write_stage_eval_suite -- --exact --nocapture

# Qwen3.8 两档 160 槽
EVAL_COMPARE=af163 EVAL_COMPARE_PROFILES=qwen38-flash-think,qwen38-flash-nothink \
EVAL_RECORD=160 EVAL_DRY_RUN=1 \
cargo test --offline --test write_stage_eval write_stage_eval_suite -- --exact --nocapture

# 五档一起，默认完整额度 400
EVAL_COMPARE=af163 EVAL_RECORD=400 EVAL_DRY_RUN=1 \
cargo test --offline --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
```

冒烟/诊断通过且 Claude 授权正式发送后才去掉 EVAL_DRY_RUN=1。
正式目录 `compare-af163/`，五档各独立冻结完整 80 槽：W1 20 + W3 20、各重复两次。
各档串行，Qwen 两档共用 DashScope 锁，不与本地模型共享进程。
选一档用 EVAL_RECORD=80；EVAL_COMPARE_CASES 可选完整样本 ID 子集，仍保留完整 80
槽和累计计数，不能扩额。EVAL_RECORD=0 严格离线重评只输出 stdout；发送后报告落在
`reports/<所选档案>/report.json`。
沿用发送前 fsync、每调用落盘、在途不重发及持久化熔断：同档连续 3 次同类 HTTP
错误或 max_output_tokens 截断，剩余槽标“熔断未发送”，不占用未发送额度。

报告严格重建 AF155 七档 520 槽指纹与评分，十二档并列：历史七档标 reference_only，
五新档 400 槽。fixed_slots=920 是观测行，authorized_slots=400、historical_slots=520；
只有所选新档可发送。39 个计分样本、1 个观察题，W1/W3 通过率、verdict/coverage、
观察输出、分层、延迟、usage、失败字段与稳定性分列。缺录制不算失败归因或通过，
单次/不一致不作排他分层。历史本地层仅说明历史观测能力。

费用采用 AF163/AF165 用户批准的官方价情景估算，代理实际计费可能不同，美元换算 7.1。
每百万 token 输入/输出（元）：DS Pro 9/27（缓存 0.3），DS Flash 非高峰 1/4
（缓存 0.02），GPT-6.1-sol/Sonnet 14.2/71，Opus 28.4/142，Luna/Haiku
0.71/3.55（Haiku 输入≤100k），Qwen3.8 0.8/2.7（缓存 0.1）；本地 API 费为零，
不含硬件/电费。按录制实际 input/output usage 计算，reasoning 已含在 output 中，
不重复加算。已知 DS/Qwen 缓存按缓存价，未知按普通输入价；其他档缓存费率差异未计。
费用包含失败/观察调用，缺 usage 或在途无录制时显示已知小计和未知次数，总价及单位
成本为 null；未发送槽不计费用。cost_per_stable_passed_sample_cny 按非观察题两次均
通过的样本数计算，cost_per_passed_attempt_cny 另列单次通过成本，零通过不算单位成本。
正式比较费用不含独立冒烟/诊断及归档失败的历史费用。


### AF193：时间限定语诊断

`EVAL_AF193=1` 固定六个虚构场景（`af193/samples.json`，draft 标注）：观察时刻、历史/当前两断言、当前雇主、未来计划、先前说过的回指、明确日期与当前状态。每场景独立 W1/W3 各一次，共 12 槽；不运行 W4/R1/R4。W3 使用固定完整候选并先经 W2，不接该次 W1 输出；因此这是分阶段诊断。输入只带虚构原文、生产目录及不含语义的来源标识，不带期望或场景 ID。

角色与生产缺省对齐：`DEEPSEEK_PRO_MODEL`、`HUOSHAN_DEEPSEEK_BASE_URL`，W1/W3 low，输出上限 32768/16384，超时 360 秒。不覆盖用户生产配置。干跑不读密钥、不发送、不创建目录或账本；环境缺模型/地址时使用明确的干跑占位，真实录制拒绝占位：

```sh
PMR_MODEL_NETWORK=offline EVAL_AF193=1 EVAL_DRY_RUN=1 EVAL_RECORD=12 \
  cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
```

本轮未执行的录制命令（须另行授权，先加载真实端点环境）：

```sh
PMR_MODEL_NETWORK=live EVAL_AF193=1 EVAL_RECORD=12 \
  cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
```

产物仅写 `target/eval-time-af193/{recordings,campaign,report.json}`。首次发送前冻结全部 12 槽身份与额度，逐调用发送前落盘、响应后立即存录制；失败也占槽，不重试、不自动重发在途槽。续跑同命令、同模型与端点，只发送剩余槽，累计不超过 12。仅接受录制额度 12 或严格回放 0；禁止其他阶段、模型/强度/样本过滤覆盖。发送前需 `PMR_MODEL_NETWORK=live`，bootstrap/offline 不会发送。

```sh
# 严格回放，仍须保留录制时的模型与地址，密钥不需要。
PMR_MODEL_NETWORK=offline EVAL_AF193=1 EVAL_RECORD=0 \
  cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
# 只读统计历史录制的提示词影响，不修改任何历史产物。
PMR_MODEL_NETWORK=offline EVAL_AF193=impact \
  cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
```

默认只出报告，显式 `EVAL_GATE=1` 才因模型评分失败使测试失败。请求与 provider/模型/端点/上限/强度/超时绑定均严格比较；新提示词不能借用旧输出。影响统计分列精确包含 AF193 修改前提示词的录制（新提示词下必过期）、更早提示词或只有摘要无法单独判定的历史录制，以及固定历史对照；不把无法判定的录制当有效。W4/R4 只有实际输入发生变化时才过期。

`af193/w1-before.txt` 与 `w3-before.txt` 是修改前公开提示词的原字节快照，仅用于影响核查及固定历史对照。AF101 强度对照原本冻结 dc604f8 的 W3 提示词：现显式引用该快照，保留原字节指纹、请求与账本身份，不随当前提示词变动。普通阶段/比较入口仍用当前提示词，如实报告旧录制过期，不改旧指纹或标注。

### AF196：第二轮，仅 W1

AF193 的 12 次实测中 W3 6/6、W1 4/6；t01/t06 的持续状态被 W1 压缩为说话当天。AF196 仅补充 W1 的持续状态时间规则，不改 W3、schema、契约或标签。`EVAL_AF196=1` 复用相同六场景与盲化输入，仅 W1 各一次，Pro low、上限32768、超时360秒；W3/W4/R1/R4=0。

```sh
PMR_MODEL_NETWORK=offline EVAL_AF196=1 EVAL_DRY_RUN=1 EVAL_RECORD=6 \
  cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
```

独立产物为 `target/eval-time-af196/{recordings,campaign,report.json}`，独立标签 `af196-pro-low`。首次发送前冻结全部6槽身份与额度、每调用先落盘 started，再发送；不重试，失败或在途仍占额度，续跑跳过在途槽且不重发。仅接受 `EVAL_RECORD=6`（录制/续跑）或0（严格回放），其他覆盖与 AF193 相同，两个入口不可同时启用。干跑不读取密钥、不创建产物；真实录制仍要求已加载真实模型/地址和 `PMR_MODEL_NETWORK=live`。

### AF198：第三轮，真实输出串联诊断

`EVAL_AF198=1` 复用 AF193 六个虚构场景与原标签，每场景两次独立重复：该次真实 W1 输出经生产 W2 核验后构造 W3 输入。最大24次（W1 12、W3 12），Pro low，输出上限32768/16384、超时360秒。W1 失败、解码失败或 W2 无非拒收候选时，W3 不发送；不以固定理想候选补位。部分拒收时仅送审非拒收项，保留原始候选索引。

```sh
PMR_MODEL_NETWORK=offline EVAL_AF198=1 EVAL_DRY_RUN=1 EVAL_RECORD=24 \
  cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture

# Claude 复核后录制/续跑：预先加载 DEEPSEEK_PRO_MODEL/BASE_URL/API_KEY，不打印密钥
PMR_MODEL_NETWORK=live EVAL_AF198=1 EVAL_RECORD=24 \
  cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture

# 严格离线回放，不读取密钥；缺录制或过期如实拒绝，已发送在途保留缺口
PMR_MODEL_NETWORK=offline EVAL_AF198=1 EVAL_RECORD=0 \
  cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
```

独立目录 `target/eval-time-af198/`：`recordings/repeat-{1,2}/{w1,w3}/`、`campaign/` 和 `report.json`；不改 AF193/AF196 产物。发送前冻结完整24槽、模型/端点/提示词/schema/超时、W2上下文与 W3 输入构造规则；每槽最多一次。W3 请求未生成时只报告“待上游”，生成后绑定该次 W1 请求和输出摘要、完整 W2 与实际 W3 请求，再将 started 记录 fsync 后发送。全批互斥；失败/中断仍占本槽额度，不重试、不重发、不增额。W1或绑定变动拒绝续跑，不能为复用而绕过指纹。干跑无需密钥、不落盘；W3输入尚未产生时不伪报精确 token。入口拒绝其他评测入口与模型/强度/样本/重复/额度目录覆盖。

报告 W1 原标签评分和真实生产 W2 结果、W3 每候选 verdict/problems（非法输出仍保留原始判断，标为未经机械核验），不把 W3 核验通过算成语义通过。统计“限定语保留但因角色/时间拒收”：仅在 W3 机械核验通过时，对同主体/关系/value 的既有标签候选，全部标签时间引用仍在实际 W2 的 times 中，且 verdict 非 supported、problems 含 time，按候选及场景×重复计数；无法与标签对应的断言标未知、不计入。该计数依据模型声明，不由代码判断时间角色或语义。标签仍为 draft，两次重复只用于诊断；默认仅报告，显式 `EVAL_GATE=1` 才因 W1 失败或 W3 核验/录制缺口失败。

后续获准发送时将上例网络门禁改为 live、去掉 `EVAL_DRY_RUN`；严格回放用 offline、`EVAL_RECORD=0`，仍保留相同模型/地址。本轮不发送。AF193 的原始录制、12槽账本和原报告保持原样；按新 W1 提示词其6条 W1录制过期（含原两次评分失败），6条 W3不因本轮提示词改动过期，不改指纹或借用旧输出。


### AF206：通用 fact 片段 value 串联与定点验证

独立产物目录为 `target/eval-fact-af206/{chain,points}/`；不读取生产数据，不覆盖历史录制或标签。六个虚构场景在 `af206/scenes.json`（草拟标签，待产品复核），覆盖一否一肯、两条肯定、共享条件/不确定性、两个明确主体、同句纠正、修改一项且另一项保持。样本 ID 只进报告，模型可见来源标识为摘要；提示词中不含样本文字。

```sh
EVAL_AF206=chain EVAL_DRY_RUN=1 EVAL_RECORD=24 cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
EVAL_AF206=points EVAL_DRY_RUN=1 EVAL_RECORD=4 cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
```

chain 固定 24 槽（6×2×W1/W3），使用生产 DeepSeek Pro、W1/W3 low、32768/16384 输出容量。W3 的实际输入由**同次 W1 输出经生产 W2**生成；没有送审候选则跳过，不用理想候选补位。W1 评分允许含标注断言内容且不含兄弟断言的更长连续片段，不强求最短值；同句纠正与限定语语义仍按标签/模型核验，不能只以子串合法算支持。W3 保留逐候选 verdict，核验后用真实 SQLite 执行新来源准入，报告实际回执/记录身份；history 仅作指代上下文，不自动变成旧记录。AF206 当时否定普通 fact 即使 supported 仍不提交，不作为该轮修复失败；AF216 已批准开放 user_statement+fact 的否定投影，当前回放按新政策重算提交数，历史原报告不改。

points 固定 4 槽（W4 2 + R4 2）：预置两条共享完整原句的片段记录，修改一项、另一项保持。W4 medium/16384，R4 minimal/4096；实际 W4 输出经 prepare→validate→plan→W5 事务提交，R4 从该 SQLite 快照的生产 R3 证据包生成，不调用 R1。W4 评分检查目标和动作；R4 核验与实际证据保留供语义复核。

每个 campaign 固定总额度和完整槽位/派生规则，各依赖槽在发送前持久化精确请求及上游输出身份；物理发送前占额度，不重试，不重发中断在途槽。干跑不读取密钥、不创建账本或录制。授权后才可去掉 `EVAL_DRY_RUN=1` 并设置 `PMR_MODEL_NETWORK=live` 与 DEEPSEEK 环境变量；离线严格回放用 `EVAL_RECORD=0`，新目录缺口如实报告。不要将本轮授权额度转移给其他 campaign。

当前契约 W1 v5、W3 v6、W4 v5、R4 v7、recall v10（AF288 升宿主协议并修小库检索，W 侧不变）；R1 v2、DB v11 不变。当前请求与旧 W 阶段/R4 的契约及提示词身份不同，旧录制严格报告过期；保留历史录制/标签/报告，不改指纹以复用。AF96/AF101 等历史冻结提示词继续保留，不能当作当前提示词验证结果。


### AF216：普通否定 fact 的提交政策与 24 槽诊断

不改提示词/schema/版本/DB。仅 assertion+user_statement+fact+negate 投影为 Negative；肯定仍为 None，否定 episode、unknown、plan、retraction 保持原规则，旧 pending 不自动重整理。

四个虚构场景（`af216/scenes.json`）：一否一肯、专用关系否定、明确状态变化、明确纠错。W1 允许完整的合法通用/专用关系期望分支，事实片段可取更长连续引用，不能用其他分支掩盖机械失败。语义标签为草稿，未匹配的实际候选待人工复核。串联中 W3 使用同次 W1 经生产 W2 的结果，核验后用真实 SQLite 检查提交/重开；不注入理想 W1 输出补位。

```sh
# 零调用干跑；无需密钥，不创建 campaign/录制
PMR_MODEL_NETWORK=offline EVAL_AF216=chain EVAL_DRY_RUN=1 EVAL_RECORD=16 \
  cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
PMR_MODEL_NETWORK=offline EVAL_AF216=points EVAL_DRY_RUN=1 EVAL_RECORD=8 \
  cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture

# 获准录制后：保留同一 DEEPSEEK_PRO_MODEL/BASE_URL，另外提供 API_KEY，改 offline 为 live，去掉 EVAL_DRY_RUN
# 严格回放：同一配置、offline、EVAL_RECORD=0，不发请求
# AF206 历史录制重算：保留录制时模型/端点，报告另存，原录制/标签/账本/报告不改
PMR_MODEL_NETWORK=offline EVAL_AF206=recount EVAL_RECORD=0 \
  cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
```

独立目录 `target/eval-negative-af216/{chain,points}/`，chain 固定16槽（4×2×W1/W3），points 固定8槽（2×2×W4/R4），合计上限24。生产 Pro：W1/W3 low（32768/16384），W4 medium（16384），R4 minimal（4096），各360秒。输出容量合计475136 token（含推理，并非预期用量）；依赖输入生成前不伪报其精确 token。

points 独立隔离 W4/R4：注入旧已提交记录和核验候选；实际 W4 输出经 prepare→validate→plan→W5 提交后，从该 SQLite 快照装配 R3，注入经 decode→validate 的 R1 计划（R1=0）。报告核对 W4 动作/目标、R4 否定/当前状态及明确记录身份；正文语义仍交人工复核，不用代码判定否定语义。

完整槽位和派生规则先冻结；每个依赖槽绑定上游请求/输出、完整 W2 或实际 R3 请求。发送前落盘占额度，失败/中断仍占槽，不重试、不重发在途槽；请求变化拒绝续跑，不绕过指纹。chain/points 不能借用彼此额度。样本 ID/期望只进报告，模型可见 ID 盲化。AF206 的提交数重算不影响任何请求指纹，旧有报告仅作历史结果；本轮不发送真实调用、不读取生产数据。


### 火山 DeepSeek 环境变量迁移（AF243）

已有 `ds-pro`、`ds-flash` 与历史固定输入/M1.5/时间/fact/否定评测的火山请求，
现在仅从 `HUOSHAN_DEEPSEEK_BASE_URL` / `HUOSHAN_DEEPSEEK_API_KEY` 读取地址与认证；
`DEEPSEEK_PRO_MODEL` 仍为火山端点 ID。`DEEPSEEK_BASE_URL` / `DEEPSEEK_API_KEY`
留给官方接口，不能作为火山缺省回落。显式 `EVAL_BASE_URL` / `EVAL_API_KEY` 覆盖行为不变。

档案的 `endpoint_env` 是运行时路由元数据，不进入指纹或授权身份；历史
`provider.base_url_env` / `api_key_env` 字段继续保留原名，运行时仅对火山 `ep-*`
模型映射读取新的变量。实际 endpoint、模型、effort、schema、请求正文、上限和超时
仍完整参与严格绑定。无环境变量时可从旧录制恢复地址用于离线回放，不读取官方地址，
也不改写录制或冻结账本。无录制时沿用历史干跑占位，真实录制必须提供新变量。
本调整只作用于评测，不修改生产配置或生产环境变量解析。

## AF247 召回标签离线重评

R1/R4 的 AF247 修订与命令见 [召回阶段说明](../recall_stage/README.md)。输入/ID、r1.v2/r4.v7 请求、AF230 录制和冻结额度不变；`EVAL_RECALL_REPORT_DIR=reports-af247` 只选择新的离线报告目录。写入侧样本、评分与历史比较不变；自动字段结果和人工复核队列仍分别报告。
### W4 标签定稿（AF276）

[W4 样本](../w4/README.md) 已更新为 w4.v5，28 题中 25 题进入模型评分。
effective、relation_scope、目标集合与完整期望分支均评分；pending 不强制旧方依据。
报告保留 raw/final、automatic_fields、runtime_state_result、recall_semantic_result
与 manual_review；机械核验通过后复用 W4 离线夹具，在临时 SQLite 执行该次模型输出、
重开并核对 after；运行时失败不计通过。机械失败标“未执行”，未标 after 的专项标
“不适用”；标准夹具另由 `cargo test --test w4_offline` 验收。
自然语言 must_not 与召回呈现绑定输入/请求/输出待人工复核，机械失败不可豁免。

历史录制、额度和指纹均不改；标签单独修改不改变请求。
未设置 EVAL_RECORD 时只读报告缺录制/过期情况，不发送、也不写历史目录：

```sh
env -u EVAL_RECORD EVAL_STAGES=w4 EVAL_GATE=0 \
cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
```

### AF302 / AF303：当前 W1 v4 / W3 v4 四档重测

独立入口 `EVAL_AF302=1`，目录固定为 `target/eval-providers/compare-af302`。
DS-Pro 使用 trhj6、W1/W3 low；Luna、Sol、Haiku 使用 medium。Haiku 沿用
prompt_schema（未使用约束解码）。W1/W3 输出上限分别 32768/16384，超时 360 秒。
旧 providers.json、录制和授权不变。不接人工复核台账；自动分不是人工语义确认。
每档 20 W1 + 20 W3 × 两遍，共 320 槽；w3_19 每遍单列观察，不进主通过率。

干跑也会冻结 manifest.json 和各档 80 槽授权，但不读密钥、不发送；冻结内容包含
请求指纹、样本期望摘要、配置、费率与总费用上限。端点由真实环境变量绑定，改变后
明确拒绝沿用 manifest。密钥值不进入指纹。不要删除账本以恢复额度。

```sh
# 先加载环境：HUOSHAN_DEEPSEEK_BASE_URL / HUOSHAN_DEEPSEEK_API_KEY
# OPENAI_BASE_URL / OPENAI_API_KEY；ANTHROPIC_BASE_URL / ANTHROPIC_API_KEY
# 不设置旧 EVAL_COMPARE 等覆盖项。
EVAL_AF302=1 EVAL_AF302_REPEAT=1 EVAL_DRY_RUN=1 \
cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture

# 第一遍，仅 160 槽；EVAL_RECORD 是两遍累计授权，不是本轮调用数。
EVAL_AF302=1 EVAL_AF302_REPEAT=1 EVAL_RECORD=320 \
cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture

# AF313：第二遍仅 W1，先按下方步骤在本机重冻结标签。
EVAL_AF302=1 EVAL_AF302_REPEAT=2 EVAL_AF302_STAGE=w1 EVAL_RECORD=320 \
cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture

# 同一命令可续跑；EVAL_RECORD=0 仅回放所选遍。
```

费用用整数 nano-CNY 记账，四档/两遍共享一把锁和 20 元预算。发送前持久化预留：
输入按完整 wire JSON UTF-8 字节数 + 1024 framing allowance 估计 token 上界，
不预先扣缓存优惠；输出按 max_output_tokens。已结算 + 在途预留 + 本次上界大于
20 元即标“费用熔断未发送”，不占调用槽。完成按 usage 结算；无 usage、中断保留
预留上界，不能自动退款或重发。记录成功但结算前中断也保守保留上界。
费用上限是冻结费率下的估算约束，输入估计不是 provider tokenizer 证明，代理实际
价格可能不同；若返回 usage 超过预留则如实计入并阻止后续超额发送，不能撤销已计费请求。
已有同类 HTTP/截断三次熔断继续生效，所有调用均无自动重试。

冻结价格（元/百万 token，输入/输出/缓存输入）：DS 9/27/0.3，Luna 与 Haiku
0.71/3.55/0.71，Sol 14.2/71/14.2。非 DS 未假定缓存优惠。干跑同时给出全输出上限
的最坏费用，以及“输入字节估计/4、每次输出 3000 token、无缓存”的情景预估，
后者不是实测预测；20 元可能提前终止，320 是授权上限而不是保证完成数。
`plan-repeat-N.json` 保存计划与指纹；`report-repeat-N.json` 分档分阶段列自动分、
观察题、usage、P50/P90、usage 已知费用及全局保守费用账本。

### AF311 标签校准（零调用）

AF311 仅修订 W1/W3 期望、说明与标签版本，输入/ID 不变，模型请求仍使用当前 W1/W3 v4。
通用 fact 接受经机械核验的连续断言片段，sentence 仍保留完整原句；不放宽主体、限定语或引用校验。
W1_11 新增“零候选 + source retention”的完整分支，空 indices 不代表全部旧事实。
W1_05 的条件“以后”可不另列时间；若列，须为 event/unknown 并保留引用。
W1_06 保持 fact；W3_07 只接受 contradicted[speech]；W3_01/14 为 contradicted[time]，
W3_05 为 insufficient[subject]。分支须整支通过，W3_19 仍仅观察。

检查：`python3 tests/eval/w1/check_samples.py w1 w3`；生成器与样本应保持一致。
AF302 repeat-1 离线重评另存 `target/eval-providers/compare-af302/reports-af311/`，
读取匹配原请求指纹的最后一次录制，保留机械核验，不发送请求。
标签不进入请求指纹；AF302 manifest 另冻结标签摘要，因此旧录制入口会拒绝新标签下续录。
本轮不改 manifest/额度/账本，离线重评不代表重新授权续录。

### AF313：AF311 标签重冻结与第二遍仅 W1

必须在 Claude 本机、使用原 AF302 端点环境执行，不在沙箱生成/替换 manifest。
重冻结仅允许各槽位 labels 摘要变化，并要求变化样本的 label_revision=AF311；
请求、槽位顺序、档位配置、费率、输入费用上界、总预算任一不同均拒绝。
保存原字节备份 `manifest.before-af311.json` 和 `label-amendment-af311.json` 审计。
原 manifest 只有标签摘要，没有旧 label_revision 明文，审计如实标未知并保留旧摘要。
成功后重复重冻结为无变化，不覆盖备份。重冻结要求干跑且禁止同时发送。

本机依次执行（zsh 加载 ~/.zshrc；不打印密钥）：
环境变量仍为 HUOSHAN_DEEPSEEK_BASE_URL/HUOSHAN_DEEPSEEK_API_KEY、
OPENAI_BASE_URL/OPENAI_API_KEY、ANTHROPIC_BASE_URL/ANTHROPIC_API_KEY。
勿覆盖模型/端点/档位；旧端点变量不同会明确拒绝，不自动迁移指纹。

```sh
# 1. 仅标签重冻结：校验全部320槽，备份并更新manifest，不发送。
zsh -ic 'unset EVAL_RECORD; EVAL_AF302=1 EVAL_AF302_REFREEZE=AF311 EVAL_AF302_REPEAT=1 EVAL_DRY_RUN=1 cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture'

# 2. 第二遍 W1 干跑：应为四档各20，共80选中槽。
zsh -ic 'unset EVAL_RECORD EVAL_AF302_REFREEZE; EVAL_AF302=1 EVAL_AF302_REPEAT=2 EVAL_AF302_STAGE=w1 EVAL_DRY_RUN=1 cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture'

# 3. 发送/续跑所选80槽，共用原20元预算；320是既有累计槽位身份。
zsh -ic 'unset EVAL_DRY_RUN EVAL_AF302_REFREEZE; EVAL_AF302=1 EVAL_AF302_REPEAT=2 EVAL_AF302_STAGE=w1 EVAL_RECORD=320 cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture'
```

第二遍未指定 w1 或指定 w3 均拒绝。W3 不发送、不预留费用、不占本次槽；
原320槽manifest及每档80槽授权身份保持不变。已有结算/在途预留继续纳入20元上限，
传输失败重试沿用 AF308。干跑金额只估算本轮选择，历史实际费用另列 charged_cny。

输出 `plan-repeat-2-w1.json`、`report-repeat-2-w1.json`；
另生成 `report-w1-combined-af311.json`，严格重读两遍 W1 录制并按当前 AF311 标签评分，
每档40槽，缺录制/传输失败如实列出，绝不为汇总补发第一遍缺口。
保留第一遍旧报告与 reports-af311。合并报告自动分不是人工确认。

### AF317：结构性 W1 标签离线重评

W1 部分标签升级 AF317：允许断言窗口＋必含谓词/属性核心，见 W1 README。
只在 fact value、显式 times.quote/deadline.quote 中启用连续包含规则，不用于专用关系值；
其他机械核验和完整分支评分保持。W3 标签不变，生产 DS-Pro 配置不变。

AF302 两遍 W1（每档40槽）及第一遍 W3（每档20槽，其中w3_19观察）的离线报告：
`target/eval-providers/compare-af302/reports-af317/`。
请求指纹不变，使用原录制严格重评；缺录制与传输失败不计通过，不补发。
标签摘要变化仍会触发原 manifest 的续录保护；本次不重冻结、不改账本，不代表新调用授权。

### AF319：W4 三档比较（独立 150 槽、15 元上限）

28 个 AF276 reviewed 样本中，排除 `call_w4=false` 的 06、08 不完整范围、08 完整范围；
其余 25 个（含 14 完整范围对照及 n1a/n1b/n2）× 三档 × 两遍 = **150 槽**，每遍75。
不调用 W1/W3/R4，不改历史样本/录制。W4 使用真实 payload，没有题型/样本 ID 标签。

三档为 ds-pro（当前生产 W4：火山 trhj6）、gpt-6.1-sol、gpt-6-luna；均 medium、
max_output_tokens=16384、超时360秒。独立目录 `target/eval-providers/compare-af319/`。
复用 AF302/AF308/AF313 的执行、严格指纹、发送前持久化、续跑与错误熔断；
费用核算/标签重冻结抽到 `cost_campaign.rs`，旧 AF302 manifest 身份不变。

冻结沿用 AF302 的费率（元/百万 token；输入/缓存输入/输出）：

| 档位 | 输入 | 缓存输入 | 输出 |
| --- | ---: | ---: | ---: |
| ds-pro | 9 | 0.3 | 27 |
| gpt-6.1-sol | 14.2 | 14.2 | 71 |
| gpt-6-luna | 0.71 | 0.71 | 3.55 |

这是冻结估算费率，不保证代理实际计费。发送前按请求 UTF-8 字节数+1024 的输入上界、
最大输出且无缓存折扣预留；usage 返回后结算。无 HTTP 状态/usage 的发送阶段传输失败
结算0，可同槽重试并追加尝试；HTTP 错误缺 usage、进程中断的预留仍按上界计。
累计结算+在途预留不得超15元，未发送槽不占调用额度。每次执行不自动重试；再次运行
只重试经账本与录制双重确认的传输失败。语义失败不重发。每档连续3次同类错误/截断熔断。

干跑输出每档指纹、pending/rejected、费用上界与假设每次3000输出token的情景估算，
该估算不是承诺。合成端点零调用测试两遍情景合计约19.63元，超过15元；实际较短输出
或缓存命中会降低费用。建议先执行第1遍、审核费用与结果，再决定是否执行第2遍。

**只在 Claude 本机生成 manifest**，避免沙箱端点进入冻结指纹。环境变量：
`HUOSHAN_DEEPSEEK_BASE_URL`、`HUOSHAN_DEEPSEEK_API_KEY`、`OPENAI_BASE_URL`、`OPENAI_API_KEY`。
不覆盖模型或档位，也不打印密钥。以下命令均在仓库根目录执行：

```sh
# 干跑并冻结全部150槽；所选第1遍75槽，实际发送0。
zsh -ic 'unset EVAL_RECORD EVAL_AF302 EVAL_AF319_REFREEZE; EVAL_AF319=1 EVAL_AF319_REPEAT=1 EVAL_AF319_STAGE=w4 EVAL_DRY_RUN=1 cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture'

# 第1遍，发送/续跑最多75个唯一槽；150是两遍累计授权，不是每次发送量。
zsh -ic 'unset EVAL_DRY_RUN EVAL_AF302 EVAL_AF319_REFREEZE; EVAL_AF319=1 EVAL_AF319_REPEAT=1 EVAL_AF319_STAGE=w4 EVAL_RECORD=150 cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture'

# 第2遍，共用同一15元账本，最多75个唯一槽。
zsh -ic 'unset EVAL_DRY_RUN EVAL_AF302 EVAL_AF319_REFREEZE; EVAL_AF319=1 EVAL_AF319_REPEAT=2 EVAL_AF319_STAGE=w4 EVAL_RECORD=150 cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture'
```

不支持其他阶段/遍数或覆盖档位。未来获批标签调整，可使用
`EVAL_AF319_REFREEZE=<获批且写入样本的label_revision>` 配合 `EVAL_DRY_RUN=1`，并 unset
`EVAL_RECORD`：只接受标签摘要变化，其他请求/槽位/配置/费率/预算逐项相等；备份原 manifest，
另写 amendment 审计。没有新授权不得借此修改标签。

输出 `plan-repeat-N-w4.json`、`report-repeat-N-w4.json`、`report-combined-w4.json`。
报告保留 raw/final、模型字段分、SQLite 运行时状态分、人工语义待复核、usage/估算费用和
延迟P50/P90；`after.recall`/自然语言 `must_not` 不自动视为通过。合并报告严格离线读取
两遍录制；未录制/中断/熔断槽如实保留，不为汇总补发。

### AF322：W4 标签重冻结与第二遍两档

7题采用 AF322 完整期望分支；首遍三档严格离线重评保存到 `compare-af319/reports-af322/`。
`EVAL_AF319_PROFILES` 只筛选执行槽，不改变三档150槽manifest、费率或15元账本。
第二遍默认 ds-pro,gpt-6.1-sol，共50槽；显式选择Luna第二遍拒绝，Luna原槽保持未发。
第一遍仍默认三档。未知档、重复档、空列表拒绝。AF319原第二遍75槽说明由本节替代。

Claude 本机加载 HUOSHAN_DEEPSEEK_BASE_URL/API_KEY、OPENAI_BASE_URL/API_KEY 后依次执行：

```sh
# 只允许标签摘要变化；备份旧manifest，不发送
zsh -ic 'unset EVAL_RECORD EVAL_AF302 EVAL_AF319_PROFILES; EVAL_AF319=1 EVAL_AF319_REFREEZE=AF322 EVAL_AF319_REPEAT=1 EVAL_DRY_RUN=1 cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture'
# 第二遍干跑：总冻结150，选中50，Luna选中0
zsh -ic 'unset EVAL_RECORD EVAL_AF302 EVAL_AF319_REFREEZE; EVAL_AF319=1 EVAL_AF319_PROFILES=ds-pro,gpt-6.1-sol EVAL_AF319_REPEAT=2 EVAL_AF319_STAGE=w4 EVAL_DRY_RUN=1 cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture'
# 第二遍发送/续跑，原15元累计账本不重置
zsh -ic 'unset EVAL_DRY_RUN EVAL_AF302 EVAL_AF319_REFREEZE; EVAL_AF319=1 EVAL_AF319_PROFILES=ds-pro,gpt-6.1-sol EVAL_AF319_REPEAT=2 EVAL_AF319_STAGE=w4 EVAL_RECORD=150 cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture'
```

零调用首遍重评命令（从旧录制恢复端点，全量核对原manifest非标签字段；只在临时目录
演练重冻结，不改真实manifest、录制或费用账本）：

```sh
EVAL_AF322_REPLAY=1 cargo test --test write_stage_eval af319::af322_offline_first_repeat -- --exact --nocapture
```

### AF331：W3 v5 本地覆盖核验

当前 W3 v5 拒收 gaps 且 missing 为空；合法连续引用的 gaps 与 complete+空 missing 保持通过。无动态 reviews 长度 schema，无自动重试。旧 W3 v4 请求/检查点因版本与提示词身份变化严格失效，历史录制保留且报告过期；W1/W4/recall/DB 不变。已提交数据不重写，已完成来源不自动重整理。


### AF342：个人陈述提取范围（W1 v5 / W3 v6）

独立草案 `af342/w1.json` / `af342/w3.json` 各 12 题，由 Claude 复核；不得将草案自动分称为人工验收。W3 使用固定候选，包含零候选的 coverage 诊断；这不改变生产零候选跳过 W3 的规则，也不声称是串联闭环。
范围外讨论不提取，coverage 只计范围内遗漏；候选原文支持核验不增加范围准入字段。旧 W1/W3 录制严格过期、原文件不改，已存检查点按版本失效，不自动重整理。

沿用 `cost_campaign`：24 个独立槽，一遍，DS-Pro W1/W3 low，输出上限 32768/16384，超时 360s。模型必须显式来自 `DEEPSEEK_PRO_MODEL`，端点 `HUOSHAN_DEEPSEEK_BASE_URL`，密钥 `HUOSHAN_DEEPSEEK_API_KEY`；无端点/模型拒绝生成占位 manifest。费率输入/缓存/输出为 9/0.3/27 元/M（情景估算，非账单）。总预算 10 元；发送前按 JSON UTF-8 字节+1024 的输入上界与最大输出预留，usage 结算，发送阶段无响应传输失败结算 0 且可手工续跑；HTTP 无 usage/中断按上界，不重发已响应或在途槽。沿用连续同类错误/截断熔断。最大费用之和可能超过 10 元，因此费用门槛可能提前停止，不扩额。

仅在 Claude 本机、确认草案后首次干跑生成 manifest；Codex 仅用临时注入环境检查计划，不能生成实际 manifest。标签或请求变化拒绝续跑（本轮不新增重冻结授权）。无人工复核台账，报告保留 draft、字段/coverage、usage、费用及延迟。

```sh
# 在仓库根目录；zsh -ic 读取现有环境，不打印密钥。
zsh -ic 'EVAL_AF342=1 EVAL_DRY_RUN=1 cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture'
# 核对 fixed_slots=24、selected_slots=24、cap_cny=10、端点/模型指纹后发送：
zsh -ic 'EVAL_AF342=1 EVAL_RECORD=24 cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture'
```

产物仅在 `target/eval-providers/af342/`（manifest、campaign 授权、cost-ledger、逐次录制、plan-repeat-1.json、report-repeat-1.json）。同一发送命令可续跑；不设置其他 EVAL campaign/model/effort 变量。
