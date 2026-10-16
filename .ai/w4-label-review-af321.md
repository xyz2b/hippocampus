# AF321 W4首遍评审草案（未修改标签）

零调用；75条最终响应按request指纹/repeat/raw_output与录制核对一致。Sol有7次先前传输失败，按最终有效响应评分，不覆盖历史。

## 最小A类修订

五样本：11、13、14_complete、14_rejudge、n1a。保留原null分支，新增完整分支：effective改为输入候选已核验的validity/unknown（前四项引“现在”，n1a引“不再”）；其余decisions/after/recall/must_not完全不变。不得泛化为任意unknown或任意时间。输入、请求指纹不变，标签摘要变化须显式重冻结。

依据：contracts 2.4（236行）允许已核验event/validity；2.5/RecordView2（276/353行）要求unknown保持end_unknown、不补日期；src/w4.rs:257–270和657–675保留已核验时间及未知换算。运行时通过，不等于所有时间都可接受，只接受上述明确的未知有效期分支。

## C类待裁定（不计入草案通过）

Sol 04/coexist填“周末的时候”、23/reinforce填“每天”。src/w4.rs:257注释明确非结束动作允许effective、仅写回执；契约2.4没有按动作禁止。然而src/write_models.rs:295–299又写“明确给出change effective time才使用”。建议明确此句对非结束动作的含义。我倾向接受这两个精确完整分支，因无伪造时间且不改旧记录；不得推广到结束动作。如果坚持effective专指变化时间，则保持失败并明确提示词。两项均接受时Sol为22/25；当前只算20。

## 不修的标签及根因

17已明确裁定pending；不能同时在学不明却认定coexist。20未指明撤回哪个住址；DS撤67错误，Sol/Luna用unclear_relation错误，target_unknown不可互换。22只讲新住址，不澄清历史两住址谁真；contracts 2.5:279、提示词301–309禁止因替换解决冲突。上述after失败是模型动作的真实后果，不是运行时评分器误报。n1b从未任职明确推翻旧值，保持correction。

Luna有15/25条非法current来源；提示词271–274要求stored来源，w4.validate拒绝正确。新方依据补入追加合法引用，不替换/删除模型非法引用。禁止用标签放宽修复。

## 全部失败逐条归因

|档位|题|类|失败字段|运行时|解释|
|---|---|---|---|---|---|
|ds-pro|w4_11_preference_strength|A|decisions[0].effective|通过|已核验 validity/unknown 原样引用，标签只接受null过严；after通过|
|ds-pro|w4_13_state_change_date_unknown|A|decisions[0].effective|通过|已核验 validity/unknown 原样引用，标签只接受null过严；after通过|
|ds-pro|w4_14_complete_rejudge|A|decisions[0].effective|通过|已核验 validity/unknown 原样引用，标签只接受null过严；after通过|
|ds-pro|w4_16_pending_scope_incomplete|B|decisions[0].status,decisions[0].action,decisions[0].pending_reason|通过|排他更新所需范围不完整，不能以 new/coexist 代替 pending|
|ds-pro|w4_17_pending_unclear_relation|B|decisions[0].status,decisions[0].action,decisions[0].pending_reason|失败|关系未明却判 coexist/new；DS/Sol 少 update_relation_unclear|
|ds-pro|w4_20_pending_target_unknown|B|decisions[0].status,decisions[0].action,decisions[0].pending_reason,decisions[0].targets|失败|DS 擅撤回67；Sol/Luna 错将 target_unknown 判 unclear_relation；Sol 两个候选目标不等于找到所指目标|
|ds-pro|w4_22_replace_conflict_members|B|resolves_relations|失败|擅自解决 rel-2；替换旧值不等于澄清历史冲突，judged 是模型决定的错误后果|
|gpt-6.1-sol|w4_04_coexist_partial|C|decisions[0].effective|通过|非结束动作的周期effective：契约/代码允许，提示词change time限定有歧义；暂不通过|
|gpt-6.1-sol|w4_11_preference_strength|A|decisions[0].effective|通过|已核验 validity/unknown 原样引用，标签只接受null过严；after通过|
|gpt-6.1-sol|w4_13_state_change_date_unknown|A|decisions[0].effective|通过|已核验 validity/unknown 原样引用，标签只接受null过严；after通过|
|gpt-6.1-sol|w4_14_complete_rejudge|A|decisions[0].effective|通过|已核验 validity/unknown 原样引用，标签只接受null过严；after通过|
|gpt-6.1-sol|w4_14_rejudge_committed|A|decisions[0].effective|通过|已核验 validity/unknown 原样引用，标签只接受null过严；after通过|
|gpt-6.1-sol|w4_17_pending_unclear_relation|B|decisions[0].status,decisions[0].action,decisions[0].pending_reason|失败|关系未明却判 coexist/new；DS/Sol 少 update_relation_unclear|
|gpt-6.1-sol|w4_20_pending_target_unknown|B|decisions[0].pending_reason,decisions[0].targets|失败|DS 擅撤回67；Sol/Luna 错将 target_unknown 判 unclear_relation；Sol 两个候选目标不等于找到所指目标|
|gpt-6.1-sol|w4_22_replace_conflict_members|B|resolves_relations|失败|擅自解决 rel-2；替换旧值不等于澄清历史冲突，judged 是模型决定的错误后果|
|gpt-6.1-sol|w4_23_rejudge_merge|C|decisions[0].effective|通过|非结束动作的周期effective：契约/代码允许，提示词change time限定有歧义；暂不通过|
|gpt-6.1-sol|w4_n1a_negative_state_change|A|decisions[0].effective|通过|已核验 validity/unknown 原样引用，标签只接受null过严；after通过|
|gpt-6-luna|w4_01_state_change|B|decisions[0].effective|失败|漏明确 effective，结束时间错误|
|gpt-6-luna|w4_03_refine_whole|B|w4.validate,decisions[0].action,decisions[0].effective,decisions[0].targets,decisions[0].basis_sources|未执行|整条修订错判 coexist 而非 refine；stored-only机械核验失败，不可豁免|
|gpt-6-luna|w4_04_coexist_partial|B|w4.validate,decisions[0].effective|未执行|持久依据非法使用current；stored-only机械核验失败，不可豁免|
|gpt-6-luna|w4_05_retract|B|w4.validate|未执行|持久依据非法使用current；stored-only机械核验失败，不可豁免|
|gpt-6-luna|w4_10_reinforce|B|w4.validate|未执行|持久依据非法使用current；stored-only机械核验失败，不可豁免|
|gpt-6-luna|w4_11_preference_strength|B|w4.validate,decisions[0].effective|未执行|持久依据非法使用current；stored-only机械核验失败，不可豁免|
|gpt-6-luna|w4_14_complete_rejudge|A|decisions[0].effective|通过|已核验 validity/unknown 原样引用，标签只接受null过严；after通过|
|gpt-6-luna|w4_14_rejudge_committed|B|w4.validate,decisions[0].effective|未执行|持久依据非法使用current；stored-only机械核验失败，不可豁免|
|gpt-6-luna|w4_15_resolve_conflict|B|w4.validate,resolves_relations|未执行|漏解决 rel-1；stored-only机械核验失败，不可豁免|
|gpt-6-luna|w4_16_pending_scope_incomplete|B|w4.validate,decisions[0].status,decisions[0].action,decisions[0].pending_reason|未执行|排他更新所需范围不完整，不能以 new/coexist 代替 pending；stored-only机械核验失败，不可豁免|
|gpt-6-luna|w4_17_pending_unclear_relation|B|w4.validate,decisions[0].status,decisions[0].action,decisions[0].pending_reason|未执行|关系未明却判 coexist/new；DS/Sol 少 update_relation_unclear；stored-only机械核验失败，不可豁免|
|gpt-6-luna|w4_18_legacy_gaps|B|w4.validate|未执行|持久依据非法使用current；stored-only机械核验失败，不可豁免|
|gpt-6-luna|w4_20_pending_target_unknown|B|w4.validate,decisions[0].pending_reason|未执行|DS 擅撤回67；Sol/Luna 错将 target_unknown 判 unclear_relation；Sol 两个候选目标不等于找到所指目标；stored-only机械核验失败，不可豁免|
|gpt-6-luna|w4_21_confirm_conflict_side|B|w4.validate,decisions[0].action,decisions[0].targets,decisions[0].basis_sources,decisions[1].status,decisions[1].action,decisions[1].pending_reason,decisions[1].basis_sources|未执行|应 refine[51]+retract[50]，却 correction[50]+pending；stored-only机械核验失败，不可豁免|
|gpt-6-luna|w4_22_replace_conflict_members|B|w4.validate,resolves_relations|未执行|擅自解决 rel-2；替换旧值不等于澄清历史冲突，judged 是模型决定的错误后果；stored-only机械核验失败，不可豁免|
|gpt-6-luna|w4_23_rejudge_merge|B|w4.validate|未执行|持久依据非法使用current；stored-only机械核验失败，不可豁免|
|gpt-6-luna|w4_n1a_negative_state_change|A|decisions[0].effective|通过|已核验 validity/unknown 原样引用，标签只接受null过严；after通过|
|gpt-6-luna|w4_n1b_negative_correction|B|decisions[0].action,decisions[0].relation_scope|失败|明确否定旧值曾成立，应 correction；conflict 留下旧值未结束|
|gpt-6-luna|w4_n2_sibling_fragments|B|w4.validate|未执行|持久依据非法使用current；stored-only机械核验失败，不可豁免|

## 草案离线重评

仅在内存替换A类精确期望；其余字段、既有机械核验和SQLite after结果原样保留。未重跑Rust。结果 `target/eval-providers/compare-af319/reports-af321/review.json`。

|档位|原通过|草案通过|A/B/C|费用元|p50秒|输出总量/均值|
|---|---:|---:|---|---:|---:|---|
|ds-pro|18/25|21/25|{'A': 3, 'B': 4}|1.8737|23.68|59180/2367.20|
|gpt-6.1-sol|15/25|20/25|{'C': 2, 'A': 5, 'B': 3}|1.2618|24.78|5320/212.80|
|gpt-6-luna|6/25|8/25|{'B': 17, 'A': 2}|0.0696|14.90|7154/286.16|

## 下一步及范围

先裁定A/C并修标签，再考虑DS/Sol第二遍各25题（50槽），按首遍费用约3.14元，继续15元预留上限。Luna暂缓；15条机械失败使同配置第二遍价值低。现有AF319入口固定三档、不支持档位筛选：旧repeat2命令仍选75槽，须先另批最小筛选改动（保留原150槽manifest与账本，不删旧槽）。本次不改工具、不发送。

最小后续允许文件：五个A样本、经裁定的两个C样本、生成器、必要检查器/README/零调用回归；已有完整分支评分可复用。无生产/契约/模型版本修改。尚未全面人工复核75条的must_not及召回语义，自动分不能当作人工确认分。
