# W5 存储扩展：冲突关系、更新状态与任务恢复

W5 的冲突关系、重判复用与归并、更新状态使用版本 8 的存储形态（7 → 8），按 w5-acceptance.md 分段实现。运行时准备第②段增加任务轮次和持久计划，数据库另升 8 → 9；M1.5 第③步再升到v10，保存来源代际、主体确认与管理审计，第④步接通历史补录出处和时间锚点，W5 回执格式仍为 v8。当前契约为 W1/W3 v4、W4 v5、R1 v2、R4 v7、recall v8。依据：contracts.md 1.6（UpdateState）、2.4（RelationLink）、2.5（W5）、3.2（RecordView）。第7节保留早期分段计划及当时的旧召回接线安排，不表示旧执行器仍可用。

## 1. 放在哪里

两者都放进现有的 owner 快照（`PersistedMemoryState`），不新增表或存储框架（contracts 1.6、2.4、§7 “未解决关系的存储”）。

| 新增 | 位置 | 说明 |
| --- | --- | --- |
| 冲突关系 | `relations: BTreeMap<String, StoredRelation>` | 与 Memory 生命周期分开；旧快照缺省为空 |
| 关系编号 | `next_relation: u64` | `rel-<n>`，单调递增，不复用 |
| 每条记录的更新状态 | `record_updates: BTreeMap<MemoryId, UpdateState>` | 只记 W5 产生或重判过的记录；没有条目即“未知”（召回为 null，不当 judged） |
| 回执里的更新状态 | `W5Receipt.updates: BTreeMap<usize, UpdateState>` | 候选 → 其产物的更新状态；contracts 1.6 要求随决定回执保存 |
| 回执里的归并映射 | `W5Receipt.merged: BTreeMap<MemoryId, MemoryId>` | 已提交 ID → 保留目标（contracts 2.5）；本段只定义字段，第 4 段才写入 |

完整 history 不另存：W5 产生的替代链从回执的产物（products）与决定里的完整 targets 读出，归并链从 `merged` 读出；W5 之前的记录仍用现有 supersedes。读取时按 owner 扫描回执建索引，不新增表。第 1 段把以上字段全部定好，后续三段只写入、不再改存储形状，这些 W5 段只升到版本 8；任务恢复新增的版本 9 见第 6 节。

不把更新状态放进 `Memory`：`Memory` 的序列化形状同时是冻结的 LegacyMemory（contracts 3.2，`tests/w4_legacy_fixture.rs` 按原样回写校验），加字段会改动它。

运行时第②段另将 `EnrichmentTask` 的可选 round / rejudge / operation_id / order_sequence 放在原任务中；完整来源输入绑定、完整候选及提交计划位于 `SourceRecord.result.w4_checkpoint`。第③段在计划前用 `w4_save_inputs` 原子保存同轮不可变候选，旁侧 `result.w4_input_meta` 保存 round、ready、Admission 布尔值、输入计数和内部诊断（预算超限的 item / actual / limit、检索 full_text / fallback 等），不保存原文副本。已提交进度仍只从 W5 回执推导，不新增表或游标。任务汇总在 `w4_summary`，旧路径不输出这些内部检查点。显式新轮清除旧输入元数据与汇总；来源删除清空自身 result，他处原文副本仍由既有检查点清洗处理。

## 2. 结构

```text
StoredRelation {
  id, kind: "conflict",
  members: [MemoryId],            // ≥2，不重复
  conditions: JSON, times: JSON,  // contracts 的 TextQualifier / TimeInterp 原样保存，W5 不解释
  basis: [EvidenceRef],           // { source_id, quote }
  status: open | resolved | invalidated,
  resolution: EvidenceRef?,       // resolved 时必填
  created_by: group_id,           // 建立它的 W5 组
  closed_by: group_id?,           // 解决或失效它的组（失效来自删除时为 null）
}
UpdateState { status: judged | pending, reasons: [update_scope_incomplete | update_target_unknown | update_relation_unclear] }
```

conditions、times 按 JSON 原样保存；完整契约已有 TextQualifier、TimeInterp 类型，W4 转换时按已核验引用取副本，W5 不再解释语义。

决定的 `basis`、`effective_time`、`relation_scope` 为空时不序列化，旧组指纹不变。`effective_time = { interp: TimeInterp, resolution: TimeResolution }` 保存时间原文和原精度换算结果；W4 校验它属于该候选已核验时间、不是 retention，转换取对应换算结果。W5 的独立一致性检查是：存在 effective_time 时必须满足 `effective == effective_time.point()`；仅 resolved、日精度、恰好一天可有时间点，其余为 None。没有 effective_time 的旧形态时间点只允许 StateChange；pending 不允许 effective、effective_time、reason 或 relation_scope，reason 仅允许 StateChange。W5 不独立重算时间，候选时间核验属于上游。

effective 可用于任何已定动作：StateChange（含 W4 correction / refine）及 Retract 用它作目标结束时间；没有或未知时不回填接收时间。月精度、多日区间不写旧 Memory 时间点，由记录视图保留原精度结束时间。New / Reinforce / Conflict 仅记回执，不据此结束记录。

## 3. 更新状态由代码生成

W4 不直接给 UpdateState；W5 按下表生成，写进回执和 `record_updates`（contracts 1.6）。

| 情况 | 产物的 UpdateState |
| --- | --- |
| 范围完整，决定 decided，且不替代未解决冲突成员 | judged，reasons 为空 |
| 范围不完整（组携带 `scope_complete = false`） | pending，含 update_scope_incomplete |
| 替代了未解决关系的成员（contracts 2.5） | pending，含 update_relation_unclear |
| W4 决定为 pending（target_unknown / unclear_relation） | pending，含对应原因；合格断言可提交，撤回只暂存 |

- 原因去重、排序；pending 至少一个原因。
- 只有显式重判覆盖了对应缺口才改写已有状态（contracts 1.6）：普通召回、整理完成、重放都不改。重判复用 committed_id 时，新缺口与旧缺口合并，旧缺口**逐项**判断是否清除，一项满足不代表其他项也解决：
  - update_scope_incomplete：本次组的 `scope_complete = true` 才清除；
  - update_target_unknown：本次决定 decided 且给出了明确目标（或判定为 new）才清除；
  - update_relation_unclear：本次显式重判覆盖了这个缺口才清除，即实际 W4 输入包含该记录仍存在且未删除的前身（或相关旧成员），以及它们所在的**全部最新**关系状态（open、resolved、invalidated，或像 w4_17 那样根本没有 StoredRelation 的普通新旧关系），并对该记录给出已定决定（共存、另建冲突、解决旧关系、普通替代均可）。被任务隔离挡住的关系不算覆盖，数据库里的关系或 open 关系编号集合不能代替实际关系输入；已删除或不存在的前身不要求输入或快照、不阻止清除。关系覆盖与 scope_complete 独立判断。不要求存在 open 关系，也不要求旧关系被关闭：更新判定完成不等于冲突解决（contracts 1.6）。关系被删除失效或被其他组解决只改变输入状态，本身不清除，要等下一次显式重判。
  - 全部清空后才变为 judged。
- 归并（重判为 reinforce 另一记录）时，已提交记录退出投影，其 `record_updates` 条目随之删除；目标记录的状态不因被强化而改变。

## 4. 关系的生命周期

- **建立**：conflict 决定的新记录与 StoredRelation 在同一 T2 写入（contracts 2.5）。成员 = 本组新产物 + 既有记录；新产物在本次 T2 中才产生，不在输入快照里，既有成员必须都在组的 `seen` 中并通过重查。basis 须同时引用新旧双方来源。关系编号的分配（`next_relation` 自增）与关系写入在同一 T2。
- **解决**：决定带 `resolves_relations` 时，同一 T2 重查关系（必须仍为 open、成员与 W4 输入时一致）；关系的快照指纹与目标一起记入组的 `seen`，不一致则整组 needs_rejudge。解决后写 status=resolved、resolution、closed_by。
- **替代未解决成员**：关系与成员不变（contracts 2.5），只给替代记录写 update_relation_unclear；召回经 history 带出前身所在的 open 关系。
- **失效**：成员、依据（basis）或解决依据（resolution）的原文被删除时，关系置为 invalidated，不连带删除另一端仍获准的记录（contracts 2.4）。open 和 resolved 关系都要检查：resolved 关系失去解决依据后不能再当作有效的已解决关系，也不回到 open（那是推断）；resolution 原文打墓碑，保留审计。
- **删除闭包必须先改**。现有 `memory_forget`（`src/chat.rs`）按“替代链双向 + 被删记录的全部原文”反复扩散到不动点。w4_22 中 110、111 都被新记录 N 替代：删除 s-501 → 110 → N → N 的原文 s-510 → 111 → s-502，三条记录和三份原文全删（gpt 已用真实 SQLite 复现）。闭包改为：
  1. **起点**：source_id / staged_id 为对应原文；memory_id 为目标记录。
  2. **谱系**（只对 memory_id 起点，保留现有测试 `forget_hides_the_entire_chain…` 的语义）：从原始目标出发，分别求全部前身（沿 supersedes 及回执 targets 反复向上，可多前身）和全部继任（沿 superseded_by 反复向下），取并集。向上只向上、向下只向下：不从继任者折回它的其他前身。w4_22 中以 N 为目标覆盖 110、111；以 110 为目标覆盖 N，但不经 N 到 111。
  3. **整条消息**：起点原文及谱系记录的原文整条删除（现有规则），这些原文派生的记录一并删除。
  4. **不再迭代**：第 3 步因“同一原文”连带删除的记录，不再把它们的其他原文加入删除集合，也不沿它们的谱系走。谱系只在第 2 步从原始目标求一次。
  5. **保留条件**：未被第 2 步选中、但部分证据来自被删原文的记录，只有在剩余证据中仍有**能确认独立支持该断言**的来源时才保留；否则删除。按证据判断，不按记录年代：能确认的支持来源 = 创建该记录的原文 + 回执中对它做 reinforce 的原文。回执包括 W5 回执和旧整理回执（`TurnReceipt` 的 outcomes 记有 New/Reinforce/Retract 的执行结果、目标记录，请求里有 session/turn，`src/chat.rs` 写入处），由代码读取确认，不调用模型。撤回、替代时追加的控制证据（例如“那句话不对，收回”）不算支持，留作审计但不能让旧事实继续可召回。只有剩余来源的角色确实无法确认时，才删除这条受影响的记录；它的其他未删除原文保留，不继续扩散（第 4 步）。
  6. **保留时同步**：去掉被删原文的证据（打墓碑），`reinforcement_count` 与 `last_reinforced_at` 按剩余的**独立强化来源**重算（按原文去重，不按回执数量：同一原文换组重试可能留下多个回执，但只算一次强化），无剩余强化则计数为 0、时间置空。
  7. **关系**：按上面“失效”规则检查全部 open 与 resolved 关系。
- 继任者被删除、前身仍在时，前身保持 superseded，不自动恢复为 active（删除不等于撤回那次变化）；召回中它的继任 ID 不可见。
- **实现说明**（`src/forget.rs`；GPT 复审 0004 确定）：
  - 支持来源的确认：旧整理回执按 `Applied{action, memory_id}`（New / Supersede / Refine 为创建，Reinforce 为强化）；`memory_clarify` 的回执按 `request.staged_id` 关联到原暂存回执（`Pending.staged_id`）的轮次，澄清消息本身不算支持；W5 只认已提交（Committed）的回执，按组指纹里候选证据的轮次确定原文，证据跨多轮的候选角色不明。
  - 文字清洗（不改存储形状）：组的原文被删时该回执指纹整体换成墓碑；其他回执逐层解析组 JSON 与其中作为字符串保存的记录、关系快照，被删记录的快照整体换成墓碑，其余字符串里的被删文字换成墓碑，元数据字段（id、session_id、turn_id、source_id 等）不动；关系与决定里出自被删原文的 basis / resolution 打墓碑，条件、时间一并清洗（已失效的关系也清洗）。受影响的组以后重放会因指纹不符而失败关闭。
  - 关系文字里的 Quote（条件、时间的 `quote.text` 等）只是原文的子片段时也要清掉：关系、决定及回执里的关系快照中，凡是被删原文正文子串的 Quote 文字整体换成墓碑（GPT 复审 0006 P1）。这条规则只用于与本次删除有关的关系与决定（引用了被删原文，或成员、目标里有被删记录；复审 0007）；记录快照、候选与无关关系只按整段片段替换，避免误清别的原文里相同的短语、改动无关回执。
  - 澄清回执（请求带 `staged_id`）自身的 Pending 结果不算原陈述；定位原暂存回执时排除它们（复审 0006 P2）。
  - 软删除的记录：status=Deleted，全部证据、值打墓碑，条件、时间（只留得知时间）、标签清空（这些都可能是原文片段；复审 0007）。审计保留 ID、类型、谓词、状态、谱系与墓碑证据，不保留原内容；`audit_evidence_retained=true` 只表示这些审计条目仍在。
  - 没有原文记录的旧记录（`capture_source=false`，契约 §2.6）：按 memory_id 删除时不扩散到同轮其他记录（没有可整条删除的原文），但它的证据片段与暂存候选的片段一样，从留下的旧回执（请求与结果）和 W5 回执里清掉；其他请求（原文记录与旧回执）历史里含有它们的副本整条去掉（复审 0008）；旧回执被改动后重放失败关闭。
  - 保留的记录不能带着只有被删原文说过的文字：文本值出自被删原文（例如通用 fact 的值就是那句原话），而剩余证据里没有这段文字时，记录一并删除；剩余原文照常可检索（A 段自查发现）。
  - 强化次数重算只算独立强化：与创建该记录的原文相同的强化（同一原文另一组的 Reinforce，执行时证据重复、计数未增）不计入（复审 0007）。最近强化时间取剩余强化来源的得知时间：原文记录优先，无原文的旧轮次取旧回执请求里的显式 `observed_at`（复审 0008）。
- 这改变了现有删除行为，放在第 3 段实现，验收至少包括：
  - w4_22 删除 s-501：111、N、s-502、s-510 保留，关系失效；
  - w4_22 按 memory_id 删除 N：110、111 都删除；删除 110：N 删除、111 保留；
  - 三代链按中间一代删除，三代全删；
  - w4_21 删除解决来源：关系变为 invalidated、不再算已解决，前身仍为 superseded；
  - 原陈述被删、只剩撤回证据的记录被删除；剩余来源角色无法确认的记录被删除，其他原文保留；
  - W5 之前创建、升级后经 W5 或旧回执确认强化的记录，删除创建原文后保留；
  - 强化过的记录删除一个强化来源后保留，计数按独立来源重算（同一来源多个回执只算一次），最近强化时间同步。
- 关系不因成员进入历史而自动关闭；只有 W4 决定或用户确认能解决。
- 删除来源时同时清空自身检查点和租约；清洗其他检查点中的原文副本、完整候选、依据与 JSON 编码快照。受影响计划丢弃、标 invalidated，任务为 needs_rejudge 并撤销认领；保留不含原文的历史组 ID / 产物 ID 供审计。无关检查点不变，晚到提交或收尾被拒绝。当前快照清洗不等于物理擦除 WAL、备份或诊断文件。

## 5. 事务与过期判断

- `SqliteMemoryStore::transaction` 用记录、实体、暂存、关系及 `record_updates` 判断业务变化，推进 revision；认领和检查点元数据不单独推进业务 revision。AF78 的纯新增实体（旧实体逐字段不变、新 ID、名称/别名按现有目录规则不与旧实体冲突、其余业务数据不变）只豁免 source_less 屏障推进，不降低既有水位，也不恢复 needs_rejudge。修改、删除、冲突及混合业务变化仍触发屏障；较新来源与阶段绑定检查不变，重启后的目录差异仍可能要求显式重判。
- 组的 `seen` 保存实际输入中的目标、重判前身及运行时绑定的 committed_id 记录版本；`seen_relations` 保存实际 W4 输入中相关关系的内容版本，包括重判前身的 resolved / invalidated 关系。已删除或越界的上下文记录、被隔离的关系内容不补入快照。
- 只比对已见关系各自的指纹不够：W4 之后若目标新加入了一条 open 关系，逐条比对发现不了。因此每个目标的指纹还要覆盖“它所在的 open 关系编号集合”；集合有增减（新建、解决、失效）即整组 needs_rejudge。
- 范围完整时，记录所在 open 关系都须有内容快照；范围不完整时允许实际存在的隔离关系缺少内容快照，不存在的关系编号仍直接拒绝。仍核对 open 编号集合且保留范围缺口，不能据缺少的内容清除关系缺口。要解决的关系不适用这条例外：关系内容与全部成员快照始终必须齐全。

- 新任务在写锁内核对完整来源输入、处理版本、round、operation_id、order_sequence、attempt 和取得锁后的租约时间；输入保存、计划写入、W5 提交、续租和收尾共用归属检查，旧执行者 claim_lost，不能释放新认领。输入不可同轮替换，计划须与已保存输入完全相同。W5 目标 / 关系快照检查继续生效，不用全局 revision 等值检查阻止自身首组后的余组。
- 新接收和显式重判共用 owner 内单调序号分配：所有 received_sequence、order_sequence 与两条屏障的最大值加一。首次任务沿用接收序号；重判只改任务排序序号，不改原文接收时间或顺序。W5 按任务排序序号推进 last_applied_source_sequence；无来源业务变更的屏障也覆盖全部任务排序序号。低于应用水位或不高于无来源屏障的余组需重判；新路径水位相等还须有本轮匹配的已提交回执，其他来源不能借用该许可。
- 首组提交前落库完整不可变计划；业务写入与组回执仍是同一事务。重开后按计划核对回执指纹，已提交组直接重放，余组分别核验。单个组 needs_rejudge 不阻止合法独立组；全部组已有回执时只补总状态，不执行业务动作或模型调用。显式重判的启动回执与轮次更新同一事务，重复请求不重复加轮次；继承历史产物绑定，失效产物不能作为新断言再次插入。
- 第③段 `enrichment::run_source` 在计划恢复时跳过两个提供者；只有输入检查点时复用候选与准入，重新组装后调用 W4 提供者。需要 W4 但未接入为 pending、释放认领、零业务提交；新输入预算超限为 budget_exhausted，不截断后继续。模型调用预算与阶段缓存由第④段提供者负责。
- 提供者/解码错误可能包含原文片段，新运行时只持久保存已知错误码（未知故障用内部 attempt_failed），不保存异常细节到 source.error 或输入诊断，避免检查点之外留下无法随来源清洗的原文副本。
- `w4_finish_round` 与 `enrichment::receipt` 共用逐候选结果：产品取真实 products（其更新状态可取 live record_updates），强化/归并取实际目标及本次回执 updates；撤回执行成功为 committed / memory_id=null，待定撤回虽有 Committed 组回执，候选及来源仍 pending。Held 为 pending（未解决主体为 unresolved），混合结果保留已提交候选。输入就绪且候选为空为 no_candidates_unreviewed，不推断 completed_no_facts；决定过期或余组屏障为 needs_rejudge。专用转换只输出 contracts 3.5 的片段，不展开检查点，也不改旧 MCP 输出。
- 当前 write_pipeline 缺省且只支持 w4_runtime_v1，legacy 值及旧检索字段在打开库前拒绝。inline/deferred 的 T1 绑定 processing_version 并排队，MCP 单 worker 自动执行；CLI --call 不启动 worker，--enrich-source 显式同步处理，drain 只选 queued。off 来源须显式整理。既有旧版本来源、失败与排队任务不自动转换或执行；读取/删除与顺序屏障保留。首轮round=0，续跑不加轮次，终态重判须显式新轮。冻结3.5对外片段、组计划及认领规则见 w4-code-side.md 第8、10节。

## 6. 版本 10 与兼容

已发布版本及本段写入版本：

| 版本号 | 写入程序 | 回执特征 |
| --- | --- | --- |
| 5 | W5 之前 | 无回执 |
| 6 | bf83fd9 | 无 status、无 legacy_fingerprint，v6 形态指纹 |
| 6 | baa1ff9 | 有 status、无 legacy_fingerprint，当前形态指纹 |
| 7 | 708b2d2 | 同 baa1ff9 |
| 7 | 88eb857 / ca16b64 | 有 status、有 legacy_fingerprint |
| 8 | f1d1828 / 9933ece | 持久 form=pre8 或 v8，关系与更新状态 |
| 9 | 运行时第②/③段 | 新任务轮次、排序、输入与计划检查点；form 不变 |
| 10 | M1.5 第③/④步 | 来源代际、主体确认、管理审计、历史补录出处与时间锚点；form 不变 |

规则：

- 新写入程序读 1–11，只读检索支持 3–11；操作连接只接受已初始化的 v11/WAL 数据库。迁移到 8 时快照补 `relations = {}`、`next_relation = 0`、`record_updates = {}`，回执缺 `updates` 时为空（含义“未知”，不是 judged）。迁移只补缺省，不推断旧记录的更新状态。
- 版本 8 → 9 只提升数据库版本、由可选字段补缺省；不将旧任务迁入新路径，不改旧回执、指纹或 form。缺省新字段不序列化，旧任务写回也保持旧形状。版本 8 的 9933ece 程序拒读 9，防止改写快照丢掉轮次和检查点。
- 版本 9 → 10 沿用现有 owner 快照与索引；旧 owner 的来源代际缺省为0、来源按普通出处读取，确认/审计缺省为空，不改旧回执、不自动入队或重整理，已删墓碑保留。v9 写程序拒绝 v10，当前程序拒读未来版本12及以上。
- 第③段直接扩展 `w4_runtime_v1` 的库级语义；此前没有运行时登记调用方或真实 v1 任务，不新增 v2 或版本兼容分支，不升数据库版本，不改冻结契约或 W5 回执形状。
- `tests/fixtures/w5_storage/v8-9933ece.sql` 来自 9933ece 自己的程序升级真实 v7-ca16b64 库到 8 并写入失败旧任务；不是将新库降版本号。`tests/w4_recovery.rs` 检查迁移前后旧任务、回执不变，关闭重开、旧回执重放与版本边界。
- 版本 7 的程序（88eb857、ca16b64）拒读 8：它们改写快照时会丢掉关系和更新状态，冲突会消失、pending 会变成未知。
- 回执的旧格式比对规则（ca16b64）保持不变。
- 组新增 `scope_complete`、`seen` 新增关系部分后，组指纹形态会变。不能靠“缺 `updates` 字段”识别旧回执：迁移补上 `updates = {}` 并保存后，这个依据就没了。改为迁移时持久写入格式标记 `form`：已有回执写 `pre8`，新回执写 `v8`；记录指纹是 Memory 的 JSON，无法从字符串可靠反推形态，所以不靠形态推断。
- `seen` 的新部分与原记录指纹分开存放：`W5Seen { id, fingerprint, relations }`（目标所在 open 关系编号集合）以及组的 `seen_relations: [{id, fingerprint}]`；都为空时不序列化。`pre8` 回执重放时，按旧投影比对：去掉 `scope_complete`、`W5Seen.relations` 和 `seen_relations`，再叠加现有 `legacy_fingerprint` 的 v6 规则。`v8` 回执只按当前形态比对。
- 测试必须包含“迁移 → 关闭 → 重开 → 重放”，以及重开后 `form` 仍为 `pre8`。
- 兼容测试按上表逐一构造各版本程序实际写出的数据（不是从新格式删字段近似），每种都验证：能打开、迁移后字段齐全、迁移保存后重开仍能识别旧格式、同组重放结果不变、旧程序拒读新版本。

## 7. 分段（历史记录）

1. **存储与迁移**（本文）：加字段（含回执 `updates`、`merged`、`form`，组的 `scope_complete`、`seen_relations` 与 `W5Seen.relations`）、版本 8、事务业务快照纳入新字段、兼容测试。不改提交行为。
   - 组与决定上后续各段要用的字段也在这一段定好，避免以后再改回执里保存的形状：决定加 `resolves_relations`、`resolution`、`relation`（conflict 要建立的关系）、`pending_reason`，动作加 `Conflict`，候选加 `committed_id`。都在为空时不序列化，旧组的指纹不变。执行尚未实现前，携带这些字段的组一律拒绝，不静默忽略。
   - W5 的存储类型（回执、组、决定、关系、更新状态）拒绝未知字段：将来的程序若再加字段，当前程序读到时报错，而不是改写快照时悄悄丢掉。
   - 兼容测试用的旧数据是各提交自己的程序写出的数据库，导出为 `tests/fixtures/w5_storage/*.sql`（模式 + 行 + user_version；仓库忽略 `*.sqlite3`，故存文本）。
2. **更新状态**：W5 按第 3 节生成并写入；召回的 RecordView/RecallFact 带出 update；`pending_updates` 计数。
   - 实际范围（用户确认）：W5 当前只在测试中调用，RecordView / recall.v3 也未实现。本段做写入与读取，并在现有输出（`memory_list` 的记录、来源检索的 `known_memories`、召回的 `state_annotations`）里附 `update`；只在有条目时附加，没有条目即未知，旧记录的输出保持原样。RecordView、`pending_updates` 计数随 recall.v3 实现。
   - 每个决定的状态写进回执 `updates`（含强化、撤回等无产物的决定）；只有产生的记录写入 `record_updates`，被强化或被结束的目标不改状态。
   - 待定决定：动作只能是 new（断言作为新记录追加）或 retract（零写入）；可列已定位目标（须有快照），不修改它们，不带 effective / effective_time / reason / relation_scope。待定撤回不进入现有暂存队列（那是主体待澄清用的，澄清后会交给旧确定性策略）；候选与原文由上游保留。
   - 逐项清除规则实现为 `UpdateState::after_rejudge`（本段单元测试），第 4 段在重判复用时接入。
   - 候选类别与动作必须一致：撤回候选只能走 retract，retract 只接受撤回候选，不一致整组拒绝、零写入。
   - 现有答复门读取待定状态：召回不为待定记录生成 `unique_in_snapshot`；`answer_policy` 对“唯一事实”“精确身份”两条路径按记录、对模型答案与计算答案按其引用的记录检查（补答候选在构造时从同一快照按记录带上待定标记，记录不在本地证据包里也能拦下；同一原文里的其他独立断言待定不影响；原文任务尚未完成、候选以未整理原文提供时，按答案片段绑定到该原文已提交的记录），有 pending 即返回 `answer_not_verified`（沿用现有状态与安全文本，不新增状态值）。没有 `update` 的旧记录照旧。
3. **冲突关系**：建立、解决、替代未解决成员、删除失效；w4_21（D1）、w4_22（E1–E3）。
   - 分两步提交：前半为建立、解决、替代未解决成员与关系快照核对；后半为删除失效与 `memory_forget` 删除范围重写。
   - 依据的机械核对：conflict 的 basis 与 resolution 的片段须逐字出现在获准保存（capture_source）的原文里；conflict 须同时有本原文与至少一个目标来源的依据；resolution 须来自本次认领的原文。
   - W4 的 correction / refine 走 state_change 并带对应结束原因（Correction、Refinement、ScopeRefined）。
   - 已定 retract 可带 effective_time，日精度单日结果同时写目标 valid_to；没有或未知时结束时间仍未知。effective_time 与 effective 的一致性及旧形态兼容规则见第 2 节。
   - 处在 open 关系里的记录与待定记录同样不能被核准为唯一当前值；旧格式输出附 `open_relations`。E2（召回经 history 带出前身所在关系）随 recall.v3 实现。
4. **重判复用与归并**：B2、B3、B3a，以及第 3 节的状态改写规则。
   - committed_id 的核对：记录仍有效、在 W4 输入快照中（指纹核对“有效版本”）、是本原文某个已提交 W5 组里候选的产物、候选的完整证据（不只是编号）都在它上面且来自本次认领的原文、由候选得来且之后不会被 W5 改动的字段（断言内容、确定程度、明确记忆请求、标签、留存层级、得知时间、时间引用与到期时间）与候选一致。不符即整组 needs_rejudge（contracts 2.5，复审 0010）。
   - 结构错误直接拒绝、零变更：committed_id 重复、不在快照中、作为自身的目标或归并目标、重判为撤回。
   - 复用：new / 待定只复用；state_change 以它接替目标；conflict 以它与目标组成关系。不插入、不追加证据、不计强化。
   - 状态改写：`UpdateState::after_rejudge`。“关系输入”按第 3 节判断：产生它的决定所列仍存在且未删除的旧目标都在实际输入对应的记录快照中，且它们的全部关系（open / resolved / invalidated）都在实际输入对应的关系内容快照中；已删除前身不要求快照。被任务隔离挡住的关系、提交检查用的 open 关系编号集合均不能冒充 W4 已见关系。
   - 归并：证据按来源幂等并入保留目标；强化与证据转移分开，只有归并原文此前没有在保留目标上留下证据时才计一次强化，顺带转入的其他来源证据不计数（复审 0011）；已提交记录退出投影（status=Deleted，`record_updates` 条目删除），回执 `merged` 记 已提交 ID → 保留目标，`products` 不含它。按整个依赖组判断：组内全部决定执行后，它若仍在 open 关系里，整组拒绝，与决定顺序无关（复审 0010）。
   - 删除时的支持与强化来源：被归并记录的确认支持来源转给保留目标（只转支持，不转强化），归并成链时反复转移。W5 回执里的 reinforce 决定（含归并）是否算一次强化，按组快照里目标在提交前是否已有该候选的证据判断（同 ID 或同一轮，与执行时共用 `evidence_present`）；没有快照的旧回执照旧认定（复审 0012）。

## 8. 已定（原待定项）

- `scope_complete`：W5 组直接携带运行时代码为 W4 输入生成的值，并纳入组指纹；W5 只核验快照，不重新判断检索范围。
- 关系编号：owner 内唯一即可，编号分配与关系写入在同一 T2。

保存计划被顺序屏障拒绝时，绑定 operation 的 `w4_order_blocked` 事件仅记阶段、轮次、排序序号、两条水位和内部原因；拒绝事务后仍落盘，诊断写入失败不改变业务结果，不增加对外失败码。

AF183/AF185：数据库10→11只增加本机派生向量缓存及索引控制/租约；迁移不嵌入、不重整理。删除/正文改动同事务清除所有 binding，向量写入不推进业务 revision/source_revision/W4 屏障，v10 写程序拒写 v11。该步当时 recall.v7 新增独立 retrieval 元数据；R4 v6 与 W5 回执 v8 不变。AF206 的模型/召回升版见现行契约，W5 回执格式仍不变。
