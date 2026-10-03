# 召回运行时：证据闭环、R4 核验与 R5

本模块实现 contracts 3.2–3.4 的证据组装、R4 跳过/注入分支与 R5，复用 SQLite、原文词法检索、W4 记录/关系投影及整理控制回执。没有模型调用、来源写入、读取计数更新、数据库迁移或旧召回入口改动。

## 入口与快照

`recall::run_offline(store, request, plan, budget)` 使用同一个 `assemble` 入口，随后消费 `Assembly::finish_skipped`。需要判断时，调用方取得同一个 Assembly 的证据包，把 r4.v3 JSON 或 JudgmentFailure 传给 Assembly::finish_r4；该入口先严格解码、完整核验再计算、组装 R5。context_only 继续走跳过分支。生产不读取评测夹具；后续④只在此注入点前增加模型提供者与调用门控，不重建测试专用流水线。

`Request` 的归属、任务、主体范围、said_on 和时区均由运行时提供。输出的 request 复制稳定 request_id、原始 query/context/task_id/at；R1 Plan 的原问题、上下文及可信范围必须与请求相同，否则拒绝组装。本段不持久化请求或缓存；跨调用同 ID 的参数一致性及模型预算留给召回④。

records、raw、relations 和 completeness 在同一个 SQLite 读事务内产生，FTS 也使用该事务。snapshot 保存该次 read_revision/source_revision。返回前重读并核对这两个版本及来源整理状态、MemoryRequest 控制回执的内存绑定：状态/限制改变不一定增加正文索引的 source_revision。发现变化返回 `recall_snapshot_changed`，不返回旧包，不自动重新执行语义判断。此错误为库级错误，不新增 MCP 输出或冻结失败码。

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

相关关系在 R3.relations 和跳过输出的 conflicts 中保留 open/resolved/invalidated 及可见依据。这是已有关系的审计呈现，不生成本轮冲突判断。跳过时 facts=[]、computed=null、history=[]、clarification=null；need=context_only 时 status=context_only，其他原因均 degraded，semantic_judgment=skipped，并写入 semantic_judgment_skipped:context_only/no_evidence/offline/budget。

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

没有接通 CLI/MCP、在线 R1/R4、请求缓存或召回迁移。并发模型调用的锁范围、重试/缓存失效策略另段处理；本段只读证据包可以独立测试和提交。


## R4 核验与计算

`r4::decode/evaluate` 检查冻结字段、枚举、必填可空项和非空文本。FactView/ComputeItem 的每条依据必须落在本包的来源与连续片段中，复用 locate_quote 的空白归一并恢复原文，不拼接不同片段。query_only 的本轮文本不能成为事实或计算依据。record_ids/supersedes_record_ids 除在输入内，还必须有该条记录的依据；supersedes 必须同时有旧条目与新方依据。legacy 没有可核准来源时不能形成事实；有获准原文时可重新判断原文，不补造旧限定语。

不确定性、条件、TimeInterp 及 anchor_quote 都检查对应依据来源；输出完整保留 polarity、strength、uncertainty、conditions、times、current、applies。来源接收日与时区在同一读快照中取得，不接受模型自报时钟；来源时刻改变会推进 source_revision，已有快照屏障拒绝旧日期锚点。时间换算复用 W2，并补核对纯月名不得补为日精度；Anchored/Recurring/Unknown 沿用 W2 的 unknown，所需端点无法确定时计算不足。代码不证明概括、主体、条件适用性、事件同一性及时间自然语言解释的语义正确性，这些由 R4 样本评测验证。

任一结构/引用失效整份输出变为 degraded、semantic_judgment=failed、facts=[]、computed=null；既有记录与原文继续交出，不保留部分有效模型事实。使用冻结 model_invalid_output/model_timeout 原因。合法输出因身份、覆盖、单位、精度或算术容量不足而不能计算时，保留事实，computed=null，附 compute_insufficient，并按状态优先级得到 insufficient 等状态。

- count：按非空 event_group 去重，null 不视作独立事件；只计已确认组，存在未知身份或范围缺口时只能给 at_least；没有已确认组时不足。完整且明确的空集可精确计零，同一来源允许多个条目。
- sum：同组重复数量归一后相等只计一次，矛盾、身份未知或数量缺失时不足。coverage=complete 还须满足代码范围完整、无裁剪/待判记录/整理缺口，相关 raw 已由条目覆盖；模型自报不能覆盖这些已知缺口。冻结形状没有证明遗漏部分非负的字段，因此不完整 sum 保守返回不足，不把已见正数的部分和当下限。
- quantity 使用有符号十进制文本及 i128 定点数，逐步 checked 运算，最多38位小数；不使用浮点数。机械依据当前只核准字面数字与相邻单位；不解释中文数量词。相同字面单位可直接运算；固定 SI 别名与质量/长度的十进制倍率可归一（kg/g/mg、km/m/cm/mm 及中文别名），其他换算、汇率和隐含单位不推断。不能运算或溢出则不足，伪造数字/单位依据则整份无效。
- elapsed：两个自然日端点用现有 openai::elapsed_count 算法与冻结四种取整规则。端点粗于日或多日区间、倒序、未知时不足。@request.at 只允许第一个操作数，由可信 at 和时区生成 request_time；不使用 current said_on 或执行日期代替。
- compare：数量同维度归一后比较；日期用半开区间，分离即确定先后（X3），重叠不足；同一个精确日可判 equal。不得以粗精度区间相同判日期相等。

## R5 派生与边界

成功判断按 needs_clarification > conflict > insufficient > answered 排序；R1 未解析/歧义主体也触发澄清。已有相关 open 关系仍阻止确定答案，除非同一个 current=yes 事实以新旧依据明确 supersedes 全部冲突成员，或只保留唯一所引用成员；此消解只用于本轮，不改变存储或关系状态。R4 自报 conflict 同样得到 conflict。没有事实或计算的 supported 不包装成 answered。

grade 仅为标注：所有依据来自已提交记录且没有 raw/current 来源时为 committed，否则 uncommitted；同一来源同时出现在 raw/records 时保守取 uncommitted。update 合并显式 ID 和依据实际涉及记录的状态，不能省略 record_ids 绕过 pending；pending 优先、原因去重；其他未知或只有原文依据则 null，全部已明确判定才 judged。grade 不改变 status。

history 仅包含模型 current=no 且由 StateChanged/PreferenceChanged/ConstraintChanged 结束的记录，Correction/UserRetracted 等只保留审计。until 从同一快照的已知结束日取得（内部读取 RecordView2.end 或已有 Explicit valid_to），不在冻结 RecordView 添加 end；未知/粗精度或多条记录结束日不一致为 null，不回填接收日。旧 Supersession/Inferred 等边界不能证明真实结束日，until 保守为空。

clarification 由代码模板列出歧义实体名称或未解析指代，引用当前触发原话。若未解析指代仅来自 USER context，冻结 SourceRef 没有 context 定位项：保留问题中的真实指代，evidence=[]，不把历史文本假装成 current。R4 单独要求澄清而未给触发事实时使用通用模板，不补猜指代或条件。

conflicts 的冻结类型是 RelationLink[]，只放输入中的未在本轮消解的 open 关系；R4 conflict 涉及的记录保留在 records/facts 与 status 中，不把记录伪装成关系或由代码新建关系。失败/跳过时沿用原有关系审计输出。

`tests/recall_r4.rs` 从已复核 R4 样本的 input 展开真实 SQLite 来源与 W5 提交、关系、结束信息，通过同一 assemble/finish_r4 注入人工输出，并重开验证无写入。样本的 expect/must_not 不进入运行时；r4_04 追加以原始 single/lookup 计划验证：问题与新原文没有词法命中时仍取得 s-40，R4 注入后南京 current=yes、grade=uncommitted，上海 current=no，不判冲突。`tests/recall_offline.rs` 同时验证最新优先的数量/字节裁剪、裁剪前计数、任务/归属/主体隔离和已完成来源相关性规则。模型语义质量另由后续真实评测衡量。
