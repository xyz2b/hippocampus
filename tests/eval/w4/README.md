# W4 更新判定样本（w4.v5，AF276）

每个样本是一组“新候选 + 代码取得的旧记录”，写明按 contracts.md 2.4 应得到的决定、执行后的状态和禁止的副作用。样本独立于模型录制，只用于设计与验收；判断是否正确由人工复核（`label_status`）。

| 字段 | 含义 |
|---|---|
| `sources` | 原文及可信原陈述日（可为 null），所有 Quote 必须逐字出现在其中 |
| `input` | 交给 W4 的输入：`candidates`（W1/W3 v4 候选）、`records`（RecordView）、`relations`、`coverage` |
| `call_w4` | 是否应调用 W4；为 false 时 `expect.code_path` 写明代码的处理 |
| `expect.decisions` | 预期决定：动作、目标集合、须引用的来源、生效时间、关系范围、待判定原因 |
| `expect.alternatives` | 完整期望分支，整支匹配；不得跨分支拼接字段 |
| `expect.after` | W5 执行后记录与关系的状态；其中 recall 的自然语言语义独立人工复核 |
| `expect.must_not` | 禁止的副作用，字段可检部分用离线运行时断言，自然语言部分绑定人工复核 |
| `rejected_alternatives` | 容易混淆的动作及排除理由；`boundary_with` 指向对照样本 |

样本由 `gen_samples.py` 生成；已复核的 JSON 是权威版本。生成器不会写出 `reviewed`：只有内容（除 `label_status` 外）与磁盘上已复核 JSON 完全一致的样本才保持 `reviewed`，新增或改动的样本一律写成 `draft`，须通过检查并经人工复核后才能改为 `reviewed`（规则见 `review_status.py`）。

AF276 已复核 28 个样本（25 个调用 W4，3 个代码跳过路径）：原 01–23、08/14 的完整范围对照、n1a 否定状态变化、n1b 否定纠错、n2 同句兄弟片段。w4_11 保留合法的 fact/preference 表达，不强制换成专用关系；w4_21 的断言 value 是连续片段，撤回 value 仍为 null；w4_17 的旧 90 仍 judged，只有新记录 pending。

确认冲突一方并重述其值不需要新动作：胜方按限定语变化判断（w4_21 中“未核实”变“已核实”为 refine，不满足强化前提），败方 retract；整组由一个合格决定明确解决关系，不强制归属撤回候选。关系解决后须写明保留方（survivors），非保留成员必须由本次决定结束，不能只关闭关系而让双方都作为当前结论。

通用 fact 的 value 为完整 sentence 内的连续原文片段，text 与 quote.text 相等；完整原句 value 仍合法，sentence 不缩短。scope 完整性取决于实际穷举授权范围与必要闭包，不能因为 relation=fact 就永久判不完整。原有 partial 样本保留固定的“不完整范围”输入，运行时验收明确注入该 coverage；08/14 complete 对照另验真实穷举的提交/重判行为。未知原陈述日不得拿接收日推算相对日期。

pending 的 targets 是可选定位信息，17/18 接受带目标与空目标的完整分支；pending 不强制旧方依据，仍要求新方原文。decided 的目标必须有对应旧方依据。目标按集合评分（重复仍由机械核验拒绝），effective 与 relation_scope 逐字段评分。

legacy 记录的 `data` 是第一阶段前 `src/model.rs` 的完整 Memory 序列化；`cargo test --test w4_legacy_fixture` 检查它能解码且原样回写。

限定语按断言绑定的人工标注检查：`qualifier_marks` 以 `candidate:<index>` / `record:<id>` 为键，列出该断言自己的时间、条件、不确定表达和强度表达，候选或记录必须恰好保留这些（无法换算的时间以 unknown 保留）；一句话里有多个断言时各自标注，不按原句含词分配。完整原句按 `sources.<id>.sentences` 的人工分句核对（未标注时整段原文为一句），不猜标点。终止动作（state_change、correction、refine、retract）的每个目标都须在 `after.records` 写明执行后状态并以对应原因结束，其他结果不结束目标。执行后新产生或复用的记录用 `from_candidate` 绑定到产生它的候选（重判时只能复用 committed_id，它也可能出现在输入记录中），不按键名识别；范围不完整时的更新状态检查同样按绑定取记录；重判为强化另一记录时，已提交记录写 `merged_into` 并退出投影（contracts 2.5 归并例外）；state_change、correction、refine 的替代记录须保留完整前身链（history 包含全部目标）。替代未解决冲突成员的记录须为 pending 且含 update_relation_unclear，同批无关候选的记录不受影响。

W5 验收（contracts 2.5）另列，须检查真实提交、回滚和重载后的状态，静态 `after` 不能证明事务正确：同一原文相关决定的原子提交与整组重判/回滚（w4_21）；替代 open 关系成员而未解决关系时关系保持不变、替代记录标 update_relation_unclear（w4_22）；幂等、并发过期、多目标原子提交。

`fixtures/<id>.output.json` 是离线验收用的模型输出夹具，由 `gen_fixtures.py` 从期望决定生成：把 `basis_sources` 换成样本里的具体片段，其余字段不变。它们不是人工标签；`cargo test --test w4_offline` 用它们跑“校验 → 转换 → 提交 → 重开核对”，并检查夹具与期望决定一致。样本改动后须重新生成夹具。

分阶段报告保留 raw/final 和 automatic_fields；机械核验通过的输出复用 `runtime.rs`，在临时 SQLite 执行、重开并核对所选完整分支的 after，结果独立放在 runtime_state_result，失败不计通过。机械失败明确标“未执行”，未标 after 的其他定点评测标“不适用”。标准夹具与生产 R3/R5 回归另由 w4_offline 覆盖极性、兄弟记录 grade/update 和截断标记。after.recall 与自然语言 must_not 绑定样本、标签版本、输入、请求/repeat 与最终输出摘要后待人工复核，机械失败不可人工豁免。

历史录制不改写：输入、schema 或提示词变化均按严格指纹报告过期；标签/label_revision 修改不影响请求指纹。原 04/15/19/21 的输入变化影响各自历史录制，其他原样本输入保持不变；旧 w4_v2 录制本就不能作为当前 w4.v5 的有效回放。

```
python3 tests/eval/w4/gen_samples.py            # 重新生成样本
python3 tests/eval/w4/gen_fixtures.py           # 重新生成模型输出夹具
python3 tests/eval/w4/check_samples.py          # 结构检查
python3 -m unittest discover -s tests/eval/w4   # 检查器自身的测试
cargo test --test w4_offline                   # 真实 SQLite 与重开/召回验收
cargo test --test write_stage_eval af276       # 字段/完整分支/指纹回归
```

### AF322 生效时间分支

11、13、14 两变体、n1a 保留 effective=null，并接受对应候选中原样的
validity/unknown；04 接受对应“周末的时候”、23 接受对应“每天”的已核验周期时间。
仅这7题标 label_revision=AF322；每支完整复制 decisions/after/recall/must_not，
除了 effective 外不变。不得接受任意时间、伪造时间或跨分支串配；17/20/22 不放宽。
检查器已有完整分支逐支核验，无需修改规则。输入、ID、reviewed 状态和请求指纹不变。
