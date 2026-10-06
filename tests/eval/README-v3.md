# 默认新路径端到端评测与历史验收

缺省 `tests/eval.rs` 只运行 v3，未设置 EVAL_RECORD 时严格离线回放已有 AC2 录制。
旧运行器已删除，EVAL_PIPELINE=legacy 明确拒绝。按 AD1，expect_common 绝对评分为主，
0517000 原始报告和纯评分适配器保留作历史参考，不作上线门槛。
新模式以 AC2 配置跑生产 `ChatMemoryService`：W1/W4 Pro medium、W3 Pro minimal、
R1 Pro minimal、R4 Pro 计算类 low / 其余 minimal，UTC+8；不复制提示词，也不把模型 ID 加入模型可见 input。
输出预算 W1=32768、W3/W4=16384（生产阶段常量），R1=4096、R4=16384；禁用模型修复。

```sh
# 干跑：不读取凭据、不绑定端口、不创建录制；全部35例/63题。
EVAL_PIPELINE=v3 EVAL_JOBS=8 EVAL_DRY_RUN=1 cargo test --test eval resident_eval_suite -- --exact --nocapture

# 默认严格回放：报告到 target/eval-replay/<配置标签>/run-replay.json；缺口不计通过。
EVAL_PIPELINE=v3 cargo test --test eval resident_eval_suite -- --exact --nocapture

# 新路径绝对评分；旧基线在附录。不存在的 run 文件明确报告未录制。
python3 tests/eval/scoring/compare.py \
  --run tests/eval/end_to_end/ac2-pro-medium-pro-minimal-pro-minimal-r4-compute-low/run.json \
  --output /tmp/eval-v3-common

# 相关离线测试（端口测试需支持本机 loopback 的环境）。
cargo test --test eval v3_ -- --skip v3_production_http_roundtrip_uses_all_five_stages \
  --skip v3_nonblocking_listener_reads_delayed_request_without_panicking
python3 -m unittest discover -s tests/eval/scoring -v
```

每条消息用 `memory_remember` 独立保存原文后显式 `enrich_source`；该调用同步执行生产
W1 → W2 → W3 → assemble → 条件性 W4 → validate/plan/commit/finish。
返回终态或失败回执后才进入下一步，不启动 worker、不续跑失败阶段。这样固定来源顺序，
录制/回放可重复；worker 并发与恢复由第4项验证。之后 `memory_recall` 走生产 R1/R3/R4/R5。
整理失败时仍继续考察原文可见性；缺失/过期录制造成未知入库状态，则后续问题记未录制。

新模式按用例并行，`EVAL_JOBS` 为正整数，缺省8，最多同时运行所选用例数。
每个用例独立 SQLite 与 ChatMemoryService；用例内严格串行。完成顺序不影响汇总顺序。
可用原 `EVAL_CASES` 前缀或 quick 选子集。
消息时钟为 `CLOCK + index*60`，题目为 `CLOCK + (after-1)*60 + 30`，用例显式时间优先。
评分器独立核对可信时间，不能以模型输出或导出文件中的时间重定义问题。

录制单独存入 `tests/eval/end_to_end/ac2-pro-medium-pro-minimal-pro-minimal-r4-compute-low/<case>.json`。
首轮 AC1 的录制保留在原目录，不移入 AC2；W3 模型/强度及 W3/R1/R4 提示词变化均使原请求指纹过期。
`EVAL_V3_DIR` 指定录制输入目录；`EVAL_V3_OUTPUT` 可显式指定运行输出，回放请用 target/ 或临时目录。真实模式缺省输入目录/run.json；零调用回放缺省 `target/eval-replay/<配置标签>/run-replay.json`，分用例文件也在该目录。回放代理错误/发送诊断日志位于同一 target 目录的 runs/，不会写入录制输入目录；真实发送日志仍在录制目录 runs/，落盘与续录规则不变。
每次真实调用完成（含 HTTP/传输失败）立即原子写入 `<case>.json`，文件同步后才向生产客户端返回。
每个用例完成即独立原子写入 `<输出名>.cases/<case>.json`（真实模式缺省 `run.cases/`，回放为 `run-replay.cases/`），
所有用例结束后写汇总；迟到响应收齐后仅补齐诊断，不改生产运行当时的结果。
录制文件按用例聚合，逐调用写入；不同用例不共用临时文件或运行时。
监听端口非阻塞以支持停止，accept 的连接显式改为阻塞；代理解析、读写与落盘错误返回 503，
同时写入发送日志的 proxy_errors。客户端断开不撤回已落盘响应。
目录内 configuration 必须匹配，不能混用配置标签。原 cases/*.json 永不写入。
请求指纹是完整生产请求的 SHA-256，包含提示词、schema、时钟、预算、模型路由和推理强度；
唯一规范化项是结构中的运行时来源 ID ↔ 稳定消息 ID。原文/值/引用文字绝不替换，
响应的结构化来源 ID 回放时还原。即使缺省模式也严格拒绝过期录制，不借用旧基线输出。
录制保存规范化请求、完整响应、HTTP 状态、usage、elapsed_ms、incomplete_details 与传输错误。
已录制失败也回放，不自动重试；不完整 JSON 保持原样用于诊断。


## 默认离线验收的两项独立检查

1. **当前严格回放**：生产请求与指纹完全匹配才复用。235次复用/59缺口/0发送的缺口是58过期+1缺录制，63题均为未录制，不计通过；不从历史结果补齐。完整补录需要用户另行批准预算，不能在默认测试中发送。
2. **已接受历史重算**：从 AC2 run.json、ac2-af-targeted 和 af14-targeted 各自的 run.json/target-plan.json、当前案例标注及 confirmed 复核台账，调用同一个 compare.merge_runs/run 重算。自动断言63题为59通过/4失败/0待复核，失败集合固定为 unresolved_conflict/conflict_q2 与 lme_7a87bd0c/q、lme_a2f3aa27/q、lme_59524333/q。前者仍会在冲突说明之外给出唯一尺寸；三道LME是已知提取/计划/判断局限，不再为提高分数放宽语义。

历史检查在普通 `cargo test` 的 historical_common_score_is_recomputed_offline 中默认运行，也纳入 Python unittest；不是当前请求的严格回放，更不是63题全量重测。最小归档来源见 [end_to_end/README.md](end_to_end/README.md)。

```sh
# 不需要端口、密钥或网络，不写报告。
cargo test --test eval historical_common_score_is_recomputed_offline -- --exact --nocapture
python3 -m unittest discover -s tests/eval/scoring -p test_m1_history.py -v
# 端口受限时的严格回放入口；报告显式写临时目录。
EVAL_V3_UNIT_OUTPUT=/tmp/m1-current-replay.json cargo test --lib recorded_full_e2e_replay_never_sends -- --nocapture
python3 tests/eval/scoring/compare.py --run /tmp/m1-current-replay.json --output /tmp/m1-current-report
```

下面各带日期/阶段名的测量和估算保留原口径，仅是历史记录，不是新的录制授权或当前性能保证。

## 后续获批真实录制时

本次任务零真实调用。以下命令只是用法，运行前仍须用户批准调用额度并配置真实端点/密钥：

```sh
EVAL_PIPELINE=v3 EVAL_JOBS=8 EVAL_RECORD=703 cargo test --test eval resident_eval_suite -- --exact --nocapture
```

`EVAL_RECORD` 是整批额度：小于所选用例的保守上限整批拒绝，预检在读取密钥或启动端口前。
预检不扣除已有录制，因为请求与分支只有运行时才能严格核对；实际仅缺失/过期请求消耗额度。
额度为所有线程共享的全局计数器，每次发送前在锁内预留一次；耗尽即拒绝本次发送并记录原因，
失败也占额度，回放不占额度。不对各用例分别授予完整 `EVAL_RECORD`。
每次运行打印独立的 `runs/<run_id>.json` 路径；每次发送前把 started 记录原子写入并同步，
计数落盘失败则不发送。响应录制成功后标为 recorded。启动时先写 run.json 入口，
因此即使用例尚未完成就中断，也能生成调用报告。日志不存密钥或认证头。
中断后使用相同录制目录重跑：严格按原请求指纹复用已落盘响应，不重复发送；
只为缺失/过期响应消耗本次新额度，不自动重试已录制失败。重跑真实调用仍须批准额度。
已发送但未完成/未落盘的项单列；本地落盘与远端接收不能原子完成，started 项保守计入，
可能在途或已计费，不能据此证明远端收到或具体费用。

```sh
# 中断后：run.json 自动定位本次发送日志；未完成题仍为未录制。
python3 tests/eval/scoring/compare.py --run <run.json> --output /tmp/eval-interrupted
# run.json 缺失时，指定启动时打印的日志；不要混用其他运行的问答结果。
python3 tests/eval/scoring/compare.py --send-log <runs/run_id.json> --output /tmp/eval-interrupted
```

报告还会核对单次录制的 send_run/send_index/请求指纹，恢复“响应已落盘、日志终态未写”的窗口。
W1≤204、W3≤204、W4≤169、R1≤63、R4≤63，合计≤703（首条消息的空库不需要W4）。
W4/R4“预计”是有效候选且需要判定的容量规划情景，不是从标签推断的质量/分支预测；实际可能为零。
干跑按 query_meta.operation 标注预计 R4 计算类7次、其余56次；实际调用只取已核验的 R1 plan.op（count/sum/elapsed/compare 用 low，其余或 R1 未理解用 minimal），总上限仍703。新的独立配置目录含 r4-compute-low，旧 AC2 配置目录不混用。干跑耗时列保留混合批次代理：W1/W4 medium；W3 用 AE9 Pro minimal 固定输入23次实测均值3.8s，R1 minimal p50、R4 按分流使用 AE14 low 8次实测中位84.4s（含1次180s超时）与 minimal 4.5s；计算类 low 本地超时240s，常规R4仍180s，无重试。
W1/W4 混合批次和 W3 失败固定输入都是代理；情景不能保证 AC2 墙钟时间，未计并发限流、排队与输出差异。
各阶段 token 估算见下述脚本，使用首轮实际 usage（含 incomplete），不是费用或 token 上界。
脚本保留旧估算，并另列 AE10 分流估算：W3 输出采用 AE9 Pro minimal 固定集23次均值169.09 token（偏长失败输入），R4 low 输出采用首轮40次均值2895.5，其余用首轮 minimal 均值278.10；输入/W1/W4量沿用首轮代理，不是费用或 token 上界。AE15 最新估算单列：low8次中7次已知 usage 输出均值5164.71 token，另1次未知不计零；中位延迟84.4s。旧AE10估算保留作历史参考，干跑使用最新分组指标，不能声称全部符合15秒前台目标。

报告复用 `adapters.py`、`trial.evaluate` 与同一复核台账，主报告给出新路径的绝对分：
通过 / 失败 / 待复核 / 未录制，按用例、逐题展开；已有失败或缺失必需项归为失败，
没有已知失败但仍有语义待确认的题归为待复核。判分规则、分母、复核键保持 common.v1。
新版仅对 completed 题计分，未录制不进入判分项分母。
按用例和 W1/W3/W4/R1/R4 展示本轮请求、真实发送、回放、未录制数及耗时，
保存 HTTP/传输错误、incomplete_details.reason、usage 和运行时失败/降级回执。
当前处理耗时与历史录制耗时分列；回放 usage 是历史账单，不是本轮费用。
旧基线、逐项转移与退步移入附录，旧参考缺失也不阻断绝对评分。
新版不套用旧入库标签（如冲突成员禁止入库的旧规则）；27个LongMemEval用例缺逐条入库标注，
报告不能回答漏提/错误更新质量。语义待复核不计最终通过，评分报告不替代用户验收。

## 全量验收 A：回放与逐次诊断

保留原 live run.json、发送日志和报告。回放默认在 target/eval-replay/<配置标签>/ 另存 run-replay.json；显式输出路径也不得覆盖 real_calls>0 的 live 文件。报告标题标注“回放”，回放处理耗时不代表在线延迟，历史 usage 不表示本轮费用。

生产调用旁只采集诊断元数据（阶段、物理调用 ID、本地超时、结果、解码/核验阶段与具体失败规则），不改变业务回执或冻结契约。报告逐次同时展示远端 completed/incomplete/HTTP 与本地 completed/timeout/decode_failed/validation_failed/output_limit 等结果。代理等待并收齐迟到响应后，仍保留调用方当时的 timeout。旧 live 文件没有本地诊断时显示 unknown，不凭远端 completed 推定本地成功。显式回放拒绝绑定含真实发送项的发送日志，避免混用旧账单。

端口受限环境可用库测试的现有注入传输，复用同一个 v3_core.rs 的生产用例驱动、录制加载和严格指纹检查：

```sh
EVAL_V3_UNIT_OUTPUT=/tmp/eval-A-replay.json cargo test --lib recorded_full_e2e_replay_never_sends -- --nocapture
python3 tests/eval/scoring/compare.py --run /tmp/eval-A-replay.json --output /tmp/eval-A-report
```

该入口只编译到测试，生产不读夹具；真实 SQLite、五阶段请求与 HTTP 回放走相同业务路径。传输只返回指纹完全匹配的现有响应，缺失/过期请求记 not_recorded，发送闭包一旦被调用测试立即失败。上游修复或 AC2 请求变化时，不能借用旧输出评分；需另行批准重录。当前严格回放为235次复用、59个缺口（58过期、1缺录制）、0发送；63题均未形成完整当前结果，不计通过。这项回放不模拟历史等待时间；不能代替真实 HTTP/MCP 端到端测试。

## B：失败阶段固定输入小集

`fixed_inputs/inputs.json` 冻结首轮 live 的 W3 失败输入23条、R4失败输入10条，
由 `fixed_inputs/prepare.py` 按 run.json 失败回执与末次实际阶段提取；每条绑定原录制的完整 SHA-256。
W3 的14条 incomplete 和9条解码/核验失败、R4的10条核验失败均有无端口回归。
模型只读取原生产 payload；来源全文仅供 R4 原有旧方依据补入/核验，来源/评分标签及 provenance 不进入请求。
小集不重新运行写入、检索或 R1；仅验证完成响应、W3逐项核验与 R4结构/引用/计算核验，
报告 `scoring_scope=contract_only`，未标注语义为 pending。契约通过不能等同端到端答案正确，
也不能据空 facts 或全 insufficient 推断质量改善；新增语义标签须另行复核。

```sh
# 33次潜在调用；无需凭据、端口，不写录制。W3 Pro minimal、R4 按冻结R1计划分流。
EVAL_FIXED_INPUTS=1 EVAL_BASE_URL=https://ark.cn-beijing.volces.com/api/v3 EVAL_DRY_RUN=1 cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
# 单选 W3=23 或 R4=10；可复用 EVAL_CASES 前缀与 EVAL_REPEAT。
EVAL_FIXED_INPUTS=1 EVAL_STAGES=r4 EVAL_DRY_RUN=1 cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
# 不设EVAL_RECORD只出离线报告，不发送；缺录制/过期不当作模型失败。
EVAL_FIXED_INPUTS=1 EVAL_BASE_URL=https://ark.cn-beijing.volces.com/api/v3 cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
# 两份token估算；只读原录制，输出到stdout。
python3 tests/eval/fixed_inputs/prepare.py
# 相关回归（不需要端口）。
cargo test --test write_stage_eval fixed_input
```

固定集按冻结 input.plan.plan.op 分为8条计算类 low、2条非计算类 minimal；不重新调用 R1，不根据 R4 输出选强度。与生产复用 RecallModels::r4_effort；EVAL_REASONING_EFFORT 仅覆盖常规强度。固定集录制存入 `fixed_inputs/recordings/ac2-fixed-pro-minimal/reasoning-{low,minimal}/repeat-N/`；严格匹配原端点/模型/请求/预算时，既有W3 23条与非计算类R4 2条有效，8条计算类 minimal 录制不得用于 low 请求。
复用已有阶段录制的每次原子落盘、失败保留、整批额度预检、EVAL_GATE显式判失败及重放。
固定集的请求指纹使用完整 SHA-256，含快照/时钟/提示词/schema/路由/强度/输出上限；
不沿用旧阶段样本的时钟字段剔除。旧失败响应只用于回归诊断，不作为新请求录制。
后续真实录制仍须用户批准；获批后加 `EVAL_RECORD=33` 与既有 `EVAL_BASE_URL`/`EVAL_API_KEY`。
AE10 时固定集潜在33次，严格复用上述25条后只需8次新low调用；AE14 已录完这8次，当前33条严格回放均有效，新增发送0；单独小集额度可为8，所有录制缺失时仍需33。新8次估算输入46,291 / 输出23,164 token、串行约334秒（单次p50相加的情景）。固定集33次情景输入120,217 / 输出27,481 token、串行约429秒；全量703次情景输入约2.35M / 输出2.14M token（R4阶段预计约544秒）。实际分支、长消息、限流与排队会改变用量/延迟。小集8次加全量保守上限703，共711次需授权；原完整小集加全量理论上限736不变。

## AE15：record_basis 机械补入与最新估算

R4 对 fact 明确列出的包内 record_ids，复用旧方依据组装边界补入一条真实、逐字存在的已核准引用；不推断记录ID，不补新方依据或操作数，完整核验及删除保护保留。固定小集的 raw/final 各自评分，引用补入不能证明未标注语义正确。模型请求完全相同，现有录制仍按完整指纹核验；180秒超时录制保持失败，改为240秒不会离线恢复它，也不会自动重发。

`prepare.py` 的 `ae15` 给出最新全量token估算，保留 `ae10` 历史数。最新low测量是失败小集8条：7条usage已知、1条未知；未知项不当零，240秒下的用量/完成率未经测量。全量最多703次（R4预计7 low/56 minimal）；是否发起重录及额度须再次批准。

固定小集真实传输也仅对已核验计算计划且实际low的R4请求设240秒，其余单阶段传输仍180秒。旧“25条minimal可复用/8条low缺失”测试改为仅复制minimal录制到临时目录模拟原条件，原断言不变；真实33条现状另有零发送回归。

## AF1–AF4 定点重录准备

`expect_common.status` 可为单一状态或非空可接受集合；AF2/AF3 仅调整四题的状态及说明，事实、来源、限定语仍按原标签评分。标注变化使复核键自然失效，须重新确认，不能迁移旧结论。报告另列 `failure_category=call_failure`（HTTP、账户、传输、超时），不计入模型解码/核验失败；HTTP 失败优先于后续通用降级码。

```sh
# 零调用，按 AC2 已知 usage 估算 C 十题、R1 两题、欠费四题，合并去重。
python3 tests/eval/targeted_plan.py
# 使用上一步输出的 EVAL_QUESTIONS；只保留所选题及其前置消息，不插入夹具答案。
EVAL_PIPELINE=v3 EVAL_QUESTIONS='<case/question,...>' EVAL_DRY_RUN=1 \
  cargo test --test eval resident_eval_suite -- --exact --nocapture
```

W4 已升为 w4.v3；所有请求仍严格匹配完整指纹。旧 W4 不能复用，新状态的 R4 包也须重建；W1/W3/R1 仅在请求完全一致时可复用。计划列出“不复用上限”和按阶段的重录容量，后者使用历史分支及失败数，不是精确缺录制数。未知 usage 使用同阶段已知均值，不能记零；金额仅按 AC2 约50元和 token 合计比例粗折，不是服务商报价或费用上限。正式额度预检仍按不复用上限，不把估算当授权。

已录制失败默认仍回放，不自动重试。获批后先确认余额充足，将 AC2 录制复制到独立目录，只有在副本中明确删去待重录的失败调用及其后续阶段，才会重新发送该请求；成功前缀完整保留，每次复用仍检查指纹。空目录不具备计划中的复用条件。原失败审计保留不动。全量旧运行输出的离线重算只校准评分，不表示 AF1 新运行时已取得对应质量；修改输入后不得借用旧输出确认完整性或当前值。

## AF5：从 AC2 准备独立续录目录

```sh
# 零调用；目标目录必须不存在。只取16题、10用例及前置消息。
python3 tests/eval/targeted_plan.py prepare --output .ai/log/af-targeted

# 零调用；按 AC2 实测均值估算 token，预检本批额度。
python3 tests/eval/targeted_plan.py dry-run --directory .ai/log/af-targeted --eval-record 58
EVAL_PIPELINE=v3 EVAL_V3_DIR=.ai/log/af-targeted EVAL_RECORD=58 EVAL_DRY_RUN=1 \
  cargo test --test eval resident_eval_suite -- --exact --nocapture

# 无端口的严格回放：使用生产请求，缺录制停止当前用例，绝不发送。
EVAL_V3_DIR=.ai/log/af-targeted EVAL_V3_UNIT_OUTPUT=.ai/log/af-frontier.json \
  cargo test --lib openai::eval_replay_tests::recorded_full_e2e_replay_never_sends -- --exact --nocapture

# 实际续录另行授权后，去掉 EVAL_DRY_RUN=1，使用同一目录/额度及已授权凭据。
# 完成后合并63题；新报告路径必须不存在，16题来自新运行，其余47题沿用AC2。
python3 tests/eval/scoring/compare.py \
  --run .ai/log/af-targeted/run.json --target-plan .ai/log/af-targeted/target-plan.json \
  --base-run tests/eval/end_to_end/ac2-pro-medium-pro-minimal-pro-minimal-r4-compute-low/run.json \
  --output .ai/log/af-targeted-merged
```

生成命令保留94条成功前缀，移除44条录制（含 HTTP 403、超时/解码/核验失败及其后续阶段，全部旧 W4 与选中 R4）；原目录逐字不动。新目录保留原请求指纹，不重写请求或响应；`target-plan.json` 绑定题目、操作、成功前缀指纹和原 AC2 运行摘要。失败是否完成以本地诊断为准，远端晚到的 completed 响应不能掩盖本地超时。

固定预留上限为58：W1=2、W3=4、W4=32、R1=4、R4=16；仅移除确定不会调用的分支（保留 W1 无候选、W3 无 supported、R1 无需 judge）。这是未决模型输出下的发送上限，不能零调用精确预测最终发送数。无端口回放另给当前请求已验证的命中/缺口数，不用旧 W4 输出模拟未来新状态。token 是相应预留阶段的 AC2 已知 usage 均值，未知 usage 不当零；R4 按 low/minimal 各自均值，缺 R1 时仅用 query_meta 作路由容量假设，实际仍由获核验的 R1 决定。

`EVAL_V3_DIR` 内存在定点计划时自动选择其中题目，并在读凭据/开端口前校验全部配置、题目/操作范围、保留前缀及预留上限。`EVAL_RECORD<58` 整批拒绝；额度足够才允许进入运行。每个预留阶段至多新发一次，仍使用发送前落盘与全局硬限额。保留前缀的实际请求指纹改变、出现未预留阶段时拒绝补发并停止后续发送；并发中已开始的调用无法撤销。中断后的新响应按完整指纹续跑复用，失败响应仍不自动重试。

合并报告逐题标出 `targeted/ac2` 与运行文件路径。定点题缺结果记未录制，不回落 AC2；本轮发送计数不累加 AC2 的583次历史调用。合并分数是混合版本的63题视图，不表示全量重测；原 AC2 报告不得覆盖。

M1.5 的 recall.v4 评分、虚构 prepare MVP 和固定 W1 low/medium 对照使用独立入口与目录，
见 [m15/README.md](m15/README.md)。两组各预留最多 24 次；默认历史评测及 59/4 核验不变。
