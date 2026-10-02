# W4 的代码侧与运行时离线闭环

依据 contracts.md 2.1–2.5、3.2、3.5。设计经 GPT 复审 0021、0022 确定，用户裁定了三项（见 contracts 第 8 节和下文“例外”）。W2 在 `src/w2.rs`，W3 类型与机械核验在 `src/contract.rs`，W4 在 `src/w4.rs`，W5 与任务持久化在 `src/commit.rs`，库级入口在 `src/enrichment.rs`。通过注入提供者实现离线闭环，尚未接模型或生产分发。

## 1. 输入组装（`assemble`）

入口接收 SQLite store、原文键、可信任务和 W3 已支持的完整候选（含运行时绑定的索引、重判时的已提交记录 ID）。`assemble` 在同一读事务里取得状态与全文命中，返回 `(状态快照, Assembly)`；调用方用这份快照及判定轮次构造 `W4Runtime` 交给 `plan`。W5 在提交事务中重读核对，不用较新的状态为旧 W4 判断补造快照。

- **相关记录**
  - 专用关系：同主体、同关系的全部有效记录，不受相似度截断；预算放不下时走整批待定路径，不截掉必需记录后调用 W4。
  - 通用 `fact`：用候选原句的自然语言词项查询现有 `source_fts` 全文索引（英文词、相邻汉字对及登记别名），沿用原文检索的排序：登记实体命中数降序、短语命中数降序、接收顺序逆序，再以来源 ID、片段起点打破平局。同一实体的多个名称只计一次，各短语只计是否命中，不按出现次数加权。命中片段映射为记录 ID，再按同主体、fact、有效状态和可信任务过滤并去重；不先限制片段数，每个候选最多新增 `budget.facts` 条检索记录。必需关系成员照常展开。多个片段或候选命中同一记录只计一次，已随关系进入输入的记录不占额外命中名额；最终未送入的唯一命中记录计入 `coverage.omitted`。
  - 检索状态按候选索引放在 `Assembly.fact_retrieval`：成功（含零命中）为 `FullText`；索引关闭、缺失、过期或投影不完整时为 `Fallback(原因)`，回落到旧规则（与候选共有至少两个词元、按共有数排序），仍执行相同记录预算且 coverage.complete=false。查询不重建或修复索引。该诊断不改变 W4 契约或存储形状；`fact_index.rs` 的可选派生提示不是本段的检索依赖。
  - 命中记录、重判记录及其全部前身所在 open 关系的成员，连同关系本身，反复展开到不再增加；结果不依赖关系编号顺序。
  - 重判还送入产生该记录的决定替代或列出过的、仍存在且未删除的前身，以及它们所在关系的最新状态（open / resolved / invalidated）。已删除或不存在的前身不作为必需记录、不阻止清除关系缺口；仍存在的前身关联到 invalidated 关系时，该关系照常送入，保留成员 ID 与墓碑，不恢复删除内容。
  - 带其他任务条件的记录不进入输入。关系含任何越界成员时整条不送入，也不由该关系带入其他成员；被挡住的成员数计入 `coverage.omitted`，范围不完整。
  - 重判的记录本身不与自己比较；它是关系成员时仍出现在记录里。
- **记录预算**：`W4Budget { facts, records }` 中 `records` 是实际可送入记录的总数上限，已删除、缺失、越界记录不计。必需集合（专用关系记录、关系成员、重判前身）展开后超限时，记录与关系输入留空、不调用 W4，整批 pending / scope_incomplete；fact 命中按检索顺序逐条尝试加入。第③段另加候选数、关系数、原文 UTF-8 字节预算（第 5 节），不改变上述取数规则；token 和付费调用预算由第④段模型提供者负责。
- **coverage.complete**：专用关系全部取到时为 true；存在通用 `fact` 候选、任务隔离挡住关系或必需集合超限时为 false。已删除记录不计入 omitted。这个值原样写进 W5 组的 `scope_complete`。
- **记录视图（`record_view`）**
  - 有完整候选和更新状态的记录给 CurrentRecordView2：
    - `ended` / `end` 取结束它的决定里保存的生效时间；
    - `history` 是全部前身；
    - `open_relations`、`update` 取当前状态。
  - 其余给 LegacyRecordView：`data` 原样，`evidence` 只含能链接到仍在的原文的引用，`gaps` 如实列出。
  - 已删除的记录不出现。
- **不调用 W4 的路径**：记录与关系输入均为空时，断言直接新增（范围不完整由 coverage 带出），撤回保留为待定（target_unknown），不写入事实；必需集合超限时则所有候选都为 pending / scope_incomplete，不对旧记录执行动作。

## 2. 输出校验（`validate`）

任何一条不满足，整份输出无效、不提交，原文保留。

- 每个候选恰好一个决定；targets 不重复、只能是输入记录；重判的断言不能以自身为目标。
- decided 必须有动作、没有待定原因；pending 的动作为 null、有待定原因，且不解决关系、不带生效时间和冲突范围。
- 目标数量：new / coexist 没有目标；reinforce 恰好一个；其余至少一个。
- 候选类别与动作一致：撤回表达只能 retract，断言不能当撤回用。
- **依据**
  - 只能引用输入里提供的已保存来源。
  - 片段用 `locate_quote` 在原文里定位，并还原为原文写法后才进入 W5。
  - decided 的决定必须有新候选原文的依据；有目标的动作逐目标检查，每个目标都要有它自己来源的依据。
  - 目标是旧记录视图（无法核对完整断言）时，不允许 decided，只能 pending。
- **解决关系**
  - 只能解决输入里 open 的关系，且只由结束或强化目标的动作提出。
  - 按整份输出验证：被解决关系的成员里，本次决定之后最多一个仍然有效。输入中原已 superseded / expired 的成员不计有效；未进入输入的成员无法确认，保守计为有效。两个以上仍有效就不算解决。
  - 同一关系不能解决两次。
- **生效时间**：任何已定动作都可携带；角色只能是事件或有效期，必须与该候选已核验的某个时间完全相同。state_change / correction / refine / retract 用作结束时间，其余动作只保存到决定回执，不据此结束记录；pending 不携带。
- **冲突范围**：只在 conflict 时非空；引用必须指向新候选或某个目标完整记录的有效限定语；时间不能是保存期限。

## 3. 转换为 W5 组（`plan`）

- **候选**：用 `ContractCandidate::project` 投影。投影不了的候选不进入 W5，记入 `held`，候选和原文保留。
- **Episode 准入**在 W4 判定之后：episode_value 为 useful、有绑定的 remember 请求，或 W4 判为对既有记录的更新 / 撤回 / 强化，才进入 W5；否则记入 `held`，只保留原文。
- **动作映射**

  | W4 | W5 |
  | --- | --- |
  | new、coexist | New |
  | reinforce | Reinforce |
  | state_change | StateChange，结束原因按类型取 StateChanged / PreferenceChanged / ConstraintChanged |
  | correction | StateChange（Correction） |
  | refine | StateChange（Refinement）。契约允许 ScopeRefined，但 W4 输出不区分，统一用 Refinement |
  | retract | Retract |
  | conflict | Conflict，关系的 conditions / times 按 `relation_scope` 的引用取出副本 |
  | pending | 断言用 New、撤回用 Retract，带 pending_reason，不对目标执行动作 |

- **决定回执**新增三个字段（为空不序列化，旧组指纹不变）：
  - `basis`：W4 实际选择的完整依据。W5 核对它与 `basis_sources` 的来源集合一致、片段逐字存在；删除来源时清洗。
  - `effective_time`：生效时间的原文与换算结果。换算结果取候选已核验时间里的对应项，不由 W4 给。
  - `relation_scope`：冲突范围的引用。
- **结束时间的精度**
  - 只有“已换算、日精度、恰好一天”的结果才同时写 `effective`；结束类动作把它写入旧 Memory 的结束时间。存在 `effective_time` 时，W5 要求 `effective == effective_time.point()`，包括月精度、未知等情形必须为 None；其余动作只保存这两个决定字段。
  - 月精度、多日区间：不写时间点，`ended = known`，`end` 保留原精度。
  - 无法换算：`ended = end_unknown`。
  - 旧读取路径的保护：结束时间已知但写不成时间点的记录，并入 `W5View` 的“限定语无法表达”集合（附 `end`），按 contract-types.md 第 3 节同样处理。
- **依赖分组**：先按完整 W4 输出分组。两个决定共享目标、共享要解决的关系，或一方的目标 / 已提交记录是另一方所解决关系的成员，就并入同一组（传递闭包）；其余各自成组。组内任何候选投影失败或 Episode 未准入，整组不提交，其余成员记 `Held::DependsOnHeld { candidate }`；独立组照常提交。
- **组 ID**：`原文键#轮次#组内最小候选索引`。同一轮重试得到相同的组；重判换轮次，不会撞上旧回执。
- **快照与 Held**：Held 表示候选未进入 W5，其依赖组没有提交快照；`seen` / `seen_relations` 表示可提交组的判定依据版本。记录快照包含实际输入中的目标、重判前身，以及候选由运行时绑定的已提交记录（即使它不参与自我比较也须核验有效版本）；关系内容快照只保留实际输入的相关关系，不把 `w5_snapshot` 自动带出的越界关系当成 W4 已见。记录所在 open 关系的编号集合仍保留，提交时集合有增减则整组重判。决定要解决的关系及全部成员始终须有快照。
- **重判覆盖**：仍存在且未删除的前身必须都有记录快照，且其全部关系（不论状态）都有实际输入对应的关系快照，已定重判才清除 `update_relation_unclear`。任务隔离挡住关系时保留该缺口；已删除前身不要求快照。关系覆盖与 `scope_complete` 分别判断：通用 fact 范围不足不等于关系输入不足。

## 4. 离线验收（`tests/w4_offline.rs`）

- **夹具**：`tests/eval/w4/gen_fixtures.py` 从期望决定生成 `fixtures/<id>.output.json`，把 `basis_sources` 换成样本里的具体片段，其余字段不变。测试核对夹具与期望决定逐字段一致。样本文件不改。
- **流程**：每个样本在真实 SQLite 里建库（完整记录经真实提交产生，带完整候选回执）→ 运行组装并与样本输入比对 → 夹具输出经校验、转换、提交 → 重开数据库 → 对 `expect.after` 逐字段断言。`call_w4=false` 的两组直接验 code_path。
- **变体**（在测试里由现有样本改出，不新增人工标签）：
  - 结束时间为单日、多日区间、无法换算。
  - 非空冲突范围。
  - 独立决定分组与轮次。
  - Episode 准入。
  - 删除来源后回执清洗。
  - W5 对不一致字段的拒绝。
  - 含越界成员的关系整条不送入；重判不能因数据库里存在关系而清除缺口（覆盖 open / resolved / invalidated）。
  - Held 阻止依赖组部分提交，独立组仍可提交；关系编号不同仍展开到不动点。
  - 已结束成员不计有效，后续原文可显式解决旧关系；重判带出前身及其最新关系状态。
  - 必需记录超限时整批待定；w4_22 删除 s-501 后重判 N 可清除缺口，`records=1` 不误判超限，零预算仍待定且只计未送入的存活记录。
  - retract 可带结束时间；reinforce 等非结束动作只记回执。
- **反例**：23 种不合规输出整份拒绝。
- **范围外**：`after.recall` 涉及 recall.v3，不在这里验收。
- **例外（用户同意）**：w4_09 不比对组装结果。样本为了测试“检索到无关记录时判为 new”，送入了一条按取数规则取不到的其他关系记录；该样本直接用样本输入验证校验、转换与提交。
- **标签状态与验证边界**：样本迁移到 w4.v2 后为 draft，等用户复核。复核前，本测试只说明代码与当前标签一致；“无新回归”仅指约定检查覆盖范围，不代表模型语义、运行时接线或 recall.v3 已验收。

## 5. 运行时库级离线闭环与未接通部分

第③段及④-1/④-2在 `w4_runtime_v1` 上扩展，数据库保持 9，W5 回执格式仍为 `v8`。生产仍使用旧 `commit_candidates`，没有新 MCP 入口。

- **入口**：调用方先显式 `w4_start_round`，再调用 `enrichment::run_source(store, key, budget, upstream, judge)`。流程为认领 → 恢复检查 → 保存输入 → `assemble` → 提供者输出或 `assembly.skipped` → `plan`（内部已 `validate`）→ 保存计划 → 逐组提交 → 汇总。提供者调用期间不持有数据库事务。终态用 `enrichment::receipt` 读取，普通读取不重新认领或判定。
- **提供者**：上游接收 `ProviderContext` 与 `SourceInput { text, history }`，返回 `UpstreamBatch { w2, review }`，包括所有 W2 ok/unresolved/rejected 候选、原始索引和请求映射，以及 W3 verdict、problems、episode_value、coverage 和 request_reviews；不返回准入布尔缺省、committed_id、Plan、组编号或提交快照。`review` 由代码 `W3Output::validate` 产生，核对候选（含 unresolved，排除 rejected）和请求一一对应、无重复、所有 Quote 逐字存在且还原、supported 的 problems 为空、非 Episode 的 episode_value 为 not_applicable。核验绑定最终完整 W2 结果（含时间换算、请求索引映射）和运行时来源快照，保存后重新加载也复验；改变输入不得沿用核验。W1 为空时 review 必须为 None；全部被 W2 拒收时允许不调用 W3，请求仍未核验。只有 W2 ok 且 W3 supported、未被控制请求阻断的候选进入 W4/W5。运行时由 episode_value 和已核验 remember 的最终索引映射计算 Admission，绑定历史产物，`plan` 生成证据、组和快照。
- **目录与 W4 注入**：`w2::relation_catalog` 从现有谓词注册定义和 memory_types 约束构造目录，始终含 fact。没有专用类型约束时沿用现有目录的四种允许类型；有约束则原样使用，不把未知谓词加入目录。W4 提供者接收同一上下文、实际 `W4Input` 和按来源 ID 去重的原文及历史，返回 `Some(W4Output)` 或 None（未接入）。上下文包含 owner、完整来源绑定、可信任务、接收时间/序号、round、operation_id、需要重新核验时可复用的 previous_w2。测试注入合成结果或夹具，生产代码不读夹具。④-3/④-4在这两个位置接模型；阶段缓存、token/付费预算归提供者，同操作同输入复用结果，结果缺失不自动重发。
- **轮次**：首轮为 0；显式重判才加一，请求 ID 绑定稳定的 `operation_id`。启动回执复用 `TurnReceipt`；同参重放返回原轮次，即使之后已开始其他轮次也不重复加一。显式新轮清除旧输入元数据和汇总。
- **检查点**：`SourceRecord.result.w4_checkpoint` 保存完整来源输入绑定、进入 W4 的候选和 `Plan { groups, held }`。④-2的 `w4_batch` 保存完整上游批次与来源绑定，包括被过滤候选及核验依据；`w4_controls` 独立保存待确认限制和尚未执行的留存请求。它们是现有 result 的内部扩展，不改变表或快照顶层形状。批次、控制标记、W4 输入及准入在同一事务保存，同轮不可替换。低层离线接口 `w4_save_inputs` 仍可单独保存 W4 候选供既有恢复测试使用。旁侧 `w4_input_meta` 只含 round、ready、Admission 布尔值、计数和内部诊断，不存原文副本。首组提交前保存完整不可变计划，写锁内核对组 ID、完整候选和分组覆盖。`w4_progress` 只从计划与 W5 回执推导进度，不另存游标。
- **续跑**：可认领 queued、过期 running、failed、budget_exhausted、stale_requires_review，以及尚无计划的 pending/coverage_gaps；同轮只换 attempt/租约，round/operation_id 不变。输入已保存就不调用上游，但仍检查 W3 输入绑定；已有计划时两个提供者都不调用，也不重新检查输入制作预算。全部组已提交但未汇总时只补总状态。需要 W4 而提供者返回 None 时，保留完整批次、释放认领、零业务提交；普通状态为 pending，有 coverage 或控制限制时保留相应汇总状态。内部诊断为 capability_unavailable，不新增冻结失败码，不回落旧策略。
- **输入预算**：`RuntimeInputBudget` 独立于 `W4Budget`，缺省候选 64、关系 64、原文 262144 字节（facts=64、records=256）。上游前检查当前原文及历史；上游后先保存完整批次，再按所有 W1 候选数检查（含 W2 拒收、W3 不支持的项）；assemble 后、W4 前检查实际关系数和实际提供的所有来源原文及历史字节，按来源去重。等于上限允许，超限为 budget_exhausted，诊断保存 item / actual / limit，保留 T1 与已获得候选，不截断后继续。必需记录超限仍沿用 scope_incomplete 的旧规则。
- **回执**：专用转换只返回 contracts 3.5 的 source/enrichment 片段，不展开内部 result，不复用旧 `source_response`。W2 拒收为 rejected 并带 candidate_rejected 原因；unresolved 保留 unresolved_subject；W3 contradicted 为 rejected，insufficient 为 pending，分别带 review_contradicted/review_insufficient。未进入 W4 的 update=null。新增、复用、归并及强化成功为 committed，绑定实际记录 ID；强化取本次回执的更新状态，不能用目标旧状态替代。执行撤回为 committed、memory_id=null，多目标留在 W5 回执；无目标撤回和 Held 为 pending。来源收尾优先保留 needs_rejudge，其后为 awaiting_confirmation、coverage_gaps；否则存在未提交候选为 pending，全提交为 committed。只有原始 W1 候选为空才是 no_candidates_unreviewed；全部被过滤不等于空 W1，completed_no_facts 本段不产生。W3 问题片段、episode_value、coverage.missing 和准入依据持久保留在内部批次，外部候选项仍是冻结字段。
- **请求与暂停**：memory_requests 和 retention_pending 由真实批次及独立控制标记生成。请求项保留原请求、原始目标索引、最终索引映射、被拒索引、verdict、resolved_target、deadline_resolution、status 和 executed=false。已支持且对象可定位的 retention 为 retention_pending，返回对象和期限原话，不设置过期时间或声称执行。do_not_store/forget 及未获核验或对象未知的 retention 为 awaiting_confirmation；候选目标只阻断相关索引，source 或未知目标暂停整源。旧 facts 的空索引只能交已有控制流程按原话定位，本入口不扩大成全部事实、不自动删除。W1 为空时 remember 仍为 unreviewed；疑似控制请求仍独立暂停。w4_controls 不因显式重判或后续 W3 verdict 自动解除；确认和恢复的宿主控制入口不在本段实现。
- **顺序与归属**：首次 `order_sequence = received_sequence`，显式重判分配新序号，保留接收序号和 observed_at。两条水位线在事务内核对，水位相等须有本轮匹配的已提交回执；其他来源不能借用许可。更新来源或无来源变更仍阻止余组，已提交候选保留真实结果。输入保存、计划保存、提交、续租和收尾均核对版本、轮次、operation、attempt、租约及完整来源输入；不用全局 revision 等值检查阻止自身余组。旧执行者不能覆盖或释放新认领，旧入口拒抢新版本任务。
- **重判复用**：新轮继承 W4 候选，从上一轮回执绑定 committed_id，原有绑定不能丢弃或替换。本轮暂停或拒收的已提交候选仍保留在完整批次，后续轮次从本来源各轮已提交 W5 回执恢复最新产物绑定；不另存进度或把暂停候选变成无 ID 的新增。低层接口没有完整批次时仍禁止移除已有绑定。完整批次来源绑定不变时，复用已经核验的 W2/W3，绝不重新提取后按索引套用产物；来源快照变化时通过 previous_w2 将最终候选与请求交提供者重新核验 W3，禁止替换该 W2 批次，旧 W3 绑定会被拒。W5 继续核对产物的来源、完整候选、有效状态及快照；失效或归并退出的产物不能回落成无 ID 的新增。新尝试仍须显式重判，不自动再次调用模型。
- **删除与验证**：清空自身 result（含批次、控制标记和输入元数据）和认领；清洗他处候选、W3 binding/问题/覆盖片段、请求、来源历史副本、依据和编码快照，受影响计划整体失效，进入 needs_rejudge。旁侧元数据没有原文副本；新轮才重建。保留历史组 ID 供审计，不执行失效旧组。重判可读取失效关系的来源 ID 与墓碑，但不向提供者附已删除原文；输出若仍把它当动作依据，validate / W5 继续拒绝。`tests/enrichment_batch.rs`、`tests/enrichment_runtime.rs` 与 `tests/w4_recovery.rs` 用真实 SQLite 关闭重开覆盖完整批次、绑定、控制暂停、T1、注入输出、预算、恢复窗口、屏障、争抢、删除和回执映射。

尚未接通：W1 / W3 / W4 模型调用及模型阶段检查点、后台版本分发与生产切换、recall.v3。检索 full_text / fallback 仅记入新路径的内部输入诊断。继续使用已有 `w1:<来源 ID>:<候选索引>` 证据 ID；旧 ID、处理版本输出与回执保持原样。关系只由 W4 决定或用户确认解决，不因成员进入历史自动关闭。
