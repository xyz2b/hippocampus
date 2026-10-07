# 旧数据读取夹具

source-less-profile.json 是合成的早期快照：仅含 repository/entities/staging/receipts，保留旧 Memory 字段，不含完整原文或新整理字段。测试在 v1–v4 数据库上验证默认缺省与往返；它不冒充历史程序生成的导出。v5–v8 测试复用 ../w5_storage 中由对应提交程序生成的 SQL 导出。

schema-v3.sql 与 schema-v4-budget.sql 从已有历史 v5 导出的表定义取出，补齐 v3/v4 当时已存在的词法/预算表；不是把空表 v1 冒充 v3/v4。
