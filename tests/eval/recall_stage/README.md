# R1 / R4 单阶段评测

复用 `tests/write_stage_eval.rs` 的录制、预检、请求绑定及离线回放。样本为已复核的 `tests/eval/r1/samples` 与 `tests/eval/r4/samples`；当前R1 20项（dev14/holdout6）、R4 24项（dev14/holdout10），见AF227、AF247、AF272修订。默认 W 阶段选择不变。

零调用的干跑与回放：

```sh
EVAL_STAGES=r1,r4 cargo test --offline --test write_stage_eval dry_run_plan -- --exact --nocapture
EVAL_STAGES=r1,r4 cargo test --offline --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
```

EVAL_CASES、EVAL_REPEAT、EVAL_MODEL/EVAL_MODEL_LABEL、EVAL_BASE_URL 与 EVAL_MAX_OUTPUT_TOKENS 沿用写入侧；R 阶段评测缺省输出16384，实际上限参与录制绑定。生产缺省仍取显式预算中的 query/review 输出限额4096，评测不自动更改生产授权。未设置 EVAL_RECORD 时不创建客户端、不读取凭证、不发送请求。单次全量潜在44次，两次88次，三次132次；真实录制必须另行取得批准，再显式设置 EVAL_RECORD 的整数上限。整个所选计划超过上限时零发送。

`EVAL_REASONING_EFFORT` 覆盖此次全部 W/R 阶段，缺省 medium。接受 low/medium/high/minimal/none/xhigh/max；omit 表示省略 reasoning（供应商默认，非本地 medium 默认）。供应商是否接受这些值需另行真实验证。实际请求和指纹包含强度，变化不能复用录制。medium 保持现有 `recordings/<模型标签>/repeat-N/` 路径；其他强度为 `recordings/<模型标签>/reasoning-<强度>/repeat-N/`，不覆盖 medium。干跑同样使用模型、强度和重复次数；两个模型分别运行以下命令，各潜在88次（R1 40、R4 48；R1 dev28/holdout12，R4 dev28/holdout20），本轮零发送：

```sh
EVAL_REASONING_EFFORT=low EVAL_STAGES=r1,r4 EVAL_REPEAT=2 cargo test --offline --test write_stage_eval dry_run_plan -- --exact --nocapture
```

录制的 `elapsed_ms` 从调用发送器前计到完整响应读取/失败返回，包含 HTTP、超时及未完成响应；不包含评分或落盘时间。每条报告保留 elapsed_ms/usage，按阶段 metrics 给出毫秒 P50/P90/最大值（nearest-rank）及 output_tokens、reasoning_tokens 均值，每项附有效计数。失败调用有 usage 时纳入 token 均值，无 usage 不填零。既有录制没有计时字段，elapsed_ms 为 null、不参与百分位；不以文件写入间隔冒充实际延迟，不改变既有录制。回放复用原测量数据，零发送。

HTTP/未完成/无效响应均保留原始尝试、usage 与 incomplete_details.reason；不自动重试。回放完整绑定角色路由、实际请求、prompt/schema、时钟、时区、证据包、输出上限、地址和重复编号；没有匹配的录制报未录制/过期，不冒充模型失败。R 请求使用 SHA-256 绑定完整数据，旧 W 指纹不变。默认只出模型评分报告；EVAL_GATE=1 才因模型评分或调用失败报错，无效输入/损坏录制/非法配置始终失败。dev/holdout 分开汇总，必须同时报告。

`adapter.rs` 只读取 input：R1 目录转换为运行时 Entity/Relation，沿用生产纯 payload。R4 展开冻结 RecallEvidence 与 R1 计划；记录用通用 fact 的完整原句，不凭标签猜专用 value/限定语；raw 保留来源日及未整理状态，关系依据来自给定成员。输入未给 sequence，固定按记录后追加 raw 分配稳定顺序；未给的 MemoryRequest/history 不猜造。legacy 保留旧 Memory 形状及 gaps，不给它补 Candidate/来源。完整性中的待判定/缺口由 input 派生，截断标记原样保留。与生产共用 r4_payload，以及 r1/r4 的核验和计算代码；单阶段不运行模型 R1、检索模型或后台写入。

评分规则：

- R1 严格解码并运行 R1 核验；比较 need、current_evidence、type、subject、conditions、time、plan.op、filter.relation，以及标签明确给出的 filter.time 和 unit。time/filter.time 归一化 Quote 与 anchor_quote，省略固定 anchor；标签中的 `plan.filter.time` 按声明字段评分，null 明确要求为空。标量的标签数组是允许集合，结构数组无序一一匹配，嵌套的条件候选数组表示该片段的允许集合。Z8：conditions 要求期望为实际的子集，额外检索提示不扣分，必需条件缺失仍失败；time 标签为 null 时不做语义评分，标签非 null 时继续比较明确的 Quote/interp 字段（如 `[null, recurring对象]` 明确只接受这两种）。模型额外给出的条件/时间仍须通过全部结构、原文引用及时间换算核验，不能靠无效额外内容通过。
- R4 先严格解码、核对全部引用/限定语/记录 ID，并由代码执行计算；比较 verdict。标签 facts 每行要求唯一匹配：record_ids 集合相同，来源包含标签要求的来源（允许更新双方依据），必须属性逐项比较。标签未指定的附加事实与 statement 含义不做自动语义裁定，原始输出留人工检查；facts=[] 表示没有要求匹配的事实行，不额外创造“模型必须不返回任何事实”的规则。
- Z18：R4 raw 直接严格核验；final 先通过生产 `r4::assemble_old_basis` 补已选包内目标的真实旧引用，再按同一规则核验、计算和评分。报告保留 raw_output_text/raw_output、final_output、raw_result/final_result；raw_summary/raw_splits 为模型原始分数，summary/splits/per_sample/status 为 final 分数。补入不改标签、请求指纹或原录制；未通过其余核验仍失败，不能把结构补入后的通过解释为替代语义已核准。
- AF264：同一入口先做有限、唯一的引号引用恢复，再补旧方依据；原始输出评分不使用恢复。R4 提示词补强使旧请求录制严格过期，历史原请求重核只作为历史诊断，不冒充新提示词效果。

AF264 独立诊断使用四个虚构固定输入（引号、问题专属条件、来源真实条件、更新依据），各两次，只调用 R4。模型可见输入不含样本标签或期望；生产对齐 Pro/minimal、4096 输出 token、180 秒。正式额度固定 8，沿用发送前落盘、跨续跑额度、在途缺录制不重发、同类错误/截断熔断。所有自动通过仍需人工核对额外事实和语义。

```sh
EVAL_AF264=trial EVAL_DRY_RUN=1 EVAL_RECORD=8 cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
# 仅由获准执行方录制；先加载 HUOSHAN_DEEPSEEK_BASE_URL/API_KEY 和 DEEPSEEK_PRO_MODEL。
EVAL_AF264=trial PMR_MODEL_NETWORK=live EVAL_RECORD=8 cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
# 严格复用 AF255 A 的原请求/输出；禁止 EVAL_RECORD，不改原录制或人工台账。
EVAL_AF264=replay cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
```

新产物只写 `target/eval-r4-af264/`：试验 `report.json` 与独立 campaign/recordings；AF255 原请求指纹自洽核对及 raw/final 机械重核写 `replay-af255.json`。历史输入不使用新提示词构造指纹；该报告明确标记当前提示词过期，不计 common.v1 或人工确认通过。
- R4 核验失败的 `fields[].field=decode/validate` 对应 `actual={code,rule,field,cause}`：code 保持 `model_invalid_output`，rule 给出生产核验规则，field 定位条目/属性，cause 可选地记录内层时间失败原因。解码错误定位根 `$`；每次只报告首个错误。生产回执不增加字段，诊断不含原文。核验实现与生产共用，不改变录制请求或指纹。
- compute=null 要求实际为空；其余按标签核对 op、items 个数、operands 个数/端点与条目绑定、数量集合/单位、distinct_groups、coverage。expected_value 由生产代码运算核对，模型不得直接输出最终数值；insufficient_ok 允许空 compute 或有效但无法计算的粗精度计划，不能接受无效引用/日期。未规定的项目不猜标签。
- must_not 全部留人工，报告明确附带；不能把字段评分“通过”解释为完成了语义人工验收。尤其额外 facts、唯一当前值、statement 限定语、纠错与历史仍需复核。这是现有样本的评分覆盖边界，新增更严要求应先补经复核的标签。

自动回归注入全44项合法输出并逐字段制造失败，检查适配、评分、上限预检、录制重放、split及请求时钟指纹。提示词扫描在 `src/recall_models_tests.rs`，同时扫描 R1/R4/W1/W3/W4 样本的原文、上下文、样本ID与文字标签。

## AF224：完整 R4 输入与召回比较干跑（历史段落；当前清单见AF227）

本段不修改上述历史简写、样本标签、写入档案、录制或账本。旧简写保留原请求；新增 `input.format="recall.full@1"` 后输入必须显式提供：

| 字段 | 形状/含义 |
|---|---|
| `owner_id` | 虚构语料的可信 owner，必须与目录及 legacy 数据一致 |
| `request` | 生产 `RecallRequest`：request_id、query、context、task_id（必须出现，可 null）、at（UTC 秒） |
| `plan` | 完整 r1.v2 输出，经生产 decode→validate，不从期望构造 |
| `entities` | 生产 Entity 数组，包含 owner、规范名、别名；不自动补 self |
| `relations` | 目录 ID 数组；本阶段沿用四种记忆类型的目录限制 |
| `subject_ids` | 必须出现，可 null；`[]` 是可信空范围 |
| `utc_offset_seconds` | 可信时区偏移，必须出现 |
| `evidence` | 完整生产 RecallEvidence，包括 current_message、records、raw、relations、completeness |
| `sources` | `{source_id:{text:完整获准正文,said_on:原话日期或null}}`，日期字段必须出现 |
| `source_context` | 可选，沿用生产 source_context；只准引用 sources 中的来源 |

CurrentRecordView 的 Candidate、nullable said_on、time_resolutions、history、update 等原样传入；value 可为连续断言片段，sentence 保留完整原句，speech=plan 和 polarity=negate 原样保留。代码核正文引用、日期一致、唯一记录/目录、W2 候选及时间解析、可信主体与缺口计数，不从字段推断计划已实现、纠错即历史或对象同一。若 W2 会修改候选，必须先输入 W2 最终结果，不把旧抽取结果伪装成提交记录。

请求复用生产 r4_payload/r4_schema_for_plan，source_context 按生产规则附加。完整输入沿用本页语义期望评分及 raw→Z18→final 流程，不走 fixed_inputs 的“仅契约核验”捷径；额外 facts、statement 和 must_not 仍待人工。改标签不改指纹；改正文、候选、日期、范围或 source_context 会改变指纹。不要为复用历史录制修指纹。

`providers.json` 是独立的召回比较配置：DS Pro/Flash 为 R1 minimal、R4 lookup/list minimal、count/sum/elapsed/compare low；GPT Sol/Luna 与 Claude Haiku/Sonnet 为 low 替代配置（不称为 minimal 等价）；Qwen3.8 为 omit+thinking=false、别名可能漂移。R1/普通 R4 上限4096、180秒；计算档上限16384、240秒。AF230仅将ds-flash普通阶段上限提高到16384，标为评测专用，与生产不同。Qwen 计算档的容量是固定比较设置，未声称其 omit 会触发生产的 compute-low 分支。Anthropic 仅 native_schema，失败后不得自动切 prompt_schema，须另批独立冒烟。

本轮入口**仅干跑**，`EVAL_RECORD>0`（包括非整数配置）直接拒绝，不创建 HTTP 客户端，也不读取密钥。不需网络、密钥，不落盘授权、录制或报告：

```sh
EVAL_RECALL_COMPARE=af224 EVAL_DRY_RUN=1 cargo test --offline --test write_stage_eval dry_run_plan -- --exact --nocapture
EVAL_RECALL_COMPARE=af224-smoke EVAL_DRY_RUN=1 cargo test --offline --test write_stage_eval dry_run_plan -- --exact --nocapture
EVAL_RECALL_COMPARE=af224 EVAL_RECALL_PROFILES=ds-pro,ds-flash EVAL_DRY_RUN=1 cargo test --offline --test write_stage_eval dry_run_plan -- --exact --nocapture
```

当前20+20样本 ×2 ×7档为560槽；冒烟用独立输入/目录，每档 R1、R4 lookup（query_only）、R4 count（允许 current，计算档）各1槽，共21槽，合计581。冒烟 count 的 current_evidence=unknown 仅在内存中用于验证另一 schema，不改变原样本/正式请求。A 段新增/修订输入后重新冻结完整清单并重新报批，不沿用560这个数。

未来录制的目录为 `target/eval-providers/recall-af224/{recordings,campaigns}/<profile>`，冒烟位于独立 `smoke/`。每档额度固定完整清单，筛选不扩额。干跑复用现有严格请求/provider/端点/超时指纹、发送日志、中断在途识别与 HTTP同类错误/截断连续3次熔断状态检查；本轮不启用录制。计费复用 AF163/AF165 批准价格与 usage 口径，不能按粗估冒充实付费用。

输入粗估按完整 wire UTF-8 字节逐请求/3向上取整；输出给的是授权容量上界（含推理），不是预计消耗。两个阶段独立样本的延迟分位不能相加当端到端延迟。人工语义台账、分阶段对比报告及真实录制接线留 B 段；当前标签待复核，不以干跑宣称质量通过。复核清单见 `.ai/recall-label-review-af224.md`。

## AF227：标注修订、计算评分与独立人工复核（历史额度；当前执行见AF230）

当前 R1 20、R4 24，正式七档 × 两次为 **616 槽**（各档 R1 40 / R4 48），独立冒烟21槽，总计637。沿用上面的 `EVAL_RECALL_COMPARE=af224` / `af224-smoke` 干跑命令；入口仍只干跑，不读取密钥或发送模型请求，616不是已经获准发送的额度。输入粗估按完整 wire UTF-8 字节/3向上取整，输出统计容量上界，不能当作预计消费。

AF227 修订记录在各样本的 `label_revision`；未改样本保留历史 contract 标签，请求仍固定 r1.v2/r4.v7。r1_12 的两支均保留 assertion_present/self；r4_07 只收有依据的当前住址，不推断搬家、冲突或 supersedes。r4_04 必需新住址、r4_10 必需旧历史住址，其他合法附加事实可以返回但仍须人工核验。r4_17 supported 支须保留粗时间事实且计算无结果；insufficient 支同样禁止补日。r4_19 必须保留两个已确认不同国家的下限2，不能借范围截断抹掉身份或声称 exact。

`expect.alternatives` 对 R1/R4 是**完整期望分支**：各支独立核验 verdict/必需 facts/compute/机械禁止项；`must_not` 是该支绑定的人工语义要求。不能字段串配，不因备选跳过解码、来源、限定语、身份、日期核验。`facts=[]` 仍表示没有必需事实行，不等于禁止额外事实。`forbid_facts` 对指定 record_ids 的字段组合断言不得出现，`forbid_supersedes` 禁止本轮替代 ID；这些只比已标注字段，不用关键词猜 statement 含义。

计算评分复用生产计算：`expected_value`、`expected_bound`、`exact_days`、`rounding`、`approximate`；`operand_sources` 按操作数顺序校验每个日期条目的 date_source，`groups` 校验来源的同组/异组关系，组名任意。r4_14 可将重复提及合并为带双方依据的 item；两组/结果2不变。r4_15 校验120/80各自计量对象、元、200及exact；r4_16使用完整输入明确冻结 R1 plan.unit=day，校验事件日到请求时刻94天、day/calendar_days/非近似；r4_18校验日语/韩语两端顺序。以上都不是模型最终数值输出。时间字段标签 `time_quotes`/`time_kinds` 核对实际字段；statement 的时间含义仍人工检查。

新增完整输入 n1–n4 对应 r4_21–24：计划取消/预订未退/执行未知；同句肯定否定片段；R1 ambiguous 下两主体开放列举；补录 said_on=null 下不补年份。完整输入明确候选、来源、原陈述时间、记录身份与版本链，不从期望反填请求。真实主体歧义/可信范围空集由注入回归核验，不能代替模型语义实测。

独立人工台账：`tests/eval/recall_stage/review_ledger.json`（初始为空，`version=recall.review@1`）。不与端到端历史复核台账混用。离线单阶段报告的 `review_summary` 按R1/R4分列 `automatic_field_passed`、`human_confirmed`、`human_rejected`、`pending`、`automatic_failed`、`not_eligible`，`review_queue` 列出去重后的待复核绑定。现有 `status/summary` 明确仍是**自动字段结果**，不能作为完整语义通过率；完整语义通过数仅 `human_confirmed`。R4 保留 raw→Z18→final，R1 保留 raw 与机械核验还原后的 final。

台账条目形状：`{"binding":<报告manual_review.binding>,"decision":"accepted|rejected","reviewer":"人工复核人","reason":"具体依据"}`。绑定包含样本、阶段、完整标签版本摘要、请求指纹、repeat、模型可见输入摘要、最终输出摘要与 dedup_key。按同一样本/标签/输入/输出去重，可跨重复/档案共用语义确认，原请求与本次绑定都留在报告；输入/标签/输出变化不能借旧确认。冲突决定或不完整台账拒绝加载。`EVAL_RECALL_REVIEW_LEDGER=/另存台账.json` 可选择新的台账，无需改录制。

人工逐项检查**所有自动通过输出**的额外事实、statement、限定语、人物归属、事件归组/操作数，以及所选分支 must_not；字段失败可复核是否遗漏合法分支，但不能直接翻为通过，须修经批准标签后重算；机械失败不能人工豁免。未确认自动通过项计待复核，不隐式通过。台账不进入请求、录制指纹、额度身份。干跑没有输出可复核，不自动生成 accepted 条目。

零调用离线重评可用：

```sh
env -u EVAL_RECORD PMR_MODEL_NETWORK=offline EVAL_STAGES=r1,r4 \
  cargo test --offline --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
```

只改期望不改变请求指纹；r4_02完整 uncertainty 输入改变指纹，旧录制如实过期，其他历史正文/录制/账本不改写。新增语料不进入生产提示词。

## AF268/AF270：已撤回的 R1 提示词试验（历史）

AF272 已撤回本轮集合成员判别提示词，恢复原提示词字节。**提示词已撤回，结果仅作历史**。原48次录制、campaign及 `target/eval-r1-af268/report.json`/Markdown 均保留，不再重录或覆盖；原题40槽自动字段34→31，虚构题8/8，不作为当前默认回归门槛。

`EVAL_AF268=history`（兼容 `trial`）只核对原报告与原录制的完整绑定/输出，然后展示历史结果，不生成当前请求、不要求当前提示词指纹、不访问模型、不改写历史产物。默认测试不运行该入口；`EVAL_RECORD` 只允许缺省或0，其他值拒绝。历史输入与虚构期望保留在 `af268.rs` 及原请求绑定中。

```sh
EVAL_AF268=history EVAL_RECORD=0 PMR_MODEL_NETWORK=offline \
  cargo test --offline --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
```

### AF272：r1_03 完整分支与 AF250 离线重评

原 aggregate/list 分支保留（relation 为 allergic_to 或 null）；新增 single/lookup 分支必须 `plan.filter.relation=allergic_to`，且 need=judge、current_evidence=query_only、subject=self、无额外条件/时间与原分支一致。有效关系过滤会枚举该关系全部记录，数量/字节预算不足仍须报告截断；不接受无有效关系过滤的 lookup，也不串配两支的 type/op。只改标签，模型输入/ID/请求指纹不变；标签修订为 AF272，2026-10-11。

以下入口用当前恢复的完整 R1 请求严格匹配 AF250 五档200条原录制，0发送；不复用过期输出、不补录、不修改原报告/台账。独立报告为 `target/eval-r1-af272/report.json` 与 `report.md`，分列原/新自动字段分数与 r1_03 每次结果；新增标签的通过仍需人工确认，不冒用旧人工裁定。R4 不重评。

```sh
EVAL_AF268=af250 EVAL_RECORD=0 PMR_MODEL_NETWORK=offline \
  cargo test --offline --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
```

## AF230：五档召回录制与协议冒烟门控

AF228历史授权为六档正式528＋冒烟18＝546槽；原始 `authorization.json`、旧冒烟录制与账本保留不改。AF230追加ds-flash新冒烟3槽，历史授权上限549；撤销Haiku正式88槽后，当前可用461槽＝旧冒烟18＋新冒烟3＋正式440。撤销的额度不能分配给其他档。实际发送数从账本统计，计划槽位不等于已发送数。

正式固定五档：`ds-pro`、`ds-flash`、`gpt-6.1-sol`、`gpt-6-luna`、`qwen38-flash-nothink`；44样本×2×5档＝**440正式槽**，每档88。Haiku与Sonnet保留配置和历史产物，但本轮选择器拒绝它们。`EVAL_RECORD` 必须显式等于549，按档筛选不能扩额；每个独立campaign上限仍为88或3。未设置（或为0）时只离线回放与写报告，不读取密钥或创建客户端。

**ds-flash评测上限**：R1与所有R4上限16384，与生产普通阶段4096不同；强度仍minimal/计算low，超时仍180/计算240秒。这些参数进入模型请求和严格指纹。旧Flash冒烟不能证明新配置，新三条冒烟使用独立 `smoke-af230/{recordings,campaigns}/ds-flash/`；四个其他档严格复用原 `smoke/` 下录制。当前五档冒烟共15槽，其中12槽复用、3槽待新录制。

干跑不发送、不读取密钥、不创建产物，即使带 `EVAL_RECORD=549`：

```sh
EVAL_RECALL_COMPARE=af230-smoke EVAL_RECALL_PROFILES=ds-flash EVAL_DRY_RUN=1 cargo test --offline --test write_stage_eval dry_run_plan -- --exact --nocapture
EVAL_RECALL_COMPARE=af230 EVAL_DRY_RUN=1 cargo test --offline --test write_stage_eval dry_run_plan -- --exact --nocapture
```

以下为**授权后的发送命令，本次实施不执行**。须在有对应环境变量/密钥的shell运行。可加 `EVAL_RECALL_PROFILES=ds-pro,gpt-6.1-sol` 分批；各档串行，DeepSeek两档共用执行锁，不应同时运行。

```sh
EVAL_RECALL_COMPARE=af230-smoke EVAL_RECALL_PROFILES=ds-flash EVAL_RECORD=549 cargo test --offline --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
EVAL_RECALL_COMPARE=af230 EVAL_RECORD=549 cargo test --offline --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
```

目录继续使用 `target/eval-providers/recall-af228/`，正式为 `recordings/<profile>/repeat-N/`、`campaigns/<profile>/`。首次新发送前追加 `authorization-af230.json`：严格核对原AF228身份，绑定原授权摘要、新完整请求清单与剩余额度；不改原清单。端点另进入各campaign严格指纹；未更改请求的四档旧冒烟有效。旧入口名 `af228`/`af224` 仍接受，路由到当前AF230计划；旧 `EVAL_RECORD=546` 不再允许发送。

每档冒烟：R1 residence、R4 stored-only lookup、R4 count（`current_evidence=unknown`，测试允许current的schema与计算容量）各一次。正式实发前只核验三条当前严格绑定录制的**completed、原schema、raw→Z18→final最终机械核验、数字usage**。原始输出缺旧方依据但Z18补入后通过核验可放行；最终引用/身份/类型等核验失败、缺失、过期、中断或缺usage仍阻断该档。**自动语义字段评分与人工语义确认都不作为冒烟门槛**，仍原样报告（如计数结果/归组不符）。干跑显示门控状态，但440是潜在槽位；Flash新冒烟通过前其正式发送数强制为0。

复用发送前日志、逐调用原子录制、不重试、严格回放与续跑。发送/在途都占槽；已发送却无录制跳过、不重发，报告“未录制（中断在途）”。同档连续3次相同HTTP status+error_type/code，或incomplete(max_output_tokens)，持久化熔断，其余槽“熔断未发送”、不占额度。Anthropic原生失败立即停档，不自动`prompt_schema`重发；Haiku后续简化schema实验须另立任务。截断首/尾正文与reasoning尾片段有界保存，仅排障，不参加评分。

新JSON与Markdown报告写正式 `reports-af230/<所选档案>/`、冒烟 `smoke-af230/reports-af230/<所选档案>/`，不覆盖AF228旧报告。刷新当前五档已有录制，默认只报告不因评分失败退出；`EVAL_GATE=1`仍可要求所选档自动字段通过，这与协议冒烟门控独立。配置、损坏录制、冻结身份/额度不符仍直接失败。报告保留raw/final、分阶段自动字段/人工确认/待复核、review_summary与去重review_queue，逐档注明角色设置与Flash容量差异。延迟按档×阶段×计算与否×结构化模式分组，P50/P90仅用实际elapsed_ms，未知不填0。

费用按AF163/AF165官方价与录制usage估算（美元按7.1，Qwen3.8输入/缓存/输出0.8/0.1/2.7元/M），代理实付可能不同；推理已包含在输出中。每通过样本成本分单次字段通过、两次字段均通过、两次人工确认，分子含失败费用，usage不全/在途中断时总额与单价为null。冒烟每题一次不宣称重复稳定通过；容量上界不是预计费用，两个阶段延迟不能相加当端到端延迟。

离线重评（不发送，旧录制/账本/报告不变，仅写新报告）：

```sh
env -u EVAL_RECORD PMR_MODEL_NETWORK=offline EVAL_RECALL_COMPARE=af230 \
  cargo test --offline --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
```


### 火山 DeepSeek 环境变量迁移（AF243/AF244）

既有火山 `ds-pro` / `ds-flash` 现读 `HUOSHAN_DEEPSEEK_*`，仍保留历史provider绑定
字段名；`endpoint_env` 不进指纹。实际端点不变时严格回放与原授权身份不变，
详细规则见 [写入侧说明](../write_stage/README.md#火山-deepseek-环境变量迁移af243)。

## AF247：只换标签的严格离线重评

AF247（2026-10-11）不修改输入、ID、提示词、请求指纹、provider、录制或账本，正式计划仍是 AF230 的五档×88＝440槽。修订为：

- R1_02/17 接受 context_only/judge 完整分支，两支均须 assertion_present/self；02 的到访关系接受 visited/fact/null。
- R1_11 接受条件或 recurring 时间分支，single/temporal 均可，仍为 lookup。非空 time/filter.time 只能保留周二周期，不接受 unknown、具体日期或相对偏移；两处字段不允许串配。
- R1_15 不强制条件字段，仍须 judge/query_only；写代码的用途与附加时间/条件由人工检查，不把使用当偏好。AF247 当时保留 R1_03 aggregate/list，仅补诊断 note；该条现由 AF272 的完整分支补充，见上文。
- R4_03 supported/insufficient 两支均要求同一有出处事实、原条件和 applies=unknown/no，不能变成无条件答案。
- R4_07 按 AF246 允许旧自述 current=no/unknown 或省略；本轮住址仍须 current=yes，禁止旧值 current=yes、supersedes 与把旧值当真实历史。statement 中的历史/搬家含义仍须人工确认。
- R4_14 可保留重复条目同组、合并双方依据，或选三月与八月各一个事件代表。代表分支恰好两个条目、不同非空组、exact=2，无额外事件；三月两次提及的同一性留人工核验。R4_21 保留 insufficient 与103/104的 negate，仅加诊断 note。

生成器与样本一致；只改标注会改变人工台账的标签绑定，旧人工确认不能冒用。报告继续分列自动字段与 review_queue、raw/final，字段通过不等于语义已确认。

以下命令严格复用 AF230 现有录制，0发送，报告另写 `target/eval-providers/recall-af228/reports-af247/<五档选择>/`：

```sh
env -u EVAL_RECORD -u EVAL_GATE PMR_MODEL_NETWORK=offline \
  EVAL_RECALL_COMPARE=af230 EVAL_RECALL_REPORT_DIR=reports-af247 \
  cargo test --offline --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
```

`EVAL_RECALL_REPORT_DIR` 仅允许离线使用 `reports-<revision>` 单层目录名，不影响绑定或授权；不能携带路径/跳出报告根目录，也不能用于实发。缺失/过期/机械失败继续如实报告，不为复用录制放宽评分或修指纹。未指定目录时维持 AF230 原行为；需要保留旧报告时必须显式指定新目录。
