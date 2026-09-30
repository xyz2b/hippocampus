# common.v1 共同判分视图（四题试跑）

依据 `docs/architecture/common-scoring-view.md`。全部离线，只用 Python 标准库，不调用模型。

```
python3 -m unittest discover -s tests/eval/scoring -v   # 审阅清单 1–3 的测试
python3 tests/eval/scoring/trial.py                      # 生成 reports/trial-4q.{md,json}
```

| 文件 | 作用 |
|---|---|
| `adapters.py` | 旧 answer_contract（0517000 归档）与 recall.v3 → 共同视图。只收问题、上下文、query_meta、原始输出和 ID 映射，拿不到标注；运行时 ID 映射为稳定引用 |
| `scoring.py` | 按 `expect_common` 生成判分项：自动通过 / 自动判错 / 待复核 / 缺失 / 不可测，另列额外结论 |
| `ledger.py` | 复核台账键（第 6 节各组成部分的哈希）与最终结果；只有 `confirmed` 条目计入通过 |
| `trial.py` | 四题试跑、反例、新旧转移与退步、报告 |
| `review_ledger.json` | 复核台账。`proposed` 为草拟，改为 `confirmed`（填 reviewer）或改判后才生效 |
| `fixtures/recall_v3/` | 四题的手写 recall.v3 正确输出夹具（不是实现的运行结果） |
| `fixtures/counterexamples/` | 反例（真实原始格式）；`pairs.json` 为旧版弃答 / 新版给错的成对反例 |
| `fixtures/probes.json` | 条件扩大探针题（不属于 63 题） |

旧答案整串比较只去掉首尾空白和句末句号（。.．）；问号、引号等有语义的符号保留。整串推定还要求版本与问题绑定正确、引用全部可映射且有效。

标注写在 `tests/eval/cases/*.json` 对应问题的 `query_meta` 与 `expect_common` 中，原 `expect` 不变。

复核流程：跑 `trial.py`，报告中“待复核”项带台账键；在 `review_ledger.json` 中按键写入或确认结论。标注、问题、输出、额外结论或语义状态任一变化，键随之改变，旧结论不再适用；`test_ledger_covers_every_pending_item` 会指出失效的键。
