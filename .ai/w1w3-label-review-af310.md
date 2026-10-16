# AF310 / 评审 58：W1/W3 标签复核草案（未批准、未修改样本）

依据：AF302 第 1 遍 report-repeat-1.json 与严格 request 指纹匹配的最后一次录制；当前 src/write_models.rs W1/W3 v4、contracts.md 1.2/1.4/1.5/2.1–2.3。零调用。

## 结论与口径

不能把所有失败都归因于旧 w1.v1 字符串。它是历史标签版本；运行请求已经 v4。评分器已有 fact 连续片段机械核验，但语义匹配仍固定于期望 value，因此合规片段仍可能 value/match 失败。match 通常是同一 value 差异的连带字段，不是第二个独立模型错误。

分类以整个失败输出为单位：A=按现规则可接受、标签/评分选项过窄；B=确有模型错误（可以同时存在 A 类字段）；C=需要样本/规则口径裁定，本稿不将 C 分支自动放宽。另有 Sol w1_20 传输失败，不归为模型 B，也不通过。

草案自动重评分不等于人工确认：对同题全局放宽一个合法 value，会让另一个档的 C 输出自动通过（Haiku w1_05），报告须继续保留人工待决。未全量复核原先所有通过项的未评分字段，不能宣称全面语义通过率。

## 规则依据

- contracts 1.5、2.1–2.2；W1 提示词第 60–70 行：通用 fact value 为定位单条断言的连续片段，完整 sentence 保留；专用关系值规则不变。
- W1 第 73–80 行：描述性陈述用 fact、喜恶用 preference、要求禁令用 constraint、有界经历用 episode；avoids 的 affirm 已表达回避，不能重复 negate。
- W1 第 29–33 行：retraction 撤回断言，不是删除；第 34–49 行及 contracts 1.4：明确 history 先行词可绑定，不能默认 self。
- W1 第 130 行附近与 contracts 2.1：request.quote 含控制词，target.quote 指明对象；未要求 quote 最短，精确原文和正确对象/索引不可放宽。
- W1 第 90–115 行及 contracts 1.2：适用时间单列 times，状态成立/事件发生角色分开；相对月份不得由模型算成指定月份。
- W3 第 147–149 行：主体无明确指代支持判 insufficient；第 169–180 行、contracts 2.3：明确限定语改变形成冲突判 contradicted；不存在支持或排除的收窄判 insufficient。
- W3 第 193–204 行、contracts 2.3：限定语仅在 sentence 出现不能替代字段，unknown 可保留不可换算时间；第 179–182 行要求指出受影响 aspect，并未要求把全部语义问题穷举。

## 逐条归因

录制统一路径：target/eval-providers/compare-af302/recordings/<档>/repeat-1/<完整样本ID>.json。以下短 ID 唯一对应原样本，完整 ID、失败字段保存在 reports-af310/attribution.json。

| 档 | 题目 | 类别 | 证据与理由 |
|---|---|---|---|
| ds-pro | w1_05 | A | value="我会考虑租房"；条件、uncertainty=考虑、以后/event/unknown 均保留。§1.5、W1:60–70。 |
| ds-pro | w1_06 | A | value="特别怕狗" 定位该断言；reported、小时候/validity 保留。旧整句 value 无必要。 |
| ds-pro | w1_07 | A | value="下个月开始学日语"；plan 与 event 时间完整。 |
| ds-pro | w1_08 | B | 除 retraction 外额外生成 forget；收回断言不等于删除记忆。W1:29–33。 |
| ds-pro | w1_10 | A | 请求 quote="记住：" 是含控制词的精确引用，目标索引仍只绑定生日候选。 |
| ds-pro | w1_11 | A | value=手机号片段；请求引用含整句控制词；unresolved 主体未捏造持有人，retention/期限/索引完整。 |
| ds-pro | w1_15 | A | avoids/affirm/fact 表达描述性不食用行为，不必强判喜恶或禁令；不是重复否定。 |
| ds-pro | w1_20 | A | 请求引用保留完整输入，仍仅输出 forget 请求，没有执行注入指令或扩大目标。 |
| ds-pro | w3_01 | A | 候选 times=[] 漏“现在”；contradicted[time] 正确，旧 supported 错。§2.3、W3:179–204。 |
| ds-pro | w3_04 | B | 不喝咖啡不证明不喜欢咖啡；应 insufficient[relation]，不能机械改 polarity 判 contradicted。 |
| ds-pro | w3_05 | A | 来源只支持母亲，不支持 self；当前 W3 对缺乏明确主体指代支持规定 insufficient[subject]。W3:147–149。 |
| ds-pro | w3_06 | A | contradicted[condition] 已准确指出明确条件被删；不应因未列 speech 再判错，问题数组未要求穷举全部错误。 |
| ds-pro | w3_07 | C | reported 被改成 user_statement，输出 insufficient[speech]；需要统一“言语身份冲突”与“事实真值支持不足”的 verdict 口径。 |
| ds-pro | w3_14 | A | times=[] 漏“刚”；contradicted[time] 正确。未支持 episode 给 unknown 不应因旧 supported/low_value 组合直接拒绝。 |
| gpt-6-luna | w1_04 | B | lives_in 的值写成“搬到成都”，不是地点；把“上个月”自行换算 month=8 也违背相对月份解释规则。 |
| gpt-6-luna | w1_05 | B | 片段可接受，但 uncertainty=null 丢失“考虑”，还漏以后时间；不能因修 value 就放行。 |
| gpt-6-luna | w1_06 | C | 片段合法；童年怕狗是状态还是 bounded experience/episode 尚需样本口径裁定，当前保留 fact 期望。 |
| gpt-6-luna | w1_07 | A | value="学日语" 足以定位计划活动，完整 sentence、plan、下个月/event 均保留。 |
| gpt-6-luna | w1_10 | A | sentence 保留“记住：”前缀是完整原句；目标仍为同一候选。生日值已完整保留，非凭空日期。 |
| gpt-6-luna | w1_11 | A | 手机号连续片段合法；保留 unresolved、retention 及期限，不强迫 value 包含保存命令。 |
| gpt-6-luna | w1_20 | A | 完整请求 quote，不改变请求类型和全体记忆目标。 |
| gpt-6-luna | w3_02 | B | 明确“可能”被删，按当前规则应 contradicted[uncertainty]；insufficient 不符合明确限定语冲突规则。 |
| gpt-6-luna | w3_04 | B | relation/polarity 两项仍不能证明“不喜欢”，不喝与喜好可同时成立。 |
| gpt-6-luna | w3_05 | A | 缺 self 主体依据，insufficient[subject] 符合现行提示词。 |
| gpt-6-luna | w3_06 | A | condition/time/value 诊断含真实遗漏条件，contradicted 合理；不强制必须出现 speech 才算拒收正确。 |
| gpt-6-luna | w3_07 | C | 同 DS：insufficient[speech] 是否应接受，需明确转述身份的 verdict 政策。 |
| gpt-6-luna | w3_10 | B | supported 把宽泛地点缩窄成具体地点；违背用户已裁定的 unsupported narrowing 规则。 |
| gpt-6-luna | w3_16 | B | 第二候选漏“今天”却 supported，并支持 remember 同时绑定无关疲劳候选。 |
| gpt-6-luna | w3_20 | B | 使用 Neovim 不证明 owns；supported 无依据，不能因软件常可获取就推断拥有。 |
| gpt-6.1-sol | w1_04 | A | fact/episode 保存“搬到成都”这一有界经历，不必强推当前居住状态；“上个月刚”连续时间引用合法。须整支匹配，不能把 episode 分支拼到 lives_in 值上。 |
| gpt-6.1-sol | w1_05 | A | 片段合法，考虑/条件/以后时间保留。 |
| gpt-6.1-sol | w1_06 | A | 片段合法，reported、小时候/validity 保留。 |
| gpt-6.1-sol | w1_07 | A | 片段合法，计划/event 时间保留。 |
| gpt-6.1-sol | w1_10 | A | 完整 sentence 与整句请求 quote 均含真实控制词；索引与对象没有扩大。 |
| gpt-6.1-sol | w1_11 | C | 仅保留 source retention、无候选；原文主要是保存命令，是否必须抽取无持有人的手机号断言需裁定。 |
| gpt-6.1-sol | w1_13 | B | history 明确老王为先行词，却 unresolved；另把直接用户陈述标 reported，无依据。片段合法不抵消主体/speech 错误。 |
| gpt-6.1-sol | w1_15 | A | avoids/affirm/fact 是描述行为，未把否定本身当冲突或撤回。 |
| gpt-6.1-sol | w3_01 | A | 正确检出“现在”未进入 times。 |
| gpt-6.1-sol | w3_02 | B | 删除明确不确定限定应 contradicted，不是 insufficient。 |
| gpt-6.1-sol | w3_05 | A | 缺 self 主体支持，应 insufficient[subject]。 |
| gpt-6.1-sol | w3_14 | A | 正确检出“刚”时间遗漏；episode_value=low_value 可保留。 |
| claude-haiku-5-5 | w1_04 | B | 搬家时间被放进 lives_in 的 validity，相对月区间不是住址有效期；会把持续状态限在上个月。 |
| claude-haiku-5-5 | w1_05 | C | value 合法，条件/考虑均保留；times=[]，需明确条件中的“以后”是否还必须单列为租房候选时间。不以 keyword 强制归属。 |
| claude-haiku-5-5 | w1_06 | A | 片段、reported 与小时候/validity 保留；句尾标点差异已有通用规则。 |
| claude-haiku-5-5 | w1_07 | A | 片段合法且 event 时间完整。 |
| claude-haiku-5-5 | w1_08 | B | retraction 额外生成 forget，无删除授权语义。 |
| claude-haiku-5-5 | w1_09 | A | quote="忘掉吧" 含控制词，target.quote 单独保留“我住址那条”，未扩大范围。 |
| claude-haiku-5-5 | w1_10 | A | quote="记住：" 合法；目标索引没变。 |
| claude-haiku-5-5 | w1_11 | C | 同 Sol：仅 source retention 的零候选分支需裁定；本稿不放行。 |
| claude-haiku-5-5 | w1_13 | B | history 已明确老王，仍 unresolved；不是目录唯一推断，现行规则允许绑定。 |
| claude-haiku-5-5 | w3_02 | B | 明确不确定词被删，insufficient 不符合 contradicted 规则。 |
| claude-haiku-5-5 | w3_04 | B | 不喝不等于不喜欢，polarity 拒收理由不正确。 |
| claude-haiku-5-5 | w3_07 | C | 转述缺确认与 speech 冲突的 verdict 分界需裁定。 |

## A 类最小标签修订（仅草案）

具体完整 expect 副本见 target/eval-providers/compare-af302/reports-af310/draft-expectations.json。输入/ID/请求不变，不放宽 decode、W2/W3 validate、精确引用和极性关系联动。

1. W1_04：保留 lives_in/fact/location/event 原分支；新增整支 fact/episode/value=“我上个月刚搬到成都”、event relative -1 month，时间 quote 可“上个月”或“上个月刚”。不接受 Luna 专用关系 value=“搬到成都”，不接受 Haiku validity 区间。
2. W1_05：value 加“我会考虑租房”；condition 与 uncertainty 仍必须保留。“以后”的适用归属另见 C，不为此先加通用关键词规则。
3. W1_06：value 加“特别怕狗”“小时候特别怕狗”“我小时候特别怕狗”；保持 reported/fact，不先接受 Luna episode。建议随后补小时候/validity/unknown 的必需时间覆盖，不能只检查 value。
4. W1_07：value 加“下个月开始学日语”“学日语”“打算下个月开始学日语”；plan、完整句子和既有字段不动。后续补 event 时间评分，不能允许合法片段掩盖时间缺失。
5. W1_09：请求 quote 加“忘掉吧”；target/indices 不动，仍须正确指向旧住址。
6. W1_10：sentence 加含“记住：”前缀的完整原句；request.quote 加“记住：”和整句。candidate_indices 仍只指生日；不把任何 substring 都当作合法请求引用。
7. W1_11：value 加号码片段，request.quote 加整句；本稿保留一候选和其绑定，零候选/source retention 分支待裁定。
8. W1_15：保留原 preference/constraint 分支；新增完整 avoids+affirm+fact 分支。不得组合出 avoids+negate 的相反语义；生产评分器已有关系/极性一致性守卫。
9. W1_20：请求 quote 加完整原句；forget 对象与空索引含义不变，不执行注入。
10. W3_01：原 supported → contradicted[time]（候选漏现在）；输入不修复，coverage 仍 complete。本轮另外两档原通过应转失败。
11. W3_05：原 contradicted[subject] → insufficient[subject]，按现提示词“主体无明确支持”规则；不接受 supported。Haiku 原通过会转失败。旧 AF151 裁定是 contradicted，本稿属于待批准的标签纠正，不能静默覆盖历史结论。
12. W3_06：完整分支保留 contradicted[speech]，增加 contradicted[condition]；条件确实丢失，含 condition 的诊断可以准确拒收。未要求每条 review 列出全部问题；supported 仍失败。
13. W3_14：原 supported/low_value → contradicted[time]；episode_value 接受 low_value 或 unknown（拒收候选不强迫给确定利用价值），不接受 useful；coverage 不动。本轮另两档原通过转失败。

W3_02/04/10/16/20 不放宽；W3_19 继续观察；不新增“任意 verdict 都对”的集合。

## C 类需裁定

- C1 W1_06（Luna）：童年怕狗这种历史状态是否同时接受 episode；倾向维持 fact，episode 留给一次/有界经历，避免为这次分数扩大类型。
- C2 W1_11（Sol/Haiku）：纯“保存该号码至期限”的命令是否允许零候选+source retention；倾向接受该完整分支，因没有明确持有人，不应强迫抽取个人归属事实；不能同时把空 indices 当作全部旧事实。需明确该 source 是整条消息，目标为其中号码时能否在执行层准确保留。
- C3 W3_07（DS/Luna/Haiku）：转述被写为 user_statement，insufficient[speech] 是否与 contradicted[speech] 并列。倾向在 speech 身份层保持 contradicted；若决定按事实支持不足接受 insufficient，应明确它不代表原事实被反驳，并绑定 speech aspect，不扩到普通限定语遗漏。所有档的当前输入还漏“小时候”时间，至少不能 supported。
- C4 W1_05（Haiku，Luna另有明确 uncertainty 错误）：条件中的“以后去工作”是否必须另作为租房计划的 event 时间。倾向先把条件作为完整限定保留，不机械把条件事件时间当目标活动时间；如必须 times，则标明适用对象，不能仅要求出现“以后”。因此 Haiku 草案自动通过仍标 C，而非人工确认。

## 遗漏与风险

- W3_01/14 是固定输入自身在当前规则下缺时间，不能通过本轮改输入来复用录制；本稿只改期望，旧 supported 结果也要重扣分。
- W1_06/07/10 等旧期望未全面检查 times。生日日期可作为断言内容，不应机械把任何生日数字转为有效期；需另做时间角色标注审核，避免把 schema 有字段当成语义完整。
- 对请求 quote 加有限、逐一审核的等价引用，不修改全局匹配规则；否则带额外控制语义的长句可能被错误放行。
- 两遍独立实验仍只完成第一遍；单次排序不足以推断模型稳定性。Sol w1_20 的传输失败另列，主质量分母为已完成 19，不冒充模型错误。

## 零调用重评分结果

临时 crate 复用原 tests/write_stage_eval.rs 的 score/W2/W3 校验；先验证每条旧分与报告一致，再仅替换 expect。读取与 request 指纹匹配的最后一次录制；机械核验不豁免。未改 input、样本、录制、manifest、费用账本或代码。

| 档 | A | B | C | W1 原→草案 | W3 原→草案 |
|---|---:|---:|---:|---|---|
| ds-pro | 11 | 2 | 1 | 12/20 → 19/20 | 13/19 → 17/19 |
| gpt-6-luna | 6 | 7 | 2 | 13/20 → 17/20 | 11/19 → 11/19 |
| gpt-6.1-sol | 9 | 2 | 1 | 11/19 → 17/19 | 15/19 → 18/19 |
| claude-haiku-5-5 | 4 | 5 | 3 | 11/20 → 16/20 | 16/19 → 13/19 |

总计 53 个语义失败：A 30、B 16、C 7；另 Sol W1_20 传输失败 1 条，保持缺口，不当作模型失败。W3_19 每档各 1 次观察，不计入 19 分母。

注意：Haiku W1 草案的 16/20 包含 C4（w1_05），只表示自动字段通过；按待裁定不计确认则是 15 自动通过+1 待裁定。C1/C2/C3 没有放宽，仍失败/待裁定。表不是最终验收或人工确认排名。

不是单向加分：W3_01/14 原先 Luna/Haiku supported 应转失败，W3_05 原先 Haiku contradicted 应转失败，共 5 项旧通过被纠正。原失败变通过 31 项（其中 1 项仍 C），净增自动通过 26 项。

文件：draft-expectations.json（内存期望覆盖）、attribution.json（53 条逐对归因）、rescore.json（160 条旧/新状态、失败字段与原请求指纹）。C 结论须先裁定；本轮不改正式标签。
