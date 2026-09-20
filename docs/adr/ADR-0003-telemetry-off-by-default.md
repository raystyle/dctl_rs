---
id: ADR-0003
title: telemetry 默认编译排除且上报端点置空
status: accepted
date: 2026-09-20
deciders: [ray]
supersedes: []
superseded_by: null
tags: [telemetry, privacy]
---

# telemetry 默认编译排除且上报端点置空

## Context

上游 telemetry 默认开启,匿名上报命令名与旗标名到 chctl.clickhouse.com(Cloudflare worker 归 ClickHouse 所有)。fork 继续向该端点发送,既污染上游统计,也让我们的使用行为流向外部方,违背 fork 的隐私边界。上游已内建 `--no-default-features` 整体编译排除能力 [实证: 上游 CI 本就跑 no-default-features clippy 门禁]。

## Decision

默认 features 清空(telemetry 变为显式开启的可选 feature);代码内 `DEFAULT_ENDPOINT` 置空字符串,发送路径遇空端点直接返回;仅当环境变量 `DCTL_TELEMETRY_URL` 指向自有 collector 时才发送。telemetry.rs 与 failure.rs 整体在删除路线上(REQ-002),禁止在其上新建功能。

## Consequences

- 好面:任何默认构建结构性不上报,零网络外流 [实证: 2026-09-20 端点置空后 telemetry 测试套件在 DCTL_TELEMETRY_URL 注入 wiremock 下仍 42/42 通过,证明发送路径只认显式配置]
- 坏面:feature 开启时首跑通知等交互仍在,对极少数显式开启者有噪音;模块约 3000 行死重暂存,等 REQ-002 删除
- 后续:若未来需要自有遥测,应新写模块而非复活这套(其失败分类词表为 cloud 语义)
