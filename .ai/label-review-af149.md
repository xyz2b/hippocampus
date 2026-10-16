# Claude 标注复核草案（AF149）：W3 20 个样本按 v3 口径 + W1 全失败样本

依据：AF137 对比录制（7 档 × 2 次，target/eval-providers/compare-af137）、契约 2.1/2.3（w1.v3、w3.v3）、当前 W1/W3 提示词。只改标注/期望（或样本输入），不改提示词与评分器；改后用现有录制离线重评。

## 主要发现
强模型 W3 失分最大来源是 coverage.status（不是 verdict）。现行 W3 提示词与契约要求：“按候选实际的 relation/value/限定语核对覆盖，句子里出现不等于已覆盖”。因此当唯一候选把断言表示错了（主体/极性/关系/值错）时，原文真实断言并未被任何候选覆盖，coverage 应为 gaps。旧标注（v1）一律写 complete。

## 拟修改（W3）
| 样本 | 现标注 | 拟改为 | 理由 |
|---|---|---|---|
| w3_04 极性反了“我不喝咖啡”→likes 咖啡 | contradicted[polarity]，coverage complete | verdict 接受 contradicted[polarity] 或 insufficient[relation]；coverage gaps（missing 含“不喝咖啡”） | “不喝”≠“不喜欢”，候选关系本身也不被支持；真实断言未覆盖 |
| w3_05 主体错绑“我妈对青霉素过敏”→self | contradicted[subject]，complete | contradicted[subject]，coverage gaps（missing 含“我妈对青霉素过敏”） | 真实断言（母亲过敏）未覆盖 |
| w3_06 假设被标成陈述 | contradicted[speech]，complete | verdict 不变（aspects 含 speech 即可）；coverage 接受 complete 或 gaps | 假设本身是否属于应提取断言有争议，两者都合理 |
| w3_10 “我住在杭州附近”→杭州 | insufficient[value]（用户曾裁定） | 接受 insufficient 或 contradicted [value]；coverage gaps（missing 含“杭州附近”） | v3：明示冲突才 contradicted，“附近”是否明示冲突有争议；真实断言未覆盖 |
| w3_16 remember 被扩展 | 候选1 supported；请求 contradicted | 样本修正：候选1 补上“今天”的 times（隔离本题考点）；请求接受 contradicted 或 insufficient | 候选1 漏了“今天”，按 v3 应不 supported，干扰考点；提示词允许两种请求判定 |
| w3_17 事件时间当保存期限 | 请求 contradicted | 请求接受 contradicted 或 insufficient | 提示词原文即“contradicted or insufficient” |
| w3_18 如实保留的未解析主体“他上周辞职了” | supported | 接受 supported 或 insufficient[subject] | AF97 措辞后模型普遍判 insufficient[subject]；两者结果相同（未解析主体都不提交）；另记提示词歧义待修 |
| w3_20 关系 owns vs uses | contradicted[relation] | 接受 insufficient 或 contradicted [relation]；coverage gaps（missing 含“用 Neovim”） | v3：未明示冲突应 insufficient；真实断言（uses）未覆盖 |

保留不改（判定为模型错误或用户已裁定）：w3_01（“现在”不是必须提取的时间）、w3_02、w3_03、w3_07、w3_08、w3_09、w3_11、w3_12（用户裁定 complete）、w3_13、w3_14、w3_15、w3_19（用户裁定“一般”是频率）。

## 拟修改（W1，7 档全失败）
| 样本 | 现标注 | 拟改为 | 理由 |
|---|---|---|---|
| w1_05 “如果我以后去深圳工作，我会考虑租房。” | speech=hypothetical，无条件、无不确定性 | **需用户裁决**：接受 hypothetical，或 plan + 条件“如果我以后去深圳工作” + 不确定性“考虑” | AF71/AF56 规则让模型把条件性打算判为 plan；“会考虑”是否表达实际打算有争议 |
| w1_08 “之前说我在字节工作那个不对，收回。” | retraction，value=null，memory_requests 0 | value 接受 null 或被撤回值“字节”；memory_requests 仍为 0 | 契约：retraction 的 value“可为” null，给出被撤回值有助 W4 定位；“收回”是撤回不是删除请求 |
| w1_14 “我住在上海，在一家银行做产品经理。” | 2 个候选（用户曾裁定不另给 works_at） | **需用户裁决**：维持 2，或接受额外 works_at“一家银行”（2 或 3 个均可） | 7 档 14 次全部给了 works_at；“一家银行”是否算雇主信息 |

请 Codex 复核：发现与归因是否成立、每项修改是否符合契约 v3 与现行提示词（不放宽到“什么都算对”）、评分器是否支持“可接受集合”（多个 verdict/aspects、coverage 二选一、可选候选）、离线重评做法；给出需要的最小代码/数据改动与允许文件。

## Codex 评审 34 后的修订方案（AF150，待用户确认）
回应 Codex：
- 采纳：w3_04 只收 insufficient[relation]；w3_10 只收 insufficient[value]（维持用户裁定）；w3_20 只收 insufficient[relation]；w3_18 只收 insufficient[subject]（现提示词明确要求，11 次录制全部如此）；w3_16 不改输入（避免指纹变化）；w3_17 双收；w1_08 value 接受 null/“字节”；w1_14 第三项只允许 works_at=“一家银行”，任意第三项不通过；评分器按“完整期望分支”实现，整支通过才算通过。
- 部分采纳（覆盖一致性）：Codex 指出若 04/05/10/20 改 gaps，则 02/03/08/09 必须一致。录制显示强模型对“唯一候选被拒收”时的 coverage 本身不一致（如 02：ds-pro gaps、GPT 一次 gaps 一次 complete、Opus complete）；现提示词“按候选实际表示核对覆盖”可读出 gaps，但没有明确规定被拒收候选是否算覆盖。故不强行定为 gaps，而是：**唯一候选被拒收的样本（02、03、04、05、08、09、10、20 及 16 的候选 1）coverage 不计分**，接受 complete 或 gaps；若给 gaps，missing 仍须为原文精确引用（05 须引用母亲的断言）。verdict 与 coverage 在报告中分列。被拒收候选的覆盖语义记为提示词待澄清项，下次改提示词时一起定。
- w3_16：按现有输入，候选 1 漏了“今天”，改为接受 contradicted 或 insufficient（aspect 含 time），不得 supported；请求接受 contradicted 或 insufficient（提示词只规定“无关候选不被 remember 授权”，没有规定哪种拒收），不得 supported。修正输入另立任务（需 13 次调用授权）。
- w3_06 与 w1_05 联合裁定；删除“works_at + 整句 value”备选。
- 保留：w3_01、07、11、12（用户裁定 complete）、13、14、15、19。

允许文件（交 Codex 实现）：经确认样本的 expect/note/修订信息（输入与 ID 不动）及两个 gen_samples.py；tests/write_stage_eval.rs（期望分支、coverage 不计分、报告标注 AF150 修订）；tests/eval/w1/check_samples.py；相关 README。不改生产代码、提示词、契约、provider 配置、录制与账本。离线重评零调用，新旧报告分开保存。
