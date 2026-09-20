---
id: REQ-001
title: Engine 抽象泛化,支持接入第三个数据库引擎
status: draft
priority: should
trace: null
---

# Engine 抽象泛化

## Scenario

使用者希望 dctl 像 manage ClickHouse 与 Postgres 一样管理其他本地数据库(候选:MySQL/MariaDB 官方二进制或容器、SQLite 无服务形态),使 DataBase Control 名副其实。

## Criteria

- [ ] 现有 `Engine` 枚举(src/local/server.rs)重构为引擎 trait 或等价扩展点,新增引擎不改 local 命令分发主干
- [ ] 至少接入一个新引擎走通 install/start/stop/status/client 生命周期
- [ ] 新引擎的 Docker 或二进制管理路径有假替身集成测试(对齐 local_postgres_readiness_test 风格)
- [ ] `CONTEXT FOR AGENTS` 帮助块与 README 覆盖新引擎
- [ ] 剪枝时保留的双引擎行为零回归(全量测试通过)

备注:动工前先立对应 ADR(引擎接入策略:二进制直管对应 Docker 托管的边界)。
