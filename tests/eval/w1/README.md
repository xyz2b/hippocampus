# W1 / W3 样本（w1.v1、w3.v1）

各 20 个，由 Claude 起草，`label_status` 一律 `draft`，须经用户抽查后才能改为 `reviewed`。用途：评价 W1 提取、W3 核验提示词在真实模型上的表现（真实调用须用户批准）。模拟服务测试不依赖它们。

- `tests/eval/w1/samples/`：输入为原文、历史、实体与关系目录、said_on；`expect.candidates` 只列需要核对的字段，未列字段不评分；字段值为数组时表示可接受集合；`candidate_count` 为输出候选的确切数量；`must_not` 为禁止行为。
- `tests/eval/w3/samples/`：输入为 W2 已通过的候选（部分故意带错，简写形式：Quote 用字符串表示，评测适配器负责展开成 w1.v1 结构）；`expect.reviews[].aspects` 是必须指出的问题方面集合。
- 标注中“待用户裁定”“由人工复核”的项目写在各样本的 `note`。

```
python3 tests/eval/w1/gen_samples.py
python3 tests/eval/w3/gen_samples.py
python3 tests/eval/w1/check_samples.py   # 同时检查 w1 与 w3
```
