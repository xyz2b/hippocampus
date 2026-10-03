# W4 的代码侧与运行时离线闭环

依据 contracts.md 2.1–2.5、3.2、3.5。设计经 GPT 复审 0021、0022 确定，用户裁定了三项（见 contracts 第 8 节和下文“例外”）。W2 在 `src/w2.rs`，W3 类型与机械核验在 `src/contract.rs`，W4 在 `src/w4.rs`，W5 与任务持久化在 `src/commit.rs`，库级入口在 `src/enrichment.rs`。注入提供者实现离线闭环；④-3/④-4 增加显式 W1/W3/W4 模型入口，④-5 接通 opt-in 配置与 CLI 分发。

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

第③段及④-1～④-5在 `w4_runtime_v1` 上扩展，数据库保持 9，W5 回执格式仍为 `v8`。默认使用旧 `commit_candidates`；新路径通过 opt-in 配置与现有 CLI 接通，没有新 MCP 入口（见第 8 节）。

- **入口**：调用方先显式 `w4_start_round`，再调用 `enrichment::run_source(store, key, budget, upstream, judge)`。流程为认领 → 恢复检查 → 保存输入 → `assemble` → 提供者输出或 `assembly.skipped` → `prepare_output`（共享组装函数后严格校验） → `plan`（内部再次 `validate`）→ 保存计划 → 逐组提交 → 汇总。提供者调用期间不持有数据库事务。终态用 `enrichment::receipt` 读取，普通读取不重新认领或判定。
- **提供者**：上游接收 `ProviderContext` 与 `SourceInput { text, history }`，返回 `UpstreamBatch { w2, review }`，包括所有 W2 ok/unresolved/rejected 候选、原始索引和请求映射，以及 W3 verdict、problems、episode_value、coverage 和 request_reviews；不返回准入布尔缺省、committed_id、Plan、组编号或提交快照。`review` 由代码 `W3Output::validate` 产生，核对候选（含 unresolved，排除 rejected）和请求一一对应、无重复、所有 Quote 逐字存在且还原、supported 的 problems 为空、非 Episode 的 episode_value 为 not_applicable。核验绑定最终完整 W2 结果（含时间换算、请求索引映射）和运行时来源快照，保存后重新加载也复验；改变输入不得沿用核验。W1 为空时 review 必须为 None；全部被 W2 拒收时允许不调用 W3，请求仍未核验。只有 W2 ok 且 W3 supported、未被控制请求阻断的候选进入 W4/W5。运行时由 episode_value 和已核验 remember 的最终索引映射计算 Admission，绑定历史产物，`plan` 生成证据、组和快照。
- **目录与 W4 注入**：`w2::relation_catalog` 从现有谓词注册定义和 memory_types 约束构造目录，始终含 fact。没有专用类型约束时沿用现有目录的四种允许类型；有约束则原样使用，不把未知谓词加入目录。W4 提供者接收同一上下文、实际 `W4Input` 和按来源 ID 去重的原文及历史，返回 `Some(W4Output)` 或 None（未接入）。上下文包含 owner、完整来源绑定、可信任务、接收时间/序号、round、operation_id、需要重新核验时可复用的 previous_w2。测试注入合成结果或夹具，生产代码不读夹具。④-3 已在上游位置接 W1/W3，④-4 已在判定位置接 W4；阶段缓存、token/付费预算归提供者，同操作同输入复用结果，结果缺失不自动重发（见第 6 节）。
- **轮次**：首轮为 0；显式重判才加一，请求 ID 绑定稳定的 `operation_id`。启动回执复用 `TurnReceipt`；同参重放返回原轮次，即使之后已开始其他轮次也不重复加一。显式新轮清除旧输入元数据和汇总。
- **检查点**：`SourceRecord.result.w4_checkpoint` 保存完整来源输入绑定、进入 W4 的候选和 `Plan { groups, held }`。④-2的 `w4_batch` 保存完整上游批次与来源绑定，包括被过滤候选及核验依据；`w4_controls` 独立保存待确认限制和尚未执行的留存请求。它们是现有 result 的内部扩展，不改变表或快照顶层形状。批次、控制标记、W4 输入及准入在同一事务保存，同轮不可替换。低层离线接口 `w4_save_inputs` 仍可单独保存 W4 候选供既有恢复测试使用。④-3/④-4 的 `w4_model_stages` 保存模型阶段输入绑定与输出（见第 6、7 节）。旁侧 `w4_input_meta` 只含 round、ready、Admission 布尔值、计数和内部诊断，不存原文副本。首组提交前保存完整不可变计划，写锁内核对组 ID、完整候选和分组覆盖。`w4_progress` 只从计划与 W5 回执推导进度，不另存游标。
- **续跑**：可认领 queued、过期 running、failed、budget_exhausted、stale_requires_review，以及尚无计划的 pending/coverage_gaps；同轮只换 attempt/租约，round/operation_id 不变。离线注入入口输入已保存就不调用上游，但仍检查 W3 输入绑定；已有计划时两个注入提供者都不调用，也不重新检查输入制作预算。显式模型入口仍核对目录、时区和阶段绑定，复验缓存，不重发已完成阶段。全部组已提交但未汇总时只补总状态。需要 W4 而提供者返回 None 时，保留完整批次、释放认领、零业务提交；普通状态为 pending，有 coverage 或控制限制时保留相应汇总状态。内部诊断为 capability_unavailable，不新增冻结失败码，不回落旧策略。
- **输入预算**：`RuntimeInputBudget` 独立于 `W4Budget`，缺省候选 64、关系 64、原文 262144 字节（facts=64、records=256）。上游前检查当前原文及历史；上游后先保存完整批次，再按所有 W1 候选数检查（含 W2 拒收、W3 不支持的项）；assemble 后、W4 前检查实际关系数和实际提供的所有来源原文及历史字节，按来源去重。等于上限允许，超限为 budget_exhausted，诊断保存 item / actual / limit，保留 T1 与已获得候选，不截断后继续。必需记录超限仍沿用 scope_incomplete 的旧规则。
- **回执**：专用转换只返回 contracts 3.5 的 source/enrichment 片段，不展开内部 result，不复用旧 `source_response`。W2 拒收为 rejected 并带 candidate_rejected 原因；unresolved 保留 unresolved_subject；W3 contradicted 为 rejected，insufficient 为 pending，分别带 review_contradicted/review_insufficient。未进入 W4 的 update=null。新增、复用、归并及强化成功为 committed，绑定实际记录 ID；强化取本次回执的更新状态，不能用目标旧状态替代。执行撤回为 committed、memory_id=null，多目标留在 W5 回执；无目标撤回和 Held 为 pending。来源收尾优先保留 needs_rejudge，其后为 awaiting_confirmation、coverage_gaps；否则存在未提交候选为 pending，全提交为 committed。只有原始 W1 候选为空才是 no_candidates_unreviewed；全部被过滤不等于空 W1，completed_no_facts 本段不产生。W3 问题片段、episode_value、coverage.missing 和准入依据持久保留在内部批次，外部候选项仍是冻结字段。
- **请求与暂停**：memory_requests 和 retention_pending 由真实批次及独立控制标记生成。请求项保留原请求、原始目标索引、最终索引映射、被拒索引、verdict、resolved_target、deadline_resolution、status 和 executed=false。已支持且对象可定位的 retention 为 retention_pending，返回对象和期限原话，不设置过期时间或声称执行。do_not_store/forget 及未获核验或对象未知的 retention 为 awaiting_confirmation；候选目标只阻断相关索引，source 或未知目标暂停整源。旧 facts 的空索引只能交已有控制流程按原话定位，本入口不扩大成全部事实、不自动删除。W1 为空时 remember 仍为 unreviewed；疑似控制请求仍独立暂停。w4_controls 不因显式重判或后续 W3 verdict 自动解除；确认和恢复的宿主控制入口不在本段实现。
- **顺序与归属**：首次 `order_sequence = received_sequence`，显式重判分配新序号，保留接收序号和 observed_at。两条水位线在事务内核对，水位相等须有本轮匹配的已提交回执；其他来源不能借用许可。更新来源或无来源变更仍阻止余组，已提交候选保留真实结果。输入保存、计划保存、提交、续租和收尾均核对版本、轮次、operation、attempt、租约及完整来源输入；不用全局 revision 等值检查阻止自身余组。旧执行者不能覆盖或释放新认领，旧入口拒抢新版本任务。
- **重判复用**：新轮继承 W4 候选，从上一轮回执绑定 committed_id，原有绑定不能丢弃或替换。本轮暂停或拒收的已提交候选仍保留在完整批次，后续轮次从本来源各轮已提交 W5 回执恢复最新产物绑定；不另存进度或把暂停候选变成无 ID 的新增。低层接口没有完整批次时仍禁止移除已有绑定。完整批次来源绑定不变时，复用已经核验的 W2/W3，绝不重新提取后按索引套用产物；来源快照变化时通过 previous_w2 将最终候选与请求交提供者重新核验 W3，禁止替换该 W2 批次，旧 W3 绑定会被拒。W5 继续核对产物的来源、完整候选、有效状态及快照；失效或归并退出的产物不能回落成无 ID 的新增。新尝试仍须显式重判，不自动再次调用模型。
- **删除与验证**：清空自身 result（含批次、控制标记、模型阶段结果和输入元数据）和认领；清洗他处候选、W3 binding/问题/覆盖片段、请求、来源历史副本、依据和编码快照，受影响计划整体失效，进入 needs_rejudge。旁侧元数据没有原文副本；新轮才重建。保留历史组 ID 供审计，不执行失效旧组。重判可读取失效关系的来源 ID 与墓碑，但不向提供者附已删除原文；输出若仍把它当动作依据，validate / W5 继续拒绝。`tests/enrichment_batch.rs`、`tests/enrichment_runtime.rs` 与 `tests/w4_recovery.rs` 用真实 SQLite 关闭重开覆盖完整批次、绑定、控制暂停、T1、注入输出、预算、恢复窗口、屏障、争抢、删除和回执映射。

尚未接通：新路径 inline 与 MCP 后台执行、默认切换、recall.v3。检索 full_text / fallback 仅记入新路径的内部输入诊断。继续使用已有 `w1:<来源 ID>:<候选索引>` 证据 ID；旧 ID、处理版本输出与回执保持原样。关系只由 W4 决定或用户确认解决，不因成员进入历史自动关闭。

## 6. W1/W3 模型提供者（④-3）

- **入口与提示词**：`src/write_models.rs`（从 `openai.rs` 导出为 `write_models`）集中保存 `W1_PROMPT`、`W3_PROMPT` 和 strict schema。`WriteModelProvider` 接收显式客户端与实体/谓词目录；`run_source_with_models` 使用现有认领、预算、assemble、plan、W5 和收尾流程，原 `run_source` 离线注入接口保留。该接口保留 W4 注入；④-4 的完整模型入口见第 7 节，④-5 的配置接线见第 8 节。未知主体要求 unresolved，不猜实体；记忆请求整条一次，原文及目录文本均无指令权限。
- **调用与核验**：W1 单次发送后用 `w2::decode` 严格解码，再按接收时间和用户时区执行 W2；关系目录始终用 `w2::relation_catalog`。W3 只送 W2 未拒收项（含 unresolved）、时间换算及整条请求，保留原始索引；调用一次并用 `W3Output::validate` 核验后构成 `UpstreamBatch`。空 W1 或全部 W2 拒收均不调用 W3，请求仍按既有未核验控制规则呈现。显式重判保留最终候选与请求，重新机械核对；不重新提取并套旧产物索引。
- **阶段检查点**：`result.w4_model_stages.w1/w3` 保存 operation_id、完整 JSON 输入绑定和已解码输出，不另建表或升库。绑定包括契约版本、来源快照、原文/必要历史、实体及关系目录、时区、said_on、可信任务；W3 另绑定完整最终 W2（候选、请求、原始/最终索引、换算）。每次模型入口续跑均核对阶段绑定，即使已有完整批次/计划也不能绕过。缓存重开后重新解码和校验；同操作输入变化要求 needs_rejudge。同输入的显式重判沿用已核验结果并绑定新操作；输入变化不能沿用旧结果，重新核验 W3 仍使用冻结最终候选。
- **预算与失败**：沿用既有 HTTP 发送、网络门禁、阶段超时、输出 token 限额、用量结算和持久预留；测试在 HTTP 执行位置注入假响应。新增 w1/w3/w4 后台计数，与旧前台/旧链路操作计数分开，共享 group 总额度；客户端可在 `with_budget` 后用 `with_background_operation_limit(1..3)` 独立限制后台；缺省为 `min(max_operation_calls,3)`，公开配置结构和修复链不变；新输出额度的授权绑定见第 9 节，旧授权可继续旧阶段。显式额度保存在同一授权的 policy_json（内部 `max_background_operation_calls`），使用该 group 的客户端须一致配置。同一授权可设 `max_operation_calls=2` 并调用 `with_background_operation_limit(3)`，前后台额度不互占而共享总预算；生产前台 R1/R4 随相应路径接线，未在本段改旧链路。后台同 owner/operation 的阶段次数跨授权 group 核对，不能换 group 重置。超时、无效 JSON/schema、HTTP 失败均不重试、不退款。调用记录存在而阶段输出缺失时为 failed，`source.error=model_invalid_output`，内部诊断 `detail_code=model_result_missing`；新预算尝试须显式重判，不自动重发。错误只保存脱敏代码，外部回执仍为冻结 3.5 字段。
- **租约与删除**：阶段加载、保存使用与 W5 相同的 attempt、租约、round、operation、来源输入检查；晚到结果不能覆盖继任认领或恢复删除。删除自身来源清空阶段结果，他处阶段输入/输出中的原文和历史副本沿用检查点清洗并使计划失效。`src/write_models_tests.rs` 使用真实 SQLite、关闭重开及进程内假 HTTP 覆盖发送计数、失败/缺结果、输入变化、预算、认领替换和删除；不调用真实模型。

## 7. W4 模型提供者（④-4）

- **入口与约束**：`enrichment::run_source_with_all_models(store, key, budget, provider)` 将同一个 `WriteModelProvider` 接到 W1/W3 上游和 W4 判定位置；保留 `run_source` 与 `run_source_with_models` 的注入接口。`W4_PROMPT` 与 `w4_schema()` 在 `src/write_models.rs`，覆盖 w4.v2 动作、逐候选决定、目标数量、新旧双方依据、legacy 待定、范围缺口、关系解决、生效时间及 QualifierRef 冲突范围；原文和历史没有指令权限。提示词不放样本原文、ID 或说明标签，静态测试扫描 W1/W3/W4 samples 的原文、ID 和说明标签。
- **输入与阶段结果**：W4 请求传入 `contract=w4.v2`、实际 `w4_input` 和按来源 ID 去重的 `sources { text, history }`；完整输入 JSON 作为 `result.w4_model_stages.w4.binding`，与已核验输出、operation_id 一起保存，不改数据库版本。记录、关系、coverage 或提供原文/历史变化时，同操作缓存拒绝复用并要求 needs_rejudge。加载和保存均核对认领、租约、轮次及来源绑定。阶段内部 `output` 保存 `{raw_output, final_output, valid, validation_error}`：原始模型 JSON 不被覆盖，最终决定经过同一组装规则和严格校验；组装成功但校验失败仍保留组装后的 final_output；解码或组装失败时为 null，valid=false。失败检查点另绑定 invalid_operation_id：同操作复验失败不重发，显式重判的新操作可再次调用。缓存重开重新组装、校验并核对保存的最终输出，差异拒收、不发送。旧的裸 W4Output 检查点继续复验，但不能冒充已保存过原始模型输出。此内部 JSON 扩展不升数据库版本，不改变外部回执或冻结 schema。
- **校验与提交**：W4 提供者、注入入口和离线回放共用 `w4::assemble_new_basis`（运行时包装入口为 prepare_output），组装逻辑只有这一处，重复调用幂等。为每条 decided/pending 复制当前输入候选的 source 与 sentence；来源必须仍存在、属于本次提供的输入、原文未变化且候选原句逐字存在。已有等价原句引用不重复；其他原始引用不删改，旧方依据、动作、目标和关系解决均不补。组装后调用原有严格 validate，再交现有 `plan`（再次 validate）、保存计划与 W5 提交。原始输出及最终输出保存到阶段检查点；无效决定仍整份失败、零业务写入，T1 与预算记录保留，错误为 w4_invalid_output 或已有模型错误码，不自动重试。有调用记录无阶段结果的崩溃窗口仍为 failed/model_invalid_output，内部 detail_code=model_result_missing。
- **预算与恢复**：w4 与 w1/w3 共用后台每消息额度（最多 3），与旧前台计数分开，共享授权 group 总额；同 owner/operation 的后台次数跨 group 统计。W1 空发送一次，无需 W4 发送两次，需要 W4 最多三次。W4 阶段已保存但计划尚未保存时，同输入重开可零发送恢复；计划已经保存时直接续交余组或补来源汇总，不重新 assemble 或调用 W4，目标变化由 W5 快照检查要求重判。
- **清洗与离线验证**：W4 的输入记录、关系、提供原文/历史和输出依据都是阶段检查点里的原文副本，沿用删除清洗及整份计划失效；固定 action/pending_reason/of 枚举保留。涉及删除的 W4 阶段整体清空 raw_output 与 validation_error，避免空白变体或无效 JSON 残留；valid=false 时 final_output 也整体清空，已校验的 final_output 沿用逐片段墓碑清洗。受影响的缓存与计划不可续跑，未涉及的来源与审计副本保持不变。真实 SQLite 与假 HTTP 回归使用 W4 强化夹具作为响应（只重绑定数据库生成的来源和目标 ID），覆盖完整闭环、1/2/3 次发送、整份无效零写入、缓存输入变化、缺结果、计划/部分提交/未写总状态重开、晚到结果和删除；删除回归另覆盖空白变体、无效/不可解码响应、检查点失效及无关审计副本保留。w4_save_model_stage 在保存的同一事务内复用 SourceInput::of 核对全部提供来源的正文与历史，不使用全局 revision 等值检查；来源已删除或变化时整次保存回滚，不保留原始输出或输入绑定，运行时仅记录 needs_rejudge 失败码。旧方删除先于保存时拒绝晚到响应，保存先于删除时沿用上述清洗。回归同时覆盖有效/无效响应、正文/历史变化及终态不自动续跑。测试不评价真实模型语义或改变样本标签。

## 8. 配置与 CLI 分发（④-5）

配置文件示例（`model_budget` 为批处理必须的显式有限授权）：

```json
{
  "write_pipeline": "w4_runtime_v1",
  "enrichment_mode": "deferred",
  "model_budget": { "group_id": "my-authorized-batch", "max_calls": 30 }
}
```

- `write_pipeline` 缺省为 `legacy`，只接受 `legacy` / `w4_runtime_v1`，未知值报错。新路径只允许 `deferred` / `off`；与 `inline` 同时配置时启动报错（J6：现有 MCP 全局锁仍覆盖整次调用），不静默改模式。改回 legacy 开关后，inline 配置仍不能整理已绑定的新来源，分发入口返回 unsupported_processing_version；旧版本来源不受影响。原文接收必须 `capture_source=true`；无独立原文的请求明确拒绝。此开关不选择或改写召回路径。
- T1 在 `SourceRecord.task.processing_version` 绑定版本，包括新路径 off 来源；此时 round 仍为空，不调用模型。修改配置不会迁移既有来源或重排队列。同消息键同参数重放只读实际回执，不增加任务、轮次或模型调用；不同参数仍要求新 turn_id。
- `pmr-mcp --db PATH --owner ID --config CONFIG --drain-enrichment --max-jobs N` 在 deferred 模式整理 queued 来源，按任务排序序号选取新旧版本并分别执行；旧来源继续 `commit_candidates`。旧顺序屏障仍生效，新路径复用 W4/W5 屏障、租约和快照检查。
- `pmr-mcp --db PATH --owner ID --config CONFIG --enrich-source SOURCE_ID` 按来源绑定版本处理，当前开关即使为 legacy 也能整理已绑定的新来源；off 来源只能由显式入口启动。新来源首次整理用稳定 `w4:<source_id>:0` 登记第 0 轮，随后 `run_source_with_all_models` 串联 W1/W2/W3/W4/W5。若首轮登记前已被顺序屏障标 stale_requires_review，只有已在 T1 绑定新版本的来源可登记第 0 轮，沿用原序号，仍由原屏障阻止模型调用与提交并返回 needs_rejudge；不放宽旧来源的登记规则。续跑沿用 operation_id 与轮次；needs_rejudge、已提交等终态只返回回执，活跃租约只返回 running；pending/coverage_gaps 仅在无计划时续跑。drain 不自动重试 failed/budget_exhausted 或显式重判；加轮次仍须库级 `w4_start_round(..., rejudge=true)`，本段不新增重判 CLI。
- 新路径返回冻结 3.5 的 source/enrichment 片段，不公开检查点、缓存、内部错误细节或旧 outcomes；尚未启动轮次时候选/请求为空。内部 stale_requires_review 映射为 needs_rejudge。drain 外层 `{max_jobs, attempted, results}` 与每项 `{source_id,status,result}` 不变；外层 completed 表示调用返回，事实结果看 enrichment.status。旧来源输出原样保留。缺少模型客户端配置时来源落 failed、释放认领、零发送，批处理可继续下一条；遇 budget_exhausted 停止本批。
- 批处理在 `pmr-mcp` 创建 `Arc<Mutex<ChatMemoryService>>` 之前执行并返回，模型调用不持有服务全局锁；提供者调用期间也无 SQLite 写事务。本段不改全局锁，不启用新路径 MCP 后台或 inline，不调用真实模型。
- `tests/write_pipeline.rs` 与 `tests/write_pipeline_process.py` 覆盖严格配置、原文授权、T1 版本绑定、关闭重开后配置切换/同参重放、终态与租约、缺凭证、两种 CLI 入口和混合队列；HTTP 仅使用本机合成 Responses。

## 9. 提示词第 2 版、输出上限与分阶段评测

- **W1**：仅 `relation=fact` 要求 value 与完整原句相同，专用关系保存宾语或属性值；`memory_type=fact` 不触发整句规则。polarity 表示命题方向，与不确定性及 speech 分开。活动适用条件放 conditions，时间按各 kind 的字段及原始粒度表达；没有时间时用空数组，禁止空 Quote。请求原话含控制动作，目标 Quote 指对象，unknown 对象不带候选索引。
- **W3**：明确冲突或删改明确限定语判 contradicted，无法确认或排除命题判 insufficient。覆盖按 relation/value/限定语判断，不因 sentence 包含整条消息就判完整；gaps 必须附遗漏断言的非空连续原文。问题 Quote 不省略中间文字。
- **W4**：提示词要求每条决定（含 pending）附新候选 `source.source_id` 的依据；decided 的每个目标还要分别引用旧依据。pending 要求仅为提示词约束，不收紧 validate。legacy 信息缺失用 unclear_relation，不伪造旧依据；依赖排除未检索记录的结论用 scope_incomplete，双方完整的已定位目标仍可更新。整个旧断言的修订用 refine，现实及偏好强度变化用 state_change，独立范围或局部补充才 coexist。整组协调目标与动作，每候选一条决定，每关系最多解决一次。
- **生产预算**：新写入阶段固定 W1=32768、W3/W4=16384；旧阶段沿用原配置及默认 4096。W1 按既有超时规则使用 360 秒，W3/W4 使用 180 秒。新授权的内部 policy_json 增加 `write_output_tokens` 绑定；已有无此字段的授权仍可用于旧阶段且不重写，不能静默用于提高额度后的新写入阶段，须提供新 group_id。公开配置和数据库版本不变，调用次数限制不变。
- **评分与门禁**：`tests/write_stage_eval.rs` 默认仅报告模型评分或调用失败；显式 `EVAL_GATE=1` 才因最终评分或调用失败而失败（原始 W4 评分另行报告），`EVAL_GATE=0` 关闭门禁。无效样本、损坏录制、非法配置及上限预检仍失败。W1 按 W2 最终候选与还原后的请求评分，保留 raw 和含拒收、原因、索引映射的 w2_final。候选完整匹配仍决定整项是否通过；字段独立诊断，不把一个错误扩散到正确字段。W4 resolves_relations 按整组比对，合法的归属变化不扣分，遗漏或重复仍失败。
- **U4 评分校准**：逐字段及逐样本分别报告 contract（解码、机械校验、编号一致性）、semantic（标签语义）和 reference（引用选择）；机械校验通过不证明完整语义正确。允许的引用等价仅为：两边均是当前原文连续片段，末尾单个句号（中文或英文）或分句逗号可有可无；conditions 可多/少一个本候选完整原句中明确存在的开头主语“我”（不从其他断言借主语）。不忽略内部标点，不缩短目标指代，不改变限定语或时间，不放宽专用关系的值；fact 的原句值沿用原句引用等价。coexist 且 targets 为空时不要求旧方依据。W4 报告 raw_result/raw_output 与 final_result/final_output，最终评分采用和运行时相同的组装规则；不能把组装后的通过当作模型原始引用能力改善。
- **多模型与重复录制**：`EVAL_MODEL_LABEL` 为安全目录标签；`EVAL_BASE_URL`、`EVAL_MODEL`、`EVAL_API_KEY` 覆盖 Responses 地址、模型 ID 和密钥，缺省沿用现有 DeepSeek 配置。密钥不进入请求、录制或报告。回放须设置与录制相同的标签、模型 ID 和 base URL，不需要 API key。需兼容现有 Responses/严格 JSON Schema 请求形状，本段不做其他供应商协议适配。`EVAL_REPEAT=N` 为正整数，缺省 1；录制放在 `tests/eval/write_stage/recordings/<label>/repeat-<k>/<sample>.json`。每次尝试单独计分并保留，报告每样本通过次数、错误类别及每次结果，不挑最好。HTTP/未完成/拒绝也保存诊断，续跑回放失败尝试而不自动重试；非法 JSON 原样保留并计失败。写文件或录制格式错误始终失败。
- **录制绑定与历史重算**：提示词/schema/模型/输入仍进入请求指纹，上限另由录制元数据严格绑定；同时核对模型标签、实际模型 ID、base URL、repeat 和录制版本。缺省上限与生产一致：W1=32768、W3/W4=16384，可显式 `EVAL_MAX_OUTPUT_TOKENS` 覆盖，但不同上限不得复用。旧平铺录制不自动进入新的重复比较；第 3 轮没有重复序号及上限元数据，且 W4 用了 32768，不能作为本轮受控比较的第 1 次重复。`EVAL_HISTORY=1` 仅离线重算：第 1 版取 54ef102 中的历史录制，第 2 版取现有平铺录制，按当前裁定标签与评分规则报告；不检查当前提示词指纹、不发请求、不改录制，标记 comparison_eligible=false。历史预算及 w3_16 输入变更使其不能作为受控 A/B 结论。

干跑或回放（不设置 EVAL_RECORD，不创建客户端）：

```sh
EVAL_STAGES=w1,w4 EVAL_REPEAT=2 cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
EVAL_MODEL_LABEL=stronger EVAL_MODEL=MODEL_ID EVAL_BASE_URL=https://example.invalid EVAL_STAGES=w1,w4 EVAL_REPEAT=2 cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
EVAL_HISTORY=1 cargo test --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
```

每个模型为 41×2=82 项，两个模型共 164 项。预检覆盖全部所需调用后才读取录制凭证和建立客户端；仅显式 `EVAL_RECORD=上限` 启用真实发送，真实评测须另行授权。新比较目录为空时上限低于 82 会在发送前拒绝。完整三阶段为 61×N 项，W4 标签仍为 draft。
