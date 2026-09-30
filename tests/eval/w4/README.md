# W4 更新判定样本（w4.v1）

每个样本是一组“新候选 + 代码取得的旧记录”，写明按 contracts.md 2.4 应得到的决定、执行后的状态和禁止的副作用。样本独立于模型录制，只用于设计与验收；判断是否正确由人工复核（`label_status`）。

| 字段 | 含义 |
|---|---|
| `sources` | 原文及接收日（用户本地日期），所有 Quote 必须逐字出现在其中 |
| `input` | 交给 W4 的输入：`candidates`（w1.v1 候选）、`records`（RecordView）、`relations`、`coverage` |
| `call_w4` | 是否应调用 W4；为 false 时 `expect.code_path` 写明代码的处理 |
| `expect.decisions` | 预期决定：动作、目标、须引用的来源（`basis_sources`）、生效时间、待判定原因 |
| `expect.after` | W5 执行后记录与关系的状态、召回时的呈现 |
| `expect.must_not` | 禁止的副作用 |
| `rejected_alternatives` | 容易混淆的动作及排除理由；`boundary_with` 指向对照样本 |

样本由 `gen_samples.py` 生成；已复核的 JSON 是权威版本，改动须重新生成、通过检查并重新复核。

```
python3 tests/eval/w4/gen_samples.py            # 重新生成样本
python3 tests/eval/w4/check_samples.py          # 结构检查
python3 -m unittest discover -s tests/eval/w4   # 检查器自身的测试
```

幂等、并发过期（needs_rejudge）和多目标原子提交属于 W5 的代码验收，另行列出，不在本样本集。
