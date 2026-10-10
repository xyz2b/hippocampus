# 架构文档

M1.5 收尾（2026-10-07）：默认写入 `w4_runtime_v1`、召回 `recall_v3`，旧写入/召回执行器已删除。当前 DB v11；旧数据通过 LegacyRecordView 读取，保留原值、Scope、条件、时间、证据和缺口；不自动调用模型重建。配置名中的 v1/v3 不是模型契约或宿主协议版本。

当前契约为 W1/W3 v4、W4 v5、R1 v2、R4 v7、recall v8。缺省角色强度为 W1 low、W3 low、W4 medium、R1/R4 minimal、R4 计算类 low；显式配置仍生效。日常使用的 [M1.5 已知局限](../memory-client-contract.md#m15-已知局限) 单独列出。

| 当前文档 | 内容 |
| --- | --- |
| [target-design.md](target-design.md) | 已冻结目标设计与不变量；实现范围和性能边界以运行时文档为准 |
| [contracts.md](contracts.md) | 冻结契约：W1/W3 v4、W4 v5、R1 v2、R4 v7、recall v8；旧数据读取与删除边界 |
| [w4-code-side.md](w4-code-side.md) | 默认写入：W0/T1、W1/W2/W3/W4、后台执行、轮次与恢复 |
| [w5-storage.md](w5-storage.md) | W5 事务、更新状态、关系、回执及删除清洗 |
| [contract-types.md](contract-types.md) | 旧 Memory 投影、限定语保留及 LegacyRecordView 兼容 |
| [recall-runtime.md](recall-runtime.md) | 默认 R1/R3/R4/R5、请求重放、计算、快照安全及宿主分发 |
| [visibility-cadence.md](visibility-cadence.md) | 七类场景与节奏测试；不代表生产完整负载 P95 已验收 |
| [m1-cutover.md](m1-cutover.md) | 用户本地升级前的手工一致性备份、配置清理和回退 |
| [../memory-client-contract.md](../memory-client-contract.md) | 客户端/宿主入口、request_id、原文授权和回执呈现 |
| [common-scoring-view.md](common-scoring-view.md) | common.v1 判分、复核台账及旧基线参考 |
| [../../tests/eval/README-v3.md](../../tests/eval/README-v3.md) | 默认严格回放、历史 59/4 自动重算、产物位置与补录授权 |

下一阶段的目标、方向和里程碑见 [../roadmap.md](../roadmap.md)。

写入模型判断语义，代码核对机械事实、预算、引用和认领；T1 保存原话与后台整理分别报告。召回先取得可信计划与 R3 读快照，再核验 R4 判断、执行确定性计算并由 R5 汇总；发布前检查删除、来源与授权/控制变化，普通后台提交允许按原快照返回。

M1 历史合并为 63 题 59/4/0 待复核；已知失败是 conflict_q2 和三道 LME。M1 收尾时严格回放为 235 复用 / 59 缺口 / 0 发送；M1.5 契约与提示词变化后仍须核对完整指纹，不能借历史输出补成当前通过，补录另行授权。

## 历史说明与报告

以下文档记录旧版本，不作为当前配置、运行命令或能力说明；旧接口在当前程序不可用，原历史结论保留。

| 历史文档 | 固定范围 |
| --- | --- |
| [overview.md](overview.md)、[write-path.md](write-path.md)、[recall-path.md](recall-path.md) | 旧引擎与 legacy/local/assisted 的实现梳理 |
| [capabilities.md](capabilities.md)、[retention.md](retention.md)、[model-usage.md](model-usage.md) | 相应历史提交的能力、留存和模型调用分析 |
| [phase1-baseline.md](phase1-baseline.md) | 0517000 基线，只作参考，不是上线门槛 |
| [retain-until-audit.md](retain-until-audit.md)、[w5-acceptance.md](w5-acceptance.md) | 旧 schema 审计与阶段验收记录 |
| [../architecture_v1.md](../architecture_v1.md) | 旧 V1 设计 |

AF185 混合检索缺省关闭：DB v11 本机来源分段缓存/维护，recall.v8 独立 retrieval 元数据；冻结规则见 contracts 3.2/3.4，接线见 recall-runtime 的 AF185 小节，严格零调用对照见 [检索评测](../../tests/eval/retrieval/README.md)。
