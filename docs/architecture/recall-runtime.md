# 召回运行时：证据闭环、R4 核验与 R5

本模块实现 contracts 3.1–3.4 的问题理解、证据组装、R4 核验与 R5，复用 SQLite、原文词法检索、W4 记录/关系投影及整理控制回执。召回④新增可选模型提供者与请求重放；不新增来源、不更新读取计数、不迁移数据库，也不改变旧召回入口。

## 入口与快照

`recall::run_offline(store, request, plan, budget)` 使用同一个 `assemble` 入口，随后消费 `Assembly::finish_skipped`。需要判断时，调用方取得同一个 Assembly 的证据包，把 r4.v3 JSON 或 JudgmentFailure 传给 Assembly::finish_r4；该入口先严格解码、完整核验再计算、组装 R5。context_only 继续走跳过分支。生产不读取评测夹具；④的 RecallRuntime 沿用相同 assemble/finish 入口，提供者只增加模型调用与门控。

`Request` 的归属、任务、主体范围、said_on 和时区均由运行时提供。输出的 request 复制稳定 request_id、原始 query/context/task_id/at；R1 Plan 的原问题、上下文及可信范围必须与请求相同，否则拒绝组装。跨调用同 ID 的参数一致性与首次 at 由④的摘要回执绑定，正文与完整结果只留内存，见下文。

records、raw、relations、completeness 和新 Prepare 的顶层 source/enrichment 回执在同一个 R3 SQLite 读事务内产生，FTS 也使用该事务。snapshot 保存该次 read_revision/source_revision；R1 前的诊断 stamp 不是证据版本。R1 等待期间检查既有输入/目录的完整性，随后只装配一次 R3，不重复模型调用。

AB2：**有效快照 = 绑定完整且内容仍获准可见，不等于最新**。整理状态推进、普通事实提交、关系/update 推进及读快照后新增来源不使结果失效；事实、等级、状态和完整性保持原读快照，不拼入最新进度。新 request_id 重新读取新版本；同参重放与 Validate 使用仍有效的原结果，不隐式重试。queued 表示原文已收到、整理待执行；enrichment 为 R3 读取时观察到的进度，不保证交付时仍处于该状态。request.at 是提问时间，不是数据库读取时刻。

返回前仍检查来源或记忆删除（含无来源 legacy）、正文/历史/时间锚点、归属/主体/任务范围、目录绑定、本轮或证据范围内的控制限制，以及回执执行身份。任一不符拒绝或丢弃整个结果，沿用 recall_snapshot_changed / recall_cached_snapshot_stale 等已有错误，不新增冻结失败码。当前保守绑定可信任务内的既有来源和可见记录；普通追加证据允许，但原有依据被删除或改写不允许。来源请求和控制绑定仅以内存指纹保存，不持久保存只读 query/context 或模型输出。

最后一次安全核验、完成回执与完整结果入缓存共用短 SQLite 写事务；重放/Validate 和 MCP prepare 发布/交付也共用安全检查。SQLite 发布事务阻止其他连接在检查与缓存/续等发布之间提交删除，无模型调用或等待后台。已交付内容无法撤回；删除后新的交付、重放和晚到结果不得再次输出已删内容。Prepare 失效时移除 recall，当前来源已删则返回 deleted 并清空整理片段；辅助 global_context 重新取当前获准视图，避免旧副本残留。W5 的快照、租约和顺序屏障不变。

## 可信范围与检索

- 仅读取 store.owner 的资料；显式主体范围 `Some([])` 表示不允许任何存储主体。记录必须通过主体、Task 条件和来源任务检查。Task 范围内可以读取无 Task 条件的全局记录；未提供 task_id 时仅读取全局内容。
- 有 R1 理解时，目录内主体和 filter.relation 在可信范围内进一步收窄种子；自然语言 relation、conditions、原问题与 USER context 只作为词法提示。filter.time 不用于猜测成立区间：枚举其范围的记录超集，时间适用性交给 R4；未知过滤时间使 plan_complete=false。已结束的未知终止时间不会被当作无限持续。
- aggregate、count/sum/list 枚举范围内的全部记录，不依赖词面命中。R1 未理解也宽泛枚举可信范围，保留 current_message，附 r1_not_understood；没有获准计划，plan_complete=false。
- 其他开放词法检索复用 FTS 候选提示，索引不可用时扫描同一快照的值与原有片段。排序依次为实体命中数、共有短语数、来源接收顺序降序、记录 ID。重复证据不会重复增加同一短语的命中数。开放词法命中本身不证明计划范围完整。
- 从种子展开完整记录的 history、旧记录的 supersedes，以及所有状态的相关关系成员，直到不再增加。任何可见成员越出可信范围时，整条关系不进入包，也不借关系带入范围外记录，plan_complete=false。Deleted 记录不进入 records；失效关系可以保留墓碑成员 ID 的审计定位信息，不返回其内容。

## 证据投影与来源

CurrentRecordView 复制 W4 完整候选、限定语、换算结果、更新状态和版本定位信息，去掉仅属于 RecordView2 的 end。未知结束时间保留 ended=end_unknown；不推导当前资格。没有更新状态条目的记录按既有投影回落 legacy，update=null；不补造 judged 或 Candidate。LegacyMemory 保留旧值、结构化条件和时间，仅执行来源/任务可见性过滤，gaps 显式列出。

逐字依据只链接仍存在且获准保存的来源。legacy 有可用原文时，即使来源已 committed，也通过 raw 交出正文供后续核对；其 grade 为 uncommitted，不表示后台重新排队。这些完成来源不计 pending_related_sources。

可信范围内所有未整理、失败、待重判、待确认等未完成来源进入 raw 候选，不依赖词法命中、R1 主体提示或记录种子命中；owner/task 和可确定的可信主体范围检查保持不变。已完成来源继续按原有相关性规则进入（如已选中的 legacy 来源），不自动变为待整理原文。按 received_sequence 最新优先进入数量/字节预算，选中的条目最终按 received_sequence 升序、source_id 排序输出；保留接收日、整理状态及 MemoryRequest 的冻结字段。接收日由来源 observed_at 与可信时区换算。MemoryRequest 从现有合并回执读取，控制记录优先，不自行重新理解请求或执行删除/期限。删除或未获准保存的来源不进入 raw。

只读 current_message 固定保留完整 query，与检索和存储证据预算无关；source_id/sequence=null，enrichment=not_stored，memory_requests=[]。bootstrap 无本轮消息时为 null。R1 current_evidence=query_only 的送审资格通过 Assembly.review_current_message() 保持为 false；标记不扩展冻结 JSON。仅有 query_only 的问题文本不算判断证据。

相关关系在 R3.relations 和跳过输出的 conflicts 中保留 open/resolved/invalidated 及可见依据。这是已有关系的审计呈现，不生成本轮冲突判断。跳过时 facts=[]、computed=null、history=[]；need=context_only 时 status=context_only。no_evidence 下 R1 已识别的未解析/歧义主体保留 needs_clarification 与代码澄清模板，主体无歧义、完整检索且无证据时 status=insufficient、clarification=null；检索不完整或其他跳过原因仍为 degraded。跳过均 semantic_judgment=skipped，并写入 semantic_judgment_skipped:context_only/no_evidence/offline/budget。

## 预算与完整性

默认上限：64 records、32 raw、64 relations，三类存储证据条目序列化 JSON 的合计字节数 262144。current_message、request 和输出包固定元数据不计入此证据预算；模型输入/输出总预算在召回④实施。

先获取范围内候选和展开关系，统计缺口，然后裁剪。记录按检索顺序整项保留，raw 优先保留接收顺序最新的条目（同序号按 source_id），裁剪后按接收顺序升序输出，关系按 ID 保留；字节额度依次分给 records、raw、relations。记录/关系不破坏其内部字段；raw 正文可以在 UTF-8 边界取前缀，按 JSON 实际转义后的长度核算，放不下元数据则整项省略。所有数量/字节裁剪均令 retrieval_truncated=true、plan_complete=false。预算裁掉的记录/来源仍进入已知缺口计数；关系中的 ID 可以指向已裁掉但未删除的记录，不绕过预算补回正文。

- pending_updates：裁剪前唯一相关记录中 update.pending 的记录数，包含已提交记录。
- pending_related_sources：裁剪前可信范围内已获准进入 raw 的未完成来源数，包含最终被数量/字节预算裁掉的条目；归属不明或状态无法映射而扣留的来源仍按已有规则标不完整/整理缺口。
- enrichment_gaps：裁剪前 legacy.gaps 的条目总数，加无法映射的来源状态的缺口数；不是 legacy 记录数。
- plan_complete：获准的计划范围已枚举、关系未因可信范围被扣留，且没有预算截断、未知过滤时间或来源归属/状态缺口。自然语言条件是否匹配仍由 R4 判断；本字段不认证语义正确或精确计算。

## 保守边界与待裁定项

来源只存 owner/task，没有可靠的独立主体字段。显式限制主体时，没有已提交记录可链接的未整理原文不按词面猜主体；整条原文同时涉及范围内外主体/任务时也不借入。两种情形都令 plan_complete=false。可见 pending_related_sources 仅计已确定可进入包的来源，不拿未知归属的计数代替原文。是否引入可核准的原文主体定位，留给后续设计，不能靠 R1 给原文扩大授权。

旧 completed_pending 映射 pending，stale_requires_review 映射 needs_rejudge；旧 completed_rejected 以及未知状态没有冻结枚举的唯一对应项，暂扣留该 raw 并增加 enrichment_gaps，plan_complete=false，不自行解释成 failed 或成功。已有 records 继续返回，旧路径字符串回执保持原状。未知状态的正式映射与 enrichment_gaps 按缺口条目计数属于需产品复核的口径，本段未改冻结契约。

④先提供库级模型入口；⑤已接通配置、CLI 与 MCP 离线分发，MCP 在线锁改造仍另项处理。生产不读取样本；现有离线证据包入口保持可独立测试。


## R4 核验与计算

`r4::decode/evaluate` 检查冻结字段、枚举、必填可空项和非空文本。FactView/ComputeItem 的每条依据必须落在本包的来源与连续片段中，复用 locate_quote 的空白归一并恢复原文，不拼接不同片段。query_only 的本轮文本不能成为事实或计算依据。record_ids/supersedes_record_ids 除在输入内，还必须有该条记录的依据；supersedes 必须同时有旧条目与新方依据。legacy 没有可核准来源时不能形成事实；有获准原文时可重新判断原文，不补造旧限定语。

Z18 在解码与完整核验之间共用 `r4::assemble_old_basis`：仅对模型明确列出的包内 supersedes 目标，按证据包记录顺序补入缺失的已核准旧引用。引用须为 stored 来源，来源未删除且 Quote 逐字存在于可信来源正文；不以记录自带 Quote 自证。生产从 SQLite 取得正文并沿用返回前的快照屏障，离线适配器仅从样本 input 展开正文。已有可核准的短片段或空白等价依据不重复；原引用或 record_ids 依据无效、缺新方依据时不补。不推断替代目标、不补新方依据或计算操作数，补入后照常执行全部核验和 R5 派生。补引用只证明旧片段真实存在，不证明替代语义、条件归属或唯一当前值正确。

来源正文回查及记录原来源/依据授权检查按 SourceRecord.id 查找；sources 的 map key 是会话/轮次键，不等于引用里的 source_id。沿用原有任务/主体/所有者边界，不因标识修正放宽权限。

`finish_r4_with_audit` 保留原始文本 raw 与组装结果 final；核验成功时 final 为恢复原文后的规范结果，核验失败时保留组装结果，解码失败时 final=null。冻结 RecallOutput 的失败行为不变，不交出部分有效事实。原 `finish_r4` 共用此边界并只返回冻结结果。

不确定性、条件、TimeInterp 及 anchor_quote 都检查对应依据来源；输出完整保留 polarity、strength、uncertainty、conditions、times、current、applies。来源接收日与时区在同一读快照中取得，不接受模型自报时钟；来源时刻改变会推进 source_revision，已有快照屏障拒绝旧日期锚点。时间换算与日历字面核验统一复用 W2：整句 Quote 中的明确日期、月份、年份也检查区间与精度，不把月/年补成具体日，完整日期区间不降为单个端点。支持数字日期、ISO 日期/月份、中文月名与既有日期区间；多个日历片段至少须有一个完整片段吻合，不机械判定它与断言的语义归属。Anchored/Recurring/Unknown 沿用 W2 的 unknown，所需端点无法确定时计算不足。代码不证明概括、主体、条件适用性、事件同一性及时间自然语言解释的语义正确性，这些由 R4 样本评测验证。

任一结构/引用失效整份输出变为 degraded、semantic_judgment=failed、facts=[]、computed=null；既有记录与原文继续交出，不保留部分有效模型事实。使用冻结 model_invalid_output/model_timeout 原因。合法输出因身份、覆盖、单位、精度或算术容量不足而不能计算时，保留事实，computed=null，附 compute_insufficient，并按状态优先级得到 insufficient 等状态。

离线评测使用 `r4::decode_detailed/evaluate_detailed` 定位失败规则与字段，生产 `decode/evaluate` 复用同一核验并继续返回冻结总码。诊断只包含规则码、字段路径和可选内层原因，不携带原文；一次报告首个核验错误，不代表其余字段均有效。结构解码错误定位为根 `$`。

- count：按非空 event_group 去重，null 不视作独立事件；只计已确认组，存在未知身份或范围缺口时只能给 at_least；没有已确认组时不足。完整且明确的空集可精确计零，同一来源允许多个条目。
- sum：同组重复数量归一后相等只计一次，矛盾、身份未知或数量缺失时不足。coverage=complete 还须满足代码范围完整、无裁剪/待判记录/整理缺口，相关 raw 已由条目覆盖；模型自报不能覆盖这些已知缺口。冻结形状没有证明遗漏部分非负的字段，因此不完整 sum 保守返回不足，不把已见正数的部分和当下限。
- quantity 使用有符号十进制文本及 i128 定点数，逐步 checked 运算，最多38位小数；不使用浮点数。机械依据当前只核准字面数字与相邻单位；不解释中文数量词。相同字面单位可直接运算；固定 SI 别名与质量/长度的十进制倍率可归一（kg/g/mg、km/m/cm/mm 及中文别名），其他换算、汇率和隐含单位不推断。不能运算或溢出则不足，伪造数字/单位依据则整份无效。
- elapsed：两个自然日端点用现有 openai::elapsed_count 算法与冻结四种取整规则。端点粗于日或多日区间、倒序、未知时不足。@request.at 只允许第一个操作数，由可信 at 和时区生成 request_time；不使用 current said_on 或执行日期代替。
- compare：数量同维度归一后比较；日期用半开区间，分离即确定先后（X3），重叠不足；同一个精确日可判 equal。不得以粗精度区间相同判日期相等。

## R5 派生与边界

成功判断按 needs_clarification > conflict > insufficient > answered 排序；R1 未解析/歧义主体也触发澄清。已有相关 open 关系仍阻止确定答案，除非同一个 current=yes 事实以新旧依据明确 supersedes 全部冲突成员，或只保留唯一所引用成员；此消解只用于本轮，不改变存储或关系状态。R4 自报 conflict 同样得到 conflict。没有事实或计算的 supported 不包装成 answered。

grade 仅为标注：所有依据来自已提交记录且没有 raw/current 来源时为 committed，否则 uncommitted；同一来源同时出现在 raw/records 时保守取 uncommitted。update 合并显式 ID 和依据实际涉及记录的状态，不能省略 record_ids 绕过 pending；pending 优先、原因去重；其他未知或只有原文依据则 null，全部已明确判定才 judged。grade 不改变 status。

**当前结论的可见性门控。** 组装在同一快照内记录获准进入 raw 的未完成来源，逐个核对预算裁剪后的覆盖：完整正文仍在 raw 或已返回记录的依据中，则已覆盖；整项省略或只保留前缀且记录依据未覆盖全文，则未覆盖。不能仅比较 pending_related_sources 与 raw 条数，字节截断可能保留相同条数。该内部定位不新增冻结字段，也不补回超预算正文。

R5 只对与未覆盖来源可能相关的 current=yes 事实降为 unknown。无可靠定位的来源保守视为可能相关，不以词法不命中或 received_sequence 先后证明无关；只有来源唯一未删除的已提交 Current 候选，其 sentence 逐字覆盖整条现存正文且依据仍能在该来源核准时，才可按其主体/关系槽位排除不同槽位的事实。多个记录、仅片段链接或 legacy 链接均不能据此证明整条来源无关。事实槽位取其实际引用及 supersedes 涉及的记录；仅 raw 依据或无法定位的事实也保守处理。

门控触发时保留事实、限定语与依据，semantic_judgment=model；按既有状态优先级至少进入 insufficient（澄清/冲突仍优先），completeness 保持 retrieval_truncated=true、plan_complete=false 和裁剪前计数。检索预算截断不附加 degraded_reasons=budget_exhausted；模型调用预算确实耗尽时仍由既有路径附该码。current=no 不被改成当前事实；门控后的 unknown 不能消解 open 关系。完整的新原文仍可支持新事实；仅 records/relations 被裁剪、确知不同槽位的 raw 被遗漏、正文已完整出现在记录依据中，或 queued/failed 等标签本身，均不触发这个门控。模型对完整证据的语义误判仍由评测检验。

新 validate_answer 使用已保留的 R5 结果，门控后的 current=unknown 或非 answered 状态均拒绝机械批准；不另作付费判断，也不因包级截断标记一律拒绝其他未受影响的事实。旧回执适配继续沿用已有更保守的整体完整性要求。

history 仅包含模型 current=no 且由 StateChanged/PreferenceChanged/ConstraintChanged 结束的记录，Correction/UserRetracted 等只保留审计。until 从同一快照的已知结束日取得（内部读取 RecordView2.end 或已有 Explicit valid_to），不在冻结 RecordView 添加 end；未知/粗精度或多条记录结束日不一致为 null，不回填接收日。旧 Supersession/Inferred 等边界不能证明真实结束日，until 保守为空。

clarification 由代码模板列出歧义实体名称或未解析指代，引用当前触发原话。若未解析指代仅来自 USER context，冻结 SourceRef 没有 context 定位项：保留问题中的真实指代，evidence=[]，不把历史文本假装成 current。R4 单独要求澄清而未给触发事实时使用通用模板，不补猜指代或条件。

conflicts 的冻结类型是 RelationLink[]，只放输入中的未在本轮消解的 open 关系；R4 conflict 涉及的记录保留在 records/facts 与 status 中，不把记录伪装成关系或由代码新建关系。失败/跳过时沿用原有关系审计输出。

`tests/recall_r4.rs` 从已复核 R4 样本的 input 展开真实 SQLite 来源与 W5 提交、关系、结束信息，通过同一 assemble/finish_r4 注入人工输出，并重开验证无写入。样本的 expect/must_not 不进入运行时；r4_04 追加以原始 single/lookup 计划验证：问题与新原文没有词法命中时仍取得 s-40，R4 注入后南京 current=yes、grade=uncommitted，上海 current=no，不判冲突。`tests/recall_offline.rs` 同时验证最新优先的数量/字节裁剪、裁剪前计数、任务/归属/主体隔离和已完成来源相关性规则。模型语义质量另由后续真实评测衡量。


## 召回④：提供者、预算与重放

`recall_models::RecallRuntime::run(store, request, evidence_budget, provider)` 串联 R1 → assemble → R4 → finish。provider=None、客户端网络关闭或 bootstrap 跳过两次调用；context_only 或 query_only 且无 records/raw 时只跳过 R4。R1 无效或失败按 judge/unknown 宽泛检索，保留 r1_not_understood 和具体失败原因；R4 超时/HTTP失败/无效输出按③的整份降级规则处理。预算不足标 semantic_judgment_skipped:budget 与 budget_exhausted，没有隐式重试。

角色配置为 `recall_models: { r1, r4 }`，各角色缺省 DEEPSEEK_MODEL；另设 `reasoning_effort: { r1, r4 }`，常规角色缺省仍发送 medium；`r4_compute_reasoning_effort` 缺省 low。R4 只按已核验 R1 的 plan.op 分流：count/sum/elapsed/compare 用计算强度，lookup/list 或 R1 未理解用常规 r4 强度。选择复用同一纯函数，实际强度进入阶段绑定与完整请求指纹，两种配置强度都进入请求/缓存绑定；更换强度不能复用旧请求结果。不增加调用或重试。配置由 with_models 与 ChatConfig/CLI/MCP 分发。支持 low/medium/high/minimal/none/xhigh/max，以及显式 omit（删除 API 请求的 reasoning 字段，使用远端默认）；除下述已测配置外，端点/模型支持范围待真实验证，见 w4-code-side 第8节。两份提示词全文在 `src/recall_models.rs` 的 R1_PROMPT、R4_PROMPT，strict schema 复用写入侧 Quote/限定语/时间结构构造，与冻结 r1.v1/r4.v3 对应。模型 ID 只用于 API 顶层路由、内部绑定，不进入 input 或 instructions；强度用于 API reasoning 及内部绑定，不进入 USER payload。生产与评测共用 r1_input/r4_payload。全量验收 B 定向修正 R1/R4 提示词，分别明确数量读取与枚举、关系过滤，以及完整计算条目/操作数、来源限定语和事实依据；两阶段的旧录制均须按完整指纹过期，不借用旧输出评分。固定输入小集与 AC2 配置见 tests/eval/README-v3.md。

新账本阶段 r1/r4 复用 model_calls：每请求每阶段一次、合计最多2次，跨 group_id 核对尝试数；总授权仍与后台共享。请求按 owner 与稳定 request_id 关联独立 operation_id。物理发送前持久预留，超时、HTTP失败、无效输出不退次数；输出为空或崩溃都不重发。R1 输出上限沿用 ModelBudgetConfig.query_output_tokens（缺省4096）。新路径 R4 的已核验计算类且实际强度为 low 时，使用 ModelBudgetConfig.r4_compute_output_tokens（缺省16384，可配置为1–65536）；其他 R4 调用（含 minimal、非计算类 low 和 R1 未理解）沿用 ModelBudgetConfig.review_output_tokens（缺省4096）。只在本次 R4 客户端副本中选择计算类额度，持久授权配置不变；账本记录实际选用的上限，旧路径默认值不变。上限进入 API max_output_tokens 及完整请求指纹，专用上限也进入参数/缓存绑定。输入总上限沿用 max_input_bytes；实际时限沿用账本阶段策略。这些限额可显式配置；单阶段评测不能代替生产预算下的整链路验收。

### 推荐配置与实测边界

用户已选 R1 使用 DeepSeek-V4-Pro（`ep-20261003130756-trhj6`）+ minimal；R4 同模型，计算类 low，其余 minimal（AE10）。常规角色缺省值和新路径启用状态不变。[示例配置](../../examples/recall-models.json) 是可反序列化为 `RecallModels` 并传给 `with_models` 的库级配置，可作为完整 ChatConfig 的 `recall_models` 字段；⑤已接通配置分发，完整示例见下文。

2026-10-04 三档结果，每阶段20个已复核样本、各重复2次；使用相同 R1/R4 第1版提示词及 Z8 评分规则：

| Pro 推理强度 | R1 通过 | R4 通过 | R1 p50 / p90 | R4 p50 / p90 |
| --- | --- | --- | --- | --- |
| medium | 24/40 | 19/40 | 约68s / 133s | 约84s / 132s |
| low | 21/40 | 19/40 | 35s / 64s | 40s / 96s |
| minimal | 23/40（holdout 11/12） | 14/40 | 2.5s / 3.5s | 3.9s / 5.2s |

medium 耗时按录制写入间隔估算，含额外开销；low/minimal 为单次物理调用实测。minimal 未产生推理 token，R1 质量接近 medium。R4 第2版已重测40次：18/40、核验失败13，p50 4.5s / p90 6.3s；收尾日期核验修复后离线评分不变，13次分别为缺同条旧方依据8次、缺操作数5次，详见[收尾分析](../../tests/eval/recall_stage/r4-closeout-review.md)。首次 minimal 冷启动为16.6s；两阶段常见耗时支持该推荐，但单阶段分位数不能相加证明全流程 P95≤15s，宿主接线后的生产预算、检索和冷启动仍需验证。

AE10 的 low 参考来自已提交的首轮40次录制：平均输出2895.5 token，p50 41.8s（约40秒）；minimal 常见约4秒。计算题预期明显变慢，不能声称满足前台15秒目标；沿用 R1/R4 单阶段180秒时限及每请求最多两次调用，不扩大预算。固定输入集按冻结生产 payload 中已核验的 R1 计划分流；全量干跑按 query_meta.operation 标注估算，实际只取核验后的模型计划，R1 失败用常规 minimal。完整指纹过期的录制必须重录，不能靠更换标签复用。

该40次 low 测量的输出上限为16384，最大输出7420 token，8次超过4096；平均值不能证明4096足够。按 AE11，新路径计算类 low 默认上限改为16384，与 AC2 和固定集一致；minimal 及旧路径沿用现有上限。可通过 model_budget.r4_compute_output_tokens 调整专用上限；自定义额度进入持久授权策略，复用已有授权组时不能改变额度。完成率仍待整链路验收。

Z18 对同40项原录制零调用重算：raw 18/40（dev12/28、holdout6/12），final 26/40（dev20/28、holdout6/12）。补入8条旧引用，原8次 record_basis 失败全部通过；5次 operands 核验失败与9次字段评分失败仍保留。请求指纹全部有效，详见[补入后报告](../../tests/eval/recall_stage/r4-old-basis-review.md)。这是同一批响应的机械组装收益，没有新模型测量，也不代替语义人工复核。

回执复用 owner 快照已有 TurnReceipt 字典，用独立命名空间 `["recall.v3", SHA256(request_id)]`；不加表/字段、不升数据库版本。request 只存 form、参数 SHA-256 摘要、首次 at、角色模型与提示词/schema 摘要；result 只存 running/finished/failed/invalidated、执行身份、R3 快照版本及来源 ID。ring 已在锁文件中，通过直接依赖复用其 SHA-256，避免将诊断用 FNV 当作参数身份检查。原始 query/context、R1/R4 输出及完整 RecallOutput 不落入此回执，不借只读查询保存正文或新事实。

同 ID 的 query/context/task_id/at、可信主体/said_on/时区/bootstrap、证据预算、角色推理强度与 R4 计算类输出上限须相同；不同参数拒绝。宿主省略 at 时，运行时从进行中执行、缓存或持久回执复用首次时钟（连同接收日），并发跨秒/跨日也不构成异参；显式不同 at 仍拒绝。缺省常规 medium / 计算 low / 计算上限16384 沿用原参数摘要；任一配置非缺省时，将 R1/R4 常规、R4 计算强度与计算上限一同规范化加入摘要。缺省与显式相同值等效，不同值不能复用结果。调用方在续等/重放时复用首次 at，不能重新填执行时钟。完整结果最多64个，仅保存在同一个 RecallRuntime 的内存中；有效快照重放直接复用。重新打开/进程崩溃/内存裁剪后没有完整结果时，返回库级 `recall_result_not_retained:no_model_retry`，保持零发送；这只保证重开后的尝试状态，不承诺未获保存授权的只读正文跨进程完整重放。需要这种能力时须另行明确保存授权或客户端携带结果，不能偷偷持久化。

第4项②提供 `ChatMemoryService::operation()`：只复制数据库位置、owner、配置与 Arc 共享的 RecallRuntime；在锁外调用句柄的 `connect()` 打开独立操作连接。`SqliteMemoryStore::open_operation` 只接受已初始化的 v9/WAL 数据库，不创建文件、不迁移、不执行初始化写事务、不修复或重建索引；self 实体等初始化仍只由启动时的 `open/new` 执行。第4项③通过 `ChatMemoryService::call_shared` 在短锁内识别新召回工具并复制句柄，在锁外连接与执行；覆盖 Recall、Prepare 和 Validate 的新路径及其 v1 输出适配。legacy 召回与其他工具仍串行；新写入 inline 只保存 T1 并唤醒后台，legacy inline 仍同步。

共享缓存与进行中表均按数据库规范路径、owner、请求身份隔离。缓存短锁仅覆盖最终发布/重放所需的短事务，不覆盖模型等待；同 ID 同参的跨线程并发调用等待已有执行，完成后按原有回执/快照规则重放，R1/R4 各发送一次；异参立即拒绝。同线程递归调用保留原有明确拒绝，避免等待自己。等待者只保留完成状态，不另留正文副本。执行异常退出也会唤醒等待者，持久回执仍阻止未获授权的重试。`memory_forget` 沿用原删除事务的来源清洗，实际删除后清空该 owner 的完整结果（含 R4 原始审计），并标记进行中的结果不得重新入缓存；其他 owner/数据库不受影响。其他连接/进程的删除或输入/权限改变同样使结果失效；普通提交和进度推进允许按原快照返回，不隐式刷新或重新调用模型。

R4 raw/final 随完整结果共同保留在这64项内存缓存中，`run_with_audit` 经相同身份与快照检查返回同一对审计；原 `run` 的冻结输出接口不变。两份审计均不写数据库，不改变模型请求格式或指纹算法。来源失效时不能返回迟到审计，重开无结果时同样不重调。

重放及结束事务核对 R3 输入、范围/控制绑定和执行身份；原输出版本保持不变，不要求等于当前全局版本。安全失效返回库级 recall_cached_snapshot_stale/recall_snapshot_changed，不自动重调。同请求正在运行时也不重新认领；结束回执核对执行身份，删除中途到达则不能发布或缓存迟到结果。forget 在同一删除事务内整体清空涉及来源的召回元数据结果；原文及其空白变体不依赖文本替换清洗。运行时下一次校验时移除失效内存结果。模型请求不持有 SQLite 事务或内存缓存锁；第4项③/④已为新召回与新后台模型调用拆开服务锁；legacy 路径仍保持串行。

以上新失败说明仅是库级错误，不增加冻结状态枚举或新的 MCP 输出。⑤需要在授权范围内决定宿主如何呈现缓存不可用，不能将其当作自动重调许可。

## 单阶段评测

`tests/write_stage_eval.rs` 新增 EVAL_STAGES=r1,r4，共40项（各20；dev14/holdout6），仍复用预检、录制、请求指纹、重复与离线回放。R4 仅把固定样本 input 展开成完整 RecordView/RawEvidence/RelationLink 与获准计划后调用一个阶段，不运行模型 R1；expect、must_not、split、样本 ID 不进入请求。展开策略与评分边界见 `tests/eval/recall_stage/README.md`。

新召回请求指纹完整绑定请求时钟、said_on、时区、模型、提示词/schema及实际证据包；旧 W 录制指纹保持不变。默认只出报告；显式 EVAL_GATE=1 才因模型评分/调用失败报错。真实调用仍需另行批准，本段只做干跑及注入/离线回放。

## ⑤ 配置、CLI 与客户端迁移

`ChatConfig.recall_pipeline` 接受 `legacy`（默认）或 `recall_v3`，未知值报错。`recall_models` 接受上面的角色配置；可直接运行的完整示例为 [examples/recall-v3.json](../../examples/recall-v3.json)。`write_pipeline` 和 `recall_pipeline` 独立选择；新召回可以读取旧写入的记录和原文，旧召回也可继续读取新写入。缺省配置和未显式迁移的客户端仍取得旧输出。

三个入口 `memory_recall`、`memory_prepare`、`memory_validate_answer` 均接受 `recall_version`：`recall.v3` 或 `answer_contract.v1`。显式参数选择权威输出版本；legacy 流水线收到 recall.v3 请求时改用新运行时，recall_v3 流水线收到 answer_contract.v1 请求时从同一次新召回/快照机械适配（不重跑旧模型链路）。新流水线请求旧回执的 validate 同样须提交 request_id 并只读已有结果。未知版本返回 `contract_version_unsupported`，不换版本。新 Recall 只返回冻结的 recall.v3，不附旧 answer_contract。CLI/库调用直接返回该对象；MCP 在 `{recall: <recall.v3>, global_context: ...}` 包络内返回。新 Prepare 直接返回冻结3.5的 `source/enrichment/recall` 三段，MCP 另附 global_context；不再返回 remember 或 recall.result.answer_contract。源未获接收时 recall 为 not_requested；合法原文先接收，召回上下文超限或运行失败不会回滚保存，recall 返回 failed 错误片段，两种情况均无事实答复权限。

使用新流水线（包括请求旧回执）须提交稳定 `request_id`，保持 query/context/task_id/limit/离线选择不变。`at` 可省略：首次取服务器时钟，重放读取已绑定的首次 at（bootstrap 也共用绑定和缓存）；显式 at 须为过去时刻并与首次绑定一致。Prepare 请求旧回执时保留 remember/recall.result 包络，其中旧 recall.result 含机械生成的 answer_contract 和 request_id/快照元数据；未指定 request_id 时从 session_id/turn_id 摘要派生，不把用户原话用于身份。只读 query/context 不保存为原文；模型预算和只读回执仍保留不含正文的元数据。跨进程结果不保留，CLI 重跑已完成 request_id 或验证上一 CLI 进程的结果会明确失败，零重调；需要新尝试须显式新请求及预算，不能把重试改成隐式付费。

`offline=true` 强制跳过新召回模型；缺凭据或网络门关闭也走离线路径。新离线 Prepare 允许 `enrichment_mode=off/deferred` 或 `w4_runtime_v1 + inline`，后者只入队，不在前台执行写入模型。legacy inline 仍拒绝离线 Prepare，避免旧写入模型请求；已启动的新后台 worker 不受单次 offline=true 约束（该参数只禁用 R1/R4）。bootstrap 无论网络配置如何均零调用，仅读取全局规则，current_message=null、raw=[]、facts=[]，语义标 skipped。普通离线保留本轮消息、记录和未完成原文，facts=[]、computed=null，不能包装为确定当前结论。

CLI 在 MCP 服务锁建立前运行一次工具：

```sh
pmr-mcp --db /tmp/recall-demo.sqlite3 --owner demo \
  --config examples/recall-v3.json \
  --call memory_recall --arguments /tmp/recall-request.json
```

参数文件示例（在线需另行批准真实调用、提供凭据与有限 model_budget；此文件示例强制离线）：

```json
{"query":"请查阅已记录信息。","recall_version":"recall.v3","request_id":"demo-recall-1","offline":true}
```

同样支持 `--call memory_prepare` 和 `--call memory_validate_answer`，其参数文件按 tools/list schema 提交。CLI 工具调用要求显式 db/owner，不能与批量整理参数混用。模型 ID 只用于 API 路由及阶段绑定，不放进模型可见 payload；预算仍为每请求 R1/R4 各一次、共最多两次，无自动重试。

**MCP 在线新召回已接通。** R1/R4 在独立操作连接上执行，不持服务全局锁、SQLite 事务或共享缓存锁；等待模型时其他 MCP 操作可完成。同 ID 同参共用一次执行，各阶段最多一次发送；同 ID 异参拒绝，安全绑定变化明确失败，普通提交/进度变化返回原读快照（不隐式重试），删除清除该 owner 的完整缓存并阻止晚到结果入缓存。只读请求取消在发送前、阶段之间及保留结果前检查；已发出的阻塞 HTTP 等待既有超时或响应，取消后不继续 R4、不保留结果、不重试，取消等待者不取消同 ID 的执行者。模型超时沿用 degraded/failed 语义及已占用预算。

Prepare 仍先保存后召回；接受后的工作继续跨越客户端取消或45秒等待超时，通过完全相同参数续等，续等表的键只保留参数摘要，发布结果前与交付前都核对来源和召回快照；删除清除新版已完成的续等副本，发布与删除清理共用短锁，避免晚到副本重新入表。写入新路径 inline 在 T1 后立即唤醒后台，Prepare 不等待整理。`offline=true` 与 bootstrap 不调用召回模型；`PMR_MODEL_NETWORK=offline` 仍允许字面 loopback 测试端点，因此仅设该环境变量并不保证完全零调用。Validate 仅读共享缓存，不调用模型；跨操作连接可用，跨进程缺结果仍明确失败。

**新 Validate 的保守边界。** 提交同一 request_id、query/context/task_id，at 若提供也须一致；只核对同进程保留结果和仍获准可见的原读快照。当前只机械批准单个 answered/model 事实的逐字 statement、完全相同的存储来源集合，且 current/applies=yes、affirm、无强度/不确定/条件/时间、无 pending 更新、无计算/历史/冲突/澄清。任意改写、复杂限定和计算均不因此批准；来源 ID 有效不能证明草稿语义。返回宿主核验回执 `{version, request, snapshot, approved, approved_source_ids, reason}`（version 指所选契约，不是一个 RecallOutput）；缺结果/快照失效报错，不新增第三次模型调用，也不给旧 safe_answer 覆盖新版。宿主仍须按冻结3.4完整呈现事实、限定、grade/update、计算界限和原文依据。

迁移客户端须先适配单一 recall.v3、prepare 三段和上述校验边界；未迁移客户端显式选择 answer_contract.v1 或保留 legacy 默认。当前未开始默认切换或旧契约退出，也未证明生产整链路 P95≤15秒。

## 第4项④：与后台整理并发

MCP 新写入 inline/deferred 以单个独立连接 worker 整理已绑定 w4_runtime_v1 的来源，启动恢复与周期租约扫描见 [w4-code-side 第10节](w4-code-side.md#10-mcp-后台执行与恢复第4项④)。T1 保存即唤醒，不等待前台 R1/R4 返回；本轮和其他可信范围内的未整理原文沿用 R3 raw 可见性与截断门控。

[联合配置](../../examples/background-recall.json) 显式给前台2次、后台3次，使用同一总授权；没有新增额度。事件屏障测试要求前台在后台 W1 释放前完成并带新 raw，不能输出无条件旧当前值。AB2 前，后台进度/提交落在快照与核验之间会明确报 recall_snapshot_changed；此前受控交错3/3失败与⑤节奏86/120失败促成策略修正。现在普通推进保留原读快照，只有安全绑定变化失效，零自动重试；新的节奏与真实 Prepare 入口结果见 visibility-cadence.md。

第4项⑤的七类 MCP 可见性验证与无屏障快照变化实验见 [w4-code-side 第11节](w4-code-side.md#11-26-七类可见性验收第4项⑤) 及 [visibility-cadence.md](visibility-cadence.md)。安全绑定失效继续明确报 recall_snapshot_changed；普通推进不失效，同参重放不自动重试。
