# R1 / R4 单阶段评测

复用 `tests/write_stage_eval.rs` 的录制、预检、请求绑定及离线回放。样本为已复核的 `tests/eval/r1/samples` 与 `tests/eval/r4/samples`，不修改样本；各20项，dev14/holdout6。默认 W 阶段选择不变。

零调用的干跑与回放：

```sh
EVAL_STAGES=r1,r4 cargo test --offline --test write_stage_eval dry_run_plan -- --exact --nocapture
EVAL_STAGES=r1,r4 cargo test --offline --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
```

EVAL_CASES、EVAL_REPEAT、EVAL_MODEL/EVAL_MODEL_LABEL、EVAL_BASE_URL 与 EVAL_MAX_OUTPUT_TOKENS 沿用写入侧；R 阶段评测缺省输出16384，实际上限参与录制绑定。生产缺省仍取显式预算中的 query/review 输出限额4096，评测不自动更改生产授权。未设置 EVAL_RECORD 时不创建客户端、不读取凭证、不发送请求。单次全量潜在40次，两次80次，三次120次；真实录制必须另行取得批准，再显式设置 EVAL_RECORD 的整数上限。整个所选计划超过上限时零发送。

`EVAL_REASONING_EFFORT` 覆盖此次全部 W/R 阶段，缺省 medium。接受 low/medium/high/minimal/none/xhigh/max；omit 表示省略 reasoning（供应商默认，非本地 medium 默认）。供应商是否接受这些值需另行真实验证。实际请求和指纹包含强度，变化不能复用录制。medium 保持现有 `recordings/<模型标签>/repeat-N/` 路径；其他强度为 `recordings/<模型标签>/reasoning-<强度>/repeat-N/`，不覆盖 medium。干跑同样使用模型、强度和重复次数；两个模型分别运行以下命令，各潜在80次（R1/R4 各40，各 dev28/holdout12），本轮零发送：

```sh
EVAL_REASONING_EFFORT=low EVAL_STAGES=r1,r4 EVAL_REPEAT=2 cargo test --offline --test write_stage_eval dry_run_plan -- --exact --nocapture
```

录制的 `elapsed_ms` 从调用发送器前计到完整响应读取/失败返回，包含 HTTP、超时及未完成响应；不包含评分或落盘时间。每条报告保留 elapsed_ms/usage，按阶段 metrics 给出毫秒 P50/P90/最大值（nearest-rank）及 output_tokens、reasoning_tokens 均值，每项附有效计数。失败调用有 usage 时纳入 token 均值，无 usage 不填零。既有录制没有计时字段，elapsed_ms 为 null、不参与百分位；不以文件写入间隔冒充实际延迟，不改变既有录制。回放复用原测量数据，零发送。

HTTP/未完成/无效响应均保留原始尝试、usage 与 incomplete_details.reason；不自动重试。回放完整绑定角色路由、实际请求、prompt/schema、时钟、时区、证据包、输出上限、地址和重复编号；没有匹配的录制报未录制/过期，不冒充模型失败。R 请求使用 SHA-256 绑定完整数据，旧 W 指纹不变。默认只出模型评分报告；EVAL_GATE=1 才因模型评分或调用失败报错，无效输入/损坏录制/非法配置始终失败。dev/holdout 分开汇总，必须同时报告。

`adapter.rs` 只读取 input：R1 目录转换为运行时 Entity/Relation，沿用生产纯 payload。R4 展开冻结 RecallEvidence 与 R1 计划；记录用通用 fact 的完整原句，不凭标签猜专用 value/限定语；raw 保留来源日及未整理状态，关系依据来自给定成员。输入未给 sequence，固定按记录后追加 raw 分配稳定顺序；未给的 MemoryRequest/history 不猜造。legacy 保留旧 Memory 形状及 gaps，不给它补 Candidate/来源。完整性中的待判定/缺口由 input 派生，截断标记原样保留。与生产共用 r4_payload，以及 r1/r4 的核验和计算代码；单阶段不运行模型 R1、检索模型或后台写入。

评分规则：

- R1 严格解码并运行 R1 核验；比较 need、current_evidence、type、subject、conditions、time、plan.op、filter.relation，以及标签明确给出的 unit。标量的标签数组是允许集合，结构数组无序一一匹配，嵌套的条件候选数组表示该片段的允许集合。Z8：conditions 要求期望为实际的子集，额外检索提示不扣分，必需条件缺失仍失败；time 标签为 null 时不做语义评分，标签非 null 时继续比较明确的 Quote/interp 字段。模型额外给出的条件/时间仍须通过全部结构、原文引用及时间换算核验，不能靠无效额外内容通过。
- R4 先严格解码、核对全部引用/限定语/记录 ID，并由代码执行计算；比较 verdict。标签 facts 每行要求唯一匹配：record_ids 集合相同，来源包含标签要求的来源（允许更新双方依据），必须属性逐项比较。标签未指定的附加事实与 statement 含义不做自动语义裁定，原始输出留人工检查；facts=[] 表示没有要求匹配的事实行，不额外创造“模型必须不返回任何事实”的规则。
- compute=null 要求实际为空；其余按标签核对 op、items 个数、operands 个数/端点与条目绑定、数量集合/单位、distinct_groups、coverage。expected_value 由生产代码运算核对，模型不得直接输出最终数值；insufficient_ok 允许空 compute 或有效但无法计算的粗精度计划，不能接受无效引用/日期。未规定的项目不猜标签。
- must_not 全部留人工，报告明确附带；不能把字段评分“通过”解释为完成了语义人工验收。尤其额外 facts、唯一当前值、statement 限定语、纠错与历史仍需复核。这是现有样本的评分覆盖边界，新增更严要求应先补经复核的标签。

自动回归注入全40项合法输出并逐字段制造失败，检查适配、评分、上限预检、录制重放、split及请求时钟指纹。提示词扫描在 `src/recall_models_tests.rs`，同时扫描 R1/R4/W1/W3/W4 样本的原文、上下文、样本ID与文字标签。
