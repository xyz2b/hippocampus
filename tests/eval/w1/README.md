# W1 / W3 样本（w1.v1、w3.v1）

各 20 个，由 Claude 起草；用户于 2026-10-02 抽查并确认全部待定标注（w1_04、w1_13、w1_14、w1_17、w3_02、w3_12、w3_19），`label_status = reviewed`。用途：评价 W1 提取、W3 核验提示词在真实模型上的表现（真实调用须用户批准）。模拟服务测试不依赖它们。

- `tests/eval/w1/samples/`：输入为原文、历史、实体与关系目录、said_on；`expect.candidates` 只列需要核对的字段，未列字段不评分；字段值为数组时表示可接受集合；`candidate_count` 为输出候选的确切数量；`must_not` 为禁止行为。
- `tests/eval/w3/samples/`：输入为 W2 已通过的候选（部分故意带错，简写形式：Quote 用字符串表示，评测适配器负责展开成 w1.v1 结构）；`expect.reviews[].aspects` 是必须指出的问题方面集合。
- 标注中“待用户裁定”“由人工复核”的项目写在各样本的 `note`。

## AF150 / AF151 标注修订（2026-10-08）

修订样本用 `label_revision` 记录决定编号与日期；输入、ID、提示词和请求指纹不变。
W1 05 按带条件及不确定性的 plan 评分；08 的撤回值接受 null 或“字节”；14 保留
两项必需候选，`optional_candidates` 仅允许额外的 works_at=“一家银行”。任意其他
第三项或第四项失败。生成器保留上述规则，可重复生成 JSON。

`expect.alternatives` 是完整期望对象的数组，整支全部通过才通过，不能跨分支拼接字段。
当前只支持一项可选候选，评分器将它展开为两条完整分支；数量、字段、请求与机械核验
都在每条分支中检查。标量 `verdict` 可使用允许集合，但不会豁免 schema、decode、W2/W3
核验失败。

W3 02、03、04、05、06、08、09、10、16、20 的 `coverage.scored=false`：complete/gaps
均接受，gaps 仍须提供非空、逐字连续原文引用，05 的引用还须包含母亲的断言。该标记
不免除机械核验。报告将候选及请求的 verdict 评分与 coverage 观察、评分、引用检查分列。
W3 16 保留 episode_value=low_value：遗漏时间与这条日常状态的记忆价值是两个独立问题。
W3 12 的 complete 期望保持原样；其他未裁定标注不变。

零调用重评命令及旧报告保留方式见
[阶段评测说明](../write_stage/README.md#af150--af151-离线重评2026-10-08)。

## AF154 / AF155 标注修订（2026-10-08）

四个样本的 `label_revision` 更新为 AF154/AF155，输入与 ID 保持不变：

- W1 12 的 memory_type 接受 fact 或 episode；主体、日期精度与请求等其他期望不变。
- W3 12 覆盖此前 complete 裁定，接受 complete/gaps 两个完整分支；两候选均须
  supported。gaps 必须非空、逐字连续引用原文且含“一家银行”，不能跨分支拼配。
- W3 10 本轮临时接受 contradicted/insufficient[value]，supported 仍失败。报告保留
  两种 verdict 的分布；它们在生产的 rejected/pending 语义不等价，长期按 insufficient
  收窄提示词另立待办。
- W3 19 标记 `observation_only=true`，原因是“频率限定语缺少明确映射，待契约决定”。
  保留原始判断和失败字段，排除主语义通过率与分层；有效输出记“观察”，不计通过。
  decode、schema、W2/W3 核验或引用失败仍记“失败”，不会被观察标记豁免。

AF137 仍为 40 样本、520 槽；其中 39 个计分样本、1 个观察样本（13 个观察槽）。
请求指纹与账本身份不变，旧报告保留。重评说明见
[AF155 离线重评](../write_stage/README.md#af154--af155-离线重评2026-10-08)。

```
python3 tests/eval/w1/gen_samples.py
python3 tests/eval/w3/gen_samples.py
python3 tests/eval/w1/check_samples.py   # 同时检查 w1 与 w3
```

单阶段评测入口覆盖 W1 / W3 / W4，直接共用生产 `structured_request`、提示词、schema 与核验函数。W3 简写先展开并通过 W2；W4 仅选择 `call_w4=true`。每个待录制样本只发送一次，不重试、不跟随重定向。

默认离线回放（无需密钥，不创建 HTTP 客户端）；逐样本逐字段的 JSON 报告打印到标准输出：

```sh
cargo test --offline --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
EVAL_STAGES=w3 EVAL_CASES=w3_0 cargo test --offline --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
```

`EVAL_STAGES` 接受 `w1,w3,w4` 的子集；`EVAL_CASES` 是 ID 前缀，也可用逗号选择多个前缀。过滤只作用于上述评测入口与干跑，注入回归测试始终完整运行。

干跑不发送请求、不读取密钥：

```sh
cargo test --offline --test write_stage_eval dry_run_plan -- --exact --nocapture
```

全部样本的潜在调用数为 W1 20、W3 20、W4 21，共 61。真实录制须用户另行批准并提前报告实际需要录制的次数；**以下命令只用于获批后**。先在环境中配置 `HUOSHAN_DEEPSEEK_BASE_URL`（API base，例如以 `/v1` 结尾）与 `HUOSHAN_DEEPSEEK_API_KEY`，再执行：

```sh
EVAL_RECORD=61 cargo test --offline --test write_stage_eval write_stage_eval_suite -- --exact --nocapture
```

`EVAL_RECORD` 是本次发送尝试的整数上限。发送前打印所选、需录制、计划发送与上限；超过上限整批拒绝，零发送。有效录制直接复用，只补未录制或过期项。HTTP 错误、未完成和拒绝响应均不录制、不自动重试；已完成但解码失败的原输出仍录制，以便离线复核。报告记录实际发送尝试数，录制保留 usage，不等同于计费次数。

录制保存于 `tests/eval/write_stage/recordings/<sample_id>.json` 的 `recorded` 数组，通过 `model` 标签分组；更新当前模型时保留其他模型。保留解析后的 `output` 和原始 `output_text`，回放优先使用原始文本，保证解码失败不会因二次解析变成通过。当前 `MODEL=DeepSeek-V4.1-Flash`，与 `tests/eval.rs` 一致；换模型时须同步标签与生产 `DEEPSEEK_MODEL`。

请求指纹沿用原评测的 canonical FNV-1a：包含提示词、schema、模型和固定样本输入，排除输出 token 上限及每次运行的时钟/绑定字段。当前模型的指纹缺失或不符记为“录制过期”，不使用旧输出打分；无当前模型录制记为“未录制”。两者单列、不算模型失败，默认离线测试不会因此失败。有效录制的评分失败、输入无效或录制调用失败会使评测入口失败。

报告的 `total` 是所选样本数，`evaluated` 是实际打分数；同时列出 passed、failed、missing、stale、draft。全为未录制时不代表模型通过。`must_not` 与 `note` 原样保留，供人工复核；不自动解释自然语言。标量数组按允许集合核对，条件/时间/索引等结构数组完整匹配、不评分顺序；W3 aspects 与 coverage missing、W4 basis_sources 按要求覆盖。W4 的 `expect.after` 属于提交与召回验收，本脚本不评分。样本没有时区与完整目录类型约束，评测固定为生产默认时区 UTC+8、关系允许四种 memory_type，不宣称验证实际账户的目录限制。

### AF311 标签校准（零调用）

AF311 仅修订 W1/W3 期望、说明与标签版本，输入/ID 不变，模型请求仍使用当前 W1/W3 v4。
通用 fact 接受经机械核验的连续断言片段，sentence 仍保留完整原句；不放宽主体、限定语或引用校验。
W1_11 新增“零候选 + source retention”的完整分支，空 indices 不代表全部旧事实。
W1_05 的条件“以后”可不另列时间；若列，须为 event/unknown 并保留引用。
W1_06 保持 fact；W3_07 只接受 contradicted[speech]；W3_01/14 为 contradicted[time]，
W3_05 为 insufficient[subject]。分支须整支通过，W3_19 仍仅观察。

检查：`python3 tests/eval/w1/check_samples.py w1 w3`；生成器与样本应保持一致。
AF302 repeat-1 离线重评另存 `target/eval-providers/compare-af302/reports-af311/`，
读取匹配原请求指纹的最后一次录制，保留机械核验，不发送请求。
标签不进入请求指纹；AF302 manifest 另冻结标签摘要，因此旧录制入口会拒绝新标签下续录。
本轮不改 manifest/额度/账本，离线重评不代表重新授权续录。

### AF317：断言窗口、核心与生日时间

通用 fact 的结构性期望使用
`"value":{"window":"我打算下个月开始学日语。","required":["学日语"]}`：
窗口必须在原文中唯一出现，值必须是窗口内唯一可定位的连续片段并包含全部核心。
生产 W2 的 sentence 包含、text/quote 相等检查仍执行；主体、极性、限定语与完整分支不放宽。
“日语”“狗”等裸名词不能代替“学日语”“特别怕狗”。核心由人工标签指定，不从模型输出推导。
专用关系不适用；w1_17 的 owns 对象分支与 fact 断言分支分开，防止串配。
本次仅改 AF315 涉及的结构性标签；w1_11 手机号值沿用 AF311 已裁定分支。

时间仅允许显式标签规则：w1_11 deadline.quote 的 window=“只保留到明天”、required=[“明天”]。
不接受窗口外引用；role/interp 不变。窗口重复、空核心、核心不在窗口均属标签错误。

w1_10 value 必须同时保留生日属性和“3月12日”。完整分支接受：
- times=[]：月日作为生日属性；
- times=[event/recurring、quote=“3月12日”]：周期生日。
不得凭空推算年份、将生日解释成有限有效期或留存期限。仅日期值不能通过任何分支。

`python3 tests/eval/w1/check_samples.py w1 w3` 检查标签与原文；
`cargo test --test write_stage_eval af317` 覆盖片段、时间窗口与字段保护。
