# common.v1 评分与历史验收重算

依据 `docs/architecture/common-scoring-view.md`。全部离线，只用 Python 标准库，不调用模型。

```
python3 -m unittest discover -s tests/eval/scoring -v   # 审阅清单 1–3 的测试
python3 tests/eval/scoring/trial.py                      # 四题新旧对照：reports/trial-4q.{md,json}
python3 tests/eval/scoring/baseline.py                   # 全部 63 题的 0517000 基线：reports/baseline-0517000.{md,json}
python3 tests/eval/scoring/compare.py --run <v3-run.json> --output /tmp/eval-v3-common
```

| 文件 | 作用 |
|---|---|
| `adapters.py` | 旧 answer_contract（0517000 归档）与 recall.v3 → 共同视图。只收问题、上下文、query_meta、原始输出和 ID 映射，拿不到标注；运行时 ID 映射为稳定引用 |
| `scoring.py` | 按 `expect_common` 生成判分项：自动通过 / 自动判错 / 待复核 / 缺失 / 不可测，另列额外结论 |
| `ledger.py` | 复核台账键（第 6 节各组成部分的哈希）与最终结果；只有 `confirmed` 条目计入通过 |
| `trial.py` | 四题试跑、反例、新旧转移与退步、报告 |
| `baseline.py` | 全部 63 题只跑旧版：原始评分（eval.rs 口径的移植）、共同评分与覆盖缺口并列 |
| `compare.py` | expect_common 新路径绝对评分为主，按用例/阶段列调用、耗时、诊断；0517000 对照在附录，缺录制报告“未录制” |
| `review_ledger.json` | 复核台账。`proposed` 为草拟，改为 `confirmed`（填 reviewer）或改判后才生效 |
| `fixtures/recall_v3/` | 四题的手写 recall.v3 正确输出夹具（不是实现的运行结果） |
| `fixtures/counterexamples/` | 反例（真实原始格式）；`pairs.json` 为旧版弃答 / 新版给错的成对反例 |
| `fixtures/probes.json` | 条件扩大探针题（不属于 63 题） |

旧答案整串比较只去掉首尾空白和句末句号（。.．）；问号、引号等有语义的符号保留。整串推定还要求版本与问题绑定正确、引用全部可映射且有效。

标注写在 `tests/eval/cases/*.json` 对应问题的 `query_meta` 与 `expect_common` 中，原 `expect` 不变。

新路径运行/干跑/录制说明见 [README-v3.md](../README-v3.md)。比较工具只读取实际运行输出，
不把四题手写夹具当作全量真实结果；缺录制不计新版通过，语义待复核仍须台账确认。
按 AD1，旧基线与新旧转移只作参考，不是新路径验收门槛；缺旧参考仍产出绝对评分。

复核流程：跑 `trial.py`，报告中“待复核”项带台账键；在 `review_ledger.json` 中按键写入或确认结论。标注、问题、输出、额外结论或语义状态任一变化，键随之改变，旧结论不再适用；`test_ledger_covers_every_pending_item` 会指出失效的键。

全量验收 A 的逐次诊断见报告“逐次远端 / 本地结果”：远端状态与本地 timeout、解码/核验失败、输出超限分列。回放明确标注，不表示线上质量或延迟；旧 live 缺本地诊断时显示 unknown。新回放默认写入 target/eval-replay/<配置标签>/run-replay.json，保留 live run.json 和原报告。

AF2/AF3：`expect_common.status` 可为单值或非空集合（common.v1 使用 answer / insufficient / clarify / conflict）。只放宽该字段，事实、来源、方向、当前资格及语义复核照常执行；标签变更使对应复核键失效。`compare.py` 单列 HTTP/账户/传输/超时为调用失败（非模型），不计入模型解码/核验失败。

## M1 已接受历史（AF27）

`test_m1_history.py` 默认随 unittest 运行，普通 cargo test 也通过 Rust 入口执行此检查。
从 AC2、两轮定点的原始 run.json/target-plan.json 和当前 confirmed 复核台账重算，不读取已有最终报告作为答案。校验两份计划均绑定原 AC2 的 SHA-256，断言63题59通过/4失败/0待复核以及准确失败集合。最小数据路径及来源见 [历史归档](../end_to_end/README.md)。

当前严格回放仍为235复用/59缺口/0发送，63题未录制，不计通过；历史59/4不能补齐当前缺口。补录须另行授权。

## AF255 问题级人工确认

历史 `review_ledger.json` 及 59/4 离线检查保持不变。AF255 使用独立 `af255-review-ledger.json`（可通过 `compare.py --question-ledger PATH` 指定）。报告 JSON 中 `af255.review_queue` 列出完整 binding：题目与现有期望、实际模型请求/原始响应、写入快照、完整最终宿主输出；key 是规范 JSON 的 SHA-256，不只绑定输出摘要。结构化来源 ID 规范化，原话不替换。同 binding 复用判断；输入、标签或任意最终输出字段变化都产生新 key。

```json
{"version":"eval.e2e.question-review@1","entries":{
  "sha256:<完整key>":{"binding":"复制该队列项的完整binding对象",
    "decision":"accepted","reviewer":"姓名","reason":"核对所有事实、额外输出、极性、条件、时间、人物及来源"}
}}
```

`binding` 必须为队列中的完整对象，上例字符串仅为填写提示。`decision` 为 accepted/rejected；可附 `category=R1` 标注已人工确认的规划错误。自动字段正确也先 pending_review，只有已确认的 accepted 才 human_confirmed；已知自动失败/缺项和机械失败不能人工豁免。原 common.v1 通过/失败仍单列，禁止把人工确认率与自动通过率混算。机械失败保留队列和具体规则。新台账不预填通过项。

子集报告分开15题评分分母与48题未选择，以及未录制；对比旧基线只作参考。R1 operation 与标注不符仅是待核查诊断，代码不据此推断语义错误。R4 延迟用原始远端录制毫秒数，按计算/非计算分列；不把回放墙钟当线上速度。
