---
id: ADR-0001
title: fork clickhousectl 并整体剪除 cloud 栈,专注本地与 Docker 引擎
status: accepted
date: 2026-09-20
deciders: [ray]
supersedes: []
superseded_by: null
tags: [fork, scope]
---

# fork clickhousectl 并整体剪除 cloud 栈

## Context

ClickHouse 官方 clickhousectl(3-crate workspace,src 约 10.9 万行)覆盖本地与云两面,月均约 450 commits 且绝大多数落在 cloud 侧 [实证: 2026-09-20 git log 统计]。我们的诉求是本地 ClickHouse 二进制与 Postgres Docker 的生命周期管理,定位 DataBase Control;维护 cloud 面意味着背上 OpenAPI 漂移管线、云集成 CI、密钥环境等全部成本而无人使用。剪枝可行性已验证:local 子树对 cloud 零代码引用,全仓耦合仅 5 处 [实证: 2026-09-20 grep 缝合面分析]。

## Decision

fork 上游自维护,一次性删除 cloud 栈(src/cloud、clickhouse-cloud-api、clickhouse-openapi-analyzer、cloud CI、OpenAPI 漂移脚本、dotenv 模块),workspace 收缩为单 crate;保留 local 双引擎(ClickHouse 官方二进制 + Postgres Docker)、version_manager、结构化错误与双模输出、全部 local 测试资产。上游以 `upstream` remote 跟踪,只选择性 backport local 引擎改进:cherry-pick 后按身份映射适配(clickhousectl 对应 dctl 等),提交信息保留原始 commit 引用,过全量门禁与评审闸门。

> 追注(2026-09-21,ray 裁定):`upstream` remote 已退役,不再做选择性 backport;上游分歧自担,必要时按需临时加回 remote 取补丁。fork 与剪枝决策本体不变。

## Consequences

- 好面:维护面缩到约四分之一,无云凭据与密钥 CI 负担,定位清晰;local 侧测试资产(约 6.7 万行)完整保留 [实证: 剪枝后 531 单测与全部集成套件在 lan-linux 容器 0 FAILED]
- 坏面:与上游 merge 基本不可行(改名清扫 + 目录剪枝),backport 只能逐 commit 人工移植;上游 local 侧的修复需要主动盯梢,漏了没人提醒(backport 已随 2026-09-21 追注退役,不再盯梢)
- 坏面:telemetry 的失败分类(failure.rs)失去 cloud 语义后大半闲置,以 feature 隔离过渡,删除事项登记为 REQ-002
