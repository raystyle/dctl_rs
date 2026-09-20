---
id: REQ-003
title: FalkorDB 图数据库引擎接入
status: implemented
priority: must
trace: cargo test -p databasectl --test local_falkor_readiness_test
---

# FalkorDB 图数据库引擎接入

> REQ-001(Engine 抽象泛化)的首个具体第三引擎;引擎策略决策见 ADR-0005。

## Scenario

使用者在项目里用 `dctl local falkordb` 管理 FalkorDB 图数据库:一条命令拉起带认证的图数据库与 Browser 可视化,client 直连执行 openCypher,与 ClickHouse、Postgres 引擎并存互不干扰。

## Criteria

- [x] `local install falkordb@X.Y.Z` 拉镜像;`local falkordb start/stop/stop-all/remove/client/dotenv` 全族可用
- [x] start 默认随机密码(打印一次)、6379 与 3000 双端口自动挑口、就绪等待含认证 PING;stop 保留容器可 resume;remove 删净容器与数据
- [x] resume 从容器 env 回读凭据;启动失败三段回滚(容器、数据、元数据)
- [x] `local server list` 与 status 跨三引擎正确展示;stop-all 覆盖三引擎
- [x] 假 Docker API 的 readiness 集成测试(仿 local_postgres_readiness_test)+ clap 解析与 start 校验测试
- [x] 帮助块 CONTEXT FOR AGENTS 与 README 中文使用节覆盖
- [x] postgres 与 clickhouse 全量测试零回归;lan-linux 真容器实弹通过 [实证: 2026-09-20 lan-linux 全量 0 FAILED + 真容器全生命周期 SMOKE OK]
