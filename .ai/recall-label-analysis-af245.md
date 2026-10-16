# Claude 分析（AF245，待 Codex 复核）：召回对比中多档共同失败样本（AF230）

## R1
| 样本 | 原文 | 标签 | 模型（几乎全部） | Claude 判断 |
|---|---|---|---|---|
| r1_02 | 我今天去了健身房。 | need=context_only | judge | 陈述句可能涉及已有记录（如健身习惯）的新旧关系，契约 3.1/target-design 允许“需要判断事实支持、新旧关系”时 judge；建议两支都接受（同 r1_12 裁定），均须 assertion_present。filter.relation=visited 是否扣分另议（fact/None 标签过窄？visited 合法）。 |
| r1_17 | 以后回答我请用英文。 | context_only | judge | 同上，偏好陈述可能与已有回答语言记录比对；建议两支都接受。 |
| r1_03 | 我对什么过敏？ | aggregate/list | single/lookup | 标签语义正确：问全部过敏，lookup 走 top-k 可能漏项（安全相关）。保留标签，列为模型/提示词问题；但需确认 R1 提示词是否清楚“问全部→list”。 |
| r1_11 | 周二我一般几点去健身房？ | conditions 含“周二” | conditions=[]，部分 type=temporal、周二进 time | W1 规则“日历周期/明确日程属 times，活动条件属 conditions”——“周二”是循环日历时间，进 time(recurring) 更符合现规则。建议接受“周二在 conditions 或在 time”两支。 |
| r1_15 | 我现在写代码用什么编辑器？ | conditions 含“写代码” | [] | “写代码”是所问工具的用途，属关系/查询主体内容而非适用条件；建议 conditions 不再必需（可选）。 |

## R4
| 样本 | 标签 | 模型 | Claude 判断 |
|---|---|---|---|
| r4_03 条件不匹配 | verdict=supported，fact 带条件、applies unknown/no | verdict=insufficient，同样给出带条件事实、applies=unknown | 问题问一般场景，证据只覆盖速写场景，整体 insufficient 合理；建议 verdict 两支（supported/insufficient），事实与条件要求不变。 |
| r4_07 当前自述 | forbid_facts：旧记录 7 的 current 不得为 yes/no | 多数给“用户曾表示住在杭州 current=no”，部分 supersedes [7] | 用户裁定“只接受支持住在 B，旧值可不提或标不确定，不推断搬家/冲突/supersedes”。但无论搬家还是纠错，旧值都不是当前值，current=no 并不推断变化性质；真正违规的是“此前住在杭州”这类把旧值当真实历史的说法（若是纠错则错误）。建议：允许旧记录 current=no，只禁止 current=yes、禁止 supersedes 与断言旧值为真实历史（人工）。**需用户重新确认**（改变 AF227 裁定的边界）。 |
| r4_21 计划/预订（新样本） | verdict=insufficient；facts[2]/[3] polarity=negate | ds-pro/qwen supported；luna polarity affirm（“已取消计划”“车票还没退”） | 极性表达有两种合法写法（“取消了计划”可作肯定陈述，也可作计划的否定）；需按记录 103/104 的实际 Candidate 极性核对标签。verdict 视问题而定，请 Codex 核对。 |

## 处理
修订仅改标签/完整分支，不改输入（不改请求指纹），用现有录制离线重评；r4_07 待用户确认。之后进行人工复核。
