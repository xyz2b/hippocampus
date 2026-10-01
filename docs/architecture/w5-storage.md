# W5 存储扩展：冲突关系与更新状态（草案）

W5 剩余三块（冲突关系、重判复用与归并、更新状态的召回）都要往存储里加东西。本文先把存储形态、迁移和兼容规则一次定下来，只升一次数据库版本（7 → 8），再按 w5-acceptance.md 分段实现。依据：contracts.md 1.6（UpdateState）、2.4（RelationLink）、2.5（W5）、3.2（RecordView）。

## 1. 放在哪里

两者都放进现有的 owner 快照（`PersistedMemoryState`），不新增表或存储框架（contracts 1.6、2.4、§7 “未解决关系的存储”）。

| 新增 | 位置 | 说明 |
| --- | --- | --- |
| 冲突关系 | `relations: BTreeMap<String, StoredRelation>` | 与 Memory 生命周期分开；旧快照缺省为空 |
| 关系编号 | `next_relation: u64` | `rel-<n>`，单调递增，不复用 |
| 每条记录的更新状态 | `record_updates: BTreeMap<MemoryId, UpdateState>` | 只记 W5 产生或重判过的记录；没有条目即“未知”（召回为 null，不当 judged） |
| 回执里的更新状态 | `W5Receipt.updates: BTreeMap<usize, UpdateState>` | 候选 → 其产物的更新状态；contracts 1.6 要求随决定回执保存 |
| 回执里的归并映射 | `W5Receipt.merged: BTreeMap<MemoryId, MemoryId>` | 已提交 ID → 保留目标（contracts 2.5）；本段只定义字段，第 4 段才写入 |

完整 history 不另存：W5 产生的替代链从回执的产物（products）与决定里的完整 targets 读出，归并链从 `merged` 读出；W5 之前的记录仍用现有 supersedes。读取时按 owner 扫描回执建索引，不新增表。第 1 段把以上字段全部定好，后续三段只写入、不再改存储形状，保证只升一次版本。

不把更新状态放进 `Memory`：`Memory` 的序列化形状同时是冻结的 LegacyMemory（contracts 3.2，`tests/w4_legacy_fixture.rs` 按原样回写校验），加字段会改动它。

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

conditions、times 先按 JSON 原样保存：当前代码还没有 TextQualifier、TimeInterp 的 Rust 类型，W5 也不需要解释它们；等 W1/W3 落地后再换成类型，那时是一次不改存储形态的替换。

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
  - update_relation_unclear：本次显式重判覆盖了这个缺口才清除，即 W4 输入包含该记录的前身（或相关旧成员）及其**最新**关系状态（open、resolved、invalidated，或像 w4_17 那样根本没有 StoredRelation 的普通新旧关系），并对该记录给出已定决定（共存、另建冲突、解决旧关系、普通替代均可）。不要求存在 open 关系，也不要求旧关系被关闭：更新判定完成不等于冲突解决（contracts 1.6）。关系被删除失效或被其他组解决只改变输入状态，本身不清除，要等下一次显式重判。
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
- 这改变了现有删除行为，放在第 3 段实现，验收至少包括：
  - w4_22 删除 s-501：111、N、s-502、s-510 保留，关系失效；
  - w4_22 按 memory_id 删除 N：110、111 都删除；删除 110：N 删除、111 保留；
  - 三代链按中间一代删除，三代全删；
  - w4_21 删除解决来源：关系变为 invalidated、不再算已解决，前身仍为 superseded；
  - 原陈述被删、只剩撤回证据的记录被删除；剩余来源角色无法确认的记录被删除，其他原文保留；
  - W5 之前创建、升级后经 W5 或旧回执确认强化的记录，删除创建原文后保留；
  - 强化过的记录删除一个强化来源后保留，计数按独立来源重算（同一来源多个回执只算一次），最近强化时间同步。
- 关系不因成员进入历史而自动关闭；只有 W4 决定或用户确认能解决。

## 5. 事务与过期判断

- `SqliteMemoryStore::transaction` 现在只用（记录、实体、暂存）判断“业务有变化”。关系和 `record_updates` 的变化也是业务变化：要一并纳入，才能推进 revision、触发现有的顺序屏障。
- 组的 `seen` 从“目标记录指纹”扩展为“目标记录 + 涉及的关系”的指纹；涉及关系 = 决定要解决的关系 + 目标及既有冲突成员所在的 open 关系。
- 只比对已见关系各自的指纹不够：W4 之后若目标新加入了一条 open 关系，逐条比对发现不了。因此每个目标的指纹还要覆盖“它所在的 open 关系编号集合”；集合有增减（新建、解决、失效）即整组 needs_rejudge。

## 6. 版本 8 与兼容

已发布、可能存在数据库的版本：

| 版本号 | 写入程序 | 回执特征 |
| --- | --- | --- |
| 5 | W5 之前 | 无回执 |
| 6 | bf83fd9 | 无 status、无 legacy_fingerprint，v6 形态指纹 |
| 6 | baa1ff9 | 有 status、无 legacy_fingerprint，当前形态指纹 |
| 7 | 708b2d2 | 同 baa1ff9 |
| 7 | 88eb857 / ca16b64 | 有 status、有 legacy_fingerprint |

规则：

- 新程序读 1–8；迁移到 8 时快照补 `relations = {}`、`next_relation = 0`、`record_updates = {}`，回执缺 `updates` 时为空（含义“未知”，不是 judged）。迁移只补缺省，不推断旧记录的更新状态。
- 版本 7 的程序（88eb857、ca16b64）拒读 8：它们改写快照时会丢掉关系和更新状态，冲突会消失、pending 会变成未知。
- 回执的旧格式比对规则（ca16b64）保持不变。
- 组新增 `scope_complete`、`seen` 新增关系部分后，组指纹形态会变。不能靠“缺 `updates` 字段”识别旧回执：迁移补上 `updates = {}` 并保存后，这个依据就没了。改为迁移时持久写入格式标记 `form`：已有回执写 `pre8`，新回执写 `v8`；记录指纹是 Memory 的 JSON，无法从字符串可靠反推形态，所以不靠形态推断。
- `seen` 的新部分与原记录指纹分开存放：`W5Seen { id, fingerprint, relations }`（目标所在 open 关系编号集合）以及组的 `seen_relations: [{id, fingerprint}]`；都为空时不序列化。`pre8` 回执重放时，按旧投影比对：去掉 `scope_complete`、`W5Seen.relations` 和 `seen_relations`，再叠加现有 `legacy_fingerprint` 的 v6 规则。`v8` 回执只按当前形态比对。
- 测试必须包含“迁移 → 关闭 → 重开 → 重放”，以及重开后 `form` 仍为 `pre8`。
- 兼容测试按上表逐一构造各版本程序实际写出的数据（不是从新格式删字段近似），每种都验证：能打开、迁移后字段齐全、迁移保存后重开仍能识别旧格式、同组重放结果不变、旧程序拒读新版本。

## 7. 分段

1. **存储与迁移**（本文）：加字段（含回执 `updates`、`merged`、`form`，组的 `scope_complete`、`seen_relations` 与 `W5Seen.relations`）、版本 8、事务业务快照纳入新字段、兼容测试。不改提交行为。
   - 组与决定上后续各段要用的字段也在这一段定好，避免以后再改回执里保存的形状：决定加 `resolves_relations`、`resolution`、`relation`（conflict 要建立的关系）、`pending_reason`，动作加 `Conflict`，候选加 `committed_id`。都在为空时不序列化，旧组的指纹不变。执行尚未实现前，携带这些字段的组一律拒绝，不静默忽略。
   - W5 的存储类型（回执、组、决定、关系、更新状态）拒绝未知字段：将来的程序若再加字段，当前程序读到时报错，而不是改写快照时悄悄丢掉。
   - 兼容测试用的旧数据是各提交自己的程序写出的数据库，导出为 `tests/fixtures/w5_storage/*.sql`（模式 + 行 + user_version；仓库忽略 `*.sqlite3`，故存文本）。
2. **更新状态**：W5 按第 3 节生成并写入；召回的 RecordView/RecallFact 带出 update；`pending_updates` 计数。
3. **冲突关系**：建立、解决、替代未解决成员、删除失效；w4_21（D1）、w4_22（E1–E3）。
4. **重判复用与归并**：B2、B3、B3a，以及第 3 节的状态改写规则。

## 8. 已定（原待定项）

- `scope_complete`：W5 组直接携带运行时代码为 W4 输入生成的值，并纳入组指纹；W5 只核验快照，不重新判断检索范围。
- 关系编号：owner 内唯一即可，编号分配与关系写入在同一 T2。
