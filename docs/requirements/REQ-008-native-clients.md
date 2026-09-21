---
id: REQ-008
title: Postgres 与 FalkorDB 原生客户端集成(宿主免装客户端落地到全引擎)
status: implemented
priority: should
trace: native-clients 批;ADR-0009
---

# Postgres 与 FalkorDB 原生客户端集成

## Scenario

宿主未装 psql/redis-cli 时,`dctl local postgres client` 与 `dctl local falkordb client` 的程序化查询道退化为容器内执行;用户令(2026-09-21 深夜裁定)要求把「client 内置集成」做成三引擎实态:Postgres 走 tokio-postgres,FalkorDB 走官方 falkordb crate,程序化查询(-q、--queries-file、stdin)全原生化,交互式 REPL 保留 docker exec。

## Criteria

- [x] Postgres 程序化查询走 tokio-postgres 直连容器发布端口,凭据复用实例元数据,宿主无 psql 可用
- [x] FalkorDB 程序化查询走 falkordb crate(tokio feature),`-q` 语义改为接 Cypher 语句(破坏性变更,README 迁移注)
- [x] 结果输出渲染契约:表格式对齐(列对齐、NULL 显示)、人类面与 --json 面与 ClickHouse client 的输出纪律一致
- [x] 退出码语义保持:查询失败 1、usage 错误 2,与现 psql/redis-cli 透传时代等价
- [x] 交互式模式(无 -q)保留 docker exec 进容器内 psql/redis-cli,行为不变
- [x] 既有集成测试面重写为原生道断言;真 Docker 复验按 lan-linux 配方过一轮(两引擎查询实弹)
- [x] 依赖面纯 Rust(rustls),musl 静态发布不受影响
