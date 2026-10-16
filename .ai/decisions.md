# 决定记录

## 2026-10-02 B 段收尾（Codex 会话 01a0fa9a-8d7d-7340-b237-ee2447511b7e，评审第 1 轮）

Codex 评审：7 项修复中 ①②④⑤⑦ 通过，③⑥ 有问题，需改代码后再提交。Claude 已对照代码核实下列三处。

| # | Codex 意见 | 结论 | 理由 |
|---|---|---|---|
| D1 | ③ `src/commit.rs:848` 只按前身是否在 `seen` 判断重判覆盖了关系缺口；关系因任务隔离未送入 W4 时仍会清除 update_relation_unclear | 采纳，改代码 | w5-storage 第 3 节要求“W4 输入包含前身及其最新关系状态”，被隔离的关系 W4 没看到，不能算覆盖 |
| D2 | ③ `src/w4.rs:490` / `related_records` 把已删除前身也算作必需输入，删除前身后缺口永远清不掉 | 采纳，改代码 | 已删除记录按契约不进入 W4 输入；覆盖判断只针对仍存在的前身及其关系（含 invalidated） |
| D3 | ⑥ `src/w4.rs:1032` 预算在过滤已删除记录前计数，误判超限 | 采纳，改代码 | 预算应只计实际可送入的记录 |
| D4 | A1 补“任务隔离后重判”“删除前身后重判及预算”两个交叉回归 | 采纳 | 正是 D1–D3 的失败场景 |
| D5 | A3 改为比较重新生成结果的文件集合与字节 | 采纳 | fixtures 未跟踪，git diff 看不出 |
| D6 | A5 增加“已结束成员的计数规则” | 采纳 | 第 ⑤ 项修复改了该规则，文档须同步 |
| D7 | 预算定义为记录数上限；候选数、关系数、原文长度的调用预算留到运行时接线 | 采纳，文档注明 | 本段不调用模型，真实 token 预算在接线时才可测 |
| D8 | “无新回归”限定为约定测试覆盖范围 | 采纳 | 如实表述验证边界 |

范围不变：不改冻结契约，effective 是否收窄仍为用户待决项。

## 2026-10-02 B 段验收（Codex 实施，同一会话）

Codex 交付：D1–D3 修复，新增 3 个回归（各自在修复前实跑失败）。它在沙箱内跑全套时 16 项因无法绑定本地端口失败（沙箱限制，未放开）。
Claude 复核（沙箱外）：
- 全套 `cargo test` 450 通过 / 0 失败 / 33 忽略（447 + 3 个新回归）。
- `test_samples.py` 18 OK；`check_samples.py` 23 ok；fixtures 复制后重新生成 `diff -r` 无差异。
- 5 个改动的 Rust 文件 rustfmt 检查干净。
- 抽查 diff：`related_records` 过滤已删除（D2）；重判覆盖要求前身全部关系都有 `seen_relations` 快照（D1）；`validate` 仅在 scope_complete=false 时放宽隔离关系的内容快照，open 编号集合仍核对。
- 文档：旧的“生效时间只允许 state_change/correction/refine”已删除，A5 列出各点均有对应段落。
结论：验收通过，提交 f1d1828（仅 brief 所列文件）。

## 2026-10-02 运行时接线方案（Codex 会话 01a0fa9a-…，评审第 1 轮，日志 .ai/log/codex-review-2.jsonl）

| # | Codex 意见 | 结论 | 理由 |
|---|---|---|---|
| E1 | 上游来源选 b：本项做接线准备与离线闭环，生产切换与第 2 项（W1/W3/W4 模型调用）一起完成 | 采纳 | `CandidateMemory` 是投影，反推完整候选会补造限定语，违反“不静默丢弃/伪造限定语”；a 的临时适配器是纯债务 |
| E2 | 需要 W4 但未接入时：保留候选、显式报告能力未接入、不提交，不回落旧策略；测试注入夹具，生产不读夹具 | 采纳 | 符合 2.5“失败不当作不存在”；避免旧确定性策略冒充 W4 |
| E3 | 新旧并存：默认旧路径；处理版本随来源任务持久绑定；旧回执原样重放 | 采纳 | 不在未验收前改变对外行为 |
| E4 | 证据 ID：新路径复用 `w1:<source_id>:<index>`；旧 ID/回执/指纹不批量改写，旧解码器不改 | 采纳 | 改旧解码器会使 W3 review binding 与录制失效，违反“不重录” |
| E5 | 部分提交后崩溃的续跑与显式重判须区分，不能笼统放宽顺序屏障 | 采纳，放第 ② 段 | `claim_source` 对终态来源返回 None（chat.rs:1700），Claude 已核实 |
| E6 | fact 检索：查询在 `lexical.rs`，`fact_index.rs` 是可选派生提示；索引不可用不能当零命中；片段截断不等于记录 omitted | 采纳 | Claude 已核实 fact_index.rs 头注释与 lexical.rs 接口 |
| E7 | 补输入预算（候选/关系/字节）与时间基准；不宣称满足 2.5/2.6 | 采纳，字节/候选预算放第 ③ 段 | 与 D7 一致 |
| E8 | 分 4 段：① fact 检索 ② 轮次与恢复 ③ 新路径离线闭环 ④ 与第 2 项共同切换 | 采纳 | 第一段小且与提交流程解耦，可独立提交 |

产品影响（需向用户报告）：m1-status 第 3 节第 1 项的“替换 commit_candidates（生产）”实际在第 ④ 段、与第 2 项一同完成；第 1 项本身交付离线闭环。

## 2026-10-02 第 ① 段验收（第 1 次）
Claude 复核：全套 456 通过 / 0 失败 / 33 忽略；样本、格式检查通过；降级回落词元规则并标 Fallback 的选择采纳（不把故障当零命中，且旧数据仍可处理）。
退回 1 项（修复第 1 轮）：
- F1 `search_w4_facts`（src/lexical.rs 新增查询）只按 `received_sequence DESC` 排序。中文相邻字对 OR 查询几乎命中所有共字记录，`budget.facts` 截断时较新的弱相关记录会挤掉较旧的强相关记录；现有原文检索（lexical.rs:820）为此先按实体/短语命中数排序再按时间，B 段旧规则也按共有词元数排序。要求按命中短语（及实体）数降序、再按时间排序，与现有 ranking 一致；补测试：较旧、共有短语多的记录排在较新、仅共一个短语的记录之前，且在 facts=1 时被选中。
F1 修复后复核：全套 458 / 0 / 33；新增 2 个排序回归（修复前失败）。验收通过，提交第 ① 段。

## 2026-10-02 第 ② 段设计（Codex 评审第 3 次，日志 .ai/log/codex-review-3.jsonl）
| # | 内容 | 结论 | 理由 |
|---|---|---|---|
| G1 | 第 ② 段范围：轮次、完整提交计划持久化、W5 提交恢复；模型阶段检查点留给 ③；不合并 | 采纳 | 可用现有 Plan/W5Group 独立测试 |
| G2 | 数据库 8→9，无新表；版本 8 程序拒读 9 | **用户批准（2026-10-02）** | 新字段在快照中，旧程序改写会丢失 |
| G3 | 已提交进度从 w5_receipts 推导，不另存游标 | 采纳 | 单一事实来源，避免两份状态不一致 |
| G4 | 续跑不加轮次；needs_rejudge / 新语义判定须显式请求，round+1，新预算操作；普通重放不触发 | 采纳 | 符合 2.5“新的模型尝试需显式预算” |
| G5 | 新增 order_sequence（单调序号空间与接收序号共用）；received_sequence/observed_at 保持接收含义；其他来源屏障不放宽 | 采纳 | 重判不能改写原文时间基准 |
| G6 | 用归属+租约+输入比对+指纹+屏障替代全局 revision 等值检查 | 采纳 | 自身首组提交会推进 revision，旧检查会挡住余组 |
| G7 | forget.rs 纳入：检查点清洗、受影响计划失效 | 采纳 | 删除后不得残留原文副本 |
| G8 | 不新增平行状态枚举；本段不启用新 MCP 路径，旧输出不变 | 采纳 | 不改对外行为 |
- H1 Codex 实施前停下：升版需改 6 个现有测试的版本断言/任务构造缺省；Claude 授权，限机械调整（task.md 范围补充）。

## 2026-10-02 第 ② 段验收
Claude 复核（沙箱外）：全套 474 / 0 / 33（+16 新增恢复回归）；s2_capture.py、s3_process.py、W4 样本检查通过；6 个现有测试文件 diff 仅版本号与 `..Default::default()`；rustfmt 干净。
代码抽查：旧任务 `order_sequence()` 回落 received_sequence，旧路径屏障行为不变；`next_source_sequence` 只会更大、不复用序号；`claim_source` 拒抢新处理版本任务；`w4_summary` 只写入新路径来源。
结论：通过，提交第 ② 段。

## 2026-10-02 第 ③ 段设计（Codex 评审第 4 次，日志 .ai/log/codex-review-4.jsonl）
数据库保持 9、不改冻结契约、不新增 MCP 输出 —— 无需用户裁定。Claude 核对：来源状态（pending / needs_rejudge / budget_exhausted / failed / no_candidates_unreviewed / committed）与候选状态均在 contracts 3.5 枚举内。
| # | 内容 | 结论 | 理由 |
|---|---|---|---|
| I1 | 新处理版本 `w4_runtime_v2`，v1 计划续交、v1 无计划须重判 | **不采纳** | v1 于 a1d578b 当天引入，无运行时调用方登记来源，任何真实库中都不存在 v1 任务；兼容逻辑没有对象。直接在 v1 上扩展语义（含 `w4_save_inputs`） |
| I2 | `src/enrichment.rs::run_source` 串联；上游与 W4 两个提供者以函数参数注入；不允许注入 Plan/组 ID/快照/committed_id；有持久计划时不调用提供者 | 采纳 | ④ 只需替换提供者 |
| I3 | 需要 W4 而未接入：保存候选，pending，释放认领，零业务提交，不回落旧策略 | 采纳 | 同 E2 |
| I4 | `w4_save_inputs` 在计划前保存候选，同轮不可替换；元数据入 `result.w4_input_meta`；删除清洗沿用检查点 | 采纳 | 恢复时准入结果不丢 |
| I5 | 独立 `RuntimeInputBudget`（原文字节 / 候选数 / 关系数），超限 budget_exhausted，记录项、实际值、上限；不截断后继续 | 采纳 | 不改旧取数行为 |
| I6 | 专用回执转换，不复用 `source_response`；completed_no_facts 本段不产生 | 采纳 | 旧转换会把 pending 原因转成 failed |
| I7 | 已执行撤回：候选 committed、memory_id=null，多目标留在 W5 回执；无目标撤回 pending | 采纳 | 与 3.5 “找不到目标的 retraction 为 pending” 对应 |
| I8 | `w4_finish_round` 按逐候选实际结果汇总 | 采纳 | 避免零写入撤回误报完成 |

## 2026-10-02 第 ③ 段验收
Claude 复核（沙箱外）：全套 504 / 0 / 33（+30）；样本检查通过；rustfmt 干净；lib.rs 仅一行；`src/enrichment.rs` 无夹具读取。
代码审查：已存输入/计划优先复用，不重复调用提供者；错误只持久化错误码；W4 未接入 → pending、零写入。
遗留（第 ④ 段必做）：`enrichment::receipt` 中 `memory_requests: []`、`retention_pending: false` 为写死值；接入 W1 后必须来自真实提取结果，不能以 false 掩盖留存请求（target-design 1.3）。默认预算值（64/256/256KB）为占位，④ 按实测校准。
结论：通过，提交第 ③ 段。

## 2026-10-02 第 ④ 段方案（Codex 评审第 5 次，日志 .ai/log/codex-review-5.jsonl）
| # | 内容 | 结论 | 理由 |
|---|---|---|---|
| J1 | 拆 5 小段：④-1 纯 W2 → ④-2 上游完整批次结果与状态汇总 → ④-3 W1/W3 提供者 → ④-4 W4 提供者 → ④-5 配置与 CLI 分发 | 采纳 | 第一段不依赖样本与真实模型 |
| J2 | `run_source` 上游接口扩展为完整批次结果（拒收、W3 不支持、请求、覆盖、准入），coverage_gaps 不被 finish 覆盖 | 采纳，④-2 | 否则回执丢失拒收与缺口 |
| J3 | 阶段结果需持久化并带输入绑定；有调用记录无结果的崩溃窗口不自动重发 | 采纳，④-3 | model_budget 只有账本无输出；符合“不自动重试” |
| J4 | 重判复用已核验候选，不重跑 W1 后按索引套 committed_id | 采纳 | 防止错绑 |
| J5 | 旧 `RawExtraction`/`SourceReview`/旧解码与修复链路原样保留，新契约另写 | 采纳 | 不影响旧录制 |
| J6 | 新路径本段只允许 deferred/off，经 CLI 整理；inline 与 MCP 后台留到第 4 项解锁 | 采纳 | Claude 核实 pmr-mcp.rs:67 锁覆盖整次调用；“不持有全局锁”与“本段不做并发”只能这样同时满足 |
| J7 | opt-in 后写入回执用冻结契约 3.5 的 source/enrichment 片段，保留外层包络与旧召回体，不外发内部检查点 | 采纳（Claude 决定，不升级给用户） | 仅 opt-in 生效、默认路径不变；片段本身是已冻结、用户认可的契约 |
| J8 | 数据库预计保持 v9；若需升版再报告 | 采纳 | — |
| J9 | W1/W3 语义样本集由 Claude 起草、用户抽查；在 ④-3 之前完成起草 | 采纳 | ④-1/④-2 用合成测试足够；样本用于评价提示词 |
| J10 | 验收补充：实际发送数（W1 空 1 次 / 无需 W4 2 次 / 最多 3 次）、四类恢复反例、受影响旧回放 | 采纳，分配到 ④-3～④-5 | — |

## 2026-10-02 ④-1 验收
Claude 复核（沙箱外）：全套 531 / 0 / 33（+27 W2）；rustfmt 干净；lib.rs 仅一行。提交。
Codex 列出的 6 处契约边界裁定：
| # | 边界 | 裁定 |
|---|---|---|
| K1 | w1.v1 候选无任务字段 → 任务取运行时可信值 | 认可；2.2 的“任务不符”在 w1.v1 下无可比对象 |
| K2 | 未知实体 ID | 认可。契约 1.4 已有 `unresolved` 主体分支，W2 已处理（w2.rs:258）；④-3 的 W1 提示词须引导模型用该分支，而非编造 entity_id |
| K3 | 请求保留原始索引并给最终映射 | 认可；④-2 调用方必须使用映射 |
| K4 | fact 无隐式允许类型 | 认可；**④-2/④-3 硬性要求**：运行时构造的 W2 关系目录必须包含 `fact` 及其允许类型，并有测试证明 fact 候选不会因目录缺项被拒 |
| K5 | 句尾截断仅在原文末尾或明确标点处，歧义拒收 | 认可（保守） |
| K6 | anchored 缺锚点为 unknown；relative week 为七日偏移 | 认可，与现有算法一致 |

## 2026-10-02 ④-2 验收
Claude 复核（沙箱外）：全套 553 / 0 / 33（+22）；W4 样本通过；rustfmt 干净；enrichment_runtime.rs 无删除行；enrichment.rs 不再写死 retention_pending / memory_requests。提交。
裁定：L1 来源状态优先级 needs_rejudge → awaiting_confirmation → coverage_gaps → 普通汇总，认可（细节保留于内部批次）。L2 请求项回执字段（原请求、原始/最终索引、verdict、executed=false 等）认可，不宣称执行。L3 W3 insufficient → 候选 pending（contradicted → rejected），认可：证据不足不等于否定，可经显式重判再提交。
遗留：宿主确认 / 解除暂停入口、旧事实对象定位留后续（非 ④ 范围，记入 m1-status）。
下一步 ④-3（W1/W3 提供者）前，Claude 须先起草 W1/W3 样本集并请用户抽查（J9）。
- J9 调整（2026-10-02）：样本起草与 ④-3 并行。理由：④-3 仅用模拟服务验证管线，不评价提示词质量；样本只卡真实模型评测，而该步本就需用户批准。Claude 起草期间 Codex 不得改 tests/eval/w1、w3。
- M1 用户裁定（2026-10-02）w1_13：历史中明确点名的对象可作为指代绑定依据；仅凭目录唯一性不得绑定。④-3 的 W1 提示词须体现。

## 2026-10-02 ④-3 验收（第 1 次）
Claude 复核（沙箱外）：全套 568 / 0 / 33（+15）；rustfmt 干净；openai.rs 改动仅为 cfg(test) 注入通道与响应读取重排，生产语义不变。
提示词审阅，退回 3 处（修复第 1 轮）：
- N1 W1/W3 未明确警告“事件/状态时间不是保存期限”：retention 请求只在用户明确要求按期限保存或删除时产生（历史录制 59/237 误填即此类，target-design 1.3）。W3 须把把事件时间当 deadline 的请求判为不支持。
- N2 W3 未写明：原文中的不确定、条件、时间、强度被候选丢失或改变时不能 supported（样本 w3_02/03/08/09）。
- N3 W1 未体现用户裁定 M1：当前消息的指代若在 history 中被明确点名为目录实体，可绑定该实体；仅凭目录唯一或排除法不得绑定。
N1–N3 修复后复核：568 / 0 / 33。Claude 另发现提示词例句“明天要去北京开会”与样本 w3_17 原文相同（评测泄漏），由 Claude 直接替换为“下周三去医院复查”（仅 write_models.rs 两处例句与对应提示词测试一行；Codex 当时空闲，无并发修改）；提示词中无其他样本文字。验收通过，与 W1/W3 草稿样本一起提交。

## 2026-10-02 ④-4 验收
Claude 复核（沙箱外）：全套 580 / 0 / 33（+12）；rustfmt 干净。W4 提示词覆盖 2.4 关键约束（双方依据、legacy 只能 pending、范围不完整不下唯一结论、effective 原样复制、解决关系后至多一个成员存续）。防泄漏测试扫描 W1/W3/W4 全部样本原文与 W4 标签。通过，提交。

## 2026-10-02 ④-5 验收（第 ④ 段完成）
Claude 复核（沙箱外）：全套 590 / 0 / 33（含 2 条新 CLI HTTP 端到端测试，沙箱内受端口限制、沙箱外通过）；rustfmt 干净。chat.rs 默认 legacy 分支与 source_response 输出不变。提交。
第 ④ 段（运行时接线 + W1/W3/W4 模型调用）离线部分全部完成；默认仍为旧路径。

## 2026-10-02 评测脚本第 1 轮（Codex 部分交付）
Codex 交付评分内核与干跑（W1 20 / W3 20 / W4 21 = 61 次），完整回放被范围阻塞。
- O1 Codex 指出 Claude 起草的样本有标注错误，均属实，Claude 已修：w1_04 sentence 改为可接受集合；w1_11 subject 不评分；w1_15、w1_03 去掉 fact（fact 须 value==sentence，与短值不能同时成立）；w3_16 第二候选 value 改为整句（原值会被 W2 以 fact_sentence_mismatch 拒收）。check_samples.py 增加 fact 规则，40/40 通过。
- O2 范围补充：允许从 openai.rs 原样抽取请求构造为公共纯函数，生产与评测共用（避免测试复制提示词）。
- P1 用户 2026-10-02 抽查确认 W1/W3 样本的 6 处待定标注（全部同意草稿），w1_13 此前已裁定；W1/W3 样本 label_status 改为 reviewed。W4 的 23 个样本仍为 draft（m1-status 第 7 节，待用户复核）。

## 2026-10-02 评测脚本验收
Claude 复核（沙箱外）：全套 605 / 0 / 33；openai.rs 改动为请求构造原样抽取。Claude 将评测时区从 UTC 改为生产默认 UTC+8（时区进入请求指纹，录制前改动零成本；Codex 空闲，仅 write_stage_eval.rs 两处与 README 一句）。提交，连同已确认的 W1/W3 样本。
真实录制计划：61 次（W1 20 / W3 20 / W4 21），待用户批准。
- Q1 用户 2026-10-02 批准 61 次真实调用（DeepSeek-V4.1-Flash），录制 W1/W3/W4 单阶段评测。

## 2026-10-03 第一次真实评测（61 次，用户批准 Q1）
结果：61 次全部发出。24 项完成，37 项“response is not completed”（未录制）。完成项：W1 2/3、W3 9/11、W4 1/6。
- R1 37 项未完成大概率是 max_output_tokens=4096（ModelBudgetConfig 默认）被推理 token 耗尽：完成项最高 3163 输出 token、其中 3089 为推理。评测未记录 incomplete_details，待重录确认。生产默认同为 4096，属上线阻断问题。
- R2 已见质量问题：W1 专用关系 value 给整句（w1_01）；W3 丢不确定判 insufficient（w3_02）、漏提未发现（w3_11）；W4 漏新候选依据（w4_02、w4_23）、一个候选两个决定（w4_07）、该 pending 判 new（w4_16、w4_17）。均被代码核验或评分拦下，无错误写入。
- R3 用户批准：评测输出上限提到 16384 并记录截断原因，重录未完成项。max_output_tokens 不进请求指纹，已录 24 项不过期。
- R4 更正（Codex 指出，Claude 核实）：第一次完成 20 项（W1 3、W3 11、W4 6）、未完成 41 项；Claude 先前把“通过+失败”误报为 24/37。用户重新批准 41 次重录（上限 16384）。
- R5 待办：评测测试默认只出报告不判失败，显式门禁（如 EVAL_GATE=1）才因模型评分失败；否则离线全套永远为红。

## 2026-10-03 重录 41 项后的完整结果（61 项）
通过：W1 3/20、W3 15/20、W4 5/21（W4 样本仍为 draft）。仍未完成 3 项（w1_06、w1_07、w1_11），原因均为 incomplete_details.reason=length：w1_11 推理耗尽 16384；w1_06/w1_07 推理 3913/9061、其余为失控输出（已知的模型噪声）。R1 假设得到证实：4096 远不够，生产默认须调整。
主要失败模式：
- W1：专用关系的 value 给整句（4 例）；明确肯定句给 polarity=unknown（4 例）；条件放进 times（w1_03）；时间 kind 乱用（“上个月”给 date "2026-08"，“以前”给 range unknown，均被 W2 拒）；sentence 尾部带乱码/JSON 残片（3 例，W2 截断规则应处理——需确认评分是否按 W2 最终候选）；请求原话与目标原话混淆。
- W3：contradicted / insufficient 两向互换（w3_02、w3_10）；漏提未发现（w3_11）；w3_16 第二候选判 contradicted（Claude 修样本时把整句含“记住…”作为经历值，可能是标注问题）。
- W4：10 例 basis 缺新候选来源的依据（被 validate 拒）；该 pending 判 new（w4_16、w4_17）；一个候选两个决定（w4_07）；refine/state_change 判成 coexist（w4_03、w4_11）；w4_21 解决关系两次。

## 2026-10-03 失败分析（Codex 评审第 6 次，日志 .ai/log/codex-review-6.jsonl）
Codex 纠正 Claude 归因 4 处（W1 value 整句为 6 例；w3_11 已判 gaps 只缺片段；W4 缺新方依据为 9 例、w4_10 缺旧方；补出 w1_16 空时间 Quote、w3_06 问题 Quote 不连续、w4_18 原因应为 unclear_relation），并指出评分脚本问题（W1 应按 W2 最终候选评分；字段连带失败）。全部采纳。
- S1 提示词修改按 Codex 第 2 节逐条采纳；不得写入样本原文、ID 或标签（自动扫描测试保持）。
- S2 裁定：pending 决定在提示词中要求附新方依据（可追溯）；代码 validate 不收紧。
- S3 裁定：resolves_relations 按整组比对，分配到哪条决定不扣分。
- S4 评测脚本：默认只报告，EVAL_GATE=1 才因模型失败退出；W1 按 W2 最终候选评分并保留 raw；字段独立计分；指纹单测改为注入录制验证。
- S5 生产输出上限：新写入路径 W1 32768、W3/W4 16384（暂定，下轮校准）；不改旧路径共享默认值。提高上限不能修复失控输出。
- S6 w3_16 样本由 Claude 修正（两个完整句子，经历候选只取经历句，保留错误的双索引绑定）。
- 全量重录：61 次，待用户批准。
- T1 用户 2026-10-03 批准提示词第 2 版（54ef102）全量重录 61 次，输出上限 32768。

## 2026-10-03 提示词第 2 版真实评测（61 次，T1）
61 项全部完成（无截断）。通过：W1 4/20（v1 3）、W3 15/20（v1 15）、W4 12/21（v1 5，标签 draft）。
- W1：v1 的问题（专用关系整句值）多数消失，但出现新的失败：sentence 被写成值（w1_14、w1_15）、uncertainty 被填入非不确定内容（“打算”“我妈说”“none”、整句）、memory_type 多给 episode、value 含 \u0000、主体“我女儿”未绑定目录实体 e-daughter。另有若干可能属评分过严：条件片段多一个“我”、请求原话多一个句号、sentence 少逗号。
- W3：w3_03、w3_20 从 v1 的通过/未完成变成 supported 误判，单次运行噪声明显（temperature 0 仍不稳定，与历史 LongMemEval 观察一致）。
- W4：新方依据仍缺 6 例（提示词已明确要求）；scope_incomplete / unclear_relation 仍判 new/coexist（w4_16、w4_17）；w4_22 自行解决了不应解决的关系。

## 2026-10-03 第 2 版分析后决定（Codex 评审第 7 次，日志 .ai/log/codex-review-7.jsonl）
Codex 纠正：W4 缺新方依据为 5 项（02、05、13、16、23），w4_04 coexist 无 targets、要求旧方依据属评分过严；w3_03/w3_20 的 supported 误判是真实风险，不能只归因于噪声。
- U1 用户选择 (b)：冻结第 2 版提示词，对 W1/W4 做当前模型与更强模型的受控比较，每样本 2 次，共 164 次（需另行授权、待用户指定更强模型）。
- U2 用户同意标注裁定：w1_13、w1_17 接受 fact 或 episode；w1_19 接受 preference 或 constraint；“打算”属 plan 不属 uncertainty（Claude 已改样本）。
- U3 Claude 决定（不改契约）：W4 新方依据由运行时机械组装（候选 source + sentence，核对来源归属、未删除、片段存在），最终 W4Output 仍须含新方与逐目标旧方依据，校验不放宽；生产、缓存、回放同一规则；评分分别报告模型原始输出与最终决定。
- U4 Claude 决定：评分校准——原文连续且含义完整的等价引用（多一个主语“我”、末尾句号、末尾分句逗号）视为等价；目标指代缩短、限定语错误等仍算错；分别报告契约合规、语义正确、引用选择；coexist 无 targets 不要求旧方依据。旧录制统一重算。
- U5 用户指定更强模型：DeepSeek-V4-Pro 正式版，端点 ID `ep-20261003130756-trhj6`；base URL 与 API key 与现有 DeepSeek 配置相同，只换模型名称。评测标签用 `DeepSeek-V4-Pro`。
- U6 Codex 发现：新增 W4 原始审计副本保留模型原写法（如带空格引用），删除来源后按字面清洗会残留旧原文；Claude 授权修改 src/forget.rs，涉及来源的原始副本整体清除。
- U7 Codex 发现第二个删除窗口：W4 请求途中删除旧方来源，晚到响应的审计保存会把已删原文写回检查点；Claude 授权在 commit.rs 阶段保存事务内核对输入来源（不能只在提供者里提前检查，有竞态）。
- U8 验收：沙箱外 620 / 0 / 33；新方依据单边界组装后仍走严格 validate；两处删除窗口在写事务内修复。提交（含第 2 版录制）。
- V1 用户 2026-10-03 批准模型对比录制 164 次（Flash 与 DeepSeek-V4-Pro 各 82：W1+W4 × 2 次）。

## 2026-10-03 模型对比结果（V1，164 次）
| | Flash 通过次数 | Pro 通过次数 | Flash 两次全过样本 | Pro 两次全过样本 |
|---|---|---|---|---|
| W1（20×2） | 13/40（1 次调用失败） | 24/40 | 4 | 9 |
| W4（21×2，draft） | 26/42（2 次调用失败） | 36/42 | 10 | 17 |
错误类别（契约/引用/语义）：W1 Flash 7/15/25 → Pro 3/2/16；W4 Flash 12/12/5 → Pro 0/2/6。
用量：输出 token 均值 W1 Flash 10135（最高 32768 触顶）→ Pro 3263（最高 9017）；W4 7150（触顶 16384）→ 2427。Pro 输入 token 约为 Flash 的 2 倍（W1 3539 vs 1457），可能是分词或系统前缀差异，未核实。两边总耗时相近（约 61–63 分钟，并行）。单价未知，费用对比未做。
- V2 用户决定（2026-10-03）：新写入路径 W1/W4 用 DeepSeek-V4-Pro，W3 暂用 V4.1-Flash（按角色配置模型）。
- V3 用户裁定：“记住……”“只保留到明天”的请求对象 facts 或 source 均接受；w1_17“以前”的时间角色 validity 或 event 均接受（Claude 已改样本与检查器）。
- V4 验收：沙箱外 627 / 0 / 33；第 3 版提示词为通用规则、无样本文字。提交。
- V5 用户 2026-10-03 批准第 3 版提示词 Pro 验证 82 次（W1+W4 × 2）。第 2 版 Pro 录制已在 554ae73 存档。

## 2026-10-03 第 3 版 Pro 验证结果（V5，82 次）
| | Pro v2 | Pro v3 |
|---|---|---|
| W1 通过次数 / 两次全过 | 24/40 / 9 | **27/40 / 10** |
| W4 通过次数 / 两次全过（draft） | **36/42 / 17** | 33/42 / 16（1 次调用失败） |
- W1：强度乱填、avoids 双重否定已消失；剩余为 memory_type 边界、请求原话带冒号或过长（可能属评分过严）、w1_14 多出第三个候选、偶发多余请求。
- W4：第 3 版退步——w4_15 不再解决明确已解决的关系（“仅凭明确证据解决”矫枉过正），w4_20 从待定变成选目标或直接 retract；w4_16、w4_17 在 v2/v3 共 4 次运行中 Pro 全部判 decided（coexist/new），与 draft 标签一致地相反，需用户复核这两条标签。
- V6 用户决定（2026-10-03）：W4 提示词退回第 2 版，W1 保留第 3 版；w4_16、w4_17 标签保持 pending（Pro 判 coexist 算错）。
- V7 Claude 尝试直接恢复 W4 v2 文本并复用 554ae73 的 Pro W4 录制：离线回放显示 42 项全部过期（8b41bc5 改了请求构造/绑定），且一项现有测试依赖 v3 措辞失败。Claude 已撤回自己的全部改动（工作区回到 8b41bc5 + 67f9573），交 Codex 正式处理。不得为复用录制而伪造或绕过指纹。
- V8 Codex 查明过期原因：8b41bc5 把 `model` 放进了发给模型的 W4 请求内容（write_models.rs:398，payload 同时充当绑定）。Claude 判定这是缺陷：模型 ID 属运行配置，只应进入阶段绑定，不应出现在模型可见输入中；它也污染了 W4 v2/v3 对比。修正后发送内容与 554ae73 录制时逐字一致，旧录制可合法复用（Codex 已验证去掉该字段后 42/42 指纹吻合）。
- V9 验收：录制与 554ae73 逐字节一致；沙箱外 629 / 0 / 33；Pro 回放 W1 27/40、W4 36/42，零过期零调用。提交。

## 2026-10-03 召回侧方案（Codex 评审第 8 次，日志 .ai/log/codex-review-8.jsonl）
| # | 内容 | 结论 | 理由 |
|---|---|---|---|
| W1 | brief 写错：r4.v3 / recall.v3 用 RecordView，RecordView2 的 `end` 接入召回须升版。本轮保持 v3，只给 ended/end_unknown | 采纳（Claude 更正 brief） | 不改冻结契约；精确结束时间如需输出再请用户批准升版 |
| W2 | 不包装旧召回结果；复用同一读快照、全文检索、Quote 核验、W2 时间换算、模型预算、现有 elapsed 实现（openai.rs:1129） | 采纳 | 旧路径按句式/Active 过滤证据，无法满足限定语与新原文可见性 |
| W3 | 本项须含 current_message 固定带入、raw 顺序、update.pending、删除检查；并发/锁专项留第 4 项 | 采纳 | 否则召回本身不合约 |
| W4 | 聚合按可信计划范围枚举，裁剪前统计缺口；条件只作检索提示不字面过滤 | 采纳 | 契约 3.1/3.2 |
| W5 | 稳定 request_id、首次冻结 at，同 ID 不同参数拒绝；重放不自动再调模型 | 采纳 | 2.5 同参重放规则 |
| W6 | sum 无法证明遗漏部分非负时返回不足，不当下限 | 采纳 | 契约 3.3 |
| W7 | 版本选择覆盖 recall、prepare、validate_answer 与宿主说明，一次一个权威输出 | 采纳，⑤ 段 | 契约 3.4 末条 |
| W8 | MCP 在线接线持全局锁（pmr-mcp.rs:182），本项只做独立运行时、CLI 与 MCP 离线分发 | 采纳 | 同 J6 |
| W9 | R1/R4 样本各约 20，Claude 起草、用户抽查，分开发集与留出集；标签不从 LongMemEval 答案推导；63 题端到端共同评分集保留 | 采纳；Claude 在 ④ 前起草 | 防过拟合 |
| W10 | 分 5 段：① R1 纯核验 ② 离线证据闭环 ③ 注入 R4 与计算 ④ 提供者与预算重放 ⑤ 配置与分发 | 采纳 | 第一段不依赖样本与模型 |
R5 状态优先级、混合 grade、history/clarification 派生规则在 ③ 段 task 中由 Claude 裁定（在契约范围内）。

## 2026-10-03 召回 ① 验收
沙箱外全套 658 / 0 / 33（+29 R1）；rustfmt 干净；lib.rs 一行。提交。
Codex 列出的边界：单位与 op/type 对应表契约未规定（按代码白名单，未加组合限制）——认可；历史上下文无逐条时间锚点，换算用本轮 said_on——认可，R1 提示词与样本须明确“历史中的相对时间不作为查询时间依据”；代码不证明主体绑定与 current_evidence 的语义——认可（由样本评测衡量）。
- X1 用户 2026-10-03 抽查确认 R1 样本（20 个，dev 14 / holdout 6），含 r1_02、r1_10、r1_12、r1_13 的判断；label_status=reviewed。
- X2 用户裁定（2026-10-03）r4_04：未整理新原文明确表达变化时必须 supported，新值 current=yes、旧值 current=no，不得判 conflict。
- X3 用户裁定（2026-10-03）r4_18：精度不同的日期按各自区间比较，区间不重叠即可确定先后，重叠返回不足（契约 3.3“精确可比较”的解释；③ 段计算实现须遵循）。
- X4 用户 2026-10-03 确认 R4 样本（20 个，dev 14 / holdout 6），含 r4_03、r4_12、r4_14；label_status=reviewed。

## 2026-10-03 召回 ② 验收
沙箱外全套 674 / 0 / 33（+16）；rustfmt 干净。提交。
Codex 边界裁定：归属不明的 raw 扣留并 plan_complete=false（认可，防跨范围泄露）；旧 completed_rejected 无冻结映射，扣留并记缺口（认可，之后再评估 W3 全拒原文能否作证据）；enrichment_gaps 按缺口条目计（认可，与 3.2“gaps 汇入”一致）。

## 2026-10-03 召回 ③ 的 R5 规则（Claude 裁定，契约 3.3/3.4 范围内）
- Y1 status 优先级：needs_clarification（R1 歧义/未解析主体，或 R4 needs_clarification）> conflict（R4 conflict，或相关 open 关系未被本轮判断解决）> insufficient > answered；context_only 保持；R4 调用失败/输出无效 → degraded，原因写入 degraded_reasons，semantic_judgment=failed，facts=[]、computed=null。
- Y2 RecallFact.grade：引用的证据全部来自已提交记录时为 committed；任一来自 raw 或本轮消息即 uncommitted（保守标注，不改变 status）。update 按 3.4 合并：有 pending 合并去重原因；否则有未知或仅未整理原文时为 null；全部 judged 才为 judged。
- Y3 history：只来自 R4 判为 current=no、且记录因现实变化（state_changed / preference / constraint changed）结束的事实；被纠正、撤回的记录不进 history（只留审计）；until 取已知结束时间，未知为 null，不回填接收日。
- Y4 clarification：由代码按模板生成问题（列出候选实体名或未解析指代原话），evidence 为触发它的片段；不调用模型。
- Y5 conflicts：包内 open 关系 + R4 verdict=conflict 涉及的记录；不由代码新建关系。
- Y6 计算：count 按 event_group 去重（null 视为身份未知，只能给下限或不足）；sum 不能证明遗漏部分非负时不足；elapsed 复用现有实现（openai.rs:1129）补端点精度与 request_time；compare 按 X3 区间规则；模型自报 coverage 不能覆盖代码已知缺口。

## 2026-10-03 召回 ③ 交付后的发现
- Z1 Codex 风险项：r4_04 测试用广泛枚举构造证据包，不证明单事实词法检索能召回未整理新原文（“我住在哪？”与“我上周已经搬到南京了”无共同词）。Claude 判定这是 target-design 2.6 的可见性缺口：未整理 / 失败 / 待重判来源是后台尚未覆盖的部分、数量通常很少，应在可信范围内按接收顺序（最新优先进入预算）全部带入 raw，不依赖词法命中；超预算如实标截断。作为 ③ 的追加修复（不另开段）。
- Z2 其余边界认可：冲突涉及记录留在 records/facts，conflicts 只放已有 RelationLink；只来自 USER context 的指代澄清 evidence 留空；中文数量词未支持、anchored 时间计算不足——记入后续真实评测观察项。
- Z3 召回 ③ 验收：沙箱外 704 / 0 / 33（+30）；rustfmt 干净。提交。
- Z4 Z1 修复验收：沙箱外 708 / 0 / 33（+4）。提交。
- Z5 召回 ④ 验收：沙箱外 730 / 0 / 33（含 HTTP mock 端口测试）；ring 为既有间接依赖改直接依赖（SHA-256 请求绑定）；评分边界 facts:[] 视为无必需事实项（verdict 仍评分、额外事实人工对照 must_not），认可。提交。
- Z6 用户 2026-10-03 批准 R1/R4 真实评测 160 次：Flash 与 DeepSeek-V4-Pro 各 2 次（R1 20 + R4 20）。

## 2026-10-04 R1/R4 真实评测结果（Z6，160 次）
| | Flash 通过 | Pro 通过 |
|---|---|---|
| R1 dev / holdout | 4/28 / 4/12（8 次 length 截断） | 4/28 / 4/12 |
| R4 dev / holdout | 11/28 / 4/12（9 次调用失败） | 15/28 / 4/12 |
- R1 低分主要是评分口径：conditions 按严格相等评分，模型把查询主题词（“健身房”“买咖啡”）作为检索提示放入 conditions（3.1 规定 conditions 只是提示）；陈述句或“最近”给出了 time（标签期望 null）。需裁定评分口径后重算。
- 延迟（按录制顺序的写入间隔估算，含失败调用时间，偏高）：R1 中位 Flash 62s / Pro 68s，p90 87s / 133s；R4 中位 73s / 84s，p90 94s / 132s。target-design 2.5 前台 P95 目标 15s（含 R1+R4 全流程），差距一个数量级。推理强度当前固定 medium（openai.rs:69/551），输出 token 主要是推理（Flash R1 均 9.2k，Pro 4.4k）。
- Z7 用户决定（2026-10-04）：先试低推理强度。推理强度按角色可配置，离线完成后再请用户批准一轮真实评测，同时看质量与延迟。
- Z8 Claude 裁定 R1 评分口径：conditions 只是检索提示（3.1），评分改为“标签期望的条件须出现，额外提示不扣分”；标签 time 为 null 时，模型额外给出的时间不扣分（filter.time 只枚举超集、不丢证据，recall-runtime.md），标签非 null 时仍须匹配。用现有录制离线重算。
- Z9 用户 2026-10-04 批准 reasoning_effort=low 的 R1/R4 评测 160 次（Flash 与 Pro 各 2 次）。
- Z10 验收：沙箱外 737 / 0 / 33；推理强度进入请求与绑定，缺省请求体不变。提交。

## 2026-10-04 low 推理强度结果（Z9，160 次，实测耗时）
| | R1 通过（medium） | R4 通过（medium） | R1 耗时 p50/p90 | R4 耗时 p50/p90 | 平均输出 token R1/R4 |
|---|---|---|---|---|---|
| Flash low | 14/40（15） | 12/40（15） | 43s / 71s | 50s / 71s | 9.0k / 8.8k |
| Pro low | 21/40（24） | 19/40（19） | 35s / 64s | 40s / 96s | 2.2k / 2.9k |
- Pro 质量基本持平、Flash 略降；耗时仍远超 15s。Flash 在 low 下推理 token 几乎未降（7.4k），low 对它基本无效。
- Pro 约 63 token/s，延迟主要由推理 token 决定；若关闭推理（只剩可见输出几百 token），R1 预计个位数秒——需实测。
- Z11 用户 2026-10-04 批准 Pro 关闭推理评测 80 次（先 minimal，不被接受则不传推理参数）。

## 2026-10-04 minimal（关闭推理）结果（Z11，80 次）
| Pro | R1 通过 | R4 通过 | R1 耗时 p50/p90 | R4 耗时 p50/p90 |
|---|---|---|---|---|
| medium | 24/40 | 19/40 | ~68s / ~133s（估算） | ~84s / ~132s（估算） |
| low | 21/40 | 19/40 | 35s / 64s | 40s / 96s |
| **minimal** | **23/40**（holdout 11/12） | 14/40 | **2.5s / 3.5s** | **3.9s / 5.2s** |
- 首次探测 16.6s 为冷启动；关闭推理后延迟降一个数量级，R1+R4 合计约 7–9s，满足前台 P95 15s 目标。
- R1 关闭推理质量不降；R4 关闭推理后 16 次解码/核验失败（契约结构错误），是主要退步来源。
- Z12 用户 2026-10-04 同意：召回推荐配置 R1、R4 = DeepSeek-V4-Pro + reasoning_effort=minimal（新路径未默认启用，只写入推荐配置与文档）；针对 R4 minimal 的 16 次结构错误改第 2 版 R4 提示词，改完再批准 40 次重测；之后开始召回第 ⑤ 段。
- Z13 R4 提示词第 2 版验收：沙箱外 738 / 0 / 33；R1 录制仍有效。提交。遗留：整句 Quote 的日期精度核验盲区（代码缺陷，下一轮修）；r4_14、r4_17 评分边界待裁定。
- Z14 用户 2026-10-04 批准 R4 v2 重测 40 次（Pro + minimal）。

## 2026-10-04 R4 v2 重测结果（Z14，40 次）
R4（Pro + minimal）18/40（v1 14/40；medium 19/40），核验失败 13 次（v1 16），p50 4.5s / p90 6.3s。报告只给出 model_invalid_output，无法逐条归因。
- Z15 Claude 安排：先做 R4 收尾（修整句 Quote 日期精度核验盲区；报告写出具体核验失败规则；分析 13 次失败并论证运行时机械补入旧方依据是否符合契约 3.3），再做召回第 ⑤ 段。
- Z16 过程记录：Claude 在汇报计划后漏发了收尾任务，用户询问进展时才发现并补发。
- Z17 收尾验收：沙箱外 742 / 0 / 33；日期精度盲区已修；13 次失败 = 旧方依据缺失 8 + 操作数缺失 5。提交。
- Z18 Claude 决定（不改契约）：实现 R4 旧方依据机械补入——仅当 supersedes_record_ids 指向包内记录、其证据来源未删除、片段逐字存在时，补入该记录已核准证据；保留新方依据；补入后完整核验与快照检查照常；raw 与 final 都保留并分别评分。操作数不补（端点顺序是语义）。
- Z19 旧方依据补入验收：沙箱外 748 / 0 / 33；断言变更仅“只缺旧方依据”改为补全通过，缺新方依据仍降级。R4 final 26/40（dev 20/28、holdout 6/12，提升只在 dev，留意过拟合）。提交。
- Z20 召回 ⑤ 验收：沙箱外 761 / 0 / 33（含 CLI HTTP 端到端）；MCP 在线 recall_v3 明确报错（与 J6 一致）；宿主文档仅描述 opt-in。提交。召回侧离线部分（第 3 节第 3 项）完成。

## 2026-10-04 第 4 项方案（Codex 评审第 9 次，日志 .ai/log/codex-review-9.jsonl）
采纳：独立 SQLite 连接（WAL 已启用）+ Arc 共享召回缓存 + 单个后台 worker；分 5 段：① 可见性门控 ② 操作连接与共享缓存 ③ MCP 在线召回 ④ 后台执行与恢复 ⑤ inline 与完整收口。预计无需升库或改冻结契约。
- AA1 Claude 核实可见性缺口：recall.rs:295 原样透传模型 current；chat_recall.rs:486 机械批准不检查完整性。新原文被截断而模型仍判旧值 current=yes 时可能被批准，违反 target-design 2.6。① 段修复：门控针对“已知关键证据未覆盖”，不因任何截断或 queued/failed 标签一律拒绝。
- AA2 Claude 裁定 inline 含义（新路径）：MCP 接收原文后立即排入后台整理，不在前台同步等待（符合 target-design 2.5“W1–W5 默认后台”）；同步整理保留在显式 CLI。legacy inline 行为不变。文档明确记录。
- AA3 快照变化：①–④ 保留现有明确失败（recall_snapshot_changed），不隐式重试；⑤ 段用并发测试数据评估实际失败频率，若影响可用性再提方案，新增冻结失败码须用户批准。
- AA4 预算：前后台共享总授权，后台须显式配置联合预算，不自动加额度；示例配置写明前台 2 次、后台 3 次。
- AA5 授权：③/⑤ 解除 J6/W8 拒绝时，可更新原先断言“必须拒绝”的测试（改为新行为的断言），交付中逐处列出。
- AA6 ① 退回 1 处（修复第 1 轮）：门控触发时附加 degraded_reasons=budget_exhausted 会被宿主理解为模型预算用尽；证据被检索预算截断应只用 completeness.retrieval_truncated / plan_complete=false 与 status=insufficient 表达（contracts 3.2、第 4 节），不附 budget_exhausted。
- AA7 第 ① 段验收：沙箱外 770 / 0 / 33；未改现有断言。提交。
- AA8 第 ② 段验收：沙箱外 778 / 0 / 33；删除时清除该 owner 的全部召回缓存（粗粒度但安全）。提交。
- AA9 第 ③ 段验收：沙箱外 788 / 0 / 33（含 MCP 并发测试）；旧测试改动仅 AA5 授权的 W8 两处。已知局限：取消后已发出的 HTTP 不能即时断开，结果被丢弃但调用已计费。提交。
- AA10 ④ 段范围补充：write_pipeline_process.py 中“MCP deferred 保存后零调用”测试与后台 worker 的新行为冲突（有意变更），授权把入队准备改用 CLI --call、保留 CLI drain 原断言，另加 MCP 自动整理测试。
- AA11 第 ④ 段验收：沙箱外 803 / 0 / 33（含 MCP 后台 HTTP 闭环与最小并发检查）；旧测试改动与 AA10 一致。提交。

## 2026-10-04 第 ⑤ 段交付与快照失效问题
⑤ 交付：新路径 inline 改为 T1 后排入后台（AA2）；七类 2.6 场景无端口全部通过；旧断言仅改 J6 两处（AA5）。快照失效频率（docs/architecture/visibility-cadence.md）：串行合成 120 次召回 86 次失效（6 次伴随提交、80 次仅整理状态变化）；保存后立即召回两组 30/30 全失败，停顿 35ms 21/30、150ms 5/30。
- AB1 Claude 判断：当前“返回前快照完全一致才返回”比契约更严。contracts 3.2 / target-design 2.6 要求的是结果绑定读快照并标注版本、不伪称最新；返回前必须重查的是来源删除与授权。拟改：删除/授权严格（清洗或失败）；整理状态变化与后台提交不判失败，按读快照版本返回。需 Codex 只读论证后实施（Codex 交付中另提“R1 后重新装配证据”策略，一并比较）。
- AB2 快照策略（Codex 评审第 10 次，日志 .ai/log/codex-review-10.jsonl，Claude 采纳）：
  - 必须拒绝/丢弃：来源或记忆删除（含无来源 legacy）；已绑定正文、历史、时间锚点改变；归属、主体、任务范围不再允许；请求/执行身份不符；本轮或证据来源的控制限制变化（保守失效）。
  - 按读快照返回：整理状态推进、普通事实提交、关系或 update 推进、读快照后新增来源。内容一律按原快照呈现，不拼入最新状态。
  - 安全检查绑定 R3 证据快照（不再绑定 R1 前状态）；返回、重放、validate、MCP 发布/交付共用；覆盖“检查通过后、入缓存/交付前删除”的窗口。
  - prepare 顶层整理回执统一到 R3 观察点；宿主说明写明 queued = 原文已收到、整理待执行，enrichment 为读取时观察到的进度。
  - 写入侧 W5 快照、租约、顺序屏障不放宽。
  - 不升库、不新增冻结失败码、不改 contracts.md；在非冻结的 recall-runtime.md 写明“有效快照 = 绑定完整且内容仍获准可见，不等于最新”。
  - 授权调整断言“普通状态变化必须失败”的测试；删除相关断言一律保留。补真正的 memory_prepare 立即召回测试（原 60 次测的是 remember→recall），复跑 120 次节奏测试。
- AB3 第 ⑤ 段验收：沙箱外 819 / 0 / 33（含 MCP 七类场景）；旧断言仅 J6 两处。提交。快照策略修正按 AB2 接着实施。
- AB4 快照修正验收发现回归（修复第 1 轮）：沙箱外 mcp_stdio::mcp_global_context_fallback 失败——memory_remember 提交成功后快照被触发器写坏，期望 remember 成功且 global_context.status=unavailable，现返回 JSON-RPC -32603 "expected value at line 1 column 1"。f4a42b3 时该测试通过，回归来自本次未提交改动。该测试需端口，Codex 沙箱未覆盖。
- AB5 回归修复后沙箱外 829 / 1 / 33：mcp_recall_concurrency.py 的 change 分支仍断言新增来源导致 recall_snapshot_changed，与 AB2（新增来源按读快照返回）冲突；属授权范围内漏改的旧断言（Codex 沙箱无法运行该端口测试）。修复第 2 轮：只改 change 分支为成功且保持原快照（不含新来源内容），delete/cancel 分支断言不动。
- AB6 Claude 更正 AB5：R3 在 R1 之后装配，R1 阻塞时新增的来源属于读快照、应出现在结果；只有 R4 阻塞时新增的来源被排除；按本次来源 ID 判断而非全文。采纳 Codex 的分阶段断言（仍为修复第 2 轮，上轮未改代码）。
- AB7 change 分支修正后，沙箱外 829 / 1 / 33：cancel 分支取消后重放返回 recall_cached_snapshot_stale（期望 cancelled / not_retained）。此前被 change 分支的早期失败掩盖，非本轮新引入。修复两轮已用完，用户 2026-10-05 授权第 3 轮：先只读判断断言应放宽还是缓存核验过严，再最小修改。
- AB8 快照修正最终验收：沙箱外 --no-fail-fast 831 / 0 / 33。第 3 轮根因：R1 期间取消时尚无 R3 绑定，safety=None 被误记 invalidated；改为用 R1 前安全绑定核验，旧测试逐字不变。提交。第 4 项（可见性与最小并发）完成。

## 2026-10-05 第 5 项准备（用户同意先做离线准备）
- AC1 Claude 定评测配置：写入 W1/W4 = DeepSeek-V4-Pro（reasoning medium，与写入侧对比时的配置一致，后台不受前台延迟约束），W3 = V4.1-Flash（medium）；召回 R1/R4 = DeepSeek-V4-Pro + minimal。新路径 write_pipeline=w4_runtime_v1、recall_pipeline=recall_v3。评分用 common.v1 共同视图（tests/eval/scoring），与冻结的 0517000 旧基线对比。

## 2026-10-05 旧路径处理（用户决定）
- AD1 用户：旧路径未完整投入生产，不再为旧行为做兼容；第 5 项按 expect_common 给新路径打绝对分，旧基线 0517000 仅作参考、不作上线门槛；验收通过后切换默认到新路径并删除旧代码路径（第 6 项提前）。
- AD2 用户：本地有旧路径写入的真实记忆数据需要保留 → 保留旧数据读取兼容（LegacyRecordView，contracts 2.6/3.2）；删除的是旧写入/召回代码路径，不是旧数据。
- AD3 target-design 与 contracts 第 7 节中“验收→迁移→删除”的顺序与迁移退出条件需按 AD1 调整；属冻结文档，修改前交用户批准。
- 对后续工作的影响：不再要求“旧输出不变/旧断言不改”的约束（旧数据读取与删除保护除外）；沙箱外复跑仍保留（它捕获的是新路径问题）。
- AD4 第 5 项离线准备验收：沙箱外 836 / 0 / 33，Python 评分测试通过。提交。下一步：新评测按用例并行（35 个用例独立数据库），再请用户批准真实调用。
- AD5 用户 2026-10-05 批准第 5 项全量验收真实评测：最多 703 次调用，8 路并行（沙箱外全套通过并提交后开始）。
- AD6 并行改造验收：沙箱外 838 / 0 / 33，Python 通过。提交。开始全量验收真实评测。
- AD7 全量验收首次运行（2026-10-05）启动约 1–2 分钟后 panic：tests/eval/v3.rs:477 回环代理 accept 的连接在 macOS 上继承监听端口的非阻塞属性，read_line 返回 WouldBlock。Claude 立即终止运行，避免假失败计费与污染。未产生任何录制（录制按用例结束时写入）；在途的真实调用约 8–16 次可能已计费但结果丢失（估算，日志无逐次发送记录，无法核实）。修复：accept 后设为阻塞；录制改为每次调用完成即落盘、续跑复用；补回归测试。重跑前向用户说明并确认额度。
- AD8 更正 AD7（Codex 指出）：现有代码在每次响应完成后即写录制；首次运行无任何录制，说明中断前没有调用完成。最多约 8–16 个请求可能已发到端点但未完成，是否计费取决于远端，无法本地确认。
- AD9 用户 2026-10-05 批准修复后重跑全量验收，上限 703（首次可能已计费最多约 8–16 次，最坏合计约 719）。
- AD10 代理修复验收：沙箱外 843 / 0 / 33（含真实连接回归）。提交。启动全量验收重跑。

## 2026-10-05 全量验收首轮结果（AD9，552 次）
新路径 common.v1 绝对评分：通过 5 / 失败 31 / 待复核 27（63 题）；判分项自动通过 322/505。旧基线 0517000 同视图最终通过 54/63。大量失败集中在 status 项，疑似系统性问题（例如召回被统一降级），需根因分析后再决定。报告 .ai/log/e2e-report-run2.md，录制已提交。

## 2026-10-05 首轮根因（Codex 评审第 11 次，日志 .ai/log/codex-review-11.jsonl）
Codex 排除了“统一降级/整理未完成/空库”（Claude 先前的假设不成立）。主因：W3 失败 23（超限 14、结构 9）、W4 本地超时 4（远端 192–242s > 本地 180s）、R4 非法输出 10、R1 计划错误、冻结 2.4 的 fact 范围永远不完整（100 个已提交候选 update.pending）、27 题 124 项待人工复核、两处 no_evidence 状态分支与标签不一致、correction_s2 同目标重复结束导致整组回滚。
- AE1 用户决定：先做 A（零调用修正与复核）；C（修改冻结 2.4 的 fact 范围规则）A/B 之后再定。
- AE2 用户 2026-10-05 确认 Claude 起草的 124 个待复核结论（115 通过 / 9 失败），台账 status=confirmed、reviewer=xyzjiao。首轮重算：通过 32 / 失败 31 / 待复核 0。
- AE3 A 交付验收：Codex 为过 rustfmt 检查整文件格式化了 7 个范围外文件（违反 m1-status 第 6 节约定）。Claude 用“rustfmt(HEAD) == 当前”核验，src/model.rs、policy.rs、temporal.rs、temporal_grounding.rs、tests/w4_legacy_fixture.rs 为纯格式改动，已还原到 HEAD；diagnostics.rs、chat.rs 含真实改动，保留。
- AE4 A 验收：沙箱外 850 / 0 / 33，Python 通过；纯格式改动已还原。提交。
- AE5 用户 2026-10-05 批准写入侧关闭推理质量检查 61 次（Pro + minimal，W1 20 / W3 20 / W4 21，每题 1 次）。
- AE6 写入侧关闭推理结果（61 次）：W1 6/20（Pro medium 27/40≈13.5/20），W3 13/20（Flash medium 15/20），W4 14/21（Pro medium 36/42≈18/21）。平均输出 96–201 token（medium 4.7–6.9k），p50 2.4–3.5s。结论：W1 关闭推理明显退化；W3 基本持平；W4 有退化。
- AE7 用户 2026-10-05 决定：写入侧配置改为 W1/W4 = Pro medium，W3 = Pro + minimal；接下来先做 B 的提示词修改（零调用），之后再报全量重录 token 估算供批准。
- AE8 B 验收：沙箱外 854 / 0 / 33，Python 通过。提交。用户批准固定输入小集 33 次（W3 23 + R4 10，Pro minimal）。
- AE9 固定小集（33 次）：W3 23/23 通过（首轮全失败）；R4 1/10，剩余：operands 3、empty_evidence 2、record_basis/blank/qualifier_source/quote_or_source 各 1。B 的 R4 提示词修正在关闭推理下无效。
- AE10 用户 2026-10-05 决定：R4 在计算类问题（R1 plan.op 为 count / sum / elapsed / compare）使用 reasoning_effort=low，其余保持 minimal；全量重录等该方案落地后再跑。
- AE11 R4 分流验收发现：生产默认 R4 输出上限 4096（model_budget.rs:45），历史 low 有 8/40 次超过；开启计算类 low 后生产会截断。并入本轮修复：新召回路径 R4 在计算类 low 时输出上限 16384（与评测一致），minimal 保持现值；旧路径默认不变。
- AE12 用户批准固定小集计算题 8 次重测（R4 low）。沙箱外全套首跑编译失败：tests/eval.rs:953 缺新字段 r4_compute_output_tokens（Codex 只跑了相关测试），Claude 直接补 16_384（Codex 空闲，单行）。
- AE13 R4 分流验收：沙箱外 858 / 0 / 33。提交。开始固定小集 8 次重测。
- AE14 固定小集计算题 low 重测（8 次）：R4 1/10 → 5/10（计算题 8 条中 5 通过，2 条 record_basis 失败，1 条 180s 本地超时；非计算 2 条仍失败）。R4 low 延迟 p50 65s、p90 143s、最大 180s（超时），平均输出 4.1k token。延迟远超先前 40s 估算。
- AE15 用户 2026-10-05 决定：计算题 R4 保持 low（接受变慢），本地超时提高到 240 秒；先零调用补 record_basis 机械依据，再全量重录（开始前再报最终估算）。
- AE16 record_basis 补入验收：沙箱外 862 / 0 / 33。提交。用户 2026-10-05 批准全量重录（最多 703 次，输出约 160 万、上限 216 万 token）。
- AE17 AC2 全量重录：583 次、59 分钟、输入 207 万 / 输出 177 万 token；复核前 通过 8 / 失败 24 / 待复核 31（首轮复核前 5 / 31 / 27）。
- AE18 用户确认 AC2 的 106 项复核（102 通过 / 4 失败）。AC2 最终：通过 39 / 失败 24（首轮 32 / 31，旧基线参考 54）。
- AE19 用户 2026-10-05 反馈：每次全量真实评测约 50 元，太贵。之后避免全量重跑：优先离线回放；一次只改一侧以复用另一侧录制；先用阶段样本/固定小集验证；批准前给出 token 与按约 50 元/全量折算的金额估计。

## 2026-10-05 AC2 失败分析（Codex 评审第 12 次，日志 .ai/log/codex-review-12.jsonl）
24 题主因：a 冻结 2.4 范围规则 10、b R1 关系硬过滤 2、c R4 判断 2、d W3 拒收 2、e 评分口径 4、f 账户欠费（HTTP 403 AccountOverdueError，共 12 次）4。AC2 全量后段已欠费，f 类非模型错误。
- AF1 用户批准修改冻结契约 2.4：通用 fact 在授权主体、任务与读快照内实际穷举完成（含必要历史与关系闭包）且未超预算时，coverage 可判 complete；FTS 命中未截断不等于完整；超预算或覆盖未知仍 pending；已有 pending 须经显式重判，不批量改 judged。契约升版并记入第 8 节。
- AF2 用户裁定：无答案题（lme_0862e8bf_abs、lme_f4f1d8a4_abs）未编造答案时，needs_clarification 与 insufficient 都算对。
- AF3 用户裁定：带未证实前提的题（lme_af8d2e46、lme_b6019101）给出正确数字并如实标注保留（applies=unknown、status insufficient）也算对。
- AF4 之后真实调用前先请用户确认账户余额充足（避免欠费污染结果）。
- AF5 AF1–AF4 零调用修正验收：沙箱外 865 / 0 / 33；Claude 补 lme_b6019101 current 接受 [yes, unknown]（AF3）与评分器集合比较。提交。AC2 离线重算 40 / 20 / 3 待复核（8 项按 AF2/AF3 起草为 pass，待用户确认）。
- AF6 用户 2026-10-05 确认 8 项（AF2/AF3）；AC2 离线最终 43 / 20 / 0。用户确认余额充足，批准定点重录（约 6 元，工具完成后报精确数）。
- AF7 定点工具验收：沙箱外 866 / 0 / 33。提交。开始定点重录（上限 58，约 5.4 元，用户已批准）。
- AF8 定点重录完成：真实调用 48（预留 58），0 次调用失败。合并 63 题：通过 43 / 失败 6 / 待复核 14。14 题 35 项由 Claude 起草为 pass（proposed，待用户确认）；确认后 57 / 6。剩余失败：conflict_q2（conflicts 结构为空，冲突写进 fact）、conflict_q3（R4 输出核验失败 degraded；conflict_s2 记录停在 pending）、AC2 旧失败 4 题（lme_59524333、lme_7a87bd0c、lme_a2f3aa27、event_q5）。报告 .ai/log/af-targeted-merged-v2.md。
- AF9 用户 2026-10-05 确认 37 项（AF8 起草 35 项 + Ibotta 补充事实 2 项，起草时漏写台账）。合并 63 题最终 57 / 6 / 0。已提交。
- AF10 Codex 只读评审 13（会话 01a0fa9a…，日志 .ai/log/codex-review-13.*）：conflict_s2 因 W1 极性 unknown 不可投影而暂存（Claude 已核 run.json 回执 Unprojectable/Polarity=unknown），契约不支持单条 raw 拆成两个冲突成员；q3 为 R4 引用来源类型错误（quote_or_source）；event_q5 未在定点重测；3 道 LME 为 W3 过度拒收/R1 操作选择/R4 保守。建议：先裁定冲突报告是否等价，再做 ≤7 次阶段试验。待用户裁决。
- AF11 用户 2026-10-06 裁决：(1) conflict_q2/q3 接受“status=conflict + 有出处的冲突说明、不给唯一尺寸”为等价答案（改标注与评分器，零调用；不改冻结契约）；(2) 批准 ≤7 次阶段试验（R1/R4 提示词修复 + event_q5 补测），后续 ≤29 次端到端重建另行申请。
- AF12 实现 39 验收：diff 已审（R1/R4 提示词无样本原文；评分器只核来源绑定，语义走复核；write_stage_eval 仅评测工具接入）。沙箱外 868 / 0 / 33，Python 通过，干跑 7 次。开跑阶段试验（已批准 ≤7）。
- AF13 AF11 阶段试验（7 次真实调用）：conflict_q3 修好（引用 stored，conflict 说明）；event_q5 答 36 元；lme_7a87bd0c R4 给出 4 周事实但仍附 elapsed 计划（coverage incomplete），R1 仍选 elapsed；lme_a2f3aa27 R4 仍 insufficient（current=unknown/applies=no），R1 仍选 count；lme_59524333 R4 仍漏 6pm。修复第 1 轮：2 修好、1 部分、2 未修好。
- AF14 用户 2026-10-06 选择：3 道 LME 暂作已知局限不再改提示词；先重录 r1/r4 各 20 阶段样本查退步（≤40 次），无退步再做冲突题 + event_q5 端到端定点重建（干跑报精确数），之后转切换默认路径。
- AF15 退步检查（40 次真实调用，Pro minimal，日志 .ai/log/af14-stage-regression.log）：R4 raw 11/20（旧 v2 基线 9/20，r4_07、r4_10 由失败转通过，无样本由通过转失败），无退步。R1 10/20；r1_10、r1_13 由通过转失败（主体未解析），但旧 R1 阶段录制早在 f41fffa 已过期，无法归因于 AF11。AF11 的 R1 改动未修好目标题（两题仍选 elapsed/count）。
- AF16 Claude 决定并辅助编码（Codex 空闲，交接范围仅下述两处）：撤回 AF11 在 R1_PROMPT 中新增的 3 行，使 AC2 的 R1 端到端录制保持有效；保留 R4 改动。新 R1 阶段录制因指纹随之过期而丢弃（结果保留在日志）。targeted_plan.py prepare 增加 --questions 参数。定点目录 .ai/log/af14-targeted（conflict_q2、conflict_q3、event_q5）干跑：W4 2、R1 2、R4 3，共 7 次，约 0.49 元（略超此前口头估计的 4–6 次，需用户确认）。
- AF17 用户批准定点重建 7 次。提交 d6d440a（沙箱外 868 / 0 / 33，Python 通过）。
- AF18 定点重建完成：真实调用 6（批准 7）。conflict_q3：冲突说明，引用 conflict_s2，草拟通过；event_q5：36 元，草拟通过；conflict_q2：除冲突说明外又输出“屏幕是7英寸”（current=yes），违反禁止项，草拟失败（模型波动，上一轮无此 fact）。10 项 proposed 待用户确认；确认后 63 题为 59 / 4（剩 conflict_q2 + 3 道 LME 已知局限）。报告 .ai/log/af14-targeted-report.md。
- AF19 用户 2026-10-06 确认 10 项。63 题最终 59 / 4（conflict_q2 + 3 道 LME 已知局限）。提交。下一步：M1 收尾——切换默认路径、删除旧代码（保留旧数据读取），先由 Codex 只读评审方案。
- AF20 Codex 只读评审 14（日志 .ai/log/codex-review-14.*）：切默认 + 删旧执行代码，存储格式不变（DB v9，无需升库），旧数据经 LegacyRecordView 读取；5 步实施（保护读取→切默认→删旧写入→删旧召回→工具收口），零真实调用；核心删除约 1.5–1.8 万行。Claude 抽查 LegacyRecordView、memory_clarify、迁移代码属实。待用户裁决 4 项：冻结文档修改、memory_clarify、备份方式、缺角色配置处理。
- AF21 用户 2026-10-06 裁决：(1) 批准冻结文档修改（Codex 评审 14 第 4 节措辞）；(2) memory_clarify 停用提交，保留读取/删除；(3) 手工一致性备份（Claude 写步骤，实际升级另行授权）；(4) 角色配置缺省采用已验收的 AC2 配置（W1/W4 Pro medium、W3/R1/R4 Pro minimal、R4 计算 low），endpoint 仍由环境变量提供、不硬编码。
- AF22 用户 2026-10-06：之后运行 Codex 使用 fast 模式（命令显式加 -c service_tier="fast"）。
- AF23 第①②步验收（沙箱外）：876 通过 / 2 失败 / 33 忽略。失败：intake_grounding（检查脚本 bootstrap 未带 request_id，测试未同步）；write_pipeline_cli_preserves_the_source_less_order_barrier（期望 needs_rejudge，实得 failed）。交 Codex 修复第 1 轮。
- AF24 第①②步修复第 1 轮验收：沙箱外 880 / 0 / 33，Python 通过；顺序屏障为业务回归已修（复用 w4_check_order）；grounding 测试按新路径重写，断言未减弱。提交。用户同意 cargo clean（构建缓存 112 万文件 / 70 GB，测试进程启动空等 30–60 s）。
- AF25 cargo clean 后：编译 117 s，全套测试 328 s（清理前约 20–30 min），沙箱外 880 / 0 / 33。下发第 ③ 步（删除旧写入路径）给 Codex，fast 模式。
- AF26 第③步沙箱外：746 通过 / 1 失败 / 1 忽略（忽略数由 33 降为 1 系删除旧 live 测试所致）。失败 resident_eval_suite：默认仍进旧运行器，17 项评测失败。Claude 答复 Codex 的范围澄清：评测默认分发切到 v3 归入第③步修复第 1 轮（不等第⑤步），默认离线回放，零真实调用。
- AF27 第③步修复第 1 轮验收：沙箱外 746 / 0 / 1，Python 通过。Claude 裁定（评测设计，零费用）：默认严格回放如实报告缺口（58 过期 + 1 缺录制，不计通过），历史合并 59/4 单独核验；第⑤步须加自动化的 59/4 离线重算检查。完整 63 题当前回放需补录（约 50 元），留待用户另行决定。提交。
- AF28 提交更正：第③步提交误含默认回放产物 run-replay.*（36 个文件，测试运行写入已跟踪的 AC2 录制目录），已从提交中移除（本地未推送，amend）。第⑤步须让回放产物写到临时目录或加入 .gitignore。
- AF29 更正：第③步 amend 后提交号为 5659356（AF27/AF28 及给 Codex 的消息中写的 43846ce 为 amend 前的号）。第④步已下发 Codex（fast）。
- AF30 第④步交付：65 文件净删 26147 行；Codex 沙箱内 601 通过 / 20 端口 + 1 失败（严格回放比较历史录制配置时，含已退出的 4 个配置字段而整批拒绝）。Claude 批准并应用 Codex 准备的最小补丁（tests/eval/v3_core.rs 比较时忽略已退出且对现行路径无效的 4 个字段；请求、指纹、录制不变；附正反测试）。
- AF31 第④步验收：沙箱外 624 / 0 / 0，Python 通过；严格回放仍为 235 复用 / 59 缺口 / 0 发送，未新增过期；历史合并 59/4。提交 2ae6aab。
- AF32 第⑤步验收：沙箱外 626 / 0 / 0，Python 通过；59/4 自动核验经 Rust 入口默认运行并断言失败集合；录制目录无新增或改动。提交 42d021c（含此前未提交的 docs/architecture/README.md 用户改动，已并入新索引）。M1 收尾 5 步完成。
- AF33 用户 2026-10-06 选择先升级本地真实数据。Claude 盘点：生产入口为 LaunchAgent com.personal-memory.tunnel → tunnel-client → pmr-mcp-configured → ~/.local/bin/pmr-mcp（2026-09-23 旧版），DB 为 v3、今天仍有写入（14 条记忆、111 条原文、2 条暂存、44 实体）；runtime.json 含已退出的 local_recall_enabled，无 model_budget。演练：对一致性副本用新 release 二进制离线打开，v3→v9 迁移成功、integrity ok，记忆/原文/暂存/实体数量与值摘要一致，bootstrap 返回旧记录（LegacyRecordView）。生产未动。待用户授权停服升级与预算额度。
- AF34 用户 2026-10-06 决定：旧数据不要了，全部清空、不留备份（~/.local/share/personal-memory-runtime/ 下数据库、traces、九月迁移/部署目录、演练副本）；换新程序后重启 ChatGPT 记忆服务，model_budget max_calls=300。
- AF35 执行（用户授权 AF34）：停 LaunchAgent → 删除 ~/.local/share/personal-memory-runtime 下全部旧数据（约 247 MB，无备份）→ 旧程序改名 ~/.local/bin/pmr-mcp.pre-m1-da053bdf（仅程序，无数据）、安装新 release 二进制 → runtime.json 改为 inline + 预算 300 次（group local-user-m1-2026-10-06；旧文件留 runtime.json.pre-m1）→ ~/.codex/config.toml personal_memory.env 加 DEEPSEEK_PRO_MODEL=V4-Pro（旧文件留 config.toml.pre-m1）→ 离线冒烟：新库 v9、integrity ok、bootstrap 0 记录 0 调用 → 重启服务，tunnel 与 pmr-mcp 运行正常。
- AF36 生产首测（用户两段 ChatGPT 对话）轨迹核查：共 10 次模型调用（W1 3、W3 3、R1 2、R4 2），无失败；记忆提交 0 条。原因：(a) 关于孩子的句子主体“我娃/出生日期/泸沽湖那次”未登记为实体 → unresolved_subject，W4 不运行，来源永久 pending（memory_clarify 已停用，无补救入口）；(b) 行程/住宿安排 speech=plan → Unprojectable NotUserStatement，计划类信息按契约不能成为事实；(c) 第二段对话 ChatGPT 只调用了 bootstrap，没对问题调用 memory_recall；(d) 第一轮消息本身未单独保存（只出现在后续消息的 history 里）。W1 Pro medium 每条约 220 s、输出 9k–17k token。
- AF37 用户 2026-10-06：先让 Codex 评审 AF36 问题；停止服务（已 bootout LaunchAgent com.personal-memory.tunnel，ChatGPT 聊天不再进入该服务；plist 未改，重新 bootstrap 即恢复）。
- AF38 Codex 只读评审 15（日志 .ai/log/codex-review-15.*）：纠正——实体登记工具 memory_entity_upsert 仍在，缺的是“登记/确认主体/重建未提交候选”闭环；两次普通召回实际返回 answered/uncommitted（原话可召回）；W4 为 0 不是缺陷。补充遗漏：W1 丢失条件/不确定性而 W3 判 supported；R1 把孩子相关内容绑到 self；memory_list.pending 看不到 pending 来源；首次 prepare 带了 offline=true。方案：P0 宿主路由说明、来源状态+主体确认+重建入口、计划存储（需改冻结契约 2.1/2.3/2.5、6.4）；P1 限定语核验、漏首轮补录；P2 W1 low 对照（≤24 次）。MVP 端到端验收 ≤24 次。私有快照已删除。待用户裁决。
- AF39 用户 2026-10-06 裁决：(1) 批准计划类信息持久化为“计划”记忆（改冻结契约 2.1/2.3/2.5、6.4；具体措辞先给用户看）；(2) 实体采用明确登记（memory_entity_upsert）+ 必要时澄清一次，不做自动受限实体；(3) 两项都做：来源状态/主体确认/未提交候选重建入口 + 历史原话补录为独立来源（保留出处，时间不明不推算；如需升级 DB 版本先问用户）；(4) 批准真实调用：MVP 验收 ≤24 次（去标识样本）、W1 low 对照 ≤24 次；未批准用用户 3 条真实原话重整理（私有内容不再发送模型）。prepare 只读模式暂缓。
- AF40 Codex 技术评审 16（方案第 1 轮，日志 .ai/log/codex-review-16.md）：给出冻结契约逐条修改稿（计划记忆、预订区分、计划更新/取消、时间语义、主体确认/重建、历史补录、删除传播）；新增三工具 memory_source_status / memory_source_reprocess / memory_source_import_history，memory_list 增 pending_sources；建议 DB 9→10（来源出处、时间锚、主体确认、候选代际）；协议升版 W1/W3 v2、W4/R4/recall v4、R1 v1 不变；补录来源 said_on 可为 null；全文重提取首版仅限无已提交动作的来源；去标识 MVP（4 消息 + 2 查询 ≤24 次）与 W1 low 对照 6 场景（≤24 次）；5 步实施。待用户批准追加事项。
- AF41 用户 2026-10-06 批准：(1) 评审 16 第 1 节冻结契约修改稿；(2) DB 9→10；(3) 协议升版 W1/W3 v2、W4/R4/recall v4、R1 v1；补录来源 said_on 可空；全文重提取首版仅限无已提交动作的来源。方案讨论收敛（第 1 轮），进入实施，第 ① 步下发 Codex。
- AF42 M1.5 第①步交付：614 通过 / 20 端口 + 6 失败（tests/m1_host.py:49 工具数写死 8）。Claude 授权修复第 1 轮：仅把该断言改为 9，并新增 memory_source_status 名称与只读标记断言，其余不动。
- AF43 M1.5 第①步验收：沙箱外 633 / 0 / 0，Python 通过。提交 f4b1b66。
- AF44 M1.5 第②步交付：核心 180/0，Python 49/0、59/4 不变；提示词无例句/私有原文（Claude 已扫描）。全套初跑 600/49（20 端口、28 宿主版本接线、1 已修）。Claude 追加范围（我在 task 中漏列）：src/chat.rs、src/chat_recall.rs 及必要 chat_* 子模块、评测执行器 schema 分发/输入构造（tests/eval/v3.rs、v3_core.rs、fixed_inputs 适配、write_stage_eval.rs），作为修复第 1 轮。
- AF45 M1.5 第②步修复第 1 轮验收：沙箱外 643 / 0 / 0，Python 通过；契约改动与批准稿一致（Claude 抽查 2.1/2.3/2.4/3.3）。提交 66f52cf。遗留：评分适配器只接受 recall.v3，第⑤步补 v4。
- AF46 M1.5 第③步验收：沙箱外 657 / 0 / 0，Python 通过；13 项操作回归覆盖确认/混合来源/重提取限制/旧执行者/删除/enqueue 授权/v9 迁移。提交 2772d7f。
- AF47 M1.5 第④步交付：补录、删除传播、幂等完成；沙箱内 650 / 20 端口 / 0 业务失败。阻断：src/contract.rs 五处 stored 日期为 String（我在 task 中漏列），R4 提示词两处“接收日/接收年”需改为“可信原陈述时间”。Claude 授权修复第 1 轮：contract.rs 改为可空并接通 W2/W4/R4/R3；R4 提示词仅改这两处措辞（属 AF41 已批准的 nullable said_on 语义，不加例句），R4 契约版本保持 v4。
- AF48 M1.5 第④步修复第 1 轮验收：沙箱外 668 / 0 / 0，Python 通过；R4 提示词仅改两处措辞。提交 5108596。
- AF49 用户 2026-10-06：之后 Codex 不使用 fast 模式（撤回 AF22）。命令不再加 service_tier 参数；但 ~/.codex/config.toml 全局仍设 service_tier="fast"，是否修改全局配置待用户决定。当前进行中的第⑤步调用是以 fast 发出的，不中断。
- AF50 M1.5 第⑤步准备验收：沙箱外 676 / 0 / 0，Python 53/0；干跑 MVP 24 次、W1 对照 24 次，EVAL_RECORD=23 整批拒绝。样本全部虚构（Claude 审）；MVP 标注 draft 待用户确认；建议 latest_plan 来源改为 m3+m4。提交 9d6fee8。
- AF51 用户 2026-10-06 确认 MVP 标注（latest_plan 来源改为 m3+m4，label_status=reviewed；check_samples 放行 reviewed，Claude 辅助一行修改）；批准开跑 MVP 24 次 + W1 对照 24 次。删除 ~/.codex/config.toml 的 service_tier="fast"（备份 config.toml.pre-nofast-20261006）。
- AF52 MVP 实跑（21 次真实调用，预留 24）：写入侧 m1 提交；m2 生日更正 W3 insufficient 未提交；m3 2 条提交、“我们可能去南堤散步”主体未解析；m4 1 条提交、2 条主体未解析（省略主语）。召回：birthday 答 2020-04-14（依据 m1+m2，待复核）；arrangements 四条事实语义正确（计划/预订/条件/旧计划），但同时要求澄清“用户本人还是小栎”→ status 失败。W1 每条约 100 s。W1 low 对照（24 次）已开跑。
- AF53 W1 low/medium 对照（24 次）完成：6 场景两档提取结果基本一致（实体绑定、计划/预订拆分、条件/不确定性、否定），差异属同档内的随机波动（⑤ hypothetical/plan 两档均有摇摆；⑥ medium 第 2 次少一条“预约未改期”）。两档共有问题：value 常为整句、带孩子参加记为 self、③ 未登记亲属正确保持 unresolved。
- AF54 W1 对照统计：low 平均 50 s（p50 33，最大 138）、输出均值 3337 token；medium 平均 67 s（p50 47，最大 248）、输出均值 4512 token。low 约省 26% 输出、快 25%，质量未见差异。交 Codex 只读评审 17（不用 fast）。
- AF55 Codex 只读评审 17（日志 .ai/log/codex-review-17.md）：m2 为 W3 提示词误判（历史可解析指代被“证据只来自当前消息”压过）；m3“我们”按现行单主体契约保守扣留合理；m4 车票回指可由 history 绑定 self，但普通否定事实仍被 2.5 投影规则扣留；arrangements 澄清来自 R1 ambiguous 被运行时无条件继承（符合冻结 3.1，需改规则）；另有遗漏：旧计划 R4 仍标 current=yes、W4 把新行程日期当 effective。修复第 1 轮四项，合并重测上限 23（预留 24）。建议 W1 默认 low（先离线复核⑤⑥）。待用户裁决。
- AF56 用户 2026-10-06 裁决：(1) 批准“R1 初步澄清、R4 依据证据终审”，改冻结 3.1/3.3/3.5 并升召回协议（DB 不变；措辞先给用户看）；(2) “我们”与普通否定事实维持现状，靠原话召回；(3) W1 默认改 low（先离线复核对照⑤⑥；⑥标注改为计划主体 self、孩子为同行者）；(4) 批准修复第 1 轮后 MVP 重测 ≤24 次（干跑精确数在内即跑）。
- AF57 Claude 离线复核 W1 对照⑤⑥：⑤“如果雾散了，我可能去西岗拍鸟”low/medium 各 1 次误判 hypothetical（应为 plan），条件与不确定性两档均保留；“不打算租摩托艇”两档均为 plan+negate。⑥ medium 第 2 次漏提“预约尚未改期”，low 第 2 次把它标成 affirm（应 negate）；其余 5 条两档一致。结论：错误率相当，未见 medium 更稳，支持 W1 改 low。
- AF58 修复第 1 轮交付：W1 默认 low、W3/W1 历史回指、R4 终审澄清（R4/recall 升 v5，R1 v1，DB 10）、W4 effective 未知、旧计划入 history；沙箱内 659 / 21 端口 / 1 失败（src/bin/pmr-mcp.rs 仍期待 recall.v4，我漏列）。干跑：复用 R1 6 次，新增上限 17。Claude 追加授权 pmr-mcp.rs 与 recall-runtime.md 版本同步（同一轮内范围补漏）。契约措辞待用户确认后再提交与重测。
- AF59 沙箱外全套 680 / 1 失败：intake_grounding 假服务仍断言 W1 effort=medium（端口测试，Codex 沙箱内未跑到）。Claude 辅助一行修改为 low（Codex 空闲，范围仅此一行）。
- AF60 用户 2026-10-06 确认 R4 终审澄清契约措辞。修复第 1 轮验收：沙箱外 681 / 0（含 Claude 一行断言同步），Python 通过。提交 45fc15e。开跑 MVP 重测（新增上限 17，R1 复用 6）。
- AF61 MVP 重测（15 次真实调用，上限 17）：两题均无自动失败，转待复核。birthday：答“正确生日 2020-04-14”（current=yes）、“曾被记录为 2020-04-12”（current=no）。arrangements：answered，不再追问；当前计划 06-09 至 06-11、散步保留条件与“可能”、已订 06-08 车票、旧计划 current=no 并进入 history；但车票未提“尚未改签”，当前计划句未带目的地。写入侧：m3 三条全部提交；m2 生日更正仍 W3 insufficient；m4 两条仍 insufficient。
- AF62 用户 2026-10-07 确认 MVP 两题通过（16 项，行程题两处小缺陷记录）；写入侧 m2/m4 W3 insufficient 记为已知局限（原话召回可答）。决定先上线试用：升级本地库（先备份）、重开 ChatGPT 记忆服务。提交 d59ae74。
- AF63 上线（用户 AF62 授权）：一致性备份 ~/.local/share/personal-memory-backups/m15-20261007/（DB v9、integrity ok、旧二进制 pmr-mcp-af36、runtime.json）；副本离线演练 v9→v10 ok、bootstrap recall.v5；安装新 release（d59ae74）、enable + bootstrap LaunchAgent；生产库已为 v10，tunnel 与 pmr-mcp 运行。3 条旧 pending 私有来源未用模型重整理（未授权）。未验证：既有预算组策略与新版是否兼容（首次真实调用时核查）。待用户在 ChatGPT 更新自定义指令。
- AF64 Claude 辅助：docs/chatgpt-custom-instructions.md（被 .gitignore 忽略）中 recall.v4 → recall.v5（修复轮漏同步）。
- AF65 用户 2026-10-07：清理之前的记忆以便重新触发。已停服务，删除生产库（含 WAL/SHM）、traces，以及备份目录中的 DB 副本（保留旧二进制与旧 runtime.json，不含数据）；空库冒烟 bootstrap recall.v5 0 记录；服务已重启。注意：预算账本随库删除，新库中 300 次额度重新计数。
- AF66 第二次生产试用轨迹（20 次模型调用，全部 HTTP 成功）：宿主行为明显改善——首轮单独保存、主动登记 child 实体、问题时调用 recall。问题：(1) 首轮 8 个候选全部被 W2 以 fact_sentence_mismatch 拒收，最关键的出生/机票/停车/路线/行程未入结构化记忆；(2) 召回“我十月份有什么安排？”R4 输出内容完整正确（行程、机票、停车、路线、日期），但机械核验 model_invalid_output 整体丢弃 → degraded、facts=[]；(3) 第 4 轮“是不是更好…可行吗？”被当作 plan 提交，并投影出 self visited 兰州/西宁（Memory 投影读作已去过）；(4) 第 3 轮同一句提交 3 条重复记忆；(5) 首轮 prepare 的 R4 输出 8414 token、耗时 145 s；status=conflict（出生 2025 与“三岁多”矛盾，判断合理）。
- AF67 更正 AF66(2)：降级召回并非空结果——R4 判断被丢弃（semantic_judgment=failed、facts=0），但响应仍带 3 条原话 raw 与 1 条已提交记录；ChatGPT 据此自行整理出正确的十月安排。问题性质是“系统没给出经核验的结论，靠宿主读原文兜底”，而非用户拿不到信息。
- AF68 用户 2026-10-07 定流程：Claude 先分析 → Codex 复核分析 → Claude 定修复方案 → Codex 复核方案 → Claude 出最终方案 → Codex 实现 → Claude 复核实现。Claude 不在仓库里自行写诊断代码。
- AF69 Claude 完成问题分析（.ai/analysis-af69.md）：A 首轮 W2 拒收（已确认，value 为 sentence 子串）；B R4 整体丢弃（具体规则待 Codex 复核确认）；C 征询被当计划且用 visited；D 重复记忆；E 首轮慢。交 Codex 复核分析（只读，可在 /tmp 做临时诊断，不改仓库）。
- AF70 Codex 评审 18 结论：B 确认为 facts[4].times[0] fact_time/quote_or_source（时间片段出自未被该事实引用的来源）；A 现行提示词本就要求共用整句，W1 违反，分句需改契约；C 计划判断需保留合法试探性意向，visited 禁令属新增政策；D 记忆 5/6 非结构重复；E 8414 为 usage 不等于文本长度。Claude 据此出修复方案草案 .ai/plan-af70.md，交 Codex 复核方案。
- AF71 Claude 最终方案（采纳 Codex 评审 19）：暂缓 A.2 标点修复（属契约新增例外）；删除 E（轨迹已有 reasoning_tokens，首轮 R4 实为 low，8414 中 7297 为推理）；C 收窄为“仅评估备选、未表达本人实际意向”才归 question/hypothetical。不改契约、不升版、DB 不变。下发 Codex 实现。
- AF72 AF71 实现验收：提示词无例句/私有原文（Claude 审 diff），诊断白名单字段；沙箱外 690 / 0 / 0，Python 通过。提交 4e5c3a8。待部署与真实验证（需用户批准）。
- AF73 用户 2026-10-07 批准部署 4e5c3a8：旧二进制备份为 m15-20261007/pmr-mcp-d59ae74，服务已重启；DB v10 integrity ok。用户不重整理首条私有来源，将自行清库重发。
- AF74 用户 2026-10-07：清库。停服务，删除生产库（含 WAL/SHM）、traces，以及 job 临时目录中提取的私有 R4 输入/输出；重启服务。预算账本随库清零。
- AF75 第三次生产试用：十月安排召回 answered（R4 时间引用修复生效）；首轮 W2 7/9 ok（整句规则生效）。新问题：P1 首轮来源最终 needs_rejudge 未提交（原因待确认）；P2 prepare 中 R1 对陈述给 query_only、R4 用 current 引用或删 “**” → quote_or_source 整批丢弃（3 次）；P3 ChatGPT 用陈述全文作 recall query；P4 W1 长消息 176 s。Claude 分析见 .ai/analysis-af75.md，交 Codex 复核（评审 20）。
- AF76 Codex 评审 20：P1 实为实体登记推进 source_less_barrier（sqlite.rs:552、commit.rs:2616），处理中的 u1 被判 stale → needs_rejudge；保守规则预期行为但对纯新增实体过宽。P2 三次 quote_or_source 均复现（u1 prepare 删 **；u1 重复 recall 同时 current+删 **；u2 prepare current）。Claude 方案草案 .ai/plan-af76.md 交 Codex 复核（评审 21）。
- AF77 Codex 评审 21：方案可采纳。P1.2 属提交失效政策调整，需用户批准并在冻结 2.5/target-design 2.5 明示例外（按 W5 无独立版本号先例记录，不升 DB/模型/recall 协议）；豁免条件需收紧（旧实体逐字段不变、新 ID、名称/别名无冲突、同事务无其他业务变化；只豁免 source_less 屏障推进，revision 仍增）。P2.3 属执行既有禁令，无需升版。R1/R4/宿主措辞与零调用验收清单已补充。待用户批准 P1.2。
- AF78 用户 2026-10-07 批准 P1.2 纯新增实体不推进 source_less 屏障（冻结 2.5 / target-design 2.5 写明例外，按 W5 先例记录，不升版；措辞提交前给用户看）；保留“先登记再保存”宿主指令。最终方案按 Codex 评审 21 范围下发。
- AF79 用户 2026-10-07 确认契约措辞；沙箱外 699 / 0 / 0。提交 26ef7d3 并部署（旧二进制备份 pmr-mcp-4e5c3a8），DB v10 integrity ok。
- AF80 用户 2026-10-07：清库（停服务，删生产库含 WAL/SHM 与 traces，重启；预算账本清零）。
- AF81 第四次生产试用：先登记再保存生效；首轮 8 条记忆已提交；十月安排 answered（7 条事实）。新问题：Q1 t2 W1 因 subject.unresolved.text 为空被整份拒收（且宿主未带 history）；Q2 t1 prepare R4 把“2025-08-25生”的日期填为当天 → fact_time；Q3 行程类主体被误判为 unresolved 事物短语。Claude 分析 .ai/analysis-af81.md 交 Codex 复核（评审 22）。
- AF82 Codex 评审 22：Q1 确认（w2::decode 递归非空拒收 unresolved.text=""），遗漏：生产 schema 缺契约要求的非空 pattern（实现缺口）；逐候选拒收需改冻结 2.2 且本例救不回。r1_not_understood=R1 降级计划（conditions quote 删 Markdown * + time.quote 抄 said_on 元数据），轨迹误记 completed。Q2 确认；Q3 收窄（t3 缺 t1 前文不能默认 self）。t3 W4 正常完成，3 条提交，库共 11 条。Claude 方案草案 .ai/plan-af82.md 交 Codex 复核（评审 23）。
- AF83 Claude 最终方案（采纳 Codex 评审 23 全部收窄）：非空 pattern 在共享 string() 一次修好（恢复冻结约束，不升版）；W1/R1 白名单诊断不改业务行为；提示词限定措辞；宿主 history 带必要获准前文。下发 Codex 实现。
- AF84 AF83 实现验收：提示词扫描 0 命中；沙箱外 705 / 0 / 0，Python 通过。提交 d92374d。
- AF85 用户 2026-10-07 批准部署并清库：d92374d 已部署（旧二进制备份 pmr-mcp-26ef7d3），生产库与 traces 已清空，服务重启。
- AF86 第五次生产试用：W1/W2 全部候选通过、宿主行为正确；turn-002 两条与 turn-003 一条提交；十月安排 answered。新问题：turn-001 的 8 个候选全被 W3 拒（time contradicted/insufficient），turn-003 coverage_gaps，turn-002 prepare R1 conditions quote 降级。分析 .ai/analysis-af86.md 交 Codex 复核（评审 24）。
- AF87 Codex 评审 24：契约 1.2 未规定缺年份取 said_on 当年（W2 只核月日，任一年都能过）；能由明确上下文定年用 date/range，否则 unknown；times=[] 计划合法，W3 不应要求全部计划有日期，但应核验该候选适用的时间；[3] 把往返两段航班合并为连续 range 是错误；生日矛盾宜判 insufficient 而非 contradicted；R1 conditions[9] 系删 history 中的 **；turn-003 history 未带 turn-001。缺“无年份日期”的产品规则，待用户裁决。
- AF88 用户 2026-10-07 裁决：无年份日期按说话时间推最近一次（计划/安排往后、经历往前；方向不明或可能跨年记 unknown；只由模型判断，代码不补年）。Claude 方案草案 .ai/plan-af88.md 交 Codex 复核（评审 25）。
- AF89 Codex 评审 25：方案可采纳，收紧边界（按事件方向而非 speech；明确年份/上下文优先；区间整体判断；R4 用来源 said_on、R1 用请求时钟；W3 内部矛盾只作用于相关候选）。语义政策改变须升版：W1 v3、W3 v3、R1 v2、R4 v6、recall v6；W4 v4 不变、DB 10 不变。给出契约 1.2/2.3 与 target-design 2.3 措辞，需用户确认。
- AF90 用户 2026-10-07 确认契约 1.2/2.3、target-design 2.3 措辞与升版（W1 v3、W3 v3、R1 v2、R4 v6、recall v6；W4 v4、DB 10 不变）。最终方案下发 Codex 实现。
- AF91 AF90 实现验收：契约措辞与用户确认一致，提示词扫描 0 命中；沙箱外 710 / 0 / 0，Python 通过。提交 91a068a。遗留：架构索引仍写 DB 9 与早期契约号（文档滞后）。
- AF92 用户 2026-10-07 批准部署并清库：91a068a 已部署（旧二进制备份 pmr-mcp-d92374d），生产库与 traces 已清空，服务重启。
- AF93 第六次生产试用（91a068a）：首条消息 8 个候选全部 W3 supported 并提交（往返航班分开、10.10-10.30 定为 2026）；住宿安排提交；十月安排 answered（8 条，含 2026 年日期）；保存时的召回均 answered，无降级。剩余：生日更正（turn-002）W3 判 subject insufficient 未提交，结构化记忆仍为 2025-08-25，召回靠原话给出 2023-08-25。
- AF94 用户选择继续修更正类语句。Claude 分析 .ai/analysis-af94.md（W3 有 history 仍判 subject insufficient；假设 H1 模型波动/H2 历史内部矛盾干扰/H3 规则措辞冲突；方向 A 提示词 / B W3 改 low / C 不改）交 Codex 复核（评审 26）。
- AF95 Codex 评审 26：不能区分 H1/H2/H3；推荐先 A（W3 提示词收窄澄清，澄清既有契约 1.4 不升版），保持 minimal，验证后再考虑 B；另记首轮出生/年龄矛盾 W3 漏检。Claude 方案草案 .ai/plan-af95.md。
- AF96 用户 2026-10-07 批准方案 A 与 16 次 W3 验证（4 虚构场景 × 旧/新提示词 × 2）。方案草案交 Codex 复核（评审 27）。
- AF97 Claude 最终方案（采纳 Codex 评审 27）：W3 提示词按收紧措辞澄清（不升版）；新增 EVAL_M15=w3 对照模式（旧提示词快照取自 91a068a、16 槽位独立累计额度、完整指纹、配对交错）；不改生产缓存失效政策、不重建来源、不切 low。下发 Codex 实现。
- AF98 AF97 实现验收：W3 提示词差异限于主体/证据边界，样本虚构（Claude 审，标 reviewed）；沙箱外 716 / 0 / 0；干跑 16。提交 dc604f8。开跑 16 次 W3 对照（AF96 已批准）。
- AF99 W3 对照（16 次真实调用，全部成功，p50 约 2.7 s、输出约 110 token/次）：新旧提示词结果完全相同、两次重复也一致。01（明确先行语）两臂均 insufficient[value]；02（历史矛盾但主体明确）均 insufficient[subject]；03（多主体）均符合预期；04（无先行语，应 insufficient）均误判 supported。结论：方案 A 在 minimal 下无效；01 的 value 问题需核查样本候选；04 为错误放行。
- AF100 用户选择核查样本后试 low。Claude 分析+方案 .ai/analysis-af100.md（样本“应为63”含情态可能导致 value 不足；改“是63”；对照改 W3 minimal vs low 16 次）交 Codex 复核（评审 28）。
- AF101 Codex 评审 28：结果确认、归因仍为假设；04 期望 insufficient[subject] 合理；新增独立 EVAL_M15=w3-effort 模式（新样本、固定当前提示词，两臂仅 effort 不同，完整指纹显式接入，新账本 16 次，用户 AF100 选择已授权）。下发 Codex 实现。
- AF102 AF101 工具验收：样本措辞改“是63”、标签 reviewed；沙箱外 721 / 0 / 0；干跑 16。提交 13468fc。开跑 W3 minimal vs low 16 次。
- AF103 W3 强度对照（16 次真实调用，全部成功）：minimal 2/8（仅多主体场景通过；01 value、02 subject 误拒，04 误放行）；low 6/8（01×2、02×1、03×2、04×1 通过；两次失败均为 aspect=time，系“刚才”被当作缺失时间，其中 04 判定本身正确）。代价：low 每次 20–131 s（中位约 57 s）、输出 1.2k–7.6k token（几乎全为推理），minimal 约 3 s、约 110 token。
- AF104 用户 2026-10-07 决定 W3 默认改为 low。Claude 方案：仅改写入角色缺省 W3 effort minimal→low（chat_config 缺省、帮助文字、README/运行时/宿主文档）；显式配置仍生效；不改契约/协议/DB；阶段超时 360 s 足够（实测最长 131 s）；预算授权组策略须核对是否因 effort 变化而不兼容。交 Codex 复核（评审 29）。
- AF105 Codex 评审 29：方案可采纳，不升版；policy_json 不含 effort，预算组兼容；超时 360 s 足够；范围见评审。Claude 最终方案采纳，下发实现。
- AF106 W3 low 默认：沙箱外 723 通过 / 1 失败（intake_grounding 端口测试仍断言 W3=minimal）。Claude 误在失败时提交 3f83a25（链式命令未按失败中止），待 Codex 修复后 amend，Python 通过。提交 3f83a25。
- AF107 Codex 同步 intake_grounding 断言 W3=low；沙箱外 724 / 0 / 0。amend 后提交 ff164c9（替代 3f83a25，未推送）。
- AF108 用户 2026-10-07 批准部署并清库：ff164c9 已部署（旧二进制备份 pmr-mcp-91a068a），生产库与 traces 清空，服务重启。
- AF109 第七次生产试用（ff164c9，W3 low）：生日更正 W3 supported 并提交（child_wawa 2023-08-25）；首条 2025 生日因内部矛盾判 insufficient 未提交（符合 2.3 新口径，结构化中不再有错误生日）；首条 7 个候选中 4 条提交（计划、机票、停车、路线），3 条 W3 insufficient[time]（“目前计划都订好了”“从阜阳开车去合肥机场”及一条），W1 未给子计划附上适用时间；turn-002 prepare R4 fact_time/quote_or_source 降级；十月安排 answered（4 条）；W3 low 首条 157 s/9670 token；turn-003 仍在整理。
- AF110 用户 2026-10-07：M1.5 先告一段落，正常使用。剩余记为已知局限：W1 未给子计划附适用时间致 W3 拒收；R4 时间引用来源偶发错误；W3 low 长消息约 2–3 分钟。收尾：文档版本号同步、M1.5 总结；推送/PR 先问用户。
- AF111 文档同步与已知局限（Codex），Claude 审阅后提交 0bf5ffb。
- AF112 用户 2026-10-07 批准推送并开 PR：m1-eval-fixes 推送至 0bf5ffb（快进，原远端 57404ad）；新建 PR #4（m1-eval-fixes → main），PR #2/#3 已于 2026-09-29 合并，未改动其描述。
- AF113 用户 2026-10-07：PR #4 已合并；批准重建评测基线，用历史上效果最好的配置跑全量、不计费用；后续要做更强模型（gpt-6.1-sol、Opus 5.5）与本地模型（Qwen3.5-9B-Q8、Qwen3.8-27B-Q4）对比。Claude 方案 .ai/plan-af113.md 交 Codex 复核（评审 30）。
- AF114 用户 2026-10-07 同意顺序：① 多模型接入（先脚本探测 OpenAI/Anthropic 可用性）② 写入侧分阶段多模型对比 ③ 混合检索 ④ 召回侧分阶段对比 ⑤ 精选组合端到端全量。目标：本地 27B/9B 在哪些 case 够用、DS-V4-Pro/Flash 补多少长尾、哪些难例只有 GPT-6.1 Sol / Opus 5.5 更可靠。凭证在 ~/.zshrc（DEEPSEEK/OPENAI/ANTHROPIC/QWEN 的 KEY 与 BASE_URL）。
- AF115 探测：OpenAI 代理需 UA，Responses+json_schema 可用，必须传 instructions（否则被注入约 4k 的 Codex 系统提示）；Anthropic 代理不支持 forced tool_choice，output_config.format=json_schema 与 effort 可用；本地 Qwen 27B json_schema 可用，9B 返回空内容待诊断。Claude 多模型接入方案 .ai/plan-af115.md 交 Codex 复核（评审 30）。
- AF116 用户告知本地模型名 qwen3.5:9b-q8_0、qwen3.8:27b-q4_K_M，Ollama context length 64k；已补入 plan-af115（按模型配置上下文上限、发送前机械检查、不截断）。
- AF117 本地实测：27B@64K 加载 20 GB、100% GPU（加载 9 s，极小请求 19 s）；9B@128K 14 GB、100% GPU，但极小请求思考 1133 token、耗时 110 s——此前空内容即思考占满 300 输出上限。测试时系统 swap 已用 3–8 GB，内存偏紧。
- AF118 Codex 评审 30：方向可采纳。抽象放在现有 openai.rs 客户端内部（三协议分支共用发送/预算/错误映射）；chat.rs 须按角色解析 provider 凭据；Anthropic 不支持整数 minimum/maximum，只在发送副本中移除；不能宣称三协议 strict 等价，返回后统一走现有核验；强度与思考开关分开配置，不支持即发送前拒绝；上下文预检用 token 保守上界而非字符估算，无法证明安全则拒绝；指纹绑定协议/endpoint/模型（含版本摘要）/发送 schema/强度/思考/上限/适配器版本；provider 身份不进 policy_json；评测额度需累计持久化。待用户确认各模型强度/思考配置与上下文拒绝政策。
- AF119 用户 2026-10-07 裁决：Qwen 思考开/关都测；GPT-6.1 Sol 与 Opus 5.5 统一 medium；上下文无法证明安全则拒绝发送并单独统计（不截断）。最终方案采纳 Codex 评审 30 全部建议，下发实现（分支 multi-model）。
- AF120 多模型接入交付：沙箱外 738 / 0 / 0，Python 59/4。Qwen 需部署证明（模型摘要、tokenizer、模板上界）方可发送。Claude 本地实测：两模型 tokenizer.ggml.model=gpt2（字节级 BPE），摘要 27B 25b843619e944cd0、9B a8ee95bc3592ebaa；模板开销实测 1 条消息 13 token、2 条 19 token，json_schema 不注入提示词。
- AF121 多模型接入验收：Claude 审阅 diff；沙箱外 738 / 0 / 0。提交 92d0e7e（分支 multi-model）。
- AF122 Claude 方案 .ai/plan-af122.md（provider 档案 + 8 次冒烟）交 Codex 复核（评审 31）。
- AF123 Claude 补齐 Qwen 证明证据并创建 Ollama 别名 qwen3.8:27b-q4_K_M-ctx64k（num_ctx 65536，100% GPU 20 GB）；/v1 默认 131072 会让 27B 部分落到 CPU。最终方案采纳 Codex 评审 31，下发实现。
- AF124 冒烟入口交付：干跑 8 档中 27B 别名被能力表拒绝（src/model_provider.rs:108 只接纳原模型名，我漏列文件）。Claude 授权修复第 1 轮：能力表接纳 qwen3.8:27b-q4_K_M-ctx64k 并补回归。
- AF125 冒烟工具验收：沙箱外 742 / 0 / 0；干跑 8 待录制 0 拒绝。提交 ccd0b7c。待用户批准 8 次冒烟。
- AF126 用户 2026-10-07 批准 8 次冒烟，开跑。
- AF127 冒烟结果（8 次发送）：ds-pro 通过（27 s，1630 输出）；ds-flash 输出 8192 全为推理被截断（length）；gpt-6.1-sol 通过（8.6 s，190 输出）；opus-5.5 发送失败——URL 拼成 {base}/messages，代理需 /v1/messages（适配器路径问题）；qwen-27b-think、qwen-9b-think 均 360 s 超时；qwen-27b-nothink 通过（89 s）；qwen-9b-nothink 通路正常但语义失败（多出 1 个 memory_request）。
- AF128 用户 2026-10-07：输出上限对齐生产（W1 32768、W3/W4 16384），云端超时 360 s，本地开思考超时 1800 s；修 Opus 路径后只重跑失败的 4 个配置（Flash、Opus、27B/9B 开思考），4 次调用。作为冒烟工具修复第 1 轮下发 Codex。
- AF129 冒烟修复验收：沙箱外全套通过；提交 6633650。开跑 4 档重跑。
- AF130 用户 2026-10-07：Opus 修复后单独 1 次确认调用可直接跑，无需再问。
- AF131 Opus 400 原因：Anthropic 约束解码报“compiled grammar is too large”，W1 schema 去掉 pattern 仍超限。Claude 方案 .ai/analysis-af131.md（Anthropic 改为提示词内 schema + 本地严格核验，绑定标注；错误正文脱敏落盘）交 Codex 复核（评审 32）。
- AF132 用户 2026-10-07 批准例外：仅评测 Opus 档案使用 prompt_schema（无约束解码 + 本地完整核验，不修补不重试），生产默认不变，报告标注。最终方案采纳 Codex 评审 32，下发实现。
- AF133 冒烟重跑 4 档：ds-flash 通过（47 s，8229 输出）；opus-5.5 400（grammar 超限，修复中）；qwen-27b-think 通过（603 s，1511 输出）；qwen-9b-think 1800 s 超时（开思考仍未完成）。
- AF134 用户 2026-10-07 同意：后续对比中 9B 只测关思考，开思考记为“不可用”（W1 单条 30 分钟仍未完成）。
- AF135 Opus prompt_schema 验收：沙箱外全套通过；提交 1f91ec3。开跑 Opus 1 次确认（AF130 已授权）。
- AF136 Opus prompt_schema 确认 1 次：通过（5 s，161 输出，核验通过）。7 个对比配置通路全部可用。
- AF137 Claude 第②步方案草案 .ai/plan-af137.md（7 档 × W1 20 + W3 20；云端/本地关思考各 2 次、27B 开思考 1 次；共 520 次；W4 草稿暂不纳入）交 Codex 复核（评审 33）。
- AF138 Codex 评审 33：方案可采纳；修正：27B 开思考 40 次约 6.7 h；输出额度上界约 1278 万 token（云端约 786 万，非预计用量）；报告口径收窄（“本轮仅 GPT/Opus 通过”、单次通过标稳定性未验证、机械失败单列）；工具需新比较入口（每档独立 campaign、逐 Case 路由与超时）。待用户批准 520 次。
- AF139 用户 2026-10-07 批准第②步 520 次真实调用（云端 320 + 本地 200）。最终方案采纳 Codex 评审 33，下发比较工具实现。
- AF140 比较工具交付：沙箱外 757 / 0；干跑 520 槽。Codex 发现评测提示泄漏：样本 ID（含题型语义，如 w1_01_simple_residence）出现在模型可见的 source_snapshot.eval_sample。Claude 决定：仅在 AF137 比较模式改用不含语义的固定标识（由样本 ID 哈希派生），历史阶段评测输入不改以保持其回放有效，并在报告中注明历史阶段评测存在该泄漏。作为修复第 1 轮下发。
- AF141 比较工具验收：沙箱外 759 / 0；盲化标识仅限 AF137；提交 5647ae6。开跑云端 320 次（DeepSeek 两档串行、GPT、Opus 并行独立进程）。
- AF142 已开跑：云端三个独立进程（DeepSeek 两档串行、GPT、Opus），本地三档单进程串行（200 次）。
- AF143 用户要更换 DS 源：已停止 DeepSeek 比较进程（ds-pro 部分完成，ds-flash 未开始）；本地进程继续。GPT/Opus 已完成。
- AF144 用户决定继续用当前 DS 源：续跑 ds-pro/ds-flash（同 campaign，已完成录制复用，中断时在途 1 次占额度）。
- AF145 续跑被拒：中断时在途的 1 个 ds-pro 槽位“已发送但无录制”，工具按“不自动重发”整批拒绝继续。Claude 决定（修复第 1 轮）：续跑时跳过该类槽位并在报告中标为“未录制（中断在途）”，其余槽位照常发送；不自动重发、不扩额。
- AF146 续跑修复验收：沙箱外全套通过；提交 e0f3dc2。续跑 DS 126 次。
- AF147 云端 320 次完成（ds-pro 1 次中断在途未录制，ds-flash 3 次调用失败/截断）。通过：ds-pro 45/79（W1 30/39、W3 15/40）、ds-flash 36/77、gpt-6.1-sol 40/80（W1 26、W3 14）、opus-5.5 49/80（W1 28、W3 21）。分层（40 样本）：DeepSeek 能过且强模型也过 25、仅 DeepSeek 过 7、仅 GPT/Opus 过 2、云端全失败 6；两次不一致 20 个。注意：W3 标注为 w3.v1，而请求为 w3.v3（2.3 口径已改：内部矛盾→insufficient 等），W3 结果受标注版本混淆。
- AF148 本地 200 次完成。全 7 档：opus 49/80、ds-pro 45/79、gpt 40/80、ds-flash 36/77、qwen-27b-think 27/40（单次）、qwen-27b-nothink 32/80、qwen-9b-nothink 12/80（W1 0/40）。分层（任一本地过=本地即可）：本地即可 31（多靠 27B 开思考单次）、需 DeepSeek 4、本轮仅 GPT/Opus 1、全部失败 4。
- AF149 用户 2026-10-08 选择先做标注复核。Claude 草案 .ai/label-review-af149.md：W3 主要失分来自 coverage（错误候选下真实断言未覆盖应 gaps）；拟改 w3_04/05/06/10/16/17/18/20，保留其余；W1 全失败 3 例中 w1_08 接受被撤回值，w1_05、w1_14 需用户裁决。交 Codex 复核（评审 34）。
- AF150 Codex 评审 34：不建议原样实施；w3_04/10/20/18 各只收单一 verdict，w3_16 不改输入，覆盖须对 02/03/08/09 一致，评分器需“完整期望分支”。Claude 回应：采纳单一 verdict、w3_16 不改输入、分支评分；覆盖一致性部分采纳——录制显示强模型对“唯一候选被拒收”的 coverage 本身不一致、提示词未明确，故这类样本 coverage 不计分（若给 gaps 仍须精确引用），verdict 与 coverage 分列，提示词澄清记为待办。修订方案见 .ai/label-review-af149.md 末节，待用户确认并裁决 w1_05、w1_14。
- AF151 用户 2026-10-08 裁定：w1_05 改为 plan（条件“如果我以后去深圳工作”与不确定性“考虑”均须保留，删除 works_at 备选）；w3_06 verdict 仍为 contradicted[speech]，coverage 按唯一候选被拒收规则不计分；w1_14 两项必需 + 可选 works_at=“一家银行”（覆盖此前裁定）；同意 AF150 修订方案，交 Codex 实现后离线重评（0 次调用）。
- AF152 Claude 验收 AF151 实现：diff 与方案一致（整支分支、coverage 不计分仍校验精确引用、可选候选仅 works_at=“一家银行”、5 项零调用回归）；Claude 沙箱外全套 766/0（含端口测试）；样本检查 0 失败；录制无改动。离线重评 0 发送：opus 64/80、gpt 63/80、ds-pro 62/79、ds-flash 40/77、27B 思考 27/40（单次）、27B 非思考 38/80、9B 非思考 12/80。遗留：w3_10、w3_12、w3_19（用户裁定的标签）上强模型普遍失败而本地偶过，w1_12 GPT/Opus 0/2，需单独看是标签还是模型问题；未提交。
- AF153 用户选择先看 4 个可疑样本。Claude 分析 .ai/analysis-af153.md：w1_12 接受 episode；w3_12 coverage 接受 gaps（与 AF151 一致）；w3_10 提示词两条规则重叠，建议双收；w3_19 为契约缺口（频率无字段），建议不计入分层。交 Codex 复核（评审 35）。
- AF154 Codex 评审 35：大体认可；纠正 Claude“4 题都是误分层”——前三题修订后本地通过仍有效，只有 w3_19 的分层不可信。w1_12 双收（Flash 非法主体、9B 日期精度/凭空 remember 仍失败）；w3_12 复用完整分支（gaps 须含“一家银行”）；w3_10 临时双收、须保留 verdict 分布，rejected/pending 运行时不等价，长期按 insufficient 收窄提示词；w3_19 新增样本级观察标记（不计通过与分层、机械失败不豁免），归因改为“频率限定语缺少明确映射”，不预设新增字段。Claude 全部采纳，待用户裁定 w3_12/w3_10/w3_19。
- AF155 用户 2026-10-08 裁定：w1_12 memory_type 双收 fact/episode；w3_12 coverage 双收（gaps 须精确引用含“一家银行”，两候选仍 supported，覆盖旧裁定）；w3_10 本轮临时双收 contradicted/insufficient（aspect 含 value，报告保留 verdict 分布；长期按 insufficient 收窄提示词另立任务）；w3_19 设为观察题（样本级观察标记，不计通过与分层，机械失败不豁免）；待办：频率限定语映射。交 Codex 实现。
- AF156 用户 2026-10-08：本地 Qwen 换为阿里云 qwen3.7-flash（QWEN_BASE_URL=DashScope compatible-mode，QWEN_API_KEY），不再测本地。Claude 探测（job tmp 脚本，4 次极小请求）：/v1/chat/completions + json_schema strict 正常；reasoning_effort none/medium 与 enable_thinking 均可开关思考；/v1/responses 也通但输出含推理摘要，不采用。用户批准 160 次（think/nothink 各 40×2，估计 ≤7 元，按偏高单价）；本地档保留历史结果、不再运行，改用独立环境变量名避免误发。待 AF155 完成后交 Codex 加配置并零调用干跑。
- AF157 Claude 验收 AF155：diff 符合裁定；全套 771/0；录制无改动。重评（排除观察题）：opus 70/78、gpt 68/78、ds-pro 65/77、ds-flash 41/75、27B 思考 28/39、27B 非思考 38/78、9B 10/78。
- AF158 Claude 复核 AF156 实现：总体符合方案（快照、max_completion_tokens、本地档改 QWEN_LOCAL_BASE_URL、上下文字节预检、compare-af156 独立目录、干跑 160 槽）。阻断问题：DashScope 端点校验要求主机以 dashscope 开头，而用户实际端点为工作空间域名 ws-<id>.cn-beijing.maas.aliyuncs.com（/compatible-mode），真跑会被拒；Codex 干跑用了官方地址占位未暴露。Claude 已用 1 次极小请求确认快照 + max_completion_tokens 在该端点可用。修复第 1 轮：放宽为 https、主机以 .aliyuncs.com 结尾、路径 /compatible-mode/v1/chat/completions；补测试；用真实环境变量（不打印）重做干跑。
- AF159 Claude 验收修复第 1 轮：主机校验改为 https + .aliyuncs.com 后缀 + 固定路径，伪装域名/http/查询/凭据均拒；全套 777/0；真实环境干跑主机为工作空间域名、160 待录制、0 拒绝。按 AF156 用户批准启动 160 次真实录制（EVAL_COMPARE=af156 EVAL_RECORD=160，单进程串行）。
- AF160 AF156 真实录制事故：qwen37-flash-think 78 次全部 HTTP 400（93 ms，invalid_request），Claude 发现后中止（nothink 未开始）。根因（Claude 极小请求复现）：DashScope 开思考默认 thinking_budget=32768，要求 max_completion_tokens > thinking_budget；显式 thinking_budget=8192 + max_completion_tokens=16384 正常。另一缺陷：工具对连续同类错误不停，整档被烧。用户批准：显式 thinking_budget=上限一半；同档连续 3 次同类错误熔断；78 条失败录制归档不删；think 档重新授权 80 次（累计发送 238，有效 160）。修复第 2 轮交 Codex。
- AF161 Claude 验收修复第 2 轮：thinking_budget=上限/2 进入请求与指纹；连续 3 次同类 HTTP 错误熔断；旧 think 产物归档 archive-af160（未删）；全套 779/0；真实环境干跑两档各 80 待录制。重新启动 160 次录制。
- AF162 重新录制后 qwen37-flash-think 前 3 次（w1_01、w1_02）均在约 175 s 截断：output 19131（reasoning 2744），回答约 16.4k 用满回答份额，疑似严格 JSON 下重复输出；Claude 中止。工具缺陷：截断不保存正文、截断不计入熔断。已达修复 2 轮上限，向用户报告。用户选择先诊断：Codex 补截断正文片段（有界首尾）与连续 3 次截断熔断；之后开/关思考各发 1 个 W1 样本（2 次）诊断。
- AF163 用户 2026-10-08：Qwen 完成后测试 claude-sonnet-5-5、gpt-6-luna、claude-haiku-5-5（代理均可用），目的为分阶段性价比。价格：Qwen3.7-Flash ≤32k 输入 0.2/输出 0.8 元/M（缓存 0.04）；DS-V4.1-Flash 空闲 1/4 元/M（缓存 0.02）；DS-V4-Pro 9/27 元/M（缓存 0.3）；官方（2026-10-08 查）Opus 5.5 $4/$20、Sonnet 5.5 $2/$10、Haiku 5.5 $0.10/$0.50（≤100k）、GPT-6.1-sol $2/$10、GPT-6-luna $0.10/$0.50；代理实际计费可能不同。AF137 实测按此折算 40 样本一遍：opus≈8.2 元、gpt≈2.0、ds-pro≈4.4、ds-flash≈1.3。用户批准 240 次（三模型 × W1+W3 × 40 × 2，medium，估 ≈4.5 元，≤10 元）。R1/R4 对比仍按顺序放在混合检索之后；W4 需用户先评审。
- AF164 AF162 诊断（2 次，约 5 万输出 token）：qwen3.7-flash 开/关思考均先输出带 Markdown 的 JSON，再自述“Wait… Do not emit Markdown / Final / I am done”并反复重输，直至用满上限；输出含 schema 外字段（qualifiers、target）与拼错字段 condiitions。结论：DashScope 未对该模型执行 json_schema strict 约束解码（极小 schema 时靠提示碰巧正确）。用户决定：qwen3.7-flash 记为“无法严格结构化输出，不适合写入侧”，剩余额度作废，转测 AF163 三模型。
- AF165 用户 2026-10-08：改试 qwen3.8-flash（价格输入 0.8、缓存 0.1、输出 2.7 元/M；仅别名无快照）。Claude 探测（诱导违规的极小 schema）：qwen3.8-flash 与 qwen3.7-flash 在极小 schema 下均执行约束 → 3.7 的失败是大 schema 下约束静默失效；3.8 不允许 reasoning_effort 与 thinking_budget 同时设置。用户批准：真实 W1 schema 诊断 2 次（开/关思考），通过后正式 160 次（≈1–2 元），与 AF163 三模型同一对比推进。
- AF166 Claude 验收 AF163 实现：全套 791/0；冒烟 6 次：gpt-6-luna W1/W3 正常（W1 通过，native_schema）；claude-sonnet-5-5、claude-haiku-5-5 W1 均 schema_grammar_too_large，W3 均 400 provider_error_unclassified。用户批准把 AF132 评测专用 prompt_schema 例外扩展到 Sonnet/Haiku（报告标注未使用约束解码）。下一步交 Codex：两档改 prompt_schema、重做 4 次冒烟；加入 qwen3.8-flash 两档（开思考用 enable_thinking+thinking_budget，不同时传 reasoning_effort；仅别名无快照）及真实 W1 schema 诊断 2 次。
- AF167 AF166 验收：全套 797/0；冒烟 4 次 Sonnet/Haiku（prompt_schema）W1/W3 均正常；qwen3.8-flash 真实 W1 schema 诊断开/关思考均通过（合法、无重复）。启动正式 400 次（af163 三档 240 + qwen38 两档 160）。
- AF168 AF163 正式 400 次完成（0 熔断、0 中断）。按官方价估算费用（80 次=W1+W3 各 40）：opus 70/80 16.3 元；gpt-6.1-sol 68/80 4.0；ds-pro 65/79 6.7；gpt-6-luna 54/80 0.21（W1 32/40、W3 22/40）；claude-haiku-5-5 56/80 0.55（W1 25、W3 31，prompt_schema）；claude-sonnet-5-5 56/80 7.2（W1 30、W3 26，prompt_schema）；qwen38-flash-think 45/80 0.38（W1 17、W3 28）；qwen38-flash-nothink 30/80 0.07；ds-flash 41/80 2.4。初步结论：W1 便宜档首选 luna（≈GPT-sol 的 94%，费用约 1/18）；W3 便宜档首选 haiku（31/40，接近强模型，但无约束解码，生产需另解决结构化）；Sonnet 性价比不如 GPT-6.1-sol。注意：两次重复、每阶段 20 样本，结论为预筛。
- AF169 用户批准后提交 23e4750 并推送 origin/multi-model（新分支，领先 main 7 个提交）；.ai/、AGENTS.md、CLAUDE.md 仍不入库；提交前确认 diff 不含密钥与工作空间 ID。
- AF170 用户 2026-10-08 选择进入第③步混合检索。Claude 分析草案 .ai/analysis-af170.md（向量为范围内候选提示、本地嵌入默认、SQLite 暴力余弦、RRF、先建检索评测量化缺口），交 Codex 复核（评审 36）。
- AF171 Codex 评审 36：方向可采纳但收窄。纠正 Claude 两处现状描述：未整理/失败/待重判来源已在可信范围内全部进入 raw（不依赖词法），不得改回命中才可见；R1 关系提示在无更强字面命中时会成为硬过滤，向量若只搜过滤后集合修不了同义漏召回——向量候选应来自 owner/任务/主体范围，关系只用于排序，聚合仍按计划枚举。建议：首版只索引来源正文分段（不做 memory 向量）；嵌入队列独立于 W1–W4；绑定权重摘要/维度/分段版本/查询指令/内容哈希而非全局 source_revision；同库持久化，生产时 DB 10→11；单词法 + 单向量排名等权 RRF(60)，按记录去重，两路各 2×records 种子，融合后展开再预算；新增降级码需 recall v7（R1/R4/W4 不升）；Ollama /api/embed + truncate=false，首轮只比 qwen3-embedding:0.6b 与 bge-m3（显式标签，默认是 8B）；“数万条毫秒级”无证据，以实测 P95 决定是否 ANN。分四步：① 检索基线（零调用）② 本地嵌入实验 ③ 持久索引 ④ R2 接线。Claude 全部采纳；建议先只授权第①步。
- AF172 用户 2026-10-08 授权混合检索第①步（检索基线，零模型调用）：Claude 起草合成记忆库与约 30 条查询 → Codex 复核 → 用户抽检 → Codex 实现评测工具并跑基线。
- AF173 Codex 评审 37：样本需修改后冻结（T04/T20 改 StateChanged 且旧值独立来源、T14 relation=fact+speech=plan、T17 value 改“团子”、T22/T23 预置 open 关系、T28 走真实删除流程、D04 改合法本人经历、Q27/Q27b 同 query 仅 task_id 不同、Q29 加 relation=null 变体、Q31 只报告不要求召回；路径按选择机制/FTS 是否调用/过滤是否撤销分列；R1 注入用完整 r1.v2 输出并经 decode→validate；构库须产生完整提交回执与 record_updates，否则退化 legacy；填充冻结公式、加一种插入顺序对照；补控制用例）。Claude 全部采纳，并裁定：Q09 必需 T09，相关 T24、T22；Q01 必需 T01，相关 T24；版本链与关系成员计为相关不计错；新增词法同短语干扰 D07“我写程序的时候喜欢听音乐。”（针对 Q07）；D06 改作一般干扰。交 Codex 实现第①步（零调用），用户抽检冻结数据与基线报告。
- AF174 Claude 验收第①步：recall.rs 仅加独立诊断入口（不改排名/过滤/输出）；沙箱外全套 798/0；基线 160 次组装、0 安全违规、两种插入顺序一致。默认预算 records=64：必需全覆盖 20/32，recall 62.5%，MRR 0.542。漏召回 12 题：10 题无词法命中（Q01 饮料、Q03 职业、Q05 药、Q06 车牌子、Q07 编辑器、Q09 忌口、Q14 出行、Q17 几点睡、Q18 英文问血型、Q29b 无关系提示问住哪），Q19 因“我的”等弱词命中大量填充记录被数量预算截断（词法排序缺乏词权重），Q31 被错误关系硬过滤。小预算 8 结果相同。
- AF175 用户 2026-10-08 授权混合检索第②步（本地向量模型实验）。Claude 在本机 ollama pull qwen3-embedding:0.6b 与 bge-m3；交 Codex 实现嵌入客户端与离线实验（零调用部分 + 干跑），本地调用次数干跑后报用户。
- AF176 Claude 验收第②步实现：embedding.rs 仅回环、truncate=false、校验截断标记；全套 806/0；干跑 28 次嵌入（两模型各 14）。用户批准执行。
- AF177 第②步实验完成（28 次本地嵌入，0 安全违规；首次请求 qwen 1.9 s / bge 5.3 s 含加载，暖请求约 37 ms）。必需全覆盖（32 题）：现状 20/32（两预算同）；vector-only 两模型两预算均 32/32；hybrid：qwen r=8 32/32、r=64 29/32（缺 Q03、Q06、Q31），bge r=8 32/32、r=64 31/32（缺 Q06）。Claude 分析 hybrid r=64 反而更差：Q03 诊断显示 fts_called=false、词法无命中时词法通道仍按接收顺序给全部记录排名（F0100 起），两路各取 2×64=128 种子后，同时出现在两路的填充记录 RRF 分数（1/(60+r词)+1/(60+r向)）高于只在向量路排第一的 T03（1/61），被数量预算挤出；r=8 时种子少所以不显。根因：零分记录不应进入词法排名。拟修：词法通道只包含实际命中（FTS 命中或 phrase/实体得分>0）的记录；其余不参与该路排名。交 Codex 复核并修复后零调用回放重评。
- AF178 Codex 修复第 1 轮：Claude 归因部分成立——Q31 为零命中词法票，已修复恢复；Q03/Q06 的填充记录真实命中低信息词“我的”（phrase_hits=1），零分过滤不能解决。hybrid r=64：qwen 29→30/32、bge 31/32；vector-only 32/32 不变；无词法命中题与 vector-only 一致、字面题无退化。另发现回放需 serde_json float_roundtrip 特性才能精确比对 elapsed_ms。Claude 决定修复第 2 轮：模拟中词法通道按可信范围内文档频率忽略高频词（ponytail：DF>20% 截断，不足再上 BM25），同时评估对“现状”模式（Q19 弱词刷屏）的影响；永久启用 float_roundtrip（dev 依赖）或改为容差比对。
- AF179 Claude 验收修复第 2 轮：DF>20% 截断 + 零命中不投票；Cargo dev 依赖启用 float_roundtrip；沙箱外全套 811/0；零新嵌入。结果：lexical-df 21/32（Q19 解决、字面题不退化）；vector-only 32/32；hybrid-df 两模型两预算均 32/32（MRR qwen .86/.84、bge .87/.84）。结论（预筛）：向量+DF 降噪的混合方案在合成集上补齐全部缺口；两模型无差别（qwen 更小、加载更快，可作默认）。局限：合成集对向量偏易，阈值未在其他语料验证。
- AF180 用户批准：提交 b3622c4 并推送 origin/multi-model（检索基线与本地嵌入实验）；随后进入第③④步方案。
- AF181 Claude 第③④步方案草案 .ai/plan-af181.md（DB v11 来源分段向量表、即时计算待嵌入集合、独立嵌入线程、删除同事务清理、opt-in；R2：关系记录全保留+融合填充剩余、DF 截断仅 enabled、RRF(60)、降级码与 recall v7），交 Codex 复核（评审 38）。
- AF182 Codex 评审 38：方向可采纳，④.2 必须改——Q31 错误关系实际选出 111 条 fact，关系成员优先会占满 8/64 预算，向量候选进不来；非穷举路径统一融合排名，关系成员不获保留席位或词法票，plan_complete 只来自实际穷举。其他：索引为派生缓存（唯一键、BLOB/归一化版本、读取校验；正文改/删/移除在 SqliteMemoryStore::transaction 统一清理；不推进业务 revision/屏障；迁移只建表）；recall v7 仅宿主输出新增独立 retrieval 元数据（disabled/not_needed/ready/pending/unavailable），不扩展进入 R4 的 RecallCompleteness，R1/R4/W4 不升；DF 用生产词元、按独立来源与宿主可信范围统计；后台需租约防多进程、有界退避防饥饿、CLI 维护入口；查询嵌入在锁外、总时限覆盖元数据请求；首版查询格式沿用实验（不加 context）；③拆 ③a 存储失效 / ③b 后台入口，④ 检索与协议，文档同步。另指出 vector-only 同为 32/32 且 MRR 更高，融合收益未证明。Claude 全部采纳；仍建议融合（词法通道保护车牌、账号、专名等精确串在真实数据上的召回），vector-only 作为对照保留在评测中。待用户批准 DB v11、recall v7、冻结检索规则与默认模型。
- AF183 用户 2026-10-09 批准：DB 10→11（向量缓存表，迁移只建表）；recall v6→v7（仅宿主输出新增独立 retrieval 元数据，R1/R4/W4 不升，v6 请求拒绝）；冻结检索规则（小范围/聚合枚举；其他统一 lexical-df + 向量 RRF(60)，关系提示无保留席位；向量不证明完整）；默认模型本机 qwen3-embedding:0.6b；混合检索默认关闭，生产部署与开启另行授权。实施顺序 ③a → ③b → ④，每段 Claude 复核 diff 与全套测试。
- AF184 Claude 验收 ③a：DB v11 只建表（source_vector_bindings、source_vectors），删除/正文变更清理挂在 SqliteMemoryStore::transaction；旧测试只改版本号；新增 12 条回归；沙箱外全套 823/0。交 Codex ③b。
- AF185 Claude 验收 ③b：embedding 配置默认关闭、默认序列化不变、仅回环；独立后台线程（租约/代次、最小间隔 5 s、失败退避 30 s–5 min、轮转防饥饿）、独立计量、关闭不等待；pmr-mcp --drain-embeddings --max-batches N；沙箱外全套 841/0。交 Codex ④。
- AF186 Claude 验收 ④：旧测试仅版本号与新增用例（未放宽业务断言）；沙箱外全套 848/0（1 忽略）；忽略的生产路径严格向量回放单独执行通过（665 s）。生产路径对照：disabled 与 AF173 一致 20/32；qwen/bge 混合两预算均 32/32，安全违规 0。遗留风险（Codex 报告）：开启混合后，无可链接原文、无法统计 DF 的旧 legacy 记录在非穷举查询中可能漏召回，回落规则待裁定；precision 下降；真实 R4 效果与生产延迟未测。
- AF187 用户 2026-10-09：提交 84fa931 并推送；要求部署生产。Claude 已构建 release 并备份至 ~/.local/share/personal-memory-backups/hybrid-20261009/（pmr-mcp-ff164c9、runtime.json、memory-v10.sqlite3，integrity ok）。替换二进制、修改 runtime.json、重启隧道被 Claude Code 自动权限分类器拒绝（Production Deploy），未执行；交用户手动执行或放行。
- AF188 用户明确要求后由 Claude 执行部署：runtime.json 加 embedding.enabled=true（qwen3-embedding:0.6b，本机）、安装 84fa931 release 二进制、重启隧道。验证：数据库 v11，隧道已连接，后台向量化已补建 7/7 来源；Ollama 中 qwen3-embedding 驻留约 5.8 GB（32K 上下文）。
- AF189 生产试用（用户 3 轮召回 + 1 轮新增）：答案正确（阜阳、辣椒、英文问住处答阜阳）。但 retrieval.status 均为 not_needed——记录总数小（约 12 条），走小范围全量枚举，向量通道未被触发（设计如此，范围>预算才用向量），本轮不能验证混合检索效果。新来源已嵌入（8/8）。问题：ChatGPT 宿主说明仍发 recall_version=recall.v6，每个会话首个 memory_recall/memory_prepare 被拒（contract_version_unsupported），ChatGPT 去掉版本重试后成功——需用户在 ChatGPT 中更新为 docs/chatgpt-custom-instructions.md（v7）。另：后台 W1 83 s、W3 约 2.5 min；10-07 的 4 条来源仍 pending。
- AF190 生产复测：ChatGPT 已发 recall.v7，无版本拒绝，忌口答对（8.8 s）。“待整理”排查（只读）：3 条来源（10-07 两条、10-09 一条）并非任务卡住，而是 W3 对部分候选判 insufficient → 来源保持 pending（无后续重判机制）。主因是时间限定语：W1 把“现在/目前/之前”及“2025-11-01 回到阜阳”留在 sentence 未进 times，W3 按“限定语只在 sentence 不算支持”判 insufficient/contradicted——即 target-design 已知冲突“现在/目前被丢”。后果：最常见的“我现在住在X”无法提交为记忆（lives_in 阜阳 insufficient），召回只能靠原文。另：ChatGPT 新建了 family_child_1（小孩，别名孩子），与已有 child_wawa（娃，别名孩子/小孩）重复，导致“小孩”主体歧义。
- AF191 用户 2026-10-09：先修“现在/目前/之前”；重复人物要合并，但可能有多个小孩，须用户确认是哪个。Claude 分析 .ai/analysis-af191.md（W1:89 与 W3:172 规则冲突；方向 A 对齐为未解析 validity 时间；实体别名冲突机械检查 + 需用户确认的 memory_entity_merge），交 Codex 复核（评审 39）。
- AF192 Codex 评审 39：主要归因成立，建议收窄的 A。更正：三条 pending 来源都已有部分候选提交，且还有主体/speech/uncertainty/值/明确日期等拒收，不全是时间词；已有主体确认与重提取路径，但全文重提取禁止混合来源；已知冲突在 target-design 第 5 节。时间：W1 把适用于该断言的观察/历史/未来限定语写入 times，无法解析用 TimeInterp(kind=unknown) 保留原文；不新增开放区间、不把“现在”变永久有效；区分有效时间与“先前说过”回指；W3 不因无法换算拒收，但字段出现原文不自动证明支持；“之前”不机械转 ended。W1/W3 保持 v3、DB/recall 不变。遗漏：W1/W3 阶段绑定未含提示词，持久缓存可能复用旧核验——需把提示词/schema 身份纳入绑定。人物：冲突≠同一人；预检复用 store.rs 解析器，检查规范名与别名；受限合并仅允许 from 无权威语义引用；墓碑/重定向需 DB12（待批）。混合来源补处理另立任务。实测建议 6 个虚构场景 × W1/W3 = 12 次。Claude 全部采纳。
- AF193 用户 2026-10-09 批准：时间词收窄版 A（仅提示词，W1/W3 保持 v3，DB/recall 不变，阶段绑定纳入提示词/schema 身份），修后 6 个虚构场景 × W1/W3 = 12 次 DeepSeek 实测（≈0.1–0.3 元）；人物受限合并：仅 from 无权威语义引用、须用户确认，合并后直接删除 from 并写审计，不升库，新建/更新人物加重名预检；用户确认生产 family_child_1（小孩）与 child_wawa（娃）是同一个孩子（生产执行另行确认）。顺序：先时间修复，后人物合并。
- AF194 Claude 验收时间修复实现：W1/W3 提示词按收窄 A 修改，阶段绑定加入提示词/schema 摘要（变更后旧批次 needs_rejudge）；沙箱外全套 853/0。影响：1014 条历史完整录制因新提示词过期（W1 511、W3 503），历史报告保留。执行用户批准的 12 次实测。
- AF195 AF193 实测 12 次（DeepSeek Pro，W1/W3 low）：10/12 通过。W3 6/6 通过（含现在/之前/回指/明确日期）。W1 4/6：t01“我现在住在橙湾”、t06“明确日期+现在”中模型把“现在”表示为 anchored/relative offset 0 day（即仅当天），标签期望 unknown——把持续状态压缩为说话当天，后续召回可能误判已失效。其余（之前/现在两断言、目前雇主、以后计划、先前说过回指）均通过。
- AF196 用户批准修复第 1 轮：W1 提示词补充“现在/目前等持续状态限定语不得换算为单日 relative/anchored，无法可靠解析时用 unknown”；重跑 W1 6 次。交 Codex。
- AF197 AF196 重跑 W1 6 次（沙箱外全套 855/0）：t01、t06“现在”已改为 unknown（修复生效）；t03、t05 通过；但 t02、t04 回退——限定语都已保留且为 unknown，只是时间角色选错：“之前住在蓝港”给 event（应 validity），“以后打算去学陶艺”给 validity（应 event）。两轮同一样本角色结论不同，属模型对 event/validity 选择不稳定。两轮合计：限定语保留且不压缩为单日 12/12 中 10 次全对，剩余均为角色选择。
- AF198 用户批准额外第 3 轮修复（超出 2 轮上限，由用户决定）：W1 提示词明确角色——状态前后（之前/现在/目前）用 validity，计划或事件安排（以后去做）用 event；新增串联测试：W1 真实输出直接进入 W2→W3，6 场景 × 2 次 = 24 次。交 Codex。
- AF199 AF198 第 3 轮（W1 角色规则 3 行）+ 串联 24 次（W1 实际输出→W2→W3）：沙箱外全套 863/0。W1 10/12；时间角色 12/12 全对（之前/现在/目前 validity，以后 event，明确日期 event），“现在”均 unknown。W3 12/12 机械有效，因时间而拒收 0 次。两个 W1 失败与时间无关：t01 第 1 次 value 给成 boolean false（W3 正确判 contradicted[value]，未提交）；t05 第 2 次额外输出“住在蓝港”的 retraction（语义上合理，标签只要 1 个候选，标签偏严）。结论：时间词修复达成目标。
- AF200 用户同意：提交 e8687f8（时间词修复）并推送，暂不部署；接着实现人物重名预检与受限合并（AF193 裁定：仅 from 无权威语义引用、用户确认、合并后删除 from 并写审计、不升库），完成后与时间修复一起部署。
- AF201 Claude 验收人物预检与受限合并：新增 src/chat_entities.rs（memory_entity_merge：须已保存 USER 确认原话的 source_id+连续片段、双方 state_token、from 无权威引用、幂等审计、禁止旧 ID 重登记），upsert 复用解析器做重名检查；旧测试仅工具数 11→12；宿主说明同步（chatgpt-custom-instructions.md 被 .gitignore 忽略，仅本地）。
- AF202 用户批准：提交 8e0bcf5 并部署（时间词修复 + 人物预检/合并）。备份 ~/.local/share/personal-memory-backups/entity-20261010/（pmr-mcp-84fa931、runtime.json、memory-v11.sqlite3 integrity ok）；安装 release、重启隧道。生产合并待用户在 ChatGPT 中给出确认原话后执行；ChatGPT 说明需用户重新粘贴本地 docs/chatgpt-custom-instructions.md。
- AF203 生产验证：① 合并成功——ChatGPT 先 memory_list 看到两个候选，保存用户确认原话后调用 memory_entity_merge（family_child_1→child_wawa），目录仅剩“娃”（别名 孩子/小孩）与 self；确认消息本身 no_candidates_unreviewed。② 时间词：“我现在不在公司上班，在家里带娃” W1 两候选均把“现在”写入 validity unknown（修复生效），但仍未提交：两候选都是 relation=fact、value=整句（一 negate 一 affirm），W3 判 contradicted[polarity “在家里带娃”] 与 insufficient[condition “在家里”]。新问题：通用 fact 要求 value=整句，一句含两个断言（否定上班 + 肯定在家带娃）时 W3 无法区分各候选代表哪部分，极性混杂被拒；“在家里”被当条件未保留。与时间词无关。
- AF204 用户同意分析“一句多断言”。Claude 分析 .ai/analysis-af204.md（契约 1.5/2.1 通用 fact value=整句，同句多断言无法区分；方向 A 片段 value / B 新增 claim_quote / C 仅改 W3 不推荐），交 Codex 复核（评审 40）。
- AF205 Codex 评审 40：主线成立，推荐收窄 A——通用 fact value 为“足以定位该单条断言的连续原文片段”（不要求最短，quote 在完整 sentence 内，代码只核引用/包含/类型）；W3 判定：独立无关断言不影响本候选，但否定范围、共享限定语、回指、同句纠正仍按完整原文核验；不缩短 sentence、不机械分句。B（claim_quote）下游全要改，不推荐；C 不足。更正：AF66 拒收主因是合法子串≠整句；W1 还把活动地点误放 conditions。遗漏：普通 user_statement+fact+negate 即使 supported 也会被执行投影拒绝（contract.rs:1268，fact/episode 的 Memory 无极性字段，存入会丢否定），是否开放须另行裁定。版本：W1/W3 v4、W4 v5、R4 v7、recall v8；R1 v2、DB v11 不变；受影响录制按指纹过期。重点回归召回 grade 引用交叠与单候选整句覆盖推定。实测：6 场景 × 2 × W1/W3 ≤24 次 + 定点 W4/R4 ≤4 次。
- AF206 用户 2026-10-10 批准：方向 A（通用 fact value 为连续原文片段；W1/W3 v4、W4 v5、R4 v7、recall v8，R1 v2、DB v11 不变；实测串联 ≤24 次 + 定点 W4/R4 ≤4 次，≈0.5 元）；否定普通事实允许提交，另立任务（A 完成后单独分析评审）。Claude 最终方案即评审 40 第 2–6 点，交 Codex 实现（零调用部分先行）。
- AF207 Claude 复核 AF206 交付：4 项旧断言与已批准规则冲突（write_models_tests:3300 整句提示词断言；chat_background_tests:249 用合法子串制造拒收；recall_r4:1202 无 record_id 事实靠引用交叠继承 pending；recall_r4:2561 单候选整句推定遗漏来源主体）。均为 AF206 明确要改的旧行为，Claude 授权修改：1 改为片段规则断言；2 夹具改为真正非法候选、诊断与业务断言不变；3、4 改为按明确记录身份/保守返回不足的新断言，不放宽其他条件。
- AF208 AF207 完成：仅 3 个授权文件；沙箱内 860/0/1。Claude 复核 diff 符合授权，执行沙箱外全套与实测。
- AF209 沙箱外全套：intake_grounding 端口测试 fact_fragment 用例仍按旧规则期望片段被拒（inline/deferred 均失败），cargo 在此中止、其余测试未跑；Claude 命令写法未把测试失败作为门槛，24+4 次实测在失败后照常发送（已获批准的调用，但违反“测试通过后再发”约定，Claude 失误）。实测：串联 actual_commit 12/12 与预期一致；W1 严格标签 2/12（多数为模型改用专用关系 uses/owns/avoids 而非通用 fact 片段，属标签偏严）；通用片段 fact 正常提交（f02/f03 各 2 条）；f01 一否一肯：一次肯定提交、否定因执行投影拒绝而 pending（预期），另一次 W1 误用 avoids，W3 两条均 contradicted；f06 自我纠正的旧值候选被 W3 contradicted[subject]。定点 W4/R4 2×2 通过（R4 语义待人工复核）。
- AF210 AF209 修复：intake_grounding_process.py 片段用例改新规则（合法片段过 W2，W3 insufficient→pending 不提交）+ 新增非法片段被拒用例；扫描其他端口脚本无旧假设。Claude 授权 write_models_tests.rs:24 夹具数量 10→11。
- AF211 Claude 验收方向 A 全部修复：沙箱外全套（--no-fail-fast，含端口测试）881/0/1 忽略。待用户决定提交与部署（部署需 ChatGPT 端说明同步 recall.v8）。
- AF212 用户批准：提交 197414b 并部署。备份 ~/.local/share/personal-memory-backups/span-20261010/（pmr-mcp-8e0bcf5、runtime.json、memory-v11.sqlite3 integrity ok）；服务已重启，DB v11，隧道正常。待用户更新 ChatGPT 说明（recall.v8）。下一项：否定普通事实存储（另立任务）与 pending 来源补处理。
- AF213 生产验证方向 A：用户已更新 ChatGPT 说明（请求 recall.v8，无版本拒绝）。“我最近在做一个记忆系统的项目，也在照顾小孩，同时还在计划秋天出游”：W1 三条各取片段（做记忆系统项目/在照顾小孩/计划秋天出游，“最近”validity unknown，计划为 speech=plan），W3 3/3 supported，来源 committed 3/3。小瑕疵：计划候选 times 中“还在”被当作 validity 时间（应属体貌，非时间），未影响提交。prepare 37 s。
- AF214 用户同意开始否定事实存储。Claude 分析 .ai/analysis-af214.md（Memory.polarity 字段已存在、投影把普通 fact/episode negate 改为 Negative、不升 DB；语义关系交 W4；契约 2.5 文字需改），交 Codex 复核（评审 41）。
- AF215 Codex 评审 41：可行，建议只开放 user_statement+fact 否定（Negative 投影），episode 否定另行确认；W4 组装未按极性隔离，肯否可同送审，否定不自动意味撤回/冲突；误判 reinforce 会丢新否定含义，须定点验证；身份检查不放宽；R4/召回已支持 negate；Validate 保持保守。契约 2.5 文字修改并记第 8 节，无需升 DB/W1/W3/W4/R4/recall，不改提示词。旧 pending 不会自修，补处理另立任务。旧“否定必须拒收”断言需授权修改；AF206 评测 f01 断言需同步。实测另批 ≤24 次。Claude 采纳。
- AF216 用户 2026-10-10 批准：否定事实收窄版（仅 user_statement+memory_type=fact 的 negate 投影为 Negative；episode 否定暂不开放），契约 2.5 按评审 41 措辞修改并记第 8 节，不升 DB/阶段版本、不改提示词；授权修改旧“否定必须拒收”断言（W4 依赖组拒收测试改用 unknown 等仍不可投影候选）；实测 ≤24 次（W1/W3 16、W4 4、R4 4）。
- AF217 Claude 验收 AF216：投影仅新增 user_statement+fact+negate→Negative；沙箱外全套 890/0/1；AF206 离线重算 f01 第一次新增否定记录（17→18）。执行批准的 24 次实测。
- AF218 AF216 实测 24 次：串联 8/8 提交与预期一致——“我不使用锡叉，我使用木匙”两条均提交（uses negate/affirm），“我不在芦湾工坊任职”“我不再在苔桥工坊任职”“更正：我不在霜坪工坊任职”均 works_at negate 提交（n03 W1 严格标签失败仅因把“不再”放入 validity times，属合理）。W4 定点 4/4 通过。R4 定点 0/4：均为 facts[0].supersedes_record_ids 的 record_basis 机械核验失败——R4 正确判断新否定取代旧肯定记录，但未在该 fact 引用旧记录依据。Z18 已有旧方依据机械补入，需确认定点评测是否只核验 raw 输出、或补入条件未满足。交 Codex 只读诊断。
- AF219 Codex 评审 42：R4 定点失败是 AF216 评测接线遗漏——points.rs 直接 score_raw，绕过生产 Z18 旧方依据补入；生产路径（recall.rs:316 assemble_old_basis）对 4 条录制均满足补入条件，预计 final 4/4；补入不依赖极性。另一个独立语义问题：n04 第 1 次 R4 把被更正的旧断言概括为“曾在霜坪工坊任职”（违反“纠错旧值不作为历史事实”），Z18 不能修，列入人工复核/召回侧待办。修复（评测范围）交 Codex，严格离线重评。
- AF220 AF219 修复验收：仅评测两文件；沙箱外全套 890/0/1；R4 定点离线重评 raw 0/4、final 4/4（0 发送）。n04 语义问题列为召回侧待办。待用户决定提交与部署。
- AF221 用户批准：提交 68628e5 并部署。备份 ~/.local/share/personal-memory-backups/negfact-20261010/（pmr-mcp-197414b、runtime.json、memory-v11.sqlite3 integrity ok）；服务重启，DB v11，隧道正常。
- AF222 用户 2026-10-10：旧记忆直接清空、不做补处理，备份中的数据库副本一并删除。已停服务（launchctl bootout），删除生产库（含 WAL/SHM）、traces、备份目录中全部 *.sqlite3*（保留旧二进制与 runtime.json）；重启后为空库 v11。预算账本随库清零。旧 pending 补处理任务取消。
- AF223 用户 2026-10-10 同意按建议顺序：先第⑤步召回侧 R1/R4 对比。Claude 方案草案 .ai/plan-af223.md（A 标注按 r1.v2/r4.v7 复核 + 新增纠错/否定/兄弟样本，零调用；B 7 档 × 80 = 560 次，先冒烟），交 Codex 复核（评审 43）。
- AF224 Codex 评审 43：采纳。A 段须先补固定输入适配器（adapter.rs 把记录固定为 self/user_statement/fact/整句 value，无法表达计划、多主体、不确定性、片段兄弟；r4_02 原文含不确定性但输入缺字段），保留旧简写、新增完整输入形式并接语义评分；“纠错不是历史事实”已有 r4_11；兄弟互借属 R5 机械规则做零调用回归；新增样本优先补计划/否定/多主体/无年份。B 段：强度不能统一——DS 基线 R1 minimal、R4 lookup/list minimal、计算 low；GPT/Claude 测 low（替代配置标注）；Qwen omit/thinking=false；输出上限 4096/计算 16384；超时 180/240 s；调用量 7×2×(N_R1+N_R4) ≈588–602 + 冒烟 21；新增召回专用 providers.json/compare.rs，不改写入档案。Claude 全部采纳。A 段先交 Codex：工具补全 + 按版本差异逐样本列出标注疑点清单（不改标签），由 Claude 复核、用户裁定。
- AF225 Claude 标注修订方案 .ai/recall-label-plan-af225.md（R1 全保留，r1_12 待裁定；R4 改 02/04/07/08/10/11/12/15/17/19，r4_07/r4_17 待裁定；新增 4 个完整输入样本；616+21 次），交 Codex 复核（评审 44）。
- AF226 Codex 评审 44：总体采纳；收窄 r4_04（必返新住址，旧可选）、r4_10（必返旧历史住址，新可选）、r4_19（日本/泰国身份明确，应给下限 2，不得 exact）；r4_12 删除 note 中错误推理；补 14/16/18 评分覆盖（结果 2、94 天、操作数对应）。待裁定：r1_12 倾向两支；r4_07 倾向只收 supported（未说明变化≠冲突）；r4_17 倾向两支。新增 4 题补完善要求。工具缺口：完整分支只支持 W1/W3 需接 R1/R4；R4 评分仅查必需事实。人工复核须覆盖所有自动通过输出（额外事实/statement/限定语/人物/分组），同输入同输出去重，台账分列。总计 616 + 21 = 637 次。Claude 采纳。
- AF227 用户 2026-10-10 裁定：r1_12 两支都接受（均须 assertion_present、self）；r4_07 只接受 supported 当前住址 B（旧值可不提或标不确定，不推断搬家/冲突）；r4_17 两支都接受（粗时间事实+计算为空，或 insufficient；禁止补日）。交 Codex 实现标注修订、4 个新样本、R1/R4 完整分支、评分覆盖补全与人工复核台账。
- AF228 沙箱外全套 902/0/1。干跑每档 88 槽、输入约 60 万 token；Claude 按官方价估 7 档约 40–60 元（GPT-6.1 Sol 与 Sonnet 各 15–24 元）。用户 2026-10-10 选择去掉 Sonnet：6 档 528 + 冒烟 18 = 546 次（约 25–40 元）；冒烟有档不通先停问用户、不自动重发。录制入口尚为仅干跑，交 Codex 接通 B 段录制。
- AF229 Claude 验收 AF228 录制接线：沙箱外全套 907/0/1；执行冒烟 18 次。
- AF230 冒烟 18 次：协议可用 ds-pro、gpt-6.1-sol、gpt-6-luna、qwen38-flash（r4_14 计数题多档自动字段失败，属质量）；ds-flash R1 在 4096 上限 length 截断；haiku schema_grammar_too_large 熔断。ds-pro r4_14 耗时 125 s。用户裁定：冒烟门槛改为只看协议/结构/机械核验/usage；ds-flash 上限提到 16384（仅该档，报告注明）重做冒烟 3 次（额度内另加 3）；Haiku：用户指出官方支持结构化输出——Claude 核对官方文档：支持，但复杂度上限（可选参数 ≤24、联合类型参数 ≤16、语法大小内部上限），我们大量可空字段超限。用户选择先为 Anthropic 做简化 schema 再用原生模式测 Haiku（另立任务，先分析评审）；其余 5 档先跑。
- AF231 Claude 验收 AF230：仅 3 个评测文件；沙箱外全套 910/0/1。执行 ds-flash 新冒烟 3 次。
- AF232 ds-flash 新冒烟通过；五档冒烟门控全部通过。启动正式 440 次。
- AF233 Claude 分析 .ai/analysis-af233.md：Anthropic 原生结构化输出简化 schema（官方上限：可选参数 ≤24、联合类型 ≤16、内部语法大小；方向 A 发送副本可逆改写+回收后按原 schema 严格核验）。交 Codex 只读复核（评审 45），与正式召回实测并行（只读不改文件）。
- AF234 Codex 评审 45：可行，更正 Claude 分析——所有字段都 required，可选参数 0；联合字段 W1 8、W4 8、R1 7、R4 11/13，均未超公开上限 16；简单 pattern 官方支持；W3 无联合，其 400 未必是 grammar。超限推断为内部语法复杂度（重复 TimeInterp 七分支、嵌套联合、正则叠加），需实测。最小改写：保留整数界删除；展平纯嵌套 anyOf；Quote/TimeInterp 提取为本地 definitions/$ref（不改响应形状、无需反向映射）；仍失败再单批“发送副本去 pattern”评测例外；不用哨兵/包装。独立诊断配置与目录，勿改现 Haiku 配置（影响 AF230 续跑授权身份）。真实验证：Haiku 六种 schema 各 1 次（≤12）另批。Claude 采纳；为避免干扰正在运行的 AF230 正式录制，待其完成后再交 Codex 实现。
- AF235 用户批准 Haiku 简化 schema 验证 ≤12 次（6 schema 各 1，成功后各 1 暖请求；失败即停）。待 AF230 正式录制完成后交 Codex 实现。
- AF236 用户询问 Haiku 进展：此前为避开 AF230 正式录制（108/440）而等待；Claude 评估后决定并行实施（零调用、独立诊断配置/目录，不触 recall-af228 与现有 providers 配置；cargo 锁冲突只会排队）。交 Codex 实现评审 45 第 2–4 点第一步。
- AF237 Claude 复核 AF236：native_simplified 仅评测（生产拒绝），schema 字节降约 35%、anyOf 少量减少、本地引用；lib 242/0（含 4 项端口测试）、诊断 10/0。全套待 AF230 完成后跑。发送 Haiku 第一轮 6 次。
- AF238 Haiku 第一轮：W1 native_simplified 仍 schema_grammar_too_large，失败即停（已发 1/12）。用户批准评测例外：发送副本去掉非空 pattern（原 schema 回收后严格核验保留），用剩余 11 次额度重验（6 种各 1，失败即停；全成功再暖请求）。交 Codex。
- AF239 Claude 复核 AF238：nopattern 仅发送副本去 pattern、回收严格核验；lib 与诊断测试通过。发送冷轮 6 次。
- AF240 AF238 冷轮：W1 nopattern 仍 schema_grammar_too_large（累计实发 2/12），失败即停。用户批准用剩余额度单测 R1、R4-stored、R4-current 三种 schema 各 1 次（nopattern 模式），失败即停。交 Codex 增加子集选择。
- AF241 AF240 实测（累计实发 4/12）：R1 nopattern 原生约束解码成功（9.4 s，核验通过）；R4-stored 仍 schema_grammar_too_large，失败即停，R4-current 未发。结论：Claude 系列对 W1、R4 schema（即使简化+去 pattern）无法原生约束解码；R1 可以（需去 pattern 评测例外）。召回侧对比 Haiku 仅 R1 可原生，R4 不行。
- AF242 AF230 正式 440 次完成（5929 s）。自动字段：ds-pro R1 26/40 R4 40/48 2.10 元（R1 p50 3 s、R4 4 s、计算 42 s）；ds-flash 17/40 37/48 2.33 元（R1 p50 43 s）；gpt-6.1-sol 20/40 43/48 5.85 元（R4 p50 8 s、计算 10 s）；gpt-6-luna 25/40 31/48 0.29 元（p50 4 s）；qwen38-flash 15/40 30/48 0.09 元（p50 2 s，多次 decode/validate）。合计约 10.7 元（官方价）。多档同时失败的样本疑为标注过严：r1_02、r1_03、r1_11、r1_15、r1_17（need/type/conditions），r4_03（verdict）、r4_07（forbid_facts）；需第二轮标注分析后再出结论。人工复核队列待处理。
- AF243 用户 2026-10-11：DeepSeek 换官方接口（~/.zshrc DEEPSEEK_BASE_URL=https://api.deepseek.com；火山改名 HUOSHAN_DEEPSEEK_*）。Claude 探测（4 次极小请求）：/models 为 deepseek-flash、deepseek-v4-pro；/chat/completions 不支持 json_schema；/responses 接受 json_schema 但不强制（v4-pro minimal/low 均输出 Markdown 违背 schema，flash minimal 碰巧合规，flash low 推理耗尽 2000）；minimal 仍有推理 token。生产（~/.codex/config.toml 内置火山 env）未受影响；评测工具读 DEEPSEEK_* 会走官方。用户选择：官方两档先冒烟 6 次（R1、R4、R4 计算），再决定正式 176 次。
- AF244 用户 2026-10-11 决定：官方 DeepSeek 接口不测（不强制 schema、少量冒烟无法证明可靠、无明显收益）；生产与评测继续用火山方舟。撤销 AF243 第 2、3 项（官方两档与 6 次冒烟），只保留第 1 项（评测火山档改读 HUOSHAN_DEEPSEEK_*，不破坏历史回放）。
- AF245 Claude 分析多档共同失败样本 .ai/recall-label-analysis-af245.md：r1_02/r1_17 need 两支、r1_11 周二 conditions 或 time 两支、r1_15 写代码条件改可选、r1_03 保留；r4_03 verdict 两支、r4_07 允许旧值 current=no（需用户重确认）、r4_21 待核对记录极性。待 AF244 完成后交 Codex 复核（评审 46）。
- AF246 用户 2026-10-11 裁定 r4_07 放宽：允许旧记录 current=no（“曾表示…不是当前”），仍禁止 current=yes、supersedes、把旧值说成真实历史（人工）。
- AF247 Codex 评审 46：大体成立。采纳：r1_02 两支且接受 filter.relation=visited；r1_17 两支（self、assertion_present、response_language/null）；r1_03 保持 list，列为提示词缺口（另立任务）；r1_11 两支（conditions 或 time recurring，type single/temporal，op lookup，同时评分 filter.time，unknown 不自动纳入）；r1_15 conditions 可选、focus 改“当前使用工具”；r4_03 两支（同一有出处事实、条件、applies unknown/no）；r4_07 按 AF246；r4_21 保持 insufficient 与 negate（记录 103 plan negate、104 negate，Pro supported 为真实错误）。新增：r4_14 增加“两个事件代表”分支（exact=2、不同组、无额外事件，人工确认重复）；修补 R1 时间评分盲区（filter.time 归一化、time=null 不跳过时防 Flash 偏移 −1 天误通过）。其余多档失败为真实错误。交 Codex 实现并离线重评。
- AF248 AF244 撤销后沙箱外全套 930/0/1（含端口测试）。
- AF249 Claude 人工复核 228 条：R1 rejected 21（整句当条件、伪造/压缩时间、无意义 relation）、R4 rejected 5，其余 202 accepted，见 .ai/recall-review-decisions-af249.md。交 Codex 写入台账并出人工确认后报告。
- AF250 AF247 后沙箱外全套 936/0/1。
- AF251 AF250 人工确认结果（每档 R1 40、R4 48 次）：R1 human_confirmed ds-pro 33、gpt-6.1-sol 26、gpt-6-luna 26、qwen38 15、ds-flash 8；R4 gpt-6.1-sol 46、ds-flash 39、ds-pro 38、gpt-6-luna 35、qwen38 28。延迟 P50：R1 ds-pro 2.5 s、sol 6.1 s、luna 4.1 s、qwen 2.2 s、flash 43 s；R4 计算 ds-pro 42 s（P90 76 s）、sol 10 s、luna 5.7 s。费用（88 次）：ds-pro 2.1 元、sol 5.85 元、luna 0.29 元、qwen 0.09 元、flash 2.33 元。Claude 初步建议：R1 保持 DS-V4-Pro；R4 候选 GPT-6.1 Sol（最准、计算快 4 倍，费用约为 ds-pro 的 2.4 倍）或保持 ds-pro；注意经第三方代理把私人记忆发给 OpenAI 的隐私/可用性风险，切换前需端到端验证与用户授权。
- AF252 用户同意按建议：提交 2996198 并推送；下一步端到端对比方案。
- AF253 Claude 端到端对比方案草案 .ai/plan-af253.md（A 现生产全量新基线 ≈50 元；B 仅 R4 换 GPT-6.1 Sol ≈5 元；C 可选 Luna），交 Codex 复核（评审 47）。
- AF254 Codex 评审 47：可行但先小子集。缺口：评测代理只有单一火山上游需按角色路由；执行配置固定为旧 AC2（W1 medium 等）需新增生产对齐 profile；录制按整份配置比较，需分阶段绑定与只读父录制复用（B 严格回放 A 的 W1/W3/W4/R1，仅重录 R4，失败输出也复用）；缺跨续跑额度/熔断。混合检索 A/B 都关闭（小库有关系提示时也可能触发；执行器无嵌入索引）。首轮子集 10 例 15 问：A 上限 104 次、B 上限 15 次，约 7–8 元；C 暂缓。需新增问题级人工确认台账；子集分母分开统计。先工具接线零调用验收，再批准录制。Claude 采纳。
- AF255 用户 2026-10-11 批准端到端小子集（10 例 15 问，A ≤104 + B ≤15 = ≤119 次，≈7–8 元）；工具验收通过且干跑不超 119 次即直接录制，超出先停问。交 Codex 实现评审 47 第 1–6 点。
- AF256 Claude 沙箱外全套：947/2/1。失败 1：tests/eval/af255.rs:1317 af255_production_loopback_roundtrip（端口测试，Codex 沙箱内未执行）——B 臂期望 R4 发送 1 次，实际 0（R4 未被发送）。失败 2：recall_compare::af228_report… 依赖可变人工台账（AF250 写入后队列 3→2），测试未隔离台账。交 Codex 修复第 1 轮（根因修复，不放宽断言；AF228 测试改用独立空台账夹具）。
- AF257 修复第 1 轮验收：B 臂 R4 被生产网络门控拒绝（offline）——改为两臂共用 loopback 客户端由代理按阶段路由；AF228 测试改用空台账夹具。沙箱外全套 950/0/1。开始录制 A（≤104）。
- AF258 端到端 A 录制完成：实发 94/104，用时 2875 s。
- AF259 端到端 B 录制完成：实发 15/15（R4=gpt-6.1-sol），用时 144 s；A+B 共 109 次。
- AF260 端到端小子集评分（common.v1 + Claude 问题级复核，见 .ai/e2e-review-decisions-af260.md）：A（R4 DS-V4-Pro）13/15，2 题为 R4 机械核验失败（lme_6dc9 107 s 6.8k 推理、device_q5）；B（R4 GPT-6.1 Sol）12/15，0 机械失败，但 task_q1/q2 过度保守判 insufficient、conflict_q2 在冲突中另给“7 英寸 current=yes”。B R4 更快（lme 15 s vs 107 s）。结论：小子集上 B 不优于 A，暂不切换 R4；DS-V4-Pro R4 的机械失败值得单独分析。
- AF261 Codex 诊断 A 的两条 R4 机械失败均为模型违背提示词：lme 引号乱码导致 quote_or_source；device_q5 把问题里的条件挂到旧来源事实上（qualifier_source）。B organization_q6 补 accepted，最终 A 13/15、B 12/15。
- AF262 用户要求提交：全套 950/0/1 后提交 41356c4 并推送（端到端 A/B 工具与结果、问题级台账）。
- AF263 用户同意按建议顺序推进。第一项 R4 格式防护：Claude 分析 .ai/analysis-af263.md（lme 失败为双引号被写成单引号；device_q5 为问题条件挂到来源事实；方向 A 引号等价机械容错+替换精确原文、b1 提示词补强、b2 失败粒度待评估、不做自动重试），交 Codex 复核（评审 48）。
- AF264 Codex 评审 48：更正 Claude——lme 失败是双引号变多字符乱码（Ã¢Â€Âœ），非单双引号替换；建议收窄 A（Z18 前独立引用恢复步骤：仅排版/明确编码变换、指定来源内唯一连续匹配才替换为精确原文，覆盖 facts 与 compute 证据，raw/final 分列，不改 locate_quote）+ b1 提示词可执行检查（条件逐一对照所引来源、引用逐字复制仅 JSON 转义）；不做 b2（需改冻结契约 3.4）、不重试。用户 2026-10-11 批准，实测 ≤8 次 R4。交 Codex 实现。
- AF265 AF264 验收：引用恢复仅限引号排版/固定乱码且唯一匹配；R4 提示词补强条件出处与逐字引号；沙箱外全套 955/0/1；AF255 A 离线重核 final 14/15（lme 恢复，device_q5 仍拒）。执行 8 次实测。
- AF266 AF264 实测 8 次（DS-V4-Pro R4 minimal）：8/8 通过，p50 约 3.3 s。人工复核：引号逐字（含"竹声""湖青"）、来源真实条件“只在画水彩时”保留、问题专属条件未挂到事实、改名场景旧名 app=no 新名 app=yes，全部 accepted。
- AF267 用户批准：提交 a97741d 并部署；备份 /Users/xyzjiao/.local/share/personal-memory-backups/r4guard-20261011（pmr-mcp-68628e5、runtime.json、memory-v11.sqlite3）。
- AF268 第二项 R1 清单类问题：Claude 分析与方案 .ai/analysis-af268.md（提示词补一条 set→aggregate/list 判别规则；验证 R1 20×2 回归 + 4×2 泛化 = 48 次），交 Codex 复核（评审 49）。
- AF269 Codex 评审 49：采纳方向，改措辞（按“答案是否为主体集合的成员”判断，不按疑问词或多版本可能；count/sum/compare/elapsed 保持；具名对象不排除枚举；宽泛请求可留 open；不举题型例子）；修正归因：lookup 有关系过滤或小范围时也会穷举，10 次字段失败不等于实际漏项。验证 48 次 R1（20×2 回归 + 4×2 泛化），独立 campaign，生产 Pro minimal/4096/180 s。Claude 采纳。
- AF270 用户批准 R1 修复与 48 次实测。交 Codex 实现。
- AF271 AF270 后沙箱外全套 958/1/1：唯一失败 v3_nonblocking_listener_reads_delayed_request_without_panicking（tests/eval/v3.rs:804 listener.accept()），单独重跑 5/5 通过，判为既有端口测试在全套负载下的偶发失败，与 R1 改动无关，记为待修。执行 48 次 R1 实测。
- AF272 R1 实测 48 次：虚构 4 题 8/8；原 20 题自动通过 34→31——目标 r1_03 仍 single/lookup（2/2），r1_07 compare→lookup（2/2 退步），r1_19 把“其他用户”绑为所有实体 ambiguous（2/2 退步），r1_10 一次主体未绑。用户 2026-10-11 决定：撤回提示词改动、不部署；r1_03 标签放宽为在 filter.relation=allergic_to 时也接受 single/lookup（有效关系过滤会带回该关系全部记录）。保留 AF268 评测入口与录制作为历史。交 Codex。
- AF273 Claude 起草 W4 23 样本评审 .ai/w4-label-review-af273.md（动作全部同意；11、21 输入按 v4 过时建议改；建议新增否定事实更新与同句兄弟样本），交 Codex 复核（评审 50）。
- AF274 AF272 验收：src 与 a97741d 一致（提示词已撤回）；沙箱外全套 957/0/1；R1 离线重评仅 r1_03 变化（ds-pro 34→36 等）。提交 59aaf63 并推送（无需部署：生产代码未变）。
- AF275 Codex 评审 50：动作大多保留；更正 Claude——11 不必改 likes（fact 整句合法）、21 撤回候选 value=null 不能填片段；“未录过无损失”错误（已有多档录制，输入改动按指纹过期）。补充：04 输入需修（周末进周期时间、去掉“更喜欢”强度遗漏风险）；19 输入需修（两句都写成已配置状态）；15/21 的 51 原文为转述却标 user_statement，应改为用户本人带不确定性断言；17 recall 断言错（旧 90 保持 judged）；08/14 补完整范围对照；n1 拆两个独立样本（不再任职→state_change，从未任职→correction，negate）；n2 同句兄弟片段。工具：check_samples 仍要求整句、禁止 fact 完整，W4 评分器未评 effective/relation_scope/after/recall/must_not，需补。Claude 采纳，待用户确认。
- AF276 用户 2026-10-11 同意 W4 样本定稿（按 AF275）：保留动作；改 04/19/15/21 输入；修 17/21 标签；08/14 加完整范围对照；新增 n1a/n1b/n2；补全评分工具；定稿后 label_status=reviewed。交 Codex 实现（零调用）。
- AF277 AF276 Codex 调用失败：OpenAI 服务流断开（重连 5/5 失败，request e53e972c-…），工作区无改动。重试一次（同一会话、同一任务）。
- AF278 用户要求立项分析借鉴三点。Claude 分析 .ai/analysis-af278.md：1 批量提取——每条消息约 0.18 元，输出（推理）约占 85%，输入多已缓存，批量收益不确定，先做小实验；2 常用事实快照——扩展 bootstrap 快照放入已核验当前本人事实，宿主直接答，机械入选条件，协议与隐私需确认，优先级最高；3 实体加权——词法通道已按实体命中优先排序，暂缓。待 W4 任务完成后交 Codex 复核（评审 51）。
- AF279 AF276 重试再次因 OpenAI 服务断流失败，但已完成大部分改动（23 样本、新增 w4_08_complete/w4_14_complete/n1a/n1b/n2、检查器、生成器、评分器、w4_offline）。让 Codex 在同一会话中基于现有工作区继续完成（核对、补齐、测试、报告）。
- AF280 AF276 完成：28 样本 reviewed/AF276（新增 08/14 完整对照、n1a/n1b/n2），生成器一致；新增 tests/eval/w4/runtime.rs 真实 SQLite 执行核对 after；评分补 effective/relation_scope/目标集合/完整分支/人工复核绑定；contract_types 数量 23→28。历史录制：04/15/19/21 关联 24 条过期，126 条旧 w4_v2 全不兼容。发送评审 51（借鉴三点）。
- AF281 Codex 评审 51：1 成本重算 0.182 元/条，输出占 82%（推理 76%），仅消掉缓存输入最多省约 1.1%，批量收益须来自输出或调用次数下降；W3 空候选跳过已实现。建议先零调用统计固定开销，再 ≤40 次小实验（2 段会话×3 消息×2，单条 W1 12 + 批量 W1 4 + 两臂 W3 ≤24），只批量 W1。2 快照：Active≠current、resolved≠条件满足、无问题时不能机械判相关 pending；global_context 附在每个工具响应会反复推送；最小方案为 opt-in 的“已存陈述快照”（bootstrap.records 少量完整 CurrentRecordView，不生成当前判断），若要直接据快照确认当前值须改 contracts 3.4/target-design 2.5 并升 recall v9；隐私白名单（住址、健康）需用户定。3 实体：判断大体对但覆盖被高估（只是登记名字面信号，受 DF 截断）；先零调用补超预算宽主体对照。
- AF282 沙箱外全套 963/0/1；提交 eb041ce 并推送（W4 定稿）。用户裁定（评审 51 后）：快照只做“已存陈述快照”（opt-in，不生成当前判断）；类别白名单=回答偏好与长期喜好、职业与工作单位、居住地、健康与过敏；用户问快照如何更新；批量提取先零调用统计再定是否实测。
- AF283 用户 2026-10-11 批准：快照只在对话开头（bootstrap）返回一次；recall v8→v9；opt-in 默认关闭；四类白名单；只收已核验、无 pending、无开放冲突的记录，作为“已存陈述”不生成当前判断。另两项零调用（批量提取固定开销统计、实体检索诊断评测）排在后面。交 Codex 实现快照。
- AF284 用户 2026-10-11：之后 Codex 使用 gpt-6-astra、medium 推理；每次调用显式传 -m gpt-6-astra -c model_reasoning_effort="medium"（全局 config 已是该设置）。正在运行的 AF283 任务按启动时的配置继续。
- AF285 AF283 验收：白名单 8 个关系 ID、opt-in 默认关、仅 bootstrap、v9；沙箱外全套 974/0/1。提交 3917d17 并推送；部署待用户确认。
- AF286 用户批准“部署并打开”：2026-10-11 部署 3917d17。备份 ~/.local/share/personal-memory-backups/snapshot-20261011/（旧二进制 a97741d、runtime.json、DB v11 备份 integrity ok）；runtime.json 增加 statement_snapshot.enabled=true；隧道重启，DB user_version 11。待用户更新 ChatGPT 说明到 v9、刷新插件后新对话实测。
- AF287 生产实测：快照 bootstrap 正确返回深圳(之前)/阜阳(现在)两条已存陈述；但同轮 recall“我住在哪？”insufficient。Claude 分析+Codex 评审 52 确认：库清空后仅 4 个来源，DF>20% 规则使任何命中词被删（lexical.rs:1047），lives_in 关系提示又挡住小范围穷举（recall.rs:1097），且 Ollama 未运行致向量 0/4、无兜底；trace 未记录向量失败具体原因。用户启动 Ollama 后向量 4/4。修复方案待用户裁定（DF 绝对下限改冻结规则需 recall v10）。
- AF288 用户批准三项全做：①embedding 开启且主体收窄后记录数≤预算时取消关系过滤、全部枚举、不调查询嵌入（关闭路径不变，截断撤销完整）；②DF 规则改为 documents>0 && (df<=1 || df×100<=documents×20)，recall v9→v10；③向量查询/缓存失败按固定类别记 trace。Codex 评审 53 可实施，Claude 全部采纳其收窄建议（不做字节预判、DF 按独立来源计、不新增宿主失败码、历史 v9 适配保留）。交 Codex 实现。
- AF289 AF288 验收：Claude 复核 lexical/recall 核心 diff 与方案一致；沙箱外全套 980/0/1；严格向量回放 28 条复用、指标与 AF185 完全一致。已提交推送；部署待用户确认（v10 拒绝 v9，需同步更新 ChatGPT 说明）。
- AF290 用户批准“现在部署”：2026-10-11 部署 044d6ee（recall v10）。备份 ~/.local/share/personal-memory-backups/smallscope-20261011/（旧二进制 3917d17、runtime.json、DB integrity ok）；隧道重启，DB v11，快照开关保持开启。待用户更新 ChatGPT 说明到 v10 后实测。
- AF291 生产实测 v10 通过：bootstrap 快照 2 条；recall“我住在哪？”走小范围枚举（retrieval not_needed，零嵌入调用），R4 answered：现在住阜阳（记录2）、之前住深圳（记录1），plan_complete=true。
- AF292 批量提取零调用统计（Claude 分析，Codex 评审 54 复核）：DS-Pro W1 每次约 ¥0.028（0 候选）/¥0.096 均值、¥0.057 中位（1 候选）；输入大多命中缓存，费用主要是推理。按假设固定开销约 ¥0.02/次，三条合批约省 ¥0.013/条（≈7%），只是情景估算、不是上限；数据为旧 W1 v3/medium，当前 v4/low 不可直接外推。W3 空候选跳过已实现，W2 无模型费用。结论：暂缓 ≤40 次批量实验，不加预过滤模型；后续观察 v4/low 逐阶段费用与失败分布，模型/推理强度对比另行立项，AF168 只作历史依据。
- AF293 实体检索诊断方案（Claude 分析，Codex 评审 55 修正，Claude 全部采纳）：③主体严格过滤缺口确认、②别名展开已支持、④小范围阈值为主体收窄后≤records 预算。做法：独立诊断语料/查询文件，不改冻结的 40 题和 AF185 基线；两臂=embedding 关（原词法）与 embedding 开但注入查询失败（lexical-df 回落），不新增向量；D1 常见实体+稀有属性、D2 只有常见实体、D3 登记别名（低/高 DF 对照）、D4 改为无指代依据的限制诊断、D5 主体与被提及者不同（e-daughter vs 不指定主体两种 R1 计划，来源已整理）；加其他孩子相似记录、宿主限定主体两个安全控制；断言实际路径（收窄后>64、非枚举）。只报告不断言，修不修另由用户定。零模型调用。交 Codex 实现。
- AF294 AF293 验收：独立语料 144 记录、56 行两臂对照；冻结 40 题未改；沙箱外全套 981/0/1；提交推送。结论：D1 稀有属性可救回；D2/D3 高 DF 只有常见实体时漏召回（关闭臂受数量预算、回落臂零命中），实体免 DF 单独不足；D4 无指代依据不猜；D5 R1 绑定女儿时 self 记录在主体过滤阶段丢失，不指定主体则命中——最值得修；宿主限定主体安全控制全部通过。修不修待用户定。
- AF295 用户 2026-10-11 批准修 D5（R1 绑定非本人主体时，提到该人的本人记录被主体过滤排除）。按流程：Claude 分析→Codex 复核→方案→复核→实现。
- AF296 Codex 评审 56：仅 (a)+(b) 解决不了 D5（通用 fact 的 value 是文本），需 (c) 字面提及，但只作扩大候选、不加分、不保送、不认定实体。用户批准改契约并升 recall v11。Claude 全部采纳评审 56 的规则、验收和范围：两臂都生效；(c) 只看 Text 值和记录自身的 evidence quote，完整登记名连续词元匹配，单字名不扩展；新增 r1_subject_mentions 诊断；拒绝 v10、历史评分保留 v10；不改 W 侧/提示词/schema/DB/排名/向量；替换 AF293 的 D5-bound 旧断言，SAFE 不放宽。交 Codex 实现。
- AF297 AF296 验收：Claude 复核 recall/lexical diff（仅非 self 主体、完整登记名、单字不扩、eligible 内）；AF293 D5-bound 8 组合转命中、SAFE 通过；向量回放 28 槽指标与 AF288 一致；沙箱外全套 984/0/1。提交推送；部署待用户确认（v11 拒绝 v10）。
- AF298 用户批准部署：2026-10-12 部署 35ba278（recall v11）。备份 ~/.local/share/personal-memory-backups/mention-20261012/（旧二进制 044d6ee、runtime.json、DB）；隧道重启。待用户更新 ChatGPT 说明到 v11 后实测。
- AF299 生产实测 v11 通过（用户说的是“儿子”）：W1 存记录 3 主体 self、值“我给儿子报了游泳课”；新对话 R1 绑定主体 son，记录 3 经名字提及纳入候选（主体过滤本会排除它），小范围枚举，R4 supported“我给儿子报了游泳课”，plan_complete=true。
- AF300 修偶发测试 AF271。Claude 分析：tests/eval/v3.rs:804 测试把 listener 设为非阻塞后只调用一次 accept().unwrap()；客户端 connect 返回后，服务端 accept 队列在负载下可能尚未就绪，返回 WouldBlock 致 panic。生产代理循环（v3.rs:279/308）已处理 WouldBlock，缺陷只在测试。方案：测试内对 WouldBlock 有界重试（短睡眠、约 5 s 截止），其他错误照常失败；不改生产代码。
- AF301 AF300 验收：Codex 复核同意并实现（仅测试，WouldBlock 每 10 ms 重试、5 s 截止）；沙箱外该测试 20/20、全套 984/0/1；提交推送。AF271 关闭。
- AF302 用户 2026-10-12 批准写入侧（W1+W3）重新对比真实调用：4 档 320 次、估 ≈12 元、费用上限 20 元超出熔断；DS-V4-Pro（生产设置，对照）+ GPT-6-luna + GPT-6.1-sol + Claude Haiku 5.5（prompt_schema 例外），每档 W1/W3 各 20 合成样本×2 遍。先由 Codex 复核方案并准备冻结计划与零调用预检，Claude 再执行。
- AF303 Codex 评审 57：当前比较入口不能组合四档/改 DS 为 low，且无发送前费用熔断，阻塞执行。Claude 采纳：DS-Pro 用生产 low，其余三档沿用 AF163 medium；先每题一遍 160 次，人工看完再续第二遍（共用 320 槽授权）；新 manifest 冻结四档配置、320 槽、指纹、冻结价格表，发送前按单次费用上界预留、按 usage 结算，累计达 20 元即停，usage 缺失按上界计；w3_19 仍为观察题。不采纳：为 W1/W3 新建问题级人工台账（YAGNI，Claude 对失败题逐条人工复核即可）。交 Codex 补齐工具（零调用）。
- AF304 AF303 验收：Claude 复核 af302.rs 预留/结算逻辑（顺序发送、发送前按上界预留、usage 缺失按上界计、累计超 20 元拒发）；两遍干跑各 160 槽；沙箱外全套 986/0/1。开始第 1 遍 160 次真实调用。
- AF305 第 1 遍首次执行因 ANTHROPIC_* 缺失未发；用户补上后再次被拒“manifest changed”：Codex 在沙箱环境生成的冻结计划指纹含 provider 端点，与本机 env 不一致。确认 0 发送、无费用账本后，旧目录移至 compare-af302-codex-dryrun，在本机重新干跑：320 槽/160 选中/费用数字不变，manifest sha256:43bc8ad5…。开始第 1 遍真实调用。
- AF306 第 1 遍进行中：gpt-6.1-sol 经代理 token.aihezu.dev 多次 “error sending request”（5 s 左右或 35 s），usage 缺失按上界 2.66 元/次计入预算；至 92 槽累计 16.97 元，其中 4 次失败计 10.6 元，实际费用很可能接近 0。预计很快触发 20 元熔断，sol 余题与 Haiku 将被拒发。让其自然结束后报告用户。
- AF307 第 1 遍结束（115 槽发送/预留，累计 19.94 元触发熔断）。有 usage 的实际估算费用 2.79 元；17 次 OpenAI 代理（token.aihezu.dev）“error sending request”无 usage，按上界计 17.14 元（luna 11 次、sol 6 次；Haiku 走同代理 0 失败）。结果（未人工复核）：ds-pro 通过 25/39（+1 观察），1.88 元，W1 p50 21 s；luna 17 通过/11 失败/11 调用失败；sol 4 通过/5 失败/6 调用失败/25 熔断未发；haiku 14 通过/6 失败/20 熔断未发（W3 14 题全发、W1 只发 6）。数据不完整，待用户决定。
- AF308 用户 2026-10-12 裁定：“都重新跑吧，失败不计费”。即：无 usage 的传输失败（error sending request）结算为 0；17 个连接失败槽允许重发，45 个熔断未发槽继续发；总预算仍 20 元（按实际 usage 结算）。交 Codex 改工具（零调用）。
- AF309 AF308 验收：Claude 复核传输失败判定（http.execute 错误，含发送超时）结算 0、仅 transport_failed 槽可重发；账本迁移 19.94→2.79 元，备份 cost-ledger.before-af308.json；沙箱外全套 988/0/1。开始第 1 遍补跑。
- AF310 第 1 遍补跑两次后 159/160 结算，sol 1 题 W1 仍传输失败（不再重试），实际累计 4.60 元。自动分：ds-pro W1 12/20 W3 13/19；luna W1 13 W3 11；sol W1 11/19 W3 15；haiku W1 11 W3 16。Claude 发现 W1 标签 value 仍按旧整句规则（如 w1_05 期望整句、模型给片段），自动分系统性偏低。交 Codex 逐题归因（标签过时/模型错误）并离线按现行规则重评分，零调用。
- AF311 用户裁定（评审 58 草案）：A 类 13 处标签修订全部批准（含 W3_05 推翻 AF151 改为 insufficient[subject]、W3_01/14 改 contradicted[time]）；C2 w1_11 零候选+source retention 分支也接受；C3 w3_07 只接受 contradicted[speech]；C4 w1_05 条件中的“以后”不必另列租房时间（列与不列都接受）；C1 w1_06 未单独提问，按 Claude/Codex 倾向维持 fact、不接受 episode。交 Codex 正式修订样本并离线重评分。
- AF312 AF311 验收：Claude 抽查标签 diff；160 请求指纹不变；沙箱外全套 990/0/1；提交推送。第 1 遍最终（AF311 标签）：ds-pro W1 19/20 W3 17/19 ¥1.88；luna 17/20 11/19 ¥0.12；sol 18/19 18/19 ¥2.31；haiku 17/20 13/19 ¥0.30。初步：W1 可考虑 Haiku/luna，W3 保持 DS-Pro 或 Sol；需第 2 遍确认（需按新标签重冻结 manifest）。
- AF313 用户批准第 2 遍只跑 W1 四档 80 次（约 2.3 元，仍在 20 元总预算内，已用 4.60）。需按 AF311 标签重冻结：交 Codex 实现显式“标签修订后重冻结”步骤（零调用），由 Claude 在本机环境执行以避免端点指纹差异。
- AF314 AF313 验收：Claude 复核重冻结逻辑（去标签摘要后其余必须一致，否则拒绝；备份 manifest.before-af311.json + 审计）；沙箱外全套 992/0/1；本机重冻结成功；第 2 遍 W1 干跑选中 80、已计 4.60 元。开始真实发送。
- AF315 第 2 遍 W1 完成：239/240 结算（sol 第 1 遍 1 题传输失败仍缺），累计 7.30 元（第 2 遍约 2.70）。第 2 遍自动分（AF311 标签）：ds-pro 17/20、luna 15/20、sol 18/20、haiku 10/20；两遍合并 W1：ds-pro 36/40、sol 36/39、luna 32/40、haiku 27/40。Claude 观察：第 2 遍又出现标签接受集合之外的合规片段（如“开始学日语”“考虑租房”“辞职了”“养过一只猫”，w1_12/13 value 仍为整句），同时有真实错误（w1_05 丢“考虑”、w1_13 未绑定老王、w1_08 撤回加 forget）。交 Codex 归因，零调用。
- AF316 Codex 评审 59：第 2 遍 20 条失败 A8/B11/C1；建议结构性评分：标签给“允许断言窗口＋必含谓词/属性核心”（如“学日语”“特别怕狗”），代码只查连续包含，专用关系不套；时间引用仅在标签指定窗口内放宽。草案两遍合并 W1：ds-pro 37/40、sol 36/40（含 1 缺口）、luna 34/40、haiku 32/40。Claude 计算 W1 实测单次费用/p50：ds-pro ¥0.053/20.7 s，sol ¥0.073/27.6 s，luna ¥0.0039/18.4 s，haiku ¥0.010/7.3 s。C：luna w1_10 value 仅“3月12日”。
- AF317 用户裁定：批准 W1 结构性评分（允许窗口＋必含核心，代码只查连续包含；主体/极性/限定语不放宽；专用关系不套）；luna w1_10 只写“3月12日”不算对；W1 生产模型暂不换，保持 DS-V4-Pro（W3 也保持 DS）。交 Codex 正式实施草案（零调用）。
- AF318 AF317 验收：Claude 抽查评分器（窗口唯一出现、value 在窗口内唯一、必含核心全包含）；240 指纹不变；沙箱外全套通过；提交推送。最终：W1 合并 ds-pro 37/40、sol 36/40、luna 34/40、haiku 32/40；W3（1 遍）ds-pro 17、sol 18、luna 11、haiku 13（/19）。生产 W1/W3 保持 DS-V4-Pro。对比总花费约 7.30 元。
- AF319 用户 2026-10-12 批准 W4 多模型对比真实调用：DS-V4-Pro（生产设置，对照）+ GPT-6.1-sol + GPT-6-luna，W4 样本（排除不调用 W4 的 06/08）×2 遍，估 156 次 ≈7 元；Claude 设费用上限 15 元熔断；传输失败按 0 计可重发（沿用 AF308 政策）。先交 Codex 复核方案并补工具（零调用）。
- AF320 AF319 工具验收：150 槽（25 题×3 档×2 遍），三档 medium、输出上限 16384；Claude 复核共用 cost_campaign 抽取；沙箱外全套 999/0/1；本机干跑第 1 遍选中 75、情景约 9.8 元（ds 2.85/sol 6.63/luna 0.33）。开始第 1 遍真实发送。
- AF321 W4 第 1 遍完成：75/75 结算（首次 7 次传输失败，重试补齐），实际 3.21 元（ds 1.87、sol 1.26、luna 0.07）。自动分（字段+运行时）：ds-pro 18/25、sol 15/25、luna 6/25。日志中的 runtime.rs panic 为运行时 after 核对失败被捕获计为“失败”，非工具崩溃。交 Codex 归因（零调用）。
- AF322 Codex 评审 61：36 条失败 DS A3/B4、Sol A5/B3/C2、Luna A2/B17（15 题引用 source.kind=current 违反 stored 规则）。Claude 核对提示词 274/295 行确认。用户裁定：A 类修订批准（effective 也接受候选已核验 validity/unknown 时间原样引用）；C Sol 04/23 周期时间作 effective 算对；第 2 遍只跑 DS+Sol 50 次（约 3.1 元），暂缓 luna。交 Codex 正式修订标签+档位筛选+重冻结（零调用）。
- AF323 AF322 验收：7 个 W4 样本修订、指纹不变；档位筛选；沙箱外全套 1001/0/1；本机重冻结成功；第 2 遍 DS+Sol 干跑选中 50、已计 3.21 元。开始真实发送。
- AF324 W4 第 2 遍（DS+Sol 50 次）完成，无传输失败；累计实际 6.12 元（上限 15）。第 2 遍：ds 20/25、sol 23/25。两遍合并（AF322 标签）：ds-pro 41/50（单次约 ¥0.071，p50 23.4 s）、sol 45/50（约 ¥0.050，p50 25.9 s）；luna 1 遍 8/25。新增失败：ds w4_07 判 pending/unclear 而非 conflict（B）；sol w4_16 pending 时 targets=[81] 而非 []（可能 C，未裁定，不影响结论）。其余为第 1 遍已归因的 B（16/17/20/22）。代码已提交推送（全套 1001/0/1 已在发送前通过）。W4 生产模型待用户决定。
- AF325 用户 2026-10-12 裁定：W4 生产模型换 GPT-6.1-sol。需先定代理失败回退策略、端到端验证与部署授权。
- AF326 代理连不上原因排查（Claude，零调用）：评测进程继承 shell 的 https_proxy，经本机 Clash Verge（mihomo 127.0.0.1:7897）出站；token.aihezu.dev 命中 MATCH 规则走“极速 专线 美国 01”节点，火山 ark 为国内直连故 0 失败。失败成簇（如 18:37:39–51、18:44:56–45:06 各 3–4 次），耗时 1–5 s 或 100+ s，像节点/隧道瞬断，非固定超时；当时的 Clash 日志已轮转，无法直接证实；reqwest 只记录了笼统的 “error sending request”，未保留底层错误链。aihezu 在 Cloudflare 上，本机直连 65 ms 可达。生产 pmr-mcp 的 env 来自 ~/.codex/config.toml，没有 https_proxy，会直连，不经 Clash。Codex 评审 62：生产 runtime.json 无 write_models，可部分配置仅 w4；但 openai.rs:404 在任一角色配 provider 后会给未配置角色新增 binding，致 W1/W3 检查点 needs_rejudge，需先修；W4 失败后台不自动重试（failed 来源不被扫描），需人工 reprocess；不加自动回退。Claude 采纳并追加：传输错误在 trace 中记录有界底层错误链（不含密钥/正文）以便生产诊断。交 Codex 实现。
- AF327 AF326 验收：Claude 复核 openai.rs（未配置角色保持原绑定、错误链只输出固定类别）；沙箱外全套 1004/0/1；提交推送。部署（W4→sol）待用户在 ~/.codex/config.toml 的 personal_memory env 加 OPENAI_BASE_URL/OPENAI_API_KEY 并确认。
- AF328 用户选择按现状在 ~/.codex/config.toml 加 OPENAI_*；用户新增段落误写成重复的 [mcp_servers.personal_memory.env]（TOML 非法），Claude 备份为 config.toml.bak-20261013 后把两行并入原 env 段，tomllib 校验通过（未显示密钥值）。部署 a0a5af9：备份 ~/.local/share/personal-memory-backups/w4sol-20261013/（旧二进制 35ba278、runtime.json、DB integrity ok）；runtime.json 新增 write_models 仅 w4=gpt-6.1-sol、medium、openai_responses+OPENAI_*；W1/W3 仍从 DEEPSEEK_PRO_MODEL 取 DS-Pro。隧道重启 00:20:41，进程含 OPENAI_* 变量名，DB v11。部署前 1 条 pending 来源（W3 后停留，不在途）。待用户真实更新时核对 W4 trace。
- AF329 两条卡住原话排查（Claude 本地分析，零调用，原文未发给任何模型）：①pending 来源（儿童安全座椅会话 t2）：W1 3 候选，W3 全部 insufficient（主体“现在用的是/现在是”未明确是谁、第三条是问句），W4 计划 groups 为空、候选 held，w4_finish_round 判 pending——按设计等待澄清，非缺陷。②failed 来源（10-11 一条粘贴的长提示词）：W1 3 候选通过；DS-Pro W3 输出 reviews=[]（应逐条 3 个）且 coverage.status=gaps 但 missing=[]，validate 判 model_invalid_output，正确拒收；W4 失败/校验失败后不自动重试。
- AF330 Codex 评审 63：A pending 符合设计（W3 未放行，非 W4 held；原话以 uncommitted raw 可被召回但不保证；需要时宿主澄清一次走 confirm_subject）；B 根因为 W3 漏审全部候选（contract.rs:469–484 索引集合校验失败），非已证实截断；另发现缺口：本地 validator 未检查 coverage gaps⇒missing 非空（只写在提示词 224–225 行）。建议两条都不重处理、不加自动重试；结构加固另做：本地核验 gaps⇒missing（收紧接受集合，建议仅 W3 升 v5），动态 reviews 长度 schema 需远端兼容验证，暂缓。待用户决定。
- AF331 用户裁定：两条原话都不重处理，安全座椅那条不存；批准只补本地核验（W3 coverage.status=gaps 时 missing 必须非空），W3 升 v5（仅内部契约/评测录制过期，宿主协议不变）。不做动态 schema 长度与自动重试。交 Codex 实现（零调用）。
- AF332 AF331 第 1 轮修复：Codex 更新 af151 测试断言（空 gaps 整体校验失败、verdict 不评分）。重跑全套 1002/4/1：新失败 visibility_normal_cadence_snapshot_change_frequency 与 3 个 v3 HTTP 回放（发送数 6/4≠7、写入未 committed），上一轮全套均通过且两轮间仅改测试断言；单独重跑 3 次全过、可见性测试单独通过 → 判为满负载下的偶发时序问题（记为待修）。再跑一次全套作为提交门槛。
- AF333 全套重跑 1006/0/1，提交推送 W3 v5（gaps 必须有 missing）。部署待用户确认。待修：满负载下偶发失败的 4 个测试（visibility cadence、3 个 v3 HTTP 回放）。
- AF334 用户批准部署：2026-10-15 部署 7520536（W3 v5）。备份 ~/.local/share/personal-memory-backups/w3v5-20261015/（旧二进制 a0a5af9、runtime.json、DB integrity ok）；配置不变（W4 仍 sol）；隧道重启，DB v11。
- AF335 用户批准修 4 个满负载偶发失败测试。Claude 初析：visibility_normal_cadence 失败点 chat_visibility_tests.rs:275 settled() 断言消息为 "failed"——即某轮来源整理终态为 failed 而非 committed（不是单纯慢）；v3 三个 HTTP 回放表现为发送数少于 7、写入未 committed，疑同一机制：满负载下后台整理失败（猜测为阶段超时/租约/快照冲突导致 failed 而非重试），需确认是测试问题还是生产代码在高负载下的真实缺陷。交 Codex 诊断（零调用，先不改代码）。
- AF336 Codex 评审 64：未复现（沙箱内 4 份并行 cadence 480 次整理全过；2–4 需端口未跑）。纠正 Claude：第 1 项用注入传输、非 HTTP；第 2–4 项为同步整理，非等待不足；调用数少可能是 R1/R4 提前失败而非写入失败。建议先只加失败诊断（不改业务、不放宽断言、不加重试），沙箱外满负载复跑捕获具体错误码后再定修复。Claude 采纳，交 Codex 加测试诊断。
- AF337 AF336 诊断提交：沙箱外满负载全套连跑 3 遍均 1007/0/1，偶发失败未复现；诊断代码（仅失败时输出，未改业务/断言）已提交推送。偶发问题保持待查，再出现时按诊断输出定性。target/debug 54G、磁盘余约 12G，已向用户提议只删 target/debug（保留 eval-* 数据与 release），待确认。
- AF338 用户批准：删除 target/debug（54G 编译缓存）；保留 target/release 与 target/eval-* 评测数据。
- AF339 生产实测 W4=sol 通过：“我搬家搬到了杭州”→ W1/W3（DS）→ W4 gpt-6.1-sol medium（7 s，in 2670/out 132）state_change targets=[2]，依据含新旧两句原话；记录 2 阜阳 Superseded/StateChanged→新记录 12 杭州 Active，记录 1 深圳（之前）保持。部署后共 4 次 W4 sol 调用全部完成、无传输错误。观察：W3 常把新句子的一部分（如“我搬家”）或讨论中的推理列为 coverage 缺口，致来源状态 coverage_gaps（7 条）；技术讨论中的观点（GPU 调度）被作为事实保存。均未处理，待用户决定。
- AF340 用户要求处理“讨论中的观点被存成记忆”。Claude 分析：宿主说明第 10 行已要求“纯闲聊、计算、一次性指令、假设、引用…不写入”，但 ChatGPT 仍对 GPU 调度讨论传了 capture_source=true——仅靠宿主约定不可靠（方案 A 已在用且失效）。推荐方案 B：在 W1 增加规则——只抽取关于用户本人及其相关人、物、活动、偏好、安排、明确项目决定/规则的断言；任务讨论中对外部主题的论证、评价、推理（技术观点）不抽取，除非用户表述为持久的个人偏好或规则；W3 coverage 同步只把这类“个人范围”断言计为遗漏，避免缺口噪音反增。原话仍保存可检索。代价：W1 v5、W3 v6，补样本，W1/W3 录制过期，验证需真实调用（DS-Pro W1+W3 约 80 次 ≈ 2 元）。交 Codex 复核（零调用）。
- AF341 Codex 评审 65：赞成 B，需收窄“项目决定/规则”为用户明确已采用/决定采用/正在执行且能定位项目范围的约束；职业职责、真实偏好/习惯/计划/经历保留（不要求“一向”）；对外部方案的评价、反驳、推演、备选不抽；混合句逐断言。W3 coverage 同步只计范围内遗漏；仍挡不住 W1 偶发漏过（W3 verdict 不能挪用，接受残余风险）。不建议新增 speech=discussion。已存记录不自动删除。原话仍留存且可被召回。W1 v5、W3 v6；新增 12 个虚构样本；最小真实验证 24 次（W1 12 + 固定候选 W3 12），估约 ¥3.3，最坏上界约 ¥17.4。
- AF342 用户裁定：按 B 做（W1 v5、W3 v6，接受残余风险，原话仍保存）；同意 24 次真实验证（DS-Pro，虚构样本），费用上限 10 元；已存 GPU 调度讨论记录“清理掉”（Claude 先列出范围供确认再执行删除）。交 Codex 实现 B（零调用）。
- AF343 用户裁定删除三个 aiinfra 会话（aiinfra-interview-20261015 16 条、aiinfra-assessment-20261015 2 条、aiinfra-interview 1 条，共 19 条原话）及其记录 4–11。先备份 ~/.local/share/personal-memory-backups/aiinfra-forget-20261015/memory-before.sqlite3，在副本上经 MCP stdio memory_forget(source_id) 试运行确认：原话正文/记录内容/FTS 均清除，仅留 deleted 墓碑。生产：停隧道服务→执行 19 次 memory_forget 全部成功→记录 4–11 Deleted→integrity ok→重新 bootstrap 服务。
- AF344 Claude 审核 AF342：W1/W3 提示词新增范围规则与评审 65 边界一致；12 组虚构样本标签认可（01–03 讨论不抽、04–10 个人/明确项目决定保留、11 漏个人职责须 gaps、12 只漏讨论为 complete）。进入门槛：沙箱外全套→本机干跑→24 次真实调用（上限 10 元）。
- AF345 AF342 门槛：沙箱外全套 1011/0/1；本机干跑 24 槽、上限 10 元、情景 2.93 元（需显式传 DEEPSEEK_PRO_MODEL=生产接入点 ID，非密钥）。开始真实发送。
- AF346 AF342 真实验证 24 次（DS-Pro，0 传输失败，实际 ¥0.60）：范围判断全部正确——W1 01–03 讨论零候选、04–12 个人/项目决定均提取；W3 coverage 全部正确（11 检出漏提职责、12 只漏讨论判 complete）。自动分 21/24，3 个失败均为标签过严：w1_10 value 为“在青梧项目中采用离线验签”（缺“已决定”，标签必含核心过严）；w3_09 episode_value low_value vs useful（主观）；w3_11 missing 引用缺句号。代码已提交推送（门槛全套 1011/0/1）。
- AF347 用户批准部署 W1 v5/W3 v6 并放宽 3 处标签。
- AF348 部署 e89f58a（W1 v5/W3 v6）：备份 ~/.local/share/personal-memory-backups/w1v5-20261016/（旧二进制 7520536、runtime.json、DB integrity ok）；配置不变（W4 sol）；隧道重启。交 Codex 放宽 3 处 AF342 标签（零调用）。
- AF350 生产成本复盘（Claude 本地只读统计，10-11~10-15，106 次调用、26 条消息，原文未外发）：总 ¥7.06，≈¥0.27/条消息。按阶段：W1 ¥3.15（45%，均 ¥0.121/次，推理均 3648 token，low 强度；有候选 ¥0.175、零候选 ¥0.058，零候选占 12/26）；R4 ¥1.72（24%，输入 5980→11611 token 持续增长，缓存仅 43%）；W3 ¥1.35（19%，推理均 2825）；R1 ¥0.46；W4(sol) ¥0.37。0 次失败。R4 输入构成（最新一次）：instructions 16k 字符 + schema 9k（固定）+ evidence 17.8k：raw 25 条未整理原话 7.7k + records 11 条 9.8k——小范围枚举把全部记录交给 R4，raw 包含所有未提交来源（含已结束的零候选来源），随库增长线性变大。
- AF351 Codex 评审 66：R4 raw 规则比 Claude 描述更宽——可信范围内除 committed/completed_no_facts 外所有来源都进 raw（不要求词法命中、不按 R1 主体过滤），默认 32 条，records 先占字节预算；与枚举无关；零候选≠无价值，直接排除会违背 contracts 3.2/target-design 2.6 可见性。C 受预算限制非无限增长，改动需重新批准，暂缓。A 有历史退化证据（AE6 W1 minimal 6/20；AF103 W3 minimal 2/8 vs low 6/8），不能直接切；建议小样本筛查：W1 12 + W3 8（含 AF103 误放行/误拒场景）minimal，配同版本 low 对照共 40 次，约 ¥5–6。B 先零调用分析。
- AF349 AF348 标签修订：离线重评 24/24、零发送；沙箱外全套 1012 passed, 0 failed, 2 ignored；提交推送。
- AF352 用户选择 A：W1/W3 minimal 小样本筛查，40 次（minimal 与现行 low 各 20：W1 12 + W3 8），上限 10 元。标签提交 f907bb2（全套 1012/0/2）。交 Codex 准备 campaign（零调用）。
- AF353 Claude 审核 AF352 选题：W1 12 + W3 8；4 个 AF103 移植 W3 主体样本标签认可（01/02 明确先行语 supported、03 多主体歧义 insufficient、04 无先行语不默认 self insufficient）。门槛：全套→干跑→发送 40 次（上限 10 元）。
- AF354 AF352 筛查结果（40 次，0 传输失败，实际 ¥1.05；门槛全套 1016/0/2）：minimal W1 8/12、W3 4/8；low W1 11/12、W3 7/8。minimal 的退化：W1 把“负责X项目”误作 works_at、计划丢“考虑”、历史明确的“他”未绑定老王；W3 漏报个人职责缺口、明确先行语误判 insufficient、丢失条件却判 supported（危险的误放行）。合成样本单次费用 minimal W1 ¥0.010/W3 ¥0.0075 vs low ¥0.033/¥0.061。两档都失败 af352_w3_01（判 contradicted），疑标签或输入问题，待查。结论：不切 minimal，W1/W3 保持 low。工具已提交推送。
- AF355 用户裁定：第 11 项提交 AGENTS.md、CLAUDE.md、.ai/*.md（仓库私有，含部分真实个人信息，用户知情），.ai/log（145M Codex 原始日志）加入 .gitignore 留本地；第 5 项 af352_w3_01 两档判 contradicted 系 low 判 contradicted[time]（“刚才”未进候选 times）、minimal 判 value——题目干扰项；裁定去掉 af352_w3_01–04 输入中的“刚才”，旧录制标过期不重跑，零调用。交 Codex 修样本。
- AF356 AF355 样本修订提交（全套 1017 passed, 0 failed, 2 ignored）；第 11 项：.gitignore 增加 /.ai/log/，提交 AGENTS.md、CLAUDE.md 与 .ai/ 下文档。
- AF357 用户提出 Inspector 需按最新流程展示。Claude 只读核查：inspector 最后更新于 M1 收尾（42d021c，10-06）；用生产数据 --dump：llm_calls=0（模型调用完全不显示），runs 27 个但 spans 只有 source_receipt，memories 只 4 条且为旧 Memory 形状（无 CurrentRecordView、update judged/pending、历史链、关系），trace_status=privacy_filtered（存在 source 删除墓碑后整体保守隐藏旧 Trace）。代码中不识别 model_start/model_end/model_check 的阶段内容、W1–W5 候选/核验/决策、R1–R5 计划/检索诊断/证据包/判决、statement_snapshot、model_transport_error、来源整理状态（coverage_gaps/needs_rejudge 等）、费用。结论：需按 W0–W5/R1–R5 重做展示。交 Codex 复核与方案（零调用）。
- AF358 Codex 评审 67：纠正——inspector 已解析 model_start/end，llm_calls=0 是 privacy_filtered 整体跳过所致；另 trace 总量 >16 MiB 直接拒绝（生产单文件 102MB）；retrieval 事件新旧字段不兼容会报解析错误；后台写入事件无 run_start 会被拒。隐私：跨来源派生内容（W4 请求、召回结果、事务快照）可能含已删来源，不能简单按主来源过滤，需服务端按完整依赖集合过滤，旧 trace 缺依赖则继续隐藏正文。建议增量改造分 4 期：①当前 DB 视图（来源状态、拒收/held 原因、Current/Legacy、update/history/relations，复用生产纯投影如 w4::record_view，只读不迁移）+ 明确 privacy_filtered/兼容错误提示；②调用与时间线兼容；③最小运行时埋点与安全依赖关联；④成本与大文件流式浏览。
- AF359 用户裁定：Inspector 四期全做，按 ①→②→③→④ 顺序逐期实现、复核、测试、提交；③ 涉及运行时埋点，部署前另询。开始 ①（零调用，只改 inspector）。
- AF360 Inspector ① 复核（生产只读 dump，DB 前后 sha 不变）：因存在删除墓碑，当前实现隐藏全部记录（memories=0）与全部来源正文（8 条均 NO TEXT），页面实际不可用。Claude 判断 DB 当前视图的依赖可逐项判定：来源自身原文只依赖自身；request.history 为未链接原文字符串，依赖不可证→隐藏；W1/W3 输出引用已核验为当前消息精确片段→随来源可见；W4 决策 basis 与记录 evidence/history 链/relations 均带 source_id/记录 ID→全部未删除才显示。交 Codex 第 1 轮修复。
- AF361 Inspector ① 第 1 轮修复复核（生产只读）：8 条来源正文与候选回执可见，4 条记录可见（1 深圳、2 阜阳 superseded、3 游泳课、12 杭州），被删 AI infra 正文无泄漏。残留：已删除来源的接收回执仍显示 session_id（aiinfra-interview-20261015 等）与时间——会话名可泄露主题。交 Codex 第 2 轮（最后一轮）修复：已删除来源的回执只保留“已删除”状态，不显示 session_id/message_id 等标识。
- AF362 Inspector ① 验收：第 2 轮修复后生产只读 dump——12 来源/4 记录/8 runs，已删除来源只留删除状态+内部 ID+时间，泄漏检查无命中；--export-trace 有墓碑时拒绝导出。根仓全套 1017/0/2，pmr-inspector 13/0。提交推送。
