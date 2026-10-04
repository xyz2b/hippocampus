# 新路径端到端评测（M1 全量验收准备）

本段不切默认、不删旧代码；缺省 `tests/eval.rs` 仍走旧路径。按 AD1，新路径以
`expect_common` 绝对评分验收，0517000 旧基线只作附录参考，不作为上线门槛。
新模式以 AC1 配置跑生产 `ChatMemoryService`：W1/W4 Pro medium、W3 Flash medium、
R1/R4 Pro minimal，UTC+8；不复制提示词，也不把模型 ID 加入模型可见 input。
输出预算 W1=32768、W3/W4=16384（生产阶段常量），R1=4096、R4=16384；禁用模型修复。

```sh
# 干跑：不读取凭据、不绑定端口、不创建录制；全部35例/63题。
EVAL_PIPELINE=v3 EVAL_JOBS=8 EVAL_DRY_RUN=1 cargo test --test eval resident_eval_suite -- --exact --nocapture

# 空录制回放：产生 run.json；缺录制为“未录制”，不计通过、不判模型失败。
EVAL_PIPELINE=v3 cargo test --test eval resident_eval_suite -- --exact --nocapture

# 新路径绝对评分；旧基线在附录。不存在的 run 文件明确报告未录制。
python3 tests/eval/scoring/compare.py \
  --run tests/eval/end_to_end/ac1-pro-medium-flash-medium-pro-minimal/run.json \
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

录制单独存入 `tests/eval/end_to_end/ac1-pro-medium-flash-medium-pro-minimal/<case>.json`。
`EVAL_V3_DIR` 可指定其他配置目录，`EVAL_V3_OUTPUT` 可指定运行输出（缺省该目录/run.json）。
每次真实调用完成（含 HTTP/传输失败）立即原子写入 `<case>.json`，文件同步后才向生产客户端返回。
每个用例完成即独立原子写入 `<输出名>.cases/<case>.json`（缺省 `run.cases/`），
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
W1/W3/W4缺独立耗时，只以历史混合批次均值作代理；R1/R4用minimal p50。
串行情景约6.94小时；按相同FIFO队列模拟，8个worker预计墙钟约54.4分钟。
这不是实际耗时上界，未计并发限流、网络排队和模型输出差异。录制的单次 elapsed_ms 供下一轮替代代理估计。

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
