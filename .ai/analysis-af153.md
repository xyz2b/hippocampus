# Claude 分析（AF153，待 Codex 复核）：4 个可疑样本

依据：AF137 录制（reports-af151 重评）、现行 W1/W3 提示词（src/write_models.rs）。只看已有录制，零调用。

## w1_12 “他上周辞职了。”（GPT/Opus 0/2）
- 失败原因：GPT 2 次、Opus 2 次、ds-flash 1 次给 memory_type=episode；其余字段（unresolved 他、relation fact、整句 value、上周 relative -1 week）与标签一致。标签只收 fact。
- 判断：标签问题。W1 提示词“episode for bounded experiences”，上周发生的一次辞职是有界经历，episode 至少同样合理。
- 建议：memory_type 接受 fact 或 episode（若选 episode 则不涉及 W3 episode_value，W1 无此字段）。其余不变；9B 绑 self/works_at、ds-flash 一次 entity_id="unresolved" 仍正确判失败。

## w3_12 两候选（上海/产品经理）全 supported，coverage 用户裁定 complete（GPT/Opus/ds-pro 0/2）
- 失败原因：ds-pro、GPT、Opus 全部 coverage=gaps，missing 为“在一家银行”或“在一家银行做产品经理”（ds-pro 一次还把候选 1 判 contradicted[condition]）。
- 判断：与 AF151 不一致。AF151 已在 w1_14 把 works_at=“一家银行”列为可接受的可选候选；既然提取它可接受，W3 指出它漏提也应可接受。
- 建议：coverage 接受 complete 或 gaps；gaps 时 missing 须包含“一家银行”。verdict 仍要求两个 supported（ds-pro 那次 contradicted[condition] 仍失败）。需用户确认覆盖此前裁定。

## w3_10 “我住在杭州附近。”→ lives_in 杭州，标签 insufficient[value]（DS/Opus 0/2，GPT 1/2）
- 失败原因：强模型几乎都判 contradicted[value]，aspect 与引用正确。
- 判断：提示词本身两条规则重叠：“candidate removes or changes an explicit qualifier and thereby changes the claim → contradicted”（“附近”就是被删掉的显式限定语），与“unsupported narrowing of a value that the source neither confirms nor excludes → insufficient”。两种读法都有原文依据，强模型选了前者。运行时差异：contradicted→rejected，insufficient→pending；两者都不提交。
- 建议：接受 insufficient 或 contradicted，aspect 须含 value；提示词重叠记为待澄清。需用户确认（覆盖 AF150 的“只收 insufficient”及此前裁定）。

## w3_19 “那种场合我一般不喝酒。”→ avoids 酒（preference，strength null，条件“那种场合” resolved=false），标签 supported（强模型 0/2，只有 9B 2/2）
- 失败原因：ds-pro/GPT/Opus 判 contradicted[strength “一般”]，ds-flash 一次 insufficient[strength]；27B 两次 insufficient[subject “那种场合”]（错误）。
- 判断：契约缺口，不是简单的标签错。用户裁定“一般”是频率不是不确定性；但契约里没有频率字段，W1 提示词要求“每个表达的限定语都要进自己的字段”、偏好的 strength=null 表示“无强度表达”，所以强模型把“一般”当被丢掉的限定语也有依据。样本原考点（未解析条件如实标注）被“一般”混淆。9B 通过不代表能力。
- 建议：本轮不计入分层（标“契约待定”，只作观察）；另立待办：决定频率/习惯（一般、通常、偶尔）放在哪个字段（strength、uncertainty、conditions 或新增），之后再修样本输入（去掉“一般”需重跑 13 次，另行授权）。

## 共同影响
上述 4 题在 reports-af151 中都被归入“本地即可”，属于标签/契约造成的误分层。改后预计：w1_12、w3_12、w3_10 的强模型通过数上升；w3_19 移出分层。

请 Codex 只读复核：每条归因与提示词依据是否成立；w3_10 是否应双收；w3_19 “不计入分层”是否比“双收”更合适、评分器能否支持；最小改动与允许文件。
