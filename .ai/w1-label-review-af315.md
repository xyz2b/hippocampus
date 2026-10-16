# AF315 / 评审59：W1 第二遍归因与片段评分草案

只读评审，未改样本、评分器、提示词、录制、manifest 或账本，零模型调用。本文件是未批准草案。
依据：AF302 report-repeat-2-w1.json、report-w1-combined-af311.json、严格匹配 request 的最后一次录制。
输出：target/eval-providers/compare-af302/reports-af315/。

## 结论

第二遍20条失败：A（标签过窄、输出可接受）8；B（模型真实错误，可能同时存在标签过窄字段）11；C（需裁定）1。
Claude 列举基本成立，但遗漏 DS w1_13 的合规“辞职了”；Luna w1_10 的纯日期不是可以直接批准的 A。
Haiku w1_04 仍是搬家事件被写成住址有效期的真实错误。
不能把专用关系 avoids 的对象“狗”单独当作片段违规；其失败在关系/类型含义和 AF311 明确保持 fact 的裁定。

## 依据与边界

- contracts.md 1.5（103行）、2.1（148–150行）：relation=fact 的 value 是足以定位单条断言的连续原文片段，代码只核类型/连续引用/包含；语义充分性由 W3 核验。sentence 保留完整原句。专用关系对象规则不变。
- write_models.rs W1 60–80行：通用片段须保留活动、地点等内容；描述性陈述用 fact；限定语放相应字段，仅留在 sentence 不够。
- W1 34–49行及 contracts 1.4：有明确 USER history 先行词可绑定；不能忽视 history 保持 unresolved，也不能目录唯一就绑定。
- W1 90–115行及 contracts 1.2：状态有效时间/事件发生时间分开；搬家时间不是 lives_in 的有限有效期。
- contracts 2.1（143–150行）：请求对象/索引不能扩大；撤回事实不是删除；表达回答偏好不是自动生成 remember 的依据。
- AF311：w1_06 保持 fact，不接受 episode；w1_05 可不单列条件里的以后，但仍须保留 uncertainty；w1_11 可零候选+source retention。

## 第二遍全部失败逐条归因

每行证据均位于 recordings/<档>/repeat-2/<完整ID>.json；完整ID、指纹、raw和评分字段见 reports-af315/rescore.json。

| 档 | 题 | 类别 | 根因与草案处理 |
|---|---|---|---|
| ds-pro | w1_05 | B | uncertainty=null；即使 value 合规也漏“考虑”的独立字段，继续失败。 |
| ds-pro | w1_06 | B | avoids/preference/strong 表达回避偏好，不等于 reported 的历史恐惧描述；违反 AF311 fact 裁定。对象“狗”对专用关系不是机械非法。 |
| ds-pro | w1_13 | A | value=辞职了；主体已正确绑定 e-laowang，event/上周完整，仅旧整句匹配失败。 |
| luna | w1_04 | A | fact/episode 的 value=上个月刚搬到成都；省略我不丢主体（self已绑定），相对月 event 正确。 |
| luna | w1_05 | B | uncertainty=null；继续失败，不以片段修订豁免。 |
| luna | w1_07 | A | 开始学日语 是原句连续断言片段；plan与下个月/event保留。 |
| luna | w1_10 | C | relation=fact 但 value=3月12日；值仅有日期、没有生日属性。是否足以“定位单条断言”需裁定，不能按日期核心命中自动放行。 |
| luna | w1_13 | B | history明确老王，仍 unresolved；片段/episode并非本次失败主因。 |
| sol | w1_05 | B（含A字段） | 考虑租房 合规片段，但 uncertainty=null；修value后仍失败。 |
| sol | w1_06 | B | avoids/episode/狗 与 fact 历史状态裁定不符；不得通过修改片段规则放行。 |
| haiku | w1_04 | B | lives_in/成都 的 times 把上个月标 validity；只知搬家事件时间，不能将住址有效期压缩为该月。 |
| haiku | w1_05 | A | 考虑租房 合规，条件、会考虑字段均保留；resolved=false仍是保留且标未理解，不应视同缺失；AF311允许不列以后。 |
| haiku | w1_07 | A | 同 Luna：开始学日语；plan、event与相对月正确。 |
| haiku | w1_08 | B | retraction之外新增forget；“收回断言”不是授权删除保存内容。 |
| haiku | w1_09 | B | 住址那条已明确是facts，却给target.object=unknown；不应因此扩大接受集合。 |
| haiku | w1_11 | A | 零候选+source retention已获AF311批准；deadline引用只保留到明天为连续精确原文，retention/relative day +1不变。 |
| haiku | w1_12 | A | 辞职了，unresolved主体与上周/event保留；未凭空绑定。 |
| haiku | w1_13 | B（含A字段） | 辞职片段合法，但history明确老王仍unresolved；继续失败。 |
| haiku | w1_17 | A | fact/episode值养过一只猫保留完整活动，unknown/以前/event符合现有允许角色。 |
| haiku | w1_19 | B | constraint/response_language之外凭空新增remember；“以后回答请用英文”不是记忆控制请求。 |

## 结构性方案：采纳方向，收窄核心定义

不建议“名词核心+任意连续引用即语义通过”。狗可能只是对象、成都可能只是地点、生日问题里的日期可能是另一事件；
同句还可能存在两次出现、不同主体、否定、纠正或条件范围。连续引用只证明出处，不证明完整语义。

最小方案：只在评测标签中引入显式的片段规则；不改生产 W2/W3，也不由代码从句子自动推导核心。
- 每个完整期望分支分别指定允许的断言窗口（原文精确片段/位置）和必含的断言核心（可多个）。
- 只用于 relation=fact、kind=assertion。必须是该窗口内且实际sentence内的连续片段、text==quote.text；原机械核验照旧。
- 核心含必要的谓词/属性和对象，不能只标名词。该核心的语义正确性由人工标注承担。
- 主体、memory_type、speech、polarity、uncertainty、条件、times、候选数量和请求仍整支匹配；不能借别支字段拼装。
- 专用关系保持现规则。w1_17 将 owns 对象分支与 fact 断言片段分支分开，避免 fact=猫 的旧笛卡尔组合。
- 多断言/同句更正/否定范围复杂、重复核心无法唯一定位时，不套通用核心规则，保留完整分支或人工复核；不自动放宽。
- 已明确的否定、程度、数量若无法在其他字段完整表达，须保留在核心/窗口限制中。
- 自动字段通过仍不是 W3 supported 或人工语义确认。无需改契约/协议/模型请求；未来实现仅评测逻辑及标签，需批准。

草案规则（固定于标签、不是按本次模型输出临时取子串）：

| 题 | 允许窗口 | 必含核心 |
|---|---|---|
| w1_04 通用fact/episode支 | 我上个月刚搬到成都 | 搬到成都 |
| w1_05 两支 | 完整条件计划原句 | 租房（同时仍要求条件、uncertainty字段） |
| w1_06 | 完整原句 | 特别怕狗（fact无strength字段表达程度，不只要求狗或怕狗） |
| w1_07 | 完整原句 | 学日语 |
| w1_12/13 | 他上周辞职了。 | 辞职 |
| w1_17 fact支 | 我以前养过一只猫。 | 养过、猫 |
| w1_10 | 暂不变 | 待用户裁定；建议生日属性与日期均保留 |

更简单的实现：评分器增加一个“窗口包含+核心全含”的标签比较方法，沿用现有候选身份和完整分支匹配。
无需分词器、同义词、模型判分、正则语义归一或第三轮模型调用。
诊断中为了不修改评分器，把规则展开成该窗口内所有满足核心的连续子串期望，交回原Rust score/W2核验；
不是把实际回答直接写回期望。正式实现不需枚举这些字符串。

## 时间 quote

同样不能全局“包含明天即等价”：不是明天、明天之前/之后、从明天起、条件中的明天，可能有不同解释。
只给明确标签加 quote_window + required_core；role、anchor、interp及精度仍独立核验。
本次仅 w1_11 deadline：窗口“只保留到明天”，核心“明天”；允许其中包含完整核心的连续引用。
不泛化到别的时间/条件，不改 deadline day+1，不接受到后天、否定期限等。
目前评分 normalize_time 去掉 anchor，但生产W2要求said_on，不能只用归一化评分替代机械核验。

## 零调用重评

160个W1槽请求指纹逐项一致；159个响应先按AF311复现原分，再按草案评分；Sol第一遍w1_20传输失败仍缺口。
第一遍无变化，第二遍8个A转通过，11个B与1个C保留不通过。未修改任何正式标签或录制。
C不是新增硬错误结论，只在主表中暂不计通过。

| 档 | 第一遍 | 第二遍 原→草案 | 两遍 原→草案 |
|---|---|---|---|
| ds-pro | 19/20 | 17/20 → 18/20 | 36/40 → 37/40 |
| gpt-6-luna | 17/20 | 15/20 → 17/20 | 32/40 → 34/40 |
| gpt-6.1-sol | 18/19有效响应，1缺口 | 18/20 → 18/20 | 36/40 → 36/40（36/39有效响应） |
| claude-haiku-5-5 | 17/20 | 10/20 → 15/20 | 27/40 → 32/40 |

## 用户待裁定与遗漏

C1：Luna w1_10 的纯日期 generic fact。
倾向不接受：专用关系可把日期当属性值，但generic fact没有birthday关系，值应保留生日属性以辨明断言；
若用户认为单断言句中只靠完整sentence可定位亦可接受，须明确适用边界，不能顺带接受多断言中的裸名词/日期。
若仅批准该例，Luna第二遍18/20，合并35/40；本稿主表不计此收益。

另有既存评分覆盖缺口：w1_10 的 expect 未评分times，所以times=[]和birthday recurring/event都会过。
“生日数值”是否必须另作事件time不能由核心匹配自行决定；建议与C1一起确认生日样本的字段口径，
在确认前不追溯改分，也不将本表当完整语义正确率。w1_17原relation/value列表存在通用fact=猫的潜在弱分支，
本草案已拆分，当前录制无因该收窄降分项。

## 最小实施范围与回归建议（待批准）

允许：tests/write_stage_eval.rs、tests/eval/w1/samples中涉及题、gen_samples.py、check_samples.py、W1/写入评测README。
不改生产代码、提示词、契约、schema、ID/input、provider、录制、费用与授权账本。
回归：合法长短片段；裸狗/日语/成都/猫拒绝；句外/跨句/不连续/错text-quote拒绝；
错主体/否定/限定语/时间仍失败；专用关系不套此规则；同句不同断言不串配；
时间窗口外/错role/interp仍失败；标签变化不改请求指纹。
标签摘要改变后，续录仍需显式重冻结，不能绕过manifest；本次只离线分析。
