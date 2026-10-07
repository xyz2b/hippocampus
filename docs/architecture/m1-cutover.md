# M1 默认路径切换：手工一致性备份与回退

本步骤由用户在实际升级前执行；开发和测试不打开或升级用户本地数据库。默认路径切换不升库，当前格式仍为 v9；旧 v1–v8 正常打开时会沿用现有迁移到 v9。LegacyRecordView 保留旧数据读取，不自动启动语义重建。

## 切换前备份

1. 关闭所有连接该数据库的 MCP 服务、CLI 和 Inspector，先停止客户端自动重启服务。确认没有后台任务继续写入。
2. 在私有目录创建一致性 SQLite 备份。下面路径须替换为实际路径，备份目标必须是新文件：

   ```sh
   umask 077
   mkdir -p /absolute/path/private-backup
   sqlite3 /absolute/path/memory.sqlite3 ".backup /absolute/path/private-backup/memory-before-m1.sqlite3"
   sqlite3 /absolute/path/private-backup/memory-before-m1.sqlite3 "PRAGMA integrity_check; PRAGMA user_version;"
   ```

   integrity_check 应为 ok；记录原 user_version。不要在 WAL 活跃时只复制主数据库文件；.backup 包含一致性快照中的已提交数据和预算账本。备份也含原文，保持私有权限并明确保留/删除安排。
3. 保存当前配置、切换前实际运行的二进制及版本/校验值。例如保存可读 DB9 的 036e7f8 版本；不要把重新编译后的 target/debug 文件误当旧版本。
4. 用新程序的 --print-default-config 核对配置，再手工编辑。write_pipeline/recall_pipeline 缺省为 w4_runtime_v1/recall_v3，显式 legacy 报错且不自动改写文件。移除 retrieval_mode、local_recall_enabled、local_recall_budget、is_a 等已退出字段，包含 false/null/空值也会拒绝。客户端改用 recall.v6；bootstrap 也提供稳定 request_id。memory_clarify 不再提交旧暂存。
5. 未配置角色时，DEEPSEEK_PRO_MODEL 提供用户自己的 Pro 接入点/模型 ID。缺省 W1 low、W4 medium、W3/R1/R4 minimal，计算 R4 low。离线不要求模型环境变量；需要全服务零调用时用 enrichment_mode=off、PMR_MODEL_NETWORK=deny。
6. 先离线核对原文、旧记录、暂存和删除。在线启用须有用户授权的有限预算；历史预算字段/计数保留。旧组没有新 W 输出额度时申请显式新授权，不自动提高额度、修改旧 policy_json 或重置 group_id。

## 回退

优先停止服务后恢复切换前的 DB9 二进制与配置，继续使用当前数据库；本次没有新格式，通常不需要恢复备份。0517000 仅是历史评分基线，不作为当前 DB9 的回退程序。回退不会撤销已经发送的模型调用。

只有确认必须恢复数据时才手工恢复备份：先停全部连接，另存当前库的一致性备份，再处理原主文件及 WAL/SHM 文件。恢复切换前备份会丢失其后的原文、整理、删除控制和预算记账，须先核对这些增量；不能恢复后视为额度未用、自动重发任务或复活已删除内容。该操作需单独授权，不由默认切换自动执行。

## 旧数据边界

旧 Memory 的值、条件、时间、证据、状态及缺口保留；缺完整来源时不伪造 Quote、source_id 或新 Candidate。旧 processing_version、暂存、失败和排队任务不自动转换或完成；原文仍可只读召回，旧暂存仍可管理和删除。重建是后续独立、有预算并保留替代映射的任务。
