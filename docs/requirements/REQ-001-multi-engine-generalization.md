---
id: REQ-001
title: Engine 抽象泛化,支持接入第三个数据库引擎
status: implemented
priority: should
trace: 第三引擎由 FalkorDB 批(PR #1)满足;四引擎明确不做
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

## 处置记录(2026-09-21,用户裁定)

- 项目范围裁定:**只控制 ClickHouse + Postgres + FalkorDB 三个引擎**,不接入第四引擎(MySQL/SQLite 候选全部放弃),待真实需求再另立 REQ。
- 本 REQ 立项时仓内为双引擎,"接入第三个引擎"的使命已由 FalkorDB 批(PR #1)完整交付:容器生命周期、FakeDocker 就绪测试、client 集成、文档全覆盖;ClickHouse Docker 化(PR #6)进一步验证了三引擎一条生命周期道的泛化性。
- "Engine 枚举重构为 trait"不单独做:无第四引擎压力时属架构化妆品(YAGNI),三引擎共用 docker.rs 原语 + server.rs 元数据层的现状即扩展点,新引擎照 FalkorDB 批模式抄即可。
