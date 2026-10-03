# 召回运行时：离线证据闭环

本段实现 contracts 3.2 和 3.4 的 R4 跳过分支，复用 SQLite、原文词法检索、W4 记录/关系投影及整理控制回执。没有模型调用、来源写入、读取计数更新、数据库迁移或旧召回入口改动。

## 入口与快照

`recall::run_offline(store, request, plan, budget)` 使用同一个 `assemble` 入口，随后消费 `Assembly::finish_skipped`。后续 R4 在这两个步骤之间取得证据包并注入判断，不重建一条测试专用流水线。

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

相关未整理、失败、待重判来源进入 raw，按 received_sequence 升序、source_id 排序；保留接收日、整理状态及 MemoryRequest 的冻结字段。接收日由来源 observed_at 与可信时区换算。MemoryRequest 从现有合并回执读取，控制记录优先，不自行重新理解请求或执行删除/期限。删除或未获准保存的来源不进入 raw。

只读 current_message 固定保留完整 query，与检索和存储证据预算无关；source_id/sequence=null，enrichment=not_stored，memory_requests=[]。bootstrap 无本轮消息时为 null。R1 current_evidence=query_only 的送审资格通过 Assembly.review_current_message() 保持为 false；标记不扩展冻结 JSON。仅有 query_only 的问题文本不算判断证据。

相关关系在 R3.relations 和跳过输出的 conflicts 中保留 open/resolved/invalidated 及可见依据。这是已有关系的审计呈现，不生成本轮冲突判断。跳过时 facts=[]、computed=null、history=[]、clarification=null；need=context_only 时 status=context_only，其他原因均 degraded，semantic_judgment=skipped，并写入 semantic_judgment_skipped:context_only/no_evidence/offline/budget。

## 预算与完整性

默认上限：64 records、32 raw、64 relations，三类存储证据条目序列化 JSON 的合计字节数 262144。current_message、request 和输出包固定元数据不计入此证据预算；模型输入/输出总预算在召回④实施。

先获取范围内候选和展开关系，统计缺口，然后裁剪。记录按检索顺序整项保留，raw 按接收顺序保留，关系按 ID 保留；字节额度依次分给 records、raw、relations。记录/关系不破坏其内部字段；raw 正文可以在 UTF-8 边界取前缀，按 JSON 实际转义后的长度核算，放不下元数据则整项省略。所有数量/字节裁剪均令 retrieval_truncated=true、plan_complete=false。预算裁掉的记录/来源仍进入已知缺口计数；关系中的 ID 可以指向已裁掉但未删除的记录，不绕过预算补回正文。

- pending_updates：裁剪前唯一相关记录中 update.pending 的记录数，包含已提交记录。
- pending_related_sources：裁剪前已检索、已获准进入 raw 的未完成来源数；不能把 0 解释为未检索范围没有缺口。
- enrichment_gaps：裁剪前 legacy.gaps 的条目总数，加无法映射的来源状态的缺口数；不是 legacy 记录数。
- plan_complete：获准的计划范围已枚举、关系未因可信范围被扣留，且没有预算截断、未知过滤时间或来源归属/状态缺口。自然语言条件是否匹配仍由 R4 判断；本字段不认证语义正确或精确计算。

## 保守边界与待裁定项

来源只存 owner/task，没有可靠的独立主体字段。显式限制主体时，没有已提交记录可链接的未整理原文不按词面猜主体；整条原文同时涉及范围内外主体/任务时也不借入。两种情形都令 plan_complete=false。可见 pending_related_sources 仅计已确定可进入包的来源，不拿未知归属的计数代替原文。是否引入可核准的原文主体定位，留给后续设计，不能靠 R1 给原文扩大授权。

旧 completed_pending 映射 pending，stale_requires_review 映射 needs_rejudge；旧 completed_rejected 以及未知状态没有冻结枚举的唯一对应项，暂扣留该 raw 并增加 enrichment_gaps，plan_complete=false，不自行解释成 failed 或成功。已有 records 继续返回，旧路径字符串回执保持原状。未知状态的正式映射与 enrichment_gaps 按缺口条目计数属于需产品复核的口径，本段未改冻结契约。

没有接通 CLI/MCP、在线 R1/R4、计算、请求缓存或召回迁移。并发模型调用的锁范围、重试/缓存失效策略另段处理；本段只读证据包可以独立测试和提交。
