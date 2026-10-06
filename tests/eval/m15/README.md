# M1.5 虚构 MVP 与 W1 推理强度对照

本目录仅评测接线和虚构输入；不修改生产提示词、契约、模型默认值或用户数据库。
`mvp.json` 和 `w1.json` 全部 `fictional=true, label_status=draft`，先由 Claude 审核、用户确认。
确认时只改标注，不能把标签或预期结果加进请求。默认测试仍是原 63 题的严格离线回放；历史合并 59/4 单独核验。

## 零调用检查和干跑

```sh
python3 tests/eval/m15/check_samples.py
python3 -m unittest discover -s tests/eval/scoring -v
EVAL_M15=mvp EVAL_DRY_RUN=1 cargo test --test eval resident_eval_suite -- --exact --nocapture
EVAL_M15=w1 EVAL_DRY_RUN=1 cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
```

MVP 精确预留 24：四条消息各至多 W1/W3/W4 + 前台 R1/R4，两个新会话查询各至多 R1/R4。
bootstrap 和实体登记不调用模型。W4 不需要时直接走生产 W5；R1/核验使某阶段跳过时少发，预算不转作重试。
W1 对照固定六场景 × low/medium × 两次，仅 W1，24 个独立槽位。干跑以完整请求字节粗估输入，
样本 ID 为无语义编号，focus/预期复核项不进请求；按同样本/同重复配对交错 low 和 medium。
本组 W1 两种强度均用生产写入侧的 360 秒超时，避免旧阶段脚本的 180 秒提前截断 medium。
列出输出配置容量，并参考 AC2 公开录制中的 usage 均值；low 的估算暂借 medium，不声称已经节省。
动态 W3/W4/R4 输入未知，精确的是预留数，不是未来实际调用数/token/费用。
`EVAL_RECORD=23` 在全新计划下整批拒绝，先于密钥读取和监听端口；MVP 每次启动均要求预留 24，
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

环境提供 `DEEPSEEK_BASE_URL`、`DEEPSEEK_API_KEY`。MVP 固定当前生产角色配置；W1 对照固定 Pro。
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

录制默认在 `recordings/mvp/`、`recordings/w1/`。每次发送前写入并 fsync `started` 日志，响应逐次
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

v3 历史适配保留 `recall_v3@1`，v4 使用 `recall_v4@1`，不复用旧语义裁定。
MVP 的主体、生日更正、计划/预订事实、日期角色、更新、条件与可能性均为必审项；
`answer_requirements` 对整份答案复核，容许分句合并/拆分、同义表达与标点差异，不靠句子顺序配对。
出处、已绑定字段、完全缺失的条件/不确定字段机械判分；限定语片段存在时，其语义和所修饰安排仍待复核。
`plan_as_occurred` 是硬性禁止项，但须人读实际 statement 裁定，不能由候选 speech=plan 推定答案合格。
日期保留与计划修改的正确关联须人工裁定，不用关键词/NLP 猜语义。所有 review/missing/draft 均不算通过。

扫描仅读取仓库公开评测输入；检查新输入与旧原文的包含重叠，以及 AF36 已公布生产主题禁词。
不打开私有快照/轨迹，不复制私有原话；自动扫描不能证明不存在未公布私有措辞，样本全部人工虚构。
