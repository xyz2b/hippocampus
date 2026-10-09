# AF173 检索基线

目的：在真实 SQLite 上量化现有 R2/R3 的检索缺口，为后续混合检索提供冻结对照。固定注入 R1，不运行 W1–W4/R4，不接 provider，不评价 R1 理解能力或最终语义回答。

```sh
PMR_MODEL_NETWORK=deny cargo build --tests
PMR_MODEL_NETWORK=deny cargo test --test retrieval_eval -- --nocapture
```

输出覆盖写入 `target/eval-retrieval/af173/report.json` 与 `report.md`，不写入历史评测目录。数据库在系统临时目录创建，结束后删除。

## 冻结数据

- `corpus.json`：owner A/B、实体目录、完整授权来源、完整 Memory、ContractCandidate、W5 v8 提交回执（规范 JSON 组内容指纹）、record_updates、版本链、预设 open 关系、删除列表及填充规则。
- `queries.json`：34 道检索题、6 道控制题；完整 r1.v2 输出、可信 owner/subject_ids/task、required/relevant/forbidden、路径检查、字节预算及控制来源。缺省可信主体范围为目录内全部，不是 R1 的主体判断。每份正常 R1 输出必须经过 decode → validate_detailed；无效普通计划使测试失败。
- F0001–F0100 全量冻结，原文为 `我的陶片标记为 F{number:04d}。`，self/fact/整句 value，各有独立来源。不会按查询避开词面；共有“我的”等弱命中正是排序压力的一部分。
- 所有 ID、接收序号、原陈述日和查询时钟固定。canonical 与 reverse 逐条以相反顺序插入来源，改变 SQLite/FTS 的物理行序，保持语义时间不变。两种顺序的结果、排序和完整性必须一致。
- T20 与 T21 有独立旧来源，分别被 T04 与 T02 替换；StateChanged、双向链，结束时间未知，不冒充 Correction。版本前身随链带出属于 relevant。
- T14 使用 fact + speech=plan，保留“下个月”的月精度事件时间。T22 保留 uncertainty=“好像”。T17 的值为原文连续引用“团子”。D04 改为本人乘坐经历；D06 为一般干扰；D07 只与编辑器查询共有“写程序”，断言不同。
- T22/T23 的 open 关系是人为预设的关系闭包夹具，代码不推断两句是否真的冲突；关系成员均计 relevant。
- T28 经 `ChatMemoryService.memory_forget(source_id)` 真正删除，核验来源正文已清除、记录不可投影、索引无残留。T30 在独立 owner B 中。

构库后和删除后均核验：完整 CurrentRecordView2/committed、候选经 W2、完整候选投影与回执一致、所有依据精确可定位、时间/限定语未丢失、更新状态、版本链、关系成员、来源索引版本与正文、FTS5 external-content integrity-check。构造错误不会被误算成检索低分。

## 查询与控制

Q01 required=T01、relevant 另含 T24；Q09 required=T09、relevant 另含 T24/T22/T23。Q27/Q27b 原问题完全一致，只改变可信 task。Q29/Q29b 问题相同，只改变 R1 关系提示；Q31 的错误 fact 提示只报告缺口，不以必需命中作断言。Q32 覆盖 T16 的代码示例语言。

R1 顶层 relation 是问题文字，不含答案提示；未提供过滤时 `plan.filter.relation=null`。Q23 的主体为原文引用 `unresolved: {text:"谁"}`，不是 null。主体注入类只测试 R2/R3，不声称测过 R1 指代解析。

| 控制 | 内容 | 硬检查 |
|---|---|---|
| C01 | 词法不相关的 queued 原文 | 可信范围内仍进入 raw |
| C02 | `subject_ids=[]` | 空授权范围，不返回记录/原文，不解释为全目录 |
| C03 | R1 decode 失败 | 去掉模型过滤，按可信范围枚举 |
| C04 | R1 catalogue 核验失败 | 同上，记录失败原因 |
| C05 | R1 未调用 | 同上，不伪造合法计划 |
| C06 | 1 字节预算、含 queued 原文 | 裁剪记录和 raw，retrieval_truncated=true/plan_complete=false |

每题运行 records=64、8；其他预算固定为 raw=32、relations=64、stored_bytes=262144（C06=1）。共 40×2×2=160 次组装。关系过滤与小范围可能免于穷举/FTS：不能仅根据总库大于64就宣称每题都调用 FTS。

## 指标与失败门槛

- `required_all`：必需记录全部进入最终 R3 records；空 required 为逻辑真，但不计入全覆盖率分母。
- recall：最终命中 required 数 / required 数；MRR：第一个 required 的倒数排名。空 required 均为 null。
- precision：最终 records 中 relevant 的数量 / 返回数量（版本链、关系成员不扣分）。没有返回为 null；无 relevant 但有返回为0。raw 和关系依据不充当记录命中。
- R3 原文覆盖：必需来源的完整正文是否由 records/raw/关系依据中的某一连续引用覆盖；分母为去重后的 required 记录来源 + required_raw。没有必需来源为 null。只拿到片段不算完整原文。
- 完整性直接报告生产 RecallCompleteness，不据标注改写 plan_complete。
- JSON 保留每阶段集合、实体/短语命中数、接收序号、排序、选择机制（穷举/关系/FTS/字面）、数量/字节裁剪、FTS 是否调用、关系是否撤销、遗漏最早阶段和耗时。Markdown 提供分类汇总及全部逐题结果。
- 遗漏原因只依据集合差异，不由代码判断语义：owner、trusted_scope、r1_subject、relation_filter、lexical_no_hit、projection、record_count_budget、record_byte_budget。
- 耗时为本机 R2/R3 单次组装微秒，不包括构库/R1 校验/重开/R4，不用于生产延迟承诺。

只有构造错误、冻结路径不符、删除/owner/task/可信主体越界、原文引用失真、预算违规、诊断改变证据/模型输入、插入顺序改变结果或出现模型事件使测试失败。低 recall/precision、Q31 错误提示仍生效等质量缺口如实报告，不放宽标注或改排名来通过。

这套全部冻结为后续验收集。向量模型选择、融合权重与阈值调参须另起合成集，不能用本集调参。

## 本次运行与成本

2026-10-08 本机 debug 基线：160 次组装及构库/完整性核验共 342.83 秒。单次组装 p50 约1.29秒、p95约3.24秒（与全套测试有并行竞争，不能视为稳定性能基准）。默认测试运行会增加数分钟；本步保留完整真实回执读取，不为提速改生产实现。

records=64 与8均为20/32道非空 required 题全覆盖；required 命中21/33、宏平均 recall=62.5%。两种插入顺序完全一致。遗漏为10道 lexical_no_hit、1道 record_count_budget（Q19）、1道 relation_filter（Q31）。这些数字是冻结输入上的检索结果，不是最终召回答案分。

## AF175：本地嵌入实验（离线模拟）

这一实验复用冻结输入，对照现状、vector-only、hybrid；**不是生产 R2**，不改变召回配置、数据库或契约。只嵌入获准保留的来源正文分段，分段直接复用 `lexical::segments`，不嵌入 W1/W4 摘要。查询只发送问题文本；标注、R1 注入计划、样本 ID 不发送给嵌入模型。

固定模型为 `qwen3-embedding:0.6b` 和 `bge-m3`（接受 Ollama 的 `bge-m3:latest` 返回名）。二者要求 1024 维。按照 [Qwen 官方模型卡](https://huggingface.co/Qwen/Qwen3-Embedding-0.6B)，Qwen 查询固定为 `Instruct: Given a personal memory query, retrieve relevant passages that answer the query\nQuery:{query}`，文档为原片段；[BGE-M3 官方模型卡](https://huggingface.co/BAAI/bge-m3) 的 dense 查询与文档均不加指令。

客户端只允许 `http(s)://127.0.0.1[:port]/` 或 `localhost` 根地址，默认 `http://127.0.0.1:11434`。禁代理、重定向、自动重试；localhost 固定解析为 127.0.0.1，不自动转 IPv6、云端或备用模型。`PMR_MODEL_NETWORK=deny` 禁止所有本机 HTTP；缺省 offline 只允许上述本机地址，live 也不扩大本实验的端点范围。metadata 中的 remote_host/remote_model 非空字段会被拒绝。

`/api/tags` 权重 digest、`/api/show` 本地 embedding 能力和维度必须可核对；否则拒绝发送。每次嵌入前后再次核对 digest。digest、标签、维度、查询/文档格式、分段版本、请求内容以及来源/owner/task/字节偏移都绑定录制。按 [Ollama embed API](https://docs.ollama.com/api/embed) 显式发送 `truncate=false`，固定批量最多16、不静默截断；校验响应数量/顺序位置、模型、维度、有限数值、非零范数和有界响应（8 MiB），再以 f64 范数归一化为 f32。数量/位置对应遵循 API 数组顺序，代码不判断向量语义。usage 缺失记 null，不猜 token 数。

### 干跑、录制、回放

先产生当前 AF173 对照报告（新增的输入指纹用于防止误用旧报告，指标和评分不变）：

```sh
PMR_MODEL_NETWORK=deny cargo test --test retrieval_eval af173_retrieval_baseline -- --exact --nocapture
```

**只干跑，不访问 Ollama，不创建录制或账本**：

```sh
PMR_MODEL_NETWORK=deny RETRIEVAL_EMBED_MODE=dry-run cargo test --test retrieval_eval af175::af175_local_embedding_experiment -- --exact --nocapture
```

当前冻结语料：137 个正文片段、34 个去重问题；每模型9次文档批次 +3次问题批次 +2次同问题首次/暖探针 = **14次嵌入**，两模型合计 **28次**。每模型最多30次元数据 HTTP（初始 tags/show 两次、每次嵌入前后 tags）；完整成功录制共最多88次本机 HTTP，其中28次为嵌入。仅指纹元数据查询不占嵌入额度。干跑的 digest 为未解析，不能声称已检查模型安装/权重/维度；请求数是全新完整 campaign 上限，续跑不会重新分配已占槽位。

**下列录制命令会实际使用本机模型，仅在另行获准后执行；本次实现未执行**：

```sh
PMR_MODEL_NETWORK=offline RETRIEVAL_EMBED_MODE=record RETRIEVAL_EMBED_LIMIT=28 cargo test --test retrieval_eval af175::af175_local_embedding_experiment -- --exact --nocapture
```

可用 `RETRIEVAL_EMBED_BASE_URL=http://127.0.0.1:11434` 指定本机服务。每模型在 `target/eval-retrieval/af175/recordings/<model>/` 冻结14槽；发送前原子落盘并 fsync 已占槽，逐次持久化规范化向量、严格请求指纹、结果摘要、usage 与 elapsed/load/total duration。录制成功后退出再续跑直接复用；已占槽但无录制只标记 `unrecorded_inflight`、不重发，不恢复额度。有失败录制不重试，并停止该模型余下新请求；向量缺失时不计算 vector/hybrid 分数。输入/权重/格式变化使 campaign 或录制失效，拒绝自动重置额度。并发锁残留必须人工核查，工具不自动解锁。

**严格零调用回放**（不构造 HTTP 客户端）：

```sh
PMR_MODEL_NETWORK=deny RETRIEVAL_EMBED_MODE=replay cargo test --test retrieval_eval af175::af175_local_embedding_experiment -- --exact --nocapture
```

回放只认证录制时的 digest，不查询当前模型是否已变。录制报告保存到 `reports-record/`；当前 AF178 回放报告保存到 `reports-af178/` 的 `report.json` / `report.md`，不覆盖 AF173、AF177 或历史模型评测报告。

### 模拟规则与报告限制

- 向量扫描先按 owner、任务、可信主体及经核验的 R1 主体过滤活来源和记录，**不使用关系过滤**；删除来源即使遗留向量也不得成为候选。只按记录候选原句与正文片段的字节区间相交进行映射，多片段取最大余弦分数，按记录去重。未整理 raw 继续遵循现有可见性，不靠向量命中才能出现。
- hybrid 使用 AF173 诊断中预算前的词法 `ranked` 和上述向量排序，两路各取 `2×records` 个记录种子、等权 RRF(60)，相同分数按记录 ID 排序。随后补范围内的历史/关系闭包，按分数裁剪 records 和既有序列化字节预算。小范围穷举、聚合/list、R1 失败枚举等直接沿用现状；向量不认证 plan_complete。
- 固定数据仅一个 open 关系、控制 raw 至多一个；没有超过现有 raw=32 / relations=64 的数量预算。向量模拟按同样的 records→raw→relations 字节顺序；C06 的1字节裁剪控制直接沿用真实生产组装结果。模拟不取代生产 R3 核验，也不支持在本集之外泛化裁剪实现。
- 报告按题×64/8×模式×模型给出 required 全覆盖、recall、MRR、precision、返回记录数、完整连续原文覆盖、完整性、种子/闭包/返回诊断与遗漏阶段；所有安全违规硬失败，低分如实报告。现状沿用 AF173 已验证一致的 canonical 插入顺序。
- 首次/暖嵌入探针为同一固定问题连续两个请求，记录 API load_duration 与客户端墙钟耗时（包含嵌入前后 digest 核对）；模型若原已驻留，不能称为真正冷启动。查询扫描另记 first/warm 微秒。记录向量载荷字节；Ollama 进程峰值内存**未测**，不以向量载荷冒充整个模型内存。
- 本集全部冻结，不用它调查询指令、融合权重或阈值；更换格式/模型须获准另起 campaign。所有真实检索得分与实际冷/暖延迟须待获准录制后再测；本次仅通过假响应/假向量测试，不报告真实向量效果。

## AF178：固定 DF 截断与严格零调用重评

词法 DF 阈值固定为测试常量 `LEXICAL_DF_PERCENT=20`，**严格超过20%**的查询词元不计分，等于20%仍保留；没有在本验收集上调参。词元复制当前 `cjk-position-v1` 的纯处理规则：中文相邻字对、字母数字整词，以及查询明确出现的登记实体别名。测试将编码与真实 SQLite `source_segments.index_text` 对照，完整回放还逐项对照截断前的生产实体/短语得分；不添加停用词、分词库或语义推断。

DF 的一份文档是一条活的、具有范围内记录的来源正文，同一来源的多个记录、版本或重叠分段只计一次，重复出现词元也只计一次。计数范围是 owner、任务、**宿主传入的可信主体范围**，先于 R1 主体/关系检索提示；不让模型提示改变 DF 分母。来源已删除、越权或任务外时，不参与计数或候选。无记录的 raw 不参与这个记录通道的 DF，原话可见性继续由原组装流程保障。

五种模式仅离线模拟：

- `current`：冻结 AF173 结果。
- `lexical-df`：保留现状的关系过滤/关系种子/枚举，用截断后的实体命中数、短语命中数、接收顺序、记录 ID 排名；旧 FTS 命中还须有保留词元在来源中真实命中。不是把关系种子改成必须词法命中。
- `vector-only`：原向量模式，不变。
- `hybrid`：AF177 旧融合，只有真实 FTS/实体/短语命中才投词法票，不变。
- `hybrid-df`：同一 RRF(60)，词法票只计截断后仍真实命中的记录；仅关系/闭包成员与仅高 DF 弱词命中不投票。两路各取 `2×records`，闭包、数量/字节预算和安全门槛保持不变。

小范围穷举、聚合/list、R1 失败枚举直接使用原组装结果。模式不认证语义或扩大 completeness，生产 R2 未修改。

`serde_json` 的 **dev-dependency** 启用 `float_roundtrip`：修复录制中 `elapsed_ms` 等数值解析后差1 ULP造成的摘要误报。请求指纹、结果摘要仍逐字节严格比较，不使用容差、不重写录制；差1 ULP的实际篡改仍拒绝。默认回放不再需要额外 `--features`：

```sh
PMR_MODEL_NETWORK=deny RETRIEVAL_EMBED_MODE=replay cargo test --test retrieval_eval af175::af175_local_embedding_experiment -- --exact --nocapture
```

`replay-af178` 是同义入口，结果也写 `target/eval-retrieval/af175/reports-af178/`。两模型全部28条录制严格复用，发送数为0；缺失、过期或损坏即失败，不自动补请求。旧录制、账本和 `reports-record/`、`reports-af177/` 不变。

本次固定阈值结果（32道非空 required；precision 按非空结果宏平均，报告列出分母）：

| 模式 | records | Qwen 全覆盖 / MRR / precision | BGE 全覆盖 / MRR / precision |
|---|---|---|---|
| current | 8 | 20/32 / .5417 / .6289 | 20/32 / .5417 / .6289 |
| current | 64 | 20/32 / .5417 / .6255 | 20/32 / .5417 / .6255 |
| lexical-df | 8、64 | 21/32 / .5729 / .6935 | 21/32 / .5729 / .6935 |
| vector-only | 8 | 32/32 / .9167 / .2831 | 32/32 / .9167 / .2831 |
| vector-only | 64 | 32/32 / .9167 / .1512 | 32/32 / .9167 / .1512 |
| hybrid（旧） | 8 | 32/32 / .8438 / .2831 | 32/32 / .8646 / .2831 |
| hybrid（旧） | 64 | 30/32 / .7688 / .1503 | 31/32 / .7687 / .1507 |
| hybrid-df | 8 | 32/32 / .8594 / .2831 | 32/32 / .8698 / .2831 |
| hybrid-df | 64 | 32/32 / .8438 / .1512 | 32/32 / .8385 / .1512 |

Q19 两预算下由现状未覆盖变为 lexical-df 全覆盖，目标排第一；hybrid-df 修复大预算下 Q03/Q06 的遗漏。字面/精确题 Q12、Q15、Q22、Q23 的全覆盖、MRR、precision 相对各自旧模式均无退化。完整 JSON/Markdown 包含800行（40题×2预算×5模式×2模型）、逐题 DF/投票及预算诊断；安全违规0。vector-only 的 MRR 仍高于 hybrid-df，不能凭全覆盖宣布融合更优。

限制：高 DF 词也可能承载有效内容，特别是宿主显式限定的小主体范围；只在这一冻结集验证，未将阈值用于生产。现状错误关系过滤（Q31）和无字面命中的同义题仍不能只靠 DF 修复。本轮不添加 BM25，也不调整 RRF 参数。


## AF185：生产混合检索严格离线对照

生产 DF 已在 `src/lexical.rs` 统一实现；AF178 不再复制分词/计分。AF175 历史模拟及录制指纹保留；AF185 使用真实 R2/R3 与 DB v11 向量缓存，注入严格复用的查询向量，不连接 Ollama。旧实验报告/录制不覆盖，新 JSON/Markdown 写 `target/eval-retrieval/af185/`。

```sh
PMR_MODEL_NETWORK=offline cargo test --release --test retrieval_eval af185_production_strict_vector_replay -- --ignored --nocapture
PMR_MODEL_NETWORK=offline cargo test --test retrieval_eval af185_ -- --nocapture
```

第一条需要本机已有完整 AF175 录制，显式执行，缺失/指纹变化直接失败、零补发。两模型×两预算在生产路径计算必需全覆盖/MRR/precision、来源原文覆盖、截断及 retrieval 覆盖，保留实际枚举路径；逐题验证 owner/主体/任务/删除硬门槛。每题 opt-out 与未接入路径的 R4 输入逐字节比较。目标必需全覆盖32/32；指标如实报告，不修改冻结标签。第二条覆盖部分/损坏缓存、重开、查询失败及删除并发，不依赖网络。

生产冻结融合：DF 严格>20%、RRF60、两路各2×records种子；关系成员没有词法票或保留席位，融合后展开闭包再裁剪；plan_complete 只来自实际枚举。实验向量录制只认证本冻结合成语料的检索质量，不证明真实语义模型/生产 P95。

AF185 本轮 release 严格回放约96秒（两模型160次生产组装及80题预算的关闭模式对照）；debug 的重复完整投影明显较慢，建议用上述 release 命令。不是实际嵌入/R1/R4 网络延迟，不能用来宣称前台 P95。
