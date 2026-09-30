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

样本由 `gen_samples.py` 生成；已复核的 JSON 是权威版本。生成器不会写出 `reviewed`：只有内容（除 `label_status` 外）与磁盘上已复核 JSON 完全一致的样本才保持 `reviewed`，新增或改动的样本一律写成 `draft`，须通过检查并经人工复核后才能改为 `reviewed`（规则见 `review_status.py`）。

当前共 21 个样本：第 1 批 w4_01–w4_08，第 2 批 w4_09–w4_21。确认冲突一方并重述其值不需要新动作：败方 retract 并解决关系，胜方按限定语变化判断（w4_21 中“未核实”变“已核实”为 refine，不满足强化前提）。关系解决后须写明保留方（survivors），非保留成员必须由本次决定结束，不能只关闭关系而让双方都作为当前结论。

legacy 记录的 `data` 是第一阶段前 `src/model.rs` 的完整 Memory 序列化；`cargo test --test w4_legacy_fixture` 检查它能解码且原样回写。

限定语保留按人工标注检查：`sources.<id>.qualifiers` 列出原文中的时间、条件、不确定表达和强度表达，取自该来源的候选和记录必须恰好保留这些（无法换算的时间以 unknown 保留），不用词表猜测。候选与记录的 sentence 须为完整原句；终止动作（state_change、correction、refine、retract）须以对应原因结束目标，其他结果不结束目标。

W5 验收（contracts 2.5）另列：同一原文相关决定的原子提交与整组重判/回滚（w4_21）；替代 open 关系成员而未解决关系时的成员迁移；幂等、并发过期、多目标原子提交。

```
python3 tests/eval/w4/gen_samples.py            # 重新生成样本
python3 tests/eval/w4/check_samples.py          # 结构检查
python3 -m unittest discover -s tests/eval/w4   # 检查器自身的测试
```

幂等、并发过期（needs_rejudge）和多目标原子提交属于 W5 的代码验收，另行列出，不在本样本集。
