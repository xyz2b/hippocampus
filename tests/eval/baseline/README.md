# 历史基线原始结果

`0517000/`：第一阶段现状基线（见 [离线评分说明](../../../docs/architecture/common-scoring-view.md)）的完整原始结果，每个用例一个文件：

- `steps[].step = "intake"`：`memory_prepare` 的完整返回，以及该步之后存储中的全部记忆（`memories_after`）。
- `steps[].step = "question"`：`memory_recall` 的完整返回（含旧 `answer_contract`）。
- `source_ids`：消息 ID 到本次运行生成的原文 ID 的映射；原文 ID 每次运行不同，判分一律按消息 ID 对应。

复现：`git archive 0517000` 导出到独立目录，应用 `0517000-export.patch`（只增加写文件的导出，不改变行为，且任何一步被跳过即报错），运行
`EVAL_STRICT=1 EVAL_EXPORT=<输出目录> cargo test --test eval resident -- --nocapture`。
严格回放结果应为入库 201/3/0、召回 42/0/0、拒答 21/0/0，最后的断言失败来自 3 个已知入库缺陷。

这些文件是历史记录，不随代码更新；新评分器只读取它们离线重算，不用当前代码的输出代替旧基线。
