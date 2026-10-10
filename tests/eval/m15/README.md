# M1.5 虚构 MVP 与 W1 推理强度对照

本目录仅评测接线和虚构输入；不修改生产提示词、契约、模型默认值或用户数据库。
`mvp.json` 和 `w1.json` 全部 `fictional=true`，标注状态以各文件的 `label_status` 为准，由 Claude 审核、用户确认。
确认时只改标注，不能把标签或预期结果加进请求。默认测试仍是原 63 题的严格离线回放；历史合并 59/4 单独核验。

## AF96：W3 更正主体的旧/新提示词对照

`w3.json` 是四个独立虚构场景：明确先行语、历史事实矛盾但主体明确、多个可能主体、
无先行语且目录唯一。负例也绑定目录内实体，使 W2=ok；指代是否成立由 W3 判断。
两个正例都指向明确登记的非 self 主体，避免默认绑定用户本人造成假阳性。
多个主体场景另含独立计划，检查局部不足不阻断无关候选。标签为 draft，Claude 验收后复核；
不靠提高 supported 数判好坏，正例须支持、负例须拒收。

旧臂来自 `91a068a:src/write_models.rs` 的 `W3_PROMPT` 原始 UTF-8 字节，保存在
`w3-old-91a068a.txt`（6838 字节，`fnv1a64:e20f41189ca2159c`），不加末尾换行，
不从 HEAD 或当前常量获取。新臂使用当前生产 `W3_PROMPT`。两臂都经生产 `structured_request`
构造，公共指令前缀、schema 和输入相同。每场景 W2 仅装配一次、无 W1 调用；
完整输入复制至两臂和两次重复。arm、repeat、focus、标签和预期不进模型输入。

固定 Pro（`ep-20261003130756-trhj6`）、minimal、w3.v3、said_on=2026-10-07、UTC+8、
max_output_tokens=16384。四场景 × 旧/新两臂 × 两次 = 16 个槽位；按场景交错，
第1次 old→new、第2次 new→old。两次仅供定点诊断，不证明统计显著。

```sh
# 本实施只执行下列零调用检查。干跑不读密钥、不创建录制。
EVAL_M15=w3 EVAL_DRY_RUN=1 cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
# 无 EVAL_RECORD：严格回放，默认只报告，不因模型分数中止。
EVAL_M15=w3 cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture

# AF96 已批准，仍须 Claude 验收后执行；本实施不执行。
EVAL_M15=w3 EVAL_RECORD=16 cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
```

请求完整指纹保留提示词、schema、完整输入（含时钟）、模型、effort、输出上限；
另核 base URL、arm、repeat。旧/新录制分目录，不能跨臂或重复借用响应。
根目录固定为 `target/eval-m15/w3-af96/`，报告为其 `report.json`，发送日志共用根目录 `sends/`。
此模式拒绝 EVAL_M15_DIR/OUTPUT、CASE/STAGE 筛选、FIXED_INPUTS/HISTORY、MODEL/LABEL、
REASONING_EFFORT、REPEAT、MAX_OUTPUT_TOKENS 覆盖；可提供同一服务的 BASE_URL/API_KEY。
保持同一 base URL 以复用；改变地址须如实报过期，不能获得额外额度。

EVAL_RECORD=15 且首次缺口16时，在读取密钥/建客户端前整批拒绝。
累计成功、失败、无响应 started 均占独立16次额度；发送前 fsync started，响应逐次原子保存并 fsync。
成功和失败响应均按严格绑定复用、不自动重试。累计发送/在途 + 缺口 >16 时拒绝，
中断导致额度不足时保留缺口，不能删除日志/换目录补发。整个发送批持有同一 OS 文件锁；
并发执行者拒绝发送，进程退出或崩溃后锁由 OS 释放，累计账本保留。
报告按臂、场景、repeat 展示结果、延迟和 usage（输出含推理 token），旧臂失败也全部报告，
默认 EVAL_GATE=0；只在显式 EVAL_GATE=1 时因评分失败退出。该配置不改变默认普通评测。

生产同轮 W3 阶段缓存不绑定提示词，本轮不改变其失效政策，也不自动处理已有 pending。
上述对照直接构造独立阶段请求，不使用生产阶段缓存或生产数据。

## AF101：W3 minimal / low 强度对照

独立入口 `EVAL_M15=w3-effort` 使用 `w3-effort.json` 的四个虚构场景。原话及第一条
候选的 sentence/value 同步用“是63”，03 保留独立计划；04 核验的是编号所属的人，
不能将记错编号或更正发言的人推作所属实体，保持 insufficient[subject]。新标签 draft，
由 Claude 确认；报告按标签计分时仍显示 draft，不作为已确认验收结论。
原 `w3.json`、AF96 录制和累计账本保留，不能改写旧结果。措辞改变后 AF96 minimal
响应也不能充当新对照，因此冷启动需要完整 16 次。

两臂固定 dc604f8 的当前 W3 提示词，字节指纹 `fnv1a64:a20cc11e24e4d855`；
代码对当前常量校验该指纹，提示词改变则拒绝执行本组，不能静默切换版本。
固定 Pro、w3.v3、said_on=2026-10-07、UTC+8、max_output_tokens=16384；每场景 W2
仅装配一次并复制，两臂请求仅 reasoning.effort（minimal/low）不同。每场景第一遍
minimal→low，第二遍 low→minimal。标签、focus、effort_arm、repeat 不进入模型输入。

完整指纹显式绑定整个请求（包括 effort、模型、提示词、schema、输入、时钟及输出上限），
另核 base URL、试验身份、arm、repeat、提示词字节指纹。根目录固定为
`target/eval-m15/w3-af101/`，臂与重复分目录；报告 `report.json`，共享累计账本 `sends/`。
新增独立累计上限 16，AF96 已花费的 16 次不重置、不转用。与 AF96 相同的环境覆盖限制、
整批预检、OS 锁、发送前 fsync、逐次保存、失败/无响应计费及严格续跑规则继续适用。
15 次额度不足时先于密钥/客户端拒绝；不自动重试，不删除日志或换目录补发。

```sh
# 干跑：零调用、无密钥、无录制写入。
EVAL_M15=w3-effort EVAL_DRY_RUN=1 cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
# 默认严格回放：只报告缺口，不发请求；报告写 target/。
EVAL_M15=w3-effort cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
# 追加 16 次已授权，Claude 验收并确认标签后执行；Codex 本轮不执行。
EVAL_M15=w3-effort EVAL_RECORD=16 cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
```

报告提供每候选 verdict/aspects/引用、每臂每场景每次结果、延迟及 usage（输出含推理 token）。
输出容量合计 262144 token 是配置上限，不能用旧 minimal 平均约 110 token 预测 low 成本。
默认 EVAL_GATE=0，显式 1 才因模型评分失败退出。对 low 的定点改善要求两次均满足：
01/02 supported，04 insufficient[subject]，03 的主体负例及独立计划不退步；不能仅比较
supported 数量。两次重复不证明统计显著；切生产缺省需另行决定，不由工具自动修改。

## 零调用检查和干跑

```sh
python3 tests/eval/m15/check_samples.py
python3 -m unittest discover -s tests/eval/scoring -v
EVAL_M15=mvp EVAL_DRY_RUN=1 cargo test --test eval resident_eval_suite -- --exact --nocapture
EVAL_M15=w1 EVAL_DRY_RUN=1 cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
```

AF56 修复 MVP 的累计授权上限为 24，使用独立目录 `target/eval-m15/mvp-af56/`，只读复用首轮
`recordings/mvp/`（可通过 EVAL_M15_REUSE_DIR 指定）。当前完整请求匹配可复用 6 个 R1；
两轮授权分别记账；首轮已花费的调用和发送日志不重置，本目录不得用于补发旧轮。
新发送精确预留上限 17：W1×4、W3×4、W4×3、R4×6。首条来源没有旧记录或关系，生产跳过 W4。
所有 R1 都不匹配时预留 23；W1 low/提示词、W3/W4 提示词、R4 v5/提示词改变均如实过期。
上述复用数是 AF56 当时的结果。AF90 后 W1/W3 v3、R1 v2、R4/recall v6 的完整请求重新绑定；旧录制如实报告过期，不能据历史复用数补发或改变指纹。
旧配置仅作为复用出处保留；每条旧请求内容与指纹都必须匹配，不改旧指纹或响应，不因配置差异放宽阶段校验。
bootstrap 和实体登记不调用模型。W4 不需要时直接走生产 W5；R1/核验使某阶段跳过时少发，预算不转作重试。
W1 对照固定六场景 × low/medium × 两次，仅 W1，24 个独立槽位。干跑以完整请求字节粗估输入，
样本 ID 为无语义编号，focus/预期复核项不进请求；按同样本/同重复配对交错 low 和 medium。
本组 W1 两种强度均用生产写入侧的 360 秒超时，避免旧阶段脚本的 180 秒提前截断 medium。
列出输出配置容量，并参考 AC2 公开录制中的 usage 均值；W1 low 对照实测平均输出 3337 token、medium 4512，
容量估算仍借 AC2 medium，不保证未来节省或质量。
动态 W3/W4/R4 输入未知，精确的是预留数，不是未来实际调用数/token/费用。
`EVAL_RECORD` 小于本次计划的预留上限时整批拒绝，先于密钥读取和监听端口；
W1 续跑按严格可复用的成功/失败响应计算剩余槽位。只有显式 EVAL_RECORD 才启用物理发送。

## MVP 输入和执行顺序

预登记 `child-demo`，名字小栎，别名我女儿。四条原话覆盖：生日、生日更正、预订事实与带条件的计划、
计划改期但票尚未改签；两题覆盖正确生日，以及最新行程/散步条件/票据状态。全部是虚构数据。
输入全文和硬性判分要求在 `mvp.json`，不出现在生产提示词中。

入口调用 `ChatMemoryService::call(memory_prepare)`，保留真实保存、入队和前台召回语义。
服务实例不启动 worker；每次 prepare 后通过生产 `drain_enrichment(1)` 显式驱动唯一已入队任务，
执行实际 W1→W2→W3→W4/W5 路径，随后重开同一个临时 SQLite 的服务连接，完成 bootstrap 和两个
空 context、独立 request_id 的查询。这是生产 CLI 分发使用的同一服务入口，不伪造 prepare 回执，
也不在评测中重写后台判定。选择受控 drain 是为了可复现录制顺序；自动 worker 并发另由既有生产测试覆盖。

MVP 代理在前台请求前从该临时库的 T1 记录读取 source ID→turn ID，仅规范化结构化出处字段；
句子、值、提示词、schema、模型 ID、时钟、推理强度仍严格进入指纹。绑定只用于评测，不涉及 src。

## 已授权录制后的使用（本任务未执行）

环境提供 `HUOSHAN_DEEPSEEK_BASE_URL`、`HUOSHAN_DEEPSEEK_API_KEY`。MVP 固定当前生产角色配置；W1 对照固定 Pro。
W1 可用 EVAL_BASE_URL/EVAL_API_KEY 指定相同服务的凭据；EVAL_MODEL/EVAL_REASONING_EFFORT/EVAL_REPEAT
不改变这组固定 Pro、两种强度、两次重复设计。目录通过 EVAL_M15_DIR、报告通过 EVAL_M15_OUTPUT 覆盖。

```sh
# 每组均有独立的已批准 24 次上限，不能合并给另一组使用。
EVAL_M15=mvp EVAL_RECORD=24 cargo test --test eval resident_eval_suite -- --exact --nocapture
EVAL_M15=w1 EVAL_RECORD=24 cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture

# 不设 EVAL_RECORD：严格回放，空录制/过期只报缺口，不调用真实模型。
EVAL_M15=mvp cargo test --test eval resident_eval_suite -- --exact --nocapture
EVAL_M15=w1 cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
```

本轮 MVP 新录制默认在 `target/eval-m15/mvp-af56/`，W1 对照在 `recordings/w1/`；修复目录不能覆盖复用目录。
每次发送前写入并 fsync `started` 日志，响应逐次
原子写入并 fsync；MVP 日志在 runs/，W1 日志在各模型/重复目录的 sends/。日志条目数是已进入
物理发送的计数，started 无响应表示可能在途/计费，不证明服务器已收到。中断后严格复用已有响应，
失败响应也复用，不能挑成功；没有响应的调用重启可能需要补发，先核对发送日志和授权剩余额度。
同一目录累计发送/在途计数自动扣除 24 次额度；W1 缺口加累计计数超限时整批拒绝，MVP 代理仅可发送剩余额度。
中断但无录制的调用也消耗额度；额度不足不补发。不要删除日志或换目录来重试或绕过指纹。

离线报告默认写 `target/eval-m15/`，不写已跟踪录制目录。MVP `mvp-run.json` 保存每条 prepare 回执、
最终来源/任务/模型审计和新会话回答，`mvp-run-report.json` 有 common.v1 逐项分数、复核 key、
完整 view、阶段 usage/延迟与本地核验原因；原始模型响应仍保留在逐调用录制里。
W1 `w1-report.json` 按强度分别提供两次 raw、W2 最终候选、机械检查、coverage/主体/限定语人工复核、
usage 与延迟；W2 只作机械核验，空候选不能视为语义通过。

## 评分边界

v3/v4/v5 历史适配保留 `recall_v3@1` / `recall_v4@1` / `recall_v5@1`，当前 v6 使用 `recall_v6@1`；仅增加版本接收，评分标准不变，不复用旧语义裁定。
MVP 的主体、生日更正、计划/预订事实、日期角色、更新、条件与可能性均为必审项；
`answer_requirements` 对整份答案复核，容许分句合并/拆分、同义表达与标点差异，不靠句子顺序配对。
出处、已绑定字段、完全缺失的条件/不确定字段机械判分；限定语片段存在时，其语义和所修饰安排仍待复核。
`plan_as_occurred` 是硬性禁止项，但须人读实际 statement 裁定，不能由候选 speech=plan 推定答案合格。
日期保留与计划修改的正确关联须人工裁定，不用关键词/NLP 猜语义。所有 review/missing/draft 均不算通过。

扫描仅读取仓库公开评测输入；检查新输入与旧原文的包含重叠，以及 AF36 已公布生产主题禁词。
不打开私有快照/轨迹，不复制私有原话；自动扫描不能证明不存在未公布私有措辞，样本全部人工虚构。
