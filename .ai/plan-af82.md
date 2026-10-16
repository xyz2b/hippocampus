# Claude 修复方案（草案，待 Codex 复核）— 依据 .ai/analysis-af81.md 与 Codex 评审 22

不改冻结契约、不改整批拒收政策、DB/协议不升版；提示词只写规则不放例句，不用评测或用户私有原文。

1. **补齐非空 schema（实现缺口）**：共享 schema helper（src/write_models.rs:726 一带及 R1/R4/W3/W4 同类构造）对契约要求非空的字符串字段补 `pattern`/`minLength`（至少 unresolved.text、各 quote.text 等），使提供者端约束与 decode 的递归非空检查一致。实际请求 schema 进入指纹；旧录制过期如实报告。
2. **诊断补全**：
   - W1 decode 失败：记录白名单 field/rule（如 `candidates[0].subject.unresolved.text` / nonblank），不含原文。
   - R1 validate 返回降级计划时：轨迹不再只记 completed；区分“函数执行成功”与“计划有效”，记录降级的 field/rule。
3. **宿主 history**：工具描述与宿主说明——prepare 的 history 带上当前消息所指代、理解所必需的前文 USER 消息（不限于上一条；不带无关内容）。
4. **提示词**：
   - R1：quote 逐字复制原文连续片段（含 Markdown/符号）；time.quote 只能来自消息或 USER context，不能抄输入元数据（said_on 等）；第一人称指用户本人 self。
   - R4：interp 必须与 quote 中的明确日历一致；改成 unknown 不能用来掩盖明确矛盾。
   - W1：识别断言的持有者/行为者；计划持有者可依据明确的前文绑定，缺依据保持 unresolved 并逐字填入原文中的指代短语（永不为空）；不按事物词或计划关键词默认 self。
5. **不做**：逐候选 decode 拒收（改冻结 2.2，且本例两候选都非法也救不回）；自动重发私有原话；文本去重。

验证：零调用——schema 实际含非空约束（请求快照/指纹）；W1/R1 诊断落盘；提示词扫描；全套 0 失败；59/4 不变。真实效果由用户再次试用。
