---
id: ADR-0009
title: Postgres 与 FalkorDB 原生客户端集成:crate 选型与查询道切分
status: accepted
date: 2026-09-21
deciders: [ray]
supersedes: []
superseded_by: null
tags: [client, engines]
---

# Postgres 与 FalkorDB 原生客户端集成

## Context

需求: REQ-008。三引擎的 client 面现状不同构:ClickHouse 已由 dctl 内置 HTTP 客户端直查;Postgres 宿主 psql 优先、FalkorDB 宿主 redis-cli 优先,缺失时回退容器内执行。用户令(2026-09-21 深夜)裁定把内置集成做成全引擎实态,并给定选型调研(Rust 生态:PG 侧 tokio-postgres/sqlx/diesel/libpq,FalkorDB 侧官方 falkordb crate)。

## Decision

1. **Postgres 选 tokio-postgres**:一次性 CLI 查询用不上 sqlx 的连接池与编译期校验(后者还需构建期 DATABASE_URL),diesel 的 ORM 面与 CLI 无关;tokio-postgres 是纯 Rust 低层驱动,与仓内 tokio 栈同频,scram/md5 认证齐备。同步封装 postgres 与全 async 仓逆流,不取。
2. **FalkorDB 选官方 falkordb crate(v0.10,tokio feature)**:官方维护、类型化参数与按列名读结果、同步/异步双面;历史社区库不取。**`-q` 语义升为 Cypher 级**(接 Cypher 语句,不再透传 GRAPH.QUERY 等裸 redis 命令),属破坏性变更,README 出迁移注;raw 命令道不保留(用户裁定单通道)。
3. **连接面**:两引擎经容器发布端口(5432/6379)从宿主直连;凭据复用实例元数据(start 时生成/存储的口令),不经 argv 不入日志,与既有边界同纪律。
4. **查询道切分**:程序化道(-q、--queries-file、stdin)全原生;交互式 REPL(无 -q)保留 docker exec 进容器内 psql/redis-cli,库不重建 REPL。
5. **输出契约**:人类面表格式渲染由 dctl 自绘(列对齐、NULL 显示),纪律对齐 ClickHouse client(查询输出保持原生语义、--json 出结构化面);退出码失败 1、usage 2 不变。
6. **依赖面**:tokio-postgres 默认无 TLS(localhost 容器面不需要,rustls feature 留旋钮);falkordb 依赖 redis crate 纯 Rust;musl 静态发布不受影响。

## Consequences

- psql 风格表格渲染由本仓承担(对齐、宽列截断、NULL 显示),是本批主要工作量与测试面。
- FK `-q` 的裸 redis 命令用法破坏性移除,现有用户需按迁移注改写为 Cypher。
- 宿主 psql/redis-cli 探测逻辑退役,client 面依赖只剩 Docker。
- 既有 client 集成测试(postgres input、falkordb readiness 的透传断言)随原生道重写。
